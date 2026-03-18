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

import json
import os
import sys
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
    """Load MCP server configs from the MCP_SERVERS environment variable."""
    raw = os.environ.get("MCP_SERVERS", "")
    if not raw:
        return {}
    try:
        servers = json.loads(raw)
    except json.JSONDecodeError:
        logger.warning("failed to parse MCP_SERVERS env var")
        return {}

    sdk_servers = {}
    for name, config in servers.items():
        server_type = config.get("type", "stdio")
        if server_type == "stdio" and config.get("command"):
            entry: dict = {
                "command": config["command"],
                "args": config.get("args", []),
            }
            if config.get("env"):
                entry["env"] = config["env"]
            sdk_servers[name] = entry
        elif server_type == "sse" and config.get("url"):
            sdk_servers[name] = {"url": config["url"]}
    return sdk_servers


def _build_options(model: str, system_prompt: str, mcp_servers: dict) -> ClaudeAgentOptions:
    """Build ClaudeAgentOptions for a request."""
    options = ClaudeAgentOptions(
        model=model,
        system_prompt=system_prompt or f"You are {AGENT_NAME}, an AI assistant.",
        permission_mode="bypassPermissions",
        allowed_tools=["Read", "Write", "Edit", "Bash", "Glob", "Grep"],
        cwd=WORKSPACE_DIR,
        max_turns=25,
    )
    if mcp_servers:
        options.mcp_servers = mcp_servers
    return options


def _extract_task(messages: list[dict]) -> tuple[str, str]:
    """Extract system prompt and last user message from chat messages."""
    system_prompt = ""
    task = ""
    for msg in messages:
        if msg["role"] == "system":
            system_prompt = msg["content"]
        elif msg["role"] == "user":
            task = msg["content"]
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

        Uses include_partial_messages=True to get raw Anthropic stream events
        (content_block_delta) for true token-by-token streaming.
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
        sent_role = False

        try:
            async for message in query(prompt=task, options=options):
                if isinstance(message, StreamEvent):
                    # Raw Anthropic API stream event
                    event = message.event
                    event_type = event.get("type", "")

                    if event_type == "content_block_delta":
                        delta_obj = event.get("delta", {})
                        delta_type = delta_obj.get("type", "")

                        if delta_type == "text_delta":
                            text = delta_obj.get("text", "")
                            if text:
                                delta = {"content": text}
                                if not sent_role:
                                    delta["role"] = "assistant"
                                    sent_role = True
                                yield _sse_chunk_raw(conv_id, use_model, delta, None)

                elif isinstance(message, ResultMessage):
                    if message.is_error:
                        error_text = f"Agent error: {message.result or 'unknown error'}"
                        delta = {"content": error_text}
                        if not sent_role:
                            delta["role"] = "assistant"
                        yield _sse_chunk_raw(conv_id, use_model, delta, None)
                    elif not sent_role and message.result:
                        # Fallback: send result if no stream events were received
                        delta = {"role": "assistant", "content": message.result}
                        yield _sse_chunk_raw(conv_id, use_model, delta, None)
                    # Final stop chunk
                    yield _sse_chunk_raw(conv_id, use_model, {}, "stop")
                    yield "data: [DONE]\n\n"
                    return

            # If we get here without a ResultMessage, send stop
            yield _sse_chunk_raw(conv_id, use_model, {}, "stop")
            yield "data: [DONE]\n\n"

        except Exception as e:
            logger.exception("streaming completion failed")
            error_payload = {"error": {"message": str(e), "type": "server_error"}}
            yield f"data: {json.dumps(error_payload)}\n\n"


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
