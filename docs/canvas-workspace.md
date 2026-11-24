# Spatial Canvas Workspace

XpressClaw offers an optional **Spatial Canvas** view alongside the default tab-and-pane workspace. Switch between **Tabs** and **Canvas** at any time from the sidebar toggle (or open `/canvas` directly); both layouts are persisted independently in browser storage.

## Spatial organization & delegation chains

The canvas provides an infinite pan-and-zoom workspace inspired by GameMaker's spatial IDE:

- **Project zones & isolation**: Windows belonging to the same Project group into labeled spatial zones. Drag any project zone's top-left header handle to move all windows in that project together as a rigid group. Use the top project pills to isolate a single Project—hiding and deactivating other projects' windows to keep large canvases responsive—or click **All Projects** to show everything again.
- **Live delegation & dependency wires**: Parent tasks connect to delegated child tasks (`parent_task_id`) and prerequisite dependencies (`depends_on`) with live Bezier curves colored by execution state. Enable **Auto-pin delegated** (on by default) to surface newly delegated parallel subtasks next to their parent automatically, or click the delegation badge on any Task window to fan them out on demand.
- **Layout controls**: Drag windows by their title bar, drag project zone headers to reposition an entire project cluster, resize from edges/corners, collapse windows to compact headers, snap to the 20px grid, **Tidy** to auto-arrange by Project and delegation depth, **Fit** to frame visible windows, or navigate quickly with the **Minimap** (click/drag to pan, scroll to zoom).

## Contextual tool windows

From any Project, Agent, Task, or Conversation window's `+` menu (or the top **Add** palette), you can open connected windows right beside the work they relate to:

- **Task & Conversation Chats**: Live interactive Task and Conversation views with streaming updates and delegation controls.
- **Code Editors**: Monaco editor with collapsible workspace file tree, Code/Diff view toggle, and save support. Clicking an image file opens a connected Image Viewer; clicking **Preview App** on an HTML file opens a connected Web App preview.
- **Terminals**: Live `TerminalPanel` connected to the Agent's retained container and tmux sessions.
- **Images**: Read-only preview of workspace images (served via `GET /api/workspaces/{agent_id}/raw` with capability-safe workspace checks, `X-Content-Type-Options: nosniff`, and `Content-Security-Policy: default-src 'none'; sandbox`) or direct attachment/image URLs with zoom and transparency-grid controls.
- **Web Apps**: Embed live previews from:
  - an Agent's exposed `container_to_host` port forwards (including quick inline port exposure),
  - workspace `.html` files rendered in a sandboxed Blob iframe (with a quick **Edit** button to open a connected Code Editor),
  - HTML task/conversation attachments rendered in a sandboxed Blob iframe, or
  - local/HTTPS preview URLs (control-plane API endpoints are blocked and same-origin URLs never combine `allow-same-origin` with `allow-scripts`).
- **Inline link previews from Task & Conversation windows**: Clicking a file link, HTML/image attachment, or web URL inside a Task, Conversation, or Agent window opens a contextual popover offering **Preview in Canvas Image Viewer**, **Preview in Canvas App**, **Open in Canvas Editor** (for workspace files), or the default **Open / Download** action. Spawned preview windows tether directly to the window you clicked from. Hold `Ctrl`/`⌘` while clicking to bypass the popover and open/download directly.

## Navigation controls

- **Pan**: Drag the canvas background, hold `Space` and drag anywhere, middle-mouse drag, or scroll on the empty canvas background.
- **Zoom**: `Ctrl`/`⌘` + scroll wheel, scroll wheel over the **Minimap**, trackpad pinch, or the toolbar zoom buttons (`-`, `100%`, `+`, **Fit**).
- **Pop out to Tabs**: Click the external-tab icon on any routed window to switch back to Tabs view at that resource.
