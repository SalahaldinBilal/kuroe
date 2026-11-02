use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::auth::{self, Permission, Role, RoleDef, Scope, User};
use crate::identity;
use crate::deploy::spec::{ContainerSpec, RunMode, SiteOptions, valid_name};
use crate::deploy::{self, Source, Spec};
use crate::docker::rewrite::ServiceSettings;
use crate::proxy::{self, RouteOptions};
use crate::runners;
use crate::saved_secrets;

pub const FORMAT: u32 = 1;
pub const GIT_TOKEN: &str = "@git_token";
const SERVER_SETTINGS: &[&str] = &[
    "acme_email",
    "behind_http",
    "behind_https",
    "cert_mode",
    "default_cpus",
    "default_memory_mb",
    "default_net_conn_out_mbit",
    "default_net_in_mbit",
    "default_net_out_mbit",
    "default_pids",
    "dns_identity",
    "dns_wildcard",
    "drain_seconds",
    "pool_cpus",
    "pool_memory_mb",
    "pool_storage_gb",
    "proxy_access_log",
    "proxy_bind",
    "proxy_http_port",
    "proxy_https_port",
    "proxy_mode",
    "public_url",
    "retention_logs_days",
    "retention_metrics_days",
    "web_server",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub format: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerBundle>,
    #[serde(default, rename = "deployment")]
    pub deployments: Vec<DeploymentBundle>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerBundle {
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default, rename = "role", skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RoleBundle>,
    #[serde(default, rename = "identity", skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<IdentityBundle>,
    #[serde(default, rename = "user")]
    pub users: Vec<UserBundle>,
    #[serde(default, rename = "runner_pool", skip_serializing_if = "Vec::is_empty")]
    pub runner_pools: Vec<RunnerPoolBundle>,
    /// Saved secrets by name. Their values never leave, even with secrets on.
    #[serde(default, rename = "secret", skip_serializing_if = "Vec::is_empty")]
    pub saved_secrets: Vec<SavedSecretBundle>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedSecretBundle {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

pub fn saved_secret_key(name: &str) -> String {
    format!("@secret/{name}")
}

/// A runner pool, naming its identity. Imported pools start switched off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerPoolBundle {
    pub name: String,
    pub provider: runners::Provider,
    pub identity: String,
    pub scope_kind: runners::ScopeKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    pub min_idle: i64,
    pub max: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_in_mbit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_out_mbit: Option<f64>,
    pub docker_mode: runners::DockerMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

impl RunnerPoolBundle {
    fn of(pool: runners::Pool, identity: String) -> Self {
        RunnerPoolBundle {
            name: pool.name,
            provider: pool.provider,
            identity,
            scope_kind: pool.scope_kind,
            scope: pool.scope,
            labels: pool.labels,
            min_idle: pool.min_idle,
            max: pool.max,
            cpus: pool.cpus,
            memory_mb: pool.memory_mb,
            net_in_mbit: pool.net_in_mbit,
            net_out_mbit: pool.net_out_mbit,
            docker_mode: pool.docker_mode,
            image: pool.image,
        }
    }

    fn input(&self, identity_id: String, enabled: bool) -> runners::PoolInput {
        runners::PoolInput {
            name: self.name.clone(),
            provider: self.provider,
            identity: identity_id,
            scope_kind: self.scope_kind,
            scope: self.scope.clone(),
            labels: self.labels.clone(),
            min_idle: self.min_idle,
            max: self.max,
            cpus: self.cpus,
            memory_mb: self.memory_mb,
            net_in_mbit: self.net_in_mbit,
            net_out_mbit: self.net_out_mbit,
            docker_mode: self.docker_mode,
            image: self.image.clone(),
            enabled,
        }
    }
}

/// A saved login without its secret. Import asks for the secret when the target server doesn't have it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityBundle {
    pub name: String,
    pub kind: identity::Kind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "is_empty_meta")]
    pub meta: serde_json::Value,
}

fn is_empty_meta(meta: &serde_json::Value) -> bool {
    meta.as_object().is_none_or(|m| m.is_empty())
}

pub fn identity_secret_key(name: &str) -> String {
    format!("@identity/{name}")
}

pub fn identity_webhook_key(name: &str) -> String {
    format!("@identity/{name}/webhook")
}

/// Sources hold identity ids on a server and identity names in a bundle.
async fn source_identities(db: &SqlitePool, source: &mut Source, to_names: bool) -> anyhow::Result<()> {
    let slot = match source {
        Source::Git { identity, .. } => identity,
        Source::Image { registry_identity, .. } => registry_identity,
        _ => return Ok(()),
    };
    let Some(value) = slot.clone() else { return Ok(()) };
    let found = if to_names { identity::get(db, &value).await? } else { identity::by_name(db, &value).await? };
    *slot = match (found, to_names) {
        (Some(i), true) => Some(i.name),
        (Some(i), false) => Some(i.id),
        (None, true) => None,
        (None, false) => bail!("the login {value} is neither on this server nor in the bundle"),
    };
    Ok(())
}

/// A custom role. A scope list names deployments, not ids, so it survives a move to another server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleBundle {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub permissions: BTreeSet<Permission>,
    pub scope: Scope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrantBundle {
    pub deployment: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserBundle {
    pub username: String,
    pub role: Role,
    #[serde(default)]
    pub host_mounts: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<GrantBundle>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DomainBundle {
    pub hostname: String,
    pub service: String,
    pub port: u16,
    pub tls: bool,
    /// A pinned certificate by name, or `acme`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
    #[serde(default, skip_serializing_if = "RouteOptions::is_empty")]
    pub options: RouteOptions,
}

/// Settings and domain pins hold identity ids on a server and names in a bundle. `acme` passes through.
async fn identity_ref(db: &SqlitePool, value: Option<&str>, to_names: bool) -> anyhow::Result<Option<String>> {
    let Some(value) = value.filter(|v| !v.is_empty()) else { return Ok(None) };
    if value == crate::proxy::certs::PIN_ACME {
        return Ok(Some(value.to_owned()));
    }
    let found = if to_names { identity::get(db, value).await? } else { identity::by_name(db, value).await? };
    match (found, to_names) {
        (Some(i), true) => Ok(Some(i.name)),
        (Some(i), false) => Ok(Some(i.id)),
        (None, true) => Ok(None),
        (None, false) => bail!("the identity {value} is neither on this server nor in the bundle"),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentBundle {
    pub name: String,
    #[serde(default)]
    pub mode: RunMode,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub secrets: BTreeSet<String>,
    /// The deployment had a push webhook. Its secret never leaves, because the hook URL changes with the server.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub webhook: bool,
    pub source: Source,
    #[serde(default)]
    pub container: ContainerSpec,
    #[serde(default)]
    pub services: BTreeMap<String, ServiceSettings>,
    /// Env keys (and `@git_token`) that use a saved secret, by the secret's name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_refs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<SiteOptions>,
    #[serde(default, rename = "domain")]
    pub domains: Vec<DomainBundle>,
    /// Deploy tokens by name. Their values are stored hashed, so import makes new ones.
    #[serde(default, rename = "token", skip_serializing_if = "Vec::is_empty")]
    pub tokens: Vec<TokenBundle>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenBundle {
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub host_mounts: bool,
}

impl DeploymentBundle {
    fn from_spec(name: &str, spec: &Spec, domains: Vec<DomainBundle>, include_secrets: bool) -> Self {
        let mut spec = spec.clone();
        let mut secrets = spec.secrets.clone();
        if let Source::Git { token, .. } = &mut spec.source
            && token.is_some()
        {
            secrets.insert(GIT_TOKEN.into());
            if !include_secrets {
                *token = None;
            }
        }
        if !include_secrets {
            for key in &spec.secrets {
                if let Some(value) = spec.container.env.get_mut(key) {
                    value.clear();
                }
            }
        }
        DeploymentBundle {
            name: name.to_owned(),
            mode: spec.mode,
            secrets,
            webhook: false,
            source: spec.source,
            container: spec.container,
            services: spec.services,
            secret_refs: spec.secret_refs,
            site: spec.site,
            domains,
            tokens: Vec::new(),
        }
    }

    /// `refs` maps the referencing keys to saved secret ids on this server.
    fn spec(&self, secrets: &BTreeMap<String, String>, refs: BTreeMap<String, String>) -> Spec {
        let mut spec = Spec {
            source: self.source.clone(),
            container: self.container.clone(),
            services: self.services.clone(),
            mode: self.mode,
            secrets: self.secrets.iter().filter(|k| *k != GIT_TOKEN).cloned().collect(),
            secret_refs: refs,
            site: self.site.clone(),
        };
        for key in &spec.secrets {
            if let Some(value) = secrets.get(&secret_key(&self.name, key)) {
                spec.container.env.insert(key.clone(), value.clone());
            }
        }
        if let Source::Git { token, .. } = &mut spec.source
            && let Some(value) = secrets.get(&secret_key(&self.name, GIT_TOKEN))
        {
            *token = Some(value.clone());
        }
        spec
    }
}

pub fn secret_key(deployment: &str, key: &str) -> String {
    format!("{deployment}/{key}")
}

pub fn user_secret_key(username: &str) -> String {
    format!("@user/{username}")
}

pub fn to_toml(bundle: &Bundle) -> anyhow::Result<String> {
    let body = toml::to_string(bundle)?;
    let missing = left_out(bundle);
    if missing.is_empty() {
        return Ok(body);
    }
    let mut out = String::from("# Left out of this file. The import asks for these values or makes new ones.\n");
    for line in &missing {
        out.push_str(&format!("#   {line}\n"));
    }
    out.push('\n');
    out.push_str(&body);
    Ok(out)
}

/// Every value the bundle names but doesn't carry, one line each.
pub fn left_out(bundle: &Bundle) -> Vec<String> {
    let mut lines = Vec::new();
    for d in &bundle.deployments {
        for key in d.secrets.iter().filter(|k| !has_value(d, k)) {
            lines.push(if key == GIT_TOKEN { format!("{}: git access token", d.name) } else { format!("{}: {key} (env)", d.name) });
        }
        if d.webhook {
            lines.push(format!("{}: push webhook secret (a new one is made on import)", d.name));
        }
        for token in &d.tokens {
            lines.push(format!("{}: deploy token \"{}\" (a new one is made on import)", d.name, token.name));
        }
    }
    if let Some(server) = &bundle.server {
        for i in &server.identities {
            lines.push(format!("login {}: secret", i.name));
            if i.kind == identity::Kind::GithubApp {
                lines.push(format!("login {}: GitHub App webhook secret", i.name));
            }
        }
        for user in &server.users {
            lines.push(format!("user {}: password", user.username));
        }
    }
    for name in saved_secret_names(bundle).keys() {
        lines.push(format!("saved secret {name}: value"));
    }
    lines
}

/// Every saved secret a bundle names, with its description when the server part lists it.
pub fn saved_secret_names(bundle: &Bundle) -> BTreeMap<String, Option<String>> {
    let mut names: BTreeMap<String, Option<String>> = BTreeMap::new();
    for s in bundle.server.iter().flat_map(|s| &s.saved_secrets) {
        names.insert(s.name.clone(), Some(s.description.clone()));
    }
    for name in bundle.deployments.iter().flat_map(|d| d.secret_refs.values()) {
        names.entry(name.clone()).or_insert(None);
    }
    names
}

pub fn from_toml(text: &str) -> anyhow::Result<Bundle> {
    let bundle: Bundle = toml::from_str(text).context("reading the bundle")?;
    if bundle.format > FORMAT {
        bail!("this bundle uses format {}, made by a newer kuroe. This kuroe reads format {FORMAT}, update it first", bundle.format);
    }
    if bundle.format != FORMAT {
        bail!("bundle format {} is not supported, this kuroe reads format {FORMAT}", bundle.format);
    }
    let mut seen = BTreeSet::new();
    for d in &bundle.deployments {
        if !valid_name(&d.name) {
            bail!("deployment name {:?} is not valid", d.name);
        }
        if !seen.insert(&d.name) {
            bail!("deployment {} appears twice", d.name);
        }
        for domain in &d.domains {
            if !proxy::valid_hostname(&domain.hostname) {
                bail!("{} has a domain {:?} that is not a valid hostname", d.name, domain.hostname);
            }
            domain.options.check().map_err(|e| anyhow::anyhow!("{}: {e}", domain.hostname))?;
        }
    }
    Ok(bundle)
}

async fn domains_of(db: &SqlitePool, deployment_id: &str) -> anyhow::Result<Vec<DomainBundle>> {
    let mut out = Vec::new();
    for d in proxy::domains_for(db, deployment_id).await? {
        let certificate = identity_ref(db, d.certificate.as_deref(), true).await?;
        out.push(DomainBundle { hostname: d.hostname, service: d.service, port: d.port as u16, tls: d.tls, certificate, options: d.options });
    }
    Ok(out)
}

async fn deployment_bundle(db: &SqlitePool, d: &deploy::Deployment, include_secrets: bool) -> anyhow::Result<DeploymentBundle> {
    let mut bundle = DeploymentBundle::from_spec(&d.name, &d.spec, domains_of(db, &d.id).await?, include_secrets);
    source_identities(db, &mut bundle.source, true).await?;
    let names = saved_secrets::names(db, &d.spec).await?;
    bundle.secret_refs = d.spec.secret_refs.iter().filter_map(|(key, id)| Some((key.clone(), names.get(id)?.clone()))).collect();
    bundle.webhook = deploy::has_webhook(db, &d.id).await?;
    bundle.tokens = deploy::tokens::list(db, &d.id)
        .await?
        .into_iter()
        .map(|t| TokenBundle { name: t.name, host_mounts: t.host_mounts })
        .collect();
    Ok(bundle)
}

pub async fn export_deployment(db: &SqlitePool, deployment_id: &str, include_secrets: bool) -> anyhow::Result<DeploymentBundle> {
    let d = deploy::get(db, deployment_id).await?.context("no such deployment")?;
    deployment_bundle(db, &d, include_secrets).await
}

pub async fn export_server(db: &SqlitePool, include_secrets: bool) -> anyhow::Result<Bundle> {
    let mut deployments = Vec::new();
    for d in deploy::list(db).await? {
        deployments.push(deployment_bundle(db, &d, include_secrets).await?);
    }
    let identities = identity::list(db)
        .await?
        .into_iter()
        .map(|i| IdentityBundle { name: i.name, kind: i.kind, host: i.host, username: i.username, meta: i.meta })
        .collect();
    let settings: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings ORDER BY key").fetch_all(db).await?;
    let mut settings: BTreeMap<String, String> =
        settings.into_iter().filter(|(k, v)| SERVER_SETTINGS.contains(&k.as_str()) && !v.is_empty()).collect();
    if let Some(id) = settings.remove("dns_identity")
        && let Some(name) = identity_ref(db, Some(&id), true).await?
    {
        settings.insert("dns_identity".into(), name);
    }
    let volumes = sqlx::query_scalar("SELECT name FROM volumes ORDER BY name").fetch_all(db).await?;
    let rows: Vec<(String, String, String, bool)> =
        sqlx::query_as("SELECT id, username, role, host_mounts FROM users ORDER BY username").fetch_all(db).await?;
    let mut users = Vec::new();
    for (id, username, role, host_mounts) in rows {
        let grants: Vec<(String, String)> = sqlx::query_as(
            "SELECT d.name, g.role FROM deployment_grants g JOIN deployments d ON d.id = g.deployment_id WHERE g.user_id = ? ORDER BY d.name",
        )
        .bind(&id)
        .fetch_all(db)
        .await?;
        let grants = grants.into_iter().map(|(deployment, role)| GrantBundle { deployment, role }).collect();
        users.push(UserBundle { username, role: Role::from(role), host_mounts, grants });
    }
    let mut roles = Vec::new();
    for role in RoleDef::list(db).await?.into_iter().filter(|r| !r.builtin) {
        roles.push(RoleBundle {
            scope: scope_names(db, &role.scope).await?,
            name: role.name,
            description: role.description,
            permissions: role.permissions,
        });
    }
    let mut runner_pools = Vec::new();
    for pool in runners::list(db).await? {
        if let Some(identity) = identity::get(db, &pool.identity).await? {
            runner_pools.push(RunnerPoolBundle::of(pool, identity.name));
        }
    }
    let saved_secrets = saved_secrets::list(db)
        .await?
        .into_iter()
        .map(|s| SavedSecretBundle { name: s.name, description: s.description })
        .collect();
    Ok(Bundle {
        format: FORMAT,
        server: Some(ServerBundle { settings, volumes, roles, identities, users, runner_pools, saved_secrets }),
        deployments,
    })
}

async fn scope_names(db: &SqlitePool, scope: &Scope) -> sqlx::Result<Scope> {
    let Scope::Some(ids) = scope else { return Ok(scope.clone()) };
    let mut names = BTreeSet::new();
    for id in ids {
        if let Some(name) = sqlx::query_scalar("SELECT name FROM deployments WHERE id = ?").bind(id).fetch_optional(db).await? {
            names.insert(name);
        }
    }
    Ok(Scope::Some(names))
}

async fn scope_ids(db: &SqlitePool, scope: &Scope) -> sqlx::Result<Scope> {
    let Scope::Some(names) = scope else { return Ok(scope.clone()) };
    let mut ids = BTreeSet::new();
    for name in names {
        if let Some(id) = sqlx::query_scalar("SELECT id FROM deployments WHERE name = ?").bind(name).fetch_optional(db).await? {
            ids.insert(id);
        }
    }
    Ok(Scope::Some(ids))
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Plan {
    pub deployments: Vec<Change>,
    pub volumes: Vec<String>,
    pub settings: Vec<SettingChange>,
    pub roles: Vec<Change>,
    pub identities: Vec<Change>,
    pub users: Vec<Change>,
    pub runner_pools: Vec<Change>,
    pub saved_secrets: Vec<Change>,
    pub secrets: Vec<SecretPrompt>,
    /// Deployments that get a new push webhook secret.
    pub webhooks: Vec<String>,
    /// Deploy tokens that get made, with new values.
    pub tokens: Vec<TokenPlan>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TokenPlan {
    pub deployment: String,
    pub name: String,
}

/// The kind of the identity a pool names, from this server or from the bundle.
async fn pool_identity_kind(db: &SqlitePool, bundle: &Bundle, name: &str) -> anyhow::Result<identity::Kind> {
    if let Some(existing) = identity::by_name(db, name).await? {
        return Ok(existing.kind);
    }
    bundle
        .server
        .iter()
        .flat_map(|s| &s.identities)
        .find(|i| i.name == name)
        .map(|i| i.kind)
        .with_context(|| format!("the runner identity {name} is neither on this server nor in the bundle"))
}

async fn plan_runner_pools(db: &SqlitePool, bundle: &Bundle, plan: &mut Plan) -> anyhow::Result<()> {
    for incoming in bundle.server.iter().flat_map(|s| &s.runner_pools) {
        let kind = pool_identity_kind(db, bundle, &incoming.identity).await?;
        incoming.input(String::new(), false).check(kind).map_err(|e| anyhow::anyhow!("runner pool {}: {e}", incoming.name))?;
        let current = runners::list(db).await?.into_iter().find(|p| p.name == incoming.name);
        let change = match current {
            None => Change { name: incoming.name.clone(), action: "create", fields: Vec::new() },
            Some(pool) => {
                let identity = identity::get(db, &pool.identity).await?.map(|i| i.name).unwrap_or_default();
                let now = RunnerPoolBundle::of(pool, identity);
                let mut fields = Vec::new();
                for (name, same) in [
                    ("provider", now.provider == incoming.provider),
                    ("identity", now.identity == incoming.identity),
                    ("scope", now.scope_kind == incoming.scope_kind && now.scope == incoming.scope),
                    ("labels", now.labels == incoming.labels),
                    ("counts", now.min_idle == incoming.min_idle && now.max == incoming.max),
                    (
                        "limits",
                        now.cpus == incoming.cpus
                            && now.memory_mb == incoming.memory_mb
                            && now.net_in_mbit == incoming.net_in_mbit
                            && now.net_out_mbit == incoming.net_out_mbit,
                    ),
                    ("docker", now.docker_mode == incoming.docker_mode),
                    ("image", now.image == incoming.image),
                ] {
                    if !same {
                        fields.push(name.to_owned());
                    }
                }
                if fields.iter().any(|f| f == "provider" || f == "identity" || f == "scope") {
                    bail!("runner pool {} serves something else here, rename one of them first", incoming.name);
                }
                Change { name: incoming.name.clone(), action: if fields.is_empty() { "unchanged" } else { "update" }, fields }
            }
        };
        plan.runner_pools.push(change);
    }
    Ok(())
}

async fn apply_runner_pools(db: &SqlitePool, bundle: &Bundle, actor: &User) -> anyhow::Result<()> {
    for incoming in bundle.server.iter().flat_map(|s| &s.runner_pools) {
        let identity = identity::by_name(db, &incoming.identity).await?.with_context(|| format!("identity {} vanished", incoming.identity))?;
        let current = runners::list(db).await?.into_iter().find(|p| p.name == incoming.name);
        let result = match current {
            Some(pool) => {
                let enabled = pool.enabled;
                runners::update(db, &pool, incoming.input(identity.id.clone(), enabled), identity.kind).await?
            }
            None => runners::insert(db, incoming.input(identity.id.clone(), false), identity.kind, &actor.id).await?.map(|_| ()),
        };
        result.map_err(|e| anyhow::anyhow!("runner pool {}: {e}", incoming.name))?;
    }
    Ok(())
}

fn source_identity(source: &Source) -> Option<&str> {
    match source {
        Source::Git { identity, .. } => identity.as_deref(),
        Source::Image { registry_identity, .. } => registry_identity.as_deref(),
        _ => None,
    }
}

async fn plan_identities(db: &SqlitePool, bundle: &Bundle, plan: &mut Plan) -> anyhow::Result<()> {
    let mut known: BTreeSet<String> = sqlx::query_scalar("SELECT name FROM identities").fetch_all(db).await?.into_iter().collect();
    for incoming in bundle.server.iter().flat_map(|s| &s.identities) {
        let change = match identity::by_name(db, &incoming.name).await? {
            Some(existing) if existing.kind != incoming.kind => {
                bail!("the login {} is a {} here but a {} in the bundle", incoming.name, existing.kind.as_str(), incoming.kind.as_str());
            }
            Some(_) => "unchanged",
            None => {
                let label = if incoming.kind == identity::Kind::GithubApp {
                    plan.secrets.push(SecretPrompt {
                        key: identity_webhook_key(&incoming.name),
                        label: format!("{}: GitHub App webhook secret", incoming.name),
                    });
                    format!("{}: GitHub App private key (PEM)", incoming.name)
                } else if incoming.kind == identity::Kind::Certificate {
                    format!("{}: certificate private key (PEM)", incoming.name)
                } else {
                    format!("{}: token or password", incoming.name)
                };
                plan.secrets.push(SecretPrompt { key: identity_secret_key(&incoming.name), label });
                "create"
            }
        };
        plan.identities.push(Change { name: incoming.name.clone(), action: change, fields: Vec::new() });
        known.insert(incoming.name.clone());
    }
    for d in &bundle.deployments {
        if let Some(name) = source_identity(&d.source).filter(|n| !known.contains(*n)) {
            bail!("{} uses the login {name}, add it on the Identities page first or export the whole server", d.name);
        }
    }
    Ok(())
}

async fn apply_identities(db: &SqlitePool, bundle: &Bundle, secrets: &BTreeMap<String, String>) -> anyhow::Result<()> {
    for incoming in bundle.server.iter().flat_map(|s| &s.identities) {
        if identity::by_name(db, &incoming.name).await?.is_some() {
            continue;
        }
        let given = secrets.get(&identity_secret_key(&incoming.name)).cloned().unwrap_or_default();
        let secret = if incoming.kind == identity::Kind::GithubApp {
            let webhook_secret = secrets.get(&identity_webhook_key(&incoming.name)).cloned().unwrap_or_default();
            serde_json::to_string(&identity::github::AppSecret { pem: given, webhook_secret })?
        } else {
            given
        };
        let new = identity::NewIdentity {
            id: None,
            kind: incoming.kind,
            name: &incoming.name,
            host: &incoming.host,
            username: &incoming.username,
            secret: &secret,
            meta: &incoming.meta,
            expires_at: None,
        };
        identity::insert(db, &new).await?;
    }
    Ok(())
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Change {
    pub name: String,
    pub action: &'static str,
    pub fields: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct SettingChange {
    pub key: String,
    pub from: Option<String>,
    pub to: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct SecretPrompt {
    pub key: String,
    pub label: String,
}

fn changed_fields(old: &DeploymentBundle, new: &DeploymentBundle) -> Vec<String> {
    let mut fields = Vec::new();
    let mut check = |name: &str, same: bool| {
        if !same {
            fields.push(name.to_owned());
        }
    };
    check("source", old.source == new.source);
    check("mode", old.mode == new.mode);
    check("ports", old.container.ports == new.container.ports);
    check("command", old.container.command == new.container.command);
    let env_keys = |b: &DeploymentBundle| b.container.env.iter().map(|(k, v)| (k.clone(), if b.secrets.contains(k) { None } else { Some(v.clone()) })).collect::<Vec<_>>();
    check("env", env_keys(old) == env_keys(new));
    check("secrets", old.secrets == new.secrets);
    check("services", old.services == new.services);
    check("site", old.site == new.site);
    check("domains", old.domains == new.domains);
    check("saved secrets", old.secret_refs == new.secret_refs);
    check("webhook", old.webhook == new.webhook);
    check("tokens", old.tokens == new.tokens);
    fields
}

fn has_value(bundle: &DeploymentBundle, key: &str) -> bool {
    spec_has_value(&bundle.source, &bundle.container, key)
}

fn spec_has_value(source: &Source, container: &ContainerSpec, key: &str) -> bool {
    if key == GIT_TOKEN {
        return matches!(source, Source::Git { token: Some(t), .. } if !t.is_empty());
    }
    container.env.get(key).is_some_and(|v| !v.is_empty())
}

/// Tokens in the bundle that the deployment doesn't have yet, by name.
async fn missing_tokens<'a>(db: &SqlitePool, id: Option<&str>, incoming: &'a DeploymentBundle) -> sqlx::Result<Vec<&'a TokenBundle>> {
    let have: BTreeSet<String> = match id {
        Some(id) => deploy::tokens::list(db, id).await?.into_iter().map(|t| t.name).collect(),
        None => BTreeSet::new(),
    };
    Ok(incoming.tokens.iter().filter(|t| !have.contains(&t.name)).collect())
}

/// A new webhook is made when the bundle had one, the target has none, and pushes don't come through a GitHub App.
async fn needs_webhook(db: &SqlitePool, id: Option<&str>, incoming: &DeploymentBundle) -> anyhow::Result<bool> {
    if !incoming.webhook || !matches!(incoming.source, Source::Git { .. }) {
        return Ok(false);
    }
    if let Some(name) = source_identity(&incoming.source) {
        let app = match identity::by_name(db, name).await? {
            Some(existing) => existing.kind == identity::Kind::GithubApp,
            None => false,
        };
        if app {
            return Ok(false);
        }
    }
    Ok(match id {
        Some(id) => !deploy::has_webhook(db, id).await?,
        None => true,
    })
}

async fn plan_saved_secrets(db: &SqlitePool, bundle: &Bundle, plan: &mut Plan) -> anyhow::Result<()> {
    for (name, description) in saved_secret_names(bundle) {
        if !saved_secrets::valid_name(&name) {
            bail!("saved secret name {name:?} is not valid");
        }
        let change = match saved_secrets::by_name(db, &name).await? {
            Some(existing) => {
                let fields: Vec<String> =
                    description.filter(|d| *d != existing.description).map(|_| "description".to_owned()).into_iter().collect();
                Change { name: name.clone(), action: if fields.is_empty() { "unchanged" } else { "update" }, fields }
            }
            None => {
                plan.secrets.push(SecretPrompt { key: saved_secret_key(&name), label: format!("saved secret {name}") });
                Change { name: name.clone(), action: "create", fields: Vec::new() }
            }
        };
        plan.saved_secrets.push(change);
    }
    Ok(())
}

async fn apply_saved_secrets(db: &SqlitePool, bundle: &Bundle, secrets: &BTreeMap<String, String>, actor: &User) -> anyhow::Result<()> {
    for (name, description) in saved_secret_names(bundle) {
        match saved_secrets::by_name(db, &name).await? {
            Some(existing) => {
                if let Some(description) = description.filter(|d| *d != existing.description) {
                    saved_secrets::update(db, &existing.id, &name, &description, None).await?;
                }
            }
            None => {
                let value = secrets.get(&saved_secret_key(&name)).with_context(|| format!("missing the value of saved secret {name}"))?;
                saved_secrets::insert(db, &name, description.as_deref().unwrap_or_default(), value, &actor.id).await?;
            }
        }
    }
    Ok(())
}

/// Saved secret names in a deployment bundle turned into ids on this server.
async fn ref_ids(db: &SqlitePool, incoming: &DeploymentBundle) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (key, name) in &incoming.secret_refs {
        let found = saved_secrets::by_name(db, name).await?.with_context(|| format!("saved secret {name} vanished"))?;
        out.insert(key.clone(), found.id);
    }
    Ok(out)
}

pub async fn plan(db: &SqlitePool, bundle: &Bundle) -> anyhow::Result<Plan> {
    let mut plan = Plan::default();
    plan_identities(db, bundle, &mut plan).await?;
    plan_runner_pools(db, bundle, &mut plan).await?;
    plan_saved_secrets(db, bundle, &mut plan).await?;
    for incoming in &bundle.deployments {
        let existing = sqlx::query_scalar::<_, String>("SELECT id FROM deployments WHERE name = ?").bind(&incoming.name).fetch_optional(db).await?;
        let (action, fields) = match &existing {
            None => ("create", Vec::new()),
            Some(id) => {
                let current = export_deployment(db, id, false).await?;
                let fields = changed_fields(&current, incoming);
                (if fields.is_empty() { "unchanged" } else { "update" }, fields)
            }
        };
        plan.deployments.push(Change { name: incoming.name.clone(), action, fields });
        let current = match &existing {
            Some(id) => deploy::get(db, id).await?.map(|d| d.spec),
            None => None,
        };
        for key in &incoming.secrets {
            let kept = current.as_ref().is_some_and(|spec| spec_has_value(&spec.source, &spec.container, key));
            if !has_value(incoming, key) && !kept {
                let label = if key == GIT_TOKEN { format!("{}: git access token", incoming.name) } else { format!("{}: {key}", incoming.name) };
                plan.secrets.push(SecretPrompt { key: secret_key(&incoming.name, key), label });
            }
        }
        if needs_webhook(db, existing.as_deref(), incoming).await? {
            plan.webhooks.push(incoming.name.clone());
        }
        for token in missing_tokens(db, existing.as_deref(), incoming).await? {
            plan.tokens.push(TokenPlan { deployment: incoming.name.clone(), name: token.name.clone() });
        }
    }
    if let Some(server) = &bundle.server {
        let known: BTreeSet<String> = sqlx::query_scalar("SELECT name FROM volumes").fetch_all(db).await?.into_iter().collect();
        plan.volumes = server.volumes.iter().filter(|v| !known.contains(*v)).cloned().collect();
        for (key, to) in &server.settings {
            if !SERVER_SETTINGS.contains(&key.as_str()) {
                bail!("setting {key} can't be imported");
            }
            let mut from: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(db).await?;
            if key == "dns_identity" {
                from = identity_ref(db, from.as_deref(), true).await?;
            }
            if from.as_deref() != Some(to.as_str()) {
                plan.settings.push(SettingChange { key: key.clone(), from, to: to.clone() });
            }
        }
        let mut known_roles: BTreeSet<String> = sqlx::query_scalar("SELECT name FROM roles").fetch_all(db).await?.into_iter().collect();
        for role in &server.roles {
            if !valid_name(&role.name) || Role::BUILTIN.contains(&role.name.as_str()) {
                bail!("role name {:?} can't be imported", role.name);
            }
            if role.scope == Scope::Granted {
                bail!("role {} has a scope only built-in roles can use", role.name);
            }
            let change = match RoleDef::load(db, &role.name).await? {
                None => Change { name: role.name.clone(), action: "create", fields: Vec::new() },
                Some(current) => {
                    let mut fields = Vec::new();
                    if current.description != role.description {
                        fields.push("description".to_owned());
                    }
                    if current.permissions != role.permissions {
                        fields.push("permissions".to_owned());
                    }
                    if scope_names(db, &current.scope).await? != role.scope {
                        fields.push("scope".to_owned());
                    }
                    Change { name: role.name.clone(), action: if fields.is_empty() { "unchanged" } else { "update" }, fields }
                }
            };
            plan.roles.push(change);
            known_roles.insert(role.name.clone());
        }
        for user in &server.users {
            let unknown = std::iter::once(user.role.as_str())
                .chain(user.grants.iter().map(|g| g.role.as_str()))
                .find(|r| !known_roles.contains(*r));
            if let Some(role) = unknown {
                bail!("{} uses the role {role}, which is neither on this server nor in the bundle", user.username);
            }
        }
        for user in &server.users {
            let current: Option<(String, bool)> =
                sqlx::query_as("SELECT role, host_mounts FROM users WHERE username = ?").bind(&user.username).fetch_optional(db).await?;
            let change = match current {
                None => {
                    plan.secrets.push(SecretPrompt { key: user_secret_key(&user.username), label: format!("password for {}", user.username) });
                    Change { name: user.username.clone(), action: "create", fields: Vec::new() }
                }
                Some((role, host_mounts)) => {
                    let mut fields = Vec::new();
                    if role != user.role.as_str() {
                        fields.push("role".to_owned());
                    }
                    if host_mounts != user.host_mounts {
                        fields.push("host_mounts".to_owned());
                    }
                    Change { name: user.username.clone(), action: if fields.is_empty() { "unchanged" } else { "update" }, fields }
                }
            };
            plan.users.push(change);
        }
    }
    Ok(plan)
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Applied {
    pub deployments: Vec<String>,
    pub volumes_to_create: Vec<String>,
    pub webhooks: Vec<NewWebhook>,
    pub tokens: Vec<NewToken>,
}

/// Plain values made during the import. They're shown once and only stored sealed or hashed.
#[derive(Debug, Serialize, PartialEq)]
pub struct NewWebhook {
    pub deployment: String,
    pub deployment_id: String,
    pub secret: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct NewToken {
    pub deployment: String,
    pub deployment_id: String,
    pub name: String,
    pub token: String,
}

pub async fn apply(db: &SqlitePool, bundle: &Bundle, secrets: &BTreeMap<String, String>, actor: &User) -> anyhow::Result<Applied> {
    let plan = plan(db, bundle).await?;
    let missing: Vec<&str> = plan.secrets.iter().filter(|p| secrets.get(&p.key).is_none_or(|v| v.is_empty())).map(|p| p.label.as_str()).collect();
    if !missing.is_empty() {
        bail!("missing secrets: {}", missing.join(", "));
    }
    let mut applied = Applied::default();
    let actor_host_mounts = actor.role.is_owner()
        || sqlx::query_scalar::<_, bool>("SELECT host_mounts FROM users WHERE id = ?").bind(&actor.id).fetch_optional(db).await?.unwrap_or(false);
    apply_identities(db, bundle, secrets).await?;
    apply_runner_pools(db, bundle, actor).await?;
    apply_saved_secrets(db, bundle, secrets, actor).await?;
    if let Some(server) = &bundle.server {
        for name in &plan.volumes {
            sqlx::query("INSERT OR IGNORE INTO volumes (name, created_by, created_at) VALUES (?, ?, ?)")
                .bind(name)
                .bind(&actor.id)
                .bind(crate::db::now())
                .execute(db)
                .await?;
            applied.volumes_to_create.push(name.clone());
        }
        for change in &plan.settings {
            let value = if change.key == "dns_identity" { identity_ref(db, Some(&change.to), false).await?.unwrap_or_default() } else { change.to.clone() };
            sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
                .bind(&change.key)
                .bind(value)
                .execute(db)
                .await?;
        }
        for user in &server.users {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE username = ?)").bind(&user.username).fetch_one(db).await?;
            if !exists {
                let password = &secrets[&user_secret_key(&user.username)];
                auth::create_user(db, &user.username, password, user.role.clone()).await?;
            } else if !user.role.is_owner() {
                sqlx::query("UPDATE users SET role = ? WHERE username = ? AND role != 'owner'").bind(user.role.as_str()).bind(&user.username).execute(db).await?;
            }
            sqlx::query("UPDATE users SET host_mounts = ? WHERE username = ?").bind(user.host_mounts).bind(&user.username).execute(db).await?;
        }
    }
    for incoming in &bundle.deployments {
        let existing = sqlx::query_scalar::<_, String>("SELECT id FROM deployments WHERE name = ?").bind(&incoming.name).fetch_optional(db).await?;
        let webhook = needs_webhook(db, existing.as_deref(), incoming).await?;
        let tokens: Vec<TokenBundle> = missing_tokens(db, existing.as_deref(), incoming).await?.into_iter().cloned().collect();
        let id = match existing {
            Some(id) => {
                let current = deploy::get(db, &id).await?.context("deployment vanished")?;
                let mut merged = secrets.clone();
                for key in &incoming.secrets {
                    let slot = secret_key(&incoming.name, key);
                    if !has_value(incoming, key) && !merged.contains_key(&slot) {
                        let kept = if key == GIT_TOKEN {
                            match &current.spec.source {
                                Source::Git { token, .. } => token.clone(),
                                _ => None,
                            }
                        } else {
                            current.spec.container.env.get(key).cloned()
                        };
                        if let Some(value) = kept {
                            merged.insert(slot, value);
                        }
                    }
                }
                let mut spec = incoming.spec(&merged, ref_ids(db, incoming).await?);
                source_identities(db, &mut spec.source, false).await?;
                spec.clear_referenced_values();
                deploy::update_spec(db, &id, &spec).await?;
                id
            }
            None => {
                let mut spec = incoming.spec(secrets, ref_ids(db, incoming).await?);
                spec.clear_referenced_values();
                source_identities(db, &mut spec.source, false).await?;
                deploy::create(db, &incoming.name, &spec, actor).await?.id
            }
        };
        sqlx::query("DELETE FROM domains WHERE deployment_id = ?").bind(&id).execute(db).await?;
        for domain in &incoming.domains {
            let created = proxy::create_domain(db, &id, &domain.hostname, &domain.service, domain.port, domain.tls).await?;
            let pin = identity_ref(db, domain.certificate.as_deref(), false).await?;
            if pin.is_some() {
                proxy::set_domain_certificate(db, &created.id, pin.as_deref()).await?;
            }
            if !domain.options.is_empty() {
                proxy::set_domain_target(db, &created.id, &domain.service, domain.port, &domain.options).await?;
            }
        }
        if webhook {
            let secret = deploy::new_webhook_secret(db, &id).await?;
            applied.webhooks.push(NewWebhook { deployment: incoming.name.clone(), deployment_id: id.clone(), secret });
        }
        for token in tokens {
            let host_mounts = token.host_mounts && actor_host_mounts;
            let (_, value) = deploy::tokens::create(db, &id, &token.name, host_mounts, &actor.id).await?;
            applied.tokens.push(NewToken { deployment: incoming.name.clone(), deployment_id: id.clone(), name: token.name, token: value });
        }
        applied.deployments.push(incoming.name.clone());
    }
    if let Some(server) = &bundle.server {
        for role in &server.roles {
            sqlx::query(
                "INSERT INTO roles (name, builtin, description, permissions, scope) VALUES (?, 0, ?, ?, ?)
                 ON CONFLICT (name) DO UPDATE SET description = excluded.description, permissions = excluded.permissions, scope = excluded.scope",
            )
            .bind(&role.name)
            .bind(&role.description)
            .bind(serde_json::to_string(&role.permissions)?)
            .bind(scope_ids(db, &role.scope).await?.to_db())
            .execute(db)
            .await?;
        }
        for user in &server.users {
            let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE username = ?").bind(&user.username).fetch_one(db).await?;
            sqlx::query("DELETE FROM deployment_grants WHERE user_id = ?").bind(&user_id).execute(db).await?;
            for grant in &user.grants {
                sqlx::query(
                    "INSERT OR IGNORE INTO deployment_grants (deployment_id, user_id, role) SELECT id, ?, ? FROM deployments WHERE name = ?",
                )
                .bind(&user_id)
                .bind(&grant.role)
                .bind(&grant.deployment)
                .execute(db)
                .await?;
            }
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::deploy::spec::{PortMapping, Protocol};
    use crate::docker::rewrite::SharedMount;

    async fn seed(db: &SqlitePool) -> User {
        let owner = auth::create_user(db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let dev = auth::create_user(db, "dev", "dev-password-12", Role::deployer()).await.unwrap();
        sqlx::query("UPDATE users SET host_mounts = 1 WHERE id = ?").bind(&dev.id).execute(db).await.unwrap();
        for (key, value) in [("pool_memory_mb", "8192"), ("proxy_mode", "managed"), ("public_url", "https://kuroe.example.com")] {
            sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?)").bind(key).bind(value).execute(db).await.unwrap();
        }
        sqlx::query("INSERT INTO volumes (name, created_at) VALUES ('uploads', 0)").execute(db).await.unwrap();

        let meta = serde_json::json!({});
        let login = identity::NewIdentity {
            id: None,
            kind: identity::Kind::Registry,
            name: "ghcr",
            host: "ghcr.io",
            username: "acme",
            secret: "regpass-123",
            meta: &meta,
            expires_at: None,
        };
        let login = identity::insert(db, &login).await.unwrap();
        let forge = identity::NewIdentity {
            id: None,
            kind: identity::Kind::Gitea,
            name: "forge",
            host: "git.example.com",
            username: "",
            secret: "forge-token",
            meta: &meta,
            expires_at: None,
        };
        let forge = identity::insert(db, &forge).await.unwrap();
        let pool = runners::PoolInput {
            name: "builds".into(),
            provider: runners::Provider::Gitea,
            identity: forge,
            scope_kind: runners::ScopeKind::Org,
            scope: "acme".into(),
            labels: vec!["big".into()],
            min_idle: 0,
            max: 3,
            cpus: Some(2.0),
            memory_mb: Some(4096),
            net_in_mbit: Some(50.0),
            net_out_mbit: Some(10.0),
            docker_mode: runners::DockerMode::Dind,
            image: None,
            enabled: true,
        };
        runners::insert(db, pool, identity::Kind::Gitea, &owner.id).await.unwrap().unwrap();
        let mut api = Spec::new(Source::Image { image: "ghcr.io/acme/api:v3".into(), registry_identity: Some(login) });
        api.container.ports = vec![PortMapping { container: 8080, host: None, protocol: Protocol::Tcp }];
        api.container.env = BTreeMap::from([("LOG".into(), "info".into()), ("DB_PASSWORD".into(), "hunter2".into())]);
        api.secrets = BTreeSet::from(["DB_PASSWORD".into()]);
        api.services.insert(
            "app".into(),
            ServiceSettings {
                public: false,
                cpus: Some(0.5),
                memory_mb: Some(256),
                pids: None,
                net_in_mbit: Some(20.0),
                net_out_mbit: Some(5.5),
                net_conn_out_mbit: Some(1.0),
                volumes: vec![SharedMount { volume: "uploads".into(), target: "/data".into(), read_only: false }],
            },
        );
        let api = deploy::create(db, "api", &api, &owner).await.unwrap();
        proxy::create_domain(db, &api.id, "api.example.com", "app", 8080, true).await.unwrap();
        let routed = proxy::create_domain(db, &api.id, "www.example.com", "app", 8080, true).await.unwrap();
        let page = |body: &str, json: bool| Some(proxy::options::ErrorPage { body: body.into(), json });
        let routing = RouteOptions {
            compress: true,
            paths: vec![proxy::options::PathRule {
                path: "/api/".into(),
                exact: false,
                service: "app".into(),
                port: 9000,
                error_page: page("{\"error\":\"down\"}", true),
            }],
            error_page: page("<h1>Back soon</h1>", false),
            ..Default::default()
        };
        proxy::set_domain_target(db, &routed.id, "app", 8080, &routing).await.unwrap();
        let old = proxy::create_domain(db, &api.id, "old.example.com", "", 0, true).await.unwrap();
        let redirect = RouteOptions { redirect: Some("https://www.example.com".into()), ..Default::default() };
        proxy::set_domain_target(db, &old.id, "", 0, &redirect).await.unwrap();

        let site = Spec::new(Source::Git {
            url: "https://github.com/acme/site.git".into(),
            reference: Some("main".into()),
            context: None,
            dockerfile: None,
            token: Some("ghp_secret".into()),
            identity: None,
            compose_file: None,
        });
        let site = deploy::create(db, "site", &site, &owner).await.unwrap();
        let mono = Spec::new(Source::Git {
            url: "https://github.com/acme/mono.git".into(),
            reference: None,
            context: None,
            dockerfile: None,
            token: None,
            identity: None,
            compose_file: Some("deploy/compose.yaml".into()),
        });
        deploy::create(db, "mono", &mono, &owner).await.unwrap();
        let mut docs = Spec::new(Source::Static {});
        docs.site = Some(SiteOptions {
            build: crate::deploy::spec::SiteBuild::Command { image: "node:26-alpine".into(), command: "npm ci && npm run build".into() },
            output: "dist".into(),
            spa: true,
        });
        deploy::create(db, "docs", &docs, &owner).await.unwrap();
        sqlx::query("INSERT INTO roles (name, description, permissions, scope) VALUES ('frontend', 'Ships the site', ?, ?)")
            .bind(r#"["dep.view","dep.deploy"]"#)
            .bind(format!(r#"["{}"]"#, site.id))
            .execute(db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO deployment_grants (deployment_id, user_id, role) VALUES (?, ?, 'frontend')")
            .bind(&site.id)
            .bind(&dev.id)
            .execute(db)
            .await
            .unwrap();

        let compose = Spec::new(Source::Compose { file: "services:\n  web:\n    image: nginx:alpine\n".into() });
        deploy::create(db, "stack", &compose, &owner).await.unwrap();
        owner
    }

    #[tokio::test]
    async fn bundle_round_trip_is_identical() {
        let a = test_pool().await;
        seed(&a).await;
        let first = to_toml(&export_server(&a, false).await.unwrap()).unwrap();
        assert!(!first.contains("hunter2") && !first.contains("ghp_secret") && !first.contains("regpass"), "secrets leaked:\n{first}");
        assert!(first.contains(r#"registry_identity = "ghcr""#), "sources name their login:\n{first}");
        assert!(first.contains(r#"compose_file = "deploy/compose.yaml""#), "git sources keep their Compose file:\n{first}");
        assert!(first.contains(r#"command = "npm ci && npm run build""#), "static sites keep their build step:\n{first}");
        assert!(first.contains(r#"redirect = "https://www.example.com""#), "domains keep their redirect:\n{first}");
        assert!(first.contains("Back soon"), "domains keep their error pages:\n{first}");

        let b = test_pool().await;
        let owner_b = auth::create_user(&b, "boss", "other-password-1", Role::owner()).await.unwrap();
        let bundle = from_toml(&first).unwrap();
        let plan = plan(&b, &bundle).await.unwrap();
        let mut prompts: Vec<&str> = plan.secrets.iter().map(|s| s.key.as_str()).collect();
        prompts.sort();
        assert_eq!(prompts, ["@identity/forge", "@identity/ghcr", "@user/dev", "api/DB_PASSWORD", "site/@git_token"]);
        assert_eq!(plan.runner_pools[0].action, "create");

        let secrets = BTreeMap::from([
            ("@identity/forge".to_owned(), "forge-token".to_owned()),
            ("@identity/ghcr".to_owned(), "regpass-123".to_owned()),
            ("@user/dev".to_owned(), "new-dev-password".to_owned()),
            ("api/DB_PASSWORD".to_owned(), "hunter2".to_owned()),
            ("site/@git_token".to_owned(), "ghp_secret".to_owned()),
        ]);
        let applied = apply(&b, &bundle, &secrets, &owner_b).await.unwrap();
        assert_eq!(applied.volumes_to_create, ["uploads"]);

        let second = to_toml(&export_server(&b, false).await.unwrap()).unwrap();
        assert_eq!(first, second);
        assert!(first.contains(r#"scope = ["site"]"#), "scopes name deployments:\n{first}");
        assert!(first.contains(r#"deployment = "site""#) && first.contains(r#"role = "frontend""#), "{first}");
        assert!(first.contains("[[server.runner_pool]]") && first.contains(r#"identity = "forge""#), "pools name their identity:\n{first}");
        let pool = runners::list(&b).await.unwrap().remove(0);
        assert!(!pool.enabled, "imported pools start switched off");

        let with_secrets = to_toml(&export_server(&b, true).await.unwrap()).unwrap();
        assert!(with_secrets.contains("hunter2") && with_secrets.contains("ghp_secret"));
        assert!(!with_secrets.contains("enc:"), "exports carry plain values, never sealed ones");
        let stored: Vec<String> = sqlx::query_scalar("SELECT spec FROM deployments").fetch_all(&b).await.unwrap();
        let identities: Vec<String> = sqlx::query_scalar("SELECT secret FROM identities").fetch_all(&b).await.unwrap();
        assert!(stored.iter().all(|s| !s.contains("hunter2") && !s.contains("ghp_secret")), "imports are sealed");
        assert!(identities.iter().all(|s| s.starts_with("enc:")));
        assert!(!with_secrets.contains("regpass"), "identity secrets never leave, even with secrets on");
        let imported = identity::by_name(&b, "ghcr").await.unwrap().unwrap();
        assert_eq!((imported.host.as_str(), imported.secret.as_str()), ("ghcr.io", "regpass-123"));
        let api = deploy::list(&b).await.unwrap().into_iter().find(|d| d.name == "api").unwrap();
        assert_eq!(api.spec.source, Source::Image { image: "ghcr.io/acme/api:v3".into(), registry_identity: Some(imported.id) });
    }

    #[tokio::test]
    async fn import_refuses_without_secrets_and_reports_updates() {
        let a = test_pool().await;
        let owner = seed(&a).await;
        let bundle = from_toml(&to_toml(&export_server(&a, false).await.unwrap()).unwrap()).unwrap();

        let plan_same = plan(&a, &bundle).await.unwrap();
        assert!(plan_same.deployments.iter().all(|d| d.action == "unchanged"), "{plan_same:?}");
        assert!(plan_same.secrets.is_empty() && plan_same.settings.is_empty() && plan_same.volumes.is_empty());

        let mut changed = bundle.clone();
        changed.deployments[0].container.ports.clear();
        changed.deployments[0].domains.clear();
        let p = plan(&a, &changed).await.unwrap();
        assert_eq!(p.deployments[0], Change { name: "api".into(), action: "update", fields: vec!["ports".into(), "domains".into()] });

        apply(&a, &changed, &BTreeMap::new(), &owner).await.unwrap();
        let api = deploy::list(&a).await.unwrap().into_iter().find(|d| d.name == "api").unwrap();
        assert_eq!(api.spec.container.env["DB_PASSWORD"], "hunter2", "a secret not in the bundle keeps its old value");

        let fresh = test_pool().await;
        let fresh_owner = auth::create_user(&fresh, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let err = apply(&fresh, &bundle, &BTreeMap::new(), &fresh_owner).await.unwrap_err().to_string();
        assert!(err.starts_with("missing secrets:"), "{err}");
        assert!(deploy::list(&fresh).await.unwrap().is_empty());
    }

    fn import_secrets() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("@identity/forge".to_owned(), "forge-token".to_owned()),
            ("@identity/ghcr".to_owned(), "regpass-123".to_owned()),
            ("@user/dev".to_owned(), "new-dev-password".to_owned()),
            ("api/DB_PASSWORD".to_owned(), "hunter2".to_owned()),
            ("site/@git_token".to_owned(), "ghp_secret".to_owned()),
        ])
    }

    #[tokio::test]
    async fn exports_name_what_they_leave_out_and_import_makes_new_values() {
        let a = test_pool().await;
        let owner = seed(&a).await;
        let site_a: String = sqlx::query_scalar("SELECT id FROM deployments WHERE name = 'site'").fetch_one(&a).await.unwrap();
        let hook = deploy::new_webhook_secret(&a, &site_a).await.unwrap();
        let (_, token) = deploy::tokens::create(&a, &site_a, "github-actions", false, &owner.id).await.unwrap();

        let text = to_toml(&export_server(&a, true).await.unwrap()).unwrap();
        assert!(!text.contains(&hook) && !text.contains(&token), "webhook secrets and tokens never leave:\n{text}");
        assert!(text.starts_with("# Left out of this file."), "{text}");
        for line in [
            "#   site: push webhook secret (a new one is made on import)",
            "#   site: deploy token \"github-actions\" (a new one is made on import)",
            "#   login ghcr: secret",
            "#   user dev: password",
        ] {
            assert!(text.contains(line), "missing {line:?}:\n{text}");
        }
        assert!(!text.contains("DB_PASSWORD (env)"), "values that are in the file aren't listed:\n{text}");
        let without = to_toml(&export_server(&a, false).await.unwrap()).unwrap();
        assert!(without.contains("#   api: DB_PASSWORD (env)") && without.contains("#   site: git access token"), "{without}");

        let bundle = from_toml(&without).unwrap();
        let site = bundle.deployments.iter().find(|d| d.name == "site").unwrap();
        assert!(site.webhook);
        assert_eq!(site.tokens, [TokenBundle { name: "github-actions".into(), host_mounts: false }]);

        let b = test_pool().await;
        let owner_b = auth::create_user(&b, "boss", "other-password-1", Role::owner()).await.unwrap();
        let p = plan(&b, &bundle).await.unwrap();
        assert_eq!(p.webhooks, ["site"]);
        assert_eq!(p.tokens, [TokenPlan { deployment: "site".into(), name: "github-actions".into() }]);

        let applied = apply(&b, &bundle, &import_secrets(), &owner_b).await.unwrap();
        let site_b: String = sqlx::query_scalar("SELECT id FROM deployments WHERE name = 'site'").fetch_one(&b).await.unwrap();
        assert_eq!(applied.webhooks.len(), 1);
        assert_eq!((applied.webhooks[0].deployment.as_str(), applied.webhooks[0].deployment_id.as_str()), ("site", site_b.as_str()));
        assert_ne!(applied.webhooks[0].secret, hook, "a fresh secret");
        let stored: String = sqlx::query_scalar("SELECT webhook_secret FROM deployments WHERE id = ?").bind(&site_b).fetch_one(&b).await.unwrap();
        assert!(stored.starts_with("enc:"), "stored sealed");
        let grant = deploy::tokens::authenticate(&b, &applied.tokens[0].token).await.unwrap().expect("the new token works");
        assert_eq!(grant.deployment_id, site_b);

        let again = plan(&b, &bundle).await.unwrap();
        assert!(again.webhooks.is_empty() && again.tokens.is_empty(), "{again:?}");
        apply(&b, &bundle, &BTreeMap::new(), &owner_b).await.unwrap();
        assert_eq!(deploy::tokens::list(&b, &site_b).await.unwrap().len(), 1, "a same-named token is left alone");
    }

    #[tokio::test]
    async fn saved_secrets_travel_by_name_and_are_asked_for_once() {
        let a = test_pool().await;
        let owner = seed(&a).await;
        let shared = saved_secrets::insert(&a, "db-password", "Postgres", "hunter2-shared", &owner.id).await.unwrap();
        for name in ["api", "site"] {
            let d = deploy::list(&a).await.unwrap().into_iter().find(|d| d.name == name).unwrap();
            let mut spec = d.spec.clone();
            spec.container.env.insert("SHARED_DB".into(), String::new());
            spec.secret_refs.insert("SHARED_DB".into(), shared.id.clone());
            deploy::update_spec(&a, &d.id, &spec).await.unwrap();
        }

        let text = to_toml(&export_server(&a, true).await.unwrap()).unwrap();
        assert!(!text.contains("hunter2-shared"), "saved values never leave, even with secrets on:\n{text}");
        assert!(text.contains("#   saved secret db-password: value") && text.contains(r#"SHARED_DB = "db-password""#), "{text}");

        let b = test_pool().await;
        let owner_b = auth::create_user(&b, "boss", "other-password-1", Role::owner()).await.unwrap();
        let bundle = from_toml(&text).unwrap();
        let p = plan(&b, &bundle).await.unwrap();
        assert_eq!(p.secrets.iter().filter(|s| s.key == "@secret/db-password").count(), 1, "asked once for both deployments");
        assert_eq!(p.saved_secrets, [Change { name: "db-password".into(), action: "create", fields: vec![] }]);

        let mut given = import_secrets();
        given.insert("@secret/db-password".into(), "hunter2-shared".into());
        apply(&b, &bundle, &given, &owner_b).await.unwrap();
        let created = saved_secrets::by_name(&b, "db-password").await.unwrap().unwrap();
        assert_eq!(created.description, "Postgres");
        for d in deploy::list(&b).await.unwrap().into_iter().filter(|d| d.name == "api" || d.name == "site") {
            assert_eq!(d.spec.secret_refs["SHARED_DB"], created.id, "{} points at the new secret", d.name);
            let resolved = saved_secrets::resolve(&b, &d.spec).await.unwrap();
            assert_eq!(resolved.container.env["SHARED_DB"], "hunter2-shared");
        }
        assert!(plan(&b, &bundle).await.unwrap().secrets.is_empty(), "nothing to ask the second time");
    }

    #[tokio::test]
    async fn updating_a_deployment_asks_for_a_secret_it_doesnt_have() {
        let a = test_pool().await;
        let owner = seed(&a).await;
        let mut bundle = from_toml(&to_toml(&export_server(&a, false).await.unwrap()).unwrap()).unwrap();
        let api = bundle.deployments.iter_mut().find(|d| d.name == "api").unwrap();
        api.secrets.insert("API_KEY".into());
        api.container.env.insert("API_KEY".into(), String::new());

        let p = plan(&a, &bundle).await.unwrap();
        let prompts: Vec<&str> = p.secrets.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(prompts, ["api/API_KEY"], "DB_PASSWORD is already on the target");
        let err = apply(&a, &bundle, &BTreeMap::new(), &owner).await.unwrap_err().to_string();
        assert!(err.contains("api: API_KEY"), "{err}");

        let given = BTreeMap::from([("api/API_KEY".to_owned(), "k-123".to_owned())]);
        apply(&a, &bundle, &given, &owner).await.unwrap();
        let api = deploy::list(&a).await.unwrap().into_iter().find(|d| d.name == "api").unwrap();
        assert_eq!((api.spec.container.env["API_KEY"].as_str(), api.spec.container.env["DB_PASSWORD"].as_str()), ("k-123", "hunter2"));
    }

    #[tokio::test]
    async fn a_lone_deployment_needs_its_login_on_the_target() {
        let db = test_pool().await;
        let text = "format = 1\n[[deployment]]\nname = \"api\"\nsource = { kind = \"image\", image = \"ghcr.io/a/b\", registry_identity = \"ghcr\" }\n";
        let err = plan(&db, &from_toml(text).unwrap()).await.unwrap_err().to_string();
        assert!(err.contains("api uses the login ghcr"), "{err}");
    }

    #[tokio::test]
    async fn import_refuses_a_role_it_doesnt_know() {
        let db = test_pool().await;
        let text = "format = 1\n[server]\n[[server.user]]\nusername = \"ana\"\nrole = \"ghost\"\n";
        let err = plan(&db, &from_toml(text).unwrap()).await.unwrap_err().to_string();
        assert!(err.contains("ana uses the role ghost"), "{err}");
    }

    #[test]
    fn rejects_other_formats_and_duplicate_names() {
        assert!(from_toml("format = 2").unwrap_err().to_string().contains("made by a newer kuroe"));
        assert!(from_toml("format = 0").unwrap_err().to_string().contains("not supported"));
        let dup = "format = 1\n[[deployment]]\nname = \"a\"\nsource = { kind = \"image\", image = \"x\" }\n[[deployment]]\nname = \"a\"\nsource = { kind = \"image\", image = \"y\" }\n";
        assert!(from_toml(dup).unwrap_err().to_string().contains("twice"));
    }
}
