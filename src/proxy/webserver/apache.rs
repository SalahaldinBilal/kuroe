use std::fmt::Write;

use super::{CertPaths, Entry, PROBE_HEADER};

fn host(out: &mut String, e: &Entry, port: u16, scheme: &str, certs: Option<(String, String)>) {
    let _ = writeln!(out, "\n<VirtualHost *:{port}>");
    let _ = writeln!(out, "    ServerName {}", e.hostname);
    if let Some((cert, key)) = certs {
        let _ = writeln!(out, "    SSLEngine on");
        let _ = writeln!(out, "    SSLCertificateFile {cert}");
        let _ = writeln!(out, "    SSLCertificateKeyFile {key}");
    }
    let _ = writeln!(out, "    ProxyPreserveHost On");
    let _ = writeln!(out, "    ProxyPass / http://{}/ upgrade=websocket", e.upstream);
    let _ = writeln!(out, "    ProxyPassReverse / http://{}/", e.upstream);
    let _ = writeln!(out, "    RequestHeader set X-Forwarded-Proto \"{scheme}\"");
    if let Some(token) = &e.probe {
        let _ = writeln!(out, "    Header always set {PROBE_HEADER} \"{token}\"");
    }
    let _ = writeln!(out, "</VirtualHost>");
}

/// A virtual host per hostname on 80, plus one on 443 with its certificate when the domain uses TLS.
pub fn front(entries: &[Entry], certs: &CertPaths) -> String {
    let mut out = "# Written by kuroe. Changes here are overwritten.\n".to_owned();
    for e in entries {
        host(&mut out, e, 80, "http", None);
        if e.tls {
            host(&mut out, e, 443, "https", Some(certs.for_host(&e.hostname)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::entries;
    use super::*;

    #[test]
    fn virtual_hosts_with_websockets_and_forwarded_proto() {
        let out = front(&entries(true), &CertPaths::default());
        assert!(out.contains(
            "<VirtualHost *:443>
    ServerName shop.example.com
    SSLEngine on
    SSLCertificateFile /etc/letsencrypt/live/shop.example.com/fullchain.pem
    SSLCertificateKeyFile /etc/letsencrypt/live/shop.example.com/privkey.pem
    ProxyPreserveHost On
    ProxyPass / http://127.0.0.1:8080/ upgrade=websocket
    ProxyPassReverse / http://127.0.0.1:8080/
    RequestHeader set X-Forwarded-Proto \"https\"
    Header always set x-kuroe-probe \"p1\"
</VirtualHost>"
        ));
        assert_eq!(out.matches("<VirtualHost").count(), 3, "api.example.com has no TLS");
    }
}
