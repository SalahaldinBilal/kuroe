use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::access;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::deploy::spec::valid_name;
use crate::volumes::{self, Limit, VolumeInfo};

#[derive(Serialize, sqlx::FromRow)]
struct Row {
    name: String,
    created_by: Option<String>,
    created_at: i64,
}

#[derive(Serialize)]
pub struct Volume {
    name: String,
    created_by: Option<String>,
    created_at: i64,
    usage: Option<VolumeInfo>,
}

#[derive(Deserialize)]
pub struct NewVolume {
    name: String,
}

fn can_create(current: &CurrentUser) -> ApiResult<()> {
    if current.access.has(Permission::Volumes) || current.access.has(Permission::DepCreate) { Ok(()) } else { Err(ApiError::forbidden()) }
}

/// Only the deployments this user can see are named.
fn visible(current: &CurrentUser, mut v: VolumeInfo) -> VolumeInfo {
    v.deployments.retain(|d| current.access.can(&d.id, Permission::DepView));
    v
}

pub async fn list(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<Volume>>> {
    let rows: Vec<Row> = sqlx::query_as("SELECT name, created_by, created_at FROM volumes ORDER BY name").fetch_all(&state.db).await?;
    let mut usage = volumes::all(&state.db).await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| {
                let found = usage.iter().position(|v| v.shared.as_deref() == Some(&r.name)).map(|i| visible(&current, usage.swap_remove(i)));
                Volume { name: r.name, created_by: r.created_by, created_at: r.created_at, usage: found }
            })
            .collect(),
    ))
}

/// Each deployment's own volumes, from its Compose file.
pub async fn of_deployments(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<VolumeInfo>>> {
    Ok(Json(
        volumes::all(&state.db)
            .await?
            .into_iter()
            .filter(|v| v.shared.is_none())
            .map(|v| visible(&current, v))
            .filter(|v| !v.deployments.is_empty())
            .collect(),
    ))
}

pub async fn of_deployment(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Vec<VolumeInfo>>> {
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    Ok(Json(
        volumes::all(&state.db)
            .await?
            .into_iter()
            .filter(|v| v.deployments.iter().any(|d| d.id == id))
            .map(|v| visible(&current, v))
            .collect(),
    ))
}

pub async fn set_limit(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(name): Path<String>,
    Json(limit): Json<Limit>,
) -> ApiResult<StatusCode> {
    current.require(Permission::Volumes)?;
    if limit.max_bytes <= 0 {
        return Err(ApiError::bad_request("give a limit above 0"));
    }
    if !volumes::all(&state.db).await?.iter().any(|v| v.name == name) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, format!("kuroe doesn't know a volume called {name}")));
    }
    volumes::set_limit(&state.db, &name, &limit, &current.user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_limit(State(state): State<AppState>, current: CurrentUser, Path(name): Path<String>) -> ApiResult<StatusCode> {
    current.require(Permission::Volumes)?;
    sqlx::query("DELETE FROM volume_limits WHERE name = ?").bind(&name).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<NewVolume>,
) -> ApiResult<StatusCode> {
    can_create(&current)?;
    if !valid_name(&body.name) {
        return Err(ApiError::bad_request("name must be 1 to 40 characters of a-z, 0-9 and '-'"));
    }
    let inserted = sqlx::query("INSERT OR IGNORE INTO volumes (name, created_by, created_at) VALUES (?, ?, ?)")
        .bind(&body.name)
        .bind(&current.user.id)
        .bind(crate::db::now())
        .execute(&state.db)
        .await?
        .rows_affected();
    if inserted == 0 {
        return Err(ApiError::new(StatusCode::CONFLICT, format!("{} already exists", body.name)));
    }
    state.runner.create_volume(&body.name).await?;
    Ok(StatusCode::CREATED)
}

pub async fn remove(State(state): State<AppState>, current: CurrentUser, Path(name): Path<String>) -> ApiResult<StatusCode> {
    current.require(Permission::Volumes)?;
    let docker_name = format!("kuroe_{name}");
    if let Err(err) = state.docker.remove_volume(&docker_name, None::<bollard::query_parameters::RemoveVolumeOptions>).await {
        let missing = matches!(&err, bollard::errors::Error::DockerResponseServerError { status_code: 404, .. });
        if !missing {
            return Err(ApiError::new(StatusCode::CONFLICT, format!("docker refused to remove it: {err}")));
        }
    }
    sqlx::query("DELETE FROM volumes WHERE name = ?").bind(&name).execute(&state.db).await?;
    volumes::forget(&state.db, &docker_name).await?;
    Ok(StatusCode::NO_CONTENT)
}
