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
# embeds statically. The server's build.rs auto-rebuilds the frontend
# whenever frontend source files change.)
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
    node --test harnesses/native/common/*.test.mjs
    cargo test -p xpressclaw-core -p xpressclaw-server
fi

if [ "$SKIP_TAURI" = false ]; then
    echo "==> Building Tauri desktop app..."
    # Pick platform-appropriate bundle format. Override with TAURI_BUNDLES env var.
    TAURI_BUNDLES="${TAURI_BUNDLES:-}"
    if [ -z "$TAURI_BUNDLES" ]; then
        case "$(uname)" in
            Linux*)  TAURI_BUNDLES="deb" ;;
            Darwin*) TAURI_BUNDLES="dmg" ;;
            *)       TAURI_BUNDLES="nsis" ;;
        esac
    fi
    BUNDLE_FLAG=""
    if [ "$TAURI_BUNDLES" != "all" ]; then
        BUNDLE_FLAG="--bundles $TAURI_BUNDLES"
    fi
    TAURI_BUNDLER_DMG_IGNORE_CI=true npx -y @tauri-apps/cli build --target "${TARGET_TRIPLE}" $BUNDLE_FLAG
fi

if [ "$SKIP_DOCKER" = true ]; then
    echo "==> Skipping runner builds (--skip-docker)"
elif command -v docker &>/dev/null || command -v podman &>/dev/null; then
    echo "==> Building native ACP runner images..."
    bash scripts/build-runner-images.sh
else
    echo "==> Skipping runner builds (no Docker or Podman command found)"
fi

if [ "$SKIP_CHECK" = false ]; then
    echo "==> Running frontend type check..."
    cd frontend
    npm run check
    cd ..
fi

echo "==> All done!"
