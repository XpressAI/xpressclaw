//! Durable, Agent-scoped repository selection inside an approved workspace.
//!
//! An Agent's configured workspace is the writable security boundary. The
//! active repository is a narrower, local-only runtime choice beneath that
//! boundary; it is deliberately not part of portable Project configuration.

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::hmac;
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::Database;
use crate::error::{Error, Result};
use crate::workers::github;

const MAX_DISCOVERY_DEPTH: usize = 4;
const MAX_SCANNED_DIRECTORIES: usize = 512;
const MAX_REPOSITORIES: usize = 32;
const CALLBACK_CAPABILITY_CONTEXT: &[u8] = b"xpressclaw-runner-callback-v1\0";

/// Derive the narrow callback capability exposed to one Agent's bundled
/// GitHub MCP. The GitHub process never receives the listener's root
/// capability and cannot retarget repository resolution at another Agent by
/// changing a URL or header.
pub fn agent_callback_capability(secret: &str, agent_id: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let mut message = Vec::with_capacity(CALLBACK_CAPABILITY_CONTEXT.len() + agent_id.len());
    message.extend_from_slice(CALLBACK_CAPABILITY_CONTEXT);
    message.extend_from_slice(agent_id.as_bytes());
    URL_SAFE_NO_PAD.encode(hmac::sign(&key, &message).as_ref())
}

pub fn verify_agent_callback_capability(secret: &str, agent_id: &str, supplied: &str) -> bool {
    let Ok(supplied) = URL_SAFE_NO_PAD.decode(supplied) else {
        return false;
    };
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let mut message = Vec::with_capacity(CALLBACK_CAPABILITY_CONTEXT.len() + agent_id.len());
    message.extend_from_slice(CALLBACK_CAPABILITY_CONTEXT);
    message.extend_from_slice(agent_id.as_bytes());
    hmac::verify(&key, &message, &supplied).is_ok()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryCandidate {
    /// Slash-separated path relative to the Agent's bootstrap workspace.
    pub relative_path: String,
    pub root: PathBuf,
    /// Kept server-side because remote URLs may embed credentials.
    #[serde(skip)]
    pub origin: Option<String>,
    pub github_repository: Option<String>,
    #[serde(skip)]
    identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositorySelectionState {
    Attached,
    Pending,
    NoRepository,
    Ambiguous,
    Missing,
    Cleared,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryInspection {
    pub bootstrap_root: PathBuf,
    pub active: Option<RepositoryCandidate>,
    pub candidates: Vec<RepositoryCandidate>,
    pub discovery_truncated: bool,
    pub state: RepositorySelectionState,
    pub selected_relative_path: Option<String>,
    pub pending_relative_path: Option<String>,
    pub pending_action: Option<String>,
    #[serde(skip)]
    stored: Option<StoredSelection>,
    #[serde(skip)]
    desired: DesiredSelection,
}

impl RepositoryInspection {
    pub fn requires_boundary_change(&self) -> bool {
        let stored = self
            .stored
            .as_ref()
            .map(StoredSelection::boundary_key)
            .unwrap_or((None, None, SelectionMode::Automatic));
        stored != self.desired.boundary_key()
            || self
                .stored
                .as_ref()
                .is_some_and(|selection| selection.pending_mode.is_some())
    }

    /// Whether consuming the current repository state changes the effective
    /// cwd/identity and therefore must retire retained native sessions. A
    /// stale pending proposal still needs boundary cleanup, but it leaves the
    /// current valid repository and process untouched.
    pub fn requires_runtime_restart(&self) -> bool {
        let stored = self
            .stored
            .as_ref()
            .map(StoredSelection::boundary_key)
            .unwrap_or((None, None, SelectionMode::Automatic));
        stored != self.desired.boundary_key()
            || (self
                .stored
                .as_ref()
                .is_some_and(|selection| selection.pending_mode.is_some())
                && self.state == RepositorySelectionState::Pending)
    }

    pub fn active_root(&self) -> &Path {
        self.active
            .as_ref()
            .map(|candidate| candidate.root.as_path())
            .unwrap_or(self.bootstrap_root.as_path())
    }

    pub fn active_relative_path(&self) -> Option<&str> {
        self.active
            .as_ref()
            .map(|candidate| candidate.relative_path.as_str())
    }

    pub fn generation(&self) -> Option<i64> {
        self.stored.as_ref().map(|selection| selection.generation)
    }
}

#[derive(Debug, Clone)]
struct StoredSelection {
    relative_path: Option<String>,
    repository_identity: Option<String>,
    mode: SelectionMode,
    pending_relative_path: Option<String>,
    pending_mode: Option<SelectionMode>,
    generation: i64,
}

impl StoredSelection {
    fn boundary_key(&self) -> (Option<&str>, Option<&str>, SelectionMode) {
        (
            self.relative_path.as_deref(),
            self.repository_identity.as_deref(),
            self.mode,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionMode {
    Automatic,
    Manual,
    Cleared,
}

#[derive(Debug, Clone, Copy)]
enum GenerationExpectation {
    Any,
    Exact(Option<i64>),
}

impl SelectionMode {
    fn parse(value: &str) -> Self {
        match value {
            "manual" => Self::Manual,
            "cleared" => Self::Cleared,
            _ => Self::Automatic,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
            Self::Cleared => "cleared",
        }
    }
}

#[derive(Debug, Clone)]
struct DesiredSelection {
    relative_path: Option<String>,
    repository_identity: Option<String>,
    mode: SelectionMode,
}

impl DesiredSelection {
    fn boundary_key(&self) -> (Option<&str>, Option<&str>, SelectionMode) {
        (
            self.relative_path.as_deref(),
            self.repository_identity.as_deref(),
            self.mode,
        )
    }
}

#[derive(Debug, Clone)]
pub struct RepositoryBoundaryResult {
    pub inspection: RepositoryInspection,
    pub changed: bool,
}

pub struct AgentRepositoryManager {
    db: Arc<Database>,
}

impl AgentRepositoryManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Inspect repository state without changing durable selection. Callers
    /// use this before deciding whether to enter the per-Agent write barrier.
    pub fn inspect(&self, agent_id: &str, bootstrap_root: &Path) -> Result<RepositoryInspection> {
        let bootstrap_root = bootstrap_root.canonicalize().map_err(|error| {
            Error::Backend(format!(
                "workspace {} is unavailable: {error}",
                bootstrap_root.display()
            ))
        })?;
        let stored = self.stored(agent_id)?;
        let (mut candidates, discovery_truncated) = discover_repositories(&bootstrap_root)?;
        // Explicit selections are not limited by automatic discovery depth.
        // Re-authorize them directly so a deliberately chosen deep checkout
        // remains usable while automatic scanning stays bounded.
        if let Some(selection) = stored.as_ref() {
            for relative_path in [
                selection.relative_path.as_deref(),
                selection.pending_relative_path.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                if candidates
                    .iter()
                    .any(|candidate| candidate.relative_path == relative_path)
                {
                    continue;
                }
                if let Ok(candidate) = authorize_candidate(&bootstrap_root, relative_path) {
                    candidates.push(candidate);
                }
            }
            candidates.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        }
        Ok(build_inspection(
            bootstrap_root,
            candidates,
            discovery_truncated,
            stored,
        ))
    }

    /// Resolve one untrusted relative path to an eligible repository while
    /// preserving the configured bootstrap workspace as the security boundary.
    pub fn candidate_at(
        &self,
        bootstrap_root: &Path,
        relative_path: &str,
    ) -> Result<RepositoryCandidate> {
        let bootstrap_root = bootstrap_root.canonicalize().map_err(|error| {
            Error::Backend(format!(
                "workspace {} is unavailable: {error}",
                bootstrap_root.display()
            ))
        })?;
        authorize_candidate(&bootstrap_root, relative_path)
    }

    /// Apply the deterministic pending/automatic transition at a safe Agent
    /// runtime boundary. A repository identity or cwd change invalidates only
    /// native ACP session handles; Task and Conversation history remains.
    pub fn apply_boundary(
        &self,
        agent_id: &str,
        bootstrap_root: &Path,
    ) -> Result<RepositoryBoundaryResult> {
        let inspection = self.inspect(agent_id, bootstrap_root)?;
        let stored = inspection
            .stored
            .as_ref()
            .map(StoredSelection::boundary_key)
            .unwrap_or((None, None, SelectionMode::Automatic));
        let boundary_changed = stored != inspection.desired.boundary_key();
        let has_pending = inspection
            .stored
            .as_ref()
            .is_some_and(|selection| selection.pending_mode.is_some());
        // A live bootstrap GitHub resolution may already persist the new
        // repository while leaving a same-path pending marker. The marker
        // forces the next safe boundary to retire the old-cwd ACP process even
        // if the just-finished turn wrote its native session ID again.
        let runtime_changed = inspection.requires_runtime_restart();
        if boundary_changed || has_pending {
            self.persist_selection(
                agent_id,
                &inspection.desired,
                None,
                None,
                runtime_changed,
                GenerationExpectation::Any,
            )?;
        }
        let inspection = self.inspect(agent_id, bootstrap_root)?;
        Ok(RepositoryBoundaryResult {
            inspection,
            changed: runtime_changed,
        })
    }

    /// Persist an explicit user selection. The path is always interpreted
    /// relative to the approved bootstrap workspace.
    pub fn select(
        &self,
        agent_id: &str,
        bootstrap_root: &Path,
        relative_path: &str,
    ) -> Result<RepositoryBoundaryResult> {
        let bootstrap_root = bootstrap_root.canonicalize().map_err(|error| {
            Error::Backend(format!(
                "workspace {} is unavailable: {error}",
                bootstrap_root.display()
            ))
        })?;
        let candidate = authorize_candidate(&bootstrap_root, relative_path)?;
        let desired = DesiredSelection {
            relative_path: Some(candidate.relative_path.clone()),
            repository_identity: Some(candidate.identity.clone()),
            mode: SelectionMode::Manual,
        };
        let previous = self.stored(agent_id)?.map(|stored| {
            (
                stored.relative_path,
                stored.repository_identity,
                stored.mode,
            )
        });
        let changed = previous
            .as_ref()
            .map(|value| (value.0.as_deref(), value.1.as_deref(), value.2))
            != Some(desired.boundary_key());
        self.persist_selection(
            agent_id,
            &desired,
            None,
            None,
            changed,
            GenerationExpectation::Any,
        )?;
        Ok(RepositoryBoundaryResult {
            inspection: self.inspect(agent_id, &bootstrap_root)?,
            changed,
        })
    }

    /// Activate a repository for one live bootstrap GitHub invocation while
    /// atomically retaining a same-path marker for the next turn boundary.
    /// The marker is essential because the current turn can write its old
    /// native session ID again after this callback returns.
    pub fn select_live(
        &self,
        agent_id: &str,
        bootstrap_root: &Path,
        relative_path: &str,
        expected_generation: Option<i64>,
    ) -> Result<Option<RepositoryBoundaryResult>> {
        let bootstrap_root = bootstrap_root.canonicalize().map_err(|error| {
            Error::Backend(format!(
                "workspace {} is unavailable: {error}",
                bootstrap_root.display()
            ))
        })?;
        let candidate = authorize_candidate(&bootstrap_root, relative_path)?;
        let desired = DesiredSelection {
            relative_path: Some(candidate.relative_path.clone()),
            repository_identity: Some(candidate.identity.clone()),
            mode: SelectionMode::Manual,
        };
        if !self.persist_selection(
            agent_id,
            &desired,
            Some(candidate.relative_path.as_str()),
            Some(SelectionMode::Manual),
            true,
            GenerationExpectation::Exact(expected_generation),
        )? {
            return Ok(None);
        }
        Ok(Some(RepositoryBoundaryResult {
            inspection: self.inspect(agent_id, &bootstrap_root)?,
            changed: true,
        }))
    }

    /// Disable automatic adoption until a user selects a repository again.
    pub fn clear(&self, agent_id: &str, bootstrap_root: &Path) -> Result<RepositoryBoundaryResult> {
        let desired = DesiredSelection {
            relative_path: None,
            repository_identity: None,
            mode: SelectionMode::Cleared,
        };
        let changed = self
            .stored(agent_id)?
            .map(|stored| stored.boundary_key() != desired.boundary_key())
            .unwrap_or(true);
        self.persist_selection(
            agent_id,
            &desired,
            None,
            None,
            changed,
            GenerationExpectation::Any,
        )?;
        Ok(RepositoryBoundaryResult {
            inspection: self.inspect(agent_id, bootstrap_root)?,
            changed,
        })
    }

    /// Record an Agent's validated proposal. It is deliberately not applied
    /// inside the current ACP process; the next safe turn boundary consumes it.
    pub fn propose(
        &self,
        agent_id: &str,
        bootstrap_root: &Path,
        relative_path: &str,
    ) -> Result<RepositoryInspection> {
        let bootstrap_root = bootstrap_root.canonicalize().map_err(|error| {
            Error::Backend(format!(
                "workspace {} is unavailable: {error}",
                bootstrap_root.display()
            ))
        })?;
        let candidate = authorize_candidate(&bootstrap_root, relative_path)?;
        let stored = self.stored(agent_id)?.unwrap_or(StoredSelection {
            relative_path: None,
            repository_identity: None,
            mode: SelectionMode::Automatic,
            pending_relative_path: None,
            pending_mode: None,
            generation: 0,
        });
        let desired = DesiredSelection {
            relative_path: stored.relative_path,
            repository_identity: stored.repository_identity,
            mode: stored.mode,
        };
        self.persist_selection(
            agent_id,
            &desired,
            Some(candidate.relative_path.as_str()),
            Some(SelectionMode::Manual),
            false,
            GenerationExpectation::Any,
        )?;
        self.inspect(agent_id, &bootstrap_root)
    }

    /// Queue an explicit clear without interrupting a running turn. The next
    /// turn boundary invalidates retained sessions and applies it atomically.
    pub fn propose_clear(
        &self,
        agent_id: &str,
        bootstrap_root: &Path,
    ) -> Result<RepositoryInspection> {
        let stored = self.stored(agent_id)?.unwrap_or(StoredSelection {
            relative_path: None,
            repository_identity: None,
            mode: SelectionMode::Automatic,
            pending_relative_path: None,
            pending_mode: None,
            generation: 0,
        });
        let desired = DesiredSelection {
            relative_path: stored.relative_path,
            repository_identity: stored.repository_identity,
            mode: stored.mode,
        };
        self.persist_selection(
            agent_id,
            &desired,
            None,
            Some(SelectionMode::Cleared),
            false,
            GenerationExpectation::Any,
        )?;
        self.inspect(agent_id, bootstrap_root)
    }

    fn stored(&self, agent_id: &str) -> Result<Option<StoredSelection>> {
        self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT relative_path, repository_identity, selection_mode,
                            pending_relative_path, pending_selection_mode, generation
                     FROM agent_repository_selections WHERE agent_id = ?1",
                    [agent_id],
                    |row| {
                        Ok(StoredSelection {
                            relative_path: row.get(0)?,
                            repository_identity: row.get(1)?,
                            mode: SelectionMode::parse(&row.get::<_, String>(2)?),
                            pending_relative_path: row.get(3)?,
                            pending_mode: row
                                .get::<_, Option<String>>(4)?
                                .as_deref()
                                .map(SelectionMode::parse),
                            generation: row.get(5)?,
                        })
                    },
                )
                .optional()
                .map_err(Error::from)
        })
    }

    fn persist_selection(
        &self,
        agent_id: &str,
        desired: &DesiredSelection,
        pending_relative_path: Option<&str>,
        pending_mode: Option<SelectionMode>,
        invalidate_sessions: bool,
        generation_expectation: GenerationExpectation,
    ) -> Result<bool> {
        self.db.with_conn(|connection| {
            let transaction =
                rusqlite::Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
            if let GenerationExpectation::Exact(expected) = generation_expectation {
                let current = transaction
                    .query_row(
                        "SELECT generation FROM agent_repository_selections WHERE agent_id = ?1",
                        [agent_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                if current != expected {
                    return Ok(false);
                }
            }
            transaction.execute(
                "INSERT INTO agent_repository_selections
                    (agent_id, relative_path, repository_identity, selection_mode,
                     pending_relative_path, pending_selection_mode, generation, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, CURRENT_TIMESTAMP)
                 ON CONFLICT(agent_id) DO UPDATE SET
                    relative_path = excluded.relative_path,
                    repository_identity = excluded.repository_identity,
                    selection_mode = excluded.selection_mode,
                    pending_relative_path = excluded.pending_relative_path,
                    pending_selection_mode = excluded.pending_selection_mode,
                    generation = agent_repository_selections.generation + 1,
                    updated_at = CURRENT_TIMESTAMP",
                rusqlite::params![
                    agent_id,
                    desired.relative_path.as_deref(),
                    desired.repository_identity.as_deref(),
                    desired.mode.as_str(),
                    pending_relative_path,
                    pending_mode.map(SelectionMode::as_str),
                ],
            )?;
            if invalidate_sessions {
                transaction.execute(
                    "UPDATE work_attempts SET native_session_id = NULL
                     WHERE session_id IN (
                         SELECT id FROM logical_sessions WHERE agent_id = ?1
                     ) AND native_session_id IS NOT NULL",
                    [agent_id],
                )?;
                transaction.execute(
                    "UPDATE conversation_agent_sessions SET native_session_id = NULL,
                            updated_at = CURRENT_TIMESTAMP
                     WHERE agent_id = ?1 AND native_session_id IS NOT NULL",
                    [agent_id],
                )?;
            }
            transaction.commit()?;
            Ok::<bool, Error>(true)
        })
    }
}

/// Resolve the currently persisted, still-valid active repository without
/// triggering automatic adoption. Background review/wait workers must never
/// guess a repository outside a turn boundary.
pub fn active_repository_root(
    db: &Arc<Database>,
    agent_id: &str,
    bootstrap_root: &Path,
) -> Result<Option<PathBuf>> {
    let inspection = AgentRepositoryManager::new(db.clone()).inspect(agent_id, bootstrap_root)?;
    if let Some(active) = inspection.active {
        return Ok(Some(active.root));
    }
    if inspection.stored.is_some() {
        return Ok(None);
    }
    // Migration compatibility: an explicit legacy workspace whose root is
    // already a repository has never been ambiguous. Keep background PR
    // monitoring functional before that Agent's first post-upgrade turn.
    Ok(inspection
        .candidates
        .into_iter()
        .find(|candidate| candidate.relative_path == ".")
        .map(|candidate| candidate.root))
}

fn build_inspection(
    bootstrap_root: PathBuf,
    candidates: Vec<RepositoryCandidate>,
    discovery_truncated: bool,
    stored: Option<StoredSelection>,
) -> RepositoryInspection {
    let pending_mode = stored.as_ref().and_then(|selection| selection.pending_mode);
    let pending_path = stored
        .as_ref()
        .and_then(|selection| selection.pending_relative_path.as_deref());
    let pending = stored
        .as_ref()
        .and_then(|selection| selection.pending_relative_path.as_deref())
        .and_then(|path| candidate(&candidates, path));
    let selected = stored
        .as_ref()
        .and_then(|selection| selection.relative_path.as_deref())
        .and_then(|path| candidate(&candidates, path));

    let (active, desired, state) = if pending_mode == Some(SelectionMode::Cleared) {
        (
            selected.cloned(),
            DesiredSelection {
                relative_path: None,
                repository_identity: None,
                mode: SelectionMode::Cleared,
            },
            RepositorySelectionState::Pending,
        )
    } else if let Some(pending) = pending {
        (
            selected.cloned(),
            DesiredSelection {
                relative_path: Some(pending.relative_path.clone()),
                repository_identity: Some(pending.identity.clone()),
                mode: SelectionMode::Manual,
            },
            RepositorySelectionState::Pending,
        )
    } else if pending_path.is_some() {
        // A proposal is validated when it is recorded, but the repository can
        // disappear before the next turn boundary. Preserve any current
        // selection, consume the stale proposal at that boundary, and never
        // substitute an unrelated repository merely because only one remains.
        let mode = stored
            .as_ref()
            .map(|selection| selection.mode)
            .unwrap_or(SelectionMode::Automatic);
        (
            selected.cloned(),
            DesiredSelection {
                relative_path: stored
                    .as_ref()
                    .and_then(|selection| selection.relative_path.clone()),
                repository_identity: selected.map(|candidate| candidate.identity.clone()),
                mode,
            },
            RepositorySelectionState::Missing,
        )
    } else if stored
        .as_ref()
        .is_some_and(|selection| selection.mode == SelectionMode::Cleared)
    {
        (
            None,
            DesiredSelection {
                relative_path: None,
                repository_identity: None,
                mode: SelectionMode::Cleared,
            },
            RepositorySelectionState::Cleared,
        )
    } else if let Some(selected) = selected {
        let mode = stored
            .as_ref()
            .map(|selection| selection.mode)
            .unwrap_or(SelectionMode::Automatic);
        let identity_changed = stored
            .as_ref()
            .and_then(|selection| selection.repository_identity.as_deref())
            != Some(selected.identity.as_str());
        (
            Some(selected.clone()),
            DesiredSelection {
                relative_path: Some(selected.relative_path.clone()),
                repository_identity: Some(selected.identity.clone()),
                mode,
            },
            if identity_changed {
                RepositorySelectionState::Pending
            } else {
                RepositorySelectionState::Attached
            },
        )
    } else {
        let mode = stored
            .as_ref()
            .map(|selection| selection.mode)
            .unwrap_or(SelectionMode::Automatic);
        let has_persisted_selection = stored
            .as_ref()
            .is_some_and(|selection| selection.relative_path.is_some());
        let automatically_selected = (mode == SelectionMode::Automatic && !has_persisted_selection)
            .then(|| automatic_candidate(&candidates, discovery_truncated))
            .flatten();
        if let Some(selected) = automatically_selected {
            (
                None,
                DesiredSelection {
                    relative_path: Some(selected.relative_path.clone()),
                    repository_identity: Some(selected.identity.clone()),
                    mode: SelectionMode::Automatic,
                },
                RepositorySelectionState::Pending,
            )
        } else {
            let selected_path = stored
                .as_ref()
                .and_then(|selection| selection.relative_path.clone());
            let state = if selected_path.is_some() {
                RepositorySelectionState::Missing
            } else if candidates.is_empty() && !discovery_truncated {
                RepositorySelectionState::NoRepository
            } else {
                RepositorySelectionState::Ambiguous
            };
            (
                None,
                DesiredSelection {
                    relative_path: selected_path,
                    repository_identity: None,
                    mode,
                },
                state,
            )
        }
    };

    RepositoryInspection {
        bootstrap_root,
        active,
        candidates,
        discovery_truncated,
        state,
        selected_relative_path: stored
            .as_ref()
            .and_then(|selection| selection.relative_path.clone()),
        pending_relative_path: stored
            .as_ref()
            .and_then(|selection| selection.pending_relative_path.clone()),
        pending_action: stored
            .as_ref()
            .and_then(|selection| selection.pending_mode)
            .map(SelectionMode::as_str)
            .map(str::to_string),
        stored,
        desired,
    }
}

fn automatic_candidate(
    candidates: &[RepositoryCandidate],
    discovery_truncated: bool,
) -> Option<&RepositoryCandidate> {
    candidates
        .iter()
        .find(|candidate| candidate.relative_path == ".")
        .or_else(|| (!discovery_truncated && candidates.len() == 1).then(|| &candidates[0]))
}

fn candidate<'a>(
    candidates: &'a [RepositoryCandidate],
    relative_path: &str,
) -> Option<&'a RepositoryCandidate> {
    candidates
        .iter()
        .find(|candidate| candidate.relative_path == relative_path)
}

fn discover_repositories(bootstrap_root: &Path) -> Result<(Vec<RepositoryCandidate>, bool)> {
    let mut repositories = Vec::new();
    let mut queue = VecDeque::from([(bootstrap_root.to_path_buf(), 0usize)]);
    let mut scanned = 0usize;
    let mut truncated = false;

    while let Some((directory, depth)) = queue.pop_front() {
        if scanned >= MAX_SCANNED_DIRECTORIES || repositories.len() >= MAX_REPOSITORIES {
            truncated = true;
            break;
        }
        scanned += 1;
        if let Some(repository) = inspect_repository(bootstrap_root, &directory) {
            repositories.push(repository);
            // A clone is one candidate. Do not descend into its internal
            // worktrees, submodules, dependencies, or test fixtures.
            continue;
        }
        if depth >= MAX_DISCOVERY_DEPTH {
            truncated |= std::fs::read_dir(&directory)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(std::result::Result::ok)
                .any(|entry| {
                    entry.file_type().is_ok_and(|file_type| {
                        file_type.is_dir()
                            && !file_type.is_symlink()
                            && !ignored_directory(&entry.file_name().to_string_lossy())
                    })
                });
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut children = entries
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                let name = entry.file_name();
                if file_type.is_symlink()
                    || !file_type.is_dir()
                    || ignored_directory(&name.to_string_lossy())
                {
                    return None;
                }
                Some(entry.path())
            })
            .collect::<Vec<_>>();
        children.sort();
        queue.extend(children.into_iter().map(|path| (path, depth + 1)));
    }
    repositories.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok((repositories, truncated))
}

fn ignored_directory(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".xpressclaw" | "node_modules" | "target" | ".venv" | "__pycache__"
    )
}

fn authorize_candidate(bootstrap_root: &Path, relative_path: &str) -> Result<RepositoryCandidate> {
    let normalized = normalize_relative_path(relative_path)?;
    let requested = bootstrap_root.join(&normalized);
    let canonical = requested.canonicalize().map_err(|error| {
        Error::Backend(format!(
            "repository path {} is unavailable: {error}",
            requested.display()
        ))
    })?;
    if canonical != bootstrap_root && !canonical.starts_with(bootstrap_root) {
        return Err(Error::Backend(
            "repository path leaves the Agent's approved workspace".into(),
        ));
    }
    inspect_repository(bootstrap_root, &canonical).ok_or_else(|| {
        Error::Backend(format!(
            "{} is not an eligible Git repository inside this Agent's workspace",
            relative_path
        ))
    })
}

fn normalize_relative_path(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    let raw = if raw.is_empty() || raw == "." {
        "."
    } else {
        raw
    };
    let mut normalized = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Backend(
                    "repository paths must be relative and cannot contain '..'".into(),
                ));
            }
        }
    }
    Ok(normalized)
}

fn inspect_repository(bootstrap_root: &Path, directory: &Path) -> Option<RepositoryCandidate> {
    let directory = directory.canonicalize().ok()?;
    if directory != bootstrap_root && !directory.starts_with(bootstrap_root) {
        return None;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(&directory)
        .args(["rev-parse", "--show-toplevel", "--absolute-git-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut lines = stdout.lines();
    let top_level = PathBuf::from(lines.next()?.trim()).canonicalize().ok()?;
    let git_dir = PathBuf::from(lines.next()?.trim()).canonicalize().ok()?;
    if top_level != directory
        // A linked worktree keeps its administrative directory beneath the
        // primary checkout (for example, `.git/worktrees/feature`) rather
        // than beneath the linked worktree itself. Both locations must stay
        // inside the Agent's approved workspace, but they need not be nested
        // within one another.
        || !git_dir.starts_with(bootstrap_root)
        || (top_level != bootstrap_root && !top_level.starts_with(bootstrap_root))
    {
        return None;
    }
    let relative = top_level.strip_prefix(bootstrap_root).ok()?;
    let relative_path = if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative_path_string(relative)?
    };
    let origin = git_origin(&top_level);
    let github_repository = origin
        .as_deref()
        .and_then(github::parse_repository_remote)
        .map(|(owner, repo)| format!("{owner}/{repo}"));
    let identity = format!(
        "{:x}",
        Sha256::digest(format!("{relative_path}\0{}", origin.as_deref().unwrap_or("")).as_bytes())
    );
    Some(RepositoryCandidate {
        relative_path,
        root: top_level,
        origin,
        github_repository,
        identity,
    })
}

fn git_origin(repository: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|origin| !origin.is_empty())
}

fn relative_path_string(path: &Path) -> Option<String> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(|components| components.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::registry::AgentRegistry;

    fn git(repository: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }

    fn repository(path: &Path, remote: Option<&str>) {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q"]);
        if let Some(remote) = remote {
            git(path, &["remote", "add", "origin", remote]);
        }
    }

    fn manager() -> (Arc<Database>, AgentRepositoryManager) {
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("agent", "native")
            .unwrap();
        let manager = AgentRepositoryManager::new(db.clone());
        (db, manager)
    }

    #[test]
    fn automatically_adopts_one_nested_clone_and_persists_it() {
        let workspace = tempfile::tempdir().unwrap();
        repository(
            &workspace.path().join("clone"),
            Some("https://github.com/XpressAI/xpressclaw.git"),
        );
        let (_db, manager) = manager();
        let initial = manager.inspect("agent", workspace.path()).unwrap();
        assert_eq!(initial.state, RepositorySelectionState::Pending);
        assert!(initial.requires_boundary_change());

        let applied = manager.apply_boundary("agent", workspace.path()).unwrap();
        assert!(applied.changed);
        assert_eq!(applied.inspection.active_relative_path(), Some("clone"));
        assert_eq!(
            applied
                .inspection
                .active
                .unwrap()
                .github_repository
                .as_deref(),
            Some("XpressAI/xpressclaw")
        );
    }

    #[test]
    fn root_repository_wins_but_multiple_nested_repositories_are_ambiguous() {
        let workspace = tempfile::tempdir().unwrap();
        repository(&workspace.path().join("a"), None);
        repository(&workspace.path().join("b"), None);
        let (_db, manager) = manager();
        let ambiguous = manager.inspect("agent", workspace.path()).unwrap();
        assert_eq!(ambiguous.state, RepositorySelectionState::Ambiguous);
        assert!(!ambiguous.requires_boundary_change());

        repository(workspace.path(), None);
        let root = manager.inspect("agent", workspace.path()).unwrap();
        assert_eq!(root.state, RepositorySelectionState::Pending);
        let applied = manager.apply_boundary("agent", workspace.path()).unwrap();
        assert_eq!(applied.inspection.active_relative_path(), Some("."));
    }

    #[test]
    fn selection_rejects_traversal_and_symlink_escape() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        repository(outside.path(), None);
        let (_db, manager) = manager();
        assert!(manager
            .select("agent", workspace.path(), "../outside")
            .is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), workspace.path().join("escape")).unwrap();
            assert!(manager.select("agent", workspace.path(), "escape").is_err());
        }
    }

    #[test]
    fn discovery_accepts_linked_worktrees_inside_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let primary = workspace.path().join("primary");
        repository(&primary, None);
        git(
            &primary,
            &[
                "-c",
                "user.name=XpressClaw Tests",
                "-c",
                "user.email=tests@xpressclaw.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        );
        let linked = workspace.path().join("feature");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().unwrap(),
            ],
        );
        let (_db, manager) = manager();

        let inspection = manager.inspect("agent", workspace.path()).unwrap();
        assert_eq!(inspection.state, RepositorySelectionState::Ambiguous);
        assert_eq!(
            inspection
                .candidates
                .iter()
                .map(|candidate| candidate.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["feature", "primary"]
        );

        let selected = manager
            .select("agent", workspace.path(), "feature")
            .unwrap();
        assert_eq!(selected.inspection.active_relative_path(), Some("feature"));
    }

    #[test]
    fn discovery_rejects_git_metadata_outside_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let repository = workspace.path().join("checkout");
        let git_dir = outside.path().join("git-dir");
        let status = Command::new("git")
            .args(["init", "-q", "--separate-git-dir"])
            .arg(&git_dir)
            .arg(&repository)
            .status()
            .unwrap();
        assert!(status.success());
        let (_db, manager) = manager();

        let inspection = manager.inspect("agent", workspace.path()).unwrap();
        assert_eq!(inspection.state, RepositorySelectionState::NoRepository);
        assert!(inspection.candidates.is_empty());
        assert!(manager
            .select("agent", workspace.path(), "checkout")
            .is_err());
    }

    #[test]
    fn changing_repository_identity_clears_native_sessions() {
        let workspace = tempfile::tempdir().unwrap();
        repository(
            &workspace.path().join("clone"),
            Some("https://github.com/example/one.git"),
        );
        let (db, manager) = manager();
        manager.apply_boundary("agent", workspace.path()).unwrap();
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO logical_sessions (id, agent_id)
                 VALUES ('logical-session', 'agent')",
                [],
            )?;
            connection.execute(
                "INSERT INTO work_attempts
                    (id, session_id, runner, native_session_id, status)
                 VALUES ('attempt', 'logical-session', 'opencode', 'native-task', 'completed')",
                [],
            )
        })
        .unwrap();
        git(
            &workspace.path().join("clone"),
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/example/two.git",
            ],
        );
        let pending = manager.inspect("agent", workspace.path()).unwrap();
        assert_eq!(pending.state, RepositorySelectionState::Pending);
        assert!(pending.requires_boundary_change());
        let changed = manager.apply_boundary("agent", workspace.path()).unwrap();
        assert!(changed.changed);
        let native: Option<String> = db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT native_session_id FROM work_attempts WHERE id = 'attempt'",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        });
        assert_eq!(native, None);
    }

    #[test]
    fn explicit_clear_disables_automatic_re_adoption() {
        let workspace = tempfile::tempdir().unwrap();
        repository(&workspace.path().join("clone"), None);
        let (_db, manager) = manager();
        manager.apply_boundary("agent", workspace.path()).unwrap();
        let cleared = manager.clear("agent", workspace.path()).unwrap();
        assert_eq!(cleared.inspection.state, RepositorySelectionState::Cleared);
        assert!(cleared.inspection.active.is_none());
        assert!(!cleared.inspection.requires_boundary_change());
        assert_eq!(
            active_repository_root(&_db, "agent", workspace.path()).unwrap(),
            None
        );
    }

    #[test]
    fn legacy_root_repository_remains_available_before_first_turn_boundary() {
        let workspace = tempfile::tempdir().unwrap();
        repository(workspace.path(), None);
        let (db, _manager) = manager();

        assert_eq!(
            active_repository_root(&db, "agent", workspace.path()).unwrap(),
            Some(workspace.path().canonicalize().unwrap())
        );
    }

    #[test]
    fn discovery_is_bounded_and_ignores_internal_directories() {
        let workspace = tempfile::tempdir().unwrap();
        repository(&workspace.path().join(".xpressclaw/hidden"), None);
        let mut too_deep = workspace.path().to_path_buf();
        for component in ["one", "two", "three", "four", "five"] {
            too_deep.push(component);
        }
        repository(&too_deep, None);
        let (_db, manager) = manager();

        let inspection = manager.inspect("agent", workspace.path()).unwrap();
        assert_eq!(inspection.state, RepositorySelectionState::Ambiguous);
        assert!(inspection.candidates.is_empty());
        assert!(inspection.discovery_truncated);
    }

    #[test]
    fn truncated_discovery_never_guesses_that_one_candidate_is_unique() {
        let workspace = tempfile::tempdir().unwrap();
        repository(&workspace.path().join("000-clone"), None);
        for index in 0..MAX_SCANNED_DIRECTORIES + 10 {
            std::fs::create_dir(workspace.path().join(format!("directory-{index:04}"))).unwrap();
        }
        let (_db, manager) = manager();

        let inspection = manager.inspect("agent", workspace.path()).unwrap();
        assert!(inspection.discovery_truncated);
        assert_eq!(inspection.candidates.len(), 1);
        assert_eq!(inspection.state, RepositorySelectionState::Ambiguous);
        assert!(!inspection.requires_boundary_change());
    }

    #[test]
    fn explicit_selection_can_authorize_a_repository_beyond_discovery_depth() {
        let workspace = tempfile::tempdir().unwrap();
        let relative = "one/two/three/four/five/clone";
        repository(&workspace.path().join(relative), None);
        let (_db, manager) = manager();

        let selected = manager.select("agent", workspace.path(), relative).unwrap();
        assert_eq!(
            selected.inspection.state,
            RepositorySelectionState::Attached
        );
        assert_eq!(selected.inspection.active_relative_path(), Some(relative));
    }

    #[test]
    fn callback_capabilities_are_bound_to_one_agent() {
        let capability = agent_callback_capability("listener-secret", "agent-a");
        assert!(verify_agent_callback_capability(
            "listener-secret",
            "agent-a",
            &capability
        ));
        assert!(!verify_agent_callback_capability(
            "listener-secret",
            "agent-b",
            &capability
        ));
        assert!(!verify_agent_callback_capability(
            "different-secret",
            "agent-a",
            &capability
        ));
    }

    #[test]
    fn selection_survives_manager_restart_and_disappearance_invalidates_it() {
        let workspace = tempfile::tempdir().unwrap();
        let clone = workspace.path().join("clone");
        repository(&clone, Some("https://github.com/example/repo.git"));
        let (db, manager) = manager();
        manager.apply_boundary("agent", workspace.path()).unwrap();

        let restarted = AgentRepositoryManager::new(db.clone());
        assert_eq!(
            restarted
                .inspect("agent", workspace.path())
                .unwrap()
                .active_relative_path(),
            Some("clone")
        );
        std::fs::remove_dir_all(&clone).unwrap();
        let invalidated = restarted.apply_boundary("agent", workspace.path()).unwrap();
        assert!(invalidated.changed);
        assert!(invalidated.inspection.active.is_none());
        assert_eq!(
            invalidated.inspection.state,
            RepositorySelectionState::Missing
        );
    }

    #[test]
    fn missing_selection_does_not_jump_to_an_unrelated_repository() {
        let workspace = tempfile::tempdir().unwrap();
        let selected = workspace.path().join("selected");
        repository(&selected, None);
        let (_db, manager) = manager();
        manager.apply_boundary("agent", workspace.path()).unwrap();

        std::fs::remove_dir_all(&selected).unwrap();
        repository(&workspace.path().join("replacement"), None);
        let invalidated = manager.apply_boundary("agent", workspace.path()).unwrap();
        assert_eq!(
            invalidated.inspection.state,
            RepositorySelectionState::Missing
        );
        assert!(invalidated.inspection.active.is_none());
        assert_eq!(
            invalidated.inspection.selected_relative_path.as_deref(),
            Some("selected")
        );
    }

    #[test]
    fn agent_proposal_is_consumed_only_at_the_next_boundary() {
        let workspace = tempfile::tempdir().unwrap();
        repository(&workspace.path().join("clone"), None);
        let (_db, manager) = manager();
        let proposed = manager.propose("agent", workspace.path(), "clone").unwrap();
        assert_eq!(proposed.pending_relative_path.as_deref(), Some("clone"));
        assert_eq!(proposed.pending_action.as_deref(), Some("manual"));
        assert!(proposed.active.is_none());

        let applied = manager.apply_boundary("agent", workspace.path()).unwrap();
        assert!(applied.changed);
        assert_eq!(applied.inspection.active_relative_path(), Some("clone"));
        assert!(applied.inspection.pending_relative_path.is_none());
        assert!(applied.inspection.pending_action.is_none());
    }

    #[test]
    fn live_adoption_marker_reinvalidates_a_session_written_at_turn_completion() {
        let workspace = tempfile::tempdir().unwrap();
        repository(&workspace.path().join("clone"), None);
        let (db, manager) = manager();
        let selected = manager
            .select_live("agent", workspace.path(), "clone", None)
            .unwrap();
        let selected = selected.expect("the initial generation should still match");
        assert_eq!(selected.inspection.state, RepositorySelectionState::Pending);
        assert_eq!(selected.inspection.active_relative_path(), Some("clone"));
        assert_eq!(
            selected.inspection.pending_relative_path.as_deref(),
            Some("clone")
        );
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO logical_sessions (id, agent_id)
                 VALUES ('logical-session', 'agent')",
                [],
            )?;
            connection.execute(
                "INSERT INTO work_attempts
                    (id, session_id, runner, native_session_id, status)
                 VALUES ('attempt', 'logical-session', 'opencode', 'stale-native', 'completed')",
                [],
            )
        })
        .unwrap();

        let applied = manager.apply_boundary("agent", workspace.path()).unwrap();
        assert!(applied.changed);
        assert_eq!(applied.inspection.state, RepositorySelectionState::Attached);
        let native: Option<String> = db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT native_session_id FROM work_attempts WHERE id = 'attempt'",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        });
        assert_eq!(native, None);
    }

    #[test]
    fn live_adoption_does_not_overwrite_a_concurrent_user_choice() {
        let workspace = tempfile::tempdir().unwrap();
        repository(&workspace.path().join("clone"), None);
        let (_db, manager) = manager();
        let stale_generation = manager
            .inspect("agent", workspace.path())
            .unwrap()
            .generation();
        manager.propose_clear("agent", workspace.path()).unwrap();

        assert!(manager
            .select_live("agent", workspace.path(), "clone", stale_generation)
            .unwrap()
            .is_none());
        let inspection = manager.inspect("agent", workspace.path()).unwrap();
        assert_eq!(inspection.state, RepositorySelectionState::Pending);
        assert_eq!(inspection.pending_action.as_deref(), Some("cleared"));
        assert!(inspection.active.is_none());
    }

    #[test]
    fn clear_is_queued_until_the_next_boundary() {
        let workspace = tempfile::tempdir().unwrap();
        repository(&workspace.path().join("clone"), None);
        let (_db, manager) = manager();
        manager.apply_boundary("agent", workspace.path()).unwrap();

        let proposed = manager.propose_clear("agent", workspace.path()).unwrap();
        assert_eq!(proposed.state, RepositorySelectionState::Pending);
        assert_eq!(proposed.active_relative_path(), Some("clone"));
        assert_eq!(proposed.pending_action.as_deref(), Some("cleared"));

        let applied = manager.apply_boundary("agent", workspace.path()).unwrap();
        assert!(applied.changed);
        assert_eq!(applied.inspection.state, RepositorySelectionState::Cleared);
        assert!(applied.inspection.active.is_none());
    }

    #[test]
    fn disappearing_proposal_preserves_the_previous_repository() {
        let workspace = tempfile::tempdir().unwrap();
        let previous = workspace.path().join("previous");
        let proposed = workspace.path().join("proposed");
        repository(&previous, None);
        repository(&proposed, None);
        let (_db, manager) = manager();
        manager
            .select("agent", workspace.path(), "previous")
            .unwrap();
        manager
            .propose("agent", workspace.path(), "proposed")
            .unwrap();

        std::fs::remove_dir_all(proposed).unwrap();
        let before = manager.inspect("agent", workspace.path()).unwrap();
        assert_eq!(before.state, RepositorySelectionState::Missing);
        assert_eq!(before.active_relative_path(), Some("previous"));
        assert!(before.requires_boundary_change());
        assert!(!before.requires_runtime_restart());
        let applied = manager.apply_boundary("agent", workspace.path()).unwrap();
        assert!(!applied.changed);
        assert_eq!(applied.inspection.state, RepositorySelectionState::Attached);
        assert_eq!(applied.inspection.active_relative_path(), Some("previous"));
    }

    #[test]
    fn non_github_origin_remains_a_valid_git_repository_without_github() {
        let workspace = tempfile::tempdir().unwrap();
        repository(
            &workspace.path().join("clone"),
            Some("https://gitlab.com/example/repo.git"),
        );
        let (_db, manager) = manager();
        let applied = manager.apply_boundary("agent", workspace.path()).unwrap();
        let active = applied.inspection.active.unwrap();
        assert_eq!(active.relative_path, "clone");
        assert_eq!(active.github_repository, None);
    }
}
