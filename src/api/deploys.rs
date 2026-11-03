use axum::Json;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use super::AppState;
use super::access;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::deploy::history::{self, Detail};
use crate::deploy::runner::{LiveDeploys, LiveMessage};

const PAGE: i64 = 20;
const MAX_PAGE: i64 = 100;

#[derive(Deserialize)]
pub struct Page {
    #[serde(default)]
    offset: i64,
    limit: Option<i64>,
}

/// Newest first, without output.
pub async fn list(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>, Query(page): Query<Page>) -> ApiResult<Json<Value>> {
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    let limit = page.limit.unwrap_or(PAGE).clamp(1, MAX_PAGE);
    let mut deploys = history::list(&state.db, &id, page.offset.max(0), limit + 1).await?;
    let more = deploys.len() as i64 > limit;
    deploys.truncate(limit as usize);
    Ok(Json(json!({ "deploys": deploys, "more": more })))
}

pub async fn detail(State(state): State<AppState>, current: CurrentUser, Path((id, deploy)): Path<(String, String)>) -> ApiResult<Json<Detail>> {
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    let found = history::get(&state.db, &id, &deploy).await?;
    Ok(Json(found.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such deploy"))?))
}

/// The running deploy's output and steps so far, then each new line and step. Closes when the deploy ends,
/// or at once when none is running.
pub async fn live(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>, ws: WebSocketUpgrade) -> ApiResult<Response> {
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    let deploys = state.runner.live.clone();
    Ok(ws.on_upgrade(move |socket| follow(socket, deploys, id)))
}

async fn send(socket: &mut WebSocket, message: &LiveMessage) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|_| ())?;
    socket.send(Message::Text(text.into())).await.map_err(|_| ())
}

/// The close reason tells the page the stream is over, so it doesn't reconnect.
const CLOSE_FINISHED: &str = "finished";
const CLOSE_IDLE: &str = "idle";

async fn follow(mut socket: WebSocket, deploys: LiveDeploys, id: String) {
    let mut reason = CLOSE_IDLE;
    'follow: while let Some((snapshot, mut rx)) = deploys.follow(&id) {
        reason = CLOSE_FINISHED;
        if send(&mut socket, &snapshot).await.is_err() {
            return;
        }
        loop {
            let message = tokio::select! {
                message = rx.recv() => message,
                incoming = socket.recv() => {
                    if !matches!(incoming, Some(Ok(_))) {
                        return;
                    }
                    continue;
                }
            };
            match message {
                Ok(message) => {
                    if send(&mut socket, &message).await.is_err() {
                        return;
                    }
                }
                // A reader that fell behind starts over from a fresh snapshot.
                Err(RecvError::Lagged(_)) => continue 'follow,
                Err(RecvError::Closed) => break 'follow,
            }
        }
    }
    let frame = CloseFrame { code: close_code::NORMAL, reason: reason.into() };
    let _ = socket.send(Message::Close(Some(frame))).await;
}
