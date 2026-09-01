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

/// IPC command: open an HTTP(S) URL in the Desktop user's browser.
#[tauri::command]
pub async fn open_external_url(
    webview: tauri::WebviewWindow,
    state: tauri::State<'_, ProfileState>,
    url: String,
) -> Result<(), String> {
    crate::profiles::verify_active_profile_identity(&state, &webview).await?;
    let url = external_http_url(&url)?;
    open::that(url.as_str()).map_err(|e| e.to_string())
}

fn external_http_url(url: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(url).map_err(|_| "Invalid external URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Only HTTP and HTTPS links can be opened".to_string());
    }
    Ok(url)
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

#[cfg(test)]
mod tests {
    use super::external_http_url;

    #[test]
    fn external_urls_are_limited_to_http_and_https() {
        assert!(external_http_url("https://registry.modelcontextprotocol.io/").is_ok());
        assert!(external_http_url("http://localhost:8935/docs").is_ok());
        assert!(external_http_url("file:///etc/passwd").is_err());
        assert!(external_http_url("javascript:alert(1)").is_err());
        assert!(external_http_url("not a url").is_err());
    }
}
