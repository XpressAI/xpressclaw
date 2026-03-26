# ADR-013: Enhanced Agent Chat UI

## Status
Superseded by ADR-015 (SvelteKit Web UI)

## Context

The current agent chat page is a simple single-column interface that:
- Has no conversation history (all messages in one stream)
- Lacks visibility into agent state (budget, memory, tasks)
- Does not support image attachments
- Provides no way to start fresh conversations

Users need:
1. Ability to have multiple separate conversations with an agent
2. Quick access to agent operational status while chatting
3. Image support for multimodal interactions
4. Better organization of chat history

### Current Implementation

The chat page (`agent_chat.html`) currently has:
- Header with agent name, status, and clear button
- Message container with HTMX polling every 3 seconds
- Simple text input form
- Messages stored in `agent_chat_messages` table (agent_id, role, content, timestamp)

### Memory Slots System

XpressAI uses an 8-slot memory system per agent:
- Each slot can hold one memory with a relevance score
- Memories are evicted based on recency and relevance
- `MemorySlotManager.get_slots()` returns slot state
- `MemorySlotManager.get_stats()` returns occupancy statistics

### Budget System

Budget tracking per agent includes:
- Daily/monthly spent vs limits
- Token counts (input, output, cache)
- Request count
- Pause status
- `BudgetManager.get_summary(agent_id)` returns all budget data

## Decision

### 1. Three-Column Layout

Adopt a responsive 3-column layout:
- **Left (280px)**: Conversation sidebar (collapsible)
- **Center (flex)**: Chat area
- **Right (300px)**: Agent info panel (collapsible)

Both sidebars collapse to ~50px showing only toggle buttons. On mobile (<1100px), sidebars become overlays activated by hamburger buttons.

```
+------------------+------------------------+------------------+
| CONVERSATIONS    |      CHAT AREA         |   AGENT INFO     |
| (collapsible)    |                        |   (collapsible)  |
|                  |  Header                |                  |
| + New Conv       |  ---------------       |  BUDGET          |
|                  |  Messages              |  $0.12 / $5.00   |
| > Conv 1         |  ...                   |                  |
| > Conv 2         |  ---------------       |  MEMORY SLOTS    |
| > Conv 3         |  [📎] [input] [Send]   |  [1][2][3][4]    |
|                  |  📎 Image attached     |  [5][6][7][8]    |
|                  |                        |                  |
|                  |                        |  TASKS           |
|                  |                        |  - Task 1        |
+------------------+------------------------+------------------+
```

### 2. Conversation Management

- Each conversation gets a unique UUID
- Conversations stored in new `conversations` table
- Title auto-generated from first user message (truncated to 40 chars)
- Messages linked via `conversation_id` foreign key
- Switching conversations loads that conversation's messages
- "New Conversation" button creates fresh context

**Database Schema:**

```sql
CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    title TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_conversations_agent ON conversations(agent_id);
CREATE INDEX idx_conversations_updated ON conversations(updated_at);

-- Extend existing table
ALTER TABLE agent_chat_messages ADD COLUMN conversation_id TEXT;
CREATE INDEX idx_agent_chat_conversation ON agent_chat_messages(conversation_id);
```

### 3. Agent Info Panel (Global)

The right panel shows agent-level information that persists across conversations:

**Budget Section:**
- Progress bar showing daily spent vs limit
- Text: `$X.XX / $Y.YY daily`
- Total spent amount

**Memory Slots Section:**
- 4x2 grid of slot indicators
- Empty slots: gray border
- Occupied slots: blue background with relevance score
- Tooltip with memory summary on hover

**Tasks Section:**
- 5 most recent tasks assigned to this agent
- Status indicator and title
- Link to task detail page

Panel refreshes every 10 seconds via HTMX polling.

### 4. Image Support

Images are supported through:
- File picker button (📎) in chat footer
- Paste from clipboard (Ctrl+V)
- Drag and drop onto chat area

**Storage:**
- Images converted to base64 data URLs client-side
- Stored in message content as JSON structure:

```json
{
  "type": "multipart",
  "text": "User message text",
  "images": ["data:image/png;base64,..."]
}
```

**Display:**
- Collapsed indicator in chat: `🖼️ Image 1`
- Click to expand/preview (max 200px)
- Maximum 5 images per message

**API Integration:**
- Images sent as base64 URLs (standard multimodal format)
- Compatible with Claude, GPT-4V, and other vision models

### 5. API Endpoints

**New Endpoints:**
- `GET /api/agent/{agent_id}/conversations` - List conversations
- `POST /api/agent/{agent_id}/conversations` - Create conversation
- `DELETE /api/agent/{agent_id}/conversations/{id}` - Delete conversation
- `GET /partials/agent/{agent_id}/conversations` - Sidebar HTML
- `GET /partials/agent/{agent_id}/info-panel` - Info panel HTML

**Modified Endpoints:**
- `POST /api/agent/{agent_id}/chat` - Add `conversation_id` and `images` parameters
- `GET /partials/agent/{agent_id}/messages` - Add `conversation_id` query param

## Consequences

### Positive
- Users can organize conversations by topic/session
- Agent state visible without leaving chat
- Multimodal interactions enabled
- Better UX for power users
- Memory slot visualization aids debugging

### Negative
- Increased complexity in chat page
- Larger database (base64 images inline)
- More API endpoints to maintain
- Additional JavaScript for image handling

### Risks
- Large images could slow down the UI (mitigated by collapsed display)
- Conversation proliferation could clutter sidebar (consider future archiving)
- Base64 encoding increases payload size ~33%

## Alternatives Considered

1. **Session-based conversations** (auto-group by time gaps)
   - Rejected: Users prefer explicit control over conversation boundaries

2. **Separate image storage** (files on disk, reference in DB)
   - Rejected: Base64 inline is simpler and matches API format expected by models

3. **Tabbed interface instead of sidebars**
   - Rejected: Sidebars allow simultaneous visibility of conversations and agent state

4. **WebSocket for real-time updates**
   - Deferred: HTMX polling is sufficient for now, can add later if needed

## Implementation

Files to modify:
- `src/xpressai/memory/database.py` - Add migration v7
- `src/xpressai/web/app.py` - Add endpoints
- `src/xpressai/web/templates/agent_chat.html` - 3-column layout
- `src/xpressai/web/static/style.css` - New styles
