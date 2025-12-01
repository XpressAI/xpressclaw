"""Docker container management for agent isolation."""

from dataclasses import dataclass, field
from typing import Any


@dataclass
class ContainerSpec:
    """Specification for an agent container."""
    image: str = "python:3.11-slim"
    memory_limit: str = "2g"
    cpu_limit: float = 1.0
    gpu: bool = False
    mounts: list[dict[str, Any]] = field(default_factory=list)
    environment: dict[str, str] = field(default_factory=dict)


class DockerManager:
    """Manages Docker containers for agent isolation."""
    
    def __init__(self):
        self._client = None
        self._containers: dict[str, Any] = {}
    
    def _get_client(self):
        """Get or create Docker client."""
        if self._client is None:
            try:
                import docker
                self._client = docker.from_env()
            except ImportError:
                raise RuntimeError("docker package not installed")
            except Exception as e:
                raise RuntimeError(f"Failed to connect to Docker: {e}")
        return self._client
    
    async def launch_agent(
        self,
        agent_id: str,
        backend: str,
        spec: ContainerSpec | None = None,
    ) -> str:
        """Launch an agent in a new container."""
        client = self._get_client()
        
        if spec is None:
            spec = ContainerSpec()
        
        # Prepare environment
        env = {
            "XPRESSAI_AGENT_ID": agent_id,
            "XPRESSAI_BACKEND": backend,
            **spec.environment,
        }
        
        try:
            container = client.containers.run(
                spec.image,
                detach=True,
                name=f"xpressai-{agent_id}",
                mem_limit=spec.memory_limit,
                nano_cpus=int(spec.cpu_limit * 1e9),
                environment=env,
                remove=True,
            )
            
            self._containers[agent_id] = container
            return container.id
            
        except Exception as e:
            raise RuntimeError(f"Failed to launch container: {e}")
    
    async def stop_agent(self, agent_id: str, timeout: int = 10) -> None:
        """Stop an agent's container."""
        container = self._containers.get(agent_id)
        
        if container:
            try:
                container.stop(timeout=timeout)
            except Exception:
                pass
            
            try:
                container.remove(force=True)
            except Exception:
                pass
            
            del self._containers[agent_id]
    
    async def exec_in_agent(
        self,
        agent_id: str,
        command: list[str],
    ) -> tuple[int, str]:
        """Execute a command in an agent's container."""
        container = self._containers.get(agent_id)
        
        if container is None:
            raise RuntimeError(f"Container not found for agent: {agent_id}")
        
        result = container.exec_run(command)
        return result.exit_code, result.output.decode()
    
    async def get_logs(self, agent_id: str, tail: int = 100) -> str:
        """Get logs from an agent's container."""
        container = self._containers.get(agent_id)
        
        if container is None:
            return ""
        
        return container.logs(tail=tail).decode()
    
    def is_running(self, agent_id: str) -> bool:
        """Check if an agent's container is running."""
        container = self._containers.get(agent_id)
        
        if container is None:
            return False
        
        try:
            container.reload()
            return container.status == "running"
        except Exception:
            return False
