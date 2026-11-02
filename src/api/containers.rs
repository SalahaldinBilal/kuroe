use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use bollard::query_parameters::{AttachContainerOptionsBuilder, LogsOptionsBuilder, ResizeContainerTTYOptionsBuilder};
use futures::{SinkExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use super::{AppState, ExecGrant, access};
use crate::auth::Permission;

fn docker_error(err: bollard::errors::Error) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, format!("docker: {err}"))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_tail")]
    tail: String,
}

fn default_tail() -> String {
    "500".into()
}

pub async fn logs(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    access::container(&state, &current, &id, Permission::DepView).await?;
    let options = LogsOptionsBuilder::new().follow(true).stdout(true).stderr(true).tail(&query.tail).build();
    let stream = state.docker.logs(&id, Some(options));
    Ok(ws.on_upgrade(move |socket| pump_output(socket, Box::pin(stream))))
}

async fn pump_output(
    socket: WebSocket,
    mut output: std::pin::Pin<Box<dyn Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>,
) {
    let (mut tx, mut rx) = socket.split();
    loop {
        tokio::select! {
            item = output.next() => match item {
                Some(Ok(chunk)) => {
                    if tx.send(Message::Binary(chunk.into_bytes())).await.is_err() { break; }
                }
                Some(Err(err)) => {
                    let _ = tx.send(Message::Text(format!("\r\n[kuroe] stream ended: {err}\r\n").into())).await;
                    break;
                }
                None => break,
            },
            msg = rx.next() => if !matches!(msg, Some(Ok(_))) { break },
        }
    }
    let _ = tx.close().await;
}

#[derive(Deserialize)]
pub struct NewExec {
    #[serde(default)]
    cmd: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct ExecCreated {
    id: String,
}

pub async fn create_exec(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<NewExec>,
) -> ApiResult<Json<ExecCreated>> {
    access::container(&state, &current, &id, Permission::DepShell).await?;
    let cmd = body.cmd.filter(|c| !c.is_empty()).unwrap_or_else(|| {
        ["/bin/sh", "-c", "command -v bash >/dev/null && exec bash || exec sh"].map(String::from).to_vec()
    });
    let exec = state
        .docker
        .create_exec(
            &id,
            CreateExecOptions {
                attach_stdin: Some(true),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(true),
                env: Some(vec!["TERM=xterm-256color".to_owned()]),
                cmd: Some(cmd),
                ..Default::default()
            },
        )
        .await
        .map_err(docker_error)?;
    state
        .execs
        .lock()
        .unwrap()
        .insert(exec.id.clone(), ExecGrant { user_id: current.user.id.clone(), container: id });
    Ok(Json(ExecCreated { id: exec.id }))
}

fn exec_grant(state: &AppState, current: &CurrentUser, exec_id: &str) -> ApiResult<ExecGrant> {
    let execs = state.execs.lock().unwrap();
    match execs.get(exec_id) {
        Some(grant) if grant.user_id == current.user.id => Ok(grant.clone()),
        _ => Err(ApiError::new(StatusCode::NOT_FOUND, "no such shell session")),
    }
}

pub async fn exec_socket(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(exec_id): Path<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let grant = exec_grant(&state, &current, &exec_id)?;
    access::container(&state, &current, &grant.container, Permission::DepShell).await?;
    let started = state
        .docker
        .start_exec(&exec_id, Some(StartExecOptions { detach: false, tty: true, output_capacity: None }))
        .await
        .map_err(docker_error)?;
    let StartExecResults::Attached { output, input } = started else {
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "exec started detached"));
    };
    let execs = state.execs.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        pump_tty(socket, output, input).await;
        execs.lock().unwrap().remove(&exec_id);
    }))
}

pub async fn attach(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let inspect = access::container(&state, &current, &id, Permission::DepShell).await?;
    let tty = inspect.config.as_ref().and_then(|c| c.tty).unwrap_or(false);
    let open_stdin = inspect.config.as_ref().and_then(|c| c.open_stdin).unwrap_or(false);
    if !tty || !open_stdin {
        return Err(ApiError::bad_request("the container was not started with tty and stdin_open, open a shell instead"));
    }
    let options = AttachContainerOptionsBuilder::new().stream(true).stdin(true).stdout(true).stderr(true).build();
    let attached = state.docker.attach_container(&id, Some(options)).await.map_err(docker_error)?;
    Ok(ws.on_upgrade(move |socket| pump_tty(socket, attached.output, attached.input)))
}

async fn pump_tty(
    socket: WebSocket,
    mut output: std::pin::Pin<Box<dyn Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>,
    mut input: std::pin::Pin<Box<dyn AsyncWrite + Send>>,
) {
    let (mut tx, mut rx) = socket.split();
    loop {
        tokio::select! {
            item = output.next() => match item {
                Some(Ok(chunk)) => {
                    if tx.send(Message::Binary(chunk.into_bytes())).await.is_err() { break; }
                }
                _ => break,
            },
            msg = rx.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if input.write_all(text.as_bytes()).await.is_err() { break; }
                }
                Some(Ok(Message::Binary(data))) => {
                    if input.write_all(&data).await.is_err() { break; }
                }
                Some(Ok(_)) => {}
                _ => break,
            },
        }
    }
    let _ = input.shutdown().await;
    let _ = tx.close().await;
}

#[derive(Deserialize)]
pub struct Size {
    cols: u16,
    rows: u16,
}

pub async fn resize_exec(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(exec_id): Path<String>,
    Json(size): Json<Size>,
) -> ApiResult<StatusCode> {
    exec_grant(&state, &current, &exec_id)?;
    state
        .docker
        .resize_exec(&exec_id, ResizeExecOptions { height: size.rows, width: size.cols })
        .await
        .map_err(docker_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn resize_container(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(size): Json<Size>,
) -> ApiResult<StatusCode> {
    access::container(&state, &current, &id, Permission::DepShell).await?;
    let options = ResizeContainerTTYOptionsBuilder::new().h(size.rows.into()).w(size.cols.into()).build();
    state.docker.resize_container_tty(&id, options).await.map_err(docker_error)?;
    Ok(StatusCode::NO_CONTENT)
}
