use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use bollard::Docker;
use bollard::models::{NetworkConnectRequest, NetworkCreateRequest, VolumeCreateRequest};
use bollard::query_parameters::{BuildImageOptionsBuilder, DataUsageOptions, ListContainersOptionsBuilder};
use bytes::Bytes;
use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::process::Command;

use super::spec::{RunMode, SiteBuild, Source, Spec, project_name};
use super::{Deployment, finish, set_active_project};
use crate::docker::admission::{self, Pool, Running};
use crate::docker::netlimit;
use crate::identity;
use crate::docker::rewrite::{
    self, Build, Defaults, LABEL_DEPLOYMENT, LABEL_MANAGED, LABEL_SERVICE, PROXY_NETWORK, Rewritten, SWARM_NETWORK,
};
use crate::proxy::bluegreen::{self, Event, Phase, Step};
use crate::proxy::certs::{self, CertMode};
use crate::proxy::routes::{self, Route};
use crate::proxy::traefik::Traefik;
use crate::proxy::{self, Mode};
use crate::sites::{self, Sites};

enum SiteSource {
    Archive(Vec<u8>),
    Dir(PathBuf),
    Image(String),
}

const LABEL_PROJECT: &str = "com.docker.compose.project";
const LABEL_STACK: &str = "com.docker.stack.namespace";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(60);
pub const ADMISSION_WINDOW_SECS: i64 = 300;

#[derive(Clone)]
pub struct Runner {
    pub db: SqlitePool,
    pub docker: Docker,
    pub data_dir: PathBuf,
    pub traefik: Traefik,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    BlueGreen,
    Recreate,
    Stack,
    Static,
}

pub fn strategy(spec: &Spec) -> (Strategy, Option<&'static str>) {
    if spec.is_static() {
        return (Strategy::Static, Some("The shared file server serves these files. A deploy switches to the new files at once."));
    }
    if spec.mode == RunMode::Stack {
        return (Strategy::Stack, Some("Runs as a Swarm stack. Updates start each new task before stopping the old one."));
    }
    if spec.is_compose() {
        return (Strategy::Recreate, Some("Compose services restart in place. The proxy retries requests during the gap."));
    }
    if spec.container.ports.iter().any(|p| p.host.is_some()) {
        return (Strategy::Recreate, Some("This deployment publishes host ports, so two copies can't run side by side. It restarts in place."));
    }
    (Strategy::BlueGreen, None)
}

pub fn next_project(name: &str, strategy: Strategy, active: Option<&str>) -> String {
    let base = project_name(name);
    match strategy {
        Strategy::Recreate | Strategy::Stack | Strategy::Static => base,
        Strategy::BlueGreen if active == Some(format!("{base}-blue").as_str()) => format!("{base}-green"),
        Strategy::BlueGreen => format!("{base}-blue"),
    }
}

struct ProjectContainer {
    name: String,
    ip: Option<String>,
}

/// Turns Docker's build stream, which splits wherever it likes, into whole lines.
/// Color codes alone on a line move to the next one, and a line redrawn with `\r` keeps its last frame.
#[derive(Default)]
struct Lines {
    partial: String,
    carried: String,
}

impl Lines {
    fn push(&mut self, chunk: &str) -> Vec<String> {
        self.partial.push_str(chunk);
        let mut out = Vec::new();
        while let Some(end) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=end).collect();
            self.take(&line, &mut out);
        }
        out
    }

    fn finish(&mut self) -> Vec<String> {
        let rest = std::mem::take(&mut self.partial);
        let mut out = Vec::new();
        self.take(&rest, &mut out);
        out
    }

    fn take(&mut self, line: &str, out: &mut Vec<String>) {
        let line = line.trim_end_matches(['\n', '\r']);
        let line = line.rsplit('\r').find(|frame| !strip_ansi(frame).trim().is_empty()).unwrap_or(line);
        if strip_ansi(line).trim().is_empty() {
            if line.contains('\u{1b}') {
                self.carried.push_str(line.trim());
            }
            return;
        }
        out.push(format!("{}{}", std::mem::take(&mut self.carried), line.trim_end()));
    }
}

/// Drops escape sequences: CSI like `ESC[31m`, OSC like window titles, and two-character escapes.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub struct Log(Vec<String>);

impl Log {
    fn line(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::debug!("{text}");
        self.0.push(text);
    }

    fn text(&self) -> String {
        self.0.join("\n")
    }
}

impl Runner {
    pub fn deployment_dir(&self, id: &str) -> PathBuf {
        self.data_dir.join("deployments").join(id)
    }

    pub fn context_upload_path(&self, id: &str) -> PathBuf {
        self.deployment_dir(id).join("context.tar")
    }

    pub fn site_upload_path(&self, id: &str) -> PathBuf {
        self.deployment_dir(id).join("site.archive")
    }

    fn sites(&self) -> Sites {
        Sites { docker: self.docker.clone() }
    }

    pub async fn deploy(&self, deployment: Deployment, revision: i64, host_mounts: bool) {
        let mut log = Log(Vec::new());
        let status = match self.try_deploy(&deployment, revision, host_mounts, &mut log).await {
            Ok(()) => "running",
            Err(err) => {
                log.line(format!("deploy failed: {err:#}"));
                "failed"
            }
        };
        if let Err(err) = finish(&self.db, &deployment.id, status, &log.text()).await {
            tracing::error!("saving deploy result for {}: {err}", deployment.name);
        }
    }

    async fn try_deploy(&self, d: &Deployment, revision: i64, host_mounts: bool, log: &mut Log) -> anyhow::Result<()> {
        let resolved = Deployment { spec: crate::saved_secrets::resolve(&self.db, &d.spec).await?, ..d.clone() };
        let d = &resolved;
        let dir = self.deployment_dir(&d.id);
        tokio::fs::create_dir_all(&dir).await?;
        if d.spec.is_static() {
            return self.deploy_site(d, revision, log).await;
        }
        let (strategy, _) = strategy(&d.spec);
        let project = next_project(&d.name, strategy, d.active_project.as_deref());
        let project_dir = dir.join(&project);
        tokio::fs::create_dir_all(&project_dir).await?;

        let config = match &d.spec.source {
            Source::Compose { file } => {
                tokio::fs::write(dir.join("compose.yaml"), file).await?;
                self.compose_config(&dir, &project, "compose.yaml", &d.spec.container.env, log).await?
            }
            Source::Image { .. } => d.spec.single_service_config(&project, None),
            Source::Upload { .. } => {
                let tar = self.context_upload_path(&d.id);
                if !tar.exists() {
                    bail!("upload a build context first");
                }
                d.spec.single_service_config(&project, Some(&tar.to_string_lossy()))
            }
            Source::Dockerfile { file } => {
                let context = project_dir.join("context");
                tokio::fs::create_dir_all(&context).await?;
                tokio::fs::write(context.join("Dockerfile"), file).await?;
                d.spec.single_service_config(&project, Some(&context.to_string_lossy()))
            }
            Source::Git { compose_file: Some(file), reference, .. } => {
                let src = self.clone_source(d, log).await?;
                let mut file = file.trim().trim_start_matches("./");
                if file.is_empty() {
                    file = crate::deploy::spec::find_compose_file(&src).with_context(|| {
                        format!(
                            "the repository at {} has none of {}",
                            reference.as_deref().unwrap_or("its default branch"),
                            crate::deploy::spec::COMPOSE_FILE_NAMES.join(", ")
                        )
                    })?;
                    log.line(format!("using {file}"));
                }
                let path = src.join(file);
                if !path.is_file() {
                    bail!("{file} is not in the repository at {}", reference.as_deref().unwrap_or("its default branch"));
                }
                self.compose_config(&src, &project, &path.to_string_lossy(), &d.spec.container.env, log).await?
            }
            Source::Git { context, .. } => {
                let src = self.clone_source(d, log).await?;
                let build_dir = match context {
                    Some(context) => src.join(context.trim_start_matches('/')),
                    None => src,
                };
                d.spec.single_service_config(&project, Some(&build_dir.to_string_lossy()))
            }
            Source::Static {} => bail!("this static site has no build and output settings"),
        };

        let defaults = self.defaults().await?;
        let stack = strategy == Strategy::Stack;
        let ctx = rewrite::Context {
            deployment_id: &d.id,
            revision,
            settings: &d.spec.services,
            defaults: &defaults,
            host_mounts,
            stack,
            host_access: d.spec.host_access,
        };
        let Rewritten { config, builds, requests, warnings } = rewrite::rewrite(config, &ctx)?;
        for warning in &warnings {
            log.line(format!("warning: {warning}"));
        }

        let pool = self.pool().await?;
        let running = counted(&self.running_usage().await?);
        let storage = self.storage_used().await?;
        admission::admit(pool, &running, storage, &d.id, &requests).map_err(anyhow::Error::msg)?;

        self.ensure_network().await?;
        self.ensure_volumes(&config).await?;
        for build in &builds {
            self.build(build, log).await?;
        }
        let login = self.registry_login(d, log).await?;
        let docker_config = login.as_ref().map(|l| l.path());

        if strategy == Strategy::Stack {
            self.ensure_swarm_network().await?;
            let mut config = config;
            if let Some(top) = config.as_object_mut() {
                top.remove("name");
            }
            tokio::fs::write(project_dir.join("kuroe.stack.json"), serde_json::to_vec_pretty(&config)?).await?;
            let args = ["stack", "deploy", "-c", "kuroe.stack.json", "--prune", "--detach=false", "--with-registry-auth", &project];
            self.docker_cli_with(&project_dir, &args, docker_config, log).await?;
            self.publish_routes(d, Some(&project)).await?;
            self.down_other_projects(d, &project, log).await?;
            set_active_project(&self.db, &d.id, &project).await?;
            log.line(format!("revision {revision} is running as stack {project}"));
            return Ok(());
        }

        tokio::fs::write(project_dir.join("kuroe.compose.json"), serde_json::to_vec_pretty(&config)?).await?;
        let up = ["up", "-d", "--wait", "--wait-timeout", "120", "--remove-orphans"];
        let started = self.compose_with(&project_dir, &project, Some("kuroe.compose.json"), &up, docker_config, log).await;
        if started.is_ok() {
            self.apply_net_limits(&project, log).await;
        }

        match strategy {
            Strategy::Recreate => {
                started?;
                self.publish_routes(d, Some(&project)).await?;
                self.down_other_projects(d, &project, log).await?;
            }
            Strategy::BlueGreen => self.roll_out(d, &project, started, log).await?,
            Strategy::Stack | Strategy::Static => unreachable!("stacks and static sites return early"),
        }
        set_active_project(&self.db, &d.id, &project).await?;
        log.line(format!("revision {revision} is running as {project}"));
        Ok(())
    }

    async fn roll_out(&self, d: &Deployment, project: &str, started: anyhow::Result<String>, log: &mut Log) -> anyhow::Result<()> {
        let old = d.active_project.as_deref().filter(|p| *p != project);
        let first = match started {
            Err(err) => Event::Unhealthy(format!("{err:#}")),
            Ok(_) => self.health_check(d, project).await,
        };
        let mut phase = Phase::Starting;
        let mut event = Some(first);
        let mut failure = None;
        while let Some(current) = event.take() {
            let (next_phase, steps) = bluegreen::next(phase, current).map_err(anyhow::Error::msg)?;
            phase = next_phase;
            for step in steps {
                event = match step {
                    Step::SwitchRoutes => {
                        log.line(format!("switching traffic to {project}"));
                        Some(match self.publish_routes(d, Some(project)).await {
                            Ok(()) => Event::Switched,
                            Err(err) => Event::SwitchFailed(format!("{err:#}")),
                        })
                    }
                    Step::Drain => {
                        let drain = proxy::load_settings(&self.db).await?.drain_seconds;
                        if old.is_some() {
                            log.line(format!("draining the old version for {drain}s"));
                            tokio::time::sleep(Duration::from_secs(drain)).await;
                        }
                        Some(Event::Drained)
                    }
                    Step::StopOld => {
                        self.down_other_projects(d, project, log).await?;
                        None
                    }
                    Step::RestoreRoutes => {
                        if let Err(err) = self.publish_routes(d, old).await {
                            log.line(format!("restoring routes failed: {err:#}"));
                        }
                        None
                    }
                    Step::StopNew => {
                        self.compose_down(project, log).await?;
                        Some(Event::RolledBack)
                    }
                    Step::Report(message) => {
                        failure = Some(message);
                        event
                    }
                };
            }
        }
        match (phase, failure) {
            (Phase::Done, _) => Ok(()),
            (_, Some(message)) => bail!("{message}. The previous version keeps serving."),
            (phase, None) => bail!("rollout stopped in {phase:?}"),
        }
    }

    async fn health_check(&self, d: &Deployment, project: &str) -> Event {
        let domains = match proxy::domains_for(&self.db, &d.id).await {
            Ok(domains) => domains,
            Err(err) => return Event::Unhealthy(format!("loading domains: {err}")),
        };
        let ports: BTreeSet<(String, u16)> = domains.iter().flat_map(|dm| dm.targets()).collect();
        let deadline = Instant::now() + HEALTH_TIMEOUT;
        for (service, port) in ports {
            let containers = match self.project_containers(project, Some(&service)).await {
                Ok(c) if !c.is_empty() => c,
                Ok(_) => return Event::Unhealthy(format!("no running container for {service}")),
                Err(err) => return Event::Unhealthy(format!("{err:#}")),
            };
            for container in containers {
                let Some(ip) = container.ip else { return Event::Unhealthy(format!("{} is not on {PROXY_NETWORK}", container.name)) };
                loop {
                    let attempt = tokio::time::timeout(Duration::from_secs(1), tokio::net::TcpStream::connect((ip.as_str(), port))).await;
                    if matches!(attempt, Ok(Ok(_))) {
                        break;
                    }
                    if Instant::now() > deadline {
                        return Event::Timeout;
                    }
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
        }
        Event::Healthy
    }

    /// Starts or updates Traefik for these settings, then puts the saved certificates it needs in place.
    pub async fn apply_proxy(&self, settings: &proxy::ProxySettings) -> anyhow::Result<()> {
        let token = self.dns_token(settings).await?;
        self.traefik.apply(settings, token.as_deref()).await?;
        self.sync_certificates().await
    }

    async fn dns_identity(&self, settings: &proxy::ProxySettings) -> anyhow::Result<Option<identity::Identity>> {
        if settings.cert_mode != CertMode::Dns {
            return Ok(None);
        }
        let Some(id) = &settings.dns_identity else { return Ok(None) };
        let saved = identity::get(&self.db, id).await?.context("the Cloudflare identity for DNS challenges was removed")?;
        if saved.kind != identity::Kind::Cloudflare {
            bail!("{} is not a Cloudflare identity", saved.name);
        }
        Ok(Some(saved))
    }

    async fn dns_token(&self, settings: &proxy::ProxySettings) -> anyhow::Result<Option<String>> {
        Ok(self.dns_identity(settings).await?.map(|i| i.secret))
    }

    async fn saved_certificates(&self) -> anyhow::Result<Vec<certs::Saved>> {
        Ok(identity::list(&self.db).await?.iter().filter_map(certs::saved).collect())
    }

    async fn wildcard_zones(&self, settings: &proxy::ProxySettings) -> anyhow::Result<Vec<String>> {
        if !settings.dns_wildcard {
            return Ok(Vec::new());
        }
        let zones = self.dns_identity(settings).await?.map(|i| i.meta["zones"].clone()).unwrap_or_default();
        Ok(zones.as_array().map(|z| z.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default())
    }

    /// Writes the saved certificates that domains use into Traefik, with the origin pull CA when one asks for it,
    /// and removes the files of ones no domain uses.
    pub async fn sync_certificates(&self) -> anyhow::Result<()> {
        let settings = proxy::load_settings(&self.db).await?;
        if settings.mode != Mode::Managed {
            return Ok(());
        }
        let saved = self.saved_certificates().await?;
        let zones = self.wildcard_zones(&settings).await?;
        let plan = certs::Plan { mode: settings.cert_mode, wildcard_zones: &zones, saved: &saved };
        let tls_domains = if settings.tls_in_front() { Vec::new() } else { proxy::tls_domains(&self.db).await? };
        let mut routes: Vec<certs::RouteCert> =
            tls_domains.iter().map(|d| plan.route_cert(&d.hostname, d.certificate.as_deref())).collect();
        if let Some(ui) = proxy::ui::load(&self.db).await?.filter(|u| u.tls && !settings.tls_in_front()) {
            routes.push(plan.route_cert(&ui.hostname, None));
        }
        let used = certs::ids_in_use(&saved, &routes);
        let in_use: Vec<&certs::Saved> = saved.iter().filter(|c| used.contains(&c.id)).collect();
        let mut files = Vec::new();
        for cert in &in_use {
            files.push((format!("{}.crt", cert.id), cert.chain.clone().into_bytes(), 0o644));
            files.push((format!("{}.key", cert.id), cert.key.clone().into_bytes(), 0o600));
        }
        if in_use.iter().any(|c| c.aop) {
            files.push((certs::AOP_CA_FILE.to_owned(), self.origin_pull_ca().await?.into_bytes(), 0o644));
        }
        if !files.is_empty() {
            self.traefik.write_cert_files(&files).await?;
        }
        self.traefik.write("_certs", &certs::store_config(&in_use)).await?;
        let keep: Vec<String> = files.into_iter().map(|(name, _, _)| name).collect();
        self.traefik.prune_cert_files(&keep).await
    }

    /// Which certificate each of these domains gets right now.
    pub async fn served_by(&self, domains: &[proxy::Domain]) -> anyhow::Result<Vec<certs::RouteCert>> {
        let settings = proxy::load_settings(&self.db).await?;
        let saved = self.saved_certificates().await?;
        let zones = self.wildcard_zones(&settings).await?;
        let plan = certs::Plan { mode: settings.cert_mode, wildcard_zones: &zones, saved: &saved };
        Ok(domains
            .iter()
            .map(|d| if d.tls { plan.route_cert(&d.hostname, d.certificate.as_deref()) } else { certs::RouteCert::Default })
            .collect())
    }

    /// Cloudflare's CA for Authenticated Origin Pulls, fetched once and kept in the data folder.
    async fn origin_pull_ca(&self) -> anyhow::Result<String> {
        let cached = self.data_dir.join("certs").join(certs::AOP_CA_FILE);
        if let Ok(text) = tokio::fs::read_to_string(&cached).await
            && certs::is_certificate(&text)
        {
            return Ok(text);
        }
        let text = self.traefik.http.get(certs::AOP_CA_URL).send().await?.error_for_status()?.text().await?;
        if !certs::is_certificate(&text) {
            bail!("Cloudflare's origin pull CA from {} is not a certificate", certs::AOP_CA_URL);
        }
        tokio::fs::create_dir_all(cached.parent().unwrap()).await?;
        tokio::fs::write(&cached, &text).await?;
        Ok(text)
    }

    /// Writes the route to kuroe's own UI, or an empty file when it has no domain.
    pub async fn publish_ui_route(&self, listen: std::net::SocketAddr) -> anyhow::Result<()> {
        let settings = proxy::load_settings(&self.db).await?;
        if settings.mode != Mode::Managed {
            return Ok(());
        }
        let Some(ui) = proxy::ui::load(&self.db).await? else {
            return self.traefik.write(proxy::ui::ROUTE_FILE, &serde_json::json!({})).await;
        };
        let upstream = proxy::ui::upstream(&self.docker, listen).await?;
        let saved = self.saved_certificates().await?;
        let zones = self.wildcard_zones(&settings).await?;
        let plan = certs::Plan { mode: settings.cert_mode, wildcard_zones: &zones, saved: &saved };
        let cert = plan.route_cert(&ui.hostname, None);
        let tls = ui.tls && !settings.tls_in_front();
        self.traefik.write(proxy::ui::ROUTE_FILE, &proxy::ui::render(&ui, tls, &upstream, listen.port(), cert)).await
    }

    pub async fn publish_routes(&self, d: &Deployment, project: Option<&str>) -> anyhow::Result<()> {
        let settings = proxy::load_settings(&self.db).await?;
        if settings.mode != Mode::Managed {
            return Ok(());
        }
        self.sync_certificates().await?;
        let saved = self.saved_certificates().await?;
        let zones = self.wildcard_zones(&settings).await?;
        let plan = certs::Plan { mode: settings.cert_mode, wildcard_zones: &zones, saved: &saved };
        let domains = proxy::domains_for(&self.db, &d.id).await?;
        if d.spec.is_static() {
            let hosts: Vec<sites::SiteRoute> = domains
                .iter()
                .map(|domain| sites::SiteRoute {
                    hostname: domain.hostname.clone(),
                    tls: domain.tls && !settings.tls_in_front(),
                    cert: plan.route_cert(&domain.hostname, domain.certificate.as_deref()),
                    redirect: domain.options.redirect.clone(),
                })
                .collect();
            let revision = project.and_then(sites::revision_of);
            let spa = d.spec.site.as_ref().is_some_and(|s| s.spa);
            return self.traefik.write(&d.name, &sites::routes(&d.name, &d.id, revision, spa, &hosts)).await;
        }
        let page_url = |domain: &proxy::Domain, rule: Option<usize>, page: &proxy::options::ErrorPage| {
            proxy::options::page_query(&d.id, &proxy::options::page_file(&domain.id, rule, page))
        };
        let mut rendered = Vec::new();
        let mut pages = Vec::new();
        for domain in &domains {
            let options = &domain.options;
            pages.extend(options.error_files(&domain.id));
            let mut paths = Vec::new();
            for (i, rule) in options.paths.iter().enumerate() {
                paths.push(routes::PathRoute {
                    path: rule.path.clone(),
                    exact: rule.exact,
                    service: rule.service.clone(),
                    port: rule.port,
                    upstreams: self.service_upstreams(d, project, &rule.service, false).await?,
                    error_page: rule.error_page.as_ref().map(|p| page_url(domain, Some(i), p)),
                    retry: options.retries(rule.error_page.is_some()),
                });
            }
            let upstreams = if domain.service.is_empty() {
                Vec::new()
            } else {
                self.service_upstreams(d, project, &domain.service, options.rewrite_host).await?
            };
            let compress = options.compress.then(|| routes::Compression {
                types: options.compress_types.clone(),
                min_bytes: options.compress_min_bytes,
                gzip_only: options.compress_gzip_only,
            });
            rendered.push(Route {
                hostname: domain.hostname.clone(),
                tls: domain.tls && !settings.tls_in_front(),
                service: domain.service.clone(),
                port: domain.port as u16,
                upstreams,
                cert: plan.route_cert(&domain.hostname, domain.certificate.as_deref()),
                extra: routes::Extra {
                    redirect: options.redirect.clone(),
                    compress,
                    error_page: options.error_page.as_ref().map(|p| page_url(domain, None, p)),
                    retry: options.retries(options.error_page.is_some()),
                    max_body_bytes: options.max_body_mb.map(|mb| u64::from(mb) * 1024 * 1024),
                    timeout: options.timeout,
                    rewrite_host: options.rewrite_host,
                    paths,
                },
            });
        }
        self.sites().write_error_pages(&d.id, &pages).await?;
        self.traefik.write(&d.name, &routes::render(&d.name, &rendered)).await?;
        let mut expected: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for route in rendered.iter().filter(|r| r.extra.redirect.is_none()) {
            let timeout = route.extra.timeout;
            let targets = std::iter::once((&route.service, route.port, &route.upstreams, route.extra.rewrite_host))
                .chain(route.extra.paths.iter().map(|p| (&p.service, p.port, &p.upstreams, false)));
            for (service, port, upstreams, rewrite_host) in targets {
                let urls = upstreams.iter().map(|u| format!("http://{u}:{port}")).collect();
                expected.insert(routes::service_key(&d.name, service, port, rewrite_host, timeout), urls);
            }
        }
        for (service, urls) in &expected {
            self.traefik.confirm(service, urls).await?;
        }
        Ok(())
    }

    /// Container names, or with `by_ip` their addresses on the proxy network, so a rewritten `Host` is an IP like
    /// nginx's `127.0.0.1:<port>`. Swarm services only have a name.
    async fn service_upstreams(&self, d: &Deployment, project: Option<&str>, service: &str, by_ip: bool) -> anyhow::Result<Vec<String>> {
        Ok(match project {
            Some(project) if d.spec.mode == RunMode::Stack => vec![format!("{project}_{service}")],
            Some(project) => {
                let containers = self.project_containers(project, Some(service)).await?.into_iter();
                if by_ip { containers.filter_map(|c| c.ip).collect() } else { containers.map(|c| c.name).collect() }
            }
            None => Vec::new(),
        })
    }

    /// Builds the site when it has a build step, installs the files as a new revision, then points the route at it.
    async fn deploy_site(&self, d: &Deployment, revision: i64, log: &mut Log) -> anyhow::Result<()> {
        if proxy::load_settings(&self.db).await?.mode != Mode::Managed {
            bail!("static sites are served through managed Traefik, turn it on under Proxy");
        }
        let site = d.spec.site.clone().context("this static site has no build and output settings")?;
        let output = site.output_dir().to_owned();
        let source = match &d.spec.source {
            Source::Static {} => SiteSource::Archive(
                tokio::fs::read(self.site_upload_path(&d.id)).await.context("upload the site's files first")?,
            ),
            Source::Git { context, .. } => {
                let src = self.clone_source(d, log).await?;
                SiteSource::Dir(match context {
                    Some(context) => src.join(context.trim_start_matches('/')),
                    None => src,
                })
            }
            Source::Image { image, .. } => {
                let login = self.registry_login(d, log).await?;
                let dir = self.deployment_dir(&d.id);
                self.docker_cli_with(&dir, &["pull", image], login.as_ref().map(|l| l.path()), log).await?;
                SiteSource::Image(image.clone())
            }
            _ => bail!("only git repositories, images and uploaded archives can be static sites"),
        };
        let sites = self.sites();
        let (files, out, hoist) = match (&site.build, source) {
            (SiteBuild::None, SiteSource::Image(image)) => {
                (sites.copy_from_image(&image, &output).await?, sites::top_folder(&output), false)
            }
            (_, SiteSource::Image(_)) => bail!("an image is already built, so its files are served as they are"),
            (SiteBuild::None, SiteSource::Archive(archive)) => (archive, output.clone(), output.is_empty()),
            (SiteBuild::None, SiteSource::Dir(dir)) => {
                let dir = if output.is_empty() { dir } else { dir.join(&output) };
                (context_tar(&dir).await?.to_vec(), String::new(), false)
            }
            (SiteBuild::Command { image, command }, source) => {
                let source = match source {
                    SiteSource::Archive(archive) => archive,
                    SiteSource::Dir(dir) => context_tar(&dir).await?.to_vec(),
                    SiteSource::Image(_) => unreachable!("images only serve their files as they are"),
                };
                let env: Vec<String> = d.spec.container.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
                let job = sites::CommandBuild { deployment_id: &d.id, image, command, output: &output, env: &env, source: &source };
                let files = sites.build_with_command(&job, &mut |line| log.line(line)).await?;
                (files, sites::top_folder(if output.is_empty() { "src" } else { &output }), false)
            }
            (SiteBuild::Dockerfile { dockerfile }, source) => {
                let tag = format!("kuroe/{}-site:{revision}", project_name(&d.name));
                let dockerfile = dockerfile.as_deref().unwrap_or("Dockerfile");
                match source {
                    SiteSource::Archive(archive) => self.build_archive(&tag, dockerfile, archive, log).await?,
                    SiteSource::Dir(dir) => {
                        let build = Build {
                            service: "site".into(),
                            context: dir.to_string_lossy().into_owned(),
                            dockerfile: dockerfile.to_owned(),
                            args: Default::default(),
                            tag: tag.clone(),
                        };
                        self.build(&build, log).await?;
                    }
                    SiteSource::Image(_) => unreachable!("images only serve their files as they are"),
                }
                let files = sites.copy_from_image(&tag, &output).await;
                let _ = self.docker.remove_image(&tag, None::<bollard::query_parameters::RemoveImageOptions>, None).await;
                (files?, sites::top_folder(&output), false)
            }
        };
        sites.ensure().await?;
        let count = sites.install(&d.id, revision, files, &out, hoist).await?;
        log.line(format!("installed {count} files as revision {revision}"));
        let previous = d.active_project.as_deref().and_then(sites::revision_of);
        let project = sites::project(revision);
        set_active_project(&self.db, &d.id, &project).await?;
        self.publish_routes(d, Some(&project)).await?;
        let keep: Vec<i64> = std::iter::once(revision).chain(previous).collect();
        if let Err(err) = sites.prune(&d.id, &keep).await {
            log.line(format!("warning: older revisions were not removed: {err:#}"));
        }
        log.line(format!("revision {revision} is live"));
        Ok(())
    }

    pub async fn stop(&self, d: &Deployment) -> anyhow::Result<String> {
        let mut log = Log(Vec::new());
        self.publish_routes(d, None).await?;
        self.down_other_projects(d, "", &mut log).await?;
        Ok(log.text())
    }

    pub async fn restart(&self, d: &Deployment) -> anyhow::Result<String> {
        let mut log = Log(Vec::new());
        if d.spec.is_static() {
            self.sites().ensure().await?;
            self.publish_routes(d, d.active_project.as_deref()).await?;
            return Ok("the static file server is up and the routes are written again".into());
        }
        let project = d.active_project.clone().unwrap_or_else(|| project_name(&d.name));
        let project_dir = self.deployment_dir(&d.id).join(&project);
        if d.spec.mode == RunMode::Stack {
            let services = self.docker_cli(&project_dir, &["stack", "services", &project, "--format", "{{.Name}}"], &mut log).await?;
            for service in services.lines().filter(|s| !s.is_empty()) {
                self.docker_cli(&project_dir, &["service", "update", "--force", "--detach=false", service], &mut log).await?;
            }
        } else {
            self.compose(&project_dir, &project, Some("kuroe.compose.json"), &["restart"], &mut log).await?;
        }
        self.publish_routes(d, Some(&project)).await?;
        Ok(log.text())
    }

    pub async fn remove(&self, d: &Deployment) -> anyhow::Result<()> {
        let mut log = Log(Vec::new());
        self.down_other_projects(d, "", &mut log).await?;
        if proxy::load_settings(&self.db).await?.mode == Mode::Managed {
            self.traefik.write(&d.name, &serde_json::json!({})).await?;
        }
        if d.spec.is_static() {
            self.sites().remove(&d.id).await?;
        } else {
            self.sites().write_error_pages(&d.id, &[]).await?;
        }
        let dir = self.deployment_dir(&d.id);
        if dir.exists() {
            tokio::fs::remove_dir_all(&dir).await?;
        }
        Ok(())
    }

    async fn down_other_projects(&self, d: &Deployment, keep: &str, log: &mut Log) -> anyhow::Result<()> {
        for project in self.labelled(&d.id, LABEL_PROJECT).await? {
            if project != keep {
                log.line(format!("stopping {project}"));
                self.compose_down(&project, log).await?;
            }
        }
        let root = self.data_dir.join("deployments");
        for stack in self.labelled(&d.id, LABEL_STACK).await? {
            if stack != keep {
                log.line(format!("removing stack {stack}"));
                self.docker_cli(&root, &["stack", "rm", "--detach=false", &stack], log).await?;
            }
        }
        Ok(())
    }

    async fn ensure_swarm_network(&self) -> anyhow::Result<()> {
        if self.docker.inspect_network(SWARM_NETWORK, None).await.is_err() {
            let labels = HashMap::from([(LABEL_MANAGED.to_owned(), "true".to_owned())]);
            self.docker
                .create_network(NetworkCreateRequest {
                    name: SWARM_NETWORK.into(),
                    driver: Some("overlay".into()),
                    attachable: Some(true),
                    labels: Some(labels),
                    ..Default::default()
                })
                .await
                .context("creating the kuroe_swarm overlay network, is this daemon a swarm manager?")?;
        }
        let traefik = self.docker.inspect_container(crate::proxy::traefik::CONTAINER, None).await.ok();
        let joined = traefik
            .as_ref()
            .and_then(|t| t.network_settings.as_ref())
            .and_then(|n| n.networks.as_ref())
            .is_some_and(|n| n.contains_key(SWARM_NETWORK));
        if traefik.is_some() && !joined {
            self.docker
                .connect_network(
                    SWARM_NETWORK,
                    NetworkConnectRequest { container: crate::proxy::traefik::CONTAINER.into(), ..Default::default() },
                )
                .await
                .context("attaching traefik to kuroe_swarm")?;
        }
        Ok(())
    }

    async fn docker_cli(&self, dir: &Path, args: &[&str], log: &mut Log) -> anyhow::Result<String> {
        self.docker_cli_with(dir, args, None, log).await
    }

    async fn docker_cli_with(&self, dir: &Path, args: &[&str], docker_config: Option<&Path>, log: &mut Log) -> anyhow::Result<String> {
        tokio::fs::create_dir_all(dir).await?;
        let mut command = Command::new("docker");
        if let Some(config) = docker_config {
            command.env("DOCKER_CONFIG", config);
        }
        let output = command
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .await
            .context("running the docker CLI, is it installed?")?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        for text in [stdout.trim_end(), stderr.trim_end()] {
            if !text.is_empty() {
                log.line(text);
            }
        }
        if !output.status.success() {
            bail!("docker {} exited with {}", args.join(" "), output.status);
        }
        Ok(stdout)
    }

    /// A limit that can't be set is reported and the deploy goes on, the same way the watcher handles it.
    async fn apply_net_limits(&self, project: &str, log: &mut Log) {
        match netlimit::apply_running(&self.docker, &format!("{LABEL_PROJECT}={project}")).await {
            Ok(results) => {
                for (name, result) in results {
                    match result {
                        Ok(()) => log.line(format!("network limit set on {name}")),
                        Err(err) => log.line(format!("warning: network limit for {name}: {err:#}")),
                    }
                }
            }
            Err(err) => log.line(format!("warning: listing containers for network limits: {err:#}")),
        }
    }

    async fn compose_down(&self, project: &str, log: &mut Log) -> anyhow::Result<()> {
        let root = self.data_dir.join("deployments");
        tokio::fs::create_dir_all(&root).await?;
        self.compose(&root, project, None, &["down", "--remove-orphans"], log).await?;
        Ok(())
    }

    async fn labelled(&self, deployment_id: &str, label: &str) -> anyhow::Result<BTreeSet<String>> {
        let filters = HashMap::from([("label", vec![format!("{LABEL_DEPLOYMENT}={deployment_id}")])]);
        let options = ListContainersOptionsBuilder::new().all(true).filters(&filters).build();
        Ok(self
            .docker
            .list_containers(Some(options))
            .await?
            .into_iter()
            .filter_map(|c| c.labels.and_then(|l| l.get(label).cloned()))
            .collect())
    }

    async fn project_containers(&self, project: &str, service: Option<&str>) -> anyhow::Result<Vec<ProjectContainer>> {
        let mut labels = vec![format!("{LABEL_PROJECT}={project}")];
        if let Some(service) = service {
            labels.push(format!("{LABEL_SERVICE}={service}"));
        }
        let filters = HashMap::from([("label", labels), ("status", vec!["running".into()])]);
        let options = ListContainersOptionsBuilder::new().filters(&filters).build();
        let mut containers: Vec<ProjectContainer> = self
            .docker
            .list_containers(Some(options))
            .await?
            .into_iter()
            .map(|c| ProjectContainer {
                name: c.names.and_then(|n| n.first().cloned()).unwrap_or_default().trim_start_matches('/').to_owned(),
                ip: c
                    .network_settings
                    .and_then(|n| n.networks)
                    .and_then(|n| n.get(PROXY_NETWORK).cloned())
                    .and_then(|n| n.ip_address)
                    .filter(|ip| !ip.is_empty()),
            })
            .collect();
        containers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(containers)
    }

    async fn compose(&self, dir: &Path, project: &str, file: Option<&str>, args: &[&str], log: &mut Log) -> anyhow::Result<String> {
        self.compose_with(dir, project, file, args, None, log).await
    }

    /// The resolved Compose model. `env` holds the deployment's variables and saved secrets, which fill `${VAR}` in the file.
    async fn compose_config(&self, dir: &Path, project: &str, file: &str, env: &BTreeMap<String, String>, log: &mut Log) -> anyhow::Result<Value> {
        let output = Command::new("docker")
            .arg("compose")
            .args(["-p", project, "-f", file, "config", "--format", "json"])
            .envs(env)
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .await
            .context("running docker compose, is the docker CLI installed?")?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            log.line(stderr.trim_end());
        }
        if !output.status.success() {
            bail!("docker compose config exited with {}", output.status);
        }
        serde_json::from_slice(&output.stdout).context("parsing docker compose config output")
    }

    async fn compose_with(
        &self,
        dir: &Path,
        project: &str,
        file: Option<&str>,
        args: &[&str],
        docker_config: Option<&Path>,
        log: &mut Log,
    ) -> anyhow::Result<String> {
        let mut command = Command::new("docker");
        if let Some(config) = docker_config {
            command.env("DOCKER_CONFIG", config);
        }
        command.arg("compose").args(["-p", project]);
        if let Some(file) = file {
            command.args(["-f", file]);
        }
        let output = command
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .await
            .context("running docker compose, is the docker CLI installed?")?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if args.first() != Some(&"config") && !stdout.trim().is_empty() {
            log.line(stdout.trim_end());
        }
        if !stderr.trim().is_empty() {
            log.line(stderr.trim_end());
        }
        if !output.status.success() {
            bail!("docker compose {} exited with {}", args.join(" "), output.status);
        }
        Ok(stdout)
    }

    async fn registry_login(&self, d: &Deployment, log: &mut Log) -> anyhow::Result<Option<identity::registry::TempDockerConfig>> {
        let Source::Image { registry_identity: Some(id), .. } = &d.spec.source else { return Ok(None) };
        let saved = identity::get(&self.db, id).await?.context("the registry login for this deployment was removed")?;
        log.line(format!("pulling with the registry login {}", saved.name));
        identity::touch(&self.db, id).await?;
        Ok(Some(identity::registry::write_docker_config(&self.data_dir, &saved)?))
    }

    async fn clone_source(&self, d: &Deployment, log: &mut Log) -> anyhow::Result<PathBuf> {
        let Source::Git { url, reference, token, identity: login, .. } = &d.spec.source else { unreachable!() };
        let src = self.deployment_dir(&d.id).join("src");
        log.line(format!("cloning {url}"));
        let creds = match login {
            Some(id) => {
                let saved = identity::get(&self.db, id).await?.context("the Git login for this deployment was removed")?;
                identity::check_git_url(&saved, url)?;
                log.line(format!("using the Git login {}", saved.name));
                Some(identity::git_credentials(&self.db, &self.traefik.http, &saved).await?)
            }
            None => token.as_deref().map(|t| (crate::git::token_login(t).0.to_owned(), t.to_owned())),
        };
        let login = creds.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
        crate::git::shallow_clone(url, reference.as_deref(), login, &src).await?;
        Ok(src)
    }

    async fn build(&self, build: &Build, log: &mut Log) -> anyhow::Result<()> {
        log.line(format!("building {} as {}", build.service, build.tag));
        let labels = HashMap::from([(LABEL_MANAGED, "true")]);
        let mut options = BuildImageOptionsBuilder::new()
            .dockerfile(&build.dockerfile)
            .t(&build.tag)
            .rm(true)
            .buildargs(&build.args.iter().collect::<HashMap<_, _>>())
            .labels(&labels);
        let remote = build.context.contains("://") || build.context.starts_with("git@");
        let body = if remote {
            options = options.remote(&build.context);
            None
        } else {
            Some(context_tar(Path::new(&build.context)).await?)
        };
        self.stream_build(options.build(), body, &build.service, log).await
    }

    /// Builds an image from an uploaded archive, which Docker takes plain or compressed.
    async fn build_archive(&self, tag: &str, dockerfile: &str, archive: Vec<u8>, log: &mut Log) -> anyhow::Result<()> {
        log.line(format!("building the site as {tag}"));
        let labels = HashMap::from([(LABEL_MANAGED, "true")]);
        let options = BuildImageOptionsBuilder::new().dockerfile(dockerfile).t(tag).rm(true).labels(&labels).build();
        self.stream_build(options, Some(Bytes::from(archive)), "site", log).await
    }

    async fn stream_build(
        &self,
        options: bollard::query_parameters::BuildImageOptions,
        body: Option<Bytes>,
        service: &str,
        log: &mut Log,
    ) -> anyhow::Result<()> {
        let mut stream = self.docker.build_image(options, None, body.map(bollard::body_full));
        let mut lines = Lines::default();
        while let Some(item) = stream.next().await {
            let info = item?;
            if let Some(text) = info.stream {
                for line in lines.push(&text) {
                    log.line(line);
                }
            }
            if let Some(err) = info.error_detail {
                lines.finish().into_iter().for_each(|line| log.line(line));
                bail!("build of {service} failed: {}", err.message.unwrap_or_default());
            }
        }
        lines.finish().into_iter().for_each(|line| log.line(line));
        Ok(())
    }

    async fn ensure_network(&self) -> anyhow::Result<()> {
        if self.docker.inspect_network(PROXY_NETWORK, None).await.is_ok() {
            return Ok(());
        }
        let labels = HashMap::from([(LABEL_MANAGED.to_owned(), "true".to_owned())]);
        self.docker
            .create_network(NetworkCreateRequest { name: PROXY_NETWORK.into(), labels: Some(labels), ..Default::default() })
            .await?;
        Ok(())
    }

    async fn ensure_volumes(&self, config: &Value) -> anyhow::Result<()> {
        let Some(volumes) = config["volumes"].as_object() else { return Ok(()) };
        for (name, volume) in volumes {
            if volume["external"] == Value::Bool(true)
                && name.starts_with("kuroe_")
                && self.docker.inspect_volume(name).await.is_err()
            {
                bail!("shared volume {} does not exist, create it on the volumes page", name.trim_start_matches("kuroe_"));
            }
        }
        Ok(())
    }

    async fn setting_f64(&self, key: &str) -> anyhow::Result<Option<f64>> {
        let value: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(&self.db).await?;
        value
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().parse::<f64>().with_context(|| format!("setting {key} is not a number: {v}")))
            .transpose()
    }

    async fn defaults(&self) -> anyhow::Result<Defaults> {
        Ok(Defaults {
            cpus: self.setting_f64("default_cpus").await?,
            memory_mb: self.setting_f64("default_memory_mb").await?.map(|mb| mb as u64),
            pids: self.setting_f64("default_pids").await?.map(|pids| pids as i64),
            net_in_mbit: self.setting_f64("default_net_in_mbit").await?,
            net_out_mbit: self.setting_f64("default_net_out_mbit").await?,
            net_conn_out_mbit: self.setting_f64("default_net_conn_out_mbit").await?,
        })
    }

    pub async fn pool(&self) -> anyhow::Result<Pool> {
        let info = self.docker.info().await?;
        let cpus = match self.setting_f64("pool_cpus").await? {
            Some(v) => v,
            None => info.ncpu.context("docker info has no CPU count")? as f64,
        };
        let memory_bytes = match self.setting_f64("pool_memory_mb").await? {
            Some(mb) => (mb * 1024.0 * 1024.0) as u64,
            None => info.mem_total.context("docker info has no memory total")? as u64,
        };
        let storage_bytes = self.setting_f64("pool_storage_gb").await?.map(|gb| (gb * (1u64 << 30) as f64) as u64);
        Ok(Pool { cpus, memory_bytes, storage_bytes })
    }

    pub async fn running_usage(&self) -> anyhow::Result<Vec<PoolMember>> {
        let filters = HashMap::from([("label", vec![format!("{LABEL_MANAGED}=true")]), ("status", vec!["running".into()])]);
        let containers = self.docker.list_containers(Some(ListContainersOptionsBuilder::new().filters(&filters).build())).await?;
        let peaks = peaks(&self.db, crate::db::now() - ADMISSION_WINDOW_SECS).await?;
        let mut members = Vec::new();
        for summary in containers {
            let (Some(id), Some(labels)) = (summary.id, summary.labels) else { continue };
            if !counts_toward_pool(&labels) {
                continue;
            }
            let host = self.docker.inspect_container(&id, None).await?.host_config.unwrap_or_default();
            let deployment = labels.get(LABEL_DEPLOYMENT).cloned().unwrap_or_default();
            let cpus_limit = host.nano_cpus.unwrap_or(0) as f64 / 1e9;
            let memory_limit = host.memory.unwrap_or(0).max(0) as u64;
            members.push(PoolMember::new(deployment, cpus_limit, memory_limit, peaks.get(&id).copied()));
        }
        Ok(members)
    }

    pub async fn storage_used(&self) -> anyhow::Result<u64> {
        let df = self.docker.df(None::<DataUsageOptions>).await?;
        let images = df.image_usage.and_then(|u| u.total_size).unwrap_or(0);
        let cache = df.build_cache_usage.and_then(|u| u.total_size).unwrap_or(0);
        let volumes: i64 = df
            .volume_usage
            .and_then(|u| u.items)
            .unwrap_or_default()
            .iter()
            .filter(|v| v["Name"].as_str().is_some_and(|n| n.starts_with("kuroe_")))
            .filter_map(|v| v["UsageData"]["Size"].as_i64())
            .filter(|size| *size > 0)
            .sum();
        Ok((images + cache + volumes).max(0) as u64)
    }

    pub async fn create_volume(&self, name: &str) -> anyhow::Result<()> {
        let labels = HashMap::from([(LABEL_MANAGED.to_owned(), "true".to_owned())]);
        self.docker
            .create_volume(VolumeCreateRequest { name: Some(format!("kuroe_{name}")), labels: Some(labels), ..Default::default() })
            .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PoolMember {
    pub counted: Running,
    pub cpus_limit: f64,
    pub memory_limit: u64,
}

impl PoolMember {
    /// Counts the peak use when there is one, otherwise the limits, so a container that just started can't slip through.
    fn new(deployment: String, cpus_limit: f64, memory_limit: u64, peak: Option<Peak>) -> Self {
        let (cpus, memory_bytes) = match peak {
            Some(p) => (p.cpu_percent.max(0.0) / 100.0, p.mem_bytes.max(0) as u64),
            None => (cpus_limit, memory_limit),
        };
        PoolMember { counted: Running { deployment, cpus, memory_bytes }, cpus_limit, memory_limit }
    }
}

pub fn counted(members: &[PoolMember]) -> Vec<Running> {
    members.iter().map(|m| m.counted.clone()).collect()
}

/// Deployments and CI runners share the pool. kuroe's own helpers (Traefik, the static server, builds) don't.
fn counts_toward_pool(labels: &HashMap<String, String>) -> bool {
    labels.contains_key(LABEL_DEPLOYMENT) || labels.contains_key(crate::runners::LABEL_POOL)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Peak {
    cpu_percent: f64,
    mem_bytes: i64,
}

async fn peaks(db: &SqlitePool, since: i64) -> sqlx::Result<HashMap<String, Peak>> {
    let rows: Vec<(String, f64, i64)> = sqlx::query_as(
        "SELECT s.container_id, MAX(m.cpu_percent), MAX(m.mem_bytes)
         FROM container_metrics m JOIN metric_series s ON s.id = m.series_id
         WHERE m.ts >= ?
         GROUP BY s.container_id",
    )
    .bind(since)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(id, cpu_percent, mem_bytes)| (id, Peak { cpu_percent, mem_bytes })).collect())
}

async fn context_tar(path: &Path) -> anyhow::Result<Bytes> {
    if !path.exists() {
        bail!("build context {} does not exist", path.display());
    }
    if path.is_file() {
        return Ok(tokio::fs::read(path).await?.into());
    }
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> anyhow::Result<Bytes> {
        let mut builder = tar::Builder::new(Vec::new());
        builder.follow_symlinks(false);
        append_dir(&mut builder, &path, Path::new(""))?;
        Ok(builder.into_inner()?.into())
    })
    .await?
}

fn append_dir(builder: &mut tar::Builder<Vec<u8>>, root: &Path, rel: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(root.join(rel))? {
        let entry = entry?;
        let name = entry.file_name();
        if rel.as_os_str().is_empty() && name == ".git" {
            continue;
        }
        let rel = rel.join(&name);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            builder.append_dir(&rel, entry.path())?;
            append_dir(builder, root, &rel)?;
        } else {
            builder.append_path_with_name(entry.path(), &rel)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn build_stream_becomes_whole_lines_with_their_colors() {
        let mut lines = Lines::default();
        let mut out = Vec::new();
        for chunk in ["\u{1b}[91m", "  Downloaded itoa v1.0.18\n", "\u{1b}[0m", "\u{1b}[91m  Downloaded ", "matchit v0.8.4\n", "\u{1b}[0m"] {
            out.extend(lines.push(chunk));
        }
        out.extend(lines.finish());
        assert_eq!(out, ["\u{1b}[91m  Downloaded itoa v1.0.18", "\u{1b}[0m\u{1b}[91m  Downloaded matchit v0.8.4"]);

        let mut lines = Lines::default();
        let mut out = lines.push("Step 1/3 : FROM alpine\n\n   \nfetch 10%\rfetch 55%\rfetch 100%\r\nno newline at the end");
        out.extend(lines.finish());
        assert_eq!(out, ["Step 1/3 : FROM alpine", "fetch 100%", "no newline at the end"]);
    }

    #[test]
    fn strip_ansi_keeps_only_the_text() {
        assert_eq!(strip_ansi("\u{1b}[1;31merror\u{1b}[0m: x"), "error: x");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(strip_ansi("\u{1b}]8;;https://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"), "link");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    async fn sample(db: &SqlitePool, container: &str, ts: i64, cpu_percent: f64, mem_bytes: i64) {
        let series = crate::collect::stats::series_id(db, "dep", "web", container).await.unwrap();
        sqlx::query("INSERT INTO container_metrics VALUES (?, ?, ?, ?, 0, 0, 0, 0, 0)")
            .bind(series)
            .bind(ts)
            .bind(cpu_percent)
            .bind(mem_bytes)
            .execute(db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn peaks_only_look_inside_the_window() {
        let db = crate::db::test_pool().await;
        let now = 1_800_000_000;
        let since = now - ADMISSION_WINDOW_SECS;
        sample(&db, "busy", now - 20, 30.0, 512 << 20).await;
        sample(&db, "busy", now - 10, 140.0, 256 << 20).await;
        sample(&db, "busy", since - 1, 600.0, 4 << 30).await;
        sample(&db, "old", since - 60, 90.0, 1 << 30).await;

        let peaks = peaks(&db, since).await.unwrap();
        assert_eq!(peaks["busy"], Peak { cpu_percent: 140.0, mem_bytes: 512 << 20 }, "the highest sample in the window");
        assert!(!peaks.contains_key("old"), "samples before the window don't count");
    }

    #[test]
    fn a_container_counts_its_peak_or_else_its_limits() {
        let idle = PoolMember::new("a".into(), 6.0, 8 * GIB, Some(Peak { cpu_percent: 30.0, mem_bytes: GIB as i64 }));
        assert_eq!(idle.counted, Running { deployment: "a".into(), cpus: 0.3, memory_bytes: GIB });
        assert_eq!((idle.cpus_limit, idle.memory_limit), (6.0, 8 * GIB), "the limits stay for the dashboard");

        let fresh = PoolMember::new("b".into(), 2.0, GIB, None);
        assert_eq!(fresh.counted, Running { deployment: "b".into(), cpus: 2.0, memory_bytes: GIB }, "no samples yet");

        let unlimited = PoolMember::new("c".into(), 0.0, 0, None);
        assert_eq!(unlimited.counted, Running { deployment: "c".into(), cpus: 0.0, memory_bytes: 0 });
    }

    #[test]
    fn idle_limits_leave_room_for_another_deploy() {
        let pool = Pool { cpus: 8.0, memory_bytes: 16 * GIB, storage_bytes: None };
        let members = [PoolMember::new("a".into(), 6.0, 8 * GIB, Some(Peak { cpu_percent: 30.0, mem_bytes: GIB as i64 }))];
        let request = rewrite::Request { service: "web".into(), replicas: 1, cpus: 6.0, memory_bytes: 8 * GIB };
        assert_eq!(admission::admit(pool, &counted(&members), 0, "b", std::slice::from_ref(&request)), Ok(()));

        let busy = [PoolMember::new("a".into(), 6.0, 8 * GIB, Some(Peak { cpu_percent: 550.0, mem_bytes: GIB as i64 }))];
        assert!(admission::admit(pool, &counted(&busy), 0, "b", &[request]).is_err());
    }

    #[test]
    fn only_deployments_and_runners_share_the_pool() {
        let labels = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>();
        assert!(counts_toward_pool(&labels(&[(LABEL_MANAGED, "true"), (LABEL_DEPLOYMENT, "d1")])));
        assert!(counts_toward_pool(&labels(&[(LABEL_MANAGED, "true"), (crate::runners::LABEL_POOL, "p1")])));
        assert!(!counts_toward_pool(&labels(&[(LABEL_MANAGED, "true")])), "kuroe_static and kuroe_traefik");
        assert!(!counts_toward_pool(&labels(&[(LABEL_MANAGED, "true"), ("kuroe.site_build", "d1")])), "site builds");
    }

    fn git(compose_file: Option<&str>, mode: RunMode) -> Spec {
        let mut spec = Spec::new(Source::Git {
            url: "https://example.com/r.git".into(),
            reference: None,
            context: None,
            dockerfile: None,
            token: None,
            identity: None,
            compose_file: compose_file.map(str::to_owned),
        });
        spec.mode = mode;
        spec
    }

    #[test]
    fn git_strategy_follows_the_compose_file() {
        assert_eq!(strategy(&git(None, RunMode::Compose)).0, Strategy::BlueGreen);
        assert_eq!(strategy(&git(Some("compose.yaml"), RunMode::Compose)).0, Strategy::Recreate);
        assert_eq!(strategy(&git(Some("compose.yaml"), RunMode::Stack)).0, Strategy::Stack);
    }
}
