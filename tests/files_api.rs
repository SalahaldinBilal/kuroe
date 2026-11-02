use std::net::SocketAddr;

use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{CreateImageOptionsBuilder, RemoveContainerOptionsBuilder};
use futures::StreamExt;
use kuroe::api::{AppState, router};
use kuroe::auth::{self, Role, SESSION_COOKIE};
use kuroe::db;
use serde_json::Value;

const IMAGE: &str = "busybox:1.37";

const SETUP: &str = r#"mkdir -p /t/sub "/t/with space" && echo hello > /t/a.txt && printf '<b>x</b>' > /t/page.html \
  && dd if=/dev/zero of=/t/sub/big bs=1024 count=300 2>/dev/null && printf 0123456789 > /t/clip.mp4 \
  && ln -s sub /t/linkdir"#;

#[tokio::test]
#[ignore = "needs a reachable Docker daemon"]
async fn file_browser_against_a_real_container() {
    let data_dir = std::env::temp_dir().join(format!("kuroe-files-{}", uuid::Uuid::new_v4()));
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
    let body = ContainerCreateBody { image: Some(IMAGE.into()), cmd: Some(vec!["sleep".into(), "300".into()]), ..Default::default() };
    let container = docker.create_container(None, body).await.unwrap().id;
    docker.start_container(&container, None).await.unwrap();
    let exec = docker
        .create_exec(&container, CreateExecOptions { cmd: Some(vec!["sh", "-c", SETUP]), attach_stdout: Some(true), ..Default::default() })
        .await
        .unwrap();
    if let StartExecResults::Attached { mut output, .. } = docker.start_exec(&exec.id, None).await.unwrap() {
        while output.next().await.is_some() {}
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
    });
    let client = reqwest::Client::new();
    let cookie = format!("{SESSION_COOKIE}={token}");
    let base = format!("http://{addr}/api/v1/containers/{container}/files");
    let get = |url: String| client.get(url).header("cookie", &cookie).send();

    let listing: Value = get(format!("{base}?path=/t")).await.unwrap().json().await.unwrap();
    let names: Vec<&str> = listing["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["linkdir", "sub", "with space", "a.txt", "clip.mp4", "page.html"], "{listing}");
    assert!(listing["note"].is_null(), "a shell is available, so no slow-path note: {listing}");
    assert_eq!(listing["entries"][0]["link_dir"], true);

    let missing = get(format!("{base}?path=/nope")).await.unwrap();
    assert_eq!(missing.status(), 404);

    let sizes: Value = get(format!("{base}/sizes?path=/t")).await.unwrap().json().await.unwrap();
    assert_eq!(sizes["complete"], true, "{sizes}");
    assert!(sizes["sizes"]["sub"].as_u64().unwrap() >= 300 * 1024, "{sizes}");

    let meta: Value = get(format!("{base}/preview?meta=true&path=/t/a.txt")).await.unwrap().json().await.unwrap();
    assert_eq!((meta["preview"].as_str(), meta["size"].as_u64()), (Some("text"), Some(6)), "{meta}");

    let html = get(format!("{base}/preview?path=/t/page.html")).await.unwrap();
    assert_eq!(html.headers()["content-type"], "text/plain; charset=utf-8", "markup is never rendered");
    assert_eq!(html.headers()["x-content-type-options"], "nosniff");
    assert_eq!(html.text().await.unwrap(), "<b>x</b>");

    let cut = get(format!("{base}/preview?path=/t/a.txt&max=3")).await.unwrap();
    assert_eq!(cut.headers()["x-kuroe-size"], "6");
    assert_eq!(cut.text().await.unwrap(), "hel");

    let download = get(format!("{base}/download?path=/t/sub/big")).await.unwrap();
    assert_eq!(download.headers()["content-length"], (300 * 1024).to_string());
    assert_eq!(download.bytes().await.unwrap().len(), 300 * 1024);
    let folder = get(format!("{base}/download?path=/t/sub")).await.unwrap();
    assert_eq!(folder.headers()["content-type"], "application/x-tar");

    let ranged = client.get(format!("{base}/media?path=/t/clip.mp4")).header("cookie", &cookie).header("range", "bytes=2-4").send().await.unwrap();
    assert_eq!(ranged.status(), 206);
    assert_eq!(ranged.headers()["content-range"], "bytes 2-4/10");
    assert_eq!(ranged.text().await.unwrap(), "234");
    let dropped = client.delete(format!("{base}/media?path=/t/clip.mp4")).header("cookie", &cookie).header("x-kuroe-csrf", "1").send().await.unwrap();
    assert_eq!(dropped.status(), 204);

    docker.stop_container(&container, None).await.unwrap();
    let stopped: Value = get(format!("{base}?path=/t")).await.unwrap().json().await.unwrap();
    assert_eq!(stopped["entries"].as_array().unwrap().len(), 6, "{stopped}");
    assert_eq!(stopped["note"], "The container is stopped, large folders load slowly.");

    docker
        .remove_container(&container, Some(RemoveContainerOptionsBuilder::new().force(true).build()))
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&data_dir);
}
