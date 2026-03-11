use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::db::Database;
use crate::error::{Error, Result};
use crate::llm::pricing::PricingTable;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub id: i64,
    pub agent_id: String,
    pub timestamp: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
    pub operation: String,
    pub session_id: Option<String>,
}

/// Tracks LLM usage and costs in the database.
pub struct CostTracker {
    db: Arc<Database>,
    pricing: PricingTable,
}

impl CostTracker {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            pricing: PricingTable::new(),
        }
    }

    pub fn record(
        &self,
        agent_id: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
        operation: &str,
        session_id: Option<&str>,
    ) -> Result<UsageRecord> {
        let cost = self.pricing.calculate(model, input_tokens, output_tokens, 0, 0);

        let id = {
            let conn = self.db.conn();
            conn.execute(
                "INSERT INTO usage_logs (agent_id, model, input_tokens, output_tokens, cost_usd, operation, session_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![agent_id, model, input_tokens, output_tokens, cost, operation, session_id],
            )?;
            conn.last_insert_rowid()
        };

        debug!(agent_id, model, input_tokens, output_tokens, cost, "recorded usage");

        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT * FROM usage_logs WHERE id = ?1")?;
        let record = stmt
            .query_row([id], |row| {
                Ok(UsageRecord {
                    id: row.get("id")?,
                    agent_id: row.get("agent_id")?,
                    timestamp: row.get("timestamp")?,
                    model: row.get("model")?,
                    input_tokens: row.get("input_tokens")?,
                    output_tokens: row.get("output_tokens")?,
                    cost_usd: row.get("cost_usd")?,
                    operation: row.get("operation")?,
                    session_id: row.get("session_id")?,
                })
            })
            .map_err(|e| Error::Database(e.to_string()))?;

        Ok(record)
    }

    pub fn get_usage(
        &self,
        agent_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<UsageRecord>> {
        let conn = self.db.conn();

        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(aid) = agent_id {
            (
                "SELECT * FROM usage_logs WHERE agent_id = ?1 ORDER BY timestamp DESC LIMIT ?2".into(),
                vec![Box::new(aid.to_string()), Box::new(limit)],
            )
        } else {
            (
                "SELECT * FROM usage_logs ORDER BY timestamp DESC LIMIT ?1".into(),
                vec![Box::new(limit)],
            )
        };

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let records = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(UsageRecord {
                    id: row.get("id")?,
                    agent_id: row.get("agent_id")?,
                    timestamp: row.get("timestamp")?,
                    model: row.get("model")?,
                    input_tokens: row.get("input_tokens")?,
                    output_tokens: row.get("output_tokens")?,
                    cost_usd: row.get("cost_usd")?,
                    operation: row.get("operation")?,
                    session_id: row.get("session_id")?,
                })
            })
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(records)
    }

    pub fn pricing(&self) -> &PricingTable {
        &self.pricing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_retrieve() {
        let db = Arc::new(Database::open_memory().unwrap());
        let tracker = CostTracker::new(db);

        let record = tracker
            .record("atlas", "claude-sonnet-4.5", 1000, 500, "chat", None)
            .unwrap();

        assert_eq!(record.agent_id, "atlas");
        assert_eq!(record.model, "claude-sonnet-4.5");
        assert_eq!(record.input_tokens, 1000);
        assert_eq!(record.output_tokens, 500);
        assert!(record.cost_usd > 0.0);

        let usage = tracker.get_usage(Some("atlas"), 10).unwrap();
        assert_eq!(usage.len(), 1);
    }

    #[test]
    fn test_local_model_zero_cost() {
        let db = Arc::new(Database::open_memory().unwrap());
        let tracker = CostTracker::new(db);

        let record = tracker
            .record("atlas", "local", 10000, 5000, "chat", None)
            .unwrap();

        assert!(record.cost_usd.abs() < 1e-10);
    }
}
