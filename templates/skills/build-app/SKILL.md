---
name: build-app
description: Build and publish a web app for the user. Use when asked to create an app, dashboard, tracker, tool, or any interactive web interface. The app runs in a container and appears in the xpressclaw sidebar.
---

# Building Apps

**IMPORTANT: When the user asks you to create, build, or make any app, website, dashboard, tracker, tool, or interactive web interface — you MUST use the `create_app_workspace` tool first, then `publish_app` when done. Do NOT write files directly with the Write tool. The app system handles containerization and deployment.**

You can create web apps that the user interacts with directly in the xpressclaw UI. Apps appear in the sidebar under "Apps" and are displayed via iframe.

## Tools Available

- `create_app_workspace(name, type, title)` — Creates `/workspace/apps/{name}/` with scaffold files
- `publish_app(name, title, icon, start_command)` — Deploys the app as a container
- `list_apps()` — Show all published apps
- `delete_app(name)` — Remove an app
- `get_app_logs(name)` — Get container logs to debug errors

## Step-by-Step Process

1. Call `create_app_workspace` with a name (lowercase, no spaces), type (`node`, `python`, or `static`), and a title.
2. The workspace is created at `/workspace/apps/{name}/` with starter files. **Always write code to this path.**
3. Build your app by editing files in the workspace. The app must:
   - Serve HTTP on the port from the `PORT` environment variable (default 3000)
   - Listen on `0.0.0.0` (not `localhost`)
   - Have a `public/` directory for static assets
4. For Node.js: run `cd /workspace/apps/{name} && npm install` to install dependencies.
5. Call `publish_app` with the name, title, emoji icon, and start command.
6. If the app shows as "error", call `get_app_logs(name)` to see what went wrong.

## Rules

- **ALWAYS use `/workspace/apps/{name}/` as the directory** — never `/home/agent/` or anywhere else.
- The app runs in its own isolated container after publishing.
- **ALWAYS use the xpressclaw dark theme** (see Design System below).
- Include all dependencies in `package.json` or `requirements.txt`.
- The app container shares the agent workspace volume — source is at `/workspace/apps/{name}/`.

## Design System

Apps are displayed inside xpressclaw's dark UI via iframe. **Always use this dark theme** so the app blends seamlessly with the parent interface. Never use light backgrounds.

### Colors

| Role | Color | Usage |
|------|-------|-------|
| Background | `#0f172a` | Page/body background |
| Surface | `#1e293b` | Cards, panels, input backgrounds |
| Surface elevated | `#283548` | Hover states, elevated cards |
| Border | `#334155` | Borders, dividers |
| Text primary | `#e2e8f0` | Main text |
| Text secondary | `#94a3b8` | Labels, descriptions, placeholders |
| Text muted | `#64748b` | Disabled text, timestamps |
| Accent/Primary | `#3b82f6` | Buttons, links, active states, focus rings |
| Accent hover | `#2563eb` | Button hover |
| Success | `#22c55e` | Completed, positive indicators |
| Warning | `#f59e0b` | Warnings, amber indicators |
| Danger | `#ef4444` | Delete buttons, errors |
| Danger bg | `#2d1f1f` | Danger button hover background |

### CSS Starter

```css
* { box-sizing: border-box; margin: 0; padding: 0; }

body {
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
  background: #0f172a;
  color: #e2e8f0;
  min-height: 100vh;
}

.container {
  max-width: 480px;
  margin: 0 auto;
  padding: 24px 16px;
}

input, select, textarea {
  background: #1e293b;
  border: 2px solid #334155;
  border-radius: 10px;
  color: #e2e8f0;
  padding: 10px 14px;
  font-size: 14px;
  outline: none;
  transition: border-color 0.2s;
}

input:focus, select:focus, textarea:focus {
  border-color: #3b82f6;
}

input::placeholder { color: #64748b; }

button {
  background: #3b82f6;
  color: white;
  border: none;
  border-radius: 10px;
  padding: 10px 18px;
  cursor: pointer;
  font-size: 14px;
  transition: background 0.2s;
}

button:hover { background: #2563eb; }

.card {
  background: #1e293b;
  border: 1px solid #334155;
  border-radius: 12px;
  padding: 16px;
}
```

### Typography

- Font: `-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif`
- Headings: `#f1f5f9`, semibold
- Body: `#e2e8f0`, 14-15px
- Small/labels: `#94a3b8`, 12-13px
- No border-radius larger than 12px

## Example

```
User: "Make me a click counter app"

1. create_app_workspace(name="counter", type="node", title="Click Counter")
2. Edit /workspace/apps/counter/server.js — Express server with /api/count endpoint
3. Edit /workspace/apps/counter/public/index.html — Button + count display (USE DARK THEME)
4. cd /workspace/apps/counter && npm install
5. publish_app(name="counter", title="Click Counter", icon="🔢", start_command="node server.js")
```
