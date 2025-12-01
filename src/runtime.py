"""XpressAI Runtime - Main orchestration layer."""

import asyncio
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import AsyncIterator, Any

from xpressai.core.config import Config, load_config


@dataclass
class AgentState:
    """Current state of an agent."""
    id: str
    name: str
    backend: str
    status: str = "stopped"  # stopped | starting | running | error
    container_id: str | None = None
    started_at: datetime | None = None
    error_message: str | None = None


@dataclass
class RuntimeEvent:
    """Event emitted by the runtime."""
    type: str
    agent_id: str | None = None
    timestamp: datetime = field(default_factory=datetime.now)
    data: dict[str, Any] = field(default_factory=dict)


class Runtime:
    """Main XpressAI runtime orchestrator.
    
    Manages agent lifecycle, memory, tools, budgets, and events.
    """
    
    def __init__(self, config: Config | None = None, workspace: Path | None = None):
        self.config = config or load_config()
        self.workspace = workspace or Path.cwd()
        
        # State
        self._agents: dict[str, AgentState] = {}
        self._event_queue: asyncio.Queue[RuntimeEvent] = asyncio.Queue()
        self._running = False
        
        # Subsystems (lazily initialized)
        self._db = None
        self._memory = None
        self._task_board = None
        self._budget_manager = None
        self._docker = None
    
    async def initialize(self) -> None:
        """Initialize the runtime and all subsystems."""
        from xpressai.memory.database import Database
        from xpressai.memory.manager import MemoryManager
        from xpressai.tasks.board import TaskBoard
        from xpressai.budget.manager import BudgetManager
        
        # Initialize database
        db_path = Path.home() / ".xpressai" / "xpressai.db"
        self._db = Database(db_path)
        
        # Initialize subsystems
        self._memory = MemoryManager(self._db, self.config.memory)
        self._task_board = TaskBoard(self._db)
        self._budget_manager = BudgetManager(self._db, self.config.system.budget)
        
        # Register agents from config
        for agent_config in self.config.agents:
            self._agents[agent_config.name] = AgentState(
                id=agent_config.name,
                name=agent_config.name,
                backend=agent_config.backend,
            )
        
        await self._emit_event(RuntimeEvent(type="runtime.initialized"))
    
    async def start(self) -> None:
        """Start the runtime and all configured agents."""
        if self._running:
            return
        
        self._running = True
        await self._emit_event(RuntimeEvent(type="runtime.starting"))
        
        # Start agents
        for agent_id in self._agents:
            await self.start_agent(agent_id)
        
        await self._emit_event(RuntimeEvent(type="runtime.started"))
    
    async def stop(self, timeout: int = 10) -> None:
        """Stop the runtime and all agents."""
        if not self._running:
            return
        
        await self._emit_event(RuntimeEvent(type="runtime.stopping"))
        
        # Stop all agents
        for agent_id in list(self._agents.keys()):
            await self.stop_agent(agent_id, timeout=timeout)
        
        self._running = False
        await self._emit_event(RuntimeEvent(type="runtime.stopped"))
    
    async def start_agent(self, agent_id: str) -> AgentState:
        """Start a specific agent."""
        if agent_id not in self._agents:
            raise ValueError(f"Unknown agent: {agent_id}")
        
        agent = self._agents[agent_id]
        
        if agent.status == "running":
            return agent
        
        agent.status = "starting"
        await self._emit_event(RuntimeEvent(
            type="agent.starting", 
            agent_id=agent_id
        ))
        
        try:
            # Launch container if using Docker isolation
            if self.config.system.isolation == "docker":
                from xpressai.isolation.docker import DockerManager
                if self._docker is None:
                    self._docker = DockerManager()
                
                container_id = await self._docker.launch_agent(agent_id, agent.backend)
                agent.container_id = container_id
            
            agent.status = "running"
            agent.started_at = datetime.now()
            agent.error_message = None
            
            await self._emit_event(RuntimeEvent(
                type="agent.started",
                agent_id=agent_id
            ))
            
        except Exception as e:
            agent.status = "error"
            agent.error_message = str(e)
            
            await self._emit_event(RuntimeEvent(
                type="agent.error",
                agent_id=agent_id,
                data={"error": str(e)}
            ))
        
        return agent
    
    async def stop_agent(self, agent_id: str, timeout: int = 10) -> AgentState:
        """Stop a specific agent."""
        if agent_id not in self._agents:
            raise ValueError(f"Unknown agent: {agent_id}")
        
        agent = self._agents[agent_id]
        
        if agent.status == "stopped":
            return agent
        
        await self._emit_event(RuntimeEvent(
            type="agent.stopping",
            agent_id=agent_id
        ))
        
        try:
            # Stop container if running
            if agent.container_id and self._docker:
                await self._docker.stop_agent(agent_id, timeout=timeout)
                agent.container_id = None
            
            agent.status = "stopped"
            agent.started_at = None
            
            await self._emit_event(RuntimeEvent(
                type="agent.stopped",
                agent_id=agent_id
            ))
            
        except Exception as e:
            agent.status = "error"
            agent.error_message = str(e)
        
        return agent
    
    async def list_agents(self) -> list[AgentState]:
        """List all agents and their states."""
        return list(self._agents.values())
    
    async def get_agent(self, agent_id: str) -> AgentState | None:
        """Get a specific agent's state."""
        return self._agents.get(agent_id)
    
    async def send_to_agent(
        self, 
        agent_id: str, 
        message: str
    ) -> AsyncIterator[str]:
        """Send a message to an agent and stream the response."""
        agent = self._agents.get(agent_id)
        if not agent:
            raise ValueError(f"Unknown agent: {agent_id}")
        
        if agent.status != "running":
            raise RuntimeError(f"Agent {agent_id} is not running")
        
        # Get backend and send message
        from xpressai.agents.registry import get_backend
        backend = await get_backend(agent.backend, agent_id)
        
        async for chunk in backend.send(message):
            yield chunk
    
    async def activity_stream(self) -> AsyncIterator[RuntimeEvent]:
        """Stream runtime events."""
        while self._running:
            try:
                event = await asyncio.wait_for(
                    self._event_queue.get(), 
                    timeout=1.0
                )
                yield event
            except asyncio.TimeoutError:
                continue
    
    async def agent_events(self) -> AsyncIterator[RuntimeEvent]:
        """Stream agent-specific events."""
        async for event in self.activity_stream():
            if event.type.startswith("agent."):
                yield event
    
    async def get_task_counts(self) -> dict[str, int]:
        """Get counts of tasks by status."""
        if self._task_board:
            return await self._task_board.get_counts()
        return {"pending": 0, "in_progress": 0, "completed": 0}
    
    async def get_budget_summary(self) -> dict[str, Any]:
        """Get budget summary for all agents."""
        if self._budget_manager:
            return await self._budget_manager.get_summary()
        return {"total_spent": 0, "limit": None}
    
    async def _emit_event(self, event: RuntimeEvent) -> None:
        """Emit an event to all listeners."""
        await self._event_queue.put(event)


# Global runtime instance
_runtime: Runtime | None = None


def get_runtime() -> Runtime:
    """Get or create the global runtime instance."""
    global _runtime
    if _runtime is None:
        _runtime = Runtime()
    return _runtime


async def initialize_runtime(config: Config | None = None) -> Runtime:
    """Initialize and return the runtime."""
    global _runtime
    _runtime = Runtime(config)
    await _runtime.initialize()
    return _runtime
