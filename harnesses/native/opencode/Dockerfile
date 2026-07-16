FROM docker:29.6.1-cli AS docker-cli

FROM node:22-bookworm-slim AS runner-base

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

# Opt-in image for trusted projects that need the host Docker-compatible
# engine. The default final stage below remains the smaller runner image.
FROM runner-base AS runner-host
USER root
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=docker-cli /usr/local/libexec/docker/cli-plugins /usr/local/libexec/docker/cli-plugins
LABEL io.xpressclaw.container-engine="host"
RUN docker --version \
    && docker compose version \
    && docker buildx version
USER node

FROM runner-base AS runner
