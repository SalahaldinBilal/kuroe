use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{AppState, CSRF_HEADER, router};
use crate::auth::{self, Role};
use crate::db::test_pool;

async fn app() -> (Router, AppState) {
    let state = AppState::new(test_pool().await, std::env::temp_dir().join("kuroe-test"), true).unwrap();
    let app = router(state.clone()).layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 1], 5000))));
    (app, state)
}

fn request(method: Method, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri).header(CSRF_HEADER, "1");
    if let Some(token) = token {
        builder = builder.header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE));
    }
    match body {
        Some(body) => builder.header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Option<String>, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let cookie = res.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().to_owned());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, cookie, body)
}

fn token_from(cookie: &str) -> String {
    cookie.split(';').next().unwrap().split_once('=').unwrap().1.to_owned()
}

#[tokio::test]
async fn first_run_setup_needs_the_code_then_locks() {
    let (app, state) = app().await;
    let data_dir = &state.runner.data_dir;
    let (_, _, body) = send(&app, request(Method::GET, "/api/v1/setup", None, None)).await;
    assert_eq!(body["needed"], true);
    let code = auth::setup_code::get_or_create(data_dir).unwrap();

    for wrong in [json!({ "username": "root", "password": "long-enough-pw" }), json!({ "username": "root", "password": "long-enough-pw", "code": "AAAA-AAAA-AAAA" })] {
        let (status, _, body) = send(&app, request(Method::POST, "/api/v1/setup", None, Some(wrong))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["error"].as_str().unwrap().contains("kuroe admin setup-code"));
    }

    let creds = json!({ "username": "root", "password": "long-enough-pw", "code": code.to_ascii_lowercase() });
    let (status, cookie, body) = send(&app, request(Method::POST, "/api/v1/setup", None, Some(creds.clone()))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["role"], "owner");
    let cookie = cookie.unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("Secure") && cookie.contains("SameSite=Lax"));
    assert!(!auth::setup_code::path(data_dir).exists(), "the code is gone once used");

    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/setup", None, Some(creds))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, _, body) = send(&app, request(Method::GET, "/api/v1/setup", None, None)).await;
    assert_eq!(body["needed"], false);
}

#[tokio::test]
async fn preview_root_reaches_the_proxy_not_the_spa() {
    let (app, _) = app().await;
    for uri in ["/preview/c1/80/", "/preview/c1/80/assets/app.js"] {
        let (status, _, _) = send(&app, request(Method::GET, uri, None, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn login_logout_and_bad_password() {
    let (app, state) = app().await;
    auth::create_user(&state.db, "dev", "dev-password-1", Role::deployer()).await.unwrap();

    let bad = json!({ "username": "dev", "password": "nope-nope-nope" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/login", None, Some(bad))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let good = json!({ "username": "dev", "password": "dev-password-1" });
    let (status, cookie, _) = send(&app, request(Method::POST, "/api/v1/login", None, Some(good))).await;
    assert_eq!(status, StatusCode::OK);
    let token = token_from(&cookie.unwrap());

    let (status, _, body) = send(&app, request(Method::GET, "/api/v1/me", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "dev");

    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/logout", Some(&token), None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = send(&app, request(Method::GET, "/api/v1/me", Some(&token), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_session_is_rejected() {
    let (app, state) = app().await;
    let user = auth::create_user(&state.db, "old", "old-password-1", Role::viewer()).await.unwrap();
    let token = auth::create_session(&state.db, &user.id).await.unwrap();
    sqlx::query("UPDATE sessions SET expires_at = 0").execute(&state.db).await.unwrap();
    let (status, _, _) = send(&app, request(Method::GET, "/api/v1/me", Some(&token), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn state_change_without_csrf_header_is_refused() {
    let (app, _) = app().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "username": "root", "password": "long-enough-pw" }).to_string()))
        .unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn role_matrix() {
    let (app, state) = app().await;
    let expected = [
        (Role::owner(), StatusCode::CREATED, StatusCode::OK, StatusCode::NO_CONTENT),
        (Role::admin(), StatusCode::FORBIDDEN, StatusCode::OK, StatusCode::NO_CONTENT),
        (Role::deployer(), StatusCode::FORBIDDEN, StatusCode::OK, StatusCode::FORBIDDEN),
        (Role::viewer(), StatusCode::FORBIDDEN, StatusCode::OK, StatusCode::FORBIDDEN),
    ];
    for (role, create_user, read_settings, write_settings) in expected {
        let name = role.as_str();
        let user = auth::create_user(&state.db, name, "role-password-1", role.clone()).await.unwrap();
        let token = auth::create_session(&state.db, &user.id).await.unwrap();

        let new_user = json!({ "username": format!("made-by-{name}"), "password": "new-user-pw-1", "role": "viewer" });
        let (status, _, _) = send(&app, request(Method::POST, "/api/v1/users", Some(&token), Some(new_user))).await;
        assert_eq!(status, create_user, "{name} create-user");

        let (status, _, _) = send(&app, request(Method::GET, "/api/v1/settings", Some(&token), None)).await;
        assert_eq!(status, read_settings, "{name} read-settings");

        let settings = json!({ "instance_name": name });
        let (status, _, _) = send(&app, request(Method::PUT, "/api/v1/settings", Some(&token), Some(settings))).await;
        assert_eq!(status, write_settings, "{name} write-settings");
    }
}

#[tokio::test]
async fn empty_number_settings_mean_not_set() {
    let (app, state) = app().await;
    let user = auth::create_user(&state.db, "root", "root-password-1", Role::owner()).await.unwrap();
    let token = auth::create_session(&state.db, &user.id).await.unwrap();
    let empty = json!({
        "default_cpus": "", "default_memory_mb": "", "default_pids": "",
        "pool_cpus": "", "pool_memory_mb": " ", "pool_storage_gb": "",
        "proxy_http_port": "", "drain_seconds": "",
    });
    let (status, _, _) = send(&app, request(Method::PUT, "/api/v1/settings", Some(&token), Some(empty))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, body) = send(&app, request(Method::GET, "/api/v1/pool", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["cpus_counted"].is_number() && body["memory_counted"].is_number(), "{body}");
    let (status, _, body) = send(&app, request(Method::GET, "/api/v1/proxy", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settings"]["http_port"], 80);
}

fn bearer(method: Method, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn deploy_token_is_scoped_to_its_deployment() {
    use crate::deploy::{self, Source, Spec, tokens};
    let (app, state) = app().await;
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let a = deploy::create(&state.db, "api", &Spec::new(Source::image("ghcr.io/acme/api:v1")), &owner).await.unwrap();
    let b = deploy::create(&state.db, "web", &Spec::new(Source::Compose { file: "services: {}".into() }), &owner).await.unwrap();
    let (_, token_a) = tokens::create(&state.db, &a.id, "ci", false, &owner.id).await.unwrap();

    let (status, _, body) = send(&app, bearer(Method::POST, &format!("/api/v1/deployments/{}/deploy", b.id), &token_a, json!({}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, _, _) = send(&app, bearer(Method::POST, &format!("/api/v1/deployments/{}/deploy", a.id), "kuroe_nope", json!({}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let empty = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/v1/deployments/{}/deploy", b.id))
        .header(CSRF_HEADER, "1")
        .header(header::COOKIE, format!("{}={}", auth::SESSION_COOKIE, auth::create_session(&state.db, &owner.id).await.unwrap()))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(&app, empty).await;
    assert_eq!(status, StatusCode::ACCEPTED, "an empty JSON body means no tag: {body}");

    let (status, _, body) =
        send(&app, bearer(Method::POST, &format!("/api/v1/deployments/{}/deploy", a.id), &token_a, json!({ "tag": "v2" }))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let updated = deploy::get(&state.db, &a.id).await.unwrap().unwrap();
    assert_eq!(updated.spec.source, Source::image("ghcr.io/acme/api:v2"));

    let (status, _, _) = send(&app, bearer(Method::GET, &format!("/api/v1/deployments/{}", a.id), &token_a, json!({}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a deploy token is not a session");
}

async fn mcp(app: &Router, token: &str, method: &str, params: Value) -> (StatusCode, Value) {
    let message = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let (status, _, body) = send(app, bearer(Method::POST, "/api/v1/mcp", token, message)).await;
    (status, body)
}

async fn call_tool(app: &Router, token: &str, name: &str, arguments: Value) -> (bool, String) {
    let (status, body) = mcp(app, token, "tools/call", json!({ "name": name, "arguments": arguments })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let result = &body["result"];
    (result["isError"] == true, result["content"][0]["text"].as_str().unwrap_or_default().to_owned())
}

#[tokio::test]
async fn cancelling_needs_the_deploy_permission_and_a_running_deploy() {
    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let boss = auth::create_session(&state.db, &owner.id).await.unwrap();
    let viewer = auth::create_user(&state.db, "look", "look-password-1", Role::viewer()).await.unwrap();
    let look = auth::create_session(&state.db, &viewer.id).await.unwrap();
    let d = deploy::create(&state.db, "shop", &Spec::new(Source::image("nginx:1.29")), &owner).await.unwrap();
    let uri = format!("/api/v1/deployments/{}/deploy/cancel", d.id);

    let (status, _, _) = send(&app, request(Method::POST, &uri, Some(&look), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = send(&app, request(Method::POST, &uri, Some(&boss), None)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "no deploy is running");
}

#[tokio::test]
async fn preview_requests_need_a_session_and_the_csrf_header() {
    let (app, state) = app().await;
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let boss = auth::create_session(&state.db, &owner.id).await.unwrap();
    let body = json!({ "port": 80, "method": "GET", "path": "/" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/containers/c1/request", None, Some(body.clone()))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let no_csrf = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/containers/c1/request")
        .header(header::COOKIE, format!("{}={boss}", auth::SESSION_COOKIE))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, _, _) = send(&app, no_csrf).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn deploy_history_pages_and_stays_inside_its_deployment() {
    use crate::deploy::history::{self, DeploySource, Started};
    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    sqlx::query("INSERT INTO roles (name, permissions, scope) VALUES ('elsewhere', '[\"dep.view\"]', '[\"another-deployment\"]')")
        .execute(&state.db)
        .await
        .unwrap();
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let boss = auth::create_session(&state.db, &owner.id).await.unwrap();
    let outsider = auth::create_user(&state.db, "out", "out-password-1", Role("elsewhere".into())).await.unwrap();
    let out = auth::create_session(&state.db, &outsider.id).await.unwrap();
    let shop = deploy::create(&state.db, "shop", &Spec::new(Source::image("nginx:1.29")), &owner).await.unwrap();
    let blog = deploy::create(&state.db, "blog", &Spec::new(Source::image("nginx:1.29")), &owner).await.unwrap();
    let mut ids = Vec::new();
    for _ in 0..3 {
        let begun = deploy::begin_deploy(&state.db, &shop.id, &Started::user(&owner), &DeploySource::of(&shop.spec))
            .await
            .unwrap()
            .unwrap();
        history::close(&state.db, &begun.deploy, "running", "pulled nginx", &[]).await.unwrap();
        deploy::finish(&state.db, &shop.id, "running", "pulled nginx").await.unwrap();
        ids.push(begun.deploy);
    }

    let page = format!("/api/v1/deployments/{}/deploys?limit=2", shop.id);
    let (status, _, body) = send(&app, request(Method::GET, &page, Some(&boss), None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let revisions: Vec<i64> = body["deploys"].as_array().unwrap().iter().map(|d| d["revision"].as_i64().unwrap()).collect();
    assert_eq!((revisions, body["more"].as_bool()), (vec![3, 2], Some(true)));
    assert!(body["deploys"][0].get("output").is_none(), "the list leaves output out");
    assert_eq!(body["deploys"][0]["source"]["image"], "nginx:1.29");
    assert_eq!(body["deploys"][0]["trigger"], "ui");

    let one = format!("/api/v1/deployments/{}/deploys/{}", shop.id, ids[0]);
    let (status, _, body) = send(&app, request(Method::GET, &one, Some(&boss), None)).await;
    assert_eq!((status, body["output"].as_str()), (StatusCode::OK, Some("pulled nginx")));
    let wrong = format!("/api/v1/deployments/{}/deploys/{}", blog.id, ids[0]);
    let (status, _, _) = send(&app, request(Method::GET, &wrong, Some(&boss), None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a deploy only shows under its own deployment");
    let (status, _, _) = send(&app, request(Method::GET, &one, Some(&out), None)).await;
    assert_ne!(status, StatusCode::OK, "a list-scoped role outside the deployment sees nothing");
}

#[tokio::test]
async fn volume_limits_show_who_is_over_and_need_the_permission() {
    use crate::deploy::{self, Source, Spec};
    use crate::docker::rewrite::{ServiceSettings, SharedMount};
    let (app, state) = app().await;
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let boss = auth::create_session(&state.db, &owner.id).await.unwrap();
    let mut spec = Spec::new(Source::image("nginx:1.29"));
    let mount = SharedMount { volume: "uploads".into(), target: "/data".into(), read_only: false };
    spec.services.insert("app".into(), ServiceSettings { volumes: vec![mount], ..Default::default() });
    let shop = deploy::create(&state.db, "shop", &spec, &owner).await.unwrap();
    sqlx::query("INSERT INTO volumes (name, created_at) VALUES ('uploads', 0)").execute(&state.db).await.unwrap();
    for (name, project, bytes) in [("kuroe_uploads", "", 3 << 20), ("kuroe_shop_db", "kuroe_shop", 1 << 20)] {
        sqlx::query("INSERT INTO volume_sizes (name, project, bytes, measured_at) VALUES (?, ?, ?, 100)")
            .bind(name)
            .bind(project)
            .bind(bytes)
            .execute(&state.db)
            .await
            .unwrap();
    }
    let viewer = auth::create_user(&state.db, "look", "look-password-1", Role::viewer()).await.unwrap();
    let look = auth::create_session(&state.db, &viewer.id).await.unwrap();

    let limit = json!({ "max_bytes": 2 << 20, "stop": true });
    let (status, _, _) = send(&app, request(Method::PUT, "/api/v1/volume-limits/kuroe_uploads", Some(&look), Some(limit.clone()))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(&app, request(Method::PUT, "/api/v1/volume-limits/kuroe_nothing", Some(&boss), Some(limit.clone()))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let zero = json!({ "max_bytes": 0, "stop": false });
    let (status, _, _) = send(&app, request(Method::PUT, "/api/v1/volume-limits/kuroe_uploads", Some(&boss), Some(zero))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(&app, request(Method::PUT, "/api/v1/volume-limits/kuroe_uploads", Some(&boss), Some(limit))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, _, body) = send(&app, request(Method::GET, "/api/v1/volumes", Some(&look), None)).await;
    let usage = &body[0]["usage"];
    assert_eq!((usage["bytes"].as_i64(), usage["over"].as_bool()), (Some(3 << 20), Some(true)), "{body}");
    assert_eq!(usage["limit"], json!({ "max_bytes": 2 << 20, "stop": true }));
    assert_eq!(usage["deployments"][0]["name"], "shop");

    let (_, _, body) = send(&app, request(Method::GET, "/api/v1/deployment-volumes", Some(&look), None)).await;
    assert_eq!(body.as_array().unwrap().len(), 1, "{body}");
    assert_eq!(body[0]["name"], "kuroe_shop_db");

    let (_, _, body) = send(&app, request(Method::GET, &format!("/api/v1/deployments/{}/volumes", shop.id), Some(&look), None)).await;
    let names: Vec<&str> = body.as_array().unwrap().iter().map(|v| v["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["kuroe_uploads", "kuroe_shop_db"]);

    let (status, _, _) = send(&app, request(Method::DELETE, "/api/v1/volume-limits/kuroe_uploads", Some(&boss), None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, body) = send(&app, request(Method::GET, "/api/v1/volumes", Some(&look), None)).await;
    assert_eq!((body[0]["usage"]["limit"].is_null(), body[0]["usage"]["over"].as_bool()), (true, Some(false)));
}

#[tokio::test]
async fn metrics_by_deployment_only_show_what_the_user_can_see() {
    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let api = deploy::create(&state.db, "api", &Spec::new(Source::image("nginx:1.29")), &owner).await.unwrap();
    let hidden = deploy::create(&state.db, "hidden", &Spec::new(Source::image("nginx:1.29")), &owner).await.unwrap();
    let now = crate::db::now();
    for (i, (deployment, container)) in [(&api.id, "c1"), (&api.id, "c2"), (&hidden.id, "c3")].into_iter().enumerate() {
        sqlx::query("INSERT INTO metric_series (id, deployment_id, service, container_id) VALUES (?, ?, 'app', ?)")
            .bind(i as i64 + 1)
            .bind(deployment)
            .bind(container)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO container_metrics VALUES (?, ?, 25.0, 1000, 0, 0, 0, 0, 0)")
            .bind(i as i64 + 1)
            .bind(now - 30)
            .execute(&state.db)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO roles (name, builtin, description, permissions, scope) VALUES ('looker', 0, '', '[\"dep.view\"]', ?)")
        .bind(format!("[\"{}\"]", api.id))
        .execute(&state.db)
        .await
        .unwrap();
    let looker = auth::create_user(&state.db, "looker", "looker-password-1", Role("looker".into())).await.unwrap();
    let session = auth::create_session(&state.db, &looker.id).await.unwrap();

    let uri = format!("/api/v1/metrics/deployments?from={}&to={}", now - 3600, now);
    let (status, _, body) = send(&app, request(Method::GET, &uri, Some(&session), None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = body["deployments"].as_array().unwrap();
    assert_eq!(list.len(), 1, "only deployments the user can see: {body}");
    assert_eq!(list[0]["name"], "api");
    assert_eq!(list[0]["points"][0]["cpu"], 50.0, "its two containers are summed");

    let uri = format!("/api/v1/deployments/{}/metrics?from={}&to={}", api.id, now - 3600, now);
    let (_, _, body) = send(&app, request(Method::GET, &uri, Some(&session), None)).await;
    let containers: Vec<&str> = body["containers"].as_array().unwrap().iter().map(|c| c["container_id"].as_str().unwrap()).collect();
    assert_eq!(containers, ["c1", "c2"]);
    assert_eq!(body["services"]["app"][0]["cpu"], 50.0);
}

#[tokio::test]
async fn mcp_reads_as_the_token_user_and_tokens_work_nowhere_else() {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let mut spec = Spec::new(Source::image("ghcr.io/acme/api:v1"));
    spec.container.env = BTreeMap::from([("DB_PASSWORD".into(), "hunter2-secret".into()), ("LOG".into(), "info".into())]);
    spec.secrets = BTreeSet::from(["DB_PASSWORD".into()]);
    let api = deploy::create(&state.db, "api", &spec, &owner).await.unwrap();
    deploy::create(&state.db, "hidden", &Spec::new(Source::image("nginx:1.29")), &owner).await.unwrap();
    crate::proxy::create_domain(&state.db, &api.id, "api.example.com", "app", 8080, true).await.unwrap();

    sqlx::query("INSERT INTO roles (name, builtin, description, permissions, scope) VALUES ('looker', 0, '', '[\"dep.view\"]', ?)")
        .bind(format!("[\"{}\"]", api.id))
        .execute(&state.db)
        .await
        .unwrap();
    let looker = auth::create_user(&state.db, "looker", "looker-password-1", Role("looker".into())).await.unwrap();
    let session = auth::create_session(&state.db, &looker.id).await.unwrap();

    let new_token = json!({ "name": "claude", "expires_days": 30 });
    let (status, _, body) = send(&app, request(Method::POST, "/api/v1/me/api-tokens", Some(&session), Some(new_token))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["value"].as_str().unwrap().to_owned();
    assert!(token.starts_with("kuroe_pat_"));
    assert!(body["token"]["expires_at"].is_number());

    let (status, body) = mcp(&app, &token, "initialize", json!({ "protocolVersion": "2025-06-18" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(body["result"]["serverInfo"]["name"], "kuroe");
    let (_, body) = mcp(&app, &token, "tools/list", json!({})).await;
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 8);
    let (_, body) = mcp(&app, &token, "deployments/delete", json!({})).await;
    assert_eq!(body["error"]["code"], -32601);
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let (status, _, _) = send(&app, bearer(Method::POST, "/api/v1/mcp", &token, note)).await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (error, text) = call_tool(&app, &token, "list_deployments", json!({})).await;
    assert!(!error, "{text}");
    assert!(text.contains("\"api\"") && text.contains("api.example.com"), "{text}");
    assert!(!text.contains("hidden"), "only deployments the user can see: {text}");

    let (error, text) = call_tool(&app, &token, "get_deployment", json!({ "deployment": "api" })).await;
    assert!(!error, "{text}");
    assert!(text.contains("DB_PASSWORD") && !text.contains("hunter2-secret"), "secret values stay out: {text}");
    let (error, text) = call_tool(&app, &token, "get_deployment", json!({ "deployment": "hidden" })).await;
    assert!(error && text.contains("no deployment named hidden"), "{text}");
    let (error, text) = call_tool(&app, &token, "deployment_logs", json!({ "deployment": api.id })).await;
    assert!(error && text.contains("secrets"), "logs need dep.secrets: {text}");
    let (error, text) = call_tool(&app, &token, "proxy_routes", json!({})).await;
    assert!(!error && text.contains("api.example.com"), "{text}");
    let (error, text) = call_tool(&app, &token, "last_deploy", json!({ "deployment": "api" })).await;
    assert!(!error && text.contains("\"status\""), "{text}");
    let (error, text) = call_tool(&app, &token, "deploy_history", json!({ "deployment": "api" })).await;
    assert!(!error && text.contains("\"deploys\": []"), "{text}");
    let window = json!({ "deployment": "api", "from": "2026-11-01T22:15:00Z", "to": "2026-11-02T01:40:00Z" });
    let (error, text) = call_tool(&app, &token, "deployment_metrics", window).await;
    assert!(!error && text.contains("2026-11-01T22:15:00Z"), "{text}");
    let (error, _) = call_tool(&app, &token, "deployment_metrics", json!({ "deployment": "api", "from": "last week" })).await;
    assert!(error);

    let session_metrics = |uri: &str| request(Method::GET, uri, Some(&session), None);
    let (status, _, body) = send(&app, session_metrics(&format!("/api/v1/deployments/{}/metrics?from=1000&to=6400", api.id))).await;
    assert_eq!((status, body["bucket"].as_i64()), (StatusCode::OK, Some(22)), "any window: {body}");
    let (status, _, _) = send(&app, session_metrics("/api/v1/metrics/host?from=6400&to=1000")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(&app, session_metrics("/api/v1/metrics/host?range=24h")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "presets are only in the UI");
    let (error, text) = call_tool(&app, &token, "server_overview", json!({})).await;
    assert!(!error && text.contains("kuroe_version"), "{text}");
    let (error, _) = call_tool(&app, &token, "deploy", json!({ "deployment": "api" })).await;
    assert!(error, "there is no tool that changes anything");

    let (status, _, _) = send(&app, bearer(Method::GET, "/api/v1/deployments", &token, json!({}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a personal token only works on /mcp");
    let (status, _, _) = send(&app, bearer(Method::POST, &format!("/api/v1/deployments/{}/deploy", api.id), &token, json!({}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(&app, bearer(Method::POST, "/api/v1/me/api-tokens", &token, json!({ "name": "more" }))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a token can't make tokens");
    let (status, _) = mcp(&app, "kuroe_pat_nope", "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let no_token = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string()))
        .unwrap();
    let (status, _, _) = send(&app, no_token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token is 401, not a CSRF refusal");

    let owner_session = auth::create_session(&state.db, &owner.id).await.unwrap();
    let (_, _, body) = send(&app, request(Method::POST, "/api/v1/me/api-tokens", Some(&owner_session), Some(json!({ "name": "boss" })))).await;
    let owner_token = body["value"].as_str().unwrap().to_owned();
    let (error, text) = call_tool(&app, &owner_token, "get_deployment", json!({ "deployment": "api" })).await;
    assert!(!error && !text.contains("hunter2-secret"), "even a user who may see secrets doesn't get them over MCP: {text}");
    assert!(text.contains("\"LOG\": \"info\""), "plain values stay: {text}");

    let (_, _, list) = send(&app, request(Method::GET, "/api/v1/me/api-tokens", Some(&session), None)).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(list[0].get("value").is_none() && list[0]["last_used_at"].is_number(), "{list}");
    let (status, _, _) = send(&app, request(Method::GET, "/api/v1/api-tokens", Some(&session), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only the owner lists everyone's tokens");
    let (_, _, all) = send(&app, request(Method::GET, "/api/v1/api-tokens", Some(&owner_session), None)).await;
    assert_eq!(all.as_array().unwrap().len(), 2, "{all}");
    assert_eq!(all[1]["username"], "looker");

    let id = list[0]["id"].as_str().unwrap();
    let (status, _, _) = send(&app, request(Method::DELETE, &format!("/api/v1/me/api-tokens/{id}"), Some(&session), None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = mcp(&app, &token, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a revoked token stops working");
}

#[tokio::test]
async fn kuroe_gets_its_own_domain_and_the_public_url_follows() {
    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let viewer = auth::create_user(&state.db, "vera", "vera-password-1", Role::viewer()).await.unwrap();
    let session = auth::create_session(&state.db, &owner.id).await.unwrap();
    let viewer_session = auth::create_session(&state.db, &viewer.id).await.unwrap();
    let shop = deploy::create(&state.db, "shop", &Spec::new(Source::image("nginx:1.29")), &owner).await.unwrap();
    crate::proxy::create_domain(&state.db, &shop.id, "shop.example.com", "app", 80, true).await.unwrap();
    let put = |s: &str, body: Value| request(Method::PUT, "/api/v1/proxy/ui", Some(s), Some(body));

    let (status, _, _) = send(&app, put(&viewer_session, json!({ "hostname": "kuroe.example.com" }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = send(&app, put(&session, json!({ "hostname": "shop.example.com" }))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"].as_str().unwrap().contains("shop"));
    let (status, _, _) = send(&app, put(&session, json!({ "hostname": "not a host" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, body) = send(&app, put(&session, json!({ "hostname": " Kuroe.Example.com. ", "tls": true }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["public_url"], "https://kuroe.example.com", "an empty public URL follows the domain");
    let (_, _, settings) = send(&app, request(Method::GET, "/api/v1/proxy", Some(&session), None)).await;
    assert_eq!(settings["ui"], json!({ "hostname": "kuroe.example.com", "tls": true }));

    let add = json!({ "hostname": "kuroe.example.com", "service": "app", "port": 80 });
    let (status, _, body) = send(&app, request(Method::POST, &format!("/api/v1/deployments/{}/domains", shop.id), Some(&session), Some(add))).await;
    assert_eq!(status, StatusCode::CONFLICT, "a deployment can't take kuroe's domain: {body}");

    let (_, _, body) = send(&app, put(&session, json!({ "hostname": "panel.example.com", "tls": false }))).await;
    assert_eq!(body["public_url"], "http://panel.example.com", "a public URL that matched the old domain moves with it");

    let custom = json!({ "public_url": "https://somewhere.else" });
    send(&app, request(Method::PUT, "/api/v1/settings", Some(&session), Some(custom))).await;
    let (_, _, body) = send(&app, put(&session, json!({ "hostname": "kuroe.example.com" }))).await;
    assert_eq!(body["public_url"], "https://somewhere.else", "a public URL someone set stays");

    let (status, _, body) = send(&app, put(&session, json!({ "hostname": "" }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ui"], Value::Null);
    let (_, _, settings) = send(&app, request(Method::GET, "/api/v1/proxy", Some(&session), None)).await;
    assert_eq!(settings["ui"], Value::Null);
}

#[test]
fn status_follows_the_containers_not_the_last_deploy() {
    use super::deployments::{Observed, live_status};
    let observed = Observed::new;
    let up = || observed("running", None);
    assert_eq!(live_status("running", &[up(), up()], false), "running");
    assert_eq!(live_status("running", &[], false), "down", "no containers at all");
    assert_eq!(live_status("running", &[observed("exited", Some(0))], false), "down");
    assert_eq!(live_status("running", &[up(), observed("exited", Some(1))], false), "degraded");
    assert_eq!(live_status("running", &[up(), observed("restarting", None)], false), "degraded");
    assert_eq!(live_status("running", &[up(), observed("exited", Some(0))], false), "running", "a finished one-shot job is fine");
    assert_eq!(live_status("running", &[up(), observed("exited", Some(137))], true), "running", "swarm keeps old task history");
    assert_eq!(live_status("stopped", &[], false), "stopped");
    assert_eq!(live_status("failed", &[up()], false), "failed");
    assert_eq!(live_status("deploying", &[], false), "deploying");
}

#[test]
fn stack_mode_is_refused_without_swarm() {
    use super::deployments::check_spec;
    use crate::deploy::spec::RunMode;
    use crate::deploy::{Source, Spec};
    let mut spec = Spec::new(Source::Compose { file: "services: {}".into() });
    assert!(check_spec(&spec, false).is_ok());
    spec.mode = RunMode::Stack;
    assert!(check_spec(&spec, false).unwrap_err().contains("not a Swarm manager"));
    assert!(check_spec(&spec, true).is_ok());
}

#[test]
fn network_limits_must_be_positive() {
    use super::deployments::check_spec;
    use crate::deploy::{Source, Spec};
    use crate::docker::rewrite::ServiceSettings;
    let mut spec = Spec::new(Source::Image { image: "nginx:1.29".into(), registry_identity: None });
    spec.services.insert("app".into(), ServiceSettings { net_in_mbit: Some(20.0), net_out_mbit: Some(0.5), ..Default::default() });
    assert!(check_spec(&spec, false).is_ok());
    spec.services.get_mut("app").unwrap().net_out_mbit = Some(0.0);
    assert!(check_spec(&spec, false).unwrap_err().contains("upload limit"));
    spec.services.get_mut("app").unwrap().net_out_mbit = None;
    spec.services.get_mut("app").unwrap().net_in_mbit = Some(-1.0);
    assert!(check_spec(&spec, false).unwrap_err().contains("download limit"));
    spec.services.get_mut("app").unwrap().net_in_mbit = None;
    spec.services.get_mut("app").unwrap().net_conn_out_mbit = Some(0.0);
    assert!(check_spec(&spec, false).unwrap_err().contains("per-connection upload limit"));
}

#[tokio::test]
async fn owner_can_create_each_role() {
    let (app, state) = app().await;
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let token = auth::create_session(&state.db, &owner.id).await.unwrap();
    for role in Role::BUILTIN.map(|r| Role(r.into())) {
        let body = json!({ "username": format!("u-{}", role.as_str()), "password": "some-password-1", "role": role });
        let (status, _, created) = send(&app, request(Method::POST, "/api/v1/users", Some(&token), Some(body))).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["role"], role.as_str());
    }
    let dup = json!({ "username": "u-admin", "password": "some-password-1", "role": "admin" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/users", Some(&token), Some(dup))).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn custom_roles_can_be_made_renamed_and_only_removed_when_unused() {
    let (app, state) = app().await;
    let (_, owner) = signed_in(&state, "boss", Role::owner()).await;
    let role = json!({ "name": "on-call", "description": "Night shifts", "permissions": ["dep.view", "dep.deploy"], "scope": "all" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/roles", Some(&owner), Some(role.clone()))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/roles", Some(&owner), Some(role))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (user, token) = signed_in(&state, "night", Role("on-call".into())).await;
    let (_, _, me) = send(&app, request(Method::GET, "/api/v1/me", Some(&token), None)).await;
    assert_eq!(me["permissions"], json!(["dep.view", "dep.deploy"]));

    let renamed = json!({ "name": "pager", "permissions": ["dep.view"], "scope": "all" });
    let (status, _, _) = send(&app, request(Method::PUT, "/api/v1/roles/on-call", Some(&owner), Some(renamed))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id = ?").bind(&user.id).fetch_one(&state.db).await.unwrap();
    assert_eq!(role, "pager", "renaming a role moves its users along");

    let (status, _, _) = send(&app, request(Method::DELETE, "/api/v1/roles/pager", Some(&owner), None)).await;
    assert_eq!(status, StatusCode::CONFLICT, "still in use");
    let builtin = json!({ "name": "viewer", "permissions": ["dep.view", "dep.shell"], "scope": "all" });
    let (status, _, _) = send(&app, request(Method::PUT, "/api/v1/roles/viewer", Some(&owner), Some(builtin))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(&app, request(Method::GET, "/api/v1/roles", Some(&token), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    send(&app, request(Method::DELETE, &format!("/api/v1/users/{}", user.id), Some(&owner), None)).await;
    let (status, _, _) = send(&app, request(Method::DELETE, "/api/v1/roles/pager", Some(&owner), None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn users_permission_does_not_reach_owners() {
    let (app, state) = app().await;
    let (boss, _) = signed_in(&state, "boss", Role::owner()).await;
    sqlx::query("INSERT INTO roles (name, permissions) VALUES ('people', '[\"users\"]')").execute(&state.db).await.unwrap();
    let (_, hr) = signed_in(&state, "hr", Role("people".into())).await;

    let owner = json!({ "username": "sneaky", "password": "sneaky-password-1", "role": "owner" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/users", Some(&hr), Some(owner))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let viewer = json!({ "username": "fine", "password": "fine-password-12", "role": "viewer" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/users", Some(&hr), Some(viewer))).await;
    assert_eq!(status, StatusCode::CREATED);
    let ghost = json!({ "username": "ghosty", "password": "ghost-password-1", "role": "ghost" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/users", Some(&hr), Some(ghost))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let reset = json!({ "password": "taken-over-pw-1" });
    let (status, _, _) = send(&app, request(Method::POST, &format!("/api/v1/users/{}/password", boss.id), Some(&hr), Some(reset))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn grants_with_roles_show_up_in_deployment_access() {
    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    let (boss, owner) = signed_in(&state, "boss", Role::owner()).await;
    let (viewer, _) = signed_in(&state, "eye", Role::viewer()).await;
    let d = deploy::create(&state.db, "shop", &Spec::new(Source::image("nginx:1.29")), &boss).await.unwrap();

    let uri = format!("/api/v1/deployments/{}/grants/{}", d.id, viewer.id);
    let (status, _, _) = send(&app, request(Method::PUT, &uri, Some(&owner), Some(json!({ "role": "deployer" })))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, reach) = send(&app, request(Method::GET, &format!("/api/v1/deployments/{}/access", d.id), Some(&owner), None)).await;
    assert_eq!(
        reach,
        json!([
            { "user_id": boss.id, "username": "boss", "role": "owner", "via": "owner" },
            { "user_id": viewer.id, "username": "eye", "role": "deployer", "via": "grant" },
        ])
    );
    let (_, _, grants) = send(&app, request(Method::GET, &format!("/api/v1/users/{}/grants", viewer.id), Some(&owner), None)).await;
    assert_eq!(grants, json!([{ "deployment_id": d.id, "role": "deployer" }]));

    let on_owner = format!("/api/v1/deployments/{}/grants/{}", d.id, boss.id);
    let (status, _, _) = send(&app, request(Method::PUT, &on_owner, Some(&owner), Some(json!({ "role": "viewer" })))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn identities_keep_their_secret_and_guard_their_host() {
    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    let (boss, owner) = signed_in(&state, "boss", Role::owner()).await;
    let (_, viewer) = signed_in(&state, "eye", Role::viewer()).await;

    let login = json!({ "kind": "registry", "name": "ghcr", "host": "https://GHCR.io/", "username": "acme", "secret": "pass-123" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/identities", Some(&viewer), Some(login.clone()))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, created) = send(&app, request(Method::POST, "/api/v1/identities", Some(&owner), Some(login))).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap().to_owned();
    let git = json!({ "kind": "github_token", "name": "gh", "secret": "ghp_x" });
    let (_, _, gh) = send(&app, request(Method::POST, "/api/v1/identities", Some(&owner), Some(git))).await;
    let gh = gh["id"].as_str().unwrap().to_owned();

    let (_, _, list) = send(&app, request(Method::GET, "/api/v1/identities", Some(&owner), None)).await;
    assert!(!list.to_string().contains("pass-123"), "secrets never leave: {list}");
    assert_eq!(list[1]["host"], "ghcr.io");

    let wrong_host = json!({ "name": "api", "spec": { "source": { "kind": "git", "url": "https://evil.example.com/a.git", "identity": gh } } });
    let (status, _, body) = send(&app, request(Method::POST, "/api/v1/deployments", Some(&owner), Some(wrong_host))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "gh is for github.com, but the repository is on evil.example.com");
    let not_registry = json!({ "name": "api", "spec": { "source": { "kind": "image", "image": "x", "registry_identity": gh } } });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/deployments", Some(&owner), Some(not_registry))).await;
    assert_eq!(status, StatusCode::CREATED, "a GitHub token can pull from ghcr.io");

    let spec = Spec::new(Source::Image { image: "ghcr.io/acme/api".into(), registry_identity: Some(id.clone()) });
    deploy::create(&state.db, "shop", &spec, &boss).await.unwrap();
    let (status, _, body) = send(&app, request(Method::DELETE, &format!("/api/v1/identities/{id}"), Some(&owner), None)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "ghcr is used by shop, pick another login on those first");

    let (_, _, used) = send(&app, request(Method::GET, "/api/v1/identities", Some(&owner), None)).await;
    assert_eq!(used[1]["used_by"][0]["name"], "shop");
}

#[tokio::test]
async fn certificates_are_matched_by_name_and_pins_must_fit() {
    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    let (boss, owner) = signed_in(&state, "boss", Role::owner()).await;
    let d = deploy::create(&state.db, "shop", &Spec::new(Source::image("nginx:1.29")), &boss).await.unwrap();

    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let cert = rcgen::CertificateParams::new(vec!["example.com".into(), "*.example.com".into()]).unwrap().self_signed(&key).unwrap();
    let other_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();

    let wrong = json!({ "certificate": cert.pem(), "key": other_key.serialize_pem() });
    let (status, _, body) = send(&app, request(Method::POST, "/api/v1/identities/certificates", Some(&owner), Some(wrong))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "the private key doesn't belong to this certificate");

    let pasted = json!({ "certificate": cert.pem(), "key": key.serialize_pem() });
    let (status, _, created) = send(&app, request(Method::POST, "/api/v1/identities/certificates", Some(&owner), Some(pasted))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["sans"], json!(["example.com", "*.example.com"]));
    let cert_id = created["id"].as_str().unwrap().to_owned();
    let (_, _, list) = send(&app, request(Method::GET, "/api/v1/identities", Some(&owner), None)).await;
    assert!(!list.to_string().contains("PRIVATE KEY"), "the key never leaves: {list}");
    assert_eq!(list[0]["name"], "example.com");

    let add = |host: &str, pin: Option<&str>| json!({ "hostname": host, "service": "app", "port": 80, "tls": true, "certificate": pin });
    let uri = format!("/api/v1/deployments/{}/domains", d.id);
    let (status, _, body) = send(&app, request(Method::POST, &uri, Some(&owner), Some(add("deep.app.example.com", Some(&cert_id))))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a wildcard covers one level only");
    assert_eq!(body["error"], "example.com doesn't cover deep.app.example.com, it's for example.com, *.example.com");
    let (status, _, pinned) = send(&app, request(Method::POST, &uri, Some(&owner), Some(add("api.example.com", Some(&cert_id))))).await;
    assert_eq!(status, StatusCode::CREATED);
    send(&app, request(Method::POST, &uri, Some(&owner), Some(add("shop.example.com", None)))).await;
    send(&app, request(Method::POST, &uri, Some(&owner), Some(add("shop.other.dev", None)))).await;

    let (_, _, domains) = send(&app, request(Method::GET, &uri, Some(&owner), None)).await;
    let served: Vec<(String, String)> = domains
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["hostname"].as_str().unwrap().to_owned(), d["served_by"]["kind"].as_str().unwrap().to_owned()))
        .collect();
    assert_eq!(
        served,
        [("api.example.com".into(), "saved".into()), ("shop.example.com".into(), "saved".into()), ("shop.other.dev".into(), "acme".into())]
    );

    let (status, _, body) = send(&app, request(Method::DELETE, &format!("/api/v1/identities/{cert_id}"), Some(&owner), None)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "example.com is pinned on api.example.com, switch those domains to another certificate first");
    let unpin = format!("/api/v1/domains/{}/certificate", pinned["id"].as_str().unwrap());
    let (status, _, _) = send(&app, request(Method::PUT, &unpin, Some(&owner), Some(json!({ "certificate": "acme" })))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = send(&app, request(Method::DELETE, &format!("/api/v1/identities/{cert_id}"), Some(&owner), None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn domain_routing_options_are_checked_and_need_managed_traefik() {
    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    let (boss, owner) = signed_in(&state, "boss", Role::owner()).await;
    let d = deploy::create(&state.db, "shop", &Spec::new(Source::image("nginx:1.29")), &boss).await.unwrap();
    let uri = format!("/api/v1/deployments/{}/domains", d.id);
    let post = |body: Value| request(Method::POST, &uri, Some(&owner), Some(body));

    let (status, _, body) = send(&app, post(json!({ "hostname": "old.example.com", "options": { "redirect": "old.example.com" } }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "old.example.com needs to start with https:// or http://");
    let (status, _, body) = send(&app, post(json!({ "hostname": "x.example.com" }))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("pick a service and a port")));
    let bad_path = json!({ "hostname": "x.example.com", "service": "app", "port": 80, "options": { "paths": [{ "path": "/a`)", "service": "api", "port": 1 }] } });
    let (status, _, body) = send(&app, post(bad_path)).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("/a`) isn't a path like /api/ or /robots.txt")));
    let compressed = json!({ "hostname": "x.example.com", "service": "app", "port": 80, "options": { "compress": true } });
    let (status, _, body) = send(&app, post(compressed)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "the proxy is off in tests");
    assert_eq!(body["error"], "paths, redirects, compression and error pages need managed Traefik");

    let (status, _, created) = send(&app, post(json!({ "hostname": "shop.example.com", "service": "app", "port": 80 }))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["options"], json!({}));
    let edit = format!("/api/v1/domains/{}", created["id"].as_str().unwrap());
    let (status, _, updated) = send(&app, request(Method::PUT, &edit, Some(&owner), Some(json!({ "service": "web", "port": 8080 })))).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!((updated["service"].as_str(), updated["port"].as_i64()), (Some("web"), Some(8080)));
    let (status, _, _) = send(&app, request(Method::PUT, &edit, Some(&owner), Some(json!({ "service": "", "port": 0 })))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

async fn signed_in(state: &AppState, name: &str, role: Role) -> (auth::User, String) {
    let user = auth::create_user(&state.db, name, "start-password-1", role).await.unwrap();
    let token = auth::create_session(&state.db, &user.id).await.unwrap();
    (user, token)
}

async fn me_status(app: &Router, token: &str) -> StatusCode {
    send(app, request(Method::GET, "/api/v1/me", Some(token), None)).await.0
}

#[tokio::test]
async fn owner_changes_role_and_host_mounts_but_not_an_owners_role() {
    let (app, state) = app().await;
    let (owner, owner_token) = signed_in(&state, "boss", Role::owner()).await;
    let (dev, dev_token) = signed_in(&state, "dev", Role::viewer()).await;

    let uri = format!("/api/v1/users/{}", dev.id);
    let change = json!({ "role": "deployer", "host_mounts": true });
    let (status, _, _) = send(&app, request(Method::PUT, &uri, Some(&owner_token), Some(change.clone()))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, me) = send(&app, request(Method::GET, "/api/v1/me", Some(&dev_token), None)).await;
    assert_eq!(me["role"], "deployer");
    let mounts: bool = sqlx::query_scalar("SELECT host_mounts FROM users WHERE id = ?").bind(&dev.id).fetch_one(&state.db).await.unwrap();
    assert!(mounts);

    let (status, _, _) = send(&app, request(Method::PUT, &uri, Some(&dev_token), Some(change))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only the owner edits users");

    let demote = json!({ "role": "admin", "host_mounts": true });
    let (status, _, body) =
        send(&app, request(Method::PUT, &format!("/api/v1/users/{}", owner.id), Some(&owner_token), Some(demote))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "an owner's role can't be changed");

    let missing = json!({ "role": "viewer", "host_mounts": false });
    let (status, _, _) = send(&app, request(Method::PUT, "/api/v1/users/nope", Some(&owner_token), Some(missing))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn password_reset_by_owner_ends_the_users_sessions() {
    let (app, state) = app().await;
    let (_, owner_token) = signed_in(&state, "boss", Role::owner()).await;
    let (dev, dev_token) = signed_in(&state, "dev", Role::deployer()).await;
    let uri = format!("/api/v1/users/{}/password", dev.id);

    let (status, _, _) = send(&app, request(Method::POST, &uri, Some(&owner_token), Some(json!({ "password": "short" })))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(me_status(&app, &dev_token).await, StatusCode::OK);

    let (status, _, _) =
        send(&app, request(Method::POST, &uri, Some(&owner_token), Some(json!({ "password": "brand-new-pw-1" })))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(me_status(&app, &dev_token).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&app, &owner_token).await, StatusCode::OK);

    let login = json!({ "username": "dev", "password": "brand-new-pw-1" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/login", None, Some(login))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn own_password_change_needs_the_current_one_and_keeps_this_session() {
    let (app, state) = app().await;
    let (dev, token) = signed_in(&state, "dev", Role::viewer()).await;
    let other = auth::create_session(&state.db, &dev.id).await.unwrap();

    let wrong = json!({ "current": "not-it-at-all", "password": "brand-new-pw-1" });
    let (status, _, body) = send(&app, request(Method::POST, "/api/v1/me/password", Some(&token), Some(wrong))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "the current password is wrong");

    let right = json!({ "current": "start-password-1", "password": "brand-new-pw-1" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/me/password", Some(&token), Some(right))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(me_status(&app, &token).await, StatusCode::OK);
    assert_eq!(me_status(&app, &other).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn removing_a_user_keeps_their_deployments_and_protects_the_last_owner() {
    use crate::deploy::{self, Source, Spec};
    let (app, state) = app().await;
    let (owner, owner_token) = signed_in(&state, "boss", Role::owner()).await;
    let (dev, dev_token) = signed_in(&state, "dev", Role::deployer()).await;
    let made = deploy::create(&state.db, "app", &Spec::new(Source::image("nginx:1.29")), &dev).await.unwrap();

    let (status, _, _) =
        send(&app, request(Method::DELETE, &format!("/api/v1/users/{}", dev.id), Some(&owner_token), None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(me_status(&app, &dev_token).await, StatusCode::UNAUTHORIZED);
    assert!(deploy::get(&state.db, &made.id).await.unwrap().is_some());

    let owner_uri = format!("/api/v1/users/{}", owner.id);
    let (status, _, body) = send(&app, request(Method::DELETE, &owner_uri, Some(&owner_token), None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "the last owner can't be removed");

    let (_, second_token) = signed_in(&state, "boss2", Role::owner()).await;
    let (status, _, _) = send(&app, request(Method::DELETE, &owner_uri, Some(&second_token), None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "an owner can go when another one is left");
}

#[tokio::test]
async fn site_uploads_only_reach_static_sites() {
    use crate::deploy::spec::SiteOptions;
    use crate::deploy::{self, Source, Spec, tokens};
    let state = plain_state().await;
    let owner = auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let app = app_at(&state, [10, 0, 0, 1]);
    let session = token_from(&login(&app, "boss", "boss-password-1").await.1.unwrap());
    let mut spec = Spec::new(Source::Static {});
    spec.site = Some(SiteOptions::default());
    let site = deploy::create(&state.db, "docs", &spec, &owner).await.unwrap();
    let image = deploy::create(&state.db, "api", &Spec::new(Source::image("nginx:1.29")), &owner).await.unwrap();
    let (_, other_token) = tokens::create(&state.db, &image.id, "ci", false, &owner.id).await.unwrap();

    let mut tar = vec![0u8; 1024];
    tar[257..262].copy_from_slice(b"ustar");
    let upload = |id: &str, body: Vec<u8>, auth: (header::HeaderName, String)| {
        Request::builder()
            .method(Method::PUT)
            .uri(format!("/api/v1/deployments/{id}/site"))
            .header(CSRF_HEADER, "1")
            .header(auth.0, auth.1)
            .body(Body::from(body))
            .unwrap()
    };
    let cookie = (header::COOKIE, format!("{}={session}", auth::SESSION_COOKIE));

    let (status, _, _) = send(&app, upload(&site.id, tar.clone(), cookie.clone())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(tokio::fs::read(state.runner.site_upload_path(&site.id)).await.unwrap(), tar);
    let (status, _, _) = send(&app, upload(&image.id, tar.clone(), cookie.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "only static sites take site files");
    let (status, _, _) = send(&app, upload(&site.id, b"PK\x03\x04 not a tar".to_vec(), cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "only tar archives");
    let bearer = (header::AUTHORIZATION, format!("Bearer {other_token}"));
    let (status, _, _) = send(&app, upload(&site.id, tar, bearer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a token for another deployment");
}

#[tokio::test]
async fn runner_pools_need_the_permission_and_only_owners_hand_out_the_host_socket() {
    use crate::identity::{self, Kind, NewIdentity};
    let state = plain_state().await;
    auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    auth::create_user(&state.db, "adm", "adm-password-1", Role::admin()).await.unwrap();
    auth::create_user(&state.db, "look", "look-password-1", Role::viewer()).await.unwrap();
    let meta = json!({});
    let new = NewIdentity { id: None, kind: Kind::Gitea, name: "forge", host: "git.example.com", username: "", secret: "tok", meta: &meta, expires_at: None };
    let identity = identity::insert(&state.db, &new).await.unwrap();
    let app = app_at(&state, [10, 0, 0, 1]);
    let session = |name: &'static str, password: &'static str| {
        let app = app.clone();
        async move { token_from(&login(&app, name, password).await.1.unwrap()) }
    };
    let (owner, admin, viewer) =
        (session("boss", "boss-password-1").await, session("adm", "adm-password-1").await, session("look", "look-password-1").await);
    let pool = |mode: &str| {
        json!({ "name": "ci", "provider": "gitea", "identity": identity, "scope_kind": "instance", "docker_mode": mode, "max": 2 })
    };

    let (status, _, _) = send(&app, request(Method::GET, "/api/v1/runners", Some(&viewer), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/runners", Some(&admin), Some(pool("host")))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the host socket is root on the host");
    let wrong = json!({ "name": "ci", "provider": "github", "identity": identity, "scope_kind": "repo", "scope": "a/b", "docker_mode": "dind" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/runners", Some(&admin), Some(wrong))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a Gitea identity can't register GitHub runners");
    let (status, _, created) = send(&app, request(Method::POST, "/api/v1/runners", Some(&admin), Some(pool("dind")))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/runners", Some(&owner), Some(pool("host")))).await;
    assert_eq!(status, StatusCode::CONFLICT, "names are unique");

    let (_, _, listed) = send(&app, request(Method::GET, "/api/v1/runners", Some(&admin), None)).await;
    assert_eq!(listed[0]["name"], "ci");
    assert_eq!(listed[0]["default_image"], "gitea/act_runner:latest");
    let (status, _, _) = send(&app, request(Method::DELETE, &format!("/api/v1/identities/{identity}"), Some(&owner), None)).await;
    assert_eq!(status, StatusCode::CONFLICT, "the identity registers a pool");
    let uri = format!("/api/v1/runners/{}", created["id"].as_str().unwrap());
    let (status, _, removed) = send(&app, request(Method::DELETE, &uri, Some(&admin), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(removed["leftovers"], json!([]));
}

#[test]
fn static_sites_keep_their_saved_status_and_strategy() {
    use crate::deploy::runner::{Strategy, strategy};
    use crate::deploy::spec::SiteOptions;
    use crate::deploy::{Source, Spec};
    let mut spec = Spec::new(Source::Static {});
    spec.site = Some(SiteOptions::default());
    assert_eq!(strategy(&spec).0, Strategy::Static);
    let mut both = Spec::new(Source::Git {
        url: "https://example.com/r.git".into(),
        reference: None,
        context: None,
        dockerfile: None,
        token: None,
        identity: None,
        compose_file: Some("compose.yaml".into()),
    });
    both.site = Some(SiteOptions::default());
    assert!(super::deployments::check_spec(&both, false).is_err(), "Compose and static at once");
    let bare = Spec::new(Source::Static {});
    assert!(super::deployments::check_spec(&bare, false).is_err(), "an archive needs site settings");

    let mut image = Spec::new(Source::image("ghcr.io/acme/docs:3"));
    image.site = Some(SiteOptions { output: "/usr/share/nginx/html".into(), ..Default::default() });
    assert!(super::deployments::check_spec(&image, false).is_ok(), "an image's files can be served");
    assert_eq!(strategy(&image).0, Strategy::Static);
    image.site = Some(SiteOptions { output: "html".into(), ..Default::default() });
    assert!(super::deployments::check_spec(&image, false).unwrap_err().contains("folder with the files in the image"));

    assert!(super::deployments::check_spec(&Spec::new(Source::Dockerfile { file: "FROM nginx:alpine".into() }), false).is_ok());
    assert!(super::deployments::check_spec(&Spec::new(Source::Dockerfile { file: " ".into() }), false).is_err());
}

#[tokio::test]
async fn saved_secrets_need_their_permissions_and_stay_out_of_reach() {
    let (app, state) = app().await;
    let owner = auth::create_user(&state.db, "root", "root-password-1", Role::owner()).await.unwrap();
    let viewer = auth::create_user(&state.db, "eye", "eye-password-12", Role::viewer()).await.unwrap();
    let owner = auth::create_session(&state.db, &owner.id).await.unwrap();
    let viewer = auth::create_session(&state.db, &viewer.id).await.unwrap();

    let body = json!({ "name": "db-password", "description": "Postgres", "value": "hunter2-long" });
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/saved-secrets", Some(&viewer), Some(body.clone()))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, created) = send(&app, request(Method::POST, "/api/v1/saved-secrets", Some(&owner), Some(body.clone()))).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap().to_owned();
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/saved-secrets", Some(&owner), Some(body))).await;
    assert_eq!(status, StatusCode::CONFLICT, "names are unique");

    let (_, _, list) = send(&app, request(Method::GET, "/api/v1/saved-secrets", Some(&owner), None)).await;
    assert!(!list.to_string().contains("hunter2"), "the list never carries values: {list}");
    let (status, _, _) = send(&app, request(Method::GET, "/api/v1/saved-secrets", Some(&viewer), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (_, _, hit) = send(&app, request(Method::POST, "/api/v1/saved-secrets/match", Some(&owner), Some(json!({ "value": "hunter2-long" })))).await;
    assert_eq!(hit["name"], "db-password");
    for _ in 0..40 {
        send(&app, request(Method::POST, "/api/v1/saved-secrets/match", Some(&owner), Some(json!({ "value": "guessing-1" })))).await;
    }
    let (status, _, _) = send(&app, request(Method::POST, "/api/v1/saved-secrets/match", Some(&owner), Some(json!({ "value": "guessing-2" })))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    let mut spec = crate::deploy::Spec::new(crate::deploy::Source::image("nginx:alpine"));
    spec.container.env.insert("DB".into(), "should-be-dropped".into());
    spec.secret_refs.insert("DB".into(), id.clone());
    let (status, _, made) = send(&app, request(Method::POST, "/api/v1/deployments", Some(&owner), Some(json!({ "name": "api", "spec": spec })))).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let dep = made["id"].as_str().unwrap().to_owned();
    let stored = crate::deploy::get(&state.db, &dep).await.unwrap().unwrap();
    assert_eq!(stored.spec.container.env["DB"], "", "no raw value hides behind a reference");

    let (status, _, err) = send(&app, request(Method::DELETE, &format!("/api/v1/saved-secrets/{id}"), Some(&owner), None)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err.to_string().contains("api"), "names the deployment: {err}");

    let mut unknown = spec.clone();
    unknown.secret_refs.insert("DB".into(), "nope".into());
    let (status, _, _) = send(&app, request(Method::PUT, &format!("/api/v1/deployments/{dep}"), Some(&owner), Some(json!(unknown)))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown ids are refused");
}

fn app_at(state: &AppState, peer: [u8; 4]) -> Router {
    router(state.clone()).layer(MockConnectInfo(SocketAddr::from((peer, 5000))))
}

async fn plain_state() -> AppState {
    AppState::new(test_pool().await, std::env::temp_dir().join("kuroe-test"), false).unwrap()
}

async fn login(app: &Router, username: &str, password: &str) -> (StatusCode, Option<String>) {
    let creds = json!({ "username": username, "password": password });
    let (status, cookie, _) = send(app, request(Method::POST, "/api/v1/login", None, Some(creds))).await;
    (status, cookie)
}

#[tokio::test]
async fn local_http_access_only_answers_this_machine() {
    use super::http_access::{self, HttpAccess};
    let state = plain_state().await;
    auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let public = app_at(&state, [203, 0, 113, 9]);
    let local = app_at(&state, [127, 0, 0, 1]);

    let (status, cookie) = login(&public, "boss", "boss-password-1").await;
    assert_eq!(status, StatusCode::OK, "open by default");
    assert!(!cookie.unwrap().contains("Secure"));

    http_access::set(&state.db, HttpAccess::Local).await.unwrap();
    let res = public.clone().oneshot(request(Method::GET, "/api/v1/setup", None, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let text = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(text.contains("kuroe admin http open"), "{text}");
    let (status, _, _) = send(&public, request(Method::GET, "/deployments", None, None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the SPA is behind the same check");
    let forged = Request::builder().uri("/api/v1/setup").header("x-forwarded-proto", "https").body(Body::empty()).unwrap();
    assert_eq!(public.clone().oneshot(forged).await.unwrap().status(), StatusCode::FORBIDDEN, "a forged header from outside");
    let (status, _, _) = send(&public, request(Method::GET, "/preview/c1/80/", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "previews keep their own session check");

    let (status, cookie) = login(&local, "boss", "boss-password-1").await;
    assert_eq!(status, StatusCode::OK, "loopback always gets through");
    assert!(cookie.unwrap().contains("Secure"), "cookies are Secure under local");

    http_access::set(&state.db, HttpAccess::Open).await.unwrap();
    assert_eq!(login(&public, "boss", "boss-password-1").await.0, StatusCode::OK, "picked up without a restart");
}

#[tokio::test]
async fn local_can_only_be_turned_on_from_where_it_still_answers() {
    let state = plain_state().await;
    auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    let public = app_at(&state, [203, 0, 113, 9]);
    let token = token_from(&login(&public, "boss", "boss-password-1").await.1.unwrap());

    let body = json!({ "http_access": "local" });
    let (status, _, _) = send(&public, request(Method::PUT, "/api/v1/settings", Some(&token), Some(body.clone()))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(&public, request(Method::PUT, "/api/v1/settings", Some(&token), Some(json!({ "http_access": "shut" })))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let trusted = json!({ "http_access": "local", "trusted_proxies": "203.0.113.0/24" });
    let mut req = request(Method::PUT, "/api/v1/settings", Some(&token), Some(trusted));
    req.headers_mut().insert("x-forwarded-proto", "https".parse().unwrap());
    let (status, _, _) = send(&public, req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "through a trusted HTTPS proxy");

    let mut socket = request(Method::GET, "/api/v1/me", Some(&token), None);
    socket.headers_mut().insert("x-forwarded-proto", "wss".parse().unwrap());
    let (status, _, _) = send(&public, socket).await;
    assert_eq!(status, StatusCode::OK, "websockets through the same proxy say wss");
    let mut plain = request(Method::GET, "/api/v1/me", Some(&token), None);
    plain.headers_mut().insert("x-forwarded-proto", "ws".parse().unwrap());
    let (status, _, _) = send(&public, plain).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "plain websockets are still refused");
}

#[tokio::test]
async fn http_banner_shows_once_per_sign_in_to_people_who_can_change_it() {
    let state = plain_state().await;
    auth::create_user(&state.db, "boss", "boss-password-1", Role::owner()).await.unwrap();
    auth::create_user(&state.db, "look", "look-password-1", Role::viewer()).await.unwrap();
    let app = app_at(&state, [10, 0, 0, 1]);
    let banner = |token: String| {
        let app = app.clone();
        async move { send(&app, request(Method::GET, "/api/v1/me", Some(&token), None)).await.2["show_http_banner"].clone() }
    };

    let before = token_from(&login(&app, "boss", "boss-password-1").await.1.unwrap());
    assert_eq!(banner(before).await, false, "not before onboarding is done");

    sqlx::query("INSERT INTO settings (key, value) VALUES ('onboarding_done', 'true')").execute(&state.db).await.unwrap();
    let owner = token_from(&login(&app, "boss", "boss-password-1").await.1.unwrap());
    assert_eq!(banner(owner.clone()).await, true);
    assert_eq!(banner(owner).await, false, "once per sign-in");

    let viewer = token_from(&login(&app, "look", "look-password-1").await.1.unwrap());
    assert_eq!(banner(viewer).await, false, "only for people who can change settings");

    super::http_access::set(&state.db, super::http_access::HttpAccess::Local).await.unwrap();
    let local = app_at(&state, [127, 0, 0, 1]);
    let token = token_from(&login(&local, "boss", "boss-password-1").await.1.unwrap());
    let (_, _, me) = send(&local, request(Method::GET, "/api/v1/me", Some(&token), None)).await;
    assert_eq!(me["show_http_banner"], false, "gone once plain HTTP is off");
}
