use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

use crate::error::{Error, Result};
use crate::external_tools::path_for_external_tool;

use super::manifest::GitStoreConfig;

pub(super) struct GitCheckout {
    _temporary: TempDir,
    root: PathBuf,
    branch: String,
    remote_branch_existed: bool,
}

impl GitCheckout {
    pub(super) fn open(store: &GitStoreConfig, allow_new_branch: bool) -> Result<Self> {
        require_git()?;
        let temporary = tempfile::Builder::new()
            .prefix("xpressclaw-sync-")
            .tempdir()
            .map_err(|error| {
                Error::Sync(format!("failed to create temporary checkout: {error}"))
            })?;
        let root = temporary.path().join("repository");
        fs::create_dir(&root).map_err(|error| {
            Error::Sync(format!(
                "failed to create temporary Git repository: {error}"
            ))
        })?;
        run_local(
            &root,
            ["init", "--quiet"],
            "initialize temporary Git repository",
        )?;
        run_local(
            &root,
            ["remote", "add", "origin", store.remote.as_str()],
            "configure temporary Git remote",
        )?;

        let remote_ref = format!("refs/heads/{}", store.branch);
        let exists = command(&root, ["ls-remote", "--exit-code", "origin", &remote_ref])
            .output()
            .map_err(|error| git_spawn_error("inspect the remote branch", error))?;
        let remote_branch_existed = match exists.status.code() {
            Some(0) => true,
            Some(2) => false,
            _ => return Err(remote_error("inspect the remote branch", &exists)),
        };

        if remote_branch_existed {
            let destination = format!("{remote_ref}:refs/remotes/origin/{}", store.branch);
            run_remote(
                &root,
                ["fetch", "--depth=1", "--no-tags", "origin", &destination],
                "fetch the synchronization branch",
            )?;
            let start = format!("refs/remotes/origin/{}", store.branch);
            run_local(
                &root,
                ["checkout", "--quiet", "-b", store.branch.as_str(), &start],
                "check out the synchronization branch",
            )?;
        } else if allow_new_branch {
            run_local(
                &root,
                ["checkout", "--quiet", "--orphan", store.branch.as_str()],
                "create the synchronization branch",
            )?;
        } else {
            return Err(Error::Sync(format!(
                "remote synchronization branch '{}' does not exist",
                store.branch
            )));
        }

        Ok(Self {
            _temporary: temporary,
            root,
            branch: store.branch.clone(),
            remote_branch_existed,
        })
    }

    pub(super) fn store_root(&self, store: &GitStoreConfig) -> Result<PathBuf> {
        let mut current = self.root.clone();
        for component in Path::new(&store.path).components() {
            current.push(component.as_os_str());
            if current.exists() {
                let metadata = fs::symlink_metadata(&current).map_err(|error| {
                    Error::Sync(format!("failed to inspect synchronization path: {error}"))
                })?;
                if metadata.file_type().is_symlink() {
                    return Err(Error::Sync(format!(
                        "synchronization path '{}' traverses a symlink",
                        store.path
                    )));
                }
            }
        }
        Ok(current)
    }

    pub(super) fn head(&self) -> Result<Option<String>> {
        if !self.remote_branch_existed {
            return Ok(None);
        }
        let output = run_local(&self.root, ["rev-parse", "HEAD"], "read Git commit")?;
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    }

    pub(super) fn commit_and_push(&self, store_path: &str, message: &str) -> Result<String> {
        run_local(
            &self.root,
            ["add", "--all", "--", store_path],
            "stage synchronized Project state",
        )?;
        let changed = command(&self.root, ["diff", "--cached", "--quiet"])
            .status()
            .map_err(|error| git_spawn_error("inspect synchronized changes", error))?;
        match changed.code() {
            Some(0) => {
                return self.head()?.ok_or_else(|| {
                    Error::Sync("the new synchronization branch has no content to publish".into())
                });
            }
            Some(1) => {}
            _ => {
                return Err(Error::Sync(
                    "Git could not inspect the staged synchronization changes".into(),
                ));
            }
        }

        run_local(
            &self.root,
            [
                "-c",
                "user.name=XpressClaw Sync",
                "-c",
                "user.email=xpressclaw@localhost",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
            "commit synchronized Project state",
        )?;
        let destination = format!("HEAD:refs/heads/{}", self.branch);
        run_remote(
            &self.root,
            ["push", "origin", &destination],
            "publish synchronized Project state",
        )?;
        let output = run_local(&self.root, ["rev-parse", "HEAD"], "read published commit")?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

fn require_git() -> Result<()> {
    let output = Command::new("git")
        .arg("--version")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|_| {
            Error::Sync(
                "Git is required for Project synchronization but was not found in PATH".into(),
            )
        })?;
    if !output.status.success() {
        return Err(Error::Sync(
            "Git is required for Project synchronization but is not usable".into(),
        ));
    }
    Ok(())
}

fn command<I, S>(root: &Path, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(path_for_external_tool(root))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

fn run_local<I, S>(root: &Path, args: I, operation: &str) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = command(root, args)
        .output()
        .map_err(|error| git_spawn_error(operation, error))?;
    if output.status.success() {
        Ok(output)
    } else {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim().lines().last().unwrap_or("unknown Git error");
        Err(Error::Sync(format!("failed to {operation}: {detail}")))
    }
}

fn run_remote<I, S>(root: &Path, args: I, operation: &str) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = command(root, args)
        .output()
        .map_err(|error| git_spawn_error(operation, error))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(remote_error(operation, &output))
    }
}

fn git_spawn_error(operation: &str, error: std::io::Error) -> Error {
    Error::Sync(format!("failed to {operation}: {error}"))
}

fn remote_error(operation: &str, output: &Output) -> Error {
    let hint = if output.status.code() == Some(1) {
        "the remote rejected the operation"
    } else {
        "verify the remote, branch, network access, and local Git credentials"
    };
    Error::Sync(format!("failed to {operation}; {hint}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn checkout_can_create_and_reopen_a_branch() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "--quiet"]);
        let store = GitStoreConfig {
            remote: remote.path().display().to_string(),
            branch: "shared".into(),
            path: "projects/one".into(),
        };
        let checkout = GitCheckout::open(&store, true).unwrap();
        let root = checkout.store_root(&store).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("state.txt"), "one").unwrap();
        let commit = checkout
            .commit_and_push(&store.path, "Initialize Project one")
            .unwrap();

        let reopened = GitCheckout::open(&store, false).unwrap();
        assert_eq!(reopened.head().unwrap().as_deref(), Some(commit.as_str()));
        assert_eq!(
            fs::read_to_string(reopened.store_root(&store).unwrap().join("state.txt")).unwrap(),
            "one"
        );
    }
}
