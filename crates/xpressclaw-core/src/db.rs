use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};

use rusqlite::Connection;
use serde_json::Value;
use tracing::info;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use crate::conversations::runtime::{reconcile_message_deletion_turns, ConversationTurnQueue};
use crate::error::{Error, Result};
use crate::workflows::context;
use crate::workflows::definition::{WorkflowDefinition, WorkflowInputType};

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

                if target == 40 {
                    backfill_workflow_agent_bindings(&transaction).map_err(|error| {
                        Error::Migration {
                            version: target,
                            message: error.to_string(),
                        }
                    })?;
                }

                if target == 46 {
                    reconcile_adopted_conversation_tombstones(&transaction).map_err(|error| {
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

/// Recover Project ownership for workflow instances created before workflow
/// Agent bindings became durable.
///
/// The SQL portion of v40 recovers the Agent attached to a current taskless
/// wait. This pass also resolves typed inputs and Agent selectors from the
/// immutable definition/trigger snapshot so future steps cannot outlive a
/// Project cascade. An active legacy run whose complete Agent set cannot be
/// recovered is cancelled rather than allowed to resume against a deleted
/// Agent later. Active scoped runs are also cancelled when their recovered
/// Agent ownership contradicts their persisted Project scope.
const UNRECOVERABLE_WORKFLOW_BINDINGS_ERROR: &str =
    "Stopped during upgrade because not every Agent binding could be recovered safely";
const CONFLICTING_WORKFLOW_BINDINGS_ERROR: &str =
    "Stopped during upgrade because an Agent binding conflicts with the workflow Project scope";

fn backfill_workflow_agent_bindings(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    let agents = {
        let mut statement = transaction.prepare("SELECT id, project_id FROM agents")?;
        let agents = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        agents
    };
    let instances = {
        let mut statement = transaction.prepare(
            "SELECT instance.id,
                    instance.status,
                    instance.project_id,
                    conversation.project_id,
                    COALESCE(NULLIF(instance.definition_yaml, ''), workflow.yaml_content),
                    COALESCE(instance.trigger_data, '{}')
             FROM workflow_instances instance
             JOIN workflows workflow ON workflow.id = instance.workflow_id
             LEFT JOIN conversations conversation
               ON conversation.id = instance.conversation_id",
        )?;
        let instances = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        instances
    };

    for (
        instance_id,
        status,
        instance_project_id,
        conversation_project_id,
        definition_yaml,
        trigger_json,
    ) in instances
    {
        let recovered = (|| -> Result<(BTreeSet<String>, bool)> {
            let mut definition = WorkflowDefinition::parse(&definition_yaml)?;
            let provided_trigger = serde_json::from_str::<Value>(&trigger_json)
                .map_err(|error| Error::Workflow(format!("invalid trigger data: {error}")))?;
            let trigger_data = definition.resolve_inputs(&provided_trigger)?;
            let initial_context =
                context::build_context(&trigger_data, &definition.variables, &HashMap::new());

            let (step_bindings, _) = definition.resolve_agent_bindings(&initial_context, false)?;
            let mut agent_ids = step_bindings
                .into_iter()
                .map(|(_, agent_id)| agent_id)
                .collect::<BTreeSet<_>>();
            for (name, input) in &definition.inputs {
                if input.input_type != WorkflowInputType::Agent {
                    continue;
                }
                if let Some(agent_id) = trigger_data.get(name).and_then(Value::as_str) {
                    agent_ids.insert(agent_id.to_string());
                }
            }
            let every_step_resolved = definition
                .resolve_agent_bindings(&initial_context, true)
                .is_ok();
            Ok((agent_ids, every_step_resolved))
        })();

        let (agent_ids, mut complete) = recovered.unwrap_or_else(|_| (BTreeSet::new(), false));
        let scoped_project_id = instance_project_id
            .as_deref()
            .or(conversation_project_id.as_deref());
        let mut scope_consistent = match (
            instance_project_id.as_deref(),
            conversation_project_id.as_deref(),
        ) {
            (Some(instance_project_id), Some(conversation_project_id)) => {
                instance_project_id == conversation_project_id
            }
            _ => true,
        };
        for agent_id in agent_ids {
            match agents.get(&agent_id) {
                Some(Some(project_id)) => {
                    transaction.execute(
                        "INSERT OR IGNORE INTO workflow_instance_agent_bindings
                         (instance_id, agent_id, project_id) VALUES (?1, ?2, ?3)",
                        rusqlite::params![instance_id, agent_id, project_id],
                    )?;
                    if scoped_project_id.is_some_and(|scope| project_id.as_str() != scope) {
                        scope_consistent = false;
                    }
                }
                Some(None) => {
                    if scoped_project_id.is_some() {
                        scope_consistent = false;
                    }
                }
                None => complete = false,
            }
        }

        let cancellation_error = if !complete {
            Some(UNRECOVERABLE_WORKFLOW_BINDINGS_ERROR)
        } else if !scope_consistent {
            Some(CONFLICTING_WORKFLOW_BINDINGS_ERROR)
        } else {
            None
        };
        if let Some(cancellation_error) = cancellation_error {
            if matches!(status.as_str(), "running" | "waiting") {
                transaction.execute(
                    "UPDATE workflow_instances
                     SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                         error_message = ?1
                     WHERE id = ?2 AND status IN ('running', 'waiting')",
                    rusqlite::params![cancellation_error, instance_id],
                )?;
            }
        }
    }

    Ok(())
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

/// Apply the normal deletion reconciliation to tombstones first seen while
/// upgrading from v45. Keep the sync marker until this Rust pass has located
/// every adopted row, then remove it as part of the same migration transaction.
fn reconcile_adopted_conversation_tombstones(transaction: &rusqlite::Connection) -> Result<()> {
    let adopted = {
        let mut statement = transaction.prepare(
            "SELECT id, conversation_id FROM conversation_messages
             WHERE deleted_at IS NOT NULL
               AND json_valid(metadata)
               AND json_type(metadata, '$.xpressclaw_deleted_at') = 'text'
             ORDER BY id ASC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for (message_id, conversation_id) in adopted {
        reconcile_message_deletion_turns(transaction, &conversation_id, message_id)?;
    }
    transaction.execute(
        "UPDATE conversation_messages
         SET metadata = json_remove(metadata, '$.xpressclaw_deleted_at')
         WHERE json_valid(metadata)
           AND json_type(metadata, '$.xpressclaw_deleted_at') = 'text'",
        [],
    )?;
    Ok(())
}

struct LegacyComponents {
    parents: Vec<usize>,
    ranks: Vec<u8>,
}

impl LegacyComponents {
    fn new(node_count: usize) -> Self {
        Self {
            parents: (0..node_count).collect(),
            ranks: vec![0; node_count],
        }
    }

    fn find(&mut self, node: usize) -> usize {
        let parent = self.parents[node];
        if parent != node {
            self.parents[node] = self.find(parent);
        }
        self.parents[node]
    }

    fn union(&mut self, left: usize, right: usize) {
        let mut left_root = self.find(left);
        let mut right_root = self.find(right);
        if left_root == right_root {
            return;
        }
        if self.ranks[left_root] < self.ranks[right_root] {
            std::mem::swap(&mut left_root, &mut right_root);
        }
        self.parents[right_root] = left_root;
        if self.ranks[left_root] == self.ranks[right_root] {
            self.ranks[left_root] += 1;
        }
    }
}

/// Before Projects existed, conversations and task hierarchies could contain
/// several Agents. Migration v33 initially gives each Agent its own Project;
/// merge every connected legacy collaboration component so the new Project
/// invariants hold without losing conversations, tasks, or vector-indexed
/// memory.
fn consolidate_legacy_conversation_projects(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    let agents = {
        let mut statement = transaction.prepare("SELECT id, project_id FROM agents")?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<(String, Option<String>)>, _>>()?;
        rows
    };
    let conversations = {
        let mut statement = transaction.prepare("SELECT id, project_id FROM conversations")?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<(String, Option<String>)>, _>>()?;
        rows
    };
    let tasks = {
        let mut statement = transaction.prepare(
            "SELECT id, agent_id, conversation_id, parent_task_id, project_id FROM tasks",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<std::result::Result<
                Vec<(
                    String,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                )>,
                _,
            >>()?;
        rows
    };
    let participants = {
        let mut statement = transaction.prepare(
            "SELECT participant.conversation_id, participant.participant_id
             FROM conversation_participants participant
             JOIN agents agent ON agent.id = participant.participant_id
             WHERE participant.participant_type = 'agent'",
        )?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<(String, String)>, _>>()?;
        rows
    };

    let conversation_offset = agents.len();
    let task_offset = conversation_offset + conversations.len();
    let mut components = LegacyComponents::new(task_offset + tasks.len());
    let agent_indexes: HashMap<&str, usize> = agents
        .iter()
        .enumerate()
        .map(|(index, (id, _))| (id.as_str(), index))
        .collect();
    let conversation_indexes: HashMap<&str, usize> = conversations
        .iter()
        .enumerate()
        .map(|(index, (id, _))| (id.as_str(), conversation_offset + index))
        .collect();
    let task_indexes: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(index, (id, ..))| (id.as_str(), task_offset + index))
        .collect();

    // Existing Project ownership is itself a collaboration edge. This also
    // makes the source-to-target Project mapping unambiguous if an older
    // migration attempt already attached several objects to one Project.
    let mut project_anchors = HashMap::<String, usize>::new();
    for (index, (_, project_id)) in agents.iter().enumerate() {
        if let Some(project_id) = project_id {
            if let Some(anchor) = project_anchors.get(project_id) {
                components.union(index, *anchor);
            } else {
                project_anchors.insert(project_id.clone(), index);
            }
        }
    }
    for (index, (_, project_id)) in conversations.iter().enumerate() {
        let index = conversation_offset + index;
        if let Some(project_id) = project_id {
            if let Some(anchor) = project_anchors.get(project_id) {
                components.union(index, *anchor);
            } else {
                project_anchors.insert(project_id.clone(), index);
            }
        }
    }
    for (index, (_, _, _, _, project_id)) in tasks.iter().enumerate() {
        let index = task_offset + index;
        if let Some(project_id) = project_id {
            if let Some(anchor) = project_anchors.get(project_id) {
                components.union(index, *anchor);
            } else {
                project_anchors.insert(project_id.clone(), index);
            }
        }
    }

    for (conversation_id, agent_id) in &participants {
        if let (Some(conversation), Some(agent)) = (
            conversation_indexes.get(conversation_id.as_str()),
            agent_indexes.get(agent_id.as_str()),
        ) {
            components.union(*conversation, *agent);
        }
    }
    for (index, (_, agent_id, conversation_id, parent_task_id, _)) in tasks.iter().enumerate() {
        let task = task_offset + index;
        if let Some(agent) = agent_id
            .as_deref()
            .and_then(|agent_id| agent_indexes.get(agent_id))
        {
            components.union(task, *agent);
        }
        if let Some(conversation) = conversation_id
            .as_deref()
            .and_then(|conversation_id| conversation_indexes.get(conversation_id))
        {
            components.union(task, *conversation);
        }
        if let Some(parent) = parent_task_id
            .as_deref()
            .and_then(|parent_task_id| task_indexes.get(parent_task_id))
        {
            components.union(task, *parent);
        }
    }

    // Prefer the lexicographically first Agent's Project, matching the old
    // deterministic consolidation behavior. Components without an Agent keep
    // their lexicographically first existing Project.
    let mut component_targets = HashMap::<usize, (u8, String, String)>::new();
    {
        let mut consider_target = |node: usize, priority: u8, key: &str, project_id: &str| {
            let root = components.find(node);
            let candidate = (priority, key.to_string(), project_id.to_string());
            if component_targets
                .get(&root)
                .is_none_or(|current| &candidate < current)
            {
                component_targets.insert(root, candidate);
            }
        };
        for (index, (agent_id, project_id)) in agents.iter().enumerate() {
            if let Some(project_id) = project_id {
                consider_target(index, 0, agent_id, project_id);
            }
        }
        for (index, (_, project_id)) in conversations.iter().enumerate() {
            if let Some(project_id) = project_id {
                consider_target(conversation_offset + index, 1, project_id, project_id);
            }
        }
        for (index, (_, _, _, _, project_id)) in tasks.iter().enumerate() {
            if let Some(project_id) = project_id {
                consider_target(task_offset + index, 1, project_id, project_id);
            }
        }
    }

    fn record_project_target(
        project_targets: &mut HashMap<String, String>,
        source_project_id: Option<&String>,
        target_project_id: &str,
    ) -> Result<()> {
        let Some(source_project_id) = source_project_id else {
            return Ok(());
        };
        if let Some(existing) = project_targets.get(source_project_id) {
            if existing != target_project_id {
                return Err(Error::Database(format!(
                    "legacy project {source_project_id} belongs to multiple collaboration components"
                )));
            }
        } else {
            project_targets.insert(source_project_id.clone(), target_project_id.to_string());
        }
        Ok(())
    }

    let mut project_targets = HashMap::<String, String>::new();
    let mut agent_updates = Vec::<(String, String)>::new();
    for (index, (agent_id, source_project_id)) in agents.iter().enumerate() {
        let root = components.find(index);
        if let Some((_, _, target_project_id)) = component_targets.get(&root) {
            record_project_target(
                &mut project_targets,
                source_project_id.as_ref(),
                target_project_id,
            )?;
            agent_updates.push((agent_id.clone(), target_project_id.clone()));
        }
    }
    let mut conversation_updates = Vec::<(String, String)>::new();
    for (index, (conversation_id, source_project_id)) in conversations.iter().enumerate() {
        let root = components.find(conversation_offset + index);
        if let Some((_, _, target_project_id)) = component_targets.get(&root) {
            record_project_target(
                &mut project_targets,
                source_project_id.as_ref(),
                target_project_id,
            )?;
            conversation_updates.push((conversation_id.clone(), target_project_id.clone()));
        }
    }
    let mut task_updates = Vec::<(String, String)>::new();
    for (index, (task_id, _, _, _, source_project_id)) in tasks.iter().enumerate() {
        let root = components.find(task_offset + index);
        if let Some((_, _, target_project_id)) = component_targets.get(&root) {
            record_project_target(
                &mut project_targets,
                source_project_id.as_ref(),
                target_project_id,
            )?;
            task_updates.push((task_id.clone(), target_project_id.clone()));
        }
    }

    let mut project_moves = project_targets.into_iter().collect::<Vec<_>>();
    project_moves.sort();
    for (source_project_id, target_project_id) in project_moves {
        if source_project_id != target_project_id {
            crate::memory::project::move_project_memory(
                transaction,
                &source_project_id,
                &target_project_id,
            )?;
        }
    }
    {
        let mut statement =
            transaction.prepare("UPDATE agents SET project_id = ?1 WHERE id = ?2")?;
        for (agent_id, project_id) in agent_updates {
            statement.execute(rusqlite::params![project_id, agent_id])?;
        }
    }
    {
        let mut statement =
            transaction.prepare("UPDATE conversations SET project_id = ?1 WHERE id = ?2")?;
        for (conversation_id, project_id) in conversation_updates {
            statement.execute(rusqlite::params![project_id, conversation_id])?;
        }
    }
    {
        let mut statement =
            transaction.prepare("UPDATE tasks SET project_id = ?1 WHERE id = ?2")?;
        for (task_id, project_id) in task_updates {
            statement.execute(rusqlite::params![project_id, task_id])?;
        }
    }
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

const MIGRATION_V36: &str = "
-- Explicit Git-backed Project synchronization. Message record IDs are kept
-- separately from SQLite row IDs so Git history remains portable across
-- installations and can represent branches through parent record IDs.
CREATE TABLE conversation_message_sync (
    record_id TEXT PRIMARY KEY,
    message_id INTEGER NOT NULL UNIQUE
        REFERENCES conversation_messages(id) ON DELETE CASCADE,
    parent_record_id TEXT
        REFERENCES conversation_message_sync(record_id) ON DELETE SET NULL
);
CREATE INDEX idx_conversation_message_sync_parent
    ON conversation_message_sync(parent_record_id);

CREATE TABLE task_message_sync (
    record_id TEXT PRIMARY KEY,
    message_id INTEGER NOT NULL UNIQUE
        REFERENCES task_messages(id) ON DELETE CASCADE,
    parent_record_id TEXT
        REFERENCES task_message_sync(record_id) ON DELETE SET NULL
);
CREATE INDEX idx_task_message_sync_parent
    ON task_message_sync(parent_record_id);

-- Reusable workflows may be shared by more than one Project. Runs establish
-- the association automatically; fetch also records imported associations.
CREATE TABLE project_workflows (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, workflow_id)
);
INSERT OR IGNORE INTO project_workflows (project_id, workflow_id)
SELECT DISTINCT project_id, workflow_id
FROM workflow_instances
WHERE project_id IS NOT NULL;
CREATE TRIGGER project_workflows_from_instance
AFTER INSERT ON workflow_instances
WHEN NEW.project_id IS NOT NULL
BEGIN
    INSERT OR IGNORE INTO project_workflows (project_id, workflow_id)
    VALUES (NEW.project_id, NEW.workflow_id);
END;
CREATE TRIGGER project_workflows_from_instance_update
AFTER UPDATE OF project_id, workflow_id ON workflow_instances
WHEN NEW.project_id IS NOT NULL
BEGIN
    INSERT OR IGNORE INTO project_workflows (project_id, workflow_id)
    VALUES (NEW.project_id, NEW.workflow_id);
END;

-- The last observed commit plus local and remote portable snapshot hashes
-- provide a path-aware optimistic-concurrency boundary. Secrets are never
-- stored here.
CREATE TABLE project_sync_state (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    remote TEXT NOT NULL,
    branch TEXT NOT NULL,
    store_path TEXT NOT NULL,
    last_commit TEXT NOT NULL,
    local_snapshot_hash TEXT NOT NULL,
    remote_snapshot_hash TEXT NOT NULL,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (project_id, remote, branch, store_path)
);
";

const MIGRATION_V37: &str = "
-- ACP plans are turn-local progress reports, not durable delegated work.
-- Keep provenance and completion gating in first-class columns so task
-- lifecycle decisions never depend on plan titles or other heuristics.
ALTER TABLE tasks ADD COLUMN provenance TEXT NOT NULL DEFAULT 'durable';
ALTER TABLE tasks ADD COLUMN blocks_parent INTEGER NOT NULL DEFAULT 1
    CHECK (blocks_parent IN (0, 1));

UPDATE tasks
SET provenance = json_extract(context, '$.origin')
WHERE context IS NOT NULL
  AND json_valid(context)
  AND json_type(context, '$.origin') = 'text'
  AND json_extract(context, '$.origin') != 'native_plan';

UPDATE tasks
SET provenance = 'native_plan', blocks_parent = 0
WHERE context IS NOT NULL
  AND json_valid(context)
  AND json_extract(context, '$.origin') = 'native_plan'
  AND json_type(context, '$.attempt_id') = 'text'
  AND trim(json_extract(context, '$.attempt_id')) != ''
  AND json_type(context, '$.index') = 'integer'
  AND json_extract(context, '$.index') >= 0;

CREATE INDEX idx_tasks_parent_gate
    ON tasks(parent_task_id, blocks_parent, status);
";

const MIGRATION_V38: &str = "
-- Work timers measure one response cycle, not the lifetime of an Agent,
-- logical session, or durable task. Legacy timestamps remain unchanged for
-- API compatibility; these fields make the current queue and response phases
-- explicit and associate task continuations with the message that triggered
-- them.
ALTER TABLE work_attempts ADD COLUMN trigger_message_id INTEGER
    REFERENCES task_messages(id) ON DELETE SET NULL;
ALTER TABLE work_attempts ADD COLUMN response_queued_at TIMESTAMP;
ALTER TABLE work_attempts ADD COLUMN response_started_at TIMESTAMP;

UPDATE work_attempts
SET response_queued_at = created_at,
    response_started_at = started_at;

ALTER TABLE conversation_turns ADD COLUMN response_queued_at TIMESTAMP;
ALTER TABLE conversation_turns ADD COLUMN response_started_at TIMESTAMP;

UPDATE conversation_turns
SET response_queued_at = queued_at,
    response_started_at = started_at;

CREATE INDEX idx_work_attempts_trigger_message
    ON work_attempts(trigger_message_id);
";

const MIGRATION_V41: &str = r#"
-- Durable, bounded telemetry for the instance-wide Control center. Dashboard
-- rows contain only short display-safe summaries and normalized counters;
-- raw tool arguments, terminal output, prompts, and diffs never enter these
-- tables.
CREATE TABLE dashboard_events (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL,
    event_kind TEXT NOT NULL,
    occurred_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    project_name TEXT,
    agent_id TEXT,
    agent_name TEXT,
    source_kind TEXT NOT NULL DEFAULT 'system',
    source_label TEXT NOT NULL DEFAULT 'XpressClaw',
    target_type TEXT NOT NULL,
    target_id TEXT NOT NULL,
    target_title TEXT NOT NULL,
    href TEXT NOT NULL,
    severity TEXT NOT NULL DEFAULT 'info'
        CHECK (severity IN ('info', 'success', 'warning', 'error')),
    needs_attention INTEGER NOT NULL DEFAULT 0
        CHECK (needs_attention IN (0, 1)),
    preview TEXT NOT NULL DEFAULT '',
    work_kind TEXT,
    work_id TEXT
);
CREATE INDEX idx_dashboard_events_cursor_project
    ON dashboard_events(project_id, cursor DESC);
CREATE INDEX idx_dashboard_events_time
    ON dashboard_events(occurred_at DESC, cursor DESC);
CREATE INDEX idx_dashboard_events_attention
    ON dashboard_events(needs_attention, occurred_at DESC, cursor DESC);
CREATE INDEX idx_dashboard_events_event_version
    ON dashboard_events(event_id, cursor DESC);
CREATE INDEX idx_dashboard_events_work
    ON dashboard_events(work_kind, work_id, cursor DESC);
CREATE INDEX idx_dashboard_events_kind_project_time
    ON dashboard_events(event_kind, project_id, occurred_at DESC);

CREATE TABLE dashboard_metric_points (
    work_kind TEXT NOT NULL CHECK (work_kind IN ('attempt', 'conversation_turn')),
    work_id TEXT NOT NULL,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    agent_id TEXT,
    bucket_at TEXT NOT NULL,
    context_used INTEGER,
    context_size INTEGER,
    tool_calls INTEGER NOT NULL DEFAULT 0,
    code_additions INTEGER,
    code_deletions INTEGER,
    git_state TEXT NOT NULL DEFAULT 'unobserved'
        CHECK (git_state IN ('unobserved', 'available', 'partial', 'unavailable')),
    git_detail TEXT,
    recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (work_kind, work_id, bucket_at)
);
CREATE INDEX idx_dashboard_metrics_project_time
    ON dashboard_metric_points(project_id, bucket_at DESC);
CREATE INDEX idx_dashboard_metrics_time
    ON dashboard_metric_points(bucket_at DESC);

CREATE TABLE dashboard_git_baselines (
    work_kind TEXT NOT NULL CHECK (work_kind IN ('attempt', 'conversation_turn')),
    work_id TEXT NOT NULL,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    agent_id TEXT,
    workspace TEXT NOT NULL,
    baseline_ref TEXT,
    baseline_json TEXT NOT NULL DEFAULT '{}',
    git_state TEXT NOT NULL DEFAULT 'unavailable'
        CHECK (git_state IN ('available', 'partial', 'unavailable')),
    git_detail TEXT,
    captured_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_snapshot_at TEXT,
    finalized_at TEXT,
    PRIMARY KEY (work_kind, work_id)
);
CREATE INDEX idx_dashboard_git_workspace_active
    ON dashboard_git_baselines(workspace, finalized_at);

-- Stable message event IDs let the browser replace an in-progress Agent
-- message with its latest text when a reconnect overlaps streaming updates.
CREATE TRIGGER dashboard_task_message_insert
AFTER INSERT ON task_messages
WHEN NEW.role IN ('user', 'assistant')
 AND (NEW.role != 'assistant' OR trim(NEW.content) != '')
BEGIN
    INSERT INTO dashboard_events (
        event_id, event_kind, occurred_at, project_id, project_name,
        agent_id, agent_name, source_kind, source_label,
        target_type, target_id, target_title, href, severity,
        needs_attention, preview
    )
    SELECT
        'task-message:' || NEW.id,
        CASE WHEN NEW.role = 'assistant' THEN 'agent_response' ELSE 'task_message' END,
        COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', NEW.timestamp),
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
        t.project_id, p.name, t.agent_id, a.name,
        CASE WHEN NEW.role = 'assistant' THEN 'agent' ELSE 'user' END,
        CASE WHEN NEW.role = 'assistant' THEN COALESCE(a.name, 'Agent') ELSE 'You' END,
        'task', t.id, t.title, '/tasks/' || t.id,
        'info', 0,
        CASE WHEN trim(NEW.content) = '' THEN 'Image attachment'
             ELSE substr(trim(replace(replace(NEW.content, char(10), ' '), char(13), ' ')), 1, 240)
        END
    FROM tasks t
    LEFT JOIN projects p ON p.id = t.project_id
    LEFT JOIN agents a ON a.id = t.agent_id
    WHERE t.id = NEW.task_id AND t.hidden = 0;
END;

CREATE TRIGGER dashboard_task_message_update
AFTER UPDATE OF content ON task_messages
WHEN NEW.role IN ('user', 'assistant')
 AND NEW.content != OLD.content
 AND trim(NEW.content) != ''
BEGIN
    INSERT INTO dashboard_events (
        event_id, event_kind, occurred_at, project_id, project_name,
        agent_id, agent_name, source_kind, source_label,
        target_type, target_id, target_title, href, severity,
        needs_attention, preview
    )
    SELECT
        'task-message:' || NEW.id,
        CASE WHEN NEW.role = 'assistant' THEN 'agent_response' ELSE 'task_message' END,
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
        t.project_id, p.name, t.agent_id, a.name,
        CASE WHEN NEW.role = 'assistant' THEN 'agent' ELSE 'user' END,
        CASE WHEN NEW.role = 'assistant' THEN COALESCE(a.name, 'Agent') ELSE 'You' END,
        'task', t.id, t.title, '/tasks/' || t.id,
        'info', 0,
        substr(trim(replace(replace(NEW.content, char(10), ' '), char(13), ' ')), 1, 240)
    FROM tasks t
    LEFT JOIN projects p ON p.id = t.project_id
    LEFT JOIN agents a ON a.id = t.agent_id
    WHERE t.id = NEW.task_id AND t.hidden = 0;
END;

CREATE TRIGGER dashboard_conversation_message_insert
AFTER INSERT ON conversation_messages
WHEN NEW.sender_type IN ('user', 'agent')
BEGIN
    INSERT INTO dashboard_events (
        event_id, event_kind, occurred_at, project_id, project_name,
        agent_id, agent_name, source_kind, source_label,
        target_type, target_id, target_title, href, severity,
        needs_attention, preview
    )
    SELECT
        'conversation-message:' || NEW.id,
        CASE WHEN NEW.sender_type = 'agent' THEN 'agent_response' ELSE 'conversation_message' END,
        COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', NEW.created_at),
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
        c.project_id, p.name,
        CASE WHEN NEW.sender_type = 'agent' THEN NEW.sender_id ELSE NULL END,
        CASE WHEN NEW.sender_type = 'agent' THEN COALESCE(NEW.sender_name, a.name) ELSE NULL END,
        NEW.sender_type,
        COALESCE(NEW.sender_name,
            CASE WHEN NEW.sender_type = 'user' THEN 'You' ELSE NEW.sender_id END),
        'conversation', c.id, COALESCE(c.title, 'Untitled conversation'),
        '/conversations/' || c.id, 'info', 0,
        CASE WHEN trim(NEW.content) = '' THEN 'File attachment'
             ELSE substr(trim(replace(replace(NEW.content, char(10), ' '), char(13), ' ')), 1, 240)
        END
    FROM conversations c
    LEFT JOIN projects p ON p.id = c.project_id
    LEFT JOIN agents a ON a.id = NEW.sender_id AND NEW.sender_type = 'agent'
    WHERE c.id = NEW.conversation_id;
END;

CREATE TRIGGER dashboard_task_status_update
AFTER UPDATE OF status ON tasks
WHEN NEW.status != OLD.status AND NEW.hidden = 0
BEGIN
    INSERT INTO dashboard_events (
        event_id, event_kind, occurred_at, project_id, project_name,
        agent_id, agent_name, source_kind, source_label,
        target_type, target_id, target_title, href, severity,
        needs_attention, preview
    )
    SELECT
        'task-status:' || lower(hex(randomblob(16))),
        CASE NEW.status
            WHEN 'waiting_for_input' THEN 'waiting_for_input'
            WHEN 'blocked' THEN 'failure'
            WHEN 'completed' THEN 'completion'
            WHEN 'cancelled' THEN 'cancellation'
            ELSE 'status_change'
        END,
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
        NEW.project_id, p.name, NEW.agent_id, a.name, 'system', 'XpressClaw',
        'task', NEW.id, NEW.title, '/tasks/' || NEW.id,
        CASE NEW.status WHEN 'blocked' THEN 'error'
             WHEN 'waiting_for_input' THEN 'warning'
             WHEN 'completed' THEN 'success' ELSE 'info' END,
        CASE WHEN NEW.status IN ('blocked', 'waiting_for_input') THEN 1 ELSE 0 END,
        CASE NEW.status
            WHEN 'waiting_for_input' THEN 'The Agent needs your input'
            WHEN 'blocked' THEN 'Task is blocked'
            WHEN 'completed' THEN 'Task completed'
            WHEN 'cancelled' THEN 'Task cancelled'
            WHEN 'in_progress' THEN 'Work started'
            ELSE 'Task is pending'
        END
    FROM (SELECT 1) singleton
    LEFT JOIN projects p ON p.id = NEW.project_id
    LEFT JOIN agents a ON a.id = NEW.agent_id;
END;

CREATE TRIGGER dashboard_session_event_insert
AFTER INSERT ON session_events
WHEN NEW.event_type IN ('runner_progress', 'elicitation_pending', 'attempt_failed')
 AND (
    NEW.event_type != 'runner_progress'
    OR NOT EXISTS (
        SELECT 1 FROM dashboard_events
        WHERE event_kind = 'progress'
          AND work_kind = 'attempt'
          AND work_id = NEW.attempt_id
          AND occurred_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 seconds')
    )
 )
BEGIN
    INSERT INTO dashboard_events (
        event_id, event_kind, occurred_at, project_id, project_name,
        agent_id, agent_name, source_kind, source_label,
        target_type, target_id, target_title, href, severity,
        needs_attention, preview, work_kind, work_id
    )
    SELECT
        'session-event:' || NEW.id,
        CASE WHEN NEW.event_type = 'elicitation_pending' THEN 'waiting_for_input'
             WHEN NEW.event_type = 'attempt_failed' THEN 'failure'
             ELSE 'progress' END,
        COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', NEW.created_at),
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
        t.project_id, p.name, ls.agent_id, a.name, 'agent', COALESCE(a.name, ls.agent_id),
        'task', t.id, t.title, '/tasks/' || t.id,
        CASE WHEN NEW.event_type = 'attempt_failed' THEN 'error'
             WHEN NEW.event_type = 'elicitation_pending' THEN 'warning' ELSE 'info' END,
        CASE WHEN NEW.event_type IN ('elicitation_pending', 'attempt_failed') THEN 1 ELSE 0 END,
        CASE WHEN NEW.event_type = 'attempt_failed' THEN 'Agent attempt failed'
             WHEN NEW.event_type = 'elicitation_pending' THEN 'The Agent needs your input'
             ELSE substr(trim(replace(replace(NEW.summary, char(10), ' '), char(13), ' ')), 1, 240)
        END,
        'attempt', NEW.attempt_id
    FROM tasks t
    JOIN logical_sessions ls ON ls.id = NEW.session_id
    LEFT JOIN projects p ON p.id = t.project_id
    LEFT JOIN agents a ON a.id = ls.agent_id
    WHERE t.id = NEW.task_id AND t.hidden = 0;
END;

CREATE TRIGGER dashboard_conversation_turn_status
AFTER UPDATE OF status ON conversation_turns
WHEN NEW.status != OLD.status AND NEW.status IN ('completed', 'failed', 'cancelled')
BEGIN
    INSERT INTO dashboard_events (
        event_id, event_kind, occurred_at, project_id, project_name,
        agent_id, agent_name, source_kind, source_label,
        target_type, target_id, target_title, href, severity,
        needs_attention, preview, work_kind, work_id
    )
    SELECT
        'conversation-turn:' || NEW.id || ':' || NEW.status,
        CASE NEW.status WHEN 'completed' THEN 'completion'
             WHEN 'failed' THEN 'failure' ELSE 'cancellation' END,
        COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', NEW.completed_at),
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
        c.project_id, p.name, NEW.agent_id, a.name, 'agent', COALESCE(a.name, NEW.agent_id),
        'conversation', c.id, COALESCE(c.title, 'Untitled conversation'),
        '/conversations/' || c.id,
        CASE NEW.status WHEN 'failed' THEN 'error'
             WHEN 'completed' THEN 'success' ELSE 'info' END,
        CASE WHEN NEW.status = 'failed' THEN 1 ELSE 0 END,
        CASE NEW.status WHEN 'completed' THEN 'Response completed'
             WHEN 'failed' THEN 'Conversation response failed'
             ELSE 'Response cancelled' END,
        'conversation_turn', NEW.id
    FROM conversations c
    LEFT JOIN projects p ON p.id = c.project_id
    LEFT JOIN agents a ON a.id = NEW.agent_id
    WHERE c.id = NEW.conversation_id;
END;

CREATE TRIGGER dashboard_attempt_context_update
AFTER UPDATE OF context_used, context_size ON work_attempts
WHEN NEW.context_used IS NOT OLD.context_used OR NEW.context_size IS NOT OLD.context_size
BEGIN
    INSERT INTO dashboard_metric_points (
        work_kind, work_id, project_id, agent_id, bucket_at,
        context_used, context_size, recorded_at
    )
    SELECT 'attempt', NEW.id, t.project_id, ls.agent_id,
        strftime('%Y-%m-%dT%H:%M:', 'now') || printf('%02dZ', (CAST(strftime('%S', 'now') AS INTEGER) / 10) * 10),
        NEW.context_used, NEW.context_size, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
    FROM logical_sessions ls
    JOIN tasks t ON t.id = NEW.task_id AND t.hidden = 0 AND t.task_type != 'IDLE'
    WHERE ls.id = NEW.session_id
    ON CONFLICT(work_kind, work_id, bucket_at) DO UPDATE SET
        context_used = excluded.context_used,
        context_size = excluded.context_size,
        recorded_at = excluded.recorded_at;
END;

CREATE TRIGGER dashboard_conversation_context_update
AFTER UPDATE OF context_used, context_size ON conversation_turns
WHEN NEW.context_used IS NOT OLD.context_used OR NEW.context_size IS NOT OLD.context_size
BEGIN
    INSERT INTO dashboard_metric_points (
        work_kind, work_id, project_id, agent_id, bucket_at,
        context_used, context_size, recorded_at
    )
    SELECT 'conversation_turn', NEW.id, c.project_id, NEW.agent_id,
        strftime('%Y-%m-%dT%H:%M:', 'now') || printf('%02dZ', (CAST(strftime('%S', 'now') AS INTEGER) / 10) * 10),
        NEW.context_used, NEW.context_size, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
    FROM conversations c WHERE c.id = NEW.conversation_id
    ON CONFLICT(work_kind, work_id, bucket_at) DO UPDATE SET
        context_used = excluded.context_used,
        context_size = excluded.context_size,
        recorded_at = excluded.recorded_at;
END;

-- Tool starts remain exact for 1h/24h/7d charts even when the bounded feed
-- rolls older display rows off its 20,000-row replay window.
CREATE TRIGGER dashboard_tool_metric_insert
AFTER INSERT ON dashboard_events
WHEN NEW.event_kind = 'tool_call'
 AND NEW.work_kind IN ('attempt', 'conversation_turn')
 AND NEW.work_id IS NOT NULL
BEGIN
    INSERT INTO dashboard_metric_points (
        work_kind, work_id, project_id, agent_id, bucket_at,
        tool_calls, recorded_at
    ) VALUES (
        NEW.work_kind, NEW.work_id, NEW.project_id, NEW.agent_id,
        strftime('%Y-%m-%dT%H:%M:', NEW.occurred_at)
            || printf('%02dZ', (CAST(strftime('%S', NEW.occurred_at) AS INTEGER) / 10) * 10),
        1, strftime('%Y-%m-%dT%H:%M:%fZ', NEW.occurred_at)
    )
    ON CONFLICT(work_kind, work_id, bucket_at) DO UPDATE SET
        tool_calls = dashboard_metric_points.tool_calls + 1,
        recorded_at = excluded.recorded_at;
END;

-- Keep storage bounded even when nobody has the dashboard open. The snapshot
-- path also prunes, while this amortized trigger limits write-side cleanup to
-- once per 256 normalized feed rows.
CREATE TRIGGER dashboard_telemetry_retention
AFTER INSERT ON dashboard_events
WHEN NEW.cursor % 256 = 0
BEGIN
    DELETE FROM dashboard_events
    WHERE occurred_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-8 days')
       OR cursor <= COALESCE((SELECT MAX(cursor) FROM dashboard_events), 0) - 20000;
    DELETE FROM dashboard_metric_points
    WHERE bucket_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-8 days');
    DELETE FROM dashboard_git_baselines
    WHERE COALESCE(finalized_at, captured_at)
        < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-8 days');
END;

-- Seed a useful bounded history for upgraded installations without copying
-- unbounded message bodies or any sensitive tool payload.
INSERT INTO dashboard_events (
    event_id, event_kind, occurred_at, project_id, project_name,
    agent_id, agent_name, source_kind, source_label,
    target_type, target_id, target_title, href, severity,
    needs_attention, preview
)
SELECT 'task-message:' || tm.id,
       CASE WHEN tm.role = 'assistant' THEN 'agent_response' ELSE 'task_message' END,
       COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', tm.timestamp),
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
       t.project_id, p.name, t.agent_id, a.name,
       CASE WHEN tm.role = 'assistant' THEN 'agent' ELSE 'user' END,
       CASE WHEN tm.role = 'assistant' THEN COALESCE(a.name, 'Agent') ELSE 'You' END,
       'task', t.id, t.title, '/tasks/' || t.id, 'info', 0,
       CASE WHEN trim(tm.content) = '' THEN 'Image attachment'
            ELSE substr(trim(replace(replace(tm.content, char(10), ' '), char(13), ' ')), 1, 240) END
FROM task_messages tm
JOIN tasks t ON t.id = tm.task_id AND t.hidden = 0
LEFT JOIN projects p ON p.id = t.project_id
LEFT JOIN agents a ON a.id = t.agent_id
WHERE tm.id IN (
    SELECT id FROM task_messages
    WHERE role IN ('user', 'assistant')
    ORDER BY id DESC LIMIT 500
)
  AND tm.role IN ('user', 'assistant')
  AND (tm.role != 'assistant' OR trim(tm.content) != '');

INSERT INTO dashboard_events (
    event_id, event_kind, occurred_at, project_id, project_name,
    agent_id, agent_name, source_kind, source_label,
    target_type, target_id, target_title, href, severity,
    needs_attention, preview
)
SELECT 'conversation-message:' || cm.id,
       CASE WHEN cm.sender_type = 'agent' THEN 'agent_response' ELSE 'conversation_message' END,
       COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', cm.created_at),
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
       c.project_id, p.name,
       CASE WHEN cm.sender_type = 'agent' THEN cm.sender_id ELSE NULL END,
       CASE WHEN cm.sender_type = 'agent' THEN COALESCE(cm.sender_name, a.name) ELSE NULL END,
       cm.sender_type,
       COALESCE(cm.sender_name, CASE WHEN cm.sender_type = 'user' THEN 'You' ELSE cm.sender_id END),
       'conversation', c.id, COALESCE(c.title, 'Untitled conversation'),
       '/conversations/' || c.id, 'info', 0,
       CASE WHEN trim(cm.content) = '' THEN 'File attachment'
            ELSE substr(trim(replace(replace(cm.content, char(10), ' '), char(13), ' ')), 1, 240) END
FROM conversation_messages cm
JOIN conversations c ON c.id = cm.conversation_id
LEFT JOIN projects p ON p.id = c.project_id
LEFT JOIN agents a ON a.id = cm.sender_id AND cm.sender_type = 'agent'
WHERE cm.id IN (
    SELECT id FROM conversation_messages
    WHERE sender_type IN ('user', 'agent')
    ORDER BY id DESC LIMIT 500
)
  AND cm.sender_type IN ('user', 'agent');

INSERT INTO dashboard_events (
    event_id, event_kind, occurred_at, project_id, project_name,
    agent_id, agent_name, source_kind, source_label,
    target_type, target_id, target_title, href, severity,
    needs_attention, preview, work_kind, work_id
)
SELECT 'tool-call:legacy:' || se.id, 'tool_call',
       COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', se.created_at),
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
       t.project_id, p.name, ls.agent_id, a.name,
       'agent', COALESCE(a.name, ls.agent_id),
       'task', t.id, t.title, '/tasks/' || t.id, 'info', 0,
       'Used an Agent tool',
       'attempt', se.attempt_id
FROM session_events se
JOIN logical_sessions ls ON ls.id = se.session_id
JOIN tasks t ON t.id = se.task_id AND t.hidden = 0
LEFT JOIN projects p ON p.id = t.project_id
LEFT JOIN agents a ON a.id = ls.agent_id
WHERE se.event_type = 'tool_call'
  AND se.id IN (
      SELECT id FROM session_events
      WHERE event_type = 'tool_call'
      ORDER BY id DESC LIMIT 1000
  );
"#;

const MIGRATION_V42: &str = r#"
-- Codex inline visualizations are copied into the control-plane database at
-- final-message ingestion time. Exactly one message owns each artifact; the
-- attempt/turn columns retain execution provenance without making pruning an
-- accidental deletion boundary for a still-visible message.
CREATE TABLE message_visualizations (
    id TEXT PRIMARY KEY,
    task_message_id INTEGER REFERENCES task_messages(id) ON DELETE CASCADE,
    conversation_message_id INTEGER REFERENCES conversation_messages(id) ON DELETE CASCADE,
    attempt_id TEXT REFERENCES work_attempts(id) ON DELETE SET NULL,
    conversation_turn_id TEXT REFERENCES conversation_turns(id) ON DELETE SET NULL,
    reference_index INTEGER NOT NULL,
    title TEXT NOT NULL,
    display_mode TEXT NOT NULL DEFAULT 'normal'
        CHECK (display_mode IN ('normal', 'wide')),
    status TEXT NOT NULL
        CHECK (status IN ('ready', 'unavailable')),
    error_code TEXT,
    content TEXT,
    content_sha256 TEXT,
    size INTEGER,
    retrieval_token TEXT NOT NULL UNIQUE,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (task_message_id IS NOT NULL AND conversation_message_id IS NULL) OR
        (task_message_id IS NULL AND conversation_message_id IS NOT NULL)
    ),
    CHECK (
        (status = 'ready' AND content IS NOT NULL AND error_code IS NULL
          AND content_sha256 IS NOT NULL AND length(content_sha256) = 64
          AND size BETWEEN 1 AND 1048576
          AND length(CAST(content AS BLOB)) = size) OR
        (status = 'unavailable' AND content IS NULL AND error_code IS NOT NULL
          AND content_sha256 IS NULL AND size IS NULL)
    )
);
CREATE UNIQUE INDEX idx_message_visualizations_task_reference
    ON message_visualizations(task_message_id, reference_index)
    WHERE task_message_id IS NOT NULL;
CREATE UNIQUE INDEX idx_message_visualizations_conversation_reference
    ON message_visualizations(conversation_message_id, reference_index)
    WHERE conversation_message_id IS NOT NULL;
CREATE INDEX idx_message_visualizations_attempt
    ON message_visualizations(attempt_id);
CREATE INDEX idx_message_visualizations_turn
    ON message_visualizations(conversation_turn_id);
"#;

const MIGRATION_V43: &str = r#"
-- An Agent's bootstrap workspace remains the writable security boundary while
-- this local-only selection records the narrower repository used as ACP cwd.
-- Portable Project sync intentionally excludes this machine-specific path.
CREATE TABLE agent_repository_selections (
    agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    relative_path TEXT,
    repository_identity TEXT,
    selection_mode TEXT NOT NULL DEFAULT 'automatic'
        CHECK (selection_mode IN ('automatic', 'manual', 'cleared')),
    pending_relative_path TEXT,
    pending_selection_mode TEXT
        CHECK (pending_selection_mode IN ('manual', 'cleared')),
    generation INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    CHECK (relative_path IS NOT NULL OR repository_identity IS NULL),
    CHECK (selection_mode != 'cleared' OR relative_path IS NULL),
    CHECK (
        (pending_selection_mode IS NULL AND pending_relative_path IS NULL) OR
        (pending_selection_mode = 'manual' AND pending_relative_path IS NOT NULL) OR
        (pending_selection_mode = 'cleared' AND pending_relative_path IS NULL)
    )
);
CREATE INDEX idx_agent_repository_updated
    ON agent_repository_selections(updated_at);
"#;

const MIGRATION_V44: &str = r#"
-- Default task workflows are local automation policy. A workflow marked as a
-- default is attached once to each ordinary Agent task when its first attempt
-- is dispatched. The source-task link is both the continuation target and the
-- idempotency boundary for dispatcher retries and process recovery.
ALTER TABLE workflows ADD COLUMN default_for_tasks INTEGER NOT NULL DEFAULT 0
    CHECK (default_for_tasks IN (0, 1));
ALTER TABLE workflow_instances ADD COLUMN source_task_id TEXT
    REFERENCES tasks(id) ON DELETE CASCADE;
ALTER TABLE workflow_step_executions ADD COLUMN continuation_attempt_id TEXT
    REFERENCES work_attempts(id) ON DELETE SET NULL;
CREATE UNIQUE INDEX idx_workflow_instances_source_task
    ON workflow_instances(workflow_id, source_task_id)
    WHERE source_task_id IS NOT NULL;
CREATE INDEX idx_workflow_instances_source_task_lookup
    ON workflow_instances(source_task_id, status)
    WHERE source_task_id IS NOT NULL;
CREATE UNIQUE INDEX idx_workflow_step_continuation_attempt
    ON workflow_step_executions(continuation_attempt_id)
    WHERE continuation_attempt_id IS NOT NULL;
"#;

const MIGRATION_V45: &str = r#"
-- Retain the exact fixed prompt owned by a same-task continuation even when
-- a later user answer adopts the execution's active attempt. Workflow
-- cancellation can then distinguish its own unstarted prompt from user text.
ALTER TABLE workflow_step_executions ADD COLUMN continuation_prompt_message_id INTEGER
    REFERENCES task_messages(id) ON DELETE SET NULL;
"#;

const MIGRATION_V46: &str = r#"
-- Conversation messages are synchronized immutable records. A deletion marker
-- hides a message everywhere without allowing a later Project fetch to revive
-- it from an older checkout.
ALTER TABLE conversation_messages ADD COLUMN deleted_at TIMESTAMP;
UPDATE conversation_messages
   SET deleted_at = json_extract(metadata, '$.xpressclaw_deleted_at'),
       processed = 1
 WHERE json_valid(metadata)
   AND json_type(metadata, '$.xpressclaw_deleted_at') = 'text';
DELETE FROM conversation_message_attachments
 WHERE message_id IN (
       SELECT id FROM conversation_messages WHERE deleted_at IS NOT NULL
 );
DELETE FROM message_visualizations
 WHERE conversation_message_id IN (
       SELECT id FROM conversation_messages WHERE deleted_at IS NOT NULL
 );
DELETE FROM dashboard_events
 WHERE event_id IN (
       SELECT 'conversation-message:' || id
       FROM conversation_messages WHERE deleted_at IS NOT NULL
 );
UPDATE conversations
   SET last_message_at = (
       SELECT created_at FROM conversation_messages
        WHERE conversation_id = conversations.id AND deleted_at IS NULL
        ORDER BY julianday(created_at) DESC, id DESC LIMIT 1
   )
 WHERE EXISTS (
       SELECT 1 FROM conversation_messages
        WHERE conversation_id = conversations.id AND deleted_at IS NOT NULL
 );
CREATE INDEX idx_conversation_messages_visible
    ON conversation_messages(conversation_id, id)
    WHERE deleted_at IS NULL;
UPDATE conversation_agent_sessions
   SET status = 'idle', last_error = NULL, updated_at = CURRENT_TIMESTAMP
 WHERE status = 'failed'
   AND NOT EXISTS (
       SELECT 1 FROM conversation_turns turn
       WHERE turn.conversation_id = conversation_agent_sessions.conversation_id
         AND turn.agent_id = conversation_agent_sessions.agent_id
         AND turn.status = 'failed'
   );

UPDATE dashboard_events
   SET needs_attention = 0
 WHERE target_type = 'task'
   AND needs_attention = 1
   AND NOT EXISTS (
       SELECT 1 FROM tasks task
        WHERE task.id = dashboard_events.target_id
          AND task.status IN ('waiting_for_input', 'blocked')
   );
UPDATE dashboard_events
   SET needs_attention = 0
 WHERE target_type = 'conversation'
   AND needs_attention = 1
   AND NOT EXISTS (
       SELECT 1 FROM conversation_agent_sessions session
        WHERE session.conversation_id = dashboard_events.target_id
          AND session.agent_id = dashboard_events.agent_id
          AND session.status = 'failed'
   );

-- Attention is current state, not permanent history. Keep the activity row as
-- an audit trail, but stop presenting it as unresolved once the user clears the
-- underlying task or Conversation failure.
CREATE TRIGGER dashboard_task_attention_clear
AFTER UPDATE OF status ON tasks
WHEN OLD.status IN ('waiting_for_input', 'blocked')
 AND NEW.status NOT IN ('waiting_for_input', 'blocked')
BEGIN
    UPDATE dashboard_events
       SET needs_attention = 0
     WHERE target_type = 'task' AND target_id = NEW.id AND needs_attention = 1;
END;

CREATE TRIGGER dashboard_conversation_attention_clear
AFTER UPDATE OF status ON conversation_agent_sessions
WHEN OLD.status = 'failed' AND NEW.status != 'failed'
BEGIN
    UPDATE dashboard_events
       SET needs_attention = 0
     WHERE target_type = 'conversation'
       AND target_id = NEW.conversation_id
       AND agent_id = NEW.agent_id
       AND needs_attention = 1;
END;

CREATE TRIGGER dashboard_conversation_message_delete
AFTER UPDATE OF deleted_at ON conversation_messages
WHEN OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL
BEGIN
    DELETE FROM dashboard_events WHERE event_id = 'conversation-message:' || NEW.id;
END;
"#;

const MIGRATION_V39: &str = "
-- Cascading Project deletion is a recoverable two-phase operation. The
-- durable marker is set before workers and retained runtimes are stopped, so
-- no new work can attach while asynchronous cleanup is in progress. A failed
-- cleanup can be retried without making a bare DELETE destructive.
ALTER TABLE projects ADD COLUMN deletion_started_at TIMESTAMP;
";

const MIGRATION_V40: &str = "
-- A workflow can deliberately remain projectless while selecting Agents that
-- belong to Projects. Persist that Agent-derived scope so taskless waits and
-- pre-task runs still participate in Project lifecycle guards and deletion.
-- agent_id is an immutable provenance snapshot rather than a foreign key: it
-- must remain available until the owning workflow instance is removed, even
-- while the Agent itself is being deleted.
CREATE TABLE workflow_instance_agent_bindings (
    instance_id TEXT NOT NULL
        REFERENCES workflow_instances(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    project_id TEXT NOT NULL
        REFERENCES projects(id) ON DELETE CASCADE,
    PRIMARY KEY (instance_id, agent_id, project_id)
);
CREATE INDEX idx_workflow_instance_agent_bindings_project
    ON workflow_instance_agent_bindings(project_id, instance_id);

-- Existing taskless event waits persist their selected Agent in input_context.
-- Recover that ownership without making runtime lifecycle queries depend on
-- parsing JSON after this one-time migration. The Rust migration pass also
-- resolves every typed-input and future-step Agent from each saved run.
INSERT OR IGNORE INTO workflow_instance_agent_bindings
    (instance_id, agent_id, project_id)
SELECT execution.instance_id,
       json_extract(execution.input_context, '$.agent_id'),
       agent.project_id
FROM workflow_step_executions execution
JOIN agents agent
  ON agent.id = CASE
      WHEN json_valid(execution.input_context)
      THEN json_extract(execution.input_context, '$.agent_id')
  END
WHERE execution.task_id IS NULL
  AND execution.input_context IS NOT NULL
  AND json_valid(execution.input_context)
  AND CASE
      WHEN json_valid(execution.input_context)
      THEN json_type(execution.input_context, '$.agent_id')
  END = 'text'
  AND agent.project_id IS NOT NULL;
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
        (36, MIGRATION_V36),
        (37, MIGRATION_V37),
        (38, MIGRATION_V38),
        (39, MIGRATION_V39),
        (40, MIGRATION_V40),
        (41, MIGRATION_V41),
        (42, MIGRATION_V42),
        (43, MIGRATION_V43),
        (44, MIGRATION_V44),
        (45, MIGRATION_V45),
        (46, MIGRATION_V46),
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
        assert_eq!(version, "46");
        let visualization_table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'message_visualizations'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(visualization_table, 1);
        let deletion_marker: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('projects')
                 WHERE name = 'deletion_started_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(deletion_marker, 1);
        let workflow_agent_scope: i64 = conn
            .query_row(
                "SELECT COUNT(*)
                 FROM pragma_table_info('workflow_instance_agent_bindings')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(workflow_agent_scope, 3);
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
    fn v46_adopts_synced_message_tombstones_and_removes_local_artifacts() {
        ensure_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        register_sql_functions(&conn).unwrap();
        for &(target, sql) in schema_migrations() {
            if target > 45 {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            r#"INSERT INTO agents (id, name, backend, config)
               VALUES ('atlas', 'Atlas', 'native', '{}'),
                      ('beta', 'Beta', 'native', '{}');
               INSERT INTO conversations (id, title, last_message_at)
               VALUES ('conversation', 'Upgrade', '2026-01-02 00:00:00');
               INSERT INTO conversation_participants
                   (conversation_id, participant_type, participant_id)
               VALUES ('conversation', 'agent', 'atlas'),
                      ('conversation', 'agent', 'beta');
               INSERT INTO conversation_messages
                   (conversation_id, sender_type, sender_id, content, metadata,
                    created_at, processed)
               VALUES
                   ('conversation', 'user', 'local', 'Keep', '{}',
                    '2026-01-01 00:00:00', 1),
                   ('conversation', 'user', 'local', 'Remove',
                    '{"xpressclaw_deleted_at":"2026-01-03 00:00:00"}',
                    '2026-01-02 00:00:00', 0),
                   ('conversation', 'user', 'local', 'Keep this follow-up', '{}',
                    '2026-01-04 00:00:00', 1);
               INSERT INTO conversation_agent_sessions
                   (conversation_id, agent_id, native_session_id, status)
               VALUES ('conversation', 'atlas', 'tainted-atlas-session', 'running'),
                      ('conversation', 'beta', 'tainted-beta-session', 'queued');
               INSERT INTO conversation_turns
                   (id, conversation_id, agent_id, trigger_message_id, status)
               VALUES ('running-turn', 'conversation', 'atlas', 3, 'running'),
                      ('queued-turn', 'conversation', 'beta', 2, 'queued');
               INSERT INTO conversation_message_attachments
                   (id, message_id, name, mime_type, data, size)
               VALUES ('attachment', 2, 'note.txt', 'text/plain', X'78', 1);
               INSERT INTO message_visualizations
                   (id, conversation_message_id, reference_index, title, status,
                    error_code, retrieval_token)
               VALUES ('visualization', 2, 0, 'Old artifact', 'unavailable',
                       'missing', 'retrieval-token');
               INSERT INTO dashboard_events
                   (event_id, event_kind, target_type, target_id, target_title, href)
               VALUES ('conversation-message:2', 'conversation_message',
                       'conversation', 'conversation', 'Upgrade', '/conversations/conversation');"#,
        )
        .unwrap();

        conn.execute_batch(MIGRATION_V46).unwrap();
        reconcile_adopted_conversation_tombstones(&conn).unwrap();

        let adopted: (Option<String>, String, bool) = conn
            .query_row(
                "SELECT deleted_at, metadata, processed
                 FROM conversation_messages WHERE id = 2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(adopted.0.as_deref(), Some("2026-01-03 00:00:00"));
        assert_eq!(adopted.1, "{}");
        assert!(adopted.2);
        let cleanup: (i64, i64, i64, Option<String>) = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM conversation_message_attachments),
                    (SELECT COUNT(*) FROM message_visualizations),
                    (SELECT COUNT(*) FROM dashboard_events
                      WHERE event_id = 'conversation-message:2'),
                    (SELECT last_message_at FROM conversations WHERE id = 'conversation')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(cleanup, (0, 0, 0, Some("2026-01-04 00:00:00".into())));

        let reconciliation: (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM conversation_agent_sessions
                      WHERE native_session_id IS NULL AND status = 'queued'),
                    (SELECT COUNT(*) FROM conversation_turns
                      WHERE agent_id = 'atlas' AND trigger_message_id = 3
                        AND status = 'cancelled'),
                    (SELECT COUNT(*) FROM conversation_turns
                      WHERE agent_id = 'atlas' AND trigger_message_id = 3
                        AND status = 'queued'),
                    (SELECT COUNT(*) FROM conversation_turns
                      WHERE agent_id = 'beta' AND trigger_message_id = 1
                        AND status = 'queued')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            reconciliation,
            (2, 1, 1, 1),
            "migration should clear both sessions, requeue later running work, and retarget queued work"
        );
    }

    #[test]
    fn v46_backfills_resolved_dashboard_attention() {
        ensure_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        register_sql_functions(&conn).unwrap();
        for &(target, sql) in schema_migrations() {
            if target > 45 {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            r#"INSERT INTO projects (id, name) VALUES ('project', 'Project');
               INSERT INTO agents (id, name, backend, config, project_id)
               VALUES ('atlas', 'Atlas', 'native', '{}', 'project');
               INSERT INTO tasks (id, title, status, agent_id, project_id)
               VALUES ('resolved-task', 'Resolved task', 'completed', 'atlas', 'project'),
                      ('active-task', 'Active task', 'blocked', 'atlas', 'project');
               INSERT INTO conversations (id, project_id, title)
               VALUES ('resolved-conversation', 'project', 'Resolved conversation'),
                      ('active-conversation', 'project', 'Active conversation');
               INSERT INTO conversation_agent_sessions
                   (conversation_id, agent_id, status, last_error)
               VALUES ('resolved-conversation', 'atlas', 'failed', 'Old failure'),
                      ('active-conversation', 'atlas', 'failed', 'Current failure');
               INSERT INTO conversation_turns
                   (id, conversation_id, agent_id, status, error_message)
               VALUES ('active-turn', 'active-conversation', 'atlas', 'failed', 'Current failure');
               INSERT INTO dashboard_events
                   (event_id, event_kind, target_type, target_id, target_title,
                    href, agent_id, needs_attention)
               VALUES ('resolved-task-event', 'failure', 'task', 'resolved-task',
                       'Resolved task', '/tasks/resolved-task', 'atlas', 1),
                      ('active-task-event', 'failure', 'task', 'active-task',
                       'Active task', '/tasks/active-task', 'atlas', 1),
                      ('resolved-conversation-event', 'failure', 'conversation',
                       'resolved-conversation', 'Resolved conversation',
                       '/conversations/resolved-conversation', 'atlas', 1),
                      ('active-conversation-event', 'failure', 'conversation',
                       'active-conversation', 'Active conversation',
                       '/conversations/active-conversation', 'atlas', 1);"#,
        )
        .unwrap();

        conn.execute_batch(MIGRATION_V46).unwrap();

        let attention = conn
            .prepare(
                "SELECT event_id, needs_attention FROM dashboard_events
                 WHERE event_id LIKE '%-event' ORDER BY event_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            attention,
            vec![
                ("active-conversation-event".into(), true),
                ("active-task-event".into(), true),
                ("resolved-conversation-event".into(), false),
                ("resolved-task-event".into(), false),
            ]
        );
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
    fn v37_backfills_native_plan_provenance_and_non_blocking_policy() {
        ensure_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        register_sql_functions(&conn).unwrap();
        for &(target, sql) in schema_migrations() {
            if target > 36 {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            r#"INSERT INTO tasks (id, title, context)
               VALUES ('parent', 'Parent', '{}');
               INSERT INTO tasks (id, title, parent_task_id, context)
               VALUES (
                   'plan',
                   'Address any further review feedback through approval or merge',
                   'parent',
                   '{"origin":"native_plan","attempt_id":"attempt-1","index":0}'
               ), (
                   'delegated',
                   'Run durable delegated work',
                   'parent',
                   '{"origin":"delegated"}'
               ), (
                   'copied-plan-context',
                   'Explicit work with copied internal context',
                   'parent',
                   '{"origin":"native_plan"}'
               );"#,
        )
        .unwrap();

        conn.execute_batch(MIGRATION_V37).unwrap();

        let plan: (String, bool) = conn
            .query_row(
                "SELECT provenance, blocks_parent FROM tasks WHERE id = 'plan'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(plan, ("native_plan".to_string(), false));
        let delegated: (String, bool) = conn
            .query_row(
                "SELECT provenance, blocks_parent FROM tasks WHERE id = 'delegated'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(delegated, ("delegated".to_string(), true));
        let copied_context: (String, bool) = conn
            .query_row(
                "SELECT provenance, blocks_parent FROM tasks WHERE id = 'copied-plan-context'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(copied_context, ("durable".to_string(), true));
    }

    #[test]
    fn v38_backfills_response_phase_timestamps_without_rewriting_legacy_history() {
        ensure_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        register_sql_functions(&conn).unwrap();
        for &(target, sql) in schema_migrations() {
            if target > 37 {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO projects (id, name) VALUES ('p', 'Project');
             INSERT INTO agents (id, name, backend, config, project_id)
             VALUES ('atlas', 'Atlas', 'native', '{}', 'p');
             INSERT INTO tasks (id, title, project_id)
             VALUES ('task', 'Investigate', 'p');
             INSERT INTO logical_sessions (id, agent_id)
             VALUES ('atlas', 'atlas');
             INSERT INTO work_attempts
                 (id, session_id, task_id, runner, status, prompt, created_at, started_at)
             VALUES
                 ('attempt', 'atlas', 'task', 'codex', 'running', 'Investigate',
                  '2026-08-16 10:00:00', '2026-08-16 10:00:05');
             INSERT INTO conversations (id, title, project_id)
             VALUES ('conversation', 'Discuss', 'p');
             INSERT INTO conversation_turns
                 (id, conversation_id, agent_id, status, queued_at, started_at)
             VALUES
                 ('turn', 'conversation', 'atlas', 'running',
                  '2026-08-16 11:00:00', '2026-08-16 11:00:07');",
        )
        .unwrap();

        conn.execute_batch(MIGRATION_V38).unwrap();

        let attempt: (String, String, String, String) = conn
            .query_row(
                "SELECT created_at, started_at, response_queued_at, response_started_at
                 FROM work_attempts WHERE id = 'attempt'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            attempt,
            (
                "2026-08-16 10:00:00".into(),
                "2026-08-16 10:00:05".into(),
                "2026-08-16 10:00:00".into(),
                "2026-08-16 10:00:05".into(),
            )
        );
        let turn: (String, String, String, String) = conn
            .query_row(
                "SELECT queued_at, started_at, response_queued_at, response_started_at
                 FROM conversation_turns WHERE id = 'turn'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            turn,
            (
                "2026-08-16 11:00:00".into(),
                "2026-08-16 11:00:07".into(),
                "2026-08-16 11:00:00".into(),
                "2026-08-16 11:00:07".into(),
            )
        );
    }

    #[test]
    fn v40_backfills_every_resolvable_workflow_agent_scope() {
        ensure_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        register_sql_functions(&conn).unwrap();
        for &(target, sql) in schema_migrations() {
            if target > 39 {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            r#"INSERT INTO projects (id, name)
               VALUES ('p-current', 'Current'),
                      ('p-input', 'Input'),
                      ('p-future', 'Future');
               INSERT INTO agents (id, name, backend, config, project_id)
               VALUES ('atlas', 'Atlas', 'native', '{}', 'p-current'),
                      ('reviewer', 'Reviewer', 'native', '{}', 'p-input'),
                      ('future-agent', 'Future Agent', 'native', '{}', 'p-future');
               INSERT INTO workflows (id, name, yaml_content)
               VALUES ('workflow', 'Workflow', 'name: Current
flows:
  main:
    steps:
      - id: no-agent
        prompt: Continue'),
                      ('unresolved', 'Unresolved', 'name: Unresolved
flows:
  main:
    steps:
      - id: known
        agent: atlas
        prompt: Continue
      - id: future
        agent: "{{future.agent_id}}"
        prompt: Continue'),
                      ('missing', 'Missing', 'name: Missing
flows:
  main:
    steps:
      - id: known
        agent: atlas
        prompt: Continue
      - id: future
        agent: missing-agent
        prompt: Continue');
               INSERT INTO workflow_instances
                   (id, workflow_id, status, trigger_data, definition_yaml)
               VALUES ('valid', 'workflow', 'waiting', '{"reviewer":"reviewer"}',
                       'name: Snapshot
inputs:
  reviewer:
    type: agent
flows:
  main:
    steps:
      - id: current
        type: wait
        agent: atlas
        event: github.pull_request.activity
        resource: https://github.com/example/repo/pull/1
      - id: input
        agent: "@reviewer"
        prompt: Review
  later:
    steps:
      - id: future
        agent: future-agent
        prompt: Continue'),
                      ('malformed', 'workflow', 'waiting', '{}', NULL),
                      ('unknown', 'workflow', 'waiting', '{}', NULL),
                      ('unresolved-active', 'unresolved', 'waiting', '{}', NULL),
                      ('unresolved-complete', 'unresolved', 'completed', '{}', NULL),
                      ('missing-active', 'missing', 'running', '{}', NULL);
               INSERT INTO workflow_instances
                   (id, workflow_id, project_id, status, trigger_data, definition_yaml)
               VALUES ('scoped-valid', 'workflow', 'p-current', 'waiting', '{}',
                       'name: Scoped valid
flows:
  main:
    steps:
      - id: implement
        agent: atlas
        prompt: Continue'),
                      ('scoped-conflict', 'workflow', 'p-current', 'waiting',
                       '{"trigger":{"future_agent":"reviewer"}}',
                       'name: Scoped conflict
flows:
  main:
    steps:
      - id: implement
        agent: atlas
        prompt: Continue
  later:
    steps:
      - id: future-review
        agent: "{{trigger.future_agent}}"
        prompt: Review'),
                      ('scoped-conflict-complete', 'workflow', 'p-current', 'completed', '{}',
                       'name: Completed scoped conflict
flows:
  main:
    steps:
      - id: review
        agent: reviewer
        prompt: Review');
               INSERT INTO workflow_step_executions
                   (id, instance_id, flow_name, step_id, task_id, status, input_context)
               VALUES ('valid-wait', 'valid', 'main', 'review', NULL, 'waiting',
                       '{"agent_id":"atlas"}'),
                      ('malformed-wait', 'malformed', 'main', 'review', NULL, 'waiting',
                       '{'),
                      ('unknown-wait', 'unknown', 'main', 'review', NULL, 'waiting',
                       '{"agent_id":"missing"}');"#,
        )
        .unwrap();

        let transaction = conn.unchecked_transaction().unwrap();
        transaction.execute_batch(MIGRATION_V40).unwrap();
        backfill_workflow_agent_bindings(&transaction).unwrap();
        transaction.commit().unwrap();

        let bindings = conn
            .prepare(
                "SELECT instance_id, agent_id, project_id
                 FROM workflow_instance_agent_bindings ORDER BY instance_id, agent_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            bindings,
            vec![
                (
                    "missing-active".to_string(),
                    "atlas".to_string(),
                    "p-current".to_string(),
                ),
                (
                    "scoped-conflict".to_string(),
                    "atlas".to_string(),
                    "p-current".to_string(),
                ),
                (
                    "scoped-conflict".to_string(),
                    "reviewer".to_string(),
                    "p-input".to_string(),
                ),
                (
                    "scoped-conflict-complete".to_string(),
                    "reviewer".to_string(),
                    "p-input".to_string(),
                ),
                (
                    "scoped-valid".to_string(),
                    "atlas".to_string(),
                    "p-current".to_string(),
                ),
                (
                    "unresolved-active".to_string(),
                    "atlas".to_string(),
                    "p-current".to_string(),
                ),
                (
                    "unresolved-complete".to_string(),
                    "atlas".to_string(),
                    "p-current".to_string(),
                ),
                (
                    "valid".to_string(),
                    "atlas".to_string(),
                    "p-current".to_string(),
                ),
                (
                    "valid".to_string(),
                    "future-agent".to_string(),
                    "p-future".to_string(),
                ),
                (
                    "valid".to_string(),
                    "reviewer".to_string(),
                    "p-input".to_string(),
                ),
            ]
        );
        let statuses = conn
            .prepare(
                "SELECT id, status, error_message FROM workflow_instances
                 WHERE id IN ('valid', 'unresolved-active', 'unresolved-complete', 'missing-active',
                              'scoped-valid', 'scoped-conflict', 'scoped-conflict-complete')
                 ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            statuses,
            vec![
                (
                    "missing-active".into(),
                    "cancelled".into(),
                    Some(UNRECOVERABLE_WORKFLOW_BINDINGS_ERROR.into()),
                ),
                (
                    "scoped-conflict".into(),
                    "cancelled".into(),
                    Some(CONFLICTING_WORKFLOW_BINDINGS_ERROR.into()),
                ),
                ("scoped-conflict-complete".into(), "completed".into(), None,),
                ("scoped-valid".into(), "waiting".into(), None),
                (
                    "unresolved-active".into(),
                    "cancelled".into(),
                    Some(UNRECOVERABLE_WORKFLOW_BINDINGS_ERROR.into()),
                ),
                ("unresolved-complete".into(), "completed".into(), None),
                ("valid".into(), "waiting".into(), None),
            ]
        );
    }

    #[test]
    fn v41_backfills_only_user_and_agent_authored_messages() {
        ensure_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        register_sql_functions(&conn).unwrap();
        for &(target, sql) in schema_migrations() {
            if target > 40 {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO projects (id, name) VALUES ('p', 'Project');
             INSERT INTO agents (id, name, backend, config, project_id)
             VALUES ('atlas', 'Atlas', 'native', '{}', 'p');
             INSERT INTO tasks (id, title, agent_id, project_id)
             VALUES ('task', 'Investigate', 'atlas', 'p');
             INSERT INTO task_messages (task_id, role, content)
             VALUES ('task', 'system', 'Private generated orchestration prompt'),
                    ('task', 'user', 'Please investigate'),
                    ('task', 'assistant', 'I found the cause');
             INSERT INTO conversations (id, title, project_id)
             VALUES ('conversation', 'Discuss', 'p');
             INSERT INTO conversation_messages
                 (conversation_id, sender_type, sender_id, sender_name, content)
             VALUES
                 ('conversation', 'system', 'scheduler', 'Scheduler',
                  'Private scheduled wake-up instructions'),
                 ('conversation', 'user', 'user', 'You', 'Can you check this?'),
                 ('conversation', 'agent', 'atlas', 'Atlas', 'I am checking it now');",
        )
        .unwrap();

        conn.execute_batch(MIGRATION_V41).unwrap();

        let previews = conn
            .prepare("SELECT preview FROM dashboard_events ORDER BY cursor")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            previews,
            vec![
                "Please investigate",
                "I found the cause",
                "Can you check this?",
                "I am checking it now"
            ]
        );
    }

    #[test]
    fn v34_merges_agents_and_adopts_every_task_in_their_hierarchy() {
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
             INSERT INTO tasks (id, title)
             VALUES ('root', 'Unassigned root');
             INSERT INTO tasks (id, title, agent_id, parent_task_id)
             VALUES ('parent', 'Atlas parent', 'atlas', 'root');
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

        let projects_after_v33: Vec<(String, Option<String>)> = conn
            .prepare("SELECT id, project_id FROM tasks ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            projects_after_v33,
            vec![
                ("grandchild".into(), Some("builder".into())),
                ("parent".into(), Some("atlas".into())),
                ("reported-step".into(), Some("atlas".into())),
                ("root".into(), None),
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
        assert_eq!(task_projects, vec!["atlas"; 4]);
    }

    #[test]
    fn legacy_consolidation_handles_a_large_task_hierarchy_without_pairwise_closure() {
        let db = Database::open_memory().unwrap();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('atlas', 'Atlas'), ('builder', 'Builder');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'atlas'),
                        ('builder', 'Builder', 'native', '{}', 'builder');",
            )?;
            let transaction = conn.unchecked_transaction()?;
            {
                let mut statement = transaction.prepare(
                    "INSERT INTO tasks (id, title, parent_task_id, agent_id, project_id)
                     VALUES (?1, ?1, ?2, ?3, ?4)",
                )?;
                let mut parent = None::<String>;
                for index in 0..2_000 {
                    let id = format!("task-{index:04}");
                    let (agent_id, project_id) = match index {
                        0 => (Some("atlas"), Some("atlas")),
                        1_999 => (Some("builder"), Some("builder")),
                        _ => (None, None),
                    };
                    statement.execute(rusqlite::params![id, parent, agent_id, project_id])?;
                    parent = Some(format!("task-{index:04}"));
                }
            }
            consolidate_legacy_conversation_projects(&transaction)?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })
        .unwrap();

        db.with_conn(|conn| {
            let adopted: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM tasks WHERE project_id = 'atlas'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(adopted, 2_000);
            let builder_project: String = conn
                .query_row(
                    "SELECT project_id FROM agents WHERE id = 'builder'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(builder_project, "atlas");
        });
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
        assert!(tables.contains(&"conversation_message_sync".to_string()));
        assert!(tables.contains(&"task_message_sync".to_string()));
        assert!(tables.contains(&"project_workflows".to_string()));
        assert!(tables.contains(&"project_sync_state".to_string()));
        assert!(tables.contains(&"dashboard_events".to_string()));
        assert!(tables.contains(&"dashboard_metric_points".to_string()));
        assert!(tables.contains(&"dashboard_git_baselines".to_string()));
        assert!(tables.contains(&"workflow_instance_agent_bindings".to_string()));
    }
}
