use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Identity;
use super::repos::{Repo, parse_github};

const API: &str = "https://api.github.com";

/// What a GitHub App identity keeps in `secret`. Both values come from the manifest conversion.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppSecret {
    pub pem: String,
    pub webhook_secret: String,
}

pub fn app_secret(identity: &Identity) -> anyhow::Result<AppSecret> {
    serde_json::from_str(&identity.secret).context("the GitHub App's key is missing, connect it again")
}

fn pem_der(pem: &str) -> anyhow::Result<(Vec<u8>, bool)> {
    let pkcs8 = pem.contains("BEGIN PRIVATE KEY");
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).map(str::trim).collect();
    Ok((STANDARD.decode(body).context("the private key is not valid PEM")?, pkcs8))
}

/// The short-lived token an app signs to talk to GitHub as itself.
pub fn app_jwt(app_id: u64, pem: &str, now: i64) -> anyhow::Result<String> {
    let (der, pkcs8) = pem_der(pem)?;
    let key = if pkcs8 { RsaKeyPair::from_pkcs8(&der) } else { RsaKeyPair::from_der(&der) }
        .map_err(|e| anyhow::anyhow!("the private key can't be used: {e}"))?;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(json!({ "iat": now - 60, "exp": now + 540, "iss": app_id.to_string() }).to_string());
    let message = format!("{header}.{claims}");
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), message.as_bytes(), &mut signature)
        .map_err(|_| anyhow::anyhow!("signing the GitHub App token failed"))?;
    Ok(format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

pub fn request(http: &reqwest::Client, method: reqwest::Method, path: &str, bearer: &str) -> reqwest::RequestBuilder {
    http.request(method, format!("{API}{path}"))
        .bearer_auth(bearer)
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .header("user-agent", "kuroe")
}

async fn json_ok(response: reqwest::Response, what: &str) -> anyhow::Result<Value> {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let message = body["message"].as_str().unwrap_or("no message");
        bail!("GitHub refused {what} ({status}): {message}");
    }
    Ok(body)
}

fn app_id(identity: &Identity) -> anyhow::Result<u64> {
    identity.meta["app_id"].as_u64().context("the GitHub App has no app id")
}

pub async fn find_installation(http: &reqwest::Client, identity: &Identity) -> anyhow::Result<Option<u64>> {
    let jwt = app_jwt(app_id(identity)?, &app_secret(identity)?.pem, crate::db::now())?;
    let body = json_ok(request(http, reqwest::Method::GET, "/app/installations", &jwt).send().await?, "listing installations").await?;
    Ok(body.as_array().and_then(|list| list.first()).and_then(|i| i["id"].as_u64()))
}

/// Mints an installation token. GitHub makes them valid for an hour.
pub async fn installation_token_for(http: &reqwest::Client, identity: &Identity) -> anyhow::Result<(String, Option<String>)> {
    let installation = identity.meta["installation_id"]
        .as_u64()
        .context("the GitHub App is not installed yet, install it and pick repositories")?;
    let jwt = app_jwt(app_id(identity)?, &app_secret(identity)?.pem, crate::db::now())?;
    let path = format!("/app/installations/{installation}/access_tokens");
    let body = json_ok(request(http, reqwest::Method::POST, &path, &jwt).send().await?, "an installation token").await?;
    let token = body["token"].as_str().context("GitHub sent no token")?.to_owned();
    Ok((token, body["expires_at"].as_str().map(str::to_owned)))
}

pub async fn repositories(http: &reqwest::Client, identity: &Identity) -> anyhow::Result<Vec<Repo>> {
    let (token, _) = installation_token_for(http, identity).await?;
    let mut repos = Vec::new();
    for page in 1..=10 {
        let path = format!("/installation/repositories?per_page=100&page={page}");
        let body = json_ok(request(http, reqwest::Method::GET, &path, &token).send().await?, "the repository list").await?;
        let list = body["repositories"].as_array().cloned().unwrap_or_default();
        repos.extend(parse_github(&list));
        if list.len() < 100 {
            break;
        }
    }
    repos.sort_by(|a, b| a.full_name.cmp(&b.full_name));
    Ok(repos)
}

/// The manifest posted to GitHub to create the app with the right permissions and hook.
pub fn manifest(public_url: &str, name: &str, identity_id: &str) -> Value {
    let base = public_url.trim_end_matches('/');
    let api = crate::api::API_BASE;
    json!({
        "name": name,
        "url": base,
        "hook_attributes": { "url": format!("{base}{api}/hooks/github-app"), "active": true },
        "redirect_url": format!("{base}{api}/identities/github/callback"),
        "setup_url": format!("{base}{api}/identities/{identity_id}/installed"),
        "setup_on_update": true,
        "public": false,
        "default_permissions": { "contents": "read", "metadata": "read" },
        "default_events": ["push"],
    })
}

pub fn new_app_url(owner: Option<&str>, state: &str) -> String {
    match owner.map(str::trim).filter(|o| !o.is_empty()) {
        Some(org) => format!("https://github.com/organizations/{org}/settings/apps/new?state={state}"),
        None => format!("https://github.com/settings/apps/new?state={state}"),
    }
}

#[derive(Debug)]
pub struct Converted {
    pub app_id: u64,
    pub slug: String,
    pub owner: String,
    pub html_url: String,
    pub secret: AppSecret,
}

pub async fn convert_manifest(http: &reqwest::Client, code: &str) -> anyhow::Result<Converted> {
    let response = http
        .post(format!("{API}/app-manifests/{code}/conversions"))
        .header("accept", "application/vnd.github+json")
        .header("user-agent", "kuroe")
        .send()
        .await?;
    let body = json_ok(response, "the app manifest").await?;
    Ok(Converted {
        app_id: body["id"].as_u64().context("GitHub sent no app id")?,
        slug: body["slug"].as_str().unwrap_or_default().to_owned(),
        owner: body["owner"]["login"].as_str().unwrap_or_default().to_owned(),
        html_url: body["html_url"].as_str().unwrap_or_default().to_owned(),
        secret: AppSecret {
            pem: body["pem"].as_str().context("GitHub sent no private key")?.to_owned(),
            webhook_secret: body["webhook_secret"].as_str().unwrap_or_default().to_owned(),
        },
    })
}

/// A personal token check. GitHub reports a fine-grained token's expiry in a response header.
pub async fn check_token(http: &reqwest::Client, token: &str) -> anyhow::Result<(String, Option<String>)> {
    let response = request(http, reqwest::Method::GET, "/user", token).send().await?;
    let expires = response
        .headers()
        .get("github-authentication-token-expiration")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = json_ok(response, "the token").await?;
    Ok((body["login"].as_str().unwrap_or("unknown").to_owned(), expires))
}

#[cfg(test)]
mod tests {
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    use super::*;

    const TEST_KEY: &str = include_str!("testdata/github-app-test.pem");

    #[test]
    fn app_jwt_is_signed_with_the_app_key() {
        let jwt = app_jwt(12345, TEST_KEY, 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims, json!({ "iat": 1_699_999_940, "exp": 1_700_000_540, "iss": "12345" }));

        let (der, _) = pem_der(TEST_KEY).unwrap();
        let key = RsaKeyPair::from_der(&der).unwrap();
        let public = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key.public().as_ref());
        let signed = format!("{}.{}", parts[0], parts[1]);
        public.verify(signed.as_bytes(), &URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
    }

    #[test]
    fn a_bad_key_is_reported() {
        let err = app_jwt(1, "-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----", 0).unwrap_err().to_string();
        assert!(err.starts_with("the private key can't be used"), "{err}");
    }

    #[test]
    fn manifest_points_github_back_at_this_server() {
        let m = manifest("https://kuroe.example.com/", "kuroe-acme", "id-1");
        assert_eq!(m["redirect_url"], "https://kuroe.example.com/api/v1/identities/github/callback");
        assert_eq!(m["setup_url"], "https://kuroe.example.com/api/v1/identities/id-1/installed");
        assert_eq!(m["hook_attributes"]["url"], "https://kuroe.example.com/api/v1/hooks/github-app");
        assert_eq!(m["default_permissions"], json!({ "contents": "read", "metadata": "read" }));
        assert_eq!(new_app_url(Some("acme"), "s1"), "https://github.com/organizations/acme/settings/apps/new?state=s1");
        assert_eq!(new_app_url(Some(" "), "s1"), "https://github.com/settings/apps/new?state=s1");
    }
}
