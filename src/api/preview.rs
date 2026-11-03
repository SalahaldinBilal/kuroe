use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::access;
use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use crate::auth::SESSION_COOKIE;
use crate::auth::Permission;
use crate::docker::rewrite::PROXY_NETWORK;

const HOP_HEADERS: &[HeaderName] = &[
    header::CONNECTION,
    header::HOST,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
    header::TE,
    header::TRAILER,
    header::PROXY_AUTHORIZATION,
];

pub async fn proxy_root(Path((container, port)): Path<(String, u16)>) -> Redirect {
    Redirect::permanent(&format!("/preview/{container}/{port}/"))
}

// The catch-all route doesn't match an empty path, so `/preview/{container}/{port}/` needs its own route.
pub async fn proxy_index(
    state: State<AppState>,
    current: CurrentUser,
    Path((container, port)): Path<(String, u16)>,
    request: Request,
) -> ApiResult<Response> {
    forward(state, current, container, port, String::new(), request).await
}

pub async fn proxy(
    state: State<AppState>,
    current: CurrentUser,
    Path((container, port, rest)): Path<(String, u16, String)>,
    request: Request,
) -> ApiResult<Response> {
    forward(state, current, container, port, rest, request).await
}

async fn container_ip(state: &AppState, current: &CurrentUser, container: &str) -> ApiResult<String> {
    let inspect = access::container(state, current, container, Permission::DepShell).await?;
    inspect
        .network_settings
        .and_then(|n| n.networks)
        .and_then(|n| n.get(PROXY_NETWORK).cloned())
        .and_then(|n| n.ip_address)
        .filter(|ip| !ip.is_empty())
        .ok_or_else(|| ApiError::new(StatusCode::BAD_GATEWAY, format!("container is not on the {PROXY_NETWORK} network")))
}

async fn forward(
    State(state): State<AppState>,
    current: CurrentUser,
    container: String,
    port: u16,
    rest: String,
    request: Request,
) -> ApiResult<Response> {
    let ip = container_ip(&state, &current, &container).await?;

    let query = request.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!("http://{ip}:{port}/{rest}{query}");
    let method = request.method().clone();
    let headers = forward_headers(request.headers(), &container, port);
    let body = reqwest::Body::wrap_stream(request.into_body().into_data_stream());

    let upstream = state
        .http
        .request(method, &url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("port {port} did not answer: {e}")))?;

    let mut response = Response::builder().status(upstream.status());
    for (name, value) in upstream.headers() {
        if !HOP_HEADERS.contains(name) {
            response = response.header(name, value);
        }
    }
    Ok(response
        .body(Body::from_stream(upstream.bytes_stream()))
        .map_err(|e| anyhow::anyhow!("building preview response: {e}"))?
        .into_response())
}

pub const MAX_ANSWER_BYTES: usize = 16 << 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A request built on the preview page, sent by the server so any header can be set and redirects show as they are.
#[derive(Deserialize)]
pub struct Outgoing {
    port: u16,
    method: String,
    path: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    /// Base64.
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Answer {
    status: u16,
    reason: Option<&'static str>,
    headers: Vec<(String, String)>,
    /// Base64, at most `MAX_ANSWER_BYTES`.
    body: String,
    /// The whole body's size, from Content-Length when the body was cut off.
    size: u64,
    truncated: bool,
    time_ms: u64,
}

/// Connection headers are the client's business. Host is kept, apps route on it.
fn outgoing_headers(pairs: &[(String, String)]) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let name: HeaderName = name.parse().map_err(|_| format!("{name} is not a header name"))?;
        if HOP_HEADERS.contains(&name) && name != header::HOST {
            continue;
        }
        let value = value.parse().map_err(|_| format!("the value of {name} can't be sent as a header"))?;
        headers.append(name, value);
    }
    Ok(headers)
}

fn outgoing_path(path: &str) -> Result<&str, String> {
    let path = path.trim();
    if !path.starts_with('/') {
        return Err("the path starts with /".into());
    }
    if path.chars().any(char::is_whitespace) {
        return Err("the path can't have spaces, encode them as %20".into());
    }
    Ok(path)
}

pub async fn send(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(container): Path<String>,
    Json(out): Json<Outgoing>,
) -> ApiResult<Json<Answer>> {
    let ip = container_ip(&state, &current, &container).await?;
    let method = reqwest::Method::from_bytes(out.method.trim().to_ascii_uppercase().as_bytes())
        .map_err(|_| ApiError::bad_request(format!("{} is not an HTTP method", out.method)))?;
    let path = outgoing_path(&out.path).map_err(ApiError::bad_request)?;
    let headers = outgoing_headers(&out.headers).map_err(ApiError::bad_request)?;
    let body = match &out.body {
        Some(body) => BASE64.decode(body).map_err(|_| ApiError::bad_request("the body is not base64"))?,
        None => Vec::new(),
    };
    let port = out.port;
    let started = Instant::now();
    let upstream = state
        .http
        .request(method, format!("http://{ip}:{port}{path}"))
        .headers(headers)
        .body(body)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| {
            let message = if e.is_timeout() {
                format!("port {port} did not answer: nothing within {} s", REQUEST_TIMEOUT.as_secs())
            } else if e.is_connect() {
                format!("nothing is listening on port {port} in this container")
            } else {
                format!("port {port} did not answer: {e}")
            };
            ApiError::new(StatusCode::BAD_GATEWAY, message)
        })?;
    let status = upstream.status();
    let headers: Vec<(String, String)> = upstream
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
        .collect();
    let length = upstream.content_length();
    let mut stream = upstream.bytes_stream();
    let mut bytes = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("reading the answer: {e}")))?;
        let room = MAX_ANSWER_BYTES - bytes.len();
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    let size = if truncated { length.unwrap_or(bytes.len() as u64) } else { bytes.len() as u64 };
    Ok(Json(Answer {
        status: status.as_u16(),
        reason: status.canonical_reason(),
        headers,
        body: BASE64.encode(&bytes),
        size,
        truncated,
        time_ms: started.elapsed().as_millis() as u64,
    }))
}

fn forward_headers(incoming: &HeaderMap, container: &str, port: u16) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in incoming {
        if HOP_HEADERS.contains(name) || name == header::COOKIE {
            continue;
        }
        headers.append(name, value.clone());
    }
    let cookies: Vec<&str> = incoming
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|c| !c.starts_with(&format!("{SESSION_COOKIE}=")))
        .collect();
    if !cookies.is_empty()
        && let Ok(value) = cookies.join("; ").parse()
    {
        headers.insert(header::COOKIE, value);
    }
    if let Ok(prefix) = format!("/preview/{container}/{port}").parse() {
        headers.insert("x-forwarded-prefix", prefix);
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookie_is_not_forwarded() {
        let mut incoming = HeaderMap::new();
        incoming.insert(header::COOKIE, "theme=dark; kuroe_session=abc; app=1".parse().unwrap());
        incoming.insert(header::HOST, "kuroe.local".parse().unwrap());
        incoming.insert(header::ACCEPT, "text/html".parse().unwrap());
        let out = forward_headers(&incoming, "c1", 3000);
        assert_eq!(out[header::COOKIE], "theme=dark; app=1");
        assert!(out.get(header::HOST).is_none());
        assert_eq!(out[header::ACCEPT], "text/html");
        assert_eq!(out["x-forwarded-prefix"], "/preview/c1/3000");
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn built_requests_keep_host_and_cookies_but_not_connection_headers() {
        let headers = outgoing_headers(&pairs(&[
            ("Host", "shop.example.com"),
            ("Cookie", "cart=3"),
            ("Connection", "close"),
            ("", "ignored"),
            ("X-Twice", "a"),
            ("X-Twice", "b"),
        ]))
        .unwrap();
        assert_eq!(headers[header::HOST], "shop.example.com");
        assert_eq!(headers[header::COOKIE], "cart=3");
        assert!(headers.get(header::CONNECTION).is_none());
        assert_eq!(headers.get_all("x-twice").iter().count(), 2);
        assert!(outgoing_headers(&pairs(&[("bad name", "x")])).is_err());
        assert!(outgoing_headers(&pairs(&[("X-Line", "a\nb")])).is_err());
    }

    #[test]
    fn built_paths_stay_on_the_container() {
        assert_eq!(outgoing_path(" /api/items?page=2 "), Ok("/api/items?page=2"));
        assert!(outgoing_path("api/items").is_err());
        assert!(outgoing_path("@evil.example.com/").is_err(), "no way to change the host");
        assert!(outgoing_path("/a b").is_err());
    }
}
