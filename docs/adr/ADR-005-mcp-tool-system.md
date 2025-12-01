# ADR-005: MCP Tool System

## Status
Accepted

## Context

Agents need tools to interact with the world: filesystems, APIs, databases, email, etc. The tool landscape is fragmented:
- Each agent framework has its own tool format
- No standard for tool discovery or permissions
- Security boundaries are inconsistent

The **Model Context Protocol (MCP)** is emerging as a standard for LLM tool integration. It provides:
- A standard JSON-RPC protocol for tools
- Server/client architecture
- Resource and prompt abstractions alongside tools
- Growing ecosystem of MCP servers

## Decision

We will use **MCP as the universal tool standard** for XpressAI.

All tools, whether built-in or custom, will be exposed as MCP servers. Agent backends that don't natively support MCP will have adapters that translate.

### Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    XpressAI Runtime                     │
│  ┌─────────────────────────────────────────────────┐    │
│  │              MCP Tool Registry                   │    │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐           │    │
│  │  │Filesystem│ │  Web    │ │  Shell  │ ...       │    │
│  │  │ Server  │ │ Server  │ │ Server  │           │    │
│  │  └────┬────┘ └────┬────┘ └────┬────┘           │    │
│  └───────┼───────────┼───────────┼─────────────────┘    │
│          │           │           │                      │
│          └───────────┴───────────┴───────────┐          │
│                                              │          │
│  ┌─────────────────────────────────────────▼─────┐     │
│  │           MCP Router / Permission Layer       │     │
│  └─────────────────────────────────────────┬─────┘     │
│                                            │            │
└────────────────────────────────────────────┼────────────┘
                                             │
                    ┌────────────────────────┼────────────────────────┐
                    │                        │                        │
              ┌─────▼─────┐            ┌─────▼─────┐            ┌─────▼─────┐
              │  Agent 1  │            │  Agent 2  │            │  Agent 3  │
              │ (Claude)  │            │  (Local)  │            │(LangChain)│
              └───────────┘            └───────────┘            └───────────┘
```

### Built-in MCP Servers

XpressAI provides built-in tools as MCP servers:

```python
# tools/builtin/filesystem.py

from mcp import Server, Tool
from mcp.types import TextContent

class FilesystemServer(Server):
    """MCP server for filesystem operations."""
    
    def __init__(self, allowed_paths: list[str], read_only: bool = False):
        super().__init__("xpressai-filesystem")
        self.allowed_paths = allowed_paths
        self.read_only = read_only
    
    @property
    def tools(self) -> list[Tool]:
        tools = [
            Tool(
                name="read_file",
                description="Read contents of a file",
                inputSchema={
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path"}
                    },
                    "required": ["path"]
                }
            ),
            Tool(
                name="list_directory",
                description="List contents of a directory",
                inputSchema={
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Directory path"}
                    },
                    "required": ["path"]
                }
            ),
        ]
        
        if not self.read_only:
            tools.extend([
                Tool(name="write_file", ...),
                Tool(name="delete_file", ...),
            ])
        
        return tools
    
    async def handle_tool_call(self, name: str, arguments: dict) -> TextContent:
        path = arguments.get("path")
        
        # Validate path is within allowed paths
        if not self._is_allowed(path):
            raise PermissionError(f"Access denied: {path}")
        
        if name == "read_file":
            content = Path(path).read_text()
            return TextContent(type="text", text=content)
        
        # ... other tools
```

### Tool Registry

```python
class MCPToolRegistry:
    """Central registry for all MCP tool servers."""
    
    def __init__(self):
        self.servers: dict[str, Server] = {}
        self.permissions: dict[str, ToolPermissions] = {}
    
    def register_builtin(self, name: str, config: dict) -> None:
        """Register a built-in tool server."""
        if name == "filesystem":
            self.servers[name] = FilesystemServer(
                allowed_paths=config.get("paths", ["."]),
                read_only=config.get("read_only", False)
            )
        elif name == "web_browser":
            self.servers[name] = WebBrowserServer()
        elif name == "shell":
            self.servers[name] = ShellServer(
                allowed_commands=config.get("allowed_commands")
            )
        # ... more built-ins
    
    def register_external(self, name: str, server_config: dict) -> None:
        """Register an external MCP server (stdio, sse, http)."""
        if server_config.get("type") == "stdio":
            self.servers[name] = StdioMCPClient(
                command=server_config["command"],
                args=server_config.get("args", [])
            )
        elif server_config.get("type") == "sse":
            self.servers[name] = SSEMCPClient(
                url=server_config["url"]
            )
        # ... more transport types
    
    def list_tools(self, agent_id: str) -> list[Tool]:
        """List all tools available to an agent."""
        available = []
        for name, server in self.servers.items():
            perms = self.permissions.get(f"{agent_id}:{name}")
            if perms and perms.enabled:
                for tool in server.tools:
                    if self._is_tool_allowed(tool.name, perms):
                        available.append(tool)
        return available
    
    async def call_tool(
        self, 
        agent_id: str,
        server_name: str, 
        tool_name: str, 
        arguments: dict
    ) -> Any:
        """Call a tool with permission checking."""
        perms = self.permissions.get(f"{agent_id}:{server_name}")
        
        if not perms or not perms.enabled:
            raise PermissionError(f"Server not enabled: {server_name}")
        
        if not self._is_tool_allowed(tool_name, perms):
            raise PermissionError(f"Tool not allowed: {tool_name}")
        
        server = self.servers[server_name]
        return await server.handle_tool_call(tool_name, arguments)
```

### Configuration

Tools are configured per-agent or globally:

```yaml
tools:
  builtin:
    filesystem:
      paths: 
        - ~/projects
        - /tmp/agent-workspace
      read_only: false
    
    web_browser:
      enabled: true
      allowed_domains:
        - "*.github.com"
        - "docs.python.org"
    
    shell:
      enabled: true
      allowed_commands:
        - git
        - npm
        - python
        - pip
      blocked_commands:
        - rm -rf
        - sudo
  
  external:
    slack:
      type: stdio
      command: npx
      args: ["@anthropic/mcp-server-slack"]
      env:
        SLACK_TOKEN: "${SLACK_TOKEN}"
    
    postgres:
      type: sse
      url: "http://localhost:3000/mcp"

agents:
  - name: atlas
    tools:
      - filesystem  # Enable with defaults
      - web_browser
      - shell
      - mcp:slack   # External MCP server
```

### Permission Layer

All tool calls go through a permission layer:

```python
@dataclass
class ToolPermissions:
    enabled: bool = True
    allowed_tools: list[str] | None = None  # None = all
    blocked_tools: list[str] = field(default_factory=list)
    require_confirmation: list[str] = field(default_factory=list)
    rate_limit: RateLimit | None = None

class PermissionEnforcer:
    """Enforces tool permissions and confirmations."""
    
    async def check_permission(
        self,
        agent_id: str,
        tool: str,
        arguments: dict,
        autonomy: str
    ) -> PermissionResult:
        perms = self.get_permissions(agent_id, tool)
        
        # Check if tool is blocked
        if tool in perms.blocked_tools:
            return PermissionResult.denied("Tool is blocked")
        
        # Check rate limits
        if perms.rate_limit and await self._is_rate_limited(agent_id, tool):
            return PermissionResult.denied("Rate limit exceeded")
        
        # Check if confirmation required
        if tool in perms.require_confirmation and autonomy != "high":
            return PermissionResult.needs_confirmation(
                f"Agent wants to use {tool}. Allow?"
            )
        
        return PermissionResult.allowed()
```

### Adapters for Non-MCP Backends

For backends that don't support MCP natively:

```python
class MCPToLangChainAdapter:
    """Adapts MCP tools to LangChain tool format."""
    
    def __init__(self, registry: MCPToolRegistry):
        self.registry = registry
    
    def to_langchain_tools(self, agent_id: str) -> list[LangChainTool]:
        lc_tools = []
        
        for mcp_tool in self.registry.list_tools(agent_id):
            lc_tool = LangChainTool(
                name=mcp_tool.name,
                description=mcp_tool.description,
                func=lambda args, t=mcp_tool: self._call_mcp(agent_id, t, args),
                args_schema=self._to_pydantic(mcp_tool.inputSchema)
            )
            lc_tools.append(lc_tool)
        
        return lc_tools
```

### Dynamic Tool Discovery

Agents can discover tools at runtime:

```python
# In agent prompt:
"""
Available tools:
{% for tool in tools %}
- {{ tool.name }}: {{ tool.description }}
{% endfor %}

To use a tool, respond with:
<tool_use name="tool_name">
{"arg": "value"}
</tool_use>
"""
```

## Consequences

### Positive
- Single standard for all tools (MCP)
- Rich ecosystem of existing MCP servers
- Clear permission model
- Works with any backend via adapters
- Future-proof as MCP adoption grows

### Negative
- MCP is still evolving; spec may change
- Adapter overhead for non-MCP backends
- Some agent frameworks have better native tool integration

### Implementation Notes

1. Start with filesystem, shell, and web browser built-ins
2. Use `mcp` Python package for MCP protocol handling
3. Implement stdio transport first (most common)
4. Add confirmation UI in TUI/Web for sensitive operations

## Related ADRs
- ADR-002: Agent Backend Abstraction
- ADR-003: Container Isolation (tool sandboxing)
