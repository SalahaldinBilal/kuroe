use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::AppState;
use super::access;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::Permission;
use crate::deploy::{self, Deployment};
use crate::proxy::certs::{self, CertMode, PIN_ACME, RouteCert};
use crate::proxy::webserver::{self, PROBE_HEADER};
use crate::proxy::{self as store, Domain, Mode, ProxySettings, RouteOptions, ui, valid_hostname};

fn proxy_error(err: anyhow::Error) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, format!("{err:#}"))
}

pub async fn settings(State(state): State<AppState>, _current: CurrentUser) -> ApiResult<Json<Value>> {
    let settings = store::load_settings(&state.db).await?;
    let running = state
        .docker
        .inspect_container(crate::proxy::traefik::CONTAINER, None)
        .await
        .ok()
        .and_then(|c| c.state)
        .and_then(|s| s.running)
        .unwrap_or(false);
    let web_server_error: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'web_server_error'").fetch_optional(&state.db).await?;
    Ok(Json(json!({
        "settings": settings,
        "traefik_running": running,
        "web_server_error": web_server_error.filter(|e| !e.is_empty()),
        "ui": ui::load(&state.db).await?,
    })))
}

#[derive(Deserialize)]
pub struct UiBody {
    /// Empty removes kuroe's own domain.
    hostname: String,
    #[serde(default = "yes")]
    tls: bool,
}

async fn setting(state: &AppState, key: &str) -> ApiResult<String> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(&state.db).await?;
    Ok(value.unwrap_or_default())
}

/// Sets or removes the domain kuroe's own UI answers on. The public URL follows it unless someone set another one.
pub async fn save_ui(State(state): State<AppState>, current: CurrentUser, Json(body): Json<UiBody>) -> ApiResult<Json<Value>> {
    current.require(Permission::Proxy)?;
    let hostname = body.hostname.trim().trim_end_matches('.').to_ascii_lowercase();
    let next = (!hostname.is_empty()).then(|| ui::UiDomain { hostname: hostname.clone(), tls: body.tls });
    if let Some(next) = &next {
        if !valid_hostname(&next.hostname) {
            return Err(ApiError::bad_request(format!("{hostname} is not a valid hostname")));
        }
        if let Some(taken) = store::all_domains(&state.db).await?.into_iter().find(|d| d.hostname == next.hostname) {
            let owner = deploy::get(&state.db, &taken.deployment_id).await?.map(|d| d.name).unwrap_or_default();
            return Err(ApiError::new(StatusCode::CONFLICT, format!("{hostname} is already a domain of {owner}")));
        }
    }
    let before = ui::load(&state.db).await?;
    ui::save(&state.db, next.as_ref()).await?;
    let public_url = setting(&state, "public_url").await?;
    let followed = public_url.is_empty() || before.as_ref().is_some_and(|b| b.url() == public_url.trim_end_matches('/'));
    let public_url = if followed { next.as_ref().map(ui::UiDomain::url).unwrap_or_default() } else { public_url };
    if followed {
        sqlx::query("INSERT INTO settings (key, value) VALUES ('public_url', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
            .bind(&public_url)
            .execute(&state.db)
            .await?;
    }
    state.runner.publish_ui_route(state.listen).await.map_err(proxy_error)?;
    state.runner.sync_certificates().await.map_err(proxy_error)?;
    super::webserver::sync_if_auto(&state).await?;
    Ok(Json(json!({ "ui": next, "public_url": public_url })))
}

/// Asks kuroe's own domain for kuroe's setup status, from this server, the way a browser would.
pub async fn check_ui(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Verified>> {
    current.require(Permission::Proxy)?;
    let ui = ui::load(&state.db).await?.ok_or_else(|| ApiError::bad_request("kuroe has no domain of its own yet"))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(3))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(anyhow::Error::from)?;
    let url = format!("{}{}/setup", ui.url(), super::API_BASE);
    let result = match client.get(&url).send().await {
        Err(err) => Verified {
            ok: false,
            message: format!(
                "{} didn't answer: {}. The DNS record has to point at this server, and a new certificate can take a minute.",
                ui.url(),
                root_cause(&err)
            ),
        },
        Ok(res) => match res.json::<Value>().await {
            Ok(body) if body.get("needed").is_some() => Verified { ok: true, message: format!("kuroe answers at {}", ui.url()) },
            _ => Verified { ok: false, message: format!("{} answered, but not with kuroe. Check where its DNS record points.", ui.url()) },
        },
    };
    Ok(Json(result))
}

/// The innermost cause, which says what went wrong without reqwest's wrapping.
fn root_cause(err: &dyn std::error::Error) -> String {
    let mut current = err;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
}

pub async fn save(State(state): State<AppState>, current: CurrentUser, Json(body): Json<ProxySettings>) -> ApiResult<StatusCode> {
    current.require(Permission::Proxy)?;
    if body.mode == Mode::Managed && body.http_port == body.https_port {
        return Err(ApiError::bad_request("HTTP and HTTPS ports must differ"));
    }
    if body.acme_email.as_deref().is_some_and(|e| !e.contains('@')) {
        return Err(ApiError::bad_request("the ACME email doesn't look like an email address"));
    }
    if body.mode == Mode::Managed && !body.tls_in_front() && body.cert_mode == CertMode::Dns {
        let id = body.dns_identity.as_deref().ok_or_else(|| ApiError::bad_request("pick the Cloudflare identity for DNS"))?;
        let saved = crate::identity::get(&state.db, id).await?;
        if saved.is_none_or(|i| i.kind != crate::identity::Kind::Cloudflare) {
            return Err(ApiError::bad_request("the DNS identity has to be a Cloudflare token"));
        }
    }
    if body.bind == store::Bind::Behind {
        store::parse_address(&body.behind_http).map_err(|e| ApiError::bad_request(e.to_string()))?;
        store::parse_address(&body.behind_https).map_err(|e| ApiError::bad_request(e.to_string()))?;
    }
    store::save_settings(&state.db, &body).await?;
    if let Err(err) = state.runner.apply_proxy(&body).await {
        let text = format!("{err:#}");
        if crate::proxy::ports::port_error(&text)
            && let Some(message) = super::webserver::blocked_message(&state, true).await
        {
            return Err(ApiError::new(StatusCode::CONFLICT, format!("{message}. Run Traefik behind it, or let kuroe write its config.")));
        }
        return Err(proxy_error(err));
    }
    republish_running(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Rewrites the routes of every running deployment, after something all of them depend on changed.
pub(super) async fn republish_running(state: &AppState) -> ApiResult<()> {
    if store::load_settings(&state.db).await?.mode != Mode::Managed {
        return Ok(());
    }
    for d in deploy::list(&state.db).await? {
        if let Some(project) = d.active_project.as_deref().filter(|_| d.status == "running") {
            state.runner.publish_routes(&d, Some(project)).await.map_err(proxy_error)?;
        }
    }
    if let Err(err) = state.runner.publish_ui_route(state.listen).await {
        tracing::warn!("route to kuroe's own domain: {err:#}");
    }
    state.runner.sync_certificates().await.map_err(proxy_error)?;
    super::webserver::sync_if_auto(state).await
}

#[derive(Deserialize)]
pub struct NewDomain {
    hostname: String,
    #[serde(default)]
    service: String,
    #[serde(default)]
    port: u16,
    #[serde(default = "yes")]
    tls: bool,
    #[serde(default)]
    certificate: Option<String>,
    #[serde(default)]
    options: RouteOptions,
}

#[derive(Deserialize)]
pub struct DomainTarget {
    #[serde(default)]
    service: String,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    options: RouteOptions,
}

/// A redirect sends traffic to no service, so it's stored with an empty one and port 0.
fn target_of<'a>(service: &'a str, port: u16, options: &RouteOptions) -> (&'a str, u16) {
    if options.redirect.is_some() { ("", 0) } else { (service.trim(), port) }
}

async fn check_routing(state: &AppState, d: &Deployment, service: &str, port: u16, options: &RouteOptions) -> ApiResult<()> {
    options.check().map_err(ApiError::bad_request)?;
    if options.redirect.is_none() && (service.is_empty() || port == 0) {
        return Err(ApiError::bad_request("pick a service and a port"));
    }
    if options.is_empty() {
        return Ok(());
    }
    if store::load_settings(&state.db).await?.mode != Mode::Managed {
        return Err(ApiError::bad_request("paths, redirects, compression and error pages need managed Traefik"));
    }
    if d.spec.is_static() && (!options.paths.is_empty() || options.compress || options.error_page.is_some()) {
        return Err(ApiError::bad_request("a static site's domain can only redirect, the file server already compresses"));
    }
    Ok(())
}

fn yes() -> bool {
    true
}

#[derive(Serialize)]
pub struct ServedBy {
    kind: &'static str,
    name: Option<String>,
    pinned: bool,
}

#[derive(Serialize)]
pub struct DomainView {
    #[serde(flatten)]
    domain: Domain,
    served_by: ServedBy,
}

fn served(cert: RouteCert, pinned: bool) -> ServedBy {
    match cert {
        RouteCert::Acme { .. } => ServedBy { kind: "acme", name: None, pinned },
        RouteCert::Saved { name, .. } => ServedBy { kind: "saved", name: Some(name), pinned },
        RouteCert::Default => ServedBy { kind: "default", name: None, pinned },
    }
}

pub async fn list_domains(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Vec<DomainView>>> {
    access::deployment(&state, &current, &id, Permission::DepView).await?;
    let domains = store::domains_for(&state.db, &id).await?;
    let certs = state.runner.served_by(&domains).await?;
    let in_front = store::load_settings(&state.db).await?.tls_in_front();
    Ok(Json(
        domains
            .into_iter()
            .zip(certs)
            .map(|(domain, cert)| {
                let pinned = domain.certificate.is_some();
                let served_by =
                    if in_front && domain.tls { ServedBy { kind: "server", name: None, pinned } } else { served(cert, pinned) };
                DomainView { served_by, domain }
            })
            .collect(),
    ))
}

/// A pin is `acme`, or a saved certificate that covers the hostname.
async fn check_pin(state: &AppState, hostname: &str, pin: Option<&str>) -> ApiResult<()> {
    let Some(id) = pin.filter(|p| *p != PIN_ACME) else { return Ok(()) };
    let saved = crate::identity::get(&state.db, id)
        .await?
        .as_ref()
        .and_then(certs::saved)
        .ok_or_else(|| ApiError::bad_request("that certificate no longer exists"))?;
    if !saved.sans.iter().any(|s| certs::covers(s, hostname)) {
        return Err(ApiError::bad_request(format!("{} doesn't cover {hostname}, it's for {}", saved.name, saved.sans.join(", "))));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct Pin {
    certificate: Option<String>,
}

pub async fn pin_certificate(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<Pin>,
) -> ApiResult<StatusCode> {
    let (domain, d) = domain_access(&state, &current, &id).await?;
    check_pin(&state, &domain.hostname, body.certificate.as_deref()).await?;
    store::set_domain_certificate(&state.db, &id, body.certificate.as_deref()).await?;
    republish(&state, &d).await?;
    Ok(StatusCode::NO_CONTENT)
}

const EXPIRY_WARNING: i64 = 14 * 24 * 60 * 60;

#[derive(Serialize)]
pub struct Attention {
    kind: &'static str,
    message: String,
    href: String,
}

/// Things about the proxy worth a look on the dashboard.
pub async fn attention(State(state): State<AppState>, _current: CurrentUser) -> ApiResult<Json<Vec<Attention>>> {
    let mut items = Vec::new();
    if let Some(message) = super::webserver::blocked_message(&state, false).await {
        items.push(Attention { kind: "ports", message: capitalize(&message), href: "/proxy".into() });
    }
    let domains = store::tls_domains(&state.db).await?;
    let served = state.runner.served_by(&domains).await?;
    let used: std::collections::BTreeSet<String> =
        served.into_iter().filter_map(|c| if let RouteCert::Saved { id, .. } = c { Some(id) } else { None }).collect();
    let now = crate::db::now();
    for identity in crate::identity::list(&state.db).await? {
        let Some(expires) = identity.expires_at.filter(|_| used.contains(&identity.id)) else { continue };
        if expires - now < EXPIRY_WARNING {
            let days = (expires - now).div_euclid(24 * 60 * 60);
            let message = if days < 0 {
                format!("The certificate {} has expired", identity.name)
            } else {
                format!("The certificate {} expires in {days} days", identity.name)
            };
            items.push(Attention { kind: "certificate", message, href: "/identities".into() });
        }
    }
    Ok(Json(items))
}

async fn republish(state: &AppState, d: &Deployment) -> ApiResult<()> {
    let project = d.active_project.as_deref().filter(|_| d.status == "running");
    state.runner.publish_routes(d, project).await.map_err(proxy_error)?;
    super::webserver::sync_if_auto(state).await
}

pub async fn add_domain(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<NewDomain>,
) -> ApiResult<(StatusCode, Json<Domain>)> {
    let d = access::deployment(&state, &current, &id, Permission::DepDomains).await?;
    let hostname = body.hostname.trim().to_ascii_lowercase();
    if !valid_hostname(&hostname) {
        return Err(ApiError::bad_request(format!("{hostname} is not a valid hostname")));
    }
    let options = body.options.normalized();
    let (service, port) = target_of(&body.service, body.port, &options);
    check_routing(&state, &d, service, port, &options).await?;
    if ui::load(&state.db).await?.is_some_and(|u| u.hostname == hostname) {
        return Err(ApiError::new(StatusCode::CONFLICT, format!("{hostname} is kuroe's own domain")));
    }
    let pin = body.certificate.as_deref().filter(|p| !p.is_empty());
    check_pin(&state, &hostname, pin).await?;
    let domain = match store::create_domain(&state.db, &id, &hostname, service, port, body.tls).await {
        Ok(mut domain) => {
            if pin.is_some() {
                store::set_domain_certificate(&state.db, &domain.id, pin).await?;
                domain.certificate = pin.map(str::to_owned);
            }
            if !options.is_empty() {
                store::set_domain_target(&state.db, &domain.id, service, port, &options).await?;
                domain.options = options;
            }
            domain
        }
        Err(err) if err.as_database_error().is_some_and(|e| e.is_unique_violation()) => {
            return Err(ApiError::new(StatusCode::CONFLICT, format!("{hostname} is already used")));
        }
        Err(err) => return Err(err.into()),
    };
    republish(&state, &d).await?;
    Ok((StatusCode::CREATED, Json(domain)))
}

async fn domain_access(state: &AppState, current: &CurrentUser, id: &str) -> ApiResult<(Domain, Deployment)> {
    let domain = store::domain(&state.db, id).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such domain"))?;
    let d = access::deployment(state, current, &domain.deployment_id, Permission::DepDomains).await?;
    Ok((domain, d))
}

/// Changes where a domain sends traffic: its service and port, paths, redirect, compression and error pages.
pub async fn update_domain(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<DomainTarget>,
) -> ApiResult<Json<Domain>> {
    let (_, d) = domain_access(&state, &current, &id).await?;
    let options = body.options.normalized();
    let (service, port) = target_of(&body.service, body.port, &options);
    check_routing(&state, &d, service, port, &options).await?;
    store::set_domain_target(&state.db, &id, service, port, &options).await?;
    republish(&state, &d).await?;
    let domain = store::domain(&state.db, &id).await?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such domain"))?;
    Ok(Json(domain))
}

pub async fn remove_domain(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let (_, d) = domain_access(&state, &current, &id).await?;
    store::delete_domain(&state.db, &id).await?;
    republish(&state, &d).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct Snippet {
    kind: webserver::Kind,
    label: &'static str,
    files: Vec<webserver::File>,
    steps: Vec<String>,
}

/// The config for this one domain, for each web server kuroe knows.
pub async fn snippets(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Vec<Snippet>>> {
    let (domain, _) = domain_access(&state, &current, &id).await?;
    if domain.options.redirect.is_some() {
        return Err(ApiError::bad_request("this domain redirects, which only managed Traefik does"));
    }
    let upstream = super::webserver::host_upstream(&state, &domain).await?.ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "port {} of {} is not published on the host. Publish it in the deployment, deploy, then come back.",
                domain.port, domain.service
            ),
        )
    })?;
    let entries =
        [webserver::Entry { hostname: domain.hostname.clone(), upstream, tls: domain.tls, probe: Some(domain.probe_token.clone()) }];
    let cert_paths = webserver::CertPaths::default();
    let mut out = Vec::new();
    for kind in webserver::Kind::ALL {
        let files = webserver::render(&webserver::Render {
            kind,
            mode: webserver::Mode::Front,
            entries: &entries,
            cert_paths: &cert_paths,
            traefik_tls: "",
            rest_tls: "",
            path: kind.default_path().into(),
            extra_path: None,
        })
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
        let mut steps = webserver::certificate_steps(kind, &entries, &cert_paths);
        steps.extend(webserver::one_time_steps(kind, webserver::Mode::Front, kind.default_path(), None, ""));
        out.push(Snippet { kind, label: kind.label(), files, steps });
    }
    Ok(Json(out))
}

#[derive(Serialize)]
pub struct Verified {
    ok: bool,
    message: String,
}

pub async fn verify(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Verified>> {
    let (domain, _) = domain_access(&state, &current, &id).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(anyhow::Error::from)?;
    let result = match client.get(format!("http://{}/", domain.hostname)).send().await {
        Err(err) => Verified { ok: false, message: format!("could not reach {}: {err}", domain.hostname) },
        Ok(res) => match res.headers().get(PROBE_HEADER).and_then(|v| v.to_str().ok()) {
            Some(token) if token == domain.probe_token => {
                store::mark_verified(&state.db, &domain.id).await?;
                Verified { ok: true, message: format!("{} answered through your proxy with status {}", domain.hostname, res.status()) }
            }
            Some(_) => Verified { ok: false, message: format!("{PROBE_HEADER} is present but has a different value. Paste the snippet again.") },
            None => Verified {
                ok: false,
                message: format!("{} answered with status {} but without the {PROBE_HEADER} header", domain.hostname, res.status()),
            },
        },
    };
    Ok(Json(result))
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}
