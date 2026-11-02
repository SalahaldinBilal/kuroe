use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::AppState;
use super::access;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::bundle::{self, Bundle, FORMAT};

#[derive(Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    secrets: bool,
}

fn toml_file(name: &str, body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/toml; charset=utf-8".to_owned()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}.kuroe.toml\"")),
        ],
        body,
    )
        .into_response()
}

pub async fn export_deployment(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    let d = access::deployment(&state, &current, &id, Permission::DepEdit).await?;
    if q.secrets && !current.access.can(&id, Permission::DepSecrets) {
        return Err(ApiError::forbidden());
    }
    let deployment = bundle::export_deployment(&state.db, &id, q.secrets).await?;
    let text = bundle::to_toml(&Bundle { format: FORMAT, server: None, deployments: vec![deployment] })?;
    Ok(toml_file(&d.name, text))
}

pub async fn export_server(State(state): State<AppState>, current: CurrentUser, Query(q): Query<ExportQuery>) -> ApiResult<Response> {
    current.require_owner()?;
    let text = bundle::to_toml(&bundle::export_server(&state.db, q.secrets).await?)?;
    Ok(toml_file("server", text))
}

fn parse(text: &str) -> ApiResult<Bundle> {
    bundle::from_toml(text).map_err(|e| ApiError::bad_request(format!("{e:#}")))
}

pub async fn preview(State(state): State<AppState>, current: CurrentUser, body: String) -> ApiResult<Json<Value>> {
    current.require(Permission::Import)?;
    let incoming = parse(&body)?;
    let plan = bundle::plan(&state.db, &incoming).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(json!(plan)))
}

#[derive(Deserialize)]
pub struct ApplyRequest {
    bundle: String,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

pub async fn apply(State(state): State<AppState>, current: CurrentUser, Json(body): Json<ApplyRequest>) -> ApiResult<Json<Value>> {
    current.require(Permission::Import)?;
    let incoming = parse(&body.bundle)?;
    let has_access_rules = incoming.server.as_ref().is_some_and(|s| !s.users.is_empty() || !s.roles.is_empty());
    if has_access_rules && !current.access.has(Permission::Users) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "importing users and roles needs the users permission"));
    }
    let pools = incoming.server.iter().flat_map(|s| &s.runner_pools);
    if pools.clone().next().is_some() && !current.access.has(Permission::Runners) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "importing runner pools needs the runners permission"));
    }
    if pools.into_iter().any(|p| p.docker_mode == crate::runners::DockerMode::Host) && !current.user.role.is_owner() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "only the owner can import runner pools that use the host's Docker"));
    }
    let mut new_secret = false;
    let names = bundle::saved_secret_names(&incoming);
    for name in names.keys() {
        new_secret |= crate::saved_secrets::by_name(&state.db, name).await?.is_none();
    }
    if new_secret && !current.access.has(Permission::SecretManage) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "importing new saved secrets needs the secret.manage permission"));
    }
    if !names.is_empty() && !current.access.has(Permission::SecretUse) && !current.access.has(Permission::SecretManage) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "importing deployments that use saved secrets needs the secret.use permission"));
    }
    let applied = bundle::apply(&state.db, &incoming, &body.secrets, &current.user)
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    for name in &applied.volumes_to_create {
        if state.docker.inspect_volume(&format!("kuroe_{name}")).await.is_err() {
            state.runner.create_volume(name).await?;
        }
    }
    if incoming.server.is_some() {
        let settings = crate::proxy::load_settings(&state.db).await?;
        state.runner.apply_proxy(&settings).await.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    }
    let mut out = json!(applied);
    for (i, hook) in applied.webhooks.iter().enumerate() {
        if let Some(d) = crate::deploy::get(&state.db, &hook.deployment_id).await? {
            out["webhooks"][i]["setup"] = super::ci::hook_setup(&state, &d).await?;
        }
    }
    Ok(Json(out))
}
