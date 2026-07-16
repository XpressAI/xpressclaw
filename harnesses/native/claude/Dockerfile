FROM node:22-bookworm-slim

ARG CLAUDE_ACP_VERSION=0.59.0
LABEL io.xpressclaw.protocol="acp" \
    io.xpressclaw.agent="claude"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gh git openssh-client ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global --omit=dev "@agentclientprotocol/claude-agent-acp@${CLAUDE_ACP_VERSION}" \
    && npm cache clean --force

RUN claude-agent-acp --version

USER node
ENV HOME=/home/node \
    CI=1 \
    NO_COLOR=1
WORKDIR /workspace
ENTRYPOINT []
