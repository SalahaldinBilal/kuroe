pub mod host;
pub mod logs;
pub mod retention;
pub mod stats;
pub mod storage;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use bollard::Docker;
use bollard::query_parameters::{DataUsageOptionsBuilder, ListContainersOptionsBuilder, StatsOptionsBuilder};
use futures::StreamExt;
use sqlx::SqlitePool;
use tokio::task::JoinHandle;

use crate::db::now;
use crate::docker::rewrite::{LABEL_DEPLOYMENT, LABEL_SERVICE};

const STATS_EVERY: Duration = Duration::from_secs(10);
const HOST_EVERY: Duration = Duration::from_secs(30);
const STORAGE_EVERY: Duration = Duration::from_secs(600);
const PRUNE_EVERY: Duration = Duration::from_secs(3_600);
const RECONCILE_EVERY: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Collector {
    pub db: SqlitePool,
    pub docker: Docker,
    pub data_dir: PathBuf,
}

struct Tasks {
    stats: JoinHandle<()>,
    logs: JoinHandle<()>,
}

impl Collector {
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn spawn(self) {
        tokio::spawn(self.clone().reconcile_loop());
        tokio::spawn(self.clone().host_loop());
        tokio::spawn(self.clone().storage_loop());
        tokio::spawn(self.prune_loop());
    }

    async fn reconcile_loop(self) {
        let mut tasks: HashMap<String, Tasks> = HashMap::new();
        let mut ticker = tokio::time::interval(RECONCILE_EVERY);
        loop {
            ticker.tick().await;
            let filters = HashMap::from([("label", vec![LABEL_DEPLOYMENT.to_owned()]), ("status", vec!["running".to_owned()])]);
            let options = ListContainersOptionsBuilder::new().filters(&filters).build();
            let running = match self.docker.list_containers(Some(options)).await {
                Ok(list) => list,
                Err(err) => {
                    tracing::warn!("collector cannot list containers: {err}");
                    continue;
                }
            };
            tasks.retain(|_, t| !(t.stats.is_finished() && t.logs.is_finished()));
            for container in running {
                let (Some(id), Some(labels)) = (container.id, container.labels) else { continue };
                let deployment = labels.get(LABEL_DEPLOYMENT).cloned().unwrap_or_default();
                let service = labels.get(LABEL_SERVICE).cloned().unwrap_or_else(|| "app".into());
                // A restart ends the log stream while the stats task may keep going, so each is restarted on its own.
                match tasks.get_mut(&id) {
                    Some(t) => {
                        if t.stats.is_finished() {
                            t.stats = self.spawn_stats(&deployment, &service, &id);
                        }
                        if t.logs.is_finished() {
                            t.logs = self.spawn_logs(&deployment, &service, &id);
                        }
                    }
                    None => {
                        let fresh = Tasks { stats: self.spawn_stats(&deployment, &service, &id), logs: self.spawn_logs(&deployment, &service, &id) };
                        tasks.insert(id, fresh);
                    }
                }
            }
        }
    }

    fn spawn_stats(&self, deployment: &str, service: &str, id: &str) -> JoinHandle<()> {
        tokio::spawn(self.clone().stats_task(deployment.to_owned(), service.to_owned(), id.to_owned()))
    }

    fn spawn_logs(&self, deployment: &str, service: &str, id: &str) -> JoinHandle<()> {
        let (db, docker, dir) = (self.db.clone(), self.docker.clone(), self.logs_dir());
        let (deployment, service, id) = (deployment.to_owned(), service.to_owned(), id.to_owned());
        tokio::spawn(async move {
            if let Err(err) = logs::follow(db, docker, dir, deployment, service, id.clone()).await {
                tracing::warn!("log follower for {id} stopped: {err:#}");
            }
        })
    }

    async fn stats_task(self, deployment: String, service: String, id: String) {
        let series = match stats::series_id(&self.db, &deployment, &service, &id).await {
            Ok(series) => series,
            Err(err) => {
                tracing::warn!("registering metrics for {id}: {err}");
                return;
            }
        };
        let mut ticker = tokio::time::interval(STATS_EVERY);
        loop {
            ticker.tick().await;
            let options = StatsOptionsBuilder::new().stream(false).build();
            let Some(Ok(raw)) = self.docker.stats(&id, Some(options)).next().await else { return };
            if raw.read.is_none() || raw.memory_stats.as_ref().and_then(|m| m.usage).is_none() {
                return;
            }
            let sample = stats::parse(&raw, now(), &deployment, &service, &id);
            if let Err(err) = stats::insert(&self.db, series, &sample).await {
                tracing::warn!("saving stats for {id}: {err}");
            }
        }
    }

    async fn host_loop(self) {
        let mut probe = match host::HostProbe::new() {
            Ok(probe) => probe,
            Err(err) => {
                tracing::error!("host metrics are off, sysinfo failed: {err}");
                return;
            }
        };
        let mut ticker = tokio::time::interval(HOST_EVERY);
        loop {
            ticker.tick().await;
            let sample = probe.sample(now(), &self.data_dir);
            if let Err(err) = host::insert(&self.db, &sample).await {
                tracing::warn!("saving host metrics: {err}");
            }
        }
    }

    pub async fn record_storage(&self) -> anyhow::Result<Vec<storage::Usage>> {
        let df = self.docker.df(Some(DataUsageOptionsBuilder::new().verbose(true).build())).await?;
        let dir = self.data_dir.clone();
        let mut usage = tokio::task::spawn_blocking(move || storage::kuroe_usage(&dir)).await??;
        usage.extend(storage::docker_usage(&df));
        let ts = now();
        storage::record(&self.db, ts, &usage).await?;
        let shared: HashSet<String> = sqlx::query_scalar::<_, String>("SELECT name FROM volumes")
            .fetch_all(&self.db)
            .await?
            .into_iter()
            .map(|name| format!("kuroe_{name}"))
            .collect();
        storage::record_volumes(&self.db, ts, &storage::volume_sizes(&df, &shared)).await?;
        Ok(usage)
    }

    async fn storage_loop(self) {
        let mut ticker = tokio::time::interval(STORAGE_EVERY);
        loop {
            ticker.tick().await;
            if let Err(err) = self.record_storage().await {
                tracing::warn!("storage accounting: {err:#}");
            }
        }
    }

    async fn prune_loop(self) {
        let mut ticker = tokio::time::interval(PRUNE_EVERY);
        loop {
            ticker.tick().await;
            match retention::prune(&self.db, &self.logs_dir(), now()).await {
                Ok(pruned) if pruned.rows > 0 || pruned.files > 0 => {
                    tracing::info!("pruned {} metric rows and {} log files", pruned.rows, pruned.files)
                }
                Ok(_) => {}
                Err(err) => tracing::warn!("pruning: {err:#}"),
            }
        }
    }
}
