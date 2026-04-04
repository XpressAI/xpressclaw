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
        self.list_inner(status, agent_id, limit, false)
    }

    /// List tasks including hidden ones (e.g. IDLE tasks).
    pub fn list_all(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Task>> {
        self.list_inner(status, agent_id, limit, true)
    }

    fn list_inner(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        limit: i64,
        include_hidden: bool,
    ) -> Result<Vec<Task>> {
        let conn = self.db.conn();
        let mut sql = "SELECT * FROM tasks WHERE 1=1".to_string();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if !include_hidden {
            sql.push_str(" AND hidden = 0");
        }

        // Subtasks belong inside their parent, not the top-level list.
        sql.push_str(" AND parent_task_id IS NULL");

        if let Some(s) = status {
            sql.push_str(" AND status = ?");
            params.push(Box::new(s.to_string()));
        }
        if let Some(a) = agent_id {
            sql.push_str(" AND agent_id = ?");
            params.push(Box::new(a.to_string()));
        }

        sql.push_str(" ORDER BY priority DESC, created_at ASC LIMIT ?");
        params.push(Box::new(limit));

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

            // Update status
            conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
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

            // Set completed_at if completing
            if parsed == TaskStatus::Completed {
                conn.execute(
                    "UPDATE tasks SET completed_at = ?1 WHERE id = ?2",
                    rusqlite::params![now, task_id],
                )?;
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
        let conn = self.db.conn();
        let mut stmt =
            conn.prepare("SELECT status, COUNT(*) as count FROM tasks GROUP BY status")?;
        let rows = stmt
            .query_map([], |row| {
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
                context: None,
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
    }

    #[test]
    fn test_list_and_counts() {
        let (_, board) = setup();
        board
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

        let all = board.list(None, None, 100).unwrap();
        assert_eq!(all.len(), 2);

        let by_agent = board.list(None, Some("atlas"), 100).unwrap();
        assert_eq!(by_agent.len(), 2);

        let counts = board.counts().unwrap();
        assert_eq!(counts.pending, 2);
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
                        depends_on: None,
                    },
                    BatchTaskInput {
                        ref_name: Some("test".into()),
                        title: "Test".into(),
                        description: None,
                        agent_id: None,
                        priority: None,
                        depends_on: Some(vec!["build".into()]),
                    },
                    BatchTaskInput {
                        ref_name: Some("deploy".into()),
                        title: "Deploy".into(),
                        description: None,
                        agent_id: None,
                        priority: None,
                        depends_on: Some(vec!["test".into()]),
                    },
                ],
                None,
            )
            .unwrap();

        assert_eq!(tasks.len(), 3);
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
