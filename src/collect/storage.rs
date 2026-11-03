use std::collections::HashSet;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSize {
    pub name: String,
    pub project: String,
    pub bytes: u64,
}

const COMPOSE_PROJECT: &str = "com.docker.compose.project";
const STACK_NAMESPACE: &str = "com.docker.stack.namespace";

/// kuroe's volumes from a verbose `df`: the shared ones by name, and those of a kuroe Compose project or stack.
/// Docker reports -1 for a size it couldn't measure, and those are left out.
pub fn volume_sizes(df: &SystemDataUsageResponse, shared: &HashSet<String>) -> Vec<VolumeSize> {
    let items = df.volume_usage.as_ref().and_then(|u| u.items.as_ref()).map(Vec::as_slice).unwrap_or_default();
    items
        .iter()
        .filter_map(|v| {
            let name = v["Name"].as_str()?.to_owned();
            let labels = &v["Labels"];
            let project = labels[COMPOSE_PROJECT].as_str().or_else(|| labels[STACK_NAMESPACE].as_str()).unwrap_or_default();
            let ours = shared.contains(&name) || project.starts_with("kuroe_");
            let bytes = u64::try_from(v["UsageData"]["Size"].as_i64()?).ok()?;
            ours.then(|| VolumeSize { project: if shared.contains(&name) { String::new() } else { project.to_owned() }, name, bytes })
        })
        .collect()
}

pub async fn record_volumes(db: &SqlitePool, ts: i64, sizes: &[VolumeSize]) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM volume_sizes").execute(&mut *tx).await?;
    for size in sizes {
        sqlx::query("INSERT INTO volume_sizes (name, project, bytes, measured_at) VALUES (?, ?, ?, ?)")
            .bind(&size.name)
            .bind(&size.project)
            .bind(size.bytes as i64)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
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
    fn volume_sizes_keep_kuroes_volumes_only() {
        let df: SystemDataUsageResponse = serde_json::from_value(serde_json::json!({
            "VolumeUsage": { "TotalSize": 0, "Items": [
                { "Name": "kuroe_uploads", "Labels": null, "UsageData": { "Size": 2048, "RefCount": 1 } },
                { "Name": "kuroe_shop_db", "Labels": { "com.docker.compose.project": "kuroe_shop" }, "UsageData": { "Size": 9000, "RefCount": 1 } },
                { "Name": "kuroe_web_cache", "Labels": { "com.docker.stack.namespace": "kuroe_web" }, "UsageData": { "Size": 10, "RefCount": 0 } },
                { "Name": "kuroe_cargo_registry", "Labels": null, "UsageData": { "Size": 500, "RefCount": 0 } },
                { "Name": "other_db", "Labels": { "com.docker.compose.project": "other" }, "UsageData": { "Size": 7, "RefCount": 0 } },
                { "Name": "kuroe_remote", "Labels": { "com.docker.compose.project": "kuroe_shop" }, "UsageData": { "Size": -1, "RefCount": 0 } }
            ] }
        }))
        .unwrap();
        let shared = HashSet::from(["kuroe_uploads".to_owned()]);
        let sizes = volume_sizes(&df, &shared);
        let got: Vec<(&str, &str, u64)> = sizes.iter().map(|s| (s.name.as_str(), s.project.as_str(), s.bytes)).collect();
        assert_eq!(got, [("kuroe_uploads", "", 2048), ("kuroe_shop_db", "kuroe_shop", 9000), ("kuroe_web_cache", "kuroe_web", 10)]);
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
