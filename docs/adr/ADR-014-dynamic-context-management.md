# ADR-014: Dynamic Context Management

## Status
Accepted

## Context

LLMs have finite context windows (e.g., 32K, 128K, 200K tokens). Long conversations eventually exceed this limit. The naive approach of truncating to the last N messages loses valuable context from earlier in the conversation.

We need a system that:
1. Provides "virtually infinite" context by intelligently selecting which messages to include
2. Maximizes context utilization (uses ~90% of available window)
3. Preserves the most relevant messages regardless of their position in history
4. Performs efficiently without slowing down interactions

## Decision

### Core Algorithm

Each request to the model uses a **relevance-threshold selection** algorithm:

1. **Recent window**: The most recent 50% of context is always included as-is (provides coherent immediate context)

2. **Older messages**: Messages beyond the recent window are scored for relevance to the current context

3. **Threshold sweep**: Start with a high relevance threshold, include all messages above it. Decrease threshold until we reach the target context utilization (default: 90% of max context length)

4. **Elision**: Messages below the final threshold are replaced with `[...]` markers to indicate content was elided

### Data Model

Each message stores:
- `token_count`: Cached token count (computed once on storage)
- `embedding`: Vector embedding for relevance scoring (computed once on storage)

```sql
ALTER TABLE agent_chat_messages ADD COLUMN token_count INTEGER;
ALTER TABLE agent_chat_messages ADD COLUMN embedding BLOB;  -- sqlite-vec format
```

### Relevance Scoring

Relevance is computed as cosine similarity between:
- The message's embedding
- A "context embedding" derived from recent messages (last ~5 messages concatenated)

This leverages the existing sqlite-vec infrastructure from the memory system.

### Context Assembly

```python
class ContextManager:
    def __init__(
        self,
        max_context_tokens: int = 32768,
        target_utilization: float = 0.90,  # Use 90% of context
        recent_window_ratio: float = 0.50,  # Recent 50% always included
        min_threshold: float = 0.3,  # Minimum relevance to include
    ):
        pass

    def assemble_context(
        self,
        messages: list[Message],
        context_embedding: list[float],
    ) -> tuple[list[Message], int]:
        """
        Returns (assembled_messages, total_tokens).

        Messages below threshold have content replaced with "[...]"
        """
        target_tokens = int(self.max_context_tokens * self.target_utilization)

        # 1. Calculate recent window token budget
        recent_budget = int(target_tokens * self.recent_window_ratio)

        # 2. Always include recent messages up to budget
        recent_messages, recent_tokens = self._take_recent(messages, recent_budget)

        # 3. Score and select older messages
        remaining_budget = target_tokens - recent_tokens
        older_messages = messages[:-len(recent_messages)] if recent_messages else messages

        # 4. Binary search for optimal threshold
        selected_older = self._select_by_threshold(
            older_messages,
            context_embedding,
            remaining_budget
        )

        # 5. Assemble with elision markers
        return self._assemble_with_elision(selected_older, recent_messages)
```

### Threshold Selection Algorithm

```python
def _select_by_threshold(
    self,
    messages: list[Message],
    context_embedding: list[float],
    token_budget: int,
) -> list[tuple[Message, float]]:  # (message, score)
    """Select messages using descending threshold until budget filled."""

    # Score all messages
    scored = [(msg, self._relevance_score(msg, context_embedding)) for msg in messages]

    # Sort by score descending
    scored.sort(key=lambda x: x[1], reverse=True)

    # Take messages until budget exhausted
    selected = []
    used_tokens = 0
    for msg, score in scored:
        if score < self.min_threshold:
            break
        if used_tokens + msg.token_count > token_budget:
            continue  # Skip this one, try smaller messages
        selected.append((msg, score))
        used_tokens += msg.token_count

    # Re-sort by original position for coherent conversation flow
    selected.sort(key=lambda x: x[0].timestamp)
    return selected
```

### Token Counting

Use tiktoken for accurate counts, with caching:

```python
import tiktoken

# Cache the encoder
_encoder = tiktoken.encoding_for_model("gpt-4")  # Works for Claude too

def count_tokens(text: str) -> int:
    return len(_encoder.encode(text))
```

Token counts are computed once when the message is stored and cached in the database.

### Embedding Generation

Reuse the embedding infrastructure from the memory system:

```python
async def compute_embedding(text: str) -> list[float]:
    """Generate embedding for relevance scoring."""
    # Use same embedder as memory system
    return await embedder.embed(text)
```

### Performance Considerations

1. **Token counting**: Done once on message storage, O(n) where n = message length
2. **Embedding generation**: Done once on message storage, single API/model call
3. **Relevance scoring**: Vector dot product, O(d) where d = embedding dimension
4. **Threshold selection**: O(m log m) where m = number of older messages (sorting)
5. **Total per-request overhead**: O(m log m) - dominated by sorting, typically < 10ms

### Configuration

```yaml
context:
  max_tokens: 32768  # Or auto-detect from model
  target_utilization: 0.90
  recent_window_ratio: 0.50
  min_relevance_threshold: 0.3
```

## Consequences

### Positive
- Conversations can effectively be infinite in length
- Most relevant context is always preserved
- Recent context remains coherent (no elision in recent window)
- Efficient: heavy computation (tokens, embeddings) done once on storage
- Graceful degradation: as conversation grows, less relevant parts fade

### Negative
- Requires embedding computation for each message (adds latency on storage)
- Additional storage for token counts and embeddings
- Elided messages may occasionally be needed (user can scroll back)
- Complexity in the context assembly logic

### Risks
- Relevance scoring may not perfectly capture user intent
- Elision markers might confuse the model (mitigated by using standard `[...]` format)

## Alternatives Considered

1. **Simple truncation**: Keep last N messages. Rejected - loses valuable early context.

2. **Summarization**: Summarize old messages. Rejected - adds latency, loses specific details, requires additional LLM calls.

3. **Fixed sliding window**: Always keep N tokens. Rejected - doesn't preserve relevant older context.

4. **User-driven archival**: Let users mark important messages. Rejected - too much friction.

## Implementation Notes

### Database Migration

```sql
-- Migration v8
ALTER TABLE agent_chat_messages ADD COLUMN token_count INTEGER DEFAULT 0;
ALTER TABLE agent_chat_messages ADD COLUMN embedding BLOB;
CREATE INDEX idx_messages_tokens ON agent_chat_messages(token_count);
```

### Backfill Strategy

For existing messages without token counts/embeddings:
- Compute lazily on first context assembly
- Or run background migration task

### Model-Specific Context Limits

```python
MODEL_CONTEXT_LIMITS = {
    "claude-sonnet-4-20250514": 200000,
    "claude-opus-4-0-20250514": 200000,
    "gpt-4o": 128000,
    "qwen3-8b": 32768,
}
```

## References

- ADR-004: Memory System (embedding infrastructure)
- ADR-002: Agent Backend Abstraction (integration point)
