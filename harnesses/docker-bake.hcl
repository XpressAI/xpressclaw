// Docker Bake file for building all harness images.
// Usage: docker buildx bake -f harnesses/docker-bake.hcl

variable "REGISTRY" {
  default = "ghcr.io/xpressai"
}

variable "TAG" {
  default = "latest"
}

group "default" {
  targets = ["native-codex", "native-claude", "native-opencode", "base", "generic", "claude-sdk", "xaibo", "langchain"]
}

target "native-codex" {
  context    = "./native/codex"
  dockerfile = "Dockerfile"
  tags       = ["xpressclaw-runner-codex:${TAG}", "${REGISTRY}/xpressclaw-runner-codex:${TAG}"]
}

target "native-claude" {
  context    = "./native/claude"
  dockerfile = "Dockerfile"
  tags       = ["xpressclaw-runner-claude:${TAG}", "${REGISTRY}/xpressclaw-runner-claude:${TAG}"]
}

target "native-opencode" {
  context    = "./native/opencode"
  dockerfile = "Dockerfile"
  tags       = ["xpressclaw-runner-opencode:${TAG}", "${REGISTRY}/xpressclaw-runner-opencode:${TAG}"]
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
