use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Error, Result};

const SECRET_FILE: &str = "collaboration-secrets.json";

#[derive(Clone, Serialize, Deserialize)]
pub struct CollaborationSecrets {
    pub gitbucket_root_password: String,
    pub gitbucket_service_password: String,
    pub gitbucket_service_token: Option<String>,
    pub jenkins_password: String,
    #[serde(default)]
    pub jenkins_initialized: bool,
    pub agent_capability_token: String,
}

impl fmt::Debug for CollaborationSecrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CollaborationSecrets")
            .field("gitbucket_root_password", &"[REDACTED]")
            .field("gitbucket_service_password", &"[REDACTED]")
            .field(
                "gitbucket_service_token",
                &self.gitbucket_service_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("jenkins_password", &"[REDACTED]")
            .field("jenkins_initialized", &self.jenkins_initialized)
            .field("agent_capability_token", &"[REDACTED]")
            .finish()
    }
}

impl CollaborationSecrets {
    pub fn generate() -> Self {
        Self {
            // GitBucket currently limits account passwords to 40 characters.
            gitbucket_root_password: random_password(),
            gitbucket_service_password: random_password(),
            gitbucket_service_token: None,
            jenkins_password: random_password(),
            jenkins_initialized: false,
            agent_capability_token: random_secret(),
        }
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("collaboration").join(SECRET_FILE)
    }

    pub fn load(data_dir: &Path) -> Result<Option<Self>> {
        let path = Self::path(data_dir);
        match fs::read(&path) {
            Ok(contents) => serde_json::from_slice(&contents)
                .map(Some)
                .map_err(|error| {
                    Error::Config(format!("invalid local collaboration secrets: {error}"))
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Config(format!(
                "failed to read local collaboration secrets: {error}"
            ))),
        }
    }

    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        if let Some(secrets) = Self::load(data_dir)? {
            return Ok(secrets);
        }
        let secrets = Self::generate();
        secrets.save(data_dir)?;
        Ok(secrets)
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let path = Self::path(data_dir);
        let parent = path.parent().expect("secret file has a parent");
        fs::create_dir_all(parent).map_err(|error| {
            Error::Config(format!(
                "failed to create collaboration secret directory: {error}"
            ))
        })?;
        let temporary = path.with_extension("json.tmp");
        write_private(&temporary, &serde_json::to_vec(self)?)?;
        fs::rename(&temporary, &path).map_err(|error| {
            Error::Config(format!("failed to commit collaboration secrets: {error}"))
        })?;
        Ok(())
    }
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn random_password() -> String {
    Uuid::new_v4().simple().to_string()
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            Error::Config(format!("failed to write collaboration secrets: {error}"))
        })?;
    file.write_all(contents)
        .map_err(|error| Error::Config(format!("failed to write collaboration secrets: {error}")))
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    fs::write(path, contents)
        .map_err(|error| Error::Config(format!("failed to write collaboration secrets: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_secret_values() {
        let secrets = CollaborationSecrets::generate();
        let debug = format!("{secrets:?}");
        assert!(!debug.contains(&secrets.gitbucket_root_password));
        assert!(!debug.contains(&secrets.jenkins_password));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn secret_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = CollaborationSecrets::load_or_create(dir.path()).unwrap();
        assert_eq!(
            CollaborationSecrets::load(dir.path())
                .unwrap()
                .unwrap()
                .agent_capability_token,
            secrets.agent_capability_token
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(CollaborationSecrets::path(dir.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
