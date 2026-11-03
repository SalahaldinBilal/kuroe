use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
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
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::history::{self, DeploySource, Planned, Step, StepRecord};
use super::spec::{RunMode, SiteBuild, Source, Spec, project_name};
use super::{Begun, Deployment, finish, set_active_project};
use crate::docker::admission::{self, Pool, Running};
use crate::docker::netlimit;
use crate::identity;
use crate::docker::rewrite::{
    self, Build, Defaults, LABEL_DEPLOYMENT, LABEL_MANAGED, LABEL_SERVICE, PROXY_NETWORK, Rewritten, SWARM_NETWORK,
};
use crate::proxy::bluegreen::{self, Event, Phase, Step as Rollout};
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
const SAVE_EVERY: Duration = Duration::from_secs(3);
const SECRET_MASK: &str = "********";

#[derive(Clone)]
pub struct Runner {
    pub db: SqlitePool,
    pub docker: Docker,
    pub data_dir: PathBuf,
    pub traefik: Traefik,
    pub live: LiveDeploys,
}

/// Deploys that are running, by deployment id: their output and steps so far, and a way to cancel them.
#[derive(Clone, Default)]
pub struct LiveDeploys(Arc<Mutex<HashMap<String, Arc<LiveDeploy>>>>);

pub struct LiveDeploy {
    deployment: String,
    /// The history row, set once the deploy has one.
    deploy: OnceLock<String>,
    token: CancellationToken,
    by: Mutex<Option<String>>,
    state: Mutex<LiveState>,
    tx: broadcast::Sender<LiveMessage>,
}

#[derive(Default)]
struct LiveState {
    lines: Vec<String>,
    partial: Option<String>,
    steps: Vec<StepRecord>,
    unsaved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveMessage {
    Snapshot { deploy: Option<String>, lines: Vec<String>, partial: Option<String>, steps: Vec<StepRecord> },
    Line { text: String },
    /// The line being written before its newline, like a progress bar. The next Line replaces it.
    Partial { text: Option<String> },
    Steps { steps: Vec<StepRecord> },
}

/// Holds a deploy's entry until the deploy is done. Taken before the deployment shows as deploying,
/// so a page that sees that status can always follow the deploy.
pub struct InFlight {
    deploys: LiveDeploys,
    live: Arc<LiveDeploy>,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut map = self.deploys.0.lock().unwrap();
        if map.get(&self.live.deployment).is_some_and(|l| Arc::ptr_eq(l, &self.live)) {
            map.remove(&self.live.deployment);
        }
    }
}

impl LiveDeploys {
    /// None while another deploy of this deployment holds its entry.
    pub fn start(&self, deployment: &str) -> Option<InFlight> {
        let mut map = self.0.lock().unwrap();
        if map.contains_key(deployment) {
            return None;
        }
        let live = Arc::new(LiveDeploy {
            deployment: deployment.to_owned(),
            deploy: OnceLock::new(),
            token: CancellationToken::new(),
            by: Mutex::new(None),
            state: Mutex::default(),
            tx: broadcast::channel(512).0,
        });
        map.insert(deployment.to_owned(), live.clone());
        Some(InFlight { deploys: self.clone(), live })
    }

    /// False when no deploy of this deployment is running.
    pub fn cancel(&self, deployment: &str, by: &str) -> bool {
        let Some(live) = self.0.lock().unwrap().get(deployment).cloned() else { return false };
        *live.by.lock().unwrap() = Some(by.to_owned());
        live.token.cancel();
        true
    }

    /// Everything the running deploy has done so far, then what comes after. The receiver closes when the deploy ends.
    pub fn follow(&self, deployment: &str) -> Option<(LiveMessage, broadcast::Receiver<LiveMessage>)> {
        let live = self.0.lock().unwrap().get(deployment).cloned()?;
        Some(live.follow())
    }

    /// The step the running deploy of this deployment is in.
    pub fn current_step(&self, deployment: &str) -> Option<StepRecord> {
        let live = self.0.lock().unwrap().get(deployment).cloned()?;
        let state = live.state.lock().unwrap();
        history::current_step(&state.steps).cloned()
    }
}

impl LiveDeploy {
    fn follow(&self) -> (LiveMessage, broadcast::Receiver<LiveMessage>) {
        let state = self.state.lock().unwrap();
        let snapshot = LiveMessage::Snapshot {
            deploy: self.deploy.get().cloned(),
            lines: state.lines.clone(),
            partial: state.partial.clone(),
            steps: state.steps.clone(),
        };
        (snapshot, self.tx.subscribe())
    }

    fn line(&self, text: &str) {
        let mut state = self.state.lock().unwrap();
        state.lines.push(text.to_owned());
        state.partial = None;
        state.unsaved = true;
        let _ = self.tx.send(LiveMessage::Line { text: text.to_owned() });
    }

    fn partial(&self, text: Option<String>) {
        let mut state = self.state.lock().unwrap();
        if state.partial != text {
            state.partial.clone_from(&text);
            let _ = self.tx.send(LiveMessage::Partial { text });
        }
    }

    fn change_steps(&self, change: impl FnOnce(&mut Vec<StepRecord>)) -> Vec<StepRecord> {
        let mut state = self.state.lock().unwrap();
        change(&mut state.steps);
        state.unsaved = true;
        let _ = self.tx.send(LiveMessage::Steps { steps: state.steps.clone() });
        state.steps.clone()
    }

    fn step(&self, step: Step, detail: Option<String>) {
        let event_detail = detail.clone();
        self.change_steps(|steps| history::start_step(steps, step, detail, history::now_ms()));
        crate::events::send(crate::events::Event::DeployStep { id: self.deployment.clone(), step, detail: event_detail });
    }

    fn unsaved(&self) -> Option<(String, Vec<StepRecord>)> {
        let mut state = self.state.lock().unwrap();
        if !state.unsaved {
            return None;
        }
        state.unsaved = false;
        Some((state.lines.join("\n"), state.steps.clone()))
    }
}

/// Writes the output to the deploy's row every few seconds, so a crash keeps what was there.
async fn save_progress(db: SqlitePool, live: Arc<LiveDeploy>, deploy: String, stop: CancellationToken) {
    let mut ticker = tokio::time::interval(SAVE_EVERY);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = ticker.tick() => {}
        }
        if let Some((output, steps)) = live.unsaved()
            && let Err(err) = history::progress(&db, &deploy, &output, &steps).await
        {
            tracing::warn!("saving deploy output: {err:#}");
        }
    }
}

/// The values a deploy must never print: secret env values, saved secrets and the git token.
fn secret_values(spec: &Spec) -> Vec<&str> {
    let mut values: Vec<&str> = spec
        .container
        .env
        .iter()
        .filter(|(key, _)| spec.secrets.contains(*key) || spec.secret_refs.contains_key(*key))
        .map(|(_, value)| value.as_str())
        .collect();
    if let Source::Git { token: Some(token), .. } = &spec.source {
        values.push(token);
    }
    values
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

/// The steps after the build, which only depend on how the new version replaces the old one.
fn rollout_plan(strategy: Strategy, project: &str, old: Option<&str>) -> Vec<Planned> {
    let project = Some(project.to_owned());
    let mut plan = Vec::new();
    if strategy != Strategy::Stack {
        plan.push((Step::Pulling, None));
    }
    plan.push((Step::Starting, project.clone()));
    match strategy {
        Strategy::BlueGreen => {
            plan.push((Step::HealthCheck, project.clone()));
            plan.push((Step::SwitchingTraffic, project));
            if let Some(old) = old {
                plan.push((Step::Draining, Some(old.to_owned())));
            }
        }
        Strategy::Recreate | Strategy::Stack | Strategy::Static => plan.push((Step::PublishingRoutes, None)),
    }
    if let Some(old) = old {
        plan.push((Step::StoppingOld, Some(old.to_owned())));
    }
    plan
}

/// What a deploy will do once its secrets are resolved, as far as the spec tells before the Compose file is read.
fn first_plan(d: &Deployment) -> Vec<Planned> {
    let mut plan = Vec::new();
    if matches!(d.spec.source, Source::Git { .. }) {
        plan.push((Step::Cloning, None));
    }
    if d.spec.is_static() {
        let site = d.spec.site.as_ref();
        if let Source::Image { image, .. } = &d.spec.source {
            plan.push((Step::Pulling, Some(image.clone())));
        }
        if site.is_some_and(|s| !matches!(s.build, SiteBuild::None)) {
            plan.push((Step::Building, Some("site".into())));
        }
        plan.extend([(Step::InstallingFiles, None), (Step::PublishingRoutes, None)]);
        return plan;
    }
    let (strategy, _) = strategy(&d.spec);
    let project = next_project(&d.name, strategy, d.active_project.as_deref());
    plan.push((Step::CheckingPool, None));
    plan.extend(rollout_plan(strategy, &project, d.active_project.as_deref().filter(|p| *p != project)));
    plan
}

/// The rest of the deploy once the Compose model says what gets built.
fn build_plan(strategy: Strategy, project: &str, old: Option<&str>, builds: &[Build]) -> Vec<Planned> {
    let mut plan = vec![(Step::CheckingPool, None)];
    plan.extend(builds.iter().map(|b| (Step::Building, Some(b.service.clone()))));
    plan.extend(rollout_plan(strategy, project, old));
    plan
}

struct ProjectContainer {
    name: String,
    ip: Option<String>,
}

/// Turns Docker's build stream, which splits wherever it likes, into whole lines.
/// Color codes alone on a line move to the next one, and a line redrawn with `\r` keeps its last frame.
#[derive(Default)]
struct Lines {
    bytes: Vec<u8>,
    partial: String,
    carried: String,
}

impl Lines {
    /// For process output, which can split a character between reads. A newline never sits inside one.
    fn push_bytes(&mut self, chunk: &[u8]) -> Vec<String> {
        self.bytes.extend_from_slice(chunk);
        let Some(end) = self.bytes.iter().rposition(|b| *b == b'\n') else { return Vec::new() };
        let complete: Vec<u8> = self.bytes.drain(..=end).collect();
        self.push(&String::from_utf8_lossy(&complete))
    }

    fn push(&mut self, chunk: &str) -> Vec<String> {
        self.partial.push_str(chunk);
        let mut out = Vec::new();
        while let Some(end) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=end).collect();
            self.take(&line, &mut out);
        }
        out
    }

    /// The line written so far, as it shows now, for the live view.
    fn pending(&self) -> Option<String> {
        let valid = match std::str::from_utf8(&self.bytes) {
            Ok(text) => text,
            Err(err) => std::str::from_utf8(&self.bytes[..err.valid_up_to()]).unwrap_or_default(),
        };
        let text = format!("{}{}{valid}", self.carried, self.partial);
        let frame = text.rsplit('\r').find(|frame| !strip_ansi(frame).trim().is_empty())?;
        Some(frame.trim_end().to_owned())
    }

    fn finish(&mut self) -> Vec<String> {
        let bytes = std::mem::take(&mut self.bytes);
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
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

#[derive(Default)]
pub struct Log {
    lines: Vec<String>,
    live: Option<Arc<LiveDeploy>>,
    hidden: Vec<String>,
    /// What this deploy deploys, with the commit or digest once known.
    source: DeploySource,
}

impl Log {
    fn live(live: Arc<LiveDeploy>, source: DeploySource) -> Self {
        Log { live: Some(live), source, ..Default::default() }
    }

    /// Masks these values wherever they show up, whatever their length.
    fn hide<'a>(&mut self, values: impl IntoIterator<Item = &'a str>) {
        for value in values {
            if !value.is_empty() && !self.hidden.iter().any(|h| h == value) {
                self.hidden.push(value.to_owned());
            }
        }
        self.hidden.sort_by_key(|v| std::cmp::Reverse(v.len()));
    }

    fn masked(&self, mut text: String) -> String {
        for value in &self.hidden {
            if text.contains(value.as_str()) {
                text = text.replace(value.as_str(), SECRET_MASK);
            }
        }
        text
    }

    fn line(&mut self, text: impl Into<String>) {
        let text = self.masked(text.into());
        tracing::debug!("{text}");
        if let Some(live) = &self.live {
            live.line(&text);
        }
        self.lines.push(text);
    }

    fn partial(&self, text: Option<String>) {
        if let Some(live) = &self.live {
            live.partial(text.map(|t| self.masked(t)));
        }
    }

    fn step(&self, step: Step, detail: Option<&str>) {
        if let Some(live) = &self.live {
            live.step(step, detail.map(str::to_owned));
        }
    }

    /// Replaces the steps still to come.
    fn plan(&self, upcoming: &[Planned]) {
        if let Some(live) = &self.live {
            live.change_steps(|steps| history::plan_steps(steps, upcoming));
        }
    }

    fn fail_step(&self) {
        if let Some(live) = &self.live {
            live.change_steps(|steps| history::fail_step(steps));
        }
    }

    fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// Runs a docker CLI command and logs its output line by line as it comes. Returns the exit status and stdout.
async fn run(mut command: Command, log: &mut Log, log_stdout: bool, what: &str) -> anyhow::Result<(ExitStatus, String)> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = command.spawn().with_context(|| format!("running {what}, is the docker CLI installed?"))?;
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        bail!("{what} started without its output pipes");
    };
    let (mut out_lines, mut err_lines) = (Lines::default(), Lines::default());
    let (mut out_buf, mut err_buf) = (vec![0u8; 8192], vec![0u8; 8192]);
    let (mut out_open, mut err_open) = (true, true);
    let mut captured = Vec::new();
    while out_open || err_open {
        let (from_stdout, read) = tokio::select! {
            read = stdout.read(&mut out_buf), if out_open => (true, read?),
            read = stderr.read(&mut err_buf), if err_open => (false, read?),
        };
        match (from_stdout, read) {
            (true, 0) => out_open = false,
            (false, 0) => err_open = false,
            (true, n) => {
                captured.extend_from_slice(&out_buf[..n]);
                if log_stdout {
                    out_lines.push_bytes(&out_buf[..n]).into_iter().for_each(|line| log.line(line));
                    log.partial(out_lines.pending());
                }
            }
            (false, n) => {
                err_lines.push_bytes(&err_buf[..n]).into_iter().for_each(|line| log.line(line));
                log.partial(err_lines.pending());
            }
        }
    }
    if log_stdout {
        out_lines.finish().into_iter().for_each(|line| log.line(line));
    }
    err_lines.finish().into_iter().for_each(|line| log.line(line));
    log.partial(None);
    let status = child.wait().await?;
    Ok((status, String::from_utf8_lossy(&captured).into_owned()))
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

    /// `in_flight` comes from `live.start`, taken before `begin_deploy`. `previous` is the status before this deploy,
    /// for when it's cancelled before touching anything.
    pub async fn deploy(&self, in_flight: InFlight, deployment: Deployment, begun: Begun, host_mounts: bool, previous: String) {
        let live = in_flight.live.clone();
        let _ = live.deploy.set(begun.deploy.clone());
        let mut log = Log::live(live.clone(), DeploySource::of(&deployment.spec));
        let stop_saving = CancellationToken::new();
        let saver = tokio::spawn(save_progress(self.db.clone(), live.clone(), begun.deploy.clone(), stop_saving.clone()));
        let outcome = tokio::select! {
            result = self.try_deploy(&deployment, begun.revision, host_mounts, &mut log) => Some(result),
            () = live.token.cancelled() => None,
        };
        let (status, history_status) = match outcome {
            Some(Ok(())) => ("running".to_owned(), "running"),
            Some(Err(err)) => {
                log.fail_step();
                log.line(format!("deploy failed: {err:#}"));
                ("failed".to_owned(), "failed")
            }
            None => {
                log.partial(None);
                let by = live.by.lock().unwrap().clone();
                log.line(format!("deploy cancelled{}", by.map(|b| format!(" by {b}")).unwrap_or_default()));
                log.plan(&[]);
                log.step(Step::CleaningUp, None);
                (self.after_cancel(&deployment, &previous, &mut log).await, "cancelled")
            }
        };
        let steps = live.change_steps(|steps| history::finish_steps(steps, history::now_ms()));
        stop_saving.cancel();
        let _ = saver.await;
        let output = log.text();
        if let Err(err) = history::set_source(&self.db, &begun.deploy, &log.source).await {
            tracing::error!("saving what {} deployed: {err:#}", deployment.name);
        }
        if let Err(err) = history::close(&self.db, &begun.deploy, history_status, &output, &steps).await {
            tracing::error!("saving the deploy history of {}: {err:#}", deployment.name);
        }
        drop((log, live, in_flight));
        if let Err(err) = finish(&self.db, &deployment.id, &status, &output).await {
            tracing::error!("saving deploy result for {}: {err}", deployment.name);
        }
    }

    async fn save_source(&self, log: &Log) {
        let Some(deploy) = log.live.as_ref().and_then(|l| l.deploy.get()) else { return };
        if let Err(err) = history::set_source(&self.db, deploy, &log.source).await {
            tracing::warn!("saving what this deploy deploys: {err:#}");
        }
    }

    /// The digest Swarm pinned the service to. Its nodes pull for themselves, so the local image can't tell.
    async fn record_stack_digest(&self, project: &str, log: &mut Log) {
        let service = format!("{project}_{}", super::spec::SINGLE_SERVICE);
        let Ok(inspected) = self.docker.inspect_service(&service, None::<bollard::query_parameters::InspectServiceOptions>).await else {
            return;
        };
        let image = inspected.spec.and_then(|s| s.task_template).and_then(|t| t.container_spec).and_then(|c| c.image);
        if let Some(digest) = image.as_deref().and_then(|i| i.split_once('@')).map(|(_, sha)| sha.to_owned()) {
            log.source.digest = Some(digest);
            self.save_source(log).await;
        }
    }

    /// The digest the image tag pointed at when it was pulled, for the history.
    async fn record_digest(&self, image: &str, log: &mut Log) {
        let Ok(inspected) = self.docker.inspect_image(image).await else { return };
        let digest = inspected.repo_digests.unwrap_or_default().into_iter().find_map(|d| d.split_once('@').map(|(_, sha)| sha.to_owned()));
        if digest.is_some() {
            log.source.digest = digest;
            self.save_source(log).await;
        }
    }

    /// Takes down what the cancelled deploy started, keeps the old version serving, and says what runs now.
    async fn after_cancel(&self, d: &Deployment, previous: &str, log: &mut Log) -> String {
        let site_builds = HashMap::from([("label", vec![format!("{}={}", sites::LABEL_SITE_BUILD, d.id)])]);
        if let Ok(left) = self.docker.list_containers(Some(ListContainersOptionsBuilder::new().all(true).filters(&site_builds).build())).await {
            for c in left.into_iter().filter_map(|c| c.id) {
                let remove = bollard::query_parameters::RemoveContainerOptionsBuilder::new().force(true).build();
                let _ = self.docker.remove_container(&c, Some(remove)).await;
            }
        }
        let (strategy, _) = strategy(&d.spec);
        let active = d.active_project.as_deref();
        let started = next_project(&d.name, strategy, active);
        match strategy {
            Strategy::Static | Strategy::Stack => {
                log.line(if strategy == Strategy::Static { "the files from before keep serving" } else { "Swarm finishes or rolls back an update it already started" });
                return previous.to_owned();
            }
            Strategy::BlueGreen if active != Some(started.as_str()) => {
                if let Err(err) = self.compose_down(&started, log).await {
                    log.line(format!("warning: taking down {started}: {err:#}"));
                }
                if let Err(err) = self.publish_routes(d, active).await {
                    log.line(format!("warning: restoring routes: {err:#}"));
                }
            }
            _ => {}
        }
        let serving = active.unwrap_or(&started);
        match self.project_containers(serving, None).await {
            Ok(list) if !list.is_empty() => {
                log.line(if strategy == Strategy::BlueGreen {
                    format!("{serving} keeps serving")
                } else {
                    format!("{serving} has containers running. It restarts in place, so some may come from the cancelled deploy.")
                });
                "running".to_owned()
            }
            Ok(_) if previous == "created" => "created".to_owned(),
            Ok(_) => "stopped".to_owned(),
            Err(err) => {
                log.line(format!("warning: checking what runs: {err:#}"));
                previous.to_owned()
            }
        }
    }

    async fn try_deploy(&self, d: &Deployment, revision: i64, host_mounts: bool, log: &mut Log) -> anyhow::Result<()> {
        log.step(Step::ResolvingSecrets, None);
        let resolved = Deployment { spec: crate::saved_secrets::resolve(&self.db, &d.spec).await?, ..d.clone() };
        let d = &resolved;
        log.hide(secret_values(&d.spec));
        log.plan(&first_plan(d));
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
        let old = d.active_project.as_deref().filter(|p| *p != project);
        log.plan(&build_plan(strategy, &project, old, &builds));

        log.step(Step::CheckingPool, None);
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
            log.step(Step::Starting, Some(project.as_str()));
            self.docker_cli_with(&project_dir, &args, docker_config, log).await?;
            if matches!(d.spec.source, Source::Image { .. }) {
                self.record_stack_digest(&project, log).await;
            }
            log.step(Step::PublishingRoutes, None);
            self.publish_routes(d, Some(&project)).await?;
            self.down_other_projects(d, &project, log).await?;
            set_active_project(&self.db, &d.id, &project).await?;
            log.line(format!("revision {revision} is running as stack {project}"));
            return Ok(());
        }

        tokio::fs::write(project_dir.join("kuroe.compose.json"), serde_json::to_vec_pretty(&config)?).await?;
        log.step(Step::Pulling, None);
        let pull = ["pull", "--ignore-buildable", "--policy", "missing"];
        self.compose_with(&project_dir, &project, Some("kuroe.compose.json"), &pull, docker_config, log).await?;
        let up = ["up", "-d", "--wait", "--wait-timeout", "120", "--remove-orphans"];
        log.step(Step::Starting, Some(project.as_str()));
        let started = self.compose_with(&project_dir, &project, Some("kuroe.compose.json"), &up, docker_config, log).await;
        if started.is_ok() {
            self.apply_net_limits(&project, log).await;
            if let Source::Image { image, .. } = &d.spec.source {
                self.record_digest(image, log).await;
            }
        }

        match strategy {
            Strategy::Recreate => {
                started?;
                log.step(Step::PublishingRoutes, None);
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
            Ok(_) => {
                log.step(Step::HealthCheck, Some(project));
                self.health_check(d, project).await
            }
        };
        let mut phase = Phase::Starting;
        let mut event = Some(first);
        let mut failure = None;
        while let Some(current) = event.take() {
            if matches!(current, Event::Unhealthy(_) | Event::Timeout | Event::SwitchFailed(_)) {
                log.fail_step();
            }
            let (next_phase, steps) = bluegreen::next(phase, current).map_err(anyhow::Error::msg)?;
            phase = next_phase;
            for step in steps {
                event = match step {
                    Rollout::SwitchRoutes => {
                        log.step(Step::SwitchingTraffic, Some(project));
                        log.line(format!("switching traffic to {project}"));
                        Some(match self.publish_routes(d, Some(project)).await {
                            Ok(()) => Event::Switched,
                            Err(err) => Event::SwitchFailed(format!("{err:#}")),
                        })
                    }
                    Rollout::Drain => {
                        let drain = proxy::load_settings(&self.db).await?.drain_seconds;
                        if let Some(old) = old {
                            log.step(Step::Draining, Some(old));
                            log.line(format!("draining the old version for {drain}s"));
                            tokio::time::sleep(Duration::from_secs(drain)).await;
                        }
                        Some(Event::Drained)
                    }
                    Rollout::StopOld => {
                        self.down_other_projects(d, project, log).await?;
                        None
                    }
                    Rollout::RestoreRoutes => {
                        log.plan(&[]);
                        log.step(Step::RestoringRoutes, old);
                        if let Err(err) = self.publish_routes(d, old).await {
                            log.line(format!("restoring routes failed: {err:#}"));
                        }
                        None
                    }
                    Rollout::StopNew => {
                        log.plan(&[]);
                        log.step(Step::StoppingNew, Some(project));
                        self.compose_down(project, log).await?;
                        Some(Event::RolledBack)
                    }
                    Rollout::Report(message) => {
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
                log.step(Step::Pulling, Some(image.as_str()));
                self.docker_cli_with(&dir, &["pull", image], login.as_ref().map(|l| l.path()), log).await?;
                self.record_digest(image, log).await;
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
                log.step(Step::Building, Some("site"));
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
        log.step(Step::InstallingFiles, None);
        sites.ensure().await?;
        let count = sites.install(&d.id, revision, files, &out, hoist).await?;
        log.line(format!("installed {count} files as revision {revision}"));
        let previous = d.active_project.as_deref().and_then(sites::revision_of);
        let project = sites::project(revision);
        set_active_project(&self.db, &d.id, &project).await?;
        log.step(Step::PublishingRoutes, None);
        self.publish_routes(d, Some(&project)).await?;
        let keep: Vec<i64> = std::iter::once(revision).chain(previous).collect();
        if let Err(err) = sites.prune(&d.id, &keep).await {
            log.line(format!("warning: older revisions were not removed: {err:#}"));
        }
        log.line(format!("revision {revision} is live"));
        Ok(())
    }

    pub async fn stop(&self, d: &Deployment) -> anyhow::Result<String> {
        let mut log = Log::default();
        self.publish_routes(d, None).await?;
        self.down_other_projects(d, "", &mut log).await?;
        Ok(log.text())
    }

    pub async fn restart(&self, d: &Deployment) -> anyhow::Result<String> {
        let mut log = Log::default();
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
        let mut log = Log::default();
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
                log.step(Step::StoppingOld, Some(project.as_str()));
                log.line(format!("stopping {project}"));
                self.compose_down(&project, log).await?;
            }
        }
        let root = self.data_dir.join("deployments");
        for stack in self.labelled(&d.id, LABEL_STACK).await? {
            if stack != keep {
                log.step(Step::StoppingOld, Some(stack.as_str()));
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
        command.args(args).current_dir(dir);
        let (status, stdout) = run(command, log, true, "docker").await?;
        if !status.success() {
            bail!("docker {} exited with {status}", args.join(" "));
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
        let mut command = Command::new("docker");
        command.arg("compose").args(["-p", project, "-f", file, "config", "--format", "json"]).envs(env).current_dir(dir);
        let (status, stdout) = run(command, log, false, "docker compose").await?;
        if !status.success() {
            bail!("docker compose config exited with {status}");
        }
        serde_json::from_str(&stdout).context("parsing docker compose config output")
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
        command.args(args).current_dir(dir);
        let (status, stdout) = run(command, log, args.first() != Some(&"config"), "docker compose").await?;
        if !status.success() {
            bail!("docker compose {} exited with {status}", args.join(" "));
        }
        Ok(stdout)
    }

    async fn registry_login(&self, d: &Deployment, log: &mut Log) -> anyhow::Result<Option<identity::registry::TempDockerConfig>> {
        let Source::Image { registry_identity: Some(id), .. } = &d.spec.source else { return Ok(None) };
        let saved = identity::get(&self.db, id).await?.context("the registry login for this deployment was removed")?;
        log.hide([saved.secret.as_str()]);
        log.line(format!("pulling with the registry login {}", saved.name));
        identity::touch(&self.db, id).await?;
        Ok(Some(identity::registry::write_docker_config(&self.data_dir, &saved)?))
    }

    async fn clone_source(&self, d: &Deployment, log: &mut Log) -> anyhow::Result<PathBuf> {
        let Source::Git { url, reference, token, identity: login, .. } = &d.spec.source else { unreachable!() };
        let src = self.deployment_dir(&d.id).join("src");
        log.step(Step::Cloning, None);
        log.line(format!("cloning {}", history::without_login(url)));
        let creds = match login {
            Some(id) => {
                let saved = identity::get(&self.db, id).await?.context("the Git login for this deployment was removed")?;
                identity::check_git_url(&saved, url)?;
                log.line(format!("using the Git login {}", saved.name));
                Some(identity::git_credentials(&self.db, &self.traefik.http, &saved).await?)
            }
            None => token.as_deref().map(|t| (crate::git::token_login(t).0.to_owned(), t.to_owned())),
        };
        if let Some((_, password)) = &creds {
            log.hide([password.as_str(), crate::git::encode(password).as_str()]);
        }
        let login = creds.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
        let commit = crate::git::shallow_clone(url, reference.as_deref(), login, &src).await?;
        log.line(format!("at commit {commit}"));
        log.source.commit = Some(commit);
        self.save_source(log).await;
        Ok(src)
    }

    async fn build(&self, build: &Build, log: &mut Log) -> anyhow::Result<()> {
        log.step(Step::Building, Some(build.service.as_str()));
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
        log.step(Step::Building, Some("site"));
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
                log.partial(lines.pending());
            }
            if let Some(err) = info.error_detail {
                lines.finish().into_iter().for_each(|line| log.line(line));
                log.partial(None);
                bail!("build of {service} failed: {}", err.message.unwrap_or_default());
            }
        }
        lines.finish().into_iter().for_each(|line| log.line(line));
        log.partial(None);
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
    fn a_deploy_can_be_cancelled_only_while_it_runs() {
        let deploys = LiveDeploys::default();
        assert!(!deploys.cancel("d1", "boss"), "nothing running yet");
        let in_flight = deploys.start("d1").unwrap();
        assert!(deploys.start("d1").is_none(), "one deploy per deployment");
        assert!(!in_flight.live.token.is_cancelled());
        assert!(deploys.cancel("d1", "boss"));
        assert!(in_flight.live.token.is_cancelled());
        assert_eq!(in_flight.live.by.lock().unwrap().as_deref(), Some("boss"));
        assert!(!deploys.cancel("d2", "boss"), "only that deployment");
        drop(in_flight);
        assert!(!deploys.cancel("d1", "boss"), "the entry goes when the deploy is done");
        assert!(deploys.start("d1").is_some(), "and the next deploy can take it");
    }

    #[tokio::test]
    async fn a_live_deploy_can_be_followed_from_any_point() {
        let deploys = LiveDeploys::default();
        let in_flight = deploys.start("d1").unwrap();
        in_flight.live.deploy.set("deploy-1".into()).unwrap();
        let mut log = Log::live(in_flight.live.clone(), DeploySource::default());
        log.step(Step::ResolvingSecrets, None);
        log.plan(&[(Step::Cloning, None), (Step::Building, Some("web".into()))]);
        log.step(Step::Cloning, None);
        log.line("cloning");
        log.partial(Some("fetch 40%".into()));

        let (snapshot, mut rx) = deploys.follow("d1").unwrap();
        let LiveMessage::Snapshot { deploy, lines, partial, steps } = snapshot else { panic!("{snapshot:?}") };
        assert_eq!((deploy.as_deref(), lines, partial.as_deref()), (Some("deploy-1"), vec!["cloning".to_owned()], Some("fetch 40%")));
        let started: Vec<(Step, bool)> = steps.iter().map(|s| (s.step, s.started_ms.is_some())).collect();
        assert_eq!(started, [(Step::ResolvingSecrets, true), (Step::Cloning, true), (Step::Building, false)], "the build is still to come");

        log.partial(Some("fetch 40%".into()));
        log.step(Step::Building, Some("web"));
        log.line("Step 1/3");
        let LiveMessage::Steps { steps } = rx.recv().await.unwrap() else { panic!("an unchanged partial line is not sent again") };
        assert!(steps[1].finished_ms.is_some(), "a new step ends the one before");
        assert!(steps[2].started_ms.is_some() && steps[2].detail.as_deref() == Some("web"));
        assert_eq!(rx.recv().await.unwrap(), LiveMessage::Line { text: "Step 1/3".into() });
        assert_eq!(deploys.current_step("d1").map(|s| s.step), Some(Step::Building));
        assert_eq!(in_flight.live.unsaved().map(|(text, _)| text).as_deref(), Some("cloning\nStep 1/3"));
        assert!(in_flight.live.unsaved().is_none(), "nothing new since the last save");

        drop((log, in_flight));
        assert!(rx.recv().await.is_err(), "the stream ends with the deploy");
        assert!(deploys.follow("d1").is_none());
    }

    #[test]
    fn deploy_output_masks_secret_values() {
        let mut spec = Spec::new(Source::Git {
            url: "https://example.com/r.git".into(),
            reference: None,
            context: None,
            dockerfile: None,
            token: Some("ghp_token123".into()),
            identity: None,
            compose_file: None,
        });
        spec.container.env = BTreeMap::from([
            ("DB_PASSWORD".into(), "hunter22".into()),
            ("API_KEY".into(), "from-saved".into()),
            ("PIN".into(), "123".into()),
            ("LOG".into(), "info".into()),
        ]);
        spec.secrets = BTreeSet::from(["DB_PASSWORD".into(), "PIN".into()]);
        spec.secret_refs = BTreeMap::from([("API_KEY".into(), "saved-1".into())]);
        let mut log = Log::default();
        log.hide(secret_values(&spec));
        log.line("connecting with hunter22 and from-saved, token ghp_token123, log info, pin 123");
        assert_eq!(log.text(), "connecting with ******** and ********, token ********, log info, pin ********", "short secrets too");
    }

    #[test]
    fn the_line_in_progress_shows_its_latest_redraw() {
        let mut lines = Lines::default();
        assert_eq!(lines.push_bytes(b"done\nfetch 10%\rfetch 55%"), ["done"]);
        assert_eq!(lines.pending().as_deref(), Some("fetch 55%"));
        let split = "größe".as_bytes();
        let mut lines = Lines::default();
        lines.push_bytes(&split[..3]);
        assert_eq!(lines.pending().as_deref(), Some("gr"), "half a character waits for the rest");
        assert_eq!(Lines::default().pending(), None);
    }

    fn named(plan: &[Planned]) -> Vec<Step> {
        plan.iter().map(|(step, _)| *step).collect()
    }

    #[test]
    fn plans_follow_the_strategy() {
        let blue_green = rollout_plan(Strategy::BlueGreen, "shop-green", Some("shop-blue"));
        assert_eq!(
            named(&blue_green),
            [Step::Pulling, Step::Starting, Step::HealthCheck, Step::SwitchingTraffic, Step::Draining, Step::StoppingOld]
        );
        assert_eq!(named(&rollout_plan(Strategy::BlueGreen, "shop-blue", None)), [Step::Pulling, Step::Starting, Step::HealthCheck, Step::SwitchingTraffic]);
        assert_eq!(named(&rollout_plan(Strategy::Recreate, "shop", None)), [Step::Pulling, Step::Starting, Step::PublishingRoutes]);
        assert_eq!(named(&rollout_plan(Strategy::Stack, "shop", None)), [Step::Starting, Step::PublishingRoutes], "Swarm nodes pull for themselves");

        let mut d = Deployment {
            id: "d1".into(),
            name: "shop".into(),
            spec: git(None, RunMode::Compose),
            status: "running".into(),
            revision: 1,
            last_output: String::new(),
            active_project: None,
            created_by: None,
            created_at: 0,
            updated_at: 0,
        };
        assert_eq!(named(&first_plan(&d))[..2], [Step::Cloning, Step::CheckingPool]);
        d.spec = Spec::new(Source::image("nginx"));
        d.spec.site = Some(Default::default());
        assert_eq!(named(&first_plan(&d)), [Step::Pulling, Step::InstallingFiles, Step::PublishingRoutes]);

        let build = Build { service: "web".into(), context: ".".into(), dockerfile: "Dockerfile".into(), args: Default::default(), tag: "t".into() };
        let plan = build_plan(Strategy::Recreate, "shop", None, &[build]);
        assert_eq!(plan[1], (Step::Building, Some("web".to_owned())));
    }

    #[test]
    fn process_output_keeps_characters_split_between_reads() {
        let mut lines = Lines::default();
        let text = "größe\nzwei";
        let bytes = text.as_bytes();
        let mut out = lines.push_bytes(&bytes[..3]);
        out.extend(lines.push_bytes(&bytes[3..]));
        out.extend(lines.finish());
        assert_eq!(out, ["größe", "zwei"]);
    }

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
