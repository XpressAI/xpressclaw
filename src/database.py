"""SQLite database management with sqlite-vec support."""

import sqlite3
from contextlib import contextmanager
from pathlib import Path
from typing import Generator

try:
    import sqlite_vec
    SQLITE_VEC_AVAILABLE = True
except ImportError:
    SQLITE_VEC_AVAILABLE = False


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
            # Apply pragmas
            conn.executescript("""
                PRAGMA foreign_keys = ON;
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA cache_size = -64000;
            """)
            
            # Run migrations
            self._migrate(conn)
    
    @contextmanager
    def connect(self) -> Generator[sqlite3.Connection, None, None]:
        """Get a database connection with sqlite-vec loaded."""
        conn = sqlite3.connect(self.db_path)
        conn.row_factory = sqlite3.Row
        
        # Load sqlite-vec extension if available
        if SQLITE_VEC_AVAILABLE:
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
        # Create config table for schema versioning
        conn.execute("""
            CREATE TABLE IF NOT EXISTS config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
        """)
        
        # Get current version
        try:
            version = conn.execute(
                "SELECT value FROM config WHERE key = 'schema_version'"
            ).fetchone()
            version = int(version[0]) if version else 0
        except sqlite3.OperationalError:
            version = 0
        
        # Apply migrations
        if version < 1:
            self._migrate_v1(conn)
            conn.execute(
                "INSERT OR REPLACE INTO config (key, value) VALUES (?, ?)",
                ("schema_version", "1")
            )
    
    def _migrate_v1(self, conn: sqlite3.Connection) -> None:
        """Version 1 schema: Core tables."""
        conn.executescript("""
            -- Memories
            CREATE TABLE IF NOT EXISTS memories (
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
            
            CREATE INDEX IF NOT EXISTS idx_memories_layer ON memories(layer);
            CREATE INDEX IF NOT EXISTS idx_memories_accessed ON memories(accessed_at);
            
            -- Memory links (Zettelkasten)
            CREATE TABLE IF NOT EXISTS memory_links (
                from_id TEXT NOT NULL,
                to_id TEXT NOT NULL,
                link_type TEXT DEFAULT 'related',
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (from_id, to_id),
                FOREIGN KEY (from_id) REFERENCES memories(id) ON DELETE CASCADE,
                FOREIGN KEY (to_id) REFERENCES memories(id) ON DELETE CASCADE
            );
            
            -- Memory tags
            CREATE TABLE IF NOT EXISTS memory_tags (
                memory_id TEXT NOT NULL,
                tag TEXT NOT NULL,
                PRIMARY KEY (memory_id, tag),
                FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE
            );
            
            -- Memory slots
            CREATE TABLE IF NOT EXISTS memory_slots (
                agent_id TEXT NOT NULL,
                slot_index INTEGER NOT NULL,
                memory_id TEXT,
                relevance_score REAL,
                loaded_at TIMESTAMP,
                PRIMARY KEY (agent_id, slot_index),
                FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE SET NULL
            );
            
            -- Tasks
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                priority INTEGER DEFAULT 0,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                completed_at TIMESTAMP,
                agent_id TEXT,
                parent_task_id TEXT,
                sop_id TEXT,
                FOREIGN KEY (parent_task_id) REFERENCES tasks(id) ON DELETE SET NULL
            );
            
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_agent ON tasks(agent_id);
            
            -- SOPs
            CREATE TABLE IF NOT EXISTS sops (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                content TEXT NOT NULL,
                triggers TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                created_by TEXT
            );
            
            -- Agents
            CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                backend TEXT NOT NULL,
                config TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'stopped',
                container_id TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                started_at TIMESTAMP,
                error_message TEXT
            );
            
            -- Usage logs
            CREATE TABLE IF NOT EXISTS usage_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                model TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL,
                operation TEXT
            );
            
            CREATE INDEX IF NOT EXISTS idx_usage_agent ON usage_logs(agent_id);
            CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON usage_logs(timestamp);
            
            -- Budget state
            CREATE TABLE IF NOT EXISTS budget_state (
                agent_id TEXT PRIMARY KEY,
                daily_spent REAL DEFAULT 0.0,
                daily_reset_at TIMESTAMP,
                total_spent REAL DEFAULT 0.0,
                is_paused INTEGER DEFAULT 0,
                pause_reason TEXT
            );
        """)
        
        # Create vector table if sqlite-vec is available
        if SQLITE_VEC_AVAILABLE:
            try:
                conn.execute("""
                    CREATE VIRTUAL TABLE IF NOT EXISTS memory_embeddings USING vec0(
                        memory_id TEXT PRIMARY KEY,
                        embedding FLOAT[1536]
                    )
                """)
            except sqlite3.OperationalError:
                # Table might already exist or vec0 not available
                pass
