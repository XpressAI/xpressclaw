use clap::Subcommand;

use super::client;

#[derive(Subcommand)]
pub enum SopCommand {
    /// List procedures
    List,
    /// Show a procedure
    Show {
        /// Procedure name
        name: String,
    },
    /// Create a procedure from a YAML file
    Create {
        /// Procedure name
        name: String,
        /// Path to YAML file with procedure content
        #[arg(short, long)]
        file: Option<String>,
        /// Description
        #[arg(short, long)]
        description: Option<String>,
    },
    /// Delete a procedure
    Delete {
        /// Procedure name
        name: String,
    },
    /// Run a procedure (creates a task)
    Run {
        /// Procedure name
        name: String,
        /// Agent to run the procedure
        #[arg(short, long)]
        agent: String,
    },
}

pub async fn run(cmd: SopCommand, port: u16) -> anyhow::Result<()> {
    let api = client::connect(port).await?;

    match cmd {
        SopCommand::List => {
            let sops: Vec<serde_json::Value> = api.get("/procedures").await?;
            println!("{} procedures:", sops.len());
            println!();
            for s in &sops {
                let name = s["name"].as_str().unwrap_or("?");
                let desc = s["description"].as_str().unwrap_or("");
                let version = s["version"].as_i64().unwrap_or(1);
                let steps = s["parsed"]["steps"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0);
                println!("  {name:<24} v{version}  {steps} steps  {desc}");
            }
            if sops.is_empty() {
                println!("  (none)");
            }
        }

        SopCommand::Show { name } => {
            let sop: serde_json::Value = api.get(&format!("/procedures/{name}")).await?;
            println!("Procedure: {name}");
            if let Some(desc) = sop["description"].as_str() {
                println!("Description: {desc}");
            }
            println!("Version: {}", sop["version"].as_i64().unwrap_or(1));

            if let Some(parsed) = sop.get("parsed") {
                if let Some(summary) = parsed["summary"].as_str() {
                    println!("Summary: {summary}");
                }

                if let Some(inputs) = parsed["inputs"].as_array() {
                    if !inputs.is_empty() {
                        println!();
                        println!("Inputs:");
                        for i in inputs {
                            let n = i["name"].as_str().unwrap_or("?");
                            let d = i["description"].as_str().unwrap_or("");
                            let req = if i["required"].as_bool().unwrap_or(false) {
                                " (required)"
                            } else {
                                ""
                            };
                            println!("  - {n}{req}: {d}");
                        }
                    }
                }

                if let Some(steps) = parsed["steps"].as_array() {
                    if !steps.is_empty() {
                        println!();
                        println!("Steps:");
                        for (i, step) in steps.iter().enumerate() {
                            let n = step["name"].as_str().unwrap_or("?");
                            let d = step["description"].as_str().unwrap_or("");
                            println!("  {}. {n}: {d}", i + 1);
                        }
                    }
                }

                if let Some(outputs) = parsed["outputs"].as_array() {
                    if !outputs.is_empty() {
                        println!();
                        println!("Outputs:");
                        for o in outputs {
                            let n = o["name"].as_str().unwrap_or("?");
                            let d = o["description"].as_str().unwrap_or("");
                            println!("  - {n}: {d}");
                        }
                    }
                }
            }
        }

        SopCommand::Create {
            name,
            file,
            description,
        } => {
            let content = if let Some(ref path) = file {
                std::fs::read_to_string(path)?
            } else {
                // Default template
                format!(
                    "summary: {}\ntools: []\ninputs: []\noutputs: []\nsteps:\n  - name: Step 1\n    description: First step\n",
                    description.as_deref().unwrap_or(&name)
                )
            };

            let body = serde_json::json!({
                "name": name,
                "description": description,
                "content": content,
            });
            let _: serde_json::Value = api.post("/procedures", &body).await?;
            println!("Created procedure: {name}");
        }

        SopCommand::Delete { name } => {
            api.delete(&format!("/procedures/{name}")).await?;
            println!("Deleted procedure: {name}");
        }

        SopCommand::Run { name, agent } => {
            let body = serde_json::json!({
                "agent_id": agent,
            });
            let task: serde_json::Value =
                api.post(&format!("/procedures/{name}/run"), &body).await?;
            let task_id = task["id"].as_str().unwrap_or("?");
            println!("Running procedure {name} → task {}", &task_id[..8.min(task_id.len())]);
        }
    }

    Ok(())
}
