use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::Value;
use uuid::Uuid;
use zerocopy::IntoBytes;

use crate::config::{
    AgentConfig, AgentLlmConfig, BudgetConfig, Config, OnExceeded, RateLimitConfig, WakeOnConfig,
};
use crate::db::{task_search_key, Database};
use crate::error::{Error, Result};
use crate::memory::vector::simple_embedding;

use super::manifest::ProjectSyncManifest;
use super::model::{
    validate_dependency_cycles, validate_parent_cycles, PortableAgent, PortableAgentSettings,
    PortableBudgetSettings, PortableConversation, PortableConversationMessage, PortableLlmSettings,
    PortableMemoryLink, PortableMemoryNote, PortableParticipant, PortableProject,
    PortableRateLimitSettings, PortableRunnerSettings, PortableSnapshot, PortableTask,
    PortableTaskDependency, PortableTaskMessage, PortableWakeOnSettings, PortableWorkflow,
    StoreDescriptor, STORE_VERSION,
};

#[derive(Debug, Clone)]
pub(super) struct SyncState {
    pub(super) local_snapshot_hash: String,
    pub(super) remote_snapshot_hash: String,
}

pub(super) fn load_sync_state(
    db: &Database,
    manifest: &ProjectSyncManifest,
) -> Result<Option<SyncState>> {
    db.with_conn(|connection| {
        connection
            .query_row(
                "SELECT local_snapshot_hash, remote_snapshot_hash
                 FROM project_sync_state
                 WHERE project_id = ?1 AND remote = ?2 AND branch = ?3 AND store_path = ?4",
                params![
                    manifest.project_id,
                    manifest.store.remote,
                    manifest.store.branch,
                    manifest.store.path
                ],
                |row| {
                    Ok(SyncState {
                        local_snapshot_hash: row.get(0)?,
                        remote_snapshot_hash: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    })
}

pub(super) fn save_sync_state(
    db: &Database,
    manifest: &ProjectSyncManifest,
    commit: &str,
    local_digest: &str,
    remote_digest: &str,
) -> Result<()> {
    db.with_conn(|connection| {
        connection.execute(
            "INSERT INTO project_sync_state
                (project_id, remote, branch, store_path, last_commit,
                 local_snapshot_hash, remote_snapshot_hash, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP)
             ON CONFLICT(project_id, remote, branch, store_path) DO UPDATE SET
                last_commit = excluded.last_commit,
                local_snapshot_hash = excluded.local_snapshot_hash,
                remote_snapshot_hash = excluded.remote_snapshot_hash,
                updated_at = CURRENT_TIMESTAMP",
            params![
                manifest.project_id,
                manifest.store.remote,
                manifest.store.branch,
                manifest.store.path,
                commit,
                local_digest,
                remote_digest
            ],
        )?;
        Ok(())
    })
}

pub(super) fn project_exists(db: &Database, project_id: &str) -> Result<bool> {
    db.with_conn(|connection| {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
                [project_id],
                |row| row.get(0),
            )
            .map_err(Error::from)
    })
}

pub(super) fn project_has_portable_data(db: &Database, project_id: &str) -> Result<bool> {
    db.with_conn(|connection| {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)
                     OR EXISTS(SELECT 1 FROM agents WHERE project_id = ?1)
                     OR EXISTS(SELECT 1 FROM tasks WHERE project_id = ?1)
                     OR EXISTS(SELECT 1 FROM conversations WHERE project_id = ?1)
                     OR EXISTS(SELECT 1 FROM project_memory_notes WHERE project_id = ?1)",
                [project_id],
                |row| row.get(0),
            )
            .map_err(Error::from)
    })
}

pub(super) fn ensure_quiescent(db: &Database, project_id: &str) -> Result<()> {
    let active = db.with_conn(|connection| project_is_active(connection, project_id))?;
    if active {
        return Err(quiescent_error());
    }
    Ok(())
}

fn project_is_active(connection: &Connection, project_id: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT
                EXISTS(
                    SELECT 1 FROM work_attempts attempt
                    JOIN tasks task ON task.id = attempt.task_id
                    WHERE task.project_id = ?1
                      AND attempt.status IN ('queued', 'preparing', 'running', 'waiting_for_input', 'review')
                )
                OR EXISTS(
                    SELECT 1 FROM conversation_turns turn
                    JOIN conversations conversation ON conversation.id = turn.conversation_id
                    WHERE conversation.project_id = ?1
                      AND turn.status IN ('queued', 'running')
                )
                OR EXISTS(
                    SELECT 1 FROM workflow_instances instance
                    WHERE instance.project_id = ?1
                      AND instance.status IN ('running', 'waiting')
                )",
            [project_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(Error::from)
}

fn quiescent_error() -> Error {
    Error::Sync(
        "Project synchronization requires a quiescent Project; stop the server or wait for active tasks, Conversations, and workflows"
            .into(),
    )
}

pub(super) fn export_snapshot(
    db: &Database,
    config: &Config,
    manifest: &ProjectSyncManifest,
) -> Result<PortableSnapshot> {
    db.with_conn(|connection| {
        let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
        let snapshot = export_transaction(&transaction, config, manifest)?;
        snapshot.validate_for_sync(&manifest.project_id)?;
        transaction.commit()?;
        Ok(snapshot)
    })
}

fn export_transaction(
    connection: &Connection,
    config: &Config,
    manifest: &ProjectSyncManifest,
) -> Result<PortableSnapshot> {
    if project_is_active(connection, &manifest.project_id)? {
        return Err(quiescent_error());
    }
    let project = connection
        .query_row(
            "SELECT id, name, description, icon, created_at, updated_at
             FROM projects WHERE id = ?1",
            [&manifest.project_id],
            |row| {
                Ok(PortableProject {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    icon: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            Error::Sync(format!(
                "Project '{}' does not exist locally",
                manifest.project_id
            ))
        })?;

    let configured_agents = config
        .agents
        .iter()
        .map(|agent| (agent.name.as_str(), agent))
        .collect::<HashMap<_, _>>();
    let agents = query_records(
        connection,
        "SELECT id, name, backend, created_at FROM agents
         WHERE project_id = ?1 ORDER BY id",
        [&manifest.project_id],
        |row| {
            let id: String = row.get(0)?;
            let configured = configured_agents.get(id.as_str()).ok_or_else(|| {
                Error::Sync(format!(
                    "Agent '{id}' has no matching local xpressclaw.yaml configuration"
                ))
            })?;
            Ok(PortableAgent {
                settings: portable_agent_settings(configured),
                id,
                name: row.get(1)?,
                backend: row.get(2)?,
                created_at: row.get(3)?,
            })
        },
    )?;

    let mut tasks = query_records(
        connection,
        "SELECT id, title, description, status, priority, task_type, hidden,
                agent_id, parent_task_id, conversation_id, created_at, updated_at,
                completed_at
         FROM tasks
         WHERE project_id = ?1 AND hidden = 0 AND UPPER(task_type) <> 'IDLE'
         ORDER BY id",
        [&manifest.project_id],
        |row| {
            Ok(PortableTask {
                id: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                status: row.get(3)?,
                priority: row.get(4)?,
                task_type: row.get(5)?,
                hidden: row.get(6)?,
                agent_id: row.get(7)?,
                parent_task_id: row.get(8)?,
                conversation_id: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
                completed_at: row.get(12)?,
            })
        },
    )?;
    let portable_agent_ids = agents
        .iter()
        .map(|agent| agent.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    for task in &mut tasks {
        if task
            .agent_id
            .as_deref()
            .is_some_and(|agent_id| !portable_agent_ids.contains(agent_id))
        {
            task.agent_id = None;
        }
    }
    let portable_task_ids = tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<std::collections::HashSet<_>>();
    for task in &mut tasks {
        if task
            .parent_task_id
            .as_deref()
            .is_some_and(|parent_id| !portable_task_ids.contains(parent_id))
        {
            task.parent_task_id = None;
        }
    }
    let task_dependencies = query_records(
        connection,
        "SELECT dependency.task_id, dependency.depends_on_id
         FROM task_dependencies dependency
         JOIN tasks task ON task.id = dependency.task_id
         JOIN tasks prerequisite ON prerequisite.id = dependency.depends_on_id
         WHERE task.project_id = ?1 AND prerequisite.project_id = ?1
           AND task.hidden = 0 AND prerequisite.hidden = 0
           AND UPPER(task.task_type) <> 'IDLE'
           AND UPPER(prerequisite.task_type) <> 'IDLE'
         ORDER BY dependency.task_id, dependency.depends_on_id",
        [&manifest.project_id],
        |row| {
            Ok(PortableTaskDependency {
                task_id: row.get(0)?,
                depends_on_id: row.get(1)?,
            })
        },
    )?;

    let conversations = query_records(
        connection,
        "SELECT id, title, icon, created_at, updated_at, last_message_at
         FROM conversations WHERE project_id = ?1 ORDER BY id",
        [&manifest.project_id],
        |row| {
            Ok(PortableConversation {
                id: row.get(0)?,
                title: row.get(1)?,
                icon: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                last_message_at: row.get(5)?,
            })
        },
    )?;
    let participants = query_records(
        connection,
        "SELECT participant.conversation_id, participant.participant_type,
                participant.participant_id, participant.joined_at
         FROM conversation_participants participant
         JOIN conversations conversation ON conversation.id = participant.conversation_id
         WHERE conversation.project_id = ?1
         ORDER BY participant.conversation_id, participant.participant_type, participant.participant_id",
        [&manifest.project_id],
        |row| {
            Ok(PortableParticipant {
                conversation_id: row.get(0)?,
                participant_type: row.get(1)?,
                participant_id: row.get(2)?,
                joined_at: row.get(3)?,
            })
        },
    )?;

    let task_messages = export_task_messages(connection, &manifest.project_id)?;
    let mut conversation_messages = export_conversation_messages(connection, &manifest.project_id)?;
    for message in &mut conversation_messages {
        if message
            .linked_task_id
            .as_deref()
            .is_some_and(|task_id| !portable_task_ids.contains(task_id))
        {
            message.linked_task_id = None;
        }
    }
    let workflows = query_records(
        connection,
        "SELECT workflow.id, workflow.name, workflow.description, workflow.yaml_content,
                workflow.version, workflow.created_at, workflow.updated_at
         FROM workflows workflow
         JOIN project_workflows project_workflow ON project_workflow.workflow_id = workflow.id
         WHERE project_workflow.project_id = ?1 ORDER BY workflow.id",
        [&manifest.project_id],
        |row| {
            Ok(PortableWorkflow {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                yaml_content: row.get(3)?,
                version: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        },
    )?;
    let (mut memory_notes, memory_links) = if manifest.share.project_memory {
        export_memory(connection, &manifest.project_id)?
    } else {
        (Vec::new(), Vec::new())
    };
    for note in &mut memory_notes {
        if note
            .source_task_id
            .as_deref()
            .is_some_and(|task_id| !portable_task_ids.contains(task_id))
        {
            note.source_task_id = None;
        }
    }

    Ok(PortableSnapshot {
        descriptor: StoreDescriptor {
            version: STORE_VERSION,
            project_id: manifest.project_id.clone(),
        },
        project,
        agents,
        tasks,
        task_dependencies,
        task_messages,
        conversations,
        participants,
        conversation_messages,
        workflows,
        memory_notes,
        memory_links,
    })
}

fn query_records<T, P, F>(
    connection: &Connection,
    sql: &str,
    parameters: P,
    mut map: F,
) -> Result<Vec<T>>
where
    P: rusqlite::Params,
    F: FnMut(&rusqlite::Row<'_>) -> Result<T>,
{
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query(parameters)?;
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        records.push(map(row)?);
    }
    Ok(records)
}

fn parse_json(value: String, label: &str) -> Result<Value> {
    serde_json::from_str(&value)
        .map_err(|error| Error::Sync(format!("local {label} is invalid JSON: {error}")))
}

fn portable_agent_settings(agent: &AgentConfig) -> PortableAgentSettings {
    PortableAgentSettings {
        llm: agent.llm.as_ref().and_then(|llm| {
            if llm.provider.is_none() && llm.model.is_none() {
                None
            } else {
                Some(PortableLlmSettings {
                    provider: llm.provider.clone(),
                    model: llm.model.clone(),
                })
            }
        }),
        runner: PortableRunnerSettings {
            kind: agent.runner.kind.clone(),
            image: agent.runner.image.clone(),
            project_name: agent.runner.project_name.clone(),
            model: agent.runner.model.clone(),
            session_config: agent
                .runner
                .session_config
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
            mcp_servers: agent.runner.mcp_servers.clone(),
            startup_commands: agent.runner.startup_commands.clone(),
            command: agent.runner.command.clone(),
        },
        tools: agent.tools.clone(),
        skills: agent.skills.clone(),
        budget: agent.budget.as_ref().map(|budget| PortableBudgetSettings {
            daily: budget.daily.clone(),
            monthly: budget.monthly.clone(),
            per_task: budget.per_task.clone(),
            on_exceeded: match budget.on_exceeded {
                OnExceeded::Pause => "pause",
                OnExceeded::Alert => "alert",
                OnExceeded::Degrade => "degrade",
                OnExceeded::Stop => "stop",
            }
            .into(),
            fallback_model: budget.fallback_model.clone(),
            warn_at_percent: budget.warn_at_percent,
        }),
        rate_limit: agent
            .rate_limit
            .as_ref()
            .map(|rate| PortableRateLimitSettings {
                requests_per_minute: rate.requests_per_minute,
                tokens_per_minute: rate.tokens_per_minute,
                concurrent_requests: rate.concurrent_requests,
            }),
        wake_on: agent
            .wake_on
            .iter()
            .map(|wake| PortableWakeOnSettings {
                schedule: wake.schedule.clone(),
                event: wake.event.clone(),
                condition: wake.condition.clone(),
            })
            .collect(),
        idle_prompt: agent.idle_prompt.clone(),
    }
}

fn export_task_messages(
    connection: &Connection,
    project_id: &str,
) -> Result<Vec<PortableTaskMessage>> {
    struct LocalMessage {
        id: i64,
        task_id: String,
        role: String,
        content: String,
        created_at: String,
        record_id: Option<String>,
        parent_record_id: Option<String>,
    }

    let messages = query_records(
        connection,
        "SELECT message.id, message.task_id, message.role, message.content, message.timestamp,
                sync.record_id, sync.parent_record_id
         FROM task_messages message
         JOIN tasks task ON task.id = message.task_id
         LEFT JOIN task_message_sync sync ON sync.message_id = message.id
         WHERE task.project_id = ?1 AND task.hidden = 0 AND UPPER(task.task_type) <> 'IDLE'
         ORDER BY message.task_id, message.timestamp, message.id",
        [project_id],
        |row| {
            Ok(LocalMessage {
                id: row.get(0)?,
                task_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
                record_id: row.get(5)?,
                parent_record_id: row.get(6)?,
            })
        },
    )?;
    let mut result = Vec::with_capacity(messages.len());
    let mut previous_by_task = HashMap::<String, String>::new();
    for message in messages {
        let record_id = message
            .record_id
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let parent_record_id = message
            .parent_record_id
            .or_else(|| previous_by_task.get(&message.task_id).cloned());
        connection.execute(
            "INSERT OR IGNORE INTO task_message_sync (record_id, message_id, parent_record_id)
             VALUES (?1, ?2, ?3)",
            params![record_id, message.id, parent_record_id],
        )?;
        previous_by_task.insert(message.task_id.clone(), record_id.clone());
        result.push(PortableTaskMessage {
            record_id,
            parent_record_id,
            task_id: message.task_id,
            role: message.role,
            content: message.content,
            created_at: message.created_at,
        });
    }
    Ok(result)
}

fn export_conversation_messages(
    connection: &Connection,
    project_id: &str,
) -> Result<Vec<PortableConversationMessage>> {
    struct LocalMessage {
        id: i64,
        conversation_id: String,
        sender_type: String,
        sender_id: String,
        sender_name: Option<String>,
        content: String,
        message_type: String,
        linked_task_id: Option<String>,
        metadata: String,
        created_at: String,
        record_id: Option<String>,
        parent_record_id: Option<String>,
    }

    let messages = query_records(
        connection,
        "SELECT message.id, message.conversation_id, message.sender_type, message.sender_id,
                message.sender_name, message.content, message.message_type,
                message.linked_task_id, message.metadata, message.created_at,
                sync.record_id, sync.parent_record_id
         FROM conversation_messages message
         JOIN conversations conversation ON conversation.id = message.conversation_id
         LEFT JOIN conversation_message_sync sync ON sync.message_id = message.id
         WHERE conversation.project_id = ?1
         ORDER BY message.conversation_id, message.created_at, message.id",
        [project_id],
        |row| {
            Ok(LocalMessage {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                sender_type: row.get(2)?,
                sender_id: row.get(3)?,
                sender_name: row.get(4)?,
                content: row.get(5)?,
                message_type: row.get(6)?,
                linked_task_id: row.get(7)?,
                metadata: row.get(8)?,
                created_at: row.get(9)?,
                record_id: row.get(10)?,
                parent_record_id: row.get(11)?,
            })
        },
    )?;
    let mut result = Vec::with_capacity(messages.len());
    let mut previous_by_conversation = HashMap::<String, String>::new();
    for message in messages {
        let record_id = message
            .record_id
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let parent_record_id = message.parent_record_id.or_else(|| {
            previous_by_conversation
                .get(&message.conversation_id)
                .cloned()
        });
        connection.execute(
            "INSERT OR IGNORE INTO conversation_message_sync
                (record_id, message_id, parent_record_id)
             VALUES (?1, ?2, ?3)",
            params![record_id, message.id, parent_record_id],
        )?;
        previous_by_conversation.insert(message.conversation_id.clone(), record_id.clone());
        result.push(PortableConversationMessage {
            record_id,
            parent_record_id,
            conversation_id: message.conversation_id,
            sender_type: message.sender_type,
            sender_id: message.sender_id,
            sender_name: message.sender_name,
            content: message.content,
            message_type: message.message_type,
            linked_task_id: message.linked_task_id,
            metadata: parse_json(message.metadata, "Conversation message metadata")?,
            created_at: message.created_at,
        });
    }
    Ok(result)
}

fn export_memory(
    connection: &Connection,
    project_id: &str,
) -> Result<(Vec<PortableMemoryNote>, Vec<PortableMemoryLink>)> {
    let mut notes = query_records(
        connection,
        "SELECT id, title, body, summary, note_type, state, source_task_id,
                created_by, pinned, created_at, updated_at
         FROM project_memory_notes WHERE project_id = ?1 ORDER BY id",
        [project_id],
        |row| {
            Ok(PortableMemoryNote {
                id: row.get(0)?,
                title: row.get(1)?,
                body: row.get(2)?,
                summary: row.get(3)?,
                note_type: row.get(4)?,
                state: row.get(5)?,
                source_task_id: row.get(6)?,
                created_by: row.get(7)?,
                pinned: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
                tags: Vec::new(),
            })
        },
    )?;
    let mut tag_statement = connection
        .prepare("SELECT tag FROM project_memory_tags WHERE note_id = ?1 ORDER BY tag_key, tag")?;
    for note in &mut notes {
        note.tags = tag_statement
            .query_map([&note.id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }
    let links = query_records(
        connection,
        "SELECT link.from_note_id, link.to_note_id, link.link_type, link.strength, link.created_at
         FROM project_memory_links link
         JOIN project_memory_notes source ON source.id = link.from_note_id
         JOIN project_memory_notes target ON target.id = link.to_note_id
         WHERE source.project_id = ?1 AND target.project_id = ?1
         ORDER BY link.from_note_id, link.to_note_id, link.link_type",
        [project_id],
        |row| {
            Ok(PortableMemoryLink {
                from_note_id: row.get(0)?,
                to_note_id: row.get(1)?,
                link_type: row.get(2)?,
                strength: row.get(3)?,
                created_at: row.get(4)?,
            })
        },
    )?;
    Ok((notes, links))
}

pub(super) fn import_snapshot(
    db: &Database,
    config: &mut Config,
    config_path: &Path,
    project_dir: &Path,
    snapshot: &PortableSnapshot,
) -> Result<()> {
    // Reload the file-backed form so values supplied only through environment
    // overrides are never materialized into xpressclaw.yaml by a fetch.
    let mut updated_config = if config_path.exists() {
        Config::load(config_path)?
    } else {
        config.clone()
    };
    let original_config = fs::read(config_path).ok();
    merge_agent_config(&mut updated_config, snapshot, project_dir)?;

    let transaction_result = db.with_conn(|connection| -> Result<()> {
        let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
        import_transaction(&transaction, snapshot)?;
        if fs::read(config_path).ok().as_deref() != original_config.as_deref() {
            return Err(Error::Sync(
                "xpressclaw.yaml changed during fetch; no synchronized state was imported".into(),
            ));
        }
        updated_config.save(config_path)?;
        if let Err(error) = transaction.commit() {
            restore_config(config_path, original_config.as_deref());
            return Err(Error::from(error));
        }
        Ok(())
    });
    transaction_result?;
    *config = updated_config;
    Ok(())
}

fn restore_config(path: &Path, contents: Option<&[u8]>) {
    match contents {
        Some(contents) => {
            let temporary = path.with_extension("yaml.sync-restore");
            if fs::write(&temporary, contents).is_ok() {
                let _ = fs::rename(temporary, path);
            }
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

fn merge_agent_config(
    config: &mut Config,
    snapshot: &PortableSnapshot,
    project_dir: &Path,
) -> Result<()> {
    let workspace = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf())
        .display()
        .to_string();
    for portable in &snapshot.agents {
        let existing = config
            .agents
            .iter_mut()
            .find(|agent| agent.name == portable.id);
        match existing {
            Some(agent) => apply_portable_agent(agent, portable, None)?,
            None => {
                let mut agent = AgentConfig {
                    name: portable.id.clone(),
                    backend: portable.backend.clone(),
                    ..AgentConfig::default()
                };
                // Subscription auth mounts host credential directories. A newly
                // synchronized agent has no local opt-in, so keep it disabled.
                agent.runner.subscription_auth = false;
                apply_portable_agent(&mut agent, portable, Some(&workspace))?;
                config.agents.push(agent);
            }
        }
    }
    Ok(())
}

fn apply_portable_agent(
    agent: &mut AgentConfig,
    portable: &PortableAgent,
    new_workspace: Option<&str>,
) -> Result<()> {
    agent.name = portable.id.clone();
    agent.backend = portable.backend.clone();
    agent.model = None;
    match &portable.settings.llm {
        Some(shared_llm) => {
            let llm = agent.llm.get_or_insert_with(AgentLlmConfig::default);
            llm.provider = shared_llm.provider.clone();
            llm.model = shared_llm.model.clone();
        }
        None => {
            if let Some(llm) = agent.llm.as_mut() {
                llm.provider = None;
                llm.model = None;
            }
        }
    }
    // api_key and base_url are deliberately preserved from local config.
    agent.runner.kind = portable.settings.runner.kind.clone();
    agent.runner.image = portable.settings.runner.image.clone();
    agent.runner.project_name = portable.settings.runner.project_name.clone();
    agent.runner.model = portable.settings.runner.model.clone();
    agent.runner.session_config = portable
        .settings
        .runner
        .session_config
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    agent.runner.mcp_servers = portable.settings.runner.mcp_servers.clone();
    agent.runner.startup_commands = portable.settings.runner.startup_commands.clone();
    agent.runner.command = portable.settings.runner.command.clone();
    if let Some(workspace) = new_workspace {
        agent.runner.workspace = Some(workspace.to_string());
    }
    // workspace, environment, subscription credentials, SSH forwarding,
    // container-engine access, volumes, hooks, and MCP definitions stay local.
    agent.tools = portable.settings.tools.clone();
    agent.skills = portable.settings.skills.clone();
    agent.budget = portable
        .settings
        .budget
        .as_ref()
        .map(portable_budget)
        .transpose()?;
    agent.rate_limit = portable
        .settings
        .rate_limit
        .as_ref()
        .map(|rate| RateLimitConfig {
            requests_per_minute: rate.requests_per_minute,
            tokens_per_minute: rate.tokens_per_minute,
            concurrent_requests: rate.concurrent_requests,
        });
    agent.wake_on = portable
        .settings
        .wake_on
        .iter()
        .map(|wake| WakeOnConfig {
            schedule: wake.schedule.clone(),
            event: wake.event.clone(),
            condition: wake.condition.clone(),
        })
        .collect();
    agent.idle_prompt = portable.settings.idle_prompt.clone();
    Ok(())
}

fn portable_budget(budget: &PortableBudgetSettings) -> Result<BudgetConfig> {
    let on_exceeded = match budget.on_exceeded.as_str() {
        "pause" => OnExceeded::Pause,
        "alert" => OnExceeded::Alert,
        "degrade" => OnExceeded::Degrade,
        "stop" => OnExceeded::Stop,
        value => {
            return Err(Error::Sync(format!(
                "Agent budget has invalid on_exceeded value '{value}'"
            )))
        }
    };
    Ok(BudgetConfig {
        daily: budget.daily.clone(),
        monthly: budget.monthly.clone(),
        per_task: budget.per_task.clone(),
        on_exceeded,
        fallback_model: budget.fallback_model.clone(),
        warn_at_percent: budget.warn_at_percent,
    })
}

fn import_transaction(connection: &Connection, snapshot: &PortableSnapshot) -> Result<()> {
    if project_is_active(connection, &snapshot.project.id)? {
        return Err(quiescent_error());
    }
    validate_local_scopes(connection, snapshot)?;
    let project = &snapshot.project;
    connection.execute(
        "INSERT INTO projects (id, name, description, icon, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            description = excluded.description,
            icon = excluded.icon,
            updated_at = excluded.updated_at",
        params![
            project.id,
            project.name,
            project.description,
            project.icon,
            project.created_at,
            project.updated_at
        ],
    )?;
    import_agents(connection, snapshot)?;
    import_conversations(connection, snapshot)?;
    import_tasks(connection, snapshot)?;
    validate_merged_task_graph(connection, &snapshot.project.id)?;
    import_task_messages(connection, snapshot)?;
    import_workflows(connection, snapshot)?;
    import_memory(connection, snapshot)?;
    import_conversation_messages(connection, snapshot)?;
    Ok(())
}

fn validate_local_scopes(connection: &Connection, snapshot: &PortableSnapshot) -> Result<()> {
    for agent in &snapshot.agents {
        reject_foreign_owner(
            connection,
            "agents",
            "project_id",
            &agent.id,
            &snapshot.project.id,
        )?;
        let session_agent: Option<String> = connection
            .query_row(
                "SELECT agent_id FROM logical_sessions WHERE id = ?1",
                [&agent.id],
                |row| row.get(0),
            )
            .optional()?;
        let session_id: Option<String> = connection
            .query_row(
                "SELECT id FROM logical_sessions WHERE agent_id = ?1",
                [&agent.id],
                |row| row.get(0),
            )
            .optional()?;
        if session_agent
            .as_deref()
            .is_some_and(|id| id != agent.id.as_str())
            || session_id
                .as_deref()
                .is_some_and(|id| id != agent.id.as_str())
        {
            return Err(Error::Sync(format!(
                "Agent '{}' conflicts with an existing local logical session",
                agent.id
            )));
        }
    }
    for task in &snapshot.tasks {
        reject_foreign_owner(
            connection,
            "tasks",
            "project_id",
            &task.id,
            &snapshot.project.id,
        )?;
    }
    for conversation in &snapshot.conversations {
        reject_foreign_owner(
            connection,
            "conversations",
            "project_id",
            &conversation.id,
            &snapshot.project.id,
        )?;
    }
    for note in &snapshot.memory_notes {
        reject_foreign_owner(
            connection,
            "project_memory_notes",
            "project_id",
            &note.id,
            &snapshot.project.id,
        )?;
    }
    for workflow in &snapshot.workflows {
        let conflicting_name: Option<String> = connection
            .query_row(
                "SELECT id FROM workflows WHERE name = ?1 AND id <> ?2",
                params![workflow.name, workflow.id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = conflicting_name {
            return Err(Error::Sync(format!(
                "workflow name '{}' is already used locally by workflow '{id}'",
                workflow.name
            )));
        }
        let existing: Option<(String, Option<String>, String, u32)> = connection
            .query_row(
                "SELECT name, description, yaml_content, version FROM workflows WHERE id = ?1",
                [&workflow.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let associated_with_project: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM project_workflows
                WHERE workflow_id = ?1 AND project_id = ?2
             )",
            params![workflow.id, snapshot.project.id],
            |row| row.get(0),
        )?;
        let shared_elsewhere: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM project_workflows
                WHERE workflow_id = ?1 AND project_id <> ?2
             )",
            params![workflow.id, snapshot.project.id],
            |row| row.get(0),
        )?;
        if (!associated_with_project || shared_elsewhere)
            && existing.is_some_and(|(name, description, yaml, version)| {
                name != workflow.name
                    || description != workflow.description
                    || yaml != workflow.yaml_content
                    || version != workflow.version
            })
        {
            return Err(Error::Sync(format!(
                "workflow '{}' already has a conflicting local definition outside this Project",
                workflow.id
            )));
        }
    }
    Ok(())
}

fn reject_foreign_owner(
    connection: &Connection,
    table: &str,
    owner_column: &str,
    id: &str,
    project_id: &str,
) -> Result<()> {
    let sql = format!("SELECT {owner_column} FROM {table} WHERE id = ?1");
    let owner: Option<Option<String>> = connection
        .query_row(&sql, [id], |row| row.get(0))
        .optional()?;
    if owner.is_some_and(|owner| owner.as_deref() != Some(project_id)) {
        return Err(Error::Sync(format!(
            "record '{id}' already belongs to another local Project"
        )));
    }
    Ok(())
}

fn import_agents(connection: &Connection, snapshot: &PortableSnapshot) -> Result<()> {
    for agent in &snapshot.agents {
        connection.execute(
            "INSERT INTO agents
                (id, name, backend, config, status, desired_status, project_id, created_at)
             VALUES (?1, ?2, ?3, '{}', 'stopped', 'stopped', ?4, COALESCE(?5, CURRENT_TIMESTAMP))
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                backend = excluded.backend,
                project_id = excluded.project_id",
            params![
                agent.id,
                agent.name,
                agent.backend,
                snapshot.project.id,
                agent.created_at
            ],
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO logical_sessions (id, agent_id, title)
             VALUES (?1, ?1, ?2)",
            params![agent.id, agent.name],
        )?;
    }
    Ok(())
}

fn import_conversations(connection: &Connection, snapshot: &PortableSnapshot) -> Result<()> {
    for conversation in &snapshot.conversations {
        connection.execute(
            "INSERT INTO conversations
                (id, title, icon, created_at, updated_at, last_message_at, project_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                icon = excluded.icon,
                updated_at = excluded.updated_at,
                last_message_at = excluded.last_message_at,
                project_id = excluded.project_id",
            params![
                conversation.id,
                conversation.title,
                conversation.icon,
                conversation.created_at,
                conversation.updated_at,
                conversation.last_message_at,
                snapshot.project.id
            ],
        )?;
    }
    for participant in &snapshot.participants {
        connection.execute(
            "INSERT INTO conversation_participants
                (conversation_id, participant_type, participant_id, joined_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(conversation_id, participant_type, participant_id) DO NOTHING",
            params![
                participant.conversation_id,
                participant.participant_type,
                participant.participant_id,
                participant.joined_at
            ],
        )?;
    }
    Ok(())
}

fn import_tasks(connection: &Connection, snapshot: &PortableSnapshot) -> Result<()> {
    for task in &snapshot.tasks {
        connection.execute(
            "INSERT INTO tasks
                (id, title, description, status, priority, agent_id, parent_task_id,
                 conversation_id, project_id, created_at, updated_at, completed_at,
                 task_type, hidden)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                description = excluded.description,
                status = excluded.status,
                priority = excluded.priority,
                agent_id = excluded.agent_id,
                conversation_id = excluded.conversation_id,
                project_id = excluded.project_id,
                updated_at = excluded.updated_at,
                completed_at = excluded.completed_at,
                task_type = excluded.task_type,
                hidden = excluded.hidden",
            params![
                task.id,
                task.title,
                task.description,
                task.status,
                task.priority,
                task.agent_id,
                task.conversation_id,
                snapshot.project.id,
                task.created_at,
                task.updated_at,
                task.completed_at,
                task.task_type,
                task.hidden
            ],
        )?;
    }
    for task in &snapshot.tasks {
        connection.execute(
            "UPDATE tasks SET parent_task_id = ?1 WHERE id = ?2",
            params![task.parent_task_id, task.id],
        )?;
    }
    for dependency in &snapshot.task_dependencies {
        connection.execute(
            "INSERT OR IGNORE INTO task_dependencies (task_id, depends_on_id)
             VALUES (?1, ?2)",
            params![dependency.task_id, dependency.depends_on_id],
        )?;
    }
    Ok(())
}

fn validate_merged_task_graph(connection: &Connection, project_id: &str) -> Result<()> {
    let tasks = query_records(
        connection,
        "SELECT id, parent_task_id FROM tasks WHERE project_id = ?1",
        [project_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )?;
    let task_ids = tasks
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<std::collections::HashSet<_>>();
    if tasks
        .iter()
        .filter_map(|(_, parent)| parent.as_ref())
        .any(|parent| !task_ids.contains(parent))
    {
        return Err(Error::Sync(
            "merged task parent graph crosses the Project boundary".into(),
        ));
    }
    validate_parent_cycles(
        "merged task",
        tasks.iter().map(|(id, parent)| (id, parent.as_ref())),
    )?;

    let dependencies = query_records(
        connection,
        "SELECT dependency.task_id, dependency.depends_on_id
         FROM task_dependencies dependency
         JOIN tasks task ON task.id = dependency.task_id
         JOIN tasks prerequisite ON prerequisite.id = dependency.depends_on_id
         WHERE task.project_id = ?1 OR prerequisite.project_id = ?1",
        [project_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    if dependencies
        .iter()
        .any(|(task, dependency)| !task_ids.contains(task) || !task_ids.contains(dependency))
    {
        return Err(Error::Sync(
            "merged task dependency graph crosses the Project boundary".into(),
        ));
    }
    validate_dependency_cycles(
        &task_ids,
        dependencies
            .iter()
            .map(|(task, dependency)| (task, dependency)),
    )
}

fn import_task_messages(connection: &Connection, snapshot: &PortableSnapshot) -> Result<()> {
    let mut imported = Vec::with_capacity(snapshot.task_messages.len());
    let order = message_import_order(snapshot.task_messages.iter().map(|message| {
        (
            message.record_id.as_str(),
            message.parent_record_id.as_deref(),
            message.created_at.as_str(),
        )
    }))?;
    for index in order {
        let message = &snapshot.task_messages[index];
        let existing: Option<(i64, String, String, String, String)> = connection
            .query_row(
                "SELECT message.id, message.task_id, message.role, message.content, message.timestamp
                 FROM task_message_sync sync
                 JOIN task_messages message ON message.id = sync.message_id
                 WHERE sync.record_id = ?1",
                [&message.record_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()?;
        let already_existed = existing.is_some();
        let message_id = if let Some((id, task_id, role, content, created_at)) = existing {
            if task_id != message.task_id
                || role != message.role
                || content != message.content
                || created_at != message.created_at
            {
                return Err(Error::Sync(format!(
                    "immutable task message record '{}' differs from the local copy",
                    message.record_id
                )));
            }
            id
        } else {
            connection.execute(
                "INSERT INTO task_messages (task_id, role, content, timestamp)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    message.task_id,
                    message.role,
                    message.content,
                    message.created_at
                ],
            )?;
            connection.last_insert_rowid()
        };
        connection.execute(
            "INSERT OR IGNORE INTO task_message_sync (record_id, message_id, parent_record_id)
             VALUES (?1, ?2, NULL)",
            params![message.record_id, message_id],
        )?;
        imported.push((
            &message.record_id,
            &message.parent_record_id,
            already_existed,
        ));
    }
    for (record_id, parent_record_id, already_existed) in imported {
        let existing_parent: Option<String> = connection.query_row(
            "SELECT parent_record_id FROM task_message_sync WHERE record_id = ?1",
            [record_id],
            |row| row.get(0),
        )?;
        if already_existed && existing_parent.as_ref() != parent_record_id.as_ref() {
            return Err(Error::Sync(format!(
                "immutable task message record '{record_id}' has a different parent"
            )));
        }
        connection.execute(
            "UPDATE task_message_sync SET parent_record_id = ?1 WHERE record_id = ?2",
            params![parent_record_id, record_id],
        )?;
    }
    Ok(())
}

fn import_workflows(connection: &Connection, snapshot: &PortableSnapshot) -> Result<()> {
    for workflow in &snapshot.workflows {
        connection.execute(
            "INSERT INTO workflows
                (id, name, description, yaml_content, enabled, version, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                description = excluded.description,
                yaml_content = excluded.yaml_content,
                version = excluded.version,
                updated_at = excluded.updated_at",
            params![
                workflow.id,
                workflow.name,
                workflow.description,
                workflow.yaml_content,
                workflow.version,
                workflow.created_at,
                workflow.updated_at
            ],
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO project_workflows (project_id, workflow_id)
             VALUES (?1, ?2)",
            params![snapshot.project.id, workflow.id],
        )?;
    }
    Ok(())
}

fn import_memory(connection: &Connection, snapshot: &PortableSnapshot) -> Result<()> {
    for note in &snapshot.memory_notes {
        let search_key = task_search_key(&format!(
            "{}\n{}\n{}\n{}",
            note.title,
            note.summary,
            note.body,
            note.tags.join(" ")
        ));
        connection.execute(
            "INSERT INTO project_memory_notes
                (id, project_id, title, body, summary, note_type, state, source_task_id,
                 source_attempt_id, created_by, pinned, search_key, created_at, updated_at,
                 last_accessed_at, access_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11, ?12, ?13, ?13, 0)
             ON CONFLICT(id) DO UPDATE SET
                project_id = excluded.project_id,
                title = excluded.title,
                body = excluded.body,
                summary = excluded.summary,
                note_type = excluded.note_type,
                state = excluded.state,
                source_task_id = excluded.source_task_id,
                created_by = excluded.created_by,
                pinned = excluded.pinned,
                search_key = excluded.search_key,
                updated_at = excluded.updated_at",
            params![
                note.id,
                snapshot.project.id,
                note.title,
                note.body,
                note.summary,
                note.note_type,
                note.state,
                note.source_task_id,
                note.created_by,
                note.pinned,
                search_key,
                note.created_at,
                note.updated_at
            ],
        )?;
        connection.execute(
            "DELETE FROM project_memory_tags WHERE note_id = ?1",
            [&note.id],
        )?;
        for tag in &note.tags {
            connection.execute(
                "INSERT INTO project_memory_tags (note_id, tag, tag_key) VALUES (?1, ?2, ?3)",
                params![note.id, tag, task_search_key(tag)],
            )?;
        }
        connection.execute(
            "DELETE FROM project_memory_embeddings WHERE note_id = ?1",
            [&note.id],
        )?;
        if note.state != "archived" {
            let embedding = simple_embedding(&search_key);
            connection.execute(
                "INSERT INTO project_memory_embeddings (note_id, embedding, project_id)
                 VALUES (?1, ?2, ?3)",
                params![note.id, embedding.as_bytes(), snapshot.project.id],
            )?;
        }
    }
    for link in &snapshot.memory_links {
        connection.execute(
            "INSERT INTO project_memory_links
                (from_note_id, to_note_id, link_type, strength, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(from_note_id, to_note_id, link_type) DO UPDATE SET
                strength = excluded.strength",
            params![
                link.from_note_id,
                link.to_note_id,
                link.link_type,
                link.strength,
                link.created_at
            ],
        )?;
    }
    Ok(())
}

fn import_conversation_messages(
    connection: &Connection,
    snapshot: &PortableSnapshot,
) -> Result<()> {
    struct ExistingMessage {
        id: i64,
        conversation_id: String,
        sender_type: String,
        sender_id: String,
        sender_name: Option<String>,
        content: String,
        message_type: String,
        metadata: String,
        created_at: String,
    }

    let mut imported = Vec::with_capacity(snapshot.conversation_messages.len());
    let order = message_import_order(snapshot.conversation_messages.iter().map(|message| {
        (
            message.record_id.as_str(),
            message.parent_record_id.as_deref(),
            message.created_at.as_str(),
        )
    }))?;
    for index in order {
        let message = &snapshot.conversation_messages[index];
        let existing: Option<ExistingMessage> = connection
            .query_row(
                "SELECT message.id, message.conversation_id, message.sender_type,
                        message.sender_id, message.sender_name, message.content,
                        message.message_type, message.metadata, message.created_at
                 FROM conversation_message_sync sync
                 JOIN conversation_messages message ON message.id = sync.message_id
                 WHERE sync.record_id = ?1",
                [&message.record_id],
                |row| {
                    Ok(ExistingMessage {
                        id: row.get(0)?,
                        conversation_id: row.get(1)?,
                        sender_type: row.get(2)?,
                        sender_id: row.get(3)?,
                        sender_name: row.get(4)?,
                        content: row.get(5)?,
                        message_type: row.get(6)?,
                        metadata: row.get(7)?,
                        created_at: row.get(8)?,
                    })
                },
            )
            .optional()?;
        let already_existed = existing.is_some();
        let message_id = if let Some(existing) = existing {
            if existing.conversation_id != message.conversation_id
                || existing.sender_type != message.sender_type
                || existing.sender_id != message.sender_id
                || existing.sender_name != message.sender_name
                || existing.content != message.content
                || existing.message_type != message.message_type
                || parse_json(existing.metadata, "Conversation message metadata")?
                    != message.metadata
                || existing.created_at != message.created_at
            {
                return Err(Error::Sync(format!(
                    "immutable Conversation message record '{}' differs from the local copy",
                    message.record_id
                )));
            }
            // The message body and identity are immutable, but this relationship
            // is intentionally mutable: deleting a task clears it via the local
            // foreign key and that cleared association must synchronize.
            connection.execute(
                "UPDATE conversation_messages SET linked_task_id = ?1 WHERE id = ?2",
                params![message.linked_task_id, existing.id],
            )?;
            existing.id
        } else {
            let metadata = serde_json::to_string(&message.metadata).map_err(|error| {
                Error::Sync(format!(
                    "failed to serialize Conversation message metadata: {error}"
                ))
            })?;
            connection.execute(
                "INSERT INTO conversation_messages
                    (conversation_id, sender_type, sender_id, sender_name, content,
                     message_type, linked_task_id, metadata, created_at, processed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)",
                params![
                    message.conversation_id,
                    message.sender_type,
                    message.sender_id,
                    message.sender_name,
                    message.content,
                    message.message_type,
                    message.linked_task_id,
                    metadata,
                    message.created_at
                ],
            )?;
            connection.last_insert_rowid()
        };
        connection.execute(
            "INSERT OR IGNORE INTO conversation_message_sync
                (record_id, message_id, parent_record_id)
             VALUES (?1, ?2, NULL)",
            params![message.record_id, message_id],
        )?;
        imported.push((
            &message.record_id,
            &message.parent_record_id,
            already_existed,
        ));
    }
    for (record_id, parent_record_id, already_existed) in imported {
        let existing_parent: Option<String> = connection.query_row(
            "SELECT parent_record_id FROM conversation_message_sync WHERE record_id = ?1",
            [record_id],
            |row| row.get(0),
        )?;
        if already_existed && existing_parent.as_ref() != parent_record_id.as_ref() {
            return Err(Error::Sync(format!(
                "immutable Conversation message record '{record_id}' has a different parent"
            )));
        }
        connection.execute(
            "UPDATE conversation_message_sync SET parent_record_id = ?1 WHERE record_id = ?2",
            params![parent_record_id, record_id],
        )?;
    }
    for conversation in &snapshot.conversations {
        connection.execute(
            "UPDATE conversations
             SET last_message_at = (
                 SELECT created_at FROM conversation_messages
                 WHERE conversation_id = ?1
                 ORDER BY julianday(created_at) DESC, id DESC
                 LIMIT 1
             )
             WHERE id = ?1",
            [&conversation.id],
        )?;
    }
    Ok(())
}

fn message_import_order<'a>(
    records: impl Iterator<Item = (&'a str, Option<&'a str>, &'a str)>,
) -> Result<Vec<usize>> {
    let records = records.collect::<Vec<_>>();
    let by_id = records
        .iter()
        .enumerate()
        .map(|(index, (id, _, _))| (*id, index))
        .collect::<HashMap<_, _>>();
    let mut children = HashMap::<&str, Vec<usize>>::new();
    let mut unmet = vec![0_u8; records.len()];
    for (index, (_, parent, _)) in records.iter().enumerate() {
        if let Some(parent) = parent {
            if !by_id.contains_key(parent) {
                return Err(Error::Sync(
                    "message record references an unknown parent".into(),
                ));
            }
            unmet[index] = 1;
            children.entry(parent).or_default().push(index);
        }
    }
    let mut ready = records
        .iter()
        .enumerate()
        .filter_map(|(index, (id, _, created_at))| {
            (unmet[index] == 0).then_some((*created_at, *id, index))
        })
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(records.len());
    while let Some(entry) = ready.pop_first() {
        let (_, id, index) = entry;
        ordered.push(index);
        for child in children.get(id).into_iter().flatten() {
            unmet[*child] = 0;
            let (child_id, _, created_at) = records[*child];
            ready.insert((created_at, child_id, *child));
        }
    }
    if ordered.len() != records.len() {
        return Err(Error::Sync("message parent graph contains a cycle".into()));
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::NativeRunnerConfig;

    fn insert_project_data(db: &Database) {
        db.with_conn(|connection| {
            connection
                .execute(
                    "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One')",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO agents (id, name, backend, config, project_id)
                     VALUES ('atlas', 'Atlas', 'codex', '{}', 'project-one')",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO conversations (id, title, project_id)
                     VALUES ('conversation-one', 'Design', 'project-one')",
                    [],
                )
                .unwrap();
            connection
                .execute_batch(
                    "INSERT INTO tasks (id, title, agent_id, project_id)
                     VALUES ('task-one', 'Build it', 'atlas', 'project-one');
                     INSERT INTO task_messages (task_id, role, content, timestamp)
                     VALUES ('task-one', 'user', 'start', '2026-01-01 00:00:00'),
                            ('task-one', 'assistant', 'done', '2026-01-01 00:00:01');",
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO conversation_messages
                        (conversation_id, sender_type, sender_id, content, created_at)
                     VALUES ('conversation-one', 'user', 'local-user', 'one', '2026-01-01 00:00:00'),
                            ('conversation-one', 'agent', 'atlas', 'two', '2026-01-01 00:00:01')",
                    [],
                )
                .unwrap();
        });
    }

    fn manifest() -> ProjectSyncManifest {
        ProjectSyncManifest::new(
            "project-one",
            "git@example.test:data.git",
            "main",
            "projects/project-one",
        )
        .unwrap()
    }

    #[test]
    fn export_assigns_stable_parented_message_records() {
        let db = Database::open_memory().unwrap();
        insert_project_data(&db);
        let config = Config {
            agents: vec![AgentConfig {
                name: "atlas".into(),
                backend: "codex".into(),
                llm: Some(AgentLlmConfig {
                    provider: Some("openai".into()),
                    model: Some("gpt-test".into()),
                    api_key: Some("must-stay-local".into()),
                    base_url: Some("https://local.invalid".into()),
                }),
                runner: NativeRunnerConfig {
                    workspace: Some("/private/workspace".into()),
                    environment: HashMap::from([("SECRET".into(), "local".into())]),
                    ..NativeRunnerConfig::default()
                },
                ..AgentConfig::default()
            }],
            ..Config::default()
        };

        let first = export_snapshot(&db, &config, &manifest()).unwrap();
        let second = export_snapshot(&db, &config, &manifest()).unwrap();
        assert_eq!(first.digest().unwrap(), second.digest().unwrap());
        assert_eq!(first.conversation_messages.len(), 2);
        assert_eq!(
            first.conversation_messages[1].parent_record_id.as_ref(),
            Some(&first.conversation_messages[0].record_id)
        );
        assert_eq!(first.task_messages.len(), 2);
        assert_eq!(
            first.task_messages[1].parent_record_id.as_ref(),
            Some(&first.task_messages[0].record_id)
        );
        let serialized = serde_yaml::to_string(&first).unwrap();
        assert!(!serialized.contains("must-stay-local"));
        assert!(!serialized.contains("/private/workspace"));
        assert!(!serialized.contains("SECRET"));
    }

    #[test]
    fn fetch_merge_preserves_local_agent_secrets_and_paths() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("xpressclaw.yaml");
        let db = Database::open_memory().unwrap();
        insert_project_data(&db);
        let mut source_config = Config {
            agents: vec![AgentConfig {
                name: "atlas".into(),
                backend: "codex".into(),
                llm: Some(AgentLlmConfig {
                    provider: Some("openai".into()),
                    model: Some("shared-model".into()),
                    api_key: None,
                    base_url: None,
                }),
                ..AgentConfig::default()
            }],
            ..Config::default()
        };
        let snapshot = export_snapshot(&db, &source_config, &manifest()).unwrap();
        source_config.agents[0].llm.as_mut().unwrap().api_key = Some("local-key".into());
        source_config.agents[0].runner.workspace = Some("/local/path".into());
        source_config.save(&config_path).unwrap();

        import_snapshot(
            &db,
            &mut source_config,
            &config_path,
            directory.path(),
            &snapshot,
        )
        .unwrap();
        let merged = &source_config.agents[0];
        assert_eq!(
            merged.llm.as_ref().unwrap().api_key.as_deref(),
            Some("local-key")
        );
        assert_eq!(merged.runner.workspace.as_deref(), Some("/local/path"));
        assert_eq!(
            merged.llm.as_ref().unwrap().model.as_deref(),
            Some("shared-model")
        );
    }

    #[test]
    fn fetch_disables_subscription_auth_for_new_agents() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("xpressclaw.yaml");
        let db = Database::open_memory().unwrap();
        insert_project_data(&db);
        let source_config = Config {
            agents: vec![AgentConfig {
                name: "atlas".into(),
                backend: "codex".into(),
                ..AgentConfig::default()
            }],
            ..Config::default()
        };
        let snapshot = export_snapshot(&db, &source_config, &manifest()).unwrap();
        let mut target_config = Config {
            agents: Vec::new(),
            ..Config::default()
        };
        target_config.save(&config_path).unwrap();

        import_snapshot(
            &db,
            &mut target_config,
            &config_path,
            directory.path(),
            &snapshot,
        )
        .unwrap();

        assert_eq!(target_config.agents.len(), 1);
        assert!(!target_config.agents[0].runner.subscription_auth);
        let saved = Config::load(&config_path).unwrap();
        assert!(!saved.agents[0].runner.subscription_auth);
    }

    #[test]
    fn fetch_does_not_persist_environment_only_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("xpressclaw.yaml");
        let db = Database::open_memory().unwrap();
        insert_project_data(&db);
        let file_config = Config {
            agents: vec![AgentConfig {
                name: "atlas".into(),
                backend: "codex".into(),
                llm: Some(AgentLlmConfig {
                    provider: Some("openai".into()),
                    model: Some("shared-model".into()),
                    api_key: None,
                    base_url: None,
                }),
                ..AgentConfig::default()
            }],
            ..Config::default()
        };
        file_config.save(&config_path).unwrap();
        let snapshot = export_snapshot(&db, &file_config, &manifest()).unwrap();
        let mut runtime_config = file_config;
        runtime_config.agents[0].llm.as_mut().unwrap().api_key =
            Some("environment-only-key".into());

        import_snapshot(
            &db,
            &mut runtime_config,
            &config_path,
            directory.path(),
            &snapshot,
        )
        .unwrap();

        let saved = Config::load(&config_path).unwrap();
        assert!(saved.agents[0].llm.as_ref().unwrap().api_key.is_none());
        assert!(runtime_config.agents[0]
            .llm
            .as_ref()
            .unwrap()
            .api_key
            .is_none());
    }

    #[test]
    fn import_rebuilds_local_project_memory_embeddings() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("xpressclaw.yaml");
        let db = Database::open_memory().unwrap();
        insert_project_data(&db);
        db.with_conn(|connection| {
            connection
                .execute(
                    "INSERT INTO project_memory_notes
                        (id, project_id, title, body, summary, note_type, state,
                         created_by, pinned, search_key)
                     VALUES ('note-one', 'project-one', 'Decision', 'Use Git',
                             'Git store', 'decision', 'evergreen', 'user', 1, 'git')",
                    [],
                )
                .unwrap();
        });
        let config = Config {
            agents: vec![AgentConfig {
                name: "atlas".into(),
                backend: "codex".into(),
                ..AgentConfig::default()
            }],
            ..Config::default()
        };
        config.save(&config_path).unwrap();
        let snapshot = export_snapshot(&db, &config, &manifest()).unwrap();

        let mut loaded = Config::load(&config_path).unwrap();
        import_snapshot(&db, &mut loaded, &config_path, directory.path(), &snapshot).unwrap();
        let embeddings: i64 = db
            .with_conn(|connection| {
                connection.query_row(
                    "SELECT COUNT(*) FROM project_memory_embeddings WHERE note_id = 'note-one'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(embeddings, 1);
    }

    #[test]
    fn import_rejects_a_conflicting_unassociated_workflow() {
        let db = Database::open_memory().unwrap();
        insert_project_data(&db);
        let config = Config {
            agents: vec![AgentConfig {
                name: "atlas".into(),
                backend: "codex".into(),
                ..AgentConfig::default()
            }],
            ..Config::default()
        };
        let mut snapshot = export_snapshot(&db, &config, &manifest()).unwrap();
        snapshot.workflows.push(PortableWorkflow {
            id: "workflow-one".into(),
            name: "shared-name".into(),
            description: None,
            yaml_content: "remote definition".into(),
            version: 1,
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
        });
        db.with_conn(|connection| {
            connection
                .execute(
                    "INSERT INTO workflows
                        (id, name, yaml_content, enabled, version)
                     VALUES ('workflow-one', 'shared-name', 'local definition', 0, 1)",
                    [],
                )
                .unwrap();
            let error = validate_local_scopes(connection, &snapshot).unwrap_err();
            assert!(error.to_string().contains("outside this Project"));
        });
    }

    #[test]
    fn imported_message_order_keeps_parents_before_children() {
        let records = [
            ("a-child", Some("z-parent"), "2026-01-01 00:00:00"),
            ("z-parent", None, "2026-01-01 00:00:01"),
        ];
        let order = message_import_order(records.into_iter()).unwrap();
        assert_eq!(order, vec![1, 0]);
    }

    #[test]
    fn import_allows_a_conversation_message_task_link_to_be_cleared() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("xpressclaw.yaml");
        let db = Database::open_memory().unwrap();
        insert_project_data(&db);
        db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE conversation_messages
                     SET linked_task_id = 'task-one'
                     WHERE content = 'one'",
                    [],
                )
                .unwrap();
        });
        let config = Config {
            agents: vec![AgentConfig {
                name: "atlas".into(),
                backend: "codex".into(),
                ..AgentConfig::default()
            }],
            ..Config::default()
        };
        config.save(&config_path).unwrap();
        let mut snapshot = export_snapshot(&db, &config, &manifest()).unwrap();
        let message = snapshot
            .conversation_messages
            .iter_mut()
            .find(|message| message.content == "one")
            .unwrap();
        assert_eq!(message.linked_task_id.as_deref(), Some("task-one"));
        message.linked_task_id = None;

        let mut loaded = Config::load(&config_path).unwrap();
        import_snapshot(&db, &mut loaded, &config_path, directory.path(), &snapshot).unwrap();

        let linked_task_id: Option<String> = db
            .with_conn(|connection| {
                connection.query_row(
                    "SELECT linked_task_id FROM conversation_messages WHERE content = 'one'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert!(linked_task_id.is_none());
    }

    #[test]
    fn import_rejects_a_cycle_created_by_retained_local_dependencies() {
        let db = Database::open_memory().unwrap();
        insert_project_data(&db);
        db.with_conn(|connection| {
            connection
                .execute_batch(
                    "INSERT INTO tasks (id, title, project_id)
                     VALUES ('task-two', 'Second task', 'project-one');
                     INSERT INTO task_dependencies (task_id, depends_on_id)
                     VALUES ('task-two', 'task-one');",
                )
                .unwrap();
        });
        let config = Config {
            agents: vec![AgentConfig {
                name: "atlas".into(),
                backend: "codex".into(),
                ..AgentConfig::default()
            }],
            ..Config::default()
        };
        let mut snapshot = export_snapshot(&db, &config, &manifest()).unwrap();
        snapshot.task_dependencies = vec![PortableTaskDependency {
            task_id: "task-one".into(),
            depends_on_id: "task-two".into(),
        }];

        db.with_conn(|connection| {
            let transaction = connection.unchecked_transaction().unwrap();
            let error = import_transaction(&transaction, &snapshot).unwrap_err();
            assert!(error
                .to_string()
                .contains("dependency graph contains a cycle"));
        });
    }
}
