use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/run", post(run_office_script))
        .route("/read", post(read_document))
        .route("/export", post(export_document))
        .route("/documents", get(list_documents))
        .route("/documents/{name}", get(download_document))
        .route("/documents/{name}/content", get(read_document_content))
        .route("/upload", post(upload_document))
        .route("/documents/{name}/delete", post(delete_document))
}

/// Get the documents directory for an agent (~/.xpressclaw/{agent_id}/documents/).
/// Each agent has its own documents directory to prevent cross-agent access.
fn documents_dir(state: &AppState, agent_id: &str) -> std::path::PathBuf {
    let dir = state
        .config_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(agent_id)
        .join("documents");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[derive(Debug, Deserialize)]
struct RunScriptRequest {
    /// Office app: "word", "excel", "powerpoint"
    app: String,
    /// Script to execute (AppleScript on macOS, PowerShell on Windows)
    script: String,
    /// Document name (resolved to ~/.xpressclaw/{agent_id}/documents/{name})
    file_name: Option<String>,
    /// Agent ID (for per-agent document directory)
    agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadDocumentRequest {
    file_name: String,
    agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExportDocumentRequest {
    file_name: String,
    format: String,
    output_name: Option<String>,
    agent_id: Option<String>,
}

async fn run_office_script(
    State(state): State<AppState>,
    Json(req): Json<RunScriptRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let platform = std::env::consts::OS;
    let agent_id = req.agent_id.as_deref().unwrap_or("default");
    let docs_dir = documents_dir(&state, agent_id);

    let file_path = req
        .file_name
        .as_ref()
        .map(|name| docs_dir.join(name).to_string_lossy().to_string());

    // Inject the documents directory path into the script as a variable
    // so the agent can reference it without knowing the absolute path
    let docs_path = docs_dir.to_string_lossy().to_string();
    let script = req
        .script
        .replace("$DOCUMENTS_DIR", &docs_path)
        .replace("${DOCUMENTS_DIR}", &docs_path);

    let result = match platform {
        "macos" => run_applescript(&req.app, &script, file_path.as_deref()).await,
        "windows" => run_powershell(&req.app, &script, file_path.as_deref()).await,
        "linux" => Err("Office automation requires macOS or Windows.".to_string()),
        _ => Err(format!("Unsupported platform: {platform}")),
    };

    match result {
        Ok(output) => {
            info!(app = %req.app, "office script executed");
            Ok(Json(json!({
                "success": true,
                "output": output,
                "documents_dir": docs_path,
            })))
        }
        Err(e) => {
            warn!(app = %req.app, error = %e, "office script failed");
            Ok(Json(json!({ "success": false, "error": e })))
        }
    }
}

async fn read_document(
    State(state): State<AppState>,
    Json(req): Json<ReadDocumentRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let platform = std::env::consts::OS;
    let agent_id = req.agent_id.as_deref().unwrap_or("default");
    let docs_dir = documents_dir(&state, agent_id);
    let file_path = docs_dir.join(&req.file_name).to_string_lossy().to_string();

    if !docs_dir.join(&req.file_name).exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Document '{}' not found", req.file_name) })),
        ));
    }

    let ext = std::path::Path::new(&req.file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let app = match ext.as_str() {
        "doc" | "docx" | "rtf" => "word",
        "xls" | "xlsx" | "csv" => "excel",
        "ppt" | "pptx" => "powerpoint",
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Unsupported file type: .{ext}") })),
            ))
        }
    };

    let script = match platform {
        "macos" => generate_read_applescript(app, &file_path),
        "windows" => generate_read_powershell(app, &file_path),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Unsupported platform" })),
            ))
        }
    };

    let result = match platform {
        "macos" => run_applescript(app, &script, Some(&file_path)).await,
        "windows" => run_powershell(app, &script, Some(&file_path)).await,
        _ => Err("Unsupported".to_string()),
    };

    match result {
        Ok(content) => Ok(Json(json!({ "content": content, "file": req.file_name }))),
        Err(e) => Ok(Json(json!({ "success": false, "error": e }))),
    }
}

async fn export_document(
    State(state): State<AppState>,
    Json(req): Json<ExportDocumentRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let platform = std::env::consts::OS;
    let agent_id = req.agent_id.as_deref().unwrap_or("default");
    let docs_dir = documents_dir(&state, agent_id);
    let file_path = docs_dir.join(&req.file_name).to_string_lossy().to_string();

    let ext = std::path::Path::new(&req.file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let app = match ext.as_str() {
        "doc" | "docx" | "rtf" => "word",
        "xls" | "xlsx" => "excel",
        "ppt" | "pptx" => "powerpoint",
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Unsupported: .{ext}") })),
            ))
        }
    };

    let output_name = req.output_name.unwrap_or_else(|| {
        let p = std::path::Path::new(&req.file_name);
        p.with_extension(&req.format)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });
    let output_path = docs_dir.join(&output_name).to_string_lossy().to_string();

    let script = match platform {
        "macos" => generate_export_applescript(app, &file_path, &output_path, &req.format),
        "windows" => generate_export_powershell(app, &file_path, &output_path, &req.format),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Unsupported platform" })),
            ))
        }
    };

    let result = match platform {
        "macos" => run_applescript(app, &script, Some(&file_path)).await,
        "windows" => run_powershell(app, &script, Some(&file_path)).await,
        _ => Err("Unsupported".to_string()),
    };

    match result {
        Ok(_) => Ok(Json(
            json!({ "exported": output_name, "format": req.format }),
        )),
        Err(e) => Ok(Json(json!({ "success": false, "error": e }))),
    }
}

#[derive(Debug, Deserialize)]
struct DocumentsQuery {
    agent_id: Option<String>,
}

/// List all documents in the agent's documents directory.
async fn list_documents(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<DocumentsQuery>,
) -> Json<Vec<Value>> {
    let agent_id = query.agent_id.as_deref().unwrap_or("default");
    let docs_dir = documents_dir(&state, agent_id);
    let mut docs = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&docs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                docs.push(json!({
                    "name": name,
                    "size": size,
                    "url": format!("/api/office/documents/{}", name),
                }));
            }
        }
    }

    Json(docs)
}

/// Download a document by name.
async fn download_document(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DocumentsQuery>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let agent_id = query.agent_id.as_deref().unwrap_or("default");
    let docs_dir = documents_dir(&state, agent_id);
    let file_path = docs_dir.join(&name);

    if !file_path.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Document not found" })),
        ));
    }

    let bytes = std::fs::read(&file_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let content_type = match file_path.extension().and_then(|e| e.to_str()) {
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("pdf") => "application/pdf",
        Some("doc") => "application/msword",
        Some("xls") => "application/vnd.ms-excel",
        Some("ppt") => "application/vnd.ms-powerpoint",
        _ => "application/octet-stream",
    };

    Ok(axum::response::Response::builder()
        .header("content-type", content_type)
        .header(
            "content-disposition",
            format!("attachment; filename=\"{name}\""),
        )
        .body(axum::body::Body::from(bytes))
        .unwrap())
}

/// Read a text document's content inline (for the workspace file viewer).
async fn read_document_content(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DocumentsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let agent_id = query.agent_id.as_deref().unwrap_or("default");
    let docs_dir = documents_dir(&state, agent_id);
    let file_path = docs_dir.join(&name);

    if !file_path.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "File not found" })),
        ));
    }

    let content = std::fs::read_to_string(&file_path).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": format!("Cannot read as text: {e}") })),
        )
    })?;

    Ok(Json(json!({ "name": name, "content": content })))
}

/// Upload a file to the agent's documents directory.
async fn upload_document(
    State(state): State<AppState>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut agent_id = "default".to_string();
    let mut files_saved = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| internal_error(e))?
    {
        let field_name = field.name().unwrap_or("").to_string();

        if field_name == "agent_id" {
            agent_id = field.text().await.map_err(|e| internal_error(e))?;
            continue;
        }

        if field_name == "file" {
            let file_name = field.file_name().unwrap_or("upload").to_string();
            let data = field.bytes().await.map_err(|e| internal_error(e))?;

            let docs_dir = documents_dir(&state, &agent_id);
            let dest = docs_dir.join(&file_name);
            std::fs::write(&dest, &data).map_err(|e| internal_error(e))?;

            info!(agent_id, file_name, size = data.len(), "file uploaded");
            files_saved.push(json!({
                "name": file_name,
                "size": data.len(),
            }));
        }
    }

    Ok(Json(json!({ "files": files_saved })))
}

/// Delete a document from the agent's documents directory.
async fn delete_document(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DocumentsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let agent_id = query.agent_id.as_deref().unwrap_or("default");
    let docs_dir = documents_dir(&state, agent_id);
    let file_path = docs_dir.join(&name);

    if !file_path.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "File not found" })),
        ));
    }

    std::fs::remove_file(&file_path).map_err(|e| internal_error(e))?;
    info!(agent_id, name, "document deleted");

    Ok(Json(json!({ "deleted": name })))
}

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

// ---------------------------------------------------------------------------
// macOS: AppleScript execution
// ---------------------------------------------------------------------------

async fn run_applescript(
    app: &str,
    script: &str,
    _file_path: Option<&str>,
) -> Result<String, String> {
    let office_app = match app {
        "word" => "Microsoft Word",
        "excel" => "Microsoft Excel",
        "powerpoint" => "Microsoft PowerPoint",
        _ => return Err(format!("Unknown app: {app}")),
    };

    // Always ensure `activate` is present so the app launches if not running.
    // Agents often send `tell application "..."` but forget `activate`.
    let full_script = if script.contains("tell application") {
        if script.contains("activate") {
            script.to_string()
        } else {
            // Inject `activate` right after the `tell application "X"` line
            if let Some(pos) = script.find('\n') {
                let (first_line, rest) = script.split_at(pos);
                format!("{first_line}\n  activate{rest}")
            } else {
                script.to_string()
            }
        }
    } else {
        format!("tell application \"{office_app}\"\n  activate\n  {script}\nend tell")
    };

    let output = tokio::process::Command::new("osascript")
        .arg("-e")
        .arg(&full_script)
        .output()
        .await
        .map_err(|e| format!("Failed to run osascript: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        Ok(stdout)
    } else if !stdout.is_empty() {
        Ok(format!("{stdout}\n\n[Warning: {stderr}]"))
    } else {
        Err(format!("AppleScript error: {stderr}"))
    }
}

// ---------------------------------------------------------------------------
// Windows: PowerShell/COM execution
// ---------------------------------------------------------------------------

async fn run_powershell(
    app: &str,
    script: &str,
    _file_path: Option<&str>,
) -> Result<String, String> {
    let com_app = match app {
        "word" => "Word.Application",
        "excel" => "Excel.Application",
        "powerpoint" => "PowerPoint.Application",
        _ => return Err(format!("Unknown app: {app}")),
    };

    let full_script = if script.contains("New-Object") || script.contains("$app") {
        script.to_string()
    } else {
        format!(
            "$app = New-Object -ComObject {com_app}\n$app.Visible = $true\n{script}\n[System.Runtime.Interopservices.Marshal]::ReleaseComObject($app) | Out-Null"
        )
    };

    let output = tokio::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &full_script])
        .output()
        .await
        .map_err(|e| format!("Failed to run PowerShell: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        Ok(stdout)
    } else if !stdout.is_empty() {
        Ok(format!("{stdout}\n\n[Warning: {stderr}]"))
    } else {
        Err(format!("PowerShell error: {stderr}"))
    }
}

// ---------------------------------------------------------------------------
// Script generators
// ---------------------------------------------------------------------------

fn generate_read_applescript(app: &str, file_path: &str) -> String {
    match app {
        "word" => format!(
            r#"tell application "Microsoft Word"
  open POSIX file "{file_path}"
  set docText to content of text object of active document
  close active document saving no
  return docText
end tell"#
        ),
        "excel" => format!(
            r#"tell application "Microsoft Excel"
  open POSIX file "{file_path}"
  set wb to active workbook
  set result to ""
  repeat with ws in worksheets of wb
    set result to result & "=== Sheet: " & name of ws & " ===" & return
    set usedRange to used range of ws
    repeat with r in rows of usedRange
      set rowText to ""
      repeat with c in cells of r
        set rowText to rowText & (value of c as text) & tab
      end repeat
      set result to result & rowText & return
    end repeat
  end repeat
  close wb saving no
  return result
end tell"#
        ),
        "powerpoint" => format!(
            r#"tell application "Microsoft PowerPoint"
  open POSIX file "{file_path}"
  set pres to active presentation
  set result to ""
  repeat with s in slides of pres
    set result to result & "=== Slide " & (slide index of s) & " ===" & return
    repeat with sh in shapes of s
      if has text frame of sh then
        set result to result & content of text range of text frame of sh & return
      end if
    end repeat
  end repeat
  close pres saving no
  return result
end tell"#
        ),
        _ => String::new(),
    }
}

fn generate_read_powershell(app: &str, file_path: &str) -> String {
    match app {
        "word" => format!(
            r#"$app = New-Object -ComObject Word.Application
$doc = $app.Documents.Open("{file_path}")
$text = $doc.Content.Text
$doc.Close($false)
$app.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($app) | Out-Null
Write-Output $text"#
        ),
        "excel" => format!(
            r#"$app = New-Object -ComObject Excel.Application
$wb = $app.Workbooks.Open("{file_path}")
$result = ""
foreach ($ws in $wb.Worksheets) {{
  $result += "=== Sheet: $($ws.Name) ===`n"
  $range = $ws.UsedRange
  for ($r = 1; $r -le $range.Rows.Count; $r++) {{
    $row = ""
    for ($c = 1; $c -le $range.Columns.Count; $c++) {{
      $row += $range.Cells($r, $c).Text + "`t"
    }}
    $result += $row + "`n"
  }}
}}
$wb.Close($false)
$app.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($app) | Out-Null
Write-Output $result"#
        ),
        _ => String::new(),
    }
}

fn generate_export_applescript(
    app: &str,
    file_path: &str,
    output_path: &str,
    format: &str,
) -> String {
    match (app, format) {
        ("word", "pdf") => format!(
            r#"tell application "Microsoft Word"
  open POSIX file "{file_path}"
  save as active document file name POSIX file "{output_path}" file format format PDF
  close active document saving no
end tell"#
        ),
        ("excel", "pdf") => format!(
            r#"tell application "Microsoft Excel"
  open POSIX file "{file_path}"
  save active workbook in POSIX file "{output_path}" as PDF file format
  close active workbook saving no
end tell"#
        ),
        ("powerpoint", "pdf") => format!(
            r#"tell application "Microsoft PowerPoint"
  open POSIX file "{file_path}"
  save active presentation in POSIX file "{output_path}" as save as PDF
  close active presentation saving no
end tell"#
        ),
        _ => format!("-- Export to {format} not directly supported for {app}"),
    }
}

fn generate_export_powershell(
    app: &str,
    file_path: &str,
    output_path: &str,
    format: &str,
) -> String {
    match (app, format) {
        ("word", "pdf") => format!(
            r#"$app = New-Object -ComObject Word.Application
$doc = $app.Documents.Open("{file_path}")
$doc.SaveAs2("{output_path}", 17)
$doc.Close($false)
$app.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($app) | Out-Null"#
        ),
        ("excel", "pdf") => format!(
            r#"$app = New-Object -ComObject Excel.Application
$wb = $app.Workbooks.Open("{file_path}")
$wb.ExportAsFixedFormat(0, "{output_path}")
$wb.Close($false)
$app.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($app) | Out-Null"#
        ),
        _ => String::new(),
    }
}
