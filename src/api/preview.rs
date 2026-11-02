use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

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

async fn forward(
    State(state): State<AppState>,
    current: CurrentUser,
    container: String,
    port: u16,
    rest: String,
    request: Request,
) -> ApiResult<Response> {
    let inspect = access::container(&state, &current, &container, Permission::DepShell).await?;
    let ip = inspect
        .network_settings
        .and_then(|n| n.networks)
        .and_then(|n| n.get(PROXY_NETWORK).cloned())
        .and_then(|n| n.ip_address)
        .filter(|ip| !ip.is_empty())
        .ok_or_else(|| ApiError::new(StatusCode::BAD_GATEWAY, format!("container is not on the {PROXY_NETWORK} network")))?;

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
}
