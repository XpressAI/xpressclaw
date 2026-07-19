#!/usr/bin/env bash
set -euo pipefail

runtime="${CONTAINER_RUNTIME:-}"
if [[ -z "$runtime" ]]; then
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    runtime=docker
  elif command -v podman >/dev/null 2>&1 && podman info >/dev/null 2>&1; then
    runtime=podman
  else
    echo "No usable Docker or Podman runtime was found." >&2
    exit 1
  fi
fi

case "$runtime" in
  docker|podman) ;;
  *) echo "CONTAINER_RUNTIME must be docker or podman." >&2; exit 2 ;;
esac

build_command=("$runtime" build)
if [[ "$runtime" == docker ]] && docker buildx version >/dev/null 2>&1; then
  # A container-backed Buildx builder keeps successful builds only in its
  # cache unless an output is selected. Loading makes the tags immediately
  # available to the daemon (including a Docker client pointed at Podman).
  build_command=(docker buildx build --load)
fi

echo "Building runner images with ${build_command[*]}"

for runner in codex claude opencode; do
  "${build_command[@]}" \
    --file "harnesses/native/${runner}/Dockerfile" \
    --target runner \
    --tag "xpressclaw-runner-${runner}:latest" \
    --tag "localhost/xpressclaw-runner-${runner}:latest" \
    harnesses/native
  "${build_command[@]}" \
    --file "harnesses/native/${runner}/Dockerfile" \
    --target runner-host \
    --tag "xpressclaw-runner-${runner}-docker:latest" \
    --tag "localhost/xpressclaw-runner-${runner}-docker:latest" \
    harnesses/native
done
