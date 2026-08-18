use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::hmac;
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
        let mut temporary = tempfile::Builder::new()
            .prefix(".collaboration-secrets-")
            .tempfile_in(parent)
            .map_err(|error| {
                Error::Config(format!("failed to create collaboration secrets: {error}"))
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    Error::Config(format!("failed to protect collaboration secrets: {error}"))
                })?;
        }
        temporary
            .write_all(&serde_json::to_vec(self)?)
            .map_err(|error| {
                Error::Config(format!("failed to write collaboration secrets: {error}"))
            })?;
        temporary.as_file().sync_all().map_err(|error| {
            Error::Config(format!("failed to sync collaboration secrets: {error}"))
        })?;
        temporary.persist(&path).map_err(|error| {
            Error::Config(format!(
                "failed to commit collaboration secrets: {}",
                error.error
            ))
        })?;
        Ok(())
    }

    /// Derives a capability that is valid only for one Agent identity.
    ///
    /// The stored random value is a signing key, not a bearer capability. This
    /// lets the control plane revoke one Agent without rotating the credentials
    /// of every other assigned Agent or persisting a second token registry.
    pub fn capability_token_for_agent(&self, agent: &str) -> String {
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.agent_capability_token.as_bytes());
        URL_SAFE_NO_PAD.encode(hmac::sign(&key, &agent_capability_message(agent)).as_ref())
    }

    pub fn capability_token_matches_agent(&self, agent: &str, supplied: &str) -> bool {
        let Ok(supplied) = URL_SAFE_NO_PAD.decode(supplied) else {
            return false;
        };
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.agent_capability_token.as_bytes());
        hmac::verify(&key, &agent_capability_message(agent), &supplied).is_ok()
    }
}

fn agent_capability_message(agent: &str) -> Vec<u8> {
    const DOMAIN: &[u8] = b"xpressclaw/local-collaboration/agent/v1\0";
    let mut message = Vec::with_capacity(DOMAIN.len() + agent.len());
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(agent.as_bytes());
    message
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn random_password() -> String {
    Uuid::new_v4().simple().to_string()
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
        let mut updated = secrets.clone();
        updated.gitbucket_service_token = Some("generated-token".to_string());
        updated.jenkins_initialized = true;
        updated.save(dir.path()).unwrap();
        let reloaded = CollaborationSecrets::load(dir.path()).unwrap().unwrap();
        assert_eq!(
            reloaded.gitbucket_service_token.as_deref(),
            Some("generated-token")
        );
        assert!(reloaded.jenkins_initialized);
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

    #[test]
    fn agent_capabilities_are_stable_and_identity_bound() {
        let secrets = CollaborationSecrets::generate();
        let atlas = secrets.capability_token_for_agent("atlas");
        let zephyr = secrets.capability_token_for_agent("zephyr");

        assert_eq!(atlas, secrets.capability_token_for_agent("atlas"));
        assert_ne!(atlas, zephyr);
        assert!(secrets.capability_token_matches_agent("atlas", &atlas));
        assert!(!secrets.capability_token_matches_agent("zephyr", &atlas));
        assert!(!secrets.capability_token_matches_agent("atlas", &secrets.agent_capability_token));
    }
}
