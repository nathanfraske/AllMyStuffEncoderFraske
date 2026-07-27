[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDir,

    [Parameter(Mandatory = $true)]
    [string]$MyOwnMeshPath,

    [string]$TargetDir = 'C:\t\target-ams-sandbox',

    [ValidateRange(1, 64)]
    [int]$Jobs = 16
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-FullPath {
    param([Parameter(Mandatory = $true)][string]$Path)
    return [System.IO.Path]::GetFullPath($Path)
}

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Invoke-Version {
    param([Parameter(Mandatory = $true)][string]$Path)
    $output = & $Path --version 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "$Path --version failed with exit code $LASTEXITCODE"
    }
    return (($output | Out-String).Trim())
}

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Text
    )
    $encoding = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($Path, $Text, $encoding)
}

$sourceRoot = Get-FullPath (Join-Path $PSScriptRoot '..')
$output = Get-FullPath $OutputDir
$target = Get-FullPath $TargetDir
$meshSource = Get-FullPath $MyOwnMeshPath

if (-not (Test-Path -LiteralPath (Join-Path $sourceRoot '.git'))) {
    $gitDir = & git -C $sourceRoot rev-parse --git-dir 2>$null
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace(($gitDir | Out-String))) {
        throw "source root is not a Git worktree: $sourceRoot"
    }
}
if (-not (Test-Path -LiteralPath $meshSource -PathType Leaf)) {
    throw "MyOwnMesh binary does not exist: $meshSource"
}
if (Test-Path -LiteralPath $output) {
    $entries = @(Get-ChildItem -LiteralPath $output -Force)
    if ($entries.Count -ne 0) {
        throw "output directory is not empty: $output"
    }
} else {
    New-Item -ItemType Directory -Path $output | Out-Null
}
New-Item -ItemType Directory -Force -Path $target | Out-Null

$pin = (Get-Content -LiteralPath (Join-Path $sourceRoot '.myownmesh-rev') -Raw).Trim()
if ($pin -notmatch '^v(?<version>[0-9]+\.[0-9]+\.[0-9]+)$') {
    throw "unsupported .myownmesh-rev value: $pin"
}
$expectedMeshVersion = $Matches.version
$meshVersion = Invoke-Version $meshSource
if ($meshVersion -notmatch "(^|\s)$([regex]::Escape($expectedMeshVersion))($|\s)") {
    throw "MyOwnMesh version mismatch: source pins $pin, candidate reports '$meshVersion'"
}

$cargo = Get-Command cargo -ErrorAction Stop
$nodeManifest = Join-Path $sourceRoot 'node\Cargo.toml'
$rootManifest = Join-Path $sourceRoot 'Cargo.toml'

& $cargo.Source build `
    --release `
    --target-dir $target `
    --manifest-path $nodeManifest `
    --bin allmystuff-serve `
    --example video_prod_probe `
    --example p2_remote_transport `
    --example sandbox_node_control `
    --example sandbox_process_launcher `
    --example sandbox_remote_worker `
    --features field-telemetry `
    -j $Jobs
if ($LASTEXITCODE -ne 0) {
    throw "sandbox node and harness build failed with exit code $LASTEXITCODE"
}

& $cargo.Source build `
    --release `
    --target-dir $target `
    --manifest-path $rootManifest `
    -p allmystuff-term `
    --bin amst `
    -j $Jobs
if ($LASTEXITCODE -ne 0) {
    throw "amst build failed with exit code $LASTEXITCODE"
}

$isWindows = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
$suffix = if ($isWindows) { '.exe' } else { '' }
$builtFiles = [ordered]@{
    "allmystuff-serve$suffix" = Join-Path $target "release\allmystuff-serve$suffix"
    "video_prod_probe$suffix" = Join-Path $target "release\examples\video_prod_probe$suffix"
    "p2_remote_transport$suffix" =
        Join-Path $target "release\examples\p2_remote_transport$suffix"
    "sandbox_node_control$suffix" =
        Join-Path $target "release\examples\sandbox_node_control$suffix"
    "sandbox_process_launcher$suffix" =
        Join-Path $target "release\examples\sandbox_process_launcher$suffix"
    "sandbox_remote_worker$suffix" =
        Join-Path $target "release\examples\sandbox_remote_worker$suffix"
    "amst$suffix" = Join-Path $target "release\amst$suffix"
    "myownmesh$suffix" = $meshSource
    'allmystuff-sandbox.ps1' = Join-Path $sourceRoot 'scripts\allmystuff-sandbox.ps1'
    'stage-allmystuff-sandbox.ps1' =
        Join-Path $sourceRoot 'scripts\stage-allmystuff-sandbox.ps1'
    'configure-allmystuff-sandbox-firewall.ps1' =
        Join-Path $sourceRoot 'scripts\configure-allmystuff-sandbox-firewall.ps1'
    'bootstrap-allmystuff-sandbox-remote.ps1' =
        Join-Path $sourceRoot 'scripts\bootstrap-allmystuff-sandbox-remote.ps1'
    'deploy-allmystuff-sandbox-remote.ps1' =
        Join-Path $sourceRoot 'scripts\deploy-allmystuff-sandbox-remote.ps1'
    'test-allmystuff-sandbox-pair.ps1' =
        Join-Path $sourceRoot 'scripts\test-allmystuff-sandbox-pair.ps1'
    'sandbox-fleet-policy.example.json' =
        Join-Path $sourceRoot 'scripts\sandbox-fleet-policy.example.json'
    'summarize_video_profile.py' =
        Join-Path $sourceRoot 'scripts\summarize_video_profile.py'
}

foreach ($entry in $builtFiles.GetEnumerator()) {
    if (-not (Test-Path -LiteralPath $entry.Value -PathType Leaf)) {
        throw "expected bundle input is missing: $($entry.Value)"
    }
    Copy-Item -LiteralPath $entry.Value -Destination (Join-Path $output $entry.Key)
}

$servePath = Join-Path $output "allmystuff-serve$suffix"
$stagedMeshPath = Join-Path $output "myownmesh$suffix"
$sourceCommit = (& git -C $sourceRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) {
    throw 'could not resolve source commit'
}
$sourceStatus = @(& git -C $sourceRoot status --porcelain=v1 --untracked-files=all)
if ($LASTEXITCODE -ne 0) {
    throw 'could not read source status'
}

$files = foreach ($entry in $builtFiles.GetEnumerator()) {
    $path = Join-Path $output $entry.Key
    $item = Get-Item -LiteralPath $path
    [ordered]@{
        name = $entry.Key
        sha256 = Get-Sha256 $path
        size = [int64]$item.Length
    }
}

$manifest = [ordered]@{
    schema = 1
    kind = 'allmystuff-sandbox-bundle'
    created_utc = [DateTime]::UtcNow.ToString('o')
    source = [ordered]@{
        commit = $sourceCommit
        status = $sourceStatus
        allmystuff_version = Invoke-Version $servePath
        myownmesh_pin = $pin
        myownmesh_version = Invoke-Version $stagedMeshPath
    }
    files = @($files)
}

$manifestPath = Join-Path $output 'sandbox-bundle.json'
Write-Utf8NoBom $manifestPath (($manifest | ConvertTo-Json -Depth 8) + [Environment]::NewLine)

[ordered]@{
    bundle = $output
    manifest = $manifestPath
    source_commit = $sourceCommit
    source_dirty = ($sourceStatus.Count -ne 0)
    allmystuff_version = $manifest.source.allmystuff_version
    myownmesh_version = $manifest.source.myownmesh_version
    files = $manifest.files
} | ConvertTo-Json -Depth 8
