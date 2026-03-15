use super::client;

pub async fn run(agent: Option<String>, port: u16) -> anyhow::Result<()> {
    let api = client::connect(port).await?;

    // Summary
    let summary: serde_json::Value = if let Some(ref a) = agent {
        api.get(&format!("/budget/{a}")).await?
    } else {
        api.get("/budget").await?
    };

    let total = summary["total_spent"].as_f64().unwrap_or(0.0);
    let requests = summary["request_count"].as_i64().unwrap_or(0);

    if let Some(ref a) = agent {
        println!("Budget for {a}:");
    } else {
        println!("Budget summary:");
    }
    println!("  Total spent:  ${total:.4}");
    println!("  Requests:     {requests}");

    if let Some(daily) = summary["daily_limit"].as_f64() {
        let remaining = summary["remaining"].as_f64().unwrap_or(0.0);
        println!("  Daily limit:  ${daily:.2}");
        println!("  Remaining:    ${remaining:.4}");
    }

    let status = summary["status"].as_str().unwrap_or("ok");
    if status != "ok" {
        println!("  Status:       {status}");
    }

    // Recent usage
    println!();
    let mut query = "/budget/usage?limit=10".to_string();
    if let Some(ref a) = agent {
        query.push_str(&format!("&agent_id={a}"));
    }

    let usage: Vec<serde_json::Value> = api.get(&query).await?;
    if usage.is_empty() {
        println!("No usage recorded.");
    } else {
        println!("Recent usage:");
        println!(
            "  {:<10} {:<18} {:>8} {:>8} {:>10}",
            "Agent", "Model", "In", "Out", "Cost"
        );
        for u in &usage {
            let agent_id = u["agent_id"].as_str().unwrap_or("-");
            let model = u["model"].as_str().unwrap_or("?");
            let input = u["input_tokens"].as_i64().unwrap_or(0);
            let output = u["output_tokens"].as_i64().unwrap_or(0);
            let cost = u["cost"].as_f64().unwrap_or(0.0);
            println!("  {agent_id:<10} {model:<18} {input:>8} {output:>8} ${cost:>9.4}");
        }
    }

    Ok(())
}
