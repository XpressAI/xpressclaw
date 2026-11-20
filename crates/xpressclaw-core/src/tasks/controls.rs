//! Explicit task preferences, distinct from observed ACP catalogs and messages.
use crate::{
    db::Database,
    error::{Error, Result},
};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlScope {
    pub agent_id: String,
    pub runner_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPreferences {
    pub scope: ControlScope,
    pub revision: String,
    /// Legacy explicit message options older than this preference cannot win.
    pub after_event_id: i64,
    pub values: HashMap<String, Value>,
    pub dependent_ids: HashSet<String>,
}

pub fn preferences(db: &Arc<Database>, task_id: &str) -> Result<Option<ControlPreferences>> {
    db.with_conn(|conn| {
        let raw: Option<String> = conn
            .query_row(
                "SELECT json_extract(context,'$.acp_preferences') FROM tasks WHERE id=?1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        raw.map(|raw| serde_json::from_str(&raw).map_err(|error| Error::Task(error.to_string())))
            .transpose()
    })
}

/// Save an explicit edit without creating/claiming work. CAS and assignment
/// checks protect another browser's edits and reassignment during an ACP RPC.
pub fn save_preferences(
    db: &Arc<Database>,
    task_id: &str,
    scope: ControlScope,
    expected_revision: Option<&str>,
    values: HashMap<String, Value>,
    dependent_ids: HashSet<String>,
) -> Result<ControlPreferences> {
    db.with_conn(|conn| {
        let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        let (agent, raw): (Option<String>, Option<String>) = tx.query_row(
            "SELECT agent_id,json_extract(context,'$.acp_preferences') FROM tasks WHERE id=?1",[task_id],|row|Ok((row.get(0)?,row.get(1)?)),
        )?;
        let current = raw.map(|raw|serde_json::from_str::<ControlPreferences>(&raw).map_err(|e|Error::Task(e.to_string()))).transpose()?;
        if agent.as_deref()!=Some(scope.agent_id.as_str()) || current.as_ref().map(|p|p.revision.as_str())!=expected_revision {
            return Err(Error::ConfigValidation("Control context changed; refresh and try again".into()));
        }
        let stored = ControlPreferences {
            scope, revision: uuid::Uuid::new_v4().to_string(),
            after_event_id: tx.query_row("SELECT COALESCE(MAX(id),0) FROM session_events",[],|row|row.get(0))?,
            values, dependent_ids,
        };
        let encoded = serde_json::to_string(&stored).map_err(|e|Error::Task(e.to_string()))?;
        tx.execute("UPDATE tasks SET context=json_set(COALESCE(context,'{}'),'$.acp_preferences',json(?1)),updated_at=CURRENT_TIMESTAMP WHERE id=?2",[encoded.as_str(),task_id])?;
        tx.commit()?;
        Ok(stored)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_are_explicit_scoped_cas_updates_without_queued_work() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.conn().execute_batch("INSERT INTO agents(id,name,backend,config) VALUES('a','A','pi','{}'),('b','B','pi','{}'); INSERT INTO tasks(id,title,agent_id,context) VALUES('t','Task','a','{\"session_config\":{\"required\":true}}');").unwrap();
        let scope = ControlScope {
            agent_id: "a".into(),
            runner_kind: "pi".into(),
        };
        let chosen = HashMap::from([("model".into(), Value::String("selected".into()))]);
        let first = save_preferences(
            &db,
            "t",
            scope.clone(),
            None,
            chosen.clone(),
            HashSet::new(),
        )
        .unwrap();
        assert_eq!(preferences(&db, "t").unwrap().unwrap().values, chosen);
        assert!(save_preferences(
            &db,
            "t",
            scope.clone(),
            None,
            HashMap::new(),
            HashSet::new()
        )
        .is_err());
        assert_eq!(
            preferences(&db, "t").unwrap().unwrap().revision,
            first.revision
        );
        let second = save_preferences(
            &db,
            "t",
            scope.clone(),
            Some(&first.revision),
            chosen,
            HashSet::new(),
        )
        .unwrap();
        assert_ne!(second.revision, first.revision);
        db.conn()
            .execute("UPDATE tasks SET agent_id='b' WHERE id='t'", [])
            .unwrap();
        assert!(save_preferences(
            &db,
            "t",
            scope,
            Some(&second.revision),
            HashMap::new(),
            HashSet::new()
        )
        .is_err());
        db.with_conn(|conn| {
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM task_queue", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM task_messages", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert!(conn.query_row(
                "SELECT json_extract(context,'$.session_config.required') FROM tasks WHERE id='t'",
                [],
                |r| r.get::<_, bool>(0)
            )?);
            Ok::<_, Error>(())
        })
        .unwrap();
    }
}
