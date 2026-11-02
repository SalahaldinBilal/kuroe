//! Self-hosted CI runners. A pool says which provider and repository or group to serve and how many runners to keep.
//! Runner instances aren't stored: Docker labels on their containers are the source of truth.

pub mod providers;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, NetworkCreateRequest, RestartPolicy, RestartPolicyNameEnum, VolumeCreateRequest};
use bollard::query_parameters::{CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ListContainersOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder};
use futures::StreamExt;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::Notify;

use crate::db::now;
use crate::docker::admission;
use crate::docker::netlimit::{LABEL_NET_IN, LABEL_NET_OUT};
use crate::docker::rewrite::{LABEL_MANAGED, Request};
use crate::identity::{self, Kind};

pub const LABEL_POOL: &str = "kuroe.runner_pool";
const LABEL_ROLE: &str = "kuroe.runner_role";
const LABEL_REMOTE: &str = "kuroe.runner_remote";
const LABEL_INSTANCE: &str = "kuroe.runner";
const LABEL_SLOT: &str = "kuroe.runner_slot";
const DIND_IMAGE: &str = "docker:29-dind";
const TICK: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Github,
    Gitlab,
    Gitea,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
    /// One repository (a project on GitLab).
    Repo,
    /// An organization (a group on GitLab).
    Org,
    /// The whole instance, GitLab and Gitea only.
    Instance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockerMode {
    /// A Docker-in-Docker sidecar per runner, so jobs can't see the host's containers.
    Dind,
    /// The host's Docker socket. Root on the host.
    Host,
    /// No Docker in jobs.
    None,
}

macro_rules! text_enum {
    ($ty:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $ty {
            pub fn as_str(self) -> &'static str {
                match self { $($ty::$variant => $text),+ }
            }
            pub fn parse(text: &str) -> Option<$ty> {
                match text { $($text => Some($ty::$variant),)+ _ => None }
            }
        }
    };
}

text_enum!(Provider { Github => "github", Gitlab => "gitlab", Gitea => "gitea" });
text_enum!(ScopeKind { Repo => "repo", Org => "org", Instance => "instance" });
text_enum!(DockerMode { Dind => "dind", Host => "host", None => "none" });

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pool {
    pub id: String,
    pub name: String,
    pub provider: Provider,
    pub identity: String,
    pub scope_kind: ScopeKind,
    pub scope: String,
    pub labels: Vec<String>,
    /// Idle runners to keep ready. GitHub only, the others keep `max` long-lived runners.
    pub min_idle: i64,
    pub max: i64,
    pub cpus: Option<f64>,
    pub memory_mb: Option<i64>,
    pub net_in_mbit: Option<f64>,
    pub net_out_mbit: Option<f64>,
    pub docker_mode: DockerMode,
    pub image: Option<String>,
    pub enabled: bool,
    pub last_error: Option<String>,
    pub created_by: Option<String>,
    pub created_at: i64,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    name: String,
    provider: String,
    identity: String,
    scope_kind: String,
    scope: String,
    labels: String,
    min_idle: i64,
    max: i64,
    cpus: Option<f64>,
    memory_mb: Option<i64>,
    net_in_mbit: Option<f64>,
    net_out_mbit: Option<f64>,
    docker_mode: String,
    image: Option<String>,
    enabled: bool,
    last_error: Option<String>,
    created_by: Option<String>,
    created_at: i64,
}

impl TryFrom<Row> for Pool {
    type Error = anyhow::Error;

    fn try_from(row: Row) -> anyhow::Result<Pool> {
        Ok(Pool {
            provider: Provider::parse(&row.provider).with_context(|| format!("runner pool {} has unknown provider {}", row.name, row.provider))?,
            scope_kind: ScopeKind::parse(&row.scope_kind).with_context(|| format!("runner pool {} has unknown scope {}", row.name, row.scope_kind))?,
            docker_mode: DockerMode::parse(&row.docker_mode).with_context(|| format!("runner pool {} has unknown Docker mode", row.name))?,
            labels: serde_json::from_str(&row.labels).unwrap_or_default(),
            id: row.id,
            name: row.name,
            identity: row.identity,
            scope: row.scope,
            min_idle: row.min_idle,
            max: row.max,
            cpus: row.cpus,
            memory_mb: row.memory_mb,
            net_in_mbit: row.net_in_mbit,
            net_out_mbit: row.net_out_mbit,
            image: row.image,
            enabled: row.enabled,
            last_error: row.last_error,
            created_by: row.created_by,
            created_at: row.created_at,
        })
    }
}

pub async fn list(db: &SqlitePool) -> anyhow::Result<Vec<Pool>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, name, provider, identity, scope_kind, scope, labels, min_idle, max, cpus, memory_mb, net_in_mbit, net_out_mbit,
                docker_mode, image, enabled, last_error, created_by, created_at
         FROM runner_pools ORDER BY name",
    )
    .fetch_all(db)
    .await?;
    rows.into_iter().map(Pool::try_from).collect()
}

pub async fn get(db: &SqlitePool, id: &str) -> anyhow::Result<Option<Pool>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, name, provider, identity, scope_kind, scope, labels, min_idle, max, cpus, memory_mb, net_in_mbit, net_out_mbit,
                docker_mode, image, enabled, last_error, created_by, created_at
         FROM runner_pools WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    row.map(Pool::try_from).transpose()
}

/// What can be set when creating a pool. Provider, identity and scope stay fixed afterwards.
#[derive(Debug, Clone, Deserialize)]
pub struct PoolInput {
    pub name: String,
    pub provider: Provider,
    pub identity: String,
    pub scope_kind: ScopeKind,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default = "one")]
    pub min_idle: i64,
    #[serde(default = "two")]
    pub max: i64,
    #[serde(default)]
    pub cpus: Option<f64>,
    #[serde(default)]
    pub memory_mb: Option<i64>,
    #[serde(default)]
    pub net_in_mbit: Option<f64>,
    #[serde(default)]
    pub net_out_mbit: Option<f64>,
    pub docker_mode: DockerMode,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn one() -> i64 {
    1
}

fn two() -> i64 {
    2
}

fn yes() -> bool {
    true
}

impl PoolInput {
    fn normalize(mut self) -> PoolInput {
        self.name = self.name.trim().to_owned();
        self.scope = self.scope.trim().trim_matches('/').to_owned();
        self.labels = self.labels.iter().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect();
        self.image = self.image.map(|i| i.trim().to_owned()).filter(|i| !i.is_empty());
        if self.scope_kind == ScopeKind::Instance {
            self.scope.clear();
        }
        self
    }

    pub fn check(&self, identity_kind: Kind) -> Result<(), String> {
        if !crate::deploy::spec::valid_name(&self.name) {
            return Err("name must be 1 to 40 characters of a-z, 0-9 and '-'".into());
        }
        let fits = match self.provider {
            Provider::Github => matches!(identity_kind, Kind::GithubApp | Kind::GithubToken),
            Provider::Gitlab => identity_kind == Kind::Gitlab,
            Provider::Gitea => identity_kind == Kind::Gitea,
        };
        if !fits {
            return Err("that identity is for another provider".into());
        }
        match (self.provider, self.scope_kind) {
            (Provider::Github, ScopeKind::Instance) => return Err("GitHub runners belong to a repository or an organization".into()),
            (_, ScopeKind::Repo) if self.scope.split('/').filter(|p| !p.is_empty()).count() < 2 => {
                return Err("give the repository as owner/name".into());
            }
            (_, ScopeKind::Org) if self.scope.is_empty() => return Err("give the organization or group".into()),
            _ => {}
        }
        if !(1..=20).contains(&self.max) || !(0..=self.max).contains(&self.min_idle) {
            return Err("keep between 1 and 20 runners, with no more idle ones than that".into());
        }
        if let Some(label) = self.labels.iter().find(|l| l.len() > 64 || !l.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:/".contains(c))) {
            return Err(format!("the label {label:?} has characters runners don't take"));
        }
        if self.cpus.is_some_and(|c| c <= 0.0) || self.memory_mb.is_some_and(|m| m < 256) {
            return Err("give runners more than 0 CPUs and at least 256 MB".into());
        }
        if [self.net_in_mbit, self.net_out_mbit].iter().flatten().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err("network limits must be a positive number of Mbit/s".into());
        }
        Ok(())
    }
}

pub async fn insert(db: &SqlitePool, input: PoolInput, identity_kind: Kind, created_by: &str) -> anyhow::Result<Result<Pool, String>> {
    let input = input.normalize();
    if let Err(message) = input.check(identity_kind) {
        return Ok(Err(message));
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO runner_pools (id, name, provider, identity, scope_kind, scope, labels, min_idle, max, cpus, memory_mb, net_in_mbit, net_out_mbit,
                                   docker_mode, image, enabled, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.name)
    .bind(input.provider.as_str())
    .bind(&input.identity)
    .bind(input.scope_kind.as_str())
    .bind(&input.scope)
    .bind(serde_json::to_string(&input.labels)?)
    .bind(input.min_idle)
    .bind(input.max)
    .bind(input.cpus)
    .bind(input.memory_mb)
    .bind(input.net_in_mbit)
    .bind(input.net_out_mbit)
    .bind(input.docker_mode.as_str())
    .bind(&input.image)
    .bind(input.enabled)
    .bind(created_by)
    .bind(now())
    .execute(db)
    .await?;
    Ok(Ok(get(db, &id).await?.context("runner pool vanished after insert")?))
}

/// Saves the settings that can change after creation.
pub async fn update(db: &SqlitePool, existing: &Pool, input: PoolInput, identity_kind: Kind) -> anyhow::Result<Result<(), String>> {
    let input = PoolInput {
        provider: existing.provider,
        identity: existing.identity.clone(),
        scope_kind: existing.scope_kind,
        scope: existing.scope.clone(),
        ..input
    }
    .normalize();
    if let Err(message) = input.check(identity_kind) {
        return Ok(Err(message));
    }
    sqlx::query(
        "UPDATE runner_pools SET name = ?, labels = ?, min_idle = ?, max = ?, cpus = ?, memory_mb = ?, net_in_mbit = ?, net_out_mbit = ?,
                docker_mode = ?, image = ?, enabled = ?, last_error = NULL WHERE id = ?",
    )
    .bind(&input.name)
    .bind(serde_json::to_string(&input.labels)?)
    .bind(input.min_idle)
    .bind(input.max)
    .bind(input.cpus)
    .bind(input.memory_mb)
    .bind(input.net_in_mbit)
    .bind(input.net_out_mbit)
    .bind(input.docker_mode.as_str())
    .bind(&input.image)
    .bind(input.enabled)
    .bind(&existing.id)
    .execute(db)
    .await?;
    Ok(Ok(()))
}

pub async fn delete(db: &SqlitePool, id: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM runner_pools WHERE id = ?").bind(id).execute(db).await?;
    Ok(())
}

async fn set_error(db: &SqlitePool, id: &str, error: Option<&str>) -> sqlx::Result<()> {
    sqlx::query("UPDATE runner_pools SET last_error = ? WHERE id = ?").bind(error).bind(id).execute(db).await?;
    Ok(())
}

pub async fn uses_identity(db: &SqlitePool, identity: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar("SELECT name FROM runner_pools WHERE identity = ? ORDER BY name").bind(identity).fetch_all(db).await
}

pub fn default_image(provider: Provider) -> &'static str {
    match provider {
        Provider::Github => "ghcr.io/actions/actions-runner:latest",
        Provider::Gitlab => "gitlab/gitlab-runner:alpine",
        Provider::Gitea => "gitea/act_runner:latest",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Instance {
    pub name: String,
    pub container: String,
    pub state: String,
    pub status: String,
    pub remote_id: Option<String>,
    pub created: i64,
    /// From the provider, when it says. GitHub only.
    pub busy: Option<bool>,
}

/// How many runners to start: GitHub keeps `min_idle` ready up to `max`, the others keep `max` long-lived runners.
pub fn wanted(pool: &Pool, running: usize, idle: usize) -> usize {
    let max = pool.max.max(0) as usize;
    let room = max.saturating_sub(running);
    match pool.provider {
        Provider::Github => (pool.min_idle.max(0) as usize).saturating_sub(idle).min(room),
        _ => room,
    }
}

/// One build cache per runner slot, because two Docker daemons can't share a data folder.
pub fn cache_volume(pool_id: &str, slot: usize) -> String {
    format!("kuroe_runnercache_{pool_id}_{slot}")
}

pub fn first_free(taken: &HashSet<usize>) -> usize {
    (0..).find(|slot| !taken.contains(slot)).unwrap_or(0)
}

/// Names for one runner: the container, its Docker sidecar and their network.
pub fn names(pool: &Pool, suffix: &str) -> (String, String, String) {
    let base = format!("kuroe_runner_{}_{suffix}", pool.name);
    (base.clone(), format!("{base}_docker"), format!("{base}_net"))
}

#[derive(Clone)]
pub struct Runners {
    pub db: SqlitePool,
    pub docker: Docker,
    pub http: reqwest::Client,
    pub runner: crate::deploy::runner::Runner,
    pub wake: Arc<Notify>,
}

impl Runners {
    pub fn spawn(self) {
        tokio::spawn(async move {
            loop {
                if let Err(err) = self.tick().await {
                    tracing::warn!("runner supervisor: {err:#}");
                }
                tokio::select! {
                    _ = tokio::time::sleep(TICK) => {}
                    _ = self.wake.notified() => {}
                }
            }
        });
    }

    async fn tick(&self) -> anyhow::Result<()> {
        let pools = list(&self.db).await?;
        let known: HashSet<&str> = pools.iter().map(|p| p.id.as_str()).collect();
        for leftover in self.containers(None).await? {
            let pool = leftover.labels.get(LABEL_POOL).map(String::as_str).unwrap_or_default();
            if !known.contains(pool) && leftover.labels.get(LABEL_ROLE).map(String::as_str) == Some("runner") {
                self.remove_instance(&leftover.name).await;
            }
        }
        for pool in &pools {
            let result = if pool.enabled { self.reconcile(pool).await } else { self.stop_all(pool).await.map(|_| ()) };
            let error = result.as_ref().err().map(|e| format!("{e:#}"));
            if error != pool.last_error {
                set_error(&self.db, &pool.id, error.as_deref()).await?;
            }
        }
        Ok(())
    }

    async fn containers(&self, pool: Option<&str>) -> anyhow::Result<Vec<Labelled>> {
        let label = match pool {
            Some(id) => format!("{LABEL_POOL}={id}"),
            None => LABEL_POOL.to_owned(),
        };
        let filters = HashMap::from([("label", vec![label])]);
        let options = ListContainersOptionsBuilder::new().all(true).filters(&filters).build();
        Ok(self
            .docker
            .list_containers(Some(options))
            .await?
            .into_iter()
            .map(|c| Labelled {
                name: c.names.and_then(|n| n.first().cloned()).unwrap_or_default().trim_start_matches('/').to_owned(),
                id: c.id.unwrap_or_default(),
                state: c.state.map(|s| s.to_string()).unwrap_or_default(),
                status: c.status.unwrap_or_default(),
                created: c.created.unwrap_or_default(),
                labels: c.labels.unwrap_or_default(),
            })
            .collect())
    }

    /// The runner containers of a pool, with whether each is busy when the provider says.
    pub async fn instances(&self, pool: &Pool) -> anyhow::Result<Vec<Instance>> {
        let busy = match identity::get(&self.db, &pool.identity).await? {
            Some(identity) => providers::busy(&self.db, &self.http, pool, &identity).await.unwrap_or_default(),
            None => HashMap::new(),
        };
        Ok(self
            .containers(Some(&pool.id))
            .await?
            .into_iter()
            .filter(|c| c.labels.get(LABEL_ROLE).map(String::as_str) == Some("runner"))
            .map(|c| {
                let remote_id = c.labels.get(LABEL_REMOTE).cloned();
                Instance {
                    busy: remote_id.as_ref().and_then(|id| busy.get(id).copied()),
                    name: c.labels.get(LABEL_INSTANCE).cloned().unwrap_or(c.name),
                    container: c.id,
                    state: c.state,
                    status: c.status,
                    remote_id,
                    created: c.created,
                }
            })
            .collect())
    }

    async fn reconcile(&self, pool: &Pool) -> anyhow::Result<()> {
        let identity = identity::get(&self.db, &pool.identity).await?.context("the identity for this pool was removed")?;
        let instances = self.instances(pool).await?;
        for gone in instances.iter().filter(|i| matches!(i.state.as_str(), "exited" | "dead" | "created")) {
            self.remove_instance(&gone.name).await;
            if pool.provider != Provider::Github
                && let Some(remote) = &gone.remote_id
            {
                let _ = providers::deregister(&self.db, &self.http, pool, &identity, remote).await;
            }
        }
        let running: Vec<&Instance> = instances.iter().filter(|i| i.state == "running").collect();
        let idle = running.iter().filter(|i| i.busy != Some(true)).count();
        for _ in 0..wanted(pool, running.len(), idle) {
            self.admit(pool).await?;
            self.start(pool, &identity).await?;
        }
        Ok(())
    }

    /// Refuses another runner when the resource pool has no room for its limits.
    async fn admit(&self, pool: &Pool) -> anyhow::Result<()> {
        let containers = if pool.docker_mode == DockerMode::Dind { 2 } else { 1 };
        let request = Request {
            service: format!("runner {}", pool.name),
            replicas: containers,
            cpus: pool.cpus.unwrap_or(0.0),
            memory_bytes: pool.memory_mb.unwrap_or(0).max(0) as u64 * 1024 * 1024,
        };
        let limits = self.runner.pool().await?;
        let running = crate::deploy::runner::counted(&self.runner.running_usage().await?);
        let storage = self.runner.storage_used().await?;
        admission::admit(limits, &running, storage, &format!("runner:{}", pool.id), &[request]).map_err(anyhow::Error::msg)
    }

    async fn pull(&self, image: &str) -> anyhow::Result<()> {
        if self.docker.inspect_image(image).await.is_ok() {
            return Ok(());
        }
        let mut pull = self.docker.create_image(Some(CreateImageOptionsBuilder::new().from_image(image).build()), None, None);
        while let Some(step) = pull.next().await {
            step.with_context(|| format!("pulling {image}"))?;
        }
        Ok(())
    }

    fn limits(pool: &Pool, host: HostConfig) -> HostConfig {
        HostConfig {
            nano_cpus: pool.cpus.map(|c| (c * 1e9) as i64),
            memory: pool.memory_mb.map(|m| m * 1024 * 1024),
            ..host
        }
    }

    async fn start(&self, pool: &Pool, identity: &identity::Identity) -> anyhow::Result<()> {
        let mut bytes = [0u8; 3];
        rand::rng().fill_bytes(&mut bytes);
        let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let (name, sidecar, network) = names(pool, &suffix);
        let image = pool.image.clone().unwrap_or_else(|| default_image(pool.provider).to_owned());
        self.pull(&image).await?;
        let mut labels = HashMap::from([
            (LABEL_MANAGED.to_owned(), "true".to_owned()),
            (LABEL_POOL.to_owned(), pool.id.clone()),
            (LABEL_INSTANCE.to_owned(), name.clone()),
        ]);
        if let Some(mbit) = pool.net_in_mbit {
            labels.insert(LABEL_NET_IN.to_owned(), mbit.to_string());
        }
        if let Some(mbit) = pool.net_out_mbit {
            labels.insert(LABEL_NET_OUT.to_owned(), mbit.to_string());
        }
        let started = self.start_inner(pool, identity, &name, &sidecar, &network, &image, &mut labels).await;
        if started.is_err() {
            self.remove_instance(&name).await;
        }
        started
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_inner(
        &self,
        pool: &Pool,
        identity: &identity::Identity,
        name: &str,
        sidecar: &str,
        network: &str,
        image: &str,
        labels: &mut HashMap<String, String>,
    ) -> anyhow::Result<()> {
        let mut env = Vec::new();
        let mut binds = Vec::new();
        let mut network_mode = None;
        match pool.docker_mode {
            DockerMode::Dind => {
                self.pull(DIND_IMAGE).await?;
                self.docker
                    .create_network(NetworkCreateRequest { name: network.into(), labels: Some(labels.clone()), ..Default::default() })
                    .await
                    .context("creating the runner's network")?;
                let slot = self.free_slot(pool).await?;
                let cache = cache_volume(&pool.id, slot);
                if self.docker.inspect_volume(&cache).await.is_err() {
                    let volume_labels = HashMap::from([(LABEL_MANAGED.to_owned(), "true".to_owned()), (LABEL_POOL.to_owned(), pool.id.clone())]);
                    self.docker
                        .create_volume(VolumeCreateRequest { name: Some(cache.clone()), labels: Some(volume_labels), ..Default::default() })
                        .await
                        .context("creating the runner's build cache")?;
                }
                let mut sidecar_labels = labels.clone();
                sidecar_labels.insert(LABEL_ROLE.into(), "docker".into());
                sidecar_labels.insert(LABEL_SLOT.into(), slot.to_string());
                let body = ContainerCreateBody {
                    image: Some(DIND_IMAGE.into()),
                    env: Some(vec!["DOCKER_TLS_CERTDIR=".into()]),
                    labels: Some(sidecar_labels),
                    host_config: Some(Self::limits(
                        pool,
                        HostConfig {
                            privileged: Some(true),
                            network_mode: Some(network.into()),
                            binds: Some(vec![format!("{cache}:/var/lib/docker")]),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                };
                self.docker.create_container(Some(CreateContainerOptionsBuilder::new().name(sidecar).build()), body).await?;
                self.docker.start_container(sidecar, None).await.context("starting the runner's Docker")?;
                env.push(format!("DOCKER_HOST=tcp://{sidecar}:2375"));
                network_mode = Some(network.to_owned());
            }
            DockerMode::Host => binds.push("/var/run/docker.sock:/var/run/docker.sock".to_owned()),
            DockerMode::None => {}
        }
        let registration = providers::register(&self.db, &self.http, pool, identity, name).await?;
        env.extend(registration.env);
        labels.insert(LABEL_ROLE.into(), "runner".into());
        if let Some(remote) = &registration.remote_id {
            labels.insert(LABEL_REMOTE.into(), remote.clone());
        }
        let restart = if pool.provider == Provider::Github { RestartPolicyNameEnum::NO } else { RestartPolicyNameEnum::UNLESS_STOPPED };
        let body = ContainerCreateBody {
            image: Some(image.into()),
            entrypoint: registration.entrypoint,
            cmd: registration.cmd,
            env: Some(env),
            labels: Some(labels.clone()),
            host_config: Some(Self::limits(
                pool,
                HostConfig {
                    binds: Some(binds),
                    network_mode,
                    restart_policy: Some(RestartPolicy { name: Some(restart), maximum_retry_count: None }),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        self.docker.create_container(Some(CreateContainerOptionsBuilder::new().name(name).build()), body).await?;
        self.docker.start_container(name, None).await.context("starting the runner")?;
        identity::touch(&self.db, &identity.id).await?;
        Ok(())
    }

    /// The lowest cache slot no running Docker sidecar of the pool holds.
    async fn free_slot(&self, pool: &Pool) -> anyhow::Result<usize> {
        let taken: HashSet<usize> = self
            .containers(Some(&pool.id))
            .await?
            .iter()
            .filter(|c| c.labels.get(LABEL_ROLE).map(String::as_str) == Some("docker"))
            .filter_map(|c| c.labels.get(LABEL_SLOT)?.parse().ok())
            .collect();
        Ok(first_free(&taken))
    }

    /// Stops the pool's runners and removes their build caches. The supervisor starts fresh runners afterwards.
    pub async fn clear_caches(&self, pool: &Pool) -> anyhow::Result<usize> {
        self.stop_all(pool).await?;
        let filters = HashMap::from([("label", vec![format!("{LABEL_POOL}={}", pool.id)])]);
        let options = bollard::query_parameters::ListVolumesOptionsBuilder::new().filters(&filters).build();
        let volumes = self.docker.list_volumes(Some(options)).await?.volumes.unwrap_or_default();
        let mut removed = 0;
        for volume in volumes {
            self.docker.remove_volume(&volume.name, None::<bollard::query_parameters::RemoveVolumeOptions>).await?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Removes a runner's container, its Docker sidecar and their network. The sidecar's cache volume stays.
    async fn remove_instance(&self, name: &str) {
        let remove = RemoveContainerOptionsBuilder::new().force(true).v(true).build();
        let _ = self.docker.remove_container(name, Some(remove.clone())).await;
        let _ = self.docker.remove_container(&format!("{name}_docker"), Some(remove)).await;
        let _ = self.docker.remove_network(&format!("{name}_net")).await;
    }

    /// Stops every runner of a pool. Returns the ones the provider still lists, with why.
    pub async fn stop_all(&self, pool: &Pool) -> anyhow::Result<Vec<String>> {
        let identity = identity::get(&self.db, &pool.identity).await?;
        let mut leftovers = Vec::new();
        for instance in self.instances(pool).await? {
            self.remove_instance(&instance.name).await;
            match (&identity, &instance.remote_id, pool.provider) {
                (Some(identity), Some(remote), _) => {
                    if let Err(err) = providers::deregister(&self.db, &self.http, pool, identity, remote).await {
                        leftovers.push(format!("{}: {err:#}", instance.name));
                    }
                }
                (_, _, Provider::Gitea) => leftovers.push(format!("{}: remove it under Actions, Runners", instance.name)),
                (None, Some(_), _) => leftovers.push(format!("{}: the identity is gone, remove it by hand", instance.name)),
                _ => {}
            }
        }
        Ok(leftovers)
    }

    /// The last lines a runner wrote, for one that belongs to `pool`.
    pub async fn logs(&self, pool: &Pool, instance: &str) -> anyhow::Result<String> {
        let found = self
            .containers(Some(&pool.id))
            .await?
            .into_iter()
            .find(|c| c.labels.get(LABEL_INSTANCE).map(String::as_str) == Some(instance) && c.labels.get(LABEL_ROLE).map(String::as_str) == Some("runner"));
        let Some(container) = found else { bail!("no runner {instance} in this pool") };
        let options = LogsOptionsBuilder::new().stdout(true).stderr(true).tail("300").build();
        let mut stream = self.docker.logs(&container.id, Some(options));
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk?.to_string());
        }
        Ok(text)
    }
}

struct Labelled {
    name: String,
    id: String,
    state: String,
    status: String,
    created: i64,
    labels: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(provider: Provider, scope_kind: ScopeKind, scope: &str) -> PoolInput {
        PoolInput {
            name: "ci".into(),
            provider,
            identity: "i1".into(),
            scope_kind,
            scope: scope.into(),
            labels: vec!["big".into()],
            min_idle: 1,
            max: 2,
            cpus: Some(2.0),
            memory_mb: Some(4096),
            net_in_mbit: Some(100.0),
            net_out_mbit: None,
            docker_mode: DockerMode::Dind,
            image: None,
            enabled: true,
        }
    }

    #[test]
    fn pool_checks() {
        let ok = input(Provider::Github, ScopeKind::Repo, "acme/api");
        assert!(ok.check(Kind::GithubApp).is_ok());
        assert!(ok.check(Kind::Gitlab).is_err(), "identity for another provider");
        assert!(input(Provider::Github, ScopeKind::Instance, "").check(Kind::GithubToken).is_err());
        assert!(input(Provider::Github, ScopeKind::Repo, "api").check(Kind::GithubToken).is_err(), "repo needs owner/name");
        assert!(input(Provider::Gitea, ScopeKind::Instance, "").normalize().check(Kind::Gitea).is_ok());
        assert!(input(Provider::Gitlab, ScopeKind::Org, "").check(Kind::Gitlab).is_err());
        assert!(PoolInput { max: 0, min_idle: 0, ..ok.clone() }.check(Kind::GithubApp).is_err());
        assert!(PoolInput { min_idle: 3, ..ok.clone() }.check(Kind::GithubApp).is_err());
        assert!(PoolInput { labels: vec!["has space".into()], ..ok.clone() }.check(Kind::GithubApp).is_err());
        assert!(PoolInput { memory_mb: Some(64), ..ok.clone() }.check(Kind::GithubApp).is_err());
        assert!(PoolInput { net_out_mbit: Some(0.0), ..ok.clone() }.check(Kind::GithubApp).is_err());
        assert!(PoolInput { name: "Bad Name".into(), ..ok }.check(Kind::GithubApp).is_err());
    }

    #[test]
    fn github_keeps_idle_runners_others_keep_max() {
        let mut pool = providers::tests::pool(Provider::Github, ScopeKind::Repo, DockerMode::Dind);
        pool.min_idle = 2;
        pool.max = 3;
        assert_eq!(wanted(&pool, 0, 0), 2);
        assert_eq!(wanted(&pool, 2, 1), 1, "one busy, one idle: start one more");
        assert_eq!(wanted(&pool, 3, 0), 0, "at the max even when all are busy");
        assert_eq!(wanted(&pool, 2, 2), 0);
        pool.provider = Provider::Gitlab;
        assert_eq!(wanted(&pool, 1, 1), 2);
        assert_eq!(wanted(&pool, 4, 0), 0);
        assert_eq!(names(&pool, "ab12cd").1, "kuroe_runner_ci_ab12cd_docker");
    }

    #[test]
    fn runners_take_the_lowest_free_cache_slot() {
        assert_eq!(first_free(&HashSet::new()), 0);
        assert_eq!(first_free(&HashSet::from([0, 1, 3])), 2);
        assert_eq!(cache_volume("p1", 2), "kuroe_runnercache_p1_2");
    }

    #[tokio::test]
    async fn pools_round_trip_and_keep_their_scope() {
        let db = crate::db::test_pool().await;
        let owner = crate::auth::create_user(&db, "boss", "boss-password-1", crate::auth::Role::owner()).await.unwrap();
        let meta = serde_json::json!({});
        let new = identity::NewIdentity { id: None, kind: Kind::GithubToken, name: "gh", host: "github.com", username: "", secret: "ghp_x", meta: &meta, expires_at: None };
        let identity = identity::insert(&db, &new).await.unwrap();
        let pool = insert(&db, PoolInput { identity: identity.clone(), ..input(Provider::Github, ScopeKind::Repo, "/acme/api/") }, Kind::GithubToken, &owner.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pool.scope, "acme/api");
        assert_eq!(pool.labels, ["big"]);
        assert_eq!((pool.net_in_mbit, pool.net_out_mbit), (Some(100.0), None));
        let changed = PoolInput { max: 5, scope: "other/repo".into(), net_out_mbit: Some(20.0), ..input(Provider::Gitlab, ScopeKind::Org, "x") };
        update(&db, &pool, changed, Kind::GithubToken).await.unwrap().unwrap();
        let saved = get(&db, &pool.id).await.unwrap().unwrap();
        assert_eq!((saved.max, saved.scope.as_str(), saved.provider), (5, "acme/api", Provider::Github), "scope and provider stay");
        assert_eq!((saved.net_in_mbit, saved.net_out_mbit), (Some(100.0), Some(20.0)));
        assert_eq!(uses_identity(&db, &identity).await.unwrap(), ["ci"]);
        let bad = insert(&db, PoolInput { name: "two".into(), ..input(Provider::Github, ScopeKind::Repo, "api") }, Kind::GithubToken, &owner.id).await.unwrap();
        assert!(bad.is_err());
        delete(&db, &pool.id).await.unwrap();
        assert!(list(&db).await.unwrap().is_empty());
    }
}
