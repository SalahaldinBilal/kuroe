use std::net::SocketAddr;
use std::time::Duration;

use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{CreateImageOptionsBuilder, RemoveContainerOptionsBuilder};
use futures::{SinkExt, StreamExt};
use kuroe::api::{AppState, CSRF_HEADER, router};
use kuroe::auth::{self, Role, SESSION_COOKIE};
use kuroe::db;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const IMAGE: &str = "nginx:alpine";

#[tokio::test]
#[ignore = "needs a reachable Docker daemon"]
async fn exec_round_trip_over_websocket() {
    let data_dir = std::env::temp_dir().join(format!("kuroe-exec-{}", uuid::Uuid::new_v4()));
    let pool = db::connect(&data_dir).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();
    let owner = auth::create_user(&pool, "root", "long-enough-pw", Role::owner()).await.unwrap();
    let token = auth::create_session(&pool, &owner.id).await.unwrap();
    let state = AppState::new(pool, data_dir.clone(), false).unwrap();
    let docker = state.docker.clone();

    if docker.inspect_image(IMAGE).await.is_err() {
        let mut pull = docker.create_image(Some(CreateImageOptionsBuilder::new().from_image(IMAGE).build()), None, None);
        while let Some(step) = pull.next().await {
            step.unwrap();
        }
    }
    let container = docker
        .create_container(
            None,
            ContainerCreateBody {
                image: Some(IMAGE.into()),
                cmd: Some(vec!["sleep".into(), "300".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id;
    docker.start_container(&container, None).await.unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
    });

    let cookie = format!("{SESSION_COOKIE}={token}");
    let body = reqwest::Client::new()
        .post(format!("http://{addr}/api/v1/containers/{container}/exec"))
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(r#"{ "cmd": ["/bin/sh"] }"#)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let exec: serde_json::Value = serde_json::from_str(&body).unwrap();
    let exec_id = exec["id"].as_str().unwrap();

    let mut request = format!("ws://{addr}/api/v1/exec/{exec_id}/ws").into_client_request().unwrap();
    request.headers_mut().insert("cookie", cookie.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket.send(Message::Text("echo kuroe-$((40+2))\n".into())).await.unwrap();

    let mut seen = String::new();
    let found = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(msg)) = socket.next().await {
            if let Message::Binary(data) = msg {
                seen.push_str(&String::from_utf8_lossy(&data));
                if seen.contains("kuroe-42") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    docker
        .remove_container(&container, Some(RemoveContainerOptionsBuilder::new().force(true).build()))
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&data_dir);
    assert!(found, "shell output did not contain kuroe-42, got: {seen:?}");
}
