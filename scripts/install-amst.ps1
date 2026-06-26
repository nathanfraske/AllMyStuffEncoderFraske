#Requires -Version 5.1
# AMSTerm (`amst`) installer for Windows — the AllMyStuff mesh terminal, as its
# own program.
#
#   irm https://allmystuff.works/install-amst.ps1 | iex
#
# `amst` is a small, self-contained client of this machine's AllMyStuff node: it
# opens a real shell on any machine you own, over the mesh. This installs *just*
# `amst` plus its desktop integration — a desktop + Start Menu shortcut and an
# "AMSTerm here" right-click context menu — separate from the main AllMyStuff
# install. It relies on an AllMyStuff node being present to reach machines; if
# none is running, `amst` opens the desktop app to bring one up — or, on a
# headless box, starts a headless node directly and says so.

[CmdletBinding()]
param(
    [switch]$DryRun,
    [switch]$FromSource,
    [switch]$NoDesktop,
    [switch]$Uninstall,
    [string]$Prefix = "$env:LOCALAPPDATA\Programs\AMSTerm",
    [string]$Repo = $(if ($env:ALLMYSTUFF_REPO) { $env:ALLMYSTUFF_REPO } else { "mrjeeves/AllMyStuff" })
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
$asset = "amst-windows-$arch.zip"
$amstExe = Join-Path $Prefix "amst.exe"

# Registry keys for the "AMSTerm here" context menu. Background = right-click an
# empty spot in a folder (%V = that folder); Directory = right-click a folder
# (%1 = the folder). Both live under HKCU, so no elevation is needed.
$ctxBackground = "HKCU:\Software\Classes\Directory\Background\shell\AMSTerm"
$ctxDirectory  = "HKCU:\Software\Classes\Directory\shell\AMSTerm"

$desktopLnk   = Join-Path ([Environment]::GetFolderPath('Desktop')) "AMSTerm.lnk"
$startMenuLnk = Join-Path ([Environment]::GetFolderPath('Programs')) "AMSTerm.lnk"

# ---- uninstall -------------------------------------------------------------

function Remove-Integration {
    foreach ($k in @($ctxBackground, $ctxDirectory)) {
        if (Test-Path $k) {
            if ($DryRun) { Log "(dry-run) would remove $k" }
            else { Remove-Item -Path $k -Recurse -Force; Log "Removed context menu: $k" }
        }
    }
    foreach ($l in @($desktopLnk, $startMenuLnk)) {
        if (Test-Path $l) {
            if ($DryRun) { Log "(dry-run) would remove $l" }
            else { Remove-Item -Path $l -Force; Log "Removed shortcut: $l" }
        }
    }
}

if ($Uninstall) {
    Log "Removing AMSTerm…"
    Remove-Integration
    if (Test-Path $amstExe) {
        if ($DryRun) { Log "(dry-run) would remove $amstExe" }
        else {
            Remove-Item -Path $amstExe -Force
            Log "Removed $amstExe"
            # Drop the install dir if it's now empty.
            if ((Test-Path $Prefix) -and -not (Get-ChildItem -Path $Prefix -Force)) {
                Remove-Item -Path $Prefix -Force
            }
        }
    }
    Log "Done. (The AllMyStuff node, if any, was left untouched.)"
    return
}

# ---- install the binary ----------------------------------------------------

function Install-FromRelease {
    if (-not (Get-Command Invoke-RestMethod -ErrorAction SilentlyContinue)) { return $false }
    $api = "https://api.github.com/repos/$Repo/releases/latest"
    Log "Looking up latest release: $api"
    try { $release = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "amst-installer" } }
    catch { Warn "GitHub releases unreachable (or no release yet)."; return $false }
    $dl = ($release.assets | Where-Object { $_.name -eq $asset } | Select-Object -First 1).browser_download_url
    if (-not $dl) { Warn "No release asset matched $asset."; return $false }
    Log "Downloading $dl"
    if ($DryRun) { Log "(dry-run) would download + install $dl"; return $true }
    $tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "amst-install-$([guid]::NewGuid())")
    try {
        $zip = Join-Path $tmp $asset
        Invoke-WebRequest -Uri $dl -OutFile $zip -Headers @{ "User-Agent" = "amst-installer" }
        try {
            $shaTxt = Invoke-WebRequest -Uri "$dl.sha256" -Headers @{ "User-Agent" = "amst-installer" } -ErrorAction Stop
            $want = ($shaTxt.Content -split '\s+')[0].ToLower()
            $got = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLower()
            if ($want -and $want -ne $got) { Err "SHA256 verification failed for $asset — not installing."; exit 1 }
        } catch { Warn "No SHA256 sidecar; skipping integrity check." }
        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
        Expand-Archive -Path $zip -DestinationPath $Prefix -Force
    } finally { Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue }
    if (-not (Test-Path $amstExe)) { throw "amst.exe not found in $asset after extraction" }
    Log "Installed: $amstExe"
    return $true
}

function Install-FromSource {
    Log "Building amst from source…"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Err "cargo not found. Install Rust via https://rustup.rs first."; exit 1 }
    if ((Test-Path "Cargo.toml") -and (Test-Path "crates\allmystuff-term")) {
        $repoDir = (Get-Location).Path
    } else {
        if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Err "git is required to build from source."; exit 1 }
        $repoDir = Join-Path $env:TEMP "AllMyStuff-$([guid]::NewGuid())"
        Log "Cloning into $repoDir"
        if (-not $DryRun) { git clone --depth 1 "https://github.com/$Repo.git" $repoDir }
    }
    if ($DryRun) { Log "(dry-run) would build amst in $repoDir"; return }
    Push-Location $repoDir
    try {
        cargo build --release --bin amst
        $built = Join-Path $repoDir "target\release\amst.exe"
        if (-not (Test-Path $built)) { Err "Build did not produce $built"; exit 1 }
        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
        Copy-Item -Force $built $amstExe
        Log "Installed: $amstExe"
    } finally { Pop-Location }
}

if ($FromSource) { Install-FromSource }
elseif (-not (Install-FromRelease)) { Warn "Falling back to building from source."; Install-FromSource }

# ---- PATH ------------------------------------------------------------------

if (-not $DryRun) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not ($userPath -split ";" | Where-Object { $_ -ieq $Prefix })) {
        Log "Adding $Prefix to user PATH"
        [Environment]::SetEnvironmentVariable("Path", "$userPath;$Prefix", "User")
        $env:Path = "$env:Path;$Prefix"
    }
}

# ---- shortcuts + context menu ----------------------------------------------

function New-AmstShortcut([string]$path) {
    if ($DryRun) { Log "(dry-run) would create shortcut $path"; return }
    $ws = New-Object -ComObject WScript.Shell
    $lnk = $ws.CreateShortcut($path)
    $lnk.TargetPath = $amstExe
    $lnk.IconLocation = "$amstExe,0"
    $lnk.Description = "AMSTerm — a shell on any machine you own, over the AllMyStuff mesh"
    $lnk.Save()
    Log "Created shortcut: $path"
}

function Set-DefaultValue([string]$key, [string]$value) {
    if (-not (Test-Path $key)) { New-Item -Path $key -Force | Out-Null }
    Set-Item -Path $key -Value $value
}

function Install-ContextMenu([string]$key, [string]$pathToken) {
    if ($DryRun) { Log "(dry-run) would add context menu $key"; return }
    Set-DefaultValue $key "AMSTerm here"
    New-ItemProperty -Path $key -Name "Icon" -Value $amstExe -PropertyType String -Force | Out-Null
    Set-DefaultValue "$key\command" ('"{0}" --cwd "{1}"' -f $amstExe, $pathToken)
    Log "Installed context menu: $key"
}

if (-not $NoDesktop) {
    New-AmstShortcut $desktopLnk
    New-AmstShortcut $startMenuLnk
    # %V is the folder when right-clicking its background; %1 when right-clicking
    # the folder itself.
    Install-ContextMenu $ctxBackground '%V'
    Install-ContextMenu $ctxDirectory  '%1'
}

# ---- node check ------------------------------------------------------------

if (-not (Get-Command allmystuff -ErrorAction SilentlyContinue) -and
    -not (Get-Command allmystuff-serve -ErrorAction SilentlyContinue)) {
    Warn "AllMyStuff isn't installed here. amst opens the desktop app to start a node"
    Warn "(or starts a headless one directly on a box with no app) - either needs AllMyStuff."
    Warn 'Install it:  irm https://allmystuff.works/install.ps1 | iex'
}

Log "Done. Try:  amst            (a shell on this machine)"
Log "            amst --list     (the machines you can reach)"
Log "Right-click any folder (or its background) → 'AMSTerm here' opens a shell there."
Log "Open a new terminal so the updated PATH takes effect."
