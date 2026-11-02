use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::AppState;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::deploy::spec::{GIT_TOKEN_REF, Spec};
use crate::deploy;
use crate::saved_secrets::{self, SavedSecret};

const MATCHES_PER_MINUTE: u32 = 30;

/// Paste matches per user, so the match endpoint can't be used to guess values quickly.
#[derive(Default)]
pub struct MatchLimiter {
    calls: Mutex<HashMap<String, (u32, Instant)>>,
}

impl MatchLimiter {
    pub fn allow(&self, user: &str) -> bool {
        let mut calls = self.calls.lock().unwrap();
        let entry = calls.entry(user.to_owned()).or_insert((0, Instant::now()));
        if entry.1.elapsed() > Duration::from_secs(60) {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
        entry.0 <= MATCHES_PER_MINUTE
    }
}

fn can_use(current: &CurrentUser) -> bool {
    current.access.has(Permission::SecretUse) || current.access.has(Permission::SecretManage)
}

fn require_use(current: &CurrentUser) -> ApiResult<()> {
    if can_use(current) { Ok(()) } else { Err(ApiError::forbidden()) }
}

#[derive(Serialize)]
pub struct UsedBy {
    id: String,
    name: String,
}

#[derive(Serialize)]
pub struct Row {
    #[serde(flatten)]
    secret: SavedSecret,
    used_by: Vec<UsedBy>,
}

async fn users_of(state: &AppState) -> ApiResult<HashMap<String, Vec<UsedBy>>> {
    let mut map: HashMap<String, Vec<UsedBy>> = HashMap::new();
    for d in deploy::list(&state.db).await? {
        for id in saved_secrets::uses(&d.spec) {
            map.entry(id.to_owned()).or_default().push(UsedBy { id: d.id.clone(), name: d.name.clone() });
        }
    }
    Ok(map)
}

async fn found(state: &AppState, id: &str) -> ApiResult<SavedSecret> {
    saved_secrets::get(&state.db, id).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such saved secret"))
}

pub async fn list(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<Row>>> {
    require_use(&current)?;
    let mut used = users_of(&state).await?;
    let rows = saved_secrets::list(&state.db)
        .await?
        .into_iter()
        .map(|secret| Row { used_by: used.remove(&secret.id).unwrap_or_default(), secret })
        .collect();
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct Body {
    name: String,
    #[serde(default)]
    description: String,
    /// Empty on an update keeps the stored value.
    #[serde(default)]
    value: String,
}

fn check_name(name: &str) -> ApiResult<()> {
    if saved_secrets::valid_name(name) {
        Ok(())
    } else {
        Err(ApiError::bad_request("name must be 1 to 40 characters of a-z, 0-9 and '-'"))
    }
}

fn taken(err: &sqlx::Error, name: &str) -> Option<ApiError> {
    err.as_database_error()
        .is_some_and(|e| e.is_unique_violation())
        .then(|| ApiError::new(StatusCode::CONFLICT, format!("a saved secret called {name} already exists")))
}

pub async fn create(State(state): State<AppState>, current: CurrentUser, Json(body): Json<Body>) -> ApiResult<(StatusCode, Json<SavedSecret>)> {
    current.require(Permission::SecretManage)?;
    let name = body.name.trim();
    check_name(name)?;
    if body.value.is_empty() {
        return Err(ApiError::bad_request("the value is empty"));
    }
    match saved_secrets::insert(&state.db, name, body.description.trim(), &body.value, &current.user.id).await {
        Ok(secret) => Ok((StatusCode::CREATED, Json(secret))),
        Err(err) => Err(taken(&err, name).unwrap_or_else(|| err.into())),
    }
}

pub async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<Body>,
) -> ApiResult<StatusCode> {
    current.require(Permission::SecretManage)?;
    found(&state, &id).await?;
    let name = body.name.trim();
    check_name(name)?;
    let value = Some(body.value.as_str()).filter(|v| !v.is_empty());
    match saved_secrets::update(&state.db, &id, name, body.description.trim(), value).await {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(err) => Err(taken(&err, name).unwrap_or_else(|| err.into())),
    }
}

pub async fn remove(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    current.require(Permission::SecretManage)?;
    let secret = found(&state, &id).await?;
    if let Some(users) = users_of(&state).await?.remove(&id) {
        let names: Vec<String> = users.into_iter().map(|u| u.name).collect();
        return Err(ApiError::new(StatusCode::CONFLICT, format!("{} is used by {}", secret.name, names.join(", "))));
    }
    saved_secrets::delete(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn reveal(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    current.require(Permission::SecretManage)?;
    let value = saved_secrets::open_value(&state.db, &id).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such saved secret"))?;
    Ok(Json(json!({ "value": value })))
}

#[derive(Deserialize)]
pub struct MatchBody {
    value: String,
}

pub async fn find_match(State(state): State<AppState>, current: CurrentUser, Json(body): Json<MatchBody>) -> ApiResult<Json<Value>> {
    require_use(&current)?;
    if !state.secret_matches.allow(&current.user.id) {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too many lookups, wait a minute"));
    }
    let found = saved_secrets::match_value(&state.db, &body.value).await?;
    Ok(Json(json!(found.map(|s| json!({ "id": s.id, "name": s.name })))))
}

/// New or changed references need `secret.use`. References kept as they were don't. Every id has to exist.
pub async fn check_spec(state: &AppState, current: &CurrentUser, spec: &Spec, before: Option<&Spec>) -> ApiResult<()> {
    for (key, id) in &spec.secret_refs {
        let kept = before.is_some_and(|b| b.secret_refs.get(key) == Some(id));
        if !kept && !can_use(current) {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "using saved secrets needs the secret.use permission"));
        }
        if saved_secrets::get(&state.db, id).await?.is_none() {
            let what = if key == GIT_TOKEN_REF { "the git access token" } else { key.as_str() };
            return Err(ApiError::bad_request(format!("{what} points at a saved secret that no longer exists")));
        }
    }
    Ok(())
}
