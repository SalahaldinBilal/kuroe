//! Config for a web server that sits in front of kuroe (managed Traefik behind it, or external mode).

mod apache;
mod caddy;
mod haproxy;
pub mod install;
mod lighttpd;
mod nginx;
mod traefik;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Response header external mode adds so Verify can tell the request went through the user's proxy.
pub const PROBE_HEADER: &str = "x-kuroe-probe";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Nginx,
    Apache,
    Caddy,
    Haproxy,
    Traefik,
    Lighttpd,
}

impl Kind {
    pub const ALL: [Kind; 6] = [Kind::Nginx, Kind::Apache, Kind::Caddy, Kind::Haproxy, Kind::Traefik, Kind::Lighttpd];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Nginx => "nginx",
            Kind::Apache => "Apache",
            Kind::Caddy => "Caddy",
            Kind::Haproxy => "HAProxy",
            Kind::Traefik => "Traefik",
            Kind::Lighttpd => "lighttpd",
        }
    }

    /// Whether it can hand TLS to Traefik untouched, by reading only the SNI.
    pub fn passthrough(self) -> bool {
        matches!(self, Kind::Nginx | Kind::Haproxy | Kind::Traefik)
    }

    /// Whether it gets its own certificates, so no certificate paths are needed.
    pub fn own_certificates(self) -> bool {
        matches!(self, Kind::Caddy | Kind::Haproxy | Kind::Traefik)
    }

    pub fn default_path(self) -> &'static str {
        match self {
            Kind::Nginx => "/etc/nginx/conf.d/kuroe.conf",
            Kind::Apache => "/etc/apache2/sites-enabled/kuroe.conf",
            Kind::Caddy => "/etc/caddy/kuroe.caddy",
            Kind::Haproxy => "/etc/haproxy/conf.d/kuroe.cfg",
            Kind::Traefik => "/etc/traefik/dynamic/kuroe.yml",
            Kind::Lighttpd => "/etc/lighttpd/conf-enabled/90-kuroe.conf",
        }
    }

    /// A second file: nginx's `stream {}` part, HAProxy's hostname map.
    pub fn default_extra_path(self) -> Option<&'static str> {
        match self {
            Kind::Nginx => Some("/etc/nginx/kuroe-stream.conf"),
            Kind::Haproxy => Some("/etc/haproxy/kuroe.map"),
            _ => None,
        }
    }

    pub fn default_command(self) -> &'static str {
        match self {
            Kind::Nginx => "nginx -t && nginx -s reload",
            Kind::Apache => "apachectl configtest && apachectl graceful",
            Kind::Caddy => "caddy validate --config /etc/caddy/Caddyfile && caddy reload --config /etc/caddy/Caddyfile",
            Kind::Haproxy => "haproxy -c -f /etc/haproxy/haproxy.cfg -f /etc/haproxy/conf.d && systemctl reload haproxy",
            Kind::Traefik => "",
            Kind::Lighttpd => "lighttpd -tt -f /etc/lighttpd/lighttpd.conf && systemctl reload lighttpd",
        }
    }

    /// What to run inside the server's own container: a config test, and the signal that reloads it.
    pub fn container_reload(self) -> (Option<&'static [&'static str]>, Reload) {
        match self {
            Kind::Nginx => (Some(&["nginx", "-t"]), Reload::Signal("HUP")),
            Kind::Apache => (Some(&["httpd", "-t"]), Reload::Signal("USR1")),
            Kind::Caddy => (Some(&["caddy", "validate", "--config", "/etc/caddy/Caddyfile"]), Reload::Exec(&["caddy", "reload", "--config", "/etc/caddy/Caddyfile"])),
            Kind::Haproxy => (Some(&["haproxy", "-c", "-f", "/usr/local/etc/haproxy/haproxy.cfg"]), Reload::Signal("USR2")),
            Kind::Traefik => (None, Reload::None),
            Kind::Lighttpd => (Some(&["lighttpd", "-tt", "-f", "/etc/lighttpd/lighttpd.conf"]), Reload::Signal("INT")),
        }
    }

    /// The process names and `Server` header prefixes it shows up as.
    pub fn from_process(name: &str) -> Option<Kind> {
        match name.trim() {
            "nginx" => Some(Kind::Nginx),
            "apache2" | "httpd" => Some(Kind::Apache),
            "caddy" => Some(Kind::Caddy),
            "haproxy" => Some(Kind::Haproxy),
            "traefik" => Some(Kind::Traefik),
            "lighttpd" => Some(Kind::Lighttpd),
            _ => None,
        }
    }

    pub fn from_server_header(value: &str) -> Option<Kind> {
        let lower = value.to_ascii_lowercase();
        [("nginx", Kind::Nginx), ("apache", Kind::Apache), ("caddy", Kind::Caddy), ("lighttpd", Kind::Lighttpd), ("traefik", Kind::Traefik)]
            .into_iter()
            .find(|(prefix, _)| lower.starts_with(prefix))
            .map(|(_, kind)| kind)
    }

    /// An image like `nginx:1.27-alpine` or `library/httpd`.
    pub fn from_image(image: &str) -> Option<Kind> {
        let name = image.rsplit('/').next().unwrap_or(image);
        let name = name.split([':', '@']).next().unwrap_or(name);
        match name {
            "nginx" | "nginx-unprivileged" | "openresty" => Some(Kind::Nginx),
            "httpd" | "apache" | "apache2" => Some(Kind::Apache),
            "caddy" => Some(Kind::Caddy),
            "haproxy" => Some(Kind::Haproxy),
            "traefik" => Some(Kind::Traefik),
            "lighttpd" => Some(Kind::Lighttpd),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reload {
    Signal(&'static str),
    Exec(&'static [&'static str]),
    None,
}

/// One hostname to send to an upstream.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub hostname: String,
    /// `host:port` the web server forwards plain HTTP to.
    pub upstream: String,
    pub tls: bool,
    /// The `x-kuroe-probe` value external mode adds so Verify can recognise the route.
    pub probe: Option<String>,
}

/// Where the web server keeps certificates when it terminates TLS itself. `{host}` is replaced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CertPaths {
    pub cert: String,
    pub key: String,
}

impl Default for CertPaths {
    fn default() -> Self {
        CertPaths { cert: "/etc/letsencrypt/live/{host}/fullchain.pem".into(), key: "/etc/letsencrypt/live/{host}/privkey.pem".into() }
    }
}

impl CertPaths {
    pub fn for_host(&self, host: &str) -> (String, String) {
        (self.cert.replace("{host}", host), self.key.replace("{host}", host))
    }
}

/// The web server in front of kuroe, and how kuroe keeps its config in sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub kind: Kind,
    /// Who does TLS. Only used when managed Traefik runs behind the server.
    #[serde(default)]
    pub mode: Mode,
    pub path: String,
    #[serde(default)]
    pub extra_path: Option<String>,
    /// Test and reload on this machine. Ignored when `container` is set.
    #[serde(default)]
    pub command: String,
    /// The server runs in this container, so kuroe tests and reloads it there.
    #[serde(default)]
    pub container: Option<String>,
    /// Rewrite and reload whenever domains change.
    #[serde(default)]
    pub auto: bool,
    #[serde(default)]
    pub cert_paths: CertPaths,
    /// Where the server's own HTTPS sites move to with passthrough.
    #[serde(default = "default_rest_tls")]
    pub rest_tls: String,
}

fn default_rest_tls() -> String {
    "127.0.0.1:4443".into()
}

impl Settings {
    pub fn defaults(kind: Kind) -> Settings {
        Settings {
            kind,
            mode: Mode::Front,
            path: kind.default_path().into(),
            extra_path: kind.default_extra_path().map(str::to_owned),
            command: kind.default_command().into(),
            container: None,
            auto: false,
            cert_paths: CertPaths::default(),
            rest_tls: default_rest_tls(),
        }
    }

    pub fn reloader(&self) -> install::Reloader {
        match &self.container {
            Some(name) if !name.trim().is_empty() => install::Reloader::Container { name: name.trim().to_owned(), kind: self.kind },
            _ => install::Reloader::Command(self.command.clone()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The web server terminates TLS and forwards HTTP.
    #[default]
    Front,
    /// The web server reads the SNI and hands TLS to Traefik.
    Passthrough,
}

pub struct Render<'a> {
    pub kind: Kind,
    pub mode: Mode,
    pub entries: &'a [Entry],
    pub cert_paths: &'a CertPaths,
    /// Traefik's TLS address, for passthrough.
    pub traefik_tls: &'a str,
    /// Where the web server's own HTTPS sites move to, for passthrough.
    pub rest_tls: &'a str,
    pub path: PathBuf,
    pub extra_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct File {
    pub path: PathBuf,
    pub body: String,
}

pub fn split_upstream(upstream: &str) -> (&str, &str) {
    upstream.rsplit_once(':').unwrap_or((upstream, "80"))
}

pub fn render(r: &Render) -> anyhow::Result<Vec<File>> {
    if r.mode == Mode::Passthrough && !r.kind.passthrough() {
        anyhow::bail!("{} can't pass TLS through to Traefik, let it handle HTTPS instead", r.kind.label());
    }
    let main = |body: String| File { path: r.path.clone(), body };
    let extra = |body: String| -> anyhow::Result<File> {
        let path = r.extra_path.clone().or_else(|| r.kind.default_extra_path().map(PathBuf::from));
        Ok(File { path: path.ok_or_else(|| anyhow::anyhow!("{} needs a second file path", r.kind.label()))?, body })
    };
    Ok(match (r.kind, r.mode) {
        (Kind::Nginx, Mode::Front) => vec![main(nginx::front(r.entries, r.cert_paths))],
        (Kind::Nginx, Mode::Passthrough) => {
            vec![main(nginx::front_http(r.entries)), extra(nginx::stream(r.entries, r.traefik_tls, r.rest_tls))?]
        }
        (Kind::Apache, _) => vec![main(apache::front(r.entries, r.cert_paths))],
        (Kind::Caddy, _) => vec![main(caddy::front(r.entries))],
        (Kind::Haproxy, mode) => {
            let map_path = extra(String::new())?.path;
            vec![main(haproxy::backends(r.entries, mode, r.traefik_tls)), File { path: map_path, body: haproxy::map(r.entries) }]
        }
        (Kind::Traefik, mode) => vec![main(traefik::file(r.entries, mode, r.traefik_tls))],
        (Kind::Lighttpd, _) => vec![main(lighttpd::front(r.entries, r.cert_paths))],
    })
}

/// What the user adds to their own config once, so kuroe's file gets loaded.
pub fn one_time_steps(kind: Kind, mode: Mode, path: &str, extra: Option<&str>, rest_tls: &str) -> Vec<String> {
    let extra = extra.filter(|e| !e.is_empty()).or(kind.default_extra_path()).unwrap_or_default();
    match (kind, mode) {
        (Kind::Nginx, Mode::Passthrough) => vec![
            format!("Add `include {extra};` at the top level of nginx.conf, outside the http block. The stream module has to be loaded."),
            format!("Move your own HTTPS sites from `listen 443` to `listen {rest_tls} ssl`."),
        ],
        (Kind::Nginx, Mode::Front) => vec![format!("Make sure nginx.conf includes {path}, conf.d is included by default.")],
        (Kind::Apache, _) => vec![
            "Enable the modules: a2enmod proxy proxy_http proxy_wstunnel headers ssl".into(),
            format!("Make sure {path} is loaded, sites-enabled is included by default on Debian."),
        ],
        (Kind::Caddy, _) => vec![format!("Add `import {path}` to your Caddyfile.")],
        (Kind::Haproxy, Mode::Front) => vec![
            format!("Start HAProxy with an extra `-f {}` so it loads kuroe's backends.", parent(path)),
            format!(
                "In your HTTP and HTTPS frontends, add: use_backend %[req.hdr(host),lower,map({extra})] if {{ req.hdr(host),lower,map({extra}) -m found }}"
            ),
        ],
        (Kind::Haproxy, Mode::Passthrough) => vec![
            format!("Start HAProxy with an extra `-f {}` so it loads kuroe's backends.", parent(path)),
            format!(
                "In your HTTP frontend, add: use_backend %[req.hdr(host),lower,map({extra})] if {{ req.hdr(host),lower,map({extra}) -m found }}"
            ),
            format!(
                "In a TCP frontend on 443, add: tcp-request inspect-delay 5s, tcp-request content accept if {{ req_ssl_hello_type 1 }}, use_backend kuroe_tls if {{ req.ssl_sni,lower,map({extra}) -m found }}"
            ),
            format!("Send other HTTPS traffic from that frontend to your own sites, which move to {rest_tls}."),
        ],
        (Kind::Traefik, _) => vec![format!("Point Traefik's file provider at the folder of {path}, with watch on.")],
        (Kind::Lighttpd, _) => vec!["Load mod_proxy, mod_setenv and mod_openssl in server.modules.".into()],
    }
}

/// Where the certificate files of HTTPS hostnames have to be before the config test passes.
pub fn certificate_steps(kind: Kind, entries: &[Entry], certs: &CertPaths) -> Vec<String> {
    let hosts: Vec<&str> = entries.iter().filter(|e| e.tls).map(|e| e.hostname.as_str()).collect();
    if hosts.is_empty() {
        return Vec::new();
    }
    if kind == Kind::Traefik {
        return vec!["The HTTPS routers use a certificate resolver named letsencrypt. Change it if yours has another name.".into()];
    }
    if kind.own_certificates() {
        return Vec::new();
    }
    let mut steps = vec![format!("Each HTTPS hostname needs its certificate at {} and its key at {} before the config test passes.", certs.cert, certs.key)];
    let plugin = match kind {
        Kind::Nginx => Some("nginx"),
        Kind::Apache => Some("apache"),
        _ => None,
    };
    if let Some(plugin) = plugin.filter(|_| *certs == CertPaths::default()) {
        steps.push(match hosts.as_slice() {
            [one] => format!("Get it with: sudo certbot certonly --{plugin} -d {one}"),
            many => format!("Get them with: for h in {}; do sudo certbot certonly --{plugin} -d $h; done", many.join(" ")),
        });
    }
    steps
}

fn parent(path: &str) -> String {
    std::path::Path::new(path).parent().map(|p| p.display().to_string()).unwrap_or_else(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn entries(probe: bool) -> Vec<Entry> {
        let probe = |t: &str| probe.then(|| t.to_owned());
        vec![
            Entry { hostname: "shop.example.com".into(), upstream: "127.0.0.1:8080".into(), tls: true, probe: probe("p1") },
            Entry { hostname: "api.example.com".into(), upstream: "127.0.0.1:8080".into(), tls: false, probe: probe("p2") },
        ]
    }

    fn render_with(kind: Kind, mode: Mode) -> anyhow::Result<Vec<File>> {
        render(&Render {
            kind,
            mode,
            entries: &entries(false),
            cert_paths: &CertPaths::default(),
            traefik_tls: "127.0.0.1:8443",
            rest_tls: "127.0.0.1:4443",
            path: kind.default_path().into(),
            extra_path: None,
        })
    }

    #[test]
    fn passthrough_only_where_the_server_can_do_it() {
        for kind in Kind::ALL {
            let result = render_with(kind, Mode::Passthrough);
            assert_eq!(result.is_ok(), kind.passthrough(), "{kind:?}");
        }
        assert_eq!(render_with(Kind::Nginx, Mode::Passthrough).unwrap().len(), 2, "nginx needs a stream file too");
        assert_eq!(render_with(Kind::Haproxy, Mode::Front).unwrap()[1].path, PathBuf::from("/etc/haproxy/kuroe.map"));
    }

    #[test]
    fn detection_names_map_to_servers() {
        assert_eq!(Kind::from_process("apache2"), Some(Kind::Apache));
        assert_eq!(Kind::from_process("httpd"), Some(Kind::Apache));
        assert_eq!(Kind::from_process("sshd"), None);
        assert_eq!(Kind::from_server_header("nginx/1.27.3"), Some(Kind::Nginx));
        assert_eq!(Kind::from_server_header("Apache/2.4.62 (Debian)"), Some(Kind::Apache));
        assert_eq!(Kind::from_server_header("Caddy"), Some(Kind::Caddy));
        assert_eq!(Kind::from_server_header("cloudflare"), None);
        assert_eq!(Kind::from_image("nginx:1.27-alpine"), Some(Kind::Nginx));
        assert_eq!(Kind::from_image("docker.io/library/httpd:2.4"), Some(Kind::Apache));
        assert_eq!(Kind::from_image("ghcr.io/acme/web:1"), None);
    }

    #[test]
    fn certificate_steps_only_for_servers_that_need_files() {
        let list = entries(false);
        let nginx = certificate_steps(Kind::Nginx, &list, &CertPaths::default());
        assert_eq!(nginx[1], "Get it with: sudo certbot certonly --nginx -d shop.example.com", "api.example.com has no TLS");
        assert!(certificate_steps(Kind::Caddy, &list, &CertPaths::default()).is_empty());
        assert!(certificate_steps(Kind::Nginx, &list[1..], &CertPaths::default()).is_empty());
        let own = CertPaths { cert: "/certs/{host}.crt".into(), key: "/certs/{host}.key".into() };
        assert_eq!(certificate_steps(Kind::Apache, &list, &own).len(), 1, "no certbot line for custom paths");
    }

    #[test]
    fn cert_paths_fill_in_the_host() {
        let (cert, key) = CertPaths::default().for_host("shop.example.com");
        assert_eq!(cert, "/etc/letsencrypt/live/shop.example.com/fullchain.pem");
        assert_eq!(key, "/etc/letsencrypt/live/shop.example.com/privkey.pem");
    }
}
