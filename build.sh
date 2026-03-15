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

# Build the Rust binary
echo "==> Building Rust binary..."
cargo build --release

echo "==> Build complete: target/release/xpressclaw"

# Build the desktop app if tauri-cli is installed
if command -v cargo-tauri &>/dev/null; then
    echo "==> Building Tauri desktop app..."
    cargo tauri build
else
    echo "==> Skipping Tauri build (install with: cargo install tauri-cli)"
fi

# Build harness Docker images if docker is available
if command -v docker &>/dev/null; then
    echo "==> Building agent harness Docker images..."
    cd harnesses
    if command -v docker-buildx &>/dev/null || docker buildx version &>/dev/null 2>&1; then
        docker buildx bake
    else
        docker build -t ghcr.io/xpressai/xpressclaw-harness-base:latest ./base
        docker build -t ghcr.io/xpressai/xpressclaw-harness-generic:latest ./generic
        docker build -t ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest ./claude-sdk
        docker build -t ghcr.io/xpressai/xpressclaw-harness-langchain:latest ./langchain
        docker build -t ghcr.io/xpressai/xpressclaw-harness-xaibo:latest ./xaibo
    fi
    cd ..
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
