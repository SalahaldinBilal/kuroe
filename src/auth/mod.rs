pub mod api_tokens;
mod password;
mod rate_limit;
mod rbac;
mod session;
pub mod setup_code;

pub use password::{hash_password, random_password, verify_password};
pub use rate_limit::LoginLimiter;
pub use rbac::{Access, Permission, Role, RoleDef, Scope};
pub use session::{SESSION_COOKIE, SESSION_TTL_SECS, create_session, delete_session, session_user};

use anyhow::bail;
use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    #[sqlx(try_from = "String")]
    pub role: Role,
    pub created_at: i64,
}

pub async fn has_users(pool: &SqlitePool) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)").fetch_one(pool).await
}

pub async fn create_user(pool: &SqlitePool, username: &str, password: &str, role: Role) -> anyhow::Result<User> {
    let user = User {
        id: uuid::Uuid::new_v4().to_string(),
        username: username.to_owned(),
        role,
        created_at: crate::db::now(),
    };
    sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&user.id)
        .bind(&user.username)
        .bind(hash_password(password)?)
        .bind(user.role.as_str())
        .bind(user.created_at)
        .execute(pool)
        .await?;
    Ok(user)
}

pub async fn reset_password(pool: &SqlitePool, username: &str, password: &str) -> anyhow::Result<()> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE username = ?").bind(username).fetch_optional(pool).await?;
    let Some(id) = id else { bail!("no user named {username}") };
    set_password(pool, &id, password, None).await?;
    Ok(())
}

/// Ends every session of the user except `keep_session`. Returns false when the user doesn't exist.
pub async fn set_password(pool: &SqlitePool, user_id: &str, password: &str, keep_session: Option<&str>) -> anyhow::Result<bool> {
    let hash = hash_password(password)?;
    let mut tx = pool.begin().await?;
    let updated =
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?").bind(hash).bind(user_id).execute(&mut *tx).await?;
    if updated.rows_affected() == 0 {
        return Ok(false);
    }
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token IS NOT ?")
        .bind(user_id)
        .bind(keep_session)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}
