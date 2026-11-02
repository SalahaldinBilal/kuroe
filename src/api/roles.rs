use std::collections::BTreeSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::access;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::{Access, Permission, RoleDef, Scope, User};
use crate::deploy::spec::valid_name;

#[derive(Serialize)]
pub struct RoleRow {
    #[serde(flatten)]
    role: RoleDef,
    users: i64,
}

pub async fn list(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<RoleRow>>> {
    current.require(Permission::Users)?;
    let mut rows = Vec::new();
    for role in RoleDef::list(&state.db).await? {
        let users = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = ?").bind(&role.name).fetch_one(&state.db).await?;
        rows.push(RoleRow { role, users });
    }
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct RoleInput {
    name: String,
    #[serde(default)]
    description: String,
    permissions: BTreeSet<Permission>,
    scope: Scope,
}

fn check_input(body: &RoleInput) -> ApiResult<()> {
    if !valid_name(&body.name) {
        return Err(ApiError::bad_request("role name must be 1 to 40 characters of a-z, 0-9 and '-'"));
    }
    if body.scope == Scope::Granted {
        return Err(ApiError::bad_request("a custom role applies to all deployments or to a list of them"));
    }
    if body.description.len() > 200 {
        return Err(ApiError::bad_request("keep the description under 200 characters"));
    }
    Ok(())
}

async fn custom_role(state: &AppState, name: &str) -> ApiResult<RoleDef> {
    let role = RoleDef::load(&state.db, name).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such role"))?;
    if role.builtin {
        return Err(ApiError::bad_request("built-in roles can't be changed, duplicate it instead"));
    }
    Ok(role)
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error().is_some_and(|e| e.is_unique_violation())
}

pub async fn create(State(state): State<AppState>, current: CurrentUser, Json(body): Json<RoleInput>) -> ApiResult<StatusCode> {
    current.require(Permission::Users)?;
    check_input(&body)?;
    let inserted = sqlx::query("INSERT INTO roles (name, builtin, description, permissions, scope) VALUES (?, 0, ?, ?, ?)")
        .bind(&body.name)
        .bind(body.description.trim())
        .bind(serde_json::to_string(&body.permissions).map_err(anyhow::Error::from)?)
        .bind(body.scope.to_db())
        .execute(&state.db)
        .await;
    match inserted {
        Ok(_) => Ok(StatusCode::CREATED),
        Err(err) if is_unique_violation(&err) => Err(ApiError::new(StatusCode::CONFLICT, format!("{} already exists", body.name))),
        Err(err) => Err(err.into()),
    }
}

pub async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(name): Path<String>,
    Json(body): Json<RoleInput>,
) -> ApiResult<StatusCode> {
    current.require(Permission::Users)?;
    check_input(&body)?;
    custom_role(&state, &name).await?;
    let mut tx = state.db.begin().await?;
    let updated = sqlx::query("UPDATE roles SET name = ?, description = ?, permissions = ?, scope = ? WHERE name = ?")
        .bind(&body.name)
        .bind(body.description.trim())
        .bind(serde_json::to_string(&body.permissions).map_err(anyhow::Error::from)?)
        .bind(body.scope.to_db())
        .bind(&name)
        .execute(&mut *tx)
        .await;
    match updated {
        Ok(_) => {}
        Err(err) if is_unique_violation(&err) => {
            return Err(ApiError::new(StatusCode::CONFLICT, format!("{} already exists", body.name)));
        }
        Err(err) => return Err(err.into()),
    }
    if body.name != name {
        sqlx::query("UPDATE users SET role = ? WHERE role = ?").bind(&body.name).bind(&name).execute(&mut *tx).await?;
        sqlx::query("UPDATE deployment_grants SET role = ? WHERE role = ?").bind(&body.name).bind(&name).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove(State(state): State<AppState>, current: CurrentUser, Path(name): Path<String>) -> ApiResult<StatusCode> {
    current.require(Permission::Users)?;
    custom_role(&state, &name).await?;
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = ?").bind(&name).fetch_one(&state.db).await?;
    let grants: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deployment_grants WHERE role = ?").bind(&name).fetch_one(&state.db).await?;
    if users > 0 || grants > 0 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("{name} is still used by {users} users and {grants} deployment grants, move them to another role first"),
        ));
    }
    sqlx::query("DELETE FROM roles WHERE name = ?").bind(&name).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn require_role(state: &AppState, name: &str) -> ApiResult<RoleDef> {
    RoleDef::load(&state.db, name).await?.ok_or_else(|| ApiError::bad_request(format!("there is no role named {name}")))
}

async fn grantable_user(state: &AppState, id: &str) -> ApiResult<User> {
    let user: Option<User> =
        sqlx::query_as("SELECT id, username, role, created_at FROM users WHERE id = ?").bind(id).fetch_optional(&state.db).await?;
    let user = user.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such user"))?;
    if user.role.is_owner() {
        return Err(ApiError::bad_request("an owner can reach every deployment already"));
    }
    Ok(user)
}

#[derive(Serialize)]
pub struct Reach {
    user_id: String,
    username: String,
    role: String,
    via: &'static str,
}

pub async fn deployment_access(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Vec<Reach>>> {
    current.require(Permission::Users)?;
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    let users: Vec<User> = sqlx::query_as("SELECT id, username, role, created_at FROM users ORDER BY username").fetch_all(&state.db).await?;
    let mut reach = Vec::new();
    for user in users {
        let grant: Option<String> = sqlx::query_scalar("SELECT role FROM deployment_grants WHERE deployment_id = ? AND user_id = ?")
            .bind(&id)
            .bind(&user.id)
            .fetch_optional(&state.db)
            .await?;
        if !Access::load(&state.db, &user).await?.can(&id, Permission::DepView) {
            continue;
        }
        let (role, via) = match grant {
            _ if user.role.is_owner() => (user.role.0.clone(), "owner"),
            Some(role) => (role, "grant"),
            None => (user.role.0.clone(), "role"),
        };
        reach.push(Reach { user_id: user.id, username: user.username, role, via });
    }
    Ok(Json(reach))
}

#[derive(Deserialize)]
pub struct GrantRole {
    role: String,
}

pub async fn set_grant(
    State(state): State<AppState>,
    current: CurrentUser,
    Path((id, user_id)): Path<(String, String)>,
    Json(body): Json<GrantRole>,
) -> ApiResult<StatusCode> {
    current.require(Permission::Users)?;
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    grantable_user(&state, &user_id).await?;
    require_role(&state, &body.role).await?;
    sqlx::query(
        "INSERT INTO deployment_grants (deployment_id, user_id, role) VALUES (?, ?, ?)
         ON CONFLICT (deployment_id, user_id) DO UPDATE SET role = excluded.role",
    )
    .bind(&id)
    .bind(&user_id)
    .bind(&body.role)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_grant(
    State(state): State<AppState>,
    current: CurrentUser,
    Path((id, user_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    current.require(Permission::Users)?;
    sqlx::query("DELETE FROM deployment_grants WHERE deployment_id = ? AND user_id = ?").bind(&id).bind(&user_id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, Deserialize, sqlx::FromRow)]
pub struct UserGrant {
    deployment_id: String,
    role: String,
}

pub async fn user_grants(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Vec<UserGrant>>> {
    current.require(Permission::Users)?;
    let grants = sqlx::query_as(
        "SELECT g.deployment_id, g.role FROM deployment_grants g JOIN deployments d ON d.id = g.deployment_id
         WHERE g.user_id = ? ORDER BY d.name",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(grants))
}

pub async fn set_user_grants(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(grants): Json<Vec<UserGrant>>,
) -> ApiResult<StatusCode> {
    current.require(Permission::Users)?;
    grantable_user(&state, &id).await?;
    let mut seen = BTreeSet::new();
    for grant in &grants {
        require_role(&state, &grant.role).await?;
        if !seen.insert(&grant.deployment_id) {
            return Err(ApiError::bad_request("a deployment is listed twice"));
        }
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM deployment_grants WHERE user_id = ?").bind(&id).execute(&mut *tx).await?;
    for grant in &grants {
        let inserted = sqlx::query("INSERT INTO deployment_grants (deployment_id, user_id, role) SELECT id, ?, ? FROM deployments WHERE id = ?")
            .bind(&id)
            .bind(&grant.role)
            .bind(&grant.deployment_id)
            .execute(&mut *tx)
            .await?;
        if inserted.rows_affected() == 0 {
            return Err(ApiError::bad_request("one of the deployments no longer exists"));
        }
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
