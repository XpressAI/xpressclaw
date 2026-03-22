use super::client;

pub async fn run(agent: Option<String>, port: u16) -> anyhow::Result<()> {
    let api = client::connect(port).await?;

    let summary: serde_json::Value = if let Some(ref a) = agent {
        api.get(&format!("/budget/{a}")).await?
    } else {
        api.get("/budget").await?
    };

    if let Some(ref a) = agent {
        // Per-agent summary (flat structure from /budget/{agent_id})
        let total = summary["total_spent"].as_f64().unwrap_or(0.0);
        let daily = summary["daily_spent"].as_f64().unwrap_or(0.0);
        let monthly = summary["monthly_spent"].as_f64().unwrap_or(0.0);
        let requests = summary["request_count"].as_i64().unwrap_or(0);

        println!("Budget for {a}:");
        println!("  Total spent:    ${total:.4}");
        println!("  Today:          ${daily:.4}");
        println!("  This month:     ${monthly:.4}");
        println!("  Requests:       {requests}");

        if let Some(daily_limit) = summary["daily_limit"].as_f64() {
            let remaining = (daily_limit - daily).max(0.0);
            println!("  Daily limit:    ${daily_limit:.2}");
            println!("  Remaining:      ${remaining:.4}");
        }
        if let Some(monthly_limit) = summary["monthly_limit"].as_f64() {
            let remaining = (monthly_limit - monthly).max(0.0);
            println!("  Monthly limit:  ${monthly_limit:.2}");
            println!("  Remaining:      ${remaining:.4}");
        }
    } else {
        // Global summary (nested under "global" key)
        let global = &summary["global"];
        let total = global["total_spent"].as_f64().unwrap_or(0.0);
        let daily = global["daily_spent"].as_f64().unwrap_or(0.0);
        let monthly = global["monthly_spent"].as_f64().unwrap_or(0.0);

        println!("Budget summary:");
        println!("  Total spent:    ${total:.4}");
        println!("  Today:          ${daily:.4}");
        println!("  This month:     ${monthly:.4}");

        if let Some(daily_limit) = global["daily_limit"].as_f64() {
            println!("  Daily limit:    ${daily_limit:.2}");
        }
        if let Some(monthly_limit) = global["monthly_limit"].as_f64() {
            println!("  Monthly limit:  ${monthly_limit:.2}");
        }

        // Per-agent breakdown
        if let Some(agents) = summary["agents"].as_array() {
            if !agents.is_empty() {
                println!();
                println!("Per agent:");
                for a in agents {
                    let id = a["agent_id"].as_str().unwrap_or("?");
                    let spent = a["total_spent"].as_f64().unwrap_or(0.0);
                    let today = a["daily_spent"].as_f64().unwrap_or(0.0);
                    let paused = a["is_paused"].as_bool().unwrap_or(false);
                    let status = if paused { " [PAUSED]" } else { "" };
                    println!("  {id:<12} ${spent:.4} total, ${today:.4} today{status}");
                }
            }
        }
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
            let cost = u["cost_usd"].as_f64().unwrap_or(0.0);
            println!("  {agent_id:<10} {model:<18} {input:>8} {output:>8} ${cost:>9.4}");
        }
    }

    Ok(())
}
