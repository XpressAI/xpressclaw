use serde_json::{json, Value};

use crate::profiles::ProfileState;

/// IPC command: check server health.
#[tauri::command]
pub async fn get_health(state: tauri::State<'_, ProfileState>) -> Result<Value, String> {
    let url = format!("{}/api/health", state.active_url()?);
    let resp = crate::profiles::http_client()?
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body)
}

/// IPC command: return the server port.
#[tauri::command]
pub fn get_server_port() -> u16 {
    crate::local_server_port()
}

/// IPC command: open the web UI in the default browser.
#[tauri::command]
pub fn open_browser(state: tauri::State<'_, ProfileState>) -> Result<(), String> {
    let url = state.active_url()?;
    open::that(&url).map_err(|e| e.to_string())
}

/// IPC command: get server status summary.
#[tauri::command]
pub async fn get_status(state: tauri::State<'_, ProfileState>) -> Result<Value, String> {
    let base = format!("{}/api", state.active_url()?);
    let client = crate::profiles::http_client()?;

    let health: Value = client
        .get(format!("{base}/health"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let authentication: Value = client
        .get(format!("{base}/auth/bootstrap"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    if authentication["authentication_enabled"].as_bool() == Some(true) {
        return Ok(json!({
            "health": health,
            "agents": null,
            "authentication": authentication,
        }));
    }

    let agents: Value = client
        .get(format!("{base}/agents"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "health": health,
        "agents": agents,
        "authentication": authentication,
    }))
}
