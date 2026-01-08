"""Dynamic context management for infinite conversation history.

Implements relevance-based message selection to maximize context utilization
while staying within model token limits. See ADR-014 for design details.
"""

import asyncio
import logging
import struct
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from xpressai.memory.database import Database
    from xpressai.memory.vector import VectorStore

logger = logging.getLogger(__name__)

# Model context limits (tokens)
MODEL_CONTEXT_LIMITS: dict[str, int] = {
    # Claude models
    "claude-sonnet-4-20250514": 200000,
    "claude-opus-4-0-20250514": 200000,
    "claude-3-5-sonnet-20241022": 200000,
    "claude-3-opus-20240229": 200000,
    "claude-3-sonnet-20240229": 200000,
    "claude-3-haiku-20240307": 200000,
    # OpenAI models
    "gpt-4o": 128000,
    "gpt-4-turbo": 128000,
    "gpt-4": 8192,
    "gpt-3.5-turbo": 16385,
    # Local models
    "qwen3-8b": 32768,
    "qwen2.5:7b": 32768,
    "llama3.1:8b": 32768,
}

# Default context limit when model not found
DEFAULT_CONTEXT_LIMIT = 32768


@dataclass
class Message:
    """A message with token count and optional embedding."""

    id: int
    role: str
    content: str
    token_count: int
    embedding: bytes | None = None
    timestamp: str | None = None

    @property
    def embedding_vector(self) -> list[float] | None:
        """Decode embedding bytes to float vector."""
        if self.embedding is None:
            return None
        # Decode as array of 32-bit floats
        count = len(self.embedding) // 4
        return list(struct.unpack(f"{count}f", self.embedding))


@dataclass
class ScoredMessage:
    """A message with its relevance score."""

    message: Message
    score: float


class TokenCounter:
    """Fast token counting with caching.

    Uses tiktoken for accurate counts. Falls back to character-based
    estimation if tiktoken is not available.
    """

    def __init__(self):
        self._encoder = None
        self._initialized = False

    def _init_encoder(self) -> None:
        """Lazily initialize the tokenizer."""
        if self._initialized:
            return

        try:
            import tiktoken

            # cl100k_base works for both GPT-4 and Claude (approximate)
            self._encoder = tiktoken.get_encoding("cl100k_base")
            logger.debug("Using tiktoken for token counting")
        except ImportError:
            logger.info("tiktoken not available, using character-based estimation")
            self._encoder = None

        self._initialized = True

    def count(self, text: str) -> int:
        """Count tokens in text.

        Args:
            text: Text to count tokens for

        Returns:
            Token count (approximate if tiktoken not available)
        """
        self._init_encoder()

        if self._encoder is not None:
            return len(self._encoder.encode(text))

        # Fallback: ~4 characters per token (conservative estimate)
        return len(text) // 4 + 1


# Global token counter instance
_token_counter = TokenCounter()


def count_tokens(text: str) -> int:
    """Count tokens in text using the global counter."""
    return _token_counter.count(text)


class ContextManager:
    """Manages conversation context to maximize token utilization.

    Uses a threshold-based algorithm:
    1. Recent messages (50% of budget) are always included
    2. Older messages are scored by relevance to recent context
    3. Start with high threshold, include messages above it
    4. Lower threshold until target utilization (90%) is reached
    5. Messages below final threshold are elided with [...]
    """

    def __init__(
        self,
        max_context_tokens: int = DEFAULT_CONTEXT_LIMIT,
        target_utilization: float = 0.90,
        recent_window_ratio: float = 0.50,
        min_threshold: float = 0.3,
        vector_store: "VectorStore | None" = None,
    ):
        """Initialize context manager.

        Args:
            max_context_tokens: Maximum tokens allowed in context
            target_utilization: Target fraction of context to use (default 0.90)
            recent_window_ratio: Fraction of target for recent messages (default 0.50)
            min_threshold: Minimum relevance score to include a message (default 0.3)
            vector_store: VectorStore for computing embeddings
        """
        self.max_context_tokens = max_context_tokens
        self.target_utilization = target_utilization
        self.recent_window_ratio = recent_window_ratio
        self.min_threshold = min_threshold
        self._vector_store = vector_store

    @classmethod
    def for_model(
        cls,
        model: str,
        target_utilization: float = 0.90,
        recent_window_ratio: float = 0.50,
        min_threshold: float = 0.3,
        vector_store: "VectorStore | None" = None,
    ) -> "ContextManager":
        """Create a ContextManager configured for a specific model.

        Args:
            model: Model identifier (e.g., "claude-sonnet-4-20250514")
            target_utilization: Target fraction of context to use
            recent_window_ratio: Fraction of target for recent messages
            min_threshold: Minimum relevance score to include
            vector_store: VectorStore for computing embeddings

        Returns:
            Configured ContextManager
        """
        # Look up context limit, trying partial matches
        context_limit = DEFAULT_CONTEXT_LIMIT
        for model_name, limit in MODEL_CONTEXT_LIMITS.items():
            if model_name in model or model in model_name:
                context_limit = limit
                break

        return cls(
            max_context_tokens=context_limit,
            target_utilization=target_utilization,
            recent_window_ratio=recent_window_ratio,
            min_threshold=min_threshold,
            vector_store=vector_store,
        )

    def assemble_context(
        self,
        messages: list[Message],
        context_embedding: list[float] | None = None,
    ) -> tuple[list[dict[str, str]], int]:
        """Assemble context from messages, maximizing relevance within token budget.

        Args:
            messages: All messages in chronological order
            context_embedding: Embedding of recent context for relevance scoring
                              (if None, will compute from recent messages)

        Returns:
            Tuple of (assembled messages as dicts, total tokens used)
            Messages below threshold have content replaced with "[...]"
        """
        if not messages:
            return [], 0

        target_tokens = int(self.max_context_tokens * self.target_utilization)
        recent_budget = int(target_tokens * self.recent_window_ratio)

        # 1. Take recent messages up to budget
        recent_messages, recent_tokens = self._take_recent(messages, recent_budget)

        if len(recent_messages) >= len(messages):
            # All messages fit in recent window
            return self._format_messages(messages), sum(m.token_count for m in messages)

        # 2. Get older messages
        older_count = len(messages) - len(recent_messages)
        older_messages = messages[:older_count]

        # 3. If no context embedding provided, compute from recent messages
        if context_embedding is None and self._vector_store is not None:
            # Combine recent messages for context
            recent_text = " ".join(m.content for m in recent_messages[-5:])
            try:
                context_embedding = self._vector_store.embed_sync(recent_text)
            except Exception as e:
                logger.warning(f"Failed to compute context embedding: {e}")
                context_embedding = None

        # 4. Score and select older messages
        remaining_budget = target_tokens - recent_tokens

        if context_embedding is not None:
            selected_older = self._select_by_threshold(
                older_messages, context_embedding, remaining_budget
            )
        else:
            # No embeddings available - fall back to taking most recent that fit
            selected_older = self._select_by_recency(older_messages, remaining_budget)

        # 5. Assemble with elision markers
        return self._assemble_with_elision(
            older_messages, selected_older, recent_messages
        )

    def _take_recent(
        self, messages: list[Message], budget: int
    ) -> tuple[list[Message], int]:
        """Take messages from the end up to token budget.

        Args:
            messages: All messages
            budget: Token budget

        Returns:
            Tuple of (selected messages, total tokens)
        """
        selected = []
        total_tokens = 0

        for msg in reversed(messages):
            if total_tokens + msg.token_count > budget:
                break
            selected.insert(0, msg)
            total_tokens += msg.token_count

        return selected, total_tokens

    def _select_by_threshold(
        self,
        messages: list[Message],
        context_embedding: list[float],
        budget: int,
    ) -> set[int]:
        """Select messages using relevance threshold.

        Args:
            messages: Older messages to select from
            context_embedding: Embedding of recent context
            budget: Token budget for older messages

        Returns:
            Set of message IDs that are selected
        """
        if not messages:
            return set()

        # Score all messages
        scored: list[ScoredMessage] = []
        for msg in messages:
            score = self._compute_relevance(msg, context_embedding)
            scored.append(ScoredMessage(message=msg, score=score))

        # Sort by score descending
        scored.sort(key=lambda x: x.score, reverse=True)

        # Take messages until budget exhausted or below threshold
        selected_ids: set[int] = set()
        used_tokens = 0

        for sm in scored:
            if sm.score < self.min_threshold:
                break
            if used_tokens + sm.message.token_count > budget:
                # Try to fit smaller messages
                continue
            selected_ids.add(sm.message.id)
            used_tokens += sm.message.token_count

        return selected_ids

    def _select_by_recency(
        self, messages: list[Message], budget: int
    ) -> set[int]:
        """Fallback: select by recency when embeddings unavailable.

        Args:
            messages: Older messages
            budget: Token budget

        Returns:
            Set of message IDs selected (most recent that fit)
        """
        selected_ids: set[int] = set()
        used_tokens = 0

        # Take from the end (most recent of the older messages)
        for msg in reversed(messages):
            if used_tokens + msg.token_count > budget:
                break
            selected_ids.add(msg.id)
            used_tokens += msg.token_count

        return selected_ids

    def _compute_relevance(
        self, message: Message, context_embedding: list[float]
    ) -> float:
        """Compute relevance score for a message.

        Args:
            message: Message to score
            context_embedding: Embedding of recent context

        Returns:
            Relevance score between 0 and 1
        """
        msg_embedding = message.embedding_vector

        if msg_embedding is None:
            # No embedding available - use moderate default score
            return 0.5

        # Cosine similarity
        return self._cosine_similarity(msg_embedding, context_embedding)

    def _cosine_similarity(self, a: list[float], b: list[float]) -> float:
        """Compute cosine similarity between two vectors.

        Args:
            a: First vector
            b: Second vector

        Returns:
            Similarity score between -1 and 1
        """
        if len(a) != len(b):
            return 0.0

        dot_product = sum(x * y for x, y in zip(a, b))
        norm_a = sum(x * x for x in a) ** 0.5
        norm_b = sum(x * x for x in b) ** 0.5

        if norm_a == 0 or norm_b == 0:
            return 0.0

        return dot_product / (norm_a * norm_b)

    def _assemble_with_elision(
        self,
        all_older: list[Message],
        selected_ids: set[int],
        recent: list[Message],
    ) -> tuple[list[dict[str, str]], int]:
        """Assemble messages with elision markers for unselected.

        Args:
            all_older: All older messages in order
            selected_ids: IDs of selected older messages
            recent: Recent messages (always included)

        Returns:
            Tuple of (formatted messages, total tokens)
        """
        result: list[dict[str, str]] = []
        total_tokens = 0
        elision_token_cost = 5  # Approximate tokens for "[...]"
        consecutive_elided = 0

        for msg in all_older:
            if msg.id in selected_ids:
                # Flush any pending elision marker
                if consecutive_elided > 0:
                    result.append({"role": "system", "content": "[...]"})
                    total_tokens += elision_token_cost
                    consecutive_elided = 0

                result.append({"role": msg.role, "content": msg.content})
                total_tokens += msg.token_count
            else:
                consecutive_elided += 1

        # Flush final elision marker if needed
        if consecutive_elided > 0:
            result.append({"role": "system", "content": "[...]"})
            total_tokens += elision_token_cost

        # Add recent messages
        for msg in recent:
            result.append({"role": msg.role, "content": msg.content})
            total_tokens += msg.token_count

        return result, total_tokens

    def _format_messages(self, messages: list[Message]) -> list[dict[str, str]]:
        """Format messages as dicts."""
        return [{"role": m.role, "content": m.content} for m in messages]


async def compute_message_embedding(
    text: str, vector_store: "VectorStore"
) -> bytes | None:
    """Compute and encode embedding for a message.

    Args:
        text: Message text
        vector_store: VectorStore for embedding

    Returns:
        Embedding as bytes (for storage), or None on failure
    """
    try:
        embedding = await vector_store.embed(text)
        # Encode as array of 32-bit floats
        return struct.pack(f"{len(embedding)}f", *embedding)
    except Exception as e:
        logger.warning(f"Failed to compute message embedding: {e}")
        return None


def encode_embedding(embedding: list[float]) -> bytes:
    """Encode float vector as bytes for storage."""
    return struct.pack(f"{len(embedding)}f", *embedding)


def decode_embedding(data: bytes) -> list[float]:
    """Decode bytes to float vector."""
    count = len(data) // 4
    return list(struct.unpack(f"{count}f", data))


class MessageEmbedder:
    """Handles async background embedding computation for chat messages.

    Computes embeddings in the background without blocking the main request,
    then stores them in the database for later use by ContextManager.
    """

    def __init__(self, db: "Database", vector_store: "VectorStore"):
        """Initialize the embedder.

        Args:
            db: Database instance for storing embeddings
            vector_store: VectorStore for computing embeddings
        """
        self._db = db
        self._vector_store = vector_store
        self._pending_tasks: set[asyncio.Task] = set()

    @property
    def available(self) -> bool:
        """Check if embedding computation is available.

        Only requires sentence-transformers, not sqlite-vec.
        """
        # Import from vector module to check if embeddings available
        from xpressai.memory.vector import EMBEDDINGS_AVAILABLE
        return EMBEDDINGS_AVAILABLE

    async def compute_and_store(self, message_id: int, content: str) -> None:
        """Compute embedding for a message and store it in the database.

        Args:
            message_id: Database ID of the message
            content: Message content to embed
        """
        if not self.available:
            logger.debug("Embeddings not available, skipping message embedding")
            return

        try:
            # Compute embedding asynchronously
            embedding = await self._vector_store.embedding_model.embed_async(content)

            # Encode as bytes for storage
            embedding_bytes = encode_embedding(embedding)

            # Store in database
            with self._db.connect() as conn:
                conn.execute(
                    "UPDATE agent_chat_messages SET embedding = ? WHERE id = ?",
                    (embedding_bytes, message_id),
                )

            logger.debug(f"Computed and stored embedding for message {message_id}")

        except Exception as e:
            logger.warning(f"Failed to compute embedding for message {message_id}: {e}")

    def schedule_embedding(self, message_id: int, content: str) -> None:
        """Schedule embedding computation as a background task.

        This method returns immediately and computes the embedding
        in the background without blocking the caller.

        Args:
            message_id: Database ID of the message
            content: Message content to embed
        """
        if not self.available:
            return

        # Create background task
        task = asyncio.create_task(
            self._compute_with_cleanup(message_id, content),
            name=f"embed-msg-{message_id}",
        )

        # Track the task to prevent garbage collection
        self._pending_tasks.add(task)
        task.add_done_callback(self._pending_tasks.discard)

    async def _compute_with_cleanup(self, message_id: int, content: str) -> None:
        """Compute embedding with error handling for background execution."""
        try:
            await self.compute_and_store(message_id, content)
        except Exception as e:
            # Log but don't raise - this is a background task
            logger.error(f"Background embedding failed for message {message_id}: {e}")

    async def backfill_missing_embeddings(
        self,
        agent_id: str,
        conversation_id: str | None = None,
        batch_size: int = 10,
    ) -> int:
        """Backfill embeddings for messages that don't have them.

        Useful for migrating existing conversations or recovering from failures.

        Args:
            agent_id: Agent ID to backfill for
            conversation_id: Optional conversation ID to limit scope
            batch_size: Number of messages to process in each batch

        Returns:
            Number of embeddings computed
        """
        if not self.available:
            return 0

        computed = 0

        try:
            with self._db.connect() as conn:
                # Find messages without embeddings
                if conversation_id:
                    rows = conn.execute(
                        """SELECT id, content FROM agent_chat_messages
                           WHERE agent_id = ? AND conversation_id = ?
                           AND embedding IS NULL AND role IN ('user', 'agent')
                           ORDER BY timestamp
                           LIMIT ?""",
                        (agent_id, conversation_id, batch_size),
                    ).fetchall()
                else:
                    rows = conn.execute(
                        """SELECT id, content FROM agent_chat_messages
                           WHERE agent_id = ? AND embedding IS NULL
                           AND role IN ('user', 'agent')
                           ORDER BY timestamp
                           LIMIT ?""",
                        (agent_id, batch_size),
                    ).fetchall()

            # Compute embeddings for batch
            for row in rows:
                await self.compute_and_store(row["id"], row["content"])
                computed += 1

            if computed > 0:
                logger.info(f"Backfilled {computed} message embeddings for agent {agent_id}")

        except Exception as e:
            logger.error(f"Backfill failed: {e}")

        return computed

    async def wait_pending(self, timeout: float = 5.0) -> None:
        """Wait for pending background tasks to complete.

        Useful for graceful shutdown or testing.

        Args:
            timeout: Maximum time to wait in seconds
        """
        if self._pending_tasks:
            try:
                await asyncio.wait(self._pending_tasks, timeout=timeout)
            except Exception:
                pass


# Global embedder instance (set by runtime)
_message_embedder: MessageEmbedder | None = None


def get_message_embedder() -> MessageEmbedder | None:
    """Get the global message embedder instance."""
    return _message_embedder


def set_message_embedder(embedder: MessageEmbedder | None) -> None:
    """Set the global message embedder instance."""
    global _message_embedder
    _message_embedder = embedder
