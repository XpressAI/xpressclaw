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
        // Older persisted results omit derived fields. Recovery must still
        // expose an authoritative passed flag to selection gates.
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_step_executions SET command_result = ?1 WHERE id = ?2",
                rusqlite::params![
                    serde_json::to_string(&result(1)).unwrap(),
                    command.execution_id
                ],
            )
        })
        .unwrap();
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
        assert_eq!(
            engine.load_variable_store(&id).unwrap()["test"]["passed"],
            false
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
    fn committed_command_results_preserve_pass_status_for_captured_timeouts() {
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
        assert!(timed.succeeds(true));
        assert!(!timed.succeeds(false));
        let output: Value = serde_json::from_str(&timed.json_output().unwrap()).unwrap();
        assert_eq!(output["passed"], false);
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
    fn choose_best_checks_each_candidate_and_delivers_only_a_verified_winner() {
        let yaml =
            include_str!("../../../../frontend/src/lib/workflows/templates/choose-best.yaml")
                .replace("__WORKFLOW_NAME__", "choose-best-test");
        for (a_exit, b_exit, winner, final_exit, remember) in [
            (0, 0, "a", 0, false),
            (0, 0, "b", 0, true),
            (0, 1, "a", 0, true),
            (1, 0, "b", 0, false),
            (1, 0, "a", 0, true),
            (0, 1, "b", 0, false),
            (1, 1, "none", 0, true),
            (0, 0, "invalid", 0, false),
            (0, 0, "b", 1, true),
            (-1, 0, "b", 0, false),
            (-1, 0, "a", 0, true),
            (0, -1, "a", 0, false),
            (0, 0, "a", -1, true),
        ] {
            let mut inputs = json!({
                "goal": "Complete the sample", "verification_command": "bash check.sh",
                "priorities": "Correctness, then maintenance",
                "refine_winner": false,
                "coordinator": "atlas", "candidate_a": "atlas", "candidate_b": "dev", "worker": "dev"
            });
            if remember {
                inputs["remember_decision"] = json!(true);
            }
            let (db, engine, id) = start_with_inputs(&yaml, inputs);
            let execution = |step: &str| {
                engine
                    .instances
                    .list_step_executions(&id)
                    .unwrap()
                    .into_iter()
                    .find(|execution| execution.step_id == step && execution.status == "running")
                    .unwrap()
            };
            let complete = |step: &str, output: Value| {
                let execution = execution(step);
                engine
                    .on_task_completed(
                        execution.task_id.as_deref().unwrap(),
                        "completed",
                        &output.to_string(),
                    )
                    .unwrap();
            };
            let execute_command = |step: &str, code: i32| {
                let execution = execution(step);
                let command = commands::for_task(&db, execution.task_id.as_deref().unwrap())
                    .unwrap()
                    .unwrap();
                commands::begin(&db, &command).unwrap();
                let mut outcome = result(code);
                if code == -1 {
                    outcome.exit_code = None;
                    outcome.timed_out = true;
                    outcome.signal = Some("SIGKILL".into());
                }
                commands::save_result(&db, &command, &outcome).unwrap();
                complete(step, json!({}));
                command
            };
            let all_steps = || engine.instances.list_step_executions(&id).unwrap();
            complete(
                "prepare",
                json!({
                    "baseline":"Pinned source and local patch", "destination":"/workspace/original",
                    "project_context":"Use the project convention from note 42"
                }),
            );
            let rubric_task = TaskBoard::new(db.clone())
                .get(execution("rubric").task_id.as_deref().unwrap())
                .unwrap();
            assert!(rubric_task
                .description
                .unwrap()
                .contains("Correctness, then maintenance"));
            complete(
                "rubric",
                json!({
                    "requirements":["Required check passes"],
                    "criteria":[{"id":"maintainability", "weight":100}],
                    "tie_break":"Simpler implementation", "brief":"Use the same pinned source",
                    "verification_script":"bash check.sh"
                }),
            );
            assert!(!all_steps().iter().any(|step| step.step_id == "check_a"));
            complete(
                "alternative_b",
                json!({"workspace":"/workspace/.worktrees/b", "change_set":"patch B", "rationale":"B approach"}),
            );
            assert!(!all_steps().iter().any(|step| step.step_id == "checks"));
            complete(
                "alternative_a",
                json!({"workspace":"/workspace/.worktrees/a", "change_set":"patch A", "rationale":"A approach"}),
            );
            let check_a = execute_command("check_a", a_exit);
            assert_eq!(check_a.script, "bash check.sh");
            assert_eq!(check_a.cwd, "/workspace/.worktrees/a");
            assert!(!all_steps().iter().any(|step| step.step_id == "evaluate"));
            let check_b = execute_command("check_b", b_exit);
            assert_eq!(check_b.script, check_a.script);
            assert_eq!(check_b.cwd, "/workspace/.worktrees/b");
            complete(
                "evaluate",
                json!({"winner":winner, "scorecard":[{"a_exit":a_exit, "b_exit":b_exit}], "rationale":"Compared against the predefined criteria"}),
            );
            let eligible = (winner == "a" && a_exit == 0) || (winner == "b" && b_exit == 0);
            if !eligible {
                assert_eq!(
                    engine.instances.get_instance(&id).unwrap().current_flow,
                    "no_winner"
                );
                assert!(!all_steps()
                    .iter()
                    .any(|step| matches!(step.step_id.as_str(), "adopt" | "remember" | "deliver")));
                complete(
                    "explain_no_winner",
                    json!("No eligible selection; report attached."),
                );
                assert_eq!(execute_command("reject_selection", 1).script, "exit 1");
                assert_eq!(engine.instances.get_instance(&id).unwrap().status, "failed");
                continue;
            }
            complete(
                "adopt",
                json!({"workspace":"/workspace/original", "summary":"Applied the chosen implementation"}),
            );
            let verify = execute_command("verify", final_exit);
            assert_eq!(verify.script, check_a.script);
            assert_eq!(verify.cwd, "/workspace/original");
            assert!(!verify.allow_failure);
            if final_exit != 0 {
                assert_eq!(engine.instances.get_instance(&id).unwrap().status, "failed");
                assert!(!all_steps()
                    .iter()
                    .any(|step| matches!(step.step_id.as_str(), "remember" | "deliver")));
                continue;
            }
            if remember {
                assert!(!all_steps().iter().any(|step| step.step_id == "deliver"));
                complete("remember", json!({"note_id":"verified-decision"}));
            } else {
                assert!(!all_steps().iter().any(|step| step.step_id == "remember"));
            }
            let delivery = TaskBoard::new(db.clone())
                .get(execution("deliver").task_id.as_deref().unwrap())
                .unwrap();
            let prompt = delivery.description.unwrap();
            assert!(!prompt.contains("@remember.note_id"));
            assert!(prompt.contains("Applied the chosen implementation"));
            assert_eq!(prompt.contains("verified-decision"), remember);
            complete(
                "deliver",
                json!("Completed and verified the task. Comparison report attached."),
            );
            assert_eq!(
                engine.instances.get_instance(&id).unwrap().status,
                "completed"
            );
        }
    }

    #[test]
    fn choose_best_preserves_the_winner_unless_refinement_passes_and_scores_higher() {
        let yaml =
            include_str!("../../../../frontend/src/lib/workflows/templates/choose-best.yaml")
                .replace("__WORKFLOW_NAME__", "refine-winner-test");
        for (
            worth_refining,
            before_exit,
            after_exit,
            requirements_met,
            after_score,
            expected_flow,
        ) in [
            (false, 0, 0, true, 90, "adopt_original"),
            (true, 0, 0, true, 90, "adopt_refined"),
            (true, 0, 0, true, 80, "adopt_original"),
            (true, 0, 0, true, 70, "adopt_original"),
            (true, 0, 1, true, 90, "adopt_original"),
            (true, 0, -1, true, 90, "adopt_original"),
            (true, 1, 0, true, 90, "no_winner"),
            (true, 0, 0, false, 90, "adopt_original"),
        ] {
            let (db, engine, id) = start_with_inputs(
                &yaml,
                json!({
                    "goal":"Complete the task", "coordinator":"atlas",
                    "candidate_a":"atlas", "candidate_b":"dev", "worker":"dev"
                }),
            );
            let execution = |step: &str| {
                engine
                    .instances
                    .list_step_executions(&id)
                    .unwrap()
                    .into_iter()
                    .find(|execution| execution.step_id == step && execution.status == "running")
                    .unwrap()
            };
            let complete = |step: &str, output: Value| {
                engine
                    .on_task_completed(
                        execution(step).task_id.as_deref().unwrap(),
                        "completed",
                        &output.to_string(),
                    )
                    .unwrap();
            };
            let command = |step: &str, code: i32| {
                let task = execution(step);
                let command = commands::for_task(&db, task.task_id.as_deref().unwrap())
                    .unwrap()
                    .unwrap();
                commands::begin(&db, &command).unwrap();
                let mut outcome = result(code);
                if code == -1 {
                    outcome.exit_code = None;
                    outcome.timed_out = true;
                    outcome.signal = Some("SIGKILL".into());
                }
                commands::save_result(&db, &command, &outcome).unwrap();
                complete(step, json!({}));
                command
            };
            let has_step = |step: &str| {
                engine
                    .instances
                    .list_step_executions(&id)
                    .unwrap()
                    .iter()
                    .any(|execution| execution.step_id == step)
            };
            complete(
                "prepare",
                json!({"baseline":"Pinned baseline", "destination":"/workspace/target", "project_context":"Project conventions"}),
            );
            complete(
                "rubric",
                json!({"requirements":["Tests pass"], "criteria":[{"id":"quality","weight":100}], "tie_break":"Simpler", "brief":"Complete task", "verification_script":"bash checks.sh"}),
            );
            complete(
                "alternative_a",
                json!({"workspace":"/workspace/a","change_set":"Original winning patch","rationale":"A"}),
            );
            complete(
                "alternative_b",
                json!({"workspace":"/workspace/b","change_set":"Other candidate ideas","rationale":"B"}),
            );
            command("check_a", 0);
            command("check_b", 0);
            complete(
                "evaluate",
                json!({"winner":"a","scorecard":[],"rationale":"A wins"}),
            );
            assert!(!has_step("adopt"));
            complete(
                "refinement_review",
                json!({"worth_refining":worth_refining,"improvements":["Use the other candidate's simpler parsing"],"rationale":"Potential quality gain"}),
            );
            if worth_refining {
                complete(
                    "refinement",
                    json!({"before_workspace":"/workspace/unchanged-copy","after_workspace":"/workspace/experimental-copy","change_set":"Improved patch","summary":"Simpler parsing"}),
                );
                let before = command("refinement_before", before_exit);
                assert!(!has_step("adopt"));
                let after = command("refinement_after", after_exit);
                assert_eq!(before.script, "bash checks.sh");
                assert_eq!(after.script, before.script);
                assert_eq!(before.cwd, "/workspace/unchanged-copy");
                assert_eq!(after.cwd, "/workspace/experimental-copy");
                if before_exit == 0 && after_exit == 0 {
                    complete(
                        "refinement_assessment",
                        json!({"requirements_met":requirements_met,"before_score":80,"after_score":after_score,"scorecard":[],"rationale":"Same rubric, observed differences"}),
                    );
                } else {
                    assert!(!has_step("refinement_assessment"));
                }
            } else {
                assert!(!has_step("refinement"));
            }
            assert_eq!(
                engine.instances.get_instance(&id).unwrap().current_flow,
                expected_flow
            );
            if expected_flow == "no_winner" {
                assert!(!has_step("adopt"));
                complete(
                    "explain_no_winner",
                    json!("Staged baseline did not pass; destination unchanged."),
                );
                command("reject_selection", 1);
                assert_eq!(engine.instances.get_instance(&id).unwrap().status, "failed");
                continue;
            }
            let adopted = execution("adopt");
            let prompt = TaskBoard::new(db.clone())
                .get(adopted.task_id.as_deref().unwrap())
                .unwrap()
                .description
                .unwrap();
            if expected_flow == "adopt_refined" {
                assert!(prompt.contains("Improved patch"));
            } else {
                assert!(prompt.contains("Original winning patch"));
                assert!(!prompt.contains("Verified refined change set"));
            }
            complete(
                "adopt",
                json!({"workspace":"/workspace/target","summary":expected_flow}),
            );
            command("verify", 0);
            complete(
                "deliver",
                json!("Completed task with the verified selected version."),
            );
            assert_eq!(
                engine.instances.get_instance(&id).unwrap().status,
                "completed"
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
