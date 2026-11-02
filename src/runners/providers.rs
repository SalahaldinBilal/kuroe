//! Registering runners with GitHub, GitLab and Gitea or Forgejo, and removing them again.

use std::collections::HashMap;

use anyhow::{Context, bail};
use reqwest::{Client, RequestBuilder, Response};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::{DockerMode, Pool, Provider, ScopeKind};
use crate::identity::{self, Identity};

/// How a runner container is started once the provider knows about it.
#[derive(Debug, Default, PartialEq)]
pub struct Registration {
    /// The provider's id for the runner, used to remove it later.
    pub remote_id: Option<String>,
    pub entrypoint: Option<Vec<String>>,
    pub cmd: Option<Vec<String>>,
    pub env: Vec<String>,
}

async fn ok(response: Response, what: &str) -> anyhow::Result<Value> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("{what} failed ({status}): {}", body.chars().take(300).collect::<String>());
    }
    Ok(response.json().await.unwrap_or(Value::Null))
}

pub fn encode(path: &str) -> String {
    path.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// The labels runners register with: `kuroe`, the pool name, then the pool's own.
pub fn labels(pool: &Pool) -> Vec<String> {
    let mut out = vec!["kuroe".to_owned(), pool.name.clone()];
    for label in &pool.labels {
        if !out.contains(label) {
            out.push(label.clone());
        }
    }
    out
}

// GitHub

fn github_base(identity: &Identity) -> String {
    match identity::host_of(identity).as_str() {
        "" | "github.com" => "https://api.github.com".into(),
        host => format!("https://{host}/api/v3"),
    }
}

fn github_scope(pool: &Pool) -> String {
    match pool.scope_kind {
        ScopeKind::Org => format!("orgs/{}", pool.scope),
        _ => format!("repos/{}", pool.scope),
    }
}

async fn github_token(db: &SqlitePool, http: &Client, identity: &Identity) -> anyhow::Result<String> {
    Ok(identity::git_credentials(db, http, identity).await?.1)
}

fn github(request: RequestBuilder, token: &str) -> RequestBuilder {
    request
        .bearer_auth(token)
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .header("user-agent", "kuroe")
}

pub fn jit_body(name: &str, pool: &Pool) -> Value {
    json!({ "name": name, "runner_group_id": 1, "labels": labels(pool), "work_folder": "_work" })
}

/// A just-in-time config: the runner takes one job with it and removes itself afterwards.
pub fn jit_registration(response: &Value) -> anyhow::Result<Registration> {
    let config = response["encoded_jit_config"].as_str().context("GitHub sent no encoded_jit_config")?;
    let id = response["runner"]["id"].as_i64().context("GitHub sent no runner id")?;
    Ok(Registration {
        remote_id: Some(id.to_string()),
        entrypoint: None,
        cmd: Some(vec!["./run.sh".into(), "--jitconfig".into(), config.into()]),
        env: Vec::new(),
    })
}

// GitLab

fn gitlab(request: RequestBuilder, identity: &Identity) -> RequestBuilder {
    request.header("private-token", &identity.secret)
}

pub fn gitlab_runner_body(pool: &Pool, name: &str, target_id: Option<i64>) -> Value {
    let mut body = json!({ "description": name, "tag_list": labels(pool).join(","), "run_untagged": false });
    match pool.scope_kind {
        ScopeKind::Repo => {
            body["runner_type"] = json!("project_type");
            body["project_id"] = json!(target_id);
        }
        ScopeKind::Org => {
            body["runner_type"] = json!("group_type");
            body["group_id"] = json!(target_id);
        }
        ScopeKind::Instance => body["runner_type"] = json!("instance_type"),
    }
    body
}

/// Registers once per container, so a restart doesn't add the runner to the config twice.
pub const GITLAB_START: &str = r#"set -e
if ! grep -q "$KUROE_NAME" /etc/gitlab-runner/config.toml 2>/dev/null; then
  gitlab-runner register --non-interactive --url "$KUROE_URL" --token "$KUROE_TOKEN" --name "$KUROE_NAME" \
    --executor "$KUROE_EXECUTOR" --docker-image alpine:3 ${DOCKER_HOST:+--docker-host "$DOCKER_HOST"} \
    ${KUROE_PRIVILEGED:+--docker-privileged} ${KUROE_SOCKET:+--docker-volumes /var/run/docker.sock:/var/run/docker.sock}
fi
exec gitlab-runner run --working-directory /home/gitlab-runner"#;

pub fn gitlab_registration(pool: &Pool, base: &str, name: &str, response: &Value) -> anyhow::Result<Registration> {
    let token = response["token"].as_str().context("GitLab sent no runner token")?;
    let id = response["id"].as_i64().context("GitLab sent no runner id")?;
    let executor = if pool.docker_mode == DockerMode::None { "shell" } else { "docker" };
    let mut env = vec![format!("KUROE_URL={base}"), format!("KUROE_TOKEN={token}"), format!("KUROE_NAME={name}"), format!("KUROE_EXECUTOR={executor}")];
    match pool.docker_mode {
        // Job containers run inside the runner's own Docker, so docker:dind services there need privileged.
        DockerMode::Dind => env.push("KUROE_PRIVILEGED=1".into()),
        DockerMode::Host => env.push("KUROE_SOCKET=1".into()),
        DockerMode::None => {}
    }
    Ok(Registration {
        remote_id: Some(id.to_string()),
        entrypoint: Some(vec!["/bin/sh".into(), "-c".into()]),
        cmd: Some(vec![GITLAB_START.into()]),
        env,
    })
}

// Gitea and Forgejo

fn gitea(request: RequestBuilder, identity: &Identity) -> RequestBuilder {
    request.header("authorization", format!("token {}", identity.secret))
}

fn gitea_token_path(pool: &Pool) -> String {
    match pool.scope_kind {
        ScopeKind::Repo => format!("/api/v1/repos/{}/actions/runners/registration-token", pool.scope),
        ScopeKind::Org => format!("/api/v1/orgs/{}/actions/runners/registration-token", pool.scope),
        ScopeKind::Instance => "/api/v1/admin/runners/registration-token".into(),
    }
}

/// act_runner labels say where jobs run: a container image when there is Docker, the runner itself otherwise.
pub fn act_labels(pool: &Pool) -> Vec<String> {
    labels(pool)
        .into_iter()
        .map(|label| {
            if label.contains(':') {
                label
            } else if pool.docker_mode == DockerMode::None {
                format!("{label}:host")
            } else {
                format!("{label}:docker://node:20-bookworm")
            }
        })
        .collect()
}

pub fn gitea_registration(pool: &Pool, base: &str, name: &str, token: &str) -> Registration {
    Registration {
        remote_id: None,
        entrypoint: None,
        cmd: None,
        env: vec![
            format!("GITEA_INSTANCE_URL={base}"),
            format!("GITEA_RUNNER_REGISTRATION_TOKEN={token}"),
            format!("GITEA_RUNNER_NAME={name}"),
            format!("GITEA_RUNNER_LABELS={}", act_labels(pool).join(",")),
        ],
    }
}

/// Tells the provider about a new runner called `name`.
pub async fn register(db: &SqlitePool, http: &Client, pool: &Pool, identity: &Identity, name: &str) -> anyhow::Result<Registration> {
    match pool.provider {
        Provider::Github => {
            let token = github_token(db, http, identity).await?;
            let url = format!("{}/{}/actions/runners/generate-jitconfig", github_base(identity), github_scope(pool));
            let response = github(http.post(url), &token).json(&jit_body(name, pool)).send().await?;
            jit_registration(&ok(response, "registering the runner with GitHub").await?)
        }
        Provider::Gitlab => {
            let base = identity::api_base(identity);
            let target = gitlab_target(http, identity, pool).await?.map(|(id, _)| id);
            let response = gitlab(http.post(format!("{base}/api/v4/user/runners")), identity)
                .json(&gitlab_runner_body(pool, name, target))
                .send()
                .await?;
            gitlab_registration(pool, &base, name, &ok(response, "creating the runner in GitLab").await?)
        }
        Provider::Gitea => {
            let base = identity::api_base(identity);
            let url = format!("{base}{}", gitea_token_path(pool));
            let mut response = gitea(http.get(&url), identity).send().await?;
            if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
                response = gitea(http.post(&url), identity).send().await?;
            }
            let body = ok(response, "getting a runner registration token").await?;
            let token = body["token"].as_str().context("no registration token in the answer")?;
            Ok(gitea_registration(pool, &base, name, token))
        }
    }
}

/// The project or group id and whether it's public, for GitLab pools scoped to one.
async fn gitlab_target(http: &Client, identity: &Identity, pool: &Pool) -> anyhow::Result<Option<(i64, bool)>> {
    let kind = match pool.scope_kind {
        ScopeKind::Repo => "projects",
        ScopeKind::Org => "groups",
        ScopeKind::Instance => return Ok(None),
    };
    let url = format!("{}/api/v4/{kind}/{}", identity::api_base(identity), encode(&pool.scope));
    let body = ok(gitlab(http.get(url), identity).send().await?, &format!("looking up {}", pool.scope)).await?;
    let id = body["id"].as_i64().context("GitLab sent no id")?;
    Ok(Some((id, body["visibility"].as_str() == Some("public"))))
}

/// Whether the repository is public. `None` when that can't be told from one repository, like an organization.
pub async fn is_public(db: &SqlitePool, http: &Client, pool: &Pool, identity: &Identity) -> anyhow::Result<Option<bool>> {
    match (pool.provider, pool.scope_kind) {
        (Provider::Github, ScopeKind::Repo) => {
            let token = github_token(db, http, identity).await?;
            let url = format!("{}/repos/{}", github_base(identity), pool.scope);
            let body = ok(github(http.get(url), &token).send().await?, &format!("looking up {}", pool.scope)).await?;
            Ok(Some(body["private"].as_bool() == Some(false)))
        }
        (Provider::Gitlab, ScopeKind::Repo) => Ok(gitlab_target(http, identity, pool).await?.map(|(_, public)| public)),
        (Provider::Gitea, ScopeKind::Repo) => {
            let url = format!("{}/api/v1/repos/{}", identity::api_base(identity), pool.scope);
            let body = ok(gitea(http.get(url), identity).send().await?, &format!("looking up {}", pool.scope)).await?;
            Ok(Some(body["private"].as_bool() == Some(false)))
        }
        _ => Ok(None),
    }
}

/// Which of the pool's GitHub runners are running a job, by runner id.
pub async fn busy(db: &SqlitePool, http: &Client, pool: &Pool, identity: &Identity) -> anyhow::Result<HashMap<String, bool>> {
    if pool.provider != Provider::Github {
        return Ok(HashMap::new());
    }
    let token = github_token(db, http, identity).await?;
    let url = format!("{}/{}/actions/runners?per_page=100", github_base(identity), github_scope(pool));
    let body = ok(github(http.get(url), &token).send().await?, "listing runners on GitHub").await?;
    Ok(body["runners"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| Some((r["id"].as_i64()?.to_string(), r["busy"].as_bool().unwrap_or(false))))
        .collect())
}

/// Removes a runner at the provider. A runner that is already gone counts as removed.
pub async fn deregister(db: &SqlitePool, http: &Client, pool: &Pool, identity: &Identity, remote_id: &str) -> anyhow::Result<()> {
    let response = match pool.provider {
        Provider::Github => {
            let token = github_token(db, http, identity).await?;
            let url = format!("{}/{}/actions/runners/{remote_id}", github_base(identity), github_scope(pool));
            github(http.delete(url), &token).send().await?
        }
        Provider::Gitlab => {
            let url = format!("{}/api/v4/runners/{remote_id}", identity::api_base(identity));
            gitlab(http.delete(url), identity).send().await?
        }
        Provider::Gitea => bail!("Gitea and Forgejo runners have to be removed under Actions, Runners"),
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(());
    }
    ok(response, "removing the runner").await.map(|_| ())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn pool(provider: Provider, scope_kind: ScopeKind, docker_mode: DockerMode) -> Pool {
        Pool {
            id: "p1".into(),
            name: "ci".into(),
            provider,
            identity: "i1".into(),
            scope_kind,
            scope: "acme/api".into(),
            labels: vec!["big".into(), "kuroe".into()],
            min_idle: 1,
            max: 2,
            cpus: None,
            memory_mb: None,
            net_in_mbit: None,
            net_out_mbit: None,
            docker_mode,
            image: None,
            enabled: true,
            last_error: None,
            created_by: None,
            created_at: 0,
        }
    }

    #[test]
    fn github_jit_config_runs_one_job() {
        let p = pool(Provider::Github, ScopeKind::Repo, DockerMode::Dind);
        assert_eq!(
            jit_body("kuroe_runner_ci_ab12cd", &p),
            json!({ "name": "kuroe_runner_ci_ab12cd", "runner_group_id": 1, "labels": ["kuroe", "ci", "big"], "work_folder": "_work" })
        );
        let response = json!({ "runner": { "id": 23, "name": "kuroe_runner_ci_ab12cd" }, "encoded_jit_config": "ZXhhbXBsZQ==" });
        let reg = jit_registration(&response).unwrap();
        assert_eq!(reg.remote_id.as_deref(), Some("23"));
        assert_eq!(reg.cmd.unwrap(), ["./run.sh", "--jitconfig", "ZXhhbXBsZQ=="]);
        assert!(jit_registration(&json!({})).is_err());
        assert_eq!(github_scope(&pool(Provider::Github, ScopeKind::Org, DockerMode::Dind)), "orgs/acme/api");
    }

    #[test]
    fn gitlab_runner_body_and_start() {
        let project = pool(Provider::Gitlab, ScopeKind::Repo, DockerMode::Dind);
        assert_eq!(
            gitlab_runner_body(&project, "r1", Some(42)),
            json!({ "description": "r1", "tag_list": "kuroe,ci,big", "run_untagged": false, "runner_type": "project_type", "project_id": 42 })
        );
        let instance = pool(Provider::Gitlab, ScopeKind::Instance, DockerMode::None);
        assert_eq!(gitlab_runner_body(&instance, "r1", None)["runner_type"], "instance_type");
        let reg = gitlab_registration(&instance, "https://gitlab.com", "r1", &json!({ "id": 9, "token": "glrt-abc" })).unwrap();
        assert_eq!(reg.remote_id.as_deref(), Some("9"));
        assert!(reg.env.contains(&"KUROE_TOKEN=glrt-abc".to_owned()));
        assert!(reg.env.contains(&"KUROE_EXECUTOR=shell".to_owned()), "no Docker means the shell executor");
        assert!(!reg.env.iter().any(|e| e.starts_with("KUROE_PRIVILEGED") || e.starts_with("KUROE_SOCKET")));
        let dind = gitlab_registration(&project, "https://gitlab.com", "r1", &json!({ "id": 9, "token": "t" })).unwrap();
        assert!(dind.env.contains(&"KUROE_PRIVILEGED=1".to_owned()));
        assert_eq!(encode("acme/web app"), "acme%2Fweb%20app");
    }

    #[test]
    fn act_runner_labels_say_where_jobs_run() {
        let docker = pool(Provider::Gitea, ScopeKind::Org, DockerMode::Dind);
        assert_eq!(act_labels(&docker)[0], "kuroe:docker://node:20-bookworm");
        let host = pool(Provider::Gitea, ScopeKind::Org, DockerMode::None);
        assert_eq!(act_labels(&host), ["kuroe:host", "ci:host", "big:host"]);
        let reg = gitea_registration(&docker, "https://git.example.com", "r1", "tok");
        assert!(reg.env.contains(&"GITEA_RUNNER_REGISTRATION_TOKEN=tok".to_owned()));
        assert_eq!(gitea_token_path(&docker), "/api/v1/orgs/acme/api/actions/runners/registration-token");
    }
}
