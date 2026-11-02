use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use bollard::Docker;
use bollard::models::{
    ContainerConfig, ContainerCreateBody, ContainerInspectResponse, EndpointSettings, HostConfig, ImageConfig, NetworkingConfig,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
    RenameContainerOptionsBuilder, StopContainerOptionsBuilder,
};
use futures::StreamExt;

pub const LABEL_UPDATER: &str = "kuroe.updater";

#[derive(Debug, PartialEq)]
pub struct Recreate {
    pub name: String,
    pub body: ContainerCreateBody,
}

fn differs<T: PartialEq + Clone>(container: &Option<T>, image: &Option<T>) -> Option<T> {
    if container == image { None } else { container.clone() }
}

/// What the user set on the container, minus what came from the old image, pointed at the new image.
pub fn recreate_body(old: &ContainerInspectResponse, old_image: &ImageConfig, new_image: &str) -> anyhow::Result<Recreate> {
    let name = old.name.as_deref().context("container has no name")?.trim_start_matches('/').to_owned();
    let id = old.id.as_deref().unwrap_or_default();
    let cfg: &ContainerConfig = old.config.as_ref().context("container has no config")?;

    let image_env = old_image.env.clone().unwrap_or_default();
    let env: Vec<String> = cfg.env.iter().flatten().filter(|e| !image_env.contains(e)).cloned().collect();
    let image_labels = old_image.labels.clone().unwrap_or_default();
    let labels: HashMap<String, String> =
        cfg.labels.iter().flatten().filter(|(k, v)| image_labels.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let image_ports = old_image.exposed_ports.clone().unwrap_or_default();
    let exposed: Vec<String> = cfg.exposed_ports.iter().flatten().filter(|p| !image_ports.contains(p)).cloned().collect();
    let image_volumes = old_image.volumes.clone().unwrap_or_default();
    let volumes: Vec<String> = cfg.volumes.iter().flatten().filter(|v| !image_volumes.contains(v)).cloned().collect();
    let hostname = cfg.hostname.clone().filter(|h| !id.starts_with(h.as_str()));

    let mut endpoints = HashMap::new();
    for (network, settings) in old.network_settings.as_ref().and_then(|n| n.networks.as_ref()).into_iter().flatten() {
        let short = &id[..id.len().min(12)];
        let aliases: Vec<String> = settings.aliases.iter().flatten().filter(|a| a.as_str() != short && *a != &name).cloned().collect();
        endpoints.insert(
            network.clone(),
            EndpointSettings {
                aliases: (!aliases.is_empty()).then_some(aliases),
                ipam_config: settings.ipam_config.clone(),
                links: settings.links.clone(),
                driver_opts: settings.driver_opts.clone(),
                gw_priority: settings.gw_priority.filter(|p| *p != 0),
                ..Default::default()
            },
        );
    }

    let body = ContainerCreateBody {
        image: Some(new_image.to_owned()),
        hostname,
        user: differs(&cfg.user, &old_image.user).filter(|u| !u.is_empty()),
        working_dir: differs(&cfg.working_dir, &old_image.working_dir).filter(|w| !w.is_empty()),
        env: (!env.is_empty()).then_some(env),
        cmd: differs(&cfg.cmd, &old_image.cmd),
        entrypoint: differs(&cfg.entrypoint, &old_image.entrypoint),
        labels: (!labels.is_empty()).then_some(labels),
        exposed_ports: (!exposed.is_empty()).then_some(exposed),
        volumes: (!volumes.is_empty()).then_some(volumes),
        stop_signal: differs(&cfg.stop_signal, &old_image.stop_signal),
        healthcheck: differs(&cfg.healthcheck, &old_image.healthcheck),
        tty: cfg.tty.filter(|t| *t),
        open_stdin: cfg.open_stdin.filter(|o| *o),
        host_config: old.host_config.clone(),
        networking_config: (!endpoints.is_empty()).then_some(NetworkingConfig { endpoints_config: Some(endpoints) }),
        ..Default::default()
    };
    Ok(Recreate { name, body })
}

/// The container's own env comes after the image's, and Docker lets it win, so the last value counts.
fn listen_port(env: &[String]) -> u16 {
    env.iter()
        .rev()
        .find_map(|e| e.strip_prefix("KUROE_LISTEN="))
        .and_then(|addr| addr.rsplit(':').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or(27863)
}

/// The old container keeps listening where its port binding points, even when the new image's default moved.
fn keep_listen(old_env: &[String], new_image_env: &[String], env: &mut Vec<String>) {
    let listen = |list: &[String]| list.iter().find(|e| e.starts_with("KUROE_LISTEN=")).cloned();
    if listen(env).is_some() {
        return;
    }
    if let Some(old) = listen(old_env)
        && listen(new_image_env).as_ref() != Some(&old)
    {
        env.push(old);
    }
}

pub async fn pull(docker: &Docker, image: &str) -> anyhow::Result<()> {
    let mut stream = docker.create_image(Some(CreateImageOptionsBuilder::new().from_image(image).build()), None, None);
    while let Some(step) = stream.next().await {
        step.with_context(|| format!("pulling {image}"))?;
    }
    Ok(())
}

/// Starts the one-shot updater from the new image. It takes over once this process returns.
pub async fn start_updater(docker: &Docker, own_id: &str, image: &str, data_dir: &Path) -> anyhow::Result<String> {
    if docker.inspect_image(image).await.is_err() {
        pull(docker, image).await?;
    } else if let Err(err) = pull(docker, image).await {
        tracing::warn!("using the local copy of {image}, pulling failed: {err:#}");
    }
    let filters = HashMap::from([("label", vec![LABEL_UPDATER.to_owned()])]);
    for old in docker.list_containers(Some(ListContainersOptionsBuilder::new().all(true).filters(&filters).build())).await? {
        if old.state.map(|s| s.to_string()) != Some("running".into())
            && let Some(id) = old.id
        {
            docker.remove_container(&id, Some(RemoveContainerOptionsBuilder::new().force(true).build())).await.ok();
        }
    }
    let me = docker.inspect_container(own_id, None).await?;
    let network = me
        .network_settings
        .as_ref()
        .and_then(|n| n.networks.as_ref())
        .and_then(|n| n.keys().next().cloned())
        .context("kuroe's container is on no network")?;
    let name = format!("kuroe_updater_{}", crate::db::now());
    let body = ContainerCreateBody {
        image: Some(image.to_owned()),
        entrypoint: Some(vec!["kuroe".into()]),
        cmd: Some(vec![
            "--data-dir".into(),
            data_dir.to_string_lossy().into_owned(),
            "self-update".into(),
            "--target".into(),
            own_id.to_owned(),
            "--image".into(),
            image.to_owned(),
        ]),
        labels: Some(HashMap::from([(LABEL_UPDATER.to_owned(), "true".to_owned())])),
        host_config: Some(HostConfig { volumes_from: Some(vec![own_id.to_owned()]), network_mode: Some(network), ..Default::default() }),
        ..Default::default()
    };
    docker.create_container(Some(CreateContainerOptionsBuilder::new().name(&name).build()), body).await?;
    docker.start_container(&name, None).await?;
    Ok(name)
}

async fn healthy(docker: &Docker, http: &reqwest::Client, container: &str, port: u16, timeout: Duration) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let inspect = docker.inspect_container(container, None).await?;
        let state = inspect.state.clone().unwrap_or_default();
        let restarted = inspect.restart_count.unwrap_or(0) > 0 || state.restarting == Some(true);
        if restarted || (state.running != Some(true) && state.status.map(|s| s.to_string()).as_deref() != Some("created")) {
            bail!("the new container exited with code {}", state.exit_code.unwrap_or_default());
        }
        let ips: Vec<String> = inspect
            .network_settings
            .and_then(|n| n.networks)
            .into_iter()
            .flatten()
            .filter_map(|(_, e)| e.ip_address.filter(|ip| !ip.is_empty()))
            .collect();
        for ip in ips {
            if let Ok(res) = http.get(format!("http://{ip}:{port}{}/setup", crate::api::API_BASE)).send().await
                && res.status().is_success()
            {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() > deadline {
            bail!("the new container did not answer on port {port} within {}s", timeout.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Runs inside the updater container.
pub async fn run_updater(docker: &Docker, target: &str, image: &str, data_dir: &Path) -> anyhow::Result<()> {
    tokio::time::sleep(Duration::from_millis(500)).await;
    let old = docker.inspect_container(target, None).await.context("inspecting the running kuroe")?;
    let old_image_id = old.image.clone().context("running kuroe has no image id")?;
    let old_image = docker.inspect_image(&old_image_id).await?.config.unwrap_or_default();
    let mut plan = recreate_body(&old, &old_image, image)?;
    let new_image_env = docker.inspect_image(image).await?.config.and_then(|c| c.env).unwrap_or_default();
    let old_env = old.config.as_ref().and_then(|c| c.env.clone()).unwrap_or_default();
    let mut own_env = plan.body.env.take().unwrap_or_default();
    keep_listen(&old_env, &new_image_env, &mut own_env);
    plan.body.env = (!own_env.is_empty()).then_some(own_env);
    let env: Vec<String> = new_image_env.iter().chain(plan.body.env.iter().flatten()).cloned().collect();
    let port = listen_port(&env);
    let parked = format!("{}_old", plan.name);
    let http = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;

    println!("updating {} to {image}", plan.name);
    docker.rename_container(target, RenameContainerOptionsBuilder::new().name(&parked).build()).await?;
    docker.stop_container(target, Some(StopContainerOptionsBuilder::new().t(10).build())).await?;

    let attempt: anyhow::Result<()> = async {
        let pool = crate::db::connect(data_dir).await?;
        crate::db::migrate(&pool).await?;
        pool.close().await;
        docker
            .create_container(Some(CreateContainerOptionsBuilder::new().name(&plan.name).build()), plan.body.clone())
            .await
            .context("creating the new container")?;
        docker.start_container(&plan.name, None).await.context("starting the new container")?;
        healthy(docker, &http, &plan.name, port, Duration::from_secs(30)).await
    }
    .await;

    match attempt {
        Ok(()) => {
            docker.remove_container(target, Some(RemoveContainerOptionsBuilder::new().force(true).build())).await.ok();
            println!("updated to {image}");
            Ok(())
        }
        Err(err) => {
            println!("update failed, rolling back: {err:#}");
            docker.remove_container(&plan.name, Some(RemoveContainerOptionsBuilder::new().force(true).build())).await.ok();
            docker.rename_container(target, RenameContainerOptionsBuilder::new().name(&plan.name).build()).await?;
            docker.start_container(target, None).await?;
            println!("the previous version is running again");
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inspect() -> ContainerInspectResponse {
        serde_json::from_value(json!({
            "Id": "3f1c2b4a5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081",
            "Name": "/kuroe",
            "Image": "sha256:old",
            "Config": {
                "Hostname": "3f1c2b4a5d6e",
                "Env": ["PATH=/usr/local/bin:/usr/bin", "KUROE_DATA=/data", "KUROE_LISTEN=0.0.0.0:8080", "RUST_LOG=debug"],
                "Cmd": ["serve"],
                "Entrypoint": ["kuroe"],
                "Image": "ghcr.io/acme/kuroe:1.0",
                "Labels": { "org.opencontainers.image.version": "1.0", "owner": "ops" },
                "ExposedPorts": { "8080/tcp": {} },
                "Volumes": { "/data": {} },
                "WorkingDir": "",
                "Tty": false,
                "OpenStdin": false
            },
            "HostConfig": {
                "Binds": ["/var/run/docker.sock:/var/run/docker.sock", "kuroe_data:/data"],
                "PortBindings": { "8080/tcp": [{ "HostIp": "", "HostPort": "8080" }] },
                "RestartPolicy": { "Name": "unless-stopped", "MaximumRetryCount": 0 },
                "NetworkMode": "kuroe_proxy"
            },
            "NetworkSettings": { "Networks": { "kuroe_proxy": {
                "Aliases": ["3f1c2b4a5d6e", "kuroe", "panel"],
                "NetworkID": "abc", "EndpointID": "def", "IPAddress": "172.18.0.5", "MacAddress": "02:42:ac:12:00:05"
            }}}
        }))
        .unwrap()
    }

    fn image() -> ImageConfig {
        serde_json::from_value(json!({
            "Env": ["PATH=/usr/local/bin:/usr/bin", "KUROE_DATA=/data", "KUROE_LISTEN=0.0.0.0:8080"],
            "Cmd": ["serve"],
            "Entrypoint": ["kuroe"],
            "Labels": { "org.opencontainers.image.version": "1.0" },
            "ExposedPorts": { "8080/tcp": {} },
            "Volumes": { "/data": {} }
        }))
        .unwrap()
    }

    #[test]
    fn keeps_user_config_and_drops_image_defaults() {
        let plan = recreate_body(&inspect(), &image(), "ghcr.io/acme/kuroe:1.1").unwrap();
        assert_eq!(plan.name, "kuroe");
        let body = plan.body;
        assert_eq!(body.image.as_deref(), Some("ghcr.io/acme/kuroe:1.1"));
        assert_eq!(body.env, Some(vec!["RUST_LOG=debug".to_owned()]));
        assert_eq!(body.cmd, None, "the new image's own default command applies");
        assert_eq!(body.entrypoint, None);
        assert_eq!(body.labels, Some(HashMap::from([("owner".to_owned(), "ops".to_owned())])));
        assert_eq!(body.exposed_ports, None);
        assert_eq!(body.volumes, None);
        assert_eq!(body.hostname, None, "the generated hostname is not carried over");
        assert_eq!(body.host_config, inspect().host_config, "ports, binds, restart policy and network stay identical");
        let endpoints = body.networking_config.unwrap().endpoints_config.unwrap();
        let proxy = &endpoints["kuroe_proxy"];
        assert_eq!(proxy.aliases, Some(vec!["panel".to_owned()]));
        assert_eq!(proxy.ip_address, None);
    }

    #[test]
    fn user_overrides_survive() {
        let mut old = inspect();
        let cfg = old.config.as_mut().unwrap();
        cfg.cmd = Some(vec!["serve".into(), "--secure-cookies".into()]);
        cfg.hostname = Some("panel.internal".into());
        let body = recreate_body(&old, &image(), "x").unwrap().body;
        assert_eq!(body.cmd, Some(vec!["serve".to_owned(), "--secure-cookies".to_owned()]));
        assert_eq!(body.hostname.as_deref(), Some("panel.internal"));
    }

    #[test]
    fn listen_port_comes_from_env() {
        assert_eq!(listen_port(&["KUROE_LISTEN=0.0.0.0:9000".into()]), 9000);
        assert_eq!(listen_port(&[]), 27863);
    }

    #[test]
    fn an_old_install_keeps_its_port_when_the_default_moves() {
        let old = vec!["KUROE_DATA=/data".to_owned(), "KUROE_LISTEN=0.0.0.0:8080".to_owned()];
        let new_image = vec!["KUROE_DATA=/data".to_owned(), "KUROE_LISTEN=0.0.0.0:27863".to_owned()];
        let mut env = vec!["RUST_LOG=debug".to_owned()];
        keep_listen(&old, &new_image, &mut env);
        assert_eq!(env, ["RUST_LOG=debug", "KUROE_LISTEN=0.0.0.0:8080"]);
        assert_eq!(listen_port(&new_image.iter().chain(&env).cloned().collect::<Vec<_>>()), 8080, "the last value wins, like Docker");

        let mut same = Vec::new();
        keep_listen(&new_image, &new_image, &mut same);
        assert!(same.is_empty(), "nothing to pin when the default didn't change");
        let mut own = vec!["KUROE_LISTEN=0.0.0.0:9000".to_owned()];
        keep_listen(&old, &new_image, &mut own);
        assert_eq!(own, ["KUROE_LISTEN=0.0.0.0:9000"], "a value the user set stays");
    }
}
