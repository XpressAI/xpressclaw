use clap::Subcommand;

use super::client;

#[derive(Subcommand)]
pub enum TasksCommand {
    /// List tasks
    List {
        /// Filter by status (pending, in_progress, completed, failed)
        #[arg(short, long)]
        status: Option<String>,
        /// Filter by agent
        #[arg(short, long)]
        agent: Option<String>,
    },
    /// Create a new task
    Create {
        /// Task title
        title: String,
        /// Task description
        #[arg(short, long)]
        description: Option<String>,
        /// Assign to agent
        #[arg(short, long)]
        agent: Option<String>,
    },
    /// Show task details
    Show {
        /// Task ID
        id: String,
    },
    /// Update task status
    Update {
        /// Task ID
        id: String,
        /// New status
        #[arg(short, long)]
        status: String,
    },
    /// Delete a task
    Delete {
        /// Task ID
        id: String,
    },
}

pub async fn run(cmd: TasksCommand, port: u16) -> anyhow::Result<()> {
    let api = client::connect(port).await?;

    match cmd {
        TasksCommand::List { status, agent } => {
            let mut query = String::from("/tasks?limit=50");
            if let Some(ref s) = status {
                query.push_str(&format!("&status={s}"));
            }
            if let Some(ref a) = agent {
                query.push_str(&format!("&agent_id={a}"));
            }

            let data: serde_json::Value = api.get(&query).await?;
            let tasks = data["tasks"].as_array();
            let counts = &data["counts"];

            println!(
                "Tasks: {} pending, {} in progress, {} completed, {} failed",
                counts["pending"].as_i64().unwrap_or(0),
                counts["in_progress"].as_i64().unwrap_or(0),
                counts["completed"].as_i64().unwrap_or(0),
                counts["failed"].as_i64().unwrap_or(0),
            );
            println!();

            if let Some(tasks) = tasks {
                for t in tasks {
                    let id = &t["id"].as_str().unwrap_or("?")[..8];
                    let title = t["title"].as_str().unwrap_or("?");
                    let status = t["status"].as_str().unwrap_or("?");
                    let agent = t["agent_id"].as_str().unwrap_or("-");
                    println!("  {id}  {status:<12} {agent:<16} {title}");
                }
                if tasks.is_empty() {
                    println!("  (none)");
                }
            }
        }

        TasksCommand::Create {
            title,
            description,
            agent,
        } => {
            let body = serde_json::json!({
                "title": title,
                "description": description,
                "agent_id": agent,
            });
            let task: serde_json::Value = api.post("/tasks", &body).await?;
            let id = task["id"].as_str().unwrap_or("?");
            println!("Created task {id}: {title}");
        }

        TasksCommand::Show { id } => {
            let task: serde_json::Value = api.get(&format!("/tasks/{id}")).await?;
            println!("Task: {}", task["title"].as_str().unwrap_or("?"));
            println!("ID:     {}", task["id"].as_str().unwrap_or("?"));
            println!("Status: {}", task["status"].as_str().unwrap_or("?"));
            println!("Agent:  {}", task["agent_id"].as_str().unwrap_or("-"));
            if let Some(desc) = task["description"].as_str() {
                println!();
                println!("{desc}");
            }
        }

        TasksCommand::Update { id, status } => {
            let body = serde_json::json!({ "status": status });
            let _: serde_json::Value = api.patch(&format!("/tasks/{id}/status"), &body).await?;
            println!("Updated task {}: status → {status}", &id[..8.min(id.len())]);
        }

        TasksCommand::Delete { id } => {
            api.delete(&format!("/tasks/{id}")).await?;
            println!("Deleted task {}", &id[..8.min(id.len())]);
        }
    }

    Ok(())
}
