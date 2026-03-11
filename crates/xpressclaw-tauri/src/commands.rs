use serde_json::{json, Value};

use crate::DEFAULT_PORT;

/// IPC command: check server health.
#[tauri::command]
pub async fn get_health() -> Result<Value, String> {
    let port = std::env::var("XPRESSCLAW_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let url = format!("http://localhost:{port}/api/health");
    let resp = reqwest::get(&url).await.map_err(|e| e.to_string())?;
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body)
}

/// IPC command: return the server port.
#[tauri::command]
pub fn get_server_port() -> u16 {
    std::env::var("XPRESSCLAW_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// IPC command: open the web UI in the default browser.
#[tauri::command]
pub fn open_browser() -> Result<(), String> {
    let port = get_server_port();
    let url = format!("http://localhost:{port}");
    open::that(&url).map_err(|e| e.to_string())
}

/// IPC command: get server status summary.
#[tauri::command]
pub async fn get_status() -> Result<Value, String> {
    let port = std::env::var("XPRESSCLAW_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let base = format!("http://localhost:{port}/api");
    let client = reqwest::Client::new();

    let health: Value = client
        .get(format!("{base}/health"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let agents: Value = client
        .get(format!("{base}/agents"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "health": health,
        "agents": agents,
    }))
}
