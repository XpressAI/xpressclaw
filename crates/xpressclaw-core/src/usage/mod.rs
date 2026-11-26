//! Harness quota and rate-limit usage tracking.
//!
//! Reads existing local credentials in read-only mode (never refreshing or
//! mutating rotating OAuth tokens owned by harnesses), queries vendor quota
//! endpoints or zero-cost CLI probes, caches results with `Retry-After`
//! awareness, and evaluates automation `quota_gate` policies so deferrable
//! scheduled workflows and tasks can wait until quota resets.

pub mod providers;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{watch, Mutex, RwLock};
use tracing::warn;

use crate::db::Database;

/// Default TTL for cached provider quota snapshots (5 minutes).
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);
/// Default HTTP request timeout for quota endpoints.
pub const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum allowed reset buffer in seconds (1 hour).
pub const MAX_RESET_BUFFER_SECONDS: i64 = 3_600;
/// Fallback deferral when a quota gate requires waiting but no `resets_at` was reported.
const UNKNOWN_RESET_DEFER_SECONDS: i64 = 15 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageTone {
    Default,
    Ok,
    Warning,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_label: Option<String>,
    #[serde(default)]
    pub summary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u64>,
    pub tone: UsageTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageBalanceUnit {
    Usd,
    Credits,
    Requests,
    Tokens,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageBalance {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    pub unit: UsageBalanceUnit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    pub tone: UsageTone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageDetail {
    pub id: String,
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<UsageTone>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UsageProblem {
    Expired {
        expires_at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refreshed_by: Option<String>,
    },
    Rejected {
        status: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refreshed_by: Option<String>,
    },
    NoQuota {
        detail: String,
    },
    RateLimited {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_at: Option<String>,
        detail: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageReportStatus {
    Available,
    Unavailable,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSuccessfulSnapshot {
    pub fetched_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub windows: Vec<UsageWindow>,
    #[serde(default)]
    pub balances: Vec<UsageBalance>,
    #[serde(default)]
    pub details: Vec<UsageDetail>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageAccountReport {
    /// Stable composite identifier (`<provider>:<hashed_key>`). Never contains raw credentials.
    pub id: String,
    /// Canonical XpressClaw harness kind (e.g., `claude`, `codex`, `antigravity`).
    pub provider: String,
    pub provider_label: String,
    /// Harness store that owns this credential (e.g., `Claude`, `Codex`, `Pi`, `OpenCode`).
    pub harness: String,
    /// Optional human-readable account hint (e.g., email from local JWT claims).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    pub status: UsageReportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    pub fetched_at: String,
    #[serde(default)]
    pub windows: Vec<UsageWindow>,
    #[serde(default)]
    pub balances: Vec<UsageBalance>,
    #[serde(default)]
    pub details: Vec<UsageDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<UsageProblem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful: Option<UsageSuccessfulSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredAutomationSummary {
    /// `"workflow"` or `"schedule"`.
    pub kind: String,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub resume_at: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub fetched_at: String,
    pub accounts: Vec<UsageAccountReport>,
    pub deferred_automations: Vec<DeferredAutomationSummary>,
}

// ---------------------------------------------------------------------------
// Automation quota gate policy
// ---------------------------------------------------------------------------

fn default_min_remaining_pct() -> f64 {
    10.0
}

fn default_window_selector() -> String {
    "any".to_string()
}

fn default_reset_buffer_seconds() -> i64 {
    60
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaGateAction {
    #[default]
    WaitForReset,
    Skip,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaUnknownBehavior {
    #[default]
    RunAnyway,
    Wait,
}

/// Optional quota policy attached to a scheduled workflow or task schedule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaGatePolicy {
    /// Minimum remaining percentage required before dispatching (0..=100).
    #[serde(default = "default_min_remaining_pct")]
    pub min_remaining_pct: f64,
    /// Which window(s) to check: `"any"` (default), `"session"`, `"weekly"`, `"monthly"`,
    /// or an exact window `id`.
    #[serde(default = "default_window_selector")]
    pub window: String,
    /// Optional explicit harness kind override (e.g., `"claude"`, `"codex"`).
    /// When omitted, XpressClaw infers the harness from the scheduled agent's runner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Behavior when remaining quota is below `min_remaining_pct`.
    #[serde(default)]
    pub on_limit: QuotaGateAction,
    /// Behavior when the provider quota cannot be read (e.g. offline, 429, unsupported).
    #[serde(default)]
    pub on_unknown: QuotaUnknownBehavior,
    /// Extra seconds to wait past `resets_at` before re-checking (default 60s, max 3600s).
    #[serde(default = "default_reset_buffer_seconds")]
    pub reset_buffer_seconds: i64,
}

impl Default for QuotaGatePolicy {
    fn default() -> Self {
        Self {
            min_remaining_pct: default_min_remaining_pct(),
            window: default_window_selector(),
            provider: None,
            on_limit: QuotaGateAction::WaitForReset,
            on_unknown: QuotaUnknownBehavior::RunAnyway,
            reset_buffer_seconds: default_reset_buffer_seconds(),
        }
    }
}

impl QuotaGatePolicy {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !self.min_remaining_pct.is_finite()
            || self.min_remaining_pct < 0.0
            || self.min_remaining_pct > 100.0
        {
            return Err("quota_gate.min_remaining_pct must be between 0 and 100".into());
        }
        if self.window.trim().is_empty() {
            return Err("quota_gate.window cannot be empty".into());
        }
        if self.reset_buffer_seconds < 0 || self.reset_buffer_seconds > MAX_RESET_BUFFER_SECONDS {
            return Err(format!(
                "quota_gate.reset_buffer_seconds must be between 0 and {MAX_RESET_BUFFER_SECONDS}"
            ));
        }
        if let Some(provider) = self.provider.as_deref() {
            if provider.trim().is_empty() {
                return Err("quota_gate.provider cannot be blank".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaGateDecision {
    Allow,
    Defer {
        until: DateTime<Utc>,
        reason: String,
    },
    Skip {
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Helper builders
// ---------------------------------------------------------------------------

pub fn tone_from_used_pct(used_pct: Option<f64>) -> UsageTone {
    match used_pct {
        Some(pct) if pct > 90.0 => UsageTone::Danger,
        Some(pct) if pct >= 70.0 => UsageTone::Warning,
        Some(_) => UsageTone::Ok,
        None => UsageTone::Default,
    }
}

pub fn balance_tone_from_remaining(remaining: Option<f64>) -> UsageTone {
    match remaining {
        Some(rem) if rem <= 0.0 => UsageTone::Danger,
        Some(_) => UsageTone::Ok,
        None => UsageTone::Default,
    }
}

pub fn used_pct_of(used: Option<f64>, limit: Option<f64>) -> Option<f64> {
    let used = used?;
    let limit = limit?;
    if !used.is_finite() || !limit.is_finite() || limit <= 0.0 {
        return None;
    }
    Some((used / limit * 100.0).clamp(0.0, 100.0))
}

pub fn hash_account_key(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn window_from_used_pct(
    id: impl Into<String>,
    label: impl Into<String>,
    short_label: Option<String>,
    summary: bool,
    utilization_pct: Option<f64>,
    duration_seconds: Option<u64>,
    resets_at: Option<String>,
    tone: Option<UsageTone>,
) -> UsageWindow {
    let used_pct = utilization_pct
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.0, 100.0));
    let remaining_pct = used_pct.map(|v| (100.0 - v).clamp(0.0, 100.0));
    UsageWindow {
        id: id.into(),
        label: label.into(),
        short_label,
        summary,
        used_pct,
        remaining_pct,
        resets_at,
        duration_seconds,
        tone: tone.unwrap_or_else(|| tone_from_used_pct(used_pct)),
    }
}

pub fn duration_window_name(seconds: u64) -> (String, String, String) {
    if seconds == 604_800 {
        return ("weekly".into(), "Weekly".into(), "wk".into());
    }
    let id = if seconds == 18_000 {
        "five_hour".to_string()
    } else {
        format!("{seconds}s")
    };
    const UNITS: &[(u64, &str, &str)] = &[
        (86_400, "day", "d"),
        (3_600, "hour", "h"),
        (60, "minute", "m"),
        (1, "second", "s"),
    ];
    let (size, unit_long, unit_short) = UNITS
        .iter()
        .copied()
        .find(|(size, _, _)| seconds.is_multiple_of(*size))
        .unwrap_or((1, "second", "s"));
    let amount = seconds / size;
    (
        id,
        format!("{amount}-{unit_long}"),
        format!("{amount}{unit_short}"),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn window_from_reported_duration(
    duration_seconds: Option<u64>,
    scope: Option<(&str, &str)>,
    unknown_id: &str,
    unknown_label: &str,
    unknown_short: &str,
    utilization_pct: Option<f64>,
    resets_at: Option<String>,
    summary: bool,
    tone: Option<UsageTone>,
) -> UsageWindow {
    let valid_duration = duration_seconds.filter(|&s| s > 0);
    let (base_id, base_label, base_short) = match valid_duration {
        Some(secs) => duration_window_name(secs),
        None => (
            unknown_id.to_string(),
            unknown_label.to_string(),
            unknown_short.to_string(),
        ),
    };
    let (id, label, short_label) = match scope {
        Some((scope_id, scope_label)) => (
            format!("{scope_id}:{base_id}"),
            format!("{scope_label} · {base_label}"),
            if base_short.is_empty() {
                Some(scope_label.to_string())
            } else {
                Some(format!("{scope_label} {base_short}"))
            },
        ),
        None => (base_id, base_label, Some(base_short)),
    };
    window_from_used_pct(
        id,
        label,
        short_label,
        summary,
        utilization_pct,
        valid_duration,
        resets_at,
        tone,
    )
}

/// Parse an HTTP `Retry-After` header (delta seconds or HTTP date) into a UTC timestamp.
pub fn parse_retry_after(header: Option<&str>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let raw = header?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(seconds) = raw.parse::<f64>() {
        if seconds.is_finite() && seconds >= 0.0 {
            let clamped = seconds.min(86_400.0) as i64;
            return Some(now + chrono::Duration::seconds(clamped));
        }
    }
    if let Ok(parsed) = DateTime::parse_from_rfc2822(raw) {
        let dt = parsed.with_timezone(&Utc);
        return Some(dt.max(now));
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        let dt = parsed.with_timezone(&Utc);
        return Some(dt.max(now));
    }
    None
}

// ---------------------------------------------------------------------------
// Quota gate evaluation (pure)
// ---------------------------------------------------------------------------

fn window_matches_selector(window: &UsageWindow, selector: &str) -> bool {
    let sel = selector.trim().to_ascii_lowercase();
    if sel.is_empty() || sel == "any" || sel == "all" {
        return true;
    }
    if window.id.eq_ignore_ascii_case(&sel) {
        return true;
    }
    match sel.as_str() {
        "session" | "5h" | "five_hour" | "rolling" => {
            window.id == "five_hour"
                || window.id == "rolling"
                || window.duration_seconds.is_some_and(|s| s <= 6 * 3600)
                || window.label.to_ascii_lowercase().contains("session")
                || window.label.to_ascii_lowercase().contains("5-hour")
        }
        "weekly" | "week" | "7d" | "wk" => {
            window.id == "weekly"
                || window.id.starts_with("weekly_")
                || window
                    .duration_seconds
                    .is_some_and(|s| (5 * 86_400..=10 * 86_400).contains(&s))
                || window.label.to_ascii_lowercase().contains("weekly")
        }
        "monthly" | "month" | "30d" | "mo" => {
            window.id == "monthly"
                || window.duration_seconds.is_some_and(|s| s >= 25 * 86_400)
                || window.label.to_ascii_lowercase().contains("monthly")
        }
        _ => false,
    }
}

/// Evaluate a provider's usage reports against a `QuotaGatePolicy`.
///
/// Uses fresh available windows when present, falling back to `last_successful`
/// only when its reset time is still in the future.
pub fn evaluate_reports_against_gate(
    provider: &str,
    reports: &[UsageAccountReport],
    gate: &QuotaGatePolicy,
    now: DateTime<Utc>,
) -> QuotaGateDecision {
    let buffer =
        chrono::Duration::seconds(gate.reset_buffer_seconds.clamp(0, MAX_RESET_BUFFER_SECONDS));

    // Collect matching windows across all accounts for this provider.
    let mut matching_windows: Vec<&UsageWindow> = Vec::new();
    for report in reports {
        if report.status == UsageReportStatus::Available {
            for window in &report.windows {
                if window_matches_selector(window, &gate.window) && window.remaining_pct.is_some() {
                    matching_windows.push(window);
                }
            }
        } else if let Some(last) = &report.last_successful {
            for window in &last.windows {
                let not_expired = window
                    .resets_at
                    .as_deref()
                    .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                    .is_some_and(|reset| reset.with_timezone(&Utc) > now);
                if not_expired
                    && window_matches_selector(window, &gate.window)
                    && window.remaining_pct.is_some()
                {
                    matching_windows.push(window);
                }
            }
        }
    }

    if matching_windows.is_empty() {
        return match gate.on_unknown {
            QuotaUnknownBehavior::RunAnyway => QuotaGateDecision::Allow,
            QuotaUnknownBehavior::Wait => {
                let until = now + chrono::Duration::seconds(UNKNOWN_RESET_DEFER_SECONDS);
                QuotaGateDecision::Defer {
                    until,
                    reason: format!("Waiting for {provider} quota data (currently unavailable)"),
                }
            }
        };
    }

    // Find any window violating min_remaining_pct. Choose the latest reset among violating windows.
    let mut violating: Vec<(&UsageWindow, f64)> = matching_windows
        .into_iter()
        .filter_map(|w| {
            let remaining = w.remaining_pct?;
            if remaining < gate.min_remaining_pct {
                Some((w, remaining))
            } else {
                None
            }
        })
        .collect();

    if violating.is_empty() {
        return QuotaGateDecision::Allow;
    }

    // Sort by lowest remaining percentage first for the most informative reason.
    violating.sort_by(|a, b| a.1.total_cmp(&b.1));
    let (worst, worst_remaining) = violating[0];

    let resume_at = violating
        .iter()
        .filter_map(|(w, _)| {
            w.resets_at
                .as_deref()
                .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| dt.with_timezone(&Utc))
                .filter(|&dt| dt > now)
        })
        .max()
        .map(|reset| reset + buffer)
        .unwrap_or_else(|| now + chrono::Duration::seconds(UNKNOWN_RESET_DEFER_SECONDS));

    let reason = format!(
        "{provider} {} has {:.0}% remaining (needs ≥{:.0}%)",
        worst.label, worst_remaining, gate.min_remaining_pct
    );

    match gate.on_limit {
        QuotaGateAction::WaitForReset => QuotaGateDecision::Defer {
            until: resume_at,
            reason,
        },
        QuotaGateAction::Skip => QuotaGateDecision::Skip { reason },
    }
}

// ---------------------------------------------------------------------------
// UsageService
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CachedReport {
    cached_at: DateTime<Utc>,
    report: UsageAccountReport,
}

#[derive(Clone)]
pub struct UsageService {
    client: reqwest::Client,
    cache_ttl: Duration,
    cache: Arc<RwLock<HashMap<String, CachedReport>>>,
    last_successful: Arc<RwLock<HashMap<String, UsageSuccessfulSnapshot>>>,
    rate_limited_until: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
    overrides: Arc<RwLock<HashMap<String, Vec<UsageAccountReport>>>>,
    in_flight: Arc<Mutex<HashMap<String, watch::Receiver<Option<UsageAccountReport>>>>>,
}

impl Default for UsageService {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageService {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(DEFAULT_HTTP_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            cache_ttl: DEFAULT_CACHE_TTL,
            cache: Arc::new(RwLock::new(HashMap::new())),
            last_successful: Arc::new(RwLock::new(HashMap::new())),
            rate_limited_until: Arc::new(RwLock::new(HashMap::new())),
            overrides: Arc::new(RwLock::new(HashMap::new())),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Inject a live/test report for a provider. Primarily used for passive live-session
    /// feeds and deterministic automation/API tests.
    pub async fn set_provider_override(&self, provider: &str, reports: Vec<UsageAccountReport>) {
        self.overrides
            .write()
            .await
            .insert(provider.to_string(), reports);
    }

    pub async fn clear_provider_override(&self, provider: &str) {
        self.overrides.write().await.remove(provider);
    }

    /// Return a full snapshot across discovered providers (or a single filtered provider),
    /// along with any automations currently deferred waiting for quota reset.
    pub async fn snapshot(
        &self,
        db: &Database,
        force_refresh: bool,
        provider_filter: Option<&str>,
    ) -> UsageSnapshot {
        let now = Utc::now();
        let accounts = self.list_reports(force_refresh, provider_filter).await;
        let deferred_automations = list_deferred_automations(db, now);
        UsageSnapshot {
            fetched_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
            accounts,
            deferred_automations,
        }
    }

    /// Discover and fetch usage reports across all supported providers (or a single provider).
    pub async fn list_reports(
        &self,
        force_refresh: bool,
        provider_filter: Option<&str>,
    ) -> Vec<UsageAccountReport> {
        let filter = provider_filter.map(|p| {
            crate::acp::canonical_agent_kind(p)
                .unwrap_or(p)
                .to_ascii_lowercase()
        });

        // Check overrides first.
        let overrides = self.overrides.read().await.clone();
        if let Some(ref provider) = filter {
            if let Some(overridden) = overrides.get(provider) {
                return overridden.clone();
            }
        }

        let discovered = tokio::task::spawn_blocking({
            let filter = filter.clone();
            move || providers::discover_accounts(filter.as_deref())
        })
        .await
        .unwrap_or_default();

        // Fetch all discovered accounts concurrently.
        let mut handles = Vec::with_capacity(discovered.len());
        for account in discovered {
            if overrides.contains_key(&account.provider) {
                continue;
            }
            let svc = self.clone();
            handles.push(tokio::spawn(async move {
                svc.fetch_account(account, force_refresh).await
            }));
        }

        let mut reports = Vec::new();
        for (provider, overridden) in overrides {
            if filter.as_deref().is_none_or(|f| f == provider) {
                reports.extend(overridden);
            }
        }
        for handle in handles {
            if let Ok(report) = handle.await {
                reports.push(report);
            }
        }

        reports.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));
        reports
    }

    /// Targeted check for a single provider used by automation schedulers.
    pub async fn evaluate_quota_gate(
        &self,
        provider: &str,
        gate: &QuotaGatePolicy,
        now: DateTime<Utc>,
    ) -> QuotaGateDecision {
        let canonical = crate::acp::canonical_agent_kind(provider)
            .unwrap_or(provider)
            .to_ascii_lowercase();
        let reports = self.list_reports(false, Some(&canonical)).await;
        evaluate_reports_against_gate(&canonical, &reports, gate, now)
    }

    async fn fetch_account(
        &self,
        account: providers::DiscoveredAccount,
        force_refresh: bool,
    ) -> UsageAccountReport {
        let id = account.id.clone();
        let now = Utc::now();

        if !force_refresh {
            if let Some(cached) = self.cache.read().await.get(&id).cloned() {
                let age = (now - cached.cached_at).to_std().unwrap_or(self.cache_ttl);
                if age < self.cache_ttl {
                    return cached.report;
                }
            }
        }

        // Coalesce concurrent requests for the same account ID using a watch channel so
        // waiters cannot miss completion signals if the leader finishes between dropping
        // the map lock and polling, and cancelled leaders never stall waiters.
        loop {
            let existing_rx = {
                let mut in_flight = self.in_flight.lock().await;
                if let Some(existing) = in_flight.get(&id) {
                    if existing.borrow().is_none() && existing.has_changed().is_ok() {
                        Some(existing.clone())
                    } else {
                        let (tx, rx) = watch::channel(None);
                        in_flight.insert(id.clone(), rx);
                        drop(in_flight);
                        return self.run_as_in_flight_leader(id, account, now, tx).await;
                    }
                } else {
                    let (tx, rx) = watch::channel(None);
                    in_flight.insert(id.clone(), rx);
                    drop(in_flight);
                    return self.run_as_in_flight_leader(id, account, now, tx).await;
                }
            };

            if let Some(mut rx) = existing_rx {
                if let Ok(guard) = rx.wait_for(|report| report.is_some()).await {
                    if let Some(report) = guard.as_ref() {
                        return report.clone();
                    }
                }
                // Leader dropped without delivering a result; retry to become leader.
            }
        }
    }

    async fn run_as_in_flight_leader(
        &self,
        id: String,
        account: providers::DiscoveredAccount,
        now: DateTime<Utc>,
        tx: watch::Sender<Option<UsageAccountReport>>,
    ) -> UsageAccountReport {
        let result = self.fetch_account_inner(account, now).await;
        {
            let mut in_flight = self.in_flight.lock().await;
            in_flight.remove(&id);
        }
        let _ = tx.send(Some(result.clone()));
        result
    }

    async fn fetch_account_inner(
        &self,
        account: providers::DiscoveredAccount,
        now: DateTime<Utc>,
    ) -> UsageAccountReport {
        let id = account.id.clone();
        let fetched_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);

        // Enforce active HTTP 429 Retry-After holds even on forced refresh.
        if let Some(until) = self.rate_limited_until.read().await.get(&id).copied() {
            if until > now {
                let last_successful = self.last_successful.read().await.get(&id).cloned();
                let wait_mins = ((until - now).num_seconds().max(60) + 59) / 60;
                return UsageAccountReport {
                    id,
                    provider: account.provider,
                    provider_label: account.provider_label,
                    harness: account.harness,
                    account_label: account.account_label,
                    status: UsageReportStatus::Unavailable,
                    plan_label: last_successful.as_ref().and_then(|s| s.plan_label.clone()),
                    observed_at: last_successful.as_ref().and_then(|s| s.observed_at.clone()),
                    fetched_at,
                    windows: Vec::new(),
                    balances: Vec::new(),
                    details: Vec::new(),
                    problem: Some(UsageProblem::RateLimited {
                        retry_at: Some(until.to_rfc3339_opts(SecondsFormat::Secs, true)),
                        detail: format!("Rate limited by provider; retrying in ~{wait_mins}m"),
                    }),
                    error: None,
                    last_successful,
                };
            }
        }

        let outcome = providers::fetch_account_usage(&self.client, &account, now).await;
        if let Some(retry_at) = outcome.retry_at {
            self.rate_limited_until
                .write()
                .await
                .insert(id.clone(), retry_at);
        } else if outcome.report.status == UsageReportStatus::Available {
            self.rate_limited_until.write().await.remove(&id);
        }

        let mut report = outcome.report;
        if report.status == UsageReportStatus::Available {
            let snap = UsageSuccessfulSnapshot {
                fetched_at: report.fetched_at.clone(),
                plan_label: report.plan_label.clone(),
                observed_at: report.observed_at.clone(),
                windows: report.windows.clone(),
                balances: report.balances.clone(),
                details: report.details.clone(),
            };
            self.last_successful.write().await.insert(id.clone(), snap);
        } else {
            report.last_successful = self.last_successful.read().await.get(&id).cloned();
            if let Some(err) = &report.error {
                warn!(provider = %report.provider, error = %err, "Harness usage fetch failed");
            }
        }

        self.cache.write().await.insert(
            id,
            CachedReport {
                cached_at: Utc::now(),
                report: report.clone(),
            },
        );
        report
    }
}

/// Query SQLite for any enabled workflows or schedules currently deferred by a quota gate.
pub fn list_deferred_automations(
    db: &Database,
    now: DateTime<Utc>,
) -> Vec<DeferredAutomationSummary> {
    let mut items = Vec::new();
    let _ = db.with_conn(|conn| {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT id, name, quota_deferred_until, quota_deferred_reason
             FROM workflows
             WHERE enabled = 1 AND quota_deferred_until IS NOT NULL",
        ) {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            }) {
                for row in rows.flatten() {
                    if is_future_rfc3339(&row.2, now) {
                        let reason = row.3.unwrap_or_else(|| "Waiting for quota reset".into());
                        let provider = extract_provider_from_reason(&reason);
                        items.push(DeferredAutomationSummary {
                            kind: "workflow".into(),
                            id: row.0,
                            name: row.1,
                            provider,
                            resume_at: row.2,
                            reason,
                        });
                    }
                }
            }
        }

        if let Ok(mut stmt) = conn.prepare(
            "SELECT id, name, quota_deferred_until, quota_deferred_reason
             FROM schedules
             WHERE enabled = 1 AND quota_deferred_until IS NOT NULL",
        ) {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            }) {
                for row in rows.flatten() {
                    if is_future_rfc3339(&row.2, now) {
                        let reason = row.3.unwrap_or_else(|| "Waiting for quota reset".into());
                        let provider = extract_provider_from_reason(&reason);
                        items.push(DeferredAutomationSummary {
                            kind: "schedule".into(),
                            id: row.0,
                            name: row.1,
                            provider,
                            resume_at: row.2,
                            reason,
                        });
                    }
                }
            }
        }
        Ok::<_, rusqlite::Error>(())
    });

    items.sort_by(|a, b| {
        a.resume_at
            .cmp(&b.resume_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    items
}

fn is_future_rfc3339(value: &str, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc) > now)
        .unwrap_or(false)
}

fn extract_provider_from_reason(reason: &str) -> Option<String> {
    let first = reason.split_whitespace().next()?;
    crate::acp::canonical_agent_kind(first).map(str::to_string)
}

/// Resolve the canonical harness kind for an agent from in-memory config or DB fallback.
///
/// Mirrors `workers::native::resolve_runner_kind` so legacy backend names (such as
/// `claude-sdk` or `copilot-cli`) under `runner.kind: auto` resolve consistently with
/// actual execution.
pub fn resolve_agent_provider(
    db: &Database,
    config: Option<&crate::config::Config>,
    agent_id: &str,
) -> Option<String> {
    if let Some(cfg) = config {
        if let Some(agent) = cfg.agents.iter().find(|a| a.name == agent_id) {
            if let Ok(kind) = crate::workers::native::resolve_runner_kind(agent) {
                if !kind.trim().is_empty() {
                    return Some(kind);
                }
            }
        }
    }
    let backend: Option<String> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT backend FROM agents WHERE id = ?1",
            [agent_id],
            |row| row.get(0),
        )
        .ok()
    });
    backend
        .as_deref()
        .and_then(crate::acp::infer_agent_kind_from_backend)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report(provider: &str, windows: Vec<UsageWindow>) -> UsageAccountReport {
        UsageAccountReport {
            id: format!("{provider}:test"),
            provider: provider.into(),
            provider_label: provider.into(),
            harness: provider.into(),
            account_label: None,
            status: UsageReportStatus::Available,
            plan_label: Some("Pro".into()),
            observed_at: None,
            fetched_at: "2026-11-26T12:00:00Z".into(),
            windows,
            balances: Vec::new(),
            details: Vec::new(),
            problem: None,
            error: None,
            last_successful: None,
        }
    }

    #[test]
    fn quota_gate_allows_when_above_threshold() {
        let now = DateTime::parse_from_rfc3339("2026-11-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let report = sample_report(
            "claude",
            vec![window_from_used_pct(
                "five_hour",
                "Session",
                Some("5h".into()),
                true,
                Some(65.0),
                Some(18_000),
                Some("2026-11-26T15:00:00Z".into()),
                None,
            )],
        );
        let gate = QuotaGatePolicy {
            min_remaining_pct: 20.0,
            ..Default::default()
        };
        assert_eq!(
            evaluate_reports_against_gate("claude", &[report], &gate, now),
            QuotaGateDecision::Allow
        );
    }

    #[test]
    fn quota_gate_defers_until_reset_plus_buffer_when_below_threshold() {
        let now = DateTime::parse_from_rfc3339("2026-11-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let report = sample_report(
            "claude",
            vec![window_from_used_pct(
                "five_hour",
                "Session",
                Some("5h".into()),
                true,
                Some(92.0),
                Some(18_000),
                Some("2026-11-26T14:30:00Z".into()),
                None,
            )],
        );
        let gate = QuotaGatePolicy {
            min_remaining_pct: 20.0,
            window: "session".into(),
            reset_buffer_seconds: 90,
            ..Default::default()
        };
        match evaluate_reports_against_gate("claude", &[report], &gate, now) {
            QuotaGateDecision::Defer { until, reason } => {
                assert_eq!(until.to_rfc3339(), "2026-11-26T14:31:30+00:00");
                assert!(reason.contains("claude"));
                assert!(reason.contains("8% remaining"));
            }
            other => panic!("expected Defer, got {other:?}"),
        }
    }

    #[test]
    fn quota_gate_respects_window_selector_and_skip_action() {
        let now = DateTime::parse_from_rfc3339("2026-11-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let report = sample_report(
            "codex",
            vec![
                window_from_used_pct(
                    "five_hour",
                    "5-hour",
                    Some("5h".into()),
                    true,
                    Some(10.0),
                    Some(18_000),
                    Some("2026-11-26T14:00:00Z".into()),
                    None,
                ),
                window_from_used_pct(
                    "weekly",
                    "Weekly",
                    Some("wk".into()),
                    true,
                    Some(95.0),
                    Some(604_800),
                    Some("2026-12-01T00:00:00Z".into()),
                    None,
                ),
            ],
        );
        // Session-only selector ignores the weekly window.
        let session_gate = QuotaGatePolicy {
            min_remaining_pct: 20.0,
            window: "session".into(),
            ..Default::default()
        };
        assert_eq!(
            evaluate_reports_against_gate(
                "codex",
                std::slice::from_ref(&report),
                &session_gate,
                now
            ),
            QuotaGateDecision::Allow
        );

        // Weekly selector triggers Skip when configured.
        let weekly_skip = QuotaGatePolicy {
            min_remaining_pct: 20.0,
            window: "weekly".into(),
            on_limit: QuotaGateAction::Skip,
            ..Default::default()
        };
        assert!(matches!(
            evaluate_reports_against_gate("codex", &[report], &weekly_skip, now),
            QuotaGateDecision::Skip { .. }
        ));
    }

    #[test]
    fn quota_gate_unknown_behavior_fails_open_or_waits() {
        let now = DateTime::parse_from_rfc3339("2026-11-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let fail_open = QuotaGatePolicy::default();
        assert_eq!(
            evaluate_reports_against_gate("claude", &[], &fail_open, now),
            QuotaGateDecision::Allow
        );

        let fail_wait = QuotaGatePolicy {
            on_unknown: QuotaUnknownBehavior::Wait,
            ..Default::default()
        };
        assert!(matches!(
            evaluate_reports_against_gate("claude", &[], &fail_wait, now),
            QuotaGateDecision::Defer { .. }
        ));
    }

    #[test]
    fn retry_after_parses_seconds_and_http_date() {
        let now = DateTime::parse_from_rfc3339("2026-11-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            parse_retry_after(Some("120"), now),
            Some(now + chrono::Duration::seconds(120))
        );
        assert_eq!(
            parse_retry_after(Some("Thu, 26 Nov 2026 12:15:00 GMT"), now),
            Some(now + chrono::Duration::minutes(15))
        );
        assert_eq!(parse_retry_after(Some("not-a-date"), now), None);
    }

    #[test]
    fn resolve_agent_provider_handles_legacy_backends_in_config_and_db() {
        use crate::config::{AgentConfig, Config, NativeRunnerConfig};

        let db = Database::open_memory().unwrap();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO agents (id, name, backend, config)
                 VALUES ('db-claude', 'DB Claude', 'claude-sdk', '{}'),
                        ('db-copilot', 'DB Copilot', 'copilot-cli', '{}'),
                        ('cfg-claude', 'Cfg Claude', 'native', '{}'),
                        ('cfg-copilot', 'Cfg Copilot', 'native', '{}'),
                        ('cfg-explicit', 'Cfg Explicit', 'native', '{}');",
            )
        })
        .unwrap();

        let cfg = Config {
            agents: vec![
                AgentConfig {
                    name: "cfg-claude".into(),
                    backend: "claude-sdk".into(),
                    runner: NativeRunnerConfig {
                        kind: "auto".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                AgentConfig {
                    name: "cfg-copilot".into(),
                    backend: "copilot-cli".into(),
                    runner: NativeRunnerConfig {
                        kind: "auto".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                AgentConfig {
                    name: "cfg-explicit".into(),
                    backend: "native".into(),
                    runner: NativeRunnerConfig {
                        kind: "copilot".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            resolve_agent_provider(&db, Some(&cfg), "cfg-claude").as_deref(),
            Some("claude")
        );
        assert_eq!(
            resolve_agent_provider(&db, Some(&cfg), "cfg-copilot").as_deref(),
            Some("github-copilot")
        );
        assert_eq!(
            resolve_agent_provider(&db, Some(&cfg), "cfg-explicit").as_deref(),
            Some("github-copilot")
        );
        assert_eq!(
            resolve_agent_provider(&db, Some(&cfg), "db-claude").as_deref(),
            Some("claude")
        );
        assert_eq!(
            resolve_agent_provider(&db, None, "db-copilot").as_deref(),
            Some("github-copilot")
        );
    }

    #[tokio::test]
    async fn in_flight_coalescing_does_not_stall_when_leader_completes_early() {
        let svc = UsageService::new();
        let account = providers::DiscoveredAccount {
            id: "codex:coalesce-test".into(),
            provider: "codex".into(),
            provider_label: "Codex".into(),
            harness: "Codex".into(),
            account_label: None,
            source: providers::AccountSource::CopilotEnv {
                var: "XPRESSCLAW_NONEXISTENT_TOKEN_VAR".into(),
            },
        };
        let expected = sample_report("codex", Vec::new());

        // Simulate a leader registering an in-flight channel, completing, and dropping
        // the sender before a concurrent waiter awaits the cloned receiver.
        let (tx, rx) = watch::channel(None);
        svc.in_flight
            .lock()
            .await
            .insert(account.id.clone(), rx.clone());

        let svc_waiter = svc.clone();
        let account_waiter = account.clone();
        let waiter =
            tokio::spawn(async move { svc_waiter.fetch_account(account_waiter, true).await });

        // Yield once so the waiter can clone the receiver, then complete the leader.
        tokio::task::yield_now().await;
        svc.in_flight.lock().await.remove(&account.id);
        let _ = tx.send(Some(expected.clone()));
        drop(tx);

        let result = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("coalesced waiter must not stall")
            .unwrap();
        assert_eq!(result.id, expected.id);
    }
}
