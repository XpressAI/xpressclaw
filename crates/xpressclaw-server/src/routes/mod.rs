use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;

mod agents;
mod health;
mod memory;
mod open_url;
mod schedules;
mod sessions;
mod settings;
mod setup;
mod tasks;
mod workflows;

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health_check))
        .route("/open-url", post(open_url::open_url))
        .nest("/agents", agents::routes())
        .nest("/memory", memory::routes())
        .nest("/tasks", tasks::routes())
        .nest("/schedules", schedules::routes())
        .nest("/sessions", sessions::routes())
        .nest("/settings", settings::routes())
        .nest("/setup", setup::routes())
        .nest("/workflows", workflows::routes())
}
