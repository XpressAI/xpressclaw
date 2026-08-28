use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use tracing::{info, warn};
use xpressclaw_core::acp::{canonical_agent_kind, infer_agent_kind_from_backend, ACP_AGENTS};
use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::config::{
    context_label, default_native_runner_image, unique_session_id, AgentConfig, Config,
    ContainerEngineAccess, McpServerConfig, NativeRunnerConfig,
};
use xpressclaw_core::docker::manager::ContainerSpec;
use xpressclaw_core::llm::anthropic::AnthropicProvider;
use xpressclaw_core::llm::local::detect_ollama;
use xpressclaw_core::llm::openai::OpenAiProvider;
use xpressclaw_core::llm::router::LlmRouter;
use xpressclaw_core::paths::strip_verbatim;
use xpressclaw_core::system;
use xpressclaw_core::workers::native::{
    host_ssh_agent_socket, local_runner_image_alias, resolve_runner_kind, resolved_runner_image,
    subscription_auth_available,
};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/status", get(setup_status))
        .route("/check-docker", get(check_docker))
        .route("/start-docker", post(start_docker))
        .route("/system-info", get(system_info))
        .route("/agent-catalog", get(agent_catalog))
        .route("/directories", get(list_directories))
        .route("/project-environment", get(project_environment))
        .route("/check-ollama", get(check_ollama))
        .route("/recommend-model", get(recommend_model))
        .route("/validate-key", post(validate_key))
        .route("/complete", post(complete_setup))
        .route("/add-session", post(add_session))
        .route("/config", get(get_config))
        .route("/mcp-servers", get(list_mcp_servers))
        .route("/mcp-servers", post(upsert_mcp_server))
        .route("/mcp-servers/{name}/verify", post(verify_mcp_server))
        .route(
            "/mcp-servers/{name}",
            axum::routing::delete(delete_mcp_server),
        )
}

/// Return the current live configuration (sanitized — no API keys).
async fn get_config(State(state): State<AppState>) -> Json<Value> {
    let config = state.config();
    // Per-agent providers — collect a sanitized summary for the frontend.
    // Each entry shows the agent's declared provider, real model, base_url,
    // and whether an API key is set (never the key itself).
    let providers: Vec<Value> = config
        .agents
        .iter()
        .filter_map(|a| {
            let l = a.llm.as_ref()?;
            Some(json!({
                "agent": a.name,
                "provider": l.provider,
                "model": l.model,
                "base_url": l.base_url,
                "has_api_key": l.api_key.is_some(),
            }))
        })
        .collect();
    Json(json!({
        "llm": {
            // No global LLM config anymore. We expose the per-agent breakdown
            // so the settings page can render a summary without needing to
            // re-fetch /agents.
            "providers": providers,
        },
        "agents": config.agents.iter().map(|a| {
            let mut agent = json!({
                "name": a.name,
                "title": a.context_label(),
                "backend": a.backend,
                "model": a.effective_model(),
                // Full llm block is retained only for legacy configurations.
                "llm": a.llm.as_ref().map(|l| json!({
                    "provider": l.provider,
                    "model": l.model,
                    "api_key": l.api_key,
                    "base_url": l.base_url,
                })),
                "runner": a.runner,
                "tools": a.tools,
                "skills": a.skills,
                "volumes": a.volumes,
            });
            if let Some(ref budget) = a.budget {
                agent["budget"] = json!({
                    "daily": budget.daily,
                    "monthly": budget.monthly,
                    "per_task": budget.per_task,
                    "on_exceeded": budget.on_exceeded,
                    "fallback_model": budget.fallback_model,
                    "warn_at_percent": budget.warn_at_percent,
                });
            }
            if let Some(ref rl) = a.rate_limit {
                agent["rate_limit"] = json!({
                    "requests_per_minute": rl.requests_per_minute,
                    "tokens_per_minute": rl.tokens_per_minute,
                    "concurrent_requests": rl.concurrent_requests,
                });
            }
            if !a.wake_on.is_empty() {
                agent["wake_on"] = json!(a.wake_on.iter().map(|w| json!({
                    "schedule": w.schedule,
                    "event": w.event,
                    "condition": w.condition,
                })).collect::<Vec<_>>());
            }
            agent["hooks"] = json!({
                "before_message": a.hooks.before_message,
                "after_message": a.hooks.after_message,
            });
            if let Some(ref ip) = a.idle_prompt {
                agent["idle_prompt"] = json!(ip);
            }
            agent
        }).collect::<Vec<_>>(),
        "system": {
            "budget": {
                "daily": config.system.budget.daily,
                "monthly": config.system.budget.monthly,
                "on_exceeded": config.system.budget.on_exceeded,
            },
        },
        "instance": {
            "config_path": state.config_path.display().to_string(),
            "data_dir": config.system.data_dir.display().to_string(),
            "workspace_dir": config.system.workspace_dir.display().to_string(),
        },
        "mcp_servers": config.mcp_servers.iter().map(|(name, cfg)| {
            json!({
                "name": name,
                "type": cfg.server_type,
                "command": cfg.command,
                "args": cfg.args,
                "url": cfg.url,
                "env": cfg.env.keys().collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    }))
}

/// Check whether setup has been completed.
async fn setup_status(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "setup_complete": state.is_setup_complete() }))
}

/// Check if Docker/Podman is available.
async fn check_docker() -> Json<Value> {
    use xpressclaw_core::docker::manager::DockerManager;
    let installed = DockerManager::is_docker_desktop_installed();
    match DockerManager::connect().await {
        Ok(runtime) => Json(json!({
            "available": true,
            "installed": installed,
            "can_start": false,
            "runtime": runtime.runtime(),
            "version": runtime.runtime_version(),
            "socket": runtime.host_engine_socket().map(|path| path.display().to_string()),
            "rootless": runtime.is_rootless(),
            "error": null,
        })),
        Err(error) => Json(json!({
            "available": false,
            "installed": installed,
            "can_start": installed,
            "runtime": null,
            "version": null,
            "socket": null,
            "rootless": null,
            "error": error.to_string(),
        })),
    }
}

/// Try to start Docker Desktop. Only works on macOS/Windows.
async fn start_docker() -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use xpressclaw_core::docker::manager::DockerManager;
    DockerManager::start_docker_desktop().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    Ok(Json(json!({ "started": true })))
}

/// Detect system hardware (RAM, CPU, GPU).
async fn system_info() -> Json<Value> {
    let info = system::detect();
    let mut value = json!(info);
    value["working_directory"] = json!(std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string()));
    let ssh_agent_socket = host_ssh_agent_socket();
    value["ssh_agent_available"] = json!(ssh_agent_socket.is_some());
    value["ssh_agent_socket"] = json!(ssh_agent_socket.map(|path| path.display().to_string()));
    Json(value)
}

/// Return every supported ACP product and read-only host detection signals.
///
/// Detection never starts a product or opens credential files. We only check
/// for a known executable on PATH and for the existence of a standard config
/// location that can later be mounted into its isolated runner.
async fn agent_catalog() -> Json<Value> {
    let mut detected = ACP_AGENTS
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let executable = find_first_executable(agent.host_executables);
            let installed = executable.is_some();
            let configured = subscription_auth_available(agent.kind);
            let status = if installed && configured {
                "ready"
            } else if installed {
                "sign_in"
            } else {
                "not_installed"
            };
            (
                !installed,
                !configured,
                index,
                json!({
                    "kind": agent.kind,
                    "name": agent.name,
                    "mark": agent.mark,
                    "description": agent.description,
                    "command": agent.command,
                    "login_command": agent.login_command,
                    "install_url": agent.install_url,
                    "image": agent.minimal_image,
                    "host_image": agent.host_image,
                    "installed": installed,
                    "configured": configured,
                    "status": status,
                    "executable": executable.map(|path| path.display().to_string()),
                }),
            )
        })
        .collect::<Vec<_>>();
    detected.sort_by_key(|(not_installed, not_configured, index, _)| {
        (*not_installed, *not_configured, *index)
    });
    Json(json!({
        "agents": detected
            .into_iter()
            .map(|(_, _, _, agent)| agent)
            .collect::<Vec<_>>()
    }))
}

fn find_first_executable(names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| find_executable(name))
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(name);
    if direct.components().count() > 1 && executable_file(&direct) {
        return Some(direct);
    }
    for directory in executable_search_directories() {
        let candidate = directory.join(name);
        if executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let extensions = std::env::var_os("PATHEXT")
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
            for extension in extensions
                .split(';')
                .filter(|extension| !extension.is_empty())
            {
                let candidate = directory.join(format!("{name}{extension}"));
                if executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn executable_search_directories() -> Vec<PathBuf> {
    let mut directories = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if let Some(home) = home.as_ref() {
        directories.extend(
            [
                ".local/bin",
                "bin",
                ".npm-global/bin",
                ".bun/bin",
                ".cargo/bin",
                ".volta/bin",
                ".local/share/pnpm",
            ]
            .map(|path| home.join(path)),
        );
        let nvm_versions = home.join(".nvm/versions/node");
        if let Ok(versions) = std::fs::read_dir(nvm_versions) {
            directories.extend(
                versions
                    .filter_map(|version| version.ok())
                    .map(|version| version.path().join("bin"))
                    .filter(|path| path.is_dir()),
            );
        }
    }
    #[cfg(unix)]
    directories.extend([
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/snap/bin"),
    ]);
    #[cfg(windows)]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            directories.push(PathBuf::from(app_data).join("npm"));
        }
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            directories.push(PathBuf::from(local_app_data).join("Programs"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    directories.retain(|path| seen.insert(path.clone()));
    directories
}

fn executable_file(path: &FsPath) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug, Deserialize)]
struct DirectoryQuery {
    path: Option<String>,
}

/// Browse directories on the machine running XpressClaw. Returning directory
/// names only (never file names or contents) gives web and mobile clients a
/// usable server-side folder picker without relying on a desktop-only API.
async fn list_directories(
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let requested = query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| {
            if path == "~" {
                home.clone().unwrap_or_else(|| PathBuf::from(path))
            } else if let Some(rest) = path.strip_prefix("~/") {
                home.clone()
                    .map(|home| home.join(rest))
                    .unwrap_or_else(|| PathBuf::from(path))
            } else {
                PathBuf::from(path)
            }
        })
        .or_else(|| home.clone())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let current = requested.canonicalize().map_err(|error| {
        bad_request(format!(
            "Cannot open directory {}: {error}",
            requested.display()
        ))
    })?;
    // Filesystem calls keep the verbatim path, which is what lifts MAX_PATH;
    // only what leaves this handler is stripped.
    let display_path = strip_verbatim(current.clone());
    if !current.is_dir() {
        return Err(bad_request(format!(
            "{} is not a directory",
            display_path.display()
        )));
    }

    let mut directories = std::fs::read_dir(&current)
        .map_err(internal_error)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() && !file_type.is_symlink() {
                return None;
            }
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            Some((
                entry.file_name().to_string_lossy().into_owned(),
                strip_verbatim(path).display().to_string(),
            ))
        })
        .collect::<Vec<_>>();
    directories.sort_by_key(|(name, _)| name.to_lowercase());
    let root = display_path
        .ancestors()
        .last()
        .unwrap_or(display_path.as_path())
        .display()
        .to_string();
    Ok(Json(json!({
        "path": display_path.display().to_string(),
        "parent": display_path.parent().map(|path| path.display().to_string()),
        "home": home.map(|path| path.display().to_string()),
        "roots": [root],
        "directories": directories.into_iter().map(|(name, path)| json!({
            "name": name,
            "path": path,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
struct ProjectEnvironmentQuery {
    path: String,
}

/// Inspect well-known project metadata without reading project source files.
/// Suggestions are informational and opt-in; the client decides which
/// commands, if any, become runner startup commands.
async fn project_environment(
    Query(query): Query<ProjectEnvironmentQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let requested = PathBuf::from(query.path.trim());
    let workspace = requested.canonicalize().map_err(|error| {
        bad_request(format!(
            "Cannot inspect workspace {}: {error}",
            requested.display()
        ))
    })?;
    // Probes below join onto the verbatim path; only the response is stripped.
    let display_workspace = strip_verbatim(workspace.clone());
    if !workspace.is_dir() {
        return Err(bad_request(format!(
            "{} is not a directory",
            display_workspace.display()
        )));
    }

    let mut detected_files = Vec::new();
    let mut suggestions = Vec::new();
    let mut add = |id: &str,
                   name: &str,
                   description: &str,
                   file: &str,
                   command: Option<&str>,
                   requires_host_engine: bool| {
        detected_files.push(file.to_string());
        suggestions.push(json!({
            "id": id,
            "name": name,
            "description": description,
            "detected_file": file,
            "command": command,
            "requires_host_engine": requires_host_engine,
        }));
    };

    if let Some(file) = first_existing(
        &workspace,
        &[
            "compose.yaml",
            "compose.yml",
            "docker-compose.yaml",
            "docker-compose.yml",
        ],
    ) {
        add(
            "compose",
            "Start Docker Compose services",
            "Starts the project's declared development services before the ACP agent.",
            file,
            Some("docker compose up -d"),
            true,
        );
    }
    if workspace.join(".devcontainer/devcontainer.json").is_file() {
        add(
            "devcontainer",
            "Prepare the development container",
            "Uses the project's devcontainer definition and the host container engine.",
            ".devcontainer/devcontainer.json",
            Some("npx --yes @devcontainers/cli up --workspace-folder ."),
            true,
        );
    }
    if let Some(file) = first_existing(&workspace, &["Dockerfile", "dockerfile"]) {
        add(
            "dockerfile",
            "Build the project development image",
            "Builds the checked-in Dockerfile with the host image cache.",
            file,
            Some("docker build --tag xpressclaw-project-dev ."),
            true,
        );
    }
    if workspace.join("Vagrantfile").is_file() {
        add(
            "vagrant",
            "Vagrant environment detected",
            "Vagrant needs to be started on the host before agent work; it is not run inside the isolated ACP container.",
            "Vagrantfile",
            None,
            false,
        );
    }

    if workspace.join("pnpm-lock.yaml").is_file() {
        add(
            "pnpm",
            "Install pnpm dependencies",
            "Uses the committed lockfile before each agent task.",
            "pnpm-lock.yaml",
            Some("COREPACK_HOME=/tmp/xpressclaw-corepack corepack pnpm install --frozen-lockfile"),
            false,
        );
    } else if workspace.join("yarn.lock").is_file() {
        add(
            "yarn",
            "Install Yarn dependencies",
            "Uses the committed lockfile before each agent task.",
            "yarn.lock",
            Some("COREPACK_HOME=/tmp/xpressclaw-corepack corepack yarn install --immutable"),
            false,
        );
    } else if workspace.join("package-lock.json").is_file() {
        add(
            "npm",
            "Install npm dependencies",
            "Uses the committed lockfile before each agent task.",
            "package-lock.json",
            Some("npm ci"),
            false,
        );
    } else if workspace.join("package.json").is_file() {
        add(
            "npm",
            "Install npm dependencies",
            "Installs dependencies declared by package.json before each agent task.",
            "package.json",
            Some("npm install"),
            false,
        );
    }

    for (file, ecosystem) in [
        ("pyproject.toml", "Python"),
        ("requirements.txt", "Python"),
        ("Cargo.toml", "Rust"),
        ("go.mod", "Go"),
    ] {
        if workspace.join(file).is_file() {
            add(
                &format!("{}-sdk", ecosystem.to_lowercase()),
                &format!("{ecosystem} SDK required"),
                "Use the detected Docker, Compose, or devcontainer setup, or provide a runner image with this SDK.",
                file,
                None,
                false,
            );
        }
    }

    let git_repository = workspace.join(".git").exists();
    let git_uses_ssh = git_repository && repository_uses_ssh_remote(&workspace);

    Ok(Json(json!({
        "workspace": display_workspace.display().to_string(),
        "detected_files": detected_files,
        "suggestions": suggestions,
        "git_repository": git_repository,
        "git_uses_ssh": git_uses_ssh,
    })))
}

fn repository_uses_ssh_remote(workspace: &FsPath) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["config", "--get-regexp", r"^remote\..*\.(url|pushurl)$"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| {
            String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                line.split_once(char::is_whitespace)
                    .is_some_and(|(_, remote)| is_ssh_git_remote(remote.trim()))
            })
        })
}

fn is_ssh_git_remote(remote: &str) -> bool {
    if remote.starts_with("ssh://") || remote.starts_with("git+ssh://") {
        return true;
    }
    if remote.contains("://") {
        return false;
    }
    let Some((host, path)) = remote.split_once(':') else {
        return false;
    };
    if path.is_empty() || host.contains('/') || host.contains('\\') {
        return false;
    }
    !(host.len() == 1 && host.as_bytes()[0].is_ascii_alphabetic())
}

fn first_existing<'a>(workspace: &FsPath, candidates: &'a [&'a str]) -> Option<&'a str> {
    candidates
        .iter()
        .find(|candidate| workspace.join(candidate).is_file())
        .copied()
}

/// Check if Ollama is running and list models.
async fn check_ollama() -> Json<Value> {
    let info = detect_ollama().await;
    Json(json!(info))
}

/// Recommend a local model based on system hardware.
async fn recommend_model() -> Json<Value> {
    let info = system::detect();
    let rec = system::recommend_model(&info);
    Json(json!(rec))
}

#[derive(Deserialize)]
struct ValidateKeyRequest {
    provider: String,
    api_key: String,
    base_url: Option<String>,
}

/// Validate an API key for a provider.
async fn validate_key(
    Json(req): Json<ValidateKeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let result = match req.provider.as_str() {
        "openai" => OpenAiProvider::validate_key(&req.api_key, req.base_url.as_deref()).await,
        "anthropic" => AnthropicProvider::validate_key(&req.api_key, req.base_url.as_deref()).await,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Unknown provider: {}", req.provider) })),
            ));
        }
    };

    match result {
        Ok(valid) => {
            if !valid {
                return Ok(Json(json!({ "valid": false, "error": "Invalid API key" })));
            }
            // Fetch available models from the provider
            let models =
                fetch_provider_models(&req.provider, &req.api_key, req.base_url.as_deref()).await;
            Ok(Json(json!({ "valid": true, "models": models })))
        }
        Err(e) => Ok(Json(json!({ "valid": false, "error": e }))),
    }
}

/// Fetch available models from a provider's API.
async fn fetch_provider_models(
    provider: &str,
    api_key: &str,
    base_url: Option<&str>,
) -> Vec<Value> {
    let client = reqwest::Client::new();
    let url = match provider {
        "openai" => {
            let base = base_url.unwrap_or("https://api.openai.com");
            format!("{}/v1/models", base.trim_end_matches('/'))
        }
        "anthropic" => {
            let base = base_url.unwrap_or("https://api.anthropic.com");
            format!("{}/v1/models", base.trim_end_matches('/'))
        }
        _ => return vec![],
    };

    let mut req = client.get(&url);
    match provider {
        "anthropic" => {
            req = req
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01");
        }
        _ => {
            req = req.header("Authorization", format!("Bearer {api_key}"));
        }
    }

    match req.timeout(std::time::Duration::from_secs(10)).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<Value>().await {
                if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
                    return data
                        .iter()
                        .filter_map(|m| {
                            let id = m.get("id")?.as_str()?;
                            Some(json!({ "id": id }))
                        })
                        .collect();
                }
            }
            vec![]
        }
        _ => vec![],
    }
}

#[derive(Deserialize)]
struct CompleteSetupRequest {
    #[serde(default)]
    agents: Vec<AgentSetup>,
    #[serde(default)]
    mcp_servers: Option<std::collections::HashMap<String, McpServerConfig>>,
    /// Isolation mode. ACP workers currently require "docker".
    #[serde(default = "default_isolation")]
    isolation: String,
}

fn default_isolation() -> String {
    "docker".into()
}

#[derive(Deserialize)]
struct AgentSetup {
    /// Existing collaboration Project that should own a newly added Agent.
    /// Initial setup leaves this empty and creates the Agent's first Project.
    project_id: Option<String>,
    backend: Option<String>,
    runner_kind: Option<String>,
    runner_image: Option<String>,
    runner_workspace: Option<String>,
    workspace_mode: Option<String>,
    project_name: Option<String>,
    runner_model: Option<String>,
    runner_command: Option<Vec<String>>,
    startup_commands: Option<Vec<String>>,
    subscription_auth: Option<bool>,
    ssh_agent_forwarding: Option<bool>,
    runner_container_engine: Option<ContainerEngineAccess>,
    volumes: Option<Vec<String>>,
    #[serde(default)]
    mcp_servers: std::collections::HashMap<String, McpServerConfig>,
}

fn runner_kind_from_setup(setup: &AgentSetup) -> String {
    if let Some(configured) = setup.runner_kind.as_deref() {
        let configured = configured.to_lowercase();
        return if configured == "auto" {
            "codex".to_string()
        } else {
            canonical_agent_kind(&configured)
                .unwrap_or(configured.as_str())
                .to_string()
        };
    }

    let backend = setup.backend.as_deref().unwrap_or("codex").to_lowercase();
    infer_agent_kind_from_backend(&backend)
        .unwrap_or(backend.as_str())
        .to_string()
}

fn runner_from_setup(setup: &AgentSetup) -> NativeRunnerConfig {
    let kind = runner_kind_from_setup(setup);
    let container_engine = setup.runner_container_engine.unwrap_or_default();
    let configured_image = setup
        .runner_image
        .as_deref()
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .unwrap_or_default();
    let minimal_image = default_native_runner_image(&kind, ContainerEngineAccess::None);
    let host_image = default_native_runner_image(&kind, ContainerEngineAccess::Host);
    let image = if configured_image.is_empty()
        || minimal_image == Some(configured_image)
        || host_image == Some(configured_image)
    {
        default_native_runner_image(&kind, container_engine)
            .unwrap_or_default()
            .to_string()
    } else {
        configured_image.to_string()
    };
    NativeRunnerConfig {
        kind,
        image,
        workspace: setup
            .runner_workspace
            .as_deref()
            .map(str::trim)
            .filter(|workspace| !workspace.is_empty())
            .map(str::to_owned),
        project_name: setup
            .project_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| managed_workspace_requested(setup).then(|| "New project".to_string())),
        model: setup
            .runner_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned),
        session_config: std::collections::HashMap::new(),
        mcp_servers: Vec::new(),
        environment: std::collections::HashMap::new(),
        startup_commands: setup
            .startup_commands
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|command| command.trim().to_string())
            .filter(|command| !command.is_empty())
            .collect(),
        command: setup
            .runner_command
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|argument| argument.trim().to_string())
            .filter(|argument| !argument.is_empty())
            .collect(),
        subscription_auth: setup.subscription_auth.unwrap_or(true),
        ssh_agent_forwarding: setup.ssh_agent_forwarding.unwrap_or(false),
        container_engine,
    }
}

fn validate_runner(runner: &NativeRunnerConfig) -> Result<(), String> {
    if runner.kind == "custom" && runner.image.is_empty() {
        return Err("custom ACP agents require a container image".to_string());
    }
    if runner.kind == "custom" && runner.command.is_empty() {
        return Err("custom ACP agents require a server command".to_string());
    }
    Ok(())
}

fn managed_workspace_requested(setup: &AgentSetup) -> bool {
    setup.workspace_mode.as_deref() == Some("managed")
}

fn runner_context(runner: &NativeRunnerConfig) -> String {
    runner
        .project_name
        .clone()
        .unwrap_or_else(|| context_label(runner.workspace.as_deref(), &runner.kind))
}

fn assign_managed_workspace(
    setup: &AgentSetup,
    runner: &mut NativeRunnerConfig,
    session_id: &str,
    data_dir: &FsPath,
) -> std::io::Result<()> {
    if !managed_workspace_requested(setup) {
        return Ok(());
    }
    let workspace = data_dir.join("workspaces").join(session_id);
    std::fs::create_dir_all(&workspace)?;
    runner.workspace = Some(workspace.display().to_string());
    if runner.project_name.is_none() {
        runner.project_name = Some("New project".to_string());
    }
    Ok(())
}

/// Save the setup configuration and mark setup as complete.
async fn complete_setup(
    State(state): State<AppState>,
    Json(req): Json<CompleteSetupRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if req.isolation != "docker" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "ACP agents require Docker or Podman isolation" })),
        ));
    }
    let _config_guard = state.config_write_lock.lock().await;
    // Native products own model selection, credentials, instructions, and
    // subagents. The control plane stores only session runtime context.
    let current_config = state.config();
    let managed_root = current_config.system.data_dir.clone();
    let mut used_ids: Vec<String> = Vec::new();
    let mut agents: Vec<AgentConfig> = Vec::new();
    for session in &req.agents {
        let mut runner = runner_from_setup(session);
        let context = runner_context(&runner);
        let id_refs: Vec<&str> = used_ids.iter().map(String::as_str).collect();
        let session_id = unique_session_id(&context, &runner.kind, &id_refs);
        assign_managed_workspace(session, &mut runner, &session_id, &managed_root)
            .map_err(internal_error)?;
        validate_runner(&runner).map_err(bad_request)?;
        used_ids.push(session_id.clone());
        agents.push(AgentConfig {
            name: session_id,
            backend: runner.kind.clone(),
            runner,
            volumes: session.volumes.clone().unwrap_or_default(),
            ..Default::default()
        });
    }

    let mut config = (*current_config).clone();
    config.agents = agents;
    // Omission means the setup form did not manage connectors. An explicit
    // map, including an empty one, still replaces the configured MCP servers.
    if let Some(mcp_servers) = req.mcp_servers {
        config.mcp_servers = mcp_servers;
    }
    config.system.isolation = req.isolation.clone();

    // Save config to disk
    config.save(&state.config_path).map_err(internal_error)?;
    info!(path = %state.config_path.display(), "saved configuration");

    // Apply config immediately — register agents and build LLM router
    let config = Arc::new(config);

    // Sync agents in the database to match the new config.
    // Remove any agents not in the new config, then register the new ones.
    let registry = AgentRegistry::new(state.db.clone());
    let sessions = xpressclaw_core::sessions::SessionManager::new(state.db.clone());
    let existing_agents = registry.list().unwrap_or_default();
    let new_agent_names: std::collections::HashSet<&str> =
        config.agents.iter().map(|a| a.name.as_str()).collect();
    for existing in &existing_agents {
        if !new_agent_names.contains(existing.name.as_str()) {
            info!(name = existing.name, "removing agent not in new config");
            let _ = registry.delete(&existing.id);
            let _ = sessions.delete(&existing.id);
        }
    }
    for agent_config in &config.agents {
        match registry.ensure(&agent_config.name, &agent_config.backend) {
            Ok(record) => {
                let title = agent_config.context_label();
                if let Err(error) = sessions.ensure(&record.id, Some(&title)) {
                    warn!(name = record.name, error = %error, "failed to sync session");
                } else {
                    info!(
                        name = record.name,
                        backend = record.backend,
                        "synced ACP project"
                    );
                }
            }
            Err(e) => warn!(name = agent_config.name, error = %e, "failed to sync agent"),
        }
    }

    // Build LLM router from the new config
    let llm_router = LlmRouter::build_from_config(&config);
    state.apply_config(config, Some(Arc::new(llm_router)));
    info!("configuration applied — setup complete");

    Ok(Json(json!({
        "success": true,
        "downloading": false,
        "config_path": state.config_path.display().to_string()
    })))
}

/// Add a durable ACP Agent without replacing existing Agents.
async fn add_session(
    State(state): State<AppState>,
    Json(req): Json<AgentSetup>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let target_project_id = req
        .project_id
        .as_deref()
        .map(str::trim)
        .filter(|project_id| !project_id.is_empty())
        .map(str::to_owned);
    let _config_guard = state.config_write_lock.lock().await;
    let old_config = state.config();
    let existing_ids: Vec<&str> = old_config.agents.iter().map(|a| a.name.as_str()).collect();
    let mut runner = runner_from_setup(&req);
    validate_runner(&runner).map_err(bad_request)?;
    let title = runner_context(&runner);
    let session_id = unique_session_id(&title, &runner.kind, &existing_ids);
    assign_managed_workspace(&req, &mut runner, &session_id, &old_config.system.data_dir)
        .map_err(internal_error)?;

    let agent_config = AgentConfig {
        name: session_id,
        backend: runner.kind.clone(),
        runner,
        volumes: req.volumes.clone().unwrap_or_default(),
        ..Default::default()
    };

    // Append to existing config (don't replace)
    let mut new_agents = old_config.agents.clone();

    new_agents.push(agent_config.clone());

    // Preserve existing explicit connectors and merge only new explicit
    // overrides. Native CLIs do not consume the old built-in agent MCP layer.
    let mut new_mcp = old_config.mcp_servers.clone();
    for (name, server) in req.mcp_servers {
        new_mcp.insert(name, server);
    }

    let new_config = Config {
        agents: new_agents,
        mcp_servers: new_mcp,
        // Preserve every other top-level setting, including tool definitions,
        // policies, memory, system, and future configuration fields.
        ..old_config.as_ref().clone()
    };
    // Register the durable Agent. ACP workers are started per
    // attempt, so there is no long-running agent to auto-start. When an
    // existing Project is selected, attach the Agent under one database write
    // reservation before saving its configuration. Project deletion then
    // either wins first (and no config is written) or observes the new Agent
    // and is rejected as non-empty.
    let registry = AgentRegistry::new(state.db.clone());
    let record = if let Some(project_id) = target_project_id.as_deref() {
        let record = registry
            .create_in_project(&agent_config.name, &agent_config.backend, project_id)
            .map_err(|error| match error {
                xpressclaw_core::error::Error::ProjectNotFound { .. } => not_found(&error),
                xpressclaw_core::error::Error::Project(_) => (
                    StatusCode::CONFLICT,
                    Json(json!({ "error": error.to_string() })),
                ),
                _ => internal_error(error),
            })?;
        if let Err(error) = new_config.save(&state.config_path) {
            let _ = registry.delete(&record.id);
            return Err(internal_error(error));
        }
        record
    } else {
        new_config
            .save(&state.config_path)
            .map_err(internal_error)?;
        registry
            .ensure(&agent_config.name, &agent_config.backend)
            .map_err(internal_error)?
    };
    info!(
        name = agent_config.name,
        "added ACP project to configuration"
    );
    xpressclaw_core::sessions::SessionManager::new(state.db.clone())
        .ensure(&record.id, Some(&title))
        .map_err(internal_error)?;

    // Rebuild the router so the new agent has a binding. The previous
    // implementation reused the existing router, which silently meant the
    // newly-added agent had no provider mapping until the server restarted.
    let new_config = std::sync::Arc::new(new_config);
    let new_router = Arc::new(LlmRouter::build_from_config(&new_config));
    state.apply_config(new_config, Some(new_router));

    Ok(Json(json!({
        "success": true,
        "session": agent_config.name,
        "session_id": record.id,
        "title": title,
        "project_id": target_project_id.or(record.project_id),
    })))
}

// ---------------------------------------------------------------------------
// MCP server management
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct McpVerificationResult {
    ok: bool,
    status: &'static str,
    message: String,
    suggestion: Option<String>,
}

impl McpVerificationResult {
    fn ready(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            status: "ready",
            message: message.into(),
            suggestion: None,
        }
    }

    fn failed(
        status: &'static str,
        message: impl Into<String>,
        suggestion: Option<String>,
    ) -> Self {
        Self {
            ok: false,
            status,
            message: message.into(),
            suggestion,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct VerifyMcpServerRequest {
    agent_id: Option<String>,
}

/// List all configured MCP servers with full details.
async fn list_mcp_servers(State(state): State<AppState>) -> Json<Value> {
    let config = state.config();
    let servers: Vec<Value> = config
        .mcp_servers
        .iter()
        .map(|(name, cfg)| {
            json!({
                "name": name,
                "type": cfg.server_type,
                "command": cfg.command,
                "args": cfg.args,
                "url": cfg.url,
                "env": cfg.env,
                "headers": cfg.headers,
            })
        })
        .collect();
    Json(json!({ "servers": servers }))
}

#[derive(Debug, Deserialize)]
struct UpsertMcpServerRequest {
    name: String,
    #[serde(rename = "type", default = "default_stdio")]
    server_type: String,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
}

fn default_stdio() -> String {
    "stdio".to_string()
}

/// Add or update an MCP server in the global config.
async fn upsert_mcp_server(
    State(state): State<AppState>,
    Json(req): Json<UpsertMcpServerRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config_guard = state.config_write_lock.lock().await;
    let old_config = state.config();

    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "MCP server name is required" })),
        ));
    }
    if !matches!(req.server_type.as_str(), "stdio" | "http" | "sse") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "MCP server type must be stdio, http, or sse" })),
        ));
    }
    if req.server_type == "stdio"
        && req
            .command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .is_none()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "stdio MCP servers require a command" })),
        ));
    }
    if req.server_type == "stdio"
        && !req
            .command
            .as_deref()
            .map(str::trim)
            .is_some_and(|command| command.starts_with('/'))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "error": "stdio MCP server commands must be absolute paths inside the harness container" }),
            ),
        ));
    }
    if matches!(req.server_type.as_str(), "http" | "sse")
        && !req
            .url
            .as_deref()
            .map(str::trim)
            .is_some_and(|url| url.starts_with("http://") || url.starts_with("https://"))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "HTTP and SSE MCP servers require an http(s) URL" })),
        ));
    }

    let mut new_mcp = old_config.mcp_servers.clone();
    new_mcp.insert(
        name.clone(),
        McpServerConfig {
            server_type: req.server_type.clone(),
            command: req.command.filter(|_| req.server_type == "stdio"),
            args: if req.server_type == "stdio" {
                req.args
            } else {
                Vec::new()
            },
            env: if req.server_type == "stdio" {
                req.env
            } else {
                std::collections::HashMap::new()
            },
            url: req.url.filter(|_| req.server_type != "stdio"),
            headers: if req.server_type == "stdio" {
                std::collections::HashMap::new()
            } else {
                req.headers
            },
        },
    );

    let new_config = Config {
        mcp_servers: new_mcp,
        agents: old_config.agents.clone(),
        instance: old_config.instance.clone(),
        collaboration: old_config.collaboration.clone(),
        llm: old_config.llm.clone(),
        system: old_config.system.clone(),
        tools: old_config.tools.clone(),
        tool_policies: old_config.tool_policies.clone(),
        memory: old_config.memory.clone(),
    };
    new_config
        .save(&state.config_path)
        .map_err(internal_error)?;

    let new_config = std::sync::Arc::new(new_config);
    state.apply_config(new_config, state.llm_router());

    Ok(Json(json!({ "success": true, "name": name })))
}

/// Verify a saved MCP server using the closest safe execution context.
///
/// Remote transports receive a live protocol request from the control plane.
/// Stdio commands are checked inside the selected project's runner image,
/// because host paths say nothing about what is installed in that container.
async fn verify_mcp_server(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<VerifyMcpServerRequest>,
) -> Result<Json<McpVerificationResult>, (StatusCode, Json<Value>)> {
    let config = state.config();
    let server = config.mcp_servers.get(&name).cloned().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("MCP server '{name}' not found") })),
        )
    })?;

    let result = match server.server_type.as_str() {
        "http" | "sse" => verify_remote_mcp_server(&server).await,
        "stdio" => {
            let Some(agent_id) = req
                .agent_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                return Ok(Json(McpVerificationResult::failed(
                    "project_required",
                    "Stdio commands must be verified inside a project's runner image.",
                    Some("Open the project's Agent page and verify this server there.".to_string()),
                )));
            };
            verify_stdio_mcp_server(&state, &server, agent_id).await
        }
        _ => McpVerificationResult::failed(
            "invalid_configuration",
            "The saved MCP transport is not supported.",
            Some("Edit the server and choose stdio, HTTP, or SSE.".to_string()),
        ),
    };

    Ok(Json(result))
}

async fn verify_remote_mcp_server(server: &McpServerConfig) -> McpVerificationResult {
    let Some(url) = server
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return McpVerificationResult::failed(
            "invalid_configuration",
            "The MCP server has no URL.",
            Some("Edit the server and provide an HTTP or HTTPS URL.".to_string()),
        );
    };

    let mut headers = HeaderMap::new();
    for (name, value) in &server.headers {
        let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) else {
            return McpVerificationResult::failed(
                "invalid_configuration",
                format!("HTTP header name '{name}' is invalid."),
                Some("Edit the server's HTTP headers and verify again.".to_string()),
            );
        };
        let Ok(header_value) = HeaderValue::from_str(value) else {
            return McpVerificationResult::failed(
                "invalid_configuration",
                format!("HTTP header '{name}' has an invalid value."),
                Some("Edit the server's HTTP headers and verify again.".to_string()),
            );
        };
        headers.insert(header_name, header_value);
    }
    let default_accept = if server.server_type == "sse" {
        HeaderValue::from_static("text/event-stream")
    } else {
        HeaderValue::from_static("application/json, text/event-stream")
    };
    headers.entry(ACCEPT).or_insert(default_accept);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            warn!(%error, "failed to build MCP verification client");
            return McpVerificationResult::failed(
                "connection_failed",
                "XpressClaw could not create an HTTP client for this verification.",
                None,
            );
        }
    };

    let request = if server.server_type == "sse" {
        client.get(url).headers(headers)
    } else {
        client.post(url).headers(headers).json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": "xpressclaw-verifier",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            },
        }))
    };

    match request.send().await {
        Ok(response) => verify_remote_mcp_response(&server.server_type, response).await,
        Err(error) => {
            let message = if error.is_timeout() {
                "The MCP server did not respond within 10 seconds."
            } else if error.is_connect() {
                "XpressClaw could not connect to the MCP server."
            } else {
                "The MCP verification request failed."
            };
            warn!(
                timed_out = error.is_timeout(),
                connection_error = error.is_connect(),
                "remote MCP verification failed"
            );
            McpVerificationResult::failed(
                "connection_failed",
                message,
                Some(
                    "Check the URL, network access, and TLS configuration, then try again."
                        .to_string(),
                ),
            )
        }
    }
}

const MCP_VERIFICATION_BODY_LIMIT: usize = 256 * 1024;
const MCP_VERIFICATION_STREAM_TIMEOUT: Duration = Duration::from_secs(5);

async fn verify_remote_mcp_response(
    server_type: &str,
    response: reqwest::Response,
) -> McpVerificationResult {
    let status = response.status();
    if !status.is_success() {
        return remote_verification_from_status(status);
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase);

    match server_type {
        "http" => match content_type.as_deref() {
            Some("application/json") => verify_json_initialize_response(response).await,
            Some("text/event-stream") => verify_sse_initialize_response(response).await,
            _ => McpVerificationResult::failed(
                "invalid_response",
                "The endpoint returned a successful HTTP status but not an MCP response.",
                Some(
                    "Check that the URL points to a Streamable HTTP MCP endpoint, not a login or web page."
                        .to_string(),
                ),
            ),
        },
        "sse" if content_type.as_deref() == Some("text/event-stream") => {
            verify_legacy_sse_handshake(response).await
        }
        "sse" => McpVerificationResult::failed(
            "invalid_response",
            "The endpoint did not return an MCP SSE stream.",
            Some(
                "Check that the URL points to the server's SSE endpoint and does not redirect to a login page."
                    .to_string(),
            ),
        ),
        _ => McpVerificationResult::failed(
            "invalid_configuration",
            "The saved MCP transport is not supported.",
            None,
        ),
    }
}

async fn verify_json_initialize_response(response: reqwest::Response) -> McpVerificationResult {
    let body = match tokio::time::timeout(
        MCP_VERIFICATION_STREAM_TIMEOUT,
        read_limited_response_body(response),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(ResponseBodyError::TooLarge)) => {
            return McpVerificationResult::failed(
                "invalid_response",
                "The MCP initialize response was unexpectedly large.",
                Some("Check the MCP server and verify again.".to_string()),
            );
        }
        Ok(Err(ResponseBodyError::ReadFailed)) => {
            return McpVerificationResult::failed(
                "connection_failed",
                "XpressClaw could not read the MCP initialize response.",
                Some("Check the MCP server and verify again.".to_string()),
            );
        }
        Err(_) => {
            return McpVerificationResult::failed(
                "verification_timeout",
                "The MCP server did not finish its initialize response within 5 seconds.",
                Some("Check the MCP server and verify again.".to_string()),
            );
        }
    };

    match serde_json::from_slice::<Value>(&body) {
        Ok(value) => initialize_verification_result(&value),
        Err(_) => McpVerificationResult::failed(
            "invalid_response",
            "The endpoint returned JSON that was not a valid MCP initialize response.",
            Some("Check that the URL and transport match the MCP server.".to_string()),
        ),
    }
}

async fn verify_sse_initialize_response(response: reqwest::Response) -> McpVerificationResult {
    let inspection = tokio::time::timeout(MCP_VERIFICATION_STREAM_TIMEOUT, async move {
        let mut response = response;
        let mut body = Vec::new();

        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len().saturating_add(chunk.len()) > MCP_VERIFICATION_BODY_LIMIT {
                        return SseInspection::TooLarge;
                    }
                    body.extend_from_slice(&chunk);

                    for event in complete_sse_events(&body) {
                        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
                            continue;
                        };
                        match validate_initialize_response(&value) {
                            InitializeResponse::Ready => return SseInspection::Ready,
                            InitializeResponse::ProtocolError => {
                                return SseInspection::ProtocolError;
                            }
                            InitializeResponse::Invalid => {}
                        }
                    }
                }
                Ok(None) => return SseInspection::Invalid,
                Err(_) => return SseInspection::ReadFailed,
            }
        }
    })
    .await;

    match inspection {
        Ok(SseInspection::Ready) => McpVerificationResult::ready(
            "The MCP endpoint returned a valid initialize response over SSE.",
        ),
        Ok(SseInspection::ProtocolError) => initialize_protocol_error_result(),
        Ok(SseInspection::TooLarge) => McpVerificationResult::failed(
            "invalid_response",
            "The MCP SSE response was unexpectedly large before initialization completed.",
            Some("Check the MCP server and verify again.".to_string()),
        ),
        Ok(SseInspection::ReadFailed) => McpVerificationResult::failed(
            "connection_failed",
            "XpressClaw could not read the MCP SSE response.",
            Some("Check the MCP server and verify again.".to_string()),
        ),
        Ok(SseInspection::Invalid) => McpVerificationResult::failed(
            "invalid_response",
            "The SSE stream ended without a valid MCP initialize response.",
            Some("Check that the URL and transport match the MCP server.".to_string()),
        ),
        Err(_) => McpVerificationResult::failed(
            "verification_timeout",
            "The SSE stream did not return an MCP initialize response within 5 seconds.",
            Some("Check the MCP server and verify again.".to_string()),
        ),
    }
}

async fn verify_legacy_sse_handshake(response: reqwest::Response) -> McpVerificationResult {
    let base_url = response.url().clone();
    let inspection = tokio::time::timeout(MCP_VERIFICATION_STREAM_TIMEOUT, async move {
        let mut response = response;
        let mut body = Vec::new();

        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len().saturating_add(chunk.len()) > MCP_VERIFICATION_BODY_LIMIT {
                        return SseInspection::TooLarge;
                    }
                    body.extend_from_slice(&chunk);

                    for event in complete_sse_events(&body) {
                        if event.event.as_deref() != Some("endpoint") {
                            continue;
                        }
                        let endpoint = event.data.trim();
                        if endpoint.is_empty() {
                            continue;
                        }
                        let Ok(endpoint_url) = base_url.join(endpoint) else {
                            continue;
                        };
                        if matches!(endpoint_url.scheme(), "http" | "https") {
                            return SseInspection::Ready;
                        }
                    }
                }
                Ok(None) => return SseInspection::Invalid,
                Err(_) => return SseInspection::ReadFailed,
            }
        }
    })
    .await;

    match inspection {
        Ok(SseInspection::Ready) => McpVerificationResult::ready(
            "The MCP endpoint returned a valid SSE endpoint handshake.",
        ),
        Ok(SseInspection::TooLarge) => McpVerificationResult::failed(
            "invalid_response",
            "The SSE response was unexpectedly large before its MCP handshake completed.",
            Some("Check the MCP server and verify again.".to_string()),
        ),
        Ok(SseInspection::ReadFailed) => McpVerificationResult::failed(
            "connection_failed",
            "XpressClaw could not read the MCP SSE handshake.",
            Some("Check the MCP server and verify again.".to_string()),
        ),
        Ok(SseInspection::Invalid | SseInspection::ProtocolError) => McpVerificationResult::failed(
            "invalid_response",
            "The SSE stream ended without a valid MCP endpoint handshake.",
            Some("Check that the URL points to a legacy MCP SSE endpoint.".to_string()),
        ),
        Err(_) => McpVerificationResult::failed(
            "verification_timeout",
            "The SSE stream did not provide an MCP endpoint handshake within 5 seconds.",
            Some("Check the MCP server and verify again.".to_string()),
        ),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ResponseBodyError {
    TooLarge,
    ReadFailed,
}

async fn read_limited_response_body(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, ResponseBodyError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ResponseBodyError::ReadFailed)?
    {
        if body.len().saturating_add(chunk.len()) > MCP_VERIFICATION_BODY_LIMIT {
            return Err(ResponseBodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug, PartialEq, Eq)]
enum InitializeResponse {
    Ready,
    ProtocolError,
    Invalid,
}

fn validate_initialize_response(value: &Value) -> InitializeResponse {
    let Some(response) = value.as_object() else {
        return InitializeResponse::Invalid;
    };
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || response.get("id").and_then(Value::as_u64) != Some(1)
    {
        return InitializeResponse::Invalid;
    }
    if response.get("error").is_some_and(Value::is_object) {
        return InitializeResponse::ProtocolError;
    }

    let Some(result) = response.get("result").and_then(Value::as_object) else {
        return InitializeResponse::Invalid;
    };
    let valid_protocol_version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .is_some_and(|version| !version.trim().is_empty());
    let valid_capabilities = result.get("capabilities").is_some_and(Value::is_object);
    let valid_server_info = result
        .get("serverInfo")
        .and_then(Value::as_object)
        .is_some_and(|server_info| {
            server_info
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| !name.trim().is_empty())
                && server_info
                    .get("version")
                    .and_then(Value::as_str)
                    .is_some_and(|version| !version.trim().is_empty())
        });

    if valid_protocol_version && valid_capabilities && valid_server_info {
        InitializeResponse::Ready
    } else {
        InitializeResponse::Invalid
    }
}

fn initialize_verification_result(value: &Value) -> McpVerificationResult {
    match validate_initialize_response(value) {
        InitializeResponse::Ready => {
            McpVerificationResult::ready("The MCP endpoint returned a valid initialize response.")
        }
        InitializeResponse::ProtocolError => initialize_protocol_error_result(),
        InitializeResponse::Invalid => McpVerificationResult::failed(
            "invalid_response",
            "The endpoint returned JSON that was not a valid MCP initialize response.",
            Some("Check that the URL and transport match the MCP server.".to_string()),
        ),
    }
}

fn initialize_protocol_error_result() -> McpVerificationResult {
    McpVerificationResult::failed(
        "protocol_error",
        "The MCP endpoint returned a JSON-RPC error for the initialize request.",
        Some("Check the MCP server configuration and authentication, then try again.".to_string()),
    )
}

#[derive(Debug, PartialEq, Eq)]
enum SseInspection {
    Ready,
    ProtocolError,
    Invalid,
    TooLarge,
    ReadFailed,
}

#[derive(Debug, PartialEq, Eq)]
struct SseEvent {
    event: Option<String>,
    data: String,
}

fn complete_sse_events(body: &[u8]) -> Vec<SseEvent> {
    let normalized = String::from_utf8_lossy(body)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let complete_length = normalized
        .rfind("\n\n")
        .map_or(0, |event_end| event_end + 2);

    normalized[..complete_length]
        .split("\n\n")
        .filter_map(|event| {
            let mut event_type = None;
            let mut data = Vec::new();
            for line in event.lines() {
                if line.starts_with(':') {
                    continue;
                }
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => event_type = Some(value.to_string()),
                    "data" => data.push(value),
                    _ => {}
                }
            }
            (!data.is_empty()).then(|| SseEvent {
                event: event_type,
                data: data.join("\n"),
            })
        })
        .collect()
}

fn remote_verification_from_status(status: StatusCode) -> McpVerificationResult {
    if status.is_success() {
        return McpVerificationResult::failed(
            "invalid_response",
            "The endpoint returned a successful status without a verified MCP response.",
            Some("Check that the URL and transport match the MCP server.".to_string()),
        );
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return McpVerificationResult::failed(
            "authentication_required",
            format!("The MCP endpoint responded with {status}; authentication is required."),
            Some(
                "Add a valid Authorization header to this server configuration, then verify again."
                    .to_string(),
            ),
        );
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return McpVerificationResult::failed(
            "rate_limited",
            "The MCP endpoint is reachable but rate-limited the verification request.",
            Some("Wait briefly and verify again.".to_string()),
        );
    }
    if status.is_server_error() {
        return McpVerificationResult::failed(
            "server_error",
            format!("The MCP endpoint is reachable but returned {status}."),
            Some("Check the MCP server and try again.".to_string()),
        );
    }
    McpVerificationResult::failed(
        "request_rejected",
        format!("The MCP endpoint rejected the protocol request with {status}."),
        Some("Check that the URL and transport match the MCP server.".to_string()),
    )
}

async fn verify_stdio_mcp_server(
    state: &AppState,
    server: &McpServerConfig,
    agent_id: &str,
) -> McpVerificationResult {
    let config = state.config();
    let registry_name = AgentRegistry::new(state.db.clone())
        .get(agent_id)
        .ok()
        .map(|record| record.name);
    let agent = config.agents.iter().find(|agent| {
        agent.name == agent_id || registry_name.as_deref() == Some(agent.name.as_str())
    });
    let Some(agent) = agent else {
        return McpVerificationResult::failed(
            "project_not_found",
            "The selected project no longer has a runner configuration.",
            Some("Reload the project and try again.".to_string()),
        );
    };

    let kind = match resolve_runner_kind(agent) {
        Ok(kind) => kind,
        Err(error) => {
            return McpVerificationResult::failed(
                "runner_unavailable",
                format!("The project runner could not be resolved: {error}"),
                Some("Save a valid ACP agent configuration first.".to_string()),
            );
        }
    };
    let image = match resolved_runner_image(&agent.runner, &kind) {
        Ok(image) => image,
        Err(error) => {
            return McpVerificationResult::failed(
                "runner_unavailable",
                format!("The project runner image could not be resolved: {error}"),
                Some("Save a valid runner image first.".to_string()),
            );
        }
    };
    let Some(docker) = state.docker().await else {
        return McpVerificationResult::failed(
            "runner_unavailable",
            "Docker or Podman is not available, so the runner image could not be checked.",
            Some("Start the configured container runtime and try again.".to_string()),
        );
    };
    let runtime_image = if docker.has_image(&image).await {
        Some(image)
    } else if let Some(local_image) = local_runner_image_alias(&image) {
        docker
            .has_image(local_image)
            .await
            .then(|| local_image.to_string())
    } else {
        None
    };
    let Some(runtime_image) = runtime_image else {
        return McpVerificationResult::failed(
            "runner_image_missing",
            "The selected project's runner image is not available locally.",
            Some("Prepare the project runner, then verify the MCP server again.".to_string()),
        );
    };

    let Some(command) = server
        .command
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty())
    else {
        return McpVerificationResult::failed(
            "invalid_configuration",
            "The stdio MCP server has no command.",
            Some("Edit the server and provide an absolute command path.".to_string()),
        );
    };
    let basename = FsPath::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let script = r#"configured="$1"
name="$2"
if [ -x "$configured" ]; then
  printf 'FOUND:%s\n' "$configured"
  exit 0
fi
if [ -n "$name" ]; then
  alternative="$(command -v "$name" 2>/dev/null || true)"
  if [ -n "$alternative" ]; then
    printf 'ALTERNATIVE:%s\n' "$alternative"
    exit 3
  fi
fi
printf 'MISSING:%s\n' "$configured"
exit 4
"#;
    let workload_id = format!("mcp-verify-{}", uuid::Uuid::new_v4().simple());
    let spec = ContainerSpec {
        image: runtime_image,
        memory_limit: Some(256 * 1024 * 1024),
        cpu_limit: None,
        environment: Vec::new(),
        volumes: Vec::new(),
        network_mode: Some("none".to_string()),
        expose_port: None,
        cmd: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            script.to_string(),
            "mcp-verify".to_string(),
            command.to_string(),
            basename.to_string(),
        ]),
        working_dir: None,
        run_as_host_user: false,
    };

    if let Err(error) = docker.launch(&workload_id, &spec).await {
        warn!(%error, "failed to launch stdio MCP verification container");
        let _ = docker.stop(&workload_id).await;
        return McpVerificationResult::failed(
            "runner_unavailable",
            "XpressClaw could not start the runner image to verify this command.",
            Some(
                "Check the project runner image and container runtime, then try again.".to_string(),
            ),
        );
    }

    let output =
        match tokio::time::timeout(Duration::from_secs(15), docker.wait_for_exit(&workload_id))
            .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                warn!(%error, "stdio MCP verification container failed");
                let _ = docker.stop(&workload_id).await;
                return McpVerificationResult::failed(
                    "runner_unavailable",
                    "XpressClaw could not inspect the command in the runner image.",
                    Some("Check the project runner image and try again.".to_string()),
                );
            }
            Err(_) => {
                let _ = docker.stop(&workload_id).await;
                return McpVerificationResult::failed(
                    "verification_timeout",
                    "The runner image check did not finish within 15 seconds.",
                    Some("Check the container runtime and try again.".to_string()),
                );
            }
        };

    stdio_verification_from_output(command, output.status_code, &output.output)
}

fn stdio_verification_from_output(
    command: &str,
    status_code: i64,
    output: &str,
) -> McpVerificationResult {
    if status_code == 0 && output.lines().any(|line| line.starts_with("FOUND:")) {
        return McpVerificationResult::ready(format!(
            "The command {command} is executable in this project's base runner image."
        ));
    }
    if let Some(alternative) = output
        .lines()
        .find_map(|line| line.strip_prefix("ALTERNATIVE:"))
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        return McpVerificationResult::failed(
            "command_path_incorrect",
            format!("{command} is not executable in this project's base runner image."),
            Some(format!("Use {alternative} instead.")),
        );
    }
    if status_code == 4 || output.lines().any(|line| line.starts_with("MISSING:")) {
        return McpVerificationResult::failed(
            "command_missing",
            format!("{command} is not executable in this project's base runner image."),
            Some(
                "Install it in the runner image, correct its path, or provide it with a project startup command."
                    .to_string(),
            ),
        );
    }
    McpVerificationResult::failed(
        "verification_failed",
        "The runner could not verify the stdio command.",
        Some(
            "Check that the runner image contains /bin/sh and the configured command.".to_string(),
        ),
    )
}

/// Delete an MCP server from the global config.
async fn delete_mcp_server(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config_guard = state.config_write_lock.lock().await;
    let old_config = state.config();

    let mut new_mcp = old_config.mcp_servers.clone();
    if new_mcp.remove(&name).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("MCP server '{name}' not found") })),
        ));
    }

    let mut agents = old_config.agents.clone();
    for agent in &mut agents {
        agent.runner.mcp_servers.retain(|server| server != &name);
    }

    let new_config = Config {
        mcp_servers: new_mcp,
        agents,
        instance: old_config.instance.clone(),
        collaboration: old_config.collaboration.clone(),
        llm: old_config.llm.clone(),
        system: old_config.system.clone(),
        tools: old_config.tools.clone(),
        tool_policies: old_config.tool_policies.clone(),
        memory: old_config.memory.clone(),
    };
    new_config
        .save(&state.config_path)
        .map_err(internal_error)?;

    let new_config = std::sync::Arc::new(new_config);
    state.apply_config(new_config, state.llm_router());

    Ok(Json(json!({ "success": true, "deleted": name })))
}

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

fn bad_request(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": e.to_string() })),
    )
}

fn not_found(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": e.to_string() })),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use xpressclaw_core::config::{Config, ToolConfig};
    use xpressclaw_core::db::Database;
    use xpressclaw_core::tools::policy::{PolicyAction, ToolPolicyRule};

    use super::*;

    fn test_config_path() -> std::path::PathBuf {
        std::env::temp_dir().join("test-xpressclaw-setup.yaml")
    }

    fn test_app() -> Router {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db, None, test_config_path(), false);

        Router::new().nest("/setup", routes()).with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn remote_mcp_verification_reports_authentication_failures() {
        let result = remote_verification_from_status(StatusCode::UNAUTHORIZED);
        assert!(!result.ok);
        assert_eq!(result.status, "authentication_required");
        assert!(result.message.contains("401"));
        assert!(result
            .suggestion
            .as_deref()
            .is_some_and(|suggestion| suggestion.contains("Authorization")));
    }

    #[tokio::test]
    async fn remote_mcp_verification_sends_headers_and_initialize_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/mcp",
            post(
                |headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
                    let authorized = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        == Some("Bearer test-token");
                    if authorized && body["method"] == "initialize" {
                        (
                            StatusCode::OK,
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": {
                                    "protocolVersion": "2025-06-18",
                                    "capabilities": {},
                                    "serverInfo": {
                                        "name": "test-server",
                                        "version": "1.0.0"
                                    }
                                }
                            })),
                        )
                    } else {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(json!({ "error": "bad request" })),
                        )
                    }
                },
            ),
        );
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let server = McpServerConfig {
            server_type: "http".to_string(),
            url: Some(format!("http://{address}/mcp")),
            headers: std::collections::HashMap::from([(
                "Authorization".to_string(),
                "Bearer test-token".to_string(),
            )]),
            ..Default::default()
        };

        let result = verify_remote_mcp_server(&server).await;
        server_task.abort();

        assert!(result.ok);
        assert_eq!(result.status, "ready");
    }

    #[tokio::test]
    async fn remote_mcp_verification_rejects_a_successful_login_page() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/mcp",
            post(|| async {
                axum::response::Html("<!doctype html><html><body>Please sign in</body></html>")
            }),
        );
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let server = McpServerConfig {
            server_type: "http".to_string(),
            url: Some(format!("http://{address}/mcp")),
            ..Default::default()
        };

        let result = verify_remote_mcp_server(&server).await;
        server_task.abort();

        assert!(!result.ok);
        assert_eq!(result.status, "invalid_response");
        assert!(result
            .suggestion
            .as_deref()
            .is_some_and(|suggestion| suggestion.contains("login")));
    }

    #[tokio::test]
    async fn remote_mcp_verification_rejects_non_mcp_json() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/mcp", post(|| async { Json(json!({ "status": "ok" })) }));
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let server = McpServerConfig {
            server_type: "http".to_string(),
            url: Some(format!("http://{address}/mcp")),
            ..Default::default()
        };

        let result = verify_remote_mcp_server(&server).await;
        server_task.abort();

        assert!(!result.ok);
        assert_eq!(result.status, "invalid_response");
        assert!(result.message.contains("initialize"));
    }

    #[tokio::test]
    async fn remote_mcp_verification_accepts_streamable_http_sse_initialize_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/mcp",
            post(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    concat!(
                        "event: message\n",
                        "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":",
                        "{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},",
                        "\"serverInfo\":{\"name\":\"test-server\",\"version\":\"1.0.0\"}}}\n\n"
                    ),
                )
            }),
        );
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let server = McpServerConfig {
            server_type: "http".to_string(),
            url: Some(format!("http://{address}/mcp")),
            ..Default::default()
        };

        let result = verify_remote_mcp_server(&server).await;
        server_task.abort();

        assert!(result.ok);
        assert_eq!(result.status, "ready");
    }

    #[tokio::test]
    async fn remote_mcp_verification_requires_a_valid_legacy_sse_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/events",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    "event: endpoint\ndata: /messages?session=test\n\n",
                )
            }),
        );
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let server = McpServerConfig {
            server_type: "sse".to_string(),
            url: Some(format!("http://{address}/events")),
            ..Default::default()
        };

        let result = verify_remote_mcp_server(&server).await;
        server_task.abort();

        assert!(result.ok);
        assert_eq!(result.status, "ready");
    }

    #[tokio::test]
    async fn remote_mcp_verification_rejects_sse_without_an_endpoint_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/events",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    "event: message\ndata: connected\n\n",
                )
            }),
        );
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let server = McpServerConfig {
            server_type: "sse".to_string(),
            url: Some(format!("http://{address}/events")),
            ..Default::default()
        };

        let result = verify_remote_mcp_server(&server).await;
        server_task.abort();

        assert!(!result.ok);
        assert_eq!(result.status, "invalid_response");
        assert!(result.message.contains("handshake"));
    }

    #[test]
    fn initialize_response_validation_requires_mcp_result_fields() {
        assert_eq!(
            validate_initialize_response(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "serverInfo": {
                        "name": "test-server",
                        "version": "1.0.0"
                    }
                }
            })),
            InitializeResponse::Ready
        );
        assert_eq!(
            validate_initialize_response(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {}
                }
            })),
            InitializeResponse::Invalid
        );
    }

    #[test]
    fn sse_parser_ignores_incomplete_events() {
        assert!(complete_sse_events(b"event: endpoint\ndata: /messages").is_empty());
        assert_eq!(
            complete_sse_events(b": keepalive\n\nevent: endpoint\ndata: /messages\n\n"),
            vec![SseEvent {
                event: Some("endpoint".to_string()),
                data: "/messages".to_string(),
            }]
        );
    }

    #[test]
    fn stdio_mcp_verification_suggests_the_executable_found_in_the_runner() {
        let result =
            stdio_verification_from_output("/usr/bin/npx", 3, "ALTERNATIVE:/usr/local/bin/npx\n");
        assert!(!result.ok);
        assert_eq!(result.status, "command_path_incorrect");
        assert_eq!(
            result.suggestion.as_deref(),
            Some("Use /usr/local/bin/npx instead.")
        );
    }

    #[test]
    fn stdio_mcp_verification_reports_a_missing_executable() {
        let result =
            stdio_verification_from_output("/opt/mcp/server", 4, "MISSING:/opt/mcp/server\n");
        assert!(!result.ok);
        assert_eq!(result.status, "command_missing");
        assert!(result.message.contains("/opt/mcp/server is not executable"));
    }

    #[test]
    fn setup_selects_the_image_for_the_builtin_acp_agent() {
        let setup: AgentSetup = serde_json::from_value(json!({
            "name": "reviewer",
            "runner_kind": "claude"
        }))
        .unwrap();
        let runner = runner_from_setup(&setup);
        assert_eq!(runner.kind, "claude");
        assert_eq!(
            runner.image,
            "ghcr.io/xpressai/xpressclaw-runner-claude:latest"
        );
    }

    #[test]
    fn setup_selects_the_docker_cli_variant_for_host_engine_access() {
        let setup: AgentSetup = serde_json::from_value(json!({
            "runner_kind": "codex",
            "runner_image": "ghcr.io/xpressai/xpressclaw-runner-codex:latest",
            "runner_container_engine": "host"
        }))
        .unwrap();
        let runner = runner_from_setup(&setup);
        assert_eq!(runner.container_engine, ContainerEngineAccess::Host);
        assert_eq!(
            runner.image,
            "ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest"
        );
    }

    #[test]
    fn setup_preserves_explicit_ssh_agent_forwarding() {
        let setup: AgentSetup = serde_json::from_value(json!({
            "runner_kind": "codex",
            "ssh_agent_forwarding": true
        }))
        .unwrap();
        let runner = runner_from_setup(&setup);
        assert!(runner.ssh_agent_forwarding);
    }

    #[test]
    fn recognizes_ssh_git_remote_forms_without_confusing_local_paths() {
        for remote in [
            "git@github.com:XpressAI/xpressclaw.git",
            "ssh://git@gitlab.example/team/repo.git",
            "work-github:team/repo.git",
        ] {
            assert!(is_ssh_git_remote(remote), "{remote}");
        }
        for remote in [
            "https://github.com/XpressAI/xpressclaw.git",
            "file:///srv/repos/xpressclaw.git",
            "/srv/repos/xpressclaw.git",
            r"C:\repos\xpressclaw",
        ] {
            assert!(!is_ssh_git_remote(remote), "{remote}");
        }
    }

    #[test]
    fn setup_selects_an_expanded_catalog_runner() {
        let setup: AgentSetup = serde_json::from_value(json!({
            "runner_kind": "qwen"
        }))
        .unwrap();
        let runner = runner_from_setup(&setup);
        assert_eq!(
            runner.image,
            "ghcr.io/xpressai/xpressclaw-runner-qwen:latest"
        );
    }

    #[test]
    fn setup_selects_deepseek_harness_by_catalog_id_or_dsh_alias() {
        for kind in ["deepseek-harness", "dsh"] {
            let setup: AgentSetup = serde_json::from_value(json!({
                "runner_kind": kind
            }))
            .unwrap();
            let runner = runner_from_setup(&setup);
            assert_eq!(runner.kind, "deepseek-harness");
            assert_eq!(
                runner.image,
                "ghcr.io/xpressai/xpressclaw-runner-deepseek-harness:latest"
            );
        }
    }

    #[test]
    fn setup_does_not_reclassify_an_explicit_custom_kind() {
        let setup: AgentSetup = serde_json::from_value(json!({
            "runner_kind": "codex-proxy",
            "backend": "codex",
            "runner_command": ["codex-proxy", "acp"]
        }))
        .unwrap();
        assert_eq!(runner_kind_from_setup(&setup), "codex-proxy");
    }

    #[test]
    fn managed_workspace_creates_a_durable_empty_project_folder() {
        let setup: AgentSetup = serde_json::from_value(json!({
            "runner_kind": "codex",
            "workspace_mode": "managed",
            "project_name": "Clone this later"
        }))
        .unwrap();
        let mut runner = runner_from_setup(&setup);
        let root =
            std::env::temp_dir().join(format!("xpressclaw-managed-test-{}", std::process::id()));
        assign_managed_workspace(&setup, &mut runner, "clone-this-later-codex", &root).unwrap();

        let workspace = PathBuf::from(runner.workspace.as_deref().unwrap());
        assert!(workspace.is_dir());
        assert_eq!(runner_context(&runner), "Clone this later");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn agent_catalog_lists_every_supported_product() {
        let catalog = agent_catalog().await.0;
        let agents = catalog["agents"].as_array().unwrap();
        assert_eq!(agents.len(), ACP_AGENTS.len());
        assert!(agents.iter().any(|agent| agent["kind"] == "cursor"));
        assert!(agents.iter().any(|agent| agent["kind"] == "mistral-vibe"));
        let dsh = agents
            .iter()
            .find(|agent| agent["kind"] == "deepseek-harness")
            .unwrap();
        assert_eq!(dsh["command"], json!(["dsh-acp"]));
        assert_eq!(dsh["login_command"], "dsh-acp login");
        assert_eq!(dsh["mark"], "DS");
        assert!(!agents.iter().any(|agent| agent["kind"] == "agoragentic"));
    }

    #[tokio::test]
    async fn environment_inspection_suggests_opt_in_commands() {
        let workspace = std::env::temp_dir().join(format!(
            "xpressclaw-environment-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("compose.yaml"), "services: {}\n").unwrap();
        std::fs::write(workspace.join("package-lock.json"), "{}\n").unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&workspace)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:XpressAI/xpressclaw.git",
            ])
            .status()
            .unwrap()
            .success());

        let response = project_environment(Query(ProjectEnvironmentQuery {
            path: workspace.display().to_string(),
        }))
        .await
        .unwrap()
        .0;
        let suggestions = response["suggestions"].as_array().unwrap();
        assert!(suggestions
            .iter()
            .any(|suggestion| suggestion["command"] == "docker compose up -d"));
        assert!(suggestions
            .iter()
            .any(|suggestion| suggestion["command"] == "npm ci"));
        assert_eq!(response["git_repository"], true);
        assert_eq!(response["git_uses_ssh"], true);

        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn detects_ssh_push_urls_when_fetching_over_https() {
        let workspace = std::env::temp_dir().join(format!(
            "xpressclaw-ssh-pushurl-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&workspace)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/XpressAI/xpressclaw.git",
            ])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args([
                "remote",
                "set-url",
                "--add",
                "--push",
                "origin",
                "git@github.com:XpressAI/xpressclaw.git",
            ])
            .status()
            .unwrap()
            .success());

        assert!(repository_uses_ssh_remote(&workspace));

        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[tokio::test]
    async fn package_manager_suggestions_do_not_enable_global_corepack_shims() {
        for (lockfile, expected_command) in [
            (
                "pnpm-lock.yaml",
                "COREPACK_HOME=/tmp/xpressclaw-corepack corepack pnpm install --frozen-lockfile",
            ),
            (
                "yarn.lock",
                "COREPACK_HOME=/tmp/xpressclaw-corepack corepack yarn install --immutable",
            ),
        ] {
            let workspace = std::env::temp_dir().join(format!(
                "xpressclaw-corepack-test-{}-{}",
                std::process::id(),
                lockfile
            ));
            std::fs::create_dir_all(&workspace).unwrap();
            std::fs::write(workspace.join(lockfile), "").unwrap();

            let response = project_environment(Query(ProjectEnvironmentQuery {
                path: workspace.display().to_string(),
            }))
            .await
            .unwrap()
            .0;
            let commands: Vec<&str> = response["suggestions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|suggestion| suggestion["command"].as_str())
                .collect();
            assert!(commands.contains(&expected_command));
            assert!(commands
                .iter()
                .all(|command| !command.contains("corepack enable")));

            std::fs::remove_dir_all(workspace).unwrap();
        }
    }

    #[test]
    fn setup_preserves_a_custom_acp_server_command() {
        let setup: AgentSetup = serde_json::from_value(json!({
            "runner_kind": "custom",
            "runner_image": "example/acp-agent:latest",
            "runner_model": "  opus  ",
            "runner_command": ["example-agent", "  acp  ", ""]
        }))
        .unwrap();
        let runner = runner_from_setup(&setup);
        assert_eq!(runner.kind, "custom");
        assert_eq!(runner.model.as_deref(), Some("opus"));
        assert_eq!(runner.command, vec!["example-agent", "acp"]);
        assert!(validate_runner(&runner).is_ok());
    }

    #[tokio::test]
    async fn add_session_returns_the_logical_session_id() {
        let config_path = std::env::temp_dir().join("test-xpressclaw-add-session.yaml");
        let _ = std::fs::remove_file(&config_path);

        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects (id, name) VALUES ('website-project', 'Website')",
                [],
            )
        })
        .unwrap();
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db.clone(), None, config_path.clone(), false);
        let app = Router::new().nest("/setup", routes()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/add-session")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "project_id": "website-project",
                            "runner_kind": "codex",
                            "runner_workspace": "/tmp/website"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["session"], "website-codex");
        assert_eq!(body["title"], "website");
        assert_eq!(body["project_id"], "website-project");
        assert!(body["session_id"].as_str().is_some_and(|id| !id.is_empty()));
        let project_id: String = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT project_id FROM agents WHERE id = 'website-codex'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(project_id, "website-project");

        let saved = Config::load(&config_path).unwrap();
        assert_eq!(saved.agents.len(), 1);
        assert_eq!(
            saved.agents[0].runner.workspace.as_deref(),
            Some("/tmp/website")
        );
        let _ = std::fs::remove_file(&config_path);
    }

    #[tokio::test]
    async fn add_session_rejects_a_missing_project_without_persisting_an_agent() {
        let config_path = std::env::temp_dir().join(format!(
            "test-xpressclaw-add-session-missing-project-{}.yaml",
            uuid::Uuid::new_v4().simple()
        ));
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db.clone(), None, config_path.clone(), false);
        let app = Router::new().nest("/setup", routes()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/add-session")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "project_id": "deleted-project",
                            "runner_kind": "codex",
                            "runner_workspace": "/tmp/website"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(AgentRegistry::new(db).get("website-codex").is_err());
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn add_session_preserves_custom_top_level_configuration() {
        let config_path =
            std::env::temp_dir().join("test-xpressclaw-add-session-preserves-config.yaml");
        let _ = std::fs::remove_file(&config_path);

        let db = Arc::new(Database::open_memory().unwrap());
        let mut config = Config::load_default().unwrap();
        config.tools.insert(
            "shell".into(),
            ToolConfig {
                enabled: false,
                confirmation_required: true,
                ..Default::default()
            },
        );
        config.tool_policies.push(ToolPolicyRule {
            pattern: "dangerous_*".into(),
            action: PolicyAction::Deny,
            approval: None,
        });
        config.memory.near_term_slots = 3;
        config.memory.eviction = "custom-eviction".into();

        let state = AppState::new(Arc::new(config), db, None, config_path.clone(), false);
        let app = Router::new()
            .nest("/setup", routes())
            .with_state(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/add-session")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "runner_kind": "claude",
                            "runner_workspace": "/tmp/preserved-project"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let saved = Config::load(&config_path).unwrap();
        assert_eq!(saved.agents.len(), 1);
        assert!(!saved.tools["shell"].enabled);
        assert!(saved.tools["shell"].confirmation_required);
        assert_eq!(saved.tool_policies.len(), 1);
        assert_eq!(saved.tool_policies[0].pattern, "dangerous_*");
        assert_eq!(saved.memory.near_term_slots, 3);
        assert_eq!(saved.memory.eviction, "custom-eviction");

        let live = state.config();
        assert_eq!(live.memory.near_term_slots, 3);
        assert_eq!(live.tool_policies[0].pattern, "dangerous_*");

        let _ = std::fs::remove_file(&config_path);
    }

    #[tokio::test]
    async fn test_setup_status() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/setup/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["setup_complete"], false);
    }

    #[tokio::test]
    async fn config_identifies_the_control_plane_instance() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/setup/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(
            body["instance"]["config_path"],
            test_config_path().display().to_string()
        );
        assert!(body["instance"]["data_dir"].as_str().is_some());
        assert!(body["instance"]["workspace_dir"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_system_info() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/setup/system-info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["total_memory_gb"].as_f64().unwrap() > 0.0);
        assert!(body["cpu_count"].as_u64().unwrap() > 0);
        assert!(body["working_directory"].as_str().is_some());
        assert!(body["ssh_agent_available"].as_bool().is_some());
        assert!(body.get("ssh_agent_socket").is_some());
    }

    #[tokio::test]
    async fn test_recommend_model() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/setup/recommend-model")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["model"].as_str().is_some());
        assert!(body["all_options"].as_array().is_some());
    }

    #[tokio::test]
    async fn test_complete_setup() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "agents": [
                                {
                                    "runner_kind": "codex",
                                    "runner_workspace": "/tmp/website"
                                }
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["success"], true);

        // Verify config was written
        let config_path = test_config_path();
        assert!(config_path.exists());
        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.agents[0].name, "website-codex");
        assert_eq!(config.agents[0].context_label(), "website");
        assert!(config.agents[0].llm.is_none());

        // Native products own their agent/tool loop; setup must not inject
        // the retired xpressclaw agent-layer skills or MCP servers.
        assert!(config.agents[0].skills.is_empty());
        assert!(config.mcp_servers.is_empty());

        // Cleanup
        let _ = std::fs::remove_file(config_path);
    }

    #[tokio::test]
    async fn complete_setup_preserves_instance_local_system_paths() {
        let root = tempfile::tempdir().unwrap();
        let instance = root.path().join("instance");
        std::fs::create_dir_all(&instance).unwrap();
        let config_path = instance.join("xpressclaw.yaml");
        let mut config = Config::default();
        config.system.data_dir = instance.clone();
        config.system.workspace_dir = instance.join("workspaces");
        let state = AppState::new(
            Arc::new(config),
            Arc::new(Database::open_memory().unwrap()),
            None,
            config_path.clone(),
            false,
        );
        let app = Router::new().nest("/setup", routes()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "agents": [] }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let saved = Config::load(&config_path).unwrap();
        assert_eq!(saved.system.data_dir, instance);
        assert_eq!(saved.system.workspace_dir, instance.join("workspaces"));
    }

    #[tokio::test]
    async fn complete_setup_preserves_existing_top_level_configuration() {
        let root = tempfile::tempdir().unwrap();
        let config_path = root.path().join("xpressclaw.yaml");
        let mut config = Config::default();
        config.system.budget.daily = Some("$12.00".into());
        config.tools.insert(
            "shell".into(),
            ToolConfig {
                enabled: false,
                confirmation_required: true,
                ..Default::default()
            },
        );
        config.tool_policies.push(ToolPolicyRule {
            pattern: "dangerous_*".into(),
            action: PolicyAction::Deny,
            approval: None,
        });
        config.memory.near_term_slots = 3;
        config.memory.eviction = "custom-eviction".into();
        config.mcp_servers.insert(
            "existing-connector".into(),
            McpServerConfig {
                server_type: "http".into(),
                url: Some("https://mcp.example.test".into()),
                ..Default::default()
            },
        );

        let state = AppState::new(
            Arc::new(config),
            Arc::new(Database::open_memory().unwrap()),
            None,
            config_path.clone(),
            false,
        );
        let app = Router::new()
            .nest("/setup", routes())
            .with_state(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "agents": [{
                                "runner_kind": "codex",
                                "runner_workspace": "/tmp/preserved-project"
                            }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let saved = Config::load(&config_path).unwrap();
        assert_eq!(saved.agents.len(), 1);
        assert_eq!(saved.system.budget.daily.as_deref(), Some("$12.00"));
        assert!(!saved.tools["shell"].enabled);
        assert!(saved.tools["shell"].confirmation_required);
        assert_eq!(saved.tool_policies[0].pattern, "dangerous_*");
        assert_eq!(saved.memory.near_term_slots, 3);
        assert_eq!(saved.memory.eviction, "custom-eviction");
        assert!(saved.mcp_servers.contains_key("existing-connector"));

        let live = state.config();
        assert_eq!(live.system.budget.daily.as_deref(), Some("$12.00"));
        assert_eq!(live.memory.near_term_slots, 3);
        assert!(live.mcp_servers.contains_key("existing-connector"));
    }

    /// Verify native session configuration round-trips without profiles.
    #[tokio::test]
    async fn test_wizard_writes_valid_config_with_mcp_servers() {
        // Use a unique temp path to avoid collisions with other tests.
        let config_path = std::env::temp_dir().join("test-xpressclaw-wizard-mcp.yaml");
        let _ = std::fs::remove_file(&config_path);

        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db, None, config_path.clone(), false);
        let app = Router::new()
            .nest("/setup", routes())
            .with_state(state.clone());

        // ── Step 1: full setup for one project context ──
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "agents": [
                                {
                                    "runner_kind": "codex",
                                    "runner_workspace": "/tmp/research"
                                }
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        // Load and validate the written config
        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].name, "research-codex");
        assert!(config.agents[0].tools.is_empty());
        assert!(config.mcp_servers.is_empty());

        // Verify the YAML round-trips: save it again, reload, still valid
        let roundtrip_path = std::env::temp_dir().join("test-xpressclaw-wizard-roundtrip.yaml");
        config.save(&roundtrip_path).unwrap();
        let reloaded = Config::load(&roundtrip_path).unwrap();
        assert_eq!(reloaded.agents[0].name, "research-codex");
        assert!(reloaded.mcp_servers.is_empty());
        let _ = std::fs::remove_file(&roundtrip_path);

        // ── Step 2: add another native session ──
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/add-session")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "runner_kind": "claude",
                            "runner_workspace": "/tmp/developer"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        // Reload config — both native sessions should be preserved.
        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.agents.len(), 2);
        let agent_names: Vec<&str> = config.agents.iter().map(|a| a.name.as_str()).collect();
        assert!(agent_names.contains(&"research-codex"));
        assert!(agent_names.contains(&"developer-claude"));

        assert!(config.mcp_servers.is_empty());

        // Cleanup
        let _ = std::fs::remove_file(&config_path);
    }

    /// Explicit MCP server configuration is still preserved.
    #[tokio::test]
    async fn test_explicit_mcp_servers_are_preserved() {
        let config_path = std::env::temp_dir().join("test-xpressclaw-wizard-override.yaml");
        let _ = std::fs::remove_file(&config_path);

        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db, None, config_path.clone(), false);
        let app = Router::new().nest("/setup", routes()).with_state(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "agents": [{
                                "runner_kind": "opencode",
                                "runner_workspace": "/tmp/research"
                            }],
                            "mcp_servers": {
                                "websearch": {
                                    "type": "stdio",
                                    "command": "npx",
                                    "args": ["-y", "duckduckgo-mcp-server"],
                                    "env": { "SEARCH_LANG": "en" }
                                }
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let config = Config::load(&config_path).unwrap();
        let ws_cfg = config
            .mcp_servers
            .get("websearch")
            .expect("websearch MCP server missing");
        assert_eq!(
            ws_cfg.env.get("SEARCH_LANG").map(|s| s.as_str()),
            Some("en"),
            "frontend env overrides should be preserved"
        );

        let _ = std::fs::remove_file(&config_path);
    }

    #[tokio::test]
    async fn duplicate_project_contexts_get_unique_session_ids() {
        let config_path = std::env::temp_dir().join("test-xpressclaw-wizard-add-override.yaml");
        let _ = std::fs::remove_file(&config_path);

        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db, None, config_path.clone(), false);
        let app = Router::new().nest("/setup", routes()).with_state(state);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "agents": [{
                                "runner_kind": "codex",
                                "runner_workspace": "/tmp/website"
                            }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/add-session")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "runner_kind": "codex",
                            "runner_workspace": "/tmp/website"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.agents[0].name, "website-codex");
        assert_eq!(config.agents[1].name, "website-codex-2");

        let _ = std::fs::remove_file(&config_path);
    }
}
