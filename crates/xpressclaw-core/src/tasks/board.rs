use std::sync::Arc;

use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    WaitingForInput,
    Blocked,
    Completed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::WaitingForInput => "waiting_for_input",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "waiting_for_input" => Ok(Self::WaitingForInput),
            "blocked" => Ok(Self::Blocked),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(Error::Task(format!("invalid task status: {s}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: i32,
    pub agent_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub sop_id: Option<String>,
    pub conversation_id: Option<String>,
    pub project_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub context: Option<serde_json::Value>,
    /// Task type: "normal" or "IDLE" (hidden single-turn idle tasks).
    #[serde(default = "default_task_type")]
    pub task_type: String,
    /// Hidden tasks (e.g. idle tasks) are excluded from default list views.
    #[serde(default)]
    pub hidden: bool,
}

fn default_task_type() -> String {
    "normal".to_string()
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateTask {
    pub title: String,
    pub description: Option<String>,
    pub agent_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub sop_id: Option<String>,
    pub conversation_id: Option<String>,
    pub priority: Option<i32>,
    pub context: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTask {
    pub title: Option<String>,
    pub description: Option<String>,
    pub agent_id: Option<String>,
    pub priority: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct TaskCounts {
    pub pending: i64,
    pub in_progress: i64,
    pub waiting_for_input: i64,
    pub blocked: i64,
    pub completed: i64,
    pub cancelled: i64,
}

/// A step reported by a native coding harness through its plan/todo stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedSubtask {
    pub title: String,
    pub status: TaskStatus,
}

#[derive(Clone, Copy)]
enum TaskListOrder {
    Scheduler,
    Recent,
}

#[derive(Clone, Copy)]
struct TaskListPage {
    limit: i64,
    offset: i64,
}

impl TaskListPage {
    fn first(limit: i64) -> Self {
        Self { limit, offset: 0 }
    }
}

#[derive(Clone, Copy)]
struct TaskListFilter<'a> {
    statuses: &'a [&'a str],
    agent_id: Option<&'a str>,
    excluded_statuses: &'a [&'a str],
    search: Option<&'a str>,
}

/// Kanban task board with CRUD operations and status transitions.
pub struct TaskBoard {
    db: Arc<Database>,
}

impl TaskBoard {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn create(&self, req: &CreateTask) -> Result<Task> {
        self.create_with_conversation_agent(req, None)
    }

    /// Create work requested by an Agent from one of its Conversations.
    ///
    /// The membership check shares the same immediate transaction as task
    /// insertion. Participant removal therefore linearizes either before this
    /// call (and rejects it) or after the task has already been created.
    pub fn create_for_conversation_agent(
        &self,
        req: &CreateTask,
        creator_agent_id: &str,
    ) -> Result<Task> {
        self.create_with_conversation_agent(req, Some(creator_agent_id))
    }

    fn create_with_conversation_agent(
        &self,
        req: &CreateTask,
        creator_agent_id: Option<&str>,
    ) -> Result<Task> {
        let id = self.db.with_conn(|conn| {
            // Reserve the writer before resolving any ownership inputs. Agent,
            // Conversation, and parent-task moves must not be able to invalidate
            // the Project boundary between validation and task insertion.
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let id = Self::create_in_transaction(&transaction, req, creator_agent_id)?;
            transaction.commit()?;
            Ok::<_, Error>(id)
        })?;

        self.get(&id)
    }

    /// Insert a task while the caller owns the surrounding write transaction.
    ///
    /// Conversation work uses this to commit task creation, dispatch, and the
    /// linked Conversation publication as one lifecycle operation.
    pub(crate) fn create_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        req: &CreateTask,
        creator_agent_id: Option<&str>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let priority = req.priority.unwrap_or(0);
        let context_json = req.context.as_ref().map(|c| c.to_string());
        let requested_project = req
            .context
            .as_ref()
            .and_then(|context| context.get("project_id"))
            .and_then(serde_json::Value::as_str);

        let conversation_project = if let Some(conversation_id) = req.conversation_id.as_deref() {
            let project = transaction
                .query_row(
                    "SELECT project_id FROM conversations WHERE id = ?1",
                    [conversation_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?;
            match project {
                Some(project_id) => project_id,
                None => {
                    return Err(Error::ConversationNotFound {
                        id: conversation_id.to_string(),
                    });
                }
            }
        } else {
            None
        };
        if let Some(creator_agent_id) = creator_agent_id {
            let conversation_id = req.conversation_id.as_deref().ok_or_else(|| {
                Error::Conversation("an Agent may only create work from a conversation".into())
            })?;
            if req.agent_id.as_deref() != Some(creator_agent_id) {
                return Err(Error::Conversation(
                    "an Agent may only create a conversation task for itself".into(),
                ));
            }
            let is_participant = transaction.query_row(
                "SELECT EXISTS(
                        SELECT 1 FROM conversation_participants
                        WHERE conversation_id = ?1
                          AND participant_type = 'agent'
                          AND participant_id = ?2
                    )",
                rusqlite::params![conversation_id, creator_agent_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !is_participant {
                return Err(Error::Conversation(format!(
                    "Agent '{creator_agent_id}' is not a participant in conversation '{conversation_id}'"
                )));
            }
        }
        if let Some(project_id) = requested_project {
            let exists = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
                [project_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(Error::ProjectNotFound {
                    id: project_id.to_string(),
                });
            }
        }
        let agent_project = if let Some(agent_id) = req.agent_id.as_deref() {
            transaction
                .query_row(
                    "SELECT project_id FROM agents WHERE id = ?1",
                    [agent_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
        } else {
            None
        };
        let parent_project = if let Some(parent_task_id) = req.parent_task_id.as_deref() {
            transaction
                .query_row(
                    "SELECT project_id FROM tasks WHERE id = ?1",
                    [parent_task_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| Error::TaskNotFound {
                    id: parent_task_id.to_string(),
                })?
        } else {
            None
        };
        let requested_project = requested_project.map(str::to_string);
        let project_id = consistent_project_id([
            ("conversation", conversation_project.as_deref()),
            ("request", requested_project.as_deref()),
            ("Agent", agent_project.as_deref()),
            ("parent task", parent_project.as_deref()),
        ])?;
        if req.parent_task_id.is_some() && parent_project.is_none() && project_id.is_some() {
            return Err(Error::Task(
                "a task in a Project cannot be added beneath a projectless parent task".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO tasks (id, title, description, status, priority, agent_id, parent_task_id, sop_id, conversation_id, context, created_at, updated_at, project_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                id,
                req.title,
                req.description,
                "pending",
                priority,
                req.agent_id,
                req.parent_task_id,
                req.sop_id,
                req.conversation_id,
                context_json,
                now,
                now,
                project_id,
            ],
        )?;
        Ok(id)
    }

    /// Create a hidden single-turn idle task for an agent (XCLAW-47).
    pub fn create_idle_task(&self, agent_id: &str, description: &str) -> Result<Task> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let title = format!("[Idle] {agent_id}");

        {
            let conn = self.db.conn();
            conn.execute(
                "INSERT INTO tasks (id, title, description, status, priority, agent_id, task_type, hidden, created_at, updated_at, project_id)
                 VALUES (?1, ?2, ?3, 'pending', 0, ?4, 'IDLE', 1, ?5, ?6,
                         (SELECT project_id FROM agents WHERE id = ?4))",
                rusqlite::params![id, title, description, agent_id, now, now],
            )?;
        }

        self.get(&id)
    }

    pub fn get(&self, task_id: &str) -> Result<Task> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT * FROM tasks WHERE id = ?1")?;
        let task = stmt
            .query_row([task_id], |row| Ok(row_to_task(row)))
            .map_err(|_| Error::TaskNotFound {
                id: task_id.to_string(),
            })??;
        Ok(task)
    }

    pub fn set_conversation_id(&self, task_id: &str, conversation_id: &str) -> Result<()> {
        let conn = self.db.conn();
        let transaction =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let conversation_project = transaction
            .query_row(
                "SELECT project_id FROM conversations WHERE id = ?1",
                [conversation_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or_else(|| Error::ConversationNotFound {
                id: conversation_id.to_string(),
            })?
            .ok_or_else(|| {
                Error::Conversation(format!(
                    "conversation '{conversation_id}' is not assigned to a project"
                ))
            })?;
        let (task_project, agent_project) = transaction
            .query_row(
                "SELECT tasks.project_id, agents.project_id
                 FROM tasks
                 LEFT JOIN agents ON agents.id = tasks.agent_id
                 WHERE tasks.id = ?1",
                [task_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::TaskNotFound {
                id: task_id.to_string(),
            })?;
        let project_id = consistent_project_id([
            ("conversation", Some(conversation_project.as_str())),
            ("task", task_project.as_deref()),
            ("Agent", agent_project.as_deref()),
        ])?
        .expect("the conversation always supplies a project");
        ensure_task_hierarchy_project(&transaction, task_id, &project_id)?;

        transaction.execute(
            "UPDATE tasks
             SET conversation_id = ?1,
                 project_id = ?2,
                 updated_at = datetime('now')
             WHERE id = ?3",
            rusqlite::params![conversation_id, project_id, task_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn project_id(&self, task_id: &str) -> Result<Option<String>> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT project_id FROM tasks WHERE id = ?1",
                [task_id],
                |row| row.get(0),
            )
            .map_err(|_| Error::TaskNotFound {
                id: task_id.to_string(),
            })
        })
    }

    pub fn list(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Task>> {
        let statuses = status.into_iter().collect::<Vec<_>>();
        self.list_inner(
            TaskListFilter {
                statuses: &statuses,
                agent_id,
                excluded_statuses: &[],
                search: None,
            },
            TaskListPage::first(limit),
            false,
            TaskListOrder::Scheduler,
        )
    }

    /// List visible tasks linked to a conversation, newest first.
    pub fn list_for_conversation(&self, conversation_id: &str) -> Result<Vec<Task>> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT * FROM tasks WHERE conversation_id = ?1 AND hidden = 0
                 ORDER BY created_at DESC",
            )?;
            let mut rows = statement.query([conversation_id])?;
            let mut tasks = Vec::new();
            while let Some(row) = rows.next()? {
                tasks.push(row_to_task(row)?);
            }
            Ok(tasks)
        })
    }

    /// List visible top-level tasks owned by a project, newest first.
    pub fn list_for_project(&self, project_id: &str, limit: i64) -> Result<Vec<Task>> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT * FROM tasks
                 WHERE project_id = ?1 AND hidden = 0 AND parent_task_id IS NULL
                 ORDER BY updated_at DESC, created_at DESC LIMIT ?2",
            )?;
            let mut rows = statement.query(rusqlite::params![project_id, limit])?;
            let mut tasks = Vec::new();
            while let Some(row) = rows.next()? {
                tasks.push(row_to_task(row)?);
            }
            Ok(tasks)
        })
    }

    /// List a page of scheduler-ordered tasks matching any supplied status.
    pub fn list_page(
        &self,
        statuses: &[&str],
        agent_id: Option<&str>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Task>> {
        self.list_inner(
            TaskListFilter {
                statuses,
                agent_id,
                excluded_statuses: &[],
                search,
            },
            TaskListPage { limit, offset },
            false,
            TaskListOrder::Scheduler,
        )
    }

    /// List the most recently updated tasks, excluding statuses that are
    /// presented elsewhere in the UI.
    pub fn list_recent(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        excluded_statuses: &[&str],
        limit: i64,
    ) -> Result<Vec<Task>> {
        let statuses = status.into_iter().collect::<Vec<_>>();
        self.list_inner(
            TaskListFilter {
                statuses: &statuses,
                agent_id,
                excluded_statuses,
                search: None,
            },
            TaskListPage::first(limit),
            false,
            TaskListOrder::Recent,
        )
    }

    /// List a page of recently updated tasks matching any supplied status.
    pub fn list_recent_page(
        &self,
        statuses: &[&str],
        agent_id: Option<&str>,
        excluded_statuses: &[&str],
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Task>> {
        self.list_inner(
            TaskListFilter {
                statuses,
                agent_id,
                excluded_statuses,
                search,
            },
            TaskListPage { limit, offset },
            false,
            TaskListOrder::Recent,
        )
    }

    /// List the most recently updated top-level tasks for each agent.
    pub fn list_recent_per_agent(&self, limit: i64) -> Result<Vec<Task>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "WITH ranked_tasks AS (
                SELECT tasks.*,
                    ROW_NUMBER() OVER (
                        PARTITION BY COALESCE(agent_id, '')
                        ORDER BY updated_at DESC, created_at DESC, id DESC
                    ) AS recency_rank
                FROM tasks
                WHERE hidden = 0 AND parent_task_id IS NULL
            )
            SELECT *
            FROM ranked_tasks
            WHERE recency_rank <= ?1
            ORDER BY updated_at DESC, created_at DESC, id DESC",
        )?;
        let tasks = stmt
            .query_map([limit.max(0)], |row| Ok(row_to_task(row)))
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .filter_map(|r| r.ok())
            .collect();

        Ok(tasks)
    }

    /// List tasks including hidden ones (e.g. IDLE tasks).
    pub fn list_all(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Task>> {
        let statuses = status.into_iter().collect::<Vec<_>>();
        self.list_inner(
            TaskListFilter {
                statuses: &statuses,
                agent_id,
                excluded_statuses: &[],
                search: None,
            },
            TaskListPage::first(limit),
            true,
            TaskListOrder::Scheduler,
        )
    }

    fn list_inner(
        &self,
        filter: TaskListFilter<'_>,
        page: TaskListPage,
        include_hidden: bool,
        order: TaskListOrder,
    ) -> Result<Vec<Task>> {
        let conn = self.db.conn();
        let mut sql = "SELECT * FROM tasks WHERE 1=1".to_string();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if !include_hidden {
            sql.push_str(" AND hidden = 0");
        }

        // Subtasks belong inside their parent, not the top-level list.
        sql.push_str(" AND parent_task_id IS NULL");

        if !filter.statuses.is_empty() {
            sql.push_str(" AND status IN (");
            for (index, status) in filter.statuses.iter().enumerate() {
                if index > 0 {
                    sql.push_str(", ");
                }
                sql.push('?');
                params.push(Box::new((*status).to_string()));
            }
            sql.push(')');
        }
        if let Some(a) = filter.agent_id {
            sql.push_str(" AND agent_id = ?");
            params.push(Box::new(a.to_string()));
        }
        for excluded_status in filter.excluded_statuses {
            sql.push_str(" AND status != ?");
            params.push(Box::new((*excluded_status).to_string()));
        }
        append_task_search(&mut sql, &mut params, filter.search);

        match order {
            TaskListOrder::Scheduler => {
                sql.push_str(" ORDER BY priority DESC, created_at ASC, id ASC LIMIT ? OFFSET ?");
            }
            TaskListOrder::Recent => {
                sql.push_str(
                    " ORDER BY updated_at DESC, created_at DESC, id DESC LIMIT ? OFFSET ?",
                );
            }
        }
        params.push(Box::new(page.limit));
        params.push(Box::new(page.offset.max(0)));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let tasks = stmt
            .query_map(param_refs.as_slice(), |row| Ok(row_to_task(row)))
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .filter_map(|r| r.ok())
            .collect();

        Ok(tasks)
    }

    pub fn update_status(
        &self,
        task_id: &str,
        status: &str,
        agent_id: Option<&str>,
    ) -> Result<Task> {
        self.update_status_with_agent_repository(task_id, status, agent_id, None)
    }

    pub fn update_status_with_agent_repository(
        &self,
        task_id: &str,
        status: &str,
        agent_id: Option<&str>,
        agent_repository: Option<(&str, &str)>,
    ) -> Result<Task> {
        let parsed = TaskStatus::parse(status)?;
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let checked_at = Utc::now().to_rfc3339();

        {
            let conn = self.db.conn();
            let transaction = rusqlite::Transaction::new_unchecked(
                &conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;

            // Verify task exists
            let exists: bool = transaction.query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = ?1",
                [task_id],
                |row| row.get::<_, i64>(0).map(|c| c > 0),
            )?;

            if !exists {
                return Err(Error::TaskNotFound {
                    id: task_id.to_string(),
                });
            }

            // Update status. Reopening a completed task must also clear its
            // old completion timestamp so the API does not report two
            // conflicting states.
            transaction.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2,
                    completed_at = CASE WHEN ?1 = 'completed' THEN ?2 ELSE NULL END
                 WHERE id = ?3",
                rusqlite::params![status, now, task_id],
            )?;

            // Release the external review gate in the same transaction as
            // cancellation so a stale poll cannot revive it after a reopen.
            if parsed == TaskStatus::Cancelled {
                transaction.execute(
                    "UPDATE task_pull_requests SET status = 'cancelled', next_poll_at = NULL,
                        last_checked_at = ?1, last_error = NULL
                     WHERE task_id = ?2 AND status IN ('waiting', 'attention')",
                    rusqlite::params![checked_at, task_id],
                )?;
            }

            // Set agent_id if transitioning to in_progress
            if parsed == TaskStatus::InProgress {
                if let Some(aid) = agent_id {
                    ensure_task_agent_project(&transaction, task_id, aid)?;
                    let previous = synchronize_pull_request_agent(
                        &transaction,
                        task_id,
                        aid,
                        agent_repository,
                    )?;
                    transaction.execute(
                        "UPDATE tasks SET agent_id = ?1 WHERE id = ?2",
                        rusqlite::params![aid, task_id],
                    )?;
                    if let Some(previous) = previous {
                        refresh_logical_session_status(&transaction, &previous)?;
                        if previous != aid {
                            refresh_logical_session_status(&transaction, aid)?;
                        }
                    }
                }
            }
            transaction.commit()?;
        }

        self.get(task_id)
    }

    pub fn update(&self, task_id: &str, req: &UpdateTask) -> Result<Task> {
        self.update_with_agent_repository(task_id, req, None)
    }

    pub fn update_with_agent_repository(
        &self,
        task_id: &str,
        req: &UpdateTask,
        agent_repository: Option<(&str, &str)>,
    ) -> Result<Task> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        {
            let conn = self.db.conn();
            let transaction = rusqlite::Transaction::new_unchecked(
                &conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;

            // Verify task exists
            let exists: bool = transaction.query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = ?1",
                [task_id],
                |row| row.get::<_, i64>(0).map(|c| c > 0),
            )?;

            if !exists {
                return Err(Error::TaskNotFound {
                    id: task_id.to_string(),
                });
            }

            if let Some(ref title) = req.title {
                transaction.execute(
                    "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![title, now, task_id],
                )?;
            }

            if let Some(ref desc) = req.description {
                transaction.execute(
                    "UPDATE tasks SET description = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![desc, now, task_id],
                )?;
            }

            if let Some(ref agent_id) = req.agent_id {
                ensure_task_agent_project(&transaction, task_id, agent_id)?;
                let previous = synchronize_pull_request_agent(
                    &transaction,
                    task_id,
                    agent_id,
                    agent_repository,
                )?;
                transaction.execute(
                    "UPDATE tasks SET agent_id = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![agent_id, now, task_id],
                )?;
                if let Some(previous) = previous {
                    refresh_logical_session_status(&transaction, &previous)?;
                    if previous != *agent_id {
                        refresh_logical_session_status(&transaction, agent_id)?;
                    }
                }
            }

            if let Some(priority) = req.priority {
                transaction.execute(
                    "UPDATE tasks SET priority = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![priority, now, task_id],
                )?;
            }
            transaction.commit()?;
        }

        self.get(task_id)
    }

    pub fn list_subtasks(&self, parent_task_id: &str) -> Result<Vec<Task>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM tasks WHERE parent_task_id = ?1 ORDER BY priority DESC, created_at ASC",
        )?;
        let tasks = stmt
            .query_map([parent_task_id], |row| Ok(row_to_task(row)))
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .filter_map(|r| r.ok())
            .collect();
        Ok(tasks)
    }

    /// Replace the ACP agent's current plan with its latest snapshot.
    /// These rows are normal subtasks so the UI and completion semantics do
    /// not need a second, runner-specific representation.
    pub fn sync_reported_subtasks(
        &self,
        parent_task_id: &str,
        attempt_id: &str,
        items: &[ReportedSubtask],
    ) -> Result<Vec<Task>> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let parent_project = transaction
                .query_row(
                    "SELECT project_id FROM tasks WHERE id = ?1",
                    [parent_task_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| Error::TaskNotFound {
                    id: parent_task_id.to_string(),
                })?;
            let mut stmt = transaction.prepare(
                "SELECT id, context FROM tasks WHERE parent_task_id = ?1 ORDER BY created_at ASC",
            )?;
            let existing: Vec<(String, usize)> = stmt
                .query_map([parent_task_id], |row| {
                    let id: String = row.get(0)?;
                    let context: Option<String> = row.get(1)?;
                    let index = context
                        .as_deref()
                        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                        .filter(|value| {
                            value.get("origin").and_then(|value| value.as_str())
                                == Some("native_plan")
                        })
                        .and_then(|value| value.get("index").and_then(|value| value.as_u64()))
                        .map(|value| value as usize);
                    Ok(index.map(|index| (id, index)))
                })?
                .filter_map(|row| row.ok().flatten())
                .collect();
            drop(stmt);

            for (index, item) in items.iter().enumerate() {
                let completed_at = (item.status == TaskStatus::Completed).then_some(now.as_str());
                let context = serde_json::json!({
                    "origin": "native_plan",
                    "attempt_id": attempt_id,
                    "index": index,
                })
                .to_string();
                if let Some((id, _)) = existing.iter().find(|(_, current)| *current == index) {
                    transaction.execute(
                        "UPDATE tasks SET title = ?1, status = ?2, updated_at = ?3,
                            completed_at = ?4, context = ?5, project_id = ?6 WHERE id = ?7",
                        rusqlite::params![
                            item.title,
                            item.status.as_str(),
                            now,
                            completed_at,
                            context,
                            parent_project,
                            id,
                        ],
                    )?;
                } else {
                    let id = Uuid::new_v4().to_string();
                    transaction.execute(
                        "INSERT INTO tasks
                            (id, title, status, priority, parent_task_id, context,
                             created_at, updated_at, completed_at, project_id)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
                        rusqlite::params![
                            id,
                            item.title,
                            item.status.as_str(),
                            -(index as i32),
                            parent_task_id,
                            context,
                            now,
                            completed_at,
                            parent_project,
                        ],
                    )?;
                }
            }

            for (id, index) in existing {
                if index >= items.len() {
                    transaction.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
                }
            }
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;

        self.list_subtasks(parent_task_id)
    }

    /// A task with steps is complete only when every step is complete. A task
    /// without steps can be completed by its own successful response.
    pub fn subtasks_complete(&self, task_id: &str) -> Result<bool> {
        let subtasks = self.list_subtasks(task_id)?;
        Ok(subtasks.is_empty()
            || subtasks
                .iter()
                .all(|subtask| subtask.status == TaskStatus::Completed))
    }

    /// Complete the current ACP plan after an external lifecycle gate (such
    /// as PR approval) finishes. Only ephemeral native-plan children are
    /// affected; user-created and workflow subtasks retain their own state.
    pub fn complete_reported_subtasks(&self, parent_task_id: &str) -> Result<usize> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, context FROM tasks
                 WHERE parent_task_id = ?1 AND status != 'completed'",
            )?;
            let ids = statement
                .query_map([parent_task_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .filter_map(|row| row.ok())
                .filter_map(|(id, context)| {
                    context
                        .as_deref()
                        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                        .and_then(|value| {
                            (value.get("origin").and_then(|value| value.as_str())
                                == Some("native_plan"))
                            .then_some(id)
                        })
                })
                .collect::<Vec<_>>();
            drop(statement);
            for id in &ids {
                conn.execute(
                    "UPDATE tasks SET status = 'completed', updated_at = ?1,
                        completed_at = ?1 WHERE id = ?2",
                    rusqlite::params![now, id],
                )?;
            }
            Ok::<_, Error>(ids.len())
        })
    }

    /// Complete a task whose steps are done, then roll that completion through
    /// any ready parents. Parents with queued/running work are left active.
    pub fn complete_and_roll_up(&self, task_id: &str, agent_id: Option<&str>) -> Result<Vec<Task>> {
        let mut completed = Vec::new();
        let mut current_id = Some(task_id.to_string());

        while let Some(id) = current_id {
            let Some(task) = self.complete_if_ready(&id)? else {
                break;
            };
            current_id = task.parent_task_id.clone();
            completed.push(task);
        }

        // Completion does not change assignment. Keep this argument for API
        // compatibility with callers that refresh the first task's session.
        let _ = agent_id;

        Ok(completed)
    }

    /// Complete one task only while every precondition and the nonterminal
    /// status still hold in the same transaction.
    fn complete_if_ready(&self, task_id: &str) -> Result<Option<Task>> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let status: String = transaction.query_row(
                "SELECT status FROM tasks WHERE id = ?1",
                [task_id],
                |row| row.get(0),
            )?;
            if matches!(status.as_str(), "completed" | "cancelled") {
                transaction.commit()?;
                return Ok(None);
            }
            let pull_request_gate_satisfied: bool = transaction.query_row(
                "SELECT NOT EXISTS(
                    SELECT 1 FROM task_pull_requests
                    WHERE task_id = ?1 AND status NOT IN ('approved', 'merged', 'cancelled')
                 )",
                [task_id],
                |row| row.get(0),
            )?;
            let subtasks_complete: bool = transaction.query_row(
                "SELECT NOT EXISTS(
                    SELECT 1 FROM tasks
                    WHERE parent_task_id = ?1 AND status != 'completed'
                 )",
                [task_id],
                |row| row.get(0),
            )?;
            let has_open_attempt: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM work_attempts WHERE task_id = ?1
                    AND status IN ('queued', 'preparing', 'running', 'waiting_for_input', 'review')
                )",
                [task_id],
                |row| row.get(0),
            )?;
            if !pull_request_gate_satisfied || !subtasks_complete || has_open_attempt {
                transaction.commit()?;
                return Ok(None);
            }
            let changed = transaction.execute(
                "UPDATE tasks SET status = 'completed', updated_at = ?1, completed_at = ?1
                 WHERE id = ?2 AND status NOT IN ('completed', 'cancelled')",
                rusqlite::params![now, task_id],
            )?;
            if changed != 1 {
                transaction.commit()?;
                return Ok(None);
            }
            let task = transaction.query_row(
                "SELECT * FROM tasks WHERE id = ?1",
                [task_id],
                |row| Ok(row_to_task(row)),
            )??;
            transaction.commit()?;
            Ok(Some(task))
        })
    }

    pub fn delete(&self, task_id: &str) -> Result<()> {
        let conn = self.db.conn();
        conn.execute("DELETE FROM tasks WHERE id = ?1", [task_id])?;
        Ok(())
    }

    pub fn delete_by_status(&self, status: &str) -> Result<i64> {
        let conn = self.db.conn();
        let count = conn.execute("DELETE FROM tasks WHERE status = ?1", [status])?;
        Ok(count as i64)
    }

    pub fn counts(&self) -> Result<TaskCounts> {
        self.counts_for_search(None)
    }

    /// Count top-level tasks by status, optionally restricted to tasks whose
    /// visible task text or conversation contains every search term.
    pub fn counts_for_search(&self, search: Option<&str>) -> Result<TaskCounts> {
        let conn = self.db.conn();
        let mut sql = "SELECT status, COUNT(*) as count FROM tasks
             WHERE hidden = 0 AND parent_task_id IS NULL"
            .to_string();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        append_task_search(&mut sql, &mut params, search);
        sql.push_str(" GROUP BY status");
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|param| param.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();

        let mut counts = TaskCounts {
            pending: 0,
            in_progress: 0,
            waiting_for_input: 0,
            blocked: 0,
            completed: 0,
            cancelled: 0,
        };

        for (status, count) in rows {
            match status.as_str() {
                "pending" => counts.pending = count,
                "in_progress" => counts.in_progress = count,
                "waiting_for_input" => counts.waiting_for_input = count,
                "blocked" => counts.blocked = count,
                "completed" => counts.completed = count,
                "cancelled" => counts.cancelled = count,
                _ => {}
            }
        }

        Ok(counts)
    }

    // -- Dependency methods (ADR-020) --

    /// Add a dependency: task_id cannot start until depends_on_id completes.
    /// Returns error if this would create a cycle.
    pub fn add_dependency(&self, task_id: &str, depends_on_id: &str) -> Result<()> {
        if task_id == depends_on_id {
            return Err(Error::Task("a task cannot depend on itself".into()));
        }
        // Cycle detection: DFS from depends_on_id — can we reach task_id?
        if self.would_create_cycle(task_id, depends_on_id)? {
            return Err(Error::Task(format!(
                "cannot add dependency: would create a cycle ({task_id} → {depends_on_id} → ... → {task_id})"
            )));
        }
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO task_dependencies (task_id, depends_on_id) VALUES (?1, ?2)",
                rusqlite::params![task_id, depends_on_id],
            )
        })?;
        Ok(())
    }

    /// Check if adding task_id → depends_on_id would create a cycle.
    fn would_create_cycle(&self, task_id: &str, depends_on_id: &str) -> Result<bool> {
        // DFS from depends_on_id: can we reach task_id?
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![depends_on_id.to_string()];
        while let Some(current) = stack.pop() {
            if current == task_id {
                return Ok(true);
            }
            if visited.insert(current.clone()) {
                for dep in self.get_dependencies(&current)? {
                    stack.push(dep);
                }
            }
        }
        Ok(false)
    }

    /// Get task IDs that this task depends on (must complete before this task).
    pub fn get_dependencies(&self, task_id: &str) -> Result<Vec<String>> {
        self.db.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT depends_on_id FROM task_dependencies WHERE task_id = ?1")?;
            let deps = stmt
                .query_map([task_id], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(deps)
        })
    }

    /// Get task IDs that depend on this task (will be unblocked when this completes).
    pub fn get_dependents(&self, task_id: &str) -> Result<Vec<String>> {
        self.db.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT task_id FROM task_dependencies WHERE depends_on_id = ?1")?;
            let deps = stmt
                .query_map([task_id], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(deps)
        })
    }

    /// Check if all dependencies of a task are completed.
    pub fn is_ready(&self, task_id: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            // Count dependencies that are NOT completed
            let unmet: i64 = conn.query_row(
                "SELECT COUNT(*) FROM task_dependencies d
                 JOIN tasks t ON t.id = d.depends_on_id
                 WHERE d.task_id = ?1 AND t.status != 'completed'",
                [task_id],
                |row| row.get(0),
            )?;
            Ok(unmet == 0)
        })
    }

    /// Get IDs of incomplete dependencies (for the blocked_by field).
    pub fn get_blockers(&self, task_id: &str) -> Result<Vec<String>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT d.depends_on_id FROM task_dependencies d
                 JOIN tasks t ON t.id = d.depends_on_id
                 WHERE d.task_id = ?1 AND t.status != 'completed'",
            )?;
            let blockers = stmt
                .query_map([task_id], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(blockers)
        })
    }

    /// Batch-create tasks with ref-based dependencies (ADR-020).
    /// Each task has an optional `ref` string for cross-referencing within
    /// the batch, and `depends_on` lists ref strings of prerequisite tasks.
    pub fn create_batch(
        &self,
        tasks: &[BatchTaskInput],
        parent_task_id: Option<&str>,
    ) -> Result<Vec<Task>> {
        let mut ref_to_id: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut created = Vec::new();

        // First pass: create all tasks and map refs to UUIDs
        for input in tasks {
            let task = self.create(&CreateTask {
                title: input.title.clone(),
                description: input.description.clone(),
                agent_id: input.agent_id.clone(),
                parent_task_id: parent_task_id.map(|s| s.to_string()),
                sop_id: None,
                conversation_id: None,
                priority: input.priority,
                context: Some(serde_json::json!({
                    "session_mode": if input.new_session { "new" } else { "continue" },
                })),
            })?;
            if let Some(ref r) = input.ref_name {
                ref_to_id.insert(r.clone(), task.id.clone());
            }
            created.push(task);
        }

        // Second pass: add dependency edges
        for (i, input) in tasks.iter().enumerate() {
            if let Some(ref deps) = input.depends_on {
                let task_id = &created[i].id;
                for dep_ref in deps {
                    // Resolve ref to UUID — could be a batch ref or an existing task UUID
                    let dep_id = ref_to_id
                        .get(dep_ref)
                        .cloned()
                        .unwrap_or_else(|| dep_ref.clone());
                    self.add_dependency(task_id, &dep_id)?;
                }
            }
        }

        Ok(created)
    }
}

fn consistent_project_id<const N: usize>(
    candidates: [(&str, Option<&str>); N],
) -> Result<Option<String>> {
    let mut selected: Option<(&str, &str)> = None;
    for (label, project_id) in candidates {
        let Some(project_id) = project_id else {
            continue;
        };
        if let Some((selected_label, selected_project_id)) = selected {
            if selected_project_id != project_id {
                return Err(Error::Task(format!(
                    "task {label} belongs to project '{project_id}', but its {selected_label} belongs to project '{selected_project_id}'"
                )));
            }
        } else {
            selected = Some((label, project_id));
        }
    }
    Ok(selected.map(|(_, project_id)| project_id.to_string()))
}

fn ensure_task_agent_project(
    conn: &rusqlite::Connection,
    task_id: &str,
    agent_id: &str,
) -> Result<()> {
    let task_project = conn.query_row(
        "SELECT project_id FROM tasks WHERE id = ?1",
        [task_id],
        |row| row.get::<_, Option<String>>(0),
    )?;
    let agent_project = conn
        .query_row(
            "SELECT project_id FROM agents WHERE id = ?1",
            [agent_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let project_id = consistent_project_id([
        ("task", task_project.as_deref()),
        ("Agent", agent_project.as_deref()),
    ])?;
    if let Some(project_id) = project_id {
        ensure_task_hierarchy_project(conn, task_id, &project_id)?;
    }
    Ok(())
}

/// Adopt an otherwise-projectless parent/child component into one Project.
///
/// Project assignment is an invariant of the complete task hierarchy rather
/// than one row. Callers hold an IMMEDIATE transaction, so validation and the
/// null-only adoption cannot race another hierarchy mutation. Any existing
/// conflicting scope fails closed without changing part of the tree.
fn ensure_task_hierarchy_project(
    conn: &rusqlite::Connection,
    task_id: &str,
    project_id: &str,
) -> Result<()> {
    let has_conflicting_scope: bool = conn.query_row(
        "WITH RECURSIVE
         task_links(origin_id, linked_id) AS (
             SELECT id, parent_task_id FROM tasks WHERE parent_task_id IS NOT NULL
             UNION
             SELECT parent_task_id, id FROM tasks WHERE parent_task_id IS NOT NULL
         ),
         hierarchy(id) AS (
             SELECT ?1
             UNION
             SELECT task_links.linked_id
             FROM hierarchy
             JOIN task_links ON task_links.origin_id = hierarchy.id
         )
         SELECT EXISTS(
             SELECT 1
             FROM tasks
             JOIN hierarchy ON hierarchy.id = tasks.id
             WHERE (tasks.project_id IS NOT NULL AND tasks.project_id <> ?2)
                OR EXISTS (
                    SELECT 1 FROM agents
                    WHERE agents.id = tasks.agent_id
                      AND agents.project_id IS NOT NULL
                      AND agents.project_id <> ?2
                )
                OR EXISTS (
                    SELECT 1 FROM conversations
                    WHERE conversations.id = tasks.conversation_id
                      AND conversations.project_id IS NOT NULL
                      AND conversations.project_id <> ?2
                )
         )",
        rusqlite::params![task_id, project_id],
        |row| row.get(0),
    )?;
    if has_conflicting_scope {
        return Err(Error::Task(format!(
            "task hierarchy cannot be assigned to project '{project_id}' because part of it belongs to another project"
        )));
    }

    conn.execute(
        "WITH RECURSIVE
         task_links(origin_id, linked_id) AS (
             SELECT id, parent_task_id FROM tasks WHERE parent_task_id IS NOT NULL
             UNION
             SELECT parent_task_id, id FROM tasks WHERE parent_task_id IS NOT NULL
         ),
         hierarchy(id) AS (
             SELECT ?1
             UNION
             SELECT task_links.linked_id
             FROM hierarchy
             JOIN task_links ON task_links.origin_id = hierarchy.id
         )
         UPDATE tasks
         SET project_id = ?2
         WHERE project_id IS NULL AND id IN (SELECT id FROM hierarchy)",
        rusqlite::params![task_id, project_id],
    )?;
    Ok(())
}

fn synchronize_pull_request_agent(
    conn: &rusqlite::Connection,
    task_id: &str,
    agent_id: &str,
    agent_repository: Option<(&str, &str)>,
) -> Result<Option<String>> {
    let has_active_pull_request: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM task_pull_requests
            WHERE task_id = ?1 AND status IN ('waiting', 'attention')
         )",
        [task_id],
        |row| row.get(0),
    )?;
    if !has_active_pull_request {
        return Ok(None);
    }
    if agent_id.trim().is_empty() {
        return Err(Error::Task(
            "cannot unassign a task while pull-request review monitoring is active".into(),
        ));
    }
    let previous_agent_id: String = conn.query_row(
        "SELECT agent_id FROM tasks WHERE id = ?1",
        [task_id],
        |row| row.get(0),
    )?;
    let running_dispatch: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM task_queue
            WHERE task_id = ?1 AND status = 'running'
         )",
        [task_id],
        |row| row.get(0),
    )?;
    if previous_agent_id != agent_id && running_dispatch {
        return Err(Error::Task(
            "cannot reassign a task while its agent turn is running; stop it first".into(),
        ));
    }
    if previous_agent_id != agent_id {
        let Some((owner, repo)) = agent_repository else {
            return Err(Error::Task(
                "cannot reassign a monitored pull request because the destination agent's project-scoped GitHub repository could not be verified".into(),
            ));
        };
        let incompatible_repository: bool = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM task_pull_requests
                WHERE task_id = ?1 AND status IN ('waiting', 'attention')
                  AND (owner != ?2 COLLATE NOCASE OR repo != ?3 COLLATE NOCASE)
             )",
            rusqlite::params![task_id, owner, repo],
            |row| row.get(0),
        )?;
        if incompatible_repository {
            return Err(Error::Task(
                "cannot reassign a monitored pull request to an agent using a different GitHub repository".into(),
            ));
        }
        let destination_lane_reserved: bool = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM task_pull_requests destination_pr
                JOIN tasks destination_task ON destination_task.id = destination_pr.task_id
                WHERE destination_pr.agent_id = ?1
                  AND destination_pr.task_id != ?2
                  AND destination_pr.status IN ('waiting', 'attention')
                  AND destination_task.status NOT IN ('completed', 'cancelled')
             )",
            rusqlite::params![agent_id, task_id],
            |row| row.get(0),
        )?;
        if destination_lane_reserved {
            return Err(Error::Task(
                "cannot reassign a monitored pull request because the destination agent is already reserved by another active pull-request review task".into(),
            ));
        }
    }
    conn.execute(
        "INSERT OR IGNORE INTO logical_sessions (id, agent_id) VALUES (?1, ?1)",
        [agent_id],
    )?;
    conn.execute(
        "UPDATE task_pull_requests SET agent_id = ?1
         WHERE task_id = ?2 AND status IN ('waiting', 'attention')",
        rusqlite::params![agent_id, task_id],
    )?;
    conn.execute(
        "UPDATE session_events SET session_id = ?1
         WHERE attempt_id IN (
            SELECT attempt_id FROM task_queue
            WHERE task_id = ?2 AND status = 'queued' AND attempt_id IS NOT NULL
         )",
        rusqlite::params![agent_id, task_id],
    )?;
    conn.execute(
        "UPDATE attempt_artifacts SET session_id = ?1
         WHERE attempt_id IN (
            SELECT attempt_id FROM task_queue
            WHERE task_id = ?2 AND status = 'queued' AND attempt_id IS NOT NULL
         )",
        rusqlite::params![agent_id, task_id],
    )?;
    conn.execute(
        "UPDATE work_attempts SET session_id = ?1
         WHERE id IN (
            SELECT attempt_id FROM task_queue
            WHERE task_id = ?2 AND status = 'queued' AND attempt_id IS NOT NULL
         ) AND status = 'queued'",
        rusqlite::params![agent_id, task_id],
    )?;
    conn.execute(
        "UPDATE task_queue SET agent_id = ?1
         WHERE task_id = ?2 AND status = 'queued'",
        rusqlite::params![agent_id, task_id],
    )?;
    conn.execute(
        "UPDATE tasks SET session_id = ?1 WHERE id = ?2",
        rusqlite::params![agent_id, task_id],
    )?;
    Ok(Some(previous_agent_id))
}

fn refresh_logical_session_status(conn: &rusqlite::Connection, session_id: &str) -> Result<()> {
    let active_attempt: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM work_attempts
            WHERE session_id = ?1 AND status IN ('preparing', 'running', 'review')
         )",
        [session_id],
        |row| row.get(0),
    )?;
    let queued_attempt: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM work_attempts WHERE session_id = ?1 AND status = 'queued'
         )",
        [session_id],
        |row| row.get(0),
    )?;
    let waiting_task: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM tasks WHERE agent_id = ?1 AND status = 'waiting_for_input'
         )",
        [session_id],
        |row| row.get(0),
    )?;
    let blocked_task: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM tasks WHERE agent_id = ?1 AND status = 'blocked'
         )",
        [session_id],
        |row| row.get(0),
    )?;
    let status = if active_attempt {
        "running"
    } else if queued_attempt {
        "queued"
    } else if waiting_task {
        "waiting_for_input"
    } else if blocked_task {
        "blocked"
    } else {
        "idle"
    };
    conn.execute(
        "UPDATE logical_sessions SET status = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        rusqlite::params![status, session_id],
    )?;
    Ok(())
}

fn append_task_search(
    sql: &mut String,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    search: Option<&str>,
) {
    let mut seen = std::collections::HashSet::new();
    let terms = search
        .into_iter()
        .flat_map(str::split_whitespace)
        .map(crate::db::task_search_key)
        .filter(|term| !term.is_empty())
        .filter(|term| seen.insert(term.clone()));

    for term in terms {
        sql.push_str(
            " AND (
                instr(xpressclaw_task_search_key(tasks.title), ?) > 0
                OR instr(xpressclaw_task_search_key(COALESCE(tasks.description, '')), ?) > 0
                OR EXISTS (
                    SELECT 1 FROM task_messages
                    WHERE task_messages.task_id = tasks.id
                      AND instr(xpressclaw_task_search_key(task_messages.content), ?) > 0
                )
                OR EXISTS (
                    SELECT 1 FROM session_events
                    WHERE session_events.task_id = tasks.id
                      AND session_events.event_type IN ('runner_progress', 'agent_thought')
                      AND instr(xpressclaw_task_search_key(session_events.summary), ?) > 0
                )
            )",
        );
        for _ in 0..4 {
            params.push(Box::new(term.clone()));
        }
    }
}

/// Input for batch task creation.
#[derive(Debug, Deserialize)]
pub struct BatchTaskInput {
    /// Local reference name for cross-referencing within the batch.
    #[serde(rename = "ref")]
    pub ref_name: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub agent_id: Option<String>,
    pub priority: Option<i32>,
    /// Start a new ACP conversation instead of continuing the
    /// project's active one. Dependencies still take precedence so a task can
    /// continue the work it is explicitly chained from.
    #[serde(default)]
    pub new_session: bool,
    /// Ref names or existing task UUIDs that must complete first.
    pub depends_on: Option<Vec<String>>,
}

fn row_to_task(row: &rusqlite::Row) -> Result<Task> {
    let context_str: Option<String> = row.get("context")?;
    let context = context_str
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());

    let status_str: String = row.get("status")?;

    Ok(Task {
        id: row.get("id")?,
        title: row.get("title")?,
        description: row.get("description")?,
        status: TaskStatus::parse(&status_str)?,
        priority: row.get("priority")?,
        agent_id: row.get("agent_id")?,
        parent_task_id: row.get("parent_task_id")?,
        sop_id: row.get("sop_id")?,
        conversation_id: row.get("conversation_id").unwrap_or(None),
        project_id: row.get("project_id").unwrap_or(None),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        completed_at: row.get("completed_at")?,
        context,
        task_type: row
            .get::<_, String>("task_type")
            .unwrap_or_else(|_| "normal".to_string()),
        hidden: row.get::<_, i32>("hidden").unwrap_or(0) != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, TaskBoard) {
        let db = Arc::new(Database::open_memory().unwrap());
        let board = TaskBoard::new(db.clone());
        (db, board)
    }

    #[test]
    fn test_create_and_get_task() {
        let (_, board) = setup();
        let task = board
            .create(&CreateTask {
                title: "Test task".to_string(),
                description: Some("A test".to_string()),
                agent_id: Some("atlas".to_string()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: Some(5),
                context: None,
            })
            .unwrap();

        assert_eq!(task.title, "Test task");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.priority, 5);

        let fetched = board.get(&task.id).unwrap();
        assert_eq!(fetched.id, task.id);
    }

    #[test]
    fn tasks_preserve_their_project_boundary() {
        let (db, board) = setup();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One'), ('two', 'Two');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'one'),
                        ('other', 'Other', 'native', '{}', 'two');
                 INSERT INTO conversations (id, title, project_id)
                 VALUES ('launch', 'Launch', 'one');",
            )
        })
        .unwrap();

        let task = board
            .create(&CreateTask {
                title: "Ship it".into(),
                agent_id: Some("atlas".into()),
                conversation_id: Some("launch".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(task.project_id.as_deref(), Some("one"));

        let mismatch = board.create(&CreateTask {
            title: "Wrong project".into(),
            agent_id: Some("other".into()),
            conversation_id: Some("launch".into()),
            ..Default::default()
        });
        assert!(matches!(mismatch, Err(Error::Task(_))));

        let reassignment = board.update(
            &task.id,
            &UpdateTask {
                title: None,
                description: None,
                agent_id: Some("other".into()),
                priority: None,
            },
        );
        assert!(matches!(reassignment, Err(Error::Task(_))));

        let detached = board
            .create(&CreateTask {
                title: "Adopt the conversation project".into(),
                ..Default::default()
            })
            .unwrap();
        board.set_conversation_id(&detached.id, "launch").unwrap();
        let detached = board.get(&detached.id).unwrap();
        assert_eq!(detached.conversation_id.as_deref(), Some("launch"));
        assert_eq!(detached.project_id.as_deref(), Some("one"));

        let legacy_parent = board
            .create(&CreateTask {
                title: "Legacy parent".into(),
                ..Default::default()
            })
            .unwrap();
        let legacy_child = board
            .create(&CreateTask {
                title: "Legacy child".into(),
                parent_task_id: Some(legacy_parent.id.clone()),
                ..Default::default()
            })
            .unwrap();
        board
            .set_conversation_id(&legacy_child.id, "launch")
            .unwrap();
        assert_eq!(
            board.get(&legacy_parent.id).unwrap().project_id.as_deref(),
            Some("one")
        );
        assert_eq!(
            board.get(&legacy_child.id).unwrap().project_id.as_deref(),
            Some("one")
        );

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conversations (id, title, project_id)
                 VALUES ('other-launch', 'Other launch', 'two')",
                [],
            )
        })
        .unwrap();
        let mismatch = board.set_conversation_id(&task.id, "other-launch");
        assert!(matches!(mismatch, Err(Error::Task(_))));
        assert_eq!(
            board.get(&task.id).unwrap().conversation_id.as_deref(),
            Some("launch")
        );
        assert!(matches!(
            board.set_conversation_id(&task.id, "missing"),
            Err(Error::ConversationNotFound { .. })
        ));
        assert!(matches!(
            board.set_conversation_id("missing", "launch"),
            Err(Error::TaskNotFound { .. })
        ));
    }

    #[test]
    fn agent_created_conversation_tasks_require_current_membership() {
        let (db, board) = setup();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'one');
                 INSERT INTO conversations (id, title, project_id)
                 VALUES ('launch', 'Launch', 'one');
                 INSERT INTO conversation_participants
                 (conversation_id, participant_type, participant_id)
                 VALUES ('launch', 'agent', 'atlas');",
            )
        })
        .unwrap();

        board
            .create_for_conversation_agent(
                &CreateTask {
                    title: "Authorized work".into(),
                    agent_id: Some("atlas".into()),
                    conversation_id: Some("launch".into()),
                    ..Default::default()
                },
                "atlas",
            )
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM conversation_participants
                 WHERE conversation_id = 'launch' AND participant_id = 'atlas'",
                [],
            )
        })
        .unwrap();

        let rejected = board.create_for_conversation_agent(
            &CreateTask {
                title: "Stale work".into(),
                agent_id: Some("atlas".into()),
                conversation_id: Some("launch".into()),
                ..Default::default()
            },
            "atlas",
        );
        assert!(matches!(rejected, Err(Error::Conversation(_))));
        assert_eq!(board.list_for_conversation("launch").unwrap().len(), 1);
    }

    #[test]
    fn child_tasks_inherit_their_parent_project() {
        let (db, board) = setup();
        db.with_conn(|conn| {
            conn.execute("INSERT INTO projects (id, name) VALUES ('one', 'One')", [])
        })
        .unwrap();
        let parent = board
            .create(&CreateTask {
                title: "Parent".into(),
                context: Some(serde_json::json!({ "project_id": "one" })),
                ..Default::default()
            })
            .unwrap();
        let child = board
            .create(&CreateTask {
                title: "Child".into(),
                parent_task_id: Some(parent.id),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(child.project_id.as_deref(), Some("one"));
    }

    #[test]
    fn projectless_parent_rejects_a_newly_scoped_child() {
        let (db, board) = setup();
        let parent = board
            .create(&CreateTask {
                title: "Legacy parent".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(parent.project_id.is_none());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'one');",
            )
        })
        .unwrap();

        let error = board
            .create(&CreateTask {
                title: "Scoped child".into(),
                agent_id: Some("atlas".into()),
                parent_task_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(error.to_string().contains("projectless parent task"));
        assert!(board.list_subtasks(&parent.id).unwrap().is_empty());
    }

    #[test]
    fn agent_assignment_adopts_the_complete_projectless_hierarchy() {
        let (db, board) = setup();
        let parent = board
            .create(&CreateTask {
                title: "Legacy parent".into(),
                ..Default::default()
            })
            .unwrap();
        let child = board
            .create(&CreateTask {
                title: "Legacy child".into(),
                parent_task_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let grandchild = board
            .create(&CreateTask {
                title: "Legacy grandchild".into(),
                parent_task_id: Some(child.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let unrelated = board
            .create(&CreateTask {
                title: "Unrelated".into(),
                ..Default::default()
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'one');",
            )
        })
        .unwrap();

        board
            .update(
                &child.id,
                &UpdateTask {
                    title: None,
                    description: None,
                    agent_id: Some("atlas".into()),
                    priority: None,
                },
            )
            .unwrap();

        for task_id in [&parent.id, &child.id, &grandchild.id] {
            assert_eq!(
                board.get(task_id).unwrap().project_id.as_deref(),
                Some("one")
            );
        }
        assert!(board.get(&unrelated.id).unwrap().project_id.is_none());
    }

    #[test]
    fn agent_assignment_rejects_a_conflicting_hierarchy_atomically() {
        let (db, board) = setup();
        let parent = board
            .create(&CreateTask {
                title: "Legacy parent".into(),
                ..Default::default()
            })
            .unwrap();
        let child = board
            .create(&CreateTask {
                title: "Legacy child".into(),
                parent_task_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One'), ('two', 'Two');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'one');",
            )?;
            conn.execute(
                "UPDATE tasks SET project_id = 'two' WHERE id = ?1",
                [&child.id],
            )
        })
        .unwrap();

        let error = board
            .update_status(&parent.id, "in_progress", Some("atlas"))
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("part of it belongs to another project"));
        let parent = board.get(&parent.id).unwrap();
        assert_eq!(parent.status, TaskStatus::Pending);
        assert!(parent.agent_id.is_none());
        assert!(parent.project_id.is_none());
        assert_eq!(
            board.get(&child.id).unwrap().project_id.as_deref(),
            Some("two")
        );
    }

    #[test]
    fn test_update_status() {
        let (_, board) = setup();
        let task = board
            .create(&CreateTask {
                title: "Status test".to_string(),
                description: None,
                agent_id: None,
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        let updated = board
            .update_status(&task.id, "in_progress", Some("atlas"))
            .unwrap();
        assert_eq!(updated.status, TaskStatus::InProgress);
        assert_eq!(updated.agent_id.as_deref(), Some("atlas"));

        let completed = board.update_status(&task.id, "completed", None).unwrap();
        assert_eq!(completed.status, TaskStatus::Completed);
        assert!(completed.completed_at.is_some());

        let reopened = board.update_status(&task.id, "pending", None).unwrap();
        assert_eq!(reopened.status, TaskStatus::Pending);
        assert!(reopened.completed_at.is_none());
    }

    #[test]
    fn task_reassignment_transfers_active_pull_request_monitoring() {
        let (db, board) = setup();
        let task = board
            .create(&CreateTask {
                title: "Review ownership".to_string(),
                agent_id: Some("project-codex".to_string()),
                ..Default::default()
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO task_pull_requests
                    (task_id, agent_id, owner, repo, number, url, status,
                     started_at, expires_at, next_poll_at, poll_interval_seconds)
                 VALUES (?1, 'project-codex', 'XpressAI', 'xpressclaw', 151,
                         'https://github.com/XpressAI/xpressclaw/pull/151', 'waiting',
                         '2026-08-10T00:00:00Z', '2026-08-24T00:00:00Z',
                         '2026-08-10T00:00:00Z', 15)",
                [&task.id],
            )
            .unwrap();
        });
        let queue = crate::tasks::queue::TaskQueue::new(db.clone());
        let queued = queue
            .enqueue_continuation(&task.id, "project-codex")
            .unwrap()
            .unwrap();
        let attempt_id = queued.attempt_id.clone().unwrap();

        let unavailable_error = board
            .update(
                &task.id,
                &UpdateTask {
                    title: None,
                    description: None,
                    agent_id: Some("unverified-codex".to_string()),
                    priority: None,
                },
            )
            .unwrap_err();
        assert!(unavailable_error
            .to_string()
            .contains("could not be verified"));
        assert_eq!(
            board.get(&task.id).unwrap().agent_id.as_deref(),
            Some("project-codex")
        );

        let incompatible_error = board
            .update_with_agent_repository(
                &task.id,
                &UpdateTask {
                    title: None,
                    description: None,
                    agent_id: Some("other-project-codex".to_string()),
                    priority: None,
                },
                Some(("XpressAI", "different-repository")),
            )
            .unwrap_err();
        assert!(incompatible_error
            .to_string()
            .contains("different GitHub repository"));
        assert_eq!(
            board.get(&task.id).unwrap().agent_id.as_deref(),
            Some("project-codex")
        );

        let destination_task = board
            .create(&CreateTask {
                title: "Existing destination review".to_string(),
                agent_id: Some("reviewer-codex".to_string()),
                ..Default::default()
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO task_pull_requests
                    (task_id, agent_id, owner, repo, number, url, status,
                     started_at, expires_at, next_poll_at, poll_interval_seconds)
                 VALUES (?1, 'reviewer-codex', 'XpressAI', 'xpressclaw', 152,
                         'https://github.com/XpressAI/xpressclaw/pull/152', 'waiting',
                         '2026-08-10T00:00:00Z', '2026-08-24T00:00:00Z',
                         '2026-08-10T00:00:00Z', 15)",
                [&destination_task.id],
            )
            .unwrap();
        });
        let reserved_error = board
            .update_with_agent_repository(
                &task.id,
                &UpdateTask {
                    title: None,
                    description: None,
                    agent_id: Some("reviewer-codex".to_string()),
                    priority: None,
                },
                Some(("XpressAI", "xpressclaw")),
            )
            .unwrap_err();
        assert!(reserved_error
            .to_string()
            .contains("already reserved by another active pull-request review task"));
        assert_eq!(
            board.get(&task.id).unwrap().agent_id.as_deref(),
            Some("project-codex")
        );
        board
            .update_status(&destination_task.id, "cancelled", None)
            .unwrap();

        let reassigned = board
            .update_with_agent_repository(
                &task.id,
                &UpdateTask {
                    title: None,
                    description: None,
                    agent_id: Some("reviewer-codex".to_string()),
                    priority: None,
                },
                Some(("xpressai", "XPRESSCLAW")),
            )
            .unwrap();
        assert_eq!(reassigned.agent_id.as_deref(), Some("reviewer-codex"));
        let monitored_agent = || {
            db.with_conn(|conn| {
                conn.query_row(
                    "SELECT agent_id FROM task_pull_requests WHERE task_id = ?1",
                    [&task.id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
            })
        };
        assert_eq!(monitored_agent(), "reviewer-codex");
        assert_eq!(queue.get(queued.id).unwrap().agent_id, "reviewer-codex");
        assert_eq!(
            crate::sessions::SessionManager::new(db.clone())
                .get_attempt(&attempt_id)
                .unwrap()
                .session_id,
            "reviewer-codex"
        );
        assert!(queue
            .enqueue_continuation(&task.id, "reviewer-codex")
            .unwrap()
            .is_none());
        let (task_session, event_session, previous_status, current_status): (
            String,
            String,
            String,
            String,
        ) = db
            .with_conn(|conn| {
                let task_session = conn.query_row(
                    "SELECT session_id FROM tasks WHERE id = ?1",
                    [&task.id],
                    |row| row.get(0),
                )?;
                let event_session = conn.query_row(
                    "SELECT session_id FROM session_events WHERE attempt_id = ?1",
                    [&attempt_id],
                    |row| row.get(0),
                )?;
                let previous_status = conn.query_row(
                    "SELECT status FROM logical_sessions WHERE id = 'project-codex'",
                    [],
                    |row| row.get(0),
                )?;
                let current_status = conn.query_row(
                    "SELECT status FROM logical_sessions WHERE id = 'reviewer-codex'",
                    [],
                    |row| row.get(0),
                )?;
                Ok::<_, rusqlite::Error>((
                    task_session,
                    event_session,
                    previous_status,
                    current_status,
                ))
            })
            .unwrap();
        assert_eq!(task_session, "reviewer-codex");
        assert_eq!(event_session, "reviewer-codex");
        assert_eq!(previous_status, "idle");
        assert_eq!(current_status, "queued");

        board
            .update_status_with_agent_repository(
                &task.id,
                "in_progress",
                Some("final-codex"),
                Some(("XpressAI", "xpressclaw")),
            )
            .unwrap();
        assert_eq!(monitored_agent(), "final-codex");
        assert_eq!(queue.get(queued.id).unwrap().agent_id, "final-codex");
        assert_eq!(
            crate::sessions::SessionManager::new(db.clone())
                .get_attempt(&attempt_id)
                .unwrap()
                .session_id,
            "final-codex"
        );

        let error = board
            .update(
                &task.id,
                &UpdateTask {
                    title: None,
                    description: None,
                    agent_id: Some(String::new()),
                    priority: None,
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("cannot unassign"));
        assert_eq!(
            board.get(&task.id).unwrap().agent_id.as_deref(),
            Some("final-codex")
        );
        assert_eq!(monitored_agent(), "final-codex");

        assert_eq!(queue.claim("final-codex").unwrap().unwrap().id, queued.id);
        let error = board
            .update(
                &task.id,
                &UpdateTask {
                    title: None,
                    description: None,
                    agent_id: Some("late-codex".to_string()),
                    priority: None,
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("turn is running"));
        assert_eq!(
            board.get(&task.id).unwrap().agent_id.as_deref(),
            Some("final-codex")
        );
        assert_eq!(monitored_agent(), "final-codex");
    }

    #[test]
    fn syncs_native_plan_steps_and_rolls_up_completion() {
        let (db, board) = setup();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('developer', 'Developer', 'native', '{}', 'one');",
            )
        })
        .unwrap();
        let parent = board
            .create(&CreateTask {
                title: "Implement feature".to_string(),
                description: None,
                agent_id: Some("developer".to_string()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        let initial = board
            .sync_reported_subtasks(
                &parent.id,
                "attempt-1",
                &[
                    ReportedSubtask {
                        title: "Inspect the code".to_string(),
                        status: TaskStatus::InProgress,
                    },
                    ReportedSubtask {
                        title: "Run tests".to_string(),
                        status: TaskStatus::Pending,
                    },
                ],
            )
            .unwrap();
        assert_eq!(initial.len(), 2);
        assert!(initial
            .iter()
            .all(|task| task.project_id.as_deref() == Some("one")));
        assert!(!board.subtasks_complete(&parent.id).unwrap());
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE tasks SET project_id = NULL WHERE id = ?1",
                [&initial[0].id],
            )
        })
        .unwrap();

        let updated = board
            .sync_reported_subtasks(
                &parent.id,
                "attempt-1",
                &[
                    ReportedSubtask {
                        title: "Inspect the code".to_string(),
                        status: TaskStatus::Completed,
                    },
                    ReportedSubtask {
                        title: "Run the full test suite".to_string(),
                        status: TaskStatus::Completed,
                    },
                ],
            )
            .unwrap();
        assert_eq!(updated.len(), 2);
        assert_eq!(updated[1].title, "Run the full test suite");
        assert!(updated
            .iter()
            .all(|task| task.project_id.as_deref() == Some("one")));
        assert!(board.subtasks_complete(&parent.id).unwrap());

        let completed = board
            .complete_and_roll_up(&parent.id, Some("developer"))
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(board.get(&parent.id).unwrap().status, TaskStatus::Completed);
    }

    #[test]
    fn roll_up_never_overwrites_a_cancelled_task() {
        let (_, board) = setup();
        let task = board
            .create(&CreateTask {
                title: "Cancelled work".to_string(),
                description: None,
                agent_id: Some("developer".to_string()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        board.update_status(&task.id, "cancelled", None).unwrap();

        assert!(board
            .complete_and_roll_up(&task.id, Some("developer"))
            .unwrap()
            .is_empty());
        assert_eq!(board.get(&task.id).unwrap().status, TaskStatus::Cancelled);
    }

    #[test]
    fn completing_the_last_child_completes_its_parent() {
        let (_, board) = setup();
        let parent = board
            .create(&CreateTask {
                title: "Parent".to_string(),
                description: None,
                agent_id: None,
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let make_child = |title: &str| {
            board
                .create(&CreateTask {
                    title: title.to_string(),
                    description: None,
                    agent_id: None,
                    parent_task_id: Some(parent.id.clone()),
                    sop_id: None,
                    conversation_id: None,
                    priority: None,
                    context: None,
                })
                .unwrap()
        };
        let first = make_child("First");
        let second = make_child("Second");

        let first_completion = board.complete_and_roll_up(&first.id, None).unwrap();
        assert_eq!(first_completion.len(), 1);
        assert_eq!(board.get(&parent.id).unwrap().status, TaskStatus::Pending);

        let second_completion = board.complete_and_roll_up(&second.id, None).unwrap();
        assert_eq!(second_completion.len(), 2);
        assert_eq!(board.get(&parent.id).unwrap().status, TaskStatus::Completed);
    }

    #[test]
    fn test_list_and_counts() {
        let (_, board) = setup();
        let first = board
            .create(&CreateTask {
                title: "Task 1".to_string(),
                description: None,
                agent_id: Some("atlas".to_string()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        board
            .create(&CreateTask {
                title: "Task 2".to_string(),
                description: None,
                agent_id: Some("atlas".to_string()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        board
            .create(&CreateTask {
                title: "Task 1 step".to_string(),
                parent_task_id: Some(first.id),
                ..Default::default()
            })
            .unwrap();

        let all = board.list(None, None, 100).unwrap();
        assert_eq!(all.len(), 2);

        let by_agent = board.list(None, Some("atlas"), 100).unwrap();
        assert_eq!(by_agent.len(), 2);

        let counts = board.counts().unwrap();
        assert_eq!(counts.pending, 2);
    }

    #[test]
    fn task_search_matches_all_terms_across_task_conversations() {
        let (db, board) = setup();
        let described = board
            .create(&CreateTask {
                title: "Review connection handling".to_string(),
                description: Some("Preserve the mobile draft during reconnects".to_string()),
                ..Default::default()
            })
            .unwrap();
        let discussed = board
            .create(&CreateTask {
                title: "Investigate the integration".to_string(),
                ..Default::default()
            })
            .unwrap();
        crate::tasks::conversation::TaskConversation::new(db.clone())
            .add_message(
                &discussed.id,
                "user",
                "The JIRA connector cannot authenticate",
            )
            .unwrap();
        let sessions = crate::sessions::SessionManager::new(db.clone());
        sessions
            .ensure("search-session", Some("Search test"))
            .unwrap();
        sessions
            .append_event(
                "search-session",
                crate::sessions::NewEvent {
                    attempt_id: None,
                    task_id: Some(&discussed.id),
                    source_type: "acp",
                    source_id: Some("codex"),
                    event_type: "runner_progress",
                    summary: "Checking the OAuth callback",
                    payload: serde_json::json!({}),
                },
            )
            .unwrap();

        let parent = board
            .create(&CreateTask {
                title: "Parent".to_string(),
                ..Default::default()
            })
            .unwrap();
        board
            .create(&CreateTask {
                title: "JIRA callback subtask".to_string(),
                parent_task_id: Some(parent.id),
                ..Default::default()
            })
            .unwrap();
        board
            .create_idle_task("developer", "JIRA callback maintenance")
            .unwrap();

        let description_matches = board
            .list_page(&[], None, Some("MOBILE draft"), 100, 0)
            .unwrap();
        assert_eq!(description_matches.len(), 1);
        assert_eq!(description_matches[0].id, described.id);

        let conversation_matches = board
            .list_recent_page(&[], None, &[], Some("jira CALLBACK"), 100, 0)
            .unwrap();
        assert_eq!(conversation_matches.len(), 1);
        assert_eq!(conversation_matches[0].id, discussed.id);

        let counts = board.counts_for_search(Some("jira callback")).unwrap();
        assert_eq!(counts.pending, 1);
    }

    #[test]
    fn task_search_normalizes_unicode_and_keeps_every_term() {
        let (_, board) = setup();
        let international = board
            .create(&CreateTask {
                title: "CAFÉ と ﾌﾟﾛｼﾞｪｸﾄ".to_string(),
                description: Some("Straße か\u{3099}く".to_string()),
                ..Default::default()
            })
            .unwrap();

        let unicode_matches = board
            .list_page(&[], None, Some("café プロジェクト STRASSE がく"), 100, 0)
            .unwrap();
        assert_eq!(unicode_matches.len(), 1);
        assert_eq!(unicode_matches[0].id, international.id);

        let terms = (1..=21).map(|index| format!("語{index}"));
        let all_terms = terms.clone().collect::<Vec<_>>().join(" ");
        let complete = board
            .create(&CreateTask {
                title: all_terms.clone(),
                ..Default::default()
            })
            .unwrap();
        board
            .create(&CreateTask {
                title: terms.take(20).collect::<Vec<_>>().join(" "),
                ..Default::default()
            })
            .unwrap();

        let every_term_matches = board
            .list_page(&[], None, Some(&all_terms), 100, 0)
            .unwrap();
        assert_eq!(every_term_matches.len(), 1);
        assert_eq!(every_term_matches[0].id, complete.id);
    }

    #[test]
    fn test_delete_task() {
        let (_, board) = setup();
        let task = board
            .create(&CreateTask {
                title: "To delete".to_string(),
                description: None,
                agent_id: None,
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        board.delete(&task.id).unwrap();
        assert!(board.get(&task.id).is_err());
    }

    #[test]
    fn test_dependencies() {
        let (_, board) = setup();
        let a = board
            .create(&CreateTask {
                title: "Build".into(),
                ..Default::default()
            })
            .unwrap();
        let b = board
            .create(&CreateTask {
                title: "Test".into(),
                ..Default::default()
            })
            .unwrap();

        // B depends on A
        board.add_dependency(&b.id, &a.id).unwrap();
        assert!(!board.is_ready(&b.id).unwrap()); // A not completed
        assert!(board.is_ready(&a.id).unwrap()); // A has no deps

        // Complete A → B becomes ready
        board.update_status(&a.id, "completed", None).unwrap();
        assert!(board.is_ready(&b.id).unwrap());

        // Check getters
        assert_eq!(board.get_dependencies(&b.id).unwrap(), vec![a.id.clone()]);
        assert_eq!(board.get_dependents(&a.id).unwrap(), vec![b.id.clone()]);
    }

    #[test]
    fn test_cycle_detection() {
        let (_, board) = setup();
        let a = board
            .create(&CreateTask {
                title: "A".into(),
                ..Default::default()
            })
            .unwrap();
        let b = board
            .create(&CreateTask {
                title: "B".into(),
                ..Default::default()
            })
            .unwrap();

        board.add_dependency(&b.id, &a.id).unwrap(); // B → A ok
        assert!(board.add_dependency(&a.id, &b.id).is_err()); // A → B cycle!
        assert!(board.add_dependency(&a.id, &a.id).is_err()); // self-cycle
    }

    #[test]
    fn test_batch_create() {
        let (_, board) = setup();
        let tasks = board
            .create_batch(
                &[
                    BatchTaskInput {
                        ref_name: Some("build".into()),
                        title: "Build".into(),
                        description: None,
                        agent_id: None,
                        priority: None,
                        new_session: false,
                        depends_on: None,
                    },
                    BatchTaskInput {
                        ref_name: Some("test".into()),
                        title: "Test".into(),
                        description: None,
                        agent_id: None,
                        priority: None,
                        new_session: false,
                        depends_on: Some(vec!["build".into()]),
                    },
                    BatchTaskInput {
                        ref_name: Some("deploy".into()),
                        title: "Deploy".into(),
                        description: None,
                        agent_id: None,
                        priority: None,
                        new_session: true,
                        depends_on: Some(vec!["test".into()]),
                    },
                ],
                None,
            )
            .unwrap();

        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[2].context.as_ref().unwrap()["session_mode"], "new");
        assert!(!board.is_ready(&tasks[2].id).unwrap()); // deploy blocked
        assert!(!board.is_ready(&tasks[1].id).unwrap()); // test blocked
        assert!(board.is_ready(&tasks[0].id).unwrap()); // build ready

        // Complete build → test ready
        board
            .update_status(&tasks[0].id, "completed", None)
            .unwrap();
        assert!(board.is_ready(&tasks[1].id).unwrap());
        assert!(!board.is_ready(&tasks[2].id).unwrap()); // deploy still blocked

        // Complete test → deploy ready
        board
            .update_status(&tasks[1].id, "completed", None)
            .unwrap();
        assert!(board.is_ready(&tasks[2].id).unwrap());
    }
}
