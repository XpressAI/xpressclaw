use std::sync::Arc;

use tracing::{info, warn};
use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::config::{self, Config};
use xpressclaw_core::db::Database;
use xpressclaw_core::docker::manager::DockerManager;
use xpressclaw_server::server;
use xpressclaw_server::state::AppState;

pub async fn run(detach: bool, port: u16, workdir: Option<String>) -> anyhow::Result<()> {
    if detach {
        return run_detached(port);
    }

    run_foreground(port, workdir).await
}

/// Run the server in the foreground (default).
async fn run_foreground(port: u16, workdir: Option<String>) -> anyhow::Result<()> {
    let state = build_state(port, workdir).await?;

    if !state.is_setup_complete() {
        println!("xpressclaw is starting in setup mode...");
        println!();
        println!("  Open http://localhost:{port} to complete setup.");
        println!();
        println!("Press Ctrl+C to stop.");
    } else {
        println!("xpressclaw is starting...");
        println!("  Web UI: http://localhost:{port}");
        println!("  API:    http://localhost:{port}/api");
        println!("  LLM:    http://localhost:{port}/v1");

        // Check LLM availability
        let config = state.config();
        if config.llm.openai_api_key.is_some() || config.llm.anthropic_api_key.is_some() {
            println!("  LLM:    cloud provider configured");
        } else if config.llm.local_model.is_some() {
            let model = config.llm.local_model.as_deref().unwrap_or("unknown");
            match reqwest::get("http://localhost:11434/api/tags").await {
                Ok(resp) if resp.status().is_success() => {
                    println!("  LLM:    Ollama ({model})");
                }
                _ => {
                    println!();
                    println!("  Warning: Ollama is not running.");
                    println!("  Chat and agent tasks need a local LLM.");
                    println!("  Start Ollama: `ollama serve`");
                    println!("  Pull model:   `ollama pull {model}`");
                }
            }
        } else {
            println!();
            println!("  Warning: No LLM provider configured.");
            println!("  Set OPENAI_API_KEY, ANTHROPIC_API_KEY, or install Ollama.");
        }

        println!();
        println!("Press Ctrl+C to stop.");
    }

    server::serve(state, port).await?;

    Ok(())
}

/// Spawn the server as a detached background process.
///
/// Re-executes `xpressclaw up` (without --detach) in a new process,
/// redirecting stdout/stderr to a log file.
fn run_detached(port: u16) -> anyhow::Result<()> {
    use std::fs::File;
    use std::process::Command;

    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))?;
    let data_dir = std::path::Path::new(&home).join(".xpressclaw");
    std::fs::create_dir_all(&data_dir)?;
    let log_path = data_dir.join("server.log");
    let pid_path = data_dir.join("server.pid");

    // Check if already running
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                // Check if process is still alive (signal 0 = check existence)
                let alive = Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .output()
                    .is_ok_and(|o| o.status.success());
                if alive {
                    println!("xpressclaw is already running (pid {pid}).");
                    println!("  Web UI: http://localhost:{port}");
                    println!("  Logs:   {}", log_path.display());
                    return Ok(());
                }
            }
        }
    }

    let exe = std::env::current_exe()?;
    let log_file = File::create(&log_path)?;
    let err_file = log_file.try_clone()?;

    let child = Command::new(exe)
        .args(["up", "--port", &port.to_string()])
        .stdout(log_file)
        .stderr(err_file)
        .stdin(std::process::Stdio::null())
        .spawn()?;

    let pid = child.id();
    std::fs::write(&pid_path, pid.to_string())?;

    println!("xpressclaw started in background (pid {pid}).");
    println!("  Web UI: http://localhost:{port}");
    println!("  Logs:   {}", log_path.display());
    println!("  PID:    {}", pid_path.display());
    println!();
    println!("Stop with `xpressclaw down`.");

    Ok(())
}

/// Build the AppState (shared between foreground and detached modes).
async fn build_state(port: u16, workdir: Option<String>) -> anyhow::Result<AppState> {
    let work_dir = match workdir {
        Some(dir) => std::path::PathBuf::from(dir),
        None => std::env::current_dir().unwrap_or_default(),
    };
    let config_path = work_dir.join("xpressclaw.yaml");

    // Check if config exists — if not, start in setup mode
    if !config_path.exists() {
        info!("no config file found — starting in setup mode");
        let config = Config::default();
        let db_path = config.system.data_dir.join("xpressclaw.db");
        std::fs::create_dir_all(&config.system.data_dir).ok();
        let db = Arc::new(Database::open(&db_path)?);

        return Ok(AppState::new(
            Arc::new(config),
            db,
            None,
            config_path,
            false,
        ));
    }

    // Load config from the resolved path
    let mut config = Config::load(&config_path)?;
    config::env_overrides(&mut config);

    info!(agents = config.agents.len(), "loaded configuration");

    // Validate Docker/Podman is available
    match DockerManager::connect().await {
        Ok(_) => info!("container runtime available"),
        Err(e) => {
            warn!(error = %e, "Docker/Podman not available — some features will be limited");
        }
    }

    // Open database
    let db_path = config.system.data_dir.join("xpressclaw.db");
    std::fs::create_dir_all(&config.system.data_dir).ok();
    let db = Arc::new(Database::open(&db_path)?);
    info!(path = %db_path.display(), "database ready");

    // Sync agents from YAML config into DB runtime state table.
    // Config (role, model, tools, llm, etc.) always comes from the YAML.
    // The DB only tracks runtime state (status, container_id, timestamps).
    let registry = AgentRegistry::new(db.clone());
    let valid_names: Vec<&str> = config.agents.iter().map(|a| a.name.as_str()).collect();
    registry.remove_stale(&valid_names).unwrap_or_default();
    for agent_config in &config.agents {
        match registry.ensure(&agent_config.name, &agent_config.backend) {
            Ok(record) => info!(
                name = record.name,
                backend = record.backend,
                status = record.status,
                "synced agent"
            ),
            Err(e) => warn!(name = agent_config.name, error = %e, "failed to sync agent"),
        }
    }

    // Build LLM router
    let config = Arc::new(config);
    let llm_router = {
        use xpressclaw_core::llm::router::LlmRouter;
        LlmRouter::build_from_config(&config.llm)
    };

    let _ = port; // available for future use (e.g., logging)

    let state = AppState::new(config, db, Some(Arc::new(llm_router)), config_path, true);

    // No agent startup here — the reconciler (ADR-018) handles all container
    // lifecycle: pulling images, starting agents with desired_status='running',
    // restarting crashed containers, and re-queuing orphaned tasks.
    // It runs as a background task started in server::serve().

    Ok(state)
}
