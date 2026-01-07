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
        self._conversation_manager = None
        self._budget_manager = None
        self._docker = None
        self._sop_manager = None
        self._scheduler = None
        self._tool_registry = None
        self._activity = None

        # Agent runners and backends
        self._runners: dict[str, Any] = {}  # AgentRunner instances
        self._backends: dict[str, Any] = {}  # AgentBackend instances

        # Memory sub-agent backends (per-agent, separate context to avoid conversation pollution)
        self._memory_backends: dict[str, Any] = {}  # cache_key -> backend instance

    @property
    def is_running(self) -> bool:
        """Check if runtime is running."""
        return self._running

    @property
    def is_initialized(self) -> bool:
        """Check if runtime is initialized."""
        return self._initialized

    @property
    def memory_manager(self):
        """Get the memory manager instance."""
        return self._memory

    @property
    def task_board(self):
        """Get the task board instance."""
        return self._task_board

    @property
    def sop_manager(self):
        """Get the SOP manager instance."""
        return self._sop_manager

    @property
    def conversation_manager(self):
        """Get the conversation manager instance."""
        return self._conversation_manager

    @property
    def activity_manager(self):
        """Get the activity manager instance."""
        return self._activity

    @property
    def memory_backends(self):
        """Get the memory sub-agent backends dict.

        Each agent gets its own memory backend (same type as main backend)
        with a separate conversation history to avoid polluting the main
        agent conversations.
        """
        return self._memory_backends

    async def get_memory_backend(self, agent_config: AgentConfig | None = None):
        """Get or create the memory sub-agent backend.

        Creates a backend of the same type as the agent, with a separate
        conversation context for memory operations.

        Priority for configuration:
        1. memory.memory_agent if explicitly configured (user override with local model)
        2. Same backend type as the agent (default - uses agent's model with separate context)

        Args:
            agent_config: The agent config to derive backend type and settings from

        Returns:
            Backend instance for memory operations, or None if unavailable
        """
        from xpressai.agents.registry import get_registry

        # 1. Check for explicit memory_agent override (always uses local model)
        if self.config.memory and self.config.memory.memory_agent:
            model_config = self.config.memory.memory_agent
            logger.debug("Using explicit memory_agent config (local model)")

            # Cache key for explicit memory agent
            cache_key = f"memory:{model_config.base_url}:{model_config.model}"
            if cache_key in self._memory_backends:
                return self._memory_backends[cache_key]

            try:
                from xpressai.agents.local import LocalModelBackend

                backend = LocalModelBackend()
                backend.configure_model(model_config)
                backend._system_prompt = self._get_memory_system_prompt()
                await backend._check_server()

                self._memory_backends[cache_key] = backend
                logger.info(f"Memory sub-agent initialized with explicit config: {model_config.model}")
                return backend

            except Exception as e:
                logger.warning(f"Failed to initialize explicit memory backend: {e}")
                return None

        # 2. Use same backend type as the agent (default)
        if not agent_config:
            logger.debug("No agent config provided for memory backend")
            return None

        backend_type = agent_config.backend
        cache_key = f"memory:{agent_config.name}:{backend_type}"

        if cache_key in self._memory_backends:
            # Clear history on reuse to ensure fresh context
            backend = self._memory_backends[cache_key]
            if hasattr(backend, 'clear_history'):
                backend.clear_history()
            return backend

        try:
            registry = get_registry()

            # Create a new backend of the same type
            backend_class = registry._backends.get(backend_type)
            if not backend_class:
                logger.warning(f"Unknown backend type for memory: {backend_type}")
                return None

            backend = backend_class()

            # Configure based on backend type
            if backend_type == "local":
                # Local model - use agent's local_model config or global
                model_config = agent_config.local_model or self.config.local_model
                if model_config and hasattr(backend, "configure_model"):
                    backend.configure_model(model_config)

            # Create a memory-focused config for initialization
            # Memory backends don't need MCP servers or tools - they're just for text analysis
            memory_agent_config = AgentConfig(
                name=f"{agent_config.name}_memory",
                backend=backend_type,
                role=self._get_memory_system_prompt(),
            )

            # Do NOT configure MCP servers for memory backends - they don't need tools
            # and spawning extra subprocesses wastes resources

            await backend.initialize(memory_agent_config)

            self._memory_backends[cache_key] = backend
            logger.info(f"Memory sub-agent initialized with {backend_type} backend for {agent_config.name}")
            return backend

        except Exception as e:
            logger.warning(f"Failed to initialize memory backend ({backend_type}): {e}")
            return None

    def _get_memory_system_prompt(self) -> str:
        """Get the system prompt for memory analysis."""
        return (
            "You are a memory analysis assistant. Your role is to:\n"
            "1. Evaluate if memories are relevant to the current context\n"
            "2. Analyze conversations to extract important information to remember\n"
            "3. Rewrite memories to be more useful based on how they were used\n"
            "Be concise and focused. Reply with just the requested format."
        )

    async def initialize(self) -> None:
        """Initialize the runtime and all subsystems."""
        if self._initialized:
            return

        from xpressai.memory.database import Database
        from xpressai.memory.manager import MemoryManager
        from xpressai.tasks.board import TaskBoard
        from xpressai.tasks.conversation import ConversationManager
        from xpressai.budget.manager import BudgetManager
        from xpressai.tasks.sop import SOPManager
        from xpressai.tasks.scheduler import TaskScheduler
        from xpressai.tools.registry import ToolRegistry
        from xpressai.core.activity import ActivityManager

        # Initialize database
        db_path = self.config.system.data_dir / "xpressai.db"
        self._db = Database(db_path)

        # Initialize subsystems
        self._activity = ActivityManager(self._db)
        self._memory = MemoryManager(self._db, self.config.memory)
        self._task_board = TaskBoard(self._db)
        self._conversation_manager = ConversationManager(self._db, self._task_board)
        self._budget_manager = BudgetManager(self._db, self.config.system.budget)
        self._sop_manager = SOPManager(self.workspace / ".xpressai" / "sops")
        self._scheduler = TaskScheduler(self._task_board, self._db)

        # Set workspace for builtin tools (file operations, shell commands)
        from xpressai.tools.builtin.filesystem import set_workspace
        set_workspace(self.config.system.workspace_dir)
        logger.info(f"Workspace set to: {self.config.system.workspace_dir}")

        # Initialize tool registry with builtin tools
        self._tool_registry = ToolRegistry()
        await self._tool_registry.initialize()
        logger.info(f"Tool registry initialized with {len(self._tool_registry.list_tools())} tools")

        # Set up ask_user tool with conversation manager
        from xpressai.tools.builtin.ask_user import set_conversation_manager
        set_conversation_manager(self._conversation_manager)

        # Register agents from config
        for agent_config in self.config.agents:
            self._agents[agent_config.name] = AgentState(
                id=agent_config.name,
                name=agent_config.name,
                backend=agent_config.backend,
            )

        # Memory sub-agent is initialized lazily via get_memory_backend()
        # to use the same LLM config as the agent being chatted with

        self._initialized = True
        await self._emit_event(RuntimeEvent(type="runtime.initialized"))

        # Log startup
        from xpressai.core.activity import EventType
        await self._activity.log(EventType.SYSTEM_STARTUP, data={"agents": list(self._agents.keys())})

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

        # Log shutdown
        if self._activity:
            from xpressai.core.activity import EventType
            await self._activity.log(EventType.SYSTEM_SHUTDOWN)

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

                # Configure tools for local backends
                if agent.backend == "local" and self._tool_registry:
                    # Get tool schemas for the backend
                    tool_schemas = self._tool_registry.get_tool_schemas()
                    if hasattr(backend, 'register_tools'):
                        await backend.register_tools(tool_schemas)
                        logger.info(f"Registered {len(tool_schemas)} tools for {agent_id}")

                    # Set tool format from config
                    local_model_config = agent_config.local_model or self.config.local_model
                    if local_model_config and hasattr(backend, 'set_tool_format'):
                        backend.set_tool_format(local_model_config.tool_format)

                # Get max_tool_calls from config and register pricing
                max_tool_calls = 20  # default
                local_model_config = agent_config.local_model or self.config.local_model
                if local_model_config:
                    max_tool_calls = local_model_config.max_tool_calls

                    # Register custom pricing for local model if set
                    if (local_model_config.price_input is not None and
                        local_model_config.price_output is not None and
                        self._budget_manager):
                        self._budget_manager.register_model_pricing(
                            local_model_config.model,
                            local_model_config.price_input,
                            local_model_config.price_output
                        )
                        logger.info(f"Registered pricing for {local_model_config.model}: "
                                    f"${local_model_config.price_input}/M input, "
                                    f"${local_model_config.price_output}/M output")

                # Create memory backend factory for this agent
                async def memory_backend_factory(ac=agent_config):
                    return await self.get_memory_backend(ac)

                # Create and start the runner
                runner = AgentRunner(
                    agent_id=agent_id,
                    backend=backend,
                    task_board=self._task_board,
                    sop_manager=self._sop_manager,
                    conversation_manager=self._conversation_manager,
                    tool_registry=self._tool_registry if agent.backend == "local" else None,
                    activity_manager=self._activity,
                    memory_manager=self._memory,
                    memory_config=self.config.memory,
                    agent_config=agent_config,
                    memory_backend_factory=memory_backend_factory,
                    budget_manager=self._budget_manager,
                )
                runner.max_tool_calls = max_tool_calls
                await runner.start()
                self._runners[agent_id] = runner

            agent.status = "running"
            agent.started_at = datetime.now()
            agent.error_message = None

            await self._emit_event(RuntimeEvent(type="agent.started", agent_id=agent_id))

            # Log agent started
            if self._activity:
                from xpressai.core.activity import EventType
                await self._activity.log(
                    EventType.AGENT_STARTED,
                    agent_id=agent_id,
                    data={"backend": agent.backend}
                )

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

            # Log agent stopped
            if self._activity:
                from xpressai.core.activity import EventType
                await self._activity.log(EventType.AGENT_STOPPED, agent_id=agent_id)

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

    def register_agent(self, agent_config: "AgentConfig") -> AgentState:
        """Register a new agent dynamically.

        Args:
            agent_config: Configuration for the new agent

        Returns:
            The new agent state

        Raises:
            ValueError: If agent already exists
        """
        if agent_config.name in self._agents:
            raise ValueError(f"Agent '{agent_config.name}' already exists")

        # Add to config's agent list
        self.config.agents.append(agent_config)

        # Create agent state
        agent = AgentState(
            id=agent_config.name,
            name=agent_config.name,
            backend=agent_config.backend,
        )
        self._agents[agent_config.name] = agent

        logger.info(f"Registered new agent: {agent_config.name}")
        return agent

    def reload_config(self) -> None:
        """Reload configuration from xpressai.yaml and register any new agents."""
        from pathlib import Path
        from xpressai.core.config import load_config

        config_path = Path.cwd() / "xpressai.yaml"
        if not config_path.exists():
            logger.warning("No xpressai.yaml found for reload")
            return

        new_config = load_config(config_path)

        # Find new agents that aren't already registered
        existing_names = set(self._agents.keys())
        for agent_config in new_config.agents:
            if agent_config.name not in existing_names:
                self.register_agent(agent_config)

        # Update the config reference for any new settings
        self.config = new_config
        logger.info("Configuration reloaded")

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
        return {"pending": 0, "in_progress": 0, "waiting_for_input": 0, "completed": 0}

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

    async def get_top_spenders(self, limit: int = 3) -> list[dict[str, Any]]:
        """Get top spending agents.

        Args:
            limit: Number of top spenders to return

        Returns:
            List of agent spending summaries
        """
        if self._budget_manager:
            return await self._budget_manager.get_top_spenders(limit)
        return []

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
