#!/usr/bin/env bash
set -euo pipefail

wait_seconds="${1:-0}"
image_tag="${2:-latest}"
deadline=$((SECONDS + wait_seconds))
registry="${XPRESSCLAW_RUNNER_REGISTRY:-ghcr.io/xpressai}"
agent_list=$(node scripts/runner-versions.mjs list)
agents=()
while IFS= read -r agent; do
  agents+=("$agent")
done <<<"$agent_list"
runners=()
for agent in "${agents[@]}"; do
  runners+=("xpressclaw-runner-${agent}" "xpressclaw-runner-${agent}-docker")
done

while true; do
  missing=()
  for runner in "${runners[@]}"; do
    if ! docker buildx imagetools inspect "${registry}/${runner}:${image_tag}" >/dev/null 2>&1; then
      missing+=("${registry}/${runner}:${image_tag}")
    fi
  done

  if ((${#missing[@]} == 0)); then
    echo "All XpressClaw runner images tagged ${image_tag} are anonymously accessible."
    exit 0
  fi

  if ((SECONDS >= deadline)); then
    echo "The following runner images are not anonymously accessible:" >&2
    printf '  - %s\n' "${missing[@]}" >&2
    echo >&2
    echo "GHCR creates new container packages as private. An XpressAI organization owner must make each package public once under Packages -> Package settings -> Change visibility." >&2
    exit 1
  fi

  echo "Waiting for ${#missing[@]} runner image(s) to become available..."
  sleep 10
done
