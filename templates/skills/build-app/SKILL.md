---
name: build-app
description: Build and publish a web app for the user. Use when asked to create an app, dashboard, tracker, tool, or any interactive web interface. The app runs in a container and appears in the xpressclaw sidebar.
---

# Building Apps

You can create web apps that the user interacts with directly in the xpressclaw UI. Apps appear in the sidebar under "Apps" and are displayed via iframe.

## Tools Available

- `create_app_workspace(name, type, title)` — Creates `/workspace/apps/{name}/` with scaffold files
- `publish_app(name, title, icon, start_command)` — Deploys the app as a container
- `list_apps()` — Show all published apps
- `delete_app(name)` — Remove an app

## Step-by-Step Process

1. Call `create_app_workspace` with a name (lowercase, no spaces), type (`node`, `python`, or `static`), and a title.
2. The workspace is created at `/workspace/apps/{name}/` with starter files. **Always write code to this path.**
3. Build your app by editing files in the workspace. The app must:
   - Serve HTTP on the port from the `PORT` environment variable (default 3000)
   - Listen on `0.0.0.0` (not `localhost`)
   - Have a `public/` directory for static assets
4. For Node.js: run `cd /workspace/apps/{name} && npm install` to install dependencies.
5. Call `publish_app` with the name, title, emoji icon, and start command.

## Rules

- **ALWAYS use `/workspace/apps/{name}/` as the directory** — never `/home/agent/` or anywhere else.
- The app runs in its own isolated container after publishing.
- Match the xpressclaw dark theme: background `#0f172a`, text `#e2e8f0`, accent `#3b82f6`.
- Include all dependencies in `package.json` or `requirements.txt`.
- Keep the app self-contained — it cannot access the agent's workspace after publishing.

## Example

```
User: "Make me a click counter app"

1. create_app_workspace(name="counter", type="node", title="Click Counter")
2. Edit /workspace/apps/counter/server.js — Express server with /api/count endpoint
3. Edit /workspace/apps/counter/public/index.html — Button + count display
4. cd /workspace/apps/counter && npm install
5. publish_app(name="counter", title="Click Counter", icon="🔢", start_command="node server.js")
```
