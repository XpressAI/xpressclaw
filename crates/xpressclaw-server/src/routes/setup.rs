use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use tracing::{info, warn};
use xpressclaw_core::acp::ACP_AGENTS;
use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::config::{
    context_label, default_native_runner_image, unique_session_id, AgentConfig, Config,
    ContainerEngineAccess, LlmConfig, McpServerConfig, NativeRunnerConfig,
};
use xpressclaw_core::llm::anthropic::AnthropicProvider;
use xpressclaw_core::llm::local::detect_ollama;
use xpressclaw_core::llm::openai::OpenAiProvider;
use xpressclaw_core::llm::router::LlmRouter;
use xpressclaw_core::paths::strip_verbatim;
use xpressclaw_core::system;
use xpressclaw_core::workers::native::subscription_auth_available;

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

    Ok(Json(json!({
        "workspace": display_workspace.display().to_string(),
        "detected_files": detected_files,
        "suggestions": suggestions,
    })))
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
    mcp_servers: std::collections::HashMap<String, McpServerConfig>,
    /// Isolation mode. ACP workers currently require "docker".
    #[serde(default = "default_isolation")]
    isolation: String,
}

fn default_isolation() -> String {
    "docker".into()
}

#[derive(Deserialize)]
struct AgentSetup {
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
    runner_container_engine: Option<ContainerEngineAccess>,
    volumes: Option<Vec<String>>,
    #[serde(default)]
    mcp_servers: std::collections::HashMap<String, McpServerConfig>,
}

fn runner_kind_from_setup(setup: &AgentSetup) -> String {
    let configured = setup
        .runner_kind
        .as_deref()
        .or(setup.backend.as_deref())
        .unwrap_or("codex")
        .to_lowercase();
    if configured.contains("claude") {
        "claude".to_string()
    } else if configured.contains("opencode") {
        "opencode".to_string()
    } else if configured.contains("codex") || configured == "auto" {
        "codex".to_string()
    } else {
        configured
    }
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
    // Native products own model selection, credentials, instructions, and
    // subagents. The control plane stores only session runtime context.
    let llm = LlmConfig::default();

    let managed_root = state.config().system.data_dir.clone();
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

    let mut config = Config {
        llm,
        agents,
        // ACP agents own their tool loop. Only keep connectors the user
        // explicitly configured; do not inject the retired agent-layer MCPs.
        mcp_servers: req.mcp_servers,
        ..Default::default()
    };
    config.system.isolation = req.isolation.clone();
    config.system.data_dir = managed_root;

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

/// Add a durable ACP project without replacing existing projects.
async fn add_session(
    State(state): State<AppState>,
    Json(req): Json<AgentSetup>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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
    new_config
        .save(&state.config_path)
        .map_err(internal_error)?;
    info!(
        name = agent_config.name,
        "added ACP project to configuration"
    );

    // Register the durable project. ACP workers are started per
    // attempt, so there is no long-running agent to auto-start.
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry
        .ensure(&agent_config.name, &agent_config.backend)
        .map_err(internal_error)?;
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
    })))
}

// ---------------------------------------------------------------------------
// MCP server management
// ---------------------------------------------------------------------------

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

/// Delete an MCP server from the global config.
async fn delete_mcp_server(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db, None, config_path.clone(), false);
        let app = Router::new().nest("/setup", routes()).with_state(state);

        let response = app
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

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["session"], "website-codex");
        assert_eq!(body["title"], "website");
        assert!(body["session_id"].as_str().is_some_and(|id| !id.is_empty()));

        let saved = Config::load(&config_path).unwrap();
        assert_eq!(saved.agents.len(), 1);
        assert_eq!(
            saved.agents[0].runner.workspace.as_deref(),
            Some("/tmp/website")
        );
        let _ = std::fs::remove_file(&config_path);
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
