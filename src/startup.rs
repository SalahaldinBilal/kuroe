//! Starting the server. When something in startup fails, kuroe still answers on its port with the web UI and the
//! error, so the problem shows in the browser instead of only in a restart-looping container's log.

use std::net::SocketAddr;
use std::path::Path;

use anyhow::Context;
use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use serde_json::json;

use crate::auth::{self, setup_code};
use crate::{api, collect, db, secrets};

pub async fn boot(data_dir: &Path, listen: SocketAddr, secure_cookies: bool, no_migrate: bool) -> anyhow::Result<Router> {
    let pool = db::connect(data_dir).await?;
    secrets::install(data_dir)?;
    if no_migrate {
        db::require_migrated(&pool).await?;
    } else {
        let applied = db::migrate(&pool).await?;
        if applied > 0 {
            tracing::info!("applied {applied} database migrations");
        }
    }
    let interrupted = crate::deploy::history::fail_interrupted(&pool).await.context("closing deploys cut off by the last stop")?;
    if interrupted > 0 {
        tracing::warn!("marked deploys failed that were still running when kuroe stopped: {interrupted}");
    }
    if !auth::has_users(&pool).await? {
        let code = setup_code::get_or_create(data_dir).context("creating the setup code")?;
        tracing::warn!("kuroe is not set up yet. Setup code: {code} (also in {})", setup_code::path(data_dir).display());
    }
    secrets::check_keys(&pool, secrets::sealer()).await?;
    let sealed = secrets::seal_plain(&pool, secrets::sealer()).await.context("sealing saved secrets")?;
    if sealed > 0 {
        tracing::info!("sealed {sealed} plain secrets");
    }
    let mut state = api::AppState::new(pool, data_dir.to_path_buf(), secure_cookies)?;
    state.listen = listen;
    state.spawn_background();
    let runner = state.runner.clone();
    tokio::spawn(async move {
        if let Err(err) = runner.publish_ui_route(listen).await {
            tracing::warn!("route to kuroe's own domain: {err:#}");
        }
    });
    collect::Collector { db: state.db.clone(), docker: state.docker.clone(), data_dir: data_dir.to_path_buf() }.spawn();
    Ok(api::router(state))
}

/// Every API call answers 503 with the reason. The page itself loads, and the SPA shows the reason.
pub fn failed(message: String) -> Router {
    let api_route = format!("{}/{{*rest}}", api::API_BASE);
    Router::new().route(&api_route, any(move || refuse(message.clone()))).fallback(crate::web::spa)
}

async fn refuse(message: String) -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": message, "startup_failed": true }))).into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn a_failed_start_still_answers_with_the_reason() {
        let app = failed("the database was migrated by a newer kuroe".into());
        for uri in ["/api/v1/setup", "/api/v1/health", "/api/v1/deployments/x/metrics?from=1&to=2"] {
            let res = app.clone().oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            let body: serde_json::Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
            assert_eq!(body["error"], "the database was migrated by a newer kuroe");
            assert_eq!(body["startup_failed"], true);
        }
        let page = app.oneshot(Request::get("/deployments").body(Body::empty()).unwrap()).await.unwrap();
        assert_ne!(page.status(), StatusCode::SERVICE_UNAVAILABLE, "the page itself loads");
    }
}
