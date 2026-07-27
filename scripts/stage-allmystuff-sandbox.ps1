[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$BundleDir,

    [string]$RuntimeDir
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

function Assert-Bundle {
    param([Parameter(Mandatory = $true)][string]$Path)

    $manifestPath = Join-Path $Path 'sandbox-bundle.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "sandbox manifest is missing: $manifestPath"
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.schema -ne 1 -or
        $manifest.kind -cne 'allmystuff-sandbox-bundle') {
        throw "unsupported sandbox manifest: $manifestPath"
    }
    foreach ($file in @($manifest.files)) {
        $name = [string]$file.name
        if ([string]::IsNullOrWhiteSpace($name) -or
            [System.IO.Path]::GetFileName($name) -cne $name) {
            throw "unsafe bundle file name in manifest: '$name'"
        }
        $filePath = Join-Path $Path $name
        if (-not (Test-Path -LiteralPath $filePath -PathType Leaf)) {
            throw "bundle file is missing: $filePath"
        }
        if ((Get-Item -LiteralPath $filePath).Length -ne [int64]$file.size) {
            throw "bundle size mismatch for $name"
        }
        if ((Get-Sha256 $filePath) -cne [string]$file.sha256) {
            throw "bundle hash mismatch for $name"
        }
    }
    return $manifest
}

function Assert-NoRuntimeProcess {
    param([Parameter(Mandatory = $true)][string]$Path)

    foreach ($process in @(
        Get-Process -Name 'allmystuff-serve', 'myownmesh' -ErrorAction SilentlyContinue
    )) {
        $processPath = [string]$process.Path
        if ([string]::IsNullOrWhiteSpace($processPath)) {
            continue
        }
        $parent = Get-FullPath (Split-Path -Parent $processPath)
        if ([string]::Equals(
                $parent,
                $Path,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw "runtime process is still active: $($process.ProcessName) PID $($process.Id)"
        }
    }
}

if ([string]::IsNullOrWhiteSpace($RuntimeDir)) {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw 'LOCALAPPDATA is not set; pass -RuntimeDir explicitly'
    }
    $RuntimeDir = Join-Path $env:LOCALAPPDATA 'AllMyStuffSandboxRuntime'
}

$bundle = Get-FullPath $BundleDir
$runtime = Get-FullPath $RuntimeDir
$manifest = Assert-Bundle -Path $bundle

if (-not [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
    $production = Get-FullPath (Join-Path $env:LOCALAPPDATA 'AllMyStuff')
    if ($runtime -ceq $production -or
        $runtime.StartsWith(
            $production + [System.IO.Path]::DirectorySeparatorChar,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
        throw "sandbox runtime cannot live under the production install: $production"
    }
}
if ([string]::Equals(
        $bundle,
        $runtime,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
    throw 'source bundle and stable runtime must be different directories'
}

Assert-NoRuntimeProcess -Path $runtime

$parent = Split-Path -Parent $runtime
$leaf = Split-Path -Leaf $runtime
New-Item -ItemType Directory -Force -Path $parent | Out-Null
$stamp = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss-fffffff')
$staging = Join-Path $parent "$leaf.staging-$stamp"
$previous = Join-Path $parent "$leaf.previous-$stamp"
if ((Test-Path -LiteralPath $staging) -or
    (Test-Path -LiteralPath $previous)) {
    throw 'generated staging or previous path already exists'
}

New-Item -ItemType Directory -Path $staging | Out-Null
try {
    foreach ($file in @($manifest.files)) {
        Copy-Item -LiteralPath (Join-Path $bundle ([string]$file.name)) `
            -Destination (Join-Path $staging ([string]$file.name))
    }
    Copy-Item -LiteralPath (Join-Path $bundle 'sandbox-bundle.json') `
        -Destination (Join-Path $staging 'sandbox-bundle.json')
    [void](Assert-Bundle -Path $staging)

    $hadPrevious = Test-Path -LiteralPath $runtime
    if ($hadPrevious) {
        Move-Item -LiteralPath $runtime -Destination $previous
    }
    try {
        Move-Item -LiteralPath $staging -Destination $runtime
    } catch {
        if ($hadPrevious -and
            -not (Test-Path -LiteralPath $runtime) -and
            (Test-Path -LiteralPath $previous)) {
            Move-Item -LiteralPath $previous -Destination $runtime
        }
        throw
    }

    [ordered]@{
        status = 'staged'
        runtime = $runtime
        previous = if ($hadPrevious) { $previous } else { $null }
        source_bundle = $bundle
        source_commit = [string]$manifest.source.commit
        myownmesh_path = Join-Path $runtime 'myownmesh.exe'
        myownmesh_sha256 = Get-Sha256 (Join-Path $runtime 'myownmesh.exe')
    } | ConvertTo-Json -Depth 6
} catch {
    if (Test-Path -LiteralPath $staging) {
        throw "$($_.Exception.Message) Staging remains for inspection at $staging"
    }
    throw
}
