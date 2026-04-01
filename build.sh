#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Flags
SKIP_TEST=false
SKIP_TAURI=false
SKIP_DOCKER=false
SKIP_CHECK=false
TARGET_OVERRIDE=""

for arg in "$@"; do
    case "$arg" in
        --clean)
            echo "==> Cleaning..."
            bazel clean --expunge 2>/dev/null || true
            cargo clean 2>/dev/null || true
            rm -rf frontend/build frontend/.svelte-kit frontend/node_modules
            rm -rf crates/xpressclaw-tauri/binaries
            echo "    Done."
            echo ""
            ;;
        --skip-test)   SKIP_TEST=true ;;
        --skip-tauri)  SKIP_TAURI=true ;;
        --skip-docker) SKIP_DOCKER=true ;;
        --skip-check)  SKIP_CHECK=true ;;
        --target=*)    TARGET_OVERRIDE="${arg#--target=}" ;;
    esac
done

# Install frontend deps so the Bazel frontend_build rule can skip npm ci
echo "==> Installing frontend dependencies..."
cd frontend && npm ci --silent 2>/dev/null && cd ..

# Build with Bazel
# -c opt disables debug_assertions so rust-embed statically embeds
# frontend files instead of reading them from the filesystem at runtime.
echo "==> Building with Bazel..."
bazel build -c opt //crates/xpressclaw-cli:xpressclaw //crates/xpressclaw-core:xpressclaw-core //crates/xpressclaw-server:xpressclaw-server

# Copy Bazel-built CLI as Tauri sidecar (before tests, which reset bazel-bin symlink)
echo "==> Copying CLI binary as Tauri sidecar..."
if [ -n "$TARGET_OVERRIDE" ]; then
    TARGET_TRIPLE="$TARGET_OVERRIDE"
else
    TARGET_TRIPLE=$(rustc --print host-tuple 2>/dev/null || rustc -vV | grep host | cut -d' ' -f2)
fi
mkdir -p crates/xpressclaw-tauri/binaries
cp "bazel-bin/crates/xpressclaw-cli/xpressclaw" "crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"
echo "    Copied to binaries/xpressclaw-${TARGET_TRIPLE}"

if [ "$SKIP_TEST" = false ]; then
    echo "==> Running tests..."
    bazel test //crates/xpressclaw-core:core_test //crates/xpressclaw-server:server_test
fi

if [ "$SKIP_TAURI" = false ]; then
    echo "==> Building Tauri desktop app..."
    TAURI_BUNDLER_DMG_IGNORE_CI=true npx -y @tauri-apps/cli build --target "${TARGET_TRIPLE}"
fi

if [ "$SKIP_DOCKER" = false ] && command -v docker &>/dev/null; then
    echo "==> Building agent harness Docker images..."
    docker build -t ghcr.io/xpressai/xpressclaw-harness-base:latest harnesses/base
    docker build -t ghcr.io/xpressai/xpressclaw-harness-generic:latest harnesses/generic
    docker build -t ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest harnesses/claude-sdk
    docker build -t ghcr.io/xpressai/xpressclaw-harness-langchain:latest harnesses/langchain
    docker build -t ghcr.io/xpressai/xpressclaw-harness-xaibo:latest harnesses/xaibo
else
    echo "==> Skipping harness builds"
fi

if [ "$SKIP_CHECK" = false ]; then
    echo "==> Running frontend type check..."
    cd frontend
    npm run check
    cd ..
fi

echo "==> All done!"
