# build.ps1 — Build xpressclaw on Windows
$ErrorActionPreference = "Stop"

# Flags
$SkipTest = $false
$SkipTauri = $false
$SkipDocker = $false
$SkipCheck = $false
$TargetOverride = ""

foreach ($arg in $args) {
    switch -Regex ($arg) {
        "^--clean$" {
            Write-Host "==> Cleaning..."
            cargo clean 2>$null
            Remove-Item -Recurse -Force frontend\build, frontend\.svelte-kit, frontend\node_modules -ErrorAction SilentlyContinue
            Remove-Item -Recurse -Force crates\xpressclaw-tauri\binaries -ErrorAction SilentlyContinue
            Write-Host "    Done.`n"
        }
        "^--skip-test$"   { $SkipTest = $true }
        "^--skip-tauri$"  { $SkipTauri = $true }
        "^--skip-docker$" { $SkipDocker = $true }
        "^--skip-check$"  { $SkipCheck = $true }
        "^--target=(.+)$" { $TargetOverride = $Matches[1] }
    }
}

# Build CLI with Cargo (build.rs auto-builds frontend if needed)
Write-Host "==> Building CLI..."
cargo build --release -p xpressclaw-cli
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Copy CLI as Tauri sidecar
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

if (-not $SkipTest) {
    Write-Host "==> Running tests..."
    cargo test -p xpressclaw-core -p xpressclaw-server
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

if (-not $SkipTauri) {
    Write-Host "==> Building Tauri desktop app..."
    npx -y @tauri-apps/cli build --target $triple
}

if ((-not $SkipDocker) -and (Get-Command docker -ErrorAction SilentlyContinue)) {
    Write-Host "==> Building agent harness Docker images..."
    docker build -t ghcr.io/xpressai/xpressclaw-harness-base:latest harnesses/base
    docker build -t ghcr.io/xpressai/xpressclaw-harness-generic:latest harnesses/generic
    docker build -t ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest harnesses/claude-sdk
    docker build -t ghcr.io/xpressai/xpressclaw-harness-langchain:latest harnesses/langchain
    docker build -t ghcr.io/xpressai/xpressclaw-harness-xaibo:latest harnesses/xaibo
} else {
    Write-Host "==> Skipping harness builds"
}

if (-not $SkipCheck) {
    Write-Host "==> Running frontend type check..."
    Push-Location frontend
    npm run check
    Pop-Location
}

Write-Host "==> All done!"
