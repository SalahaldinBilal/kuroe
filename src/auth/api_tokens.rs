//! Personal tokens for the read-only MCP endpoint. They work nowhere else in the API.

use serde::Serialize;
use sqlx::SqlitePool;

use super::User;
use crate::db::now;
use crate::deploy::tokens::{hash, random_secret};

pub const PREFIX: &str = "kuroe_pat_";
const TOUCH_EVERY_SECS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct ApiToken {
    pub id: String,
    pub user_id: String,
    pub username: String,
    pub name: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

pub fn valid_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && name.chars().count() <= 60
}

/// Returns the token and its value, which is shown once and never stored.
pub async fn create(db: &SqlitePool, user: &User, name: &str, expires_at: Option<i64>) -> sqlx::Result<(ApiToken, String)> {
    let value = format!("{PREFIX}{}", random_secret(32));
    let token = ApiToken {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: user.id.clone(),
        username: user.username.clone(),
        name: name.trim().to_owned(),
        created_at: now(),
        expires_at,
        last_used_at: None,
    };
    sqlx::query("INSERT INTO api_tokens (id, user_id, name, token_hash, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&token.id)
        .bind(&token.user_id)
        .bind(&token.name)
        .bind(hash(&value))
        .bind(token.created_at)
        .bind(token.expires_at)
        .execute(db)
        .await?;
    Ok((token, value))
}

pub async fn list(db: &SqlitePool, user_id: Option<&str>) -> sqlx::Result<Vec<ApiToken>> {
    sqlx::query_as(
        "SELECT t.id, t.user_id, u.username, t.name, t.created_at, t.expires_at, t.last_used_at
         FROM api_tokens t JOIN users u ON u.id = t.user_id
         WHERE ? IS NULL OR t.user_id = ?
         ORDER BY u.username, t.created_at",
    )
    .bind(user_id)
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// With a user, only that user's token is removed. Returns whether one was.
pub async fn revoke(db: &SqlitePool, id: &str, user_id: Option<&str>) -> sqlx::Result<bool> {
    let removed = sqlx::query("DELETE FROM api_tokens WHERE id = ? AND (? IS NULL OR user_id = ?)")
        .bind(id)
        .bind(user_id)
        .bind(user_id)
        .execute(db)
        .await?
        .rows_affected();
    Ok(removed > 0)
}

/// The token's user, while the token exists and hasn't expired.
pub async fn authenticate(db: &SqlitePool, value: &str) -> sqlx::Result<Option<User>> {
    if !value.starts_with(PREFIX) {
        return Ok(None);
    }
    let row: Option<(String, Option<i64>, Option<i64>)> =
        sqlx::query_as("SELECT id, expires_at, last_used_at FROM api_tokens WHERE token_hash = ?")
            .bind(hash(value))
            .fetch_optional(db)
            .await?;
    let Some((id, expires_at, last_used_at)) = row else { return Ok(None) };
    let now = now();
    if expires_at.is_some_and(|at| at <= now) {
        return Ok(None);
    }
    if last_used_at.is_none_or(|at| now - at >= TOUCH_EVERY_SECS) {
        sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?").bind(now).bind(&id).execute(db).await?;
    }
    sqlx::query_as(
        "SELECT u.id, u.username, u.role, u.created_at FROM users u JOIN api_tokens t ON t.user_id = u.id WHERE t.id = ?",
    )
    .bind(&id)
    .fetch_optional(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, create_user};
    use crate::db::test_pool;

    #[tokio::test]
    async fn tokens_are_hashed_expire_and_belong_to_their_user() {
        let db = test_pool().await;
        let boss = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let viewer = create_user(&db, "vera", "vera-password-1", Role::viewer()).await.unwrap();
        let (token, value) = create(&db, &viewer, " claude ", None).await.unwrap();
        assert_eq!(token.name, "claude");
        assert!(value.starts_with(PREFIX));

        let stored: String = sqlx::query_scalar("SELECT token_hash FROM api_tokens").fetch_one(&db).await.unwrap();
        assert_ne!(stored, value);
        assert_eq!(authenticate(&db, &value).await.unwrap().map(|u| u.id), Some(viewer.id.clone()));
        assert!(list(&db, Some(&viewer.id)).await.unwrap()[0].last_used_at.is_some());
        assert!(authenticate(&db, "kuroe_pat_0000").await.unwrap().is_none());
        assert!(authenticate(&db, "kuroe_0000").await.unwrap().is_none(), "a deploy token isn't a personal token");

        let (_, expired) = create(&db, &viewer, "old", Some(now() - 1)).await.unwrap();
        assert!(authenticate(&db, &expired).await.unwrap().is_none());

        assert!(create(&db, &viewer, "claude", None).await.is_err(), "names are unique per user");
        create(&db, &boss, "claude", None).await.unwrap();
        assert_eq!(list(&db, None).await.unwrap().len(), 3);
        assert_eq!(list(&db, Some(&viewer.id)).await.unwrap().len(), 2);

        assert!(!revoke(&db, &token.id, Some(&boss.id)).await.unwrap(), "someone else's token stays");
        assert!(revoke(&db, &token.id, Some(&viewer.id)).await.unwrap());
        assert!(authenticate(&db, &value).await.unwrap().is_none());

        sqlx::query("DELETE FROM users WHERE id = ?").bind(&viewer.id).execute(&db).await.unwrap();
        assert_eq!(list(&db, None).await.unwrap().len(), 1, "a deleted user's tokens go with them");
    }
}
