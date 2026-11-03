//! Repositories a saved Git login can read, for the picker on New deployment.

use anyhow::bail;
use serde::Serialize;
use serde_json::Value;

use super::{Identity, Kind, api_base, github, host_of};

/// At most this many pages, so a token that sees thousands of repositories still answers quickly.
const MAX_PAGES: u32 = 10;
const GITEA_PAGE: usize = 50;
const PAGE: usize = 100;

#[derive(Debug, Serialize, PartialEq)]
pub struct Repo {
    pub full_name: String,
    pub clone_url: String,
    pub default_branch: String,
    pub private: bool,
}

/// GitHub and Gitea name things the same way.
pub fn parse_github(list: &[Value]) -> Vec<Repo> {
    list.iter()
        .map(|r| Repo {
            full_name: r["full_name"].as_str().unwrap_or_default().to_owned(),
            clone_url: r["clone_url"].as_str().unwrap_or_default().to_owned(),
            default_branch: r["default_branch"].as_str().unwrap_or_default().to_owned(),
            private: r["private"].as_bool().unwrap_or(false),
        })
        .collect()
}

pub fn parse_gitlab(list: &[Value]) -> Vec<Repo> {
    list.iter()
        .map(|r| Repo {
            full_name: r["path_with_namespace"].as_str().unwrap_or_default().to_owned(),
            clone_url: r["http_url_to_repo"].as_str().unwrap_or_default().to_owned(),
            default_branch: r["default_branch"].as_str().unwrap_or_default().to_owned(),
            private: r["visibility"].as_str() != Some("public"),
        })
        .collect()
}

pub fn lists(kind: Kind) -> bool {
    matches!(kind, Kind::GithubApp | Kind::GithubToken | Kind::Gitlab | Kind::Gitea)
}

async fn page(request: reqwest::RequestBuilder, provider: &str) -> anyhow::Result<Vec<Value>> {
    let response = request.send().await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let message = body["message"].as_str().or_else(|| body["error"].as_str()).unwrap_or("no message");
        bail!("{provider} refused the repository list ({status}): {message}");
    }
    Ok(body.as_array().cloned().unwrap_or_default())
}

pub async fn list(http: &reqwest::Client, identity: &Identity) -> anyhow::Result<Vec<Repo>> {
    let mut repos = Vec::new();
    match identity.kind {
        Kind::GithubApp => repos = github::repositories(http, identity).await?,
        Kind::GithubToken => {
            for n in 1..=MAX_PAGES {
                let path = format!("/user/repos?per_page={PAGE}&page={n}&affiliation=owner,collaborator,organization_member");
                let list = page(github::request(http, reqwest::Method::GET, &path, &identity.secret), "GitHub").await?;
                repos.extend(parse_github(&list));
                if list.len() < PAGE {
                    break;
                }
            }
        }
        Kind::Gitlab => {
            for n in 1..=MAX_PAGES {
                let url = format!("{}/api/v4/projects?membership=true&simple=true&per_page={PAGE}&page={n}", api_base(identity));
                let list = page(http.get(url).header("private-token", &identity.secret), "GitLab").await?;
                repos.extend(parse_gitlab(&list));
                if list.len() < PAGE {
                    break;
                }
            }
        }
        Kind::Gitea => {
            for n in 1..=MAX_PAGES {
                let url = format!("{}/api/v1/user/repos?limit={GITEA_PAGE}&page={n}", api_base(identity));
                let list = page(http.get(url).header("authorization", format!("token {}", identity.secret)), &host_of(identity)).await?;
                repos.extend(parse_github(&list));
                if list.len() < GITEA_PAGE {
                    break;
                }
            }
        }
        Kind::Git => bail!("a plain Git login has no API to list repositories, type the URL"),
        Kind::Registry | Kind::Cloudflare | Kind::Certificate => bail!("{} is not a Git login", identity.name),
    }
    repos.retain(|r| !r.clone_url.is_empty());
    repos.sort_by_key(|r| r.full_name.to_lowercase());
    Ok(repos)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn github_and_gitea_lists_read_the_same_fields() {
        let list = [json!({ "full_name": "acme/api", "clone_url": "https://github.com/acme/api.git", "default_branch": "main", "private": true })];
        assert_eq!(
            parse_github(&list),
            [Repo { full_name: "acme/api".into(), clone_url: "https://github.com/acme/api.git".into(), default_branch: "main".into(), private: true }]
        );
    }

    #[test]
    fn gitlab_projects_become_repos() {
        let list = [
            json!({ "path_with_namespace": "acme/web", "http_url_to_repo": "https://gitlab.com/acme/web.git", "default_branch": "trunk", "visibility": "private" }),
            json!({ "path_with_namespace": "acme/empty", "http_url_to_repo": "https://gitlab.com/acme/empty.git", "default_branch": null, "visibility": "public" }),
        ];
        let repos = parse_gitlab(&list);
        assert_eq!((repos[0].full_name.as_str(), repos[0].default_branch.as_str(), repos[0].private), ("acme/web", "trunk", true));
        assert_eq!((repos[1].default_branch.as_str(), repos[1].private), ("", false), "an empty project has no branch yet");
    }

    #[test]
    fn only_logins_with_an_api_list_repos() {
        assert!(lists(Kind::GithubApp) && lists(Kind::GithubToken) && lists(Kind::Gitlab) && lists(Kind::Gitea));
        assert!(!lists(Kind::Git) && !lists(Kind::Registry));
    }
}
