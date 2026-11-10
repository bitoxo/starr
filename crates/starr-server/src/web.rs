//! Serves the web UI (built into `web/dist`) embedded in the binary, with SPA
//! fallback to `index.html` for client-side routes.

use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/dist"]
struct Assets;

const PLACEHOLDER: &str = "<!doctype html><title>Starr</title>\
<p>Starr is running, but the web UI was not built into this binary. \
Run <code>just web-build</code> and rebuild.</p>";

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(file) = Assets::get(path) {
        return asset_response(path, &file.data);
    }
    // Unknown path without a file extension: client-side route -> index.html.
    let looks_like_file = path.rsplit('/').next().is_some_and(|seg| seg.contains('.'));
    if looks_like_file {
        return StatusCode::NOT_FOUND.into_response();
    }
    match Assets::get("index.html") {
        Some(index) => asset_response("index.html", &index.data),
        None => Html(PLACEHOLDER).into_response(),
    }
}

fn asset_response(path: &str, data: &[u8]) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Vite emits content-hashed files under assets/: cache them forever.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_owned()),
            (header::CACHE_CONTROL, cache.to_owned()),
        ],
        data.to_vec(),
    )
        .into_response()
}
