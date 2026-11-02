//! Manager-created workflows are owned by a Project. Scheduling a template
//! creates an independent copy, leaving its other users' inputs and timing alone.
use super::*;
use crate::projects::agent_project;
use crate::workflows::{
    context,
    definition::{WorkflowInputType, WorkflowSchedule},
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};
use std::collections::HashMap;

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectWorkflowInput {
    pub yaml_content: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduledWorkflowInput {
    pub workflow_id: String,
    pub name: Option<String>,
    #[serde(default)]
    pub inputs: HashMap<String, Value>,
    pub run_at: Option<String>,
    pub cron: Option<String>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn read_workflow(conn: &Connection, id: &str) -> Result<WorkflowRecord> {
    conn.query_row("SELECT * FROM workflows WHERE id=?1", [id], |row| {
        Ok(row_to_workflow(row))
    })
    .optional()?
    .ok_or_else(|| Error::WorkflowNotFound { id: id.into() })
}

fn accessible(record: &WorkflowRecord, project: &str) -> Result<()> {
    if record
        .project_id
        .as_deref()
        .is_some_and(|owner| owner != project)
    {
        return Err(Error::ProjectAccess(
            "Workflow was not found in the current Project".into(),
        ));
    }
    Ok(())
}

fn validate_project_definition(
    conn: &Connection,
    project: &str,
    definition: &WorkflowDefinition,
) -> Result<()> {
    definition.validate()?;
    if definition.name.trim().is_empty() {
        return Err(Error::Workflow("Workflow name must not be empty".into()));
    }
    if definition.uses_connector_automation() {
        return Err(Error::Workflow(
            "Connector triggers and notification sinks are unavailable".into(),
        ));
    }
    let inputs = if let Some(schedule) = &definition.schedule {
        definition.resolve_inputs(&json!(schedule.inputs))?
    } else {
        Value::Object(
            definition
                .inputs
                .iter()
                .filter_map(|(name, input)| {
                    input.default.clone().map(|value| (name.clone(), value))
                })
                .collect(),
        )
    };
    let initial = context::build_context(&inputs, &definition.variables, &HashMap::new());
    let mut inspected = definition.clone();
    let (mut bindings, _) = inspected.resolve_agent_bindings(&initial, false)?;
    for (name, input) in &definition.inputs {
        if input.input_type == WorkflowInputType::Agent {
            if let Some(agent) = inputs.get(name).and_then(Value::as_str) {
                bindings.push((name.clone(), agent.into()));
            }
        }
    }
    for (_, agent) in bindings {
        if !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM agents WHERE id=?1 AND project_id=?2)",
            params![agent, project],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(Error::ProjectAccess("Workflow Agent was not found in the current Project; use list_project_agents for its ID".into()));
        }
    }
    Ok(())
}

fn insert_project_workflow(
    tx: &Transaction<'_>,
    project: &str,
    definition: &WorkflowDefinition,
    enabled: bool,
) -> Result<WorkflowRecord> {
    validate_project_definition(tx, project, definition)?;
    let id = Uuid::new_v4().to_string();
    // Workflow names are globally unique in existing installations. Copies
    // receive a readable suffix while keeping their independent identity.
    let mut definition = definition.clone();
    let name = definition.name.clone();
    let mut suffix = 2;
    while tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflows WHERE name=?1)",
        [&definition.name],
        |row| row.get::<_, bool>(0),
    )? {
        definition.name = format!("{name} ({suffix})");
        suffix += 1;
    }
    tx.execute(
        "INSERT INTO workflows(id,name,description,yaml_content,enabled,version,project_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            id,
            definition.name,
            definition.description,
            definition.to_yaml()?,
            enabled,
            definition.version,
            project
        ],
    )?;
    read_workflow(tx, &id)
}

fn validate_batch_size(size: usize) -> Result<()> {
    if !(1..=100).contains(&size) {
        return Err(Error::Workflow(
            "Create between 1 and 100 workflows per batch".into(),
        ));
    }
    Ok(())
}

impl WorkflowManager {
    pub fn list_for_agent(
        &self,
        agent: &str,
        limit: i64,
        offset: i64,
        include_templates: bool,
    ) -> Result<Value> {
        self.db.with_conn(|conn| {
            let project = agent_project(conn, agent)?;
            let predicate = "(project_id=?1 OR (?2 AND project_id IS NULL))";
            let total: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM workflows WHERE {predicate}"),
                params![project, include_templates], |row| row.get(0))?;
            let mut statement = conn.prepare(&format!(
                "SELECT * FROM workflows WHERE {predicate} ORDER BY created_at DESC,id LIMIT ?3 OFFSET ?4"
            ))?;
            let records = statement.query_map(params![project, include_templates, limit.clamp(1,200), offset.max(0)],
                |row| Ok(row_to_workflow(row)))?.collect::<std::result::Result<Vec<_>, _>>()?;
            let workflows = records.into_iter().map(|record| {
                let definition = WorkflowDefinition::parse(&record.yaml_content).ok();
                json!({
                    "id":record.id,"name":record.name,"description":record.description,
                    "project_id":record.project_id,"enabled":record.enabled,"revision":record.revision,
                    "inputs":definition.as_ref().map(|d| &d.inputs),
                    "schedule":definition.as_ref().and_then(|d| d.schedule.as_ref()),
                    "last_triggered_at":record.last_triggered_at,"trigger_count":record.trigger_count,
                    "trigger_error":record.trigger_error,
                })
            }).collect::<Vec<_>>();
            Ok(json!({"workflows":workflows,"total":total,"project_id":project}))
        })
    }

    pub fn get_for_agent(&self, id: &str, agent: &str) -> Result<WorkflowRecord> {
        self.db.with_conn(|conn| {
            let project = agent_project(conn, agent)?;
            let record = read_workflow(conn, id)?;
            accessible(&record, &project)?;
            Ok(record)
        })
    }

    pub fn create_for_agent(
        &self,
        agent: &str,
        inputs: &[ProjectWorkflowInput],
    ) -> Result<Vec<WorkflowRecord>> {
        validate_batch_size(inputs.len())?;
        self.db.with_conn(|conn| {
            let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
            let project = agent_project(&tx, agent)?;
            let records = inputs
                .iter()
                .map(|input| {
                    let definition = WorkflowDefinition::parse(&input.yaml_content)?;
                    insert_project_workflow(&tx, &project, &definition, input.enabled)
                })
                .collect::<Result<Vec<_>>>()?;
            tx.commit()?;
            Ok(records)
        })
    }

    pub fn schedule_for_agent(
        &self,
        agent: &str,
        inputs: &[ScheduledWorkflowInput],
    ) -> Result<Vec<WorkflowRecord>> {
        validate_batch_size(inputs.len())?;
        self.db.with_conn(|conn| {
            let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
            let project = agent_project(&tx, agent)?;
            let records = inputs
                .iter()
                .map(|input| {
                    let source = read_workflow(&tx, &input.workflow_id)?;
                    accessible(&source, &project)?;
                    let mut definition = WorkflowDefinition::parse(&source.yaml_content)?;
                    definition.name = input
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("{} (scheduled)", source.name));
                    definition.schedule = Some(WorkflowSchedule {
                        cron: input.cron.clone().unwrap_or_default(),
                        run_at: input.run_at.clone(),
                        inputs: input.inputs.clone(),
                    });
                    insert_project_workflow(&tx, &project, &definition, input.enabled)
                })
                .collect::<Result<Vec<_>>>()?;
            tx.commit()?;
            Ok(records)
        })
    }

    pub fn update_for_agent(
        &self,
        id: &str,
        agent: &str,
        revision: i64,
        yaml: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<WorkflowRecord> {
        self.db.with_conn(|conn| {
            let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
            let project = agent_project(&tx, agent)?;
            let previous = read_workflow(&tx, id)?;
            if previous.project_id.as_deref() != Some(&project) {
                return Err(Error::ProjectAccess("Only workflows owned by the current Project can be edited; schedule a copy of a shared template".into()));
            }
            if previous.revision != revision {
                return Err(Error::Workflow("Workflow changed; reload its revision before editing".into()));
            }
            let definition = WorkflowDefinition::parse(yaml.unwrap_or(&previous.yaml_content))?;
            if yaml.is_some() || enabled != Some(false) {
                validate_project_definition(&tx, &project, &definition)?;
                if previous.default_for_tasks { definition.validate_default_task_trigger()?; }
            }
            let changed = definition.schedule != WorkflowDefinition::parse(&previous.yaml_content)?.schedule;
            tx.execute(
                "UPDATE workflows SET name=?1,description=?2,yaml_content=?3,version=?4,enabled=?5,
                 revision=revision+1,updated_at=CURRENT_TIMESTAMP,
                 last_triggered_at=CASE WHEN ?6 THEN NULL ELSE last_triggered_at END,
                 trigger_error=NULL WHERE id=?7",
                params![definition.name, definition.description, definition.to_yaml()?, definition.version,
                    enabled.unwrap_or(previous.enabled), changed, id],
            )?;
            let updated = read_workflow(&tx, id)?;
            tx.commit()?;
            Ok(updated)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::registry::AgentRegistry;
    fn setup() -> (Arc<Database>, WorkflowManager, WorkflowRecord) {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db.clone());
        for agent in ["manager", "worker", "outside"] {
            registry.ensure(agent, "codex").unwrap();
        }
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET project_id='manager' WHERE id='worker'",
                [],
            )
        })
        .unwrap();
        let manager = WorkflowManager::new(db.clone());
        let template = manager.create(&CreateWorkflow {
            name:"Audit".into(),description:None,yaml_content:
                "name: Audit\ninputs:\n  worker: {type: agent, required: true}\n  goal: {type: string, required: true}\nflows:\n  main:\n    steps:\n      - id: audit\n        agent: '@worker'\n        prompt: '@goal'\n".into(),
        }).unwrap();
        (db, manager, template)
    }
    fn schedules(value: Value) -> Vec<ScheduledWorkflowInput> {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn manager_schedules_many_independent_project_copies_without_changing_template() {
        let (_, manager, template) = setup();
        let requests = (0..100)
            .map(|index| {
                json!({
                    "workflow_id":template.id,"name":format!("Milestone {index}"),
                    "inputs":{"worker":"worker","goal":format!("Audit milestone {index}")},
                    "run_at":"2099-06-01T09:00:00+09:00","enabled":index!=0,
                })
            })
            .collect::<Vec<_>>();
        let copies = manager
            .schedule_for_agent("manager", &schedules(json!(requests)))
            .unwrap();
        assert_eq!(copies.len(), 100);
        assert!(copies
            .iter()
            .all(|copy| copy.project_id.as_deref() == Some("manager") && copy.id != template.id));
        assert!(!copies[0].enabled);
        assert!(copies[1].enabled);
        assert_eq!(
            manager.get(&template.id).unwrap().yaml_content,
            template.yaml_content
        );
        let page = manager.list_for_agent("manager", 10, 10, false).unwrap();
        assert_eq!(page["total"], 100);
        assert_eq!(page["workflows"].as_array().unwrap().len(), 10);
        assert_eq!(
            manager.list_for_agent("outside", 200, 0, false).unwrap()["total"],
            0
        );
        assert!(manager.get_for_agent(&copies[0].id, "outside").is_err());
    }
    #[test]
    fn invalid_workflow_batches_leave_no_partial_schedule() {
        let (_, manager, template) = setup();
        let valid = json!({"workflow_id":template.id,"inputs":{"worker":"worker","goal":"Audit"},"cron":"0 9 * * 1"});
        for update in [
            json!({"inputs":{"worker":"outside","goal":"Audit"}}),
            json!({"inputs":{"worker":"worker"}}),
            json!({"cron":"invalid"}),
            json!({"run_at":"2099-01-01T00:00:00Z"}),
            json!({"workflow_id":"missing"}),
            json!({"name":"  "}),
        ] {
            let mut invalid = valid.clone();
            for (key, value) in update.as_object().unwrap() {
                invalid[key] = value.clone();
            }
            assert!(manager
                .schedule_for_agent("manager", &schedules(json!([valid, invalid])))
                .is_err());
            assert_eq!(manager.list().unwrap().len(), 1);
        }
        assert!(manager.schedule_for_agent("manager", &[]).is_err());
    }
    #[test]
    fn workflows_can_be_authored_paused_rescheduled_and_revision_checked() {
        let (_, manager, template) = setup();
        let created=manager.create_for_agent("manager",&[
            ProjectWorkflowInput {yaml_content:"name: Research\nflows:\n  main:\n    steps:\n      - id: read\n        agent: worker\n        prompt: Read the repository\n".into(),enabled:false},
        ]).unwrap().remove(0);
        assert!(!created.enabled);
        let mut definition = WorkflowDefinition::parse(&created.yaml_content).unwrap();
        definition.schedule = Some(WorkflowSchedule {
            cron: String::new(),
            run_at: Some("2099-01-01T00:00:00Z".into()),
            inputs: HashMap::new(),
        });
        let yaml = definition.to_yaml().unwrap();
        let scheduled = manager
            .update_for_agent(
                &created.id,
                "manager",
                created.revision,
                Some(&yaml),
                Some(true),
            )
            .unwrap();
        assert_eq!(scheduled.revision, 1);
        assert!(scheduled.enabled);
        assert!(manager
            .update_for_agent(&created.id, "manager", 0, None, Some(false))
            .is_err());
        let paused = manager
            .update_for_agent(&created.id, "manager", 1, None, Some(false))
            .unwrap();
        assert!(!paused.enabled);
        assert!(manager
            .update_for_agent(&created.id, "outside", paused.revision, None, Some(true))
            .is_err());
        assert!(manager
            .update_for_agent(&template.id, "manager", 0, None, Some(false))
            .is_err());
        let bad = created.yaml_content.replace("worker", "outside");
        assert!(manager
            .update_for_agent(&created.id, "manager", paused.revision, Some(&bad), None)
            .is_err());
        assert_eq!(manager.get(&created.id).unwrap().revision, paused.revision);
    }
    #[test]
    fn authored_batches_reject_foreign_agents_and_connector_actions_atomically() {
        let (_, manager, _) = setup();
        for bad in [
            "name: Foreign\nflows:\n  main:\n    steps:\n      - id: run\n        agent: outside\n        prompt: no",
            "name: Connector\ntrigger: {connector: slack, channel: test, event: message}\nflows:\n  main:\n    steps: []",
        ] {
            let drafts=vec![
                ProjectWorkflowInput {yaml_content:"name: Valid\nflows:\n  main:\n    steps: []".into(),enabled:false},
                ProjectWorkflowInput {yaml_content:bad.into(),enabled:false},
            ];
            assert!(manager.create_for_agent("manager",&drafts).is_err());
            assert_eq!(manager.list().unwrap().len(),1);
        }
    }
}
