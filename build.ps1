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
            bazel clean --expunge 2>$null
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

# Ensure Bazel can find bash for genrules
if (-not $env:BAZEL_SH) {
    $gitBash = "C:\Program Files\Git\bin\bash.exe"
    if (Test-Path $gitBash) {
        $env:BAZEL_SH = $gitBash
    }
}

# Use short output base to mitigate Windows path length issues.
# cargo-bazel's canonicalize() produces \\?\ paths that break cargo/MSVC.
$outputBase = "C:\b"
New-Item -ItemType Directory -Force -Path $outputBase | Out-Null

# Build with Bazel
# -c opt: disables debug_assertions so rust-embed statically embeds
# frontend files instead of reading them from the filesystem at runtime.
Write-Host "==> Building with Bazel..."
$env:CARGO_BAZEL_TIMEOUT = "1800"
bazel --output_base=$outputBase build -c opt //crates/xpressclaw-cli:xpressclaw //crates/xpressclaw-core:xpressclaw-core //crates/xpressclaw-server:xpressclaw-server
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Copy Bazel-built CLI as Tauri sidecar (before tests, which reset bazel-bin)
Write-Host "==> Copying CLI binary as Tauri sidecar..."
if ($TargetOverride) {
    $triple = $TargetOverride
} else {
    $triple = (rustc --print host-tuple).Trim()
}
$binDir = "crates\xpressclaw-tauri\binaries"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
Copy-Item "bazel-bin\crates\xpressclaw-cli\xpressclaw.exe" "$binDir\xpressclaw-$triple.exe"
Write-Host "    Copied to $binDir\xpressclaw-$triple.exe"

if (-not $SkipTest) {
    Write-Host "==> Running tests..."
    bazel --output_base=$outputBase test //crates/xpressclaw-core:core_test //crates/xpressclaw-server:server_test
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