use std::sync::Arc;

use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::{task_search_key, Database};
use crate::error::{Error, Result};
use crate::memory::project::move_project_memory;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub agent_ids: Vec<String>,
    pub conversation_count: i64,
    pub task_count: i64,
    pub deletion_started_at: Option<String>,
    #[serde(default)]
    pub deletion_counts: ProjectDeletionCounts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectDeletionCounts {
    pub agents: i64,
    pub tasks: i64,
    pub task_messages: i64,
    pub conversations: i64,
    pub conversation_messages: i64,
    pub memory_notes: i64,
    pub workflow_runs: i64,
    pub schedules: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDeletionAgent {
    pub id: String,
    pub name: String,
    pub container_id: Option<String>,
}

/// Durable records whose asynchronous runtime owners must be stopped before
/// [`ProjectManager::finish_cascade`] removes the Project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDeletionPlan {
    pub project_id: String,
    pub project_name: String,
    pub agents: Vec<ProjectDeletionAgent>,
    pub conversation_ids: Vec<String>,
    pub task_ids: Vec<String>,
    pub active_attempt_ids: Vec<String>,
    pub active_turn_ids: Vec<String>,
    pub app_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProject {
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProject {
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
}

pub struct ProjectManager {
    db: Arc<Database>,
}

/// Reject new Project-owned work after the first phase of cascading deletion.
///
/// Callers use this inside the same write transaction that attaches the new
/// record, so validation cannot race the deletion marker.
pub(crate) fn ensure_project_accepts_work(
    conn: &rusqlite::Connection,
    project_id: &str,
) -> Result<()> {
    let deletion_started_at = conn
        .query_row(
            "SELECT deletion_started_at FROM projects WHERE id = ?1",
            [project_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    match deletion_started_at {
        None => Err(Error::ProjectNotFound {
            id: project_id.to_string(),
        }),
        Some(Some(_)) => Err(Error::Project(format!(
            "Project '{project_id}' is being deleted and cannot accept new work; retry the confirmed deletion or choose another Project"
        ))),
        Some(None) => Ok(()),
    }
}

impl ProjectManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn ensure_accepting_work(&self, id: &str) -> Result<()> {
        self.db
            .with_conn(|conn| ensure_project_accepts_work(conn, id))
    }

    pub fn create(&self, request: &CreateProject) -> Result<Project> {
        let name = request.name.trim();
        if name.is_empty() {
            return Err(Error::Project("project name cannot be empty".into()));
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects (id, name, description, icon, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                rusqlite::params![id, name, request.description, request.icon, now],
            )
        })?;
        self.get(&id)
    }

    pub fn list(&self) -> Result<Vec<Project>> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT p.*,
                        (SELECT COUNT(*) FROM conversations c WHERE c.project_id = p.id) AS conversation_count,
                        (SELECT COUNT(*) FROM tasks t WHERE t.project_id = p.id AND t.hidden = 0) AS task_count
                 FROM projects p
                 ORDER BY p.updated_at DESC, p.name COLLATE NOCASE ASC",
            )?;
            let projects = statement
                .query_map([], |row| row_to_project(conn, row))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(projects)
        })
    }

    pub fn get(&self, id: &str) -> Result<Project> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT p.*,
                        (SELECT COUNT(*) FROM conversations c WHERE c.project_id = p.id) AS conversation_count,
                        (SELECT COUNT(*) FROM tasks t WHERE t.project_id = p.id AND t.hidden = 0) AS task_count
                 FROM projects p WHERE p.id = ?1",
                [id],
                |row| row_to_project(conn, row),
            )
            .map_err(|_| Error::ProjectNotFound { id: id.to_string() })
        })
    }

    /// Resolve a human-readable Project name or an exact canonical Project ID.
    ///
    /// Exact IDs take precedence over names so existing integrations remain
    /// deterministic even when a different Project happens to use that ID as
    /// its display name.
    pub fn resolve(&self, selector: &str) -> Result<Project> {
        let selector = selector.trim();
        if !selector.is_empty() {
            if let Ok(project) = self.get(selector) {
                return Ok(project);
            }
        }

        let projects = self.list()?;
        let selector_key = task_search_key(selector);
        let matches = projects
            .iter()
            .filter(|project| task_search_key(&project.name) == selector_key)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [project] => Ok((*project).clone()),
            [] => {
                let available = if projects.is_empty() {
                    "There are no local Projects yet; create one in the Projects UI first."
                        .to_string()
                } else {
                    let choices = projects
                        .iter()
                        .map(|project| format!("'{}' ({})", project.name, project.id))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("Available local Projects: {choices}.")
                };
                Err(Error::Project(format!(
                    "no local Project matches '{selector}' by name or exact ID. {available} Use `--project <NAME>` or copy a canonical ID from the Project page and use `--project-id <ID>`."
                )))
            }
            _ => {
                let ids = matches
                    .iter()
                    .map(|project| project.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(Error::Project(format!(
                    "Project name '{selector}' is ambiguous; matching canonical IDs: {ids}. Rerun with one of those IDs using `--project-id <ID>`."
                )))
            }
        }
    }

    pub fn update(&self, id: &str, request: &UpdateProject) -> Result<Project> {
        let current = self.get(id)?;
        let name = request.name.as_deref().unwrap_or(&current.name).trim();
        if name.is_empty() {
            return Err(Error::Project("project name cannot be empty".into()));
        }
        let description = request
            .description
            .as_ref()
            .or(current.description.as_ref());
        let icon = request.icon.as_ref().or(current.icon.as_ref());
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE projects
                 SET name = ?1, description = ?2, icon = ?3, updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?4",
                rusqlite::params![name, description, icon, id],
            )
        })?;
        self.get(id)
    }

    pub fn assign_agent(&self, project_id: &str, agent_id: &str) -> Result<Project> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_project_accepts_work(&transaction, project_id)?;
            let previous_project_id = transaction
                .query_row(
                    "SELECT project_id FROM agents WHERE id = ?1",
                    [agent_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(|_| Error::AgentNotFound {
                    name: agent_id.to_string(),
                })?;
            if let Some(previous_project_id) = previous_project_id.as_deref() {
                ensure_project_accepts_work(&transaction, previous_project_id)?;
            }
            if previous_project_id.as_deref() == Some(project_id) {
                transaction.commit()?;
                return Ok(());
            }

            let has_live_turn: bool = transaction.query_row(
                "SELECT
                    EXISTS(
                        SELECT 1 FROM work_attempts attempt
                        WHERE attempt.status IN ('preparing', 'running', 'review')
                          AND (
                              attempt.session_id = ?1
                              OR EXISTS (
                                  SELECT 1 FROM tasks task
                                  WHERE task.id = attempt.task_id
                                    AND task.agent_id = ?1
                              )
                          )
                    )
                    OR EXISTS(
                        SELECT 1 FROM conversation_turns turn
                        WHERE turn.agent_id = ?1 AND turn.status = 'running'
                    )",
                [agent_id],
                |row| row.get(0),
            )?;
            if has_live_turn {
                return Err(Error::Project(
                    "wait for this Agent's active task or Conversation response to finish before moving it"
                        .into(),
                ));
            }

            let has_active_workflow: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM workflow_instances instance
                    WHERE instance.status IN ('running', 'waiting')
                      AND (
                          instance.project_id = ?2
                          OR instance.conversation_id IN (
                              SELECT participant.conversation_id
                              FROM conversation_participants participant
                              WHERE participant.participant_type = 'agent'
                                AND participant.participant_id = ?1
                          )
                          OR EXISTS (
                              SELECT 1
                              FROM workflow_step_executions execution
                              JOIN tasks task ON task.id = execution.task_id
                              WHERE execution.instance_id = instance.id
                                AND task.agent_id = ?1
                          )
                      )
                )",
                rusqlite::params![agent_id, previous_project_id],
                |row| row.get(0),
            )?;
            if has_active_workflow {
                return Err(Error::Project(
                    "wait for or cancel this Agent's active Project workflow before moving it"
                        .into(),
                ));
            }

            let shared_conversation = transaction
                .query_row(
                    "SELECT c.id
                     FROM conversations c
                     JOIN conversation_participants own
                       ON own.conversation_id = c.id
                      AND own.participant_type = 'agent'
                      AND own.participant_id = ?1
                     WHERE c.project_id IS NOT ?2
                       AND EXISTS (
                           SELECT 1 FROM conversation_participants other
                           WHERE other.conversation_id = c.id
                             AND other.participant_type = 'agent'
                             AND other.participant_id <> ?1
                       )
                     LIMIT 1",
                    rusqlite::params![agent_id, project_id],
                    |row| row.get::<_, String>(0),
                )
                .ok();
            if shared_conversation.is_some() {
                return Err(Error::Project(
                    "this Agent belongs to a shared conversation in another project; move the whole conversation or remove the Agent from it first".into(),
                ));
            }

            let has_incompatible_linked_task: bool = transaction.query_row(
                "SELECT
                    EXISTS(
                        SELECT 1
                        FROM tasks task
                        JOIN conversations conversation ON conversation.id = task.conversation_id
                        JOIN conversation_participants participant
                          ON participant.conversation_id = conversation.id
                         AND participant.participant_type = 'agent'
                         AND participant.participant_id = ?2
                        WHERE task.agent_id IS NOT NULL
                          AND task.agent_id <> ?2
                          AND NOT EXISTS (
                              SELECT 1 FROM agents assigned
                              WHERE assigned.id = task.agent_id
                                AND assigned.project_id = ?1
                          )
                    )
                    OR EXISTS(
                        SELECT 1
                        FROM tasks task
                        JOIN conversations conversation ON conversation.id = task.conversation_id
                        WHERE task.agent_id = ?2
                          AND conversation.project_id IS NOT ?1
                          AND NOT EXISTS (
                              SELECT 1 FROM conversation_participants participant
                              WHERE participant.conversation_id = conversation.id
                                AND participant.participant_type = 'agent'
                                AND participant.participant_id = ?2
                          )
                    )",
                rusqlite::params![project_id, agent_id],
                |row| row.get(0),
            )?;
            if has_incompatible_linked_task {
                return Err(Error::Project(
                    "moving this Agent would split a linked task from its Agent or Conversation; reassign or unlink that task before moving the Agent".into(),
                ));
            }

            let has_incompatible_task_hierarchy: bool = transaction.query_row(
                "WITH task_destinations AS (
                    SELECT task.id,
                           task.parent_task_id,
                           task.project_id AS source_project_id,
                           CASE
                               WHEN task.agent_id = ?2
                                 OR task.conversation_id IN (
                                     SELECT conversation.id
                                     FROM conversations conversation
                                     WHERE conversation.project_id = ?1
                                        OR EXISTS (
                                            SELECT 1
                                            FROM conversation_participants participant
                                            WHERE participant.conversation_id = conversation.id
                                              AND participant.participant_type = 'agent'
                                              AND participant.participant_id = ?2
                                        )
                                 )
                               THEN ?1
                               ELSE task.project_id
                           END AS destination_project_id
                    FROM tasks task
                )
                SELECT EXISTS(
                    SELECT 1
                    FROM task_destinations child
                    JOIN task_destinations parent ON parent.id = child.parent_task_id
                    WHERE child.destination_project_id IS NOT parent.destination_project_id
                      AND (
                          child.destination_project_id IS NOT child.source_project_id
                          OR parent.destination_project_id IS NOT parent.source_project_id
                      )
                )",
                rusqlite::params![project_id, agent_id],
                |row| row.get(0),
            )?;
            if has_incompatible_task_hierarchy {
                return Err(Error::Project(
                    "moving this Agent would split a parent task from one of its child tasks; reassign the task hierarchy before moving the Agent".into(),
                ));
            }

            transaction.execute(
                "UPDATE conversations
                 SET project_id = ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE id IN (
                     SELECT conversation_id FROM conversation_participants
                     WHERE participant_type = 'agent' AND participant_id = ?2
                 )",
                rusqlite::params![project_id, agent_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET project_id = ?1
                 WHERE agent_id = ?2
                    OR conversation_id IN (SELECT id FROM conversations WHERE project_id = ?1)",
                rusqlite::params![project_id, agent_id],
            )?;
            transaction.execute(
                "UPDATE agents SET project_id = ?1 WHERE id = ?2",
                rusqlite::params![project_id, agent_id],
            )?;
            transaction.execute(
                "UPDATE projects SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                [project_id],
            )?;

            if let Some(previous_project_id) = previous_project_id {
                let remaining: i64 = transaction.query_row(
                    "SELECT
                       (SELECT COUNT(*) FROM agents WHERE project_id = ?1)
                       + (SELECT COUNT(*) FROM conversations WHERE project_id = ?1)
                       + (SELECT COUNT(*) FROM tasks WHERE project_id = ?1)",
                    [&previous_project_id],
                    |row| row.get(0),
                )?;
                if remaining == 0 {
                    move_project_memory(&transaction, &previous_project_id, project_id)?;
                    transaction.execute(
                        "DELETE FROM projects WHERE id = ?1",
                        [&previous_project_id],
                    )?;
                }
            }
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;
        self.get(project_id)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            // Acquire the SQLite write reservation before checking whether the
            // Project is empty. This keeps another connection from attaching
            // an Agent, Conversation, or Task between validation and deletion.
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let exists = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
                [id],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(Error::ProjectNotFound { id: id.to_string() });
            }
            let deleting = transaction.query_row(
                "SELECT deletion_started_at IS NOT NULL FROM projects WHERE id = ?1",
                [id],
                |row| row.get::<_, bool>(0),
            )?;
            if deleting {
                return Err(Error::Project(
                    "Project deletion is already in progress; retry with explicit cascade acknowledgement"
                        .into(),
                ));
            }
            let owned_records: i64 = transaction.query_row(
                "SELECT
                     (SELECT COUNT(*) FROM agents WHERE project_id = ?1)
                   + (SELECT COUNT(*) FROM conversations WHERE project_id = ?1)
                   + (SELECT COUNT(*) FROM tasks WHERE project_id = ?1)
                   + (SELECT COUNT(*) FROM project_memory_notes WHERE project_id = ?1)
                   + (SELECT COUNT(*) FROM workflow_instances
                      WHERE project_id = ?1
                         OR conversation_id IN
                            (SELECT id FROM conversations WHERE project_id = ?1))
                   + (SELECT COUNT(*) FROM schedules
                      WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)
                         OR continuation_task_id IN
                            (SELECT id FROM tasks WHERE project_id = ?1)
                         OR conversation_id IN
                            (SELECT id FROM conversations WHERE project_id = ?1))
                   + (SELECT COUNT(*) FROM project_sync_state WHERE project_id = ?1)
                   + (SELECT COUNT(*) FROM project_workflows WHERE project_id = ?1)",
                [id],
                |row| row.get(0),
            )?;
            if owned_records > 0 {
                return Err(Error::Project(
                    "this Project is not empty; use explicit cascade acknowledgement to permanently delete its Agents, tasks, conversations, memory, workflow runs or schedules, and sync configuration".into(),
                ));
            }
            transaction.execute("DELETE FROM projects WHERE id = ?1", [id])?;
            transaction.commit()?;
            Ok(())
        })
    }

    /// Begin a recoverable cascading deletion.
    ///
    /// The immediate transaction first makes the Project reject every new
    /// attachment, then transitions live durable work to terminal states.
    /// Runtime process/container cleanup happens in the server before
    /// [`Self::finish_cascade`] removes these rows.
    pub fn begin_cascade(&self, id: &str) -> Result<ProjectDeletionPlan> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let project_name = transaction
                .query_row("SELECT name FROM projects WHERE id = ?1", [id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?
                .ok_or_else(|| Error::ProjectNotFound { id: id.to_string() })?;

            transaction.execute(
                "UPDATE projects
                 SET deletion_started_at = COALESCE(deletion_started_at, CURRENT_TIMESTAMP),
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                [id],
            )?;

            let agents = {
                let mut statement = transaction.prepare(
                    "SELECT id, name, container_id FROM agents
                     WHERE project_id = ?1 ORDER BY id",
                )?;
                let agents = statement
                    .query_map([id], |row| {
                        Ok(ProjectDeletionAgent {
                            id: row.get(0)?,
                            name: row.get(1)?,
                            container_id: row.get(2)?,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                agents
            };
            let conversation_ids = collect_ids(
                &transaction,
                "SELECT id FROM conversations WHERE project_id = ?1 ORDER BY id",
                id,
            )?;
            let task_ids = collect_ids(
                &transaction,
                "SELECT id FROM tasks WHERE project_id = ?1 ORDER BY id",
                id,
            )?;
            let active_attempt_ids = collect_ids(
                &transaction,
                "SELECT DISTINCT attempt.id
                 FROM work_attempts attempt
                 LEFT JOIN tasks task ON task.id = attempt.task_id
                 LEFT JOIN logical_sessions session ON session.id = attempt.session_id
                 LEFT JOIN agents agent ON agent.id = session.agent_id
                 WHERE (task.project_id = ?1 OR agent.project_id = ?1)
                   AND (
                       attempt.status IN
                           ('queued', 'preparing', 'running', 'waiting_for_input', 'review')
                       OR attempt.container_id IS NOT NULL
                   )
                 ORDER BY attempt.id",
                id,
            )?;
            let active_turn_ids = collect_ids(
                &transaction,
                "SELECT turn.id
                 FROM conversation_turns turn
                 JOIN conversations conversation ON conversation.id = turn.conversation_id
                 WHERE conversation.project_id = ?1
                   AND turn.status IN ('queued', 'running')
                 ORDER BY turn.id",
                id,
            )?;
            let app_ids = collect_ids(
                &transaction,
                "SELECT app.id
                 FROM apps app
                 WHERE app.agent_id IN (SELECT id FROM agents WHERE project_id = ?1)
                    OR app.conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1)
                 ORDER BY app.id",
                id,
            )?;

            transaction.execute(
                "UPDATE work_attempts
                 SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                     error_message = 'Project deleted'
                 WHERE status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')
                   AND (
                       task_id IN (SELECT id FROM tasks WHERE project_id = ?1)
                       OR session_id IN (
                           SELECT session.id FROM logical_sessions session
                           JOIN agents agent ON agent.id = session.agent_id
                           WHERE agent.project_id = ?1
                       )
                   )",
                [id],
            )?;
            transaction.execute(
                "UPDATE task_queue
                 SET status = 'failed', completed_at = CURRENT_TIMESTAMP,
                     harness_response = 'Project deleted'
                 WHERE status IN ('queued', 'running')
                   AND (
                       task_id IN (SELECT id FROM tasks WHERE project_id = ?1)
                       OR agent_id IN (SELECT id FROM agents WHERE project_id = ?1)
                   )",
                [id],
            )?;
            transaction.execute(
                "UPDATE tasks
                 SET status = 'cancelled', active_attempt_id = NULL,
                     completed_at = NULL, updated_at = CURRENT_TIMESTAMP
                 WHERE project_id = ?1
                   AND status NOT IN ('completed', 'cancelled')",
                [id],
            )?;
            transaction.execute(
                "UPDATE task_pull_requests
                 SET status = 'cancelled', next_poll_at = NULL,
                     last_checked_at = CURRENT_TIMESTAMP,
                     last_error = 'Project deleted'
                 WHERE task_id IN (SELECT id FROM tasks WHERE project_id = ?1)
                   AND status IN ('waiting', 'attention')",
                [id],
            )?;
            transaction.execute(
                "UPDATE conversation_turns
                 SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                     error_message = 'Project deleted'
                 WHERE conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1)
                   AND status IN ('queued', 'running')",
                [id],
            )?;
            transaction.execute(
                "UPDATE conversation_agent_sessions
                 SET status = 'idle', last_error = 'Project deleted',
                     updated_at = CURRENT_TIMESTAMP
                 WHERE conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "UPDATE workflow_instances
                 SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                     error_message = 'Project deleted'
                 WHERE (project_id = ?1 OR conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1))
                   AND status IN ('running', 'waiting')",
                [id],
            )?;
            transaction.execute(
                "UPDATE schedules SET enabled = 0
                 WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)
                    OR continuation_task_id IN
                       (SELECT id FROM tasks WHERE project_id = ?1)
                    OR conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "UPDATE logical_sessions
                 SET status = 'idle', latest_summary = 'Project deleted',
                     updated_at = CURRENT_TIMESTAMP
                 WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "UPDATE agents
                 SET desired_status = 'stopped', status = 'stopped',
                     stopped_at = CURRENT_TIMESTAMP
                 WHERE project_id = ?1",
                [id],
            )?;
            transaction.commit()?;

            Ok(ProjectDeletionPlan {
                project_id: id.to_string(),
                project_name,
                agents,
                conversation_ids,
                task_ids,
                active_attempt_ids,
                active_turn_ids,
                app_ids,
            })
        })
    }

    /// Remove every durable record owned by a Project after runtime cleanup.
    /// Shared workflow definitions, connectors, and host workspaces are
    /// deliberately preserved.
    pub fn finish_cascade(&self, id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let deletion_started_at = transaction
                .query_row(
                    "SELECT deletion_started_at FROM projects WHERE id = ?1",
                    [id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| Error::ProjectNotFound { id: id.to_string() })?;
            if deletion_started_at.is_none() {
                return Err(Error::Project(
                    "cascading Project deletion requires explicit acknowledgement".into(),
                ));
            }

            // Preserve anomalous cross-Project records rather than allowing a
            // stale polymorphic link to broaden the deletion boundary.
            transaction.execute(
                "UPDATE tasks SET conversation_id = NULL, updated_at = CURRENT_TIMESTAMP
                 WHERE project_id IS NOT ?1
                   AND conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "UPDATE tasks
                 SET agent_id = NULL, session_id = NULL, active_attempt_id = NULL,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE project_id IS NOT ?1
                   AND agent_id IN (SELECT id FROM agents WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "UPDATE connector_channels SET agent_id = NULL
                 WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "DELETE FROM conversation_channel_bindings
                 WHERE conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1)
                    OR agent_id IN (SELECT id FROM agents WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "DELETE FROM schedules
                 WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)
                    OR continuation_task_id IN
                       (SELECT id FROM tasks WHERE project_id = ?1)
                    OR conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "DELETE FROM task_queue
                 WHERE task_id IN (SELECT id FROM tasks WHERE project_id = ?1)
                    OR agent_id IN (SELECT id FROM agents WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "DELETE FROM task_pull_requests
                 WHERE task_id IN (SELECT id FROM tasks WHERE project_id = ?1)
                    OR agent_id IN (SELECT id FROM agents WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "DELETE FROM apps
                 WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)
                    OR conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1)",
                [id],
            )?;

            for table in [
                "budget_state",
                "tool_logs",
                "agent_chat_messages",
                "memory_slots",
            ] {
                transaction.execute(
                    &format!(
                        "DELETE FROM {table}
                         WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)"
                    ),
                    [id],
                )?;
            }
            for table in ["usage_logs", "activity_logs"] {
                transaction.execute(
                    &format!(
                        "DELETE FROM {table}
                         WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)
                            OR session_id IN (
                                SELECT session.id FROM logical_sessions session
                                JOIN agents agent ON agent.id = session.agent_id
                                WHERE agent.project_id = ?1
                            )"
                    ),
                    [id],
                )?;
            }
            transaction.execute(
                "DELETE FROM memory_embeddings
                 WHERE memory_id IN (
                     SELECT memory.id FROM memories memory
                     WHERE memory.agent_id IN
                           (SELECT id FROM agents WHERE project_id = ?1)
                 )",
                [id],
            )?;
            transaction.execute(
                "DELETE FROM memories
                 WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)",
                [id],
            )?;

            transaction.execute(
                "DELETE FROM session_events
                 WHERE task_id IN (SELECT id FROM tasks WHERE project_id = ?1)
                    OR session_id IN (
                        SELECT session.id FROM logical_sessions session
                        JOIN agents agent ON agent.id = session.agent_id
                        WHERE agent.project_id = ?1
                    )
                    OR attempt_id IN (
                        SELECT attempt.id FROM work_attempts attempt
                        LEFT JOIN tasks task ON task.id = attempt.task_id
                        LEFT JOIN logical_sessions session
                          ON session.id = attempt.session_id
                        LEFT JOIN agents agent ON agent.id = session.agent_id
                        WHERE task.project_id = ?1 OR agent.project_id = ?1
                    )",
                [id],
            )?;
            transaction.execute(
                "DELETE FROM work_attempts
                 WHERE task_id IN (SELECT id FROM tasks WHERE project_id = ?1)
                    OR session_id IN (
                        SELECT session.id FROM logical_sessions session
                        JOIN agents agent ON agent.id = session.agent_id
                        WHERE agent.project_id = ?1
                    )",
                [id],
            )?;
            transaction.execute(
                "DELETE FROM workflow_instances
                 WHERE project_id = ?1
                    OR conversation_id IN
                       (SELECT id FROM conversations WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute("DELETE FROM conversations WHERE project_id = ?1", [id])?;
            transaction.execute("DELETE FROM tasks WHERE project_id = ?1", [id])?;
            transaction.execute(
                "DELETE FROM logical_sessions
                 WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute(
                "DELETE FROM conversation_participants
                 WHERE participant_type = 'agent'
                   AND participant_id IN
                       (SELECT id FROM agents WHERE project_id = ?1)",
                [id],
            )?;
            transaction.execute("DELETE FROM agents WHERE project_id = ?1", [id])?;
            transaction.execute(
                "DELETE FROM project_memory_notes WHERE project_id = ?1",
                [id],
            )?;
            transaction.execute("DELETE FROM project_sync_state WHERE project_id = ?1", [id])?;
            transaction.execute("DELETE FROM project_workflows WHERE project_id = ?1", [id])?;
            transaction.execute("DELETE FROM projects WHERE id = ?1", [id])?;
            transaction.commit()?;
            Ok(())
        })
    }
}

fn collect_ids(conn: &rusqlite::Connection, sql: &str, project_id: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let ids = statement
        .query_map([project_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn row_to_project(
    conn: &rusqlite::Connection,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Project> {
    let id: String = row.get("id")?;
    let mut statement =
        conn.prepare("SELECT id FROM agents WHERE project_id = ?1 ORDER BY name")?;
    let agent_ids = statement
        .query_map([&id], |agent| agent.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    let deletion_counts = conn.query_row(
        "SELECT
             (SELECT COUNT(*) FROM tasks WHERE project_id = ?1),
             (SELECT COUNT(*) FROM task_messages
              WHERE task_id IN (SELECT id FROM tasks WHERE project_id = ?1)),
             (SELECT COUNT(*) FROM conversations WHERE project_id = ?1),
             (SELECT COUNT(*) FROM conversation_messages
              WHERE conversation_id IN
                    (SELECT id FROM conversations WHERE project_id = ?1)),
             (SELECT COUNT(*) FROM project_memory_notes WHERE project_id = ?1),
             (SELECT COUNT(*) FROM workflow_instances
              WHERE project_id = ?1
                 OR conversation_id IN
                    (SELECT id FROM conversations WHERE project_id = ?1)),
             (SELECT COUNT(*) FROM schedules
              WHERE agent_id IN (SELECT id FROM agents WHERE project_id = ?1)
                 OR continuation_task_id IN
                    (SELECT id FROM tasks WHERE project_id = ?1)
                 OR conversation_id IN
                    (SELECT id FROM conversations WHERE project_id = ?1))",
        [&id],
        |count| {
            Ok(ProjectDeletionCounts {
                agents: agent_ids.len() as i64,
                tasks: count.get(0)?,
                task_messages: count.get(1)?,
                conversations: count.get(2)?,
                conversation_messages: count.get(3)?,
                memory_notes: count.get(4)?,
                workflow_runs: count.get(5)?,
                schedules: count.get(6)?,
            })
        },
    )?;
    Ok(Project {
        id,
        name: row.get("name")?,
        description: row.get("description")?,
        icon: row.get("icon")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        agent_ids,
        conversation_count: row.get("conversation_count")?,
        task_count: row.get("task_count")?,
        deletion_started_at: row.get("deletion_started_at")?,
        deletion_counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::project::{CreateProjectMemoryNote, ProjectMemoryStore};

    #[test]
    fn project_selectors_support_names_and_keep_exact_ids_deterministic() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('platform-id', 'Platform');
                 INSERT INTO projects (id, name) VALUES ('platform', 'A Project Named Like An ID');
                 INSERT INTO projects (id, name) VALUES ('website-id', 'Straße');",
            )
        })
        .unwrap();
        let manager = ProjectManager::new(db);

        assert_eq!(manager.resolve("PLATFORM").unwrap().id, "platform-id");
        assert_eq!(manager.resolve("platform").unwrap().id, "platform");
        assert_eq!(manager.resolve(" platform-id ").unwrap().name, "Platform");
        assert_eq!(manager.resolve("STRASSE").unwrap().id, "website-id");
    }

    #[test]
    fn project_selectors_report_ambiguous_and_unknown_names_actionably() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('platform-one', 'Platform');
                 INSERT INTO projects (id, name) VALUES ('platform-two', 'platform');
                 INSERT INTO projects (id, name) VALUES ('website-id', 'Website');",
            )
        })
        .unwrap();
        let manager = ProjectManager::new(db);

        let ambiguous = manager.resolve("platform").unwrap_err().to_string();
        assert!(ambiguous.contains("ambiguous"));
        assert!(ambiguous.contains("platform-one"));
        assert!(ambiguous.contains("platform-two"));
        assert!(ambiguous.contains("--project-id <ID>"));

        let unknown = manager.resolve("mobile").unwrap_err().to_string();
        assert!(unknown.contains("no local Project matches 'mobile'"));
        assert!(unknown.contains("Website"));
        assert!(unknown.contains("Project page"));
    }

    #[test]
    fn projects_group_existing_agents_and_reject_nonempty_deletion() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute("INSERT INTO projects (id, name) VALUES ('one', 'One')", [])?;
            conn.execute(
                "INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'one')",
                [],
            )
        })
        .unwrap();
        let manager = ProjectManager::new(db);
        let project = manager.get("one").unwrap();
        assert_eq!(project.agent_ids, vec!["atlas"]);
        assert!(manager.delete("one").is_err());
    }

    #[test]
    fn project_deletion_requires_cascade_for_active_or_completed_workflow_runs() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One');
                 INSERT INTO workflows (id, name, yaml_content)
                    VALUES ('review', 'Review', 'name: Review');
                 INSERT INTO workflow_instances
                    (id, workflow_id, status, project_id)
                    VALUES ('review-run', 'review', 'waiting', 'one');",
            )
        })
        .unwrap();
        let manager = ProjectManager::new(db.clone());

        let error = manager.delete("one").unwrap_err();
        assert!(error.to_string().contains("not empty"));
        assert!(manager.get("one").is_ok());

        db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET status = 'completed' WHERE id = 'review-run'",
                [],
            )
        })
        .unwrap();
        let error = manager.delete("one").unwrap_err();
        assert!(error.to_string().contains("explicit cascade"));

        manager.begin_cascade("one").unwrap();
        manager.finish_cascade("one").unwrap();
        assert!(matches!(
            manager.get("one"),
            Err(Error::ProjectNotFound { .. })
        ));
        let workflows: i64 = db
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM workflows", [], |row| row.get(0))
            })
            .unwrap();
        assert_eq!(workflows, 1, "reusable workflow definitions are shared");
    }

    #[test]
    fn newly_created_projects_own_memory_without_a_shadow_agent_session() {
        let db = Arc::new(Database::open_memory().unwrap());
        let project = ProjectManager::new(db.clone())
            .create(&CreateProject {
                name: "Website".into(),
                description: None,
                icon: None,
            })
            .unwrap();
        let note = ProjectMemoryStore::new(db)
            .create(
                &project.id,
                &CreateProjectMemoryNote {
                    title: "Release policy".into(),
                    body: "Review before release.".into(),
                    summary: None,
                    note_type: "convention".into(),
                    state: "evergreen".into(),
                    source_task_id: None,
                    source_attempt_id: None,
                    created_by: "user".into(),
                    pinned: false,
                    tags: vec![],
                },
            )
            .unwrap();

        assert_eq!(note.project_id, project.id);
    }

    #[test]
    fn project_memory_requires_confirmed_cascade_and_is_then_removed() {
        let db = Arc::new(Database::open_memory().unwrap());
        let manager = ProjectManager::new(db.clone());
        let project = manager
            .create(&CreateProject {
                name: "Temporary".into(),
                description: None,
                icon: None,
            })
            .unwrap();
        ProjectMemoryStore::new(db.clone())
            .create(
                &project.id,
                &CreateProjectMemoryNote {
                    title: "Temporary note".into(),
                    body: "Delete with the Project.".into(),
                    summary: None,
                    note_type: "fact".into(),
                    state: "evergreen".into(),
                    source_task_id: None,
                    source_attempt_id: None,
                    created_by: "user".into(),
                    pinned: false,
                    tags: vec![],
                },
            )
            .unwrap();

        let error = manager.delete(&project.id).unwrap_err();
        assert!(error.to_string().contains("not empty"));
        assert!(manager.get(&project.id).is_ok());

        manager.begin_cascade(&project.id).unwrap();
        manager.finish_cascade(&project.id).unwrap();

        assert!(matches!(
            manager.get(&project.id),
            Err(Error::ProjectNotFound { .. })
        ));
        let notes: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM project_memory_notes WHERE project_id = ?1",
                    [&project.id],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(notes, 0);
    }

    #[test]
    fn moving_an_agent_waits_for_live_task_and_conversation_turns() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('source', 'Source');
                 INSERT INTO projects (id, name) VALUES ('target', 'Target');
                 INSERT INTO agents (id, name, backend, config, project_id)
                    VALUES ('atlas', 'Atlas', 'native', '{}', 'source');
                 INSERT INTO logical_sessions (id, agent_id, title)
                    VALUES ('atlas', 'atlas', 'Atlas');
                 INSERT INTO work_attempts (id, session_id, runner, status)
                    VALUES ('attempt-one', 'atlas', 'native', 'running');",
            )
        })
        .unwrap();
        let manager = ProjectManager::new(db.clone());

        let error = manager.assign_agent("target", "atlas").unwrap_err();
        assert!(error
            .to_string()
            .contains("active task or Conversation response"));

        db.with_conn(|conn| {
            conn.execute_batch(
                "UPDATE work_attempts SET status = 'completed' WHERE id = 'attempt-one';
                 INSERT INTO conversations (id, title, project_id)
                    VALUES ('conversation-one', 'Design', 'source');
                 INSERT INTO conversation_participants
                    (conversation_id, participant_type, participant_id)
                    VALUES ('conversation-one', 'agent', 'atlas');
                 INSERT INTO conversation_turns
                    (id, conversation_id, agent_id, status)
                    VALUES ('turn-one', 'conversation-one', 'atlas', 'running');",
            )
        })
        .unwrap();

        let error = manager.assign_agent("target", "atlas").unwrap_err();
        assert!(error
            .to_string()
            .contains("active task or Conversation response"));
        let project_id = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT project_id FROM agents WHERE id = 'atlas'",
                    [],
                    |row| row.get::<_, String>(0),
                )
            })
            .unwrap();
        assert_eq!(project_id, "source");
    }

    #[test]
    fn moving_an_agent_waits_for_project_workflows_to_finish() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('source', 'Source');
                 INSERT INTO projects (id, name) VALUES ('target', 'Target');
                 INSERT INTO agents (id, name, backend, config, project_id)
                    VALUES ('atlas', 'Atlas', 'native', '{}', 'source');
                 INSERT INTO workflows (id, name, yaml_content)
                    VALUES ('review', 'Review', 'name: Review');
                 INSERT INTO workflow_instances
                    (id, workflow_id, status, project_id)
                    VALUES ('review-run', 'review', 'waiting', 'source');",
            )
        })
        .unwrap();
        let manager = ProjectManager::new(db.clone());

        let error = manager.assign_agent("target", "atlas").unwrap_err();
        assert!(error.to_string().contains("active Project workflow"));

        db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET status = 'completed' WHERE id = 'review-run'",
                [],
            )
        })
        .unwrap();
        let target = manager.assign_agent("target", "atlas").unwrap();
        assert_eq!(target.agent_ids, vec!["atlas"]);
    }

    #[test]
    fn moving_an_agent_rejects_linked_tasks_owned_outside_the_target_project() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('source', 'Source');
                 INSERT INTO projects (id, name) VALUES ('target', 'Target');
                 INSERT INTO agents (id, name, backend, config, project_id)
                    VALUES ('atlas', 'Atlas', 'native', '{}', 'source');
                 INSERT INTO agents (id, name, backend, config, project_id)
                    VALUES ('reviewer', 'Reviewer', 'native', '{}', 'source');",
            )
        })
        .unwrap();
        let conversation = crate::conversations::ConversationManager::new(db.clone())
            .create_in_project(
                Some("source"),
                &crate::conversations::CreateConversation {
                    title: Some("Review room".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into()],
                },
            )
            .unwrap();
        let task = crate::tasks::board::TaskBoard::new(db.clone())
            .create(&crate::tasks::board::CreateTask {
                title: "Review the patch".into(),
                agent_id: Some("reviewer".into()),
                conversation_id: Some(conversation.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let manager = ProjectManager::new(db.clone());

        let error = manager.assign_agent("target", "atlas").unwrap_err();
        assert!(error.to_string().contains("linked task"));
        let (agent_project, conversation_project, task_project): (String, String, String) = db
            .with_conn(|conn| {
                Ok::<_, rusqlite::Error>((
                    conn.query_row(
                        "SELECT project_id FROM agents WHERE id = 'atlas'",
                        [],
                        |row| row.get(0),
                    )?,
                    conn.query_row(
                        "SELECT project_id FROM conversations WHERE id = ?1",
                        [&conversation.id],
                        |row| row.get(0),
                    )?,
                    conn.query_row(
                        "SELECT project_id FROM tasks WHERE id = ?1",
                        [&task.id],
                        |row| row.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!(agent_project, "source");
        assert_eq!(conversation_project, "source");
        assert_eq!(task_project, "source");

        let error = manager.assign_agent("target", "reviewer").unwrap_err();
        assert!(error.to_string().contains("linked task"));

        db.with_conn(|conn| {
            conn.execute(
                "UPDATE tasks SET agent_id = 'atlas' WHERE id = ?1",
                [&task.id],
            )
        })
        .unwrap();
        manager.assign_agent("target", "atlas").unwrap();
        let (conversation_project, task_project): (String, String) = db
            .with_conn(|conn| {
                Ok::<_, rusqlite::Error>((
                    conn.query_row(
                        "SELECT project_id FROM conversations WHERE id = ?1",
                        [&conversation.id],
                        |row| row.get(0),
                    )?,
                    conn.query_row(
                        "SELECT project_id FROM tasks WHERE id = ?1",
                        [&task.id],
                        |row| row.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!(conversation_project, "target");
        assert_eq!(task_project, "target");
    }

    #[test]
    fn moving_an_agent_keeps_parent_and_child_tasks_in_one_project() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('source', 'Source');
                 INSERT INTO projects (id, name) VALUES ('target', 'Target');
                 INSERT INTO agents (id, name, backend, config, project_id)
                    VALUES ('atlas', 'Atlas', 'native', '{}', 'source');
                 INSERT INTO agents (id, name, backend, config, project_id)
                    VALUES ('reviewer', 'Reviewer', 'native', '{}', 'source');",
            )
        })
        .unwrap();
        let board = crate::tasks::board::TaskBoard::new(db.clone());
        let parent = board
            .create(&crate::tasks::board::CreateTask {
                title: "Implement the change".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let reviewed_child = board
            .create(&crate::tasks::board::CreateTask {
                title: "Review the change".into(),
                agent_id: Some("reviewer".into()),
                parent_task_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let unassigned_child = board
            .create(&crate::tasks::board::CreateTask {
                title: "Publish the change".into(),
                parent_task_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let manager = ProjectManager::new(db.clone());

        let error = manager.assign_agent("target", "atlas").unwrap_err();
        assert!(error.to_string().contains("parent task"));
        for task_id in [&parent.id, &reviewed_child.id, &unassigned_child.id] {
            assert_eq!(
                board.get(task_id).unwrap().project_id.as_deref(),
                Some("source")
            );
        }

        db.with_conn(|conn| {
            conn.execute(
                "UPDATE tasks SET agent_id = 'atlas'
                 WHERE id IN (?1, ?2)",
                rusqlite::params![reviewed_child.id, unassigned_child.id],
            )
        })
        .unwrap();
        manager.assign_agent("target", "atlas").unwrap();
        for task_id in [&parent.id, &reviewed_child.id, &unassigned_child.id] {
            assert_eq!(
                board.get(task_id).unwrap().project_id.as_deref(),
                Some("target")
            );
        }
    }

    #[test]
    fn cascade_marker_cancels_live_work_and_blocks_new_project_attachments() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One'), ('two', 'Two');
                 INSERT INTO agents (id, name, backend, config, project_id)
                    VALUES ('atlas', 'Atlas', 'native', '{}', 'one');
                 INSERT INTO logical_sessions (id, agent_id, title, status)
                    VALUES ('atlas', 'atlas', 'Atlas', 'running');
                 INSERT INTO tasks (id, title, status, agent_id, project_id)
                    VALUES ('task-one', 'Active task', 'in_progress', 'atlas', 'one');
                 INSERT INTO task_queue (task_id, agent_id, status)
                    VALUES ('task-one', 'atlas', 'running');
                 INSERT INTO work_attempts
                    (id, session_id, task_id, runner, status)
                    VALUES ('attempt-one', 'atlas', 'task-one', 'native', 'running');
                 INSERT INTO conversations (id, title, project_id)
                    VALUES ('conversation-one', 'Active conversation', 'one');
                 INSERT INTO conversation_participants
                    (conversation_id, participant_type, participant_id)
                    VALUES ('conversation-one', 'agent', 'atlas');
                 INSERT INTO conversation_turns
                    (id, conversation_id, agent_id, status)
                    VALUES ('turn-one', 'conversation-one', 'atlas', 'running');
                 INSERT INTO conversation_agent_sessions
                    (conversation_id, agent_id, status)
                    VALUES ('conversation-one', 'atlas', 'running');
                 INSERT INTO workflows (id, name, yaml_content)
                    VALUES ('shared-workflow', 'Shared', 'name: Shared');
                 INSERT INTO workflow_instances
                    (id, workflow_id, status, project_id)
                    VALUES ('run-one', 'shared-workflow', 'waiting', 'one');
                 INSERT INTO schedules (id, name, cron, agent_id, title)
                    VALUES ('schedule-one', 'Daily', '* * * * *', 'atlas', 'Daily');",
            )
        })
        .unwrap();
        let manager = ProjectManager::new(db.clone());

        let plan = manager.begin_cascade("one").unwrap();
        assert_eq!(plan.project_name, "One");
        assert_eq!(plan.active_attempt_ids, vec!["attempt-one"]);
        assert_eq!(plan.active_turn_ids, vec!["turn-one"]);
        let retry = manager.begin_cascade("one").unwrap();
        assert_eq!(retry.project_id, plan.project_id);
        assert_eq!(retry.agents, plan.agents);
        assert!(retry.active_attempt_ids.is_empty());
        assert!(retry.active_turn_ids.is_empty());

        db.with_conn(|conn| {
            for (table, id) in [
                ("work_attempts", "attempt-one"),
                ("conversation_turns", "turn-one"),
                ("workflow_instances", "run-one"),
            ] {
                let status: String = conn
                    .query_row(
                        &format!("SELECT status FROM {table} WHERE id = ?1"),
                        [id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(status, "cancelled");
            }
            let enabled: bool = conn
                .query_row(
                    "SELECT enabled FROM schedules WHERE id = 'schedule-one'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!enabled);
        });

        let task_error = crate::tasks::board::TaskBoard::new(db.clone())
            .create(&crate::tasks::board::CreateTask {
                title: "Late task".into(),
                context: Some(serde_json::json!({ "project_id": "one" })),
                ..Default::default()
            })
            .unwrap_err();
        assert!(task_error.to_string().contains("being deleted"));
        let message_error = crate::conversations::ConversationManager::new(db.clone())
            .send_message(
                "conversation-one",
                &crate::conversations::SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Late reply".into(),
                    message_type: None,
                },
            )
            .unwrap_err();
        assert!(message_error.to_string().contains("being deleted"));
        let task_message_error = crate::tasks::conversation::TaskConversation::new(db.clone())
            .add_message("task-one", "user", "Late task reply")
            .unwrap_err();
        assert!(task_message_error.to_string().contains("being deleted"));
        let queue_error = crate::tasks::queue::TaskQueue::new(db.clone())
            .enqueue("task-one", "atlas")
            .unwrap_err();
        assert!(queue_error.to_string().contains("being deleted"));
        let reopen_error = crate::tasks::board::TaskBoard::new(db.clone())
            .update_status("task-one", "pending", Some("atlas"))
            .unwrap_err();
        assert!(reopen_error.to_string().contains("being deleted"));
        let memory_error = ProjectMemoryStore::new(db.clone())
            .create(
                "one",
                &CreateProjectMemoryNote {
                    title: "Late memory".into(),
                    body: "Do not persist this.".into(),
                    summary: None,
                    note_type: "fact".into(),
                    state: "evergreen".into(),
                    source_task_id: None,
                    source_attempt_id: None,
                    created_by: "user".into(),
                    pinned: false,
                    tags: vec![],
                },
            )
            .unwrap_err();
        assert!(memory_error.to_string().contains("being deleted"));
        let agent_error = crate::agents::registry::AgentRegistry::new(db.clone())
            .create_in_project("late-agent", "native", "one")
            .unwrap_err();
        assert!(agent_error.to_string().contains("being deleted"));
        let move_error = manager.assign_agent("two", "atlas").unwrap_err();
        assert!(move_error.to_string().contains("being deleted"));
        let schedule_error = crate::tasks::scheduler::ScheduleManager::new(db)
            .create(&crate::tasks::scheduler::CreateSchedule {
                name: "Late schedule".into(),
                cron: "* * * * *".into(),
                agent_id: "atlas".into(),
                title: "Late".into(),
                description: None,
            })
            .unwrap_err();
        assert!(schedule_error.to_string().contains("being deleted"));
    }

    #[test]
    fn cascading_deletion_removes_owned_graph_without_cross_project_or_shared_data() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One'), ('two', 'Two');
                 INSERT INTO agents (id, name, backend, config, project_id)
                    VALUES ('atlas', 'Atlas', 'native', '{}', 'one'),
                           ('other', 'Other', 'native', '{}', 'two');
                 INSERT INTO logical_sessions (id, agent_id, title)
                    VALUES ('atlas', 'atlas', 'Atlas'), ('other', 'other', 'Other');
                 INSERT INTO conversations (id, title, project_id)
                    VALUES ('conversation-one', 'One conversation', 'one'),
                           ('conversation-two', 'Two conversation', 'two');
                 INSERT INTO conversation_participants
                    (conversation_id, participant_type, participant_id)
                    VALUES ('conversation-one', 'agent', 'atlas'),
                           ('conversation-two', 'agent', 'other'),
                           ('conversation-two', 'agent', 'atlas');
                 INSERT INTO conversation_messages
                    (id, conversation_id, sender_type, sender_id, content)
                    VALUES (1, 'conversation-one', 'user', 'local', 'Delete me'),
                           (2, 'conversation-two', 'user', 'local', 'Keep me');
                 INSERT INTO conversation_message_attachments
                    (id, message_id, name, mime_type, data, size)
                    VALUES ('conversation-file', 1, 'one.txt', 'text/plain', X'31', 1);
                 INSERT INTO tasks
                    (id, title, status, agent_id, conversation_id, project_id)
                    VALUES ('task-one', 'Delete task', 'in_progress', 'atlas', 'conversation-one', 'one'),
                           ('task-two', 'Keep task', 'pending', 'atlas', 'conversation-one', 'two');
                 UPDATE tasks SET parent_task_id = 'task-one' WHERE id = 'task-two';
                 INSERT INTO task_messages (id, task_id, role, content)
                    VALUES (1, 'task-one', 'user', 'Delete message');
                 INSERT INTO task_message_attachments
                    (id, message_id, name, mime_type, data, size)
                    VALUES ('task-file', 1, 'one.txt', 'text/plain', X'31', 1);
                 INSERT INTO task_queue (task_id, agent_id, status)
                    VALUES ('task-one', 'atlas', 'running');
                 INSERT INTO work_attempts
                    (id, session_id, task_id, runner, status)
                    VALUES ('attempt-one', 'atlas', 'task-one', 'native', 'running');
                 INSERT INTO session_events
                    (session_id, attempt_id, task_id, source_type, event_type, summary)
                    VALUES ('atlas', 'attempt-one', 'task-one', 'runner', 'progress', 'Delete event');
                 INSERT INTO attempt_artifacts
                    (id, attempt_id, session_id, artifact_type, title)
                    VALUES ('artifact-one', 'attempt-one', 'atlas', 'file', 'Delete artifact');
                 INSERT INTO project_memory_notes
                    (id, project_id, title, body, summary, search_key)
                    VALUES ('note-one', 'one', 'Delete note', 'body', 'summary', 'delete note');
                 INSERT INTO workflows (id, name, yaml_content)
                    VALUES ('shared-workflow', 'Shared', 'name: Shared');
                 INSERT INTO workflow_instances
                    (id, workflow_id, status, project_id, conversation_id)
                    VALUES ('run-one', 'shared-workflow', 'waiting', 'one', 'conversation-one'),
                           ('run-two', 'shared-workflow', 'completed', 'two', 'conversation-two');
                 INSERT INTO workflow_step_executions
                    (id, instance_id, flow_name, step_id, task_id, status)
                    VALUES ('step-one', 'run-one', 'main', 'work', 'task-one', 'running');
                 INSERT INTO schedules (id, name, cron, agent_id, title, continuation_task_id)
                    VALUES ('schedule-one', 'Delete schedule', '', 'atlas', 'Wake', 'task-one');
                 INSERT INTO project_sync_state
                    (project_id, remote, branch, store_path, last_commit,
                     local_snapshot_hash, remote_snapshot_hash)
                    VALUES ('one', 'origin', 'main', 'projects/one', 'abc', 'local', 'remote');
                 INSERT INTO apps (id, title, agent_id, conversation_id)
                    VALUES ('app-one', 'Delete app', 'atlas', 'conversation-one');
                 INSERT INTO connectors (id, name, connector_type)
                    VALUES ('connector', 'Shared connector', 'webhook');
                 INSERT INTO connector_channels
                    (id, connector_id, name, agent_id)
                    VALUES ('channel', 'connector', 'Shared channel', 'atlas');
                 INSERT INTO conversation_channel_bindings
                    (conversation_id, channel_id, agent_id)
                    VALUES ('conversation-one', 'channel', 'atlas');
                 INSERT INTO memories (id, content, summary, source, agent_id)
                    VALUES ('legacy-memory', 'Delete memory', 'summary', 'agent', 'atlas');
                 INSERT INTO memory_slots (agent_id, slot_index, memory_id)
                    VALUES ('atlas', 0, 'legacy-memory');
                 INSERT INTO budget_state (agent_id) VALUES ('atlas');
                 INSERT INTO usage_logs
                    (agent_id, model, input_tokens, output_tokens, cost_usd)
                    VALUES ('atlas', 'test', 1, 1, 0.0);
                 INSERT INTO activity_logs (agent_id, event_type)
                    VALUES ('atlas', 'test');
                 INSERT INTO tool_logs (agent_id, tool_name)
                    VALUES ('atlas', 'test');
                 INSERT INTO agent_chat_messages (agent_id, role, content)
                    VALUES ('atlas', 'user', 'Delete chat');",
            )
        })
        .unwrap();
        let manager = ProjectManager::new(db.clone());
        let counts = manager.get("one").unwrap().deletion_counts;
        assert_eq!(counts.agents, 1);
        assert_eq!(counts.tasks, 1);
        assert_eq!(counts.task_messages, 1);
        assert_eq!(counts.conversations, 1);
        assert_eq!(counts.conversation_messages, 1);
        assert_eq!(counts.memory_notes, 1);
        assert_eq!(counts.workflow_runs, 1);
        assert_eq!(counts.schedules, 1);

        manager.begin_cascade("one").unwrap();
        manager.finish_cascade("one").unwrap();

        assert!(matches!(
            manager.get("one"),
            Err(Error::ProjectNotFound { .. })
        ));
        let (projects, agents, conversations, tasks, workflows, runs): (
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = db
            .with_conn(|conn| {
                Ok::<_, rusqlite::Error>((
                    conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?,
                    conn.query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))?,
                    conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?,
                    conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?,
                    conn.query_row("SELECT COUNT(*) FROM workflows", [], |row| row.get(0))?,
                    conn.query_row("SELECT COUNT(*) FROM workflow_instances", [], |row| {
                        row.get(0)
                    })?,
                ))
            })
            .unwrap();
        assert_eq!((projects, agents, conversations, tasks), (1, 1, 1, 1));
        assert_eq!((workflows, runs), (1, 1));

        db.with_conn(|conn| {
            let preserved_task: (Option<String>, Option<String>, Option<String>) = conn
                .query_row(
                    "SELECT agent_id, conversation_id, parent_task_id
                     FROM tasks WHERE id = 'task-two'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(preserved_task, (None, None, None));
            let channel_agent: Option<String> = conn
                .query_row(
                    "SELECT agent_id FROM connector_channels WHERE id = 'channel'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(channel_agent, None);
            for table in [
                "project_memory_notes",
                "project_sync_state",
                "apps",
                "memories",
                "usage_logs",
                "activity_logs",
                "tool_logs",
                "agent_chat_messages",
                "conversation_channel_bindings",
            ] {
                let count: i64 = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(count, 0, "{table} should not retain Project-owned rows");
            }
        });

        assert!(matches!(
            manager.finish_cascade("one"),
            Err(Error::ProjectNotFound { .. })
        ));
    }
}
