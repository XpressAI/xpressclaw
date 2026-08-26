use std::path::PathBuf;

use super::client;
use super::instance;

pub async fn run(port: Option<u16>, instance_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let instance = instance::resolve(instance_dir)?;
    let saved = xpressclaw_core::config::Config::load(&instance.config_path())
        .map(|config| config.instance)
        .unwrap_or_default();
    let port = port.unwrap_or(saved.port);
    let api = client::connect_to(saved.bind, port).await?;

    // Health
    let health: serde_json::Value = api.get("/health").await?;
    println!(
        "xpressclaw v{} — {}",
        health["version"].as_str().unwrap_or("?"),
        health["status"].as_str().unwrap_or("?"),
    );
    println!();

    // Browser authentication intentionally keeps credentials out of CLI
    // configuration. Health remains public, but instance data requires the
    // operator to sign in through the UI when authentication is enabled.
    let bootstrap: serde_json::Value = api.get("/auth/bootstrap").await?;
    if bootstrap["authentication_enabled"].as_bool() == Some(true) {
        println!(
            "Authentication: {} (sign in through the web UI to inspect Agents)",
            bootstrap["credential_kind"].as_str().unwrap_or("enabled")
        );
        return Ok(());
    }

    // Durable Agents
    let agents: Vec<serde_json::Value> = api.get("/agents").await?;
    if agents.is_empty() {
        println!("No Agents configured.");
    } else {
        println!("Agents:");
        for a in &agents {
            let name = a["name"].as_str().unwrap_or("?");
            let backend = a["backend"].as_str().unwrap_or("?");
            let status = a["status"].as_str().unwrap_or("?");
            let icon = match status {
                "running" => "+",
                "queued" | "waiting_for_input" => "~",
                "error" => "!",
                _ => "-",
            };
            println!("  [{icon}] {name:<20} {backend:<16} {status}");
        }
    }
    println!();

    Ok(())
}
