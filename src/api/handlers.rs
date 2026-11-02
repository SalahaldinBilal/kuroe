use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::AppState;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use super::http_access::{self, HttpAccess};
use crate::auth::{self, Permission, Role, SESSION_COOKIE, SESSION_TTL_SECS, User};

#[derive(Deserialize)]
pub struct Credentials {
    username: String,
    password: String,
}

#[derive(Deserialize)]
pub struct SetupRequest {
    username: String,
    password: String,
    #[serde(default)]
    code: String,
}

#[derive(Deserialize)]
pub struct NewUser {
    username: String,
    password: String,
    role: Role,
}

fn validate_credentials(username: &str, password: &str) -> ApiResult<()> {
    let name_ok = (1..=64).contains(&username.len())
        && username.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !name_ok {
        return Err(ApiError::bad_request("username must be 1 to 64 characters of letters, digits, '.', '_' or '-'"));
    }
    validate_password(password)
}

fn validate_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < 10 {
        return Err(ApiError::bad_request("password must be at least 10 characters"));
    }
    Ok(())
}

fn no_such_user() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "no such user")
}

async fn is_secure(state: &AppState, headers: &HeaderMap) -> ApiResult<bool> {
    Ok(state.secure_cookies
        || http_access::forwarded_https(headers)
        || http_access::load(&state.db).await?.access == HttpAccess::Local)
}

fn session_cookie(token: &str, max_age: i64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}");
    HeaderValue::from_str(&cookie).expect("cookie is ascii")
}

async fn signed_in(state: &AppState, headers: &HeaderMap, user: User) -> ApiResult<Response> {
    let token = auth::create_session(&state.db, &user.id).await?;
    let cookie = session_cookie(&token, SESSION_TTL_SECS, is_secure(state, headers).await?);
    state.http_access.flag_banner(&token);
    Ok(([(header::SET_COOKIE, cookie)], Json(user)).into_response())
}

pub async fn setup_status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "needed": !auth::has_users(&state.db).await? })))
}

pub async fn setup(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<SetupRequest>,
) -> ApiResult<Response> {
    if auth::has_users(&state.db).await? {
        return Err(ApiError::new(StatusCode::CONFLICT, "setup is already done"));
    }
    let ip = addr.ip();
    if state.limiter.is_blocked(ip) {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too many wrong setup codes, try again in 15 minutes"));
    }
    if !auth::setup_code::matches(&state.runner.data_dir, &body.code) {
        state.limiter.record_failure(ip);
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "wrong setup code. Run `kuroe admin setup-code` on the server, or search kuroe's log for \"Setup code\"",
        ));
    }
    state.limiter.clear(ip);
    validate_credentials(&body.username, &body.password)?;
    let user = User {
        id: uuid::Uuid::new_v4().to_string(),
        username: body.username,
        role: Role::owner(),
        created_at: crate::db::now(),
    };
    let inserted = sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at)
         SELECT ?, ?, ?, 'owner', ? WHERE NOT EXISTS (SELECT 1 FROM users)",
    )
    .bind(&user.id)
    .bind(&user.username)
    .bind(auth::hash_password(&body.password)?)
    .bind(user.created_at)
    .execute(&state.db)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Err(ApiError::new(StatusCode::CONFLICT, "setup is already done"));
    }
    auth::setup_code::clear(&state.runner.data_dir);
    signed_in(&state, &headers, user).await
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Credentials>,
) -> ApiResult<Response> {
    let ip = addr.ip();
    if state.limiter.is_blocked(ip) {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too many failed sign-ins, try again in 15 minutes"));
    }
    let row: Option<(String, String)> = sqlx::query_as("SELECT id, password_hash FROM users WHERE username = ?")
        .bind(&body.username)
        .fetch_optional(&state.db)
        .await?;
    let user_id = match row {
        Some((id, hash)) if auth::verify_password(&body.password, &hash) => id,
        _ => {
            state.limiter.record_failure(ip);
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "wrong username or password"));
        }
    };
    state.limiter.clear(ip);
    let user: User = sqlx::query_as("SELECT id, username, role, created_at FROM users WHERE id = ?")
        .bind(&user_id)
        .fetch_one(&state.db)
        .await?;
    signed_in(&state, &headers, user).await
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap, current: CurrentUser) -> ApiResult<Response> {
    auth::delete_session(&state.db, &current.token).await?;
    state.http_access.take_banner(&current.token);
    let cookie = session_cookie("", 0, is_secure(&state, &headers).await?);
    Ok(([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response())
}

/// The plain-HTTP banner shows on the first `/me` after a sign-in, to people who can change settings,
/// once onboarding is done and while plain HTTP is still open.
async fn show_http_banner(state: &AppState, current: &CurrentUser) -> ApiResult<bool> {
    if !state.http_access.take_banner(&current.token) || !current.access.has(Permission::Settings) {
        return Ok(false);
    }
    let onboarded: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'onboarding_done'").fetch_optional(&state.db).await?;
    Ok(onboarded.as_deref() == Some("true") && http_access::load(&state.db).await?.access == HttpAccess::Open)
}

pub async fn me(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Value>> {
    let permissions: Vec<Permission> = Permission::ALL.into_iter().filter(|p| current.access.has(*p)).collect();
    Ok(Json(json!({
        "id": current.user.id,
        "username": current.user.username,
        "role": current.user.role,
        "created_at": current.user.created_at,
        "permissions": permissions,
        "show_http_banner": show_http_banner(&state, &current).await?,
    })))
}

/// Only an owner can hand out or touch the owner role.
async fn check_role_change(state: &AppState, current: &CurrentUser, role: &Role) -> ApiResult<()> {
    super::roles::require_role(state, role.as_str()).await?;
    if role.is_owner() {
        current.require_owner()?;
    }
    Ok(())
}

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct UserRow {
    #[sqlx(flatten)]
    #[serde(flatten)]
    user: User,
    host_mounts: bool,
    grants: i64,
}

pub async fn list_users(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<UserRow>>> {
    current.require(Permission::Users)?;
    let users = sqlx::query_as(
        "SELECT id, username, role, created_at, host_mounts,
                (SELECT COUNT(*) FROM deployment_grants g WHERE g.user_id = users.id) AS grants
         FROM users ORDER BY created_at",
    )
        .fetch_all(&state.db)
        .await?;
    Ok(Json(users))
}

pub async fn create_user(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<NewUser>,
) -> ApiResult<(StatusCode, Json<User>)> {
    current.require(Permission::Users)?;
    validate_credentials(&body.username, &body.password)?;
    check_role_change(&state, &current, &body.role).await?;
    match auth::create_user(&state.db, &body.username, &body.password, body.role).await {
        Ok(user) => Ok((StatusCode::CREATED, Json(user))),
        Err(err) => {
            let duplicate = err
                .downcast_ref::<sqlx::Error>()
                .and_then(|e| e.as_database_error())
                .is_some_and(|e| e.is_unique_violation());
            if duplicate {
                Err(ApiError::new(StatusCode::CONFLICT, format!("{} already exists", body.username)))
            } else {
                Err(err.into())
            }
        }
    }
}

pub async fn read_settings(State(state): State<AppState>, _current: CurrentUser) -> ApiResult<Json<BTreeMap<String, String>>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings").fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().collect()))
}

/// Where the key that seals saved secrets comes from, for the Settings page.
pub async fn secret_key(current: CurrentUser) -> ApiResult<Json<Value>> {
    current.require(Permission::Settings)?;
    let source = crate::secrets::source().ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "the secret key isn't loaded"))?;
    Ok(Json(json!(source)))
}

pub async fn write_settings(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    current: CurrentUser,
    Json(body): Json<BTreeMap<String, String>>,
) -> ApiResult<StatusCode> {
    current.require(Permission::Settings)?;
    let bad_key = body
        .keys()
        .find(|k| k.is_empty() || k.len() > 64 || !k.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
    if let Some(key) = bad_key {
        return Err(ApiError::bad_request(format!("invalid setting name {key:?}")));
    }
    if let Some(value) = body.get(http_access::SETTING) {
        let access = HttpAccess::parse(value).ok_or_else(|| ApiError::bad_request("plain HTTP access is open or local"))?;
        let trusted = match body.get(http_access::TRUSTED_SETTING) {
            Some(list) => http_access::parse_list(list),
            None => http_access::load(&state.db).await?.trusted,
        };
        if access == HttpAccess::Local && !http_access::request_allowed(&state, Some(addr), &headers, trusted).await {
            return Err(ApiError::bad_request(
                "turn this on from an HTTPS address, otherwise this page would stop answering you",
            ));
        }
    }
    let mut tx = state.db.begin().await?;
    for (key, value) in &body {
        sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
            .bind(key)
            .bind(value)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct UserUpdate {
    role: Role,
    host_mounts: bool,
}

async fn role_of(state: &AppState, id: &str) -> ApiResult<Role> {
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = ?").bind(id).fetch_optional(&state.db).await?;
    Ok(Role::from(role.ok_or_else(no_such_user)?))
}

async fn require_editable(state: &AppState, current: &CurrentUser, id: &str) -> ApiResult<Role> {
    current.require(Permission::Users)?;
    let role = role_of(state, id).await?;
    if role.is_owner() {
        current.require_owner()?;
    }
    Ok(role)
}

pub async fn update_user(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<UserUpdate>,
) -> ApiResult<StatusCode> {
    let role = require_editable(&state, &current, &id).await?;
    if role.is_owner() && !body.role.is_owner() {
        return Err(ApiError::bad_request("an owner's role can't be changed"));
    }
    check_role_change(&state, &current, &body.role).await?;
    sqlx::query("UPDATE users SET role = ?, host_mounts = ? WHERE id = ?")
        .bind(body.role.as_str())
        .bind(body.host_mounts)
        .bind(&id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct NewPassword {
    password: String,
}

pub async fn set_user_password(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<NewPassword>,
) -> ApiResult<StatusCode> {
    require_editable(&state, &current, &id).await?;
    validate_password(&body.password)?;
    if !auth::set_password(&state.db, &id, &body.password, None).await? {
        return Err(no_such_user());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct PasswordChange {
    current: String,
    password: String,
}

pub async fn change_own_password(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<PasswordChange>,
) -> ApiResult<StatusCode> {
    let hash: String =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?").bind(&current.user.id).fetch_one(&state.db).await?;
    if !auth::verify_password(&body.current, &hash) {
        return Err(ApiError::bad_request("the current password is wrong"));
    }
    validate_password(&body.password)?;
    auth::set_password(&state.db, &current.user.id, &body.password, Some(&current.token)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_user(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    require_editable(&state, &current, &id).await?;
    let removed = sqlx::query(
        "DELETE FROM users WHERE id = ? AND (role != 'owner' OR (SELECT COUNT(*) FROM users WHERE role = 'owner') > 1)",
    )
    .bind(&id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if removed == 0 {
        return Err(ApiError::bad_request("the last owner can't be removed"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// Samples older than this belong to containers that stopped, so they don't count as current usage.
const CURRENT_SAMPLE_WINDOW_SECS: i64 = 30;

pub async fn pool_usage(State(state): State<AppState>, _current: CurrentUser) -> ApiResult<Json<Value>> {
    let unreachable = |e: anyhow::Error| ApiError::new(StatusCode::BAD_GATEWAY, format!("docker: {e:#}"));
    let pool = state.runner.pool().await.map_err(unreachable)?;
    let members = state.runner.running_usage().await.map_err(unreachable)?;
    let storage_used = state.runner.storage_used().await.map_err(unreachable)?;
    let (cpu_percent, memory_actual): (f64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(m.cpu_percent), 0.0), COALESCE(SUM(m.mem_bytes), 0)
         FROM container_metrics m
         JOIN (SELECT series_id, MAX(ts) AS ts FROM container_metrics WHERE ts >= ? GROUP BY series_id) latest
           ON latest.series_id = m.series_id AND latest.ts = m.ts",
    )
    .bind(crate::db::now() - CURRENT_SAMPLE_WINDOW_SECS)
    .fetch_one(&state.db)
    .await?;
    let disk: Option<(i64, i64)> =
        sqlx::query_as("SELECT disk_used, disk_total FROM host_metrics ORDER BY ts DESC LIMIT 1").fetch_optional(&state.db).await?;
    Ok(Json(json!({
        "cpus": pool.cpus,
        "memory_bytes": pool.memory_bytes,
        "storage_bytes": pool.storage_bytes,
        "cpus_used": members.iter().fold(0.0, |sum, m| sum + m.cpus_limit),
        "memory_used": members.iter().map(|m| m.memory_limit).sum::<u64>(),
        "cpus_counted": members.iter().fold(0.0, |sum, m| sum + m.counted.cpus),
        "memory_counted": members.iter().map(|m| m.counted.memory_bytes).sum::<u64>(),
        "storage_used": storage_used,
        "cpus_actual": cpu_percent / 100.0,
        "memory_actual": memory_actual.max(0),
        "disk_used": disk.map(|d| d.0),
        "disk_total": disk.map(|d| d.1),
        "containers": members.len(),
        "cpus_unlimited": members.iter().filter(|m| m.cpus_limit == 0.0).count(),
        "memory_unlimited": members.iter().filter(|m| m.memory_limit == 0).count(),
    })))
}

pub async fn docker_info(State(state): State<AppState>, _current: CurrentUser) -> ApiResult<Response> {
    match crate::docker::daemon_info(&state.docker).await {
        Ok(info) => Ok(Json(info).into_response()),
        Err(err) => Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("docker daemon unreachable: {err}"))),
    }
}
