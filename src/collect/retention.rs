use std::path::Path;

use sqlx::SqlitePool;
use time::{Date, OffsetDateTime, format_description::FormatItem, macros::format_description};

pub const DAY: i64 = 86_400;
pub const DATE_FORMAT: &[FormatItem<'static>] = format_description!("[year]-[month]-[day]");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub metrics_days: i64,
    pub logs_days: i64,
    /// Finished deploys kept per deployment.
    pub deploys: i64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub rows: u64,
    pub files: u64,
}

pub async fn load(db: &SqlitePool) -> anyhow::Result<Retention> {
    async fn count(db: &SqlitePool, key: &str, default: i64, unit: &str) -> anyhow::Result<i64> {
        let value: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(db).await?;
        match value.filter(|v| !v.is_empty()) {
            None => Ok(default),
            Some(v) => match v.parse::<i64>() {
                Ok(n) if n >= 1 => Ok(n),
                _ => anyhow::bail!("setting {key} must be a whole number of {unit}, got {v}"),
            },
        }
    }
    Ok(Retention {
        metrics_days: count(db, "retention_metrics_days", 30, "days").await?,
        logs_days: count(db, "retention_logs_days", 30, "days").await?,
        deploys: count(db, "retention_deploys", crate::deploy::history::DEFAULT_KEEP, "deploys").await?,
    })
}

pub async fn prune_rows(db: &SqlitePool, now: i64, retention: Retention) -> sqlx::Result<u64> {
    let cutoff = now - retention.metrics_days * DAY;
    let mut deleted = 0;
    for sql in [
        "DELETE FROM container_metrics WHERE series_id IN (SELECT id FROM metric_series) AND ts < ?",
        "DELETE FROM host_metrics WHERE ts < ?",
        "DELETE FROM storage_metrics WHERE ts < ?",
    ] {
        deleted += sqlx::query(sql).bind(cutoff).execute(db).await?.rows_affected();
    }
    sqlx::query("DELETE FROM metric_series WHERE NOT EXISTS (SELECT 1 FROM container_metrics m WHERE m.series_id = metric_series.id)")
        .execute(db)
        .await?;
    Ok(deleted)
}

/// A day file is removed only once every line it can hold is older than the cutoff.
pub fn file_expired(date: Date, now: i64, logs_days: i64) -> bool {
    let day_end = date.next_day().map(|d| d.midnight().assume_utc().unix_timestamp()).unwrap_or(i64::MAX);
    day_end <= now - logs_days * DAY
}

pub fn prune_files(logs_dir: &Path, now: i64, logs_days: i64) -> std::io::Result<u64> {
    if !logs_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for deployment in std::fs::read_dir(logs_dir)? {
        let deployment = deployment?.path();
        if !deployment.is_dir() {
            continue;
        }
        for service in std::fs::read_dir(&deployment)? {
            let service = service?.path();
            if !service.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(&service)? {
                let path = file?.path();
                let date = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| Date::parse(s, DATE_FORMAT).ok());
                if path.extension().is_some_and(|e| e == "log") && date.is_some_and(|d| file_expired(d, now, logs_days)) {
                    std::fs::remove_file(&path)?;
                    removed += 1;
                }
            }
            if std::fs::read_dir(&service)?.next().is_none() {
                std::fs::remove_dir(&service)?;
            }
        }
        if std::fs::read_dir(&deployment)?.next().is_none() {
            std::fs::remove_dir(&deployment)?;
        }
    }
    Ok(removed)
}

pub async fn prune(db: &SqlitePool, logs_dir: &Path, now: i64) -> anyhow::Result<Pruned> {
    let retention = load(db).await?;
    let rows = prune_rows(db, now, retention).await? + crate::deploy::history::prune(db, retention.deploys).await?;
    let dir = logs_dir.to_owned();
    let files = tokio::task::spawn_blocking(move || prune_files(&dir, now, retention.logs_days)).await??;
    Ok(Pruned { rows, files })
}

pub fn today(now: i64) -> String {
    OffsetDateTime::from_unix_timestamp(now).map(|t| t.date()).unwrap_or(Date::MIN).format(DATE_FORMAT).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;

    const HOUR: i64 = 3_600;
    const NOW: i64 = 1_793_448_000;
    const RETAIN: Retention = Retention { metrics_days: 30, logs_days: 30, deploys: 50 };

    async fn insert_at(db: &SqlitePool, ts: i64) {
        let series = crate::collect::stats::series_id(db, "d", "s", "c").await.unwrap();
        sqlx::query("INSERT INTO container_metrics VALUES (?, ?, 0, 0, 0, 0, 0, 0, 0)")
            .bind(series)
            .bind(ts)
            .execute(db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO host_metrics VALUES (?, 0, 0, 0, 0, 0, 0)").bind(ts).execute(db).await.unwrap();
        sqlx::query("INSERT INTO storage_metrics VALUES (?, 'logs', 0)").bind(ts).execute(db).await.unwrap();
    }

    #[tokio::test]
    async fn prune_boundary_keeps_29d23h_and_drops_30d1h() {
        let db = test_pool().await;
        let kept = NOW - (29 * DAY + 23 * HOUR);
        let gone = NOW - (30 * DAY + HOUR);
        insert_at(&db, kept).await;
        insert_at(&db, gone).await;

        assert_eq!(prune_rows(&db, NOW, RETAIN).await.unwrap(), 3);
        let container: Vec<i64> = sqlx::query_scalar("SELECT ts FROM container_metrics").fetch_all(&db).await.unwrap();
        let host: Vec<i64> = sqlx::query_scalar("SELECT ts FROM host_metrics").fetch_all(&db).await.unwrap();
        let storage: Vec<i64> = sqlx::query_scalar("SELECT ts FROM storage_metrics").fetch_all(&db).await.unwrap();
        assert_eq!((container, host, storage), (vec![kept], vec![kept], vec![kept]));
    }

    #[tokio::test]
    async fn series_without_rows_are_dropped() {
        let db = test_pool().await;
        let old = crate::collect::stats::series_id(&db, "d", "s", "old-container").await.unwrap();
        sqlx::query("INSERT INTO container_metrics VALUES (?, ?, 0, 0, 0, 0, 0, 0, 0)")
            .bind(old)
            .bind(NOW - 40 * DAY)
            .execute(&db)
            .await
            .unwrap();
        insert_at(&db, NOW).await;
        prune_rows(&db, NOW, RETAIN).await.unwrap();
        let left: Vec<String> = sqlx::query_scalar("SELECT container_id FROM metric_series").fetch_all(&db).await.unwrap();
        assert_eq!(left, ["c"]);
    }

    #[test]
    fn log_file_kept_while_any_hour_is_in_window() {
        let at = |ts: i64| OffsetDateTime::from_unix_timestamp(ts).unwrap().date();
        assert!(!file_expired(at(NOW - (29 * DAY + 23 * HOUR)), NOW, 30));
        assert!(file_expired(at(NOW - (31 * DAY + HOUR)), NOW, 30));
        let boundary_day = at(NOW - 30 * DAY);
        assert!(!file_expired(boundary_day, NOW, 30), "the day containing the cutoff still has lines inside the window");
    }

    #[test]
    fn prune_files_removes_exactly_expired_days() {
        let dir = std::env::temp_dir().join(format!("kuroe-prune-{}", uuid::Uuid::new_v4()));
        let service = dir.join("dep1").join("web");
        std::fs::create_dir_all(&service).unwrap();
        let day = |ago: i64| today(NOW - ago);
        let keep = [day(0), day(29 * DAY), day(30 * DAY)];
        let drop = [day(31 * DAY + HOUR), day(45 * DAY)];
        for name in keep.iter().chain(&drop) {
            std::fs::write(service.join(format!("{name}.log")), "x\n").unwrap();
        }
        std::fs::write(service.join("notes.txt"), "kept").unwrap();

        assert_eq!(prune_files(&dir, NOW, 30).unwrap(), 2);
        let mut left: Vec<String> = std::fs::read_dir(&service).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        let mut expected: Vec<String> = keep.iter().map(|d| format!("{d}.log")).chain(["notes.txt".to_owned()]).collect();
        expected.sort();
        assert_eq!(left, expected);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_service_dirs_are_cleaned_up() {
        let dir = std::env::temp_dir().join(format!("kuroe-prune-{}", uuid::Uuid::new_v4()));
        let service = dir.join("gone").join("web");
        std::fs::create_dir_all(&service).unwrap();
        std::fs::write(service.join(format!("{}.log", today(NOW - 60 * DAY))), "x\n").unwrap();
        assert_eq!(prune_files(&dir, NOW, 30).unwrap(), 1);
        assert!(!dir.join("gone").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
