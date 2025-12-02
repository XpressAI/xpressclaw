"""Tests for the tools module."""

import pytest
from pathlib import Path

from xpressai.tools.registry import (
    ToolRegistry,
    ToolDefinition,
    ToolCategory,
    ToolPermission,
)


class TestToolDefinition:
    """Tests for ToolDefinition."""

    def test_create_tool_definition(self):
        """Test creating a tool definition."""

        def my_handler(x: int) -> int:
            return x * 2

        tool = ToolDefinition(
            name="double",
            description="Doubles a number",
            category=ToolCategory.CUSTOM,
            input_schema={"type": "object", "properties": {"x": {"type": "integer"}}},
            handler=my_handler,
        )

        assert tool.name == "double"
        assert tool.category == ToolCategory.CUSTOM
        assert not tool.requires_confirmation
        assert tool.allowed_by_default


class TestToolRegistry:
    """Tests for ToolRegistry."""

    @pytest.fixture
    def registry(self):
        """Create a fresh registry for each test."""
        return ToolRegistry()

    def test_register_tool(self, registry: ToolRegistry):
        """Test registering a tool."""
        tool = ToolDefinition(
            name="test_tool",
            description="A test tool",
            category=ToolCategory.CUSTOM,
            input_schema={},
            handler=lambda: "hello",
        )

        registry.register_tool(tool)

        assert registry.get_tool("test_tool") is not None
        assert registry.get_tool("test_tool").name == "test_tool"

    def test_unregister_tool(self, registry: ToolRegistry):
        """Test unregistering a tool."""
        tool = ToolDefinition(
            name="test_tool",
            description="A test tool",
            category=ToolCategory.CUSTOM,
            input_schema={},
            handler=lambda: "hello",
        )

        registry.register_tool(tool)
        assert registry.unregister_tool("test_tool")
        assert registry.get_tool("test_tool") is None

    def test_unregister_nonexistent_returns_false(self, registry: ToolRegistry):
        """Test unregistering nonexistent tool returns False."""
        assert not registry.unregister_tool("nonexistent")

    def test_list_tools(self, registry: ToolRegistry):
        """Test listing tools."""
        tools = [
            ToolDefinition(
                name=f"tool_{i}",
                description=f"Tool {i}",
                category=ToolCategory.CUSTOM,
                input_schema={},
                handler=lambda: None,
            )
            for i in range(3)
        ]

        for tool in tools:
            registry.register_tool(tool)

        listed = registry.list_tools()
        assert len(listed) == 3

    def test_list_tools_by_category(self, registry: ToolRegistry):
        """Test filtering tools by category."""
        registry.register_tool(
            ToolDefinition(
                name="fs_tool",
                description="Filesystem tool",
                category=ToolCategory.FILESYSTEM,
                input_schema={},
                handler=lambda: None,
            )
        )
        registry.register_tool(
            ToolDefinition(
                name="shell_tool",
                description="Shell tool",
                category=ToolCategory.SHELL,
                input_schema={},
                handler=lambda: None,
            )
        )

        fs_tools = registry.list_tools(category=ToolCategory.FILESYSTEM)
        assert len(fs_tools) == 1
        assert fs_tools[0].name == "fs_tool"

    def test_set_permission(self, registry: ToolRegistry):
        """Test setting tool permissions."""
        tool = ToolDefinition(
            name="test_tool",
            description="A test tool",
            category=ToolCategory.CUSTOM,
            input_schema={},
            handler=lambda: None,
        )
        registry.register_tool(tool)

        permission = ToolPermission(tool_name="test_tool", allowed=False)
        registry.set_permission(permission)

        assert not registry.is_tool_allowed("test_tool")

    def test_tool_allowed_by_default(self, registry: ToolRegistry):
        """Test tools are allowed by default."""
        tool = ToolDefinition(
            name="test_tool",
            description="A test tool",
            category=ToolCategory.CUSTOM,
            input_schema={},
            handler=lambda: None,
        )
        registry.register_tool(tool)

        assert registry.is_tool_allowed("test_tool")

    @pytest.mark.asyncio
    async def test_call_tool(self, registry: ToolRegistry):
        """Test calling a tool."""

        def add(a: int, b: int) -> int:
            return a + b

        tool = ToolDefinition(
            name="add",
            description="Add two numbers",
            category=ToolCategory.CUSTOM,
            input_schema={},
            handler=add,
        )
        registry.register_tool(tool)

        result = await registry.call_tool("add", {"a": 2, "b": 3})
        assert result == 5

    @pytest.mark.asyncio
    async def test_call_async_tool(self, registry: ToolRegistry):
        """Test calling an async tool."""

        async def async_double(x: int) -> int:
            return x * 2

        tool = ToolDefinition(
            name="async_double",
            description="Double a number async",
            category=ToolCategory.CUSTOM,
            input_schema={},
            handler=async_double,
        )
        registry.register_tool(tool)

        result = await registry.call_tool("async_double", {"x": 5})
        assert result == 10

    @pytest.mark.asyncio
    async def test_call_nonexistent_tool_raises(self, registry: ToolRegistry):
        """Test calling nonexistent tool raises ValueError."""
        with pytest.raises(ValueError, match="not found"):
            await registry.call_tool("nonexistent", {})

    @pytest.mark.asyncio
    async def test_call_disabled_tool_raises(self, registry: ToolRegistry):
        """Test calling disabled tool raises ValueError."""
        tool = ToolDefinition(
            name="test_tool",
            description="A test tool",
            category=ToolCategory.CUSTOM,
            input_schema={},
            handler=lambda: None,
        )
        registry.register_tool(tool)

        permission = ToolPermission(tool_name="test_tool", allowed=False)
        registry.set_permission(permission)

        with pytest.raises(ValueError, match="not allowed"):
            await registry.call_tool("test_tool", {})

    def test_get_tool_schemas(self, registry: ToolRegistry):
        """Test getting tool schemas for MCP."""
        tool = ToolDefinition(
            name="test_tool",
            description="A test tool",
            category=ToolCategory.CUSTOM,
            input_schema={"type": "object", "properties": {"x": {"type": "integer"}}},
            handler=lambda x: x,
        )
        registry.register_tool(tool)

        schemas = registry.get_tool_schemas()
        assert len(schemas) == 1
        assert schemas[0]["name"] == "test_tool"
        assert schemas[0]["inputSchema"]["type"] == "object"
