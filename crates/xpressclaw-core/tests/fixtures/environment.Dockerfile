FROM node:22-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends tmux tar openssh-client openssh-server python3 iptables \
    && rm -rf /var/lib/apt/lists/*

# Ephemeral keys used only by the local network-policy integration test.
RUN mkdir -p /run/sshd \
    && ssh-keygen -A \
    && ssh-keygen -q -t ed25519 -N '' -f /test-key \
    && cp /test-key.pub /etc/ssh/test-authorized-keys \
    && chmod 644 /test-key \
    && passwd -d root

USER node
WORKDIR /tmp
ENTRYPOINT []
CMD ["node", "-e", "setInterval(() => {}, 1000)"]
