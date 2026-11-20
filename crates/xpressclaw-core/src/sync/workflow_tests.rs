use super::*;
use crate::workflows::manager::{
    CreateWorkflow, ProjectWorkflowInput, WorkflowManager, WorkflowRecord,
};
use std::sync::Arc;

fn setup() -> (Arc<Database>, Config, WorkflowManager) {
    let db = Arc::new(Database::open_memory().unwrap());
    db.with_conn(|conn| {
        conn.execute_batch(
            "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One'), ('project-two', 'Project Two');
             INSERT INTO agents (id, name, backend, config, project_id)
                VALUES ('atlas', 'Atlas', 'codex', '{}', 'project-one'),
                       ('outside', 'Outside', 'codex', '{}', 'project-two');",
        )
    })
    .unwrap();
    let config = Config {
        agents: ["atlas", "outside"]
            .into_iter()
            .map(|name| AgentConfig {
                name: name.into(),
                backend: "codex".into(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let workflows = WorkflowManager::new(db.clone());
    (db, config, workflows)
}

fn owned(
    workflows: &WorkflowManager,
    agent: &str,
    name: &str,
    schedule: &str,
    enabled: bool,
) -> WorkflowRecord {
    workflows
        .create_for_agent(
            agent,
            &[ProjectWorkflowInput {
                yaml_content: format!("name: {name}\n{schedule}flows:\n  main:\n    steps: []\n"),
                enabled,
            }],
        )
        .unwrap()
        .remove(0)
}

fn associated(db: &Database, id: &str) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO project_workflows(project_id,workflow_id) VALUES('project-one',?1)",
            [id],
        )
    })
    .unwrap();
}

fn export(db: &Database, config: &Config) -> PortableSnapshot {
    export_snapshot(
        db,
        config,
        &ProjectSyncManifest::new(
            "project-one",
            "git@example.test:data.git",
            "main",
            "projects/project-one",
        )
        .unwrap(),
    )
    .unwrap()
}

fn import(db: &Database, config: &mut Config, snapshot: &PortableSnapshot) -> Result<()> {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("xpressclaw.yaml");
    import_snapshot(db, config, &config_path, directory.path(), snapshot).map(|_| ())
}

#[test]
fn export_includes_unstarted_owned_workflows_and_only_associated_shared_templates() {
    let (db, config, workflows) = setup();
    let paused = owned(&workflows, "atlas", "Approval held", "", false);
    let future = owned(
        &workflows,
        "atlas",
        "Future",
        "schedule:\n  run_at: '2099-01-01T12:00:00Z'\n",
        true,
    );
    let recurring = owned(
        &workflows,
        "atlas",
        "Recurring",
        "schedule:\n  cron: '0 9 * * Mon'\n",
        true,
    );
    let foreign = owned(&workflows, "outside", "Foreign", "", false);
    let mut shared = None;
    for name in ["Shared", "Unassociated"] {
        let template = workflows
            .create(&CreateWorkflow {
                name: name.into(),
                description: None,
                yaml_content: format!("name: {name}\nflows:\n  main:\n    steps: []\n"),
            })
            .unwrap();
        if name == "Shared" {
            associated(&db, &template.id);
            shared = Some(template);
        }
    }
    // An association from an earlier run must neither duplicate an owned
    // definition nor expose another Project's owned definition.
    associated(&db, &recurring.id);
    associated(&db, &foreign.id);
    let snapshot = export(&db, &config);
    let actual = snapshot
        .workflows
        .iter()
        .map(|w| w.id.clone())
        .collect::<BTreeSet<_>>();
    let expected = [paused.id, future.id, recurring.id, shared.unwrap().id]
        .into_iter()
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(snapshot.workflows.len(), actual.len());
    assert_eq!(
        snapshot
            .workflows
            .iter()
            .filter(|w| w.project_owned)
            .count(),
        3
    );
}

#[test]
fn imported_owned_workflows_remain_private_and_editable() {
    let (source, config, workflows) = setup();
    let planned = owned(
        &workflows,
        "atlas",
        "Future",
        "schedule:\n  run_at: '2099-01-01T12:00:00Z'\n",
        true,
    );
    associated(&source, &planned.id);
    let mut snapshot = export(&source, &config);
    let directory = tempfile::tempdir().unwrap();
    snapshot.write(directory.path()).unwrap();
    let loaded = PortableSnapshot::load(directory.path(), "project-one").unwrap();
    assert_eq!(snapshot, loaded);

    let (target, mut config, manager) = setup();
    import(&target, &mut config, &loaded).unwrap();
    let imported = manager.get(&planned.id).unwrap();
    assert_eq!(imported.project_id.as_deref(), Some("project-one"));
    assert_eq!(imported.yaml_content, planned.yaml_content);
    assert!(
        !imported.enabled,
        "new imports remain paused on this installation"
    );
    assert!(manager.get_for_agent(&planned.id, "outside").is_err());
    let outside = manager.list_for_agent("outside", 100, 0, true).unwrap();
    assert!(outside["workflows"].as_array().unwrap().is_empty());
    let enabled = manager
        .update_for_agent(&planned.id, "atlas", imported.revision, None, Some(true))
        .unwrap();
    assert!(enabled.enabled);
    assert!(manager
        .update_for_agent(&planned.id, "outside", enabled.revision, None, Some(false))
        .is_err());
}

#[test]
fn legacy_shared_workflows_keep_their_representation_and_ownership() {
    let (source, config, workflows) = setup();
    let shared = workflows
        .create(&CreateWorkflow {
            name: "Shared".into(),
            description: None,
            yaml_content: "name: Shared\nflows:\n  main:\n    steps: []\n".into(),
        })
        .unwrap();
    associated(&source, &shared.id);
    let mut snapshot = export(&source, &config);
    let record = serde_json::to_value(&snapshot.workflows[0]).unwrap();
    assert!(record.get("project_owned").is_none());
    let legacy: PortableWorkflow = serde_json::from_value(record).unwrap();
    assert!(!legacy.project_owned);
    let directory = tempfile::tempdir().unwrap();
    snapshot.write(directory.path()).unwrap();
    let loaded = PortableSnapshot::load(directory.path(), "project-one").unwrap();
    assert_eq!(snapshot, loaded);
    let (target, mut config, manager) = setup();
    import(&target, &mut config, &loaded).unwrap();
    let imported = manager.get(&shared.id).unwrap();
    assert!(imported.project_id.is_none());
    assert!(!imported.enabled);
    assert!(manager
        .update_for_agent(&shared.id, "atlas", imported.revision, None, Some(true))
        .is_err());
}

#[test]
fn import_rejects_workflow_ownership_changes_atomically() {
    let (source, config, workflows) = setup();
    let planned = owned(&workflows, "atlas", "Planned", "", false);
    let snapshot = export(&source, &config);
    for (local_owner, remote_owned) in [
        (None, true),
        (Some("project-one"), false),
        (Some("project-two"), true),
        (Some("project-two"), false),
    ] {
        let mut snapshot = snapshot.clone();
        snapshot.project.name = "Changed remotely".into();
        snapshot.workflows[0].project_owned = remote_owned;
        let (target, mut config, manager) = setup();
        target
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO workflows(id,name,yaml_content,project_id,enabled,revision)
             VALUES(?1,?2,?3,?4,1,7)",
                    params![planned.id, planned.name, planned.yaml_content, local_owner],
                )
            })
            .unwrap();
        associated(&target, &planned.id);
        let before = serde_json::to_value(manager.get(&planned.id).unwrap()).unwrap();
        let error = import(&target, &mut config, &snapshot).unwrap_err();
        assert!(error.to_string().contains("ownership"));
        assert_eq!(
            serde_json::to_value(manager.get(&planned.id).unwrap()).unwrap(),
            before
        );
        let project = crate::projects::ProjectManager::new(target)
            .get("project-one")
            .unwrap();
        assert_eq!(project.name, "Project One");
    }
}

#[test]
fn importing_owned_workflow_updates_invalidates_stale_edits_and_resets_changed_schedules() {
    let (db, mut config, workflows) = setup();
    let planned = owned(
        &workflows,
        "atlas",
        "Future",
        "schedule:\n  run_at: '2099-01-01T12:00:00Z'\n",
        true,
    );
    db.with_conn(|conn| conn.execute(
        "UPDATE workflows SET last_triggered_at='2099-01-01T12:00:00Z',trigger_count=1,trigger_error='Previous error' WHERE id=?1",
        [&planned.id],
    )).unwrap();
    let mut snapshot = export(&db, &config);
    snapshot.workflows[0].yaml_content = snapshot.workflows[0]
        .yaml_content
        .replace("2099-01-01", "2099-02-01");
    import(&db, &mut config, &snapshot).unwrap();
    let current = workflows.get(&planned.id).unwrap();
    assert_eq!(current.project_id.as_deref(), Some("project-one"));
    assert!(current.enabled, "existing local enablement is preserved");
    assert_eq!(current.revision, planned.revision + 1);
    assert!(current.last_triggered_at.is_none());
    assert!(current.trigger_error.is_none());
    assert_eq!(current.trigger_count, 1);
    assert!(workflows
        .update_for_agent(&planned.id, "atlas", planned.revision, None, Some(false))
        .is_err());
    import(&db, &mut config, &snapshot).unwrap();
    assert_eq!(
        workflows.get(&planned.id).unwrap().revision,
        current.revision
    );
    assert!(
        !workflows
            .update_for_agent(&planned.id, "atlas", current.revision, None, Some(false))
            .unwrap()
            .enabled
    );
}
