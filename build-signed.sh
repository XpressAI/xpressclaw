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

# Load signing config
if [ -f .env.signing ]; then
    source .env.signing
fi

# Prompt for app-specific password if not set
if [ -z "${APPLE_PASSWORD:-}" ]; then
    echo -n "Apple app-specific password: "
    read -s APPLE_PASSWORD
    echo
    export APPLE_PASSWORD
fi

TARGET_TRIPLE=$(rustc --print host-tuple 2>/dev/null || rustc -vV | grep host | cut -d' ' -f2)
echo "==> Target: ${TARGET_TRIPLE}"

# 1. Build with Bazel (CLI + frontend + server)
echo "==> Building with Bazel..."
bazel build //crates/xpressclaw-cli:xpressclaw

# 2. Copy Bazel-built CLI as Tauri sidecar
echo "==> Copying CLI sidecar..."
mkdir -p crates/xpressclaw-tauri/binaries
cp "bazel-bin/crates/xpressclaw-cli/xpressclaw" "crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"
echo "    Sidecar: crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"

# 3. Build Tauri desktop app with signing + notarization
echo "==> Building Tauri app (signed + notarized)..."
echo "    Signing identity: ${APPLE_SIGNING_IDENTITY:-not set}"
echo "    Team ID: ${APPLE_TEAM_ID:-not set}"
npx @tauri-apps/cli build --target "${TARGET_TRIPLE}"

# 4. Show output
echo ""
echo "==> Done!"
echo ""
DMG=$(find "target/${TARGET_TRIPLE}/release/bundle/dmg" -name "*.dmg" 2>/dev/null || find target/release/bundle/dmg -name "*.dmg" 2>/dev/null || echo "")
if [ -n "$DMG" ]; then
    echo "DMG: ${DMG}"
    ls -lh $DMG
else
    echo "No DMG found. Check target/*/release/bundle/ for output."
fi
