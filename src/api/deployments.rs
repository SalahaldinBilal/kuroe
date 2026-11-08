use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use bollard::models::ContainerSummary;
use bollard::query_parameters::ListContainersOptionsBuilder;
use serde::{Deserialize, Serialize};

use super::access;
use super::error::{ApiError, ApiResult};
use super::extract::{Caller, CurrentUser};
use super::AppState;
use crate::auth::Permission;
use crate::deploy::history::StepRecord;
use crate::deploy::runner::{self, Strategy};
use crate::deploy::spec::{GIT_TOKEN_REF, RunMode, valid_name};
use crate::deploy::{self, Deployment, Source, Spec};
use crate::docker::rewrite::{LABEL_DEPLOYMENT, LABEL_SERVICE};

const MASK: &str = "********";

#[derive(Deserialize)]
pub struct NewDeployment {
    name: String,
    spec: Spec,
}

#[derive(Serialize)]
pub struct ContainerRow {
    id: String,
    name: String,
    service: Option<String>,
    image: Option<String>,
    state: Option<String>,
    status: Option<String>,
    ports: Vec<String>,
    private_ports: Vec<u16>,
}

#[derive(Serialize)]
pub struct Detail {
    #[serde(flatten)]
    deployment: Deployment,
    containers: Vec<ContainerRow>,
    permissions: BTreeSet<Permission>,
    strategy: Strategy,
    strategy_note: Option<&'static str>,
    /// Saved secret names by id, for the references in the spec.
    secret_names: BTreeMap<String, String>,
    last_deploy: Option<String>,
}

fn present(current: &CurrentUser, mut d: Deployment) -> Deployment {
    d.spec = if current.access.can(&d.id, Permission::DepSecrets) { mask_token(d.spec) } else { d.spec.masked() };
    d
}

fn mask_token(mut spec: Spec) -> Spec {
    if let Source::Git { token: Some(token), .. } = &mut spec.source {
        *token = MASK.into();
    }
    spec
}

pub fn check_spec(spec: &Spec, swarm_manager: bool) -> Result<(), String> {
    let empty = match &spec.source {
        Source::Image { image, .. } => image.trim().is_empty(),
        Source::Git { url, .. } => url.trim().is_empty(),
        Source::Compose { file } | Source::Dockerfile { file } => file.trim().is_empty(),
        Source::Upload { .. } | Source::Static {} => false,
    };
    if empty {
        return Err("the source is empty".into());
    }
    if let Source::Git { compose_file: Some(path), .. } = &spec.source {
        crate::deploy::spec::check_compose_file(path)?;
    }
    match (&spec.source, &spec.site) {
        (Source::Static {}, None) => return Err("a static site needs its build and output settings".into()),
        (Source::Git { compose_file: Some(_), .. }, Some(_)) => {
            return Err("a repository is either a Compose stack or a static site, not both".into());
        }
        (Source::Git { .. } | Source::Static {}, Some(site)) => site.check()?,
        (Source::Image { .. }, Some(site)) => {
            crate::sites::check_mime_types(&site.mime_types)?;
            if !matches!(site.build, crate::deploy::spec::SiteBuild::None) {
                return Err("an image is already built, so its files are served as they are".into());
            }
            let output = site.output.trim();
            if !output.starts_with('/') || output.split('/').any(|part| part == "..") {
                return Err("give the folder with the files in the image, like /usr/share/nginx/html".into());
            }
        }
        (_, Some(_)) => return Err("only git repositories, images and uploaded archives can be static sites".into()),
        (_, None) => {}
    }
    if spec.is_static() && spec.mode == RunMode::Stack {
        return Err("static sites are served by the shared file server, not as a Swarm stack".into());
    }
    if spec.mode == RunMode::Stack && !swarm_manager {
        return Err("this Docker host is not a Swarm manager, so stack mode isn't available".into());
    }
    for (name, service) in &spec.services {
        let limits = [
            ("download", service.net_in_mbit),
            ("upload", service.net_out_mbit),
            ("per-connection upload", service.net_conn_out_mbit),
        ];
        for (direction, value) in limits {
            if value.is_some_and(|v| !v.is_finite() || v <= 0.0) {
                return Err(format!("{name}: the {direction} limit must be a positive number of Mbit/s"));
            }
        }
    }
    if let Some(key) = spec.secrets.iter().find(|k| !spec.container.env.contains_key(*k)) {
        return Err(format!("{key} is marked secret but is not in the environment"));
    }
    for key in spec.secret_refs.keys() {
        if key == GIT_TOKEN_REF {
            if !matches!(spec.source, Source::Git { .. }) {
                return Err("only git repositories take an access token".into());
            }
        } else if !spec.container.env.contains_key(key) {
            return Err(format!("{key} points at a saved secret but is not in the environment"));
        }
        if spec.secrets.contains(key) {
            return Err(format!("{key} is a raw secret and a saved secret at once, pick one"));
        }
    }
    Ok(())
}

async fn validate(state: &AppState, spec: &Spec) -> ApiResult<()> {
    let manager = spec.mode == RunMode::Stack
        && crate::docker::is_swarm_manager(&state.docker)
            .await
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("docker: {e:#}")))?;
    check_spec(spec, manager).map_err(ApiError::bad_request)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    state: String,
    exit_code: Option<i64>,
}

impl Observed {
    pub fn new(state: &str, exit_code: Option<i64>) -> Self {
        Observed { state: state.to_owned(), exit_code }
    }
}

fn observe(summary: &ContainerSummary) -> Observed {
    let status = summary.status.as_deref().unwrap_or_default();
    let exit_code = status
        .strip_prefix("Exited (")
        .and_then(|rest| rest.split_once(')'))
        .and_then(|(code, _)| code.parse().ok());
    Observed::new(&summary.state.map(|s| s.to_string()).unwrap_or_default(), exit_code)
}

/// The saved status is the last deploy's result. Docker decides whether a "running" deployment still is.
pub fn live_status(saved: &str, containers: &[Observed], stack: bool) -> String {
    if saved != "running" {
        return saved.to_owned();
    }
    let running = containers.iter().filter(|c| c.state == "running").count();
    let broken = !stack
        && containers.iter().any(|c| matches!(c.state.as_str(), "restarting" | "dead") || (c.state == "exited" && c.exit_code != Some(0)));
    match (running, broken) {
        (0, _) => "down".into(),
        (_, true) => "degraded".into(),
        _ => "running".into(),
    }
}

async fn containers_by_deployment(state: &AppState) -> Option<HashMap<String, Vec<Observed>>> {
    let filters = HashMap::from([("label", vec![LABEL_DEPLOYMENT.to_owned()])]);
    let options = ListContainersOptionsBuilder::new().all(true).filters(&filters).build();
    let containers = state.docker.list_containers(Some(options)).await.ok()?;
    let mut grouped: HashMap<String, Vec<Observed>> = HashMap::new();
    for c in &containers {
        if let Some(deployment) = c.labels.as_ref().and_then(|l| l.get(LABEL_DEPLOYMENT)) {
            grouped.entry(deployment.clone()).or_default().push(observe(c));
        }
    }
    Some(grouped)
}

#[derive(Serialize)]
pub struct ListRow {
    #[serde(flatten)]
    pub deployment: Deployment,
    /// The step a running deploy is in.
    pub step: Option<StepRecord>,
}

pub async fn list(State(state): State<AppState>, current: CurrentUser) -> ApiResult<Json<Vec<ListRow>>> {
    let all = deploy::list(&state.db).await?;
    let observed = containers_by_deployment(&state).await;
    Ok(Json(
        all.into_iter()
            .filter(|d| current.access.can(&d.id, Permission::DepView))
            .map(|mut d| {
                if let Some(observed) = &observed
                    && !d.spec.is_static()
                {
                    let containers = observed.get(&d.id).map(Vec::as_slice).unwrap_or_default();
                    d.status = live_status(&d.status, containers, d.spec.mode == RunMode::Stack);
                }
                let step = state.runner.live.current_step(&d.id);
                ListRow { deployment: present(&current, d), step }
            })
            .collect(),
    ))
}

pub async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<NewDeployment>,
) -> ApiResult<(StatusCode, Json<Deployment>)> {
    current.require(Permission::DepCreate)?;
    if !valid_name(&body.name) {
        return Err(ApiError::bad_request("name must be 1 to 40 characters of a-z, 0-9 and '-'"));
    }
    let mut spec = body.spec;
    validate(&state, &spec).await?;
    super::identities::check_spec(&state, &current, &spec, None).await?;
    super::saved_secrets::check_spec(&state, &current, &spec, None).await?;
    spec.clear_referenced_values();
    match deploy::create(&state.db, &body.name, &spec, &current.user).await {
        Ok(d) => Ok((StatusCode::CREATED, Json(present(&current, d)))),
        Err(err) if is_unique_violation(&err) => {
            Err(ApiError::new(StatusCode::CONFLICT, format!("{} already exists", body.name)))
        }
        Err(err) => Err(err.into()),
    }
}

fn is_unique_violation(err: &anyhow::Error) -> bool {
    err.downcast_ref::<sqlx::Error>()
        .and_then(|e| e.as_database_error())
        .is_some_and(|e| e.is_unique_violation())
}

pub async fn detail(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Detail>> {
    let mut d = access::deployment(&state, &current, &id, Permission::DepView).await?;
    let filters = HashMap::from([("label", vec![format!("{LABEL_DEPLOYMENT}={id}")])]);
    let options = ListContainersOptionsBuilder::new().all(true).filters(&filters).build();
    let summaries = state
        .docker
        .list_containers(Some(options))
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("docker: {e}")))?;
    let observed: Vec<Observed> = summaries.iter().map(observe).collect();
    if !d.spec.is_static() {
        d.status = live_status(&d.status, &observed, d.spec.mode == RunMode::Stack);
    }
    let containers = summaries
        .into_iter()
        .map(|c| {
            let ports = c.ports.unwrap_or_default();
            let mut private_ports: Vec<u16> = ports.iter().map(|p| p.private_port).collect();
            private_ports.sort_unstable();
            private_ports.dedup();
            ContainerRow {
                id: c.id.unwrap_or_default(),
                name: c.names.and_then(|n| n.first().cloned()).unwrap_or_default().trim_start_matches('/').to_owned(),
                service: c.labels.as_ref().and_then(|l| l.get(LABEL_SERVICE).cloned()),
                image: c.image,
                state: c.state.map(|s| s.to_string()),
                status: c.status,
                ports: ports
                    .into_iter()
                    .filter(|p| p.ip.as_deref() != Some("::"))
                    .map(|p| match p.public_port {
                        Some(public) => format!("{}:{}->{}", p.ip.unwrap_or_default(), public, p.private_port),
                        None => p.private_port.to_string(),
                    })
                    .collect(),
                private_ports,
            }
        })
        .collect();
    let permissions = current.access.on(&id);
    let (strategy, strategy_note) = runner::strategy(&d.spec);
    let secret_names = crate::saved_secrets::names(&state.db, &d.spec).await?;
    let last_deploy = crate::deploy::history::latest(&state.db, &id).await?;
    let deployment = present(&current, d);
    Ok(Json(Detail { deployment, containers, permissions, strategy, strategy_note, secret_names, last_deploy }))
}

pub async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(mut spec): Json<Spec>,
) -> ApiResult<StatusCode> {
    let existing = access::deployment(&state, &current, &id, Permission::DepEdit).await?;
    validate(&state, &spec).await?;
    super::identities::check_spec(&state, &current, &spec, Some(&existing.spec)).await?;
    if let (Source::Git { token: new, .. }, Source::Git { token: old, .. }) = (&mut spec.source, &existing.spec.source)
        && new.as_deref() == Some(MASK)
    {
        *new = old.clone();
    }
    if !current.access.can(&id, Permission::DepSecrets) {
        keep_hidden_values(&mut spec, &existing.spec);
    }
    super::saved_secrets::check_spec(&state, &current, &spec, Some(&existing.spec)).await?;
    spec.clear_referenced_values();
    deploy::update_spec(&state.db, &id, &spec).await?;
    if site_types(&spec) != site_types(&existing.spec) {
        refresh_site_types(&state).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

fn site_types(spec: &Spec) -> Option<&crate::sites::MimeTypes> {
    spec.site.as_ref().map(|s| &s.mime_types).filter(|t| !t.is_empty())
}

async fn refresh_site_types(state: &AppState) -> ApiResult<()> {
    state.runner.sites().refresh_config().await.map_err(|e| {
        ApiError::new(StatusCode::BAD_GATEWAY, format!("the static file server didn't take the new file types: {e:#}"))
    })
}

/// The deploy stops where it is and cleans up after itself, then reports through the usual status event.
pub async fn cancel_deploy(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    access::deployment(&state, &current, &id, Permission::DepDeploy).await?;
    if !state.runner.live.cancel(&id, &current.user.username) {
        return Err(ApiError::new(StatusCode::CONFLICT, "no deploy is running"));
    }
    Ok(StatusCode::ACCEPTED)
}

pub async fn stop(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<String>> {
    let d = access::deployment(&state, &current, &id, Permission::DepDeploy).await?;
    let output = state.runner.stop(&d).await?;
    deploy::finish(&state.db, &id, "stopped", &output).await?;
    Ok(Json(output))
}

pub async fn restart(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<String>> {
    let d = access::deployment(&state, &current, &id, Permission::DepDeploy).await?;
    Ok(Json(state.runner.restart(&d).await?))
}

pub async fn remove(State(state): State<AppState>, current: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let d = access::deployment(&state, &current, &id, Permission::DepDelete).await?;
    if d.status == "deploying" {
        return Err(ApiError::new(StatusCode::CONFLICT, "wait for the running deploy to finish"));
    }
    state.runner.remove(&d).await?;
    deploy::delete(&state.db, &id).await?;
    if site_types(&d.spec).is_some() {
        refresh_site_types(&state).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn upload_context(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let d = access::deployment(&state, &current, &id, Permission::DepDeploy).await?;
    if !matches!(d.spec.source, Source::Upload { .. }) {
        return Err(ApiError::bad_request("this deployment does not build from an upload"));
    }
    if body.is_empty() {
        return Err(ApiError::bad_request("the upload is empty"));
    }
    let path = state.runner.context_upload_path(&id);
    tokio::fs::create_dir_all(path.parent().unwrap()).await.map_err(anyhow::Error::from)?;
    tokio::fs::write(&path, &body).await.map_err(anyhow::Error::from)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct SiteUpload {
    #[serde(default)]
    deploy: bool,
}

/// An archive for a static site, from the UI or from CI with a deploy token. `?deploy=true` deploys it right away.
pub async fn upload_site(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    Query(query): Query<SiteUpload>,
    body: Bytes,
) -> ApiResult<StatusCode> {
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
    let started = super::ci::started_by(&caller);
    if !matches!(d.spec.source, Source::Static {}) {
        return Err(ApiError::bad_request("this deployment doesn't take uploaded site files"));
    }
    if crate::sites::archive_kind(&body).is_none() {
        return Err(ApiError::bad_request("upload a .tar, .tar.gz, .tar.bz2 or .tar.xz archive"));
    }
    let path = state.runner.site_upload_path(&id);
    tokio::fs::create_dir_all(path.parent().unwrap()).await.map_err(anyhow::Error::from)?;
    tokio::fs::write(&path, &body).await.map_err(anyhow::Error::from)?;
    if query.deploy {
        super::ci::start(&state, d, host_mounts, started).await?;
        return Ok(StatusCode::ACCEPTED);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Someone without `dep.secrets` only ever saw masked values, so the saved ones stay.
pub fn keep_hidden_values(spec: &mut Spec, existing: &Spec) {
    for (key, value) in spec.container.env.iter_mut() {
        if value == MASK
            && let Some(old) = existing.container.env.get(key)
        {
            value.clone_from(old);
        }
    }
    spec.secrets.clone_from(&existing.secrets);
    spec.secrets.retain(|key| spec.container.env.contains_key(key));
    spec.secret_refs.clone_from(&existing.secret_refs);
    spec.secret_refs.retain(|key, _| key == GIT_TOKEN_REF || spec.container.env.contains_key(key));
}
