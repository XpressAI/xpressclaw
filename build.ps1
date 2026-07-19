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

$ContainerRuntime = $null
if (-not $SkipDocker) {
    foreach ($candidate in @("docker", "podman")) {
        if (Get-Command $candidate -ErrorAction SilentlyContinue) {
            & $candidate info *> $null
            if ($LASTEXITCODE -eq 0) {
                $ContainerRuntime = $candidate
                break
            }
        }
    }
}

if ($ContainerRuntime) {
    $BuildArgs = @("build")
    if ($ContainerRuntime -eq "docker") {
        & docker buildx version *> $null
        if ($LASTEXITCODE -eq 0) {
            $BuildArgs = @("buildx", "build", "--load")
        }
    }
    Write-Host "==> Building native ACP runner images with $ContainerRuntime $($BuildArgs -join ' ')..."
    foreach ($runner in @("codex", "claude", "opencode")) {
        & $ContainerRuntime @BuildArgs --file "harnesses/native/$runner/Dockerfile" --target runner `
            --tag "xpressclaw-runner-${runner}:latest" `
            --tag "localhost/xpressclaw-runner-${runner}:latest" harnesses/native
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        & $ContainerRuntime @BuildArgs --file "harnesses/native/$runner/Dockerfile" --target runner-host `
            --tag "xpressclaw-runner-${runner}-docker:latest" `
            --tag "localhost/xpressclaw-runner-${runner}-docker:latest" harnesses/native
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
} elseif ($SkipDocker) {
    Write-Host "==> Skipping runner builds (--skip-docker)"
} else {
    Write-Host "==> Skipping runner builds (no usable Docker or Podman runtime found)"
}

if (-not $SkipCheck) {
    Write-Host "==> Running frontend type check..."
    Push-Location frontend
    npm run check
    Pop-Location
}

Write-Host "==> All done!"
