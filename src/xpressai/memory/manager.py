"""Memory manager - High-level interface for the memory system.

Coordinates between Zettelkasten, vector search, and memory slots.
"""

from dataclasses import dataclass
from typing import Any
import logging

from xpressai.memory.database import Database
from xpressai.memory.zettelkasten import Memory, Zettelkasten
from xpressai.memory.vector import VectorStore, SearchResult
from xpressai.memory.slots import MemorySlotManager, MemorySlot
from xpressai.core.config import MemoryConfig

logger = logging.getLogger(__name__)


@dataclass
class MemorySearchResult:
    """Combined search result with memory and relevance info."""

    memory: Memory
    relevance_score: float
    source: str  # "vector" | "tag" | "recent" | "linked"


class MemoryManager:
    """High-level interface for the memory system.

    Provides a unified API for:
    - Storing and retrieving memories
    - Semantic search via vector embeddings
    - Managing agent memory slots
    - Memory lifecycle (retention, cleanup)
    """

    def __init__(self, db: Database, config: MemoryConfig | None = None):
        """Initialize memory manager.

        Args:
            db: Database instance
            config: Memory configuration
        """
        self.db = db
        self.config = config or MemoryConfig()

        # Initialize subsystems
        self.zettelkasten = Zettelkasten(db)
        self.vector_store = VectorStore(db, self.config.embedding_dim)

        # Slot managers per agent (lazy init)
        self._slot_managers: dict[str, MemorySlotManager] = {}

    def get_slot_manager(self, agent_id: str) -> MemorySlotManager:
        """Get or create slot manager for an agent.

        Args:
            agent_id: ID of the agent

        Returns:
            MemorySlotManager for the agent
        """
        if agent_id not in self._slot_managers:
            self._slot_managers[agent_id] = MemorySlotManager(
                self.db,
                agent_id,
                num_slots=self.config.near_term_slots,
                eviction_strategy=self.config.eviction,
            )
        return self._slot_managers[agent_id]

    # Memory CRUD operations

    async def add(
        self,
        content: str,
        summary: str | None = None,
        tags: list[str] | None = None,
        source: str = "user",
        layer: str = "shared",
        agent_id: str | None = None,
        user_id: str | None = None,
        links: list[str] | None = None,
    ) -> Memory:
        """Add a new memory.

        Args:
            content: Full content of the memory
            summary: Optional summary
            tags: Optional list of tags
            source: Source of the memory
            layer: Memory layer (shared, user, agent)
            agent_id: Associated agent ID
            user_id: Associated user ID
            links: IDs of memories to link to

        Returns:
            Created memory
        """
        memory = Memory.create(
            content=content,
            summary=summary,
            tags=tags,
            source=source,
            layer=layer,
            agent_id=agent_id,
            user_id=user_id,
        )

        if links:
            memory.links = links

        # Add to Zettelkasten
        memory = await self.zettelkasten.add(memory)

        # Generate and store embedding
        await self.vector_store.add(memory.id, content)

        logger.debug(f"Added memory: {memory.id}")
        return memory

    async def get(self, memory_id: str) -> Memory:
        """Get a memory by ID.

        Args:
            memory_id: ID of memory to retrieve

        Returns:
            Memory instance
        """
        return await self.zettelkasten.get(memory_id)

    async def update(self, memory: Memory) -> Memory:
        """Update an existing memory.

        Args:
            memory: Memory with updated content

        Returns:
            Updated memory
        """
        memory = await self.zettelkasten.update(memory)

        # Update embedding
        await self.vector_store.add(memory.id, memory.content)

        return memory

    async def delete(self, memory_id: str) -> None:
        """Delete a memory.

        Args:
            memory_id: ID of memory to delete
        """
        await self.zettelkasten.delete(memory_id)
        await self.vector_store.delete(memory_id)

    # Search operations

    async def search(
        self,
        query: str,
        limit: int = 10,
        layer: str | None = None,
        agent_id: str | None = None,
        include_tags: list[str] | None = None,
    ) -> list[MemorySearchResult]:
        """Search for relevant memories.

        Combines vector search with tag and metadata filtering.

        Args:
            query: Search query
            limit: Maximum results
            layer: Filter by layer
            agent_id: Filter by agent
            include_tags: Filter by tags

        Returns:
            List of search results sorted by relevance
        """
        results: list[MemorySearchResult] = []
        seen_ids: set[str] = set()

        # Vector search
        vector_results = await self.vector_store.search(query, limit=limit * 2)

        for vr in vector_results:
            if vr.memory_id in seen_ids:
                continue

            try:
                memory = await self.get(vr.memory_id)

                # Apply filters
                if layer and memory.layer != layer:
                    continue
                if agent_id and memory.agent_id != agent_id:
                    continue
                if include_tags and not any(t in memory.tags for t in include_tags):
                    continue

                seen_ids.add(vr.memory_id)
                results.append(
                    MemorySearchResult(
                        memory=memory,
                        relevance_score=vr.score,
                        source="vector",
                    )
                )

            except Exception as e:
                logger.warning(f"Failed to get memory {vr.memory_id}: {e}")

        # Sort by relevance and limit
        results.sort(key=lambda r: r.relevance_score, reverse=True)
        return results[:limit]

    async def search_by_tag(
        self,
        tag: str,
        layer: str | None = None,
        limit: int = 50,
    ) -> list[Memory]:
        """Search memories by tag.

        Args:
            tag: Tag to search for
            layer: Optional layer filter
            limit: Maximum results

        Returns:
            List of matching memories
        """
        return await self.zettelkasten.find_by_tag(tag, layer=layer, limit=limit)

    async def get_recent(
        self,
        layer: str | None = None,
        agent_id: str | None = None,
        limit: int = 10,
    ) -> list[Memory]:
        """Get recently accessed memories.

        Args:
            layer: Optional layer filter
            agent_id: Optional agent filter
            limit: Maximum results

        Returns:
            List of recent memories
        """
        return await self.zettelkasten.find_recent(
            layer=layer,
            agent_id=agent_id,
            limit=limit,
        )

    async def find_related(
        self,
        memory_id: str,
        limit: int = 5,
    ) -> list[MemorySearchResult]:
        """Find memories related to a given memory.

        Combines graph traversal with vector similarity.

        Args:
            memory_id: Source memory ID
            limit: Maximum results

        Returns:
            List of related memories
        """
        results: list[MemorySearchResult] = []
        seen_ids: set[str] = {memory_id}

        # Graph-based (linked memories)
        linked = await self.zettelkasten.traverse(memory_id, depth=1)
        for memory in linked[:limit]:
            if memory.id not in seen_ids:
                seen_ids.add(memory.id)
                results.append(
                    MemorySearchResult(
                        memory=memory,
                        relevance_score=0.9,  # High score for direct links
                        source="linked",
                    )
                )

        # Vector similarity
        similar = await self.vector_store.find_similar(memory_id, limit=limit)
        for sr in similar:
            if sr.memory_id not in seen_ids:
                try:
                    memory = await self.get(sr.memory_id)
                    seen_ids.add(sr.memory_id)
                    results.append(
                        MemorySearchResult(
                            memory=memory,
                            relevance_score=sr.score,
                            source="vector",
                        )
                    )
                except Exception:
                    pass

        results.sort(key=lambda r: r.relevance_score, reverse=True)
        return results[:limit]

    # Slot operations (per-agent near-term memory)

    async def load_to_slot(
        self,
        agent_id: str,
        memory: Memory,
        relevance_score: float = 1.0,
    ) -> int:
        """Load a memory into an agent's slot.

        When slots are full, the evicted memory is linked to related
        memories in the Zettelkasten before being removed from active slots.

        Args:
            agent_id: ID of the agent
            memory: Memory to load
            relevance_score: Relevance score

        Returns:
            Slot index where memory was loaded
        """
        slot_manager = self.get_slot_manager(agent_id)
        slots = await slot_manager.get_slots()

        # Check if we need to evict
        empty_slot = next((s for s in slots if s.is_empty), None)
        if empty_slot is None:
            # Find slot to evict
            evict_index = await slot_manager._select_for_eviction(slots, relevance_score)
            evicted_memory = slots[evict_index].memory

            if evicted_memory:
                # Link evicted memory to related memories in Zettelkasten
                await self._link_to_related(evicted_memory)
                logger.info(f"Evicted memory {evicted_memory.id} from slot, linked to related memories")

        return await slot_manager.load(memory, relevance_score)

    async def _link_to_related(self, memory: Memory, limit: int = 3) -> None:
        """Link a memory to its most related memories in Zettelkasten.

        Args:
            memory: Memory to link
            limit: Maximum number of links to create
        """
        try:
            # Find similar memories via vector search
            similar = await self.vector_store.find_similar(memory.id, limit=limit + 1)

            for result in similar:
                if result.memory_id != memory.id and result.score > 0.5:
                    # Create bidirectional link
                    await self.zettelkasten.link(
                        memory.id, result.memory_id,
                        link_type="related",
                        bidirectional=True
                    )
                    logger.debug(f"Linked memory {memory.id} to {result.memory_id} (score={result.score:.2f})")

        except Exception as e:
            logger.warning(f"Failed to link memory to related: {e}")

    async def get_slots(self, agent_id: str) -> list[MemorySlot]:
        """Get all slots for an agent.

        Args:
            agent_id: ID of the agent

        Returns:
            List of memory slots
        """
        slot_manager = self.get_slot_manager(agent_id)
        return await slot_manager.get_slots()

    async def get_context_for_agent(self, agent_id: str) -> str:
        """Get formatted context string from agent's memory slots.

        Args:
            agent_id: ID of the agent

        Returns:
            Formatted string for injection into prompt
        """
        slot_manager = self.get_slot_manager(agent_id)
        return await slot_manager.get_context_string()

    async def refresh_slots(
        self,
        agent_id: str,
        query: str,
    ) -> None:
        """Refresh agent's slots with relevant memories.

        Args:
            agent_id: ID of the agent
            query: Current context/query to find relevant memories
        """
        slot_manager = self.get_slot_manager(agent_id)

        # Search for relevant memories
        results = await self.search(
            query,
            limit=self.config.near_term_slots,
            agent_id=agent_id,
        )

        # Clear and reload slots
        await slot_manager.clear()

        for result in results:
            await slot_manager.load(result.memory, result.relevance_score)

    # Stats and maintenance

    async def get_stats(self) -> dict[str, Any]:
        """Get statistics about the memory system.

        Returns:
            Dictionary with stats
        """
        zettel_stats = await self.zettelkasten.get_stats()
        vector_stats = await self.vector_store.get_stats()

        return {
            "zettelkasten": zettel_stats,
            "vector_store": vector_stats,
            "config": {
                "near_term_slots": self.config.near_term_slots,
                "eviction": self.config.eviction,
                "retention": self.config.retention,
            },
        }
