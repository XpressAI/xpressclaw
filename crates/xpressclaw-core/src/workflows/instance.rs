use std::sync::Arc;

use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};
use crate::projects::ensure_project_accepts_work;
use crate::tasks::conversation::TaskConversation;
use crate::tasks::queue::TaskQueue;

pub(super) const SOURCE_TASK_STEP_ID: &str = "__source_task__";

/// A running (or completed) workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInstance {
    pub id: String,
    pub workflow_id: String,
    pub project_id: Option<String>,
    pub conversation_id: Option<String>,
    /// Ordinary task that triggered this default workflow. Manual, scheduled,
    /// and connector runs leave this unset.
    #[serde(default)]
    pub source_task_id: Option<String>,
    pub status: String, // running, waiting, completed, failed, cancelled
    pub current_flow: String,
    pub current_step_index: i32,
    pub trigger_data: Option<String>, // JSON
    pub variable_store: String,       // JSON
    pub loop_state: Option<String>,   // JSON
    /// Immutable definition snapshot for this run. Older instances created
    /// before schema v31 fall back to the workflow's current definition.
    #[serde(skip_serializing)]
    pub definition_yaml: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error_message: Option<String>,
}

/// A single step execution within a workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecution {
    pub id: String,
    pub instance_id: String,
    pub flow_name: String,
    pub step_id: String,
    pub task_id: Option<String>,
    pub status: String, // pending, running, waiting, resuming, completed, failed, skipped
    pub input_context: Option<String>,
    pub output: Option<String>,
    pub attempt: i32,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Manages workflow instances and step executions in the database.
pub struct InstanceManager {
    db: Arc<Database>,
}

#[derive(Clone, Copy, Default)]
pub(super) struct WorkflowInstanceScope<'a> {
    pub project_id: Option<&'a str>,
    pub conversation_id: Option<&'a str>,
    pub creator_agent_id: Option<&'a str>,
    pub workflow_agent_bindings: &'a [(String, String)],
    pub source_task_id: Option<&'a str>,
}

impl InstanceManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Create a new workflow instance.
    pub fn create_instance(
        &self,
        workflow_id: &str,
        trigger_data: Option<&str>,
        variables_json: Option<&str>,
    ) -> Result<WorkflowInstance> {
        self.create_instance_with_definition(workflow_id, trigger_data, variables_json, None)
    }

    /// Create a workflow run pinned to an immutable definition snapshot.
    pub fn create_instance_with_definition(
        &self,
        workflow_id: &str,
        trigger_data: Option<&str>,
        variables_json: Option<&str>,
        definition_yaml: Option<&str>,
    ) -> Result<WorkflowInstance> {
        self.create_instance_with_definition_in_context(
            workflow_id,
            trigger_data,
            variables_json,
            definition_yaml,
            WorkflowInstanceScope::default(),
        )
    }

    pub(super) fn create_instance_with_definition_in_context(
        &self,
        workflow_id: &str,
        trigger_data: Option<&str>,
        variables_json: Option<&str>,
        definition_yaml: Option<&str>,
        scope: WorkflowInstanceScope<'_>,
    ) -> Result<WorkflowInstance> {
        self.create_instance_with_definition_in_context_and_then(
            workflow_id,
            trigger_data,
            variables_json,
            definition_yaml,
            scope,
            |_, _| Ok(()),
        )
        .map(|(instance, ())| instance)
    }

    pub(super) fn create_instance_with_definition_in_context_and_then<T, F>(
        &self,
        workflow_id: &str,
        trigger_data: Option<&str>,
        variables_json: Option<&str>,
        definition_yaml: Option<&str>,
        scope: WorkflowInstanceScope<'_>,
        after_insert: F,
    ) -> Result<(WorkflowInstance, T)>
    where
        F: FnOnce(&rusqlite::Transaction<'_>, &str) -> Result<T>,
    {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let var_store = variables_json.unwrap_or("{}");

        let after_insert = self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let conversation_project = if let Some(conversation_id) = scope.conversation_id {
                transaction
                    .query_row(
                        "SELECT project_id FROM conversations WHERE id = ?1",
                        [conversation_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        Error::Workflow(format!("conversation '{conversation_id}' not found"))
                    })?
            } else {
                None
            };
            if let (Some(requested), Some(actual)) =
                (scope.project_id, conversation_project.as_deref())
            {
                if requested != actual {
                    return Err(Error::Workflow(format!(
                        "conversation '{}' belongs to project '{actual}', not '{requested}'",
                        scope.conversation_id.unwrap_or_default()
                    )));
                }
            }
            let resolved_project = scope.project_id.or(conversation_project.as_deref());
            if let Some(project_id) = resolved_project {
                ensure_project_accepts_work(&transaction, project_id).map_err(|error| {
                    Error::Workflow(error.to_string())
                })?;
            }
            if let Some(creator_agent_id) = scope.creator_agent_id {
                let conversation_id = scope.conversation_id.ok_or_else(|| {
                    Error::Conversation(
                        "an Agent may only create work from a conversation".into(),
                    )
                })?;
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
            let mut agent_bindings = Vec::new();
            for (source, agent_id) in scope.workflow_agent_bindings {
                let agent_project = transaction
                    .query_row(
                        "SELECT project_id FROM agents WHERE id = ?1",
                        [agent_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        Error::Workflow(format!(
                            "workflow {source} references unknown agent '{agent_id}'"
                        ))
                    })?;
                if let Some(project_id) = resolved_project {
                    if agent_project.as_deref() != Some(project_id) {
                        return Err(Error::Workflow(format!(
                            "workflow {source} references Agent '{agent_id}' outside project '{project_id}'"
                        )));
                    }
                } else if let Some(project_id) = agent_project.as_deref() {
                    // A projectless workflow still creates work through its
                    // selected Agents. Validate each Agent's owning Project
                    // before inserting the instance so a deletion marker
                    // cannot leave an undiscoverable pre-step run behind.
                    ensure_project_accepts_work(&transaction, project_id)
                        .map_err(|error| Error::Workflow(error.to_string()))?;
                }
                if let Some(project_id) = agent_project {
                    agent_bindings.push((agent_id.as_str(), project_id));
                }
            }
            transaction.execute(
                "INSERT INTO workflow_instances
                 (id, workflow_id, project_id, conversation_id, source_task_id, status,
                  current_flow, current_step_index, trigger_data, variable_store,
                  started_at, definition_yaml)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'running', 'main', ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    id,
                    workflow_id,
                    resolved_project,
                    scope.conversation_id,
                    scope.source_task_id,
                    if scope.source_task_id.is_some() { -1 } else { 0 },
                    trigger_data,
                    var_store,
                    now,
                    definition_yaml
                ],
            )?;
            for (agent_id, project_id) in agent_bindings {
                insert_instance_agent_binding(&transaction, &id, agent_id, &project_id)?;
            }
            let after_insert = after_insert(&transaction, &id)?;
            transaction.commit()?;
            Ok::<_, Error>(after_insert)
        })?;

        Ok((self.get_instance(&id)?, after_insert))
    }

    pub fn set_context(
        &self,
        instance_id: &str,
        project_id: Option<&str>,
        conversation_id: Option<&str>,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            // Validate the instance's current ownership before allowing its
            // context to move. Otherwise a concurrent cascade could mark the
            // source Project for deletion and this update could detach the
            // instance before the cascade's final ownership sweep sees it.
            ensure_instance_accepts_work(&transaction, instance_id)?;
            let conversation_project = if let Some(conversation_id) = conversation_id {
                transaction
                    .query_row(
                        "SELECT project_id FROM conversations WHERE id = ?1",
                        [conversation_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        Error::Workflow(format!("conversation '{conversation_id}' not found"))
                    })?
            } else {
                None
            };
            if let (Some(requested), Some(actual)) = (project_id, conversation_project.as_deref()) {
                if requested != actual {
                    return Err(Error::Workflow(format!(
                        "conversation '{}' belongs to project '{actual}', not '{requested}'",
                        conversation_id.unwrap_or_default()
                    )));
                }
            }
            let resolved_project = project_id.or(conversation_project.as_deref());
            if let Some(project_id) = resolved_project {
                ensure_project_accepts_work(&transaction, project_id)
                    .map_err(|error| Error::Workflow(error.to_string()))?;
                ensure_instance_project_matches_durable_work(
                    &transaction,
                    instance_id,
                    project_id,
                )?;
            }
            let updated = transaction.execute(
                "UPDATE workflow_instances SET project_id = ?1, conversation_id = ?2 WHERE id = ?3",
                rusqlite::params![project_id, conversation_id, instance_id],
            )?;
            if updated == 0 {
                return Err(Error::WorkflowInstanceNotFound {
                    id: instance_id.to_string(),
                });
            }
            transaction.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    /// Get a workflow instance by ID.
    pub fn get_instance(&self, id: &str) -> Result<WorkflowInstance> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflow_instances WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_instance(row)))
                .map_err(|_| Error::WorkflowInstanceNotFound { id: id.to_string() })
        })
    }

    /// Find the durable default-workflow attachment for one source task.
    pub fn find_source_instance(
        &self,
        workflow_id: &str,
        source_task_id: &str,
    ) -> Result<Option<WorkflowInstance>> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT * FROM workflow_instances
                 WHERE workflow_id = ?1 AND source_task_id = ?2",
                rusqlite::params![workflow_id, source_task_id],
                |row| Ok(row_to_instance(row)),
            )
            .optional()
            .map_err(Error::from)
        })
    }

    /// List instances for a given workflow, ordered by most recent first.
    pub fn list_instances(&self, workflow_id: &str, limit: i64) -> Result<Vec<WorkflowInstance>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM workflow_instances WHERE workflow_id = ?1 ORDER BY started_at DESC LIMIT ?2",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map(rusqlite::params![workflow_id, limit], |row| {
                    Ok(row_to_instance(row))
                })
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// List all active workflow instances, including durable event waits.
    pub fn list_running_instances(&self) -> Result<Vec<WorkflowInstance>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflow_instances WHERE status IN ('running', 'waiting') ORDER BY started_at ASC")
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map([], |row| Ok(row_to_instance(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// Change between non-terminal runtime states without recording a
    /// completion timestamp.
    pub fn set_active_status(&self, id: &str, status: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_instance_accepts_work(&transaction, id)?;
            transaction.execute(
                "UPDATE workflow_instances SET status = ?1, completed_at = NULL, error_message = NULL WHERE id = ?2",
                rusqlite::params![status, id],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;
        Ok(())
    }

    /// Update the status of a workflow instance.
    pub fn update_status(&self, id: &str, status: &str, error_msg: Option<&str>) -> Result<()> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            if status != "cancelled" {
                ensure_instance_accepts_work(&transaction, id)?;
            }
            transaction.execute(
                "UPDATE workflow_instances SET status = ?1, error_message = ?2, completed_at = ?3 WHERE id = ?4",
                rusqlite::params![status, error_msg, now, id],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;

        Ok(())
    }

    /// Mark a workflow instance as completed.
    pub fn complete_instance(&self, id: &str) -> Result<()> {
        self.update_status(id, "completed", None)
    }

    /// Set the current position (flow + step index) of a workflow instance.
    pub fn set_current_position(
        &self,
        instance_id: &str,
        flow: &str,
        step_index: i32,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET current_flow = ?1, current_step_index = ?2 WHERE id = ?3",
                rusqlite::params![flow, step_index, instance_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Get the variable store JSON for an instance.
    pub fn get_variable_store(&self, instance_id: &str) -> Result<String> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT variable_store FROM workflow_instances WHERE id = ?1",
                [instance_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| Error::WorkflowInstanceNotFound {
                id: instance_id.to_string(),
            })
        })
    }

    /// Update the variable store JSON for an instance.
    pub fn update_variable_store(&self, instance_id: &str, store_json: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET variable_store = ?1 WHERE id = ?2",
                rusqlite::params![store_json, instance_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Update the loop state JSON for an instance.
    pub fn update_loop_state(&self, instance_id: &str, loop_state: Option<&str>) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET loop_state = ?1 WHERE id = ?2",
                rusqlite::params![loop_state, instance_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    // -- Step Execution methods --

    /// Create a new step execution record.
    pub fn create_step_execution(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        input_context: Option<&str>,
    ) -> Result<StepExecution> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_instance_accepts_work(&transaction, instance_id)?;
            let attempt = transaction.query_row(
                "SELECT COUNT(*) FROM workflow_step_executions WHERE instance_id = ?1 AND step_id = ?2",
                rusqlite::params![instance_id, step_id],
                |row| row.get::<_, i32>(0),
            )? + 1;
            transaction.execute(
                "INSERT INTO workflow_step_executions (id, instance_id, flow_name, step_id, status, input_context, attempt, started_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7)",
                rusqlite::params![id, instance_id, flow_name, step_id, input_context, attempt, now],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;

        self.get_step_execution(&id)
    }

    /// Create a running task-backed execution before its task is made
    /// dispatchable. Recovery can safely enqueue the linked task if the
    /// process exits before dispatch.
    pub fn create_task_execution(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        input_context: Option<&str>,
        task_id: &str,
    ) -> Result<StepExecution> {
        let id = Uuid::new_v4().to_string();
        self.insert_task_execution(
            &id,
            instance_id,
            flow_name,
            step_id,
            input_context,
            task_id,
            None,
        )?;
        self.get_step_execution(&id)
    }

    /// Insert a running task-backed execution inside a caller-owned
    /// transaction. Default workflow attachment uses this so the instance and
    /// its source-task completion gate become durable together.
    pub(super) fn create_task_execution_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        input_context: Option<&str>,
        task_id: &str,
    ) -> Result<String> {
        ensure_instance_accepts_work(transaction, instance_id)?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let attempt = transaction.query_row(
            "SELECT COUNT(*) FROM workflow_step_executions
             WHERE instance_id = ?1 AND step_id = ?2",
            rusqlite::params![instance_id, step_id],
            |row| row.get::<_, i32>(0),
        )? + 1;
        transaction.execute(
            "INSERT INTO workflow_step_executions
             (id, instance_id, flow_name, step_id, task_id, status,
              input_context, attempt, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8)",
            rusqlite::params![
                id,
                instance_id,
                flow_name,
                step_id,
                task_id,
                input_context,
                attempt,
                now
            ],
        )?;
        Ok(id)
    }

    /// Atomically append one fixed user prompt to an instance's source task,
    /// link the workflow execution to that task, and queue its next response
    /// cycle. An execution can therefore never exist without its prompt, and a
    /// retry cannot send the same prompt twice.
    pub fn create_continuation_execution(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        input_context: Option<&str>,
        task_id: &str,
        prompt: &str,
    ) -> Result<StepExecution> {
        let execution_id = self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_instance_accepts_work(&transaction, instance_id)?;
            if let Some(existing) = transaction
                .query_row(
                    "SELECT id FROM workflow_step_executions
                     WHERE instance_id = ?1 AND flow_name = ?2 AND step_id = ?3
                       AND status = 'running'
                     ORDER BY rowid DESC LIMIT 1",
                    rusqlite::params![instance_id, flow_name, step_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                transaction.commit()?;
                return Ok::<_, Error>(existing);
            }

            let agent_id = transaction
                .query_row(
                    "SELECT agent_id FROM tasks WHERE id = ?1",
                    [task_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| Error::TaskNotFound {
                    id: task_id.to_string(),
                })?
                .filter(|agent_id| !agent_id.trim().is_empty())
                .ok_or_else(|| {
                    Error::Workflow(format!(
                        "continue step '{step_id}' cannot resume unassigned task '{task_id}'"
                    ))
                })?;
            let message = TaskConversation::insert_text_message_in_transaction(
                &transaction,
                task_id,
                "user",
                prompt,
            )?;
            let execution_id = Self::create_task_execution_in_transaction(
                &transaction,
                instance_id,
                flow_name,
                step_id,
                input_context,
                task_id,
            )?;
            TaskQueue::enqueue_continuation_for_message_in_transaction(
                &transaction,
                task_id,
                &agent_id,
                message.id,
                &message.timestamp,
            )?;
            transaction.execute(
                "UPDATE tasks
                 SET status = 'pending', completed_at = NULL,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                [task_id],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(execution_id)
        })?;
        self.get_step_execution(&execution_id)
    }

    /// Atomically link a loop body task to its execution and persist that
    /// execution as the loop cursor's owner. The caller may only enqueue the
    /// task after this transaction commits.
    pub fn create_loop_task_execution<F>(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        input_context: Option<&str>,
        task_id: &str,
        loop_state_for_execution: F,
    ) -> Result<StepExecution>
    where
        F: FnOnce(&str) -> Result<String>,
    {
        let id = Uuid::new_v4().to_string();
        let loop_state = loop_state_for_execution(&id)?;
        self.insert_task_execution(
            &id,
            instance_id,
            flow_name,
            step_id,
            input_context,
            task_id,
            Some(&loop_state),
        )?;
        self.get_step_execution(&id)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_task_execution(
        &self,
        id: &str,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        input_context: Option<&str>,
        task_id: &str,
        loop_state: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| -> Result<()> {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )
            .map_err(|error| Error::Database(error.to_string()))?;
            ensure_instance_accepts_work(&transaction, instance_id)?;
            let attempt = transaction
                .query_row(
                    "SELECT COUNT(*) FROM workflow_step_executions WHERE instance_id = ?1 AND step_id = ?2",
                    rusqlite::params![instance_id, step_id],
                    |row| row.get::<_, i32>(0),
                )
                .map_err(|error| Error::Database(error.to_string()))?
                + 1;
            transaction
                .execute(
                    "INSERT INTO workflow_step_executions
                     (id, instance_id, flow_name, step_id, task_id, status, input_context, attempt, started_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8)",
                    rusqlite::params![
                        id,
                        instance_id,
                        flow_name,
                        step_id,
                        task_id,
                        input_context,
                        attempt,
                        now
                    ],
                )
                .map_err(|error| Error::Database(error.to_string()))?;
            if let Some(loop_state) = loop_state {
                let changed = transaction
                    .execute(
                        "UPDATE workflow_instances SET loop_state = ?1 WHERE id = ?2",
                        rusqlite::params![loop_state, instance_id],
                    )
                    .map_err(|error| Error::Database(error.to_string()))?;
                if changed != 1 {
                    return Err(Error::WorkflowInstanceNotFound {
                        id: instance_id.to_string(),
                    });
                }
            }
            transaction
                .commit()
                .map_err(|error| Error::Database(error.to_string()))?;
            Ok(())
        })
    }

    /// Atomically persist a durable wait and put its instance to sleep. This
    /// avoids a crash window where a pending execution exists but neither the
    /// worker recovery path nor the wait poller owns it.
    pub fn create_wait_execution(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        wait_state: &str,
    ) -> Result<StepExecution> {
        self.create_wait_execution_inner(instance_id, flow_name, step_id, None, wait_state)
    }

    /// Persist a taskless event wait together with the Project scope implied
    /// by its selected Agent. The binding and wait are committed atomically so
    /// Project deletion cannot miss the instance between those operations.
    pub(super) fn create_agent_wait_execution(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        agent_id: &str,
        wait_state: &str,
    ) -> Result<StepExecution> {
        self.create_wait_execution_inner(
            instance_id,
            flow_name,
            step_id,
            Some(agent_id),
            wait_state,
        )
    }

    fn create_wait_execution_inner(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        agent_id: Option<&str>,
        wait_state: &str,
    ) -> Result<StepExecution> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| -> Result<()> {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )
            .map_err(|error| Error::Database(error.to_string()))?;
            ensure_instance_accepts_work(&transaction, instance_id)?;
            if let Some(agent_id) = agent_id {
                bind_instance_to_agent_project(&transaction, instance_id, agent_id)?;
            }
            let attempt = transaction
                .query_row(
                    "SELECT COUNT(*) FROM workflow_step_executions WHERE instance_id = ?1 AND step_id = ?2",
                    rusqlite::params![instance_id, step_id],
                    |row| row.get::<_, i32>(0),
                )
                .map_err(|error| Error::Database(error.to_string()))?
                + 1;
            transaction
                .execute(
                    "INSERT INTO workflow_step_executions (id, instance_id, flow_name, step_id, status, input_context, attempt, started_at)
                     VALUES (?1, ?2, ?3, ?4, 'waiting', ?5, ?6, ?7)",
                    rusqlite::params![id, instance_id, flow_name, step_id, wait_state, attempt, now],
                )
                .map_err(|error| Error::Database(error.to_string()))?;
            transaction
                .execute(
                    "UPDATE workflow_instances SET status = 'waiting', completed_at = NULL, error_message = NULL WHERE id = ?1",
                    [instance_id],
                )
                .map_err(|error| Error::Database(error.to_string()))?;
            transaction
                .commit()
                .map_err(|error| Error::Database(error.to_string()))?;
            Ok(())
        })?;

        self.get_step_execution(&id)
    }

    /// Get a step execution by ID.
    pub fn get_step_execution(&self, id: &str) -> Result<StepExecution> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflow_step_executions WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_step_execution(row)))
                .map_err(|_| Error::Workflow(format!("step execution not found: {id}")))
        })
    }

    /// Find a step execution linked to a task, regardless of its current
    /// status. Recovery callers use completed executions at crash boundaries;
    /// live completion uses `find_running_executions_by_task` below.
    pub fn find_execution_by_task(&self, task_id: &str) -> Result<Option<StepExecution>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM workflow_step_executions
                     WHERE task_id = ?1
                     ORDER BY rowid DESC LIMIT 1",
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            stmt.query_row([task_id], |row| Ok(row_to_step_execution(row)))
                .optional()
                .map_err(Error::from)
        })
    }

    /// Find every active workflow execution waiting on the same task. A task
    /// may be the source for more than one default workflow.
    pub fn find_running_executions_by_task(&self, task_id: &str) -> Result<Vec<StepExecution>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT execution.*
                     FROM workflow_step_executions execution
                     JOIN workflow_instances instance ON instance.id = execution.instance_id
                     WHERE execution.task_id = ?1
                       AND execution.status = 'running'
                       AND instance.status = 'running'
                     ORDER BY CASE WHEN execution.step_id = ?2 THEN 1 ELSE 0 END,
                              execution.started_at ASC, execution.rowid ASC",
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            let records = stmt
                .query_map([task_id, SOURCE_TASK_STEP_ID], |row| {
                    Ok(row_to_step_execution(row))
                })
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|record| record.ok())
                .collect();
            Ok(records)
        })
    }

    /// Find a running continuation that reuses an instance's source task
    /// inside a caller-owned transaction. The synthetic source gate also
    /// points at that task, so exclude it from the result. Ordinary workflow
    /// steps always own a newly created task.
    pub(super) fn find_running_source_continuation_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        instance_id: &str,
    ) -> Result<Option<StepExecution>> {
        transaction
            .query_row(
                "SELECT execution.*
                 FROM workflow_step_executions execution
                 JOIN workflow_instances instance ON instance.id = execution.instance_id
                 WHERE execution.instance_id = ?1
                   AND instance.status IN ('running', 'waiting')
                   AND execution.status = 'running'
                   AND execution.task_id = instance.source_task_id
                   AND execution.step_id != ?2
                 ORDER BY execution.started_at DESC, execution.rowid DESC
                 LIMIT 1",
                rusqlite::params![instance_id, SOURCE_TASK_STEP_ID],
                |row| Ok(row_to_step_execution(row)),
            )
            .optional()
            .map_err(Error::from)
    }

    pub(super) fn cancel_instance_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        instance_id: &str,
    ) -> Result<()> {
        let changed = transaction.execute(
            "UPDATE workflow_instances
             SET status = 'cancelled', error_message = NULL,
                 completed_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            [instance_id],
        )?;
        if changed == 0 {
            return Err(Error::WorkflowInstanceNotFound {
                id: instance_id.to_string(),
            });
        }
        Ok(())
    }

    /// Update the status and output of a step execution.
    pub fn update_step_status(&self, id: &str, status: &str, output: Option<&str>) -> Result<()> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_step_executions SET status = ?1, output = ?2, completed_at = ?3 WHERE id = ?4",
                rusqlite::params![status, output, now, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Atomically claim a wait for resumption and persist the event before
    /// advancing the workflow. A `resuming` execution is replayed by recovery
    /// after a crash, while competing pollers can no longer claim it twice.
    pub fn claim_wait(&self, id: &str, output: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_step_execution_accepts_work(&transaction, id)?;
            let changed = transaction.execute(
                "UPDATE workflow_step_executions SET status = 'resuming', output = ?1
                 WHERE id = ?2 AND status = 'waiting'",
                rusqlite::params![output, id],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(changed == 1)
        })
    }

    /// Persist wait-provider cursor, health, and adaptive polling state while
    /// leaving a concurrently claimed execution untouched.
    pub fn update_wait_state(&self, id: &str, wait_state: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_step_executions SET input_context = ?1
                 WHERE id = ?2 AND status = 'waiting'",
                rusqlite::params![wait_state, id],
            )
            .map_err(|error| Error::Database(error.to_string()))
        })?;
        Ok(())
    }

    /// Link a step execution to a task.
    pub fn set_step_task(&self, execution_id: &str, task_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_step_execution_accepts_work(&transaction, execution_id)?;
            transaction.execute(
                "UPDATE workflow_step_executions SET task_id = ?1, status = 'running' WHERE id = ?2",
                rusqlite::params![task_id, execution_id],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;

        Ok(())
    }

    pub fn mark_step_running(&self, execution_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_step_execution_accepts_work(&transaction, execution_id)?;
            transaction.execute(
                "UPDATE workflow_step_executions SET status = 'running' WHERE id = ?1",
                [execution_id],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;
        Ok(())
    }

    /// List all step executions for a workflow instance.
    pub fn list_step_executions(&self, instance_id: &str) -> Result<Vec<StepExecution>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM workflow_step_executions WHERE instance_id = ?1 ORDER BY started_at ASC, rowid ASC",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map([instance_id], |row| Ok(row_to_step_execution(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// List durable waits across all running workflow instances.
    pub fn list_waiting_step_executions(&self) -> Result<Vec<StepExecution>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT e.* FROM workflow_step_executions e
                     JOIN workflow_instances i ON i.id = e.instance_id
                     WHERE e.status = 'waiting' AND i.status IN ('running', 'waiting')
                     ORDER BY e.started_at ASC",
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            let records = stmt
                .query_map([], |row| Ok(row_to_step_execution(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|record| record.ok())
                .collect();
            Ok(records)
        })
    }

    /// Get the number of times a step has been executed within an instance.
    pub fn get_step_attempt_count(&self, instance_id: &str, step_id: &str) -> Result<i32> {
        self.db.with_conn(|conn| {
            let count: i32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM workflow_step_executions WHERE instance_id = ?1 AND step_id = ?2",
                    rusqlite::params![instance_id, step_id],
                    |row| row.get(0),
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            Ok(count)
        })
    }
}

fn ensure_instance_accepts_work(conn: &rusqlite::Connection, instance_id: &str) -> Result<()> {
    let (project_id, conversation_project_id, status) = conn
        .query_row(
            "SELECT instance.project_id, conversation.project_id, instance.status
             FROM workflow_instances instance
             LEFT JOIN conversations conversation
               ON conversation.id = instance.conversation_id
             WHERE instance.id = ?1",
            [instance_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::WorkflowInstanceNotFound {
            id: instance_id.to_string(),
        })?;
    if let Some(project_id) = project_id.as_deref() {
        ensure_project_accepts_work(conn, project_id)
            .map_err(|error| Error::Workflow(error.to_string()))?;
    }
    if conversation_project_id.as_deref() != project_id.as_deref() {
        if let Some(project_id) = conversation_project_id.as_deref() {
            ensure_project_accepts_work(conn, project_id)
                .map_err(|error| Error::Workflow(error.to_string()))?;
        }
    }
    let derived_project_ids = instance_durable_project_ids(conn, instance_id)?;
    for derived_project_id in derived_project_ids {
        if Some(derived_project_id.as_str()) != project_id.as_deref()
            && Some(derived_project_id.as_str()) != conversation_project_id.as_deref()
        {
            ensure_project_accepts_work(conn, &derived_project_id)
                .map_err(|error| Error::Workflow(error.to_string()))?;
        }
    }
    if !matches!(status.as_str(), "running" | "waiting") {
        return Err(Error::Workflow(format!(
            "workflow instance '{instance_id}' no longer accepts work ({status})"
        )));
    }
    Ok(())
}

fn ensure_instance_project_matches_durable_work(
    conn: &rusqlite::Connection,
    instance_id: &str,
    project_id: &str,
) -> Result<()> {
    let conflicting_projects = instance_durable_project_ids(conn, instance_id)?
        .into_iter()
        .filter(|derived_project_id| derived_project_id != project_id)
        .collect::<Vec<_>>();
    if conflicting_projects.is_empty() {
        return Ok(());
    }

    Err(Error::Workflow(format!(
        "workflow instance '{instance_id}' has durable Agent or task work bound to project(s) {} and cannot move to project '{project_id}'",
        conflicting_projects.join(", ")
    )))
}

fn instance_durable_project_ids(
    conn: &rusqlite::Connection,
    instance_id: &str,
) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT project_id
         FROM (
             SELECT binding.project_id AS project_id
             FROM workflow_instance_agent_bindings binding
             WHERE binding.instance_id = ?1
             UNION
             SELECT task.project_id AS project_id
             FROM workflow_step_executions execution
             JOIN tasks task ON task.id = execution.task_id
             WHERE execution.instance_id = ?1 AND task.project_id IS NOT NULL
         )
         ORDER BY project_id",
    )?;
    let project_ids = statement
        .query_map([instance_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(project_ids)
}

fn bind_instance_to_agent_project(
    conn: &rusqlite::Connection,
    instance_id: &str,
    agent_id: &str,
) -> Result<()> {
    let Some(project_id) = conn
        .query_row(
            "SELECT project_id FROM agents WHERE id = ?1",
            [agent_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
    else {
        // Preserve the existing workflow error/recovery path for stale or
        // externally supplied Agent names. Only registered Project Agents add
        // a lifecycle binding.
        return Ok(());
    };
    if let Some(project_id) = project_id {
        ensure_project_accepts_work(conn, &project_id)
            .map_err(|error| Error::Workflow(error.to_string()))?;
        insert_instance_agent_binding(conn, instance_id, agent_id, &project_id)?;
    }
    Ok(())
}

fn insert_instance_agent_binding(
    conn: &rusqlite::Connection,
    instance_id: &str,
    agent_id: &str,
    project_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO workflow_instance_agent_bindings
         (instance_id, agent_id, project_id) VALUES (?1, ?2, ?3)",
        rusqlite::params![instance_id, agent_id, project_id],
    )?;
    Ok(())
}

fn ensure_step_execution_accepts_work(
    conn: &rusqlite::Connection,
    execution_id: &str,
) -> Result<()> {
    let instance_id = conn
        .query_row(
            "SELECT instance_id FROM workflow_step_executions WHERE id = ?1",
            [execution_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| Error::Workflow(format!("step execution not found: {execution_id}")))?;
    ensure_instance_accepts_work(conn, &instance_id)
}

fn row_to_instance(row: &rusqlite::Row) -> WorkflowInstance {
    WorkflowInstance {
        id: row.get("id").unwrap_or_default(),
        workflow_id: row.get("workflow_id").unwrap_or_default(),
        project_id: row.get("project_id").unwrap_or_default(),
        conversation_id: row.get("conversation_id").unwrap_or_default(),
        source_task_id: row.get("source_task_id").unwrap_or_default(),
        status: row.get("status").unwrap_or_default(),
        current_flow: row
            .get("current_flow")
            .unwrap_or_else(|_| "main".to_string()),
        current_step_index: row.get("current_step_index").unwrap_or(0),
        trigger_data: row.get("trigger_data").unwrap_or_default(),
        variable_store: row
            .get("variable_store")
            .unwrap_or_else(|_| "{}".to_string()),
        loop_state: row.get("loop_state").unwrap_or_default(),
        definition_yaml: row.get("definition_yaml").unwrap_or_default(),
        started_at: row.get("started_at").unwrap_or_default(),
        completed_at: row.get("completed_at").unwrap_or_default(),
        error_message: row.get("error_message").unwrap_or_default(),
    }
}

fn row_to_step_execution(row: &rusqlite::Row) -> StepExecution {
    StepExecution {
        id: row.get("id").unwrap_or_default(),
        instance_id: row.get("instance_id").unwrap_or_default(),
        flow_name: row.get("flow_name").unwrap_or_default(),
        step_id: row.get("step_id").unwrap_or_default(),
        task_id: row.get("task_id").unwrap_or_default(),
        status: row.get("status").unwrap_or_default(),
        input_context: row.get("input_context").unwrap_or_default(),
        output: row.get("output").unwrap_or_default(),
        attempt: row.get("attempt").unwrap_or(1),
        started_at: row.get("started_at").unwrap_or_default(),
        completed_at: row.get("completed_at").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, InstanceManager) {
        let db = Arc::new(Database::open_memory().unwrap());

        // Insert a dummy workflow for foreign key
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflows (id, name, yaml_content, version) VALUES ('wf1', 'test', 'yaml', 2)",
                [],
            )
            .unwrap();
        });

        let mgr = InstanceManager::new(db.clone());
        (db, mgr)
    }

    #[test]
    fn test_create_and_get_instance() {
        let (_, mgr) = setup();
        let inst = mgr
            .create_instance("wf1", Some(r#"{"key": "value"}"#), None)
            .unwrap();

        assert_eq!(inst.workflow_id, "wf1");
        assert_eq!(inst.status, "running");
        assert_eq!(inst.current_flow, "main");
        assert_eq!(inst.current_step_index, 0);
        assert_eq!(inst.trigger_data.as_deref(), Some(r#"{"key": "value"}"#));
        assert_eq!(inst.variable_store, "{}");

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.id, inst.id);
    }

    #[test]
    fn test_create_instance_with_variables() {
        let (_, mgr) = setup();
        let inst = mgr
            .create_instance("wf1", None, Some(r#"{"default_agent": "atlas"}"#))
            .unwrap();

        assert_eq!(inst.variable_store, r#"{"default_agent": "atlas"}"#);
    }

    #[test]
    fn deleting_project_blocks_new_and_resumed_workflow_work() {
        let (db, mgr) = setup();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One');
                 INSERT INTO agents (id, name, backend, config, status, project_id)
                 VALUES ('project-agent', 'Project Agent', 'codex', '{}', 'stopped', 'project-one');",
            )
        })
        .unwrap();
        let instance = mgr
            .create_instance_with_definition_in_context(
                "wf1",
                None,
                None,
                None,
                WorkflowInstanceScope {
                    project_id: Some("project-one"),
                    ..WorkflowInstanceScope::default()
                },
            )
            .unwrap();
        let waiting = mgr
            .create_wait_execution(&instance.id, "main", "review", "{}")
            .unwrap();
        let agent_inputs = vec![("worker".to_string(), "project-agent".to_string())];
        let projectless_input = mgr
            .create_instance_with_definition_in_context(
                "wf1",
                None,
                None,
                None,
                WorkflowInstanceScope {
                    workflow_agent_bindings: &agent_inputs,
                    ..WorkflowInstanceScope::default()
                },
            )
            .unwrap();
        let projectless = mgr.create_instance("wf1", None, None).unwrap();
        let projectless_wait = mgr
            .create_agent_wait_execution(
                &projectless.id,
                "main",
                "review",
                "project-agent",
                r#"{"agent_id":"project-agent"}"#,
            )
            .unwrap();
        let binding: (String, String) = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT agent_id, project_id
                     FROM workflow_instance_agent_bindings WHERE instance_id = ?1",
                    [&projectless.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .unwrap();
        assert_eq!(binding, ("project-agent".into(), "project-one".into()));
        let input_binding: (String, String) = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT agent_id, project_id
                     FROM workflow_instance_agent_bindings WHERE instance_id = ?1",
                    [&projectless_input.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .unwrap();
        assert_eq!(
            input_binding,
            ("project-agent".into(), "project-one".into())
        );

        crate::projects::ProjectManager::new(db.clone())
            .begin_cascade("project-one")
            .unwrap();

        let create_error = mgr
            .create_instance_with_definition_in_context(
                "wf1",
                None,
                None,
                None,
                WorkflowInstanceScope {
                    project_id: Some("project-one"),
                    ..WorkflowInstanceScope::default()
                },
            )
            .unwrap_err();
        assert!(create_error.to_string().contains("being deleted"));
        let projectless_error = mgr
            .create_instance_with_definition_in_context(
                "wf1",
                None,
                None,
                None,
                WorkflowInstanceScope {
                    workflow_agent_bindings: &agent_inputs,
                    ..WorkflowInstanceScope::default()
                },
            )
            .unwrap_err();
        assert!(projectless_error.to_string().contains("being deleted"));
        let instance_count = db
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM workflow_instances", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .unwrap();
        assert_eq!(instance_count, 3);
        let resume_error = mgr.set_active_status(&instance.id, "running").unwrap_err();
        assert!(resume_error.to_string().contains("being deleted"));
        let claim_error = mgr.claim_wait(&waiting.id, "approved").unwrap_err();
        assert!(claim_error.to_string().contains("being deleted"));
        let projectless_claim_error = mgr
            .claim_wait(&projectless_wait.id, "approved")
            .unwrap_err();
        assert!(projectless_claim_error
            .to_string()
            .contains("being deleted"));
        assert_eq!(mgr.get_instance(&instance.id).unwrap().status, "cancelled");
        assert_eq!(
            mgr.get_instance(&projectless.id).unwrap().status,
            "cancelled"
        );
        assert_eq!(
            mgr.get_instance(&projectless_input.id).unwrap().status,
            "cancelled"
        );
    }

    #[test]
    fn deleting_project_scope_cannot_be_moved_to_another_project() {
        let (db, mgr) = setup();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One');
                 INSERT INTO projects (id, name) VALUES ('project-two', 'Project Two');
                 INSERT INTO tasks (id, title, status, project_id)
                 VALUES ('project-task', 'Project task', 'pending', 'project-one');",
            )
        })
        .unwrap();

        let scoped = mgr
            .create_instance_with_definition_in_context(
                "wf1",
                None,
                None,
                None,
                WorkflowInstanceScope {
                    project_id: Some("project-one"),
                    ..WorkflowInstanceScope::default()
                },
            )
            .unwrap();
        let task_scoped = mgr.create_instance("wf1", None, None).unwrap();
        let execution = mgr
            .create_step_execution(&task_scoped.id, "main", "work", None)
            .unwrap();
        mgr.set_step_task(&execution.id, "project-task").unwrap();

        crate::projects::ProjectManager::new(db)
            .begin_cascade("project-one")
            .unwrap();

        for instance_id in [&scoped.id, &task_scoped.id] {
            let error = mgr
                .set_context(instance_id, Some("project-two"), None)
                .unwrap_err();
            assert!(error.to_string().contains("being deleted"));
        }
        assert_eq!(
            mgr.get_instance(&scoped.id).unwrap().project_id.as_deref(),
            Some("project-one")
        );
        assert_eq!(mgr.get_instance(&task_scoped.id).unwrap().project_id, None);
    }

    #[test]
    fn workflow_context_cannot_cross_durable_project_boundaries() {
        let (db, mgr) = setup();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One');
                 INSERT INTO projects (id, name) VALUES ('project-two', 'Project Two');
                 INSERT INTO agents (id, name, backend, config, status, project_id)
                 VALUES ('project-agent', 'Project Agent', 'codex', '{}', 'stopped', 'project-one');
                 INSERT INTO tasks (id, title, status, project_id)
                 VALUES ('project-task', 'Project task', 'pending', 'project-one');",
            )
        })
        .unwrap();

        let agent_inputs = vec![("worker".to_string(), "project-agent".to_string())];
        let agent_scoped = mgr
            .create_instance_with_definition_in_context(
                "wf1",
                None,
                None,
                None,
                WorkflowInstanceScope {
                    workflow_agent_bindings: &agent_inputs,
                    ..WorkflowInstanceScope::default()
                },
            )
            .unwrap();
        let task_scoped = mgr.create_instance("wf1", None, None).unwrap();
        let execution = mgr
            .create_step_execution(&task_scoped.id, "main", "work", None)
            .unwrap();
        mgr.set_step_task(&execution.id, "project-task").unwrap();

        for instance_id in [&agent_scoped.id, &task_scoped.id] {
            let error = mgr
                .set_context(instance_id, Some("project-two"), None)
                .unwrap_err();
            assert!(error
                .to_string()
                .contains("durable Agent or task work bound to project(s) project-one"));
            assert_eq!(mgr.get_instance(instance_id).unwrap().project_id, None);
        }

        let projects = crate::projects::ProjectManager::new(db);
        projects.begin_cascade("project-one").unwrap();
        projects.finish_cascade("project-one").unwrap();
        for instance_id in [&agent_scoped.id, &task_scoped.id] {
            assert!(matches!(
                mgr.get_instance(instance_id),
                Err(Error::WorkflowInstanceNotFound { .. })
            ));
        }
    }

    #[test]
    fn test_list_instances() {
        let (_, mgr) = setup();
        mgr.create_instance("wf1", None, None).unwrap();
        mgr.create_instance("wf1", None, None).unwrap();

        let list = mgr.list_instances("wf1", 10).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_list_running_instances() {
        let (_, mgr) = setup();
        let inst1 = mgr.create_instance("wf1", None, None).unwrap();
        let inst2 = mgr.create_instance("wf1", None, None).unwrap();

        mgr.complete_instance(&inst1.id).unwrap();

        let running = mgr.list_running_instances().unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, inst2.id);
    }

    #[test]
    fn test_update_status() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        mgr.update_status(&inst.id, "failed", Some("something broke"))
            .unwrap();

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.status, "failed");
        assert_eq!(fetched.error_message.as_deref(), Some("something broke"));
        assert!(fetched.completed_at.is_some());
    }

    #[test]
    fn test_set_current_position() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        mgr.set_current_position(&inst.id, "bug_flow", 2).unwrap();

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.current_flow, "bug_flow");
        assert_eq!(fetched.current_step_index, 2);
    }

    #[test]
    fn test_variable_store() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        let store = mgr.get_variable_store(&inst.id).unwrap();
        assert_eq!(store, "{}");

        mgr.update_variable_store(&inst.id, r#"{"classify": {"intent": "bug"}}"#)
            .unwrap();

        let store = mgr.get_variable_store(&inst.id).unwrap();
        assert_eq!(store, r#"{"classify": {"intent": "bug"}}"#);
    }

    #[test]
    fn test_loop_state() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();
        assert!(inst.loop_state.is_none());

        mgr.update_loop_state(&inst.id, Some(r#"{"index": 0, "items": [1,2,3]}"#))
            .unwrap();
        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert!(fetched.loop_state.is_some());

        mgr.update_loop_state(&inst.id, None).unwrap();
        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert!(fetched.loop_state.is_none());
    }

    #[test]
    fn test_create_and_get_step_execution() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        let exec = mgr
            .create_step_execution(&inst.id, "main", "step1", Some(r#"{"ctx": true}"#))
            .unwrap();

        assert_eq!(exec.instance_id, inst.id);
        assert_eq!(exec.flow_name, "main");
        assert_eq!(exec.step_id, "step1");
        assert_eq!(exec.status, "pending");
        assert_eq!(exec.attempt, 1);
        assert_eq!(exec.input_context.as_deref(), Some(r#"{"ctx": true}"#));
    }

    #[test]
    fn test_step_attempt_count() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        assert_eq!(mgr.get_step_attempt_count(&inst.id, "step1").unwrap(), 0);

        mgr.create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();
        assert_eq!(mgr.get_step_attempt_count(&inst.id, "step1").unwrap(), 1);

        mgr.create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();
        assert_eq!(mgr.get_step_attempt_count(&inst.id, "step1").unwrap(), 2);
    }

    #[test]
    fn test_set_step_task() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();
        let exec = mgr
            .create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();

        mgr.set_step_task(&exec.id, "task-123").unwrap();

        let fetched = mgr.get_step_execution(&exec.id).unwrap();
        assert_eq!(fetched.task_id.as_deref(), Some("task-123"));
        assert_eq!(fetched.status, "running");
    }

    #[test]
    fn test_find_execution_by_task() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();
        let exec = mgr
            .create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();

        mgr.set_step_task(&exec.id, "task-456").unwrap();

        let found = mgr.find_execution_by_task("task-456").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, exec.id);

        let not_found = mgr.find_execution_by_task("nonexistent").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn active_continuations_are_completed_before_other_default_source_gates() {
        let (_, mgr) = setup();
        let first = mgr.create_instance("wf1", None, None).unwrap();
        let second = mgr.create_instance("wf1", None, None).unwrap();

        let completed_source = mgr
            .create_step_execution(&first.id, "main", SOURCE_TASK_STEP_ID, None)
            .unwrap();
        mgr.set_step_task(&completed_source.id, "shared-task")
            .unwrap();
        mgr.update_step_status(&completed_source.id, "completed", None)
            .unwrap();

        let waiting_source = mgr
            .create_step_execution(&second.id, "main", SOURCE_TASK_STEP_ID, None)
            .unwrap();
        mgr.set_step_task(&waiting_source.id, "shared-task")
            .unwrap();

        let continuation = mgr
            .create_step_execution(&first.id, "main", "final_check", None)
            .unwrap();
        mgr.set_step_task(&continuation.id, "shared-task").unwrap();

        let active = mgr.find_running_executions_by_task("shared-task").unwrap();
        assert_eq!(
            active
                .iter()
                .map(|execution| &execution.id)
                .collect::<Vec<_>>(),
            vec![&continuation.id, &waiting_source.id]
        );
    }

    #[test]
    fn test_update_step_status() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();
        let exec = mgr
            .create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();

        mgr.update_step_status(&exec.id, "completed", Some("output text"))
            .unwrap();

        let fetched = mgr.get_step_execution(&exec.id).unwrap();
        assert_eq!(fetched.status, "completed");
        assert_eq!(fetched.output.as_deref(), Some("output text"));
        assert!(fetched.completed_at.is_some());
    }

    #[test]
    fn test_list_step_executions() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        mgr.create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();
        mgr.create_step_execution(&inst.id, "main", "step2", None)
            .unwrap();

        let execs = mgr.list_step_executions(&inst.id).unwrap();
        assert_eq!(execs.len(), 2);
    }

    #[test]
    fn test_complete_instance() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        mgr.complete_instance(&inst.id).unwrap();

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.status, "completed");
        assert!(fetched.completed_at.is_some());
    }

    #[test]
    fn test_get_instance_not_found() {
        let (_, mgr) = setup();
        assert!(matches!(
            mgr.get_instance("nonexistent"),
            Err(Error::WorkflowInstanceNotFound { .. })
        ));
    }
}
