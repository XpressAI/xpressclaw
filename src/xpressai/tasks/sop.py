"""Standard Operating Procedures (SOPs) for XpressAI.

SOPs are YAML files that guide agents through workflows using prompts and tools.
They live in the .xpressai/sops/ directory.
"""

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
import yaml
import logging

from xpressai.core.exceptions import SOPError

logger = logging.getLogger(__name__)


@dataclass
class SOPInput:
    """An input parameter for an SOP.

    Attributes:
        name: Input parameter name
        context: Description of what this input is for
        default: Optional default value
    """

    name: str
    context: str
    default: str | None = None


@dataclass
class SOPOutput:
    """An output from an SOP.

    Attributes:
        name: Output name
        context: Description of what this output represents
    """

    name: str
    context: str


@dataclass
class SOPStep:
    """A single step in an SOP.

    Attributes:
        prompt: The prompt/instruction for the agent
        tools: List of tools the agent can use for this step
        inputs: List of input names this step uses
    """

    prompt: str
    tools: list[str] = field(default_factory=list)
    inputs: list[str] = field(default_factory=list)


@dataclass
class SOP:
    """A Standard Operating Procedure.

    SOPs are simple YAML files that define a workflow for an agent.

    Attributes:
        name: SOP name
        summary: Brief description of what this SOP does
        tools: List of all tools this SOP may use
        inputs: Input parameters
        outputs: Expected outputs
        steps: Ordered list of steps to execute
    """

    name: str
    summary: str
    tools: list[str] = field(default_factory=list)
    inputs: list[SOPInput] = field(default_factory=list)
    outputs: list[SOPOutput] = field(default_factory=list)
    steps: list[SOPStep] = field(default_factory=list)

    @classmethod
    def from_yaml(cls, yaml_content: str) -> "SOP":
        """Parse an SOP from YAML content.

        Args:
            yaml_content: YAML string

        Returns:
            SOP instance
        """
        data = yaml.safe_load(yaml_content)

        # Parse inputs
        inputs = []
        for inp in data.get("inputs", []):
            inputs.append(
                SOPInput(
                    name=inp.get("name", ""),
                    context=inp.get("context", ""),
                    default=inp.get("default"),
                )
            )

        # Parse outputs
        outputs = []
        for out in data.get("outputs", []):
            outputs.append(
                SOPOutput(
                    name=out.get("name", ""),
                    context=out.get("context", ""),
                )
            )

        # Parse steps
        steps = []
        for step in data.get("steps", []):
            steps.append(
                SOPStep(
                    prompt=step.get("prompt", ""),
                    tools=step.get("tools", []),
                    inputs=step.get("inputs", []),
                )
            )

        return cls(
            name=data.get("name", "Unnamed SOP"),
            summary=data.get("summary", ""),
            tools=data.get("tools", []),
            inputs=inputs,
            outputs=outputs,
            steps=steps,
        )

    @classmethod
    def from_file(cls, path: Path) -> "SOP":
        """Load an SOP from a YAML file.

        Args:
            path: Path to YAML file

        Returns:
            SOP instance

        Raises:
            SOPError: If file cannot be read or parsed
        """
        try:
            content = path.read_text()
            return cls.from_yaml(content)
        except FileNotFoundError:
            raise SOPError(f"SOP file not found: {path}", {"path": str(path)})
        except yaml.YAMLError as e:
            raise SOPError(f"Invalid YAML in SOP file: {e}", {"path": str(path)})

    def to_yaml(self) -> str:
        """Convert SOP to YAML string.

        Returns:
            YAML string
        """
        data: dict[str, Any] = {
            "name": self.name,
            "summary": self.summary,
        }

        if self.tools:
            data["tools"] = self.tools

        if self.inputs:
            data["inputs"] = [
                {"name": inp.name, "context": inp.context}
                | ({"default": inp.default} if inp.default else {})
                for inp in self.inputs
            ]

        if self.outputs:
            data["outputs"] = [{"name": out.name, "context": out.context} for out in self.outputs]

        if self.steps:
            data["steps"] = []
            for step in self.steps:
                step_data: dict[str, Any] = {"prompt": step.prompt}
                if step.tools:
                    step_data["tools"] = step.tools
                if step.inputs:
                    step_data["inputs"] = step.inputs
                data["steps"].append(step_data)

        return yaml.dump(data, default_flow_style=False, sort_keys=False)

    def to_file(self, path: Path) -> None:
        """Save SOP to a YAML file.

        Args:
            path: Path to save to
        """
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(self.to_yaml())
        logger.info(f"Saved SOP to {path}")


class SOPManager:
    """Manages SOPs stored as YAML files."""

    def __init__(self, sops_dir: Path | None = None):
        """Initialize SOP manager.

        Args:
            sops_dir: Directory containing SOP files. Defaults to .xpressai/sops/
        """
        self.sops_dir = sops_dir or Path.cwd() / ".xpressai" / "sops"

    def list_sops(self) -> list[SOP]:
        """List all SOPs.

        Returns:
            List of SOPs
        """
        sops = []

        if not self.sops_dir.exists():
            return sops

        for path in self.sops_dir.glob("*.yaml"):
            try:
                sops.append(SOP.from_file(path))
            except SOPError as e:
                logger.warning(f"Failed to load SOP {path}: {e}")

        for path in self.sops_dir.glob("*.yml"):
            try:
                sops.append(SOP.from_file(path))
            except SOPError as e:
                logger.warning(f"Failed to load SOP {path}: {e}")

        return sops

    def get(self, name: str) -> SOP | None:
        """Get an SOP by name.

        Args:
            name: SOP name (without .yaml extension)

        Returns:
            SOP instance or None
        """
        # Try exact filename match first
        for ext in [".yaml", ".yml"]:
            path = self.sops_dir / f"{name}{ext}"
            if path.exists():
                return SOP.from_file(path)

        # Search by SOP name field
        for sop in self.list_sops():
            if sop.name == name:
                return sop

        return None

    def create(self, sop: SOP, filename: str | None = None) -> Path:
        """Create a new SOP file.

        Args:
            sop: SOP to save
            filename: Optional filename (without extension)

        Returns:
            Path to created file
        """
        if filename is None:
            # Convert name to filename-friendly format
            filename = sop.name.lower().replace(" ", "-")

        path = self.sops_dir / f"{filename}.yaml"
        sop.to_file(path)
        return path

    def delete(self, name: str) -> bool:
        """Delete an SOP file.

        Args:
            name: SOP name or filename (without extension)

        Returns:
            True if deleted, False if not found
        """
        for ext in [".yaml", ".yml"]:
            path = self.sops_dir / f"{name}{ext}"
            if path.exists():
                path.unlink()
                logger.info(f"Deleted SOP: {path}")
                return True

        return False
