use axum::middleware;
use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;

mod agents;
pub(crate) mod auth;
mod conversations;
mod dashboard;
mod health;
mod memory;
mod open_url;
mod projects;
mod schedules;
mod sessions;
mod settings;
mod settings_collaboration;
mod settings_instance;
mod settings_sync;
mod setup;
mod tasks;
mod workflows;
mod workspace;

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health_check))
        .nest("/auth", auth::routes())
        .merge(protected_api_routes())
}

/// Public browser API. Health/bootstrap/login remain reachable while all
/// instance data, files, streams, and WebSocket handshakes share one auth
/// boundary.
pub fn public_api_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health_check))
        .nest("/auth", auth::routes())
        .merge(
            protected_api_routes().route_layer(middleware::from_fn_with_state(
                state.auth.clone(),
                crate::server::require_user_session,
            )),
        )
}

fn protected_api_routes() -> Router<AppState> {
    Router::new()
        .route("/open-url", post(open_url::open_url))
        .nest("/agents", agents::routes())
        .nest("/conversations", conversations::routes())
        .nest("/dashboard", dashboard::routes())
        .nest("/memory", memory::routes())
        .nest("/projects", projects::routes())
        .nest("/tasks", tasks::routes())
        .nest("/schedules", schedules::routes())
        .nest("/sessions", sessions::routes())
        .nest("/settings", settings::routes())
        .nest("/setup", setup::routes())
        .nest("/workflows", workflows::routes())
        .nest("/workspaces", workspace::routes())
}
