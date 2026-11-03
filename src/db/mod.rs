use std::path::Path;

use anyhow::{Context, bail};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub async fn connect(data_dir: &Path) -> anyhow::Result<SqlitePool> {
    std::fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let options = SqliteConnectOptions::new()
        .filename(data_dir.join("kuroe.db"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new().max_connections(8).connect_with(options).await?;
    Ok(pool)
}

async fn applied_versions(pool: &SqlitePool) -> anyhow::Result<Vec<i64>> {
    let has_table: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations')")
            .fetch_one(pool)
            .await?;
    if !has_table {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = 1").fetch_all(pool).await?)
}

fn known_versions() -> Vec<i64> {
    MIGRATOR.iter().filter(|m| !m.migration_type.is_down_migration()).map(|m| m.version).collect()
}

/// A database a newer kuroe migrated can't be used by this one. Starting anyway could break it.
fn refuse_newer(applied: &[i64]) -> anyhow::Result<()> {
    let known = known_versions();
    if let Some(newer) = applied.iter().filter(|v| !known.contains(v)).max() {
        bail!(
            "the database was migrated by a newer kuroe (migration {newer}), this one only knows up to {}. \
             Run the newer version or restore a backup of the data folder.",
            known.iter().max().copied().unwrap_or(0)
        );
    }
    Ok(())
}

pub async fn require_migrated(pool: &SqlitePool) -> anyhow::Result<()> {
    let applied = applied_versions(pool).await?;
    refuse_newer(&applied)?;
    if known_versions().iter().any(|v| !applied.contains(v)) {
        bail!("database has pending migrations, run `kuroe migrate` first");
    }
    Ok(())
}

/// Applies pending migrations and returns how many there were.
pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<usize> {
    let applied = applied_versions(pool).await?;
    refuse_newer(&applied)?;
    let pending = known_versions().iter().filter(|v| !applied.contains(v)).count();
    if pending > 0 {
        MIGRATOR.run(pool).await.context("running migrations")?;
    }
    Ok(pending)
}

pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
pub async fn test_pool() -> SqlitePool {
    let options = SqliteConnectOptions::new().in_memory(true).foreign_keys(true);
    let pool = SqlitePoolOptions::new().max_connections(1).connect_with(options).await.unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    pool
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn table_names(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '\\_%' ESCAPE '\\' AND name NOT LIKE 'sqlite%' ORDER BY name")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    const TABLES_ADDED: [&[&str]; 3] = [
        &[
        "api_tokens",
        "container_metrics",
        "deploy_tokens",
        "deployment_grants",
        "deployments",
        "domains",
        "host_metrics",
        "identities",
        "log_cursors",
        "metric_series",
        "roles",
        "runner_pools",
        "saved_secrets",
        "sessions",
        "settings",
        "storage_metrics",
        "users",
        "volumes",
        ],
        &["volume_limits", "volume_sizes"],
        &["deploys"],
    ];

    fn tables_at(version: usize) -> Vec<String> {
        let mut tables: Vec<String> = TABLES_ADDED[..version].iter().flat_map(|t| t.iter().map(|s| s.to_string())).collect();
        tables.sort();
        tables
    }

    #[tokio::test]
    async fn migrations_up_and_down() {
        let pool = test_pool().await;
        let latest = MIGRATOR.iter().filter(|m| !m.migration_type.is_down_migration()).count();
        assert_eq!(latest, TABLES_ADDED.len(), "add the new migration's tables to TABLES_ADDED");
        require_migrated(&pool).await.unwrap();
        assert_eq!(table_names(&pool).await, tables_at(latest));

        for version in (0..latest).rev() {
            MIGRATOR.undo(&pool, version as i64).await.unwrap();
            assert_eq!(table_names(&pool).await, tables_at(version), "after undoing to {version}");
        }
        assert!(require_migrated(&pool).await.is_err());

        MIGRATOR.run(&pool).await.unwrap();
        assert_eq!(table_names(&pool).await, tables_at(latest));
    }

    #[tokio::test]
    async fn migrate_applies_what_is_pending_and_refuses_a_newer_database() {
        let pool = test_pool().await;
        let latest = known_versions().len();
        assert_eq!(migrate(&pool).await.unwrap(), 0, "nothing left to do");
        MIGRATOR.undo(&pool, 0).await.unwrap();
        assert!(require_migrated(&pool).await.is_err());
        assert_eq!(migrate(&pool).await.unwrap(), latest);
        require_migrated(&pool).await.unwrap();

        sqlx::query("INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (9999, 'future', 1, x'00', 0)")
            .execute(&pool)
            .await
            .unwrap();
        let err = migrate(&pool).await.unwrap_err().to_string();
        assert!(err.contains("newer kuroe (migration 9999)"), "{err}");
        assert!(require_migrated(&pool).await.unwrap_err().to_string().contains("newer kuroe"));
    }
}
