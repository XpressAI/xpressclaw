//! Durable review lifecycle for pull requests published by ordinary tasks.
//!
//! The GitHub MCP registers a pull request with the task that created it. A
//! lightweight control-plane poller then wakes that same task for new review
//! feedback and keeps the agent's queue lane reserved until every registered
//! pull request is approved or merged. Workflow-owned tasks are deliberately
//! excluded because reusable workflows already model draft and wait steps
//! explicitly.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::sessions::SessionManager;
use crate::tasks::board::{TaskBoard, TaskStatus};
use crate::tasks::conversation::TaskConversation;
use crate::tasks::queue::TaskQueue;
use crate::workers::{github, native};

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
        let task = TaskBoard::new(self.db.clone()).get(task_id)?;
        if task.hidden
            || task
                .context
                .as_ref()
                .and_then(|context| context.get("origin"))
                .and_then(Value::as_str)
                == Some("workflow")
        {
            return Err(Error::Task(
                "workflow and hidden tasks manage pull-request waits explicitly".into(),
            ));
        }
        if matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled) {
            return Err(Error::Task(
                "cannot register a pull request for a finished task".into(),
            ));
        }
        if task.agent_id.as_deref() != Some(agent_id) {
            return Err(Error::Task(
                "the pull request was not created by this task's assigned agent".into(),
            ));
        }

        let pull_request = parse_pull_request(url)?;
        let expected = repository.trim().trim_end_matches(".git");
        let actual = format!("{}/{}", pull_request.owner, pull_request.repo);
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(Error::Task(format!(
                "pull request {actual} does not belong to the task repository {expected}"
            )));
        }

        let now = Utc::now();
        let started_at = (now - ChronoDuration::minutes(5)).to_rfc3339();
        let expires_at = (now + ChronoDuration::days(MONITOR_FOR_DAYS)).to_rfc3339();
        let next_poll_at = now.to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
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
                    pull_request.owner,
                    pull_request.repo,
                    pull_request.number as i64,
                    url,
                    started_at,
                    expires_at,
                    next_poll_at,
                    MIN_POLL_INTERVAL_SECONDS,
                ],
            )?;
            Ok::<_, Error>(())
        })?;
        TaskBoard::new(self.db.clone()).update_status(task_id, "in_progress", Some(agent_id))?;
        self.get(
            task_id,
            &pull_request.owner,
            &pull_request.repo,
            pull_request.number,
        )
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

    fn mark_terminal(&self, item: &TaskPullRequest, outcome: ReviewOutcome) -> Result<()> {
        let status = match outcome {
            ReviewOutcome::Approved => "approved",
            ReviewOutcome::Merged => "merged",
        };
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_pull_requests SET status = ?1, next_poll_at = NULL,
                    last_checked_at = ?2, last_error = NULL
                 WHERE task_id = ?3 AND owner = ?4 AND repo = ?5 AND number = ?6",
                rusqlite::params![
                    status,
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

    fn mark_attention(&self, item: &TaskPullRequest, reason: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_pull_requests SET status = 'attention', next_poll_at = NULL,
                    last_checked_at = ?1, last_error = ?2
                 WHERE task_id = ?3 AND owner = ?4 AND repo = ?5 AND number = ?6",
                rusqlite::params![
                    Utc::now().to_rfc3339(),
                    reason,
                    item.task_id,
                    item.owner,
                    item.repo,
                    item.number as i64,
                ],
            )?;
            Ok::<_, Error>(())
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
    for item in manager.waiting()? {
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
        if parse_timestamp(&item.expires_at).is_some_and(|expires| now >= expires) {
            let reason = format!(
                "XpressClaw monitored {} for {} days without approval or merge.",
                item.url, MONITOR_FOR_DAYS
            );
            require_user_attention(db, &manager, &item, &reason)?;
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
        let Some(access) = github::discover(db, &workspace) else {
            manager.defer(&item, Some("project-scoped GitHub access is unavailable"))?;
            continue;
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
                    manager.mark_terminal(&item, outcome)?;
                    finalize_if_satisfied(db, &manager, &item, outcome)?;
                    changes += 1;
                    continue;
                }
                if snapshot.closed_without_merge {
                    require_user_attention(
                        db,
                        &manager,
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

fn require_user_attention(
    db: &Arc<Database>,
    manager: &GithubReviewManager,
    item: &TaskPullRequest,
    reason: &str,
) -> Result<()> {
    manager.mark_attention(item, reason)?;
    TaskConversation::new(db.clone()).add_message(
        &item.task_id,
        "assistant",
        &format!("GitHub review monitoring needs your input. {reason}"),
    )?;
    TaskBoard::new(db.clone()).update_status(
        &item.task_id,
        "waiting_for_input",
        Some(&item.agent_id),
    )?;
    SessionManager::new(db.clone()).refresh_status(&item.agent_id)?;
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
        "Automated GitHub review follow-up for {}\n\n{}\n\nInspect the entire pull request, all unresolved review threads, conversation comments, requested changes, and CI—not just the activity quoted above. Address every actionable comment, run the relevant validation, commit and push the fixes, reply to reviewers, and resolve each thread once its fix is published. Keep the pull request ready for review. Do not mark this task complete while it awaits review; XpressClaw will continue monitoring until the pull request is approved or merged.",
        item.url, activity
    );
    TaskConversation::new(db.clone()).add_message(&item.task_id, "user", &message)?;
    TaskQueue::new(db.clone()).enqueue_continuation(&item.task_id, &item.agent_id)?;
    TaskBoard::new(db.clone()).update_status(&item.task_id, "in_progress", Some(&item.agent_id))?;
    SessionManager::new(db.clone()).refresh_status(&item.agent_id)?;
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
    manager: &GithubReviewManager,
    item: &TaskPullRequest,
    outcome: ReviewOutcome,
) -> Result<()> {
    if manager.gate(&item.task_id)? != GithubReviewGate::Satisfied {
        return Ok(());
    }
    let board = TaskBoard::new(db.clone());
    let task = board.get(&item.task_id)?;
    if matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled) {
        return Ok(());
    }
    let verb = match outcome {
        ReviewOutcome::Approved => "approved",
        ReviewOutcome::Merged => "merged",
    };
    TaskConversation::new(db.clone()).add_message(
        &item.task_id,
        "assistant",
        &format!(
            "GitHub review complete: all pull requests for this task are approved or merged ({} was {verb}).",
            item.url
        ),
    )?;
    board.complete_reported_subtasks(&item.task_id)?;
    let _ = board.complete_and_roll_up(&item.task_id, Some(&item.agent_id))?;
    SessionManager::new(db.clone()).refresh_status(&item.agent_id)?;
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

    let mut latest_review_by_author = HashMap::<String, (&str, DateTime<Utc>)>::new();
    let mut activities = Vec::new();
    for review in reviews {
        let reviewer = review
            .pointer("/user/login")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let submitted = value_timestamp(review, "submitted_at");
        match (review.get("state").and_then(Value::as_str), submitted) {
            (Some(state), Some(submitted)) if !reviewer.is_empty() => {
                let key = reviewer.to_ascii_lowercase();
                if latest_review_by_author
                    .get(&key)
                    .is_none_or(|(_, previous)| submitted >= *previous)
                {
                    latest_review_by_author.insert(key, (state, submitted));
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

    let formal_approval = latest_review_by_author
        .iter()
        .any(|(reviewer, (state, _))| {
            reviewer != &author && state.eq_ignore_ascii_case("approved")
        });
    let approval_comment = activities
        .iter()
        .any(|activity| is_approval_text(&activity.body));
    activities.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.cursor.cmp(&right.cursor))
    });

    ReviewSnapshot {
        outcome: if merged {
            Some(ReviewOutcome::Merged)
        } else if thumbs_up || formal_approval || approval_comment {
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
    let query = r#"query($owner:String!,$repo:String!,$number:Int!){
      repository(owner:$owner,name:$repo){
        pullRequest(number:$number){
          reviewThreads(first:100){nodes{id isResolved}}
        }
      }
    }"#;
    let response = access
        .graphql(
            query,
            json!({ "owner": access.owner, "repo": access.repo, "number": number }),
        )
        .await?;
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            return Err(Error::Backend(format!(
                "GitHub GraphQL returned errors: {}",
                Value::Array(errors.clone())
            )));
        }
    }
    Ok(response
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(Value::as_array)
        .map(|threads| {
            threads
                .iter()
                .filter(|thread| thread.get("isResolved").and_then(Value::as_bool) == Some(false))
                .count()
        })
        .unwrap_or(0))
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

    fn setup_task(context: Option<Value>) -> (Arc<Database>, String) {
        let db = Arc::new(Database::open_memory().unwrap());
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

        manager
            .mark_terminal(&registered, ReviewOutcome::Approved)
            .unwrap();
        assert_eq!(manager.gate(&task_id).unwrap(), GithubReviewGate::Satisfied);
        assert_eq!(
            TaskBoard::new(manager.db.clone())
                .complete_and_roll_up(&task_id, Some("project-codex"))
                .unwrap()
                .len(),
            1
        );
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
                "body": ""
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
