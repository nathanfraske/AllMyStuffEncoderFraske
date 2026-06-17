# AllMyStuff end-user installer (Windows).
#
# Tries (in order):
#   1. Download a pre-built release binary from GitHub for the current platform.
#   2. Fall back to building from source via cargo.
#
# Installs both the `allmystuff` CLI and the `allmystuff-gui` desktop
# app (the app is small and makes a bare `allmystuff` open it — pass
# -NoGui for a CLI-only install), then makes sure the `myownmesh`
# daemon the app's live mode runs on is in place:
#
#   * an installed daemon that's new enough (>= the version pinned in
#     .myownmesh-rev) is used as-is;
#   * an older one is asked to update itself (`myownmesh update`);
#   * none at all -> the latest MyOwnMesh release is installed next to
#     the app (same download + SHA-256 verification as the app itself).
#
# Pass -NoMesh to leave the daemon entirely alone. Mesh trouble never
# fails the install — the app always opens (demo graph) without it.
#
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/mrjeeves/AllMyStuff/main/scripts/install.ps1 | iex
#   iex "& { $(irm https://raw.githubusercontent.com/mrjeeves/AllMyStuff/main/scripts/install.ps1) } -NoGui"
#   .\scripts\install.ps1 -DryRun

[CmdletBinding()]
param(
    [switch]$DryRun,
    [switch]$FromSource,
    [switch]$NoGui,
    [switch]$NoMesh,
    [string]$Prefix = "$env:LOCALAPPDATA\Programs\AllMyStuff",
    [string]$Repo = $(if ($env:ALLMYSTUFF_REPO) { $env:ALLMYSTUFF_REPO } else { "mrjeeves/AllMyStuff" }),
    [string]$MeshRepo = $(if ($env:MYOWNMESH_REPO) { $env:MYOWNMESH_REPO } else { "mrjeeves/MyOwnMesh" })
)

$ErrorActionPreference = "Stop"

function Log($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Host "!!! $msg" -ForegroundColor Yellow }
function Err($msg)  { Write-Host "xxx $msg" -ForegroundColor Red }

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { "x86_64" }
    "ARM64" { "aarch64" }
    default { $env:PROCESSOR_ARCHITECTURE.ToLower() }
}
$asset = "allmystuff-windows-$arch.zip"
$guiAsset = "allmystuff-gui-windows-$arch.zip"
$serveAsset = "allmystuff-serve-windows-$arch.zip"
$meshAsset = "myownmesh-windows-$arch.zip"

function Install-FromZip([string]$zipPath) {
    if (-not (Test-Path $Prefix)) {
        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    }
    Expand-Archive -Path $zipPath -DestinationPath $Prefix -Force
    $exe = Join-Path $Prefix "allmystuff.exe"
    if (-not (Test-Path $exe)) {
        throw "allmystuff.exe not found in $zipPath after extraction"
    }
    Log "Installed: $exe"

    # Add prefix to user PATH if it isn't already there.
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not ($userPath -split ";" | Where-Object { $_ -ieq $Prefix })) {
        Log "Adding $Prefix to user PATH"
        [Environment]::SetEnvironmentVariable("Path", "$userPath;$Prefix", "User")
        $env:Path = "$env:Path;$Prefix"
    }
}

function Install-GuiFromZip([string]$zipPath) {
    if (-not (Test-Path $Prefix)) {
        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    }
    Expand-Archive -Path $zipPath -DestinationPath $Prefix -Force
    $exe = Join-Path $Prefix "allmystuff-gui.exe"
    if (-not (Test-Path $exe)) {
        throw "allmystuff-gui.exe not found in $zipPath after extraction"
    }
    Log "Installed: $exe"
}

function Install-ServeFromZip([string]$zipPath) {
    if (-not (Test-Path $Prefix)) {
        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    }
    Expand-Archive -Path $zipPath -DestinationPath $Prefix -Force
    $exe = Join-Path $Prefix "allmystuff-serve.exe"
    if (-not (Test-Path $exe)) {
        throw "allmystuff-serve.exe not found in $zipPath after extraction"
    }
    Log "Installed: $exe"
}

function Try-Release {
    $api = "https://api.github.com/repos/$Repo/releases/latest"
    Log "Looking up latest release: $api"
    try {
        $release = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "allmystuff-installer" }
    } catch {
        Warn "GitHub releases unreachable (or no release yet): $($_.Exception.Message)"
        return $false
    }
    $match = $release.assets | Where-Object { $_.name -eq $asset } | Select-Object -First 1
    if (-not $match) {
        Warn "No release asset matched $asset."
        return $false
    }
    $url = $match.browser_download_url
    Log "Downloading $url"
    if ($DryRun) { Log "(dry-run) would download $url"; return $true }

    $tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "allmystuff-install-$([guid]::NewGuid())")
    try {
        $zip = Join-Path $tmp $asset
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
        # A missing sidecar downgrades to a warning; a present-but-wrong
        # checksum must never install.
        $shaFile = "$zip.sha256"
        $haveSha = $true
        try {
            Invoke-WebRequest -Uri "$url.sha256" -OutFile $shaFile -UseBasicParsing
        } catch {
            Warn "No SHA256 sidecar; skipping integrity check."
            $haveSha = $false
        }
        if ($haveSha) {
            $expected = (Get-Content $shaFile -Raw).Split()[0].Trim().ToLower()
            $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
            if ($expected -ne $actual) {
                Err "SHA256 mismatch for ${asset}: expected $expected, got $actual — not installing it."
                exit 1
            }
            Log "SHA256 OK"
        }
        Install-FromZip $zip
        return $true
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

# Best-effort GUI install: fetch the portable `allmystuff-gui` zip and
# drop it next to the CLI. Returns $false (without aborting the
# overall install) if the asset is missing, unreachable, or the
# download fails — the CLI is the half that must succeed.
function Try-ReleaseGui {
    $api = "https://api.github.com/repos/$Repo/releases/latest"
    try {
        $release = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "allmystuff-installer" }
    } catch {
        Warn "GitHub releases unreachable; skipping GUI."
        return $false
    }
    $match = $release.assets | Where-Object { $_.name -eq $guiAsset } | Select-Object -First 1
    if (-not $match) {
        Warn "No GUI asset matched $guiAsset in the latest release."
        return $false
    }
    $url = $match.browser_download_url
    Log "Downloading $url"
    if ($DryRun) { Log "(dry-run) would download $url"; return $true }

    $tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "allmystuff-gui-install-$([guid]::NewGuid())")
    try {
        $zip = Join-Path $tmp $guiAsset
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
        # Missing sidecar -> warn and continue; wrong checksum -> don't install.
        $shaFile = "$zip.sha256"
        $haveSha = $true
        try {
            Invoke-WebRequest -Uri "$url.sha256" -OutFile $shaFile -UseBasicParsing
        } catch {
            Warn "No SHA256 sidecar for GUI; skipping integrity check."
            $haveSha = $false
        }
        if ($haveSha) {
            $expected = (Get-Content $shaFile -Raw).Split()[0].Trim().ToLower()
            $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
            if ($expected -ne $actual) {
                Warn "SHA256 mismatch for $guiAsset — not installing the GUI."
                return $false
            }
            Log "SHA256 OK"
        }
        Install-GuiFromZip $zip
        return $true
    } catch {
        Warn "GUI download/install failed: $($_.Exception.Message)"
        return $false
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

# Best-effort node install: fetch the portable `allmystuff-serve` zip and
# drop it next to the CLI. This is the headless node `allmystuff serve` runs;
# without it that command prints a hint pointing here. (Windows has no
# `allmystuff service` backend yet — register `allmystuff serve` as a Task
# Scheduler task instead — but the binary itself runs fine.)
function Try-ReleaseServe {
    $api = "https://api.github.com/repos/$Repo/releases/latest"
    try {
        $release = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "allmystuff-installer" }
    } catch {
        Warn "GitHub releases unreachable; skipping the node binary."
        return $false
    }
    $match = $release.assets | Where-Object { $_.name -eq $serveAsset } | Select-Object -First 1
    if (-not $match) {
        Warn "No node asset matched $serveAsset in the latest release."
        return $false
    }
    $url = $match.browser_download_url
    Log "Downloading $url"
    if ($DryRun) { Log "(dry-run) would download $url"; return $true }

    $tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "allmystuff-serve-install-$([guid]::NewGuid())")
    try {
        $zip = Join-Path $tmp $serveAsset
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
        $shaFile = "$zip.sha256"
        $haveSha = $true
        try {
            Invoke-WebRequest -Uri "$url.sha256" -OutFile $shaFile -UseBasicParsing
        } catch {
            Warn "No SHA256 sidecar for the node binary; skipping integrity check."
            $haveSha = $false
        }
        if ($haveSha) {
            $expected = (Get-Content $shaFile -Raw).Split()[0].Trim().ToLower()
            $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
            if ($expected -ne $actual) {
                Warn "SHA256 mismatch for $serveAsset — not installing the node binary."
                return $false
            }
            Log "SHA256 OK"
        }
        Install-ServeFromZip $zip
        return $true
    } catch {
        Warn "node download/install failed: $($_.Exception.Message)"
        return $false
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

# ---------------------------------------------------------------------------
# The mesh daemon. The desktop app's live mode runs on `myownmesh`
# (demo mode needs nothing), so the installer makes sure a usable daemon
# is in place — without ever failing the app install over it:
#
#   1. installed and new enough (>= the .myownmesh-rev pin) -> used as-is;
#   2. installed but older -> asked to update itself (`myownmesh update`);
#   3. missing -> the latest MyOwnMesh release is installed next to the
#      app, where the app finds it without any PATH refresh.

# The minimum daemon version this app wants: the rev pinned in
# .myownmesh-rev, read from the checkout when running from one, fetched
# from the repo otherwise. $null when the pin is unreachable or isn't a
# version tag (a sha pin can't be compared) — any installed daemon
# passes then.
function Get-MeshMinVersion {
    $rev = $null
    if ($PSScriptRoot) {
        $local = Join-Path (Split-Path $PSScriptRoot -Parent) ".myownmesh-rev"
        if (Test-Path $local) { $rev = (Get-Content $local -Raw).Trim() }
    }
    if (-not $rev) {
        try {
            $rev = (Invoke-RestMethod -Uri "https://raw.githubusercontent.com/$Repo/main/.myownmesh-rev" -Headers @{ "User-Agent" = "allmystuff-installer" }).Trim()
        } catch {
            return $null
        }
    }
    if ($rev -match '^v(\d+\.\d+(\.\d+)?)') { return [version]$Matches[1] }
    return $null
}

# `myownmesh --version` -> [version]0.2.4 ($null when it won't answer).
function Get-MeshVersion([string]$exe) {
    try {
        $out = (& $exe --version 2>$null | Select-Object -First 1)
        if ($LASTEXITCODE -ne 0 -or -not $out) { return $null }
        return [version](("$out".Trim() -split '\s+')[-1].TrimStart('v'))
    } catch {
        return $null
    }
}

# Fetch the daemon zip from MyOwnMesh's latest release (SHA-256 verified,
# like the app's own assets) and install it next to the app.
function Try-ReleaseMesh {
    $api = "https://api.github.com/repos/$MeshRepo/releases/latest"
    try {
        $release = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "allmystuff-installer" }
    } catch {
        Warn "MyOwnMesh releases unreachable: $($_.Exception.Message)"
        return $false
    }
    $match = $release.assets | Where-Object { $_.name -eq $meshAsset } | Select-Object -First 1
    if (-not $match) {
        Warn "No release asset matched $meshAsset in MyOwnMesh's latest release."
        return $false
    }
    $url = $match.browser_download_url
    Log "Downloading $url"

    $tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "myownmesh-install-$([guid]::NewGuid())")
    try {
        $zip = Join-Path $tmp $meshAsset
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
        # Missing sidecar -> warn and continue; wrong checksum -> don't install.
        $shaFile = "$zip.sha256"
        $haveSha = $true
        try {
            Invoke-WebRequest -Uri "$url.sha256" -OutFile $shaFile -UseBasicParsing
        } catch {
            Warn "No SHA256 sidecar for the daemon; skipping integrity check."
            $haveSha = $false
        }
        if ($haveSha) {
            $expected = (Get-Content $shaFile -Raw).Split()[0].Trim().ToLower()
            $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
            if ($expected -ne $actual) {
                Warn "SHA256 mismatch for $meshAsset — not installing the daemon."
                return $false
            }
            Log "SHA256 OK"
        }
        Expand-Archive -Path $zip -DestinationPath $Prefix -Force
        $exe = Join-Path $Prefix "myownmesh.exe"
        if (-not (Test-Path $exe)) {
            Warn "myownmesh.exe not found in $meshAsset after extraction."
            return $false
        }
        Log "Installed: $exe"
        return $true
    } catch {
        Warn "Daemon download/install failed: $($_.Exception.Message)"
        return $false
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

function Ensure-Mesh {
    # Prefer a daemon sitting next to the app (where we'd install one —
    # the app checks there first too), then PATH.
    $existing = $null
    $local = Join-Path $Prefix "myownmesh.exe"
    if (Test-Path $local) {
        $existing = $local
    } else {
        $cmd = Get-Command myownmesh -ErrorAction SilentlyContinue
        if ($cmd) { $existing = $cmd.Source }
    }
    $min = Get-MeshMinVersion

    if ($existing) {
        $ver = Get-MeshVersion $existing
        if ($ver -and (-not $min -or $ver -ge $min)) {
            if ($min) { Log "Mesh: using the installed myownmesh v$ver at $existing (needs v$min+)." }
            else      { Log "Mesh: using the installed myownmesh v$ver at $existing." }
            return
        }
        if ($ver) { Log "Mesh: installed myownmesh is v$ver but this release wants v$min+." }
        else      { Log "Mesh: $existing didn't answer --version." }
        if ($DryRun) {
            Log "(dry-run) would ask it to update itself: myownmesh update"
            return
        }
        Log "Asking it to update itself (myownmesh update)…"
        try { & $existing update } catch { Warn "myownmesh update failed: $($_.Exception.Message)" }
        $ver = Get-MeshVersion $existing
        if ($ver -and (-not $min -or $ver -ge $min)) {
            Log "Mesh: myownmesh is now v$ver."
        } else {
            Warn "Mesh: couldn't bring myownmesh up to v$min (see above). The app still runs —"
            Warn "an older daemon just lacks the newer mesh features. Retry later with: myownmesh update"
        }
        return
    }

    if ($DryRun) {
        Log "(dry-run) would install the myownmesh daemon ($meshAsset) next to the app"
        return
    }
    Log "Mesh: no myownmesh daemon found — installing it next to the app…"
    if (Try-ReleaseMesh) {
        $ver = Get-MeshVersion (Join-Path $Prefix "myownmesh.exe")
        if ($ver) { Log "Mesh: installed myownmesh v$ver — the app starts it automatically." }
        else      { Log "Mesh: installed myownmesh — the app starts it automatically." }
    } else {
        Warn "Mesh: couldn't fetch the daemon. The app still opens (demo graph); for"
        Warn "live machines, re-run this installer later or use MyOwnMesh's:"
        Warn ('  iex "& { $(irm https://raw.githubusercontent.com/' + $MeshRepo + '/main/scripts/install.ps1) } -NoGui"')
    }
}

function Build-FromSource {
    Log "Building from source…"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Err "cargo not found. Install Rust via https://rustup.rs first."
        exit 1
    }
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        Err "git is required to build from source."
        exit 1
    }
    if ((Test-Path "Cargo.toml") -and (Test-Path "crates\allmystuff-cli")) {
        $repoDir = (Get-Location).Path
        Log "Using current directory as source: $repoDir"
    } else {
        $repoDir = Join-Path $env:TEMP "AllMyStuff-$([guid]::NewGuid())"
        Log "Cloning into $repoDir"
        if (-not $DryRun) { git clone --depth 1 "https://github.com/$Repo.git" $repoDir }
    }
    if ($DryRun) { Log "(dry-run) would build in $repoDir"; return }

    Push-Location $repoDir
    try {
        cargo build --release --bin allmystuff
        $built = Join-Path $repoDir "target\release\allmystuff.exe"
        if (-not (Test-Path $built)) {
            Err "Build did not produce $built"
            exit 1
        }
        if (-not (Test-Path $Prefix)) {
            New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
        }
        Copy-Item -Force $built (Join-Path $Prefix "allmystuff.exe")
        Log "Installed: $(Join-Path $Prefix 'allmystuff.exe')"

        $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
        if (-not ($userPath -split ";" | Where-Object { $_ -ieq $Prefix })) {
            [Environment]::SetEnvironmentVariable("Path", "$userPath;$Prefix", "User")
            $env:Path = "$env:Path;$Prefix"
        }
    } finally {
        Pop-Location
    }
}

$installedFromRelease = $false
if ($FromSource -or -not (Try-Release)) {
    Build-FromSource
} else {
    $installedFromRelease = $true
}

# Desktop app (allmystuff-gui). On by default; -NoGui skips it. Only
# attempted on the release path — building the GUI from source needs
# the full Tauri/pnpm toolchain, out of scope for this installer.
$guiInstalled = $false
if (-not $NoGui) {
    if ($installedFromRelease) {
        if (Try-ReleaseGui) {
            $guiInstalled = $true
        } else {
            Warn "GUI binary not installed; a bare 'allmystuff' will print a hint until it is. Re-run the installer later, or build it from gui\."
        }
    } elseif ($DryRun) {
        Log "(dry-run) would install the GUI binary ($guiAsset) next to allmystuff"
    } else {
        Warn "Built the CLI from source; skipping the GUI binary (needs the Tauri/pnpm toolchain)."
        Warn "Build it with:  cd gui; pnpm install; pnpm tauri build"
    }
}

# The headless node binary (allmystuff-serve) — what `allmystuff serve` runs.
# Installed on every release install; a from-source CLI build skips it (it
# links the media toolchain).
$serveInstalled = $false
if ($installedFromRelease) {
    if (Try-ReleaseServe) {
        $serveInstalled = $true
    } else {
        Warn "Node binary not installed; 'allmystuff serve' will print a hint until it is."
    }
} elseif ($DryRun) {
    Log "(dry-run) would install the node binary ($serveAsset) next to allmystuff"
} else {
    Warn "Built the CLI from source; skipping the node binary (needs the media toolchain)."
    Warn "Build it with:  cargo build --release --manifest-path node\Cargo.toml"
}

# Mesh daemon — see the block above Ensure-Mesh for the rules. Both the
# desktop app *and* the headless node (`allmystuff serve`) run on it, so it's
# installed whenever either is; a from-source build skips it.
if ($NoMesh) {
    Log "Skipping the mesh daemon (-NoMesh)."
} elseif ($guiInstalled -or $serveInstalled) {
    Ensure-Mesh
} elseif ($DryRun) {
    Ensure-Mesh
} else {
    Log "Mesh: skipped — neither the desktop app nor the node binary was"
    Log "installed (only they use the daemon; scan/capabilities don't)."
}

if (-not $NoGui) {
    Log "Done. Try: allmystuff (opens the app) | allmystuff scan | allmystuff capabilities"
    Log "The app opens into a demo graph even with no mesh. Live machines run on the"
    Log "'myownmesh' daemon (handled above), which the app starts and manages"
    Log "automatically."
    if ($NoMesh) {
        Log "You skipped it (-NoMesh) — when you want live mode:"
        Log ('  iex "& { $(irm https://raw.githubusercontent.com/' + $MeshRepo + '/main/scripts/install.ps1) } -NoGui"')
    }
} else {
    Log "Done. Try: allmystuff scan | allmystuff capabilities | allmystuff update"
    if ($serveInstalled -or $DryRun) {
        Log "Headless node: 'allmystuff serve' runs this machine on the mesh (no GUI)."
        Log "On Windows, register it as a startup task (Task Scheduler) to keep it running."
    }
}
Log "Open a new terminal so the updated PATH takes effect."
