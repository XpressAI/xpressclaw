# build.ps1 — Build xpressclaw on Windows
$ErrorActionPreference = "Stop"

Write-Host "==> Building frontend..."
Push-Location frontend
npm ci
npm run build
Pop-Location

Write-Host "==> Building Rust workspace..."
cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "==> Build complete: target\release\xpressclaw.exe"

# Copy CLI binary as Tauri sidecar
Write-Host "==> Copying CLI binary as Tauri sidecar..."
$triple = (rustc --print host-tuple).Trim()
$binDir = "crates\xpressclaw-tauri\binaries"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
Copy-Item "target\release\xpressclaw.exe" "$binDir\xpressclaw-$triple.exe"
Write-Host "    Copied to $binDir\xpressclaw-$triple.exe"

# Build Tauri desktop app if tauri-cli is installed
if (Get-Command cargo-tauri -ErrorAction SilentlyContinue) {
    Write-Host "==> Building Tauri desktop app..."
    cargo tauri build
} else {
    Write-Host "==> Skipping Tauri build (install with: cargo install tauri-cli)"
}

# Run tests
Write-Host "==> Running tests..."
cargo test --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "==> Running frontend type check..."
Push-Location frontend
npm run check
Pop-Location

Write-Host "==> All done!"
