use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};

use rusqlite::Connection;
use tracing::info;

use crate::error::{Error, Result};

/// Register sqlite-vec as an auto-extension. Must be called before opening connections.
static INIT_SQLITE_VEC: Once = Once::new();

fn ensure_sqlite_vec() {
    INIT_SQLITE_VEC.call_once(|| unsafe {
        type ExtFn = unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut std::os::raw::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> std::os::raw::c_int;
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<*const (), ExtFn>(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// Database manager for xpressclaw.
///
/// Uses SQLite with WAL mode for concurrent reads.
/// sqlite-vec is loaded as an extension when available.
pub struct Database {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open (or create) the database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        ensure_sqlite_vec();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Database(format!("failed to create data dir: {e}")))?;
        }

        let conn = Connection::open(path)?;

        // Performance pragmas
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -64000;
             PRAGMA temp_store = MEMORY;",
        )?;

        let db = Self {
            path: path.to_path_buf(),
            conn: Arc::new(Mutex::new(conn)),
        };

        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory database (for testing).
    pub fn open_memory() -> Result<Self> {
        ensure_sqlite_vec();

        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        let db = Self {
            path: PathBuf::from(":memory:"),
            conn: Arc::new(Mutex::new(conn)),
        };

        db.migrate()?;
        Ok(db)
    }

    /// Get a reference to the connection (locked).
    ///
    /// **Warning:** The returned `MutexGuard` holds the lock for its entire lifetime.
    /// Prefer [`with_conn`] to avoid accidental deadlocks when calling other methods
    /// that also need the connection.
    pub fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("database mutex poisoned")
    }

    /// Execute a closure with the connection, ensuring the lock is released afterward.
    ///
    /// This prevents the common deadlock pattern where a method holds `conn()` and
    /// then calls another method that also calls `conn()`.
    pub fn with_conn<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&Connection) -> T,
    {
        let conn = self.conn.lock().expect("database mutex poisoned");
        f(&conn)
    }

    /// Run all pending migrations.
    fn migrate(&self) -> Result<()> {
        let conn = self.conn();

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )",
        )?;

        let version: u32 = conn
            .query_row(
                "SELECT value FROM config WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let migrations: &[(u32, &str)] = &[
            (1, MIGRATION_V1),
            (2, MIGRATION_V2),
            (3, MIGRATION_V3),
            (4, MIGRATION_V4),
            (5, MIGRATION_V5),
            (6, MIGRATION_V6),
            (7, MIGRATION_V7),
            (8, MIGRATION_V8),
            (9, MIGRATION_V9),
            (10, MIGRATION_V10),
            (11, MIGRATION_V11),
            (12, MIGRATION_V12),
            (13, MIGRATION_V13),
            (14, MIGRATION_V14),
            (15, MIGRATION_V15),
            (16, MIGRATION_V16),
            (17, MIGRATION_V17),
            (18, MIGRATION_V18),
        ];

        for &(target, sql) in migrations {
            if version < target {
                conn.execute_batch(sql).map_err(|e| Error::Migration {
                    version: target,
                    message: e.to_string(),
                })?;

                conn.execute(
                    "INSERT OR REPLACE INTO config (key, value) VALUES ('schema_version', ?1)",
                    [target.to_string()],
                )?;

                info!("applied migration v{target}");
            }
        }

        Ok(())
    }

    /// Path to the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// -- Migrations --

const MIGRATION_V1: &str = "
-- Memories (Zettelkasten notes)
CREATE TABLE IF NOT EXISTS memories (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    summary TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    accessed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    access_count INTEGER DEFAULT 0,
    source TEXT NOT NULL,
    layer TEXT NOT NULL DEFAULT 'shared',
    agent_id TEXT,
    user_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_memories_layer ON memories(layer);
CREATE INDEX IF NOT EXISTS idx_memories_accessed ON memories(accessed_at);
CREATE INDEX IF NOT EXISTS idx_memories_agent ON memories(agent_id);

-- Memory links (Zettelkasten bidirectional links)
CREATE TABLE IF NOT EXISTS memory_links (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    link_type TEXT DEFAULT 'related',
    strength REAL DEFAULT 1.0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (from_id, to_id),
    FOREIGN KEY (from_id) REFERENCES memories(id) ON DELETE CASCADE,
    FOREIGN KEY (to_id) REFERENCES memories(id) ON DELETE CASCADE
);

-- Memory tags
CREATE TABLE IF NOT EXISTS memory_tags (
    memory_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY (memory_id, tag),
    FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_memory_tags_tag ON memory_tags(tag);

-- Memory slots (near-term memory)
CREATE TABLE IF NOT EXISTS memory_slots (
    agent_id TEXT NOT NULL,
    slot_index INTEGER NOT NULL,
    memory_id TEXT,
    relevance_score REAL,
    loaded_at TIMESTAMP,
    PRIMARY KEY (agent_id, slot_index),
    FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE SET NULL
);

-- Tasks
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    priority INTEGER DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP,
    agent_id TEXT,
    parent_task_id TEXT,
    sop_id TEXT,
    context TEXT,
    FOREIGN KEY (parent_task_id) REFERENCES tasks(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_agent ON tasks(agent_id);
CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_task_id);

-- SOPs (Standard Operating Procedures)
CREATE TABLE IF NOT EXISTS sops (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    content TEXT NOT NULL,
    triggers TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    created_by TEXT,
    version INTEGER DEFAULT 1
);

-- Agents
CREATE TABLE IF NOT EXISTS agents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    backend TEXT NOT NULL,
    config TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'stopped',
    container_id TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    started_at TIMESTAMP,
    stopped_at TIMESTAMP,
    error_message TEXT
);
CREATE INDEX IF NOT EXISTS idx_agents_status ON agents(status);

-- Usage logs
CREATE TABLE IF NOT EXISTS usage_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id TEXT NOT NULL,
    timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_usd REAL NOT NULL,
    operation TEXT,
    session_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_usage_agent ON usage_logs(agent_id);
CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON usage_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_usage_session ON usage_logs(session_id);

-- Budget state
CREATE TABLE IF NOT EXISTS budget_state (
    agent_id TEXT PRIMARY KEY,
    daily_spent REAL DEFAULT 0.0,
    daily_reset_at TIMESTAMP,
    monthly_spent REAL DEFAULT 0.0,
    monthly_reset_at TIMESTAMP,
    total_spent REAL DEFAULT 0.0,
    is_paused INTEGER DEFAULT 0,
    pause_reason TEXT
);

-- Agent sessions
CREATE TABLE IF NOT EXISTS agent_sessions (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    started_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    ended_at TIMESTAMP,
    messages TEXT,
    total_tokens INTEGER DEFAULT 0,
    total_cost REAL DEFAULT 0.0,
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_sessions_agent ON agent_sessions(agent_id);
";

const MIGRATION_V2: &str = "
-- Activity logs for observability
CREATE TABLE IF NOT EXISTS activity_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    agent_id TEXT,
    event_type TEXT NOT NULL,
    event_data TEXT,
    session_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_activity_agent ON activity_logs(agent_id);
CREATE INDEX IF NOT EXISTS idx_activity_timestamp ON activity_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_activity_type ON activity_logs(event_type);

-- Tool execution logs
CREATE TABLE IF NOT EXISTS tool_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    agent_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    input_data TEXT,
    output_data TEXT,
    duration_ms INTEGER,
    success INTEGER DEFAULT 1,
    error_message TEXT
);
CREATE INDEX IF NOT EXISTS idx_tool_agent ON tool_logs(agent_id);
CREATE INDEX IF NOT EXISTS idx_tool_name ON tool_logs(tool_name);
";

const MIGRATION_V3: &str = "
-- Scheduled tasks
CREATE TABLE IF NOT EXISTS schedules (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    cron TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    enabled INTEGER DEFAULT 1,
    last_run TIMESTAMP,
    run_count INTEGER DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_schedules_agent ON schedules(agent_id);
CREATE INDEX IF NOT EXISTS idx_schedules_enabled ON schedules(enabled);
";

const MIGRATION_V4: &str = "
-- Task messages for conversation threads
CREATE TABLE IF NOT EXISTS task_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_task_messages_task ON task_messages(task_id);
CREATE INDEX IF NOT EXISTS idx_task_messages_timestamp ON task_messages(timestamp);
";

const MIGRATION_V5: &str = "
-- Agent chat messages for direct conversations
CREATE TABLE IF NOT EXISTS agent_chat_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_agent_chat_agent ON agent_chat_messages(agent_id);
CREATE INDEX IF NOT EXISTS idx_agent_chat_timestamp ON agent_chat_messages(timestamp);
";

const MIGRATION_V6: &str = "
-- Add cache token columns (SQLite ALTER TABLE is limited, use try-add pattern via separate statements)
";

// Note: v6 adds columns via ALTER TABLE. In Rust we handle this differently
// since we can't do try/except like Python. We check if columns exist first.

const MIGRATION_V7: &str = "
-- Conversations table for agent chat
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    title TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_conversations_agent ON conversations(agent_id);
CREATE INDEX IF NOT EXISTS idx_conversations_updated ON conversations(updated_at);
";

const MIGRATION_V8: &str = "
-- Task queue for harness dispatch
CREATE TABLE IF NOT EXISTS task_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    queued_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    started_at TIMESTAMP,
    completed_at TIMESTAMP,
    harness_response TEXT,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_task_queue_status ON task_queue(status);
CREATE INDEX IF NOT EXISTS idx_task_queue_agent ON task_queue(agent_id);
";

const MIGRATION_V9: &str = "
-- Drop legacy brute-force JSON embeddings table
DROP TABLE IF EXISTS memory_embeddings_json;

-- Vector embeddings via sqlite-vec (vec0 virtual table)
-- Uses cosine distance for similarity search
CREATE VIRTUAL TABLE memory_embeddings USING vec0(
    memory_id text primary key,
    embedding float[384] distance_metric=cosine
);
";

const MIGRATION_V10: &str = "
-- Rebuild conversations with multi-participant support
DROP TABLE IF EXISTS conversations;

CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    title TEXT,
    icon TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    last_message_at TIMESTAMP
);
CREATE INDEX idx_conv_updated ON conversations(updated_at);
CREATE INDEX idx_conv_last_msg ON conversations(last_message_at);

-- Participants (user or agent) in a conversation
CREATE TABLE conversation_participants (
    conversation_id TEXT NOT NULL,
    participant_type TEXT NOT NULL,
    participant_id TEXT NOT NULL,
    joined_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (conversation_id, participant_type, participant_id),
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
);
CREATE INDEX idx_conv_part_conv ON conversation_participants(conversation_id);

-- Messages in a conversation
CREATE TABLE conversation_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    sender_type TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    sender_name TEXT,
    content TEXT NOT NULL,
    message_type TEXT NOT NULL DEFAULT 'message',
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
);
CREATE INDEX idx_conv_msg_conv ON conversation_messages(conversation_id);
CREATE INDEX idx_conv_msg_created ON conversation_messages(created_at);
";

const MIGRATION_V11: &str = "
-- Link tasks to conversations for the continuation pattern.
-- When a task is created from a conversation, completion/failure
-- notifications are sent back to the originating conversation.
ALTER TABLE tasks ADD COLUMN conversation_id TEXT;
CREATE INDEX idx_tasks_conversation ON tasks(conversation_id);
";

const MIGRATION_V12: &str = "
-- Add degraded_model column for budget degrade action.
-- When on_exceeded=degrade, the fallback model name is stored here.
ALTER TABLE budget_state ADD COLUMN degraded_model TEXT;
";

const MIGRATION_V13: &str = "
-- Agent-published apps (ADR-017).
CREATE TABLE IF NOT EXISTS apps (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    icon TEXT,
    description TEXT,
    agent_id TEXT NOT NULL,
    conversation_id TEXT,
    container_id TEXT,
    port INTEGER DEFAULT 3000,
    source_version INTEGER DEFAULT 1,
    status TEXT DEFAULT 'stopped',
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
";

const MIGRATION_V14: &str = "
-- ADR-018: Desired-state reconciliation.
-- The DB stores desired state (what the user wants), not observed state
-- (what Docker reports). Observed state is queried live from Docker.
ALTER TABLE agents ADD COLUMN desired_status TEXT NOT NULL DEFAULT 'stopped';
ALTER TABLE agents ADD COLUMN restart_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agents ADD COLUMN last_attempt_at TIMESTAMP;

-- Migrate: agents that were 'running' or 'starting' should have
-- desired_status='running' (the user wanted them running).
UPDATE agents SET desired_status = 'running'
    WHERE status IN ('running', 'starting');
";

const MIGRATION_V15: &str = "
-- Store app start_command so the reconciler can restart apps.
ALTER TABLE apps ADD COLUMN start_command TEXT;
ALTER TABLE apps ADD COLUMN image TEXT;
";

const MIGRATION_V16: &str = "
-- ADR-019: Background conversations.
-- Track which messages have been processed by the agent so the
-- background task knows what to respond to.
ALTER TABLE conversation_messages ADD COLUMN processed INTEGER NOT NULL DEFAULT 1;
-- New user messages start as unprocessed (0). Existing messages are already processed.

-- Track whether a background task is active for a conversation.
ALTER TABLE conversations ADD COLUMN processing_status TEXT NOT NULL DEFAULT 'idle';
";

const MIGRATION_V17: &str = "
-- ADR-020: Task dependencies.
-- Directed edges: task_id depends on depends_on_id.
-- A task cannot start until all its dependencies are completed.
CREATE TABLE task_dependencies (
    task_id TEXT NOT NULL,
    depends_on_id TEXT NOT NULL,
    PRIMARY KEY (task_id, depends_on_id),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (depends_on_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE INDEX idx_task_deps_task ON task_dependencies(task_id);
CREATE INDEX idx_task_deps_dep ON task_dependencies(depends_on_id);
";

const MIGRATION_V18: &str = "
-- Idle-task tracking columns on agents (XCLAW-47).
ALTER TABLE agents ADD COLUMN idle_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agents ADD COLUMN last_idle_check TIMESTAMP;

-- Task type and hidden flag for idle tasks.
ALTER TABLE tasks ADD COLUMN task_type TEXT NOT NULL DEFAULT 'normal';
ALTER TABLE tasks ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_memory_db() {
        let db = Database::open_memory().unwrap();
        let conn = db.conn();

        // Verify schema version
        let version: String = conn
            .query_row(
                "SELECT value FROM config WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "18");
    }

    #[test]
    fn test_tables_exist() {
        let db = Database::open_memory().unwrap();
        let conn = db.conn();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"memories".to_string()));
        assert!(tables.contains(&"tasks".to_string()));
        assert!(tables.contains(&"agents".to_string()));
        assert!(tables.contains(&"schedules".to_string()));
        assert!(tables.contains(&"activity_logs".to_string()));
        assert!(tables.contains(&"task_queue".to_string()));
        assert!(tables.contains(&"conversations".to_string()));
        assert!(tables.contains(&"conversation_participants".to_string()));
        assert!(tables.contains(&"conversation_messages".to_string()));
    }
}
