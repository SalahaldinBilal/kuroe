//! A domain for kuroe's own web UI, routed by the managed Traefik like a deployment's domain.

use std::net::SocketAddr;

use anyhow::{Context, bail};
use bollard::Docker;
use bollard::models::NetworkConnectRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;

use super::certs::RouteCert;
use super::routes::{self, Route};
use crate::docker::rewrite::PROXY_NETWORK;
use crate::update::{self, InstallMode};

/// Deployment names can't start with `_`, so this never collides with a deployment's route file.
pub const ROUTE_FILE: &str = "_ui";
const SERVICE: &str = "kuroe";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiDomain {
    pub hostname: String,
    pub tls: bool,
}

impl UiDomain {
    pub fn url(&self) -> String {
        format!("{}://{}", if self.tls { "https" } else { "http" }, self.hostname)
    }
}

pub async fn load(db: &SqlitePool) -> sqlx::Result<Option<UiDomain>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings WHERE key IN ('ui_domain', 'ui_tls')").fetch_all(db).await?;
    let get = |key: &str| rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.trim().to_owned()).unwrap_or_default();
    let hostname = get("ui_domain");
    Ok((!hostname.is_empty()).then(|| UiDomain { hostname, tls: get("ui_tls") != "false" }))
}

pub async fn save(db: &SqlitePool, ui: Option<&UiDomain>) -> sqlx::Result<()> {
    let values = [
        ("ui_domain", ui.map(|u| u.hostname.clone()).unwrap_or_default()),
        ("ui_tls", ui.is_none_or(|u| u.tls).to_string()),
    ];
    let mut tx = db.begin().await?;
    for (key, value) in values {
        sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
            .bind(key)
            .bind(value)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// Where Traefik reaches kuroe. In a container it's the container's name on `kuroe_proxy`, which kuroe joins first
/// when it isn't on it. Otherwise it's the host, through that network's gateway.
pub async fn upstream(docker: &Docker, listen: SocketAddr) -> anyhow::Result<String> {
    if let InstallMode::Container { id, name, .. } = update::detect(docker).await {
        let inspect = docker.inspect_container(&id, None).await.context("inspecting kuroe's own container")?;
        let joined = inspect.network_settings.and_then(|n| n.networks).is_some_and(|n| n.contains_key(PROXY_NETWORK));
        if !joined {
            docker
                .connect_network(PROXY_NETWORK, NetworkConnectRequest { container: id, ..Default::default() })
                .await
                .context("joining kuroe_proxy so Traefik can reach kuroe")?;
        }
        return Ok(name);
    }
    if listen.ip().is_loopback() {
        bail!("kuroe listens on {listen}, which Traefik can't reach. Start it with --listen 0.0.0.0:{}", listen.port());
    }
    let network = docker.inspect_network(PROXY_NETWORK, None).await.context("the kuroe_proxy network doesn't exist yet")?;
    network
        .ipam
        .and_then(|i| i.config)
        .unwrap_or_default()
        .into_iter()
        .find_map(|c| c.gateway.filter(|g| !g.is_empty()))
        .context("kuroe_proxy has no gateway address")
}

/// kuroe's address on the host, for a web server that forwards to it directly. In a container that's the host
/// port its listen port is published on.
pub async fn host_address(docker: &Docker, listen: SocketAddr) -> Option<String> {
    let InstallMode::Container { id, .. } = update::detect(docker).await else {
        return Some(format!("127.0.0.1:{}", listen.port()));
    };
    let inspect = docker.inspect_container(&id, None).await.ok()?;
    let ports = inspect.network_settings?.ports?;
    let bindings = ports.get(&format!("{}/tcp", listen.port()))?.clone()?;
    let port = bindings.iter().find_map(|b| b.host_port.clone().filter(|p| !p.is_empty()))?;
    Some(format!("127.0.0.1:{port}"))
}

pub fn render(ui: &UiDomain, tls: bool, upstream: &str, port: u16, cert: RouteCert) -> Value {
    let route = Route {
        hostname: ui.hostname.clone(),
        tls,
        service: SERVICE.into(),
        port,
        upstreams: vec![upstream.to_owned()],
        cert,
        extra: Default::default(),
    };
    routes::render(ROUTE_FILE, &[route])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn the_domain_is_saved_and_cleared() {
        let db = crate::db::test_pool().await;
        assert_eq!(load(&db).await.unwrap(), None);
        let ui = UiDomain { hostname: "kuroe.example.com".into(), tls: true };
        save(&db, Some(&ui)).await.unwrap();
        assert_eq!(load(&db).await.unwrap(), Some(ui.clone()));
        assert_eq!(ui.url(), "https://kuroe.example.com");
        save(&db, Some(&UiDomain { tls: false, ..ui })).await.unwrap();
        assert_eq!(load(&db).await.unwrap().map(|u| u.url()), Some("http://kuroe.example.com".into()));
        save(&db, None).await.unwrap();
        assert_eq!(load(&db).await.unwrap(), None);
    }

    #[test]
    fn the_route_sends_the_hostname_to_kuroe() {
        let ui = UiDomain { hostname: "kuroe.example.com".into(), tls: true };
        let out = render(&ui, true, "kuroe", 8080, RouteCert::Acme { wildcard_zone: None });
        let routers = &out["http"]["routers"];
        assert_eq!(routers["_ui-kuroe-example-com"]["rule"], "Host(`kuroe.example.com`)");
        assert_eq!(routers["_ui-kuroe-example-com"]["tls"], json!({ "certResolver": "le" }));
        assert_eq!(routers["_ui-kuroe-example-com-http"]["middlewares"], json!(["kuroe-https@file"]));
        assert_eq!(out["http"]["services"]["_ui-kuroe-8080"]["loadBalancer"]["servers"], json!([{ "url": "http://kuroe:8080" }]));

        let plain = render(&ui, false, "172.18.0.1", 9000, RouteCert::Default);
        assert_eq!(plain["http"]["routers"]["_ui-kuroe-example-com"]["entryPoints"], json!(["web"]));
        assert_eq!(plain["http"]["services"]["_ui-kuroe-9000"]["loadBalancer"]["servers"], json!([{ "url": "http://172.18.0.1:9000" }]));
    }
}
