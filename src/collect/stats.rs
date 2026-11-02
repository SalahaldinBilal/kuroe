use bollard::models::ContainerStatsResponse;
use sqlx::SqlitePool;

#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub ts: i64,
    pub deployment_id: String,
    pub service: String,
    pub container_id: String,
    pub cpu_percent: f64,
    pub mem_bytes: i64,
    pub mem_limit: i64,
    pub net_rx: i64,
    pub net_tx: i64,
    pub blk_read: i64,
    pub blk_write: i64,
}

pub fn parse(stats: &ContainerStatsResponse, ts: i64, deployment_id: &str, service: &str, container_id: &str) -> Sample {
    let cpu = stats.cpu_stats.as_ref();
    let pre = stats.precpu_stats.as_ref();
    let total = |c: Option<&bollard::models::ContainerCpuStats>| c.and_then(|c| c.cpu_usage.as_ref()).and_then(|u| u.total_usage).unwrap_or(0);
    let system = |c: Option<&bollard::models::ContainerCpuStats>| c.and_then(|c| c.system_cpu_usage).unwrap_or(0);
    let cpu_delta = total(cpu).saturating_sub(total(pre)) as f64;
    let system_delta = system(cpu).saturating_sub(system(pre)) as f64;
    let online = cpu.and_then(|c| c.online_cpus).filter(|n| *n > 0).unwrap_or(1) as f64;
    let cpu_percent = if system_delta > 0.0 && system(pre) > 0 { cpu_delta / system_delta * online * 100.0 } else { 0.0 };

    let memory = stats.memory_stats.as_ref();
    let usage = memory.and_then(|m| m.usage).unwrap_or(0);
    let cache = memory
        .and_then(|m| m.stats.as_ref())
        .and_then(|s| s.get("inactive_file").or_else(|| s.get("total_inactive_file")).copied())
        .unwrap_or(0);

    let (net_rx, net_tx) = stats.networks.iter().flatten().fold((0u64, 0u64), |(rx, tx), (_, n)| {
        (rx + n.rx_bytes.unwrap_or(0), tx + n.tx_bytes.unwrap_or(0))
    });
    let (blk_read, blk_write) = stats
        .blkio_stats
        .as_ref()
        .and_then(|b| b.io_service_bytes_recursive.as_ref())
        .into_iter()
        .flatten()
        .fold((0u64, 0u64), |(r, w), e| match e.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("read") => (r + e.value.unwrap_or(0), w),
            Some("write") => (r, w + e.value.unwrap_or(0)),
            _ => (r, w),
        });

    Sample {
        ts,
        deployment_id: deployment_id.to_owned(),
        service: service.to_owned(),
        container_id: container_id.to_owned(),
        cpu_percent: (cpu_percent * 100.0).round() / 100.0,
        mem_bytes: usage.saturating_sub(cache) as i64,
        mem_limit: memory.and_then(|m| m.limit).unwrap_or(0) as i64,
        net_rx: net_rx as i64,
        net_tx: net_tx as i64,
        blk_read: blk_read as i64,
        blk_write: blk_write as i64,
    }
}

pub async fn series_id(db: &SqlitePool, deployment_id: &str, service: &str, container_id: &str) -> sqlx::Result<i64> {
    sqlx::query("INSERT OR IGNORE INTO metric_series (deployment_id, service, container_id) VALUES (?, ?, ?)")
        .bind(deployment_id)
        .bind(service)
        .bind(container_id)
        .execute(db)
        .await?;
    sqlx::query_scalar("SELECT id FROM metric_series WHERE deployment_id = ? AND service = ? AND container_id = ?")
        .bind(deployment_id)
        .bind(service)
        .bind(container_id)
        .fetch_one(db)
        .await
}

pub async fn insert(db: &SqlitePool, series: i64, s: &Sample) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO container_metrics
         (series_id, ts, cpu_percent, mem_bytes, mem_limit, net_rx, net_tx, blk_read, blk_write)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(series)
    .bind(s.ts)
    .bind(s.cpu_percent)
    .bind(s.mem_bytes)
    .bind(s.mem_limit)
    .bind(s.net_rx)
    .bind(s.net_tx)
    .bind(s.blk_read)
    .bind(s.blk_write)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;

    fn docker_stats() -> ContainerStatsResponse {
        serde_json::from_value(serde_json::json!({
            "read": "2026-10-31T10:00:10Z",
            "preread": "2026-10-31T10:00:00Z",
            "cpu_stats": { "cpu_usage": { "total_usage": 3_000_000_000u64 }, "system_cpu_usage": 120_000_000_000u64, "online_cpus": 4 },
            "precpu_stats": { "cpu_usage": { "total_usage": 1_000_000_000u64 }, "system_cpu_usage": 80_000_000_000u64, "online_cpus": 4 },
            "memory_stats": { "usage": 150_000_000u64, "limit": 536_870_912u64, "stats": { "inactive_file": 50_000_000u64 } },
            "networks": {
                "eth0": { "rx_bytes": 1000, "tx_bytes": 2000 },
                "eth1": { "rx_bytes": 500, "tx_bytes": 250 }
            },
            "blkio_stats": { "io_service_bytes_recursive": [
                { "major": 8, "minor": 0, "op": "read", "value": 4096 },
                { "major": 8, "minor": 0, "op": "write", "value": 8192 },
                { "major": 8, "minor": 16, "op": "Read", "value": 1024 }
            ]}
        }))
        .unwrap()
    }

    #[test]
    fn parses_docker_stats_into_one_row() {
        let sample = parse(&docker_stats(), 1_793_440_010, "dep1", "web", "c1");
        assert_eq!(
            sample,
            Sample {
                ts: 1_793_440_010,
                deployment_id: "dep1".into(),
                service: "web".into(),
                container_id: "c1".into(),
                cpu_percent: 20.0,
                mem_bytes: 100_000_000,
                mem_limit: 536_870_912,
                net_rx: 1500,
                net_tx: 2250,
                blk_read: 5120,
                blk_write: 8192,
            }
        );
    }

    #[test]
    fn first_sample_without_previous_cpu_is_zero() {
        let mut stats = docker_stats();
        stats.precpu_stats = None;
        assert_eq!(parse(&stats, 0, "d", "s", "c").cpu_percent, 0.0);
    }

    #[tokio::test]
    async fn collector_writes_expected_row_shape() {
        let db = test_pool().await;
        let series = series_id(&db, "dep1", "web", "c1").await.unwrap();
        assert_eq!(series_id(&db, "dep1", "web", "c1").await.unwrap(), series);
        insert(&db, series, &parse(&docker_stats(), 42, "dep1", "web", "c1")).await.unwrap();
        let row: (i64, String, String, String, f64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT m.ts, s.deployment_id, s.service, s.container_id, m.cpu_percent, m.mem_bytes, m.mem_limit,
                    m.net_rx, m.net_tx, m.blk_read, m.blk_write
             FROM container_metrics m JOIN metric_series s ON s.id = m.series_id",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(row, (42, "dep1".into(), "web".into(), "c1".into(), 20.0, 100_000_000, 536_870_912, 1500, 2250, 5120, 8192));
    }
}
