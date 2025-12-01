# ADR-006: SQLite Storage Layer

## Status
Accepted

## Context

XpressAI needs persistent storage for:
- Memory (notes, embeddings, links)
- Tasks and SOPs
- Agent state and sessions
- Budget tracking and usage logs
- Configuration

Options considered:
1. **PostgreSQL**: Powerful, but requires a server
2. **SQLite**: Simple, embedded, zero-config
3. **Redis**: Fast, but primarily for caching
4. **Files (JSON/YAML)**: Simple, but no query capability

For a local-first tool that prioritizes simplicity, SQLite is the clear choice. With **sqlite-vec**, we get vector search without needing a separate vector database.

## Decision

We will use **SQLite** as the primary database, with **sqlite-vec** for vector operations.

### Database Structure

```
~/.xpressai/
├── xpressai.db          # Main database
├── xpressai.db-shm      # WAL shared memory (auto-managed)
├── xpressai.db-wal      # WAL log (auto-managed)
└── backups/             # Periodic backups
```

### Schema Overview

```sql
-- Enable foreign keys and WAL mode for performance
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

-- ============================================
-- MEMORY SYSTEM (see ADR-004)
-- ============================================

CREATE TABLE memories (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    summary TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    accessed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    access_count INTEGER DEFAULT 0,
    source TEXT NOT NULL,
    layer TEXT NOT NULL DEFAULT 'shared',
    agent_id TEXT,
    user_id TEXT
);

CREATE INDEX idx_memories_layer ON memories(layer);
CREATE INDEX idx_memories_accessed ON memories(accessed_at);

-- Vector embeddings (sqlite-vec)
CREATE VIRTUAL TABLE memory_embeddings USING vec0(
    memory_id TEXT PRIMARY KEY,
    embedding FLOAT[1536]
);

CREATE TABLE memory_links (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    link_type TEXT DEFAULT 'related',
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (from_id, to_id),
    FOREIGN KEY (from_id) REFERENCES memories(id) ON DELETE CASCADE,
    FOREIGN KEY (to_id) REFERENCES memories(id) ON DELETE CASCADE
);

CREATE TABLE memory_tags (
    memory_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY (memory_id, tag),
    FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE
);

CREATE TABLE memory_slots (
    agent_id TEXT NOT NULL,
    slot_index INTEGER NOT NULL,
    memory_id TEXT,
    relevance_score REAL,
    loaded_at TIMESTAMP,
    PRIMARY KEY (agent_id, slot_index),
    FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE SET NULL
);

-- ============================================
-- TASK SYSTEM (see ADR-009)
-- ============================================

CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'pending',  -- pending, in_progress, completed, blocked
    priority INTEGER DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP,
    agent_id TEXT,
    parent_task_id TEXT,
    sop_id TEXT,
    FOREIGN KEY (parent_task_id) REFERENCES tasks(id) ON DELETE SET NULL,
    FOREIGN KEY (sop_id) REFERENCES sops(id) ON DELETE SET NULL
);

CREATE INDEX idx_tasks_status ON tasks(status);
CREATE INDEX idx_tasks_agent ON tasks(agent_id);

CREATE TABLE sops (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    content TEXT NOT NULL,  -- YAML or Markdown
    triggers TEXT,  -- JSON array of trigger conditions
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    created_by TEXT  -- 'user' or agent_id
);

-- ============================================
-- AGENT STATE
-- ============================================

CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    backend TEXT NOT NULL,
    config TEXT NOT NULL,  -- JSON
    status TEXT NOT NULL DEFAULT 'stopped',  -- stopped, starting, running, error
    container_id TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    started_at TIMESTAMP,
    error_message TEXT
);

CREATE TABLE agent_sessions (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    started_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    ended_at TIMESTAMP,
    messages TEXT,  -- JSON array (for session replay)
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
);

-- ============================================
-- BUDGET TRACKING
-- ============================================

CREATE TABLE usage_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id TEXT NOT NULL,
    timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_usd REAL NOT NULL,
    operation TEXT  -- 'query', 'tool_call', etc.
);

CREATE INDEX idx_usage_agent ON usage_logs(agent_id);
CREATE INDEX idx_usage_timestamp ON usage_logs(timestamp);

CREATE TABLE budget_state (
    agent_id TEXT PRIMARY KEY,
    daily_spent REAL DEFAULT 0.0,
    daily_reset_at TIMESTAMP,
    total_spent REAL DEFAULT 0.0,
    is_paused BOOLEAN DEFAULT FALSE,
    pause_reason TEXT
);

-- ============================================
-- CONFIGURATION
-- ============================================

CREATE TABLE config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
```

### Database Manager

```python
import sqlite3
import sqlite_vec
from pathlib import Path
from contextlib import contextmanager

class Database:
    """Central database manager for XpressAI."""
    
    def __init__(self, db_path: Path | None = None):
        if db_path is None:
            db_path = Path.home() / ".xpressai" / "xpressai.db"
        
        db_path.parent.mkdir(parents=True, exist_ok=True)
        self.db_path = db_path
        self._init_db()
    
    def _init_db(self) -> None:
        """Initialize database with schema and extensions."""
        with self.connect() as conn:
            # Load sqlite-vec extension
            conn.enable_load_extension(True)
            sqlite_vec.load(conn)
            conn.enable_load_extension(False)
            
            # Apply pragmas
            conn.executescript("""
                PRAGMA foreign_keys = ON;
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA cache_size = -64000;  -- 64MB cache
            """)
            
            # Run migrations
            self._migrate(conn)
    
    @contextmanager
    def connect(self):
        """Get a database connection with sqlite-vec loaded."""
        conn = sqlite3.connect(self.db_path)
        conn.row_factory = sqlite3.Row
        
        # Load extension for each connection
        conn.enable_load_extension(True)
        sqlite_vec.load(conn)
        conn.enable_load_extension(False)
        
        try:
            yield conn
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        finally:
            conn.close()
    
    def _migrate(self, conn: sqlite3.Connection) -> None:
        """Run database migrations."""
        # Get current version
        try:
            version = conn.execute(
                "SELECT value FROM config WHERE key = 'schema_version'"
            ).fetchone()
            version = int(version[0]) if version else 0
        except sqlite3.OperationalError:
            version = 0
        
        # Apply migrations in order
        migrations = self._get_migrations()
        for v, migration_sql in migrations:
            if v > version:
                conn.executescript(migration_sql)
                conn.execute(
                    "INSERT OR REPLACE INTO config (key, value) VALUES (?, ?)",
                    ("schema_version", str(v))
                )
```

### Vector Search with sqlite-vec

```python
from sqlite_vec import serialize_float32
import numpy as np

class VectorStore:
    """Vector operations using sqlite-vec."""
    
    def __init__(self, db: Database, embedding_dim: int = 1536):
        self.db = db
        self.embedding_dim = embedding_dim
    
    async def insert(self, id: str, embedding: list[float]) -> None:
        """Insert an embedding."""
        with self.db.connect() as conn:
            conn.execute(
                "INSERT INTO memory_embeddings (memory_id, embedding) VALUES (?, ?)",
                (id, serialize_float32(embedding))
            )
    
    async def search(
        self, 
        query_embedding: list[float], 
        limit: int = 10,
        filters: dict | None = None
    ) -> list[tuple[str, float]]:
        """Find nearest neighbors."""
        
        # Convert to numpy for serialization
        query_vec = np.array(query_embedding, dtype=np.float32)
        
        with self.db.connect() as conn:
            # Basic KNN query
            # Note: sqlite-vec uses L2 distance by default
            sql = """
                SELECT 
                    me.memory_id,
                    vec_distance_L2(me.embedding, ?) as distance
                FROM memory_embeddings me
            """
            
            # Join with memories table for filtering
            if filters:
                sql = """
                    SELECT 
                        me.memory_id,
                        vec_distance_L2(me.embedding, ?) as distance
                    FROM memory_embeddings me
                    JOIN memories m ON m.id = me.memory_id
                    WHERE 1=1
                """
                params = [query_vec]
                
                if "layer" in filters:
                    sql += " AND m.layer IN ({})".format(
                        ",".join("?" * len(filters["layer"]))
                    )
                    params.extend(filters["layer"])
            else:
                params = [query_vec]
            
            sql += " ORDER BY distance LIMIT ?"
            params.append(limit)
            
            results = conn.execute(sql, params).fetchall()
            return [(row["memory_id"], row["distance"]) for row in results]
    
    async def delete(self, id: str) -> None:
        """Delete an embedding."""
        with self.db.connect() as conn:
            conn.execute(
                "DELETE FROM memory_embeddings WHERE memory_id = ?",
                (id,)
            )
```

### Backup and Recovery

```python
class DatabaseBackup:
    """Handles database backups."""
    
    def __init__(self, db: Database, backup_dir: Path | None = None):
        self.db = db
        self.backup_dir = backup_dir or (db.db_path.parent / "backups")
        self.backup_dir.mkdir(exist_ok=True)
    
    def create_backup(self) -> Path:
        """Create a timestamped backup."""
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        backup_path = self.backup_dir / f"xpressai_{timestamp}.db"
        
        with self.db.connect() as source:
            backup = sqlite3.connect(backup_path)
            source.backup(backup)
            backup.close()
        
        return backup_path
    
    def restore_backup(self, backup_path: Path) -> None:
        """Restore from a backup."""
        if not backup_path.exists():
            raise FileNotFoundError(f"Backup not found: {backup_path}")
        
        # Close current connections
        # Replace database file
        shutil.copy(backup_path, self.db.db_path)
```

### Concurrency Considerations

SQLite with WAL mode handles concurrent reads well, but writes are serialized. For our use case:
- Reads: Multiple agents can read simultaneously
- Writes: Batched where possible, use connection pools
- Memory slots: Updated frequently, use transactions

```python
class ConnectionPool:
    """Simple connection pool for SQLite."""
    
    def __init__(self, db: Database, pool_size: int = 5):
        self.db = db
        self.pool_size = pool_size
        self._connections: list[sqlite3.Connection] = []
        self._lock = asyncio.Lock()
    
    async def acquire(self) -> sqlite3.Connection:
        async with self._lock:
            if self._connections:
                return self._connections.pop()
            return self._create_connection()
    
    async def release(self, conn: sqlite3.Connection) -> None:
        async with self._lock:
            if len(self._connections) < self.pool_size:
                self._connections.append(conn)
            else:
                conn.close()
```

## Consequences

### Positive
- Zero configuration (no database server)
- Single file, easy to backup and move
- sqlite-vec provides vector search without external service
- WAL mode enables concurrent reads
- Works offline

### Negative
- Write contention with many agents
- Vector search may be slower than dedicated vector DB at scale
- 1536-dimensional embeddings add size
- No built-in replication

### Performance Notes

- For < 100K memories, sqlite-vec performs well
- Use indexes on frequently queried columns
- Consider periodic VACUUM for long-running instances
- Batch embedding insertions

## Related ADRs
- ADR-004: Memory System (uses this storage)
- ADR-009: Task System (uses this storage)
- ADR-010: Budget Controls (usage tracking)
