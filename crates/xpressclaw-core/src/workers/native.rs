//! Dispatcher and adapters for native coding-agent CLIs running in retained
//! project environments.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    EnvVariable, HttpHeader, McpServer, McpServerHttp, McpServerSse, McpServerStdio,
};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{
    Mutex as AsyncMutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock as AsyncRwLock,
    Semaphore,
};
use tracing::{error, info, warn};

use crate::acp::{
    agent_definition, canonical_agent_kind, infer_agent_kind_from_backend, local_runner_image,
};
use crate::agents::registry::AgentRegistry;
use crate::collaboration::{network_name as collaboration_network_name, CollaborationSecrets};
use crate::config::{
    default_native_runner_image, AgentConfig, Config, ContainerEngineAccess, McpServerConfig,
    NativeRunnerConfig,
};
use crate::conversations::event_bus::{ConversationEvent, ConversationEventBus};
use crate::conversations::runtime::{ConversationTurn, ConversationTurnQueue};
use crate::conversations::{ConversationManager, NewConversationAttachment, SendMessage};
use crate::dashboard::DashboardManager;
use crate::db::Database;
use crate::docker::manager::{
    container_spec_fingerprint, ContainerSpec, DockerManager, SelinuxRelabel, VolumeMount,
};
use crate::error::{Error, Result};
use crate::message_artifacts::{bound_published_file_name, prepare_message_artifacts};
use crate::repositories::{
    agent_callback_capability, discover_github_access, run_repository_blocking,
    AgentRepositoryManager, RepositoryBoundaryResult, RepositoryInspection,
};
use crate::sessions::SessionManager;
use crate::tasks::board::{Task, TaskBoard};
use crate::tasks::conversation::{FinalAssistantAttempt, PromptImageAttachment, TaskConversation};
use crate::tasks::queue::{QueueItem, TaskQueue};
use crate::visualizations::{
    is_absolute_runner_root, prepare_message_visualizations, PreparedVisualization,
    VisualizationSourceRoot,
};
use crate::workers::acp::{
    AcpElicitationBroker, AcpEventRecorder, AcpProcess, AcpSessionStart, AcpTurnControlBroker,
    AcpTurnOptions, AcpTurnRuntime,
};
use crate::workers::github;
use crate::workers::presentations::{
    configure_codex_presentations, PRESENTATION_CAPABILITY, PRESENTATION_CAPABILITY_LABEL,
    PRESENTATION_RUNTIME_VERSION, PRESENTATION_RUNTIME_VERSION_LABEL,
};

const BUILT_IN_RUNNER_PROTOCOL: &str = "acp-xpressclaw-v2";
const PI_MCP_BRIDGE_LABEL: &str = "pi-config-v1";
const PI_MCP_CONFIG_TARGET: &str = "/run/xpressclaw/pi-mcp/config.json";
const PI_MCP_CONFIG_DIR_TARGET: &str = "/run/xpressclaw/pi-mcp";
const PI_MCP_WRAPPER: &str = "/opt/xpressclaw/pi-with-mcp";
static PI_MCP_CONFIG_LOCK: StdMutex<()> = StdMutex::new(());
static GIT_WORKTREE_REDIRECT_LOCK: StdMutex<()> = StdMutex::new(());
const BUNDLED_CONTROL_MCP_COMMAND: &str = "/usr/local/bin/node";
const CODEX_INITIAL_AGENT_MODE: &str = "INITIAL_AGENT_MODE";
const CODEX_FULL_ACCESS_MODE: &str = "agent-full-access";
const DOCKER_DESKTOP_SSH_AGENT_SOURCE: &str = "/run/host-services/ssh-auth.sock";
const SSH_AGENT_SOCKET_TARGET: &str = "/tmp/xpressclaw-ssh-agent.sock";
const SSH_CONFIG_TARGET: &str = "/tmp/xpressclaw-host-ssh-config";
const SSH_KNOWN_HOSTS_TARGET: &str = "/tmp/xpressclaw-host-known-hosts";
const SSH_RUNTIME_DIR_TARGET: &str = "/run/xpressclaw/ssh";
const SSH_RETAINED_KNOWN_HOSTS: &str = "/run/xpressclaw/ssh/known_hosts";
const SSH_CONFIG_INCLUDE_LIMIT: usize = 128;
const SSH_CONFIG_FILE_SIZE_LIMIT: u64 = 1024 * 1024;
const BUNDLED_CONTROL_MCP_SOURCE: &str = concat!(
    include_str!("../../../../harnesses/native/common/mcp-xpressclaw.mjs"),
    "\nawait main();\n"
);

struct NativeAttemptRuntime {
    db: Arc<Database>,
    config: Arc<Config>,
    docker: Arc<DockerManager>,
    event_bus: Arc<ConversationEventBus>,
    elicitation_broker: Arc<AcpElicitationBroker>,
    turn_controls: Arc<AcpTurnControlBroker>,
    processes: Arc<ProjectAcpProcesses>,
    conversation_processes: Arc<ConversationAcpProcesses>,
    runtime_lifecycle: Arc<NativeRuntimeLifecycle>,
    control_plane_port: u16,
    control_plane_token: Arc<str>,
}

#[derive(Clone)]
struct ProjectAcpProcess {
    fingerprint: String,
    container_id: String,
    process: AcpProcess,
}

#[derive(Default)]
struct ProjectAcpProcesses {
    slots: StdMutex<HashMap<String, Arc<AsyncMutex<Option<ProjectAcpProcess>>>>>,
}

#[derive(Clone)]
struct ConversationAcpProcess {
    fingerprint: String,
    container_id: String,
    process: AcpProcess,
}

#[derive(Debug)]
struct PiMcpBridge {
    signature: String,
    process_environment: Vec<String>,
}

#[derive(Default)]
pub struct ConversationAcpProcesses {
    slots: StdMutex<HashMap<String, Arc<AsyncMutex<Option<ConversationAcpProcess>>>>>,
}

/// Per-Agent barrier covering native runtime preparation, launch, and use.
///
/// Destructive Project lifecycle operations take the write side for their
/// stable Agent IDs after signalling cancellation. This waits for workers that
/// already passed their durable queue checks and prevents a claimed worker
/// from launching a new retained runtime until deletion has finalized.
#[derive(Default)]
pub struct NativeRuntimeLifecycle {
    slots: StdMutex<HashMap<String, Arc<AsyncRwLock<()>>>>,
}

impl NativeRuntimeLifecycle {
    fn slot(&self, agent_id: &str) -> Arc<AsyncRwLock<()>> {
        self.slots
            .lock()
            .unwrap()
            .entry(agent_id.to_string())
            .or_insert_with(|| Arc::new(AsyncRwLock::new(())))
            .clone()
    }

    /// Enter a runtime operation that may create, replace, or use resources
    /// owned by an Agent. Reconcilers and dispatchers must acquire this before
    /// acting on a durable desired-state snapshot so Project deletion can
    /// quiesce every launch path with the write side of the same barrier.
    pub(crate) async fn enter(&self, agent_id: &str) -> OwnedRwLockReadGuard<()> {
        self.slot(agent_id).read_owned().await
    }

    /// Wait for all in-flight native work using these stable Agent IDs and
    /// prevent new work from entering until the returned guards are dropped.
    pub async fn quiesce_agents(&self, agent_ids: &[String]) -> Vec<OwnedRwLockWriteGuard<()>> {
        let mut agent_ids = agent_ids.to_vec();
        agent_ids.sort();
        agent_ids.dedup();
        let slots = agent_ids
            .iter()
            .map(|agent_id| self.slot(agent_id))
            .collect::<Vec<_>>();
        let mut guards = Vec::with_capacity(slots.len());
        for slot in slots {
            guards.push(slot.write_owned().await);
        }
        guards
    }
}

impl ConversationAcpProcesses {
    fn key(conversation_id: &str, agent_id: &str) -> String {
        format!("{conversation_id}\u{0}{agent_id}")
    }

    fn slot(
        &self,
        conversation_id: &str,
        agent_id: &str,
    ) -> Arc<AsyncMutex<Option<ConversationAcpProcess>>> {
        let key = Self::key(conversation_id, agent_id);
        self.slots
            .lock()
            .unwrap()
            .entry(key)
            .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
            .clone()
    }

    async fn get_or_start(
        &self,
        docker: &DockerManager,
        conversation_id: &str,
        agent_id: &str,
        base: &ProjectAcpProcess,
        spec: &ContainerSpec,
        process_environment: &[String],
    ) -> Result<ConversationAcpProcess> {
        let fingerprint = container_spec_fingerprint(spec)?;
        let slot = self.slot(conversation_id, agent_id);
        let mut entry = slot.lock().await;
        if entry.as_ref().is_some_and(|current| {
            current.fingerprint == fingerprint
                && current.container_id == base.container_id
                && current.process.is_alive()
        }) {
            return Ok(entry.as_ref().unwrap().clone());
        }
        entry.take();
        let command = spec.cmd.as_deref().ok_or_else(|| {
            Error::Backend("the selected ACP runner has no process command".into())
        })?;
        let attached = docker
            .open_project_process(
                agent_id,
                command,
                spec.working_dir.as_deref(),
                process_environment,
            )
            .await?;
        let process = AcpProcess::start(attached).await?;
        let started = ConversationAcpProcess {
            fingerprint,
            container_id: base.container_id.clone(),
            process,
        };
        *entry = Some(started.clone());
        Ok(started)
    }

    async fn invalidate(&self, conversation_id: &str, agent_id: &str, process: &AcpProcess) {
        let key = Self::key(conversation_id, agent_id);
        let Some(slot) = self.slots.lock().unwrap().get(&key).cloned() else {
            return;
        };
        let mut entry = slot.lock().await;
        if entry
            .as_ref()
            .is_some_and(|current| current.process.same_process(process))
        {
            entry.take();
        }
    }

    /// Remove and close every retained ACP lane for a deleted Conversation.
    /// The shared project container and its task process remain untouched.
    pub async fn retire_conversation(&self, conversation_id: &str) -> usize {
        let prefix = format!("{conversation_id}\u{0}");
        let slots = {
            let mut registered = self.slots.lock().unwrap();
            let keys = registered
                .keys()
                .filter(|key| key.starts_with(&prefix))
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| registered.remove(&key))
                .collect::<Vec<_>>()
        };
        Self::shutdown_slots(slots).await
    }

    /// Remove and close one Agent's lane after it leaves a Conversation.
    pub async fn retire_agent(&self, conversation_id: &str, agent_id: &str) -> usize {
        let slot = self
            .slots
            .lock()
            .unwrap()
            .remove(&Self::key(conversation_id, agent_id));
        Self::shutdown_slots(slot.into_iter().collect()).await
    }

    /// Remove and close an Agent's retained lanes in every Conversation while
    /// leaving the shared project container and all other Agent lanes intact.
    pub async fn retire_agent_everywhere(&self, agent_id: &str) -> usize {
        let suffix = format!("\u{0}{agent_id}");
        let slots = {
            let mut registered = self.slots.lock().unwrap();
            let keys = registered
                .keys()
                .filter(|key| key.ends_with(&suffix))
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| registered.remove(&key))
                .collect::<Vec<_>>()
        };
        Self::shutdown_slots(slots).await
    }

    async fn shutdown_slots(slots: Vec<Arc<AsyncMutex<Option<ConversationAcpProcess>>>>) -> usize {
        let count = slots.len();
        let mut processes = Vec::new();
        for slot in slots {
            if let Some(process) = slot.lock().await.take() {
                process.process.shutdown();
                processes.push(process.process);
            }
        }
        if !processes.is_empty() {
            let waits = processes
                .into_iter()
                .map(|process| async move { process.wait_for_exit().await });
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                futures_util::future::join_all(waits),
            )
            .await;
        }
        count
    }

    #[cfg(test)]
    fn slot_count(&self) -> usize {
        self.slots.lock().unwrap().len()
    }
}

impl ProjectAcpProcesses {
    fn slot(&self, agent_id: &str) -> Arc<AsyncMutex<Option<ProjectAcpProcess>>> {
        self.slots
            .lock()
            .unwrap()
            .entry(agent_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
            .clone()
    }

    async fn get_or_start(
        &self,
        docker: &DockerManager,
        agent_id: &str,
        spec: &ContainerSpec,
    ) -> Result<ProjectAcpProcess> {
        let fingerprint = container_spec_fingerprint(spec)?;
        let slot = self.slot(agent_id);
        let mut entry = slot.lock().await;
        let reusable = if let Some(current) = entry.as_ref() {
            current.fingerprint == fingerprint
                && current.process.is_alive()
                && docker.is_running(agent_id).await
                && docker.project_container_matches(agent_id, spec).await
        } else {
            false
        };
        if reusable {
            return Ok(entry.as_ref().unwrap().clone());
        }

        let previous = entry.take();
        docker.stop_preserving(agent_id).await?;
        if let Some(previous) = previous {
            let _ = tokio::time::timeout(Duration::from_secs(2), previous.process.wait_for_exit())
                .await;
        }
        let attached = docker.launch_project_attached(agent_id, spec).await?;
        let container_id = attached.info.container_id.clone();
        let process = match AcpProcess::start(attached).await {
            Ok(process) => process,
            Err(error) => {
                let _ = docker.stop_preserving(agent_id).await;
                return Err(error);
            }
        };
        let started = ProjectAcpProcess {
            fingerprint,
            container_id,
            process,
        };
        *entry = Some(started.clone());
        Ok(started)
    }

    async fn invalidate(&self, agent_id: &str, process: &AcpProcess) -> bool {
        let slot = self.slot(agent_id);
        let mut entry = slot.lock().await;
        if entry
            .as_ref()
            .is_some_and(|current| current.process.same_process(process))
        {
            entry.take();
            true
        } else {
            false
        }
    }

    async fn retire_agent(&self, agent_id: &str) -> bool {
        let slot = self.slots.lock().unwrap().remove(agent_id);
        let Some(slot) = slot else {
            return false;
        };
        let Some(process) = slot.lock().await.take() else {
            return false;
        };
        process.process.shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(2), process.process.wait_for_exit()).await;
        true
    }
}

/// Shared control-plane services used by both task and Conversation ACP
/// dispatch. Keeping these together also guarantees that HTTP lifecycle
/// handlers and the background workers refer to the same process registry.
pub struct NativeDispatcherServices {
    pub event_bus: Arc<ConversationEventBus>,
    pub elicitation_broker: Arc<AcpElicitationBroker>,
    pub turn_controls: Arc<AcpTurnControlBroker>,
    pub conversation_processes: Arc<ConversationAcpProcesses>,
    /// Per-Agent launch/use barrier shared with destructive lifecycle routes.
    pub runtime_lifecycle: Arc<NativeRuntimeLifecycle>,
    /// Ephemeral capability used only on the container callback listener.
    pub control_plane_token: Arc<str>,
}

/// Consume the durable task queue as an Agent Client Protocol client. Each
/// project gets one retained container and initialized ACP process that are
/// reused across ordinary prompt turns.
pub async fn start_dispatcher(
    db: Arc<Database>,
    config: Arc<RwLock<Arc<Config>>>,
    initial_docker: Option<Arc<DockerManager>>,
    services: NativeDispatcherServices,
    control_plane_port: u16,
) {
    info!("native attempt dispatcher started");
    let NativeDispatcherServices {
        event_bus,
        elicitation_broker,
        turn_controls,
        conversation_processes,
        runtime_lifecycle,
        control_plane_token,
    } = services;
    let installation_id = match db.installation_id() {
        Ok(installation_id) => installation_id,
        Err(error) => {
            warn!(%error, "native dispatcher could not load its installation identity");
            return;
        }
    };
    let concurrency = Arc::new(Semaphore::new(4));
    let processes = Arc::new(ProjectAcpProcesses::default());
    let mut docker = initial_docker;

    let _ = ConversationTurnQueue::new(db.clone()).recover();
    let conversation_db = db.clone();
    let conversation_config = config.clone();
    let conversation_docker = docker.clone();
    let conversation_bus = event_bus.clone();
    let conversation_elicitations = elicitation_broker.clone();
    let conversation_controls = turn_controls.clone();
    let conversation_base_processes = processes.clone();
    let conversation_runtime_lifecycle = runtime_lifecycle.clone();
    let conversation_control_token = control_plane_token.clone();
    let task_conversation_processes = conversation_processes.clone();
    tokio::spawn(async move {
        start_conversation_dispatcher(
            conversation_db,
            conversation_config,
            conversation_docker,
            conversation_bus,
            conversation_elicitations,
            conversation_controls,
            conversation_base_processes,
            conversation_processes,
            conversation_runtime_lifecycle,
            control_plane_port,
            conversation_control_token,
        )
        .await;
    });

    loop {
        let docker = match docker.clone() {
            Some(docker) => docker,
            None => match DockerManager::connect_for_installation(&installation_id).await {
                Ok(connected) => {
                    let connected = Arc::new(connected);
                    docker = Some(connected.clone());
                    connected
                }
                Err(_) => {
                    warn!("native dispatcher is waiting for Docker/Podman");
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    continue;
                }
            },
        };

        let permit = match concurrency.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return,
        };
        let queue = TaskQueue::new(db.clone());
        match queue.claim_next() {
            Ok(Some(item)) => {
                let db = db.clone();
                let config = config.read().unwrap().clone();
                let event_bus = event_bus.clone();
                let elicitation_broker = elicitation_broker.clone();
                let turn_controls = turn_controls.clone();
                let processes = processes.clone();
                let conversation_processes = task_conversation_processes.clone();
                let runtime_lifecycle = runtime_lifecycle.clone();
                let control_plane_token = control_plane_token.clone();
                if let Some(attempt_id) = item.attempt_id.as_deref() {
                    turn_controls.begin_attempt(attempt_id);
                }
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = execute_item(
                        NativeAttemptRuntime {
                            db: db.clone(),
                            config,
                            docker,
                            event_bus: event_bus.clone(),
                            elicitation_broker,
                            turn_controls: turn_controls.clone(),
                            processes,
                            conversation_processes,
                            runtime_lifecycle,
                            control_plane_port,
                            control_plane_token,
                        },
                        item.clone(),
                    )
                    .await;
                    if let Some(attempt_id) = item.attempt_id.as_deref() {
                        turn_controls.finish_attempt(attempt_id);
                    }
                    if let Err(error) = result {
                        error!(
                            queue_id = item.id,
                            task_id = item.task_id,
                            error = %error,
                            "native work attempt failed"
                        );
                        let _ = fail_item(&db, &item, &error.to_string(), &event_bus);
                    }
                });
            }
            Ok(None) => {
                drop(permit);
                tokio::time::sleep(Duration::from_millis(750)).await;
            }
            Err(error) => {
                drop(permit);
                warn!(error = %error, "failed to claim native work");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_conversation_dispatcher(
    db: Arc<Database>,
    config: Arc<RwLock<Arc<Config>>>,
    initial_docker: Option<Arc<DockerManager>>,
    event_bus: Arc<ConversationEventBus>,
    elicitation_broker: Arc<AcpElicitationBroker>,
    turn_controls: Arc<AcpTurnControlBroker>,
    project_processes: Arc<ProjectAcpProcesses>,
    conversation_processes: Arc<ConversationAcpProcesses>,
    runtime_lifecycle: Arc<NativeRuntimeLifecycle>,
    control_plane_port: u16,
    control_plane_token: Arc<str>,
) {
    info!("conversation ACP dispatcher started");
    let installation_id = match db.installation_id() {
        Ok(installation_id) => installation_id,
        Err(error) => {
            warn!(%error, "conversation dispatcher could not load installation identity");
            return;
        }
    };
    let concurrency = Arc::new(Semaphore::new(8));
    let mut docker = initial_docker;
    loop {
        let docker = match docker.clone() {
            Some(docker) => docker,
            None => match DockerManager::connect_for_installation(&installation_id).await {
                Ok(connected) => {
                    let connected = Arc::new(connected);
                    docker = Some(connected.clone());
                    connected
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    continue;
                }
            },
        };
        let permit = match concurrency.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return,
        };
        match ConversationTurnQueue::new(db.clone()).claim_next() {
            Ok(Some(turn)) => {
                let runtime = ConversationAttemptRuntime {
                    db: db.clone(),
                    config: config.read().unwrap().clone(),
                    docker,
                    event_bus: event_bus.clone(),
                    elicitation_broker: elicitation_broker.clone(),
                    turn_controls: turn_controls.clone(),
                    project_processes: project_processes.clone(),
                    conversation_processes: conversation_processes.clone(),
                    runtime_lifecycle: runtime_lifecycle.clone(),
                    control_plane_port,
                    control_plane_token: control_plane_token.clone(),
                };
                let failure_db = runtime.db.clone();
                let failure_bus = runtime.event_bus.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = execute_conversation_turn(runtime, turn.clone()).await {
                        warn!(
                            conversation_id = turn.conversation_id,
                            agent_id = turn.agent_id,
                            %error,
                            "conversation turn failed"
                        );
                        let _ =
                            ConversationTurnQueue::new(failure_db).fail(&turn, &error.to_string());
                        failure_bus.send(
                            &turn.conversation_id,
                            ConversationEvent::Error {
                                agent_id: Some(turn.agent_id.clone()),
                                error: error.to_string(),
                            },
                        );
                        failure_bus.send(&turn.conversation_id, ConversationEvent::Done);
                    }
                });
            }
            Ok(None) => {
                drop(permit);
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) => {
                drop(permit);
                warn!(%error, "failed to claim conversation turn");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

struct ConversationAttemptRuntime {
    db: Arc<Database>,
    config: Arc<Config>,
    docker: Arc<DockerManager>,
    event_bus: Arc<ConversationEventBus>,
    elicitation_broker: Arc<AcpElicitationBroker>,
    turn_controls: Arc<AcpTurnControlBroker>,
    project_processes: Arc<ProjectAcpProcesses>,
    conversation_processes: Arc<ConversationAcpProcesses>,
    runtime_lifecycle: Arc<NativeRuntimeLifecycle>,
    control_plane_port: u16,
    control_plane_token: Arc<str>,
}

async fn execute_conversation_turn(
    runtime: ConversationAttemptRuntime,
    turn: ConversationTurn,
) -> Result<()> {
    let ConversationAttemptRuntime {
        db,
        config,
        docker,
        event_bus,
        elicitation_broker,
        turn_controls,
        project_processes,
        conversation_processes,
        runtime_lifecycle,
        control_plane_port,
        control_plane_token,
    } = runtime;
    let queue = ConversationTurnQueue::new(db.clone());
    if !queue.is_running(&turn.id)? {
        return Ok(());
    }
    let manager = ConversationManager::new(db.clone());
    let conversation = manager.get(&turn.conversation_id)?;
    let agent = config
        .agents
        .iter()
        .find(|agent| agent.name == turn.agent_id)
        .ok_or_else(|| Error::AgentNotFound {
            name: turn.agent_id.clone(),
        })?;
    let kind = resolve_runner_kind(agent)?;
    let (_runtime_lifecycle_guard, repository) = prepare_repository_for_turn(
        &db,
        &config,
        agent,
        &docker,
        &project_processes,
        &conversation_processes,
        &runtime_lifecycle,
    )
    .await?;
    if !queue.is_running(&turn.id)? {
        return Ok(());
    }
    let github = if repository.active {
        discover_github_access(&db, &repository.active_root).await?
    } else {
        None
    };
    let mut spec = build_spec(&config, agent, &kind, &docker, &repository, github.as_ref())?;
    let collaboration_token =
        configure_local_collaboration_access(&db, &config, agent, &mut spec, &docker).await?;
    let container_workspace = spec
        .working_dir
        .clone()
        .unwrap_or_else(|| repository.container_root.clone());
    let visualization_roots = visualization_source_roots(
        &repository.bootstrap_root,
        &repository.container_bootstrap,
        agent,
    );
    let built_in_image = default_native_runner_image(&kind, agent.runner.container_engine)
        == Some(spec.image.as_str());
    if !runner_image_ready(&docker, &spec.image, built_in_image, agent).await {
        let local_fallback = match local_runner_image_alias(&spec.image) {
            Some(image) if runner_image_ready(&docker, image, built_in_image, agent).await => {
                Some(image)
            }
            _ => None,
        };
        if let Some(local_image) = local_fallback {
            spec.image = local_image.to_string();
        } else {
            docker.pull_image(&spec.image).await?;
        }
    }

    let mut mcp_servers = configured_mcp_servers(&config, agent)?;
    let bundled_control_tools = docker
        .image_has_label(
            &spec.image,
            "io.xpressclaw.protocol",
            BUILT_IN_RUNNER_PROTOCOL,
        )
        .await;
    let github_mcp_attached = configure_bundled_github_mcp(
        &agent.runner,
        &kind,
        bundled_control_tools,
        &mut spec.environment,
    )?;
    let presentation_runtime = presentation_runtime_available(&docker, &spec.image).await;
    let presentation_support =
        configure_codex_presentations(&kind, presentation_runtime, &mut spec.environment)?;
    if bundled_control_tools
        && !agent
            .runner
            .mcp_servers
            .iter()
            .any(|name| name == "xpressclaw")
    {
        mcp_servers.push(xpressclaw_control_mcp_server_for_context(
            &agent.name,
            None,
            Some(&turn.conversation_id),
            conversation.project_id.as_deref(),
            &repository.container_bootstrap,
            &repository.container_root,
            RunnerCallback {
                port: control_plane_port,
                token: control_plane_token.as_ref(),
                container_runtime: docker.runtime(),
                collaboration_token: collaboration_token.as_deref(),
            },
        ));
    }
    if github_mcp_attached {
        mcp_servers.push(github::mcp_server(&github::GithubMcpContext {
            control_plane_url: control_plane_url(control_plane_port, docker.runtime()),
            control_plane_token: agent_callback_capability(
                control_plane_token.as_ref(),
                &agent.name,
            ),
            agent_id: agent.name.clone(),
            workspace: repository.container_bootstrap.clone(),
            active_repository: repository.active.then(|| repository.container_root.clone()),
            task_id: None,
            review_lifecycle: false,
        }));
    }
    let pi_mcp_bridge = kind == "pi"
        && docker
            .image_has_label(&spec.image, "io.xpressclaw.pi-mcp", PI_MCP_BRIDGE_LABEL)
            .await;
    let (mcp_signature, pi_process_environment) = if pi_mcp_bridge {
        let bridge = configure_pi_mcp_bridge(
            &config.system.data_dir,
            &agent.name,
            Some(&turn.conversation_id),
            &mcp_servers,
            &mut spec,
        )?;
        mcp_servers.clear();
        (Some(bridge.signature), bridge.process_environment)
    } else {
        (None, Vec::new())
    };

    if !queue.is_running(&turn.id)? {
        return Ok(());
    }

    let base = project_processes
        .get_or_start(&docker, &agent.name, &spec)
        .await?;
    let live = conversation_processes
        .get_or_start(
            &docker,
            &turn.conversation_id,
            &turn.agent_id,
            &base,
            &spec,
            &pi_process_environment,
        )
        .await?;
    let session = queue.session(&turn.conversation_id, &turn.agent_id)?;
    let session_start = session
        .native_session_id
        .map(AcpSessionStart::Resume)
        .unwrap_or(AcpSessionStart::New);
    let previous_trigger_message_id = if matches!(&session_start, AcpSessionStart::Resume(_)) {
        queue.last_completed_trigger(&turn.conversation_id, &turn.agent_id)?
    } else {
        None
    };
    let prompt = build_conversation_prompt(
        &manager,
        &conversation,
        &turn,
        agent,
        previous_trigger_message_id,
    )?;
    turn_controls.begin_attempt(&turn.id);
    if !queue.is_running(&turn.id)? {
        turn_controls.finish_attempt(&turn.id);
        conversation_processes
            .retire_agent(&turn.conversation_id, &turn.agent_id)
            .await;
        return Ok(());
    }
    if !queue.start_response(&turn.id)? {
        turn_controls.finish_attempt(&turn.id);
        conversation_processes
            .retire_agent(&turn.conversation_id, &turn.agent_id)
            .await;
        return Ok(());
    }
    event_bus.send(
        &turn.conversation_id,
        ConversationEvent::Thinking {
            agent_id: turn.agent_id.clone(),
        },
    );
    let recorder = AcpEventRecorder::for_conversation(
        db.clone(),
        turn.conversation_id.clone(),
        turn.agent_id.clone(),
        turn.id.clone(),
        kind.clone(),
    );
    if let Err(error) = DashboardManager::new(db.clone()).capture_git_baseline(
        "conversation_turn",
        &turn.id,
        conversation.project_id.as_deref(),
        &turn.agent_id,
        &repository.active_root,
    ) {
        warn!(%error, turn_id = turn.id, "failed to capture conversation Git baseline");
    }
    let result = live
        .process
        .run_turn(
            AcpTurnRuntime::for_conversation(recorder, elicitation_broker, turn_controls.clone()),
            session_start,
            Path::new(&container_workspace),
            &prompt,
            AcpTurnOptions {
                model: agent.runner.model.clone(),
                session_config: agent.runner.session_config.clone(),
                mcp_servers,
                mcp_signature,
                additional_directories: presentation_support.additional_directories,
                image_attachments: vec![],
            },
        )
        .await;
    if let Err(error) =
        DashboardManager::new(db.clone()).record_git_snapshot("conversation_turn", &turn.id, true)
    {
        warn!(%error, turn_id = turn.id, "failed to finalize conversation Git metrics");
    }
    turn_controls.finish_attempt(&turn.id);
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            conversation_processes
                .invalidate(&turn.conversation_id, &turn.agent_id, &live.process)
                .await;
            if !queue.is_running(&turn.id)? {
                return Ok(());
            }
            return Err(error);
        }
    };
    let visualizations = prepare_message_visualizations(&result.summary, &visualization_roots);
    let published = prepare_message_artifacts(&result.summary, &visualization_roots);
    let Some(message) = queue.complete_with_message_and_visualizations(
        &turn,
        &result.session_id,
        &SendMessage {
            sender_type: "agent".into(),
            sender_id: turn.agent_id.clone(),
            sender_name: Some(agent.context_label()),
            content: published.content,
            message_type: None,
        },
        &json!({ "conversation_turn_id": turn.id, "runner": kind }),
        &visualizations,
        &published.attachments,
    )?
    else {
        event_bus.send(&turn.conversation_id, ConversationEvent::Done);
        return Ok(());
    };
    let mut message_value = json!(message);
    message_value["attachments"] = json!(manager.attachments(message.id).unwrap_or_default());
    message_value["visualizations"] = json!(manager.visualizations(message.id).unwrap_or_default());
    event_bus.send(
        &turn.conversation_id,
        ConversationEvent::Message {
            message: message_value,
        },
    );
    event_bus.send(&turn.conversation_id, ConversationEvent::Done);
    Ok(())
}

fn build_conversation_prompt(
    manager: &ConversationManager,
    conversation: &crate::conversations::Conversation,
    turn: &ConversationTurn,
    agent: &AgentConfig,
    previous_trigger_message_id: Option<i64>,
) -> Result<String> {
    let messages = match (previous_trigger_message_id, turn.trigger_message_id) {
        (Some(after_id), Some(through_id)) => {
            manager.get_messages_between(&turn.conversation_id, after_id, through_id, 80)?
        }
        (None, Some(through_id)) => {
            manager.get_messages_between(&turn.conversation_id, 0, through_id, 80)?
        }
        (_, None) => manager.get_messages(&turn.conversation_id, 80, None)?,
    }
    .into_iter()
    .filter(|message| message.sender_type != "agent" || message.sender_id != turn.agent_id)
    .collect::<Vec<_>>();
    let mut history = String::new();
    for message in messages {
        let name = message.sender_name.as_deref().unwrap_or(&message.sender_id);
        history.push_str(&format!("[{name}]: {}\n", message.content));
        for attachment in manager.attachments(message.id).unwrap_or_default() {
            history.push_str(&format!(
                "  [file: {} | attachment_id: {} | {} bytes]\n",
                attachment.name, attachment.id, attachment.size
            ));
        }
    }
    let history_label = if previous_trigger_message_id.is_some() {
        "New conversation activity since your previous response"
    } else {
        "Recent conversation history"
    };
    Ok(format!(
        "You are {} participating in the project conversation {:?}. Reply conversationally and concisely to the newest messages addressed to you. Your normal final response is automatically delivered to this project conversation; use it for your one final reply. Reserve send_conversation_message for genuine interim updates or publishing workspace files while you continue working. Never use the tool to duplicate your final response. You are in an independent chat lane, so do not duplicate a long-running task. If substantial work is needed, use create_conversation_task to create it and tell the participants. Use download_conversation_attachment with an attachment_id when you need to inspect a published file. Other Agents may be working in parallel.\n\nConversation ID: {}\nProject ID: {}\n\n{history_label}:\n{}",
        agent.context_label(),
        conversation.title.as_deref().unwrap_or("Untitled conversation"),
        turn.conversation_id,
        conversation.project_id.as_deref().unwrap_or("unassigned"),
        history,
    ))
}

#[derive(Debug, Clone)]
struct RuntimeRepository {
    bootstrap_root: PathBuf,
    active_root: PathBuf,
    active: bool,
    git_dir: Option<PathBuf>,
    git_common_dir: Option<PathBuf>,
    container_bootstrap: String,
    container_root: String,
}

impl RuntimeRepository {
    fn from_inspection(inspection: RepositoryInspection, agent: &AgentConfig) -> Self {
        let bootstrap_root = inspection.bootstrap_root.clone();
        let active_root = inspection.active_root().to_path_buf();
        let git_dir = inspection
            .active
            .as_ref()
            .map(|candidate| candidate.git_dir().to_path_buf());
        let git_common_dir = inspection
            .active
            .as_ref()
            .map(|candidate| candidate.git_common_dir().to_path_buf());
        let container_bootstrap =
            container_workspace_path(&bootstrap_root, agent.runner.container_engine);
        let container_root =
            if agent.runner.container_engine == ContainerEngineAccess::Host && cfg!(unix) {
                active_root.display().to_string()
            } else {
                inspection
                    .active_relative_path()
                    .filter(|relative| *relative != ".")
                    .map(|relative| format!("{container_bootstrap}/{relative}"))
                    .unwrap_or_else(|| container_bootstrap.clone())
            };
        Self {
            bootstrap_root,
            active_root,
            active: inspection.active.is_some(),
            git_dir,
            git_common_dir,
            container_bootstrap,
            container_root,
        }
    }
}

async fn prepare_repository_for_turn(
    db: &Arc<Database>,
    config: &Config,
    agent: &AgentConfig,
    docker: &DockerManager,
    project_processes: &ProjectAcpProcesses,
    conversation_processes: &ConversationAcpProcesses,
    runtime_lifecycle: &NativeRuntimeLifecycle,
) -> Result<(OwnedRwLockReadGuard<()>, RuntimeRepository)> {
    let bootstrap_root = resolved_workspace(config, agent);
    let lifecycle = runtime_lifecycle.slot(&agent.name);
    // Stable repositories retain the ordinary shared runtime path, so Task
    // and Conversation lanes for one Agent can continue concurrently.
    let read_guard = lifecycle.clone().read_owned().await;
    let inspection = inspect_repository_for_turn(db, &agent.name, &bootstrap_root).await?;
    if !inspection.requires_boundary_change() {
        return Ok((
            read_guard,
            RuntimeRepository::from_inspection(inspection, agent),
        ));
    }

    // Repository reconciliation and destructive Project cleanup share the
    // same per-Agent write boundary. Recheck after escalation because another
    // turn or Project deletion may have completed while the read guard was
    // released. Downgrading atomically then prevents deletion from entering
    // between repository cleanup and the new turn.
    drop(read_guard);
    let write_guard = lifecycle.write_owned().await;
    AgentRegistry::new(db.clone()).get(&agent.name)?;
    let inspection = inspect_repository_for_turn(db, &agent.name, &bootstrap_root).await?;
    let inspection = if inspection.requires_boundary_change() {
        let boundary = apply_repository_boundary_for_turn(db, &agent.name, &bootstrap_root).await?;
        if boundary.changed {
            conversation_processes
                .retire_agent_everywhere(&agent.name)
                .await;
            project_processes.retire_agent(&agent.name).await;
            let _ = docker.stop_preserving(&agent.name).await;
        }
        boundary.inspection
    } else {
        inspection
    };
    let guard = OwnedRwLockWriteGuard::downgrade(write_guard);
    Ok((guard, RuntimeRepository::from_inspection(inspection, agent)))
}

async fn inspect_repository_for_turn(
    db: &Arc<Database>,
    agent_id: &str,
    bootstrap_root: &Path,
) -> Result<RepositoryInspection> {
    let db = db.clone();
    let agent_id = agent_id.to_string();
    let bootstrap_root = bootstrap_root.to_path_buf();
    run_repository_blocking(move || {
        AgentRepositoryManager::new(db).inspect(&agent_id, &bootstrap_root)
    })
    .await
}

async fn apply_repository_boundary_for_turn(
    db: &Arc<Database>,
    agent_id: &str,
    bootstrap_root: &Path,
) -> Result<RepositoryBoundaryResult> {
    let db = db.clone();
    let agent_id = agent_id.to_string();
    let bootstrap_root = bootstrap_root.to_path_buf();
    run_repository_blocking(move || {
        AgentRepositoryManager::new(db).apply_boundary(&agent_id, &bootstrap_root)
    })
    .await
}

async fn execute_item(runtime: NativeAttemptRuntime, item: QueueItem) -> Result<()> {
    let NativeAttemptRuntime {
        db,
        config,
        docker,
        event_bus,
        elicitation_broker,
        turn_controls,
        processes,
        conversation_processes,
        runtime_lifecycle,
        control_plane_port,
        control_plane_token,
    } = runtime;
    let attempt_id = item
        .attempt_id
        .as_deref()
        .ok_or_else(|| Error::Task(format!("queue item {} has no work attempt", item.id)))?;
    let agent = config
        .agents
        .iter()
        .find(|agent| agent.name == item.agent_id)
        .ok_or_else(|| Error::AgentNotFound {
            name: item.agent_id.clone(),
        })?;
    let kind = resolve_runner_kind(agent)?;
    let (_runtime_lifecycle_guard, repository) = prepare_repository_for_turn(
        &db,
        &config,
        agent,
        &docker,
        &processes,
        &conversation_processes,
        &runtime_lifecycle,
    )
    .await?;
    let session_start = session_start(&db, &item, &kind)?;
    let requested_session_config = requested_session_config(&db, agent, &item.task_id)?;
    let mut prompt = build_prompt(&db, &item, attempt_id)?;
    if session_start == AcpSessionStart::New {
        prepend_unresumed_interrupted_prompt(&db, &item, attempt_id, &mut prompt)?;
    }
    append_plan_lifecycle_guidance(&mut prompt.content);
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE work_attempts SET runner = ?1, prompt = ?2 WHERE id = ?3",
            rusqlite::params![kind, prompt.content, attempt_id],
        )
    })?;

    let sessions = SessionManager::new(db.clone());
    let preparing = sessions.transition_attempt(
        attempt_id,
        "preparing",
        &format!("Preparing {kind}"),
        None,
        None,
    )?;
    if attempt_is_terminal(&preparing.status) {
        return Ok(());
    }
    if let Some(conversation_id) = conversation_id(&db, &item.task_id) {
        event_bus.send(
            &conversation_id,
            ConversationEvent::Thinking {
                agent_id: item.agent_id.clone(),
            },
        );
    }
    let board = TaskBoard::new(db.clone());
    let task = board.get(&item.task_id)?;
    let task_project_id = board.project_id(&task.id)?;
    let capture_task_dashboard_metrics = dashboard_task_metrics_enabled(&task);
    let control_task_id = continuation_task_id(&task).map(str::to_owned);
    let github_review_lifecycle =
        control_task_id.is_some() && github_review_lifecycle_enabled(&task);
    let _ = board.update_status(&item.task_id, "in_progress", Some(&item.agent_id));

    if let AcpSessionStart::Resume(native_session_id) = &session_start {
        sessions.set_native_session(attempt_id, native_session_id)?;
    }
    let github = if repository.active {
        discover_github_access(&db, &repository.active_root).await?
    } else {
        None
    };
    let mut spec = build_spec(&config, agent, &kind, &docker, &repository, github.as_ref())?;
    let collaboration_token =
        configure_local_collaboration_access(&db, &config, agent, &mut spec, &docker).await?;
    let container_workspace = spec
        .working_dir
        .clone()
        .unwrap_or_else(|| repository.container_root.clone());
    let visualization_roots = visualization_source_roots(
        &repository.bootstrap_root,
        &repository.container_bootstrap,
        agent,
    );
    let built_in_image = default_native_runner_image(&kind, agent.runner.container_engine)
        == Some(spec.image.as_str());
    let image_ready = runner_image_ready(&docker, &spec.image, built_in_image, agent).await;
    if !image_ready {
        let local_fallback = match local_runner_image_alias(&spec.image) {
            Some(image) if runner_image_ready(&docker, image, built_in_image, agent).await => {
                Some(image)
            }
            _ => None,
        };
        if let Some(local_image) = local_fallback {
            spec.image = local_image.to_string();
        } else {
            let pulling = sessions.transition_attempt(
                attempt_id,
                "preparing",
                &format!("Pulling {kind} runner image"),
                None,
                None,
            )?;
            if attempt_is_terminal(&pulling.status) {
                return Ok(());
            }
            docker.pull_image(&spec.image).await?;
            if !runner_image_ready(&docker, &spec.image, built_in_image, agent).await {
                return Err(Error::Backend(format!(
                    "runner image {} is incompatible with the configured ACP or container-engine mode; rebuild it from the current Dockerfile",
                    spec.image
                )));
            }
        }
    }
    if attempt_is_terminal(&sessions.get_attempt(attempt_id)?.status) {
        return Ok(());
    }
    let mut mcp_servers = configured_mcp_servers(&config, agent)?;
    let bundled_control_tools = docker
        .image_has_label(
            &spec.image,
            "io.xpressclaw.protocol",
            BUILT_IN_RUNNER_PROTOCOL,
        )
        .await;
    let github_mcp_attached = configure_bundled_github_mcp(
        &agent.runner,
        &kind,
        bundled_control_tools,
        &mut spec.environment,
    )?;
    let presentation_runtime = presentation_runtime_available(&docker, &spec.image).await;
    let presentation_support =
        configure_codex_presentations(&kind, presentation_runtime, &mut spec.environment)?;
    if bundled_control_tools
        && !agent
            .runner
            .mcp_servers
            .iter()
            .any(|name| name == "xpressclaw")
    {
        mcp_servers.push(xpressclaw_control_mcp_server_for_context(
            &agent.name,
            control_task_id.as_deref(),
            task.conversation_id.as_deref(),
            task_project_id.as_deref(),
            &repository.container_bootstrap,
            &repository.container_root,
            RunnerCallback {
                port: control_plane_port,
                token: control_plane_token.as_ref(),
                container_runtime: docker.runtime(),
                collaboration_token: collaboration_token.as_deref(),
            },
        ));
    }
    if github_mcp_attached {
        mcp_servers.push(github::mcp_server(&github::GithubMcpContext {
            control_plane_url: control_plane_url(control_plane_port, docker.runtime()),
            control_plane_token: agent_callback_capability(
                control_plane_token.as_ref(),
                &agent.name,
            ),
            agent_id: agent.name.clone(),
            workspace: repository.container_bootstrap.clone(),
            active_repository: repository.active.then(|| repository.container_root.clone()),
            task_id: control_task_id.clone(),
            review_lifecycle: github_review_lifecycle,
        }));
    } else if github.is_some() && !bundled_control_tools {
        warn!(
            image = spec.image,
            repository = github.as_ref().map(|access| access.repository()),
            "runner image does not include the constrained GitHub MCP server"
        );
    }
    let pi_mcp_bridge = kind == "pi"
        && docker
            .image_has_label(&spec.image, "io.xpressclaw.pi-mcp", PI_MCP_BRIDGE_LABEL)
            .await;
    let mcp_signature = if pi_mcp_bridge {
        let bridge = configure_pi_mcp_bridge(
            &config.system.data_dir,
            &agent.name,
            None,
            &mcp_servers,
            &mut spec,
        )?;
        mcp_servers.clear();
        Some(bridge.signature)
    } else {
        None
    };
    if attempt_is_terminal(&sessions.get_attempt(attempt_id)?.status) {
        return Ok(());
    }
    let workload_id = agent.name.as_str();
    let live = processes.get_or_start(&docker, workload_id, &spec).await?;
    if let Err(error) = sessions.set_container(attempt_id, &live.container_id) {
        processes.invalidate(workload_id, &live.process).await;
        let _ = docker.stop_preserving(workload_id).await;
        return Err(error);
    }
    let running = match sessions.transition_attempt(
        attempt_id,
        "running",
        &format!("{kind} is working over ACP"),
        None,
        None,
    ) {
        Ok(running) => running,
        Err(error) => {
            processes.invalidate(workload_id, &live.process).await;
            if docker.stop_preserving(workload_id).await.is_ok() {
                let _ = sessions.clear_container(attempt_id);
            }
            return Err(error);
        }
    };
    if attempt_is_terminal(&running.status) {
        processes.invalidate(workload_id, &live.process).await;
        if docker.stop_preserving(workload_id).await.is_ok() {
            let _ = sessions.clear_container(attempt_id);
        }
        return Ok(());
    }

    let attempt = sessions.get_attempt(attempt_id)?;
    let recorder = AcpEventRecorder::new(
        db.clone(),
        attempt.session_id.clone(),
        attempt_id,
        item.task_id.clone(),
        kind.clone(),
    );
    if capture_task_dashboard_metrics {
        if let Err(error) = DashboardManager::new(db.clone()).capture_git_baseline(
            "attempt",
            attempt_id,
            task_project_id.as_deref(),
            &item.agent_id,
            &repository.active_root,
        ) {
            warn!(%error, attempt_id, "failed to capture task Git baseline");
        }
    }
    let turn = live
        .process
        .run_turn(
            AcpTurnRuntime::new(recorder, elicitation_broker, turn_controls),
            session_start,
            Path::new(&container_workspace),
            &prompt.content,
            AcpTurnOptions {
                model: agent.runner.model.clone(),
                session_config: requested_session_config,
                mcp_servers,
                mcp_signature,
                additional_directories: presentation_support.additional_directories,
                image_attachments: prompt.attachments,
            },
        )
        .await;
    if capture_task_dashboard_metrics {
        if let Err(error) =
            DashboardManager::new(db.clone()).record_git_snapshot("attempt", attempt_id, true)
        {
            warn!(%error, attempt_id, "failed to finalize task Git metrics");
        }
    }
    if turn.is_err() && processes.invalidate(workload_id, &live.process).await {
        docker.stop_preserving(workload_id).await?;
    }
    sessions.clear_container(attempt_id)?;
    let mut turn = turn?;
    let current = sessions.get_attempt(attempt_id)?;
    if matches!(current.status.as_str(), "cancelled" | "interrupted") {
        return Ok(());
    }
    if turn.interrupted {
        sessions.add_artifact(
            attempt_id,
            "runner_output",
            "Interrupted ACP event transcript",
            Some(&turn.diagnostic),
            None,
            json!({ "protocol": "acp", "stop_reason": turn.stop_reason, "runner": kind }),
        )?;
        sessions.set_native_session(attempt_id, &turn.session_id)?;
        let interrupted = sessions.transition_attempt(
            attempt_id,
            "interrupted",
            "Agent interrupted to apply new guidance",
            None,
            None,
        )?;
        if interrupted.status != "interrupted" {
            return Ok(());
        }
        let queue = TaskQueue::new(db.clone());
        queue.complete(item.id, "interrupted to apply new guidance")?;
        let next_status = if queue.has_queued_for_task(&item.task_id)? {
            "in_progress"
        } else {
            "pending"
        };
        board.update_status(&item.task_id, next_status, Some(&item.agent_id))?;
        sessions.refresh_status(&item.agent_id)?;
        return Ok(());
    }
    let visualizations = prepare_message_visualizations(&turn.summary, &visualization_roots);
    let published = prepare_message_artifacts(&turn.summary, &visualization_roots);
    turn.summary = published.content;
    sessions.add_artifact(
        attempt_id,
        "runner_output",
        "ACP event transcript",
        Some(&turn.diagnostic),
        None,
        json!({ "protocol": "acp", "stop_reason": turn.stop_reason, "runner": kind }),
    )?;
    sessions.set_native_session(attempt_id, &turn.session_id)?;
    sessions.add_artifact(
        attempt_id,
        "result",
        "Attempt result",
        Some(&turn.summary),
        None,
        json!({ "protocol": "acp", "stop_reason": turn.stop_reason, "runner": kind }),
    )?;
    let completion_summary = truncate(&turn.summary, 2_000);
    let queue = TaskQueue::new(db.clone());
    let Some(_) = TaskConversation::new(db.clone()).complete_final_assistant_attempt(
        FinalAssistantAttempt {
            task_id: &item.task_id,
            queue_id: item.id,
            attempt_id,
            completion_summary: &completion_summary,
            content: &turn.summary,
            visualizations: &visualizations,
            published_files: &published.attachments,
        },
    )?
    else {
        return Ok(());
    };

    let continuation_queued = queue.has_queued_for_task(&item.task_id)?;
    let waiting_for_user = needs_user_input(&turn.summary);
    if !continuation_queued && !waiting_for_user {
        board.defer_reported_subtasks(&item.task_id, "successful_attempt_completed")?;
    }
    let review_gate =
        crate::workers::github_review::GithubReviewManager::new(db.clone()).gate(&item.task_id)?;
    let completed_tasks = if continuation_queued {
        board.update_status(&item.task_id, "in_progress", Some(&item.agent_id))?;
        Vec::new()
    } else if waiting_for_user {
        board.update_status(&item.task_id, "waiting_for_input", Some(&item.agent_id))?;
        Vec::new()
    } else if review_gate == crate::workers::github_review::GithubReviewGate::Waiting {
        board.update_status(&item.task_id, "in_progress", Some(&item.agent_id))?;
        Vec::new()
    } else if review_gate == crate::workers::github_review::GithubReviewGate::NeedsInput {
        board.update_status(&item.task_id, "waiting_for_input", Some(&item.agent_id))?;
        Vec::new()
    } else if board.subtasks_complete(&item.task_id)? {
        board.complete_and_roll_up(&item.task_id, Some(&item.agent_id))?
    } else {
        board.update_status(&item.task_id, "in_progress", Some(&item.agent_id))?;
        Vec::new()
    };
    sessions.refresh_status(&item.agent_id)?;
    publish_conversation_result(
        &db,
        &event_bus,
        &item,
        &agent.context_label(),
        &turn.summary,
        attempt_id,
        ConversationResultArtifacts {
            visualizations: &visualizations,
            published_files: &published.attachments,
        },
    );
    for completed in completed_tasks {
        advance_workflow(&db, &completed.id, "completed", &turn.summary);
    }
    Ok(())
}

fn attempt_is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

fn fail_item(
    db: &Arc<Database>,
    item: &QueueItem,
    message: &str,
    event_bus: &Arc<ConversationEventBus>,
) -> Result<()> {
    let sessions = SessionManager::new(db.clone());
    if let Some(attempt_id) = item.attempt_id.as_deref() {
        if let Ok(attempt) = sessions.get_attempt(attempt_id) {
            // Cancellation updates the durable queue/task state before it
            // stops the container. The waiter may then observe a Docker error;
            // do not overwrite the user's cancellation with a failure.
            if matches!(attempt.status.as_str(), "cancelled" | "interrupted") {
                return Ok(());
            }
            let _ = sessions.transition_attempt(
                attempt_id,
                "failed",
                "Work attempt failed",
                None,
                Some(message),
            );
        }
    }
    let queue = TaskQueue::new(db.clone());
    queue.fail(item.id, message)?;
    let chat_message = format!(
        "The worker could not complete this turn.\n\n{}",
        truncate(message, 2_000)
    );
    if let Err(error) =
        TaskConversation::new(db.clone()).add_message(&item.task_id, "assistant", &chat_message)
    {
        warn!(%error, task_id = item.task_id, "failed to persist native task failure");
    }
    let board = TaskBoard::new(db.clone());
    let continuation_queued = queue.has_queued_for_task(&item.task_id).unwrap_or(false);
    let _ = board.update_status(
        &item.task_id,
        if continuation_queued {
            "in_progress"
        } else {
            "blocked"
        },
        Some(&item.agent_id),
    );
    let _ = sessions.refresh_status(&item.agent_id);
    if let Some(conversation_id) = conversation_id(db, &item.task_id) {
        event_bus.send(
            &conversation_id,
            ConversationEvent::Error {
                agent_id: Some(item.agent_id.clone()),
                error: message.to_string(),
            },
        );
        event_bus.send(&conversation_id, ConversationEvent::Done);
    }
    if !continuation_queued {
        advance_workflow(db, &item.task_id, "failed", message);
    }
    Ok(())
}

fn conversation_id(db: &Arc<Database>, task_id: &str) -> Option<String> {
    TaskBoard::new(db.clone())
        .get(task_id)
        .ok()
        .and_then(|task| task.conversation_id)
}

struct ConversationResultArtifacts<'a> {
    visualizations: &'a [PreparedVisualization],
    published_files: &'a [crate::message_artifacts::PublishedFileAttachment],
}

fn publish_conversation_result(
    db: &Arc<Database>,
    event_bus: &Arc<ConversationEventBus>,
    item: &QueueItem,
    sender_name: &str,
    content: &str,
    attempt_id: &str,
    artifacts: ConversationResultArtifacts<'_>,
) {
    let Some(conversation_id) = conversation_id(db, &item.task_id) else {
        return;
    };
    let manager = ConversationManager::new(db.clone());
    let attachments = artifacts
        .published_files
        .iter()
        .map(|attachment| NewConversationAttachment {
            name: bound_published_file_name(&attachment.name),
            mime_type: attachment.mime_type.clone(),
            data: attachment.data.clone(),
            source_task_id: Some(item.task_id.clone()),
        })
        .collect::<Vec<_>>();
    if let Ok((message, _, _)) = manager.send_agent_routed_message_with_visualizations(
        &conversation_id,
        &SendMessage {
            sender_type: "agent".to_string(),
            sender_id: item.agent_id.clone(),
            sender_name: Some(sender_name.to_string()),
            content: content.to_string(),
            message_type: Some("task_result".to_string()),
        },
        Some(&item.task_id),
        &attachments,
        Some(attempt_id),
        artifacts.visualizations,
    ) {
        let mut message_value = json!(message);
        message_value["attachments"] = json!(manager.attachments(message.id).unwrap_or_default());
        message_value["visualizations"] =
            json!(manager.visualizations(message.id).unwrap_or_default());
        event_bus.send(
            &conversation_id,
            ConversationEvent::Message {
                message: message_value,
            },
        );
    }
    let _ = manager.mark_processed(&conversation_id);
    event_bus.send(&conversation_id, ConversationEvent::Done);
}

fn advance_workflow(db: &Arc<Database>, task_id: &str, status: &str, output: &str) {
    let engine = crate::workflows::engine::WorkflowEngine::new(db.clone());
    if engine
        .find_execution_by_task(task_id)
        .is_ok_and(|execution| execution.is_some())
    {
        if let Err(error) = engine.on_task_completed(task_id, status, output) {
            warn!(task_id, status, error = %error, "failed to advance workflow");
        }
    }
}

pub fn resolve_runner_kind(agent: &AgentConfig) -> Result<String> {
    if agent.runner.kind != "auto" {
        let configured = agent.runner.kind.to_lowercase();
        return Ok(canonical_agent_kind(&configured)
            .unwrap_or(configured.as_str())
            .to_string());
    }
    let backend = agent.backend.to_lowercase();
    if let Some(kind) = infer_agent_kind_from_backend(&backend) {
        Ok(kind.to_string())
    } else if !agent.runner.command.is_empty() {
        Ok("custom".to_string())
    } else {
        Err(Error::Backend(format!(
            "session '{}' needs runner.kind or runner.command (backend was '{}')",
            agent.name, agent.backend
        )))
    }
}

struct AgentPrompt {
    content: String,
    attachments: Vec<PromptImageAttachment>,
}

const PLAN_LIFECYCLE_GUIDANCE: &str = "<xpressclaw-plan-lifecycle>\n\
ACP plan items are current-turn checklists, not durable future work. Before finishing, mark completed current-turn items complete and do not leave speculative review, approval, merge, or other future work in progress. Use create_task with this task as parent when work must be explicitly delegated and block completion.\n\
</xpressclaw-plan-lifecycle>";

fn append_plan_lifecycle_guidance(prompt: &mut String) {
    if prompt.contains(PLAN_LIFECYCLE_GUIDANCE) {
        return;
    }
    if !prompt.trim().is_empty() {
        prompt.push_str("\n\n");
    }
    prompt.push_str(PLAN_LIFECYCLE_GUIDANCE);
}

fn prepend_unresumed_interrupted_prompt(
    db: &Arc<Database>,
    item: &QueueItem,
    attempt_id: &str,
    prompt: &mut AgentPrompt,
) -> Result<()> {
    let previous_prompt: Option<String> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT prompt FROM work_attempts
             WHERE task_id = ?1 AND id != ?2 AND status = 'interrupted'
               AND native_session_id IS NULL AND prompt != ''
             ORDER BY COALESCE(completed_at, created_at) DESC LIMIT 1",
            rusqlite::params![item.task_id, attempt_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    let Some(previous_prompt) = previous_prompt else {
        return Ok(());
    };
    if prompt.content.trim().is_empty() {
        prompt.content = previous_prompt;
    } else if prompt.content != previous_prompt
        && !prompt
            .content
            .starts_with(&format!("{previous_prompt}\n\n"))
    {
        prompt.content = format!(
            "{previous_prompt}\n\nAdditional guidance from the user:\n{}",
            prompt.content
        );
    }
    Ok(())
}

fn build_prompt(db: &Arc<Database>, item: &QueueItem, attempt_id: &str) -> Result<AgentPrompt> {
    let task = TaskBoard::new(db.clone()).get(&item.task_id)?;
    let previous_started: Option<String> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT started_at FROM work_attempts
                 WHERE task_id = ?1 AND id != ?2 AND started_at IS NOT NULL
                   AND NOT (status = 'interrupted' AND native_session_id IS NULL)
                 ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![item.task_id, attempt_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    let pending_user_messages = TaskConversation::new(db.clone())
        .get_user_messages_since(&item.task_id, previous_started.as_deref())?;
    if !pending_user_messages.is_empty() {
        let content = pending_user_messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let attachments = pending_user_messages
            .into_iter()
            .flat_map(|message| message.attachments)
            .collect();
        return Ok(AgentPrompt {
            content,
            attachments,
        });
    }

    let description = task
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty());
    let from_project_composer = task
        .context
        .as_ref()
        .and_then(|context| context.get("origin"))
        .and_then(Value::as_str)
        == Some("session_message");
    let content = match (description, from_project_composer) {
        (Some(description), true) => description.to_string(),
        (Some(description), false) => format!("{}\n\n{}", task.title, description),
        (None, _) => task.title,
    };
    Ok(AgentPrompt {
        content,
        attachments: Vec::new(),
    })
}

/// Pick how this task enters its ACP conversation.
///
/// Follow-ups resume the task's branch while it is still active. Reopening an
/// older task forks its saved branch, and first turns fork either their
/// dependency or the project's active branch. Agents without `session/fork`
/// support fall back to resuming the selected source inside `run_turn`.
fn session_start(db: &Arc<Database>, item: &QueueItem, runner: &str) -> Result<AcpSessionStart> {
    let board = TaskBoard::new(db.clone());
    let task = board.get(&item.task_id)?;

    let task_session: Option<(String, String)> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, native_session_id FROM work_attempts
             WHERE task_id = ?1 AND session_id = ?2 AND runner = ?3
               AND native_session_id IS NOT NULL
               AND status IN ('completed', 'interrupted')
             ORDER BY COALESCE(completed_at, created_at) DESC, rowid DESC LIMIT 1",
            rusqlite::params![item.task_id, item.agent_id, runner],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Error::from)
    })?;

    let project_session: Option<(String, String)> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, native_session_id FROM work_attempts
             WHERE session_id = ?1 AND runner = ?2
               AND native_session_id IS NOT NULL
               AND status IN ('completed', 'interrupted')
             ORDER BY COALESCE(completed_at, created_at) DESC, rowid DESC LIMIT 1",
            rusqlite::params![item.agent_id, runner],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Error::from)
    })?;
    if let Some((task_attempt_id, task_session_id)) = task_session {
        return if project_session
            .as_ref()
            .is_some_and(|(project_attempt_id, _)| project_attempt_id != &task_attempt_id)
        {
            Ok(AcpSessionStart::Fork(task_session_id))
        } else {
            Ok(AcpSessionStart::Resume(task_session_id))
        };
    }

    let dependency_session: Option<String> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT a.native_session_id
             FROM task_dependencies d
             JOIN work_attempts a ON a.task_id = d.depends_on_id
             WHERE d.task_id = ?1 AND a.session_id = ?2 AND a.runner = ?3
               AND a.native_session_id IS NOT NULL
               AND a.status IN ('completed', 'interrupted')
             ORDER BY COALESCE(a.completed_at, a.created_at) DESC, a.rowid DESC LIMIT 1",
            rusqlite::params![item.task_id, item.agent_id, runner],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    if let Some(dependency_session) = dependency_session {
        return Ok(AcpSessionStart::Fork(dependency_session));
    }

    let start_new = task
        .context
        .as_ref()
        .and_then(|context| context.get("session_mode"))
        .and_then(Value::as_str)
        == Some("new");
    if start_new {
        return Ok(AcpSessionStart::New);
    }

    Ok(
        project_session.map_or(AcpSessionStart::New, |(_, session_id)| {
            AcpSessionStart::Fork(session_id)
        }),
    )
}

async fn runner_image_ready(
    docker: &DockerManager,
    image: &str,
    built_in_image: bool,
    agent: &AgentConfig,
) -> bool {
    runner_image_compatible(
        docker,
        image,
        built_in_image,
        built_in_image && agent.runner.container_engine == ContainerEngineAccess::Host,
        built_in_image && resolve_runner_kind(agent).is_ok_and(|kind| kind == "pi"),
    )
    .await
}

/// Apply the one compatibility contract used by readiness checks and the
/// dispatcher. Keeping this in the core worker module prevents the UI from
/// accepting a stale local image that the dispatcher will reject (or vice
/// versa), which otherwise turns Prepare runner into an unnecessary registry
/// pull.
pub async fn runner_image_compatible(
    docker: &DockerManager,
    image: &str,
    built_in_image: bool,
    host_engine_image: bool,
    pi_mcp_image: bool,
) -> bool {
    docker.has_image(image).await
        && (!built_in_image
            || docker
                .image_has_label(image, "io.xpressclaw.protocol", BUILT_IN_RUNNER_PROTOCOL)
                .await)
        && (!host_engine_image
            || docker
                .image_has_label(image, "io.xpressclaw.container-engine", "host")
                .await)
        && (!pi_mcp_image
            || docker
                .image_has_label(image, "io.xpressclaw.pi-mcp", PI_MCP_BRIDGE_LABEL)
                .await)
}

/// Exact image contract for the XpressClaw-owned Codex presentation workflow.
/// Custom images may opt in only by packaging the same paths and labels.
pub async fn presentation_runtime_available(docker: &DockerManager, image: &str) -> bool {
    docker
        .image_has_label(
            image,
            PRESENTATION_CAPABILITY_LABEL,
            PRESENTATION_CAPABILITY,
        )
        .await
        && docker
            .image_has_label(
                image,
                PRESENTATION_RUNTIME_VERSION_LABEL,
                PRESENTATION_RUNTIME_VERSION,
            )
            .await
}

fn configure_pi_mcp_bridge(
    data_dir: &Path,
    agent_id: &str,
    process_scope: Option<&str>,
    mcp_servers: &[McpServer],
    spec: &mut ContainerSpec,
) -> Result<PiMcpBridge> {
    // Pi reads MCP configuration from a file when its ACP process starts.
    // Serialize the small host-side publication step so a task and a newly
    // created conversation process cannot contend over temporary files.
    let _config_guard = PI_MCP_CONFIG_LOCK.lock().unwrap();
    if spec
        .volumes
        .iter()
        .any(|mount| container_paths_overlap(&mount.target, PI_MCP_CONFIG_DIR_TARGET))
    {
        return Err(Error::Backend(format!(
            "the Pi MCP bridge reserves container mount target {PI_MCP_CONFIG_DIR_TARGET}"
        )));
    }
    let encoded = pi_mcp_config(mcp_servers)?;
    let config_dir = pi_mcp_config_dir(data_dir, agent_id);
    std::fs::create_dir_all(&config_dir).map_err(|error| {
        Error::Backend(format!(
            "failed to create Pi MCP runtime directory {}: {error}",
            config_dir.display()
        ))
    })?;
    set_private_directory_permissions(&config_dir)?;

    let (config_path, process_environment) = if let Some(scope) = process_scope {
        let scope_hash = format!("{:x}", Sha256::digest(scope.as_bytes()));
        let process_dir = config_dir.join("processes").join(&scope_hash);
        std::fs::create_dir_all(&process_dir).map_err(|error| {
            Error::Backend(format!(
                "failed to create scoped Pi MCP runtime directory {}: {error}",
                process_dir.display()
            ))
        })?;
        set_private_directory_permissions(&config_dir.join("processes"))?;
        set_private_directory_permissions(&process_dir)?;

        // The retained container starts one primary ACP process as its init
        // command. Give that process a valid, context-free bootstrap file;
        // the independently exec'd conversation process receives its own
        // immutable path below. Never replace an active task's root config.
        let bootstrap_path = config_dir.join("config.json");
        if !bootstrap_path.exists() {
            write_private_atomic(&bootstrap_path, &pi_mcp_config(&[])?)?;
        }

        (
            process_dir.join("config.json"),
            vec![format!(
                "XPRESSCLAW_PI_MCP_CONFIG={PI_MCP_CONFIG_DIR_TARGET}/processes/{scope_hash}/config.json"
            )],
        )
    } else {
        (config_dir.join("config.json"), Vec::new())
    };
    write_private_atomic(&config_path, &encoded)?;

    spec.volumes.push(VolumeMount {
        source: config_dir.display().to_string(),
        target: PI_MCP_CONFIG_DIR_TARGET.to_string(),
        read_only: true,
        selinux_relabel: SelinuxRelabel::Shared,
    });
    spec.environment.retain(|variable| {
        !variable.starts_with("PI_ACP_PI_COMMAND=")
            && !variable.starts_with("XPRESSCLAW_PI_MCP_CONFIG=")
    });
    spec.environment
        .push(format!("PI_ACP_PI_COMMAND={PI_MCP_WRAPPER}"));
    spec.environment
        .push(format!("XPRESSCLAW_PI_MCP_CONFIG={PI_MCP_CONFIG_TARGET}"));

    Ok(PiMcpBridge {
        signature: format!("pi-mcp:{:x}", Sha256::digest(&encoded)),
        process_environment,
    })
}

fn container_paths_overlap(left: &str, right: &str) -> bool {
    let left = left.trim_end_matches('/');
    let right = right.trim_end_matches('/');
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn pi_mcp_config_dir(data_dir: &Path, agent_id: &str) -> PathBuf {
    let agent_hash = format!("{:x}", Sha256::digest(agent_id.as_bytes()));
    data_dir.join("runtime").join("pi-mcp").join(agent_hash)
}

/// Remove XpressClaw-generated per-Agent runtime files after an Agent is
/// permanently deleted. Repository and managed-workspace directories are
/// intentionally outside these hashed runtime roots and are never touched.
pub fn remove_agent_runtime_state(data_dir: &Path, agent_id: &str) -> Result<()> {
    let agent_hash = format!("{:x}", Sha256::digest(agent_id.as_bytes()));
    for runtime_kind in ["pi-mcp", "ssh-known-hosts", "ssh-config", "git-worktrees"] {
        let path = data_dir
            .join("runtime")
            .join(runtime_kind)
            .join(&agent_hash);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::Backend(format!(
                    "failed to remove Agent runtime directory {}: {error}",
                    path.display()
                )))
            }
        }
    }
    Ok(())
}

fn pi_mcp_config(mcp_servers: &[McpServer]) -> Result<Vec<u8>> {
    let mut servers = serde_json::Map::new();
    for server in mcp_servers {
        let (name, mut definition) = pi_mcp_server(server)?;
        if matches!(name.as_str(), "xpressclaw" | "github") {
            definition.insert("directTools".to_string(), Value::Bool(true));
        }
        if servers
            .insert(name.clone(), Value::Object(definition))
            .is_some()
        {
            return Err(Error::Backend(format!(
                "Pi MCP configuration contains duplicate server name '{name}'"
            )));
        }
    }
    serde_json::to_vec_pretty(&json!({ "mcpServers": servers })).map_err(Error::from)
}

fn pi_mcp_server(server: &McpServer) -> Result<(String, serde_json::Map<String, Value>)> {
    let mut definition = serde_json::Map::new();
    let name = match server {
        McpServer::Stdio(server) => {
            let command = server.command.to_str().ok_or_else(|| {
                Error::Backend(format!(
                    "Pi MCP server '{}' command is not valid UTF-8",
                    server.name
                ))
            })?;
            definition.insert("command".to_string(), json!(command));
            if !server.args.is_empty() {
                definition.insert("args".to_string(), json!(server.args));
            }
            if !server.env.is_empty() {
                let env = server
                    .env
                    .iter()
                    .map(|variable| (variable.name.clone(), json!(variable.value)))
                    .collect::<serde_json::Map<_, _>>();
                definition.insert("env".to_string(), Value::Object(env));
            }
            server.name.clone()
        }
        McpServer::Http(server) => {
            definition.insert("url".to_string(), json!(server.url));
            definition.insert("httpTransport".to_string(), json!("streamable-http"));
            add_pi_mcp_headers(&mut definition, &server.headers);
            server.name.clone()
        }
        McpServer::Sse(server) => {
            definition.insert("url".to_string(), json!(server.url));
            definition.insert("httpTransport".to_string(), json!("sse"));
            add_pi_mcp_headers(&mut definition, &server.headers);
            server.name.clone()
        }
        _ => {
            return Err(Error::Backend(
                "Pi MCP bridge received an unsupported ACP MCP transport".to_string(),
            ));
        }
    };
    Ok((name, definition))
}

fn add_pi_mcp_headers(definition: &mut serde_json::Map<String, Value>, headers: &[HttpHeader]) {
    if headers.is_empty() {
        return;
    }
    let headers = headers
        .iter()
        .map(|header| (header.name.clone(), json!(header.value)))
        .collect::<serde_json::Map<_, _>>();
    definition.insert("headers".to_string(), Value::Object(headers));
}

fn set_private_directory_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                Error::Backend(format!(
                    "failed to protect private runtime directory {}: {error}",
                    path.display()
                ))
            },
        )?;
    }
    #[cfg(windows)]
    set_windows_owner_only_acl(path, true)?;
    #[cfg(not(any(unix, windows)))]
    let _ = path;
    Ok(())
}

fn write_private_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    if std::fs::read(path).ok().as_deref() == Some(contents) {
        set_private_file_permissions(path)?;
        return Ok(());
    }
    let temporary = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|error| {
        Error::Backend(format!(
            "failed to create private runtime file {}: {error}",
            temporary.display()
        ))
    })?;
    set_private_file_permissions(&temporary)?;
    file.write_all(contents).map_err(|error| {
        Error::Backend(format!(
            "failed to write private runtime file {}: {error}",
            temporary.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        Error::Backend(format!(
            "failed to sync private runtime file {}: {error}",
            temporary.display()
        ))
    })?;
    drop(file);
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path).map_err(|error| {
            Error::Backend(format!(
                "failed to replace private runtime file {}: {error}",
                path.display()
            ))
        })?;
    }
    std::fs::rename(&temporary, path).map_err(|error| {
        Error::Backend(format!(
            "failed to publish private runtime file {}: {error}",
            path.display()
        ))
    })?;
    set_private_file_permissions(path)
}

fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                Error::Backend(format!(
                    "failed to protect private runtime file {}: {error}",
                    path.display()
                ))
            },
        )?;
    }
    #[cfg(windows)]
    set_windows_owner_only_acl(path, false)?;
    #[cfg(not(any(unix, windows)))]
    let _ = path;
    Ok(())
}

#[cfg(windows)]
struct WindowsLocalAllocation(*mut core::ffi::c_void);

#[cfg(windows)]
impl Drop for WindowsLocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: The pointer was returned by a Windows security API that
            // documents LocalFree as its matching deallocator.
            unsafe {
                let _ = windows_sys::Win32::Foundation::LocalFree(self.0);
            }
        }
    }
}

/// Apply a protected owner-only DACL to a private file or directory.
///
/// This is public so other XpressClaw crates can use the same audited Windows
/// secret-storage primitive instead of inheriting a permissive parent ACL.
#[cfg(windows)]
pub fn set_windows_owner_only_acl(path: &Path, directory: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{
        GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
        NO_MULTIPLE_TRUSTEE, SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
        TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    let mut wide_path = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide_path.contains(&0) {
        return Err(Error::Backend(format!(
            "failed to protect private path {}: path contains a NUL character",
            path.display()
        )));
    }
    wide_path.push(0);

    let mut owner: PSID = null_mut();
    let mut security_descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `wide_path` is NUL-terminated and all output pointers remain
    // valid for the duration of the call. The returned descriptor is released
    // by `WindowsLocalAllocation` below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut security_descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(windows_acl_error("read owner for", path, status));
    }
    let _security_descriptor = WindowsLocalAllocation(security_descriptor);
    if owner.is_null() {
        return Err(Error::Backend(format!(
            "failed to protect private path {}: Windows returned no owner",
            path.display()
        )));
    }

    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if directory {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: owner.cast(),
        },
    };
    let mut acl: *mut ACL = null_mut();
    // SAFETY: `access` points at the owner SID held alive by
    // `_security_descriptor`; `acl` is an out pointer released below.
    let status = unsafe { SetEntriesInAclW(1, &access, null(), &mut acl) };
    if status != ERROR_SUCCESS {
        return Err(windows_acl_error("build ACL for", path, status));
    }
    let _acl = WindowsLocalAllocation(acl.cast());

    // SAFETY: The path and ACL remain valid for this call. A protected DACL
    // removes inherited access and leaves one full-control ACE for the owner.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(windows_acl_error("set ACL on", path, status));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_acl_error(action: &str, path: &Path, status: u32) -> Error {
    Error::Backend(format!(
        "failed to {action} private path {}: {}",
        path.display(),
        std::io::Error::from_raw_os_error(status as i32)
    ))
}

fn configured_mcp_servers(config: &Config, agent: &AgentConfig) -> Result<Vec<McpServer>> {
    agent
        .runner
        .mcp_servers
        .iter()
        .map(|name| {
            let server = config.mcp_servers.get(name).ok_or_else(|| {
                Error::Backend(format!(
                    "harness references MCP server '{name}', but it is not configured"
                ))
            })?;
            mcp_server_from_config(name, server)
        })
        .collect()
}

fn continuation_task_id(task: &Task) -> Option<&str> {
    dashboard_task_metrics_enabled(task).then_some(task.id.as_str())
}

fn dashboard_task_metrics_enabled(task: &Task) -> bool {
    !task.hidden && task.task_type != "IDLE"
}

fn github_review_lifecycle_enabled(task: &Task) -> bool {
    task.context
        .as_ref()
        .and_then(|context| context.get("origin"))
        .and_then(Value::as_str)
        != Some("workflow")
}

fn control_plane_url(control_plane_port: u16, container_runtime: &str) -> String {
    let host = if container_runtime == "podman" {
        "host.containers.internal"
    } else {
        "host.docker.internal"
    };
    format!("http://{host}:{control_plane_port}")
}

#[derive(Clone, Copy)]
struct RunnerCallback<'a> {
    port: u16,
    token: &'a str,
    container_runtime: &'a str,
    collaboration_token: Option<&'a str>,
}

#[cfg(test)]
fn xpressclaw_control_mcp_server(
    agent_id: &str,
    task_id: Option<&str>,
    control_plane_port: u16,
    container_runtime: &str,
) -> McpServer {
    xpressclaw_control_mcp_server_for_context(
        agent_id,
        task_id,
        None,
        None,
        "/workspace",
        "/workspace",
        RunnerCallback {
            port: control_plane_port,
            token: "test-control-token",
            container_runtime,
            collaboration_token: None,
        },
    )
}

fn xpressclaw_control_mcp_server_for_context(
    agent_id: &str,
    task_id: Option<&str>,
    conversation_id: Option<&str>,
    project_id: Option<&str>,
    workspace: &str,
    repository: &str,
    callback: RunnerCallback<'_>,
) -> McpServer {
    let mut env = vec![
        EnvVariable::new(
            "XPRESSCLAW_URL",
            control_plane_url(callback.port, callback.container_runtime),
        ),
        EnvVariable::new("XPRESSCLAW_AGENT_ID", agent_id),
        EnvVariable::new("XPRESSCLAW_WORKSPACE", workspace),
        EnvVariable::new("XPRESSCLAW_REPOSITORY", repository),
        EnvVariable::new("XPRESSCLAW_CONTROL_TOKEN", callback.token),
    ];
    if let Some(task_id) = task_id {
        env.push(EnvVariable::new("XPRESSCLAW_TASK_ID", task_id));
    }
    if let Some(conversation_id) = conversation_id {
        env.push(EnvVariable::new(
            "XPRESSCLAW_CONVERSATION_ID",
            conversation_id,
        ));
    }
    if let Some(project_id) = project_id {
        env.push(EnvVariable::new("XPRESSCLAW_PROJECT_ID", project_id));
    }
    if let Some(token) = callback.collaboration_token {
        env.push(EnvVariable::new("XPRESSCLAW_LOCAL_COLLABORATION", "1"));
        env.push(EnvVariable::new("XPRESSCLAW_COLLABORATION_TOKEN", token));
    }
    // The control MCP must move in lockstep with the control plane. Runner
    // images are cached independently and can legitimately remain on an older
    // build, so execute the source embedded in this XpressClaw binary instead
    // of the image's compatibility copy.
    McpServer::Stdio(
        McpServerStdio::new("xpressclaw", BUNDLED_CONTROL_MCP_COMMAND)
            .args(vec![
                "--input-type=module".to_string(),
                "--eval".to_string(),
                BUNDLED_CONTROL_MCP_SOURCE.to_string(),
            ])
            .env(env),
    )
}

async fn configure_local_collaboration_access(
    db: &Arc<Database>,
    config: &Config,
    agent: &AgentConfig,
    spec: &mut ContainerSpec,
    docker: &DockerManager,
) -> Result<Option<String>> {
    if !config.collaboration.agent_authorized(&agent.name) {
        return Ok(None);
    }
    let installation_id = db.installation_id()?;
    let network = collaboration_network_name(&installation_id);
    let network = docker
        .installation_network_present(&network)
        .await
        .then_some(network);
    configure_local_collaboration_access_for_network(config, agent, spec, network.as_deref())
}

fn configure_local_collaboration_access_for_network(
    config: &Config,
    agent: &AgentConfig,
    spec: &mut ContainerSpec,
    network: Option<&str>,
) -> Result<Option<String>> {
    if !config.collaboration.agent_authorized(&agent.name) {
        return Ok(None);
    }
    let Some(network) = network else {
        warn!(
            agent = %agent.name,
            "local collaboration network is unavailable or not owned by this installation; continuing without collaboration tools"
        );
        return Ok(None);
    };
    let secrets = match CollaborationSecrets::load(&config.system.data_dir) {
        Ok(Some(secrets)) => secrets,
        Ok(None) => {
            warn!(
                agent = %agent.name,
                "local collaboration credentials are unavailable; continuing without collaboration tools"
            );
            return Ok(None);
        }
        Err(error) => {
            warn!(
                agent = %agent.name,
                %error,
                "local collaboration credentials could not be loaded; continuing without collaboration tools"
            );
            return Ok(None);
        }
    };
    spec.network_mode = Some(network.to_string());
    Ok(Some(secrets.capability_token_for_agent(&agent.name)))
}

fn is_absolute_container_path(path: &str) -> bool {
    // MCP servers run inside the Linux harness container, so their command
    // paths must not be interpreted using the desktop host's path semantics.
    path.starts_with('/')
}

fn mcp_server_from_config(name: &str, config: &McpServerConfig) -> Result<McpServer> {
    let headers = || {
        config
            .headers
            .iter()
            .map(|(name, value)| HttpHeader::new(name, value))
            .collect::<Vec<_>>()
    };
    match config.server_type.as_str() {
        "stdio" => {
            let command = config
                .command
                .as_deref()
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .ok_or_else(|| Error::Backend(format!("MCP server '{name}' has no command")))?;
            if !is_absolute_container_path(command) {
                return Err(Error::Backend(format!(
                    "MCP server '{name}' command must be an absolute path inside the harness container"
                )));
            }
            let env = config
                .env
                .iter()
                .map(|(name, value)| EnvVariable::new(name, value))
                .collect();
            Ok(McpServer::Stdio(
                McpServerStdio::new(name, command)
                    .args(config.args.clone())
                    .env(env),
            ))
        }
        "http" => {
            let url = config
                .url
                .as_deref()
                .ok_or_else(|| Error::Backend(format!("HTTP MCP server '{name}' has no URL")))?;
            Ok(McpServer::Http(
                McpServerHttp::new(name, url).headers(headers()),
            ))
        }
        "sse" => {
            let url = config
                .url
                .as_deref()
                .ok_or_else(|| Error::Backend(format!("SSE MCP server '{name}' has no URL")))?;
            Ok(McpServer::Sse(
                McpServerSse::new(name, url).headers(headers()),
            ))
        }
        other => Err(Error::Backend(format!(
            "MCP server '{name}' has unsupported transport '{other}'"
        ))),
    }
}

/// Merge harness defaults, workflow/task overrides, and the controls chosen
/// alongside the latest user message. Values stay keyed by opaque ACP option
/// IDs; the adapter remains the source of truth for what each option means.
fn requested_session_config(
    db: &Arc<Database>,
    agent: &AgentConfig,
    task_id: &str,
) -> Result<std::collections::HashMap<String, Value>> {
    let mut requested = agent.runner.session_config.clone();
    let task = TaskBoard::new(db.clone()).get(task_id)?;
    if let Some(overrides) = task
        .context
        .as_ref()
        .and_then(|context| context.get("session_config"))
        .and_then(Value::as_object)
    {
        requested.extend(
            overrides
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }

    let latest_message_payload: Option<String> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT payload FROM session_events
             WHERE task_id = ?1 AND event_type = 'task_message_received'
             ORDER BY id DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    if let Some(overrides) = latest_message_payload
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .as_ref()
        .and_then(|payload| payload.get("config_options"))
        .and_then(Value::as_object)
    {
        requested.extend(
            overrides
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    Ok(requested)
}

fn repository_volume_mounts(
    data_dir: &Path,
    agent_id: &str,
    repository: &RuntimeRepository,
) -> Result<Vec<VolumeMount>> {
    let mut mounts = vec![VolumeMount {
        source: repository.bootstrap_root.display().to_string(),
        target: repository.container_bootstrap.clone(),
        read_only: false,
        selinux_relabel: SelinuxRelabel::Shared,
    }];
    let dot_git = repository.active_root.join(".git");
    let linked_worktree = std::fs::symlink_metadata(&dot_git)
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_file());
    if !repository.active || !linked_worktree {
        return Ok(mounts);
    }

    let git_dir = repository.git_dir.as_deref().ok_or_else(|| {
        Error::Backend("active linked worktree has no authorized Git directory".to_string())
    })?;
    let git_common_dir = repository.git_common_dir.as_deref().ok_or_else(|| {
        Error::Backend("active linked worktree has no authorized common Git directory".to_string())
    })?;
    let container_git_dir = container_descendant_path(repository, git_dir)?;
    let container_git_common_dir = container_descendant_path(repository, git_common_dir)?;
    let topology = format!(
        "{}\0{container_git_dir}\0{container_git_common_dir}",
        repository.container_root
    );
    let topology_hash = format!("{:x}", Sha256::digest(topology.as_bytes()));
    let agent_hash = format!("{:x}", Sha256::digest(agent_id.as_bytes()));
    let runtime_dir = data_dir
        .join("runtime")
        .join("git-worktrees")
        .join(agent_hash)
        .join(topology_hash);
    let _redirect_guard = GIT_WORKTREE_REDIRECT_LOCK.lock().unwrap();
    std::fs::create_dir_all(&runtime_dir).map_err(|error| {
        Error::Backend(format!(
            "failed to create linked-worktree runtime directory {}: {error}",
            runtime_dir.display()
        ))
    })?;
    if let Some(agent_dir) = runtime_dir.parent() {
        set_private_directory_permissions(agent_dir)?;
    }
    set_private_directory_permissions(&runtime_dir)?;

    let dot_git_redirect = runtime_dir.join("dot-git");
    write_private_atomic(
        &dot_git_redirect,
        format!("gitdir: {container_git_dir}\n").as_bytes(),
    )?;
    mounts.push(VolumeMount {
        source: dot_git_redirect.display().to_string(),
        target: format!("{}/.git", repository.container_root),
        read_only: true,
        selinux_relabel: SelinuxRelabel::Shared,
    });

    if git_common_dir != git_dir {
        let host_commondir = git_dir.join("commondir");
        if !std::fs::symlink_metadata(&host_commondir)
            .ok()
            .is_some_and(|metadata| metadata.file_type().is_file())
        {
            return Err(Error::Backend(format!(
                "linked worktree Git directory {} has no regular commondir file",
                git_dir.display()
            )));
        }
        let common_redirect = runtime_dir.join("commondir");
        write_private_atomic(
            &common_redirect,
            format!("{container_git_common_dir}\n").as_bytes(),
        )?;
        mounts.push(VolumeMount {
            source: common_redirect.display().to_string(),
            target: format!("{container_git_dir}/commondir"),
            read_only: true,
            selinux_relabel: SelinuxRelabel::Shared,
        });
    }

    let host_gitdir_backlink = git_dir.join("gitdir");
    if std::fs::symlink_metadata(&host_gitdir_backlink)
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_file())
    {
        let gitdir_backlink = runtime_dir.join("gitdir");
        write_private_atomic(
            &gitdir_backlink,
            format!("{}/.git\n", repository.container_root).as_bytes(),
        )?;
        mounts.push(VolumeMount {
            source: gitdir_backlink.display().to_string(),
            target: format!("{container_git_dir}/gitdir"),
            read_only: true,
            selinux_relabel: SelinuxRelabel::Shared,
        });
    }
    Ok(mounts)
}

fn container_descendant_path(repository: &RuntimeRepository, host_path: &Path) -> Result<String> {
    let relative = host_path
        .strip_prefix(&repository.bootstrap_root)
        .map_err(|_| {
            Error::Backend(format!(
                "Git metadata path {} leaves the Agent workspace {}",
                host_path.display(),
                repository.bootstrap_root.display()
            ))
        })?;
    let components = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(value) => value.to_str().ok_or_else(|| {
                Error::Backend(format!(
                    "Git metadata path {} is not valid UTF-8",
                    host_path.display()
                ))
            }),
            _ => Err(Error::Backend(format!(
                "Git metadata path {} is not a normalized workspace descendant",
                host_path.display()
            ))),
        })
        .collect::<Result<Vec<_>>>()?;
    if components.is_empty() {
        Ok(repository.container_bootstrap.clone())
    } else {
        Ok(format!(
            "{}/{}",
            repository.container_bootstrap.trim_end_matches('/'),
            components.join("/")
        ))
    }
}

fn build_spec(
    config: &Config,
    agent: &AgentConfig,
    kind: &str,
    docker: &DockerManager,
    repository: &RuntimeRepository,
    github: Option<&github::GithubSessionAccess>,
) -> Result<ContainerSpec> {
    let image = resolved_runner_image(&agent.runner, kind)?;
    let command = acp_command_for(&agent.runner, kind, &repository.container_root)?;
    let command = with_startup_commands(command, &agent.runner.startup_commands);
    let mut volumes = repository_volume_mounts(&config.system.data_dir, &agent.name, repository)?;
    for volume in &agent.volumes {
        let mount = parse_volume(volume).ok_or_else(|| {
            Error::Backend(format!(
                "invalid additional mount {volume:?}; expected source:absolute-container-path[:options] with ro, rw, z, or Z options"
            ))
        })?;
        volumes.push(mount);
    }
    if agent.runner.subscription_auth {
        volumes.extend(auth_mounts(kind));
    }
    let mut environment = vec![
        "HOME=/home/node".to_string(),
        "CI=1".to_string(),
        "NO_COLOR=1".to_string(),
    ];
    if agent.runner.ssh_agent_forwarding {
        let access = discover_host_ssh_agent().ok_or_else(|| {
            Error::Backend(
                "host SSH-agent forwarding is enabled, but XpressClaw could not find a live Unix SSH_AUTH_SOCK; start an SSH agent and restart XpressClaw from that desktop session"
                    .to_string(),
            )
        })?;
        let retained_known_hosts =
            prepare_retained_ssh_known_hosts(&config.system.data_dir, &agent.name)?;
        let forwarded_config = access
            .config
            .as_deref()
            .map(|contents| {
                prepare_forwarded_ssh_config(&config.system.data_dir, &agent.name, contents)
            })
            .transpose()?;
        let socket_mount_source = ssh_agent_mount_source(
            &access.socket,
            docker.is_docker_desktop(),
            cfg!(target_os = "macos"),
        );
        apply_ssh_agent_forwarding(
            &access,
            &socket_mount_source,
            &retained_known_hosts,
            forwarded_config.as_deref(),
            &mut volumes,
            &mut environment,
        );
    }
    apply_codex_mode_default(kind, &agent.runner, &mut environment);
    let mut runner_environment = agent.runner.environment.iter().collect::<Vec<_>>();
    runner_environment.sort_by(|left, right| left.0.cmp(right.0));
    for (name, value) in runner_environment {
        if name.trim().is_empty() || name.contains('=') {
            return Err(Error::Backend(format!(
                "invalid harness environment variable name: {name:?}"
            )));
        }
        environment.push(format!("{name}={value}"));
    }
    github::extend_git_environment(&mut environment, github);
    if agent.runner.container_engine == ContainerEngineAccess::Host {
        let socket = docker.host_engine_socket().ok_or_else(|| {
            Error::DockerNotAvailable(
                "host container-engine access requires a local Docker-compatible Unix socket"
                    .to_string(),
            )
        })?;
        volumes.push(VolumeMount {
            source: socket.display().to_string(),
            target: "/var/run/docker.sock".to_string(),
            read_only: false,
            selinux_relabel: SelinuxRelabel::None,
        });
        environment.push("DOCKER_HOST=unix:///var/run/docker.sock".to_string());
    }

    Ok(ContainerSpec {
        image,
        memory_limit: Some(4 * 1024 * 1024 * 1024),
        cpu_limit: None,
        environment,
        volumes,
        network_mode: Some("bridge".to_string()),
        expose_port: None,
        cmd: Some(command),
        working_dir: Some(repository.container_root.clone()),
        run_as_host_user: true,
    })
}

/// Codex normally applies its own workspace sandbox inside the retained
/// XpressClaw container. The container is already the project security
/// boundary, so the nested sandbox only makes ordinary development tools less
/// reliable. Keep the adapter's explicit environment override authoritative.
fn apply_codex_mode_default(
    kind: &str,
    runner: &NativeRunnerConfig,
    environment: &mut Vec<String>,
) {
    if kind == "codex" && !runner.environment.contains_key(CODEX_INITIAL_AGENT_MODE) {
        environment.push(format!(
            "{CODEX_INITIAL_AGENT_MODE}={CODEX_FULL_ACCESS_MODE}"
        ));
    }
}

fn configure_bundled_github_mcp(
    runner: &NativeRunnerConfig,
    kind: &str,
    bundled_control_tools: bool,
    environment: &mut Vec<String>,
) -> Result<bool> {
    let attached = bundled_control_tools && !runner.mcp_servers.iter().any(|name| name == "github");
    if attached && kind == "codex" {
        const PREFIX: &str = "CODEX_CONFIG=";
        let existing_index = environment
            .iter()
            .position(|variable| variable.starts_with(PREFIX));
        let mut codex_environment = std::collections::HashMap::new();
        if let Some(index) = existing_index {
            codex_environment.insert(
                "CODEX_CONFIG".to_string(),
                environment[index][PREFIX.len()..].to_string(),
            );
        }
        // The retained container is shared by task and Conversation ACP
        // lanes, so its specification must not depend on the current lane.
        // Keep the complete, conditional guidance stable here; the scoped
        // GitHub MCP environment remains authoritative about whether an
        // ordinary task actually participates in the review lifecycle.
        github::add_codex_mcp_guidance(&mut codex_environment, true)?;
        let config = codex_environment
            .remove("CODEX_CONFIG")
            .ok_or_else(|| Error::Backend("GitHub guidance did not produce CODEX_CONFIG".into()))?;
        let variable = format!("{PREFIX}{config}");
        if let Some(index) = existing_index {
            environment[index] = variable;
        } else {
            environment.push(variable);
        }
    }
    Ok(attached)
}

fn with_startup_commands(command: Vec<String>, startup_commands: &[String]) -> Vec<String> {
    if startup_commands.is_empty() {
        return command;
    }
    let startup_commands = startup_commands
        .iter()
        .map(|command| command.trim())
        .filter(|command| !command.is_empty())
        .collect::<Vec<_>>();
    let mut initializer = Sha256::new();
    for startup_command in &startup_commands {
        initializer.update(startup_command.len().to_le_bytes());
        initializer.update(startup_command.as_bytes());
    }
    let marker = format!("/tmp/.xpressclaw-environment-{:x}", initializer.finalize());
    let mut script = format!("set -eu\nmarker={marker}\nif [ ! -e \"$marker\" ]; then\n");
    for startup_command in startup_commands {
        script.push_str("  ");
        script.push_str(startup_command);
        script.push('\n');
    }
    script.push_str("  touch \"$marker\"\nfi\nexec \"$@\"");
    let mut wrapped = vec![
        "/bin/sh".to_string(),
        "-lc".to_string(),
        script,
        "xpressclaw-startup".to_string(),
    ];
    wrapped.extend(command);
    wrapped
}

fn container_workspace_path(workspace: &Path, container_engine: ContainerEngineAccess) -> String {
    if container_engine == ContainerEngineAccess::Host && cfg!(unix) {
        workspace.display().to_string()
    } else {
        "/workspace".to_string()
    }
}

pub fn resolved_workspace(config: &Config, agent: &AgentConfig) -> PathBuf {
    let workspace = agent
        .runner
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(expand_home)
        .map(PathBuf::from)
        .unwrap_or_else(|| config.system.workspace_dir.clone());
    canonical_or_original(&workspace)
}

/// Resolve only runner paths that the Agent can write and that map back to a
/// known host directory. XpressClaw-injected credential/config mounts are not
/// part of `agent.volumes`, so they cannot become visualization sources.
fn visualization_source_roots(
    workspace: &Path,
    container_workspace: &str,
    agent: &AgentConfig,
) -> Vec<VisualizationSourceRoot> {
    let mut roots = vec![VisualizationSourceRoot::new(container_workspace, workspace)];
    for raw in &agent.volumes {
        let Some(mount) = parse_volume(raw) else {
            continue;
        };
        if mount.read_only
            || !is_absolute_runner_root(&mount.target)
            || !Path::new(&mount.source).is_absolute()
        {
            continue;
        }
        let root = VisualizationSourceRoot::new(mount.target, mount.source);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

pub fn resolved_runner_image(config: &NativeRunnerConfig, kind: &str) -> Result<String> {
    let desired_default = default_native_runner_image(kind, config.container_engine);
    let alternate_mode = match config.container_engine {
        ContainerEngineAccess::None => ContainerEngineAccess::Host,
        ContainerEngineAccess::Host => ContainerEngineAccess::None,
    };
    let alternate_default = default_native_runner_image(kind, alternate_mode);
    let configured_image = config.image.trim();
    let built_in_image = [desired_default, alternate_default]
        .into_iter()
        .flatten()
        .any(|image| {
            configured_image == image || local_runner_image_alias(image) == Some(configured_image)
        });
    if configured_image.is_empty() || built_in_image {
        return desired_default.map(str::to_owned).ok_or_else(|| {
            Error::Backend(format!(
                "runner '{kind}' requires an explicit container image"
            ))
        });
    }
    Ok(configured_image.to_string())
}

/// Local tags used by the ACP runner images. A published image is the
/// default, but retaining these aliases lets existing developer builds run
/// without a forced retag or registry pull.
pub fn local_runner_image_alias(image: &str) -> Option<&'static str> {
    local_runner_image(image)
}

fn acp_command_for(
    config: &NativeRunnerConfig,
    kind: &str,
    container_workspace: &str,
) -> Result<Vec<String>> {
    if !config.command.is_empty() {
        return Ok(config
            .command
            .iter()
            .map(|part| part.replace("{workspace}", container_workspace))
            .collect());
    }
    agent_definition(kind)
        .map(|agent| {
            agent
                .command
                .iter()
                .map(|part| (*part).to_string())
                .collect()
        })
        .ok_or_else(|| {
            Error::Backend(format!(
                "ACP runner '{kind}' requires an explicit server command"
            ))
        })
}

fn auth_candidates(kind: &str) -> Vec<(PathBuf, &'static str, bool)> {
    let Some(home) = host_home() else {
        return Vec::new();
    };
    agent_definition(kind)
        .map(|agent| {
            agent
                .auth_mounts
                .iter()
                .map(|mount| (home.join(mount.source), mount.target, mount.read_only))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the host has a standard login location that can be mounted for the
/// selected agent product. This intentionally reports only presence, never
/// credential contents.
pub fn subscription_auth_available(kind: &str) -> bool {
    auth_candidates(kind)
        .iter()
        .any(|(source, _, _)| source.exists())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostSshAgentAccess {
    socket: PathBuf,
    generation: String,
    config: Option<Vec<u8>>,
    known_hosts: Option<PathBuf>,
}

/// Return the live host SSH-agent socket that XpressClaw would forward. The
/// path is exposed for readiness diagnostics only; private key files are never
/// inspected or mounted.
pub fn host_ssh_agent_socket() -> Option<PathBuf> {
    discover_host_ssh_agent().map(|access| access.socket)
}

fn discover_host_ssh_agent() -> Option<HostSshAgentAccess> {
    let home = host_home();
    discover_host_ssh_agent_from(
        ssh_agent_socket_candidates(home.as_deref()),
        home.as_deref(),
    )
}

fn discover_host_ssh_agent_from(
    candidates: Vec<PathBuf>,
    home: Option<&Path>,
) -> Option<HostSshAgentAccess> {
    let socket = candidates
        .into_iter()
        .find(|candidate| live_ssh_agent_socket(candidate))?;
    let socket = socket.canonicalize().unwrap_or(socket);
    let config = home
        .and_then(|home| regular_ssh_file(home, "config").map(|config| (home, config)))
        .and_then(|(home, config)| materialize_ssh_config(&config, home));
    let known_hosts = home.and_then(|home| regular_ssh_file(home, "known_hosts"));
    let generation = ssh_forwarding_generation(&socket, config.as_deref(), known_hosts.as_deref())?;
    Some(HostSshAgentAccess {
        socket,
        generation,
        config,
        known_hosts,
    })
}

fn ssh_agent_socket_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(socket) = std::env::var_os("SSH_AUTH_SOCK") {
        candidates.push(PathBuf::from(socket));
    }
    #[cfg(unix)]
    {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                // SAFETY: getuid has no preconditions and only reads process
                // credentials.
                PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
            });
        candidates.extend([
            runtime.join("gcr/ssh"),
            runtime.join("keyring/ssh"),
            runtime.join("gnupg/S.gpg-agent.ssh"),
            runtime.join("ssh-agent.socket"),
        ]);
    }
    if let Some(home) = home {
        candidates.extend([
            home.join(".1password/agent.sock"),
            home.join(".ssh/agent.sock"),
        ]);
        #[cfg(target_os = "macos")]
        candidates.extend([
            home.join("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"),
            home.join("Library/Containers/com.maxgoedjen.Secretive.SecretAgent/Data/socket.ssh"),
        ]);
    }
    let mut unique = Vec::new();
    for candidate in candidates {
        if !unique.contains(&candidate) {
            unique.push(candidate);
        }
    }
    unique
}

#[cfg(unix)]
fn live_ssh_agent_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;

    path.is_absolute()
        && std::fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
        && std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(not(unix))]
fn live_ssh_agent_socket(_path: &Path) -> bool {
    false
}

fn materialize_ssh_config(config: &Path, home: &Path) -> Option<Vec<u8>> {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let ssh_dir = home.join(".ssh");
    let ssh_dir = ssh_dir.canonicalize().unwrap_or(ssh_dir);
    let mut stack = HashSet::new();
    let mut included = 0;
    let mut output = String::new();
    expand_ssh_config_file(
        config,
        &home,
        &ssh_dir,
        false,
        &mut stack,
        &mut included,
        &mut output,
    )?;
    Some(output.into_bytes())
}

fn expand_ssh_config_file(
    source: &Path,
    home: &Path,
    ssh_dir: &Path,
    reject_private_key: bool,
    stack: &mut HashSet<PathBuf>,
    included: &mut usize,
    output: &mut String,
) -> Option<()> {
    let source = safe_ssh_config_source(source, home, ssh_dir, reject_private_key)?;
    if !stack.insert(source.clone()) {
        warn!(path = %source.display(), "cyclic SSH Include was skipped");
        return Some(());
    }
    let result = (|| {
        let contents = std::fs::read_to_string(&source).ok()?;
        for line in contents.split_inclusive('\n') {
            let Some(patterns) = ssh_include_patterns(line) else {
                output.push_str(line);
                continue;
            };
            for pattern in patterns {
                let Some(host_pattern) = ssh_include_host_pattern(&pattern, home, ssh_dir) else {
                    warn!(pattern, "skipping an unsupported SSH Include pattern");
                    continue;
                };
                let Some(host_pattern) = host_pattern.to_str() else {
                    warn!(pattern, "skipping a non-Unicode SSH Include pattern");
                    continue;
                };
                let Ok(matches) = glob::glob(host_pattern) else {
                    warn!(pattern, "skipping an invalid SSH Include glob");
                    continue;
                };
                let mut matches = matches
                    .filter_map(std::result::Result::ok)
                    .collect::<Vec<_>>();
                matches.sort();
                for candidate in matches {
                    if *included >= SSH_CONFIG_INCLUDE_LIMIT {
                        warn!(
                            limit = SSH_CONFIG_INCLUDE_LIMIT,
                            "SSH Include file limit reached; remaining files were skipped"
                        );
                        continue;
                    }
                    *included += 1;
                    let before = output.len();
                    let _ = expand_ssh_config_file(
                        &candidate, home, ssh_dir, true, stack, included, output,
                    );
                    if output.len() > before && !output.ends_with('\n') {
                        output.push('\n');
                    }
                }
            }
        }
        Some(())
    })();
    stack.remove(&source);
    result
}

fn safe_ssh_config_source(
    path: &Path,
    home: &Path,
    ssh_dir: &Path,
    reject_private_key: bool,
) -> Option<PathBuf> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > SSH_CONFIG_FILE_SIZE_LIMIT {
        return None;
    }
    let path = path.canonicalize().ok()?;
    if !path.starts_with(home) && !path.starts_with(ssh_dir) {
        warn!(path = %path.display(), "SSH Include outside the host home directory was not forwarded");
        return None;
    }
    if reject_private_key {
        let contents = std::fs::read(&path).ok()?;
        if looks_like_private_ssh_key(&path, &contents)
            || !contains_only_ssh_config_directives(&contents)
        {
            warn!(path = %path.display(), "SSH Include was not established as a safe configuration file");
            return None;
        }
    }
    Some(path)
}

fn looks_like_private_ssh_key(path: &Path, contents: &[u8]) -> bool {
    let private_key_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("id_") && !name.ends_with(".pub"));
    let private_key_header = [
        b"PRIVATE KEY-----".as_slice(),
        b"PuTTY-User-Key-File-".as_slice(),
        b"BEGIN SSH2 ENCRYPTED PRIVATE KEY".as_slice(),
        b"SSH PRIVATE KEY FILE FORMAT".as_slice(),
        b"openssh-key-v1\0".as_slice(),
    ]
    .iter()
    .any(|marker| {
        contents
            .windows(marker.len())
            .any(|window| window == *marker)
    });
    private_key_name || private_key_header
}

fn contains_only_ssh_config_directives(contents: &[u8]) -> bool {
    let Ok(contents) = std::str::from_utf8(contents) else {
        return false;
    };
    let mut found_directive = false;
    for line in contents.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let keyword_end = line
            .find(|character: char| character.is_ascii_whitespace() || character == '=')
            .unwrap_or(line.len());
        let keyword = &line[..keyword_end];
        if !SSH_CONFIG_DIRECTIVES
            .iter()
            .any(|known| keyword.eq_ignore_ascii_case(known))
        {
            return false;
        }
        found_directive = true;
    }
    found_directive
}

// Keep this conservative allowlist aligned with OpenSSH's client-side
// readconf.c keywords. Included files with an unknown first token are skipped
// instead of risking that an arbitrary secret matched by a broad glob is
// copied into the harness container. Common portable platform extensions are
// included explicitly.
const SSH_CONFIG_DIRECTIVES: &[&str] = &[
    "addkeystoagent",
    "addressfamily",
    "afstokenpassing",
    "batchmode",
    "bindaddress",
    "bindinterface",
    "canonicaldomains",
    "canonicalizefallbacklocal",
    "canonicalizehostname",
    "canonicalizemaxdots",
    "canonicalizepermittedcnames",
    "casignaturealgorithms",
    "certificatefile",
    "challengeresponseauthentication",
    "channeltimeout",
    "checkhostip",
    "cipher",
    "ciphers",
    "clearallforwardings",
    "compression",
    "compressionlevel",
    "connectionattempts",
    "connecttimeout",
    "controlmaster",
    "controlpath",
    "controlpersist",
    "dsaauthentication",
    "dynamicforward",
    "enableescapecommandline",
    "enablesshkeysign",
    "escapechar",
    "exitonforwardfailure",
    "fallbacktorsh",
    "fingerprinthash",
    "forkafterauthentication",
    "forwardagent",
    "forwardx11",
    "forwardx11timeout",
    "forwardx11trusted",
    "gatewayports",
    "globalknownhostsfile",
    "globalknownhostsfile2",
    "gssapiauthentication",
    "gssapidelegatecredentials",
    "gssapikexalgorithms",
    "gssapikeyexchange",
    "gssapirenewalforcesrekey",
    "gssapitrustdns",
    "hashknownhosts",
    "host",
    "hostbasedacceptedalgorithms",
    "hostbasedauthentication",
    "hostbasedkeytypes",
    "hostkeyalgorithms",
    "hostkeyalias",
    "hostname",
    "identityagent",
    "identityfile",
    "identityfile2",
    "identitiesonly",
    "ignoreunknown",
    "include",
    "ipqos",
    "kbdinteractiveauthentication",
    "kbdinteractivedevices",
    "keepalive",
    "kerberosauthentication",
    "kerberostgtpassing",
    "kexalgorithms",
    "knownhostscommand",
    "localcommand",
    "localforward",
    "loglevel",
    "logverbose",
    "macs",
    "match",
    "nohostauthenticationforlocalhost",
    "numberofpasswordprompts",
    "obscurekeystroketiming",
    "passwordauthentication",
    "permitlocalcommand",
    "permitremoteopen",
    "pkcs11provider",
    "port",
    "preferredauthentications",
    "protocol",
    "proxycommand",
    "proxyjump",
    "proxyusefdpass",
    "pubkeyacceptedalgorithms",
    "pubkeyacceptedkeytypes",
    "pubkeyauthentication",
    "refuseconnection",
    "rekeylimit",
    "remotecommand",
    "remoteforward",
    "requesttty",
    "requiredrsasize",
    "revokedhostkeys",
    "rhostsauthentication",
    "rhostsrsaauthentication",
    "rsaauthentication",
    "securitykeyprovider",
    "sendenv",
    "serveralivecountmax",
    "serveraliveinterval",
    "sessiontype",
    "setenv",
    "skeyauthentication",
    "smartcarddevice",
    "stdinnull",
    "streamlocalbindmask",
    "streamlocalbindunlink",
    "stricthostkeychecking",
    "syslogfacility",
    "tag",
    "tcpkeepalive",
    "tisauthentication",
    "tunnel",
    "tunneldevice",
    "updatehostkeys",
    "usekeychain",
    "useprivilegedport",
    "user",
    "userknownhostsfile",
    "userknownhostsfile2",
    "useroaming",
    "usersh",
    "verifyhostkeydns",
    "versionaddendum",
    "visualhostkey",
    "warnweakcrypto",
    "xauthlocation",
];

fn ssh_include_patterns(line: &str) -> Option<Vec<String>> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let keyword_end = line
        .find(|character: char| character.is_ascii_whitespace() || character == '=')
        .unwrap_or(line.len());
    if !line[..keyword_end].eq_ignore_ascii_case("include") {
        return None;
    }
    let mut arguments = line[keyword_end..].trim_start();
    if let Some(rest) = arguments.strip_prefix('=') {
        arguments = rest.trim_start();
    }
    Some(split_ssh_config_arguments(arguments).unwrap_or_default())
}

fn split_ssh_config_arguments(arguments: &str) -> Option<Vec<String>> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in arguments.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '#' {
            break;
        } else if character.is_ascii_whitespace() {
            if !current.is_empty() {
                values.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        values.push(current);
    }
    Some(values)
}

fn ssh_include_host_pattern(pattern: &str, home: &Path, ssh_dir: &Path) -> Option<PathBuf> {
    if let Some(relative) = pattern
        .strip_prefix("~/")
        .or_else(|| pattern.strip_prefix("%d/"))
        .or_else(|| pattern.strip_prefix("${HOME}/"))
    {
        return Some(home.join(relative));
    }
    if pattern.contains('%') || pattern.contains("${") || pattern.starts_with('~') {
        return None;
    }
    let pattern = PathBuf::from(pattern);
    if pattern.is_absolute() {
        Some(pattern)
    } else {
        Some(ssh_dir.join(pattern))
    }
}

#[cfg(unix)]
fn ssh_forwarding_generation(
    socket: &Path,
    config: Option<&[u8]>,
    known_hosts: Option<&Path>,
) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let mut digest = Sha256::new();
    for (label, path) in [("socket", Some(socket)), ("known_hosts", known_hosts)] {
        digest.update(label.as_bytes());
        if let Some(path) = path {
            let metadata = std::fs::metadata(path).ok()?;
            digest.update(path.to_string_lossy().as_bytes());
            digest.update(metadata.dev().to_le_bytes());
            digest.update(metadata.ino().to_le_bytes());
        } else {
            digest.update(b"absent");
        }
    }
    digest.update(b"config");
    if let Some(config) = config {
        digest.update(config);
    } else {
        digest.update(b"absent");
    }
    Some(format!("{:x}", digest.finalize()))
}

#[cfg(not(unix))]
fn ssh_forwarding_generation(
    _socket: &Path,
    _config: Option<&[u8]>,
    _known_hosts: Option<&Path>,
) -> Option<String> {
    None
}

fn regular_ssh_file(home: &Path, name: &str) -> Option<PathBuf> {
    let path = home.join(".ssh").join(name);
    std::fs::symlink_metadata(&path)
        .ok()
        .filter(|metadata| metadata.file_type().is_file())
        .map(|_| path)
}

fn retained_ssh_runtime_dir(data_dir: &Path, agent_id: &str) -> PathBuf {
    let agent_hash = format!("{:x}", Sha256::digest(agent_id.as_bytes()));
    data_dir
        .join("runtime")
        .join("ssh-known-hosts")
        .join(agent_hash)
}

fn prepare_retained_ssh_known_hosts(data_dir: &Path, agent_id: &str) -> Result<PathBuf> {
    let runtime_dir = retained_ssh_runtime_dir(data_dir, agent_id);
    std::fs::create_dir_all(&runtime_dir).map_err(|error| {
        Error::Backend(format!(
            "failed to create retained SSH host-key directory {}: {error}",
            runtime_dir.display()
        ))
    })?;
    set_private_directory_permissions(&runtime_dir)?;

    let known_hosts = runtime_dir.join("known_hosts");
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&known_hosts).map_err(|error| {
        Error::Backend(format!(
            "failed to create retained SSH host-key file {}: {error}",
            known_hosts.display()
        ))
    })?;
    set_private_file_permissions(&known_hosts)?;
    Ok(runtime_dir)
}

fn prepare_forwarded_ssh_config(
    data_dir: &Path,
    agent_id: &str,
    contents: &[u8],
) -> Result<PathBuf> {
    let agent_hash = format!("{:x}", Sha256::digest(agent_id.as_bytes()));
    let runtime_dir = data_dir.join("runtime").join("ssh-config").join(agent_hash);
    std::fs::create_dir_all(&runtime_dir).map_err(|error| {
        Error::Backend(format!(
            "failed to create forwarded SSH configuration directory {}: {error}",
            runtime_dir.display()
        ))
    })?;
    set_private_directory_permissions(&runtime_dir)?;
    let config = runtime_dir.join("config");
    write_private_atomic(&config, contents)?;
    Ok(config)
}

fn apply_ssh_agent_forwarding(
    access: &HostSshAgentAccess,
    socket_mount_source: &Path,
    retained_known_hosts: &Path,
    forwarded_config: Option<&Path>,
    volumes: &mut Vec<VolumeMount>,
    environment: &mut Vec<String>,
) {
    volumes.push(VolumeMount {
        source: socket_mount_source.display().to_string(),
        target: SSH_AGENT_SOCKET_TARGET.to_string(),
        read_only: false,
        // A shared label makes rootless Podman/Docker usable on SELinux hosts
        // without exposing any private-key files.
        selinux_relabel: SelinuxRelabel::Shared,
    });
    environment.push(format!("SSH_AUTH_SOCK={SSH_AGENT_SOCKET_TARGET}"));
    // Bind mounts stay attached to an inode when a socket or SSH metadata file
    // is atomically replaced at the same pathname. Including every mounted
    // source identity in the spec forces the retained container to rebind all
    // current sources on the next turn.
    environment.push(format!(
        "XPRESSCLAW_SSH_FORWARDING_GENERATION={}",
        access.generation
    ));

    volumes.push(VolumeMount {
        source: retained_known_hosts.display().to_string(),
        target: SSH_RUNTIME_DIR_TARGET.to_string(),
        read_only: false,
        selinux_relabel: SelinuxRelabel::Shared,
    });

    let mut command = String::from("ssh");
    if let Some(config) = forwarded_config {
        volumes.push(VolumeMount {
            source: config.display().to_string(),
            target: SSH_CONFIG_TARGET.to_string(),
            read_only: true,
            selinux_relabel: SelinuxRelabel::Shared,
        });
        command.push_str(&format!(" -F {SSH_CONFIG_TARGET}"));
    }
    command.push_str(&format!(
        " -o IdentityAgent={SSH_AGENT_SOCKET_TARGET} -o IdentitiesOnly=no"
    ));
    if let Some(known_hosts) = &access.known_hosts {
        volumes.push(VolumeMount {
            source: known_hosts.display().to_string(),
            target: SSH_KNOWN_HOSTS_TARGET.to_string(),
            read_only: true,
            selinux_relabel: SelinuxRelabel::Shared,
        });
        command.push_str(&format!(
            " -o 'UserKnownHostsFile={SSH_RETAINED_KNOWN_HOSTS} {SSH_KNOWN_HOSTS_TARGET}'"
        ));
    } else {
        command.push_str(&format!(
            " -o UserKnownHostsFile={SSH_RETAINED_KNOWN_HOSTS}"
        ));
    }
    command.push_str(" -o StrictHostKeyChecking=accept-new");
    environment.push(format!("GIT_SSH_COMMAND={command}"));
}

fn ssh_agent_mount_source(host_socket: &Path, docker_desktop: bool, macos_host: bool) -> PathBuf {
    if docker_desktop && macos_host {
        PathBuf::from(DOCKER_DESKTOP_SSH_AGENT_SOURCE)
    } else {
        host_socket.to_path_buf()
    }
}

fn auth_mounts(kind: &str) -> Vec<VolumeMount> {
    auth_candidates(kind)
        .into_iter()
        .filter(|(source, _, _)| source.exists())
        .map(|(source, target, read_only)| VolumeMount {
            source: source.display().to_string(),
            target: target.to_string(),
            // Native agent OAuth directories must be writable so their CLIs
            // can refresh credentials; common Git/GitHub config is read-only.
            read_only,
            selinux_relabel: SelinuxRelabel::Shared,
        })
        .collect()
}

fn host_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn parse_volume(raw: &str) -> Option<VolumeMount> {
    let mut mount = VolumeMount::parse(raw)?;
    mount.source = expand_home(&mount.source);
    Some(mount)
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = host_home() {
            return home.join(rest).display().to_string();
        }
    }
    path.to_string()
}

fn canonical_or_original(path: &Path) -> PathBuf {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim(resolved)
}

/// Strip the `\\?\` prefix Windows `canonicalize()` adds to drive paths;
/// Docker Desktop's bind-mount parser rejects it. Others pass through as-is.
#[cfg(windows)]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

#[cfg(not(windows))]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
}

fn needs_user_input(summary: &str) -> bool {
    if summary.lines().any(|line| {
        line.trim_start()
            .trim_start_matches(['`', '*', '-', '#', ' '])
            .starts_with("NEEDS_USER_INPUT:")
    }) {
        return true;
    }

    // Native products do not share a structured "ask the user" event. Treat
    // a final, direct question as waiting without requiring XpressClaw to
    // rewrite the user's task or inject its own agent protocol.
    let last_line = summary
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .trim_matches(['`', '*', '_', '#', ' ']);
    if !last_line.ends_with('?') {
        return false;
    }
    let question = last_line.to_lowercase();
    [
        "which ",
        "what ",
        "where ",
        "when ",
        "how ",
        "can you ",
        "could you ",
        "would you ",
        "should i ",
        "do you ",
        "please ",
        "i need ",
    ]
    .iter()
    .any(|signal| question.starts_with(signal) || question.contains(&format!(" {signal}")))
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated: String = value.chars().take(max_chars).collect();
    truncated.push_str("\n… output truncated …");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(repository: &Path, arguments: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }

    fn add_test_agent(db: &Arc<Database>, agent_id: &str) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO agents (id, name, backend, config) VALUES (?1, ?1, 'native', '{}')",
                [agent_id],
            )
        })
        .unwrap();
    }

    #[test]
    fn opencode_nested_repository_becomes_cwd_and_accepts_bundled_github_mcp() {
        let workspace = tempfile::tempdir().unwrap();
        let repository = workspace.path().join("product");
        std::fs::create_dir_all(&repository).unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["init", "--quiet"])
            .status()
            .unwrap()
            .success());
        let db = Arc::new(Database::open_memory().unwrap());
        add_test_agent(&db, "opencode-agent");
        let manager = AgentRepositoryManager::new(db);
        manager
            .apply_boundary("opencode-agent", workspace.path())
            .unwrap();
        let inspection = manager.inspect("opencode-agent", workspace.path()).unwrap();
        let mut agent = AgentConfig {
            name: "opencode-agent".into(),
            backend: "opencode".into(),
            ..AgentConfig::default()
        };
        agent.runner.kind = "opencode".into();
        agent.runner.workspace = Some(workspace.path().display().to_string());
        let runtime = RuntimeRepository::from_inspection(inspection, &agent);
        assert_eq!(runtime.container_bootstrap, "/workspace");
        assert_eq!(runtime.container_root, "/workspace/product");

        let mut environment = Vec::new();
        assert!(
            configure_bundled_github_mcp(&agent.runner, "opencode", true, &mut environment,)
                .unwrap()
        );
        let control = xpressclaw_control_mcp_server_for_context(
            &agent.name,
            Some("task"),
            None,
            Some("project"),
            &runtime.container_bootstrap,
            &runtime.container_root,
            RunnerCallback {
                port: 8935,
                token: "control",
                container_runtime: "docker",
                collaboration_token: None,
            },
        );
        let McpServer::Stdio(control) = control else {
            panic!("control MCP must use stdio");
        };
        assert!(control.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_REPOSITORY" && variable.value == "/workspace/product"
        }));

        let github = github::mcp_server(&github::GithubMcpContext {
            control_plane_url: "http://host.docker.internal:8935".into(),
            control_plane_token: agent_callback_capability("control", &agent.name),
            agent_id: agent.name.clone(),
            workspace: runtime.container_bootstrap,
            active_repository: Some(runtime.container_root),
            task_id: Some("task".into()),
            review_lifecycle: true,
        });
        let McpServer::Stdio(github) = github else {
            panic!("GitHub MCP must use stdio");
        };
        assert!(github.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_REPOSITORY" && variable.value == "/workspace/product"
        }));
        assert!(github
            .env
            .iter()
            .any(|variable| variable.name == "XPRESSCLAW_TASK_ID" && variable.value == "task"));
        assert!(github.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE" && variable.value == "1"
        }));
        assert!(github
            .env
            .iter()
            .all(|variable| variable.name != "GH_TOKEN" && variable.name != "GH_REPO"));
    }

    #[test]
    fn linked_worktree_mounts_translate_git_metadata_into_the_container() {
        let workspace = tempfile::tempdir().unwrap();
        let primary = workspace.path().join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        run_git(&primary, &["init", "-q"]);
        run_git(
            &primary,
            &[
                "-c",
                "user.name=XpressClaw Tests",
                "-c",
                "user.email=tests@xpressclaw.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        );
        let linked = workspace.path().join("feature");
        run_git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().unwrap(),
            ],
        );
        let original_dot_git = std::fs::read_to_string(linked.join(".git")).unwrap();
        assert!(original_dot_git.starts_with("gitdir: "));
        assert!(!original_dot_git.contains("/workspace/primary"));

        let db = Arc::new(Database::open_memory().unwrap());
        add_test_agent(&db, "opencode-agent");
        let manager = AgentRepositoryManager::new(db);
        let inspection = manager
            .select("opencode-agent", workspace.path(), "feature")
            .unwrap()
            .inspection;
        let mut agent = AgentConfig {
            name: "opencode-agent".into(),
            backend: "opencode".into(),
            ..AgentConfig::default()
        };
        agent.runner.kind = "opencode".into();
        agent.runner.workspace = Some(workspace.path().display().to_string());
        let runtime = RuntimeRepository::from_inspection(inspection, &agent);
        let data_dir = tempfile::tempdir().unwrap();

        let mounts = repository_volume_mounts(data_dir.path(), &agent.name, &runtime).unwrap();
        assert_eq!(mounts.len(), 4);
        assert!(mounts.iter().any(|mount| {
            mount.source
                == workspace
                    .path()
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
                && mount.target == "/workspace"
                && !mount.read_only
        }));
        let expected = [
            (
                "/workspace/feature/.git",
                "gitdir: /workspace/primary/.git/worktrees/feature\n",
            ),
            (
                "/workspace/primary/.git/worktrees/feature/commondir",
                "/workspace/primary/.git\n",
            ),
            (
                "/workspace/primary/.git/worktrees/feature/gitdir",
                "/workspace/feature/.git\n",
            ),
        ];
        for (target, contents) in expected {
            let mount = mounts.iter().find(|mount| mount.target == target).unwrap();
            assert!(mount.read_only);
            assert_eq!(mount.selinux_relabel, SelinuxRelabel::Shared);
            assert_eq!(std::fs::read_to_string(&mount.source).unwrap(), contents);
        }
        assert_eq!(
            std::fs::read_to_string(linked.join(".git")).unwrap(),
            original_dot_git
        );
    }

    #[test]
    fn agent_runtime_cleanup_is_idempotent_and_preserves_workspaces() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        let agent_id = "atlas";
        let agent_hash = format!("{:x}", Sha256::digest(agent_id.as_bytes()));
        for runtime_kind in ["pi-mcp", "ssh-known-hosts", "ssh-config", "git-worktrees"] {
            let runtime_dir = data_dir
                .join("runtime")
                .join(runtime_kind)
                .join(&agent_hash);
            std::fs::create_dir_all(&runtime_dir).unwrap();
            std::fs::write(runtime_dir.join("generated.conf"), "owned").unwrap();
        }
        let workspace = data_dir.join("workspaces").join(agent_id);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("user-source.txt"), "preserve").unwrap();

        remove_agent_runtime_state(&data_dir, agent_id).unwrap();
        remove_agent_runtime_state(&data_dir, agent_id).unwrap();

        for runtime_kind in ["pi-mcp", "ssh-known-hosts", "ssh-config", "git-worktrees"] {
            assert!(!data_dir
                .join("runtime")
                .join(runtime_kind)
                .join(&agent_hash)
                .exists());
        }
        assert_eq!(
            std::fs::read_to_string(workspace.join("user-source.txt")).unwrap(),
            "preserve"
        );
    }

    #[tokio::test]
    async fn retiring_conversations_removes_all_registered_acp_lanes() {
        let processes = ConversationAcpProcesses::default();
        processes.slot("conversation-one", "atlas");
        processes.slot("conversation-one", "reviewer");
        processes.slot("conversation-two", "atlas");
        processes.slot("conversation-two", "reviewer");
        assert_eq!(processes.slot_count(), 4);

        assert_eq!(processes.retire_agent_everywhere("atlas").await, 2);
        assert_eq!(processes.slot_count(), 2);
        assert_eq!(processes.retire_conversation("conversation-one").await, 1);
        assert_eq!(processes.slot_count(), 1);
        assert_eq!(
            processes.retire_agent("conversation-two", "reviewer").await,
            1
        );
        assert_eq!(processes.slot_count(), 0);
    }

    #[tokio::test]
    async fn runtime_quiescence_waits_for_only_the_selected_agents() {
        let lifecycle = Arc::new(NativeRuntimeLifecycle::default());
        let active = lifecycle.enter("atlas").await;
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        let quiescing = lifecycle.clone();
        tokio::spawn(async move {
            let guards = quiescing.quiesce_agents(&["atlas".to_string()]).await;
            let _ = sender.send(guards);
        });
        tokio::task::yield_now().await;
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        let other_agent = tokio::time::timeout(Duration::from_secs(1), lifecycle.enter("reviewer"))
            .await
            .expect("an unrelated Agent must not be blocked");
        drop(other_agent);

        drop(active);
        let guards = tokio::time::timeout(Duration::from_secs(1), receiver)
            .await
            .expect("quiescence should acquire after active work exits")
            .expect("quiescence task should return its guards");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), lifecycle.enter("atlas"))
                .await
                .is_err(),
            "new work must wait while destructive cleanup holds the barrier"
        );
        drop(guards);
        tokio::time::timeout(Duration::from_secs(1), lifecycle.enter("atlas"))
            .await
            .expect("new work should resume after cleanup releases the barrier");
    }

    #[test]
    fn conversation_prompt_stops_at_its_claimed_trigger_and_explains_reply_delivery() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects (id, name) VALUES ('p', 'Project')",
                [],
            )?;
            conn.execute(
                "INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'p')",
                [],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let manager = ConversationManager::new(db);
        let conversation = manager
            .create_in_project(
                Some("p"),
                &crate::conversations::CreateConversation {
                    title: Some("Race check".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into()],
                },
            )
            .unwrap();
        manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "First context".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let trigger = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Claimed request".into(),
                    message_type: None,
                },
            )
            .unwrap();
        manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Arrived after claim".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let turn = ConversationTurn {
            id: "turn".into(),
            conversation_id: conversation.id.clone(),
            agent_id: "atlas".into(),
            trigger_message_id: Some(trigger.id),
            status: "running".into(),
            result_message_id: None,
            error_message: None,
            context_used: None,
            context_size: None,
            queued_at: "now".into(),
            started_at: None,
            completed_at: None,
            response_queued_at: None,
            response_started_at: None,
        };
        let agent = AgentConfig {
            name: "atlas".into(),
            runner: NativeRunnerConfig {
                project_name: Some("Atlas".into()),
                ..Default::default()
            },
            ..Default::default()
        };

        let prompt =
            build_conversation_prompt(&manager, &conversation, &turn, &agent, None).unwrap();
        assert!(prompt.contains("First context"));
        assert!(prompt.contains("Claimed request"));
        assert!(!prompt.contains("Arrived after claim"));
        assert!(prompt.contains(
            "Your normal final response is automatically delivered to this project conversation; use it for your one final reply."
        ));
        assert!(prompt.contains(
            "Reserve send_conversation_message for genuine interim updates or publishing workspace files while you continue working."
        ));
        assert!(prompt.contains("Never use the tool to duplicate your final response."));
    }

    #[test]
    fn mcp_commands_use_container_path_semantics() {
        assert!(is_absolute_container_path("/opt/project/mcp-server"));
        assert!(!is_absolute_container_path("npx"));
        assert!(!is_absolute_container_path(r"C:\tools\mcp-server.exe"));
    }

    #[test]
    fn converts_selected_mcp_servers_to_acp_session_configuration() {
        let stdio = mcp_server_from_config(
            "project-tools",
            &McpServerConfig {
                command: Some("/opt/project/mcp-server".into()),
                args: vec!["--stdio".into()],
                env: [("PROJECT_ROOT".into(), "/workspace".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap();
        let McpServer::Stdio(stdio) = stdio else {
            panic!("expected stdio MCP configuration");
        };
        assert_eq!(stdio.name, "project-tools");
        assert_eq!(stdio.command, PathBuf::from("/opt/project/mcp-server"));
        assert_eq!(stdio.args, ["--stdio"]);
        assert_eq!(stdio.env.len(), 1);

        let http = mcp_server_from_config(
            "metrics",
            &McpServerConfig {
                server_type: "http".into(),
                url: Some("https://mcp.example.test/rpc".into()),
                headers: [("Authorization".into(), "Bearer test".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap();
        let McpServer::Http(http) = http else {
            panic!("expected HTTP MCP configuration");
        };
        assert_eq!(http.name, "metrics");
        assert_eq!(http.url, "https://mcp.example.test/rpc");
        assert_eq!(http.headers.len(), 1);

        let error = mcp_server_from_config(
            "host-only",
            &McpServerConfig {
                command: Some("npx".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("absolute path inside the harness container"));
    }

    #[test]
    fn converts_acp_servers_to_pi_proxy_and_script_configuration() {
        let encoded = pi_mcp_config(&[
            McpServer::Stdio(
                McpServerStdio::new("xpressclaw", "/usr/local/bin/node")
                    .args(vec!["control.mjs".into()])
                    .env(vec![EnvVariable::new("XPRESSCLAW_TASK_ID", "task-123")]),
            ),
            McpServer::Http(
                McpServerHttp::new("github", "https://github.example.test/mcp")
                    .headers(vec![HttpHeader::new("Authorization", "Bearer secret")]),
            ),
            McpServer::Sse(McpServerSse::new(
                "project-search",
                "https://search.example.test/sse",
            )),
        ])
        .unwrap();
        let config: Value = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(
            config["mcpServers"]["xpressclaw"]["command"],
            "/usr/local/bin/node"
        );
        assert_eq!(
            config["mcpServers"]["xpressclaw"]["env"]["XPRESSCLAW_TASK_ID"],
            "task-123"
        );
        assert_eq!(
            config["mcpServers"]["github"]["httpTransport"],
            "streamable-http"
        );
        assert_eq!(
            config["mcpServers"]["github"]["headers"]["Authorization"],
            "Bearer secret"
        );
        assert_eq!(
            config["mcpServers"]["project-search"]["httpTransport"],
            "sse"
        );
        assert_eq!(config["mcpServers"]["xpressclaw"]["directTools"], true);
        assert_eq!(config["mcpServers"]["github"]["directTools"], true);
        assert!(config["mcpServers"]["project-search"]["directTools"].is_null());
    }

    #[test]
    fn pi_mcp_bridge_uses_private_unicode_safe_runtime_configuration() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut spec = ContainerSpec {
            environment: vec![
                "PI_ACP_PI_COMMAND=/custom/pi".into(),
                "XPRESSCLAW_PI_MCP_CONFIG=/custom/config.json".into(),
            ],
            ..Default::default()
        };
        let servers = [McpServer::Stdio(McpServerStdio::new(
            "xpressclaw",
            "/usr/local/bin/node",
        ))];

        let bridge =
            configure_pi_mcp_bridge(data_dir.path(), "エリ-pi", None, &servers, &mut spec).unwrap();
        let config_dir = pi_mcp_config_dir(data_dir.path(), "エリ-pi");
        let leaf = config_dir.file_name().unwrap().to_string_lossy();
        assert!(leaf.chars().all(|character| character.is_ascii_hexdigit()));
        assert!(!config_dir.display().to_string().contains("エリ"));
        assert!(bridge.signature.starts_with("pi-mcp:"));
        assert!(bridge.process_environment.is_empty());

        let mount = spec
            .volumes
            .iter()
            .find(|mount| mount.target == PI_MCP_CONFIG_DIR_TARGET)
            .unwrap();
        assert_eq!(mount.source, config_dir.display().to_string());
        assert!(mount.read_only);
        assert_eq!(mount.selinux_relabel, SelinuxRelabel::Shared);
        assert!(spec
            .environment
            .contains(&format!("PI_ACP_PI_COMMAND={PI_MCP_WRAPPER}")));
        assert!(spec
            .environment
            .contains(&format!("XPRESSCLAW_PI_MCP_CONFIG={PI_MCP_CONFIG_TARGET}")));
        assert_eq!(
            spec.environment
                .iter()
                .filter(|variable| variable.starts_with("PI_ACP_PI_COMMAND="))
                .count(),
            1
        );

        let config_path = config_dir.join("config.json");
        assert!(config_path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&config_dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&config_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        {
            assert_windows_owner_only_acl(&config_dir, true);
            assert_windows_owner_only_acl(&config_path, false);
        }
    }

    #[test]
    fn pi_conversation_processes_use_isolated_mcp_configuration_files() {
        let data_dir = tempfile::tempdir().unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (first, second) = std::thread::scope(|scope| {
            let launch = |conversation_id: &'static str, barrier: Arc<std::sync::Barrier>| {
                let data_dir = data_dir.path();
                scope.spawn(move || {
                    let mut spec = ContainerSpec::default();
                    let servers = [McpServer::Stdio(
                        McpServerStdio::new("xpressclaw", "/usr/local/bin/node").env(vec![
                            EnvVariable::new("XPRESSCLAW_CONVERSATION_ID", conversation_id),
                        ]),
                    )];
                    barrier.wait();
                    let bridge = configure_pi_mcp_bridge(
                        data_dir,
                        "エリ-pi",
                        Some(conversation_id),
                        &servers,
                        &mut spec,
                    )
                    .unwrap();
                    (conversation_id, bridge, spec)
                })
            };
            let first = launch("conversation-one", barrier.clone());
            let second = launch("conversation-two", barrier);
            (first.join().unwrap(), second.join().unwrap())
        });

        let config_dir = pi_mcp_config_dir(data_dir.path(), "エリ-pi");
        let config_for = |conversation_id: &str| {
            let scope_hash = format!("{:x}", Sha256::digest(conversation_id.as_bytes()));
            config_dir
                .join("processes")
                .join(scope_hash)
                .join("config.json")
        };
        for (conversation_id, bridge, spec) in [&first, &second] {
            let config: Value =
                serde_json::from_slice(&std::fs::read(config_for(conversation_id)).unwrap())
                    .unwrap();
            assert_eq!(
                config["mcpServers"]["xpressclaw"]["env"]["XPRESSCLAW_CONVERSATION_ID"],
                *conversation_id
            );
            assert_eq!(bridge.process_environment.len(), 1);
            assert!(bridge.process_environment[0].contains("/processes/"));
            assert!(spec
                .environment
                .contains(&format!("XPRESSCLAW_PI_MCP_CONFIG={PI_MCP_CONFIG_TARGET}")));
        }
        assert_ne!(first.1.process_environment, second.1.process_environment);
        assert_eq!(
            container_spec_fingerprint(&first.2).unwrap(),
            container_spec_fingerprint(&second.2).unwrap()
        );
        let bootstrap: Value =
            serde_json::from_slice(&std::fs::read(config_dir.join("config.json")).unwrap())
                .unwrap();
        assert_eq!(bootstrap["mcpServers"], json!({}));
    }

    #[cfg(windows)]
    fn assert_windows_owner_only_acl(path: &Path, directory: bool) {
        use std::mem::size_of;
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::{addr_of, null_mut};

        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::{
            AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
            ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE,
            DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        };
        use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

        let wide_path = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut owner: PSID = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `wide_path` is NUL-terminated and the output pointers are
        // valid. The returned descriptor owns `owner` and `dacl` and is kept
        // alive until all assertions finish.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        let _descriptor = WindowsLocalAllocation(descriptor);
        assert!(!owner.is_null());
        assert!(!dacl.is_null());

        let mut control = 0;
        let mut revision = 0;
        // SAFETY: `descriptor` remains valid through `_descriptor`.
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
            0
        );
        assert_ne!(control & SE_DACL_PROTECTED, 0);

        let mut information = ACL_SIZE_INFORMATION::default();
        // SAFETY: `dacl` remains valid through `_descriptor` and the output
        // buffer has the exact type and size requested.
        assert_ne!(
            unsafe {
                GetAclInformation(
                    dacl,
                    (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
                    size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            },
            0
        );
        assert_eq!(information.AceCount, 1);

        let mut raw_ace = null_mut();
        // SAFETY: The DACL has one ACE, as asserted above, and `raw_ace` is a
        // valid output pointer.
        assert_ne!(unsafe { GetAce(dacl, 0, &mut raw_ace) }, 0);
        // SAFETY: `SetEntriesInAclW` created this sole ACE from an
        // `EXPLICIT_ACCESS_W`, so it is an ACCESS_ALLOWED_ACE.
        let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
        assert_eq!(ace.Header.AceType, 0);
        assert_eq!(ace.Mask, FILE_ALL_ACCESS);
        assert_eq!(
            u32::from(ace.Header.AceFlags),
            if directory {
                OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
            } else {
                0
            }
        );
        // SAFETY: `SidStart` is the variable-length SID at the end of the ACE,
        // and both it and `owner` remain valid through `_descriptor`.
        let ace_sid = addr_of!(ace.SidStart).cast_mut().cast();
        assert_ne!(unsafe { EqualSid(owner, ace_sid) }, 0);
    }

    #[test]
    fn pi_mcp_bridge_rejects_mounts_overlapping_its_reserved_target() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut spec = ContainerSpec {
            volumes: vec![VolumeMount {
                source: "/tmp/custom".into(),
                target: "/run/xpressclaw".into(),
                read_only: false,
                selinux_relabel: SelinuxRelabel::None,
            }],
            ..Default::default()
        };

        let error =
            configure_pi_mcp_bridge(data_dir.path(), "pi", None, &[], &mut spec).unwrap_err();
        assert!(error
            .to_string()
            .contains("reserves container mount target"));
        assert!(!pi_mcp_config_dir(data_dir.path(), "pi").exists());
    }

    #[test]
    fn scopes_the_bundled_control_mcp_to_the_current_project() {
        let server = xpressclaw_control_mcp_server("dgx-codex", Some("task-123"), 9123, "docker");
        let McpServer::Stdio(server) = server else {
            panic!("expected stdio MCP configuration");
        };

        assert_eq!(server.name, "xpressclaw");
        assert_eq!(server.command, PathBuf::from(BUNDLED_CONTROL_MCP_COMMAND));
        assert_eq!(
            &server.args[..2],
            ["--input-type=module".to_string(), "--eval".to_string()]
        );
        assert!(server.args[2].contains("continuation_task_id"));
        assert!(server.args[2].ends_with("await main();\n"));
        assert!(server.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_URL"
                && variable.value == "http://host.docker.internal:9123"
        }));
        assert!(server.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_AGENT_ID" && variable.value == "dgx-codex"
        }));
        assert!(server.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_TASK_ID" && variable.value == "task-123"
        }));
        assert!(server.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_CONTROL_TOKEN" && variable.value == "test-control-token"
        }));
        assert!(server.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_REPOSITORY" && variable.value == "/workspace"
        }));

        let McpServer::Stdio(podman) =
            xpressclaw_control_mcp_server("dgx-codex", Some("task-123"), 9123, "podman")
        else {
            panic!("expected stdio MCP configuration");
        };
        assert!(podman.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_URL"
                && variable.value == "http://host.containers.internal:9123"
        }));

        let McpServer::Stdio(idle) =
            xpressclaw_control_mcp_server("dgx-codex", None, 9123, "docker")
        else {
            panic!("expected stdio MCP configuration");
        };
        assert!(!idle
            .env
            .iter()
            .any(|variable| variable.name == "XPRESSCLAW_TASK_ID"));
    }

    #[test]
    fn hidden_idle_tasks_do_not_bind_scheduled_wakeups() {
        use crate::tasks::board::CreateTask;

        let db = Arc::new(Database::open_memory().unwrap());
        crate::agents::registry::AgentRegistry::new(db.clone())
            .ensure("atlas", "native")
            .unwrap();
        let board = TaskBoard::new(db.clone());
        let visible = board
            .create(&CreateTask {
                title: "Visible work".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let idle = board
            .create_idle_task("atlas", "Look for proactive work")
            .unwrap();

        assert_eq!(continuation_task_id(&visible), Some(visible.id.as_str()));
        assert_eq!(continuation_task_id(&idle), None);
        assert!(dashboard_task_metrics_enabled(&visible));
        assert!(!dashboard_task_metrics_enabled(&idle));

        db.with_conn(|conn| conn.execute("UPDATE tasks SET hidden = 0 WHERE id = ?1", [&idle.id]))
            .unwrap();
        let visible_idle = board.get(&idle.id).unwrap();
        assert!(!visible_idle.hidden);
        assert_eq!(visible_idle.task_type, "IDLE");
        assert!(!dashboard_task_metrics_enabled(&visible_idle));
    }

    #[test]
    fn message_controls_override_workflow_and_harness_session_defaults() {
        use crate::sessions::NewEvent;
        use crate::tasks::board::CreateTask;

        let db = Arc::new(Database::open_memory().unwrap());
        add_test_agent(&db, "atlas");
        let sessions = SessionManager::new(db.clone());
        sessions.ensure("atlas", Some("Atlas")).unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Configurable turn".into(),
                agent_id: Some("atlas".into()),
                context: Some(json!({
                    "session_config": {
                        "mode": "plan",
                        "thought_level": "medium"
                    }
                })),
                ..Default::default()
            })
            .unwrap();
        sessions
            .append_event(
                "atlas",
                NewEvent {
                    attempt_id: None,
                    task_id: Some(&task.id),
                    source_type: "user",
                    source_id: None,
                    event_type: "task_message_received",
                    summary: "Change controls",
                    payload: json!({
                        "config_options": {
                            "mode": "build",
                            "approval_policy": true
                        }
                    }),
                },
            )
            .unwrap();

        let agent = AgentConfig {
            runner: NativeRunnerConfig {
                session_config: [
                    ("mode".into(), json!("default")),
                    ("model".into(), json!("fast")),
                    ("approval_policy".into(), json!(false)),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        let requested = requested_session_config(&db, &agent, &task.id).unwrap();
        assert_eq!(requested.get("model"), Some(&json!("fast")));
        assert_eq!(requested.get("thought_level"), Some(&json!("medium")));
        assert_eq!(requested.get("mode"), Some(&json!("build")));
        assert_eq!(requested.get("approval_policy"), Some(&json!(true)));
    }

    #[test]
    fn resolves_legacy_claude_backend_to_native_cli() {
        let agent = AgentConfig {
            backend: "claude-sdk".to_string(),
            ..Default::default()
        };
        assert_eq!(resolve_runner_kind(&agent).unwrap(), "claude");
    }

    #[test]
    fn does_not_confuse_copilot_with_pi() {
        let agent = AgentConfig {
            backend: "copilot-cli".to_string(),
            ..Default::default()
        };
        assert_eq!(resolve_runner_kind(&agent).unwrap(), "github-copilot");
    }

    #[test]
    fn resolves_deepseek_harness_aliases_to_the_builtin_runner() {
        for backend in ["deepseek-harness", "dsh", "dsh-acp"] {
            let agent = AgentConfig {
                backend: backend.to_string(),
                ..Default::default()
            };
            assert_eq!(resolve_runner_kind(&agent).unwrap(), "deepseek-harness");
        }

        let agent = AgentConfig {
            runner: NativeRunnerConfig {
                kind: "dsh".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(resolve_runner_kind(&agent).unwrap(), "deepseek-harness");
    }

    #[test]
    fn explicit_custom_kind_is_not_reclassified_as_a_builtin() {
        let agent = AgentConfig {
            backend: "codex".into(),
            runner: NativeRunnerConfig {
                kind: "codex-proxy".into(),
                command: vec!["codex-proxy".into(), "acp".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let kind = resolve_runner_kind(&agent).unwrap();
        assert_eq!(kind, "codex-proxy");
        assert!(agent_definition(&kind).is_none());
    }

    #[test]
    fn recognizes_explicit_native_questions() {
        assert!(needs_user_input(
            "I need one decision.\n\nNEEDS_USER_INPUT: Which database should I use?"
        ));
        assert!(needs_user_input(
            "**NEEDS_USER_INPUT: Which database should I use?**"
        ));
        assert!(needs_user_input(
            "I can continue once you decide.\n\nWhich database should I use?"
        ));
        assert!(needs_user_input(
            "The options are ready. Would you like the compact layout?"
        ));
        assert!(!needs_user_input("Implemented and tested. No blockers."));
        assert!(!needs_user_input("Implemented the requested FAQ page."));
    }

    #[test]
    fn expands_custom_command_placeholders() {
        let config = NativeRunnerConfig {
            command: vec!["runner".into(), "--cwd={workspace}".into()],
            ..Default::default()
        };
        assert_eq!(
            acp_command_for(&config, "custom", "/workspace").unwrap(),
            vec!["runner", "--cwd=/workspace"]
        );
    }

    #[test]
    fn wraps_acp_command_with_one_time_project_environment_startup() {
        let wrapped = with_startup_commands(
            vec!["qwen".into(), "--acp".into()],
            &["npm ci".into(), "docker compose up -d".into()],
        );
        assert_eq!(&wrapped[..2], ["/bin/sh", "-lc"]);
        assert!(wrapped[2].contains("/tmp/.xpressclaw-environment-"));
        assert!(wrapped[2].contains("  npm ci\n  docker compose up -d"));
        assert!(wrapped[2].contains("touch \"$marker\""));
        assert!(wrapped[2].ends_with("exec \"$@\""));
        assert_eq!(&wrapped[4..], ["qwen", "--acp"]);
    }

    #[test]
    fn starts_the_builtin_acp_servers() {
        let config = NativeRunnerConfig::default();
        assert_eq!(
            acp_command_for(&config, "codex", "/workspace").unwrap(),
            vec!["codex-acp"]
        );
        assert_eq!(
            acp_command_for(&config, "claude", "/workspace").unwrap(),
            vec!["claude-agent-acp"]
        );
        assert_eq!(
            acp_command_for(&config, "opencode", "/workspace").unwrap(),
            vec!["opencode", "acp"]
        );
        assert_eq!(
            acp_command_for(&config, "deepseek-harness", "/workspace").unwrap(),
            vec!["dsh-acp"]
        );
    }

    #[test]
    fn codex_defaults_to_full_access_inside_its_project_container() {
        let runner = NativeRunnerConfig::default();
        let mut environment = vec!["HOME=/home/node".to_string()];

        apply_codex_mode_default("codex", &runner, &mut environment);

        assert!(environment
            .iter()
            .any(|value| value == "INITIAL_AGENT_MODE=agent-full-access"));
    }

    #[test]
    fn explicit_codex_mode_overrides_the_container_default() {
        let runner = NativeRunnerConfig {
            environment: [("INITIAL_AGENT_MODE".into(), "agent".into())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let mut environment = vec!["HOME=/home/node".to_string()];

        apply_codex_mode_default("codex", &runner, &mut environment);
        environment.extend(
            runner
                .environment
                .iter()
                .map(|(name, value)| format!("{name}={value}")),
        );

        assert_eq!(
            environment
                .iter()
                .filter(|value| value.starts_with("INITIAL_AGENT_MODE="))
                .collect::<Vec<_>>(),
            ["INITIAL_AGENT_MODE=agent"]
        );
    }

    #[test]
    fn other_harnesses_do_not_receive_a_codex_mode() {
        let runner = NativeRunnerConfig::default();
        let mut environment = vec!["HOME=/home/node".to_string()];

        apply_codex_mode_default("claude", &runner, &mut environment);

        assert!(!environment
            .iter()
            .any(|value| value.starts_with("INITIAL_AGENT_MODE=")));
    }

    #[cfg(unix)]
    #[test]
    fn forwards_only_a_live_ssh_agent_and_non_secret_ssh_configuration() {
        let root = std::env::temp_dir().join(format!(
            "xpressclaw-ssh-forwarding-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ssh_dir = root.join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::create_dir_all(ssh_dir.join("config.d")).unwrap();
        std::fs::create_dir_all(ssh_dir.join("nested")).unwrap();
        std::fs::write(
            ssh_dir.join("config"),
            "Include \"~/.ssh/config.d/*\"\nHost work\n  HostName git.example\n",
        )
        .unwrap();
        std::fs::write(
            ssh_dir.join("config.d/20-work.conf"),
            "Include nested/*.conf\nHost included\n  HostName included.example\n",
        )
        .unwrap();
        std::fs::write(
            ssh_dir.join("nested/30-extra.conf"),
            "Host nested\n  HostName nested.example\n",
        )
        .unwrap();
        std::fs::write(
            ssh_dir.join("config.d/90-secret.conf"),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nnever mount this\n",
        )
        .unwrap();
        std::fs::write(
            ssh_dir.join("config.d/80-putty.ppk"),
            "PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\nComment: secret\nPublic-Lines: 1\npublic\nPrivate-Lines: 1\nprivate\nPrivate-MAC: secret\n",
        )
        .unwrap();
        std::fs::write(
            ssh_dir.join("config.d/85-ssh2.key"),
            "---- BEGIN SSH2 ENCRYPTED PRIVATE KEY ----\nnever mount this\n---- END SSH2 ENCRYPTED PRIVATE KEY ----\n",
        )
        .unwrap();
        std::fs::write(ssh_dir.join("known_hosts"), "git.example ssh-ed25519 key\n").unwrap();
        std::fs::write(ssh_dir.join("id_ed25519"), "never mount this").unwrap();
        let socket = root.join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        let access = discover_host_ssh_agent_from(vec![socket.clone()], Some(&root)).unwrap();
        assert_eq!(access.socket, socket);
        assert!(!access.generation.is_empty());
        let materialized_config = std::str::from_utf8(access.config.as_deref().unwrap()).unwrap();
        assert!(materialized_config.contains("Host included"));
        assert!(materialized_config.contains("Host nested"));
        assert!(!materialized_config.contains("Include"));
        assert!(!materialized_config.contains("PRIVATE KEY"));
        assert!(!materialized_config.contains("PuTTY-User-Key-File"));
        assert!(!materialized_config.contains("Private-MAC"));
        assert!(
            materialized_config.find("Host nested").unwrap()
                < materialized_config.find("Host included").unwrap()
        );
        assert!(
            materialized_config.find("Host included").unwrap()
                < materialized_config.find("Host work").unwrap()
        );
        let retained_known_hosts =
            prepare_retained_ssh_known_hosts(&root.join("data"), "エリ-codex").unwrap();
        let forwarded_config = prepare_forwarded_ssh_config(
            &root.join("data"),
            "エリ-codex",
            access.config.as_deref().unwrap(),
        )
        .unwrap();

        let mut volumes = Vec::new();
        let mut environment = Vec::new();
        apply_ssh_agent_forwarding(
            &access,
            &socket,
            &retained_known_hosts,
            Some(&forwarded_config),
            &mut volumes,
            &mut environment,
        );

        assert!(volumes.iter().any(|mount| {
            mount.source == socket.display().to_string()
                && mount.target == SSH_AGENT_SOCKET_TARGET
                && !mount.read_only
                && mount.selinux_relabel == SelinuxRelabel::Shared
        }));
        assert!(volumes.iter().any(|mount| {
            mount.source == forwarded_config.display().to_string()
                && mount.target == SSH_CONFIG_TARGET
                && mount.read_only
        }));
        assert!(volumes
            .iter()
            .any(|mount| mount.target == SSH_KNOWN_HOSTS_TARGET && mount.read_only));
        assert!(volumes.iter().any(|mount| {
            mount.source == retained_known_hosts.display().to_string()
                && mount.target == SSH_RUNTIME_DIR_TARGET
                && !mount.read_only
                && mount.selinux_relabel == SelinuxRelabel::Shared
        }));
        assert!(!volumes
            .iter()
            .any(|mount| mount.source.ends_with("id_ed25519")
                || mount.source.ends_with("80-putty.ppk")
                || mount.source.ends_with("85-ssh2.key")
                || mount.source.ends_with("90-secret.conf")));
        assert!(environment
            .iter()
            .any(|entry| entry == &format!("SSH_AUTH_SOCK={SSH_AGENT_SOCKET_TARGET}")));
        let git_ssh = environment
            .iter()
            .find(|entry| entry.starts_with("GIT_SSH_COMMAND="))
            .unwrap();
        assert!(git_ssh.contains(SSH_AGENT_SOCKET_TARGET));
        assert!(git_ssh.contains(SSH_CONFIG_TARGET));
        assert!(git_ssh.contains(SSH_KNOWN_HOSTS_TARGET));
        assert!(git_ssh.contains(SSH_RETAINED_KNOWN_HOSTS));
        assert!(git_ssh.contains("StrictHostKeyChecking=accept-new"));

        let retained_file = retained_known_hosts.join("known_hosts");
        std::fs::write(&retained_file, "learned.example ssh-ed25519 learned\n").unwrap();
        let after_recreation =
            prepare_retained_ssh_known_hosts(&root.join("data"), "エリ-codex").unwrap();
        assert_eq!(after_recreation, retained_known_hosts);
        assert_eq!(
            std::fs::read_to_string(after_recreation.join("known_hosts")).unwrap(),
            "learned.example ssh-ed25519 learned\n"
        );
        let leaf = retained_known_hosts.file_name().unwrap().to_string_lossy();
        assert!(leaf.chars().all(|character| character.is_ascii_hexdigit()));
        assert!(!retained_known_hosts.display().to_string().contains("エリ"));
        assert_ne!(
            retained_known_hosts,
            retained_ssh_runtime_dir(&root.join("data"), "another-agent")
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&retained_known_hosts)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&retained_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read(&forwarded_config).unwrap(),
            access.config.as_deref().unwrap()
        );
        assert_eq!(
            std::fs::metadata(&forwarded_config)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(!forwarded_config.display().to_string().contains("エリ"));

        let first_generation = access.generation;
        let replacement_include = ssh_dir.join("config.d/20-work.next");
        std::fs::write(
            &replacement_include,
            "Include nested/*.conf\nHost included\n  HostName replacement.example\n",
        )
        .unwrap();
        std::fs::rename(replacement_include, ssh_dir.join("config.d/20-work.conf")).unwrap();
        let replaced_include =
            discover_host_ssh_agent_from(vec![socket.clone()], Some(&root)).unwrap();
        assert_ne!(replaced_include.generation, first_generation);

        let replacement_config = ssh_dir.join("config.next");
        std::fs::write(
            &replacement_config,
            "Include ~/.ssh/config.d/*\nHost work\n  HostName replacement.example\n",
        )
        .unwrap();
        std::fs::rename(replacement_config, ssh_dir.join("config")).unwrap();
        let replaced_config =
            discover_host_ssh_agent_from(vec![socket.clone()], Some(&root)).unwrap();
        assert_ne!(replaced_config.generation, replaced_include.generation);

        let replacement_known_hosts = ssh_dir.join("known_hosts.next");
        std::fs::write(
            &replacement_known_hosts,
            "replacement.example ssh-ed25519 key\n",
        )
        .unwrap();
        std::fs::rename(replacement_known_hosts, ssh_dir.join("known_hosts")).unwrap();
        let replaced_known_hosts =
            discover_host_ssh_agent_from(vec![socket.clone()], Some(&root)).unwrap();
        assert_ne!(replaced_known_hosts.generation, replaced_config.generation);

        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn docker_desktop_on_macos_uses_its_ssh_agent_bridge() {
        let host_socket = Path::new("/private/tmp/com.apple.launchd.example/Listeners");

        assert_eq!(
            ssh_agent_mount_source(host_socket, true, true),
            PathBuf::from(DOCKER_DESKTOP_SSH_AGENT_SOURCE)
        );
        assert_eq!(
            ssh_agent_mount_source(host_socket, false, true),
            host_socket
        );
        assert_eq!(
            ssh_agent_mount_source(host_socket, true, false),
            host_socket
        );
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_ssh_configuration_symlinks() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "xpressclaw-ssh-symlink-test-{}",
            std::process::id()
        ));
        let ssh_dir = root.join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        let private_key = ssh_dir.join("id_ed25519");
        std::fs::write(&private_key, "never mount this").unwrap();
        symlink(&private_key, ssh_dir.join("config")).unwrap();

        assert_eq!(regular_ssh_file(&root, "config"), None);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_non_socket_ssh_agent_candidate() {
        let candidate =
            std::env::temp_dir().join(format!("xpressclaw-not-an-agent-{}", std::process::id()));
        std::fs::write(&candidate, "not a socket").unwrap();
        assert!(discover_host_ssh_agent_from(vec![candidate.clone()], None).is_none());
        std::fs::remove_file(candidate).unwrap();
    }

    #[test]
    fn codex_runner_gets_github_guidance_only_when_bundled_mcp_is_attached() {
        let runner = NativeRunnerConfig::default();
        let mut environment = vec!["HOME=/home/node".to_string()];

        assert!(configure_bundled_github_mcp(&runner, "codex", true, &mut environment).unwrap());
        let config: Value = serde_json::from_str(
            environment
                .iter()
                .find_map(|variable| variable.strip_prefix("CODEX_CONFIG="))
                .unwrap(),
        )
        .unwrap();
        assert!(config["developer_instructions"]
            .as_str()
            .unwrap()
            .contains("GitHub runtime"));

        let mut custom_image_environment = vec![
            "HOME=/home/node".to_string(),
            r#"CODEX_CONFIG={"developer_instructions":"Use the image's shell gh."}"#.to_string(),
        ];
        let original_custom_environment = custom_image_environment.clone();
        assert!(!configure_bundled_github_mcp(
            &runner,
            "codex",
            false,
            &mut custom_image_environment
        )
        .unwrap());
        assert_eq!(custom_image_environment, original_custom_environment);

        let mut bootstrap_environment = Vec::new();
        assert!(
            configure_bundled_github_mcp(&runner, "codex", true, &mut bootstrap_environment)
                .unwrap()
        );

        let configured_runner = NativeRunnerConfig {
            mcp_servers: vec!["github".to_string()],
            ..Default::default()
        };
        let mut configured_environment = Vec::new();
        assert!(!configure_bundled_github_mcp(
            &configured_runner,
            "codex",
            true,
            &mut configured_environment
        )
        .unwrap());
    }

    #[test]
    fn codex_github_guidance_keeps_the_shared_container_lane_neutral() {
        let runner = NativeRunnerConfig::default();
        let mut task = ContainerSpec::default();
        let mut conversation = ContainerSpec::default();

        assert!(
            configure_bundled_github_mcp(&runner, "codex", true, &mut task.environment,).unwrap()
        );
        assert!(configure_bundled_github_mcp(
            &runner,
            "codex",
            true,
            &mut conversation.environment,
        )
        .unwrap());

        assert_eq!(task.environment, conversation.environment);
        assert_eq!(
            container_spec_fingerprint(&task).unwrap(),
            container_spec_fingerprint(&conversation).unwrap()
        );
    }

    #[test]
    fn local_collaboration_network_and_capability_are_assigned_per_agent() {
        let db = Arc::new(Database::open_memory().unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.system.data_dir = data_dir.path().to_path_buf();
        config.collaboration.enabled = true;
        config.collaboration.authorized_agents = vec!["allowed".to_string()];
        let expected_token = CollaborationSecrets::generate();
        expected_token.save(data_dir.path()).unwrap();

        let allowed = AgentConfig {
            name: "allowed".to_string(),
            ..Default::default()
        };
        let expected_network = collaboration_network_name(&db.installation_id().unwrap());
        let mut allowed_spec = ContainerSpec::default();
        let token = configure_local_collaboration_access_for_network(
            &config,
            &allowed,
            &mut allowed_spec,
            Some(&expected_network),
        )
        .unwrap();
        assert_eq!(
            token.as_deref(),
            Some(
                expected_token
                    .capability_token_for_agent("allowed")
                    .as_str()
            )
        );
        assert_ne!(
            token.as_deref(),
            Some(
                expected_token
                    .capability_token_for_agent("another-agent")
                    .as_str()
            )
        );
        assert_eq!(
            allowed_spec.network_mode.as_deref(),
            Some(expected_network.as_str())
        );

        let mut missing_network_spec = ContainerSpec {
            network_mode: Some("ordinary-network".to_string()),
            ..Default::default()
        };
        assert!(configure_local_collaboration_access_for_network(
            &config,
            &allowed,
            &mut missing_network_spec,
            None,
        )
        .unwrap()
        .is_none());
        assert_eq!(
            missing_network_spec.network_mode.as_deref(),
            Some("ordinary-network")
        );

        let unassigned = AgentConfig {
            name: "unassigned".to_string(),
            ..Default::default()
        };
        let mut unassigned_spec = ContainerSpec {
            network_mode: Some("existing-network".to_string()),
            ..Default::default()
        };
        assert!(configure_local_collaboration_access_for_network(
            &config,
            &unassigned,
            &mut unassigned_spec,
            Some(&expected_network),
        )
        .unwrap()
        .is_none());
        assert_eq!(
            unassigned_spec.network_mode.as_deref(),
            Some("existing-network")
        );

        std::fs::remove_file(CollaborationSecrets::path(data_dir.path())).unwrap();
        let mut ordinary_turn_spec = ContainerSpec {
            network_mode: Some("ordinary-network".to_string()),
            ..Default::default()
        };
        assert!(configure_local_collaboration_access_for_network(
            &config,
            &allowed,
            &mut ordinary_turn_spec,
            Some(&expected_network),
        )
        .unwrap()
        .is_none());
        assert_eq!(
            ordinary_turn_spec.network_mode.as_deref(),
            Some("ordinary-network")
        );

        std::fs::create_dir_all(data_dir.path().join("collaboration")).unwrap();
        std::fs::write(
            CollaborationSecrets::path(data_dir.path()),
            "not valid collaboration credentials",
        )
        .unwrap();
        let mut malformed_credentials_spec = ContainerSpec::default();
        let original_network = malformed_credentials_spec.network_mode.clone();
        assert!(configure_local_collaboration_access_for_network(
            &config,
            &allowed,
            &mut malformed_credentials_spec,
            Some(&expected_network),
        )
        .unwrap()
        .is_none());
        assert_eq!(malformed_credentials_spec.network_mode, original_network);
    }

    #[test]
    fn reset_collaboration_access_keeps_task_and_conversation_specs_usable() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.system.data_dir = data_dir.path().to_path_buf();
        config.collaboration.enabled = false;
        config.collaboration.authorized_agents.clear();
        let agent = AgentConfig {
            name: "formerly-assigned".to_string(),
            ..Default::default()
        };

        for network in ["task-network", "conversation-network"] {
            let mut spec = ContainerSpec {
                network_mode: Some(network.to_string()),
                ..Default::default()
            };
            assert!(configure_local_collaboration_access_for_network(
                &config,
                &agent,
                &mut spec,
                Some("collaboration-network"),
            )
            .unwrap()
            .is_none());
            assert_eq!(spec.network_mode.as_deref(), Some(network));
        }
    }

    #[test]
    fn startup_interrupt_preserves_original_messages_and_images() {
        use crate::tasks::attachments::DecodedImageAttachment;
        use crate::tasks::board::CreateTask;

        let db = Arc::new(Database::open_memory().unwrap());
        add_test_agent(&db, "atlas");
        let board = TaskBoard::new(db.clone());
        SessionManager::new(db.clone())
            .ensure("atlas", Some("atlas"))
            .unwrap();
        let task = board
            .create(&CreateTask {
                title: "Inspect the screenshot".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        TaskConversation::new(db.clone())
            .add_message_with_attachments(
                &task.id,
                "user",
                "Original request",
                &[DecodedImageAttachment {
                    name: "screen.png".into(),
                    mime_type: "image/png".into(),
                    data: b"original image".to_vec(),
                }],
            )
            .unwrap();
        let queue = TaskQueue::new(db.clone());
        queue.enqueue(&task.id, "atlas").unwrap();
        let first = queue.claim("atlas").unwrap().unwrap();
        let first_attempt_id = first.attempt_id.as_deref().unwrap();
        let first_prompt = build_prompt(&db, &first, first_attempt_id).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET prompt = ?1 WHERE id = ?2",
                rusqlite::params![first_prompt.content, first_attempt_id],
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(
                first_attempt_id,
                "interrupted",
                "Stopped during startup",
                None,
                None,
            )
            .unwrap();
        queue.complete(first.id, "interrupted").unwrap();
        TaskConversation::new(db.clone())
            .add_message(&task.id, "user", "New guidance")
            .unwrap();
        let continuation = queue
            .enqueue_continuation(&task.id, "atlas")
            .unwrap()
            .unwrap();
        let continuation_attempt_id = continuation.attempt_id.as_deref().unwrap();

        let mut prompt = build_prompt(&db, &continuation, continuation_attempt_id).unwrap();
        prepend_unresumed_interrupted_prompt(
            &db,
            &continuation,
            continuation_attempt_id,
            &mut prompt,
        )
        .unwrap();

        assert_eq!(prompt.content, "Original request\n\nNew guidance");
        assert_eq!(prompt.attachments.len(), 1);
        assert_eq!(prompt.attachments[0].name, "screen.png");
        assert_eq!(prompt.attachments[0].data, b"original image");
    }

    #[test]
    fn task_prompts_explain_plan_and_durable_delegation_semantics() {
        let mut prompt = "Implement the requested change".to_string();
        append_plan_lifecycle_guidance(&mut prompt);
        append_plan_lifecycle_guidance(&mut prompt);

        assert!(prompt.contains("current-turn checklists"));
        assert!(prompt.contains("do not leave speculative review"));
        assert!(prompt.contains("Use create_task with this task as parent"));
        assert_eq!(prompt.matches(PLAN_LIFECYCLE_GUIDANCE).count(), 1);
    }

    #[test]
    fn selects_fork_resume_and_fresh_conversation_contexts() {
        use crate::tasks::board::CreateTask;

        let db = Arc::new(Database::open_memory().unwrap());
        add_test_agent(&db, "atlas");
        let board = TaskBoard::new(db.clone());
        SessionManager::new(db.clone())
            .ensure("atlas", Some("atlas"))
            .unwrap();
        let first = board
            .create(&CreateTask {
                title: "First turn".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let queue = TaskQueue::new(db.clone());
        let first_item = queue.enqueue(&first.id, "atlas").unwrap();
        let first_attempt = first_item.attempt_id.as_deref().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET runner = 'codex', status = 'completed',
                    completed_at = '2026-01-01 00:00:01' WHERE id = ?1",
                [first_attempt],
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            .set_native_session(first_attempt, "thread-1")
            .unwrap();

        let failed = board
            .create(&CreateTask {
                title: "Failed setup".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let failed_item = queue.enqueue(&failed.id, "atlas").unwrap();
        let failed_attempt = failed_item.attempt_id.as_deref().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET runner = 'codex' WHERE id = ?1",
                [failed_attempt],
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            .set_native_session(failed_attempt, "failed-thread")
            .unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(
                failed_attempt,
                "failed",
                "Setup failed",
                None,
                Some("bad config"),
            )
            .unwrap();

        let regular = board
            .create(&CreateTask {
                title: "Continue project".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let regular_item = queue.enqueue(&regular.id, "atlas").unwrap();
        assert_eq!(
            session_start(&db, &regular_item, "codex").unwrap(),
            AcpSessionStart::Fork("thread-1".into())
        );

        let fresh = board
            .create(&CreateTask {
                title: "Start clean".into(),
                agent_id: Some("atlas".into()),
                context: Some(json!({ "session_mode": "new" })),
                ..Default::default()
            })
            .unwrap();
        let fresh_item = queue.enqueue(&fresh.id, "atlas").unwrap();
        assert_eq!(
            session_start(&db, &fresh_item, "codex").unwrap(),
            AcpSessionStart::New
        );

        let dependent = board
            .create(&CreateTask {
                title: "Continue prerequisite".into(),
                agent_id: Some("atlas".into()),
                context: Some(json!({ "session_mode": "new" })),
                ..Default::default()
            })
            .unwrap();
        board.add_dependency(&dependent.id, &first.id).unwrap();
        let dependent_item = queue.enqueue(&dependent.id, "atlas").unwrap();
        assert_eq!(
            session_start(&db, &dependent_item, "codex").unwrap(),
            AcpSessionStart::Fork("thread-1".into())
        );

        let regular_attempt = regular_item.attempt_id.as_deref().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET runner = 'codex', status = 'completed',
                    completed_at = '2026-01-01 00:00:01' WHERE id = ?1",
                [regular_attempt],
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            // Legacy attempts can share a mutable ACP session ID. Attempt
            // identity still distinguishes the current task from an old one.
            .set_native_session(regular_attempt, "thread-1")
            .unwrap();

        let active_follow_up = queue.enqueue(&regular.id, "atlas").unwrap();
        assert_eq!(
            session_start(&db, &active_follow_up, "codex").unwrap(),
            AcpSessionStart::Resume("thread-1".into())
        );

        let old_task_follow_up = queue.enqueue(&first.id, "atlas").unwrap();
        assert_eq!(
            session_start(&db, &old_task_follow_up, "codex").unwrap(),
            AcpSessionStart::Fork("thread-1".into())
        );
    }

    #[test]
    fn scheduled_wakeup_resumes_the_armed_tasks_codex_conversation() {
        use crate::tasks::board::CreateTask;
        use crate::tasks::scheduler::{CreateOneShotSchedule, ScheduleManager};

        let db = Arc::new(Database::open_memory().unwrap());
        crate::agents::registry::AgentRegistry::new(db.clone())
            .ensure("dgx-codex", "codex")
            .unwrap();
        let board = TaskBoard::new(db.clone());
        let sessions = SessionManager::new(db.clone());
        sessions.ensure("dgx-codex", Some("DGX")).unwrap();
        let original = board
            .create(&CreateTask {
                title: "Run the DGX experiment".into(),
                agent_id: Some("dgx-codex".into()),
                ..Default::default()
            })
            .unwrap();
        let queue = TaskQueue::new(db.clone());
        let original_item = queue.enqueue(&original.id, "dgx-codex").unwrap();
        let original_attempt = original_item.attempt_id.as_deref().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET runner = 'codex' WHERE id = ?1",
                [original_attempt],
            )
        })
        .unwrap();
        sessions
            .set_native_session(original_attempt, "codex-thread-1")
            .unwrap();
        sessions
            .transition_attempt(
                original_attempt,
                "completed",
                "Waiting",
                Some("Waiting"),
                None,
            )
            .unwrap();
        queue.complete(original_item.id, "Waiting").unwrap();

        let schedules = ScheduleManager::new(db.clone());
        let wakeup = schedules
            .create_one_shot(&CreateOneShotSchedule {
                name: "Check DGX".into(),
                run_at: None,
                delay_seconds: Some(5 * 60 * 60),
                agent_id: "dgx-codex".into(),
                title: "Resume the DGX experiment".into(),
                description: Some("Inspect the results and continue the active goal.".into()),
                continuation_task_id: Some(original.id.clone()),
                conversation_id: None,
            })
            .unwrap();
        let wakeup_task = schedules
            .trigger(&wakeup.id, &board)
            .unwrap()
            .into_task()
            .unwrap();
        let wakeup_item = queue
            .list(Some("dgx-codex"), Some("queued"), 10)
            .unwrap()
            .into_iter()
            .find(|item| item.task_id == wakeup_task.id)
            .unwrap();

        assert_eq!(wakeup_task.id, original.id);
        assert_eq!(
            session_start(&db, &wakeup_item, "codex").unwrap(),
            AcpSessionStart::Resume("codex-thread-1".into())
        );
        let messages = TaskConversation::new(db)
            .get_messages(&original.id)
            .unwrap();
        assert!(messages.iter().any(|message| {
            message
                .content
                .contains("Inspect the results and continue the active goal.")
        }));
    }

    #[test]
    fn selects_a_minimal_image_for_each_native_runner() {
        assert_eq!(
            default_native_runner_image("codex", ContainerEngineAccess::None),
            Some("ghcr.io/xpressai/xpressclaw-runner-codex:latest")
        );
        assert_eq!(
            default_native_runner_image("claude", ContainerEngineAccess::None),
            Some("ghcr.io/xpressai/xpressclaw-runner-claude:latest")
        );
        assert_eq!(
            default_native_runner_image("opencode", ContainerEngineAccess::None),
            Some("ghcr.io/xpressai/xpressclaw-runner-opencode:latest")
        );
        assert_eq!(
            default_native_runner_image("deepseek-harness", ContainerEngineAccess::None),
            Some("ghcr.io/xpressai/xpressclaw-runner-deepseek-harness:latest")
        );
        assert_eq!(
            default_native_runner_image("custom", ContainerEngineAccess::None),
            None
        );
    }

    #[test]
    fn host_engine_mode_selects_a_docker_cli_image_and_same_path_workspace() {
        let config = NativeRunnerConfig {
            kind: "codex".into(),
            image: "ghcr.io/xpressai/xpressclaw-runner-codex:latest".into(),
            container_engine: ContainerEngineAccess::Host,
            ..Default::default()
        };
        assert_eq!(
            resolved_runner_image(&config, "codex").unwrap(),
            "ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest"
        );
        let workspace = Path::new("/home/me/project");
        assert_eq!(
            container_workspace_path(workspace, ContainerEngineAccess::Host),
            if cfg!(unix) {
                "/home/me/project"
            } else {
                "/workspace"
            }
        );
    }

    #[test]
    fn host_engine_mode_migrates_a_local_minimal_alias() {
        let config = NativeRunnerConfig {
            kind: "codex".into(),
            image: "xpressclaw-runner-codex:latest".into(),
            container_engine: ContainerEngineAccess::Host,
            ..Default::default()
        };
        assert_eq!(
            resolved_runner_image(&config, "codex").unwrap(),
            "ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest"
        );
    }

    #[test]
    fn parses_read_only_and_read_write_volume_mounts() {
        let read_only = parse_volume("/tmp/reference:/workspace/reference:ro").unwrap();
        assert_eq!(read_only.source, "/tmp/reference");
        assert_eq!(read_only.target, "/workspace/reference");
        assert!(read_only.read_only);

        let read_write = parse_volume("/tmp/project:/workspace/project").unwrap();
        assert_eq!(read_write.source, "/tmp/project");
        assert_eq!(read_write.target, "/workspace/project");
        assert!(!read_write.read_only);

        let selinux_shared = parse_volume("/tmp/shared:/workspace/shared:ro,z").unwrap();
        assert!(selinux_shared.read_only);
        assert_eq!(selinux_shared.selinux_relabel, SelinuxRelabel::Shared);

        let selinux_private = parse_volume("/tmp/private:/workspace/private:Z").unwrap();
        assert!(!selinux_private.read_only);
        assert_eq!(selinux_private.selinux_relabel, SelinuxRelabel::Private);

        assert!(parse_volume("/tmp/cache:/workspace/cache:cached").is_none());
    }

    #[test]
    fn visualization_roots_include_only_explicit_writable_agent_mounts() {
        let host = tempfile::tempdir().unwrap();
        let project = host.path().join("project");
        let reference = host.path().join("reference");
        let output = host.path().join("output");
        let relative_target = host.path().join("relative-target");
        let agent = AgentConfig {
            volumes: vec![
                format!("{}:/workspace/reference:ro", reference.display()),
                format!("{}:/workspace/output:rw", output.display()),
                format!("{}:relative-target:rw", relative_target.display()),
                "relative-host:/workspace/relative-host:rw".into(),
            ],
            ..Default::default()
        };
        assert_eq!(
            visualization_source_roots(&project, "/workspace", &agent),
            vec![
                VisualizationSourceRoot::new("/workspace", project),
                VisualizationSourceRoot::new("/workspace/output", output),
            ]
        );
    }

    #[test]
    fn published_images_keep_the_prototype_local_aliases() {
        assert_eq!(
            local_runner_image_alias("ghcr.io/xpressai/xpressclaw-runner-codex:latest"),
            Some("xpressclaw-runner-codex:latest")
        );
        assert_eq!(
            local_runner_image_alias("ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest"),
            Some("xpressclaw-runner-codex-docker:latest")
        );
        assert_eq!(local_runner_image_alias("example/custom:latest"), None);
    }

    #[test]
    fn cancellation_is_not_overwritten_by_container_exit_error() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tasks (id, title, agent_id) VALUES ('task-1', 'Cancel me', 'atlas')",
                [],
            )
            .unwrap();
        });
        let queue = TaskQueue::new(db.clone());
        let item = queue.enqueue("task-1", "atlas").unwrap();
        let attempt_id = item.attempt_id.as_deref().unwrap();
        let sessions = SessionManager::new(db.clone());
        sessions
            .transition_attempt(attempt_id, "cancelled", "Cancelled", None, None)
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_queue SET status = 'failed', harness_response = 'cancelled by user' WHERE id = ?1",
                [item.id],
            )?;
            conn.execute("UPDATE tasks SET status = 'cancelled' WHERE id = 'task-1'", [])?;
            Ok::<_, Error>(())
        })
        .unwrap();

        fail_item(
            &db,
            &item,
            "container disappeared",
            &Arc::new(ConversationEventBus::new()),
        )
        .unwrap();

        assert_eq!(
            sessions.get_attempt(attempt_id).unwrap().status,
            "cancelled"
        );
        assert_eq!(
            TaskBoard::new(db.clone())
                .get("task-1")
                .unwrap()
                .status
                .as_str(),
            "cancelled"
        );
        assert_eq!(
            queue.get(item.id).unwrap().harness_response.as_deref(),
            Some("cancelled by user")
        );
    }

    #[test]
    fn native_results_return_to_conversation_history() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = crate::agents::registry::AgentRegistry::new(db.clone());
        registry.ensure("atlas", "generic").unwrap();
        registry.ensure("reviewer", "generic").unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET project_id = 'atlas' WHERE id = 'reviewer'",
                [],
            )
        })
        .unwrap();
        let conversations = ConversationManager::new(db.clone());
        let conversation = conversations
            .create(&crate::conversations::CreateConversation {
                title: Some("Native session".to_string()),
                icon: None,
                participant_ids: vec!["atlas".to_string(), "reviewer".to_string()],
            })
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&crate::tasks::board::CreateTask {
                title: "Reply".to_string(),
                description: Some("Reply from the native worker".to_string()),
                agent_id: Some("atlas".to_string()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: Some(conversation.id.clone()),
                priority: None,
                context: None,
            })
            .unwrap();
        let item = TaskQueue::new(db.clone())
            .enqueue(&task.id, "atlas")
            .unwrap();
        let published_file = crate::message_artifacts::PublishedFileAttachment {
            name: format!("{}.pptx", "📊".repeat(64)),
            mime_type: "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                .into(),
            data: b"pptx bytes".to_vec(),
        };

        publish_conversation_result(
            &db,
            &Arc::new(ConversationEventBus::new()),
            &item,
            "Atlas",
            "@[AGENT:reviewer:Reviewer] Native result",
            "attempt",
            ConversationResultArtifacts {
                visualizations: &[],
                published_files: &[published_file],
            },
        );

        let messages = conversations
            .get_messages(&conversation.id, 10, None)
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender_type, "agent");
        assert_eq!(messages[0].message_type, "task_result");
        assert_eq!(
            messages[0].linked_task_id.as_deref(),
            Some(task.id.as_str())
        );
        assert_eq!(
            messages[0].content,
            "@[AGENT:reviewer:Reviewer] Native result"
        );
        let attachments = conversations.attachments(messages[0].id).unwrap();
        assert_eq!(attachments.len(), 1);
        assert!(attachments[0].name.len() <= 255);
        assert!(attachments[0].name.ends_with(".pptx"));
        assert_eq!(
            attachments[0].source_task_id.as_deref(),
            Some(task.id.as_str())
        );
        let (_, data) = conversations
            .attachment_data(&conversation.id, &attachments[0].id)
            .unwrap();
        assert_eq!(data, b"pptx bytes");
        let turns = ConversationTurnQueue::new(db)
            .list_for_conversation(&conversation.id, 10)
            .unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].agent_id, "reviewer");
    }
}
