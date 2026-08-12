use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::db::task_search_key;
use crate::error::{Error, Result};
use crate::sync::manifest::validate_identifier;
use crate::workflows::definition::WorkflowDefinition;

pub const STORE_VERSION: u32 = 1;
const STORE_DESCRIPTOR: &str = ".xpressclaw-store.yml";
const PROJECT_FILE: &str = "project.yml";
const MAX_RECORD_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RECORDS_PER_KIND: usize = 100_000;

const GENERATED_DIRS: &[&str] = &[
    "agents",
    "tasks",
    "task-dependencies",
    "task-messages",
    "conversations",
    "conversation-participants",
    "conversation-messages",
    "workflows",
    "memory-notes",
    "memory-links",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StoreDescriptor {
    pub version: u32,
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableProject {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableAgent {
    pub id: String,
    pub name: String,
    pub backend: String,
    #[serde(default)]
    pub settings: PortableAgentSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PortableAgentSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm: Option<PortableLlmSettings>,
    pub runner: PortableRunnerSettings,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<PortableBudgetSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<PortableRateLimitSettings>,
    pub wake_on: Vec<PortableWakeOnSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PortableLlmSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PortableRunnerSettings {
    pub kind: String,
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub session_config: BTreeMap<String, Value>,
    /// Accepted for compatibility with early version 1 stores. Selections are
    /// ignored on import and new snapshots leave this empty because attachment
    /// to a local, potentially credential-bearing MCP definition is local-only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,
    pub startup_commands: Vec<String>,
    pub command: Vec<String>,
}

impl Default for PortableRunnerSettings {
    fn default() -> Self {
        Self {
            kind: "auto".to_string(),
            image: String::new(),
            project_name: None,
            model: None,
            session_config: BTreeMap::new(),
            mcp_servers: Vec::new(),
            startup_commands: Vec::new(),
            command: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableBudgetSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_task: Option<String>,
    pub on_exceeded: String,
    pub fallback_model: String,
    pub warn_at_percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableRateLimitSettings {
    pub requests_per_minute: u32,
    pub tokens_per_minute: u32,
    pub concurrent_requests: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PortableWakeOnSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableTask {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: String,
    pub priority: i32,
    #[serde(default = "default_task_type")]
    pub task_type: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

fn default_task_type() -> String {
    "normal".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableTaskDependency {
    pub task_id: String,
    pub depends_on_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableTaskMessage {
    pub record_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_record_id: Option<String>,
    pub task_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableConversation {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableParticipant {
    pub conversation_id: String,
    pub participant_type: String,
    pub participant_id: String,
    pub joined_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableConversationMessage {
    pub record_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_record_id: Option<String>,
    pub conversation_id: String,
    pub sender_type: String,
    pub sender_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    pub content: String,
    pub message_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_task_id: Option<String>,
    #[serde(default = "empty_object", skip_serializing_if = "is_empty_object")]
    pub metadata: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableWorkflow {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub yaml_content: String,
    pub version: u32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableMemoryNote {
    pub id: String,
    pub title: String,
    pub body: String,
    pub summary: String,
    pub note_type: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task_id: Option<String>,
    pub created_by: String,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableMemoryLink {
    pub from_note_id: String,
    pub to_note_id: String,
    pub link_type: String,
    pub strength: f64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortableSnapshot {
    pub descriptor: StoreDescriptor,
    pub project: PortableProject,
    #[serde(default)]
    pub agents: Vec<PortableAgent>,
    #[serde(default)]
    pub tasks: Vec<PortableTask>,
    #[serde(default)]
    pub task_dependencies: Vec<PortableTaskDependency>,
    #[serde(default)]
    pub task_messages: Vec<PortableTaskMessage>,
    #[serde(default)]
    pub conversations: Vec<PortableConversation>,
    #[serde(default)]
    pub participants: Vec<PortableParticipant>,
    #[serde(default)]
    pub conversation_messages: Vec<PortableConversationMessage>,
    #[serde(default)]
    pub workflows: Vec<PortableWorkflow>,
    #[serde(default)]
    pub memory_notes: Vec<PortableMemoryNote>,
    #[serde(default)]
    pub memory_links: Vec<PortableMemoryLink>,
}

impl PortableSnapshot {
    pub fn load(root: &Path, expected_project_id: &str) -> Result<Self> {
        ensure_real_directory(root, "synchronization store")?;
        validate_store_entries(root)?;
        let descriptor: StoreDescriptor = read_yaml(&root.join(STORE_DESCRIPTOR))?;
        let project: PortableProject = read_yaml(&root.join(PROJECT_FILE))?;
        let mut snapshot = Self {
            descriptor,
            project,
            agents: read_records(root, "agents")?,
            tasks: read_records(root, "tasks")?,
            task_dependencies: read_records(root, "task-dependencies")?,
            task_messages: read_records(root, "task-messages")?,
            conversations: read_records(root, "conversations")?,
            participants: read_records(root, "conversation-participants")?,
            conversation_messages: read_records(root, "conversation-messages")?,
            workflows: read_records(root, "workflows")?,
            memory_notes: read_records(root, "memory-notes")?,
            memory_links: read_records(root, "memory-links")?,
        };
        snapshot.normalize();
        snapshot.validate(expected_project_id)?;
        reject_snapshot_secrets(&snapshot)?;
        Ok(snapshot)
    }

    pub fn write(&mut self, root: &Path) -> Result<()> {
        self.normalize();
        let project_id = self.project.id.clone();
        self.validate_for_sync(&project_id)?;
        prepare_store_root(root)?;
        write_yaml(&root.join(STORE_DESCRIPTOR), &self.descriptor)?;
        write_yaml(&root.join(PROJECT_FILE), &self.project)?;
        write_records(root, "agents", &self.agents, |value| value.id.clone())?;
        write_records(root, "tasks", &self.tasks, |value| value.id.clone())?;
        write_records(
            root,
            "task-dependencies",
            &self.task_dependencies,
            |value| dependency_key(&value.task_id, &value.depends_on_id),
        )?;
        write_records(root, "task-messages", &self.task_messages, |value| {
            value.record_id.clone()
        })?;
        write_records(root, "conversations", &self.conversations, |value| {
            value.id.clone()
        })?;
        write_records(
            root,
            "conversation-participants",
            &self.participants,
            |value| {
                participant_key(
                    &value.conversation_id,
                    &value.participant_type,
                    &value.participant_id,
                )
            },
        )?;
        write_records(
            root,
            "conversation-messages",
            &self.conversation_messages,
            |value| value.record_id.clone(),
        )?;
        write_records(root, "workflows", &self.workflows, |value| value.id.clone())?;
        write_records(root, "memory-notes", &self.memory_notes, |value| {
            value.id.clone()
        })?;
        write_records(root, "memory-links", &self.memory_links, |value| {
            memory_link_key(value)
        })?;
        Ok(())
    }

    pub fn digest(&self) -> Result<String> {
        let mut normalized = self.clone();
        normalized.normalize();
        let bytes = serde_json::to_vec(&normalized)
            .map_err(|error| Error::Sync(format!("failed to hash portable state: {error}")))?;
        Ok(hex_digest(&bytes))
    }

    pub fn counts(&self) -> SnapshotCounts {
        SnapshotCounts {
            agents: self.agents.len(),
            tasks: self.tasks.len(),
            task_messages: self.task_messages.len(),
            conversations: self.conversations.len(),
            conversation_messages: self.conversation_messages.len(),
            workflows: self.workflows.len(),
            memory_notes: self.memory_notes.len(),
        }
    }

    pub(super) fn validate_for_sync(&self, expected_project_id: &str) -> Result<()> {
        reject_snapshot_secrets(self)?;
        self.validate(expected_project_id)
    }

    fn normalize(&mut self) {
        self.agents.sort_by(|left, right| left.id.cmp(&right.id));
        self.tasks.sort_by(|left, right| left.id.cmp(&right.id));
        self.task_dependencies.sort_by(|left, right| {
            (&left.task_id, &left.depends_on_id).cmp(&(&right.task_id, &right.depends_on_id))
        });
        self.task_messages
            .sort_by(|left, right| left.record_id.cmp(&right.record_id));
        self.conversations
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.participants.sort_by(|left, right| {
            (
                &left.conversation_id,
                &left.participant_type,
                &left.participant_id,
            )
                .cmp(&(
                    &right.conversation_id,
                    &right.participant_type,
                    &right.participant_id,
                ))
        });
        self.conversation_messages
            .sort_by(|left, right| left.record_id.cmp(&right.record_id));
        self.workflows.sort_by(|left, right| left.id.cmp(&right.id));
        for note in &mut self.memory_notes {
            note.tags.sort();
            note.tags.dedup();
        }
        self.memory_notes
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.memory_links.sort_by(|left, right| {
            (&left.from_note_id, &left.to_note_id, &left.link_type).cmp(&(
                &right.from_note_id,
                &right.to_note_id,
                &right.link_type,
            ))
        });
    }

    fn validate(&self, expected_project_id: &str) -> Result<()> {
        for (label, count) in [
            ("agents", self.agents.len()),
            ("tasks", self.tasks.len()),
            ("task-dependencies", self.task_dependencies.len()),
            ("task-messages", self.task_messages.len()),
            ("conversations", self.conversations.len()),
            ("conversation-participants", self.participants.len()),
            ("conversation-messages", self.conversation_messages.len()),
            ("workflows", self.workflows.len()),
            ("memory-notes", self.memory_notes.len()),
            ("memory-links", self.memory_links.len()),
        ] {
            if count > MAX_RECORDS_PER_KIND {
                return Err(Error::Sync(format!(
                    "{label} contains more than {MAX_RECORDS_PER_KIND} records"
                )));
            }
        }
        if self.descriptor.version != STORE_VERSION {
            return Err(Error::Sync(format!(
                "unsupported store version {}; this release supports version {STORE_VERSION}",
                self.descriptor.version
            )));
        }
        if self.descriptor.project_id != expected_project_id
            || self.project.id != expected_project_id
        {
            return Err(Error::Sync(format!(
                "store Project ID does not match manifest Project '{expected_project_id}'"
            )));
        }
        validate_identifier("project.id", &self.project.id)?;
        validate_text("project.name", &self.project.name, 500, false)?;
        validate_optional_text(
            "project.description",
            self.project.description.as_deref(),
            100_000,
        )?;
        validate_optional_text("project.icon", self.project.icon.as_deref(), 1_000)?;
        validate_timestamp("project.created_at", &self.project.created_at)?;
        validate_timestamp("project.updated_at", &self.project.updated_at)?;

        let agent_ids = unique_ids("Agent", self.agents.iter().map(|value| &value.id))?;
        for agent in &self.agents {
            validate_identifier("Agent ID", &agent.id)?;
            validate_text("Agent name", &agent.name, 500, false)?;
            validate_text("Agent backend", &agent.backend, 200, false)?;
            if let Some(created_at) = agent.created_at.as_deref() {
                validate_timestamp("Agent created_at", created_at)?;
            }
            if agent.settings.runner.session_config.len() > 200 {
                return Err(Error::Sync(format!(
                    "Agent '{}' has too many runner session settings",
                    agent.id
                )));
            }
            if agent.settings.budget.as_ref().is_some_and(|budget| {
                !["pause", "alert", "degrade", "stop"].contains(&budget.on_exceeded.as_str())
                    || budget.warn_at_percent > 100
            }) {
                return Err(Error::Sync(format!(
                    "Agent '{}' has invalid budget settings",
                    agent.id
                )));
            }
        }

        let conversation_ids = unique_ids(
            "Conversation",
            self.conversations.iter().map(|value| &value.id),
        )?;
        for conversation in &self.conversations {
            validate_optional_text("Conversation title", conversation.title.as_deref(), 10_000)?;
            validate_optional_text("Conversation icon", conversation.icon.as_deref(), 10_000)?;
            validate_timestamp("Conversation created_at", &conversation.created_at)?;
            validate_timestamp("Conversation updated_at", &conversation.updated_at)?;
            if let Some(last_message_at) = conversation.last_message_at.as_deref() {
                validate_timestamp("Conversation last_message_at", last_message_at)?;
            }
        }

        let task_ids = unique_ids("task", self.tasks.iter().map(|value| &value.id))?;
        for task in &self.tasks {
            validate_text("task title", &task.title, 10_000, false)?;
            validate_optional_text("task description", task.description.as_deref(), 1_500_000)?;
            if ![
                "pending",
                "in_progress",
                "waiting_for_input",
                "blocked",
                "completed",
                "cancelled",
            ]
            .contains(&task.status.as_str())
            {
                return Err(Error::Sync(format!(
                    "task '{}' has invalid status '{}'",
                    task.id, task.status
                )));
            }
            if task.hidden || !task.task_type.eq_ignore_ascii_case("normal") {
                return Err(Error::Sync(format!(
                    "task '{}' is hidden or has non-portable task_type '{}'",
                    task.id, task.task_type,
                )));
            }
            validate_optional_reference(
                "task Agent",
                &task.id,
                task.agent_id.as_ref(),
                &agent_ids,
            )?;
            validate_optional_reference(
                "parent task",
                &task.id,
                task.parent_task_id.as_ref(),
                &task_ids,
            )?;
            validate_optional_reference(
                "task Conversation",
                &task.id,
                task.conversation_id.as_ref(),
                &conversation_ids,
            )?;
            validate_timestamp("task created_at", &task.created_at)?;
            validate_timestamp("task updated_at", &task.updated_at)?;
            if let Some(completed_at) = task.completed_at.as_deref() {
                validate_timestamp("task completed_at", completed_at)?;
            }
        }
        validate_parent_cycles(
            "task",
            self.tasks
                .iter()
                .map(|value| (&value.id, value.parent_task_id.as_ref())),
        )?;

        let mut dependency_keys = HashSet::new();
        for dependency in &self.task_dependencies {
            if !task_ids.contains(&dependency.task_id)
                || !task_ids.contains(&dependency.depends_on_id)
            {
                return Err(Error::Sync(
                    "task dependency references a task outside this Project snapshot".into(),
                ));
            }
            if dependency.task_id == dependency.depends_on_id
                || !dependency_keys.insert((&dependency.task_id, &dependency.depends_on_id))
            {
                return Err(Error::Sync(
                    "task dependencies contain a self-edge or duplicate edge".into(),
                ));
            }
        }
        validate_dependency_cycles(
            &task_ids,
            self.task_dependencies
                .iter()
                .map(|value| (&value.task_id, &value.depends_on_id)),
        )?;

        let participant_keys = self
            .participants
            .iter()
            .map(|value| {
                (
                    &value.conversation_id,
                    &value.participant_type,
                    &value.participant_id,
                )
            })
            .collect::<HashSet<_>>();
        if participant_keys.len() != self.participants.len() {
            return Err(Error::Sync("duplicate Conversation participant".into()));
        }
        for participant in &self.participants {
            if !conversation_ids.contains(&participant.conversation_id) {
                return Err(Error::Sync(
                    "Conversation participant references an unknown Conversation".into(),
                ));
            }
            if !["agent", "user"].contains(&participant.participant_type.as_str()) {
                return Err(Error::Sync(format!(
                    "invalid participant type '{}'",
                    participant.participant_type
                )));
            }
            if participant.participant_type == "agent"
                && !agent_ids.contains(&participant.participant_id)
            {
                return Err(Error::Sync(
                    "Conversation participant references an Agent outside this Project snapshot"
                        .into(),
                ));
            }
            validate_text(
                "Conversation participant ID",
                &participant.participant_id,
                500,
                false,
            )?;
            validate_timestamp("participant joined_at", &participant.joined_at)?;
        }

        validate_message_graph(
            "Conversation",
            self.conversation_messages.iter().map(|value| {
                (
                    &value.record_id,
                    value.parent_record_id.as_ref(),
                    &value.conversation_id,
                )
            }),
            &conversation_ids,
        )?;
        for message in &self.conversation_messages {
            if let Some(task_id) = message.linked_task_id.as_ref() {
                if !task_ids.contains(task_id) {
                    return Err(Error::Sync(format!(
                        "Conversation message '{}' links to a task outside this Project snapshot",
                        message.record_id
                    )));
                }
            }
            validate_text(
                "Conversation message sender_type",
                &message.sender_type,
                100,
                false,
            )?;
            if !["user", "agent", "system"].contains(&message.sender_type.as_str()) {
                return Err(Error::Sync(format!(
                    "Conversation message '{}' has invalid sender_type '{}'",
                    message.record_id, message.sender_type
                )));
            }
            validate_text(
                "Conversation message sender_id",
                &message.sender_id,
                500,
                false,
            )?;
            validate_text(
                "Conversation message type",
                &message.message_type,
                100,
                false,
            )?;
            validate_optional_text(
                "Conversation message sender_name",
                message.sender_name.as_deref(),
                500,
            )?;
            validate_text(
                "Conversation message content",
                &message.content,
                1_500_000,
                true,
            )?;
            if !message.metadata.is_object() {
                return Err(Error::Sync(format!(
                    "Conversation message '{}' metadata must be an object",
                    message.record_id
                )));
            }
            validate_timestamp("Conversation message created_at", &message.created_at)?;
        }

        validate_message_graph(
            "task",
            self.task_messages.iter().map(|value| {
                (
                    &value.record_id,
                    value.parent_record_id.as_ref(),
                    &value.task_id,
                )
            }),
            &task_ids,
        )?;
        for message in &self.task_messages {
            validate_text("task message role", &message.role, 100, false)?;
            if !["user", "assistant", "system"].contains(&message.role.as_str()) {
                return Err(Error::Sync(format!(
                    "task message '{}' has invalid role '{}'",
                    message.record_id, message.role
                )));
            }
            validate_text("task message content", &message.content, 1_500_000, true)?;
            validate_timestamp("task message created_at", &message.created_at)?;
        }

        unique_ids("workflow", self.workflows.iter().map(|value| &value.id))?;
        let mut workflow_names = HashSet::new();
        for workflow in &self.workflows {
            validate_text("workflow name", &workflow.name, 500, false)?;
            validate_optional_text(
                "workflow description",
                workflow.description.as_deref(),
                100_000,
            )?;
            validate_text(
                "workflow YAML definition",
                &workflow.yaml_content,
                1_500_000,
                false,
            )?;
            if !workflow_names.insert(&workflow.name) {
                return Err(Error::Sync(format!(
                    "duplicate workflow name '{}'",
                    workflow.name
                )));
            }
            let definition = WorkflowDefinition::parse(&workflow.yaml_content)?;
            definition.validate()?;
            if definition.name != workflow.name || definition.version != workflow.version {
                return Err(Error::Sync(format!(
                    "workflow '{}' metadata does not match its YAML definition",
                    workflow.id
                )));
            }
            validate_timestamp("workflow created_at", &workflow.created_at)?;
            validate_timestamp("workflow updated_at", &workflow.updated_at)?;
        }

        let note_ids = unique_ids(
            "memory note",
            self.memory_notes.iter().map(|value| &value.id),
        )?;
        for note in &self.memory_notes {
            validate_required_chars("memory note title", &note.title, 200)?;
            validate_required_chars("memory note body", &note.body, 100_000)?;
            validate_required_chars("memory note summary", &note.summary, 1_000)?;
            if ![
                "decision",
                "convention",
                "procedure",
                "fact",
                "warning",
                "question",
            ]
            .contains(&note.note_type.as_str())
                || !["inbox", "evergreen", "archived"].contains(&note.state.as_str())
                || !["user", "agent", "upkeep"].contains(&note.created_by.as_str())
            {
                return Err(Error::Sync(format!(
                    "memory note '{}' contains an invalid enum value",
                    note.id
                )));
            }
            validate_optional_reference(
                "memory source task",
                &note.id,
                note.source_task_id.as_ref(),
                &task_ids,
            )?;
            validate_timestamp("memory note created_at", &note.created_at)?;
            validate_timestamp("memory note updated_at", &note.updated_at)?;
            if note.tags.len() > 32 {
                return Err(Error::Sync(format!(
                    "memory note '{}' has more than 32 tags",
                    note.id
                )));
            }
            let mut tag_keys = HashSet::new();
            for tag in &note.tags {
                validate_required_chars("memory note tag", tag, 64)?;
                if !tag_keys.insert(task_search_key(tag)) {
                    return Err(Error::Sync(format!(
                        "memory note '{}' has duplicate normalized tags",
                        note.id
                    )));
                }
            }
        }
        let mut memory_link_keys = HashSet::new();
        for link in &self.memory_links {
            if !note_ids.contains(&link.from_note_id)
                || !note_ids.contains(&link.to_note_id)
                || link.from_note_id == link.to_note_id
                || !link.strength.is_finite()
                || !(0.0..=1.0).contains(&link.strength)
                || ![
                    "related",
                    "supports",
                    "contradicts",
                    "supersedes",
                    "depends_on",
                    "example_of",
                ]
                .contains(&link.link_type.as_str())
                || !memory_link_keys.insert((&link.from_note_id, &link.to_note_id, &link.link_type))
            {
                return Err(Error::Sync(
                    "invalid or duplicate project-memory link".into(),
                ));
            }
            validate_timestamp("memory link created_at", &link.created_at)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct SnapshotCounts {
    pub agents: usize,
    pub tasks: usize,
    pub task_messages: usize,
    pub conversations: usize,
    pub conversation_messages: usize,
    pub workflows: usize,
    pub memory_notes: usize,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
}

fn dependency_key(task_id: &str, depends_on_id: &str) -> String {
    format!("{task_id}\0{depends_on_id}")
}

fn participant_key(conversation_id: &str, kind: &str, participant_id: &str) -> String {
    format!("{conversation_id}\0{kind}\0{participant_id}")
}

fn memory_link_key(link: &PortableMemoryLink) -> String {
    format!(
        "{}\0{}\0{}",
        link.from_note_id, link.to_note_id, link.link_type
    )
}

fn record_filename(key: &str) -> String {
    format!("{}.yml", hex_digest(key.as_bytes()))
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn prepare_store_root(root: &Path) -> Result<()> {
    if root.exists() {
        ensure_real_directory(root, "synchronization store")?;
        validate_store_entries(root)?;
    } else {
        fs::create_dir_all(root).map_err(|error| {
            Error::Sync(format!(
                "failed to create synchronization store {}: {error}",
                root.display()
            ))
        })?;
    }
    for directory in GENERATED_DIRS {
        let path = root.join(directory);
        if path.exists() {
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                Error::Sync(format!("failed to inspect {}: {error}", path.display()))
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(Error::Sync(format!(
                    "refusing to replace non-directory or symlink {}",
                    path.display()
                )));
            }
            fs::remove_dir_all(&path).map_err(|error| {
                Error::Sync(format!("failed to replace {}: {error}", path.display()))
            })?;
        }
        fs::create_dir(&path).map_err(|error| {
            Error::Sync(format!("failed to create {}: {error}", path.display()))
        })?;
    }
    Ok(())
}

fn validate_store_entries(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)
        .map_err(|error| Error::Sync(format!("failed to read {}: {error}", root.display())))?
    {
        let entry = entry
            .map_err(|error| Error::Sync(format!("failed to read {}: {error}", root.display())))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            Error::Sync("synchronization store contains a non-portable filename".into())
        })?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            Error::Sync(format!(
                "failed to inspect {}: {error}",
                entry.path().display()
            ))
        })?;
        let allowed_file = [STORE_DESCRIPTOR, PROJECT_FILE].contains(&name) && metadata.is_file();
        let allowed_directory = GENERATED_DIRS.contains(&name) && metadata.is_dir();
        if metadata.file_type().is_symlink() || !(allowed_file || allowed_directory) {
            return Err(Error::Sync(format!(
                "synchronization store contains unsupported entry '{name}'"
            )));
        }
    }
    Ok(())
}

fn ensure_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| Error::Sync(format!("failed to inspect {}: {error}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::Sync(format!(
            "{label} {} must be a real directory, not a symlink",
            path.display()
        )));
    }
    Ok(())
}

fn write_records<T, K>(root: &Path, directory: &str, records: &[T], key: K) -> Result<()>
where
    T: Serialize,
    K: Fn(&T) -> String,
{
    if records.len() > MAX_RECORDS_PER_KIND {
        return Err(Error::Sync(format!(
            "{directory} contains more than {MAX_RECORDS_PER_KIND} records"
        )));
    }
    for record in records {
        let key = key(record);
        write_yaml(&root.join(directory).join(record_filename(&key)), record)?;
    }
    Ok(())
}

fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let yaml = serde_yaml::to_string(value)
        .map_err(|error| Error::Sync(format!("failed to serialize {}: {error}", path.display())))?;
    if yaml.len() as u64 > MAX_RECORD_BYTES {
        return Err(Error::Sync(format!(
            "{} exceeds the 2 MiB record limit",
            path.display()
        )));
    }
    reject_secret_text(&yaml, &path.display().to_string())?;
    fs::write(path, yaml)
        .map_err(|error| Error::Sync(format!("failed to write {}: {error}", path.display())))
}

fn read_records<T: DeserializeOwned>(root: &Path, directory: &str) -> Result<Vec<T>> {
    let path = root.join(directory);
    if !path.exists() {
        return Ok(Vec::new());
    }
    ensure_real_directory(&path, "record directory")?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&path)
        .map_err(|error| Error::Sync(format!("failed to read {}: {error}", path.display())))?
    {
        if entries.len() == MAX_RECORDS_PER_KIND {
            return Err(Error::Sync(format!(
                "{} contains more than {MAX_RECORDS_PER_KIND} records",
                path.display()
            )));
        }
        entries.push(
            entry.map_err(|error| {
                Error::Sync(format!("failed to read {}: {error}", path.display()))
            })?,
        );
    }
    entries.sort_by_key(|entry| entry.file_name());
    entries
        .into_iter()
        .map(|entry| {
            let entry_path = entry.path();
            let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
                Error::Sync(format!(
                    "failed to inspect {}: {error}",
                    entry_path.display()
                ))
            })?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || entry_path.extension().and_then(|value| value.to_str()) != Some("yml")
            {
                return Err(Error::Sync(format!(
                    "{} may contain only regular .yml record files",
                    path.display()
                )));
            }
            read_yaml(&entry_path)
        })
        .collect()
}

fn read_yaml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| Error::Sync(format!("failed to inspect {}: {error}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::Sync(format!(
            "{} must be a regular file, not a symlink",
            path.display()
        )));
    }
    if metadata.len() > MAX_RECORD_BYTES {
        return Err(Error::Sync(format!(
            "{} exceeds the 2 MiB record limit",
            path.display()
        )));
    }
    let contents = fs::read_to_string(path)
        .map_err(|error| Error::Sync(format!("failed to read {}: {error}", path.display())))?;
    reject_secret_text(&contents, &path.display().to_string())?;
    serde_yaml::from_str(&contents)
        .map_err(|error| Error::Sync(format!("invalid {}: {error}", path.display())))
}

fn unique_ids<'a>(label: &str, ids: impl Iterator<Item = &'a String>) -> Result<HashSet<String>> {
    let mut unique = HashSet::new();
    for id in ids {
        validate_identifier(&format!("{label} ID"), id)?;
        if !unique.insert(id.clone()) {
            return Err(Error::Sync(format!("duplicate {label} ID '{id}'")));
        }
    }
    Ok(unique)
}

fn validate_text(field: &str, value: &str, max: usize, allow_empty: bool) -> Result<()> {
    if (!allow_empty && value.trim().is_empty()) || value.len() > max {
        return Err(Error::Sync(format!(
            "{field} must contain {} to {max} bytes",
            usize::from(!allow_empty)
        )));
    }
    if value.contains('\0') {
        return Err(Error::Sync(format!("{field} contains a NUL byte")));
    }
    Ok(())
}

fn validate_required_chars(field: &str, value: &str, max: usize) -> Result<()> {
    validate_text(field, value, max.saturating_mul(4), false)?;
    if value.chars().count() > max {
        return Err(Error::Sync(format!(
            "{field} must contain between 1 and {max} characters"
        )));
    }
    Ok(())
}

fn validate_timestamp(field: &str, value: &str) -> Result<()> {
    validate_text(field, value, 80, false)?;
    let sqlite = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f").is_ok();
    let rfc3339 = chrono::DateTime::parse_from_rfc3339(value).is_ok();
    if !sqlite && !rfc3339 {
        return Err(Error::Sync(format!("{field} is not a valid timestamp")));
    }
    Ok(())
}

fn validate_optional_text(field: &str, value: Option<&str>, max: usize) -> Result<()> {
    if let Some(value) = value {
        validate_text(field, value, max, true)?;
    }
    Ok(())
}

fn validate_optional_reference(
    label: &str,
    owner: &str,
    reference: Option<&String>,
    valid: &HashSet<String>,
) -> Result<()> {
    if reference.is_some_and(|reference| !valid.contains(reference)) {
        return Err(Error::Sync(format!(
            "{label} on '{owner}' points outside this Project snapshot"
        )));
    }
    Ok(())
}

pub(super) fn validate_parent_cycles<'a>(
    label: &str,
    values: impl Iterator<Item = (&'a String, Option<&'a String>)>,
) -> Result<()> {
    let parents = values
        .map(|(id, parent)| (id.as_str(), parent.map(String::as_str)))
        .collect::<HashMap<_, _>>();
    let mut resolved = HashSet::new();
    for start in parents.keys().copied() {
        if resolved.contains(start) {
            continue;
        }
        let mut path = HashSet::new();
        let mut current = Some(start);
        while let Some(id) = current {
            if resolved.contains(id) {
                break;
            }
            if !path.insert(id) {
                return Err(Error::Sync(format!(
                    "{label} parent graph contains a cycle"
                )));
            }
            current = parents.get(id).copied().flatten();
        }
        resolved.extend(path);
    }
    Ok(())
}

pub(super) fn validate_dependency_cycles<'a>(
    task_ids: &'a HashSet<String>,
    dependencies: impl Iterator<Item = (&'a String, &'a String)>,
) -> Result<()> {
    let mut unmet = task_ids
        .iter()
        .map(|id| (id.as_str(), 0_usize))
        .collect::<HashMap<_, _>>();
    let mut dependents = HashMap::<&str, Vec<&str>>::new();
    for (task_id, depends_on_id) in dependencies {
        *unmet.entry(task_id.as_str()).or_default() += 1;
        dependents
            .entry(depends_on_id.as_str())
            .or_default()
            .push(task_id.as_str());
    }
    let mut ready = unmet
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect::<VecDeque<_>>();
    let mut visited = 0;
    while let Some(id) = ready.pop_front() {
        visited += 1;
        for dependent in dependents.get(id).into_iter().flatten() {
            let Some(count) = unmet.get_mut(dependent) else {
                return Err(Error::Sync(
                    "task dependency references an unknown task".into(),
                ));
            };
            *count -= 1;
            if *count == 0 {
                ready.push_back(dependent);
            }
        }
    }
    if visited != task_ids.len() {
        return Err(Error::Sync("task dependency graph contains a cycle".into()));
    }
    Ok(())
}

fn validate_message_graph<'a>(
    label: &str,
    values: impl Iterator<Item = (&'a String, Option<&'a String>, &'a String)>,
    owners: &HashSet<String>,
) -> Result<()> {
    let values = values.collect::<Vec<_>>();
    let ids = unique_ids(
        &format!("{label} message record"),
        values.iter().map(|(id, _, _)| *id),
    )?;
    let by_id = values
        .iter()
        .map(|(id, parent, owner)| (id.as_str(), (parent.map(String::as_str), owner.as_str())))
        .collect::<HashMap<_, _>>();
    for (id, parent, owner) in &values {
        if !owners.contains(*owner) {
            return Err(Error::Sync(format!(
                "{label} message '{id}' references an unknown owner"
            )));
        }
        if let Some(parent) = parent {
            if !ids.contains(*parent) {
                return Err(Error::Sync(format!(
                    "{label} message '{id}' references an unknown parent record"
                )));
            }
            if by_id
                .get(parent.as_str())
                .is_some_and(|(_, parent_owner)| *parent_owner != owner.as_str())
            {
                return Err(Error::Sync(format!(
                    "{label} message '{id}' has a parent belonging to another thread"
                )));
            }
        }
    }
    validate_parent_cycles(label, values.iter().map(|(id, parent, _)| (*id, *parent)))
}

fn reject_snapshot_secrets(snapshot: &PortableSnapshot) -> Result<()> {
    let json = serde_json::to_string(snapshot)
        .map_err(|error| Error::Sync(format!("failed to inspect portable state: {error}")))?;
    reject_secret_text(&json, "portable Project state")
}

fn reject_secret_text(contents: &str, label: &str) -> Result<()> {
    let lower = contents.to_ascii_lowercase();
    let fixed_markers = [
        "-----begin private key-----",
        "-----begin rsa private key-----",
        "-----begin openssh private key-----",
        "github_pat_",
        "ghp_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "sk-proj-",
        "sk-ant-",
    ];
    let bearer = lower.match_indices("bearer ").any(|(index, _)| {
        lower[index + 7..]
            .split_whitespace()
            .next()
            .is_some_and(|token| token.len() >= 16 && !is_placeholder(token))
    });
    let named_secret = ["api_key", "access_token", "client_secret", "password"]
        .iter()
        .any(|name| has_named_secret_assignment(&lower, name));
    if bearer || named_secret || fixed_markers.iter().any(|marker| lower.contains(marker)) {
        return Err(Error::Sync(format!(
            "{label} appears to contain a credential; remove it before synchronization"
        )));
    }
    Ok(())
}

fn has_named_secret_assignment(contents: &str, name: &str) -> bool {
    contents.match_indices(name).any(|(index, _)| {
        if contents[..index]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return false;
        }
        let suffix =
            contents[index + name.len()..].trim_start_matches([' ', '\t', '\\', '\"', '\'']);
        let Some(delimiter) = suffix.chars().next() else {
            return false;
        };
        if delimiter != ':' && delimiter != '=' {
            return false;
        }
        let value = suffix[delimiter.len_utf8()..]
            .trim_start_matches([' ', '\t', '\\', '\"', '\''])
            .split(|character| {
                [' ', '\t', '\r', '\n', '\\', '\"', '\'', ',', '}', ']'].contains(&character)
            })
            .next()
            .unwrap_or_default();
        value.len() >= 12 && !is_placeholder(value)
    })
}

fn is_placeholder(value: &str) -> bool {
    let raw = value.trim_matches(|character: char| {
        character.is_ascii_whitespace() || ['\"', '\'', ',', '}', ']'].contains(&character)
    });
    if raw.starts_with('$') || raw.starts_with("${") {
        return true;
    }
    let value = value
        .trim_matches(|character: char| !character.is_ascii_alphanumeric())
        .to_ascii_lowercase();
    value.is_empty()
        || value.contains("example")
        || value.contains("placeholder")
        || value.contains("redacted")
        || value.contains("your_token")
        || value.chars().all(|character| character == 'x')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> PortableSnapshot {
        PortableSnapshot {
            descriptor: StoreDescriptor {
                version: STORE_VERSION,
                project_id: "project-one".into(),
            },
            project: PortableProject {
                id: "project-one".into(),
                name: "Project One".into(),
                description: None,
                icon: None,
                created_at: "2026-01-01 00:00:00".into(),
                updated_at: "2026-01-01 00:00:00".into(),
            },
            agents: vec![PortableAgent {
                id: "atlas".into(),
                name: "Atlas".into(),
                backend: "codex".into(),
                settings: PortableAgentSettings::default(),
                created_at: None,
            }],
            tasks: Vec::new(),
            task_dependencies: Vec::new(),
            task_messages: Vec::new(),
            conversations: vec![PortableConversation {
                id: "conversation-one".into(),
                title: Some("Design".into()),
                icon: None,
                created_at: "2026-01-01 00:00:00".into(),
                updated_at: "2026-01-01 00:00:00".into(),
                last_message_at: None,
            }],
            participants: vec![PortableParticipant {
                conversation_id: "conversation-one".into(),
                participant_type: "agent".into(),
                participant_id: "atlas".into(),
                joined_at: "2026-01-01 00:00:00".into(),
            }],
            conversation_messages: vec![
                PortableConversationMessage {
                    record_id: "message-one".into(),
                    parent_record_id: None,
                    conversation_id: "conversation-one".into(),
                    sender_type: "user".into(),
                    sender_id: "local-user".into(),
                    sender_name: None,
                    content: "Hello".into(),
                    message_type: "message".into(),
                    linked_task_id: None,
                    metadata: empty_object(),
                    created_at: "2026-01-01 00:00:00".into(),
                },
                PortableConversationMessage {
                    record_id: "message-two".into(),
                    parent_record_id: Some("message-one".into()),
                    conversation_id: "conversation-one".into(),
                    sender_type: "agent".into(),
                    sender_id: "atlas".into(),
                    sender_name: Some("Atlas".into()),
                    content: "Hi".into(),
                    message_type: "message".into(),
                    linked_task_id: None,
                    metadata: empty_object(),
                    created_at: "2026-01-01 00:00:01".into(),
                },
            ],
            workflows: Vec::new(),
            memory_notes: Vec::new(),
            memory_links: Vec::new(),
        }
    }

    #[test]
    fn message_records_round_trip_as_separate_files_with_parents() {
        let directory = tempfile::tempdir().unwrap();
        let mut snapshot = snapshot();
        snapshot.write(directory.path()).unwrap();
        let loaded = PortableSnapshot::load(directory.path(), "project-one").unwrap();
        assert_eq!(loaded, snapshot);
        assert_eq!(
            fs::read_dir(directory.path().join("conversation-messages"))
                .unwrap()
                .count(),
            2
        );
    }

    #[test]
    fn message_graph_rejects_cross_thread_parent() {
        let mut snapshot = snapshot();
        snapshot.conversations.push(PortableConversation {
            id: "conversation-two".into(),
            title: None,
            icon: None,
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
            last_message_at: None,
        });
        snapshot.conversation_messages[1].conversation_id = "conversation-two".into();
        assert!(snapshot.validate("project-one").is_err());
    }

    #[test]
    fn publish_guard_rejects_bearer_credentials() {
        let mut snapshot = snapshot();
        snapshot.conversation_messages[0].content =
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz".into();
        assert!(reject_snapshot_secrets(&snapshot).is_err());
    }

    #[test]
    fn secret_guard_allows_environment_variable_references() {
        let mut snapshot = snapshot();
        snapshot.conversation_messages[0].content =
            "Use API_KEY=$OPENAI_API_KEY from the local environment".into();
        reject_snapshot_secrets(&snapshot).unwrap();
    }

    #[test]
    fn secret_guard_allows_security_terms_in_prose() {
        let mut snapshot = snapshot();
        snapshot.conversation_messages[0].content =
            "Document password authentication and client_secret rotation.".into();
        reject_snapshot_secrets(&snapshot).unwrap();
    }

    #[test]
    fn secret_guard_rejects_named_secret_assignments() {
        let mut snapshot = snapshot();
        snapshot.conversation_messages[0].content =
            r#"Configure {"password": "abcdefghijklmnop"}"#.into();
        assert!(reject_snapshot_secrets(&snapshot).is_err());
    }

    #[test]
    fn store_rejects_unknown_root_entries() {
        let directory = tempfile::tempdir().unwrap();
        let mut snapshot = snapshot();
        snapshot.write(directory.path()).unwrap();
        fs::write(
            directory.path().join("credentials.txt"),
            "not even a secret",
        )
        .unwrap();
        assert!(PortableSnapshot::load(directory.path(), "project-one").is_err());
    }

    #[test]
    fn publish_rejects_an_oversized_record() {
        let directory = tempfile::tempdir().unwrap();
        let mut snapshot = snapshot();
        snapshot.conversation_messages[0].metadata =
            serde_json::json!({"blob": "x".repeat(MAX_RECORD_BYTES as usize)});
        let error = snapshot.write(directory.path()).unwrap_err();
        assert!(error.to_string().contains("2 MiB record limit"));
    }

    #[test]
    fn dependency_graph_rejects_cycles() {
        let task_ids = HashSet::from(["one".to_string(), "two".to_string()]);
        let dependencies = [
            ("one".to_string(), "two".to_string()),
            ("two".to_string(), "one".to_string()),
        ];
        assert!(validate_dependency_cycles(
            &task_ids,
            dependencies
                .iter()
                .map(|(task, dependency)| (task, dependency)),
        )
        .is_err());
    }
}
