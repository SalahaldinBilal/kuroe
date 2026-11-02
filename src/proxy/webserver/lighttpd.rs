use std::fmt::Write;

use super::{CertPaths, Entry, PROBE_HEADER, split_upstream};

/// A host condition per hostname with mod_proxy. TLS certificates are picked per host by SNI.
pub fn front(entries: &[Entry], certs: &CertPaths) -> String {
    let mut out = "# Written by kuroe. Changes here are overwritten.\n".to_owned();
    for e in entries {
        let (host, port) = split_upstream(&e.upstream);
        let _ = writeln!(out, "\n$HTTP[\"host\"] == \"{}\" {{", e.hostname);
        let _ = writeln!(out, "    proxy.server = ( \"\" => ( ( \"host\" => \"{host}\", \"port\" => {port} ) ) )");
        let _ = writeln!(out, "    proxy.header = ( \"upgrade\" => \"enable\" )");
        if e.tls {
            let (cert, key) = certs.for_host(&e.hostname);
            let _ = writeln!(out, "    ssl.pemfile = \"{cert}\"");
            let _ = writeln!(out, "    ssl.privkey = \"{key}\"");
        }
        if let Some(token) = &e.probe {
            let _ = writeln!(out, "    setenv.add-response-header = ( \"{PROBE_HEADER}\" => \"{token}\" )");
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
    fn host_conditions_with_proxy_and_sni_certificates() {
        let out = front(&entries(true), &CertPaths::default());
        assert!(out.contains(
            "$HTTP[\"host\"] == \"shop.example.com\" {
    proxy.server = ( \"\" => ( ( \"host\" => \"127.0.0.1\", \"port\" => 8080 ) ) )
    proxy.header = ( \"upgrade\" => \"enable\" )
    ssl.pemfile = \"/etc/letsencrypt/live/shop.example.com/fullchain.pem\"
    ssl.privkey = \"/etc/letsencrypt/live/shop.example.com/privkey.pem\"
    setenv.add-response-header = ( \"x-kuroe-probe\" => \"p1\" )
}"
        ));
        assert!(!out.split("api.example.com").nth(1).unwrap().contains("ssl.pemfile"));
    }
}
