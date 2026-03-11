use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::db::Database;
use crate::error::{Error, Result};

/// Maximum number of near-term memory slots per agent.
pub const MAX_SLOTS: u8 = 8;

/// A single memory slot holding a reference to a memory with relevance score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySlot {
    pub agent_id: String,
    pub slot_index: i32,
    pub memory_id: Option<String>,
    pub relevance_score: Option<f64>,
    pub loaded_at: Option<String>,
}

/// Manages 8 near-term memory slots per agent.
///
/// Slots are "working memory" — the most relevant memories currently active
/// for an agent's context. When slots are full, eviction happens based on
/// the configured strategy.
pub struct MemorySlotManager {
    db: Arc<Database>,
    eviction_strategy: EvictionStrategy,
}

/// Strategy for evicting memories when slots are full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionStrategy {
    /// Evict the least recently loaded slot.
    Lru,
    /// Evict the slot with the lowest combined score of relevance × recency.
    LeastRecentlyRelevant,
}

impl From<&str> for EvictionStrategy {
    fn from(s: &str) -> Self {
        match s {
            "lru" => Self::Lru,
            _ => Self::LeastRecentlyRelevant,
        }
    }
}

impl MemorySlotManager {
    pub fn new(db: Arc<Database>, eviction_strategy: &str) -> Self {
        Self {
            db,
            eviction_strategy: EvictionStrategy::from(eviction_strategy),
        }
    }

    /// Initialize slots for an agent (creates empty slots if they don't exist).
    pub fn init_slots(&self, agent_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            for i in 0..MAX_SLOTS {
                conn.execute(
                    "INSERT OR IGNORE INTO memory_slots (agent_id, slot_index) VALUES (?1, ?2)",
                    rusqlite::params![agent_id, i as i32],
                )?;
            }
            Ok::<_, Error>(())
        })?;
        Ok(())
    }

    /// Load a memory into a slot. If all slots are full, evicts one first.
    ///
    /// Returns the evicted memory ID if one was evicted.
    pub fn load(
        &self,
        agent_id: &str,
        memory_id: &str,
        relevance: f64,
    ) -> Result<Option<String>> {
        self.init_slots(agent_id)?;

        // Check if this memory is already loaded
        let existing = self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT slot_index FROM memory_slots \
                     WHERE agent_id = ?1 AND memory_id = ?2",
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            let idx: Option<i32> = stmt
                .query_row(rusqlite::params![agent_id, memory_id], |row| row.get(0))
                .ok();
            Ok::<_, Error>(idx)
        })?;

        if let Some(idx) = existing {
            // Update relevance for existing slot
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE memory_slots SET relevance_score = ?1, loaded_at = CURRENT_TIMESTAMP \
                     WHERE agent_id = ?2 AND slot_index = ?3",
                    rusqlite::params![relevance, agent_id, idx],
                )
            })?;
            return Ok(None);
        }

        // Find an empty slot
        let empty_slot = self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT slot_index FROM memory_slots \
                     WHERE agent_id = ?1 AND memory_id IS NULL \
                     ORDER BY slot_index ASC LIMIT 1",
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            let idx: Option<i32> = stmt
                .query_row([agent_id], |row| row.get(0))
                .ok();
            Ok::<_, Error>(idx)
        })?;

        if let Some(slot_idx) = empty_slot {
            // Load into empty slot
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE memory_slots SET memory_id = ?1, relevance_score = ?2, loaded_at = CURRENT_TIMESTAMP \
                     WHERE agent_id = ?3 AND slot_index = ?4",
                    rusqlite::params![memory_id, relevance, agent_id, slot_idx],
                )
            })?;
            debug!(agent_id, memory_id, slot = slot_idx, "loaded memory into slot");
            return Ok(None);
        }

        // All slots full — evict one
        let evict_slot = self.find_eviction_candidate(agent_id)?;
        let evicted_memory_id = self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT memory_id FROM memory_slots \
                     WHERE agent_id = ?1 AND slot_index = ?2",
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            let mid: Option<String> = stmt
                .query_row(rusqlite::params![agent_id, evict_slot], |row| row.get(0))
                .ok()
                .flatten();
            Ok::<_, Error>(mid)
        })?;

        // Replace the evicted slot
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE memory_slots SET memory_id = ?1, relevance_score = ?2, loaded_at = CURRENT_TIMESTAMP \
                 WHERE agent_id = ?3 AND slot_index = ?4",
                rusqlite::params![memory_id, relevance, agent_id, evict_slot],
            )
        })?;

        debug!(
            agent_id,
            memory_id,
            slot = evict_slot,
            evicted = ?evicted_memory_id,
            "evicted and loaded memory into slot"
        );

        Ok(evicted_memory_id)
    }

    /// Unload a memory from its slot.
    pub fn unload(&self, agent_id: &str, memory_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE memory_slots SET memory_id = NULL, relevance_score = NULL, loaded_at = NULL \
                 WHERE agent_id = ?1 AND memory_id = ?2",
                rusqlite::params![agent_id, memory_id],
            )
        })?;
        Ok(())
    }

    /// Update the relevance score of a loaded memory.
    pub fn update_relevance(
        &self,
        agent_id: &str,
        memory_id: &str,
        relevance: f64,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE memory_slots SET relevance_score = ?1 \
                 WHERE agent_id = ?2 AND memory_id = ?3",
                rusqlite::params![relevance, agent_id, memory_id],
            )
        })?;
        Ok(())
    }

    /// Clear all slots for an agent.
    pub fn clear(&self, agent_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE memory_slots SET memory_id = NULL, relevance_score = NULL, loaded_at = NULL \
                 WHERE agent_id = ?1",
                [agent_id],
            )
        })?;
        Ok(())
    }

    /// Get all slots for an agent.
    pub fn get_slots(&self, agent_id: &str) -> Result<Vec<MemorySlot>> {
        self.init_slots(agent_id)?;

        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT agent_id, slot_index, memory_id, relevance_score, loaded_at \
                     FROM memory_slots WHERE agent_id = ?1 ORDER BY slot_index",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let slots = stmt
                .query_map([agent_id], |row| {
                    Ok(MemorySlot {
                        agent_id: row.get(0)?,
                        slot_index: row.get(1)?,
                        memory_id: row.get(2)?,
                        relevance_score: row.get(3)?,
                        loaded_at: row.get(4)?,
                    })
                })
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(slots)
        })
    }

    /// Get the count of occupied slots for an agent.
    pub fn occupied_count(&self, agent_id: &str) -> Result<i64> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM memory_slots WHERE agent_id = ?1 AND memory_id IS NOT NULL",
                [agent_id],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(e.to_string()))
        })
    }

    /// Build a context string from loaded memories for prompt injection.
    pub fn get_context_string(&self, agent_id: &str) -> Result<String> {
        let slots = self.get_slots(agent_id)?;
        let occupied: Vec<&MemorySlot> = slots
            .iter()
            .filter(|s| s.memory_id.is_some())
            .collect();

        if occupied.is_empty() {
            return Ok(String::new());
        }

        let mut context = String::from("## Active Memories\n\n");
        for slot in &occupied {
            if let Some(ref mid) = slot.memory_id {
                // We only have the ID here; the manager layer will resolve to full content
                context.push_str(&format!(
                    "- [Slot {}] Memory {}: (relevance: {:.2})\n",
                    slot.slot_index,
                    mid,
                    slot.relevance_score.unwrap_or(0.0),
                ));
            }
        }

        Ok(context)
    }

    /// Find the slot to evict based on the eviction strategy.
    fn find_eviction_candidate(&self, agent_id: &str) -> Result<i32> {
        match self.eviction_strategy {
            EvictionStrategy::Lru => {
                // Evict the slot loaded longest ago
                self.db.with_conn(|conn| {
                    conn.query_row(
                        "SELECT slot_index FROM memory_slots \
                         WHERE agent_id = ?1 AND memory_id IS NOT NULL \
                         ORDER BY loaded_at ASC LIMIT 1",
                        [agent_id],
                        |row| row.get(0),
                    )
                    .map_err(|e| Error::Memory(format!("no slot to evict: {e}")))
                })
            }
            EvictionStrategy::LeastRecentlyRelevant => {
                // Score = relevance * recency_factor
                // We approximate recency by sorting by relevance ASC, loaded_at ASC
                self.db.with_conn(|conn| {
                    conn.query_row(
                        "SELECT slot_index FROM memory_slots \
                         WHERE agent_id = ?1 AND memory_id IS NOT NULL \
                         ORDER BY COALESCE(relevance_score, 0) ASC, loaded_at ASC LIMIT 1",
                        [agent_id],
                        |row| row.get(0),
                    )
                    .map_err(|e| Error::Memory(format!("no slot to evict: {e}")))
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, MemorySlotManager) {
        let db = Arc::new(Database::open_memory().unwrap());
        let mgr = MemorySlotManager::new(db.clone(), "least-recently-relevant");
        (db, mgr)
    }

    /// Helper: insert a fake memory so slot foreign key is satisfied.
    fn insert_memory(db: &Database, id: &str) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO memories (id, content, summary, source, layer) VALUES (?1, ?1, ?1, 'test', 'shared')",
                [id],
            )
            .unwrap();
        });
    }

    #[test]
    fn test_init_and_get_slots() {
        let (_, mgr) = setup();
        mgr.init_slots("atlas").unwrap();

        let slots = mgr.get_slots("atlas").unwrap();
        assert_eq!(slots.len(), MAX_SLOTS as usize);
        assert!(slots.iter().all(|s| s.memory_id.is_none()));
    }

    #[test]
    fn test_load_and_unload() {
        let (db, mgr) = setup();
        insert_memory(&db, "mem-1");

        let evicted = mgr.load("atlas", "mem-1", 0.9).unwrap();
        assert!(evicted.is_none());

        let count = mgr.occupied_count("atlas").unwrap();
        assert_eq!(count, 1);

        mgr.unload("atlas", "mem-1").unwrap();
        let count = mgr.occupied_count("atlas").unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_load_duplicate_updates_relevance() {
        let (db, mgr) = setup();
        insert_memory(&db, "mem-1");

        mgr.load("atlas", "mem-1", 0.5).unwrap();
        mgr.load("atlas", "mem-1", 0.9).unwrap(); // should update, not duplicate

        let count = mgr.occupied_count("atlas").unwrap();
        assert_eq!(count, 1);

        let slots = mgr.get_slots("atlas").unwrap();
        let loaded = slots.iter().find(|s| s.memory_id.as_deref() == Some("mem-1")).unwrap();
        assert!((loaded.relevance_score.unwrap() - 0.9).abs() < 0.01);
    }

    #[test]
    fn test_eviction_when_full() {
        let (db, mgr) = setup();

        // Fill all 8 slots
        for i in 0..MAX_SLOTS {
            let id = format!("mem-{i}");
            insert_memory(&db, &id);
            let evicted = mgr.load("atlas", &id, 0.5).unwrap();
            assert!(evicted.is_none());
        }

        assert_eq!(mgr.occupied_count("atlas").unwrap(), 8);

        // Load one more — should evict
        insert_memory(&db, "mem-new");
        let evicted = mgr.load("atlas", "mem-new", 0.95).unwrap();
        assert!(evicted.is_some()); // one was evicted

        // Still 8 slots occupied
        assert_eq!(mgr.occupied_count("atlas").unwrap(), 8);

        // The new one should be in there
        let slots = mgr.get_slots("atlas").unwrap();
        assert!(slots.iter().any(|s| s.memory_id.as_deref() == Some("mem-new")));
    }

    #[test]
    fn test_clear() {
        let (db, mgr) = setup();
        insert_memory(&db, "mem-1");
        insert_memory(&db, "mem-2");

        mgr.load("atlas", "mem-1", 0.5).unwrap();
        mgr.load("atlas", "mem-2", 0.7).unwrap();
        assert_eq!(mgr.occupied_count("atlas").unwrap(), 2);

        mgr.clear("atlas").unwrap();
        assert_eq!(mgr.occupied_count("atlas").unwrap(), 0);
    }

    #[test]
    fn test_update_relevance() {
        let (db, mgr) = setup();
        insert_memory(&db, "mem-1");

        mgr.load("atlas", "mem-1", 0.5).unwrap();
        mgr.update_relevance("atlas", "mem-1", 0.99).unwrap();

        let slots = mgr.get_slots("atlas").unwrap();
        let slot = slots.iter().find(|s| s.memory_id.as_deref() == Some("mem-1")).unwrap();
        assert!((slot.relevance_score.unwrap() - 0.99).abs() < 0.01);
    }

    #[test]
    fn test_context_string() {
        let (db, mgr) = setup();
        insert_memory(&db, "mem-1");

        mgr.load("atlas", "mem-1", 0.85).unwrap();
        let ctx = mgr.get_context_string("atlas").unwrap();
        assert!(ctx.contains("Active Memories"));
        assert!(ctx.contains("mem-1"));
        assert!(ctx.contains("0.85"));
    }

    #[test]
    fn test_lru_eviction() {
        let db = Arc::new(Database::open_memory().unwrap());
        let mgr = MemorySlotManager::new(db.clone(), "lru");

        // Fill slots
        for i in 0..MAX_SLOTS {
            let id = format!("mem-{i}");
            insert_memory(&db, &id);
            mgr.load("atlas", &id, 0.5 + i as f64 * 0.05).unwrap();
        }

        // mem-0 was loaded first, should be evicted with LRU
        insert_memory(&db, "mem-new");
        let evicted = mgr.load("atlas", "mem-new", 0.5).unwrap();
        assert_eq!(evicted.as_deref(), Some("mem-0"));
    }
}
