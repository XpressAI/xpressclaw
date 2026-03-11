"""LangChain harness.

Runs a LangChain agent with tool access inside the container.
Supports both LangChain and CrewAI-style agents.

Environment variables:
  LLM_BASE_URL     — OpenAI-compatible LLM API URL (default: http://host.docker.internal:8935/v1)
  LLM_API_KEY      — API key for the LLM API
  LLM_MODEL        — Model name (default: gpt-4o)
  OPENAI_API_KEY   — Alias for LLM_API_KEY (LangChain convention)
  ANTHROPIC_API_KEY — For Anthropic models
  WORKSPACE_DIR    — Agent workspace (default: /workspace)
"""

import os
import sys

sys.path.insert(0, "/app")

from server import BaseHarness, logger, AGENT_ID, AGENT_NAME

LLM_BASE_URL = os.environ.get("LLM_BASE_URL", "http://host.docker.internal:8935/v1")
LLM_API_KEY = os.environ.get("LLM_API_KEY", os.environ.get("OPENAI_API_KEY", ""))
LLM_MODEL = os.environ.get("LLM_MODEL", "gpt-4o")
WORKSPACE_DIR = os.environ.get("WORKSPACE_DIR", "/workspace")


class LangChainHarness(BaseHarness):
    """Runs a LangChain ReAct agent per request."""

    def __init__(self):
        super().__init__()
        self._agent = None

    def _get_agent(self, model: str, system_prompt: str):
        """Build a LangChain agent with file/shell tools."""
        from langchain_openai import ChatOpenAI
        from langchain.agents import AgentExecutor, create_react_agent
        from langchain_core.prompts import ChatPromptTemplate
        from langchain_community.tools import ShellTool
        from langchain_community.tools.file_management import (
            ReadFileTool,
            WriteFileTool,
            ListDirectoryTool,
        )

        llm = ChatOpenAI(
            model=model,
            base_url=LLM_BASE_URL,
            api_key=LLM_API_KEY or "not-set",
            temperature=0.7,
        )

        tools = [
            ShellTool(ask_human_input=False),
            ReadFileTool(root_dir=WORKSPACE_DIR),
            WriteFileTool(root_dir=WORKSPACE_DIR),
            ListDirectoryTool(root_dir=WORKSPACE_DIR),
        ]

        prompt = ChatPromptTemplate.from_messages([
            ("system", system_prompt or f"You are {AGENT_NAME}, an AI assistant with access to tools."),
            ("human", "{input}"),
            ("placeholder", "{agent_scratchpad}"),
        ])

        agent = create_react_agent(llm, tools, prompt)
        return AgentExecutor(
            agent=agent,
            tools=tools,
            verbose=True,
            handle_parsing_errors=True,
            max_iterations=10,
        )

    async def complete(
        self,
        messages: list[dict],
        model: str,
        temperature: float,
        max_tokens: int,
    ) -> str:
        # Extract system prompt and last user message
        system_prompt = ""
        task = ""
        for msg in messages:
            if msg["role"] == "system":
                system_prompt = msg["content"]
        for msg in reversed(messages):
            if msg["role"] == "user":
                task = msg["content"]
                break

        if not task:
            return "No task provided."

        use_model = model if model != AGENT_NAME else LLM_MODEL

        logger.info(
            "running langchain agent: model=%s task_len=%d workspace=%s",
            use_model, len(task), WORKSPACE_DIR,
        )

        executor = self._get_agent(use_model, system_prompt)
        result = await executor.ainvoke({"input": task})

        return result.get("output", str(result))


if __name__ == "__main__":
    LangChainHarness().run()
