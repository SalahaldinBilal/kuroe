use std::collections::HashMap;
use std::time::Duration;

use bollard::Docker;
use bollard::query_parameters::ListContainersOptionsBuilder;
use serde::Serialize;

use super::traefik::CONTAINER as KUROE_TRAEFIK;
use super::webserver::Kind;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Holder {
    pub port: u16,
    /// The server it looks like, when it's one kuroe knows.
    pub by: Option<Kind>,
    /// What to call it in a message: `nginx`, `the container web`, `something`.
    pub what: String,
    /// The container that holds the port, when it's one.
    pub container: Option<String>,
    pub how: &'static str,
}

/// What holds each port, leaving out kuroe's own Traefik. Free ports are left out of the result.
pub async fn detect(docker: &Docker, http_probe: &reqwest::Client, ports: &[u16], binary_install: bool) -> Vec<Holder> {
    let mut found = Vec::new();
    let published = published_ports(docker).await;
    for &port in ports {
        if let Some((name, image)) = published.get(&port) {
            if name != KUROE_TRAEFIK {
                let by = Kind::from_image(image);
                let what = by.map(|k| k.label().to_owned()).unwrap_or_else(|| format!("the container {name}"));
                found.push(Holder { port, by, what, container: Some(name.clone()), how: "a Docker container publishes it" });
            }
            continue;
        }
        if binary_install && let Some(process) = process_on_port(port) {
            let by = Kind::from_process(&process);
            let what = by.map(|k| k.label().to_owned()).unwrap_or_else(|| process.clone());
            found.push(Holder { port, by, what, container: None, how: "a process on this machine listens on it" });
            continue;
        }
        if let Some(server) = probe(http_probe, &probe_host(binary_install), port).await {
            let by = server.as_deref().and_then(Kind::from_server_header);
            let what = by.map(|k| k.label().to_owned()).unwrap_or_else(|| "something".into());
            found.push(Holder { port, by, what, container: None, how: "it answered a request" });
        }
    }
    found
}

/// Published host ports of running containers, with the container's name and image.
async fn published_ports(docker: &Docker) -> HashMap<u16, (String, String)> {
    let mut map = HashMap::new();
    let Ok(containers) = docker.list_containers(Some(ListContainersOptionsBuilder::new().build())).await else { return map };
    for c in containers {
        let name = c.names.as_ref().and_then(|n| n.first()).map(|n| n.trim_start_matches('/').to_owned()).unwrap_or_default();
        let image = c.image.clone().unwrap_or_default();
        for p in c.ports.iter().flatten() {
            if let Some(public) = p.public_port {
                map.entry(public).or_insert_with(|| (name.clone(), image.clone()));
            }
        }
    }
    map
}

/// Socket inodes listening on `port` in a `/proc/net/tcp` or `tcp6` table.
pub fn listening_inodes(table: &str, port: u16) -> Vec<String> {
    let wanted = format!("{port:04X}");
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let local_port = cols.get(1)?.rsplit(':').next()?;
            (local_port == wanted && cols.get(3) == Some(&"0A")).then(|| cols.get(9).map(|s| s.to_string()))?
        })
        .collect()
}

fn process_on_port(port: u16) -> Option<String> {
    let mut inodes = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = std::fs::read_to_string(table) {
            inodes.extend(listening_inodes(&text, port));
        }
    }
    if inodes.is_empty() {
        return None;
    }
    let targets: Vec<String> = inodes.iter().map(|i| format!("socket:[{i}]")).collect();
    for proc_entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let pid = proc_entry.file_name();
        if !pid.to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(proc_entry.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            if let Ok(link) = std::fs::read_link(fd.path())
                && targets.iter().any(|t| link.to_string_lossy() == *t)
            {
                return std::fs::read_to_string(proc_entry.path().join("comm")).ok().map(|c| c.trim().to_owned());
            }
        }
    }
    Some("an unknown process".into())
}

/// The machine's own address as seen from here: loopback for the binary, the default gateway from a container.
fn probe_host(binary_install: bool) -> String {
    if binary_install {
        return "127.0.0.1".into();
    }
    std::fs::read_to_string("/proc/net/route").ok().and_then(|t| default_gateway(&t)).unwrap_or_else(|| "172.17.0.1".into())
}

/// The gateway of the default route in `/proc/net/route`, which stores addresses as little-endian hex.
pub fn default_gateway(table: &str) -> Option<String> {
    let line = table.lines().skip(1).find(|l| l.split_whitespace().nth(1) == Some("00000000"))?;
    let hex = line.split_whitespace().nth(2)?;
    let n = u32::from_str_radix(hex, 16).ok()?;
    let [a, b, c, d] = n.to_le_bytes();
    Some(format!("{a}.{b}.{c}.{d}"))
}

/// `Some(server header)` when something answers on the port, `None` when nothing does.
async fn probe(client: &reqwest::Client, host: &str, port: u16) -> Option<Option<String>> {
    let scheme = if port == 443 { "https" } else { "http" };
    let response = client.get(format!("{scheme}://{host}:{port}/")).timeout(Duration::from_secs(3)).send().await;
    match response {
        Ok(res) => Some(res.headers().get("server").and_then(|v| v.to_str().ok()).map(str::to_owned)),
        Err(err) if err.is_connect() || err.is_timeout() => None,
        Err(_) => Some(None),
    }
}

/// "nginx is using ports 80 and 443" or "the container web is using port 80".
pub fn describe(holders: &[Holder]) -> Option<String> {
    let first = holders.first()?;
    let ports: Vec<String> = holders.iter().map(|h| h.port.to_string()).collect();
    let same = holders.iter().all(|h| h.what == first.what);
    let ports = if ports.len() == 1 { format!("port {}", ports[0]) } else { format!("ports {}", ports.join(" and ")) };
    if same { Some(format!("{} is using {ports}", first.what)) } else { Some(format!("other programs are using {ports}")) }
}

pub fn port_error(text: &str) -> bool {
    text.contains("port is already allocated") || text.contains("address already in use")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 31337 1 0000000000000000 100 0 0 10 0
   1: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 4242 1 0000000000000000 100 0 0 10 0
   2: 0100007F:0050 0100007F:9C40 01 00000000:00000000 00:00000000 00000000     0        0 999 1 0000000000000000 100 0 0 10 0
";

    #[test]
    fn finds_the_listening_socket_for_a_port() {
        assert_eq!(listening_inodes(TCP, 80), ["31337"], "the connected socket on 80 isn't a listener");
        assert_eq!(listening_inodes(TCP, 8080), ["4242"]);
        assert!(listening_inodes(TCP, 443).is_empty());
    }

    #[test]
    fn reads_the_default_gateway() {
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t010011AC\t0003\t0\t0\t0\t00000000\t0\t0\t0
eth0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
";
        assert_eq!(default_gateway(route).as_deref(), Some("172.17.0.1"));
    }

    #[test]
    fn messages_name_the_server_and_ports() {
        let holder = |port, what: &str| Holder { port, by: None, what: what.into(), container: None, how: "" };
        assert_eq!(describe(&[holder(80, "nginx"), holder(443, "nginx")]).unwrap(), "nginx is using ports 80 and 443");
        assert_eq!(describe(&[holder(443, "Caddy")]).unwrap(), "Caddy is using port 443");
        assert_eq!(describe(&[holder(80, "nginx"), holder(443, "Apache")]).unwrap(), "other programs are using ports 80 and 443");
        assert!(describe(&[]).is_none());
        assert!(port_error("Bind for 0.0.0.0:80 failed: port is already allocated"));
    }
}
