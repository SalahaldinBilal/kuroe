//! `http_access = local`: plain HTTP only from this machine, everything else through an HTTPS proxy.

use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sqlx::SqlitePool;

use super::AppState;
use crate::docker::rewrite::PROXY_NETWORK;

pub const SETTING: &str = "http_access";
pub const TRUSTED_SETTING: &str = "trusted_proxies";
const PEERS_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpAccess {
    Open,
    Local,
}

impl HttpAccess {
    pub fn parse(text: &str) -> Option<HttpAccess> {
        match text {
            "" | "open" => Some(HttpAccess::Open),
            "local" => Some(HttpAccess::Local),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            HttpAccess::Open => "open",
            HttpAccess::Local => "local",
        }
    }
}

/// An address or a CIDR range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl Net {
    pub fn parse(text: &str) -> Option<Net> {
        let text = text.trim();
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr.parse::<IpAddr>().ok()?, Some(prefix.parse::<u8>().ok()?)),
            None => (text.parse::<IpAddr>().ok()?, None),
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(max);
        (prefix <= max).then_some(Net { addr: addr.to_canonical(), prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(self.prefix)).unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - u32::from(self.prefix)).unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

/// Who may still use plain HTTP with `local`.
#[derive(Debug, Clone, Default)]
pub struct Peers {
    /// The host as seen from inside kuroe's container. None on the binary install, where the host is loopback.
    pub host_gateway: Option<IpAddr>,
    /// The `kuroe_proxy` network, where managed Traefik connects from.
    pub proxy_nets: Vec<Net>,
    /// Addresses from the `trusted_proxies` setting.
    pub trusted: Vec<Net>,
}

pub fn allowed(peer: IpAddr, forwarded_https: bool, peers: &Peers) -> bool {
    let peer = peer.to_canonical();
    if peer.is_loopback() || peers.host_gateway == Some(peer) {
        return true;
    }
    forwarded_https && peers.proxy_nets.iter().chain(&peers.trusted).any(|n| n.contains(peer))
}

/// Traefik sends `wss` instead of `https` for websocket upgrades over TLS.
pub fn forwarded_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https") || v.eq_ignore_ascii_case("wss"))
}

pub struct Settings {
    pub access: HttpAccess,
    pub trusted: Vec<Net>,
    pub public_url: Option<String>,
}

pub async fn load(db: &SqlitePool) -> sqlx::Result<Settings> {
    let rows: BTreeMap<String, String> = sqlx::query_as::<_, (String, String)>(
        "SELECT key, value FROM settings WHERE key IN ('http_access', 'trusted_proxies', 'public_url')",
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();
    Ok(Settings {
        access: rows.get(SETTING).and_then(|v| HttpAccess::parse(v)).unwrap_or(HttpAccess::Open),
        trusted: rows.get(TRUSTED_SETTING).map(|v| parse_list(v)).unwrap_or_default(),
        public_url: rows.get("public_url").filter(|v| !v.is_empty()).cloned(),
    })
}

/// Comma or space separated addresses and ranges. Anything unreadable is left out.
pub fn parse_list(text: &str) -> Vec<Net> {
    text.split([',', ' ', '\n']).filter(|s| !s.trim().is_empty()).filter_map(Net::parse).collect()
}

pub async fn set(db: &SqlitePool, access: HttpAccess) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(SETTING)
        .bind(access.as_str())
        .execute(db)
        .await?;
    Ok(())
}

#[derive(Default)]
pub struct HttpState {
    peers: Mutex<Option<(Instant, Peers)>>,
    /// Sessions that signed in and haven't read `/me` yet, for the plain-HTTP banner.
    banners: Mutex<HashSet<String>>,
}

impl HttpState {
    pub fn flag_banner(&self, token: &str) {
        self.banners.lock().unwrap().insert(token.to_owned());
    }

    pub fn take_banner(&self, token: &str) -> bool {
        self.banners.lock().unwrap().remove(token)
    }
}

/// The host gateway and proxy network, refreshed once a minute. Trusted proxies come from the settings each time.
pub async fn peers(state: &AppState, trusted: Vec<Net>) -> Peers {
    let cached = state.http_access.peers.lock().unwrap().as_ref().filter(|(at, _)| at.elapsed() < PEERS_TTL).map(|(_, p)| p.clone());
    let mut found = match cached {
        Some(p) => p,
        None => {
            let in_container = std::fs::read_to_string("/proc/self/mountinfo")
                .ok()
                .and_then(|m| crate::update::container_id_from_mountinfo(&m))
                .is_some();
            let host_gateway = in_container
                .then(|| std::fs::read_to_string("/proc/net/route").ok().and_then(|t| crate::proxy::ports::default_gateway(&t)))
                .flatten()
                .and_then(|g| g.parse().ok());
            let proxy_nets = match state.docker.inspect_network(PROXY_NETWORK, None).await {
                Ok(net) => net
                    .ipam
                    .and_then(|i| i.config)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|c| c.subnet.as_deref().and_then(Net::parse))
                    .collect(),
                Err(_) => Vec::new(),
            };
            let p = Peers { host_gateway, proxy_nets, trusted: Vec::new() };
            *state.http_access.peers.lock().unwrap() = Some((Instant::now(), p.clone()));
            p
        }
    };
    found.trusted = trusted;
    found
}

/// Whether this request would still get through with `local`.
pub async fn request_allowed(state: &AppState, peer: Option<SocketAddr>, headers: &HeaderMap, trusted: Vec<Net>) -> bool {
    let Some(peer) = peer else { return false };
    allowed(peer.ip(), forwarded_https(headers), &peers(state, trusted).await)
}

pub fn refusal(public_url: Option<&str>) -> String {
    let use_url = match public_url {
        Some(url) if url.starts_with("https://") => format!("Use {url}, or run"),
        _ => "Use its HTTPS address, or run".into(),
    };
    format!("kuroe only answers over HTTPS here. {use_url} `kuroe admin http open` on the server.\n")
}

pub async fn gate(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, request: Request, next: Next) -> Response {
    let settings = match load(&state.db).await {
        Ok(s) => s,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("reading settings: {err}")).into_response(),
    };
    if settings.access == HttpAccess::Open {
        return next.run(request).await;
    }
    if request_allowed(&state, Some(peer), request.headers(), settings.trusted).await {
        return next.run(request).await;
    }
    (StatusCode::FORBIDDEN, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], refusal(settings.public_url.as_deref())).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn allow_and_deny_table() {
        let peers = Peers {
            host_gateway: Some(ip("172.17.0.1")),
            proxy_nets: vec![Net::parse("172.20.0.0/16").unwrap()],
            trusted: parse_list("10.1.2.3, fd00::/8"),
        };
        let cases = [
            ("127.0.0.1", false, true, "loopback"),
            ("::1", false, true, "IPv6 loopback"),
            ("::ffff:127.0.0.1", false, true, "mapped loopback"),
            ("172.17.0.1", false, true, "host side"),
            ("172.20.0.5", true, true, "proxy network over HTTPS"),
            ("172.20.0.5", false, false, "proxy network over plain HTTP"),
            ("10.1.2.3", true, true, "trusted proxy"),
            ("10.1.2.4", true, false, "next to a trusted proxy"),
            ("fd00::7", true, true, "trusted IPv6 range"),
            ("203.0.113.9", true, false, "public address with a forged header"),
            ("203.0.113.9", false, false, "public address"),
        ];
        for (peer, https, expected, what) in cases {
            assert_eq!(allowed(ip(peer), https, &peers), expected, "{what}");
        }
        assert!(!allowed(ip("172.17.0.1"), false, &Peers::default()), "the gateway only counts inside a container");
    }

    #[test]
    fn nets_and_settings_parse() {
        assert!(Net::parse("10.0.0.0/8").unwrap().contains(ip("10.200.1.1")));
        assert!(!Net::parse("10.0.0.0/8").unwrap().contains(ip("11.0.0.1")));
        assert!(Net::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
        assert!(Net::parse("10.0.0.0/33").is_none());
        assert!(Net::parse("nonsense").is_none());
        assert_eq!(parse_list("10.0.0.1,, bad 192.168.0.0/24").len(), 2);
        assert_eq!(HttpAccess::parse(""), Some(HttpAccess::Open));
        assert_eq!(HttpAccess::parse("local"), Some(HttpAccess::Local));
        assert_eq!(HttpAccess::parse("closed"), None);
        assert!(refusal(Some("https://kuroe.example.com")).contains("Use https://kuroe.example.com"));
    }

    #[test]
    fn forwarded_https_takes_websockets_over_tls() {
        let proto = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-proto", value.parse().unwrap());
            forwarded_https(&headers)
        };
        assert!(proto("https"));
        assert!(proto("HTTPS"));
        assert!(proto("wss"));
        assert!(!proto("http"));
        assert!(!proto("ws"));
        assert!(!forwarded_https(&HeaderMap::new()));
    }

    #[tokio::test]
    async fn setting_round_trip() {
        let db = crate::db::test_pool().await;
        assert_eq!(load(&db).await.unwrap().access, HttpAccess::Open);
        set(&db, HttpAccess::Local).await.unwrap();
        assert_eq!(load(&db).await.unwrap().access, HttpAccess::Local);
        set(&db, HttpAccess::Open).await.unwrap();
        assert_eq!(load(&db).await.unwrap().access, HttpAccess::Open);
    }
}
