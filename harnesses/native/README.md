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
or remote environment. XpressClaw will create and lease that environment to a
work attempt through a scoped control-plane interface. Runner containers will
not mount the host Docker socket: access to it is equivalent to host-level
control and would defeat the isolation boundary.
