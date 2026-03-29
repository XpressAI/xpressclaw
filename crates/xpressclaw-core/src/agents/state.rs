use serde::{Deserialize, Serialize};

/// What the user/system wants for this agent. Persisted in the DB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredStatus {
    Running,
    Stopped,
}

impl std::fmt::Display for DesiredStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

impl std::str::FromStr for DesiredStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "stopped" => Ok(Self::Stopped),
            other => Err(format!("invalid desired status: {other}")),
        }
    }
}

/// What Docker reports for this agent right now. Never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedStatus {
    /// Container is running and healthy.
    Running,
    /// Container exists but is not running (exited, paused, etc.)
    Exited,
    /// No container found for this agent.
    NotFound,
    /// Docker daemon is not available.
    DockerUnavailable,
}

impl std::fmt::Display for ObservedStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Exited => write!(f, "exited"),
            Self::NotFound => write!(f, "not_found"),
            Self::DockerUnavailable => write!(f, "docker_unavailable"),
        }
    }
}

/// Compute the user-facing status from desired + observed.
/// This is the backward-compatible `status` field in the API response.
pub fn compute_status(desired: &DesiredStatus, observed: &ObservedStatus) -> &'static str {
    match (desired, observed) {
        (DesiredStatus::Running, ObservedStatus::Running) => "running",
        (DesiredStatus::Running, _) => "starting",
        (DesiredStatus::Stopped, ObservedStatus::Running) => "stopping",
        (DesiredStatus::Stopped, _) => "stopped",
    }
}

// Keep the old enum around until all callers are migrated.
// TODO: remove once the reconciler handles all lifecycle transitions.
#[deprecated(note = "use DesiredStatus + ObservedStatus instead")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Stopped,
    Starting,
    Running,
    Paused,
    Error(String),
}

#[allow(deprecated)]
impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => write!(f, "stopped"),
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Paused => write!(f, "paused"),
            Self::Error(msg) => write!(f, "error: {msg}"),
        }
    }
}
