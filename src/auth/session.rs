use rand::RngCore;
use sqlx::SqlitePool;

use super::User;
use crate::db::now;

pub const SESSION_COOKIE: &str = "kuroe_session";
pub const SESSION_TTL_SECS: i64 = 14 * 24 * 60 * 60;

pub async fn create_session(pool: &SqlitePool, user_id: &str) -> sqlx::Result<String> {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let created = now();
    sqlx::query("INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)")
        .bind(&token)
        .bind(user_id)
        .bind(created)
        .bind(created + SESSION_TTL_SECS)
        .execute(pool)
        .await?;
    Ok(token)
}

pub async fn session_user(pool: &SqlitePool, token: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as(
        "SELECT u.id, u.username, u.role, u.created_at FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token = ? AND s.expires_at > ?",
    )
    .bind(token)
    .bind(now())
    .fetch_optional(pool)
    .await
}

pub async fn delete_session(pool: &SqlitePool, token: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token = ?").bind(token).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, create_user};
    use crate::db::test_pool;

    #[tokio::test]
    async fn session_lookup_and_expiry() {
        let pool = test_pool().await;
        let user = create_user(&pool, "ana", "pw-ana-123", Role::viewer()).await.unwrap();
        let token = create_session(&pool, &user.id).await.unwrap();

        let found = session_user(&pool, &token).await.unwrap().unwrap();
        assert_eq!(found.username, "ana");

        sqlx::query("UPDATE sessions SET expires_at = ? WHERE token = ?")
            .bind(now() - 1)
            .bind(&token)
            .execute(&pool)
            .await
            .unwrap();
        assert!(session_user(&pool, &token).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn logout_removes_session() {
        let pool = test_pool().await;
        let user = create_user(&pool, "ben", "pw-ben-123", Role::admin()).await.unwrap();
        let token = create_session(&pool, &user.id).await.unwrap();
        delete_session(&pool, &token).await.unwrap();
        assert!(session_user(&pool, &token).await.unwrap().is_none());
    }
}
