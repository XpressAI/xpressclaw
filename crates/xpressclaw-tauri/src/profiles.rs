use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::StreamExt;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::agreement::{
    agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey as AgreementPublicKey, X25519,
};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use tauri::webview::cookie::{time::Duration as CookieDuration, SameSite};
use tauri::webview::Cookie;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tempfile::NamedTempFile;
use xpressclaw_core::desktop_auth::{
    credential_aad as desktop_credential_aad,
    credential_proof_message as desktop_credential_proof_message,
    derive_credential_keys as derive_desktop_credential_keys, identity_proof_message,
    DesktopCredentialPurpose, BROWSER_SESSION_COOKIE, BROWSER_SESSION_LIFETIME_SECONDS,
    CREDENTIAL_CHANNEL_NONCE, CREDENTIAL_REQUEST_DIRECTION, CREDENTIAL_RESPONSE_DIRECTION,
};
use zeroize::{Zeroize, Zeroizing};

const PROFILE_VERSION: u8 = 1;
const LOCAL_PROFILE_ID: &str = "local";
const KEYCHAIN_SERVICE: &str = "ai.xpress.xpressclaw.instance";
const MAX_AUTH_RESPONSE_BYTES: usize = 16 * 1024;
const PROFILE_INSPECTION_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredProfile {
    id: String,
    name: String,
    url: String,
    instance_id: Option<String>,
    #[serde(default)]
    identity_public_key: Option<String>,
    authentication: String,
    local: bool,
    #[serde(default)]
    confirmed_unauthenticated_remote: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ProfileFile {
    version: u8,
    active_profile_id: String,
    profiles: Vec<StoredProfile>,
}

impl Default for ProfileFile {
    fn default() -> Self {
        Self {
            version: PROFILE_VERSION,
            active_profile_id: LOCAL_PROFILE_ID.to_string(),
            profiles: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceProfile {
    id: String,
    name: String,
    url: String,
    instance_id: Option<String>,
    authentication: String,
    local: bool,
    active: bool,
    health: String,
    confirmed_unauthenticated_remote: bool,
}

#[derive(Debug, Serialize)]
pub struct ActiveProfileIdentity {
    identity_status: &'static str,
    navigation_status: &'static str,
    local: bool,
}

#[derive(Deserialize)]
pub struct SaveProfileInput {
    #[serde(default)]
    id: Option<String>,
    name: String,
    url: String,
    authentication: String,
    #[serde(default)]
    credential: Option<String>,
    #[serde(default)]
    confirm_unauthenticated_remote: bool,
}

#[derive(Debug, Deserialize)]
struct Bootstrap {
    instance_id: String,
    identity_public_key: String,
    authentication_enabled: bool,
    credential_kind: String,
}

#[derive(Debug, Deserialize)]
struct PendingInstanceSettings {
    instance_id: String,
    saved: PendingListenerSettings,
    password_configured: bool,
}

#[derive(Debug, Deserialize)]
struct PendingListenerSettings {
    authentication_enabled: bool,
}

#[derive(Debug, Deserialize)]
struct IdentityProof {
    instance_id: String,
    identity_public_key: String,
    signature: String,
}

#[derive(Debug, Deserialize)]
struct DesktopCredentialProof {
    instance_id: String,
    identity_public_key: String,
    exchange_id: String,
    server_public_key: String,
    signature: String,
}

struct DesktopCredentialChannel {
    exchange_id: String,
    exchange_id_bytes: [u8; 32],
    request_key: Zeroizing<[u8; 32]>,
    response_key: Zeroizing<[u8; 32]>,
}

#[derive(Debug, Deserialize)]
struct EncryptedDesktopSession {
    ciphertext: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DesktopAuthResponse {
    Validated {
        instance_id: String,
    },
    BrowserSession {
        instance_id: String,
        session: String,
    },
}

struct DesktopBrowserSession {
    session: Zeroizing<String>,
}

pub struct ProfileState {
    path: PathBuf,
    file: Mutex<ProfileFile>,
    /// Serialize profile/keychain mutations so their compensating writes are
    /// atomic from the Desktop command surface.
    mutation_lock: tokio::sync::Mutex<()>,
    /// Secure process-memory fallback when an OS keychain is temporarily
    /// unavailable. It rotates with the local server and is never serialized.
    local_ephemeral_credential: Mutex<Option<Zeroizing<String>>>,
    /// Public key announced by the exact bundled child after it owns both
    /// listeners. This is process-local and lets first-use local pairing avoid
    /// trusting an unrelated process that happened to win the port.
    local_bound_identity: Mutex<Option<String>>,
}

impl ProfileState {
    pub fn load(app: &AppHandle, local_url: &str) -> Result<Self, String> {
        let directory = app
            .path()
            .app_config_dir()
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let path = directory.join("instance-profiles.json");
        let mut file = if path.exists() {
            serde_json::from_slice::<ProfileFile>(
                &std::fs::read(&path).map_err(|error| error.to_string())?,
            )
            .map_err(|error| format!("Could not read Desktop instance profiles: {error}"))?
        } else {
            ProfileFile::default()
        };
        if file.version != PROFILE_VERSION {
            return Err(format!(
                "Unsupported Desktop instance profile version {}",
                file.version
            ));
        }
        if let Some(local) = file.profiles.iter_mut().find(|profile| profile.local) {
            local.id = LOCAL_PROFILE_ID.to_string();
            local.name = "Local XpressClaw".to_string();
            local.url = local_url.to_string();
        } else {
            file.profiles.insert(
                0,
                StoredProfile {
                    id: LOCAL_PROFILE_ID.to_string(),
                    name: "Local XpressClaw".to_string(),
                    url: local_url.to_string(),
                    instance_id: None,
                    identity_public_key: None,
                    authentication: "none".to_string(),
                    local: true,
                    confirmed_unauthenticated_remote: true,
                },
            );
        }
        if !file
            .profiles
            .iter()
            .any(|profile| profile.id == file.active_profile_id)
        {
            file.active_profile_id = LOCAL_PROFILE_ID.to_string();
        }
        let state = Self {
            path,
            file: Mutex::new(file),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };
        state.persist()?;
        Ok(state)
    }

    fn persist(&self) -> Result<(), String> {
        let file = self.file.lock().map_err(|_| "Profile lock failed")?;
        persist_file(&self.path, &file)
    }

    fn update<T>(
        &self,
        change: impl FnOnce(&mut ProfileFile) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut current = self.file.lock().map_err(|_| "Profile lock failed")?;
        let mut next = current.clone();
        let result = change(&mut next)?;
        persist_file(&self.path, &next)?;
        *current = next;
        Ok(result)
    }

    fn active(&self) -> Result<StoredProfile, String> {
        let file = self.file.lock().map_err(|_| "Profile lock failed")?;
        file.profiles
            .iter()
            .find(|profile| profile.id == file.active_profile_id)
            .cloned()
            .ok_or_else(|| "The selected Desktop profile no longer exists".to_string())
    }

    fn set_local_bootstrap(&self, bootstrap: &Bootstrap) -> Result<StoredProfile, String> {
        let authentication = effective_authentication(bootstrap)?.to_string();
        self.update(|file| {
            let local = file
                .profiles
                .iter_mut()
                .find(|profile| profile.local)
                .ok_or_else(|| "Local profile is missing".to_string())?;
            if local.identity_public_key.is_some() {
                require_profile_identity(local, bootstrap)?;
            } else if local
                .instance_id
                .as_deref()
                .is_some_and(|expected| expected != bootstrap.instance_id)
            {
                return Err(
                    "The local XpressClaw instance identity changed. Explicitly trust the replacement before Desktop can use credentials."
                        .to_string(),
                );
            }
            local.instance_id = Some(bootstrap.instance_id.clone());
            local.identity_public_key = Some(bootstrap.identity_public_key.clone());
            local.authentication = authentication;
            Ok(local.clone())
        })
    }

    fn replace_local_bootstrap(
        &self,
        previous_instance_id: &str,
        bootstrap: &Bootstrap,
    ) -> Result<(), String> {
        let authentication = effective_authentication(bootstrap)?.to_string();
        self.update(|file| {
            if file.active_profile_id != LOCAL_PROFILE_ID {
                return Err("Connect to the local profile before trusting its replacement".into());
            }
            let local = file
                .profiles
                .iter_mut()
                .find(|profile| profile.local)
                .ok_or_else(|| "Local profile is missing".to_string())?;
            if local.instance_id.as_deref() != Some(previous_instance_id) {
                return Err(
                    "The saved local instance identity changed while confirmation was open; try again"
                        .to_string(),
                );
            }
            if previous_instance_id == bootstrap.instance_id {
                return Err("This local instance identity is already trusted".to_string());
            }
            local.instance_id = Some(bootstrap.instance_id.clone());
            local.identity_public_key = Some(bootstrap.identity_public_key.clone());
            local.authentication = authentication;
            Ok(())
        })
    }

    fn clear_local_credential(&self) -> Result<(), String> {
        let mut current = self
            .local_ephemeral_credential
            .lock()
            .map_err(|_| "Local credential lock failed".to_string())?;
        *current = None;
        delete_credential(LOCAL_PROFILE_ID)
    }

    fn forget_local_credential_if_unchanged(&self, observed: Option<&str>) -> Result<(), String> {
        let mut current = self
            .local_ephemeral_credential
            .lock()
            .map_err(|_| "Local credential lock failed".to_string())?;
        let unchanged = match (current.as_ref(), observed) {
            (Some(current), Some(observed)) => current.as_str() == observed,
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            // A listener-bound token arrived after the caller's snapshot. It
            // belongs to a newer managed sidecar start and must not be erased
            // as though it were the previous instance's credential.
            return Ok(());
        }
        *current = None;
        delete_credential(LOCAL_PROFILE_ID)
    }

    fn local_startup_token(&self) -> Result<Option<Zeroizing<String>>, String> {
        self.local_ephemeral_credential
            .lock()
            .map_err(|_| "Local credential lock failed".to_string())
            .map(|credential| credential.clone())
    }

    fn select_local(&self) -> Result<String, String> {
        self.update(|file| {
            let local = file
                .profiles
                .iter()
                .find(|profile| profile.local)
                .ok_or_else(|| "Local profile is missing".to_string())?;
            let url = local.url.clone();
            file.active_profile_id = LOCAL_PROFILE_ID.to_string();
            Ok(url)
        })
    }

    pub fn active_url(&self) -> Result<String, String> {
        self.active().map(|profile| profile.url)
    }

    pub fn active_is_local(&self) -> Result<bool, String> {
        self.active().map(|profile| profile.local)
    }

    pub fn remember_local_startup_token(&self, token: Zeroizing<String>) -> Result<(), String> {
        // Serialize process-memory and keychain updates with replacement
        // recovery so the latter cannot erase a freshly rotated sidecar token
        // between the two stores.
        let mut current = self
            .local_ephemeral_credential
            .lock()
            .map_err(|_| "Local credential lock failed".to_string())?;
        *current = Some(token.clone());
        set_credential(LOCAL_PROFILE_ID, &token)
    }

    pub fn remember_local_bound_identity(&self, identity: &str) -> Result<(), String> {
        validate_identity_public_key(identity)?;
        let mut current = self
            .local_bound_identity
            .lock()
            .map_err(|_| "Local identity lock failed".to_string())?;
        *current = Some(identity.to_string());
        Ok(())
    }

    fn local_bound_identity(&self) -> Result<Option<String>, String> {
        self.local_bound_identity
            .lock()
            .map_err(|_| "Local identity lock failed".to_string())
            .map(|identity| identity.clone())
    }
}

pub(crate) async fn verify_managed_local_instance(state: &ProfileState) -> Result<String, String> {
    let profile = {
        let file = state.file.lock().map_err(|_| "Profile lock failed")?;
        file.profiles
            .iter()
            .find(|profile| profile.local)
            .cloned()
            .ok_or_else(|| "Local profile is missing".to_string())?
    };
    // A saved cryptographic pin can authenticate an already-running managed
    // sidecar. First use has no such pin, so require the public key announced
    // by the exact child process after it acquired both listeners.
    let expected_identity = if let Some(identity) = profile.identity_public_key.clone() {
        identity
    } else {
        let mut identity = None;
        for _ in 0..40 {
            identity = state.local_bound_identity()?;
            if identity.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        identity.ok_or_else(|| {
            "The bundled sidecar did not confirm ownership of the local listeners".to_string()
        })?
    };
    let bootstrap = fetch_verified_bootstrap(&profile.url, Some(&expected_identity)).await?;
    if profile.identity_public_key.is_some() && !profile_identity_matches(&profile, &bootstrap) {
        return Err(
            "The bound local sidecar does not match the saved Desktop instance identity"
                .to_string(),
        );
    }
    state.set_local_bootstrap(&bootstrap)?;
    Ok(profile.url)
}

#[tauri::command]
pub async fn list_instance_profiles(
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
) -> Result<Vec<InstanceProfile>, String> {
    require_active_profile_identity(&state, &webview).await?;
    let (profiles, active) = {
        let file = state.file.lock().map_err(|_| "Profile lock failed")?;
        (file.profiles.clone(), file.active_profile_id.clone())
    };
    inspect_instance_profiles(&state, profiles, &active).await
}

async fn inspect_instance_profiles(
    state: &ProfileState,
    profiles: Vec<StoredProfile>,
    active: &str,
) -> Result<Vec<InstanceProfile>, String> {
    let inspected = futures_util::stream::iter(profiles)
        .map(|profile| summarize_profile(state, profile, active))
        // Unreachable profiles can consume the full request timeout. Inspect
        // independent endpoints concurrently, but cap fan-out so a large
        // profile file cannot create an unbounded connection burst.
        .buffered(PROFILE_INSPECTION_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    inspected.into_iter().collect()
}

async fn summarize_profile(
    state: &ProfileState,
    profile: StoredProfile,
    active: &str,
) -> Result<InstanceProfile, String> {
    let (health, bootstrap) = inspect_profile(state, &profile).await;
    let mut authentication = profile.authentication.clone();
    let mut instance_id = profile.instance_id.clone();
    if profile.local {
        if let Some(bootstrap) = bootstrap.as_ref() {
            if profile_identity_matches(&profile, bootstrap) {
                // Listing is passive discovery. Report the live values, but
                // establish the local identity pin only on an explicit
                // connect/login path so a stale keychain entry cannot be
                // reused merely because Settings observed a listener.
                authentication = effective_authentication(bootstrap)?.to_string();
                instance_id = Some(bootstrap.instance_id.clone());
            }
        }
    }
    Ok(InstanceProfile {
        id: profile.id.clone(),
        name: profile.name,
        url: profile.url,
        instance_id,
        authentication,
        local: profile.local,
        active: profile.id == active,
        health,
        confirmed_unauthenticated_remote: profile.confirmed_unauthenticated_remote,
    })
}

#[tauri::command]
pub async fn save_instance_profile(
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
    input: SaveProfileInput,
) -> Result<InstanceProfile, String> {
    let _mutation_guard = state.mutation_lock.lock().await;
    require_active_profile_identity(&state, &webview).await?;
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("Profile name must be between 1 and 80 characters".to_string());
    }
    let url = normalize_url(&input.url)?;
    if !matches!(
        input.authentication.as_str(),
        "password" | "startup_token" | "none"
    ) {
        return Err("Choose password, startup token, or no authentication".to_string());
    }
    if input.authentication == "none" && !input.confirm_unauthenticated_remote {
        return Err(
            "Confirm that this unauthenticated profile uses an operator-trusted LAN or tailnet"
                .to_string(),
        );
    }
    // Move the IPC string into a zeroizing owner before any network I/O.
    let mut credential = input.credential.map(Zeroizing::new);
    if credential
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 4096)
    {
        return Err("Profile credential length is invalid".to_string());
    }

    let editing = input.id.is_some();
    let (id, existing) = if let Some(id) = input.id {
        let file = state.file.lock().map_err(|_| "Profile lock failed")?;
        let existing = file
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| "Desktop profile not found".to_string())?;
        if existing.local {
            return Err("The automatic local profile cannot be replaced".to_string());
        }
        (id, Some(existing.clone()))
    } else {
        (uuid::Uuid::new_v4().simple().to_string(), None)
    };
    if id == LOCAL_PROFILE_ID {
        return Err("The automatic local profile cannot be replaced".to_string());
    }

    let bootstrap = fetch_bootstrap(&url).await?;
    verify_bootstrap_identity(
        &url,
        &bootstrap,
        existing
            .as_ref()
            .and_then(|profile| profile.identity_public_key.as_deref()),
    )
    .await?;
    let expected_authentication = effective_authentication(&bootstrap)?;
    if input.authentication != expected_authentication {
        return Err(match expected_authentication {
            "none" => "This instance currently has authentication disabled".to_string(),
            "password" => "This instance currently requires its password".to_string(),
            "startup_token" => {
                "This instance currently requires the startup token from its latest launch"
                    .to_string()
            }
            _ => unreachable!(),
        });
    }
    if existing.as_ref().is_some_and(|profile| {
        profile.instance_id.as_deref() != Some(bootstrap.instance_id.as_str())
            || profile
                .identity_public_key
                .as_deref()
                .is_some_and(|expected| expected != bootstrap.identity_public_key)
    }) {
        return Err("The instance identity at this address changed. Delete and re-add the profile only if you trust the replacement.".to_string());
    }

    let previous_credential = if editing {
        get_optional_credential(&id)?
    } else {
        None
    };
    if bootstrap.authentication_enabled {
        let retained;
        let supplied = if let Some(supplied) = credential.as_ref() {
            supplied
        } else if existing.as_ref().is_some_and(|profile| {
            may_reuse_stored_credential(profile, &url, &input.authentication, &bootstrap)
        }) {
            retained = get_credential(&id)?;
            &retained
        } else {
            return Err(
                "Enter the credential again when changing a profile address or authentication mode"
                    .to_string(),
            );
        };
        validate_desktop_credential(&url, supplied, &bootstrap).await?;
        if credential.is_some() {
            set_credential(&id, supplied)?;
        }
    } else if editing {
        delete_credential(&id)?;
    }
    if let Some(value) = credential.as_mut() {
        value.zeroize();
    }

    let confirmed_unauthenticated_remote =
        input.authentication == "none" && input.confirm_unauthenticated_remote;
    let stored = StoredProfile {
        id: id.clone(),
        name: name.to_string(),
        url: url.clone(),
        instance_id: Some(bootstrap.instance_id),
        identity_public_key: Some(bootstrap.identity_public_key),
        authentication: input.authentication,
        local: false,
        confirmed_unauthenticated_remote,
    };
    if let Err(error) = state.update(|file| {
        if let Some(existing) = file.profiles.iter_mut().find(|profile| profile.id == id) {
            *existing = stored.clone();
        } else if editing {
            return Err("Desktop profile was deleted while it was being edited".to_string());
        } else {
            file.profiles.push(stored.clone());
        }
        Ok(())
    }) {
        let rollback = if let Some(previous) = previous_credential.as_ref() {
            set_credential(&id, previous)
        } else {
            delete_credential(&id)
        };
        return Err(match rollback {
            Ok(()) => error,
            Err(rollback) => {
                format!("{error}; the profile credential also could not be restored: {rollback}")
            }
        });
    }
    Ok(InstanceProfile {
        id: stored.id,
        name: stored.name,
        url: stored.url,
        instance_id: stored.instance_id,
        authentication: stored.authentication,
        local: false,
        active: false,
        health: "healthy".to_string(),
        confirmed_unauthenticated_remote: stored.confirmed_unauthenticated_remote,
    })
}

#[tauri::command]
pub async fn select_instance_profile(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
    id: String,
) -> Result<(), String> {
    let _mutation_guard = state.mutation_lock.lock().await;
    let active_profile = require_active_profile_origin(&state, &webview)?;
    let returning_from_remote = id == LOCAL_PROFILE_ID && !active_profile.local;
    // A page served by a replaced remote instance must retain one safe escape
    // hatch back to the automatic local profile. Every other selection first
    // proves that the currently loaded page still belongs to its pinned
    // instance, so a same-origin replacement cannot pivot into another saved
    // profile.
    if id != LOCAL_PROFILE_ID || active_profile.local {
        require_active_profile_identity_for(&state, &webview, active_profile, false).await?;
    }
    let (mut profile, previous_active) = {
        let file = state.file.lock().map_err(|_| "Profile lock failed")?;
        let profile = file
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| "Desktop profile not found".to_string())?;
        (profile, file.active_profile_id.clone())
    };
    let bootstrap = match fetch_bootstrap(&profile.url).await {
        Ok(bootstrap) => bootstrap,
        Err(_) if profile.local && returning_from_remote => {
            return enter_local_recovery(&app, &state, &profile)
        }
        Err(error) => return Err(error),
    };
    let local_bound_identity = profile
        .local
        .then(|| state.local_bound_identity())
        .transpose()?
        .flatten();
    let expected_identity = profile
        .identity_public_key
        .as_deref()
        .or(local_bound_identity.as_deref());
    let identity_result = if let Some(expected_identity) = expected_identity {
        verify_bootstrap_identity(&profile.url, &bootstrap, Some(expected_identity)).await
    } else {
        Err("Desktop has no trusted identity proof for this instance".to_string())
    };
    if local_profile_requires_recovery(&profile, &bootstrap, identity_result.is_ok()) {
        // Enter a credential-free recovery state even when the caller is a
        // healthy remote profile. The local page receives only guarded profile
        // commands until the operator explicitly trusts the replacement.
        return enter_local_recovery(&app, &state, &profile);
    }
    identity_result?;
    if profile.identity_public_key.is_some() {
        require_profile_identity(&profile, &bootstrap)?;
    }
    let credential_profile = profile.clone();
    if profile.local {
        profile = state.set_local_bootstrap(&bootstrap)?;
    } else {
        validate_remote_profile_navigation(&profile, &bootstrap)?;
    }
    if profile.local && bootstrap.authentication_enabled {
        let expected = effective_authentication(&bootstrap)?;
        let credential =
            credential_for_authentication(&state, &credential_profile, expected).await?;
        validate_desktop_credential(&profile.url, &credential, &bootstrap).await?;
    }
    state.update(|file| {
        let Some(current) = file.profiles.iter().find(|current| current.id == id) else {
            return Err("Desktop profile was deleted while connecting".to_string());
        };
        if current != &profile {
            return Err("Desktop profile changed while connecting; try again".to_string());
        }
        file.active_profile_id = id.clone();
        Ok(())
    })?;
    let rollback_selection = || {
        state.update(|file| {
            if file.active_profile_id == id
                && file
                    .profiles
                    .iter()
                    .any(|profile| profile.id == previous_active)
            {
                file.active_profile_id = previous_active.clone();
            }
            Ok(())
        })
    };
    if let Err(error) = crate::enable_profile_capabilities(&app, &profile.url, profile.local) {
        let _ = rollback_selection();
        return Err(error);
    }
    if let Err(error) = navigate_to_profile(&app, &profile.url) {
        let _ = rollback_selection();
        return Err(error);
    }
    Ok(())
}

fn enter_local_recovery(
    app: &AppHandle,
    state: &ProfileState,
    profile: &StoredProfile,
) -> Result<(), String> {
    if !profile.local {
        return Err("Only the automatic local profile can enter recovery".to_string());
    }
    state.select_local()?;
    crate::enable_profile_capabilities(app, &profile.url, false)?;
    navigate_to_profile(app, &profile.url)
}

fn navigate_to_profile(app: &AppHandle, url: &str) -> Result<(), String> {
    // The first slice deliberately enforces one profile for all Desktop
    // windows. Close secondary windows so none remain bound to stale state.
    for (label, window) in app.webview_windows() {
        if label != "main" {
            let _ = window.close();
        }
    }
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Main Desktop window is unavailable".to_string())?;
    window
        .navigate(
            url.parse()
                .map_err(|error| format!("Invalid profile URL: {error}"))?,
        )
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_instance_profile(
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
    id: String,
) -> Result<(), String> {
    let _mutation_guard = state.mutation_lock.lock().await;
    require_active_profile_identity(&state, &webview).await?;
    if id == LOCAL_PROFILE_ID {
        return Err("The automatic local profile cannot be deleted".to_string());
    }
    {
        let file = state.file.lock().map_err(|_| "Profile lock failed")?;
        if !file.profiles.iter().any(|profile| profile.id == id) {
            return Err("Desktop profile not found".to_string());
        }
        if file.active_profile_id == id {
            return Err("Connect to another profile before deleting this one".to_string());
        }
    }
    // Remove any credential even for a nominally no-auth profile so stale
    // entries from older profile modes cannot survive profile deletion.
    let previous_credential = get_optional_credential(&id)?;
    delete_credential(&id)?;
    if let Err(error) = state.update(|file| {
        if file.active_profile_id == id {
            return Err("Connect to another profile before deleting this one".to_string());
        }
        let before = file.profiles.len();
        file.profiles.retain(|profile| profile.id != id);
        if before == file.profiles.len() {
            return Err("Desktop profile not found".to_string());
        }
        Ok(())
    }) {
        if let Some(previous) = previous_credential {
            if let Err(rollback) = set_credential(&id, &previous) {
                return Err(format!(
                    "{error}; the profile credential also could not be restored: {rollback}"
                ));
            }
        }
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
pub async fn login_active_profile(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
) -> Result<bool, String> {
    let _mutation_guard = state.mutation_lock.lock().await;
    let profile = require_active_profile_origin(&state, &webview)?;
    let (mut profile, bootstrap) =
        require_active_profile_identity_for(&state, &webview, profile, true).await?;
    let credential_profile = profile.clone();
    if profile.local {
        profile = state.set_local_bootstrap(&bootstrap)?;
        crate::enable_verified_local_capabilities(&app, &profile.url)?;
    }
    if !bootstrap.authentication_enabled {
        return Ok(false);
    }
    let expected = effective_authentication(&bootstrap)?;
    // A remote page must authenticate through its visible browser origin.
    // Native code cannot bind the WebView's later cookie-bearing connection to
    // the separately proved XpressClaw identity, even when that origin uses
    // HTTPS, so silently attempting keychain login would reintroduce a relay.
    if !profile.local {
        return Ok(false);
    }
    if profile.authentication != expected {
        return Err(format!(
            "This instance now requires {expected}; edit the profile before reconnecting"
        ));
    }
    require_listener_bound_local_session_origin(&state, &profile, &bootstrap)?;
    let credential = credential_for_authentication(&state, &credential_profile, expected).await?;
    let session = request_desktop_session(&state, &profile, &credential, &bootstrap).await?;
    if require_active_profile_origin(&state, &webview)? != profile {
        return Err("The selected Desktop profile changed while authentication completed".into());
    }
    install_browser_session_cookie(&webview, &profile.url, &session)?;
    Ok(true)
}

#[tauri::command]
pub async fn get_active_instance_profile(
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
) -> Result<ActiveProfileIdentity, String> {
    let _mutation_guard = state.mutation_lock.lock().await;
    // The saved instance ID is a native-only pin. Returning it to content from
    // the selected origin would let a same-origin replacement replay it from
    // its own bootstrap endpoint and trick a later credential-bearing command.
    // Report only the result of the native comparison.
    let profile = require_active_profile_origin(&state, &webview)?;
    let bootstrap = fetch_bootstrap(&profile.url).await?;
    let identity_status = match profile.identity_public_key.as_deref() {
        None => "unpinned",
        Some(expected)
            if require_profile_identity(&profile, &bootstrap).is_ok()
                && verify_bootstrap_identity(&profile.url, &bootstrap, Some(expected))
                    .await
                    .is_ok() =>
        {
            "matched"
        }
        Some(_) => "changed",
    };
    let navigation_status = active_profile_navigation_status(&profile, &bootstrap, identity_status);
    if require_active_profile_origin(&state, &webview)? != profile {
        return Err("The selected Desktop profile changed while its identity was inspected".into());
    }
    Ok(ActiveProfileIdentity {
        identity_status,
        navigation_status,
        local: profile.local,
    })
}

#[tauri::command]
pub async fn trust_local_instance_replacement(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
    instance_id: String,
) -> Result<(), String> {
    let _mutation_guard = state.mutation_lock.lock().await;
    // Identity mismatch is the condition this native-confirmation recovery
    // command repairs. It is the sole mutating command allowed past the origin
    // boundary without first matching the saved pin.
    let profile = require_active_profile_origin(&state, &webview)?;
    if !profile.local {
        return Err("Only the automatic local profile can use this recovery action".to_string());
    }
    let previous_instance_id = profile.instance_id.as_deref().ok_or_else(|| {
        "The local profile does not have a pinned identity to replace".to_string()
    })?;
    let bootstrap = fetch_verified_bootstrap(&profile.url, None).await?;
    if bootstrap.instance_id != instance_id {
        return Err(
            "The local instance identity changed again before confirmation; reload and try again"
                .to_string(),
        );
    }
    if previous_instance_id == bootstrap.instance_id {
        return Err("This local instance identity is already trusted".to_string());
    }
    effective_authentication(&bootstrap)?;

    let dialog = webview.clone();
    let confirmed = tauri::async_runtime::spawn_blocking(move || {
        dialog
            .dialog()
            .message(
                "Another XpressClaw instance is answering on the local address. Trusting it will discard credentials for the previous instance. Desktop retains a new listener-bound startup token only if the replacement verifies it. Continue only if you intentionally reset or replaced this local instance.",
            )
            .title("Trust replacement local instance?")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Trust replacement".into(),
                "Cancel".into(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|error| format!("Could not show the local replacement confirmation: {error}"))?;
    if !confirmed {
        return Err("The replacement local instance was not trusted".to_string());
    }

    // Re-read the identity after the operator responds so a process cannot
    // swap the listener while the native confirmation is open.
    let confirmed_bootstrap =
        fetch_verified_bootstrap(&profile.url, Some(&bootstrap.identity_public_key)).await?;
    if confirmed_bootstrap.instance_id != instance_id {
        return Err(
            "The local instance identity changed while confirmation was open; reload and try again"
                .to_string(),
        );
    }
    effective_authentication(&confirmed_bootstrap)?;

    // Discard credentials belonging to the previous instance, but retain a
    // listener-bound startup token only after the replacement accepts it. The
    // Desktop sidecar may already have delivered that fresh token while the
    // native confirmation dialog was open.
    preserve_verified_replacement_token_or_forget(&state, &profile, &confirmed_bootstrap).await?;
    state.replace_local_bootstrap(previous_instance_id, &confirmed_bootstrap)?;
    crate::enable_verified_local_capabilities(&app, &profile.url)
}

async fn preserve_verified_replacement_token_or_forget(
    state: &ProfileState,
    profile: &StoredProfile,
    bootstrap: &Bootstrap,
) -> Result<bool, String> {
    let observed = state.local_startup_token()?;
    let preserve = if effective_authentication(bootstrap)? == "startup_token" {
        if let Some(credential) = observed.as_ref() {
            validate_desktop_credential(&profile.url, credential, bootstrap)
                .await
                .is_ok()
        } else {
            false
        }
    } else {
        false
    };
    if !preserve {
        state.forget_local_credential_if_unchanged(
            observed.as_ref().map(|credential| credential.as_str()),
        )?;
    }
    Ok(preserve)
}

#[tauri::command]
pub async fn store_active_profile_credential(
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
    credential: Option<String>,
) -> Result<(), String> {
    let _mutation_guard = state.mutation_lock.lock().await;
    let (mut profile, bootstrap) = require_active_profile_identity(&state, &webview).await?;
    if let Some(value) = credential {
        let value = Zeroizing::new(value);
        if profile.local {
            profile = state.set_local_bootstrap(&bootstrap)?;
        }
        let authentication = effective_authentication(&bootstrap)?;
        let stored_authentication = if authentication == "none" {
            require_pending_password_configuration(&profile.url, &bootstrap.instance_id).await?;
            "password"
        } else {
            authentication
        };
        if profile.local {
            *state
                .local_ephemeral_credential
                .lock()
                .map_err(|_| "Local credential lock failed".to_string())? = Some(value.clone());
            // If the OS keychain is unavailable, the fresh credential remains
            // usable in process memory for this sidecar lifetime. The caller
            // still receives the keychain error so persistence is explicit.
            return set_credential(&profile.id, &value);
        }

        let next_instance_id = bootstrap.instance_id;
        let previous_credential = get_optional_credential(&profile.id)?;
        set_credential(&profile.id, &value)?;
        if let Err(error) = state.update(|file| {
            let current = file
                .profiles
                .iter_mut()
                .find(|current| current.id == profile.id)
                .ok_or_else(|| "The selected Desktop profile no longer exists".to_string())?;
            current.instance_id = Some(next_instance_id);
            current.authentication = stored_authentication.to_string();
            Ok(())
        }) {
            let rollback = if let Some(previous) = previous_credential {
                set_credential(&profile.id, &previous)
            } else {
                delete_credential(&profile.id)
            };
            return Err(match rollback {
                Ok(()) => error,
                Err(rollback) => {
                    format!("{error}; the previous keychain credential also could not be restored: {rollback}")
                }
            });
        }
        Ok(())
    } else {
        if profile.local {
            state.clear_local_credential()?;
        } else {
            delete_credential(&profile.id)?;
        }
        Ok(())
    }
}

pub async fn preferred_startup_url(state: &ProfileState) -> String {
    let Ok(profile) = state.active() else {
        return "http://localhost:8935".to_string();
    };
    if profile.local {
        return profile.url;
    }
    let remote_is_usable = match profile.identity_public_key.as_deref() {
        Some(expected_identity) => {
            match fetch_verified_bootstrap(&profile.url, Some(expected_identity)).await {
                Ok(bootstrap) if profile_identity_matches(&profile, &bootstrap) => {
                    validate_remote_profile_navigation(&profile, &bootstrap).is_ok()
                }
                _ => false,
            }
        }
        None => false,
    };
    if remote_is_usable {
        profile.url
    } else {
        state
            .select_local()
            .unwrap_or_else(|_| "http://localhost:8935".to_string())
    }
}

fn validate_remote_profile_navigation(
    profile: &StoredProfile,
    bootstrap: &Bootstrap,
) -> Result<(), String> {
    if bootstrap.authentication_enabled {
        // Remote profiles authenticate in their visible browser origin. Once
        // the target has proved its pinned instance identity, navigate to its
        // login page even when a saved password or rotating startup token is
        // stale. Native code must not read or validate keychain material before
        // that navigation; a successful browser login updates the saved mode
        // and credential afterward.
        effective_authentication(bootstrap)?;
        return Ok(());
    }
    if !profile.confirmed_unauthenticated_remote {
        return Err("Confirm unauthenticated remote access before connecting".to_string());
    }
    if profile.authentication != "none" {
        return Err(
            "This instance now has authentication disabled; edit the profile and confirm its trusted network before connecting"
                .to_string(),
        );
    }
    Ok(())
}

fn active_profile_navigation_status(
    profile: &StoredProfile,
    bootstrap: &Bootstrap,
    identity_status: &str,
) -> &'static str {
    if profile.local || identity_status == "changed" {
        return "ready";
    }
    if identity_status != "matched" {
        return "profile_review_required";
    }
    match validate_remote_profile_navigation(profile, bootstrap) {
        Ok(()) => "ready",
        Err(_) if !bootstrap.authentication_enabled => "confirmation_required",
        Err(_) => "profile_review_required",
    }
}

/// Dynamic Tauri capabilities cannot be revoked after a profile switch. Keep
/// even the two recovery-safe commands bound to the origin that is selected
/// *now* so a previously selected instance cannot reuse a stale capability.
fn require_active_profile_origin(
    state: &ProfileState,
    webview: &tauri::WebviewWindow,
) -> Result<StoredProfile, String> {
    let profile = state.active()?;
    let actual = webview
        .url()
        .map_err(|error| format!("Could not verify the Desktop page origin: {error}"))?;
    if !urls_have_same_origin(&profile.url, &actual) {
        return Err(
            "This Desktop page no longer belongs to the selected instance profile".to_string(),
        );
    }
    Ok(profile)
}

/// Protect profile metadata and all keychain/configuration mutations with the
/// persisted instance pin as well as the web origin. This check is native so a
/// replacement server at the same URL cannot bypass it by skipping the login
/// page (notably when authentication is disabled).
async fn require_active_profile_identity(
    state: &ProfileState,
    webview: &tauri::WebviewWindow,
) -> Result<(StoredProfile, Bootstrap), String> {
    let profile = require_active_profile_origin(state, webview)?;
    require_active_profile_identity_for(state, webview, profile, false).await
}

async fn require_active_profile_identity_for(
    state: &ProfileState,
    webview: &tauri::WebviewWindow,
    profile: StoredProfile,
    allow_unpinned_local: bool,
) -> Result<(StoredProfile, Bootstrap), String> {
    if (profile.instance_id.is_none() || profile.identity_public_key.is_none())
        && !(allow_unpinned_local && profile.local)
    {
        return Err(
            "Desktop has not established this instance identity yet; reconnect before using profile commands"
                .to_string(),
        );
    }
    let local_bound_identity = if allow_unpinned_local && profile.local {
        state.local_bound_identity()?
    } else {
        None
    };
    let expected_identity = profile
        .identity_public_key
        .as_deref()
        .or(local_bound_identity.as_deref())
        .ok_or_else(|| {
            "Desktop has not received a listener-bound identity for this local instance".to_string()
        })?;
    let bootstrap = fetch_matching_bootstrap(&profile, expected_identity).await?;

    // The bootstrap request yields across the executor. Recheck both the
    // selected profile and page origin afterward so a concurrent switch or
    // navigation cannot turn a successful check into authority for stale
    // state.
    if require_active_profile_origin(state, webview)? != profile {
        return Err("The selected Desktop profile changed while its identity was verified".into());
    }
    Ok((profile, bootstrap))
}

async fn fetch_matching_bootstrap(
    profile: &StoredProfile,
    expected_identity: &str,
) -> Result<Bootstrap, String> {
    let bootstrap = fetch_verified_bootstrap(&profile.url, Some(expected_identity)).await?;
    if profile.identity_public_key.is_some() {
        require_profile_identity(profile, &bootstrap)?;
    } else if profile.instance_id.is_some()
        && profile.instance_id.as_deref() != Some(bootstrap.instance_id.as_str())
    {
        return Err("The local XpressClaw instance identity changed".to_string());
    }
    Ok(bootstrap)
}

fn urls_have_same_origin(expected: &str, actual: &reqwest::Url) -> bool {
    reqwest::Url::parse(expected).is_ok_and(|expected| expected.origin() == actual.origin())
}

async fn inspect_profile(
    state: &ProfileState,
    profile: &StoredProfile,
) -> (String, Option<Bootstrap>) {
    match fetch_bootstrap(&profile.url).await {
        Ok(bootstrap) => {
            let local_bound_identity = profile
                .local
                .then(|| state.local_bound_identity().ok().flatten())
                .flatten();
            let expected_identity = profile
                .identity_public_key
                .as_deref()
                .or(local_bound_identity.as_deref());
            let proof_valid = if let Some(expected) = expected_identity {
                verify_bootstrap_identity(&profile.url, &bootstrap, Some(expected))
                    .await
                    .is_ok()
            } else {
                false
            };
            let metadata_matches = if profile.identity_public_key.is_some() {
                profile_identity_matches(profile, &bootstrap)
            } else {
                profile.local
                    && profile
                        .instance_id
                        .as_deref()
                        .is_none_or(|expected| expected == bootstrap.instance_id)
            };
            let status = if !proof_valid || !metadata_matches {
                "identity_changed"
            } else if bootstrap.authentication_enabled {
                match effective_authentication(&bootstrap) {
                    Ok(expected) if profile.local || expected == profile.authentication => {
                        // Profile listing is passive discovery. Validating a
                        // stored credential here would spend the instance's
                        // interactive brute-force budget every time Settings
                        // refreshes, potentially locking out the operator when
                        // a saved password or startup token is stale. The
                        // explicit Save/Connect/Login paths still authenticate.
                        if profile_credential(state, profile).is_ok() {
                            "reachable"
                        } else {
                            "authentication_required"
                        }
                    }
                    _ => "authentication_required",
                }
            } else if !profile.local
                && (profile.authentication != "none" || !profile.confirmed_unauthenticated_remote)
            {
                "authentication_required"
            } else {
                "healthy"
            };
            (status.to_string(), Some(bootstrap))
        }
        Err(_) => ("unreachable".to_string(), None),
    }
}

fn effective_authentication(bootstrap: &Bootstrap) -> Result<&'static str, String> {
    if !bootstrap.authentication_enabled {
        return Ok("none");
    }
    match bootstrap.credential_kind.as_str() {
        "password" => Ok("password"),
        "startup_token" => Ok("startup_token"),
        "restart_required" => {
            Err("Authentication changed on this instance; restart it before connecting".to_string())
        }
        _ => Err("The remote instance reported an unsupported authentication mode".to_string()),
    }
}

fn profile_identity_matches(profile: &StoredProfile, bootstrap: &Bootstrap) -> bool {
    profile.instance_id.as_deref() == Some(bootstrap.instance_id.as_str())
        && profile.identity_public_key.as_deref() == Some(bootstrap.identity_public_key.as_str())
}

fn local_profile_requires_recovery(
    profile: &StoredProfile,
    bootstrap: &Bootstrap,
    identity_proof_valid: bool,
) -> bool {
    profile.local
        && profile.identity_public_key.is_some()
        && (!identity_proof_valid
            || profile.instance_id.as_deref() != Some(bootstrap.instance_id.as_str()))
}

fn require_profile_identity(profile: &StoredProfile, bootstrap: &Bootstrap) -> Result<(), String> {
    if profile_identity_matches(profile, bootstrap) {
        return Ok(());
    }
    Err(if profile.local {
        "The local XpressClaw instance identity changed. Explicitly trust the replacement before Desktop can use credentials."
            .to_string()
    } else {
        "The instance identity at this address changed. Delete and re-add the profile only if you trust the replacement."
            .to_string()
    })
}

fn may_reuse_stored_credential(
    profile: &StoredProfile,
    url: &str,
    authentication: &str,
    bootstrap: &Bootstrap,
) -> bool {
    profile_identity_matches(profile, bootstrap)
        && profile.url == url
        && profile.authentication == authentication
}

fn profile_credential(
    state: &ProfileState,
    profile: &StoredProfile,
) -> Result<Zeroizing<String>, String> {
    if profile.local {
        if let Some(credential) = state
            .local_ephemeral_credential
            .lock()
            .map_err(|_| "Local credential lock failed".to_string())?
            .clone()
        {
            // The process-memory value belongs to this exact sidecar start.
            // Prefer it over a stale keychain entry when a keychain write was
            // denied or temporarily unavailable during token rotation.
            return Ok(credential);
        }
    }
    get_credential(&profile.id)
}

async fn credential_for_authentication(
    state: &ProfileState,
    profile: &StoredProfile,
    authentication: &str,
) -> Result<Zeroizing<String>, String> {
    if profile.local && authentication == "startup_token" {
        // The sidecar prints its per-start token after it owns the listeners,
        // but the stdout-drain thread can be scheduled a few milliseconds
        // later. Prefer the current process-memory token over a stale keychain
        // value and briefly wait for that handoff.
        for _ in 0..40 {
            if let Some(credential) = state
                .local_ephemeral_credential
                .lock()
                .map_err(|_| "Local credential lock failed".to_string())?
                .clone()
            {
                return Ok(credential);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    if profile.local && profile.instance_id.is_none() {
        return Err(
            "Enter the credential once to finish trusting this local XpressClaw instance"
                .to_string(),
        );
    }
    profile_credential(state, profile)
}

async fn fetch_bootstrap(url: &str) -> Result<Bootstrap, String> {
    let response = http_client()?
        .get(format!("{url}/api/auth/bootstrap"))
        .send()
        .await
        .map_err(|error| format!("Could not reach the XpressClaw instance: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "The remote server returned {} instead of XpressClaw bootstrap data",
            response.status()
        ));
    }
    read_bounded_json(
        response,
        "The remote address is not a compatible XpressClaw instance",
    )
    .await
}

fn validate_identity_public_key(value: &str) -> Result<Vec<u8>, String> {
    let key = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| "The XpressClaw instance returned an invalid identity key".to_string())?;
    if key.len() != 32 {
        return Err("The XpressClaw instance returned an invalid identity key".to_string());
    }
    Ok(key)
}

fn decode_fixed_32(value: &str, invalid_message: &str) -> Result<[u8; 32], String> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid_message.to_string())?
        .try_into()
        .map_err(|_| invalid_message.to_string())
}

async fn open_desktop_credential_channel(
    url: &str,
    bootstrap: &Bootstrap,
) -> Result<DesktopCredentialChannel, String> {
    let identity_public_key = validate_identity_public_key(&bootstrap.identity_public_key)?;
    let mut challenge = [0u8; 32];
    let rng = SystemRandom::new();
    rng.fill(&mut challenge)
        .map_err(|_| "Could not create a secure Desktop credential challenge".to_string())?;
    let client_private = EphemeralPrivateKey::generate(&X25519, &rng)
        .map_err(|_| "Could not create a secure Desktop credential channel".to_string())?;
    let client_public = client_private
        .compute_public_key()
        .map_err(|_| "Could not create a secure Desktop credential channel".to_string())?;
    let client_public_key: [u8; 32] = client_public
        .as_ref()
        .try_into()
        .map_err(|_| "Could not create a secure Desktop credential channel".to_string())?;

    let response = http_client()?
        .post(format!("{url}/api/auth/identity-proof"))
        .json(&serde_json::json!({
            "challenge": URL_SAFE_NO_PAD.encode(challenge),
            "client_public_key": URL_SAFE_NO_PAD.encode(client_public_key),
        }))
        .send()
        .await
        .map_err(|error| format!("Could not establish the Desktop credential channel: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "The Desktop credential channel failed with {}",
            response.status()
        ));
    }
    let proof: DesktopCredentialProof = read_bounded_json(
        response,
        "The XpressClaw instance returned an invalid Desktop credential proof",
    )
    .await?;
    if proof.instance_id != bootstrap.instance_id
        || proof.identity_public_key != bootstrap.identity_public_key
    {
        return Err(
            "The XpressClaw instance identity changed while opening the credential channel"
                .to_string(),
        );
    }
    let exchange_id = decode_fixed_32(
        &proof.exchange_id,
        "The XpressClaw instance returned an invalid credential exchange ID",
    )?;
    let server_public_key = decode_fixed_32(
        &proof.server_public_key,
        "The XpressClaw instance returned an invalid credential channel key",
    )?;
    let signature = URL_SAFE_NO_PAD.decode(proof.signature).map_err(|_| {
        "The XpressClaw instance returned an invalid Desktop credential proof".to_string()
    })?;
    UnparsedPublicKey::new(&ED25519, identity_public_key)
        .verify(
            &desktop_credential_proof_message(
                &bootstrap.instance_id,
                &challenge,
                &exchange_id,
                &client_public_key,
                &server_public_key,
            ),
            &signature,
        )
        .map_err(|_| {
            "The XpressClaw instance could not authenticate its credential channel".to_string()
        })?;
    let peer = AgreementPublicKey::new(&X25519, server_public_key);
    let keys = agree_ephemeral(client_private, &peer, |shared| {
        derive_desktop_credential_keys(
            shared,
            &bootstrap.instance_id,
            &challenge,
            &exchange_id,
            &client_public_key,
            &server_public_key,
        )
    })
    .map_err(|_| "The XpressClaw instance returned an invalid credential channel key".to_string())?
    .map_err(|_| "Could not derive the Desktop credential channel".to_string())?;
    Ok(DesktopCredentialChannel {
        exchange_id: proof.exchange_id,
        exchange_id_bytes: exchange_id,
        request_key: keys.request,
        response_key: keys.response,
    })
}

async fn verify_bootstrap_identity(
    url: &str,
    bootstrap: &Bootstrap,
    expected_public_key: Option<&str>,
) -> Result<(), String> {
    if expected_public_key.is_some_and(|expected| expected != bootstrap.identity_public_key) {
        return Err("The XpressClaw instance identity key changed".to_string());
    }
    let public_key = validate_identity_public_key(&bootstrap.identity_public_key)?;
    let mut challenge = [0u8; 32];
    SystemRandom::new()
        .fill(&mut challenge)
        .map_err(|_| "Could not create a secure instance identity challenge".to_string())?;
    let encoded_challenge = URL_SAFE_NO_PAD.encode(challenge);
    let response = http_client()?
        .post(format!("{url}/api/auth/identity-proof"))
        .json(&serde_json::json!({ "challenge": encoded_challenge }))
        .send()
        .await
        .map_err(|error| format!("Could not verify the XpressClaw instance identity: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "The XpressClaw instance identity proof failed with {}",
            response.status()
        ));
    }
    let proof: IdentityProof = read_bounded_json(
        response,
        "The XpressClaw instance returned an invalid identity proof",
    )
    .await?;
    if proof.instance_id != bootstrap.instance_id
        || proof.identity_public_key != bootstrap.identity_public_key
    {
        return Err("The XpressClaw instance identity changed during verification".to_string());
    }
    let signature = URL_SAFE_NO_PAD
        .decode(proof.signature)
        .map_err(|_| "The XpressClaw instance returned an invalid identity proof".to_string())?;
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(
            &identity_proof_message(&bootstrap.instance_id, &challenge),
            &signature,
        )
        .map_err(|_| "The XpressClaw instance could not prove its saved identity".to_string())
}

async fn fetch_verified_bootstrap(
    url: &str,
    expected_public_key: Option<&str>,
) -> Result<Bootstrap, String> {
    let bootstrap = fetch_bootstrap(url).await?;
    verify_bootstrap_identity(url, &bootstrap, expected_public_key).await?;
    Ok(bootstrap)
}

async fn require_pending_password_configuration(
    url: &str,
    expected_instance_id: &str,
) -> Result<(), String> {
    let response = http_client()?
        .get(format!("{url}/api/settings/instance/"))
        .send()
        .await
        .map_err(|error| format!("Could not verify pending instance authentication: {error}"))?;
    if !response.status().is_success() {
        return Err(
            "The selected instance does not currently require a credential, and its pending authentication settings could not be verified"
                .to_string(),
        );
    }
    let settings: PendingInstanceSettings = read_bounded_json(
        response,
        "The selected instance returned invalid pending authentication settings",
    )
    .await?;
    validate_pending_password_configuration(&settings, expected_instance_id)
}

fn validate_pending_password_configuration(
    settings: &PendingInstanceSettings,
    expected_instance_id: &str,
) -> Result<(), String> {
    if settings.instance_id != expected_instance_id {
        return Err(
            "The instance identity changed while Desktop verified pending authentication"
                .to_string(),
        );
    }
    if !settings.saved.authentication_enabled || !settings.password_configured {
        return Err("The selected instance does not require a credential".to_string());
    }
    Ok(())
}

async fn request_desktop_auth(
    url: &str,
    credential: &str,
    bootstrap: &Bootstrap,
    purpose: DesktopCredentialPurpose,
) -> Result<DesktopAuthResponse, String> {
    let channel = open_desktop_credential_channel(url, bootstrap).await?;
    let key = LessSafeKey::new(
        UnboundKey::new(&CHACHA20_POLY1305, channel.request_key.as_ref())
            .map_err(|_| "Could not open the Desktop credential channel".to_string())?,
    );
    let mut encrypted_credential = Zeroizing::new(credential.as_bytes().to_vec());
    let aad = desktop_credential_aad(
        &bootstrap.instance_id,
        &channel.exchange_id_bytes,
        CREDENTIAL_REQUEST_DIRECTION,
        purpose,
    );
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(CREDENTIAL_CHANNEL_NONCE),
        Aad::from(aad),
        &mut *encrypted_credential,
    )
    .map_err(|_| "Could not encrypt the Desktop credential".to_string())?;
    let response = http_client()?
        .post(format!("{url}/api/auth/desktop-session"))
        .json(&serde_json::json!({
            "exchange_id": channel.exchange_id,
            "ciphertext": URL_SAFE_NO_PAD.encode(encrypted_credential.as_slice()),
            "purpose": purpose,
        }))
        .send()
        .await
        .map_err(|error| format!("Could not authenticate to the remote instance: {error}"))?;
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 => "The saved instance credential was rejected".to_string(),
            429 => "Too many login attempts; wait before reconnecting".to_string(),
            _ => format!("Remote login failed with {}", response.status()),
        });
    }
    let encrypted_session: EncryptedDesktopSession = read_bounded_json(
        response,
        "The remote instance returned an invalid Desktop authentication response",
    )
    .await?;
    let mut ciphertext = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(encrypted_session.ciphertext)
            .map_err(|_| {
                "The remote instance returned an invalid Desktop authentication response"
                    .to_string()
            })?,
    );
    let response_key = LessSafeKey::new(
        UnboundKey::new(&CHACHA20_POLY1305, channel.response_key.as_ref())
            .map_err(|_| "Could not open the Desktop credential channel".to_string())?,
    );
    let aad = desktop_credential_aad(
        &bootstrap.instance_id,
        &channel.exchange_id_bytes,
        CREDENTIAL_RESPONSE_DIRECTION,
        purpose,
    );
    let plaintext = response_key
        .open_in_place(
            Nonce::assume_unique_for_key(CREDENTIAL_CHANNEL_NONCE),
            Aad::from(aad),
            ciphertext.as_mut(),
        )
        .map_err(|_| {
            "The remote instance returned an invalid Desktop authentication response".to_string()
        })?;
    let response: DesktopAuthResponse = serde_json::from_slice(plaintext).map_err(|_| {
        "The remote instance returned an invalid Desktop authentication response".to_string()
    })?;
    let instance_id = match &response {
        DesktopAuthResponse::Validated { instance_id }
        | DesktopAuthResponse::BrowserSession { instance_id, .. } => instance_id,
    };
    if instance_id != &bootstrap.instance_id {
        return Err(
            "The instance identity changed while Desktop was authenticating; no profile change was saved"
                .to_string(),
        );
    }
    Ok(response)
}

async fn validate_desktop_credential(
    url: &str,
    credential: &str,
    bootstrap: &Bootstrap,
) -> Result<(), String> {
    match request_desktop_auth(
        url,
        credential,
        bootstrap,
        DesktopCredentialPurpose::Validate,
    )
    .await?
    {
        DesktopAuthResponse::Validated { .. } => Ok(()),
        DesktopAuthResponse::BrowserSession { .. } => {
            Err("The remote instance returned an unexpected browser session".to_string())
        }
    }
}

async fn request_desktop_session(
    state: &ProfileState,
    profile: &StoredProfile,
    credential: &str,
    bootstrap: &Bootstrap,
) -> Result<DesktopBrowserSession, String> {
    require_listener_bound_local_session_origin(state, profile, bootstrap)?;
    match request_desktop_auth(
        &profile.url,
        credential,
        bootstrap,
        DesktopCredentialPurpose::BrowserSession,
    )
    .await?
    {
        DesktopAuthResponse::BrowserSession {
            instance_id: _,
            session,
        } => Ok(DesktopBrowserSession {
            session: Zeroizing::new(session),
        }),
        DesktopAuthResponse::Validated { .. } => {
            Err("The remote instance did not create a browser session".to_string())
        }
    }
}

fn require_listener_bound_local_session_origin(
    state: &ProfileState,
    profile: &StoredProfile,
    bootstrap: &Bootstrap,
) -> Result<(), String> {
    if profile.local
        && profile.id == LOCAL_PROFILE_ID
        && state.local_bound_identity()?.as_deref() == Some(bootstrap.identity_public_key.as_str())
    {
        return Ok(());
    }
    Err(if profile.local {
        "Desktop automatic login is unavailable because this HTTP local instance was not started by the current Desktop process. Enter the credential manually, or restart the local sidecar from Desktop."
            .to_string()
    } else {
        "Desktop automatic login is available only for its managed local instance. Enter the credential manually for this remote profile; use HTTPS or a trusted tailnet for transport security."
            .to_string()
    })
}

fn desktop_session_cookie<'a>(
    url: &'a reqwest::Url,
    session: &'a str,
) -> Result<Cookie<'a>, String> {
    let domain = url
        .host_str()
        .ok_or_else(|| "The selected Desktop profile URL has no host".to_string())?;
    Ok(Cookie::build((BROWSER_SESSION_COOKIE, session))
        .domain(domain)
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(url.scheme() == "https")
        .max_age(CookieDuration::seconds(
            BROWSER_SESSION_LIFETIME_SECONDS as i64,
        ))
        .build())
}

fn install_browser_session_cookie(
    webview: &tauri::WebviewWindow,
    profile_url: &str,
    session: &DesktopBrowserSession,
) -> Result<(), String> {
    let url = reqwest::Url::parse(profile_url)
        .map_err(|error| format!("The selected Desktop profile URL is invalid: {error}"))?;
    let cookie = desktop_session_cookie(&url, &session.session)?;
    webview
        .set_cookie(cookie)
        .map_err(|error| format!("Could not install the Desktop browser session: {error}"))
}

async fn read_bounded_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    invalid_message: &str,
) -> Result<T, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_AUTH_RESPONSE_BYTES as u64)
    {
        return Err(format!("{invalid_message}: response is too large"));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| invalid_message.to_string())?;
        if body.len().saturating_add(chunk.len()) > MAX_AUTH_RESPONSE_BYTES {
            return Err(format!("{invalid_message}: response is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| invalid_message.to_string())
}

pub(crate) fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        // A malicious or misconfigured profile endpoint must not redirect a
        // credential-bearing ticket request to another origin.
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| error.to_string())
}

fn normalize_url(raw: &str) -> Result<String, String> {
    if raw.trim().len() > 2048 {
        return Err("Instance URL is too long".to_string());
    }
    let mut url = reqwest::Url::parse(raw.trim()).map_err(|_| "Enter a valid instance URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Instance profiles support only http:// and https:// URLs".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Do not put credentials in an instance URL".to_string());
    }
    if url.host_str().is_none() {
        return Err("Instance URL must include a host".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Instance URL cannot include a query or fragment".to_string());
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err("Instance URL must be an origin without a path".to_string());
    }
    url.set_path("");
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn credential_entry(profile_id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, profile_id)
        .map_err(|error| format!("Could not access the operating-system keychain: {error}"))
}

fn set_credential(profile_id: &str, credential: &str) -> Result<(), String> {
    credential_entry(profile_id)?
        .set_password(credential)
        .map_err(|error| format!("Could not save the profile credential in the keychain: {error}"))
}

fn get_credential(profile_id: &str) -> Result<Zeroizing<String>, String> {
    get_optional_credential(profile_id)?
        .ok_or_else(|| "No credential is saved for this Desktop profile".to_string())
}

fn get_optional_credential(profile_id: &str) -> Result<Option<Zeroizing<String>>, String> {
    match credential_entry(profile_id)?.get_password() {
        Ok(credential) => Ok(Some(Zeroizing::new(credential))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!(
            "Could not read the profile credential from the keychain: {error}"
        )),
    }
}

fn delete_credential(profile_id: &str) -> Result<(), String> {
    let entry = credential_entry(profile_id)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("Could not remove the profile credential: {error}")),
    }
}

fn persist_file(path: &Path, file: &ProfileFile) -> Result<(), String> {
    let directory = path
        .parent()
        .ok_or_else(|| "Profile file has no parent directory".to_string())?;
    let mut temporary = NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(&mut temporary, file).map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| format!("Could not replace Desktop profile file: {}", error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::io::{Read, Write};

    struct TestIdentity {
        instance_id: String,
        key_pair: Ed25519KeyPair,
    }

    struct TestCredentialChannel {
        exchange_id: String,
        exchange_id_bytes: [u8; 32],
        request_key: Zeroizing<[u8; 32]>,
        response_key: Zeroizing<[u8; 32]>,
    }

    impl TestIdentity {
        fn new(instance_id: impl Into<String>) -> Self {
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
            Self {
                instance_id: instance_id.into(),
                key_pair: Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap(),
            }
        }

        fn public_key(&self) -> String {
            URL_SAFE_NO_PAD.encode(self.key_pair.public_key().as_ref())
        }

        fn bootstrap(&self, authentication_enabled: bool, credential_kind: &str) -> Bootstrap {
            Bootstrap {
                instance_id: self.instance_id.clone(),
                identity_public_key: self.public_key(),
                authentication_enabled,
                credential_kind: credential_kind.to_string(),
            }
        }

        fn bootstrap_json(&self, authentication_enabled: bool, credential_kind: &str) -> String {
            serde_json::json!({
                "instance_id": self.instance_id,
                "identity_public_key": self.public_key(),
                "authentication_enabled": authentication_enabled,
                "credential_kind": credential_kind,
            })
            .to_string()
        }

        fn proof_json(&self, request: &str) -> String {
            let (_, body) = request
                .split_once("\r\n\r\n")
                .expect("identity proof request has a body");
            let request: serde_json::Value = serde_json::from_str(body).unwrap();
            let challenge = URL_SAFE_NO_PAD
                .decode(request["challenge"].as_str().unwrap())
                .unwrap();
            serde_json::json!({
                "instance_id": self.instance_id,
                "identity_public_key": self.public_key(),
                "signature": URL_SAFE_NO_PAD.encode(
                    self.key_pair
                        .sign(&identity_proof_message(&self.instance_id, &challenge))
                        .as_ref(),
                ),
            })
            .to_string()
        }

        fn credential_proof_json(&self, request: &str) -> (String, TestCredentialChannel) {
            let (_, body) = request
                .split_once("\r\n\r\n")
                .expect("credential proof request has a body");
            let request: serde_json::Value = serde_json::from_str(body).unwrap();
            let challenge: [u8; 32] = URL_SAFE_NO_PAD
                .decode(request["challenge"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            let client_public_key: [u8; 32] = URL_SAFE_NO_PAD
                .decode(request["client_public_key"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            let rng = SystemRandom::new();
            let server_private = EphemeralPrivateKey::generate(&X25519, &rng).unwrap();
            let server_public_key: [u8; 32] = server_private
                .compute_public_key()
                .unwrap()
                .as_ref()
                .try_into()
                .unwrap();
            let mut exchange_id_bytes = [0u8; 32];
            rng.fill(&mut exchange_id_bytes).unwrap();
            let peer = AgreementPublicKey::new(&X25519, client_public_key);
            let keys = agree_ephemeral(server_private, &peer, |shared| {
                derive_desktop_credential_keys(
                    shared,
                    &self.instance_id,
                    &challenge,
                    &exchange_id_bytes,
                    &client_public_key,
                    &server_public_key,
                )
            })
            .unwrap()
            .unwrap();
            let signature = self.key_pair.sign(&desktop_credential_proof_message(
                &self.instance_id,
                &challenge,
                &exchange_id_bytes,
                &client_public_key,
                &server_public_key,
            ));
            let exchange_id = URL_SAFE_NO_PAD.encode(exchange_id_bytes);
            (
                serde_json::json!({
                    "instance_id": self.instance_id,
                    "identity_public_key": self.public_key(),
                    "exchange_id": exchange_id,
                    "server_public_key": URL_SAFE_NO_PAD.encode(server_public_key),
                    "signature": URL_SAFE_NO_PAD.encode(signature.as_ref()),
                })
                .to_string(),
                TestCredentialChannel {
                    exchange_id,
                    exchange_id_bytes,
                    request_key: keys.request,
                    response_key: keys.response,
                },
            )
        }
    }

    impl TestCredentialChannel {
        fn open_credential(
            &self,
            request: &str,
            instance_id: &str,
            expected_purpose: DesktopCredentialPurpose,
        ) -> Zeroizing<String> {
            let (_, body) = request
                .split_once("\r\n\r\n")
                .expect("credential request has a body");
            let request: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(request["exchange_id"], self.exchange_id);
            assert!(request.get("credential").is_none());
            let purpose: DesktopCredentialPurpose =
                serde_json::from_value(request["purpose"].clone()).unwrap();
            assert_eq!(purpose, expected_purpose);
            let mut ciphertext = Zeroizing::new(
                URL_SAFE_NO_PAD
                    .decode(request["ciphertext"].as_str().unwrap())
                    .unwrap(),
            );
            let key = LessSafeKey::new(
                UnboundKey::new(&CHACHA20_POLY1305, self.request_key.as_ref()).unwrap(),
            );
            let aad = desktop_credential_aad(
                instance_id,
                &self.exchange_id_bytes,
                CREDENTIAL_REQUEST_DIRECTION,
                purpose,
            );
            let plaintext = key
                .open_in_place(
                    Nonce::assume_unique_for_key(CREDENTIAL_CHANNEL_NONCE),
                    Aad::from(aad),
                    ciphertext.as_mut(),
                )
                .unwrap();
            Zeroizing::new(std::str::from_utf8(plaintext).unwrap().to_string())
        }

        fn encrypted_response_json(
            &self,
            response: serde_json::Value,
            instance_id: &str,
            purpose: DesktopCredentialPurpose,
        ) -> String {
            let mut plaintext = serde_json::to_vec(&response).unwrap();
            let key = LessSafeKey::new(
                UnboundKey::new(&CHACHA20_POLY1305, self.response_key.as_ref()).unwrap(),
            );
            let aad = desktop_credential_aad(
                instance_id,
                &self.exchange_id_bytes,
                CREDENTIAL_RESPONSE_DIRECTION,
                purpose,
            );
            key.seal_in_place_append_tag(
                Nonce::assume_unique_for_key(CREDENTIAL_CHANNEL_NONCE),
                Aad::from(aad),
                &mut plaintext,
            )
            .unwrap();
            serde_json::json!({ "ciphertext": URL_SAFE_NO_PAD.encode(plaintext) }).to_string()
        }
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_millis(750)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 4096];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "HTTP client closed an incomplete request");
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let header_end = header_end + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if request.len() >= header_end + content_length {
                request.truncate(header_end + content_length);
                return String::from_utf8(request).unwrap();
            }
        }
    }

    fn respond_json(stream: &mut std::net::TcpStream, body: &str) {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    fn serve_bootstrap_and_proof(
        listener: std::net::TcpListener,
        identity: TestIdentity,
        authentication_enabled: bool,
        credential_kind: &str,
    ) {
        let (mut bootstrap_stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut bootstrap_stream);
        assert!(request.starts_with("GET /api/auth/bootstrap HTTP/1.1"));
        respond_json(
            &mut bootstrap_stream,
            &identity.bootstrap_json(authentication_enabled, credential_kind),
        );

        let (mut proof_stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut proof_stream);
        assert!(request.starts_with("POST /api/auth/identity-proof HTTP/1.1"));
        respond_json(&mut proof_stream, &identity.proof_json(&request));
    }

    #[test]
    fn profile_urls_are_origin_only_and_never_contain_credentials() {
        assert_eq!(
            normalize_url("https://host.example:9443/").unwrap(),
            "https://host.example:9443"
        );
        assert!(normalize_url("https://user:secret@host.example").is_err());
        assert!(normalize_url("https://host.example/path").is_err());
        assert!(normalize_url("file:///tmp/xpressclaw").is_err());
    }

    #[test]
    fn native_browser_session_cookie_matches_the_server_policy() {
        let http = reqwest::Url::parse("http://127.0.0.1:8935").unwrap();
        let cookie = desktop_session_cookie(&http, "native-only-session").unwrap();
        assert_eq!(cookie.name(), BROWSER_SESSION_COOKIE);
        assert_eq!(cookie.value(), "native-only-session");
        assert_eq!(cookie.domain(), Some("127.0.0.1"));
        assert_eq!(cookie.path(), Some("/"));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Strict));
        assert_eq!(cookie.secure(), Some(false));
        assert_eq!(
            cookie.max_age().unwrap().whole_seconds(),
            BROWSER_SESSION_LIFETIME_SECONDS as i64
        );

        let https = reqwest::Url::parse("https://control.example.test").unwrap();
        assert_eq!(
            desktop_session_cookie(&https, "native-only-session")
                .unwrap()
                .secure(),
            Some(true)
        );
    }

    #[test]
    fn persisted_profiles_contain_no_credential_field() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profiles.json");
        let public_identity = TestIdentity::new("identity").public_key();
        let file = ProfileFile {
            profiles: vec![StoredProfile {
                id: "remote".into(),
                name: "Remote".into(),
                url: "http://host:8935".into(),
                instance_id: Some("identity".into()),
                identity_public_key: Some(public_identity.clone()),
                authentication: "password".into(),
                local: false,
                confirmed_unauthenticated_remote: false,
            }],
            ..ProfileFile::default()
        };
        persist_file(&path, &file).unwrap();
        let stored = std::fs::read_to_string(path).unwrap();
        assert!(!stored.contains("credential"));
        assert!(!stored.contains("password_hash"));
        assert!(!stored.contains("private_key"));
        assert!(stored.contains(&public_identity));
    }

    #[test]
    fn profile_selection_is_persisted_atomically() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profiles.json");
        let file = ProfileFile {
            active_profile_id: "remote".into(),
            profiles: vec![
                StoredProfile {
                    id: LOCAL_PROFILE_ID.into(),
                    name: "Local XpressClaw".into(),
                    url: "http://localhost:8935".into(),
                    instance_id: Some("local-instance".into()),
                    identity_public_key: None,
                    authentication: "none".into(),
                    local: true,
                    confirmed_unauthenticated_remote: true,
                },
                StoredProfile {
                    id: "remote".into(),
                    name: "Remote".into(),
                    url: "https://remote.example".into(),
                    instance_id: Some("remote-instance".into()),
                    identity_public_key: None,
                    authentication: "password".into(),
                    local: false,
                    confirmed_unauthenticated_remote: false,
                },
            ],
            ..ProfileFile::default()
        };
        persist_file(&path, &file).unwrap();
        let state = ProfileState {
            path: path.clone(),
            file: Mutex::new(file),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };

        assert_eq!(state.select_local().unwrap(), "http://localhost:8935");
        assert_eq!(state.active().unwrap().id, LOCAL_PROFILE_ID);
        let persisted: ProfileFile = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted.active_profile_id, LOCAL_PROFILE_ID);
    }

    #[test]
    fn remote_selection_can_enter_credential_free_local_recovery() {
        let local_identity = TestIdentity::new("trusted-local");
        let replacement = TestIdentity::new("replacement-local").bootstrap(false, "disabled");
        let local = StoredProfile {
            id: LOCAL_PROFILE_ID.into(),
            name: "Local XpressClaw".into(),
            url: "http://localhost:8935".into(),
            instance_id: Some("trusted-local".into()),
            identity_public_key: Some(local_identity.public_key()),
            authentication: "password".into(),
            local: true,
            confirmed_unauthenticated_remote: true,
        };
        assert!(local_profile_requires_recovery(&local, &replacement, false));

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profiles.json");
        let state = ProfileState {
            path: path.clone(),
            file: Mutex::new(ProfileFile {
                active_profile_id: "remote".into(),
                profiles: vec![
                    local,
                    StoredProfile {
                        id: "remote".into(),
                        name: "Remote".into(),
                        url: "https://remote.example".into(),
                        instance_id: Some("remote-instance".into()),
                        identity_public_key: Some(
                            TestIdentity::new("remote-instance").public_key(),
                        ),
                        authentication: "password".into(),
                        local: false,
                        confirmed_unauthenticated_remote: false,
                    },
                ],
                ..ProfileFile::default()
            }),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };

        assert_eq!(state.select_local().unwrap(), "http://localhost:8935");
        assert_eq!(state.active().unwrap().id, LOCAL_PROFILE_ID);
        let persisted: ProfileFile = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted.active_profile_id, LOCAL_PROFILE_ID);
    }

    #[test]
    fn bootstrap_mode_must_be_supported_and_exact() {
        let bootstrap = |enabled, kind: &str| Bootstrap {
            instance_id: "instance".into(),
            identity_public_key: TestIdentity::new("instance").public_key(),
            authentication_enabled: enabled,
            credential_kind: kind.into(),
        };
        assert_eq!(
            effective_authentication(&bootstrap(false, "disabled")).unwrap(),
            "none"
        );
        assert_eq!(
            effective_authentication(&bootstrap(true, "password")).unwrap(),
            "password"
        );
        assert_eq!(
            effective_authentication(&bootstrap(true, "startup_token")).unwrap(),
            "startup_token"
        );
        assert!(effective_authentication(&bootstrap(true, "restart_required")).is_err());
        assert!(effective_authentication(&bootstrap(true, "future_mode")).is_err());
    }

    #[test]
    fn authenticated_remote_navigation_defers_credentials_to_browser_login() {
        let identity = TestIdentity::new("remote-instance");
        let mut profile = StoredProfile {
            id: "remote".into(),
            name: "Remote".into(),
            url: "https://remote.example".into(),
            instance_id: Some("remote-instance".into()),
            identity_public_key: Some(identity.public_key()),
            authentication: "password".into(),
            local: false,
            confirmed_unauthenticated_remote: false,
        };

        // A restart can rotate a startup token or change the authenticated
        // mode. Neither condition should prevent the proved origin from
        // showing the browser form that collects the current credential.
        assert!(validate_remote_profile_navigation(
            &profile,
            &identity.bootstrap(true, "startup_token")
        )
        .is_ok());

        let no_auth = identity.bootstrap(false, "disabled");
        assert!(validate_remote_profile_navigation(&profile, &no_auth).is_err());
        assert_eq!(
            active_profile_navigation_status(&profile, &no_auth, "matched"),
            "confirmation_required"
        );
        profile.authentication = "none".into();
        assert_eq!(
            active_profile_navigation_status(&profile, &no_auth, "matched"),
            "confirmation_required"
        );
        profile.confirmed_unauthenticated_remote = true;
        assert!(validate_remote_profile_navigation(&profile, &no_auth).is_ok());
        assert_eq!(
            active_profile_navigation_status(&profile, &no_auth, "matched"),
            "ready"
        );
        assert_eq!(
            active_profile_navigation_status(&profile, &no_auth, "unpinned"),
            "profile_review_required"
        );
    }

    #[tokio::test]
    async fn signed_instance_identity_accepts_a_fresh_challenge() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let identity = TestIdentity::new("signed-instance");
        let expected_public_key = identity.public_key();
        let server = std::thread::spawn(move || {
            serve_bootstrap_and_proof(listener, identity, false, "disabled")
        });

        let bootstrap =
            fetch_verified_bootstrap(&format!("http://{address}"), Some(&expected_public_key))
                .await
                .unwrap();
        server.join().unwrap();
        assert_eq!(bootstrap.instance_id, "signed-instance");
    }

    #[tokio::test]
    async fn recorded_identity_proof_cannot_be_replayed_for_a_new_challenge() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let identity = TestIdentity::new("signed-instance");
        let expected_public_key = identity.public_key();
        let proof_public_key = expected_public_key.clone();
        let bootstrap = identity.bootstrap_json(false, "disabled");
        let recorded_signature = URL_SAFE_NO_PAD.encode(
            identity
                .key_pair
                .sign(&identity_proof_message("signed-instance", &[7_u8; 32]))
                .as_ref(),
        );
        let server = std::thread::spawn(move || {
            let (mut bootstrap_stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut bootstrap_stream);
            assert!(request.starts_with("GET /api/auth/bootstrap HTTP/1.1"));
            respond_json(&mut bootstrap_stream, &bootstrap);

            let (mut proof_stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut proof_stream);
            assert!(request.starts_with("POST /api/auth/identity-proof HTTP/1.1"));
            respond_json(
                &mut proof_stream,
                &serde_json::json!({
                    "instance_id": "signed-instance",
                    "identity_public_key": proof_public_key,
                    "signature": recorded_signature,
                })
                .to_string(),
            );
        });

        let error =
            fetch_verified_bootstrap(&format!("http://{address}"), Some(&expected_public_key))
                .await
                .unwrap_err();
        server.join().unwrap();
        assert!(error.contains("could not prove its saved identity"));
    }

    #[tokio::test]
    async fn startup_falls_back_before_using_a_replayed_remote_identity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let identity = TestIdentity::new("recorded-remote");
        let expected_public_key = identity.public_key();
        let proof_public_key = expected_public_key.clone();
        let bootstrap = identity.bootstrap_json(false, "disabled");
        let recorded_signature = URL_SAFE_NO_PAD.encode(
            identity
                .key_pair
                .sign(&identity_proof_message("recorded-remote", &[9_u8; 32]))
                .as_ref(),
        );
        let server = std::thread::spawn(move || {
            let (mut bootstrap_stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut bootstrap_stream);
            assert!(request.starts_with("GET /api/auth/bootstrap HTTP/1.1"));
            respond_json(&mut bootstrap_stream, &bootstrap);

            let (mut proof_stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut proof_stream);
            assert!(request.starts_with("POST /api/auth/identity-proof HTTP/1.1"));
            respond_json(
                &mut proof_stream,
                &serde_json::json!({
                    "instance_id": "recorded-remote",
                    "identity_public_key": proof_public_key,
                    "signature": recorded_signature,
                })
                .to_string(),
            );
        });

        let root = tempfile::tempdir().unwrap();
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile {
                active_profile_id: "remote".into(),
                profiles: vec![
                    StoredProfile {
                        id: LOCAL_PROFILE_ID.into(),
                        name: "Local XpressClaw".into(),
                        url: "http://localhost:8935".into(),
                        instance_id: None,
                        identity_public_key: None,
                        authentication: "none".into(),
                        local: true,
                        confirmed_unauthenticated_remote: true,
                    },
                    StoredProfile {
                        id: "remote".into(),
                        name: "Remote".into(),
                        url: format!("http://{address}"),
                        instance_id: Some("recorded-remote".into()),
                        identity_public_key: Some(expected_public_key),
                        authentication: "none".into(),
                        local: false,
                        confirmed_unauthenticated_remote: true,
                    },
                ],
                ..ProfileFile::default()
            }),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };

        assert_eq!(preferred_startup_url(&state).await, "http://localhost:8935");
        server.join().unwrap();
        assert!(state.active().unwrap().local);
    }

    #[tokio::test]
    async fn startup_opens_a_proved_remote_before_validating_its_rotating_token() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let remote_url = format!("http://{address}");
        let identity = TestIdentity::new("rotating-token-remote");
        let expected_public_key = identity.public_key();
        let server = std::thread::spawn(move || {
            // Only bootstrap and identity proof are served. A native
            // credential-validation request would fail this regression.
            serve_bootstrap_and_proof(listener, identity, true, "startup_token")
        });

        let root = tempfile::tempdir().unwrap();
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile {
                active_profile_id: "remote".into(),
                profiles: vec![
                    StoredProfile {
                        id: LOCAL_PROFILE_ID.into(),
                        name: "Local XpressClaw".into(),
                        url: "http://localhost:8935".into(),
                        instance_id: None,
                        identity_public_key: None,
                        authentication: "none".into(),
                        local: true,
                        confirmed_unauthenticated_remote: true,
                    },
                    StoredProfile {
                        id: "remote".into(),
                        name: "Remote".into(),
                        url: remote_url.clone(),
                        instance_id: Some("rotating-token-remote".into()),
                        identity_public_key: Some(expected_public_key),
                        authentication: "startup_token".into(),
                        local: false,
                        confirmed_unauthenticated_remote: false,
                    },
                ],
                ..ProfileFile::default()
            }),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };

        assert_eq!(preferred_startup_url(&state).await, remote_url);
        server.join().unwrap();
        assert_eq!(state.active().unwrap().id, "remote");
    }

    #[tokio::test]
    async fn saved_local_pin_authenticates_an_already_running_sidecar() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let identity = TestIdentity::new("managed-local");
        let expected_public_key = identity.public_key();
        let server = std::thread::spawn(move || {
            serve_bootstrap_and_proof(listener, identity, false, "disabled")
        });
        let root = tempfile::tempdir().unwrap();
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile {
                profiles: vec![StoredProfile {
                    id: LOCAL_PROFILE_ID.into(),
                    name: "Local XpressClaw".into(),
                    url: format!("http://{address}"),
                    instance_id: Some("managed-local".into()),
                    identity_public_key: Some(expected_public_key),
                    authentication: "none".into(),
                    local: true,
                    confirmed_unauthenticated_remote: true,
                }],
                ..ProfileFile::default()
            }),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };

        assert_eq!(
            verify_managed_local_instance(&state).await.unwrap(),
            format!("http://{address}")
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn first_local_pairing_requires_the_listener_bound_child_identity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let identity = TestIdentity::new("first-managed-local");
        let expected_public_key = identity.public_key();
        let server = std::thread::spawn(move || {
            serve_bootstrap_and_proof(listener, identity, false, "disabled")
        });
        let root = tempfile::tempdir().unwrap();
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile {
                profiles: vec![StoredProfile {
                    id: LOCAL_PROFILE_ID.into(),
                    name: "Local XpressClaw".into(),
                    url: format!("http://{address}"),
                    instance_id: None,
                    identity_public_key: None,
                    authentication: "none".into(),
                    local: true,
                    confirmed_unauthenticated_remote: true,
                }],
                ..ProfileFile::default()
            }),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };
        state
            .remember_local_bound_identity(&expected_public_key)
            .unwrap();

        verify_managed_local_instance(&state).await.unwrap();
        server.join().unwrap();
        let local = state.active().unwrap();
        assert_eq!(local.instance_id.as_deref(), Some("first-managed-local"));
        assert_eq!(
            local.identity_public_key.as_deref(),
            Some(expected_public_key.as_str())
        );
    }

    #[test]
    fn pending_password_storage_requires_the_same_instance_and_complete_configuration() {
        let settings = PendingInstanceSettings {
            instance_id: "instance".into(),
            saved: PendingListenerSettings {
                authentication_enabled: true,
            },
            password_configured: true,
        };
        validate_pending_password_configuration(&settings, "instance").unwrap();
        assert!(
            validate_pending_password_configuration(&settings, "replacement")
                .unwrap_err()
                .contains("identity changed")
        );

        let without_password = PendingInstanceSettings {
            password_configured: false,
            ..settings
        };
        assert_eq!(
            validate_pending_password_configuration(&without_password, "instance").unwrap_err(),
            "The selected instance does not require a credential"
        );
    }

    #[tokio::test]
    async fn pending_password_storage_reads_the_authoritative_saved_settings() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read])
                .starts_with("GET /api/settings/instance/ HTTP/1.1"));
            let body = r#"{"instance_id":"pending-instance","saved":{"authentication_enabled":true},"password_configured":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });

        require_pending_password_configuration(&format!("http://{address}"), "pending-instance")
            .await
            .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn saved_credentials_are_reused_only_for_the_exact_pinned_profile() {
        let identity = TestIdentity::new("instance-a");
        let bootstrap = identity.bootstrap(true, "password");
        let profile = StoredProfile {
            id: "remote".into(),
            name: "Remote".into(),
            url: "https://remote.example".into(),
            instance_id: Some("instance-a".into()),
            identity_public_key: Some(identity.public_key()),
            authentication: "password".into(),
            local: false,
            confirmed_unauthenticated_remote: false,
        };
        assert!(may_reuse_stored_credential(
            &profile,
            "https://remote.example",
            "password",
            &bootstrap
        ));
        assert!(!may_reuse_stored_credential(
            &profile,
            "https://attacker.example",
            "password",
            &bootstrap
        ));
        assert!(!may_reuse_stored_credential(
            &profile,
            "https://remote.example",
            "startup_token",
            &bootstrap
        ));
        let replacement = TestIdentity::new("instance-b").bootstrap(true, "password");
        assert!(!may_reuse_stored_credential(
            &profile,
            "https://remote.example",
            "password",
            &replacement
        ));
    }

    #[test]
    fn active_profile_identity_reports_only_native_comparison_status() {
        let identity = TestIdentity::new("native-secret-pin");
        let bootstrap = identity.bootstrap(true, "password");
        let profile = StoredProfile {
            id: "remote".into(),
            name: "Remote".into(),
            url: "https://remote.example".into(),
            instance_id: Some("native-secret-pin".into()),
            identity_public_key: Some(identity.public_key()),
            authentication: "password".into(),
            local: false,
            confirmed_unauthenticated_remote: false,
        };
        assert!(profile_identity_matches(&profile, &bootstrap));
        assert!(!profile_identity_matches(
            &profile,
            &TestIdentity::new("replacement").bootstrap(true, "password")
        ));

        let response = serde_json::to_string(&ActiveProfileIdentity {
            identity_status: "changed",
            navigation_status: "ready",
            local: false,
        })
        .unwrap();
        assert_eq!(
            response,
            r#"{"identity_status":"changed","navigation_status":"ready","local":false}"#
        );
        assert!(!response.contains("native-secret-pin"));
        assert!(!response.contains("instance_id"));
    }

    #[test]
    fn local_identity_pin_cannot_be_replaced_by_passive_discovery() {
        let trusted = TestIdentity::new("trusted-local-instance");
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profiles.json");
        let file = ProfileFile {
            active_profile_id: LOCAL_PROFILE_ID.into(),
            profiles: vec![StoredProfile {
                id: LOCAL_PROFILE_ID.into(),
                name: "Local XpressClaw".into(),
                url: "http://localhost:8935".into(),
                instance_id: Some("trusted-local-instance".into()),
                identity_public_key: Some(trusted.public_key()),
                authentication: "password".into(),
                local: true,
                confirmed_unauthenticated_remote: true,
            }],
            ..ProfileFile::default()
        };
        persist_file(&path, &file).unwrap();
        let state = ProfileState {
            path: path.clone(),
            file: Mutex::new(file),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };
        let replacement =
            TestIdentity::new("replacement-local-instance").bootstrap(true, "startup_token");

        let error = state.set_local_bootstrap(&replacement).unwrap_err();
        assert!(error.contains("Explicitly trust the replacement"));
        let local = state.active().unwrap();
        assert_eq!(local.instance_id.as_deref(), Some("trusted-local-instance"));
        assert_eq!(local.authentication, "password");

        state
            .replace_local_bootstrap("trusted-local-instance", &replacement)
            .unwrap();
        let local = state.active().unwrap();
        assert_eq!(
            local.instance_id.as_deref(),
            Some("replacement-local-instance")
        );
        assert_eq!(local.authentication, "startup_token");
    }

    #[tokio::test]
    async fn unpinned_local_profile_never_reuses_a_keychain_credential() {
        let root = tempfile::tempdir().unwrap();
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile::default()),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };
        let profile = StoredProfile {
            id: LOCAL_PROFILE_ID.into(),
            name: "Local XpressClaw".into(),
            url: "http://localhost:8935".into(),
            instance_id: None,
            identity_public_key: None,
            authentication: "password".into(),
            local: true,
            confirmed_unauthenticated_remote: true,
        };

        let error = credential_for_authentication(&state, &profile, "password")
            .await
            .unwrap_err();
        assert!(error.contains("Enter the credential once"));
    }

    #[test]
    fn stale_profile_origins_cannot_reuse_desktop_commands() {
        let selected = "https://selected.example:9443";
        assert!(urls_have_same_origin(
            selected,
            &reqwest::Url::parse("https://selected.example:9443/settings/server").unwrap()
        ));
        assert!(!urls_have_same_origin(
            selected,
            &reqwest::Url::parse("https://previous.example:9443/settings/server").unwrap()
        ));
        assert!(!urls_have_same_origin(
            selected,
            &reqwest::Url::parse("http://selected.example:9443/settings/server").unwrap()
        ));
    }

    #[tokio::test]
    async fn credential_requests_never_follow_cross_origin_redirects() {
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};

        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let target_address = target.local_addr().unwrap();
        let target_thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(750);
            while Instant::now() < deadline {
                match target.accept() {
                    Ok(_) => return true,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("target listener failed: {error}"),
                }
            }
            false
        });

        let redirect = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_address = redirect.local_addr().unwrap();
        let redirect_thread = std::thread::spawn(move || {
            let (mut stream, _) = redirect.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_address}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });

        let bootstrap = TestIdentity::new("expected-instance").bootstrap(true, "password");
        let result = validate_desktop_credential(
            &format!("http://{redirect_address}"),
            "credential-that-must-not-be-forwarded",
            &bootstrap,
        )
        .await;
        assert!(result.is_err());
        redirect_thread.join().unwrap();
        assert!(!target_thread.join().unwrap());
    }

    #[tokio::test]
    async fn listener_bound_login_keeps_credential_and_session_native() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let identity = TestIdentity::new("genuine-instance");
        let bootstrap = identity.bootstrap(true, "password");
        let root = tempfile::tempdir().unwrap();
        let profile = StoredProfile {
            id: LOCAL_PROFILE_ID.to_string(),
            name: "Local XpressClaw".to_string(),
            url: format!("http://{address}"),
            instance_id: Some(bootstrap.instance_id.clone()),
            identity_public_key: Some(bootstrap.identity_public_key.clone()),
            authentication: "password".to_string(),
            local: true,
            confirmed_unauthenticated_remote: true,
        };
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile {
                active_profile_id: LOCAL_PROFILE_ID.to_string(),
                profiles: vec![profile.clone()],
                ..ProfileFile::default()
            }),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(Some(bootstrap.identity_public_key.clone())),
        };
        let server = std::thread::spawn(move || {
            // A listener-bound local endpoint can observe both HTTP bodies,
            // but only native Desktop and the server own the ephemeral keys.
            let (mut proof_stream, _) = listener.accept().unwrap();
            let proof_request = read_http_request(&mut proof_stream);
            let (proof, genuine_channel) = identity.credential_proof_json(&proof_request);
            respond_json(&mut proof_stream, &proof);

            let (mut session_stream, _) = listener.accept().unwrap();
            let session_request = read_http_request(&mut session_stream);
            assert!(session_request.starts_with("POST /api/auth/desktop-session HTTP/1.1"));
            assert!(!session_request.contains("saved-password-must-stay-secret"));
            let (_, body) = session_request.split_once("\r\n\r\n").unwrap();
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            assert!(body.get("credential").is_none());
            assert!(body.get("session").is_none());
            assert!(body["ciphertext"].as_str().is_some());

            // The genuine server behind the relay can decrypt and answer;
            // the relay itself only sees opaque request/response ciphertext.
            assert_eq!(
                genuine_channel
                    .open_credential(
                        &session_request,
                        "genuine-instance",
                        DesktopCredentialPurpose::BrowserSession,
                    )
                    .as_str(),
                "saved-password-must-stay-secret"
            );
            let response = genuine_channel.encrypted_response_json(
                serde_json::json!({
                    "kind": "browser_session",
                    "instance_id": "genuine-instance",
                    "session": "native-only-session-must-stay-secret",
                }),
                "genuine-instance",
                DesktopCredentialPurpose::BrowserSession,
            );
            assert!(!response.contains("native-only-session-must-stay-secret"));
            respond_json(&mut session_stream, &response);
        });

        let session = request_desktop_session(
            &state,
            &profile,
            "saved-password-must-stay-secret",
            &bootstrap,
        )
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(
            session.session.as_str(),
            "native-only-session-must-stay-secret"
        );
    }

    #[tokio::test]
    async fn remote_profiles_cannot_receive_a_native_browser_session() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let identity = TestIdentity::new("remote-instance");
        let bootstrap = identity.bootstrap(true, "password");
        let profile = StoredProfile {
            id: "remote".to_string(),
            name: "Remote".to_string(),
            url: format!("http://{address}"),
            instance_id: Some(bootstrap.instance_id.clone()),
            identity_public_key: Some(bootstrap.identity_public_key.clone()),
            authentication: "password".to_string(),
            local: false,
            confirmed_unauthenticated_remote: false,
        };
        let root = tempfile::tempdir().unwrap();
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile {
                active_profile_id: profile.id.clone(),
                profiles: vec![profile.clone()],
                ..ProfileFile::default()
            }),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };

        let error = match request_desktop_session(
            &state,
            &profile,
            "saved-password-must-stay-secret",
            &bootstrap,
        )
        .await
        {
            Ok(_) => panic!("a remote profile received a native browser session"),
            Err(error) => error,
        };
        assert!(error.contains("managed local instance"));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );

        let mut https_profile = profile.clone();
        https_profile.url = format!("https://{address}");
        let error = match request_desktop_session(
            &state,
            &https_profile,
            "saved-password-must-stay-secret",
            &bootstrap,
        )
        .await
        {
            Ok(_) => panic!("an HTTPS remote profile received a native browser session"),
            Err(error) => error,
        };
        assert!(error.contains("managed local instance"));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );

        let mut local_profile = profile;
        local_profile.local = true;
        local_profile.id = LOCAL_PROFILE_ID.to_string();
        assert!(
            require_listener_bound_local_session_origin(&state, &local_profile, &bootstrap)
                .is_err()
        );
        state
            .remember_local_bound_identity(&bootstrap.identity_public_key)
            .unwrap();
        assert!(
            require_listener_bound_local_session_origin(&state, &local_profile, &bootstrap).is_ok()
        );
    }

    #[tokio::test]
    async fn profile_inspection_never_submits_a_saved_credential() {
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex as StdMutex};
        use std::time::{Duration, Instant};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let trusted_identity = TestIdentity::new("instance");
        let replacement_identity = TestIdentity::new("replacement-instance");
        let replacement_bootstrap = replacement_identity.bootstrap_json(true, "startup_token");
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let observed = requests.clone();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(400);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(250)))
                            .unwrap();
                        let mut request = [0_u8; 4096];
                        let read = stream.read(&mut request).unwrap();
                        let request = String::from_utf8_lossy(&request[..read]);
                        observed
                            .lock()
                            .unwrap()
                            .push(request.lines().next().unwrap_or_default().to_string());
                        let body = &replacement_bootstrap;
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("profile test listener failed: {error}"),
                }
            }
        });

        let root = tempfile::tempdir().unwrap();
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile::default()),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(Some(Zeroizing::new(
                "saved-token-that-may-have-gone-stale".to_string(),
            ))),
            local_bound_identity: Mutex::new(None),
        };
        let profile = StoredProfile {
            id: LOCAL_PROFILE_ID.to_string(),
            name: "Local XpressClaw".to_string(),
            url: format!("http://{address}"),
            instance_id: Some("instance".to_string()),
            identity_public_key: Some(trusted_identity.public_key()),
            authentication: "startup_token".to_string(),
            local: true,
            confirmed_unauthenticated_remote: true,
        };

        let (health, _) = inspect_profile(&state, &profile).await;
        assert_eq!(health, "identity_changed");
        server.join().unwrap();
        assert_eq!(
            requests.lock().unwrap().as_slice(),
            ["GET /api/auth/bootstrap HTTP/1.1"]
        );
    }

    #[tokio::test]
    async fn pinned_no_auth_profile_rejects_a_same_url_replacement() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let trusted_identity = TestIdentity::new("trusted-instance");
        let replacement_identity = TestIdentity::new("replacement-instance");
        let replacement_bootstrap = replacement_identity.bootstrap_json(false, "disabled");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("GET /api/auth/bootstrap HTTP/1.1"));
            respond_json(&mut stream, &replacement_bootstrap);
        });
        let profile = StoredProfile {
            id: "remote".to_string(),
            name: "Remote".to_string(),
            url: format!("http://{address}"),
            instance_id: Some("trusted-instance".to_string()),
            identity_public_key: Some(trusted_identity.public_key()),
            authentication: "none".to_string(),
            local: false,
            confirmed_unauthenticated_remote: true,
        };

        let error =
            fetch_matching_bootstrap(&profile, profile.identity_public_key.as_deref().unwrap())
                .await
                .unwrap_err();
        server.join().unwrap();
        assert!(error.contains("identity key changed"));
    }

    #[tokio::test]
    async fn trusting_a_replacement_preserves_its_verified_sidecar_token() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let identity = TestIdentity::new("replacement-instance");
        let bootstrap = identity.bootstrap(true, "startup_token");
        let server = std::thread::spawn(move || {
            let (mut proof_stream, _) = listener.accept().unwrap();
            let proof_request = read_http_request(&mut proof_stream);
            assert!(proof_request.starts_with("POST /api/auth/identity-proof HTTP/1.1"));
            let (proof, channel) = identity.credential_proof_json(&proof_request);
            respond_json(&mut proof_stream, &proof);

            let (mut validation_stream, _) = listener.accept().unwrap();
            let validation_request = read_http_request(&mut validation_stream);
            assert!(validation_request.starts_with("POST /api/auth/desktop-session HTTP/1.1"));
            assert!(!validation_request.contains("fresh-sidecar-token"));
            assert_eq!(
                channel
                    .open_credential(
                        &validation_request,
                        "replacement-instance",
                        DesktopCredentialPurpose::Validate,
                    )
                    .as_str(),
                "fresh-sidecar-token"
            );
            respond_json(
                &mut validation_stream,
                &channel.encrypted_response_json(
                    serde_json::json!({
                        "kind": "validated",
                        "instance_id": "replacement-instance",
                    }),
                    "replacement-instance",
                    DesktopCredentialPurpose::Validate,
                ),
            );
        });

        let root = tempfile::tempdir().unwrap();
        let profile = StoredProfile {
            id: LOCAL_PROFILE_ID.to_string(),
            name: "Local XpressClaw".to_string(),
            url: format!("http://{address}"),
            instance_id: Some("previous-instance".to_string()),
            identity_public_key: Some(TestIdentity::new("previous-instance").public_key()),
            authentication: "startup_token".to_string(),
            local: true,
            confirmed_unauthenticated_remote: true,
        };
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile {
                active_profile_id: LOCAL_PROFILE_ID.to_string(),
                profiles: vec![profile.clone()],
                ..ProfileFile::default()
            }),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(Some(Zeroizing::new(
                "fresh-sidecar-token".to_string(),
            ))),
            local_bound_identity: Mutex::new(None),
        };
        assert!(
            preserve_verified_replacement_token_or_forget(&state, &profile, &bootstrap)
                .await
                .unwrap()
        );
        state
            .replace_local_bootstrap("previous-instance", &bootstrap)
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            profile_credential(&state, &state.active().unwrap())
                .unwrap()
                .as_str(),
            "fresh-sidecar-token"
        );
        assert_eq!(
            state.active().unwrap().instance_id.as_deref(),
            Some("replacement-instance")
        );
    }

    #[tokio::test]
    async fn profile_inspections_are_concurrent_but_bounded_and_keep_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let active_requests = Arc::new(AtomicUsize::new(0));
        let maximum_requests = Arc::new(AtomicUsize::new(0));
        let mut profiles = Vec::new();
        let mut servers = Vec::new();

        for index in 0..(PROFILE_INSPECTION_CONCURRENCY + 2) {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let identity = TestIdentity::new(format!("instance-{index}"));
            let identity_public_key = identity.public_key();
            let active_requests = active_requests.clone();
            let maximum_requests = maximum_requests.clone();
            servers.push(std::thread::spawn(move || {
                let accept_deadline = Instant::now() + Duration::from_secs(3);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < accept_deadline,
                                "profile inspection never reached test endpoint {index}"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("profile listener failed: {error}"),
                    }
                };
                let request = read_http_request(&mut stream);
                assert!(request.starts_with("GET /api/auth/bootstrap HTTP/1.1"));

                let current = active_requests.fetch_add(1, Ordering::SeqCst) + 1;
                maximum_requests.fetch_max(current, Ordering::SeqCst);
                let overlap_deadline = Instant::now() + Duration::from_millis(500);
                while maximum_requests.load(Ordering::SeqCst) < 2
                    && Instant::now() < overlap_deadline
                {
                    std::thread::sleep(Duration::from_millis(5));
                }

                respond_json(&mut stream, &identity.bootstrap_json(false, "disabled"));
                active_requests.fetch_sub(1, Ordering::SeqCst);

                listener.set_nonblocking(false).unwrap();
                let (mut proof_stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut proof_stream);
                assert!(request.starts_with("POST /api/auth/identity-proof HTTP/1.1"));
                respond_json(&mut proof_stream, &identity.proof_json(&request));
            }));
            profiles.push(StoredProfile {
                id: format!("profile-{index}"),
                name: format!("Profile {index}"),
                url: format!("http://{address}"),
                instance_id: Some(format!("instance-{index}")),
                identity_public_key: Some(identity_public_key),
                authentication: "none".to_string(),
                local: false,
                confirmed_unauthenticated_remote: true,
            });
        }

        let root = tempfile::tempdir().unwrap();
        let state = ProfileState {
            path: root.path().join("profiles.json"),
            file: Mutex::new(ProfileFile::default()),
            mutation_lock: tokio::sync::Mutex::new(()),
            local_ephemeral_credential: Mutex::new(None),
            local_bound_identity: Mutex::new(None),
        };
        let inspected = inspect_instance_profiles(&state, profiles, "profile-3")
            .await
            .unwrap();
        for server in servers {
            server.join().unwrap();
        }

        assert!(maximum_requests.load(Ordering::SeqCst) >= 2);
        assert!(maximum_requests.load(Ordering::SeqCst) <= PROFILE_INSPECTION_CONCURRENCY);
        assert_eq!(
            inspected
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
            (0..(PROFILE_INSPECTION_CONCURRENCY + 2))
                .map(|index| format!("profile-{index}"))
                .collect::<Vec<_>>()
        );
        assert!(inspected[3].active);
        assert!(inspected.iter().all(|profile| profile.health == "healthy"));
    }
}
