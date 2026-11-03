pub mod cloudflare;
pub mod github;
pub mod registry;
pub mod repos;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::db::now;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    GithubApp,
    GithubToken,
    Gitlab,
    Gitea,
    Git,
    Registry,
    Cloudflare,
    Certificate,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::GithubApp => "github_app",
            Kind::GithubToken => "github_token",
            Kind::Gitlab => "gitlab",
            Kind::Gitea => "gitea",
            Kind::Git => "git",
            Kind::Registry => "registry",
            Kind::Cloudflare => "cloudflare",
            Kind::Certificate => "certificate",
        }
    }

    pub fn parse(text: &str) -> Option<Kind> {
        serde_json::from_value(Value::String(text.to_owned())).ok()
    }

    pub fn clones_git(self) -> bool {
        matches!(self, Kind::GithubApp | Kind::GithubToken | Kind::Gitlab | Kind::Gitea | Kind::Git)
    }

    pub fn pulls_images(self) -> bool {
        matches!(self, Kind::Registry | Kind::GithubToken | Kind::Gitlab)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub host: String,
    pub username: String,
    #[serde(skip)]
    pub secret: String,
    pub meta: Value,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub expires_at: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    kind: String,
    name: String,
    host: String,
    username: String,
    secret: String,
    meta: String,
    created_at: i64,
    last_used_at: Option<i64>,
    expires_at: Option<i64>,
}

impl TryFrom<Row> for Identity {
    type Error = anyhow::Error;

    fn try_from(row: Row) -> anyhow::Result<Self> {
        let secret = crate::secrets::sealer().open(&crate::secrets::identity_aad(&row.id), &row.secret)?;
        Ok(Identity {
            secret,
            kind: Kind::parse(&row.kind).with_context(|| format!("identity {} has unknown kind {}", row.name, row.kind))?,
            meta: serde_json::from_str(&row.meta).unwrap_or_else(|_| json!({})),
            id: row.id,
            name: row.name,
            host: row.host,
            username: row.username,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            expires_at: row.expires_at,
        })
    }
}

pub async fn get(db: &SqlitePool, id: &str) -> anyhow::Result<Option<Identity>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, kind, name, host, username, secret, meta, created_at, last_used_at, expires_at FROM identities WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    row.map(Identity::try_from).transpose()
}

pub async fn by_name(db: &SqlitePool, name: &str) -> anyhow::Result<Option<Identity>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, kind, name, host, username, secret, meta, created_at, last_used_at, expires_at FROM identities WHERE name = ?",
    )
    .bind(name)
    .fetch_optional(db)
    .await?;
    row.map(Identity::try_from).transpose()
}

pub async fn list(db: &SqlitePool) -> anyhow::Result<Vec<Identity>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, kind, name, host, username, secret, meta, created_at, last_used_at, expires_at FROM identities ORDER BY name",
    )
    .fetch_all(db)
    .await?;
    rows.into_iter().map(Identity::try_from).collect()
}

pub struct NewIdentity<'a> {
    pub id: Option<&'a str>,
    pub kind: Kind,
    pub name: &'a str,
    pub host: &'a str,
    pub username: &'a str,
    pub secret: &'a str,
    pub meta: &'a Value,
    pub expires_at: Option<i64>,
}

pub async fn insert(db: &SqlitePool, new: &NewIdentity<'_>) -> sqlx::Result<String> {
    let id = new.id.map(str::to_owned).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    sqlx::query(
        "INSERT INTO identities (id, kind, name, host, username, secret, meta, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(new.kind.as_str())
    .bind(new.name)
    .bind(new.host)
    .bind(new.username)
    .bind(crate::secrets::sealer().seal(&crate::secrets::identity_aad(&id), new.secret))
    .bind(new.meta.to_string())
    .bind(now())
    .bind(new.expires_at)
    .execute(db)
    .await?;
    Ok(id)
}

pub async fn touch(db: &SqlitePool, id: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE identities SET last_used_at = ? WHERE id = ?").bind(now()).bind(id).execute(db).await?;
    Ok(())
}

pub async fn save_check(db: &SqlitePool, id: &str, meta: &Value, expires_at: Option<i64>) -> sqlx::Result<()> {
    sqlx::query("UPDATE identities SET meta = ?, expires_at = ? WHERE id = ?")
        .bind(meta.to_string())
        .bind(expires_at)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub fn valid_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}

/// `https://Host:443/x` and `host` both become `host`, so the URL a token is sent to can be compared to its host.
pub fn normalize_host(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    let host = host.split("://").nth(1).unwrap_or(&host).to_owned();
    let host = host.split('/').next().unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or_default();
    host.strip_suffix(":443").unwrap_or(host).to_owned()
}

pub fn default_host(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::GithubApp | Kind::GithubToken => Some("github.com"),
        Kind::Gitlab => Some("gitlab.com"),
        Kind::Cloudflare => Some("api.cloudflare.com"),
        _ => None,
    }
}

pub fn host_of(identity: &Identity) -> String {
    if identity.host.trim().is_empty() {
        default_host(identity.kind).unwrap_or_default().to_owned()
    } else {
        normalize_host(&identity.host)
    }
}

/// A saved token only goes to the host it was saved for.
pub fn check_git_url(identity: &Identity, url: &str) -> anyhow::Result<()> {
    if !identity.kind.clones_git() {
        bail!("{} is not a Git login", identity.name);
    }
    let Some(rest) = url.trim().strip_prefix("https://") else { bail!("saved logins only work with https Git URLs") };
    let url_host = normalize_host(rest);
    let expected = host_of(identity);
    if url_host != expected {
        bail!("{} is for {expected}, but the repository is on {url_host}", identity.name);
    }
    Ok(())
}

/// The user and password a clone sends over https.
pub async fn git_credentials(db: &SqlitePool, http: &reqwest::Client, identity: &Identity) -> anyhow::Result<(String, String)> {
    let creds = match identity.kind {
        Kind::GithubToken => ("x-access-token".to_owned(), identity.secret.clone()),
        Kind::Gitlab => ("oauth2".to_owned(), identity.secret.clone()),
        Kind::Gitea | Kind::Git => {
            let user = if identity.username.is_empty() { "x-access-token" } else { &identity.username };
            (user.to_owned(), identity.secret.clone())
        }
        Kind::GithubApp => ("x-access-token".to_owned(), github::installation_token_for(http, identity).await?.0),
        Kind::Registry | Kind::Cloudflare | Kind::Certificate => bail!("{} is not a Git login", identity.name),
    };
    touch(db, &identity.id).await?;
    Ok(creds)
}

/// Expiry dates come as RFC 3339, `2025-06-01 00:00:00 UTC` (GitHub header), or a plain date (GitLab).
pub fn parse_expiry(text: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    use time::macros::format_description;
    let text = text.trim();
    if let Ok(t) = time::OffsetDateTime::parse(text, &Rfc3339) {
        return Some(t.unix_timestamp());
    }
    let with_offset = format_description!("[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]");
    if let Ok(t) = time::OffsetDateTime::parse(text, with_offset) {
        return Some(t.unix_timestamp());
    }
    let utc = format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");
    if let Ok(t) = time::PrimitiveDateTime::parse(text, utc) {
        return Some(t.assume_utc().unix_timestamp());
    }
    let day = format_description!("[year]-[month]-[day]");
    time::Date::parse(text, day).ok().map(|d| d.midnight().assume_utc().unix_timestamp())
}

pub struct Checked {
    pub message: String,
    pub expires_at: Option<i64>,
    pub meta: Value,
}

pub fn api_base(identity: &Identity) -> String {
    format!("https://{}", host_of(identity))
}

/// Talks to the provider with the saved secret. On success the result's meta replaces the saved one.
pub async fn check(http: &reqwest::Client, identity: &Identity) -> anyhow::Result<Checked> {
    let mut meta = identity.meta.clone();
    let (message, expires_at) = match identity.kind {
        Kind::GithubToken => {
            let (login, expires) = github::check_token(http, &identity.secret).await?;
            meta["login"] = json!(login);
            (format!("Works, signed in as {login}"), expires.as_deref().and_then(parse_expiry))
        }
        Kind::GithubApp => {
            if meta["installation_id"].as_u64().is_none() {
                let Some(installation) = github::find_installation(http, identity).await? else {
                    bail!("the app is not installed anywhere yet, install it and pick repositories");
                };
                meta["installation_id"] = json!(installation);
            }
            let installed = Identity { meta: meta.clone(), ..identity.clone() };
            let repos = github::repositories(http, &installed).await?;
            meta["repositories"] = json!(repos.len());
            (format!("Works, can read {} repositories", repos.len()), None)
        }
        Kind::Gitlab => {
            let url = format!("{}/api/v4/personal_access_tokens/self", api_base(identity));
            let response = http.get(url).header("private-token", &identity.secret).send().await?;
            if !response.status().is_success() {
                bail!("GitLab refused the token ({})", response.status());
            }
            let body: Value = response.json().await?;
            meta["scopes"] = body["scopes"].clone();
            (format!("Works, token {}", body["name"].as_str().unwrap_or("unnamed")), body["expires_at"].as_str().and_then(parse_expiry))
        }
        Kind::Gitea => {
            let url = format!("{}/api/v1/user", api_base(identity));
            let response = http.get(url).header("authorization", format!("token {}", identity.secret)).send().await?;
            if !response.status().is_success() {
                bail!("{} refused the token ({})", host_of(identity), response.status());
            }
            let body: Value = response.json().await?;
            (format!("Works, signed in as {}", body["login"].as_str().unwrap_or("unknown")), None)
        }
        Kind::Git => bail!("a plain Git login can't be checked on its own, deploy with it to try it"),
        Kind::Certificate => {
            let chain = identity.meta["chain"].as_str().unwrap_or_default();
            let parsed = crate::proxy::certs::parse_chain(chain)?;
            crate::proxy::certs::check_key(chain, &identity.secret)?;
            meta["sans"] = json!(parsed.sans);
            meta["not_after"] = json!(parsed.not_after);
            (format!("Valid, covers {}", parsed.sans.join(", ")), Some(parsed.not_after))
        }
        Kind::Registry => (registry::check_login(http, identity).await?, None),
        Kind::Cloudflare => {
            let checked = cloudflare::check(http, &identity.secret).await?;
            let count = checked.zones.len();
            meta["zones"] = json!(checked.zones);
            (format!("Works, can reach {count} zones"), checked.expires_on.as_deref().and_then(parse_expiry))
        }
    };
    Ok(Checked { message, expires_at, meta })
}

/// Where each saved identity is used. Deployment sources hold the identity id.
pub fn uses(spec: &crate::deploy::Spec) -> Vec<&str> {
    use crate::deploy::Source;
    match &spec.source {
        Source::Git { identity: Some(id), .. } => vec![id.as_str()],
        Source::Image { registry_identity: Some(id), .. } => vec![id.as_str()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(kind: Kind, host: &str) -> Identity {
        Identity {
            id: "i1".into(),
            kind,
            name: "acme".into(),
            host: host.into(),
            username: String::new(),
            secret: "s3cret".into(),
            meta: json!({}),
            created_at: 0,
            last_used_at: None,
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn clone_login_per_kind() {
        let db = crate::db::test_pool().await;
        let http = reqwest::Client::new();
        let meta = json!({});
        let mut ids = Vec::new();
        for (kind, name, user) in [(Kind::GithubToken, "gh", ""), (Kind::Gitlab, "gl", ""), (Kind::Git, "plain", "deploy-bot"), (Kind::Gitea, "tea", "")] {
            let new = NewIdentity { id: None, kind, name, host: "h", username: user, secret: "tok", meta: &meta, expires_at: None };
            ids.push(insert(&db, &new).await.unwrap());
        }
        let mut logins = Vec::new();
        for id in &ids {
            let saved = get(&db, id).await.unwrap().unwrap();
            logins.push(git_credentials(&db, &http, &saved).await.unwrap());
        }
        let expected = [("x-access-token", "tok"), ("oauth2", "tok"), ("deploy-bot", "tok"), ("x-access-token", "tok")];
        assert_eq!(logins.iter().map(|(u, p)| (u.as_str(), p.as_str())).collect::<Vec<_>>(), expected);
        assert!(get(&db, &ids[0]).await.unwrap().unwrap().last_used_at.is_some(), "using a login records when");

        let registry = NewIdentity { id: None, kind: Kind::Registry, name: "r", host: "ghcr.io", username: "u", secret: "p", meta: &meta, expires_at: None };
        let registry = get(&db, &insert(&db, &registry).await.unwrap()).await.unwrap().unwrap();
        assert!(git_credentials(&db, &http, &registry).await.is_err());
    }

    #[test]
    fn expiry_dates_in_every_provider_format() {
        assert_eq!(parse_expiry("2025-06-01T00:00:00Z"), Some(1_748_736_000));
        assert_eq!(parse_expiry("2025-06-01 00:00:00 UTC"), Some(1_748_736_000));
        assert_eq!(parse_expiry("2025-06-01 02:00:00 +0200"), Some(1_748_736_000));
        assert_eq!(parse_expiry("2025-06-01"), Some(1_748_736_000));
        assert_eq!(parse_expiry("soon"), None);
    }

    #[test]
    fn hosts_are_compared_without_scheme_port_or_user() {
        assert_eq!(normalize_host("https://Git.Example.com:443/group"), "git.example.com");
        assert_eq!(normalize_host("git.example.com:8443"), "git.example.com:8443");
        assert_eq!(normalize_host("user@gitlab.com/x"), "gitlab.com");
    }

    #[test]
    fn a_token_only_goes_to_its_own_host() {
        let gh = identity(Kind::GithubToken, "");
        assert!(check_git_url(&gh, "https://github.com/acme/api.git").is_ok());
        let err = check_git_url(&gh, "https://evil.example.com/acme/api.git").unwrap_err().to_string();
        assert_eq!(err, "acme is for github.com, but the repository is on evil.example.com");
        assert!(check_git_url(&gh, "git@github.com:acme/api.git").is_err());

        let gitea = identity(Kind::Gitea, "https://git.example.com");
        assert!(check_git_url(&gitea, "https://git.example.com/team/app.git").is_ok());
        assert!(check_git_url(&identity(Kind::Registry, "ghcr.io"), "https://ghcr.io/x").is_err());
    }
}
