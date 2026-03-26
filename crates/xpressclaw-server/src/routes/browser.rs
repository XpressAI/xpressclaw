use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/run", post(run_browser_script))
        .route("/screenshot", post(take_screenshot))
        .route("/fetch", post(fetch_page))
}

/// Per-agent screenshots directory.
fn screenshots_dir(state: &AppState, agent_id: &str) -> std::path::PathBuf {
    let dir = state
        .config_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(agent_id)
        .join("screenshots");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[derive(Debug, Deserialize)]
struct BrowserScriptRequest {
    /// Python script using playwright.sync_api
    script: String,
    agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScreenshotRequest {
    /// URL to screenshot
    url: String,
    /// Output file name (e.g. "page.png")
    file_name: Option<String>,
    /// Wait for selector before screenshot
    wait_for: Option<String>,
    /// Full page screenshot
    full_page: Option<bool>,
    agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FetchPageRequest {
    /// URL to fetch
    url: String,
    /// CSS selector to extract text from (optional, extracts full page if omitted)
    selector: Option<String>,
    /// Wait for selector before extracting
    wait_for: Option<String>,
}

async fn run_browser_script(
    State(state): State<AppState>,
    Json(req): Json<BrowserScriptRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let agent_id = req.agent_id.as_deref().unwrap_or("default");
    let screenshots = screenshots_dir(&state, agent_id);
    let screenshots_path = screenshots.to_string_lossy().to_string();

    // Inject $SCREENSHOTS_DIR into the script
    let script = req
        .script
        .replace("$SCREENSHOTS_DIR", &screenshots_path)
        .replace("${SCREENSHOTS_DIR}", &screenshots_path);

    let result = run_python_playwright(&script).await;

    match result {
        Ok(output) => {
            info!("browser script executed");
            Ok(Json(json!({
                "success": true,
                "output": output,
                "screenshots_dir": screenshots_path,
            })))
        }
        Err(e) => {
            warn!(error = %e, "browser script failed");
            Ok(Json(json!({ "success": false, "error": e })))
        }
    }
}

async fn take_screenshot(
    State(state): State<AppState>,
    Json(req): Json<ScreenshotRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let agent_id = req.agent_id.as_deref().unwrap_or("default");
    let screenshots = screenshots_dir(&state, agent_id);
    let file_name = req
        .file_name
        .unwrap_or_else(|| "screenshot.png".to_string());
    let output_path = screenshots.join(&file_name).to_string_lossy().to_string();
    let full_page = req.full_page.unwrap_or(false);

    let wait_code = req
        .wait_for
        .as_ref()
        .map(|s| format!("page.wait_for_selector('{s}', timeout=10000)"))
        .unwrap_or_default();

    let script = format!(
        r#"
from playwright.sync_api import sync_playwright
with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page()
    page.goto("{url}", wait_until="networkidle")
    {wait_code}
    page.screenshot(path="{output_path}", full_page={full_page_py})
    browser.close()
print("saved")
"#,
        url = req.url,
        output_path = output_path,
        full_page_py = if full_page { "True" } else { "False" },
    );

    let result = run_python_playwright(&script).await;

    match result {
        Ok(_) => Ok(Json(json!({
            "success": true,
            "file": file_name,
            "path": output_path,
        }))),
        Err(e) => Ok(Json(json!({ "success": false, "error": e }))),
    }
}

async fn fetch_page(
    Json(req): Json<FetchPageRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let wait_code = req
        .wait_for
        .as_ref()
        .map(|s| format!("page.wait_for_selector('{s}', timeout=10000)"))
        .unwrap_or_default();

    let extract_code = if let Some(ref selector) = req.selector {
        format!("page.text_content('{selector}') or ''")
    } else {
        "page.content()".to_string()
    };

    let script = format!(
        r#"
from playwright.sync_api import sync_playwright
with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page()
    page.goto("{url}", wait_until="networkidle")
    {wait_code}
    content = {extract_code}
    browser.close()
print(content)
"#,
        url = req.url,
    );

    let result = run_python_playwright(&script).await;

    match result {
        Ok(content) => Ok(Json(json!({
            "success": true,
            "content": content,
            "url": req.url,
        }))),
        Err(e) => Ok(Json(json!({ "success": false, "error": e }))),
    }
}

/// Execute a Python script that uses playwright.sync_api.
async fn run_python_playwright(script: &str) -> Result<String, String> {
    let output = tokio::process::Command::new("python3")
        .args(["-c", script])
        .output()
        .await
        .map_err(|e| format!("Failed to run python3: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        Ok(stdout)
    } else if !stdout.is_empty() {
        Ok(format!("{stdout}\n\n[Warning: {stderr}]"))
    } else {
        Err(format!("Playwright error: {stderr}"))
    }
}
