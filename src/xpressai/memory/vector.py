"""Vector storage and semantic search using sqlite-vec.

Provides embedding-based similarity search for memories.
"""

import asyncio
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from typing import Any
import logging
import os

from xpressai.memory.database import Database, SQLITE_VEC_AVAILABLE
from xpressai.core.exceptions import EmbeddingError

logger = logging.getLogger(__name__)

# Shared thread pool for CPU-bound embedding operations
_embedding_executor: ThreadPoolExecutor | None = None

def _get_executor() -> ThreadPoolExecutor:
    """Get or create the shared thread pool executor."""
    global _embedding_executor
    if _embedding_executor is None:
        _embedding_executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="embedding")
    return _embedding_executor

# Check if embedding dependencies are available (lazy import to avoid loading PyTorch at startup)
def _check_embeddings_available() -> bool:
    """Check if sentence-transformers is importable without actually importing it."""
    try:
        import importlib.util
        return importlib.util.find_spec("sentence_transformers") is not None
    except Exception:
        return False

EMBEDDINGS_AVAILABLE = _check_embeddings_available()

# These will be imported lazily when first needed
np = None
SentenceTransformer = None
_embeddings_imported = False

def _lazy_import_embeddings():
    """Lazily import heavy embedding dependencies."""
    global np, SentenceTransformer, _embeddings_imported, EMBEDDINGS_AVAILABLE

    if _embeddings_imported:
        return EMBEDDINGS_AVAILABLE

    _embeddings_imported = True

    if not EMBEDDINGS_AVAILABLE:
        logger.info("sentence-transformers not available, embedding search disabled. "
                    "Install with: pip install xpressai[local]")
        return False

    try:
        # Disable tokenizers parallelism before importing
        os.environ["TOKENIZERS_PARALLELISM"] = "false"

        import numpy as _np
        from sentence_transformers import SentenceTransformer as _ST

        # Set globals
        globals()['np'] = _np
        globals()['SentenceTransformer'] = _ST

        logger.info("Loaded sentence-transformers for embedding search")
        return True
    except ImportError as e:
        logger.warning(f"Failed to import sentence-transformers: {e}")
        EMBEDDINGS_AVAILABLE = False
        return False


@dataclass
class SearchResult:
    """A search result with similarity score."""

    memory_id: str
    score: float
    distance: float


class EmbeddingModel:
    """Wrapper for embedding model.

    Uses sentence-transformers for generating embeddings from text.
    """

    def __init__(self, model_name: str = "all-MiniLM-L6-v2"):
        """Initialize embedding model.

        Args:
            model_name: Name of the sentence-transformers model
        """
        self.model_name = model_name
        self._model = None
        self.dim = 384  # Default for MiniLM

    def _get_model(self):
        """Lazy load the model."""
        if self._model is None:
            # Trigger lazy import of heavy dependencies
            if not _lazy_import_embeddings():
                raise EmbeddingError(
                    "sentence-transformers not installed. "
                    "Install with: pip install xpressai[local]"
                )
            self._model = SentenceTransformer(self.model_name)
            self.dim = self._model.get_sentence_embedding_dimension()
        return self._model

    def embed(self, text: str) -> list[float]:
        """Generate embedding for text (synchronous).

        Args:
            text: Text to embed

        Returns:
            Embedding vector as list of floats
        """
        model = self._get_model()
        embedding = model.encode(text, convert_to_numpy=True)
        return embedding.tolist()

    def embed_batch(self, texts: list[str]) -> list[list[float]]:
        """Generate embeddings for multiple texts (synchronous).

        Args:
            texts: List of texts to embed

        Returns:
            List of embedding vectors
        """
        model = self._get_model()
        embeddings = model.encode(texts, convert_to_numpy=True)
        return [e.tolist() for e in embeddings]

    async def embed_async(self, text: str) -> list[float]:
        """Generate embedding for text (non-blocking).

        Runs the CPU-bound encoding in a thread pool to avoid blocking
        the async event loop.

        Args:
            text: Text to embed

        Returns:
            Embedding vector as list of floats
        """
        loop = asyncio.get_event_loop()
        return await loop.run_in_executor(_get_executor(), self.embed, text)

    async def embed_batch_async(self, texts: list[str]) -> list[list[float]]:
        """Generate embeddings for multiple texts (non-blocking).

        Args:
            texts: List of texts to embed

        Returns:
            List of embedding vectors
        """
        loop = asyncio.get_event_loop()
        return await loop.run_in_executor(_get_executor(), self.embed_batch, texts)


class VectorStore:
    """Vector storage using sqlite-vec.

    Stores embeddings alongside memory IDs and supports similarity search.
    """

    def __init__(self, db: Database, embedding_dim: int = 384):
        """Initialize vector store.

        Args:
            db: Database instance
            embedding_dim: Dimension of embeddings
        """
        self.db = db
        self.embedding_dim = embedding_dim
        self._embedding_model = None

    @property
    def embedding_model(self) -> EmbeddingModel:
        """Get or create embedding model."""
        if self._embedding_model is None:
            self._embedding_model = EmbeddingModel()
        return self._embedding_model

    @property
    def available(self) -> bool:
        """Check if vector operations are available."""
        return SQLITE_VEC_AVAILABLE and EMBEDDINGS_AVAILABLE

    def _ensure_table_exists(self, conn) -> bool:
        """Ensure the memory_embeddings table exists.

        Args:
            conn: Database connection

        Returns:
            True if table exists or was created, False otherwise
        """
        # Check if table already exists
        table_exists = conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='memory_embeddings'"
        ).fetchone()

        if table_exists:
            return True

        # Try to create it if sqlite-vec is available
        if not SQLITE_VEC_AVAILABLE:
            return False

        try:
            conn.execute("SELECT vec_version()")
            conn.execute(f"""
                CREATE VIRTUAL TABLE IF NOT EXISTS memory_embeddings USING vec0(
                    memory_id TEXT PRIMARY KEY,
                    embedding FLOAT[{self.embedding_dim}]
                )
            """)
            logger.info("Created memory_embeddings table")
            return True
        except Exception as e:
            logger.debug(f"Could not create vector table: {e}")
            return False

    async def add(self, memory_id: str, text: str) -> None:
        """Add embedding for a memory.

        Args:
            memory_id: ID of the memory
            text: Text to generate embedding from
        """
        if not self.available:
            logger.debug("Vector storage not available, skipping embedding")
            return

        try:
            # Use async embed to avoid blocking the event loop
            embedding = await self.embedding_model.embed_async(text)

            with self.db.connect() as conn:
                # Ensure table exists (creates if sqlite-vec is available)
                if not self._ensure_table_exists(conn):
                    logger.debug("Vector table not available, skipping embedding")
                    return

                # Convert to bytes for sqlite-vec
                embedding_bytes = np.array(embedding, dtype=np.float32).tobytes()

                # sqlite-vec virtual tables don't support INSERT OR REPLACE
                # Delete existing first, then insert
                conn.execute(
                    "DELETE FROM memory_embeddings WHERE memory_id = ?",
                    (memory_id,),
                )
                conn.execute(
                    """
                    INSERT INTO memory_embeddings (memory_id, embedding)
                    VALUES (?, ?)
                """,
                    (memory_id, embedding_bytes),
                )

        except Exception as e:
            logger.warning(f"Failed to add embedding: {e}")

    async def add_batch(self, items: list[tuple[str, str]]) -> None:
        """Add embeddings for multiple memories.

        Args:
            items: List of (memory_id, text) tuples
        """
        if not self.available:
            return

        try:
            texts = [text for _, text in items]
            # Use async embed to avoid blocking the event loop
            embeddings = await self.embedding_model.embed_batch_async(texts)

            with self.db.connect() as conn:
                for (memory_id, _), embedding in zip(items, embeddings):
                    embedding_bytes = np.array(embedding, dtype=np.float32).tobytes()
                    # sqlite-vec virtual tables don't support INSERT OR REPLACE
                    conn.execute(
                        "DELETE FROM memory_embeddings WHERE memory_id = ?",
                        (memory_id,),
                    )
                    conn.execute(
                        """
                        INSERT INTO memory_embeddings (memory_id, embedding)
                        VALUES (?, ?)
                    """,
                        (memory_id, embedding_bytes),
                    )

        except Exception as e:
            logger.warning(f"Failed to add embeddings: {e}")

    async def delete(self, memory_id: str) -> None:
        """Delete embedding for a memory.

        Args:
            memory_id: ID of the memory
        """
        if not self.available:
            return

        with self.db.connect() as conn:
            conn.execute("DELETE FROM memory_embeddings WHERE memory_id = ?", (memory_id,))

    async def search(
        self, query: str, limit: int = 10, threshold: float = 0.0
    ) -> list[SearchResult]:
        """Search for similar memories.

        Args:
            query: Search query text
            limit: Maximum number of results
            threshold: Minimum similarity score (0-1)

        Returns:
            List of search results sorted by similarity
        """
        if not self.available:
            logger.debug("Vector search not available")
            return []

        try:
            # Use async embed to avoid blocking the event loop
            query_embedding = await self.embedding_model.embed_async(query)
            query_bytes = np.array(query_embedding, dtype=np.float32).tobytes()

            with self.db.connect() as conn:
                # Use sqlite-vec for similarity search
                rows = conn.execute(
                    """
                    SELECT 
                        memory_id,
                        vec_distance_cosine(embedding, ?) as distance
                    FROM memory_embeddings
                    ORDER BY distance ASC
                    LIMIT ?
                """,
                    (query_bytes, limit),
                ).fetchall()

                results = []
                for row in rows:
                    # Convert cosine distance to similarity score
                    distance = row["distance"]
                    score = 1.0 - distance  # Higher is more similar

                    if score >= threshold:
                        results.append(
                            SearchResult(
                                memory_id=row["memory_id"],
                                score=score,
                                distance=distance,
                            )
                        )

                return results

        except Exception as e:
            logger.warning(f"Vector search failed: {e}")
            return []

    async def find_similar(self, memory_id: str, limit: int = 5) -> list[SearchResult]:
        """Find memories similar to a given memory.

        Args:
            memory_id: ID of the source memory
            limit: Maximum number of results

        Returns:
            List of similar memories
        """
        if not self.available:
            return []

        try:
            with self.db.connect() as conn:
                # Get the embedding for the source memory
                row = conn.execute(
                    "SELECT embedding FROM memory_embeddings WHERE memory_id = ?", (memory_id,)
                ).fetchone()

                if not row:
                    return []

                embedding_bytes = row["embedding"]

                # Find similar memories
                rows = conn.execute(
                    """
                    SELECT 
                        memory_id,
                        vec_distance_cosine(embedding, ?) as distance
                    FROM memory_embeddings
                    WHERE memory_id != ?
                    ORDER BY distance ASC
                    LIMIT ?
                """,
                    (embedding_bytes, memory_id, limit),
                ).fetchall()

                return [
                    SearchResult(
                        memory_id=row["memory_id"],
                        score=1.0 - row["distance"],
                        distance=row["distance"],
                    )
                    for row in rows
                ]

        except Exception as e:
            logger.warning(f"Similar search failed: {e}")
            return []

    async def get_stats(self) -> dict[str, Any]:
        """Get statistics about the vector store.

        Returns:
            Dictionary with stats
        """
        if not self.available:
            return {"available": False, "total_embeddings": 0}

        try:
            with self.db.connect() as conn:
                # Check if table exists first
                table_exists = conn.execute(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name='memory_embeddings'"
                ).fetchone()

                if not table_exists:
                    return {
                        "available": True,
                        "total_embeddings": 0,
                        "embedding_dim": self.embedding_dim,
                        "model": self.embedding_model.model_name if self._embedding_model else "not loaded",
                        "note": "table not yet created",
                    }

                count = conn.execute("SELECT COUNT(*) FROM memory_embeddings").fetchone()[0]

                return {
                    "available": True,
                    "total_embeddings": count,
                    "embedding_dim": self.embedding_dim,
                    "model": self.embedding_model.model_name if self._embedding_model else "not loaded",
                }
        except Exception as e:
            logger.warning(f"Failed to get vector stats: {e}")
            return {"available": False, "total_embeddings": 0, "error": str(e)}
