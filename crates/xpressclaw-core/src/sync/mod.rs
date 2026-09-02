//! Explicit Git-backed synchronization for portable Project collaboration data.
//!
//! This module is intentionally not called by control-plane startup, Project
//! updates, or worker lifecycle code. A caller must explicitly invoke fetch or
//! publish, keeping the shared store independent from the main project's VCS.

mod git;
mod manifest;
mod model;
mod state;

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::Config;
use crate::db::Database;
use crate::error::{Error, Result};

use git::GitCheckout;
pub use manifest::{GitStoreConfig, ProjectSyncManifest, ShareConfig, MANIFEST_FILE};
use model::PortableSnapshot;
pub use model::SnapshotCounts;

#[derive(Debug, Clone, Serialize)]
pub struct InitializeOutcome {
    pub manifest_path: PathBuf,
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncOutcome {
    pub project_id: String,
    pub commit: String,
    pub counts: SnapshotCounts,
    #[serde(skip_serializing)]
    pub interrupted_conversation_turn_ids: Vec<String>,
}

/// Create the portable pointer manifest. This performs no network operation
/// and does not require the main project directory to be a Git checkout.
pub fn initialize(
    db: &Database,
    project_dir: &Path,
    project_id: &str,
    remote: &str,
    branch: &str,
    store_path: &str,
    share_project_memory: bool,
) -> Result<InitializeOutcome> {
    if !state::project_exists(db, project_id)? {
        return Err(Error::Sync(format!(
            "Project '{project_id}' does not exist locally; create or fetch it before initializing synchronization"
        )));
    }
    let mut manifest = ProjectSyncManifest::new(project_id, remote, branch, store_path)?;
    manifest.share.project_memory = share_project_memory;
    let manifest_path = manifest.save_new(project_dir)?;
    Ok(InitializeOutcome {
        manifest_path,
        project_id: project_id.to_string(),
    })
}

/// Fetch and explicitly merge the configured remote snapshot into local state.
/// Existing local-only records are retained; `force` acknowledges a first-fetch
/// or two-sided-change merge, but never turns the operation into a destructive
/// replacement.
pub fn fetch(
    db: &Database,
    config: &mut Config,
    config_path: &Path,
    project_dir: &Path,
    force: bool,
) -> Result<SyncOutcome> {
    fetch_with_interrupt_handler(db, config, config_path, project_dir, force, |_| {})
}

/// Fetch while reporting live Conversation turns immediately after their
/// synchronized cancellation commits. The handler runs before fallible
/// post-import snapshot and bookkeeping work, so callers can always interrupt
/// an ACP process whose turn was cancelled by a remote tombstone.
pub fn fetch_with_interrupt_handler<F>(
    db: &Database,
    config: &mut Config,
    config_path: &Path,
    project_dir: &Path,
    force: bool,
    interrupt_handler: F,
) -> Result<SyncOutcome>
where
    F: FnOnce(&[String]),
{
    let manifest = ProjectSyncManifest::load(project_dir)?;
    if state::project_exists(db, &manifest.project_id)? {
        state::ensure_fetch_ready(db, &manifest.project_id)?;
    }

    let checkout = GitCheckout::open(&manifest.store, false)?;
    let commit = checkout
        .head()?
        .ok_or_else(|| Error::Sync("remote synchronization branch has no commits".into()))?;
    let store_root = checkout.store_root(&manifest.store)?;
    if !store_root.is_dir() {
        return Err(Error::Sync(format!(
            "remote synchronization path '{}' does not exist",
            manifest.store.path
        )));
    }
    let mut snapshot = PortableSnapshot::load(&store_root, &manifest.project_id)?;
    apply_share_policy(&manifest, &mut snapshot);
    let remote_digest = snapshot.digest()?;

    let existing_state = state::load_sync_state(db, &manifest)?;
    if state::project_exists(db, &manifest.project_id)? {
        let local = state::export_snapshot_for_fetch(db, config, &manifest)?;
        let local_digest = local.digest()?;
        match existing_state.as_ref() {
            Some(previous) if previous.remote_snapshot_hash == remote_digest && !force => {
                state::save_sync_state(
                    db,
                    &manifest,
                    &commit,
                    &previous.local_snapshot_hash,
                    &previous.remote_snapshot_hash,
                )?;
                return Ok(SyncOutcome {
                    project_id: manifest.project_id,
                    commit,
                    counts: snapshot.counts(),
                    interrupted_conversation_turn_ids: Vec::new(),
                });
            }
            Some(previous)
                if previous.remote_snapshot_hash != remote_digest
                    && previous.local_snapshot_hash != local_digest
                    && !force =>
            {
                return Err(Error::Sync(
                    "both the local Project and remote synchronization store changed since the last fetch; inspect the changes and rerun with --force to merge non-destructively"
                        .into(),
                ));
            }
            None if state::project_has_portable_data(db, &manifest.project_id)? && !force => {
                return Err(Error::Sync(
                    "this is the first fetch for a populated local Project; rerun with --force to acknowledge a non-destructive merge"
                        .into(),
                ));
            }
            _ => {}
        }
    }

    let interrupted_conversation_turn_ids =
        state::import_snapshot(db, config, config_path, project_dir, &snapshot)?;
    interrupt_handler(&interrupted_conversation_turn_ids);
    let merged = state::export_snapshot_for_fetch(db, config, &manifest)?;
    let digest = merged.digest()?;
    state::save_sync_state(db, &manifest, &commit, &digest, &remote_digest)?;
    Ok(SyncOutcome {
        project_id: manifest.project_id,
        commit,
        counts: snapshot.counts(),
        interrupted_conversation_turn_ids,
    })
}

/// Explicitly publish current portable state. Publishing is optimistic: when
/// the configured store path already exists, the caller must have fetched the
/// current Project snapshot first. The final Git push is non-forced, providing
/// a second concurrency check at the remote.
pub fn publish(db: &Database, config: &Config, project_dir: &Path) -> Result<SyncOutcome> {
    let manifest = ProjectSyncManifest::load(project_dir)?;
    if !state::project_exists(db, &manifest.project_id)? {
        return Err(Error::Sync(format!(
            "Project '{}' does not exist locally",
            manifest.project_id
        )));
    }
    state::ensure_quiescent(db, &manifest.project_id)?;
    let mut snapshot = state::export_snapshot(db, config, &manifest)?;
    let digest = snapshot.digest()?;

    let checkout = GitCheckout::open(&manifest.store, true)?;
    let store_root = checkout.store_root(&manifest.store)?;
    let previous = state::load_sync_state(db, &manifest)?;
    let remote_digest = if store_root.exists() {
        // Validate the remote payload before using its existence as the
        // optimistic-concurrency boundary.
        let mut remote = PortableSnapshot::load(&store_root, &manifest.project_id)?;
        apply_share_policy(&manifest, &mut remote);
        Some(remote.digest()?)
    } else {
        None
    };
    match (previous.as_ref(), remote_digest.as_deref()) {
        (None, Some(_)) => {
            return Err(Error::Sync(
                "the remote Project already exists; run sync fetch before publishing".into(),
            ));
        }
        (Some(previous), Some(remote_digest)) if previous.remote_snapshot_hash != remote_digest => {
            return Err(Error::Sync(
                "the remote Project changed since the last fetch; fetch before publishing".into(),
            ));
        }
        (Some(_), None) => {
            return Err(Error::Sync(
                "the remote Project was removed since the last fetch; restore it or update the manifest before publishing"
                    .into(),
            ));
        }
        _ => {}
    }

    snapshot.write(&store_root)?;
    let commit = checkout.commit_and_push(
        &manifest.store.path,
        &format!("Synchronize XpressClaw Project {}", manifest.project_id),
    )?;
    state::save_sync_state(db, &manifest, &commit, &digest, &digest)?;
    Ok(SyncOutcome {
        project_id: manifest.project_id,
        commit,
        counts: snapshot.counts(),
        interrupted_conversation_turn_ids: Vec::new(),
    })
}

fn apply_share_policy(manifest: &ProjectSyncManifest, snapshot: &mut PortableSnapshot) {
    if !manifest.share.project_memory {
        snapshot.memory_notes.clear();
        snapshot.memory_links.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;

    fn add_project_with_message(db: &Database, content: &str) {
        db.with_conn(|connection| {
            connection
                .execute(
                    "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One')",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO conversations (id, title, project_id)
                     VALUES ('conversation-one', 'Design', 'project-one')",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO conversation_messages
                        (conversation_id, sender_type, sender_id, content)
                     VALUES ('conversation-one', 'user', 'local-user', ?1)",
                    [content],
                )
                .unwrap();
        });
    }

    fn add_message(db: &Database, content: &str) {
        db.with_conn(|connection| {
            connection
                .execute(
                    "INSERT INTO conversation_messages
                        (conversation_id, sender_type, sender_id, content)
                     VALUES ('conversation-one', 'user', 'local-user', ?1)",
                    [content],
                )
                .unwrap();
        });
    }

    fn add_memory_note(db: &Database) {
        db.with_conn(|connection| {
            connection
                .execute(
                    "INSERT INTO project_memory_notes
                        (id, project_id, title, body, summary, search_key)
                     VALUES ('memory-one', 'project-one', 'Private', 'body', 'summary', 'private')",
                    [],
                )
                .unwrap();
        });
    }

    fn save_config(directory: &Path) -> (Config, PathBuf) {
        let config = Config::default();
        let path = directory.join("xpressclaw.yaml");
        config.save(&path).unwrap();
        (config, path)
    }

    #[test]
    fn explicit_publish_fetch_and_divergent_merge_round_trip() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let remote = root.path().join("remote.git");
        fs::create_dir(&remote).unwrap();
        assert!(Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .current_dir(&remote)
            .status()
            .unwrap()
            .success());

        let first_dir = root.path().join("first");
        let second_dir = root.path().join("second");
        fs::create_dir_all(&first_dir).unwrap();
        fs::create_dir_all(&second_dir).unwrap();
        let first_db = Database::open_memory().unwrap();
        add_project_with_message(&first_db, "initial");
        add_memory_note(&first_db);
        let (first_config, first_config_path) = save_config(&first_dir);
        initialize(
            &first_db,
            &first_dir,
            "project-one",
            &remote.display().to_string(),
            "shared",
            "projects/project-one",
            true,
        )
        .unwrap();
        let first_publish = publish(&first_db, &first_config, &first_dir).unwrap();

        let unrelated_store = GitStoreConfig {
            remote: remote.display().to_string(),
            branch: "shared".into(),
            path: "projects/unrelated".into(),
        };
        let unrelated_checkout = GitCheckout::open(&unrelated_store, false).unwrap();
        let unrelated_root = unrelated_checkout.store_root(&unrelated_store).unwrap();
        fs::create_dir_all(&unrelated_root).unwrap();
        fs::write(unrelated_root.join("state.txt"), "unrelated Project").unwrap();
        let unrelated_commit = unrelated_checkout
            .commit_and_push(&unrelated_store.path, "Update unrelated Project")
            .unwrap();
        assert_ne!(unrelated_commit, first_publish.commit);

        first_db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE projects SET name = 'Local rename' WHERE id = 'project-one'",
                    [],
                )
                .unwrap();
        });
        let mut unchanged_remote_config = Config::load(&first_config_path).unwrap();
        let unchanged_fetch = fetch(
            &first_db,
            &mut unchanged_remote_config,
            &first_config_path,
            &first_dir,
            false,
        )
        .unwrap();
        assert_eq!(unchanged_fetch.commit, unrelated_commit);
        let local_name: String = first_db
            .with_conn(|connection| {
                connection.query_row(
                    "SELECT name FROM projects WHERE id = 'project-one'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(local_name, "Local rename");

        let second_manifest = fs::read_to_string(first_dir.join(MANIFEST_FILE))
            .unwrap()
            .replace("project_memory: true", "project_memory: false");
        fs::write(second_dir.join(MANIFEST_FILE), second_manifest).unwrap();
        let second_db = Database::open_memory().unwrap();
        let (mut second_config, second_config_path) = save_config(&second_dir);
        let first_fetch = fetch(
            &second_db,
            &mut second_config,
            &second_config_path,
            &second_dir,
            false,
        )
        .unwrap();
        assert_eq!(first_fetch.commit, unrelated_commit);
        let fetched_memory: i64 = second_db
            .with_conn(|connection| {
                connection.query_row(
                    "SELECT COUNT(*) FROM project_memory_notes WHERE project_id = 'project-one'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(fetched_memory, 0);

        add_message(&second_db, "remote branch");
        publish(&second_db, &second_config, &second_dir).unwrap();
        add_message(&first_db, "local branch");

        let mut first_config_after = Config::load(&first_config_path).unwrap();
        let conflict = fetch(
            &first_db,
            &mut first_config_after,
            &first_config_path,
            &first_dir,
            false,
        )
        .unwrap_err();
        assert!(conflict
            .to_string()
            .contains("both the local Project and remote"));

        fetch(
            &first_db,
            &mut first_config_after,
            &first_config_path,
            &first_dir,
            true,
        )
        .unwrap();
        let messages: i64 = first_db
            .with_conn(|connection| {
                connection.query_row(
                    "SELECT COUNT(*) FROM conversation_messages
                     WHERE conversation_id = 'conversation-one'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(messages, 3);
        let merged = publish(&first_db, &first_config_after, &first_dir).unwrap();
        assert_eq!(merged.counts.conversation_messages, 3);
    }

    #[test]
    fn interrupted_turns_are_reported_before_post_import_failure() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let remote = root.path().join("remote.git");
        fs::create_dir(&remote).unwrap();
        assert!(Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .current_dir(&remote)
            .status()
            .unwrap()
            .success());

        let source_dir = root.path().join("source");
        let replica_dir = root.path().join("replica");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&replica_dir).unwrap();

        let source_db = Database::open_memory().unwrap();
        add_project_with_message(&source_db, "delete me");
        source_db.with_conn(|connection| {
            connection
                .execute(
                    "INSERT INTO agents (id, name, backend, config, project_id)
                     VALUES ('atlas', 'Atlas', 'codex', '{}', 'project-one')",
                    [],
                )
                .unwrap();
        });
        let source_config = Config {
            agents: vec![crate::config::AgentConfig {
                name: "atlas".into(),
                backend: "codex".into(),
                ..crate::config::AgentConfig::default()
            }],
            ..Config::default()
        };
        source_config
            .save(&source_dir.join("xpressclaw.yaml"))
            .unwrap();
        initialize(
            &source_db,
            &source_dir,
            "project-one",
            &remote.display().to_string(),
            "shared",
            "projects/project-one",
            true,
        )
        .unwrap();
        publish(&source_db, &source_config, &source_dir).unwrap();

        fs::copy(
            source_dir.join(MANIFEST_FILE),
            replica_dir.join(MANIFEST_FILE),
        )
        .unwrap();
        let replica_db = Database::open_memory().unwrap();
        let (mut replica_config, replica_config_path) = save_config(&replica_dir);
        fetch(
            &replica_db,
            &mut replica_config,
            &replica_config_path,
            &replica_dir,
            false,
        )
        .unwrap();
        replica_db.with_conn(|connection| {
            connection
                .execute_batch(
                    "INSERT INTO conversation_agent_sessions
                        (conversation_id, agent_id, native_session_id, status)
                     VALUES ('conversation-one', 'atlas', 'active-session', 'running');
                     INSERT INTO conversation_turns
                        (id, conversation_id, agent_id, trigger_message_id, status)
                     SELECT 'running-turn', 'conversation-one', 'atlas', id, 'running'
                     FROM conversation_messages WHERE content = 'delete me';",
                )
                .unwrap();
        });

        source_db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE conversation_messages
                     SET deleted_at = '2026-01-02 00:00:00'
                     WHERE content = 'delete me'",
                    [],
                )
                .unwrap();
        });
        publish(&source_db, &source_config, &source_dir).unwrap();

        let mut interrupted = Vec::new();
        let error = fetch_with_interrupt_handler(
            &replica_db,
            &mut replica_config,
            &replica_config_path,
            &replica_dir,
            false,
            |turn_ids| {
                interrupted.extend_from_slice(turn_ids);
                replica_db.with_conn(|connection| {
                    connection
                        .execute_batch(
                            "INSERT INTO tasks (id, title, status, agent_id, project_id)
                             VALUES ('racing-task', 'Racing task', 'in_progress', 'atlas', 'project-one');
                             INSERT INTO work_attempts
                                (id, session_id, task_id, runner, status)
                             VALUES ('racing-attempt', 'atlas', 'racing-task', 'native', 'running');",
                        )
                        .unwrap();
                });
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("active tasks and workflows"));
        assert_eq!(interrupted, ["running-turn"]);
        let turn_status = replica_db
            .with_conn(|connection| {
                connection.query_row(
                    "SELECT status FROM conversation_turns WHERE id = 'running-turn'",
                    [],
                    |row| row.get::<_, String>(0),
                )
            })
            .unwrap();
        assert_eq!(turn_status, "cancelled");
    }
}
