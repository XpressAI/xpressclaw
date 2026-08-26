use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Component, Path as FsPath, PathBuf};
use std::process::Output;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

use xpressclaw_core::docker::manager::DockerManager;

use crate::state::AppState;

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 2_000;
const MAX_GIT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

type ApiError = (StatusCode, Json<Value>);
type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Deserialize)]
struct WorkspacePathQuery {
    #[serde(default)]
    path: String,
}

#[derive(Debug, Deserialize)]
struct SaveFileInput {
    path: String,
    content: String,
    expected_revision: String,
}

#[derive(Debug, Deserialize)]
struct TerminalQuery {
    columns: Option<u16>,
    rows: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct TerminalControl {
    #[serde(rename = "type")]
    kind: String,
    columns: Option<u16>,
    rows: Option<u16>,
}

#[derive(Debug, Serialize)]
struct WorkspaceEntry {
    name: String,
    path: String,
    kind: &'static str,
    symlink: bool,
    size: Option<u64>,
    modified_at: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct GitChange {
    path: String,
    original_path: Option<String>,
    status: String,
    index_status: String,
    worktree_status: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/{agent_id}", get(workspace_status))
        .route("/{agent_id}/tree", get(list_directory))
        .route("/{agent_id}/file", get(read_file).put(write_file))
        .route("/{agent_id}/git/status", get(git_status))
        .route("/{agent_id}/git/diff", get(git_diff))
        .route("/{agent_id}/terminal", get(open_terminal))
}

async fn workspace_status(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_same_origin(&headers)?;
    let root = workspace_root(&state, &agent_id)?;
    let docker = state.docker().await;
    let container_exists = match docker.as_ref() {
        Some(docker) => docker.is_project_container(&agent_id).await,
        None => false,
    };
    let container_running = match docker.as_ref() {
        Some(docker) if container_exists => docker.is_running(&agent_id).await,
        _ => false,
    };
    Ok(Json(json!({
        "agent_id": agent_id,
        "root": root.display().to_string(),
        "container_exists": container_exists,
        "container_running": container_running,
        "terminal_available": container_exists && docker.is_some(),
    })))
}

async fn list_directory(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(query): Query<WorkspacePathQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_same_origin(&headers)?;
    let root = workspace_root(&state, &agent_id)?;
    let relative = normalize_relative_path(&query.path)?;
    let workspace = open_workspace_dir(&root)?;

    let mut entries = Vec::new();
    let mut truncated = false;
    let directory_entries = workspace
        .read_dir(cap_path(&relative))
        .map_err(|error| internal_error(format!("failed to list directory: {error}")))?;
    for (index, entry) in directory_entries.enumerate() {
        if index == MAX_DIRECTORY_ENTRIES {
            truncated = true;
            break;
        }
        let entry = entry
            .map_err(|error| internal_error(format!("failed to read directory entry: {error}")))?;
        let file_type = entry.file_type().map_err(|error| {
            internal_error(format!("failed to inspect directory entry: {error}"))
        })?;
        let symlink = file_type.is_symlink();
        // Do not follow symlinks while describing the tree. File reads and
        // writes are capability-relative too, but keeping symlinks visibly
        // distinct avoids implying that they are ordinary editable files.
        let effective_metadata = if symlink { None } else { entry.metadata().ok() };
        let kind = match effective_metadata.as_ref() {
            Some(metadata) if metadata.is_dir() => "directory",
            Some(metadata) if metadata.is_file() => "file",
            _ if symlink => "symlink",
            _ => "other",
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let entry_relative = relative.join(&name);
        let modified_at = effective_metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .map(cap_std::time::SystemTime::into_std)
            .map(chrono::DateTime::<chrono::Utc>::from)
            .map(|value| value.to_rfc3339());
        entries.push(WorkspaceEntry {
            name,
            path: relative_path_string(&entry_relative),
            kind,
            symlink,
            size: effective_metadata
                .as_ref()
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len()),
            modified_at,
        });
    }
    entries.sort_by(|left, right| {
        let left_rank = (left.kind != "directory") as u8;
        let right_rank = (right.kind != "directory") as u8;
        left_rank
            .cmp(&right_rank)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });

    Ok(Json(json!({
        "path": relative_path_string(&relative),
        "entries": entries,
        "truncated": truncated,
    })))
}

async fn read_file(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(query): Query<WorkspacePathQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_same_origin(&headers)?;
    let root = workspace_root(&state, &agent_id)?;
    let relative = normalize_relative_path(&query.path)?;
    let workspace = open_workspace_dir(&root)?;
    let mut file = workspace
        .open(&relative)
        .map_err(|error| workspace_path_error("open file", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| internal_error(format!("failed to inspect file: {error}")))?;
    if !metadata.is_file() {
        return Err(api_error(StatusCode::BAD_REQUEST, "path is not a file"));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "files larger than {} MiB cannot be edited",
                MAX_FILE_BYTES / 1024 / 1024
            ),
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| internal_error(format!("failed to read file: {error}")))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "files larger than {} MiB cannot be edited",
                MAX_FILE_BYTES / 1024 / 1024
            ),
        ));
    }
    let content = String::from_utf8(bytes.clone()).map_err(|_| {
        api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "binary and non-UTF-8 files cannot be edited",
        )
    })?;
    Ok(Json(json!({
        "path": relative_path_string(&relative),
        "content": content,
        "revision": content_revision(&bytes),
        "size": bytes.len(),
    })))
}

async fn write_file(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<SaveFileInput>,
) -> ApiResult<Json<Value>> {
    require_same_origin(&headers)?;
    if input.content.len() as u64 > MAX_FILE_BYTES {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "files larger than {} MiB cannot be edited",
                MAX_FILE_BYTES / 1024 / 1024
            ),
        ));
    }
    let root = workspace_root(&state, &agent_id)?;
    let relative = normalize_relative_path(&input.path)?;
    let workspace = open_workspace_dir(&root)?;
    let (parent, file_name) = open_parent_dir(&workspace, &relative)?;
    let mut file = parent
        .open(&file_name)
        .map_err(|error| workspace_path_error("open file", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| internal_error(format!("failed to inspect file: {error}")))?;
    if !metadata.is_file() {
        return Err(api_error(StatusCode::BAD_REQUEST, "path is not a file"));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the file grew beyond the editable size limit; reload it before saving",
        ));
    }
    let mut current = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut current)
        .map_err(|error| internal_error(format!("failed to read file before saving: {error}")))?;
    if current.len() as u64 > MAX_FILE_BYTES {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the file grew beyond the editable size limit; reload it before saving",
        ));
    }
    let current_revision = content_revision(&current);
    if input.expected_revision != current_revision {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "the file changed after it was opened; reload it before saving",
                "current_revision": current_revision,
            })),
        ));
    }
    atomic_write(
        &parent,
        &file_name,
        input.content.as_bytes(),
        metadata.permissions(),
    )
    .map_err(|error| internal_error(format!("failed to save file: {error}")))?;
    let revision = content_revision(input.content.as_bytes());
    Ok(Json(json!({
        "path": relative_path_string(&relative),
        "revision": revision,
        "size": input.content.len(),
    })))
}

async fn git_status(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_same_origin(&headers)?;
    let root = workspace_root(&state, &agent_id)?;
    let repository = git_output(&root, &["rev-parse", "--is-inside-work-tree"]).await?;
    if !repository.status.success() {
        return Ok(Json(json!({
            "repository": false,
            "branch": Value::Null,
            "files": [],
        })));
    }
    let branch = git_output(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|branch| !branch.is_empty());
    let status = git_output(
        &root,
        &[
            "-c",
            "core.quotepath=false",
            "-c",
            "status.relativePaths=true",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )
    .await?;
    if !status.status.success() {
        return Err(git_command_error("git status", &status));
    }
    if status.stdout.len() > MAX_GIT_OUTPUT_BYTES {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "git status returned too many changed paths",
        ));
    }
    let files = parse_porcelain_status(&status.stdout);
    Ok(Json(json!({
        "repository": true,
        "branch": branch,
        "files": files,
    })))
}

async fn git_diff(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(query): Query<WorkspacePathQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_same_origin(&headers)?;
    let root = workspace_root(&state, &agent_id)?;
    let relative = normalize_relative_path(&query.path)?;
    if relative.as_os_str().is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "a file path is required",
        ));
    }
    let path = relative_path_string(&relative);
    let staged = git_output(
        &root,
        &[
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-color",
            "--unified=3",
            "--",
            &path,
        ],
    )
    .await?;
    if !staged.status.success() {
        return Err(git_command_error("git diff --cached", &staged));
    }
    let working = git_output(
        &root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--unified=3",
            "--",
            &path,
        ],
    )
    .await?;
    if !working.status.success() {
        return Err(git_command_error("git diff", &working));
    }
    let mut bytes = Vec::with_capacity(staged.stdout.len() + working.stdout.len() + 32);
    if !staged.stdout.is_empty() {
        bytes.extend_from_slice(b"# Staged changes\n\n");
        bytes.extend_from_slice(&staged.stdout);
    }
    if !working.stdout.is_empty() {
        if !bytes.is_empty() {
            bytes.extend_from_slice(b"\n");
        }
        bytes.extend_from_slice(b"# Working tree changes\n\n");
        bytes.extend_from_slice(&working.stdout);
    }
    let truncated = bytes.len() > MAX_GIT_OUTPUT_BYTES;
    bytes.truncate(MAX_GIT_OUTPUT_BYTES);
    let diff = String::from_utf8_lossy(&bytes).to_string();
    Ok(Json(json!({
        "path": path,
        "diff": diff,
        "truncated": truncated,
    })))
}

async fn open_terminal(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(query): Query<TerminalQuery>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> ApiResult<Response> {
    require_same_origin(&headers)?;
    let _ = workspace_root(&state, &agent_id)?;
    let docker = state.docker().await.ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Docker or Podman is not available",
        )
    })?;
    if !docker.is_project_container(&agent_id).await {
        return Err(api_error(
            StatusCode::CONFLICT,
            "run a task once to initialize this agent's retained environment",
        ));
    }
    let columns = query.columns.unwrap_or(120).clamp(20, 500);
    let rows = query.rows.unwrap_or(32).clamp(5, 300);
    Ok(
        websocket
            .on_upgrade(move |socket| terminal_socket(socket, docker, agent_id, columns, rows)),
    )
}

async fn terminal_socket(
    socket: WebSocket,
    docker: std::sync::Arc<DockerManager>,
    agent_id: String,
    columns: u16,
    rows: u16,
) {
    let (mut sender, mut receiver) = socket.split();
    let terminal = match docker.open_project_terminal(&agent_id, columns, rows).await {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = sender
                .send(Message::Text(
                    json!({ "type": "error", "message": error.to_string() })
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    let exec_id = terminal.exec_id;
    let mut input = terminal.input;
    let mut output = terminal.output;
    if sender
        .send(Message::Text(json!({ "type": "ready" }).to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            terminal_output = output.next() => {
                match terminal_output {
                    Some(Ok(output)) => {
                        if sender.send(Message::Binary(log_output_bytes(output).into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        let _ = sender.send(Message::Text(json!({
                            "type": "error",
                            "message": format!("terminal output failed: {error}"),
                        }).to_string().into())).await;
                        break;
                    }
                    None => {
                        let _ = sender.send(Message::Text(json!({ "type": "exit" }).to_string().into())).await;
                        break;
                    }
                }
            }
            browser_message = receiver.next() => {
                match browser_message {
                    Some(Ok(Message::Binary(data))) => {
                        if input.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(data))) => {
                        if let Ok(control) = serde_json::from_str::<TerminalControl>(data.as_str()) {
                            if control.kind == "resize" {
                                if let (Some(columns), Some(rows)) = (control.columns, control.rows) {
                                    let _ = docker.resize_terminal(&exec_id, columns, rows).await;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if sender.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(Message::Pong(_))) => {}
                }
            }
        }
    }
    let _ = input.shutdown().await;
}

fn log_output_bytes(output: bollard::container::LogOutput) -> Vec<u8> {
    match output {
        bollard::container::LogOutput::StdIn { message }
        | bollard::container::LogOutput::StdOut { message }
        | bollard::container::LogOutput::StdErr { message }
        | bollard::container::LogOutput::Console { message } => message.to_vec(),
    }
}

fn workspace_root(state: &AppState, agent_id: &str) -> ApiResult<PathBuf> {
    let config = state.config();
    let agent = config
        .agents
        .iter()
        .find(|agent| agent.name == agent_id)
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                format!("agent configuration not found: {agent_id}"),
            )
        })?;
    let workspace = agent
        .runner
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(expand_home)
        .unwrap_or_else(|| config.system.workspace_dir.clone());
    workspace.canonicalize().map_err(|error| {
        api_error(
            StatusCode::NOT_FOUND,
            format!("workspace {} is unavailable: {error}", workspace.display()),
        )
    })
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(relative) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(relative);
        }
    }
    PathBuf::from(path)
}

fn normalize_relative_path(raw: &str) -> ApiResult<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in FsPath::new(raw).components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    "workspace paths must be relative and cannot contain '..'",
                ));
            }
        }
    }
    Ok(normalized)
}

fn open_workspace_dir(root: &FsPath) -> ApiResult<Dir> {
    Dir::open_ambient_dir(root, ambient_authority()).map_err(|error| {
        api_error(
            StatusCode::NOT_FOUND,
            format!("workspace {} is unavailable: {error}", root.display()),
        )
    })
}

fn cap_path(path: &FsPath) -> &FsPath {
    if path.as_os_str().is_empty() {
        FsPath::new(".")
    } else {
        path
    }
}

fn open_parent_dir(workspace: &Dir, relative: &FsPath) -> ApiResult<(Dir, OsString)> {
    let file_name = relative
        .file_name()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "a file path is required"))?
        .to_os_string();
    let parent = relative.parent().unwrap_or_else(|| FsPath::new(""));
    let directory = if parent.as_os_str().is_empty() {
        workspace
            .try_clone()
            .map_err(|error| internal_error(format!("failed to clone workspace handle: {error}")))?
    } else {
        workspace
            .open_dir(parent)
            .map_err(|error| workspace_path_error("open parent directory", error))?
    };
    Ok((directory, file_name))
}

fn workspace_path_error(operation: &str, error: std::io::Error) -> ApiError {
    let status = match error.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, format!("failed to {operation}: {error}"))
}

fn relative_path_string(path: &FsPath) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn content_revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn atomic_write(
    parent: &Dir,
    file_name: &OsStr,
    content: &[u8],
    permissions: cap_std::fs::Permissions,
) -> std::io::Result<()> {
    let temporary = OsString::from(format!(".xpressclaw-save-{}", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = parent.open_with(&temporary, &options)?;
        file.write_all(content)?;
        file.sync_all()?;
        file.set_permissions(permissions)?;
        drop(file);
        parent.rename(&temporary, parent, file_name)
    })();
    if result.is_err() {
        let _ = parent.remove_file(&temporary);
    }
    result
}

async fn git_output(root: &FsPath, arguments: &[&str]) -> ApiResult<Output> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(arguments);
    tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| api_error(StatusCode::GATEWAY_TIMEOUT, "git command timed out"))?
        .map_err(|error| internal_error(format!("failed to run git: {error}")))
}

fn parse_porcelain_status(output: &[u8]) -> Vec<GitChange> {
    let fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let field = fields[index];
        if field.len() < 4 || field[2] != b' ' {
            index += 1;
            continue;
        }
        let status = String::from_utf8_lossy(&field[..2]).to_string();
        let path = String::from_utf8_lossy(&field[3..]).to_string();
        let renamed = matches!(field[0], b'R' | b'C') || matches!(field[1], b'R' | b'C');
        let original_path = if renamed && index + 1 < fields.len() {
            index += 1;
            Some(String::from_utf8_lossy(fields[index]).to_string())
        } else {
            None
        };
        changes.push(GitChange {
            path,
            original_path,
            index_status: String::from_utf8_lossy(&field[..1]).to_string(),
            worktree_status: String::from_utf8_lossy(&field[1..2]).to_string(),
            status,
        });
        index += 1;
    }
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    changes
}

fn git_command_error(command: &str, output: &Output) -> ApiError {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    internal_error(if detail.is_empty() {
        format!("{command} failed")
    } else {
        format!("{command} failed: {detail}")
    })
}

/// Browser requests must be same-origin because file writes and terminal
/// access are intentionally as powerful as local access to the project. Calls
/// without an Origin header remain available to the desktop shell and trusted
/// local API clients.
fn require_same_origin(headers: &HeaderMap) -> ApiResult<()> {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(());
    };
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let origin = origin.parse::<axum::http::Uri>().ok();
    let is_http_origin = origin
        .as_ref()
        .and_then(axum::http::Uri::scheme_str)
        .is_some_and(|scheme| matches!(scheme, "http" | "https"));
    let authority = origin.as_ref().and_then(axum::http::Uri::authority);
    let host_matches = host.is_some_and(|host| {
        authority.is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(host))
    });
    // A TLS-terminating proxy may legitimately rewrite Host to the upstream
    // listener. In that topology the browser's unforgeable Fetch Metadata
    // header still describes the request relative to the public origin. Raw
    // non-browser clients gain nothing by forging it because they may already
    // omit Origin; this check protects browser sessions from another origin.
    let browser_reports_same_origin = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("same-origin"));
    if is_http_origin && (host_matches || browser_reports_same_origin) {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::FORBIDDEN,
            "workspace and terminal APIs only accept same-origin browser requests",
        ))
    }
}

fn api_error(status: StatusCode, message: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": message.into() })))
}

fn internal_error(message: impl Into<String>) -> ApiError {
    api_error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_paths_reject_traversal_and_absolute_paths() {
        assert!(normalize_relative_path("src/main.rs").is_ok());
        assert!(normalize_relative_path("./src/main.rs").is_ok());
        assert!(normalize_relative_path("../secret").is_err());
        assert!(normalize_relative_path("src/../../secret").is_err());
        assert!(normalize_relative_path("/etc/passwd").is_err());
    }

    #[test]
    fn resolved_symlinks_cannot_escape_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "nope").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("secret"),
                workspace.path().join("link"),
            )
            .unwrap();
            let root = workspace.path().canonicalize().unwrap();
            let directory = open_workspace_dir(&root).unwrap();
            assert!(directory.open("link").is_err());
        }
    }

    #[test]
    fn file_revision_detects_conflicts_and_atomic_save_preserves_permissions() {
        let workspace = tempfile::tempdir().unwrap();
        let file = workspace.path().join("hello.rs");
        std::fs::write(&file, "first").unwrap();
        let metadata = std::fs::metadata(&file).unwrap();
        let revision = content_revision(&std::fs::read(&file).unwrap());
        std::fs::write(&file, "agent edit").unwrap();
        assert_ne!(revision, content_revision(&std::fs::read(&file).unwrap()));
        let root = open_workspace_dir(workspace.path()).unwrap();
        let permissions = root
            .open("hello.rs")
            .unwrap()
            .metadata()
            .unwrap()
            .permissions();
        atomic_write(&root, OsStr::new("hello.rs"), b"saved", permissions).unwrap();
        assert_eq!(std::fs::read_to_string(file).unwrap(), "saved");
        assert_eq!(
            std::fs::metadata(workspace.path().join("hello.rs"))
                .unwrap()
                .permissions(),
            metadata.permissions()
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_stays_on_open_parent_when_path_is_replaced_by_symlink() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src/main.rs"), "workspace").unwrap();
        std::fs::write(outside.path().join("main.rs"), "outside").unwrap();

        let root = open_workspace_dir(workspace.path()).unwrap();
        let (parent, file_name) = open_parent_dir(&root, FsPath::new("src/main.rs")).unwrap();
        let permissions = parent
            .open(&file_name)
            .unwrap()
            .metadata()
            .unwrap()
            .permissions();

        std::fs::rename(
            workspace.path().join("src"),
            workspace.path().join("src-original"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("src")).unwrap();
        atomic_write(&parent, &file_name, b"saved", permissions).unwrap();

        assert_eq!(
            std::fs::read_to_string(workspace.path().join("src-original/main.rs")).unwrap(),
            "saved"
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("main.rs")).unwrap(),
            "outside"
        );
    }

    #[test]
    fn parses_porcelain_status_with_unicode_and_renames() {
        let changes = parse_porcelain_status(
            b" M src/main.rs\0?? \xe6\x96\xb0\xe3\x81\x97\xe3\x81\x84.txt\0R  new-name.rs\0old-name.rs\0",
        );
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].path, "new-name.rs");
        assert_eq!(changes[0].original_path.as_deref(), Some("old-name.rs"));
        assert_eq!(changes[1].path, "src/main.rs");
        assert_eq!(changes[2].path, "新しい.txt");
        assert_eq!(changes[2].status, "??");
    }

    #[test]
    fn browser_workspace_access_is_same_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "xpressclaw.example".parse().unwrap());
        headers.insert(
            header::ORIGIN,
            "https://xpressclaw.example".parse().unwrap(),
        );
        assert!(require_same_origin(&headers).is_ok());

        headers.insert(header::ORIGIN, "https://malicious.example".parse().unwrap());
        assert_eq!(
            require_same_origin(&headers).unwrap_err().0,
            StatusCode::FORBIDDEN
        );

        headers.insert("x-forwarded-host", "malicious.example".parse().unwrap());
        assert_eq!(
            require_same_origin(&headers).unwrap_err().0,
            StatusCode::FORBIDDEN
        );

        headers.insert(header::HOST, "127.0.0.1:8935".parse().unwrap());
        headers.insert(
            header::ORIGIN,
            "https://xpressclaw.example".parse().unwrap(),
        );
        headers.insert("sec-fetch-site", "same-origin".parse().unwrap());
        assert!(require_same_origin(&headers).is_ok());

        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert_eq!(
            require_same_origin(&headers).unwrap_err().0,
            StatusCode::FORBIDDEN
        );
    }
}
