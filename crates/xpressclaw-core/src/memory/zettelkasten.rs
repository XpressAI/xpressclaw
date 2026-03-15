use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::db::Database;
use crate::error::{Error, Result};

/// A single memory note in the Zettelkasten.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub content: String,
    pub summary: String,
    pub created_at: String,
    pub accessed_at: String,
    pub access_count: i64,
    pub source: String,
    /// Visibility layer: "shared", "user", or "agent".
    pub layer: String,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links: Vec<MemoryLink>,
}

/// A link between two memories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLink {
    pub from_id: String,
    pub to_id: String,
    pub link_type: String,
    pub strength: f64,
}

/// Input for creating a memory.
#[derive(Debug, Deserialize)]
pub struct CreateMemory {
    pub content: String,
    pub summary: String,
    pub source: String,
    #[serde(default = "default_layer")]
    pub layer: String,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_layer() -> String {
    "shared".to_string()
}

/// Graph-based knowledge store with bidirectional links and tags.
pub struct Zettelkasten {
    db: Arc<Database>,
}

impl Zettelkasten {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Add a new memory note.
    pub fn add(&self, req: &CreateMemory) -> Result<Memory> {
        let id = uuid::Uuid::new_v4().to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO memories (id, content, summary, source, layer, agent_id, user_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    id,
                    req.content,
                    req.summary,
                    req.source,
                    req.layer,
                    req.agent_id,
                    req.user_id,
                ],
            )?;

            // Insert tags
            for tag in &req.tags {
                conn.execute(
                    "INSERT OR IGNORE INTO memory_tags (memory_id, tag) VALUES (?1, ?2)",
                    rusqlite::params![id, tag],
                )?;
            }

            Ok::<_, Error>(())
        })?;

        // Parse [[wiki-style]] links from content and auto-link
        let link_targets = parse_wiki_links(&req.content);
        for target_id in &link_targets {
            let _ = self.link(&id, target_id, "reference", 1.0);
        }

        debug!(id, summary = req.summary, tags = ?req.tags, "added memory");
        self.get(&id)
    }

    /// Get a memory by ID, updating access stats.
    pub fn get(&self, id: &str) -> Result<Memory> {
        // Update access stats
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET accessed_at = CURRENT_TIMESTAMP, access_count = access_count + 1 WHERE id = ?1",
                [id],
            )
        })?;

        self.get_without_access(id)
    }

    /// Get a memory without updating access stats (for internal use).
    fn get_without_access(&self, id: &str) -> Result<Memory> {
        let memory = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, content, summary, created_at, accessed_at, access_count, source, layer, agent_id, user_id \
                 FROM memories WHERE id = ?1",
            )?;

            stmt.query_row([id], |row| {
                Ok(Memory {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    summary: row.get(2)?,
                    created_at: row.get(3)?,
                    accessed_at: row.get(4)?,
                    access_count: row.get(5)?,
                    source: row.get(6)?,
                    layer: row.get(7)?,
                    agent_id: row.get(8)?,
                    user_id: row.get(9)?,
                    tags: Vec::new(),
                    links: Vec::new(),
                })
            })
            .map_err(|_| Error::MemoryNotFound { id: id.to_string() })
        })?;

        // Load tags
        let tags = self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT tag FROM memory_tags WHERE memory_id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;
            let tags: Vec<String> = stmt
                .query_map([id], |row| row.get(0))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();
            Ok::<_, Error>(tags)
        })?;

        // Load links
        let links = self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT from_id, to_id, link_type, strength FROM memory_links \
                     WHERE from_id = ?1 OR to_id = ?1",
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            let links: Vec<MemoryLink> = stmt
                .query_map([id], |row| {
                    Ok(MemoryLink {
                        from_id: row.get(0)?,
                        to_id: row.get(1)?,
                        link_type: row.get(2)?,
                        strength: row.get(3)?,
                    })
                })
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();
            Ok::<_, Error>(links)
        })?;

        Ok(Memory {
            tags,
            links,
            ..memory
        })
    }

    /// Update a memory's content and/or summary.
    pub fn update(&self, id: &str, content: Option<&str>, summary: Option<&str>) -> Result<Memory> {
        // Verify it exists
        self.get_without_access(id)?;

        self.db.with_conn(|conn| {
            if let Some(c) = content {
                conn.execute(
                    "UPDATE memories SET content = ?1, accessed_at = CURRENT_TIMESTAMP WHERE id = ?2",
                    rusqlite::params![c, id],
                )?;
            }
            if let Some(s) = summary {
                conn.execute(
                    "UPDATE memories SET summary = ?1, accessed_at = CURRENT_TIMESTAMP WHERE id = ?2",
                    rusqlite::params![s, id],
                )?;
            }
            Ok::<_, Error>(())
        })?;

        self.get(id)
    }

    /// Delete a memory (cascading deletes handle links and tags).
    pub fn delete(&self, id: &str) -> Result<()> {
        self.db
            .with_conn(|conn| conn.execute("DELETE FROM memories WHERE id = ?1", [id]))?;
        Ok(())
    }

    /// Create a bidirectional link between two memories.
    pub fn link(&self, from_id: &str, to_id: &str, link_type: &str, strength: f64) -> Result<()> {
        self.db.with_conn(|conn| {
            // Forward link
            conn.execute(
                "INSERT OR REPLACE INTO memory_links (from_id, to_id, link_type, strength) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![from_id, to_id, link_type, strength],
            )?;
            // Reverse link
            conn.execute(
                "INSERT OR REPLACE INTO memory_links (from_id, to_id, link_type, strength) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![to_id, from_id, link_type, strength],
            )?;
            Ok::<_, Error>(())
        })?;

        debug!(from_id, to_id, link_type, "linked memories");
        Ok(())
    }

    /// Add tags to a memory.
    pub fn add_tags(&self, id: &str, tags: &[String]) -> Result<()> {
        self.db.with_conn(|conn| {
            for tag in tags {
                conn.execute(
                    "INSERT OR IGNORE INTO memory_tags (memory_id, tag) VALUES (?1, ?2)",
                    rusqlite::params![id, tag],
                )?;
            }
            Ok::<_, Error>(())
        })?;
        Ok(())
    }

    /// Remove tags from a memory.
    pub fn remove_tags(&self, id: &str, tags: &[String]) -> Result<()> {
        self.db.with_conn(|conn| {
            for tag in tags {
                conn.execute(
                    "DELETE FROM memory_tags WHERE memory_id = ?1 AND tag = ?2",
                    rusqlite::params![id, tag],
                )?;
            }
            Ok::<_, Error>(())
        })?;
        Ok(())
    }

    /// Find memories by tag.
    pub fn find_by_tag(&self, tag: &str, limit: i64) -> Result<Vec<Memory>> {
        let ids: Vec<String> = self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT memory_id FROM memory_tags WHERE tag = ?1 \
                     ORDER BY memory_id LIMIT ?2",
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            let ids = stmt
                .query_map(rusqlite::params![tag, limit], |row| row.get(0))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();
            Ok::<_, Error>(ids)
        })?;

        ids.iter().map(|id| self.get_without_access(id)).collect()
    }

    /// Find recently accessed memories.
    pub fn find_recent(
        &self,
        layer: Option<&str>,
        agent_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Memory>> {
        let ids: Vec<String> = self.db.with_conn(|conn| {
            let mut sql = "SELECT id FROM memories WHERE 1=1".to_string();
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            if let Some(l) = layer {
                sql.push_str(" AND layer = ?");
                params.push(Box::new(l.to_string()));
            }
            if let Some(aid) = agent_id {
                sql.push_str(" AND (agent_id = ? OR layer = 'shared')");
                params.push(Box::new(aid.to_string()));
            }
            sql.push_str(" ORDER BY accessed_at DESC LIMIT ?");
            params.push(Box::new(limit));

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| Error::Database(e.to_string()))?;
            let ids = stmt
                .query_map(param_refs.as_slice(), |row| row.get(0))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();
            Ok::<_, Error>(ids)
        })?;

        ids.iter().map(|id| self.get_without_access(id)).collect()
    }

    /// Traverse the knowledge graph from a starting memory.
    ///
    /// Returns memories reachable within `depth` hops.
    pub fn traverse(&self, start_id: &str, depth: u32) -> Result<Vec<Memory>> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = vec![start_id.to_string()];
        let mut result = Vec::new();

        for _ in 0..=depth {
            let mut next_queue = Vec::new();
            for id in &queue {
                if visited.contains(id) {
                    continue;
                }
                visited.insert(id.clone());

                if let Ok(memory) = self.get_without_access(id) {
                    let neighbor_ids: Vec<String> = memory
                        .links
                        .iter()
                        .map(|l| {
                            if l.from_id == *id {
                                l.to_id.clone()
                            } else {
                                l.from_id.clone()
                            }
                        })
                        .collect();
                    next_queue.extend(neighbor_ids);
                    result.push(memory);
                }
            }
            queue = next_queue;
        }

        Ok(result)
    }

    /// Search memories by content (simple LIKE search).
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<Memory>> {
        let pattern = format!("%{query}%");
        let ids: Vec<String> = self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM memories WHERE content LIKE ?1 OR summary LIKE ?1 \
                     ORDER BY accessed_at DESC LIMIT ?2",
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            let ids = stmt
                .query_map(rusqlite::params![pattern, limit], |row| row.get(0))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();
            Ok::<_, Error>(ids)
        })?;

        ids.iter().map(|id| self.get_without_access(id)).collect()
    }

    /// Get statistics about the memory store.
    pub fn get_stats(&self) -> Result<MemoryStats> {
        self.db.with_conn(|conn| {
            let total: i64 = conn
                .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
                .map_err(|e| Error::Database(e.to_string()))?;

            let links: i64 = conn
                .query_row("SELECT COUNT(*) FROM memory_links", [], |row| row.get(0))
                .map_err(|e| Error::Database(e.to_string()))?;

            let tags: i64 = conn
                .query_row("SELECT COUNT(DISTINCT tag) FROM memory_tags", [], |row| {
                    row.get(0)
                })
                .map_err(|e| Error::Database(e.to_string()))?;

            Ok(MemoryStats {
                total_memories: total,
                total_links: links,
                unique_tags: tags,
            })
        })
    }
}

/// Statistics about the memory store.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryStats {
    pub total_memories: i64,
    pub total_links: i64,
    #[serde(rename = "total_tags")]
    pub unique_tags: i64,
}

/// Parse `[[wiki-style]]` link targets from content.
fn parse_wiki_links(content: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("[[") {
        rest = &rest[start + 2..];
        if let Some(end) = rest.find("]]") {
            let target = rest[..end].trim().to_string();
            if !target.is_empty() {
                links.push(target);
            }
            rest = &rest[end + 2..];
        } else {
            break;
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, Zettelkasten) {
        let db = Arc::new(Database::open_memory().unwrap());
        let zk = Zettelkasten::new(db.clone());
        (db, zk)
    }

    #[test]
    fn test_add_and_get() {
        let (_, zk) = setup();

        let mem = zk
            .add(&CreateMemory {
                content: "Rust is a systems programming language".into(),
                summary: "Rust language overview".into(),
                source: "user".into(),
                layer: "shared".into(),
                agent_id: None,
                user_id: None,
                tags: vec!["rust".into(), "programming".into()],
            })
            .unwrap();

        assert_eq!(mem.summary, "Rust language overview");
        assert_eq!(mem.layer, "shared");
        assert_eq!(mem.tags.len(), 2);
        assert!(mem.tags.contains(&"rust".to_string()));

        let fetched = zk.get(&mem.id).unwrap();
        assert_eq!(fetched.id, mem.id);
        assert!(fetched.access_count >= 1);
    }

    #[test]
    fn test_update() {
        let (_, zk) = setup();

        let mem = zk
            .add(&CreateMemory {
                content: "Original content".into(),
                summary: "Original".into(),
                source: "user".into(),
                layer: "shared".into(),
                agent_id: None,
                user_id: None,
                tags: vec![],
            })
            .unwrap();

        let updated = zk
            .update(&mem.id, Some("Updated content"), Some("Updated"))
            .unwrap();
        assert_eq!(updated.content, "Updated content");
        assert_eq!(updated.summary, "Updated");
    }

    #[test]
    fn test_delete() {
        let (_, zk) = setup();

        let mem = zk
            .add(&CreateMemory {
                content: "To be deleted".into(),
                summary: "Delete me".into(),
                source: "user".into(),
                layer: "shared".into(),
                agent_id: None,
                user_id: None,
                tags: vec!["temp".into()],
            })
            .unwrap();

        zk.delete(&mem.id).unwrap();
        assert!(zk.get(&mem.id).is_err());
    }

    #[test]
    fn test_link_and_traverse() {
        let (_, zk) = setup();

        let m1 = zk
            .add(&CreateMemory {
                content: "Memory A".into(),
                summary: "A".into(),
                source: "user".into(),
                layer: "shared".into(),
                agent_id: None,
                user_id: None,
                tags: vec![],
            })
            .unwrap();

        let m2 = zk
            .add(&CreateMemory {
                content: "Memory B".into(),
                summary: "B".into(),
                source: "user".into(),
                layer: "shared".into(),
                agent_id: None,
                user_id: None,
                tags: vec![],
            })
            .unwrap();

        let m3 = zk
            .add(&CreateMemory {
                content: "Memory C".into(),
                summary: "C".into(),
                source: "user".into(),
                layer: "shared".into(),
                agent_id: None,
                user_id: None,
                tags: vec![],
            })
            .unwrap();

        // A -> B -> C
        zk.link(&m1.id, &m2.id, "related", 1.0).unwrap();
        zk.link(&m2.id, &m3.id, "related", 1.0).unwrap();

        // Traverse from A with depth 1 should get A and B
        let result = zk.traverse(&m1.id, 1).unwrap();
        assert_eq!(result.len(), 2);

        // Traverse from A with depth 2 should get A, B, and C
        let result = zk.traverse(&m1.id, 2).unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_find_by_tag() {
        let (_, zk) = setup();

        zk.add(&CreateMemory {
            content: "Rust memory".into(),
            summary: "Rust".into(),
            source: "user".into(),
            layer: "shared".into(),
            agent_id: None,
            user_id: None,
            tags: vec!["rust".into(), "lang".into()],
        })
        .unwrap();

        zk.add(&CreateMemory {
            content: "Python memory".into(),
            summary: "Python".into(),
            source: "user".into(),
            layer: "shared".into(),
            agent_id: None,
            user_id: None,
            tags: vec!["python".into(), "lang".into()],
        })
        .unwrap();

        let rust = zk.find_by_tag("rust", 10).unwrap();
        assert_eq!(rust.len(), 1);

        let langs = zk.find_by_tag("lang", 10).unwrap();
        assert_eq!(langs.len(), 2);
    }

    #[test]
    fn test_find_recent() {
        let (_, zk) = setup();

        zk.add(&CreateMemory {
            content: "Shared memory".into(),
            summary: "Shared".into(),
            source: "user".into(),
            layer: "shared".into(),
            agent_id: None,
            user_id: None,
            tags: vec![],
        })
        .unwrap();

        zk.add(&CreateMemory {
            content: "Agent memory".into(),
            summary: "Agent".into(),
            source: "agent".into(),
            layer: "agent".into(),
            agent_id: Some("atlas".into()),
            user_id: None,
            tags: vec![],
        })
        .unwrap();

        let all = zk.find_recent(None, None, 10).unwrap();
        assert_eq!(all.len(), 2);

        let shared = zk.find_recent(Some("shared"), None, 10).unwrap();
        assert_eq!(shared.len(), 1);

        // Agent sees its own + shared
        let for_atlas = zk.find_recent(None, Some("atlas"), 10).unwrap();
        assert_eq!(for_atlas.len(), 2);
    }

    #[test]
    fn test_search() {
        let (_, zk) = setup();

        zk.add(&CreateMemory {
            content: "The quick brown fox jumps over the lazy dog".into(),
            summary: "Fox and dog".into(),
            source: "user".into(),
            layer: "shared".into(),
            agent_id: None,
            user_id: None,
            tags: vec![],
        })
        .unwrap();

        zk.add(&CreateMemory {
            content: "Rust is blazingly fast".into(),
            summary: "Rust speed".into(),
            source: "user".into(),
            layer: "shared".into(),
            agent_id: None,
            user_id: None,
            tags: vec![],
        })
        .unwrap();

        let results = zk.search("fox", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary, "Fox and dog");

        let results = zk.search("blazingly", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_tags_management() {
        let (_, zk) = setup();

        let mem = zk
            .add(&CreateMemory {
                content: "Tag test".into(),
                summary: "Tags".into(),
                source: "user".into(),
                layer: "shared".into(),
                agent_id: None,
                user_id: None,
                tags: vec!["a".into()],
            })
            .unwrap();

        // Add more tags
        zk.add_tags(&mem.id, &["b".into(), "c".into()]).unwrap();
        let fetched = zk.get(&mem.id).unwrap();
        assert_eq!(fetched.tags.len(), 3);

        // Remove a tag
        zk.remove_tags(&mem.id, &["b".into()]).unwrap();
        let fetched = zk.get(&mem.id).unwrap();
        assert_eq!(fetched.tags.len(), 2);
    }

    #[test]
    fn test_wiki_links_parsed() {
        let links = parse_wiki_links("See also [[abc-123]] and [[def-456]].");
        assert_eq!(links, vec!["abc-123", "def-456"]);

        let links = parse_wiki_links("No links here.");
        assert!(links.is_empty());

        let links = parse_wiki_links("Unclosed [[link");
        assert!(links.is_empty());
    }

    #[test]
    fn test_stats() {
        let (_, zk) = setup();

        zk.add(&CreateMemory {
            content: "M1".into(),
            summary: "M1".into(),
            source: "user".into(),
            layer: "shared".into(),
            agent_id: None,
            user_id: None,
            tags: vec!["a".into(), "b".into()],
        })
        .unwrap();

        let stats = zk.get_stats().unwrap();
        assert_eq!(stats.total_memories, 1);
        assert_eq!(stats.unique_tags, 2);
    }
}
