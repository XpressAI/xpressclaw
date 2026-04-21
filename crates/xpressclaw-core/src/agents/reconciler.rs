//! Desired-state reconciler for agent lifecycle (ADR-018, ADR-023 task 10).
//!
//! Runs every 10 seconds. For each configured agent with
//! `desired_status=running`, launches it via the harness if it isn't
//! already. For agents with `desired_status=stopped`, stops them.
//! Also pulls Ollama models for agents that use Ollama as their LLM.
//!
//! The reconciler takes `Arc<dyn Harness>` by reference — if no
//! harness is installed in `AppState` (wasmtime init failed), the
//! agent branch of the loop logs once and skips.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::agents::registry::AgentRegistry;
use crate::agents::state::{AgentStatus, DesiredStatus};
use crate::config::Config;
use crate::db::Database;
use crate::harness::types::{ContainerSpec, VolumeMount};
use crate::harness::Harness;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(10);

/// Start the reconciliation loop as a background task.
///
/// `harness` is the shared agent harness from `AppState`. Passing it
/// in rather than reading from `AppState` lets the reconciler exist
/// entirely in xpressclaw-core (which doesn't know about AppState)
/// while still sharing the harness the server routes use.
pub async fn start(
    db: Arc<Database>,
    config: Arc<RwLock<Arc<Config>>>,
    _server_port: u16,
    harness: Option<Arc<dyn Harness>>,
) {
    info!(
        interval_secs = RECONCILE_INTERVAL.as_secs(),
        harness_present = harness.is_some(),
        "starting desired-state reconciler"
    );
    loop {
        let config_snapshot = config.read().unwrap().clone();
        reconcile_models(&config_snapshot).await;
        if let Some(ref h) = harness {
            reconcile_agents(&db, &config_snapshot, h.as_ref()).await;
        }
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

/// Reconcile desired vs observed state for every agent in the config.
///
/// - If desired=running and the harness says the agent isn't running,
///   launch it with the agent's configured image + workspace mount.
/// - If desired=stopped and the agent is running, stop it.
/// - Errors are logged and skipped — the next tick retries.
async fn reconcile_agents(db: &Arc<Database>, config: &Config, harness: &dyn Harness) {
    let registry = AgentRegistry::new(db.clone());

    for agent_cfg in &config.agents {
        let record = match registry.ensure(&agent_cfg.name, &agent_cfg.backend) {
            Ok(r) => r,
            Err(e) => {
                warn!(name = agent_cfg.name, error = %e, "registry::ensure failed");
                continue;
            }
        };

        let desired: DesiredStatus = record
            .desired_status
            .parse()
            .unwrap_or(DesiredStatus::Stopped);
        let is_running = harness.is_running(&record.id).await;

        match (desired, is_running) {
            (DesiredStatus::Running, false) => {
                let spec = build_agent_spec(agent_cfg);
                info!(
                    agent_id = %record.id,
                    image = %spec.image,
                    "launching agent via harness"
                );
                let _ = registry.update_status(&record.id, &AgentStatus::Starting, None);
                match harness.launch(&record.id, &spec).await {
                    Ok(info) => {
                        if let Err(e) = registry.update_status(
                            &record.id,
                            &AgentStatus::Running,
                            Some(&info.container_id),
                        ) {
                            warn!(agent_id = %record.id, error = %e, "update_status after launch failed");
                        }
                    }
                    Err(e) => {
                        warn!(agent_id = %record.id, error = %e, "harness launch failed");
                        let _ = registry.update_status(
                            &record.id,
                            &AgentStatus::Error(e.to_string()),
                            None,
                        );
                    }
                }
            }
            (DesiredStatus::Stopped, true) => {
                info!(agent_id = %record.id, "stopping agent via harness");
                if let Err(e) = harness.stop(&record.id).await {
                    warn!(agent_id = %record.id, error = %e, "harness stop failed");
                } else {
                    let _ = registry.update_status(&record.id, &AgentStatus::Stopped, None);
                }
            }
            _ => {} // steady state
        }
    }
}

/// Turn an agent config into a launch spec the harness understands.
///
/// The caller's `image` field is passed through unchanged — the
/// harness's image resolver decides whether to interpret it as a file
/// path, an OCI ref, or fall back to the bundled noop harness (when
/// configured with `with_fallback`). Workspace and user-specified
/// volumes are added; env vars are collected.
fn build_agent_spec(agent: &crate::config::AgentConfig) -> ContainerSpec {
    let mut spec = ContainerSpec {
        image: agent.image.clone().unwrap_or_default(),
        ..Default::default()
    };
    spec.environment.push(format!("AGENT_ID={}", agent.name));
    spec.environment.push(format!("AGENT_NAME={}", agent.name));
    for vol in &agent.volumes {
        if let Some((source, target)) = vol.split_once(':') {
            spec.volumes.push(VolumeMount {
                source: source.to_string(),
                target: target.to_string(),
                read_only: false,
            });
        }
    }
    spec
}

/// Ensure Ollama models are pulled for agents that explicitly use Ollama.
///
/// Provider-side reconciliation, not container-side — survives the
/// ADR-023 Docker removal unchanged.
async fn reconcile_models(config: &Config) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST_OLLAMA_FAIL: AtomicU64 = AtomicU64::new(0);

    if config.llm.default_provider != "ollama" {
        return;
    }

    let base_url = config
        .llm
        .local_base_url
        .as_deref()
        .unwrap_or("http://localhost:11434");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last_fail = LAST_OLLAMA_FAIL.load(Ordering::Relaxed);
    if last_fail > 0 && now - last_fail < 60 {
        return;
    }

    if !crate::llm::local::ollama_is_reachable(base_url).await {
        LAST_OLLAMA_FAIL.store(now, Ordering::Relaxed);
        debug!("Ollama not reachable at {base_url}, skipping model pull");
        return;
    }

    let mut models = std::collections::HashSet::new();
    if let Some(ref m) = config.llm.local_model {
        models.insert(m.clone());
    }
    for agent in &config.agents {
        let uses_ollama = agent
            .llm
            .as_ref()
            .and_then(|l| l.provider.as_deref())
            .unwrap_or(&config.llm.default_provider)
            == "ollama";
        if uses_ollama {
            if let Some(ref m) = agent.model {
                models.insert(m.clone());
            }
        }
    }

    for model in &models {
        if !crate::llm::local::ollama_has_model(base_url, model).await {
            info!(model, "pulling Ollama model");
            if let Err(e) = crate::llm::local::ollama_pull(base_url, model).await {
                warn!(model, error = %e, "failed to pull Ollama model");
                LAST_OLLAMA_FAIL.store(now, Ordering::Relaxed);
            } else {
                info!(model, "Ollama model pull complete");
            }
        }
    }
}
