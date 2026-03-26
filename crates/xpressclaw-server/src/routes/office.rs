use axum::http::StatusCode;
use axum::routing::post;
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
}

#[derive(Debug, Deserialize)]
struct RunScriptRequest {
    /// Office app: "word", "excel", "powerpoint"
    app: String,
    /// Script to execute (AppleScript on macOS, PowerShell on Windows)
    script: String,
    /// Optional file path to operate on
    file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadDocumentRequest {
    file_path: String,
}

#[derive(Debug, Deserialize)]
struct ExportDocumentRequest {
    file_path: String,
    format: String, // "pdf", "html", etc.
    output_path: Option<String>,
}

async fn run_office_script(
    Json(req): Json<RunScriptRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let platform = std::env::consts::OS;

    let result = match platform {
        "macos" => run_applescript(&req.app, &req.script, req.file_path.as_deref()).await,
        "windows" => run_powershell(&req.app, &req.script, req.file_path.as_deref()).await,
        "linux" => Err("Office automation requires macOS or Windows. Linux support via LibreOffice CLI is planned.".to_string()),
        _ => Err(format!("Unsupported platform: {platform}")),
    };

    match result {
        Ok(output) => {
            info!(app = %req.app, "office script executed");
            Ok(Json(json!({ "success": true, "output": output })))
        }
        Err(e) => {
            warn!(app = %req.app, error = %e, "office script failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            ))
        }
    }
}

async fn read_document(
    Json(req): Json<ReadDocumentRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let platform = std::env::consts::OS;
    let ext = std::path::Path::new(&req.file_path)
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
        "macos" => generate_read_applescript(app, &req.file_path),
        "windows" => generate_read_powershell(app, &req.file_path),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Unsupported platform" })),
            ))
        }
    };

    let result = match platform {
        "macos" => run_applescript(app, &script, Some(&req.file_path)).await,
        "windows" => run_powershell(app, &script, Some(&req.file_path)).await,
        _ => Err("Unsupported platform".to_string()),
    };

    match result {
        Ok(content) => Ok(Json(
            json!({ "content": content, "file": req.file_path, "app": app }),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )),
    }
}

async fn export_document(
    Json(req): Json<ExportDocumentRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let platform = std::env::consts::OS;
    let ext = std::path::Path::new(&req.file_path)
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
                Json(json!({ "error": format!("Unsupported file type: .{ext}") })),
            ))
        }
    };

    let output_path = req.output_path.unwrap_or_else(|| {
        let p = std::path::Path::new(&req.file_path);
        p.with_extension(&req.format).to_string_lossy().to_string()
    });

    let script = match platform {
        "macos" => generate_export_applescript(app, &req.file_path, &output_path, &req.format),
        "windows" => generate_export_powershell(app, &req.file_path, &output_path, &req.format),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Unsupported platform" })),
            ))
        }
    };

    let result = match platform {
        "macos" => run_applescript(app, &script, Some(&req.file_path)).await,
        "windows" => run_powershell(app, &script, Some(&req.file_path)).await,
        _ => Err("Unsupported".to_string()),
    };

    match result {
        Ok(_) => Ok(Json(
            json!({ "exported": output_path, "format": req.format }),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )),
    }
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

    // Wrap the user script in a tell block if it doesn't already have one
    let full_script = if script.contains("tell application") {
        script.to_string()
    } else {
        format!("tell application \"{office_app}\"\n  activate\n  {script}\nend tell")
    };

    let output = tokio::process::Command::new("osascript")
        .arg("-e")
        .arg(&full_script)
        .output()
        .await
        .map_err(|e| format!("Failed to run osascript: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
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

    // Wrap in COM object creation if not already done
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

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("PowerShell error: {stderr}"))
    }
}

// ---------------------------------------------------------------------------
// Script generators for common operations
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
        _ => format!(
            r#"-- Export to {format} not directly supported for {app}
-- Try using the run endpoint with a custom script"#
        ),
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
$doc.SaveAs2("{output_path}", 17) # 17 = wdFormatPDF
$doc.Close($false)
$app.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($app) | Out-Null"#
        ),
        ("excel", "pdf") => format!(
            r#"$app = New-Object -ComObject Excel.Application
$wb = $app.Workbooks.Open("{file_path}")
$wb.ExportAsFixedFormat(0, "{output_path}") # 0 = xlTypePDF
$wb.Close($false)
$app.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($app) | Out-Null"#
        ),
        _ => String::new(),
    }
}
