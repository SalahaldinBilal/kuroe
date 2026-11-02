use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::AppState;
use super::error::ApiError;
use crate::auth::{Access, Permission, SESSION_COOKIE, User, session_user};
use crate::deploy::tokens::{self, Grant};

pub const CSRF_HEADER: &str = "x-kuroe-csrf";

pub struct CurrentUser {
    pub user: User,
    pub token: String,
    pub access: Access,
}

impl CurrentUser {
    pub fn require(&self, permission: Permission) -> Result<(), ApiError> {
        if self.access.has(permission) { Ok(()) } else { Err(ApiError::forbidden()) }
    }

    pub fn require_owner(&self) -> Result<(), ApiError> {
        if self.access.is_owner() { Ok(()) } else { Err(ApiError::forbidden()) }
    }
}

pub fn session_cookie(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_owned())
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = session_cookie(parts).ok_or_else(ApiError::unauthorized)?;
        let user = session_user(&state.db, &token).await?.ok_or_else(ApiError::unauthorized)?;
        let access = Access::load(&state.db, &user).await?;
        Ok(CurrentUser { user, token, access })
    }
}

pub enum Caller {
    User(CurrentUser),
    Token(Grant),
}

fn bearer(parts: &Parts) -> Option<&str> {
    parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim)
}

impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if let Some(token) = bearer(parts) {
            let grant = tokens::authenticate(&state.db, token)
                .await?
                .ok_or_else(|| ApiError::new(axum::http::StatusCode::UNAUTHORIZED, "unknown deploy token"))?;
            return Ok(Caller::Token(grant));
        }
        Ok(Caller::User(CurrentUser::from_request_parts(parts, state).await?))
    }
}

/// Bearer and webhook requests carry no cookie, so a forged cross-site request can't ride on them. The MCP endpoint
/// never reads cookies, so it answers a missing token with 401 itself.
fn csrf_exempt(request: &Request) -> bool {
    request.headers().contains_key(header::AUTHORIZATION) && !request.headers().contains_key(header::COOKIE)
        || request.uri().path().starts_with("/hooks/")
        || request.uri().path().trim_start_matches(super::API_BASE) == "/mcp"
}

pub async fn require_csrf_header(State(_): State<AppState>, request: Request, next: Next) -> Response {
    let safe = matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe && !request.headers().contains_key(CSRF_HEADER) && !csrf_exempt(&request) {
        return ApiError::forbidden_csrf().into_response();
    }
    next.run(request).await
}

impl ApiError {
    fn forbidden_csrf() -> Self {
        ApiError::new(axum::http::StatusCode::FORBIDDEN, format!("missing {CSRF_HEADER} header"))
    }
}
