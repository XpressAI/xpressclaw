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
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tempfile::NamedTempFile;
use xpressclaw_core::desktop_auth::{
    credential_aad as desktop_credential_aad,
    credential_proof_message as desktop_credential_proof_message,
    derive_credential_keys as derive_desktop_credential_keys, identity_proof_message,
    DesktopCredentialPurpose, BROWSER_SESSION_COOKIE, BROWSER_SESSION_LIFETIME_SECONDS,
    CREDENTIAL_CHANNEL_NONCE, CREDENTIAL_REQUEST_DIRECTION, CREDENTIAL_RESPONSE_DIRECTION,
};
use zeroize::Zeroizing;

pub const SESSION_COOKIE: &str = BROWSER_SESSION_COOKIE;
pub const CSRF_HEADER: &str = "x-xpressclaw-csrf";
pub const STARTUP_TOKEN_PREFIX: &str = "XPRESSCLAW_STARTUP_TOKEN=";
pub const INSTANCE_IDENTITY_PREFIX: &str = "XPRESSCLAW_INSTANCE_IDENTITY=";

const SESSION_LIFETIME: Duration = Duration::from_secs(BROWSER_SESSION_LIFETIME_SECONDS);
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const MAX_LOGIN_FAILURES: usize = 5;
const MAX_TRACKED_LOGIN_PEERS: usize = 4096;
const MAX_BROWSER_SESSIONS: usize = 256;
const MAX_DESKTOP_CREDENTIAL_CHANNELS: usize = 128;
const DESKTOP_CREDENTIAL_CHANNEL_LIFETIME: Duration = Duration::from_secs(30);

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

struct DesktopCredentialChannel {
    request_key: Zeroizing<[u8; 32]>,
    response_key: Zeroizing<[u8; 32]>,
    expires_at: Instant,
}

pub struct DesktopCredentialProof {
    pub exchange_id: String,
    pub server_public_key: String,
    pub signature: String,
}

pub struct OpenedDesktopCredential {
    pub credential: Zeroizing<String>,
    pub purpose: DesktopCredentialPurpose,
    exchange_id: [u8; 32],
    response_key: Zeroizing<[u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopCredentialError {
    Invalid,
    Internal,
}

#[derive(Default)]
struct LoginFailures {
    failures: VecDeque<Instant>,
}

/// Process-local authentication state. Sessions and startup tokens are
/// intentionally lost on restart.
pub struct InstanceAuth {
    instance_id: String,
    identity_key: Ed25519KeyPair,
    effective_enabled: bool,
    credential: Mutex<Credential>,
    sessions: Mutex<HashMap<[u8; 32], BrowserSession>>,
    desktop_credential_channels: Mutex<HashMap<[u8; 32], DesktopCredentialChannel>>,
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
        let identity_key = generate_identity_key()
            .expect("operating-system random number generator failed for test identity");
        Self::disabled_with_identity(instance_id, identity_key)
    }

    fn disabled_with_identity(instance_id: String, identity_key: Ed25519KeyPair) -> Self {
        Self {
            instance_id,
            identity_key,
            effective_enabled: false,
            credential: Mutex::new(Credential::Disabled),
            sessions: Mutex::new(HashMap::new()),
            desktop_credential_channels: Mutex::new(HashMap::new()),
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
        let identity_key = load_or_create_identity_key(data_dir)?;
        if !effective_enabled {
            return Ok(Self::disabled_with_identity(instance_id, identity_key));
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
            identity_key,
            effective_enabled,
            credential: Mutex::new(credential),
            sessions: Mutex::new(HashMap::new()),
            desktop_credential_channels: Mutex::new(HashMap::new()),
            login_failures: Mutex::new(HashMap::new()),
            login_gate: tokio::sync::Mutex::new(()),
            announcement: Mutex::new(announcement),
        })
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Stable public identity pinned by Desktop profiles. The matching private
    /// key lives only in the restricted instance secret directory.
    pub fn identity_public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.identity_key.public_key().as_ref())
    }

    /// Sign a caller nonce with domain separation and the installation ID so
    /// a recorded public bootstrap response cannot be replayed by a process
    /// that later takes over the same address.
    pub fn sign_identity_challenge(&self, challenge: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(
            self.identity_key
                .sign(&identity_proof_message(&self.instance_id, challenge))
                .as_ref(),
        )
    }

    /// Establish a one-use encrypted channel for a Desktop credential. The
    /// instance identity signs both ephemeral X25519 keys and the caller's
    /// fresh challenge, so a relay can forward the exchange but cannot read
    /// the password or startup token that follows.
    pub fn begin_desktop_credential_exchange(
        &self,
        challenge: &[u8],
        client_public_key: &[u8],
    ) -> Result<DesktopCredentialProof, DesktopCredentialError> {
        if challenge.len() != 32 || client_public_key.len() != 32 {
            return Err(DesktopCredentialError::Invalid);
        }
        let rng = SystemRandom::new();
        let server_private = EphemeralPrivateKey::generate(&X25519, &rng)
            .map_err(|_| DesktopCredentialError::Internal)?;
        let server_public = server_private
            .compute_public_key()
            .map_err(|_| DesktopCredentialError::Internal)?;
        let server_public_key: [u8; 32] = server_public
            .as_ref()
            .try_into()
            .map_err(|_| DesktopCredentialError::Internal)?;
        let client_public_key: [u8; 32] = client_public_key
            .try_into()
            .map_err(|_| DesktopCredentialError::Invalid)?;
        let mut exchange_id = [0u8; 32];
        rng.fill(&mut exchange_id)
            .map_err(|_| DesktopCredentialError::Internal)?;
        let peer = UnparsedPublicKey::new(&X25519, client_public_key);
        let keys = agree_ephemeral(server_private, &peer, |shared| {
            derive_desktop_credential_keys(
                shared,
                &self.instance_id,
                challenge,
                &exchange_id,
                &client_public_key,
                &server_public_key,
            )
        })
        .map_err(|_| DesktopCredentialError::Invalid)?
        .map_err(|_| DesktopCredentialError::Internal)?;

        let proof_message = desktop_credential_proof_message(
            &self.instance_id,
            challenge,
            &exchange_id,
            &client_public_key,
            &server_public_key,
        );
        let signature = URL_SAFE_NO_PAD.encode(self.identity_key.sign(&proof_message).as_ref());
        let now = Instant::now();
        let mut channels = self.desktop_credential_channels.lock().unwrap();
        channels.retain(|_, channel| channel.expires_at > now);
        if channels.len() >= MAX_DESKTOP_CREDENTIAL_CHANNELS {
            if let Some(oldest) = channels
                .iter()
                .min_by_key(|(_, channel)| channel.expires_at)
                .map(|(id, _)| *id)
            {
                channels.remove(&oldest);
            }
        }
        channels.insert(
            exchange_id,
            DesktopCredentialChannel {
                request_key: keys.request,
                response_key: keys.response,
                expires_at: now + DESKTOP_CREDENTIAL_CHANNEL_LIFETIME,
            },
        );
        Ok(DesktopCredentialProof {
            exchange_id: URL_SAFE_NO_PAD.encode(exchange_id),
            server_public_key: URL_SAFE_NO_PAD.encode(server_public_key),
            signature,
        })
    }

    /// Consume a credential exchange before attempting decryption so an
    /// invalid or replayed ciphertext cannot be tried against the same key.
    pub fn open_desktop_credential(
        &self,
        exchange_id: &str,
        ciphertext: &str,
        purpose: DesktopCredentialPurpose,
    ) -> Result<OpenedDesktopCredential, DesktopCredentialError> {
        let exchange_id: [u8; 32] = URL_SAFE_NO_PAD
            .decode(exchange_id)
            .map_err(|_| DesktopCredentialError::Invalid)?
            .try_into()
            .map_err(|_| DesktopCredentialError::Invalid)?;
        let mut channels = self.desktop_credential_channels.lock().unwrap();
        let now = Instant::now();
        channels.retain(|_, channel| channel.expires_at > now);
        let channel = channels
            .remove(&exchange_id)
            .ok_or(DesktopCredentialError::Invalid)?;
        drop(channels);

        let mut encrypted = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(ciphertext)
                .map_err(|_| DesktopCredentialError::Invalid)?,
        );
        if encrypted.len() > 4096 + CHACHA20_POLY1305.tag_len() {
            return Err(DesktopCredentialError::Invalid);
        }
        let key = LessSafeKey::new(
            UnboundKey::new(&CHACHA20_POLY1305, channel.request_key.as_ref())
                .map_err(|_| DesktopCredentialError::Internal)?,
        );
        let aad = desktop_credential_aad(
            &self.instance_id,
            &exchange_id,
            CREDENTIAL_REQUEST_DIRECTION,
            purpose,
        );
        let plaintext = key
            .open_in_place(
                Nonce::assume_unique_for_key(CREDENTIAL_CHANNEL_NONCE),
                Aad::from(aad),
                encrypted.as_mut(),
            )
            .map_err(|_| DesktopCredentialError::Invalid)?;
        if plaintext.is_empty() || plaintext.len() > 4096 {
            return Err(DesktopCredentialError::Invalid);
        }
        let credential = std::str::from_utf8(plaintext)
            .map_err(|_| DesktopCredentialError::Invalid)?
            .to_owned();
        Ok(OpenedDesktopCredential {
            credential: Zeroizing::new(credential),
            purpose,
            exchange_id,
            response_key: channel.response_key,
        })
    }

    pub fn seal_desktop_credential_response(
        &self,
        channel: &OpenedDesktopCredential,
        plaintext: &mut Vec<u8>,
    ) -> Result<String, DesktopCredentialError> {
        let key = LessSafeKey::new(
            UnboundKey::new(&CHACHA20_POLY1305, channel.response_key.as_ref())
                .map_err(|_| DesktopCredentialError::Internal)?,
        );
        let aad = desktop_credential_aad(
            &self.instance_id,
            &channel.exchange_id,
            CREDENTIAL_RESPONSE_DIRECTION,
            channel.purpose,
        );
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(CREDENTIAL_CHANNEL_NONCE),
            Aad::from(aad),
            plaintext,
        )
        .map_err(|_| DesktopCredentialError::Internal)?;
        Ok(URL_SAFE_NO_PAD.encode(plaintext))
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
        self.desktop_credential_channels.lock().unwrap().clear();
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

    pub async fn validate_desktop_credential(
        &self,
        credential: Zeroizing<String>,
        peer: IpAddr,
    ) -> Result<(), LoginError> {
        let (session, _) = self.login(credential, peer).await?;
        // Profile setup and health validation must not leave an unused browser
        // session behind. Only the explicit browser-session purpose retains it.
        self.logout(&session);
        Ok(())
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceIdentitySecret {
    version: u8,
    private_key: String,
}

fn identity_secret_path(data_dir: &Path) -> PathBuf {
    data_dir.join("instance-identity.json")
}

fn generate_identity_key() -> anyhow::Result<Ed25519KeyPair> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| anyhow::anyhow!("operating-system random number generator failed"))?;
    Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| anyhow::anyhow!("generated instance identity key was invalid"))
}

fn decode_identity_key(path: &Path, bytes: &[u8]) -> anyhow::Result<Ed25519KeyPair> {
    let secret: InstanceIdentitySecret = serde_json::from_slice(bytes)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error}", path.display()))?;
    if secret.version != 1 {
        anyhow::bail!(
            "unsupported instance identity secret version {}",
            secret.version
        );
    }
    let private_key = URL_SAFE_NO_PAD
        .decode(secret.private_key)
        .map_err(|_| anyhow::anyhow!("instance identity secret contains invalid base64"))?;
    Ed25519KeyPair::from_pkcs8(&private_key)
        .map_err(|_| anyhow::anyhow!("instance identity secret contains an invalid key"))
}

fn load_identity_key(path: &Path) -> anyhow::Result<Ed25519KeyPair> {
    restrict_existing_file(path)?;
    decode_identity_key(path, &std::fs::read(path)?)
}

fn load_or_create_identity_key(data_dir: &Path) -> anyhow::Result<Ed25519KeyPair> {
    std::fs::create_dir_all(data_dir)?;
    let path = identity_secret_path(data_dir);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => return load_identity_key(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| anyhow::anyhow!("operating-system random number generator failed"))?;
    let secret = InstanceIdentitySecret {
        version: 1,
        private_key: URL_SAFE_NO_PAD.encode(pkcs8.as_ref()),
    };
    let bytes = serde_json::to_vec_pretty(&secret)?;
    let mut temporary = NamedTempFile::new_in(data_dir)?;
    use std::io::Write;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    set_restricted_permissions(temporary.path())?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => load_identity_key(&path),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            load_identity_key(&path)
        }
        Err(error) => Err(anyhow::anyhow!(
            "failed to create {}: {}",
            path.display(),
            error.error
        )),
    }
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
    use ring::signature::{UnparsedPublicKey, ED25519};

    #[test]
    fn identity_key_persists_and_proves_fresh_challenges() {
        let root = tempfile::tempdir().unwrap();
        let first = InstanceAuth::load(root.path(), "instance".into(), false, None).unwrap();
        let public_key = URL_SAFE_NO_PAD.decode(first.identity_public_key()).unwrap();
        let first_challenge = [7u8; 32];
        let first_signature = URL_SAFE_NO_PAD
            .decode(first.sign_identity_challenge(&first_challenge))
            .unwrap();
        UnparsedPublicKey::new(&ED25519, &public_key)
            .verify(
                &identity_proof_message("instance", &first_challenge),
                &first_signature,
            )
            .unwrap();

        let second = InstanceAuth::load(root.path(), "instance".into(), false, None).unwrap();
        assert_eq!(second.identity_public_key(), first.identity_public_key());
        let replayed_for_another_challenge = URL_SAFE_NO_PAD
            .decode(first.sign_identity_challenge(&first_challenge))
            .unwrap();
        assert!(UnparsedPublicKey::new(&ED25519, &public_key)
            .verify(
                &identity_proof_message("instance", &[8u8; 32]),
                &replayed_for_another_challenge,
            )
            .is_err());
    }

    #[tokio::test]
    async fn desktop_credentials_use_a_signed_one_time_encrypted_channel() {
        let root = tempfile::tempdir().unwrap();
        let auth = InstanceAuth::load(
            root.path(),
            "instance".into(),
            true,
            Some(Zeroizing::new("saved-password".into())),
        )
        .unwrap();
        let rng = SystemRandom::new();
        let challenge = [7u8; 32];
        let client_private = EphemeralPrivateKey::generate(&X25519, &rng).unwrap();
        let client_public_key: [u8; 32] = client_private
            .compute_public_key()
            .unwrap()
            .as_ref()
            .try_into()
            .unwrap();
        let proof = auth
            .begin_desktop_credential_exchange(&challenge, &client_public_key)
            .unwrap();
        let exchange_id: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&proof.exchange_id)
            .unwrap()
            .try_into()
            .unwrap();
        let server_public_key: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&proof.server_public_key)
            .unwrap()
            .try_into()
            .unwrap();
        let identity_public_key = URL_SAFE_NO_PAD.decode(auth.identity_public_key()).unwrap();
        let signature = URL_SAFE_NO_PAD.decode(proof.signature).unwrap();
        UnparsedPublicKey::new(&ED25519, identity_public_key)
            .verify(
                &desktop_credential_proof_message(
                    "instance",
                    &challenge,
                    &exchange_id,
                    &client_public_key,
                    &server_public_key,
                ),
                &signature,
            )
            .unwrap();
        let peer = ring::agreement::UnparsedPublicKey::new(&X25519, server_public_key);
        let keys = agree_ephemeral(client_private, &peer, |shared| {
            derive_desktop_credential_keys(
                shared,
                "instance",
                &challenge,
                &exchange_id,
                &client_public_key,
                &server_public_key,
            )
        })
        .unwrap()
        .unwrap();

        let mut ciphertext = b"saved-password".to_vec();
        let request_aad = desktop_credential_aad(
            "instance",
            &exchange_id,
            CREDENTIAL_REQUEST_DIRECTION,
            DesktopCredentialPurpose::BrowserSession,
        );
        LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, keys.request.as_ref()).unwrap())
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(CREDENTIAL_CHANNEL_NONCE),
                Aad::from(request_aad),
                &mut ciphertext,
            )
            .unwrap();
        assert!(!ciphertext
            .windows("saved-password".len())
            .any(|window| window == b"saved-password"));
        let opened = auth
            .open_desktop_credential(
                &proof.exchange_id,
                &URL_SAFE_NO_PAD.encode(&ciphertext),
                DesktopCredentialPurpose::BrowserSession,
            )
            .unwrap();
        assert_eq!(opened.credential.as_str(), "saved-password");
        assert_eq!(opened.purpose, DesktopCredentialPurpose::BrowserSession);
        assert!(matches!(
            auth.open_desktop_credential(
                &proof.exchange_id,
                &URL_SAFE_NO_PAD.encode(&ciphertext),
                DesktopCredentialPurpose::BrowserSession,
            ),
            Err(DesktopCredentialError::Invalid)
        ));

        let (session, _csrf) = auth
            .login(opened.credential.clone(), "127.0.0.1".parse().unwrap())
            .await
            .unwrap();
        let mut encrypted_response = serde_json::to_vec(&serde_json::json!({
            "kind": "browser_session",
            "session": session.as_str(),
            "instance_id": "instance",
        }))
        .unwrap();
        let encoded = auth
            .seal_desktop_credential_response(&opened, &mut encrypted_response)
            .unwrap();
        let mut encrypted_response = URL_SAFE_NO_PAD.decode(encoded).unwrap();
        let response_aad = desktop_credential_aad(
            "instance",
            &exchange_id,
            CREDENTIAL_RESPONSE_DIRECTION,
            DesktopCredentialPurpose::BrowserSession,
        );
        let plaintext =
            LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, keys.response.as_ref()).unwrap())
                .open_in_place(
                    Nonce::assume_unique_for_key(CREDENTIAL_CHANNEL_NONCE),
                    Aad::from(response_aad),
                    &mut encrypted_response,
                )
                .unwrap();
        let response: serde_json::Value = serde_json::from_slice(plaintext).unwrap();
        assert_eq!(response["instance_id"], "instance");
        assert_eq!(response["session"], session.as_str());
        assert!(auth.authenticate(&session).is_some());
    }

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
    async fn expired_sessions_and_validation_only_credentials_leave_no_session() {
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

        auth.validate_desktop_credential(
            Zeroizing::new("expected".into()),
            "127.0.0.1".parse().unwrap(),
        )
        .await
        .unwrap();
        assert!(auth.sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn secret_files_are_restricted_on_unix() {
        let root = tempfile::tempdir().unwrap();
        store_password_hash(root.path(), Some("hash")).unwrap();
        InstanceAuth::load(root.path(), "instance".into(), false, None).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [secret_path(root.path()), identity_secret_path(root.path())] {
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
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

        let identity_root = tempfile::tempdir().unwrap();
        symlink(outside.path(), identity_secret_path(identity_root.path())).unwrap();
        assert!(
            InstanceAuth::load(identity_root.path(), "instance".into(), false, None)
                .err()
                .unwrap()
                .to_string()
                .contains("not a regular file")
        );
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
