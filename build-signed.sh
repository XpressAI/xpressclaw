#!/usr/bin/env bash
set -euo pipefail

# Load non-secret signing config
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

echo "==> Building frontend..."
cd frontend && npm ci && npm run build && cd ..

echo "==> Building CLI sidecar..."
TARGET_TRIPLE=$(rustc --print host-tuple 2>/dev/null || rustc -vV | grep host | cut -d' ' -f2)
cargo build --release -p xpressclaw-cli
mkdir -p crates/xpressclaw-tauri/binaries
cp "target/release/xpressclaw" "crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"

echo "==> Building Tauri app (signed + notarized)..."
npx @tauri-apps/cli build

echo "==> Done! DMG is at:"
find target/release/bundle/dmg -name "*.dmg" 2>/dev/null
