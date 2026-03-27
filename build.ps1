# build.ps1 — Build xpressclaw on Windows
$ErrorActionPreference = "Stop"

# Handle --clean flag
if ($args -contains "--clean") {
    Write-Host "==> Cleaning..."
    bazel clean --expunge 2>$null
    cargo clean 2>$null
    Remove-Item -Recurse -Force frontend\build, frontend\.svelte-kit, frontend\node_modules -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force crates\xpressclaw-tauri\binaries -ErrorAction SilentlyContinue
    Write-Host "    Done."
    Write-Host ""
}

# Ensure Bazel can find bash for genrules
if (-not $env:BAZEL_SH) {
    $gitBash = "C:\Program Files\Git\bin\bash.exe"
    if (Test-Path $gitBash) {
        $env:BAZEL_SH = $gitBash
    }
}

# Use short output base to avoid Windows path length issues with llama.cpp
$outputBase = "C:\b"
New-Item -ItemType Directory -Force -Path $outputBase | Out-Null

# Install frontend dependencies so Bazel genrule skips npm ci
Write-Host "==> Installing frontend dependencies..."
Push-Location frontend
npm ci
Pop-Location

# Pre-warm cargo's git cache — llama-cpp-rs + llama.cpp submodule is large
# and cargo-bazel's default timeout (600s) isn't enough on cold Windows builds
Write-Host "==> Fetching Cargo dependencies..."
cargo fetch

# Build with Bazel
Write-Host "==> Building with Bazel..."
$env:CARGO_BAZEL_GENERATOR_TIMEOUT = "1800"
bazel --output_base=$outputBase build //crates/xpressclaw-cli:xpressclaw //crates/xpressclaw-core:xpressclaw-core //crates/xpressclaw-server:xpressclaw-server
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "==> Running tests..."
bazel --output_base=$outputBase test //crates/xpressclaw-core:core_test //crates/xpressclaw-server:server_test
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Copy Bazel-built CLI as Tauri sidecar
Write-Host "==> Copying CLI binary as Tauri sidecar..."
$triple = (rustc --print host-tuple).Trim()
$binDir = "crates\xpressclaw-tauri\binaries"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
Copy-Item "bazel-bin\crates\xpressclaw-cli\xpressclaw.exe" "$binDir\xpressclaw-$triple.exe"
Write-Host "    Copied to $binDir\xpressclaw-$triple.exe"

# Build Tauri desktop app
Write-Host "==> Building Tauri desktop app..."
npx -y @tauri-apps/cli build --target $triple

# Build harness Docker images locally
if (Get-Command docker -ErrorAction SilentlyContinue) {
    Write-Host "==> Building agent harness Docker images..."
    docker build -t ghcr.io/xpressai/xpressclaw-harness-base:latest harnesses/base
    docker build -t ghcr.io/xpressai/xpressclaw-harness-generic:latest harnesses/generic
    docker build -t ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest harnesses/claude-sdk
    docker build -t ghcr.io/xpressai/xpressclaw-harness-langchain:latest harnesses/langchain
    docker build -t ghcr.io/xpressai/xpressclaw-harness-xaibo:latest harnesses/xaibo
} else {
    Write-Host "==> Skipping harness builds (Docker not found)"
}

# Run frontend type check
Write-Host "==> Running frontend type check..."
Push-Location frontend
npm run check
Pop-Location

Write-Host "==> All done!"
