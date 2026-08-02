//! Durable workflow event waits.
//!
//! Wait state lives in `workflow_step_executions`, so the poller can resume a
//! workflow after a process restart without keeping an agent turn or container
//! alive. GitHub polling deliberately reuses the same project-scoped access
//! discovery as native workers.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::workers::github;

use super::engine::WorkflowEngine;
use super::instance::InstanceManager;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WaitState {
    pub event: String,
    pub resource: String,
    pub agent_id: String,
    pub started_at: String,
    /// Stable tie-breaker for multiple activities with the same GitHub
    /// timestamp. It lets a repeated wait consume each event exactly once.
    #[serde(default)]
    pub after_cursor: Option<String>,
    #[serde(default)]
    pub timeout_at: Option<String>,
    #[serde(default)]
    pub next_poll_at: Option<String>,
    #[serde(default = "default_poll_interval_seconds")]
    pub poll_interval_seconds: u64,
    #[serde(default)]
    pub last_checked_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
}

const MIN_POLL_INTERVAL_SECONDS: u64 = 15;
const MAX_POLL_INTERVAL_SECONDS: u64 = 300;

fn default_poll_interval_seconds() -> u64 {
    MIN_POLL_INTERVAL_SECONDS
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PullRequestRef {
    owner: String,
    repo: String,
    number: u64,
}

pub(crate) fn validate_resource(event: &str, resource: &str) -> Result<()> {
    if event.starts_with("github.pull_request.") {
        parse_pull_request(resource)?;
    }
    Ok(())
}

/// Poll durable event waits until shutdown. Individual authentication or
/// network failures leave the wait intact and are retried on the next pass.
pub async fn start_wait_runner(db: Arc<Database>, config: Arc<RwLock<Arc<Config>>>) {
    info!("workflow wait runner started");
    loop {
        let current_config = config.read().expect("config lock poisoned").clone();
        if let Err(error) = poll_waits_once(&db, &current_config).await {
            error!(error = %error, "workflow wait check failed");
        }
        tokio::time::sleep(std::time::Duration::from_secs(MIN_POLL_INTERVAL_SECONDS)).await;
    }
}

pub async fn poll_waits_once(db: &Arc<Database>, config: &Config) -> Result<u32> {
    let instances = InstanceManager::new(db.clone());
    let engine = WorkflowEngine::new(db.clone());
    let waiting = instances.list_waiting_step_executions()?;
    let mut resumed = 0;

    for execution in waiting {
        let Some(raw_state) = execution.input_context.as_deref() else {
            warn!(execution_id = execution.id, "workflow wait has no persisted state");
            continue;
        };
        let mut state = match serde_json::from_str::<WaitState>(raw_state) {
            Ok(state) => state,
            Err(error) => {
                warn!(execution_id = execution.id, error = %error, "workflow wait state is invalid");
                continue;
            }
        };

        if state
            .timeout_at
            .as_deref()
            .and_then(parse_timestamp)
            .is_some_and(|timeout| Utc::now() >= timeout)
        {
            if let Err(error) = engine.timeout_wait_execution(&execution.id) {
                warn!(execution_id = execution.id, error = %error, "failed to time out workflow wait");
            } else {
                resumed += 1;
            }
            continue;
        }

        if state
            .next_poll_at
            .as_deref()
            .and_then(parse_timestamp)
            .is_some_and(|next_poll| Utc::now() < next_poll)
        {
            continue;
        }

        let Some(agent) = config.agents.iter().find(|agent| agent.name == state.agent_id) else {
            warn!(execution_id = execution.id, agent_id = state.agent_id, "workflow wait agent is no longer configured");
            defer_wait(&instances, &execution.id, &mut state, Some("the bound agent is no longer configured"))?;
            continue;
        };
        let workspace = agent
            .runner
            .workspace
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| config.system.workspace_dir.clone());
        let Some(access) = github::discover(db, &workspace) else {
            warn!(execution_id = execution.id, agent_id = state.agent_id, workspace = %workspace.display(), "workflow wait cannot discover project-scoped GitHub access");
            defer_wait(&instances, &execution.id, &mut state, Some("project-scoped GitHub access is unavailable"))?;
            continue;
        };
        let pull_request = match parse_pull_request(&state.resource) {
            Ok(pull_request) => pull_request,
            Err(error) => {
                warn!(execution_id = execution.id, error = %error, "workflow wait pull-request resource is invalid");
                defer_wait(&instances, &execution.id, &mut state, Some(&error.to_string()))?;
                continue;
            }
        };
        if !access.owner.eq_ignore_ascii_case(&pull_request.owner)
            || !access.repo.eq_ignore_ascii_case(&pull_request.repo)
        {
            warn!(
                execution_id = execution.id,
                expected = access.repository(),
                actual = format!("{}/{}", pull_request.owner, pull_request.repo),
                "workflow wait resource does not belong to the bound agent's repository"
            );
            defer_wait(
                &instances,
                &execution.id,
                &mut state,
                Some("the pull request does not belong to the bound agent repository"),
            )?;
            continue;
        }

        let since = match parse_timestamp(&state.started_at) {
            Some(since) => since,
            None => {
                warn!(execution_id = execution.id, "workflow wait start timestamp is invalid");
                defer_wait(&instances, &execution.id, &mut state, Some("the wait cursor is invalid"))?;
                continue;
            }
        };
        match pull_request_activity(
            &access,
            pull_request.number,
            &state.event,
            since,
            state.after_cursor.as_deref(),
        )
        .await
        {
            Ok(Some(activity)) => {
                if let Err(error) = engine.resume_wait_execution(&execution.id, activity) {
                    warn!(execution_id = execution.id, error = %error, "failed to resume workflow wait");
                } else {
                    resumed += 1;
                }
            }
            Ok(None) => {
                defer_wait(&instances, &execution.id, &mut state, None)?;
            }
            Err(error) => {
                warn!(execution_id = execution.id, error = %error, "GitHub workflow wait poll failed");
                defer_wait(&instances, &execution.id, &mut state, Some(&error.to_string()))?;
            }
        }
    }

    Ok(resumed)
}

fn defer_wait(
    instances: &InstanceManager,
    execution_id: &str,
    state: &mut WaitState,
    error: Option<&str>,
) -> Result<()> {
    let now = Utc::now();
    let delay = state
        .poll_interval_seconds
        .clamp(MIN_POLL_INTERVAL_SECONDS, MAX_POLL_INTERVAL_SECONDS);
    state.last_checked_at = Some(now.to_rfc3339());
    state.last_error = error.map(str::to_string);
    state.next_poll_at = Some((now + chrono::Duration::seconds(delay as i64)).to_rfc3339());
    state.poll_interval_seconds = (delay.saturating_mul(2)).min(MAX_POLL_INTERVAL_SECONDS);
    let serialized = serde_json::to_string(state)
        .map_err(|error| Error::Workflow(format!("failed to persist wait cadence: {error}")))?;
    instances.update_wait_state(execution_id, &serialized)
}

async fn pull_request_activity(
    access: &github::GithubSessionAccess,
    number: u64,
    event: &str,
    since: DateTime<Utc>,
    after_cursor: Option<&str>,
) -> Result<Option<Value>> {
    let mut candidates = Vec::<(DateTime<Utc>, String, Value)>::new();

    if matches!(
        event,
        "github.pull_request.review" | "github.pull_request.activity"
    ) {
        let path = format!("pulls/{number}/reviews");
        for item in access.api_get_pages(&path).await? {
            push_activity(
                &mut candidates,
                &item,
                "submitted_at",
                "review",
                since,
                after_cursor,
            );
        }
    }

    if matches!(
        event,
        "github.pull_request.comment" | "github.pull_request.activity"
    ) {
        let path = format!("issues/{number}/comments");
        for item in access.api_get_pages(&path).await? {
            push_activity(
                &mut candidates,
                &item,
                "created_at",
                "conversation_comment",
                since,
                after_cursor,
            );
        }
        let path = format!("pulls/{number}/comments");
        for item in access.api_get_pages(&path).await? {
            push_activity(
                &mut candidates,
                &item,
                "created_at",
                "review_comment",
                since,
                after_cursor,
            );
        }
    }

    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    Ok(candidates
        .into_iter()
        .next()
        .map(|(_, _, value)| value))
}

fn push_activity(
    candidates: &mut Vec<(DateTime<Utc>, String, Value)>,
    item: &Value,
    timestamp_field: &str,
    kind: &str,
    since: DateTime<Utc>,
    after_cursor: Option<&str>,
) {
    let Some(timestamp) = item
        .get(timestamp_field)
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
    else {
        return;
    };
    let cursor = activity_cursor(kind, item);
    if timestamp < since
        || (timestamp == since && after_cursor.is_some_and(|after| cursor.as_str() <= after))
    {
        return;
    }
    candidates.push((
        timestamp,
        cursor.clone(),
        json!({
            "kind": kind,
            "id": item.get("id").cloned().unwrap_or(Value::Null),
            "url": item.get("html_url").cloned().unwrap_or(Value::Null),
            "author": item.pointer("/user/login").cloned().unwrap_or(Value::Null),
            "state": item.get("state").cloned().unwrap_or(Value::Null),
            "body": item.get("body").cloned().unwrap_or(Value::Null),
            "created_at": timestamp.to_rfc3339(),
            "cursor": cursor,
        }),
    ));
}

fn activity_cursor(kind: &str, item: &Value) -> String {
    activity_cursor_from_parts(kind, item.get("id").unwrap_or(&Value::Null))
}

pub(crate) fn activity_cursor_from_parts(kind: &str, id: &Value) -> String {
    let id = match id {
        Value::Number(number) => number
            .as_u64()
            .map(|number| format!("{number:020}"))
            .unwrap_or_else(|| number.to_string()),
        Value::String(value) => value.clone(),
        Value::Null => "unknown".into(),
        value => value.to_string(),
    };
    format!("{kind}:{id}")
}

fn parse_pull_request(value: &str) -> Result<PullRequestRef> {
    let value = value.trim().trim_end_matches('/');
    let path = value
        .strip_prefix("https://github.com/")
        .or_else(|| value.strip_prefix("http://github.com/"))
        .unwrap_or(value);
    let parts: Vec<&str> = path.split('/').collect();
    if let [owner, repo, "pull", number] = parts.as_slice() {
        let number = number.parse::<u64>().map_err(|_| {
            Error::Workflow(format!("invalid GitHub pull-request URL '{value}'"))
        })?;
        if !owner.is_empty() && !repo.is_empty() {
            return Ok(PullRequestRef {
                owner: (*owner).to_string(),
                repo: repo.trim_end_matches(".git").to_string(),
                number,
            });
        }
    }
    Err(Error::Workflow(format!(
        "wait resource must be a GitHub pull-request URL, got '{value}'"
    )))
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_pull_request_urls() {
        assert_eq!(
            parse_pull_request("https://github.com/XpressAI/xpressclaw/pull/143/").unwrap(),
            PullRequestRef {
                owner: "XpressAI".into(),
                repo: "xpressclaw".into(),
                number: 143,
            }
        );
        assert!(parse_pull_request("https://example.com/pull/143").is_err());
    }

    #[test]
    fn activity_filter_uses_wait_start_cursor() {
        let since = parse_timestamp("2026-08-02T12:00:00Z").unwrap();
        let mut candidates = Vec::new();
        push_activity(
            &mut candidates,
            &json!({"id": 1, "created_at": "2026-08-02T11:59:59Z"}),
            "created_at",
            "review_comment",
            since,
            None,
        );
        push_activity(
            &mut candidates,
            &json!({"id": 2, "created_at": "2026-08-02T12:00:01Z", "body": "Please add a test"}),
            "created_at",
            "review_comment",
            since,
            None,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].2["id"], 2);
        assert_eq!(candidates[0].2["kind"], "review_comment");
        let cursor = candidates[0].1.clone();

        let mut same_second = Vec::new();
        push_activity(
            &mut same_second,
            &json!({"id": 3, "created_at": "2026-08-02T12:00:01Z"}),
            "created_at",
            "review_comment",
            parse_timestamp("2026-08-02T12:00:01Z").unwrap(),
            Some(&cursor),
        );
        assert_eq!(same_second.len(), 1);
        assert_eq!(
            activity_cursor_from_parts("review_comment", &json!(3)),
            "review_comment:00000000000000000003"
        );
    }

    #[test]
    fn old_wait_state_gets_safe_polling_defaults() {
        let state: WaitState = serde_json::from_value(json!({
            "event": "github.pull_request.review",
            "resource": "https://github.com/XpressAI/xpressclaw/pull/143",
            "agent_id": "project-a",
            "started_at": "2026-08-02T12:00:00Z"
        }))
        .unwrap();
        assert_eq!(state.poll_interval_seconds, MIN_POLL_INTERVAL_SECONDS);
        assert!(state.after_cursor.is_none());
        assert!(state.next_poll_at.is_none());
        assert!(state.last_error.is_none());
    }
}
