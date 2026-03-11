use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::{BudgetConfig, OnExceeded};
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

/// Budget enforcement and policies.
pub struct BudgetManager {
    db: Arc<Database>,
    config: BudgetConfig,
}

impl BudgetManager {
    pub fn new(db: Arc<Database>, config: BudgetConfig) -> Self {
        Self { db, config }
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
                })
            })
        });

        match result {
            Ok(state) => Ok(state),
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
                };
                self.save_state(&state)?;
                Ok(state)
            }
            Err(e) => Err(Error::Database(e.to_string())),
        }
    }

    pub fn update_spending(&self, agent_id: &str, cost: f64) -> Result<BudgetState> {
        let mut state = self.get_state(agent_id)?;

        state.daily_spent += cost;
        state.monthly_spent += cost;
        state.total_spent += cost;

        self.save_state(&state)?;
        self.check_limits(agent_id, &state)?;

        debug!(agent_id, cost, total = state.total_spent, "updated spending");
        Ok(state)
    }

    pub fn check_budget(&self, agent_id: &str) -> Result<bool> {
        let state = self.get_state(agent_id)?;

        if state.is_paused {
            return Err(Error::Budget(format!(
                "Agent {} is paused: {}",
                agent_id,
                state.pause_reason.as_deref().unwrap_or("budget exceeded")
            )));
        }

        if let Some(daily_limit) = self.config.daily_amount() {
            if state.daily_spent >= daily_limit {
                if self.config.on_exceeded == OnExceeded::Stop {
                    return Err(Error::Budget(format!(
                        "Daily budget exceeded: ${:.2} >= ${:.2}",
                        state.daily_spent, daily_limit
                    )));
                }
                return Ok(false);
            }
        }

        if let Some(monthly_limit) = self.config.monthly_amount() {
            if state.monthly_spent >= monthly_limit {
                if self.config.on_exceeded == OnExceeded::Stop {
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

    fn check_limits(&self, agent_id: &str, state: &BudgetState) -> Result<()> {
        let mut exceeded = false;
        let mut reason = None;

        if let Some(daily_limit) = self.config.daily_amount() {
            if state.daily_spent >= daily_limit {
                exceeded = true;
                reason = Some(format!("Daily limit ${:.2} exceeded", daily_limit));
            }
        }

        if let Some(monthly_limit) = self.config.monthly_amount() {
            if state.monthly_spent >= monthly_limit {
                exceeded = true;
                reason = Some(format!("Monthly limit ${:.2} exceeded", monthly_limit));
            }
        }

        if exceeded {
            self.handle_exceeded(agent_id, reason.as_deref())?;
        }

        Ok(())
    }

    fn handle_exceeded(&self, agent_id: &str, reason: Option<&str>) -> Result<()> {
        match self.config.on_exceeded {
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
                warn!(agent_id, ?reason, "budget alert");
            }
            OnExceeded::Degrade => {
                debug!(agent_id, ?reason, "degrading to local model");
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
                "UPDATE budget_state SET is_paused = 0, pause_reason = NULL WHERE agent_id = ?1",
                [agent_id],
            )
        })?;
        debug!(agent_id, "agent resumed");
        Ok(())
    }

    pub fn get_summary(&self, agent_id: Option<&str>) -> Result<BudgetSummary> {
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
            let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(aid) = agent_id {
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

            let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = conn.prepare(sql)?;
            stmt.query_row(param_refs.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
            })
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(BudgetSummary {
            daily_spent: spent_daily,
            daily_limit: self.config.daily_amount(),
            monthly_spent: spent_monthly,
            monthly_limit: self.config.monthly_amount(),
            total_spent: spent_total,
            input_tokens,
            output_tokens,
            request_count,
        })
    }

    pub fn get_top_spenders(&self, limit: i64) -> Result<Vec<BudgetState>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT * FROM budget_state ORDER BY total_spent DESC LIMIT ?1",
            )?;
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
                "INSERT OR REPLACE INTO budget_state (agent_id, daily_spent, daily_reset_at, monthly_spent, monthly_reset_at, total_spent, is_paused, pause_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    state.agent_id,
                    state.daily_spent,
                    state.daily_reset_at,
                    state.monthly_spent,
                    state.monthly_reset_at,
                    state.total_spent,
                    state.is_paused as i32,
                    state.pause_reason,
                ],
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, BudgetManager) {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = BudgetConfig {
            daily: Some("$10.00".to_string()),
            monthly: Some("$100.00".to_string()),
            ..Default::default()
        };
        let mgr = BudgetManager::new(db.clone(), config);
        (db, mgr)
    }

    #[test]
    fn test_initial_state() {
        let (_db, mgr) = setup();
        let state = mgr.get_state("atlas").unwrap();
        assert_eq!(state.agent_id, "atlas");
        assert!(state.daily_spent.abs() < 1e-10);
        assert!(!state.is_paused);
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
        let config = BudgetConfig {
            daily: Some("$10.00".to_string()),
            on_exceeded: OnExceeded::Stop,
            ..Default::default()
        };
        let mgr = BudgetManager::new(db, config);

        let result = mgr.update_spending("atlas", 11.0);
        assert!(result.is_err());
    }
}
