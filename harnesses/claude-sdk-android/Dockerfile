# Android-enabled variant of the claude-sdk harness.
#
# Adds adb (Android Debug Bridge) and scrcpy v4.0 so that agents running
# `scrcpy-mcp` inside this container can drive Android devices/emulators
# reached via the host adb daemon. Opt-in: not pulled or built unless the
# user runs `xpressclaw android install`.

FROM ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest

# Need root for apt + extracting to /opt.
USER root
WORKDIR /tmp

# adb from the distro is fine — the protocol is stable across versions.
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        android-tools-adb \
        curl \
        ca-certificates && \
    apt-get clean && rm -rf /var/lib/apt/lists/*

# scrcpy v4.0 portable tarball. Cannot use distro scrcpy: Ubuntu/Debian
# ships 1.25 (years stale) and scrcpy-mcp pushes the bundled v4.x
# scrcpy-server JAR, producing a "server version 1.25 does not match
# client 4.0" failure. See android-poc-gotchas.md §8a.
ARG SCRCPY_VERSION=4.0
RUN curl -fsSL -o scrcpy.tar.gz \
        "https://github.com/Genymobile/scrcpy/releases/download/v${SCRCPY_VERSION}/scrcpy-linux-x86_64-v${SCRCPY_VERSION}.tar.gz" && \
    mkdir -p /opt/scrcpy && \
    tar xzf scrcpy.tar.gz -C /opt/scrcpy --strip-components=1 && \
    rm scrcpy.tar.gz && \
    ln -s /opt/scrcpy/scrcpy /usr/local/bin/scrcpy && \
    chmod +x /opt/scrcpy/scrcpy

# scrcpy-mcp resolves its server JAR via this env var. Setting it
# image-wide means the user/template doesn't have to.
ENV SCRCPY_SERVER_PATH=/opt/scrcpy/scrcpy-server

# Return to the non-root agent user + workdir from the parent image so
# the parent's CMD ["python", "harness.py"] still applies unchanged.
USER agent
WORKDIR /app
