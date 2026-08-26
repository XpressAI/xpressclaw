//! Browser authentication for one XpressClaw control-plane instance.
//!
//! The public listener uses cookie sessions. Runner callbacks retain their
//! separate process capability and never pass through this module.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tempfile::NamedTempFile;
use zeroize::Zeroizing;

pub const SESSION_COOKIE: &str = "xpressclaw_session";
pub const CSRF_HEADER: &str = "x-xpressclaw-csrf";
pub const STARTUP_TOKEN_PREFIX: &str = "XPRESSCLAW_STARTUP_TOKEN=";

const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
const DESKTOP_TICKET_LIFETIME: Duration = Duration::from_secs(30);
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const MAX_LOGIN_FAILURES: usize = 5;
const MAX_TRACKED_LOGIN_PEERS: usize = 4096;
const MAX_BROWSER_SESSIONS: usize = 256;
const MAX_DESKTOP_TICKETS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Disabled,
    Password,
    StartupToken,
    RestartRequired,
}

impl CredentialKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Password => "password",
            Self::StartupToken => "startup_token",
            Self::RestartRequired => "restart_required",
        }
    }
}

enum Credential {
    Disabled,
    Password(String),
    StartupToken(Zeroizing<String>),
    RestartRequired,
}

struct BrowserSession {
    csrf: Zeroizing<String>,
    expires_at: Instant,
}

struct DesktopTicket {
    expires_at: Instant,
}

#[derive(Default)]
struct LoginFailures {
    failures: VecDeque<Instant>,
}

/// Process-local authentication state. Sessions and startup tokens are
/// intentionally lost on restart.
pub struct InstanceAuth {
    instance_id: String,
    effective_enabled: bool,
    credential: Mutex<Credential>,
    sessions: Mutex<HashMap<[u8; 32], BrowserSession>>,
    desktop_tickets: Mutex<HashMap<[u8; 32], DesktopTicket>>,
    login_failures: Mutex<HashMap<IpAddr, LoginFailures>>,
    /// Serialize credential verification so a parallel burst cannot race
    /// through the failure threshold and fan out memory-hard Argon2 work.
    login_gate: tokio::sync::Mutex<()>,
    announcement: Mutex<Option<Zeroizing<String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginError {
    Invalid,
    Internal,
    RateLimited { retry_after_seconds: u64 },
    RestartRequired,
    Disabled,
}

impl InstanceAuth {
    pub fn disabled(instance_id: String) -> Self {
        Self {
            instance_id,
            effective_enabled: false,
            credential: Mutex::new(Credential::Disabled),
            sessions: Mutex::new(HashMap::new()),
            desktop_tickets: Mutex::new(HashMap::new()),
            login_failures: Mutex::new(HashMap::new()),
            login_gate: tokio::sync::Mutex::new(()),
            announcement: Mutex::new(None),
        }
    }

    /// Load the verifier for an effective server configuration. If auth is
    /// enabled without a password, a token is generated unless the detached
    /// launcher supplied one through its anonymous stdin pipe.
    pub fn load(
        data_dir: &Path,
        instance_id: String,
        effective_enabled: bool,
        supplied_startup_token: Option<Zeroizing<String>>,
    ) -> anyhow::Result<Self> {
        if !effective_enabled {
            return Ok(Self::disabled(instance_id));
        }

        let password_hash = load_password_hash(data_dir)?;
        let (credential, announcement) = if let Some(password_hash) = password_hash {
            // Reject a corrupt verifier during startup rather than silently
            // falling back to a weaker credential mode.
            PasswordHash::new(&password_hash)
                .map_err(|error| anyhow::anyhow!("invalid instance password verifier: {error}"))?;
            (Credential::Password(password_hash), None)
        } else if let Some(token) = supplied_startup_token {
            (Credential::StartupToken(token), None)
        } else {
            let token = Zeroizing::new(generate_secret()?);
            (Credential::StartupToken(token.clone()), Some(token))
        };

        Ok(Self {
            instance_id,
            effective_enabled,
            credential: Mutex::new(credential),
            sessions: Mutex::new(HashMap::new()),
            desktop_tickets: Mutex::new(HashMap::new()),
            login_failures: Mutex::new(HashMap::new()),
            login_gate: tokio::sync::Mutex::new(()),
            announcement: Mutex::new(announcement),
        })
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn enabled(&self) -> bool {
        self.effective_enabled
    }

    pub fn credential_kind(&self) -> CredentialKind {
        match &*self.credential.lock().unwrap() {
            Credential::Disabled => CredentialKind::Disabled,
            Credential::Password(_) => CredentialKind::Password,
            Credential::StartupToken(_) => CredentialKind::StartupToken,
            Credential::RestartRequired => CredentialKind::RestartRequired,
        }
    }

    /// Return the generated startup token once. Callers must write it only to
    /// an operator-owned foreground stream, never to tracing or a log file.
    pub fn take_startup_token_announcement(&self) -> Option<Zeroizing<String>> {
        self.announcement.lock().unwrap().take()
    }

    pub async fn login(
        &self,
        credential: Zeroizing<String>,
        peer: IpAddr,
    ) -> Result<(Zeroizing<String>, Zeroizing<String>), LoginError> {
        if !self.effective_enabled {
            return Err(LoginError::Disabled);
        }
        let _login_guard = self.login_gate.lock().await;
        self.check_throttle(peer)?;

        enum Verification {
            Password(String),
            Secret([u8; 32]),
        }
        let verification = {
            let current = self.credential.lock().unwrap();
            match &*current {
                Credential::Password(hash) => Verification::Password(hash.clone()),
                Credential::StartupToken(expected) => Verification::Secret(secret_key(expected)),
                Credential::RestartRequired => return Err(LoginError::RestartRequired),
                Credential::Disabled => return Err(LoginError::Disabled),
            }
        };
        let verification = match verification {
            Verification::Password(hash) => {
                let candidate = credential.clone();
                tokio::task::spawn_blocking(move || verify_password(&hash, &candidate))
                    .await
                    .unwrap_or(false)
            }
            Verification::Secret(expected) => bool::from(expected.ct_eq(&secret_key(&credential))),
        };

        if !verification {
            self.record_failure(peer);
            return Err(LoginError::Invalid);
        }
        self.login_failures.lock().unwrap().remove(&peer);
        self.new_session()
    }

    pub fn authenticate(&self, cookie_value: &str) -> Option<Zeroizing<String>> {
        if !self.effective_enabled {
            return None;
        }
        let key = secret_key(cookie_value);
        let mut sessions = self.sessions.lock().unwrap();
        let now = Instant::now();
        sessions.retain(|_, session| session.expires_at > now);
        sessions.get(&key).map(|session| session.csrf.clone())
    }

    pub fn verify_csrf(&self, cookie_value: &str, supplied: &str) -> bool {
        self.authenticate(cookie_value)
            .is_some_and(|expected| bool::from(secret_key(&expected).ct_eq(&secret_key(supplied))))
    }

    pub fn logout(&self, cookie_value: &str) {
        self.sessions
            .lock()
            .unwrap()
            .remove(&secret_key(cookie_value));
    }

    pub fn revoke_all(&self) {
        self.sessions.lock().unwrap().clear();
        self.desktop_tickets.lock().unwrap().clear();
    }

    /// Apply a password change to the running process and revoke every
    /// existing session. Removing the password from an auth-enabled process
    /// requires a restart so the new startup token can be delivered safely.
    pub fn replace_password_hash(&self, password_hash: Option<String>) {
        self.revoke_all();
        let next = if !self.effective_enabled {
            Credential::Disabled
        } else if let Some(hash) = password_hash {
            Credential::Password(hash)
        } else {
            Credential::RestartRequired
        };
        *self.credential.lock().unwrap() = next;
    }

    pub async fn create_desktop_ticket(
        &self,
        credential: Zeroizing<String>,
        peer: IpAddr,
    ) -> Result<Zeroizing<String>, LoginError> {
        let (session, _) = self.login(credential, peer).await?;
        // A desktop credential check must not also leave an unused browser
        // session behind.
        self.logout(&session);

        let ticket = Zeroizing::new(generate_secret().map_err(|_| LoginError::Internal)?);
        let mut tickets = self.desktop_tickets.lock().unwrap();
        let now = Instant::now();
        tickets.retain(|_, ticket| ticket.expires_at > now);
        if tickets.len() >= MAX_DESKTOP_TICKETS {
            if let Some(oldest) = tickets
                .iter()
                .min_by_key(|(_, ticket)| ticket.expires_at)
                .map(|(key, _)| *key)
            {
                tickets.remove(&oldest);
            }
        }
        tickets.insert(
            secret_key(&ticket),
            DesktopTicket {
                expires_at: now + DESKTOP_TICKET_LIFETIME,
            },
        );
        Ok(ticket)
    }

    pub fn exchange_desktop_ticket(
        &self,
        ticket: &str,
    ) -> Option<(Zeroizing<String>, Zeroizing<String>)> {
        let key = secret_key(ticket);
        let mut tickets = self.desktop_tickets.lock().unwrap();
        let now = Instant::now();
        tickets.retain(|_, ticket| ticket.expires_at > now);
        tickets.remove(&key)?;
        self.new_session().ok()
    }

    fn new_session(&self) -> Result<(Zeroizing<String>, Zeroizing<String>), LoginError> {
        let token = Zeroizing::new(generate_secret().map_err(|_| LoginError::Internal)?);
        let csrf = Zeroizing::new(generate_secret().map_err(|_| LoginError::Internal)?);
        let mut sessions = self.sessions.lock().unwrap();
        let now = Instant::now();
        sessions.retain(|_, session| session.expires_at > now);
        if sessions.len() >= MAX_BROWSER_SESSIONS {
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, session)| session.expires_at)
                .map(|(key, _)| *key)
            {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            secret_key(&token),
            BrowserSession {
                csrf: csrf.clone(),
                expires_at: now + SESSION_LIFETIME,
            },
        );
        Ok((token, csrf))
    }

    fn check_throttle(&self, peer: IpAddr) -> Result<(), LoginError> {
        let now = Instant::now();
        let mut failures = self.login_failures.lock().unwrap();
        failures.retain(|_, state| {
            state
                .failures
                .retain(|at| now.duration_since(*at) < LOGIN_WINDOW);
            !state.failures.is_empty()
        });
        let Some(state) = failures.get(&peer) else {
            return Ok(());
        };
        if state.failures.len() < MAX_LOGIN_FAILURES {
            return Ok(());
        }
        let retry_after = LOGIN_WINDOW
            .saturating_sub(now.duration_since(*state.failures.front().unwrap()))
            .as_secs()
            .max(1);
        Err(LoginError::RateLimited {
            retry_after_seconds: retry_after,
        })
    }

    fn record_failure(&self, peer: IpAddr) {
        let now = Instant::now();
        let mut failures = self.login_failures.lock().unwrap();
        failures.retain(|_, state| {
            state
                .failures
                .retain(|at| now.duration_since(*at) < LOGIN_WINDOW);
            !state.failures.is_empty()
        });
        if !failures.contains_key(&peer) && failures.len() >= MAX_TRACKED_LOGIN_PEERS {
            if let Some(oldest) = failures
                .iter()
                .min_by_key(|(_, state)| state.failures.back().copied())
                .map(|(peer, _)| *peer)
            {
                failures.remove(&oldest);
            }
        }
        let state = failures.entry(peer).or_default();
        state.failures.push_back(now);
    }
}

fn verify_password(hash: &str, candidate: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(candidate.as_bytes(), &parsed)
        .is_ok()
}

pub async fn hash_password(password: Zeroizing<String>) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|error| anyhow::anyhow!("failed to hash instance password: {error}"))
    })
    .await?
}

pub fn generate_startup_token() -> anyhow::Result<Zeroizing<String>> {
    Ok(Zeroizing::new(generate_secret()?))
}

fn generate_secret() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("operating-system random number generator failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn secret_key(value: &str) -> [u8; 32] {
    digest(&SHA256, value.as_bytes())
        .as_ref()
        .try_into()
        .expect("SHA-256 has a fixed length")
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceAuthSecret {
    #[serde(default = "secret_version")]
    version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password_hash: Option<String>,
}

fn secret_version() -> u8 {
    1
}

fn secret_path(data_dir: &Path) -> PathBuf {
    data_dir.join("instance-auth.json")
}

pub fn password_configured(data_dir: &Path) -> anyhow::Result<bool> {
    Ok(load_password_hash(data_dir)?.is_some())
}

pub fn load_password_hash(data_dir: &Path) -> anyhow::Result<Option<String>> {
    let path = secret_path(data_dir);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    restrict_existing_file(&path)?;
    let bytes = std::fs::read(&path)?;
    let secret: InstanceAuthSecret = serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error}", path.display()))?;
    if secret.version != secret_version() {
        anyhow::bail!(
            "unsupported instance auth secret version {}",
            secret.version
        );
    }
    Ok(secret.password_hash)
}

pub fn store_password_hash(data_dir: &Path, password_hash: Option<&str>) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = secret_path(data_dir);
    let secret = InstanceAuthSecret {
        version: secret_version(),
        password_hash: password_hash.map(ToOwned::to_owned),
    };
    let bytes = serde_json::to_vec_pretty(&secret)?;
    let mut temporary = NamedTempFile::new_in(data_dir)?;
    use std::io::Write;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    set_restricted_permissions(temporary.path())?;
    temporary.persist(&path).map_err(|error| {
        anyhow::anyhow!("failed to replace {}: {}", path.display(), error.error)
    })?;
    restrict_existing_file(&path)?;
    Ok(())
}

#[cfg(unix)]
fn set_restricted_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_restricted_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    xpressclaw_core::workers::native::set_windows_owner_only_acl(path, false)?;
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

fn restrict_existing_file(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!(
            "instance authentication secret path {} is not a regular file",
            path.display()
        );
    }
    set_restricted_permissions(path)
}

pub fn session_cookie(value: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_LIFETIME.as_secs()
    ) + secure
}

pub fn expired_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

pub fn cookie_value(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| (name == SESSION_COOKIE).then_some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn password_verifier_and_sessions_never_store_plaintext() {
        let root = tempfile::tempdir().unwrap();
        let password = Zeroizing::new("correct horse battery staple".to_string());
        let hash = hash_password(password.clone()).await.unwrap();
        assert!(!hash.contains(password.as_str()));
        store_password_hash(root.path(), Some(&hash)).unwrap();
        let stored = std::fs::read_to_string(secret_path(root.path())).unwrap();
        assert!(!stored.contains(password.as_str()));

        let auth = InstanceAuth::load(root.path(), "instance".into(), true, None).unwrap();
        let (session, csrf) = auth
            .login(password, "127.0.0.1".parse().unwrap())
            .await
            .unwrap();
        assert!(auth.authenticate(&session).is_some());
        assert!(auth.verify_csrf(&session, &csrf));
        assert!(!auth.verify_csrf(&session, "wrong"));
        let replacement = hash_password(Zeroizing::new("replacement password value".to_string()))
            .await
            .unwrap();
        auth.replace_password_hash(Some(replacement));
        assert!(auth.authenticate(&session).is_none());
        auth.logout(&session);
        assert!(auth.authenticate(&session).is_none());
    }

    #[tokio::test]
    async fn startup_tokens_rotate_and_password_removal_locks_until_restart() {
        let root = tempfile::tempdir().unwrap();
        let first = InstanceAuth::load(root.path(), "instance".into(), true, None).unwrap();
        let first_token = first.take_startup_token_announcement().unwrap();
        let first_token_value = first_token.to_string();
        let (first_session, _) = first
            .login(first_token, "127.0.0.1".parse().unwrap())
            .await
            .unwrap();

        let second = InstanceAuth::load(root.path(), "instance".into(), true, None).unwrap();
        let second_token = second.take_startup_token_announcement().unwrap();
        assert_ne!(*second_token, first_token_value);
        assert!(second.authenticate(&first_session).is_none());

        second.replace_password_hash(None);
        assert_eq!(second.credential_kind(), CredentialKind::RestartRequired);
        assert_eq!(
            second
                .login(second_token, "127.0.0.1".parse().unwrap())
                .await,
            Err(LoginError::RestartRequired)
        );
    }

    #[tokio::test]
    async fn brute_force_attempts_are_bounded_per_peer() {
        let root = tempfile::tempdir().unwrap();
        let auth = InstanceAuth::load(
            root.path(),
            "instance".into(),
            true,
            Some(Zeroizing::new("expected".into())),
        )
        .unwrap();
        let peer = "192.0.2.8".parse().unwrap();
        for _ in 0..MAX_LOGIN_FAILURES {
            assert_eq!(
                auth.login(Zeroizing::new("wrong".into()), peer).await,
                Err(LoginError::Invalid)
            );
        }
        assert!(matches!(
            auth.login(Zeroizing::new("expected".into()), peer).await,
            Err(LoginError::RateLimited { .. })
        ));
    }

    #[tokio::test]
    async fn brute_force_tracking_has_a_hard_peer_bound() {
        let root = tempfile::tempdir().unwrap();
        let auth = InstanceAuth::load(
            root.path(),
            "instance".into(),
            true,
            Some(Zeroizing::new("expected".into())),
        )
        .unwrap();
        for value in 0..=MAX_TRACKED_LOGIN_PEERS {
            let peer = IpAddr::V6(std::net::Ipv6Addr::from(value as u128 + 1));
            assert_eq!(
                auth.login(Zeroizing::new("wrong".into()), peer).await,
                Err(LoginError::Invalid)
            );
        }
        assert!(auth.login_failures.lock().unwrap().len() <= MAX_TRACKED_LOGIN_PEERS);
    }

    #[tokio::test]
    async fn concurrent_attempts_cannot_race_past_the_failure_threshold() {
        let root = tempfile::tempdir().unwrap();
        let auth = std::sync::Arc::new(
            InstanceAuth::load(
                root.path(),
                "instance".into(),
                true,
                Some(Zeroizing::new("expected".into())),
            )
            .unwrap(),
        );
        let peer = "192.0.2.18".parse().unwrap();
        let mut attempts = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let auth = auth.clone();
            attempts.spawn(async move { auth.login(Zeroizing::new("wrong".into()), peer).await });
        }

        let mut invalid = 0;
        let mut limited = 0;
        while let Some(result) = attempts.join_next().await {
            match result.unwrap() {
                Err(LoginError::Invalid) => invalid += 1,
                Err(LoginError::RateLimited { .. }) => limited += 1,
                other => panic!("unexpected concurrent login result: {other:?}"),
            }
        }
        assert_eq!(invalid, MAX_LOGIN_FAILURES);
        assert_eq!(limited, 16 - MAX_LOGIN_FAILURES);
    }

    #[tokio::test]
    async fn expired_sessions_and_replayed_desktop_tickets_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let auth = InstanceAuth::load(
            root.path(),
            "instance".into(),
            true,
            Some(Zeroizing::new("expected".into())),
        )
        .unwrap();
        let (session, _) = auth
            .login(
                Zeroizing::new("expected".into()),
                "127.0.0.1".parse().unwrap(),
            )
            .await
            .unwrap();
        auth.sessions
            .lock()
            .unwrap()
            .get_mut(&secret_key(&session))
            .unwrap()
            .expires_at = Instant::now() - Duration::from_secs(1);
        assert!(auth.authenticate(&session).is_none());

        let ticket = auth
            .create_desktop_ticket(
                Zeroizing::new("expected".into()),
                "127.0.0.1".parse().unwrap(),
            )
            .await
            .unwrap();
        assert!(auth.exchange_desktop_ticket(&ticket).is_some());
        assert!(auth.exchange_desktop_ticket(&ticket).is_none());
    }

    #[test]
    fn secret_files_are_restricted_on_unix() {
        let root = tempfile::tempdir().unwrap();
        store_password_hash(root.path(), Some("hash")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(secret_path(root.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn secret_loader_rejects_symlink_paths() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), secret_path(root.path())).unwrap();
        assert!(load_password_hash(root.path())
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));
    }

    #[test]
    fn secure_cookie_attribute_is_explicit_and_transport_aware() {
        let http = session_cookie("opaque", false);
        let https = session_cookie("opaque", true);
        assert!(http.contains("HttpOnly"));
        assert!(http.contains("SameSite=Strict"));
        assert!(!http.contains("; Secure"));
        assert!(https.contains("; Secure"));
    }
}
