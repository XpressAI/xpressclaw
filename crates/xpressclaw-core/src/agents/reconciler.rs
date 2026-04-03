//! Desired-state reconciler for agent lifecycle (ADR-018).
//!
//! Runs every 10 seconds. Compares desired state (DB) to observed state
//! (Docker) and takes the minimum action to converge: pulling images,
//! starting containers, stopping containers, re-queuing orphaned tasks.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::{debug, error, info, warn};

use crate::agents::registry::AgentRegistry;
use crate::config::Config;
use crate::db::Database;
use crate::docker::images::build_container_spec;
use crate::docker::manager::{DockerManager, VolumeMount};
use crate::tasks::board::TaskBoard;
use crate::tasks::queue::TaskQueue;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(10);
const STABLE_THRESHOLD_SECS: u64 = 300; // 5 minutes

/// Start the reconciliation loop as a background task.
/// Takes the config behind a RwLock so it always sees the latest
/// config after user changes (add/remove agents, update API keys, etc.)
pub async fn start(db: Arc<Database>, config: Arc<RwLock<Arc<Config>>>, server_port: u16) {
    info!(
        "starting desired-state reconciler ({}s interval)",
        RECONCILE_INTERVAL.as_secs()
    );
    loop {
        let config_snapshot = config.read().unwrap().clone();
        if let Err(e) = reconcile_once(&db, &config_snapshot, server_port).await {
            warn!(error = %e, "reconciliation cycle failed");
        }
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

async fn reconcile_once(
    db: &Arc<Database>,
    config: &Config,
    server_port: u16,
) -> crate::error::Result<()> {
    // Connect to Docker — if unavailable, skip this cycle
    let docker = match DockerManager::connect().await {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "Docker not available, skipping reconciliation");
            return Ok(());
        }
    };

    reconcile_models(config).await;
    reconcile_images(config, &docker).await;
    reconcile_agents(db, config, &docker, server_port).await;
    reconcile_apps(db, &docker).await;
    reconcile_tasks(db, &docker).await;
    reconcile_idle_tasks(db, config);

    Ok(())
}

/// Ensure Ollama models are pulled for agents using the local provider.
async fn reconcile_models(config: &Config) {
    // Only relevant when using Ollama (local provider without a GGUF path)
    if config.llm.local_model_path.is_some() {
        return;
    }
    if config.llm.default_provider != "local" {
        return;
    }

    let base_url = config
        .llm
        .local_base_url
        .as_deref()
        .unwrap_or("http://localhost:11434");

    // Collect unique model names across agents + global config
    let mut models = std::collections::HashSet::new();
    if let Some(ref m) = config.llm.local_model {
        models.insert(m.clone());
    }
    for agent in &config.agents {
        let uses_local = agent
            .llm
            .as_ref()
            .and_then(|l| l.provider.as_deref())
            .unwrap_or(&config.llm.default_provider)
            == "local";
        if uses_local {
            if let Some(ref m) = agent.model {
                models.insert(m.clone());
            }
        }
    }

    for model in &models {
        if !crate::llm::local::ollama_has_model(base_url, model).await {
            info!(model, "pulling Ollama model");
            if let Err(e) = crate::llm::local::ollama_pull(base_url, model).await {
                warn!(model, error = %e, "failed to pull Ollama model");
            } else {
                info!(model, "Ollama model pull complete");
            }
        }
    }
}

/// Pull images for all configured backends.
async fn reconcile_images(config: &Config, docker: &DockerManager) {
    let mut seen = std::collections::HashSet::new();
    for agent in &config.agents {
        let image = crate::docker::images::image_for_backend(&agent.backend);
        if !seen.insert(image.to_string()) {
            continue; // Already handled this image
        }
        if let Err(e) = docker.pull_image(image).await {
            // Non-fatal: use local image if available
            if docker.has_image(image).await {
                debug!(image, error = %e, "pull failed, local image available");
            } else {
                warn!(image, error = %e, "pull failed and no local image");
            }
        }
    }
}

/// Reconcile agent containers against desired state.
async fn reconcile_agents(
    db: &Arc<Database>,
    config: &Config,
    docker: &DockerManager,
    server_port: u16,
) {
    let registry = AgentRegistry::new(db.clone());

    for agent_config in &config.agents {
        let agent = match registry.get(&agent_config.name) {
            Ok(a) => a,
            Err(_) => continue, // Not in DB yet (setup hasn't run)
        };

        debug!(
            agent = agent.id,
            desired = agent.desired_status,
            restart_count = agent.restart_count,
            "reconciling agent"
        );

        let container_name = format!("xpressclaw-{}", agent.id);
        let is_running = docker.is_container_running(&container_name).await;

        match (agent.desired_status.as_str(), is_running) {
            // Desired running, is running → check stability
            ("running", true) => {
                if agent.restart_count > 0 {
                    let uptime = docker.container_uptime_secs(&container_name).await;
                    if uptime >= STABLE_THRESHOLD_SECS {
                        let _ = registry.reset_restart_count(&agent.id);
                        debug!(
                            agent = agent.id,
                            uptime, "agent stable, reset restart count"
                        );
                    }
                }
            }

            // Desired running, not running → start (with backoff)
            ("running", false) => {
                if !should_attempt(agent.restart_count, agent.last_attempt_at.as_deref()) {
                    continue; // Backoff not elapsed, check next agent
                }

                info!(
                    agent = agent.id,
                    restart_count = agent.restart_count,
                    "starting agent"
                );

                // Build container spec
                let mut spec = build_container_spec(
                    agent_config,
                    server_port,
                    config.llm.anthropic_api_key.as_deref(),
                    config.llm.openai_api_key.as_deref(),
                    config.llm.openai_base_url.as_deref(),
                );

                // Mount workspace if not already mounted by build_container_spec
                let has_workspace = spec.volumes.iter().any(|v| v.target == "/workspace");
                if !has_workspace {
                    let workspace = config.system.workspace_dir.display().to_string();
                    spec.volumes.push(VolumeMount {
                        source: workspace,
                        target: "/workspace".to_string(),
                        read_only: false,
                    });
                }

                // Mount documents directory
                let docs_dir = config.system.data_dir.join(&agent.id).join("documents");
                let _ = std::fs::create_dir_all(&docs_dir);
                if !spec
                    .volumes
                    .iter()
                    .any(|v| v.target == "/workspace/Documents")
                {
                    spec.volumes.push(VolumeMount {
                        source: docs_dir.display().to_string(),
                        target: "/workspace/Documents".to_string(),
                        read_only: false,
                    });
                }
                if !spec
                    .environment
                    .iter()
                    .any(|e| e.starts_with("DOCUMENTS_DIR="))
                {
                    spec.environment
                        .push("DOCUMENTS_DIR=/workspace/Documents".to_string());
                }

                match docker.launch(&agent.id, &spec).await {
                    Ok(info) => {
                        info!(
                            agent = agent.id,
                            container_id = %info.container_id,
                            "agent container started"
                        );
                        let _ = registry.record_attempt(&agent.id, None);
                        // Also update old status column for backward compat
                        let _ = registry.update_status(
                            &agent.id,
                            &crate::agents::state::AgentStatus::Running,
                            Some(&info.container_id),
                        );
                    }
                    Err(e) => {
                        error!(agent = agent.id, error = %e, "failed to start agent");
                        let _ = registry.record_attempt(&agent.id, Some(&e.to_string()));
                        let _ = registry.update_status(
                            &agent.id,
                            &crate::agents::state::AgentStatus::Error(e.to_string()),
                            None,
                        );
                    }
                }
            }

            // Desired stopped, is running → stop
            ("stopped", true) => {
                info!(agent = agent.id, "stopping agent (desired=stopped)");
                let _ = docker.stop(&agent.id).await;
                let _ = registry.update_status(
                    &agent.id,
                    &crate::agents::state::AgentStatus::Stopped,
                    None,
                );
            }

            // Desired stopped, not running → converged
            ("stopped", false) => {}

            _ => {}
        }
    }
}

struct AppRow {
    id: String,
    agent_id: String,
    start_command: Option<String>,
    image: Option<String>,
    port: i64,
}

/// Reconcile published app containers — restart any that should be running but aren't.
async fn reconcile_apps(db: &Arc<Database>, docker: &DockerManager) {
    let apps: Vec<AppRow> = {
        let conn = db.conn();
        let mut stmt = match conn.prepare(
            "SELECT id, agent_id, start_command, image, port FROM apps
             WHERE status IN ('running', 'starting') AND start_command IS NOT NULL",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        stmt.query_map([], |row| {
            Ok(AppRow {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                start_command: row.get(2)?,
                image: row.get(3)?,
                port: row.get(4)?,
            })
        })
        .unwrap_or_else(|_| panic!("query apps"))
        .filter_map(|r| r.ok())
        .collect()
    };

    for app in apps {
        let app_id = &app.id;
        let agent_id = &app.agent_id;
        let container_name = format!("app-{app_id}");
        if docker.is_container_running(&container_name).await {
            continue; // Already running
        }

        let Some(cmd) = app.start_command else {
            continue;
        };
        let image = app.image.unwrap_or_else(|| "node:20-alpine".to_string());
        let app_port = app.port as u16;

        info!(app_id, "restarting app container");

        let volume_name = format!("xpressclaw-workspace-{agent_id}");
        let spec = crate::docker::manager::ContainerSpec {
            image,
            memory_limit: Some(512 * 1024 * 1024),
            cpu_limit: None,
            environment: vec![format!("APP_ID={app_id}"), format!("PORT={app_port}")],
            volumes: vec![VolumeMount {
                source: volume_name,
                target: "/workspace".to_string(),
                read_only: true,
            }],
            network_mode: Some("bridge".to_string()),
            expose_port: Some(app_port),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), cmd]),
            working_dir: Some(format!("/workspace/apps/{app_id}")),
        };

        match docker.launch(&container_name, &spec).await {
            Ok(info) => {
                let conn = db.conn();
                let _ = conn.execute(
                    "UPDATE apps SET container_id = ?1, status = 'running', updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
                    rusqlite::params![info.container_id, app_id],
                );
                info!(
                    app_id,
                    container_id = &info.container_id[..12],
                    "app restarted"
                );
            }
            Err(e) => {
                warn!(app_id, error = %e, "failed to restart app");
                let conn = db.conn();
                let _ = conn.execute(
                    "UPDATE apps SET status = 'error', updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                    [&app_id],
                );
            }
        }
    }
}

/// Re-queue tasks stuck in_progress whose agent isn't running.
async fn reconcile_tasks(db: &Arc<Database>, docker: &DockerManager) {
    let board = TaskBoard::new(db.clone());
    let queue = TaskQueue::new(db.clone());

    let tasks = match board.list_all(Some("in_progress"), None, 100) {
        Ok(t) => t,
        Err(_) => return,
    };

    let registry = AgentRegistry::new(db.clone());
    for task in tasks {
        if let Some(ref agent_id) = task.agent_id {
            let container_name = format!("xpressclaw-{agent_id}");
            if !docker.is_container_running(&container_name).await {
                // Only re-queue if the agent is supposed to be running.
                // If desired=stopped, move task to pending without re-queuing
                // to the same agent (it won't come back).
                let _ = board.update_status(&task.id, "pending", None);
                let agent_desired_running = registry
                    .get(agent_id)
                    .map(|a| a.desired_status == "running")
                    .unwrap_or(false);
                if agent_desired_running {
                    let _ = queue.enqueue(&task.id, agent_id);
                    info!(task_id = task.id, agent_id, "re-queued orphaned task");
                } else {
                    info!(
                        task_id = task.id,
                        agent_id, "unassigned orphaned task (agent stopped)"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Idle tasks (XCLAW-47)
// ---------------------------------------------------------------------------

/// Create idle tasks for agents that have an `idle_prompt` configured and
/// currently have no work. Follows the desired-state pattern: if an agent
/// should have an idle task but doesn't, create one.
fn reconcile_idle_tasks(db: &Arc<Database>, config: &Config) {
    let registry = AgentRegistry::new(db.clone());
    let board = TaskBoard::new(db.clone());
    let queue = TaskQueue::new(db.clone());

    for agent_cfg in &config.agents {
        let idle_prompt = match agent_cfg.idle_prompt.as_deref() {
            Some(p) if !p.is_empty() => p,
            _ => continue,
        };

        // Agent must be running
        let agent = match registry.get(&agent_cfg.name) {
            Ok(a) if a.desired_status == "running" && a.status == "running" => a,
            _ => continue,
        };

        // Skip if the agent already has an IDLE task queued or in progress
        let has_idle_task = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM tasks
                     WHERE agent_id = ?1 AND task_type = 'IDLE'
                       AND status IN ('pending', 'in_progress')",
                    [&agent_cfg.name],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| crate::error::Error::Database(e.to_string()))
            })
            .unwrap_or(0)
            > 0;
        if has_idle_task {
            continue;
        }

        // Skip if agent has real (non-IDLE) work pending or in progress
        let has_real_work = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM tasks
                     WHERE agent_id = ?1 AND task_type != 'IDLE'
                       AND status IN ('pending', 'in_progress')",
                    [&agent_cfg.name],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| crate::error::Error::Database(e.to_string()))
            })
            .unwrap_or(0)
            > 0;
        if has_real_work {
            continue;
        }

        // Skip if a conversation involving this agent is currently processing
        let conv_busy = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM conversations c
                     JOIN conversation_participants cp ON cp.conversation_id = c.id
                     WHERE cp.participant_id = ?1
                       AND cp.participant_type = 'agent'
                       AND c.processing_status = 'processing'",
                    [&agent_cfg.name],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| crate::error::Error::Database(e.to_string()))
            })
            .unwrap_or(0)
            > 0;
        if conv_busy {
            continue;
        }

        // Check backoff
        if !idle_backoff_elapsed(agent.idle_count, agent.last_idle_check.as_deref()) {
            continue;
        }

        // Build the idle prompt with scratch pad contents
        let description = build_idle_prompt(idle_prompt, &agent_cfg.name, &config.system.data_dir);

        // Seed scratch pad on first use
        seed_scratch_pad(&agent_cfg.name, &config.system.data_dir);

        // Create the hidden idle task
        match board.create_idle_task(&agent_cfg.name, &description) {
            Ok(task) => {
                if let Err(e) = queue.enqueue(&task.id, &agent_cfg.name) {
                    warn!(
                        agent = agent_cfg.name,
                        error = %e,
                        "failed to enqueue idle task"
                    );
                }
                // Update idle tracking columns
                let now = chrono::Utc::now()
                    .naive_utc()
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string();
                let _ = db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE agents SET idle_count = idle_count + 1, last_idle_check = ?1 WHERE id = ?2",
                        rusqlite::params![now, agent_cfg.name],
                    )
                    .map_err(|e| crate::error::Error::Database(e.to_string()))
                });
                info!(
                    agent = agent_cfg.name,
                    idle_count = agent.idle_count + 1,
                    task_id = task.id,
                    "created idle task"
                );
            }
            Err(e) => {
                warn!(
                    agent = agent_cfg.name,
                    error = %e,
                    "failed to create idle task"
                );
            }
        }
    }

    // Safety net: clean up stuck IDLE tasks (>30 min in in_progress)
    cleanup_stuck_idle_tasks(db);
}

/// Build the idle prompt with current time and scratch pad contents.
fn build_idle_prompt(idle_prompt: &str, agent_id: &str, data_dir: &std::path::Path) -> String {
    let now = chrono::Local::now();
    let mut prompt = format!(
        "Current time: {} ({})\n\n",
        now.format("%Y-%m-%d %H:%M:%S"),
        now.format("%Z")
    );

    prompt.push_str(idle_prompt);
    prompt.push_str("\n\n");

    // Embed scratch pad contents to save a tool-call round-trip
    let scratch_path = data_dir.join(agent_id).join("idle.md");
    prompt.push_str(&format!(
        "Your persistent scratch pad is at {} — update it as needed.\n",
        scratch_path.display()
    ));
    if let Ok(contents) = std::fs::read_to_string(&scratch_path) {
        if !contents.trim().is_empty() {
            prompt.push_str("\n--- scratch pad contents ---\n");
            prompt.push_str(&contents);
            prompt.push_str("\n--- end scratch pad ---\n");
        }
    }

    prompt
}

/// Write a default scratch pad if one doesn't exist yet.
fn seed_scratch_pad(agent_id: &str, data_dir: &std::path::Path) {
    let dir = data_dir.join(agent_id);
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("idle.md");
    if !path.exists() {
        let template = "# Idle Scratch Pad\n\n\
            Notes and observations maintained across idle cycles.\n\n\
            ## Reminders\n\
            - (Add items here)\n\n\
            ## Agent Notes\n\
            (Update this section with observations, pending items, and context between sessions.)\n";
        let _ = std::fs::write(&path, template);
    }
}

/// Delete IDLE tasks stuck in in_progress for >30 minutes.
fn cleanup_stuck_idle_tasks(db: &Arc<Database>) {
    let cutoff = (chrono::Utc::now() - chrono::Duration::minutes(30))
        .naive_utc()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let deleted = db
        .with_conn(|conn| {
            conn.execute(
                "DELETE FROM tasks
                 WHERE task_type = 'IDLE' AND status = 'in_progress'
                   AND updated_at < ?1",
                [&cutoff],
            )
            .map_err(|e| crate::error::Error::Database(e.to_string()))
        })
        .unwrap_or(0);
    if deleted > 0 {
        warn!(count = deleted, "cleaned up stuck idle tasks");
    }
}

/// Idle-task exponential backoff. Returns true if enough time has elapsed
/// since the last idle check for the given idle_count.
///
/// Schedule (matching XpressAI platform):
/// - idle_count=0 → immediate
/// - idle_count=1 → 30 min
/// - idle_count=2 → 2 hours
/// - idle_count=3 → 6 hours
/// - idle_count>=4 → 12 hours
fn idle_backoff_elapsed(idle_count: i32, last_idle_check: Option<&str>) -> bool {
    if idle_count == 0 {
        return true;
    }

    let required_secs: u64 = match idle_count {
        1 => 30 * 60,      // 30 min
        2 => 2 * 60 * 60,  // 2 hours
        3 => 6 * 60 * 60,  // 6 hours
        _ => 12 * 60 * 60, // 12 hours
    };

    let Some(last) = last_idle_check else {
        return true;
    };

    let Ok(last_time) = chrono::NaiveDateTime::parse_from_str(last, "%Y-%m-%d %H:%M:%S") else {
        return true;
    };

    let elapsed = chrono::Utc::now()
        .naive_utc()
        .signed_duration_since(last_time)
        .num_seconds()
        .max(0) as u64;

    elapsed >= required_secs
}

/// Exponential backoff: should we attempt to start this agent now?
fn should_attempt(restart_count: i32, last_attempt_at: Option<&str>) -> bool {
    if restart_count == 0 {
        return true;
    }

    let backoff_secs = std::cmp::min(
        10u64.saturating_mul(2u64.saturating_pow(restart_count as u32)),
        300, // cap at 5 minutes
    );

    let Some(last) = last_attempt_at else {
        return true;
    };

    let Ok(last_time) = chrono::NaiveDateTime::parse_from_str(last, "%Y-%m-%d %H:%M:%S") else {
        // Can't parse — attempt anyway
        return true;
    };

    let elapsed = chrono::Utc::now()
        .naive_utc()
        .signed_duration_since(last_time)
        .num_seconds()
        .max(0) as u64;

    elapsed >= backoff_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idle_backoff_immediate_at_zero() {
        assert!(idle_backoff_elapsed(0, None));
        assert!(idle_backoff_elapsed(0, Some("2020-01-01 00:00:00")));
    }

    #[test]
    fn test_idle_backoff_no_last_check() {
        // No last_idle_check means we should fire regardless of count
        assert!(idle_backoff_elapsed(1, None));
        assert!(idle_backoff_elapsed(4, None));
    }

    #[test]
    fn test_idle_backoff_not_elapsed() {
        // Just now — backoff should prevent firing for idle_count >= 1
        let now = chrono::Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert!(!idle_backoff_elapsed(1, Some(&now))); // needs 30min
        assert!(!idle_backoff_elapsed(2, Some(&now))); // needs 2h
        assert!(!idle_backoff_elapsed(3, Some(&now))); // needs 6h
        assert!(!idle_backoff_elapsed(4, Some(&now))); // needs 12h
    }

    #[test]
    fn test_idle_backoff_elapsed() {
        // 2 hours ago — sufficient for idle_count 1 (30min) and 2 (2h)
        let two_hours_ago =
            (chrono::Utc::now() - chrono::Duration::hours(2) - chrono::Duration::seconds(1))
                .naive_utc()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
        assert!(idle_backoff_elapsed(1, Some(&two_hours_ago)));
        assert!(idle_backoff_elapsed(2, Some(&two_hours_ago)));
        assert!(!idle_backoff_elapsed(3, Some(&two_hours_ago))); // needs 6h
    }

    #[test]
    fn test_idle_backoff_bad_timestamp() {
        // Unparseable timestamp → allow
        assert!(idle_backoff_elapsed(3, Some("not-a-timestamp")));
    }

    #[test]
    fn test_build_idle_prompt_basic() {
        let prompt = build_idle_prompt(
            "Check your tasks.",
            "atlas",
            std::path::Path::new("/tmp/test-data"),
        );
        assert!(prompt.contains("Check your tasks."));
        assert!(prompt.contains("Current time:"));
        assert!(prompt.contains("scratch pad"));
    }

    #[test]
    fn test_build_idle_prompt_with_scratch_pad() {
        let dir = std::env::temp_dir().join("xpressclaw-test-idle");
        let agent_dir = dir.join("test-agent");
        let _ = std::fs::create_dir_all(&agent_dir);
        std::fs::write(agent_dir.join("idle.md"), "# My Notes\nImportant stuff").unwrap();

        let prompt = build_idle_prompt("Check tasks.", "test-agent", &dir);
        assert!(prompt.contains("Important stuff"));
        assert!(prompt.contains("scratch pad contents"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_seed_scratch_pad_creates_file() {
        let dir = std::env::temp_dir().join("xpressclaw-test-seed");
        let _ = std::fs::remove_dir_all(&dir);

        seed_scratch_pad("test-agent", &dir);

        let path = dir.join("test-agent").join("idle.md");
        assert!(path.exists());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("Idle Scratch Pad"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_seed_scratch_pad_does_not_overwrite() {
        let dir = std::env::temp_dir().join("xpressclaw-test-seed-no-overwrite");
        let agent_dir = dir.join("test-agent");
        let _ = std::fs::create_dir_all(&agent_dir);
        std::fs::write(agent_dir.join("idle.md"), "custom content").unwrap();

        seed_scratch_pad("test-agent", &dir);

        let contents = std::fs::read_to_string(agent_dir.join("idle.md")).unwrap();
        assert_eq!(contents, "custom content");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_create_idle_task() {
        let db = Arc::new(crate::db::Database::open_memory().unwrap());
        let board = TaskBoard::new(db.clone());

        let task = board
            .create_idle_task("atlas", "Check your workspace")
            .unwrap();

        assert_eq!(task.task_type, "IDLE");
        assert!(task.hidden);
        assert_eq!(task.title, "[Idle] atlas");
        assert_eq!(task.agent_id.as_deref(), Some("atlas"));
        assert_eq!(task.status, crate::tasks::board::TaskStatus::Pending);
    }

    #[test]
    fn test_idle_tasks_hidden_from_default_list() {
        let db = Arc::new(crate::db::Database::open_memory().unwrap());
        let board = TaskBoard::new(db.clone());

        // Create a normal task and an idle task
        board
            .create(&crate::tasks::board::CreateTask {
                title: "Normal task".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        board.create_idle_task("atlas", "idle check").unwrap();

        // Default list should only show the normal task
        let visible = board.list(None, None, 100).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].title, "Normal task");

        // list_all should show both
        let all = board.list_all(None, None, 100).unwrap();
        assert_eq!(all.len(), 2);
    }
}
