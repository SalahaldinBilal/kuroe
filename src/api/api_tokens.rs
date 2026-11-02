use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::AppState;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::api_tokens::{self, ApiToken};

const DAY_SECS: i64 = 86_400;

#[derive(Deserialize)]
pub struct NewToken {
    name: String,
    /// Empty means the token never expires.
    expires_days: Option<i64>,
}

pub async fn list_own(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<ApiToken>>> {
    Ok(Json(api_tokens::list(&state.db, Some(&current.user.id)).await?))
}

pub async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<NewToken>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if !api_tokens::valid_name(&body.name) {
        return Err(ApiError::bad_request("give the token a name of up to 60 characters"));
    }
    if body.expires_days.is_some_and(|days| !(1..=3650).contains(&days)) {
        return Err(ApiError::bad_request("a token expires after 1 to 3650 days, or never"));
    }
    let expires_at = body.expires_days.map(|days| crate::db::now() + days * DAY_SECS);
    match api_tokens::create(&state.db, &current.user, &body.name, expires_at).await {
        Ok((token, value)) => Ok((StatusCode::CREATED, Json(json!({ "token": token, "value": value })))),
        Err(err) if err.as_database_error().is_some_and(|e| e.is_unique_violation()) => {
            Err(ApiError::new(StatusCode::CONFLICT, format!("you already have a token named {}", body.name.trim())))
        }
        Err(err) => Err(err.into()),
    }
}

pub async fn revoke_own(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    if api_tokens::revoke(&state.db, &id, Some(&current.user.id)).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "no such token"))
    }
}

pub async fn list_all(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<ApiToken>>> {
    current.require_owner()?;
    Ok(Json(api_tokens::list(&state.db, None).await?))
}

pub async fn revoke_any(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    current.require_owner()?;
    if api_tokens::revoke(&state.db, &id, None).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "no such token"))
    }
}
