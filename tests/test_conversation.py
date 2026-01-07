"""Tests for task conversation management."""

import pytest
from datetime import datetime
from unittest.mock import MagicMock, AsyncMock

from xpressai.memory.database import Database
from xpressai.tasks.board import TaskBoard, TaskStatus
from xpressai.tasks.conversation import ConversationManager, TaskMessage


@pytest.fixture
def db(temp_dir):
    """Create a test database."""
    return Database(temp_dir / "test.db")


@pytest.fixture
def task_board(db):
    """Create a test task board."""
    return TaskBoard(db)


@pytest.fixture
def conversation_manager(db, task_board):
    """Create a test conversation manager."""
    return ConversationManager(db, task_board)


class TestConversationManager:
    """Tests for ConversationManager."""

    @pytest.mark.asyncio
    async def test_add_message(self, conversation_manager, task_board):
        """Test adding a message to a task."""
        # Create a task first
        task = await task_board.create_task(title="Test Task")

        # Add a message
        msg = await conversation_manager.add_message(
            task_id=task.id,
            role="agent",
            content="Hello, user!"
        )

        assert msg.task_id == task.id
        assert msg.role == "agent"
        assert msg.content == "Hello, user!"
        assert isinstance(msg.timestamp, datetime)

    @pytest.mark.asyncio
    async def test_get_messages(self, conversation_manager, task_board):
        """Test retrieving messages for a task."""
        task = await task_board.create_task(title="Test Task")

        # Add multiple messages
        await conversation_manager.add_message(task.id, "agent", "First message")
        await conversation_manager.add_message(task.id, "user", "Second message")
        await conversation_manager.add_message(task.id, "agent", "Third message")

        messages = await conversation_manager.get_messages(task.id)

        assert len(messages) == 3
        assert messages[0].content == "First message"
        assert messages[1].content == "Second message"
        assert messages[2].content == "Third message"
        assert messages[0].role == "agent"
        assert messages[1].role == "user"

    @pytest.mark.asyncio
    async def test_get_messages_empty(self, conversation_manager, task_board):
        """Test getting messages for a task with no messages."""
        task = await task_board.create_task(title="Test Task")

        messages = await conversation_manager.get_messages(task.id)

        assert messages == []

    @pytest.mark.asyncio
    async def test_request_input(self, conversation_manager, task_board):
        """Test requesting input from user."""
        task = await task_board.create_task(title="Test Task")

        await conversation_manager.request_input(task.id, "What should I do next?")

        # Check message was added
        messages = await conversation_manager.get_messages(task.id)
        assert len(messages) == 1
        assert messages[0].role == "agent"
        assert messages[0].content == "What should I do next?"

        # Check task status changed
        updated_task = await task_board.get_task(task.id)
        assert updated_task.status == TaskStatus.WAITING_FOR_INPUT

    @pytest.mark.asyncio
    async def test_provide_input(self, conversation_manager, task_board):
        """Test providing user input to a waiting task."""
        task = await task_board.create_task(title="Test Task")

        # First request input
        await conversation_manager.request_input(task.id, "Need your help!")

        # Then provide input
        await conversation_manager.provide_input(task.id, "Here's my answer")

        # Check messages
        messages = await conversation_manager.get_messages(task.id)
        assert len(messages) == 2
        assert messages[1].role == "user"
        assert messages[1].content == "Here's my answer"

        # Check task status changed back to pending
        updated_task = await task_board.get_task(task.id)
        assert updated_task.status == TaskStatus.PENDING

    @pytest.mark.asyncio
    async def test_get_pending_input(self, conversation_manager, task_board):
        """Test checking for pending user input."""
        task = await task_board.create_task(title="Test Task")

        # No messages yet - should return None
        result = await conversation_manager.get_pending_input(task.id)
        assert result is None

        # Add agent message - should still return None
        await conversation_manager.add_message(task.id, "agent", "Question?")
        result = await conversation_manager.get_pending_input(task.id)
        assert result is None

        # Add user message - should return the content
        await conversation_manager.add_message(task.id, "user", "Answer!")
        result = await conversation_manager.get_pending_input(task.id)
        assert result == "Answer!"

    @pytest.mark.asyncio
    async def test_get_conversation_context(self, conversation_manager, task_board):
        """Test getting formatted conversation context."""
        task = await task_board.create_task(title="Test Task")

        # Add conversation
        await conversation_manager.add_message(task.id, "agent", "Hello")
        await conversation_manager.add_message(task.id, "user", "Hi there")

        context = await conversation_manager.get_conversation_context(task.id)

        assert "Previous conversation:" in context
        assert "[AGENT]: Hello" in context
        assert "[USER]: Hi there" in context

    @pytest.mark.asyncio
    async def test_get_conversation_context_empty(self, conversation_manager, task_board):
        """Test getting context for task with no messages."""
        task = await task_board.create_task(title="Test Task")

        context = await conversation_manager.get_conversation_context(task.id)

        assert context == ""


class TestWaitingForInputStatus:
    """Tests for waiting_for_input task status."""

    @pytest.mark.asyncio
    async def test_task_status_enum_has_waiting_for_input(self):
        """Test TaskStatus enum includes waiting_for_input."""
        assert hasattr(TaskStatus, 'WAITING_FOR_INPUT')
        assert TaskStatus.WAITING_FOR_INPUT.value == "waiting_for_input"

    @pytest.mark.asyncio
    async def test_task_counts_include_waiting(self, task_board):
        """Test task counts include waiting_for_input."""
        counts = await task_board.get_counts()
        assert "waiting_for_input" in counts

    @pytest.mark.asyncio
    async def test_get_tasks_by_waiting_status(self, task_board):
        """Test filtering tasks by waiting_for_input status."""
        # Create task and set to waiting
        task = await task_board.create_task(title="Waiting Task")
        await task_board.update_status(task.id, TaskStatus.WAITING_FOR_INPUT)

        # Query by status
        tasks = await task_board.get_tasks(status=TaskStatus.WAITING_FOR_INPUT)
        assert len(tasks) == 1
        assert tasks[0].title == "Waiting Task"
