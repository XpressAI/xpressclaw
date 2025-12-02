"""Near-term memory slots for agents.

Manages the 8 memory slots that are spliced into agent context.
Implements eviction strategies for when slots are full.
"""

from dataclasses import dataclass
from datetime import datetime
from typing import Any
import logging

from xpressai.memory.database import Database
from xpressai.memory.zettelkasten import Memory

logger = logging.getLogger(__name__)


@dataclass
class MemorySlot:
    """A single memory slot.

    Attributes:
        index: Slot index (0-7)
        memory: The memory in this slot
        relevance_score: How relevant this memory is
        loaded_at: When the memory was loaded into the slot
    """

    index: int
    memory: Memory | None
    relevance_score: float = 0.0
    loaded_at: datetime | None = None

    @property
    def is_empty(self) -> bool:
        """Check if slot is empty."""
        return self.memory is None


class MemorySlotManager:
    """Manages near-term memory slots for an agent.

    Each agent has 8 slots that hold memories to be spliced into context.
    When slots are full, memories are evicted based on the configured strategy.
    """

    def __init__(
        self,
        db: Database,
        agent_id: str,
        num_slots: int = 8,
        eviction_strategy: str = "least-recently-relevant",
    ):
        """Initialize slot manager.

        Args:
            db: Database instance
            agent_id: ID of the agent
            num_slots: Number of slots (default 8)
            eviction_strategy: Strategy for eviction (lru | least-recently-relevant)
        """
        self.db = db
        self.agent_id = agent_id
        self.num_slots = num_slots
        self.eviction_strategy = eviction_strategy

        # Initialize slots in database
        self._init_slots()

    def _init_slots(self) -> None:
        """Initialize empty slots in database."""
        with self.db.connect() as conn:
            for i in range(self.num_slots):
                conn.execute(
                    """
                    INSERT OR IGNORE INTO memory_slots (agent_id, slot_index)
                    VALUES (?, ?)
                """,
                    (self.agent_id, i),
                )

    async def get_slots(self) -> list[MemorySlot]:
        """Get all memory slots.

        Returns:
            List of MemorySlot objects
        """
        from xpressai.memory.zettelkasten import Zettelkasten

        slots = []

        with self.db.connect() as conn:
            rows = conn.execute(
                """
                SELECT slot_index, memory_id, relevance_score, loaded_at
                FROM memory_slots
                WHERE agent_id = ?
                ORDER BY slot_index
            """,
                (self.agent_id,),
            ).fetchall()

            zettel = Zettelkasten(self.db)

            for row in rows:
                memory = None
                if row["memory_id"]:
                    try:
                        memory = await zettel.get(row["memory_id"])
                    except Exception:
                        pass

                loaded_at = None
                if row["loaded_at"]:
                    loaded_at = datetime.fromisoformat(row["loaded_at"])

                slots.append(
                    MemorySlot(
                        index=row["slot_index"],
                        memory=memory,
                        relevance_score=row["relevance_score"] or 0.0,
                        loaded_at=loaded_at,
                    )
                )

        return slots

    async def load(
        self, memory: Memory, relevance_score: float = 1.0, slot_index: int | None = None
    ) -> int:
        """Load a memory into a slot.

        Args:
            memory: Memory to load
            relevance_score: Relevance score for eviction decisions
            slot_index: Specific slot to load into (auto-selects if None)

        Returns:
            Index of slot where memory was loaded
        """
        if slot_index is None:
            slot_index = await self._find_slot(relevance_score)

        with self.db.connect() as conn:
            conn.execute(
                """
                UPDATE memory_slots
                SET memory_id = ?, relevance_score = ?, loaded_at = CURRENT_TIMESTAMP
                WHERE agent_id = ? AND slot_index = ?
            """,
                (memory.id, relevance_score, self.agent_id, slot_index),
            )

        logger.debug(f"Loaded memory {memory.id} into slot {slot_index}")
        return slot_index

    async def unload(self, slot_index: int) -> Memory | None:
        """Unload a memory from a slot.

        Args:
            slot_index: Index of slot to unload

        Returns:
            The unloaded memory, or None if slot was empty
        """
        slots = await self.get_slots()

        if slot_index >= len(slots):
            return None

        memory = slots[slot_index].memory

        with self.db.connect() as conn:
            conn.execute(
                """
                UPDATE memory_slots
                SET memory_id = NULL, relevance_score = NULL, loaded_at = NULL
                WHERE agent_id = ? AND slot_index = ?
            """,
                (self.agent_id, slot_index),
            )

        return memory

    async def update_relevance(self, slot_index: int, relevance_score: float) -> None:
        """Update the relevance score of a slot.

        Args:
            slot_index: Index of slot
            relevance_score: New relevance score
        """
        with self.db.connect() as conn:
            conn.execute(
                """
                UPDATE memory_slots
                SET relevance_score = ?
                WHERE agent_id = ? AND slot_index = ?
            """,
                (relevance_score, self.agent_id, slot_index),
            )

    async def clear(self) -> None:
        """Clear all slots."""
        with self.db.connect() as conn:
            conn.execute(
                """
                UPDATE memory_slots
                SET memory_id = NULL, relevance_score = NULL, loaded_at = NULL
                WHERE agent_id = ?
            """,
                (self.agent_id,),
            )

    async def _find_slot(self, relevance_score: float) -> int:
        """Find the best slot to use.

        Args:
            relevance_score: Relevance of the memory to load

        Returns:
            Index of slot to use
        """
        slots = await self.get_slots()

        # First, try to find an empty slot
        for slot in slots:
            if slot.is_empty:
                return slot.index

        # All slots full, need to evict
        return await self._select_for_eviction(slots, relevance_score)

    async def _select_for_eviction(self, slots: list[MemorySlot], new_relevance: float) -> int:
        """Select a slot for eviction.

        Args:
            slots: Current slots
            new_relevance: Relevance of new memory

        Returns:
            Index of slot to evict
        """
        if self.eviction_strategy == "lru":
            # Evict least recently loaded
            oldest = min(slots, key=lambda s: s.loaded_at or datetime.min)
            return oldest.index

        elif self.eviction_strategy == "least-recently-relevant":
            # Combine recency and relevance
            # Score = relevance * recency_factor
            now = datetime.now()

            def score(slot: MemorySlot) -> float:
                if slot.loaded_at is None:
                    return float("inf")  # Empty slots have infinite score

                age_hours = (now - slot.loaded_at).total_seconds() / 3600
                recency_factor = 1.0 / (1.0 + age_hours)  # Decay over time
                return slot.relevance_score * recency_factor

            # Evict lowest score
            lowest = min(slots, key=score)
            return lowest.index

        else:
            # Default: evict first slot
            return 0

    async def get_context_string(self) -> str:
        """Generate context string from active memories.

        Returns:
            Formatted string of memories for injection into prompt
        """
        slots = await self.get_slots()
        active_slots = [s for s in slots if not s.is_empty]

        if not active_slots:
            return ""

        lines = ["## Active Memories\n"]

        for slot in sorted(active_slots, key=lambda s: s.relevance_score, reverse=True):
            if slot.memory:
                lines.append(f"### {slot.memory.summary}")
                lines.append(slot.memory.content)
                lines.append("")

        return "\n".join(lines)

    async def get_stats(self) -> dict[str, Any]:
        """Get statistics about slots.

        Returns:
            Dictionary with stats
        """
        slots = await self.get_slots()

        active = sum(1 for s in slots if not s.is_empty)
        avg_relevance = 0.0
        if active > 0:
            avg_relevance = sum(s.relevance_score for s in slots if not s.is_empty) / active

        return {
            "total_slots": self.num_slots,
            "active_slots": active,
            "empty_slots": self.num_slots - active,
            "avg_relevance": avg_relevance,
            "eviction_strategy": self.eviction_strategy,
        }
