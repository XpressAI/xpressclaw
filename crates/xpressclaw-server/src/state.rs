use std::sync::Arc;

use xpressclaw_core::config::Config;
use xpressclaw_core::db::Database;
use xpressclaw_core::llm::router::LlmRouter;

/// Shared application state passed to all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Arc<Database>,
    pub llm_router: Option<Arc<LlmRouter>>,
}
