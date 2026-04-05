//! Memory hooks — automatic recall and remember via sub-agent calls.
//!
//! The memory system uses a "hippocampus" model: a sub-agent silently
//! manages memory before and after the main agent responds.
//!
//! - **Recall** (before first response): searches memory, synthesizes
//!   a narrative recollection, injected as a message (not system prompt
//!   — preserves prompt caching).
//!
//! - **Remember** (async, after each response): reviews the conversation
//!   turn and creates fleeting notes in Inbox/.
//!
//! - **Consolidate** (pre-compaction / end-of-session): processes Inbox/
//!   into permanent Zettel/ notes, adds links.

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::agents::harness::HarnessClient;
use crate::config::HooksConfig;
use crate::db::Database;
use crate::memory::manager::MemoryManager;

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
        // Check both agent-specific and shared memories
        mgr.get_recent(None, Some(agent_id), 1)
            .map(|r| !r.is_empty())
            .unwrap_or(false)
            || mgr
                .get_recent(Some("shared"), None, 1)
                .map(|r| !r.is_empty())
                .unwrap_or(false)
    }

    /// Recall: search memory and synthesize a recollection via sub-agent.
    ///
    /// Returns the recollection text to inject into the conversation,
    /// or None if no memories exist or the harness call fails.
    pub async fn recall(
        &self,
        agent_id: &str,
        conversation_context: &str,
        harness_port: u16,
    ) -> Option<String> {
        if !self.has_memories(agent_id) {
            debug!(agent_id, "no memories found, skipping recall");
            return None;
        }

        info!(agent_id, "recalling memories for first turn");

        let harness = HarnessClient::new(harness_port);
        let prompt = format!(
            "SYSTEM: You are the memory subsystem for agent '{agent_id}'. \
             Your job is to search memory for context relevant to the \
             upcoming conversation and synthesize a brief recollection.\n\n\
             Use the `search_memory` tool to find relevant memories for \
             this context:\n\n\
             {context}\n\n\
             Then write a concise narrative recollection (2-4 sentences) \
             that will help the agent respond naturally. Focus on facts, \
             preferences, past decisions, and anything the agent would \
             naturally remember. Write in first person as if you ARE the \
             agent recalling things. If you find nothing relevant, just \
             say 'No relevant memories found.'\n\n\
             Do NOT use any tools other than search_memory and list_memories.",
            agent_id = agent_id,
            context = truncate(conversation_context, 500),
        );

        match harness
            .send_task(
                "", // no separate system prompt needed
                &prompt,
                "memory-recall",
            )
            .await
        {
            Ok(response) => {
                let text = response
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default();

                if text.is_empty() || text.contains("No relevant memories") {
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

    /// Remember: review a conversation turn and create fleeting notes.
    ///
    /// This runs asynchronously after the agent has already responded.
    /// It spawns a sub-agent call to analyze the exchange and save
    /// noteworthy information to the Inbox/.
    pub async fn remember(
        &self,
        agent_id: &str,
        user_message: &str,
        agent_response: &str,
        harness_port: u16,
    ) {
        // Skip very short exchanges — nothing worth remembering
        if user_message.len() + agent_response.len() < 200 {
            debug!(agent_id, "exchange too short, skipping remember");
            return;
        }

        info!(agent_id, "running async memory remember");

        let harness = HarnessClient::new(harness_port);
        let prompt = format!(
            "SYSTEM: You are the memory subsystem for agent '{agent_id}'. \
             Review this conversation exchange and save any noteworthy \
             information as fleeting notes.\n\n\
             USER MESSAGE:\n{user_msg}\n\n\
             AGENT RESPONSE:\n{agent_resp}\n\n\
             Use `save_memory` to save important facts, decisions, user \
             preferences, or anything the agent should remember later. \
             Tag each memory with 'inbox' (these are fleeting notes that \
             will be consolidated later). Include context about WHY this \
             matters.\n\n\
             Save at most 2 memories. Skip if nothing is worth remembering. \
             Do NOT use any tools other than save_memory.",
            agent_id = agent_id,
            user_msg = truncate(user_message, 500),
            agent_resp = truncate(agent_response, 1000),
        );

        match harness.send_task("", &prompt, "memory-remember").await {
            Ok(_) => {
                info!(agent_id, "async remember complete");
            }
            Err(e) => {
                warn!(agent_id, error = %e, "async remember failed");
            }
        }
    }

    /// Consolidate: process Inbox/ fleeting notes into permanent Zettel/ notes.
    ///
    /// Called before compaction or at end-of-session. The sub-agent reviews
    /// all inbox-tagged memories, links related ones, promotes important
    /// ones to permanent notes, and cleans up duplicates.
    pub async fn consolidate(&self, agent_id: &str, harness_port: u16) {
        info!(agent_id, "running memory consolidation");

        let harness = HarnessClient::new(harness_port);
        let prompt = format!(
            "SYSTEM: You are the memory subsystem for agent '{agent_id}'. \
             It's time to consolidate your memory.\n\n\
             1. Use `list_memories` with tag 'inbox' to see all fleeting notes.\n\
             2. Review them and decide which are worth keeping permanently.\n\
             3. For important ones, use `save_memory` with tag 'zettel' \
                (permanent note) and a clear, descriptive summary.\n\
             4. Delete processed inbox notes with `delete_memory`.\n\
             5. Look for related memories and note connections in the content \
                using [[wiki-style]] links.\n\n\
             Be selective — only promote genuinely useful information to \
             permanent notes. Merge duplicates. Add context and links.",
            agent_id = agent_id,
        );

        match harness.send_task("", &prompt, "memory-consolidate").await {
            Ok(_) => {
                info!(agent_id, "memory consolidation complete");
            }
            Err(e) => {
                warn!(agent_id, error = %e, "memory consolidation failed");
            }
        }
    }
}

/// Check if hooks are enabled for an agent.
pub fn has_recall_hook(hooks: &HooksConfig) -> bool {
    hooks.before_message.iter().any(|h| h == "memory_recall")
}

pub fn has_remember_hook(hooks: &HooksConfig) -> bool {
    hooks.after_message.iter().any(|h| h == "memory_remember")
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a safe char boundary
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}
