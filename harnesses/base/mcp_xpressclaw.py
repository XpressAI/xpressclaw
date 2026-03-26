"""Unified MCP stdio server for all xpressclaw tools.

Combines tasks, memory, skills, and apps into a single MCP server
to work around Claude CLI's MCP server connection limits.

Environment variables:
  XPRESSCLAW_URL  — Base URL of the xpressclaw server
  AGENT_ID        — Current agent's ID
  WORKSPACE_DIR   — Agent workspace directory
"""

import json
import os
import sys

# Import all tool modules
sys.path.insert(0, os.path.dirname(__file__))
import mcp_tasks
import mcp_memory
import mcp_skills
import mcp_apps

# Merge all tools from all modules
ALL_TOOLS = (
    mcp_tasks.TOOLS +
    mcp_memory.TOOLS +
    mcp_skills.TOOLS +
    mcp_apps.TOOLS
)

# Map tool names to their handler modules
HANDLERS = {}
for tool in mcp_tasks.TOOLS:
    HANDLERS[tool["name"]] = mcp_tasks.handle_tool
for tool in mcp_memory.TOOLS:
    HANDLERS[tool["name"]] = mcp_memory.handle_tool
for tool in mcp_skills.TOOLS:
    HANDLERS[tool["name"]] = mcp_skills.handle_tool
for tool in mcp_apps.TOOLS:
    HANDLERS[tool["name"]] = mcp_apps.handle_tool


def handle_tool(name: str, arguments: dict) -> str:
    handler = HANDLERS.get(name)
    if handler is None:
        raise ValueError(f"unknown tool: {name}")
    return handler(name, arguments)


# --- MCP stdio protocol (newline-delimited JSON) ---

def _read_message():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line.strip())


def _write_message(obj: dict):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


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
                            "name": "xpressclaw",
                            "version": "0.1.0",
                        },
                    },
                )
            )
        elif method == "notifications/initialized":
            pass
        elif method == "tools/list":
            _write_message(_response(msg_id, {"tools": ALL_TOOLS}))
        elif method == "tools/call":
            tool_name = params.get("name", "")
            arguments = params.get("arguments", {})
            try:
                result_text = handle_tool(tool_name, arguments)
                _write_message(
                    _response(
                        msg_id,
                        {
                            "content": [{"type": "text", "text": str(result_text)}],
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
