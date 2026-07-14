FROM node:22-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gh git openssh-client ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global @openai/codex @anthropic-ai/claude-code opencode-ai@latest

USER node
ENV HOME=/home/node \
    CI=1 \
    NO_COLOR=1
WORKDIR /workspace

# The control plane supplies the selected native CLI and prompt per attempt.
ENTRYPOINT []
