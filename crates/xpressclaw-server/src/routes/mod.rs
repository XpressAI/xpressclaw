use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;

mod activity;
mod agents;
mod budget;
mod conversations;
mod health;
pub mod llm;
mod memory;
mod open_url;
mod procedures;
mod schedules;
mod setup;
mod tasks;
mod tools_proxy;

pub fn tools_proxy_routes() -> Router<AppState> {
    tools_proxy::routes()
}

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health_check))
        .route("/open-url", post(open_url::open_url))
        .nest("/agents", agents::routes())
        .nest("/conversations", conversations::routes())
        .nest("/tasks", tasks::routes())
        .nest("/activity", activity::routes())
        .nest("/budget", budget::routes())
        .nest("/memory", memory::routes())
        .nest("/schedules", schedules::routes())
        .nest("/procedures", procedures::routes())
        .nest("/setup", setup::routes())
}
