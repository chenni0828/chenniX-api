//! Embedded static file serving for the admin SPA frontend.
//!
//! Uses `rust-embed` to bundle the frontend build output (`static/`) into
//! the binary at compile time (release) or read from disk (debug).
//!
//! ## Routes
//! - `/admin` and `/admin/` → serve `index.html` (SPA entry point)
//! - `/admin/{*path}` → serve embedded file, fallback to `index.html` (SPA routing)
//! - `/assets/{*path}` → serve embedded file (for absolute-path asset references in index.html)
//! - `/favicon.svg` → serve favicon
//!
//! ## Route priority
//! Admin API routes (`/admin/api/*`) are registered separately and are more
//! specific than the `/admin/{*path}` catch-all, so axum will match them first.
//! Requests to non-existent `/admin/api/*` paths return 404 (not the SPA fallback)
//! to avoid serving HTML for API calls.

use axum::{
    body::Body,
    http::{header, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rust_embed::Embed;

use crate::state::AppState;

/// Embedded frontend assets from `crates/server/static/`.
#[derive(Embed)]
#[folder = "static"]
struct WebAssets;

/// Build the web static-file router.
///
/// Merge this into the main router alongside the admin API routes.
/// The admin API routes (`/admin/api/*`) are more specific than
/// `/admin/{*path}`, so they take precedence in axum's routing tree.
pub fn web_routes() -> Router<AppState> {
    Router::new()
        .route("/admin", get(index_handler))
        .route("/admin/", get(index_handler))
        .route("/admin/*path", get(admin_static_handler))
        .route("/assets/*path", get(assets_handler))
        .route("/favicon.svg", get(favicon_handler))
}

/// Serve `index.html` for the SPA entry point (`/admin` and `/admin/`).
async fn index_handler() -> Response {
    serve_file_or_spa("index.html")
}

/// Handle `/admin/{*path}` — serve the embedded file or fall back to
/// `index.html` for SPA client-side routing.
///
/// Paths starting with `api/` return 404 to avoid serving HTML for API calls.
async fn admin_static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches("/admin/");

    // Don't serve SPA fallback for API paths — they should get a proper 404.
    if path.starts_with("api/") {
        return (axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    }

    serve_file_or_spa(path)
}

/// Handle `/assets/{*path}` — serve embedded assets directly (no SPA fallback).
/// This supports the absolute-path references (`/assets/...`) in index.html.
async fn assets_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    serve_file(path)
}

/// Handle `/favicon.svg` — serve the favicon directly.
async fn favicon_handler() -> Response {
    serve_file("favicon.svg")
}

/// Try to serve an embedded file. Returns 404 if not found.
fn serve_file(path: &str) -> Response {
    if let Some(file) = WebAssets::get(path) {
        return file_to_response(file, path);
    }
    (axum::http::StatusCode::NOT_FOUND, "Not Found").into_response()
}

/// Try to serve an embedded file; if not found, fall back to `index.html`
/// for SPA client-side routing.
fn serve_file_or_spa(path: &str) -> Response {
    // Try exact file match first.
    if let Some(file) = WebAssets::get(path) {
        return file_to_response(file, path);
    }

    // SPA fallback: serve index.html so the client-side router can handle the route.
    if let Some(file) = WebAssets::get("index.html") {
        return file_to_response(file, "index.html");
    }

    (axum::http::StatusCode::NOT_FOUND, "Not Found").into_response()
}

/// Convert an embedded file into an HTTP response with appropriate
/// Content-Type and Cache-Control headers.
/// `index.html` uses `no-cache` to ensure the browser always gets the
/// latest SPA entrypoint (which references versioned asset files).
/// Other files (CSS/JS with content-hash filenames) can be safely cached.
fn file_to_response(file: rust_embed::EmbeddedFile, path: &str) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let body = Body::from(file.data);
    let cache = if path == "index.html" || path.ends_with(".html") {
        "no-cache"
    } else {
        "public, max-age=86400"
    };
    Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CACHE_CONTROL, cache)
        .body(body)
        .unwrap()
}
