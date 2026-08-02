use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDateTime, Utc};
use serde_json::Value;
use tracing::{debug, error, info};

use crate::db::Database;
use crate::error::Result;

use super::definition::{WorkflowDefinition, WorkflowSchedule};
use super::engine::WorkflowEngine;
use super::manager::{WorkflowManager, WorkflowRecord};

/// Start the recurring workflow trigger loop.
///
/// Due occurrences are found from persisted timestamps, so a short process
/// restart does not lose a run and a restart within the same cron window does
/// not duplicate it.
pub async fn start_schedule_runner(db: Arc<Database>) {
    info!("workflow schedule runner started");

    loop {
        if let Err(error) = check_scheduled_workflows_at(&db, Utc::now()) {
            error!(error = %error, "workflow schedule check failed");
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

fn check_scheduled_workflows_at(db: &Arc<Database>, now: DateTime<Utc>) -> Result<u32> {
    let manager = WorkflowManager::new(db.clone());
    let engine = WorkflowEngine::new(db.clone());
    let workflows = manager.list()?;
    let mut started = 0;

    for workflow in workflows.iter().filter(|workflow| workflow.enabled) {
        let definition = match WorkflowDefinition::parse(&workflow.yaml_content) {
            Ok(definition) => definition,
            Err(error) => {
                error!(workflow_id = workflow.id, error = %error, "invalid scheduled workflow");
                continue;
            }
        };
        let Some(schedule) = definition.schedule.as_ref() else {
            continue;
        };

        if !should_trigger(workflow, schedule, now) {
            continue;
        }

        let triggered_at = now.naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();
        if !manager.claim_scheduled_run(
            &workflow.id,
            workflow.last_triggered_at.as_deref(),
            &triggered_at,
        )? {
            debug!(
                workflow_id = workflow.id,
                "scheduled workflow was already claimed"
            );
            continue;
        }

        let inputs = Value::Object(schedule.inputs.clone().into_iter().collect());
        match engine.start_instance(&workflow.id, inputs) {
            Ok(instance_id) => {
                started += 1;
                info!(
                    workflow_id = workflow.id,
                    instance_id, "started scheduled workflow instance"
                );
            }
            Err(error) => {
                manager.record_trigger_error(&workflow.id, &error.to_string())?;
                error!(
                    workflow_id = workflow.id,
                    error = %error,
                    "failed to start scheduled workflow instance"
                );
            }
        }
    }

    Ok(started)
}

fn should_trigger(
    workflow: &WorkflowRecord,
    schedule: &WorkflowSchedule,
    now: DateTime<Utc>,
) -> bool {
    let cron = match croner::Cron::new(schedule.cron.trim()).parse() {
        Ok(cron) => cron,
        Err(error) => {
            debug!(
                workflow_id = workflow.id,
                cron = schedule.cron,
                error = %error,
                "invalid workflow cron expression"
            );
            return false;
        }
    };

    let check_from = workflow
        .last_triggered_at
        .as_deref()
        .and_then(parse_db_timestamp)
        .or_else(|| parse_db_timestamp(&workflow.updated_at))
        .unwrap_or_else(|| now - chrono::Duration::minutes(2))
        .with_timezone(&Local);
    let now_local = now.with_timezone(&Local);

    cron.iter_after(check_from)
        .next()
        .is_some_and(|next| next <= now_local)
}

fn parse_db_timestamp(value: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|value| value.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::instance::InstanceManager;
    use crate::workflows::manager::CreateWorkflow;

    const SCHEDULED_WORKFLOW: &str = r#"
name: scheduled-report
inputs:
  topic:
    type: string
    required: true
schedule:
  cron: "* * * * *"
  inputs:
    topic: release-readiness
flows:
  main:
    steps:
      - id: report
        agent: atlas
        prompt: "Report on @topic"
"#;

    #[test]
    fn scheduled_workflow_fires_once_and_persists_inputs() {
        let db = Arc::new(Database::open_memory().unwrap());
        let manager = WorkflowManager::new(db.clone());
        let workflow = manager
            .create(&CreateWorkflow {
                name: "scheduled-report".into(),
                description: None,
                yaml_content: SCHEDULED_WORKFLOW.into(),
            })
            .unwrap();
        db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE workflows SET updated_at = '2026-08-02 11:59:58' WHERE id = ?1",
                    [&workflow.id],
                )
                .unwrap();
        });
        let now = DateTime::parse_from_rfc3339("2026-08-02T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(check_scheduled_workflows_at(&db, now).unwrap(), 1);
        assert_eq!(check_scheduled_workflows_at(&db, now).unwrap(), 0);

        let updated = manager.get(&workflow.id).unwrap();
        assert_eq!(updated.trigger_count, 1);
        assert_eq!(
            updated.last_triggered_at.as_deref(),
            Some("2026-08-02 12:00:00")
        );
        assert!(updated.trigger_error.is_none());
        let instances = InstanceManager::new(db)
            .list_instances(&workflow.id, 10)
            .unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(
            instances[0].trigger_data.as_deref(),
            Some(r#"{"topic":"release-readiness"}"#)
        );
    }

    #[test]
    fn disabled_workflow_does_not_fire() {
        let db = Arc::new(Database::open_memory().unwrap());
        let manager = WorkflowManager::new(db.clone());
        let workflow = manager
            .create(&CreateWorkflow {
                name: "scheduled-report".into(),
                description: None,
                yaml_content: SCHEDULED_WORKFLOW.into(),
            })
            .unwrap();
        manager.set_enabled(&workflow.id, false).unwrap();

        assert_eq!(check_scheduled_workflows_at(&db, Utc::now()).unwrap(), 0);
        assert!(InstanceManager::new(db)
            .list_instances(&workflow.id, 10)
            .unwrap()
            .is_empty());
    }
}
