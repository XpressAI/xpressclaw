"""Standard Operating Procedures (SOPs) for XpressAI.

SOPs are reusable workflows that agents can follow for consistent behavior.
"""

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any
import uuid
import json
import yaml
import logging

from xpressai.memory.database import Database
from xpressai.core.exceptions import SOPError

logger = logging.getLogger(__name__)


@dataclass
class SOPStep:
    """A single step in an SOP.

    Attributes:
        description: What this step does
        action: The action to perform
        expected_outcome: What should happen
        on_failure: What to do if step fails (retry | skip | abort)
        timeout_seconds: Max time for this step
    """

    description: str
    action: str
    expected_outcome: str | None = None
    on_failure: str = "abort"  # retry | skip | abort
    timeout_seconds: int | None = None


@dataclass
class SOP:
    """A Standard Operating Procedure.

    Attributes:
        id: Unique SOP ID
        name: SOP name
        description: What this SOP does
        steps: List of steps to execute
        triggers: List of triggers
        timeout_seconds: Total timeout
        confirmation_required: Whether to ask user before running
        created_at: Creation timestamp
        created_by: Who created this SOP
        version: SOP version number
    """

    id: str
    name: str
    description: str | None = None
    steps: list[SOPStep] = field(default_factory=list)
    triggers: list[dict[str, Any]] = field(default_factory=list)
    timeout_seconds: int | None = None
    confirmation_required: bool = False
    created_at: datetime = field(default_factory=datetime.now)
    created_by: str | None = None
    version: int = 1

    @classmethod
    def from_yaml(cls, yaml_content: str) -> "SOP":
        """Parse an SOP from YAML content.

        Args:
            yaml_content: YAML string

        Returns:
            SOP instance
        """
        data = yaml.safe_load(yaml_content)

        steps = []
        for step_data in data.get("steps", []):
            steps.append(
                SOPStep(
                    description=step_data.get("description", ""),
                    action=step_data.get("action", ""),
                    expected_outcome=step_data.get("expected_outcome"),
                    on_failure=step_data.get("on_failure", "abort"),
                    timeout_seconds=step_data.get("timeout_seconds"),
                )
            )

        return cls(
            id=data.get("id", str(uuid.uuid4())),
            name=data.get("name", "Unnamed SOP"),
            description=data.get("description"),
            steps=steps,
            triggers=data.get("triggers", []),
            timeout_seconds=data.get("timeout_seconds"),
            confirmation_required=data.get("confirmation_required", False),
            created_by=data.get("created_by"),
        )

    def to_yaml(self) -> str:
        """Convert SOP to YAML string.

        Returns:
            YAML string
        """
        data = {
            "id": self.id,
            "name": self.name,
            "description": self.description,
            "steps": [
                {
                    "description": step.description,
                    "action": step.action,
                    "expected_outcome": step.expected_outcome,
                    "on_failure": step.on_failure,
                    "timeout_seconds": step.timeout_seconds,
                }
                for step in self.steps
            ],
            "triggers": self.triggers,
            "timeout_seconds": self.timeout_seconds,
            "confirmation_required": self.confirmation_required,
        }

        return yaml.dump(data, default_flow_style=False)


class SOPManager:
    """Manages SOPs storage and execution."""

    def __init__(self, db: Database):
        """Initialize SOP manager.

        Args:
            db: Database instance
        """
        self.db = db

    async def create(self, sop: SOP) -> SOP:
        """Create a new SOP.

        Args:
            sop: SOP to create

        Returns:
            Created SOP
        """
        with self.db.connect() as conn:
            content = sop.to_yaml()
            triggers = json.dumps(sop.triggers) if sop.triggers else None

            conn.execute(
                """
                INSERT INTO sops (id, name, description, content, triggers, created_by, version)
                VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
                (
                    sop.id,
                    sop.name,
                    sop.description,
                    content,
                    triggers,
                    sop.created_by,
                    sop.version,
                ),
            )

        return sop

    async def get(self, sop_id: str) -> SOP:
        """Get an SOP by ID.

        Args:
            sop_id: SOP ID

        Returns:
            SOP instance

        Raises:
            SOPError: If SOP not found
        """
        with self.db.connect() as conn:
            row = conn.execute("SELECT * FROM sops WHERE id = ?", (sop_id,)).fetchone()

            if row is None:
                raise SOPError(f"SOP not found: {sop_id}", {"sop_id": sop_id})

            return self._row_to_sop(row)

    async def get_by_name(self, name: str) -> SOP | None:
        """Get an SOP by name.

        Args:
            name: SOP name

        Returns:
            SOP instance or None
        """
        with self.db.connect() as conn:
            row = conn.execute("SELECT * FROM sops WHERE name = ?", (name,)).fetchone()

            if row is None:
                return None

            return self._row_to_sop(row)

    async def list_sops(self) -> list[SOP]:
        """List all SOPs.

        Returns:
            List of SOPs
        """
        with self.db.connect() as conn:
            rows = conn.execute("SELECT * FROM sops ORDER BY name").fetchall()

            return [self._row_to_sop(row) for row in rows]

    async def update(self, sop: SOP) -> SOP:
        """Update an existing SOP.

        Args:
            sop: SOP with updates

        Returns:
            Updated SOP
        """
        sop.version += 1

        with self.db.connect() as conn:
            content = sop.to_yaml()
            triggers = json.dumps(sop.triggers) if sop.triggers else None

            conn.execute(
                """
                UPDATE sops
                SET name = ?, description = ?, content = ?, triggers = ?, 
                    version = ?, updated_at = CURRENT_TIMESTAMP
                WHERE id = ?
            """,
                (
                    sop.name,
                    sop.description,
                    content,
                    triggers,
                    sop.version,
                    sop.id,
                ),
            )

        return sop

    async def delete(self, sop_id: str) -> None:
        """Delete an SOP.

        Args:
            sop_id: SOP ID
        """
        with self.db.connect() as conn:
            conn.execute("DELETE FROM sops WHERE id = ?", (sop_id,))

    async def find_by_trigger(self, trigger_type: str, trigger_value: str) -> list[SOP]:
        """Find SOPs that match a trigger.

        Args:
            trigger_type: Type of trigger (schedule, event, etc.)
            trigger_value: Trigger value to match

        Returns:
            List of matching SOPs
        """
        sops = await self.list_sops()
        matching = []

        for sop in sops:
            for trigger in sop.triggers:
                if trigger.get(trigger_type) == trigger_value:
                    matching.append(sop)
                    break

        return matching

    def _row_to_sop(self, row) -> SOP:
        """Convert database row to SOP."""
        sop = SOP.from_yaml(row["content"])
        sop.id = row["id"]
        sop.name = row["name"]
        sop.description = row["description"]
        sop.version = row["version"]

        if row["triggers"]:
            try:
                sop.triggers = json.loads(row["triggers"])
            except json.JSONDecodeError:
                pass

        if row["created_at"]:
            sop.created_at = datetime.fromisoformat(row["created_at"])

        sop.created_by = row["created_by"]

        return sop
