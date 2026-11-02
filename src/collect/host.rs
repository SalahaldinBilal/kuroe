use std::path::Path;

use sqlx::SqlitePool;
use sysinfo::{Disks, System};

#[derive(Debug, Clone, PartialEq)]
pub struct HostSample {
    pub ts: i64,
    pub cpu_percent: f64,
    pub mem_used: i64,
    pub mem_total: i64,
    pub disk_used: i64,
    pub disk_total: i64,
    pub load1: Option<f64>,
}

pub struct HostProbe {
    system: System,
    disks: Disks,
}

impl HostProbe {
    pub fn new() -> Result<Self, sysinfo::Error> {
        let mut system = System::new()?;
        system.refresh_cpu_usage();
        Ok(Self { system, disks: Disks::new_with_refreshed_list()? })
    }

    pub fn sample(&mut self, ts: i64, data_dir: &Path) -> HostSample {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.disks.refresh(true);
        let data_dir = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_owned());
        let disk = self
            .disks
            .iter()
            .filter(|d| data_dir.starts_with(d.mount_point()))
            .max_by_key(|d| d.mount_point().as_os_str().len());
        let (disk_total, disk_free) = disk.map(|d| (d.total_space(), d.available_space())).unwrap_or((0, 0));
        HostSample {
            ts,
            cpu_percent: (self.system.global_cpu_usage() as f64 * 100.0).round() / 100.0,
            mem_used: self.system.used_memory() as i64,
            mem_total: self.system.total_memory() as i64,
            disk_used: disk_total.saturating_sub(disk_free) as i64,
            disk_total: disk_total as i64,
            load1: System::load_average().ok().map(|l| l.one),
        }
    }
}

pub async fn insert(db: &SqlitePool, s: &HostSample) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO host_metrics (ts, cpu_percent, mem_used, mem_total, disk_used, disk_total, load1)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(s.ts)
    .bind(s.cpu_percent)
    .bind(s.mem_used)
    .bind(s.mem_total)
    .bind(s.disk_used)
    .bind(s.disk_total)
    .bind(s.load1)
    .execute(db)
    .await?;
    Ok(())
}
