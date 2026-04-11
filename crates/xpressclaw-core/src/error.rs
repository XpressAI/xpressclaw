use thiserror::Error;

/// Root error type for all xpressclaw errors.
#[derive(Error, Debug)]
pub enum Error {
    // Configuration
    #[error("configuration error: {0}")]
    Config(String),

    #[error("configuration file not found: {path}")]
    ConfigNotFound { path: String },

    #[error("configuration validation failed: {0}")]
    ConfigValidation(String),

    // Agent
    #[error("agent error: {0}")]
    Agent(String),

    #[error("agent not found: {name}")]
    AgentNotFound { name: String },

    #[error("agent already running: {name}")]
    AgentAlreadyRunning { name: String },

    #[error("agent not running: {name}")]
    AgentNotRunning { name: String },

    #[error("backend error: {0}")]
    Backend(String),

    #[error("backend not found: {name}")]
    BackendNotFound { name: String },

    // Memory
    #[error("memory error: {0}")]
    Memory(String),

    #[error("memory not found: {id}")]
    MemoryNotFound { id: String },

    #[error("embedding error: {0}")]
    Embedding(String),

    // Budget
    #[error("budget error: {0}")]
    Budget(String),

    #[error("budget exceeded for agent {agent_id}: {limit_type} limit ${limit:.2} (current: ${current:.2})")]
    BudgetExceeded {
        agent_id: String,
        limit_type: String,
        limit: f64,
        current: f64,
    },

    #[error("rate limit exceeded: {0}")]
    RateLimit(String),

    // Tasks
    #[error("task error: {0}")]
    Task(String),

    #[error("task not found: {id}")]
    TaskNotFound { id: String },

    #[error("schedule not found: {id}")]
    ScheduleNotFound { id: String },

    #[error("SOP error: {0}")]
    Sop(String),

    #[error("SOP not found: {name}")]
    SopNotFound { name: String },

    // Tools
    #[error("tool error: {0}")]
    Tool(String),

    #[error("tool not found: {name}")]
    ToolNotFound { name: String },

    #[error("tool permission denied: {0}")]
    ToolPermission(String),

    #[error("tool execution failed: {0}")]
    ToolExecution(String),

    // Conversations
    #[error("conversation not found: {id}")]
    ConversationNotFound { id: String },

    #[error("conversation error: {0}")]
    Conversation(String),

    // Database
    #[error("database error: {0}")]
    Database(String),

    #[error("migration failed (target v{version}): {message}")]
    Migration { version: u32, message: String },

    // Connectors
    #[error("connector error: {0}")]
    Connector(String),

    #[error("connector not found: {id}")]
    ConnectorNotFound { id: String },

    #[error("channel not found: {id}")]
    ChannelNotFound { id: String },

    // Workflows
    #[error("workflow error: {0}")]
    Workflow(String),

    #[error("workflow not found: {id}")]
    WorkflowNotFound { id: String },

    #[error("workflow instance not found: {id}")]
    WorkflowInstanceNotFound { id: String },

    // LLM
    #[error("LLM error: {0}")]
    Llm(String),

    #[error("LLM provider not found: {name}")]
    LlmProviderNotFound { name: String },
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Database(e.to_string())
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(e: serde_yaml::Error) -> Self {
        Error::Config(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Config(e.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Llm(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
