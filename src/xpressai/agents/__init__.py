"""Agent backend adapters for XpressAI.

Provides a common interface for different agent backends.
"""

from xpressai.agents.base import AgentBackend, AgentMessage, AgentStatus
from xpressai.agents.registry import (
    BackendRegistry,
    get_backend,
    register_backend,
    available_backends,
)

__all__ = [
    "AgentBackend",
    "AgentMessage",
    "AgentStatus",
    "BackendRegistry",
    "get_backend",
    "register_backend",
    "available_backends",
]
