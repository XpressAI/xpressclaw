use super::*;
use crate::agents::registry::AgentRegistry;
use crate::tasks::planning::{PlanningAction, PlanningChange, PlanningFilter, TaskPlanner};
use serde_json::json;

fn setup() -> (Arc<Database>, TaskBoard, TaskQueue) {
    let db = Arc::new(Database::open_memory().unwrap());
    let registry = AgentRegistry::new(db.clone());
    for name in ["manager", "worker-a", "worker-b", "outside"] {
        registry.ensure(name, "codex").unwrap();
    }
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE agents SET project_id='manager' WHERE id IN ('worker-a','worker-b')",
            [],
        )
    })
    .unwrap();
    (db.clone(), TaskBoard::new(db.clone()), TaskQueue::new(db))
}
fn inputs(value: serde_json::Value) -> Vec<BatchTaskInput> {
    serde_json::from_value(value).unwrap()
}
fn counts(db: &Database) -> (i64, i64) {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT (SELECT COUNT(*) FROM tasks),(SELECT COUNT(*) FROM task_queue)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    })
    .unwrap()
}

#[test]
fn manager_plan_obeys_backlog_future_start_and_dependencies() {
    let (db, board, queue) = setup();
    let created = board.create_batch_for_agent(&inputs(json!([
        {"ref":"research","title":"Research","agent_id":"worker-a","backlog":true},
        {"title":"Implement","agent_id":"worker-b","depends_on":["research"]},
        {"title":"Next month","agent_id":"worker-a","start_after":"2099-06-01T09:00:00+09:00"}
    ])), None, "manager").unwrap();
    assert_eq!(created.len(), 3);
    assert!(
        created
            .iter()
            .all(|task| task.project_id.as_deref() == Some("manager")
                && task.parent_task_id.is_none())
    );
    assert_eq!(
        board.get_dependencies(&created[1].id).unwrap(),
        vec![created[0].id.clone()]
    );
    assert_eq!(
        created[2].start_after.as_deref(),
        Some("2099-06-01T00:00:00.000Z")
    );
    assert_eq!(counts(&db), (3, 2));
    assert!(queue.claim("worker-a").unwrap().is_none());
    assert!(queue.claim("worker-b").unwrap().is_none());
    let planner = TaskPlanner::new(db.clone());
    planner
        .change_for_agent(
            &created[0].id,
            &PlanningChange {
                expected_revision: created[0].revision,
                change: PlanningAction::Queue,
            },
            "manager",
        )
        .unwrap();
    assert_eq!(
        queue.claim("worker-a").unwrap().unwrap().task_id,
        created[0].id
    );
    board
        .update_status(&created[0].id, "completed", Some("worker-a"))
        .unwrap();
    assert_eq!(
        queue.claim("worker-b").unwrap().unwrap().task_id,
        created[1].id
    );
}

#[test]
fn manager_batches_roll_back_all_tasks_and_queue_entries_on_invalid_plans() {
    let (db, board, _) = setup();
    let outside = board
        .create(&CreateTask {
            title: "Private".into(),
            agent_id: Some("outside".into()),
            ..Default::default()
        })
        .unwrap();
    let before = counts(&db);
    for invalid in [
        json!([{"title":"Valid"},{"title":"Invalid","agent_id":"missing"}]),
        json!([{"title":"Valid"},{"title":"Invalid","agent_id":"outside"}]),
        json!([{"title":"Valid"},{"title":"Invalid","depends_on":[outside.id]}]),
        json!([{"title":"Valid"},{"title":"Invalid","depends_on":["missing"]}]),
        json!([{"title":"A","ref":"same"},{"title":"B","ref":"same"}]),
        json!([{"title":"A","ref":"a","depends_on":["b"]},{"title":"B","ref":"b","depends_on":["a"]}]),
        json!([{"title":"Valid"},{"title":"Invalid","start_after":"tomorrow"}]),
        json!([{"title":"Valid"},{"title":"   "}]),
        json!([{"title":"Invalid","ref":"self","depends_on":["self"]}]),
    ] {
        assert!(
            board
                .create_batch_for_agent(&inputs(invalid.clone()), None, "manager")
                .is_err(),
            "{invalid}"
        );
        assert_eq!(counts(&db), before, "{invalid}");
    }
    assert!(board
        .create_batch_for_agent(
            &inputs(json!([{"title":"Child"}])),
            Some(&outside.id),
            "manager"
        )
        .is_err());
    assert_eq!(counts(&db), before);
}

#[test]
fn manager_batches_scale_and_cannot_create_parent_completion_cycles() {
    let (db, board, _) = setup();
    let batch = (0..100)
        .map(|index| {
            json!({
                "ref":format!("step-{index}"),"title":format!("Task {index}"),"backlog":true,
                "depends_on":if index==0 {vec![]} else {vec![format!("step-{}",index-1)]},
            })
        })
        .collect::<Vec<_>>();
    let created = board
        .create_batch_for_agent(&inputs(json!(batch)), None, "manager")
        .unwrap();
    assert_eq!(counts(&db), (100, 0));
    let prerequisite = board
        .create_batch_for_agent(
            &inputs(json!([{
                "title":"Wait for parent","backlog":true,"depends_on":[created[0].id]
            }])),
            None,
            "manager",
        )
        .unwrap()
        .remove(0);
    assert!(board
        .create_batch_for_agent(
            &inputs(json!([{
                "title":"Indirect cycle","depends_on":[prerequisite.id]
            }])),
            Some(&created[0].id),
            "manager"
        )
        .is_err());
    assert_eq!(counts(&db), (101, 0));
    assert_eq!(
        board.get_dependencies(&created[99].id).unwrap(),
        vec![created[98].id.clone()]
    );
    assert!(board
        .create_batch_for_agent(
            &inputs(json!([{"title":"Deadlock","depends_on":[created[0].id]}])),
            Some(&created[0].id),
            "manager"
        )
        .is_err());
    assert_eq!(counts(&db), (101, 0));
    assert!(board.create_batch_for_agent(&[], None, "manager").is_err());
    assert!(board
        .create_batch_for_agent(
            &inputs(json!((0..101)
                .map(|_| json!({"title":"Too many"}))
                .collect::<Vec<_>>())),
            None,
            "manager"
        )
        .is_err());
}

#[test]
fn manager_discovery_and_planning_are_project_scoped_and_revision_checked() {
    let (db, board, _) = setup();
    let roster = AgentRegistry::new(db.clone())
        .project_roster("manager", 2, 0)
        .unwrap();
    assert_eq!(roster["total"], 3);
    assert_eq!(roster["agents"].as_array().unwrap().len(), 2);
    assert!(!roster.to_string().contains("outside"));
    let own = board
        .create_batch_for_agent(
            &inputs(json!([{"title":"Plan","backlog":true}])),
            None,
            "manager",
        )
        .unwrap()
        .remove(0);
    let other = board
        .create(&CreateTask {
            title: "Private".into(),
            agent_id: Some("outside".into()),
            ..Default::default()
        })
        .unwrap();
    let planner = TaskPlanner::new(db);
    let listed = planner
        .list_for_agent(
            &PlanningFilter {
                project_id: Some("outside".into()),
                ..Default::default()
            },
            "manager",
        )
        .unwrap();
    assert_eq!(listed.total, 1);
    assert_eq!(listed.tasks[0].task.id, own.id);
    assert!(planner.detail_for_agent(&other.id, "manager").is_err());
    assert!(planner
        .change_for_agent(
            &other.id,
            &PlanningChange {
                expected_revision: other.revision,
                change: PlanningAction::Backlog
            },
            "manager"
        )
        .is_err());
    assert!(planner
        .change_for_agent(
            &own.id,
            &PlanningChange {
                expected_revision: own.revision,
                change: PlanningAction::Assign {
                    agent_id: "outside".into()
                }
            },
            "manager"
        )
        .is_err());
    planner
        .change_for_agent(
            &own.id,
            &PlanningChange {
                expected_revision: own.revision,
                change: PlanningAction::Priority { priority: 3 },
            },
            "manager",
        )
        .unwrap();
    assert!(planner
        .change_for_agent(
            &own.id,
            &PlanningChange {
                expected_revision: own.revision,
                change: PlanningAction::Queue
            },
            "manager"
        )
        .is_err());
}

#[test]
fn manager_can_delegate_from_its_own_hidden_turn_context() {
    let (db, board, _) = setup();
    let parent = board
        .create(&CreateTask {
            title: "Idle planning".into(),
            agent_id: Some("manager".into()),
            ..Default::default()
        })
        .unwrap();
    db.with_conn(|conn| conn.execute("UPDATE tasks SET hidden=1 WHERE id=?1", [&parent.id]))
        .unwrap();
    let child = board
        .create_batch_for_agent(
            &inputs(json!([{
                "title":"Follow up", "agent_id":"worker-a", "backlog":true
            }])),
            Some(&parent.id),
            "manager",
        )
        .unwrap()
        .remove(0);
    assert_eq!(child.parent_task_id.as_deref(), Some(parent.id.as_str()));
    assert!(child.blocks_parent);
}
