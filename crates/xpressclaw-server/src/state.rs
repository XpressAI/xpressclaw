use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use xpressclaw_core::budget::rate_limiter::RateLimiter;
use xpressclaw_core::config::Config;
use xpressclaw_core::conversations::event_bus::ConversationEventBus;
use xpressclaw_core::db::Database;
use xpressclaw_core::docker::manager::DockerManager;
use xpressclaw_core::llm::router::LlmRouter;
use xpressclaw_core::tools::mcp_manager::McpManager;
use xpressclaw_core::workers::acp::{AcpElicitationBroker, AcpInterruptMode, AcpTurnControlBroker};
use xpressclaw_core::workers::native::{ConversationAcpProcesses, NativeRuntimeLifecycle};

/// Shared application state passed to all Axum handlers.
///
/// Fields that can change at runtime (config reload, setup completion)
/// are wrapped in `Arc<RwLock<>>` so all cloned handles see updates.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<Arc<Config>>>,
    pub db: Arc<Database>,
    pub llm_router: Arc<RwLock<Option<Arc<LlmRouter>>>>,
    pub rate_limiter: Arc<RwLock<Arc<RateLimiter>>>,
    /// Path to the config file (for setup wizard to write to).
    pub config_path: PathBuf,
    /// Whether initial setup has been completed.
    pub setup_complete: Arc<RwLock<bool>>,
    /// MCP tool server manager.
    pub mcp_manager: Arc<McpManager>,
    /// Per-conversation event broadcast channels (ADR-019).
    pub event_bus: Arc<ConversationEventBus>,
    /// Shared Docker connection (reused across all requests).
    pub docker: Arc<RwLock<Option<Arc<DockerManager>>>>,
    /// Live ACP forms waiting for a response from the task UI.
    pub elicitations: Arc<AcpElicitationBroker>,
    /// Live signals that let queued user guidance interrupt an ACP prompt.
    pub turn_controls: Arc<AcpTurnControlBroker>,
    /// Retained per-Conversation ACP lanes shared with the native dispatcher.
    pub conversation_processes: Arc<ConversationAcpProcesses>,
    /// Per-Agent barrier shared by native dispatchers and destructive Project
    /// lifecycle operations.
    pub native_runtime_lifecycle: Arc<NativeRuntimeLifecycle>,
    /// Serialize writes that replace the file-backed and in-memory
    /// configuration so handlers cannot persist and apply stale snapshots.
    pub config_write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Serialize explicit Git-backed Project sync operations so fetch and
    /// publish cannot race through the same temporary Git store state.
    pub project_sync_lock: Arc<tokio::sync::Mutex<()>>,
    /// Serialize Docker lifecycle reconciliation for the local collaboration
    /// stack. Every clone shares this lock, so fixed container, network, and
    /// volume names cannot be concurrently recreated or removed.
    pub collaboration_lifecycle_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AppState {
    /// Create a new AppState. Wraps mutable fields in RwLock.
    pub fn new(
        config: Arc<Config>,
        db: Arc<Database>,
        llm_router: Option<Arc<LlmRouter>>,
        config_path: PathBuf,
        setup_complete: bool,
    ) -> Self {
        let rate_limiter = Arc::new(RateLimiter::new(config.clone()));
        Self {
            config: Arc::new(RwLock::new(config)),
            db,
            llm_router: Arc::new(RwLock::new(llm_router)),
            rate_limiter: Arc::new(RwLock::new(rate_limiter)),
            config_path,
            setup_complete: Arc::new(RwLock::new(setup_complete)),
            mcp_manager: Arc::new(McpManager::new()),
            event_bus: Arc::new(ConversationEventBus::new()),
            docker: Arc::new(RwLock::new(None)),
            elicitations: Arc::new(AcpElicitationBroker::new()),
            turn_controls: Arc::new(AcpTurnControlBroker::new()),
            conversation_processes: Arc::new(ConversationAcpProcesses::default()),
            native_runtime_lifecycle: Arc::new(NativeRuntimeLifecycle::default()),
            config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            project_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            collaboration_lifecycle_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Read the current config.
    pub fn config(&self) -> Arc<Config> {
        self.config.read().unwrap().clone()
    }

    /// Read the current LLM router.
    pub fn llm_router(&self) -> Option<Arc<LlmRouter>> {
        self.llm_router.read().unwrap().clone()
    }

    /// Get the rate limiter.
    pub fn rate_limiter(&self) -> Arc<RateLimiter> {
        self.rate_limiter.read().unwrap().clone()
    }

    /// Get the shared Docker connection, creating it on first use.
    /// Returns None if Docker is unavailable.
    pub async fn docker(&self) -> Option<Arc<DockerManager>> {
        // Fast path: already connected
        {
            let guard = self.docker.read().unwrap();
            if let Some(ref d) = *guard {
                return Some(d.clone());
            }
        }
        // Slow path: try to connect
        let installation_id = self.db.installation_id().ok()?;
        match DockerManager::connect_for_installation(&installation_id).await {
            Ok(d) => {
                let d = Arc::new(d);
                *self.docker.write().unwrap() = Some(d.clone());
                Some(d)
            }
            Err(_) => None,
        }
    }

    /// Check if setup is complete.
    pub fn is_setup_complete(&self) -> bool {
        *self.setup_complete.read().unwrap()
    }

    /// Update config and LLM router after setup/reload.
    pub fn apply_config(&self, config: Arc<Config>, llm_router: Option<Arc<LlmRouter>>) {
        let rate_limiter = Arc::new(RateLimiter::new(config.clone()));
        *self.config.write().unwrap() = config;
        *self.llm_router.write().unwrap() = llm_router;
        *self.rate_limiter.write().unwrap() = rate_limiter;
        *self.setup_complete.write().unwrap() = true;
    }

    /// Stop one worker attempt without cancelling its task. Any queued user
    /// messages remain runnable; otherwise the task returns to pending.
    pub async fn interrupt_attempt(
        &self,
        attempt_id: &str,
    ) -> xpressclaw_core::error::Result<xpressclaw_core::sessions::WorkAttempt> {
        use xpressclaw_core::sessions::SessionManager;
        use xpressclaw_core::tasks::board::TaskBoard;
        use xpressclaw_core::tasks::queue::TaskQueue;

        let sessions = SessionManager::new(self.db.clone());
        let attempt = sessions.get_attempt(attempt_id)?;
        if matches!(
            attempt.status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        ) {
            return Ok(attempt);
        }

        self.turn_controls
            .request_interrupt(attempt_id, AcpInterruptMode::Immediate);
        self.elicitations.cancel_attempt(attempt_id);

        // Re-read after signalling so a container launched concurrently with
        // the request is still stopped. Terminal transitions are immutable,
        // so a late worker update cannot revive this attempt afterwards.
        let current = sessions.get_attempt(attempt_id)?;
        if matches!(
            current.status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        ) {
            return Ok(current);
        }
        if current.container_id.is_some() {
            let docker = self.docker().await.ok_or_else(|| {
                xpressclaw_core::error::Error::DockerNotAvailable(
                    "cannot stop the active agent container".to_string(),
                )
            })?;
            if docker.is_running(&current.session_id).await {
                docker.stop_preserving(&current.session_id).await?;
            }
            sessions.clear_container(attempt_id)?;
        }

        // The worker may have completed while its container was stopping.
        let current = sessions.get_attempt(attempt_id)?;
        if matches!(
            current.status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        ) {
            return Ok(current);
        }
        let interrupted = sessions.transition_attempt(
            attempt_id,
            "interrupted",
            "Agent interrupted by user",
            None,
            None,
        )?;
        if interrupted.status != "interrupted" {
            return Ok(interrupted);
        }
        let queue = TaskQueue::new(self.db.clone());
        if let Some(queue_id) = current.queue_id {
            queue.complete(queue_id, "interrupted by user")?;
        }
        if let Some(task_id) = current.task_id.as_deref() {
            let status = if queue.has_queued_for_task(task_id)? {
                "in_progress"
            } else {
                "pending"
            };
            TaskBoard::new(self.db.clone()).update_status(
                task_id,
                status,
                Some(&current.session_id),
            )?;
        }
        sessions.refresh_status(&current.session_id)?;
        Ok(interrupted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cloned_state_serializes_collaboration_lifecycle_operations() {
        let state = AppState::new(
            Arc::new(Config::default()),
            Arc::new(Database::open_memory().unwrap()),
            None,
            PathBuf::from("test.yaml"),
            true,
        );
        let concurrent_request = state.clone();
        let first = state.collaboration_lifecycle_lock.lock().await;
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            concurrent_request.collaboration_lifecycle_lock.lock(),
        )
        .await
        .is_err());
        drop(first);
        let _second = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            concurrent_request.collaboration_lifecycle_lock.lock(),
        )
        .await
        .expect("the next lifecycle operation should proceed after release");
    }
}
