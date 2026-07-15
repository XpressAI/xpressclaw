# Configuration Reference

Xpressclaw stores local control-plane configuration in `xpressclaw.yaml`.
Create and edit sessions through the web UI; it validates and applies changes
without requiring users to hand-author the legacy agent-layer fields.

`xpressclaw init` writes an empty starting point:

```yaml
system:
  isolation: docker
  workspace_dir: .

agents: []
```

## Native session representation

Internally, durable sessions remain in the `agents` array for file and API
compatibility. New entries use only the native runner fields:

```yaml
agents:
  - name: website-maintainer
    display_name: Website maintainer
    backend: codex
    runner:
      kind: codex
      image: ghcr.io/xpressai/xpressclaw-runner-codex:latest
      workspace: /home/me/projects/site
      subscription_auth: true
      max_turns: 100
    volumes: []
```

### Runner fields

| Field | Description |
|---|---|
| `kind` | `codex`, `claude`, `opencode`, or `custom` |
| `image` | Product-specific worker image or compatible derivative |
| `workspace` | Host project mounted read-write at `/workspace` |
| `subscription_auth` | Reuse the product CLI's host login directory |
| `max_turns` | Product turn limit where supported |
| `command` | Argument list for a custom adapter; supports `{prompt}` and `{workspace}` |

Additional `volumes` use `host:container` or `host:container:ro` syntax.

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
