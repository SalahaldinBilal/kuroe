//! Volume sizes and soft limits. Docker can't cap a volume, so kuroe compares the measured size with the limit.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::deploy::spec::project_name;
use crate::deploy::{self, Deployment};
use crate::deploy::runner::Runner;

const CHECK_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limit {
    pub max_bytes: i64,
    pub stop: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct User {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VolumeInfo {
    /// The Docker volume name.
    pub name: String,
    /// The name on the Volumes page, for a shared volume.
    pub shared: Option<String>,
    /// Deployments that mount a shared volume, or the one a Compose volume belongs to.
    pub deployments: Vec<User>,
    pub bytes: Option<i64>,
    pub measured_at: Option<i64>,
    pub limit: Option<Limit>,
    pub over: bool,
}

pub struct Measured {
    pub name: String,
    pub project: String,
    pub bytes: i64,
    pub measured_at: i64,
}

fn owns(d: &Deployment, project: &str) -> bool {
    let base = project_name(&d.name);
    project == base || project.strip_prefix(&base).is_some_and(|color| color == "-blue" || color == "-green")
}

fn mounts(d: &Deployment, shared: &str) -> bool {
    d.spec.services.values().any(|s| s.volumes.iter().any(|m| m.volume == shared))
}

fn user(d: &Deployment) -> User {
    User { id: d.id.clone(), name: d.name.clone() }
}

/// Shared volumes first, then each deployment's own. A Compose volume whose deployment is gone is left out.
pub fn build(shared: &[String], measured: &[Measured], limits: &BTreeMap<String, Limit>, deployments: &[Deployment]) -> Vec<VolumeInfo> {
    let sizes: BTreeMap<&str, &Measured> = measured.iter().map(|m| (m.name.as_str(), m)).collect();
    let info = |name: String, shared: Option<String>, users: Vec<User>| {
        let size = sizes.get(name.as_str());
        let limit = limits.get(&name).cloned();
        let over = matches!((&limit, size), (Some(l), Some(s)) if s.bytes > l.max_bytes);
        VolumeInfo { bytes: size.map(|s| s.bytes), measured_at: size.map(|s| s.measured_at), limit, over, name, shared, deployments: users }
    };
    let mut out: Vec<VolumeInfo> = shared
        .iter()
        .map(|s| info(format!("kuroe_{s}"), Some(s.clone()), deployments.iter().filter(|d| mounts(d, s)).map(user).collect()))
        .collect();
    for m in measured.iter().filter(|m| !m.project.is_empty()) {
        let users: Vec<User> = deployments.iter().filter(|d| owns(d, &m.project)).map(user).collect();
        if !users.is_empty() {
            out.push(info(m.name.clone(), None, users));
        }
    }
    out
}

pub async fn all(db: &SqlitePool) -> anyhow::Result<Vec<VolumeInfo>> {
    let shared: Vec<String> = sqlx::query_scalar("SELECT name FROM volumes ORDER BY name").fetch_all(db).await?;
    let measured: Vec<Measured> = sqlx::query_as::<_, (String, String, i64, i64)>("SELECT name, project, bytes, measured_at FROM volume_sizes ORDER BY name")
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|(name, project, bytes, measured_at)| Measured { name, project, bytes, measured_at })
        .collect();
    let limits: BTreeMap<String, Limit> = sqlx::query_as::<_, (String, i64, bool)>("SELECT name, max_bytes, stop FROM volume_limits")
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|(name, max_bytes, stop)| (name, Limit { max_bytes, stop }))
        .collect();
    Ok(build(&shared, &measured, &limits, &deploy::list(db).await?))
}

pub async fn set_limit(db: &SqlitePool, name: &str, limit: &Limit, by: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO volume_limits (name, max_bytes, stop, set_by, set_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (name) DO UPDATE SET max_bytes = excluded.max_bytes, stop = excluded.stop, set_by = excluded.set_by, set_at = excluded.set_at",
    )
    .bind(name)
    .bind(limit.max_bytes)
    .bind(limit.stop)
    .bind(by)
    .bind(crate::db::now())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn forget(db: &SqlitePool, name: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM volume_limits WHERE name = ?").bind(name).execute(db).await?;
    sqlx::query("DELETE FROM volume_sizes WHERE name = ?").bind(name).execute(db).await?;
    Ok(())
}

pub fn human(bytes: i64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", units[unit]) }
}

/// Stops the running deployments that use a volume over a limit that says to stop.
pub async fn enforce(db: &SqlitePool, runner: &Runner) -> anyhow::Result<()> {
    for v in all(db).await? {
        let Some(limit) = v.limit.as_ref().filter(|l| l.stop && v.over) else { continue };
        for u in &v.deployments {
            let Some(d) = deploy::get(db, &u.id).await? else { continue };
            if d.status != "running" {
                continue;
            }
            let shown = v.shared.as_deref().unwrap_or(&v.name);
            let message = format!(
                "stopped by kuroe: the volume {shown} holds {} and its limit is {}. Free up space or raise the limit, then deploy again.",
                human(v.bytes.unwrap_or_default()),
                human(limit.max_bytes)
            );
            tracing::warn!("{} {message}", d.name);
            let output = runner.stop(&d).await.unwrap_or_else(|err| format!("stopping failed: {err:#}"));
            deploy::finish(db, &d.id, "stopped", format!("{message}\n{output}").trim_end()).await?;
        }
    }
    Ok(())
}

pub fn spawn_enforcer(db: SqlitePool, runner: Runner) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(CHECK_EVERY);
        loop {
            ticker.tick().await;
            if let Err(err) = enforce(&db, &runner).await {
                tracing::warn!("checking volume limits: {err:#}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::{Source, Spec};
    use crate::docker::rewrite::{ServiceSettings, SharedMount};

    fn deployment(id: &str, name: &str, mounts: &[&str]) -> Deployment {
        let mut spec = Spec::new(Source::image("nginx"));
        let volumes = mounts.iter().map(|v| SharedMount { volume: (*v).into(), target: "/data".into(), read_only: false }).collect();
        spec.services.insert("app".into(), ServiceSettings { volumes, ..Default::default() });
        Deployment {
            id: id.into(),
            name: name.into(),
            spec,
            status: "running".into(),
            revision: 1,
            last_output: String::new(),
            active_project: None,
            created_by: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn measured(name: &str, project: &str, bytes: i64) -> Measured {
        Measured { name: name.into(), project: project.into(), bytes, measured_at: 100 }
    }

    #[test]
    fn volumes_know_who_uses_them_and_when_they_are_over() {
        let deployments = [deployment("d1", "shop", &["uploads"]), deployment("d2", "blog", &["uploads"]), deployment("d3", "api", &[])];
        let measured = [
            measured("kuroe_uploads", "", 3000),
            measured("kuroe_shop_db", "kuroe_shop", 500),
            measured("kuroe_api-green_cache", "kuroe_api-green", 10),
            measured("kuroe_gone_db", "kuroe_gone", 10),
        ];
        let limits = BTreeMap::from([("kuroe_uploads".to_owned(), Limit { max_bytes: 2000, stop: true }), ("kuroe_shop_db".to_owned(), Limit { max_bytes: 1000, stop: false })]);
        let shared = ["uploads".to_owned(), "empty".to_owned()];
        let out = build(&shared, &measured, &limits, &deployments);

        let names: Vec<&str> = out.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["kuroe_uploads", "kuroe_empty", "kuroe_shop_db", "kuroe_api-green_cache"], "a deleted deployment's volume is left out");
        let uploads = &out[0];
        assert_eq!(uploads.deployments.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(), ["shop", "blog"]);
        assert!(uploads.over);
        assert_eq!((out[1].bytes, out[1].over), (None, false), "not measured yet");
        assert!(!out[2].over, "under its limit");
        assert_eq!(out[3].deployments[0].name, "api", "the green copy of a blue/green deployment");
    }

    #[test]
    fn sizes_read_like_the_ui() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(3 << 30), "3.0 GiB");
    }
}
