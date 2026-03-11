use std::sync::Arc;

use tracing::{debug, error, info, warn};

use crate::agents::harness::HarnessClient;
use crate::agents::registry::AgentRegistry;
use crate::agents::state::AgentStatus;
use crate::config::Config;
use crate::db::Database;
use crate::docker::images::build_container_spec;
use crate::docker::manager::{DockerManager, VolumeMount};
use crate::error::{Error, Result};
use crate::tasks::board::TaskBoard;
use crate::tasks::queue::TaskQueue;

/// The main orchestrator that ties agents, Docker, tasks, and the queue together.
///
/// The runtime:
/// 1. Launches agent containers when started
/// 2. Assigns tasks to agents and dispatches via the queue
/// 3. Sends work to harness containers via the OpenAI-compatible protocol
/// 4. Collects results and updates task status
pub struct Runtime {
    pub config: Arc<Config>,
    pub db: Arc<Database>,
    docker: Option<DockerManager>,
}

impl Runtime {
    /// Create a runtime with a Docker connection.
    pub async fn new(config: Arc<Config>, db: Arc<Database>) -> Result<Self> {
        let docker = match DockerManager::connect().await {
            Ok(d) => {
                info!("connected to container runtime");
                Some(d)
            }
            Err(e) => {
                warn!("Docker/Podman not available: {e}. Agent containers will not be started.");
                None
            }
        };

        Ok(Self { config, db, docker })
    }

    /// Create a runtime without Docker (for testing).
    pub fn without_docker(config: Arc<Config>, db: Arc<Database>) -> Self {
        Self {
            config,
            db,
            docker: None,
        }
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

    /// Start an agent: launch its container and mark it running.
    pub async fn start_agent(&self, agent_id: &str) -> Result<()> {
        let registry = self.registry();
        let record = registry.get(agent_id)?;

        if record.status == "running" {
            return Err(Error::AgentAlreadyRunning {
                name: agent_id.to_string(),
            });
        }

        let docker = self
            .docker
            .as_ref()
            .ok_or_else(|| Error::DockerNotAvailable("no container runtime available".into()))?;

        registry.update_status(agent_id, &AgentStatus::Starting, None)?;

        // Build container spec from agent config
        let agent_config = self
            .config
            .agents
            .iter()
            .find(|a| a.name == agent_id);

        let mut spec = if let Some(ac) = agent_config {
            build_container_spec(
                ac,
                8935, // default server port
                self.config.llm.anthropic_api_key.as_deref(),
                self.config.llm.openai_api_key.as_deref(),
                self.config.llm.openai_base_url.as_deref(),
            )
        } else {
            // Fallback: minimal spec from registry data
            let mut spec = crate::docker::manager::ContainerSpec {
                image: crate::docker::images::image_for_backend(&record.backend).to_string(),
                ..Default::default()
            };
            spec.environment.push(format!("AGENT_ID={agent_id}"));
            spec.environment.push(format!("AGENT_NAME={}", record.name));
            spec.environment.push(format!("AGENT_BACKEND={}", record.backend));
            spec
        };

        // Always mount workspace dir as /workspace (read-write)
        let workspace = self.config.system.workspace_dir.display().to_string();
        spec.volumes.push(VolumeMount {
            source: workspace,
            target: "/workspace".to_string(),
            read_only: false,
        });

        match docker.launch(agent_id, &spec).await {
            Ok(info) => {
                info!(agent_id, container_id = %info.container_id, "agent container started");
                registry.update_status(
                    agent_id,
                    &AgentStatus::Running,
                    Some(&info.container_id),
                )?;
                Ok(())
            }
            Err(e) => {
                error!(agent_id, error = %e, "failed to start agent container");
                registry.update_status(
                    agent_id,
                    &AgentStatus::Error(e.to_string()),
                    None,
                )?;
                Err(e)
            }
        }
    }

    /// Stop an agent: stop its container and mark it stopped.
    pub async fn stop_agent(&self, agent_id: &str) -> Result<()> {
        let registry = self.registry();
        let record = registry.get(agent_id)?;

        if record.status == "stopped" {
            return Err(Error::AgentNotRunning {
                name: agent_id.to_string(),
            });
        }

        if let Some(docker) = &self.docker {
            if let Err(e) = docker.stop(agent_id).await {
                warn!(agent_id, error = %e, "failed to stop container (may already be stopped)");
            }
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

        // Also clean up any orphaned containers
        if let Some(docker) = &self.docker {
            if let Err(e) = docker.stop_all().await {
                warn!(error = %e, "failed to stop orphaned containers");
            }
        }

        Ok(())
    }

    /// Dispatch a task to the appropriate agent's harness container.
    ///
    /// 1. Enqueues the task in the queue
    /// 2. Finds the agent's container host port
    /// 3. Sends the task prompt to the harness
    /// 4. Updates queue and task status with the result
    pub async fn dispatch_task(&self, task_id: &str, agent_id: &str) -> Result<String> {
        let board = self.task_board();
        let queue = self.task_queue();
        let registry = self.registry();

        // Verify agent exists and is running
        let agent = registry.get(agent_id)?;
        if agent.status != "running" {
            return Err(Error::AgentNotRunning {
                name: agent_id.to_string(),
            });
        }

        // Get the task
        let task = board.get(task_id)?;

        // Enqueue
        let queue_item = queue.enqueue(task_id, agent_id)?;
        debug!(task_id, agent_id, queue_id = queue_item.id, "task dispatched to queue");

        // Update task status to in_progress
        board.update_status(task_id, "in_progress", Some(agent_id))?;

        // Find the container's host port
        let docker = self
            .docker
            .as_ref()
            .ok_or_else(|| Error::DockerNotAvailable("no container runtime".into()))?;

        let containers = docker.list().await?;
        let container = containers
            .iter()
            .find(|c| c.agent_id == agent_id)
            .ok_or_else(|| Error::Container(format!("no container found for agent {agent_id}")))?;

        let host_port = container
            .host_port
            .ok_or_else(|| Error::Container(format!("no host port for agent {agent_id}")))?;

        // Build the harness client
        let harness = HarnessClient::new(host_port);

        // Wait for harness to be ready
        if !harness.health_check().await {
            let err = format!("harness for agent {agent_id} is not healthy");
            queue.fail(queue_item.id, &err)?;
            board.update_status(task_id, "failed", Some(agent_id))?;
            return Err(Error::Agent(err));
        }

        // Build prompt from task
        let system_prompt = agent
            .config
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("You are a helpful assistant.");

        let task_prompt = if let Some(desc) = &task.description {
            format!("Task: {}\n\nDescription: {}", task.title, desc)
        } else {
            format!("Task: {}", task.title)
        };

        let model = agent
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("default");

        // Send to harness
        match harness.send_task(system_prompt, &task_prompt, model).await {
            Ok(response) => {
                let result_text = response
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default();

                queue.complete(queue_item.id, &result_text)?;
                board.update_status(task_id, "completed", Some(agent_id))?;

                info!(task_id, agent_id, "task completed successfully");
                Ok(result_text)
            }
            Err(e) => {
                let err_msg = e.to_string();
                queue.fail(queue_item.id, &err_msg)?;
                board.update_status(task_id, "failed", Some(agent_id))?;

                error!(task_id, agent_id, error = %e, "task execution failed");
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// Parse a volume mount string in the format "source:target[:ro]".
    fn parse_volume_mount(spec: &str) -> Option<crate::docker::manager::VolumeMount> {
        let parts: Vec<&str> = spec.splitn(3, ':').collect();
        if parts.len() < 2 {
            return None;
        }

        let read_only = parts.get(2).map(|s| *s == "ro").unwrap_or(false);

        Some(crate::docker::manager::VolumeMount {
            source: parts[0].to_string(),
            target: parts[1].to_string(),
            read_only,
        })
    }
    use super::*;
    use crate::agents::registry::RegisterAgent;
    use crate::tasks::board::CreateTask;

    fn setup() -> (Arc<Config>, Arc<Database>, Runtime) {
        let config = Arc::new(Config::load_default().unwrap());
        let db = Arc::new(Database::open_memory().unwrap());
        let runtime = Runtime::without_docker(config.clone(), db.clone());
        (config, db, runtime)
    }

    #[test]
    fn test_registry_through_runtime() {
        let (_, _, runtime) = setup();
        let registry = runtime.registry();

        registry
            .register(&RegisterAgent {
                name: "atlas".into(),
                backend: "generic".into(),
                config: serde_json::json!({"role": "test agent"}),
            })
            .unwrap();

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

        // Create a task first (for foreign key)
        let board = TaskBoard::new(db);
        let task = board
            .create(&CreateTask {
                title: "Queue test".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
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

    #[tokio::test]
    async fn test_start_agent_without_docker() {
        let (_, _, runtime) = setup();
        let registry = runtime.registry();

        registry
            .register(&RegisterAgent {
                name: "atlas".into(),
                backend: "generic".into(),
                config: serde_json::json!({}),
            })
            .unwrap();

        // Should fail because no Docker
        let result = runtime.start_agent("atlas").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::DockerNotAvailable(_) => {} // expected
            e => panic!("unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_fails_when_agent_not_running() {
        let (_, db, runtime) = setup();
        let registry = runtime.registry();

        registry
            .register(&RegisterAgent {
                name: "atlas".into(),
                backend: "generic".into(),
                config: serde_json::json!({}),
            })
            .unwrap();

        let board = TaskBoard::new(db);
        let task = board
            .create(&CreateTask {
                title: "Dispatch test".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        let result = runtime.dispatch_task(&task.id, "atlas").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::AgentNotRunning { name } => assert_eq!(name, "atlas"),
            e => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn test_parse_volume_mount() {
        let m = parse_volume_mount("/home/user/project:/workspace").unwrap();
        assert_eq!(m.source, "/home/user/project");
        assert_eq!(m.target, "/workspace");
        assert!(!m.read_only);

        let m = parse_volume_mount("/data:/mnt/data:ro").unwrap();
        assert_eq!(m.source, "/data");
        assert_eq!(m.target, "/mnt/data");
        assert!(m.read_only);

        let m = parse_volume_mount("/data:/mnt/data:rw").unwrap();
        assert!(!m.read_only);

        assert!(parse_volume_mount("invalid").is_none());
    }
}
