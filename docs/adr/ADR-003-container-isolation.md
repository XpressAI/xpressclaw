# ADR-003: Container-based Agent Isolation

## Status
Accepted

## Context

Agents can be dangerous. They execute code, modify files, make network requests, and spend money. When something goes wrong, the blast radius should be minimized.

Xpress AI's production system uses Kubernetes for isolation, but that's overkill for most users. We need isolation that:
- Works on a developer's laptop
- Doesn't require Kubernetes knowledge
- Provides meaningful security boundaries
- Allows controlled access to resources (files, network, GPU)

## Decision

We will use **Docker containers** as the primary isolation mechanism.

### Container Architecture

```
┌─────────────────────────────────────────────────┐
│                 XpressAI Runtime                │
│  (Host process: CLI, TUI, Web UI, Orchestrator) │
└───────────────┬─────────────────────────────────┘
                │
    ┌───────────┼───────────┐
    │           │           │
┌───▼───┐   ┌───▼───┐   ┌───▼───┐
│Agent 1│   │Agent 2│   │Agent 3│
│(Docker│   │(Docker│   │(Docker│
│  ctr) │   │  ctr) │   │  ctr) │
└───────┘   └───────┘   └───────┘
```

Each agent runs in its own container with:
- Isolated filesystem (with optional mounts)
- Isolated network (with optional egress rules)
- Resource limits (CPU, memory)
- GPU passthrough when needed

### Container Specification

```python
@dataclass
class ContainerSpec:
    image: str = "xpressai/agent-runtime:latest"
    
    # Resource limits
    memory_limit: str = "2g"
    cpu_limit: float = 1.0
    gpu: bool = False  # Passthrough GPU
    
    # Filesystem
    mounts: list[Mount] = field(default_factory=list)
    read_only_root: bool = True
    
    # Network
    network_mode: str = "bridge"  # bridge, none, host
    allowed_egress: list[str] = field(default_factory=list)  # domains
    
    # Security
    no_new_privileges: bool = True
    drop_capabilities: list[str] = field(default_factory=lambda: ["ALL"])
    add_capabilities: list[str] = field(default_factory=list)

@dataclass
class Mount:
    host_path: str
    container_path: str
    read_only: bool = False
```

### Container Management

```python
class DockerIsolation:
    """Manages Docker containers for agent isolation."""
    
    def __init__(self):
        self.client = docker.from_env()
        self.containers: dict[str, Container] = {}
    
    async def launch_agent(
        self, 
        agent_id: str, 
        spec: ContainerSpec,
        agent_config: AgentConfig
    ) -> Container:
        """Launch an agent in a new container."""
        
        container = self.client.containers.run(
            spec.image,
            detach=True,
            name=f"xpressai-{agent_id}",
            mem_limit=spec.memory_limit,
            nano_cpus=int(spec.cpu_limit * 1e9),
            mounts=[self._to_docker_mount(m) for m in spec.mounts],
            read_only=spec.read_only_root,
            network_mode=spec.network_mode,
            security_opt=["no-new-privileges"] if spec.no_new_privileges else [],
            cap_drop=spec.drop_capabilities,
            cap_add=spec.add_capabilities,
            device_requests=self._gpu_request() if spec.gpu else None,
            environment={
                "XPRESSAI_AGENT_ID": agent_id,
                "XPRESSAI_AGENT_CONFIG": json.dumps(asdict(agent_config)),
            },
        )
        
        self.containers[agent_id] = container
        return container
    
    async def exec_in_agent(
        self, 
        agent_id: str, 
        command: list[str]
    ) -> tuple[int, str]:
        """Execute a command in an agent's container."""
        container = self.containers[agent_id]
        result = container.exec_run(command)
        return result.exit_code, result.output.decode()
    
    async def stop_agent(self, agent_id: str, timeout: int = 10) -> None:
        """Gracefully stop an agent container."""
        container = self.containers.get(agent_id)
        if container:
            container.stop(timeout=timeout)
            container.remove()
            del self.containers[agent_id]
```

### Base Image

We'll provide a base image that includes:
- Python 3.11+ runtime
- Common agent dependencies (claude-agent-sdk, langchain, etc.)
- MCP client libraries
- Qwen3-8B weights (in a separate image variant for local inference)

```dockerfile
# Dockerfile.agent-runtime
FROM python:3.11-slim

# Install system dependencies
RUN apt-get update && apt-get install -y \
    git curl build-essential \
    && rm -rf /var/lib/apt/lists/*

# Install Python dependencies
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

# XpressAI agent entrypoint
COPY entrypoint.py /entrypoint.py
ENTRYPOINT ["python", "/entrypoint.py"]
```

### Communication Between Runtime and Container

The runtime communicates with agent containers via:
1. **Unix socket** mounted into the container for MCP
2. **Exec** for one-off commands
3. **Logs** streaming for observability

```python
# Inside the container, the agent connects to the runtime
mcp_socket = os.environ.get("XPRESSAI_MCP_SOCKET", "/run/xpressai/mcp.sock")
runtime_client = XpressAIRuntimeClient(mcp_socket)

# Agent can request tools, memory, etc. through this socket
tools = await runtime_client.list_tools()
memories = await runtime_client.get_memories(query="project deadlines")
```

### Fallback: No Isolation Mode

For development or trusted environments:

```yaml
system:
  isolation: none  # Run agents in-process (no containers)
```

This skips Docker entirely and runs agents in the same process as the runtime.

## Consequences

### Positive
- Strong isolation between agents and host system
- Familiar technology (Docker) with good tooling
- Resource limits prevent runaway agents
- GPU passthrough works for local models
- Portable across Linux, macOS (Docker Desktop), Windows (WSL2)

### Negative
- Docker is a dependency (though common)
- Container startup adds latency (~1-2 seconds)
- File permission issues when mounting host directories
- GPU passthrough can be tricky on non-Linux

### Risks
- Docker socket access is powerful; must secure runtime
- Container escapes are possible (defense in depth)
- macOS/Windows Docker has performance overhead

## Implementation Notes

1. Use `docker-py` for Python Docker integration
2. Pre-pull base images on `xpressai init`
3. Support Podman as an alternative (same API via `podman-docker`)
4. GPU: use NVIDIA Container Toolkit on Linux, Docker Desktop GPU on Windows/macOS

## Related ADRs
- ADR-002: Agent Backend Abstraction
- ADR-011: Default Local Model (GPU requirements)
