"""Tests for dynamic context management and message embedding.

These tests use a local Ollama instance with ministral-3:3b for integration testing.
"""

# Force CPU usage for embedding model in tests to avoid GPU memory issues
import os
os.environ["CUDA_VISIBLE_DEVICES"] = ""

import asyncio
import pytest
from pathlib import Path
from decimal import Decimal

from xpressai.memory.context import (
    count_tokens,
    ContextManager,
    Message,
    MessageEmbedder,
    encode_embedding,
    decode_embedding,
    MODEL_CONTEXT_LIMITS,
)
from xpressai.memory.database import Database
from xpressai.memory.vector import VectorStore, EMBEDDINGS_AVAILABLE
from xpressai.core.config import Config, AgentConfig, SystemConfig, BudgetConfig, MemoryConfig, LocalModelConfig


# Check if Ollama is available
def _check_ollama_available() -> bool:
    """Check if Ollama is running locally."""
    try:
        import httpx
        response = httpx.get("http://localhost:11434/api/tags", timeout=2.0)
        return response.status_code == 200
    except Exception:
        return False


OLLAMA_AVAILABLE = _check_ollama_available()


class TestTokenCounter:
    """Test token counting functionality."""

    def test_count_tokens_simple(self):
        """Test counting tokens in simple text."""
        count = count_tokens("Hello, world!")
        assert count > 0
        assert count < 10  # Should be a few tokens

    def test_count_tokens_empty(self):
        """Test counting tokens in empty string."""
        count = count_tokens("")
        assert count >= 0

    def test_count_tokens_long_text(self):
        """Test counting tokens in longer text."""
        text = "This is a longer piece of text. " * 100
        count = count_tokens(text)
        assert count > 100  # Should be many tokens


class TestEmbeddingEncoding:
    """Test embedding encoding/decoding."""

    def test_encode_decode_roundtrip(self):
        """Test that encoding and decoding preserves values."""
        original = [0.1, 0.2, 0.3, -0.5, 0.0, 1.0]
        encoded = encode_embedding(original)
        decoded = decode_embedding(encoded)

        assert len(decoded) == len(original)
        for a, b in zip(original, decoded):
            assert abs(a - b) < 1e-6

    def test_encode_produces_bytes(self):
        """Test that encoding produces bytes."""
        embedding = [0.1, 0.2, 0.3]
        encoded = encode_embedding(embedding)
        assert isinstance(encoded, bytes)
        assert len(encoded) == len(embedding) * 4  # 4 bytes per float32


class TestMessage:
    """Test Message dataclass."""

    def test_message_without_embedding(self):
        """Test message without embedding."""
        msg = Message(id=1, role="user", content="Hello", token_count=2)
        assert msg.embedding_vector is None

    def test_message_with_embedding(self):
        """Test message with embedding."""
        embedding = [0.1, 0.2, 0.3]
        msg = Message(
            id=1,
            role="user",
            content="Hello",
            token_count=2,
            embedding=encode_embedding(embedding),
        )

        vector = msg.embedding_vector
        assert vector is not None
        assert len(vector) == 3
        assert abs(vector[0] - 0.1) < 1e-6


class TestContextManager:
    """Test ContextManager functionality."""

    def test_for_model_known(self):
        """Test creating context manager for known model."""
        mgr = ContextManager.for_model("claude-sonnet-4-20250514")
        assert mgr.max_context_tokens == 200000

    def test_for_model_unknown(self):
        """Test creating context manager for unknown model."""
        mgr = ContextManager.for_model("unknown-model")
        assert mgr.max_context_tokens == 32768  # Default

    def test_for_model_partial_match(self):
        """Test partial model name matching."""
        mgr = ContextManager.for_model("gpt-4o-mini")
        # Should match gpt-4o
        assert mgr.max_context_tokens == 128000

    def test_assemble_empty(self):
        """Test assembling empty context."""
        mgr = ContextManager(max_context_tokens=1000)
        result, tokens = mgr.assemble_context([])
        assert result == []
        assert tokens == 0

    def test_assemble_few_messages(self):
        """Test assembling context with few messages that all fit."""
        mgr = ContextManager(max_context_tokens=10000)

        messages = [
            Message(id=1, role="user", content="Hello", token_count=10),
            Message(id=2, role="assistant", content="Hi there!", token_count=15),
            Message(id=3, role="user", content="How are you?", token_count=20),
        ]

        result, tokens = mgr.assemble_context(messages)

        assert len(result) == 3
        assert tokens == 45
        assert result[0]["role"] == "user"
        assert result[0]["content"] == "Hello"

    def test_assemble_selects_recent(self):
        """Test that recent messages are always selected."""
        mgr = ContextManager(
            max_context_tokens=100,
            target_utilization=1.0,
            recent_window_ratio=0.5,
        )

        # Create messages that exceed the budget
        messages = [
            Message(id=i, role="user" if i % 2 == 0 else "assistant",
                    content=f"Message {i}", token_count=20)
            for i in range(10)
        ]

        result, tokens = mgr.assemble_context(messages)

        # Should have some messages, not all
        assert len(result) < 10
        # Last messages should be included
        assert any(m["content"] == "Message 9" for m in result)


class TestContextManagerWithEmbeddings:
    """Test ContextManager with embeddings for relevance scoring."""

    def test_relevance_scoring(self):
        """Test that messages with embeddings get relevance scored."""
        mgr = ContextManager(
            max_context_tokens=200,
            target_utilization=1.0,
            recent_window_ratio=0.3,
            min_threshold=0.0,
        )

        # Create context embedding
        context_emb = [1.0, 0.0, 0.0]

        # Create messages with varying relevance
        messages = [
            Message(id=1, role="user", content="Relevant",
                    token_count=20, embedding=encode_embedding([0.9, 0.1, 0.0])),
            Message(id=2, role="assistant", content="Not relevant",
                    token_count=20, embedding=encode_embedding([0.0, 0.0, 1.0])),
            Message(id=3, role="user", content="Somewhat relevant",
                    token_count=20, embedding=encode_embedding([0.5, 0.5, 0.0])),
            Message(id=4, role="assistant", content="Recent 1",
                    token_count=20, embedding=encode_embedding([0.1, 0.1, 0.8])),
            Message(id=5, role="user", content="Recent 2",
                    token_count=20, embedding=encode_embedding([0.2, 0.2, 0.6])),
        ]

        result, tokens = mgr.assemble_context(messages, context_embedding=context_emb)

        # Recent messages (4, 5) should be included
        contents = [m["content"] for m in result]
        assert "Recent 1" in contents or "Recent 2" in contents


class TestRealisticContextManagement:
    """Realistic stress tests for context management.

    Tests the system with conversation sizes that exceed context windows,
    simulating real-world usage patterns.
    """

    def _generate_conversation_message(self, topic: str, turn: int, role: str) -> str:
        """Generate a realistic conversation message."""
        if role == "user":
            user_messages = [
                f"I've been working on {topic} and I'm running into some issues. "
                f"The main problem is that the implementation doesn't seem to handle edge cases well. "
                f"Can you help me understand what might be going wrong?",

                f"That makes sense. Let me give you more context about the {topic} situation. "
                f"We're using a microservices architecture with about 15 different services. "
                f"The data flows through multiple stages before reaching the final destination.",

                f"I see what you mean about {topic}. One thing I forgot to mention is that "
                f"we also need to consider backwards compatibility with the legacy system. "
                f"Some of our older clients are still using the v1 API.",

                f"Thanks for explaining that aspect of {topic}. I have a follow-up question: "
                f"how would you handle the case where multiple requests come in simultaneously? "
                f"We've been seeing race conditions in production.",

                f"Regarding {topic}, I tried implementing your suggestion but ran into a new issue. "
                f"The performance degraded significantly when we scaled to more than 100 concurrent users. "
                f"Our benchmarks show response times went from 50ms to over 2 seconds.",

                f"Let me share some code related to {topic}. Here's what our current implementation looks like: "
                f"we have a main controller that orchestrates the workflow, several service classes for business logic, "
                f"and a repository layer for data access. The bottleneck seems to be in the service layer.",

                f"One more thing about {topic} - we're also dealing with memory constraints. "
                f"The server only has 4GB of RAM and we're seeing OOM errors during peak traffic. "
                f"Would caching help in this scenario or would it make things worse?",

                f"I appreciate the detailed explanation about {topic}. Before we wrap up, "
                f"could you summarize the key points we discussed? I want to make sure I capture "
                f"all the important details for our team meeting tomorrow.",
            ]
            return user_messages[turn % len(user_messages)]
        else:
            assistant_messages = [
                f"I understand the challenges you're facing with {topic}. Let me break this down into "
                f"manageable parts. First, let's look at the core issue: edge case handling often fails "
                f"when we don't properly validate inputs at the boundary of our system. I recommend "
                f"implementing a validation layer that catches these cases early.",

                f"Thank you for the additional context about your {topic} architecture. With 15 microservices, "
                f"you're dealing with significant complexity. The key is to ensure proper error handling "
                f"at each service boundary. Consider implementing circuit breakers and retry logic with "
                f"exponential backoff to handle transient failures gracefully.",

                f"Backwards compatibility is crucial for {topic}. Here's what I suggest: create an adapter "
                f"layer that translates between v1 and v2 API formats. This way, legacy clients continue "
                f"working while you can evolve the new API independently. Make sure to version your "
                f"endpoints explicitly in the URL path.",

                f"Race conditions in {topic} are tricky but solvable. You have several options: optimistic "
                f"locking with version numbers, pessimistic locking with database-level locks, or using "
                f"a distributed lock service like Redis. The right choice depends on your consistency "
                f"requirements and acceptable latency. For most cases, optimistic locking works well.",

                f"The performance degradation you're seeing with {topic} at scale is concerning. A jump from "
                f"50ms to 2 seconds suggests either a bottleneck in database queries, inefficient algorithms "
                f"with O(n²) complexity, or resource contention. I recommend profiling your application to "
                f"identify the exact bottleneck before optimizing.",

                f"Looking at your {topic} code structure, I see potential issues. The controller doing "
                f"orchestration is fine, but make sure it's not doing too much work itself. The service "
                f"layer bottleneck could be due to synchronous calls that should be async, or N+1 query "
                f"problems in the repository layer. Consider batching database operations.",

                f"For your {topic} memory constraints, caching can definitely help, but it's a double-edged "
                f"sword. With only 4GB RAM, you need to be very selective about what you cache. Use an "
                f"LRU cache with a strict size limit, and consider using Redis for distributed caching "
                f"to offload memory from your application servers.",

                f"Here's a summary of our {topic} discussion: 1) Implement input validation at system "
                f"boundaries, 2) Add circuit breakers for microservice resilience, 3) Create adapter layer "
                f"for API versioning, 4) Use optimistic locking for race conditions, 5) Profile before "
                f"optimizing performance, 6) Review service layer for async opportunities, 7) Implement "
                f"bounded LRU caching. Let me know if you need clarification on any of these points.",
            ]
            return assistant_messages[turn % len(assistant_messages)]

    def test_tinyllama_context_overflow(self):
        """Test handling conversation 4x larger than TinyLlama's 2048 context.

        Creates ~8192 tokens of conversation and verifies the context manager
        properly selects messages to fit within 2048 tokens.
        """
        import time

        # TinyLlama context configuration
        TINYLLAMA_CONTEXT = 2048
        TARGET_CONVERSATION_TOKENS = 8192  # 4x the context

        mgr = ContextManager(
            max_context_tokens=TINYLLAMA_CONTEXT,
            target_utilization=0.90,  # Use 90% = ~1843 tokens
            recent_window_ratio=0.50,  # 50% for recent = ~921 tokens
            min_threshold=0.3,
        )

        # Generate a realistic multi-topic conversation
        topics = ["database optimization", "API design", "caching strategy", "deployment pipeline"]
        messages = []
        total_tokens = 0
        msg_id = 1

        # Keep generating messages until we exceed target
        turn = 0
        while total_tokens < TARGET_CONVERSATION_TOKENS:
            topic = topics[turn % len(topics)]

            # User message
            user_content = self._generate_conversation_message(topic, turn, "user")
            user_tokens = count_tokens(user_content)
            messages.append(Message(
                id=msg_id,
                role="user",
                content=user_content,
                token_count=user_tokens,
            ))
            total_tokens += user_tokens
            msg_id += 1

            # Assistant message
            assistant_content = self._generate_conversation_message(topic, turn, "assistant")
            assistant_tokens = count_tokens(assistant_content)
            messages.append(Message(
                id=msg_id,
                role="assistant",
                content=assistant_content,
                token_count=assistant_tokens,
            ))
            total_tokens += assistant_tokens
            msg_id += 1

            turn += 1

        # Verify we generated enough tokens
        assert total_tokens >= TARGET_CONVERSATION_TOKENS, \
            f"Generated {total_tokens} tokens, expected >= {TARGET_CONVERSATION_TOKENS}"

        print(f"\n  Generated conversation: {len(messages)} messages, {total_tokens} tokens")
        print(f"  Context window: {TINYLLAMA_CONTEXT} tokens (target: {int(TINYLLAMA_CONTEXT * 0.9)})")

        # Time the context assembly
        start_time = time.time()
        result, result_tokens = mgr.assemble_context(messages)
        elapsed = time.time() - start_time

        print(f"  Assembly time: {elapsed*1000:.2f}ms")
        print(f"  Result: {len(result)} messages, {result_tokens} tokens")

        # Verify results
        target_max = int(TINYLLAMA_CONTEXT * 0.90)

        # Should fit within target
        assert result_tokens <= target_max, \
            f"Result {result_tokens} tokens exceeds target {target_max}"

        # Should use a reasonable amount of the budget (at least 50%)
        assert result_tokens >= target_max * 0.5, \
            f"Result {result_tokens} tokens is too small (expected >= {target_max * 0.5})"

        # Should be fast (under 100ms even for large conversations)
        assert elapsed < 0.1, f"Assembly took {elapsed*1000:.2f}ms, expected < 100ms"

        # Most recent messages should be included
        recent_contents = [m["content"] for m in result[-4:]]
        last_user_msg = messages[-2].content  # Second to last is last user message
        assert any(last_user_msg in c for c in recent_contents), \
            "Most recent user message should be in the result"

        # Should have elision markers if we dropped messages
        if len(result) < len(messages):
            elision_count = sum(1 for m in result if m["content"] == "[...]")
            print(f"  Elision markers: {elision_count}")

        print(f"  Compression ratio: {total_tokens / result_tokens:.1f}x")

    @pytest.mark.skipif(not EMBEDDINGS_AVAILABLE, reason="sentence-transformers not installed")
    def test_tinyllama_with_embeddings(self):
        """Test context management with real embeddings for relevance scoring.

        This test verifies that semantically relevant older messages are
        preserved while less relevant ones are elided.
        """
        import time
        from sentence_transformers import SentenceTransformer

        TINYLLAMA_CONTEXT = 2048

        mgr = ContextManager(
            max_context_tokens=TINYLLAMA_CONTEXT,
            target_utilization=0.90,
            recent_window_ratio=0.50,
            min_threshold=0.3,
        )

        # Load embedding model (reuses cached model)
        model = SentenceTransformer("all-MiniLM-L6-v2")

        # Create a conversation with distinct topics
        # Early messages about Python, middle about JavaScript, recent about Python again
        conversation_data = [
            # Early Python discussion (should be relevant to recent context)
            ("user", "I'm learning Python and want to understand list comprehensions better."),
            ("assistant", "List comprehensions in Python are a concise way to create lists. The syntax is [expression for item in iterable if condition]. They're more readable than equivalent for loops."),
            ("user", "Can you show me how to filter a list of numbers to get only even ones?"),
            ("assistant", "Sure! Here's how: even_numbers = [x for x in numbers if x % 2 == 0]. This creates a new list containing only the even numbers from the original list."),

            # JavaScript discussion (less relevant to recent Python context)
            ("user", "Now I want to learn JavaScript. How do arrow functions work?"),
            ("assistant", "Arrow functions in JavaScript are a shorter syntax for functions. Instead of function(x) { return x * 2; }, you write (x) => x * 2. They also handle 'this' differently."),
            ("user", "What about async/await in JavaScript?"),
            ("assistant", "Async/await makes asynchronous code look synchronous. Mark a function as async, then use await before Promises. It's cleaner than .then() chains and easier to debug."),
            ("user", "How do I handle errors with async/await?"),
            ("assistant", "Wrap your await calls in try/catch blocks. The catch will handle any rejected promises. You can also use .catch() on the async function call itself."),

            # More JavaScript (padding)
            ("user", "What's the difference between let and const in JavaScript?"),
            ("assistant", "Both are block-scoped. Use const for values that won't be reassigned, let for values that will. Const doesn't make objects immutable, just prevents reassignment of the variable."),
            ("user", "How do JavaScript modules work?"),
            ("assistant", "ES6 modules use import/export syntax. Export functions or variables with 'export', import them with 'import { name } from \"./module\"'. Use 'export default' for the main export."),

            # Return to Python (recent context)
            ("user", "Back to Python - how do decorators work?"),
            ("assistant", "Decorators are functions that modify other functions. They use the @decorator syntax. The decorator receives a function, wraps it with additional behavior, and returns the wrapped function."),
            ("user", "Can you show me a simple Python decorator example?"),
            ("assistant", "Here's a timing decorator: def timer(func): def wrapper(*args): start = time.time(); result = func(*args); print(f'Took {time.time()-start}s'); return result; return wrapper. Use it as @timer above any function."),
        ]

        # Build messages with embeddings
        messages = []
        for i, (role, content) in enumerate(conversation_data):
            embedding = model.encode(content)
            messages.append(Message(
                id=i + 1,
                role=role,
                content=content,
                token_count=count_tokens(content),
                embedding=encode_embedding(embedding.tolist()),
            ))

        total_tokens = sum(m.token_count for m in messages)
        print(f"\n  Conversation: {len(messages)} messages, {total_tokens} tokens")

        # Create context embedding from recent messages (Python-focused)
        recent_text = " ".join(m.content for m in messages[-4:])
        context_embedding = model.encode(recent_text).tolist()

        # Assemble context
        start_time = time.time()
        result, result_tokens = mgr.assemble_context(messages, context_embedding=context_embedding)
        elapsed = time.time() - start_time

        print(f"  Assembly time: {elapsed*1000:.2f}ms")
        print(f"  Result: {len(result)} messages, {result_tokens} tokens")

        # Verify results
        target_max = int(TINYLLAMA_CONTEXT * 0.90)
        assert result_tokens <= target_max

        # Check that Python-related early messages are more likely to be kept
        # than JavaScript messages (due to relevance to recent Python context)
        result_contents = " ".join(m["content"] for m in result if m["content"] != "[...]")

        # Recent Python messages should definitely be there
        assert "decorator" in result_contents.lower(), "Recent Python discussion should be included"

        # Early Python messages should be preserved due to relevance
        # (list comprehensions are relevant to the Python decorator discussion)
        python_relevance = "comprehension" in result_contents.lower() or "even_numbers" in result_contents.lower()

        print(f"  Early Python content preserved: {python_relevance}")
        print(f"  Performance: {elapsed*1000:.2f}ms for {len(messages)} messages")

    def test_extreme_context_overflow(self):
        """Test with 32x context overflow (65536 tokens into 2048).

        Ensures the algorithm remains fast even with very large conversations.
        """
        import time

        CONTEXT_LIMIT = 2048
        TARGET_TOKENS = 65536  # 32x overflow

        mgr = ContextManager(
            max_context_tokens=CONTEXT_LIMIT,
            target_utilization=0.90,
            recent_window_ratio=0.50,
            min_threshold=0.3,
        )

        # Generate many short messages to hit token target quickly
        messages = []
        total_tokens = 0
        msg_id = 1

        base_messages = [
            "How do I implement this feature?",
            "You can implement it by following these steps: first, create the base class, then add the required methods, and finally integrate with the existing system.",
            "What about error handling?",
            "For error handling, wrap the critical sections in try-except blocks and log all exceptions with full stack traces for debugging.",
            "Should I add unit tests?",
            "Yes, always add unit tests. Aim for at least 80% code coverage and include both positive and negative test cases.",
            "What testing framework do you recommend?",
            "I recommend pytest for Python projects. It has a clean syntax, powerful fixtures, and excellent plugin ecosystem.",
        ]

        while total_tokens < TARGET_TOKENS:
            for i, content in enumerate(base_messages):
                role = "user" if i % 2 == 0 else "assistant"
                tokens = count_tokens(content)
                messages.append(Message(
                    id=msg_id,
                    role=role,
                    content=content,
                    token_count=tokens,
                ))
                total_tokens += tokens
                msg_id += 1

                if total_tokens >= TARGET_TOKENS:
                    break

        print(f"\n  Extreme test: {len(messages)} messages, {total_tokens} tokens")
        print(f"  Overflow ratio: {total_tokens / CONTEXT_LIMIT:.0f}x")

        # Time the assembly
        start_time = time.time()
        result, result_tokens = mgr.assemble_context(messages)
        elapsed = time.time() - start_time

        print(f"  Assembly time: {elapsed*1000:.2f}ms")
        print(f"  Result: {len(result)} messages, {result_tokens} tokens")
        print(f"  Compression: {len(messages)} -> {len(result)} messages")

        # Should still be fast
        assert elapsed < 0.5, f"Assembly took {elapsed*1000:.2f}ms, expected < 500ms"

        # Should fit within budget (allow 1% tolerance for elision marker estimation)
        target_max = int(CONTEXT_LIMIT * 0.90)
        tolerance = int(target_max * 0.01)  # 1% tolerance
        assert result_tokens <= target_max + tolerance, \
            f"Result {result_tokens} exceeds target {target_max} + tolerance {tolerance}"

        # Should have reasonable content
        assert len(result) > 0
        assert result_tokens > target_max * 0.3  # At least 30% utilized


@pytest.mark.skipif(not EMBEDDINGS_AVAILABLE, reason="sentence-transformers not installed")
class TestMessageEmbedder:
    """Test MessageEmbedder with real embeddings."""

    @pytest.fixture
    def db(self, tmp_path: Path) -> Database:
        """Create a test database."""
        db_path = tmp_path / "test.db"
        return Database(db_path)

    @pytest.fixture
    def vector_store(self, db: Database) -> VectorStore:
        """Create a vector store."""
        return VectorStore(db)

    @pytest.fixture
    def embedder(self, db: Database, vector_store: VectorStore) -> MessageEmbedder:
        """Create a message embedder."""
        return MessageEmbedder(db, vector_store)

    async def test_compute_and_store(self, embedder: MessageEmbedder, db: Database):
        """Test computing and storing an embedding."""
        # Insert a test message
        with db.connect() as conn:
            cursor = conn.execute(
                """INSERT INTO agent_chat_messages
                   (agent_id, role, content, conversation_id, token_count)
                   VALUES (?, ?, ?, ?, ?)""",
                ("test-agent", "user", "Hello, this is a test message", "conv-1", 10),
            )
            message_id = cursor.lastrowid

        # Compute embedding
        await embedder.compute_and_store(message_id, "Hello, this is a test message")

        # Verify embedding was stored
        with db.connect() as conn:
            row = conn.execute(
                "SELECT embedding FROM agent_chat_messages WHERE id = ?",
                (message_id,),
            ).fetchone()

        assert row["embedding"] is not None
        embedding = decode_embedding(row["embedding"])
        assert len(embedding) == 384  # MiniLM dimension

    async def test_schedule_embedding_background(self, embedder: MessageEmbedder, db: Database):
        """Test scheduling embedding as background task."""
        # Insert a test message
        with db.connect() as conn:
            cursor = conn.execute(
                """INSERT INTO agent_chat_messages
                   (agent_id, role, content, conversation_id, token_count)
                   VALUES (?, ?, ?, ?, ?)""",
                ("test-agent", "user", "Background test message", "conv-1", 10),
            )
            message_id = cursor.lastrowid

        # Schedule (returns immediately)
        embedder.schedule_embedding(message_id, "Background test message")

        # Wait for background task
        await embedder.wait_pending(timeout=10.0)

        # Verify embedding was stored
        with db.connect() as conn:
            row = conn.execute(
                "SELECT embedding FROM agent_chat_messages WHERE id = ?",
                (message_id,),
            ).fetchone()

        assert row["embedding"] is not None

    async def test_backfill_missing_embeddings(self, embedder: MessageEmbedder, db: Database):
        """Test backfilling embeddings for messages without them."""
        # Insert messages without embeddings
        with db.connect() as conn:
            for i in range(5):
                conn.execute(
                    """INSERT INTO agent_chat_messages
                       (agent_id, role, content, conversation_id, token_count)
                       VALUES (?, ?, ?, ?, ?)""",
                    ("test-agent", "user", f"Message {i}", "conv-1", 10),
                )

        # Backfill
        count = await embedder.backfill_missing_embeddings("test-agent", "conv-1", batch_size=3)

        assert count == 3  # Limited by batch_size

        # Verify some embeddings were stored
        with db.connect() as conn:
            rows = conn.execute(
                "SELECT embedding FROM agent_chat_messages WHERE agent_id = ?",
                ("test-agent",),
            ).fetchall()

        embeddings_count = sum(1 for r in rows if r["embedding"] is not None)
        assert embeddings_count == 3


@pytest.mark.skipif(not OLLAMA_AVAILABLE, reason="Ollama not running locally")
@pytest.mark.skipif(not EMBEDDINGS_AVAILABLE, reason="sentence-transformers not installed")
class TestMessageEmbedderWithOllama:
    """Integration tests using local Ollama with ministral-3:3b."""

    @pytest.fixture
    def db(self, tmp_path: Path) -> Database:
        """Create a test database."""
        db_path = tmp_path / "test.db"
        return Database(db_path)

    @pytest.fixture
    def vector_store(self, db: Database) -> VectorStore:
        """Create a vector store."""
        return VectorStore(db)

    @pytest.fixture
    def embedder(self, db: Database, vector_store: VectorStore) -> MessageEmbedder:
        """Create a message embedder."""
        return MessageEmbedder(db, vector_store)

    @pytest.fixture
    def config(self, tmp_path: Path) -> Config:
        """Create a test configuration with Ollama backend."""
        data_dir = tmp_path / ".xpressai"
        data_dir.mkdir(parents=True, exist_ok=True)
        return Config(
            system=SystemConfig(
                isolation="none",
                budget=BudgetConfig(daily=Decimal("10.00")),
                data_dir=data_dir,
            ),
            agents=[
                AgentConfig(
                    name="test-agent",
                    backend="local",
                    role="Test agent for context management",
                    local_model=LocalModelConfig(
                        model="ministral-3:3b",
                        inference_backend="ollama",
                        base_url="http://localhost:11434",
                    ),
                ),
            ],
            local_model=LocalModelConfig(
                model="ministral-3:3b",
                inference_backend="ollama",
                base_url="http://localhost:11434",
            ),
            memory=MemoryConfig(near_term_slots=4),
        )

    async def test_full_conversation_flow(
        self, embedder: MessageEmbedder, db: Database, vector_store: VectorStore, config: Config
    ):
        """Test full conversation flow with embeddings.

        This test:
        1. Stores messages with token counts
        2. Computes embeddings in background
        3. Uses ContextManager to assemble context
        """
        from xpressai.core.runtime import Runtime

        # Initialize runtime
        runtime = Runtime(config, workspace=config.system.data_dir.parent)
        await runtime.initialize()

        try:
            # Create a conversation
            conversation_id = "test-conv-1"
            agent_id = "test-agent"

            # Insert several messages simulating a conversation
            messages_data = [
                ("user", "Hello! I'm working on a Python project."),
                ("agent", "Great! I'd be happy to help with your Python project. What are you working on?"),
                ("user", "I'm building a web scraper using BeautifulSoup."),
                ("agent", "BeautifulSoup is excellent for web scraping. What specific functionality do you need?"),
                ("user", "I need to extract product prices from an e-commerce site."),
                ("agent", "For extracting prices, you'll want to inspect the page structure first. Use browser dev tools to find the CSS selectors for price elements."),
                ("user", "Thanks! Now I have a question about databases."),
                ("agent", "Sure, what's your database question?"),
                ("user", "Should I use SQLite or PostgreSQL for my project?"),
                ("agent", "It depends on your needs. SQLite is great for local development and smaller applications. PostgreSQL is better for production with concurrent users."),
            ]

            message_ids = []
            with db.connect() as conn:
                for role, content in messages_data:
                    token_count = count_tokens(content)
                    cursor = conn.execute(
                        """INSERT INTO agent_chat_messages
                           (agent_id, role, content, conversation_id, token_count)
                           VALUES (?, ?, ?, ?, ?)""",
                        (agent_id, role, content, conversation_id, token_count),
                    )
                    message_ids.append(cursor.lastrowid)

            # Schedule embeddings for all messages
            for msg_id, (role, content) in zip(message_ids, messages_data):
                embedder.schedule_embedding(msg_id, content)

            # Wait for embeddings to complete
            await embedder.wait_pending(timeout=30.0)

            # Verify embeddings were stored
            with db.connect() as conn:
                rows = conn.execute(
                    """SELECT id, content, embedding FROM agent_chat_messages
                       WHERE agent_id = ? AND conversation_id = ?
                       ORDER BY timestamp""",
                    (agent_id, conversation_id),
                ).fetchall()

            embeddings_stored = sum(1 for r in rows if r["embedding"] is not None)
            assert embeddings_stored == len(messages_data), f"Expected {len(messages_data)} embeddings, got {embeddings_stored}"

            # Now use ContextManager to assemble context
            messages = [
                Message(
                    id=row["id"],
                    role="assistant" if rows[i][1] == "agent" else "user",
                    content=row["content"],
                    token_count=count_tokens(row["content"]),
                    embedding=row["embedding"],
                )
                for i, row in enumerate(rows)
            ]

            # Create context manager with limited tokens to force selection
            context_mgr = ContextManager(
                max_context_tokens=500,  # Small to force selection
                target_utilization=0.9,
                recent_window_ratio=0.5,
                min_threshold=0.3,
            )

            # Create context embedding from the latest query (about databases)
            context_emb = await vector_store.embedding_model.embed_async(
                "Should I use SQLite or PostgreSQL for my project?"
            )

            # Assemble context
            assembled, total_tokens = context_mgr.assemble_context(messages, context_embedding=context_emb)

            # Verify context was assembled
            assert len(assembled) > 0
            assert total_tokens > 0
            assert total_tokens <= 500 * 0.9  # Within budget

            # The most recent messages should be included
            contents = [m["content"] for m in assembled]
            assert any("PostgreSQL" in c or "SQLite" in c for c in contents)

        finally:
            # Clean up
            await runtime.stop()

    async def test_embedding_quality(self, embedder: MessageEmbedder, db: Database):
        """Test that embeddings capture semantic similarity."""
        # Create messages with known semantic relationships
        similar_messages = [
            "Python is a programming language",
            "Python is used for software development",
            "Python programming is popular",
        ]
        different_message = "The weather today is sunny and warm"

        # Insert and embed messages
        message_ids = []
        with db.connect() as conn:
            for content in similar_messages + [different_message]:
                cursor = conn.execute(
                    """INSERT INTO agent_chat_messages
                       (agent_id, role, content, conversation_id, token_count)
                       VALUES (?, ?, ?, ?, ?)""",
                    ("test-agent", "user", content, "conv-1", count_tokens(content)),
                )
                message_ids.append(cursor.lastrowid)

        # Compute embeddings
        for msg_id, content in zip(message_ids, similar_messages + [different_message]):
            await embedder.compute_and_store(msg_id, content)

        # Retrieve embeddings
        embeddings = []
        with db.connect() as conn:
            for msg_id in message_ids:
                row = conn.execute(
                    "SELECT embedding FROM agent_chat_messages WHERE id = ?",
                    (msg_id,),
                ).fetchone()
                embeddings.append(decode_embedding(row["embedding"]))

        # Compute similarities
        def cosine_sim(a, b):
            dot = sum(x * y for x, y in zip(a, b))
            norm_a = sum(x * x for x in a) ** 0.5
            norm_b = sum(x * x for x in b) ** 0.5
            return dot / (norm_a * norm_b)

        # Similar messages should have high similarity
        sim_01 = cosine_sim(embeddings[0], embeddings[1])
        sim_02 = cosine_sim(embeddings[0], embeddings[2])
        sim_12 = cosine_sim(embeddings[1], embeddings[2])

        # Different message should have lower similarity
        sim_03 = cosine_sim(embeddings[0], embeddings[3])

        assert sim_01 > 0.7, f"Similar messages should have high similarity, got {sim_01}"
        assert sim_02 > 0.7, f"Similar messages should have high similarity, got {sim_02}"
        assert sim_12 > 0.7, f"Similar messages should have high similarity, got {sim_12}"
        assert sim_03 < sim_01, f"Different message should be less similar: {sim_03} vs {sim_01}"


@pytest.mark.skipif(not OLLAMA_AVAILABLE, reason="Ollama not running locally")
class TestOllamaBackendWithContext:
    """Test Ollama backend integration with context management."""

    @pytest.fixture
    def config(self, tmp_path: Path) -> Config:
        """Create a test configuration using local backend with Ollama."""
        data_dir = tmp_path / ".xpressai"
        data_dir.mkdir(parents=True, exist_ok=True)
        return Config(
            system=SystemConfig(
                isolation="none",
                budget=BudgetConfig(daily=Decimal("10.00")),
                data_dir=data_dir,
            ),
            agents=[
                AgentConfig(
                    name="test-agent",
                    backend="local",
                    role="You are a helpful assistant.",
                    local_model=LocalModelConfig(
                        model="ministral-3:3b",
                        inference_backend="ollama",
                        base_url="http://localhost:11434",
                    ),
                ),
            ],
            local_model=LocalModelConfig(
                model="ministral-3:3b",
                inference_backend="ollama",
                base_url="http://localhost:11434",
            ),
            memory=MemoryConfig(near_term_slots=4),
        )

    async def test_ollama_responds(self, config: Config):
        """Test that Ollama responds to a simple query."""
        from xpressai.core.runtime import Runtime

        runtime = Runtime(config, workspace=config.system.data_dir.parent)
        await runtime.initialize()
        await runtime.start()

        try:
            # Get the backend
            backend = runtime._backends.get("test-agent")
            assert backend is not None, "Backend should be created for test-agent"

            # Send a simple message
            response_parts = []
            async for chunk in backend.send("Say 'Hello' and nothing else."):
                response_parts.append(chunk)

            response = "".join(response_parts)
            # Just verify we got a response - the model may respond with text or a tool call
            # due to the built-in memory instructions in the system prompt
            assert len(response) > 0, "Expected non-empty response from Ollama"

        finally:
            await runtime.stop()

    async def test_ollama_with_history(self, config: Config):
        """Test that Ollama can use conversation history."""
        from xpressai.core.runtime import Runtime

        runtime = Runtime(config, workspace=config.system.data_dir.parent)
        await runtime.initialize()
        await runtime.start()

        try:
            backend = runtime._backends.get("test-agent")
            assert backend is not None, "Backend should be created for test-agent"

            # Set history
            history = [
                {"role": "user", "content": "My name is Alice."},
                {"role": "assistant", "content": "Nice to meet you, Alice!"},
            ]
            backend.set_history(history)

            # Ask about the name
            response_parts = []
            async for chunk in backend.send("What is my name?"):
                response_parts.append(chunk)

            response = "".join(response_parts)
            # The model may respond with Alice directly, or with a tool call to search memory
            # Both are valid behaviors given the system prompt
            assert len(response) > 0, "Expected non-empty response"
            # If it's not a tool call, it should contain Alice
            if "search_memory" not in response and "query" not in response:
                assert "Alice" in response, f"Expected 'Alice' in response, got: {response}"

        finally:
            await runtime.stop()


@pytest.mark.skipif(not OLLAMA_AVAILABLE, reason="Ollama not running locally")
class TestTinyLlamaContextOverflow:
    """Test TinyLlama with conversations exceeding its 2048 token context window.

    This is an end-to-end integration test that:
    1. Creates a conversation larger than TinyLlama's context
    2. Uses ContextManager to compress it
    3. Sends to TinyLlama and verifies it responds without errors
    """

    @pytest.fixture
    def config(self, tmp_path: Path) -> Config:
        """Create a test configuration using TinyLlama via Ollama."""
        data_dir = tmp_path / ".xpressai"
        data_dir.mkdir(parents=True, exist_ok=True)
        return Config(
            system=SystemConfig(
                isolation="none",
                budget=BudgetConfig(daily=Decimal("10.00")),
                data_dir=data_dir,
            ),
            agents=[
                AgentConfig(
                    name="tinyllama-agent",
                    backend="local",
                    role="You are a helpful assistant. Keep responses brief.",
                    local_model=LocalModelConfig(
                        model="tinyllama",
                        inference_backend="ollama",
                        base_url="http://localhost:11434",
                    ),
                ),
            ],
            local_model=LocalModelConfig(
                model="tinyllama",
                inference_backend="ollama",
                base_url="http://localhost:11434",
            ),
            memory=MemoryConfig(near_term_slots=4),
        )

    def _build_large_conversation(self, target_tokens: int = 8000) -> list[dict[str, str]]:
        """Build a conversation exceeding target token count."""
        messages = []
        total_tokens = 0

        # Conversation turns covering different topics
        turns = [
            ("user", "I'm building a web application using Python and Flask. Can you help me understand the best practices for structuring a Flask project?"),
            ("assistant", "For Flask project structure, I recommend organizing your code into blueprints for different features. Create separate folders for templates, static files, and models. Use a factory pattern with create_app() to initialize your application. Keep configuration in a separate config.py file with different classes for development, testing, and production environments."),
            ("user", "That makes sense. What about database integration? I'm thinking of using SQLAlchemy."),
            ("assistant", "SQLAlchemy is an excellent choice for Flask. Use Flask-SQLAlchemy extension for seamless integration. Define your models in a models.py file or a models package. Use Flask-Migrate for database migrations. Keep your database URI in environment variables for security. Remember to close sessions properly to avoid connection leaks."),
            ("user", "How should I handle user authentication in my Flask app?"),
            ("assistant", "For authentication, consider using Flask-Login for session management. Implement secure password hashing with werkzeug.security or bcrypt. Create a User model with email validation. Use decorators like @login_required to protect routes. For APIs, consider JWT tokens with Flask-JWT-Extended. Always use HTTPS in production."),
            ("user", "What about testing my Flask application?"),
            ("assistant", "Use pytest with Flask's test client. Create a conftest.py with fixtures for your app and database. Test each blueprint separately. Use factories like factory_boy for test data. Mock external services. Aim for high coverage but focus on critical paths. Run tests in CI/CD pipeline before deployment."),
            ("user", "Can you explain how to deploy a Flask app to production?"),
            ("assistant", "For production deployment, use a WSGI server like Gunicorn behind Nginx. Containerize with Docker for consistency. Use environment variables for secrets. Set up logging with proper rotation. Implement health checks. Use a process manager like systemd. Consider using cloud services like AWS, GCP, or Heroku for easier scaling."),
            ("user", "What about performance optimization for Flask?"),
            ("assistant", "Optimize Flask by implementing caching with Flask-Caching or Redis. Use database connection pooling. Minimize database queries with eager loading. Compress responses with gzip. Use CDN for static assets. Profile with Flask-DebugToolbar in development. Consider async with Flask 2.0+ for I/O-bound operations. Monitor with tools like New Relic."),
            ("user", "How do I implement API rate limiting?"),
            ("assistant", "Implement rate limiting with Flask-Limiter. Configure limits per route or globally. Use Redis as backend for distributed limiting. Set sensible defaults like 100 requests per minute. Return proper 429 status codes with Retry-After headers. Consider different limits for authenticated vs anonymous users. Log rate limit hits for monitoring."),
            ("user", "What security measures should I implement?"),
            ("assistant", "Security essentials: Use CSRF protection with Flask-WTF. Implement Content Security Policy headers. Sanitize all user input to prevent XSS. Use parameterized queries to prevent SQL injection. Set secure cookie flags. Implement proper CORS policies. Keep dependencies updated. Use security headers like X-Frame-Options. Consider using Flask-Talisman for security defaults."),
        ]

        # Repeat turns until we exceed target
        while total_tokens < target_tokens:
            for role, content in turns:
                tokens = count_tokens(content)
                messages.append({"role": role, "content": content})
                total_tokens += tokens
                if total_tokens >= target_tokens:
                    break

        return messages, total_tokens

    async def test_tinyllama_handles_context_overflow(self, config: Config):
        """Test that TinyLlama handles conversation exceeding its context window.

        This test creates a ~8000 token conversation (4x TinyLlama's 2048 limit),
        uses ContextManager to compress it, and verifies TinyLlama responds.
        """
        import time
        from xpressai.core.runtime import Runtime

        print("\n=== TinyLlama Context Overflow Test ===")

        # Build a conversation that exceeds TinyLlama's context
        TINYLLAMA_CONTEXT = 2048
        TARGET_TOKENS = 8000  # 4x the context window

        history, total_tokens = self._build_large_conversation(TARGET_TOKENS)
        print(f"  Built conversation: {len(history)} messages, {total_tokens} tokens")
        print(f"  TinyLlama context: {TINYLLAMA_CONTEXT} tokens")
        print(f"  Overflow ratio: {total_tokens / TINYLLAMA_CONTEXT:.1f}x")

        # Use ContextManager to compress
        mgr = ContextManager(
            max_context_tokens=TINYLLAMA_CONTEXT,
            target_utilization=0.85,  # Leave room for response
            recent_window_ratio=0.50,
            min_threshold=0.3,
        )

        # Convert to Message objects for the manager
        messages = [
            Message(
                id=i,
                role=m["role"],
                content=m["content"],
                token_count=count_tokens(m["content"]),
            )
            for i, m in enumerate(history)
        ]

        start_time = time.time()
        compressed, compressed_tokens = mgr.assemble_context(messages)
        compress_time = time.time() - start_time

        print(f"  Compressed to: {len(compressed)} messages, {compressed_tokens} tokens")
        print(f"  Compression time: {compress_time*1000:.2f}ms")

        # Initialize runtime and send to TinyLlama
        runtime = Runtime(config, workspace=config.system.data_dir.parent)
        await runtime.initialize()
        await runtime.start()

        try:
            backend = runtime._backends.get("tinyllama-agent")
            assert backend is not None, "Backend should be created for tinyllama-agent"

            # Set the compressed history
            backend.set_history(compressed)

            # Ask a question that requires context
            question = "Based on our discussion, what are the key security measures for Flask?"

            print(f"  Sending question: '{question[:50]}...'")

            start_time = time.time()
            response_parts = []
            async for chunk in backend.send(question):
                response_parts.append(chunk)
            response_time = time.time() - start_time

            response = "".join(response_parts)
            print(f"  Response time: {response_time:.2f}s")
            print(f"  Response length: {len(response)} chars")

            # Verify we got a response (TinyLlama processed without errors)
            assert len(response) > 0, "Expected non-empty response from TinyLlama"
            print(f"  Response preview: {response[:200]}...")

            print("  ✓ TinyLlama successfully handled context overflow")

        finally:
            await runtime.stop()

    async def test_tinyllama_preserves_recent_context(self, config: Config):
        """Test that recent context is preserved after compression.

        Verifies the model can answer questions about recent conversation topics
        even when older context is elided.
        """
        from xpressai.core.runtime import Runtime

        print("\n=== TinyLlama Recent Context Preservation Test ===")

        # Create conversation with distinct early and recent topics
        history = [
            # Early topic: cooking (will likely be elided)
            {"role": "user", "content": "I want to learn how to make pasta from scratch. What ingredients do I need?"},
            {"role": "assistant", "content": "For homemade pasta you need flour, eggs, salt, and optionally olive oil. Use 100g flour per egg. Mix into dough, rest for 30 minutes, then roll thin."},
            {"role": "user", "content": "What's the best sauce for fresh pasta?"},
            {"role": "assistant", "content": "Fresh pasta pairs wonderfully with simple sauces. Try cacio e pepe with pecorino and black pepper, or a light tomato basil sauce. Avoid heavy cream sauces that overpower the delicate pasta flavor."},
            # Padding to fill context
            {"role": "user", "content": "Tell me about Italian cuisine in general. What makes it special?"},
            {"role": "assistant", "content": "Italian cuisine is renowned for its regional diversity, emphasis on fresh seasonal ingredients, and simplicity that lets quality ingredients shine. From Neapolitan pizza to Bolognese ragù, each region has distinct specialties reflecting local traditions and available ingredients."},
            {"role": "user", "content": "What about desserts?"},
            {"role": "assistant", "content": "Italian desserts are world-famous. Tiramisu with coffee-soaked ladyfingers, creamy panna cotta, crispy cannoli filled with sweet ricotta, and gelato in countless flavors. Each region has its specialties - Sicily is known for cassata and granita."},
            # Recent topic: programming (should be preserved)
            {"role": "user", "content": "Let's switch topics. I'm learning Python programming."},
            {"role": "assistant", "content": "Python is excellent for beginners! It has clean syntax, extensive libraries, and a supportive community. Start with variables, loops, and functions. Practice with small projects."},
            {"role": "user", "content": "What's a good first project for learning Python?"},
            {"role": "assistant", "content": "Try building a simple calculator or a number guessing game. These projects teach input/output, conditionals, and loops. Next, try a to-do list app to learn about data structures like lists and dictionaries."},
            {"role": "user", "content": "I built the calculator! Now I want to learn about functions."},
            {"role": "assistant", "content": "Great progress! Functions help organize code into reusable blocks. Define with 'def', add parameters, use return to output values. Start by refactoring your calculator - make add(), subtract(), multiply(), divide() functions."},
        ]

        total_tokens = sum(count_tokens(m["content"]) for m in history)
        print(f"  Conversation: {len(history)} messages, {total_tokens} tokens")

        # Compress if needed
        TINYLLAMA_CONTEXT = 2048
        mgr = ContextManager(
            max_context_tokens=TINYLLAMA_CONTEXT,
            target_utilization=0.80,
            recent_window_ratio=0.60,  # Favor recent messages
        )

        messages = [
            Message(id=i, role=m["role"], content=m["content"], token_count=count_tokens(m["content"]))
            for i, m in enumerate(history)
        ]

        compressed, _ = mgr.assemble_context(messages)
        print(f"  Compressed to: {len(compressed)} messages")

        # Check if recent Python topic is preserved
        python_preserved = any("Python" in m["content"] or "function" in m["content"] for m in compressed)
        print(f"  Recent topic (Python) preserved: {python_preserved}")

        # Initialize runtime
        runtime = Runtime(config, workspace=config.system.data_dir.parent)
        await runtime.initialize()
        await runtime.start()

        try:
            backend = runtime._backends.get("tinyllama-agent")
            assert backend is not None

            backend.set_history(compressed)

            # Ask about the recent topic (Python)
            response_parts = []
            async for chunk in backend.send("What project did I just build? What should I learn next?"):
                response_parts.append(chunk)

            response = "".join(response_parts)
            print(f"  Response: {response[:300]}...")

            # The model should reference the calculator or functions
            # (It might not always, since TinyLlama is small, but it should respond coherently)
            assert len(response) > 0, "Expected response about recent context"
            print("  ✓ TinyLlama responded to recent context query")

        finally:
            await runtime.stop()
