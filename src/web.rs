use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

pub async fn spa(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(response) = asset(path) {
        return response;
    }
    asset("index.html").unwrap_or_else(|| (StatusCode::NOT_FOUND, "web/dist is empty, build the SPA").into_response())
}

fn asset(path: &str) -> Option<Response> {
    let path = if path.is_empty() { "index.html" } else { path };
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    Some(([(header::CONTENT_TYPE, mime.as_ref()), (header::CACHE_CONTROL, cache)], file.data).into_response())
}
