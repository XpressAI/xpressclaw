FROM docker:29.6.1-cli AS docker-cli

FROM node:24-bookworm-slim AS runner-base

ARG AGENT_KIND
ARG AGENT_PACKAGE
ARG AGENT_BINARY
# Companion agent for bridge packages (e.g. pi-acp needs pi); binary verified on PATH.
ARG AGENT_EXTRA_PACKAGES=""
ARG AGENT_EXTRA_BINARY=""
LABEL io.xpressclaw.protocol="acp-xpressclaw-v2" \
    io.xpressclaw.agent="${AGENT_KIND}"

RUN test -n "${AGENT_KIND}" \
    && test -n "${AGENT_PACKAGE}" \
    && test -n "${AGENT_BINARY}" \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gh git openssh-client ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && install -d /opt/xpressclaw/libexec \
    && mv /usr/bin/gh /opt/xpressclaw/libexec/gh \
    && npm install --global --omit=dev "${AGENT_PACKAGE}" ${AGENT_EXTRA_PACKAGES} \
    && npm cache clean --force \
    && command -v "${AGENT_BINARY}" \
    && { [ -z "${AGENT_EXTRA_BINARY}" ] || command -v "${AGENT_EXTRA_BINARY}"; }

COPY --chmod=0755 common/mcp-github.mjs /opt/xpressclaw/mcp-github.mjs
COPY --chmod=0755 common/mcp-xpressclaw.mjs /opt/xpressclaw/mcp-xpressclaw.mjs
COPY --chmod=0755 common/git-credential-xpressclaw /usr/local/bin/git-credential-xpressclaw

RUN /opt/xpressclaw/libexec/gh --version \
    && node --check /opt/xpressclaw/mcp-github.mjs \
    && node --check /opt/xpressclaw/mcp-xpressclaw.mjs

USER node
ENV HOME=/home/node \
    CI=1 \
    NO_COLOR=1
WORKDIR /workspace
ENTRYPOINT []

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
