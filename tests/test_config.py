"""Tests for the configuration module."""

import pytest
from pathlib import Path
from decimal import Decimal

from xpressai.core.config import (
    Config,
    SystemConfig,
    BudgetConfig,
    AgentConfig,
    MemoryConfig,
    McpServerConfig,
    load_config,
)


class TestBudgetConfig:
    """Tests for BudgetConfig."""

    def test_default_on_exceeded(self):
        """Test default on_exceeded is pause."""
        config = BudgetConfig()
        assert config.on_exceeded == "pause"

    def test_invalid_on_exceeded_raises(self):
        """Test that invalid on_exceeded raises error."""
        from xpressai.core.exceptions import ConfigValidationError

        with pytest.raises(ConfigValidationError):
            BudgetConfig(on_exceeded="invalid")


class TestSystemConfig:
    """Tests for SystemConfig."""

    def test_default_isolation(self):
        """Test default isolation is docker."""
        config = SystemConfig()
        assert config.isolation == "docker"

    def test_budget_defaults(self):
        """Test budget has sensible defaults."""
        config = SystemConfig()
        assert config.budget.on_exceeded == "pause"


class TestAgentConfig:
    """Tests for AgentConfig."""

    def test_required_fields(self):
        """Test name is required."""
        with pytest.raises(TypeError):
            AgentConfig()  # Missing name

    def test_default_backend(self):
        """Test default backend."""
        config = AgentConfig(name="test")
        assert config.backend == "local"


class TestConfig:
    """Tests for the main Config."""

    def test_from_dict(self, sample_config: dict):
        """Test creating config from dictionary."""
        config = Config.from_dict(sample_config)

        assert config.system.isolation == "none"
        assert len(config.agents) == 1
        assert config.agents[0].name == "test-agent"

    def test_to_dict(self, sample_config: dict):
        """Test converting config to dictionary."""
        config = Config.from_dict(sample_config)
        result = config.to_dict()

        assert result["system"]["isolation"] == "none"
        assert result["agents"][0]["name"] == "test-agent"

    def test_parse_dollar_amounts(self):
        """Test parsing dollar amounts in config."""
        data = {
            "system": {
                "budget": {
                    "daily": "$20.00",
                }
            },
            "agents": [],
        }
        config = Config.from_dict(data)
        assert config.system.budget.daily == Decimal("20.00")


class TestLoadConfig:
    """Tests for load_config function."""

    def test_load_from_file(self, config_file: Path):
        """Test loading config from file."""
        config = load_config(config_file)

        assert config is not None
        assert config.system.isolation == "none"

    def test_load_nonexistent_returns_default(self, temp_dir: Path):
        """Test loading nonexistent file returns default config."""
        import os

        old_cwd = os.getcwd()
        os.chdir(temp_dir)
        try:
            config = load_config(temp_dir / "nonexistent.yaml")
            assert config is not None
            # Should have default agent
            assert len(config.agents) == 1
            assert config.agents[0].name == "default"
        finally:
            os.chdir(old_cwd)


class TestMemoryConfig:
    """Tests for MemoryConfig."""

    def test_default_slots(self):
        """Test default near_term_slots."""
        config = MemoryConfig()
        assert config.near_term_slots == 8

    def test_slots_validation(self):
        """Test slots must be in valid range."""
        from xpressai.core.exceptions import ConfigValidationError

        with pytest.raises(ConfigValidationError):
            MemoryConfig(near_term_slots=0)
        with pytest.raises(ConfigValidationError):
            MemoryConfig(near_term_slots=20)


class TestMcpServerConfig:
    """Tests for McpServerConfig."""

    def test_default_type(self):
        """Test default type is stdio."""
        config = McpServerConfig()
        assert config.type == "stdio"

    def test_stdio_server(self):
        """Test stdio server configuration."""
        config = McpServerConfig(
            type="stdio",
            command="npx",
            args=["-y", "@modelcontextprotocol/server-github"],
            env={"GITHUB_TOKEN": "test"},
        )
        assert config.type == "stdio"
        assert config.command == "npx"
        assert len(config.args) == 2
        assert config.env["GITHUB_TOKEN"] == "test"

    def test_sse_server(self):
        """Test SSE server configuration."""
        config = McpServerConfig(
            type="sse",
            url="http://localhost:3000/mcp",
            headers={"Authorization": "Bearer token"},
        )
        assert config.type == "sse"
        assert config.url == "http://localhost:3000/mcp"
        assert "Authorization" in config.headers

    def test_mcp_servers_in_config(self):
        """Test parsing mcp_servers from config dict."""
        data = {
            "system": {"isolation": "none"},
            "agents": [],
            "mcp_servers": {
                "github": {
                    "type": "stdio",
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "env": {"GITHUB_TOKEN": "test"},
                },
                "custom_api": {
                    "type": "sse",
                    "url": "http://localhost:3000/mcp",
                },
            },
        }
        config = Config.from_dict(data)

        assert len(config.mcp_servers) == 2
        assert "github" in config.mcp_servers
        assert "custom_api" in config.mcp_servers
        assert config.mcp_servers["github"].type == "stdio"
        assert config.mcp_servers["github"].command == "npx"
        assert config.mcp_servers["custom_api"].type == "sse"
