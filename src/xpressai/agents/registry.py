"""Agent backend registry and management.

Handles discovery, registration, and instantiation of agent backends.
"""

from typing import Any
import logging

from xpressai.agents.base import AgentBackend, AgentStatus
from xpressai.core.config import AgentConfig
from xpressai.core.exceptions import BackendNotFoundError, BackendInitializationError

logger = logging.getLogger(__name__)


class BackendRegistry:
    """Registry for agent backends.

    Manages available backend types and their instances.
    """

    def __init__(self):
        """Initialize the registry."""
        self._backends: dict[str, type[AgentBackend]] = {}
        self._instances: dict[str, AgentBackend] = {}

        # Register built-in backends
        self._register_builtin()

    def _register_builtin(self) -> None:
        """Register built-in backends."""
        from xpressai.agents.local import LocalModelBackend
        from xpressai.agents.claude import ClaudeAgentBackend

        self._backends["local"] = LocalModelBackend
        self._backends["claude-code"] = ClaudeAgentBackend
        self._backends["claude"] = ClaudeAgentBackend  # Alias

    def register(self, name: str, backend_class: type[AgentBackend]) -> None:
        """Register a new backend type.

        Args:
            name: Name for the backend
            backend_class: Backend class
        """
        self._backends[name] = backend_class
        logger.info(f"Registered backend: {name}")

    def available(self) -> list[str]:
        """List available backend types.

        Returns:
            List of backend names
        """
        return list(self._backends.keys())

    async def get(
        self,
        backend_type: str,
        agent_id: str,
        config: AgentConfig | None = None,
    ) -> AgentBackend:
        """Get or create a backend instance for an agent.

        Args:
            backend_type: Type of backend
            agent_id: ID of the agent
            config: Optional agent configuration

        Returns:
            Backend instance

        Raises:
            BackendNotFoundError: If backend type is unknown
            BackendInitializationError: If backend fails to initialize
        """
        key = f"{agent_id}:{backend_type}"

        if key in self._instances:
            return self._instances[key]

        if backend_type not in self._backends:
            raise BackendNotFoundError(
                f"Unknown backend: {backend_type}",
                {"backend_type": backend_type, "available": self.available()},
            )

        try:
            backend = self._backends[backend_type]()

            if config:
                await backend.initialize(config)

            self._instances[key] = backend
            logger.info(f"Created backend instance: {key}")

            return backend

        except Exception as e:
            raise BackendInitializationError(
                f"Failed to initialize backend: {e}",
                {"backend_type": backend_type, "agent_id": agent_id},
            )

    async def shutdown(self, agent_id: str) -> None:
        """Shutdown all backends for an agent.

        Args:
            agent_id: ID of the agent
        """
        keys_to_remove = [k for k in self._instances if k.startswith(f"{agent_id}:")]

        for key in keys_to_remove:
            try:
                await self._instances[key].shutdown()
            except Exception as e:
                logger.warning(f"Error shutting down {key}: {e}")
            finally:
                del self._instances[key]

    async def shutdown_all(self) -> None:
        """Shutdown all backend instances."""
        for key, backend in list(self._instances.items()):
            try:
                await backend.shutdown()
            except Exception as e:
                logger.warning(f"Error shutting down {key}: {e}")

        self._instances.clear()


# Global registry instance
_registry: BackendRegistry | None = None


def get_registry() -> BackendRegistry:
    """Get the global backend registry."""
    global _registry
    if _registry is None:
        _registry = BackendRegistry()
    return _registry


def register_backend(name: str, backend_class: type[AgentBackend]) -> None:
    """Register a new backend type.

    Args:
        name: Name for the backend
        backend_class: Backend class
    """
    get_registry().register(name, backend_class)


async def get_backend(
    backend_type: str,
    agent_id: str,
    config: AgentConfig | None = None,
) -> AgentBackend:
    """Get or create a backend instance.

    Args:
        backend_type: Type of backend
        agent_id: ID of the agent
        config: Optional agent configuration

    Returns:
        Backend instance
    """
    return await get_registry().get(backend_type, agent_id, config)


def available_backends() -> list[str]:
    """List available backend types.

    Returns:
        List of backend names
    """
    return get_registry().available()
