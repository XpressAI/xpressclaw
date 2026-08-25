//! Project-scoped durable knowledge for native agent harnesses.
//!
//! Notes are explicit, atomic Zettelkasten entries. Typed graph links capture
//! relationships an agent or user has asserted; the vector index is used only
//! to suggest retrieval candidates and never silently creates graph edges.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rusqlite::types::Value;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use zerocopy::IntoBytes;

use crate::db::{task_search_key, Database};
use crate::error::{Error, Result};
use crate::memory::vector::{simple_embedding, EMBEDDING_DIM};
use crate::projects::ensure_project_accepts_work;

const NOTE_TYPES: &[&str] = &[
    "decision",
    "convention",
    "procedure",
    "fact",
    "warning",
    "question",
];
const NOTE_STATES: &[&str] = &["inbox", "evergreen", "archived"];
const NOTE_AUTHORS: &[&str] = &["user", "agent", "upkeep"];
const LINK_TYPES: &[&str] = &[
    "related",
    "supports",
    "contradicts",
    "supersedes",
    "depends_on",
    "example_of",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectMemoryLink {
    pub from_note_id: String,
    pub to_note_id: String,
    pub link_type: String,
    pub strength: f64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectMemoryNote {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub body: String,
    pub summary: String,
    pub note_type: String,
    pub state: String,
    pub source_task_id: Option<String>,
    pub source_attempt_id: Option<String>,
    pub created_by: String,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_accessed_at: String,
    pub access_count: i64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links: Vec<ProjectMemoryLink>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectMemoryNote {
    pub title: String,
    pub body: String,
    pub summary: Option<String>,
    #[serde(default = "default_note_type")]
    pub note_type: String,
    #[serde(default = "default_note_state")]
    pub state: String,
    pub source_task_id: Option<String>,
    pub source_attempt_id: Option<String>,
    #[serde(default = "default_note_author")]
    pub created_by: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateProjectMemoryNote {
    pub title: Option<String>,
    pub body: Option<String>,
    pub summary: Option<String>,
    pub note_type: Option<String>,
    pub state: Option<String>,
    pub pinned: Option<bool>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectMemoryLink {
    pub from_note_id: String,
    pub to_note_id: String,
    #[serde(default = "default_link_type")]
    pub link_type: String,
    #[serde(default = "default_link_strength")]
    pub strength: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectMemorySearchResult {
    pub note: ProjectMemoryNote,
    pub score: f64,
    pub lexical_score: f64,
    pub vector_similarity: Option<f64>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectMemoryCount {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectMemoryRetrievalInfo {
    pub vector_index: &'static str,
    pub embedding_model: &'static str,
    pub embedding_dimensions: usize,
    pub lexical_normalization: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectMemoryReference {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub note_type: String,
    pub state: String,
    pub pinned: bool,
    pub tags: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectMemoryBriefing {
    pub project_id: String,
    pub total_notes: i64,
    pub active_notes: i64,
    pub pinned_notes: i64,
    pub note_types: Vec<ProjectMemoryCount>,
    pub top_tags: Vec<ProjectMemoryCount>,
    pub pinned: Vec<ProjectMemoryReference>,
    pub recent: Vec<ProjectMemoryReference>,
    pub retrieval: ProjectMemoryRetrievalInfo,
    pub hint: String,
}

fn default_note_type() -> String {
    "fact".to_string()
}

fn default_note_state() -> String {
    "evergreen".to_string()
}

fn default_note_author() -> String {
    "agent".to_string()
}

fn default_link_type() -> String {
    "related".to_string()
}

fn default_link_strength() -> f64 {
    1.0
}

pub struct ProjectMemoryStore {
    db: Arc<Database>,
}

impl ProjectMemoryStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn create(
        &self,
        project_id: &str,
        req: &CreateProjectMemoryNote,
    ) -> Result<ProjectMemoryNote> {
        self.ensure_project(project_id)?;
        let title = required_text("title", &req.title, 200)?;
        let body = required_text("body", &req.body, 100_000)?;
        let summary = optional_text(req.summary.as_deref(), &title, 1_000, "summary")?;
        validate_choice("note_type", &req.note_type, NOTE_TYPES)?;
        validate_choice("state", &req.state, NOTE_STATES)?;
        validate_choice("created_by", &req.created_by, NOTE_AUTHORS)?;
        let tags = normalize_tags(&req.tags)?;
        self.validate_provenance(
            project_id,
            req.source_task_id.as_deref(),
            req.source_attempt_id.as_deref(),
        )?;

        let id = uuid::Uuid::new_v4().to_string();
        let searchable = searchable_text(&title, &summary, &body, &tags);
        let search_key = task_search_key(&searchable);
        let embedding = simple_embedding(&search_key);

        self.db.with_conn(|conn| {
            let tx = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_project_accepts_work(&tx, project_id)?;
            tx.execute(
                "INSERT INTO project_memory_notes
                 (id, project_id, title, body, summary, note_type, state,
                  source_task_id, source_attempt_id, created_by, pinned, search_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    id,
                    project_id,
                    title,
                    body,
                    summary,
                    req.note_type,
                    req.state,
                    req.source_task_id,
                    req.source_attempt_id,
                    req.created_by,
                    req.pinned,
                    search_key,
                ],
            )?;
            replace_tags(&tx, &id, &tags)?;
            if req.state != "archived" {
                insert_embedding(&tx, &id, project_id, &embedding)?;
            }
            tx.commit()?;
            Ok::<_, Error>(())
        })?;

        self.get_without_access(project_id, &id)
    }

    pub fn get(&self, project_id: &str, note_id: &str) -> Result<ProjectMemoryNote> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE project_memory_notes
                 SET last_accessed_at = CURRENT_TIMESTAMP, access_count = access_count + 1
                 WHERE id = ?1 AND project_id = ?2",
                rusqlite::params![note_id, project_id],
            )?;
            if changed == 0 {
                return Err(Error::MemoryNotFound {
                    id: note_id.to_string(),
                });
            }
            Ok::<_, Error>(())
        })?;
        self.get_without_access(project_id, note_id)
    }

    pub fn update(
        &self,
        project_id: &str,
        note_id: &str,
        req: &UpdateProjectMemoryNote,
    ) -> Result<ProjectMemoryNote> {
        let existing = self.get_without_access(project_id, note_id)?;
        let title = match req.title.as_deref() {
            Some(value) => required_text("title", value, 200)?,
            None => existing.title,
        };
        let body = match req.body.as_deref() {
            Some(value) => required_text("body", value, 100_000)?,
            None => existing.body,
        };
        let summary = match req.summary.as_deref() {
            Some(value) => required_text("summary", value, 1_000)?,
            None => existing.summary,
        };
        let note_type = req.note_type.as_deref().unwrap_or(&existing.note_type);
        let state = req.state.as_deref().unwrap_or(&existing.state);
        validate_choice("note_type", note_type, NOTE_TYPES)?;
        validate_choice("state", state, NOTE_STATES)?;
        let pinned = req.pinned.unwrap_or(existing.pinned);
        let tags = match req.tags.as_ref() {
            Some(tags) => normalize_tags(tags)?,
            None => existing.tags,
        };
        let searchable = searchable_text(&title, &summary, &body, &tags);
        let search_key = task_search_key(&searchable);
        let embedding = simple_embedding(&search_key);

        self.db.with_conn(|conn| {
            let tx = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_project_accepts_work(&tx, project_id)?;
            tx.execute(
                "UPDATE project_memory_notes
                 SET title = ?1, body = ?2, summary = ?3, note_type = ?4,
                     state = ?5, pinned = ?6, search_key = ?7,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?8 AND project_id = ?9",
                rusqlite::params![
                    title, body, summary, note_type, state, pinned, search_key, note_id,
                    project_id,
                ],
            )?;
            replace_tags(&tx, note_id, &tags)?;
            tx.execute(
                "DELETE FROM project_memory_embeddings WHERE note_id = ?1",
                [note_id],
            )?;
            if state != "archived" {
                insert_embedding(&tx, note_id, project_id, &embedding)?;
            }
            tx.commit()?;
            Ok::<_, Error>(())
        })?;

        self.get_without_access(project_id, note_id)
    }

    pub fn archive(&self, project_id: &str, note_id: &str) -> Result<ProjectMemoryNote> {
        self.update(
            project_id,
            note_id,
            &UpdateProjectMemoryNote {
                state: Some("archived".to_string()),
                ..UpdateProjectMemoryNote::default()
            },
        )
    }

    pub fn link(
        &self,
        project_id: &str,
        req: &CreateProjectMemoryLink,
    ) -> Result<ProjectMemoryLink> {
        validate_choice("link_type", &req.link_type, LINK_TYPES)?;
        if req.from_note_id == req.to_note_id {
            return Err(Error::Memory(
                "a memory note cannot link to itself".to_string(),
            ));
        }
        if !req.strength.is_finite() || !(0.0..=1.0).contains(&req.strength) {
            return Err(Error::Memory(
                "link strength must be between 0 and 1".to_string(),
            ));
        }
        self.get_without_access(project_id, &req.from_note_id)?;
        self.get_without_access(project_id, &req.to_note_id)?;

        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_project_accepts_work(&transaction, project_id)?;
            transaction.execute(
                "INSERT INTO project_memory_links
                 (from_note_id, to_note_id, link_type, strength)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(from_note_id, to_note_id, link_type)
                 DO UPDATE SET strength = excluded.strength",
                rusqlite::params![
                    req.from_note_id,
                    req.to_note_id,
                    req.link_type,
                    req.strength,
                ],
            )?;
            let link = transaction
                .query_row(
                    "SELECT from_note_id, to_note_id, link_type, strength, created_at
                 FROM project_memory_links
                 WHERE from_note_id = ?1 AND to_note_id = ?2 AND link_type = ?3",
                    rusqlite::params![req.from_note_id, req.to_note_id, req.link_type],
                    row_to_link,
                )
                .map_err(Error::from)?;
            transaction.commit()?;
            Ok(link)
        })
    }

    pub fn list(
        &self,
        project_id: &str,
        state: Option<&str>,
        tag: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ProjectMemoryNote>> {
        self.ensure_project(project_id)?;
        if let Some(state) = state {
            validate_choice("state", state, NOTE_STATES)?;
        }
        let tag_key = tag.map(task_search_key);
        let limit = limit.clamp(1, 100);
        let ids = self.db.with_conn(|conn| {
            let mut sql = String::from(
                "SELECT DISTINCT n.id FROM project_memory_notes n
                 LEFT JOIN project_memory_tags t ON t.note_id = n.id
                 WHERE n.project_id = ?1",
            );
            let mut values = vec![Value::Text(project_id.to_string())];
            if let Some(state) = state {
                sql.push_str(" AND n.state = ?");
                values.push(Value::Text(state.to_string()));
            } else {
                sql.push_str(" AND n.state <> 'archived'");
            }
            if let Some(tag_key) = tag_key {
                sql.push_str(" AND t.tag_key = ?");
                values.push(Value::Text(tag_key));
            }
            sql.push_str(" ORDER BY n.pinned DESC, n.updated_at DESC, n.id DESC LIMIT ?");
            values.push(Value::Integer(limit));
            let mut stmt = conn.prepare(&sql)?;
            let ids = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |row| row.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            Ok::<_, Error>(ids)
        })?;
        ids.iter()
            .map(|id| self.get_without_access(project_id, id))
            .collect()
    }

    pub fn search(
        &self,
        project_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ProjectMemorySearchResult>> {
        self.ensure_project(project_id)?;
        let limit = limit.clamp(1, 50);
        let normalized = task_search_key(query.trim());
        if normalized.is_empty() {
            let results = self
                .list(project_id, None, None, limit as i64)?
                .into_iter()
                .map(|note| ProjectMemorySearchResult {
                    score: if note.pinned { 0.05 } else { 0.0 },
                    lexical_score: 0.0,
                    vector_similarity: None,
                    reasons: vec!["recent".to_string()],
                    note,
                })
                .collect();
            return Ok(results);
        }

        let terms: Vec<&str> = normalized.split_whitespace().collect();
        let lexical = self.lexical_candidates(project_id, &normalized, &terms, limit * 8)?;
        let vector = if normalized.chars().count() >= 3 {
            self.vector_candidates(project_id, &normalized, limit * 8)?
        } else {
            HashMap::new()
        };
        let ids: HashSet<String> = lexical.keys().chain(vector.keys()).cloned().collect();
        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            let note = self.get_without_access(project_id, &id)?;
            if note.state == "archived" {
                continue;
            }
            let lexical_score = lexical.get(&id).copied().unwrap_or(0.0);
            let vector_similarity = vector.get(&id).copied();
            let mut score = lexical_score * 0.65 + vector_similarity.unwrap_or(0.0) * 0.30;
            let mut reasons = Vec::new();
            if lexical_score > 0.0 {
                reasons.push("lexical".to_string());
            }
            if vector_similarity.is_some() {
                reasons.push("vector".to_string());
            }
            if note.pinned {
                score += 0.05;
                reasons.push("pinned".to_string());
            }
            results.push(ProjectMemorySearchResult {
                note,
                score: score.clamp(0.0, 1.0),
                lexical_score,
                vector_similarity,
                reasons,
            });
        }
        results.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| right.note.updated_at.cmp(&left.note.updated_at))
        });
        results.truncate(limit);
        Ok(results)
    }

    pub fn briefing(&self, project_id: &str) -> Result<ProjectMemoryBriefing> {
        self.ensure_project(project_id)?;
        let (total_notes, active_notes, pinned_notes) = self.db.with_conn(|conn| {
            let total = conn.query_row(
                "SELECT COUNT(*) FROM project_memory_notes WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )?;
            let active = conn.query_row(
                "SELECT COUNT(*) FROM project_memory_notes
                 WHERE project_id = ?1 AND state <> 'archived'",
                [project_id],
                |row| row.get(0),
            )?;
            let pinned = conn.query_row(
                "SELECT COUNT(*) FROM project_memory_notes
                 WHERE project_id = ?1 AND state <> 'archived' AND pinned = 1",
                [project_id],
                |row| row.get(0),
            )?;
            Ok::<_, Error>((total, active, pinned))
        })?;
        let note_types = self.group_counts(
            project_id,
            "SELECT note_type, COUNT(*) FROM project_memory_notes
             WHERE project_id = ?1 AND state <> 'archived'
             GROUP BY note_type ORDER BY COUNT(*) DESC, note_type",
        )?;
        let top_tags = self.group_counts(
            project_id,
            "SELECT t.tag, COUNT(*) FROM project_memory_tags t
             JOIN project_memory_notes n ON n.id = t.note_id
             WHERE n.project_id = ?1 AND n.state <> 'archived'
             GROUP BY t.tag_key ORDER BY COUNT(*) DESC, t.tag LIMIT 12",
        )?;
        let pinned = self
            .list_pinned(project_id, 8)?
            .into_iter()
            .map(ProjectMemoryReference::from)
            .collect();
        let recent = self
            .list(project_id, None, None, 8)?
            .into_iter()
            .map(ProjectMemoryReference::from)
            .collect();
        let hint = if active_notes == 0 {
            "No active project memories yet. Capture durable decisions, conventions, procedures, facts, or warnings when they will help a later task.".to_string()
        } else {
            format!(
                "This project has {active_notes} active memory notes ({pinned_notes} pinned). Read the briefing or search memory before making project-wide decisions; write back only durable, reusable knowledge."
            )
        };
        Ok(ProjectMemoryBriefing {
            project_id: project_id.to_string(),
            total_notes,
            active_notes,
            pinned_notes,
            note_types,
            top_tags,
            pinned,
            recent,
            retrieval: ProjectMemoryRetrievalInfo {
                vector_index: "sqlite-vec vec0 (project partition key)",
                embedding_model: "character-trigram-hash-v1",
                embedding_dimensions: EMBEDDING_DIM,
                lexical_normalization: "NFKC plus full Unicode case folding",
            },
            hint,
        })
    }

    fn get_without_access(&self, project_id: &str, note_id: &str) -> Result<ProjectMemoryNote> {
        let note = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, project_id, title, body, summary, note_type, state,
                        source_task_id, source_attempt_id, created_by, pinned,
                        created_at, updated_at, last_accessed_at, access_count
                 FROM project_memory_notes WHERE id = ?1 AND project_id = ?2",
                rusqlite::params![note_id, project_id],
                row_to_note,
            )
            .optional()
            .map_err(Error::from)
        })?;
        let Some(mut note) = note else {
            return Err(Error::MemoryNotFound {
                id: note_id.to_string(),
            });
        };
        note.tags = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT tag FROM project_memory_tags WHERE note_id = ?1 ORDER BY tag_key",
            )?;
            let tags = stmt
                .query_map([note_id], |row| row.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            Ok::<_, Error>(tags)
        })?;
        note.links = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT from_note_id, to_note_id, link_type, strength, created_at
                 FROM project_memory_links
                 WHERE from_note_id = ?1 OR to_note_id = ?1
                 ORDER BY created_at, from_note_id, to_note_id",
            )?;
            let links = stmt
                .query_map([note_id], row_to_link)?
                .collect::<std::result::Result<Vec<ProjectMemoryLink>, _>>()?;
            Ok::<_, Error>(links)
        })?;
        Ok(note)
    }

    fn ensure_project(&self, project_id: &str) -> Result<()> {
        let exists = self.db.with_conn(|conn| {
            conn.query_row("SELECT 1 FROM projects WHERE id = ?1", [project_id], |_| {
                Ok(())
            })
            .optional()
            .map_err(Error::from)
        })?;
        if exists.is_none() {
            return Err(Error::Memory(format!("project not found: {project_id}")));
        }
        Ok(())
    }

    fn validate_provenance(
        &self,
        project_id: &str,
        task_id: Option<&str>,
        attempt_id: Option<&str>,
    ) -> Result<()> {
        if let Some(task_id) = task_id {
            let belongs = self.db.with_conn(|conn| {
                conn.query_row(
                    "SELECT 1 FROM tasks
                     WHERE id = ?1 AND (
                         project_id = ?2
                         OR (project_id IS NULL AND (session_id = ?2 OR agent_id = ?2))
                     )",
                    rusqlite::params![task_id, project_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(Error::from)
            })?;
            if belongs.is_none() {
                return Err(Error::Memory(format!(
                    "source task {task_id} does not belong to project {project_id}"
                )));
            }
        }
        if let Some(attempt_id) = attempt_id {
            let belongs = self.db.with_conn(|conn| {
                conn.query_row(
                    "SELECT 1
                     FROM work_attempts wa
                     LEFT JOIN tasks t ON t.id = wa.task_id
                     LEFT JOIN logical_sessions ls ON ls.id = wa.session_id
                     LEFT JOIN agents a ON a.id = ls.agent_id
                     WHERE wa.id = ?1
                       AND COALESCE(t.project_id, a.project_id, wa.session_id) = ?2",
                    rusqlite::params![attempt_id, project_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(Error::from)
            })?;
            if belongs.is_none() {
                return Err(Error::Memory(format!(
                    "source attempt {attempt_id} does not belong to project {project_id}"
                )));
            }
        }
        Ok(())
    }

    fn lexical_candidates(
        &self,
        project_id: &str,
        normalized_query: &str,
        terms: &[&str],
        limit: usize,
    ) -> Result<HashMap<String, f64>> {
        self.db.with_conn(|conn| {
            let mut sql = String::from(
                "SELECT id, search_key FROM project_memory_notes
                 WHERE project_id = ? AND state <> 'archived'",
            );
            let mut values = vec![Value::Text(project_id.to_string())];
            for term in terms {
                sql.push_str(" AND search_key LIKE ? ESCAPE '\\'");
                values.push(Value::Text(like_pattern(term)));
            }
            sql.push_str(" ORDER BY pinned DESC, updated_at DESC LIMIT ?");
            values.push(Value::Integer(limit as i64));
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut scores = HashMap::new();
            for row in rows {
                let (id, search_key) = row?;
                let phrase = search_key.contains(normalized_query);
                let matched = terms
                    .iter()
                    .filter(|term| search_key.contains(**term))
                    .count();
                let coverage = matched as f64 / terms.len().max(1) as f64;
                scores.insert(id, if phrase { 1.0 } else { 0.8 + coverage * 0.2 });
            }
            Ok(scores)
        })
    }

    fn vector_candidates(
        &self,
        project_id: &str,
        normalized_query: &str,
        limit: usize,
    ) -> Result<HashMap<String, f64>> {
        let embedding = simple_embedding(normalized_query);
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT note_id, distance FROM project_memory_embeddings
                 WHERE embedding MATCH ?1 AND k = ?2 AND project_id = ?3",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![embedding.as_bytes(), limit as i64, project_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )?;
            let mut scores = HashMap::new();
            for row in rows {
                let (id, distance) = row?;
                let similarity = (1.0 - distance).clamp(0.0, 1.0);
                if similarity >= 0.2 {
                    scores.insert(id, similarity);
                }
            }
            Ok(scores)
        })
    }

    fn list_pinned(&self, project_id: &str, limit: i64) -> Result<Vec<ProjectMemoryNote>> {
        let ids = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id FROM project_memory_notes
                 WHERE project_id = ?1 AND state <> 'archived' AND pinned = 1
                 ORDER BY updated_at DESC, id DESC LIMIT ?2",
            )?;
            let ids = stmt
                .query_map(rusqlite::params![project_id, limit], |row| row.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            Ok::<_, Error>(ids)
        })?;
        ids.iter()
            .map(|id| self.get_without_access(project_id, id))
            .collect()
    }

    fn group_counts(&self, project_id: &str, sql: &str) -> Result<Vec<ProjectMemoryCount>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(sql)?;
            let counts = stmt
                .query_map([project_id], |row| {
                    Ok(ProjectMemoryCount {
                        name: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(counts)
        })
    }
}

/// Transfer the durable memory partition while consolidating an otherwise
/// empty imported Project into another Project.
pub(crate) fn move_project_memory(
    transaction: &rusqlite::Transaction<'_>,
    source_project_id: &str,
    target_project_id: &str,
) -> Result<()> {
    let notes = {
        let mut statement = transaction.prepare(
            "SELECT id, search_key, state FROM project_memory_notes WHERE project_id = ?1",
        )?;
        let rows = statement
            .query_map([source_project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for (note_id, _, _) in &notes {
        transaction.execute(
            "DELETE FROM project_memory_embeddings WHERE note_id = ?1",
            [note_id],
        )?;
    }
    transaction.execute(
        "UPDATE project_memory_notes SET project_id = ?1 WHERE project_id = ?2",
        rusqlite::params![target_project_id, source_project_id],
    )?;
    for (note_id, search_key, state) in notes {
        if state != "archived" {
            insert_embedding(
                transaction,
                &note_id,
                target_project_id,
                &simple_embedding(&search_key),
            )?;
        }
    }
    Ok(())
}

impl From<ProjectMemoryNote> for ProjectMemoryReference {
    fn from(note: ProjectMemoryNote) -> Self {
        Self {
            id: note.id,
            title: note.title,
            summary: note.summary,
            note_type: note.note_type,
            state: note.state,
            pinned: note.pinned,
            tags: note.tags,
            updated_at: note.updated_at,
        }
    }
}

fn row_to_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectMemoryNote> {
    Ok(ProjectMemoryNote {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        summary: row.get(4)?,
        note_type: row.get(5)?,
        state: row.get(6)?,
        source_task_id: row.get(7)?,
        source_attempt_id: row.get(8)?,
        created_by: row.get(9)?,
        pinned: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        last_accessed_at: row.get(13)?,
        access_count: row.get(14)?,
        tags: Vec::new(),
        links: Vec::new(),
    })
}

fn row_to_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectMemoryLink> {
    Ok(ProjectMemoryLink {
        from_note_id: row.get(0)?,
        to_note_id: row.get(1)?,
        link_type: row.get(2)?,
        strength: row.get(3)?,
        created_at: row.get(4)?,
    })
}

fn replace_tags(tx: &rusqlite::Transaction<'_>, note_id: &str, tags: &[String]) -> Result<()> {
    tx.execute(
        "DELETE FROM project_memory_tags WHERE note_id = ?1",
        [note_id],
    )?;
    for tag in tags {
        tx.execute(
            "INSERT INTO project_memory_tags (note_id, tag, tag_key) VALUES (?1, ?2, ?3)",
            rusqlite::params![note_id, tag, task_search_key(tag)],
        )?;
    }
    Ok(())
}

fn insert_embedding(
    tx: &rusqlite::Transaction<'_>,
    note_id: &str,
    project_id: &str,
    embedding: &[f32],
) -> Result<()> {
    tx.execute(
        "INSERT INTO project_memory_embeddings (note_id, embedding, project_id)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![note_id, embedding.as_bytes(), project_id],
    )?;
    Ok(())
}

fn searchable_text(title: &str, summary: &str, body: &str, tags: &[String]) -> String {
    format!("{title}\n{summary}\n{body}\n{}", tags.join(" "))
}

fn required_text(field: &str, value: &str, max_chars: usize) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::Memory(format!("{field} must not be empty")));
    }
    if value.chars().count() > max_chars {
        return Err(Error::Memory(format!(
            "{field} must be {max_chars} characters or fewer"
        )));
    }
    Ok(value.to_string())
}

fn optional_text(
    value: Option<&str>,
    fallback: &str,
    max_chars: usize,
    field: &str,
) -> Result<String> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => required_text(field, value, max_chars),
        None => Ok(fallback.to_string()),
    }
}

fn validate_choice(field: &str, value: &str, allowed: &[&str]) -> Result<()> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(Error::Memory(format!(
            "invalid {field} {value:?}; expected one of {}",
            allowed.join(", ")
        )))
    }
}

fn normalize_tags(tags: &[String]) -> Result<Vec<String>> {
    if tags.len() > 32 {
        return Err(Error::Memory(
            "a memory note may have at most 32 tags".to_string(),
        ));
    }
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for tag in tags {
        let tag = required_text("tag", tag, 64)?;
        if seen.insert(task_search_key(&tag)) {
            normalized.push(tag);
        }
    }
    Ok(normalized)
}

fn like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('%');
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('%');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, ProjectMemoryStore) {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('alpha', 'Alpha');
                 INSERT INTO projects (id, name) VALUES ('beta', 'Beta');",
            )
        })
        .unwrap();
        let store = ProjectMemoryStore::new(db.clone());
        (db, store)
    }

    fn create_note(
        store: &ProjectMemoryStore,
        project_id: &str,
        title: &str,
        body: &str,
    ) -> ProjectMemoryNote {
        store
            .create(
                project_id,
                &CreateProjectMemoryNote {
                    title: title.to_string(),
                    body: body.to_string(),
                    summary: None,
                    note_type: "fact".to_string(),
                    state: "evergreen".to_string(),
                    source_task_id: None,
                    source_attempt_id: None,
                    created_by: "agent".to_string(),
                    pinned: false,
                    tags: Vec::new(),
                },
            )
            .unwrap()
    }

    #[test]
    fn project_notes_are_isolated_in_reads_and_vector_search() {
        let (db, store) = setup();
        let alpha = create_note(&store, "alpha", "Deployment", "Use blue green deployment");
        let beta = create_note(&store, "beta", "Deployment", "Use blue green deployment");

        assert!(matches!(
            store.get("alpha", &beta.id),
            Err(Error::MemoryNotFound { .. })
        ));
        let results = store.search("alpha", "blue green deployment", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].note.id, alpha.id);

        db.with_conn(|conn| {
            let projects: Vec<String> = conn
                .prepare(
                    "SELECT project_id FROM project_memory_embeddings
                     WHERE embedding MATCH ?1 AND k = 10 AND project_id = ?2",
                )
                .unwrap()
                .query_map(
                    rusqlite::params![
                        simple_embedding("blue green deployment").as_bytes(),
                        "alpha"
                    ],
                    |row| row.get(0),
                )
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert_eq!(projects, vec!["alpha"]);
        });
    }

    #[test]
    fn unicode_search_handles_japanese_width_and_case_folding() {
        let (_db, store) = setup();
        let japanese = create_note(
            &store,
            "alpha",
            "デプロイ手順",
            "本番環境ではカタカナの設定を確認する",
        );
        let width = store.search("alpha", "ｶﾀｶﾅ", 10).unwrap();
        assert_eq!(width[0].note.id, japanese.id);

        let german = create_note(&store, "alpha", "Straße", "Release convention");
        let folded = store.search("alpha", "STRASSE", 10).unwrap();
        assert_eq!(folded[0].note.id, german.id);
    }

    #[test]
    fn typed_links_reject_cross_project_targets() {
        let (_db, store) = setup();
        let first = create_note(&store, "alpha", "Decision", "Use SQLite");
        let second = create_note(&store, "alpha", "Reason", "Offline first");
        let foreign = create_note(&store, "beta", "Other", "Other project");

        let link = store
            .link(
                "alpha",
                &CreateProjectMemoryLink {
                    from_note_id: first.id.clone(),
                    to_note_id: second.id.clone(),
                    link_type: "supports".to_string(),
                    strength: 0.9,
                },
            )
            .unwrap();
        assert_eq!(link.link_type, "supports");
        assert!(store
            .link(
                "alpha",
                &CreateProjectMemoryLink {
                    from_note_id: first.id,
                    to_note_id: foreign.id,
                    link_type: "related".to_string(),
                    strength: 1.0,
                },
            )
            .is_err());
    }

    #[test]
    fn update_reindexes_and_archive_removes_vector() {
        let (db, store) = setup();
        let note = create_note(&store, "alpha", "Old convention", "Use yarn");
        let updated = store
            .update(
                "alpha",
                &note.id,
                &UpdateProjectMemoryNote {
                    title: Some("Current convention".to_string()),
                    body: Some("Use pnpm".to_string()),
                    ..UpdateProjectMemoryNote::default()
                },
            )
            .unwrap();
        assert_eq!(updated.body, "Use pnpm");
        assert_eq!(store.search("alpha", "pnpm", 10).unwrap().len(), 1);

        store.archive("alpha", &note.id).unwrap();
        assert!(store.search("alpha", "pnpm", 10).unwrap().is_empty());
        db.with_conn(|conn| {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM project_memory_embeddings WHERE note_id = ?1",
                    [&note.id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0);
        });
    }

    #[test]
    fn unrelated_zero_similarity_notes_are_not_returned() {
        let (_db, store) = setup();
        create_note(&store, "alpha", "Database", "SQLite migration strategy");

        assert!(store
            .search("alpha", "watermelon orchestra", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn deleting_a_project_cleans_up_partitioned_embeddings() {
        let (db, store) = setup();
        create_note(&store, "alpha", "Database", "SQLite migration strategy");
        db.with_conn(|conn| conn.execute("DELETE FROM projects WHERE id = 'alpha'", []))
            .unwrap();

        db.with_conn(|conn| {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM project_memory_embeddings WHERE project_id = ?1",
                    ["alpha"],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0);
        });
    }
}
