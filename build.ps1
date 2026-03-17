# build.ps1 — Build xpressclaw on Windows
$ErrorActionPreference = "Stop"

Write-Host "==> Building frontend..."
Push-Location frontend
npm ci
npm run build
Pop-Location

Write-Host "==> Building CLI sidecar binary..."
cargo build --release -p xpressclaw-cli
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "==> Build complete: target\release\xpressclaw.exe"

# Copy CLI binary as Tauri sidecar
Write-Host "==> Copying CLI binary as Tauri sidecar..."
$triple = (rustc --print host-tuple).Trim()
$binDir = "crates\xpressclaw-tauri\binaries"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
# Check both native and cross-compile paths
if (Test-Path "target\release\xpressclaw.exe") {
    Copy-Item "target\release\xpressclaw.exe" "$binDir\xpressclaw-$triple.exe"
} elseif (Test-Path "target\$triple\release\xpressclaw.exe") {
    Copy-Item "target\$triple\release\xpressclaw.exe" "$binDir\xpressclaw-$triple.exe"
} else {
    Write-Error "CLI binary not found in target\release\ or target\$triple\release\"
    exit 1
}
Write-Host "    Copied to $binDir\xpressclaw-$triple.exe"

# Build Tauri desktop app if tauri-cli is installed
if (Get-Command cargo-tauri -ErrorAction SilentlyContinue) {
    Write-Host "==> Building Tauri desktop app..."
    cargo tauri build
} else {
    Write-Host "==> Skipping Tauri build (install with: cargo install tauri-cli)"
}

Write-Host "==> Building remaining workspace crates..."
cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Run tests
Write-Host "==> Running tests..."
cargo test --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "==> Running frontend type check..."
Push-Location frontend
npm run check
Pop-Location

Write-Host "==> All done!"
