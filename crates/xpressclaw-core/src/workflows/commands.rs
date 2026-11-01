//! Command steps are read from the run snapshot, never from editable task text.
use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    context,
    definition::{parse_wait_duration, validate_command_cwd, WorkflowDefinition},
    instance::InstanceManager,
};
use crate::{
    db::Database,
    error::{Error, Result},
};

pub(crate) struct WorkflowCommand {
    pub execution_id: String,
    pub script: String,
    pub cwd: String,
    pub timeout_secs: u64,
    pub allow_failure: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CommandResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub signal: Option<String>,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl CommandResult {
    pub fn passed(&self) -> bool {
        !self.timed_out && self.signal.is_none() && self.exit_code == Some(0)
    }

    pub fn succeeds(&self, allow_failure: bool) -> bool {
        self.passed()
            || (allow_failure
                && (self.exit_code.is_some() || self.signal.is_some() || self.timed_out))
    }

    pub fn json_output(&self) -> Result<String> {
        let mut output =
            serde_json::to_value(self).map_err(|error| Error::Workflow(error.to_string()))?;
        output["passed"] = Value::Bool(self.passed());
        Ok(output.to_string())
    }
}

pub(crate) fn for_task(db: &Arc<Database>, task_id: &str) -> Result<Option<WorkflowCommand>> {
    let instances = InstanceManager::new(db.clone());
    let Some(execution) = instances.find_execution_by_task(task_id)? else {
        return Ok(None);
    };
    let instance = instances.get_instance(&execution.instance_id)?;
    let Some(snapshot) = instance.definition_yaml.as_deref() else {
        return Ok(None);
    };
    let definition = WorkflowDefinition::parse(snapshot)?;
    let Some(step) = definition
        .find_step(&execution.flow_name, &execution.step_id)
        .filter(|step| step.step_type == "command")
    else {
        return Ok(None);
    };
    if instance.status != "running" || execution.status != "running" {
        return Err(Error::Workflow("command step is no longer active".into()));
    }
    let input: Value = serde_json::from_str(execution.input_context.as_deref().unwrap_or("{}"))
        .map_err(|error| Error::Workflow(error.to_string()))?;
    let expected_agent =
        context::render_template(step.agent.as_deref().unwrap_or_default(), &input);
    let assigned = crate::tasks::board::TaskBoard::new(db.clone())
        .get(task_id)?
        .agent_id;
    if assigned.as_deref() != Some(expected_agent.as_str()) {
        return Err(Error::Workflow(
            "command Agent differs from the workflow's frozen binding".into(),
        ));
    }
    let script = context::render_template(step.command.as_deref().unwrap_or_default(), &input);
    let cwd = context::render_template(step.cwd.as_deref().unwrap_or("."), &input);
    validate_command_cwd(&cwd)?;
    if script.trim().is_empty() {
        return Err(Error::Workflow("command rendered an empty script".into()));
    }
    let timeout_secs = parse_wait_duration(step.timeout.as_deref().unwrap_or("5m"))
        .map_err(Error::Workflow)?
        .num_seconds() as u64;
    Ok(Some(WorkflowCommand {
        execution_id: execution.id,
        script,
        cwd,
        timeout_secs,
        allow_failure: step.allow_failure,
    }))
}

/// A result committed before a restart can be delivered without running again.
/// An interrupted command is failed, because replay could repeat side effects.
pub(crate) fn begin(db: &Database, command: &WorkflowCommand) -> Result<Option<CommandResult>> {
    db.with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        let (started, result): (Option<String>, Option<String>) = tx.query_row(
            "SELECT e.command_started_at, e.command_result FROM workflow_step_executions e
             JOIN workflow_instances i ON i.id = e.instance_id
             WHERE e.id = ?1 AND e.status = 'running' AND i.status = 'running'",
            [&command.execution_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?.ok_or_else(|| Error::Workflow("command step is no longer active".into()))?;
        if let Some(result) = result {
            return serde_json::from_str(&result).map(Some).map_err(|error| Error::Workflow(error.to_string()));
        }
        if started.is_some() {
            return Err(Error::Workflow("command was interrupted before its result was saved; start a new workflow run to execute it again".into()));
        }
        tx.execute("UPDATE workflow_step_executions SET command_started_at = CURRENT_TIMESTAMP WHERE id = ?1", [&command.execution_id])?;
        tx.commit()?;
        Ok(None)
    })
}

pub(crate) fn save_result(
    db: &Database,
    command: &WorkflowCommand,
    result: &CommandResult,
) -> Result<()> {
    let output = result.json_output()?;
    db.with_conn(|conn| conn.execute(
        "UPDATE workflow_step_executions SET command_result = ?1 WHERE id = ?2 AND command_result IS NULL",
        rusqlite::params![output, command.execution_id],
    ))?;
    Ok(())
}
