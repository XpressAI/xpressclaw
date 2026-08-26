use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::config::InstanceConfig;
use xpressclaw_core::config::{self, Config};
use xpressclaw_core::db::Database;
use xpressclaw_core::docker::manager::DockerManager;
use xpressclaw_core::sessions::SessionManager;
use xpressclaw_server::server;
use xpressclaw_server::state::AppState;
use zeroize::{Zeroize, Zeroizing};

use super::instance::{self, Instance, InstanceSource};

pub async fn run(
    detach: bool,
    port: Option<u16>,
    instance_dir: Option<PathBuf>,
    workdir: Option<PathBuf>,
    bind: Option<IpAddr>,
    allow_insecure_remote: bool,
    startup_token_stdin: bool,
) -> anyhow::Result<()> {
    if detach && startup_token_stdin {
        anyhow::bail!("--startup-token-stdin is an internal launcher option and cannot be combined with --detach");
    }
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

    let configured = load_instance_config(&selected)?;
    let effective =
        resolve_instance_config(&configured.instance, bind, port, allow_insecure_remote)?;
    let launcher_input = if startup_token_stdin {
        Some(read_detached_launcher_input()?)
    } else {
        None
    };

    if detach {
        return run_detached(&effective, &selected);
    }

    run_foreground(effective, &selected, launcher_input).await
}

struct DetachedLauncherInput {
    startup_token: Zeroizing<String>,
    ready_port: u16,
    ready_nonce: Zeroizing<String>,
}

#[derive(Deserialize)]
struct RawDetachedLauncherInput {
    startup_token: String,
    ready_port: u16,
    ready_nonce: String,
}

#[derive(Serialize)]
struct DetachedLauncherPayload<'a> {
    startup_token: &'a str,
    ready_port: u16,
    ready_nonce: &'a str,
}

#[derive(Serialize)]
struct DetachedReadyAnnouncement<'a> {
    nonce: &'a str,
    startup_token_in_use: bool,
}

#[derive(Deserialize)]
struct RawDetachedReadyAnnouncement {
    nonce: String,
    startup_token_in_use: bool,
}

/// Run the server in the foreground (default).
async fn run_foreground(
    effective: InstanceConfig,
    instance: &Instance,
    launcher_input: Option<DetachedLauncherInput>,
) -> anyhow::Result<()> {
    let (supplied_startup_token, readiness) = match launcher_input {
        Some(input) => (
            Some(input.startup_token),
            Some((input.ready_port, input.ready_nonce)),
        ),
        None => (None, None),
    };
    let state = build_state(instance, effective.clone(), supplied_startup_token).await?;
    let desktop_identity = (std::env::var_os("XPRESSCLAW_DESKTOP_HANDSHAKE").as_deref()
        == Some(std::ffi::OsStr::new("1")))
    .then(|| state.auth.identity_public_key());
    let startup_token_in_use = matches!(
        state.auth.credential_kind(),
        xpressclaw_server::auth::CredentialKind::StartupToken
    );
    let ui_url = ui_url(effective.bind, effective.port);

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

    let startup_token = state.auth.take_startup_token_announcement();
    server::serve_on_with_bound_callback(state, effective.bind, effective.port, move || {
        if let Some((ready_port, ready_nonce)) = readiness {
            announce_detached_ready(ready_port, &ready_nonce, startup_token_in_use)?;
        }
        if let Some(identity) = desktop_identity {
            print_desktop_identity(&identity)?;
        }
        if let Some(token) = startup_token {
            print_startup_token(&token)?;
        }
        Ok(())
    })
    .await?;

    Ok(())
}

/// Spawn the server as a detached background process.
///
/// Re-executes `xpressclaw up` (without --detach) in a new process,
/// redirecting stdout/stderr to a log file.
fn run_detached(effective: &InstanceConfig, instance: &Instance) -> anyhow::Result<()> {
    use std::fs::File;
    use std::io::Write;
    use std::process::Command;

    std::fs::create_dir_all(&instance.root)?;
    let log_path = instance.root.join("server.log");
    let pid_path = instance.root.join("server.pid");
    let ui_url = ui_url(effective.bind, effective.port);

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

    let ready_listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    ready_listener.set_nonblocking(true)?;
    let ready_port = ready_listener.local_addr()?.port();
    let ready_nonce = xpressclaw_server::auth::generate_startup_token()?;

    let mut command = Command::new(exe);
    command
        .arg("up")
        .args(["--port", &effective.port.to_string()])
        .args(["--bind", &effective.bind.to_string()])
        .arg("--instance")
        .arg(&instance.root)
        .arg("--startup-token-stdin")
        .stdin(std::process::Stdio::piped());
    if !effective.bind.is_loopback() && !effective.authentication_enabled {
        command.arg("--allow-insecure-remote");
    }
    // Always supply a candidate token. The child ignores it when auth is off
    // or a password verifier exists, and reports the effective credential mode
    // in its authenticated readiness response. This keeps a concurrent config
    // change from generating a token in the child log or exposing an unused
    // token from the parent.
    let startup_token = xpressclaw_server::auth::generate_startup_token()?;
    let mut child = command.stdout(log_file).stderr(err_file).spawn()?;

    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("detached child did not expose its launcher pipe"))
        .and_then(|mut stdin| {
            serde_json::to_writer(
                &mut stdin,
                &DetachedLauncherPayload {
                    startup_token: startup_token.as_str(),
                    ready_port,
                    ready_nonce: &ready_nonce,
                },
            )?;
            writeln!(stdin)?;
            Ok(())
        });
    if let Err(error) = write_result {
        terminate_child(&mut child);
        return Err(error);
    }

    let pid = child.id();
    let startup_token_in_use =
        match wait_for_detached_ready(&mut child, &ready_listener, &ready_nonce, &log_path) {
            Ok(startup_token_in_use) => startup_token_in_use,
            Err(error) => {
                terminate_child(&mut child);
                let _ = std::fs::remove_file(&pid_path);
                return Err(error);
            }
        };
    if let Err(error) = std::fs::write(&pid_path, pid.to_string()) {
        terminate_child(&mut child);
        return Err(error.into());
    }

    if startup_token_in_use {
        let token = startup_token;
        if let Err(error) = print_startup_token(&token) {
            terminate_child(&mut child);
            let _ = std::fs::remove_file(&pid_path);
            return Err(error);
        }
    }
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

fn wait_for_detached_ready(
    child: &mut std::process::Child,
    listener: &std::net::TcpListener,
    expected_nonce: &str,
    log_path: &std::path::Path,
) -> anyhow::Result<bool> {
    use std::io::Read;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                "detached XpressClaw exited with {status} before owning its listeners; see {}",
                log_path.display()
            );
        }

        match listener.accept() {
            Ok((stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                stream.set_read_timeout(Some(Duration::from_millis(500)))?;
                let mut announcement = String::new();
                if stream.take(1025).read_to_string(&mut announcement).is_ok() {
                    let parsed =
                        serde_json::from_str::<RawDetachedReadyAnnouncement>(announcement.trim());
                    announcement.zeroize();
                    if let Ok(mut parsed) = parsed {
                        let nonce_matches = parsed.nonce == expected_nonce;
                        parsed.nonce.zeroize();
                        if nonce_matches {
                            return Ok(parsed.startup_token_in_use);
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "detached XpressClaw did not own its listeners within 10 seconds; see {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn terminate_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn announce_detached_ready(
    port: u16,
    nonce: &str,
    startup_token_in_use: bool,
) -> anyhow::Result<()> {
    use std::io::Write;

    let address = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);
    let mut stream = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    serde_json::to_writer(
        &mut stream,
        &DetachedReadyAnnouncement {
            nonce,
            startup_token_in_use,
        },
    )?;
    writeln!(stream)?;
    stream.flush()?;
    Ok(())
}

/// Build the AppState (shared between foreground and detached modes).
async fn build_state(
    instance: &Instance,
    effective: InstanceConfig,
    supplied_startup_token: Option<Zeroizing<String>>,
) -> anyhow::Result<AppState> {
    std::fs::create_dir_all(&instance.root)?;
    let config_path = instance.config_path();

    // Check if config exists — if not, start in setup mode
    if !config_path.exists() {
        instance::mark_materialized(instance)?;
        info!(instance = %instance.root.display(), "no instance config found — starting setup");
        let mut config = Config::default();
        config.system.data_dir = instance.root.clone();
        config.system.workspace_dir = instance.root.join("workspaces");
        let db_path = config.system.data_dir.join("xpressclaw.db");
        std::fs::create_dir_all(&config.system.workspace_dir)?;
        let db = Arc::new(Database::open(&db_path)?);

        return AppState::new_with_instance(
            Arc::new(config),
            db,
            None,
            config_path,
            false,
            effective,
            supplied_startup_token,
        );
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

    let state = AppState::new_with_instance(
        config,
        db,
        Some(Arc::new(llm_router)),
        config_path,
        setup_complete,
        effective,
        supplied_startup_token,
    )?;

    // No worker startup here. The server dispatches queued work into isolated,
    // short-lived ACP server containers (ADR-026).

    Ok(state)
}

fn load_instance_config(instance: &Instance) -> anyhow::Result<Config> {
    if instance.config_path().exists() {
        return Ok(Config::load(&instance.config_path())?);
    }
    let mut config = Config::default();
    config.system.data_dir = instance.root.clone();
    config.system.workspace_dir = instance.root.join("workspaces");
    Ok(config)
}

fn resolve_instance_config(
    saved: &InstanceConfig,
    cli_bind: Option<IpAddr>,
    cli_port: Option<u16>,
    allow_insecure_remote: bool,
) -> anyhow::Result<InstanceConfig> {
    let mut effective = saved.clone();
    if let Some(bind) = cli_bind {
        effective.bind = bind;
    }
    if let Some(port) = cli_port {
        if port == 0 {
            anyhow::bail!("--port must be between 1 and 65535");
        }
        effective.port = port;
    }

    if effective.bind.is_loopback() || effective.authentication_enabled {
        effective.allow_unauthenticated_remote = false;
        return Ok(effective);
    }

    let saved_acknowledgement_applies =
        cli_bind.is_none() && saved.bind == effective.bind && saved.allow_unauthenticated_remote;
    if allow_insecure_remote || saved_acknowledgement_applies {
        effective.allow_unauthenticated_remote = true;
        return Ok(effective);
    }

    anyhow::bail!(
        "refusing to expose an unauthenticated XpressClaw control plane on {}. Direct access is supported on an operator-trusted LAN or tailnet, but it is not encrypted. Rerun with --allow-insecure-remote, or save and confirm this address in Settings → Instance",
        effective.bind
    )
}

fn read_detached_launcher_input() -> anyhow::Result<DetachedLauncherInput> {
    use std::io::Read;
    let mut payload = String::new();
    std::io::stdin().take(8193).read_to_string(&mut payload)?;
    let parsed = serde_json::from_str::<RawDetachedLauncherInput>(&payload);
    payload.zeroize();
    let mut parsed = parsed
        .map_err(|_| anyhow::anyhow!("the detached launcher supplied invalid startup data"))?;
    if parsed.ready_port == 0 || !(20..=256).contains(&parsed.ready_nonce.len()) {
        parsed.ready_nonce.zeroize();
        parsed.startup_token.zeroize();
        anyhow::bail!("the detached launcher supplied invalid readiness data");
    }
    if !(20..=256).contains(&parsed.startup_token.len()) {
        parsed.ready_nonce.zeroize();
        parsed.startup_token.zeroize();
        anyhow::bail!("the detached launcher supplied an invalid startup token");
    }
    Ok(DetachedLauncherInput {
        startup_token: Zeroizing::new(std::mem::take(&mut parsed.startup_token)),
        ready_port: parsed.ready_port,
        ready_nonce: Zeroizing::new(std::mem::take(&mut parsed.ready_nonce)),
    })
}

fn print_desktop_identity(identity: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(
        output,
        "{}{identity}",
        xpressclaw_server::auth::INSTANCE_IDENTITY_PREFIX
    )?;
    output.flush()?;
    Ok(())
}

fn print_startup_token(token: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(output)?;
    writeln!(output, "Authentication is enabled without a password.")?;
    writeln!(output, "Use this login token until XpressClaw restarts:")?;
    writeln!(
        output,
        "{}{token}",
        xpressclaw_server::auth::STARTUP_TOKEN_PREFIX
    )?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
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
        let saved = InstanceConfig::default();
        let effective = resolve_instance_config(&saved, None, None, false).unwrap();
        assert_eq!(effective.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(effective.port, 8935);
    }

    #[test]
    fn non_loopback_bind_requires_explicit_acknowledgement() {
        let bind = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        let saved = InstanceConfig::default();
        let error = resolve_instance_config(&saved, Some(bind), None, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unauthenticated"));
        assert!(error.contains("tailnet"));
        assert!(resolve_instance_config(&saved, Some(bind), None, true).is_ok());
    }

    #[test]
    fn saved_remote_acknowledgement_is_reusable_but_does_not_cover_cli_override() {
        let saved = InstanceConfig {
            bind: "0.0.0.0".parse().unwrap(),
            allow_unauthenticated_remote: true,
            ..InstanceConfig::default()
        };
        assert!(resolve_instance_config(&saved, None, None, false).is_ok());
        assert!(
            resolve_instance_config(&saved, Some("192.0.2.10".parse().unwrap()), None, false,)
                .is_err()
        );
    }

    #[test]
    fn explicit_cli_values_override_saved_instance_values() {
        let saved = InstanceConfig {
            port: 9000,
            authentication_enabled: true,
            ..InstanceConfig::default()
        };
        let effective =
            resolve_instance_config(&saved, Some("::".parse().unwrap()), Some(9443), false)
                .unwrap();
        assert_eq!(effective.bind, "::".parse::<IpAddr>().unwrap());
        assert_eq!(effective.port, 9443);
        assert!(effective.authentication_enabled);
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

        let state = build_state(&instance, config.instance.clone(), None)
            .await
            .unwrap();

        assert!(!state.is_setup_complete());
    }
}
