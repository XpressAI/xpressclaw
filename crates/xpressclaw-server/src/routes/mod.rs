use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;

mod activity;
mod agents;
mod apps;
mod browser;
mod budget;
mod conversations;
mod health;
pub mod llm;
mod memory;
mod office;
mod open_url;
mod procedures;
mod schedules;
mod settings;
mod setup;
mod skills;
mod tasks;
mod tools_proxy;

pub fn tools_proxy_routes() -> Router<AppState> {
    tools_proxy::routes()
}

pub fn app_proxy_routes() -> Router<AppState> {
    apps::proxy_routes()
}

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health_check))
        .route("/open-url", post(open_url::open_url))
        .nest("/agents", agents::routes())
        .nest("/apps", apps::routes())
        .nest("/conversations", conversations::routes())
        .nest("/tasks", tasks::routes())
        .nest("/activity", activity::routes())
        .nest("/budget", budget::routes())
        .nest("/memory", memory::routes())
        .nest("/schedules", schedules::routes())
        .nest("/procedures", procedures::routes())
        .nest("/settings", settings::routes())
        .nest("/skills", skills::routes())
        .nest("/setup", setup::routes())
        .nest("/office", office::routes())
        .nest("/browser", browser::routes())
}
