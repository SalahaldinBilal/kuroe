use std::collections::BTreeMap;
use std::time::Duration;

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::AppState;
use super::access;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::collect::retention::today;
use crate::collect::{Collector, logs};
use crate::db::now;

const POINTS: i64 = 240;
const TAIL_BYTES: u64 = 256 * 1024;
const MIN_SPAN_SECS: i64 = 60;
const MAX_SPAN_SECS: i64 = 400 * 86_400;

/// A time window in unix seconds. Every chart asks for one, the presets only exist in the UI.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Window {
    pub from: i64,
    pub to: i64,
}

impl Window {
    pub fn check(&self) -> ApiResult<Window> {
        let span = self.to - self.from;
        if span < MIN_SPAN_SECS {
            return Err(ApiError::bad_request("pick a range of at least a minute, with from before to"));
        }
        if span > MAX_SPAN_SECS {
            return Err(ApiError::bad_request("pick a range of at most 400 days"));
        }
        Ok(*self)
    }

    fn bucket(&self) -> i64 {
        ((self.to - self.from) / POINTS).max(10)
    }
}

type HostRow = (i64, f64, f64, i64, f64, i64, Option<f64>);

pub async fn host(State(state): State<AppState>, _current: CurrentUser, Query(window): Query<Window>) -> ApiResult<Json<Value>> {
    let window = window.check()?;
    let b = window.bucket();
    let rows: Vec<HostRow> = sqlx::query_as(
        "SELECT (ts / ?) * ? AS t, AVG(cpu_percent), AVG(mem_used), MAX(mem_total), AVG(disk_used), MAX(disk_total), AVG(load1)
         FROM host_metrics WHERE ts >= ? AND ts <= ? GROUP BY t ORDER BY t",
    )
    .bind(b)
    .bind(b)
    .bind(window.from)
    .bind(window.to)
    .fetch_all(&state.db)
    .await?;
    let points: Vec<Value> = rows
        .into_iter()
        .map(|(t, cpu, mem, mem_total, disk, disk_total, load)| {
            json!({ "t": t, "cpu": cpu, "mem": mem as i64, "mem_total": mem_total, "disk": disk as i64, "disk_total": disk_total, "load1": load })
        })
        .collect();
    let containers: Vec<ContainerBucket> = sqlx::query_as(
        "SELECT '' AS service, s.container_id, (m.ts / ?) * ? AS t, 0.0 AS cpu, 0.0 AS mem, 0 AS mem_limit,
                MAX(m.net_rx) AS rx, MAX(m.net_tx) AS tx, 0 AS read, 0 AS write
         FROM metric_series s JOIN container_metrics m ON m.series_id = s.id
         WHERE m.ts >= ? AND m.ts <= ?
         GROUP BY s.container_id, t",
    )
    .bind(b)
    .bind(b)
    .bind(window.from)
    .bind(window.to)
    .fetch_all(&state.db)
    .await?;
    let network: Vec<Value> = aggregate(containers)
        .into_values()
        .flatten()
        .map(|p| json!({ "t": p.t, "rx_rate": p.rx_rate, "tx_rate": p.tx_rate }))
        .collect();
    Ok(Json(json!({ "bucket": b, "points": points, "network": network })))
}

#[derive(sqlx::FromRow)]
struct ContainerBucket {
    service: String,
    container_id: String,
    t: i64,
    cpu: f64,
    mem: f64,
    mem_limit: i64,
    rx: i64,
    tx: i64,
    read: i64,
    write: i64,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct ServicePoint {
    t: i64,
    cpu: f64,
    mem: i64,
    mem_limit: i64,
    rx_rate: f64,
    tx_rate: f64,
    read_rate: f64,
    write_rate: f64,
}

fn rate(current: i64, previous: Option<(i64, i64)>, t: i64) -> f64 {
    match previous {
        Some((prev_t, prev)) if current >= prev && t > prev_t => (current - prev) as f64 / (t - prev_t) as f64,
        _ => 0.0,
    }
}

/// Each container's points, by service and container id. Counters become rates within one container.
fn per_container(rows: Vec<ContainerBucket>) -> BTreeMap<(String, String), Vec<ServicePoint>> {
    let mut by_container: BTreeMap<(String, String), Vec<ContainerBucket>> = BTreeMap::new();
    for row in rows {
        by_container.entry((row.service.clone(), row.container_id.clone())).or_default().push(row);
    }
    by_container
        .into_iter()
        .map(|(key, mut series)| {
            series.sort_by_key(|r| r.t);
            let mut prev: Option<&ContainerBucket> = None;
            let mut points = Vec::with_capacity(series.len());
            for row in &series {
                points.push(ServicePoint {
                    t: row.t,
                    cpu: row.cpu,
                    mem: row.mem as i64,
                    mem_limit: row.mem_limit,
                    rx_rate: rate(row.rx, prev.map(|p| (p.t, p.rx)), row.t),
                    tx_rate: rate(row.tx, prev.map(|p| (p.t, p.tx)), row.t),
                    read_rate: rate(row.read, prev.map(|p| (p.t, p.read)), row.t),
                    write_rate: rate(row.write, prev.map(|p| (p.t, p.write)), row.t),
                });
                prev = Some(row);
            }
            (key, points)
        })
        .collect()
}

fn sum_by_service(containers: &BTreeMap<(String, String), Vec<ServicePoint>>) -> BTreeMap<String, Vec<ServicePoint>> {
    let mut by_service: BTreeMap<String, BTreeMap<i64, ServicePoint>> = BTreeMap::new();
    for ((service, _), points) in containers {
        for p in points {
            let total = by_service.entry(service.clone()).or_default().entry(p.t).or_insert_with(|| ServicePoint { t: p.t, ..Default::default() });
            total.cpu += p.cpu;
            total.mem += p.mem;
            total.mem_limit += p.mem_limit;
            total.rx_rate += p.rx_rate;
            total.tx_rate += p.tx_rate;
            total.read_rate += p.read_rate;
            total.write_rate += p.write_rate;
        }
    }
    by_service.into_iter().map(|(s, points)| (s, points.into_values().collect())).collect()
}

fn aggregate(rows: Vec<ContainerBucket>) -> BTreeMap<String, Vec<ServicePoint>> {
    sum_by_service(&per_container(rows))
}

pub async fn deployment(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(window): Query<Window>,
) -> ApiResult<Json<Value>> {
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    let window = window.check()?;
    let b = window.bucket();
    let rows: Vec<ContainerBucket> = sqlx::query_as(
        "SELECT s.service, s.container_id, (m.ts / ?) * ? AS t, AVG(m.cpu_percent) AS cpu, AVG(m.mem_bytes) AS mem,
                MAX(m.mem_limit) AS mem_limit, MAX(m.net_rx) AS rx, MAX(m.net_tx) AS tx, MAX(m.blk_read) AS read,
                MAX(m.blk_write) AS write
         FROM metric_series s JOIN container_metrics m ON m.series_id = s.id
         WHERE s.deployment_id = ? AND m.ts >= ? AND m.ts <= ?
         GROUP BY s.service, s.container_id, t",
    )
    .bind(b)
    .bind(b)
    .bind(&id)
    .bind(window.from)
    .bind(window.to)
    .fetch_all(&state.db)
    .await?;
    let containers = per_container(rows);
    let services = sum_by_service(&containers);
    let containers: Vec<Value> = containers
        .into_iter()
        .map(|((service, container_id), points)| json!({ "service": service, "container_id": container_id, "points": points }))
        .collect();
    Ok(Json(json!({ "bucket": b, "services": services, "containers": containers })))
}

/// Every deployment the user can see, its containers summed.
pub async fn by_deployment(State(state): State<AppState>, current: CurrentUser, Query(window): Query<Window>) -> ApiResult<Json<Value>> {
    let window = window.check()?;
    let b = window.bucket();
    let names: BTreeMap<String, String> = crate::deploy::list(&state.db)
        .await?
        .into_iter()
        .filter(|d| current.access.can(&d.id, Permission::DepView))
        .map(|d| (d.id, d.name))
        .collect();
    let rows: Vec<ContainerBucket> = sqlx::query_as(
        "SELECT s.deployment_id AS service, s.container_id, (m.ts / ?) * ? AS t, AVG(m.cpu_percent) AS cpu, AVG(m.mem_bytes) AS mem,
                MAX(m.mem_limit) AS mem_limit, MAX(m.net_rx) AS rx, MAX(m.net_tx) AS tx, MAX(m.blk_read) AS read,
                MAX(m.blk_write) AS write
         FROM metric_series s JOIN container_metrics m ON m.series_id = s.id
         WHERE m.ts >= ? AND m.ts <= ?
         GROUP BY s.deployment_id, s.container_id, t",
    )
    .bind(b)
    .bind(b)
    .bind(window.from)
    .bind(window.to)
    .fetch_all(&state.db)
    .await?;
    let rows = rows.into_iter().filter(|r| names.contains_key(&r.service)).collect();
    let deployments: Vec<Value> = aggregate(rows)
        .into_iter()
        .filter_map(|(id, points)| Some(json!({ "id": id, "name": names.get(&id)?, "points": points })))
        .collect();
    Ok(Json(json!({ "bucket": b, "deployments": deployments })))
}

fn collector(state: &AppState) -> Collector {
    Collector { db: state.db.clone(), docker: state.docker.clone(), data_dir: state.runner.data_dir.clone() }
}

pub async fn storage(State(state): State<AppState>, _current: CurrentUser) -> ApiResult<Json<Value>> {
    let latest: Option<i64> = sqlx::query_scalar("SELECT MAX(ts) FROM storage_metrics").fetch_one(&state.db).await?;
    let rows: Vec<(String, i64)> = match latest {
        Some(ts) => sqlx::query_as("SELECT category, bytes FROM storage_metrics WHERE ts = ? ORDER BY category").bind(ts).fetch_all(&state.db).await?,
        None => Vec::new(),
    };
    let categories: BTreeMap<String, i64> = rows.into_iter().collect();
    Ok(Json(json!({ "ts": latest, "categories": categories })))
}

pub async fn refresh_storage(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Value>> {
    current.require(Permission::Settings)?;
    let usage = collector(&state).record_storage().await.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    let categories: BTreeMap<&str, u64> = usage.iter().map(|u| (u.category, u.bytes)).collect();
    Ok(Json(json!({ "ts": now(), "categories": categories })))
}

#[derive(Serialize)]
pub struct LogFile {
    service: String,
    date: String,
    bytes: u64,
}

pub async fn log_files(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Vec<LogFile>>> {
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    let dir = collector(&state).logs_dir().join(&id);
    let files = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<LogFile>> {
        let mut files = Vec::new();
        if !dir.exists() {
            return Ok(files);
        }
        for service in std::fs::read_dir(&dir)? {
            let service = service?;
            if !service.file_type()?.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(service.path())? {
                let file = file?;
                let name = file.file_name().to_string_lossy().into_owned();
                if let Some(date) = name.strip_suffix(".log") {
                    files.push(LogFile {
                        service: service.file_name().to_string_lossy().into_owned(),
                        date: date.to_owned(),
                        bytes: file.metadata()?.len(),
                    });
                }
            }
        }
        files.sort_by(|a, b| (&b.date, &a.service).cmp(&(&a.date, &b.service)));
        Ok(files)
    })
    .await
    .map_err(anyhow::Error::from)?
    .map_err(anyhow::Error::from)?;
    Ok(Json(files))
}

#[derive(Deserialize)]
pub struct LogQuery {
    service: String,
    date: Option<String>,
}

fn safe_segment(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !value.contains("..")
}

pub async fn log_socket(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<LogQuery>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    let today = today(now());
    let date = q.date.unwrap_or_else(|| today.clone());
    if !safe_segment(&q.service) || !safe_segment(&date) {
        return Err(ApiError::bad_request("bad service or date"));
    }
    let path = logs::day_file(&collector(&state).logs_dir(), &id, &q.service, &date);
    let live = date == today;
    Ok(ws.on_upgrade(move |socket| stream_file(socket, path, live)))
}

async fn stream_file(mut socket: WebSocket, path: std::path::PathBuf, live: bool) {
    let mut offset = match tokio::fs::metadata(&path).await {
        Ok(meta) => {
            let start = meta.len();
            let text = tokio::task::spawn_blocking({
                let path = path.clone();
                move || logs::tail(&path, TAIL_BYTES)
            })
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
            if socket.send(Message::Text(text.into())).await.is_err() {
                return;
            }
            start
        }
        Err(_) => 0,
    };
    if !live {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let Ok(meta) = tokio::fs::metadata(&path).await else { continue };
                if meta.len() <= offset {
                    continue;
                }
                let chunk = read_range(&path, offset, meta.len()).await;
                offset = meta.len();
                if socket.send(Message::Text(chunk.into())).await.is_err() {
                    return;
                }
            }
            msg = socket.recv() => if !matches!(msg, Some(Ok(_))) { return },
        }
    }
}

async fn read_range(path: &std::path::Path, from: u64, to: u64) -> String {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let Ok(mut file) = tokio::fs::File::open(path).await else { return String::new() };
    if file.seek(std::io::SeekFrom::Start(from)).await.is_err() {
        return String::new();
    }
    let mut buf = vec![0u8; (to - from) as usize];
    match file.read_exact(&mut buf).await {
        Ok(_) => String::from_utf8_lossy(&buf).into_owned(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(container: &str, t: i64, cpu: f64, mem: f64, rx: i64) -> ContainerBucket {
        ContainerBucket {
            service: "web".into(),
            container_id: container.into(),
            t,
            cpu,
            mem,
            mem_limit: 100,
            rx,
            tx: 0,
            read: 0,
            write: 0,
        }
    }

    #[test]
    fn replicas_are_summed_and_counters_become_rates() {
        let out = aggregate(vec![
            row("a", 0, 10.0, 50.0, 1000),
            row("a", 60, 20.0, 60.0, 7000),
            row("b", 0, 5.0, 40.0, 0),
            row("b", 60, 5.0, 40.0, 600),
        ]);
        let web = &out["web"];
        assert_eq!(web.len(), 2);
        assert_eq!((web[0].cpu, web[0].mem, web[0].mem_limit, web[0].rx_rate), (15.0, 90, 200, 0.0));
        assert_eq!((web[1].cpu, web[1].mem, web[1].rx_rate), (25.0, 100, 110.0));
    }

    #[test]
    fn host_network_totals_every_container() {
        let mut a = row("a", 0, 0.0, 0.0, 0);
        a.service = String::new();
        let mut a2 = row("a", 60, 0.0, 0.0, 6000);
        a2.service = String::new();
        let mut b = row("b", 60, 0.0, 0.0, 100);
        b.service = String::new();
        let mut b2 = row("b", 120, 0.0, 0.0, 700);
        b2.service = String::new();
        let all = aggregate(vec![a, a2, b, b2]).remove("").unwrap();
        let rates: Vec<(i64, f64)> = all.iter().map(|p| (p.t, p.rx_rate)).collect();
        assert_eq!(rates, [(0, 0.0), (60, 100.0), (120, 10.0)]);
    }

    #[test]
    fn each_container_keeps_its_own_series() {
        let containers = per_container(vec![row("a", 0, 10.0, 50.0, 1000), row("a", 60, 20.0, 60.0, 7000), row("b", 60, 5.0, 40.0, 600)]);
        let a = &containers[&("web".to_owned(), "a".to_owned())];
        assert_eq!(a.iter().map(|p| (p.t, p.cpu, p.rx_rate)).collect::<Vec<_>>(), [(0, 10.0, 0.0), (60, 20.0, 100.0)]);
        let b = &containers[&("web".to_owned(), "b".to_owned())];
        assert_eq!((b.len(), b[0].mem), (1, 40));
        assert_eq!(sum_by_service(&containers)["web"][1].cpu, 25.0);
    }

    #[test]
    fn counter_reset_after_restart_is_not_a_negative_rate() {
        let out = aggregate(vec![row("a", 0, 0.0, 0.0, 9000), row("a", 60, 0.0, 0.0, 100)]);
        assert_eq!(out["web"][1].rx_rate, 0.0);
    }

    #[test]
    fn any_window_maps_to_about_240_points() {
        let window = |from: i64, to: i64| Window { from, to };
        assert_eq!(window(0, 3_600).check().unwrap().bucket(), 15);
        assert_eq!(window(1_000, 1_000 + 30 * 86_400).check().unwrap().bucket(), 10_800);
        assert_eq!(window(0, 600).check().unwrap().bucket(), 10, "never finer than the samples");
        assert_eq!(window(5_000, 5_000 + 90 * 60).check().unwrap().bucket(), 22, "any span, not only presets");
        assert!(window(100, 100).check().is_err());
        assert!(window(200, 100).check().is_err(), "from before to");
        assert!(window(0, 401 * 86_400).check().is_err());
    }
}
