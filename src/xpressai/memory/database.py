"""SQLite database management with sqlite-vec support.

Handles all persistent storage for XpressAI including migrations.
"""

import sqlite3
from contextlib import contextmanager
from pathlib import Path
from typing import Generator
import logging

from xpressai.core.exceptions import DatabaseError, MigrationError

logger = logging.getLogger(__name__)

# Check if SQLite extension loading is supported
# macOS system Python and some distributions disable this for security
_EXTENSIONS_SUPPORTED: bool | None = None


def _check_extension_support() -> bool:
    """Check if SQLite extension loading is supported."""
    global _EXTENSIONS_SUPPORTED
    if _EXTENSIONS_SUPPORTED is not None:
        return _EXTENSIONS_SUPPORTED
    
    try:
        conn = sqlite3.connect(":memory:")
        conn.enable_load_extension(True)
        conn.close()
        _EXTENSIONS_SUPPORTED = True
    except AttributeError:
        _EXTENSIONS_SUPPORTED = False
        logger.warning(
            "SQLite extension loading not supported by this Python build. "
            "Vector search will be disabled. To enable, use a Python build "
            "compiled with --enable-loadable-sqlite-extensions (e.g., pyenv or python.org installer)."
        )
    return _EXTENSIONS_SUPPORTED


# Try to import sqlite-vec
try:
    import sqlite_vec

    SQLITE_VEC_AVAILABLE = True
except ImportError:
    SQLITE_VEC_AVAILABLE = False
    logger.info("sqlite-vec not installed, vector search will be disabled")


class Database:
    """Central database manager for XpressAI.

    Uses SQLite with WAL mode for concurrent reads and sqlite-vec for
    vector operations when available.
    """

    def __init__(self, db_path: Path | None = None):
        """Initialize database.

        Args:
            db_path: Path to database file. Defaults to ~/.xpressai/xpressai.db
        """
        if db_path is None:
            db_path = Path.home() / ".xpressai" / "xpressai.db"

        db_path.parent.mkdir(parents=True, exist_ok=True)
        self.db_path = db_path
        self._init_db()

    def _init_db(self) -> None:
        """Initialize database with schema and extensions."""
        with self.connect() as conn:
            # Apply pragmas for performance
            conn.executescript("""
                PRAGMA foreign_keys = ON;
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA cache_size = -64000;
                PRAGMA temp_store = MEMORY;
            """)

            # Run migrations
            self._migrate(conn)

    @contextmanager
    def connect(self) -> Generator[sqlite3.Connection, None, None]:
        """Get a database connection with sqlite-vec loaded.

        Yields:
            SQLite connection with row factory set
        """
        conn = sqlite3.connect(self.db_path, timeout=30.0)
        conn.row_factory = sqlite3.Row

        # Load sqlite-vec extension if available and supported
        if SQLITE_VEC_AVAILABLE and _check_extension_support():
            try:
                conn.enable_load_extension(True)
                sqlite_vec.load(conn)
                conn.enable_load_extension(False)
            except Exception as e:
                # Only log once per session
                if not getattr(self, '_vec_load_warned', False):
                    logger.warning(f"Failed to load sqlite-vec: {e}")
                    self._vec_load_warned = True

        try:
            yield conn
            conn.commit()
        except Exception as e:
            conn.rollback()
            raise DatabaseError(f"Database operation failed: {e}")
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
            result = conn.execute(
                "SELECT value FROM config WHERE key = 'schema_version'"
            ).fetchone()
            version = int(result[0]) if result else 0
        except sqlite3.OperationalError:
            version = 0

        # Apply migrations sequentially
        migrations = [
            (1, self._migrate_v1),
            (2, self._migrate_v2),
            (3, self._migrate_v3),
            (4, self._migrate_v4),
            (5, self._migrate_v5),
            (6, self._migrate_v6),
        ]

        for target_version, migrate_func in migrations:
            if version < target_version:
                try:
                    migrate_func(conn)
                    conn.execute(
                        "INSERT OR REPLACE INTO config (key, value) VALUES (?, ?)",
                        ("schema_version", str(target_version)),
                    )
                    logger.info(f"Applied migration v{target_version}")
                except Exception as e:
                    raise MigrationError(
                        f"Migration to v{target_version} failed: {e}",
                        {"target_version": target_version},
                    )

    def _migrate_v1(self, conn: sqlite3.Connection) -> None:
        """Version 1 schema: Core tables."""
        conn.executescript("""
            -- Memories (Zettelkasten notes)
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
            CREATE INDEX IF NOT EXISTS idx_memories_agent ON memories(agent_id);
            
            -- Memory links (Zettelkasten bidirectional links)
            CREATE TABLE IF NOT EXISTS memory_links (
                from_id TEXT NOT NULL,
                to_id TEXT NOT NULL,
                link_type TEXT DEFAULT 'related',
                strength REAL DEFAULT 1.0,
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
            
            CREATE INDEX IF NOT EXISTS idx_memory_tags_tag ON memory_tags(tag);
            
            -- Memory slots (near-term memory)
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
                context TEXT,
                FOREIGN KEY (parent_task_id) REFERENCES tasks(id) ON DELETE SET NULL
            );
            
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_agent ON tasks(agent_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_task_id);
            
            -- SOPs (Standard Operating Procedures)
            CREATE TABLE IF NOT EXISTS sops (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                description TEXT,
                content TEXT NOT NULL,
                triggers TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                created_by TEXT,
                version INTEGER DEFAULT 1
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
                stopped_at TIMESTAMP,
                error_message TEXT
            );
            
            CREATE INDEX IF NOT EXISTS idx_agents_status ON agents(status);
            
            -- Usage logs
            CREATE TABLE IF NOT EXISTS usage_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                model TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL,
                operation TEXT,
                session_id TEXT
            );
            
            CREATE INDEX IF NOT EXISTS idx_usage_agent ON usage_logs(agent_id);
            CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON usage_logs(timestamp);
            CREATE INDEX IF NOT EXISTS idx_usage_session ON usage_logs(session_id);
            
            -- Budget state
            CREATE TABLE IF NOT EXISTS budget_state (
                agent_id TEXT PRIMARY KEY,
                daily_spent REAL DEFAULT 0.0,
                daily_reset_at TIMESTAMP,
                monthly_spent REAL DEFAULT 0.0,
                monthly_reset_at TIMESTAMP,
                total_spent REAL DEFAULT 0.0,
                is_paused INTEGER DEFAULT 0,
                pause_reason TEXT
            );
            
            -- Agent sessions
            CREATE TABLE IF NOT EXISTS agent_sessions (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                started_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                ended_at TIMESTAMP,
                messages TEXT,
                total_tokens INTEGER DEFAULT 0,
                total_cost REAL DEFAULT 0.0,
                FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
            );
            
            CREATE INDEX IF NOT EXISTS idx_sessions_agent ON agent_sessions(agent_id);
        """)

        # Create vector table if sqlite-vec is available and extensions are supported
        if SQLITE_VEC_AVAILABLE and _check_extension_support():
            try:
                # Check if vec0 is available
                conn.execute("SELECT vec_version()")
                conn.execute("""
                    CREATE VIRTUAL TABLE IF NOT EXISTS memory_embeddings USING vec0(
                        memory_id TEXT PRIMARY KEY,
                        embedding FLOAT[384]
                    )
                """)
            except sqlite3.OperationalError as e:
                logger.debug(f"Could not create vector table: {e}")

    def _migrate_v2(self, conn: sqlite3.Connection) -> None:
        """Version 2 schema: Activity logs and events."""
        conn.executescript("""
            -- Activity logs for observability
            CREATE TABLE IF NOT EXISTS activity_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                agent_id TEXT,
                event_type TEXT NOT NULL,
                event_data TEXT,
                session_id TEXT
            );
            
            CREATE INDEX IF NOT EXISTS idx_activity_agent ON activity_logs(agent_id);
            CREATE INDEX IF NOT EXISTS idx_activity_timestamp ON activity_logs(timestamp);
            CREATE INDEX IF NOT EXISTS idx_activity_type ON activity_logs(event_type);
            
            -- Tool execution logs
            CREATE TABLE IF NOT EXISTS tool_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                agent_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                input_data TEXT,
                output_data TEXT,
                duration_ms INTEGER,
                success INTEGER DEFAULT 1,
                error_message TEXT
            );
            
            CREATE INDEX IF NOT EXISTS idx_tool_agent ON tool_logs(agent_id);
            CREATE INDEX IF NOT EXISTS idx_tool_name ON tool_logs(tool_name);
        """)

    def _migrate_v3(self, conn: sqlite3.Connection) -> None:
        """Version 3 schema: Scheduled tasks."""
        conn.executescript("""
            -- Scheduled tasks
            CREATE TABLE IF NOT EXISTS schedules (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                cron TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                title TEXT NOT NULL,
                description TEXT,
                enabled INTEGER DEFAULT 1,
                last_run TIMESTAMP,
                run_count INTEGER DEFAULT 0,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS idx_schedules_agent ON schedules(agent_id);
            CREATE INDEX IF NOT EXISTS idx_schedules_enabled ON schedules(enabled);
        """)

    def _migrate_v4(self, conn: sqlite3.Connection) -> None:
        """Version 4 schema: Task messages for conversations."""
        conn.executescript("""
            -- Task messages for conversation threads
            CREATE TABLE IF NOT EXISTS task_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_task_messages_task ON task_messages(task_id);
            CREATE INDEX IF NOT EXISTS idx_task_messages_timestamp ON task_messages(timestamp);
        """)

    def _migrate_v5(self, conn: sqlite3.Connection) -> None:
        """Version 5 schema: Agent chat messages for direct conversations."""
        conn.executescript("""
            -- Agent chat messages for direct conversations (not task-based)
            CREATE TABLE IF NOT EXISTS agent_chat_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS idx_agent_chat_agent ON agent_chat_messages(agent_id);
            CREATE INDEX IF NOT EXISTS idx_agent_chat_timestamp ON agent_chat_messages(timestamp);
        """)

    def _migrate_v6(self, conn: sqlite3.Connection) -> None:
        """Version 6 schema: Add cache token columns to usage_logs."""
        # Add cache token columns to usage_logs
        try:
            conn.execute(
                "ALTER TABLE usage_logs ADD COLUMN cache_creation_tokens INTEGER DEFAULT 0"
            )
        except sqlite3.OperationalError:
            pass  # Column already exists

        try:
            conn.execute(
                "ALTER TABLE usage_logs ADD COLUMN cache_read_tokens INTEGER DEFAULT 0"
            )
        except sqlite3.OperationalError:
            pass  # Column already exists

    def backup(self, backup_path: Path | None = None) -> Path:
        """Create a backup of the database.

        Args:
            backup_path: Path for backup file. Auto-generated if not provided.

        Returns:
            Path to backup file
        """
        import shutil
        from datetime import datetime

        if backup_path is None:
            timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
            backup_path = self.db_path.parent / f"backup_{timestamp}.db"

        # Use SQLite's backup API for consistency
        with self.connect() as conn:
            backup_conn = sqlite3.connect(backup_path)
            conn.backup(backup_conn)
            backup_conn.close()

        return backup_path

    def vacuum(self) -> None:
        """Optimize database by running VACUUM."""
        conn = sqlite3.connect(self.db_path)
        conn.execute("VACUUM")
        conn.close()
