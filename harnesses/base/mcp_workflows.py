"""MCP stdio server for xpressclaw workflow and procedure management.

Provides tools for creating, listing, updating, and running workflows,
as well as creating and listing procedures (SOPs).

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

TOOLS = [
    {
        "name": "create_workflow",
        "description": (
            "Create a new workflow. Provide the workflow name and its YAML "
            "definition content. The YAML defines triggers, steps, and "
            "sub-workflows."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Workflow name (e.g. 'onboard-customer')",
                },
                "yaml_content": {
                    "type": "string",
                    "description": "YAML workflow definition content",
                },
            },
            "required": ["name", "yaml_content"],
        },
    },
    {
        "name": "update_workflow",
        "description": (
            "Update an existing workflow's YAML definition. "
            "The workflow must already exist."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "workflow_id": {
                    "type": "string",
                    "description": "Workflow ID to update",
                },
                "yaml_content": {
                    "type": "string",
                    "description": "New YAML workflow definition content",
                },
            },
            "required": ["workflow_id", "yaml_content"],
        },
    },
    {
        "name": "list_workflows",
        "description": "List all workflows.",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
    {
        "name": "get_workflow",
        "description": "Get full details of a specific workflow by ID.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workflow_id": {
                    "type": "string",
                    "description": "Workflow ID",
                },
            },
            "required": ["workflow_id"],
        },
    },
    {
        "name": "run_workflow",
        "description": (
            "Run a workflow by ID. Optionally pass trigger data as a JSON "
            "object that will be available to the workflow steps."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "workflow_id": {
                    "type": "string",
                    "description": "Workflow ID to run",
                },
                "trigger_data": {
                    "type": "object",
                    "description": "Trigger data to pass to the workflow (optional)",
                },
            },
            "required": ["workflow_id"],
        },
    },
    {
        "name": "create_procedure",
        "description": (
            "Create a new standard operating procedure (SOP). "
            "The content is a YAML string defining the procedure's steps, "
            "inputs, and outputs."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Procedure name (e.g. 'deploy-service')",
                },
                "description": {
                    "type": "string",
                    "description": "Brief description of what this procedure does",
                },
                "content": {
                    "type": "string",
                    "description": (
                        "YAML content defining the procedure steps, inputs, and outputs"
                    ),
                },
            },
            "required": ["name", "content"],
        },
    },
]


def _api(method: str, path: str, body: dict | None = None) -> dict | list:
    """Call the xpressclaw REST API."""
    url = f"{BASE_URL}/api{path}"
    with httpx.Client(timeout=30) as client:
        if method == "GET":
            r = client.get(url)
        elif method == "POST":
            r = client.post(url, json=body)
        elif method == "PUT":
            r = client.put(url, json=body)
        elif method == "DELETE":
            r = client.delete(url)
        else:
            raise ValueError(f"unsupported method: {method}")
        r.raise_for_status()
        return r.json() if r.status_code != 204 else {}


def handle_tool(name: str, arguments: dict) -> str:
    """Execute a workflow/procedure tool and return a text result."""

    if name == "create_workflow":
        body = {
            "name": arguments["name"],
            "yaml_content": arguments["yaml_content"],
        }
        wf = _api("POST", "/workflows", body)
        return (
            f"Created workflow '{wf.get('name', arguments['name'])}' "
            f"(id: {wf['id']})"
        )

    elif name == "update_workflow":
        workflow_id = arguments["workflow_id"]
        # The PUT endpoint expects name + yaml_content; fetch current name first
        current = _api("GET", f"/workflows/{workflow_id}")
        body = {
            "name": current.get("name", ""),
            "yaml_content": arguments["yaml_content"],
        }
        wf = _api("PUT", f"/workflows/{workflow_id}", body)
        return (
            f"Updated workflow '{wf.get('name', '')}' "
            f"(id: {workflow_id})"
        )

    elif name == "list_workflows":
        workflows = _api("GET", "/workflows")
        if not workflows:
            return "No workflows found."
        lines = []
        for wf in workflows:
            enabled = "enabled" if wf.get("enabled") else "disabled"
            lines.append(
                f"- [{enabled}] {wf.get('name', '?')} (id: {wf['id']})"
            )
        return f"{len(workflows)} workflow(s):\n" + "\n".join(lines)

    elif name == "get_workflow":
        wf = _api("GET", f"/workflows/{arguments['workflow_id']}")
        return json.dumps(wf, indent=2)

    elif name == "run_workflow":
        workflow_id = arguments["workflow_id"]
        trigger_data = arguments.get("trigger_data", {})
        instance = _api("POST", f"/workflows/{workflow_id}/run", trigger_data)
        instance_id = instance.get("id", "unknown")
        status = instance.get("status", "unknown")
        return (
            f"Started workflow instance (id: {instance_id}, status: {status})"
        )

    elif name == "create_procedure":
        body = {
            "name": arguments["name"],
            "content": arguments["content"],
        }
        if "description" in arguments:
            body["description"] = arguments["description"]
        proc = _api("POST", "/procedures", body)
        return (
            f"Created procedure '{proc.get('name', arguments['name'])}' "
            f"(id: {proc.get('id', 'unknown')})"
        )

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
                            "name": "xpressclaw-workflows",
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
