use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::deploy::spec::valid_name;

#[derive(Serialize, sqlx::FromRow)]
pub struct Volume {
    name: String,
    created_by: Option<String>,
    created_at: i64,
}

#[derive(Deserialize)]
pub struct NewVolume {
    name: String,
}

fn can_create(current: &CurrentUser) -> ApiResult<()> {
    if current.access.has(Permission::Volumes) || current.access.has(Permission::DepCreate) { Ok(()) } else { Err(ApiError::forbidden()) }
}

pub async fn list(State(state): State<AppState>, _current: CurrentUser) -> ApiResult<Json<Vec<Volume>>> {
    let rows = sqlx::query_as("SELECT name, created_by, created_at FROM volumes ORDER BY name")
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows))
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
    Ok(StatusCode::NO_CONTENT)
}
