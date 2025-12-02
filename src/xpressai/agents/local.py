"""Local model backend using vLLM or Ollama.

Supports local models via vLLM (OpenAI-compatible API) or Ollama.
vLLM is the default for better performance with GPU inference.
"""

from typing import AsyncIterator, Any
import logging
import json

import aiohttp

from xpressai.agents.base import AgentBackend
from xpressai.core.config import AgentConfig, LocalModelConfig
from xpressai.core.exceptions import BackendError

logger = logging.getLogger(__name__)


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
        """Check if the inference server is available."""
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
                            logger.info(f"vLLM server available with models: {models}")
                else:
                    # Ollama uses /api/version
                    async with session.get(f"{self._base_url}/api/version") as resp:
                        if resp.status != 200:
                            logger.warning("Ollama not responding, inference may fail")
        except Exception as e:
            logger.warning(f"Could not connect to inference server: {e}")

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
                                    if "content" in delta:
                                        chunk = delta["content"]
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
                                if "message" in data and "content" in data["message"]:
                                    chunk = data["message"]["content"]
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

    def _build_messages(self, user_message: str) -> list[dict[str, str]]:
        """Build the messages list for the API call.

        Args:
            user_message: Current user message

        Returns:
            List of message dicts
        """
        messages = []

        # System prompt with memory and tools
        system_content = self._system_prompt

        if self._memory_context:
            system_content += f"\n\n{self._memory_context}"

        if self._tools:
            from xpressai.agents.base import ToolPromptWrapper

            wrapper = ToolPromptWrapper(self._tools)
            system_content += f"\n\n{wrapper.format_tools_prompt()}"

        if system_content:
            messages.append({"role": "system", "content": system_content})

        # Conversation history
        messages.extend(self._conversation_history)

        # Current message
        messages.append({"role": "user", "content": user_message})

        return messages

    async def inject_memory(self, context: str) -> None:
        """Inject memory context.

        Args:
            context: Formatted memory context
        """
        self._memory_context = context

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
