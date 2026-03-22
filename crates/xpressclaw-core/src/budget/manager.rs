use std::sync::Arc;

use chrono::{Datelike, Local, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::config::{BudgetConfig, Config, OnExceeded};
use crate::db::Database;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetState {
    pub agent_id: String,
    pub daily_spent: f64,
    pub daily_reset_at: Option<String>,
    pub monthly_spent: f64,
    pub monthly_reset_at: Option<String>,
    pub total_spent: f64,
    pub is_paused: bool,
    pub pause_reason: Option<String>,
    pub degraded_model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BudgetSummary {
    pub daily_spent: f64,
    pub daily_limit: Option<f64>,
    pub monthly_spent: f64,
    pub monthly_limit: Option<f64>,
    pub total_spent: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub request_count: i64,
}

/// Budget enforcement with per-agent inheritance from system defaults.
///
/// Each agent can have its own budget config. If not set, the system-wide
/// budget config is used as the default.
pub struct BudgetManager {
    db: Arc<Database>,
    config: Arc<Config>,
}

impl BudgetManager {
    pub fn new(db: Arc<Database>, config: Arc<Config>) -> Self {
        Self { db, config }
    }

    /// Resolve the effective budget config for an agent.
    /// Agent-specific budget overrides system defaults.
    pub fn effective_budget(&self, agent_id: &str) -> &BudgetConfig {
        self.config
            .agents
            .iter()
            .find(|a| a.name == agent_id)
            .and_then(|a| a.budget.as_ref())
            .unwrap_or(&self.config.system.budget)
    }

    /// Get the system-wide budget config (used for global summaries).
    pub fn system_budget(&self) -> &BudgetConfig {
        &self.config.system.budget
    }

    pub fn get_state(&self, agent_id: &str) -> Result<BudgetState> {
        let result = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT * FROM budget_state WHERE agent_id = ?1")?;
            stmt.query_row([agent_id], |row| {
                Ok(BudgetState {
                    agent_id: row.get("agent_id")?,
                    daily_spent: row.get("daily_spent")?,
                    daily_reset_at: row.get("daily_reset_at")?,
                    monthly_spent: row.get("monthly_spent")?,
                    monthly_reset_at: row.get("monthly_reset_at")?,
                    total_spent: row.get("total_spent")?,
                    is_paused: row.get::<_, i32>("is_paused")? != 0,
                    pause_reason: row.get("pause_reason")?,
                    degraded_model: row.get("degraded_model").unwrap_or(None),
                })
            })
        });

        let mut state = match result {
            Ok(state) => state,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                let state = BudgetState {
                    agent_id: agent_id.to_string(),
                    daily_spent: 0.0,
                    daily_reset_at: None,
                    monthly_spent: 0.0,
                    monthly_reset_at: None,
                    total_spent: 0.0,
                    is_paused: false,
                    pause_reason: None,
                    degraded_model: None,
                };
                self.save_state(&state)?;
                state
            }
            Err(e) => return Err(Error::Database(e.to_string())),
        };

        // Apply daily/monthly resets if needed
        if self.maybe_reset(&mut state) {
            self.save_state(&state)?;
        }

        Ok(state)
    }

    pub fn update_spending(&self, agent_id: &str, cost: f64) -> Result<BudgetState> {
        let mut state = self.get_state(agent_id)?;

        state.daily_spent += cost;
        state.monthly_spent += cost;
        state.total_spent += cost;

        self.save_state(&state)?;
        self.check_limits(agent_id, &state)?;

        debug!(
            agent_id,
            cost,
            total = state.total_spent,
            "updated spending"
        );
        Ok(state)
    }

    /// Check if an agent is within budget.
    ///
    /// Returns `Ok(true)` if within budget, `Ok(false)` if exceeded but allowed
    /// to continue (alert mode), or `Err` if paused/stopped.
    ///
    /// Automatically resets daily/monthly counters when the period expires,
    /// and auto-resumes paused agents that are now under budget.
    pub fn check_budget(&self, agent_id: &str) -> Result<bool> {
        let mut state = self.get_state(agent_id)?; // get_state already calls maybe_reset

        // Auto-resume if paused but now under budget (e.g. after daily reset)
        if state.is_paused {
            let budget = self.effective_budget(agent_id);
            let under_daily = budget
                .daily_amount()
                .map(|l| state.daily_spent < l)
                .unwrap_or(true);
            let under_monthly = budget
                .monthly_amount()
                .map(|l| state.monthly_spent < l)
                .unwrap_or(true);

            if under_daily && under_monthly {
                info!(agent_id, "auto-resuming agent (now under budget)");
                self.resume(agent_id)?;
                state.is_paused = false;
                // Also clear degraded model
                if state.degraded_model.is_some() {
                    state.degraded_model = None;
                    self.save_state(&state)?;
                }
            } else {
                return Err(Error::Budget(format!(
                    "Agent {} is paused: {}",
                    agent_id,
                    state.pause_reason.as_deref().unwrap_or("budget exceeded")
                )));
            }
        }

        let budget = self.effective_budget(agent_id);

        if let Some(daily_limit) = budget.daily_amount() {
            if state.daily_spent >= daily_limit {
                if budget.on_exceeded == OnExceeded::Stop {
                    return Err(Error::Budget(format!(
                        "Daily budget exceeded: ${:.2} >= ${:.2}",
                        state.daily_spent, daily_limit
                    )));
                }
                return Ok(false);
            }
        }

        if let Some(monthly_limit) = budget.monthly_amount() {
            if state.monthly_spent >= monthly_limit {
                if budget.on_exceeded == OnExceeded::Stop {
                    return Err(Error::Budget(format!(
                        "Monthly budget exceeded: ${:.2} >= ${:.2}",
                        state.monthly_spent, monthly_limit
                    )));
                }
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Get the degraded model for an agent (if budget degradation is active).
    pub fn degraded_model(&self, agent_id: &str) -> Result<Option<String>> {
        let state = self.get_state(agent_id)?;
        Ok(state.degraded_model)
    }

    fn check_limits(&self, agent_id: &str, state: &BudgetState) -> Result<()> {
        let budget = self.effective_budget(agent_id);
        let mut exceeded = false;
        let mut reason = None;

        if let Some(daily_limit) = budget.daily_amount() {
            if state.daily_spent >= daily_limit {
                exceeded = true;
                reason = Some(format!("Daily limit ${:.2} exceeded", daily_limit));
            }
        }

        if let Some(monthly_limit) = budget.monthly_amount() {
            if state.monthly_spent >= monthly_limit {
                exceeded = true;
                reason = Some(format!("Monthly limit ${:.2} exceeded", monthly_limit));
            }
        }

        if exceeded {
            self.handle_exceeded(agent_id, budget, reason.as_deref())?;
        }

        Ok(())
    }

    fn handle_exceeded(
        &self,
        agent_id: &str,
        budget: &BudgetConfig,
        reason: Option<&str>,
    ) -> Result<()> {
        match budget.on_exceeded {
            OnExceeded::Pause => {
                self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE budget_state SET is_paused = 1, pause_reason = ?1 WHERE agent_id = ?2",
                        rusqlite::params![reason, agent_id],
                    )
                })?;
                warn!(agent_id, ?reason, "agent paused due to budget");
            }
            OnExceeded::Alert => {
                warn!(agent_id, ?reason, "budget alert (agent continues)");
            }
            OnExceeded::Degrade => {
                let fallback = &budget.fallback_model;
                self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE budget_state SET degraded_model = ?1 WHERE agent_id = ?2",
                        rusqlite::params![fallback, agent_id],
                    )
                })?;
                info!(
                    agent_id,
                    fallback_model = fallback.as_str(),
                    ?reason,
                    "degrading to fallback model"
                );
            }
            OnExceeded::Stop => {
                return Err(Error::Budget(
                    reason.unwrap_or("Budget exceeded").to_string(),
                ));
            }
        }
        Ok(())
    }

    pub fn resume(&self, agent_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE budget_state SET is_paused = 0, pause_reason = NULL, degraded_model = NULL WHERE agent_id = ?1",
                [agent_id],
            )
        })?;
        debug!(agent_id, "agent resumed");
        Ok(())
    }

    /// Get budget summary for an agent (or global if agent_id is None).
    pub fn get_summary(&self, agent_id: Option<&str>) -> Result<BudgetSummary> {
        let budget = match agent_id {
            Some(aid) => self.effective_budget(aid),
            None => self.system_budget(),
        };

        let (spent_daily, spent_monthly, spent_total) = if let Some(aid) = agent_id {
            let state = self.get_state(aid)?;
            (state.daily_spent, state.monthly_spent, state.total_spent)
        } else {
            self.db.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT COALESCE(SUM(daily_spent), 0), COALESCE(SUM(monthly_spent), 0), COALESCE(SUM(total_spent), 0) FROM budget_state",
                )?;
                stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                    .map_err(|e| Error::Database(e.to_string()))
            })?
        };

        let (input_tokens, output_tokens, request_count) = self.db.with_conn(|conn| {
            let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) =
                if let Some(aid) = agent_id {
                    (
                        "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COUNT(*) FROM usage_logs WHERE agent_id = ?1",
                        vec![Box::new(aid.to_string())],
                    )
                } else {
                    (
                        "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COUNT(*) FROM usage_logs",
                        vec![],
                    )
                };

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = conn.prepare(sql)?;
            stmt.query_row(param_refs.as_slice(), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(BudgetSummary {
            daily_spent: spent_daily,
            daily_limit: budget.daily_amount(),
            monthly_spent: spent_monthly,
            monthly_limit: budget.monthly_amount(),
            total_spent: spent_total,
            input_tokens,
            output_tokens,
            request_count,
        })
    }

    pub fn get_top_spenders(&self, limit: i64) -> Result<Vec<BudgetState>> {
        self.db.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT * FROM budget_state ORDER BY total_spent DESC LIMIT ?1")?;
            let states = stmt
                .query_map([limit], |row| {
                    Ok(BudgetState {
                        agent_id: row.get("agent_id")?,
                        daily_spent: row.get("daily_spent")?,
                        daily_reset_at: row.get("daily_reset_at")?,
                        monthly_spent: row.get("monthly_spent")?,
                        monthly_reset_at: row.get("monthly_reset_at")?,
                        total_spent: row.get("total_spent")?,
                        is_paused: row.get::<_, i32>("is_paused")? != 0,
                        pause_reason: row.get("pause_reason")?,
                        degraded_model: row.get("degraded_model").unwrap_or(None),
                    })
                })
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(states)
        })
    }

    fn save_state(&self, state: &BudgetState) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO budget_state (agent_id, daily_spent, daily_reset_at, monthly_spent, monthly_reset_at, total_spent, is_paused, pause_reason, degraded_model)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    state.agent_id,
                    state.daily_spent,
                    state.daily_reset_at,
                    state.monthly_spent,
                    state.monthly_reset_at,
                    state.total_spent,
                    state.is_paused as i32,
                    state.pause_reason,
                    state.degraded_model,
                ],
            )
        })?;
        Ok(())
    }

    /// Reset daily/monthly counters if the period has expired.
    /// Returns true if any reset was applied (caller should save state).
    fn maybe_reset(&self, state: &mut BudgetState) -> bool {
        let now = Utc::now();
        let mut changed = false;

        // Daily reset at local midnight
        let needs_daily_reset = match &state.daily_reset_at {
            Some(reset_str) => {
                chrono::NaiveDateTime::parse_from_str(reset_str, "%Y-%m-%d %H:%M:%S")
                    .map(|dt| dt.and_utc() <= now)
                    .unwrap_or(true)
            }
            None => true, // Never set — initialize it
        };

        if needs_daily_reset {
            state.daily_spent = 0.0;
            // Next reset: tomorrow midnight local time
            let tomorrow = Local::now()
                .date_naive()
                .succ_opt()
                .unwrap_or_else(|| Local::now().date_naive() + chrono::Duration::days(1));
            let next_reset = tomorrow.and_hms_opt(0, 0, 0).unwrap().and_utc();
            state.daily_reset_at = Some(next_reset.format("%Y-%m-%d %H:%M:%S").to_string());
            changed = true;
        }

        // Monthly reset on 1st of month
        let needs_monthly_reset = match &state.monthly_reset_at {
            Some(reset_str) => {
                chrono::NaiveDateTime::parse_from_str(reset_str, "%Y-%m-%d %H:%M:%S")
                    .map(|dt| dt.and_utc() <= now)
                    .unwrap_or(true)
            }
            None => true,
        };

        if needs_monthly_reset {
            state.monthly_spent = 0.0;
            // Next reset: 1st of next month local time
            let today = Local::now().date_naive();
            let next_month = if today.month() == 12 {
                chrono::NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)
            } else {
                chrono::NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1)
            }
            .unwrap_or(today + chrono::Duration::days(30));
            let next_reset = next_month.and_hms_opt(0, 0, 0).unwrap().and_utc();
            state.monthly_reset_at = Some(next_reset.format("%Y-%m-%d %H:%M:%S").to_string());
            changed = true;
        }

        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, BudgetManager) {
        let db = Arc::new(Database::open_memory().unwrap());
        let mut config = Config::default();
        config.system.budget.daily = Some("$10.00".to_string());
        config.system.budget.monthly = Some("$100.00".to_string());
        let mgr = BudgetManager::new(db.clone(), Arc::new(config));
        (db, mgr)
    }

    #[test]
    fn test_initial_state() {
        let (_db, mgr) = setup();
        let state = mgr.get_state("atlas").unwrap();
        assert_eq!(state.agent_id, "atlas");
        assert!(state.daily_spent.abs() < 1e-10);
        assert!(!state.is_paused);
        // Reset timestamps should be initialized
        assert!(state.daily_reset_at.is_some());
        assert!(state.monthly_reset_at.is_some());
    }

    #[test]
    fn test_update_spending() {
        let (_db, mgr) = setup();
        let state = mgr.update_spending("atlas", 5.0).unwrap();
        assert!((state.daily_spent - 5.0).abs() < 1e-10);
        assert!((state.total_spent - 5.0).abs() < 1e-10);

        let state = mgr.update_spending("atlas", 3.0).unwrap();
        assert!((state.daily_spent - 8.0).abs() < 1e-10);
    }

    #[test]
    fn test_budget_pause_on_exceed() {
        let (_db, mgr) = setup();
        mgr.update_spending("atlas", 11.0).unwrap();

        let state = mgr.get_state("atlas").unwrap();
        assert!(state.is_paused);
        assert!(state.pause_reason.is_some());
    }

    #[test]
    fn test_budget_check_when_paused() {
        let (_db, mgr) = setup();
        mgr.update_spending("atlas", 11.0).unwrap();

        let result = mgr.check_budget("atlas");
        assert!(result.is_err());
    }

    #[test]
    fn test_resume() {
        let (_db, mgr) = setup();
        mgr.update_spending("atlas", 11.0).unwrap();

        mgr.resume("atlas").unwrap();
        let state = mgr.get_state("atlas").unwrap();
        assert!(!state.is_paused);
    }

    #[test]
    fn test_summary() {
        let (_db, mgr) = setup();
        mgr.update_spending("atlas", 5.0).unwrap();
        mgr.update_spending("hermes", 3.0).unwrap();

        let summary = mgr.get_summary(None).unwrap();
        assert!((summary.daily_spent - 8.0).abs() < 1e-10);
        assert!((summary.daily_limit.unwrap() - 10.0).abs() < 1e-10);

        let agent_summary = mgr.get_summary(Some("atlas")).unwrap();
        assert!((agent_summary.daily_spent - 5.0).abs() < 1e-10);
    }

    #[test]
    fn test_top_spenders() {
        let (_db, mgr) = setup();
        mgr.update_spending("atlas", 5.0).unwrap();
        mgr.update_spending("hermes", 8.0).unwrap();

        let top = mgr.get_top_spenders(10).unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].agent_id, "hermes");
    }

    #[test]
    fn test_stop_policy() {
        let db = Arc::new(Database::open_memory().unwrap());
        let mut config = Config::default();
        config.system.budget.daily = Some("$10.00".to_string());
        config.system.budget.on_exceeded = OnExceeded::Stop;
        let mgr = BudgetManager::new(db, Arc::new(config));

        let result = mgr.update_spending("atlas", 11.0);
        assert!(result.is_err());
    }

    #[test]
    fn test_degrade_policy() {
        let db = Arc::new(Database::open_memory().unwrap());
        let mut config = Config::default();
        config.system.budget.daily = Some("$10.00".to_string());
        config.system.budget.on_exceeded = OnExceeded::Degrade;
        config.system.budget.fallback_model = "local".to_string();
        let mgr = BudgetManager::new(db, Arc::new(config));

        mgr.update_spending("atlas", 11.0).unwrap();

        let state = mgr.get_state("atlas").unwrap();
        assert_eq!(state.degraded_model.as_deref(), Some("local"));
        assert!(!state.is_paused); // Not paused, just degraded
    }

    #[test]
    fn test_per_agent_budget_override() {
        let db = Arc::new(Database::open_memory().unwrap());
        let mut config = Config::default();
        config.system.budget.daily = Some("$10.00".to_string());
        config.agents = vec![crate::config::AgentConfig {
            name: "atlas".to_string(),
            budget: Some(BudgetConfig {
                daily: Some("$5.00".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }];
        let mgr = BudgetManager::new(db, Arc::new(config));

        let budget = mgr.effective_budget("atlas");
        assert!((budget.daily_amount().unwrap() - 5.0).abs() < 1e-10);

        let budget = mgr.effective_budget("hermes");
        assert!((budget.daily_amount().unwrap() - 10.0).abs() < 1e-10);

        mgr.update_spending("atlas", 6.0).unwrap();
        let state = mgr.get_state("atlas").unwrap();
        assert!(state.is_paused);

        mgr.update_spending("hermes", 6.0).unwrap();
        let state = mgr.get_state("hermes").unwrap();
        assert!(!state.is_paused);
    }

    #[test]
    fn test_per_agent_summary_shows_effective_limits() {
        let db = Arc::new(Database::open_memory().unwrap());
        let mut config = Config::default();
        config.system.budget.daily = Some("$10.00".to_string());
        config.agents = vec![crate::config::AgentConfig {
            name: "atlas".to_string(),
            budget: Some(BudgetConfig {
                daily: Some("$5.00".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }];
        let mgr = BudgetManager::new(db, Arc::new(config));

        mgr.update_spending("atlas", 2.0).unwrap();

        let summary = mgr.get_summary(Some("atlas")).unwrap();
        assert!((summary.daily_limit.unwrap() - 5.0).abs() < 1e-10);

        let summary = mgr.get_summary(None).unwrap();
        assert!((summary.daily_limit.unwrap() - 10.0).abs() < 1e-10);
    }

    #[test]
    fn test_auto_resume_after_reset() {
        let (_db, mgr) = setup();

        // Exceed budget — agent gets paused
        mgr.update_spending("atlas", 11.0).unwrap();
        assert!(mgr.check_budget("atlas").is_err());

        // Simulate daily reset by manually clearing spent
        let mut state = mgr.get_state("atlas").unwrap();
        state.daily_spent = 0.0;
        state.monthly_spent = 0.0;
        mgr.save_state(&state).unwrap();

        // check_budget should auto-resume
        let result = mgr.check_budget("atlas");
        assert!(result.is_ok());
        assert!(result.unwrap());

        let state = mgr.get_state("atlas").unwrap();
        assert!(!state.is_paused);
    }
}
