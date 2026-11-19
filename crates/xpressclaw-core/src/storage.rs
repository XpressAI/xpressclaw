//! Project-local worktree inventory and retention. Git remains the authority
//! for registrations; recorded paths are never accepted as deletion authority.
use crate::{
    config::Config,
    db::Database,
    error::{Error, Result},
    external_tools::path_for_external_tool,
    projects::ProjectManager,
    repositories::{bounded_command_output, AgentRepositoryManager},
    workers::native::resolved_workspace,
};
use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionPolicy {
    pub enabled: bool,
    pub days: u32,
}
impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            days: 14,
        }
    }
}
impl RetentionPolicy {
    pub fn validate(&self) -> Result<()> {
        if !(1..=365).contains(&self.days) {
            return Err(Error::Config(
                "Retention must be between 1 and 365 days".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct StorageState {
    pub policy: RetentionPolicy,
    pub worktrees: BTreeMap<String, Observation>,
    pub last_cleanup_at: Option<i64>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Observation {
    pub fingerprint: String,
    pub first_seen_at: i64,
    pub eligible_since: Option<i64>,
}
pub fn load_state(db: &Database, project: &str) -> Result<StorageState> {
    db.with_conn(|conn| {
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM config WHERE key=?1",
                [format!("project_storage:{project}")],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value
            .map(|value| serde_json::from_str(&value))
            .transpose()?
            .unwrap_or_default())
    })
}
pub fn save_state(db: &Database, project: &str, state: &StorageState) -> Result<()> {
    let value = serde_json::to_string(state)?;
    db.with_conn(|conn| {
        conn.execute("INSERT INTO config(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [format!("project_storage:{project}"), value])?;
        Ok(())
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct Worktree {
    pub id: String,
    #[serde(serialize_with = "serialize_path")]
    pub repository: PathBuf,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub agent_ids: Vec<String>,
    pub bytes: u64,
    pub size_complete: bool,
    pub missing: bool,
    pub eligible: bool,
    pub reasons: Vec<String>,
    pub eligible_since: Option<i64>,
    pub first_seen_at: i64,
    pub fingerprint: String,
    #[serde(skip)]
    bootstrap: PathBuf,
}
#[derive(Serialize)]
pub struct WorktreeInventory {
    pub policy: RetentionPolicy,
    pub worktrees: Vec<Worktree>,
    pub scanned_at: i64,
    pub last_cleanup_at: Option<i64>,
    pub errors: Vec<String>,
}
#[derive(Default, Debug)]
struct Entry {
    path: PathBuf,
    head: String,
    branch: Option<String>,
    locked: bool,
    bare: bool,
}

fn serialize_path<S: serde::Serializer>(
    path: &Path,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    path_for_external_tool(path).serialize(serializer)
}

/// Resolve filesystem aliases before comparing paths. A missing worktree still
/// has a canonical identity through its existing parent, so Git's stale
/// registration can be inspected without treating it as outside the workspace.
fn canonical_worktree_path(path: &Path) -> Option<PathBuf> {
    let mut parent = path;
    let mut missing = Vec::new();
    loop {
        match parent.canonicalize() {
            Ok(mut canonical) => {
                for component in missing.into_iter().rev() {
                    canonical.push(component);
                }
                return Some(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(parent.file_name()?.to_owned());
                parent = parent.parent()?;
            }
            Err(_) => return None,
        }
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<Output> {
    let mut command = git_command(repo);
    command.args(args);
    bounded_command_output(&mut command, Duration::from_secs(5)).ok_or_else(|| {
        Error::Backend("Git inspection timed out or exceeded its output limit".into())
    })
}
fn git_command(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(path_for_external_tool(repo))
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}
fn git_text(repo: &Path, args: &[&str]) -> Option<String> {
    let output = git(repo, args).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|text| text.trim().to_owned())
}
fn entries(repo: &Path) -> Result<Vec<Entry>> {
    let output = git(repo, &["worktree", "list", "--porcelain", "-z"])?;
    if !output.status.success() {
        return Err(Error::Backend("Could not list Git worktrees".into()));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| Error::Backend("Worktree paths must be valid UTF-8".into()))?;
    let mut result = Vec::new();
    let mut current = Entry::default();
    for field in text.split('\0') {
        if field.is_empty() {
            if !current.path.as_os_str().is_empty() {
                result.push(std::mem::take(&mut current));
            }
        } else if let Some(path) = field.strip_prefix("worktree ") {
            current.path = path.into();
        } else if let Some(head) = field.strip_prefix("HEAD ") {
            current.head = head.into();
        } else if let Some(branch) = field.strip_prefix("branch ") {
            current.branch = Some(branch.trim_start_matches("refs/heads/").into());
        } else if field == "locked" || field.starts_with("locked ") {
            current.locked = true;
        } else if field == "bare" {
            current.bare = true;
        }
    }
    Ok(result)
}

fn default_branch(repo: &Path) -> Option<String> {
    git_text(repo, &["symbolic-ref", "refs/remotes/origin/HEAD"]).or_else(|| {
        [
            "refs/remotes/origin/main",
            "refs/remotes/origin/master",
            "refs/heads/main",
            "refs/heads/master",
        ]
        .into_iter()
        .find(|reference| git_text(repo, &["rev-parse", "--verify", reference]).is_some())
        .map(str::to_owned)
    })
}

fn process_paths() -> Vec<PathBuf> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );
    system
        .processes()
        .values()
        .flat_map(|process| {
            [process.cwd(), process.exe()]
                .into_iter()
                .flatten()
                .filter_map(canonical_worktree_path)
        })
        .collect()
}

// Only recognised generated directories may accompany an otherwise clean
// worktree. In particular, ignored credentials and personal files protect it.
fn generated_path(root: &Path, path: &str) -> bool {
    let path = Path::new(path.trim_end_matches('/'));
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !root.join(path).is_dir() {
        return false;
    }
    let generated = match name {
        "node_modules" | ".svelte-kit" | ".next" | ".pytest_cache" | "__pycache__"
        | ".mypy_cache" | ".ruff_cache" => true,
        "target" => std::fs::read_to_string(root.join(path).join("CACHEDIR.TAG"))
            .is_ok_and(|tag| tag.starts_with("Signature: 8a477f597d28d172789f06886806bc55")),
        _ => false,
    };
    generated && !contains_repository(&root.join(path))
}

fn contains_repository(root: &Path) -> bool {
    let mut pending = vec![root.to_owned()];
    let mut inspected = 0usize;
    while let Some(path) = pending.pop() {
        inspected += 1;
        if inspected > 200_000 {
            return true;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return true;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return true;
            };
            if entry.file_name() == ".git" {
                return true;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(entry.path()),
                Ok(_) => {}
                Err(_) => return true,
            }
        }
    }
    false
}

fn status_reasons(path: &Path) -> Vec<String> {
    let mut reasons = Vec::new();
    match git(
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignored=matching",
            "--ignore-submodules=none",
        ],
    ) {
        Ok(output) if output.status.success() => match String::from_utf8(output.stdout) {
            Ok(status) => {
                if status
                    .split('\0')
                    .filter(|field| !field.is_empty())
                    .any(|field| !field.starts_with("!! "))
                {
                    reasons.push("Uncommitted or untracked files".into());
                }
                if status
                    .split('\0')
                    .filter_map(|field| field.strip_prefix("!! "))
                    .any(|ignored| !generated_path(path, ignored))
                {
                    reasons.push("Ignored files outside recognised build caches".into());
                }
            }
            Err(_) => reasons.push("Git status could not be decoded".into()),
        },
        _ => reasons.push("Git status could not be checked".into()),
    }
    reasons
}

fn submodule_reasons(root: &Path, bootstrap: &Path) -> (Vec<String>, bool) {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_owned());
    let mut reasons = Vec::new();
    let mut pending = vec![root.to_owned()];
    let mut initialized = false;
    let mut inspected = 0;
    while let Some(repo) = pending.pop() {
        inspected += 1;
        if inspected > 64 {
            reasons.push("Submodule inspection exceeded its limit".into());
            break;
        }
        let Some(files) = git_text(&repo, &["ls-files", "--stage", "-z"]) else {
            reasons.push("Could not inspect submodules".into());
            break;
        };
        for entry in files
            .split('\0')
            .filter(|entry| entry.starts_with("160000 "))
        {
            let Some((_, relative)) = entry.split_once('\t') else {
                reasons.push("Invalid submodule index entry".into());
                continue;
            };
            let path = repo.join(relative);
            if !path.join(".git").exists() {
                continue;
            }
            initialized = true;
            let Ok(path) = path.canonicalize() else {
                reasons.push("Submodule path is unavailable".into());
                continue;
            };
            if !path.starts_with(&root) {
                reasons.push("Submodule is outside the worktree".into());
                continue;
            }
            if crate::repositories::inspect_repository(bootstrap, &path).is_none() {
                reasons.push(format!("Submodule {relative} metadata is outside the approved workspace or could not be verified"));
                continue;
            }
            reasons.extend(
                status_reasons(&path)
                    .into_iter()
                    .map(|reason| format!("Submodule {relative}: {reason}")),
            );
            // --force is needed by Git to remove initialized submodules. Only
            // use it when every submodule ref (including stashes) is already
            // reachable from a remote ref, and its working files are clean.
            match git_text(
                &path,
                &["rev-list", "--max-count=1", "--all", "--not", "--remotes"],
            ) {
                Some(local) if local.is_empty() => {}
                _ => reasons.push(format!(
                    "Submodule {relative} has local-only commits or could not be verified"
                )),
            }
            pending.push(path);
        }
    }
    (reasons, initialized)
}

fn directory_size(root: &Path, deadline: Instant) -> (u64, bool) {
    let mut queue = vec![root.to_path_buf()];
    let mut bytes = 0u64;
    let mut count = 0;
    let mut complete = true;
    while let Some(path) = queue.pop() {
        if count >= 200_000 || Instant::now() >= deadline {
            return (bytes, false);
        }
        count += 1;
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            complete = false;
            continue;
        };
        if metadata.is_dir() {
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => queue.push(entry.path()),
                            Err(_) => complete = false,
                        }
                    }
                }
                Err(_) => complete = false,
            }
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                bytes = bytes.saturating_add(metadata.blocks().saturating_mul(512));
            }
            #[cfg(not(unix))]
            {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    (bytes, complete)
}

/// Inspect only repositories already authorised inside configured workspaces.
pub fn inventory(
    db: Arc<Database>,
    config: &Config,
    project_id: &str,
    measure: bool,
) -> Result<WorktreeInventory> {
    let project = ProjectManager::new(db.clone()).get(project_id)?;
    let mut roots = BTreeMap::<PathBuf, (PathBuf, Vec<String>)>::new();
    let mut protected = Vec::new();
    let mut errors = Vec::new();
    for agent in &config.agents {
        let bootstrap = resolved_workspace(config, agent);
        let Ok(inspection) =
            AgentRepositoryManager::new(db.clone()).inspect(&agent.name, &bootstrap)
        else {
            if project.agent_ids.contains(&agent.name) {
                errors.push(format!("{}: workspace could not be inspected", agent.name));
            }
            continue;
        };
        protected.push(inspection.bootstrap_root.clone());
        if let Some(active) = &inspection.active {
            protected.push(active.root.clone());
        }
        if !project.agent_ids.contains(&agent.name) {
            continue;
        }
        for candidate in inspection.candidates.iter().chain(inspection.active.iter()) {
            // The primary checkout owns the registry, even when an Agent has
            // selected a linked worktree as its active repository.
            let Some(repo) = candidate.git_common_dir().parent().filter(|_| {
                candidate
                    .git_common_dir()
                    .file_name()
                    .is_some_and(|name| name == ".git")
            }) else {
                continue;
            };
            let root = roots
                .entry(repo.to_owned())
                .or_insert_with(|| (inspection.bootstrap_root.clone(), Vec::new()));
            if !root.1.contains(&agent.name) {
                root.1.push(agent.name.clone());
            }
        }
    }
    let processes = process_paths();
    let now = Utc::now().timestamp();
    let mut state = load_state(&db, project_id)?;
    let mut worktrees = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    for (repo, (bootstrap, agents)) in roots {
        let list = match entries(&repo) {
            Ok(list) => list,
            Err(error) => {
                errors.push(format!("{}: {error}", repo.display()));
                continue;
            }
        };
        let paths = list
            .iter()
            .map(|entry| canonical_worktree_path(&entry.path))
            .collect::<Vec<_>>();
        let base = default_branch(&repo);
        for (entry, canonical) in list.iter().zip(&paths) {
            let mut reasons = Vec::new();
            let missing = !entry.path.exists();
            let path = canonical.as_ref().unwrap_or(&entry.path);
            if entry.bare || path == &repo {
                reasons.push("Primary checkout".into());
            }
            if entry.locked {
                reasons.push("Worktree is locked".into());
            }
            let within = path.starts_with(&bootstrap);
            if !within {
                reasons.push("Outside the configured project workspace".into());
            }
            let mut component = entry.path.as_path();
            while canonical_worktree_path(component)
                .is_some_and(|path| path.starts_with(&bootstrap) && path != bootstrap)
            {
                if std::fs::symlink_metadata(component)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    reasons.push("Worktree path contains a symbolic link".into());
                    break;
                }
                let Some(parent) = component.parent() else {
                    break;
                };
                component = parent;
            }
            if protected
                .iter()
                .any(|active| active == path || active.starts_with(path))
            {
                reasons.push("An Agent uses this workspace or repository".into());
            }
            if paths
                .iter()
                .flatten()
                .any(|other| other != path && other.starts_with(path))
            {
                reasons.push("Contains another registered worktree".into());
            }
            if processes.iter().any(|active| active.starts_with(path)) {
                reasons.push("A running process uses this worktree".into());
            }
            if entry.head.is_empty()
                || base.as_ref().is_none_or(|base| {
                    !git(&repo, &["merge-base", "--is-ancestor", &entry.head, base])
                        .is_ok_and(|output| output.status.success())
                })
            {
                reasons.push("Branch is not merged into the local default-branch reference".into());
            }
            if !missing && canonical.is_some() && within && path != &repo {
                if crate::repositories::inspect_repository(&bootstrap, path)
                    .is_some_and(|candidate| candidate.git_common_dir() == repo.join(".git"))
                {
                    reasons.extend(status_reasons(path));
                    reasons.extend(submodule_reasons(path, &bootstrap).0);
                } else {
                    reasons.push("Worktree metadata does not match its repository inside the approved workspace".into());
                }
            } else if canonical.is_none() {
                reasons.push("Worktree path is unavailable".into());
            }
            let id = format!(
                "worktree:{:x}",
                Sha256::digest(format!("{}\0{}", repo.display(), entry.path.display()))
            );
            let fingerprint = format!(
                "{:x}",
                Sha256::digest(format!("{}:{:?}:{}", entry.head, entry.branch, missing))
            );
            let eligible = reasons.is_empty();
            let observation = state.worktrees.entry(id.clone()).or_insert(Observation {
                fingerprint: fingerprint.clone(),
                first_seen_at: now,
                eligible_since: None,
            });
            if observation.fingerprint != fingerprint || !eligible {
                observation.eligible_since = None;
            }
            observation.fingerprint = fingerprint.clone();
            if eligible && observation.eligible_since.is_none() {
                observation.eligible_since = Some(now);
            }
            let (bytes, size_complete) = if missing {
                (0, true)
            } else if measure && within && path != &repo {
                directory_size(path, deadline)
            } else {
                (0, false)
            };
            worktrees.push(Worktree {
                id,
                repository: repo.clone(),
                path: path.clone(),
                branch: entry.branch.clone(),
                head: entry.head.clone(),
                agent_ids: agents.clone(),
                bytes,
                size_complete,
                missing,
                eligible,
                reasons,
                first_seen_at: observation.first_seen_at,
                eligible_since: observation.eligible_since,
                fingerprint,
                bootstrap: bootstrap.clone(),
            });
        }
    }
    let visible = worktrees
        .iter()
        .map(|tree| tree.id.clone())
        .collect::<BTreeSet<_>>();
    state.worktrees.retain(|id, _| visible.contains(id));
    save_state(&db, project_id, &state)?;
    Ok(WorktreeInventory {
        policy: state.policy,
        worktrees,
        scanned_at: now,
        last_cleanup_at: state.last_cleanup_at,
        errors,
    })
}

/// Callers hold all affected Agent maintenance leases and rescan immediately
/// before calling this. Removal rechecks status and submodule references.
pub fn remove_worktree(tree: &Worktree) -> Result<()> {
    if !tree.eligible {
        return Err(Error::Backend(
            "Worktree is no longer eligible for cleanup".into(),
        ));
    }
    let registered = entries(&tree.repository)?
        .into_iter()
        .find(|entry| canonical_worktree_path(&entry.path).as_ref() == Some(&tree.path))
        .ok_or_else(|| {
            Error::Backend("Worktree registration changed; refresh the inventory".into())
        })?;
    if registered.head != tree.head || registered.branch != tree.branch || registered.locked {
        return Err(Error::Backend(
            "Worktree changed or was locked after inspection".into(),
        ));
    }
    let mut initialized_submodules = false;
    if !tree.missing {
        if crate::repositories::inspect_repository(&tree.bootstrap, &tree.path)
            .is_none_or(|candidate| candidate.git_common_dir() != tree.repository.join(".git"))
        {
            return Err(Error::Backend(
                "Worktree metadata changed since inspection".into(),
            ));
        }
        let reasons = status_reasons(&tree.path);
        if !reasons.is_empty() {
            return Err(Error::Backend(reasons.join("; ")));
        }
        let (reasons, initialized) = submodule_reasons(&tree.path, &tree.bootstrap);
        if !reasons.is_empty() {
            return Err(Error::Backend(reasons.join("; ")));
        }
        initialized_submodules = initialized;
    }
    let mut command = git_command(&tree.repository);
    command.args(["worktree", "remove"]);
    if initialized_submodules {
        command.arg("--force");
    }
    let output = command
        .arg("--")
        .arg(path_for_external_tool(&tree.path))
        .output()
        .map_err(|error| Error::Backend(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::Backend(format!(
            "Git kept {}: {}",
            tree.path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agents::registry::AgentRegistry,
        config::{AgentConfig, NativeRunnerConfig},
    };

    fn run(root: &Path, args: &[&str]) {
        let output = git(root, args).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn fixture() -> (tempfile::TempDir, Arc<Database>, Config) {
        let root = tempfile::tempdir().unwrap();
        run(root.path(), &["init", "-b", "main"]);
        run(
            root.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        run(root.path(), &["config", "user.name", "Storage test"]);
        std::fs::write(
            root.path().join(".gitignore"),
            ".worktrees/\nnode_modules/\n.env\n",
        )
        .unwrap();
        std::fs::write(root.path().join("source.txt"), "source").unwrap();
        run(root.path(), &["add", "."]);
        run(root.path(), &["commit", "-m", "initial"]);
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
        (root, db, config)
    }
    fn add(root: &Path, branch: &str) -> PathBuf {
        let path = root.join(".worktrees").join(branch);
        run(
            root,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        path.canonicalize().unwrap()
    }
    #[test]
    fn cleanup_removes_only_finished_clean_worktrees_and_preserves_branches() {
        let (root, db, config) = fixture();
        let clean = add(root.path(), "clean");
        std::fs::create_dir(clean.join("node_modules")).unwrap();
        std::fs::write(clean.join("node_modules/package.js"), "rebuildable").unwrap();
        let dirty = add(root.path(), "dirty");
        std::fs::write(dirty.join("source.txt"), "uncommitted").unwrap();
        let personal = add(root.path(), "personal");
        std::fs::write(personal.join(".env"), "private fixture").unwrap();
        let unmerged = add(root.path(), "unmerged");
        std::fs::write(unmerged.join("source.txt"), "new commit").unwrap();
        run(&unmerged, &["commit", "-am", "unmerged work"]);
        let locked = add(root.path(), "locked");
        run(
            root.path(),
            &[
                "worktree",
                "lock",
                path_for_external_tool(&locked).to_str().unwrap(),
            ],
        );
        let report = inventory(db.clone(), &config, "agent", true).unwrap();
        let eligible = report
            .worktrees
            .iter()
            .filter(|tree| tree.eligible)
            .collect::<Vec<_>>();
        assert_eq!(eligible.len(), 1, "{:#?}", report.worktrees);
        assert_eq!(eligible[0].path, clean);
        assert!(eligible[0]
            .path
            .starts_with(eligible[0].repository.join(".worktrees")));
        let public = serde_json::to_value(eligible[0]).unwrap();
        assert_eq!(
            public["path"],
            path_for_external_tool(&clean).to_str().unwrap()
        );
        remove_worktree(eligible[0]).unwrap();
        assert!(!clean.exists());
        assert!(git_text(root.path(), &["rev-parse", "--verify", "refs/heads/clean"]).is_some());
        for kept in [dirty, personal, unmerged, locked] {
            assert!(kept.exists());
        }
    }

    #[test]
    fn new_ignored_files_invalidate_cleanup_and_reset_retention() {
        let (root, db, config) = fixture();
        let path = add(root.path(), "finished");
        let first = inventory(db.clone(), &config, "agent", false).unwrap();
        let tree = first.worktrees.iter().find(|tree| tree.eligible).unwrap();
        assert!(tree.eligible_since.is_some());
        let mut saved = load_state(&db, "agent").unwrap();
        saved.worktrees.get_mut(&tree.id).unwrap().eligible_since = Some(1);
        save_state(&db, "agent", &saved).unwrap();
        std::fs::write(path.join(".env"), "private fixture").unwrap();
        assert!(remove_worktree(tree).is_err());
        assert!(path.join(".env").exists());
        let next = inventory(db, &config, "agent", false).unwrap();
        let tree = next
            .worktrees
            .iter()
            .find(|tree| tree.path == path)
            .unwrap();
        assert!(!tree.eligible);
        assert!(tree.eligible_since.is_none());
    }

    #[test]
    fn nested_worktrees_keep_their_parent_checkout() {
        let (root, db, config) = fixture();
        let parent = add(root.path(), "parent");
        let child = parent.join(".worktrees/child");
        run(
            root.path(),
            &[
                "worktree",
                "add",
                "-b",
                "child",
                path_for_external_tool(&child).to_str().unwrap(),
            ],
        );
        let report = inventory(db, &config, "agent", false).unwrap();
        let tree = report
            .worktrees
            .iter()
            .find(|tree| tree.path == parent)
            .unwrap();
        assert!(!tree.eligible);
        assert!(tree
            .reasons
            .iter()
            .any(|reason| reason.contains("another registered worktree")));
        assert!(remove_worktree(tree).is_err());
        assert!(child.join("source.txt").exists());
    }

    #[test]
    fn missing_worktrees_keep_their_canonical_identity_and_branch() {
        let (root, db, config) = fixture();
        let path = add(root.path(), "missing");
        std::fs::remove_dir_all(&path).unwrap();
        let report = inventory(db, &config, "agent", false).unwrap();
        let tree = report
            .worktrees
            .iter()
            .find(|tree| tree.path == path)
            .unwrap();
        assert!(tree.missing);
        assert!(tree.eligible, "{:?}", tree.reasons);
        assert!(tree.path.starts_with(tree.repository.join(".worktrees")));
        remove_worktree(tree).unwrap();
        assert!(git_text(
            root.path(),
            &["rev-parse", "--verify", "refs/heads/missing"]
        )
        .is_some());
        assert!(!entries(root.path())
            .unwrap()
            .iter()
            .any(|entry| canonical_worktree_path(&entry.path).as_ref() == Some(&path)));
    }

    #[test]
    fn clean_published_submodules_can_be_removed_but_local_submodule_work_is_kept() {
        let (root, db, config) = fixture();
        let (dependency, _, _) = fixture();
        run(
            root.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                dependency.path().to_str().unwrap(),
                "external/dependency",
            ],
        );
        run(root.path(), &["commit", "-am", "add dependency"]);
        let path = add(root.path(), "with-submodule");
        run(
            &path,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
            ],
        );
        let report = inventory(db.clone(), &config, "agent", false).unwrap();
        let tree = report
            .worktrees
            .iter()
            .find(|tree| tree.path == path)
            .unwrap();
        assert!(tree.eligible, "{:?}", tree.reasons);
        let dependency_path = path.join("external/dependency");
        std::fs::write(dependency_path.join(".env"), "private fixture").unwrap();
        assert!(remove_worktree(tree).is_err());
        assert!(dependency_path.join(".env").exists());
        std::fs::remove_file(dependency_path.join(".env")).unwrap();
        let original = git_text(&dependency_path, &["rev-parse", "HEAD"]).unwrap();
        run(&dependency_path, &["switch", "-c", "private-work"]);
        std::fs::write(dependency_path.join("source.txt"), "local commit").unwrap();
        run(
            &dependency_path,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-am",
                "unpublished work",
            ],
        );
        run(&dependency_path, &["checkout", "--detach", &original]);
        assert!(remove_worktree(tree).is_err());
        assert!(git_text(
            &dependency_path,
            &["rev-parse", "--verify", "refs/heads/private-work"]
        )
        .is_some());
        run(&dependency_path, &["branch", "-D", "private-work"]);
        remove_worktree(tree).unwrap();
        assert!(!path.exists());
        assert!(root.path().join("external/dependency/source.txt").exists());
        assert!(git_text(
            root.path(),
            &["rev-parse", "--verify", "refs/heads/with-submodule"]
        )
        .is_some());
    }

    #[test]
    fn redirected_git_metadata_is_never_cleanup_authority() {
        let (root, db, config) = fixture();
        let path = add(root.path(), "redirected");
        let (outside, _, _) = fixture();
        std::fs::write(
            path.join(".git"),
            format!("gitdir: {}\n", outside.path().join(".git").display()),
        )
        .unwrap();
        let report = inventory(db, &config, "agent", false).unwrap();
        let tree = report
            .worktrees
            .iter()
            .find(|tree| tree.path == path)
            .unwrap();
        assert!(!tree.eligible);
        assert!(tree
            .reasons
            .iter()
            .any(|reason| reason.contains("metadata")));
        assert!(remove_worktree(tree).is_err());
        assert!(outside.path().join("source.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_process_running_in_a_worktree_keeps_it_out_of_cleanup() {
        let (root, db, config) = fixture();
        let path = add(root.path(), "running");
        let mut child = Command::new("sleep")
            .arg("30")
            .current_dir(&path)
            .spawn()
            .unwrap();
        let report = inventory(db, &config, "agent", false);
        let _ = child.kill();
        let _ = child.wait();
        let report = report.unwrap();
        let tree = report
            .worktrees
            .iter()
            .find(|tree| tree.path == path)
            .unwrap();
        assert!(!tree.eligible);
        assert!(tree
            .reasons
            .iter()
            .any(|reason| reason.contains("running process")));
    }
}
