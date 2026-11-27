use std::sync::{Arc, RwLock};

use chrono::{DateTime, Local, NaiveDateTime, SecondsFormat, Utc};
use tracing::{debug, error, info};

use crate::config::Config;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::usage::{resolve_agent_provider, QuotaGateDecision, UsageService};

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
    start_schedule_runner_with_usage(db, None, None).await;
}

/// Start the recurring workflow trigger loop with optional quota-gate awareness.
pub async fn start_schedule_runner_with_usage(
    db: Arc<Database>,
    config: Option<Arc<RwLock<Arc<Config>>>>,
    usage: Option<Arc<UsageService>>,
) {
    info!("workflow schedule runner started");

    loop {
        let cfg_snapshot = config
            .as_ref()
            .and_then(|c| c.read().ok().map(|guard| guard.clone()));
        if let Err(error) = check_scheduled_workflows_with_usage(
            &db,
            cfg_snapshot.as_deref(),
            usage.as_deref(),
            Utc::now(),
        )
        .await
        {
            error!(error = %error, "workflow schedule check failed");
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
fn check_scheduled_workflows_at(db: &Arc<Database>, now: DateTime<Utc>) -> Result<u32> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Backend(e.to_string()))?;
    rt.block_on(check_scheduled_workflows_with_usage(db, None, None, now))
}

pub async fn check_scheduled_workflows_with_usage(
    db: &Arc<Database>,
    config: Option<&Config>,
    usage: Option<&UsageService>,
    now: DateTime<Utc>,
) -> Result<u32> {
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

        let deferred_until = workflow
            .quota_deferred_until
            .as_deref()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let deferred_due = match deferred_until {
            Some(until) if until > now => continue,
            Some(_) => schedule.run_at.is_none() || workflow.last_triggered_at.is_none(),
            None => false,
        };

        if !deferred_due && !should_trigger(workflow, schedule, now) {
            continue;
        }

        // Enforce optional quota_gate before starting the instance.
        if let (Some(gate), Some(usage_service)) = (schedule.quota_gate.as_ref(), usage) {
            if let Some(provider) = resolve_workflow_provider(
                db,
                config,
                &definition,
                schedule,
                gate.provider.as_deref(),
            ) {
                match usage_service
                    .evaluate_quota_gate_with_refresh(&provider, gate, now, deferred_due)
                    .await
                {
                    QuotaGateDecision::Allow => {
                        if workflow.quota_deferred_until.is_some() {
                            let _ = manager.clear_quota_deferral(workflow);
                        }
                    }
                    QuotaGateDecision::Defer { until, reason } => {
                        let resume_at = until.to_rfc3339_opts(SecondsFormat::Secs, true);
                        if manager.record_quota_deferral(workflow, &resume_at, &reason)? {
                            info!(
                                workflow_id = workflow.id,
                                provider = %provider,
                                resume_at = %resume_at,
                                "deferred scheduled workflow until quota reset"
                            );
                        } else {
                            debug!(
                                workflow_id = workflow.id,
                                "skipped persisting quota deferral because workflow schedule changed"
                            );
                        }
                        continue;
                    }
                    QuotaGateDecision::Skip { reason } => {
                        let skipped_at = now.naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();
                        manager.skip_scheduled_occurrence(
                            workflow,
                            &skipped_at,
                            &format!("Skipped by quota gate: {reason}"),
                            schedule.run_at.is_some(),
                        )?;
                        info!(
                            workflow_id = workflow.id,
                            provider = %provider,
                            reason = %reason,
                            "skipped scheduled workflow due to quota gate"
                        );
                        continue;
                    }
                }
            }
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

fn resolve_workflow_provider(
    db: &Database,
    config: Option<&Config>,
    definition: &WorkflowDefinition,
    schedule: &WorkflowSchedule,
    explicit_provider: Option<&str>,
) -> Option<String> {
    if let Some(explicit) = explicit_provider.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(
            crate::acp::canonical_agent_kind(explicit)
                .unwrap_or(explicit)
                .to_ascii_lowercase(),
        );
    }

    // Mirror WorkflowEngine::start_scheduled_instance: resolve inputs, build the initial
    // workflow context with variables and schedule.inputs, and render templated agent
    // selectors (such as `{{trigger.payload.worker}}` or `@worker`).
    let raw_trigger = serde_json::json!(schedule.inputs);
    let trigger_data = definition
        .resolve_inputs(&raw_trigger)
        .unwrap_or(raw_trigger);
    let initial_context = super::context::build_context(
        &trigger_data,
        &definition.variables,
        &std::collections::HashMap::new(),
    );
    let mut resolved_def = definition.clone();
    let _ = resolved_def.resolve_agent_bindings(&initial_context, false);

    let first_agent = resolved_def
        .flows
        .get("main")
        .or_else(|| resolved_def.flows.values().next())
        .and_then(|flow| find_first_step_agent(&flow.steps))?;

    let trimmed = first_agent.trim();
    if trimmed.is_empty() || trimmed.starts_with('@') || trimmed.contains("{{") {
        return None;
    }
    resolve_agent_provider(db, config, trimmed)
}

fn find_first_step_agent(steps: &[super::definition::Step]) -> Option<&str> {
    for step in steps {
        if let Some(agent) = step
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            return Some(agent);
        }
        if let Some(body) = step.body.as_deref() {
            if let Some(agent) = find_first_step_agent(body) {
                return Some(agent);
            }
        }
    }
    None
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

    #[tokio::test]
    async fn quota_gate_defers_scheduled_workflow_until_reset_then_runs() {
        use crate::usage::{window_from_used_pct, UsageAccountReport, UsageReportStatus};

        let db = setup();
        let manager = WorkflowManager::new(db.clone());
        let yaml = r#"
name: quota-gated-audit
schedule:
  run_at: "2099-06-01T12:00:00Z"
  quota_gate:
    provider: claude
    min_remaining_pct: 20
    window: session
    on_limit: wait_for_reset
    reset_buffer_seconds: 60
flows:
  main:
    steps:
      - id: audit
        agent: atlas
        prompt: "Run audit"
"#;
        let workflow = manager
            .create(&CreateWorkflow {
                name: "quota-gated-audit".into(),
                description: None,
                yaml_content: yaml.into(),
            })
            .unwrap();

        let usage = UsageService::new();
        let due_at = DateTime::parse_from_rfc3339("2099-06-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        usage
            .set_provider_override(
                "claude",
                vec![UsageAccountReport {
                    id: "claude:test".into(),
                    provider: "claude".into(),
                    provider_label: "Claude".into(),
                    harness: "Claude".into(),
                    account_label: None,
                    status: UsageReportStatus::Available,
                    plan_label: Some("Pro".into()),
                    observed_at: None,
                    fetched_at: "2099-06-01T12:00:00Z".into(),
                    windows: vec![window_from_used_pct(
                        "five_hour",
                        "Session",
                        Some("5h".into()),
                        true,
                        Some(92.0),
                        Some(18_000),
                        Some("2099-06-01T14:00:00Z".into()),
                        None,
                    )],
                    balances: Vec::new(),
                    details: Vec::new(),
                    problem: None,
                    error: None,
                    last_successful: None,
                }],
            )
            .await;

        // First check at due time defers until 14:01:00Z (reset + 60s buffer).
        assert_eq!(
            check_scheduled_workflows_with_usage(&db, None, Some(&usage), due_at)
                .await
                .unwrap(),
            0
        );
        let deferred = manager.get(&workflow.id).unwrap();
        assert_eq!(
            deferred.quota_deferred_until.as_deref(),
            Some("2099-06-01T14:01:00Z")
        );
        assert!(deferred
            .quota_deferred_reason
            .as_deref()
            .is_some_and(|r| r.contains("claude")));

        // While deferred, still does not run before 14:01:00Z.
        let still_waiting = DateTime::parse_from_rfc3339("2099-06-01T13:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            check_scheduled_workflows_with_usage(&db, None, Some(&usage), still_waiting)
                .await
                .unwrap(),
            0
        );

        // After reset, quota recovers to 5% used (95% remaining) and workflow starts!
        usage
            .set_provider_override(
                "claude",
                vec![UsageAccountReport {
                    id: "claude:test".into(),
                    provider: "claude".into(),
                    provider_label: "Claude".into(),
                    harness: "Claude".into(),
                    account_label: None,
                    status: UsageReportStatus::Available,
                    plan_label: Some("Pro".into()),
                    observed_at: None,
                    fetched_at: "2099-06-01T14:01:00Z".into(),
                    windows: vec![window_from_used_pct(
                        "five_hour",
                        "Session",
                        Some("5h".into()),
                        true,
                        Some(5.0),
                        Some(18_000),
                        Some("2099-06-01T19:00:00Z".into()),
                        None,
                    )],
                    balances: Vec::new(),
                    details: Vec::new(),
                    problem: None,
                    error: None,
                    last_successful: None,
                }],
            )
            .await;

        let after_reset = DateTime::parse_from_rfc3339("2099-06-01T14:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            check_scheduled_workflows_with_usage(&db, None, Some(&usage), after_reset)
                .await
                .unwrap(),
            1
        );
        let started_wf = manager.get(&workflow.id).unwrap();
        assert!(started_wf.quota_deferred_until.is_none());
        assert!(started_wf.quota_deferred_reason.is_none());
        assert_eq!(started_wf.trigger_count, 1);
    }

    #[tokio::test]
    async fn quota_gate_resolves_templated_workflow_agent_from_schedule_inputs() {
        use crate::usage::{window_from_used_pct, UsageAccountReport, UsageReportStatus};

        let db = setup();
        let manager = WorkflowManager::new(db.clone());
        let yaml = r#"
name: templated-agent-audit
inputs:
  worker:
    type: agent
    required: true
schedule:
  run_at: "2099-06-01T12:00:00Z"
  inputs:
    worker: atlas
  quota_gate:
    min_remaining_pct: 20
    window: session
    on_limit: wait_for_reset
    reset_buffer_seconds: 60
flows:
  main:
    steps:
      - id: audit
        agent: "{{trigger.payload.worker}}"
        prompt: "Run audit"
"#;
        let workflow = manager
            .create(&CreateWorkflow {
                name: "templated-agent-audit".into(),
                description: None,
                yaml_content: yaml.into(),
            })
            .unwrap();

        let usage = UsageService::new();
        let due_at = DateTime::parse_from_rfc3339("2099-06-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // `setup()` registers `atlas` with backend `codex`.
        usage
            .set_provider_override(
                "codex",
                vec![UsageAccountReport {
                    id: "codex:test".into(),
                    provider: "codex".into(),
                    provider_label: "Codex".into(),
                    harness: "Codex".into(),
                    account_label: None,
                    status: UsageReportStatus::Available,
                    plan_label: Some("Plus".into()),
                    observed_at: None,
                    fetched_at: "2099-06-01T12:00:00Z".into(),
                    windows: vec![window_from_used_pct(
                        "five_hour",
                        "5-hour",
                        Some("5h".into()),
                        true,
                        Some(95.0),
                        Some(18_000),
                        Some("2099-06-01T14:00:00Z".into()),
                        None,
                    )],
                    balances: Vec::new(),
                    details: Vec::new(),
                    problem: None,
                    error: None,
                    last_successful: None,
                }],
            )
            .await;

        assert_eq!(
            check_scheduled_workflows_with_usage(&db, None, Some(&usage), due_at)
                .await
                .unwrap(),
            0
        );
        let deferred = manager.get(&workflow.id).unwrap();
        assert_eq!(
            deferred.quota_deferred_until.as_deref(),
            Some("2099-06-01T14:01:00Z")
        );
    }

    #[test]
    fn stale_quota_deferral_does_not_overwrite_concurrent_schedule_edit() {
        let db = setup();
        let manager = WorkflowManager::new(db.clone());
        let initial_yaml = r#"
name: guarded-deferral
schedule:
  run_at: "2099-06-01T12:00:00Z"
flows:
  main:
    steps:
      - id: audit
        agent: atlas
        prompt: "Run audit"
"#;
        let stale_snapshot = manager
            .create(&CreateWorkflow {
                name: "guarded-deferral".into(),
                description: None,
                yaml_content: initial_yaml.into(),
            })
            .unwrap();

        // User edits the schedule to a later date while the quota check is in flight.
        let updated_yaml = initial_yaml.replace("2099-06-01T12:00:00Z", "2099-12-01T12:00:00Z");
        manager.update(&stale_snapshot.id, &updated_yaml).unwrap();

        // Persisting the deferral from the stale revision must be rejected.
        let persisted = manager
            .record_quota_deferral(
                &stale_snapshot,
                "2099-06-01T14:01:00Z",
                "codex 5-hour has 5% remaining",
            )
            .unwrap();
        assert!(!persisted);
        let current = manager.get(&stale_snapshot.id).unwrap();
        assert!(current.quota_deferred_until.is_none());
        assert!(current.quota_deferred_reason.is_none());
    }
}
