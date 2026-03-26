#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Handle --clean flag
if [ "${1:-}" = "--clean" ]; then
    echo "==> Cleaning..."
    bazel clean --expunge 2>/dev/null || true
    cargo clean 2>/dev/null || true
    rm -rf frontend/build frontend/.svelte-kit frontend/node_modules
    rm -rf crates/xpressclaw-tauri/binaries
    echo "    Done."
    echo ""
fi

# Build with Bazel (CLI, core, server + frontend)
echo "==> Building with Bazel..."
bazel build //crates/xpressclaw-cli:xpressclaw //crates/xpressclaw-core:xpressclaw-core //crates/xpressclaw-server:xpressclaw-server

echo "==> Running tests..."
bazel test //crates/xpressclaw-core:core_test //crates/xpressclaw-server:server_test

# Copy Bazel-built CLI as Tauri sidecar
echo "==> Copying CLI binary as Tauri sidecar..."
TARGET_TRIPLE=$(rustc --print host-tuple 2>/dev/null || rustc -vV | grep host | cut -d' ' -f2)
mkdir -p crates/xpressclaw-tauri/binaries
cp "bazel-bin/crates/xpressclaw-cli/xpressclaw" "crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"
echo "    Copied to binaries/xpressclaw-${TARGET_TRIPLE}"

# Build the desktop app via Tauri CLI
echo "==> Building Tauri desktop app..."
npx -y @tauri-apps/cli build --target "${TARGET_TRIPLE}"

# Build harness Docker images locally
if command -v docker &>/dev/null; then
    echo "==> Building agent harness Docker images..."
    docker build -t ghcr.io/xpressai/xpressclaw-harness-base:latest harnesses/base
    docker build -t ghcr.io/xpressai/xpressclaw-harness-generic:latest harnesses/generic
    docker build -t ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest harnesses/claude-sdk
    docker build -t ghcr.io/xpressai/xpressclaw-harness-langchain:latest harnesses/langchain
    docker build -t ghcr.io/xpressai/xpressclaw-harness-xaibo:latest harnesses/xaibo
else
    echo "==> Skipping harness builds (Docker not found)"
fi

echo "==> Running frontend type check..."
cd frontend
npm run check
cd ..

echo "==> All done!"
