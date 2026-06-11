# AllMyStuff dev bootstrap (Windows). Idempotent: re-running is a no-op.
# Run from PowerShell: `powershell -ExecutionPolicy Bypass -File scripts/bootstrap.ps1`
#
# Installs Rust (+ the MSVC linker), Node, pnpm, WebView2, and `just`, then
# the GUI dependencies — everything `just dev` needs. The mesh daemon is
# not a prerequisite: the GUI's build.rs fetches and bundles it
# automatically on the first `just dev`.

$ErrorActionPreference = "Stop"

function Have($cmd) { $null -ne (Get-Command $cmd -ErrorAction SilentlyContinue) }
function Log($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Host "!!! $msg" -ForegroundColor Yellow }

if (-not (Have "winget")) {
    Warn "winget not found. Install 'App Installer' from the Microsoft Store and re-run."
    exit 1
}

if (-not (Have "rustup")) {
    Log "Installing rustup..."
    winget install --id Rustlang.Rustup --silent --accept-source-agreements --accept-package-agreements
    $env:Path = "$env:Path;$env:USERPROFILE\.cargo\bin"
}

# Rust on Windows links via MSVC's link.exe from Visual Studio Build Tools.
# rustup-init normally prompts to install it, but --silent suppresses that,
# so a fresh box hits "linker `link.exe` not found" on the first build.
function Have-MsvcLinker {
    if (Have "link.exe") { return $true }
    foreach ($base in @(
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC"
    )) {
        if (Test-Path $base) {
            $found = Get-ChildItem -Path $base -Recurse -Filter "link.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($found) { return $true }
        }
    }
    return $false
}

if (-not (Have-MsvcLinker)) {
    Log "Installing Visual Studio Build Tools (C++ workload, ~5 GB - first run only)..."
    winget install --id Microsoft.VisualStudio.2022.BuildTools --silent `
        --accept-source-agreements --accept-package-agreements `
        --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended"
    if ($LASTEXITCODE -ne 0) {
        Warn "Build Tools install returned exit $LASTEXITCODE. If a build later fails with 'link.exe not found', install"
        Warn "'Desktop development with C++' from https://visualstudio.microsoft.com/downloads/ and re-run this script."
    }
}

Log "Ensuring the pinned Rust toolchain + components..."
rustup show | Out-Null
rustup component add clippy rustfmt | Out-Null

if (-not (Have "node")) {
    Log "Installing Node.js LTS..."
    winget install --id OpenJS.NodeJS.LTS --silent --accept-source-agreements --accept-package-agreements
}

if (-not (Have "pnpm")) {
    # winget updates the persistent PATH but not the running session's.
    $env:Path = [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" + [Environment]::GetEnvironmentVariable("Path", "User")
    if (Have "corepack") {
        Log "Enabling pnpm via corepack..."
        corepack enable
        corepack prepare pnpm@latest --activate
    } elseif (Have "npm") {
        Log "Installing pnpm via npm..."
        npm install -g pnpm
    } else {
        Warn "Neither corepack nor npm is on PATH. Open a NEW terminal (so the post-install PATH refreshes) and re-run this script."
        exit 1
    }
}

# WebView2 is required by Tauri on Windows.
$webView2 = Get-ItemProperty -Path "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" -ErrorAction SilentlyContinue
if (-not $webView2) {
    Log "Installing Microsoft Edge WebView2 Runtime..."
    winget install --id Microsoft.EdgeWebView2Runtime --silent --accept-source-agreements --accept-package-agreements
}

if (-not (Have "just")) {
    Log "Installing just..."
    winget install --id Casey.Just --silent --accept-source-agreements --accept-package-agreements
}

Log "Installing GUI dependencies..."
Push-Location gui
pnpm install --silent
Pop-Location

Log "Done. 'just dev' runs the app (the first build fetches + bundles the mesh daemon automatically)."
