//! Saved secrets: a named value that deployments point at instead of holding a copy.
//! The only code that reads or writes the `saved_secrets` table.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::db::now;
use crate::deploy::spec::{GIT_TOKEN_REF, Source, Spec};
use crate::secrets::{saved_secret_aad, sealer};

/// Pastes shorter than this aren't matched, so short common words don't turn into references.
pub const MATCH_MIN_LEN: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct SavedSecret {
    pub id: String,
    pub name: String,
    pub description: String,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub use crate::deploy::spec::valid_name;

pub async fn list(db: &SqlitePool) -> sqlx::Result<Vec<SavedSecret>> {
    sqlx::query_as("SELECT id, name, description, created_by, created_at, updated_at FROM saved_secrets ORDER BY name").fetch_all(db).await
}

pub async fn get(db: &SqlitePool, id: &str) -> sqlx::Result<Option<SavedSecret>> {
    sqlx::query_as("SELECT id, name, description, created_by, created_at, updated_at FROM saved_secrets WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn by_name(db: &SqlitePool, name: &str) -> sqlx::Result<Option<SavedSecret>> {
    sqlx::query_as("SELECT id, name, description, created_by, created_at, updated_at FROM saved_secrets WHERE name = ?")
        .bind(name)
        .fetch_optional(db)
        .await
}

pub async fn insert(db: &SqlitePool, name: &str, description: &str, value: &str, created_by: &str) -> sqlx::Result<SavedSecret> {
    let id = uuid::Uuid::new_v4().to_string();
    let at = now();
    sqlx::query(
        "INSERT INTO saved_secrets (id, name, description, value, fingerprint, created_by, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(description)
    .bind(sealer().seal(&saved_secret_aad(&id), value))
    .bind(sealer().fingerprint(value))
    .bind(created_by)
    .bind(at)
    .bind(at)
    .execute(db)
    .await?;
    Ok(SavedSecret { id, name: name.to_owned(), description: description.to_owned(), created_by: Some(created_by.to_owned()), created_at: at, updated_at: at })
}

/// `value` of `None` keeps the stored one. Returns false when there is no such secret.
pub async fn update(db: &SqlitePool, id: &str, name: &str, description: &str, value: Option<&str>) -> sqlx::Result<bool> {
    let done = match value {
        Some(value) => {
            sqlx::query("UPDATE saved_secrets SET name = ?, description = ?, value = ?, fingerprint = ?, updated_at = ? WHERE id = ?")
                .bind(name)
                .bind(description)
                .bind(sealer().seal(&saved_secret_aad(id), value))
                .bind(sealer().fingerprint(value))
                .bind(now())
                .bind(id)
                .execute(db)
                .await?
        }
        None => {
            sqlx::query("UPDATE saved_secrets SET name = ?, description = ?, updated_at = ? WHERE id = ?")
                .bind(name)
                .bind(description)
                .bind(now())
                .bind(id)
                .execute(db)
                .await?
        }
    };
    Ok(done.rows_affected() > 0)
}

pub async fn delete(db: &SqlitePool, id: &str) -> sqlx::Result<bool> {
    Ok(sqlx::query("DELETE FROM saved_secrets WHERE id = ?").bind(id).execute(db).await?.rows_affected() > 0)
}

pub async fn open_value(db: &SqlitePool, id: &str) -> anyhow::Result<Option<String>> {
    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM saved_secrets WHERE id = ?").bind(id).fetch_optional(db).await?;
    stored.map(|v| sealer().open(&saved_secret_aad(id), &v)).transpose()
}

pub async fn match_value(db: &SqlitePool, value: &str) -> sqlx::Result<Option<SavedSecret>> {
    if value.chars().count() < MATCH_MIN_LEN {
        return Ok(None);
    }
    sqlx::query_as(
        "SELECT id, name, description, created_by, created_at, updated_at FROM saved_secrets WHERE fingerprint = ? ORDER BY name LIMIT 1",
    )
        .bind(sealer().fingerprint(value))
        .fetch_optional(db)
        .await
}

/// The saved secret ids a spec points at.
pub fn uses(spec: &Spec) -> BTreeSet<&str> {
    spec.secret_refs.values().map(String::as_str).collect()
}

/// Names for the ids a spec points at, for showing references to anyone who can view the deployment.
pub async fn names(db: &SqlitePool, spec: &Spec) -> sqlx::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for id in uses(spec) {
        if let Some(s) = get(db, id).await? {
            out.insert(s.id, s.name);
        }
    }
    Ok(out)
}

/// Fills referenced env values and the git token with the saved values, right before they're used.
pub async fn resolve(db: &SqlitePool, spec: &Spec) -> anyhow::Result<Spec> {
    let mut out = spec.clone();
    for (key, id) in &spec.secret_refs {
        let what = if key == GIT_TOKEN_REF { "the git access token".to_owned() } else { key.clone() };
        let value = open_value(db, id)
            .await
            .with_context(|| format!("opening the saved secret for {what}"))?
            .with_context(|| format!("{what} points at a saved secret that was deleted"))?;
        if key == GIT_TOKEN_REF {
            if let Source::Git { token, .. } = &mut out.source {
                *token = Some(value);
            }
        } else {
            out.container.env.insert(key.clone(), value);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;

    async fn owner(db: &SqlitePool) -> String {
        crate::auth::create_user(db, "boss", "boss-password-1", crate::auth::Role::owner()).await.unwrap().id
    }

    #[tokio::test]
    async fn values_are_sealed_and_found_by_value() {
        let db = test_pool().await;
        let by = owner(&db).await;
        let s = insert(&db, "db-password", "Postgres", "hunter2-long", &by).await.unwrap();
        let raw: String = sqlx::query_scalar("SELECT value FROM saved_secrets").fetch_one(&db).await.unwrap();
        assert!(crate::secrets::is_sealed(&raw) && !raw.contains("hunter2"));
        assert_eq!(open_value(&db, &s.id).await.unwrap().as_deref(), Some("hunter2-long"));

        assert_eq!(match_value(&db, "hunter2-long").await.unwrap().map(|m| m.name), Some("db-password".into()));
        assert_eq!(match_value(&db, "something-else").await.unwrap(), None);
        insert(&db, "pin", "", "1234", &by).await.unwrap();
        assert_eq!(match_value(&db, "1234").await.unwrap(), None, "short values never match");

        assert!(update(&db, &s.id, "db-password", "Postgres main", None).await.unwrap());
        assert_eq!(open_value(&db, &s.id).await.unwrap().as_deref(), Some("hunter2-long"), "no value keeps the old one");
        update(&db, &s.id, "db-password", "Postgres main", Some("rotated-value")).await.unwrap();
        assert_eq!(match_value(&db, "hunter2-long").await.unwrap(), None);
        assert!(match_value(&db, "rotated-value").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn resolve_fills_env_and_token_and_fails_on_a_deleted_secret() {
        let db = test_pool().await;
        let by = owner(&db).await;
        let pass = insert(&db, "db-password", "", "hunter2-long", &by).await.unwrap();
        let token = insert(&db, "gh-token", "", "ghp_abcdefgh", &by).await.unwrap();
        let mut spec = Spec::new(Source::Git {
            url: "https://example.com/r.git".into(),
            reference: None,
            context: None,
            dockerfile: None,
            token: None,
            identity: None,
            compose_file: None,
        });
        spec.container.env = BTreeMap::from([("DB_PASSWORD".into(), String::new()), ("PORT".into(), "3000".into())]);
        spec.secret_refs = BTreeMap::from([("DB_PASSWORD".into(), pass.id.clone()), (GIT_TOKEN_REF.into(), token.id.clone())]);

        let out = resolve(&db, &spec).await.unwrap();
        assert_eq!(out.container.env["DB_PASSWORD"], "hunter2-long");
        assert_eq!(out.container.env["PORT"], "3000");
        assert!(matches!(&out.source, Source::Git { token: Some(t), .. } if t == "ghp_abcdefgh"));
        let mut found: Vec<String> = names(&db, &spec).await.unwrap().into_values().collect();
        found.sort();
        assert_eq!(found, ["db-password", "gh-token"]);

        delete(&db, &pass.id).await.unwrap();
        let err = format!("{:#}", resolve(&db, &spec).await.unwrap_err());
        assert!(err.contains("DB_PASSWORD points at a saved secret that was deleted"), "{err}");
    }
}
