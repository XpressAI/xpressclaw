FROM ghcr.io/xpressai/xpressclaw-harness-base:latest

WORKDIR /app

# Node.js is required for the bundled Claude Code CLI and MCP servers (npx)
RUN apt-get update && \
    apt-get install -y --no-install-recommends curl ca-certificates && \
    curl -fsSL https://deb.nodesource.com/setup_20.x | bash - && \
    apt-get install -y --no-install-recommends nodejs && \
    apt-get clean && rm -rf /var/lib/apt/lists/*

# Install Claude SDK (bundles the Claude Code CLI)
COPY requirements.txt /tmp/claude-requirements.txt
RUN pip install --no-cache-dir -r /tmp/claude-requirements.txt

COPY harness.py .

# Disable Claude Code telemetry and attribution headers
ENV CLAUDE_CODE_ATTRIBUTION_HEADER=0
ENV CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
ENV DISABLE_TELEMETRY=1

# Default workspace mount point
RUN mkdir -p /workspace

# Create non-root user — Claude CLI refuses --dangerously-skip-permissions as root
RUN useradd -m -s /bin/bash agent && chown -R agent:agent /app /workspace
USER agent

EXPOSE 8080
CMD ["python", "harness.py"]
