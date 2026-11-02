use std::collections::BTreeSet;
use std::fmt::Write;

use super::{Entry, Mode, PROBE_HEADER, split_upstream};

/// One shared backend when every hostname goes to Traefik. External mode needs one per hostname for its own probe header.
fn shared(entries: &[Entry]) -> Option<&str> {
    let upstreams: BTreeSet<&str> = entries.iter().map(|e| e.upstream.as_str()).collect();
    match upstreams.into_iter().collect::<Vec<_>>().as_slice() {
        [one] if entries.iter().all(|e| e.probe.is_none()) => Some(one),
        _ => None,
    }
}

/// HAProxy config sections can't be split across files, so kuroe owns only backends and a hostname map.
/// The user's frontends pick a backend with one `use_backend` line each, added once.
pub fn backends(entries: &[Entry], mode: Mode, traefik_tls: &str) -> String {
    let mut out = "# Written by kuroe. Changes here are overwritten.\n".to_owned();
    if let Some(upstream) = shared(entries) {
        backend(&mut out, "kuroe_http", upstream, None);
    } else {
        for e in entries {
            backend(&mut out, &backend_name(&e.hostname), &e.upstream, e.probe.as_deref());
        }
    }
    if mode == Mode::Passthrough {
        let (host, port) = split_upstream(traefik_tls);
        let _ = writeln!(out, "\nbackend kuroe_tls\n    mode tcp\n    server traefik {host}:{port}");
    }
    out
}

fn backend(out: &mut String, name: &str, upstream: &str, probe: Option<&str>) {
    let _ = writeln!(out, "\nbackend {name}\n    mode http\n    option forwardfor\n    http-request set-header X-Forwarded-Proto https if {{ ssl_fc }}");
    if let Some(token) = probe {
        let _ = writeln!(out, "    http-response set-header {PROBE_HEADER} {token}");
    }
    let _ = writeln!(out, "    server upstream {upstream}");
}

pub fn backend_name(hostname: &str) -> String {
    format!("kuroe_{}", hostname.replace(['.', '-'], "_"))
}

/// One hostname per line. HAProxy's `map()` returns the second column, which is the backend to use.
pub fn map(entries: &[Entry]) -> String {
    let one = shared(entries).is_some();
    let mut out = String::new();
    for e in entries {
        let target = if one { "kuroe_http".to_owned() } else { backend_name(&e.hostname) };
        let _ = writeln!(out, "{} {target}", e.hostname);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::entries;
    use super::*;

    #[test]
    fn one_backend_and_a_map_of_hostnames() {
        let out = backends(&entries(false), Mode::Passthrough, "127.0.0.1:8443");
        assert!(out.contains("backend kuroe_http\n    mode http\n    option forwardfor"));
        assert!(out.contains("    server upstream 127.0.0.1:8080"));
        assert!(out.contains("backend kuroe_tls\n    mode tcp\n    server traefik 127.0.0.1:8443"));
        assert_eq!(map(&entries(false)), "shop.example.com kuroe_http\napi.example.com kuroe_http\n");
        assert!(!backends(&entries(false), Mode::Front, "x:1").contains("kuroe_tls"));
    }

    #[test]
    fn external_domains_get_a_backend_each() {
        let list = entries(true);
        let out = backends(&list, Mode::Front, "x:1");
        assert!(out.contains("backend kuroe_shop_example_com"));
        assert!(out.contains("http-response set-header x-kuroe-probe p1"));
        assert!(out.contains("http-response set-header x-kuroe-probe p2"), "same upstream, still its own probe");
        assert_eq!(map(&list), "shop.example.com kuroe_shop_example_com\napi.example.com kuroe_api_example_com\n");
    }
}
