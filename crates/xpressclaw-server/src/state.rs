use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use xpressclaw_core::budget::rate_limiter::RateLimiter;
use xpressclaw_core::config::Config;
use xpressclaw_core::db::Database;
#[cfg(feature = "local-llm")]
use xpressclaw_core::llm::llamacpp::DownloadProgress;
use xpressclaw_core::llm::router::LlmRouter;

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
    /// GGUF model download progress (for setup wizard progress bar).
    #[cfg(feature = "local-llm")]
    pub download_progress: Arc<RwLock<DownloadProgress>>,
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
            #[cfg(feature = "local-llm")]
            download_progress: Arc::new(RwLock::new(DownloadProgress::default())),
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
}
