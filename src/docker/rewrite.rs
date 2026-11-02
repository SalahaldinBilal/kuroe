use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::netlimit::{LABEL_NET_CONN_OUT, LABEL_NET_IN, LABEL_NET_OUT};

pub const PROXY_NETWORK: &str = "kuroe_proxy";
pub const LABEL_MANAGED: &str = "kuroe.managed";
pub const LABEL_DEPLOYMENT: &str = "kuroe.deployment";
pub const LABEL_SERVICE: &str = "kuroe.service";

const CAP_ALLOWLIST: &[&str] = &[
    "AUDIT_WRITE",
    "CHOWN",
    "DAC_OVERRIDE",
    "FOWNER",
    "FSETID",
    "KILL",
    "MKNOD",
    "NET_BIND_SERVICE",
    "NET_RAW",
    "SETFCAP",
    "SETGID",
    "SETPCAP",
    "SETUID",
    "SYS_CHROOT",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServiceSettings {
    #[serde(default)]
    pub public: bool,
    pub cpus: Option<f64>,
    pub memory_mb: Option<u64>,
    pub pids: Option<i64>,
    #[serde(default)]
    pub net_in_mbit: Option<f64>,
    #[serde(default)]
    pub net_out_mbit: Option<f64>,
    #[serde(default)]
    pub net_conn_out_mbit: Option<f64>,
    #[serde(default)]
    pub volumes: Vec<SharedMount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedMount {
    pub volume: String,
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Defaults {
    pub cpus: Option<f64>,
    pub memory_mb: Option<u64>,
    pub pids: Option<i64>,
    pub net_in_mbit: Option<f64>,
    pub net_out_mbit: Option<f64>,
    pub net_conn_out_mbit: Option<f64>,
}

pub const SWARM_NETWORK: &str = "kuroe_swarm";

pub struct Context<'a> {
    pub deployment_id: &'a str,
    pub revision: i64,
    pub settings: &'a BTreeMap<String, ServiceSettings>,
    pub defaults: &'a Defaults,
    pub host_mounts: bool,
    pub stack: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Build {
    pub service: String,
    pub context: String,
    pub dockerfile: String,
    pub args: BTreeMap<String, String>,
    pub tag: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Request {
    pub service: String,
    pub replicas: u64,
    pub cpus: f64,
    pub memory_bytes: u64,
}

#[derive(Debug, PartialEq)]
pub struct Rewritten {
    pub config: Value,
    pub builds: Vec<Build>,
    pub requests: Vec<Request>,
    pub warnings: Vec<String>,
}

pub fn rewrite(mut config: Value, ctx: &Context) -> anyhow::Result<Rewritten> {
    let project = config["name"].as_str().unwrap_or_default().to_owned();
    let services = config
        .get_mut("services")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow::anyhow!("compose file has no services"))?;

    let mut builds = Vec::new();
    let mut requests = Vec::new();
    let mut warnings = Vec::new();
    let mut shared_volumes = Vec::new();

    for (name, service) in services.iter_mut() {
        let service = service
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("service {name} is not an object"))?;
        let settings = ctx.settings.get(name).cloned().unwrap_or_default();
        let mut warn = |msg: String| warnings.push(format!("{name}: {msg}"));

        if let Some(build) = service.remove("build") {
            let tag = format!("kuroe/{project}-{name}:{}", ctx.revision);
            builds.push(Build {
                service: name.clone(),
                context: build["context"].as_str().unwrap_or(".").to_owned(),
                dockerfile: build["dockerfile"].as_str().unwrap_or("Dockerfile").to_owned(),
                args: string_map(&build["args"]),
                tag: tag.clone(),
            });
            service.insert("image".into(), json!(tag));
        }

        strip_privileges(service, &mut warn);
        strip_bind_mounts(service, ctx.host_mounts, &mut warn);
        if ctx.stack {
            service.retain(|_, value| !value.is_null());
            rewrite_stack_ports(service, settings.public);
            if service.remove("container_name").is_some() {
                warn("container_name removed, swarm names tasks itself".into());
            }
        } else {
            rewrite_ports(service, settings.public);
        }

        let (cpus, memory_bytes, pids) = effective_limits(service, &settings, ctx.defaults);
        let mut limits = Map::new();
        if let Some(cpus) = cpus {
            limits.insert("cpus".into(), if ctx.stack { json!(cpus.to_string()) } else { json!(cpus) });
        }
        if let Some(memory) = memory_bytes {
            limits.insert("memory".into(), json!(memory.to_string()));
        }
        if let Some(pids) = pids {
            limits.insert("pids".into(), json!(pids));
        }
        let deploy = object_entry(service, "deploy");
        let replicas = deploy.get("replicas").and_then(as_u64).unwrap_or(1);
        set_limits(deploy, limits);
        if ctx.stack {
            let update = object_entry(deploy, "update_config");
            update.insert("order".into(), json!("start-first"));
            update.entry("failure_action").or_insert(json!("rollback"));
            object_entry(deploy, "rollback_config").insert("order".into(), json!("start-first"));
        }
        if service.get("deploy").and_then(Value::as_object).is_some_and(Map::is_empty) {
            service.remove("deploy");
        }
        requests.push(Request {
            service: name.clone(),
            replicas,
            cpus: cpus.unwrap_or(0.0),
            memory_bytes: memory_bytes.unwrap_or(0),
        });

        let labels = object_entry(service, "labels");
        labels.insert(LABEL_MANAGED.into(), json!("true"));
        labels.insert(LABEL_DEPLOYMENT.into(), json!(ctx.deployment_id));
        labels.insert(LABEL_SERVICE.into(), json!(name));
        let net = [
            (LABEL_NET_IN, settings.net_in_mbit.or(ctx.defaults.net_in_mbit)),
            (LABEL_NET_OUT, settings.net_out_mbit.or(ctx.defaults.net_out_mbit)),
            (LABEL_NET_CONN_OUT, settings.net_conn_out_mbit.or(ctx.defaults.net_conn_out_mbit)),
        ];
        for (label, _) in &net {
            labels.remove(*label);
        }
        let net: Vec<(&str, f64)> = net.into_iter().filter_map(|(label, v)| Some((label, v.filter(|v| *v > 0.0)?))).collect();
        if ctx.stack {
            if !net.is_empty() {
                warn("network limits are not applied in Swarm mode".into());
            }
        } else {
            for (label, mbit) in net {
                labels.insert(label.into(), json!(mbit.to_string()));
            }
        }

        if service.get("logging").and_then(|l| l["driver"].as_str()).is_some_and(|d| d != "json-file") {
            warn("logging driver replaced with json-file so kuroe can read logs".into());
        }
        service.insert(
            "logging".into(),
            json!({ "driver": "json-file", "options": { "max-size": "10m", "max-file": "3" } }),
        );

        if !service.contains_key("network_mode") {
            let networks = object_entry(service, "networks");
            if networks.is_empty() {
                networks.insert("default".into(), Value::Null);
            }
            networks.insert(proxy_network(ctx).into(), Value::Null);
        }

        for mount in &settings.volumes {
            let source = format!("kuroe_{}", mount.volume);
            array_entry(service, "volumes").push(json!({
                "type": "volume",
                "source": source,
                "target": mount.target,
                "read_only": mount.read_only,
            }));
            shared_volumes.push(source);
        }
    }

    let network = proxy_network(ctx);
    object_entry_value(&mut config, "networks").insert(network.into(), json!({ "name": network, "external": true }));
    if !shared_volumes.is_empty() {
        let volumes = object_entry_value(&mut config, "volumes");
        for source in shared_volumes {
            volumes.insert(source.clone(), json!({ "name": source, "external": true }));
        }
    }

    Ok(Rewritten { config, builds, requests, warnings })
}

fn proxy_network(ctx: &Context) -> &'static str {
    if ctx.stack { SWARM_NETWORK } else { PROXY_NETWORK }
}

/// Swarm's ingress can't bind to loopback, so private ports are left unpublished and reached over the overlay.
fn rewrite_stack_ports(service: &mut Map<String, Value>, public: bool) {
    if !public {
        service.remove("ports");
        return;
    }
    let Some(ports) = service.get_mut("ports").and_then(Value::as_array_mut) else { return };
    for port in ports.iter_mut().filter_map(Value::as_object_mut) {
        port.remove("host_ip");
        port.insert("mode".into(), json!("ingress"));
    }
}

fn strip_privileges(service: &mut Map<String, Value>, warn: &mut impl FnMut(String)) {
    if service.remove("privileged").is_some_and(|v| v == json!(true)) {
        warn("privileged removed".into());
    }
    for key in ["pid", "ipc", "network_mode", "userns_mode", "uts"] {
        if service.get(key).and_then(Value::as_str) == Some("host") {
            service.remove(key);
            warn(format!("{key}: host removed"));
        }
    }
    if service.remove("devices").is_some() {
        warn("devices removed".into());
    }
    if let Some(caps) = service.get_mut("cap_add").and_then(Value::as_array_mut) {
        caps.retain(|cap| {
            let cap = cap.as_str().unwrap_or_default();
            let allowed = CAP_ALLOWLIST.contains(&cap.trim_start_matches("CAP_"));
            if !allowed {
                warn(format!("cap_add {cap} removed"));
            }
            allowed
        });
    }
    if let Some(opts) = service.get_mut("security_opt").and_then(Value::as_array_mut) {
        opts.retain(|opt| {
            let opt = opt.as_str().unwrap_or_default();
            let unconfined = opt.contains("unconfined");
            if unconfined {
                warn(format!("security_opt {opt} removed"));
            }
            !unconfined
        });
    }
}

fn strip_bind_mounts(service: &mut Map<String, Value>, host_mounts: bool, warn: &mut impl FnMut(String)) {
    if host_mounts {
        return;
    }
    if let Some(volumes) = service.get_mut("volumes").and_then(Value::as_array_mut) {
        volumes.retain(|v| {
            let bind = v["type"].as_str() == Some("bind");
            if bind {
                warn(format!("bind mount {} removed, needs host_mounts", v["source"].as_str().unwrap_or("?")));
            }
            !bind
        });
    }
}

fn rewrite_ports(service: &mut Map<String, Value>, public: bool) {
    let Some(ports) = service.get_mut("ports").and_then(Value::as_array_mut) else { return };
    for port in ports.iter_mut().filter_map(Value::as_object_mut) {
        if public {
            port.remove("host_ip");
        } else {
            port.insert("host_ip".into(), json!("127.0.0.1"));
        }
    }
}

fn set_limits(deploy: &mut Map<String, Value>, limits: Map<String, Value>) {
    if !limits.is_empty() {
        object_entry(deploy, "resources").insert("limits".into(), Value::Object(limits));
        return;
    }
    let Some(resources) = deploy.get_mut("resources").and_then(Value::as_object_mut) else { return };
    resources.remove("limits");
    if resources.is_empty() {
        deploy.remove("resources");
    }
}

fn effective_limits(
    service: &mut Map<String, Value>,
    settings: &ServiceSettings,
    defaults: &Defaults,
) -> (Option<f64>, Option<u64>, Option<i64>) {
    let existing = &service.get("deploy").map(|d| d["resources"]["limits"].clone()).unwrap_or(Value::Null);
    let legacy_cpus = service.remove("cpus");
    let legacy_mem = service.remove("mem_limit");
    let legacy_pids = service.remove("pids_limit");
    service.remove("memswap_limit");

    let cpus = settings
        .cpus
        .or_else(|| as_f64(&existing["cpus"]))
        .or_else(|| legacy_cpus.as_ref().and_then(as_f64))
        .or(defaults.cpus);
    let memory = settings
        .memory_mb
        .map(|mb| mb * 1024 * 1024)
        .or_else(|| as_bytes(&existing["memory"]))
        .or_else(|| legacy_mem.as_ref().and_then(as_bytes))
        .or(defaults.memory_mb.map(|mb| mb * 1024 * 1024));
    let pids = settings
        .pids
        .or_else(|| existing["pids"].as_i64())
        .or_else(|| legacy_pids.as_ref().and_then(Value::as_i64))
        .or(defaults.pids);
    (cpus, memory, pids)
}

fn object_entry<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let entry = map.entry(key).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry.as_object_mut().unwrap()
}

fn object_entry_value<'a>(value: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    object_entry(value.as_object_mut().expect("compose config is an object"), key)
}

fn array_entry<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    let entry = map.entry(key).or_insert_with(|| json!([]));
    if !entry.is_array() {
        *entry = json!([]);
    }
    entry.as_array_mut().unwrap()
}

fn string_map(value: &Value) -> BTreeMap<String, String> {
    value
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))).collect())
        .unwrap_or_default()
}

fn as_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn as_f64(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}

pub fn as_bytes(value: &Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return Some(n);
    }
    let text = value.as_str()?.trim().to_ascii_lowercase();
    let text = text.trim_end_matches('b');
    let (digits, unit) = match text.find(|c: char| !c.is_ascii_digit() && c != '.') {
        Some(i) => text.split_at(i),
        None => (text, ""),
    };
    let multiplier: u64 = match unit {
        "" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        _ => return None,
    };
    Some((digits.parse::<f64>().ok()? * multiplier as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Defaults {
        Defaults { cpus: Some(1.0), memory_mb: Some(512), pids: Some(512), ..Default::default() }
    }

    #[test]
    fn network_limits_become_labels_with_defaults_filling_gaps() {
        let mut input = web_input();
        input["services"]["web"]["labels"] = json!({ "kuroe.net_in": "999" });
        input["services"]["web"]["labels"]["kuroe.net_conn_out"] = json!("999");
        let settings = BTreeMap::from([("web".into(), ServiceSettings { net_out_mbit: Some(5.5), ..Default::default() })]);
        let defaults = Defaults { net_in_mbit: Some(20.0), net_out_mbit: Some(50.0), ..Default::default() };
        let out = run_with(input, settings, false, &defaults);
        let labels = &out.config["services"]["web"]["labels"];
        assert_eq!(labels["kuroe.net_in"], "20", "a label from the compose file can't set the limit");
        assert_eq!(labels["kuroe.net_out"], "5.5");
        assert!(labels.get("kuroe.net_conn_out").is_none(), "{labels}");
        let settings = BTreeMap::from([("web".into(), ServiceSettings { net_conn_out_mbit: Some(2.0), ..Default::default() })]);
        let out = run_with(web_input(), settings, false, &Defaults::default());
        assert_eq!(out.config["services"]["web"]["labels"]["kuroe.net_conn_out"], "2");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn no_network_limit_means_no_labels() {
        let out = run_with(web_input(), BTreeMap::new(), false, &Defaults::default());
        let labels = &out.config["services"]["web"]["labels"];
        assert!(labels.get("kuroe.net_in").is_none() && labels.get("kuroe.net_out").is_none(), "{labels}");
    }

    #[test]
    fn swarm_skips_network_limits_with_a_warning() {
        let settings = BTreeMap::from([("web".into(), ServiceSettings { net_in_mbit: Some(10.0), ..Default::default() })]);
        let out = run_stack(web_input(), settings);
        assert!(out.config["services"]["web"]["labels"].get("kuroe.net_in").is_none());
        assert_eq!(out.warnings, ["web: network limits are not applied in Swarm mode"]);
    }

    fn run(input: Value, settings: BTreeMap<String, ServiceSettings>, host_mounts: bool) -> Rewritten {
        run_with(input, settings, host_mounts, &defaults())
    }

    fn run_with(input: Value, settings: BTreeMap<String, ServiceSettings>, host_mounts: bool, defaults: &Defaults) -> Rewritten {
        let ctx = Context { deployment_id: "dep1", revision: 3, settings: &settings, defaults, host_mounts, stack: false };
        rewrite(input, &ctx).unwrap()
    }

    #[test]
    fn unset_defaults_mean_no_limit_keys() {
        let out = run_with(web_input(), BTreeMap::new(), false, &Defaults::default());
        let web = &out.config["services"]["web"];
        assert!(web.get("deploy").is_none(), "no limits leaves no deploy block: {web}");
        assert_eq!(out.requests, [Request { service: "web".into(), replicas: 1, cpus: 0.0, memory_bytes: 0 }]);
    }

    #[test]
    fn unset_defaults_keep_partial_limits_from_settings_and_compose() {
        let mut input = web_input();
        input["services"]["web"]["deploy"] = json!({ "replicas": 2, "resources": { "limits": { "cpus": "0.5" } } });
        let settings = BTreeMap::from([("web".into(), ServiceSettings { memory_mb: Some(256), ..Default::default() })]);
        let out = run_with(input, settings, false, &Defaults::default());
        let deploy = &out.config["services"]["web"]["deploy"];
        assert_eq!(deploy["resources"]["limits"], json!({ "cpus": 0.5, "memory": "268435456" }));
        assert_eq!(deploy["replicas"], 2);
        assert_eq!(out.requests[0], Request { service: "web".into(), replicas: 2, cpus: 0.5, memory_bytes: 256 << 20 });
    }

    fn run_stack(input: Value, settings: BTreeMap<String, ServiceSettings>) -> Rewritten {
        let defaults = defaults();
        let ctx = Context { deployment_id: "dep1", revision: 3, settings: &settings, defaults: &defaults, host_mounts: false, stack: true };
        rewrite(input, &ctx).unwrap()
    }

    #[test]
    fn stack_leaves_private_ports_unpublished_and_updates_start_first() {
        let mut input = web_input();
        input["services"]["web"]["container_name"] = json!("fixed");
        input["services"]["web"]["deploy"] = json!({ "replicas": 2 });
        input["services"]["web"]["entrypoint"] = Value::Null;
        input["services"]["web"]["command"] = Value::Null;
        let out = run_stack(input, BTreeMap::new());
        let web = &out.config["services"]["web"];
        assert!(web.get("ports").is_none());
        assert!(web.get("entrypoint").is_none() && web.get("command").is_none(), "the stack schema rejects nulls");
        assert!(web.get("container_name").is_none());
        assert_eq!(web["networks"], json!({ "default": null, "kuroe_swarm": null }));
        assert_eq!(web["deploy"]["update_config"], json!({ "order": "start-first", "failure_action": "rollback" }));
        assert_eq!(web["deploy"]["rollback_config"], json!({ "order": "start-first" }));
        assert_eq!(web["deploy"]["replicas"], 2);
        assert_eq!(web["deploy"]["resources"]["limits"]["cpus"], "1", "docker stack's schema wants cpus as a string");
        assert_eq!(out.config["networks"]["kuroe_swarm"], json!({ "name": "kuroe_swarm", "external": true }));
        assert!(out.config["networks"].get("kuroe_proxy").is_none());
        assert_eq!(out.warnings, ["web: container_name removed, swarm names tasks itself"]);
    }

    #[test]
    fn stack_public_ports_go_on_the_routing_mesh() {
        let mut input = web_input();
        input["services"]["web"]["ports"][0]["host_ip"] = json!("127.0.0.1");
        let settings = BTreeMap::from([("web".into(), ServiceSettings { public: true, ..Default::default() })]);
        let out = run_stack(input, settings);
        assert_eq!(
            out.config["services"]["web"]["ports"],
            json!([{ "mode": "ingress", "target": 3000, "published": "3000", "protocol": "tcp" }])
        );
    }

    fn web_input() -> Value {
        json!({
            "name": "kuroe_shop",
            "services": {
                "web": {
                    "image": "nginx:1.29",
                    "ports": [{ "mode": "ingress", "target": 3000, "published": "3000", "protocol": "tcp" }],
                    "networks": { "default": null }
                }
            },
            "networks": { "default": { "name": "kuroe_shop_default" } }
        })
    }

    #[test]
    fn private_port_binds_loopback_with_default_limits() {
        let out = run(web_input(), BTreeMap::new(), false);
        assert_eq!(
            out.config,
            json!({
                "name": "kuroe_shop",
                "services": {
                    "web": {
                        "image": "nginx:1.29",
                        "ports": [{ "mode": "ingress", "target": 3000, "published": "3000", "protocol": "tcp", "host_ip": "127.0.0.1" }],
                        "networks": { "default": null, "kuroe_proxy": null },
                        "deploy": { "resources": { "limits": { "cpus": 1.0, "memory": "536870912", "pids": 512 } } },
                        "labels": { "kuroe.managed": "true", "kuroe.deployment": "dep1", "kuroe.service": "web" },
                        "logging": { "driver": "json-file", "options": { "max-size": "10m", "max-file": "3" } }
                    }
                },
                "networks": {
                    "default": { "name": "kuroe_shop_default" },
                    "kuroe_proxy": { "name": "kuroe_proxy", "external": true }
                }
            })
        );
        assert_eq!(out.requests, [Request { service: "web".into(), replicas: 1, cpus: 1.0, memory_bytes: 536870912 }]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn public_port_drops_host_ip() {
        let mut input = web_input();
        input["services"]["web"]["ports"][0]["host_ip"] = json!("10.0.0.5");
        let settings = BTreeMap::from([("web".into(), ServiceSettings { public: true, ..Default::default() })]);
        let out = run(input, settings, false);
        assert_eq!(
            out.config["services"]["web"]["ports"],
            json!([{ "mode": "ingress", "target": 3000, "published": "3000", "protocol": "tcp" }])
        );
    }

    #[test]
    fn limits_prefer_settings_then_compose_then_legacy() {
        let mut input = web_input();
        input["services"]["web"]["deploy"] = json!({ "replicas": 2, "resources": { "limits": { "cpus": "0.5" } } });
        input["services"]["web"]["mem_limit"] = json!("256m");
        input["services"]["web"]["pids_limit"] = json!(64);
        let settings = BTreeMap::from([("web".into(), ServiceSettings { pids: Some(100), ..Default::default() })]);
        let out = run(input, settings, false);
        let web = &out.config["services"]["web"];
        assert_eq!(web["deploy"]["resources"]["limits"], json!({ "cpus": 0.5, "memory": "268435456", "pids": 100 }));
        assert_eq!(web["deploy"]["replicas"], 2);
        assert!(web.get("mem_limit").is_none() && web.get("pids_limit").is_none());
        assert_eq!(out.requests[0].replicas, 2);
    }

    #[test]
    fn strips_privileges_and_reports_them() {
        let mut input = web_input();
        let web = input["services"]["web"].as_object_mut().unwrap();
        web.insert("privileged".into(), json!(true));
        web.insert("pid".into(), json!("host"));
        web.insert("cap_add".into(), json!(["NET_BIND_SERVICE", "SYS_ADMIN", "CAP_NET_ADMIN"]));
        web.insert("security_opt".into(), json!(["seccomp=unconfined", "no-new-privileges:true"]));
        web.insert("devices".into(), json!([{ "source": "/dev/kvm", "target": "/dev/kvm" }]));
        web.insert(
            "volumes".into(),
            json!([
                { "type": "bind", "source": "/var/run/docker.sock", "target": "/var/run/docker.sock" },
                { "type": "volume", "source": "data", "target": "/data" }
            ]),
        );
        let out = run(input, BTreeMap::new(), false);
        let web = &out.config["services"]["web"];
        assert!(web.get("privileged").is_none() && web.get("pid").is_none() && web.get("devices").is_none());
        assert_eq!(web["cap_add"], json!(["NET_BIND_SERVICE"]));
        assert_eq!(web["security_opt"], json!(["no-new-privileges:true"]));
        assert_eq!(web["volumes"], json!([{ "type": "volume", "source": "data", "target": "/data" }]));
        assert_eq!(
            out.warnings,
            [
                "web: privileged removed",
                "web: pid: host removed",
                "web: devices removed",
                "web: cap_add SYS_ADMIN removed",
                "web: cap_add CAP_NET_ADMIN removed",
                "web: security_opt seccomp=unconfined removed",
                "web: bind mount /var/run/docker.sock removed, needs host_mounts",
            ]
        );
    }

    #[test]
    fn host_mounts_keeps_binds() {
        let mut input = web_input();
        input["services"]["web"]["volumes"] = json!([{ "type": "bind", "source": "/srv/www", "target": "/www" }]);
        let out = run(input, BTreeMap::new(), true);
        assert_eq!(out.config["services"]["web"]["volumes"][0]["source"], "/srv/www");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn host_network_mode_is_removed_and_proxy_network_added() {
        let mut input = web_input();
        let web = input["services"]["web"].as_object_mut().unwrap();
        web.remove("networks");
        web.insert("network_mode".into(), json!("host"));
        let out = run(input, BTreeMap::new(), false);
        assert_eq!(out.config["services"]["web"]["networks"], json!({ "default": null, "kuroe_proxy": null }));
        assert_eq!(out.warnings, ["web: network_mode: host removed"]);
    }

    #[test]
    fn build_becomes_pinned_image() {
        let mut input = web_input();
        let web = input["services"]["web"].as_object_mut().unwrap();
        web.remove("image");
        web.insert("build".into(), json!({ "context": "/src/app", "dockerfile": "Dockerfile.prod", "args": { "V": "1" } }));
        let out = run(input, BTreeMap::new(), false);
        assert_eq!(out.config["services"]["web"]["image"], "kuroe/kuroe_shop-web:3");
        assert!(out.config["services"]["web"].get("build").is_none());
        assert_eq!(
            out.builds,
            [Build {
                service: "web".into(),
                context: "/src/app".into(),
                dockerfile: "Dockerfile.prod".into(),
                args: BTreeMap::from([("V".into(), "1".into())]),
                tag: "kuroe/kuroe_shop-web:3".into(),
            }]
        );
    }

    #[test]
    fn shared_volume_is_mounted_and_declared_external() {
        let mount = SharedMount { volume: "uploads".into(), target: "/srv/uploads".into(), read_only: false };
        let settings = BTreeMap::from([("web".into(), ServiceSettings { volumes: vec![mount], ..Default::default() })]);
        let out = run(web_input(), settings, false);
        assert_eq!(
            out.config["services"]["web"]["volumes"],
            json!([{ "type": "volume", "source": "kuroe_uploads", "target": "/srv/uploads", "read_only": false }])
        );
        assert_eq!(out.config["volumes"], json!({ "kuroe_uploads": { "name": "kuroe_uploads", "external": true } }));
    }

    #[test]
    fn parses_byte_units() {
        assert_eq!(as_bytes(&json!("512m")), Some(512 << 20));
        assert_eq!(as_bytes(&json!("1.5g")), Some(3 << 29));
        assert_eq!(as_bytes(&json!("1024")), Some(1024));
        assert_eq!(as_bytes(&json!(2048)), Some(2048));
        assert_eq!(as_bytes(&json!("12x")), None);
    }
}
