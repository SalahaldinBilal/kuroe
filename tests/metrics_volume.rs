use kuroe::collect::storage::{DB_FILES, dir_size};
use kuroe::db;

const DAYS: i64 = 30;
const CONTAINERS: i64 = 20;
const STATS_EVERY: i64 = 10;
const HOST_EVERY: i64 = 30;
const STORAGE_EVERY: i64 = 600;

#[tokio::test]
#[ignore = "writes about 5 million rows, run with --release"]
async fn thirty_days_of_twenty_containers_fits_the_budget() {
    let dir = std::env::temp_dir().join(format!("kuroe-volume-{}", uuid::Uuid::new_v4()));
    let pool = db::connect(&dir).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();

    let start = 1_790_000_000i64;
    let end = start + DAYS * 86_400;
    let mut series = Vec::new();
    for c in 0..CONTAINERS {
        let deployment = format!("deployment-{:02}-{}", c / 2, "0".repeat(22));
        series.push(kuroe::collect::stats::series_id(&pool, &deployment, "web", &format!("{c:064x}")).await.unwrap());
    }
    let mut tx = pool.begin().await.unwrap();
    let mut rows = 0i64;
    for ts in (start..end).step_by(STATS_EVERY as usize) {
        for c in 0..CONTAINERS {
            sqlx::query("INSERT INTO container_metrics VALUES (?, ?, ?, ?, 536870912, ?, ?, ?, ?)")
                .bind(series[c as usize])
                .bind(ts)
                .bind((ts % 97) as f64 / 3.0)
                .bind(100_000_000 + (ts % 4096) * 1000)
                .bind(ts * 13 % 9_999_999_937)
                .bind(ts * 7 % 9_999_999_937)
                .bind(ts * 3 % 9_999_999_937)
                .bind(ts * 5 % 9_999_999_937)
                .execute(&mut *tx)
                .await
                .unwrap();
            rows += 1;
        }
        if ts % HOST_EVERY == 0 {
            sqlx::query("INSERT INTO host_metrics VALUES (?, 12.5, 8000000000, 16000000000, 50000000000, 200000000000, 0.4)")
                .bind(ts)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        if ts % STORAGE_EVERY == 0 {
            for category in ["kuroe_db", "kuroe_logs", "kuroe_other", "docker_images", "docker_containers", "docker_volumes", "docker_build_cache"] {
                sqlx::query("INSERT INTO storage_metrics VALUES (?, ?, 123456789)").bind(ts).bind(category).execute(&mut *tx).await.unwrap();
            }
        }
        if rows % 200_000 == 0 {
            tx.commit().await.unwrap();
            tx = pool.begin().await.unwrap();
        }
    }
    tx.commit().await.unwrap();
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&pool).await.unwrap();
    pool.close().await;

    let bytes: u64 = DB_FILES.iter().map(|f| std::fs::metadata(dir.join(f)).map(|m| m.len()).unwrap_or(0)).sum();
    let total = dir_size(&dir).unwrap();
    println!("{rows} container rows, database {:.1} MiB", bytes as f64 / (1 << 20) as f64);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(rows, DAYS * 86_400 / STATS_EVERY * CONTAINERS);
    assert!(total < 2 * (1 << 30), "metrics use {total} bytes");
}
