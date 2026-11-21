//! Serves the embedded Svelte build (`web/dist`) with SPA fallback to index.html.

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(resp) = serve(path) {
        return resp;
    }
    // Unknown non-asset paths are client-side routes: hand back the app shell.
    if path.starts_with("api/") || path.starts_with("assets/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    return serve("index.html").unwrap_or_else(|| {
        log_missing();
        (
            StatusCode::NOT_FOUND,
            "frontend not built: run tools/build.sh",
        )
            .into_response()
    });
}

fn serve(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = file.metadata.mimetype().to_string();
    // Vite hashes asset filenames, so they can be cached forever; index.html must not be.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let resp = (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        file.data,
    )
        .into_response();
    return Some(resp);
}

fn log_missing() {
    log::warn!("index.html not embedded; build the frontend with tools/build.sh");
}
