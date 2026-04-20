#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Flags (opt-out pattern — each step runs by default unless skipped).
SKIP_TEST=false
SKIP_TAURI=false
SKIP_DOCKER=false
SKIP_CHECK=false
SKIP_PUSH=false
TARGET_OVERRIDE=""
# Image ref the pi harness WASM gets pushed to. Override with
# XCLAW_PI_IMAGE env var or --pi-image=<ref>.
XCLAW_PI_IMAGE="${XCLAW_PI_IMAGE:-localhost:5000/pi:dev}"

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
        --skip-push)   SKIP_PUSH=true ;;
        --pi-image=*)  XCLAW_PI_IMAGE="${arg#--pi-image=}" ;;
        --target=*)    TARGET_OVERRIDE="${arg#--target=}" ;;
    esac
done

# Detect GPU acceleration
CARGO_FEATURES=""
if command -v nvcc &>/dev/null; then
    CARGO_FEATURES="--features cuda"
    echo "==> CUDA detected ($(nvcc --version | grep -oP 'release \K[0-9.]+'))"

    # Set CUDA_PATH if not already set — cmake and find_cuda_helper need it
    # to locate headers (cuda.h) and libraries.
    if [ -z "${CUDA_PATH:-}" ]; then
        # Check well-known roots first (they contain include/cuda.h)
        for candidate in /usr/local/cuda /opt/cuda /usr/lib/cuda; do
            if [ -f "$candidate/include/cuda.h" ]; then
                export CUDA_PATH="$candidate"
                break
            fi
        done
        # Fallback: derive from nvcc location
        if [ -z "${CUDA_PATH:-}" ]; then
            NVCC_REAL=$(readlink -f "$(which nvcc)")
            export CUDA_PATH=$(dirname "$(dirname "$NVCC_REAL")")
        fi
        echo "    CUDA_PATH=$CUDA_PATH"
    fi

    # find_cuda_helper only searches <root>/lib64 paths, which misses
    # Debian/Ubuntu multiarch layouts (e.g. /usr/lib/x86_64-linux-gnu).
    # Add the actual library dir to RUSTFLAGS so the linker finds cudart_static.
    CUDA_LIB_DIR=""
    for candidate in \
        "${CUDA_PATH}/lib64" \
        "/usr/local/cuda/lib64" \
        "/usr/lib/$(gcc -dumpmachine 2>/dev/null)" \
    ; do
        if [ -f "$candidate/libcudart_static.a" ]; then
            CUDA_LIB_DIR="$candidate"
            break
        fi
    done
    if [ -n "$CUDA_LIB_DIR" ]; then
        export RUSTFLAGS="${RUSTFLAGS:-} -L $CUDA_LIB_DIR"
        echo "    CUDA libs=$CUDA_LIB_DIR"
    fi
elif [[ "$(uname)" == "Darwin" ]]; then
    CARGO_FEATURES="--features metal"
    echo "==> macOS detected, enabling Metal acceleration"
fi

# Build CLI (release mode — disables debug_assertions so rust-embed
# embeds statically. The server's build.rs auto-rebuilds the frontend
# whenever frontend source files change.)
echo "==> Building CLI..."
cargo build --release -p xpressclaw-cli $CARGO_FEATURES

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

if [ "$SKIP_DOCKER" = false ] && command -v docker &>/dev/null; then
    echo "==> Building agent harness Docker images..."
    docker build -t ghcr.io/xpressai/xpressclaw-harness-base:latest harnesses/base
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

if [ "$SKIP_PUSH" = false ]; then
    # ADR-023 task 10 phase 2: push the bundled harness WASM to a local
    # OCI registry so the pi harness can be tested end-to-end against
    # a real OCI pull. The registry ref is $XCLAW_PI_IMAGE (default
    # localhost:5000/pi:dev); override with --pi-image=<ref>.
    #
    # Start the registry first:
    #   podman run -d -p 5000:5000 --name xclaw-registry registry:2
    # Then run with XPRESSCLAW_HARNESS=pi and set each agent's
    # config `image:` field to $XCLAW_PI_IMAGE.
    #
    # Skipped gracefully if prerequisites are missing — matches how the
    # docker step handles absent `docker`. Pass --skip-push to skip
    # even when prerequisites are present.
    REGISTRY_HOST="${XCLAW_PI_IMAGE%%/*}"
    if ! command -v oras &>/dev/null; then
        echo "==> Skipping pi WASM push: oras not on PATH (install from https://oras.land/ or pass --skip-push)."
    elif ! curl -fsS --max-time 3 "http://${REGISTRY_HOST}/v2/" &>/dev/null; then
        echo "==> Skipping pi WASM push: no registry responding at http://${REGISTRY_HOST}/v2/."
        echo "    Start one with: podman run -d -p 5000:5000 --name xclaw-registry registry:2"
    else
        echo "==> Pushing pi harness WASM to ${XCLAW_PI_IMAGE}..."
        WASM_STAGE="$(mktemp -d)"
        trap 'rm -rf "$WASM_STAGE"' EXIT
        WASM_FILE="${WASM_STAGE}/pi.wasm"
        "target/release/xpressclaw" write-bundled-wasm "$WASM_FILE"

        # Push as an OCI artifact. The custom media type keeps the
        # artifact discoverable as "a WASM module intended for
        # xpressclaw" rather than a generic blob, so future harness
        # types can differentiate via media type even if they share a
        # repo.
        (
            cd "$WASM_STAGE"
            oras push --plain-http \
                --artifact-type "application/vnd.xpressclaw.harness.wasm+v1" \
                "$XCLAW_PI_IMAGE" \
                "pi.wasm:application/wasm"
        )
        echo "    Pushed. Test with:"
        echo "      XPRESSCLAW_HARNESS=pi ./target/release/xpressclaw up"
        echo "    (and set agent config \`image: ${XCLAW_PI_IMAGE}\`)"
    fi
fi

echo "==> All done!"
