"""Claude Agent SDK harness.

Runs a Claude agent using the official claude-agent-sdk package.
The SDK bundles the Claude Code CLI — no separate installation required.
The agent uses the Anthropic Messages API (ANTHROPIC_BASE_URL) which the
xpressclaw server exposes at /v1/messages, routing to any configured LLM.

Environment variables:
  ANTHROPIC_API_KEY  — API key (real or placeholder for local models).
  ANTHROPIC_BASE_URL — API base URL (set by xpressclaw to route through server).
  LLM_MODEL          — Model to use (default: claude-sonnet-4-6)
  WORKSPACE_DIR      — Agent workspace (default: /workspace)
  MCP_SERVERS        — JSON dict of MCP server configs (injected by xpressclaw)
"""

import asyncio
import json
import os
import queue
import sys
import threading
import time
from uuid import uuid4

sys.path.insert(0, "/app")

from claude_agent_sdk import (
    ClaudeAgentOptions,
    ResultMessage,
    AssistantMessage,
    query,
)
try:
    from claude_agent_sdk import StreamEvent
except ImportError:
    from claude_agent_sdk.types import StreamEvent
from server import BaseHarness, logger, AGENT_ID, AGENT_NAME

LLM_MODEL = os.environ.get("LLM_MODEL", "claude-sonnet-4-6")
WORKSPACE_DIR = os.environ.get("WORKSPACE_DIR", "/workspace")


def load_mcp_servers() -> dict:
    """Load MCP server configs from the MCP_SERVERS environment variable.

    Writes a JSON config file and returns it as a path for --mcp-config.
    The SDK passes this to the CLI which manages the MCP server processes.
    """
    raw = os.environ.get("MCP_SERVERS", "")
    if not raw:
        return {}
    try:
        servers = json.loads(raw)
    except json.JSONDecodeError:
        logger.warning("failed to parse MCP_SERVERS env var")
        return {}

    # Build the mcpServers dict for the CLI config file format
    cli_servers = {}
    for name, config in servers.items():
        server_type = config.get("type", "stdio")
        if server_type == "stdio" and config.get("command"):
            entry: dict = {
                "command": config["command"],
                "args": config.get("args", []),
            }
            if config.get("env"):
                entry["env"] = config["env"]
            cli_servers[name] = entry
        elif server_type == "sse" and config.get("url"):
            cli_servers[name] = {"type": "sse", "url": config["url"]}

    if not cli_servers:
        return {}

    # Write config file for --mcp-config
    config_path = "/tmp/mcp-servers.json"
    with open(config_path, "w") as f:
        json.dump({"mcpServers": cli_servers}, f)

    logger.info("MCP config written to %s with %d servers: %s",
                config_path, len(cli_servers), list(cli_servers.keys()))

    # Return as a path string — the SDK passes it to CLI as --mcp-config <path>
    return config_path


def _build_options(model: str, system_prompt: str, mcp_servers: dict) -> ClaudeAgentOptions:
    """Build ClaudeAgentOptions for a request."""
    options = ClaudeAgentOptions(
        model=model,
        system_prompt=system_prompt or f"You are {AGENT_NAME}, an AI assistant.",
        permission_mode="bypassPermissions",
        cwd=WORKSPACE_DIR,
        max_turns=25,
    )
    if mcp_servers:
        options.mcp_servers = mcp_servers
    return options


def _extract_task(messages: list[dict]) -> tuple[str, str]:
    """Extract system prompt and build a conversation prompt from all messages.

    The Claude SDK query() takes a single prompt string. We format the full
    conversation history so the agent has context from prior exchanges.
    """
    system_prompt = ""
    parts = []
    for msg in messages:
        if msg["role"] == "system":
            system_prompt = msg["content"]
        elif msg["role"] == "user":
            parts.append(f"User: {msg['content']}")
        elif msg["role"] == "assistant":
            parts.append(f"Assistant: {msg['content']}")

    # If there's only one user message, use it directly (no "User:" prefix)
    user_messages = [m for m in messages if m["role"] == "user"]
    if len(user_messages) <= 1:
        task = user_messages[-1]["content"] if user_messages else ""
    else:
        # Multiple turns: format as conversation, ask to continue
        task = "\n\n".join(parts) + "\n\nContinue the conversation. Respond to the last user message."

    return system_prompt, task


class ClaudeSdkHarness(BaseHarness):
    """Runs a Claude Agent SDK agent per request with streaming support."""

    async def complete(
        self,
        messages: list[dict],
        model: str,
        temperature: float,
        max_tokens: int,
    ) -> str:
        system_prompt, task = _extract_task(messages)
        if not task:
            return "No task provided."

        use_model = model if model != AGENT_NAME else LLM_MODEL
        mcp_servers = load_mcp_servers()
        options = _build_options(use_model, system_prompt, mcp_servers)

        logger.info(
            "running claude agent: model=%s task_len=%d workspace=%s",
            use_model, len(task), WORKSPACE_DIR,
        )

        result_text = ""
        async for message in query(prompt=task, options=options):
            if isinstance(message, ResultMessage):
                if message.is_error:
                    return f"Agent error: {message.result or 'unknown error'}"
                result_text = message.result or ""
            elif isinstance(message, AssistantMessage):
                if hasattr(message, "content"):
                    for block in message.content:
                        if hasattr(block, "text"):
                            result_text = block.text

        return result_text or "No response from agent."

    async def _stream_response(
        self,
        messages: list,
        model: str,
        temperature: float,
        max_tokens: int,
    ):
        """Stream OpenAI-format SSE chunks from the Claude SDK query.

        Runs the Claude SDK in a separate thread with its own event loop to
        completely isolate anyio's cancel scopes from FastAPI's async context.
        """
        system_prompt, task = _extract_task(messages)
        if not task:
            yield _sse_chunk(model, {"role": "assistant", "content": "No task provided."}, "stop")
            yield "data: [DONE]\n\n"
            return

        use_model = model if model != AGENT_NAME else LLM_MODEL
        mcp_servers = load_mcp_servers()
        options = _build_options(use_model, system_prompt, mcp_servers)
        options.include_partial_messages = True

        logger.info(
            "streaming claude agent: model=%s task_len=%d workspace=%s",
            use_model, len(task), WORKSPACE_DIR,
        )

        conv_id = uuid4().hex[:16]
        q: queue.Queue[str | None] = queue.Queue()

        def _run_in_thread():
            """Run the Claude SDK query in a fresh event loop on a separate thread."""
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(_query_to_queue(q, conv_id, use_model, task, options))
            except Exception as e:
                logger.exception("query thread failed")
                error_payload = {"error": {"message": str(e), "type": "server_error"}}
                q.put(f"data: {json.dumps(error_payload)}\n\n")
            finally:
                q.put(None)  # sentinel
                # Clean up async generators before closing the loop.
                # Without this, the query() generator gets destroyed mid-flight,
                # killing the TCP connection before the server finishes reading.
                try:
                    loop.run_until_complete(loop.shutdown_asyncgens())
                except Exception:
                    pass
                loop.close()

        thread = threading.Thread(target=_run_in_thread, daemon=True)
        thread.start()

        # Yield chunks from the thread-safe queue
        while True:
            try:
                chunk = await asyncio.get_event_loop().run_in_executor(None, q.get, True, 300)
            except Exception:
                break
            if chunk is None:
                break
            yield chunk


async def _query_to_queue(
    q: "queue.Queue[str | None]",
    conv_id: str,
    model: str,
    task: str,
    options: ClaudeAgentOptions,
):
    """Run the Claude SDK query and push SSE chunks to a thread-safe queue.

    Wraps reasoning in <think> tags and tool calls in <tool_call> tags
    so the frontend can render them as collapsible panels.
    """
    sent_role = False
    in_thinking = False
    in_tool_call = False

    def _send(text: str):
        nonlocal sent_role
        delta = {"content": text}
        if not sent_role:
            delta["role"] = "assistant"
            sent_role = True
        q.put(_sse_chunk_raw(conv_id, model, delta, None))

    try:
        async for message in query(prompt=task, options=options):
            if isinstance(message, StreamEvent):
                event = message.event
                event_type = event.get("type", "")

                if event_type == "content_block_start":
                    block = event.get("content_block", {})
                    block_type = block.get("type", "")
                    if block_type == "thinking":
                        in_thinking = True
                        _send("<think>")
                    elif block_type == "tool_use":
                        tool_name = block.get("name", "tool")
                        _send(f'<tool_call name="{tool_name}">')
                        in_tool_call = True

                elif event_type == "content_block_delta":
                    delta_obj = event.get("delta", {})
                    delta_type = delta_obj.get("type", "")

                    if delta_type == "thinking_delta":
                        text = delta_obj.get("thinking", "")
                        if text:
                            _send(text)
                    elif delta_type == "text_delta":
                        text = delta_obj.get("text", "")
                        if text:
                            _send(text)
                    elif delta_type == "input_json_delta":
                        text = delta_obj.get("partial_json", "")
                        if text:
                            _send(text)

                elif event_type == "content_block_stop":
                    if in_thinking:
                        _send("</think>")
                        in_thinking = False
                    elif in_tool_call:
                        _send("</tool_call>")
                        in_tool_call = False

            elif isinstance(message, AssistantMessage):
                # Skip if we already streamed content via StreamEvent — AssistantMessage
                # contains the same text that was already sent token-by-token.
                if not sent_role and hasattr(message, "content"):
                    for block in message.content:
                        if hasattr(block, "text") and block.text:
                            _send(block.text)

            elif isinstance(message, ResultMessage):
                logger.info("ResultMessage: is_error=%s result_len=%d",
                            message.is_error, len(message.result or ""))
                if message.is_error:
                    _send(f"Agent error: {message.result or 'unknown error'}")
                elif not sent_role and message.result:
                    _send(message.result)
                q.put(_sse_chunk_raw(conv_id, model, {}, "stop"))
                q.put("data: [DONE]\n\n")
                return

    except Exception as e:
        logger.exception("_query_to_queue error")
        error_payload = {"error": {"message": str(e), "type": "server_error"}}
        q.put(f"data: {json.dumps(error_payload)}\n\n")

    # No ResultMessage
    if not sent_role:
        _send("No response from agent.")
    q.put(_sse_chunk_raw(conv_id, model, {}, "stop"))
    q.put("data: [DONE]\n\n")


def _sse_chunk_raw(conv_id: str, model: str, delta: dict, finish_reason: str | None) -> str:
    """Format a single SSE chunk in OpenAI chat.completion.chunk format."""
    payload = {
        "id": f"chatcmpl-{conv_id}",
        "object": "chat.completion.chunk",
        "created": int(time.time()),
        "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    }
    return f"data: {json.dumps(payload)}\n\n"


def _sse_chunk(model: str, delta: dict, finish_reason: str | None) -> str:
    return _sse_chunk_raw(uuid4().hex[:16], model, delta, finish_reason)


if __name__ == "__main__":
    ClaudeSdkHarness().run()
