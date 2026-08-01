use std::sync::Arc;

use chrono::Utc;
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
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let priority = req.priority.unwrap_or(0);
        let context_json = req.context.as_ref().map(|c| c.to_string());

        {
            let conn = self.db.conn();
            conn.execute(
                "INSERT INTO tasks (id, title, description, status, priority, agent_id, parent_task_id, sop_id, conversation_id, context, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                ],
            )?;
        }

        self.get(&id)
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
                "INSERT INTO tasks (id, title, description, status, priority, agent_id, task_type, hidden, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'pending', 0, ?4, 'IDLE', 1, ?5, ?6)",
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
        conn.execute(
            "UPDATE tasks SET conversation_id = ?1, updated_at = datetime('now') WHERE id = ?2",
            rusqlite::params![conversation_id, task_id],
        )?;
        Ok(())
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
        let parsed = TaskStatus::parse(status)?;
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        {
            let conn = self.db.conn();

            // Verify task exists
            let exists: bool = conn.query_row(
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
            conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2,
                    completed_at = CASE WHEN ?1 = 'completed' THEN ?2 ELSE NULL END
                 WHERE id = ?3",
                rusqlite::params![status, now, task_id],
            )?;

            // Set agent_id if transitioning to in_progress
            if parsed == TaskStatus::InProgress {
                if let Some(aid) = agent_id {
                    conn.execute(
                        "UPDATE tasks SET agent_id = ?1 WHERE id = ?2",
                        rusqlite::params![aid, task_id],
                    )?;
                }
            }
        }

        self.get(task_id)
    }

    pub fn update(&self, task_id: &str, req: &UpdateTask) -> Result<Task> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        {
            let conn = self.db.conn();

            // Verify task exists
            let exists: bool = conn.query_row(
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
                conn.execute(
                    "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![title, now, task_id],
                )?;
            }

            if let Some(ref desc) = req.description {
                conn.execute(
                    "UPDATE tasks SET description = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![desc, now, task_id],
                )?;
            }

            if let Some(ref agent_id) = req.agent_id {
                conn.execute(
                    "UPDATE tasks SET agent_id = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![agent_id, now, task_id],
                )?;
            }

            if let Some(priority) = req.priority {
                conn.execute(
                    "UPDATE tasks SET priority = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![priority, now, task_id],
                )?;
            }
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
        self.get(parent_task_id)?;
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
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
                    conn.execute(
                        "UPDATE tasks SET title = ?1, status = ?2, updated_at = ?3,
                            completed_at = ?4, context = ?5 WHERE id = ?6",
                        rusqlite::params![
                            item.title,
                            item.status.as_str(),
                            now,
                            completed_at,
                            context,
                            id,
                        ],
                    )?;
                } else {
                    let id = Uuid::new_v4().to_string();
                    conn.execute(
                        "INSERT INTO tasks
                            (id, title, status, priority, parent_task_id, context,
                             created_at, updated_at, completed_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
                        rusqlite::params![
                            id,
                            item.title,
                            item.status.as_str(),
                            -(index as i32),
                            parent_task_id,
                            context,
                            now,
                            completed_at,
                        ],
                    )?;
                }
            }

            for (id, index) in existing {
                if index >= items.len() {
                    conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
                }
            }
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

    /// Complete a task whose steps are done, then roll that completion through
    /// any ready parents. Parents with queued/running work are left active.
    pub fn complete_and_roll_up(&self, task_id: &str, agent_id: Option<&str>) -> Result<Vec<Task>> {
        let mut completed = Vec::new();
        let mut current_id = Some(task_id.to_string());
        let mut first = true;

        while let Some(id) = current_id {
            if !self.subtasks_complete(&id)? {
                break;
            }
            if self.has_open_attempt(&id)? {
                break;
            }
            let task = self.update_status(&id, "completed", if first { agent_id } else { None })?;
            current_id = task.parent_task_id.clone();
            completed.push(task);
            first = false;
        }

        Ok(completed)
    }

    fn has_open_attempt(&self, task_id: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM work_attempts WHERE task_id = ?1
                    AND status IN ('queued', 'preparing', 'running', 'waiting_for_input', 'review')
                )",
                [task_id],
                |row| row.get(0),
            )
            .map_err(Error::from)
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
    fn syncs_native_plan_steps_and_rolls_up_completion() {
        let (_, board) = setup();
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
        assert!(!board.subtasks_complete(&parent.id).unwrap());

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
        assert!(board.subtasks_complete(&parent.id).unwrap());

        let completed = board
            .complete_and_roll_up(&parent.id, Some("developer"))
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(board.get(&parent.id).unwrap().status, TaskStatus::Completed);
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
            .list_page(
                &[],
                None,
                Some("café プロジェクト STRASSE がく"),
                100,
                0,
            )
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
