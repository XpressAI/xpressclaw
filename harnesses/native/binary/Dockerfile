FROM docker:29.6.1-cli AS docker-cli

FROM node:24-bookworm-slim AS runner-base

ARG TARGETARCH
ARG AGENT_KIND
ARG AGENT_BINARY
ARG AGENT_PATH
ARG AGENT_ARCHIVE_AMD64
ARG AGENT_ARCHIVE_ARM64
ARG AGENT_SHA256_AMD64
ARG AGENT_SHA256_ARM64
ARG RUNNER_VERSION=development
LABEL io.xpressclaw.protocol="acp-xpressclaw-v2" \
    io.xpressclaw.agent="${AGENT_KIND}" \
    io.xpressclaw.runner.version="${RUNNER_VERSION}"

RUN test -n "${AGENT_KIND}" \
    && test -n "${AGENT_BINARY}" \
    && test -n "${AGENT_PATH}" \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl gh git openssh-client ripgrep unzip \
    && rm -rf /var/lib/apt/lists/* \
    && install -d /opt/xpressclaw/libexec /opt/agent \
    && mv /usr/bin/gh /opt/xpressclaw/libexec/gh \
    && case "${TARGETARCH}" in \
         amd64) archive="${AGENT_ARCHIVE_AMD64}"; checksum="${AGENT_SHA256_AMD64}" ;; \
         arm64) archive="${AGENT_ARCHIVE_ARM64}"; checksum="${AGENT_SHA256_ARM64}" ;; \
         *) echo "unsupported architecture: ${TARGETARCH}" >&2; exit 2 ;; \
       esac \
    && test -n "${archive}" \
    && curl --fail --location --retry 3 --output /tmp/agent-archive "${archive}" \
    && if [ -n "${checksum}" ]; then echo "${checksum}  /tmp/agent-archive" | sha256sum --check -; fi \
    && case "${archive}" in \
         *.zip) unzip -q /tmp/agent-archive -d /opt/agent ;; \
         *.tar.gz|*.tgz) tar -xzf /tmp/agent-archive -C /opt/agent ;; \
         *) echo "unsupported agent archive: ${archive}" >&2; exit 2 ;; \
       esac \
    && rm /tmp/agent-archive \
    && test -f "/opt/agent/${AGENT_PATH}" \
    && chmod 0755 "/opt/agent/${AGENT_PATH}" \
    && ln -s "/opt/agent/${AGENT_PATH}" "/usr/local/bin/${AGENT_BINARY}" \
    && command -v "${AGENT_BINARY}"

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
