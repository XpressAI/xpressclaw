use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::auth::{hash_password, load_password_hash, password_configured, store_password_hash};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(get_instance).put(put_instance))
        // This endpoint accepts listener settings and, optionally, one
        // password. It must not inherit the attachment-sized API body limit.
        .layer(DefaultBodyLimit::max(8 * 1024))
}

#[derive(Debug, Serialize)]
struct ListenerSettings {
    bind: IpAddr,
    port: u16,
    authentication_enabled: bool,
    allow_unauthenticated_remote: bool,
}

#[derive(Debug, Serialize)]
struct InstanceSettingsResponse {
    instance_id: String,
    effective: ListenerSettings,
    saved: ListenerSettings,
    restart_required: bool,
    credential_kind: &'static str,
    password_configured: bool,
    config_path: String,
    data_dir: String,
    workspace_dir: String,
    transport_encryption: &'static str,
}

async fn get_instance(State(state): State<AppState>) -> Response {
    match response_for(&state) {
        Ok(response) => Json(response).into_response(),
        Err(error) => super::auth::error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Could not inspect instance authentication: {error}"),
        ),
    }
}

#[derive(Deserialize)]
struct UpdateInstanceRequest {
    bind: IpAddr,
    port: u16,
    authentication_enabled: bool,
    #[serde(default)]
    acknowledge_unauthenticated_remote: bool,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    remove_password: bool,
}

async fn put_instance(
    State(state): State<AppState>,
    Json(mut body): Json<UpdateInstanceRequest>,
) -> Response {
    if body.port == 0 {
        return super::auth::error_response(
            StatusCode::BAD_REQUEST,
            "Port must be between 1 and 65535",
        );
    }
    if !body.bind.is_loopback()
        && !body.authentication_enabled
        && !body.acknowledge_unauthenticated_remote
    {
        return super::auth::error_response(
            StatusCode::BAD_REQUEST,
            "Confirm that this unauthenticated non-loopback listener is protected by an operator-trusted LAN or tailnet",
        );
    }
    if body.remove_password && body.password.is_some() {
        return super::auth::error_response(
            StatusCode::BAD_REQUEST,
            "Choose either a new password or Remove password, not both",
        );
    }
    if let Some(password) = body.password.as_ref() {
        if !(12..=1024).contains(&password.chars().count()) {
            return super::auth::error_response(
                StatusCode::BAD_REQUEST,
                "Password must be between 12 and 1024 characters",
            );
        }
    }

    let next_hash = match body.password.take() {
        Some(password) => match hash_password(Zeroizing::new(password)).await {
            Ok(hash) => Some(Some(hash)),
            Err(error) => {
                return super::auth::error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Could not hash the password: {error}"),
                )
            }
        },
        None if body.remove_password => Some(None),
        None => None,
    };

    let _config_guard = state.config_write_lock.lock().await;
    let mut config = if state.config_path.exists() {
        match xpressclaw_core::config::Config::load(&state.config_path) {
            Ok(config) => config,
            Err(error) => {
                return super::auth::error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Could not reload instance configuration: {error}"),
                )
            }
        }
    } else {
        (*state.config()).clone()
    };

    let previous_auth_enabled = config.instance.authentication_enabled;
    config.instance.bind = body.bind;
    config.instance.port = body.port;
    config.instance.authentication_enabled = body.authentication_enabled;
    config.instance.allow_unauthenticated_remote = !body.bind.is_loopback()
        && !body.authentication_enabled
        && body.acknowledge_unauthenticated_remote;

    let data_dir = config.system.data_dir.clone();
    let previous_hash = match load_password_hash(&data_dir) {
        Ok(hash) => hash,
        Err(error) => {
            return super::auth::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Could not read instance authentication secret: {error}"),
            )
        }
    };

    if let Some(hash) = next_hash.as_ref() {
        if let Err(error) = store_password_hash(&data_dir, hash.as_deref()) {
            return super::auth::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Could not update instance authentication secret: {error}"),
            );
        }
    }
    if let Err(error) = config.save(&state.config_path) {
        let rollback_error = if next_hash.is_some() {
            store_password_hash(&data_dir, previous_hash.as_deref()).err()
        } else {
            None
        };
        let message = match rollback_error {
            Some(rollback) => format!(
                "Could not save instance configuration: {error}; the authentication secret also could not be restored: {rollback}"
            ),
            None => format!("Could not save instance configuration: {error}"),
        };
        return super::auth::error_response(StatusCode::INTERNAL_SERVER_ERROR, &message);
    }

    state.apply_config(Arc::new(config), state.llm_router());
    if let Some(hash) = next_hash {
        state.auth.replace_password_hash(hash);
    } else if previous_auth_enabled != body.authentication_enabled {
        state.auth.revoke_all();
    }

    match response_for(&state) {
        Ok(response) => Json(response).into_response(),
        Err(error) => super::auth::error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Instance settings were saved but could not be re-read: {error}"),
        ),
    }
}

fn response_for(state: &AppState) -> anyhow::Result<InstanceSettingsResponse> {
    let saved = state.config();
    let effective = state.effective_instance.as_ref();
    let restart_required = saved.instance.bind != effective.bind
        || saved.instance.port != effective.port
        || saved.instance.authentication_enabled != effective.authentication_enabled
        || state.auth.credential_kind() == crate::auth::CredentialKind::RestartRequired;
    Ok(InstanceSettingsResponse {
        instance_id: state.auth.instance_id().to_string(),
        effective: ListenerSettings {
            bind: effective.bind,
            port: effective.port,
            authentication_enabled: effective.authentication_enabled,
            allow_unauthenticated_remote: effective.allow_unauthenticated_remote,
        },
        saved: ListenerSettings {
            bind: saved.instance.bind,
            port: saved.instance.port,
            authentication_enabled: saved.instance.authentication_enabled,
            allow_unauthenticated_remote: saved.instance.allow_unauthenticated_remote,
        },
        restart_required,
        credential_kind: state.auth.credential_kind().as_str(),
        password_configured: password_configured(&saved.system.data_dir)?,
        config_path: state.config_path.display().to_string(),
        data_dir: saved.system.data_dir.display().to_string(),
        workspace_dir: saved.system.workspace_dir.display().to_string(),
        transport_encryption: "operator_managed",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;

    fn state() -> (AppState, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap().keep();
        let config_path = root.join("xpressclaw.yaml");
        let mut config = Config::default();
        config.system.data_dir = root.clone();
        config.system.workspace_dir = root.join("workspaces");
        config.save(&config_path).unwrap();
        let effective = config.instance.clone();
        let state = AppState::new_with_instance(
            Arc::new(config),
            Arc::new(Database::open_memory().unwrap()),
            None,
            config_path,
            true,
            effective,
            None,
        )
        .unwrap();
        (state, root)
    }

    async fn put(state: AppState, body: serde_json::Value) -> Response {
        routes()
            .with_state(state)
            .oneshot(
                Request::put("/")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn non_loopback_no_auth_requires_and_persists_explicit_acknowledgement() {
        let (state, _) = state();
        let body = serde_json::json!({
            "bind": "0.0.0.0",
            "port": 8935,
            "authentication_enabled": false,
            "acknowledge_unauthenticated_remote": false,
        });
        assert_eq!(
            put(state.clone(), body).await.status(),
            StatusCode::BAD_REQUEST
        );

        let accepted = put(
            state.clone(),
            serde_json::json!({
                "bind": "0.0.0.0",
                "port": 9443,
                "authentication_enabled": false,
                "acknowledge_unauthenticated_remote": true,
            }),
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&accepted.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["effective"]["bind"], "127.0.0.1");
        assert_eq!(body["saved"]["bind"], "0.0.0.0");
        assert_eq!(body["saved"]["port"], 9443);
        assert_eq!(body["restart_required"], true);
        let saved = Config::load(&state.config_path).unwrap();
        assert!(saved.instance.allow_unauthenticated_remote);
    }

    #[tokio::test]
    async fn password_updates_store_only_a_verifier_and_never_return_it() {
        let (state, root) = state();
        let response = put(
            state.clone(),
            serde_json::json!({
                "bind": "127.0.0.1",
                "port": 8935,
                "authentication_enabled": true,
                "acknowledge_unauthenticated_remote": false,
                "password": "a durable test password",
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response_text = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(!response_text.contains("durable test password"));
        assert!(!response_text.contains("argon2"));

        let secret = std::fs::read_to_string(root.join("instance-auth.json")).unwrap();
        assert!(secret.contains("argon2"));
        assert!(!secret.contains("durable test password"));
        assert!(password_configured(&root).unwrap());
        let yaml = std::fs::read_to_string(&state.config_path).unwrap();
        assert!(!yaml.contains("argon2"));
        assert!(!yaml.contains("durable test password"));
    }

    #[tokio::test]
    async fn settings_password_payloads_have_a_dedicated_small_limit() {
        let (state, _) = state();
        let response = put(
            state,
            serde_json::json!({
                "bind": "127.0.0.1",
                "port": 8935,
                "authentication_enabled": true,
                "acknowledge_unauthenticated_remote": false,
                "password": "x".repeat(9 * 1024),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
