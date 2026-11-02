use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::broadcast::error::RecvError;

use super::AppState;
use super::extract::CurrentUser;
use crate::auth::Access;
use crate::events;

const ACCESS_REFRESH: Duration = Duration::from_secs(60);

pub async fn socket(State(state): State<AppState>, current: CurrentUser, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| stream(socket, state, current))
}

async fn stream(mut socket: WebSocket, state: AppState, current: CurrentUser) {
    let mut events = events::subscribe();
    let mut access = current.access;
    let mut refresh = tokio::time::interval(ACCESS_REFRESH);
    refresh.tick().await;
    loop {
        tokio::select! {
            received = events.recv() => {
                let text = match received {
                    Ok(event) if event.visible_to(&access) => serde_json::to_string(&event).unwrap_or_default(),
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => r#"{"kind":"resync"}"#.to_owned(),
                    Err(RecvError::Closed) => break,
                };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            },
            _ = refresh.tick() => {
                if let Ok(fresh) = Access::load(&state.db, &current.user).await {
                    access = fresh;
                }
            }
        }
    }
}
