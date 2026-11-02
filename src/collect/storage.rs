use std::path::Path;

use bollard::models::SystemDataUsageResponse;
use serde::Serialize;
use sqlx::SqlitePool;

pub const DB_FILES: [&str; 3] = ["kuroe.db", "kuroe.db-wal", "kuroe.db-shm"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub category: &'static str,
    pub bytes: u64,
}

pub fn docker_usage(df: &SystemDataUsageResponse) -> Vec<Usage> {
    let size = |v: Option<i64>| v.unwrap_or(0).max(0) as u64;
    vec![
        Usage { category: "docker_images", bytes: size(df.image_usage.as_ref().and_then(|u| u.total_size)) },
        Usage { category: "docker_containers", bytes: size(df.container_usage.as_ref().and_then(|u| u.total_size)) },
        Usage { category: "docker_volumes", bytes: size(df.volume_usage.as_ref().and_then(|u| u.total_size)) },
        Usage { category: "docker_build_cache", bytes: size(df.build_cache_usage.as_ref().and_then(|u| u.total_size)) },
    ]
}

pub fn dir_size(path: &Path) -> std::io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Ok(meta.len());
    }
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        total += if kind.is_dir() { dir_size(&entry.path())? } else if kind.is_file() { entry.metadata()?.len() } else { 0 };
    }
    Ok(total)
}

pub fn kuroe_usage(data_dir: &Path) -> std::io::Result<Vec<Usage>> {
    let db: u64 = DB_FILES.iter().map(|f| std::fs::metadata(data_dir.join(f)).map(|m| m.len()).unwrap_or(0)).sum();
    let logs = dir_size(&data_dir.join("logs"))?;
    let all = dir_size(data_dir)?;
    Ok(vec![
        Usage { category: "kuroe_db", bytes: db },
        Usage { category: "kuroe_logs", bytes: logs },
        Usage { category: "kuroe_other", bytes: all.saturating_sub(db + logs) },
    ])
}

pub fn total(usages: &[Usage]) -> u64 {
    usages.iter().map(|u| u.bytes).sum()
}

pub async fn record(db: &SqlitePool, ts: i64, usages: &[Usage]) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    for usage in usages {
        sqlx::query("INSERT OR REPLACE INTO storage_metrics (ts, category, bytes) VALUES (?, ?, ?)")
            .bind(ts)
            .bind(usage.category)
            .bind(usage.bytes as i64)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_categories_come_from_df_totals() {
        let df: SystemDataUsageResponse = serde_json::from_value(serde_json::json!({
            "ImageUsage": { "TotalSize": 1_500_000_000i64, "TotalCount": 12 },
            "ContainerUsage": { "TotalSize": 20_000_000i64 },
            "VolumeUsage": { "TotalSize": 300_000_000i64 },
            "BuildCacheUsage": { "TotalSize": -1i64 }
        }))
        .unwrap();
        let usage = docker_usage(&df);
        assert_eq!(usage.iter().map(|u| u.bytes).collect::<Vec<_>>(), [1_500_000_000, 20_000_000, 300_000_000, 0]);
        assert_eq!(total(&usage), 1_820_000_000);
    }

    #[test]
    fn kuroe_categories_add_up_to_the_data_dir() {
        let dir = std::env::temp_dir().join(format!("kuroe-storage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("logs/dep/web")).unwrap();
        std::fs::create_dir_all(dir.join("deployments/dep")).unwrap();
        std::fs::write(dir.join("kuroe.db"), vec![0u8; 4096]).unwrap();
        std::fs::write(dir.join("kuroe.db-wal"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join("logs/dep/web/2026-10-31.log"), vec![b'x'; 700]).unwrap();
        std::fs::write(dir.join("deployments/dep/context.tar"), vec![0u8; 300]).unwrap();

        let usage = kuroe_usage(&dir).unwrap();
        assert_eq!(
            usage,
            [
                Usage { category: "kuroe_db", bytes: 5096 },
                Usage { category: "kuroe_logs", bytes: 700 },
                Usage { category: "kuroe_other", bytes: 300 },
            ]
        );
        assert_eq!(total(&usage), dir_size(&dir).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
