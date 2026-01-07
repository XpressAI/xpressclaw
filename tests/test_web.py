"""Tests for the web dashboard."""

import pytest
from unittest.mock import MagicMock, AsyncMock, patch
from datetime import datetime

# Skip all tests if FastAPI is not available
pytest.importorskip("fastapi")

from fastapi.testclient import TestClient
from xpressai.web.app import create_app


@pytest.fixture
def mock_runtime():
    """Create a mock runtime for testing."""
    runtime = MagicMock()
    runtime.is_running = True
    runtime.is_initialized = True

    # Mock memory manager
    runtime.memory_manager = MagicMock()
    runtime.memory_manager.get_stats = AsyncMock(return_value={
        "zettelkasten": {"total_memories": 10, "total_links": 5, "by_layer": {"shared": 10}},
        "vector_store": {"total_embeddings": 10},
    })
    runtime.memory_manager.get_recent = AsyncMock(return_value=[])
    runtime.memory_manager.search = AsyncMock(return_value=[])

    # Mock task board - use get_tasks (the actual method name)
    runtime.task_board = MagicMock()
    runtime.task_board.get_tasks = AsyncMock(return_value=[])

    # Mock activity manager
    runtime.activity_manager = MagicMock()
    runtime.activity_manager.get_recent = AsyncMock(return_value=[])

    # Mock other methods
    runtime.list_agents = AsyncMock(return_value=[
        MagicMock(name="atlas", status="running", backend="claude-code"),
    ])
    runtime.get_agent = AsyncMock(return_value=MagicMock(
        name="atlas", status="running", backend="claude-code"
    ))
    runtime.get_budget_summary = AsyncMock(return_value={
        "total_spent": 5.00,
        "daily_spent": 2.50,
        "daily_limit": 20.00,
        "input_tokens": 10000,
        "output_tokens": 5000,
        "request_count": 50,
    })
    runtime.get_top_spenders = AsyncMock(return_value=[
        {"agent_id": "atlas", "total_spent": 3.50, "daily_spent": 1.50, "monthly_spent": 3.50},
        {"agent_id": "gaia", "total_spent": 1.50, "daily_spent": 1.00, "monthly_spent": 1.50},
    ])
    runtime.get_task_counts = AsyncMock(return_value={
        "pending": 3,
        "in_progress": 1,
        "completed": 10,
    })

    return runtime


@pytest.fixture
def client_with_runtime(mock_runtime):
    """Create a test client with a mock runtime."""
    app = create_app(mock_runtime)
    return TestClient(app)


@pytest.fixture
def client_without_runtime():
    """Create a test client without a runtime."""
    app = create_app(None)
    return TestClient(app)


class TestApiEndpoints:
    """Tests for API endpoints."""

    def test_health_with_runtime(self, client_with_runtime):
        """Test health endpoint with runtime."""
        response = client_with_runtime.get("/api/health")
        assert response.status_code == 200
        assert response.json()["status"] == "connected"

    def test_health_without_runtime(self, client_without_runtime):
        """Test health endpoint without runtime."""
        response = client_without_runtime.get("/api/health")
        assert response.status_code == 200
        assert response.json()["status"] == "disconnected"

    def test_status_with_runtime(self, client_with_runtime):
        """Test status endpoint with runtime."""
        response = client_with_runtime.get("/api/status")
        assert response.status_code == 200
        data = response.json()
        assert data["status"] == "running"
        assert "agents" in data
        assert "budget" in data

    def test_status_without_runtime(self, client_without_runtime):
        """Test status endpoint without runtime."""
        response = client_without_runtime.get("/api/status")
        assert response.status_code == 200
        assert response.json()["status"] == "no_runtime"

    def test_agents_list(self, client_with_runtime):
        """Test agents list endpoint."""
        response = client_with_runtime.get("/api/agents")
        assert response.status_code == 200
        assert "agents" in response.json()

    def test_budget_endpoint(self, client_with_runtime):
        """Test budget endpoint."""
        response = client_with_runtime.get("/api/budget")
        assert response.status_code == 200
        data = response.json()
        assert "total_spent" in data

    def test_tasks_endpoint(self, client_with_runtime):
        """Test tasks endpoint."""
        response = client_with_runtime.get("/api/tasks")
        assert response.status_code == 200
        assert "counts" in response.json()

    def test_memory_stats_endpoint(self, client_with_runtime):
        """Test memory stats endpoint."""
        response = client_with_runtime.get("/api/memory/stats")
        assert response.status_code == 200
        data = response.json()
        assert "zettelkasten" in data


class TestHtmxPartials:
    """Tests for HTMX partial endpoints."""

    def test_agents_partial(self, client_with_runtime):
        """Test agents partial returns HTML."""
        response = client_with_runtime.get("/partials/agents")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]

    def test_agents_partial_no_runtime(self, client_without_runtime):
        """Test agents partial without runtime."""
        response = client_without_runtime.get("/partials/agents")
        assert response.status_code == 200
        assert "No runtime available" in response.text

    def test_budget_partial(self, client_with_runtime):
        """Test budget partial returns HTML."""
        response = client_with_runtime.get("/partials/budget")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]

    def test_tasks_partial(self, client_with_runtime):
        """Test tasks partial returns HTML with counts."""
        response = client_with_runtime.get("/partials/tasks")
        assert response.status_code == 200
        # Should contain task counts
        assert "Pending" in response.text or "pending" in response.text.lower()

    def test_tasks_by_status_partial_pending(self, client_with_runtime):
        """Test tasks by status partial for pending tasks."""
        response = client_with_runtime.get("/partials/tasks/pending")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]

    def test_tasks_by_status_partial_in_progress(self, client_with_runtime):
        """Test tasks by status partial for in_progress tasks."""
        response = client_with_runtime.get("/partials/tasks/in_progress")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]

    def test_tasks_by_status_partial_completed(self, client_with_runtime):
        """Test tasks by status partial for completed tasks."""
        response = client_with_runtime.get("/partials/tasks/completed")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]

    def test_tasks_by_status_partial_invalid(self, client_with_runtime):
        """Test tasks by status partial with invalid status."""
        response = client_with_runtime.get("/partials/tasks/invalid_status")
        assert response.status_code == 200
        assert "Invalid status" in response.text or "empty-state" in response.text

    def test_tasks_by_status_partial_no_runtime(self, client_without_runtime):
        """Test tasks by status partial without runtime."""
        response = client_without_runtime.get("/partials/tasks/pending")
        assert response.status_code == 200
        assert "No tasks" in response.text

    def test_memory_partial(self, client_with_runtime):
        """Test memory partial returns HTML."""
        response = client_with_runtime.get("/partials/memory")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]

    def test_memory_partial_no_runtime(self, client_without_runtime):
        """Test memory partial without runtime."""
        response = client_without_runtime.get("/partials/memory")
        assert response.status_code == 200
        assert "Memory not available" in response.text

    def test_memory_search_partial_empty_query(self, client_with_runtime):
        """Test memory search partial with empty query."""
        response = client_with_runtime.get("/partials/memory/search")
        assert response.status_code == 200
        assert "Enter a search query" in response.text

    def test_memory_search_partial_with_query(self, client_with_runtime):
        """Test memory search partial with query."""
        response = client_with_runtime.get("/partials/memory/search?q=test")
        assert response.status_code == 200
        # Should return results or no results message
        assert response.status_code == 200

    def test_activity_partial(self, client_with_runtime):
        """Test activity partial returns HTML."""
        response = client_with_runtime.get("/partials/activity")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]

    def test_logs_partial(self, client_with_runtime):
        """Test logs partial returns HTML."""
        response = client_with_runtime.get("/partials/logs")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]


class TestPageRoutes:
    """Tests for page routes."""

    def test_index_page(self, client_with_runtime):
        """Test index page loads."""
        response = client_with_runtime.get("/")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]

    def test_tasks_page(self, client_with_runtime):
        """Test tasks page loads."""
        response = client_with_runtime.get("/tasks")
        assert response.status_code == 200

    def test_memory_page(self, client_with_runtime):
        """Test memory page loads."""
        response = client_with_runtime.get("/memory")
        assert response.status_code == 200

    def test_logs_page(self, client_with_runtime):
        """Test logs page loads."""
        response = client_with_runtime.get("/logs")
        assert response.status_code == 200


class TestStaticFiles:
    """Tests for static file serving."""

    def test_static_css_exists(self, client_with_runtime):
        """Test static CSS is served."""
        response = client_with_runtime.get("/static/style.css")
        assert response.status_code == 200
        assert "text/css" in response.headers["content-type"]


class TestVectorStoreStats:
    """Tests for vector store stats handling."""

    def test_vector_stats_missing_table(self, temp_dir):
        """Test get_stats handles missing memory_embeddings table."""
        from xpressai.memory.database import Database
        from xpressai.memory.vector import VectorStore

        # Create database without sqlite-vec (won't create memory_embeddings)
        db = Database(temp_dir / "test.db")
        vector_store = VectorStore(db)

        import asyncio
        stats = asyncio.run(vector_store.get_stats())

        # Should not raise, should return valid stats
        assert "total_embeddings" in stats
        assert stats["total_embeddings"] == 0


class TestTaskCreationEndpoints:
    """Tests for task creation API endpoints."""

    def test_create_task_no_runtime(self, client_without_runtime):
        """Test creating task without runtime."""
        response = client_without_runtime.post(
            "/api/tasks",
            data={"title": "Test Task"}  # Form data, not JSON
        )
        assert response.status_code == 503

    def test_create_task_with_runtime(self, client_with_runtime, mock_runtime):
        """Test creating task with runtime."""
        # Mock task board
        from unittest.mock import MagicMock, AsyncMock
        mock_task = MagicMock()
        mock_task.id = "task-123"
        mock_task.title = "Test Task"
        mock_task.status.value = "pending"
        mock_runtime.task_board.create_task = AsyncMock(return_value=mock_task)

        response = client_with_runtime.post(
            "/api/tasks",
            data={"title": "Test Task", "description": "A test"}  # Form data, not JSON
        )
        assert response.status_code == 200
        data = response.json()
        assert data["title"] == "Test Task"
        assert "id" in data

    def test_get_task_endpoint(self, client_with_runtime, mock_runtime):
        """Test getting a specific task."""
        mock_task = MagicMock()
        mock_task.id = "task-123"
        mock_task.title = "Test Task"
        mock_task.description = "Description"
        mock_task.status.value = "pending"
        mock_task.agent_id = "atlas"
        mock_task.created_at.isoformat.return_value = "2024-01-01T00:00:00"
        mock_runtime.task_board.get_task = AsyncMock(return_value=mock_task)

        response = client_with_runtime.get("/api/tasks/task-123")
        assert response.status_code == 200
        data = response.json()
        assert data["title"] == "Test Task"


class TestTaskMessagesEndpoints:
    """Tests for task message API endpoints."""

    def test_get_task_messages_no_runtime(self, client_without_runtime):
        """Test getting messages without runtime."""
        response = client_without_runtime.get("/api/tasks/task-123/messages")
        assert response.status_code == 503

    def test_get_task_messages_with_runtime(self, client_with_runtime, mock_runtime):
        """Test getting messages for a task."""
        # Mock conversation manager
        mock_runtime.conversation_manager = MagicMock()
        mock_msg = MagicMock()
        mock_msg.id = 1
        mock_msg.role = "agent"
        mock_msg.content = "Hello"
        mock_msg.timestamp.isoformat.return_value = "2024-01-01T00:00:00"
        mock_runtime.conversation_manager.get_messages = AsyncMock(return_value=[mock_msg])

        response = client_with_runtime.get("/api/tasks/task-123/messages")
        assert response.status_code == 200
        data = response.json()
        assert "messages" in data
        assert len(data["messages"]) == 1
        assert data["messages"][0]["content"] == "Hello"

    def test_add_task_message(self, client_with_runtime, mock_runtime):
        """Test adding a message to a task."""
        from xpressai.tasks.board import TaskStatus

        # Mock task and conversation manager
        mock_task = MagicMock()
        mock_task.status = TaskStatus.IN_PROGRESS
        mock_runtime.task_board.get_task = AsyncMock(return_value=mock_task)
        mock_runtime.conversation_manager = MagicMock()
        mock_runtime.conversation_manager.add_message = AsyncMock()

        response = client_with_runtime.post(
            "/api/tasks/task-123/messages",
            data={"content": "User response"}  # Form data, not JSON
        )
        assert response.status_code == 200
        assert response.json()["status"] == "ok"

    def test_add_message_resumes_waiting_task(self, client_with_runtime, mock_runtime):
        """Test that adding message to waiting task resumes it."""
        from xpressai.tasks.board import TaskStatus

        # Mock task in waiting state
        mock_task = MagicMock()
        mock_task.status = TaskStatus.WAITING_FOR_INPUT
        mock_runtime.task_board.get_task = AsyncMock(return_value=mock_task)
        mock_runtime.task_board.update_status = AsyncMock()
        mock_runtime.conversation_manager = MagicMock()
        mock_runtime.conversation_manager.add_message = AsyncMock()

        response = client_with_runtime.post(
            "/api/tasks/task-123/messages",
            data={"content": "User response"}  # Form data, not JSON
        )
        assert response.status_code == 200

        # Verify add_message was called and status was updated
        mock_runtime.conversation_manager.add_message.assert_called_once()
        mock_runtime.task_board.update_status.assert_called_once_with(
            "task-123", TaskStatus.PENDING
        )


class TestTaskDetailPage:
    """Tests for task detail page."""

    def test_task_detail_page_no_runtime(self, client_without_runtime):
        """Test task detail page without runtime."""
        response = client_without_runtime.get("/task/task-123")
        assert response.status_code == 200
        assert "Runtime not available" in response.text

    def test_task_detail_page_with_runtime(self, client_with_runtime, mock_runtime):
        """Test task detail page loads."""
        from xpressai.tasks.board import TaskStatus
        from datetime import datetime

        mock_task = MagicMock()
        mock_task.id = "task-123"
        mock_task.title = "Test Task"
        mock_task.description = "Description"
        mock_task.status = TaskStatus.PENDING
        mock_task.agent_id = "atlas"
        mock_task.created_at = datetime.now()
        mock_runtime.task_board.get_task = AsyncMock(return_value=mock_task)
        mock_runtime.conversation_manager = MagicMock()
        mock_runtime.conversation_manager.get_messages = AsyncMock(return_value=[])

        response = client_with_runtime.get("/task/task-123")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]


class TestTaskMessagesPartial:
    """Tests for task messages partial."""

    def test_task_messages_partial_no_runtime(self, client_without_runtime):
        """Test messages partial without runtime."""
        response = client_without_runtime.get("/partials/task/task-123/messages")
        assert response.status_code == 200
        assert "Runtime not available" in response.text

    def test_task_messages_partial_empty(self, client_with_runtime, mock_runtime):
        """Test messages partial with no messages."""
        mock_runtime.conversation_manager = MagicMock()
        mock_runtime.conversation_manager.get_messages = AsyncMock(return_value=[])

        response = client_with_runtime.get("/partials/task/task-123/messages")
        assert response.status_code == 200
        assert "No messages" in response.text

    def test_task_messages_partial_with_messages(self, client_with_runtime, mock_runtime):
        """Test messages partial with messages."""
        from datetime import datetime

        mock_msg = MagicMock()
        mock_msg.role = "agent"
        mock_msg.content = "Hello user!"
        mock_msg.timestamp = datetime.now()

        mock_runtime.conversation_manager = MagicMock()
        mock_runtime.conversation_manager.get_messages = AsyncMock(return_value=[mock_msg])

        response = client_with_runtime.get("/partials/task/task-123/messages")
        assert response.status_code == 200
        assert "Hello user!" in response.text
        assert "AGENT" in response.text


class TestWaitingForInputPartial:
    """Tests for waiting_for_input tasks partial."""

    def test_waiting_tasks_partial(self, client_with_runtime, mock_runtime):
        """Test getting waiting_for_input tasks partial."""
        response = client_with_runtime.get("/partials/tasks/waiting_for_input")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]


class TestAgentCreationEndpoints:
    """Tests for agent creation API endpoints."""

    def test_agents_new_page_loads(self, client_with_runtime):
        """Test /agents/new page loads."""
        response = client_with_runtime.get("/agents/new")
        assert response.status_code == 200
        assert "text/html" in response.headers["content-type"]
        assert "Create New Agent" in response.text

    def test_create_agent_missing_name(self, client_with_runtime, tmp_path, monkeypatch):
        """Test create agent fails without name."""
        monkeypatch.chdir(tmp_path)

        response = client_with_runtime.post(
            "/api/agents",
            json={"backend": "claude"}
        )
        assert response.status_code == 400
        assert "name is required" in response.json()["detail"]

    def test_create_agent_invalid_name_format(self, client_with_runtime, tmp_path, monkeypatch):
        """Test create agent fails with invalid name format."""
        monkeypatch.chdir(tmp_path)

        # Name starting with a number (invalid even after lowercasing)
        response = client_with_runtime.post(
            "/api/agents",
            json={"name": "123-invalid"}
        )
        assert response.status_code == 400
        assert "must start with a letter" in response.json()["detail"]

    def test_create_agent_success(self, client_with_runtime, tmp_path, monkeypatch):
        """Test successful agent creation."""
        monkeypatch.chdir(tmp_path)

        response = client_with_runtime.post(
            "/api/agents",
            json={
                "name": "test-agent",
                "backend": "claude",
                "role": "You are a test agent.",
                "tools": ["filesystem", "shell"],
                "hooks": {
                    "before_message": ["memory_recall"],
                    "after_message": ["memory_remember"]
                }
            }
        )
        assert response.status_code == 200
        data = response.json()
        assert data["success"] is True
        assert data["agent"]["name"] == "test-agent"
        assert data["agent"]["backend"] == "claude"

        # Verify YAML file was created
        config_path = tmp_path / "xpressai.yaml"
        assert config_path.exists()

        import yaml
        with open(config_path) as f:
            config = yaml.safe_load(f)

        assert "agents" in config
        assert len(config["agents"]) == 1
        assert config["agents"][0]["name"] == "test-agent"
        assert config["agents"][0]["backend"] == "claude"

    def test_create_agent_duplicate_name(self, client_with_runtime, tmp_path, monkeypatch):
        """Test create agent fails if name already exists."""
        import yaml
        monkeypatch.chdir(tmp_path)

        # Create initial config with existing agent
        config = {
            "agents": [{"name": "existing-agent", "backend": "claude", "role": "Test"}]
        }
        with open(tmp_path / "xpressai.yaml", "w") as f:
            yaml.dump(config, f)

        response = client_with_runtime.post(
            "/api/agents",
            json={"name": "existing-agent", "backend": "local"}
        )
        assert response.status_code == 400
        assert "already exists" in response.json()["detail"]

    def test_create_agent_creates_config_file(self, client_with_runtime, tmp_path, monkeypatch):
        """Test agent creation creates config file if it doesn't exist."""
        monkeypatch.chdir(tmp_path)

        # No config file exists initially
        config_path = tmp_path / "xpressai.yaml"
        assert not config_path.exists()

        response = client_with_runtime.post(
            "/api/agents",
            json={"name": "first-agent", "backend": "local"}
        )
        assert response.status_code == 200

        # Config file should now exist with default structure
        assert config_path.exists()
        import yaml
        with open(config_path) as f:
            config = yaml.safe_load(f)
        assert "system" in config
        assert "agents" in config
        assert len(config["agents"]) == 1

    def test_create_agent_with_minimal_data(self, client_with_runtime, tmp_path, monkeypatch):
        """Test agent creation with only required fields."""
        monkeypatch.chdir(tmp_path)

        response = client_with_runtime.post(
            "/api/agents",
            json={"name": "minimal"}
        )
        assert response.status_code == 200

        import yaml
        with open(tmp_path / "xpressai.yaml") as f:
            config = yaml.safe_load(f)

        agent = config["agents"][0]
        assert agent["name"] == "minimal"
        assert agent["backend"] == "local"  # Default backend
        assert "role" in agent  # Should have default role

    def test_create_local_agent_with_config(self, client_with_runtime, tmp_path, monkeypatch):
        """Test creating a local agent with full configuration."""
        monkeypatch.chdir(tmp_path)

        response = client_with_runtime.post(
            "/api/agents",
            json={
                "name": "local-agent",
                "backend": "local",
                "role": "You are a local AI assistant.",
                "local_model": {
                    "model": "llama3:8b",
                    "inference_backend": "ollama",
                    "base_url": "http://localhost:11434",
                    "context_length": 8192,
                    "tool_format": "xml",
                    "thinking_mode": "never",
                    "max_tool_calls": 10,
                    "api_key": ""
                }
            }
        )
        assert response.status_code == 200

        import yaml
        with open(tmp_path / "xpressai.yaml") as f:
            config = yaml.safe_load(f)

        agent = config["agents"][0]
        assert agent["name"] == "local-agent"
        assert agent["backend"] == "local"
        assert "local_model" in agent

        lm = agent["local_model"]
        assert lm["model"] == "llama3:8b"
        assert lm["inference_backend"] == "ollama"
        assert lm["base_url"] == "http://localhost:11434"
        assert lm["context_length"] == 8192
        assert lm["tool_format"] == "xml"
        assert lm["thinking_mode"] == "never"
        assert lm["max_tool_calls"] == 10
        # Empty api_key should not be saved
        assert "api_key" not in lm

    def test_create_claude_agent_with_model(self, client_with_runtime, tmp_path, monkeypatch):
        """Test creating a Claude agent with specific model."""
        monkeypatch.chdir(tmp_path)

        response = client_with_runtime.post(
            "/api/agents",
            json={
                "name": "claude-agent",
                "backend": "claude",
                "model": "claude-opus-4-5-20251101",
                "role": "You are a Claude assistant."
            }
        )
        assert response.status_code == 200

        import yaml
        with open(tmp_path / "xpressai.yaml") as f:
            config = yaml.safe_load(f)

        agent = config["agents"][0]
        assert agent["name"] == "claude-agent"
        assert agent["backend"] == "claude"
        assert agent["model"] == "claude-opus-4-5-20251101"

    def test_create_openai_agent_with_model(self, client_with_runtime, tmp_path, monkeypatch):
        """Test creating an OpenAI agent with specific model."""
        monkeypatch.chdir(tmp_path)

        response = client_with_runtime.post(
            "/api/agents",
            json={
                "name": "openai-agent",
                "backend": "openai",
                "model": "gpt-5.2",
                "role": "You are a GPT assistant."
            }
        )
        assert response.status_code == 200

        import yaml
        with open(tmp_path / "xpressai.yaml") as f:
            config = yaml.safe_load(f)

        agent = config["agents"][0]
        assert agent["name"] == "openai-agent"
        assert agent["backend"] == "openai"
        assert agent["model"] == "gpt-5.2"
