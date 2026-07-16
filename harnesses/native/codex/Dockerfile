FROM node:22-bookworm-slim

ARG CODEX_ACP_VERSION=1.1.4
LABEL io.xpressclaw.protocol="acp" \
    io.xpressclaw.agent="codex"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gh git openssh-client ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global --omit=dev "@agentclientprotocol/codex-acp@${CODEX_ACP_VERSION}" \
    && npm cache clean --force

RUN codex-acp --version

USER node
ENV HOME=/home/node \
    CI=1 \
    NO_COLOR=1
WORKDIR /workspace
ENTRYPOINT []
