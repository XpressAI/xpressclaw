#!/usr/bin/env bash
set -euo pipefail

runner_list=$(node scripts/runner-versions.mjs list)
all_runners=()
while IFS= read -r runner; do
  all_runners+=("$runner")
done <<<"$runner_list"

if [[ $# -gt 0 ]]; then
  runners=("$@")
  for runner in "${runners[@]}"; do
    supported=false
    for known_runner in "${all_runners[@]}"; do
      if [[ "$runner" == "$known_runner" ]]; then
        supported=true
        break
      fi
    done
    if [[ "$supported" == false ]]; then
      echo "Unknown runner: $runner" >&2
      echo "Supported runners: ${all_runners[*]}" >&2
      exit 2
    fi
  done
else
  runners=("${all_runners[@]}")
fi

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

for runner in "${runners[@]}"; do
  dockerfile=$(node scripts/runner-versions.mjs dockerfile "$runner")
  build_args=()
  resolved_build_args_output=$(node scripts/runner-versions.mjs build-args "$runner")
  resolved_build_args=()
  while IFS= read -r build_arg; do
    resolved_build_args+=("$build_arg")
  done <<<"$resolved_build_args_output"
  for build_arg in "${resolved_build_args[@]}"; do
    build_args+=(--build-arg "$build_arg")
  done
  "${build_command[@]}" \
    --file "harnesses/native/${dockerfile}/Dockerfile" \
    --target runner \
    "${build_args[@]}" \
    --tag "xpressclaw-runner-${runner}:latest" \
    --tag "localhost/xpressclaw-runner-${runner}:latest" \
    harnesses/native
  "${build_command[@]}" \
    --file "harnesses/native/${dockerfile}/Dockerfile" \
    --target runner-host \
    "${build_args[@]}" \
    --tag "xpressclaw-runner-${runner}-docker:latest" \
    --tag "localhost/xpressclaw-runner-${runner}-docker:latest" \
    harnesses/native
done
