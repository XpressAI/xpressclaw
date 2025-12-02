"""XpressAI Runtime - Main orchestration layer.

The runtime is the central coordinator for all XpressAI subsystems.
"""

import asyncio
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import AsyncIterator, Any
import logging

from xpressai.core.config import Config, load_config, AgentConfig
from xpressai.core.exceptions import (
    AgentNotFoundError,
    AgentAlreadyRunningError,
    AgentNotRunningError,
)

logger = logging.getLogger(__name__)


@dataclass
class AgentState:
    """Current state of an agent.

    Attributes:
        id: Unique agent identifier
        name: Display name
        backend: Backend type
        status: Current status
        container_id: Docker container ID if running in container
        started_at: When the agent was started
        error_message: Last error message if any
    """

    id: str
    name: str
    backend: str
    status: str = "stopped"  # stopped | starting | running | paused | error
    container_id: str | None = None
    started_at: datetime | None = None
    error_message: str | None = None


@dataclass
class RuntimeEvent:
    """Event emitted by the runtime.

    Attributes:
        type: Event type (e.g., "agent.started", "runtime.initialized")
        agent_id: Associated agent ID if applicable
        timestamp: When the event occurred
        data: Additional event data
    """

    type: str
    agent_id: str | None = None
    timestamp: datetime = field(default_factory=datetime.now)
    data: dict[str, Any] = field(default_factory=dict)


class Runtime:
    """Main XpressAI runtime orchestrator.

    Manages agent lifecycle, memory, tools, budgets, and events.
    Coordinates between all subsystems to provide a unified interface.
    """

    def __init__(self, config: Config | None = None, workspace: Path | None = None):
        """Initialize the runtime.

        Args:
            config: Configuration to use. Loads from file if not provided.
            workspace: Workspace directory. Defaults to current directory.
        """
        self.config = config or load_config()
        self.workspace = workspace or Path.cwd()

        # State
        self._agents: dict[str, AgentState] = {}
        self._event_queue: asyncio.Queue[RuntimeEvent] = asyncio.Queue()
        self._running = False
        self._initialized = False

        # Subsystems (lazily initialized)
        self._db = None
        self._memory = None
        self._task_board = None
        self._budget_manager = None
        self._docker = None
        self._sop_manager = None
        self._scheduler = None

        # Agent runners and backends
        self._runners: dict[str, Any] = {}  # AgentRunner instances
        self._backends: dict[str, Any] = {}  # AgentBackend instances

    @property
    def is_running(self) -> bool:
        """Check if runtime is running."""
        return self._running

    @property
    def is_initialized(self) -> bool:
        """Check if runtime is initialized."""
        return self._initialized

    async def initialize(self) -> None:
        """Initialize the runtime and all subsystems."""
        if self._initialized:
            return

        from xpressai.memory.database import Database
        from xpressai.memory.manager import MemoryManager
        from xpressai.tasks.board import TaskBoard
        from xpressai.budget.manager import BudgetManager
        from xpressai.tasks.sop import SOPManager
        from xpressai.tasks.scheduler import TaskScheduler

        # Initialize database
        db_path = self.config.system.data_dir / "xpressai.db"
        self._db = Database(db_path)

        # Initialize subsystems
        self._memory = MemoryManager(self._db, self.config.memory)
        self._task_board = TaskBoard(self._db)
        self._budget_manager = BudgetManager(self._db, self.config.system.budget)
        self._sop_manager = SOPManager(self.workspace / ".xpressai" / "sops")
        self._scheduler = TaskScheduler(self._task_board, self._db)

        # Register agents from config
        for agent_config in self.config.agents:
            self._agents[agent_config.name] = AgentState(
                id=agent_config.name,
                name=agent_config.name,
                backend=agent_config.backend,
            )

        self._initialized = True
        await self._emit_event(RuntimeEvent(type="runtime.initialized"))
        logger.info("Runtime initialized")

    async def start(self) -> None:
        """Start the runtime and all configured agents."""
        if not self._initialized:
            await self.initialize()

        if self._running:
            return

        self._running = True
        await self._emit_event(RuntimeEvent(type="runtime.starting"))

        # Start scheduler
        if self._scheduler:
            self._scheduler.start()

        # Start agents
        for agent_id in self._agents:
            try:
                await self.start_agent(agent_id)
            except Exception as e:
                logger.error(f"Failed to start agent {agent_id}: {e}")

        await self._emit_event(RuntimeEvent(type="runtime.started"))
        logger.info("Runtime started")

    async def stop(self, timeout: int = 10) -> None:
        """Stop the runtime and all agents.

        Args:
            timeout: Shutdown timeout per agent in seconds
        """
        if not self._running:
            return

        await self._emit_event(RuntimeEvent(type="runtime.stopping"))

        # Stop scheduler
        if self._scheduler:
            self._scheduler.stop()

        # Stop all agents
        for agent_id in list(self._agents.keys()):
            try:
                await self.stop_agent(agent_id, timeout=timeout)
            except Exception as e:
                logger.error(f"Failed to stop agent {agent_id}: {e}")

        # Stop Docker containers if any
        if self._docker:
            await self._docker.stop_all(timeout)

        self._running = False
        await self._emit_event(RuntimeEvent(type="runtime.stopped"))
        logger.info("Runtime stopped")

    async def start_agent(self, agent_id: str) -> AgentState:
        """Start a specific agent.

        Args:
            agent_id: ID of agent to start

        Returns:
            Updated agent state

        Raises:
            AgentNotFoundError: If agent doesn't exist
            AgentAlreadyRunningError: If agent is already running
        """
        if agent_id not in self._agents:
            raise AgentNotFoundError(f"Unknown agent: {agent_id}", {"agent_id": agent_id})

        agent = self._agents[agent_id]

        if agent.status == "running":
            raise AgentAlreadyRunningError(
                f"Agent already running: {agent_id}", {"agent_id": agent_id}
            )

        agent.status = "starting"
        await self._emit_event(RuntimeEvent(type="agent.starting", agent_id=agent_id))

        try:
            # Launch container if using Docker isolation
            if self.config.system.isolation == "docker":
                from xpressai.isolation.docker import DockerManager

                if self._docker is None:
                    self._docker = DockerManager()

                if self._docker.available:
                    container_id = await self._docker.launch_agent(agent_id, agent.backend)
                    agent.container_id = container_id
                else:
                    logger.warning("Docker not available, running without isolation")

            # Initialize backend
            from xpressai.agents.registry import get_backend
            from xpressai.agents.runner import AgentRunner

            agent_config = self._get_agent_config(agent_id)
            if agent_config:
                backend = await get_backend(
                    agent.backend,
                    agent_id,
                    agent_config,
                    mcp_servers=self.config.mcp_servers,
                )

                # Store backend for later use
                self._backends[agent_id] = backend

                # Create and start the runner
                runner = AgentRunner(
                    agent_id=agent_id,
                    backend=backend,
                    task_board=self._task_board,
                    sop_manager=self._sop_manager,
                )
                await runner.start()
                self._runners[agent_id] = runner

            agent.status = "running"
            agent.started_at = datetime.now()
            agent.error_message = None

            await self._emit_event(RuntimeEvent(type="agent.started", agent_id=agent_id))

            logger.info(f"Agent {agent_id} started (runner active)")

        except Exception as e:
            agent.status = "error"
            agent.error_message = str(e)

            await self._emit_event(
                RuntimeEvent(type="agent.error", agent_id=agent_id, data={"error": str(e)})
            )

            logger.error(f"Agent {agent_id} failed to start: {e}")

        return agent

    async def stop_agent(self, agent_id: str, timeout: int = 10) -> AgentState:
        """Stop a specific agent.

        Args:
            agent_id: ID of agent to stop
            timeout: Shutdown timeout in seconds

        Returns:
            Updated agent state
        """
        if agent_id not in self._agents:
            raise AgentNotFoundError(f"Unknown agent: {agent_id}", {"agent_id": agent_id})

        agent = self._agents[agent_id]

        if agent.status == "stopped":
            return agent

        await self._emit_event(RuntimeEvent(type="agent.stopping", agent_id=agent_id))

        try:
            # Stop runner if running
            if agent_id in self._runners:
                await self._runners[agent_id].stop()
                del self._runners[agent_id]

            # Clean up backend reference
            if agent_id in self._backends:
                del self._backends[agent_id]

            # Stop container if running
            if agent.container_id and self._docker:
                await self._docker.stop_agent(agent_id, timeout=timeout)
                agent.container_id = None

            # Shutdown backend via registry
            from xpressai.agents.registry import get_registry

            await get_registry().shutdown(agent_id)

            agent.status = "stopped"
            agent.started_at = None

            await self._emit_event(RuntimeEvent(type="agent.stopped", agent_id=agent_id))

            logger.info(f"Agent {agent_id} stopped")

        except Exception as e:
            agent.status = "error"
            agent.error_message = str(e)
            logger.error(f"Error stopping agent {agent_id}: {e}")

        return agent

    async def list_agents(self) -> list[AgentState]:
        """List all agents and their states.

        Returns:
            List of agent states
        """
        return list(self._agents.values())

    async def get_agent(self, agent_id: str) -> AgentState | None:
        """Get a specific agent's state.

        Args:
            agent_id: ID of agent

        Returns:
            Agent state or None
        """
        return self._agents.get(agent_id)

    async def send_to_agent(self, agent_id: str, message: str) -> AsyncIterator[str]:
        """Send a message to an agent and stream the response.

        Args:
            agent_id: ID of agent
            message: Message to send

        Yields:
            Response text chunks
        """
        agent = self._agents.get(agent_id)
        if not agent:
            raise AgentNotFoundError(f"Unknown agent: {agent_id}", {"agent_id": agent_id})

        if agent.status != "running":
            raise AgentNotRunningError(
                f"Agent {agent_id} is not running", {"agent_id": agent_id, "status": agent.status}
            )

        # Get backend and send message
        from xpressai.agents.registry import get_backend

        agent_config = self._get_agent_config(agent_id)
        backend = await get_backend(
            agent.backend,
            agent_id,
            agent_config,
            mcp_servers=self.config.mcp_servers,
        )

        # Inject memory context
        if self._memory:
            context = await self._memory.get_context_for_agent(agent_id)
            if context:
                await backend.inject_memory(context)

        # Send message and stream response
        async for chunk in backend.send(message):
            yield chunk

    async def activity_stream(self) -> AsyncIterator[RuntimeEvent]:
        """Stream runtime events.

        Yields:
            Runtime events as they occur
        """
        while self._running:
            try:
                event = await asyncio.wait_for(self._event_queue.get(), timeout=1.0)
                yield event
            except asyncio.TimeoutError:
                continue

    async def get_task_counts(self) -> dict[str, int]:
        """Get counts of tasks by status.

        Returns:
            Dict of status -> count
        """
        if self._task_board:
            return await self._task_board.get_counts()
        return {"pending": 0, "in_progress": 0, "completed": 0}

    async def get_budget_summary(self, agent_id: str | None = None) -> dict[str, Any]:
        """Get budget summary.

        Args:
            agent_id: Optional agent filter

        Returns:
            Budget summary dict
        """
        if self._budget_manager:
            return await self._budget_manager.get_summary(agent_id)
        return {"total_spent": 0, "limit": None}

    def _get_agent_config(self, agent_id: str) -> AgentConfig | None:
        """Get config for an agent."""
        for agent in self.config.agents:
            if agent.name == agent_id:
                return agent
        return None

    async def _emit_event(self, event: RuntimeEvent) -> None:
        """Emit an event to all listeners."""
        await self._event_queue.put(event)


# Global runtime instance
_runtime: Runtime | None = None


def get_runtime() -> Runtime:
    """Get or create the global runtime instance.

    Loads config from xpressai.yaml in current directory if available.

    Returns:
        Global Runtime instance
    """
    global _runtime
    if _runtime is None:
        # Try to load config from current directory
        config_path = Path.cwd() / "xpressai.yaml"
        if config_path.exists():
            config = load_config(config_path)
            _runtime = Runtime(config)
        else:
            _runtime = Runtime()
    return _runtime


async def initialize_runtime(config: Config | None = None) -> Runtime:
    """Initialize and return the runtime.

    Args:
        config: Configuration to use

    Returns:
        Initialized runtime
    """
    global _runtime
    _runtime = Runtime(config)
    await _runtime.initialize()
    return _runtime
