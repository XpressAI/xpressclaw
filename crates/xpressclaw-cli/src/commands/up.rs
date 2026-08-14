use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use tracing::{info, warn};
use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::config::{self, Config};
use xpressclaw_core::db::Database;
use xpressclaw_core::docker::manager::DockerManager;
use xpressclaw_core::sessions::SessionManager;
use xpressclaw_server::server;
use xpressclaw_server::state::AppState;

use super::instance::{self, Instance, InstanceSource};

pub async fn run(
    detach: bool,
    port: u16,
    instance_dir: Option<PathBuf>,
    workdir: Option<PathBuf>,
    bind: IpAddr,
    allow_insecure_remote: bool,
) -> anyhow::Result<()> {
    validate_bind(bind, allow_insecure_remote)?;
    if workdir.is_some() {
        eprintln!("warning: --workdir is deprecated; use --instance instead");
    }
    let selected = instance::resolve(instance_dir.or(workdir))?;
    if selected.source == InstanceSource::LegacyCurrentDirectory {
        eprintln!(
            "warning: using the legacy current-directory instance at {}; use --instance to select it explicitly",
            selected.root.display()
        );
    }

    if detach {
        return run_detached(port, bind, allow_insecure_remote, &selected);
    }

    run_foreground(port, bind, &selected).await
}

/// Run the server in the foreground (default).
async fn run_foreground(port: u16, bind: IpAddr, instance: &Instance) -> anyhow::Result<()> {
    let state = build_state(instance).await?;
    let ui_url = ui_url(bind, port);

    if !state.is_setup_complete() {
        println!("XpressClaw is starting its control plane...");
        println!();
        println!("  Open {ui_url} to create your first Project and Agent.");
        println!();
        println!("Press Ctrl+C to stop.");
    } else {
        println!("XpressClaw control plane is starting...");
        println!("  Instance: {}", instance.root.display());
        println!("  Web UI:   {ui_url}");
        println!("  API:      {ui_url}/api");
        let config = state.config();
        if config.agents.is_empty() {
            println!();
            println!("  No Agents configured yet.");
            println!("  Create one at {ui_url}/setup?mode=add-session");
        } else {
            println!("  Agents:   {}", config.agents.len());
        }

        println!();
        println!("Press Ctrl+C to stop.");
    }

    server::serve_on(state, bind, port).await?;

    Ok(())
}

/// Spawn the server as a detached background process.
///
/// Re-executes `xpressclaw up` (without --detach) in a new process,
/// redirecting stdout/stderr to a log file.
fn run_detached(
    port: u16,
    bind: IpAddr,
    allow_insecure_remote: bool,
    instance: &Instance,
) -> anyhow::Result<()> {
    use std::fs::File;
    use std::process::Command;

    std::fs::create_dir_all(&instance.root)?;
    let log_path = instance.root.join("server.log");
    let pid_path = instance.root.join("server.pid");
    let ui_url = ui_url(bind, port);

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
                    println!("  Web UI: {ui_url}");
                    println!("  Logs:   {}", log_path.display());
                    return Ok(());
                }
            }
        }
    }

    let exe = std::env::current_exe()?;
    let log_file = File::create(&log_path)?;
    let err_file = log_file.try_clone()?;

    let mut command = Command::new(exe);
    command
        .arg("up")
        .args(["--port", &port.to_string()])
        .args(["--bind", &bind.to_string()])
        .arg("--instance")
        .arg(&instance.root);
    if allow_insecure_remote {
        command.arg("--allow-insecure-remote");
    }
    let child = command
        .stdout(log_file)
        .stderr(err_file)
        .stdin(std::process::Stdio::null())
        .spawn()?;

    let pid = child.id();
    std::fs::write(&pid_path, pid.to_string())?;

    println!("xpressclaw started in background (pid {pid}).");
    println!("  Instance: {}", instance.root.display());
    println!("  Web UI:   {ui_url}");
    println!("  Logs:   {}", log_path.display());
    println!("  PID:    {}", pid_path.display());
    println!();
    if instance.is_default() {
        println!("Stop with `xpressclaw down`.");
    } else {
        println!(
            "Stop with `xpressclaw down --instance \"{}\"`.",
            instance.root.display()
        );
    }

    Ok(())
}

/// Build the AppState (shared between foreground and detached modes).
async fn build_state(instance: &Instance) -> anyhow::Result<AppState> {
    std::fs::create_dir_all(&instance.root)?;
    let config_path = instance.config_path();

    // Check if config exists — if not, start in setup mode
    if !config_path.exists() {
        info!(instance = %instance.root.display(), "no instance config found — starting setup");
        let mut config = Config::default();
        config.system.data_dir = instance.root.clone();
        config.system.workspace_dir = instance.root.join("workspaces");
        let db_path = config.system.data_dir.join("xpressclaw.db");
        std::fs::create_dir_all(&config.system.workspace_dir)?;
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

    // Sync configured runtime contexts and their durable logical sessions. Native
    // workers are launched later for individual queued attempts.
    let registry = AgentRegistry::new(db.clone());
    let sessions = SessionManager::new(db.clone());
    let valid_names: Vec<&str> = config.agents.iter().map(|a| a.name.as_str()).collect();
    for existing in registry.list().unwrap_or_default() {
        if !valid_names.contains(&existing.name.as_str()) {
            let _ = sessions.delete(&existing.id);
        }
    }
    registry.remove_stale(&valid_names).unwrap_or_default();
    for agent_config in &config.agents {
        match registry.ensure(&agent_config.name, &agent_config.backend) {
            Ok(record) => {
                let title = agent_config.context_label();
                sessions.ensure(&record.id, Some(&title))?;
                info!(
                    name = record.name,
                    backend = record.backend,
                    "synced ACP project"
                );
            }
            Err(e) => warn!(name = agent_config.name, error = %e, "failed to sync agent"),
        }
    }

    let setup_complete = !config.agents.is_empty();

    // Build LLM router
    let config = Arc::new(config);
    let llm_router = {
        use xpressclaw_core::llm::router::LlmRouter;
        LlmRouter::build_from_config(&config)
    };

    let state = AppState::new(
        config,
        db,
        Some(Arc::new(llm_router)),
        config_path,
        setup_complete,
    );

    // No worker startup here. The server dispatches queued work into isolated,
    // short-lived ACP server containers (ADR-026).

    Ok(state)
}

fn validate_bind(bind: IpAddr, allow_insecure_remote: bool) -> anyhow::Result<()> {
    if bind.is_loopback() || allow_insecure_remote {
        return Ok(());
    }

    anyhow::bail!(
        "refusing to expose an unauthenticated XpressClaw control plane on {bind}. Keep the default loopback bind and use an SSH tunnel or authenticated TLS proxy. If another security layer already protects this address, rerun with --allow-insecure-remote to acknowledge the risk"
    )
}

fn ui_url(bind: IpAddr, port: u16) -> String {
    if bind.is_loopback() {
        format!("http://localhost:{port}")
    } else {
        format!("http://{}", SocketAddr::new(bind, port))
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn loopback_bind_is_safe_by_default() {
        assert!(validate_bind(IpAddr::V4(Ipv4Addr::LOCALHOST), false).is_ok());
    }

    #[test]
    fn non_loopback_bind_requires_explicit_acknowledgement() {
        let bind = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        let error = validate_bind(bind, false).unwrap_err().to_string();
        assert!(error.contains("unauthenticated"));
        assert!(error.contains("SSH tunnel"));
        assert!(validate_bind(bind, true).is_ok());
    }

    #[test]
    fn formats_ipv6_listen_urls() {
        assert_eq!(ui_url("::".parse().unwrap(), 8935), "http://[::]:8935");
    }

    #[tokio::test]
    async fn initialized_empty_instance_still_opens_first_run_setup() {
        let root = tempfile::tempdir().unwrap();
        let instance = Instance {
            root: root.path().join("instance"),
            source: InstanceSource::Explicit,
        };
        std::fs::create_dir_all(&instance.root).unwrap();
        let mut config = Config::default();
        config.system.data_dir = instance.root.clone();
        config.system.workspace_dir = instance.root.join("workspaces");
        config.save(&instance.config_path()).unwrap();

        let state = build_state(&instance).await.unwrap();

        assert!(!state.is_setup_complete());
    }
}
