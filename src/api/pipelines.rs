//! CI pipelines kuroe hands out: GitHub Actions, GitLab CI, Gitea and Forgejo Actions, and a plain shell script.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use super::{API_BASE, AppState, access};
use crate::auth::Permission;
use crate::deploy::Source;
use crate::deploy::spec::image_repository;
use crate::runners::{self, DockerMode, Provider};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Github,
    Gitlab,
    Gitea,
    Forgejo,
    Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunOn {
    /// The platform's own runners.
    #[default]
    Hosted,
    /// A runner pool on this server.
    Here,
}

/// What the pipeline does for this kind of deployment.
#[derive(Debug, Clone, PartialEq)]
pub enum What {
    /// Build the image, push it (or keep it on this server), deploy that tag.
    Image { repository: String },
    /// kuroe pulls and builds itself, the pipeline only starts the deploy.
    Pull,
    /// Send the source as an archive to a static site, which builds it.
    Site,
}

pub struct Job<'a> {
    pub url: &'a str,
    pub deployment_id: &'a str,
    pub what: What,
    pub run_on: RunOn,
    /// Build into this server's Docker instead of pushing, because the runner has the host's socket.
    pub local_build: bool,
    pub branch: Option<&'a str>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Pipeline {
    pub platform: Platform,
    /// Where to save it in the repository, empty for the shell script.
    pub path: String,
    pub body: String,
    /// Secrets or CI variables the pipeline expects.
    pub secrets: Vec<String>,
    pub notes: Vec<String>,
}

/// The registry host of an image repository, Docker Hub when it has none.
pub fn registry_host(repository: &str) -> &str {
    match repository.split_once('/') {
        Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => first,
        _ => "docker.io",
    }
}

struct Login {
    host: String,
    user: String,
    password: String,
    secrets: Vec<String>,
}

fn login(platform: Platform, repository: &str) -> Login {
    let host = registry_host(repository).to_owned();
    match platform {
        Platform::Github if host == "ghcr.io" => {
            Login { host, user: "${{ github.actor }}".into(), password: "${{ secrets.GITHUB_TOKEN }}".into(), secrets: Vec::new() }
        }
        Platform::Github | Platform::Gitea | Platform::Forgejo => Login {
            host,
            user: "${{ secrets.REGISTRY_USER }}".into(),
            password: "${{ secrets.REGISTRY_PASSWORD }}".into(),
            secrets: vec!["REGISTRY_USER".into(), "REGISTRY_PASSWORD".into()],
        },
        Platform::Gitlab if host == "registry.gitlab.com" => {
            Login { host: "$CI_REGISTRY".into(), user: "$CI_REGISTRY_USER".into(), password: "$CI_REGISTRY_PASSWORD".into(), secrets: Vec::new() }
        }
        Platform::Gitlab | Platform::Shell => Login {
            host,
            user: "$REGISTRY_USER".into(),
            password: "$REGISTRY_PASSWORD".into(),
            secrets: vec!["REGISTRY_USER".into(), "REGISTRY_PASSWORD".into()],
        },
    }
}

pub fn render(platform: Platform, job: &Job) -> Pipeline {
    match platform {
        Platform::Github | Platform::Gitea | Platform::Forgejo => actions(platform, job),
        Platform::Gitlab => gitlab(job),
        Platform::Shell => shell(job),
    }
}

fn actions(platform: Platform, job: &Job) -> Pipeline {
    let deploy_url = format!("{}{API_BASE}/deployments/{}", job.url, job.deployment_id);
    let sha = "${{ github.sha }}";
    let token = "${{ secrets.KUROE_TOKEN }}";
    let mut secrets = vec!["KUROE_TOKEN".to_owned()];
    let mut permissions = String::new();
    let mut steps = String::new();
    match &job.what {
        What::Image { repository } => {
            if job.local_build {
                steps.push_str(&format!("\n      - name: Build on this server\n        run: docker build -t {repository}:{sha} .\n"));
            } else {
                let l = login(platform, repository);
                if platform == Platform::Github && l.secrets.is_empty() {
                    permissions = "\npermissions:\n  contents: read\n  packages: write\n".into();
                }
                secrets.extend(l.secrets);
                steps.push_str(&format!(
                    "\n      - uses: docker/login-action@v3\n        with:\n          registry: {}\n          username: {}\n          password: {}\n\n      - uses: docker/build-push-action@v6\n        with:\n          push: true\n          tags: {repository}:{sha}\n",
                    l.host, l.user, l.password
                ));
            }
            steps.push_str(&format!(
                "\n      - name: Deploy\n        run: |\n          curl --fail-with-body -sS -X POST \\\n            -H \"Authorization: Bearer {token}\" \\\n            -H \"Content-Type: application/json\" \\\n            -d '{{\"tag\":\"{sha}\"}}' \\\n            {deploy_url}/deploy\n"
            ));
        }
        What::Pull => steps.push_str(&format!(
            "\n      - name: Deploy\n        run: |\n          curl --fail-with-body -sS -X POST \\\n            -H \"Authorization: Bearer {token}\" \\\n            {deploy_url}/deploy\n"
        )),
        What::Site => steps.push_str(&format!(
            "\n      - name: Send the source to kuroe\n        run: |\n          git archive --format=tar.gz -o site.tar.gz HEAD\n          curl --fail-with-body -sS -X PUT \\\n            -H \"Authorization: Bearer {token}\" \\\n            --data-binary @site.tar.gz \\\n            \"{deploy_url}/site?deploy=true\"\n"
        )),
    }
    let runs_on = match (job.run_on, platform) {
        (RunOn::Here, Platform::Github) => "[self-hosted, kuroe]",
        (RunOn::Here, _) => "kuroe",
        (RunOn::Hosted, _) => "ubuntu-latest",
    };
    let branch = job.branch.unwrap_or("main");
    let body = format!(
        "name: Deploy to kuroe\n\non:\n  push:\n    branches: [{branch}]\n  workflow_dispatch:\n{permissions}\njobs:\n  deploy:\n    runs-on: {runs_on}\n    steps:\n      - uses: actions/checkout@v5\n{steps}"
    );
    let (path, settings) = match platform {
        Platform::Github => (".github/workflows/kuroe.yml", "Settings, Secrets and variables, Actions"),
        Platform::Gitea => (".gitea/workflows/kuroe.yml", "Settings, Actions, Secrets"),
        _ => (".forgejo/workflows/kuroe.yml", "Settings, Actions, Secrets"),
    };
    let mut notes = vec![format!("Add {} as repository secrets under {settings}.", secrets.join(", "))];
    notes.extend(common_notes(job, branch));
    Pipeline { platform, path: path.into(), body, secrets, notes }
}

fn gitlab(job: &Job) -> Pipeline {
    let deploy_url = format!("{}{API_BASE}/deployments/{}", job.url, job.deployment_id);
    let auth = "-H \"Authorization: Bearer $KUROE_TOKEN\"";
    let mut secrets = vec!["KUROE_TOKEN".to_owned()];
    let mut head = String::new();
    let mut script = Vec::new();
    match &job.what {
        What::Image { repository } => {
            if job.local_build {
                head.push_str("  image: docker:29-cli\n");
            } else {
                let l = login(Platform::Gitlab, repository);
                secrets.extend(l.secrets);
                head.push_str("  image: docker:29-cli\n  services:\n    - docker:29-dind\n  variables:\n    DOCKER_TLS_CERTDIR: \"/certs\"\n");
                script.push(format!("echo \"{}\" | docker login -u \"{}\" --password-stdin {}", l.password, l.user, l.host));
            }
            script.push(format!("docker build -t {repository}:$CI_COMMIT_SHA ."));
            if !job.local_build {
                script.push(format!("docker push {repository}:$CI_COMMIT_SHA"));
            }
            script.push("apk add --no-cache curl".into());
            script.push(format!(
                "curl --fail-with-body -sS -X POST {auth} -H \"Content-Type: application/json\" -d \"{{\\\"tag\\\":\\\"$CI_COMMIT_SHA\\\"}}\" {deploy_url}/deploy"
            ));
        }
        What::Pull => {
            head.push_str("  image: alpine:3\n");
            script.push("apk add --no-cache curl".into());
            script.push(format!("curl --fail-with-body -sS -X POST {auth} {deploy_url}/deploy"));
        }
        What::Site => {
            head.push_str("  image: alpine:3\n");
            script.push("apk add --no-cache curl git".into());
            script.push("git archive --format=tar.gz -o site.tar.gz HEAD".into());
            script.push(format!("curl --fail-with-body -sS -X PUT {auth} --data-binary @site.tar.gz \"{deploy_url}/site?deploy=true\""));
        }
    }
    if job.run_on == RunOn::Here {
        head.push_str("  tags:\n    - kuroe\n");
    }
    let rule = match job.branch {
        Some(branch) => format!("$CI_COMMIT_BRANCH == \"{branch}\""),
        None => "$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH".into(),
    };
    let lines: String = script.iter().map(|line| format!("    - '{}'\n", line.replace('\'', "''"))).collect();
    let body = format!("deploy:\n  stage: deploy\n{head}  rules:\n    - if: {rule}\n  script:\n{lines}");
    let mut notes = vec![format!("Add {} as masked CI/CD variables under Settings, CI/CD, Variables.", secrets.join(", "))];
    notes.extend(common_notes(job, job.branch.unwrap_or("the default branch")));
    Pipeline { platform: Platform::Gitlab, path: ".gitlab-ci.yml".into(), body, secrets, notes }
}

fn shell(job: &Job) -> Pipeline {
    let deploy_url = format!("{}{API_BASE}/deployments/{}", job.url, job.deployment_id);
    let auth = "-H \"Authorization: Bearer $KUROE_TOKEN\"";
    let mut secrets = vec!["KUROE_TOKEN".to_owned()];
    let mut lines = vec!["#!/bin/sh".to_owned(), "set -eu".to_owned()];
    match &job.what {
        What::Image { repository } => {
            let l = login(Platform::Shell, repository);
            secrets.extend(l.secrets);
            lines.push("SHA=$(git rev-parse HEAD)".into());
            lines.push(format!("echo \"{}\" | docker login -u \"{}\" --password-stdin {}", l.password, l.user, l.host));
            lines.push(format!("docker build -t {repository}:$SHA ."));
            lines.push(format!("docker push {repository}:$SHA"));
            lines.push(format!(
                "curl --fail-with-body -sS -X POST {auth} -H \"Content-Type: application/json\" -d \"{{\\\"tag\\\":\\\"$SHA\\\"}}\" {deploy_url}/deploy"
            ));
        }
        What::Pull => lines.push(format!("curl --fail-with-body -sS -X POST {auth} {deploy_url}/deploy")),
        What::Site => {
            lines.push("git archive --format=tar.gz -o site.tar.gz HEAD".into());
            lines.push(format!("curl --fail-with-body -sS -X PUT {auth} --data-binary @site.tar.gz \"{deploy_url}/site?deploy=true\""));
        }
    }
    let notes = vec![format!("Run it from the repository in any CI, with {} in the environment.", secrets.join(", "))];
    Pipeline { platform: Platform::Shell, path: String::new(), body: lines.join("\n") + "\n", secrets, notes }
}

fn common_notes(job: &Job, branch: &str) -> Vec<String> {
    let mut notes = vec![format!("It runs on pushes to {branch}. Change the branch in the file if you deploy from another one.")];
    if job.run_on == RunOn::Here {
        notes.push("It runs on a runner pool on this server. Add one for this repository under Runners.".into());
    }
    if job.local_build {
        notes.push("The runner uses this server's Docker, so the image is built here and never pushed to a registry.".into());
    }
    notes
}

fn provider_of(platform: Platform) -> Option<Provider> {
    match platform {
        Platform::Github => Some(Provider::Github),
        Platform::Gitlab => Some(Provider::Gitlab),
        Platform::Gitea | Platform::Forgejo => Some(Provider::Gitea),
        Platform::Shell => None,
    }
}

#[derive(Deserialize)]
pub struct ConfigQuery {
    platform: Platform,
    #[serde(default)]
    run_on: RunOn,
}

pub async fn config(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<ConfigQuery>,
) -> ApiResult<Json<Pipeline>> {
    let d = access::deployment(&state, &current, &id, Permission::DepCi).await?;
    let (what, branch) = match &d.spec.source {
        Source::Image { image, .. } => (What::Image { repository: image_repository(image).to_ascii_lowercase() }, None),
        Source::Git { reference, .. } => (What::Pull, reference.clone()),
        Source::Static {} => (What::Site, None),
        Source::Upload { .. } | Source::Compose { .. } | Source::Dockerfile { .. } => {
            return Err(ApiError::bad_request("this deployment has nothing for CI to build, call the deploy endpoint with a token instead"));
        }
    };
    let run_on = if q.platform == Platform::Shell { RunOn::Hosted } else { q.run_on };
    let local_build = run_on == RunOn::Here
        && matches!(what, What::Image { .. })
        && runners::list(&state.db)
            .await?
            .iter()
            .any(|p| p.enabled && Some(p.provider) == provider_of(q.platform) && p.docker_mode == DockerMode::Host);
    let url = super::ci::public_url(&state.db).await?;
    let job = Job { url: &url, deployment_id: &id, what, run_on, local_build, branch: branch.as_deref() };
    Ok(Json(render(q.platform, &job)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(what: What, run_on: RunOn, local_build: bool) -> Job<'static> {
        Job { url: "https://kuroe.example.com", deployment_id: "dep-1", what, run_on, local_build, branch: None }
    }

    fn image(repo: &str) -> What {
        What::Image { repository: repo.into() }
    }

    #[test]
    fn github_builds_pushes_to_ghcr_and_deploys_the_commit() {
        let p = render(Platform::Github, &job(image("ghcr.io/acme/api"), RunOn::Hosted, false));
        assert_eq!(p.path, ".github/workflows/kuroe.yml");
        assert!(p.body.contains("tags: ghcr.io/acme/api:${{ github.sha }}"), "{}", p.body);
        assert!(p.body.contains("password: ${{ secrets.GITHUB_TOKEN }}") && p.body.contains("packages: write"));
        assert!(p.body.contains(r#"-d '{"tag":"${{ github.sha }}"}'"#));
        assert!(p.body.contains("https://kuroe.example.com/api/v1/deployments/dep-1/deploy"));
        assert!(p.body.contains("runs-on: ubuntu-latest"));
        assert_eq!(p.secrets, ["KUROE_TOKEN"]);

        let hub = render(Platform::Github, &job(image("acme/api"), RunOn::Hosted, false));
        assert!(hub.body.contains("registry: docker.io") && !hub.body.contains("packages: write"));
        assert_eq!(hub.secrets, ["KUROE_TOKEN", "REGISTRY_USER", "REGISTRY_PASSWORD"]);
    }

    #[test]
    fn on_this_server_runs_on_the_pool_and_can_skip_the_registry() {
        let here = render(Platform::Github, &job(image("ghcr.io/acme/api"), RunOn::Here, true));
        assert!(here.body.contains("runs-on: [self-hosted, kuroe]"));
        assert!(here.body.contains("docker build -t ghcr.io/acme/api:${{ github.sha }} ."));
        assert!(!here.body.contains("login-action") && !here.body.contains("push: true"));
        let gitea = render(Platform::Gitea, &job(What::Pull, RunOn::Here, false));
        assert_eq!(gitea.path, ".gitea/workflows/kuroe.yml");
        assert!(gitea.body.contains("runs-on: kuroe"));
        assert_eq!(render(Platform::Forgejo, &job(What::Pull, RunOn::Hosted, false)).path, ".forgejo/workflows/kuroe.yml");
    }

    #[test]
    fn gitlab_uses_its_registry_and_tags() {
        let p = render(Platform::Gitlab, &job(image("registry.gitlab.com/acme/api"), RunOn::Here, false));
        assert_eq!(p.path, ".gitlab-ci.yml");
        assert!(p.body.contains("docker login -u \"$CI_REGISTRY_USER\" --password-stdin $CI_REGISTRY"), "{}", p.body);
        assert!(p.body.contains("    - docker:29-dind") && p.body.contains("  tags:\n    - kuroe"));
        assert!(p.body.contains(r#"-d "{\"tag\":\"$CI_COMMIT_SHA\"}""#), "{}", p.body);
        assert!(p.body.contains("$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH"));
        assert_eq!(p.secrets, ["KUROE_TOKEN"]);
        let site = render(Platform::Gitlab, &Job { branch: Some("release"), ..job(What::Site, RunOn::Hosted, false) });
        assert!(site.body.contains("git archive --format=tar.gz -o site.tar.gz HEAD"));
        assert!(site.body.contains("site?deploy=true") && site.body.contains(r#"$CI_COMMIT_BRANCH == "release""#));
    }

    #[test]
    fn shell_script_works_anywhere() {
        let p = render(Platform::Shell, &job(image("registry.example.com/acme/api"), RunOn::Here, true));
        assert!(p.path.is_empty());
        assert!(p.body.starts_with("#!/bin/sh\nset -eu\n"));
        assert!(p.body.contains("--password-stdin registry.example.com") && p.body.contains("docker push registry.example.com/acme/api:$SHA"));
        assert_eq!(registry_host("localhost:5000/api"), "localhost:5000");
        assert_eq!(registry_host("nginx"), "docker.io");
    }
}
