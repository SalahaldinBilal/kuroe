pub mod bluegreen;
pub mod certs;
pub mod options;
pub mod ports;
pub mod routes;
pub mod traefik;
pub mod ui;
pub mod webserver;

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::db::now;
pub use options::RouteOptions;

macro_rules! select_domains {
    ($rest:literal) => {
        concat!(
            "SELECT id, deployment_id, hostname, service, port, tls, probe_token, verified_at, created_at, certificate, options FROM domains ",
            $rest
        )
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Managed,
    External,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxySettings {
    pub mode: Mode,
    pub acme_email: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
    pub drain_seconds: u64,
    #[serde(default)]
    pub cert_mode: certs::CertMode,
    /// Cloudflare identity for DNS challenges.
    #[serde(default)]
    pub dns_identity: Option<String>,
    /// One `*.zone` certificate per zone instead of one per hostname.
    #[serde(default)]
    pub dns_wildcard: bool,
    #[serde(default)]
    pub bind: Bind,
    /// Where Traefik listens with `bind = behind`, as `ip:port`.
    #[serde(default = "default_behind_http")]
    pub behind_http: String,
    #[serde(default = "default_behind_https")]
    pub behind_https: String,
    /// The web server in front, for `bind = behind` and for external mode.
    #[serde(default)]
    pub web_server: Option<webserver::Settings>,
    /// Traefik logs every request to its container output.
    #[serde(default)]
    pub access_log: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Bind {
    /// Traefik takes the HTTP and HTTPS ports on every address.
    #[default]
    Ports,
    /// Traefik listens on loopback and another web server forwards to it.
    Behind,
}

impl ProxySettings {
    /// Traefik runs behind a web server that terminates TLS, so its routes are plain HTTP without a redirect.
    pub fn tls_in_front(&self) -> bool {
        self.mode == Mode::Managed && self.bind == Bind::Behind && self.web_server.as_ref().is_none_or(|w| w.mode == webserver::Mode::Front)
    }
}

/// Not 8080, which many apps on the host already use.
fn default_behind_http() -> String {
    "127.0.0.1:8081".into()
}

fn default_behind_https() -> String {
    "127.0.0.1:8443".into()
}

/// `127.0.0.1:8080` into its address and port.
pub fn parse_address(text: &str) -> anyhow::Result<(String, u16)> {
    let (ip, port) = text.trim().rsplit_once(':').ok_or_else(|| anyhow::anyhow!("{text} needs an address and a port, like 127.0.0.1:8080"))?;
    let port: u16 = port.parse().map_err(|_| anyhow::anyhow!("{text} has no valid port"))?;
    let ip: std::net::IpAddr = ip.trim_matches(['[', ']']).parse().map_err(|_| anyhow::anyhow!("{text} has no valid IP address"))?;
    Ok((ip.to_string(), port))
}

async fn setting(db: &SqlitePool, key: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(db).await
}

pub async fn load_settings(db: &SqlitePool) -> anyhow::Result<ProxySettings> {
    let mode = match setting(db, "proxy_mode").await?.as_deref() {
        None | Some("off") => Mode::Off,
        Some("managed") => Mode::Managed,
        Some("external") => Mode::External,
        Some(other) => anyhow::bail!("setting proxy_mode has unknown value {other}"),
    };
    let number = |value: Option<String>, key: &str, default: u64| -> anyhow::Result<u64> {
        value
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().parse().map_err(|_| anyhow::anyhow!("setting {key} is not a number: {v}")))
            .transpose()
            .map(|v| v.unwrap_or(default))
    };
    Ok(ProxySettings {
        mode,
        acme_email: setting(db, "acme_email").await?.filter(|e| !e.is_empty()),
        http_port: number(setting(db, "proxy_http_port").await?, "proxy_http_port", 80)? as u16,
        https_port: number(setting(db, "proxy_https_port").await?, "proxy_https_port", 443)? as u16,
        drain_seconds: number(setting(db, "drain_seconds").await?, "drain_seconds", 10)?,
        cert_mode: match setting(db, "cert_mode").await?.as_deref() {
            None | Some("") | Some("http") => certs::CertMode::Http,
            Some("dns") => certs::CertMode::Dns,
            Some("none") => certs::CertMode::None,
            Some(other) => anyhow::bail!("setting cert_mode has unknown value {other}"),
        },
        dns_identity: setting(db, "dns_identity").await?.filter(|v| !v.is_empty()),
        dns_wildcard: setting(db, "dns_wildcard").await?.as_deref() == Some("true"),
        bind: if setting(db, "proxy_bind").await?.as_deref() == Some("behind") { Bind::Behind } else { Bind::Ports },
        behind_http: setting(db, "behind_http").await?.filter(|v| !v.is_empty()).unwrap_or_else(default_behind_http),
        behind_https: setting(db, "behind_https").await?.filter(|v| !v.is_empty()).unwrap_or_else(default_behind_https),
        web_server: match setting(db, "web_server").await?.filter(|v| !v.is_empty()) {
            Some(json) => Some(serde_json::from_str(&json).map_err(|e| anyhow::anyhow!("setting web_server can't be read: {e}"))?),
            None => None,
        },
        access_log: setting(db, "proxy_access_log").await?.as_deref() == Some("true"),
    })
}

pub async fn save_settings(db: &SqlitePool, s: &ProxySettings) -> sqlx::Result<()> {
    let mode = match s.mode {
        Mode::Off => "off",
        Mode::Managed => "managed",
        Mode::External => "external",
    };
    let values = [
        ("proxy_mode", mode.to_owned()),
        ("acme_email", s.acme_email.clone().unwrap_or_default()),
        ("proxy_http_port", s.http_port.to_string()),
        ("proxy_https_port", s.https_port.to_string()),
        ("drain_seconds", s.drain_seconds.to_string()),
        (
            "cert_mode",
            match s.cert_mode {
                certs::CertMode::Http => "http",
                certs::CertMode::Dns => "dns",
                certs::CertMode::None => "none",
            }
            .to_owned(),
        ),
        ("dns_identity", s.dns_identity.clone().unwrap_or_default()),
        ("dns_wildcard", s.dns_wildcard.to_string()),
        ("proxy_bind", if s.bind == Bind::Behind { "behind" } else { "ports" }.to_owned()),
        ("behind_http", s.behind_http.clone()),
        ("behind_https", s.behind_https.clone()),
        ("web_server", s.web_server.as_ref().map(|w| serde_json::to_string(w).expect("settings serialize")).unwrap_or_default()),
        ("proxy_access_log", s.access_log.to_string()),
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

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Domain {
    pub id: String,
    pub deployment_id: String,
    pub hostname: String,
    /// Empty, with port 0, for a domain that redirects.
    pub service: String,
    pub port: i64,
    pub tls: bool,
    pub probe_token: String,
    pub verified_at: Option<i64>,
    pub created_at: i64,
    /// `None` picks by hostname, `acme` forces Let's Encrypt, otherwise a certificate identity id.
    pub certificate: Option<String>,
    #[sqlx(try_from = "String")]
    pub options: RouteOptions,
}

impl Domain {
    /// Every (service, port) this domain sends traffic to, its own first. Nothing for a redirect.
    pub fn targets(&self) -> Vec<(String, u16)> {
        let mut targets = Vec::new();
        if !self.service.is_empty() && self.port > 0 {
            targets.push((self.service.clone(), self.port as u16));
        }
        for (service, port) in self.options.targets() {
            if !targets.iter().any(|(s, p)| s == service && *p == port) {
                targets.push((service.to_owned(), port));
            }
        }
        targets
    }
}

pub fn valid_hostname(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

pub async fn create_domain(
    db: &SqlitePool,
    deployment_id: &str,
    hostname: &str,
    service: &str,
    port: u16,
    tls: bool,
) -> sqlx::Result<Domain> {
    let mut token = [0u8; 12];
    rand::rng().fill_bytes(&mut token);
    let domain = Domain {
        id: uuid::Uuid::new_v4().to_string(),
        deployment_id: deployment_id.to_owned(),
        hostname: hostname.to_ascii_lowercase(),
        service: service.to_owned(),
        port: port.into(),
        tls,
        probe_token: token.iter().map(|b| format!("{b:02x}")).collect(),
        verified_at: None,
        created_at: now(),
        certificate: None,
        options: RouteOptions::default(),
    };
    sqlx::query(
        "INSERT INTO domains (id, deployment_id, hostname, service, port, tls, probe_token, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&domain.id)
    .bind(&domain.deployment_id)
    .bind(&domain.hostname)
    .bind(&domain.service)
    .bind(domain.port)
    .bind(domain.tls)
    .bind(&domain.probe_token)
    .bind(domain.created_at)
    .execute(db)
    .await?;
    crate::events::changed(&domain.deployment_id);
    Ok(domain)
}

fn domain_changed(deployment_id: Option<String>) {
    if let Some(id) = deployment_id {
        crate::events::changed(&id);
    }
}

pub async fn domains_for(db: &SqlitePool, deployment_id: &str) -> sqlx::Result<Vec<Domain>> {
    sqlx::query_as(select_domains!("WHERE deployment_id = ? ORDER BY hostname")).bind(deployment_id).fetch_all(db).await
}

pub async fn domain(db: &SqlitePool, id: &str) -> sqlx::Result<Option<Domain>> {
    sqlx::query_as(select_domains!("WHERE id = ?")).bind(id).fetch_optional(db).await
}

pub async fn set_domain_certificate(db: &SqlitePool, id: &str, certificate: Option<&str>) -> sqlx::Result<()> {
    let deployment = sqlx::query_scalar("UPDATE domains SET certificate = ? WHERE id = ? RETURNING deployment_id")
        .bind(certificate)
        .bind(id)
        .fetch_optional(db)
        .await?;
    domain_changed(deployment);
    Ok(())
}

/// Where a domain sends traffic. A redirect passes an empty service and port 0.
pub async fn set_domain_target(db: &SqlitePool, id: &str, service: &str, port: u16, options: &RouteOptions) -> sqlx::Result<()> {
    let deployment = sqlx::query_scalar("UPDATE domains SET service = ?, port = ?, options = ? WHERE id = ? RETURNING deployment_id")
        .bind(service)
        .bind(port)
        .bind(options.to_json())
        .bind(id)
        .fetch_optional(db)
        .await?;
    domain_changed(deployment);
    Ok(())
}

pub async fn all_domains(db: &SqlitePool) -> sqlx::Result<Vec<Domain>> {
    sqlx::query_as(select_domains!("ORDER BY hostname")).fetch_all(db).await
}

pub async fn tls_domains(db: &SqlitePool) -> sqlx::Result<Vec<Domain>> {
    sqlx::query_as(select_domains!("WHERE tls = 1 ORDER BY hostname")).fetch_all(db).await
}

pub async fn delete_domain(db: &SqlitePool, id: &str) -> sqlx::Result<()> {
    let deployment = sqlx::query_scalar("DELETE FROM domains WHERE id = ? RETURNING deployment_id").bind(id).fetch_optional(db).await?;
    domain_changed(deployment);
    Ok(())
}

pub async fn mark_verified(db: &SqlitePool, id: &str) -> sqlx::Result<()> {
    let deployment = sqlx::query_scalar("UPDATE domains SET verified_at = ? WHERE id = ? RETURNING deployment_id")
        .bind(now())
        .bind(id)
        .fetch_optional(db)
        .await?;
    domain_changed(deployment);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;

    #[test]
    fn hostnames() {
        assert!(valid_hostname("shop.example.com"));
        assert!(valid_hostname("a-b.test"));
        assert!(!valid_hostname("localhost"));
        assert!(!valid_hostname("-x.test"));
        assert!(!valid_hostname("x..test"));
        assert!(!valid_hostname("x_y.test"));
    }

    #[tokio::test]
    async fn domains_keep_their_routing_and_redirects_need_no_port() {
        let db = test_pool().await;
        let owner = crate::auth::create_user(&db, "boss", "boss-password-1", crate::auth::Role::owner()).await.unwrap();
        let spec = crate::deploy::Spec::new(crate::deploy::Source::image("nginx"));
        let d = crate::deploy::create(&db, "web", &spec, &owner).await.unwrap();
        let plain = create_domain(&db, &d.id, "web.example.com", "app", 80, true).await.unwrap();
        assert!(domain(&db, &plain.id).await.unwrap().unwrap().options.is_empty());

        let options = RouteOptions {
            paths: vec![options::PathRule { path: "/api/".into(), exact: false, service: "api".into(), port: 3420, error_page: None }],
            compress: true,
            ..Default::default()
        };
        set_domain_target(&db, &plain.id, "app", 80, &options).await.unwrap();
        let stored = domain(&db, &plain.id).await.unwrap().unwrap();
        assert_eq!(stored.options, options);
        assert_eq!(stored.targets(), [("app".to_owned(), 80), ("api".to_owned(), 3420)]);

        let old = create_domain(&db, &d.id, "old.example.com", "app", 80, true).await.unwrap();
        let redirect = RouteOptions { redirect: Some("https://web.example.com".into()), ..Default::default() };
        set_domain_target(&db, &old.id, "", 0, &redirect).await.unwrap();
        let stored = domain(&db, &old.id).await.unwrap().unwrap();
        assert_eq!((stored.service.as_str(), stored.port), ("", 0));
        assert!(stored.targets().is_empty(), "a redirect sends traffic to no service");
    }

    #[tokio::test]
    async fn settings_round_trip_with_defaults() {
        let db = test_pool().await;
        let defaults = load_settings(&db).await.unwrap();
        assert_eq!(
            defaults,
            ProxySettings {
                mode: Mode::Off,
                acme_email: None,
                http_port: 80,
                https_port: 443,
                drain_seconds: 10,
                cert_mode: certs::CertMode::Http,
                dns_identity: None,
                dns_wildcard: false,
                bind: Bind::Ports,
                behind_http: "127.0.0.1:8081".into(),
                behind_https: "127.0.0.1:8443".into(),
                web_server: None,
                access_log: false,
            }
        );
        let next = ProxySettings {
            mode: Mode::Managed,
            acme_email: Some("ops@example.com".into()),
            http_port: 8000,
            https_port: 8443,
            drain_seconds: 3,
            cert_mode: certs::CertMode::Dns,
            dns_identity: Some("cf1".into()),
            dns_wildcard: true,
            bind: Bind::Behind,
            behind_http: "127.0.0.1:9080".into(),
            behind_https: "127.0.0.1:9443".into(),
            web_server: Some(webserver::Settings::defaults(webserver::Kind::Nginx)),
            access_log: true,
        };
        save_settings(&db, &next).await.unwrap();
        assert_eq!(load_settings(&db).await.unwrap(), next);
    }
}
