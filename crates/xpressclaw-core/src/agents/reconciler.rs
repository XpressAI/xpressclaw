//! Desired-state reconciler for agent lifecycle (ADR-018).
//!
//! In the Docker era this loop launched containers, pulled images,
//! stopped stale agents, and re-queued orphaned tasks every 10 seconds.
//! ADR-023 removed Docker; the agent-launching half of this file is
//! reduced to a warning log until the pi harness (task 10) provides a
//! real launch path.
//!
//! What still runs: `reconcile_models` — pulls Ollama models for
//! ollama-backed agent configs. That's provider-side, not
//! container-side, and is unaffected by the Docker removal.
//!
//! Task 8 (snapshot/rollback) and task 10 (GHCR pull for pi harness)
//! will restore agent-reconciliation behavior on top of
//! `Arc<dyn Harness>` with c2w as the runtime.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::config::Config;
use crate::db::Database;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(10);

/// Start the reconciliation loop as a background task.
pub async fn start(_db: Arc<Database>, config: Arc<RwLock<Arc<Config>>>, _server_port: u16) {
    info!(
        "starting desired-state reconciler ({}s interval, ADR-023 agent launching disabled pending task 10)",
        RECONCILE_INTERVAL.as_secs()
    );
    loop {
        let config_snapshot = config.read().unwrap().clone();
        reconcile_models(&config_snapshot).await;
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
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
