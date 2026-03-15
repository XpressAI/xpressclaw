use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::db::Database;
use crate::error::Result;
use crate::memory::slots::MemorySlotManager;
use crate::memory::vector::{simple_embedding, VectorStore};
use crate::memory::zettelkasten::{CreateMemory, Memory, MemoryStats, Zettelkasten};

/// A search result with source attribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchResult {
    pub memory: Memory,
    pub relevance_score: f64,
    /// How this result was found: "vector", "tag", "recent", "linked", "text".
    pub source: String,
}

/// Unified memory API coordinating zettelkasten, vector search, and slots.
///
/// This is the main interface for memory operations. It:
/// - Creates memories with automatic embedding generation
/// - Searches using vector similarity + text + tags
/// - Manages near-term memory slots for agent context
/// - Handles eviction with smart linking
pub struct MemoryManager {
    zk: Zettelkasten,
    vector: VectorStore,
    slots: MemorySlotManager,
    #[allow(dead_code)]
    db: Arc<Database>,
}

impl MemoryManager {
    pub fn new(db: Arc<Database>, eviction_strategy: &str) -> Self {
        Self {
            zk: Zettelkasten::new(db.clone()),
            vector: VectorStore::new(db.clone()),
            slots: MemorySlotManager::new(db.clone(), eviction_strategy),
            db,
        }
    }

    // ── CRUD ──

    /// Add a new memory with automatic embedding.
    pub fn add(&self, req: &CreateMemory) -> Result<Memory> {
        let memory = self.zk.add(req)?;

        // Generate and store embedding
        let embedding = simple_embedding(&format!("{} {}", memory.summary, memory.content));
        if let Err(e) = self.vector.add(&memory.id, &embedding) {
            tracing::warn!(id = memory.id, error = %e, "failed to store embedding");
        }

        debug!(
            id = memory.id,
            summary = memory.summary,
            "memory added with embedding"
        );
        Ok(memory)
    }

    /// Get a memory by ID.
    pub fn get(&self, id: &str) -> Result<Memory> {
        self.zk.get(id)
    }

    /// Update a memory's content and/or summary, re-computing embedding.
    pub fn update(&self, id: &str, content: Option<&str>, summary: Option<&str>) -> Result<Memory> {
        let memory = self.zk.update(id, content, summary)?;

        // Re-compute embedding
        let embedding = simple_embedding(&format!("{} {}", memory.summary, memory.content));
        let _ = self.vector.add(&memory.id, &embedding);

        Ok(memory)
    }

    /// Delete a memory and its embedding.
    pub fn delete(&self, id: &str) -> Result<()> {
        self.vector.remove(id)?;
        self.zk.delete(id)
    }

    // ── Search ──

    /// Search memories using vector similarity.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemorySearchResult>> {
        let query_embedding = simple_embedding(query);
        let vector_results = self.vector.search(&query_embedding, limit)?;

        let mut results = Vec::new();
        for vr in vector_results {
            if let Ok(memory) = self.zk.get(&vr.memory_id) {
                results.push(MemorySearchResult {
                    memory,
                    relevance_score: vr.similarity,
                    source: "vector".to_string(),
                });
            }
        }

        Ok(results)
    }

    /// Search memories by tag.
    pub fn search_by_tag(&self, tag: &str, limit: i64) -> Result<Vec<MemorySearchResult>> {
        let memories = self.zk.find_by_tag(tag, limit)?;
        Ok(memories
            .into_iter()
            .map(|m| MemorySearchResult {
                memory: m,
                relevance_score: 1.0,
                source: "tag".to_string(),
            })
            .collect())
    }

    /// Get recently accessed memories.
    pub fn get_recent(
        &self,
        layer: Option<&str>,
        agent_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<MemorySearchResult>> {
        let memories = self.zk.find_recent(layer, agent_id, limit)?;
        Ok(memories
            .into_iter()
            .enumerate()
            .map(|(i, m)| MemorySearchResult {
                memory: m,
                // Recency score decreases with position
                relevance_score: 1.0 - (i as f64 * 0.05).min(0.9),
                source: "recent".to_string(),
            })
            .collect())
    }

    /// Search by text content (LIKE search).
    pub fn search_text(&self, query: &str, limit: i64) -> Result<Vec<MemorySearchResult>> {
        let memories = self.zk.search(query, limit)?;
        Ok(memories
            .into_iter()
            .map(|m| MemorySearchResult {
                memory: m,
                relevance_score: 0.8,
                source: "text".to_string(),
            })
            .collect())
    }

    /// Find memories related to a given memory (via graph links + vector similarity).
    pub fn find_related(&self, memory_id: &str, limit: usize) -> Result<Vec<MemorySearchResult>> {
        let mut results = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Graph traversal (1 hop)
        let linked = self.zk.traverse(memory_id, 1)?;
        for mem in linked {
            if mem.id != memory_id && seen.insert(mem.id.clone()) {
                results.push(MemorySearchResult {
                    memory: mem,
                    relevance_score: 0.9,
                    source: "linked".to_string(),
                });
            }
        }

        // Vector similarity
        if let Ok(similar) = self.vector.find_similar(memory_id, limit) {
            for vr in similar {
                if seen.insert(vr.memory_id.clone()) {
                    if let Ok(memory) = self.zk.get(&vr.memory_id) {
                        results.push(MemorySearchResult {
                            memory,
                            relevance_score: vr.similarity,
                            source: "vector".to_string(),
                        });
                    }
                }
            }
        }

        // Sort by relevance
        results.sort_by(|a, b| {
            b.relevance_score
                .partial_cmp(&a.relevance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);

        Ok(results)
    }

    // ── Slots ──

    /// Load a memory into a slot for an agent.
    ///
    /// If eviction occurs, the evicted memory is linked to related memories
    /// before removal from slots.
    pub fn load_to_slot(&self, agent_id: &str, memory_id: &str, relevance: f64) -> Result<()> {
        let evicted = self.slots.load(agent_id, memory_id, relevance)?;

        // If a memory was evicted, try to link it to related memories
        if let Some(evicted_id) = evicted {
            if let Ok(similar) = self.vector.find_similar(&evicted_id, 3) {
                for vr in similar {
                    if vr.similarity > 0.5 {
                        let _ = self
                            .zk
                            .link(&evicted_id, &vr.memory_id, "similar", vr.similarity);
                    }
                }
            }
        }

        Ok(())
    }

    /// Get the memory slots for an agent.
    pub fn get_slots(&self, agent_id: &str) -> Result<Vec<crate::memory::slots::MemorySlot>> {
        self.slots.get_slots(agent_id)
    }

    /// Build a context string from an agent's loaded memories for prompt injection.
    ///
    /// Returns the full memory content (not just IDs) formatted for the agent.
    pub fn get_context_for_agent(&self, agent_id: &str) -> Result<String> {
        let slots = self.slots.get_slots(agent_id)?;
        let occupied: Vec<_> = slots.iter().filter(|s| s.memory_id.is_some()).collect();

        if occupied.is_empty() {
            return Ok(String::new());
        }

        let mut context = String::from("## Active Memories\n\n");
        for slot in &occupied {
            if let Some(ref mid) = slot.memory_id {
                if let Ok(memory) = self.zk.get(mid) {
                    context.push_str(&format!(
                        "### [{}] {} (relevance: {:.2})\n{}\n\n",
                        slot.slot_index,
                        memory.summary,
                        slot.relevance_score.unwrap_or(0.0),
                        memory.content,
                    ));
                }
            }
        }

        Ok(context)
    }

    /// Refresh slots for an agent based on a query.
    ///
    /// Searches for relevant memories and loads the top results into slots.
    pub fn refresh_slots(&self, agent_id: &str, query: &str, max_load: usize) -> Result<()> {
        let results = self.search(query, max_load)?;

        for result in results {
            self.load_to_slot(agent_id, &result.memory.id, result.relevance_score)?;
        }

        Ok(())
    }

    // ── Stats ──

    /// Get combined statistics from all memory subsystems.
    pub fn get_stats(&self) -> Result<CombinedStats> {
        let zk_stats = self.zk.get_stats()?;
        let vec_stats = self.vector.get_stats()?;

        Ok(CombinedStats {
            zettelkasten: zk_stats,
            vector: vec_stats,
        })
    }

    /// Access the underlying zettelkasten.
    pub fn zettelkasten(&self) -> &Zettelkasten {
        &self.zk
    }

    /// Access the underlying vector store.
    pub fn vector_store(&self) -> &VectorStore {
        &self.vector
    }

    /// Access the underlying slot manager.
    pub fn slot_manager(&self) -> &MemorySlotManager {
        &self.slots
    }
}

/// Combined statistics from all memory subsystems.
#[derive(Debug, Clone, Serialize)]
pub struct CombinedStats {
    pub zettelkasten: MemoryStats,
    pub vector: crate::memory::vector::VectorStats,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, MemoryManager) {
        let db = Arc::new(Database::open_memory().unwrap());
        let mgr = MemoryManager::new(db.clone(), "least-recently-relevant");
        (db, mgr)
    }

    fn add_memory(mgr: &MemoryManager, summary: &str, content: &str) -> Memory {
        mgr.add(&CreateMemory {
            content: content.to_string(),
            summary: summary.to_string(),
            source: "test".to_string(),
            layer: "shared".to_string(),
            agent_id: None,
            user_id: None,
            tags: vec![],
        })
        .unwrap()
    }

    #[test]
    fn test_add_creates_embedding() {
        let (_, mgr) = setup();

        let mem = add_memory(&mgr, "Test memory", "Some content for testing");
        assert!(mgr.vector_store().has_embedding(&mem.id));
    }

    #[test]
    fn test_delete_removes_embedding() {
        let (_, mgr) = setup();

        let mem = add_memory(&mgr, "Deletable", "Will be deleted");
        assert!(mgr.vector_store().has_embedding(&mem.id));

        mgr.delete(&mem.id).unwrap();
        assert!(!mgr.vector_store().has_embedding(&mem.id));
    }

    #[test]
    fn test_update_recomputes_embedding() {
        let (_, mgr) = setup();

        let mem = add_memory(&mgr, "Original", "Original content");
        let orig_emb = mgr.vector_store().get_embedding(&mem.id).unwrap();

        mgr.update(&mem.id, Some("Completely different content"), None)
            .unwrap();
        let new_emb = mgr.vector_store().get_embedding(&mem.id).unwrap();

        // Embedding should have changed
        assert_ne!(orig_emb, new_emb);
    }

    #[test]
    fn test_semantic_search() {
        let (_, mgr) = setup();

        add_memory(
            &mgr,
            "Rust language",
            "Rust is a systems programming language focused on safety",
        );
        add_memory(
            &mgr,
            "Python language",
            "Python is a high-level scripting language",
        );
        add_memory(
            &mgr,
            "Italian cooking",
            "How to make pasta carbonara with eggs and cheese",
        );

        let results = mgr.search("programming language", 3).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].source, "vector");
        // Programming memories should rank higher
        assert!(results[0].relevance_score >= results[2].relevance_score);
    }

    #[test]
    fn test_search_by_tag() {
        let (_, mgr) = setup();

        mgr.add(&CreateMemory {
            content: "Tagged content".into(),
            summary: "Tagged".into(),
            source: "test".into(),
            layer: "shared".into(),
            agent_id: None,
            user_id: None,
            tags: vec!["important".into()],
        })
        .unwrap();

        let results = mgr.search_by_tag("important", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, "tag");
    }

    #[test]
    fn test_find_related() {
        let (_, mgr) = setup();

        let m1 = add_memory(&mgr, "Cats", "I love cats and kittens");
        let m2 = add_memory(&mgr, "Dogs", "I love dogs and puppies");
        let _m3 = add_memory(&mgr, "Physics", "Quantum entanglement is fascinating");

        // Link m1 and m2
        mgr.zettelkasten()
            .link(&m1.id, &m2.id, "related", 1.0)
            .unwrap();

        let related = mgr.find_related(&m1.id, 5).unwrap();
        assert!(!related.is_empty());
        // m2 should be in related results (via link)
        assert!(related.iter().any(|r| r.memory.id == m2.id));
    }

    #[test]
    fn test_slots_integration() {
        let (_, mgr) = setup();

        let m1 = add_memory(&mgr, "Slot test 1", "First memory for slots");
        let m2 = add_memory(&mgr, "Slot test 2", "Second memory for slots");

        mgr.load_to_slot("atlas", &m1.id, 0.9).unwrap();
        mgr.load_to_slot("atlas", &m2.id, 0.7).unwrap();

        let slots = mgr.get_slots("atlas").unwrap();
        let occupied: Vec<_> = slots.iter().filter(|s| s.memory_id.is_some()).collect();
        assert_eq!(occupied.len(), 2);
    }

    #[test]
    fn test_context_for_agent() {
        let (_, mgr) = setup();

        let mem = add_memory(
            &mgr,
            "Context test",
            "This is important context for the agent",
        );
        mgr.load_to_slot("atlas", &mem.id, 0.85).unwrap();

        let context = mgr.get_context_for_agent("atlas").unwrap();
        assert!(context.contains("Active Memories"));
        assert!(context.contains("Context test"));
        assert!(context.contains("important context"));
        assert!(context.contains("0.85"));
    }

    #[test]
    fn test_refresh_slots() {
        let (_, mgr) = setup();

        add_memory(
            &mgr,
            "Rust systems",
            "Rust systems programming memory safety",
        );
        add_memory(
            &mgr,
            "Python scripting",
            "Python high level scripting language",
        );

        mgr.refresh_slots("atlas", "programming language", 2)
            .unwrap();

        let slots = mgr.get_slots("atlas").unwrap();
        let occupied: Vec<_> = slots.iter().filter(|s| s.memory_id.is_some()).collect();
        assert!(!occupied.is_empty()); // at least one relevant memory loaded
    }

    #[test]
    fn test_stats() {
        let (_, mgr) = setup();

        add_memory(&mgr, "Stats test", "Content for stats");

        let stats = mgr.get_stats().unwrap();
        assert_eq!(stats.zettelkasten.total_memories, 1);
        assert_eq!(stats.vector.embedding_count, 1);
        assert_eq!(stats.vector.dimension, 384);
    }
}
