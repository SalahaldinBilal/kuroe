use std::fmt::Write;

use super::{CertPaths, Entry, PROBE_HEADER};

const HEADER: &str = "# Written by kuroe. Changes here are overwritten.\n";

fn location(out: &mut String, e: &Entry) {
    let _ = writeln!(out, "    location / {{");
    let _ = writeln!(out, "        proxy_pass http://{};", e.upstream);
    for line in [
        "proxy_http_version 1.1;",
        "proxy_set_header Host $host;",
        "proxy_set_header X-Real-IP $remote_addr;",
        "proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;",
        "proxy_set_header X-Forwarded-Proto $scheme;",
        "proxy_set_header Upgrade $http_upgrade;",
        "proxy_set_header Connection \"upgrade\";",
        "proxy_buffering off;",
        // Longer than a domain's own timeout in Traefik, so Traefik's 504 and error page answer first.
        "proxy_read_timeout 3700s;",
        "proxy_send_timeout 3700s;",
    ] {
        let _ = writeln!(out, "        {line}");
    }
    if let Some(token) = &e.probe {
        let _ = writeln!(out, "        add_header {PROBE_HEADER} {token} always;");
    }
    let _ = writeln!(out, "    }}");
}

/// One server block per hostname. An HTTPS hostname gets its certificate files, and plain HTTP redirects to it.
pub fn front(entries: &[Entry], certs: &CertPaths) -> String {
    let mut out = HEADER.to_owned();
    for e in entries {
        if e.tls {
            let _ = writeln!(out, "\nserver {{");
            let _ = writeln!(out, "    listen 80;");
            let _ = writeln!(out, "    listen [::]:80;");
            let _ = writeln!(out, "    server_name {};", e.hostname);
            let _ = writeln!(out, "    return 301 https://$host$request_uri;");
            let _ = writeln!(out, "}}");
        }
        let _ = writeln!(out, "\nserver {{");
        if e.tls {
            let (cert, key) = certs.for_host(&e.hostname);
            let _ = writeln!(out, "    listen 443 ssl;");
            let _ = writeln!(out, "    listen [::]:443 ssl;");
            let _ = writeln!(out, "    ssl_certificate {cert};");
            let _ = writeln!(out, "    ssl_certificate_key {key};");
        } else {
            let _ = writeln!(out, "    listen 80;");
            let _ = writeln!(out, "    listen [::]:80;");
        }
        let _ = writeln!(out, "    server_name {};", e.hostname);
        out.push('\n');
        location(&mut out, e);
        let _ = writeln!(out, "}}");
    }
    out
}

/// Port 80 only. HTTPS goes through the stream block untouched.
pub fn front_http(entries: &[Entry]) -> String {
    let plain: Vec<Entry> = entries.iter().map(|e| Entry { tls: false, ..e.clone() }).collect();
    front(&plain, &CertPaths::default())
}

/// Sends kuroe's hostnames to Traefik by SNI and everything else to the user's own HTTPS sites.
pub fn stream(entries: &[Entry], traefik_tls: &str, rest_tls: &str) -> String {
    let mut out = HEADER.to_owned();
    out.push_str("stream {\n    map $ssl_preread_server_name $kuroe_upstream {\n");
    for e in entries {
        let _ = writeln!(out, "        {} {traefik_tls};", e.hostname);
    }
    let _ = writeln!(out, "        default {rest_tls};");
    out.push_str("    }\n\n    server {\n        listen 443;\n        ssl_preread on;\n        proxy_pass $kuroe_upstream;\n    }\n}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::entries;
    use super::*;

    #[test]
    fn front_terminates_tls_per_host() {
        assert_eq!(
            front(&entries(false)[..1], &CertPaths::default()),
            "# Written by kuroe. Changes here are overwritten.

server {
    listen 80;
    listen [::]:80;
    server_name shop.example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    listen [::]:443 ssl;
    ssl_certificate /etc/letsencrypt/live/shop.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/shop.example.com/privkey.pem;
    server_name shop.example.com;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection \"upgrade\";
        proxy_buffering off;
        proxy_read_timeout 3700s;
        proxy_send_timeout 3700s;
    }
}
"
        );
        assert!(front(&entries(true), &CertPaths::default()).contains("add_header x-kuroe-probe p2 always;"));
        let plain = front(&entries(false)[1..], &CertPaths::default());
        assert!(
            plain.contains("    listen 80;\n    listen [::]:80;\n    server_name api.example.com;\n\n    location / {"),
            "HTTP hostnames are served on 80:\n{plain}"
        );
        assert!(!plain.contains("return 301"));
    }

    #[test]
    fn stream_routes_by_sni() {
        let out = stream(&entries(false), "127.0.0.1:8443", "127.0.0.1:4443");
        assert!(out.contains("        shop.example.com 127.0.0.1:8443;\n        api.example.com 127.0.0.1:8443;\n        default 127.0.0.1:4443;"));
        assert!(out.contains("ssl_preread on;"));
        assert!(!front_http(&entries(false)).contains("443"), "port 80 only next to the stream block");
    }
}
