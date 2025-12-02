"""Configuration management for XpressAI.

Configuration is loaded from YAML files with sensible defaults.
Environment variables override config file settings for secrets.
"""

from dataclasses import dataclass, field
from decimal import Decimal
from pathlib import Path
from typing import Any
import os

import yaml

from xpressai.core.exceptions import ConfigError, ConfigNotFoundError, ConfigValidationError


@dataclass
class BudgetConfig:
    """Budget and rate limiting configuration."""

    daily: Decimal | None = None
    monthly: Decimal | None = None
    per_task: Decimal | None = None
    on_exceeded: str = "pause"  # pause | alert | degrade | stop
    fallback_model: str = "local"
    warn_at_percent: int = 80

    def __post_init__(self) -> None:
        valid_actions = {"pause", "alert", "degrade", "stop"}
        if self.on_exceeded not in valid_actions:
            raise ConfigValidationError(
                f"Invalid on_exceeded action: {self.on_exceeded}",
                {"valid_actions": list(valid_actions)},
            )


@dataclass
class RateLimitConfig:
    """Rate limiting configuration."""

    requests_per_minute: int = 60
    tokens_per_minute: int = 100000
    concurrent_requests: int = 5


@dataclass
class ToolConfig:
    """Tool configuration."""

    enabled: bool = True
    config: dict[str, Any] = field(default_factory=dict)
    allowed_commands: list[str] = field(default_factory=list)
    paths: list[str] = field(default_factory=list)
    confirmation_required: bool = False


@dataclass
class McpServerConfig:
    """MCP server configuration.

    Supports stdio, SSE, and HTTP server types.
    """

    type: str = "stdio"  # stdio | sse | http
    command: str | None = None  # For stdio: command to run
    args: list[str] = field(default_factory=list)  # For stdio: command arguments
    env: dict[str, str] = field(default_factory=dict)  # Environment variables
    url: str | None = None  # For sse/http: server URL
    headers: dict[str, str] = field(default_factory=dict)  # For sse/http: headers


@dataclass
class WakeOnConfig:
    """Wake-on trigger configuration."""

    schedule: str | None = None  # Cron expression
    event: str | None = None  # Event name
    condition: str | None = None  # Condition expression


@dataclass
class AgentConfig:
    """Configuration for a single agent."""

    name: str
    backend: str = "local"
    role: str = ""
    autonomy: str = "medium"  # low | medium | high
    tools: list[str] = field(default_factory=list)
    budget: BudgetConfig | None = None
    rate_limit: RateLimitConfig | None = None
    wake_on: list[WakeOnConfig] = field(default_factory=list)
    container: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        valid_autonomy = {"low", "medium", "high"}
        if self.autonomy not in valid_autonomy:
            raise ConfigValidationError(
                f"Invalid autonomy level: {self.autonomy}", {"valid_levels": list(valid_autonomy)}
            )


@dataclass
class SystemConfig:
    """System-wide configuration."""

    isolation: str = "docker"  # docker | none
    budget: BudgetConfig = field(default_factory=BudgetConfig)
    rate_limit: RateLimitConfig = field(default_factory=RateLimitConfig)
    data_dir: Path = field(default_factory=lambda: Path.home() / ".xpressai")
    workspace_dir: Path = field(default_factory=lambda: Path.home() / "agent-workspace")

    def __post_init__(self) -> None:
        valid_isolation = {"docker", "none"}
        if self.isolation not in valid_isolation:
            raise ConfigValidationError(
                f"Invalid isolation mode: {self.isolation}", {"valid_modes": list(valid_isolation)}
            )


@dataclass
class MemoryConfig:
    """Memory system configuration."""

    near_term_slots: int = 8
    eviction: str = "least-recently-relevant"  # lru | least-recently-relevant
    retention: str = "none"  # none | delete_after | summarize
    embedding_model: str = "all-MiniLM-L6-v2"
    embedding_dim: int = 384

    def __post_init__(self) -> None:
        if self.near_term_slots < 1 or self.near_term_slots > 16:
            raise ConfigValidationError(
                f"near_term_slots must be between 1 and 16, got {self.near_term_slots}"
            )


@dataclass
class LocalModelConfig:
    """Local model configuration."""

    model: str = "Qwen/Qwen3-8B"
    inference_backend: str = "vllm"  # vllm | ollama | llama.cpp
    quantization: str = "q4_k_m"
    context_length: int = 32768
    thinking_mode: str = "auto"  # auto | always | never
    base_url: str = "http://localhost:8000"  # vLLM default port
    api_key: str = "EMPTY"  # vLLM doesn't require auth by default


@dataclass
class Config:
    """Root configuration for XpressAI."""

    system: SystemConfig = field(default_factory=SystemConfig)
    agents: list[AgentConfig] = field(default_factory=list)
    tools: dict[str, ToolConfig] = field(default_factory=dict)
    mcp_servers: dict[str, McpServerConfig] = field(default_factory=dict)
    memory: MemoryConfig = field(default_factory=MemoryConfig)
    local_model: LocalModelConfig = field(default_factory=LocalModelConfig)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Config":
        """Create Config from a dictionary."""
        # Parse system config
        system_data = data.get("system", {})
        budget_data = system_data.get("budget", {})

        # Parse budget - handle string dollar amounts
        if "daily" in budget_data and isinstance(budget_data["daily"], str):
            budget_data["daily"] = Decimal(budget_data["daily"].replace("$", ""))
        if "monthly" in budget_data and isinstance(budget_data["monthly"], str):
            budget_data["monthly"] = Decimal(budget_data["monthly"].replace("$", ""))
        if "per_task" in budget_data and isinstance(budget_data["per_task"], str):
            budget_data["per_task"] = Decimal(budget_data["per_task"].replace("$", ""))

        system = SystemConfig(
            isolation=system_data.get("isolation", "docker"),
            budget=BudgetConfig(**budget_data) if budget_data else BudgetConfig(),
            data_dir=Path(system_data.get("data_dir", Path.home() / ".xpressai")),
            workspace_dir=Path(system_data.get("workspace_dir", Path.home() / "agent-workspace")),
        )

        # Parse agents
        agents = []
        for agent_data in data.get("agents", []):
            agent_budget = None
            if "budget" in agent_data:
                ab = agent_data["budget"].copy()
                if "daily" in ab and isinstance(ab["daily"], str):
                    ab["daily"] = Decimal(ab["daily"].replace("$", ""))
                agent_budget = BudgetConfig(**ab)

            wake_on = []
            for wo in agent_data.get("wake_on", []):
                wake_on.append(
                    WakeOnConfig(
                        schedule=wo.get("schedule"),
                        event=wo.get("event"),
                        condition=wo.get("condition"),
                    )
                )

            agents.append(
                AgentConfig(
                    name=agent_data.get("name", "default"),
                    backend=agent_data.get("backend", "local"),
                    role=agent_data.get("role", ""),
                    autonomy=agent_data.get("autonomy", "medium"),
                    tools=agent_data.get("tools", []),
                    budget=agent_budget,
                    wake_on=wake_on,
                    container=agent_data.get("container", {}),
                )
            )

        # Parse tools
        tools = {}
        tools_data = data.get("tools", {})
        builtin = tools_data.get("builtin", {})
        for name, tool_data in builtin.items():
            if isinstance(tool_data, bool):
                tools[name] = ToolConfig(enabled=tool_data)
            elif isinstance(tool_data, dict):
                tools[name] = ToolConfig(
                    enabled=tool_data.get("enabled", True),
                    config=tool_data,
                    allowed_commands=tool_data.get("allowed_commands", []),
                    paths=tool_data.get("paths", []),
                    confirmation_required=tool_data.get("confirmation_required", False),
                )
            elif isinstance(tool_data, str):
                # Path shorthand
                tools[name] = ToolConfig(enabled=True, paths=[tool_data])

        # Parse MCP servers
        mcp_servers = {}
        mcp_data = data.get("mcp_servers", {})
        for name, server_data in mcp_data.items():
            if isinstance(server_data, dict):
                mcp_servers[name] = McpServerConfig(
                    type=server_data.get("type", "stdio"),
                    command=server_data.get("command"),
                    args=server_data.get("args", []),
                    env=server_data.get("env", {}),
                    url=server_data.get("url"),
                    headers=server_data.get("headers", {}),
                )

        # Parse memory
        memory_data = data.get("memory", {})
        memory = MemoryConfig(
            near_term_slots=memory_data.get("near_term_slots", 8),
            eviction=memory_data.get("eviction", "least-recently-relevant"),
            retention=memory_data.get("retention", "none"),
            embedding_model=memory_data.get("embedding_model", "all-MiniLM-L6-v2"),
        )

        # Parse local model config
        local_data = data.get("local_model", {})
        local_model = LocalModelConfig(
            model=local_data.get("model", "Qwen/Qwen3-8B"),
            inference_backend=local_data.get("inference_backend", "vllm"),
            quantization=local_data.get("quantization", "q4_k_m"),
            context_length=local_data.get("context_length", 32768),
            thinking_mode=local_data.get("thinking_mode", "auto"),
            base_url=local_data.get("base_url", "http://localhost:8000"),
            api_key=local_data.get("api_key", "EMPTY"),
        )

        return cls(
            system=system,
            agents=agents,
            tools=tools,
            mcp_servers=mcp_servers,
            memory=memory,
            local_model=local_model,
        )

    def to_dict(self) -> dict[str, Any]:
        """Convert config to dictionary for serialization."""
        return {
            "system": {
                "isolation": self.system.isolation,
                "budget": {
                    "daily": f"${self.system.budget.daily}" if self.system.budget.daily else None,
                    "monthly": f"${self.system.budget.monthly}"
                    if self.system.budget.monthly
                    else None,
                    "on_exceeded": self.system.budget.on_exceeded,
                },
            },
            "agents": [
                {
                    "name": agent.name,
                    "backend": agent.backend,
                    "role": agent.role,
                    "autonomy": agent.autonomy,
                    "tools": agent.tools,
                }
                for agent in self.agents
            ],
            "memory": {
                "near_term_slots": self.memory.near_term_slots,
                "eviction": self.memory.eviction,
            },
            "local_model": {
                "model": self.local_model.model,
                "inference_backend": self.local_model.inference_backend,
            },
        }


def load_config(path: Path | None = None) -> Config:
    """Load configuration from file.

    Args:
        path: Path to config file. Defaults to ./xpressai.yaml

    Returns:
        Loaded configuration

    Raises:
        ConfigNotFoundError: If config file doesn't exist
        ConfigError: If config file is invalid
    """
    if path is None:
        path = Path.cwd() / "xpressai.yaml"

    if not path.exists():
        # Return defaults with a single default agent
        return Config(agents=[AgentConfig(name="default")])

    try:
        with open(path) as f:
            data = yaml.safe_load(f) or {}
    except yaml.YAMLError as e:
        raise ConfigError(f"Invalid YAML in config file: {e}")

    try:
        return Config.from_dict(data)
    except Exception as e:
        raise ConfigError(f"Failed to parse config: {e}")


def save_config(config: Config, path: Path | None = None) -> None:
    """Save configuration to file.

    Args:
        config: Configuration to save
        path: Path to save to. Defaults to ./xpressai.yaml
    """
    if path is None:
        path = Path.cwd() / "xpressai.yaml"

    data = config.to_dict()

    # Clean up None values in budget
    if data.get("system", {}).get("budget"):
        data["system"]["budget"] = {
            k: v for k, v in data["system"]["budget"].items() if v is not None
        }

    with open(path, "w") as f:
        yaml.dump(data, f, default_flow_style=False, sort_keys=False)


def get_env_config() -> dict[str, str]:
    """Get configuration from environment variables.

    Environment variables override config file values for secrets.
    """
    return {
        "anthropic_api_key": os.environ.get("ANTHROPIC_API_KEY", ""),
        "openai_api_key": os.environ.get("OPENAI_API_KEY", ""),
        "vllm_base_url": os.environ.get("VLLM_BASE_URL", "http://localhost:8000"),
        "vllm_api_key": os.environ.get("VLLM_API_KEY", "EMPTY"),
    }


DEFAULT_CONFIG_TEMPLATE = """\
# XpressAI Configuration
# Generated by `xpressai init`
# Docs: https://docs.xpress.ai

# System-wide settings
system:
  # Container isolation for agents (docker | none)
  isolation: docker
  
  # Budget controls
  budget:
    daily: $20.00
    # monthly: $100.00
    on_exceeded: pause  # pause | alert | degrade | stop

# Agent definitions
agents:
  - name: atlas
    backend: {backend}
    role: |
      You are a helpful AI assistant.
    # autonomy: medium  # low | medium | high

# Tool configuration
tools:
  builtin:
    filesystem:
      paths:
        - ~/agent-workspace
    # web_browser: true
    shell:
      enabled: true
      allowed_commands:
        - git
        - npm
        - python
        - pip

# Memory settings
memory:
  near_term_slots: 8
  eviction: least-recently-relevant

# MCP (Model Context Protocol) servers
# These provide additional tools to your agents
# mcp_servers:
#   github:
#     type: stdio
#     command: npx
#     args: ["-y", "@modelcontextprotocol/server-github"]
#     env:
#       GITHUB_PERSONAL_ACCESS_TOKEN: ${{GITHUB_TOKEN}}
#   
#   filesystem:
#     type: stdio
#     command: npx
#     args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]
#   
#   # SSE server example
#   custom_api:
#     type: sse
#     url: http://localhost:3000/mcp
#     headers:
#       Authorization: Bearer ${{API_TOKEN}}

# Local model settings (when using backend: local)
# Start vLLM with: vllm serve Qwen/Qwen3-8B
local_model:
  model: Qwen/Qwen3-8B
  inference_backend: vllm  # vllm | ollama | llama.cpp
  base_url: http://localhost:8000
  # api_key: EMPTY  # vLLM doesn't require auth by default
  # context_length: 32768
"""
