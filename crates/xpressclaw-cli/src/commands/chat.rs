use std::io::{self, BufRead, Write};

use super::client;

pub async fn run(agent: &str, port: u16) -> anyhow::Result<()> {
    let api = client::connect(port).await?;

    // Verify agent exists
    let agent_info: serde_json::Value = api
        .get(&format!("/agents/{agent}"))
        .await
        .map_err(|_| anyhow::anyhow!("agent '{agent}' not found"))?;

    // Determine which model to use: check agent config, then /v1/models for default
    let model = agent_info["config"]["model"].as_str().map(String::from);

    let model = match model {
        Some(m) => m,
        None => {
            // Query available models and pick the first one
            let url = format!("http://127.0.0.1:{port}/v1/models");
            let client = reqwest::Client::new();
            let resp: serde_json::Value = client.get(&url).send().await?.json().await?;
            resp["data"][0]["id"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| "local".to_string())
        }
    };

    println!("Chatting with {agent} (model: {model}). Type 'exit' or Ctrl+D to quit.");
    println!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut history: Vec<serde_json::Value> = Vec::new();

    loop {
        print!("you> ");
        stdout.flush()?;

        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            // EOF
            println!();
            break;
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "exit" || line == "quit" {
            break;
        }

        history.push(serde_json::json!({"role": "user", "content": line}));

        let body = serde_json::json!({
            "model": model,
            "messages": history,
            "temperature": 0.7,
            "max_tokens": 4096,
            "stream": false
        });

        // Use /v1/chat/completions endpoint
        let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
        let client = reqwest::Client::new();
        let resp = client.post(&url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            eprintln!("Error {status}: {text}");
            // Remove the failed user message from history
            history.pop();
            continue;
        }

        let data: serde_json::Value = resp.json().await?;
        if let Some(content) = data["choices"][0]["message"]["content"].as_str() {
            println!();
            println!("{content}");
            println!();
            // Add assistant response to history for multi-turn conversation
            history.push(serde_json::json!({"role": "assistant", "content": content}));
        } else {
            eprintln!("No response from agent.");
            history.pop();
        }
    }

    Ok(())
}
