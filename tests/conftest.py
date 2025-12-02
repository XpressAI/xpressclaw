"""Pytest configuration and fixtures for XpressAI tests."""

import asyncio
import tempfile
from pathlib import Path
from typing import AsyncGenerator, Generator

import pytest


@pytest.fixture
def temp_dir() -> Generator[Path, None, None]:
    """Create a temporary directory for tests."""
    with tempfile.TemporaryDirectory() as tmpdir:
        yield Path(tmpdir)


@pytest.fixture
def sample_config() -> dict:
    """Sample configuration for testing."""
    return {
        "system": {
            "isolation": "none",
            "budget": {
                "daily": 20.0,
                "on_exceeded": "pause",
            },
        },
        "agents": [
            {
                "name": "test-agent",
                "backend": "local",
                "role": "You are a test agent.",
            }
        ],
        "tools": {
            "builtin": {
                "filesystem": {"paths": ["/tmp/test-workspace"]},
                "shell": {"enabled": True, "allowed_commands": ["echo", "ls"]},
            }
        },
        "memory": {
            "near_term_slots": 8,
            "eviction": "least-recently-relevant",
        },
    }


@pytest.fixture
def config_file(temp_dir: Path, sample_config: dict) -> Path:
    """Create a temporary config file."""
    import yaml

    config_path = temp_dir / "xpressai.yaml"
    with open(config_path, "w") as f:
        yaml.dump(sample_config, f)
    return config_path
