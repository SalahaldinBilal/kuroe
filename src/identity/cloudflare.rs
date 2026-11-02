use anyhow::{Context, bail};
use serde_json::{Value, json};

const API: &str = "https://api.cloudflare.com/client/v4";

async fn send(http: &reqwest::Client, token: &str, method: reqwest::Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
    let mut request = http.request(method, format!("{API}{path}")).bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.context("reaching Cloudflare")?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() || body["success"] != Value::Bool(true) {
        let message = body["errors"][0]["message"].as_str().unwrap_or("no message");
        bail!("Cloudflare refused it ({status}): {message}");
    }
    Ok(body)
}

async fn call(http: &reqwest::Client, token: &str, path: &str) -> anyhow::Result<Value> {
    send(http, token, reqwest::Method::GET, path, None).await
}

pub struct Checked {
    pub zones: Vec<String>,
    pub expires_on: Option<String>,
}

/// Checks the token is active and lists the zones it can reach.
pub async fn check(http: &reqwest::Client, token: &str) -> anyhow::Result<Checked> {
    let verify = call(http, token, "/user/tokens/verify").await?;
    if verify["result"]["status"].as_str() != Some("active") {
        bail!("the Cloudflare token is {}", verify["result"]["status"].as_str().unwrap_or("not active"));
    }
    let mut zones = Vec::new();
    for page in 1..=10 {
        let body = call(http, token, &format!("/zones?per_page=50&page={page}")).await?;
        let list = body["result"].as_array().cloned().unwrap_or_default();
        zones.extend(list.iter().filter_map(|z| z["name"].as_str().map(str::to_owned)));
        if list.len() < 50 {
            break;
        }
    }
    zones.sort();
    Ok(Checked { zones, expires_on: verify["result"]["expires_on"].as_str().map(str::to_owned) })
}

pub const ORIGIN_VALIDITY_DAYS: u32 = 5475;

/// The request body for an origin certificate. kuroe sends a CSR so the key stays on this server.
pub fn origin_request(hostnames: &[String], csr: &str) -> Value {
    json!({ "hostnames": hostnames, "requested_validity": ORIGIN_VALIDITY_DAYS, "request_type": "origin-ecc", "csr": csr })
}

pub struct Origin {
    pub id: String,
    pub certificate: String,
}

pub async fn create_origin(http: &reqwest::Client, token: &str, hostnames: &[String], csr: &str) -> anyhow::Result<Origin> {
    let body = send(http, token, reqwest::Method::POST, "/certificates", Some(origin_request(hostnames, csr))).await?;
    Ok(Origin {
        id: body["result"]["id"].as_str().context("Cloudflare sent no certificate id")?.to_owned(),
        certificate: body["result"]["certificate"].as_str().context("Cloudflare sent no certificate")?.to_owned(),
    })
}

pub async fn revoke_origin(http: &reqwest::Client, token: &str, id: &str) -> anyhow::Result<()> {
    send(http, token, reqwest::Method::DELETE, &format!("/certificates/{id}"), None).await?;
    Ok(())
}

/// The zone a hostname belongs to, the longest zone name that is a suffix of it.
pub fn zone_for<'a>(zones: &'a [String], host: &str) -> Option<&'a String> {
    let host = host.trim_start_matches("*.");
    zones.iter().filter(|z| host == z.as_str() || host.ends_with(&format!(".{z}"))).max_by_key(|z| z.len())
}

/// Turns on Authenticated Origin Pulls for a zone, so Cloudflare presents its client certificate to this server.
pub async fn enable_origin_pulls(http: &reqwest::Client, token: &str, zone: &str) -> anyhow::Result<()> {
    let found = call(http, token, &format!("/zones?name={zone}")).await?;
    let id = found["result"][0]["id"].as_str().with_context(|| format!("Cloudflare doesn't show the zone {zone} to this token"))?;
    send(http, token, reqwest::Method::PUT, &format!("/zones/{id}/settings/tls_client_auth"), Some(json!({ "value": "on" }))).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_request_sends_a_csr_for_fifteen_years() {
        let body = origin_request(&["example.com".into(), "*.example.com".into()], "-----BEGIN CERTIFICATE REQUEST-----");
        assert_eq!(
            body,
            json!({
                "hostnames": ["example.com", "*.example.com"],
                "requested_validity": 5475,
                "request_type": "origin-ecc",
                "csr": "-----BEGIN CERTIFICATE REQUEST-----",
            })
        );
    }

    #[test]
    fn picks_the_zone_a_hostname_lives_in() {
        let zones = ["example.com".to_owned(), "shop.example.com".to_owned(), "other.dev".to_owned()];
        assert_eq!(zone_for(&zones, "*.example.com").map(String::as_str), Some("example.com"));
        assert_eq!(zone_for(&zones, "a.shop.example.com").map(String::as_str), Some("shop.example.com"));
        assert_eq!(zone_for(&zones, "nope.test"), None);
    }
}
