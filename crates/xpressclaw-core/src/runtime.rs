use std::sync::Arc;

use tracing::{error, info};

use crate::agents::registry::AgentRegistry;
use crate::agents::state::AgentStatus;
use crate::config::Config;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::tasks::board::TaskBoard;
use crate::tasks::queue::TaskQueue;

/// The main orchestrator that ties agents, tasks, and the queue together.
pub struct Runtime {
    pub config: Arc<Config>,
    pub db: Arc<Database>,
}

impl Runtime {
    pub async fn new(config: Arc<Config>, db: Arc<Database>) -> Result<Self> {
        Ok(Self { config, db })
    }

    pub fn registry(&self) -> AgentRegistry {
        AgentRegistry::new(self.db.clone())
    }

    pub fn task_board(&self) -> TaskBoard {
        TaskBoard::new(self.db.clone())
    }

    pub fn task_queue(&self) -> TaskQueue {
        TaskQueue::new(self.db.clone())
    }

    /// Start an agent: mark it as running.
    pub async fn start_agent(&self, agent_id: &str) -> Result<()> {
        let registry = self.registry();
        let record = registry.get(agent_id)?;

        if record.status == "running" {
            return Ok(());
        }

        registry.update_status(agent_id, &AgentStatus::Running, None)?;
        info!(agent_id, "agent started");
        Ok(())
    }

    /// Stop an agent: mark it as stopped.
    pub async fn stop_agent(&self, agent_id: &str) -> Result<()> {
        let registry = self.registry();
        let record = registry.get(agent_id)?;

        if record.status == "stopped" {
            return Err(Error::AgentNotRunning {
                name: agent_id.to_string(),
            });
        }

        registry.update_status(agent_id, &AgentStatus::Stopped, None)?;
        info!(agent_id, "agent stopped");
        Ok(())
    }

    /// Start all registered agents.
    pub async fn start_all(&self) -> Result<()> {
        let registry = self.registry();
        let agents = registry.list()?;

        for agent in agents {
            if agent.status != "running" {
                if let Err(e) = self.start_agent(&agent.id).await {
                    error!(agent_id = agent.id, error = %e, "failed to start agent");
                }
            }
        }

        Ok(())
    }

    /// Stop all running agents.
    pub async fn stop_all(&self) -> Result<()> {
        let registry = self.registry();
        let agents = registry.list()?;

        for agent in agents {
            if agent.status == "running" || agent.status == "starting" {
                if let Err(e) = self.stop_agent(&agent.id).await {
                    error!(agent_id = agent.id, error = %e, "failed to stop agent");
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::board::CreateTask;

    fn setup() -> (Arc<Config>, Arc<Database>, Runtime) {
        let config = Arc::new(Config::load_default().unwrap());
        let db = Arc::new(Database::open_memory().unwrap());
        let runtime = Runtime {
            config: config.clone(),
            db: db.clone(),
        };
        (config, db, runtime)
    }

    #[test]
    fn test_registry_through_runtime() {
        let (_, _, runtime) = setup();
        let registry = runtime.registry();

        registry.ensure("atlas", "generic").unwrap();

        let agents = registry.list().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "atlas");
    }

    #[test]
    fn test_task_board_through_runtime() {
        let (_, _, runtime) = setup();
        let board = runtime.task_board();

        let task = board
            .create(&CreateTask {
                title: "Test task".into(),
                description: Some("A task".into()),
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        assert_eq!(task.title, "Test task");
        assert_eq!(task.status.as_str(), "pending");
    }

    #[test]
    fn test_queue_through_runtime() {
        let (_, db, runtime) = setup();

        let board = TaskBoard::new(db);
        let task = board
            .create(&CreateTask {
                title: "Queue test".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        let queue = runtime.task_queue();
        let item = queue.enqueue(&task.id, "atlas").unwrap();
        assert_eq!(item.status, "queued");

        let count = queue.pending_count("atlas").unwrap();
        assert_eq!(count, 1);
    }
}
