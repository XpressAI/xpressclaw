# Configuration Reference

XpressAI is configured through an `xpressai.yaml` file in your project directory. Run `xpressai init` to generate one with sensible defaults.

## Complete Example

```yaml
system:
  budget:
    daily: $20.00
    on_exceeded: pause

  isolation: docker

agents:
  - name: atlas
    backend: claude-code
    role: |
      You are a helpful coding assistant.
    autonomy: high

tools:
  builtin:
    filesystem: ~/agent-workspace
    web_browser: true
    shell:
      enabled: true
      allowed_commands: [git, npm, python, pip]

  mcp:
    - name: github
      command: npx
      args: ["-y", "@modelcontextprotocol/server-github"]
      env:
        GITHUB_TOKEN: ${GITHUB_TOKEN}

memory:
  near_term_slots: 8
  eviction: least-recently-relevant
```

## Section Reference

### `system`

Global system configuration.

#### `system.budget`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `daily` | string | `$20.00` | Daily spending limit (e.g., `$10.00`, `$5.50`) |
| `on_exceeded` | string | `pause` | Action when budget exceeded: `pause`, `alert`, `stop` |

**Budget actions:**
- `pause` - Agent pauses until next budget period
- `alert` - Log a warning but continue running
- `stop` - Stop the agent completely

#### `system.isolation`

| Value | Description |
|-------|-------------|
| `docker` | Run agents in Docker containers (recommended for production) |
| `none` | Run agents in the host environment |

### `agents`

List of agent configurations. Each agent runs independently.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Unique identifier for the agent |
| `backend` | string | Yes | Agent backend to use |
| `role` | string | No | System prompt describing the agent's purpose |
| `autonomy` | string | No | Autonomy level: `high`, `medium`, `low` |

#### Supported Backends

| Backend | Description |
|---------|-------------|
| `claude-code` | Claude Agent SDK (recommended) |
| `claude` | Anthropic Claude API directly |
| `openai` | OpenAI API |
| `local` | Local model via Ollama |
| `llama-cpp` | Local model via llama.cpp |

#### Example: Multiple Agents

```yaml
agents:
  - name: coder
    backend: claude-code
    role: |
      You are a senior software engineer. You write clean,
      well-tested code and follow best practices.
    autonomy: high

  - name: reviewer
    backend: claude-code
    role: |
      You review code for bugs, security issues, and style.
      You provide constructive feedback.
    autonomy: medium
```

### `tools`

Tool configuration for agents.

#### `tools.builtin`

Built-in tools that come with XpressAI.

| Tool | Type | Description |
|------|------|-------------|
| `filesystem` | string | Path to allow filesystem access (e.g., `~/projects`) |
| `web_browser` | bool | Enable web browsing capability |
| `shell` | object | Shell command execution settings |

**Shell configuration:**

```yaml
tools:
  builtin:
    shell:
      enabled: true
      allowed_commands:
        - git
        - npm
        - python
        - pip
        - make
```

#### `tools.mcp`

MCP (Model Context Protocol) servers for external tools.

```yaml
tools:
  mcp:
    - name: github
      command: npx
      args: ["-y", "@modelcontextprotocol/server-github"]
      env:
        GITHUB_TOKEN: ${GITHUB_TOKEN}

    - name: slack
      command: npx
      args: ["-y", "@modelcontextprotocol/server-slack"]
      env:
        SLACK_TOKEN: ${SLACK_TOKEN}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Name for the MCP server |
| `command` | string | Yes | Command to start the server |
| `args` | list | No | Command arguments |
| `env` | object | No | Environment variables (supports `${VAR}` syntax) |

### `memory`

Memory system configuration.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `near_term_slots` | int | `8` | Number of near-term memory slots |
| `eviction` | string | `least-recently-relevant` | Eviction strategy |

**Eviction strategies:**
- `least-recently-relevant` - Evict memories that are least relevant to current context
- `least-recently-used` - Evict oldest accessed memories
- `fifo` - First in, first out

## Environment Variables

Use `${VAR_NAME}` syntax to reference environment variables:

```yaml
tools:
  mcp:
    - name: github
      env:
        GITHUB_TOKEN: ${GITHUB_TOKEN}
```

Required environment variables for backends:

| Backend | Variable | Description |
|---------|----------|-------------|
| `claude-code` | `ANTHROPIC_API_KEY` | Anthropic API key |
| `claude` | `ANTHROPIC_API_KEY` | Anthropic API key |
| `openai` | `OPENAI_API_KEY` | OpenAI API key |

## Data Directory

XpressAI stores data in `~/.xpressai/`:

```
~/.xpressai/
├── xpressai.db       # SQLite database (tasks, memory, budgets)
├── runtime.log       # Runtime logs
├── runtime.pid       # PID file for daemon mode
└── memory/           # Zettelkasten notes
```

## Multiple Configurations

You can have different configurations for different projects. Just create an `xpressai.yaml` in each project directory.

```bash
# In project A
cd ~/projects/webapp
xpressai init  # Creates xpressai.yaml for this project

# In project B
cd ~/projects/api
xpressai init  # Creates separate xpressai.yaml
```

Each project has its own agents, budgets, and task boards.
