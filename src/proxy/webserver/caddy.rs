use std::fmt::Write;

use super::{Entry, PROBE_HEADER};

/// Caddy gets certificates itself for sites without a scheme, and forwards headers and websockets on its own.
pub fn front(entries: &[Entry]) -> String {
    let mut out = "# Written by kuroe. Changes here are overwritten.\n".to_owned();
    for e in entries {
        let site = if e.tls { e.hostname.clone() } else { format!("http://{}", e.hostname) };
        let _ = writeln!(out, "\n{site} {{");
        let _ = writeln!(out, "    reverse_proxy {}", e.upstream);
        if let Some(token) = &e.probe {
            let _ = writeln!(out, "    header {PROBE_HEADER} {token}");
        }
        let _ = writeln!(out, "}}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::entries;
    use super::*;

    #[test]
    fn sites_with_automatic_https() {
        assert_eq!(
            front(&entries(true)),
            "# Written by kuroe. Changes here are overwritten.

shop.example.com {
    reverse_proxy 127.0.0.1:8080
    header x-kuroe-probe p1
}

http://api.example.com {
    reverse_proxy 127.0.0.1:8080
    header x-kuroe-probe p2
}
"
        );
    }
}
