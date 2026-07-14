use super::client;

pub async fn run(port: u16) -> anyhow::Result<()> {
    // Sessions have no persistent processes to stop. A graceful server
    // shutdown cancels and removes any active short-lived worker containers.
    if client::connect(port).await.is_ok() {
        println!("Stopping the control plane and active native workers...");
    }

    // Kill background server process if running
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))?;
    let pid_path = std::path::Path::new(&home)
        .join(".xpressclaw")
        .join("server.pid");

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
