"""Abstract base class for agent backends.

All agent backends must implement this interface to be compatible with XpressAI.
"""

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import AsyncIterator, Any, Protocol


class AgentStatus(str, Enum):
    """Status of an agent."""

    STOPPED = "stopped"
    STARTING = "starting"
    RUNNING = "running"
    PAUSED = "paused"
    ERROR = "error"


@dataclass
class AgentMessage:
    """A message from an agent.

    Attributes:
        role: Message role (user, assistant, system, tool_use, tool_result)
        content: Message content (string or structured data for tools)
        metadata: Additional metadata
        timestamp: When the message was created
    """

    role: str
    content: str | dict[str, Any]
    metadata: dict[str, Any] = field(default_factory=dict)
    timestamp: datetime = field(default_factory=datetime.now)

    @classmethod
    def user(cls, content: str) -> "AgentMessage":
        """Create a user message."""
        return cls(role="user", content=content)

    @classmethod
    def assistant(cls, content: str) -> "AgentMessage":
        """Create an assistant message."""
        return cls(role="assistant", content=content)

    @classmethod
    def system(cls, content: str) -> "AgentMessage":
        """Create a system message."""
        return cls(role="system", content=content)

    @classmethod
    def tool_use(cls, tool_name: str, tool_input: dict[str, Any]) -> "AgentMessage":
        """Create a tool use message."""
        return cls(
            role="tool_use",
            content={"name": tool_name, "input": tool_input},
        )

    @classmethod
    def tool_result(cls, result: Any, is_error: bool = False) -> "AgentMessage":
        """Create a tool result message."""
        return cls(
            role="tool_result",
            content={"result": result, "is_error": is_error},
        )


class AgentBackend(ABC):
    """Abstract base class for agent backends.

    All backends must implement these methods to work with XpressAI.
    """

    @abstractmethod
    async def initialize(self, config: Any) -> None:
        """Initialize the backend with configuration.

        Args:
            config: AgentConfig from the configuration system
        """
        pass

    @abstractmethod
    async def send(self, message: str) -> AsyncIterator[str]:
        """Send a message and stream response chunks.

        Args:
            message: User message to send

        Yields:
            Response text chunks as they arrive
        """
        pass

    @abstractmethod
    async def shutdown(self) -> None:
        """Gracefully shut down the backend."""
        pass

    async def inject_memory(self, context: str) -> None:
        """Inject memory context into the agent.

        Default implementation does nothing. Override for backends that
        support memory injection.

        Args:
            context: Formatted memory context string
        """
        pass

    async def register_tools(self, tools: list[dict[str, Any]]) -> None:
        """Register tools with the backend.

        Default implementation does nothing. Override for backends that
        support tool registration.

        Args:
            tools: List of tool definitions
        """
        pass

    async def interrupt(self) -> None:
        """Interrupt the current operation.

        Default implementation does nothing. Override for backends that
        support interruption.
        """
        pass

    @property
    def model(self) -> str:
        """The model being used."""
        return "unknown"

    @property
    def supports_streaming(self) -> bool:
        """Whether this backend supports streaming responses."""
        return True

    @property
    def supports_tools(self) -> bool:
        """Whether this backend supports tool use."""
        return False

    @property
    def supports_memory(self) -> bool:
        """Whether this backend supports memory injection."""
        return False


class ToolHandler(Protocol):
    """Protocol for tool execution handlers."""

    async def execute(self, tool_name: str, tool_input: dict[str, Any]) -> dict[str, Any]:
        """Execute a tool and return the result.

        Args:
            tool_name: Name of the tool to execute
            tool_input: Tool input parameters

        Returns:
            Tool execution result
        """
        ...


class StreamingAdapter:
    """Adapter for backends that don't support streaming.

    Wraps a non-streaming send() method to provide streaming interface.
    """

    def __init__(self, backend: AgentBackend):
        """Initialize the adapter.

        Args:
            backend: Backend to wrap
        """
        self.backend = backend

    async def send(self, message: str) -> AsyncIterator[str]:
        """Send message and simulate streaming.

        Yields the complete response as a single chunk.
        """
        # Get the complete response
        response_parts = []
        async for chunk in self.backend.send(message):
            response_parts.append(chunk)

        # Yield as single chunk
        yield "".join(response_parts)


class ToolPromptWrapper:
    """Wrapper for backends that don't support native tool use.

    Injects tool definitions into the system prompt and parses
    tool calls from the response.
    """

    def __init__(self, tools: list[dict[str, Any]]):
        """Initialize the wrapper.

        Args:
            tools: List of tool definitions
        """
        self.tools = tools

    def format_tools_prompt(self) -> str:
        """Format tools as a prompt section.

        Returns:
            Formatted tools prompt
        """
        if not self.tools:
            return ""

        lines = ["## Available Tools\n"]

        for tool in self.tools:
            name = tool.get("name", "unknown")
            description = tool.get("description", "")
            params = tool.get("parameters", {})

            lines.append(f"### {name}")
            lines.append(description)

            if params:
                lines.append("\nParameters:")
                for param_name, param_info in params.get("properties", {}).items():
                    param_type = param_info.get("type", "any")
                    param_desc = param_info.get("description", "")
                    required = param_name in params.get("required", [])
                    req_mark = " (required)" if required else ""
                    lines.append(f"- {param_name}: {param_type}{req_mark} - {param_desc}")

            lines.append("")

        lines.append(
            'To use a tool, respond with:\n```tool\n{"name": "tool_name", "input": {...}}\n```'
        )

        return "\n".join(lines)

    def parse_tool_call(self, response: str) -> tuple[str | None, dict[str, Any] | None]:
        """Parse a tool call from a response.

        Args:
            response: Response text to parse

        Returns:
            Tuple of (tool_name, tool_input) or (None, None) if no tool call
        """
        import re
        import json

        # Look for ```tool ... ``` blocks
        pattern = r"```tool\s*\n?(.*?)\n?```"
        match = re.search(pattern, response, re.DOTALL)

        if not match:
            return None, None

        try:
            data = json.loads(match.group(1))
            return data.get("name"), data.get("input", {})
        except json.JSONDecodeError:
            return None, None
