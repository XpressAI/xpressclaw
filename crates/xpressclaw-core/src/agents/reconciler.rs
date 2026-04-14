//! Desired-state reconciler for agent lifecycle (ADR-018).
//!
//! Runs every 10 seconds. Ensures agents with desired_status='running'
//! are marked as running in the DB. With Wanix, there are no containers
//! to manage — agents run in browser environments or server-side.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::agents::pi_rpc::{PiLaunchConfig, PiPool};
use crate::agents::registry::AgentRegistry;
use crate::agents::state::AgentStatus;
use crate::config::Config;
use crate::db::Database;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(10);

/// Start the reconciliation loop as a background task.
pub async fn start(
    db: Arc<Database>,
    config: Arc<RwLock<Arc<Config>>>,
    _server_port: u16,
    pi_pool: Arc<PiPool>,
) {
    info!(
        "starting desired-state reconciler ({}s interval)",
        RECONCILE_INTERVAL.as_secs()
    );
    loop {
        let config_snapshot = config.read().unwrap().clone();
        if let Err(e) = reconcile_once(&db, &config_snapshot, &pi_pool).await {
            warn!(error = %e, "reconciliation cycle failed");
        }
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

async fn reconcile_once(
    db: &Arc<Database>,
    config: &Config,
    pi_pool: &Arc<PiPool>,
) -> crate::error::Result<()> {
    reconcile_agents(db, config, pi_pool).await;
    reconcile_idle_tasks(db, config);
    Ok(())
}

/// Ensure all agents defined in config exist in the DB and have the
/// correct status. When pi.enabled, also warm the pi WASM container
/// for every running agent so the first prompt isn't a 30s pause.
async fn reconcile_agents(db: &Arc<Database>, config: &Config, pi_pool: &Arc<PiPool>) {
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

        // Desired running, status not yet running → mark + warm.
        if record.desired_status == "running" && record.status != "running" {
            debug!(agent_id, "marking agent as running");
            let _ = registry.update_status(agent_id, &AgentStatus::Running, None);

            if config.pi.enabled {
                spawn_pi_warmer(agent_id.clone(), config, pi_pool.clone());
            }
        }

        // Desired stopped, status not yet stopped → mark + evict pi.
        if record.desired_status == "stopped" && record.status != "stopped" {
            debug!(agent_id, "marking agent as stopped");
            let _ = registry.update_status(agent_id, &AgentStatus::Stopped, None);

            if config.pi.enabled {
                let pool = pi_pool.clone();
                let id = agent_id.clone();
                tokio::spawn(async move {
                    pool.evict(&id).await;
                });
            }
        }
    }
}

/// Background-spawn the pi WASM container for an agent. Errors are
/// logged but don't fail reconciliation — the container will retry
/// on next cycle, or on the first conversation message.
fn spawn_pi_warmer(agent_id: String, config: &Config, pi_pool: Arc<PiPool>) {
    let mut launch = PiLaunchConfig::defaults_for(&agent_id);
    launch.c2w_net = config.pi.c2w_net.clone();
    launch.wasm_path = config.pi.wasm_path.clone();
    launch.wasmtime_shim = config.pi.wasmtime_shim.clone();
    launch.xpressclaw_url = config.pi.xpressclaw_url.clone();
    launch.llm_url = config.pi.llm_url.clone();
    launch.llm_key = config.pi.llm_key.clone();
    launch.llm_model = config.pi.llm_model.clone();

    tokio::spawn(async move {
        match pi_pool.get_or_spawn(&launch).await {
            Ok(_) => info!(agent_id, "pi-agent WASM warmed and ready"),
            Err(e) => warn!(agent_id, error = %e, "pi-agent warm-up failed"),
        }
    });
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
        match registry.get(agent_id) {
            Ok(r) if r.status == "running" => {}
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
