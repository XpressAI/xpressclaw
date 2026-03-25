# ADR-015: SvelteKit Web UI

## Status
Accepted (supersedes ADR-007 and ADR-013)

## Context

ADR-007 specified an HTMX + Jinja2 + FastAPI architecture for the web UI, and ADR-013 extended it with enhanced chat features. During the Rust rewrite, the backend moved to Axum (Rust) and the frontend was rebuilt as a SvelteKit SPA. The HTMX approach was abandoned for several reasons:

1. **Backend is now Rust (Axum)**, not Python — Jinja2 templates are not idiomatic
2. **Tauri desktop app** requires a static frontend that can be embedded via `rust-embed`
3. **Chat streaming** needs fine-grained control over SSE parsing and state that HTMX couldn't provide
4. **Complex forms** (agent config editor, setup wizard) benefit from client-side state management
5. **Svelte 5 runes** (`$state`, `$derived`, `$effect`) provide reactivity without framework weight

## Decision

The web UI is a **SvelteKit static SPA** that:
- Builds to static HTML/JS/CSS via `@sveltejs/adapter-static`
- Is embedded into the Rust binary via `rust-embed` at compile time
- Communicates with the Axum backend exclusively through a REST JSON API
- Streams agent responses via Server-Sent Events (SSE) with custom parsing
- Runs identically in the browser (development) and inside the Tauri desktop app

### Tech Stack

| Layer | Technology |
|-------|-----------|
| Framework | SvelteKit 2 with Svelte 5 |
| Styling | Tailwind CSS 3 with CSS custom properties (HSL theme) |
| Icons | Lucide Svelte + Unicode |
| Markdown | marked + DOMPurify |
| Build | Vite → static adapter → `frontend/build/` |
| Embedding | rust-embed compiles `frontend/build/` into the server binary |

### Application Structure

```
frontend/
├── src/
│   ├── routes/                    # SvelteKit file-based routing
│   │   ├── +layout.svelte         # Sidebar + main content shell
│   │   ├── +page.svelte           # Home (new conversation launcher)
│   │   ├── agents/
│   │   │   ├── +page.svelte       # Agent list (card grid)
│   │   │   └── [id]/+page.svelte  # Agent config editor
│   │   ├── conversations/
│   │   │   └── [id]/+page.svelte  # Chat interface
│   │   ├── dashboard/             # Overview stats
│   │   ├── tasks/                 # Task board + detail view
│   │   ├── memory/                # Knowledge search
│   │   ├── schedules/             # Cron management
│   │   ├── procedures/            # SOP library
│   │   ├── budget/                # Spending report
│   │   ├── settings/              # System config + user profile
│   │   └── setup/                 # First-run wizard
│   ├── lib/
│   │   ├── api.ts                 # REST client (all endpoints)
│   │   └── utils.ts               # Helpers (timeAgo, agentAvatar, etc.)
│   └── app.css                    # Theme variables + prose-chat styles
├── static/
│   └── avatars/                   # 32 agent profile images (64x64)
├── tailwind.config.ts
└── svelte.config.js
```

### Theme System

Dark-first design using CSS custom properties in HSL format. The `.dark` class (always set) defines a navy/indigo palette:

```css
.dark {
    --background: 228 25% 8%;      /* deep navy */
    --card: 228 22% 11%;           /* slightly lighter */
    --primary: 225 65% 55%;        /* blue accent */
    --muted-foreground: 225 15% 55%;
    --sidebar: 228 25% 10%;
    --sidebar-active: 225 50% 30%;
    --bubble-user: 260 60% 45%;    /* purple */
    --bubble-agent: 228 22% 15%;   /* dark navy */
}
```

Tailwind maps these to semantic names (`bg-background`, `text-foreground`, `bg-primary`, etc.) so components use intent, not color values.

### API Client Pattern

All backend communication goes through `lib/api.ts`:

```typescript
export const agents = {
    list: () => request<Agent[]>('/api/agents'),
    get: (id: string) => request<Agent>(`/api/agents/${id}`),
    start: (id: string) => request('/api/agents/${id}/start', { method: 'POST' }),
    // ...
};
```

No direct `fetch()` calls in components — everything goes through the typed API module.

### Chat Streaming

The conversation page uses SSE for real-time agent responses:

1. `POST /api/conversations/{id}/messages/stream` with the user message
2. Server returns an SSE stream with events: `user_message`, `thinking`, `chunk`, `agent_message`, `error`, `done`
3. Frontend parses these incrementally, updating `$state` variables
4. Markdown is rendered via `marked`, with custom handling for `<think>` blocks (collapsible reasoning) and `<tool_call>` blocks (collapsible tool invocations)
5. `@mention` picker for multi-agent conversations

### Sidebar Layout

```
┌──────────────┬────────────────────────────┐
│ Logo         │                            │
│              │                            │
│ APPS         │                            │
│  Dashboard   │     Main Content           │
│              │     (route-dependent)       │
│ CONVERSATIONS│                            │
│  conv1       │                            │
│  conv2       │                            │
│              │                            │
│ AGENTS       │                            │
│  agent1 🟢   │                            │
│  agent2 🔴   │                            │
│              │                            │
│ Knowledge    │                            │
│ Procedures   │                            │
│ Settings     │                            │
│              │                            │
│ ┌──┬──┬──┬──┐│                            │
│ │Ap│Ta│Sc│Bu││                            │
│ └──┴──┴──┴──┘│                            │
└──────────────┴────────────────────────────┘
```

- **Apps**: Dashboard (future: agent-published apps)
- **Conversations**: Multi-participant chat sessions with emoji icons
- **Agents**: Avatars with colored ring border (green=running, amber=starting, red=stopped)
- **Bottom bar**: Quick nav to Apps, Tasks, Schedules, Budget

### User Profile

Stored server-side in the SQLite `config` table (key: `user_profile`) via `PUT /api/settings/profile`. Contains name and avatar (uploaded image resized to 128x128 JPEG data URI).

### Desktop Integration (Tauri)

The Tauri app opens a window pointing to `http://localhost:8935` (the Axum server). This means:
- The frontend runs from the embedded `rust-embed` build, served by Axum
- `data-tauri-drag-region` and `-webkit-app-region: drag` do NOT work (external URL)
- Native window decorations are used with `hiddenTitle: true`
- The sidecar CLI binary is bundled and launched by Tauri on startup

### Build Pipeline

```
build.sh:
  1. bazel build → CLI binary (includes core + server + embedded frontend)
  2. Copy CLI binary as Tauri sidecar
  3. npx @tauri-apps/cli build → .app bundle with sidecar
  4. Docker build → agent harness images
```

**Important**: `build.rs` in the Tauri crate must NOT overwrite the Bazel-built sidecar. It only copies from Cargo's `target/` if no sidecar already exists (for `cargo tauri dev` workflows).

## Consequences

### Positive
- Full client-side reactivity without page reloads
- Type-safe API client catches mismatches at build time
- Static build embeds cleanly via rust-embed — single binary deployment
- Svelte 5 runes eliminate boilerplate (no stores, no subscriptions)
- Tailwind + CSS variables enable consistent theming
- Same frontend works in browser, Tauri, and embedded

### Negative
- Requires Node.js toolchain for frontend development
- Frontend build step adds time to CI (~30s)
- No progressive enhancement — JavaScript required
- SSE parsing is custom code that must be maintained

### Trade-offs vs HTMX (ADR-007)
- More complex build, but richer interactivity
- Client-side state management needed, but enables complex forms (agent editor, wizard)
- JavaScript required, but desktop app requires it anyway (Tauri webview)

## Related ADRs
- ADR-007: HTMX Web UI (superseded by this ADR)
- ADR-013: Enhanced Agent Chat (superseded — chat features are now in SvelteKit)
- ADR-008: Textual TUI (alternative terminal interface, independent)
