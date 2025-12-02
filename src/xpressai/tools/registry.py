"""Tool Registry for discovering and managing MCP tools.

Provides a central registry for:
- Registering tools (both built-in and external)
- Discovering available tools
- Managing tool permissions and access
"""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable, Dict, List, Optional, TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.tools.mcp import MCPServer

logger = logging.getLogger(__name__)


class ToolCategory(Enum):
    """Categories of tools."""

    FILESYSTEM = "filesystem"
    SHELL = "shell"
    WEB = "web"
    DATABASE = "database"
    CUSTOM = "custom"


@dataclass
class ToolDefinition:
    """Definition of a tool available in the registry."""

    name: str
    description: str
    category: ToolCategory
    input_schema: Dict[str, Any]
    handler: Callable[..., Any]
    requires_confirmation: bool = False
    allowed_by_default: bool = True
    metadata: Dict[str, Any] = field(default_factory=dict)


@dataclass
class ToolPermission:
    """Permission settings for a tool."""

    tool_name: str
    allowed: bool = True
    allowed_paths: List[str] = field(default_factory=list)
    denied_paths: List[str] = field(default_factory=list)
    allowed_commands: List[str] = field(default_factory=list)
    denied_commands: List[str] = field(default_factory=list)


class ToolRegistry:
    """Central registry for MCP tools.

    Manages tool registration, discovery, and access control.
    """

    def __init__(self):
        self._tools: Dict[str, ToolDefinition] = {}
        self._permissions: Dict[str, ToolPermission] = {}
        self._mcp_servers: Dict[str, MCPServer] = {}
        self._initialized = False

    async def initialize(self) -> None:
        """Initialize the registry and load built-in tools."""
        if self._initialized:
            return

        # Register built-in tools
        await self._register_builtin_tools()
        self._initialized = True
        logger.info(f"Tool registry initialized with {len(self._tools)} tools")

    async def _register_builtin_tools(self) -> None:
        """Register built-in tools."""
        # Import here to avoid circular imports
        from xpressai.tools.builtin.filesystem import register_filesystem_tools
        from xpressai.tools.builtin.shell import register_shell_tools
        from xpressai.tools.builtin.web import register_web_tools

        await register_filesystem_tools(self)
        await register_shell_tools(self)
        await register_web_tools(self)

    def register_tool(self, tool: ToolDefinition) -> None:
        """Register a tool in the registry.

        Args:
            tool: The tool definition to register
        """
        if tool.name in self._tools:
            logger.warning(f"Tool '{tool.name}' already registered, overwriting")

        self._tools[tool.name] = tool
        logger.debug(f"Registered tool: {tool.name}")

    def unregister_tool(self, name: str) -> bool:
        """Unregister a tool from the registry.

        Args:
            name: Name of the tool to unregister

        Returns:
            True if tool was unregistered, False if not found
        """
        if name in self._tools:
            del self._tools[name]
            logger.debug(f"Unregistered tool: {name}")
            return True
        return False

    def get_tool(self, name: str) -> Optional[ToolDefinition]:
        """Get a tool by name.

        Args:
            name: Name of the tool

        Returns:
            The tool definition, or None if not found
        """
        return self._tools.get(name)

    def list_tools(
        self, category: Optional[ToolCategory] = None, include_disabled: bool = False
    ) -> List[ToolDefinition]:
        """List all registered tools.

        Args:
            category: Optional category to filter by
            include_disabled: Whether to include disabled tools

        Returns:
            List of tool definitions
        """
        tools = list(self._tools.values())

        if category:
            tools = [t for t in tools if t.category == category]

        if not include_disabled:
            tools = [t for t in tools if self.is_tool_allowed(t.name)]

        return tools

    def set_permission(self, permission: ToolPermission) -> None:
        """Set permission for a tool.

        Args:
            permission: The permission settings
        """
        self._permissions[permission.tool_name] = permission

    def get_permission(self, tool_name: str) -> Optional[ToolPermission]:
        """Get permission settings for a tool.

        Args:
            tool_name: Name of the tool

        Returns:
            The permission settings, or None if not set
        """
        return self._permissions.get(tool_name)

    def is_tool_allowed(self, tool_name: str) -> bool:
        """Check if a tool is allowed to be used.

        Args:
            tool_name: Name of the tool

        Returns:
            True if tool is allowed
        """
        tool = self._tools.get(tool_name)
        if not tool:
            return False

        permission = self._permissions.get(tool_name)
        if permission:
            return permission.allowed

        return tool.allowed_by_default

    async def call_tool(self, name: str, arguments: Dict[str, Any]) -> Any:
        """Call a tool with the given arguments.

        Args:
            name: Name of the tool to call
            arguments: Arguments to pass to the tool

        Returns:
            The result of the tool call

        Raises:
            ValueError: If tool not found or not allowed
        """
        tool = self._tools.get(name)
        if not tool:
            raise ValueError(f"Tool '{name}' not found")

        if not self.is_tool_allowed(name):
            raise ValueError(f"Tool '{name}' is not allowed")

        logger.debug(f"Calling tool: {name} with args: {arguments}")

        # Call the tool handler
        if asyncio.iscoroutinefunction(tool.handler):
            result = await tool.handler(**arguments)
        else:
            result = tool.handler(**arguments)

        return result

    def register_mcp_server(self, name: str, server: MCPServer) -> None:
        """Register an MCP server.

        Args:
            name: Name of the server
            server: The MCP server instance
        """
        self._mcp_servers[name] = server
        logger.info(f"Registered MCP server: {name}")

    def get_mcp_server(self, name: str) -> Optional[MCPServer]:
        """Get an MCP server by name.

        Args:
            name: Name of the server

        Returns:
            The MCP server, or None if not found
        """
        return self._mcp_servers.get(name)

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        """Get JSON schemas for all allowed tools.

        Returns:
            List of tool schemas in MCP format
        """
        schemas = []
        for tool in self.list_tools():
            schemas.append(
                {
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                }
            )
        return schemas
