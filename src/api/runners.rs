use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::AppState;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::runners::{self, DockerMode, Instance, Pool, PoolInput, providers};

#[derive(Serialize)]
pub struct PoolView {
    #[serde(flatten)]
    pool: Pool,
    instances: Vec<Instance>,
    default_image: &'static str,
}

pub async fn list(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<PoolView>>> {
    current.require(Permission::Runners)?;
    let mut out = Vec::new();
    for pool in runners::list(&state.db).await? {
        let instances = state.runners.instances(&pool).await.unwrap_or_default();
        out.push(PoolView { default_image: runners::default_image(pool.provider), pool, instances });
    }
    Ok(Json(out))
}

async fn pool_of(state: &AppState, id: &str) -> ApiResult<Pool> {
    runners::get(&state.db, id).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such runner pool"))
}

/// The host's Docker socket is root on the host, so only the owner hands it to jobs.
fn check_docker_mode(current: &CurrentUser, mode: DockerMode) -> ApiResult<()> {
    if mode == DockerMode::Host && !current.user.role.is_owner() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "only the owner can give jobs the host's Docker"));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct NewPool {
    #[serde(flatten)]
    input: PoolInput,
    /// Set after the user read why runners for public repositories are risky.
    #[serde(default)]
    confirm_public: bool,
}

pub async fn create(State(state): State<AppState>, current: CurrentUser, Json(body): Json<NewPool>) -> ApiResult<(StatusCode, Json<Pool>)> {
    current.require(Permission::Runners)?;
    check_docker_mode(&current, body.input.docker_mode)?;
    let identity = crate::identity::get(&state.db, &body.input.identity)
        .await?
        .ok_or_else(|| ApiError::bad_request("that identity no longer exists"))?;
    let kind = identity.kind;
    let pool = match runners::insert(&state.db, body.input, kind, &current.user.id).await {
        Ok(Ok(pool)) => pool,
        Ok(Err(message)) => return Err(ApiError::bad_request(message)),
        Err(err) if err.downcast_ref::<sqlx::Error>().and_then(|e| e.as_database_error()).is_some_and(|e| e.is_unique_violation()) => {
            return Err(ApiError::new(StatusCode::CONFLICT, "a runner pool with that name exists"));
        }
        Err(err) => return Err(err.into()),
    };
    if !body.confirm_public {
        let public = providers::is_public(&state.db, &state.http, &pool, &identity).await;
        let refusal = match public {
            Ok(Some(true)) => Some(format!(
                "{} is public. A pull request from a fork could run code on this server. Confirm to create the pool anyway.",
                pool.scope
            )),
            Ok(_) => None,
            Err(ref err) => Some(format!("couldn't check {}: {err:#}", pool.scope)),
        };
        if let Some(message) = refusal {
            runners::delete(&state.db, &pool.id).await?;
            let status = if matches!(public, Ok(Some(true))) { StatusCode::CONFLICT } else { StatusCode::BAD_GATEWAY };
            return Err(ApiError::new(status, message));
        }
    }
    state.runners.wake.notify_one();
    Ok((StatusCode::CREATED, Json(pool)))
}

pub async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(input): Json<PoolInput>,
) -> ApiResult<StatusCode> {
    current.require(Permission::Runners)?;
    check_docker_mode(&current, input.docker_mode)?;
    let existing = pool_of(&state, &id).await?;
    let identity = crate::identity::get(&state.db, &existing.identity).await?.ok_or_else(|| ApiError::bad_request("the pool's identity was removed"))?;
    let mode_changed = existing.docker_mode != input.docker_mode || existing.image != input.image;
    if let Err(message) = runners::update(&state.db, &existing, input, identity.kind).await? {
        return Err(ApiError::bad_request(message));
    }
    if mode_changed {
        state.runners.stop_all(&existing).await?;
    }
    state.runners.wake.notify_one();
    Ok(StatusCode::NO_CONTENT)
}

/// Stops every runner, removes them at the provider, then the pool. Answers with runners that need removing by hand.
pub async fn remove(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    current.require(Permission::Runners)?;
    let pool = pool_of(&state, &id).await?;
    runners::delete(&state.db, &pool.id).await?;
    let leftovers = state.runners.stop_all(&pool).await?;
    state.runners.clear_caches(&pool).await?;
    Ok(Json(json!({ "leftovers": leftovers })))
}

/// Stops the pool's runners, which ends running jobs, and removes their build caches.
pub async fn clear_cache(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    current.require(Permission::Runners)?;
    let pool = pool_of(&state, &id).await?;
    let removed = state.runners.clear_caches(&pool).await?;
    state.runners.wake.notify_one();
    Ok(Json(json!({ "removed": removed })))
}

pub async fn logs(State(state): State<AppState>, current: CurrentUser, Path((id, instance)): Path<(String, String)>) -> ApiResult<String> {
    current.require(Permission::Runners)?;
    let pool = pool_of(&state, &id).await?;
    state.runners.logs(&pool, &instance).await.map_err(|e| ApiError::new(StatusCode::NOT_FOUND, format!("{e:#}")))
}
