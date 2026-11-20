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
        tracing::warn!(
            manifest_dir,
            "NO frontend assets embedded — UI will show 'frontend not built'"
        );
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
    let if_none_match = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());

    // An unmatched API path is a client bug, not a page. Answering with the
    // SPA's index.html (200, text/html) surfaced only as a JSON parse error
    // in the UI, e.g. a trailing slash that the nested API router rejects.
    if path == "api" || path.starts_with("api/") {
        return crate::routes::auth::error_response(StatusCode::NOT_FOUND, "API route not found");
    }

    // Try the exact path first
    if let Some(file) = FrontendAssets::get(path) {
        return serve_embedded_file(path, &file, if_none_match);
    }

    // Try with index.html appended (for directory paths)
    let index_path = if path.is_empty() {
        "index.html".to_string()
    } else {
        format!("{path}/index.html")
    };
    if let Some(file) = FrontendAssets::get(&index_path) {
        return serve_embedded_file(&index_path, &file, if_none_match);
    }

    // SPA fallback: serve index.html for client-side routing
    if let Some(file) = FrontendAssets::get("index.html") {
        return serve_embedded_file("index.html", &file, if_none_match);
    }

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("frontend not built"))
        .unwrap()
}

fn serve_embedded_file(
    path: &str,
    file: &rust_embed::EmbeddedFile,
    if_none_match: Option<&str>,
) -> Response<Body> {
    let etag = format_etag(&file.metadata.sha256_hash());
    serve_bytes(path, &file.data, &etag, if_none_match)
}

fn serve_bytes(path: &str, data: &[u8], etag: &str, if_none_match: Option<&str>) -> Response<Body> {
    let cache_control = if path.contains("immutable") {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=0, must-revalidate"
    };
    if if_none_match.is_some_and(|header_val| etag_matches(header_val, etag)) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, cache_control)
            .body(Body::empty())
            .unwrap();
    }

    let mime = mime_type(path);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, cache_control)
        .body(Body::from(data.to_vec()))
        .unwrap()
}

fn format_etag(hash: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(66);
    out.push('"');
    for byte in hash {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out.push('"');
    out
}

fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    let trimmed = if_none_match.trim();
    if trimmed == "*" {
        return true;
    }
    trimmed.split(',').any(|candidate| {
        let candidate = candidate.trim();
        let normalized = candidate.strip_prefix("W/").unwrap_or(candidate).trim();
        normalized == etag
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_bytes_emits_etag_and_honors_if_none_match() {
        let hash = [0xab_u8; 32];
        let etag = format_etag(&hash);
        let initial = serve_bytes("index.html", b"<!doctype html>", &etag, None);
        assert_eq!(initial.status(), StatusCode::OK);
        assert_eq!(initial.headers()[header::ETAG], etag);
        assert_eq!(
            initial.headers()[header::CACHE_CONTROL],
            "public, max-age=0, must-revalidate"
        );

        let revalidated = serve_bytes("index.html", b"<!doctype html>", &etag, Some(&etag));
        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(revalidated.headers()[header::ETAG], etag);

        let weak_tag = format!("W/{etag}");
        let weak_revalidated =
            serve_bytes("index.html", b"<!doctype html>", &etag, Some(&weak_tag));
        assert_eq!(weak_revalidated.status(), StatusCode::NOT_MODIFIED);

        let stale = serve_bytes("index.html", b"<!doctype html>", &etag, Some("\"other\""));
        assert_eq!(stale.status(), StatusCode::OK);
    }
}
