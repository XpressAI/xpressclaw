use agent_client_protocol::schema::v1::Usage;

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardTokenUsage {
    pub total_tokens: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_read_tokens: Option<i64>,
    pub cached_write_tokens: Option<i64>,
    pub thought_tokens: Option<i64>,
    pub reported_responses: i64,
    pub unreported_responses: i64,
    pub unclassified_responses: i64,
    pub recording_started_at: String,
}

impl DashboardManager {
    /// One row per actual ACP prompt, not per Task/attempt (which can contain
    /// multiple prompts). Retried persistence of the same response is harmless.
    pub fn record_prompt_usage(
        &self,
        prompt_id: &str,
        work_kind: &str,
        work_id: &str,
        runner: &str,
        usage: Option<&Usage>,
    ) -> Result<()> {
        validate_work_kind(work_kind)?;
        // ACP v1's draft has conflicting turn/session semantics. The built-in
        // Claude and Codex bridges report per prompt; other adapters must not
        // be summed until their semantics are verified. See docs/control-center.md.
        let per_turn = matches!(runner, "claude" | "codex");
        let values = usage.map(|usage| {
            [
                Some(usage.total_tokens),
                Some(usage.input_tokens),
                Some(usage.output_tokens),
                usage.cached_read_tokens,
                usage.cached_write_tokens,
                usage.thought_tokens,
            ]
        });
        let valid = values
            .as_ref()
            .is_some_and(|values| values.iter().flatten().all(|n| *n <= i64::MAX as u64));
        let status = if usage.is_none() {
            "not_reported"
        } else if per_turn && valid {
            "reported"
        } else {
            "unclassified"
        };
        let counts = if status == "reported" {
            values.unwrap().map(|n| n.map(|n| n as i64))
        } else {
            [None; 6]
        };
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO dashboard_prompt_usage (
                    prompt_id, work_kind, work_id, project_id, agent_id, runner, status,
                    total_tokens, input_tokens, output_tokens, cached_read_tokens, cached_write_tokens, thought_tokens
                 ) SELECT ?1, ?2, ?3, work.project_id, work.agent_id, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
                   FROM (
                     SELECT t.project_id, ls.agent_id FROM work_attempts wa
                     JOIN tasks t ON t.id = wa.task_id AND t.hidden = 0
                     JOIN logical_sessions ls ON ls.id = wa.session_id
                     WHERE ?2 = 'attempt' AND wa.id = ?3
                     UNION ALL
                     SELECT c.project_id, ct.agent_id FROM conversation_turns ct
                     JOIN conversations c ON c.id = ct.conversation_id
                     WHERE ?2 = 'conversation_turn' AND ct.id = ?3
                   ) work",
                params![prompt_id, work_kind, work_id, runner, status,
                    counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]],
            )?;
            // Retain bounded history even when nobody opens the dashboard.
            conn.execute(
                "DELETE FROM dashboard_prompt_usage WHERE recorded_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-8 days')", [],
            )?;
            Ok(())
        })
    }

    pub(super) fn token_usage(&self, filter: &DashboardFilter) -> Result<DashboardTokenUsage> {
        let until = Utc::now();
        let since = until - Duration::seconds(filter.range.seconds());
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT SUM(total_tokens), SUM(input_tokens), SUM(output_tokens),
                    SUM(cached_read_tokens), SUM(cached_write_tokens), SUM(thought_tokens),
                    COALESCE(SUM(status = 'reported'), 0),
                    COALESCE(SUM(status = 'not_reported'), 0),
                    COALESCE(SUM(status = 'unclassified'), 0),
                    (SELECT value FROM config WHERE key = 'dashboard_token_recording_started_at')
                 FROM dashboard_prompt_usage
                 WHERE recorded_at >= ?1 AND recorded_at <= ?2
                   AND (?3 IS NULL OR project_id = ?3)",
                params![
                    timestamp_string(since),
                    timestamp_string(until),
                    filter.project_id
                ],
                |row| {
                    Ok(DashboardTokenUsage {
                        total_tokens: row.get(0)?,
                        input_tokens: row.get(1)?,
                        output_tokens: row.get(2)?,
                        cached_read_tokens: row.get(3)?,
                        cached_write_tokens: row.get(4)?,
                        thought_tokens: row.get(5)?,
                        reported_responses: row.get(6)?,
                        unreported_responses: row.get(7)?,
                        unclassified_responses: row.get(8)?,
                        recording_started_at: row.get(9)?,
                    })
                },
            )
            .map_err(Error::from)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::registry::AgentRegistry;
    use crate::projects::{CreateProject, ProjectManager};
    use crate::tasks::board::{CreateTask, TaskBoard};

    fn fixture(db: Arc<Database>) -> (DashboardManager, String) {
        let project = ProjectManager::new(db.clone())
            .create(&CreateProject {
                name: "Tokens".into(),
                description: None,
                icon: None,
            })
            .unwrap();
        AgentRegistry::new(db.clone())
            .create_in_project("tokens-agent", "native", &project.id)
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Token accounting".into(),
                agent_id: Some("tokens-agent".into()),
                ..Default::default()
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute("INSERT INTO logical_sessions (id, agent_id, title) VALUES ('s', 'tokens-agent', 'Tokens')", []).unwrap();
            conn.execute("INSERT INTO work_attempts (id, session_id, task_id, runner, status) VALUES ('a', 's', ?1, 'codex', 'running')", [&task.id]).unwrap();
            conn.execute("INSERT INTO conversations (id, project_id) VALUES ('c', ?1)", [&project.id]).unwrap();
            conn.execute("INSERT INTO conversation_turns (id, conversation_id, agent_id, trigger_message_id, status) VALUES ('ct', 'c', 'tokens-agent', NULL, 'running')", []).unwrap();
        });
        (DashboardManager::new(db), project.id)
    }

    fn filter(project_id: Option<String>) -> DashboardFilter {
        DashboardFilter {
            project_id,
            range: DashboardRange::Hour,
        }
    }

    #[test]
    fn reports_persist_and_count_each_prompt_once_across_tasks_and_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.db");
        let (manager, project) = fixture(Arc::new(Database::open(&path).unwrap()));
        let usage = Usage::new(150, 100, 50)
            .cached_read_tokens(80)
            .thought_tokens(10);
        for _ in 0..2 {
            manager
                .record_prompt_usage("first", "attempt", "a", "codex", Some(&usage))
                .unwrap();
        }
        // The same attempt can be prompted again. Lower counts are not a
        // cumulative counter reset and must still contribute their full value.
        manager
            .record_prompt_usage(
                "second",
                "attempt",
                "a",
                "codex",
                Some(&Usage::new(15, 10, 5)),
            )
            .unwrap();
        manager
            .record_prompt_usage(
                "chat",
                "conversation_turn",
                "ct",
                "claude",
                Some(&Usage::new(9, 3, 2).cached_write_tokens(4)),
            )
            .unwrap();
        drop(manager);
        let manager = DashboardManager::new(Arc::new(Database::open(&path).unwrap()));
        let totals = manager.token_usage(&filter(Some(project))).unwrap();
        assert_eq!(totals.total_tokens, Some(174)); // Never re-add cache/reasoning categories.
        assert_eq!(totals.input_tokens, Some(113));
        assert_eq!(totals.output_tokens, Some(57));
        assert_eq!(totals.cached_read_tokens, Some(80));
        assert_eq!(totals.reported_responses, 3);
        assert!(!totals.recording_started_at.is_empty());
    }

    #[test]
    fn missing_unsupported_and_zero_reports_stay_distinct_from_context_usage() {
        let (manager, _) = fixture(Arc::new(Database::open_memory().unwrap()));
        manager
            .record_prompt_usage("missing", "attempt", "a", "codex", None)
            .unwrap();
        manager
            .record_prompt_usage(
                "cumulative",
                "attempt",
                "a",
                "copilot",
                Some(&Usage::new(999, 900, 99)),
            )
            .unwrap();
        manager
            .record_prompt_usage(
                "overflow",
                "attempt",
                "a",
                "claude",
                Some(&Usage::new(u64::MAX, 0, 0)),
            )
            .unwrap();
        manager.db.with_conn(|conn| { conn.execute("UPDATE work_attempts SET context_used = 42000, context_size = 64000 WHERE id = 'a'", []).unwrap(); });
        let unknown = manager.token_usage(&filter(None)).unwrap();
        assert_eq!(unknown.total_tokens, None);
        assert_eq!(unknown.unreported_responses, 1);
        assert_eq!(unknown.unclassified_responses, 2);
        manager
            .record_prompt_usage("zero", "attempt", "a", "claude", Some(&Usage::new(0, 0, 0)))
            .unwrap();
        let zero = manager.token_usage(&filter(None)).unwrap();
        assert_eq!(zero.total_tokens, Some(0));
        assert_eq!(zero.cached_read_tokens, None);
        assert_eq!(zero.reported_responses, 1);
    }

    #[test]
    fn totals_use_response_time_and_project_scope_without_historical_backfill() {
        let (manager, project) = fixture(Arc::new(Database::open_memory().unwrap()));
        for id in ["old", "now", "future", "other"] {
            manager
                .record_prompt_usage(id, "attempt", "a", "codex", Some(&Usage::new(100, 60, 40)))
                .unwrap();
        }
        manager.db.with_conn(|conn| {
            conn.execute("UPDATE dashboard_prompt_usage SET recorded_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 hours') WHERE prompt_id = 'old'", []).unwrap();
            conn.execute("UPDATE dashboard_prompt_usage SET recorded_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+2 hours') WHERE prompt_id = 'future'", []).unwrap();
            conn.execute("UPDATE dashboard_prompt_usage SET project_id = NULL WHERE prompt_id = 'other'", []).unwrap();
            // Legacy character estimates are not provider token reports.
            conn.execute("INSERT INTO usage_logs (agent_id, model, input_tokens, output_tokens, cost_usd) VALUES ('tokens-agent', 'legacy', 1000000, 999, 0)", []).unwrap();
        });
        assert_eq!(
            manager
                .token_usage(&filter(Some(project.clone())))
                .unwrap()
                .total_tokens,
            Some(100)
        );
        assert_eq!(
            manager.token_usage(&filter(None)).unwrap().total_tokens,
            Some(200)
        );
        assert_eq!(
            manager
                .token_usage(&DashboardFilter {
                    project_id: Some(project),
                    range: DashboardRange::Day
                })
                .unwrap()
                .total_tokens,
            Some(200)
        );
    }
}
