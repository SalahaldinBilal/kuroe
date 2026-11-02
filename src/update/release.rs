use anyhow::{Context, bail};
use serde::Serialize;
use serde_json::Value;

const LATEST: &str = "https://api.github.com/repos/SalahaldinBilal/kuroe/releases/latest";
pub const IMAGE: &str = "ghcr.io/salahaldinbilal/kuroe";

#[derive(Debug, Serialize, PartialEq)]
pub struct Latest {
    pub version: String,
    pub newer: bool,
    pub notes: String,
    pub url: String,
    pub image: String,
    pub binary_url: Option<String>,
}

pub fn arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim_start_matches('v').splitn(3, '.').map(|p| p.parse().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse(candidate), parse(current)), (Some(a), Some(b)) if a > b)
}

pub fn from_release(release: &Value, current: &str, arch: Option<&str>) -> anyhow::Result<Latest> {
    let tag = release["tag_name"].as_str().context("the release has no tag")?;
    let version = tag.trim_start_matches('v').to_owned();
    let binary_url = arch.and_then(|arch| {
        let name = format!("kuroe-linux-{arch}");
        release["assets"].as_array()?.iter().find(|a| a["name"] == name.as_str())?["browser_download_url"].as_str().map(str::to_owned)
    });
    Ok(Latest {
        newer: is_newer(&version, current),
        notes: release["body"].as_str().unwrap_or_default().to_owned(),
        url: release["html_url"].as_str().unwrap_or_default().to_owned(),
        image: format!("{IMAGE}:{version}"),
        binary_url,
        version,
    })
}

pub async fn latest(http: &reqwest::Client, current: &str) -> anyhow::Result<Latest> {
    let response = http
        .get(LATEST)
        .header("accept", "application/vnd.github+json")
        .header("user-agent", "kuroe")
        .send()
        .await
        .context("asking GitHub for the latest release")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("no kuroe release is published yet");
    }
    let release: Value = response.error_for_status().context("GitHub refused the release check")?.json().await?;
    from_release(&release, current, arch())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compares_versions_by_number() {
        assert!(is_newer("0.10.0", "0.9.3"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("nightly", "0.2.0"));
    }

    #[test]
    fn picks_the_binary_for_this_arch() {
        let release = json!({
            "tag_name": "v0.3.0",
            "body": "Fixes.",
            "html_url": "https://github.com/SalahaldinBilal/kuroe/releases/tag/v0.3.0",
            "assets": [
                { "name": "kuroe-linux-amd64", "browser_download_url": "https://example.com/kuroe-linux-amd64" },
                { "name": "kuroe-linux-amd64.sha256", "browser_download_url": "https://example.com/kuroe-linux-amd64.sha256" },
                { "name": "kuroe-linux-arm64", "browser_download_url": "https://example.com/kuroe-linux-arm64" }
            ]
        });
        let latest = from_release(&release, "0.2.0", Some("arm64")).unwrap();
        assert_eq!(latest.version, "0.3.0");
        assert!(latest.newer);
        assert_eq!(latest.image, "ghcr.io/salahaldinbilal/kuroe:0.3.0");
        assert_eq!(latest.binary_url.as_deref(), Some("https://example.com/kuroe-linux-arm64"));
        assert_eq!(from_release(&release, "0.3.0", None).unwrap().binary_url, None);
        assert!(from_release(&json!({}), "0.2.0", None).is_err());
    }
}
