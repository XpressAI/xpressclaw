use std::path::PathBuf;

use super::client;
use super::instance;

pub async fn run(
    port: Option<u16>,
    instance_dir: Option<PathBuf>,
    workdir: Option<PathBuf>,
) -> anyhow::Result<()> {
    if workdir.is_some() {
        eprintln!("warning: --workdir is deprecated; use --instance instead");
    }
    let instance = instance::resolve(instance_dir.or(workdir))?;
    let saved = xpressclaw_core::config::Config::load(&instance.config_path())
        .map(|config| config.instance)
        .unwrap_or_default();
    let port = port.unwrap_or(saved.port);

    // Sessions have no persistent processes to stop. A graceful server
    // shutdown cancels and removes any active short-lived worker containers.
    if client::connect_to(saved.bind, port).await.is_ok() {
        println!("Stopping the control plane and active ACP workers...");
    }

    // Kill background server process if running
    let pid_path = instance.root.join("server.pid");

    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                // Send SIGTERM
                let result = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .output();

                match result {
                    Ok(output) if output.status.success() => {
                        println!("Stopped background control plane (pid {pid}).");
                    }
                    _ => {
                        // Process already dead
                    }
                }
            }
        }
        let _ = std::fs::remove_file(&pid_path);
    }

    Ok(())
}
