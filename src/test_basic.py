"""Basic tests for XpressAI."""

import pytest
from pathlib import Path


def test_version():
    """Test version is accessible."""
    from xpressai import __version__
    assert __version__ == "0.1.0"


def test_config_defaults():
    """Test default configuration."""
    from xpressai.core.config import Config, BudgetConfig
    
    config = Config()
    assert config.system.isolation == "docker"
    assert config.memory.near_term_slots == 8


def test_config_from_dict():
    """Test loading config from dict."""
    from xpressai.core.config import Config
    
    data = {
        "system": {
            "isolation": "none",
            "budget": {
                "daily": "$10.00",
                "on_exceeded": "alert",
            }
        },
        "agents": [
            {
                "name": "test-agent",
                "backend": "local",
            }
        ]
    }
    
    config = Config.from_dict(data)
    assert config.system.isolation == "none"
    assert len(config.agents) == 1
    assert config.agents[0].name == "test-agent"


@pytest.mark.asyncio
async def test_runtime_initialization():
    """Test runtime can be initialized."""
    from xpressai.core.runtime import Runtime
    from xpressai.core.config import Config, AgentConfig
    
    config = Config(agents=[AgentConfig(name="test")])
    runtime = Runtime(config)
    
    # Note: Full initialization requires database, so we just test creation
    assert runtime.config == config


def test_task_status_enum():
    """Test task status enum."""
    from xpressai.tasks.board import TaskStatus
    
    assert TaskStatus.PENDING.value == "pending"
    assert TaskStatus.COMPLETED.value == "completed"


def test_cost_calculator():
    """Test cost calculation."""
    from xpressai.budget.manager import CostCalculator
    from decimal import Decimal
    
    calc = CostCalculator()
    
    # Local model should be free
    cost = calc.calculate("local", 1000, 1000)
    assert cost == Decimal("0")
    
    # Claude should have cost
    cost = calc.calculate("claude-sonnet", 1000000, 1000000)
    assert cost > Decimal("0")
