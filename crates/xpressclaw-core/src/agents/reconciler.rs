//! Desired-state reconciler for agent lifecycle (ADR-018).
//!
//! Runs every 10 seconds. Ensures agents with desired_status='running'
//! are marked as running in the DB. With Wanix, there are no containers
//! to manage — agents run in browser environments or server-side.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::agents::registry::AgentRegistry;
use crate::agents::state::AgentStatus;
use crate::config::Config;
use crate::db::Database;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(10);

/// Start the reconciliation loop as a background task.
pub async fn start(db: Arc<Database>, config: Arc<RwLock<Arc<Config>>>, _server_port: u16) {
    info!(
        "starting desired-state reconciler ({}s interval)",
        RECONCILE_INTERVAL.as_secs()
    );
    loop {
        let config_snapshot = config.read().unwrap().clone();
        if let Err(e) = reconcile_once(&db, &config_snapshot).await {
            warn!(error = %e, "reconciliation cycle failed");
        }
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

async fn reconcile_once(
    db: &Arc<Database>,
    config: &Config,
) -> crate::error::Result<()> {
    reconcile_agents(db, config).await;
    reconcile_idle_tasks(db, config);
    Ok(())
}

/// Ensure all agents defined in config exist in the DB and have the
/// correct status. Without Docker, agents are "running" as long as
/// their desired_status is "running".
async fn reconcile_agents(db: &Arc<Database>, config: &Config) {
    let registry = AgentRegistry::new(db.clone());

    for agent_cfg in &config.agents {
        let agent_id = &agent_cfg.name;

        // Ensure agent exists in registry
        if let Err(e) = registry.ensure(agent_id, &agent_cfg.backend) {
            warn!(agent_id, error = %e, "failed to register agent");
            continue;
        }

        let record = match registry.get(agent_id) {
            Ok(r) => r,
            Err(_) => continue,
        };

        // If desired running but status isn't, mark as running
        if record.desired_status == "running" && record.status != "running" {
            debug!(agent_id, "marking agent as running");
            let _ = registry.update_status(agent_id, &AgentStatus::Running, None);
        }

        // If desired stopped but status isn't, mark as stopped
        if record.desired_status == "stopped" && record.status != "stopped" {
            debug!(agent_id, "marking agent as stopped");
            let _ = registry.update_status(agent_id, &AgentStatus::Stopped, None);
        }
    }
}

/// Create idle tasks for agents that have an idle_prompt configured.
fn reconcile_idle_tasks(db: &Arc<Database>, config: &Config) {
    use crate::tasks::board::TaskBoard;
    use crate::tasks::queue::TaskQueue;

    let registry = AgentRegistry::new(db.clone());
    let board = TaskBoard::new(db.clone());
    let queue = TaskQueue::new(db.clone());

    for agent_cfg in &config.agents {
        let Some(ref prompt) = agent_cfg.idle_prompt else {
            continue;
        };

        let agent_id = &agent_cfg.name;
        let record = match registry.get(agent_id) {
            Ok(r) if r.status == "running" => r,
            _ => continue,
        };

        // Only if agent has nothing to do
        if queue.pending_count(agent_id).unwrap_or(0) > 0 {
            continue;
        }

        let task = board.create(&crate::tasks::board::CreateTask {
            title: "Idle task".into(),
            description: Some(prompt.clone()),
            agent_id: Some(agent_id.clone()),
            parent_task_id: None,
            sop_id: None,
            conversation_id: None,
            priority: None,
            context: None,
        });

        if let Ok(task) = task {
            let _ = queue.enqueue(&task.id, agent_id);
            debug!(agent_id, task_id = task.id, "created idle task");
        }
    }
}
