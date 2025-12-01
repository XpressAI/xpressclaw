"""Configuration management for XpressAI."""

from dataclasses import dataclass, field
from decimal import Decimal
from pathlib import Path
from typing import Any
import yaml


@dataclass
class BudgetConfig:
    """Budget and rate limiting configuration."""
    daily: Decimal | None = None
    monthly: Decimal | None = None
    per_task: Decimal | None = None
    on_exceeded: str = "pause"  # pause | alert | degrade | stop
    fallback_model: str = "local"
    warn_at_percent: int = 80


@dataclass
class ToolConfig:
    """Tool configuration."""
    enabled: bool = True
    config: dict[str, Any] = field(default_factory=dict)


@dataclass
class AgentConfig:
    """Configuration for a single agent."""
    name: str
    backend: str = "local"
    role: str = ""
    autonomy: str = "medium"  # low | medium | high
    tools: list[str] = field(default_factory=list)
    budget: BudgetConfig | None = None
    wake_on: list[dict[str, str]] = field(default_factory=list)


@dataclass
class SystemConfig:
    """System-wide configuration."""
    isolation: str = "docker"  # docker | none
    budget: BudgetConfig = field(default_factory=BudgetConfig)


@dataclass
class MemoryConfig:
    """Memory system configuration."""
    near_term_slots: int = 8
    eviction: str = "least-recently-relevant"
    retention: str = "none"  # none | delete_after | summarize


@dataclass
class Config:
    """Root configuration for XpressAI."""
    system: SystemConfig = field(default_factory=SystemConfig)
    agents: list[AgentConfig] = field(default_factory=list)
    tools: dict[str, ToolConfig] = field(default_factory=dict)
    memory: MemoryConfig = field(default_factory=MemoryConfig)
    
    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Config":
        """Create Config from a dictionary."""
        system_data = data.get("system", {})
        budget_data = system_data.get("budget", {})
        
        # Parse budget
        if "daily" in budget_data and isinstance(budget_data["daily"], str):
            budget_data["daily"] = Decimal(budget_data["daily"].replace("$", ""))
        if "monthly" in budget_data and isinstance(budget_data["monthly"], str):
            budget_data["monthly"] = Decimal(budget_data["monthly"].replace("$", ""))
        
        system = SystemConfig(
            isolation=system_data.get("isolation", "docker"),
            budget=BudgetConfig(**budget_data) if budget_data else BudgetConfig(),
        )
        
        # Parse agents
        agents = []
        for agent_data in data.get("agents", []):
            agent_budget = None
            if "budget" in agent_data:
                ab = agent_data["budget"]
                if "daily" in ab and isinstance(ab["daily"], str):
                    ab["daily"] = Decimal(ab["daily"].replace("$", ""))
                agent_budget = BudgetConfig(**ab)
            
            agents.append(AgentConfig(
                name=agent_data.get("name", "default"),
                backend=agent_data.get("backend", "local"),
                role=agent_data.get("role", ""),
                autonomy=agent_data.get("autonomy", "medium"),
                tools=agent_data.get("tools", []),
                budget=agent_budget,
                wake_on=agent_data.get("wake_on", []),
            ))
        
        # Parse tools
        tools = {}
        tools_data = data.get("tools", {})
        for name, tool_data in tools_data.get("builtin", {}).items():
            if isinstance(tool_data, bool):
                tools[name] = ToolConfig(enabled=tool_data)
            elif isinstance(tool_data, dict):
                tools[name] = ToolConfig(
                    enabled=tool_data.get("enabled", True),
                    config=tool_data,
                )
        
        # Parse memory
        memory_data = data.get("memory", {})
        memory = MemoryConfig(
            near_term_slots=memory_data.get("near_term_slots", 8),
            eviction=memory_data.get("eviction", "least-recently-relevant"),
            retention=memory_data.get("retention", "none"),
        )
        
        return cls(system=system, agents=agents, tools=tools, memory=memory)


def load_config(path: Path | None = None) -> Config:
    """Load configuration from file."""
    if path is None:
        path = Path.cwd() / "xpressai.yaml"
    
    if not path.exists():
        # Return defaults
        return Config(agents=[AgentConfig(name="default")])
    
    with open(path) as f:
        data = yaml.safe_load(f) or {}
    
    return Config.from_dict(data)


def save_config(config: Config, path: Path | None = None) -> None:
    """Save configuration to file."""
    if path is None:
        path = Path.cwd() / "xpressai.yaml"
    
    # Convert to dict for YAML serialization
    data = {
        "system": {
            "isolation": config.system.isolation,
            "budget": {
                "daily": f"${config.system.budget.daily}" if config.system.budget.daily else None,
                "monthly": f"${config.system.budget.monthly}" if config.system.budget.monthly else None,
                "on_exceeded": config.system.budget.on_exceeded,
            },
        },
        "agents": [
            {
                "name": agent.name,
                "backend": agent.backend,
                "role": agent.role,
            }
            for agent in config.agents
        ],
        "memory": {
            "near_term_slots": config.memory.near_term_slots,
            "eviction": config.memory.eviction,
        },
    }
    
    # Clean up None values
    data["system"]["budget"] = {k: v for k, v in data["system"]["budget"].items() if v is not None}
    
    with open(path, "w") as f:
        yaml.dump(data, f, default_flow_style=False, sort_keys=False)


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
"""
