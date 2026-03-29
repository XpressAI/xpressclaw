//! Desired-state reconciler for agent lifecycle (ADR-018).
//!
//! Runs every 10 seconds. Compares desired state (DB) to observed state
//! (Docker) and takes the minimum action to converge: pulling images,
//! starting containers, stopping containers, re-queuing orphaned tasks.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::{debug, error, info, warn};

use crate::agents::registry::AgentRegistry;
use crate::config::Config;
use crate::db::Database;
use crate::docker::images::build_container_spec;
use crate::docker::manager::{DockerManager, VolumeMount};
use crate::tasks::board::TaskBoard;
use crate::tasks::queue::TaskQueue;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(10);
const STABLE_THRESHOLD_SECS: u64 = 300; // 5 minutes

/// Start the reconciliation loop as a background task.
/// Takes the config behind a RwLock so it always sees the latest
/// config after user changes (add/remove agents, update API keys, etc.)
pub async fn start(db: Arc<Database>, config: Arc<RwLock<Arc<Config>>>, server_port: u16) {
    info!(
        "starting desired-state reconciler ({}s interval)",
        RECONCILE_INTERVAL.as_secs()
    );
    loop {
        let config_snapshot = config.read().unwrap().clone();
        if let Err(e) = reconcile_once(&db, &config_snapshot, server_port).await {
            warn!(error = %e, "reconciliation cycle failed");
        }
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

async fn reconcile_once(
    db: &Arc<Database>,
    config: &Config,
    server_port: u16,
) -> crate::error::Result<()> {
    // Connect to Docker — if unavailable, skip this cycle
    let docker = match DockerManager::connect().await {
        Ok(d) => d,
        Err(e) => {
            debug!(error = %e, "Docker not available, skipping reconciliation");
            return Ok(());
        }
    };

    reconcile_images(config, &docker).await;
    reconcile_agents(db, config, &docker, server_port).await;
    reconcile_tasks(db, &docker).await;

    Ok(())
}

/// Pull images for all configured backends.
async fn reconcile_images(config: &Config, docker: &DockerManager) {
    let mut seen = std::collections::HashSet::new();
    for agent in &config.agents {
        let image = crate::docker::images::image_for_backend(&agent.backend);
        if !seen.insert(image.to_string()) {
            continue; // Already handled this image
        }
        if let Err(e) = docker.pull_image(image).await {
            // Non-fatal: use local image if available
            if docker.has_image(image).await {
                debug!(image, error = %e, "pull failed, local image available");
            } else {
                warn!(image, error = %e, "pull failed and no local image");
            }
        }
    }
}

/// Reconcile agent containers against desired state.
async fn reconcile_agents(
    db: &Arc<Database>,
    config: &Config,
    docker: &DockerManager,
    server_port: u16,
) {
    let registry = AgentRegistry::new(db.clone());

    for agent_config in &config.agents {
        let agent = match registry.get(&agent_config.name) {
            Ok(a) => a,
            Err(_) => continue, // Not in DB yet (setup hasn't run)
        };

        let container_name = format!("xpressclaw-{}", agent.id);
        let is_running = docker.is_container_running(&container_name).await;

        match (agent.desired_status.as_str(), is_running) {
            // Desired running, is running → check stability
            ("running", true) => {
                if agent.restart_count > 0 {
                    let uptime = docker.container_uptime_secs(&container_name).await;
                    if uptime >= STABLE_THRESHOLD_SECS {
                        let _ = registry.reset_restart_count(&agent.id);
                        debug!(
                            agent = agent.id,
                            uptime, "agent stable, reset restart count"
                        );
                    }
                }
            }

            // Desired running, not running → start (with backoff)
            ("running", false) => {
                if !should_attempt(agent.restart_count, agent.last_attempt_at.as_deref()) {
                    continue; // Backoff not elapsed, check next agent
                }

                info!(
                    agent = agent.id,
                    restart_count = agent.restart_count,
                    "starting agent"
                );

                // Build container spec
                let mut spec = build_container_spec(
                    agent_config,
                    server_port,
                    config.llm.anthropic_api_key.as_deref(),
                    config.llm.openai_api_key.as_deref(),
                    config.llm.openai_base_url.as_deref(),
                );

                // Mount workspace
                let workspace = config.system.workspace_dir.display().to_string();
                spec.volumes.push(VolumeMount {
                    source: workspace,
                    target: "/workspace".to_string(),
                    read_only: false,
                });

                // Mount documents directory
                let docs_dir = config.system.data_dir.join(&agent.id).join("documents");
                let _ = std::fs::create_dir_all(&docs_dir);
                spec.volumes.push(VolumeMount {
                    source: docs_dir.display().to_string(),
                    target: "/workspace/Documents".to_string(),
                    read_only: false,
                });
                spec.environment
                    .push("DOCUMENTS_DIR=/workspace/Documents".to_string());

                match docker.launch(&agent.id, &spec).await {
                    Ok(info) => {
                        info!(
                            agent = agent.id,
                            container_id = %info.container_id,
                            "agent container started"
                        );
                        let _ = registry.record_attempt(&agent.id, None);
                        // Also update old status column for backward compat
                        let _ = registry.update_status(
                            &agent.id,
                            &crate::agents::state::AgentStatus::Running,
                            Some(&info.container_id),
                        );
                    }
                    Err(e) => {
                        error!(agent = agent.id, error = %e, "failed to start agent");
                        let _ = registry.record_attempt(&agent.id, Some(&e.to_string()));
                        let _ = registry.update_status(
                            &agent.id,
                            &crate::agents::state::AgentStatus::Error(e.to_string()),
                            None,
                        );
                    }
                }
            }

            // Desired stopped, is running → stop
            ("stopped", true) => {
                info!(agent = agent.id, "stopping agent (desired=stopped)");
                let _ = docker.stop(&agent.id).await;
                let _ = registry.update_status(
                    &agent.id,
                    &crate::agents::state::AgentStatus::Stopped,
                    None,
                );
            }

            // Desired stopped, not running → converged
            ("stopped", false) => {}

            _ => {}
        }
    }
}

/// Re-queue tasks stuck in_progress whose agent isn't running.
async fn reconcile_tasks(db: &Arc<Database>, docker: &DockerManager) {
    let board = TaskBoard::new(db.clone());
    let queue = TaskQueue::new(db.clone());

    let tasks = match board.list(Some("in_progress"), None, 100) {
        Ok(t) => t,
        Err(_) => return,
    };

    let registry = AgentRegistry::new(db.clone());
    for task in tasks {
        if let Some(ref agent_id) = task.agent_id {
            let container_name = format!("xpressclaw-{agent_id}");
            if !docker.is_container_running(&container_name).await {
                // Only re-queue if the agent is supposed to be running.
                // If desired=stopped, move task to pending without re-queuing
                // to the same agent (it won't come back).
                let _ = board.update_status(&task.id, "pending", None);
                let agent_desired_running = registry
                    .get(agent_id)
                    .map(|a| a.desired_status == "running")
                    .unwrap_or(false);
                if agent_desired_running {
                    let _ = queue.enqueue(&task.id, agent_id);
                    info!(task_id = task.id, agent_id, "re-queued orphaned task");
                } else {
                    info!(
                        task_id = task.id,
                        agent_id, "unassigned orphaned task (agent stopped)"
                    );
                }
            }
        }
    }
}

/// Exponential backoff: should we attempt to start this agent now?
fn should_attempt(restart_count: i32, last_attempt_at: Option<&str>) -> bool {
    if restart_count == 0 {
        return true;
    }

    let backoff_secs = std::cmp::min(
        10u64.saturating_mul(2u64.saturating_pow(restart_count as u32)),
        300, // cap at 5 minutes
    );

    let Some(last) = last_attempt_at else {
        return true;
    };

    let Ok(last_time) = chrono::NaiveDateTime::parse_from_str(last, "%Y-%m-%d %H:%M:%S") else {
        // Can't parse — attempt anyway
        return true;
    };

    let elapsed = chrono::Utc::now()
        .naive_utc()
        .signed_duration_since(last_time)
        .num_seconds()
        .max(0) as u64;

    elapsed >= backoff_secs
}
