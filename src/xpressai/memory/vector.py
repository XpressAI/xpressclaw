"""Vector storage and semantic search using sqlite-vec.

Provides embedding-based similarity search for memories.
"""

from dataclasses import dataclass
from typing import Any
import logging
import numpy as np

from xpressai.memory.database import Database, SQLITE_VEC_AVAILABLE
from xpressai.core.exceptions import EmbeddingError

logger = logging.getLogger(__name__)

# Try to load sentence-transformers for embeddings
try:
    from sentence_transformers import SentenceTransformer

    EMBEDDINGS_AVAILABLE = True
except ImportError:
    EMBEDDINGS_AVAILABLE = False
    logger.warning("sentence-transformers not available, embedding search disabled")


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
            if not EMBEDDINGS_AVAILABLE:
                raise EmbeddingError(
                    "sentence-transformers not installed. "
                    "Install with: pip install sentence-transformers"
                )
            self._model = SentenceTransformer(self.model_name)
            self.dim = self._model.get_sentence_embedding_dimension()
        return self._model

    def embed(self, text: str) -> list[float]:
        """Generate embedding for text.

        Args:
            text: Text to embed

        Returns:
            Embedding vector as list of floats
        """
        model = self._get_model()
        embedding = model.encode(text, convert_to_numpy=True)
        return embedding.tolist()

    def embed_batch(self, texts: list[str]) -> list[list[float]]:
        """Generate embeddings for multiple texts.

        Args:
            texts: List of texts to embed

        Returns:
            List of embedding vectors
        """
        model = self._get_model()
        embeddings = model.encode(texts, convert_to_numpy=True)
        return [e.tolist() for e in embeddings]


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
            embedding = self.embedding_model.embed(text)

            with self.db.connect() as conn:
                # Convert to bytes for sqlite-vec
                embedding_bytes = np.array(embedding, dtype=np.float32).tobytes()

                conn.execute(
                    """
                    INSERT OR REPLACE INTO memory_embeddings (memory_id, embedding)
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
            embeddings = self.embedding_model.embed_batch(texts)

            with self.db.connect() as conn:
                for (memory_id, _), embedding in zip(items, embeddings):
                    embedding_bytes = np.array(embedding, dtype=np.float32).tobytes()
                    conn.execute(
                        """
                        INSERT OR REPLACE INTO memory_embeddings (memory_id, embedding)
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
            query_embedding = self.embedding_model.embed(query)
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
            return {"available": False}

        with self.db.connect() as conn:
            count = conn.execute("SELECT COUNT(*) FROM memory_embeddings").fetchone()[0]

            return {
                "available": True,
                "total_embeddings": count,
                "embedding_dim": self.embedding_dim,
                "model": self.embedding_model.model_name if self._embedding_model else "not loaded",
            }
