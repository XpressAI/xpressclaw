FROM node:22-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends tmux tar \
    && rm -rf /var/lib/apt/lists/*

USER node
WORKDIR /tmp
ENTRYPOINT []
CMD ["node", "-e", "setInterval(() => {}, 1000)"]
