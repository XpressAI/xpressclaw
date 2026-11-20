# build.ps1 — Build xpressclaw on Windows
$ErrorActionPreference = "Stop"

# Flags
$SkipTest = $false
$SkipTauri = $false
$BuildRunners = $false
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
        "^--skip-docker$" { $BuildRunners = $false }
        "^--with-runners$" { $BuildRunners = $true }
        "^--skip-check$"  { $SkipCheck = $true }
        "^--target=(.+)$" { $TargetOverride = $Matches[1] }
    }
}

if ($TargetOverride) {
    $triple = $TargetOverride
    $CargoTargetArgs = @("--target", $triple)
    $CliOutputDir = "target\$triple\release"
} else {
    $triple = (rustc --print host-tuple).Trim()
    $CargoTargetArgs = @()
    $CliOutputDir = "target\release"
}

# Build CLI with Cargo (build.rs auto-builds frontend if needed)
Write-Host "==> Building CLI..."
cargo build --release -p xpressclaw-cli @CargoTargetArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Copy CLI as Tauri sidecar
Write-Host "==> Copying CLI binary as Tauri sidecar..."
$binDir = "crates\xpressclaw-tauri\binaries"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
Copy-Item "$CliOutputDir\xpressclaw.exe" "$binDir\xpressclaw-$triple.exe"
Write-Host "    Copied to $binDir\xpressclaw-$triple.exe"

if (-not $SkipTest) {
    Write-Host "==> Running tests..."
    # Linux-container MCP and runner tests run in build.sh and Linux CI.
    cargo test -p xpressclaw-core -p xpressclaw-server
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

if (-not $SkipTauri) {
    Write-Host "==> Building Tauri desktop app..."
    if (-not (Test-Path "frontend\node_modules\.bin\tauri.cmd")) {
        Write-Host "==> Installing pinned frontend build tools..."
        Push-Location frontend
        npm ci
        if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
        Pop-Location
    }
    & "frontend\node_modules\.bin\tauri.cmd" build --target $triple
}

$ContainerRuntime = $null
if ($BuildRunners) {
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
    $Runners = @(node scripts/runner-versions.mjs list)
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    foreach ($runner in $Runners) {
        $Dockerfile = (node scripts/runner-versions.mjs dockerfile $runner).Trim()
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        $ResolvedBuildArgs = @(node scripts/runner-versions.mjs build-args $runner)
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        $RunnerBuildArgs = @()
        foreach ($BuildArg in $ResolvedBuildArgs) {
            $RunnerBuildArgs += "--build-arg", $BuildArg
        }
        & $ContainerRuntime @BuildArgs --file "harnesses/native/$Dockerfile/Dockerfile" --target runner @RunnerBuildArgs `
            --tag "xpressclaw-runner-${runner}:latest" `
            --tag "localhost/xpressclaw-runner-${runner}:latest" harnesses/native
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        & $ContainerRuntime @BuildArgs --file "harnesses/native/$Dockerfile/Dockerfile" --target runner-host @RunnerBuildArgs `
            --tag "xpressclaw-runner-${runner}-docker:latest" `
            --tag "localhost/xpressclaw-runner-${runner}-docker:latest" harnesses/native
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
} elseif (-not $BuildRunners) {
    Write-Host "==> Skipping runner builds (use --with-runners)"
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
