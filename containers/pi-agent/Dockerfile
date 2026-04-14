# Pi coding agent container for xpressclaw.
#
# Contains:
#   - pi-coding-agent (npm) — the agent runtime
#   - mcpfs (Go) — mounts xpressclaw MCP tools as filesystem
#   - Standard dev tools (git, curl, etc.)
#
# Designed to be converted to WASM via container2wasm for
# snapshotable, isolated agent execution.

FROM node:20-slim

# System dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl ca-certificates fuse3 libfuse3-dev \
    python3 python3-pip \
    && rm -rf /var/lib/apt/lists/*

# Install Go (for mcpfs)
RUN curl -fsSL https://go.dev/dl/go1.23.4.linux-amd64.tar.gz | tar -C /usr/local -xz
ENV PATH="/usr/local/go/bin:/root/go/bin:${PATH}"

# Install pi-coding-agent
RUN npm install -g @mariozechner/pi-coding-agent

# Install mcpfs
RUN go install github.com/airshelf/mcpfs/cmd/mcpfs@latest

# Create workspace and pi config directories
RUN mkdir -p /workspace /root/.pi/agent/extensions

# Install xpressclaw provider extension for pi
COPY extensions/xpressclaw-provider/ /root/.pi/agent/extensions/xpressclaw-provider/

# Pi settings: enable the extension and extra tools
RUN echo '{"extensions":["/root/.pi/agent/extensions/xpressclaw-provider"],"tools":{"grep":true,"find":true,"ls":true}}' > /root/.pi/agent/settings.json

# Default pi configuration
COPY AGENTS.md /workspace/AGENTS.md
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

WORKDIR /workspace
ENTRYPOINT ["/entrypoint.sh"]
