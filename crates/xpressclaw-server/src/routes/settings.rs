use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/profile", get(get_profile).put(put_profile))
}

#[derive(Serialize, Deserialize)]
struct UserProfile {
    name: String,
    /// Base64-encoded avatar image (data URI), or null
    avatar: Option<String>,
}

async fn get_profile(State(state): State<AppState>) -> Json<UserProfile> {
    let db = state.db.conn();
    let profile: Option<String> = db
        .query_row(
            "SELECT value FROM config WHERE key = 'user_profile'",
            [],
            |row| row.get(0),
        )
        .ok();

    match profile.and_then(|p| serde_json::from_str::<UserProfile>(&p).ok()) {
        Some(p) => Json(p),
        None => Json(UserProfile {
            name: "You".to_string(),
            avatar: None,
        }),
    }
}

async fn put_profile(
    State(state): State<AppState>,
    Json(profile): Json<UserProfile>,
) -> Json<UserProfile> {
    let db = state.db.conn();
    let json = serde_json::to_string(&profile).unwrap_or_default();
    let _ = db.execute(
        "INSERT INTO config (key, value, updated_at) VALUES ('user_profile', ?1, CURRENT_TIMESTAMP)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP",
        [&json],
    );
    Json(profile)
}
