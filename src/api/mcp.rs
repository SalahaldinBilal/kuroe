//! Read-only MCP server over streamable HTTP. Each tool calls the same handler the web UI uses, as the token's
//! user, so access checks and secret masking are the ones the UI already has. There are no tools that change anything.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::AppState;
use super::error::ApiError;
use super::extract::CurrentUser;
use super::metrics::Window;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use crate::auth::{Access, Permission, api_tokens};
use crate::collect::{Collector, logs, retention::today};
use crate::deploy::{self, Deployment, Source, Spec};

const SECRET_MASK: &str = "********";
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const LOG_TAIL_BYTES: u64 = 512 * 1024;
const MAX_LOG_LINES: u64 = 500;
const MAX_DEPLOYS: i64 = 100;
const HOUR_SECS: i64 = 3_600;

const INSTRUCTIONS: &str = "kuroe deploys containers on a Docker host. These tools only read: deployments, their \
    deploys, logs, metrics, the server's resources and the proxy routes. Nothing here can deploy, stop or change \
    anything. Deployments can be named by name or id.";

pub async fn post(State(state): State<AppState>, headers: HeaderMap, Json(message): Json<Value>) -> Response {
    let Some(current) = caller(&state, &headers).await else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({ "error": "send a personal API token from Settings as a bearer token" })),
        )
            .into_response();
    };
    if message.is_array() {
        return Json(rpc_error(Value::Null, -32600, "send one message per request")).into_response();
    }
    let Some(id) = message.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let method = message["method"].as_str().unwrap_or_default();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let reply = match method {
        "initialize" => rpc_result(id, initialize(&params)),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({ "tools": tools() })),
        "tools/call" => match params["name"].as_str() {
            Some(name) => rpc_result(id, call(&state, &current, name, &params["arguments"]).await),
            None => rpc_error(id, -32602, "tools/call needs a tool name"),
        },
        "" => rpc_error(id, -32600, "a request needs a method"),
        other => rpc_error(id, -32601, &format!("kuroe doesn't support {other}")),
    };
    Json(reply).into_response()
}

pub async fn get() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

async fn caller(state: &AppState, headers: &HeaderMap) -> Option<CurrentUser> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")?.trim();
    let user = api_tokens::authenticate(&state.db, value).await.ok()??;
    let access = Access::load(&state.db, &user).await.ok()?;
    Some(CurrentUser { user, token: String::new(), access })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "kuroe", "version": crate::update::VERSION },
        "instructions": INSTRUCTIONS,
    })
}

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
    })
}

pub fn tools() -> Vec<Value> {
    let deployment = json!({ "type": "string", "description": "Deployment name or id" });
    vec![
        tool(
            "list_deployments",
            "List deployments",
            "Every deployment you can see, with its live status, revision, source kind and domains.",
            json!({}),
            &[],
        ),
        tool(
            "get_deployment",
            "Get a deployment",
            "One deployment's settings as the deployment page shows them, its containers and their state. Secret values are left out.",
            json!({ "deployment": deployment }),
            &["deployment"],
        ),
        tool(
            "last_deploy",
            "Last deploy",
            "The result of the deployment's last deploy: status, revision, when, and the deploy output.",
            json!({ "deployment": deployment }),
            &["deployment"],
        ),
        tool(
            "deploy_history",
            "Deploy history",
            "The deployment's recent deploys, newest first: revision, status, what started it and who, what was deployed (commit or image digest), and when. Without output.",
            json!({
                "deployment": deployment,
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_DEPLOYS, "description": "How many, 20 when left out" },
            }),
            &["deployment"],
        ),
        tool(
            "deployment_logs",
            "Deployment logs",
            "Recent log lines of a deployment's containers, per service. Needs permission to see the deployment's secrets, because apps print secrets into logs.",
            json!({
                "deployment": deployment,
                "service": { "type": "string", "description": "Only this service. Leave out for all of them." },
                "lines": { "type": "integer", "minimum": 1, "maximum": MAX_LOG_LINES, "description": "Lines per service, 100 when left out" },
                "date": { "type": "string", "description": "A day as YYYY-MM-DD (UTC). Today when left out." },
            }),
            &["deployment"],
        ),
        tool(
            "deployment_metrics",
            "Deployment metrics",
            "CPU, memory, network and disk use per service over any time window: latest, average and peak. CPU is in percent of one core. Rates are bytes per second.",
            json!({
                "deployment": deployment,
                "from": { "type": "string", "description": "Start as RFC 3339, like 2026-11-02T08:00:00Z. An hour before `to` when left out." },
                "to": { "type": "string", "description": "End as RFC 3339. Now when left out." },
            }),
            &["deployment"],
        ),
        tool(
            "server_overview",
            "Server overview",
            "The resource pool and what's counted against it, the host's latest CPU, memory, disk and load, total deployment network traffic, Docker, the proxy and the kuroe version.",
            json!({}),
            &[],
        ),
        tool(
            "proxy_routes",
            "Proxy routes",
            "Every domain on deployments you can see: which deployment, service and port it goes to, and whether it uses HTTPS.",
            json!({}),
            &[],
        ),
    ]
}

async fn call(state: &AppState, current: &CurrentUser, name: &str, args: &Value) -> Value {
    let result = match name {
        "list_deployments" => list_deployments(state, current).await,
        "get_deployment" => get_deployment(state, current, args).await,
        "last_deploy" => last_deploy(state, current, args).await,
        "deploy_history" => deploy_history(state, current, args).await,
        "deployment_logs" => deployment_logs(state, current, args).await,
        "deployment_metrics" => deployment_metrics(state, current, args).await,
        "server_overview" => server_overview(state, current).await,
        "proxy_routes" => proxy_routes(state, current).await,
        other => Err(format!("there is no tool named {other}")),
    };
    match result {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_default();
            json!({ "content": [{ "type": "text", "text": text }], "isError": false })
        }
        Err(message) => json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
    }
}

type ToolResult = Result<Value, String>;

fn as_user(current: &CurrentUser) -> CurrentUser {
    CurrentUser { user: current.user.clone(), token: String::new(), access: current.access.clone() }
}

fn api(err: ApiError) -> String {
    err.message
}

fn internal(err: impl std::fmt::Display) -> String {
    tracing::error!("mcp: {err}");
    "internal error, see the server log".into()
}

/// Finds a deployment by name or id. One the caller can't see answers the same as one that doesn't exist.
async fn resolve(state: &AppState, current: &CurrentUser, args: &Value) -> Result<Deployment, String> {
    let wanted = args["deployment"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("say which deployment")?;
    deploy::list(&state.db)
        .await
        .map_err(internal)?
        .into_iter()
        .find(|d| (d.id == wanted || d.name == wanted) && current.access.can(&d.id, Permission::DepView))
        .ok_or_else(|| format!("no deployment named {wanted} that you can see"))
}

fn source_kind(d: &Value) -> Value {
    d["spec"]["source"]["kind"].clone()
}

async fn list_deployments(state: &AppState, current: &CurrentUser) -> ToolResult {
    let Json(list) = super::deployments::list(State(state.clone()), as_user(current)).await.map_err(api)?;
    let mut out = Vec::new();
    for d in list.into_iter().map(|row| row.deployment) {
        let domains: Vec<String> =
            crate::proxy::domains_for(&state.db, &d.id).await.map_err(internal)?.into_iter().map(|dm| dm.hostname).collect();
        let value = serde_json::to_value(&d).map_err(internal)?;
        out.push(json!({
            "name": d.name,
            "id": d.id,
            "status": d.status,
            "revision": d.revision,
            "source": source_kind(&value),
            "domains": domains,
            "updated_at": d.updated_at,
        }));
    }
    Ok(json!({ "deployments": out }))
}

async fn detail(state: &AppState, current: &CurrentUser, args: &Value) -> ToolResult {
    let d = resolve(state, current, args).await?;
    let Json(detail) = super::deployments::detail(State(state.clone()), as_user(current), Path(d.id)).await.map_err(api)?;
    serde_json::to_value(detail).map_err(internal)
}

/// The web UI shows secret values to users who may see them. An MCP client never gets them.
fn hide_secrets(mut spec: Spec) -> Spec {
    for key in &spec.secrets {
        if let Some(value) = spec.container.env.get_mut(key) {
            *value = SECRET_MASK.into();
        }
    }
    if let Source::Git { token: Some(token), .. } = &mut spec.source {
        *token = SECRET_MASK.into();
    }
    spec
}

async fn get_deployment(state: &AppState, current: &CurrentUser, args: &Value) -> ToolResult {
    let mut value = detail(state, current, args).await?;
    let spec: Spec = serde_json::from_value(value["spec"].clone()).map_err(internal)?;
    value["spec"] = serde_json::to_value(hide_secrets(spec)).map_err(internal)?;
    if let Some(map) = value.as_object_mut() {
        map.remove("last_output");
        map.remove("permissions");
    }
    Ok(value)
}

async fn last_deploy(state: &AppState, current: &CurrentUser, args: &Value) -> ToolResult {
    let value = detail(state, current, args).await?;
    Ok(json!({
        "name": value["name"],
        "status": value["status"],
        "revision": value["revision"],
        "finished_at": value["updated_at"],
        "output": value["last_output"],
    }))
}

async fn deploy_history(state: &AppState, current: &CurrentUser, args: &Value) -> ToolResult {
    let d = resolve(state, current, args).await?;
    let limit = args["limit"].as_i64().unwrap_or(20).clamp(1, MAX_DEPLOYS);
    let deploys = deploy::history::list(&state.db, &d.id, 0, limit).await.map_err(internal)?;
    Ok(json!({ "name": d.name, "deploys": deploys }))
}

fn valid_date(date: &str) -> bool {
    let parts: Vec<&str> = date.split('-').collect();
    parts.len() == 3 && [4, 2, 2].iter().zip(&parts).all(|(len, part)| part.len() == *len && part.chars().all(|c| c.is_ascii_digit()))
}

fn safe_service(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !name.contains("..")
}

fn last_lines(text: &str, count: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(count)..].iter().map(|l| (*l).to_owned()).collect()
}

async fn deployment_logs(state: &AppState, current: &CurrentUser, args: &Value) -> ToolResult {
    let d = resolve(state, current, args).await?;
    if !current.access.can(&d.id, Permission::DepSecrets) {
        return Err("reading logs needs permission to see this deployment's secrets, because apps print secrets into logs".into());
    }
    let count = args["lines"].as_u64().unwrap_or(100).clamp(1, MAX_LOG_LINES) as usize;
    let date = match args["date"].as_str() {
        Some(date) if valid_date(date) => date.to_owned(),
        Some(date) => return Err(format!("{date} is not a day as YYYY-MM-DD")),
        None => today(crate::db::now()),
    };
    let dir = Collector { db: state.db.clone(), docker: state.docker.clone(), data_dir: state.runner.data_dir.clone() }.logs_dir();
    let wanted = args["service"].as_str().map(str::to_owned);
    if let Some(service) = &wanted
        && !safe_service(service)
    {
        return Err(format!("{service} is not a service name"));
    }
    let id = d.id.clone();
    let day = date.clone();
    let services = tokio::task::spawn_blocking(move || -> std::io::Result<BTreeMap<String, Vec<String>>> {
        let root = dir.join(&id);
        let mut names: Vec<String> = match std::fs::read_dir(&root) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        };
        if let Some(service) = &wanted {
            names.retain(|n| n == service);
        }
        let mut out = BTreeMap::new();
        for name in names {
            let text = logs::tail(&logs::day_file(&dir, &id, &name, &day), LOG_TAIL_BYTES).unwrap_or_default();
            out.insert(name, last_lines(&text, count));
        }
        Ok(out)
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    if let Some(service) = args["service"].as_str()
        && services.is_empty()
    {
        return Err(format!("{} has no logs for a service named {service}", d.name));
    }
    Ok(json!({ "name": d.name, "date": date, "services": services }))
}

fn summarize(points: &[Value], key: &str) -> Value {
    let values: Vec<f64> = points.iter().filter_map(|p| p[key].as_f64()).collect();
    if values.is_empty() {
        return Value::Null;
    }
    let peak = values.iter().copied().fold(f64::MIN, f64::max);
    let average = values.iter().sum::<f64>() / values.len() as f64;
    json!({ "latest": values.last(), "average": average, "peak": peak })
}

fn instant(args: &Value, key: &str) -> Result<Option<i64>, String> {
    match args[key].as_str() {
        None => Ok(None),
        Some(text) => OffsetDateTime::parse(text.trim(), &Rfc3339)
            .map(|t| Some(t.unix_timestamp()))
            .map_err(|_| format!("{key} is not a time like 2026-11-02T08:00:00Z")),
    }
}

fn window(args: &Value) -> Result<Window, String> {
    let to = instant(args, "to")?.unwrap_or_else(crate::db::now);
    let from = instant(args, "from")?.unwrap_or(to - HOUR_SECS);
    Window { from, to }.check().map_err(api)
}

fn rfc3339(t: i64) -> Value {
    OffsetDateTime::from_unix_timestamp(t).ok().and_then(|t| t.format(&Rfc3339).ok()).map(Value::String).unwrap_or(Value::Null)
}

async fn deployment_metrics(state: &AppState, current: &CurrentUser, args: &Value) -> ToolResult {
    let d = resolve(state, current, args).await?;
    let window = window(args)?;
    let query = Query(window);
    let Json(raw) = super::metrics::deployment(State(state.clone()), as_user(current), Path(d.id), query).await.map_err(api)?;
    let mut services = serde_json::Map::new();
    for (service, points) in raw["services"].as_object().into_iter().flatten() {
        let points = points.as_array().map(Vec::as_slice).unwrap_or_default();
        services.insert(
            service.clone(),
            json!({
                "samples": points.len(),
                "cpu_percent": summarize(points, "cpu"),
                "memory_bytes": summarize(points, "mem"),
                "memory_limit_bytes": points.last().map(|p| p["mem_limit"].clone()),
                "network_in_bytes_per_second": summarize(points, "rx_rate"),
                "network_out_bytes_per_second": summarize(points, "tx_rate"),
                "disk_read_bytes_per_second": summarize(points, "read_rate"),
                "disk_write_bytes_per_second": summarize(points, "write_rate"),
            }),
        );
    }
    Ok(json!({ "name": d.name, "from": rfc3339(window.from), "to": rfc3339(window.to), "services": services }))
}

async fn server_overview(state: &AppState, current: &CurrentUser) -> ToolResult {
    let Json(pool) = super::handlers::pool_usage(State(state.clone()), as_user(current)).await.map_err(api)?;
    let to = crate::db::now();
    let query = Query(Window { from: to - HOUR_SECS, to });
    let Json(host) = super::metrics::host(State(state.clone()), as_user(current), query).await.map_err(api)?;
    let docker = crate::docker::daemon_info(&state.docker).await.ok();
    let traefik = state.docker.inspect_container(crate::proxy::traefik::CONTAINER, None).await.ok();
    let proxy = crate::proxy::load_settings(&state.db).await.map_err(internal)?;
    let network = host["network"].as_array().map(Vec::as_slice).unwrap_or_default();
    Ok(json!({
        "kuroe_version": crate::update::VERSION,
        "pool": pool,
        "host_latest": host["points"].as_array().and_then(|p| p.last()).cloned(),
        "deployment_network_last_hour": {
            "in_bytes_per_second": summarize(network, "rx_rate"),
            "out_bytes_per_second": summarize(network, "tx_rate"),
        },
        "docker": docker.map(|d| json!({
            "version": d.server_version,
            "cpus": d.cpus,
            "memory_bytes": d.memory_bytes,
            "containers_running": d.containers_running,
            "swarm": d.swarm,
        })),
        "proxy": {
            "mode": serde_json::to_value(proxy.mode).unwrap_or(Value::Null),
            "traefik_running": traefik.and_then(|t| t.state).and_then(|s| s.running).unwrap_or(false),
        },
    }))
}

async fn proxy_routes(state: &AppState, current: &CurrentUser) -> ToolResult {
    let mut routes = Vec::new();
    for d in deploy::list(&state.db).await.map_err(internal)? {
        if !current.access.can(&d.id, Permission::DepView) {
            continue;
        }
        for domain in crate::proxy::domains_for(&state.db, &d.id).await.map_err(internal)? {
            let certificate = match domain.certificate.as_deref() {
                None => "automatic",
                Some("acme") => "Let's Encrypt",
                Some(_) => "saved certificate",
            };
            routes.push(json!({
                "hostname": domain.hostname,
                "deployment": d.name,
                "service": domain.service,
                "port": domain.port,
                "https": domain.tls,
                "certificate": if domain.tls { Some(certificate) } else { None },
                "redirect": domain.options.redirect,
                "compress": domain.options.compress,
                "error_page": domain.options.error_page.is_some(),
                "retry": domain.options.retries(domain.options.error_page.is_some()),
                "timeout_seconds": domain.options.timeout,
                "max_body_mb": domain.options.max_body_mb,
                "rewrite_host": domain.options.rewrite_host,
                "paths": domain.options.paths.iter().map(|p| json!({
                    "path": p.path,
                    "exact": p.exact,
                    "service": p.service,
                    "port": p.port,
                    "error_page": p.error_page.is_some(),
                })).collect::<Vec<_>>(),
            }));
        }
    }
    Ok(json!({ "routes": routes }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_marked_read_only() {
        let tools = tools();
        assert_eq!(tools.len(), 8);
        for tool in &tools {
            assert_eq!(tool["annotations"]["readOnlyHint"], true, "{}", tool["name"]);
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn initialize_answers_with_a_version_the_client_knows() {
        assert_eq!(initialize(&json!({ "protocolVersion": "2025-06-18" }))["protocolVersion"], "2025-06-18");
        assert_eq!(initialize(&json!({ "protocolVersion": "1999-01-01" }))["protocolVersion"], PROTOCOL_VERSIONS[0]);
        assert!(initialize(&json!({}))["capabilities"]["tools"].is_object());
    }

    #[test]
    fn log_helpers() {
        assert!(valid_date("2026-11-02"));
        assert!(!valid_date("2026-11-2") && !valid_date("../../etc") && !valid_date("2026-11-02x"));
        assert!(safe_service("web") && !safe_service("..") && !safe_service("a/b"));
        assert_eq!(last_lines("a\nb\n\nc\n", 2), ["b", "c"]);
        assert_eq!(last_lines("a\n", 5), ["a"]);
    }

    #[test]
    fn secret_values_are_hidden_and_plain_ones_kept() {
        let mut spec = Spec::new(Source::Git {
            url: "https://github.com/acme/api.git".into(),
            reference: None,
            context: None,
            dockerfile: None,
            token: Some("ghp_secret".into()),
            identity: None,
            compose_file: None,
        });
        spec.container.env = BTreeMap::from([("DB_PASSWORD".into(), "hunter2".into()), ("LOG".into(), "info".into())]);
        spec.secrets = std::collections::BTreeSet::from(["DB_PASSWORD".into()]);
        let hidden = serde_json::to_string(&hide_secrets(spec)).unwrap();
        assert!(!hidden.contains("hunter2") && !hidden.contains("ghp_secret"), "{hidden}");
        assert!(hidden.contains("\"LOG\":\"info\""), "{hidden}");
    }

    #[test]
    fn metric_windows_take_any_times() {
        let w = window(&json!({ "from": "2026-11-02T08:00:00Z", "to": "2026-11-02T09:30:00Z" })).unwrap();
        assert_eq!(w.to - w.from, 90 * 60);
        assert_eq!(rfc3339(w.from), "2026-11-02T08:00:00Z");
        let w = window(&json!({ "to": "2026-11-02T09:00:00+02:00" })).unwrap();
        assert_eq!(w.to - w.from, HOUR_SECS, "an hour when from is left out");
        assert!(window(&json!({ "from": "yesterday" })).is_err());
        assert!(window(&json!({ "from": "2026-11-02T09:00:00Z", "to": "2026-11-02T08:00:00Z" })).is_err());
    }

    #[test]
    fn summaries_have_latest_average_and_peak() {
        let points = [json!({ "cpu": 10.0 }), json!({ "cpu": 30.0 }), json!({ "cpu": 20.0 })];
        assert_eq!(summarize(&points, "cpu"), json!({ "latest": 20.0, "average": 20.0, "peak": 30.0 }));
        assert_eq!(summarize(&[], "cpu"), Value::Null);
    }
}
