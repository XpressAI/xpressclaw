"""Zettelkasten-style note storage with bidirectional links.

Implements a graph-based knowledge store where each note (memory) can
link to other notes, creating a web of interconnected knowledge.
"""

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any
import uuid
import json
import re
import logging

from xpressai.memory.database import Database
from xpressai.core.exceptions import MemoryNotFoundError

logger = logging.getLogger(__name__)


@dataclass
class Memory:
    """A single memory/note in the Zettelkasten.

    Attributes:
        id: Unique identifier
        content: Full content of the memory
        summary: Brief summary for display
        tags: List of tags for categorization
        links: IDs of linked memories
        backlinks: IDs of memories linking to this one
        metadata: Additional metadata
        created_at: Creation timestamp
        accessed_at: Last access timestamp
        access_count: Number of times accessed
        source: Where this memory came from
        layer: Memory layer (shared, user, agent)
        agent_id: Associated agent if layer is 'agent'
        user_id: Associated user if layer is 'user'
    """

    id: str
    content: str
    summary: str
    tags: list[str] = field(default_factory=list)
    links: list[str] = field(default_factory=list)
    backlinks: list[str] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)
    created_at: datetime = field(default_factory=datetime.now)
    accessed_at: datetime = field(default_factory=datetime.now)
    access_count: int = 0
    source: str = "user"
    layer: str = "shared"
    agent_id: str | None = None
    user_id: str | None = None

    @classmethod
    def create(
        cls,
        content: str,
        summary: str | None = None,
        tags: list[str] | None = None,
        source: str = "user",
        layer: str = "shared",
        agent_id: str | None = None,
        user_id: str | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> "Memory":
        """Create a new memory with auto-generated ID and summary.

        Args:
            content: Full content of the memory
            summary: Optional summary (auto-generated if not provided)
            tags: Optional list of tags
            source: Source of the memory
            layer: Memory layer
            agent_id: Associated agent ID
            user_id: Associated user ID
            metadata: Additional metadata

        Returns:
            New Memory instance
        """
        if summary is None:
            # Auto-generate summary from first line or first 100 chars
            first_line = content.split("\n")[0].strip()
            summary = first_line[:100] + "..." if len(first_line) > 100 else first_line

        return cls(
            id=str(uuid.uuid4()),
            content=content,
            summary=summary,
            tags=tags or [],
            source=source,
            layer=layer,
            agent_id=agent_id,
            user_id=user_id,
            metadata=metadata or {},
        )


class Zettelkasten:
    """Graph-based knowledge store with bidirectional links.

    Implements a Zettelkasten system where memories can be linked to each
    other, forming a knowledge graph that can be traversed and searched.
    """

    def __init__(self, db: Database):
        """Initialize Zettelkasten.

        Args:
            db: Database instance for persistence
        """
        self.db = db

    async def add(self, memory: Memory) -> Memory:
        """Add a new memory to the Zettelkasten.

        Args:
            memory: Memory to add

        Returns:
            Added memory with ID
        """
        with self.db.connect() as conn:
            conn.execute(
                """
                INSERT INTO memories 
                (id, content, summary, source, layer, agent_id, user_id)
                VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
                (
                    memory.id,
                    memory.content,
                    memory.summary,
                    memory.source,
                    memory.layer,
                    memory.agent_id,
                    memory.user_id,
                ),
            )

            # Add tags
            for tag in memory.tags:
                conn.execute(
                    "INSERT INTO memory_tags (memory_id, tag) VALUES (?, ?)", (memory.id, tag)
                )

            # Add links
            for link_id in memory.links:
                await self._add_link(conn, memory.id, link_id)

        # Auto-detect and create links from content
        await self._auto_link(memory)

        return memory

    async def get(self, memory_id: str) -> Memory:
        """Get a memory by ID.

        Args:
            memory_id: ID of memory to retrieve

        Returns:
            Memory instance

        Raises:
            MemoryNotFoundError: If memory doesn't exist
        """
        with self.db.connect() as conn:
            row = conn.execute("SELECT * FROM memories WHERE id = ?", (memory_id,)).fetchone()

            if row is None:
                raise MemoryNotFoundError(
                    f"Memory not found: {memory_id}", {"memory_id": memory_id}
                )

            # Update access stats
            conn.execute(
                """
                UPDATE memories 
                SET accessed_at = CURRENT_TIMESTAMP, access_count = access_count + 1
                WHERE id = ?
            """,
                (memory_id,),
            )

            # Get tags
            tags = [
                r["tag"]
                for r in conn.execute(
                    "SELECT tag FROM memory_tags WHERE memory_id = ?", (memory_id,)
                ).fetchall()
            ]

            # Get links
            links = [
                r["to_id"]
                for r in conn.execute(
                    "SELECT to_id FROM memory_links WHERE from_id = ?", (memory_id,)
                ).fetchall()
            ]

            # Get backlinks
            backlinks = [
                r["from_id"]
                for r in conn.execute(
                    "SELECT from_id FROM memory_links WHERE to_id = ?", (memory_id,)
                ).fetchall()
            ]

            return Memory(
                id=row["id"],
                content=row["content"],
                summary=row["summary"],
                tags=tags,
                links=links,
                backlinks=backlinks,
                created_at=datetime.fromisoformat(row["created_at"])
                if row["created_at"]
                else datetime.now(),
                accessed_at=datetime.fromisoformat(row["accessed_at"])
                if row["accessed_at"]
                else datetime.now(),
                access_count=row["access_count"],
                source=row["source"],
                layer=row["layer"],
                agent_id=row["agent_id"],
                user_id=row["user_id"],
            )

    async def update(self, memory: Memory) -> Memory:
        """Update an existing memory.

        Args:
            memory: Memory with updated content

        Returns:
            Updated memory
        """
        with self.db.connect() as conn:
            conn.execute(
                """
                UPDATE memories 
                SET content = ?, summary = ?, accessed_at = CURRENT_TIMESTAMP
                WHERE id = ?
            """,
                (memory.content, memory.summary, memory.id),
            )

            # Update tags
            conn.execute("DELETE FROM memory_tags WHERE memory_id = ?", (memory.id,))
            for tag in memory.tags:
                conn.execute(
                    "INSERT INTO memory_tags (memory_id, tag) VALUES (?, ?)", (memory.id, tag)
                )

        return memory

    async def delete(self, memory_id: str) -> None:
        """Delete a memory.

        Args:
            memory_id: ID of memory to delete
        """
        with self.db.connect() as conn:
            conn.execute("DELETE FROM memories WHERE id = ?", (memory_id,))

    async def link(
        self, from_id: str, to_id: str, link_type: str = "related", bidirectional: bool = True
    ) -> None:
        """Create a link between two memories.

        Args:
            from_id: Source memory ID
            to_id: Target memory ID
            link_type: Type of link
            bidirectional: Whether to create reverse link too
        """
        with self.db.connect() as conn:
            await self._add_link(conn, from_id, to_id, link_type)
            if bidirectional:
                await self._add_link(conn, to_id, from_id, link_type)

    async def _add_link(self, conn, from_id: str, to_id: str, link_type: str = "related") -> None:
        """Add a single directional link."""
        conn.execute(
            """
            INSERT OR IGNORE INTO memory_links (from_id, to_id, link_type)
            VALUES (?, ?, ?)
        """,
            (from_id, to_id, link_type),
        )

    async def unlink(self, from_id: str, to_id: str) -> None:
        """Remove a link between two memories.

        Args:
            from_id: Source memory ID
            to_id: Target memory ID
        """
        with self.db.connect() as conn:
            conn.execute(
                "DELETE FROM memory_links WHERE from_id = ? AND to_id = ?", (from_id, to_id)
            )
            conn.execute(
                "DELETE FROM memory_links WHERE from_id = ? AND to_id = ?", (to_id, from_id)
            )

    async def find_by_tag(
        self, tag: str, layer: str | None = None, limit: int = 100
    ) -> list[Memory]:
        """Find memories by tag.

        Args:
            tag: Tag to search for
            layer: Optional layer filter
            limit: Maximum results

        Returns:
            List of matching memories
        """
        with self.db.connect() as conn:
            sql = """
                SELECT DISTINCT m.* FROM memories m
                JOIN memory_tags t ON m.id = t.memory_id
                WHERE t.tag = ?
            """
            params: list[Any] = [tag]

            if layer:
                sql += " AND m.layer = ?"
                params.append(layer)

            sql += " ORDER BY m.accessed_at DESC LIMIT ?"
            params.append(limit)

            rows = conn.execute(sql, params).fetchall()

            return [await self.get(row["id"]) for row in rows]

    async def find_recent(
        self, layer: str | None = None, agent_id: str | None = None, limit: int = 10
    ) -> list[Memory]:
        """Find recently accessed memories.

        Args:
            layer: Optional layer filter
            agent_id: Optional agent filter
            limit: Maximum results

        Returns:
            List of recent memories
        """
        with self.db.connect() as conn:
            sql = "SELECT * FROM memories WHERE 1=1"
            params: list[Any] = []

            if layer:
                sql += " AND layer = ?"
                params.append(layer)

            if agent_id:
                sql += " AND agent_id = ?"
                params.append(agent_id)

            sql += " ORDER BY accessed_at DESC LIMIT ?"
            params.append(limit)

            rows = conn.execute(sql, params).fetchall()

            return [await self.get(row["id"]) for row in rows]

    async def traverse(
        self, start_id: str, depth: int = 2, link_types: list[str] | None = None
    ) -> list[Memory]:
        """Traverse the graph from a starting memory.

        Args:
            start_id: ID of memory to start from
            depth: How many hops to traverse
            link_types: Optional filter for link types

        Returns:
            List of connected memories
        """
        visited = set()
        to_visit = [(start_id, 0)]
        results = []

        while to_visit:
            current_id, current_depth = to_visit.pop(0)

            if current_id in visited or current_depth > depth:
                continue

            visited.add(current_id)

            try:
                memory = await self.get(current_id)
                if current_id != start_id:  # Don't include start node
                    results.append(memory)

                # Add linked memories to visit
                if current_depth < depth:
                    for link_id in memory.links + memory.backlinks:
                        if link_id not in visited:
                            to_visit.append((link_id, current_depth + 1))

            except MemoryNotFoundError:
                continue

        return results

    async def _auto_link(self, memory: Memory) -> None:
        """Automatically detect and create links from content.

        Looks for [[wiki-style]] links and references to other memories.
        """
        # Find [[wiki-style]] links
        link_pattern = r"\[\[([^\]]+)\]\]"
        matches = re.findall(link_pattern, memory.content)

        for match in matches:
            # Try to find a memory with matching summary or ID
            with self.db.connect() as conn:
                row = conn.execute(
                    """
                    SELECT id FROM memories 
                    WHERE summary LIKE ? OR id = ?
                    LIMIT 1
                """,
                    (f"%{match}%", match),
                ).fetchone()

                if row:
                    await self.link(memory.id, row["id"])

    async def get_stats(self) -> dict[str, Any]:
        """Get statistics about the Zettelkasten.

        Returns:
            Dictionary with stats
        """
        with self.db.connect() as conn:
            total = conn.execute("SELECT COUNT(*) FROM memories").fetchone()[0]
            by_layer = {
                row["layer"]: row["count"]
                for row in conn.execute("""
                    SELECT layer, COUNT(*) as count 
                    FROM memories GROUP BY layer
                """).fetchall()
            }
            total_links = conn.execute("SELECT COUNT(*) FROM memory_links").fetchone()[0]

            return {
                "total_memories": total,
                "by_layer": by_layer,
                "total_links": total_links,
            }
