# build-cargo.ps1 — Build xpressclaw on Windows using Cargo (no Bazel)
# Workaround for Bazel path length issues with llama.cpp on Windows.
$ErrorActionPreference = "Stop"

$SkipTauri = $false
$SkipDocker = $false
$TargetOverride = ""

foreach ($arg in $args) {
    switch -Regex ($arg) {
        "^--skip-tauri$"  { $SkipTauri = $true }
        "^--skip-docker$" { $SkipDocker = $true }
        "^--target=(.+)$" { $TargetOverride = $Matches[1] }
    }
}

# Build frontend
Write-Host "==> Building frontend..."
Push-Location frontend
npm ci
npm run build
Pop-Location

# Build CLI with Cargo (release mode = debug_assertions off = rust-embed embeds statically)
Write-Host "==> Building CLI with Cargo..."
cargo build --release -p xpressclaw-cli
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Copy as Tauri sidecar
Write-Host "==> Copying CLI binary as Tauri sidecar..."
if ($TargetOverride) {
    $triple = $TargetOverride
} else {
    $triple = (rustc --print host-tuple).Trim()
}
$binDir = "crates\xpressclaw-tauri\binaries"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
Copy-Item "target\release\xpressclaw.exe" "$binDir\xpressclaw-$triple.exe"
Write-Host "    Copied to $binDir\xpressclaw-$triple.exe"

if (-not $SkipTauri) {
    Write-Host "==> Building Tauri desktop app..."
    npx -y @tauri-apps/cli build --target $triple
}

if ((-not $SkipDocker) -and (Get-Command docker -ErrorAction SilentlyContinue)) {
    Write-Host "==> Building agent harness Docker images..."
    docker build -t ghcr.io/xpressai/xpressclaw-harness-base:latest harnesses/base
    docker build -t ghcr.io/xpressai/xpressclaw-harness-generic:latest harnesses/generic
} else {
    Write-Host "==> Skipping harness builds"
}

Write-Host "==> All done!"
