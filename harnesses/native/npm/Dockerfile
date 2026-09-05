FROM docker:29.6.1-cli AS docker-cli

FROM node:24-bookworm-slim AS runner-base

ARG AGENT_KIND
ARG AGENT_PACKAGE
ARG AGENT_BINARY
ARG RUNNER_VERSION=development
# Companion agent for bridge packages (e.g. pi-acp needs pi); binary verified on PATH.
ARG AGENT_EXTRA_PACKAGES=""
ARG AGENT_EXTRA_BINARY=""
ARG AGENT_ACP_SMOKE=""
ARG AGENT_DSH_RUNTIME=""
ARG AGENT_DSH_PATH=""
ARG AGENT_NODE_PATH=""
ARG AGENT_DSH_SESSION_ROOT=""
ARG PI_MCP_PACKAGE="pi-mcp-adapter@latest"
LABEL io.xpressclaw.protocol="acp-xpressclaw-v2" \
    io.xpressclaw.agent="${AGENT_KIND}" \
    io.xpressclaw.runner.version="${RUNNER_VERSION}" \
    io.xpressclaw.pi-mcp="${AGENT_KIND}-config-v1"

RUN test -n "${AGENT_KIND}" \
    && test -n "${AGENT_PACKAGE}" \
    && test -n "${AGENT_BINARY}" \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gh git openssh-client ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && install -d /opt/xpressclaw/libexec \
    && mv /usr/bin/gh /opt/xpressclaw/libexec/gh \
    && PI_MCP_PACKAGE_TO_INSTALL="" \
    && { [ "${AGENT_KIND}" != pi ] || PI_MCP_PACKAGE_TO_INSTALL="${PI_MCP_PACKAGE}"; } \
    && npm install --global --omit=dev "${AGENT_PACKAGE}" ${AGENT_EXTRA_PACKAGES} ${PI_MCP_PACKAGE_TO_INSTALL} \
    && npm cache clean --force \
    && command -v "${AGENT_BINARY}" \
    && { [ -z "${AGENT_EXTRA_BINARY}" ] || command -v "${AGENT_EXTRA_BINARY}"; } \
    && { [ "${AGENT_KIND}" != pi ] || test -f /usr/local/lib/node_modules/pi-mcp-adapter/index.ts; }

COPY --chmod=0755 common/mcp-github.mjs /opt/xpressclaw/mcp-github.mjs
COPY --chmod=0755 common/mcp-xpressclaw.mjs /opt/xpressclaw/mcp-xpressclaw.mjs
COPY --chmod=0755 common/pi-with-mcp /opt/xpressclaw/pi-with-mcp
COPY --chmod=0755 common/prepare-deepseek-harness.mjs /opt/xpressclaw/prepare-deepseek-harness.mjs
COPY --chmod=0755 common/verify-acp-stdio.mjs /opt/xpressclaw/verify-acp-stdio.mjs
COPY --chmod=0755 common/git-credential-xpressclaw /usr/local/bin/git-credential-xpressclaw

RUN /opt/xpressclaw/libexec/gh --version \
    && node --check /opt/xpressclaw/mcp-github.mjs \
    && node --check /opt/xpressclaw/mcp-xpressclaw.mjs \
    && node --check /opt/xpressclaw/prepare-deepseek-harness.mjs \
    && node --check /opt/xpressclaw/verify-acp-stdio.mjs \
    && { [ "${AGENT_DSH_RUNTIME}" != "1" ] || node /opt/xpressclaw/prepare-deepseek-harness.mjs; } \
    && npm cache clean --force \
    && test -x /opt/xpressclaw/pi-with-mcp

USER node
ENV HOME=/home/node \
    CI=1 \
    NO_COLOR=1 \
    DSH_PATH="${AGENT_DSH_PATH}" \
    NODE_PATH="${AGENT_NODE_PATH}" \
    DSH_SESSION_ROOT="${AGENT_DSH_SESSION_ROOT}"
RUN if [ "${AGENT_DSH_RUNTIME}" = "1" ]; then \
      test "${DSH_SESSION_ROOT}" = "/home/node/.dsh/acp-sessions"; \
    fi \
    && if [ "${AGENT_ACP_SMOKE}" = "1" ]; then \
      node /opt/xpressclaw/verify-acp-stdio.mjs "${AGENT_BINARY}"; \
    fi
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
