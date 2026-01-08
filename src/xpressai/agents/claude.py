"""Claude Agent SDK backend.

Provides integration with the Claude Agent SDK for advanced agent capabilities.
Uses ClaudeSDKClient for session-based conversations with tool support.
"""

from typing import AsyncIterator, Any
import logging
import os
from pathlib import Path

from xpressai.agents.base import AgentBackend
from xpressai.core.config import AgentConfig
from xpressai.core.exceptions import BackendError, BackendInitializationError

logger = logging.getLogger(__name__)

# Memory system instructions - prepended to all agents
MEMORY_SYSTEM_INSTRUCTIONS = """
## CRITICAL: YOU HAVE ANTEROGRADE AMNESIA

You cannot form new long-term memories naturally. After each conversation ends, you will forget everything unless you explicitly save it using your memory tools.

**Before starting work:** Use `search_memory` to recall what you know about the user, their projects, and relevant context.

**During conversations:** When you learn important information (user details, company info, project decisions, preferences, technical details, contacts, URLs), use `create_memory` IMMEDIATELY. If you don't save it, you won't remember it next time.

**Be proactive:** If someone tells you about themselves or their work, SAVE IT. Your memory is your zettelkasten - treat it as essential to your function.
""".strip()

# Check for SDK availability
CLAUDE_SDK_AVAILABLE = False
ANTHROPIC_AVAILABLE = False

try:
    from claude_agent_sdk import (
        ClaudeSDKClient,
        ClaudeAgentOptions,
        AssistantMessage,
        TextBlock,
        ToolUseBlock,
        ResultMessage,
        tool,
        create_sdk_mcp_server,
    )

    CLAUDE_SDK_AVAILABLE = True
    logger.info("Claude Agent SDK available")

    # Define task control tools for the SDK
    # Note: SDK tools receive arguments as a dict and return MCP-style content
    @tool(
        name="complete_task",
        description="Mark the current task as COMPLETED. You MUST call this tool when you have finished the task. Do not just respond with text - call this tool to signal completion.",
        input_schema={
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "A brief summary of what was accomplished",
                },
            },
            "required": ["summary"],
        },
    )
    async def complete_task(args: dict) -> dict:
        from xpressai.tools.builtin.task_control import _mark_task_done
        summary = args.get("summary", "")
        _mark_task_done(summary)
        logger.info(f"Task marked as completed via SDK: {summary[:100]}...")
        return {"content": [{"type": "text", "text": f"Task completed: {summary}"}]}

    @tool(
        name="fail_task",
        description="Mark the current task as FAILED. Call this if you cannot complete the task for any reason (missing permissions, missing information, errors, etc.).",
        input_schema={
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Explanation of why the task cannot be completed",
                },
            },
            "required": ["reason"],
        },
    )
    async def fail_task(args: dict) -> dict:
        from xpressai.tools.builtin.task_control import _mark_task_done
        reason = args.get("reason", "")
        _mark_task_done(f"FAILED: {reason}")
        logger.info(f"Task marked as failed via SDK: {reason[:100]}...")
        return {"content": [{"type": "text", "text": f"Task failed: {reason}"}]}

except ImportError:
    logger.debug("claude-agent-sdk not installed, falling back to anthropic")

try:
    from anthropic import Anthropic

    ANTHROPIC_AVAILABLE = True
except ImportError:
    logger.debug("anthropic package not available")


class ClaudeAgentBackend(AgentBackend):
    """Backend using Claude Agent SDK with ClaudeSDKClient.

    Uses the official Claude Agent SDK for session-based conversations
    with full tool support, hooks, and interrupts.

    Falls back to direct Anthropic API if SDK is not available.
    """

    def __init__(self):
        """Initialize the backend."""
        self._client: Any = None
        self._config: AgentConfig | None = None
        self._model = "claude-sonnet-4-20250514"
        self._system_prompt = ""
        self._memory_context = ""
        self._tools: list[dict[str, Any]] = []
        self._cwd: Path | None = None
        self._use_sdk = CLAUDE_SDK_AVAILABLE
        self._connected = False

        # MCP server configurations
        self._mcp_server_configs: dict[str, dict[str, Any]] = {}

        # Fallback anthropic client
        self._anthropic_client: Any = None
        self._conversation_history: list[dict[str, Any]] = []
        self._max_tokens = 8192

    async def initialize(self, config: AgentConfig) -> None:
        """Initialize with configuration.

        Args:
            config: Agent configuration

        Raises:
            BackendInitializationError: If initialization fails
        """
        self._config = config
        self._system_prompt = config.role
        self._cwd = Path.cwd()

        if self._use_sdk:
            await self._initialize_sdk()
        else:
            await self._initialize_anthropic()

    async def _initialize_sdk(self) -> None:
        """Initialize using Claude Agent SDK."""
        if not CLAUDE_SDK_AVAILABLE:
            raise BackendInitializationError(
                "claude-agent-sdk not installed. Install with: pip install claude-agent-sdk",
                {"backend": "claude-code"},
            )

        # Build MCP server configs from stored config
        mcp_servers = self._build_mcp_servers()

        # Build allowed tools list - include our custom task control tools
        allowed_tools = [
            "Read", "Write", "Edit", "Bash", "Glob", "Grep", "Task",
            "WebFetch", "WebSearch",  # Web tools
            "complete_task", "fail_task",  # Task control tools
        ]

        # Add MCP tool permissions
        for server_name in mcp_servers.keys():
            # Allow all tools from each MCP server
            allowed_tools.append(f"mcp__{server_name}__*")

        # Create in-process MCP server for task control tools
        task_control_server = create_sdk_mcp_server(
            name="xpressai_task_control",
            version="1.0.0",
            tools=[complete_task, fail_task],
        )
        mcp_servers["xpressai_task_control"] = task_control_server

        # Build full system prompt with memory instructions
        # Memory tools are always available in chat mode via meta tools
        full_system_prompt = f"{MEMORY_SYSTEM_INSTRUCTIONS}\n\n{self._system_prompt}" if self._system_prompt else MEMORY_SYSTEM_INSTRUCTIONS

        # Use bypassPermissions for autonomous agent operation
        # Valid modes: acceptEdits, bypassPermissions, default, dontAsk, plan
        options = ClaudeAgentOptions(
            system_prompt=full_system_prompt if full_system_prompt else None,
            cwd=str(self._cwd) if self._cwd else None,
            allowed_tools=allowed_tools,
            permission_mode="bypassPermissions",
            mcp_servers=mcp_servers,
        )

        self._client = ClaudeSDKClient(options=options)
        logger.info(f"Claude Agent SDK backend initialized with {len(mcp_servers)} MCP servers")

    def _build_mcp_servers(self) -> dict[str, Any]:
        """Build MCP server configurations for the SDK.

        Returns:
            Dict of MCP server configs compatible with ClaudeAgentOptions
        """
        mcp_servers: dict[str, Any] = {}

        for name, config in self._mcp_server_configs.items():
            if config.get("type") == "stdio":
                mcp_servers[name] = {
                    "command": config.get("command"),
                    "args": config.get("args", []),
                    "env": config.get("env", {}),
                }
            elif config.get("type") == "sse":
                mcp_servers[name] = {
                    "type": "sse",
                    "url": config.get("url"),
                    "headers": config.get("headers", {}),
                }
            elif config.get("type") == "http":
                mcp_servers[name] = {
                    "type": "http",
                    "url": config.get("url"),
                    "headers": config.get("headers", {}),
                }

        return mcp_servers

    async def _initialize_anthropic(self) -> None:
        """Initialize using direct Anthropic API (fallback)."""
        if not ANTHROPIC_AVAILABLE:
            raise BackendInitializationError(
                "Neither claude-agent-sdk nor anthropic package installed. "
                "Install with: pip install claude-agent-sdk",
                {"backend": "claude-code"},
            )

        api_key = os.environ.get("ANTHROPIC_API_KEY")
        if not api_key:
            raise BackendInitializationError(
                "ANTHROPIC_API_KEY environment variable not set",
                {"backend": "claude-code"},
            )

        try:
            self._anthropic_client = Anthropic(api_key=api_key)
            logger.info("Claude backend initialized (using Anthropic API fallback)")
        except Exception as e:
            raise BackendInitializationError(
                f"Failed to initialize Anthropic client: {e}",
                {"backend": "claude-code"},
            )

    async def send(self, message: str) -> AsyncIterator[str]:
        """Send a message and stream response chunks.

        Args:
            message: User message

        Yields:
            Response text chunks
        """
        if self._use_sdk:
            async for chunk in self._send_sdk(message):
                yield chunk
        else:
            async for chunk in self._send_anthropic(message):
                yield chunk

    async def _send_sdk(self, message: str) -> AsyncIterator[str]:
        """Send message via Claude Agent SDK.

        Args:
            message: User message

        Yields:
            Response text chunks
        """
        if self._client is None:
            raise BackendError("Backend not initialized")

        try:
            # Connect if not connected
            if not self._connected:
                await self._client.connect()
                self._connected = True

            # Send query
            await self._client.query(message)

            # Stream response
            async for msg in self._client.receive_response():
                if isinstance(msg, AssistantMessage):
                    for block in msg.content:
                        if isinstance(block, TextBlock):
                            yield block.text
                        elif isinstance(block, ToolUseBlock):
                            # Yield tool use information for visibility
                            yield f"\n[Using tool: {block.name}]\n"
                elif isinstance(msg, ResultMessage):
                    # Log completion info
                    if msg.total_cost_usd:
                        logger.debug(f"Request cost: ${msg.total_cost_usd:.4f}")
                    if msg.is_error:
                        logger.warning(f"Request ended with error: {msg.result}")

        except Exception as e:
            raise BackendError(f"Claude Agent SDK error: {e}")

    async def _send_anthropic(self, message: str) -> AsyncIterator[str]:
        """Send message via direct Anthropic API (fallback).

        Args:
            message: User message

        Yields:
            Response text chunks
        """
        if self._anthropic_client is None:
            raise BackendError("Backend not initialized")

        # Build system prompt with memory instructions
        system = f"{MEMORY_SYSTEM_INSTRUCTIONS}\n\n{self._system_prompt}" if self._system_prompt else MEMORY_SYSTEM_INSTRUCTIONS
        if self._memory_context:
            system += f"\n\n{self._memory_context}"

        # Add user message to history
        self._conversation_history.append({"role": "user", "content": message})

        try:
            with self._anthropic_client.messages.stream(
                model=self._model,
                max_tokens=self._max_tokens,
                system=system if system else None,
                messages=self._conversation_history,
                tools=self._format_tools() if self._tools else None,
            ) as stream:
                response_text = ""

                for text in stream.text_stream:
                    response_text += text
                    yield text

                self._conversation_history.append({"role": "assistant", "content": response_text})

            # Keep history manageable
            if len(self._conversation_history) > 40:
                self._conversation_history = self._conversation_history[-40:]

        except Exception as e:
            raise BackendError(f"Anthropic API error: {e}")

    def _format_tools(self) -> list[dict[str, Any]]:
        """Format tools for Anthropic API.

        Returns:
            Formatted tool definitions
        """
        formatted = []

        for tool_def in self._tools:
            formatted.append(
                {
                    "name": tool_def.get("name", "unknown"),
                    "description": tool_def.get("description", ""),
                    "input_schema": tool_def.get(
                        "parameters", {"type": "object", "properties": {}}
                    ),
                }
            )

        return formatted

    async def inject_memory(self, context: str) -> None:
        """Inject memory context.

        Args:
            context: Formatted memory context
        """
        self._memory_context = context

        # For SDK, we'd need to reconnect with updated system prompt
        if self._use_sdk and self._connected:
            # Update will take effect on next query via system prompt
            pass

    async def register_tools(self, tools: list[dict[str, Any]]) -> None:
        """Register tools.

        Args:
            tools: List of tool definitions
        """
        self._tools = tools

        # For SDK, tools are registered via MCP servers
        # This is used primarily for the Anthropic API fallback

    async def shutdown(self) -> None:
        """Shutdown the backend."""
        if self._use_sdk and self._client and self._connected:
            try:
                await self._client.disconnect()
            except Exception as e:
                logger.warning(f"Error disconnecting SDK client: {e}")
            self._connected = False

        self._client = None
        self._anthropic_client = None
        self._conversation_history.clear()
        self._memory_context = ""

    async def interrupt(self) -> None:
        """Interrupt current operation."""
        if self._use_sdk and self._client and self._connected:
            try:
                await self._client.interrupt()
            except Exception as e:
                logger.warning(f"Error interrupting: {e}")

    def clear_history(self) -> None:
        """Clear conversation history."""
        self._conversation_history.clear()

    def set_history(self, history: list[dict[str, str]]) -> None:
        """Set conversation history from external source.

        Args:
            history: List of message dicts with 'role' and 'content' keys.
                     Role should be 'user' or 'assistant'.
        """
        self._conversation_history = [
            {"role": msg["role"], "content": msg["content"]}
            for msg in history
            if msg.get("role") in ("user", "assistant") and msg.get("content")
        ]

    def set_model(self, model: str) -> None:
        """Set the model to use.

        Args:
            model: Model identifier (e.g., "claude-sonnet-4-20250514")
        """
        self._model = model

    def set_working_directory(self, path: Path) -> None:
        """Set the working directory for file operations.

        Args:
            path: Working directory path
        """
        self._cwd = path

    def configure_mcp_servers(self, mcp_servers: dict[str, Any]) -> None:
        """Configure MCP servers for the backend.

        Must be called before initialize() to take effect.

        Args:
            mcp_servers: Dict mapping server names to their configurations.
                Each config should have:
                - type: "stdio" | "sse" | "http"
                - command: Command to run (for stdio)
                - args: Command arguments (for stdio)
                - env: Environment variables
                - url: Server URL (for sse/http)
                - headers: Request headers (for sse/http)
        """
        self._mcp_server_configs = {}

        for name, config in mcp_servers.items():
            # Handle both McpServerConfig objects and dicts
            if hasattr(config, "__dict__"):
                # It's a dataclass, convert to dict
                self._mcp_server_configs[name] = {
                    "type": getattr(config, "type", "stdio"),
                    "command": getattr(config, "command", None),
                    "args": getattr(config, "args", []),
                    "env": getattr(config, "env", {}),
                    "url": getattr(config, "url", None),
                    "headers": getattr(config, "headers", {}),
                }
            else:
                # It's already a dict
                self._mcp_server_configs[name] = config

        logger.debug(f"Configured {len(self._mcp_server_configs)} MCP servers")

    @property
    def model(self) -> str:
        """The model being used."""
        return self._model

    @property
    def supports_streaming(self) -> bool:
        """Whether streaming is supported."""
        return True

    @property
    def supports_tools(self) -> bool:
        """Whether tool use is supported."""
        return True

    @property
    def supports_memory(self) -> bool:
        """Whether memory injection is supported."""
        return True

    @property
    def using_sdk(self) -> bool:
        """Whether using the Claude Agent SDK."""
        return self._use_sdk


class ClaudeCodeBackend(ClaudeAgentBackend):
    """Alias for ClaudeAgentBackend for backwards compatibility."""

    pass
