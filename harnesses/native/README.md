# Native runner images

Each directory builds one ACP-compatible agent product:

```bash
docker buildx build --load -t xpressclaw-runner-codex:latest codex
docker buildx build --load -t xpressclaw-runner-claude:latest claude
docker buildx build --load -t xpressclaw-runner-opencode:latest opencode
```

Run these commands from `harnesses/native`, or use the paths documented in the
repository README from the repository root. With Podman, use `podman build`
and the same local tags.

The default `runner` stage stays minimal. For a project that needs the host
Docker-compatible engine, build the separate `runner-host` stage:

```bash
docker buildx build --load --target runner-host -t xpressclaw-runner-codex-docker:latest codex
docker buildx build --load --target runner-host -t xpressclaw-runner-claude-docker:latest claude
docker buildx build --load --target runner-host -t xpressclaw-runner-opencode-docker:latest opencode
```

These variants copy Docker CLI, Compose, and Buildx from the official Docker
CLI image. Xpressclaw mounts its detected Docker or rootless Podman Unix socket
only when a project explicitly enables host-engine access.

The control plane launches the image's ACP server and attaches over
bidirectional stdio. Tasks are sent with `session/prompt`, while fresh and
continued work use ACP session lifecycle methods. Codex and Claude use the
ACP Registry adapters; OpenCode exposes ACP directly.

## Customization

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
