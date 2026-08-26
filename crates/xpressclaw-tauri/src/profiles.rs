use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tempfile::NamedTempFile;
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
    instance_id: Option<String>,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct DesktopTicket {
    ticket: String,
    instance_id: String,
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
            require_profile_identity(local, &bootstrap.instance_id)?;
            local.instance_id = Some(bootstrap.instance_id.clone());
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
            if profile_identity_matches(&profile, &bootstrap.instance_id) {
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
    if existing
        .as_ref()
        .is_some_and(|profile| !profile_identity_matches(profile, &bootstrap.instance_id))
    {
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
            may_reuse_stored_credential(
                profile,
                &url,
                &input.authentication,
                &bootstrap.instance_id,
            )
        }) {
            retained = get_credential(&id)?;
            &retained
        } else {
            return Err(
                "Enter the credential again when changing a profile address or authentication mode"
                    .to_string(),
            );
        };
        request_ticket(&url, supplied, &bootstrap.instance_id).await?;
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
    let bootstrap = fetch_bootstrap(&profile.url).await?;
    require_profile_identity(&profile, &bootstrap.instance_id)?;
    let credential_profile = profile.clone();
    if profile.local {
        profile = state.set_local_bootstrap(&bootstrap)?;
    }
    if !profile.local
        && !bootstrap.authentication_enabled
        && !profile.confirmed_unauthenticated_remote
    {
        return Err("Confirm unauthenticated remote access before connecting".to_string());
    }
    if !profile.local && !bootstrap.authentication_enabled && profile.authentication != "none" {
        return Err(
            "This instance now has authentication disabled; edit the profile and confirm its trusted network before connecting"
                .to_string(),
        );
    }
    if bootstrap.authentication_enabled {
        let expected = effective_authentication(&bootstrap)?;
        if !profile.local && profile.authentication != expected {
            return Err(format!(
                "This instance now requires {expected}; edit the profile before reconnecting"
            ));
        }
        let credential =
            credential_for_authentication(&state, &credential_profile, expected).await?;
        request_ticket(&profile.url, &credential, &bootstrap.instance_id).await?;
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
    if let Err(error) = window.navigate(
        profile
            .url
            .parse()
            .map_err(|error| format!("Invalid profile URL: {error}"))?,
    ) {
        let _ = rollback_selection();
        return Err(error.to_string());
    }
    Ok(())
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
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
) -> Result<Option<DesktopTicket>, String> {
    let _mutation_guard = state.mutation_lock.lock().await;
    let profile = require_active_profile_origin(&state, &webview)?;
    let (mut profile, bootstrap) =
        require_active_profile_identity_for(&state, &webview, profile, true).await?;
    let credential_profile = profile.clone();
    if profile.local {
        profile = state.set_local_bootstrap(&bootstrap)?;
    }
    if !bootstrap.authentication_enabled {
        return Ok(None);
    }
    let expected = effective_authentication(&bootstrap)?;
    if profile.authentication != expected {
        return Err(format!(
            "This instance now requires {expected}; edit the profile before reconnecting"
        ));
    }
    let credential = credential_for_authentication(&state, &credential_profile, expected).await?;
    let ticket = request_ticket(&profile.url, &credential, &bootstrap.instance_id).await?;
    Ok(Some(ticket))
}

#[tauri::command]
pub fn get_active_instance_profile(
    webview: tauri::WebviewWindow,
    state: State<'_, ProfileState>,
) -> Result<ActiveProfileIdentity, String> {
    // This intentionally remains origin-bound rather than identity-bound: it
    // exposes no credential or mutable state, and the login page needs the
    // saved pin in order to offer the explicit local-replacement recovery or
    // safe return-to-local flow when the live identity does not match.
    let profile = require_active_profile_origin(&state, &webview)?;
    Ok(ActiveProfileIdentity {
        instance_id: profile.instance_id,
        local: profile.local,
    })
}

#[tauri::command]
pub async fn trust_local_instance_replacement(
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
    let bootstrap = fetch_bootstrap(&profile.url).await?;
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
    let confirmed_bootstrap = fetch_bootstrap(&profile.url).await?;
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
    state.replace_local_bootstrap(previous_instance_id, &confirmed_bootstrap)
}

async fn preserve_verified_replacement_token_or_forget(
    state: &ProfileState,
    profile: &StoredProfile,
    bootstrap: &Bootstrap,
) -> Result<bool, String> {
    let observed = state.local_startup_token()?;
    let preserve = if effective_authentication(bootstrap)? == "startup_token" {
        if let Some(credential) = observed.as_ref() {
            request_ticket(&profile.url, credential, &bootstrap.instance_id)
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
    let remote_is_usable = match fetch_bootstrap(&profile.url).await {
        Ok(bootstrap)
            if profile
                .instance_id
                .as_deref()
                .is_none_or(|expected| expected == bootstrap.instance_id) =>
        {
            if bootstrap.authentication_enabled {
                match (
                    effective_authentication(&bootstrap),
                    profile_credential(state, &profile),
                ) {
                    (Ok(expected), Ok(credential)) if expected == profile.authentication => {
                        request_ticket(&profile.url, &credential, &bootstrap.instance_id)
                            .await
                            .is_ok()
                    }
                    _ => false,
                }
            } else {
                profile.authentication == "none" && profile.confirmed_unauthenticated_remote
            }
        }
        _ => false,
    };
    if remote_is_usable {
        profile.url
    } else {
        state
            .select_local()
            .unwrap_or_else(|_| "http://localhost:8935".to_string())
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
    if profile.instance_id.is_none() && !(allow_unpinned_local && profile.local) {
        return Err(
            "Desktop has not established this instance identity yet; reconnect before using profile commands"
                .to_string(),
        );
    }
    let bootstrap = fetch_matching_bootstrap(&profile).await?;

    // The bootstrap request yields across the executor. Recheck both the
    // selected profile and page origin afterward so a concurrent switch or
    // navigation cannot turn a successful check into authority for stale
    // state.
    if require_active_profile_origin(state, webview)? != profile {
        return Err("The selected Desktop profile changed while its identity was verified".into());
    }
    Ok((profile, bootstrap))
}

async fn fetch_matching_bootstrap(profile: &StoredProfile) -> Result<Bootstrap, String> {
    let bootstrap = fetch_bootstrap(&profile.url).await?;
    require_profile_identity(profile, &bootstrap.instance_id)?;
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
            let status = if !profile_identity_matches(profile, &bootstrap.instance_id) {
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

fn profile_identity_matches(profile: &StoredProfile, instance_id: &str) -> bool {
    profile
        .instance_id
        .as_deref()
        .is_none_or(|expected| expected == instance_id)
}

fn require_profile_identity(profile: &StoredProfile, instance_id: &str) -> Result<(), String> {
    if profile_identity_matches(profile, instance_id) {
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
    instance_id: &str,
) -> bool {
    profile_identity_matches(profile, instance_id)
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

async fn request_ticket(
    url: &str,
    credential: &str,
    expected_instance_id: &str,
) -> Result<DesktopTicket, String> {
    let response = http_client()?
        .post(format!("{url}/api/auth/desktop-ticket"))
        .json(&serde_json::json!({ "credential": credential }))
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
    let ticket: DesktopTicket = read_bounded_json(
        response,
        "The remote instance returned an invalid login ticket",
    )
    .await?;
    if ticket.instance_id != expected_instance_id {
        return Err(
            "The instance identity changed while Desktop was authenticating; no profile change was saved"
                .to_string(),
        );
    }
    Ok(ticket)
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
    fn persisted_profiles_contain_no_credential_field() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profiles.json");
        let file = ProfileFile {
            profiles: vec![StoredProfile {
                id: "remote".into(),
                name: "Remote".into(),
                url: "http://host:8935".into(),
                instance_id: Some("identity".into()),
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
                    authentication: "none".into(),
                    local: true,
                    confirmed_unauthenticated_remote: true,
                },
                StoredProfile {
                    id: "remote".into(),
                    name: "Remote".into(),
                    url: "https://remote.example".into(),
                    instance_id: Some("remote-instance".into()),
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
        let profile = StoredProfile {
            id: "remote".into(),
            name: "Remote".into(),
            url: "https://remote.example".into(),
            instance_id: Some("instance-a".into()),
            authentication: "password".into(),
            local: false,
            confirmed_unauthenticated_remote: false,
        };
        assert!(may_reuse_stored_credential(
            &profile,
            "https://remote.example",
            "password",
            "instance-a"
        ));
        assert!(!may_reuse_stored_credential(
            &profile,
            "https://attacker.example",
            "password",
            "instance-a"
        ));
        assert!(!may_reuse_stored_credential(
            &profile,
            "https://remote.example",
            "startup_token",
            "instance-a"
        ));
        assert!(!may_reuse_stored_credential(
            &profile,
            "https://remote.example",
            "password",
            "instance-b"
        ));
    }

    #[test]
    fn local_identity_pin_cannot_be_replaced_by_passive_discovery() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profiles.json");
        let file = ProfileFile {
            active_profile_id: LOCAL_PROFILE_ID.into(),
            profiles: vec![StoredProfile {
                id: LOCAL_PROFILE_ID.into(),
                name: "Local XpressClaw".into(),
                url: "http://localhost:8935".into(),
                instance_id: Some("trusted-local-instance".into()),
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
        };
        let replacement = Bootstrap {
            instance_id: "replacement-local-instance".into(),
            authentication_enabled: true,
            credential_kind: "startup_token".into(),
        };

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
        };
        let profile = StoredProfile {
            id: LOCAL_PROFILE_ID.into(),
            name: "Local XpressClaw".into(),
            url: "http://localhost:8935".into(),
            instance_id: None,
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

        let result = request_ticket(
            &format!("http://{redirect_address}"),
            "credential-that-must-not-be-forwarded",
            "expected-instance",
        )
        .await;
        assert!(result.is_err());
        redirect_thread.join().unwrap();
        assert!(!target_thread.join().unwrap());
    }

    #[tokio::test]
    async fn profile_inspection_never_submits_a_saved_credential() {
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex as StdMutex};
        use std::time::{Duration, Instant};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
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
                        let body = r#"{"instance_id":"replacement-instance","authentication_enabled":true,"credential_kind":"startup_token"}"#;
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
        };
        let profile = StoredProfile {
            id: LOCAL_PROFILE_ID.to_string(),
            name: "Local XpressClaw".to_string(),
            url: format!("http://{address}"),
            instance_id: Some("instance".to_string()),
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
                .starts_with("GET /api/auth/bootstrap HTTP/1.1"));
            let body = r#"{"instance_id":"replacement-instance","authentication_enabled":false,"credential_kind":"disabled"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let profile = StoredProfile {
            id: "remote".to_string(),
            name: "Remote".to_string(),
            url: format!("http://{address}"),
            instance_id: Some("trusted-instance".to_string()),
            authentication: "none".to_string(),
            local: false,
            confirmed_unauthenticated_remote: true,
        };

        let error = fetch_matching_bootstrap(&profile).await.unwrap_err();
        server.join().unwrap();
        assert!(error.contains("identity at this address changed"));
    }

    #[tokio::test]
    async fn trusting_a_replacement_preserves_its_verified_sidecar_token() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /api/auth/desktop-ticket HTTP/1.1"));
            assert!(request.contains("fresh-sidecar-token"));
            let body = r#"{"ticket":"single-use-ticket","instance_id":"replacement-instance"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });

        let root = tempfile::tempdir().unwrap();
        let profile = StoredProfile {
            id: LOCAL_PROFILE_ID.to_string(),
            name: "Local XpressClaw".to_string(),
            url: format!("http://{address}"),
            instance_id: Some("previous-instance".to_string()),
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
        };
        let bootstrap = Bootstrap {
            instance_id: "replacement-instance".to_string(),
            authentication_enabled: true,
            credential_kind: "startup_token".to_string(),
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
        use std::io::{Read, Write};
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
                stream
                    .set_read_timeout(Some(Duration::from_millis(500)))
                    .unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).unwrap();

                let current = active_requests.fetch_add(1, Ordering::SeqCst) + 1;
                maximum_requests.fetch_max(current, Ordering::SeqCst);
                let overlap_deadline = Instant::now() + Duration::from_millis(500);
                while maximum_requests.load(Ordering::SeqCst) < 2
                    && Instant::now() < overlap_deadline
                {
                    std::thread::sleep(Duration::from_millis(5));
                }

                let body = format!(
                    r#"{{"instance_id":"instance-{index}","authentication_enabled":false,"credential_kind":"disabled"}}"#
                );
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                active_requests.fetch_sub(1, Ordering::SeqCst);
            }));
            profiles.push(StoredProfile {
                id: format!("profile-{index}"),
                name: format!("Profile {index}"),
                url: format!("http://{address}"),
                instance_id: Some(format!("instance-{index}")),
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
