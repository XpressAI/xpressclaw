#!/bin/bash
# Build the pi-agent container and convert it to a WASM image via container2wasm.
#
# Outputs: wasm-agents/pi-agent.wasm (Bochs-based, ~1.3 GB)
#
# Requirements:
#   - Docker CE with buildx (not podman — c2w needs COPY --link and --platform)
#   - c2w binary on PATH (https://github.com/container2wasm/container2wasm/releases)
#   - User in the docker group
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE_TAG="xpressclaw-pi-agent:latest"
OUT_DIR="${REPO_ROOT}/wasm-agents"
OUT_WASM="${OUT_DIR}/pi-agent.wasm"

unset DOCKER_HOST  # c2w only supports real Docker

echo "[build] Docker image: ${IMAGE_TAG}"
docker build -t "${IMAGE_TAG}" "${REPO_ROOT}/containers/pi-agent/"

echo "[build] Converting to WASM: ${OUT_WASM}"
mkdir -p "${OUT_DIR}"
DOCKER_API_VERSION=1.48 c2w "${IMAGE_TAG}" "${OUT_WASM}"

echo "[build] Done: $(du -h "${OUT_WASM}" | cut -f1) at ${OUT_WASM}"
