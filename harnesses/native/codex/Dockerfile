FROM docker:29.6.1-cli AS docker-cli

FROM node:22-bookworm-slim AS runner-base

ARG CODEX_ACP_VERSION=latest
ARG RUNNER_VERSION=development
ARG PPTXGENJS_VERSION=4.0.1
LABEL io.xpressclaw.protocol="acp-xpressclaw-v2" \
    io.xpressclaw.agent="codex" \
    io.xpressclaw.runner.version="${RUNNER_VERSION}" \
    io.xpressclaw.presentations="xpressclaw-pptx-v1" \
    io.xpressclaw.presentations.pptxgenjs="4.0.1"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        fonts-dejavu-core \
        fonts-liberation2 \
        gh \
        git \
        libreoffice-impress \
        openssh-client \
        poppler-utils \
        ripgrep \
        unzip \
    && rm -rf /var/lib/apt/lists/* \
    && install -d /opt/xpressclaw/libexec \
    && mv /usr/bin/gh /opt/xpressclaw/libexec/gh \
    && npm install --global --omit=dev "@agentclientprotocol/codex-acp@${CODEX_ACP_VERSION}" \
    && npm cache clean --force

COPY codex/presentation-runtime/package.json codex/presentation-runtime/package-lock.json /opt/xpressclaw/presentation-runtime/
RUN test "$(node -p 'require("/opt/xpressclaw/presentation-runtime/package.json").dependencies.pptxgenjs')" = "$PPTXGENJS_VERSION" \
    && npm ci --omit=dev --ignore-scripts --prefix /opt/xpressclaw/presentation-runtime \
    && npm cache clean --force
COPY --chmod=0444 codex/presentation-runtime/pptxgenjs.mjs /opt/xpressclaw/presentation-runtime/pptxgenjs.mjs
COPY --chmod=0555 codex/presentation-runtime/bin/ /opt/xpressclaw/presentation-runtime/bin/
COPY codex/presentation-runtime/.agents/ /opt/xpressclaw/presentation-runtime/.agents/
COPY --chmod=0444 codex/presentation-runtime/verify-runtime.mjs /opt/xpressclaw/presentation-runtime/verify-runtime.mjs
COPY --chmod=0444 codex/presentation-runtime/verify-codex-acp.mjs /opt/xpressclaw/presentation-runtime/verify-codex-acp.mjs

COPY --chmod=0755 common/mcp-github.mjs /opt/xpressclaw/mcp-github.mjs
COPY --chmod=0755 common/mcp-xpressclaw.mjs /opt/xpressclaw/mcp-xpressclaw.mjs
COPY --chmod=0755 common/verify-acp-initialize.mjs /opt/xpressclaw/verify-acp-initialize.mjs
COPY --chmod=0755 common/git-credential-xpressclaw /usr/local/bin/git-credential-xpressclaw

RUN codex-acp --version \
    && /opt/xpressclaw/libexec/gh --version \
    && node --check /opt/xpressclaw/mcp-github.mjs \
    && node --check /opt/xpressclaw/mcp-xpressclaw.mjs \
    && /opt/xpressclaw/presentation-runtime/bin/xpressclaw-presentation-runtime \
    && EXPECTED_CODEX_ACP_VERSION="${CODEX_ACP_VERSION}" node /opt/xpressclaw/presentation-runtime/verify-codex-acp.mjs \
    && node /opt/xpressclaw/presentation-runtime/verify-runtime.mjs \
    && chmod -R a-w /opt/xpressclaw/presentation-runtime

USER node
ARG ACP_SMOKE_COMMAND_B64
ENV HOME=/home/node \
    CI=1 \
    NO_COLOR=1
RUN node /opt/xpressclaw/verify-acp-initialize.mjs "${ACP_SMOKE_COMMAND_B64}"
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
