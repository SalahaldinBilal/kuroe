use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "sign in first")
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "your role does not allow this")
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (public_status(self.status), Json(json!({ "error": self.message }))).into_response()
    }
}

/// Cloudflare and other proxies in front replace a 502 or 504 body with their own page, which hides kuroe's message.
pub fn public_status(status: StatusCode) -> StatusCode {
    match status {
        StatusCode::BAD_GATEWAY | StatusCode::GATEWAY_TIMEOUT => StatusCode::SERVICE_UNAVAILABLE,
        other => other,
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        tracing::error!("{err:#}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error, see server log")
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        anyhow::Error::from(err).into()
    }
}
