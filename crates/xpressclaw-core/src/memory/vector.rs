use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;
use zerocopy::IntoBytes;

use crate::db::Database;
use crate::error::{Error, Result};

/// Dimensionality of the embedding vectors (all-MiniLM-L6-v2).
pub const EMBEDDING_DIM: usize = 384;

/// A memory with its embedding vector.
#[derive(Debug, Clone)]
pub struct EmbeddedMemory {
    pub memory_id: String,
    pub embedding: Vec<f32>,
}

/// A search result from the vector store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub memory_id: String,
    pub distance: f64,
    /// Similarity score (1 - distance), clamped to [0, 1].
    pub similarity: f64,
}

/// Vector store for semantic memory search using sqlite-vec.
///
/// Uses the vec0 virtual table with cosine distance for efficient KNN search.
/// The vec0 table is created by migration V9 in db.rs.
///
/// Embeddings can come from:
/// - `simple_embedding()` — trigram-hash (default, no external deps)
/// - Bundled ONNX model (all-MiniLM-L6-v2) via `ort` crate
/// - External embedding API
/// - Pre-computed vectors
pub struct VectorStore {
    db: Arc<Database>,
}

impl VectorStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Store an embedding for a memory.
    pub fn add(&self, memory_id: &str, embedding: &[f32]) -> Result<()> {
        if embedding.len() != EMBEDDING_DIM {
            return Err(Error::Embedding(format!(
                "expected {EMBEDDING_DIM} dimensions, got {}",
                embedding.len()
            )));
        }

        let bytes = embedding.as_bytes();

        self.db.with_conn(|conn| {
            // Delete existing entry first (vec0 doesn't support OR REPLACE)
            conn.execute(
                "DELETE FROM memory_embeddings WHERE memory_id = ?1",
                [memory_id],
            )
            .ok();

            conn.execute(
                "INSERT INTO memory_embeddings (memory_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![memory_id, bytes],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        debug!(
            memory_id,
            dim = embedding.len(),
            "stored embedding (sqlite-vec)"
        );
        Ok(())
    }

    /// Store embeddings for multiple memories.
    pub fn add_batch(&self, items: &[EmbeddedMemory]) -> Result<()> {
        for item in items {
            self.add(&item.memory_id, &item.embedding)?;
        }
        Ok(())
    }

    /// Remove an embedding.
    pub fn remove(&self, memory_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM memory_embeddings WHERE memory_id = ?1",
                [memory_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;
        Ok(())
    }

    /// Search for similar memories using KNN via sqlite-vec's vec0 MATCH.
    pub fn search(&self, query_embedding: &[f32], limit: usize) -> Result<Vec<VectorSearchResult>> {
        if query_embedding.len() != EMBEDDING_DIM {
            return Err(Error::Embedding(format!(
                "expected {EMBEDDING_DIM} dimensions, got {}",
                query_embedding.len()
            )));
        }

        let bytes = query_embedding.as_bytes();

        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT memory_id, distance
                     FROM memory_embeddings
                     WHERE embedding MATCH ?1
                     AND k = ?2",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let results = stmt
                .query_map(rusqlite::params![bytes, limit as i64], |row| {
                    let memory_id: String = row.get(0)?;
                    let distance: f64 = row.get(1)?;
                    Ok(VectorSearchResult {
                        memory_id,
                        distance,
                        similarity: (1.0 - distance).clamp(0.0, 1.0),
                    })
                })
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(results)
        })
    }

    /// Find memories similar to a given memory.
    pub fn find_similar(&self, memory_id: &str, limit: usize) -> Result<Vec<VectorSearchResult>> {
        let embedding = self.get_embedding(memory_id)?;
        let mut results = self.search(&embedding, limit + 1)?;
        // Remove the query memory itself from results
        results.retain(|r| r.memory_id != memory_id);
        results.truncate(limit);
        Ok(results)
    }

    /// Get the embedding for a memory.
    pub fn get_embedding(&self, memory_id: &str) -> Result<Vec<f32>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT embedding FROM memory_embeddings WHERE memory_id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            let blob: Vec<u8> = stmt
                .query_row([memory_id], |row| row.get(0))
                .map_err(|_| Error::Embedding(format!("no embedding for memory {memory_id}")))?;

            Ok(bytes_to_f32_vec(&blob))
        })
    }

    /// Check if a memory has an embedding.
    pub fn has_embedding(&self, memory_id: &str) -> bool {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT 1 FROM memory_embeddings WHERE memory_id = ?1",
                [memory_id],
                |_| Ok(()),
            )
            .is_ok()
        })
    }

    /// Get statistics about the vector store.
    pub fn get_stats(&self) -> Result<VectorStats> {
        self.db.with_conn(|conn| {
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| {
                    row.get(0)
                })
                .unwrap_or(0);

            Ok(VectorStats {
                embedding_count: count,
                dimension: EMBEDDING_DIM as u32,
                model: "all-MiniLM-L6-v2".to_string(),
            })
        })
    }
}

/// Vector store statistics.
#[derive(Debug, Clone, Serialize)]
pub struct VectorStats {
    pub embedding_count: i64,
    pub dimension: u32,
    pub model: String,
}

/// Convert raw bytes back to f32 vector.
fn bytes_to_f32_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Generate a trigram-hash embedding for text.
///
/// Produces a deterministic 384-dimensional vector by hashing character
/// trigrams into buckets and L2-normalizing. This captures surface-level
/// textual similarity (shared substrings) rather than deep semantics.
///
/// For production-quality semantic search, replace with the bundled
/// all-MiniLM-L6-v2 ONNX model via the `ort` crate.
pub fn simple_embedding(text: &str) -> Vec<f32> {
    let mut vec = vec![0.0f32; EMBEDDING_DIM];
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();

    // Hash character trigrams into buckets
    for window in chars.windows(3) {
        let hash = window
            .iter()
            .fold(0u64, |acc, c| acc.wrapping_mul(31).wrapping_add(*c as u64));
        let idx = (hash % EMBEDDING_DIM as u64) as usize;
        vec[idx] += 1.0;
    }

    // L2 normalize
    let norm = vec_norm(&vec);
    if norm > 0.0 {
        for v in &mut vec {
            *v /= norm;
        }
    }

    vec
}

fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn vec_norm(v: &[f32]) -> f32 {
    dot_product(v, v).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, VectorStore) {
        let db = Arc::new(Database::open_memory().unwrap());
        let store = VectorStore::new(db.clone());
        (db, store)
    }

    /// Insert a fake memory into the memories table.
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
    fn test_sqlite_vec_loaded() {
        let (db, _store) = setup();
        db.with_conn(|conn| {
            let version: String = conn
                .query_row("SELECT vec_version()", [], |row| row.get(0))
                .unwrap();
            assert!(version.starts_with("v0."));
        });
    }

    #[test]
    fn test_add_and_get_embedding() {
        let (db, store) = setup();
        insert_memory(&db, "mem-1");

        let emb = simple_embedding("Hello, world!");
        store.add("mem-1", &emb).unwrap();

        assert!(store.has_embedding("mem-1"));
        assert!(!store.has_embedding("mem-2"));

        let retrieved = store.get_embedding("mem-1").unwrap();
        assert_eq!(retrieved.len(), EMBEDDING_DIM);
        assert!((retrieved[0] - emb[0]).abs() < 1e-6);
    }

    #[test]
    fn test_wrong_dimension_rejected() {
        let (db, store) = setup();
        insert_memory(&db, "mem-1");

        let bad_emb = vec![0.0f32; 100];
        let result = store.add("mem-1", &bad_emb);
        assert!(result.is_err());
    }

    #[test]
    fn test_search() {
        let (db, store) = setup();

        insert_memory(&db, "mem-rust");
        insert_memory(&db, "mem-python");
        insert_memory(&db, "mem-cooking");

        store
            .add(
                "mem-rust",
                &simple_embedding("Rust programming language systems"),
            )
            .unwrap();
        store
            .add(
                "mem-python",
                &simple_embedding("Python programming language scripting"),
            )
            .unwrap();
        store
            .add(
                "mem-cooking",
                &simple_embedding("Italian cooking pasta recipe"),
            )
            .unwrap();

        let query = simple_embedding("programming language");
        let results = store.search(&query, 3).unwrap();

        assert_eq!(results.len(), 3);
        // Programming memories should be more similar to the query
        assert!(results[0].similarity > results[2].similarity);
    }

    #[test]
    fn test_find_similar() {
        let (db, store) = setup();

        insert_memory(&db, "mem-a");
        insert_memory(&db, "mem-b");
        insert_memory(&db, "mem-c");

        store
            .add("mem-a", &simple_embedding("cats and dogs"))
            .unwrap();
        store
            .add("mem-b", &simple_embedding("cats and kittens"))
            .unwrap();
        store
            .add("mem-c", &simple_embedding("quantum physics"))
            .unwrap();

        let similar = store.find_similar("mem-a", 2).unwrap();
        assert_eq!(similar.len(), 2);
        // mem-a should not be in its own results
        assert!(similar.iter().all(|r| r.memory_id != "mem-a"));
    }

    #[test]
    fn test_remove() {
        let (db, store) = setup();
        insert_memory(&db, "mem-1");

        store.add("mem-1", &simple_embedding("test")).unwrap();
        assert!(store.has_embedding("mem-1"));

        store.remove("mem-1").unwrap();
        assert!(!store.has_embedding("mem-1"));
    }

    #[test]
    fn test_stats() {
        let (db, store) = setup();

        insert_memory(&db, "mem-1");
        insert_memory(&db, "mem-2");

        store.add("mem-1", &simple_embedding("one")).unwrap();
        store.add("mem-2", &simple_embedding("two")).unwrap();

        let stats = store.get_stats().unwrap();
        assert_eq!(stats.embedding_count, 2);
        assert_eq!(stats.dimension, EMBEDDING_DIM as u32);
    }

    #[test]
    fn test_simple_embedding_deterministic() {
        let e1 = simple_embedding("Hello world");
        let e2 = simple_embedding("Hello world");
        assert_eq!(e1, e2);
    }

    #[test]
    fn test_simple_embedding_different_texts() {
        let e1 = simple_embedding("cats and dogs");
        let e2 = simple_embedding("quantum physics");
        assert_ne!(e1, e2);
    }

    #[test]
    fn test_simple_embedding_normalized() {
        let emb = simple_embedding("Some text for embedding");
        let norm = vec_norm(&emb);
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_upsert_replaces_embedding() {
        let (db, store) = setup();
        insert_memory(&db, "mem-1");

        let emb1 = simple_embedding("first version");
        store.add("mem-1", &emb1).unwrap();

        let emb2 = simple_embedding("completely different text");
        store.add("mem-1", &emb2).unwrap();

        let retrieved = store.get_embedding("mem-1").unwrap();
        assert!((retrieved[0] - emb2[0]).abs() < 1e-6);
        assert_eq!(store.get_stats().unwrap().embedding_count, 1);
    }
}
