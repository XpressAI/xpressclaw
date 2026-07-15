# Native runner images

Each directory builds one native agent product:

```bash
docker build -t ghcr.io/xpressai/xpressclaw-runner-codex:latest codex
docker build -t ghcr.io/xpressai/xpressclaw-runner-claude:latest claude
docker build -t ghcr.io/xpressai/xpressclaw-runner-opencode:latest opencode
```

Run these commands from `harnesses/native`, or use the paths documented in the
repository README from the repository root.

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
