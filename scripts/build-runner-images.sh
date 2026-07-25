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

runners=(
  codex claude github-copilot junie kimi opencode pi qwen
  cline cursor glm grok kilo mistral-vibe
)

for runner in "${runners[@]}"; do
  dockerfile="$runner"
  build_args=()
  case "$runner" in
    github-copilot)
      dockerfile=npm
      build_args=(--build-arg AGENT_KIND=github-copilot --build-arg AGENT_PACKAGE=@github/copilot@1.0.71 --build-arg AGENT_BINARY=copilot)
      ;;
    cline)
      dockerfile=npm
      build_args=(--build-arg AGENT_KIND=cline --build-arg AGENT_PACKAGE=cline@3.0.38 --build-arg AGENT_BINARY=cline)
      ;;
    glm)
      dockerfile=npm
      build_args=(--build-arg AGENT_KIND=glm --build-arg AGENT_PACKAGE=glm-acp-agent@1.1.4 --build-arg AGENT_BINARY=glm-acp-agent)
      ;;
    grok)
      dockerfile=npm
      build_args=(--build-arg AGENT_KIND=grok --build-arg AGENT_PACKAGE=@xai-official/grok@0.2.97 --build-arg AGENT_BINARY=grok)
      ;;
    kilo)
      dockerfile=npm
      build_args=(--build-arg AGENT_KIND=kilo --build-arg AGENT_PACKAGE=@kilocode/cli@7.3.54 --build-arg AGENT_BINARY=kilo)
      ;;
    pi)
      dockerfile=npm
      build_args=(--build-arg AGENT_KIND=pi --build-arg AGENT_PACKAGE=pi-acp@0.0.31 --build-arg AGENT_BINARY=pi-acp --build-arg AGENT_EXTRA_PACKAGES=@earendil-works/pi-coding-agent@0.82.0 --build-arg AGENT_EXTRA_BINARY=pi)
      ;;
    qwen)
      dockerfile=npm
      build_args=(--build-arg AGENT_KIND=qwen --build-arg AGENT_PACKAGE=@qwen-code/qwen-code@0.19.10 --build-arg AGENT_BINARY=qwen)
      ;;
    cursor)
      dockerfile=binary
      build_args=(
        --build-arg AGENT_KIND=cursor
        --build-arg AGENT_BINARY=cursor-agent
        --build-arg AGENT_PATH=dist-package/cursor-agent
        --build-arg AGENT_ARCHIVE_AMD64=https://downloads.cursor.com/lab/2026.07.20-8cc9c0b/linux/x64/agent-cli-package.tar.gz
        --build-arg AGENT_ARCHIVE_ARM64=https://downloads.cursor.com/lab/2026.07.20-8cc9c0b/linux/arm64/agent-cli-package.tar.gz
      )
      ;;
    junie)
      dockerfile=binary
      build_args=(
        --build-arg AGENT_KIND=junie
        --build-arg AGENT_BINARY=junie
        --build-arg AGENT_PATH=junie-app/bin/junie
        --build-arg AGENT_ARCHIVE_AMD64=https://github.com/JetBrains/junie/releases/download/1966.57/junie-release-1966.57-linux-amd64.zip
        --build-arg AGENT_ARCHIVE_ARM64=https://github.com/JetBrains/junie/releases/download/1966.57/junie-release-1966.57-linux-aarch64.zip
      )
      ;;
    kimi)
      dockerfile=binary
      build_args=(
        --build-arg AGENT_KIND=kimi
        --build-arg AGENT_BINARY=kimi
        --build-arg AGENT_PATH=kimi
        --build-arg AGENT_ARCHIVE_AMD64=https://github.com/MoonshotAI/kimi-cli/releases/download/1.49.0/kimi-1.49.0-x86_64-unknown-linux-gnu.tar.gz
        --build-arg AGENT_ARCHIVE_ARM64=https://github.com/MoonshotAI/kimi-cli/releases/download/1.49.0/kimi-1.49.0-aarch64-unknown-linux-gnu.tar.gz
        --build-arg AGENT_SHA256_AMD64=6ce0b83f583c45a64cc9f51ffe7e1a8e03ee79acda69945fcf8c23341b9d892f
        --build-arg AGENT_SHA256_ARM64=5ac54cabce16ede27b9d2069b9b88edee25528646e7bb5befa9980a1ca71febb
      )
      ;;
    mistral-vibe)
      dockerfile=binary
      build_args=(
        --build-arg AGENT_KIND=mistral-vibe
        --build-arg AGENT_BINARY=vibe-acp
        --build-arg AGENT_PATH=vibe-acp
        --build-arg AGENT_ARCHIVE_AMD64=https://github.com/mistralai/mistral-vibe/releases/download/v2.17.1/vibe-acp-linux-x86_64-2.17.1.zip
        --build-arg AGENT_ARCHIVE_ARM64=https://github.com/mistralai/mistral-vibe/releases/download/v2.17.1/vibe-acp-linux-aarch64-2.17.1.zip
      )
      ;;
  esac
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
