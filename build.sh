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

# Build with Bazel (CLI, core, server + frontend)
echo "==> Building with Bazel..."
bazel build //crates/xpressclaw-cli:xpressclaw //crates/xpressclaw-core:xpressclaw-core //crates/xpressclaw-server:xpressclaw-server

# Verify the binary has frontend assets embedded
echo "==> Verifying frontend is embedded in CLI binary..."
EXEC_ROOT=$(bazel info execution_root 2>/dev/null)
echo "    Exec root: $EXEC_ROOT"
echo "    frontend/ type: $(ls -ld "$EXEC_ROOT/frontend" 2>&1)"
echo "    crates/ type: $(ls -ld "$EXEC_ROOT/crates" 2>&1)"
echo "    frontend/build/index.html: $(ls -la "$EXEC_ROOT/frontend/build/index.html" 2>&1)"
echo "    Binary size: $(ls -la bazel-bin/crates/xpressclaw-cli/xpressclaw 2>&1)"
# Check binary content directly (no need to run it)
if strings bazel-bin/crates/xpressclaw-cli/xpressclaw | grep -qi "sveltekit\|_app/immutable\|doctype"; then
    echo "    ✓ Frontend is embedded in binary"
else
    echo "    ✗ Frontend NOT embedded in binary!"
    echo "    Binary string 'index.html' count: $(strings bazel-bin/crates/xpressclaw-cli/xpressclaw | grep -c 'index.html')"
    echo "    Exec root frontend/ listing:"
    ls -la "$EXEC_ROOT/frontend/" 2>&1
    echo "    frontend/build/ listing:"
    ls -la "$EXEC_ROOT/frontend/build/" 2>&1 | head -10
fi

if [ "$SKIP_TEST" = false ]; then
    echo "==> Running tests..."
    bazel test //crates/xpressclaw-core:core_test //crates/xpressclaw-server:server_test
fi

# Copy Bazel-built CLI as Tauri sidecar
echo "==> Copying CLI binary as Tauri sidecar..."
if [ -n "$TARGET_OVERRIDE" ]; then
    TARGET_TRIPLE="$TARGET_OVERRIDE"
else
    TARGET_TRIPLE=$(rustc --print host-tuple 2>/dev/null || rustc -vV | grep host | cut -d' ' -f2)
fi
mkdir -p crates/xpressclaw-tauri/binaries
cp "bazel-bin/crates/xpressclaw-cli/xpressclaw" "crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"
echo "    Copied to binaries/xpressclaw-${TARGET_TRIPLE}"

if [ "$SKIP_TAURI" = false ]; then
    # Build the desktop app via Tauri CLI
    echo "==> Building Tauri desktop app..."
    npx -y @tauri-apps/cli build --target "${TARGET_TRIPLE}"
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
