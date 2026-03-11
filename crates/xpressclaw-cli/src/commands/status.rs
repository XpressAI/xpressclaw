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

    // Agents
    let agents: Vec<serde_json::Value> = api.get("/agents").await?;
    if agents.is_empty() {
        println!("No agents registered.");
    } else {
        println!("Agents:");
        for a in &agents {
            let name = a["name"].as_str().unwrap_or("?");
            let backend = a["backend"].as_str().unwrap_or("?");
            let status = a["status"].as_str().unwrap_or("?");
            let icon = match status {
                "running" => "+",
                "starting" => "~",
                "error" => "!",
                _ => "-",
            };
            println!("  [{icon}] {name:<20} {backend:<16} {status}");
        }
    }
    println!();

    // Budget
    let budget: serde_json::Value = api.get("/budget").await?;
    let total = budget["total_spent"].as_f64().unwrap_or(0.0);
    let daily_limit = budget["daily_limit"].as_f64();

    print!("Budget: ${total:.4} spent");
    if let Some(limit) = daily_limit {
        print!(" / ${limit:.2} daily limit");
    }
    println!();

    Ok(())
}
