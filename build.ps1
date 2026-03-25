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

# Build with Bazel
Write-Host "==> Building with Bazel..."
bazel build //crates/xpressclaw-cli:xpressclaw //crates/xpressclaw-core:xpressclaw-core //crates/xpressclaw-server:xpressclaw-server
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "==> Running tests..."
bazel test //crates/xpressclaw-core:core_test //crates/xpressclaw-server:server_test
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

# Run frontend type check
Write-Host "==> Running frontend type check..."
Push-Location frontend
npm run check
Pop-Location

Write-Host "==> All done!"
