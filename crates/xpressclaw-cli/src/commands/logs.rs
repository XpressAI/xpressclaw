use super::client;

pub async fn run(agent: Option<String>, limit: usize, port: u16) -> anyhow::Result<()> {
    let api = client::connect(port).await?;

    let mut query = format!("/activity?limit={limit}");
    if let Some(ref a) = agent {
        query.push_str(&format!("&agent_id={a}"));
    }

    let events: Vec<serde_json::Value> = api.get(&query).await?;

    if events.is_empty() {
        println!("No activity recorded.");
        return Ok(());
    }

    for e in &events {
        let ts = e["created_at"].as_str().unwrap_or("?");
        let kind = e["event_type"].as_str().unwrap_or("?");
        let agent_id = e["agent_id"].as_str().unwrap_or("-");
        let message = e["message"].as_str().unwrap_or("");
        println!("{ts}  {agent_id:<16} {kind:<20} {message}");
    }

    Ok(())
}
