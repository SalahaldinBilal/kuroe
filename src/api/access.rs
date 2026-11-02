use axum::http::StatusCode;
use bollard::models::ContainerInspectResponse;

use super::AppState;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::deploy::{self, Deployment};
use crate::docker::rewrite::LABEL_DEPLOYMENT;

pub async fn deployment(state: &AppState, current: &CurrentUser, id: &str, permission: Permission) -> ApiResult<Deployment> {
    let found = deploy::get(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such deployment"))?;
    if !current.access.can(id, permission) {
        return Err(ApiError::forbidden());
    }
    Ok(found)
}

pub async fn container(
    state: &AppState,
    current: &CurrentUser,
    id: &str,
    permission: Permission,
) -> ApiResult<ContainerInspectResponse> {
    let inspect = state
        .docker
        .inspect_container(id, None)
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "no such container"))?;
    let deployment = inspect
        .config
        .as_ref()
        .and_then(|c| c.labels.as_ref())
        .and_then(|l| l.get(LABEL_DEPLOYMENT))
        .cloned();
    let allowed = match deployment {
        Some(deployment) => current.access.can(&deployment, permission),
        None => current.access.everywhere(permission),
    };
    if !allowed {
        return Err(ApiError::forbidden());
    }
    Ok(inspect)
}

pub async fn host_mounts(state: &AppState, current: &CurrentUser) -> ApiResult<bool> {
    if current.access.is_owner() {
        return Ok(true);
    }
    let allowed: bool = sqlx::query_scalar("SELECT host_mounts FROM users WHERE id = ?")
        .bind(&current.user.id)
        .fetch_one(&state.db)
        .await?;
    Ok(allowed)
}
