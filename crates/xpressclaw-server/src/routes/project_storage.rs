use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use xpressclaw_core::{
    error::{Error, Result},
    projects::ProjectManager,
    storage::{self, RetentionPolicy, WorktreeInventory},
    workers::native::resolved_workspace,
};

type ApiError = (StatusCode, Json<Value>);
fn failure(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(json!({"error":error.to_string()})),
    )
}
fn browser(headers: &HeaderMap) -> std::result::Result<(), ApiError> {
    super::workspace::require_same_origin(headers)?;
    if headers.contains_key("x-xpressclaw-agent-id") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Storage cleanup requires the browser session"})),
        ));
    }
    Ok(())
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/{id}/storage", get(inventory).patch(update_policy))
        .route("/{id}/storage/cleanup", post(cleanup))
}

fn cleanup_agents(
    config: &xpressclaw_core::config::Config,
    project_agents: &[String],
) -> Vec<String> {
    let roots = config
        .agents
        .iter()
        .filter(|agent| project_agents.contains(&agent.name))
        .filter_map(|agent| resolved_workspace(config, agent).canonicalize().ok())
        .collect::<Vec<_>>();
    config
        .agents
        .iter()
        .filter(|agent| {
            project_agents.contains(&agent.name)
                || resolved_workspace(config, agent)
                    .canonicalize()
                    .is_ok_and(|path| {
                        roots
                            .iter()
                            .any(|root| path.starts_with(root) || root.starts_with(&path))
                    })
        })
        .map(|agent| agent.name.clone())
        .collect()
}

async fn scan(state: &AppState, id: &str, measure: bool) -> Result<WorktreeInventory> {
    let db = state.db.clone();
    let config = state.config();
    let project_id = id.to_owned();
    let mut report =
        tokio::task::spawn_blocking(move || storage::inventory(db, &config, &project_id, measure))
            .await
            .map_err(|error| Error::Backend(error.to_string()))??;
    let paths = report
        .worktrees
        .iter()
        .filter(|tree| tree.eligible)
        .map(|tree| tree.path.clone())
        .collect::<Vec<_>>();
    let docker = state.docker.read().unwrap().clone();
    let kept = if paths.is_empty() {
        Default::default()
    } else if let Some(docker) = docker {
        let project = ProjectManager::new(state.db.clone()).get(id)?;
        let inspected_agents = cleanup_agents(&state.config(), &project.agent_ids);
        match tokio::time::timeout(
            Duration::from_secs(30),
            docker.active_worktree_uses(&paths, &inspected_agents),
        )
        .await
        {
            Ok(Ok(kept)) => kept,
            _ => paths
                .into_iter()
                .map(|path| (path, "Container activity could not be checked".into()))
                .collect(),
        }
    } else {
        let project = ProjectManager::new(state.db.clone()).get(id)?;
        let registry = xpressclaw_core::agents::registry::AgentRegistry::new(state.db.clone());
        if project.agent_ids.iter().any(|agent| {
            registry
                .get(agent)
                .is_ok_and(|record| record.container_id.is_some())
        }) {
            paths.into_iter().map(|path| (path, "Container engine is unavailable; retained Agent activity could not be checked".into())).collect()
        } else {
            Default::default()
        }
    };
    if !kept.is_empty() {
        let mut saved = storage::load_state(&state.db, id)?;
        for tree in &mut report.worktrees {
            if let Some(reason) = kept.get(&tree.path) {
                tree.eligible = false;
                tree.eligible_since = None;
                tree.reasons.push(reason.clone());
                if let Some(observation) = saved.worktrees.get_mut(&tree.id) {
                    observation.eligible_since = None;
                }
            }
        }
        storage::save_state(&state.db, id, &saved)?;
    }
    Ok(report)
}

async fn inventory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, ApiError> {
    browser(&headers)?;
    let _guard = state.storage_lock.lock().await;
    let report = scan(&state, &id, true).await.map_err(failure)?;
    let project = ProjectManager::new(state.db.clone())
        .get(&id)
        .map_err(failure)?;
    let configured = state
        .config()
        .agents
        .iter()
        .map(|agent| agent.runner.image.clone())
        .collect::<Vec<_>>();
    let (engine, engine_error) = if let Some(docker) = state.docker().await {
        match tokio::time::timeout(
            Duration::from_secs(30),
            docker.project_storage(&project.agent_ids, &configured),
        )
        .await
        {
            Ok(Ok(report)) => (Some(report), None),
            Ok(Err(error)) => (None, Some(error.to_string())),
            Err(_) => (None, Some("Container storage inspection timed out".into())),
        }
    } else {
        (None, Some("Docker or Podman is not available".into()))
    };
    Ok(Json(
        json!({"worktrees":report.worktrees,"policy":report.policy,"scanned_at":report.scanned_at,"last_cleanup_at":report.last_cleanup_at,"errors":report.errors,"engine":engine,"engine_error":engine_error}),
    ))
}

async fn update_policy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(policy): Json<RetentionPolicy>,
) -> std::result::Result<Json<Value>, ApiError> {
    browser(&headers)?;
    policy.validate().map_err(failure)?;
    let _guard = state.storage_lock.lock().await;
    let project = ProjectManager::new(state.db.clone())
        .get(&id)
        .map_err(failure)?;
    if project.deletion_started_at.is_some() {
        return Err(failure("Project deletion is in progress"));
    }
    let mut saved = storage::load_state(&state.db, &id).map_err(failure)?;
    saved.policy = policy;
    storage::save_state(&state.db, &id, &saved).map_err(failure)?;
    Ok(Json(json!(saved.policy)))
}

#[derive(Deserialize)]
struct CleanupRequest {
    #[serde(default)]
    worktrees: BTreeMap<String, String>,
    #[serde(default)]
    recovery_images: Vec<String>,
}
#[derive(Default, Serialize)]
struct CleanupResult {
    removed: Vec<String>,
    kept: Vec<Kept>,
    finished_at: i64,
}
#[derive(Serialize)]
struct Kept {
    id: String,
    reason: String,
}

async fn cleanup(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CleanupRequest>,
) -> std::result::Result<Json<Value>, ApiError> {
    browser(&headers)?;
    if request.worktrees.len() + request.recovery_images.len() > 500 {
        return Err(failure("Select at most 500 items per cleanup"));
    }
    let result = perform_cleanup(state, id, Some(request))
        .await
        .map_err(failure)?;
    Ok(Json(json!(result)))
}

async fn perform_cleanup(
    state: AppState,
    id: String,
    request: Option<CleanupRequest>,
) -> Result<CleanupResult> {
    // The operation owns its locks even if the browser disconnects while Git
    // is removing files. A dropped HTTP response cannot reopen the runtime.
    tokio::spawn(async move {
        let _storage = state.storage_lock.clone().lock_owned().await;
        let _config = state.config_write_lock.clone().lock_owned().await;
        let project = ProjectManager::new(state.db.clone()).get(&id)?;
        if project.deletion_started_at.is_some() { return Err(Error::Project("Project deletion is in progress".into())); }
        let config = state.config();
        let mut leases = Vec::new();
        for agent in cleanup_agents(&config, &project.agent_ids) {
            leases.push(state.native_runtime_lifecycle.try_quiesce_agent(&agent).ok_or_else(|| Error::Agent(format!("{agent} is busy. Cleanup will keep its files until its current work finishes.")))?);
        }
        let report = scan(&state, &id, false).await?;
        let selected = if let Some(request) = &request { request.worktrees.clone() } else {
            if !report.policy.enabled { return Ok(CleanupResult::default()); }
            let cutoff = report.scanned_at - i64::from(report.policy.days) * 86_400;
            report.worktrees.iter().filter(|tree| tree.eligible && tree.path.starts_with(tree.repository.join(".worktrees")) && tree.eligible_since.is_some_and(|since| since <= cutoff)).map(|tree| (tree.id.clone(), tree.fingerprint.clone())).collect()
        };
        let (mut result, _leases) = tokio::task::spawn_blocking(move || {
            let mut result = CleanupResult::default();
            for (id, fingerprint) in selected {
                match report.worktrees.iter().find(|tree| tree.id == id) {
                    Some(tree) if tree.fingerprint == fingerprint && tree.eligible => match storage::remove_worktree(tree) {
                        Ok(()) => result.removed.push(id),
                        Err(error) => result.kept.push(Kept { id, reason: error.to_string() }),
                    },
                    Some(tree) => result.kept.push(Kept { id, reason: if tree.eligible { "Worktree changed since the preview. Refresh the inventory.".into() } else { tree.reasons.join("; ") } }),
                    None => result.kept.push(Kept { id, reason: "Worktree is no longer in this Project's inventory".into() }),
                }
            }
            (result, leases)
        }).await.map_err(|error| Error::Backend(error.to_string()))?;
        if let Some(request) = request.filter(|request| !request.recovery_images.is_empty()) {
            let inspection = async {
                let docker = state.docker().await.ok_or_else(|| Error::DockerNotAvailable("Docker or Podman is not available".into()))?;
                let configured = config.agents.iter().map(|agent| agent.runner.image.clone()).collect::<Vec<_>>();
                let engine = docker.project_storage(&project.agent_ids, &configured).await?;
                Ok::<_, Error>((docker, engine))
            }.await;
            match inspection {
                Ok((docker, engine)) => for id in request.recovery_images {
                    match engine.recovery_images.iter().find(|image| image.id == id) {
                        Some(image) if image.eligible => match docker.remove_recovery_image(image).await {
                            Ok(()) => result.removed.push(id),
                            Err(error) => result.kept.push(Kept { id, reason: error.to_string() }),
                        },
                        Some(image) => result.kept.push(Kept { id, reason: image.reasons.join("; ") }),
                        None => result.kept.push(Kept { id, reason: "Image is not an owned recovery snapshot for this Project".into() }),
                    }
                },
                Err(error) => for id in request.recovery_images {
                    result.kept.push(Kept { id, reason: error.to_string() });
                }
            }
        }
        result.finished_at = chrono::Utc::now().timestamp();
        let mut saved = storage::load_state(&state.db, &id)?;
        saved.last_cleanup_at = Some(result.finished_at);
        for removed in &result.removed { saved.worktrees.remove(removed); }
        storage::save_state(&state.db, &id, &saved)?;
        Ok(result)
    }).await.map_err(|error| Error::Backend(error.to_string()))?
}

pub(crate) async fn run_retention(state: AppState, shutdown: tokio_util::sync::CancellationToken) {
    let mut interval = tokio::time::interval(Duration::from_secs(3600));
    // Startup first recovers interrupted runtimes; retention starts on the
    // first hourly tick after that startup sequence has completed.
    interval.tick().await;
    loop {
        tokio::select! { _ = shutdown.cancelled() => break, _ = interval.tick() => {} }
        let Ok(projects) = ProjectManager::new(state.db.clone()).list() else {
            continue;
        };
        for project in projects {
            if shutdown.is_cancelled() {
                return;
            }
            if storage::load_state(&state.db, &project.id).is_ok_and(|state| state.policy.enabled) {
                match perform_cleanup(state.clone(), project.id.clone(), None).await {
                    Ok(result) if !result.removed.is_empty() => tracing::info!(
                        project_id = project.id,
                        removed = result.removed.len(),
                        "removed expired project worktrees"
                    ),
                    Ok(_) => {}
                    Err(error) => {
                        tracing::debug!(project_id = project.id, %error, "project cleanup deferred")
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use std::{path::Path as FilePath, process::Command, sync::Arc};
    use tower::ServiceExt;
    use xpressclaw_core::{
        agents::registry::AgentRegistry,
        config::{AgentConfig, Config, NativeRunnerConfig},
        db::Database,
    };

    fn git(root: &FilePath, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn fixture() -> (tempfile::TempDir, AppState) {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        git(
            root.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(root.path(), &["config", "user.name", "Cleanup test"]);
        std::fs::write(root.path().join(".gitignore"), ".worktrees/\n.env\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("agent", "custom")
            .unwrap();
        let config = Config {
            agents: vec![AgentConfig {
                name: "agent".into(),
                runner: NativeRunnerConfig {
                    workspace: Some(root.path().display().to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let state = AppState::new(
            Arc::new(config),
            db,
            None,
            root.path().join("config.yaml"),
            true,
        );
        (root, state)
    }
    #[tokio::test]
    async fn retention_waits_for_idle_agents_and_only_removes_expired_worktrees() {
        let (root, state) = fixture();
        for branch in ["old", "recent"] {
            git(
                root.path(),
                &[
                    "worktree",
                    "add",
                    "-b",
                    branch,
                    &format!(".worktrees/{branch}"),
                ],
            );
        }
        let report = scan(&state, "agent", false).await.unwrap();
        let old = report
            .worktrees
            .iter()
            .find(|tree| tree.branch.as_deref() == Some("old"))
            .unwrap();
        let recent = report
            .worktrees
            .iter()
            .find(|tree| tree.branch.as_deref() == Some("recent"))
            .unwrap();
        let mut saved = storage::load_state(&state.db, "agent").unwrap();
        saved.policy.enabled = true;
        saved.worktrees.get_mut(&old.id).unwrap().eligible_since =
            Some(report.scanned_at - 15 * 86_400);
        storage::save_state(&state.db, "agent", &saved).unwrap();
        let busy = state
            .native_runtime_lifecycle
            .try_quiesce_agent("agent")
            .unwrap();
        assert!(perform_cleanup(state.clone(), "agent".into(), None)
            .await
            .is_err());
        assert!(old.path.exists());
        drop(busy);
        let result = perform_cleanup(state.clone(), "agent".into(), None)
            .await
            .unwrap();
        assert_eq!(result.removed.as_slice(), std::slice::from_ref(&old.id));
        assert!(!old.path.exists());
        assert!(recent.path.exists());
        std::fs::write(recent.path.join(".env"), "private fixture").unwrap();
        let result = perform_cleanup(
            state,
            "agent".into(),
            Some(CleanupRequest {
                worktrees: BTreeMap::from([
                    (recent.id.clone(), recent.fingerprint.clone()),
                    ("foreign-worktree".into(), "preview".into()),
                ]),
                recovery_images: vec![],
            }),
        )
        .await
        .unwrap();
        assert!(result.removed.is_empty());
        assert_eq!(result.kept.len(), 2);
        assert!(recent.path.join(".env").exists());
    }

    #[tokio::test]
    async fn retention_policy_is_local_browser_only_and_validated() {
        let (_root, state) = fixture();
        let app = Router::new()
            .nest("/api/projects", routes())
            .with_state(state.clone());
        for (caller, origin, days, expected) in [
            (Some("agent"), None, 7, StatusCode::FORBIDDEN),
            (
                None,
                Some("https://example.invalid"),
                7,
                StatusCode::FORBIDDEN,
            ),
            (None, None, 0, StatusCode::CONFLICT),
            (None, None, 7, StatusCode::OK),
        ] {
            let mut request = Request::builder()
                .method("PATCH")
                .uri("/api/projects/agent/storage")
                .header("host", "localhost")
                .header("content-type", "application/json");
            if let Some(caller) = caller {
                request = request.header("x-xpressclaw-agent-id", caller);
            }
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = app
                .clone()
                .oneshot(
                    request
                        .body(Body::from(json!({"enabled":true,"days":days}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
        let saved = storage::load_state(&state.db, "agent").unwrap();
        assert!(saved.policy.enabled);
        assert_eq!(saved.policy.days, 7);
    }
}
