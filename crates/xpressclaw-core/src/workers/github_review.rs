//! Durable review lifecycle for pull requests published by ordinary tasks.
//!
//! The GitHub MCP registers a pull request with the task that created it. A
//! lightweight control-plane poller then wakes that same task for new review
//! feedback and keeps the agent's queue lane reserved until every registered
//! pull request is approved or merged. Workflow-owned tasks are deliberately
//! excluded because reusable workflows already model draft and wait steps
//! explicitly.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::{json, Value};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::sessions::SessionManager;
use crate::tasks::board::{TaskBoard, TaskStatus};
use crate::tasks::queue::TaskQueue;
use crate::workers::{github, native};
use crate::workflows::engine::WorkflowEngine;

const MIN_POLL_INTERVAL_SECONDS: i64 = 15;
const MAX_POLL_INTERVAL_SECONDS: i64 = 300;
const MONITOR_FOR_DAYS: i64 = 14;
const UNRESOLVED_REMINDER_SECONDS: i64 = 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubReviewGate {
    None,
    Waiting,
    Satisfied,
    NeedsInput,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TaskPullRequest {
    pub task_id: String,
    pub agent_id: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub status: String,
    pub started_at: String,
    pub expires_at: String,
    pub next_poll_at: Option<String>,
    pub poll_interval_seconds: i64,
    pub last_checked_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub last_feedback_at: Option<String>,
    pub after_cursor: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PullRequestRef {
    owner: String,
    repo: String,
    number: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewOutcome {
    Approved,
    Merged,
}

#[derive(Debug, Clone)]
struct ReviewActivity {
    at: DateTime<Utc>,
    cursor: String,
    kind: String,
    author: String,
    body: String,
    url: Option<String>,
    state: Option<String>,
}

#[derive(Debug, Clone)]
struct ReviewSnapshot {
    outcome: Option<ReviewOutcome>,
    closed_without_merge: bool,
    activities: Vec<ReviewActivity>,
    unresolved_threads: usize,
}

pub struct GithubReviewManager {
    db: Arc<Database>,
}

impl GithubReviewManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Arm a fail-closed completion gate before the GitHub command publishes
    /// or readies a pull request. The real PR registration atomically replaces
    /// this sentinel; if that follow-up request is interrupted, the task stays
    /// waiting for input instead of completing with an unmonitored PR.
    pub fn begin_registration(
        &self,
        task_id: &str,
        agent_id: &str,
        repository: &str,
        registration_id: &str,
        registration_key: &str,
    ) -> Result<(String, bool)> {
        validate_registration_id(registration_id)?;
        validate_registration_key(registration_key)?;
        let (owner, repo) = parse_repository(repository)?;
        let now = Utc::now();
        self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            activate_registration_task_in_connection(&transaction, task_id, agent_id)?;
            let existing = transaction
                .query_row(
                    "SELECT url, registration_key FROM task_pull_requests
                     WHERE task_id = ?1 AND owner = ?2 AND repo = ?3 AND number = 0",
                    rusqlite::params![task_id, owner, repo],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()?;
            if let Some((existing_id, existing_key)) = existing {
                if existing_key.as_deref() != Some(registration_key) {
                    return Err(Error::Task(
                        "another pull-request publication is still awaiting durable registration; retry that pull request before publishing a different one"
                            .into(),
                    ));
                }
                transaction.commit()?;
                return Ok((existing_id, true));
            }
            transaction.execute(
                "INSERT INTO task_pull_requests
                    (task_id, agent_id, owner, repo, number, url, status,
                     started_at, expires_at, next_poll_at, poll_interval_seconds, last_error,
                     registration_key)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, 'attention', ?6, ?7, NULL, ?8, ?9, ?10)",
                rusqlite::params![
                    task_id,
                    agent_id,
                    owner,
                    repo,
                    registration_id,
                    now.to_rfc3339(),
                    (now + ChronoDuration::days(MONITOR_FOR_DAYS)).to_rfc3339(),
                    MIN_POLL_INTERVAL_SECONDS,
                    "pull-request publication began, but durable registration has not completed",
                    registration_key,
                ],
            )?;
            transaction.commit()?;
            Ok::<_, Error>((registration_id.to_string(), false))
        })
    }

    /// Remove a pre-publication sentinel when the GitHub command itself
    /// failed, so a command that created no PR does not hold the task open.
    pub fn cancel_registration(
        &self,
        task_id: &str,
        agent_id: &str,
        repository: &str,
        registration_id: &str,
        registration_key: &str,
    ) -> Result<()> {
        self.validate_registration_task(task_id, agent_id)?;
        validate_registration_id(registration_id)?;
        validate_registration_key(registration_key)?;
        let (owner, repo) = parse_repository(repository)?;
        self.db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM task_pull_requests
                 WHERE task_id = ?1 AND agent_id = ?2 AND owner = ?3 AND repo = ?4
                   AND number = 0 AND url = ?5 AND registration_key = ?6",
                rusqlite::params![
                    task_id,
                    agent_id,
                    owner,
                    repo,
                    registration_id,
                    registration_key,
                ],
            )?;
            Ok::<_, Error>(())
        })
    }

    /// Register a PR published through XpressClaw's bundled GitHub MCP.
    /// Registration is idempotent and re-arms a PR that previously needed
    /// attention (for example after the agent reopened it).
    pub fn register(
        &self,
        task_id: &str,
        agent_id: &str,
        repository: &str,
        url: &str,
    ) -> Result<TaskPullRequest> {
        self.register_with_registration_id(task_id, agent_id, repository, url, None)
    }

    /// Replace the sentinel armed by one specific managed GitHub command with
    /// its published pull request. A different command's sentinel must remain
    /// fail-closed until that command registers or cancels itself.
    pub fn register_pending(
        &self,
        task_id: &str,
        agent_id: &str,
        repository: &str,
        url: &str,
        registration_id: &str,
        registration_key: &str,
    ) -> Result<TaskPullRequest> {
        validate_registration_id(registration_id)?;
        validate_registration_key(registration_key)?;
        self.register_with_registration_id(
            task_id,
            agent_id,
            repository,
            url,
            Some((registration_id, registration_key)),
        )
    }

    fn register_with_registration_id(
        &self,
        task_id: &str,
        agent_id: &str,
        repository: &str,
        url: &str,
        registration: Option<(&str, &str)>,
    ) -> Result<TaskPullRequest> {
        let pull_request = parse_pull_request(url)?;
        let (expected_owner, expected_repo) = parse_repository(repository)?;
        if !pull_request.owner.eq_ignore_ascii_case(&expected_owner)
            || !pull_request.repo.eq_ignore_ascii_case(&expected_repo)
        {
            let actual = format!("{}/{}", pull_request.owner, pull_request.repo);
            let expected = format!("{expected_owner}/{expected_repo}");
            return Err(Error::Task(format!(
                "pull request {actual} does not belong to the task repository {expected}"
            )));
        }

        let now = Utc::now();
        let started_at = (now - ChronoDuration::minutes(5)).to_rfc3339();
        let expires_at = (now + ChronoDuration::days(MONITOR_FOR_DAYS)).to_rfc3339();
        let next_poll_at = now.to_rfc3339();
        self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            activate_registration_task_in_connection(&transaction, task_id, agent_id)?;
            if let Some((registration_id, registration_key)) = registration {
                let sentinel_exists = transaction.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM task_pull_requests
                        WHERE task_id = ?1 AND agent_id = ?2 AND owner = ?3 AND repo = ?4
                          AND number = 0 AND url = ?5 AND registration_key = ?6
                     )",
                    rusqlite::params![
                        task_id,
                        agent_id,
                        expected_owner,
                        expected_repo,
                        registration_id,
                        registration_key,
                    ],
                    |row| row.get::<_, bool>(0),
                )?;
                if !sentinel_exists {
                    return Err(Error::Task(
                        "pull-request registration no longer matches its durable publication gate; begin the retry before registering it"
                            .into(),
                    ));
                }
            }
            transaction.execute(
                "INSERT INTO task_pull_requests
                    (task_id, agent_id, owner, repo, number, url, status,
                     started_at, expires_at, next_poll_at, poll_interval_seconds)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'waiting', ?7, ?8, ?9, ?10)
                 ON CONFLICT(task_id, owner, repo, number) DO UPDATE SET
                    agent_id = excluded.agent_id,
                    url = excluded.url,
                    status = CASE
                        WHEN task_pull_requests.status IN ('approved', 'merged')
                            THEN task_pull_requests.status
                        ELSE 'waiting'
                    END,
                    expires_at = CASE
                        WHEN task_pull_requests.status IN ('approved', 'merged')
                            THEN task_pull_requests.expires_at
                        ELSE excluded.expires_at
                    END,
                    next_poll_at = CASE
                        WHEN task_pull_requests.status IN ('approved', 'merged')
                            THEN NULL
                        ELSE excluded.next_poll_at
                    END,
                    poll_interval_seconds = ?10,
                    last_error = NULL",
                rusqlite::params![
                    task_id,
                    agent_id,
                    expected_owner,
                    expected_repo,
                    pull_request.number as i64,
                    url,
                    started_at,
                    expires_at,
                    next_poll_at,
                    MIN_POLL_INTERVAL_SECONDS,
                ],
            )?;
            if let Some((registration_id, registration_key)) = registration {
                let deleted = transaction.execute(
                    "DELETE FROM task_pull_requests
                     WHERE task_id = ?1 AND agent_id = ?2 AND owner = ?3 AND repo = ?4
                       AND number = 0 AND url = ?5 AND registration_key = ?6",
                    rusqlite::params![
                        task_id,
                        agent_id,
                        expected_owner,
                        expected_repo,
                        registration_id,
                        registration_key,
                    ],
                )?;
                if deleted != 1 {
                    return Err(Error::Task(
                        "pull-request publication gate changed before registration completed"
                            .into(),
                    ));
                }
            }
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;
        self.get(
            task_id,
            &expected_owner,
            &expected_repo,
            pull_request.number,
        )
    }

    fn validate_registration_task(&self, task_id: &str, agent_id: &str) -> Result<()> {
        self.db
            .with_conn(|conn| validate_registration_task_in_connection(conn, task_id, agent_id))
    }

    pub fn gate(&self, task_id: &str) -> Result<GithubReviewGate> {
        self.db.with_conn(|conn| {
            let (total, waiting, attention, satisfied): (i64, i64, i64, i64) = conn.query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN status = 'waiting' THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN status = 'attention' THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN status IN ('approved', 'merged') THEN 1 ELSE 0 END), 0)
                 FROM task_pull_requests WHERE task_id = ?1 AND status != 'cancelled'",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            Ok(if total == 0 {
                GithubReviewGate::None
            } else if attention > 0 {
                GithubReviewGate::NeedsInput
            } else if waiting > 0 {
                GithubReviewGate::Waiting
            } else if satisfied == total {
                GithubReviewGate::Satisfied
            } else {
                GithubReviewGate::NeedsInput
            })
        })
    }

    fn get(&self, task_id: &str, owner: &str, repo: &str, number: u64) -> Result<TaskPullRequest> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT task_id, agent_id, owner, repo, number, url, status,
                        started_at, expires_at, next_poll_at, poll_interval_seconds,
                        last_checked_at, last_activity_at, last_feedback_at,
                        after_cursor, last_error
                 FROM task_pull_requests
                 WHERE task_id = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4",
                rusqlite::params![task_id, owner, repo, number as i64],
                row_to_pull_request,
            )
            .map_err(Error::from)
        })
    }

    fn waiting(&self) -> Result<Vec<TaskPullRequest>> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT task_id, agent_id, owner, repo, number, url, status,
                        started_at, expires_at, next_poll_at, poll_interval_seconds,
                        last_checked_at, last_activity_at, last_feedback_at,
                        after_cursor, last_error
                 FROM task_pull_requests WHERE status = 'waiting'",
            )?;
            let items = statement
                .query_map([], row_to_pull_request)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Error::from)?;
            Ok(items)
        })
    }

    /// Refresh a possibly stale polling snapshot from the task's current
    /// assignment before any queue or session work is attributed to an agent.
    fn synchronize_assignment(&self, item: &TaskPullRequest) -> Result<Option<String>> {
        self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let agent_id: Option<String> = transaction.query_row(
                "SELECT agent_id FROM tasks WHERE id = ?1",
                [&item.task_id],
                |row| row.get(0),
            )?;
            let agent_id = agent_id.filter(|agent_id| !agent_id.trim().is_empty());
            if agent_id.as_deref() != Some(&item.agent_id) {
                if let Some(ref agent_id) = agent_id {
                    transaction.execute(
                        "UPDATE task_pull_requests SET agent_id = ?1
                         WHERE task_id = ?2 AND owner = ?3 AND repo = ?4 AND number = ?5
                           AND status IN ('waiting', 'attention')",
                        rusqlite::params![
                            agent_id,
                            item.task_id,
                            item.owner,
                            item.repo,
                            item.number as i64,
                        ],
                    )?;
                }
            }
            transaction.commit()?;
            Ok(agent_id)
        })
    }

    fn cancel(&self, item: &TaskPullRequest) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_pull_requests SET status = 'cancelled', next_poll_at = NULL,
                    last_checked_at = ?1, last_error = NULL
                 WHERE task_id = ?2 AND owner = ?3 AND repo = ?4 AND number = ?5",
                rusqlite::params![
                    Utc::now().to_rfc3339(),
                    item.task_id,
                    item.owner,
                    item.repo,
                    item.number as i64,
                ],
            )?;
            Ok::<_, Error>(())
        })
    }

    fn record_feedback(
        &self,
        item: &TaskPullRequest,
        activity: Option<&ReviewActivity>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_pull_requests SET
                    last_activity_at = COALESCE(?1, last_activity_at),
                    after_cursor = COALESCE(?2, after_cursor),
                    last_feedback_at = ?3,
                    next_poll_at = ?3,
                    poll_interval_seconds = ?4,
                    last_checked_at = ?3,
                    last_error = NULL
                 WHERE task_id = ?5 AND owner = ?6 AND repo = ?7 AND number = ?8",
                rusqlite::params![
                    activity.map(|activity| activity.at.to_rfc3339()),
                    activity.map(|activity| activity.cursor.as_str()),
                    now,
                    MIN_POLL_INTERVAL_SECONDS,
                    item.task_id,
                    item.owner,
                    item.repo,
                    item.number as i64,
                ],
            )?;
            Ok::<_, Error>(())
        })
    }

    fn defer(&self, item: &TaskPullRequest, message: Option<&str>) -> Result<()> {
        let now = Utc::now();
        let delay = item
            .poll_interval_seconds
            .clamp(MIN_POLL_INTERVAL_SECONDS, MAX_POLL_INTERVAL_SECONDS);
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_pull_requests SET next_poll_at = ?1,
                    poll_interval_seconds = ?2, last_checked_at = ?3, last_error = ?4
                 WHERE task_id = ?5 AND owner = ?6 AND repo = ?7 AND number = ?8",
                rusqlite::params![
                    (now + ChronoDuration::seconds(delay)).to_rfc3339(),
                    (delay * 2).min(MAX_POLL_INTERVAL_SECONDS),
                    now.to_rfc3339(),
                    message,
                    item.task_id,
                    item.owner,
                    item.repo,
                    item.number as i64,
                ],
            )?;
            Ok::<_, Error>(())
        })
    }
}

fn validate_registration_task_in_connection(
    conn: &rusqlite::Connection,
    task_id: &str,
    agent_id: &str,
) -> Result<()> {
    let task = conn
        .query_row(
            "SELECT agent_id, status, hidden, context FROM tasks WHERE id = ?1",
            [task_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::TaskNotFound {
            id: task_id.to_string(),
        })?;
    let (assigned_agent, status, hidden, context) = task;
    let workflow_owned = context
        .as_deref()
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| {
            value
                .get("origin")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some("workflow");
    if hidden != 0 || workflow_owned {
        return Err(Error::Task(
            "workflow and hidden tasks manage pull-request waits explicitly".into(),
        ));
    }
    if matches!(status.as_str(), "completed" | "cancelled") {
        return Err(Error::Task(
            "cannot register a pull request for a finished task".into(),
        ));
    }
    if assigned_agent.as_deref() != Some(agent_id) {
        return Err(Error::Task(
            "the pull request was not created by this task's assigned agent".into(),
        ));
    }
    Ok(())
}

fn activate_registration_task_in_connection(
    conn: &rusqlite::Connection,
    task_id: &str,
    agent_id: &str,
) -> Result<()> {
    validate_registration_task_in_connection(conn, task_id, agent_id)?;
    let changed = conn.execute(
        "UPDATE tasks SET status = 'in_progress', completed_at = NULL,
            updated_at = CURRENT_TIMESTAMP
         WHERE id = ?1 AND agent_id = ?2
           AND status NOT IN ('completed', 'cancelled')",
        rusqlite::params![task_id, agent_id],
    )?;
    if changed != 1 {
        return Err(Error::Task(
            "cannot register a pull request because the task finished or changed assignment".into(),
        ));
    }
    Ok(())
}

fn row_to_pull_request(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskPullRequest> {
    Ok(TaskPullRequest {
        task_id: row.get(0)?,
        agent_id: row.get(1)?,
        owner: row.get(2)?,
        repo: row.get(3)?,
        number: row.get::<_, i64>(4)? as u64,
        url: row.get(5)?,
        status: row.get(6)?,
        started_at: row.get(7)?,
        expires_at: row.get(8)?,
        next_poll_at: row.get(9)?,
        poll_interval_seconds: row.get(10)?,
        last_checked_at: row.get(11)?,
        last_activity_at: row.get(12)?,
        last_feedback_at: row.get(13)?,
        after_cursor: row.get(14)?,
        last_error: row.get(15)?,
    })
}

/// Poll registered task PRs until shutdown. Failures remain durable and are
/// retried with the same bounded cadence as workflow event waits.
pub async fn start_review_runner(db: Arc<Database>, config: Arc<RwLock<Arc<Config>>>) {
    info!("GitHub task review runner started");
    loop {
        let current_config = config.read().expect("config lock poisoned").clone();
        if let Err(error) = poll_reviews_once(&db, &current_config).await {
            error!(error = %error, "GitHub task review check failed");
        }
        tokio::time::sleep(std::time::Duration::from_secs(
            MIN_POLL_INTERVAL_SECONDS as u64,
        ))
        .await;
    }
}

pub async fn poll_reviews_once(db: &Arc<Database>, config: &Config) -> Result<u32> {
    let manager = GithubReviewManager::new(db.clone());
    let mut changes = 0;
    let now = Utc::now();
    for mut item in manager.waiting()? {
        if item
            .next_poll_at
            .as_deref()
            .and_then(parse_timestamp)
            .is_some_and(|next| next > now)
        {
            continue;
        }
        let task = match TaskBoard::new(db.clone()).get(&item.task_id) {
            Ok(task) => task,
            Err(error) => {
                warn!(task_id = item.task_id, error = %error, "review task no longer exists");
                continue;
            }
        };
        if matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled) {
            manager.cancel(&item)?;
            continue;
        }
        let Some(agent_id) = manager.synchronize_assignment(&item)? else {
            manager.defer(&item, Some("the task no longer has an assigned agent"))?;
            continue;
        };
        item.agent_id = agent_id;
        if parse_timestamp(&item.expires_at).is_some_and(|expires| now >= expires) {
            let reason = format!(
                "XpressClaw monitored {} for {} days without approval or merge.",
                item.url, MONITOR_FOR_DAYS
            );
            require_user_attention(db, &item, &reason)?;
            changes += 1;
            continue;
        }

        let Some(agent) = config
            .agents
            .iter()
            .find(|agent| agent.name == item.agent_id)
        else {
            manager.defer(&item, Some("the assigned agent is no longer configured"))?;
            continue;
        };
        let workspace = native::resolved_workspace(config, agent);
        let access = match crate::repositories::discover_active_github_access(
            db,
            &agent.name,
            &workspace,
        )
        .await
        {
            Ok(Some(access)) => access,
            Ok(None) => {
                manager.defer(&item, Some("project-scoped GitHub access is unavailable"))?;
                continue;
            }
            Err(error) => {
                warn!(task_id = item.task_id, agent_id = item.agent_id, workspace = %workspace.display(), %error, "review repository is unavailable");
                manager.defer(
                    &item,
                    Some("the assigned Agent workspace or repository is unavailable"),
                )?;
                continue;
            }
        };
        if !access.owner.eq_ignore_ascii_case(&item.owner)
            || !access.repo.eq_ignore_ascii_case(&item.repo)
        {
            manager.defer(
                &item,
                Some("the registered pull request no longer matches the agent repository"),
            )?;
            continue;
        }

        match fetch_snapshot(&access, &item).await {
            Ok(snapshot) => {
                if let Some(outcome) = snapshot.outcome {
                    finalize_if_satisfied(db, &item, outcome)?;
                    changes += 1;
                    continue;
                }
                if snapshot.closed_without_merge {
                    require_user_attention(
                        db,
                        &item,
                        &format!("{} was closed without being merged or approved.", item.url),
                    )?;
                    changes += 1;
                    continue;
                }

                let latest = latest_new_activity(&item, &snapshot.activities);
                let reminder_due = snapshot.unresolved_threads > 0
                    && item
                        .last_feedback_at
                        .as_deref()
                        .and_then(parse_timestamp)
                        .map(|last| {
                            now - last >= ChronoDuration::seconds(UNRESOLVED_REMINDER_SECONDS)
                        })
                        .unwrap_or(true)
                    && !task_has_dispatch_in_flight(db, &item.task_id)?;
                if latest.is_some() || reminder_due {
                    enqueue_review_follow_up(db, &item, latest, snapshot.unresolved_threads)?;
                    manager.record_feedback(&item, latest)?;
                    changes += 1;
                } else {
                    manager.defer(&item, None)?;
                }
            }
            Err(error) => {
                warn!(task_id = item.task_id, pull_request = item.url, error = %error, "GitHub task review poll failed");
                manager.defer(&item, Some(&error.to_string()))?;
            }
        }
    }
    Ok(changes)
}

fn require_user_attention(db: &Arc<Database>, item: &TaskPullRequest, reason: &str) -> Result<()> {
    let agent_id = db.with_conn(|conn| {
        let transaction = conn.unchecked_transaction()?;
        let (agent_id, task_status): (Option<String>, String) = transaction.query_row(
            "SELECT agent_id, status FROM tasks WHERE id = ?1",
            [&item.task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if matches!(task_status.as_str(), "completed" | "cancelled") {
            transaction.execute(
                "UPDATE task_pull_requests SET status = 'cancelled', next_poll_at = NULL,
                    last_checked_at = ?1, last_error = NULL
                 WHERE task_id = ?2 AND owner = ?3 AND repo = ?4 AND number = ?5
                   AND status IN ('waiting', 'attention')",
                rusqlite::params![
                    Utc::now().to_rfc3339(),
                    item.task_id,
                    item.owner,
                    item.repo,
                    item.number as i64,
                ],
            )?;
            transaction.commit()?;
            return Ok::<_, Error>(None);
        }
        let agent_id = agent_id
            .filter(|agent_id| !agent_id.trim().is_empty())
            .ok_or_else(|| Error::Task("review task no longer has an assigned agent".into()))?;
        let changed = transaction.execute(
            "UPDATE task_pull_requests SET agent_id = ?1, status = 'attention',
                next_poll_at = NULL, last_checked_at = ?2, last_error = ?3
             WHERE task_id = ?4 AND owner = ?5 AND repo = ?6 AND number = ?7
               AND status = 'waiting'",
            rusqlite::params![
                agent_id,
                Utc::now().to_rfc3339(),
                reason,
                item.task_id,
                item.owner,
                item.repo,
                item.number as i64,
            ],
        )?;
        if changed == 0 {
            transaction.commit()?;
            return Ok(None);
        }
        transaction.execute(
            "INSERT INTO task_messages (task_id, role, content) VALUES (?1, 'assistant', ?2)",
            rusqlite::params![
                item.task_id,
                format!("GitHub review monitoring needs your input. {reason}"),
            ],
        )?;
        transaction.execute(
            "UPDATE tasks SET status = 'waiting_for_input', completed_at = NULL,
                updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            [&item.task_id],
        )?;
        transaction.commit()?;
        Ok(Some(agent_id))
    })?;
    if let Some(agent_id) = agent_id {
        SessionManager::new(db.clone()).refresh_status(&agent_id)?;
    }
    Ok(())
}

fn enqueue_review_follow_up(
    db: &Arc<Database>,
    item: &TaskPullRequest,
    latest: Option<&ReviewActivity>,
    unresolved_threads: usize,
) -> Result<()> {
    let activity = latest.map(format_activity).unwrap_or_else(|| {
        format!("GitHub still reports {unresolved_threads} unresolved review thread(s).")
    });
    let message = format!(
        "Automated GitHub review follow-up for {}\n\n{}\n\nInspect the entire pull request, all unresolved review threads, conversation comments, requested changes, and CI—not just the activity quoted above. Address every actionable comment, run the relevant validation, commit and push the fixes, reply to reviewers, and resolve each thread once its fix is published. Keep the pull request ready for review. If the configured automated reviewer requires an explicit re-review request after fixes, send it (for example, comment `@codex review`). Do not mark this task complete while it awaits review; XpressClaw will continue monitoring until the pull request is approved or merged.",
        item.url, activity
    );
    let Some((agent_id, _)) = TaskQueue::new(db.clone())
        .enqueue_review_follow_up_for_current_agent(&item.task_id, &message)?
    else {
        return Ok(());
    };
    SessionManager::new(db.clone()).refresh_status(&agent_id)?;
    Ok(())
}

fn format_activity(activity: &ReviewActivity) -> String {
    let body = if activity.body.trim().is_empty() {
        "(no body)".to_string()
    } else {
        truncate(activity.body.trim(), 1_500)
    };
    let state = activity
        .state
        .as_deref()
        .map(|state| format!(" [{state}]"))
        .unwrap_or_default();
    let url = activity
        .url
        .as_deref()
        .map(|url| format!("\n{url}"))
        .unwrap_or_default();
    format!(
        "Newest external {} from @{}{}:\n{}{}",
        activity.kind, activity.author, state, body, url
    )
}

fn finalize_if_satisfied(
    db: &Arc<Database>,
    item: &TaskPullRequest,
    outcome: ReviewOutcome,
) -> Result<()> {
    let status = match outcome {
        ReviewOutcome::Approved => "approved",
        ReviewOutcome::Merged => "merged",
    };
    let verb = match outcome {
        ReviewOutcome::Approved => "approved",
        ReviewOutcome::Merged => "merged",
    };
    let message = format!(
        "GitHub review complete: all pull requests for this task are approved or merged ({} was {verb}).",
        item.url
    );
    let completion = db.with_conn(|conn| {
        let transaction = conn.unchecked_transaction()?;
        let task = transaction
            .query_row(
                "SELECT status, agent_id, parent_task_id FROM tasks WHERE id = ?1",
                [&item.task_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((task_status, agent_id, parent_task_id)) = task else {
            transaction.commit()?;
            return Ok::<_, Error>(None);
        };
        if matches!(task_status.as_str(), "completed" | "cancelled") {
            transaction.execute(
                "UPDATE task_pull_requests SET status = 'cancelled', next_poll_at = NULL,
                    last_checked_at = ?1, last_error = NULL
                 WHERE task_id = ?2 AND owner = ?3 AND repo = ?4 AND number = ?5
                   AND status IN ('waiting', 'attention')",
                rusqlite::params![
                    Utc::now().to_rfc3339(),
                    item.task_id,
                    item.owner,
                    item.repo,
                    item.number as i64,
                ],
            )?;
            transaction.commit()?;
            return Ok(None);
        }

        transaction.execute(
            "UPDATE task_pull_requests SET status = ?1, next_poll_at = NULL,
                last_checked_at = ?2, last_error = NULL
             WHERE task_id = ?3 AND owner = ?4 AND repo = ?5 AND number = ?6
               AND status = 'waiting'",
            rusqlite::params![
                status,
                Utc::now().to_rfc3339(),
                item.task_id,
                item.owner,
                item.repo,
                item.number as i64,
            ],
        )?;
        let item_satisfied: bool = transaction.query_row(
            "SELECT status IN ('approved', 'merged') FROM task_pull_requests
             WHERE task_id = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4",
            rusqlite::params![item.task_id, item.owner, item.repo, item.number as i64],
            |row| row.get(0),
        )?;
        let gate_satisfied: bool = transaction.query_row(
            "SELECT NOT EXISTS(
                SELECT 1 FROM task_pull_requests
                WHERE task_id = ?1 AND status NOT IN ('approved', 'merged', 'cancelled')
             )",
            [&item.task_id],
            |row| row.get(0),
        )?;
        if !item_satisfied || !gate_satisfied {
            transaction.commit()?;
            return Ok(None);
        }

        let completed_at = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        transaction.execute(
            "UPDATE tasks SET status = 'cancelled', updated_at = ?1, completed_at = NULL,
                context = json_set(
                    COALESCE(context, '{}'),
                    '$.plan_disposition', 'deferred',
                    '$.resolved_reason', 'review_gate_satisfied',
                    '$.resolved_at', ?1
                )
             WHERE parent_task_id = ?2 AND provenance = 'native_plan'
               AND status NOT IN ('completed', 'cancelled')",
            rusqlite::params![completed_at, item.task_id],
        )?;
        let subtasks_complete: bool = transaction.query_row(
            "SELECT NOT EXISTS(
                SELECT 1 FROM tasks
                WHERE parent_task_id = ?1 AND blocks_parent = 1
                  AND status != 'completed'
             )",
            [&item.task_id],
            |row| row.get(0),
        )?;
        let has_open_attempt: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM work_attempts WHERE task_id = ?1
                AND status IN ('queued', 'preparing', 'running', 'waiting_for_input', 'review')
             )",
            [&item.task_id],
            |row| row.get(0),
        )?;
        if !subtasks_complete || has_open_attempt {
            transaction.commit()?;
            return Ok(None);
        }

        transaction.execute(
            "INSERT INTO task_messages (task_id, role, content) VALUES (?1, 'assistant', ?2)",
            rusqlite::params![item.task_id, message],
        )?;
        let changed = transaction.execute(
            "UPDATE tasks SET status = 'completed', updated_at = ?1, completed_at = ?1
             WHERE id = ?2 AND status NOT IN ('completed', 'cancelled')",
            rusqlite::params![completed_at, item.task_id],
        )?;
        if changed != 1 {
            return Err(Error::Task(
                "review completion lost ownership of the task status transition".into(),
            ));
        }
        transaction.commit()?;
        Ok(Some((agent_id, parent_task_id)))
    })?;
    let Some((agent_id, parent_task_id)) = completion else {
        return Ok(());
    };
    let board = TaskBoard::new(db.clone());
    let conversation = crate::tasks::conversation::TaskConversation::new(db.clone());
    let workflows = WorkflowEngine::new(db.clone());
    let advance_completed_workflow = |task_id: &str| {
        let output = conversation
            .get_messages(task_id)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .find(|message| message.role == "assistant")
            .map(|message| message.content)
            .unwrap_or_else(|| message.clone());
        if let Err(error) = workflows.on_task_completed(task_id, "completed", &output) {
            warn!(
                task_id,
                error = %error,
                "failed to advance workflow after GitHub review completion"
            );
        }
    };
    advance_completed_workflow(&item.task_id);
    if board.get(&item.task_id)?.status == crate::tasks::board::TaskStatus::Completed {
        if let Some(parent_task_id) = parent_task_id {
            board.complete_and_roll_up_with(&parent_task_id, None, |completed| {
                advance_completed_workflow(&completed.id);
                Ok(())
            })?;
        }
    }
    if let Some(agent_id) = agent_id.filter(|agent_id| !agent_id.trim().is_empty()) {
        SessionManager::new(db.clone()).refresh_status(&agent_id)?;
    }
    Ok(())
}

async fn fetch_snapshot(
    access: &github::GithubSessionAccess,
    item: &TaskPullRequest,
) -> Result<ReviewSnapshot> {
    let number = item.number;
    let pull_path = format!("pulls/{number}");
    let reactions_path = format!("issues/{number}/reactions");
    let reviews_path = format!("pulls/{number}/reviews");
    let conversation_comments_path = format!("issues/{number}/comments");
    let review_comments_path = format!("pulls/{number}/comments");
    let (pull_request, reactions, reviews, conversation_comments, review_comments) = tokio::try_join!(
        access.api_get(&pull_path),
        access.api_get_pages(&reactions_path),
        access.api_get_pages(&reviews_path),
        access.api_get_pages(&conversation_comments_path),
        access.api_get_pages(&review_comments_path),
    )?;
    let unresolved_threads = fetch_unresolved_thread_count(access, number)
        .await
        .unwrap_or_else(|error| {
            warn!(pull_request = item.url, error = %error, "could not inspect unresolved GitHub review threads");
            0
        });
    Ok(review_snapshot_from_values(
        &pull_request,
        &reactions,
        &reviews,
        &conversation_comments,
        &review_comments,
        unresolved_threads,
    ))
}

fn review_snapshot_from_values(
    pull_request: &Value,
    reactions: &[Value],
    reviews: &[Value],
    conversation_comments: &[Value],
    review_comments: &[Value],
    unresolved_threads: usize,
) -> ReviewSnapshot {
    let author = pull_request
        .pointer("/user/login")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let merged = pull_request
        .get("merged_at")
        .is_some_and(|value| !value.is_null());
    let closed = pull_request.get("state").and_then(Value::as_str) == Some("closed");
    let thumbs_up = reactions.iter().any(|reaction| {
        reaction.get("content").and_then(Value::as_str) == Some("+1")
            && reaction
                .pointer("/user/login")
                .and_then(Value::as_str)
                .is_some_and(|reactor| {
                    !reactor.is_empty() && !reactor.eq_ignore_ascii_case(&author)
                })
    });

    let mut latest_review_by_author = HashMap::<String, (&str, DateTime<Utc>, &str)>::new();
    let mut activities = Vec::new();
    for review in reviews {
        let reviewer = review
            .pointer("/user/login")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let submitted = value_timestamp(review, "submitted_at");
        let body = review
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match (review.get("state").and_then(Value::as_str), submitted) {
            (Some(state), Some(submitted)) if !reviewer.is_empty() => {
                let key = reviewer.to_ascii_lowercase();
                if latest_review_by_author
                    .get(&key)
                    .is_none_or(|(_, previous, _)| submitted >= *previous)
                {
                    latest_review_by_author.insert(key, (state, submitted, body));
                }
            }
            _ => {}
        }
        push_activity(&mut activities, review, "submitted_at", "review", &author);
    }
    for comment in conversation_comments {
        push_activity(
            &mut activities,
            comment,
            "updated_at",
            "conversation comment",
            &author,
        );
    }
    for comment in review_comments {
        push_activity(
            &mut activities,
            comment,
            "updated_at",
            "review comment",
            &author,
        );
    }

    let latest_review_approval =
        latest_review_by_author
            .iter()
            .any(|(reviewer, (state, _, body))| {
                reviewer != &author
                    && (state.eq_ignore_ascii_case("approved")
                        || (state.eq_ignore_ascii_case("commented") && is_approval_text(body)))
            });
    let approval_comment = activities
        .iter()
        .any(|activity| activity.kind != "review" && is_approval_text(&activity.body));
    activities.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.cursor.cmp(&right.cursor))
    });

    ReviewSnapshot {
        outcome: if merged {
            Some(ReviewOutcome::Merged)
        } else if thumbs_up || latest_review_approval || approval_comment {
            Some(ReviewOutcome::Approved)
        } else {
            None
        },
        closed_without_merge: closed && !merged,
        activities,
        unresolved_threads,
    }
}

async fn fetch_unresolved_thread_count(
    access: &github::GithubSessionAccess,
    number: u64,
) -> Result<usize> {
    let query = r#"query($owner:String!,$repo:String!,$number:Int!,$after:String){
      repository(owner:$owner,name:$repo){
        pullRequest(number:$number){
          reviewThreads(first:100,after:$after){
            nodes{id isResolved}
            pageInfo{hasNextPage endCursor}
          }
        }
      }
    }"#;
    let mut after = None;
    let mut seen_cursors = HashSet::new();
    let mut unresolved = 0;
    loop {
        let response = access
            .graphql(
                query,
                json!({
                    "owner": &access.owner,
                    "repo": &access.repo,
                    "number": number,
                    "after": after.clone(),
                }),
            )
            .await?;
        let (page_unresolved, next_cursor) = unresolved_thread_page(&response)?;
        unresolved += page_unresolved;
        let Some(next_cursor) = next_cursor else {
            return Ok(unresolved);
        };
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(Error::Backend(
                "GitHub returned a repeated review-thread page cursor".into(),
            ));
        }
        after = Some(next_cursor);
    }
}

fn unresolved_thread_page(response: &Value) -> Result<(usize, Option<String>)> {
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            return Err(Error::Backend(format!(
                "GitHub GraphQL returned errors: {}",
                Value::Array(errors.clone())
            )));
        }
    }
    let connection = response
        .pointer("/data/repository/pullRequest/reviewThreads")
        .ok_or_else(|| Error::Backend("GitHub omitted the pull-request review threads".into()))?;
    let unresolved = connection
        .get("nodes")
        .and_then(Value::as_array)
        .map(|threads| {
            threads
                .iter()
                .filter(|thread| thread.get("isResolved").and_then(Value::as_bool) == Some(false))
                .count()
        })
        .ok_or_else(|| Error::Backend("GitHub returned invalid review-thread nodes".into()))?;
    let has_next_page = connection
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::Backend("GitHub omitted review-thread page information".into()))?;
    let next_cursor = if has_next_page {
        Some(
            connection
                .pointer("/pageInfo/endCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .ok_or_else(|| {
                    Error::Backend(
                        "GitHub reported another review-thread page without a cursor".into(),
                    )
                })?
                .to_string(),
        )
    } else {
        None
    };
    Ok((unresolved, next_cursor))
}

fn push_activity(
    activities: &mut Vec<ReviewActivity>,
    value: &Value,
    timestamp_field: &str,
    kind: &str,
    pull_request_author: &str,
) {
    let author = value
        .pointer("/user/login")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if author.is_empty() || author.eq_ignore_ascii_case(pull_request_author) {
        return;
    }
    let Some(at) = value_timestamp(value, timestamp_field) else {
        return;
    };
    let id = value
        .get("id")
        .map(|id| match id {
            Value::Number(number) => number
                .as_u64()
                .map(|number| format!("{number:020}"))
                .unwrap_or_else(|| number.to_string()),
            Value::String(value) => value.clone(),
            value => value.to_string(),
        })
        .unwrap_or_else(|| "unknown".into());
    activities.push(ReviewActivity {
        at,
        cursor: format!("{}:{id}", kind.replace(' ', "_")),
        kind: kind.into(),
        author: author.into(),
        body: value
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        url: value
            .get("html_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        state: value
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_owned),
    });
}

fn latest_new_activity<'a>(
    item: &TaskPullRequest,
    activities: &'a [ReviewActivity],
) -> Option<&'a ReviewActivity> {
    let boundary = item
        .last_activity_at
        .as_deref()
        .and_then(parse_timestamp)
        .or_else(|| parse_timestamp(&item.started_at))?;
    activities
        .iter()
        .filter(|activity| {
            activity.at > boundary
                || (activity.at == boundary
                    && item
                        .after_cursor
                        .as_deref()
                        .is_none_or(|cursor| activity.cursor.as_str() > cursor))
        })
        .max_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then_with(|| left.cursor.cmp(&right.cursor))
        })
}

fn is_approval_text(body: &str) -> bool {
    let trimmed = body.trim();
    if matches!(
        trimmed,
        "+1" | ":+1:" | "👍" | "👍🏻" | "👍🏼" | "👍🏽" | "👍🏾" | "👍🏿"
    ) {
        return true;
    }
    let normalized = trimmed
        .trim_matches(|character: char| !character.is_alphanumeric())
        .to_ascii_lowercase();
    matches!(normalized.as_str(), "lgtm" | "approved")
}

fn validate_registration_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(Error::Task(
            "pull-request registration ID must contain only ASCII letters, numbers, '-' or '_'"
                .into(),
        ));
    }
    Ok(())
}

fn validate_registration_key(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Error::Task(
            "pull-request registration key must be a non-empty hexadecimal value".into(),
        ));
    }
    Ok(())
}

fn parse_repository(value: &str) -> Result<(String, String)> {
    let value = value.trim().trim_end_matches('/').trim_end_matches(".git");
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(Error::Task(format!(
            "expected a GitHub repository in owner/repo form, got '{value}'"
        )));
    }
    Ok((owner.to_string(), repo.to_string()))
}

fn parse_pull_request(value: &str) -> Result<PullRequestRef> {
    let value = value.trim().trim_end_matches('/');
    let path = value
        .strip_prefix("https://github.com/")
        .or_else(|| value.strip_prefix("http://github.com/"))
        .ok_or_else(|| Error::Task(format!("invalid GitHub pull-request URL '{value}'")))?;
    let parts = path.split('/').collect::<Vec<_>>();
    if let [owner, repo, "pull", number] = parts.as_slice() {
        let number = number
            .parse::<u64>()
            .map_err(|_| Error::Task(format!("invalid GitHub pull-request URL '{value}'")))?;
        if !owner.is_empty() && !repo.is_empty() {
            return Ok(PullRequestRef {
                owner: (*owner).to_string(),
                repo: repo.trim_end_matches(".git").to_string(),
                number,
            });
        }
    }
    Err(Error::Task(format!(
        "expected a GitHub pull-request URL, got '{value}'"
    )))
}

fn value_timestamp(value: &Value, field: &str) -> Option<DateTime<Utc>> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn task_has_dispatch_in_flight(db: &Arc<Database>, task_id: &str) -> Result<bool> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM task_queue WHERE task_id = ?1 AND status IN ('queued', 'running')
             )",
            [task_id],
            |row| row.get(0),
        )
        .map_err(Error::from)
    })
}

fn truncate(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::board::CreateTask;
    use crate::tasks::conversation::TaskConversation;

    const REGISTRATION_KEY_A: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REGISTRATION_KEY_B: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn setup_task(context: Option<Value>) -> (Arc<Database>, String) {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO agents (id, name, backend, config) VALUES
                    ('project-codex', 'Project Codex', 'native', '{}'),
                    ('reviewer-codex', 'Reviewer Codex', 'native', '{}');",
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            .ensure("project-codex", Some("Project"))
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Publish a PR".into(),
                description: None,
                agent_id: Some("project-codex".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context,
            })
            .unwrap();
        (db, task.id)
    }

    #[test]
    fn registers_ordinary_task_pr_and_gates_completion() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db);
        let registered = manager
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap();
        assert_eq!(registered.number, 151);
        assert_eq!(registered.status, "waiting");
        assert_eq!(manager.gate(&task_id).unwrap(), GithubReviewGate::Waiting);
        assert!(TaskBoard::new(manager.db.clone())
            .complete_and_roll_up(&task_id, Some("project-codex"))
            .unwrap()
            .is_empty());

        finalize_if_satisfied(&manager.db, &registered, ReviewOutcome::Approved).unwrap();
        assert_eq!(manager.gate(&task_id).unwrap(), GithubReviewGate::Satisfied);
        assert_eq!(
            TaskBoard::new(manager.db.clone())
                .get(&task_id)
                .unwrap()
                .status,
            TaskStatus::Completed
        );
        let messages = TaskConversation::new(manager.db.clone())
            .get_messages(&task_id)
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.contains("GitHub review complete"));
    }

    #[test]
    fn approval_advances_child_default_before_rolling_up_parent() {
        let (db, parent_task_id) = setup_task(None);
        let child = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Publish the reviewed change".into(),
                agent_id: Some("project-codex".into()),
                parent_task_id: Some(parent_task_id.clone()),
                ..Default::default()
            })
            .unwrap();
        let workflow = crate::workflows::manager::WorkflowManager::new(db.clone())
            .create(&crate::workflows::manager::CreateWorkflow {
                name: "post-review-check".into(),
                description: None,
                yaml_content: r#"
name: post-review-check
flows:
  main:
    steps:
      - id: verify_result
        type: continue
        prompt: Verify the completed task one more time.
"#
                .into(),
            })
            .unwrap();
        crate::workflows::manager::WorkflowManager::new(db.clone())
            .set_default_for_tasks(&workflow.id, true)
            .unwrap();
        let workflows = WorkflowEngine::new(db.clone());
        let parent_instance = workflows
            .attach_default_workflows_to_task(&parent_task_id)
            .unwrap()[0]
            .clone();
        let child_instance = workflows
            .attach_default_workflows_to_task(&child.id)
            .unwrap()[0]
            .clone();

        let manager = GithubReviewManager::new(db.clone());
        let registered = manager
            .register(
                &child.id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap();
        finalize_if_satisfied(&db, &registered, ReviewOutcome::Approved).unwrap();

        assert_eq!(
            TaskBoard::new(db.clone()).get(&child.id).unwrap().status,
            TaskStatus::Pending
        );
        assert_eq!(
            TaskBoard::new(db.clone())
                .get(&parent_task_id)
                .unwrap()
                .status,
            TaskStatus::Pending
        );
        assert!(!TaskBoard::new(db.clone())
            .subtasks_complete(&parent_task_id)
            .unwrap());
        let child_messages = TaskConversation::new(db.clone())
            .get_messages(&child.id)
            .unwrap();
        assert_eq!(
            child_messages
                .iter()
                .filter(|message| message.role == "user"
                    && message.content == "Verify the completed task one more time.")
                .count(),
            1
        );
        assert!(TaskConversation::new(db.clone())
            .get_messages(&parent_task_id)
            .unwrap()
            .is_empty());
        assert_eq!(
            TaskQueue::new(db.clone())
                .pending_count("project-codex")
                .unwrap(),
            1
        );

        let execution_statuses = |instance_id: &str| {
            db.with_conn(|conn| {
                let mut statement = conn.prepare(
                    "SELECT step_id, status FROM workflow_step_executions
                     WHERE instance_id = ?1 ORDER BY rowid",
                )?;
                let values = statement
                    .query_map([instance_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok::<_, Error>(values)
            })
            .unwrap()
        };
        assert_eq!(
            execution_statuses(&child_instance),
            vec![
                ("__source_task__".to_string(), "completed".to_string()),
                ("verify_result".to_string(), "running".to_string()),
            ]
        );
        assert_eq!(
            execution_statuses(&parent_instance),
            vec![("__source_task__".to_string(), "running".to_string())]
        );
    }

    #[test]
    fn stale_polling_snapshot_refreshes_to_the_tasks_current_agent() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db.clone());
        let stale = manager
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap();
        TaskBoard::new(db)
            .update_with_agent_repository(
                &task_id,
                &crate::tasks::board::UpdateTask {
                    title: None,
                    description: None,
                    agent_id: Some("reviewer-codex".into()),
                    priority: None,
                },
                Some(("XpressAI", "xpressclaw")),
            )
            .unwrap();

        assert_eq!(stale.agent_id, "project-codex");
        assert_eq!(
            manager.synchronize_assignment(&stale).unwrap().as_deref(),
            Some("reviewer-codex")
        );
        assert_eq!(
            manager
                .get(&task_id, "XpressAI", "xpressclaw", 151)
                .unwrap()
                .agent_id,
            "reviewer-codex"
        );
    }

    #[test]
    fn review_follow_up_uses_assignment_current_at_enqueue_time() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db.clone());
        let stale = manager
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap();
        TaskBoard::new(db.clone())
            .update_with_agent_repository(
                &task_id,
                &crate::tasks::board::UpdateTask {
                    title: None,
                    description: None,
                    agent_id: Some("reviewer-codex".into()),
                    priority: None,
                },
                Some(("XpressAI", "xpressclaw")),
            )
            .unwrap();

        enqueue_review_follow_up(&db, &stale, None, 1).unwrap();

        let queue = TaskQueue::new(db.clone());
        let queued = queue.claim("reviewer-codex").unwrap().unwrap();
        assert_eq!(queued.task_id, task_id);
        assert!(queue.claim("project-codex").unwrap().is_none());
        assert_eq!(
            SessionManager::new(db)
                .get_attempt(queued.attempt_id.as_deref().unwrap())
                .unwrap()
                .session_id,
            "reviewer-codex"
        );
    }

    #[test]
    fn review_follow_up_does_not_reopen_a_task_cancelled_during_github_io() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db.clone());
        let stale = manager
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap();

        // This models cancellation after polling captured `stale` but before
        // the GitHub request returned and attempted to enqueue its feedback.
        TaskBoard::new(db.clone())
            .update_status(&task_id, "cancelled", None)
            .unwrap();
        enqueue_review_follow_up(&db, &stale, None, 1).unwrap();

        assert_eq!(
            TaskBoard::new(db.clone()).get(&task_id).unwrap().status,
            TaskStatus::Cancelled
        );
        assert!(!TaskQueue::new(db.clone())
            .has_queued_for_task(&task_id)
            .unwrap());
        assert!(TaskConversation::new(db)
            .get_messages(&task_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn review_attention_does_not_reopen_a_task_cancelled_during_github_io() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db.clone());
        let stale = manager
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap();

        TaskBoard::new(db.clone())
            .update_status(&task_id, "cancelled", None)
            .unwrap();
        require_user_attention(&db, &stale, "The pull request was closed.").unwrap();

        assert_eq!(
            TaskBoard::new(db.clone()).get(&task_id).unwrap().status,
            TaskStatus::Cancelled
        );
        assert_eq!(
            manager
                .get(&task_id, "XpressAI", "xpressclaw", 151)
                .unwrap()
                .status,
            "cancelled"
        );
        assert!(TaskConversation::new(db)
            .get_messages(&task_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn review_attention_atomically_records_the_reason_and_waiting_state() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db.clone());
        let item = manager
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap();

        require_user_attention(&db, &item, "The pull request was closed.").unwrap();

        assert_eq!(
            TaskBoard::new(db.clone()).get(&task_id).unwrap().status,
            TaskStatus::WaitingForInput
        );
        assert_eq!(
            manager
                .get(&task_id, "XpressAI", "xpressclaw", 151)
                .unwrap()
                .status,
            "attention"
        );
        let messages = TaskConversation::new(db).get_messages(&task_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        assert!(messages[0].content.contains("The pull request was closed."));
    }

    #[test]
    fn task_cancellation_atomically_retires_an_attention_monitor() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db.clone());
        let item = manager
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap();
        require_user_attention(&db, &item, "The pull request was closed.").unwrap();

        TaskBoard::new(db.clone())
            .update_status(&task_id, "cancelled", None)
            .unwrap();

        assert_eq!(
            manager
                .get(&task_id, "XpressAI", "xpressclaw", 151)
                .unwrap()
                .status,
            "cancelled"
        );
        assert_eq!(manager.gate(&task_id).unwrap(), GithubReviewGate::None);
    }

    #[test]
    fn approval_finalization_does_not_mutate_a_cancelled_task_or_plan() {
        let (db, task_id) = setup_task(None);
        let board = TaskBoard::new(db.clone());
        let manager = GithubReviewManager::new(db.clone());
        let item = manager
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap();
        let plan_step = board
            .sync_reported_subtasks(
                &task_id,
                "attempt-1",
                &[crate::tasks::board::ReportedSubtask {
                    title: "Run the tests".into(),
                    status: TaskStatus::Pending,
                }],
            )
            .unwrap();
        let plan_step = plan_step
            .into_iter()
            .find(|task| task.is_native_plan_item())
            .unwrap();

        // Models cancellation after the poll captured `item` but before the
        // approval response returned from GitHub.
        board.update_status(&task_id, "cancelled", None).unwrap();
        finalize_if_satisfied(&db, &item, ReviewOutcome::Approved).unwrap();

        assert_eq!(board.get(&task_id).unwrap().status, TaskStatus::Cancelled);
        assert_eq!(
            board.get(&plan_step.id).unwrap().status,
            TaskStatus::Pending
        );
        assert_eq!(
            manager
                .get(&task_id, "XpressAI", "xpressclaw", 151)
                .unwrap()
                .status,
            "cancelled"
        );
        assert!(TaskConversation::new(db)
            .get_messages(&task_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn registration_does_not_reopen_or_persist_for_a_cancelled_task() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db.clone());
        TaskBoard::new(db.clone())
            .update_status(&task_id, "cancelled", None)
            .unwrap();

        let begin_error = manager
            .begin_registration(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "registration-1",
                REGISTRATION_KEY_A,
            )
            .unwrap_err();
        assert!(begin_error.to_string().contains("finished task"));
        let register_error = manager
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap_err();
        assert!(register_error.to_string().contains("finished task"));

        assert_eq!(
            TaskBoard::new(db.clone()).get(&task_id).unwrap().status,
            TaskStatus::Cancelled
        );
        let registrations: i64 = db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM task_pull_requests WHERE task_id = ?1",
                [&task_id],
                |row| row.get(0),
            )
            .unwrap()
        });
        assert_eq!(registrations, 0);
    }

    #[test]
    fn prepublication_gate_blocks_completion_until_registration_is_durable() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db.clone());
        manager
            .begin_registration(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "registration-1",
                REGISTRATION_KEY_A,
            )
            .unwrap();
        assert_eq!(
            manager.gate(&task_id).unwrap(),
            GithubReviewGate::NeedsInput
        );
        assert!(TaskBoard::new(db.clone())
            .complete_and_roll_up(&task_id, Some("project-codex"))
            .unwrap()
            .is_empty());

        manager
            .register_pending(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
                "registration-1",
                REGISTRATION_KEY_A,
            )
            .unwrap();
        assert_eq!(manager.gate(&task_id).unwrap(), GithubReviewGate::Waiting);
        let sentinel_count: i64 = db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM task_pull_requests WHERE task_id = ?1 AND number = 0",
                [&task_id],
                |row| row.get(0),
            )
            .unwrap()
        });
        assert_eq!(sentinel_count, 0);
    }

    #[test]
    fn retry_reuses_only_its_matching_registration_sentinel() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db.clone());
        let (original_id, reused) = manager
            .begin_registration(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "registration-a",
                REGISTRATION_KEY_A,
            )
            .unwrap();
        assert_eq!(original_id, "registration-a");
        assert!(!reused);

        let (retry_id, reused) = manager
            .begin_registration(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "registration-b",
                REGISTRATION_KEY_A,
            )
            .unwrap();
        assert_eq!(retry_id, "registration-a");
        assert!(reused);

        let unrelated_error = manager
            .begin_registration(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "registration-c",
                REGISTRATION_KEY_B,
            )
            .unwrap_err();
        assert!(unrelated_error
            .to_string()
            .contains("another pull-request publication"));

        let mismatched_error = manager
            .register_pending(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/152",
                "registration-b",
                REGISTRATION_KEY_A,
            )
            .unwrap_err();
        assert!(mismatched_error
            .to_string()
            .contains("no longer matches its durable publication gate"));
        assert_eq!(
            manager.gate(&task_id).unwrap(),
            GithubReviewGate::NeedsInput
        );

        manager
            .register_pending(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
                &retry_id,
                REGISTRATION_KEY_A,
            )
            .unwrap();
        assert_eq!(manager.gate(&task_id).unwrap(), GithubReviewGate::Waiting);
        let monitored: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM task_pull_requests WHERE task_id = ?1 AND number > 0",
                    [&task_id],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(monitored, 1);
    }

    #[test]
    fn failed_github_command_can_cancel_prepublication_gate() {
        let (db, task_id) = setup_task(None);
        let manager = GithubReviewManager::new(db);
        manager
            .begin_registration(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "registration-1",
                REGISTRATION_KEY_A,
            )
            .unwrap();
        manager
            .cancel_registration(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "registration-2",
                REGISTRATION_KEY_A,
            )
            .unwrap();
        assert_eq!(
            manager.gate(&task_id).unwrap(),
            GithubReviewGate::NeedsInput
        );
        manager
            .cancel_registration(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "registration-1",
                REGISTRATION_KEY_A,
            )
            .unwrap();
        assert_eq!(manager.gate(&task_id).unwrap(), GithubReviewGate::None);
    }

    #[test]
    fn tasks_without_pull_requests_have_no_review_gate() {
        let (db, task_id) = setup_task(None);
        assert_eq!(
            GithubReviewManager::new(db).gate(&task_id).unwrap(),
            GithubReviewGate::None
        );
    }

    #[test]
    fn workflow_task_cannot_opt_into_implicit_pr_lifecycle() {
        let (db, task_id) = setup_task(Some(json!({ "origin": "workflow" })));
        let error = GithubReviewManager::new(db)
            .register(
                &task_id,
                "project-codex",
                "XpressAI/xpressclaw",
                "https://github.com/XpressAI/xpressclaw/pull/151",
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("manage pull-request waits explicitly"));
    }

    #[test]
    fn recognizes_only_unambiguous_approval_messages() {
        for body in ["+1", ":+1:", "LGTM", "LGTM!", "approved.", "👍🏽"] {
            assert!(is_approval_text(body), "{body}");
        }
        for body in [
            "I will approve after this changes",
            "not approved",
            "LGTM except for one thing",
            "please add +1 test",
        ] {
            assert!(!is_approval_text(body), "{body}");
        }
    }

    #[test]
    fn classifies_every_supported_github_approval_signal() {
        let open_pr = json!({
            "state": "open",
            "merged_at": null,
            "user": { "login": "author" }
        });
        let approved_review = json!({
            "id": 1,
            "state": "APPROVED",
            "submitted_at": "2026-08-10T12:00:00Z",
            "user": { "login": "reviewer" },
            "body": ""
        });
        assert_eq!(
            review_snapshot_from_values(&open_pr, &[], &[approved_review], &[], &[], 0,).outcome,
            Some(ReviewOutcome::Approved)
        );

        let approval_review_comment = json!({
            "id": 2,
            "state": "COMMENTED",
            "submitted_at": "2026-08-10T12:00:00Z",
            "user": { "login": "reviewer" },
            "body": "LGTM!"
        });
        assert_eq!(
            review_snapshot_from_values(&open_pr, &[], &[approval_review_comment], &[], &[], 0,)
                .outcome,
            Some(ReviewOutcome::Approved)
        );

        let reaction = json!({
            "content": "+1",
            "user": { "login": "reviewer" }
        });
        assert_eq!(
            review_snapshot_from_values(&open_pr, &[reaction], &[], &[], &[], 0).outcome,
            Some(ReviewOutcome::Approved)
        );

        let lgtm = json!({
            "id": 2,
            "updated_at": "2026-08-10T12:00:00Z",
            "user": { "login": "reviewer" },
            "body": "LGTM!"
        });
        assert_eq!(
            review_snapshot_from_values(&open_pr, &[], &[], &[lgtm], &[], 0).outcome,
            Some(ReviewOutcome::Approved)
        );

        let merged_pr = json!({
            "state": "closed",
            "merged_at": "2026-08-10T12:01:00Z",
            "user": { "login": "author" }
        });
        assert_eq!(
            review_snapshot_from_values(&merged_pr, &[], &[], &[], &[], 0).outcome,
            Some(ReviewOutcome::Merged)
        );
    }

    #[test]
    fn ignores_own_approval_words_and_uses_each_reviewers_latest_state() {
        let pull_request = json!({
            "state": "open",
            "merged_at": null,
            "user": { "login": "author" }
        });
        let reviews = vec![
            json!({
                "id": 1,
                "state": "APPROVED",
                "submitted_at": "2026-08-10T12:00:00Z",
                "user": { "login": "reviewer" },
                "body": "LGTM"
            }),
            json!({
                "id": 2,
                "state": "CHANGES_REQUESTED",
                "submitted_at": "2026-08-10T12:01:00Z",
                "user": { "login": "reviewer" },
                "body": "Please fix this"
            }),
        ];
        let own_lgtm = json!({
            "id": 3,
            "updated_at": "2026-08-10T12:02:00Z",
            "user": { "login": "author" },
            "body": "LGTM"
        });
        let own_reaction = json!({
            "content": "+1",
            "user": { "login": "AUTHOR" }
        });
        let snapshot = review_snapshot_from_values(
            &pull_request,
            &[own_reaction],
            &reviews,
            &[own_lgtm],
            &[],
            1,
        );
        assert_eq!(snapshot.outcome, None);
        assert_eq!(snapshot.unresolved_threads, 1);
        assert_eq!(snapshot.activities.len(), 2);
    }

    #[test]
    fn parses_review_thread_pages_and_requires_a_next_cursor() {
        let first_page = json!({
            "data": { "repository": { "pullRequest": { "reviewThreads": {
                "nodes": [
                    { "id": "one", "isResolved": false },
                    { "id": "two", "isResolved": true }
                ],
                "pageInfo": { "hasNextPage": true, "endCursor": "cursor-1" }
            }}}}
        });
        assert_eq!(
            unresolved_thread_page(&first_page).unwrap(),
            (1, Some("cursor-1".into()))
        );

        let final_page = json!({
            "data": { "repository": { "pullRequest": { "reviewThreads": {
                "nodes": [{ "id": "three", "isResolved": false }],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }}}}
        });
        assert_eq!(unresolved_thread_page(&final_page).unwrap(), (1, None));

        let missing_cursor = json!({
            "data": { "repository": { "pullRequest": { "reviewThreads": {
                "nodes": [],
                "pageInfo": { "hasNextPage": true, "endCursor": null }
            }}}}
        });
        assert!(unresolved_thread_page(&missing_cursor)
            .unwrap_err()
            .to_string()
            .contains("without a cursor"));
    }

    #[test]
    fn activity_cursor_ignores_pr_author_and_delivers_edits() {
        let mut activities = Vec::new();
        push_activity(
            &mut activities,
            &json!({
                "id": 1,
                "updated_at": "2026-08-10T12:00:00Z",
                "user": { "login": "author" },
                "body": "my own reply"
            }),
            "updated_at",
            "review comment",
            "author",
        );
        push_activity(
            &mut activities,
            &json!({
                "id": 2,
                "updated_at": "2026-08-10T12:00:01Z",
                "user": { "login": "reviewer" },
                "body": "please add a test"
            }),
            "updated_at",
            "review comment",
            "author",
        );
        assert_eq!(activities.len(), 1);
        assert_eq!(activities[0].author, "reviewer");

        let item = TaskPullRequest {
            task_id: "task".into(),
            agent_id: "agent".into(),
            owner: "owner".into(),
            repo: "repo".into(),
            number: 1,
            url: "https://github.com/owner/repo/pull/1".into(),
            status: "waiting".into(),
            started_at: "2026-08-10T12:00:00Z".into(),
            expires_at: "2026-08-24T12:00:00Z".into(),
            next_poll_at: None,
            poll_interval_seconds: 15,
            last_checked_at: None,
            last_activity_at: None,
            last_feedback_at: None,
            after_cursor: None,
            last_error: None,
        };
        assert_eq!(
            latest_new_activity(&item, &activities).unwrap().body,
            "please add a test"
        );
    }

    #[test]
    fn parses_only_exact_github_pull_request_urls() {
        assert_eq!(
            parse_pull_request("https://github.com/XpressAI/xpressclaw/pull/151/").unwrap(),
            PullRequestRef {
                owner: "XpressAI".into(),
                repo: "xpressclaw".into(),
                number: 151,
            }
        );
        assert!(parse_pull_request("https://example.com/XpressAI/xpressclaw/pull/151").is_err());
        assert!(parse_pull_request("https://github.com/XpressAI/xpressclaw/issues/151").is_err());
    }
}
