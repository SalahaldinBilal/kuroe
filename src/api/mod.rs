mod access;
mod api_tokens;
mod bundle;
mod ci;
mod containers;
mod deployments;
mod deploys;
mod error;
mod events;
mod extract;
mod files;
mod handlers;
pub mod http_access;
mod identities;
mod mcp;
mod metrics;
mod pipelines;
mod preview;
mod proxy;
mod roles;
mod runners;
mod saved_secrets;
mod update;
mod volumes;
mod webserver;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{any, get, post, put};
use bollard::Docker;
use sqlx::SqlitePool;
use tower_http::trace::TraceLayer;

use crate::auth::LoginLimiter;
use crate::deploy::runner::Runner;
use crate::proxy::traefik::Traefik;

pub use error::{ApiError, ApiResult};
pub use extract::CSRF_HEADER;

const UPLOAD_LIMIT: usize = 1 << 30;
/// A domain's options carry its error pages.
const DOMAIN_LIMIT: usize = 8 << 20;
/// A preview request's body arrives as base64, a third bigger than the bytes.
const PREVIEW_REQUEST_LIMIT: usize = 24 << 20;
pub const API_VERSION: u32 = 1;
pub const API_BASE: &str = "/api/v1";

#[derive(Clone)]
pub struct ExecGrant {
    pub user_id: String,
    pub container: String,
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub docker: Docker,
    pub limiter: Arc<LoginLimiter>,
    pub secure_cookies: bool,
    pub runner: Runner,
    pub execs: Arc<Mutex<HashMap<String, ExecGrant>>>,
    pub http: reqwest::Client,
    pub files: Arc<files::FileState>,
    pub github_apps: Arc<identities::PendingApps>,
    pub ports: Arc<webserver::PortsCache>,
    pub http_access: Arc<http_access::HttpState>,
    pub runners: crate::runners::Runners,
    pub secret_matches: Arc<saved_secrets::MatchLimiter>,
    /// The address `serve` listens on, where Traefik sends kuroe's own domain.
    pub listen: std::net::SocketAddr,
}

impl AppState {
    pub fn new(db: SqlitePool, data_dir: PathBuf, secure_cookies: bool) -> anyhow::Result<Self> {
        crate::secrets::install(&data_dir)?;
        let docker = crate::docker::connect()?;
        let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
        let traefik = Traefik { docker: docker.clone(), http: http.clone() };
        let runner = Runner { db: db.clone(), docker: docker.clone(), data_dir, traefik, live: Default::default() };
        let runners = crate::runners::Runners {
            db: db.clone(),
            docker: docker.clone(),
            http: http.clone(),
            runner: runner.clone(),
            wake: Arc::new(tokio::sync::Notify::new()),
        };
        Ok(Self {
            db,
            docker,
            limiter: Arc::default(),
            secure_cookies,
            runner,
            execs: Arc::default(),
            http,
            files: Arc::default(),
            github_apps: Arc::default(),
            ports: Arc::default(),
            http_access: Arc::default(),
            runners,
            secret_matches: Arc::default(),
            listen: std::net::SocketAddr::from(([0, 0, 0, 0], 27863)),
        })
    }

    pub fn spawn_background(&self) {
        files::spawn_media_sweeper(self.clone());
        self.runners.clone().spawn();
        crate::docker::netlimit::spawn_watcher(self.docker.clone());
        crate::events::spawn_docker_watcher(self.docker.clone());
        crate::volumes::spawn_enforcer(self.db.clone(), self.runner.clone());
    }
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/setup", get(handlers::setup_status).post(handlers::setup))
        .route("/login", post(handlers::login))
        .route("/logout", post(handlers::logout))
        .route("/me", get(handlers::me))
        .route("/me/password", post(handlers::change_own_password))
        .route("/me/api-tokens", get(api_tokens::list_own).post(api_tokens::create))
        .route("/me/api-tokens/{id}", axum::routing::delete(api_tokens::revoke_own))
        .route("/api-tokens", get(api_tokens::list_all))
        .route("/api-tokens/{id}", axum::routing::delete(api_tokens::revoke_any))
        .route("/mcp", get(mcp::get).post(mcp::post))
        .route("/proxy/ui", put(proxy::save_ui))
        .route("/proxy/ui/check", post(proxy::check_ui))
        .route("/users", get(handlers::list_users).post(handlers::create_user))
        .route("/users/{id}", put(handlers::update_user).delete(handlers::delete_user))
        .route("/users/{id}/password", post(handlers::set_user_password))
        .route("/settings", get(handlers::read_settings).put(handlers::write_settings))
        .route("/settings/secret-key", get(handlers::secret_key))
        .route("/docker/info", get(handlers::docker_info))
        .route("/pool", get(handlers::pool_usage))
        .route("/deployments", get(deployments::list).post(deployments::create))
        .route("/deployments/{id}", get(deployments::detail).put(deployments::update).delete(deployments::remove))
        .route("/deployments/{id}/deploy", post(ci::deploy))
        .route("/deployments/{id}/deploy/cancel", post(deployments::cancel_deploy))
        .route("/deployments/{id}/deploy/ws", get(deploys::live))
        .route("/deployments/{id}/deploys", get(deploys::list))
        .route("/deployments/{id}/deploys/{deploy}", get(deploys::detail))
        .route("/deployments/{id}/tokens", get(ci::list_tokens).post(ci::create_token))
        .route("/deployments/{id}/tokens/{token}", axum::routing::delete(ci::delete_token))
        .route("/deployments/{id}/ci-config", get(pipelines::config))
        .route(
            "/deployments/{id}/webhook",
            get(ci::webhook_info).post(ci::rotate_webhook).delete(ci::disable_webhook),
        )
        .route("/deployments/{id}/webhook/install", post(ci::install_webhook))
        .route("/hooks/github/{id}", post(ci::github_hook))
        .route("/hooks/gitlab/{id}", post(ci::gitlab_hook))
        .route("/hooks/gitea/{id}", post(ci::gitea_hook))
        .route("/hooks/github-app", post(identities::github_app_hook))
        .route("/identities", get(identities::list).post(identities::create))
        .route("/identities/github/start", post(identities::github_start))
        .route("/identities/github/callback", get(identities::github_callback))
        .route("/identities/{id}", put(identities::update).delete(identities::remove))
        .route("/identities/{id}/test", post(identities::test))
        .route("/identities/{id}/repos", get(identities::repos))
        .route("/identities/{id}/installed", get(identities::github_installed))
        .route("/saved-secrets", get(saved_secrets::list).post(saved_secrets::create))
        .route("/saved-secrets/match", post(saved_secrets::find_match))
        .route("/saved-secrets/{id}", put(saved_secrets::update).delete(saved_secrets::remove))
        .route("/saved-secrets/{id}/reveal", post(saved_secrets::reveal))
        .route("/deployments/{id}/export", get(bundle::export_deployment))
        .route("/export", get(bundle::export_server))
        .route("/import/preview", post(bundle::preview))
        .route("/import/apply", post(bundle::apply))
        .route("/deployments/{id}/stop", post(deployments::stop))
        .route("/deployments/{id}/restart", post(deployments::restart))
        .route(
            "/deployments/{id}/context",
            put(deployments::upload_context).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)),
        )
        .route("/deployments/{id}/site", put(deployments::upload_site).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)))
        .route("/deployments/{id}/access", get(roles::deployment_access))
        .route("/deployments/{id}/grants/{user}", put(roles::set_grant).delete(roles::remove_grant))
        .route("/users/{id}/grants", get(roles::user_grants).put(roles::set_user_grants))
        .route("/roles", get(roles::list).post(roles::create))
        .route("/roles/{name}", put(roles::update).delete(roles::remove))
        .route("/events/ws", get(events::socket))
        .route("/version", get(update::version))
        .route("/update", post(update::start))
        .route("/update/status", get(update::status))
        .route("/update/latest", get(update::latest))
        .route("/metrics/host", get(metrics::host))
        .route("/metrics/deployments", get(metrics::by_deployment))
        .route("/storage", get(metrics::storage))
        .route("/storage/refresh", post(metrics::refresh_storage))
        .route("/deployments/{id}/metrics", get(metrics::deployment))
        .route("/deployments/{id}/logs", get(metrics::log_files))
        .route("/deployments/{id}/logs/ws", get(metrics::log_socket))
        .route("/proxy", get(proxy::settings).put(proxy::save))
        .route(
            "/deployments/{id}/domains",
            get(proxy::list_domains).post(proxy::add_domain).layer(DefaultBodyLimit::max(DOMAIN_LIMIT)),
        )
        .route("/domains/{id}", put(proxy::update_domain).delete(proxy::remove_domain).layer(DefaultBodyLimit::max(DOMAIN_LIMIT)))
        .route("/domains/{id}/snippets", get(proxy::snippets))
        .route("/domains/{id}/verify", post(proxy::verify))
        .route("/domains/{id}/certificate", put(proxy::pin_certificate))
        .route("/proxy/attention", get(proxy::attention))
        .route("/proxy/ports", get(webserver::ports))
        .route("/proxy/webserver/preview", post(webserver::preview))
        .route("/proxy/webserver/install", post(webserver::install))
        .route("/identities/certificates", post(identities::add_certificate))
        .route("/identities/certificates/cloudflare", post(identities::create_origin_certificate))
        .route("/identities/{id}/origin-pulls", put(identities::origin_pulls))
        .route("/runners", get(runners::list).post(runners::create))
        .route("/runners/{id}", put(runners::update).delete(runners::remove))
        .route("/runners/{id}/instances/{instance}/logs", get(runners::logs))
        .route("/runners/{id}/clear-cache", post(runners::clear_cache))
        .route("/volumes", get(volumes::list).post(volumes::create))
        .route("/volumes/{name}", axum::routing::delete(volumes::remove))
        .route("/deployment-volumes", get(volumes::of_deployments))
        .route("/volume-limits/{name}", put(volumes::set_limit).delete(volumes::remove_limit))
        .route("/deployments/{id}/volumes", get(volumes::of_deployment))
        .route("/containers/{id}/request", post(preview::send).layer(DefaultBodyLimit::max(PREVIEW_REQUEST_LIMIT)))
        .route("/containers/{id}/logs", get(containers::logs))
        .route("/containers/{id}/exec", post(containers::create_exec))
        .route("/containers/{id}/attach", get(containers::attach))
        .route("/containers/{id}/resize", post(containers::resize_container))
        .route(
            "/containers/{id}/files",
            get(files::list).put(files::upload).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)),
        )
        .route("/containers/{id}/files/download", get(files::download))
        .route("/containers/{id}/files/sizes", get(files::sizes))
        .route("/containers/{id}/files/preview", get(files::preview))
        .route("/containers/{id}/files/media", get(files::media).delete(files::drop_media))
        .route("/exec/{id}/ws", get(containers::exec_socket))
        .route("/exec/{id}/resize", post(containers::resize_exec))
        .route_layer(middleware::from_fn_with_state(state.clone(), extract::require_csrf_header));

    let gated = Router::new()
        .nest(API_BASE, api)
        .fallback(crate::web::spa)
        .layer(middleware::from_fn_with_state(state.clone(), http_access::gate));

    Router::new()
        .route("/preview/{container}/{port}", any(preview::proxy_root))
        .route("/preview/{container}/{port}/", any(preview::proxy_index).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)))
        .route("/preview/{container}/{port}/{*rest}", any(preview::proxy).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)))
        .merge(gated)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

