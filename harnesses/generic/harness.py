"""Generic harness — LLM proxy with MCP tool execution.

Forwards chat completion requests to a configured LLM API endpoint.
Parses tool calls from the response and executes them via the
xpressclaw server's REST API, feeding results back for multi-turn
agentic loops.

Supports streaming: text is streamed as SSE chunks, tool calls are
executed between turns, and the final response streams back.

Environment variables:
  LLM_BASE_URL      — Base URL for the LLM API (default: http://host.docker.internal:8935/v1)
  LLM_API_KEY       — API key for the LLM API (default: none)
  LLM_MODEL         — Override model name (default: use request model)
  XPRESSCLAW_URL    — Base URL for tool execution (default: http://host.docker.internal:8935)
  AGENT_ID          — Agent identifier for tool calls
"""

import json
import os
import re
import sys
import time
from typing import Optional
from uuid import uuid4

# Add base harness to path
sys.path.insert(0, "/app")

import httpx
from server import BaseHarness, logger, AGENT_ID

LLM_BASE_URL = os.environ.get("LLM_BASE_URL", "http://host.docker.internal:8935/v1")
LLM_API_KEY = os.environ.get("LLM_API_KEY", "")
LLM_MODEL = os.environ.get("LLM_MODEL", "")
XPRESSCLAW_URL = os.environ.get("XPRESSCLAW_URL", "http://host.docker.internal:8935")
MAX_TOOL_TURNS = 15

# Import MCP tool handlers for direct execution
sys.path.insert(0, "/app")
try:
    import mcp_xpressclaw
    HAS_MCP = True
except ImportError:
    HAS_MCP = False
    logger.warning("mcp_xpressclaw not available — tool execution disabled")


def parse_tool_calls(content: str) -> tuple[str, list[dict]]:
    """Extract tool calls from LLM output.

    Handles formats:
    - <tool_call name="...">{"arg": "val"}</tool_call>
    - <tool_call>{"name": "...", "arguments": {...}}</tool_call>

    Returns (text_without_tools, list_of_tool_calls).
    """
    calls = []
    text_parts = []
    remaining = content

    # Pattern: <tool_call name="NAME">JSON_ARGS</tool_call>
    named_pattern = re.compile(
        r'<tool_call\s+name="([^"]+)">(.*?)</tool_call>',
        re.DOTALL,
    )
    # Pattern: <tool_call>{"name": "...", ...}</tool_call>
    json_pattern = re.compile(
        r'<tool_call>(.*?)</tool_call>',
        re.DOTALL,
    )

    last_end = 0
    for m in named_pattern.finditer(content):
        text_parts.append(content[last_end:m.start()])
        name = m.group(1)
        try:
            args = json.loads(m.group(2).strip())
        except json.JSONDecodeError:
            args = {"raw": m.group(2).strip()}
        calls.append({"name": name, "arguments": args})
        last_end = m.end()

    if not calls:
        for m in json_pattern.finditer(content):
            text_parts.append(content[last_end:m.start()])
            try:
                obj = json.loads(m.group(1).strip())
                name = obj.get("name", "")
                args = obj.get("arguments", obj)
                if name:
                    calls.append({"name": name, "arguments": args})
            except json.JSONDecodeError:
                pass
            last_end = m.end()

    text_parts.append(content[last_end:])
    clean_text = "".join(text_parts).strip()
    return clean_text, calls


def execute_tool(name: str, arguments: dict) -> str:
    """Execute a tool via the MCP handler."""
    if not HAS_MCP:
        return f"Error: tool execution not available"
    try:
        result = mcp_xpressclaw.handle_tool(name, arguments)
        return result
    except Exception as e:
        logger.exception("tool execution failed: %s", name)
        return f"Error executing {name}: {e}"


class GenericHarness(BaseHarness):
    """Proxies requests to an OpenAI-compatible LLM API with tool execution."""

    def __init__(self):
        super().__init__()
        headers = {"Content-Type": "application/json"}
        if LLM_API_KEY:
            headers["Authorization"] = f"Bearer {LLM_API_KEY}"
        self.client = httpx.AsyncClient(
            base_url=LLM_BASE_URL,
            headers=headers,
            timeout=300.0,
        )
        logger.info("generic harness: LLM_BASE_URL=%s, tools=%s", LLM_BASE_URL, HAS_MCP)

    async def _call_llm(self, messages, model, temperature, max_tokens):
        """Single LLM call, returns content string."""
        payload = {
            "model": LLM_MODEL or model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": False,
        }
        resp = await self.client.post("/chat/completions", json=payload)
        resp.raise_for_status()
        data = resp.json()
        choices = data.get("choices", [])
        if not choices:
            raise ValueError("LLM returned no choices")
        return choices[0]["message"]["content"]

    async def complete(
        self,
        messages: list[dict],
        model: str,
        temperature: float,
        max_tokens: int,
    ) -> str:
        """Multi-turn completion with tool execution."""
        conversation = list(messages)

        for turn in range(MAX_TOOL_TURNS):
            content = await self._call_llm(conversation, model, temperature, max_tokens)
            text, tool_calls = parse_tool_calls(content)

            if not tool_calls:
                return text or content

            # Execute tools and add results to conversation
            conversation.append({"role": "assistant", "content": content})

            for call in tool_calls:
                logger.info("executing tool: %s", call["name"])
                result = execute_tool(call["name"], call["arguments"])
                conversation.append({
                    "role": "user",
                    "content": f"Tool result for {call['name']}:\n{result}",
                })

        return text or content

    async def _stream_response(self, messages, model, temperature, max_tokens):
        """Streaming multi-turn completion with tool execution.

        Streams text chunks as SSE. When tool calls are detected,
        streams a tool indicator, executes the tool, and continues
        the conversation with the LLM.
        """
        conv_id = uuid4().hex[:16]
        conversation = list(messages)

        def chunk(delta: dict, finish_reason: Optional[str] = None) -> str:
            payload = {
                "id": f"chatcmpl-{conv_id}",
                "object": "chat.completion.chunk",
                "created": int(time.time()),
                "model": model,
                "choices": [
                    {"index": 0, "delta": delta, "finish_reason": finish_reason}
                ],
            }
            return f"data: {json.dumps(payload)}\n\n"

        try:
            for turn in range(MAX_TOOL_TURNS):
                # Stream the LLM response
                payload = {
                    "model": LLM_MODEL or model,
                    "messages": conversation,
                    "temperature": temperature,
                    "max_tokens": max_tokens,
                    "stream": True,
                }
                async with self.client.stream(
                    "POST", "/chat/completions", json=payload
                ) as resp:
                    resp.raise_for_status()
                    full_content = ""
                    async for line in resp.aiter_lines():
                        if not line.startswith("data: "):
                            continue
                        data_str = line[6:]
                        if data_str.strip() == "[DONE]":
                            break
                        try:
                            data = json.loads(data_str)
                            delta = data["choices"][0].get("delta", {})
                            text = delta.get("content", "")
                            if text:
                                full_content += text
                                yield chunk({"content": text})
                        except (json.JSONDecodeError, KeyError, IndexError):
                            continue

                # Check for tool calls in the accumulated content
                text, tool_calls = parse_tool_calls(full_content)

                if not tool_calls:
                    # No tools — we're done
                    yield chunk({}, "stop")
                    yield "data: [DONE]\n\n"
                    return

                # Execute tools
                conversation.append({"role": "assistant", "content": full_content})
                for call in tool_calls:
                    logger.info("executing tool (streaming): %s", call["name"])
                    result = execute_tool(call["name"], call["arguments"])
                    conversation.append({
                        "role": "user",
                        "content": f"Tool result for {call['name']}:\n{result}",
                    })

                # Stream a newline before next turn
                yield chunk({"content": "\n"})

            # Max turns reached
            yield chunk({}, "stop")
            yield "data: [DONE]\n\n"

        except Exception as e:
            logger.exception("streaming completion failed")
            error_payload = {"error": {"message": str(e), "type": "server_error"}}
            yield f"data: {json.dumps(error_payload)}\n\n"


if __name__ == "__main__":
    GenericHarness().run()
