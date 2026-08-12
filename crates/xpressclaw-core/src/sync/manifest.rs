use std::path::{Component, Path, PathBuf};
use std::{fs, io::Write};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{Error, Result};

pub const MANIFEST_FILE: &str = ".xpressclaw.yml";
pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectSyncManifest {
    pub version: u32,
    pub project_id: String,
    pub store: GitStoreConfig,
    #[serde(default)]
    pub share: ShareConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitStoreConfig {
    pub remote: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ShareConfig {
    /// Project memory is collaboration data, but installations may choose to
    /// keep their accumulated notes private while sharing the rest of a Project.
    pub project_memory: bool,
}

impl Default for ShareConfig {
    fn default() -> Self {
        Self {
            project_memory: true,
        }
    }
}

fn default_branch() -> String {
    "main".to_string()
}

impl ProjectSyncManifest {
    pub fn new(
        project_id: impl Into<String>,
        remote: impl Into<String>,
        branch: impl Into<String>,
        store_path: impl Into<String>,
    ) -> Result<Self> {
        let manifest = Self {
            version: MANIFEST_VERSION,
            project_id: project_id.into(),
            store: GitStoreConfig {
                remote: remote.into(),
                branch: branch.into(),
                path: store_path.into(),
            },
            share: ShareConfig::default(),
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn load(project_dir: &Path) -> Result<Self> {
        let path = project_dir.join(MANIFEST_FILE);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            Error::Sync(format!("failed to inspect {}: {error}", path.display()))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Sync(format!(
                "{} must be a regular file, not a symlink",
                path.display()
            )));
        }
        if metadata.len() > 64 * 1024 {
            return Err(Error::Sync(format!(
                "{} is larger than the 64 KiB manifest limit",
                path.display()
            )));
        }
        let contents = fs::read_to_string(&path)
            .map_err(|error| Error::Sync(format!("failed to read {}: {error}", path.display())))?;
        reject_manifest_text(&contents)?;
        let manifest: Self = serde_yaml::from_str(&contents)
            .map_err(|error| Error::Sync(format!("invalid {}: {error}", path.display())))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn save_new(&self, project_dir: &Path) -> Result<PathBuf> {
        self.validate()?;
        let path = project_dir.join(MANIFEST_FILE);
        if path.exists() {
            return Err(Error::Sync(format!(
                "{} already exists; refusing to overwrite it",
                path.display()
            )));
        }
        fs::create_dir_all(project_dir).map_err(|error| {
            Error::Sync(format!(
                "failed to create Project directory {}: {error}",
                project_dir.display()
            ))
        })?;
        let yaml = serde_yaml::to_string(self)
            .map_err(|error| Error::Sync(format!("failed to serialize manifest: {error}")))?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".xpressclaw-manifest-")
            .tempfile_in(project_dir)
            .map_err(|error| Error::Sync(format!("failed to create manifest: {error}")))?;
        temporary
            .write_all(yaml.as_bytes())
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|error| Error::Sync(format!("failed to write manifest: {error}")))?;
        temporary.persist_noclobber(&path).map_err(|error| {
            Error::Sync(format!(
                "failed to install manifest {}: {}",
                path.display(),
                error.error
            ))
        })?;
        Ok(path)
    }

    pub fn store_path(&self) -> PathBuf {
        PathBuf::from(&self.store.path)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != MANIFEST_VERSION {
            return Err(Error::Sync(format!(
                "unsupported manifest version {}; this release supports version {MANIFEST_VERSION}",
                self.version
            )));
        }
        validate_identifier("project_id", &self.project_id)?;
        validate_remote(&self.store.remote)?;
        validate_branch(&self.store.branch)?;
        validate_store_path(&self.store.path)?;
        reject_credential_marker(&self.project_id)?;
        reject_credential_marker(&self.store.branch)?;
        reject_credential_marker(&self.store.path)?;
        Ok(())
    }
}

fn reject_credential_marker(value: &str) -> Result<()> {
    let lower = value.to_ascii_lowercase();
    if [
        "bearer ",
        "ghp_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "sk-proj-",
        "sk-ant-",
        "access_token=",
        "api_key=",
        "password=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Err(Error::Sync(
            "the Project manifest appears to contain a credential".into(),
        ));
    }
    Ok(())
}

fn reject_manifest_text(value: &str) -> Result<()> {
    let lower = value.to_ascii_lowercase();
    if [
        "-----begin private key-----",
        "-----begin rsa private key-----",
        "-----begin openssh private key-----",
        "bearer ",
        "ghp_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "sk-proj-",
        "sk-ant-",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Err(Error::Sync(
            "the Project manifest appears to contain a credential".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_identifier(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 200 {
        return Err(Error::Sync(format!(
            "{field} must contain between 1 and 200 bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(Error::Sync(format!("{field} contains control characters")));
    }
    Ok(())
}

fn validate_remote(remote: &str) -> Result<()> {
    if remote.is_empty() || remote != remote.trim() || remote.len() > 2_048 {
        return Err(Error::Sync(
            "store.remote must contain a non-empty Git remote of at most 2048 bytes".into(),
        ));
    }
    if remote.starts_with('-') || remote.chars().any(char::is_control) {
        return Err(Error::Sync("store.remote is not a safe Git remote".into()));
    }
    let remote_helper = !remote.contains("://")
        && remote.split_once("::").is_some_and(|(helper, _)| {
            !helper.is_empty()
                && helper
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "+-.".contains(character))
        });
    if remote_helper {
        return Err(Error::Sync(
            "store.remote must not use a Git remote-helper transport".into(),
        ));
    }
    let lower = remote.to_ascii_lowercase();
    for marker in [
        "ghp_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "access_token=",
        "api_key=",
        "password=",
    ] {
        if lower.contains(marker) {
            return Err(Error::Sync(
                "store.remote appears to contain a credential; use a credential helper or SSH agent instead"
                    .into(),
            ));
        }
    }
    if remote.contains("://") {
        let url = Url::parse(remote)
            .map_err(|_| Error::Sync("store.remote is not a valid Git URL".into()))?;
        if !matches!(url.scheme(), "file" | "git" | "http" | "https" | "ssh") {
            return Err(Error::Sync(format!(
                "store.remote uses unsupported Git URL scheme '{}'",
                url.scheme()
            )));
        }
        if url.password().is_some()
            || matches!(url.scheme(), "http" | "https") && !url.username().is_empty()
            || url.fragment().is_some()
        {
            return Err(Error::Sync(
                "store.remote must not embed a username, password, token, or fragment; use local Git credentials"
                    .into(),
            ));
        }
        if url.query().is_some() {
            return Err(Error::Sync(
                "store.remote must not contain query parameters; use local Git credentials".into(),
            ));
        }
    } else if !remote.contains(':') && !Path::new(remote).is_absolute() {
        return Err(Error::Sync(
            "store.remote must be a Git URL, SCP-style remote, or absolute local path".into(),
        ));
    }
    Ok(())
}

fn validate_branch(branch: &str) -> Result<()> {
    let invalid = branch.is_empty()
        || branch.len() > 240
        || branch.starts_with('-')
        || branch.starts_with('.')
        || branch.ends_with('.')
        || branch.ends_with('/')
        || branch.starts_with('/')
        || branch.contains("//")
        || branch.contains("/.")
        || branch.contains(".lock/")
        || branch.ends_with(".lock")
        || branch == "@"
        || branch.contains("..")
        || branch.contains("@{")
        || branch
            .chars()
            .any(|character| character.is_control() || " ~^:?*[\\".contains(character));
    if invalid {
        return Err(Error::Sync(format!(
            "store.branch '{branch}' is not a valid Git branch name"
        )));
    }
    Ok(())
}

fn validate_store_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 512
        || path.ends_with('/')
        || path.contains("//")
        || path
            .chars()
            .any(|character| character.is_control() || "\\:*?\"<>|".contains(character))
    {
        return Err(Error::Sync(
            "store.path must be a non-empty portable relative path".into(),
        ));
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(Error::Sync("store.path must be relative".into()));
    }
    let mut count = 0;
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                count += 1;
                if value
                    .to_str()
                    .is_none_or(|value| value.eq_ignore_ascii_case(".git") || value.is_empty())
                {
                    return Err(Error::Sync(
                        "store.path cannot select Git metadata or an empty component".into(),
                    ));
                }
            }
            _ => {
                return Err(Error::Sync(
                    "store.path cannot contain '.', '..', a root, or a platform prefix".into(),
                ));
            }
        }
    }
    if count == 0 {
        return Err(Error::Sync(
            "store.path must select a directory inside the synchronization repository".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_manifest_accepts_portable_git_pointer() {
        let yaml = r#"
version: 1
project_id: project-one
store:
  remote: git@github.com:example/xpressclaw-data.git
  branch: collaboration
  path: projects/project-one
share:
  project_memory: false
"#;
        let manifest: ProjectSyncManifest = serde_yaml::from_str(yaml).unwrap();
        manifest.validate().unwrap();
        assert!(!manifest.share.project_memory);
        ProjectSyncManifest::new(
            "project-one",
            "ssh://git@[::1]/data.git",
            "main",
            "projects/project-one",
        )
        .unwrap();
    }

    #[test]
    fn manifest_rejects_unknown_and_credential_fields() {
        let unknown = r#"
version: 1
project_id: project-one
token: secret
store:
  remote: https://github.com/example/data.git
  branch: main
  path: projects/project-one
"#;
        assert!(serde_yaml::from_str::<ProjectSyncManifest>(unknown).is_err());

        let credential = ProjectSyncManifest::new(
            "project-one",
            "https://ghp_supersecret@github.com/example/data.git",
            "main",
            "projects/project-one",
        );
        assert!(credential.is_err());
    }

    #[test]
    fn manifest_rejects_unsafe_branch_and_path() {
        assert!(ProjectSyncManifest::new(
            "project-one",
            "git@example.test:data.git",
            "../main",
            "projects/project-one",
        )
        .is_err());
        assert!(ProjectSyncManifest::new(
            "project-one",
            "git@example.test:data.git",
            "main",
            "../outside",
        )
        .is_err());
        assert!(ProjectSyncManifest::new(
            "project-one",
            "ext::sh -c dangerous",
            "main",
            "projects/project-one",
        )
        .is_err());
        assert!(ProjectSyncManifest::new(
            "project-one",
            "relative/repository.git",
            "main",
            "projects/project-one",
        )
        .is_err());
        assert!(ProjectSyncManifest::new(
            "project-one",
            "https://example.test/data.git?sig=credential",
            "main",
            "projects/project-one",
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn manifest_loader_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("manifest-target.yml");
        fs::write(
            &target,
            "version: 1\nproject_id: one\nstore:\n  remote: local\n  path: projects/one\n",
        )
        .unwrap();
        symlink(&target, directory.path().join(MANIFEST_FILE)).unwrap();
        assert!(ProjectSyncManifest::load(directory.path()).is_err());
    }
}
