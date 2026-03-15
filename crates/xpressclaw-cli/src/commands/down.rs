use super::client;

pub async fn run(port: u16) -> anyhow::Result<()> {
    // First try to stop agents via the API
    match client::connect(port).await {
        Ok(api) => {
            let agents: Vec<serde_json::Value> = api.get("/agents").await?;

            let mut stopped = 0;
            for agent in &agents {
                let id = agent["id"].as_str().unwrap_or_default();
                let status = agent["status"].as_str().unwrap_or_default();

                if status == "running" || status == "starting" {
                    match api.post_empty(&format!("/agents/{id}/stop")).await {
                        Ok(_) => {
                            println!("  stopped {id}");
                            stopped += 1;
                        }
                        Err(e) => eprintln!("  failed to stop {id}: {e}"),
                    }
                }
            }

            if stopped == 0 {
                println!("No running agents to stop.");
            } else {
                println!("Stopped {stopped} agent(s).");
            }
        }
        Err(_) => {
            // Server not reachable via API — try killing via PID file
        }
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
                        println!("Stopped background server (pid {pid}).");
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
