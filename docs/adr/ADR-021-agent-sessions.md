# ADR-021: Agent Sessions (Actor Model)

## Status
Proposed

## Context

The current conversation architecture has a fragile hack: the Rust conversation processor streams from the LLM router (which has no tools), injects text tool descriptions into the system prompt, regex-detects `<tool_call>` XML in the output, and sends a follow-up to the harness for tool execution. This causes:

- Disappearing messages (false-positive tool call detection)
- Inconsistent agent personality (different behavior inside vs outside harness)
- Tool calls that silently fail (container_id missing, harness unreachable)
- Streaming and tools are mutually exclusive

The harness (claude-sdk) has the correct tools via MCP and the right personality, but its streaming is buffered because the Claude Agent SDK collects the entire response before yielding events for local models.

## Decision

### Agent Sessions as Actors

Each agent runs as a persistent **session** — a long-lived Claude Agent SDK session that maintains context across interactions. This replaces the current request-response model where each message starts a fresh `query()`.

The Claude Agent SDK (v0.1.55+) supports:
- `session_id` — resume a specific session
- `resume` / `continue_conversation` — continue in the same session
- `prompt` as `AsyncIterable[dict]` — stream input into a running session
- `list_sessions`, `get_session_info`, `get_session_messages` — session introspection
- Built-in context compaction

### Message Flow

```
User sends message in conversation "proj-foo"
    ↓
Server injects into agent session:
    SYSTEM: Message received in conversation "proj-foo"
    From: Eduardo Gonzalez
    Content: Can you review the latest PR?
    ↓
Agent processes (with full MCP tools, memory, workspace access)
    ↓
Agent responds (streamed via SDK events or buffered):
    <reply conversation="proj-foo">
    I'll review PR #42 now. Let me check the diff...
    </reply>
    ↓
Server parses reply, routes to conversation "proj-foo"
    ↓
Broadcast via SSE to connected clients
```

### Session Lifecycle

**Daily cycle:**
1. **Morning start**: New session created with workspace listing, previous day's notes, and agent role/config
2. **During the day**: Messages from conversations, tasks, connectors are injected as system events
3. **End of day**: Agent gets a chance to write notes for the next session and clean up workspace
4. **Compaction**: The SDK handles context window management automatically

**Session persistence:**
- Sessions are stored by the Claude Agent SDK in the agent's workspace
- Session ID is stored in the xpressclaw database on the agent record
- On restart, the agent resumes the current session (or creates a new one)

### Conversations

Conversations remain multi-participant (users + agents). A conversation is NOT tied to a single agent — it's a shared context where multiple agents and users interact.

When a message is sent to a conversation:
1. The server identifies which agents are participants
2. For each targeted agent, the message is injected into that agent's session
3. Each agent responds in its own session, and replies are routed back to the conversation

### Connectors (Future)

Connectors (Slack, email, Telegram) send messages to an agent's session the same way:
```
SYSTEM: Message received via Slack channel #engineering
From: @john
Content: The deploy is failing
```

The agent uses connector tools or inline XML to reply:
```
<reply connector="slack" channel="#engineering">
I'll check the CI logs now.
</reply>
```

## Naming

Following actor model terminology:
- **Session** — the agent's persistent execution context (not "gateway")
- **Message** — an event injected into the session
- **Reply** — the agent's response routed to a destination
- **Actor** — the agent itself

## Consequences

### Positive
- Consistent personality across all interactions
- Full MCP tool access for every response
- Context maintained across conversations
- Natural foundation for multi-agent coordination
- Connectors are just another message source

### Negative
- Responses may not stream token-by-token (SDK buffers for local models)
- Requires the harness container to be running for any interaction
- Session startup adds latency on cold start
- Context compaction may lose nuance from older messages

### Implementation Phases

1. **Phase 1**: Route all conversation messages through harness with `session_id` and `continue_conversation`. Remove the LLM router hack.
2. **Phase 2**: Daily session lifecycle (morning start, end-of-day notes)
3. **Phase 3**: Reply routing (parse `<reply>` tags, route to conversations)
4. **Phase 4**: Connector integration (Slack, email, etc.)
