use axum::Json;
use serde_json::{json, Value};

pub async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "build": env!("XPRESSCLAW_BUILD_NUMBER"),
        "git_hash": option_env!("XPRESSCLAW_GIT_HASH").unwrap_or("dev"),
        "name": "xpressclaw"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_reports_version_build_and_commit() {
        let health = health_check().await.0;

        assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(health["build"], env!("XPRESSCLAW_BUILD_NUMBER"));
        assert!(health["git_hash"]
            .as_str()
            .is_some_and(|hash| !hash.is_empty()));
    }
}
