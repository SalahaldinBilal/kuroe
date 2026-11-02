use std::collections::{BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use anyhow::{Context, bail};
use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, RestartPolicy, RestartPolicyNameEnum};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use futures::StreamExt;
use serde_json::Value;

use sha2::{Digest, Sha256};

use super::certs::{CERTS_DIR, CertMode};
use super::routes::{CERT_RESOLVER, shared_config};
use super::{Bind, Mode, ProxySettings};
use crate::docker::rewrite::{LABEL_MANAGED, PROXY_NETWORK};

pub const CONTAINER: &str = "kuroe_traefik";
pub const IMAGE: &str = "traefik:3.7.16";
const DYNAMIC_DIR: &str = "/etc/traefik/dynamic";
const LABEL_CONFIG: &str = "kuroe.config";
const API_PORT: u16 = 8080;

#[derive(Clone)]
pub struct Traefik {
    pub docker: Docker,
    pub http: reqwest::Client,
}

/// Private ranges, where the web server in front connects from through Docker's port binding.
const FRONT_IPS: &str = "127.0.0.1/32,::1/128,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,fc00::/7";

fn args(s: &ProxySettings) -> Vec<String> {
    let mut args = base_args();
    if s.access_log {
        args.push("--accesslog=true".into());
    }
    if s.bind == Bind::Behind {
        for entrypoint in ["web", "websecure"] {
            args.push(format!("--entrypoints.{entrypoint}.forwardedHeaders.trustedIPs={FRONT_IPS}"));
        }
    }
    let Some(email) = &s.acme_email else { return args };
    let resolver = format!("--certificatesresolvers.{CERT_RESOLVER}.acme");
    match s.cert_mode {
        CertMode::None => {}
        CertMode::Http => args.extend([
            format!("{resolver}.email={email}"),
            format!("{resolver}.storage=/acme/acme.json"),
            format!("{resolver}.httpchallenge.entrypoint=web"),
        ]),
        CertMode::Dns => args.extend([
            format!("{resolver}.email={email}"),
            format!("{resolver}.storage=/acme/acme.json"),
            format!("{resolver}.dnschallenge.provider=cloudflare"),
            format!("{resolver}.dnschallenge.resolvers=1.1.1.1:53,1.0.0.1:53"),
        ]),
    }
    args
}

/// Traefik reads the Cloudflare token from the environment for DNS challenges.
fn env(s: &ProxySettings, dns_token: Option<&str>) -> Vec<String> {
    match (s.cert_mode, dns_token) {
        (CertMode::Dns, Some(token)) => vec![format!("CF_DNS_API_TOKEN={token}")],
        _ => Vec::new(),
    }
}

fn base_args() -> Vec<String> {
    let args = vec![
        format!("--providers.file.directory={DYNAMIC_DIR}"),
        "--providers.file.watch=true".into(),
        "--providers.providersThrottleDuration=200ms".into(),
        "--entrypoints.web.address=:80".into(),
        "--entrypoints.websecure.address=:443".into(),
        "--entrypoints.websecure.http.tls=true".into(),
        // Traefik's default of 60 s for a whole request cuts off big uploads, which nginx allowed while data kept coming.
        "--entrypoints.web.transport.respondingTimeouts.readTimeout=0".into(),
        "--entrypoints.websecure.transport.respondingTimeouts.readTimeout=0".into(),
        "--api.insecure=true".into(),
        "--ping=true".into(),
        "--log.level=WARN".into(),
    ];
    args
}

/// Changes when anything Traefik is started with changes. The token is hashed so the label doesn't show it.
fn fingerprint(s: &ProxySettings, dns_token: Option<&str>) -> String {
    let token = env(s, dns_token).first().map(|t| hex(&Sha256::digest(t.as_bytes())[..8])).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    let ports = host_ports(s).map(|p| format!("{p:?}")).unwrap_or_default();
    (IMAGE, args(s), ports, BINDS, token, s.dns_identity.as_deref()).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Host address and port for Traefik's HTTP and HTTPS entry points. Behind another server they sit on loopback.
pub fn host_ports(s: &ProxySettings) -> anyhow::Result<[(Option<String>, u16); 2]> {
    Ok(match s.bind {
        Bind::Ports => [(None, s.http_port), (None, s.https_port)],
        Bind::Behind => {
            let (http_ip, http_port) = super::parse_address(&s.behind_http)?;
            let (https_ip, https_port) = super::parse_address(&s.behind_https)?;
            [(Some(http_ip), http_port), (Some(https_ip), https_port)]
        }
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const BINDS: [&str; 3] = ["kuroe_traefik_dynamic:/etc/traefik/dynamic", "kuroe_traefik_acme:/acme", "kuroe_traefik_certs:/etc/traefik/certs"];

impl Traefik {
    pub async fn apply(&self, settings: &ProxySettings, dns_token: Option<&str>) -> anyhow::Result<()> {
        let existing = self.docker.inspect_container(CONTAINER, None).await.ok();
        if settings.mode != Mode::Managed {
            if existing.is_some() {
                self.remove_container().await?;
            }
            return Ok(());
        }
        if settings.cert_mode == CertMode::Dns && dns_token.is_none() {
            bail!("Let's Encrypt over DNS needs a Cloudflare identity");
        }
        let wanted = fingerprint(settings, dns_token);
        let current = existing
            .as_ref()
            .and_then(|c| c.config.as_ref())
            .and_then(|c| c.labels.as_ref())
            .and_then(|l| l.get(LABEL_CONFIG).cloned());
        let running = existing.as_ref().and_then(|c| c.state.as_ref()).and_then(|s| s.running).unwrap_or(false);
        if current.as_deref() == Some(wanted.as_str()) && running {
            return self.write("_kuroe", &shared_config()).await;
        }
        if existing.is_some() {
            self.remove_container().await?;
        }
        self.create(settings, &env(settings, dns_token), &wanted).await?;
        self.write("_kuroe", &shared_config()).await?;
        self.wait_ready().await
    }

    async fn remove_container(&self) -> anyhow::Result<()> {
        let options = RemoveContainerOptionsBuilder::new().force(true).build();
        self.docker.remove_container(CONTAINER, Some(options)).await.context("removing traefik")
    }

    async fn create(&self, s: &ProxySettings, env: &[String], fingerprint: &str) -> anyhow::Result<()> {
        if self.docker.inspect_image(IMAGE).await.is_err() {
            let mut pull = self.docker.create_image(Some(CreateImageOptionsBuilder::new().from_image(IMAGE).build()), None, None);
            while let Some(step) = pull.next().await {
                step.context("pulling traefik")?;
            }
        }
        let [(http_ip, http_port), (https_ip, https_port)] = host_ports(s)?;
        let binding = |ip: Option<String>, port: u16| Some(vec![PortBinding { host_ip: ip, host_port: Some(port.to_string()) }]);
        let port_bindings =
            HashMap::from([("80/tcp".to_owned(), binding(http_ip, http_port)), ("443/tcp".to_owned(), binding(https_ip, https_port))]);
        let labels = HashMap::from([(LABEL_MANAGED.to_owned(), "true".to_owned()), (LABEL_CONFIG.to_owned(), fingerprint.to_owned())]);
        let body = ContainerCreateBody {
            image: Some(IMAGE.into()),
            cmd: Some(args(s)),
            env: Some(env.to_vec()),
            labels: Some(labels),
            host_config: Some(HostConfig {
                port_bindings: Some(port_bindings),
                binds: Some(BINDS.map(str::to_owned).to_vec()),
                network_mode: Some(PROXY_NETWORK.into()),
                restart_policy: Some(RestartPolicy { name: Some(RestartPolicyNameEnum::UNLESS_STOPPED), maximum_retry_count: None }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let options = CreateContainerOptionsBuilder::new().name(CONTAINER).build();
        self.docker.create_container(Some(options), body).await.context("creating traefik")?;
        self.docker.start_container(CONTAINER, None).await.context("starting traefik")?;
        Ok(())
    }

    async fn api_base(&self) -> anyhow::Result<String> {
        let inspect = self.docker.inspect_container(CONTAINER, None).await.context("traefik is not running")?;
        let ip = inspect
            .network_settings
            .and_then(|n| n.networks)
            .and_then(|n| n.get(PROXY_NETWORK).cloned())
            .and_then(|n| n.ip_address)
            .filter(|ip| !ip.is_empty())
            .context("traefik has no address on kuroe_proxy")?;
        Ok(format!("http://{ip}:{API_PORT}"))
    }

    async fn wait_ready(&self) -> anyhow::Result<()> {
        for _ in 0..50 {
            if let Ok(base) = self.api_base().await
                && let Ok(res) = self.http.get(format!("{base}/ping")).send().await
                && res.status().is_success()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("traefik did not answer /ping within 10 seconds")
    }

    pub async fn write(&self, name: &str, config: &Value) -> anyhow::Result<()> {
        let body = serde_json::to_vec_pretty(config)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(crate::db::now() as u64);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append_data(&mut header, format!("{name}.yml"), body.as_slice())?;
        let tar = builder.into_inner()?;
        let options = UploadToContainerOptionsBuilder::new().path(DYNAMIC_DIR).build();
        self.docker
            .upload_to_container(CONTAINER, Some(options), bollard::body_full(tar.into()))
            .await
            .with_context(|| format!("writing route file {name}.yml"))
    }

    /// Writes certificate and key files into Traefik's certificate volume, keys readable by Traefik only.
    pub async fn write_cert_files(&self, files: &[(String, Vec<u8>, u32)]) -> anyhow::Result<()> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, body, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(*mode);
            header.set_mtime(crate::db::now() as u64);
            header.set_cksum();
            builder.append_data(&mut header, name, body.as_slice())?;
        }
        let tar = builder.into_inner()?;
        let options = UploadToContainerOptionsBuilder::new().path(CERTS_DIR).build();
        self.docker
            .upload_to_container(CONTAINER, Some(options), bollard::body_full(tar.into()))
            .await
            .context("writing certificate files")
    }

    /// Removes certificate files that no domain uses anymore, so old keys don't linger in the volume.
    pub async fn prune_cert_files(&self, keep: &[String]) -> anyhow::Result<()> {
        let script = format!(
            "cd {CERTS_DIR} || exit 0; for f in *; do [ -f \"$f\" ] || continue; case \" $KEEP \" in *\" $f \"*) ;; *) rm -f -- \"$f\" ;; esac; done"
        );
        let exec = self
            .docker
            .create_exec(
                CONTAINER,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), script]),
                    env: Some(vec![format!("KEEP={}", keep.join(" "))]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .context("cleaning up certificate files")?;
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = self.docker.start_exec(&exec.id, None).await? {
            while output.next().await.is_some() {}
        }
        Ok(())
    }

    /// Waits until Traefik serves `service` with exactly `urls`.
    pub async fn confirm(&self, service: &str, urls: &BTreeSet<String>) -> anyhow::Result<()> {
        let base = self.api_base().await?;
        let mut seen = BTreeSet::new();
        for _ in 0..75 {
            if let Ok(res) = self.http.get(format!("{base}/api/http/services/{service}@file")).send().await
                && res.status().is_success()
                && let Ok(body) = res.json::<Value>().await
            {
                seen = body["loadBalancer"]["servers"]
                    .as_array()
                    .map(|s| s.iter().filter_map(|s| s["url"].as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                if &seen == urls {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("traefik did not pick up {service} within 15 seconds, it serves {seen:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(cert_mode: CertMode) -> ProxySettings {
        ProxySettings {
            mode: Mode::Managed,
            acme_email: Some("ops@example.com".into()),
            http_port: 80,
            https_port: 443,
            drain_seconds: 10,
            cert_mode,
            dns_identity: Some("cf1".into()),
            dns_wildcard: false,
            bind: Bind::Ports,
            behind_http: "127.0.0.1:8081".into(),
            behind_https: "127.0.0.1:8443".into(),
            web_server: None,
            access_log: false,
        }
    }

    #[test]
    fn uploads_have_no_total_time_limit_and_the_access_log_is_optional() {
        let plain = args(&settings(CertMode::Http));
        assert!(plain.contains(&"--entrypoints.web.transport.respondingTimeouts.readTimeout=0".to_owned()));
        assert!(plain.contains(&"--entrypoints.websecure.transport.respondingTimeouts.readTimeout=0".to_owned()));
        assert!(!plain.iter().any(|a| a.starts_with("--accesslog")));
        let logged = ProxySettings { access_log: true, ..settings(CertMode::Http) };
        assert!(args(&logged).contains(&"--accesslog=true".to_owned()));
        assert_ne!(fingerprint(&logged, None), fingerprint(&settings(CertMode::Http), None), "turning it on recreates Traefik");
    }

    fn acme(args: &[String]) -> Vec<&str> {
        args.iter().filter(|a| a.contains("certificatesresolvers")).map(String::as_str).collect()
    }

    #[test]
    fn args_per_certificate_mode() {
        assert!(acme(&args(&settings(CertMode::Http))).contains(&"--certificatesresolvers.le.acme.httpchallenge.entrypoint=web"));
        let dns = args(&settings(CertMode::Dns));
        assert!(acme(&dns).contains(&"--certificatesresolvers.le.acme.dnschallenge.provider=cloudflare"));
        assert!(!acme(&dns).iter().any(|a| a.contains("httpchallenge")));
        assert!(acme(&args(&settings(CertMode::None))).is_empty());
        let no_email = ProxySettings { acme_email: None, ..settings(CertMode::Http) };
        assert!(acme(&args(&no_email)).is_empty());
        assert_eq!(env(&settings(CertMode::Dns), Some("cf-token")), ["CF_DNS_API_TOKEN=cf-token"]);
        assert!(env(&settings(CertMode::Http), Some("cf-token")).is_empty());
    }

    #[test]
    fn fingerprint_follows_mode_identity_and_token_without_showing_it() {
        let http = fingerprint(&settings(CertMode::Http), None);
        let dns = fingerprint(&settings(CertMode::Dns), Some("one"));
        assert_ne!(http, dns);
        assert_ne!(dns, fingerprint(&settings(CertMode::Dns), Some("two")), "a new token recreates Traefik");
        let other_identity = ProxySettings { dns_identity: Some("cf2".into()), ..settings(CertMode::Dns) };
        assert_ne!(dns, fingerprint(&other_identity, Some("one")));
        assert!(!dns.contains("one"));
        let behind = ProxySettings { bind: Bind::Behind, ..settings(CertMode::Http) };
        assert_ne!(http, fingerprint(&behind, None), "moving behind a web server recreates Traefik");
    }

    #[test]
    fn behind_binds_loopback() {
        let behind = ProxySettings { bind: Bind::Behind, behind_http: "127.0.0.1:9080".into(), ..settings(CertMode::Http) };
        assert_eq!(host_ports(&behind).unwrap(), [(Some("127.0.0.1".into()), 9080), (Some("127.0.0.1".into()), 8443)]);
        assert_eq!(host_ports(&settings(CertMode::Http)).unwrap(), [(None, 80), (None, 443)]);
        let bad = ProxySettings { bind: Bind::Behind, behind_http: "localhost".into(), ..settings(CertMode::Http) };
        assert!(host_ports(&bad).is_err());
        assert!(args(&behind).iter().any(|a| a.starts_with("--entrypoints.web.forwardedHeaders.trustedIPs=")));
        assert!(!args(&settings(CertMode::Http)).iter().any(|a| a.contains("forwardedHeaders")));
    }
}
