#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Flags
SKIP_TEST=false
SKIP_TAURI=false
BUILD_RUNNERS=false
SKIP_CHECK=false
TARGET_OVERRIDE=""
RUNNERS=()
RUNNER_COUNT=0

usage() {
    cat <<'EOF'
Usage: ./build.sh [options]

Build the CLI and desktop application for the local platform.

Options:
  --clean              Remove generated build output before building
  --skip-test          Skip native harness and Rust tests
  --skip-tauri         Skip the desktop application bundle
  --skip-check         Skip the frontend type check
  --skip-docker        Do not build runner images (the default)
  --with-runners       Build all local runner images
  --runner=NAME        Build one runner and its runner-host variant; repeatable
  --target=TRIPLE      Build for a specific Rust target triple
  --help               Show this help
EOF
}

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
        --skip-docker) BUILD_RUNNERS=false; RUNNERS=(); RUNNER_COUNT=0 ;;
        --with-runners) BUILD_RUNNERS=true; RUNNERS=(); RUNNER_COUNT=0 ;;
        --runner=*)
            BUILD_RUNNERS=true
            RUNNERS+=("${arg#--runner=}")
            RUNNER_COUNT=$((RUNNER_COUNT + 1))
            ;;
        --skip-check)  SKIP_CHECK=true ;;
        --target=*)    TARGET_OVERRIDE="${arg#--target=}" ;;
        --help|-h)     usage; exit 0 ;;
        *) echo "Unknown option: $arg" >&2; usage >&2; exit 2 ;;
    esac
done

# Resolve the target before building so Cargo and Tauri use the same output.
if [ -n "$TARGET_OVERRIDE" ]; then
    TARGET_TRIPLE="$TARGET_OVERRIDE"
    CLI_OUTPUT_DIR="target/${TARGET_TRIPLE}/release"
else
    TARGET_TRIPLE=$(rustc --print host-tuple 2>/dev/null || rustc -vV | grep host | cut -d' ' -f2)
    CLI_OUTPUT_DIR="target/release"
fi

# Build CLI (release mode — disables debug_assertions so rust-embed
# embeds statically. The server's build.rs auto-rebuilds the frontend
# whenever frontend source files change.)
echo "==> Building CLI..."
if [ -n "$TARGET_OVERRIDE" ]; then
    cargo build --release -p xpressclaw-cli --target "$TARGET_TRIPLE"
else
    cargo build --release -p xpressclaw-cli
fi

# Copy CLI as Tauri sidecar
echo "==> Copying CLI binary as Tauri sidecar..."
mkdir -p crates/xpressclaw-tauri/binaries
cp "${CLI_OUTPUT_DIR}/xpressclaw" "crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"
echo "    Copied to binaries/xpressclaw-${TARGET_TRIPLE}"

if [ "$SKIP_TEST" = false ]; then
    echo "==> Running tests..."
    node --test harnesses/native/common/*.test.mjs scripts/runner-versions.test.mjs
    cargo test -p xpressclaw-core -p xpressclaw-server
fi

if [ "$SKIP_TAURI" = false ]; then
    echo "==> Building Tauri desktop app..."
    if [ ! -x frontend/node_modules/.bin/tauri ]; then
        echo "==> Installing pinned frontend build tools..."
        (cd frontend && npm ci)
    fi
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
    TAURI_BUNDLER_DMG_IGNORE_CI=true frontend/node_modules/.bin/tauri build --target "${TARGET_TRIPLE}" $BUNDLE_FLAG
fi

if [ "$BUILD_RUNNERS" = false ]; then
    echo "==> Skipping runner builds (use --with-runners or --runner=NAME)"
elif command -v docker &>/dev/null || command -v podman &>/dev/null; then
    echo "==> Building native ACP runner images..."
    if [ "$RUNNER_COUNT" -gt 0 ]; then
        bash scripts/build-runner-images.sh "${RUNNERS[@]}"
    else
        bash scripts/build-runner-images.sh
    fi
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
