// Legacy convenience targets for a small native-runner subset.
// Product builds use scripts/build-runner-images.sh and the exact versions in
// harnesses/runner-versions.json.
// Usage: docker buildx bake -f harnesses/docker-bake.hcl

variable "REGISTRY" {
  default = "ghcr.io/xpressai"
}

variable "TAG" {
  default = "latest"
}

group "default" {
  targets = ["native-codex", "native-claude", "native-opencode", "native-codex-docker", "native-claude-docker", "native-opencode-docker"]
}

// Retained only for developers maintaining the pre-ACP compatibility images.
// Product builds and releases do not build or publish this group.
group "legacy" {
  targets = ["base", "generic", "claude-sdk", "xaibo", "langchain"]
}

target "native-codex" {
  context    = "./native"
  dockerfile = "codex/Dockerfile"
  target     = "runner"
  tags       = ["xpressclaw-runner-codex:${TAG}", "localhost/xpressclaw-runner-codex:${TAG}", "${REGISTRY}/xpressclaw-runner-codex:${TAG}"]
}

target "native-claude" {
  context    = "./native"
  dockerfile = "claude/Dockerfile"
  target     = "runner"
  tags       = ["xpressclaw-runner-claude:${TAG}", "localhost/xpressclaw-runner-claude:${TAG}", "${REGISTRY}/xpressclaw-runner-claude:${TAG}"]
}

target "native-opencode" {
  context    = "./native"
  dockerfile = "opencode/Dockerfile"
  target     = "runner"
  tags       = ["xpressclaw-runner-opencode:${TAG}", "localhost/xpressclaw-runner-opencode:${TAG}", "${REGISTRY}/xpressclaw-runner-opencode:${TAG}"]
}

target "native-codex-docker" {
  context    = "./native"
  dockerfile = "codex/Dockerfile"
  target     = "runner-host"
  tags       = ["xpressclaw-runner-codex-docker:${TAG}", "localhost/xpressclaw-runner-codex-docker:${TAG}", "${REGISTRY}/xpressclaw-runner-codex-docker:${TAG}"]
}

target "native-claude-docker" {
  context    = "./native"
  dockerfile = "claude/Dockerfile"
  target     = "runner-host"
  tags       = ["xpressclaw-runner-claude-docker:${TAG}", "localhost/xpressclaw-runner-claude-docker:${TAG}", "${REGISTRY}/xpressclaw-runner-claude-docker:${TAG}"]
}

target "native-opencode-docker" {
  context    = "./native"
  dockerfile = "opencode/Dockerfile"
  target     = "runner-host"
  tags       = ["xpressclaw-runner-opencode-docker:${TAG}", "localhost/xpressclaw-runner-opencode-docker:${TAG}", "${REGISTRY}/xpressclaw-runner-opencode-docker:${TAG}"]
}

target "base" {
  context    = "./base"
  dockerfile = "Dockerfile"
  tags       = ["${REGISTRY}/xpressclaw-harness-base:${TAG}"]
}

target "generic" {
  context    = "./generic"
  dockerfile = "Dockerfile"
  tags       = ["${REGISTRY}/xpressclaw-harness-generic:${TAG}"]
  contexts = {
    "ghcr.io/xpressai/xpressclaw-harness-base:latest" = "target:base"
  }
}

target "claude-sdk" {
  context    = "./claude-sdk"
  dockerfile = "Dockerfile"
  tags       = ["${REGISTRY}/xpressclaw-harness-claude-sdk:${TAG}"]
  contexts = {
    "ghcr.io/xpressai/xpressclaw-harness-base:latest" = "target:base"
  }
}

target "xaibo" {
  context    = "./xaibo"
  dockerfile = "Dockerfile"
  tags       = ["${REGISTRY}/xpressclaw-harness-xaibo:${TAG}"]
  contexts = {
    "ghcr.io/xpressai/xpressclaw-harness-base:latest" = "target:base"
  }
}

target "langchain" {
  context    = "./langchain"
  dockerfile = "Dockerfile"
  tags       = ["${REGISTRY}/xpressclaw-harness-langchain:${TAG}"]
  contexts = {
    "ghcr.io/xpressai/xpressclaw-harness-base:latest" = "target:base"
  }
}
