use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::{Identity, Kind, host_of};

const DOCKER_HUB_KEY: &str = "https://index.docker.io/v1/";

pub fn registry_host(identity: &Identity) -> String {
    match identity.kind {
        Kind::GithubToken => "ghcr.io".into(),
        Kind::Gitlab if identity.host.trim().is_empty() => "registry.gitlab.com".into(),
        _ => host_of(identity),
    }
}

fn registry_user(identity: &Identity) -> String {
    match identity.kind {
        Kind::GithubToken if identity.username.is_empty() => "x-access-token".into(),
        Kind::Gitlab if identity.username.is_empty() => "oauth2".into(),
        _ => identity.username.clone(),
    }
}

/// `docker.io` and its aliases live under the old index URL in a Docker config file.
fn config_key(host: &str) -> String {
    match host {
        "docker.io" | "index.docker.io" | "registry-1.docker.io" | "registry.hub.docker.com" => DOCKER_HUB_KEY.into(),
        other => other.into(),
    }
}

/// A Docker CLI config that logs in to one registry. Plugin folders stay reachable so `docker compose` still works.
pub fn docker_config(identity: &Identity, plugin_dirs: &[PathBuf]) -> Value {
    let auth = STANDARD.encode(format!("{}:{}", registry_user(identity), identity.secret));
    json!({
        "auths": { config_key(&registry_host(identity)): { "auth": auth } },
        "cliPluginsExtraDirs": plugin_dirs,
    })
}

/// A temporary `DOCKER_CONFIG` folder for one deploy. It's removed when this is dropped.
pub struct TempDockerConfig {
    dir: PathBuf,
}

impl TempDockerConfig {
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TempDockerConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn user_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("DOCKER_CONFIG").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".docker"))) {
        dirs.push(home.join("cli-plugins"));
    }
    dirs
}

pub fn write_docker_config(data_dir: &Path, identity: &Identity) -> anyhow::Result<TempDockerConfig> {
    if !identity.kind.pulls_images() {
        bail!("{} can't log in to a registry", identity.name);
    }
    let dir = data_dir.join("tmp").join(format!("docker-config-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let guard = TempDockerConfig { dir };
    let file = guard.dir.join("config.json");
    std::fs::write(&file, docker_config(identity, &user_plugin_dirs()).to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&guard.dir, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(guard)
}

/// `Bearer realm="https://auth.docker.io/token",service="registry.docker.io"` into its parts.
pub fn parse_challenge(header: &str) -> Option<(String, Vec<(String, String)>)> {
    let rest = header.trim().strip_prefix("Bearer ")?;
    let mut realm = None;
    let mut params = Vec::new();
    for part in rest.split(',') {
        let (key, value) = part.trim().split_once('=')?;
        let value = value.trim_matches('"').to_owned();
        if key == "realm" {
            realm = Some(value);
        } else {
            params.push((key.to_owned(), value));
        }
    }
    Some((realm?, params))
}

fn api_host(host: &str) -> &str {
    if config_key(host) == DOCKER_HUB_KEY { "registry-1.docker.io" } else { host }
}

/// Logs in the way `docker login` does: ask `/v2/`, and follow a token challenge with basic auth.
pub async fn check_login(http: &reqwest::Client, identity: &Identity) -> anyhow::Result<String> {
    let host = registry_host(identity);
    let user = registry_user(identity);
    let base = format!("https://{}/v2/", api_host(&host));
    let first = http.get(&base).basic_auth(&user, Some(&identity.secret)).send().await.with_context(|| format!("reaching {host}"))?;
    if first.status().is_success() {
        return Ok(format!("Logged in to {host}"));
    }
    let challenge = first.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).and_then(parse_challenge);
    let Some((realm, params)) = challenge else { bail!("{host} refused the login ({})", first.status()) };
    let url = reqwest::Url::parse_with_params(&realm, &params).with_context(|| format!("{host} sent a bad token address"))?;
    let token = http.get(url).basic_auth(&user, Some(&identity.secret)).send().await?;
    if !token.status().is_success() {
        bail!("{host} refused the login ({})", token.status());
    }
    Ok(format!("Logged in to {host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(kind: Kind, host: &str, user: &str) -> Identity {
        Identity {
            id: "r1".into(),
            kind,
            name: "hub".into(),
            host: host.into(),
            username: user.into(),
            secret: "pa:ss".into(),
            meta: json!({}),
            created_at: 0,
            last_used_at: None,
            expires_at: None,
        }
    }

    #[test]
    fn docker_config_logs_in_to_one_registry() {
        let config = docker_config(&identity(Kind::Registry, "docker.io", "acmebot"), &[PathBuf::from("/home/k/.docker/cli-plugins")]);
        assert_eq!(config["auths"][DOCKER_HUB_KEY]["auth"], STANDARD.encode("acmebot:pa:ss"));
        assert_eq!(config["cliPluginsExtraDirs"], json!(["/home/k/.docker/cli-plugins"]));

        let ghcr = docker_config(&identity(Kind::GithubToken, "", ""), &[]);
        assert_eq!(ghcr["auths"]["ghcr.io"]["auth"], STANDARD.encode("x-access-token:pa:ss"));
        let own = docker_config(&identity(Kind::Registry, "https://registry.example.com:5000", "ci"), &[]);
        assert!(own["auths"]["registry.example.com:5000"].is_object(), "{own}");
    }

    #[test]
    fn temp_config_is_private_and_goes_away() {
        let data = std::env::temp_dir().join(format!("kuroe-dc-{}", uuid::Uuid::new_v4()));
        let config = write_docker_config(&data, &identity(Kind::Registry, "ghcr.io", "me")).unwrap();
        let file = config.path().join("config.json");
        let written: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(written["auths"]["ghcr.io"].is_object());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let dir = config.path().to_owned();
        drop(config);
        assert!(!dir.exists());
        assert!(write_docker_config(&data, &identity(Kind::Cloudflare, "", "")).is_err());
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn reads_a_bearer_challenge() {
        let (realm, params) = parse_challenge(r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io""#).unwrap();
        assert_eq!(realm, "https://auth.docker.io/token");
        assert_eq!(params, [("service".to_owned(), "registry.docker.io".to_owned())]);
        assert!(parse_challenge("Basic realm=\"x\"").is_none());
    }
}
