"""XpressAI chat command - Interactive chat with an agent."""

import asyncio
from pathlib import Path
import click
import sys

from xpressai.core.config import load_config
from xpressai.core.runtime import Runtime


def run_chat(agent: str) -> None:
    """Start an interactive chat session with an agent."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        click.echo("Run 'xpressai init' first.")
        return

    config = load_config(config_path)

    # Check if agent exists in config
    agent_names = [a.name for a in config.agents]
    if agent not in agent_names:
        click.echo(click.style(f"Agent '{agent}' not found.", fg="red"))
        click.echo(f"Available agents: {', '.join(agent_names)}")
        return

    click.echo(click.style(f"Starting chat with {agent}...", fg="cyan", bold=True))
    click.echo(click.style("Type 'exit' or 'quit' to end the session.", fg="white"))
    click.echo(click.style("Type '/clear' to clear chat history.", fg="white"))
    click.echo()

    try:
        asyncio.run(_chat_loop(config, agent))
    except KeyboardInterrupt:
        click.echo()
        click.echo(click.style("Chat ended.", fg="yellow"))


async def _chat_loop(config, agent: str) -> None:
    """Main chat loop."""
    runtime = Runtime(config)
    await runtime.initialize()
    await runtime.start()

    # Get the backend for this agent
    backend = runtime._backends.get(agent)
    if not backend:
        click.echo(click.style(f"Agent backend not available for '{agent}'", fg="red"))
        return

    # Get agent config for hooks
    agent_config = None
    for ac in config.agents:
        if ac.name == agent:
            agent_config = ac
            break

    # Set up meta tools
    from xpressai.tools.builtin.meta import (
        set_managers,
        get_meta_tool_schemas,
        execute_meta_tool,
    )

    set_managers(
        runtime.task_board,
        runtime.memory_manager,
        runtime.sop_manager,
        agent_id=agent,
    )

    # Register meta tools with the backend
    tool_schemas = get_meta_tool_schemas()
    if hasattr(backend, "register_tools"):
        await backend.register_tools(tool_schemas)

    try:
        while True:
            try:
                # Get user input
                message = input(click.style("You: ", fg="green", bold=True))
            except EOFError:
                break

            message = message.strip()
            if not message:
                continue

            # Handle special commands
            if message.lower() in ("exit", "quit"):
                break

            if message.lower() == "/clear":
                if hasattr(backend, "clear_history"):
                    backend.clear_history()
                click.echo(click.style("Chat history cleared.", fg="yellow"))
                continue

            # Run memory recall hook if configured
            memory_context = ""
            memory_backend = None
            if (agent_config and agent_config.hooks and
                agent_config.hooks.before_message and
                runtime.memory_manager and config.memory):

                from xpressai.memory.hooks import memory_recall

                memory_backend = await runtime.get_memory_backend(agent_config)

                if memory_backend:
                    async def memory_llm_callback(prompt: str) -> str:
                        memory_backend.clear_history()
                        parts = []
                        async for chunk in memory_backend.send(prompt):
                            parts.append(chunk)
                        return "".join(parts)

                    try:
                        result = await memory_recall(
                            agent_id=agent,
                            message=message,
                            memory_manager=runtime.memory_manager,
                            memory_config=config.memory,
                            llm_callback=memory_llm_callback,
                        )
                        memory_context = result.get("context", "")
                        if memory_context:
                            click.echo(click.style("  [Memory recalled]", fg="cyan", dim=True))
                    except Exception as e:
                        click.echo(click.style(f"  [Memory recall error: {e}]", fg="red", dim=True))

            # Inject memory context if available
            if memory_context and hasattr(backend, "inject_memory"):
                await backend.inject_memory(memory_context)

            # Send message and stream response
            click.echo(click.style(f"{agent}: ", fg="blue", bold=True), nl=False)

            try:
                response_text = ""

                # Check if backend supports native tools
                if hasattr(backend, "_tool_format") and backend._tool_format == "native":
                    text, tool_calls = await backend.send_native_with_tools(message)
                    response_text = text
                    click.echo(text, nl=False)

                    # Execute tool calls
                    for tool_name, args, tool_id in tool_calls:
                        click.echo()
                        click.echo(click.style(f"  [Calling {tool_name}...]", fg="yellow", dim=True))
                        result = await execute_meta_tool(tool_name, args)
                        backend.add_tool_result(tool_id, tool_name, result)

                    # Get final response if there were tool calls
                    if tool_calls:
                        final_text, _ = await backend.send_native_with_tools(
                            "", is_continuation=True
                        )
                        if final_text:
                            click.echo(final_text, nl=False)
                            response_text = (response_text + "\n" + final_text).strip()
                else:
                    # Stream response
                    async for chunk in backend.send(message):
                        click.echo(chunk, nl=False)
                        response_text += chunk

                click.echo()  # Newline after response

                # Clear injected memory
                if memory_context and hasattr(backend, "clear_injected_memory"):
                    await backend.clear_injected_memory()

                # Run memory remember hook if configured
                if (agent_config and agent_config.hooks and
                    agent_config.hooks.after_message and
                    runtime.memory_manager and config.memory):

                    from xpressai.memory.hooks import memory_remember

                    if memory_backend is None:
                        memory_backend = await runtime.get_memory_backend(agent_config)

                    if memory_backend:
                        async def memory_remember_callback(prompt: str) -> str:
                            memory_backend.clear_history()
                            parts = []
                            async for chunk in memory_backend.send(prompt):
                                parts.append(chunk)
                            return "".join(parts)

                        try:
                            conversation = [
                                {"role": "user", "content": message},
                                {"role": "assistant", "content": response_text},
                            ]
                            remember_result = await memory_remember(
                                agent_id=agent,
                                conversation=conversation,
                                memory_manager=runtime.memory_manager,
                                memory_config=config.memory,
                                llm_callback=memory_remember_callback,
                            )
                            if isinstance(remember_result, dict) and remember_result.get("stored"):
                                click.echo(click.style("  [Memory stored]", fg="cyan", dim=True))
                        except Exception as e:
                            click.echo(click.style(f"  [Memory error: {e}]", fg="red", dim=True))

            except Exception as e:
                click.echo()
                click.echo(click.style(f"Error: {e}", fg="red"))

            click.echo()  # Blank line between exchanges

    finally:
        await runtime.stop()
