//! Memory hooks — automatic recall and remember.
//!
//! The memory system uses a "hippocampus" model:
//!
//! - **Recall** (before first response): searches memory in Rust,
//!   sends results to LLM for synthesis into a brief narrative.
//!
//! - **Remember** (async, after each response): sends the exchange
//!   to LLM for analysis, parses the response, saves notes in Rust.
//!
//! - **Consolidate** (pre-compaction): processes inbox notes into
//!   permanent zettel notes.
//!
//! Memory search and save happen in Rust — the LLM is only used for
//! natural language synthesis and analysis. No MCP tools are exposed
//! to the sub-agent calls.

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::agents::harness::HarnessClient;
use crate::config::HooksConfig;
use crate::db::Database;
use crate::memory::manager::MemoryManager;
use crate::memory::zettelkasten::CreateMemory;

/// Orchestrates memory hooks for an agent.
pub struct MemoryHooks {
    db: Arc<Database>,
    eviction_strategy: String,
}

impl MemoryHooks {
    pub fn new(db: Arc<Database>, eviction_strategy: &str) -> Self {
        Self {
            db,
            eviction_strategy: eviction_strategy.to_string(),
        }
    }

    /// Check if an agent has any memories at all (quick DB count).
    pub fn has_memories(&self, agent_id: &str) -> bool {
        let mgr = MemoryManager::new(self.db.clone(), &self.eviction_strategy);
        mgr.get_recent(None, Some(agent_id), 1)
            .map(|r| !r.is_empty())
            .unwrap_or(false)
            || mgr
                .get_recent(Some("shared"), None, 1)
                .map(|r| !r.is_empty())
                .unwrap_or(false)
    }

    /// Recall: search memory and synthesize a recollection.
    ///
    /// 1. Searches memory in Rust for relevant entries
    /// 2. Sends the results to the LLM for natural language synthesis
    /// 3. Returns the recollection text to inject into the conversation
    pub async fn recall(
        &self,
        agent_id: &str,
        user_query: &str,
        harness_port: u16,
    ) -> Option<String> {
        if !self.has_memories(agent_id) {
            debug!(agent_id, "no memories found, skipping recall");
            return None;
        }

        info!(agent_id, "recalling memories for first turn");

        // Step 1: Search memory in Rust
        let mgr = MemoryManager::new(self.db.clone(), &self.eviction_strategy);
        let query = truncate(user_query, 300);

        let results = mgr.search_text(query, 8).unwrap_or_default();
        if results.is_empty() {
            debug!(agent_id, "memory search returned no results");
            return None;
        }

        // Format search results as context
        let mut memory_context = String::new();
        for (i, result) in results.iter().enumerate() {
            let summary = if result.memory.summary.is_empty() {
                &result.memory.content
            } else {
                &result.memory.summary
            };
            memory_context.push_str(&format!("{}. {}\n", i + 1, truncate(summary, 200)));
        }

        // Step 2: Send to LLM for synthesis (no tools — just text in, text out)
        let harness = HarnessClient::new(harness_port);
        let prompt = format!(
            "You are synthesizing memories for an AI agent named '{agent_id}'.\n\n\
             The user's query is:\n\
             \"{query}\"\n\n\
             Here are relevant memories found in the agent's memory store:\n\
             {memories}\n\
             Write a concise recollection (2-3 sentences) that will help the \
             agent respond to the user's query. Write in first person as the \
             agent. Focus only on facts relevant to the query.\n\n\
             If none of the memories are relevant, respond with exactly: \
             No relevant memories found.",
            agent_id = agent_id,
            query = truncate(user_query, 400),
            memories = memory_context,
        );

        match harness.send_task("", &prompt, agent_id).await {
            Ok(response) => {
                let text = response
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default();

                if text.is_empty() || text.contains("No relevant memories found") {
                    debug!(agent_id, "recall returned no relevant memories");
                    return None;
                }

                info!(
                    agent_id,
                    recollection_len = text.len(),
                    "memory recall complete"
                );
                Some(text)
            }
            Err(e) => {
                warn!(agent_id, error = %e, "memory recall failed");
                None
            }
        }
    }

    /// Remember: review a conversation turn and save noteworthy information.
    ///
    /// 1. Sends the exchange to the LLM for analysis
    /// 2. Parses the response for memory-worthy items
    /// 3. Saves them as fleeting notes in Rust
    pub async fn remember(
        &self,
        agent_id: &str,
        user_message: &str,
        agent_response: &str,
        harness_port: u16,
    ) {
        if user_message.len() + agent_response.len() < 200 {
            debug!(agent_id, "exchange too short, skipping remember");
            return;
        }

        info!(agent_id, "running async memory remember");

        let harness = HarnessClient::new(harness_port);
        let prompt = format!(
            "You are the memory subsystem for an AI agent named '{agent_id}'.\n\n\
             Review this conversation exchange and identify facts worth remembering.\n\n\
             USER SAID:\n\"{user_msg}\"\n\n\
             AGENT RESPONDED:\n\"{agent_resp}\"\n\n\
             List 0-2 facts worth saving for future conversations. Each fact \
             should be a single clear sentence. Format each on its own line \
             prefixed with \"SAVE: \". Include context about why it matters.\n\n\
             If nothing is worth remembering, respond with: Nothing to save.\n\n\
             Examples:\n\
             SAVE: User prefers Python over JavaScript for backend work.\n\
             SAVE: The project deadline is March 15th 2026.",
            agent_id = agent_id,
            user_msg = truncate(user_message, 500),
            agent_resp = truncate(agent_response, 800),
        );

        match harness.send_task("", &prompt, agent_id).await {
            Ok(response) => {
                let text = response
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default();

                // Parse SAVE: lines and store them
                let mgr = MemoryManager::new(self.db.clone(), &self.eviction_strategy);
                let mut saved = 0;
                for line in text.lines() {
                    let trimmed = line.trim();
                    if let Some(content) = trimmed
                        .strip_prefix("SAVE:")
                        .or_else(|| trimmed.strip_prefix("SAVE :"))
                    {
                        let content = content.trim();
                        if content.is_empty() {
                            continue;
                        }
                        match mgr.add(&CreateMemory {
                            content: content.to_string(),
                            summary: content.to_string(),
                            source: "memory_hook".to_string(),
                            layer: "agent".to_string(),
                            agent_id: Some(agent_id.to_string()),
                            user_id: None,
                            tags: vec!["inbox".to_string()],
                        }) {
                            Ok(_) => saved += 1,
                            Err(e) => warn!(agent_id, error = %e, "failed to save memory"),
                        }
                    }
                }

                if saved > 0 {
                    info!(agent_id, count = saved, "saved memories from exchange");
                } else {
                    debug!(agent_id, "nothing worth remembering");
                }
            }
            Err(e) => {
                warn!(agent_id, error = %e, "async remember failed");
            }
        }
    }

    /// Consolidate: process Inbox/ fleeting notes into permanent Zettel/ notes.
    pub async fn consolidate(&self, agent_id: &str, harness_port: u16) {
        info!(agent_id, "running memory consolidation");

        let mgr = MemoryManager::new(self.db.clone(), &self.eviction_strategy);

        // Get all inbox-tagged memories
        let inbox = mgr.search_by_tag("inbox", 50).unwrap_or_default();
        if inbox.is_empty() {
            debug!(agent_id, "no inbox notes to consolidate");
            return;
        }

        let mut inbox_text = String::new();
        for (i, mem) in inbox.iter().enumerate() {
            inbox_text.push_str(&format!(
                "{}. {}\n",
                i + 1,
                if mem.memory.summary.is_empty() {
                    &mem.memory.content
                } else {
                    &mem.memory.summary
                }
            ));
        }

        let harness = HarnessClient::new(harness_port);
        let prompt = format!(
            "You are consolidating memory for agent '{agent_id}'.\n\n\
             Here are {count} inbox notes (fleeting observations):\n\
             {notes}\n\
             Review them and produce a consolidated summary. Group related \
             facts together. Remove duplicates. For each consolidated fact, \
             write it on its own line prefixed with \"KEEP: \".\n\n\
             For notes that are no longer relevant or are duplicates, \
             write: DELETE: <note number>\n\n\
             Example:\n\
             KEEP: User works on the xpressclaw project, an AI agent runtime.\n\
             KEEP: The project uses Rust backend and SvelteKit frontend.\n\
             DELETE: 3\n\
             DELETE: 5",
            agent_id = agent_id,
            count = inbox.len(),
            notes = inbox_text,
        );

        match harness.send_task("", &prompt, agent_id).await {
            Ok(response) => {
                let text = response
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default();

                let mut kept = 0;
                let mut deleted = 0;

                for line in text.lines() {
                    let trimmed = line.trim();
                    if let Some(content) = trimmed.strip_prefix("KEEP:") {
                        let content = content.trim();
                        if content.is_empty() {
                            continue;
                        }
                        let _ = mgr.add(&CreateMemory {
                            content: content.to_string(),
                            summary: content.to_string(),
                            source: "consolidation".to_string(),
                            layer: "agent".to_string(),
                            agent_id: Some(agent_id.to_string()),
                            user_id: None,
                            tags: vec!["zettel".to_string()],
                        });
                        kept += 1;
                    }
                    if let Some(num_str) = trimmed.strip_prefix("DELETE:") {
                        if let Ok(idx) = num_str.trim().parse::<usize>() {
                            if idx > 0 && idx <= inbox.len() {
                                let _ = mgr.delete(&inbox[idx - 1].memory.id);
                                deleted += 1;
                            }
                        }
                    }
                }

                info!(
                    agent_id,
                    kept,
                    deleted,
                    total_inbox = inbox.len(),
                    "consolidation complete"
                );
            }
            Err(e) => {
                warn!(agent_id, error = %e, "consolidation failed");
            }
        }
    }
}

/// Check if the recall hook is configured.
pub fn has_recall_hook(hooks: &HooksConfig) -> bool {
    hooks
        .before_message
        .iter()
        .any(|h| h == "memory_recall" || h == "memory:recall")
}

/// Check if the remember hook is configured.
pub fn has_remember_hook(hooks: &HooksConfig) -> bool {
    hooks
        .after_message
        .iter()
        .any(|h| h == "memory_remember" || h == "memory:remember")
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s
            .char_indices()
            .take(max)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0)]
    }
}
