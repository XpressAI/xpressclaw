"""Agent runner - executes tasks using agent backends.

The runner is the core loop that makes agents actually do work:
1. Polls for pending tasks assigned to the agent
2. Picks up tasks and executes them
3. Uses SOPs to guide execution when specified
4. Reports results back to the task board
"""

import asyncio
import logging
from datetime import datetime
from typing import Any

from xpressai.agents.base import AgentBackend
from xpressai.tasks.board import TaskBoard, Task, TaskStatus
from xpressai.tasks.sop import SOPManager, SOP
from xpressai.memory.hooks import run_before_message_hooks, run_after_message_hooks

# Import conditionally to avoid circular imports
from typing import TYPE_CHECKING
if TYPE_CHECKING:
    from xpressai.tasks.conversation import ConversationManager
    from xpressai.tools.registry import ToolRegistry
    from xpressai.core.activity import ActivityManager
    from xpressai.memory.manager import MemoryManager
    from xpressai.core.config import AgentConfig, MemoryConfig
    from xpressai.budget.manager import BudgetManager

logger = logging.getLogger(__name__)

# Maximum tool execution iterations to prevent infinite loops
MAX_TOOL_ITERATIONS = 20


class AgentRunner:
    """Runs an agent, processing tasks from the task board.

    The runner continuously:
    1. Checks for pending tasks assigned to this agent
    2. Picks up the highest priority task
    3. Executes it (following SOP if specified)
    4. Marks it complete or failed
    """

    def __init__(
        self,
        agent_id: str,
        backend: AgentBackend,
        task_board: TaskBoard,
        sop_manager: SOPManager | None = None,
        conversation_manager: "ConversationManager | None" = None,
        tool_registry: "ToolRegistry | None" = None,
        activity_manager: "ActivityManager | None" = None,
        memory_manager: "MemoryManager | None" = None,
        memory_config: "MemoryConfig | None" = None,
        agent_config: "AgentConfig | None" = None,
        memory_backend_factory=None,
        budget_manager: "BudgetManager | None" = None,
        poll_interval: float = 2.0,
    ):
        """Initialize the agent runner.

        Args:
            agent_id: ID of the agent
            backend: Agent backend for LLM interactions
            task_board: Task board to poll for work
            sop_manager: Optional SOP manager for workflow guidance
            conversation_manager: Optional conversation manager for logging
            tool_registry: Optional tool registry for tool execution
            activity_manager: Optional activity manager for event logging
            memory_manager: Optional memory manager for recall/remember hooks
            memory_config: Optional memory configuration with prompts
            agent_config: Optional agent configuration with hooks
            memory_backend_factory: Optional async callable that returns a memory backend
            budget_manager: Optional budget manager for cost tracking
            poll_interval: How often to check for new tasks (seconds)
        """
        self.agent_id = agent_id
        self.backend = backend
        self.task_board = task_board
        self.sop_manager = sop_manager
        self.conversation_manager = conversation_manager
        self.tool_registry = tool_registry
        self.activity_manager = activity_manager
        self.memory_manager = memory_manager
        self.memory_config = memory_config
        self.agent_config = agent_config
        self.memory_backend_factory = memory_backend_factory
        self.budget_manager = budget_manager
        self.poll_interval = poll_interval

        self._running = False
        self._current_task: Task | None = None
        self._task: asyncio.Task | None = None
        self.max_tool_calls = MAX_TOOL_ITERATIONS  # Can be overridden by runtime
        self.max_completion_retries = 3  # Max times to prompt for completion before auto-fail

        # Track completion retry attempts per task
        self._completion_retries: dict[str, int] = {}

        # Set up tool registry on backend if it supports it
        if tool_registry and hasattr(backend, 'set_tool_registry'):
            backend.set_tool_registry(tool_registry)

    async def _record_usage(
        self,
        input_text: str,
        output_text: str,
        operation: str = "query",
    ) -> None:
        """Record usage for budget tracking.

        Uses estimated token counts based on character count.
        Real token counts would require backend-specific API response parsing.

        Args:
            input_text: Input text sent to the model
            output_text: Output text received from the model
            operation: Type of operation (query, tool_call, etc.)
        """
        if not self.budget_manager:
            return

        # Get the model name from the backend
        model = getattr(self.backend, '_model', 'unknown')
        if not model or model == 'unknown':
            model = getattr(self.agent_config, 'backend', 'unknown') if self.agent_config else 'unknown'

        # Estimate tokens (rough: ~4 chars per token for English)
        input_tokens = max(1, len(input_text) // 4)
        output_tokens = max(1, len(output_text) // 4)

        try:
            await self.budget_manager.record_usage(
                agent_id=self.agent_id,
                model=model,
                input_tokens=input_tokens,
                output_tokens=output_tokens,
                operation=operation,
            )
        except Exception as e:
            logger.warning(f"Failed to record usage: {e}")

    async def start(self) -> None:
        """Start the agent runner loop."""
        if self._running:
            return

        self._running = True
        self._task = asyncio.create_task(self._run_loop())
        logger.info(f"Agent {self.agent_id} runner started")

    async def stop(self) -> None:
        """Stop the agent runner."""
        self._running = False

        if self._task:
            self._task.cancel()
            # Don't await the task - it may be attached to a different event loop
            # (e.g., when started via CLI but stopped via web API)
            # The CancelledError will be caught in the _run_loop
            self._task = None

        logger.info(f"Agent {self.agent_id} runner stopped")

    async def _run_loop(self) -> None:
        """Main agent loop - poll for tasks and execute them."""
        logger.info(f"Agent {self.agent_id} entering run loop")

        while self._running:
            try:
                # Check for pending tasks
                task = await self._get_next_task()

                if task:
                    await self._execute_task(task)
                else:
                    # No tasks, wait before polling again
                    await asyncio.sleep(self.poll_interval)

            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Agent {self.agent_id} loop error: {e}")
                await asyncio.sleep(self.poll_interval)

    async def _get_next_task(self) -> Task | None:
        """Get the next pending task for this agent."""
        tasks = await self.task_board.get_tasks(
            status=TaskStatus.PENDING,
            agent_id=self.agent_id,
            limit=1,
        )
        return tasks[0] if tasks else None

    async def _execute_task(self, task: Task) -> None:
        """Execute a single task with tool execution loop."""
        logger.info(f"Agent {self.agent_id} starting task: {task.title}")
        self._current_task = task

        # Set task context for tools like ask_user and task_control
        from xpressai.tools.builtin.ask_user import current_task_id
        from xpressai.tools.builtin.task_control import (
            set_current_task_id,
            reset_task_completion,
            is_task_completed,
            get_completion_summary,
        )

        # Set meta tools context to prevent task creation during task execution
        from xpressai.tools.builtin.meta import set_managers
        set_managers(
            self.task_board,
            self.memory_manager,
            self.sop_manager,
            agent_id=self.agent_id,
            in_task_context=True,
            task_id=task.id,
        )

        token = current_task_id.set(task.id)
        set_current_task_id(task.id)  # For thread-safe access from SDK tools
        reset_task_completion(task.id)

        # Clear backend's conversation history for fresh start
        if hasattr(self.backend, 'clear_history'):
            self.backend.clear_history()
            logger.debug(f"Cleared backend conversation history for task {task.id}")

        try:
            # Mark as in progress
            await self.task_board.update_status(
                task.id,
                TaskStatus.IN_PROGRESS,
                self.agent_id,
            )

            # Log task started
            if self.activity_manager:
                from xpressai.core.activity import EventType
                await self.activity_manager.log(
                    EventType.TASK_STARTED,
                    agent_id=self.agent_id,
                    data={"task_id": task.id, "title": task.title}
                )

            # Run memory hooks and inject context into backend (temporary - not saved)
            memory_context = await self._run_before_message_hooks(
                task.title + " " + (task.description or "")
            )
            if memory_context and hasattr(self.backend, "inject_memory"):
                await self.backend.inject_memory(memory_context)

            # Build the prompt (without memory - it's injected into system message separately)
            prompt = await self._build_task_prompt(task)

            # Log the task prompt (clean, without memory context which is temporary)
            if self.conversation_manager:
                await self.conversation_manager.add_message(
                    task.id, "system", f"Task prompt sent to agent:\n\n{prompt}"
                )

            # Execute with tool loop
            final_response, hit_max_iterations = await self._execute_with_tools(task.id, prompt)

            # Clear temporary memory injection after execution
            if hasattr(self.backend, "clear_injected_memory"):
                await self.backend.clear_injected_memory()

            # Log the final response
            if self.conversation_manager and final_response:
                await self.conversation_manager.add_message(task.id, "agent", final_response)

            # Run after_message hooks (e.g., memory_remember)
            conversation_for_hooks = [
                {"role": "user", "content": prompt},
                {"role": "assistant", "content": final_response or ""},
            ]
            await self._run_after_message_hooks(conversation_for_hooks)

            # Determine final status based on task completion
            if is_task_completed():
                # Clean up retry counter on successful completion/failure
                self._completion_retries.pop(task.id, None)

                summary = get_completion_summary()
                if summary and summary.startswith("FAILED:"):
                    # Agent explicitly marked task as failed
                    logger.info(f"Agent {self.agent_id} explicitly failed task: {task.title}")
                    await self.task_board.update_status(task.id, TaskStatus.BLOCKED)
                    if self.activity_manager:
                        from xpressai.core.activity import EventType
                        await self.activity_manager.log(
                            EventType.TASK_FAILED,
                            agent_id=self.agent_id,
                            data={"task_id": task.id, "title": task.title, "reason": summary}
                        )
                else:
                    # Agent explicitly completed task
                    logger.info(f"Agent {self.agent_id} completed task: {task.title}")
                    await self.task_board.update_status(task.id, TaskStatus.COMPLETED)
                    if self.activity_manager:
                        from xpressai.core.activity import EventType
                        await self.activity_manager.log(
                            EventType.TASK_COMPLETED,
                            agent_id=self.agent_id,
                            data={"task_id": task.id, "title": task.title, "summary": summary}
                        )
            elif hit_max_iterations:
                # Hit max iterations without calling complete_task
                logger.warning(f"Agent {self.agent_id} hit max iterations without completing task")
                if self.conversation_manager:
                    await self.conversation_manager.add_message(
                        task.id, "system",
                        "Task did not complete: hit maximum tool iterations without calling complete_task"
                    )
                await self.task_board.update_status(task.id, TaskStatus.BLOCKED)
                if self.activity_manager:
                    from xpressai.core.activity import EventType
                    await self.activity_manager.log(
                        EventType.TASK_FAILED,
                        agent_id=self.agent_id,
                        data={"task_id": task.id, "title": task.title, "reason": "max_iterations"}
                    )
            else:
                # No tools used and no completion - check retry count
                retries = self._completion_retries.get(task.id, 0) + 1
                self._completion_retries[task.id] = retries

                if retries >= self.max_completion_retries:
                    # Too many retries - auto-fail the task
                    logger.error(
                        f"Agent {self.agent_id} failed to complete/fail task after {retries} attempts, auto-failing"
                    )
                    if self.conversation_manager:
                        await self.conversation_manager.add_message(
                            task.id, "system",
                            f"Task auto-failed: Agent could not call complete_task or fail_task after {retries} attempts"
                        )
                    await self.task_board.update_status(task.id, TaskStatus.BLOCKED)
                    # Clean up retry counter
                    del self._completion_retries[task.id]
                    if self.activity_manager:
                        from xpressai.core.activity import EventType
                        await self.activity_manager.log(
                            EventType.TASK_FAILED,
                            agent_id=self.agent_id,
                            data={"task_id": task.id, "title": task.title, "reason": "completion_retry_limit"}
                        )
                else:
                    # Prompt agent to finish properly
                    logger.warning(
                        f"Agent {self.agent_id} did not complete task properly, prompting to finish "
                        f"(attempt {retries}/{self.max_completion_retries})"
                    )
                    if self.conversation_manager:
                        await self.conversation_manager.add_message(
                            task.id, "user",
                            "Please call complete_task if you're done, fail_task if you cannot proceed, "
                            "or continue using tools if more work is needed."
                        )
                    # Keep as pending so agent picks it up again
                    await self.task_board.update_status(task.id, TaskStatus.PENDING)

        except Exception as e:
            logger.error(f"Agent {self.agent_id} task failed: {e}")

            # Log the error
            if self.conversation_manager:
                await self.conversation_manager.add_message(
                    task.id, "system", f"Task failed: {e}"
                )

            # Mark as blocked (could retry later)
            await self.task_board.update_status(task.id, TaskStatus.BLOCKED)

            # Log task failure
            if self.activity_manager:
                from xpressai.core.activity import EventType
                await self.activity_manager.log(
                    EventType.TASK_FAILED,
                    agent_id=self.agent_id,
                    data={"task_id": task.id, "title": task.title, "error": str(e)}
                )

        finally:
            # Reset task context
            current_task_id.reset(token)
            set_current_task_id(None)
            self._current_task = None

            # Reset meta tools context (allow task creation again in chat mode)
            from xpressai.tools.builtin.meta import set_managers
            set_managers(
                self.task_board,
                self.memory_manager,
                self.sop_manager,
                agent_id=self.agent_id,
                in_task_context=False,
                task_id=None,
            )

    async def _execute_with_tools(self, task_id: str, initial_prompt: str) -> tuple[str, bool]:
        """Execute a prompt with tool calling loop.

        Args:
            task_id: Task ID for logging
            initial_prompt: The initial prompt to send

        Returns:
            Tuple of (final_response, hit_max_iterations)
        """
        # Check if backend supports tool parsing
        has_tool_support = (
            hasattr(self.backend, 'parse_tool_calls') and
            hasattr(self.backend, 'execute_tool') and
            self.tool_registry is not None
        )

        # Check if using native tool format
        is_native = (
            hasattr(self.backend, '_tool_format') and
            self.backend._tool_format == "native" and
            hasattr(self.backend, 'send_native_with_tools')
        )

        if is_native:
            return await self._execute_with_native_tools(task_id, initial_prompt)
        else:
            return await self._execute_with_text_tools(task_id, initial_prompt, has_tool_support)

    async def _execute_with_native_tools(self, task_id: str, initial_prompt: str) -> tuple[str, bool]:
        """Execute using native OpenAI-compatible tool calling.

        Args:
            task_id: Task ID for logging
            initial_prompt: The initial prompt

        Returns:
            Tuple of (final_response, hit_max_iterations)
        """
        from xpressai.tools.builtin.task_control import is_task_completed

        all_responses = []
        iterations = 0
        current_prompt = initial_prompt
        no_tool_retries = 0
        max_no_tool_retries = 3  # Retry up to 3 times if no tools used
        total_tools_used = 0
        is_continuation = False  # First call is not a continuation

        while iterations < self.max_tool_calls:
            iterations += 1

            # Get response with native tool calls
            content, tool_calls = await self.backend.send_native_with_tools(
                current_prompt, is_continuation=is_continuation
            )

            # Record usage for budget tracking
            await self._record_usage(current_prompt, content or "", "task_execution")

            logger.debug(f"Agent response (iteration {iterations}): {content[:200] if content else '[no content]'}...")

            if content:
                all_responses.append(content)

            # Check if task was completed via tool
            if is_task_completed():
                logger.info("Task marked as completed via complete_task tool")
                break

            if not tool_calls:
                # No tool calls - check if we should retry
                if total_tools_used == 0 and no_tool_retries < max_no_tool_retries:
                    no_tool_retries += 1
                    logger.warning(f"No tools used, prompting agent to use tools (retry {no_tool_retries}/{max_no_tool_retries})")

                    # Prompt the agent to use tools - this IS a user message
                    is_continuation = False
                    current_prompt = (
                        "You must use tools to complete this task. Do not just describe what you would do - "
                        "actually call the tools. Available tools include: write_file, read_file, list_directory, "
                        "execute_command, and complete_task. When you are done, call complete_task with a summary."
                    )
                    continue
                else:
                    # Either tools were used or max retries reached
                    break

            # Execute tool calls and add results to conversation
            for tool_name, arguments, tool_call_id in tool_calls:
                logger.info(f"Executing tool: {tool_name} with args: {arguments}")
                total_tools_used += 1

                # Log tool call to conversation (agent makes the call)
                if self.conversation_manager:
                    await self.conversation_manager.add_message(
                        task_id, "agent",
                        f"Calling tool: {tool_name}({arguments})"
                    )

                result = await self.backend.execute_tool(tool_name, arguments)

                logger.info(f"Tool {tool_name} result: {result[:200] if len(result) > 200 else result}")

                # Add tool result to backend's conversation history
                self.backend.add_tool_result(tool_call_id, tool_name, result)

                # Log tool result to conversation (background/collapsible)
                if self.conversation_manager:
                    result_preview = result[:500] + "..." if len(result) > 500 else result
                    await self.conversation_manager.add_message(
                        task_id, "tool",
                        f"{tool_name}: {result_preview}"
                    )

                # Check if task was completed
                if is_task_completed():
                    break

            # Check if task was completed via tool
            if is_task_completed():
                break

            # Run memory hooks between tool iterations
            # This allows the agent to learn from tool results and recall relevant context
            await self._run_inter_iteration_memory(task_id, tool_calls)

            # Continue conversation - the tool results are in the history
            # Mark as continuation so we don't add another user message
            is_continuation = True
            current_prompt = ""  # Not used for continuation

        hit_max = iterations >= self.max_tool_calls and not is_task_completed()
        if hit_max:
            logger.warning(f"Agent {self.agent_id} hit max tool iterations ({self.max_tool_calls})")
            all_responses.append(f"\n\n[Stopped after {self.max_tool_calls} tool iterations without completion]")

        return "\n\n".join(all_responses) if all_responses else "", hit_max

    async def _execute_with_text_tools(
        self, task_id: str, initial_prompt: str, has_tool_support: bool
    ) -> tuple[str, bool]:
        """Execute using text-based tool calling (xml/json format).

        Args:
            task_id: Task ID for logging
            initial_prompt: The initial prompt
            has_tool_support: Whether backend supports tools

        Returns:
            Tuple of (final_response, hit_max_iterations)
        """
        from xpressai.tools.builtin.task_control import is_task_completed

        current_prompt = initial_prompt
        all_responses = []
        iterations = 0
        no_tool_retries = 0
        max_no_tool_retries = 3
        total_tools_used = 0
        response = ""

        while iterations < self.max_tool_calls:
            iterations += 1

            # Get response from model
            response_parts = []
            async for chunk in self.backend.send(current_prompt):
                response_parts.append(chunk)
            response = "".join(response_parts)

            # Record usage for budget tracking
            await self._record_usage(current_prompt, response, "task_execution")

            logger.debug(f"Agent response (iteration {iterations}): {response[:200]}...")

            # If no tool support, just return the response
            if not has_tool_support:
                return response, False

            # Check if task was completed
            if is_task_completed():
                all_responses.append(response)
                break

            # Parse tool calls from response
            tool_calls = self.backend.parse_tool_calls(response)

            if not tool_calls:
                # No tool calls - check if we should retry
                if total_tools_used == 0 and no_tool_retries < max_no_tool_retries:
                    no_tool_retries += 1
                    logger.warning(f"No tools used, prompting agent to use tools (retry {no_tool_retries}/{max_no_tool_retries})")
                    current_prompt = (
                        "You must use tools to complete this task. Do not just describe what you would do - "
                        "actually call the tools using the XML format: <tool_name>{\"arg\": \"value\"}</tool_name>. "
                        "When you are done, call <complete_task>{\"summary\": \"what you did\"}</complete_task>."
                    )
                    continue
                else:
                    all_responses.append(response)
                    break

            # Execute tool calls
            tool_results = []
            for tool_name, arguments in tool_calls:
                logger.info(f"Executing tool: {tool_name} with args: {arguments}")
                total_tools_used += 1

                # Log tool call to conversation (agent makes the call)
                if self.conversation_manager:
                    await self.conversation_manager.add_message(
                        task_id, "agent",
                        f"Calling tool: {tool_name}({arguments})"
                    )

                result = await self.backend.execute_tool(tool_name, arguments)
                tool_results.append((tool_name, result))

                logger.info(f"Tool {tool_name} result: {result[:200] if len(result) > 200 else result}")

                # Log tool result to conversation (background/collapsible)
                if self.conversation_manager:
                    result_preview = result[:500] + "..." if len(result) > 500 else result
                    await self.conversation_manager.add_message(
                        task_id, "tool",
                        f"{tool_name}: {result_preview}"
                    )

                # Check if task was completed
                if is_task_completed():
                    break

            if is_task_completed():
                break

            # Run memory hooks between tool iterations
            # Convert tool_calls format for memory hook
            tool_calls_for_memory = [(name, args, None) for name, args in tool_calls]
            await self._run_inter_iteration_memory(task_id, tool_calls_for_memory)

            # Build follow-up prompt with tool results
            results_text = "\n\n".join([
                f"Result of {name}:\n{result}"
                for name, result in tool_results
            ])
            current_prompt = f"Tool execution results:\n\n{results_text}\n\nContinue with the task. Remember to call complete_task when done."

        hit_max = iterations >= self.max_tool_calls and not is_task_completed()
        if hit_max:
            logger.warning(f"Agent {self.agent_id} hit max tool iterations ({self.max_tool_calls})")
            all_responses.append(f"\n\n[Stopped after {self.max_tool_calls} tool iterations without completion]")

        return "\n\n".join(all_responses) if all_responses else response, hit_max

    async def _run_before_message_hooks(self, message: str) -> str:
        """Run before_message hooks like memory_recall.

        Args:
            message: The incoming message/task description

        Returns:
            Combined context from hooks to inject into prompt
        """
        logger.info(f"_run_before_message_hooks called for agent {self.agent_id}")
        logger.info(f"  agent_config: {self.agent_config is not None}")
        logger.info(f"  agent_config.hooks: {self.agent_config.hooks if self.agent_config else None}")

        if not self.agent_config or not self.agent_config.hooks:
            logger.info("  -> No agent_config or hooks, returning")
            return ""

        hooks = self.agent_config.hooks.before_message
        logger.info(f"  before_message hooks: {hooks}")
        if not hooks:
            logger.info("  -> No before_message hooks, returning")
            return ""

        logger.info(f"  memory_manager: {self.memory_manager is not None}")
        logger.info(f"  memory_config: {self.memory_config is not None}")
        if not self.memory_manager or not self.memory_config:
            logger.info("Memory hooks configured but memory system not available")
            return ""

        logger.info("  -> Running memory hooks!")

        # Create LLM callback for hooks that need LLM
        llm_callback = await self._create_llm_callback()

        try:
            # Import memory_recall directly to get debug info
            from xpressai.memory.hooks import memory_recall

            for hook_name in hooks:
                if hook_name == "memory_recall":
                    result = await memory_recall(
                        agent_id=self.agent_id,
                        message=message,
                        memory_manager=self.memory_manager,
                        memory_config=self.memory_config,
                        llm_callback=llm_callback,
                    )

                    context = result.get("context", "")
                    debug = result.get("debug", {})

                    # Log hook activity to task conversation
                    if self.conversation_manager and self._current_task:
                        log_parts = []
                        log_parts.append(f"Search query: {debug.get('search_query', 'N/A')}")
                        log_parts.append(f"Results found: {debug.get('results_count', 0)}")

                        if debug.get("memories"):
                            log_parts.append("\nMemories retrieved:")
                            for mem in debug["memories"]:
                                log_parts.append(f"  - {mem['summary']} (score: {mem['score']:.2f})")

                        if debug.get("error"):
                            log_parts.append(f"\nError: {debug['error']}")

                        await self.conversation_manager.add_message(
                            self._current_task.id, "hook",
                            "memory_recall:\n" + "\n".join(log_parts)
                        )

                    return context
                else:
                    logger.warning(f"Unknown before_message hook: {hook_name}")

            return ""
        except Exception as e:
            logger.error(f"Error running before_message hooks: {e}")
            return ""

    async def _run_after_message_hooks(self, conversation: list[dict]) -> None:
        """Run after_message hooks like memory_remember.

        Args:
            conversation: List of conversation messages
        """
        if not self.agent_config or not self.agent_config.hooks:
            return

        hooks = self.agent_config.hooks.after_message
        if not hooks:
            return

        if not self.memory_manager or not self.memory_config:
            logger.debug("Memory hooks configured but memory system not available")
            return

        # Create LLM callback for hooks that need LLM
        llm_callback = await self._create_llm_callback()

        try:
            from xpressai.memory.hooks import memory_remember

            for hook_name in hooks:
                if hook_name == "memory_remember":
                    stored = await memory_remember(
                        agent_id=self.agent_id,
                        conversation=conversation,
                        memory_manager=self.memory_manager,
                        memory_config=self.memory_config,
                        llm_callback=llm_callback,
                    )

                    # Log hook activity to task conversation
                    if self.conversation_manager and self._current_task:
                        hook_msg = "memory_remember: Stored new memory" if stored else "memory_remember: Nothing to remember"
                        await self.conversation_manager.add_message(
                            self._current_task.id, "hook", hook_msg
                        )
                else:
                    logger.warning(f"Unknown after_message hook: {hook_name}")
        except Exception as e:
            logger.error(f"Error running after_message hooks: {e}")

    async def _run_inter_iteration_memory(self, task_id: str, tool_calls: list) -> None:
        """Run memory hooks between tool iterations.

        This allows the agent to:
        1. Remember lessons from tool results (via memory_remember)
        2. Recall relevant context for the next iteration (via memory_recall)

        Args:
            task_id: Task ID for logging
            tool_calls: List of (tool_name, arguments, tool_call_id) tuples
        """
        if not self.agent_config or not self.agent_config.hooks:
            return

        if not self.memory_manager or not self.memory_config:
            return

        # Build a summary of what just happened for memory_remember
        tool_summary = []
        for tool_name, arguments, _ in tool_calls:
            tool_summary.append(f"Called {tool_name} with {arguments}")

        if not tool_summary:
            return

        try:
            llm_callback = await self._create_llm_callback()

            # Run memory_remember on the tool execution
            if "memory_remember" in self.agent_config.hooks.after_message:
                from xpressai.memory.hooks import memory_remember

                conversation = [
                    {"role": "assistant", "content": "I executed the following tools:\n" + "\n".join(tool_summary)}
                ]

                stored = await memory_remember(
                    agent_id=self.agent_id,
                    conversation=conversation,
                    memory_manager=self.memory_manager,
                    memory_config=self.memory_config,
                    llm_callback=llm_callback,
                )

                if self.conversation_manager and self._current_task:
                    hook_msg = "memory_remember (inter-iteration): Stored lesson" if stored else "memory_remember (inter-iteration): Nothing to store"
                    await self.conversation_manager.add_message(
                        self._current_task.id, "hook", hook_msg
                    )

            # Run memory_recall to get context for next iteration
            if "memory_recall" in self.agent_config.hooks.before_message:
                from xpressai.memory.hooks import memory_recall

                # Use the task title/description as the search context
                task = self._current_task
                search_context = f"{task.title} {task.description or ''}" if task else "tool execution"

                result = await memory_recall(
                    agent_id=self.agent_id,
                    message=search_context,
                    memory_manager=self.memory_manager,
                    memory_config=self.memory_config,
                    llm_callback=llm_callback,
                )

                context = result.get("context", "")
                debug = result.get("debug", {})

                if self.conversation_manager and self._current_task:
                    log_parts = [f"Inter-iteration recall - Results: {debug.get('results_count', 0)}"]
                    if debug.get("memories"):
                        for mem in debug["memories"]:
                            log_parts.append(f"  - {mem['summary']}")

                    await self.conversation_manager.add_message(
                        self._current_task.id, "hook",
                        "memory_recall (inter-iteration):\n" + "\n".join(log_parts)
                    )

                # Inject memory context into the backend if available
                if context and hasattr(self.backend, "inject_memory"):
                    await self.backend.inject_memory(context)

        except Exception as e:
            logger.error(f"Error running inter-iteration memory hooks: {e}")

    async def _create_llm_callback(self):
        """Create an async callback for hooks to call the LLM.

        Uses a dedicated memory backend if available to avoid polluting
        the main agent's conversation history.

        Returns:
            Async function that sends a prompt to the LLM and returns the response
        """
        # Try to get a dedicated memory backend
        memory_backend = None
        if self.memory_backend_factory:
            memory_backend = await self.memory_backend_factory()

        if memory_backend:
            async def llm_callback(prompt: str) -> str:
                """Send prompt to LLM using dedicated memory backend."""
                memory_backend.clear_history()
                response_parts = []
                async for chunk in memory_backend.send(prompt):
                    response_parts.append(chunk)
                return "".join(response_parts)
        else:
            # Fallback to main backend (not ideal but works)
            async def llm_callback(prompt: str) -> str:
                """Send prompt to LLM and get response."""
                response_parts = []
                async for chunk in self.backend.send(prompt):
                    response_parts.append(chunk)
                return "".join(response_parts)

        return llm_callback

    async def _build_task_prompt(self, task: Task) -> str:
        """Build the prompt for a task, incorporating SOP and conversation history.

        Note: Memory context is NOT included here - it's injected temporarily via
        backend.inject_memory() so the LLM sees it but it's not persisted in the
        conversation history.
        """
        parts = []

        # If task has an SOP, load and include it
        if task.sop_id and self.sop_manager:
            sop = self.sop_manager.get(task.sop_id)
            if sop:
                parts.append(self._format_sop_prompt(sop, task))
            else:
                logger.warning(f"SOP not found: {task.sop_id}")

        # Add the task itself
        parts.append(f"# Task: {task.title}")
        if task.description:
            parts.append(f"\n{task.description}")

        # Add any context
        if task.context:
            parts.append(f"\n## Context\n{task.context}")

        # Add conversation history if available
        if self.conversation_manager:
            conversation = await self.conversation_manager.get_conversation_context(task.id)
            if conversation:
                parts.append(f"\n## Conversation History\n{conversation}")

        return "\n\n".join(parts)

    def _format_sop_prompt(self, sop: SOP, task: Task) -> str:
        """Format an SOP as part of the prompt."""
        parts = [f"# Standard Operating Procedure: {sop.name}"]

        if sop.summary:
            parts.append(f"\n{sop.summary}")

        if sop.steps:
            parts.append("\n## Steps to follow:")
            for i, step in enumerate(sop.steps, 1):
                parts.append(f"\n{i}. {step.prompt}")
                if step.tools:
                    parts.append(f"   Tools available: {', '.join(step.tools)}")

        return "\n".join(parts)

    @property
    def is_running(self) -> bool:
        """Whether the runner is active."""
        return self._running

    @property
    def current_task(self) -> Task | None:
        """The task currently being executed, if any."""
        return self._current_task
