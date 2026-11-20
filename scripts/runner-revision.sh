#!/usr/bin/env bash
set -euo pipefail

# Identify the commit that last changed the inputs used to build runner images.
# Both the image and release workflows use this value, including when a single
# push contains multiple commits and the runner change is not the push tip.
runner_revision=$(git log --first-parent -m -1 --format=%H -- \
  harnesses \
  scripts/build-runner-images.sh \
  scripts/runner-revision.sh \
  scripts/runner-versions.mjs \
  scripts/runner-versions.test.mjs \
  scripts/verify-runner-images.sh \
  .github/workflows/harnesses.yml)

if [[ ! "$runner_revision" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Could not resolve the runner-image revision." >&2
  exit 1
fi

printf '%s\n' "$runner_revision"
