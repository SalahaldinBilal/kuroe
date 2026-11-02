use std::collections::HashMap;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use bollard::query_parameters::{ListContainersOptionsBuilder, LogsOptionsBuilder};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use super::AppState;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::update::{self, BUILD, InstallMode, VERSION, binary, container};

pub async fn version(State(state): State<AppState>, _current: CurrentUser) -> ApiResult<Json<Value>> {
    let mode = update::detect(&state.docker).await;
    Ok(Json(json!({ "version": VERSION, "api": super::API_VERSION, "build": BUILD, "install": mode })))
}

#[derive(Deserialize)]
pub struct UpdateRequest {
    image: Option<String>,
    binary_url: Option<String>,
    sha256: Option<String>,
}

pub async fn start(State(state): State<AppState>, current: CurrentUser, Json(body): Json<UpdateRequest>) -> ApiResult<(StatusCode, Json<Value>)> {
    current.require_owner()?;
    let bad_gateway = |e: anyhow::Error| ApiError::new(StatusCode::BAD_GATEWAY, format!("{e:#}"));
    match update::detect(&state.docker).await {
        InstallMode::Container { id, .. } => {
            let image = body.image.as_deref().map(str::trim).filter(|i| !i.is_empty()).ok_or_else(|| ApiError::bad_request("give the image to update to"))?;
            let updater = container::start_updater(&state.docker, &id, image, &state.runner.data_dir).await.map_err(bad_gateway)?;
            Ok((StatusCode::ACCEPTED, Json(json!({ "updater": updater }))))
        }
        InstallMode::Binary => {
            let url = body.binary_url.clone().filter(|u| u.starts_with("https://") || u.starts_with("http://"))
                .ok_or_else(|| ApiError::bad_request("give the URL of the new binary"))?;
            let (staged, sha) = binary::prepare(&state.http, &url, body.sha256.as_deref(), &state.runner.data_dir)
                .await
                .map_err(|e| ApiError::bad_request(format!("{e:#}. Still running {VERSION}.")))?;
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                if let Err(err) = binary::swap_and_exec(&staged) {
                    tracing::error!("self-update failed, still running {VERSION}: {err:#}");
                }
            });
            Ok((StatusCode::ACCEPTED, Json(json!({ "sha256": sha }))))
        }
        InstallMode::Unsupported => Err(ApiError::bad_request("self-update works for the container install and the Linux binary")),
    }
}

pub async fn latest(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<update::release::Latest>> {
    current.require_owner()?;
    let latest = update::release::latest(&state.http, VERSION).await.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    Ok(Json(latest))
}

pub async fn status(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Value>> {
    current.require_owner()?;
    let filters = HashMap::from([("label", vec![container::LABEL_UPDATER.to_owned()])]);
    let mut updaters = state
        .docker
        .list_containers(Some(ListContainersOptionsBuilder::new().all(true).filters(&filters).build()))
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("docker: {e}")))?;
    updaters.sort_by_key(|c| std::cmp::Reverse(c.created.unwrap_or(0)));
    let Some(latest) = updaters.into_iter().next() else { return Ok(Json(json!({ "updater": null }))) };
    let id = latest.id.unwrap_or_default();
    let inspect = state.docker.inspect_container(&id, None).await.ok();
    let exit_code = inspect.as_ref().and_then(|i| i.state.as_ref()).and_then(|s| s.exit_code);
    let mut stream = state.docker.logs(&id, Some(LogsOptionsBuilder::new().stdout(true).stderr(true).tail("50").build()));
    let mut log = String::new();
    while let Some(Ok(chunk)) = stream.next().await {
        log.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
    }
    Ok(Json(json!({
        "updater": latest.names.and_then(|n| n.first().cloned()).unwrap_or_default().trim_start_matches('/'),
        "state": latest.state.map(|s| s.to_string()),
        "exit_code": exit_code,
        "created": latest.created,
        "log": log,
    })))
}
