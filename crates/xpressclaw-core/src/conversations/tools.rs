//! Server-side tool execution for the agentic loop.
//!
//! Executes tool calls from the LLM directly on the server,
//! without requiring Docker containers or external harnesses.

use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::db::Database;
use crate::memory::manager::MemoryManager;
use crate::memory::zettelkasten::CreateMemory;
use crate::tasks::board::{CreateTask, TaskBoard};

/// Tool definitions sent to the LLM so it knows what's available.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "search_memory",
                "description": "Search the agent's memory for relevant information.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" }
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "save_memory",
                "description": "Save a piece of information to memory for future recall.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string", "description": "The information to remember" },
                        "tags": { "type": "string", "description": "Comma-separated tags for categorization" }
                    },
                    "required": ["content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "create_task",
                "description": "Create a new task on the task board.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "Task title" },
                        "description": { "type": "string", "description": "Task description" }
                    },
                    "required": ["title"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_tasks",
                "description": "List tasks on the task board, optionally filtered by status.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "string", "description": "Filter by status: pending, in_progress, completed, failed" }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "update_task",
                "description": "Update a task's status.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string", "description": "The task ID" },
                        "status": { "type": "string", "description": "New status: pending, in_progress, completed, failed" }
                    },
                    "required": ["task_id", "status"]
                }
            }
        }),
    ]
}

/// Execute a tool call and return the result as a string.
pub async fn execute(
    name: &str,
    arguments: &str,
    agent_id: &str,
    conv_id: &str,
    db: &Arc<Database>,
) -> (String, bool) {
    let args: Value = serde_json::from_str(arguments).unwrap_or(json!({}));

    match name {
        "search_memory" => {
            let query = args["query"].as_str().unwrap_or("");
            execute_search_memory(query, agent_id, db).await
        }
        "save_memory" => {
            let content = args["content"].as_str().unwrap_or("");
            let tags = args["tags"].as_str().unwrap_or("");
            execute_save_memory(content, tags, agent_id, db).await
        }
        "create_task" => {
            let title = args["title"].as_str().unwrap_or("Untitled");
            let desc = args["description"].as_str();
            execute_create_task(title, desc, agent_id, conv_id, db)
        }
        "list_tasks" => {
            let status = args["status"].as_str();
            execute_list_tasks(status, agent_id, db)
        }
        "update_task" => {
            let task_id = args["task_id"].as_str().unwrap_or("");
            let status = args["status"].as_str().unwrap_or("");
            execute_update_task(task_id, status, db)
        }
        _ => {
            warn!(name, "unknown tool call");
            (format!("Unknown tool: {name}"), true)
        }
    }
}

async fn execute_search_memory(
    query: &str,
    _agent_id: &str,
    db: &Arc<Database>,
) -> (String, bool) {
    let mem_mgr = MemoryManager::new(db.clone(), "least-recently-relevant");
    match mem_mgr.search(query, 5) {
        Ok(results) => {
            if results.is_empty() {
                ("No memories found.".to_string(), false)
            } else {
                let formatted: Vec<String> = results
                    .iter()
                    .map(|m| format!("- {}", m.memory.content))
                    .collect();
                (formatted.join("\n"), false)
            }
        }
        Err(e) => {
            debug!(error = %e, "memory search failed");
            ("Memory search unavailable.".to_string(), false)
        }
    }
}

async fn execute_save_memory(
    content: &str,
    tags: &str,
    agent_id: &str,
    db: &Arc<Database>,
) -> (String, bool) {
    let mem_mgr = MemoryManager::new(db.clone(), "least-recently-relevant");
    let tag_list: Vec<String> = if tags.is_empty() {
        vec![]
    } else {
        tags.split(',').map(|t| t.trim().to_string()).collect()
    };
    match mem_mgr.add(&CreateMemory {
        content: content.to_string(),
        summary: content.chars().take(100).collect(),
        source: format!("agent:{agent_id}"),
        layer: "shared".to_string(),
        agent_id: Some(agent_id.to_string()),
        user_id: None,
        tags: tag_list,
    }) {
        Ok(mem) => (format!("Saved to memory (id: {})", mem.id), false),
        Err(e) => (format!("Failed to save memory: {e}"), true),
    }
}

fn execute_create_task(
    title: &str,
    description: Option<&str>,
    agent_id: &str,
    conv_id: &str,
    db: &Arc<Database>,
) -> (String, bool) {
    let board = TaskBoard::new(db.clone());
    match board.create(&CreateTask {
        title: title.to_string(),
        description: description.map(String::from),
        agent_id: Some(agent_id.to_string()),
        parent_task_id: None,
        sop_id: None,
        conversation_id: Some(conv_id.to_string()),
        priority: None,
        context: None,
    }) {
        Ok(task) => (format!("Created task '{}' (id: {})", task.title, task.id), false),
        Err(e) => (format!("Failed to create task: {e}"), true),
    }
}

fn execute_list_tasks(
    status: Option<&str>,
    agent_id: &str,
    db: &Arc<Database>,
) -> (String, bool) {
    let board = TaskBoard::new(db.clone());
    match board.list(status, Some(agent_id), 50) {
        Ok(filtered) => {
            if filtered.is_empty() {
                ("No tasks found.".to_string(), false)
            } else {
                let lines: Vec<String> = filtered
                    .iter()
                    .map(|t| {
                        format!(
                            "- [{}] {} ({})",
                            t.status.as_str(), t.title, t.id
                        )
                    })
                    .collect();
                (lines.join("\n"), false)
            }
        }
        Err(e) => (format!("Failed to list tasks: {e}"), true),
    }
}

fn execute_update_task(task_id: &str, status: &str, db: &Arc<Database>) -> (String, bool) {
    let board = TaskBoard::new(db.clone());
    match board.update_status(task_id, status, None) {
        Ok(_) => (format!("Updated task {task_id} to '{status}'"), false),
        Err(e) => (format!("Failed to update task: {e}"), true),
    }
}
