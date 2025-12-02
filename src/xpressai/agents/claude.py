"""Claude Agent SDK backend.

Provides integration with the Claude Agent SDK for advanced agent capabilities.
"""

from typing import AsyncIterator, Any
import logging
import os

from xpressai.agents.base import AgentBackend
from xpressai.core.config import AgentConfig
from xpressai.core.exceptions import BackendError, BackendInitializationError

logger = logging.getLogger(__name__)

# Check for SDK availability
try:
    from anthropic import Anthropic

    ANTHROPIC_AVAILABLE = True
except ImportError:
    ANTHROPIC_AVAILABLE = False
    logger.warning("anthropic package not available")


class ClaudeAgentBackend(AgentBackend):
    """Backend using Claude via Anthropic API.

    Uses the Anthropic Python SDK for Claude model access with streaming
    and tool support.
    """

    def __init__(self):
        """Initialize the backend."""
        self._client = None
        self._config: AgentConfig | None = None
        self._model = "claude-sonnet-4-20250514"
        self._system_prompt = ""
        self._memory_context = ""
        self._tools: list[dict[str, Any]] = []
        self._conversation_history: list[dict[str, Any]] = []
        self._max_tokens = 4096

    async def initialize(self, config: AgentConfig) -> None:
        """Initialize with configuration.

        Args:
            config: Agent configuration

        Raises:
            BackendInitializationError: If initialization fails
        """
        self._config = config
        self._system_prompt = config.role

        if not ANTHROPIC_AVAILABLE:
            raise BackendInitializationError(
                "anthropic package not installed. Install with: pip install anthropic",
                {"backend": "claude-code"},
            )

        # Get API key
        api_key = os.environ.get("ANTHROPIC_API_KEY")
        if not api_key:
            raise BackendInitializationError(
                "ANTHROPIC_API_KEY environment variable not set", {"backend": "claude-code"}
            )

        try:
            self._client = Anthropic(api_key=api_key)
            logger.info("Claude backend initialized")
        except Exception as e:
            raise BackendInitializationError(
                f"Failed to initialize Anthropic client: {e}", {"backend": "claude-code"}
            )

    async def send(self, message: str) -> AsyncIterator[str]:
        """Send a message and stream response chunks.

        Args:
            message: User message

        Yields:
            Response text chunks
        """
        if self._client is None:
            raise BackendError("Backend not initialized")

        # Build system prompt
        system = self._system_prompt
        if self._memory_context:
            system += f"\n\n{self._memory_context}"

        # Add user message to history
        self._conversation_history.append(
            {
                "role": "user",
                "content": message,
            }
        )

        try:
            # Create streaming message
            with self._client.messages.stream(
                model=self._model,
                max_tokens=self._max_tokens,
                system=system,
                messages=self._conversation_history,
                tools=self._format_tools() if self._tools else None,
            ) as stream:
                response_text = ""

                for text in stream.text_stream:
                    response_text += text
                    yield text

                # Add response to history
                self._conversation_history.append(
                    {
                        "role": "assistant",
                        "content": response_text,
                    }
                )

            # Keep history manageable
            if len(self._conversation_history) > 40:
                self._conversation_history = self._conversation_history[-40:]

        except Exception as e:
            raise BackendError(f"Claude API error: {e}")

    def _format_tools(self) -> list[dict[str, Any]]:
        """Format tools for Claude API.

        Returns:
            Formatted tool definitions
        """
        formatted = []

        for tool in self._tools:
            formatted.append(
                {
                    "name": tool.get("name", "unknown"),
                    "description": tool.get("description", ""),
                    "input_schema": tool.get("parameters", {"type": "object", "properties": {}}),
                }
            )

        return formatted

    async def inject_memory(self, context: str) -> None:
        """Inject memory context.

        Args:
            context: Formatted memory context
        """
        self._memory_context = context

    async def register_tools(self, tools: list[dict[str, Any]]) -> None:
        """Register tools.

        Args:
            tools: List of tool definitions
        """
        self._tools = tools

    async def shutdown(self) -> None:
        """Shutdown the backend."""
        self._client = None
        self._conversation_history.clear()
        self._memory_context = ""

    async def interrupt(self) -> None:
        """Interrupt current operation."""
        # The Anthropic SDK handles interruption via context managers
        pass

    def clear_history(self) -> None:
        """Clear conversation history."""
        self._conversation_history.clear()

    def set_model(self, model: str) -> None:
        """Set the model to use.

        Args:
            model: Model identifier (e.g., "claude-sonnet-4-20250514")
        """
        self._model = model

    @property
    def model(self) -> str:
        """The model being used."""
        return self._model

    @property
    def supports_streaming(self) -> bool:
        return True

    @property
    def supports_tools(self) -> bool:
        return True

    @property
    def supports_memory(self) -> bool:
        return True


class ClaudeAgentSDKBackend(AgentBackend):
    """Backend using the Claude Agent SDK.

    This is a placeholder for when the official Claude Agent SDK is available.
    Currently falls back to the standard Anthropic API.
    """

    def __init__(self):
        """Initialize the backend."""
        # Use the standard Claude backend for now
        self._inner = ClaudeAgentBackend()

    async def initialize(self, config: AgentConfig) -> None:
        """Initialize with configuration."""
        await self._inner.initialize(config)

    async def send(self, message: str) -> AsyncIterator[str]:
        """Send a message and stream response."""
        async for chunk in self._inner.send(message):
            yield chunk

    async def inject_memory(self, context: str) -> None:
        """Inject memory context."""
        await self._inner.inject_memory(context)

    async def register_tools(self, tools: list[dict[str, Any]]) -> None:
        """Register tools."""
        await self._inner.register_tools(tools)

    async def shutdown(self) -> None:
        """Shutdown the backend."""
        await self._inner.shutdown()

    async def interrupt(self) -> None:
        """Interrupt current operation."""
        await self._inner.interrupt()

    @property
    def model(self) -> str:
        return self._inner.model

    @property
    def supports_streaming(self) -> bool:
        return True

    @property
    def supports_tools(self) -> bool:
        return True

    @property
    def supports_memory(self) -> bool:
        return True
