use super::client;

pub async fn run(port: u16) -> anyhow::Result<()> {
    let api = client::connect(port).await?;

    // Health
    let health: serde_json::Value = api.get("/health").await?;
    println!(
        "xpressclaw v{} — {}",
        health["version"].as_str().unwrap_or("?"),
        health["status"].as_str().unwrap_or("?"),
    );
    println!();

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
