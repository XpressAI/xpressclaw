"""MCP stdio server for xpressclaw agent-published apps.

Provides tools for agents to create app workspaces, develop iteratively,
and publish web apps that appear in the xpressclaw UI.

Workflow:
  1. create_app_workspace — sets up directory + scaffold at /workspace/apps/{name}/
  2. Agent writes code using filesystem/shell tools
  3. preview_app — starts a dev server in the workspace for testing (future)
  4. publish_app — deploys the app as a container, visible in the UI

Environment variables:
  XPRESSCLAW_URL  — Base URL of the xpressclaw server
  AGENT_ID        — Current agent's ID
"""

import json
import os
import sys

import httpx

BASE_URL = os.environ.get(
    "XPRESSCLAW_URL",
    f"http://host.docker.internal:{os.environ.get('XPRESSCLAW_PORT', '8935')}",
)
AGENT_ID = os.environ.get("AGENT_ID", "")
WORKSPACE = os.environ.get("WORKSPACE_DIR", "/workspace")

TOOLS = [
    {
        "name": "create_app_workspace",
        "description": (
            "Create a workspace directory for developing a new app. "
            "This sets up the directory structure and optional scaffold "
            "(package.json for Node, requirements.txt for Python, or index.html for static). "
            "After creating the workspace, use filesystem and shell tools to write your app code. "
            "When the app is ready, call publish_app to deploy it."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Unique app identifier (lowercase, no spaces, e.g. 'stocks')",
                },
                "type": {
                    "type": "string",
                    "enum": ["node", "python", "static"],
                    "description": "App type determines the scaffold and runtime. 'node' creates package.json, 'python' creates requirements.txt, 'static' creates index.html",
                    "default": "node",
                },
                "title": {
                    "type": "string",
                    "description": "Display name for the app (e.g. 'Stock Portfolio')",
                },
                "description": {
                    "type": "string",
                    "description": "What this app will do",
                },
            },
            "required": ["name"],
        },
    },
    {
        "name": "publish_app",
        "description": (
            "Deploy an app from a workspace directory. The app will be containerized "
            "and appear in the xpressclaw sidebar. Call create_app_workspace first "
            "to set up the directory, write your code, then call this to deploy."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "App identifier (must match the workspace name)",
                },
                "title": {
                    "type": "string",
                    "description": "Display name shown in the sidebar",
                },
                "icon": {
                    "type": "string",
                    "description": "Emoji icon for the sidebar (e.g. '📈')",
                },
                "port": {
                    "type": "integer",
                    "description": "Port the app server listens on (default 3000)",
                    "default": 3000,
                },
                "start_command": {
                    "type": "string",
                    "description": "Command to start the server (e.g. 'node server.js' or 'python app.py')",
                },
                "description": {
                    "type": "string",
                    "description": "What this app does (shown in the app header)",
                },
            },
            "required": ["name", "title", "start_command"],
        },
    },
    {
        "name": "list_apps",
        "description": "List all published apps.",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
    {
        "name": "delete_app",
        "description": "Delete a published app and stop its container.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "App identifier to delete",
                },
            },
            "required": ["name"],
        },
    },
    {
        "name": "get_app_logs",
        "description": (
            "Get the container logs for a published app. "
            "Use to debug why an app is in an error state or not working."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "App identifier",
                },
            },
            "required": ["name"],
        },
    },
    {
        "name": "office_run",
        "description": (
            "Run a script against an Office application (Word, Excel, PowerPoint). "
            "On macOS, provide AppleScript. On Windows, provide PowerShell. "
            "Use $DOCUMENTS_DIR in your script to reference the documents folder. "
            "Files are stored in a managed documents directory — use file_name not full paths."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "enum": ["word", "excel", "powerpoint"],
                    "description": "Which Office application to use",
                },
                "script": {
                    "type": "string",
                    "description": "The script to execute. Use $DOCUMENTS_DIR for the documents folder path.",
                },
                "file_name": {
                    "type": "string",
                    "description": "Document file name (e.g. 'report.docx'). Resolved to the documents directory.",
                },
            },
            "required": ["app", "script"],
        },
    },
    {
        "name": "office_read",
        "description": (
            "Read the text content of a document (docx, xlsx, pptx) "
            "from the documents directory."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "file_name": {
                    "type": "string",
                    "description": "Document file name (e.g. 'report.docx')",
                },
            },
            "required": ["file_name"],
        },
    },
    {
        "name": "office_export",
        "description": (
            "Export a document to a different format (e.g., docx to PDF). "
            "Both source and output are in the documents directory."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "file_name": {
                    "type": "string",
                    "description": "Source document name (e.g. 'report.docx')",
                },
                "format": {
                    "type": "string",
                    "enum": ["pdf", "html"],
                    "description": "Target format",
                },
                "output_name": {
                    "type": "string",
                    "description": "Output file name (defaults to same name with new extension)",
                },
            },
            "required": ["file_name", "format"],
        },
    },
    {
        "name": "list_documents",
        "description": "List all documents in the documents directory.",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
    {
        "name": "browser_screenshot",
        "description": (
            "Take a screenshot of a web page using Playwright. "
            "The screenshot is saved to the agent's screenshots directory."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL to screenshot"},
                "file_name": {"type": "string", "description": "Output file name (default: screenshot.png)"},
                "wait_for": {"type": "string", "description": "CSS selector to wait for before screenshot"},
                "full_page": {"type": "boolean", "description": "Capture full page (default: false)"},
            },
            "required": ["url"],
        },
    },
    {
        "name": "browser_fetch",
        "description": (
            "Navigate to a URL and extract text content using a real browser (Playwright). "
            "Unlike HTTP fetch, this renders JavaScript and dynamic content."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL to fetch"},
                "selector": {"type": "string", "description": "CSS selector to extract text from (optional)"},
                "wait_for": {"type": "string", "description": "CSS selector to wait for before extracting"},
            },
            "required": ["url"],
        },
    },
    {
        "name": "browser_run",
        "description": (
            "Run a custom Playwright Python script on the host machine. "
            "Use $SCREENSHOTS_DIR in your script for the screenshots output path. "
            "The script should use playwright.sync_api."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "Python script using playwright.sync_api",
                },
            },
            "required": ["script"],
        },
    },
    {
        "name": "get_agent_logs",
        "description": (
            "Get your own agent container logs. "
            "Useful for debugging startup issues or errors."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
]


# --- Scaffolds ---

NODE_PACKAGE_JSON = """{
  "name": "%(name)s",
  "version": "1.0.0",
  "private": true,
  "scripts": {
    "start": "node server.js"
  },
  "dependencies": {
    "express": "^4.18.0"
  }
}
"""

NODE_SERVER_JS = """const express = require('express');
const app = express();
const port = process.env.PORT || 3000;

app.use(express.static('public'));
app.use(express.json());

app.get('/', (req, res) => {
  res.sendFile(__dirname + '/public/index.html');
});

app.listen(port, '0.0.0.0', () => {
  console.log(`%(title)s running on port ${port}`);
});
"""

PYTHON_APP_PY = """from http.server import HTTPServer, SimpleHTTPRequestHandler
import os

port = int(os.environ.get('PORT', 3000))

class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory='public', **kwargs)

print(f'%(title)s running on port {port}')
HTTPServer(('0.0.0.0', port), Handler).serve_forever()
"""

INDEX_HTML = """<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>%(title)s</title>
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
           background: #0f172a; color: #e2e8f0; min-height: 100vh;
           display: flex; align-items: center; justify-content: center; }
    .container { text-align: center; padding: 2rem; }
    h1 { font-size: 1.5rem; margin-bottom: 0.5rem; }
    p { color: #94a3b8; }
  </style>
</head>
<body>
  <div class="container">
    <h1>%(title)s</h1>
    <p>Edit public/index.html to build your app.</p>
  </div>
</body>
</html>
"""


def _api(method: str, path: str, body: dict | None = None) -> dict:
    url = f"{BASE_URL}/api{path}"
    with httpx.Client(timeout=30) as client:
        if method == "GET":
            r = client.get(url)
        elif method == "POST":
            r = client.post(url, json=body)
        elif method == "DELETE":
            r = client.delete(url)
        else:
            raise ValueError(f"unsupported method: {method}")
        r.raise_for_status()
        return r.json()


def handle_tool(name: str, arguments: dict) -> str:
    if name == "create_app_workspace":
        app_name = arguments["name"]
        app_type = arguments.get("type", "node")
        title = arguments.get("title", app_name)
        desc = arguments.get("description", "")

        app_dir = os.path.join(WORKSPACE, "apps", app_name)
        public_dir = os.path.join(app_dir, "public")
        os.makedirs(public_dir, exist_ok=True)

        ctx = {"name": app_name, "title": title}

        # Write scaffold based on type
        if app_type == "node":
            _write_if_missing(os.path.join(app_dir, "package.json"), NODE_PACKAGE_JSON % ctx)
            _write_if_missing(os.path.join(app_dir, "server.js"), NODE_SERVER_JS % ctx)
            _write_if_missing(os.path.join(public_dir, "index.html"), INDEX_HTML % ctx)
            start_hint = "node server.js (after npm install)"
        elif app_type == "python":
            _write_if_missing(os.path.join(app_dir, "app.py"), PYTHON_APP_PY % ctx)
            _write_if_missing(os.path.join(app_dir, "requirements.txt"), "")
            _write_if_missing(os.path.join(public_dir, "index.html"), INDEX_HTML % ctx)
            start_hint = "python app.py"
        else:  # static
            _write_if_missing(os.path.join(public_dir, "index.html"), INDEX_HTML % ctx)
            start_hint = "python -m http.server $PORT --directory public"

        lines = [
            f"App workspace created at: {app_dir}",
            f"Type: {app_type}",
            f"",
            f"Directory structure:",
            f"  {app_dir}/",
        ]
        for root, dirs, files in os.walk(app_dir):
            level = root.replace(app_dir, "").count(os.sep)
            indent = "  " + "  " * (level + 1)
            for f in files:
                lines.append(f"{indent}{f}")

        lines.extend([
            f"",
            f"Next steps:",
            f"  1. Edit the files in {app_dir} to build your app",
            f"  2. The app should serve HTTP on the PORT environment variable (default 3000)",
            f"  3. When ready, call publish_app with:",
            f"     name: '{app_name}'",
            f"     title: '{title}'",
            f"     start_command: '{start_hint}'",
        ])

        return "\n".join(lines)

    elif name == "publish_app":
        app_name = arguments["name"]
        source_dir = os.path.join(WORKSPACE, "apps", app_name)

        if not os.path.isdir(source_dir):
            raise ValueError(
                f"App directory not found: {source_dir}\n"
                f"Call create_app_workspace first to set up the directory."
            )

        body = {
            "id": app_name,
            "title": arguments["title"],
            "icon": arguments.get("icon"),
            "description": arguments.get("description"),
            "agent_id": AGENT_ID,
            "port": arguments.get("port", 3000),
            "source_dir": source_dir,
            "start_command": arguments.get("start_command"),
        }
        _api("POST", "/apps/publish", body)
        return (
            f"Published app '{arguments['title']}' (id: {app_name}).\n"
            f"It will appear in the Apps section of the sidebar.\n"
            f"The user can click on it to view it in the UI."
        )

    elif name == "list_apps":
        apps = _api("GET", "/apps")
        if not apps:
            return "No apps published yet."
        lines = []
        for app in apps:
            status = app.get("status", "unknown")
            lines.append(
                f"- {app['title']} ({app['id']}) [{status}] v{app.get('source_version', 1)}"
            )
        return "\n".join(lines)

    elif name == "delete_app":
        _api("DELETE", f"/apps/{arguments['name']}")
        return f"Deleted app '{arguments['name']}'."

    elif name == "office_run":
        body = {
            "app": arguments["app"],
            "script": arguments["script"],
            "file_name": arguments.get("file_name"),
            "agent_id": AGENT_ID,
        }
        result = _api("POST", "/office/run", body)
        if result.get("success"):
            output = result.get("output", "Script executed successfully.")
            docs_dir = result.get("documents_dir", "")
            return f"{output}\n\nDocuments directory: {docs_dir}"
        else:
            error = result.get("error", "Unknown error")
            return f"Script error: {error}\n\nTry adjusting the script syntax and retrying."

    elif name == "office_read":
        result = _api("POST", "/office/read", {"file_name": arguments["file_name"], "agent_id": AGENT_ID})
        if result.get("success") is False:
            return f"Error reading document: {result.get('error', 'unknown')}"
        return result.get("content", "No content extracted.")

    elif name == "office_export":
        body = {
            "file_name": arguments["file_name"],
            "format": arguments["format"],
            "output_name": arguments.get("output_name"),
            "agent_id": AGENT_ID,
        }
        result = _api("POST", "/office/export", body)
        if result.get("success") is False:
            return f"Export error: {result.get('error', 'unknown')}"
        return f"Exported to: {result.get('exported', 'unknown')}"

    elif name == "list_documents":
        docs = _api("GET", f"/office/documents?agent_id={AGENT_ID}")
        if not docs:
            return "No documents in the documents directory."
        lines = []
        for d in docs:
            size = d.get("size", 0)
            size_str = f"{size / 1024:.1f} KB" if size > 1024 else f"{size} bytes"
            lines.append(f"- {d['name']} ({size_str})")
        return "\n".join(lines)

    elif name == "browser_screenshot":
        body = {
            "url": arguments["url"],
            "file_name": arguments.get("file_name"),
            "wait_for": arguments.get("wait_for"),
            "full_page": arguments.get("full_page"),
            "agent_id": AGENT_ID,
        }
        result = _api("POST", "/browser/screenshot", body)
        if result.get("success"):
            return f"Screenshot saved: {result.get('file', 'screenshot.png')}"
        return f"Screenshot error: {result.get('error', 'unknown')}"

    elif name == "browser_fetch":
        body = {
            "url": arguments["url"],
            "selector": arguments.get("selector"),
            "wait_for": arguments.get("wait_for"),
            "agent_id": AGENT_ID,
        }
        result = _api("POST", "/browser/fetch", body)
        if result.get("success"):
            return result.get("content", "No content extracted.")
        return f"Fetch error: {result.get('error', 'unknown')}"

    elif name == "browser_run":
        body = {
            "script": arguments["script"],
            "agent_id": AGENT_ID,
        }
        result = _api("POST", "/browser/run", body)
        if result.get("success"):
            return result.get("output", "Script executed successfully.")
        return f"Script error: {result.get('error', 'unknown')}"

    elif name == "get_app_logs":
        result = _api("GET", f"/apps/{arguments['name']}/logs")
        logs = result.get("logs", "No logs available.")
        return f"App '{arguments['name']}' logs:\n{logs}"

    elif name == "get_agent_logs":
        result = _api("GET", f"/agents/{AGENT_ID}/logs")
        logs = result.get("logs", "No logs available.")
        return f"Agent logs:\n{logs}"

    raise ValueError(f"unknown tool: {name}")


def _write_if_missing(path: str, content: str):
    """Write a file only if it doesn't exist (don't overwrite on re-scaffold)."""
    if not os.path.exists(path):
        with open(path, "w") as f:
            f.write(content)


# --- MCP stdio protocol ---

def _read_message():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line.strip())


def _write_message(obj: dict):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
    body = json.dumps(obj)
    header = f"Content-Length: {len(body)}\r\n\r\n"


def _response(msg_id, result):
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def _error_response(msg_id, code, message):
    return {"jsonrpc": "2.0", "id": msg_id, "error": {"code": code, "message": message}}


def main():
    while True:
        msg = _read_message()
        if msg is None:
            break

        msg_id = msg.get("id")
        method = msg.get("method", "")
        params = msg.get("params", {})

        if method == "initialize":
            _write_message(
                _response(
                    msg_id,
                    {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}},
                        "serverInfo": {
                            "name": "xpressclaw-apps",
                            "version": "0.1.0",
                        },
                    },
                )
            )
        elif method == "notifications/initialized":
            pass
        elif method == "tools/list":
            _write_message(_response(msg_id, {"tools": TOOLS}))
        elif method == "tools/call":
            tool_name = params.get("name", "")
            arguments = params.get("arguments", {})
            try:
                result_text = handle_tool(tool_name, arguments)
                _write_message(
                    _response(
                        msg_id,
                        {
                            "content": [{"type": "text", "text": result_text}],
                            "isError": False,
                        },
                    )
                )
            except Exception as e:
                _write_message(
                    _response(
                        msg_id,
                        {
                            "content": [{"type": "text", "text": f"Error: {e}"}],
                            "isError": True,
                        },
                    )
                )
        elif method == "notifications/cancelled":
            pass
        else:
            if msg_id is not None:
                _write_message(
                    _error_response(msg_id, -32601, f"method not found: {method}")
                )


if __name__ == "__main__":
    main()
