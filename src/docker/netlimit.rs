use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context as _, bail};
use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    BuildImageOptionsBuilder, EventsOptionsBuilder, ListContainersOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
};
use futures::StreamExt;

use super::rewrite::{LABEL_DEPLOYMENT, LABEL_MANAGED};

pub const LABEL_NET_IN: &str = "kuroe.net_in";
pub const LABEL_NET_OUT: &str = "kuroe.net_out";
pub const LABEL_NET_CONN_OUT: &str = "kuroe.net_conn_out";
const LABEL_HELPER: &str = "kuroe.netlimit";
const IMAGE: &str = "kuroe/nettools:1";
const DOCKERFILE: &str = "FROM alpine:3.22\nRUN apk add --no-cache iproute2-tc\n";
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const RECONNECT_AFTER: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetLimit {
    pub in_mbit: Option<f64>,
    pub out_mbit: Option<f64>,
    pub conn_out_mbit: Option<f64>,
}

impl NetLimit {
    pub fn is_empty(&self) -> bool {
        self.in_mbit.is_none() && self.out_mbit.is_none() && self.conn_out_mbit.is_none()
    }

    /// Only deployment and CI runner containers are limited. kuroe itself, Traefik, the static server, builds and
    /// this helper carry neither label.
    pub fn from_labels(labels: &HashMap<String, String>) -> Option<Self> {
        if !labels.contains_key(LABEL_DEPLOYMENT) && !labels.contains_key(crate::runners::LABEL_POOL) {
            return None;
        }
        let read = |key| labels.get(key).and_then(|v: &String| v.parse::<f64>().ok()).filter(|v| *v > 0.0);
        let limit = NetLimit { in_mbit: read(LABEL_NET_IN), out_mbit: read(LABEL_NET_OUT), conn_out_mbit: read(LABEL_NET_CONN_OUT) };
        (!limit.is_empty()).then_some(limit)
    }
}

fn kbit(mbit: f64) -> u64 {
    ((mbit * 1000.0).round() as u64).max(8)
}

/// Shaping needs about 10 ms of traffic per burst. Policing drops what's over, so TCP needs about half a second.
fn bursts(kbit: u64) -> (u64, u64) {
    let bytes_per_second = kbit * 125;
    ((bytes_per_second / 100).max(32 * 1024), (bytes_per_second / 2).max(64 * 1024))
}

/// Runs in the container's network namespace and sets every interface but loopback. The per-connection cap is `fq`
/// pacing each flow, under the total `tbf` when there is one.
pub fn script(limit: &NetLimit) -> String {
    let mut out = String::from("set -e\nfor dev in $(ls /sys/class/net); do\n  [ \"$dev\" = lo ] && continue\n");
    out.push_str("  tc qdisc del dev \"$dev\" root 2>/dev/null || true\n");
    let conn = limit.conn_out_mbit.map(kbit);
    match (limit.out_mbit.map(kbit), conn) {
        (Some(rate), conn) => {
            let (burst, _) = bursts(rate);
            let _ = writeln!(out, "  tc qdisc add dev \"$dev\" root handle 1: tbf rate {rate}kbit burst {burst} latency 50ms");
            if let Some(conn) = conn {
                let _ = writeln!(out, "  tc qdisc add dev \"$dev\" parent 1:1 handle 10: fq maxrate {conn}kbit");
            }
        }
        (None, Some(conn)) => {
            let _ = writeln!(out, "  tc qdisc add dev \"$dev\" root fq maxrate {conn}kbit");
        }
        (None, None) => {}
    }
    out.push_str("  tc qdisc del dev \"$dev\" ingress 2>/dev/null || true\n");
    if let Some(mbit) = limit.in_mbit {
        let rate = kbit(mbit);
        let (_, burst) = bursts(rate);
        out.push_str("  tc qdisc add dev \"$dev\" handle ffff: ingress\n");
        let _ = writeln!(
            out,
            "  tc filter add dev \"$dev\" parent ffff: protocol all u32 match u32 0 0 police rate {rate}kbit burst {burst} drop flowid :1"
        );
    }
    out.push_str("done\n");
    out
}

static LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = LazyLock::new(Mutex::default);
static IMAGE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn lock_for(container: &str) -> Arc<tokio::sync::Mutex<()>> {
    LOCKS.lock().unwrap().entry(container.to_owned()).or_default().clone()
}

fn release(container: &str, lock: Arc<tokio::sync::Mutex<()>>) {
    let mut locks = LOCKS.lock().unwrap();
    drop(lock);
    if locks.get(container).is_some_and(|l| Arc::strong_count(l) == 1) {
        locks.remove(container);
    }
}

/// Sets the limit inside a running container's network namespace. A helper container joins that namespace with
/// `NET_ADMIN`, so the deployment's own container never gets the capability and can't lift its limit.
pub async fn apply(docker: &Docker, container: &str, limit: &NetLimit) -> anyhow::Result<()> {
    let lock = lock_for(container);
    let result = {
        let _guard = lock.lock().await;
        run_helper(docker, container, &script(limit)).await.map(|_| ())
    };
    release(container, lock);
    result
}

async fn ensure_image(docker: &Docker) -> anyhow::Result<()> {
    let _guard = IMAGE_LOCK.lock().await;
    if docker.inspect_image(IMAGE).await.is_ok() {
        return Ok(());
    }
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(DOCKERFILE.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "Dockerfile", DOCKERFILE.as_bytes())?;
    let labels = HashMap::from([(LABEL_MANAGED, "true")]);
    let options = BuildImageOptionsBuilder::new().t(IMAGE).rm(true).labels(&labels).build();
    let mut stream = docker.build_image(options, None, Some(bollard::body_full(builder.into_inner()?.into())));
    while let Some(item) = stream.next().await {
        if let Some(err) = item.context("building the network limit helper image")?.error_detail {
            bail!("building the network limit helper image failed: {}", err.message.unwrap_or_default());
        }
    }
    Ok(())
}

async fn run_helper(docker: &Docker, container: &str, script: &str) -> anyhow::Result<String> {
    ensure_image(docker).await?;
    let labels = HashMap::from([(LABEL_MANAGED.to_owned(), "true".to_owned()), (LABEL_HELPER.to_owned(), "true".to_owned())]);
    let body = ContainerCreateBody {
        image: Some(IMAGE.into()),
        cmd: Some(vec!["/bin/sh".into(), "-c".into(), script.to_owned()]),
        labels: Some(labels),
        host_config: Some(HostConfig {
            network_mode: Some(format!("container:{container}")),
            cap_add: Some(vec!["NET_ADMIN".into()]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let created = docker
        .create_container(None::<bollard::query_parameters::CreateContainerOptions>, body)
        .await
        .context("creating the network limit helper")?;
    let result = finish_helper(docker, &created.id).await;
    let _ = docker.remove_container(&created.id, Some(RemoveContainerOptionsBuilder::new().force(true).build())).await;
    result
}

async fn finish_helper(docker: &Docker, id: &str) -> anyhow::Result<String> {
    docker.start_container(id, None).await.context("starting the network limit helper")?;
    let mut output = String::new();
    let follow = async {
        let options = LogsOptionsBuilder::new().follow(true).stdout(true).stderr(true).build();
        let mut logs = docker.logs(id, Some(options));
        while let Some(Ok(chunk)) = logs.next().await {
            output.push_str(&chunk.to_string());
        }
    };
    if tokio::time::timeout(HELPER_TIMEOUT, follow).await.is_err() {
        bail!("setting the network limit took longer than {}s", HELPER_TIMEOUT.as_secs());
    }
    let code = docker.inspect_container(id, None).await?.state.and_then(|s| s.exit_code).unwrap_or(-1);
    if code != 0 {
        bail!("setting the network limit failed: {}", output.trim());
    }
    Ok(output)
}

/// Applies the limit of every running container matching the label filter that has one, with each container's result.
pub async fn apply_running(docker: &Docker, label: &str) -> anyhow::Result<Vec<(String, anyhow::Result<()>)>> {
    let filters = HashMap::from([("label", vec![label.to_owned()]), ("status", vec!["running".to_owned()])]);
    let containers = docker.list_containers(Some(ListContainersOptionsBuilder::new().filters(&filters).build())).await?;
    let mut results = Vec::new();
    for summary in containers {
        let (Some(id), Some(labels)) = (summary.id, summary.labels) else { continue };
        let Some(limit) = NetLimit::from_labels(&labels) else { continue };
        let result = apply(docker, &id, &limit).await;
        let name = summary.names.and_then(|n| n.first().cloned()).unwrap_or(id);
        results.push((name.trim_start_matches('/').to_owned(), result));
    }
    Ok(results)
}

/// A restart gives a container a new network namespace without the rules, so every start is limited again.
pub fn spawn_watcher(docker: Docker) {
    tokio::spawn(async move {
        loop {
            match apply_running(&docker, &format!("{LABEL_MANAGED}=true")).await {
                Ok(results) => {
                    for (name, result) in results {
                        if let Err(err) = result {
                            tracing::warn!("network limit for {name}: {err:#}");
                        }
                    }
                }
                Err(err) => tracing::warn!("listing containers for network limits: {err:#}"),
            }
            watch_starts(&docker).await;
            tokio::time::sleep(RECONNECT_AFTER).await;
        }
    });
}

async fn watch_starts(docker: &Docker) {
    let managed = format!("{LABEL_MANAGED}=true");
    let filters = HashMap::from([
        ("type", vec!["container"]),
        ("event", vec!["start"]),
        ("label", vec![managed.as_str()]),
    ]);
    let mut events = docker.events(Some(EventsOptionsBuilder::new().filters(&filters).build()));
    while let Some(event) = events.next().await {
        let event = match event {
            Ok(event) => event,
            Err(err) => {
                tracing::warn!("docker events for network limits stopped: {err}");
                return;
            }
        };
        let Some(actor) = event.actor else { continue };
        let (Some(id), Some(attributes)) = (actor.id, actor.attributes) else { continue };
        let Some(limit) = NetLimit::from_labels(&attributes) else { continue };
        let docker = docker.clone();
        tokio::spawn(async move {
            if let Err(err) = apply(&docker, &id, &limit).await {
                let name = attributes.get("name").cloned().unwrap_or(id);
                tracing::warn!("network limit for {name}: {err:#}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn only_deployment_and_runner_containers_with_a_limit_are_limited() {
        let both = labels(&[(LABEL_DEPLOYMENT, "d1"), (LABEL_NET_IN, "20"), (LABEL_NET_OUT, "5.5"), (LABEL_NET_CONN_OUT, "1")]);
        assert_eq!(
            NetLimit::from_labels(&both),
            Some(NetLimit { in_mbit: Some(20.0), out_mbit: Some(5.5), conn_out_mbit: Some(1.0) })
        );
        let out_only = labels(&[(LABEL_DEPLOYMENT, "d1"), (LABEL_NET_OUT, "5")]);
        assert_eq!(NetLimit::from_labels(&out_only), Some(NetLimit { out_mbit: Some(5.0), ..Default::default() }));
        let runner = labels(&[(crate::runners::LABEL_POOL, "p1"), (LABEL_NET_IN, "100")]);
        assert_eq!(NetLimit::from_labels(&runner), Some(NetLimit { in_mbit: Some(100.0), ..Default::default() }));
        assert_eq!(NetLimit::from_labels(&labels(&[(LABEL_DEPLOYMENT, "d1")])), None, "no limit, nothing to do");
        assert_eq!(NetLimit::from_labels(&labels(&[(LABEL_DEPLOYMENT, "d1"), (LABEL_NET_IN, "0")])), None);
        assert_eq!(NetLimit::from_labels(&labels(&[(LABEL_DEPLOYMENT, "d1"), (LABEL_NET_IN, "fast")])), None);
        let system = labels(&[(LABEL_MANAGED, "true"), (LABEL_NET_IN, "20"), (LABEL_NET_OUT, "20")]);
        assert_eq!(NetLimit::from_labels(&system), None, "kuroe's own containers are never limited");
    }

    #[test]
    fn outbound_only_shapes_and_clears_inbound() {
        let s = script(&NetLimit { out_mbit: Some(5.0), ..Default::default() });
        assert_eq!(
            s,
            "set -e\nfor dev in $(ls /sys/class/net); do\n  [ \"$dev\" = lo ] && continue\n  \
             tc qdisc del dev \"$dev\" root 2>/dev/null || true\n  \
             tc qdisc add dev \"$dev\" root handle 1: tbf rate 5000kbit burst 32768 latency 50ms\n  \
             tc qdisc del dev \"$dev\" ingress 2>/dev/null || true\ndone\n"
        );
    }

    #[test]
    fn inbound_only_polices_and_clears_outbound() {
        let s = script(&NetLimit { in_mbit: Some(50.0), ..Default::default() });
        assert!(s.contains("tc qdisc del dev \"$dev\" root 2>/dev/null || true\n"));
        assert!(s.contains("tc qdisc add dev \"$dev\" handle ffff: ingress\n"));
        assert!(s.contains("police rate 50000kbit burst 3125000 drop flowid :1\n"), "{s}");
        assert!(!s.contains("tbf") && !s.contains("fq"));
    }

    #[test]
    fn both_directions_and_fractions() {
        let s = script(&NetLimit { in_mbit: Some(0.5), out_mbit: Some(1000.0), ..Default::default() });
        assert!(s.contains("root handle 1: tbf rate 1000000kbit burst 1250000 latency 50ms"), "{s}");
        assert!(s.contains("police rate 500kbit burst 65536 drop"), "{s}");
    }

    #[test]
    fn per_connection_cap_alone_paces_each_flow() {
        let s = script(&NetLimit { conn_out_mbit: Some(10.0), ..Default::default() });
        assert!(s.contains("tc qdisc add dev \"$dev\" root fq maxrate 10000kbit\n"), "{s}");
        assert!(!s.contains("tbf"));
    }

    #[test]
    fn per_connection_cap_sits_under_the_total() {
        let s = script(&NetLimit { out_mbit: Some(25.0), conn_out_mbit: Some(10.0), ..Default::default() });
        let total = s.find("root handle 1: tbf rate 25000kbit").expect("total cap");
        let conn = s.find("parent 1:1 handle 10: fq maxrate 10000kbit").expect("per connection cap");
        assert!(total < conn, "{s}");
    }

    #[test]
    fn no_limit_removes_both() {
        let s = script(&NetLimit::default());
        assert!(s.contains("tc qdisc del dev \"$dev\" root"));
        assert!(s.contains("tc qdisc del dev \"$dev\" ingress"));
        assert!(!s.contains("tc qdisc add") && !s.contains("tc filter"));
    }

    #[test]
    fn bursts_have_floors() {
        assert_eq!(bursts(kbit(1.0)), (32 * 1024, 64 * 1024));
        assert_eq!(bursts(kbit(200.0)), (250_000, 12_500_000));
    }

    async fn start(docker: &Docker, name: &str, labels: &[(&str, &str)]) -> String {
        let _ = docker.remove_container(name, Some(RemoveContainerOptionsBuilder::new().force(true).build())).await;
        let mut pull = docker.create_image(
            Some(bollard::query_parameters::CreateImageOptionsBuilder::new().from_image("alpine:3.22").build()),
            None,
            None,
        );
        while pull.next().await.is_some() {}
        let body = ContainerCreateBody {
            image: Some("alpine:3.22".into()),
            cmd: Some(vec!["sleep".into(), "300".into()]),
            labels: Some(labels.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()),
            ..Default::default()
        };
        let options = bollard::query_parameters::CreateContainerOptionsBuilder::new().name(name).build();
        let id = docker.create_container(Some(options), body).await.unwrap().id;
        docker.start_container(&id, None).await.unwrap();
        id
    }

    async fn qdiscs(docker: &Docker, id: &str) -> String {
        run_helper(docker, id, "tc qdisc show; tc filter show dev eth0 parent ffff: 2>/dev/null || true").await.unwrap()
    }

    #[tokio::test]
    #[ignore = "needs the Docker socket"]
    async fn limits_follow_restarts_and_new_runners_and_skip_system_containers() {
        let docker = crate::docker::connect().unwrap();
        let app = start(&docker, "kuroe_netlimit_test_app", &[
            (LABEL_MANAGED, "true"),
            (LABEL_DEPLOYMENT, "netlimit-test"),
            (LABEL_NET_IN, "3"),
            (LABEL_NET_OUT, "5"),
            (LABEL_NET_CONN_OUT, "2"),
        ])
        .await;
        let system = start(&docker, "kuroe_netlimit_test_system", &[(LABEL_MANAGED, "true"), (LABEL_NET_OUT, "5")]).await;

        let results = apply_running(&docker, &format!("{LABEL_DEPLOYMENT}=netlimit-test")).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_ok(), "{:?}", results[0].1);
        let shown = qdiscs(&docker, &app).await;
        assert!(shown.contains("tbf") && shown.contains("rate 5Mbit"), "{shown}");
        assert!(shown.contains("fq") && shown.contains("maxrate 2Mbit"), "{shown}");
        assert!(shown.contains("ingress") && shown.contains("police"), "{shown}");

        spawn_watcher(docker.clone());
        tokio::time::sleep(Duration::from_secs(2)).await;
        docker.restart_container(&app, None).await.unwrap();
        let runner = start(&docker, "kuroe_netlimit_test_runner", &[
            (LABEL_MANAGED, "true"),
            (crate::runners::LABEL_POOL, "netlimit-test"),
            (LABEL_NET_OUT, "7"),
        ])
        .await;
        async fn wait_for(docker: &Docker, id: &str, wanted: &[&str]) -> String {
            let mut shown = String::new();
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                shown = qdiscs(docker, id).await;
                if wanted.iter().all(|w| shown.contains(w)) {
                    break;
                }
            }
            shown
        }
        let restored = wait_for(&docker, &app, &["tbf", "maxrate 2Mbit", "police"]).await;
        assert!(restored.contains("tbf") && restored.contains("police"), "the watcher sets the limit again: {restored}");
        let runner_shown = wait_for(&docker, &runner, &["rate 7Mbit"]).await;
        assert!(runner_shown.contains("tbf") && runner_shown.contains("rate 7Mbit"), "a new runner is limited: {runner_shown}");

        let untouched = qdiscs(&docker, &system).await;
        assert!(!untouched.contains("tbf") && !untouched.contains("ingress"), "{untouched}");

        for id in [app, system, runner] {
            let _ = docker.remove_container(&id, Some(RemoveContainerOptionsBuilder::new().force(true).build())).await;
        }
    }

    #[test]
    fn locks_are_dropped_after_use() {
        let lock = lock_for("abc");
        assert!(LOCKS.lock().unwrap().contains_key("abc"));
        release("abc", lock);
        assert!(!LOCKS.lock().unwrap().contains_key("abc"));
    }
}
