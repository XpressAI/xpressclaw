FROM docker:29.6.1-cli AS docker-cli

FROM node:22-bookworm-slim AS runner-base

ARG CLAUDE_ACP_VERSION=0.59.0
LABEL io.xpressclaw.protocol="acp-xpressclaw-v2" \
    io.xpressclaw.agent="claude"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gh git openssh-client ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && install -d /opt/xpressclaw/libexec \
    && mv /usr/bin/gh /opt/xpressclaw/libexec/gh \
    && npm install --global --omit=dev "@agentclientprotocol/claude-agent-acp@${CLAUDE_ACP_VERSION}" \
    && npm cache clean --force

COPY --chmod=0755 common/mcp-github.mjs /opt/xpressclaw/mcp-github.mjs
COPY --chmod=0755 common/mcp-xpressclaw.mjs /opt/xpressclaw/mcp-xpressclaw.mjs
COPY --chmod=0755 common/git-credential-xpressclaw /usr/local/bin/git-credential-xpressclaw

RUN claude-agent-acp --version \
    && /opt/xpressclaw/libexec/gh --version \
    && node --check /opt/xpressclaw/mcp-github.mjs \
    && node --check /opt/xpressclaw/mcp-xpressclaw.mjs

USER node
ENV HOME=/home/node \
    CI=1 \
    NO_COLOR=1
WORKDIR /workspace
ENTRYPOINT []

# Opt-in image for trusted projects that need the host Docker-compatible
# engine. The default final stage below remains the smaller runner image.
FROM runner-base AS runner-host
USER root
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=docker-cli /usr/local/libexec/docker/cli-plugins /usr/local/libexec/docker/cli-plugins
LABEL io.xpressclaw.protocol="acp-xpressclaw-v2" \
    io.xpressclaw.container-engine="host"
RUN docker --version \
    && docker compose version \
    && docker buildx version \
    && test -x /opt/xpressclaw/mcp-github.mjs \
    && test -x /opt/xpressclaw/mcp-xpressclaw.mjs \
    && test -x /usr/local/bin/git-credential-xpressclaw \
    && test ! -e /usr/bin/gh
USER node

FROM runner-base AS runner
LABEL io.xpressclaw.protocol="acp-xpressclaw-v2"
RUN test -x /opt/xpressclaw/mcp-github.mjs \
    && test -x /opt/xpressclaw/mcp-xpressclaw.mjs \
    && test -x /usr/local/bin/git-credential-xpressclaw \
    && test ! -e /usr/bin/gh
