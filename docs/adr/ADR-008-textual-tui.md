# ADR-008: Textual TUI

## Status
Accepted

## Context

While the web UI serves monitoring and management, many developers prefer staying in the terminal. We need a rich Terminal User Interface (TUI) for:
- Interactive agent conversations
- Real-time log streaming
- Quick status checks
- Task management
- Memory exploration

Options considered:
1. **Rich**: Good for output, limited interactivity
2. **Textual**: Full TUI framework, modern, async-native
3. **urwid**: Mature, but dated API
4. **blessed/curses**: Low-level, lots of boilerplate

**Textual** is the clear winner: modern Python, async-first, CSS-like styling, excellent documentation.

## Decision

We will build the TUI using **Textual** (from the creators of Rich).

### Application Structure

```
src/xpressai/tui/
├── __init__.py
├── app.py              # Main Textual application
├── screens/
│   ├── __init__.py
│   ├── dashboard.py    # Main dashboard screen
│   ├── agent.py        # Agent detail/chat screen
│   ├── tasks.py        # Task board screen
│   ├── memory.py       # Memory browser screen
│   └── logs.py         # Log viewer screen
├── widgets/
│   ├── __init__.py
│   ├── agent_card.py
│   ├── chat.py
│   ├── task_list.py
│   └── memory_tree.py
└── styles/
    └── app.tcss        # Textual CSS
```

### Main Application

```python
# app.py
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.widgets import Header, Footer, TabbedContent, TabPane

from .screens import DashboardScreen, AgentScreen, TasksScreen, MemoryScreen

class XpressAIApp(App):
    """XpressAI Terminal User Interface."""
    
    TITLE = "XpressAI"
    CSS_PATH = "styles/app.tcss"
    
    BINDINGS = [
        Binding("d", "switch_tab('dashboard')", "Dashboard"),
        Binding("a", "switch_tab('agents')", "Agents"),
        Binding("t", "switch_tab('tasks')", "Tasks"),
        Binding("m", "switch_tab('memory')", "Memory"),
        Binding("q", "quit", "Quit"),
        Binding("?", "help", "Help"),
    ]
    
    def __init__(self, runtime: Runtime):
        super().__init__()
        self.runtime = runtime
    
    def compose(self) -> ComposeResult:
        yield Header()
        
        with TabbedContent():
            with TabPane("Dashboard", id="dashboard"):
                yield DashboardScreen(self.runtime)
            with TabPane("Agents", id="agents"):
                yield AgentScreen(self.runtime)
            with TabPane("Tasks", id="tasks"):
                yield TasksScreen(self.runtime)
            with TabPane("Memory", id="memory"):
                yield MemoryScreen(self.runtime)
        
        yield Footer()
    
    def action_switch_tab(self, tab_id: str) -> None:
        self.query_one(TabbedContent).active = tab_id

def run_tui(runtime: Runtime):
    app = XpressAIApp(runtime)
    app.run()
```

### Dashboard Screen

```python
# screens/dashboard.py
from textual.app import ComposeResult
from textual.containers import Container, Horizontal, Vertical
from textual.widgets import Static, DataTable, Log
from textual.reactive import reactive

class DashboardScreen(Container):
    """Main dashboard with agent status, budget, and activity."""
    
    def __init__(self, runtime: Runtime):
        super().__init__()
        self.runtime = runtime
    
    def compose(self) -> ComposeResult:
        with Horizontal():
            # Left column: Agents
            with Vertical(id="agents-panel", classes="panel"):
                yield Static("⚡ Agents", classes="panel-title")
                yield AgentList(self.runtime)
            
            # Middle column: Activity log
            with Vertical(id="activity-panel", classes="panel"):
                yield Static("📋 Activity", classes="panel-title")
                yield ActivityLog(self.runtime)
            
            # Right column: Budget & Stats
            with Vertical(id="stats-panel", classes="panel"):
                yield Static("💰 Budget", classes="panel-title")
                yield BudgetWidget(self.runtime)
                yield Static("📊 Stats", classes="panel-title")
                yield StatsWidget(self.runtime)
    
    async def on_mount(self) -> None:
        # Start background updates
        self.set_interval(1.0, self.refresh_data)
    
    async def refresh_data(self) -> None:
        # Trigger child widget refreshes
        self.query_one(AgentList).refresh()
        self.query_one(BudgetWidget).refresh()

class AgentList(Container):
    """List of agents with status indicators."""
    
    def __init__(self, runtime: Runtime):
        super().__init__()
        self.runtime = runtime
    
    def compose(self) -> ComposeResult:
        yield DataTable()
    
    async def on_mount(self) -> None:
        table = self.query_one(DataTable)
        table.add_columns("Status", "Name", "Backend", "Actions")
        await self.refresh()
    
    async def refresh(self) -> None:
        table = self.query_one(DataTable)
        table.clear()
        
        agents = await self.runtime.list_agents()
        for agent in agents:
            status_icon = {
                "running": "🟢",
                "stopped": "⚫",
                "error": "🔴",
                "starting": "🟡",
            }.get(agent.status, "❓")
            
            table.add_row(
                status_icon,
                agent.name,
                agent.backend,
                "[Start] [Stop]"  # Clickable actions
            )

class ActivityLog(Log):
    """Streaming activity log."""
    
    def __init__(self, runtime: Runtime):
        super().__init__(highlight=True, markup=True)
        self.runtime = runtime
    
    async def on_mount(self) -> None:
        # Stream activity in background
        self.run_worker(self.stream_activity())
    
    async def stream_activity(self) -> None:
        async for event in self.runtime.activity_stream():
            timestamp = event.timestamp.strftime("%H:%M:%S")
            agent = f"[bold cyan]{event.agent}[/]"
            self.write_line(f"[dim]{timestamp}[/] {agent} {event.message}")
```

### Agent Chat Screen

```python
# screens/agent.py
from textual.app import ComposeResult
from textual.containers import Container, Vertical, Horizontal
from textual.widgets import Static, Input, RichLog, Select
from textual.message import Message

class AgentScreen(Container):
    """Interactive chat with an agent."""
    
    class MessageSent(Message):
        def __init__(self, text: str):
            self.text = text
            super().__init__()
    
    def __init__(self, runtime: Runtime):
        super().__init__()
        self.runtime = runtime
        self.current_agent: str | None = None
    
    def compose(self) -> ComposeResult:
        with Horizontal():
            # Agent selector
            with Vertical(id="agent-selector", classes="sidebar"):
                yield Static("Select Agent", classes="sidebar-title")
                yield Select([], id="agent-select")
                yield Static("", id="agent-info")
            
            # Chat area
            with Vertical(id="chat-area"):
                yield ChatHistory(id="chat-history")
                yield ChatInput(id="chat-input")
    
    async def on_mount(self) -> None:
        # Populate agent selector
        agents = await self.runtime.list_agents()
        select = self.query_one("#agent-select", Select)
        select.set_options([(a.name, a.id) for a in agents])
    
    async def on_select_changed(self, event: Select.Changed) -> None:
        self.current_agent = event.value
        await self.load_agent_history()
    
    async def on_chat_input_submitted(self, event: Input.Submitted) -> None:
        if not self.current_agent:
            return
        
        message = event.value
        event.input.value = ""
        
        # Show user message
        history = self.query_one("#chat-history", ChatHistory)
        history.add_user_message(message)
        
        # Send to agent and stream response
        async for response in self.runtime.send_to_agent(
            self.current_agent, message
        ):
            history.add_assistant_chunk(response)

class ChatHistory(RichLog):
    """Scrollable chat history with formatted messages."""
    
    def add_user_message(self, text: str) -> None:
        self.write("[bold green]You:[/] " + text)
    
    def add_assistant_message(self, text: str) -> None:
        self.write("[bold blue]Agent:[/] " + text)
    
    def add_assistant_chunk(self, chunk: str) -> None:
        # Append to current message (streaming)
        self.write(chunk, expand=True)

class ChatInput(Input):
    """Input field for chat messages."""
    
    BINDINGS = [
        ("enter", "submit", "Send"),
        ("escape", "clear", "Clear"),
    ]
    
    def action_clear(self) -> None:
        self.value = ""
```

### Task Board Screen

```python
# screens/tasks.py
from textual.app import ComposeResult
from textual.containers import Container, Horizontal, Vertical, ScrollableContainer
from textual.widgets import Static, ListItem, ListView

class TasksScreen(Container):
    """Kanban-style task board."""
    
    def __init__(self, runtime: Runtime):
        super().__init__()
        self.runtime = runtime
    
    def compose(self) -> ComposeResult:
        with Horizontal(id="task-board"):
            yield TaskColumn("Pending", "pending", self.runtime)
            yield TaskColumn("In Progress", "in_progress", self.runtime)
            yield TaskColumn("Completed", "completed", self.runtime)

class TaskColumn(Vertical):
    """A column in the kanban board."""
    
    def __init__(self, title: str, status: str, runtime: Runtime):
        super().__init__(classes="task-column")
        self.title = title
        self.status = status
        self.runtime = runtime
    
    def compose(self) -> ComposeResult:
        yield Static(f"[bold]{self.title}[/]", classes="column-header")
        yield ScrollableContainer(
            ListView(id=f"tasks-{self.status}"),
            classes="task-list"
        )
    
    async def on_mount(self) -> None:
        await self.refresh_tasks()
    
    async def refresh_tasks(self) -> None:
        tasks = await self.runtime.get_tasks(status=self.status)
        list_view = self.query_one(f"#tasks-{self.status}", ListView)
        list_view.clear()
        
        for task in tasks:
            list_view.append(TaskItem(task))

class TaskItem(ListItem):
    """A single task item."""
    
    def __init__(self, task: Task):
        super().__init__()
        self.task = task
    
    def compose(self) -> ComposeResult:
        yield Static(f"[bold]{self.task.title}[/]")
        if self.task.description:
            yield Static(
                f"[dim]{self.task.description[:50]}...[/]",
                classes="task-description"
            )
        yield Static(
            f"[dim italic]{self.task.agent_id or 'Unassigned'}[/]",
            classes="task-meta"
        )
```

### Memory Browser Screen

```python
# screens/memory.py
from textual.app import ComposeResult
from textual.containers import Container, Horizontal, Vertical
from textual.widgets import Static, Input, Tree, TextArea

class MemoryScreen(Container):
    """Browse and search agent memories."""
    
    def __init__(self, runtime: Runtime):
        super().__init__()
        self.runtime = runtime
    
    def compose(self) -> ComposeResult:
        with Horizontal():
            # Left: Search and tree
            with Vertical(id="memory-nav", classes="sidebar"):
                yield Input(placeholder="Search memories...", id="memory-search")
                yield MemoryTree(self.runtime)
            
            # Right: Memory detail
            with Vertical(id="memory-detail"):
                yield Static("Select a memory", id="memory-title")
                yield TextArea(id="memory-content", read_only=True)
                yield Static("", id="memory-meta")
    
    async def on_input_submitted(self, event: Input.Submitted) -> None:
        query = event.value
        tree = self.query_one(MemoryTree)
        await tree.search(query)

class MemoryTree(Tree):
    """Tree view of memory hierarchy."""
    
    def __init__(self, runtime: Runtime):
        super().__init__("Memories")
        self.runtime = runtime
    
    async def on_mount(self) -> None:
        await self.load_layers()
    
    async def load_layers(self) -> None:
        self.clear()
        
        # Add layer nodes
        shared = self.root.add("📁 Shared")
        for memory in await self.runtime.get_memories(layer="shared"):
            shared.add_leaf(f"📝 {memory.summary[:30]}", data=memory)
        
        # User layers
        users = self.root.add("👥 Users")
        for user_id in await self.runtime.get_user_ids():
            user_node = users.add(f"👤 {user_id}")
            for memory in await self.runtime.get_memories(layer=f"user:{user_id}"):
                user_node.add_leaf(f"📝 {memory.summary[:30]}", data=memory)
    
    async def search(self, query: str) -> None:
        results = await self.runtime.search_memories(query)
        
        self.clear()
        search_node = self.root.add(f"🔍 Results for '{query}'")
        for memory, score in results:
            search_node.add_leaf(
                f"📝 {memory.summary[:30]} ({score:.2f})",
                data=memory
            )
```

### Styles (TCSS)

```css
/* styles/app.tcss */

Screen {
    background: $surface;
}

.panel {
    border: solid $primary;
    margin: 1;
    padding: 1;
    height: 100%;
}

.panel-title {
    text-style: bold;
    color: $primary;
    margin-bottom: 1;
}

.sidebar {
    width: 30;
    border-right: solid $primary-darken-2;
}

.sidebar-title {
    text-style: bold;
    padding: 1;
    background: $primary-darken-3;
}

#chat-area {
    width: 1fr;
}

#chat-history {
    height: 1fr;
    border: solid $primary-darken-2;
    margin: 1;
    padding: 1;
}

#chat-input {
    margin: 1;
}

.task-column {
    width: 1fr;
    margin: 1;
    border: solid $primary-darken-2;
}

.column-header {
    padding: 1;
    background: $primary-darken-3;
    text-align: center;
}

.task-list {
    height: 1fr;
}

TaskItem {
    padding: 1;
    margin: 1;
    background: $surface-darken-1;
}

TaskItem:hover {
    background: $surface-lighten-1;
}

.task-description {
    color: $text-muted;
}

.task-meta {
    color: $text-disabled;
}

#memory-content {
    height: 1fr;
}

#memory-meta {
    height: 5;
    background: $surface-darken-1;
    padding: 1;
}
```

### CLI Integration

```python
# cli/tui.py
import click
from ..tui.app import run_tui

@click.command()
@click.pass_context
def tui(ctx):
    """Launch the Terminal User Interface."""
    runtime = ctx.obj["runtime"]
    run_tui(runtime)
```

## Consequences

### Positive
- Rich, interactive terminal experience
- Async-native, works well with our runtime
- CSS-like styling is intuitive
- Keyboard-driven for power users
- Works over SSH
- Same Python codebase as backend

### Negative
- Terminal limitations (no images, limited colors)
- Learning curve for Textual specifics
- Testing TUI components is harder
- May need fallback for basic terminals

### Implementation Notes

1. Start with dashboard and agent list
2. Add chat interface next (core use case)
3. Build task board with keyboard navigation
4. Add memory browser with search
5. Consider "focus mode" for single-agent view

## Related ADRs
- ADR-007: HTMX Web UI (alternative interface)
- ADR-002: Agent Backend (chat integration)
