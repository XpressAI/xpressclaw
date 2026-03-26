"""MCP stdio server for reading xpressclaw skills.

Agents see a list of available skills in their system prompt.
They call read_skill to load the full instructions when needed.

Environment variables:
  XPRESSCLAW_URL  — Base URL of the xpressclaw server
"""

import json
import os
import sys

import httpx

BASE_URL = os.environ.get(
    "XPRESSCLAW_URL",
    f"http://host.docker.internal:{os.environ.get('XPRESSCLAW_PORT', '8935')}",
)

TOOLS = [
    {
        "name": "read_skill",
        "description": (
            "Load the full instructions for a skill. "
            "Your system prompt lists available skills by name. "
            "Call this tool to read the detailed instructions before using a skill."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name (e.g. 'build-app')",
                },
            },
            "required": ["name"],
        },
    },
    {
        "name": "list_skills",
        "description": "List all available skills with their descriptions.",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
]


def _api(method: str, path: str) -> dict:
    url = f"{BASE_URL}/api{path}"
    with httpx.Client(timeout=10) as client:
        r = client.get(url) if method == "GET" else client.post(url)
        r.raise_for_status()
        return r.json()


def handle_tool(name: str, arguments: dict) -> str:
    if name == "read_skill":
        skill_name = arguments["name"]
        result = _api("GET", f"/skills/{skill_name}")
        return result.get("content", f"Skill '{skill_name}' not found.")

    elif name == "list_skills":
        skills = _api("GET", "/skills")
        if not skills:
            return "No skills available."
        lines = []
        for s in skills:
            lines.append(f"- **{s['name']}**: {s.get('description', '')}")
        return "\n".join(lines)

    raise ValueError(f"unknown tool: {name}")


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
                            "name": "xpressclaw-skills",
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
