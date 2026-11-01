use super::*;
use crate::workflows::instance::ensure_instance_accepts_work;

impl WorkflowEngine {
    pub(super) fn execute_parallel_step(
        &self,
        instance_id: &str,
        flow_name: &str,
        step: &Step,
        ctx: &Value,
        ctx_json: &str,
    ) -> Result<()> {
        let branches = step.body.as_deref().unwrap_or_default();
        let prepared = branches
            .iter()
            .map(|branch| self.prepare_task_step(instance_id, flow_name, branch, ctx))
            .collect::<Result<Vec<_>>>()?;
        // Publish the entire group before exposing any branch to the dispatcher.
        let ids = self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            ensure_instance_accepts_work(&tx, instance_id)?;
            let parent = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO workflow_step_executions
                 (id, instance_id, flow_name, step_id, status, input_context, attempt, started_at)
                 VALUES (?1, ?2, ?3, ?4, 'running', ?5,
                    (SELECT COUNT(*) + 1 FROM workflow_step_executions WHERE instance_id = ?2 AND step_id = ?4),
                    CURRENT_TIMESTAMP)",
                rusqlite::params![parent, instance_id, flow_name, step.id, ctx_json],
            )?;
            tx.execute("UPDATE workflow_instances SET current_execution_id = ?1 WHERE id = ?2", rusqlite::params![parent, instance_id])?;
            let mut ids = Vec::new();
            for (branch, task) in branches.iter().zip(&prepared) {
                let id = InstanceManager::create_task_execution_in_transaction(
                    &tx, instance_id, flow_name, &branch.id, Some(ctx_json), &task.task_id,
                )?;
                tx.execute("UPDATE workflow_step_executions SET parent_execution_id = ?1 WHERE id = ?2",
                    rusqlite::params![parent, id])?;
                ids.push(id);
            }
            tx.commit()?;
            Ok::<_, Error>(ids)
        })?;
        for ((branch, task), id) in branches.iter().zip(&prepared).zip(ids) {
            self.dispatch_task_step(branch, task, &self.instances.get_step_execution(&id)?)?;
        }
        Ok(())
    }

    pub(super) fn complete_parallel_branch(
        &self,
        exec: &StepExecution,
        status: &str,
        output: &str,
    ) -> Result<()> {
        let parent = exec
            .parent_execution_id
            .as_deref()
            .ok_or_else(|| Error::Workflow("parallel branch has no parent".into()))?;
        let claimed = self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE workflow_step_executions SET status = ?1, output = ?2, completed_at = CURRENT_TIMESTAMP
                 WHERE id = ?3 AND status = 'running'
                   AND EXISTS (SELECT 1 FROM workflow_instances WHERE id = ?4 AND status = 'running')
                   AND EXISTS (SELECT 1 FROM workflow_step_executions WHERE id = ?5 AND status = 'running')",
                rusqlite::params![status, output, exec.id, exec.instance_id, parent],
            )?;
            let claimed = tx.execute(
                "UPDATE workflow_step_executions SET status = 'resuming'
                 WHERE id = ?1 AND status = 'running'
                   AND EXISTS (SELECT 1 FROM workflow_instances WHERE id = ?2 AND status = 'running')
                   AND NOT EXISTS (SELECT 1 FROM workflow_step_executions
                     WHERE parent_execution_id = ?1 AND status NOT IN ('completed', 'failed', 'cancelled'))",
                rusqlite::params![parent, exec.instance_id],
            )? == 1;
            tx.commit()?;
            Ok::<_, Error>(claimed)
        })?;
        if claimed {
            self.finish_parallel(parent)?;
        }
        Ok(())
    }

    fn finish_parallel(&self, parent_id: &str) -> Result<()> {
        let parent = self.instances.get_step_execution(parent_id)?;
        let instance = self.instances.get_instance(&parent.instance_id)?;
        if instance.status != "running" {
            return Ok(());
        }
        let definition = self.definition_for_instance(&instance)?;
        let Some(index) = definition.step_index(&parent.flow_name, &parent.step_id) else {
            return Err(Error::Workflow(
                "parallel parent is not a top-level step".into(),
            ));
        };
        // A replay after the next step was dispatched must not dispatch it twice.
        if instance.current_flow != parent.flow_name
            || instance.current_step_index != index as i32
            || instance.current_execution_id.as_deref() != Some(parent_id)
        {
            return Ok(());
        }
        let executions = self.instances.list_step_executions(&instance.id)?;
        let branches: Vec<_> = executions
            .iter()
            .filter(|child| child.parent_execution_id.as_deref() == Some(parent_id))
            .collect();
        if branches.is_empty()
            || branches
                .iter()
                .any(|child| !matches!(child.status.as_str(), "completed" | "failed" | "cancelled"))
        {
            return Ok(());
        }
        let mut variables = self.load_variable_store(&instance.id)?;
        for branch in &branches {
            let output = parse_task_output(branch.output.as_deref().unwrap_or_default());
            let value = if let Some(schema) = definition
                .find_step(&branch.flow_name, &branch.step_id)
                .and_then(|step| step.outputs.as_ref())
            {
                match output.as_object() {
                    Some(map) => Value::Object(
                        map.iter()
                            .filter(|(name, _)| schema.contains_key(*name))
                            .map(|(name, value)| (name.clone(), value.clone()))
                            .collect(),
                    ),
                    None => serde_json::json!({"output": output}),
                }
            } else if output.is_object() {
                output
            } else {
                serde_json::json!({"output": output})
            };
            variables.insert(branch.step_id.clone(), value);
        }
        self.save_variable_store(&instance.id, &variables)?;
        let failed: Vec<_> = branches
            .iter()
            .filter(|child| child.status != "completed")
            .map(|child| child.step_id.as_str())
            .collect();
        let trigger = instance
            .trigger_data
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or(Value::Null);
        let status = if failed.is_empty() {
            "completed"
        } else {
            "failed"
        };
        let summary = serde_json::json!({"branches": branches.len(), "failed": failed}).to_string();
        self.instances
            .update_step_status(parent_id, status, Some(&summary))?;
        if failed.is_empty() {
            self.advance_to_next(
                &instance.id,
                &parent.flow_name,
                index,
                &definition,
                &trigger,
                &variables,
            )
        } else if definition.flows.contains_key("on_error") {
            self.execute_step(
                &instance.id,
                "on_error",
                0,
                &definition,
                &trigger,
                &variables,
            )
        } else {
            self.instances.update_status(
                &instance.id,
                "failed",
                Some(&format!(
                    "parallel step '{}' failed: {}",
                    parent.step_id,
                    failed.join(", ")
                )),
            )
        }
    }

    pub(super) fn recover_parallel(&self, instance: &WorkflowInstance) -> Result<bool> {
        let definition = self.definition_for_instance(instance)?;
        let Some(_) = definition
            .flows
            .get(&instance.current_flow)
            .and_then(|flow| flow.steps.get(instance.current_step_index as usize))
            .filter(|step| step.step_type == "parallel")
        else {
            return Ok(false);
        };
        let executions = self.instances.list_step_executions(&instance.id)?;
        let Some(parent) = executions
            .iter()
            .find(|exec| instance.current_execution_id.as_deref() == Some(exec.id.as_str()))
        else {
            // The position was saved just before the atomic group publication.
            let variables = self.load_variable_store(&instance.id)?;
            let trigger = instance
                .trigger_data
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            self.execute_step(
                &instance.id,
                &instance.current_flow,
                instance.current_step_index as usize,
                &definition,
                &trigger,
                &variables,
            )?;
            return Ok(true);
        };
        for child in executions.iter().filter(|exec| {
            exec.parent_execution_id.as_deref() == Some(parent.id.as_str())
                && exec.status == "running"
        }) {
            let task_id = child
                .task_id
                .as_deref()
                .ok_or_else(|| Error::Workflow("parallel branch has no task".into()))?;
            let task = TaskBoard::new(self.db.clone()).get(task_id)?;
            if let Some(status) =
                self.recoverable_task_status(task_id, task.status.as_str(), Some(&child.status))?
            {
                let output = self.completed_task_output(task_id);
                self.on_execution_task_completed(
                    child,
                    task_id,
                    if status == "completed" {
                        "completed"
                    } else {
                        "failed"
                    },
                    &output,
                    None,
                )?;
            } else if task.status.as_str() == "pending" {
                // Recover an unpublished initial dispatch. Other nonterminal
                // tasks keep their existing work or wait for a user response.
                if let Some(agent_id) = task.agent_id.as_deref() {
                    TaskQueue::new(self.db.clone()).ensure_enqueued(task_id, agent_id)?;
                }
            }
        }
        // Includes the crash boundary after all child results were committed,
        // or after the join finished but before the following task was created.
        self.finish_parallel(&parent.id)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::{commands, manager::CreateWorkflow};
    use serde_json::json;

    const PARALLEL: &str = r#"
name: parallel-test
flows:
  main:
    steps:
      - id: proposals
        type: parallel
        steps:
          - id: a
            agent: atlas
            prompt: independent A
            new_session: true
            outputs:
              proposal: { type: string }
          - id: b
            agent: dev
            prompt: independent B
            new_session: true
            outputs:
              proposal: { type: string }
      - id: solve
        agent: atlas
        prompt: "Compare @a.proposal and @b.proposal"
"#;

    fn start(yaml: &str) -> (Arc<Database>, WorkflowEngine, String) {
        start_with_inputs(yaml, json!({}))
    }

    fn start_with_inputs(yaml: &str, inputs: Value) -> (Arc<Database>, WorkflowEngine, String) {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db.clone());
        for agent in ["atlas", "dev"] {
            registry.ensure(agent, "codex").unwrap();
        }
        let workflow = WorkflowManager::new(db.clone())
            .create(&CreateWorkflow {
                name: "test".into(),
                description: None,
                yaml_content: yaml.into(),
            })
            .unwrap();
        let engine = WorkflowEngine::new(db.clone());
        let id = engine.start_instance(&workflow.id, inputs).unwrap();
        (db, engine, id)
    }

    fn branches(engine: &WorkflowEngine, id: &str) -> Vec<StepExecution> {
        engine
            .instances
            .list_step_executions(id)
            .unwrap()
            .into_iter()
            .filter(|exec| exec.parent_execution_id.is_some())
            .collect()
    }

    #[test]
    fn parallel_waits_for_every_branch_and_joins_out_of_order_once() {
        let (db, engine, id) = start(PARALLEL);
        let branches = branches(&engine, &id);
        assert_eq!(branches.len(), 2);
        assert!(TaskQueue::new(db.clone()).claim("atlas").unwrap().is_some());
        assert!(TaskQueue::new(db.clone()).claim("dev").unwrap().is_some());
        engine
            .on_task_completed(
                branches[1].task_id.as_deref().unwrap(),
                "completed",
                r#"{"proposal":"B"}"#,
            )
            .unwrap();
        assert_eq!(engine.instances.list_step_executions(&id).unwrap().len(), 3);
        engine
            .on_task_completed(
                branches[0].task_id.as_deref().unwrap(),
                "completed",
                r#"{"proposal":"A"}"#,
            )
            .unwrap();
        engine
            .on_task_completed(
                branches[0].task_id.as_deref().unwrap(),
                "completed",
                r#"{"proposal":"duplicate"}"#,
            )
            .unwrap();
        let executions = engine.instances.list_step_executions(&id).unwrap();
        assert_eq!(executions.len(), 4);
        let task = TaskBoard::new(db)
            .get(executions[3].task_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(task.description.as_deref(), Some("Compare A and B"));
        assert_eq!(
            engine
                .instances
                .get_instance(&id)
                .unwrap()
                .current_step_index,
            1
        );
    }

    #[test]
    fn parallel_failure_waits_for_siblings_before_entering_error_flow() {
        let yaml = format!("{PARALLEL}\n  on_error:\n    steps:\n      - id: report\n        agent: atlas\n        prompt: Diagnose @a and @b\n");
        let (_, engine, id) = start(&yaml);
        let branches = branches(&engine, &id);
        engine
            .on_task_completed(
                branches[0].task_id.as_deref().unwrap(),
                "failed",
                "A failed",
            )
            .unwrap();
        assert_eq!(
            engine.instances.get_instance(&id).unwrap().current_flow,
            "main"
        );
        engine
            .on_task_completed(
                branches[1].task_id.as_deref().unwrap(),
                "completed",
                "B finished",
            )
            .unwrap();
        assert_eq!(
            engine.instances.get_instance(&id).unwrap().current_flow,
            "on_error"
        );
        assert!(!engine
            .instances
            .list_step_executions(&id)
            .unwrap()
            .iter()
            .any(|exec| exec.step_id == "solve"));
    }

    #[test]
    fn parallel_callbacks_can_finish_concurrently_without_losing_outputs() {
        let (db, engine, id) = start(PARALLEL);
        let branches = branches(&engine, &id);
        std::thread::scope(|scope| {
            for branch in &branches {
                let db = db.clone();
                scope.spawn(move || {
                    WorkflowEngine::new(db)
                        .on_task_completed(
                            branch.task_id.as_deref().unwrap(),
                            "completed",
                            &json!({"proposal": branch.step_id}).to_string(),
                        )
                        .unwrap();
                });
            }
        });
        let variables = engine.load_variable_store(&id).unwrap();
        assert_eq!(variables["a"]["proposal"], "a");
        assert_eq!(variables["b"]["proposal"], "b");
        assert_eq!(
            engine
                .instances
                .list_step_executions(&id)
                .unwrap()
                .iter()
                .filter(|exec| exec.step_id == "solve")
                .count(),
            1
        );
    }

    #[test]
    fn recovery_recovers_all_branches_and_committed_join_without_duplicates() {
        let (db, engine, id) = start(PARALLEL);
        let branches = branches(&engine, &id);
        // Simulate completion committed just before the join callback.
        for branch in &branches {
            engine
                .instances
                .update_step_status(
                    &branch.id,
                    "completed",
                    Some(&json!({"proposal": branch.step_id}).to_string()),
                )
                .unwrap();
        }
        WorkflowEngine::new(db.clone()).recover().unwrap();
        WorkflowEngine::new(db.clone()).recover().unwrap();
        let executions = engine.instances.list_step_executions(&id).unwrap();
        assert_eq!(
            executions
                .iter()
                .filter(|exec| exec.step_id == "solve")
                .count(),
            1
        );
        let parent = &executions[0];
        // Replaying a completed group must not move the current position back.
        engine.finish_parallel(&parent.id).unwrap();
        assert_eq!(engine.instances.list_step_executions(&id).unwrap().len(), 4);
    }

    #[test]
    fn recovery_requeues_every_unpublished_branch_once() {
        let (db, engine, id) = start(PARALLEL);
        let branches = branches(&engine, &id);
        db.with_conn(|conn| {
            conn.execute_batch("DELETE FROM work_attempts; DELETE FROM task_queue;")
        })
        .unwrap();
        engine.recover().unwrap();
        engine.recover().unwrap();
        let queue = TaskQueue::new(db);
        for agent in ["atlas", "dev"] {
            assert!(queue.claim(agent).unwrap().is_some());
        }
        assert_eq!(
            engine.instances.list_step_executions(&id).unwrap().len(),
            branches.len() + 1
        );
    }

    #[test]
    fn recovery_preserves_parallel_questions_until_the_user_answers() {
        for status_after_question in ["waiting_for_input", "in_progress"] {
            let (db, engine, id) = start(PARALLEL);
            let branches = branches(&engine, &id);
            let task_id = branches[0].task_id.as_deref().unwrap();
            let queue = TaskQueue::new(db.clone());
            let board = TaskBoard::new(db.clone());
            let conversation = TaskConversation::new(db.clone());
            let complete_response = |item: &crate::tasks::queue::QueueItem, output: &str| {
                conversation
                    .complete_final_assistant_attempt(
                        crate::tasks::conversation::FinalAssistantAttempt {
                            task_id,
                            queue_id: item.id,
                            attempt_id: item.attempt_id.as_deref().unwrap(),
                            completion_summary: output,
                            content: output,
                            visualizations: &[],
                            published_files: &[],
                        },
                    )
                    .unwrap()
                    .unwrap();
            };
            let question = queue.claim("atlas").unwrap().unwrap();
            complete_response(
                &question,
                "NEEDS_USER_INPUT: Which constraint should I use?",
            );
            // Also cover a restart between persisting the question and marking
            // the task as waiting: recovery must park it without dispatching.
            board
                .update_status(task_id, status_after_question, Some("atlas"))
                .unwrap();
            engine
                .on_task_completed(
                    branches[1].task_id.as_deref().unwrap(),
                    "completed",
                    r#"{"proposal":"B"}"#,
                )
                .unwrap();

            let recovered = WorkflowEngine::new(db.clone());
            recovered.recover().unwrap();
            recovered.recover().unwrap();
            assert_eq!(
                board.get(task_id).unwrap().status.as_str(),
                "waiting_for_input"
            );
            assert!(!queue.has_active_response_for_task(task_id).unwrap());
            assert!(queue.claim("atlas").unwrap().is_none());
            assert_eq!(
                recovered.instances.list_step_executions(&id).unwrap().len(),
                3
            );
            assert_eq!(
                recovered
                    .instances
                    .get_step_execution(&branches[0].id)
                    .unwrap()
                    .status,
                "running"
            );

            let (_, answer) = conversation
                .add_user_message_with_attachments_and_enqueue(
                    task_id,
                    Some("atlas"),
                    "Use the tighter constraint.",
                    &[],
                )
                .unwrap();
            recovered.recover().unwrap();
            let response = queue.claim("atlas").unwrap().unwrap();
            assert_eq!(response.id, answer.unwrap().id);
            complete_response(&response, r#"{"proposal":"A"}"#);
            board
                .update_status(task_id, "completed", Some("atlas"))
                .unwrap();
            recovered
                .on_task_completed(task_id, "completed", r#"{"proposal":"A"}"#)
                .unwrap();
            let executions = recovered.instances.list_step_executions(&id).unwrap();
            assert_eq!(executions.len(), 4);
            let solve = board
                .get(executions[3].task_id.as_deref().unwrap())
                .unwrap();
            assert_eq!(solve.description.as_deref(), Some("Compare A and B"));
        }
    }

    #[test]
    fn cancelling_parallel_stops_all_owned_tasks_and_ignores_late_results() {
        let (db, engine, id) = start(PARALLEL);
        let branches = branches(&engine, &id);
        let cancelled = engine.cancel_instance(&id, "test cancellation").unwrap();
        assert_eq!(cancelled.owned_tasks.len(), 2);
        for branch in &branches {
            let task = TaskBoard::new(db.clone())
                .get(branch.task_id.as_deref().unwrap())
                .unwrap();
            assert_eq!(task.status.as_str(), "cancelled");
            engine
                .on_task_completed(&task.id, "completed", "late")
                .unwrap();
        }
        assert_eq!(
            engine.instances.get_instance(&id).unwrap().status,
            "cancelled"
        );
        assert_eq!(engine.instances.list_step_executions(&id).unwrap().len(), 3);
        assert!(TaskQueue::new(db).claim("atlas").unwrap().is_none());
    }

    const COMMAND: &str = r#"
name: verification
flows:
  main:
    steps:
      - id: test
        type: command
        agent: atlas
        command: "printf failure; exit 1"
        allow_failure: true
      - id: verify
        type: command
        agent: atlas
        command: "exit 1"
"#;
    fn result(code: i32) -> commands::CommandResult {
        commands::CommandResult {
            exit_code: Some(code),
            stdout: "actual output".into(),
            stderr: String::new(),
            signal: None,
            timed_out: false,
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }

    #[test]
    fn commands_capture_initial_failure_and_enforce_final_exit_status() {
        let (db, engine, id) = start(COMMAND);
        let first = engine
            .instances
            .list_step_executions(&id)
            .unwrap()
            .remove(0);
        let task_id = first.task_id.as_deref().unwrap();
        let command = commands::for_task(&db, task_id).unwrap().unwrap();
        assert!(commands::begin(&db, &command).unwrap().is_none());
        commands::save_result(&db, &command, &result(1)).unwrap();
        engine
            .on_task_completed(task_id, "completed", "model says success")
            .unwrap();
        let second = engine
            .instances
            .list_step_executions(&id)
            .unwrap()
            .remove(1);
        assert_eq!(
            engine.load_variable_store(&id).unwrap()["test"]["exit_code"],
            1
        );
        let command = commands::for_task(&db, second.task_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        commands::begin(&db, &command).unwrap();
        commands::save_result(&db, &command, &result(1)).unwrap();
        engine
            .on_task_completed(
                second.task_id.as_deref().unwrap(),
                "completed",
                "claimed pass",
            )
            .unwrap();
        assert_eq!(engine.instances.get_instance(&id).unwrap().status, "failed");
    }

    #[test]
    fn command_cannot_be_completed_without_running_and_does_not_replay_after_restart() {
        let (db, engine, id) = start(COMMAND);
        let first = engine
            .instances
            .list_step_executions(&id)
            .unwrap()
            .remove(0);
        let task_id = first.task_id.as_deref().unwrap();
        let command = commands::for_task(&db, task_id).unwrap().unwrap();
        commands::begin(&db, &command).unwrap();
        assert!(commands::begin(&db, &command)
            .unwrap_err()
            .to_string()
            .contains("interrupted"));
        engine
            .on_task_completed(task_id, "completed", r#"{"exit_code":0}"#)
            .unwrap();
        assert_eq!(engine.instances.get_instance(&id).unwrap().status, "failed");
    }

    #[test]
    fn committed_command_result_is_reused_and_timeout_is_never_allowed() {
        let (db, _, id) = start(COMMAND);
        let instances = InstanceManager::new(db.clone());
        let first = instances.list_step_executions(&id).unwrap().remove(0);
        let command = commands::for_task(&db, first.task_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        commands::begin(&db, &command).unwrap();
        commands::save_result(&db, &command, &result(0)).unwrap();
        assert_eq!(
            commands::begin(&db, &command).unwrap().unwrap().exit_code,
            Some(0)
        );
        let mut timed = result(0);
        timed.timed_out = true;
        assert!(!timed.succeeds(true));
    }

    #[test]
    fn parallel_and_command_validation_rejects_invalid_shapes_and_nested_jumps() {
        for yaml in [
            PARALLEL.replace(
                "type: parallel",
                "type: parallel\n        steps: []\n        body:",
            ),
            PARALLEL.replace("agent: dev", "type: wait\n            agent: dev"),
            COMMAND.replace("printf failure; exit 1", ""),
            COMMAND.replace("allow_failure: true", "cwd: ../escape"),
            COMMAND.replace("allow_failure: true", "timeout: 48h"),
            PARALLEL.replace(
                "prompt: \"Compare @a.proposal and @b.proposal\"",
                "type: jump\n        target: step a",
            ),
        ] {
            assert!(
                WorkflowDefinition::parse(&yaml)
                    .and_then(|definition| definition.validate())
                    .is_err(),
                "accepted invalid definition: {yaml}"
            );
        }
    }
    #[test]
    fn team_solve_runs_both_repair_paths_and_requires_final_verification() {
        let yaml = include_str!("../../../../frontend/src/lib/workflows/templates/team-solve.yaml")
            .replace("__WORKFLOW_NAME__", "team-solve-test");
        for repair in [true, false] {
            let (db, engine, id) = start_with_inputs(
                &yaml,
                json!({
                    "problem": "Solve the sample", "verification_command": "bash check.sh",
                    "coordinator": "atlas", "ideator_a": "atlas", "ideator_b": "dev", "worker": "dev"
                }),
            );
            let complete = |step: &str, output: Value| {
                let execution = engine
                    .instances
                    .list_step_executions(&id)
                    .unwrap()
                    .into_iter()
                    .find(|execution| execution.step_id == step && execution.status == "running")
                    .unwrap();
                engine
                    .on_task_completed(
                        execution.task_id.as_deref().unwrap(),
                        "completed",
                        &output.to_string(),
                    )
                    .unwrap();
            };
            complete(
                "plan",
                json!({"brief":"Requirements and correctness checks"}),
            );
            complete("proposal_b", json!({"proposal":"B"}));
            complete("proposal_a", json!({"proposal":"A"}));
            complete(
                "solve",
                json!({"implementation":"Source, decision, and changed files"}),
            );
            let test = engine
                .instances
                .list_step_executions(&id)
                .unwrap()
                .into_iter()
                .find(|execution| execution.step_id == "test")
                .unwrap();
            let command = commands::for_task(&db, test.task_id.as_deref().unwrap())
                .unwrap()
                .unwrap();
            commands::begin(&db, &command).unwrap();
            commands::save_result(&db, &command, &result(if repair { 1 } else { 0 })).unwrap();
            complete("test", json!({}));
            complete(
                "review",
                json!({"repair_needed":repair,"instructions":"Checked the result"}),
            );
            if repair {
                complete("repair", json!({"summary":"Fixed"}));
            }
            let verify = engine
                .instances
                .list_step_executions(&id)
                .unwrap()
                .into_iter()
                .find(|execution| execution.step_id == "verify")
                .unwrap();
            let command = commands::for_task(&db, verify.task_id.as_deref().unwrap())
                .unwrap()
                .unwrap();
            commands::begin(&db, &command).unwrap();
            commands::save_result(&db, &command, &result(0)).unwrap();
            complete("verify", json!({}));
            assert_eq!(
                engine.instances.get_instance(&id).unwrap().status,
                "completed"
            );
            assert_eq!(
                engine
                    .instances
                    .list_step_executions(&id)
                    .unwrap()
                    .iter()
                    .filter(|execution| execution.step_id == "repair")
                    .count(),
                usize::from(repair)
            );
        }
    }

    #[test]
    fn recovery_advances_a_committed_command_completion_once() {
        let (db, engine, id) = start(COMMAND);
        let first = engine
            .instances
            .list_step_executions(&id)
            .unwrap()
            .remove(0);
        let command = commands::for_task(&db, first.task_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        commands::begin(&db, &command).unwrap();
        commands::save_result(&db, &command, &result(0)).unwrap();
        engine
            .instances
            .update_step_status(&first.id, "completed", Some("done"))
            .unwrap();
        engine.recover().unwrap();
        engine.recover().unwrap();
        assert_eq!(engine.instances.list_step_executions(&id).unwrap().len(), 2);
        assert_eq!(
            engine
                .instances
                .get_instance(&id)
                .unwrap()
                .current_step_index,
            1
        );
    }

    #[test]
    fn recovery_does_not_reuse_an_earlier_parallel_cycle() {
        let (_, engine, id) = start(PARALLEL);
        let old = branches(&engine, &id);
        for branch in &old {
            engine
                .on_task_completed(
                    branch.task_id.as_deref().unwrap(),
                    "completed",
                    r#"{"proposal":"old"}"#,
                )
                .unwrap();
        }
        // A jump saved the position, but the process stopped before publishing
        // the second group. Its previous results must not satisfy this join.
        engine
            .instances
            .set_current_position(&id, "main", 0)
            .unwrap();
        engine.recover().unwrap();
        let groups = engine.instances.list_step_executions(&id).unwrap();
        assert_eq!(
            groups
                .iter()
                .filter(|execution| execution.step_id == "proposals")
                .count(),
            2
        );
        assert_eq!(
            groups
                .iter()
                .filter(|execution| execution.parent_execution_id.is_some()
                    && execution.status == "running")
                .count(),
            2
        );
    }
    #[test]
    fn default_parallel_workflow_waits_for_its_source_during_recovery() {
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("atlas", "codex")
            .unwrap();
        let yaml = PARALLEL.replace("flows:", "inputs:\n  worker:\n    type: agent\n    required: true\n    primary: true\nflows:")
            .replace("agent: atlas", "agent: \"@worker\"").replace("agent: dev", "agent: \"@worker\"");
        let manager = WorkflowManager::new(db.clone());
        let workflow = manager
            .create(&CreateWorkflow {
                name: "default-parallel".into(),
                description: None,
                yaml_content: yaml,
            })
            .unwrap();
        manager.set_default_for_tasks(&workflow.id, true).unwrap();
        let source = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Source".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let engine = WorkflowEngine::new(db.clone());
        let id = engine
            .attach_default_workflows_to_task(&source.id)
            .unwrap()
            .remove(0);
        engine.recover().unwrap();
        assert_eq!(engine.instances.list_step_executions(&id).unwrap().len(), 1);
        assert!(branches(&engine, &id).is_empty());
    }
}
