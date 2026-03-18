# Configuration Reference

The xpressclaw project is configured through an `xpressclaw.yaml` file (or `xpressai.yaml` for backwards compatibility) in your project directory. Run `xpressclaw init` to generate one with sensible defaults.

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
| `daily` | string | `$20.00` | Daily spending limit |
| `on_exceeded` | string | `pause` | Action: `pause`, `alert`, `stop` |

#### `system.isolation`

| Value | Description |
|-------|-------------|
| `docker` | Run agents in Docker containers (recommended) |
| `none` | Run agents in host environment |

### `agents`

List of agent configurations. Each agent runs independently.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Unique identifier |
| `backend` | string | Yes | Agent backend to use |
| `role` | string | No | System prompt |
| `autonomy` | string | No | Autonomy level: `high`, `medium`, `low` |

#### Supported Backends

| Backend | Description |
|---------|-------------|
| `claude-code` | Claude Agent SDK (recommended) |
| `generic` | Generic OpenAI-compatible API |
| `local` | Local model via Ollama |

### `tools`

Tool configuration for agents.

#### `tools.builtin`

| Tool | Type | Description |
|------|------|-------------|
| `filesystem` | string | Path to allow filesystem access |
| `web_browser` | bool | Enable web browsing |
| `shell` | object | Shell command execution |

#### `tools.mcp`

MCP (Model Context Protocol) servers for external tools.

### `memory`

Memory system configuration.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `near_term_slots` | int | `8` | Number of near-term memory slots |
| `eviction` | string | `least-recently-relevant` | Eviction strategy |

## Data Directory

The xpressclaw runtime stores data in your workspace directory (`.xpressai/`).

## Environment Variables

Use `${VAR_NAME}` syntax to reference environment variables in your config:

```yaml
tools:
  mcp:
    - name: github
      env:
        GITHUB_TOKEN: ${GITHUB_TOKEN}
```

Required for cloud backends:
- `ANTHROPIC_API_KEY` - Claude
- `OPENAI_API_KEY` - OpenAI

## Multiple Configurations

You can have different configurations for different projects. Just create an `xpressclaw.yaml` (or `xpressai.yaml`) in each project directory.
