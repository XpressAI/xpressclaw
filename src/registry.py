"""Agent backend registry and management."""

from abc import ABC, abstractmethod
from dataclasses import dataclass
from typing import AsyncIterator, Any

from xpressai.core.config import AgentConfig


@dataclass
class AgentMessage:
    """A message from an agent."""
    role: str  # "user", "assistant", "system", "tool_use", "tool_result"
    content: str | dict[str, Any]
    metadata: dict[str, Any] | None = None


class AgentBackend(ABC):
    """Abstract base class for agent backends."""
    
    @abstractmethod
    async def initialize(self, config: AgentConfig) -> None:
        """Initialize the backend with configuration."""
        pass
    
    @abstractmethod
    async def send(self, message: str) -> AsyncIterator[str]:
        """Send a message and stream response chunks."""
        pass
    
    @abstractmethod
    async def shutdown(self) -> None:
        """Gracefully shut down the backend."""
        pass
    
    @property
    def model(self) -> str:
        """The model being used."""
        return "unknown"


class ClaudeAgentBackend(AgentBackend):
    """Backend using Claude Agent SDK."""
    
    def __init__(self):
        self._client = None
        self._config = None
    
    async def initialize(self, config: AgentConfig) -> None:
        self._config = config
        
        try:
            from claude_agent_sdk import ClaudeSDKClient, ClaudeAgentOptions
            
            options = ClaudeAgentOptions(
                system_prompt=config.role,
                allowed_tools=config.tools,
            )
            
            self._client = ClaudeSDKClient(options=options)
            await self._client.connect()
        except ImportError:
            raise RuntimeError("claude-agent-sdk not installed")
    
    async def send(self, message: str) -> AsyncIterator[str]:
        if self._client is None:
            raise RuntimeError("Backend not initialized")
        
        from claude_agent_sdk import AssistantMessage, TextBlock
        
        await self._client.query(message)
        
        async for msg in self._client.receive_messages():
            if isinstance(msg, AssistantMessage):
                for block in msg.content:
                    if isinstance(block, TextBlock):
                        yield block.text
    
    async def shutdown(self) -> None:
        if self._client:
            await self._client.disconnect()
            self._client = None
    
    @property
    def model(self) -> str:
        return "claude-sonnet"


class LocalModelBackend(AgentBackend):
    """Backend for local models via Ollama."""
    
    def __init__(self):
        self._model = "qwen3:8b"
        self._base_url = "http://localhost:11434"
        self._config = None
    
    async def initialize(self, config: AgentConfig) -> None:
        self._config = config
        # Could check if Ollama is running here
    
    async def send(self, message: str) -> AsyncIterator[str]:
        import aiohttp
        
        messages = [
            {"role": "system", "content": self._config.role if self._config else ""},
            {"role": "user", "content": message},
        ]
        
        async with aiohttp.ClientSession() as session:
            async with session.post(
                f"{self._base_url}/api/chat",
                json={
                    "model": self._model,
                    "messages": messages,
                    "stream": True,
                }
            ) as response:
                async for line in response.content:
                    if line:
                        import json
                        try:
                            data = json.loads(line)
                            if "message" in data and "content" in data["message"]:
                                yield data["message"]["content"]
                        except json.JSONDecodeError:
                            pass
    
    async def shutdown(self) -> None:
        pass
    
    @property
    def model(self) -> str:
        return self._model


# Backend registry
_backends: dict[str, type[AgentBackend]] = {
    "claude-code": ClaudeAgentBackend,
    "local": LocalModelBackend,
}

# Active backend instances
_instances: dict[str, AgentBackend] = {}


def register_backend(name: str, backend_class: type[AgentBackend]) -> None:
    """Register a new backend type."""
    _backends[name] = backend_class


async def get_backend(backend_type: str, agent_id: str) -> AgentBackend:
    """Get or create a backend instance for an agent."""
    key = f"{agent_id}:{backend_type}"
    
    if key not in _instances:
        if backend_type not in _backends:
            raise ValueError(f"Unknown backend: {backend_type}")
        
        _instances[key] = _backends[backend_type]()
    
    return _instances[key]


def available_backends() -> list[str]:
    """List available backend types."""
    return list(_backends.keys())
