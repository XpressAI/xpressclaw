"""Custom exception hierarchy for XpressAI.

All XpressAI exceptions inherit from XpressAIError for easy catching.
"""

from typing import Any


class XpressAIError(Exception):
    """Base exception for all XpressAI errors."""

    def __init__(self, message: str, context: dict[str, Any] | None = None):
        super().__init__(message)
        self.message = message
        self.context = context or {}

    def __str__(self) -> str:
        if self.context:
            ctx = ", ".join(f"{k}={v}" for k, v in self.context.items())
            return f"{self.message} ({ctx})"
        return self.message


# Configuration Errors
class ConfigError(XpressAIError):
    """Error in configuration loading or validation."""

    pass


class ConfigNotFoundError(ConfigError):
    """Configuration file not found."""

    pass


class ConfigValidationError(ConfigError):
    """Configuration validation failed."""

    pass


# Agent Errors
class AgentError(XpressAIError):
    """Error related to agent operations."""

    pass


class AgentNotFoundError(AgentError):
    """Agent not found."""

    pass


class AgentAlreadyRunningError(AgentError):
    """Agent is already running."""

    pass


class AgentNotRunningError(AgentError):
    """Agent is not running."""

    pass


class BackendError(AgentError):
    """Error in agent backend."""

    pass


class BackendNotFoundError(BackendError):
    """Backend type not found."""

    pass


class BackendInitializationError(BackendError):
    """Backend failed to initialize."""

    pass


# Isolation Errors
class IsolationError(XpressAIError):
    """Error in isolation system."""

    pass


class ContainerError(IsolationError):
    """Error with Docker container."""

    pass


class ContainerNotFoundError(ContainerError):
    """Container not found."""

    pass


class ContainerStartError(ContainerError):
    """Failed to start container."""

    pass


# Memory Errors
class MemoryError(XpressAIError):
    """Error in memory system."""

    pass


class MemoryNotFoundError(MemoryError):
    """Memory not found."""

    pass


class EmbeddingError(MemoryError):
    """Error generating embeddings."""

    pass


# Budget Errors
class BudgetError(XpressAIError):
    """Error in budget system."""

    pass


class BudgetExceededError(BudgetError):
    """Budget limit exceeded."""

    def __init__(
        self,
        message: str,
        agent_id: str,
        limit_type: str,
        limit: float,
        current: float,
    ):
        super().__init__(
            message,
            {
                "agent_id": agent_id,
                "limit_type": limit_type,
                "limit": limit,
                "current": current,
            },
        )
        self.agent_id = agent_id
        self.limit_type = limit_type
        self.limit = limit
        self.current = current


class RateLimitError(BudgetError):
    """Rate limit exceeded."""

    pass


# Task Errors
class TaskError(XpressAIError):
    """Error in task system."""

    pass


class TaskNotFoundError(TaskError):
    """Task not found."""

    pass


class SOPError(TaskError):
    """Error in SOP execution."""

    pass


# Tool Errors
class ToolError(XpressAIError):
    """Error in tool system."""

    pass


class ToolNotFoundError(ToolError):
    """Tool not found."""

    pass


class ToolPermissionError(ToolError):
    """Tool permission denied."""

    pass


class ToolExecutionError(ToolError):
    """Tool execution failed."""

    pass


# Database Errors
class DatabaseError(XpressAIError):
    """Error in database operations."""

    pass


class MigrationError(DatabaseError):
    """Database migration failed."""

    pass
