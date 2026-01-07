"""Tests for the tools module."""

import pytest
from pathlib import Path
from unittest.mock import MagicMock, AsyncMock

from xpressai.tools.registry import (
    ToolRegistry,
    ToolDefinition,
    ToolCategory,
    ToolPermission,
)
from xpressai.tools.builtin.meta import (
    set_managers,
    create_task,
    create_memory,
    search_memory,
    META_TOOLS,
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


class TestMetaTools:
    """Tests for meta tools (task creation, memory, etc.)."""

    @pytest.fixture(autouse=True)
    def reset_managers(self):
        """Reset managers before and after each test."""
        set_managers(None, None, None, agent_id=None, in_task_context=False, task_id=None)
        yield
        set_managers(None, None, None, agent_id=None, in_task_context=False, task_id=None)

    @pytest.mark.asyncio
    async def test_create_task_blocked_in_task_context(self):
        """Test that create_task is blocked when agent is executing a task."""
        mock_board = MagicMock()
        mock_memory = MagicMock()
        mock_sop = MagicMock()

        # Set context as if we're inside a task execution
        set_managers(
            mock_board,
            mock_memory,
            mock_sop,
            agent_id="test-agent",
            in_task_context=True,
            task_id="task-123",
        )

        # Try to create a task - should be blocked
        result = await create_task("New Task", "Description")

        assert "error" in result
        assert "Cannot create new tasks while executing a task" in result["error"]
        assert "hint" in result

    @pytest.mark.asyncio
    async def test_create_task_allowed_in_chat_context(self):
        """Test that create_task works in chat context (not in task)."""
        mock_board = MagicMock()
        mock_task = MagicMock()
        mock_task.id = "task-456"
        mock_task.title = "New Task"
        mock_task.status.value = "pending"
        mock_task.agent_id = "test-agent"
        mock_board.create_task = AsyncMock(return_value=mock_task)

        set_managers(
            mock_board,
            MagicMock(),
            MagicMock(),
            agent_id="test-agent",
            in_task_context=False,  # Chat context, not task
            task_id=None,
        )

        result = await create_task("New Task", "Description")

        assert result["success"] is True
        assert result["task_id"] == "task-456"
        mock_board.create_task.assert_called_once()

    @pytest.mark.asyncio
    async def test_create_task_without_board_returns_error(self):
        """Test create_task returns error if task board not available."""
        set_managers(None, None, None, agent_id=None, in_task_context=False)

        result = await create_task("Test Task")

        assert "error" in result
        assert "Task board not available" in result["error"]

    @pytest.mark.asyncio
    async def test_create_memory_works(self):
        """Test create_memory creates memories."""
        mock_memory = MagicMock()
        mock_result = MagicMock()
        mock_result.id = "mem-123"
        mock_result.summary = "Test summary"
        mock_result.tags = ["tag1", "tag2"]
        mock_memory.add = AsyncMock(return_value=mock_result)

        set_managers(MagicMock(), mock_memory, MagicMock(), agent_id="test-agent")

        result = await create_memory(
            content="Full content here",
            summary="Test summary",
            tags=["tag1", "tag2"],
        )

        assert result["success"] is True
        assert result["memory_id"] == "mem-123"
        mock_memory.add.assert_called_once()

    @pytest.mark.asyncio
    async def test_search_memory_works(self):
        """Test search_memory returns results."""
        mock_memory = MagicMock()
        mock_result = MagicMock()
        mock_result.memory.id = "mem-123"
        mock_result.memory.summary = "Found memory"
        mock_result.memory.content = "Memory content"
        mock_result.memory.tags = ["tag1"]
        mock_result.relevance_score = 0.95
        mock_memory.search = AsyncMock(return_value=[mock_result])

        set_managers(MagicMock(), mock_memory, MagicMock(), agent_id="test-agent")

        result = await search_memory("test query", limit=5)

        assert result["success"] is True
        assert result["count"] == 1
        assert len(result["memories"]) == 1
        assert result["memories"][0]["id"] == "mem-123"

    def test_meta_tools_dict_contains_expected_tools(self):
        """Test META_TOOLS dict has the expected tools."""
        assert "create_task" in META_TOOLS
        assert "create_memory" in META_TOOLS
        assert "search_memory" in META_TOOLS
        assert "create_procedure" in META_TOOLS
