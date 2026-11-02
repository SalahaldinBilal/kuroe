use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use bollard::query_parameters::ListContainersOptionsBuilder;
use serde::Serialize;
use serde_json::{Value, json};

use super::AppState;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::docker::rewrite::{LABEL_DEPLOYMENT, LABEL_SERVICE};
use crate::proxy::ports::{self, Holder};
use crate::proxy::webserver::{self, Entry, File, Render};
use crate::proxy::{self as store, Bind, Domain, Mode, ProxySettings};

const PORTS_TTL: Duration = Duration::from_secs(60);

pub type PortsCache = std::sync::Mutex<Option<(Instant, Vec<Holder>)>>;

/// The host port a domain's service publishes, for external mode where the web server forwards to it directly.
pub(super) async fn host_upstream(state: &AppState, domain: &Domain) -> ApiResult<Option<String>> {
    let filters = HashMap::from([(
        "label",
        vec![format!("{LABEL_DEPLOYMENT}={}", domain.deployment_id), format!("{LABEL_SERVICE}={}", domain.service)],
    )]);
    let containers = state
        .docker
        .list_containers(Some(ListContainersOptionsBuilder::new().filters(&filters).build()))
        .await
        .map_err(|e| ApiError::new(axum::http::StatusCode::BAD_GATEWAY, format!("docker: {e}")))?;
    Ok(containers
        .iter()
        .flat_map(|c| c.ports.iter().flatten())
        .find(|p| p.private_port as i64 == domain.port && p.public_port.is_some() && p.ip.as_deref() != Some("::"))
        .and_then(|p| p.public_port)
        .map(|port| format!("127.0.0.1:{port}")))
}

/// Whether kuroe writes a web server config at all for these settings.
fn in_front(s: &ProxySettings) -> bool {
    (s.mode == Mode::Managed && s.bind == Bind::Behind) || s.mode == Mode::External
}

/// Every domain as an entry for the web server. Behind: all to Traefik. External: each to its own host port.
async fn entries(state: &AppState, s: &ProxySettings) -> ApiResult<(Vec<Entry>, Vec<String>)> {
    let mut list = Vec::new();
    let mut skipped = Vec::new();
    for d in store::all_domains(&state.db).await? {
        if s.mode == Mode::Managed {
            list.push(Entry { hostname: d.hostname, upstream: s.behind_http.clone(), tls: d.tls, probe: None });
            continue;
        }
        match host_upstream(state, &d).await? {
            Some(upstream) => list.push(Entry { hostname: d.hostname.clone(), upstream, tls: d.tls, probe: Some(d.probe_token.clone()) }),
            None => skipped.push(d.hostname),
        }
    }
    if let Some(own) = crate::proxy::ui::load(&state.db).await? {
        let upstream = if s.mode == Mode::Managed {
            Some(s.behind_http.clone())
        } else {
            crate::proxy::ui::host_address(&state.docker, state.listen).await
        };
        match upstream {
            Some(upstream) => list.push(Entry { hostname: own.hostname, upstream, tls: own.tls, probe: None }),
            None => skipped.push(own.hostname),
        }
    }
    Ok((list, skipped))
}

fn render(s: &ProxySettings, ws: &webserver::Settings, list: &[Entry]) -> ApiResult<Vec<File>> {
    let mode = if s.mode == Mode::Managed { ws.mode } else { webserver::Mode::Front };
    webserver::render(&Render {
        kind: ws.kind,
        mode,
        entries: list,
        cert_paths: &ws.cert_paths,
        traefik_tls: &s.behind_https,
        rest_tls: &ws.rest_tls,
        path: PathBuf::from(&ws.path),
        extra_path: ws.extra_path.as_ref().filter(|p| !p.is_empty()).map(PathBuf::from),
    })
    .map_err(|e| ApiError::bad_request(e.to_string()))
}

#[derive(Serialize)]
pub struct Preview {
    files: Vec<File>,
    steps: Vec<String>,
    skipped: Vec<String>,
}

/// The files kuroe would write for these (unsaved) settings, and what the user adds to their own config once.
pub async fn preview(State(state): State<AppState>, current: CurrentUser, Json(s): Json<ProxySettings>) -> ApiResult<Json<Preview>> {
    current.require(Permission::Proxy)?;
    let ws = s.web_server.clone().ok_or_else(|| ApiError::bad_request("pick the web server first"))?;
    let (list, skipped) = entries(&state, &s).await?;
    let files = render(&s, &ws, &list)?;
    let mode = if s.mode == Mode::Managed { ws.mode } else { webserver::Mode::Front };
    let mut steps = if mode == webserver::Mode::Front { webserver::certificate_steps(ws.kind, &list, &ws.cert_paths) } else { Vec::new() };
    steps.extend(webserver::one_time_steps(ws.kind, mode, &ws.path, ws.extra_path.as_deref(), &ws.rest_tls));
    Ok(Json(Preview { files, steps, skipped }))
}

/// Renders and installs the web server config for the saved settings. `Ok(None)` when there is nothing to write.
pub(super) async fn write_config(state: &AppState) -> ApiResult<Option<String>> {
    let s = store::load_settings(&state.db).await?;
    let Some(ws) = s.web_server.clone().filter(|_| in_front(&s)) else { return Ok(None) };
    let (list, _) = entries(state, &s).await?;
    let files = render(&s, &ws, &list)?;
    let result = webserver::install::install(&state.docker, &files, &ws.reloader()).await;
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    sqlx::query("INSERT INTO settings (key, value) VALUES ('web_server_error', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(error.clone().unwrap_or_default())
        .execute(&state.db)
        .await?;
    result.map(Some).map_err(|e| ApiError::bad_request(format!("{e:#}")))
}

/// After a domain change: rewrite the web server config when the user asked kuroe to keep it in sync.
/// A failure is saved for the Proxy page rather than failing the domain change, Traefik already has the route.
pub(super) async fn sync_if_auto(state: &AppState) -> ApiResult<()> {
    let s = store::load_settings(&state.db).await?;
    if s.web_server.as_ref().is_some_and(|w| w.auto) && in_front(&s)
        && let Err(err) = write_config(state).await
    {
        tracing::warn!("web server config not updated: {}", err.message);
    }
    Ok(())
}

pub async fn install(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Value>> {
    current.require(Permission::Proxy)?;
    match write_config(&state).await? {
        Some(output) => Ok(Json(json!({ "output": output }))),
        None => Err(ApiError::bad_request("nothing to write, pick a web server and run Traefik behind it or use external mode")),
    }
}

async fn binary_install(state: &AppState) -> bool {
    matches!(crate::update::detect(&state.docker).await, crate::update::InstallMode::Binary)
}

fn probe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("a plain client builds")
}

pub(super) async fn holders(state: &AppState, fresh: bool) -> Vec<Holder> {
    if !fresh
        && let Some((at, cached)) = state.ports.lock().unwrap().as_ref()
        && at.elapsed() < PORTS_TTL
    {
        return cached.clone();
    }
    let settings = store::load_settings(&state.db).await.ok();
    let wanted = settings.map(|s| vec![s.http_port, s.https_port]).unwrap_or_else(|| vec![80, 443]);
    let found = ports::detect(&state.docker, &probe_client(), &wanted, binary_install(state).await).await;
    *state.ports.lock().unwrap() = Some((Instant::now(), found.clone()));
    found
}

pub async fn ports(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Value>> {
    current.require(Permission::Proxy)?;
    let found = holders(&state, true).await;
    Ok(Json(json!({ "holders": found, "message": ports::describe(&found) })))
}

/// "nginx is using ports 80 and 443, so Traefik can't start." when the ports Traefik wants are taken.
pub(super) async fn blocked_message(state: &AppState, fresh: bool) -> Option<String> {
    let s = store::load_settings(&state.db).await.ok()?;
    if s.mode != Mode::Managed || s.bind != Bind::Ports {
        return None;
    }
    let found = holders(state, fresh).await;
    ports::describe(&found).map(|who| format!("{who}, so Traefik can't start"))
}
