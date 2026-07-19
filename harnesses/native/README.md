# Native runner images

Each directory builds one ACP-compatible agent product:

```bash
docker buildx build --load -f codex/Dockerfile -t xpressclaw-runner-codex:latest -t localhost/xpressclaw-runner-codex:latest .
docker buildx build --load -f claude/Dockerfile -t xpressclaw-runner-claude:latest -t localhost/xpressclaw-runner-claude:latest .
docker buildx build --load -f opencode/Dockerfile -t xpressclaw-runner-opencode:latest -t localhost/xpressclaw-runner-opencode:latest .
```

Run these commands from `harnesses/native`, or use the paths documented in the
repository README from the repository root. The duplicate local tag keeps the
same build discoverable through Docker and Podman's short-name conventions.

The default `runner` stage stays minimal. For a project that needs the host
Docker-compatible engine, build the separate `runner-host` stage:

```bash
docker buildx build --load --target runner-host -f codex/Dockerfile -t xpressclaw-runner-codex-docker:latest -t localhost/xpressclaw-runner-codex-docker:latest .
docker buildx build --load --target runner-host -f claude/Dockerfile -t xpressclaw-runner-claude-docker:latest -t localhost/xpressclaw-runner-claude-docker:latest .
docker buildx build --load --target runner-host -f opencode/Dockerfile -t xpressclaw-runner-opencode-docker:latest -t localhost/xpressclaw-runner-opencode-docker:latest .
```

These variants copy Docker CLI, Compose, and Buildx from the official Docker
CLI image. Xpressclaw mounts its detected Docker or rootless Podman Unix socket
only when a project explicitly enables host-engine access.

The control plane launches the image's ACP server and attaches over
bidirectional stdio. Tasks are sent with `session/prompt`, while fresh and
continued work use ACP session lifecycle methods. Codex and Claude use the
ACP Registry adapters; OpenCode exposes ACP directly.

## Publishing

`Build & Push Runner Images` publishes all six multi-architecture tags on a
push to `main` or a manual workflow dispatch. It then verifies every tag from
an unauthenticated job. GitHub creates a container package as private on its
first publication, so an XpressAI organization owner must change each new
package to **Public** once in its package settings. The release workflow will
not publish a desktop beta until all runner tags pass the anonymous check.

For a `github.com` origin, Xpressclaw discovers the host's existing `gh`
login and supplies Git credentials through `credential.helper`. The worker
still has the complete Git CLI. GitHub PR, review, check, and Actions
operations are exposed as one constrained, `gh`-shaped MCP tool; arbitrary
`gh api` and direct `gh` shell access are intentionally unavailable.

## Customization

With **Use host login and harness configuration** enabled, XpressClaw mounts
the product's normal host configuration directory into `/home/node` in the
worker. This includes installed Codex skills and plugins, Claude Code plugins,
hooks and custom agents, OpenCode configuration, and the subscription login.
Project-local configuration is read from the workspace. The session settings
UI can add other host-to-container mounts and environment values when an
extension uses a nonstandard location.

The control plane can also attach selected MCP servers through ACP. A stdio
server executable must exist at the configured absolute path inside this
image, so either extend the image or mount the server at that path. HTTP and
SSE servers are remote and do not increase the runner image size. ACP
advertised commands and session controls appear in task chat and workflows
after the harness has completed one discovery turn.

Runner images are intentionally product-specific. You can extend one while
the control-plane-managed development-environment interface is being built:

```dockerfile
FROM ghcr.io/xpressai/xpressclaw-runner-codex:latest

USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends openjdk-21-jdk-headless \
    && rm -rf /var/lib/apt/lists/*
USER node
```

Long term, project toolchains should live in a separate development container
or remote environment. Xpressclaw can currently expose the host engine as a
pragmatic trusted mode so an agent can run Compose or create those environments
itself. Socket access is equivalent to the engine user's host-level authority;
the worker container prevents accidental damage to its own root filesystem,
but does not isolate the host engine or host paths mounted through it.
