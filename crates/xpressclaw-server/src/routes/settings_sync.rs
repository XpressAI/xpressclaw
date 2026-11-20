use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::config;
use xpressclaw_core::error::Error;
use xpressclaw_core::llm::router::LlmRouter;
use xpressclaw_core::projects::{Project, ProjectManager};
use xpressclaw_core::sync::{self, ProjectSyncManifest, SnapshotCounts, MANIFEST_FILE};
use xpressclaw_core::workers::acp::AcpInterruptMode;
use xpressclaw_core::workers::native::resolved_workspace;

use crate::state::AppState;

type ApiError = (StatusCode, Json<Value>);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_project_sync))
        .route("/{project_id}/fetch", post(fetch_project))
        .route("/{project_id}/publish", post(publish_project))
}

#[derive(Debug, Clone, Serialize)]
struct ProjectSyncStatus {
    project_id: String,
    project_name: String,
    project_icon: Option<String>,
    status: &'static str,
    project_dir: Option<String>,
    remote: Option<String>,
    branch: Option<String>,
    store_path: Option<String>,
    share_project_memory: Option<bool>,
    last_commit: Option<String>,
    last_synced_at: Option<String>,
    message: Option<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FetchRequest {
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Serialize)]
struct ProjectSyncAction {
    action: &'static str,
    project_id: String,
    commit: String,
    counts: SnapshotCounts,
}

#[derive(Debug)]
struct SyncInspection {
    project_dir: Option<PathBuf>,
    manifest: Option<ProjectSyncManifest>,
    status: &'static str,
    message: Option<String>,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct ManifestReplicaGroup {
    manifest: ProjectSyncManifest,
    workspaces: Vec<PathBuf>,
}

async fn list_project_sync(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let projects = ProjectManager::new(state.db.clone())
        .list()
        .map_err(core_error)?;
    let statuses = projects
        .iter()
        .map(|project| project_status(&state, project))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "projects": statuses })))
}

async fn fetch_project(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    request: Option<Json<FetchRequest>>,
) -> Result<Json<ProjectSyncAction>, ApiError> {
    let _sync_guard = state.project_sync_lock.lock().await;
    let _config_guard = state.config_write_lock.lock().await;
    ProjectManager::new(state.db.clone())
        .ensure_accepting_work(&project_id)
        .map_err(core_error)?;
    let project_dir = resolved_sync_directory(&state, &project_id)?;
    let db = state.db.clone();
    let config_path = state.config_path.clone();
    let mut runtime_config = state.config().as_ref().clone();
    let force = request.map(|Json(request)| request.force).unwrap_or(false);
    let turn_controls = state.turn_controls.clone();
    let elicitations = state.elicitations.clone();

    let (outcome, mut updated_config) = tokio::task::spawn_blocking(move || {
        let outcome = sync::fetch_with_interrupt_handler(
            &db,
            &mut runtime_config,
            &config_path,
            &project_dir,
            force,
            |turn_ids| {
                for turn_id in turn_ids {
                    turn_controls.request_interrupt(turn_id, AcpInterruptMode::Immediate);
                    elicitations.cancel_attempt(turn_id);
                }
            },
        )?;
        Ok::<_, Error>((outcome, runtime_config))
    })
    .await
    .map_err(join_error)?
    .map_err(core_error)?;

    // fetch deliberately reloads the file-backed configuration so environment
    // credentials are never written to disk. Restore those runtime-only
    // overrides before applying the synchronized configuration live.
    config::env_overrides(&mut updated_config);
    let updated_config = Arc::new(updated_config);
    let router = Arc::new(LlmRouter::build_from_config(&updated_config));
    state.apply_config(updated_config, Some(router));

    Ok(Json(action("fetch", outcome)))
}

async fn publish_project(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<ProjectSyncAction>, ApiError> {
    let _sync_guard = state.project_sync_lock.lock().await;
    let _config_guard = state.config_write_lock.lock().await;
    ProjectManager::new(state.db.clone())
        .ensure_accepting_work(&project_id)
        .map_err(core_error)?;
    let project_dir = resolved_sync_directory(&state, &project_id)?;
    let db = state.db.clone();
    let config = state.config();
    let outcome = tokio::task::spawn_blocking(move || sync::publish(&db, &config, &project_dir))
        .await
        .map_err(join_error)?
        .map_err(core_error)?;
    Ok(Json(action("publish", outcome)))
}

fn action(action: &'static str, outcome: sync::SyncOutcome) -> ProjectSyncAction {
    ProjectSyncAction {
        action,
        project_id: outcome.project_id,
        commit: outcome.commit,
        counts: outcome.counts,
    }
}

fn project_status(state: &AppState, project: &Project) -> ProjectSyncStatus {
    let inspection = inspect_project(state, &project.id).unwrap_or_else(|error| SyncInspection {
        project_dir: None,
        manifest: None,
        status: "error",
        message: Some(error.to_string()),
        warnings: Vec::new(),
    });
    let (last_commit, last_synced_at) = inspection
        .manifest
        .as_ref()
        .and_then(|manifest| last_sync_metadata(state, manifest))
        .map(|(commit, synced_at)| (Some(commit), Some(synced_at)))
        .unwrap_or((None, None));

    ProjectSyncStatus {
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        project_icon: project.icon.clone(),
        status: inspection.status,
        project_dir: inspection
            .project_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        remote: inspection
            .manifest
            .as_ref()
            .map(|manifest| manifest.store.remote.clone()),
        branch: inspection
            .manifest
            .as_ref()
            .map(|manifest| manifest.store.branch.clone()),
        store_path: inspection
            .manifest
            .as_ref()
            .map(|manifest| manifest.store.path.clone()),
        share_project_memory: inspection
            .manifest
            .as_ref()
            .map(|manifest| manifest.share.project_memory),
        last_commit,
        last_synced_at,
        message: inspection.message,
        warnings: inspection.warnings,
    }
}

fn inspect_project(state: &AppState, project_id: &str) -> Result<SyncInspection, Error> {
    let workspaces = project_workspaces(state, project_id)?;
    let mut groups = Vec::<ManifestReplicaGroup>::new();
    let mut manifest_errors = Vec::new();
    let mut missing_manifests = Vec::new();
    let mut unavailable_workspaces = Vec::new();

    for workspace in &workspaces {
        if !workspace.is_dir() {
            unavailable_workspaces.push(workspace.clone());
            continue;
        }
        let manifest_path = workspace.join(MANIFEST_FILE);
        if !manifest_path.exists() {
            missing_manifests.push(workspace.clone());
            continue;
        }
        match ProjectSyncManifest::load(workspace) {
            Ok(manifest) if manifest.project_id == project_id => {
                if let Some(group) = groups.iter_mut().find(|group| group.manifest == manifest) {
                    group.workspaces.push(workspace.clone());
                } else {
                    groups.push(ManifestReplicaGroup {
                        manifest,
                        workspaces: vec![workspace.clone()],
                    });
                }
            }
            Ok(manifest) => manifest_errors.push(format!(
                "{} belongs to Project '{}' instead of this Project",
                manifest_path.display(),
                manifest.project_id
            )),
            Err(error) => manifest_errors.push(format!(
                "{} could not be loaded: {error}",
                manifest_path.display()
            )),
        }
    }

    groups.sort_by(|left, right| left.workspaces[0].cmp(&right.workspaces[0]));
    let warnings = discovery_warnings(
        &manifest_errors,
        &missing_manifests,
        &unavailable_workspaces,
    );

    if groups.len() == 1 {
        let group = groups.remove(0);
        return Ok(SyncInspection {
            // project_workspaces canonicalizes existing paths and returns them
            // in lexical order. Every replica in this group has the same
            // parsed configuration, so the first path is a stable source for
            // both inspection and explicit Fetch/Publish operations.
            project_dir: group.workspaces.first().cloned(),
            manifest: Some(group.manifest),
            status: "ready",
            message: None,
            warnings,
        });
    }
    if groups.len() > 1 {
        return Ok(SyncInspection {
            project_dir: None,
            manifest: None,
            status: "conflict",
            message: Some(manifest_conflict_message(&groups)),
            warnings,
        });
    }
    if !manifest_errors.is_empty() {
        return Ok(SyncInspection {
            project_dir: workspaces.first().cloned(),
            manifest: None,
            status: "error",
            message: Some(format!(
                "No usable {MANIFEST_FILE} was found for this Project. {} Fix the listed manifest or run `xpressclaw sync init` in an assigned workspace.",
                manifest_errors.join("; ")
            )),
            warnings: discovery_warnings(&[], &missing_manifests, &unavailable_workspaces),
        });
    }
    if workspaces.is_empty() {
        return Ok(SyncInspection {
            project_dir: None,
            manifest: None,
            status: "unavailable",
            message: Some(
                "Assign an Agent with a local workspace before configuring Project sync."
                    .to_string(),
            ),
            warnings: Vec::new(),
        });
    }
    if unavailable_workspaces.len() == workspaces.len() {
        return Ok(SyncInspection {
            project_dir: None,
            manifest: None,
            status: "unavailable",
            message: Some(format!(
                "The assigned Agent workspaces are unavailable: {}. Restore a workspace or update the Agent workspace path, then refresh.",
                display_paths(&unavailable_workspaces)
            )),
            warnings: Vec::new(),
        });
    }

    let available_workspaces = workspaces
        .iter()
        .filter(|workspace| workspace.is_dir())
        .cloned()
        .collect::<Vec<_>>();
    let one_workspace = (available_workspaces.len() == 1).then(|| available_workspaces[0].clone());
    Ok(SyncInspection {
        project_dir: one_workspace,
        manifest: None,
        status: "unconfigured",
        message: Some(if workspaces.len() == 1 {
            format!(
                "No {MANIFEST_FILE} was found in this Project workspace. Run `xpressclaw sync init` there first."
            )
        } else {
            format!(
                "No matching {MANIFEST_FILE} was found across {} Agent workspaces.",
                workspaces.len()
            )
        }),
        warnings: discovery_warnings(&[], &[], &unavailable_workspaces),
    })
}

fn discovery_warnings(
    manifest_errors: &[String],
    missing_manifests: &[PathBuf],
    unavailable_workspaces: &[PathBuf],
) -> Vec<String> {
    let mut warnings = manifest_errors
        .iter()
        .map(|error| format!("Ignored {error}."))
        .collect::<Vec<_>>();
    if !missing_manifests.is_empty() {
        warnings.push(format!(
            "No {MANIFEST_FILE} was found in these assigned Agent workspaces: {}.",
            display_paths(missing_manifests)
        ));
    }
    if !unavailable_workspaces.is_empty() {
        warnings.push(format!(
            "These assigned Agent workspaces are unavailable: {}.",
            display_paths(unavailable_workspaces)
        ));
    }
    warnings
}

fn manifest_conflict_message(groups: &[ManifestReplicaGroup]) -> String {
    let fields = differing_manifest_fields(groups).join(", ");
    let configurations = groups
        .iter()
        .map(|group| {
            let manifest = &group.manifest;
            format!(
                "- version={}, remote={}, branch={}, store path={}, project memory={}: {}",
                manifest.version,
                manifest.store.remote,
                manifest.store.branch,
                manifest.store.path,
                if manifest.share.project_memory {
                    "included"
                } else {
                    "local only"
                },
                display_paths(&group.workspaces)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Project sync configuration conflict. Differing fields: {fields}. Conflicting manifest groups:\n{configurations}\nMake the listed {MANIFEST_FILE} copies identical, then refresh."
    )
}

fn differing_manifest_fields(groups: &[ManifestReplicaGroup]) -> Vec<&'static str> {
    let first = &groups[0].manifest;
    let mut fields = Vec::new();
    if groups
        .iter()
        .skip(1)
        .any(|group| group.manifest.version != first.version)
    {
        fields.push("version");
    }
    if groups
        .iter()
        .skip(1)
        .any(|group| group.manifest.project_id != first.project_id)
    {
        fields.push("project_id");
    }
    if groups
        .iter()
        .skip(1)
        .any(|group| group.manifest.store.remote != first.store.remote)
    {
        fields.push("store.remote");
    }
    if groups
        .iter()
        .skip(1)
        .any(|group| group.manifest.store.branch != first.store.branch)
    {
        fields.push("store.branch");
    }
    if groups
        .iter()
        .skip(1)
        .any(|group| group.manifest.store.path != first.store.path)
    {
        fields.push("store.path");
    }
    if groups
        .iter()
        .skip(1)
        .any(|group| group.manifest.share.project_memory != first.share.project_memory)
    {
        fields.push("share.project_memory");
    }
    if fields.is_empty() {
        fields.push("other effective settings");
    }
    fields
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn project_workspaces(state: &AppState, project_id: &str) -> Result<Vec<PathBuf>, Error> {
    let records = AgentRegistry::new(state.db.clone()).list()?;
    let names = records
        .iter()
        .filter(|record| record.project_id.as_deref() == Some(project_id))
        .map(|record| record.id.as_str())
        .collect::<HashSet<_>>();
    let config = state.config();
    let workspaces = config
        .agents
        .iter()
        .filter(|agent| names.contains(agent.name.as_str()))
        .map(|agent| resolved_workspace(&config, agent))
        .collect::<BTreeSet<_>>();
    Ok(workspaces.into_iter().collect())
}

fn resolved_sync_directory(state: &AppState, project_id: &str) -> Result<PathBuf, ApiError> {
    ProjectManager::new(state.db.clone())
        .get(project_id)
        .map_err(core_error)?;
    let inspection = inspect_project(state, project_id).map_err(core_error)?;
    match (inspection.project_dir, inspection.manifest) {
        (Some(project_dir), Some(_)) => Ok(project_dir),
        _ => Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": inspection.message.unwrap_or_else(|| "Project sync is not configured".to_string())
            })),
        )),
    }
}

fn last_sync_metadata(
    state: &AppState,
    manifest: &ProjectSyncManifest,
) -> Option<(String, String)> {
    state
        .db
        .conn()
        .query_row(
            "SELECT last_commit, updated_at FROM project_sync_state
             WHERE project_id = ?1 AND remote = ?2 AND branch = ?3 AND store_path = ?4",
            [
                manifest.project_id.as_str(),
                manifest.store.remote.as_str(),
                manifest.store.branch.as_str(),
                manifest.store.path.as_str(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
}

fn core_error(error: Error) -> ApiError {
    let status = match &error {
        Error::ProjectNotFound { .. } => StatusCode::NOT_FOUND,
        Error::Sync(_) | Error::Project(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": error.to_string() })))
}

fn join_error(error: tokio::task::JoinError) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": format!("Project sync worker failed: {error}") })),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use xpressclaw_core::config::{AgentConfig, Config, NativeRunnerConfig};
    use xpressclaw_core::db::Database;

    use super::*;

    fn test_state(control_plane_dir: &Path, workspaces: &[(&str, &Path)]) -> AppState {
        let db = Arc::new(Database::open_memory().unwrap());
        let connection = db.conn();
        connection
            .execute(
                "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One')",
                [],
            )
            .unwrap();
        for (agent_id, _) in workspaces {
            connection
                .execute(
                    "INSERT INTO agents (id, name, backend, config, project_id)
                     VALUES (?1, ?2, 'codex', '{}', 'project-one')",
                    [*agent_id, *agent_id],
                )
                .unwrap();
        }
        drop(connection);
        let config = Config {
            agents: workspaces
                .iter()
                .map(|(agent_id, workspace)| AgentConfig {
                    name: (*agent_id).into(),
                    backend: "codex".into(),
                    runner: NativeRunnerConfig {
                        kind: "codex".into(),
                        workspace: Some(workspace.display().to_string()),
                        ..NativeRunnerConfig::default()
                    },
                    ..AgentConfig::default()
                })
                .collect(),
            ..Config::default()
        };
        let config_path = control_plane_dir.join("xpressclaw.yaml");
        config.save(&config_path).unwrap();
        AppState::new(Arc::new(config), db, None, config_path, true)
    }

    fn test_manifest(remote: &Path) -> ProjectSyncManifest {
        ProjectSyncManifest::new(
            "project-one",
            remote.display().to_string(),
            "main",
            "projects/project-one",
        )
        .unwrap()
    }

    fn initialize_bare_remote(remote: &Path) {
        fs::create_dir_all(remote).unwrap();
        assert!(Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .current_dir(remote)
            .status()
            .unwrap()
            .success());
    }

    fn resolved_test_workspace(state: &AppState, agent_id: &str) -> PathBuf {
        let config = state.config();
        let agent = config
            .agents
            .iter()
            .find(|agent| agent.name == agent_id)
            .unwrap();
        resolved_workspace(&config, agent)
    }

    #[tokio::test]
    async fn lists_one_configured_project_and_publishes() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let remote = root.path().join("remote.git");
        fs::create_dir_all(&workspace).unwrap();
        initialize_bare_remote(&remote);

        let state = test_state(root.path(), &[("agent-one", &workspace)]);
        sync::initialize(
            &state.db,
            &workspace,
            "project-one",
            &remote.display().to_string(),
            "main",
            "projects/project-one",
            true,
        )
        .unwrap();
        let app = Router::new()
            .nest("/api/settings/sync", routes())
            .with_state(state);

        let response = app
            .clone()
            .oneshot(
                Request::get("/api/settings/sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["projects"][0]["status"], "ready");
        assert_eq!(body["projects"][0]["branch"], "main");
        assert!(body["projects"][0]["last_commit"].is_null());

        let response = app
            .clone()
            .oneshot(
                Request::post("/api/settings/sync/project-one/publish")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["action"], "publish");
        assert_eq!(body["project_id"], "project-one");
        assert_eq!(body["counts"]["agents"], 1);

        let response = app
            .oneshot(
                Request::get("/api/settings/sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(body["projects"][0]["last_commit"].is_string());
        assert!(body["projects"][0]["last_synced_at"].is_string());
    }

    #[tokio::test]
    async fn equivalent_manifest_replicas_are_ready_and_support_fetch_and_publish() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("a-workspace");
        let second = root.path().join("z-workspace");
        let remote = root.path().join("remote.git");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        initialize_bare_remote(&remote);

        // Deliberately register the lexical-last workspace first. Discovery
        // must not depend on Agent configuration order.
        let state = test_state(root.path(), &[("agent-z", &second), ("agent-a", &first)]);
        sync::initialize(
            &state.db,
            &second,
            "project-one",
            &remote.display().to_string(),
            "main",
            "projects/project-one",
            true,
        )
        .unwrap();
        let manifest = ProjectSyncManifest::load(&second).unwrap();
        manifest.save_new(&first).unwrap();
        let manifest_path = first.join(MANIFEST_FILE);
        let mut yaml = fs::read_to_string(&manifest_path).unwrap();
        yaml = yaml
            .replace("  branch: main\n", "")
            .replace("share:\n  project_memory: true\n", "");
        fs::write(
            manifest_path,
            format!("# replica with defaults omitted\n{yaml}"),
        )
        .unwrap();

        let inspection = inspect_project(&state, "project-one").unwrap();
        assert_eq!(inspection.status, "ready");
        assert_eq!(
            inspection.project_dir,
            Some(resolved_test_workspace(&state, "agent-a"))
        );
        assert!(inspection.warnings.is_empty());

        let app = Router::new()
            .nest("/api/settings/sync", routes())
            .with_state(state);
        let publish = app
            .clone()
            .oneshot(
                Request::post("/api/settings/sync/project-one/publish")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(publish.status(), StatusCode::OK);

        let fetch = app
            .oneshot(
                Request::post("/api/settings/sync/project-one/fetch")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetch.status(), StatusCode::OK);
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_and_symlink_workspace_paths_are_one_source() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let workspace_link = root.path().join("workspace-link");
        let remote = root.path().join("remote.git");
        fs::create_dir_all(&workspace).unwrap();
        symlink(&workspace, &workspace_link).unwrap();
        test_manifest(&remote).save_new(&workspace).unwrap();
        let state = test_state(
            root.path(),
            &[
                ("agent-direct", &workspace),
                ("agent-link", &workspace_link),
            ],
        );

        let workspaces = project_workspaces(&state, "project-one").unwrap();
        assert_eq!(workspaces, vec![workspace.canonicalize().unwrap()]);
        let inspection = inspect_project(&state, "project-one").unwrap();
        assert_eq!(inspection.status, "ready");
        assert!(inspection.warnings.is_empty());
    }

    #[test]
    fn conflicting_manifests_report_fields_and_grouped_paths() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        let third = root.path().join("third");
        let remote = root.path().join("remote.git");
        for workspace in [&first, &second, &third] {
            fs::create_dir_all(workspace).unwrap();
        }
        let main = test_manifest(&remote);
        main.save_new(&first).unwrap();
        main.save_new(&second).unwrap();
        let mut release = test_manifest(&remote);
        release.store.branch = "release".into();
        release.share.project_memory = false;
        release.save_new(&third).unwrap();
        let state = test_state(
            root.path(),
            &[
                ("agent-three", &third),
                ("agent-two", &second),
                ("agent-one", &first),
            ],
        );

        let inspection = inspect_project(&state, "project-one").unwrap();
        assert_eq!(inspection.status, "conflict");
        assert!(inspection.project_dir.is_none());
        let message = inspection.message.unwrap();
        let first_workspace = resolved_test_workspace(&state, "agent-one");
        let second_workspace = resolved_test_workspace(&state, "agent-two");
        let third_workspace = resolved_test_workspace(&state, "agent-three");
        assert!(message.contains("Differing fields: store.branch, share.project_memory"));
        assert!(message.contains(&first_workspace.display().to_string()));
        assert!(message.contains(&second_workspace.display().to_string()));
        assert!(message.contains(&third_workspace.display().to_string()));
        let main_group = format!(
            "project memory=included: {}, {}",
            first_workspace.display(),
            second_workspace.display()
        );
        assert!(message.contains(&main_group));
    }

    #[test]
    fn valid_manifest_survives_invalid_wrong_project_and_missing_workspaces() {
        let root = tempfile::tempdir().unwrap();
        let valid = root.path().join("valid");
        let invalid = root.path().join("invalid");
        let wrong = root.path().join("wrong");
        let missing = root.path().join("missing");
        let unavailable = root.path().join("unavailable");
        let remote = root.path().join("remote.git");
        for workspace in [&valid, &invalid, &wrong, &missing] {
            fs::create_dir_all(workspace).unwrap();
        }
        test_manifest(&remote).save_new(&valid).unwrap();
        fs::write(invalid.join(MANIFEST_FILE), "not: [valid").unwrap();
        ProjectSyncManifest::new(
            "another-project",
            remote.display().to_string(),
            "main",
            "projects/another-project",
        )
        .unwrap()
        .save_new(&wrong)
        .unwrap();
        let state = test_state(
            root.path(),
            &[
                ("agent-unavailable", &unavailable),
                ("agent-wrong", &wrong),
                ("agent-missing", &missing),
                ("agent-invalid", &invalid),
                ("agent-valid", &valid),
            ],
        );

        let inspection = inspect_project(&state, "project-one").unwrap();
        assert_eq!(inspection.status, "ready");
        assert_eq!(
            inspection.project_dir,
            Some(resolved_test_workspace(&state, "agent-valid"))
        );
        let warnings = inspection.warnings.join("\n");
        let invalid_workspace = resolved_test_workspace(&state, "agent-invalid");
        let wrong_workspace = resolved_test_workspace(&state, "agent-wrong");
        let missing_workspace = resolved_test_workspace(&state, "agent-missing");
        let unavailable_workspace = resolved_test_workspace(&state, "agent-unavailable");
        assert!(warnings.contains(&invalid_workspace.join(MANIFEST_FILE).display().to_string()));
        assert!(warnings.contains(&wrong_workspace.join(MANIFEST_FILE).display().to_string()));
        assert!(warnings.contains("belongs to Project 'another-project'"));
        assert!(warnings.contains(&missing_workspace.display().to_string()));
        assert!(warnings.contains(&unavailable_workspace.display().to_string()));
    }
}
