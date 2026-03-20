#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Build the frontend
echo "==> Building frontend..."
cd frontend
npm ci
npm run build
cd ..

# Build the Rust binary (touch frontend.rs to force re-embedding the frontend build)
echo "==> Building Rust binary..."
touch crates/xpressclaw-server/src/frontend.rs
cargo build --release

echo "==> Build complete: target/release/xpressclaw"

# Copy CLI binary as Tauri sidecar
echo "==> Copying CLI binary as Tauri sidecar..."
TARGET_TRIPLE=$(rustc --print host-tuple 2>/dev/null || rustc -vV | grep host | cut -d' ' -f2)
mkdir -p crates/xpressclaw-tauri/binaries
# Check both native and cross-compile paths
if [ -f "target/release/xpressclaw" ]; then
    cp "target/release/xpressclaw" "crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"
elif [ -f "target/${TARGET_TRIPLE}/release/xpressclaw" ]; then
    cp "target/${TARGET_TRIPLE}/release/xpressclaw" "crates/xpressclaw-tauri/binaries/xpressclaw-${TARGET_TRIPLE}"
fi
echo "    Copied to binaries/xpressclaw-${TARGET_TRIPLE}"

# Build the desktop app via npx (no global tauri-cli install needed)
echo "==> Building Tauri desktop app..."
npx -y @tauri-apps/cli build

# Build harness Docker images locally (CI handles pushing to GHCR)
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

# Run tests
echo "==> Running tests..."
cargo test --workspace

echo "==> Running frontend type check..."
cd frontend
npm run check
cd ..

echo "==> All done!"
