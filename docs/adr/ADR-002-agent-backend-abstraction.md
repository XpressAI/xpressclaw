# ADR-002: Agent Backend Abstraction

## Status
Superseded by ADR-003 (Container Isolation)

> **Note:** This ADR was written for a Python in-process architecture.
> The Rust implementation uses Docker container harnesses (ADR-003) as the
> abstraction boundary instead of Rust trait objects. Each agent backend is
> a Docker image (e.g., `xpressclaw-harness-generic`, `xpressclaw-harness-claude-sdk`)
> that exposes an OpenAI-compatible HTTP API. The server routes messages to
> the harness container, which handles the agent reasoning loop internally
> and calls back to the server for LLM access and MCP tools.

## Context

XpressAI needs to support multiple agent backends:
- Claude Agent SDK (primary for developer teams)
- OpenAI Codex CLI
- Gemini CLI
- Aider
- LangChain agents
- CrewAI
- Xaibo (our framework)
- Local models (Qwen3-8B via llama.cpp/Ollama)

Each of these has different:
- APIs and SDKs
- Capability models (some stream, some don't)
- Tool integration patterns
- Session/conversation management

We need a common abstraction that:
1. Exposes a unified interface to the runtime
2. Preserves the unique capabilities of each backend
3. Allows the runtime to manage lifecycle, memory, and tools consistently

## Decision

We will implement an **Agent Backend Protocol** - an abstract interface that all backends must implement.

### Core Interface

```python
from abc import ABC, abstractmethod
from typing import AsyncIterator, Any
from dataclasses import dataclass

@dataclass
class AgentMessage:
    role: str  # "user", "assistant", "system", "tool_use", "tool_result"
    content: str | dict[str, Any]
    metadata: dict[str, Any] | None = None

@dataclass
class AgentConfig:
    name: str
    role: str  # System prompt / role description
    backend: str
    backend_config: dict[str, Any]
    allowed_tools: list[str]
    autonomy: str  # "low", "medium", "high"

class AgentBackend(ABC):
    """Abstract base class for all agent backends."""
    
    @abstractmethod
    async def initialize(self, config: AgentConfig) -> None:
        """Initialize the backend with configuration."""
        pass
    
    @abstractmethod
    async def send(self, message: str) -> AsyncIterator[AgentMessage]:
        """Send a message and stream responses."""
        pass
    
    @abstractmethod
    async def inject_memory(self, memories: list[str]) -> None:
        """Inject memories into the agent's context."""
        pass
    
    @abstractmethod
    async def register_tools(self, tools: list[MCPTool]) -> None:
        """Register MCP tools with the backend."""
        pass
    
    @abstractmethod
    async def interrupt(self) -> None:
        """Interrupt current operation."""
        pass
    
    @abstractmethod
    async def shutdown(self) -> None:
        """Gracefully shut down the backend."""
        pass
    
    @property
    @abstractmethod
    def supports_streaming(self) -> bool:
        """Whether this backend supports streaming responses."""
        pass
    
    @property
    @abstractmethod
    def supports_tools(self) -> bool:
        """Whether this backend supports tool use natively."""
        pass
```

### Backend Registry

```python
class BackendRegistry:
    """Discovers and instantiates agent backends."""
    
    _backends: dict[str, type[AgentBackend]] = {}
    
    @classmethod
    def register(cls, name: str, backend_class: type[AgentBackend]) -> None:
        cls._backends[name] = backend_class
    
    @classmethod
    def get(cls, name: str) -> type[AgentBackend]:
        if name not in cls._backends:
            raise UnknownBackendError(name)
        return cls._backends[name]
    
    @classmethod
    def available(cls) -> list[str]:
        return list(cls._backends.keys())

# Registration happens at module load
BackendRegistry.register("claude-code", ClaudeAgentBackend)
BackendRegistry.register("local", LocalModelBackend)
BackendRegistry.register("openai-codex", OpenAICodexBackend)
# etc.
```

### Claude Agent SDK Backend (Primary)

```python
class ClaudeAgentBackend(AgentBackend):
    """Backend using Claude Agent SDK for Claude Code-style agents."""
    
    def __init__(self):
        self.client: ClaudeSDKClient | None = None
        self.config: AgentConfig | None = None
    
    async def initialize(self, config: AgentConfig) -> None:
        self.config = config
        
        options = ClaudeAgentOptions(
            system_prompt=config.role,
            allowed_tools=config.allowed_tools,
            permission_mode=self._map_autonomy(config.autonomy),
            # MCP servers are registered separately
        )
        
        self.client = ClaudeSDKClient(options=options)
        await self.client.connect()
    
    async def send(self, message: str) -> AsyncIterator[AgentMessage]:
        await self.client.query(message)
        
        async for msg in self.client.receive_messages():
            yield self._convert_message(msg)
    
    # ... rest of implementation
```

### Local Model Backend

```python
class LocalModelBackend(AgentBackend):
    """Backend for local models (Qwen3-8B via llama.cpp or Ollama)."""
    
    async def initialize(self, config: AgentConfig) -> None:
        # Detect if Ollama is available, otherwise use llama.cpp
        self.inference = await self._setup_inference()
        
        # Local models need manual tool handling
        self.tool_handler = LocalToolHandler()
    
    async def send(self, message: str) -> AsyncIterator[AgentMessage]:
        # Build prompt with system message, memories, and tools
        prompt = self._build_prompt(message)
        
        async for token in self.inference.generate(prompt):
            # Parse for tool calls, handle them, continue
            yield self._parse_response(token)
```

### Capability Detection

Not all backends support all features. The runtime queries capabilities:

```python
backend = BackendRegistry.get(config.backend)()
await backend.initialize(config)

if not backend.supports_tools:
    # Fall back to prompt-based tool use
    backend = ToolPromptWrapper(backend)

if not backend.supports_streaming:
    # Buffer responses
    backend = StreamingAdapter(backend)
```

## Consequences

### Positive
- Clean separation between runtime and backends
- Easy to add new backends (implement the interface)
- Runtime can provide consistent experience regardless of backend
- Capability detection allows graceful degradation

### Negative
- Abstraction may hide useful backend-specific features
- Maintaining multiple backends is ongoing work
- Some backends may not map cleanly to the interface

### Implementation Notes

1. Start with two backends: `claude-code` and `local`
2. The `local` backend uses Qwen3-8B by default
3. Tool registration happens through MCP, which the backend adapts to its native format
4. Memory injection happens by modifying the system prompt or conversation history

## Related ADRs
- ADR-005: MCP Tool System
- ADR-011: Default Local Model
