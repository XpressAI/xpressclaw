use clap::Subcommand;

use super::client;

#[derive(Subcommand)]
pub enum MemoryCommand {
    /// List memories
    List {
        /// Filter by tag
        #[arg(short, long)]
        tag: Option<String>,
        /// Limit results
        #[arg(short, long, default_value = "20")]
        limit: i64,
    },
    /// Search memories by content
    Search {
        /// Search query
        query: String,
        /// Number of results
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },
    /// Show a single memory
    Show {
        /// Memory ID
        id: String,
    },
    /// Add a memory
    Add {
        /// Memory content
        content: String,
        /// Short summary
        #[arg(short, long)]
        summary: Option<String>,
        /// Tags (comma-separated)
        #[arg(short, long)]
        tags: Option<String>,
    },
    /// Delete a memory
    Delete {
        /// Memory ID
        id: String,
    },
    /// Show memory stats
    Stats,
}

pub async fn run(cmd: MemoryCommand, port: u16) -> anyhow::Result<()> {
    let api = client::connect(port).await?;

    match cmd {
        MemoryCommand::List { tag, limit } => {
            let mut query = format!("/memory?limit={limit}");
            if let Some(ref t) = tag {
                query.push_str(&format!("&tag={t}"));
            }

            let memories: Vec<serde_json::Value> = api.get(&query).await?;
            println!("{} memories:", memories.len());
            println!();
            for m in &memories {
                let id = &m["id"].as_str().unwrap_or("?")[..8];
                let summary = m["summary"]
                    .as_str()
                    .or_else(|| m["content"].as_str().map(|c| &c[..c.len().min(60)]))
                    .unwrap_or("?");
                let tags = m["tags"]
                    .as_array()
                    .map(|t| {
                        t.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                print!("  {id}  {summary}");
                if !tags.is_empty() {
                    print!("  [{tags}]");
                }
                println!();
            }
            if memories.is_empty() {
                println!("  (none)");
            }
        }

        MemoryCommand::Search { query, limit } => {
            let encoded = urlencoding::encode(&query);
            let results: Vec<serde_json::Value> =
                api.get(&format!("/memory/search?q={encoded}&limit={limit}")).await?;

            println!("{} results for \"{query}\":", results.len());
            println!();
            for r in &results {
                let id = &r["id"].as_str().unwrap_or("?")[..8];
                let score = r["score"].as_f64().unwrap_or(0.0);
                let summary = r["summary"]
                    .as_str()
                    .or_else(|| r["content"].as_str().map(|c| &c[..c.len().min(80)]))
                    .unwrap_or("?");
                println!("  {id}  ({score:.3})  {summary}");
            }
            if results.is_empty() {
                println!("  (none)");
            }
        }

        MemoryCommand::Show { id } => {
            let m: serde_json::Value = api.get(&format!("/memory/{id}")).await?;
            println!("Memory: {}", m["id"].as_str().unwrap_or("?"));
            if let Some(summary) = m["summary"].as_str() {
                println!("Summary: {summary}");
            }
            if let Some(tags) = m["tags"].as_array() {
                let tag_str: Vec<&str> = tags.iter().filter_map(|t| t.as_str()).collect();
                if !tag_str.is_empty() {
                    println!("Tags: {}", tag_str.join(", "));
                }
            }
            println!();
            if let Some(content) = m["content"].as_str() {
                println!("{content}");
            }
        }

        MemoryCommand::Add {
            content,
            summary,
            tags,
        } => {
            let tags_vec: Vec<&str> = tags
                .as_deref()
                .map(|t| t.split(',').map(|s| s.trim()).collect())
                .unwrap_or_default();

            let body = serde_json::json!({
                "content": content,
                "summary": summary,
                "tags": tags_vec,
                "source": "cli",
            });
            let m: serde_json::Value = api.post("/memory", &body).await?;
            println!("Created memory {}", m["id"].as_str().unwrap_or("?"));
        }

        MemoryCommand::Delete { id } => {
            api.delete(&format!("/memory/{id}")).await?;
            println!("Deleted memory {}", &id[..8.min(id.len())]);
        }

        MemoryCommand::Stats => {
            let stats: serde_json::Value = api.get("/memory/stats").await?;
            if let Some(zk) = stats.get("zettelkasten") {
                println!(
                    "Zettelkasten: {} memories",
                    zk["total_memories"].as_i64().unwrap_or(0)
                );
                if let Some(by_tag) = zk["by_tag"].as_object() {
                    if !by_tag.is_empty() {
                        print!("  Tags:");
                        for (tag, count) in by_tag {
                            print!(" {tag}({count})");
                        }
                        println!();
                    }
                }
            }
            if let Some(vec) = stats.get("vector") {
                println!(
                    "Vectors: {} embeddings ({}d)",
                    vec["embedding_count"].as_i64().unwrap_or(0),
                    vec["dimension"].as_i64().unwrap_or(0),
                );
            }
        }
    }

    Ok(())
}
