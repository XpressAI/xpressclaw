use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::error::{Error, Result};

use super::context;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub trigger: Option<WorkflowTrigger>,
    /// Optional recurring schedule. Manual runs are always available; this
    /// adds an automatic cron trigger without depending on connector channels.
    #[serde(default)]
    pub schedule: Option<WorkflowSchedule>,
    /// Values callers may provide when starting this workflow.
    #[serde(default)]
    pub inputs: HashMap<String, WorkflowInput>,
    #[serde(default)]
    pub variables: HashMap<String, Value>,
    pub flows: HashMap<String, SubWorkflow>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTrigger {
    pub connector: String,
    pub channel: String,
    pub event: String,
    #[serde(default)]
    pub filter: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSchedule {
    /// Standard five-field cron expressions use server-local time.
    pub cron: String,
    /// Input overrides supplied to every scheduled run.
    #[serde(default)]
    pub inputs: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInput {
    #[serde(rename = "type", default)]
    pub input_type: WorkflowInputType,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    /// Marks the agent role controlled by the primary agent picker on New
    /// Work. At most one agent input may be primary.
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowInputType {
    #[default]
    String,
    Number,
    Boolean,
    /// A configured XpressClaw agent/session ID. Agent inputs make a workflow
    /// reusable across project contexts instead of baking concrete IDs into
    /// every task step.
    Agent,
    /// Any JSON value, including objects and arrays.
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubWorkflow {
    #[serde(default)]
    pub color: Option<String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    #[serde(rename = "type", default = "default_step_type")]
    pub step_type: String, // step, sink, when, loop, jump
    #[serde(default)]
    pub label: Option<String>,
    // Step (task) and continue-current-task fields
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    /// Optional harness-native slash command. The rendered prompt is passed
    /// as its argument, so `command: /loop` plus a goal prompt behaves like
    /// the same command in the native product UI.
    #[serde(default)]
    pub command: Option<String>,
    /// ACP configuration values to apply before this step's prompt. Keys are
    /// the opaque IDs advertised by the selected harness (for example mode,
    /// model, or reasoning effort).
    #[serde(default)]
    pub session_config: HashMap<String, Value>,
    /// Optional MCP tool that the selected native harness must invoke during
    /// this step. ACP attaches the server to the native session; the tool call
    /// itself remains part of the agent turn because ACP has no client-side
    /// MCP invocation method.
    #[serde(default)]
    pub mcp_server: Option<String>,
    #[serde(default)]
    pub mcp_tool: Option<String>,
    #[serde(default)]
    pub mcp_arguments: Option<Value>,
    /// Start a clean native conversation for this step. Dependencies still
    /// take precedence so dependent steps can deliberately continue context.
    #[serde(default)]
    pub new_session: bool,
    #[serde(default)]
    pub procedure: Option<String>,
    #[serde(default)]
    pub outputs: Option<HashMap<String, OutputSchema>>,
    // Sink fields
    #[serde(default)]
    pub sinks: Option<Vec<SinkConfig>>,
    // When (conditional) fields
    #[serde(rename = "switch", default)]
    pub switch_var: Option<String>,
    #[serde(default)]
    pub arms: Option<Vec<WhenArm>>,
    // Loop fields
    #[serde(default)]
    pub over: Option<String>,
    #[serde(rename = "as", default)]
    pub as_var: Option<String>,
    #[serde(default, rename = "steps", alias = "body")]
    pub body: Option<Vec<Step>>, // nested steps for loops (YAML key: "steps")
    // Jump fields
    #[serde(default)]
    pub target: Option<String>,
    // Wait fields
    /// Durable event name. The first built-in events are GitHub pull-request
    /// review, comment, and combined activity.
    #[serde(default)]
    pub event: Option<String>,
    /// Event resource, normally a rendered pull-request URL from an earlier
    /// step output.
    #[serde(default)]
    pub resource: Option<String>,
    /// Optional human duration such as `30m`, `24h`, or `14d`.
    #[serde(default)]
    pub timeout: Option<String>,
    /// Optional goto target used when the wait times out. Without one the
    /// workflow fails with a useful timeout error.
    #[serde(default)]
    pub on_timeout: Option<String>,
}

fn default_step_type() -> String {
    "step".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSchema {
    #[serde(rename = "type", default)]
    pub output_type: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkConfig {
    pub connector: String,
    pub channel: String,
    #[serde(default)]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhenArm {
    #[serde(rename = "match", default)]
    pub match_value: Option<String>,
    #[serde(rename = "continue", default)]
    pub continue_flow: Option<bool>,
    #[serde(default)]
    pub goto: Option<String>,
}

impl WorkflowDefinition {
    /// Parse a workflow definition from YAML.
    pub fn parse(yaml: &str) -> Result<Self> {
        let def: WorkflowDefinition = serde_yaml::from_str(yaml)
            .map_err(|e| Error::Workflow(format!("YAML parse error: {e}")))?;
        Ok(def)
    }

    /// Serialize back to YAML.
    pub fn to_yaml(&self) -> Result<String> {
        serde_yaml::to_string(self)
            .map_err(|e| Error::Workflow(format!("YAML serialize error: {e}")))
    }

    /// Whether this definition depends on the connector runtime that is
    /// intentionally disabled for the ACP beta.
    pub fn uses_connector_automation(&self) -> bool {
        self.trigger.is_some()
            || self
                .flows
                .values()
                .any(|flow| Self::steps_use_connectors(&flow.steps))
    }

    /// Whether this workflow contains a step that must be attached to an
    /// existing source task. Such definitions can be saved before they are
    /// marked as defaults, but cannot be started as independent manual runs.
    pub fn continues_source_task(&self) -> bool {
        self.flows
            .values()
            .any(|flow| Self::steps_continue_source_task(&flow.steps))
    }

    fn steps_use_connectors(steps: &[Step]) -> bool {
        steps.iter().any(|step| {
            step.step_type == "sink" || step.body.as_deref().is_some_and(Self::steps_use_connectors)
        })
    }

    fn steps_continue_source_task(steps: &[Step]) -> bool {
        steps.iter().any(|step| {
            step.step_type == "continue"
                || step
                    .body
                    .as_deref()
                    .is_some_and(Self::steps_continue_source_task)
        })
    }

    /// Resolve and freeze every Agent selector known when a run starts.
    ///
    /// Projectless instances must persist the Project provenance of every
    /// future Agent before the instance is inserted. Selectors based on
    /// trigger data, variables, or typed Agent inputs are therefore rendered
    /// against the initial context and replaced with their concrete value.
    /// A selector that depends on a future step output is safe only when the
    /// instance already has an explicit Project scope.
    pub(crate) fn resolve_agent_bindings(
        &mut self,
        initial_context: &Value,
        require_all: bool,
    ) -> Result<(Vec<(String, String)>, bool)> {
        let mut bindings = BTreeSet::new();
        let mut changed = false;
        for (flow_name, flow) in &mut self.flows {
            Self::resolve_step_agent_bindings(
                flow_name,
                &mut flow.steps,
                initial_context,
                require_all,
                &mut bindings,
                &mut changed,
            )?;
        }
        Ok((bindings.into_iter().collect(), changed))
    }

    fn resolve_step_agent_bindings(
        flow_name: &str,
        steps: &mut [Step],
        initial_context: &Value,
        require_all: bool,
        bindings: &mut BTreeSet<(String, String)>,
        changed: &mut bool,
    ) -> Result<()> {
        for step in steps {
            if matches!(step.step_type.as_str(), "step" | "wait") {
                if let Some(configured) = step
                    .agent
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                {
                    let rendered = context::render_template(configured, initial_context);
                    let agent_id = rendered.trim();
                    let unresolved =
                        agent_id.is_empty() || agent_id.starts_with('@') || agent_id.contains("{{");
                    if unresolved {
                        if require_all {
                            return Err(Error::Workflow(format!(
                                "projectless workflow step '{flow_name}.{}' agent binding '{configured}' must resolve when the run starts so Project ownership can be recorded; use a literal Agent, a typed Agent input, or explicit Project scope",
                                step.id
                            )));
                        }
                    } else {
                        if configured != agent_id {
                            step.agent = Some(agent_id.to_string());
                            *changed = true;
                        }
                        let source = format!("step '{flow_name}.{}'", step.id);
                        bindings.insert((source, agent_id.to_string()));
                    }
                }
            }
            if let Some(body) = step.body.as_deref_mut() {
                Self::resolve_step_agent_bindings(
                    flow_name,
                    body,
                    initial_context,
                    require_all,
                    bindings,
                    changed,
                )?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn agent_bindings_for_test(
        &mut self,
        initial_context: &Value,
    ) -> Result<Vec<(String, String)>> {
        self.resolve_agent_bindings(initial_context, true)
            .map(|(bindings, _)| bindings)
    }

    /// Validate the definition:
    /// - at least one flow
    /// - all step IDs unique within each flow
    /// - all goto targets valid (step IDs exist within target flow, flow names exist)
    /// - loops have `over` and `as`
    /// - when has `switch` and `arms`
    pub fn validate(&self) -> Result<()> {
        if self.flows.is_empty() {
            return Err(Error::Workflow(
                "workflow must have at least one flow".into(),
            ));
        }

        let mut primary_agent = None;
        for (name, input) in &self.inputs {
            if name.trim().is_empty() {
                return Err(Error::Workflow(
                    "workflow input names cannot be empty".into(),
                ));
            }
            if !name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                return Err(Error::Workflow(format!(
                    "workflow input name '{name}' may contain only ASCII letters, numbers, and underscores"
                )));
            }
            if name == "trigger" {
                return Err(Error::Workflow(
                    "workflow input name 'trigger' is reserved".into(),
                ));
            }
            if let Some(default) = input.default.as_ref() {
                input.validate_value(name, default)?;
            }
            if input.primary {
                if input.input_type != WorkflowInputType::Agent {
                    return Err(Error::Workflow(format!(
                        "workflow input '{name}' can be primary only when its type is agent"
                    )));
                }
                if let Some(existing) = primary_agent {
                    return Err(Error::Workflow(format!(
                        "workflow agent inputs '{existing}' and '{name}' are both primary; choose one"
                    )));
                }
                primary_agent = Some(name.as_str());
            }
        }

        if let Some(schedule) = self.schedule.as_ref() {
            if self.continues_source_task() {
                return Err(Error::Workflow(
                    "continue steps require a source task and cannot run from a cron schedule"
                        .into(),
                ));
            }
            if schedule.cron.trim().is_empty() {
                return Err(Error::Workflow(
                    "workflow schedule cron cannot be empty".into(),
                ));
            }
            croner::Cron::new(schedule.cron.trim())
                .parse()
                .map_err(|e| Error::Workflow(format!("invalid workflow schedule cron: {e}")))?;
            self.resolve_inputs(&Value::Object(
                schedule
                    .inputs
                    .clone()
                    .into_iter()
                    .collect::<serde_json::Map<String, Value>>(),
            ))?;
        }

        let flow_names: HashSet<&str> = self.flows.keys().map(|k| k.as_str()).collect();

        for (flow_name, sub) in &self.flows {
            // Collect step IDs for this flow and check uniqueness
            let mut step_ids: HashSet<&str> = HashSet::new();
            self.collect_step_ids(&sub.steps, &mut step_ids, flow_name)?;

            // Validate each step
            self.validate_steps(&sub.steps, &step_ids, &flow_names, flow_name)?;
        }

        Ok(())
    }

    /// Validate caller-provided input values, apply defaults, and return the
    /// exact payload made available to templates. Workflows without an input
    /// schema retain the legacy behavior of accepting any JSON payload.
    pub fn resolve_inputs(&self, provided: &Value) -> Result<Value> {
        if self.inputs.is_empty() {
            return Ok(provided.clone());
        }

        let provided = provided
            .as_object()
            .ok_or_else(|| Error::Workflow("workflow inputs must be a JSON object".to_string()))?;

        for name in provided.keys() {
            if !self.inputs.contains_key(name) {
                return Err(Error::Workflow(format!("unknown workflow input '{name}'")));
            }
        }

        let mut resolved = serde_json::Map::new();
        for (name, input) in &self.inputs {
            match provided.get(name).or(input.default.as_ref()) {
                Some(value) => {
                    input.validate_value(name, value)?;
                    resolved.insert(name.clone(), value.clone());
                }
                None if input.required => {
                    return Err(Error::Workflow(format!(
                        "required workflow input '{name}' is missing"
                    )));
                }
                None => {}
            }
        }
        Ok(Value::Object(resolved))
    }

    /// Resolve the inputs available to a workflow that runs automatically for
    /// a task. Default workflows cannot stop to ask for run-time values. Their
    /// primary Agent input, when present, is bound to the task's Agent and all
    /// other required inputs must declare defaults.
    pub fn resolve_default_task_inputs(&self, agent_id: Option<&str>) -> Result<Value> {
        let mut provided = serde_json::Map::new();
        if let Some((name, _)) = self
            .inputs
            .iter()
            .find(|(_, input)| input.input_type == WorkflowInputType::Agent && input.primary)
        {
            if let Some(agent_id) = agent_id.filter(|agent_id| !agent_id.trim().is_empty()) {
                provided.insert(name.clone(), Value::String(agent_id.to_string()));
            }
        }
        self.resolve_inputs(&Value::Object(provided)).map_err(|error| {
            Error::Workflow(format!(
                "default task workflow cannot collect run-time inputs: {error}; add defaults for required inputs"
            ))
        })
    }

    /// Check that every automatic task run can be initialized without a form.
    pub fn validate_default_task_trigger(&self) -> Result<()> {
        const TASK_AGENT_SENTINEL: &str = "__xpressclaw_source_task_agent__";
        if self.inputs.contains_key("source_task") || self.variables.contains_key("source_task") {
            return Err(Error::Workflow(
                "default task workflows reserve 'source_task' for the triggering task metadata"
                    .into(),
            ));
        }
        let inputs = self.resolve_default_task_inputs(Some(TASK_AGENT_SENTINEL))?;
        for (name, input) in &self.inputs {
            if input.input_type == WorkflowInputType::Agent {
                if let Some(agent_id) = inputs.get(name).and_then(Value::as_str) {
                    if agent_id != TASK_AGENT_SENTINEL {
                        return Err(Error::Workflow(format!(
                            "default task workflow agent input '{name}' resolves to Agent '{agent_id}'; use one primary agent input so it follows each source task's Project"
                        )));
                    }
                }
            }
        }
        let initial_context = context::build_context(&inputs, &self.variables, &HashMap::new());
        let mut resolved = self.clone();
        let (bindings, _) = resolved.resolve_agent_bindings(&initial_context, true)?;
        if let Some((source, agent_id)) = bindings
            .iter()
            .find(|(_, agent_id)| agent_id != TASK_AGENT_SENTINEL)
        {
            return Err(Error::Workflow(format!(
                "default task workflow {source} resolves to Agent '{agent_id}'; use a primary agent input so it follows each source task's Project"
            )));
        }
        Ok(())
    }

    /// Recursively collect step IDs, detecting duplicates.
    #[allow(clippy::only_used_in_recursion)]
    fn collect_step_ids<'a>(
        &self,
        steps: &'a [Step],
        ids: &mut HashSet<&'a str>,
        flow_name: &str,
    ) -> Result<()> {
        for step in steps {
            if !ids.insert(&step.id) {
                return Err(Error::Workflow(format!(
                    "duplicate step ID '{}' in flow '{flow_name}'",
                    step.id
                )));
            }
            // Also collect IDs from loop bodies
            if let Some(ref body) = step.body {
                self.collect_step_ids(body, ids, flow_name)?;
            }
        }
        Ok(())
    }

    /// Recursively validate steps.
    fn validate_steps(
        &self,
        steps: &[Step],
        step_ids: &HashSet<&str>,
        flow_names: &HashSet<&str>,
        flow_name: &str,
    ) -> Result<()> {
        for step in steps {
            match step.step_type.as_str() {
                "when" => {
                    if step.switch_var.is_none() {
                        return Err(Error::Workflow(format!(
                            "when step '{}' in flow '{flow_name}' is missing 'switch'",
                            step.id
                        )));
                    }
                    if step.arms.is_none() || step.arms.as_ref().is_some_and(|a| a.is_empty()) {
                        return Err(Error::Workflow(format!(
                            "when step '{}' in flow '{flow_name}' is missing 'arms'",
                            step.id
                        )));
                    }
                    // Validate goto targets in arms
                    if let Some(ref arms) = step.arms {
                        for arm in arms {
                            if let Some(ref goto) = arm.goto {
                                self.validate_goto_target(
                                    goto, step_ids, flow_names, &step.id, flow_name,
                                )?;
                            }
                        }
                    }
                }
                "loop" => {
                    if step.over.is_none() {
                        return Err(Error::Workflow(format!(
                            "loop step '{}' in flow '{flow_name}' is missing 'over'",
                            step.id
                        )));
                    }
                    if step.as_var.is_none() {
                        return Err(Error::Workflow(format!(
                            "loop step '{}' in flow '{flow_name}' is missing 'as'",
                            step.id
                        )));
                    }
                    // Validate nested steps in loop body
                    if let Some(ref body) = step.body {
                        if let Some(unsupported) =
                            body.iter().find(|body_step| body_step.step_type != "step")
                        {
                            return Err(Error::Workflow(format!(
                                "loop step '{}' in flow '{flow_name}' contains unsupported '{}' step '{}'; loop bodies currently accept agent task steps only",
                                step.id, unsupported.step_type, unsupported.id
                            )));
                        }
                        self.validate_steps(body, step_ids, flow_names, flow_name)?;
                    }
                }
                "step" => self.validate_agent_reference(step, flow_name, false)?,
                "continue" => {
                    if step
                        .prompt
                        .as_deref()
                        .map(str::trim)
                        .is_none_or(str::is_empty)
                    {
                        return Err(Error::Workflow(format!(
                            "continue step '{}' in flow '{flow_name}' is missing 'prompt'",
                            step.id
                        )));
                    }
                }
                "wait" => {
                    self.validate_agent_reference(step, flow_name, true)?;
                    let event = step
                        .event
                        .as_deref()
                        .map(str::trim)
                        .filter(|event| !event.is_empty())
                        .ok_or_else(|| {
                            Error::Workflow(format!(
                                "wait step '{}' in flow '{flow_name}' is missing 'event'",
                                step.id
                            ))
                        })?;
                    if !matches!(
                        event,
                        "github.pull_request.review"
                            | "github.pull_request.comment"
                            | "github.pull_request.activity"
                    ) {
                        return Err(Error::Workflow(format!(
                            "wait step '{}' in flow '{flow_name}' uses unsupported event '{event}'",
                            step.id
                        )));
                    }
                    if step
                        .resource
                        .as_deref()
                        .map(str::trim)
                        .is_none_or(str::is_empty)
                    {
                        return Err(Error::Workflow(format!(
                            "wait step '{}' in flow '{flow_name}' is missing 'resource'",
                            step.id
                        )));
                    }
                    if let Some(target) = step.on_timeout.as_deref() {
                        self.validate_goto_target(
                            target, step_ids, flow_names, &step.id, flow_name,
                        )?;
                    }
                    if let Some(timeout) = step.timeout.as_deref() {
                        parse_wait_duration(timeout).map_err(|message| {
                            Error::Workflow(format!(
                                "wait step '{}' in flow '{flow_name}' has invalid timeout: {message}",
                                step.id
                            ))
                        })?;
                    }
                }
                "jump" => {
                    if let Some(ref target) = step.target {
                        self.validate_goto_target(
                            target, step_ids, flow_names, &step.id, flow_name,
                        )?;
                    } else {
                        return Err(Error::Workflow(format!(
                            "jump step '{}' in flow '{flow_name}' is missing 'target'",
                            step.id
                        )));
                    }
                }
                "sink" => {}
                other => {
                    return Err(Error::Workflow(format!(
                        "step '{}' in flow '{flow_name}' has unsupported type '{other}'",
                        step.id
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_agent_reference(&self, step: &Step, flow_name: &str, required: bool) -> Result<()> {
        let agent = step
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|agent| !agent.is_empty());
        let Some(agent) = agent else {
            return if required {
                Err(Error::Workflow(format!(
                    "{} step '{}' in flow '{flow_name}' is missing 'agent'",
                    step.step_type, step.id
                )))
            } else {
                Ok(())
            };
        };
        let Some(name) = agent.strip_prefix('@') else {
            return Ok(());
        };
        if name.contains('.') || name.is_empty() {
            return Err(Error::Workflow(format!(
                "step '{}' in flow '{flow_name}' must reference an agent input as '@role'",
                step.id
            )));
        }
        match self.inputs.get(name) {
            Some(input) if input.input_type == WorkflowInputType::Agent => Ok(()),
            Some(_) => Err(Error::Workflow(format!(
                "step '{}' in flow '{flow_name}' references '@{name}', but that input is not type agent",
                step.id
            ))),
            None => Err(Error::Workflow(format!(
                "step '{}' in flow '{flow_name}' references undeclared agent input '@{name}'",
                step.id
            ))),
        }
    }

    /// Validate a goto/jump target string.
    ///
    /// Valid formats:
    /// - `"step <step_id>"` — must exist in current flow's step_ids
    /// - `"flow <flow_name>"` — must exist in flow_names
    /// - `"flow <flow_name> step <step_id>"` — flow must exist, step must exist in that flow
    /// - `"workflow <name>"` — always accepted (cross-workflow, not validated here)
    fn validate_goto_target(
        &self,
        target: &str,
        current_step_ids: &HashSet<&str>,
        flow_names: &HashSet<&str>,
        from_step_id: &str,
        from_flow: &str,
    ) -> Result<()> {
        let parts: Vec<&str> = target.split_whitespace().collect();
        match parts.as_slice() {
            ["step", step_id] => {
                if !current_step_ids.contains(step_id) {
                    return Err(Error::Workflow(format!(
                        "goto in step '{from_step_id}' (flow '{from_flow}') references unknown step '{step_id}'"
                    )));
                }
            }
            ["flow", fname] => {
                if !flow_names.contains(fname) {
                    return Err(Error::Workflow(format!(
                        "goto in step '{from_step_id}' (flow '{from_flow}') references unknown flow '{fname}'"
                    )));
                }
            }
            ["flow", fname, "step", step_id] => {
                if !flow_names.contains(fname) {
                    return Err(Error::Workflow(format!(
                        "goto in step '{from_step_id}' (flow '{from_flow}') references unknown flow '{fname}'"
                    )));
                }
                // Check that the step exists in the target flow
                if let Some(target_flow) = self.flows.get(*fname) {
                    let target_ids: HashSet<&str> =
                        target_flow.steps.iter().map(|s| s.id.as_str()).collect();
                    if !target_ids.contains(step_id) {
                        return Err(Error::Workflow(format!(
                            "goto in step '{from_step_id}' (flow '{from_flow}') references unknown step '{step_id}' in flow '{fname}'"
                        )));
                    }
                }
            }
            ["workflow", _name] => {
                // Cross-workflow references aren't validated structurally
            }
            _ => {
                return Err(Error::Workflow(format!(
                    "invalid goto target '{target}' in step '{from_step_id}' (flow '{from_flow}')"
                )));
            }
        }
        Ok(())
    }

    /// Get the names of all flows.
    pub fn flow_names(&self) -> Vec<&str> {
        self.flows.keys().map(|k| k.as_str()).collect()
    }

    /// Find a step by flow name and step ID.
    pub fn find_step(&self, flow: &str, step_id: &str) -> Option<&Step> {
        let sub = self.flows.get(flow)?;
        find_step_in_list(&sub.steps, step_id)
    }

    /// Find the index of a step within a flow's top-level steps.
    pub fn step_index(&self, flow: &str, step_id: &str) -> Option<usize> {
        let sub = self.flows.get(flow)?;
        sub.steps.iter().position(|s| s.id == step_id)
    }
}

/// Parse the compact duration syntax used by durable wait steps.
pub(crate) fn parse_wait_duration(value: &str) -> std::result::Result<chrono::Duration, String> {
    let value = value.trim();
    if value.len() < 2 {
        return Err("use a positive duration such as 30m, 24h, or 14d".into());
    }
    let (amount, unit) = value.split_at(value.len() - 1);
    let amount = amount
        .parse::<i64>()
        .map_err(|_| "use a whole number followed by s, m, h, d, or w".to_string())?;
    if amount <= 0 {
        return Err("duration must be greater than zero".into());
    }
    let duration = match unit {
        "s" => chrono::Duration::try_seconds(amount),
        "m" => chrono::Duration::try_minutes(amount),
        "h" => chrono::Duration::try_hours(amount),
        "d" => chrono::Duration::try_days(amount),
        "w" => chrono::Duration::try_weeks(amount),
        _ => return Err("unit must be s, m, h, d, or w".into()),
    }
    .ok_or_else(|| "duration is too large".to_string())?;
    if chrono::Utc::now().checked_add_signed(duration).is_none() {
        return Err("duration exceeds the supported timestamp range".into());
    }
    Ok(duration)
}

impl WorkflowInput {
    fn validate_value(&self, name: &str, value: &Value) -> Result<()> {
        let valid = match self.input_type {
            WorkflowInputType::String => value.is_string(),
            WorkflowInputType::Number => value.is_number(),
            WorkflowInputType::Boolean => value.is_boolean(),
            WorkflowInputType::Agent => {
                value.as_str().is_some_and(|agent| !agent.trim().is_empty())
            }
            WorkflowInputType::Json => true,
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Workflow(format!(
                "workflow input '{name}' must be {}",
                self.input_type.label()
            )))
        }
    }
}

impl WorkflowInputType {
    fn label(self) -> &'static str {
        match self {
            Self::String => "a string",
            Self::Number => "a number",
            Self::Boolean => "a boolean",
            Self::Agent => "a configured agent ID",
            Self::Json => "valid JSON",
        }
    }
}

/// Recursively search for a step by ID in a list (including loop bodies).
fn find_step_in_list<'a>(steps: &'a [Step], step_id: &str) -> Option<&'a Step> {
    for step in steps {
        if step.id == step_id {
            return Some(step);
        }
        if let Some(ref body) = step.body {
            if let Some(found) = find_step_in_list(body, step_id) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_YAML: &str = r##"
name: support-ticket-pipeline
description: "Handle incoming support tickets"
version: 1

trigger:
  connector: telegram
  channel: support-chat
  event: message
  filter:
    type: text

variables:
  default_agent: atlas

flows:
  main:
    color: "#4A90D9"
    steps:
      - id: classify
        label: "Classify Ticket"
        agent: atlas
        prompt: |
          Classify the following support message into one of:
          bug, feature_request, question
          Message: @trigger.payload.text
        outputs:
          intent:
            type: string
            description: "One of: bug, feature_request, question"

      - id: route
        type: when
        switch: "@classify.intent"
        arms:
          - match: bug
            goto: "flow bug_flow"
          - match: feature_request
            goto: "flow feature_flow"
          - match: default
            continue: true

      - id: generic_reply
        type: sink
        sinks:
          - connector: telegram
            channel: support-chat
            template: "Thanks for your question: @trigger.payload.text"

  bug_flow:
    color: "#E74C3C"
    steps:
      - id: investigate
        label: "Investigate Bug"
        agent: atlas
        prompt: "Investigate this bug report: @trigger.payload.text"

      - id: notify_devs
        type: sink
        sinks:
          - connector: telegram
            channel: dev-chat
            template: "Bug found: @investigate.output"

  feature_flow:
    color: "#27AE60"
    steps:
      - id: draft_spec
        label: "Draft Feature Spec"
        agent: atlas
        prompt: "Draft a feature spec for: @trigger.payload.text"

      - id: notify_pm
        type: sink
        sinks:
          - connector: telegram
            channel: pm-chat
            template: "New feature request: @draft_spec.output"
"##;

    #[test]
    fn test_parse_sample_yaml() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        assert_eq!(def.name, "support-ticket-pipeline");
        assert_eq!(def.version, 1);
        assert_eq!(def.flows.len(), 3);
        assert!(def.trigger.is_some());

        let trigger = def.trigger.as_ref().unwrap();
        assert_eq!(trigger.connector, "telegram");
        assert_eq!(trigger.event, "message");

        let main = def.flows.get("main").unwrap();
        assert_eq!(main.steps.len(), 3);
        assert_eq!(main.color.as_deref(), Some("#4A90D9"));

        let classify = &main.steps[0];
        assert_eq!(classify.id, "classify");
        assert_eq!(classify.step_type, "step");
        assert!(classify.outputs.is_some());

        let route = &main.steps[1];
        assert_eq!(route.step_type, "when");
        assert!(route.switch_var.is_some());
        assert_eq!(route.arms.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn test_native_code_review_example_is_valid() {
        let yaml = include_str!("../../../../examples/workflows/implementation-review-loop.yaml");
        let definition = WorkflowDefinition::parse(yaml).unwrap();
        definition.validate().unwrap();
        let main = definition.flows.get("main").unwrap();
        assert_eq!(main.steps[0].agent.as_deref(), Some("@implementer"));
        assert_eq!(main.steps[1].agent.as_deref(), Some("@reviewer"));
        assert_eq!(
            definition
                .inputs
                .get("implementer")
                .map(|input| input.input_type),
            Some(WorkflowInputType::Agent)
        );
        assert_eq!(
            definition
                .inputs
                .get("reviewer")
                .map(|input| input.input_type),
            Some(WorkflowInputType::Agent)
        );
    }

    #[test]
    fn test_validate_valid_definition() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        assert!(def.validate().is_ok());
    }

    #[test]
    fn test_validate_no_flows() {
        let yaml = r#"
name: empty
flows: {}
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        assert!(def.validate().is_err());
    }

    #[test]
    fn test_validate_duplicate_step_id() {
        let yaml = r#"
name: dupes
flows:
  main:
    steps:
      - id: a
        prompt: "do a"
      - id: a
        prompt: "do a again"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate step ID"));
    }

    #[test]
    fn test_validate_goto_nonexistent_step() {
        let yaml = r#"
name: bad-goto
flows:
  main:
    steps:
      - id: check
        type: when
        switch: "@check.result"
        arms:
          - match: "yes"
            goto: "step nonexistent"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn test_validate_goto_nonexistent_flow() {
        let yaml = r#"
name: bad-goto-flow
flows:
  main:
    steps:
      - id: check
        type: when
        switch: "@check.result"
        arms:
          - match: "yes"
            goto: "flow no_such_flow"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("no_such_flow"));
    }

    #[test]
    fn test_validate_when_without_switch() {
        let yaml = r#"
name: bad-when
flows:
  main:
    steps:
      - id: route
        type: when
        arms:
          - match: "yes"
            continue: true
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("missing 'switch'"));
    }

    #[test]
    fn test_validate_when_without_arms() {
        let yaml = r#"
name: bad-when
flows:
  main:
    steps:
      - id: route
        type: when
        switch: "@x.y"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("missing 'arms'"));
    }

    #[test]
    fn test_validate_loop_without_over() {
        let yaml = r#"
name: bad-loop
flows:
  main:
    steps:
      - id: loop1
        type: loop
        as: item
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("missing 'over'"));
    }

    #[test]
    fn test_validate_loop_without_as() {
        let yaml = r#"
name: bad-loop
flows:
  main:
    steps:
      - id: loop1
        type: loop
        over: "@items.list"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("missing 'as'"));
    }

    #[test]
    fn test_validate_jump_without_target() {
        let yaml = r#"
name: bad-jump
flows:
  main:
    steps:
      - id: j1
        type: jump
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("missing 'target'"));
    }

    #[test]
    fn test_to_yaml_roundtrip() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        let yaml_out = def.to_yaml().unwrap();
        let def2 = WorkflowDefinition::parse(&yaml_out).unwrap();
        assert_eq!(def.name, def2.name);
        assert_eq!(def.flows.len(), def2.flows.len());
        for (name, flow) in &def.flows {
            let flow2 = def2.flows.get(name).unwrap();
            assert_eq!(flow.steps.len(), flow2.steps.len());
        }
    }

    #[test]
    fn test_default_version() {
        let yaml = r#"
name: minimal
flows:
  main:
    steps:
      - id: a
        prompt: "do a"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        assert_eq!(def.version, 1);
    }

    #[test]
    fn test_flow_names() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        let names = def.flow_names();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"main"));
        assert!(names.contains(&"bug_flow"));
        assert!(names.contains(&"feature_flow"));
    }

    #[test]
    fn test_find_step() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        let step = def.find_step("main", "classify").unwrap();
        assert_eq!(step.label.as_deref(), Some("Classify Ticket"));

        assert!(def.find_step("main", "nonexistent").is_none());
        assert!(def.find_step("nonexistent_flow", "classify").is_none());
    }

    #[test]
    fn test_step_index() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        assert_eq!(def.step_index("main", "classify"), Some(0));
        assert_eq!(def.step_index("main", "route"), Some(1));
        assert_eq!(def.step_index("main", "generic_reply"), Some(2));
        assert_eq!(def.step_index("main", "nonexistent"), None);
    }

    #[test]
    fn test_sink_step() {
        let def = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        let step = def.find_step("main", "generic_reply").unwrap();
        assert_eq!(step.step_type, "sink");
        let sinks = step.sinks.as_ref().unwrap();
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].connector, "telegram");
    }

    #[test]
    fn detects_connector_automation_in_triggers_and_nested_sinks() {
        let triggered = WorkflowDefinition::parse(SAMPLE_YAML).unwrap();
        assert!(triggered.uses_connector_automation());

        let nested_sink = WorkflowDefinition::parse(
            r#"
name: nested-sink
flows:
  main:
    steps:
      - id: iterate
        type: loop
        over: "{{items}}"
        as: item
        steps:
          - id: notify
            type: sink
            sinks:
              - connector: webhook
                channel: results
"#,
        )
        .unwrap();
        assert!(nested_sink.uses_connector_automation());

        let native_only = WorkflowDefinition::parse(
            r#"
name: native-only
flows:
  main:
    steps:
      - id: implement
        type: step
        agent: codex
        prompt: Implement the change.
"#,
        )
        .unwrap();
        assert!(!native_only.uses_connector_automation());
    }

    #[test]
    fn test_loop_step() {
        let yaml = r#"
name: loop-test
flows:
  main:
    steps:
      - id: fetch
        prompt: "Fetch items"
      - id: process_each
        type: loop
        over: "@fetch.items"
        as: item
        body:
          - id: handle
            prompt: "Handle @item"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        def.validate().unwrap();
        let loop_step = def.find_step("main", "process_each").unwrap();
        assert_eq!(loop_step.step_type, "loop");
        assert_eq!(loop_step.over.as_deref(), Some("@fetch.items"));
        assert_eq!(loop_step.as_var.as_deref(), Some("item"));
        assert!(loop_step.body.is_some());
        assert_eq!(loop_step.body.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_jump_step() {
        let yaml = r#"
name: jump-test
flows:
  main:
    steps:
      - id: start
        prompt: "Start"
      - id: go_to_other
        type: jump
        target: "flow other"
  other:
    steps:
      - id: finish
        prompt: "Finish"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        def.validate().unwrap();
        let jump = def.find_step("main", "go_to_other").unwrap();
        assert_eq!(jump.step_type, "jump");
        assert_eq!(jump.target.as_deref(), Some("flow other"));
    }

    #[test]
    fn test_validate_cross_flow_step_reference() {
        let yaml = r#"
name: cross-ref
flows:
  main:
    steps:
      - id: check
        type: when
        switch: "@check.val"
        arms:
          - match: "go"
            goto: "flow other step finish"
  other:
    steps:
      - id: finish
        prompt: "Done"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        assert!(def.validate().is_ok());
    }

    #[test]
    fn test_validate_cross_flow_bad_step_reference() {
        let yaml = r#"
name: cross-ref-bad
flows:
  main:
    steps:
      - id: check
        type: when
        switch: "@check.val"
        arms:
          - match: "go"
            goto: "flow other step nonexistent"
  other:
    steps:
      - id: finish
        prompt: "Done"
"#;
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let err = def.validate().unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn resolves_typed_inputs_with_defaults_and_overrides() {
        let definition = WorkflowDefinition::parse(
            r#"
name: typed-inputs
inputs:
  goal:
    type: string
    required: true
  retries:
    type: number
    default: 2
  publish:
    type: boolean
    default: false
flows:
  main:
    steps:
      - id: work
        prompt: "@goal"
"#,
        )
        .unwrap();
        definition.validate().unwrap();

        let resolved = definition
            .resolve_inputs(&serde_json::json!({"goal": "Ship it", "retries": 4}))
            .unwrap();
        assert_eq!(
            resolved,
            serde_json::json!({"goal": "Ship it", "retries": 4, "publish": false})
        );
    }

    #[test]
    fn rejects_missing_unknown_and_mistyped_inputs() {
        let definition = WorkflowDefinition::parse(
            r#"
name: typed-inputs
inputs:
  goal:
    type: string
    required: true
flows:
  main:
    steps:
      - id: work
        prompt: "@goal"
"#,
        )
        .unwrap();

        assert!(definition
            .resolve_inputs(&serde_json::json!({}))
            .unwrap_err()
            .to_string()
            .contains("required"));
        assert!(definition
            .resolve_inputs(&serde_json::json!({"goal": "ok", "typo": true}))
            .unwrap_err()
            .to_string()
            .contains("unknown"));
        assert!(definition
            .resolve_inputs(&serde_json::json!({"goal": 42}))
            .unwrap_err()
            .to_string()
            .contains("string"));

        for name in ["release-goal", "release.goal"] {
            let invalid_name = WorkflowDefinition::parse(&format!(
                r#"
name: invalid-input-name
inputs:
  {name}:
    type: string
flows:
  main:
    steps:
      - id: work
        prompt: "@{name}"
"#,
            ))
            .unwrap();
            assert!(invalid_name
                .validate()
                .unwrap_err()
                .to_string()
                .contains("letters, numbers, and underscores"));
        }
    }

    #[test]
    fn validates_scheduled_inputs_and_cron() {
        let valid = WorkflowDefinition::parse(
            r#"
name: scheduled
inputs:
  topic:
    type: string
    required: true
schedule:
  cron: "0 9 * * 1"
  inputs:
    topic: weekly-report
flows:
  main:
    steps:
      - id: report
        prompt: "@topic"
"#,
        )
        .unwrap();
        valid.validate().unwrap();
        assert!(!valid.uses_connector_automation());

        let missing = WorkflowDefinition::parse(
            r#"
name: scheduled
inputs:
  topic:
    type: string
    required: true
schedule:
  cron: "0 9 * * 1"
flows:
  main:
    steps:
      - id: report
        prompt: "@topic"
"#,
        )
        .unwrap();
        assert!(missing
            .validate()
            .unwrap_err()
            .to_string()
            .contains("topic"));

        let invalid_cron = WorkflowDefinition::parse(
            r#"
name: scheduled
schedule:
  cron: definitely-not-cron
flows:
  main:
    steps:
      - id: report
        prompt: report
"#,
        )
        .unwrap();
        assert!(invalid_cron
            .validate()
            .unwrap_err()
            .to_string()
            .contains("cron"));
    }

    #[test]
    fn validates_reusable_agent_inputs_and_primary_role() {
        let definition = WorkflowDefinition::parse(
            r#"
name: reusable-review
inputs:
  implementer:
    type: agent
    required: true
    primary: true
  reviewer:
    type: agent
    required: true
flows:
  main:
    steps:
      - id: implement
        agent: "@implementer"
        prompt: implement
      - id: review
        agent: "@reviewer"
        prompt: review
"#,
        )
        .unwrap();
        definition.validate().unwrap();
        assert!(definition
            .resolve_inputs(&serde_json::json!({
                "implementer": "project-a",
                "reviewer": "project-b"
            }))
            .is_ok());

        let invalid = WorkflowDefinition::parse(
            r#"
name: invalid-role
inputs:
  goal:
    type: string
flows:
  main:
    steps:
      - id: work
        agent: "@goal"
        prompt: work
"#,
        )
        .unwrap();
        assert!(invalid
            .validate()
            .unwrap_err()
            .to_string()
            .contains("not type agent"));
    }

    #[test]
    fn resolves_and_freezes_agent_bindings_across_flows_and_loop_bodies() {
        let mut definition = WorkflowDefinition::parse(
            r#"
name: agent-bindings
inputs:
  worker: { type: agent }
flows:
  main:
    steps:
      - id: dynamic
        agent: "@worker"
        prompt: dynamic
      - id: templated
        agent: "{{trigger.payload.templated_worker}}"
        prompt: templated
      - id: repeated
        type: loop
        over: "{{trigger.items}}"
        as: item
        steps:
          - id: nested
            agent: nested-agent
            prompt: nested
  later:
    steps:
      - id: wait
        type: wait
        agent: review-agent
        event: github.pull_request.activity
        resource: https://github.com/example/repo/pull/1
"#,
        )
        .unwrap();

        let context = context::build_context(
            &serde_json::json!({
                "worker": "dynamic-agent",
                "templated_worker": "templated-agent",
            }),
            &definition.variables,
            &HashMap::new(),
        );

        assert_eq!(
            definition.agent_bindings_for_test(&context).unwrap(),
            vec![
                ("step 'later.wait'".into(), "review-agent".into()),
                ("step 'main.dynamic'".into(), "dynamic-agent".into()),
                ("step 'main.nested'".into(), "nested-agent".into()),
                ("step 'main.templated'".into(), "templated-agent".into()),
            ]
        );
        assert_eq!(
            definition.flows["main"].steps[0].agent.as_deref(),
            Some("dynamic-agent")
        );
        assert_eq!(
            definition.flows["main"].steps[1].agent.as_deref(),
            Some("templated-agent")
        );
    }

    #[test]
    fn projectless_agent_bindings_cannot_depend_on_future_step_output() {
        let mut definition = WorkflowDefinition::parse(
            r#"
name: deferred-agent
flows:
  main:
    steps:
      - id: choose
        agent: atlas
        prompt: Choose an Agent
      - id: work
        agent: "{{choose.agent_id}}"
        prompt: Do the work
"#,
        )
        .unwrap();
        let context = context::build_context(
            &serde_json::json!({}),
            &definition.variables,
            &HashMap::new(),
        );

        let error = definition.agent_bindings_for_test(&context).unwrap_err();
        assert!(error
            .to_string()
            .contains("must resolve when the run starts"));
    }

    #[test]
    fn validates_durable_pull_request_waits() {
        let definition = WorkflowDefinition::parse(
            r#"
name: review-wait
inputs:
  implementer:
    type: agent
    required: true
flows:
  main:
    steps:
      - id: wait_for_review
        type: wait
        agent: "@implementer"
        event: github.pull_request.activity
        resource: "@publish.pull_request_url"
        timeout: 14d
        on_timeout: flow timed_out
  timed_out:
    steps: []
"#,
        )
        .unwrap();
        definition.validate().unwrap();
        assert_eq!(parse_wait_duration("90m").unwrap().num_minutes(), 90);

        let mut invalid = definition.clone();
        invalid.flows.get_mut("main").unwrap().steps[0].timeout = Some("eventually".into());
        assert!(invalid
            .validate()
            .unwrap_err()
            .to_string()
            .contains("invalid timeout"));
    }

    #[test]
    fn rejects_overflowing_wait_durations_without_panicking() {
        let constructor_overflow =
            std::panic::catch_unwind(|| parse_wait_duration("9223372036854775807w"));
        assert!(constructor_overflow.is_ok());
        assert!(constructor_overflow
            .unwrap()
            .unwrap_err()
            .contains("too large"));

        let timestamp_overflow = parse_wait_duration("1000000000d").unwrap_err();
        assert!(timestamp_overflow.contains("timestamp range"));

        let definition = WorkflowDefinition::parse(
            r#"
name: invalid-review-wait
flows:
  main:
    steps:
      - id: wait_for_review
        type: wait
        agent: project-a
        event: github.pull_request.activity
        resource: https://github.com/XpressAI/xpressclaw/pull/144
        timeout: 9223372036854775807w
"#,
        )
        .unwrap();
        let validation = std::panic::catch_unwind(|| definition.validate());
        assert!(validation.is_ok());
        assert!(validation
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("invalid timeout"));
    }

    #[test]
    fn validates_fixed_current_task_prompts() {
        let definition = WorkflowDefinition::parse(
            r#"
name: final-ui-check
flows:
  main:
    steps:
      - id: review_ui
        type: continue
        prompt: Ensure the UI contains no messages unnecessary to the end user.
"#,
        )
        .unwrap();
        definition.validate().unwrap();

        let missing_prompt = WorkflowDefinition::parse(
            r#"
name: invalid-final-check
flows:
  main:
    steps:
      - id: review_ui
        type: continue
"#,
        )
        .unwrap();
        assert!(missing_prompt
            .validate()
            .unwrap_err()
            .to_string()
            .contains("missing 'prompt'"));

        let scheduled = WorkflowDefinition::parse(
            r#"
name: invalid-scheduled-final-check
schedule:
  cron: "0 0 * * *"
flows:
  main:
    steps:
      - id: review_ui
        type: continue
        prompt: Review the completed task.
"#,
        )
        .unwrap();
        assert!(scheduled
            .validate()
            .unwrap_err()
            .to_string()
            .contains("cannot run from a cron schedule"));
    }

    #[test]
    fn default_task_inputs_bind_the_primary_agent_and_require_other_defaults() {
        let definition = WorkflowDefinition::parse(
            r#"
name: default-policy
inputs:
  worker: { type: agent, required: true, primary: true }
  focus: { type: string, required: true, default: user-facing UI }
flows:
  main:
    steps:
      - id: check
        agent: "@worker"
        prompt: Check @focus
"#,
        )
        .unwrap();
        assert_eq!(
            definition
                .resolve_default_task_inputs(Some("atlas"))
                .unwrap(),
            serde_json::json!({"worker": "atlas", "focus": "user-facing UI"})
        );

        let missing = WorkflowDefinition::parse(
            r#"
name: invalid-default-policy
inputs:
  focus: { type: string, required: true }
flows:
  main:
    steps: []
"#,
        )
        .unwrap();
        assert!(missing
            .validate_default_task_trigger()
            .unwrap_err()
            .to_string()
            .contains("cannot collect run-time inputs"));

        let fixed_agent = WorkflowDefinition::parse(
            r#"
name: invalid-fixed-agent-policy
flows:
  main:
    steps:
      - id: check
        agent: atlas
        prompt: Check every task
"#,
        )
        .unwrap();
        assert!(fixed_agent
            .validate_default_task_trigger()
            .unwrap_err()
            .to_string()
            .contains("use a primary agent input"));

        let reserved_source_task = WorkflowDefinition::parse(
            r#"
name: invalid-source-task-variable
variables:
  source_task: stale value
flows:
  main:
    steps: []
"#,
        )
        .unwrap();
        assert!(reserved_source_task
            .validate_default_task_trigger()
            .unwrap_err()
            .to_string()
            .contains("reserve 'source_task'"));
    }
}
