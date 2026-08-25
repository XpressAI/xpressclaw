//! Durable, instance-wide telemetry for the Control center dashboard.
//!
//! The dashboard intentionally stores only normalized counters and short,
//! display-safe summaries. The source tables remain authoritative; raw ACP
//! payloads, terminal output, prompts, tool arguments, and Git diffs are never
//! copied into dashboard telemetry.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::Database;
use crate::error::{Error, Result};

const EVENT_RETENTION_DAYS: i64 = 8;
const EVENT_RETENTION_ROWS: i64 = 20_000;
const GIT_SNAPSHOT_DEBOUNCE_SECONDS: i64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardRange {
    Hour,
    Day,
    Week,
}

impl DashboardRange {
    pub const fn seconds(self) -> i64 {
        match self {
            Self::Hour => 60 * 60,
            Self::Day => 24 * 60 * 60,
            Self::Week => 7 * 24 * 60 * 60,
        }
    }

    const fn bucket_seconds(self) -> i64 {
        match self {
            Self::Hour => 5 * 60,
            Self::Day => 60 * 60,
            Self::Week => 6 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DashboardFilter {
    pub project_id: Option<String>,
    pub range: DashboardRange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardProject {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardCounters {
    pub working_agents: i64,
    pub active_work: i64,
    pub needs_attention: i64,
    pub tool_calls: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardSeriesPoint {
    pub timestamp: String,
    /// ACP context-window occupancy. This is deliberately not called tokens.
    pub context_used: i64,
    pub context_size: i64,
    pub tool_calls: i64,
    pub code_additions: i64,
    pub code_deletions: i64,
    pub git_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardActiveWork {
    pub work_kind: String,
    pub work_id: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub agent_id: String,
    pub agent_name: String,
    pub target_type: String,
    pub target_id: String,
    pub target_title: String,
    pub href: String,
    pub phase: String,
    pub queued_at: String,
    pub started_at: Option<String>,
    pub activity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardEvent {
    pub cursor: i64,
    pub event_id: String,
    pub event_kind: String,
    pub occurred_at: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub source_kind: String,
    pub source_label: String,
    pub target_type: String,
    pub target_id: String,
    pub target_title: String,
    pub href: String,
    pub severity: String,
    pub needs_attention: bool,
    pub preview: String,
    pub work_kind: Option<String>,
    pub work_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardAttentionItem {
    pub id: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub target_type: String,
    pub target_id: String,
    pub target_title: String,
    pub href: String,
    pub summary: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardFeedPage {
    pub events: Vec<DashboardEvent>,
    pub next_before: Option<i64>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardSnapshot {
    pub generated_at: String,
    pub cursor: i64,
    pub projects: Vec<DashboardProject>,
    pub counters: DashboardCounters,
    pub series: Vec<DashboardSeriesPoint>,
    pub active_work: Vec<DashboardActiveWork>,
    pub attention: Vec<DashboardAttentionItem>,
    pub feed: DashboardFeedPage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardReplay {
    pub events: Vec<DashboardEvent>,
    pub latest_cursor: i64,
    pub reset_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitMetricResult {
    pub state: String,
    pub detail: Option<String>,
    pub additions: Option<i64>,
    pub deletions: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GitFileStat {
    additions: i64,
    deletions: i64,
}

#[derive(Debug)]
struct GitSnapshot {
    files: HashMap<String, GitFileStat>,
    baseline_ref: Option<String>,
    state: String,
    detail: Option<String>,
}

#[derive(Debug)]
struct MetricRow {
    work_key: String,
    bucket_at: String,
    context_used: Option<i64>,
    context_size: Option<i64>,
    tool_calls: i64,
    code_additions: Option<i64>,
    code_deletions: Option<i64>,
    git_state: String,
}

struct GitMetricSample<'a> {
    work_kind: &'a str,
    work_id: &'a str,
    project_id: Option<&'a str>,
    agent_id: Option<&'a str>,
    observed_at: &'a DateTime<Utc>,
    additions: Option<i64>,
    deletions: Option<i64>,
    state: &'a str,
    detail: Option<&'a str>,
}

pub struct DashboardManager {
    db: Arc<Database>,
}

impl DashboardManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn snapshot(&self, filter: &DashboardFilter, feed_limit: i64) -> Result<DashboardSnapshot> {
        self.validate_project(filter.project_id.as_deref())?;
        self.prune()?;
        let generated_at = now_string();
        let cursor = self.latest_cursor()?;
        Ok(DashboardSnapshot {
            generated_at,
            cursor,
            projects: self.projects()?,
            counters: self.counters(filter)?,
            series: self.series(filter)?,
            active_work: self.active_work(filter.project_id.as_deref())?,
            attention: self.attention(filter.project_id.as_deref())?,
            feed: self.feed(filter, None, feed_limit)?,
        })
    }

    pub fn feed(
        &self,
        filter: &DashboardFilter,
        before: Option<i64>,
        limit: i64,
    ) -> Result<DashboardFeedPage> {
        self.validate_project(filter.project_id.as_deref())?;
        let limit = limit.clamp(1, 100);
        let since = Utc::now() - Duration::seconds(filter.range.seconds());
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "WITH ranked AS (
                    SELECT de.*,
                           row_number() OVER (
                               PARTITION BY de.event_id ORDER BY de.cursor DESC
                           ) AS event_version
                    FROM dashboard_events de
                    WHERE de.occurred_at >= ?1
                      AND (?2 IS NULL OR de.project_id = ?2)
                 )
                 SELECT cursor, event_id, event_kind, occurred_at,
                        project_id, project_name, agent_id, agent_name,
                        source_kind, source_label, target_type, target_id,
                        target_title, href, severity, needs_attention, preview,
                        work_kind, work_id
                 FROM ranked
                 WHERE event_version = 1 AND (?3 IS NULL OR cursor < ?3)
                 ORDER BY cursor DESC
                 LIMIT ?4",
            )?;
            let mut events = statement
                .query_map(
                    params![
                        timestamp_string(since),
                        filter.project_id,
                        before,
                        limit + 1
                    ],
                    row_to_event,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let has_more = events.len() > limit as usize;
            if has_more {
                events.pop();
            }
            let next_before = events.last().map(|event| event.cursor);
            Ok(DashboardFeedPage {
                events,
                next_before,
                has_more,
            })
        })
    }

    /// Return new durable rows in cursor order. Unlike initial feed pages,
    /// message versions are not collapsed here: the client advances through
    /// every cursor and replaces matching stable event IDs.
    pub fn replay_after(
        &self,
        filter: &DashboardFilter,
        after: i64,
        limit: i64,
    ) -> Result<DashboardReplay> {
        self.validate_project(filter.project_id.as_deref())?;
        let limit = limit.clamp(1, 250);
        let since = Utc::now() - Duration::seconds(filter.range.seconds());
        self.db.with_conn(|conn| {
            let (oldest_cursor, latest_cursor) = conn.query_row(
                "SELECT COALESCE(MIN(cursor), 0), COALESCE(MAX(cursor), 0)
                 FROM dashboard_events",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )?;
            let reset_required = after > 0
                && (after > latest_cursor || (oldest_cursor > 0 && after < oldest_cursor - 1));
            if reset_required {
                return Ok(DashboardReplay {
                    events: Vec::new(),
                    latest_cursor,
                    reset_required,
                });
            }
            let mut statement = conn.prepare(
                "SELECT cursor, event_id, event_kind, occurred_at,
                        project_id, project_name, agent_id, agent_name,
                        source_kind, source_label, target_type, target_id,
                        target_title, href, severity, needs_attention, preview,
                        work_kind, work_id
                 FROM dashboard_events
                 WHERE cursor > ?1
                   AND occurred_at >= ?2
                   AND (?3 IS NULL OR project_id = ?3)
                 ORDER BY cursor ASC LIMIT ?4",
            )?;
            let events = statement
                .query_map(
                    params![after, timestamp_string(since), filter.project_id, limit],
                    row_to_event,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(DashboardReplay {
                events,
                latest_cursor,
                reset_required: false,
            })
        })
    }

    pub fn latest_cursor(&self) -> Result<i64> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT COALESCE(MAX(cursor), 0) FROM dashboard_events",
                [],
                |row| row.get(0),
            )
            .map_err(Error::from)
        })
    }

    pub fn record_task_tool_call(
        &self,
        attempt_id: &str,
        task_id: &str,
        summary: &str,
    ) -> Result<()> {
        let preview = safe_tool_activity(summary);
        self.db.with_conn(|conn| {
            let inserted = conn.execute(
                "INSERT INTO dashboard_events (
                    event_id, event_kind, project_id, project_name,
                    agent_id, agent_name, source_kind, source_label,
                    target_type, target_id, target_title, href, severity,
                    needs_attention, preview, work_kind, work_id
                 )
                 SELECT ?1, 'tool_call', t.project_id, p.name,
                        COALESCE(ls.agent_id, t.agent_id),
                        COALESCE(a.name, ls.agent_id, t.agent_id, 'Agent'),
                        'agent', COALESCE(a.name, ls.agent_id, t.agent_id, 'Agent'),
                        'task', t.id, t.title, '/tasks/' || t.id, 'info', 0,
                        ?2, 'attempt', ?3
                 FROM tasks t
                 LEFT JOIN projects p ON p.id = t.project_id
                 LEFT JOIN work_attempts wa ON wa.id = ?3 AND wa.task_id = t.id
                 LEFT JOIN logical_sessions ls ON ls.id = wa.session_id
                 LEFT JOIN agents a ON a.id = COALESCE(ls.agent_id, t.agent_id)
                 WHERE t.id = ?4 AND t.hidden = 0",
                params![
                    format!("tool-call:{}", uuid::Uuid::new_v4()),
                    preview,
                    attempt_id,
                    task_id
                ],
            )?;
            if inserted == 0 {
                let hidden = conn
                    .query_row("SELECT hidden FROM tasks WHERE id = ?1", [task_id], |row| {
                        row.get::<_, bool>(0)
                    })
                    .optional()?;
                if hidden == Some(true) {
                    return Ok(());
                }
                return Err(Error::Task(format!("task {task_id} not found")));
            }
            Ok(())
        })
    }

    pub fn record_conversation_tool_call(
        &self,
        turn_id: &str,
        conversation_id: &str,
        agent_id: &str,
        summary: &str,
    ) -> Result<()> {
        let preview = safe_tool_activity(summary);
        self.db.with_conn(|conn| {
            let inserted = conn.execute(
                "INSERT INTO dashboard_events (
                    event_id, event_kind, project_id, project_name,
                    agent_id, agent_name, source_kind, source_label,
                    target_type, target_id, target_title, href, severity,
                    needs_attention, preview, work_kind, work_id
                 )
                 SELECT ?1, 'tool_call', c.project_id, p.name,
                        ?2, COALESCE(a.name, ?2), 'agent', COALESCE(a.name, ?2),
                        'conversation', c.id, COALESCE(c.title, 'Untitled conversation'),
                        '/conversations/' || c.id, 'info', 0,
                        ?3, 'conversation_turn', ?4
                 FROM conversations c
                 LEFT JOIN projects p ON p.id = c.project_id
                 LEFT JOIN agents a ON a.id = ?2
                 WHERE c.id = ?5",
                params![
                    format!("tool-call:{}", uuid::Uuid::new_v4()),
                    agent_id,
                    preview,
                    turn_id,
                    conversation_id
                ],
            )?;
            if inserted == 0 {
                return Err(Error::Conversation(format!(
                    "conversation {conversation_id} not found"
                )));
            }
            Ok(())
        })
    }

    pub fn capture_git_baseline(
        &self,
        work_kind: &str,
        work_id: &str,
        project_id: Option<&str>,
        agent_id: &str,
        workspace: &Path,
    ) -> Result<GitMetricResult> {
        validate_work_kind(work_kind)?;
        let mut snapshot = GitSnapshot::capture(workspace, None);
        if !snapshot.files.is_empty() {
            snapshot.state = "partial".to_string();
            snapshot.detail = combine_details(
                snapshot.detail.as_deref(),
                Some("Pre-existing dirty files were subtracted conservatively"),
            );
        }
        let baseline_json = serde_json::to_string(&snapshot.files)
            .map_err(|error| Error::Backend(format!("failed to encode Git baseline: {error}")))?;
        let workspace =
            std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        let captured_at = Utc::now();
        self.db.with_conn(|conn| {
            let concurrent = conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM dashboard_git_baselines
                    WHERE workspace = ?1 AND finalized_at IS NULL
                      AND NOT (work_kind = ?2 AND work_id = ?3)
                 )",
                params![workspace.to_string_lossy(), work_kind, work_id],
                |row| row.get::<_, bool>(0),
            )?;
            if concurrent && snapshot.state != "unavailable" {
                const DETAIL: &str =
                    "Concurrent turns share this workspace; line attribution may overlap";
                snapshot.state = "partial".to_string();
                snapshot.detail = combine_details(snapshot.detail.as_deref(), Some(DETAIL));
                conn.execute(
                    "UPDATE dashboard_git_baselines
                     SET git_state = CASE WHEN git_state = 'unavailable'
                                          THEN git_state ELSE 'partial' END,
                         git_detail = CASE
                            WHEN git_state = 'unavailable' THEN git_detail
                            WHEN git_detail IS NULL OR git_detail = '' THEN ?1
                            WHEN instr(git_detail, ?1) > 0 THEN git_detail
                            ELSE git_detail || '; ' || ?1 END
                     WHERE workspace = ?2 AND finalized_at IS NULL
                       AND NOT (work_kind = ?3 AND work_id = ?4)",
                    params![DETAIL, workspace.to_string_lossy(), work_kind, work_id],
                )?;
                conn.execute(
                    "UPDATE dashboard_metric_points AS metrics
                     SET git_state = CASE WHEN git_state = 'unavailable'
                                          THEN git_state ELSE 'partial' END,
                         git_detail = CASE
                            WHEN git_state = 'unavailable' THEN git_detail
                            WHEN git_detail IS NULL OR git_detail = '' THEN ?1
                            WHEN instr(git_detail, ?1) > 0 THEN git_detail
                            ELSE git_detail || '; ' || ?1 END
                     WHERE EXISTS (
                        SELECT 1 FROM dashboard_git_baselines baseline
                        WHERE baseline.work_kind = metrics.work_kind
                          AND baseline.work_id = metrics.work_id
                          AND baseline.workspace = ?2
                          AND baseline.finalized_at IS NULL
                          AND NOT (baseline.work_kind = ?3 AND baseline.work_id = ?4)
                     )",
                    params![DETAIL, workspace.to_string_lossy(), work_kind, work_id],
                )?;
            }
            let available = snapshot.state != "unavailable";
            conn.execute(
                "INSERT INTO dashboard_git_baselines (
                    work_kind, work_id, project_id, agent_id, workspace,
                    baseline_ref, baseline_json, git_state, git_detail
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(work_kind, work_id) DO UPDATE SET
                    project_id = excluded.project_id,
                    agent_id = excluded.agent_id,
                    workspace = excluded.workspace,
                    baseline_ref = excluded.baseline_ref,
                    baseline_json = excluded.baseline_json,
                    git_state = excluded.git_state,
                    git_detail = excluded.git_detail,
                    captured_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                    last_snapshot_at = NULL,
                    finalized_at = NULL",
                params![
                    work_kind,
                    work_id,
                    project_id,
                    agent_id,
                    workspace.to_string_lossy(),
                    snapshot.baseline_ref,
                    baseline_json,
                    snapshot.state,
                    snapshot.detail
                ],
            )?;
            insert_git_metric_sample(
                conn,
                GitMetricSample {
                    work_kind,
                    work_id,
                    project_id,
                    agent_id: Some(agent_id),
                    observed_at: &captured_at,
                    additions: available.then_some(0),
                    deletions: available.then_some(0),
                    state: &snapshot.state,
                    detail: snapshot.detail.as_deref(),
                },
            )?;
            Ok::<_, Error>(())
        })?;
        let available = snapshot.state != "unavailable";
        Ok(GitMetricResult {
            state: snapshot.state,
            detail: snapshot.detail,
            additions: available.then_some(0),
            deletions: available.then_some(0),
        })
    }

    pub fn record_git_snapshot(
        &self,
        work_kind: &str,
        work_id: &str,
        final_snapshot: bool,
    ) -> Result<Option<GitMetricResult>> {
        validate_work_kind(work_kind)?;
        let baseline = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT project_id, agent_id, workspace, baseline_ref,
                        baseline_json, git_state, git_detail,
                        last_snapshot_at, finalized_at
                 FROM dashboard_git_baselines
                 WHERE work_kind = ?1 AND work_id = ?2",
                params![work_kind, work_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(Error::from)
        })?;
        let Some((
            project_id,
            agent_id,
            workspace,
            baseline_ref,
            baseline_json,
            baseline_state,
            baseline_detail,
            last_snapshot,
            finalized,
        )) = baseline
        else {
            return Ok(None);
        };
        if finalized.is_some() {
            return Ok(None);
        }
        if !final_snapshot
            && last_snapshot
                .as_deref()
                .and_then(parse_timestamp)
                .is_some_and(|timestamp| {
                    Utc::now().signed_duration_since(timestamp).num_seconds()
                        < GIT_SNAPSHOT_DEBOUNCE_SECONDS
                })
        {
            return Ok(None);
        }

        let baseline_files: HashMap<String, GitFileStat> =
            serde_json::from_str(&baseline_json).unwrap_or_default();
        let snapshot = GitSnapshot::capture(Path::new(&workspace), baseline_ref.as_deref());
        let (additions, deletions) = attributed_delta(&baseline_files, &snapshot.files);
        let state = if baseline_state == "unavailable" || snapshot.state == "unavailable" {
            "unavailable"
        } else if baseline_state == "partial" || snapshot.state == "partial" {
            "partial"
        } else {
            "available"
        };
        let detail = combine_details(baseline_detail.as_deref(), snapshot.detail.as_deref());
        let available = state != "unavailable";
        let now = Utc::now();
        self.db.with_conn(|conn| {
            insert_git_metric_sample(
                conn,
                GitMetricSample {
                    work_kind,
                    work_id,
                    project_id: project_id.as_deref(),
                    agent_id: agent_id.as_deref(),
                    observed_at: &now,
                    additions: available.then_some(additions),
                    deletions: available.then_some(deletions),
                    state,
                    detail: detail.as_deref(),
                },
            )?;
            conn.execute(
                "UPDATE dashboard_git_baselines
                 SET last_snapshot_at = ?1,
                     finalized_at = CASE WHEN ?2 THEN ?1 ELSE finalized_at END,
                     git_state = ?3, git_detail = ?4
                 WHERE work_kind = ?5 AND work_id = ?6",
                params![
                    timestamp_string(now),
                    final_snapshot,
                    state,
                    detail,
                    work_kind,
                    work_id
                ],
            )?;
            Ok::<_, Error>(())
        })?;
        Ok(Some(GitMetricResult {
            state: state.to_string(),
            detail,
            additions: available.then_some(additions),
            deletions: available.then_some(deletions),
        }))
    }

    fn validate_project(&self, project_id: Option<&str>) -> Result<()> {
        let Some(project_id) = project_id else {
            return Ok(());
        };
        self.db.with_conn(|conn| {
            let exists = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
                [project_id],
                |row| row.get::<_, bool>(0),
            )?;
            if exists {
                Ok(())
            } else {
                Err(Error::ProjectNotFound {
                    id: project_id.to_string(),
                })
            }
        })
    }

    fn projects(&self) -> Result<Vec<DashboardProject>> {
        self.db.with_conn(|conn| {
            let mut statement =
                conn.prepare("SELECT id, name FROM projects ORDER BY name COLLATE NOCASE, id")?;
            let projects = statement
                .query_map([], |row| {
                    Ok(DashboardProject {
                        id: row.get(0)?,
                        name: safe_preview(&row.get::<_, String>(1)?, 120),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(projects)
        })
    }

    fn counters(&self, filter: &DashboardFilter) -> Result<DashboardCounters> {
        let since = timestamp_string(Utc::now() - Duration::seconds(filter.range.seconds()));
        self.db.with_conn(|conn| {
            let working_agents = conn.query_row(
                "SELECT COUNT(DISTINCT agent_id) FROM (
                    SELECT ls.agent_id AS agent_id
                    FROM work_attempts wa
                    JOIN logical_sessions ls ON ls.id = wa.session_id
                    JOIN tasks t ON t.id = wa.task_id AND t.hidden = 0
                    WHERE wa.status IN ('preparing', 'running')
                      AND (?1 IS NULL OR t.project_id = ?1)
                    UNION ALL
                    SELECT ct.agent_id
                    FROM conversation_turns ct
                    JOIN conversations c ON c.id = ct.conversation_id
                    WHERE ct.status = 'running'
                      AND (?1 IS NULL OR c.project_id = ?1)
                 )",
                [filter.project_id.as_deref()],
                |row| row.get(0),
            )?;
            let active_work = conn.query_row(
                "SELECT COUNT(*) FROM (
                    SELECT wa.id
                    FROM work_attempts wa
                    JOIN tasks t ON t.id = wa.task_id AND t.hidden = 0
                    WHERE wa.status IN ('queued', 'preparing', 'running')
                      AND (?1 IS NULL OR t.project_id = ?1)
                    UNION ALL
                    SELECT ct.id
                    FROM conversation_turns ct
                    JOIN conversations c ON c.id = ct.conversation_id
                    WHERE ct.status IN ('queued', 'running')
                      AND (?1 IS NULL OR c.project_id = ?1)
                 )",
                [filter.project_id.as_deref()],
                |row| row.get(0),
            )?;
            let needs_attention = conn.query_row(
                "SELECT COUNT(*) FROM (
                    SELECT 'task:' || t.id AS target
                    FROM tasks t
                    WHERE t.hidden = 0
                      AND t.status IN ('waiting_for_input', 'blocked')
                      AND (?1 IS NULL OR t.project_id = ?1)
                    UNION
                    SELECT 'conversation:' || c.id || ':' || cas.agent_id
                    FROM conversation_agent_sessions cas
                    JOIN conversations c ON c.id = cas.conversation_id
                    WHERE cas.status = 'failed'
                      AND (?1 IS NULL OR c.project_id = ?1)
                 )",
                [filter.project_id.as_deref()],
                |row| row.get(0),
            )?;
            let tool_calls = conn.query_row(
                "SELECT COALESCE(SUM(tool_calls), 0)
                 FROM dashboard_metric_points
                 WHERE bucket_at >= ?1
                   AND (?2 IS NULL OR project_id = ?2)",
                params![since, filter.project_id],
                |row| row.get(0),
            )?;
            Ok(DashboardCounters {
                working_agents,
                active_work,
                needs_attention,
                tool_calls,
            })
        })
    }

    fn active_work(&self, project_id: Option<&str>) -> Result<Vec<DashboardActiveWork>> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT work_kind, work_id, project_id, project_name, agent_id,
                        agent_name, target_type, target_id, target_title, href,
                        phase, queued_at, started_at,
                        COALESCE((
                            SELECT de.preview FROM dashboard_events de
                            WHERE de.work_kind = active.work_kind
                              AND de.work_id = active.work_id
                            ORDER BY de.cursor DESC LIMIT 1
                        ), fallback_activity) AS activity
                 FROM (
                    SELECT 'attempt' AS work_kind, wa.id AS work_id,
                           t.project_id, p.name AS project_name,
                           ls.agent_id, COALESCE(a.name, ls.agent_id) AS agent_name,
                           'task' AS target_type, t.id AS target_id, t.title AS target_title,
                           '/tasks/' || t.id AS href,
                           CASE WHEN wa.status = 'queued' THEN 'queued' ELSE 'working' END AS phase,
                           COALESCE(wa.response_queued_at, wa.created_at) AS queued_at,
                           COALESCE(wa.response_started_at, wa.started_at) AS started_at,
                           CASE WHEN wa.status = 'queued' THEN 'Waiting for an Agent slot'
                                WHEN wa.status = 'preparing' THEN 'Preparing the Agent runtime'
                                ELSE 'Agent is working' END AS fallback_activity
                    FROM work_attempts wa
                    JOIN logical_sessions ls ON ls.id = wa.session_id
                    JOIN tasks t ON t.id = wa.task_id AND t.hidden = 0
                    LEFT JOIN projects p ON p.id = t.project_id
                    LEFT JOIN agents a ON a.id = ls.agent_id
                    WHERE wa.status IN ('queued', 'preparing', 'running')
                      AND (?1 IS NULL OR t.project_id = ?1)
                    UNION ALL
                    SELECT 'conversation_turn', ct.id, c.project_id, p.name,
                           ct.agent_id, COALESCE(a.name, ct.agent_id),
                           'conversation', c.id, COALESCE(c.title, 'Untitled conversation'),
                           '/conversations/' || c.id,
                           CASE WHEN ct.status = 'queued' THEN 'queued' ELSE 'working' END,
                           COALESCE(ct.response_queued_at, ct.queued_at),
                           COALESCE(ct.response_started_at, ct.started_at),
                           CASE WHEN ct.status = 'queued' THEN 'Waiting for an Agent slot'
                                ELSE 'Responding in the conversation' END
                    FROM conversation_turns ct
                    JOIN conversations c ON c.id = ct.conversation_id
                    LEFT JOIN projects p ON p.id = c.project_id
                    LEFT JOIN agents a ON a.id = ct.agent_id
                    WHERE ct.status IN ('queued', 'running')
                      AND (?1 IS NULL OR c.project_id = ?1)
                 ) active
                 ORDER BY CASE phase WHEN 'working' THEN 0 ELSE 1 END,
                          COALESCE(started_at, queued_at) ASC
                 LIMIT 24",
            )?;
            let active_work = statement
                .query_map([project_id], |row| {
                    Ok(DashboardActiveWork {
                        work_kind: row.get(0)?,
                        work_id: row.get(1)?,
                        project_id: row.get(2)?,
                        project_name: row
                            .get::<_, Option<String>>(3)?
                            .map(|value| safe_preview(&value, 120)),
                        agent_id: row.get(4)?,
                        agent_name: safe_preview(&row.get::<_, String>(5)?, 120),
                        target_type: row.get(6)?,
                        target_id: row.get(7)?,
                        target_title: safe_preview(&row.get::<_, String>(8)?, 180),
                        href: row.get(9)?,
                        phase: row.get(10)?,
                        queued_at: row.get(11)?,
                        started_at: row.get(12)?,
                        activity: safe_preview(&row.get::<_, String>(13)?, 240),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(active_work)
        })
    }

    fn attention(&self, project_id: Option<&str>) -> Result<Vec<DashboardAttentionItem>> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, kind, project_id, project_name, agent_id, agent_name,
                        target_type, target_id, target_title, href, summary, updated_at
                 FROM (
                    SELECT 'task:' || t.id AS id,
                           CASE WHEN t.status = 'waiting_for_input' THEN 'waiting_for_input'
                                ELSE 'blocked' END AS kind,
                           t.project_id, p.name AS project_name, t.agent_id, a.name AS agent_name,
                           'task' AS target_type, t.id AS target_id, t.title AS target_title,
                           '/tasks/' || t.id AS href,
                           CASE WHEN t.status = 'waiting_for_input' THEN 'The Agent needs your input'
                                ELSE 'Task is blocked' END AS summary,
                           t.updated_at
                    FROM tasks t
                    LEFT JOIN projects p ON p.id = t.project_id
                    LEFT JOIN agents a ON a.id = t.agent_id
                    WHERE t.hidden = 0 AND t.status IN ('waiting_for_input', 'blocked')
                      AND (?1 IS NULL OR t.project_id = ?1)
                    UNION ALL
                    SELECT 'conversation:' || c.id || ':' || cas.agent_id,
                           'failed', c.project_id, p.name, cas.agent_id, COALESCE(a.name, cas.agent_id),
                           'conversation', c.id, COALESCE(c.title, 'Untitled conversation'),
                           '/conversations/' || c.id,
                           'Conversation response failed',
                           cas.updated_at
                    FROM conversation_agent_sessions cas
                    JOIN conversations c ON c.id = cas.conversation_id
                    LEFT JOIN projects p ON p.id = c.project_id
                    LEFT JOIN agents a ON a.id = cas.agent_id
                    WHERE cas.status = 'failed' AND (?1 IS NULL OR c.project_id = ?1)
                 )
                 ORDER BY updated_at DESC LIMIT 24",
            )?;
            let attention = statement
                .query_map([project_id], |row| {
                    Ok(DashboardAttentionItem {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        project_id: row.get(2)?,
                        project_name: row
                            .get::<_, Option<String>>(3)?
                            .map(|value| safe_preview(&value, 120)),
                        agent_id: row.get(4)?,
                        agent_name: row
                            .get::<_, Option<String>>(5)?
                            .map(|value| safe_preview(&value, 120)),
                        target_type: row.get(6)?,
                        target_id: row.get(7)?,
                        target_title: safe_preview(&row.get::<_, String>(8)?, 180),
                        href: row.get(9)?,
                        summary: safe_preview(&row.get::<_, String>(10)?, 240),
                        updated_at: row.get(11)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(attention)
        })
    }

    fn series(&self, filter: &DashboardFilter) -> Result<Vec<DashboardSeriesPoint>> {
        let now = Utc::now();
        let since = now - Duration::seconds(filter.range.seconds());
        let bucket_seconds = filter.range.bucket_seconds();
        let metric_rows = self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT work_kind || ':' || work_id, bucket_at, context_used, context_size,
                        tool_calls, code_additions, code_deletions, git_state
                 FROM dashboard_metric_points
                 WHERE bucket_at >= ?1 AND (?2 IS NULL OR project_id = ?2)
                 ORDER BY bucket_at ASC, work_kind, work_id",
            )?;
            let metrics = statement
                .query_map(params![timestamp_string(since), filter.project_id], |row| {
                    Ok(MetricRow {
                        work_key: row.get(0)?,
                        bucket_at: row.get(1)?,
                        context_used: row.get(2)?,
                        context_size: row.get(3)?,
                        tool_calls: row.get(4)?,
                        code_additions: row.get(5)?,
                        code_deletions: row.get(6)?,
                        git_state: row.get(7)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok::<_, Error>(metrics)
        })?;

        let mut points = BTreeMap::<i64, DashboardSeriesPoint>::new();
        let first_bucket = bucket_epoch(since.timestamp(), bucket_seconds);
        let last_bucket = bucket_epoch(now.timestamp(), bucket_seconds);
        let mut epoch = first_bucket;
        while epoch <= last_bucket {
            points.insert(epoch, empty_series_point(epoch));
            epoch += bucket_seconds;
        }

        let mut latest_context = HashMap::<(i64, String), (i64, i64)>::new();
        // Seed each work item with its latest cumulative Git snapshot before
        // the selected window. Otherwise a long-running turn would attribute
        // all of its earlier changes to the first visible chart bucket.
        let mut previous_code = self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT d.work_kind || ':' || d.work_id,
                        d.code_additions, d.code_deletions
                 FROM dashboard_metric_points d
                 WHERE d.bucket_at < ?1
                   AND d.code_additions IS NOT NULL
                   AND d.code_deletions IS NOT NULL
                   AND (?2 IS NULL OR d.project_id = ?2)
                   AND d.bucket_at = (
                       SELECT MAX(previous.bucket_at)
                       FROM dashboard_metric_points previous
                       WHERE previous.work_kind = d.work_kind
                         AND previous.work_id = d.work_id
                         AND previous.bucket_at < ?1
                         AND previous.code_additions IS NOT NULL
                         AND previous.code_deletions IS NOT NULL
                   )",
            )?;
            let rows = statement
                .query_map(params![timestamp_string(since), filter.project_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
                    ))
                })?
                .collect::<std::result::Result<HashMap<_, _>, _>>()?;
            Ok::<_, Error>(rows)
        })?;
        let mut git_observed = HashMap::<i64, (bool, bool, bool)>::new();
        for row in metric_rows {
            let Some(timestamp) = parse_timestamp(&row.bucket_at) else {
                continue;
            };
            let bucket = bucket_epoch(timestamp.timestamp(), bucket_seconds);
            if let Some(used) = row.context_used {
                latest_context.insert(
                    (bucket, row.work_key.clone()),
                    (used, row.context_size.unwrap_or_default()),
                );
            }
            if let Some(point) = points.get_mut(&bucket) {
                point.tool_calls += row.tool_calls;
            }
            if let (Some(additions), Some(deletions)) = (row.code_additions, row.code_deletions) {
                let (previous_additions, previous_deletions) = previous_code
                    .insert(row.work_key.clone(), (additions, deletions))
                    .unwrap_or_default();
                if let Some(point) = points.get_mut(&bucket) {
                    // Git snapshots are cumulative relative to the turn baseline.
                    // A falling addition count means added lines were reverted, while
                    // a falling deletion count means deleted lines were restored.
                    // Record those decreases as reverse activity so the chart does not
                    // permanently retain changes that no longer exist.
                    let added = additions
                        .saturating_sub(previous_additions)
                        .max(0)
                        .saturating_add(previous_deletions.saturating_sub(deletions).max(0));
                    let deleted = deletions
                        .saturating_sub(previous_deletions)
                        .max(0)
                        .saturating_add(previous_additions.saturating_sub(additions).max(0));
                    point.code_additions = point.code_additions.saturating_add(added);
                    point.code_deletions = point.code_deletions.saturating_add(deleted);
                }
            }
            let observed = git_observed.entry(bucket).or_default();
            observed.0 |= row.git_state == "available";
            observed.1 |= row.git_state == "partial";
            observed.2 |= row.git_state == "unavailable";
        }
        for ((bucket, _), (used, size)) in latest_context {
            if let Some(point) = points.get_mut(&bucket) {
                point.context_used += used;
                point.context_size += size;
            }
        }
        for (bucket, (available, partial, unavailable)) in git_observed {
            if let Some(point) = points.get_mut(&bucket) {
                point.git_state = if partial || (available && unavailable) {
                    "partial".to_string()
                } else if available {
                    "available".to_string()
                } else if unavailable {
                    "unavailable".to_string()
                } else {
                    "none".to_string()
                };
            }
        }

        Ok(points.into_values().collect())
    }

    fn prune(&self) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM dashboard_events
                 WHERE occurred_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)
                    OR cursor <= COALESCE((SELECT MAX(cursor) FROM dashboard_events), 0) - ?2",
                params![
                    format!("-{EVENT_RETENTION_DAYS} days"),
                    EVENT_RETENTION_ROWS
                ],
            )?;
            conn.execute(
                "DELETE FROM dashboard_metric_points
                 WHERE bucket_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
                [format!("-{EVENT_RETENTION_DAYS} days")],
            )?;
            conn.execute(
                "DELETE FROM dashboard_git_baselines
                 WHERE COALESCE(finalized_at, captured_at)
                    < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
                [format!("-{EVENT_RETENTION_DAYS} days")],
            )?;
            Ok(())
        })
    }
}

impl GitSnapshot {
    fn capture(workspace: &Path, baseline_ref: Option<&str>) -> Self {
        if !workspace.is_dir() {
            return Self::unavailable("Workspace is unavailable");
        }
        let inside = git_output(workspace, &["rev-parse", "--is-inside-work-tree"]);
        if inside.as_deref().map(str::trim) != Some("true") {
            return Self::unavailable("Workspace is not a Git repository");
        }
        let current_head = git_output(workspace, &["rev-parse", "--verify", "HEAD"])
            .map(|value| value.trim().to_string());
        let Some(current_head) = current_head.filter(|value| !value.is_empty()) else {
            return Self::unavailable("Repository has no baseline commit");
        };
        let baseline_ref = baseline_ref.unwrap_or(&current_head).to_string();
        let Some(numstat) = git_output(
            workspace,
            &[
                "diff",
                "--no-ext-diff",
                "--numstat",
                "--no-renames",
                &baseline_ref,
                "--",
            ],
        ) else {
            return Self::unavailable("The turn's Git baseline is no longer available");
        };
        let mut files = HashMap::new();
        let mut partial = false;
        for line in numstat.lines() {
            let mut fields = line.splitn(3, '\t');
            let (Some(additions), Some(deletions), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                partial = true;
                continue;
            };
            if additions == "-" || deletions == "-" {
                partial = true;
                continue;
            }
            let (Ok(additions), Ok(deletions)) =
                (additions.parse::<i64>(), deletions.parse::<i64>())
            else {
                partial = true;
                continue;
            };
            files.insert(
                hashed_path(path),
                GitFileStat {
                    additions,
                    deletions,
                },
            );
        }
        if git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=normal"],
        )
        .is_some_and(|status| status.lines().any(|line| line.starts_with("??")))
        {
            partial = true;
        }
        Self {
            files,
            baseline_ref: Some(baseline_ref),
            state: if partial { "partial" } else { "available" }.to_string(),
            detail: partial
                .then(|| "Binary or untracked files are excluded from line counts".to_string()),
        }
    }

    fn unavailable(detail: &str) -> Self {
        Self {
            files: HashMap::new(),
            baseline_ref: None,
            state: "unavailable".to_string(),
            detail: Some(detail.to_string()),
        }
    }
}

fn git_output(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_EXTERNAL_DIFF")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn combine_details(first: Option<&str>, second: Option<&str>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) if first != second => Some(format!("{first}; {second}")),
        (Some(detail), _) | (_, Some(detail)) => Some(detail.to_string()),
        (None, None) => None,
    }
}

fn attributed_delta(
    baseline: &HashMap<String, GitFileStat>,
    current: &HashMap<String, GitFileStat>,
) -> (i64, i64) {
    current.iter().fold(
        (0, 0),
        |(total_additions, total_deletions), (path, current_stat)| {
            let baseline_stat = baseline.get(path).cloned().unwrap_or_default();
            (
                total_additions + (current_stat.additions - baseline_stat.additions).max(0),
                total_deletions + (current_stat.deletions - baseline_stat.deletions).max(0),
            )
        },
    )
}

fn hashed_path(path: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(path.as_bytes());
    format!("{:x}", digest.finalize())
}

fn validate_work_kind(work_kind: &str) -> Result<()> {
    if matches!(work_kind, "attempt" | "conversation_turn") {
        Ok(())
    } else {
        Err(Error::Backend(format!(
            "unsupported dashboard work kind: {work_kind}"
        )))
    }
}

fn row_to_event(row: &Row<'_>) -> rusqlite::Result<DashboardEvent> {
    let project_name = row
        .get::<_, Option<String>>(5)?
        .map(|value| safe_preview(&value, 120));
    let agent_name = row
        .get::<_, Option<String>>(7)?
        .map(|value| safe_preview(&value, 120));
    Ok(DashboardEvent {
        cursor: row.get(0)?,
        event_id: row.get(1)?,
        event_kind: row.get(2)?,
        occurred_at: row.get(3)?,
        project_id: row.get(4)?,
        project_name,
        agent_id: row.get(6)?,
        agent_name,
        source_kind: row.get(8)?,
        source_label: safe_preview(&row.get::<_, String>(9)?, 120),
        target_type: row.get(10)?,
        target_id: row.get(11)?,
        target_title: safe_preview(&row.get::<_, String>(12)?, 180),
        href: row.get(13)?,
        severity: row.get(14)?,
        needs_attention: row.get::<_, i64>(15)? != 0,
        preview: safe_preview(&row.get::<_, String>(16)?, 240),
        work_kind: row.get(17)?,
        work_id: row.get(18)?,
    })
}

fn safe_preview(value: &str, limit: usize) -> String {
    let collapsed = value
        .chars()
        .map(|character| {
            if character.is_control() || character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed.chars().take(limit).collect()
}

fn safe_tool_activity(value: &str) -> String {
    match value {
        "Reading workspace data"
        | "Editing workspace files"
        | "Removing workspace content"
        | "Moving workspace content"
        | "Searching workspace data"
        | "Running a command"
        | "Using an internal planning tool"
        | "Fetching external data"
        | "Switching Agent mode"
        | "Using an Agent tool" => value.to_string(),
        _ => "Using an Agent tool".to_string(),
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|timestamp| Utc.from_utc_datetime(&timestamp))
        })
}

fn timestamp_string(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn insert_git_metric_sample(conn: &Connection, sample: GitMetricSample<'_>) -> Result<()> {
    // Git values are cumulative snapshots. Keep each ordered sample so a quick
    // add-and-revert cannot be collapsed by the 10-second rows used for
    // coalesced context and tool metrics. Chart bucketing happens at read time.
    let recorded_at = timestamp_string(sample.observed_at.to_owned());
    let mut sample_at = sample.observed_at.to_owned();
    loop {
        let bucket_at = timestamp_string(sample_at);
        let occupied = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM dashboard_metric_points
                WHERE work_kind = ?1 AND work_id = ?2 AND bucket_at = ?3
             )",
            params![sample.work_kind, sample.work_id, bucket_at],
            |row| row.get::<_, bool>(0),
        )?;
        if !occupied {
            conn.execute(
                "INSERT INTO dashboard_metric_points (
                    work_kind, work_id, project_id, agent_id, bucket_at,
                    code_additions, code_deletions, git_state, git_detail, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    sample.work_kind,
                    sample.work_id,
                    sample.project_id,
                    sample.agent_id,
                    bucket_at,
                    sample.additions,
                    sample.deletions,
                    sample.state,
                    sample.detail,
                    recorded_at
                ],
            )?;
            return Ok(());
        }
        sample_at += Duration::milliseconds(1);
    }
}

fn now_string() -> String {
    timestamp_string(Utc::now())
}

fn bucket_epoch(epoch: i64, bucket_seconds: i64) -> i64 {
    epoch.div_euclid(bucket_seconds) * bucket_seconds
}

#[cfg(test)]
fn bucket_time(value: DateTime<Utc>, bucket_seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(bucket_epoch(value.timestamp(), bucket_seconds), 0)
        .single()
        .unwrap_or(value)
}

fn empty_series_point(epoch: i64) -> DashboardSeriesPoint {
    DashboardSeriesPoint {
        timestamp: timestamp_string(
            Utc.timestamp_opt(epoch, 0)
                .single()
                .unwrap_or_else(Utc::now),
        ),
        context_used: 0,
        context_size: 0,
        tool_calls: 0,
        code_additions: 0,
        code_deletions: 0,
        git_state: "none".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::registry::AgentRegistry;
    use crate::projects::{CreateProject, ProjectManager};
    use crate::tasks::board::{CreateTask, TaskBoard};
    use crate::tasks::conversation::TaskConversation;

    fn fixture() -> (Arc<Database>, String, String, String) {
        let db = Arc::new(Database::open_memory().unwrap());
        let project = ProjectManager::new(db.clone())
            .create(&CreateProject {
                name: "Platform".into(),
                description: None,
                icon: None,
            })
            .unwrap();
        let agent = "platform-agent".to_string();
        AgentRegistry::new(db.clone())
            .create_in_project(&agent, "native", &project.id)
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Ship dashboard".into(),
                agent_id: Some(agent.clone()),
                ..Default::default()
            })
            .unwrap();
        (db, project.id, agent, task.id)
    }

    #[test]
    fn source_triggers_create_bounded_literal_message_events() {
        let (db, project_id, _agent, task_id) = fixture();
        TaskConversation::new(db.clone())
            .add_message(
                &task_id,
                "system",
                "Private orchestration context must never reach the dashboard",
            )
            .unwrap();
        let message = "<script>alert('never')</script>\n".repeat(30);
        TaskConversation::new(db.clone())
            .add_message(&task_id, "user", &message)
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conversations (id, title, project_id)
                 VALUES ('conversation', 'Dashboard privacy', ?1)",
                [&project_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO conversation_messages
                     (conversation_id, sender_type, sender_id, sender_name, content)
                 VALUES
                     ('conversation', 'system', 'scheduler', 'Scheduler',
                      'Private scheduled wake-up instructions'),
                     ('conversation', 'user', 'user', 'You',
                      'Visible conversation question'),
                     ('conversation', 'agent', 'platform-agent', 'Platform Agent',
                      'Visible conversation response')",
                [],
            )
            .unwrap();
        });
        let snapshot = DashboardManager::new(db)
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Hour,
                },
                20,
            )
            .unwrap();
        let event = snapshot
            .feed
            .events
            .iter()
            .find(|event| event.event_kind == "task_message")
            .unwrap();
        assert!(event.preview.starts_with("<script>alert('never')</script>"));
        assert!(!event.preview.contains('\n'));
        assert!(event.preview.chars().count() <= 240);
        assert!(snapshot
            .feed
            .events
            .iter()
            .all(|event| !event.preview.contains("Private orchestration context")));
        assert!(snapshot
            .feed
            .events
            .iter()
            .all(|event| !event.preview.contains("Private scheduled wake-up")));
        assert!(snapshot
            .feed
            .events
            .iter()
            .any(|event| event.preview == "Visible conversation question"));
        assert!(snapshot
            .feed
            .events
            .iter()
            .any(|event| event.preview == "Visible conversation response"));
    }

    #[test]
    fn projectless_task_status_transitions_reach_the_all_projects_feed() {
        let db = Arc::new(Database::open_memory().unwrap());
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Projectless maintenance".into(),
                ..Default::default()
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE tasks SET status = 'completed' WHERE id = ?1",
                [&task.id],
            )
            .unwrap();
        });

        let snapshot = DashboardManager::new(db)
            .snapshot(
                &DashboardFilter {
                    project_id: None,
                    range: DashboardRange::Hour,
                },
                20,
            )
            .unwrap();
        let event = snapshot
            .feed
            .events
            .iter()
            .find(|event| event.event_kind == "completion" && event.target_id == task.id)
            .expect("projectless status transition should be visible");
        assert_eq!(event.project_id, None);
        assert_eq!(event.project_name, None);
        assert_eq!(event.preview, "Task completed");
    }

    #[test]
    fn canonical_tool_count_ignores_updates_and_enforces_project_scope() {
        let (db, project_id, _agent_id, task_id) = fixture();
        let manager = DashboardManager::new(db.clone());
        manager
            .record_task_tool_call("attempt-1", &task_id, "Run command with token=super-secret")
            .unwrap();
        let hidden_task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "IDLE".into(),
                agent_id: Some("platform-agent".into()),
                ..Default::default()
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE tasks SET hidden = 1 WHERE id = ?1",
                [&hidden_task.id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO dashboard_events (
                    event_id, event_kind, project_id, source_label,
                    target_type, target_id, target_title, href, preview
                 ) VALUES ('update', 'tool_call_update', ?1, 'Agent',
                           'task', ?2, 'Ship dashboard', '/tasks/' || ?2, 'Done')",
                params![project_id, task_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO dashboard_events (
                    event_id, event_kind, occurred_at, project_id, source_label,
                    target_type, target_id, target_title, href, preview
                 ) VALUES ('old-tool', 'tool_call', '2000-01-01T00:00:00.000Z',
                           ?1, 'Agent', 'task', ?2, 'Ship dashboard',
                           '/tasks/' || ?2, 'Historical tool')",
                params![project_id, task_id],
            )
            .unwrap();
        });
        manager
            .record_task_tool_call("attempt-idle", &hidden_task.id, "Run hidden idle check")
            .unwrap();
        let snapshot = manager
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Day,
                },
                20,
            )
            .unwrap();
        assert_eq!(snapshot.counters.tool_calls, 1);
        assert!(snapshot
            .feed
            .events
            .iter()
            .all(|event| event.target_id != hidden_task.id));
        db.with_conn(|conn| {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_events WHERE work_id = 'attempt-idle'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                0
            );
        });
        let tool = snapshot
            .feed
            .events
            .iter()
            .find(|event| event.event_kind == "tool_call")
            .unwrap();
        assert_eq!(tool.preview, "Using an Agent tool");
        assert!(!tool.preview.contains("super-secret"));
        assert_eq!(
            snapshot
                .series
                .iter()
                .map(|point| point.tool_calls)
                .sum::<i64>(),
            1
        );
    }

    #[test]
    fn high_frequency_progress_is_coalesced_without_losing_attention_events() {
        let (db, _project_id, agent_id, task_id) = fixture();
        let hidden_task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "IDLE".into(),
                agent_id: Some(agent_id.clone()),
                ..Default::default()
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE tasks SET hidden = 1 WHERE id = ?1",
                [&hidden_task.id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO logical_sessions (id, agent_id, title)
                 VALUES (?1, ?1, ?1)",
                [&agent_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO work_attempts (id, session_id, task_id, runner, status)
                 VALUES ('attempt-progress', ?1, ?2, 'codex', 'running')",
                params![agent_id, task_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO work_attempts (id, session_id, task_id, runner, status)
                 VALUES ('attempt-hidden-progress', ?1, ?2, 'codex', 'running')",
                params![agent_id, hidden_task.id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_events (
                    session_id, attempt_id, task_id, source_type,
                    event_type, summary
                 ) VALUES (?1, 'attempt-hidden-progress', ?2, 'acp',
                           'runner_progress', 'Hidden idle progress')",
                params![agent_id, hidden_task.id],
            )
            .unwrap();
            for summary in ["Reading files", "Still reading files"] {
                conn.execute(
                    "INSERT INTO session_events (
                        session_id, attempt_id, task_id, source_type,
                        event_type, summary
                     ) VALUES (?1, 'attempt-progress', ?2, 'acp',
                               'runner_progress', ?3)",
                    params![agent_id, task_id, summary],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO session_events (
                    session_id, attempt_id, task_id, source_type,
                    event_type, summary
                 ) VALUES (?1, 'attempt-progress', ?2, 'acp',
                           'elicitation_pending', 'Approval required')",
                params![agent_id, task_id],
            )
            .unwrap();
            for (event_type, summary) in [
                ("agent_thought", "hidden reasoning must stay private"),
                ("tool_result", "terminal output with a credential"),
                (
                    "attempt_failed",
                    "backend failure included token=super-secret",
                ),
            ] {
                conn.execute(
                    "INSERT INTO session_events (
                        session_id, attempt_id, task_id, source_type,
                        event_type, summary
                     ) VALUES (?1, 'attempt-progress', ?2, 'acp', ?3, ?4)",
                    params![agent_id, task_id, event_type, summary],
                )
                .unwrap();
            }
        });
        db.with_conn(|conn| {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_events
                     WHERE work_id = 'attempt-progress' AND event_kind = 'progress'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                1
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_events
                     WHERE work_id = 'attempt-hidden-progress'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                0
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_events
                     WHERE work_id = 'attempt-progress'
                       AND event_kind = 'waiting_for_input' AND needs_attention = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                1
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_events
                     WHERE preview LIKE '%hidden reasoning%'
                        OR preview LIKE '%terminal output%'
                        OR preview LIKE '%super-secret%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                0
            );
            assert_eq!(
                conn.query_row(
                    "SELECT preview FROM dashboard_events
                     WHERE work_id = 'attempt-progress' AND event_kind = 'failure'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
                "Agent attempt failed"
            );
        });
    }

    #[test]
    fn summary_aggregates_active_context_and_filters_other_projects() {
        let (db, project_id, agent_id, task_id) = fixture();
        let other_project = ProjectManager::new(db.clone())
            .create(&CreateProject {
                name: "Docs".into(),
                description: None,
                icon: None,
            })
            .unwrap();
        AgentRegistry::new(db.clone())
            .create_in_project("docs-agent", "native", &other_project.id)
            .unwrap();
        AgentRegistry::new(db.clone())
            .create_in_project("idle-agent", "native", &project_id)
            .unwrap();
        let other_task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Write docs".into(),
                agent_id: Some("docs-agent".into()),
                ..Default::default()
            })
            .unwrap();
        let hidden_task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "IDLE".into(),
                agent_id: Some("idle-agent".into()),
                ..Default::default()
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE tasks SET hidden = 1 WHERE id = ?1",
                [&hidden_task.id],
            )
            .unwrap();
            for (session, task, attempt) in [
                (agent_id.as_str(), task_id.as_str(), "attempt-platform"),
                ("docs-agent", other_task.id.as_str(), "attempt-docs"),
                ("idle-agent", hidden_task.id.as_str(), "attempt-idle"),
            ] {
                conn.execute(
                    "INSERT INTO logical_sessions (id, agent_id, title, status)
                     VALUES (?1, ?1, ?1, 'running')",
                    [session],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO work_attempts (
                        id, session_id, task_id, runner, status,
                        response_queued_at, response_started_at
                     ) VALUES (?1, ?2, ?3, 'codex', 'running',
                               CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
                    params![attempt, session, task],
                )
                .unwrap();
            }
            conn.execute(
                "UPDATE work_attempts SET context_used = 42000, context_size = 258400
                 WHERE id = 'attempt-platform'",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE work_attempts SET context_used = 99000, context_size = 258400
                 WHERE id = 'attempt-docs'",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE work_attempts SET context_used = 258399, context_size = 258400
                 WHERE id = 'attempt-idle'",
                [],
            )
            .unwrap();
        });
        let manager = DashboardManager::new(db.clone());
        manager
            .record_task_tool_call("attempt-platform", &task_id, "Read source")
            .unwrap();
        manager
            .record_task_tool_call("attempt-docs", &other_task.id, "Read docs")
            .unwrap();

        let platform = manager
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Hour,
                },
                40,
            )
            .unwrap();
        assert_eq!(platform.counters.working_agents, 1);
        assert_eq!(platform.counters.active_work, 1);
        assert_eq!(platform.counters.tool_calls, 1);
        assert_eq!(platform.active_work.len(), 1);
        let tool_event = platform
            .feed
            .events
            .iter()
            .find(|event| event.event_kind == "tool_call")
            .unwrap();
        assert_eq!(tool_event.agent_id.as_deref(), Some(agent_id.as_str()));
        assert_eq!(tool_event.agent_name.as_deref(), Some("platform-agent"));
        assert_eq!(
            platform.series.iter().map(|point| point.context_used).max(),
            Some(42_000)
        );
        db.with_conn(|conn| {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_metric_points
                     WHERE work_kind = 'attempt' AND work_id = 'attempt-idle'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                0
            );
        });
        assert!(!platform
            .feed
            .events
            .iter()
            .any(|event| event.project_id.as_deref() == Some(&other_project.id)));
    }

    #[test]
    fn non_git_metric_buckets_do_not_report_git_capture_failures() {
        let (db, project_id, agent_id, _task_id) = fixture();
        let now = Utc::now();
        let observed = timestamp_string(bucket_time(now - Duration::minutes(20), 10));
        let context_only = timestamp_string(bucket_time(now - Duration::minutes(10), 10));
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO dashboard_metric_points (
                    work_kind, work_id, project_id, agent_id, bucket_at,
                    code_additions, code_deletions, git_state
                 ) VALUES ('attempt', 'git-work', ?1, ?2, ?3, 0, 0, 'available')",
                params![project_id, agent_id, observed],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO dashboard_metric_points (
                    work_kind, work_id, project_id, agent_id, bucket_at,
                    context_used, context_size
                 ) VALUES ('attempt', 'git-work', ?1, ?2, ?3, 12000, 258400)",
                params![project_id, agent_id, context_only],
            )
            .unwrap();
            assert_eq!(
                conn.query_row(
                    "SELECT git_state FROM dashboard_metric_points
                     WHERE work_id = 'git-work' AND bucket_at = ?1",
                    [&context_only],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
                "unobserved"
            );
        });

        let snapshot = DashboardManager::new(db)
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Hour,
                },
                20,
            )
            .unwrap();
        assert!(snapshot
            .series
            .iter()
            .any(|point| point.git_state == "available"));
        assert!(snapshot
            .series
            .iter()
            .all(|point| !matches!(point.git_state.as_str(), "partial" | "unavailable")));
    }

    #[test]
    fn code_series_only_attributes_changes_created_inside_the_window() {
        let (db, project_id, agent_id, _task_id) = fixture();
        let outside = timestamp_string(Utc::now() - Duration::hours(2));
        let inside = timestamp_string(Utc::now() - Duration::minutes(10));
        db.with_conn(|conn| {
            for (bucket_at, additions, deletions) in [
                (outside.as_str(), 20_i64, 4_i64),
                (inside.as_str(), 23_i64, 6_i64),
            ] {
                conn.execute(
                    "INSERT INTO dashboard_metric_points (
                        work_kind, work_id, project_id, agent_id, bucket_at,
                        code_additions, code_deletions, git_state
                     ) VALUES ('attempt', 'long-turn', ?1, ?2, ?3, ?4, ?5, 'available')",
                    params![project_id, agent_id, bucket_at, additions, deletions],
                )
                .unwrap();
            }
        });
        let snapshot = DashboardManager::new(db)
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Hour,
                },
                20,
            )
            .unwrap();
        assert_eq!(
            snapshot
                .series
                .iter()
                .map(|point| point.code_additions)
                .sum::<i64>(),
            3
        );
        assert_eq!(
            snapshot
                .series
                .iter()
                .map(|point| point.code_deletions)
                .sum::<i64>(),
            2
        );
    }

    #[test]
    fn code_series_counts_reverts_as_reverse_activity() {
        let (db, project_id, agent_id, _task_id) = fixture();
        let outside = timestamp_string(Utc::now() - Duration::hours(2));
        let inside = timestamp_string(Utc::now() - Duration::minutes(10));
        db.with_conn(|conn| {
            for (work_id, outside_additions, outside_deletions) in [
                ("reverted-additions", 10_i64, 0_i64),
                ("restored-deletions", 0, 6),
            ] {
                conn.execute(
                    "INSERT INTO dashboard_metric_points (
                        work_kind, work_id, project_id, agent_id, bucket_at,
                        code_additions, code_deletions, git_state
                     ) VALUES ('attempt', ?1, ?2, ?3, ?4, ?5, ?6, 'available')",
                    params![
                        work_id,
                        project_id,
                        agent_id,
                        outside,
                        outside_additions,
                        outside_deletions
                    ],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO dashboard_metric_points (
                        work_kind, work_id, project_id, agent_id, bucket_at,
                        code_additions, code_deletions, git_state
                     ) VALUES ('attempt', ?1, ?2, ?3, ?4, 0, 0, 'available')",
                    params![work_id, project_id, agent_id, inside],
                )
                .unwrap();
            }
        });

        let snapshot = DashboardManager::new(db)
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Hour,
                },
                20,
            )
            .unwrap();
        assert_eq!(
            snapshot
                .series
                .iter()
                .map(|point| point.code_additions)
                .sum::<i64>(),
            6,
            "restoring previously deleted lines is addition activity"
        );
        assert_eq!(
            snapshot
                .series
                .iter()
                .map(|point| point.code_deletions)
                .sum::<i64>(),
            10,
            "reverting previously added lines is deletion activity"
        );
    }

    #[test]
    fn code_series_preserves_reversals_sampled_in_one_storage_bucket() {
        let (db, project_id, agent_id, _task_id) = fixture();
        let observed_at = bucket_time(Utc::now() - Duration::minutes(10), 10);
        db.with_conn(|conn| {
            for additions in [0_i64, 10, 0] {
                insert_git_metric_sample(
                    conn,
                    GitMetricSample {
                        work_kind: "attempt",
                        work_id: "short-revert",
                        project_id: Some(&project_id),
                        agent_id: Some(&agent_id),
                        observed_at: &observed_at,
                        additions: Some(additions),
                        deletions: Some(0),
                        state: "available",
                        detail: None,
                    },
                )
                .unwrap();
            }

            let mut statement = conn
                .prepare(
                    "SELECT bucket_at FROM dashboard_metric_points
                     WHERE work_kind = 'attempt' AND work_id = 'short-revert'
                     ORDER BY bucket_at",
                )
                .unwrap();
            let stored = statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(stored.len(), 3);
            assert!(stored.iter().all(|sample| {
                parse_timestamp(sample)
                    .is_some_and(|timestamp| bucket_time(timestamp, 10) == observed_at)
            }));
        });

        let snapshot = DashboardManager::new(db)
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Hour,
                },
                20,
            )
            .unwrap();
        assert_eq!(
            snapshot
                .series
                .iter()
                .map(|point| point.code_additions)
                .sum::<i64>(),
            10
        );
        assert_eq!(
            snapshot
                .series
                .iter()
                .map(|point| point.code_deletions)
                .sum::<i64>(),
            10
        );
    }

    #[test]
    fn streaming_message_versions_deduplicate_without_double_escaping() {
        let (db, project_id, _agent_id, task_id) = fixture();
        let conversation = TaskConversation::new(db.clone());
        let message = conversation.add_message(&task_id, "assistant", "").unwrap();
        conversation
            .update_message_content(message.id, "First <strong>draft</strong>")
            .unwrap();
        let first_cursor = DashboardManager::new(db.clone()).latest_cursor().unwrap();
        conversation
            .update_message_content(message.id, "Final <strong>answer</strong>")
            .unwrap();

        let manager = DashboardManager::new(db);
        let snapshot = manager
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Hour,
                },
                40,
            )
            .unwrap();
        let versions = snapshot
            .feed
            .events
            .iter()
            .filter(|event| event.event_id == format!("task-message:{}", message.id))
            .collect::<Vec<_>>();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].preview, "Final <strong>answer</strong>");
        let overlap = manager
            .replay_after(
                &DashboardFilter {
                    project_id: None,
                    range: DashboardRange::Hour,
                },
                first_cursor,
                20,
            )
            .unwrap();
        assert_eq!(overlap.events.len(), 1);
        assert_eq!(overlap.events[0].event_id, versions[0].event_id);
    }

    #[test]
    fn telemetry_retention_runs_on_writes_and_snapshot_reads() {
        let (db, project_id, _agent_id, task_id) = fixture();
        db.with_conn(|conn| {
            for (event_id, occurred_at) in [
                ("expired", "2000-01-01T00:00:00.000Z"),
                ("recent", "2999-01-01T00:00:00.000Z"),
            ] {
                conn.execute(
                    "INSERT INTO dashboard_events (
                        event_id, event_kind, occurred_at, project_id,
                        source_label, target_type, target_id, target_title,
                        href, preview
                     ) VALUES (?1, 'progress', ?2, ?3, 'Agent', 'task', ?4,
                               'Ship dashboard', '/tasks/' || ?4, 'Safe progress')",
                    params![event_id, occurred_at, project_id, task_id],
                )
                .unwrap();
            }
            for index in 0..254 {
                conn.execute(
                    "INSERT INTO dashboard_events (
                        event_id, event_kind, occurred_at, project_id,
                        source_label, target_type, target_id, target_title,
                        href, preview
                     ) VALUES (?1, 'progress', '2999-01-01T00:00:00.000Z', ?2,
                               'Agent', 'task', ?3, 'Ship dashboard',
                               '/tasks/' || ?3, 'Safe progress')",
                    params![format!("retention-fill-{index}"), project_id, task_id],
                )
                .unwrap();
            }
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_events WHERE event_id = 'expired'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                0
            );
            conn.execute(
                "INSERT INTO dashboard_events (
                    event_id, event_kind, occurred_at, project_id, source_label,
                    target_type, target_id, target_title, href, preview
                 ) VALUES ('expired-on-read', 'progress',
                           '2000-01-01T00:00:00.000Z', ?1, 'Agent', 'task', ?2,
                           'Ship dashboard', '/tasks/' || ?2, 'Safe progress')",
                params![project_id, task_id],
            )
            .unwrap();
        });
        DashboardManager::new(db.clone())
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Week,
                },
                40,
            )
            .unwrap();
        db.with_conn(|conn| {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_events WHERE event_id = 'expired-on-read'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                0
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_events WHERE event_id = 'recent'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                1
            );
        });
    }

    #[test]
    fn replay_is_cursor_ordered_and_detects_retention_gaps() {
        let (db, _project_id, _agent_id, task_id) = fixture();
        let manager = DashboardManager::new(db.clone());
        TaskConversation::new(db.clone())
            .add_message(&task_id, "user", "Start the work")
            .unwrap();
        manager
            .record_task_tool_call("attempt-1", &task_id, "First")
            .unwrap();
        let first = manager.latest_cursor().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO dashboard_events (
                    event_id, event_kind, occurred_at, source_label,
                    target_type, target_id, target_title, href, preview
                 ) VALUES ('outside-replay-range', 'progress', ?1, 'Agent',
                           'task', ?2, 'Ship dashboard', '/tasks/' || ?2,
                           'Out-of-window progress')",
                params![timestamp_string(Utc::now() - Duration::hours(2)), task_id],
            )
            .unwrap();
        });
        manager
            .record_task_tool_call("attempt-1", &task_id, "Second")
            .unwrap();
        let filter = DashboardFilter {
            project_id: None,
            range: DashboardRange::Hour,
        };
        let replay = manager.replay_after(&filter, first, 20).unwrap();
        assert_eq!(replay.events.len(), 1);
        assert!(replay.events[0].cursor > first);
        assert_ne!(replay.events[0].event_id, "outside-replay-range");
        assert!(!replay.reset_required);
        assert!(
            manager
                .replay_after(&filter, replay.latest_cursor + 100, 20)
                .unwrap()
                .reset_required
        );

        db.with_conn(|conn| {
            conn.execute("DELETE FROM dashboard_events WHERE cursor <= ?1", [first])
                .unwrap();
        });
        assert!(
            manager
                .replay_after(&filter, first - 1, 20)
                .unwrap()
                .reset_required
        );
    }

    #[test]
    fn git_metrics_subtract_preexisting_dirty_lines_and_report_non_git() {
        let (db, project_id, agent_id, _task_id) = fixture();
        let manager = DashboardManager::new(db);
        let repository = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(repository.path())
            .status()
            .unwrap();
        std::fs::write(repository.path().join("tracked.txt"), "one\n").unwrap();
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(repository.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Dashboard test",
                "-c",
                "user.email=dashboard@example.invalid",
                "commit",
                "-qm",
                "baseline",
            ])
            .current_dir(repository.path())
            .status()
            .unwrap();
        std::fs::write(repository.path().join("tracked.txt"), "one\npreexisting\n").unwrap();
        manager
            .capture_git_baseline(
                "attempt",
                "attempt-git",
                Some(&project_id),
                &agent_id,
                repository.path(),
            )
            .unwrap();
        std::fs::write(
            repository.path().join("tracked.txt"),
            "one\npreexisting\nagent-one\nagent-two\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(repository.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Dashboard test",
                "-c",
                "user.email=dashboard@example.invalid",
                "commit",
                "-qm",
                "turn changes",
            ])
            .current_dir(repository.path())
            .status()
            .unwrap();
        let metric = manager
            .record_git_snapshot("attempt", "attempt-git", true)
            .unwrap()
            .unwrap();
        assert_eq!(metric.additions, Some(2));
        assert_eq!(metric.deletions, Some(0));
        assert_eq!(metric.state, "partial");

        let first_concurrent = manager
            .capture_git_baseline(
                "attempt",
                "concurrent-a",
                Some(&project_id),
                &agent_id,
                repository.path(),
            )
            .unwrap();
        assert_eq!(first_concurrent.state, "available");
        let second_concurrent = manager
            .capture_git_baseline(
                "conversation_turn",
                "concurrent-b",
                Some(&project_id),
                &agent_id,
                repository.path(),
            )
            .unwrap();
        assert_eq!(second_concurrent.state, "partial");
        db_state(&manager, "attempt", "concurrent-a", |state, detail| {
            assert_eq!(state, "partial");
            assert!(detail.contains("Concurrent turns"));
        });
        manager
            .record_git_snapshot("attempt", "concurrent-a", true)
            .unwrap();
        manager
            .record_git_snapshot("conversation_turn", "concurrent-b", true)
            .unwrap();

        std::fs::write(repository.path().join("untracked.txt"), "not counted\n").unwrap();
        let partial = manager
            .capture_git_baseline(
                "attempt",
                "attempt-partial",
                Some(&project_id),
                &agent_id,
                repository.path(),
            )
            .unwrap();
        assert_eq!(partial.state, "partial");
        assert_eq!(partial.additions, Some(0));
        assert!(partial.detail.unwrap().contains("untracked"));

        let non_git = tempfile::tempdir().unwrap();
        let unavailable = manager
            .capture_git_baseline(
                "conversation_turn",
                "turn-no-git",
                Some(&project_id),
                &agent_id,
                non_git.path(),
            )
            .unwrap();
        assert_eq!(unavailable.state, "unavailable");
        assert!(unavailable.detail.unwrap().contains("not a Git repository"));
    }

    #[test]
    fn unavailable_git_sample_does_not_imply_a_revert() {
        let (db, project_id, agent_id, _task_id) = fixture();
        let manager = DashboardManager::new(db.clone());
        let repository = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(repository.path())
            .status()
            .unwrap();
        std::fs::write(repository.path().join("tracked.txt"), "one\n").unwrap();
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(repository.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Dashboard test",
                "-c",
                "user.email=dashboard@example.invalid",
                "commit",
                "-qm",
                "baseline",
            ])
            .current_dir(repository.path())
            .status()
            .unwrap();

        manager
            .capture_git_baseline(
                "attempt",
                "capture-failure",
                Some(&project_id),
                &agent_id,
                repository.path(),
            )
            .unwrap();
        std::fs::write(repository.path().join("tracked.txt"), "one\nagent line\n").unwrap();
        let observed = manager
            .record_git_snapshot("attempt", "capture-failure", false)
            .unwrap()
            .unwrap();
        assert_eq!(observed.additions, Some(1));
        assert_eq!(observed.deletions, Some(0));

        repository.close().unwrap();
        let unavailable = manager
            .record_git_snapshot("attempt", "capture-failure", true)
            .unwrap()
            .unwrap();
        assert_eq!(unavailable.state, "unavailable");
        assert_eq!(unavailable.additions, None);
        assert_eq!(unavailable.deletions, None);
        db.with_conn(|conn| {
            let stored = conn
                .query_row(
                    "SELECT code_additions, code_deletions, git_state
                     FROM dashboard_metric_points
                     WHERE work_kind = 'attempt' AND work_id = 'capture-failure'
                     ORDER BY bucket_at DESC LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, Option<i64>>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(stored, (None, None, "unavailable".to_string()));
        });

        let snapshot = manager
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Hour,
                },
                20,
            )
            .unwrap();
        assert_eq!(
            snapshot
                .series
                .iter()
                .map(|point| point.code_additions)
                .sum::<i64>(),
            1
        );
        assert_eq!(
            snapshot
                .series
                .iter()
                .map(|point| point.code_deletions)
                .sum::<i64>(),
            0
        );
    }

    #[test]
    fn project_filter_rejects_unknown_scope() {
        let (db, _, _, _) = fixture();
        let error = DashboardManager::new(db)
            .snapshot(
                &DashboardFilter {
                    project_id: Some("missing".into()),
                    range: DashboardRange::Day,
                },
                20,
            )
            .unwrap_err();
        assert!(matches!(error, Error::ProjectNotFound { .. }));
    }

    #[test]
    fn attention_counter_matches_each_failed_conversation_agent_item() {
        let (db, project_id, agent_id, _) = fixture();
        let reviewer_id = "reviewer-agent";
        AgentRegistry::new(db.clone())
            .create_in_project(reviewer_id, "native", &project_id)
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conversations (id, title, project_id)
                 VALUES ('conversation-failed', 'Failed responses', ?1)",
                [&project_id],
            )?;
            for failed_agent in [agent_id.as_str(), reviewer_id] {
                conn.execute(
                    "INSERT INTO conversation_agent_sessions
                         (conversation_id, agent_id, status)
                     VALUES ('conversation-failed', ?1, 'failed')",
                    [failed_agent],
                )?;
            }
            Ok::<_, Error>(())
        })
        .unwrap();

        let snapshot = DashboardManager::new(db)
            .snapshot(
                &DashboardFilter {
                    project_id: Some(project_id),
                    range: DashboardRange::Hour,
                },
                20,
            )
            .unwrap();

        assert_eq!(snapshot.counters.needs_attention, 2);
        assert_eq!(snapshot.attention.len(), 2);
        assert!(snapshot
            .attention
            .iter()
            .any(|item| item.id == format!("conversation:conversation-failed:{agent_id}")));
        assert!(snapshot
            .attention
            .iter()
            .any(|item| item.id == "conversation:conversation-failed:reviewer-agent"));
    }

    #[test]
    fn dashboard_query_plans_use_bounded_telemetry_indexes() {
        let (db, project_id, _, _) = fixture();
        db.with_conn(|conn| {
            for (sql, parameters, expected_index) in [
                (
                    "EXPLAIN QUERY PLAN
                     SELECT COALESCE(SUM(tool_calls), 0)
                     FROM dashboard_metric_points
                     WHERE bucket_at >= ?1 AND project_id = ?2",
                    params!["2000-01-01T00:00:00.000Z", project_id],
                    "idx_dashboard_metrics_project_time",
                ),
                (
                    "EXPLAIN QUERY PLAN
                     SELECT cursor FROM dashboard_events
                     WHERE project_id = ?1 AND cursor > ?2
                     ORDER BY cursor ASC LIMIT 200",
                    params![project_id, 0_i64],
                    "idx_dashboard_events_cursor_project",
                ),
            ] {
                let details = conn
                    .prepare(sql)
                    .unwrap()
                    .query_map(parameters, |row| row.get::<_, String>(3))
                    .unwrap()
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .unwrap();
                assert!(
                    details.iter().any(|detail| detail.contains(expected_index)),
                    "dashboard query did not use {expected_index}: {details:?}"
                );
            }
        });
    }

    fn db_state(
        manager: &DashboardManager,
        work_kind: &str,
        work_id: &str,
        assertion: impl FnOnce(&str, &str),
    ) {
        manager.db.with_conn(|conn| {
            let (state, detail) = conn
                .query_row(
                    "SELECT git_state, COALESCE(git_detail, '')
                     FROM dashboard_git_baselines
                     WHERE work_kind = ?1 AND work_id = ?2",
                    params![work_kind, work_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap();
            assertion(&state, &detail);
        });
    }
}
