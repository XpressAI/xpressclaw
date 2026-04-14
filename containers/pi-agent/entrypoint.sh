#!/bin/bash
set -euo pipefail

# Xpressclaw Pi Agent Entrypoint
#
# Starts mcpfs to mount xpressclaw tools, then runs pi in RPC mode.
# Communication with xpressclaw is via JSONL over stdin/stdout.

# c2w-net NATs the host to 192.168.127.254 for the guest. The xpressclaw
# main server hosts the `/mcp` endpoint on port 8935.
XPRESSCLAW_URL="${XPRESSCLAW_URL:-http://192.168.127.254:8935}"
AGENT_ID="${AGENT_ID:-assistant}"
WORKSPACE="/workspace"

# --- Mount xpressclaw tools via mcpfs ---
# mcpfs exposes MCP server tools as files that pi can read/write.
# The MCP server is an HTTP endpoint on the xpressclaw server.
MCPFS_DIR="${WORKSPACE}/.mcpfs"
mkdir -p "${MCPFS_DIR}"

if command -v mcpfs &>/dev/null; then
    # Mount xpressclaw's MCP server
    mcpfs "${MCPFS_DIR}/xpressclaw" \
        --http "${XPRESSCLAW_URL}/mcp" \
        --background 2>/dev/null || true
    echo "[entrypoint] mcpfs mounted at ${MCPFS_DIR}/xpressclaw" >&2
else
    echo "[entrypoint] mcpfs not available, skipping mount" >&2
fi

# --- Configure pi ---
# Set the model from environment (xpressclaw passes this)
export PI_MODEL="${LLM_MODEL:-local}"
export PI_PROVIDER="${LLM_PROVIDER:-xpressclaw}"

# API keys from environment
# (xpressclaw injects these based on agent config)

# --- Start pi in RPC mode ---
# RPC mode: JSONL over stdin/stdout.
# xpressclaw sends prompts, pi streams events back.
cd "${WORKSPACE}"
exec pi --mode rpc \
    --provider "${PI_PROVIDER}" \
    --model "${PI_MODEL}" \
    --no-session \
    "$@"
