# ADR-004: Memory System (Zettelkasten + Vector Search)

## Status
Accepted

## Context

Agents need memory that persists across sessions and scales with usage. Xpress AI's production memory system combines:
- **Zettelkasten**: A graph of interconnected notes
- **Vector search**: Finding relevant memories by semantic similarity
- **Near-term slots**: A small working memory spliced into prompts

This works well for single-user agents but needs adaptation for:
- Multi-user scenarios (layered memory)
- Local-first operation (SQLite, not cloud DB)
- Configurable retention policies

## Decision

We will implement a **three-tier memory system**:

1. **Near-term slots**: 8 slots of active memories in the system prompt
2. **Zettelkasten**: Persistent graph of notes with links
3. **Vector index**: Semantic search over all notes

### Data Model

```python
@dataclass
class Memory:
    id: str
    content: str
    summary: str  # Concise version for slot display
    embedding: list[float]
    
    # Zettelkasten links
    links: list[str]  # IDs of related memories
    backlinks: list[str]  # IDs of memories linking here
    
    # Metadata
    created_at: datetime
    accessed_at: datetime
    access_count: int
    source: str  # "conversation", "user", "agent", "sop"
    tags: list[str]
    
    # Multi-user support
    layer: str  # "shared", "user:{user_id}", "agent:{agent_id}"

@dataclass
class MemorySlot:
    index: int  # 0-7
    memory_id: str | None
    relevance_score: float
    loaded_at: datetime
```

### Storage Schema (SQLite + sqlite-vec)

```sql
-- Core memory table
CREATE TABLE memories (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    summary TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    accessed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    access_count INTEGER DEFAULT 0,
    source TEXT NOT NULL,
    layer TEXT NOT NULL DEFAULT 'shared'
);

-- Vector embeddings (sqlite-vec virtual table)
CREATE VIRTUAL TABLE memory_embeddings USING vec0(
    memory_id TEXT PRIMARY KEY,
    embedding FLOAT[1536]  -- Dimension depends on embedding model
);

-- Zettelkasten links
CREATE TABLE memory_links (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    link_type TEXT DEFAULT 'related',
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (from_id, to_id),
    FOREIGN KEY (from_id) REFERENCES memories(id),
    FOREIGN KEY (to_id) REFERENCES memories(id)
);

-- Tags
CREATE TABLE memory_tags (
    memory_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY (memory_id, tag),
    FOREIGN KEY (memory_id) REFERENCES memories(id)
);

-- Current slot state per agent
CREATE TABLE memory_slots (
    agent_id TEXT NOT NULL,
    slot_index INTEGER NOT NULL,
    memory_id TEXT,
    relevance_score REAL,
    loaded_at TIMESTAMP,
    PRIMARY KEY (agent_id, slot_index)
);
```

### Near-Term Memory Manager

```python
class MemorySlotManager:
    """Manages the 8 near-term memory slots."""
    
    def __init__(self, agent_id: str, db: Database, num_slots: int = 8):
        self.agent_id = agent_id
        self.db = db
        self.num_slots = num_slots
        self.slots: list[MemorySlot] = []
    
    async def load_relevant(self, context: str) -> list[Memory]:
        """Find and load memories relevant to the current context."""
        
        # Get embedding for context
        embedding = await self.embed(context)
        
        # Vector search for relevant memories
        candidates = await self.db.vector_search(
            embedding, 
            limit=20,
            layer_filter=self._get_layer_filter()
        )
        
        # Score by relevance + recency + access frequency
        scored = self._score_candidates(candidates, context)
        
        # Fill slots with top candidates
        for i, (memory, score) in enumerate(scored[:self.num_slots]):
            await self._load_slot(i, memory, score)
        
        return [s.memory for s in self.slots if s.memory_id]
    
    async def evict(self, slot_index: int) -> None:
        """Evict a memory from a slot back to the zettelkasten."""
        slot = self.slots[slot_index]
        
        if slot.memory_id:
            # Update access metadata
            await self.db.touch_memory(slot.memory_id)
            
            # Clear slot
            slot.memory_id = None
            slot.relevance_score = 0.0
            await self._persist_slot(slot)
    
    async def evict_least_relevant(self) -> int:
        """Evict the least relevant slot, return its index."""
        if not any(s.memory_id for s in self.slots):
            return 0
        
        # Find slot with lowest relevance that isn't empty
        occupied = [(i, s) for i, s in enumerate(self.slots) if s.memory_id]
        least_relevant = min(occupied, key=lambda x: x[1].relevance_score)
        
        await self.evict(least_relevant[0])
        return least_relevant[0]
    
    def format_for_prompt(self) -> str:
        """Format current slots for injection into system prompt."""
        lines = ["## Active Memories"]
        
        for slot in self.slots:
            if slot.memory_id:
                memory = self._get_memory(slot.memory_id)
                lines.append(f"- [{slot.index}] {memory.summary}")
        
        return "\n".join(lines)
```

### Vector Search

```python
class VectorMemorySearch:
    """Semantic search over memories using sqlite-vec."""
    
    def __init__(self, db_path: str):
        self.db = sqlite3.connect(db_path)
        self.db.enable_load_extension(True)
        sqlite_vec.load(self.db)
        self.db.enable_load_extension(False)
    
    async def search(
        self, 
        query_embedding: list[float],
        limit: int = 10,
        layer_filter: list[str] | None = None
    ) -> list[tuple[Memory, float]]:
        """Find memories similar to the query embedding."""
        
        from sqlite_vec import serialize_float32
        
        query = """
            SELECT m.*, me.distance
            FROM memory_embeddings me
            JOIN memories m ON m.id = me.memory_id
            WHERE me.embedding MATCH ?
              AND vec_length(me.embedding) = 1536
        """
        
        if layer_filter:
            placeholders = ",".join("?" * len(layer_filter))
            query += f" AND m.layer IN ({placeholders})"
        
        query += " ORDER BY me.distance LIMIT ?"
        
        params = [serialize_float32(query_embedding)]
        if layer_filter:
            params.extend(layer_filter)
        params.append(limit)
        
        results = self.db.execute(query, params).fetchall()
        return [(self._row_to_memory(r), r["distance"]) for r in results]
```

### Zettelkasten Navigation

```python
class Zettelkasten:
    """Navigate the memory graph."""
    
    async def get_related(self, memory_id: str, depth: int = 1) -> list[Memory]:
        """Get memories linked to this one, up to depth hops."""
        visited = set()
        to_visit = [(memory_id, 0)]
        related = []
        
        while to_visit:
            current_id, current_depth = to_visit.pop(0)
            
            if current_id in visited or current_depth > depth:
                continue
            
            visited.add(current_id)
            
            if current_id != memory_id:
                memory = await self.db.get_memory(current_id)
                if memory:
                    related.append(memory)
            
            # Add linked memories to visit
            links = await self.db.get_links(current_id)
            for link in links:
                if link.to_id not in visited:
                    to_visit.append((link.to_id, current_depth + 1))
        
        return related
    
    async def create_link(
        self, 
        from_id: str, 
        to_id: str, 
        link_type: str = "related"
    ) -> None:
        """Create a link between two memories."""
        await self.db.create_link(from_id, to_id, link_type)
    
    async def auto_link(self, memory: Memory) -> list[str]:
        """Automatically find and create links for a new memory."""
        # Vector search for similar memories
        similar = await self.vector_search.search(memory.embedding, limit=5)
        
        links_created = []
        for similar_memory, distance in similar:
            if distance < 0.3:  # Threshold for auto-linking
                await self.create_link(memory.id, similar_memory.id)
                links_created.append(similar_memory.id)
        
        return links_created
```

### Multi-User Memory Layers

For multi-user scenarios, memories are layered:

```
┌─────────────────────────────────┐
│         Agent Layer             │ <- Agent-specific context
├─────────────────────────────────┤
│         User Layer              │ <- User preferences, history
├─────────────────────────────────┤
│        Shared Layer             │ <- Company info, SOPs
└─────────────────────────────────┘
```

```python
class LayeredMemory:
    """Memory system with user/agent/shared layers."""
    
    def __init__(self, db: Database, user_id: str | None, agent_id: str):
        self.db = db
        self.layers = self._build_layers(user_id, agent_id)
    
    def _build_layers(self, user_id: str | None, agent_id: str) -> list[str]:
        layers = ["shared"]
        if user_id:
            layers.append(f"user:{user_id}")
        layers.append(f"agent:{agent_id}")
        return layers
    
    async def search(self, query: str) -> list[Memory]:
        """Search across all accessible layers."""
        embedding = await self.embed(query)
        return await self.vector_search.search(
            embedding,
            layer_filter=self.layers
        )
    
    async def create(
        self, 
        content: str, 
        layer: str = "shared"
    ) -> Memory:
        """Create a memory in the specified layer."""
        if layer not in self.layers:
            raise PermissionError(f"Cannot write to layer: {layer}")
        
        # ... create memory
```

### Retention Policies

Configurable policies for memory cleanup:

```yaml
memory:
  retention:
    policy: none  # none | delete_after | summarize
    
    # For delete_after:
    delete_after_days: 30
    preserve_accessed: true  # Keep if accessed recently
    
    # For summarize:
    summarize_after_days: 7
    cluster_similar: true  # Merge similar memories
```

```python
class MemoryRetention:
    """Implements memory retention policies."""
    
    async def apply_policy(self, policy: RetentionPolicy) -> None:
        if policy.type == "none":
            return
        
        if policy.type == "delete_after":
            await self._delete_old(
                days=policy.delete_after_days,
                preserve_accessed=policy.preserve_accessed
            )
        
        if policy.type == "summarize":
            await self._summarize_old(
                days=policy.summarize_after_days,
                cluster=policy.cluster_similar
            )
```

## Consequences

### Positive
- Proven architecture from Xpress AI production
- SQLite + sqlite-vec is simple and local-first
- Near-term slots provide focused context
- Zettelkasten links enable exploration
- Layered access works for multi-user

### Negative
- Embedding model adds latency (can be local)
- 8 slots may be limiting for complex tasks
- Vector search quality depends on embedding model
- Graph navigation can be expensive at scale

### Implementation Notes

1. Use a small, local embedding model for speed (e.g., all-MiniLM-L6-v2)
2. Batch embedding operations when possible
3. Consider read replicas of SQLite for concurrent access
4. Implement memory compaction for long-running agents

## Related ADRs
- ADR-006: SQLite Storage Layer
- ADR-003: Container Isolation (memory per container)
