use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};

use rusqlite::Connection;
use tracing::info;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use crate::conversations::runtime::ConversationTurnQueue;
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

/// Build the normalized, case-insensitive representation used by task search.
///
/// NFKC makes canonically equivalent and compatibility forms compare the same,
/// including composed kana and full-/half-width Japanese text. Full Unicode
/// case folding handles non-ASCII case mappings such as `É` and `ß`.
pub(crate) fn task_search_key(text: &str) -> String {
    text.nfkc().case_fold().nfkc().collect()
}

fn register_sql_functions(conn: &Connection) -> Result<()> {
    use rusqlite::functions::FunctionFlags;

    conn.create_scalar_function(
        "xpressclaw_task_search_key",
        1,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |context| {
            let text = context.get::<String>(0)?;
            Ok(task_search_key(&text))
        },
    )?;
    Ok(())
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
        register_sql_functions(&conn)?;

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
        register_sql_functions(&conn)?;

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

        for &(target, sql) in schema_migrations() {
            if version < target {
                let transaction = conn.unchecked_transaction()?;
                transaction
                    .execute_batch(sql)
                    .map_err(|e| Error::Migration {
                        version: target,
                        message: e.to_string(),
                    })?;

                if target == 33 {
                    backfill_pending_conversation_turns(&transaction).map_err(|error| {
                        Error::Migration {
                            version: target,
                            message: error.to_string(),
                        }
                    })?;
                }

                if target == 34 {
                    consolidate_legacy_conversation_projects(&transaction).map_err(|error| {
                        Error::Migration {
                            version: target,
                            message: error.to_string(),
                        }
                    })?;
                }

                transaction.execute(
                    "INSERT OR REPLACE INTO config (key, value) VALUES ('schema_version', ?1)",
                    [target.to_string()],
                )?;
                transaction.commit()?;

                info!("applied migration v{target}");
            }
        }

        // Container ownership must survive control-plane restarts while still
        // distinguishing separate XpressClaw data stores that share one
        // Docker/Podman engine. The config table is available before every
        // schema migration, so this identity does not require a schema bump.
        conn.execute(
            "INSERT OR IGNORE INTO config (key, value) VALUES ('installation_id', ?1)",
            [uuid::Uuid::new_v4().to_string()],
        )?;

        Ok(())
    }

    /// Stable identity for the XpressClaw installation backed by this
    /// database. It scopes retained runtime resources on a shared engine.
    pub fn installation_id(&self) -> Result<String> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM config WHERE key = 'installation_id'",
                [],
                |row| row.get(0),
            )
            .map_err(Error::from)
        })
    }

    /// Path to the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Move work committed by the retired background conversation processor into
/// the durable per-Agent turn queue introduced by migration v33. Routing each
/// legacy message in ID order preserves mention semantics and lets the queue
/// coalesce each addressed Agent to the correct high-water message.
fn backfill_pending_conversation_turns(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    // Participant identities were historically polymorphic and therefore did
    // not have an Agent foreign key. Installations that deleted an Agent
    // before durable turns were introduced can still contain stale rows. Drop
    // those rows before routing legacy messages so the new Agent-owned turn
    // foreign keys cannot make the migration fail at startup.
    transaction.execute(
        "DELETE FROM conversation_participants
         WHERE participant_type = 'agent'
           AND NOT EXISTS (
               SELECT 1 FROM agents
               WHERE agents.id = conversation_participants.participant_id
           )",
        [],
    )?;

    let pending = {
        let mut statement = transaction.prepare(
            "SELECT id, conversation_id, sender_id, content
             FROM conversation_messages
             WHERE sender_type = 'user' AND processed = 0
             ORDER BY id ASC",
        )?;
        let pending = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        pending
    };

    for (message_id, conversation_id, sender_id, content) in pending {
        ConversationTurnQueue::enqueue_for_message_in_transaction(
            transaction,
            &conversation_id,
            message_id,
            "user",
            &sender_id,
            &content,
        )?;
    }
    transaction.execute(
        "UPDATE conversation_messages
         SET processed = 1
         WHERE sender_type = 'user' AND processed = 0",
        [],
    )?;
    Ok(())
}

/// Before Projects existed, conversations and task hierarchies could contain
/// several Agents. Migration v33 initially gives each Agent its own Project;
/// merge every connected legacy collaboration component so the new Project
/// invariants hold without losing conversations, tasks, or vector-indexed
/// memory.
fn consolidate_legacy_conversation_projects(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    let mappings = {
        let mut statement = transaction.prepare(
            "WITH RECURSIVE
             task_agents(task_id, agent_id) AS (
                 SELECT task.id, agent.id
                 FROM tasks task
                 JOIN agents agent ON agent.id = task.agent_id
                 UNION
                 SELECT task.id, agent.id
                 FROM tasks task
                 JOIN conversation_participants participant
                   ON participant.conversation_id = task.conversation_id
                  AND participant.participant_type = 'agent'
                 JOIN agents agent ON agent.id = participant.participant_id
             ),
             task_links(origin, peer) AS (
                 SELECT id, id FROM tasks
                 UNION
                 SELECT parent_task_id, id
                 FROM tasks
                 WHERE parent_task_id IS NOT NULL
                 UNION
                 SELECT id, parent_task_id
                 FROM tasks
                 WHERE parent_task_id IS NOT NULL
             ),
             task_reachable(origin, peer) AS (
                 SELECT origin, peer FROM task_links
                 UNION
                 SELECT task_reachable.origin, task_links.peer
                 FROM task_reachable
                 JOIN task_links ON task_links.origin = task_reachable.peer
             ),
             links(origin, peer) AS (
                 SELECT id, id FROM agents
                 UNION
                 SELECT own.participant_id, peer.participant_id
                 FROM conversation_participants own
                 JOIN conversation_participants peer
                  ON peer.conversation_id = own.conversation_id
                  AND peer.participant_type = 'agent'
                 WHERE own.participant_type = 'agent'
                 UNION
                 SELECT own.agent_id, peer.agent_id
                 FROM task_reachable hierarchy
                 JOIN task_agents own ON own.task_id = hierarchy.origin
                 JOIN task_agents peer ON peer.task_id = hierarchy.peer
             ),
             reachable(origin, peer) AS (
                 SELECT origin, peer FROM links
                 UNION
                 SELECT reachable.origin, links.peer
                 FROM reachable JOIN links ON links.origin = reachable.peer
             ),
             components(agent_id, root_agent_id) AS (
                 SELECT origin, MIN(peer) FROM reachable GROUP BY origin
             )
             SELECT agent.id, agent.project_id, root.project_id
             FROM components
             JOIN agents agent ON agent.id = components.agent_id
             JOIN agents root ON root.id = components.root_agent_id
             WHERE agent.project_id IS NOT NULL AND root.project_id IS NOT NULL",
        )?;
        let mappings = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        mappings
    };

    for (agent_id, source_project_id, target_project_id) in mappings {
        if source_project_id == target_project_id {
            continue;
        }
        crate::memory::project::move_project_memory(
            transaction,
            &source_project_id,
            &target_project_id,
        )?;
        transaction.execute(
            "UPDATE agents SET project_id = ?1 WHERE id = ?2",
            rusqlite::params![target_project_id, agent_id],
        )?;
        transaction.execute(
            "UPDATE tasks SET project_id = ?1 WHERE project_id = ?2",
            rusqlite::params![target_project_id, source_project_id],
        )?;
    }
    transaction.execute(
        "UPDATE conversations
         SET project_id = (
             SELECT agent.project_id
             FROM conversation_participants participant
             JOIN agents agent ON agent.id = participant.participant_id
             WHERE participant.conversation_id = conversations.id
               AND participant.participant_type = 'agent'
             ORDER BY participant.joined_at, participant.participant_id
             LIMIT 1
         )
         WHERE EXISTS (
             SELECT 1 FROM conversation_participants participant
             WHERE participant.conversation_id = conversations.id
               AND participant.participant_type = 'agent'
         )",
        [],
    )?;
    transaction.execute(
        "UPDATE tasks
         SET project_id = (
             SELECT conversation.project_id
             FROM conversations conversation
             WHERE conversation.id = tasks.conversation_id
         )
         WHERE conversation_id IS NOT NULL",
        [],
    )?;
    transaction.execute(
        "DELETE FROM projects
         WHERE NOT EXISTS (SELECT 1 FROM agents WHERE agents.project_id = projects.id)
           AND NOT EXISTS (SELECT 1 FROM conversations WHERE conversations.project_id = projects.id)
           AND NOT EXISTS (SELECT 1 FROM tasks WHERE tasks.project_id = projects.id)
           AND NOT EXISTS (
               SELECT 1 FROM project_memory_notes
               WHERE project_memory_notes.project_id = projects.id
           )",
        [],
    )?;
    Ok(())
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

const MIGRATION_V19: &str = "
-- Agent session ID for persistent harness sessions (ADR-021).
ALTER TABLE agents ADD COLUMN session_id TEXT;
";

const MIGRATION_V20: &str = "
-- App container restart backoff (like agents have in V14).
ALTER TABLE apps ADD COLUMN restart_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE apps ADD COLUMN last_attempt_at TIMESTAMP;
";

const MIGRATION_V21: &str = "
-- ADR-022: Connectors and Workflows.

CREATE TABLE IF NOT EXISTS connectors (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    connector_type TEXT NOT NULL,
    config TEXT NOT NULL DEFAULT '{}',
    enabled INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'disconnected',
    error_message TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS connector_channels (
    id TEXT PRIMARY KEY,
    connector_id TEXT NOT NULL REFERENCES connectors(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    channel_type TEXT NOT NULL DEFAULT 'both',
    config TEXT NOT NULL DEFAULT '{}',
    agent_id TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS connector_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    connector_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}',
    processed INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_conn_events_unprocessed ON connector_events(processed, created_at);

CREATE TABLE IF NOT EXISTS workflows (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    yaml_content TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS workflow_instances (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    workflow_version INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    trigger_data TEXT,
    current_node_id TEXT,
    context TEXT DEFAULT '{}',
    started_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP,
    error_message TEXT
);
CREATE INDEX IF NOT EXISTS idx_wf_instances_status ON workflow_instances(status);

CREATE TABLE IF NOT EXISTS workflow_node_executions (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    task_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    input_context TEXT,
    output TEXT,
    attempt INTEGER NOT NULL DEFAULT 1,
    started_at TIMESTAMP,
    completed_at TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_wf_node_exec_instance ON workflow_node_executions(instance_id);
CREATE INDEX IF NOT EXISTS idx_wf_node_exec_task ON workflow_node_executions(task_id);
";

const MIGRATION_V22: &str = "
-- Channel-to-conversation bindings for direct agent routing.
CREATE TABLE IF NOT EXISTS conversation_channel_bindings (
    conversation_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (channel_id, agent_id)
);
";

const MIGRATION_V23: &str = "
-- V2 workflow engine: sequential step-based execution replaces node/edge graph.
-- Drop old tables (v1 was never released, no data to preserve).
DROP TABLE IF EXISTS workflow_node_executions;
DROP TABLE IF EXISTS workflow_instances;

CREATE TABLE workflow_instances (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'running',
    current_flow TEXT DEFAULT 'main',
    current_step_index INTEGER DEFAULT 0,
    trigger_data TEXT,
    variable_store TEXT DEFAULT '{}',
    loop_state TEXT,
    started_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP,
    error_message TEXT
);
CREATE INDEX IF NOT EXISTS idx_wf_instances_status ON workflow_instances(status);

CREATE TABLE workflow_step_executions (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    flow_name TEXT NOT NULL,
    step_id TEXT NOT NULL,
    task_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    input_context TEXT,
    output TEXT,
    attempt INTEGER NOT NULL DEFAULT 1,
    started_at TIMESTAMP,
    completed_at TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_wf_step_exec_instance ON workflow_step_executions(instance_id);
CREATE INDEX IF NOT EXISTS idx_wf_step_exec_task ON workflow_step_executions(task_id);
";

const MIGRATION_V24: &str = "
-- ADR-025: one logical session with isolated native work attempts.
CREATE TABLE logical_sessions (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL UNIQUE,
    title TEXT,
    status TEXT NOT NULL DEFAULT 'idle',
    latest_summary TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE work_attempts (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES logical_sessions(id) ON DELETE CASCADE,
    task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    queue_id INTEGER,
    kind TEXT NOT NULL DEFAULT 'task',
    runner TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    prompt TEXT NOT NULL DEFAULT '',
    native_session_id TEXT,
    container_id TEXT,
    result TEXT,
    error_message TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    started_at TIMESTAMP,
    completed_at TIMESTAMP
);
CREATE INDEX idx_work_attempts_session ON work_attempts(session_id, created_at DESC);
CREATE INDEX idx_work_attempts_status ON work_attempts(status, created_at);
CREATE INDEX idx_work_attempts_task ON work_attempts(task_id, created_at DESC);
CREATE UNIQUE INDEX idx_work_attempts_queue ON work_attempts(queue_id) WHERE queue_id IS NOT NULL;

CREATE TABLE session_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES logical_sessions(id) ON DELETE CASCADE,
    attempt_id TEXT REFERENCES work_attempts(id) ON DELETE SET NULL,
    task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    source_type TEXT NOT NULL,
    source_id TEXT,
    event_type TEXT NOT NULL,
    summary TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}',
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_session_events_session ON session_events(session_id, id);
CREATE INDEX idx_session_events_attempt ON session_events(attempt_id, id);
CREATE INDEX idx_session_events_task ON session_events(task_id, id);

CREATE TABLE attempt_artifacts (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES work_attempts(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES logical_sessions(id) ON DELETE CASCADE,
    artifact_type TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT,
    uri TEXT,
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_attempt_artifacts_session ON attempt_artifacts(session_id, created_at DESC);
CREATE INDEX idx_attempt_artifacts_attempt ON attempt_artifacts(attempt_id, created_at);

ALTER TABLE tasks ADD COLUMN session_id TEXT;
ALTER TABLE tasks ADD COLUMN active_attempt_id TEXT;
ALTER TABLE task_queue ADD COLUMN attempt_id TEXT;

-- Existing configured agents become logical sessions. Their old long-running
-- container state is deliberately not carried forward.
INSERT OR IGNORE INTO logical_sessions (id, agent_id, title)
SELECT id, id, name FROM agents;
UPDATE tasks SET session_id = agent_id WHERE session_id IS NULL AND agent_id IS NOT NULL;

-- Preserve work that was queued before the pivot as native attempts.
INSERT OR IGNORE INTO work_attempts
    (id, session_id, task_id, queue_id, runner, status, prompt, created_at, started_at)
SELECT
    'migrated-' || q.id,
    q.agent_id,
    q.task_id,
    q.id,
    COALESCE((SELECT backend FROM agents WHERE id = q.agent_id), 'native'),
    q.status,
    COALESCE(t.description, t.title, ''),
    q.queued_at,
    q.started_at
FROM task_queue q
JOIN tasks t ON t.id = q.task_id
JOIN logical_sessions s ON s.id = q.agent_id
WHERE q.status IN ('queued', 'running');

UPDATE task_queue
SET attempt_id = 'migrated-' || id
WHERE status IN ('queued', 'running') AND attempt_id IS NULL;
UPDATE tasks
SET active_attempt_id = (
    SELECT q.attempt_id FROM task_queue q
    WHERE q.task_id = tasks.id AND q.status IN ('queued', 'running')
    ORDER BY q.id DESC LIMIT 1
)
WHERE active_attempt_id IS NULL;
";

const MIGRATION_V25: &str = "
-- Durable one-shot schedules for resuming a native project conversation.
ALTER TABLE schedules ADD COLUMN schedule_type TEXT NOT NULL DEFAULT 'cron';
ALTER TABLE schedules ADD COLUMN run_at TEXT;
CREATE INDEX idx_schedules_due ON schedules(enabled, schedule_type, run_at);
";

const MIGRATION_V26: &str = "
-- Binary image attachments sent alongside task chat messages.
CREATE TABLE task_message_attachments (
    id TEXT PRIMARY KEY,
    message_id INTEGER NOT NULL REFERENCES task_messages(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    data BLOB NOT NULL,
    size INTEGER NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_task_message_attachments_message
    ON task_message_attachments(message_id);
";

const MIGRATION_V27: &str = "
-- Latest ACP context-window state belongs to the attempt, not its activity feed.
ALTER TABLE work_attempts ADD COLUMN context_used INTEGER;
ALTER TABLE work_attempts ADD COLUMN context_size INTEGER;
";

const MIGRATION_V28: &str = "
-- One-shot wake-ups created by an active agent continue the task that armed
-- them so their future turn remains in the original user-visible transcript.
ALTER TABLE schedules ADD COLUMN continuation_task_id TEXT
    REFERENCES tasks(id) ON DELETE CASCADE;
CREATE INDEX idx_schedules_continuation_task
    ON schedules(continuation_task_id);
";

const MIGRATION_V29: &str = "
-- Project-scoped, structured knowledge notes exposed to native agents over MCP.
-- This is intentionally separate from the legacy prompt-hook memory tables:
-- agents choose when to read and write durable project knowledge.
CREATE TABLE project_memory_notes (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES logical_sessions(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    summary TEXT NOT NULL,
    note_type TEXT NOT NULL DEFAULT 'fact'
        CHECK (note_type IN ('decision', 'convention', 'procedure', 'fact', 'warning', 'question')),
    state TEXT NOT NULL DEFAULT 'evergreen'
        CHECK (state IN ('inbox', 'evergreen', 'archived')),
    source_task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    source_attempt_id TEXT REFERENCES work_attempts(id) ON DELETE SET NULL,
    created_by TEXT NOT NULL DEFAULT 'agent'
        CHECK (created_by IN ('user', 'agent', 'upkeep')),
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    search_key TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    last_accessed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    access_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_project_memory_notes_project_updated
    ON project_memory_notes(project_id, state, pinned DESC, updated_at DESC);
CREATE INDEX idx_project_memory_notes_task
    ON project_memory_notes(source_task_id);

CREATE TABLE project_memory_tags (
    note_id TEXT NOT NULL REFERENCES project_memory_notes(id) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    tag_key TEXT NOT NULL,
    PRIMARY KEY (note_id, tag_key)
);
CREATE INDEX idx_project_memory_tags_key
    ON project_memory_tags(tag_key, note_id);

CREATE TABLE project_memory_links (
    from_note_id TEXT NOT NULL REFERENCES project_memory_notes(id) ON DELETE CASCADE,
    to_note_id TEXT NOT NULL REFERENCES project_memory_notes(id) ON DELETE CASCADE,
    link_type TEXT NOT NULL DEFAULT 'related'
        CHECK (link_type IN ('related', 'supports', 'contradicts', 'supersedes', 'depends_on', 'example_of')),
    strength REAL NOT NULL DEFAULT 1.0 CHECK (strength >= 0.0 AND strength <= 1.0),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (from_note_id, to_note_id, link_type),
    CHECK (from_note_id <> to_note_id)
);
CREATE INDEX idx_project_memory_links_to
    ON project_memory_links(to_note_id, link_type);

-- Partition-key filtering keeps nearest-neighbour candidates inside a single
-- project instead of retrieving globally and filtering after the fact.
CREATE VIRTUAL TABLE project_memory_embeddings USING vec0(
    note_id text primary key,
    embedding float[384] distance_metric=cosine,
    project_id text partition key
);

CREATE TRIGGER project_memory_notes_delete_embedding
AFTER DELETE ON project_memory_notes
BEGIN
    DELETE FROM project_memory_embeddings WHERE note_id = OLD.id;
END;
";

const MIGRATION_V30: &str = "
-- Persist automatic workflow trigger state so cron schedules fire exactly
-- once across process restarts and can report their latest error in the UI.
ALTER TABLE workflows ADD COLUMN last_triggered_at TIMESTAMP;
ALTER TABLE workflows ADD COLUMN trigger_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workflows ADD COLUMN trigger_error TEXT;
CREATE INDEX idx_workflows_scheduled
    ON workflows(enabled, last_triggered_at);
";

const MIGRATION_V31: &str = "
-- Long-running workflow instances must keep executing the definition they
-- started with, even if the reusable workflow is edited while they wait.
ALTER TABLE workflow_instances ADD COLUMN definition_yaml TEXT;
";

const MIGRATION_V32: &str = "
-- Ordinary tasks that publish pull requests remain active until a human or
-- automated reviewer approves them, or GitHub reports that they were merged.
CREATE TABLE task_pull_requests (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    repo TEXT NOT NULL,
    number INTEGER NOT NULL,
    url TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'waiting'
        CHECK (status IN ('waiting', 'approved', 'merged', 'attention', 'cancelled')),
    started_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    next_poll_at TEXT,
    poll_interval_seconds INTEGER NOT NULL DEFAULT 15,
    last_checked_at TEXT,
    last_activity_at TEXT,
    last_feedback_at TEXT,
    after_cursor TEXT,
    last_error TEXT,
    registration_key TEXT,
    PRIMARY KEY (task_id, owner, repo, number)
);
CREATE INDEX idx_task_pull_requests_due
    ON task_pull_requests(status, next_poll_at);
CREATE INDEX idx_task_pull_requests_agent
    ON task_pull_requests(agent_id, task_id, status);
";

const MIGRATION_V33: &str = "
-- Projects are the top-level collaboration boundary. Existing installations
-- previously treated each configured Agent as a project, so preserve that
-- mental model by creating one project for each existing Agent and assigning
-- its tasks and conversations to it.
CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    icon TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE agents ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
INSERT INTO projects (id, name, description)
SELECT id, name, 'Imported from the existing Agent workspace' FROM agents;

UPDATE agents SET project_id = id WHERE project_id IS NULL;
CREATE INDEX idx_agents_project ON agents(project_id, name);

ALTER TABLE conversations ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE CASCADE;
UPDATE conversations
SET project_id = (
    SELECT a.project_id
    FROM conversation_participants cp
    JOIN agents a ON a.id = cp.participant_id
    WHERE cp.conversation_id = conversations.id
      AND cp.participant_type = 'agent'
      AND a.project_id IS NOT NULL
    ORDER BY cp.joined_at ASC
    LIMIT 1
)
WHERE project_id IS NULL;
INSERT INTO projects (id, name, description)
SELECT 'imported-conversations-' || lower(hex(randomblob(16))), 'Imported conversations',
       'Conversations created before project organization was available'
WHERE EXISTS (SELECT 1 FROM conversations WHERE project_id IS NULL);
UPDATE conversations
SET project_id = (
    SELECT id FROM projects
    WHERE description = 'Conversations created before project organization was available'
    LIMIT 1
)
WHERE project_id IS NULL;
CREATE INDEX idx_conversations_project_activity
    ON conversations(project_id, last_message_at DESC, created_at DESC);

ALTER TABLE tasks ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
UPDATE tasks
SET project_id = COALESCE(
    (SELECT c.project_id FROM conversations c WHERE c.id = tasks.conversation_id),
    (SELECT a.project_id FROM agents a WHERE a.id = tasks.agent_id)
)
WHERE project_id IS NULL;
-- Legacy ACP plans stored their reported steps as unassigned child tasks. Once
-- the assigned parent has a Project, carry that scope through the complete
-- descendant tree so the migration cannot introduce a cross-Project task
-- hierarchy.
WITH RECURSIVE inherited_task_projects(id, project_id) AS (
    SELECT id, project_id FROM tasks WHERE project_id IS NOT NULL
    UNION
    SELECT child.id, parent.project_id
    FROM tasks child
    JOIN inherited_task_projects parent ON parent.id = child.parent_task_id
    WHERE child.project_id IS NULL
)
UPDATE tasks
SET project_id = (
    SELECT inherited.project_id
    FROM inherited_task_projects inherited
    WHERE inherited.id = tasks.id
    LIMIT 1
)
WHERE project_id IS NULL
  AND EXISTS (
      SELECT 1 FROM inherited_task_projects inherited
      WHERE inherited.id = tasks.id
  );
CREATE INDEX idx_tasks_project_updated ON tasks(project_id, updated_at DESC);

ALTER TABLE workflow_instances ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
ALTER TABLE workflow_instances ADD COLUMN conversation_id TEXT REFERENCES conversations(id) ON DELETE SET NULL;
CREATE INDEX idx_workflow_instances_conversation
    ON workflow_instances(conversation_id, started_at DESC);

-- Conversation messages may represent linked tasks or durable file
-- publications in addition to plain chat.
ALTER TABLE conversation_messages ADD COLUMN linked_task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL;
ALTER TABLE conversation_messages ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
CREATE TABLE conversation_message_attachments (
    id TEXT PRIMARY KEY,
    message_id INTEGER NOT NULL REFERENCES conversation_messages(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    data BLOB NOT NULL,
    size INTEGER NOT NULL,
    source_task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_conversation_attachments_message
    ON conversation_message_attachments(message_id);

-- Conversation turns use a queue independent of task execution. One ACP
-- session is retained per conversation/Agent pair, while each queued turn is
-- independently recoverable after a control-plane restart.
CREATE TABLE conversation_agent_sessions (
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    native_session_id TEXT,
    status TEXT NOT NULL DEFAULT 'idle'
        CHECK (status IN ('idle', 'queued', 'running', 'failed')),
    last_error TEXT,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (conversation_id, agent_id)
);
CREATE TABLE conversation_turns (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    trigger_message_id INTEGER REFERENCES conversation_messages(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'running', 'completed', 'failed', 'cancelled')),
    result_message_id INTEGER REFERENCES conversation_messages(id) ON DELETE SET NULL,
    error_message TEXT,
    context_used INTEGER,
    context_size INTEGER,
    queued_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    started_at TIMESTAMP,
    completed_at TIMESTAMP
);
CREATE INDEX idx_conversation_turns_queue
    ON conversation_turns(status, queued_at);
CREATE INDEX idx_conversation_turns_conversation
    ON conversation_turns(conversation_id, queued_at DESC);
CREATE UNIQUE INDEX idx_conversation_turns_active_agent
    ON conversation_turns(conversation_id, agent_id)
    WHERE status IN ('queued', 'running');
";

const MIGRATION_V34: &str = "
-- Project memory originally used the logical Agent session as its ownership
-- foreign key. Rebuild the relational tables around the real Project boundary
-- while preserving notes, tags, links, and the existing vector index.
-- Removed Agents can still own durable memory through their retained logical
-- session. Seed every legacy memory owner before changing the ownership
-- foreign key. Keep this repair in v34 so installations where v33 committed
-- before an older v34 failed can resume the upgrade safely.
INSERT OR IGNORE INTO projects (id, name, description)
SELECT DISTINCT note.project_id,
       COALESCE(
           (SELECT NULLIF(TRIM(session.title), '')
            FROM logical_sessions session WHERE session.id = note.project_id),
           note.project_id
       ),
       'Imported from retained Agent memory'
FROM project_memory_notes note;

CREATE TABLE project_memory_notes_v34 (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    summary TEXT NOT NULL,
    note_type TEXT NOT NULL DEFAULT 'fact'
        CHECK (note_type IN ('decision', 'convention', 'procedure', 'fact', 'warning', 'question')),
    state TEXT NOT NULL DEFAULT 'evergreen'
        CHECK (state IN ('inbox', 'evergreen', 'archived')),
    source_task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    source_attempt_id TEXT REFERENCES work_attempts(id) ON DELETE SET NULL,
    created_by TEXT NOT NULL DEFAULT 'agent'
        CHECK (created_by IN ('user', 'agent', 'upkeep')),
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    search_key TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    last_accessed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    access_count INTEGER NOT NULL DEFAULT 0
);
INSERT INTO project_memory_notes_v34
SELECT * FROM project_memory_notes;

CREATE TABLE project_memory_tags_v34 (
    note_id TEXT NOT NULL REFERENCES project_memory_notes_v34(id) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    tag_key TEXT NOT NULL,
    PRIMARY KEY (note_id, tag_key)
);
INSERT INTO project_memory_tags_v34 SELECT * FROM project_memory_tags;

CREATE TABLE project_memory_links_v34 (
    from_note_id TEXT NOT NULL REFERENCES project_memory_notes_v34(id) ON DELETE CASCADE,
    to_note_id TEXT NOT NULL REFERENCES project_memory_notes_v34(id) ON DELETE CASCADE,
    link_type TEXT NOT NULL DEFAULT 'related'
        CHECK (link_type IN ('related', 'supports', 'contradicts', 'supersedes', 'depends_on', 'example_of')),
    strength REAL NOT NULL DEFAULT 1.0 CHECK (strength >= 0.0 AND strength <= 1.0),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (from_note_id, to_note_id, link_type),
    CHECK (from_note_id <> to_note_id)
);
INSERT INTO project_memory_links_v34 SELECT * FROM project_memory_links;

DROP TRIGGER project_memory_notes_delete_embedding;
DROP TABLE project_memory_links;
DROP TABLE project_memory_tags;
DROP TABLE project_memory_notes;
ALTER TABLE project_memory_notes_v34 RENAME TO project_memory_notes;
ALTER TABLE project_memory_tags_v34 RENAME TO project_memory_tags;
ALTER TABLE project_memory_links_v34 RENAME TO project_memory_links;

CREATE INDEX idx_project_memory_notes_project_updated
    ON project_memory_notes(project_id, state, pinned DESC, updated_at DESC);
CREATE INDEX idx_project_memory_notes_task
    ON project_memory_notes(source_task_id);
CREATE INDEX idx_project_memory_tags_key
    ON project_memory_tags(tag_key, note_id);
CREATE INDEX idx_project_memory_links_to
    ON project_memory_links(to_note_id, link_type);
CREATE TRIGGER project_memory_notes_delete_embedding
AFTER DELETE ON project_memory_notes
BEGIN
    DELETE FROM project_memory_embeddings WHERE note_id = OLD.id;
END;
";

const MIGRATION_V35: &str = "
-- One-shot wake-ups armed from a Conversation must return to that Agent's
-- independent Conversation lane instead of creating a standalone task.
ALTER TABLE schedules ADD COLUMN conversation_id TEXT
    REFERENCES conversations(id) ON DELETE CASCADE;
CREATE INDEX idx_schedules_conversation
    ON schedules(conversation_id);
";

fn schema_migrations() -> &'static [(u32, &'static str)] {
    &[
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
        (19, MIGRATION_V19),
        (20, MIGRATION_V20),
        (21, MIGRATION_V21),
        (22, MIGRATION_V22),
        (23, MIGRATION_V23),
        (24, MIGRATION_V24),
        (25, MIGRATION_V25),
        (26, MIGRATION_V26),
        (27, MIGRATION_V27),
        (28, MIGRATION_V28),
        (29, MIGRATION_V29),
        (30, MIGRATION_V30),
        (31, MIGRATION_V31),
        (32, MIGRATION_V32),
        (33, MIGRATION_V33),
        (34, MIGRATION_V34),
        (35, MIGRATION_V35),
    ]
}

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
        assert_eq!(version, "35");
        let memory_owner: String = conn
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_list('project_memory_notes')
                 WHERE \"from\" = 'project_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(memory_owner, "projects");
    }

    #[test]
    fn v33_backfills_project_scope_through_task_ancestry() {
        ensure_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        register_sql_functions(&conn).unwrap();
        for &(target, sql) in schema_migrations() {
            if target > 32 {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO agents (id, name, backend, config)
             VALUES ('atlas', 'Atlas', 'native', '{}');
             INSERT INTO tasks (id, title, agent_id)
             VALUES ('parent', 'Assigned parent', 'atlas');
             INSERT INTO tasks (id, title, parent_task_id)
             VALUES ('child', 'Reported child', 'parent'),
                    ('grandchild', 'Reported grandchild', 'child');",
        )
        .unwrap();

        let transaction = conn.unchecked_transaction().unwrap();
        transaction.execute_batch(MIGRATION_V33).unwrap();
        transaction.commit().unwrap();

        for task_id in ["parent", "child", "grandchild"] {
            let project_id: String = conn
                .query_row(
                    "SELECT project_id FROM tasks WHERE id = ?1",
                    [task_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(project_id, "atlas", "scope for {task_id}");
        }
    }

    #[test]
    fn v34_merges_agents_connected_through_an_unassigned_task() {
        ensure_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        register_sql_functions(&conn).unwrap();
        for &(target, sql) in schema_migrations() {
            if target > 32 {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO agents (id, name, backend, config)
             VALUES ('atlas', 'Atlas', 'native', '{}'),
                    ('builder', 'Builder', 'native', '{}');
             INSERT INTO tasks (id, title, agent_id)
             VALUES ('parent', 'Atlas parent', 'atlas');
             INSERT INTO tasks (id, title, parent_task_id)
             VALUES ('reported-step', 'Unassigned reported step', 'parent');
             INSERT INTO tasks (id, title, agent_id, parent_task_id)
             VALUES ('grandchild', 'Builder grandchild', 'builder', 'reported-step');",
        )
        .unwrap();

        let transaction = conn.unchecked_transaction().unwrap();
        transaction.execute_batch(MIGRATION_V33).unwrap();
        backfill_pending_conversation_turns(&transaction).unwrap();
        transaction.commit().unwrap();

        let projects_after_v33: Vec<(String, String)> = conn
            .prepare("SELECT id, project_id FROM tasks ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            projects_after_v33,
            vec![
                ("grandchild".into(), "builder".into()),
                ("parent".into(), "atlas".into()),
                ("reported-step".into(), "atlas".into()),
            ]
        );

        let transaction = conn.unchecked_transaction().unwrap();
        transaction.execute_batch(MIGRATION_V34).unwrap();
        consolidate_legacy_conversation_projects(&transaction).unwrap();
        transaction.commit().unwrap();

        let agent_projects: Vec<String> = conn
            .prepare("SELECT project_id FROM agents ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(agent_projects, vec!["atlas"; 2]);
        let task_projects: Vec<String> = conn
            .prepare("SELECT project_id FROM tasks ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(task_projects, vec!["atlas"; 3]);
    }

    #[test]
    fn v34_recovers_memory_owned_by_a_removed_agent_after_v33_commits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-memory.db");
        ensure_sqlite_vec();
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE config (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL,
                    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );",
            )
            .unwrap();
            register_sql_functions(&conn).unwrap();
            for &(target, sql) in schema_migrations() {
                if target > 32 {
                    break;
                }
                let transaction = conn.unchecked_transaction().unwrap();
                transaction.execute_batch(sql).unwrap();
                transaction
                    .execute(
                        "INSERT OR REPLACE INTO config (key, value)
                         VALUES ('schema_version', ?1)",
                        [target.to_string()],
                    )
                    .unwrap();
                transaction.commit().unwrap();
            }
            conn.execute_batch(
                "INSERT INTO agents (id, name, backend, config)
                    VALUES ('retired', 'Retired researcher', 'native', '{}');
                 INSERT INTO logical_sessions (id, agent_id, title)
                    VALUES ('retired', 'retired', 'Retired researcher');
                 INSERT INTO project_memory_notes
                    (id, project_id, title, body, summary, search_key)
                    VALUES (
                        'remembered-note',
                        'retired',
                        'Retained decision',
                        'Keep this knowledge after removing the Agent.',
                        'Keep retained knowledge.',
                        'retained decision keep knowledge'
                    );
                 DELETE FROM agents WHERE id = 'retired';",
            )
            .unwrap();
            let transaction = conn.unchecked_transaction().unwrap();
            transaction.execute_batch(MIGRATION_V33).unwrap();
            transaction
                .execute(
                    "INSERT OR REPLACE INTO config (key, value)
                     VALUES ('schema_version', '33')",
                    [],
                )
                .unwrap();
            transaction.commit().unwrap();
            let missing_project: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM projects WHERE id = 'retired'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(missing_project, 0);
        }

        let upgraded = Database::open(&path).unwrap();
        upgraded.with_conn(|conn| {
            let project: (String, String) = conn
                .query_row(
                    "SELECT name, description FROM projects WHERE id = 'retired'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(project.0, "Retired researcher");
            assert_eq!(project.1, "Imported from retained Agent memory");
            let note: (String, String) = conn
                .query_row(
                    "SELECT project_id, body FROM project_memory_notes
                     WHERE id = 'remembered-note'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(note.0, "retired");
            assert_eq!(note.1, "Keep this knowledge after removing the Agent.");
            let foreign_key_errors: i64 = conn
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(foreign_key_errors, 0);
        });
    }

    #[test]
    fn legacy_multi_agent_conversations_merge_projects_and_memory() {
        use crate::memory::project::{CreateProjectMemoryNote, ProjectMemoryStore};

        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name)
                 VALUES ('atlas', 'Atlas'), ('builder', 'Builder'), ('reviewer', 'Reviewer');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'atlas'),
                        ('builder', 'Builder', 'native', '{}', 'builder'),
                        ('reviewer', 'Reviewer', 'native', '{}', 'reviewer');
                 INSERT INTO conversations (id, title, project_id) VALUES ('shared', 'Shared', 'atlas');
                 INSERT INTO conversation_participants
                    (conversation_id, participant_type, participant_id)
                 VALUES ('shared', 'agent', 'atlas'), ('shared', 'agent', 'reviewer');
                 INSERT INTO tasks (id, title, agent_id, project_id)
                 VALUES ('parent', 'Cross-Agent parent', 'atlas', 'atlas'),
                        ('child', 'Cross-Agent child', 'builder', 'builder'),
                        ('review-parent', 'Reviewer parent', 'reviewer', 'reviewer');
                 INSERT INTO tasks (id, title, parent_task_id, project_id)
                 VALUES ('review-plan', 'Unassigned reported step', 'review-parent', 'reviewer');
                 UPDATE tasks SET parent_task_id = 'parent' WHERE id = 'child';",
            )
        })
        .unwrap();
        let memory = ProjectMemoryStore::new(db.clone())
            .create(
                "reviewer",
                &CreateProjectMemoryNote {
                    title: "Review convention".into(),
                    body: "Check every migration before approval.".into(),
                    summary: None,
                    note_type: "convention".into(),
                    state: "evergreen".into(),
                    source_task_id: None,
                    source_attempt_id: None,
                    created_by: "agent".into(),
                    pinned: false,
                    tags: vec!["review".into()],
                },
            )
            .unwrap();

        db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            consolidate_legacy_conversation_projects(&transaction)?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })
        .unwrap();

        db.with_conn(|conn| {
            let reviewer_project: String = conn
                .query_row(
                    "SELECT project_id FROM agents WHERE id = 'reviewer'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(reviewer_project, "atlas");
            let builder_project: String = conn
                .query_row(
                    "SELECT project_id FROM agents WHERE id = 'builder'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(builder_project, "atlas");
            let task_projects: Vec<String> = conn
                .prepare("SELECT project_id FROM tasks ORDER BY id")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(task_projects, vec!["atlas"; 4]);
            let remaining_projects: i64 = conn
                .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
                .unwrap();
            assert_eq!(remaining_projects, 1);
        });
        let moved = ProjectMemoryStore::new(db)
            .get("atlas", &memory.id)
            .unwrap();
        assert_eq!(moved.project_id, "atlas");
    }

    #[test]
    fn pending_legacy_messages_become_durable_addressed_turns() {
        let db = Database::open_memory().unwrap();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('p', 'Project');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'p'),
                        ('reviewer', 'Reviewer', 'native', '{}', 'p');
                 INSERT INTO conversations (id, title, project_id)
                 VALUES ('legacy', 'Pending work', 'p');
                 INSERT INTO conversation_participants
                    (conversation_id, participant_type, participant_id)
                 VALUES ('legacy', 'agent', 'atlas'),
                        ('legacy', 'agent', 'reviewer'),
                        ('legacy', 'agent', 'removed-agent');
                 INSERT INTO conversation_messages
                    (conversation_id, sender_type, sender_id, content, processed)
                 VALUES ('legacy', 'user', 'local', 'Please investigate', 0),
                        ('legacy', 'user', 'local', '@[AGENT:atlas:Atlas] Extra context', 0);",
            )?;
            let transaction = conn.unchecked_transaction()?;
            backfill_pending_conversation_turns(&transaction)?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })
        .unwrap();

        db.with_conn(|conn| {
            let unprocessed: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM conversation_messages WHERE processed = 0",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(unprocessed, 0);

            let turns = conn
                .prepare(
                    "SELECT agent_id, trigger_message_id FROM conversation_turns
                     WHERE conversation_id = 'legacy' ORDER BY agent_id",
                )
                .unwrap()
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(turns, vec![("atlas".into(), 2), ("reviewer".into(), 1)]);

            let sessions: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM conversation_agent_sessions
                     WHERE conversation_id = 'legacy' AND status = 'queued'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(sessions, 2);

            let orphan_memberships: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM conversation_participants
                     WHERE participant_type = 'agent'
                       AND participant_id = 'removed-agent'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(orphan_memberships, 0);
        });
    }

    #[test]
    fn installation_id_survives_database_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("xpressclaw.db");
        let first = Database::open(&path).unwrap().installation_id().unwrap();
        let second = Database::open(&path).unwrap().installation_id().unwrap();

        assert_eq!(first, second);
        assert!(uuid::Uuid::parse_str(&first).is_ok());
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
        assert!(tables.contains(&"logical_sessions".to_string()));
        assert!(tables.contains(&"session_events".to_string()));
        assert!(tables.contains(&"work_attempts".to_string()));
        assert!(tables.contains(&"attempt_artifacts".to_string()));
        assert!(tables.contains(&"task_message_attachments".to_string()));
        assert!(tables.contains(&"project_memory_notes".to_string()));
        assert!(tables.contains(&"project_memory_tags".to_string()));
        assert!(tables.contains(&"project_memory_links".to_string()));
        assert!(tables.contains(&"project_memory_embeddings".to_string()));
        assert!(tables.contains(&"task_pull_requests".to_string()));
    }
}
