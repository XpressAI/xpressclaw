"""Local model backend using vLLM or Ollama.

Supports local models via vLLM (OpenAI-compatible API) or Ollama.
vLLM is the default for better performance with GPU inference.
"""

from typing import AsyncIterator, Any, Literal
import logging
import json
import re

import aiohttp

from xpressai.agents.base import AgentBackend
from xpressai.core.config import AgentConfig, LocalModelConfig
from xpressai.core.exceptions import BackendError

logger = logging.getLogger(__name__)

# Tool format types
ToolFormat = Literal["xml", "json", "native"]

# Memory system instructions - prepended to all agents
MEMORY_SYSTEM_INSTRUCTIONS = """
## CRITICAL: YOU HAVE ANTEROGRADE AMNESIA

You cannot form new long-term memories naturally. After each conversation ends, you will forget everything unless you explicitly save it using your memory tools.

**Before starting work:** Use `search_memory` to recall what you know about the user, their projects, and relevant context.

**During conversations:** When you learn important information (user details, company info, project decisions, preferences, technical details, contacts, URLs), use `create_memory` IMMEDIATELY. If you don't save it, you won't remember it next time.

**Be proactive:** If someone tells you about themselves or their work, SAVE IT. Your memory is your zettelkasten - treat it as essential to your function.
""".strip()


def format_tools_xml(tools: list[dict[str, Any]]) -> tuple[str, str]:
    """Format tools for XML-style tool calling.

    Returns:
        Tuple of (tools_description, tool_instructions)
    """
    if not tools:
        return "", ""

    tools_desc = ["## Available Tools\n"]
    for tool in tools:
        name = tool.get("name", "unknown")
        description = tool.get("description", "")
        schema = tool.get("inputSchema", tool.get("input_schema", {}))

        tools_desc.append(f"### {name}")
        tools_desc.append(f"{description}\n")

        props = schema.get("properties", {})
        required = schema.get("required", [])
        if props:
            tools_desc.append("Parameters:")
            for param_name, param_info in props.items():
                param_type = param_info.get("type", "any")
                param_desc = param_info.get("description", "")
                req_mark = " (required)" if param_name in required else ""
                tools_desc.append(f"  - {param_name}: {param_type}{req_mark} - {param_desc}")
        tools_desc.append("")

    instructions = """## Tool Usage Instructions

To use a tool, output XML tags with the tool name and JSON arguments inside:

<tool_name>{"arg1": "value1", "arg2": "value2"}</tool_name>

Examples:
- To write a file: <write_file>{"path": "hello.txt", "content": "Hello World"}</write_file>
- To read a file: <read_file>{"path": "hello.txt"}</read_file>
- To list directory: <list_directory>{"path": "."}</list_directory>
- To run a command: <execute_command>{"command": "ls -la"}</execute_command>

You can use multiple tools in a single response. After each tool use, you will receive the result.
When you have completed the task, respond normally without tool tags."""

    return "\n".join(tools_desc), instructions


def format_tools_json(tools: list[dict[str, Any]]) -> tuple[str, str]:
    """Format tools for JSON-style tool calling (```tool blocks).

    Returns:
        Tuple of (tools_description, tool_instructions)
    """
    if not tools:
        return "", ""

    tools_desc = ["## Available Tools\n"]
    for tool in tools:
        name = tool.get("name", "unknown")
        description = tool.get("description", "")
        schema = tool.get("inputSchema", tool.get("input_schema", {}))

        tools_desc.append(f"### {name}")
        tools_desc.append(f"{description}\n")

        props = schema.get("properties", {})
        required = schema.get("required", [])
        if props:
            tools_desc.append("Parameters:")
            for param_name, param_info in props.items():
                param_type = param_info.get("type", "any")
                param_desc = param_info.get("description", "")
                req_mark = " (required)" if param_name in required else ""
                tools_desc.append(f"  - {param_name}: {param_type}{req_mark} - {param_desc}")
        tools_desc.append("")

    instructions = '''## Tool Usage Instructions

To use a tool, respond with a code block like this:

```tool
{"name": "tool_name", "input": {"arg1": "value1"}}
```

Examples:
```tool
{"name": "write_file", "input": {"path": "hello.txt", "content": "Hello World"}}
```

You can use multiple tools in a single response. When done, respond normally without tool blocks.'''

    return "\n".join(tools_desc), instructions


def parse_tool_calls_xml(response: str) -> list[tuple[str, dict[str, Any]]]:
    """Parse XML-style tool calls from response.

    Args:
        response: The model's response text

    Returns:
        List of (tool_name, arguments) tuples
    """
    tool_calls = []
    # Match <tool_name>{...}</tool_name> patterns
    pattern = r"<(\w+)>\s*(\{.*?\})\s*</\1>"

    for match in re.finditer(pattern, response, re.DOTALL):
        tool_name = match.group(1)
        try:
            args = json.loads(match.group(2))
            tool_calls.append((tool_name, args))
        except json.JSONDecodeError:
            logger.warning(f"Failed to parse tool arguments for {tool_name}")

    return tool_calls


def parse_tool_calls_json(response: str) -> list[tuple[str, dict[str, Any]]]:
    """Parse JSON-style tool calls from response (```tool blocks).

    Args:
        response: The model's response text

    Returns:
        List of (tool_name, arguments) tuples
    """
    tool_calls = []
    pattern = r"```tool\s*\n?(.*?)\n?```"

    for match in re.finditer(pattern, response, re.DOTALL):
        try:
            data = json.loads(match.group(1))
            tool_name = data.get("name")
            args = data.get("input", {})
            if tool_name:
                tool_calls.append((tool_name, args))
        except json.JSONDecodeError:
            logger.warning("Failed to parse tool block")

    return tool_calls


def format_tools_native(tools: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Format tools for OpenAI-compatible native tool calling API.

    Args:
        tools: List of tool definitions

    Returns:
        List of tools in OpenAI function format
    """
    formatted = []
    for tool in tools:
        name = tool.get("name", "unknown")
        description = tool.get("description", "")
        schema = tool.get("inputSchema", tool.get("input_schema", {}))

        formatted.append({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": schema,
            }
        })
    return formatted


def parse_tool_calls_native(response_data: dict) -> list[tuple[str, dict[str, Any], str]]:
    """Parse native tool calls from OpenAI-compatible API response.

    Args:
        response_data: The API response data (parsed JSON)

    Returns:
        List of (tool_name, arguments, tool_call_id) tuples
    """
    tool_calls = []

    if "choices" not in response_data or not response_data["choices"]:
        return tool_calls

    message = response_data["choices"][0].get("message", {})
    raw_tool_calls = message.get("tool_calls", [])

    for tc in raw_tool_calls:
        if tc.get("type") == "function":
            func = tc.get("function", {})
            tool_name = func.get("name")
            try:
                args = json.loads(func.get("arguments", "{}"))
            except json.JSONDecodeError:
                args = {}
            tool_id = tc.get("id", "")
            if tool_name:
                tool_calls.append((tool_name, args, tool_id))

    return tool_calls


class LocalModelBackend(AgentBackend):
    """Backend for local models via vLLM or Ollama.

    Uses the OpenAI-compatible API (vLLM default) or Ollama API.
    Supports streaming responses and system prompts.
    """

    def __init__(self):
        """Initialize the backend."""
        self._model = "Qwen/Qwen3-8B"
        self._base_url = "http://localhost:8000"
        self._api_key = "EMPTY"
        self._inference_backend = "vllm"
        self._system_prompt = ""
        self._config: AgentConfig | None = None
        self._model_config: LocalModelConfig | None = None
        self._memory_context = ""
        self._tools: list[dict[str, Any]] = []
        self._tool_format: ToolFormat = "xml"  # xml, json, or native
        self._tool_registry = None  # Set by runner for tool execution
        self._conversation_history: list[dict[str, str]] = []

    async def initialize(self, config: AgentConfig) -> None:
        """Initialize with configuration.

        Args:
            config: Agent configuration
        """
        self._config = config
        self._system_prompt = config.role

        # Check if inference server is running
        await self._check_server()

    async def _check_server(self) -> None:
        """Check if the inference server is available and auto-detect model if needed."""
        try:
            async with aiohttp.ClientSession() as session:
                if self._inference_backend == "vllm":
                    # vLLM uses OpenAI-compatible /v1/models endpoint
                    async with session.get(
                        f"{self._base_url}/v1/models",
                        headers={"Authorization": f"Bearer {self._api_key}"},
                    ) as resp:
                        if resp.status != 200:
                            logger.warning("vLLM server not responding, inference may fail")
                        else:
                            data = await resp.json()
                            models = [m["id"] for m in data.get("data", [])]
                            logger.info(f"Server available with models: {models}")
                            # Auto-select first model if current model not in list
                            if models and self._model not in models:
                                self._model = models[0]
                                logger.info(f"Auto-selected model: {self._model}")
                else:
                    # Ollama - check server and model availability
                    async with session.get(f"{self._base_url}/api/version") as resp:
                        if resp.status != 200:
                            logger.warning("Ollama not responding, inference may fail")
                            return

                    # Check if model is available, pull if not
                    model_available = await self._check_ollama_model(session)
                    if not model_available:
                        print(f"\n    Model '{self._model}' not found locally, pulling from Ollama...")
                        await self._pull_ollama_model(session)
        except Exception as e:
            logger.warning(f"Could not connect to inference server: {e}")

    async def _check_ollama_model(self, session: aiohttp.ClientSession) -> bool:
        """Check if the model is available in Ollama.

        Args:
            session: aiohttp session

        Returns:
            True if model is available, False otherwise
        """
        try:
            async with session.get(f"{self._base_url}/api/tags") as resp:
                if resp.status != 200:
                    return False
                data = await resp.json()
                models = [m.get("name", "") for m in data.get("models", [])]
                # Check both exact match and base name match (e.g., "qwen3:8b" vs "qwen3:8b-instruct")
                model_base = self._model.split(":")[0] if ":" in self._model else self._model
                for m in models:
                    if m == self._model or m.startswith(f"{model_base}:"):
                        logger.info(f"Model {self._model} is available")
                        return True
                logger.info(f"Model {self._model} not in available models: {models}")
                return False
        except Exception as e:
            logger.warning(f"Failed to check Ollama models: {e}")
            return False

    async def _pull_ollama_model(self, session: aiohttp.ClientSession) -> bool:
        """Pull a model from Ollama registry.

        Args:
            session: aiohttp session

        Returns:
            True if pull succeeded, False otherwise
        """
        import sys

        try:
            async with session.post(
                f"{self._base_url}/api/pull",
                json={"name": self._model, "stream": True},
            ) as resp:
                if resp.status != 200:
                    print(f"    Failed to pull model: HTTP {resp.status}")
                    return False

                # Stream the pull progress
                last_status = ""
                last_pct = -1
                async for line in resp.content:
                    if line:
                        try:
                            data = json.loads(line.decode())
                            status = data.get("status", "")

                            if "pulling" in status:
                                # Show download progress
                                total = data.get("total", 0)
                                completed = data.get("completed", 0)
                                if total > 0:
                                    pct = int(completed / total * 100)
                                    # Only update every 5%
                                    if pct >= last_pct + 5 or pct == 100:
                                        print(f"\r    Downloading: {pct}%", end="", flush=True)
                                        last_pct = pct
                            elif status == "verifying sha256 digest":
                                print(f"\r    Verifying...      ", end="", flush=True)
                            elif status == "writing manifest":
                                print(f"\r    Finalizing...     ", end="", flush=True)
                            elif status == "success":
                                print(f"\r    Download complete!   ")
                                logger.info(f"Successfully pulled {self._model}")
                            elif status != last_status and status:
                                # Other status messages
                                if last_pct == -1:  # Haven't started downloading yet
                                    print(f"    {status}")
                            last_status = status
                        except json.JSONDecodeError:
                            pass

                return True
        except Exception as e:
            print(f"\n    Failed to pull model: {e}")
            logger.error(f"Failed to pull model {self._model}: {e}")
            return False

    def configure_model(self, model_config: LocalModelConfig) -> None:
        """Configure the model settings.

        Args:
            model_config: Local model configuration
        """
        self._model_config = model_config
        self._model = model_config.model
        self._base_url = model_config.base_url
        self._inference_backend = model_config.inference_backend
        self._api_key = model_config.api_key

    async def send(self, message: str) -> AsyncIterator[str]:
        """Send a message and stream response chunks.

        Args:
            message: User message

        Yields:
            Response text chunks
        """
        if self._inference_backend == "vllm":
            async for chunk in self._send_vllm(message):
                yield chunk
        else:
            async for chunk in self._send_ollama(message):
                yield chunk

    async def _send_vllm(self, message: str) -> AsyncIterator[str]:
        """Send message via vLLM OpenAI-compatible API.

        Args:
            message: User message

        Yields:
            Response text chunks
        """
        messages = self._build_messages(message)
        self._conversation_history.append({"role": "user", "content": message})

        try:
            response_text = ""

            async with aiohttp.ClientSession() as session:
                async with session.post(
                    f"{self._base_url}/v1/chat/completions",
                    headers={
                        "Authorization": f"Bearer {self._api_key}",
                        "Content-Type": "application/json",
                    },
                    json={
                        "model": self._model,
                        "messages": messages,
                        "stream": True,
                        "max_tokens": self._model_config.context_length // 2
                        if self._model_config
                        else 16384,
                    },
                ) as response:
                    if response.status != 200:
                        error_text = await response.text()
                        raise BackendError(f"vLLM error ({response.status}): {error_text}")

                    # Parse SSE stream
                    async for line in response.content:
                        line = line.decode("utf-8").strip()
                        if line.startswith("data: "):
                            data_str = line[6:]
                            if data_str == "[DONE]":
                                break
                            try:
                                data = json.loads(data_str)
                                if "choices" in data and data["choices"]:
                                    delta = data["choices"][0].get("delta", {})
                                    chunk = delta.get("content")
                                    if chunk:  # Skip None or empty strings
                                        response_text += chunk
                                        yield chunk
                            except json.JSONDecodeError:
                                pass

            self._conversation_history.append({"role": "assistant", "content": response_text})
            self._trim_history()

        except aiohttp.ClientError as e:
            raise BackendError(f"Failed to connect to vLLM: {e}")

    async def _send_ollama(self, message: str) -> AsyncIterator[str]:
        """Send message via Ollama API.

        Args:
            message: User message

        Yields:
            Response text chunks
        """
        messages = self._build_messages(message)
        self._conversation_history.append({"role": "user", "content": message})

        try:
            response_text = ""

            async with aiohttp.ClientSession() as session:
                async with session.post(
                    f"{self._base_url}/api/chat",
                    json={
                        "model": self._model,
                        "messages": messages,
                        "stream": True,
                        "options": {
                            "num_ctx": self._model_config.context_length
                            if self._model_config
                            else 32768,
                        },
                    },
                ) as response:
                    if response.status != 200:
                        error_text = await response.text()
                        raise BackendError(f"Ollama error: {error_text}")

                    async for line in response.content:
                        if line:
                            try:
                                data = json.loads(line)
                                if "message" in data:
                                    chunk = data["message"].get("content")
                                    if chunk:  # Skip None or empty strings
                                        response_text += chunk
                                        yield chunk
                            except json.JSONDecodeError:
                                pass

            self._conversation_history.append({"role": "assistant", "content": response_text})
            self._trim_history()

        except aiohttp.ClientError as e:
            raise BackendError(f"Failed to connect to Ollama: {e}")

    def _trim_history(self) -> None:
        """Keep conversation history manageable."""
        if len(self._conversation_history) > 20:
            self._conversation_history = self._conversation_history[-20:]

    def _normalize_history(self) -> list[dict[str, Any]]:
        """Normalize conversation history to ensure strict user/assistant alternation.

        vLLM chat templates require: system (optional) -> user -> assistant -> user -> assistant...

        This method:
        1. Converts 'system' messages to 'user' messages with [System] prefix
        2. Converts 'tool' messages to 'user' messages with [Tool Result] prefix
        3. Merges consecutive same-role messages (including consecutive tool results)
        4. Injects placeholder messages if alternation is broken

        Returns:
            Normalized message list ready for the LLM
        """
        if not self._conversation_history:
            return []

        normalized = []
        pending_tool_results = []

        def flush_tool_results():
            """Merge pending tool results into a single user message."""
            nonlocal pending_tool_results
            if pending_tool_results:
                merged_content = "\n\n".join(pending_tool_results)
                pending_tool_results = []
                return {"role": "user", "content": merged_content}
            return None

        for msg in self._conversation_history:
            role = msg.get("role", "user")
            content = msg.get("content", "")

            # Collect tool results to merge them
            if role == "tool":
                tool_name = msg.get("name", "tool")
                pending_tool_results.append(f"[Tool Result - {tool_name}]: {content}")
                continue

            # Flush any pending tool results before processing other messages
            if pending_tool_results:
                tool_msg = flush_tool_results()
                if tool_msg:
                    # Need to ensure alternation before adding tool results
                    if normalized and normalized[-1].get("role") == "user":
                        normalized.append({"role": "assistant", "content": "[Processing tool calls...]"})
                    normalized.append(tool_msg)

            # Convert system messages to user messages
            if role == "system":
                role = "user"
                content = f"[System]: {content}"

            # Handle alternation
            if normalized:
                last_role = normalized[-1].get("role")
                if role == last_role:
                    if role == "user":
                        # Two user messages in a row - inject assistant placeholder
                        normalized.append({"role": "assistant", "content": "[Acknowledged]"})
                    elif role == "assistant":
                        # Two assistant messages in a row - merge them
                        normalized[-1]["content"] += f"\n\n{content}"
                        continue

            normalized.append({"role": role, "content": content})

        # Flush any remaining tool results at the end
        if pending_tool_results:
            tool_msg = flush_tool_results()
            if tool_msg:
                if normalized and normalized[-1].get("role") == "user":
                    normalized.append({"role": "assistant", "content": "[Processing tool calls...]"})
                normalized.append(tool_msg)

        return normalized

    def _build_messages(self, user_message: str) -> list[dict[str, str]]:
        """Build the messages list for the API call.

        Ensures strict alternation: system (optional) -> user -> assistant -> user -> assistant...

        Args:
            user_message: Current user message

        Returns:
            List of message dicts
        """
        messages = []

        # System prompt with memory instructions
        # Memory tools are always available in chat mode via meta tools
        system_content = f"{MEMORY_SYSTEM_INSTRUCTIONS}\n\n{self._system_prompt}" if self._system_prompt else MEMORY_SYSTEM_INSTRUCTIONS

        if self._memory_context:
            system_content += f"\n\n{self._memory_context}"

        # Add tool definitions and instructions based on format
        # Skip for native format - tools are passed via API parameter
        if self._tools and self._tool_format != "native":
            if self._tool_format == "xml":
                tools_desc, instructions = format_tools_xml(self._tools)
            else:  # json format
                tools_desc, instructions = format_tools_json(self._tools)

            system_content += f"\n\n{tools_desc}\n\n{instructions}"

        if system_content:
            messages.append({"role": "system", "content": system_content})

        # Normalized conversation history (ensures proper alternation)
        normalized = self._normalize_history()
        messages.extend(normalized)

        # Current message - ensure we can add it
        # If history ends with user, inject assistant placeholder first
        if normalized and normalized[-1].get("role") == "user":
            messages.append({"role": "assistant", "content": "[Acknowledged]"})

        messages.append({"role": "user", "content": user_message})

        return messages

    def _build_messages_for_continuation(self) -> list[dict[str, Any]]:
        """Build messages for continuation after tool results.

        Returns messages without adding a new user message, for when the model
        should continue after receiving tool results.

        Ensures strict alternation: system (optional) -> user -> assistant -> user -> assistant...

        Returns:
            List of message dicts
        """
        messages = []

        # System prompt with memory instructions
        # Memory tools are always available in chat mode via meta tools
        system_content = f"{MEMORY_SYSTEM_INSTRUCTIONS}\n\n{self._system_prompt}" if self._system_prompt else MEMORY_SYSTEM_INSTRUCTIONS

        if self._memory_context:
            system_content += f"\n\n{self._memory_context}"

        if system_content:
            messages.append({"role": "system", "content": system_content})

        # Normalized conversation history (ensures proper alternation)
        normalized = self._normalize_history()
        messages.extend(normalized)

        # For continuation, if history ends with assistant, we need a user message
        # to prompt continuation (the model expects user -> assistant alternation)
        if normalized and normalized[-1].get("role") == "assistant":
            messages.append({"role": "user", "content": "Continue with the task based on the tool results above."})

        return messages

    def set_tool_format(self, format: ToolFormat) -> None:
        """Set the tool calling format.

        Args:
            format: "xml" for <tool>{}</tool>, "json" for ```tool blocks, "native" for API function calling
        """
        self._tool_format = format

    async def send_native_with_tools(
        self, message: str, is_continuation: bool = False
    ) -> tuple[str, list[tuple[str, dict[str, Any], str]]]:
        """Send message with native tool calling (OpenAI-compatible API).

        Args:
            message: User message
            is_continuation: If True, don't add a user message (continuing after tool results)

        Returns:
            Tuple of (response_text, tool_calls) where tool_calls is
            list of (tool_name, arguments, tool_call_id)
        """
        if is_continuation:
            # After tool results, don't add another user message - let model continue
            messages = self._build_messages_for_continuation()
        else:
            messages = self._build_messages(message)
            self._conversation_history.append({"role": "user", "content": message})

        # Format tools for API
        tools = format_tools_native(self._tools) if self._tools else None

        try:
            async with aiohttp.ClientSession() as session:
                request_body = {
                    "model": self._model,
                    "messages": messages,
                    "stream": False,  # Non-streaming for tool calls
                    "max_tokens": self._model_config.context_length // 2
                    if self._model_config
                    else 16384,
                }
                if tools:
                    request_body["tools"] = tools

                async with session.post(
                    f"{self._base_url}/v1/chat/completions",
                    headers={
                        "Authorization": f"Bearer {self._api_key}",
                        "Content-Type": "application/json",
                    },
                    json=request_body,
                ) as response:
                    if response.status != 200:
                        error_text = await response.text()
                        raise BackendError(f"vLLM error ({response.status}): {error_text}")

                    data = await response.json()

                    # Extract response content and tool calls
                    choice = data.get("choices", [{}])[0]
                    msg = choice.get("message", {})
                    content = msg.get("content", "") or ""
                    tool_calls = parse_tool_calls_native(data)

                    # Add to conversation history
                    history_entry = {"role": "assistant", "content": content}
                    if tool_calls:
                        # Store tool calls in history for context
                        history_entry["tool_calls"] = [
                            {"id": tc[2], "type": "function", "function": {"name": tc[0], "arguments": json.dumps(tc[1])}}
                            for tc in tool_calls
                        ]
                    self._conversation_history.append(history_entry)
                    self._trim_history()

                    return content, tool_calls

        except aiohttp.ClientError as e:
            raise BackendError(f"Failed to connect to vLLM: {e}")

    def add_tool_result(self, tool_call_id: str, tool_name: str, result: str) -> None:
        """Add a tool result to conversation history for native tool calling.

        Args:
            tool_call_id: The tool call ID from the API response
            tool_name: Name of the tool
            result: Tool execution result
        """
        self._conversation_history.append({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "name": tool_name,
            "content": result,
        })

    def set_tool_registry(self, registry) -> None:
        """Set the tool registry for executing tools.

        Args:
            registry: ToolRegistry instance
        """
        self._tool_registry = registry

    def parse_tool_calls(self, response: str) -> list[tuple[str, dict[str, Any]]]:
        """Parse tool calls from a response based on current format.

        Args:
            response: Model response text

        Returns:
            List of (tool_name, arguments) tuples
        """
        if self._tool_format == "xml":
            return parse_tool_calls_xml(response)
        else:
            return parse_tool_calls_json(response)

    async def execute_tool(self, tool_name: str, arguments: dict[str, Any]) -> str:
        """Execute a tool and return the result.

        Args:
            tool_name: Name of the tool to execute
            arguments: Tool arguments

        Returns:
            Tool result as string
        """
        if not self._tool_registry:
            return f"Error: No tool registry available to execute {tool_name}"

        try:
            result = await self._tool_registry.call_tool(tool_name, arguments)
            # Format result for model consumption
            if isinstance(result, dict):
                return json.dumps(result, indent=2)
            return str(result)
        except Exception as e:
            return f"Error executing {tool_name}: {e}"

    async def inject_memory(self, context: str) -> None:
        """Inject memory context into the system prompt.

        This temporarily augments the system prompt with relevant memories.
        The agent won't see this as explicit "memory" - it appears as
        additional context in the system message.

        Args:
            context: Formatted memory context
        """
        self._memory_context = context

    async def clear_injected_memory(self) -> None:
        """Clear injected memory context.

        Called after a response to reset the system prompt to its original state.
        This ensures the next message starts fresh (memory sub-agent will
        inject relevant context again if configured).
        """
        self._memory_context = ""

    async def register_tools(self, tools: list[dict[str, Any]]) -> None:
        """Register tools.

        Args:
            tools: List of tool definitions
        """
        self._tools = tools

    async def shutdown(self) -> None:
        """Shutdown the backend."""
        self._conversation_history.clear()
        self._memory_context = ""

    async def interrupt(self) -> None:
        """Interrupt current operation."""
        # Ollama doesn't support mid-stream interruption easily
        pass

    def clear_history(self) -> None:
        """Clear conversation history."""
        self._conversation_history.clear()

    def set_history(self, history: list[dict[str, str]]) -> None:
        """Set conversation history from external source.

        Args:
            history: List of message dicts with 'role' and 'content' keys.
        """
        self._conversation_history = [
            {"role": msg["role"], "content": msg["content"]}
            for msg in history
            if msg.get("role") in ("user", "assistant") and msg.get("content")
        ]

    @property
    def model(self) -> str:
        """The model being used."""
        return self._model

    @property
    def supports_streaming(self) -> bool:
        """Whether streaming is supported."""
        return True

    @property
    def supports_tools(self) -> bool:
        """Whether tool use is supported."""
        return True  # Via prompt wrapping

    @property
    def supports_memory(self) -> bool:
        """Whether memory injection is supported."""
        return True


class LlamaCppBackend(AgentBackend):
    """Backend for direct llama.cpp inference.

    Alternative to Ollama for direct GGUF model loading.
    Requires llama-cpp-python package.
    """

    def __init__(self):
        """Initialize the backend."""
        self._model = None
        self._model_path: str | None = None
        self._system_prompt = ""
        self._config: AgentConfig | None = None
        self._conversation_history: list[dict[str, str]] = []

    async def initialize(self, config: AgentConfig) -> None:
        """Initialize with configuration.

        Args:
            config: Agent configuration
        """
        self._config = config
        self._system_prompt = config.role

    def load_model(self, model_path: str, n_ctx: int = 32768, n_gpu_layers: int = -1) -> None:
        """Load a GGUF model.

        Args:
            model_path: Path to the GGUF file
            n_ctx: Context length
            n_gpu_layers: Number of layers to offload to GPU (-1 = all)
        """
        try:
            from llama_cpp import Llama

            self._model = Llama(
                model_path=model_path,
                n_ctx=n_ctx,
                n_gpu_layers=n_gpu_layers,
                verbose=False,
            )
            self._model_path = model_path
            logger.info(f"Loaded model: {model_path}")

        except ImportError:
            raise BackendError(
                "llama-cpp-python not installed. Install with: pip install llama-cpp-python"
            )

    async def send(self, message: str) -> AsyncIterator[str]:
        """Send a message and stream response.

        Args:
            message: User message

        Yields:
            Response text chunks
        """
        if self._model is None:
            raise BackendError("Model not loaded. Call load_model() first.")

        # Build prompt
        prompt = self._build_prompt(message)

        # Add to history
        self._conversation_history.append({"role": "user", "content": message})

        response_text = ""

        # Generate with streaming
        for output in self._model(
            prompt,
            max_tokens=4096,
            stop=["<|im_end|>", "<|endoftext|>"],
            stream=True,
        ):
            chunk = output["choices"][0]["text"]
            response_text += chunk
            yield chunk

        # Add to history
        self._conversation_history.append({"role": "assistant", "content": response_text})

    def _build_prompt(self, user_message: str) -> str:
        """Build the prompt for the model.

        Args:
            user_message: Current user message

        Returns:
            Formatted prompt string
        """
        # ChatML format (used by Qwen)
        parts = []

        if self._system_prompt:
            parts.append(f"<|im_start|>system\n{self._system_prompt}<|im_end|>")

        for msg in self._conversation_history:
            role = msg["role"]
            content = msg["content"]
            parts.append(f"<|im_start|>{role}\n{content}<|im_end|>")

        parts.append(f"<|im_start|>user\n{user_message}<|im_end|>")
        parts.append("<|im_start|>assistant\n")

        return "\n".join(parts)

    async def shutdown(self) -> None:
        """Shutdown the backend."""
        if self._model:
            del self._model
            self._model = None
        self._conversation_history.clear()

    @property
    def model(self) -> str:
        """The model being used."""
        return self._model_path or "unknown"

    @property
    def supports_streaming(self) -> bool:
        return True

    @property
    def supports_tools(self) -> bool:
        return False  # Would need prompt wrapping

    @property
    def supports_memory(self) -> bool:
        return False
