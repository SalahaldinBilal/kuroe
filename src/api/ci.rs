use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::SqlitePool;

use super::access;
use super::{API_BASE, AppState};
use super::error::{ApiError, ApiResult};
use super::extract::{Caller, CurrentUser};
use crate::auth::{Permission, Role};
use crate::deploy::{self, Deployment, Source, tokens};

#[derive(Deserialize, Default)]
pub struct DeployRequest {
    tag: Option<String>,
}

pub(super) async fn start(state: &AppState, d: Deployment, host_mounts: bool) -> ApiResult<()> {
    let Some(revision) = deploy::begin_deploy(&state.db, &d.id).await? else {
        return Err(ApiError::new(StatusCode::CONFLICT, "a deploy is already running"));
    };
    let fresh = deploy::get(&state.db, &d.id).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such deployment"))?;
    let runner = state.runner.clone();
    tokio::spawn(async move { runner.deploy(fresh, revision, host_mounts).await });
    Ok(())
}

pub async fn deploy(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let request: DeployRequest = if body.iter().all(u8::is_ascii_whitespace) {
        DeployRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("body must be JSON like {{\"tag\": \"v2\"}}: {e}")))?
    };
    let (d, host_mounts) = match &caller {
        Caller::User(current) => {
            let d = access::deployment(&state, current, &id, Permission::DepDeploy).await?;
            (d, access::host_mounts(&state, current).await?)
        }
        Caller::Token(grant) => {
            if grant.deployment_id != id || grant.action != "deploy" {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "this token is for a different deployment"));
            }
            let d = deploy::get(&state.db, &id).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such deployment"))?;
            (d, grant.host_mounts)
        }
    };
    let tag = request.tag;
    let d = match tag.as_deref() {
        Some(tag) => {
            let spec = d.spec.with_tag(tag).map_err(ApiError::bad_request)?;
            deploy::update_spec(&state.db, &id, &spec).await?;
            Deployment { spec, ..d }
        }
        None => d,
    };
    let revision_hint = d.revision + 1;
    start(&state, d, host_mounts).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "revision": revision_hint, "tag": tag }))))
}

#[derive(Deserialize)]
pub struct NewToken {
    name: String,
}

pub async fn list_tokens(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    access::deployment(&state, &current, &id, Permission::DepCi).await?;
    Ok(Json(json!(tokens::list(&state.db, &id).await?)))
}

pub async fn create_token(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<NewToken>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    access::deployment(&state, &current, &id, Permission::DepCi).await?;
    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::bad_request("give the token a name of up to 64 characters"));
    }
    let host_mounts = access::host_mounts(&state, &current).await?;
    let (token, plaintext) = tokens::create(&state.db, &id, name, host_mounts, &current.user.id).await?;
    Ok((StatusCode::CREATED, Json(json!({ "token": token, "secret": plaintext }))))
}

pub async fn delete_token(
    State(state): State<AppState>,
    current: CurrentUser,
    Path((id, token)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    access::deployment(&state, &current, &id, Permission::DepCi).await?;
    if !tokens::delete(&state.db, &id, &token).await? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such token"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn public_url(db: &SqlitePool) -> ApiResult<String> {
    let url: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'public_url'").fetch_optional(db).await?;
    url.map(|u| u.trim_end_matches('/').to_owned())
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "set kuroe's public URL in Settings first, CI needs it to reach this server"))
}

/// Which provider's push webhook fits a deployment: from its login, else from the repository host.
pub fn hook_provider(url: &str, login: Option<crate::identity::Kind>) -> &'static str {
    use crate::identity::Kind;
    match login {
        Some(Kind::GithubApp | Kind::GithubToken) => return "github",
        Some(Kind::Gitlab) => return "gitlab",
        Some(Kind::Gitea) => return "gitea",
        _ => {}
    }
    let host = crate::identity::normalize_host(url);
    if host == "github.com" {
        "github"
    } else if host.contains("gitlab") {
        "gitlab"
    } else {
        "gitea"
    }
}

fn hook_urls(base: &str, id: &str) -> Value {
    json!({
        "github": format!("{base}{API_BASE}/hooks/github/{id}"),
        "gitlab": format!("{base}{API_BASE}/hooks/gitlab/{id}"),
        "gitea": format!("{base}{API_BASE}/hooks/gitea/{id}"),
    })
}

async fn login_of(state: &AppState, d: &Deployment) -> ApiResult<Option<crate::identity::Identity>> {
    let Source::Git { identity: Some(id), .. } = &d.spec.source else { return Ok(None) };
    Ok(crate::identity::get(&state.db, id).await?)
}

/// The GitHub App, if the deployment clones through one. Its pushes arrive on the app's own hook.
async fn app_name(state: &AppState, d: &Deployment) -> ApiResult<Option<String>> {
    let Source::Git { identity: Some(id), .. } = &d.spec.source else { return Ok(None) };
    let saved = crate::identity::get(&state.db, id).await?;
    Ok(saved.filter(|i| i.kind == crate::identity::Kind::GithubApp).map(|i| i.name))
}

/// Where a deployment's push webhook goes, and whether kuroe can add it through the login.
pub(super) async fn hook_setup(state: &AppState, d: &Deployment) -> ApiResult<Value> {
    let urls = public_url(&state.db).await.ok().map(|u| hook_urls(&u, &d.id));
    let login = login_of(state, d).await?;
    let repo_url = match &d.spec.source {
        Source::Git { url, .. } => url.as_str(),
        _ => "",
    };
    let provider = hook_provider(repo_url, login.as_ref().map(|l| l.kind));
    let can_install = login.as_ref().is_some_and(|l| {
        matches!(l.kind, crate::identity::Kind::Gitlab | crate::identity::Kind::Gitea | crate::identity::Kind::GithubToken)
    });
    Ok(json!({ "urls": urls, "provider": provider, "can_install": can_install }))
}

pub async fn webhook_info(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let d = access::deployment(&state, &current, &id, Permission::DepCi).await?;
    let mut info = hook_setup(&state, &d).await?;
    info["enabled"] = json!(deploy::has_webhook(&state.db, &id).await?);
    info["app"] = json!(app_name(&state, &d).await?);
    Ok(Json(info))
}

/// A new webhook secret, saved sealed. Returns it in plain text once.
async fn new_hook_secret(state: &AppState, d: &Deployment) -> ApiResult<String> {
    if !matches!(d.spec.source, Source::Git { .. }) {
        return Err(ApiError::bad_request("push webhooks pull and build, so they need a git deployment"));
    }
    if let Some(app) = app_name(state, d).await? {
        return Err(ApiError::bad_request(format!("pushes already come in through the GitHub App {app}")));
    }
    Ok(deploy::new_webhook_secret(&state.db, &d.id).await?)
}

pub async fn rotate_webhook(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let d = access::deployment(&state, &current, &id, Permission::DepCi).await?;
    let base = public_url(&state.db).await?;
    let secret = new_hook_secret(&state, &d).await?;
    Ok(Json(json!({ "urls": hook_urls(&base, &id), "secret": secret, "content_type": "application/json", "events": ["push"] })))
}

/// `owner/repo` out of a clone URL, for the provider APIs.
pub fn repo_path(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let path = rest.split_once('/')?.1.trim_end_matches('/').trim_end_matches(".git");
    (path.split('/').filter(|p| !p.is_empty()).count() >= 2).then(|| path.to_owned())
}

/// Adds the push webhook at the provider through the deployment's login, with a fresh secret.
pub async fn install_webhook(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    use crate::identity::Kind;
    let d = access::deployment(&state, &current, &id, Permission::DepCi).await?;
    let Source::Git { url, .. } = &d.spec.source else {
        return Err(ApiError::bad_request("push webhooks pull and build, so they need a git deployment"));
    };
    let login = login_of(&state, &d).await?.ok_or_else(|| ApiError::bad_request("pick a saved login for this repository first"))?;
    let repo = repo_path(url).ok_or_else(|| ApiError::bad_request(format!("can't tell owner and repository from {url}")))?;
    let base = public_url(&state.db).await?;
    let secret = new_hook_secret(&state, &d).await?;
    let api = crate::identity::api_base(&login);
    let request = match login.kind {
        Kind::Gitlab => state
            .http
            .post(format!("{api}/api/v4/projects/{}/hooks", crate::runners::providers::encode(&repo)))
            .header("private-token", &login.secret)
            .json(&json!({ "url": format!("{base}{API_BASE}/hooks/gitlab/{id}"), "token": secret, "push_events": true, "enable_ssl_verification": true })),
        Kind::Gitea => state
            .http
            .post(format!("{api}/api/v1/repos/{repo}/hooks"))
            .header("authorization", format!("token {}", login.secret))
            .json(&json!({
                "type": "gitea",
                "active": true,
                "events": ["push"],
                "config": { "url": format!("{base}{API_BASE}/hooks/gitea/{id}"), "content_type": "json", "secret": secret },
            })),
        Kind::GithubToken => state
            .http
            .post(format!("https://api.github.com/repos/{repo}/hooks"))
            .bearer_auth(&login.secret)
            .header("accept", "application/vnd.github+json")
            .header("user-agent", "kuroe")
            .json(&json!({
                "name": "web",
                "active": true,
                "events": ["push"],
                "config": { "url": format!("{base}{API_BASE}/hooks/github/{id}"), "content_type": "json", "secret": secret },
            })),
        _ => return Err(ApiError::bad_request("this login can't add webhooks, set it up by hand")),
    };
    let response = request.send().await.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("{e}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("the provider refused the webhook ({status}): {}", body.chars().take(300).collect::<String>())));
    }
    Ok(Json(json!({ "installed": true })))
}

pub async fn disable_webhook(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    access::deployment(&state, &current, &id, Permission::DepCi).await?;
    sqlx::query("UPDATE deployments SET webhook_secret = NULL WHERE id = ?").bind(&id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn signature_valid(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex) = header.strip_prefix("sha256=") else { return false };
    let Some(expected) = (0..hex.len())
        .step_by(2)
        .map(|i| hex.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect::<Option<Vec<u8>>>()
    else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else { return false };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

pub fn wants_branch(reference: Option<&str>, payload: &Value) -> Option<String> {
    let branch = reference
        .map(str::to_owned)
        .or_else(|| payload["repository"]["default_branch"].as_str().map(str::to_owned))
        .or_else(|| payload["project"]["default_branch"].as_str().map(str::to_owned))?;
    let pushed = payload["ref"].as_str()?;
    (pushed == format!("refs/heads/{branch}")).then_some(branch)
}

pub(super) async fn creator_host_mounts(db: &SqlitePool, d: &Deployment) -> sqlx::Result<bool> {
    let Some(creator) = &d.created_by else { return Ok(false) };
    let row: Option<(String, bool)> =
        sqlx::query_as("SELECT role, host_mounts FROM users WHERE id = ?").bind(creator).fetch_optional(db).await?;
    Ok(row.is_some_and(|(role, allowed)| role == Role::owner().as_str() || allowed))
}

async fn hook_secret(state: &AppState, id: &str) -> ApiResult<String> {
    let secret: Option<Option<String>> =
        sqlx::query_scalar("SELECT webhook_secret FROM deployments WHERE id = ?").bind(id).fetch_optional(&state.db).await?;
    let Some(Some(secret)) = secret else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no webhook here"));
    };
    Ok(crate::secrets::sealer().open(&crate::secrets::webhook_aad(id), &secret)?)
}

fn header<'a>(headers: &'a HeaderMap, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|n| headers.get(*n).and_then(|v| v.to_str().ok()))
}

/// GitLab sends the secret itself in `X-Gitlab-Token`.
pub fn gitlab_token_valid(secret: &str, header: Option<&str>) -> bool {
    header.is_some_and(|given| {
        given.len() == secret.len() && given.bytes().zip(secret.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
    })
}

/// Gitea and Forgejo send the HMAC as plain hex, without GitHub's `sha256=`.
pub fn gitea_signature_valid(secret: &str, body: &[u8], header: Option<&str>) -> bool {
    header.is_some_and(|hex| signature_valid(secret, body, &format!("sha256={hex}")))
}

pub async fn github_hook(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let secret = hook_secret(&state, &id).await?;
    let signature = headers.get("x-hub-signature-256").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !signature_valid(&secret, &body, signature) {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "bad signature"));
    }
    match headers.get("x-github-event").and_then(|v| v.to_str().ok()) {
        Some("ping") => return Ok((StatusCode::OK, Json(json!({ "ok": true })))),
        Some("push") => {}
        other => return Ok((StatusCode::ACCEPTED, Json(json!({ "ignored": other })))),
    }
    pushed(&state, &id, &body).await
}

pub async fn gitlab_hook(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let secret = hook_secret(&state, &id).await?;
    if !gitlab_token_valid(&secret, header(&headers, &["x-gitlab-token"])) {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "bad token"));
    }
    match header(&headers, &["x-gitlab-event"]) {
        Some("Push Hook") => pushed(&state, &id, &body).await,
        other => Ok((StatusCode::ACCEPTED, Json(json!({ "ignored": other })))),
    }
}

pub async fn gitea_hook(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let secret = hook_secret(&state, &id).await?;
    if !gitea_signature_valid(&secret, &body, header(&headers, &["x-gitea-signature", "x-forgejo-signature"])) {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "bad signature"));
    }
    match header(&headers, &["x-gitea-event", "x-forgejo-event"]) {
        Some("push") => pushed(&state, &id, &body).await,
        other => Ok((StatusCode::ACCEPTED, Json(json!({ "ignored": other })))),
    }
}

/// A push arrived with a valid signature: deploy when it's to the branch the deployment follows.
async fn pushed(state: &AppState, id: &str, body: &[u8]) -> ApiResult<(StatusCode, Json<Value>)> {
    let payload: Value = serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("payload is not JSON: {e}")))?;
    let d = deploy::get(&state.db, id).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such deployment"))?;
    let Source::Git { reference, .. } = &d.spec.source else {
        return Err(ApiError::bad_request("this deployment does not build from git"));
    };
    let Some(branch) = wants_branch(reference.as_deref(), &payload) else {
        return Ok((StatusCode::ACCEPTED, Json(json!({ "ignored": payload["ref"] }))));
    };
    let host_mounts = creator_host_mounts(&state.db, &d).await?;
    start(state, d, host_mounts).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "deploying": branch }))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    #[test]
    fn webhook_signature() {
        let body = br#"{"ref":"refs/heads/main"}"#;
        let good = sign("s3cret", body);
        assert!(signature_valid("s3cret", body, &good));
        assert!(!signature_valid("other", body, &good));
        assert!(!signature_valid("s3cret", b"tampered", &good));
        assert!(!signature_valid("s3cret", body, "sha1=abc"));
        assert!(!signature_valid("s3cret", body, "sha256=zz"));
    }

    #[test]
    fn push_must_match_the_configured_branch() {
        let payload = json!({ "ref": "refs/heads/main", "repository": { "default_branch": "main" } });
        assert_eq!(wants_branch(None, &payload).as_deref(), Some("main"));
        assert_eq!(wants_branch(Some("main"), &payload).as_deref(), Some("main"));
        assert_eq!(wants_branch(Some("release"), &payload), None);
        let tag = json!({ "ref": "refs/tags/v1", "repository": { "default_branch": "main" } });
        assert_eq!(wants_branch(None, &tag), None);
    }

    #[test]
    fn gitlab_push_payloads_carry_the_default_branch_on_the_project() {
        let payload = json!({ "ref": "refs/heads/trunk", "project": { "default_branch": "trunk" } });
        assert_eq!(wants_branch(None, &payload).as_deref(), Some("trunk"));
    }

    #[test]
    fn gitlab_token_check() {
        assert!(gitlab_token_valid("s3cret", Some("s3cret")));
        assert!(!gitlab_token_valid("s3cret", Some("wrong")));
        assert!(!gitlab_token_valid("s3cret", Some("s3cre")));
        assert!(!gitlab_token_valid("s3cret", None));
    }

    #[test]
    fn gitea_signature_check() {
        let body = br#"{"ref":"refs/heads/main"}"#;
        let good = sign("s3cret", body);
        let hex = good.strip_prefix("sha256=").unwrap();
        assert!(gitea_signature_valid("s3cret", body, Some(hex)));
        assert!(!gitea_signature_valid("s3cret", b"tampered", Some(hex)));
        assert!(!gitea_signature_valid("other", body, Some(hex)));
        assert!(!gitea_signature_valid("s3cret", body, None));
    }

    #[test]
    fn hooks_follow_the_login_then_the_host() {
        use crate::identity::Kind;
        assert_eq!(hook_provider("https://github.com/acme/api.git", None), "github");
        assert_eq!(hook_provider("https://gitlab.example.com/acme/api.git", None), "gitlab");
        assert_eq!(hook_provider("https://git.example.com/acme/api.git", None), "gitea");
        assert_eq!(hook_provider("https://git.example.com/acme/api.git", Some(Kind::Gitlab)), "gitlab");
        assert_eq!(repo_path("https://gitlab.com/group/sub/api.git").as_deref(), Some("group/sub/api"));
        assert_eq!(repo_path("https://github.com/acme/api").as_deref(), Some("acme/api"));
        assert_eq!(repo_path("https://github.com/acme"), None);
    }
}
