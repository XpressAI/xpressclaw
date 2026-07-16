# Configuration Reference

Xpressclaw stores local control-plane configuration in `xpressclaw.yaml`.
Create and edit project-context sessions through the web UI. Xpressclaw does
not define identities, personas, system prompts, tools, or subagents for ACP
agents.

`xpressclaw init` writes an empty starting point:

```yaml
system:
  isolation: docker
  workspace_dir: .

agents: []
```

## ACP project representation

Internally, durable sessions remain in the `agents` array for file and API
compatibility. New entries use only the ACP runner fields:

```yaml
agents:
  - name: site-codex # internal ID derived from project + harness
    backend: codex
    runner:
      kind: codex
      image: ghcr.io/xpressai/xpressclaw-runner-codex:latest
      workspace: /home/me/projects/site
      subscription_auth: true
    volumes: []
```

### Runner fields

| Field | Description |
|---|---|
| `kind` | `codex`, `claude`, `opencode`, or `custom` |
| `image` | Product-specific ACP server image or compatible derivative |
| `workspace` | Host project mounted read-write at `/workspace` |
| `model` | Optional model value ID applied through ACP session configuration |
| `subscription_auth` | Reuse the built-in product's host login directory |
| `command` | ACP server argument list; required for custom agents and supports `{workspace}` |

Additional `volumes` use `host:container` or `host:container:ro` syntax.

The UI label is derived from the workspace folder (`site` above). The `name`
field is only a stable internal reference used by tasks, schedules, and
workflows. Codex and Claude receive no Xpressclaw profile or identity prompt;
they retain ownership of their own instructions, tools, and subagents. The
control plane sends task text as an ACP `session/prompt` and records standard
ACP updates. Older profile fields are removed automatically when a
configuration is loaded.

For another agent, set `kind: custom`, provide an image, and enter the command
that starts its ACP server over stdin/stdout. Authentication and credential
mounts for custom agents are explicit volumes because Xpressclaw does not know
their host directory conventions.

## Built-in images

| Product | Default image |
|---|---|
| Codex | `ghcr.io/xpressai/xpressclaw-runner-codex:latest` |
| Claude Code | `ghcr.io/xpressai/xpressclaw-runner-claude:latest` |
| OpenCode | `ghcr.io/xpressai/xpressclaw-runner-opencode:latest` |

The retired `xpressclaw-native-runner:latest` tag is migrated to the image for
the configured product when an older file is loaded.

## Multiple projects

Use a separate working directory and `xpressclaw.yaml` for each independent
control plane, or create multiple sessions with different project workspaces in
one control plane. Select the server working directory with
`xpressclaw up --workdir /path/to/control-plane`.
