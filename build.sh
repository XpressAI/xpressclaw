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

# Build CLI (release mode — disables debug_assertions so rust-embed
# embeds statically. The server's build.rs auto-builds the frontend
# if frontend/build/ doesn't exist.)
echo "==> Building CLI..."
cargo build --release -p xpressclaw-cli

# Copy CLI as Tauri sidecar
echo "==> Copying CLI binary as Tauri sidecar..."
if [ -n "$TARGET_OVERRIDE" ]; then
    TARGET_TRIPLE="$TARGET_OVERRIDE"
else
    TARGET_TRIPLE=$(rustc --print host-tuple 2>/dev/null || rustc -vV | grep host | cut -d' ' -f2)
fi
mkdir -p crates/xpressclaw-tauri/binaries
cp "target/release/xpressclaw" "crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"
echo "    Copied to binaries/xpressclaw-${TARGET_TRIPLE}"

if [ "$SKIP_TEST" = false ]; then
    echo "==> Running tests..."
    cargo test -p xpressclaw-core -p xpressclaw-server
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
