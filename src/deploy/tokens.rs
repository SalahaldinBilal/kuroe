use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::db::now;

pub const PREFIX: &str = "kuroe_";

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Token {
    pub id: String,
    pub deployment_id: String,
    pub name: String,
    pub action: String,
    pub host_mounts: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub token_id: String,
    pub name: String,
    pub deployment_id: String,
    pub action: String,
    pub host_mounts: bool,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

pub fn random_secret(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    hex(&buf)
}

pub async fn create(
    db: &SqlitePool,
    deployment_id: &str,
    name: &str,
    host_mounts: bool,
    created_by: &str,
) -> sqlx::Result<(Token, String)> {
    let plaintext = format!("{PREFIX}{}", random_secret(32));
    let token = Token {
        id: uuid::Uuid::new_v4().to_string(),
        deployment_id: deployment_id.to_owned(),
        name: name.to_owned(),
        action: "deploy".into(),
        host_mounts,
        created_at: now(),
        last_used_at: None,
    };
    sqlx::query(
        "INSERT INTO deploy_tokens (id, deployment_id, name, action, token_hash, host_mounts, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&token.id)
    .bind(&token.deployment_id)
    .bind(&token.name)
    .bind(&token.action)
    .bind(hash(&plaintext))
    .bind(token.host_mounts)
    .bind(created_by)
    .bind(token.created_at)
    .execute(db)
    .await?;
    Ok((token, plaintext))
}

pub async fn list(db: &SqlitePool, deployment_id: &str) -> sqlx::Result<Vec<Token>> {
    sqlx::query_as(
        "SELECT id, deployment_id, name, action, host_mounts, created_at, last_used_at
         FROM deploy_tokens WHERE deployment_id = ? ORDER BY created_at",
    )
    .bind(deployment_id)
    .fetch_all(db)
    .await
}

pub async fn delete(db: &SqlitePool, deployment_id: &str, id: &str) -> sqlx::Result<bool> {
    let deleted = sqlx::query("DELETE FROM deploy_tokens WHERE id = ? AND deployment_id = ?")
        .bind(id)
        .bind(deployment_id)
        .execute(db)
        .await?
        .rows_affected();
    Ok(deleted > 0)
}

pub async fn authenticate(db: &SqlitePool, plaintext: &str) -> sqlx::Result<Option<Grant>> {
    if !plaintext.starts_with(PREFIX) {
        return Ok(None);
    }
    let row: Option<(String, String, String, String, bool)> =
        sqlx::query_as("SELECT id, name, deployment_id, action, host_mounts FROM deploy_tokens WHERE token_hash = ?")
            .bind(hash(plaintext))
            .fetch_optional(db)
            .await?;
    let Some((token_id, name, deployment_id, action, host_mounts)) = row else { return Ok(None) };
    sqlx::query("UPDATE deploy_tokens SET last_used_at = ? WHERE id = ?").bind(now()).bind(&token_id).execute(db).await?;
    Ok(Some(Grant { token_id, name, deployment_id, action, host_mounts }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, create_user};
    use crate::db::test_pool;
    use crate::deploy::{self, Source, Spec};

    #[tokio::test]
    async fn token_is_stored_hashed_and_authenticates_once_known() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let d = deploy::create(&db, "api", &Spec::new(Source::image("nginx")), &owner).await.unwrap();
        let (token, plaintext) = create(&db, &d.id, "ci", false, &owner.id).await.unwrap();

        let stored: String = sqlx::query_scalar("SELECT token_hash FROM deploy_tokens").fetch_one(&db).await.unwrap();
        assert_ne!(stored, plaintext);
        assert_eq!(stored.len(), 64);

        let grant = authenticate(&db, &plaintext).await.unwrap().unwrap();
        assert_eq!(grant, Grant { token_id: token.id, name: "ci".into(), deployment_id: d.id.clone(), action: "deploy".into(), host_mounts: false });
        assert!(authenticate(&db, "kuroe_0000").await.unwrap().is_none());
        assert!(authenticate(&db, "not-a-token").await.unwrap().is_none());
        assert!(list(&db, &d.id).await.unwrap()[0].last_used_at.is_some());
    }
}
