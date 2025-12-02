"""Docker container management for agent isolation.

Manages Docker containers for running agents in isolated environments.
"""

from dataclasses import dataclass, field
from typing import Any
import logging

from xpressai.core.exceptions import ContainerError, ContainerStartError, ContainerNotFoundError

logger = logging.getLogger(__name__)

# Try to import docker
try:
    import docker
    from docker.errors import DockerException, NotFound, APIError

    DOCKER_AVAILABLE = True
except ImportError:
    DOCKER_AVAILABLE = False
    logger.warning("docker package not available, container isolation disabled")


@dataclass
class ContainerSpec:
    """Specification for an agent container.

    Attributes:
        image: Docker image to use
        memory_limit: Memory limit (e.g., "2g")
        cpu_limit: CPU limit (0-1 = fraction of one CPU)
        gpu: Whether to enable GPU
        mounts: Volume mounts
        environment: Environment variables
        network_mode: Network mode
        security_opts: Security options
    """

    image: str = "python:3.11-slim"
    memory_limit: str = "2g"
    cpu_limit: float = 1.0
    gpu: bool = False
    mounts: list[dict[str, Any]] = field(default_factory=list)
    environment: dict[str, str] = field(default_factory=dict)
    network_mode: str = "bridge"
    security_opts: list[str] = field(default_factory=list)


class DockerManager:
    """Manages Docker containers for agent isolation.

    Provides methods to launch, stop, and manage containers for agents.
    """

    def __init__(self):
        """Initialize Docker manager."""
        self._client = None
        self._containers: dict[str, Any] = {}

    def _get_client(self):
        """Get or create Docker client."""
        if not DOCKER_AVAILABLE:
            raise ContainerError("docker package not installed")

        if self._client is None:
            try:
                self._client = docker.from_env()
                # Verify connection
                self._client.ping()
            except Exception as e:
                raise ContainerError(f"Failed to connect to Docker: {e}")

        return self._client

    @property
    def available(self) -> bool:
        """Check if Docker is available."""
        if not DOCKER_AVAILABLE:
            return False

        try:
            self._get_client()
            return True
        except ContainerError:
            return False

    async def launch_agent(
        self,
        agent_id: str,
        backend: str,
        spec: ContainerSpec | None = None,
    ) -> str:
        """Launch an agent in a new container.

        Args:
            agent_id: Unique agent identifier
            backend: Backend type for the agent
            spec: Container specification

        Returns:
            Container ID

        Raises:
            ContainerStartError: If container fails to start
        """
        client = self._get_client()

        if spec is None:
            spec = ContainerSpec()

        # Prepare environment
        env = {
            "XPRESSAI_AGENT_ID": agent_id,
            "XPRESSAI_BACKEND": backend,
            **spec.environment,
        }

        # Prepare container kwargs
        container_kwargs = {
            "detach": True,
            "name": f"xpressai-{agent_id}",
            "mem_limit": spec.memory_limit,
            "nano_cpus": int(spec.cpu_limit * 1e9),
            "environment": env,
            "network_mode": spec.network_mode,
            "remove": False,  # Don't auto-remove for logs
        }

        # Add security options if specified
        if spec.security_opts:
            container_kwargs["security_opt"] = spec.security_opts

        # Add GPU support if requested
        if spec.gpu:
            container_kwargs["device_requests"] = [
                docker.types.DeviceRequest(count=-1, capabilities=[["gpu"]])
            ]

        # Add mounts
        if spec.mounts:
            container_kwargs["mounts"] = [
                docker.types.Mount(
                    target=m.get("target"),
                    source=m.get("source"),
                    type=m.get("type", "bind"),
                    read_only=m.get("read_only", False),
                )
                for m in spec.mounts
            ]

        try:
            container = client.containers.run(
                spec.image,
                **container_kwargs,
            )

            self._containers[agent_id] = container
            logger.info(f"Launched container for agent {agent_id}: {container.id[:12]}")

            return container.id

        except Exception as e:
            raise ContainerStartError(
                f"Failed to launch container for {agent_id}: {e}",
                {"agent_id": agent_id, "image": spec.image},
            )

    async def stop_agent(self, agent_id: str, timeout: int = 10) -> None:
        """Stop an agent's container.

        Args:
            agent_id: Agent identifier
            timeout: Shutdown timeout in seconds
        """
        container = self._containers.get(agent_id)

        if container is None:
            return

        try:
            container.stop(timeout=timeout)
            logger.info(f"Stopped container for agent {agent_id}")
        except Exception as e:
            logger.warning(f"Error stopping container for {agent_id}: {e}")

        try:
            container.remove(force=True)
        except Exception:
            pass

        del self._containers[agent_id]

    async def stop_all(self, timeout: int = 10) -> None:
        """Stop all agent containers.

        Args:
            timeout: Shutdown timeout per container
        """
        for agent_id in list(self._containers.keys()):
            await self.stop_agent(agent_id, timeout)

    async def exec_in_agent(
        self,
        agent_id: str,
        command: list[str],
    ) -> tuple[int, str]:
        """Execute a command in an agent's container.

        Args:
            agent_id: Agent identifier
            command: Command to execute

        Returns:
            Tuple of (exit_code, output)

        Raises:
            ContainerNotFoundError: If container doesn't exist
        """
        container = self._containers.get(agent_id)

        if container is None:
            raise ContainerNotFoundError(
                f"Container not found for agent: {agent_id}", {"agent_id": agent_id}
            )

        result = container.exec_run(command)
        return result.exit_code, result.output.decode()

    async def get_logs(
        self,
        agent_id: str,
        tail: int = 100,
        follow: bool = False,
    ) -> str:
        """Get logs from an agent's container.

        Args:
            agent_id: Agent identifier
            tail: Number of lines to return
            follow: Whether to follow logs

        Returns:
            Log output
        """
        container = self._containers.get(agent_id)

        if container is None:
            return ""

        if follow:
            # Return generator for following
            return container.logs(tail=tail, stream=True, follow=True)

        return container.logs(tail=tail).decode()

    def is_running(self, agent_id: str) -> bool:
        """Check if an agent's container is running.

        Args:
            agent_id: Agent identifier

        Returns:
            True if container is running
        """
        container = self._containers.get(agent_id)

        if container is None:
            return False

        try:
            container.reload()
            return container.status == "running"
        except Exception:
            return False

    def get_container_stats(self, agent_id: str) -> dict[str, Any] | None:
        """Get container resource stats.

        Args:
            agent_id: Agent identifier

        Returns:
            Stats dict or None
        """
        container = self._containers.get(agent_id)

        if container is None:
            return None

        try:
            stats = container.stats(stream=False)
            return {
                "cpu_percent": self._calculate_cpu_percent(stats),
                "memory_usage": stats.get("memory_stats", {}).get("usage", 0),
                "memory_limit": stats.get("memory_stats", {}).get("limit", 0),
            }
        except Exception:
            return None

    def _calculate_cpu_percent(self, stats: dict) -> float:
        """Calculate CPU percentage from stats."""
        cpu_stats = stats.get("cpu_stats", {})
        precpu_stats = stats.get("precpu_stats", {})

        cpu_delta = cpu_stats.get("cpu_usage", {}).get("total_usage", 0) - precpu_stats.get(
            "cpu_usage", {}
        ).get("total_usage", 0)
        system_delta = cpu_stats.get("system_cpu_usage", 0) - precpu_stats.get(
            "system_cpu_usage", 0
        )

        if system_delta > 0 and cpu_delta > 0:
            cpu_count = len(cpu_stats.get("cpu_usage", {}).get("percpu_usage", [1]))
            return (cpu_delta / system_delta) * cpu_count * 100.0

        return 0.0

    async def pull_image(self, image: str) -> None:
        """Pull a Docker image.

        Args:
            image: Image name to pull
        """
        client = self._get_client()

        logger.info(f"Pulling image: {image}")
        client.images.pull(image)
        logger.info(f"Pulled image: {image}")
