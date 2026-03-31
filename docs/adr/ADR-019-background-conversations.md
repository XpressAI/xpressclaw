# ADR-019: Background Conversations

## Status
Proposed

## Context

Conversations currently require an active client connection. The frontend sends a message via `POST /conversations/{id}/messages/stream`, and the server streams the agent's response back via SSE. If the user navigates away, the connection drops and the response is lost — partially generated text is never stored, and the agent's work is wasted.

This breaks several important use cases:

1. **Navigate away during a long response.** The user should be able to browse other pages while the agent works.
2. **Queue messages while the agent is thinking.** The user types a follow-up before the agent finishes — it should be delivered at the next opportunity.
3. **Agent-to-agent handoffs.** When workflows hand off between agents, there's no client connected. The conversation must progress without a browser.
4. **Scheduled tasks that use conversations.** A cron-triggered task needs to send messages to a conversation with no UI open.
5. **Mobile / flaky connections.** A dropped WiFi connection shouldn't kill the agent mid-thought.

### How OpenAI's Responses API Works

OpenAI's approach: the client creates a "response" which runs server-side. The client can poll or stream events, but the response completes regardless of client presence. Events are stored and can be replayed.

We need the same pattern.

## Decision

Move conversation message processing to a **server-side background task**. The client becomes a subscriber to events, not the driver of the conversation.

### Architecture

```
Client                     Server                      Agent
  |                          |                           |
  |-- POST /messages ------->|                           |
  |<-- 201 {user_msg} ------|                           |
  |                          |-- spawn background task -->|
  |                          |   (runs independently)    |
  |-- GET /messages/events ->|                           |
  |<-- SSE: thinking -------|                           |
  |<-- SSE: chunk ----------|<-- streaming response ----|
  |<-- SSE: chunk ----------|                           |
  |   (user navigates away)  |                           |
  |                          |<-- more chunks -----------|
  |                          |-- store complete message  |
  |                          |                           |
  |-- GET /messages -------->|                           |
  |<-- [user_msg, agent_msg]-|  (full response available)|
```

### 1. Send Message (Non-Streaming)

`POST /conversations/{id}/messages` becomes fire-and-forget:

```rust
async fn send_message(conv_id, content) -> 201 Created {
    // Store user message
    let user_msg = mgr.store_message(conv_id, "user", content);

    // Spawn background task — runs independently of the client
    tokio::spawn(async move {
        process_agent_response(conv_id, agent_id).await;
    });

    // Return immediately — don't wait for the agent
    return user_msg;
}
```

### 2. Subscribe to Events

`GET /conversations/{id}/events` is a reconnectable SSE stream:

```rust
async fn subscribe_events(conv_id, query: { after_message_id }) -> SSE {
    // Replay any events the client missed (messages stored since after_message_id)
    for msg in mgr.get_messages_after(conv_id, after_message_id) {
        yield event("message", msg);
    }

    // Then stream live events from the broadcast channel
    let mut rx = event_bus.subscribe(conv_id);
    loop {
        match rx.recv().await {
            Ok(event) => yield event,
            Err(_) => break, // Channel closed
        }
    }
}
```

The client can disconnect and reconnect at any time. On reconnect, it passes the last `message_id` it saw, and the server replays missed messages before switching to live streaming.

### 3. Background Processing

The background task processes the conversation regardless of client presence:

```rust
async fn process_agent_response(conv_id: &str, agent_id: &str) {
    // Broadcast "thinking" event
    event_bus.send(conv_id, Event::Thinking { agent_id });

    // Stream from the LLM
    let mut full_content = String::new();
    let stream = llm_router.chat_stream(&request).await;

    while let Some(chunk) = stream.next().await {
        full_content.push_str(&chunk);
        // Broadcast chunk to any connected clients
        event_bus.send(conv_id, Event::Chunk { agent_id, content: chunk });
    }

    // Store the complete message — this is the source of truth
    let agent_msg = mgr.store_message(conv_id, "agent", &full_content);

    // Broadcast completion
    event_bus.send(conv_id, Event::Message(agent_msg));
}
```

If no client is connected, the broadcast events are dropped — that's fine. The stored message is the source of truth. When the client reconnects and calls `GET /messages`, it gets the full conversation including the response it missed.

### 4. Queued User Messages

If the user sends a message while the agent is still responding:

```rust
async fn send_message(conv_id, content) {
    let user_msg = mgr.store_message(conv_id, "user", content);

    // Check if a background task is already running for this conversation
    if !is_processing(conv_id) {
        tokio::spawn(process_agent_response(conv_id, agent_id));
    }
    // If already processing, the background task will pick up
    // the new message when it finishes the current response
    // (it re-checks for unprocessed user messages before exiting)

    return user_msg;
}
```

The background task checks for new user messages after completing each response:

```rust
async fn process_agent_response(conv_id, agent_id) {
    loop {
        // Get all unprocessed user messages
        let pending = mgr.get_unprocessed_messages(conv_id);
        if pending.is_empty() {
            break; // Nothing more to process
        }

        // Build context from full history + pending messages
        let history = mgr.get_messages(conv_id);
        let response = llm_router.chat(&history).await;

        mgr.store_message(conv_id, "agent", &response);
        event_bus.send(conv_id, Event::Message(agent_msg));

        // Mark messages as processed
        mgr.mark_processed(conv_id, &pending);

        // Loop to check for more messages that arrived during processing
    }
}
```

### 5. Event Bus

A per-conversation broadcast channel:

```rust
struct ConversationEventBus {
    channels: RwLock<HashMap<String, broadcast::Sender<ConversationEvent>>>,
}

enum ConversationEvent {
    Thinking { agent_id: String },
    Chunk { agent_id: String, content: String },
    Message(ConversationMessage),
    Error { agent_id: String, error: String },
}
```

`broadcast::channel` with a buffer of 256 events. Slow receivers get `Lagged` errors and should reconnect with `after_message_id` to catch up from the DB.

### 6. Frontend Changes

The conversation page:

```typescript
// On mount: load history, then subscribe to live events
const messages = await api.conversations.messages(convId);
renderMessages(messages);

const lastId = messages.at(-1)?.id ?? 0;
const eventSource = new EventSource(`/api/conversations/${convId}/events?after=${lastId}`);

eventSource.onmessage = (e) => {
    const event = JSON.parse(e.data);
    switch (event.type) {
        case 'thinking': showThinkingIndicator(event.agent_id); break;
        case 'chunk': appendChunk(event.agent_id, event.content); break;
        case 'message': addCompleteMessage(event); break;
    }
};

// On navigate away: eventSource closes automatically.
// On navigate back: reconnect with after_message_id, replay missed events.
```

Sending a message:

```typescript
async function sendMessage(content: string) {
    // Fire and forget — don't wait for agent response
    const userMsg = await api.conversations.sendMessage(convId, content);
    addMessage(userMsg);
    // The SSE stream will deliver the agent's response when ready
}
```

### 7. Conversation State Machine

Conversations get their own desired state (per ADR-018):

```
idle ──▶ processing ──▶ idle
processing ──▶ processing  (new message queued)
idle ──▶ closed
```

- `idle`: No background task running. Ready for new messages.
- `processing`: Background task is generating a response. New messages are queued.
- `closed`: Conversation archived. No new messages.

### 8. Migration Path

1. Add `processing_status` column to conversations table ('idle' | 'processing')
2. Add `processed` boolean column to conversation_messages
3. Add `ConversationEventBus` to `AppState`
4. Convert `stream_message` handler to fire-and-forget + spawn background task
5. Add `GET /conversations/{id}/events` SSE endpoint with replay
6. Update frontend to use event subscription instead of streaming POST
7. Keep old `POST /messages/stream` working during transition (deprecated)

## Consequences

### Positive
- Conversations survive navigation, disconnects, and tab closes
- Users can queue messages while the agent is thinking
- Enables agent-to-agent handoffs in workflows
- Enables scheduled/cron-triggered conversations
- Mobile-friendly — tolerates flaky connections
- Foundation for the conversation state machine in ADR-018

### Negative
- More complex than direct streaming
- Requires event bus infrastructure (broadcast channels)
- Replay logic adds complexity to the SSE endpoint
- Background tasks need lifecycle management (timeouts, cancellation)

### Risks
- Memory pressure from broadcast channels for many concurrent conversations
  (mitigated by small buffer + DB replay for slow clients)
- Background task leak if not properly tracked
  (mitigated by conversation state machine — reconciler can detect stuck "processing" conversations)

## Related ADRs
- ADR-013: Enhanced Agent Chat UI (conversation management)
- ADR-018: Desired-State Reconciliation (state machine pattern)
