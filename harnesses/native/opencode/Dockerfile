FROM node:22-bookworm-slim

ARG OPENCODE_VERSION=1.18.2
LABEL io.xpressclaw.protocol="acp" \
    io.xpressclaw.agent="opencode"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gh git openssh-client ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global --omit=dev "opencode-ai@${OPENCODE_VERSION}" \
    && npm cache clean --force

RUN opencode --version

USER node
ENV HOME=/home/node \
    CI=1 \
    NO_COLOR=1
WORKDIR /workspace
ENTRYPOINT []
