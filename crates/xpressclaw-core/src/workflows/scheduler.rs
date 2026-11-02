use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDateTime, Utc};
use tracing::{debug, error, info};

use crate::db::Database;
use crate::error::{Error, Result};

use super::definition::{WorkflowDefinition, WorkflowSchedule};
use super::engine::WorkflowEngine;
use super::manager::{WorkflowManager, WorkflowRecord};

const CONNECTOR_AUTOMATION_DISABLED: &str =
    "connector triggers and notification sinks cannot run on a schedule in this beta";

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

        if definition.uses_connector_automation() {
            if workflow.trigger_error.as_deref() != Some(CONNECTOR_AUTOMATION_DISABLED) {
                manager.record_trigger_error(&workflow.id, CONNECTOR_AUTOMATION_DISABLED)?;
            }
            debug!(
                workflow_id = workflow.id,
                "skipping scheduled workflow that uses disabled connector automation"
            );
            continue;
        }

        if !should_trigger(workflow, schedule, now) {
            continue;
        }

        let triggered_at = now.naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();
        match engine.start_scheduled_instance(workflow, &triggered_at) {
            Ok(instance_id) => {
                started += 1;
                info!(
                    workflow_id = workflow.id,
                    instance_id, "started scheduled workflow instance"
                );
            }
            Err(Error::WorkflowScheduleChanged) => {
                debug!(
                    workflow_id = workflow.id,
                    "workflow schedule was updated or claimed"
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
    if let Some(run_at) = schedule.run_at.as_deref() {
        return workflow.last_triggered_at.is_none()
            && DateTime::parse_from_rfc3339(run_at).is_ok_and(|deadline| deadline <= now);
    }
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
        .max(parse_db_timestamp(&workflow.updated_at))
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
    use crate::agents::registry::AgentRegistry;
    use crate::workflows::instance::InstanceManager;
    use crate::workflows::manager::CreateWorkflow;
    use crate::workflows::manager::ScheduledWorkflowInput;

    #[test]
    fn project_once_schedule_survives_reload_and_fires_once_in_its_project() {
        let db = setup();
        let manager = WorkflowManager::new(db.clone());
        let template=manager.create(&CreateWorkflow {name:"Once".into(),description:None,
            yaml_content:"name: Once\nflows:\n  main:\n    steps:\n      - id: report\n        agent: atlas\n        prompt: Report\n".into()}).unwrap();
        let copies = manager
            .schedule_for_agent(
                "atlas",
                &[ScheduledWorkflowInput {
                    workflow_id: template.id.clone(),
                    name: None,
                    inputs: Default::default(),
                    run_at: Some("2099-06-01T09:00:00+09:00".into()),
                    cron: None,
                    enabled: true,
                }],
            )
            .unwrap();
        let id = &copies[0].id;
        let deadline = DateTime::parse_from_rfc3339("2099-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            check_scheduled_workflows_at(&db, deadline - chrono::Duration::seconds(1)).unwrap(),
            0
        );
        assert_eq!(check_scheduled_workflows_at(&db, deadline).unwrap(), 1);
        assert_eq!(
            check_scheduled_workflows_at(&db, deadline + chrono::Duration::days(1)).unwrap(),
            0
        );
        let reloaded = WorkflowManager::new(db.clone()).get(id).unwrap();
        assert!(!reloaded.enabled);
        assert_eq!(reloaded.trigger_count, 1);
        let instances = InstanceManager::new(db.clone())
            .list_instances(id, 10)
            .unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].project_id.as_deref(), Some("atlas"));
        // Re-enabling a fired one-shot cannot replay its occurrence.
        manager.set_enabled(id, true).unwrap();
        assert_eq!(
            check_scheduled_workflows_at(&db, deadline + chrono::Duration::days(1)).unwrap(),
            0
        );
        assert!(manager.get(&template.id).unwrap().enabled);
    }

    #[test]
    fn scheduled_instance_and_occurrence_commit_together_and_reject_stale_edits() {
        let db = setup();
        let manager = WorkflowManager::new(db.clone());
        let workflow = manager
            .create(&CreateWorkflow {
                name: "scheduled".into(),
                description: None,
                yaml_content: SCHEDULED_WORKFLOW.into(),
            })
            .unwrap();
        let engine = WorkflowEngine::new(db.clone());
        manager.set_enabled(&workflow.id, false).unwrap();
        assert!(matches!(
            engine.start_scheduled_instance(&workflow, "2099-01-01 00:00:00"),
            Err(Error::WorkflowScheduleChanged)
        ));
        assert!(InstanceManager::new(db.clone())
            .list_instances(&workflow.id, 10)
            .unwrap()
            .is_empty());
        let ready = manager.set_enabled(&workflow.id, true).unwrap();
        engine
            .start_scheduled_instance(&ready, "2099-01-01 00:00:00")
            .unwrap();
        assert!(matches!(
            engine.start_scheduled_instance(&ready, "2099-01-01 00:00:00"),
            Err(Error::WorkflowScheduleChanged)
        ));
        assert_eq!(
            InstanceManager::new(db)
                .list_instances(&workflow.id, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn scheduled_project_work_cannot_follow_an_agent_into_another_project() {
        let db = setup();
        AgentRegistry::new(db.clone())
            .ensure("other", "codex")
            .unwrap();
        let manager = WorkflowManager::new(db.clone());
        let template = manager
            .create(&CreateWorkflow {
                name: "Report".into(),
                description: None,
                yaml_content: SCHEDULED_WORKFLOW.into(),
            })
            .unwrap();
        let record = manager
            .schedule_for_agent(
                "atlas",
                &[ScheduledWorkflowInput {
                    workflow_id: template.id.clone(),
                    name: None,
                    inputs: std::collections::HashMap::from([(
                        "topic".into(),
                        serde_json::json!("Audit"),
                    )]),
                    run_at: Some("2099-01-01T00:00:00Z".into()),
                    cron: None,
                    enabled: true,
                }],
            )
            .unwrap()
            .remove(0);
        manager.set_enabled(&template.id, false).unwrap();
        db.with_conn(|conn| {
            conn.execute("UPDATE agents SET project_id='other' WHERE id='atlas'", [])
        })
        .unwrap();
        let now = DateTime::parse_from_rfc3339("2099-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(check_scheduled_workflows_at(&db, now).unwrap(), 0);
        assert!(InstanceManager::new(db.clone())
            .list_instances(&record.id, 10)
            .unwrap()
            .is_empty());
        assert!(manager.get(&record.id).unwrap().last_triggered_at.is_none());
        db.with_conn(|conn| {
            conn.execute("UPDATE agents SET project_id='atlas' WHERE id='atlas'", [])
        })
        .unwrap();
        assert_eq!(check_scheduled_workflows_at(&db, now).unwrap(), 1);
    }

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

    const CONNECTOR_SCHEDULED_WORKFLOW: &str = r#"
name: scheduled-notification
schedule:
  cron: "* * * * *"
flows:
  main:
    steps:
      - id: notify
        type: sink
        sinks:
          - connector: webhook
            channel: release-notifications
            template: "Release ready"
"#;

    fn setup() -> Arc<Database> {
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("atlas", "codex")
            .unwrap();
        db
    }

    #[test]
    fn scheduled_workflow_fires_once_and_persists_inputs() {
        let db = setup();
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
        let db = setup();
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

    #[test]
    fn connector_backed_workflow_never_fires_on_a_schedule() {
        let db = setup();
        let manager = WorkflowManager::new(db.clone());
        let workflow = manager
            .create(&CreateWorkflow {
                name: "scheduled-notification".into(),
                description: None,
                yaml_content: CONNECTOR_SCHEDULED_WORKFLOW.into(),
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

        assert_eq!(check_scheduled_workflows_at(&db, now).unwrap(), 0);
        let updated = manager.get(&workflow.id).unwrap();
        assert_eq!(updated.trigger_count, 0);
        assert!(updated.last_triggered_at.is_none());
        assert_eq!(
            updated.trigger_error.as_deref(),
            Some(CONNECTOR_AUTOMATION_DISABLED)
        );
        assert!(InstanceManager::new(db)
            .list_instances(&workflow.id, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn resumed_schedule_skips_occurrences_from_before_it_was_enabled() {
        let db = setup();
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
                    "UPDATE workflows
                     SET last_triggered_at = '2026-08-02 11:58:00',
                         updated_at = '2026-08-02 12:00:00'
                     WHERE id = ?1",
                    [&workflow.id],
                )
                .unwrap();
        });

        let just_resumed = DateTime::parse_from_rfc3339("2026-08-02T12:00:30Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(check_scheduled_workflows_at(&db, just_resumed).unwrap(), 0);

        let next_occurrence = DateTime::parse_from_rfc3339("2026-08-02T12:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            check_scheduled_workflows_at(&db, next_occurrence).unwrap(),
            1
        );
    }
}
