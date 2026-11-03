use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Redirect;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::AppState;
use super::ci::{creator_host_mounts, signature_valid, start, wants_branch};
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::deploy::{self, Source, Spec};
use crate::identity::{self, Identity, Kind, NewIdentity, github};
use crate::proxy::certs::{self, CertMode};

const PENDING_TTL: Duration = Duration::from_secs(30 * 60);

/// A GitHub App that was sent off to GitHub and hasn't come back yet. Keyed by the manifest `state`.
pub struct PendingApp {
    identity_id: String,
    name: String,
    user_id: String,
    started: Instant,
}

pub type PendingApps = std::sync::Mutex<HashMap<String, PendingApp>>;

fn bad_gateway(err: anyhow::Error) -> ApiError {
    ApiError::bad_request(format!("{err:#}"))
}

fn require_use(current: &CurrentUser) -> ApiResult<()> {
    if current.access.has(Permission::IdentUse) || current.access.has(Permission::IdentManage) {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

async fn found(state: &AppState, id: &str) -> ApiResult<Identity> {
    identity::get(&state.db, id).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such identity"))
}

#[derive(Serialize)]
pub struct UsedBy {
    id: String,
    name: String,
}

#[derive(Serialize)]
pub struct Row {
    #[serde(flatten)]
    identity: Identity,
    used_by: Vec<UsedBy>,
}

async fn users_of(state: &AppState) -> ApiResult<HashMap<String, Vec<UsedBy>>> {
    let mut map: HashMap<String, Vec<UsedBy>> = HashMap::new();
    for d in deploy::list(&state.db).await? {
        for id in identity::uses(&d.spec) {
            map.entry(id.to_owned()).or_default().push(UsedBy { id: d.id.clone(), name: d.name.clone() });
        }
    }
    let domains = crate::proxy::tls_domains(&state.db).await?;
    let served = state.runner.served_by(&domains).await?;
    for (domain, cert) in domains.iter().zip(served) {
        if let certs::RouteCert::Saved { id, .. } = cert {
            map.entry(id).or_default().push(UsedBy { id: domain.deployment_id.clone(), name: domain.hostname.clone() });
        }
    }
    let settings = crate::proxy::load_settings(&state.db).await?;
    if let (CertMode::Dns, Some(id)) = (settings.cert_mode, settings.dns_identity) {
        map.entry(id).or_default().push(UsedBy { id: String::new(), name: "Proxy certificates".into() });
    }
    Ok(map)
}

pub async fn list(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<Row>>> {
    require_use(&current)?;
    let mut used = users_of(&state).await?;
    let rows = identity::list(&state.db)
        .await?
        .into_iter()
        .map(|identity| Row { used_by: used.remove(&identity.id).unwrap_or_default(), identity })
        .collect();
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct NewBody {
    kind: Kind,
    name: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    username: String,
    secret: String,
}

fn check_fields(kind: Kind, name: &str, host: &str) -> ApiResult<()> {
    if !identity::valid_name(name) {
        return Err(ApiError::bad_request("give it a name of up to 64 characters"));
    }
    let needs_host = matches!(kind, Kind::Gitea | Kind::Git | Kind::Registry);
    if needs_host && identity::normalize_host(host).is_empty() {
        return Err(ApiError::bad_request("add the host, like git.example.com"));
    }
    Ok(())
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error().is_some_and(|e| e.is_unique_violation())
}

pub async fn create(State(state): State<AppState>, current: CurrentUser, Json(body): Json<NewBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    current.require(Permission::IdentManage)?;
    if body.kind == Kind::GithubApp {
        return Err(ApiError::bad_request("GitHub Apps are added through GitHub, use Continue on GitHub"));
    }
    if body.kind == Kind::Certificate {
        return Err(ApiError::bad_request("add certificates through Paste a certificate"));
    }
    let name = body.name.trim();
    check_fields(body.kind, name, &body.host)?;
    let secret = body.secret.trim();
    if secret.is_empty() {
        return Err(ApiError::bad_request("the token or password is empty"));
    }
    let host = identity::normalize_host(&body.host);
    let draft = Identity {
        id: String::new(),
        kind: body.kind,
        name: name.to_owned(),
        host: host.clone(),
        username: body.username.trim().to_owned(),
        secret: secret.to_owned(),
        meta: json!({}),
        created_at: 0,
        last_used_at: None,
        expires_at: None,
    };
    let (meta, expires_at) = if body.kind == Kind::Cloudflare {
        let checked = identity::check(&state.http, &draft).await.map_err(bad_gateway)?;
        (checked.meta, checked.expires_at)
    } else {
        (json!({}), None)
    };
    let new = NewIdentity {
        id: None,
        kind: body.kind,
        name,
        host: &host,
        username: &draft.username,
        secret,
        meta: &meta,
        expires_at,
    };
    match identity::insert(&state.db, &new).await {
        Ok(id) => Ok((StatusCode::CREATED, Json(json!({ "id": id })))),
        Err(err) if is_unique_violation(&err) => Err(ApiError::new(StatusCode::CONFLICT, format!("{name} already exists"))),
        Err(err) => Err(err.into()),
    }
}

#[derive(Deserialize)]
pub struct EditBody {
    name: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    username: String,
    /// Empty keeps the saved secret.
    #[serde(default)]
    secret: String,
}

pub async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<EditBody>,
) -> ApiResult<StatusCode> {
    current.require(Permission::IdentManage)?;
    let existing = found(&state, &id).await?;
    let name = body.name.trim();
    check_fields(existing.kind, name, &body.host)?;
    if existing.kind == Kind::GithubApp && !body.secret.trim().is_empty() {
        return Err(ApiError::bad_request("a GitHub App's key comes from GitHub, connect it again to replace it"));
    }
    let secret = if body.secret.trim().is_empty() { existing.secret.clone() } else { body.secret.trim().to_owned() };
    let host = if existing.kind == Kind::GithubApp { existing.host.clone() } else { identity::normalize_host(&body.host) };
    let updated = sqlx::query("UPDATE identities SET name = ?, host = ?, username = ?, secret = ? WHERE id = ?")
        .bind(name)
        .bind(host)
        .bind(body.username.trim())
        .bind(crate::secrets::sealer().seal(&crate::secrets::identity_aad(&id), &secret))
        .bind(&id)
        .execute(&state.db)
        .await;
    match updated {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(err) if is_unique_violation(&err) => Err(ApiError::new(StatusCode::CONFLICT, format!("{name} already exists"))),
        Err(err) => Err(err.into()),
    }
}

#[derive(Deserialize)]
pub struct RemoveQuery {
    #[serde(default)]
    revoke: bool,
}

pub async fn remove(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<StatusCode> {
    current.require(Permission::IdentManage)?;
    let existing = found(&state, &id).await?;
    if existing.kind == Kind::Certificate {
        let pinned: Vec<String> = sqlx::query_scalar("SELECT hostname FROM domains WHERE certificate = ? ORDER BY hostname")
            .bind(&id)
            .fetch_all(&state.db)
            .await?;
        if !pinned.is_empty() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!("{} is pinned on {}, switch those domains to another certificate first", existing.name, pinned.join(", ")),
            ));
        }
        if q.revoke {
            revoke_at_cloudflare(&state, &existing).await?;
        }
        sqlx::query("DELETE FROM identities WHERE id = ?").bind(&id).execute(&state.db).await?;
        super::proxy::republish_running(&state).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    if existing.kind == Kind::Cloudflare {
        let settings = crate::proxy::load_settings(&state.db).await?;
        if settings.cert_mode == CertMode::Dns && settings.dns_identity.as_deref() == Some(id.as_str()) {
            return Err(ApiError::new(StatusCode::CONFLICT, format!("{} gets certificates over DNS, pick another one on the Proxy page first", existing.name)));
        }
    }
    if let Some(users) = users_of(&state).await?.get(&id) {
        let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("{} is used by {}, pick another login on those first", existing.name, names.join(", ")),
        ));
    }
    let pools = crate::runners::uses_identity(&state.db, &id).await?;
    if !pools.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("{} registers the runner pools {}, remove those first", existing.name, pools.join(", ")),
        ));
    }
    sqlx::query("DELETE FROM identities WHERE id = ?").bind(&id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_at_cloudflare(state: &AppState, cert: &Identity) -> ApiResult<()> {
    let (Some(cf_id), Some(login)) = (cert.meta["cloudflare_id"].as_str(), cert.meta["cloudflare_identity"].as_str()) else {
        return Err(ApiError::bad_request("this certificate wasn't made through a Cloudflare identity here, revoke it in Cloudflare"));
    };
    let token = found(state, login).await?.secret;
    identity::cloudflare::revoke_origin(&state.http, &token, cf_id).await.map_err(bad_gateway)
}

fn certificate_meta(chain: &str, parsed: &certs::Parsed, source: &str) -> Value {
    json!({
        "chain": chain,
        "sans": parsed.sans,
        "issuer": parsed.issuer,
        "not_after": parsed.not_after,
        "cloudflare_origin": parsed.cloudflare_origin,
        "source": source,
        "aop": false,
    })
}

async fn save_certificate(state: &AppState, name: &str, key: &str, meta: &Value, not_after: i64) -> ApiResult<String> {
    let new = NewIdentity { id: None, kind: Kind::Certificate, name, host: "", username: "", secret: key, meta, expires_at: Some(not_after) };
    let id = match identity::insert(&state.db, &new).await {
        Ok(id) => id,
        Err(err) if is_unique_violation(&err) => return Err(ApiError::new(StatusCode::CONFLICT, format!("{name} already exists"))),
        Err(err) => return Err(err.into()),
    };
    super::proxy::republish_running(state).await?;
    Ok(id)
}

#[derive(Deserialize)]
pub struct PastedCertificate {
    #[serde(default)]
    name: String,
    certificate: String,
    key: String,
}

pub async fn add_certificate(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<PastedCertificate>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    current.require(Permission::IdentManage)?;
    let chain = body.certificate.trim();
    let key = body.key.trim();
    let parsed = certs::parse_chain(chain).map_err(|e| ApiError::bad_request(e.to_string()))?;
    certs::check_key(chain, key).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let name = if body.name.trim().is_empty() { parsed.sans[0].trim_start_matches("*.").to_owned() } else { body.name.trim().to_owned() };
    if !identity::valid_name(&name) {
        return Err(ApiError::bad_request("give it a name of up to 64 characters"));
    }
    let meta = certificate_meta(chain, &parsed, "pasted");
    let id = save_certificate(&state, &name, key, &meta, parsed.not_after).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "sans": parsed.sans, "not_after": parsed.not_after, "cloudflare_origin": parsed.cloudflare_origin }))))
}

#[derive(Deserialize)]
pub struct OriginRequest {
    #[serde(default)]
    name: String,
    cloudflare: String,
    hostnames: Vec<String>,
}

pub async fn create_origin_certificate(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<OriginRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    current.require(Permission::IdentManage)?;
    let login = found(&state, &body.cloudflare).await?;
    if login.kind != Kind::Cloudflare {
        return Err(ApiError::bad_request("pick a Cloudflare identity"));
    }
    let hostnames: Vec<String> = body.hostnames.iter().map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect();
    let valid = |h: &str| crate::proxy::valid_hostname(h.trim_start_matches("*."));
    if hostnames.is_empty() || !hostnames.iter().all(|h| valid(h)) {
        return Err(ApiError::bad_request("list the hostnames, like example.com and *.example.com"));
    }
    let (key, csr) = certs::key_and_csr(&hostnames).map_err(bad_gateway)?;
    let origin = identity::cloudflare::create_origin(&state.http, &login.secret, &hostnames, &csr).await.map_err(bad_gateway)?;
    let parsed = certs::parse_chain(&origin.certificate).map_err(bad_gateway)?;
    let mut meta = certificate_meta(&origin.certificate, &parsed, "cloudflare");
    meta["cloudflare_id"] = json!(origin.id);
    meta["cloudflare_identity"] = json!(login.id);
    let name = if body.name.trim().is_empty() { format!("{} origin", hostnames[0].trim_start_matches("*.")) } else { body.name.trim().to_owned() };
    let id = save_certificate(&state, &name, &key, &meta, parsed.not_after).await?;
    identity::touch(&state.db, &login.id).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "sans": parsed.sans, "not_after": parsed.not_after }))))
}

#[derive(Deserialize)]
pub struct OriginPulls {
    on: bool,
    /// Also turn the zone setting on in Cloudflare with this identity.
    #[serde(default)]
    cloudflare: Option<String>,
}

pub async fn origin_pulls(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<OriginPulls>,
) -> ApiResult<StatusCode> {
    current.require(Permission::IdentManage)?;
    let mut cert = found(&state, &id).await?;
    if cert.kind != Kind::Certificate {
        return Err(ApiError::bad_request("origin pulls belong to certificates"));
    }
    if body.on
        && let Some(login) = &body.cloudflare
    {
        let login = found(&state, login).await?;
        let zones: Vec<String> = login.meta["zones"].as_array().map(|z| z.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        let sans: Vec<String> = cert.meta["sans"].as_array().map(|s| s.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        let wanted: std::collections::BTreeSet<&String> = sans.iter().filter_map(|s| identity::cloudflare::zone_for(&zones, s)).collect();
        if wanted.is_empty() {
            return Err(ApiError::bad_request(format!("{} reaches none of this certificate's zones, run Test on it to refresh its zones", login.name)));
        }
        for zone in wanted {
            identity::cloudflare::enable_origin_pulls(&state.http, &login.secret, zone).await.map_err(bad_gateway)?;
        }
    }
    cert.meta["aop"] = json!(body.on);
    identity::save_check(&state.db, &id, &cert.meta, cert.expires_at).await?;
    super::proxy::republish_running(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_use(&current)?;
    let existing = found(&state, &id).await?;
    let checked = identity::check(&state.http, &existing).await.map_err(bad_gateway)?;
    identity::save_check(&state.db, &id, &checked.meta, checked.expires_at).await?;
    Ok(Json(json!({ "message": checked.message, "expires_at": checked.expires_at })))
}

pub async fn repos(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Vec<identity::repos::Repo>>> {
    require_use(&current)?;
    let existing = found(&state, &id).await?;
    if !identity::repos::lists(existing.kind) {
        return Err(ApiError::bad_request(format!("{} can't list repositories, type the URL instead", existing.name)));
    }
    let repos = identity::repos::list(&state.http, &existing).await.map_err(bad_gateway)?;
    identity::touch(&state.db, &id).await?;
    Ok(Json(repos))
}

async fn public_url(state: &AppState) -> ApiResult<String> {
    let url: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'public_url'").fetch_optional(&state.db).await?;
    url.filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .ok_or_else(|| ApiError::bad_request("set the public URL in Settings first, GitHub needs it to send you back"))
}

#[derive(Deserialize)]
pub struct StartApp {
    name: String,
    #[serde(default)]
    owner: String,
}

pub async fn github_start(State(state): State<AppState>, current: CurrentUser, Json(body): Json<StartApp>) -> ApiResult<Json<Value>> {
    current.require(Permission::IdentManage)?;
    let name = body.name.trim();
    if !identity::valid_name(name) {
        return Err(ApiError::bad_request("give it a name of up to 64 characters"));
    }
    if identity::by_name(&state.db, name).await?.is_some() {
        return Err(ApiError::new(StatusCode::CONFLICT, format!("{name} already exists")));
    }
    let base = public_url(&state).await?;
    let identity_id = uuid::Uuid::new_v4().to_string();
    let state_token = crate::deploy::tokens::random_secret(24);
    let app_name = format!("kuroe-{}", name.to_ascii_lowercase().replace(|c: char| !c.is_ascii_alphanumeric(), "-"));
    let manifest = github::manifest(&base, &app_name, &identity_id);
    {
        let mut pending = state.github_apps.lock().unwrap();
        pending.retain(|_, p| p.started.elapsed() < PENDING_TTL);
        pending.insert(
            state_token.clone(),
            PendingApp { identity_id, name: name.to_owned(), user_id: current.user.id.clone(), started: Instant::now() },
        );
    }
    let owner = Some(body.owner.as_str());
    Ok(Json(json!({ "action": github::new_app_url(owner, &state_token), "manifest": manifest.to_string() })))
}

#[derive(Deserialize)]
pub struct Callback {
    code: String,
    state: String,
}

/// The GitHub round trips are page loads, so failures go back to the Identities page instead of showing JSON.
fn back_with_error(result: ApiResult<Redirect>) -> Redirect {
    result.unwrap_or_else(|err| {
        let message: String = err.message.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
        Redirect::to(&format!("/identities?error={message}"))
    })
}

pub async fn github_callback(state: State<AppState>, current: CurrentUser, q: Query<Callback>) -> Redirect {
    back_with_error(finish_app(state, current, q).await)
}

pub async fn github_installed(state: State<AppState>, current: CurrentUser, id: Path<String>, q: Query<Installed>) -> Redirect {
    back_with_error(save_installation(state, current, id, q).await)
}

async fn finish_app(State(state): State<AppState>, current: CurrentUser, Query(q): Query<Callback>) -> ApiResult<Redirect> {
    current.require(Permission::IdentManage)?;
    let pending = state.github_apps.lock().unwrap().remove(&q.state);
    let Some(pending) = pending.filter(|p| p.user_id == current.user.id && p.started.elapsed() < PENDING_TTL) else {
        return Err(ApiError::bad_request("this GitHub link expired or belongs to someone else, start again from Identities"));
    };
    let app = github::convert_manifest(&state.http, &q.code).await.map_err(bad_gateway)?;
    let meta = json!({ "app_id": app.app_id, "slug": app.slug, "owner": app.owner, "html_url": app.html_url });
    let secret = serde_json::to_string(&app.secret).map_err(anyhow::Error::from)?;
    let new = NewIdentity {
        id: Some(&pending.identity_id),
        kind: Kind::GithubApp,
        name: &pending.name,
        host: "github.com",
        username: &app.owner,
        secret: &secret,
        meta: &meta,
        expires_at: None,
    };
    identity::insert(&state.db, &new).await?;
    Ok(Redirect::to(&format!("https://github.com/apps/{}/installations/new", app.slug)))
}

#[derive(Deserialize)]
pub struct Installed {
    installation_id: u64,
}

async fn save_installation(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<Installed>,
) -> ApiResult<Redirect> {
    current.require(Permission::IdentManage)?;
    let mut existing = found(&state, &id).await?;
    if existing.kind != Kind::GithubApp {
        return Err(ApiError::bad_request("this identity is not a GitHub App"));
    }
    existing.meta["installation_id"] = json!(q.installation_id);
    identity::save_check(&state.db, &id, &existing.meta, existing.expires_at).await?;
    Ok(Redirect::to("/identities"))
}

fn repo_key(url: &str) -> String {
    url.trim().trim_end_matches('/').trim_end_matches(".git").to_ascii_lowercase()
}

/// Deployments that clone this repository through this app.
pub fn matches_repo(spec: &Spec, app_id: &str, payload: &Value) -> bool {
    let Source::Git { url, identity: Some(identity), .. } = &spec.source else { return false };
    let wanted = repo_key(url);
    identity == app_id
        && ["clone_url", "html_url"].iter().any(|k| payload["repository"][k].as_str().is_some_and(|u| repo_key(u) == wanted))
}

/// Push hooks for every repository an app can read arrive here. The app is found by the id GitHub sends along.
pub async fn github_app_hook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult<(StatusCode, Json<Value>)> {
    let app_id = headers
        .get("x-github-hook-installation-target-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| ApiError::bad_request("no app id in the request"))?;
    let app = identity::list(&state.db)
        .await?
        .into_iter()
        .find(|i| i.kind == Kind::GithubApp && i.meta["app_id"].as_u64() == Some(app_id))
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such app"))?;
    let secret = github::app_secret(&app).map_err(bad_gateway)?.webhook_secret;
    let signature = headers.get("x-hub-signature-256").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if secret.is_empty() || !signature_valid(&secret, &body, signature) {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "bad signature"));
    }
    match headers.get("x-github-event").and_then(|v| v.to_str().ok()) {
        Some("push") => {}
        other => return Ok((StatusCode::ACCEPTED, Json(json!({ "ignored": other })))),
    }
    let payload: Value = serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("payload is not JSON: {e}")))?;
    let mut started = Vec::new();
    for d in deploy::list(&state.db).await? {
        if !matches_repo(&d.spec, &app.id, &payload) {
            continue;
        }
        let Source::Git { reference, .. } = &d.spec.source else { continue };
        if wants_branch(reference.as_deref(), &payload).is_none() {
            continue;
        }
        let name = d.name.clone();
        let host_mounts = creator_host_mounts(&state.db, &d).await?;
        match start(&state, d, host_mounts).await {
            Ok(()) => started.push(name),
            Err(err) => tracing::warn!("push for {name} not deployed: {}", err.message),
        }
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "deploying": started }))))
}

/// A saved login on a spec must exist, be the right kind, and point at the right host.
/// Keeping the one that was already there needs no permission, picking a new one needs `ident.use`.
pub async fn check_spec(state: &AppState, current: &CurrentUser, spec: &Spec, before: Option<&Spec>) -> ApiResult<()> {
    let (id, git_url) = match &spec.source {
        Source::Git { identity: Some(id), url, .. } => (id, Some(url.as_str())),
        Source::Image { registry_identity: Some(id), .. } => (id, None),
        _ => return Ok(()),
    };
    let unchanged = before.is_some_and(|b| identity::uses(b).contains(&id.as_str()) && same_target(b, spec));
    if !unchanged {
        require_use(current)?;
    }
    let saved = identity::get(&state.db, id).await?.ok_or_else(|| ApiError::bad_request("the picked login no longer exists"))?;
    match git_url {
        Some(url) => identity::check_git_url(&saved, url).map_err(|e| ApiError::bad_request(e.to_string())),
        None if !saved.kind.pulls_images() => Err(ApiError::bad_request(format!("{} can't log in to a registry", saved.name))),
        None => Ok(()),
    }
}

fn same_target(before: &Spec, after: &Spec) -> bool {
    match (&before.source, &after.source) {
        (Source::Git { url: a, .. }, Source::Git { url: b, .. }) => a == b,
        (Source::Image { .. }, Source::Image { .. }) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_pushes_reach_deployments_of_that_repo_only() {
        let spec = |url: &str, identity: Option<&str>| {
            Spec::new(Source::Git {
                url: url.into(),
                reference: None,
                context: None,
                dockerfile: None,
                token: None,
                identity: identity.map(str::to_owned),
                compose_file: None,
            })
        };
        let payload = json!({ "repository": { "clone_url": "https://github.com/Acme/API.git", "html_url": "https://github.com/Acme/API" } });
        assert!(matches_repo(&spec("https://github.com/acme/api", Some("app1")), "app1", &payload));
        assert!(matches_repo(&spec("https://github.com/acme/api.git/", Some("app1")), "app1", &payload));
        assert!(!matches_repo(&spec("https://github.com/acme/api.git", Some("app2")), "app1", &payload));
        assert!(!matches_repo(&spec("https://github.com/acme/web.git", Some("app1")), "app1", &payload));
        assert!(!matches_repo(&spec("https://github.com/acme/api.git", None), "app1", &payload));
    }
}
