use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use xpressclaw_core::visualizations::VisualizationArtifact;

pub(crate) const RETRIEVAL_TOKEN_HEADER: &str = "x-xpressclaw-artifact-token";

const RESOURCE_ORIGINS: &str =
    "https://cdnjs.cloudflare.com https://esm.sh https://cdn.jsdelivr.net https://unpkg.com";
const STYLE_ORIGINS: &str = "https://cdnjs.cloudflare.com https://esm.sh https://cdn.jsdelivr.net https://unpkg.com https://fonts.googleapis.com https://fonts.bunny.net";
const FONT_ORIGINS: &str = "https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com https://fonts.gstatic.com https://fonts.bunny.net";

pub(crate) fn retrieval_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(RETRIEVAL_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 128)
}

pub(crate) fn artifact_response(
    artifact: VisualizationArtifact,
) -> Result<Response<Body>, axum::http::Error> {
    let csp = content_security_policy();
    let artifact_id = serde_json::to_string(&artifact.visualization.id)
        .unwrap_or_else(|_| "\"invalid\"".to_string());
    let prefix = VISUALIZATION_PREFIX
        .replace("__XPRESSCLAW_CSP__", &html_attribute_escape(&csp))
        .replace("__XPRESSCLAW_ARTIFACT_ID__", &artifact_id);
    let document = format!("{prefix}{}{VISUALIZATION_SUFFIX}", artifact.content);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::CONTENT_DISPOSITION, "inline")
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("content-security-policy", csp)
        .header(
            "permissions-policy",
            "accelerometer=(), autoplay=(), camera=(), clipboard-read=(), clipboard-write=(), geolocation=(), gyroscope=(), microphone=(), payment=(), usb=()",
        )
        .body(Body::from(document))
}

fn content_security_policy() -> String {
    format!(
        "default-src 'none'; script-src 'unsafe-inline' {RESOURCE_ORIGINS}; style-src 'unsafe-inline' {STYLE_ORIGINS}; font-src {FONT_ORIGINS}; img-src data: blob: {RESOURCE_ORIGINS}; media-src data: blob: {RESOURCE_ORIGINS}; connect-src 'none'; object-src 'none'; frame-src 'none'; child-src 'none'; worker-src 'none'; manifest-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'; navigate-to 'none'; sandbox allow-scripts"
    )
}

fn html_attribute_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

const VISUALIZATION_PREFIX: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="__XPRESSCLAW_CSP__">
<meta name="referrer" content="no-referrer">
<style>
:root {
  color-scheme: light dark;
  --background: #ffffff; --foreground: #171923; --card: #f7f8fb;
  --card-foreground: #171923; --popover: #ffffff; --popover-foreground: #171923;
  --primary: #242936; --primary-foreground: #ffffff; --secondary: #eef0f5;
  --secondary-foreground: #242936; --muted: #eef0f5; --muted-foreground: #5f6676;
  --accent: #e8eeff; --accent-foreground: #183e95; --destructive: #c32929;
  --border: #d8dce5; --input: #c9ced9; --ring: #3b73e8;
  --blue: #2e6fe8; --orange: #c96714; --green: #18855b; --red: #c32929;
  --purple: #7b4acb; --yellow: #a87900;
  --viz-series-1: #2e6fe8; --viz-series-2: #c96714; --viz-series-3: #18855b;
  --viz-series-4: #7b4acb; --viz-series-5: #c32929; --viz-series-6: #a87900;
  --font-size-base: 14px;
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  font-size: var(--font-size-base); background: transparent; color: var(--foreground);
}
:root[data-theme="dark"] {
  --background: #11141b; --foreground: #eef1f7; --card: #1a1f2a;
  --card-foreground: #eef1f7; --popover: #1a1f2a; --popover-foreground: #eef1f7;
  --primary: #eef1f7; --primary-foreground: #171923; --secondary: #252b38;
  --secondary-foreground: #eef1f7; --muted: #252b38; --muted-foreground: #aab1c0;
  --accent: #1e315c; --accent-foreground: #d9e5ff; --destructive: #ff7373;
  --border: #343b4a; --input: #4a5262; --ring: #79a4ff;
  --blue: #79a4ff; --orange: #f2a25f; --green: #5fd3a5; --red: #ff7373;
  --purple: #b993ff; --yellow: #e8c55b;
  --viz-series-1: #79a4ff; --viz-series-2: #f2a25f; --viz-series-3: #5fd3a5;
  --viz-series-4: #b993ff; --viz-series-5: #ff7373; --viz-series-6: #e8c55b;
}
* { box-sizing: border-box; }
html, body { margin: 0; min-width: 0; background: transparent; color: var(--foreground); }
body { padding: 12px; overflow-x: hidden; }
img, svg, canvas { max-width: 100%; }
button, input, select, textarea { font: inherit; }
button, a, input, select, textarea { outline-color: var(--ring); }
h1, h2, h3 { margin: 0 0 .65rem; font-weight: 500; line-height: 1.25; }
p { margin: .5rem 0; }
a { color: var(--blue); }
.card { padding: 12px; color: var(--card-foreground); background: var(--card); border: 1px solid var(--border); border-radius: 10px; }
.viz-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(180px, 100%), 1fr)); gap: 10px; }
.viz-row, .viz-controls { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.viz-stat-value { font-size: 1.5rem; font-weight: 500; line-height: 1.2; }
.viz-badge { display: inline-flex; align-items: center; padding: 2px 7px; border-radius: 999px; background: var(--accent); color: var(--accent-foreground); }
.btn { display: inline-flex; align-items: center; justify-content: center; min-height: 34px; padding: 6px 11px; border: 1px solid var(--border); border-radius: 7px; background: var(--secondary); color: var(--secondary-foreground); cursor: pointer; text-decoration: none; }
.btn:hover { filter: brightness(.97); }
.btn-primary { background: var(--primary); color: var(--primary-foreground); border-color: var(--primary); }
.btn-ghost { background: transparent; }
.btn-block { width: 100%; }
.viz-tile { min-height: 70px; }
.btn[aria-pressed="true"], .btn[aria-selected="true"], .btn.is-selected { box-shadow: 0 0 0 2px var(--ring); }
.form-label { display: grid; gap: 5px; color: var(--foreground); }
.form-control, .form-select { width: 100%; min-height: 34px; padding: 6px 8px; border: 1px solid var(--input); border-radius: 7px; background: var(--background); color: var(--foreground); }
.form-check { display: inline-flex; align-items: center; gap: 6px; }
.table-responsive { overflow-x: auto; }
.table { width: 100%; border-collapse: collapse; }
.table th, .table td { padding: 8px; border-bottom: 1px solid var(--border); text-align: left; vertical-align: top; }
.table-sm th, .table-sm td { padding: 5px 7px; }
.text-small { font-size: max(11px, .82rem); }
.text-muted { color: var(--muted-foreground); }
.text-destructive { color: var(--destructive); }
.text-end { text-align: end !important; font-variant-numeric: tabular-nums; }
.text-center { text-align: center !important; }
.text-nowrap { white-space: nowrap; }
.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0,0,0,0); white-space: nowrap; border: 0; }
.tooltip { position: fixed; z-index: 1000; max-width: 280px; padding: 6px 8px; border: 1px solid var(--border); border-radius: 6px; background: var(--popover); color: var(--popover-foreground); pointer-events: none; box-shadow: 0 8px 24px color-mix(in srgb, var(--foreground) 14%, transparent); }
@media (prefers-reduced-motion: reduce) { *, *::before, *::after { animation-duration: .001ms !important; animation-iteration-count: 1 !important; transition-duration: .001ms !important; scroll-behavior: auto !important; } }
</style>
<script>
(() => {
  "use strict";
  const artifactId = __XPRESSCLAW_ARTIFACT_ID__;
  const pending = new Map();
  const cleanText = (value, max) => typeof value === "string" && value.length <= max && !/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/.test(value);
  const requestId = () => {
    if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    return Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");
  };
  const sendFollowUpMessage = (value) => {
    if (!value || Object.getPrototypeOf(value) !== Object.prototype || !cleanText(value.prompt, 20000) || !value.prompt.trim()) {
      return Promise.reject(new TypeError("sendFollowUpMessage requires a non-empty prompt"));
    }
    if (value.title !== undefined && (!cleanText(value.title, 250) || !value.title.trim())) {
      return Promise.reject(new TypeError("sendFollowUpMessage title must be non-empty and at most 250 characters"));
    }
    if (pending.size >= 3) return Promise.reject(new Error("too many pending follow-up requests"));
    const id = requestId();
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      parent.postMessage({ source: "xpressclaw-visualization", type: "follow-up-request", artifactId, requestId: id, prompt: value.prompt, title: value.title || null }, "*");
    });
  };
  Object.defineProperty(window, "openai", {
    configurable: false,
    enumerable: true,
    writable: false,
    value: Object.freeze({ sendFollowUpMessage })
  });
  const reportHeight = () => parent.postMessage({ source: "xpressclaw-visualization", type: "resize", artifactId, height: Math.ceil(document.documentElement.scrollHeight) }, "*");
  addEventListener("message", (event) => {
    if (event.source !== parent || !event.data || event.data.source !== "xpressclaw-host" || event.data.artifactId !== artifactId) return;
    if (event.data.type === "theme" && (event.data.theme === "light" || event.data.theme === "dark")) {
      document.documentElement.dataset.theme = event.data.theme;
      reportHeight();
    }
    if (event.data.type === "follow-up-result" && cleanText(event.data.requestId, 100)) {
      const request = pending.get(event.data.requestId);
      if (!request) return;
      pending.delete(event.data.requestId);
      if (event.data.ok) request.resolve();
      else request.reject(new Error(cleanText(event.data.error, 500) ? event.data.error : "follow-up was cancelled"));
    }
  });
  addEventListener("DOMContentLoaded", () => {
    new ResizeObserver(reportHeight).observe(document.documentElement);
    reportHeight();
  });
})();
</script>
</head>
<body>
"#;

const VISUALIZATION_SUFFIX: &str = r#"
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use xpressclaw_core::visualizations::MessageVisualization;

    #[test]
    fn wrapper_enforces_the_visualization_security_boundary() {
        let response = artifact_response(VisualizationArtifact {
            visualization: MessageVisualization {
                id: "viz-test".into(),
                reference_index: 0,
                title: "Test".into(),
                mode: "normal".into(),
                status: "ready".into(),
                error_code: None,
                size: Some(31),
                retrieval_token: "token".into(),
            },
            content: "<button onclick=\"openai.sendFollowUpMessage({prompt:'go'})\">Go</button>"
                .into(),
        })
        .unwrap();
        let csp = response
            .headers()
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(csp.contains("connect-src 'none'"));
        assert!(csp.contains("form-action 'none'"));
        assert!(csp.contains("object-src 'none'"));
        assert!(csp.contains("frame-src 'none'"));
        assert!(csp.contains("worker-src 'none'"));
        assert!(csp.contains("navigate-to 'none'"));
        assert!(csp.contains("sandbox allow-scripts"));
        assert!(csp.contains("frame-ancestors 'self'"));
        for origin in [
            "https://cdnjs.cloudflare.com",
            "https://esm.sh",
            "https://cdn.jsdelivr.net",
            "https://unpkg.com",
            "https://fonts.googleapis.com",
            "https://fonts.gstatic.com",
            "https://fonts.bunny.net",
        ] {
            assert!(csp.contains(origin), "missing allowed origin {origin}");
        }
        assert!(!csp
            .split(';')
            .find(|directive| directive.trim_start().starts_with("script-src"))
            .unwrap()
            .contains("'self'"));
        assert_eq!(
            response.headers().get(header::REFERRER_POLICY).unwrap(),
            "no-referrer"
        );
    }

    #[test]
    fn retrieval_token_header_is_strict_and_bounded() {
        let mut headers = HeaderMap::new();
        assert_eq!(retrieval_token(&headers), None);
        headers.insert(RETRIEVAL_TOKEN_HEADER, " token ".parse().unwrap());
        assert_eq!(retrieval_token(&headers), Some("token"));
        headers.insert(RETRIEVAL_TOKEN_HEADER, "x".repeat(129).parse().unwrap());
        assert_eq!(retrieval_token(&headers), None);
    }
}
