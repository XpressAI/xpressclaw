"""Integration tests for XpressAI runtime."""

import asyncio
import pytest
from pathlib import Path
from decimal import Decimal

from xpressai.core.config import Config, AgentConfig, SystemConfig, BudgetConfig, MemoryConfig
from xpressai.core.runtime import Runtime, AgentState
from xpressai.core.exceptions import AgentNotFoundError, AgentAlreadyRunningError


class TestRuntimeLifecycle:
    """Test runtime initialization and lifecycle."""

    @pytest.fixture
    def config(self) -> Config:
        """Create a test configuration."""
        return Config(
            system=SystemConfig(
                isolation="none",  # No Docker for tests
                budget=BudgetConfig(daily=Decimal("10.00")),
            ),
            agents=[
                AgentConfig(name="test-agent", backend="local", role="Test agent"),
            ],
            memory=MemoryConfig(near_term_slots=4),
        )

    @pytest.fixture
    def runtime(self, config: Config, tmp_path: Path) -> Runtime:
        """Create a runtime instance."""
        # Override data dir to use temp path
        config.system.data_dir = tmp_path / ".xpressai"
        config.system.data_dir.mkdir(parents=True, exist_ok=True)
        return Runtime(config, workspace=tmp_path)

    async def test_runtime_initialize(self, runtime: Runtime):
        """Test runtime initialization."""
        assert not runtime.is_initialized
        assert not runtime.is_running

        await runtime.initialize()

        assert runtime.is_initialized
        assert not runtime.is_running

    async def test_runtime_start_stop(self, runtime: Runtime):
        """Test runtime start and stop."""
        await runtime.initialize()
        await runtime.start()

        assert runtime.is_running

        # Check agent is registered
        agents = await runtime.list_agents()
        assert len(agents) == 1
        assert agents[0].name == "test-agent"
        assert agents[0].status == "running"

        await runtime.stop()
        assert not runtime.is_running

    async def test_list_agents(self, runtime: Runtime):
        """Test listing agents."""
        await runtime.initialize()

        agents = await runtime.list_agents()
        assert len(agents) == 1
        assert agents[0].name == "test-agent"
        assert agents[0].backend == "local"
        assert agents[0].status == "stopped"

    async def test_get_agent(self, runtime: Runtime):
        """Test getting a specific agent."""
        await runtime.initialize()

        agent = await runtime.get_agent("test-agent")
        assert agent is not None
        assert agent.name == "test-agent"

        # Non-existent agent
        agent = await runtime.get_agent("non-existent")
        assert agent is None

    async def test_start_agent(self, runtime: Runtime):
        """Test starting a specific agent."""
        await runtime.initialize()

        agent = await runtime.start_agent("test-agent")
        assert agent.status == "running"
        assert agent.started_at is not None

    async def test_start_nonexistent_agent_raises(self, runtime: Runtime):
        """Test starting non-existent agent raises error."""
        await runtime.initialize()

        with pytest.raises(AgentNotFoundError):
            await runtime.start_agent("non-existent")

    async def test_start_already_running_agent_raises(self, runtime: Runtime):
        """Test starting already running agent raises error."""
        await runtime.initialize()
        await runtime.start_agent("test-agent")

        with pytest.raises(AgentAlreadyRunningError):
            await runtime.start_agent("test-agent")

    async def test_stop_agent(self, runtime: Runtime):
        """Test stopping a specific agent."""
        await runtime.initialize()
        await runtime.start_agent("test-agent")

        agent = await runtime.stop_agent("test-agent")
        assert agent.status == "stopped"
        assert agent.started_at is None

    async def test_stop_already_stopped_agent(self, runtime: Runtime):
        """Test stopping already stopped agent is a no-op."""
        await runtime.initialize()

        agent = await runtime.stop_agent("test-agent")
        assert agent.status == "stopped"

    async def test_get_task_counts(self, runtime: Runtime):
        """Test getting task counts."""
        await runtime.initialize()

        counts = await runtime.get_task_counts()
        assert "pending" in counts
        assert "in_progress" in counts
        assert "completed" in counts

    async def test_get_budget_summary(self, runtime: Runtime):
        """Test getting budget summary."""
        await runtime.initialize()

        summary = await runtime.get_budget_summary()
        assert "total_spent" in summary


class TestMultipleAgents:
    """Test runtime with multiple agents."""

    @pytest.fixture
    def config(self) -> Config:
        """Create a test configuration with multiple agents."""
        return Config(
            system=SystemConfig(isolation="none"),
            agents=[
                AgentConfig(name="agent-1", backend="local", role="Agent 1"),
                AgentConfig(name="agent-2", backend="local", role="Agent 2"),
                AgentConfig(name="agent-3", backend="local", role="Agent 3"),
            ],
        )

    @pytest.fixture
    def runtime(self, config: Config, tmp_path: Path) -> Runtime:
        """Create a runtime instance."""
        config.system.data_dir = tmp_path / ".xpressai"
        config.system.data_dir.mkdir(parents=True, exist_ok=True)
        return Runtime(config, workspace=tmp_path)

    async def test_start_all_agents(self, runtime: Runtime):
        """Test starting all agents."""
        await runtime.initialize()
        await runtime.start()

        agents = await runtime.list_agents()
        assert len(agents) == 3
        assert all(a.status == "running" for a in agents)

    async def test_stop_all_agents(self, runtime: Runtime):
        """Test stopping all agents."""
        await runtime.initialize()
        await runtime.start()
        await runtime.stop()

        agents = await runtime.list_agents()
        assert all(a.status == "stopped" for a in agents)

    async def test_start_specific_agent(self, runtime: Runtime):
        """Test starting only specific agents."""
        await runtime.initialize()

        await runtime.start_agent("agent-2")

        agents = await runtime.list_agents()
        statuses = {a.name: a.status for a in agents}

        assert statuses["agent-1"] == "stopped"
        assert statuses["agent-2"] == "running"
        assert statuses["agent-3"] == "stopped"


class TestAgentState:
    """Test AgentState dataclass."""

    def test_create_agent_state(self):
        """Test creating an agent state."""
        state = AgentState(
            id="test",
            name="test-agent",
            backend="local",
        )

        assert state.id == "test"
        assert state.name == "test-agent"
        assert state.backend == "local"
        assert state.status == "stopped"
        assert state.container_id is None
        assert state.started_at is None
        assert state.error_message is None

    def test_agent_state_with_error(self):
        """Test agent state with error."""
        state = AgentState(
            id="test",
            name="test-agent",
            backend="local",
            status="error",
            error_message="Connection failed",
        )

        assert state.status == "error"
        assert state.error_message == "Connection failed"


class TestRuntimeEvents:
    """Test runtime event emission."""

    @pytest.fixture
    def config(self) -> Config:
        """Create a test configuration."""
        return Config(
            system=SystemConfig(isolation="none"),
            agents=[AgentConfig(name="test-agent", backend="local")],
        )

    @pytest.fixture
    def runtime(self, config: Config, tmp_path: Path) -> Runtime:
        """Create a runtime instance."""
        config.system.data_dir = tmp_path / ".xpressai"
        config.system.data_dir.mkdir(parents=True, exist_ok=True)
        return Runtime(config, workspace=tmp_path)

    async def test_events_emitted_on_initialize(self, runtime: Runtime):
        """Test that events are emitted on initialize."""
        await runtime.initialize()

        # Event queue should have runtime.initialized
        event = await asyncio.wait_for(runtime._event_queue.get(), timeout=1.0)
        assert event.type == "runtime.initialized"

    async def test_events_emitted_on_start(self, runtime: Runtime):
        """Test that events are emitted on start."""
        await runtime.initialize()
        # Drain initialize event
        await runtime._event_queue.get()

        await runtime.start()

        # Should have: runtime.starting, agent.starting, agent.started, runtime.started
        events = []
        while not runtime._event_queue.empty():
            events.append(await runtime._event_queue.get())

        event_types = [e.type for e in events]
        assert "runtime.starting" in event_types
        assert "runtime.started" in event_types
        assert "agent.starting" in event_types
        assert "agent.started" in event_types
