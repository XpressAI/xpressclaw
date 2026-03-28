use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

/// Embedded SvelteKit build output.
///
/// At compile time, rust-embed includes all files from the `build/` directory
/// into the binary. In debug builds with the `build/` directory absent, this
/// gracefully returns 404 for all assets.
#[derive(Embed)]
#[folder = "../../frontend/build/"]
#[prefix = ""]
struct FrontendAssets;

/// Log how many frontend assets are embedded (debug diagnostic).
pub fn log_frontend_status() {
    let count = FrontendAssets::iter().count();
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    if count > 0 {
        tracing::info!(count, manifest_dir, "frontend assets embedded");
    } else {
        tracing::warn!(manifest_dir, "NO frontend assets embedded — UI will show 'frontend not built'");
        // List first few files if iter works
        for (i, name) in FrontendAssets::iter().enumerate() {
            tracing::warn!(file = %name, "embedded file");
            if i >= 5 {
                break;
            }
        }
    }
}

/// Axum handler that serves embedded static files.
///
/// For SPA routing: if the requested path doesn't match a static file,
/// serve `index.html` so SvelteKit's client-side router handles it.
pub async fn serve_frontend(req: Request<Body>) -> impl IntoResponse {
    let path = req.uri().path().trim_start_matches('/');

    // Try the exact path first
    if let Some(file) = FrontendAssets::get(path) {
        return serve_file(path, &file.data);
    }

    // Try with index.html appended (for directory paths)
    let index_path = if path.is_empty() {
        "index.html".to_string()
    } else {
        format!("{path}/index.html")
    };
    if let Some(file) = FrontendAssets::get(&index_path) {
        return serve_file(&index_path, &file.data);
    }

    // SPA fallback: serve index.html for client-side routing
    if let Some(file) = FrontendAssets::get("index.html") {
        return serve_file("index.html", &file.data);
    }

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("frontend not built"))
        .unwrap()
}

fn serve_file(path: &str, data: &[u8]) -> Response<Body> {
    let mime = mime_type(path);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(
            header::CACHE_CONTROL,
            if path.contains("immutable") {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=0, must-revalidate"
            },
        )
        .body(Body::from(data.to_vec()))
        .unwrap()
}

fn mime_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        _ => "application/octet-stream",
    }
}
