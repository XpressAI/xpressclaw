use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
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

impl ProjectManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
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
        let _ = self.get(project_id)?;
        self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let previous_project_id = transaction
                .query_row(
                    "SELECT project_id FROM agents WHERE id = ?1",
                    [agent_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(|_| Error::AgentNotFound {
                    name: agent_id.to_string(),
                })?;
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
            let (agents, conversations, tasks): (i64, i64, i64) = (
                transaction.query_row(
                    "SELECT COUNT(*) FROM agents WHERE project_id = ?1",
                    [id],
                    |row| row.get(0),
                )?,
                transaction.query_row(
                    "SELECT COUNT(*) FROM conversations WHERE project_id = ?1",
                    [id],
                    |row| row.get(0),
                )?,
                transaction.query_row(
                    "SELECT COUNT(*) FROM tasks WHERE project_id = ?1",
                    [id],
                    |row| row.get(0),
                )?,
            );
            if agents + conversations + tasks > 0 {
                return Err(Error::Project(
                    "move or remove this project's agents, conversations, and tasks first".into(),
                ));
            }
            transaction.execute(
                "DELETE FROM project_memory_notes WHERE project_id = ?1",
                [id],
            )?;
            transaction.execute("DELETE FROM projects WHERE id = ?1", [id])?;
            transaction.commit()?;
            Ok(())
        })
    }
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::project::{CreateProjectMemoryNote, ProjectMemoryStore};

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
    fn deleting_an_empty_project_removes_its_memory_in_the_same_transaction() {
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

        manager.delete(&project.id).unwrap();

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
}
