[CmdletBinding()]
param(
    [string]$RepoRoot,
    [ValidateSet(
        "linux-x86_64",
        "linux-aarch64",
        "linux-aarch64-musl",
        "linux-riscv64",
        "macos-x86_64",
        "macos-aarch64",
        "windows-x86_64"
    )]
    [string]$Platform,
    [string]$Archive
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($RepoRoot)) {
    $RepoRoot = Split-Path -Parent $PSScriptRoot
}
$root = (Resolve-Path -LiteralPath $RepoRoot).Path
$pinPath = Join-Path $root ".myownmesh-rev"
$metadataPath = Join-Path $root ".myownmesh-release-sha256"
$pin = (Get-Content -Raw -LiteralPath $pinPath).Trim()
$entries = [System.Collections.Generic.Dictionary[string, string]]::new(
    [System.StringComparer]::Ordinal
)

$lineNumber = 0
foreach ($rawLine in Get-Content -LiteralPath $metadataPath) {
    $lineNumber++
    $line = $rawLine.Trim()
    if ($line.Length -eq 0 -or $line.StartsWith("#", [System.StringComparison]::Ordinal)) {
        continue
    }

    $fields = $line -split "\s+"
    if ($fields.Count -ne 2) {
        throw "${metadataPath}:${lineNumber}: expected exactly two fields"
    }
    if ($entries.ContainsKey($fields[0])) {
        throw "${metadataPath}:${lineNumber}: duplicate key '$($fields[0])'"
    }
    $entries.Add($fields[0], $fields[1])
}

$expectedPlatforms = @(
    "linux-x86_64",
    "linux-aarch64",
    "linux-aarch64-musl",
    "linux-riscv64",
    "macos-x86_64",
    "macos-aarch64",
    "windows-x86_64"
)
$expectedKeys = @("tag", "commit") + $expectedPlatforms
foreach ($key in $expectedKeys) {
    if (-not $entries.ContainsKey($key)) {
        throw "${metadataPath}: missing '$key'"
    }
}
if ($entries.Count -ne $expectedKeys.Count) {
    $unknown = @($entries.Keys | Where-Object { $_ -notin $expectedKeys })
    throw "${metadataPath}: unexpected key(s): $($unknown -join ', ')"
}
if ($entries["tag"] -cne $pin) {
    throw "${metadataPath}: tag '$($entries["tag"])' does not match pin '$pin'"
}
if ($entries["tag"] -cnotmatch "^v[0-9]+\.[0-9]+\.[0-9]+$") {
    throw "${metadataPath}: tag must be a stable semantic version"
}
if ($entries["commit"] -cnotmatch "^[0-9a-f]{40}$") {
    throw "${metadataPath}: commit must be a 40-character lowercase hexadecimal SHA"
}
foreach ($name in $expectedPlatforms) {
    if ($entries[$name] -cnotmatch "^[0-9a-f]{64}$") {
        throw "${metadataPath}: '$name' must have a 64-character lowercase SHA-256"
    }
}

if ($Archive.Length -gt 0 -and $Platform.Length -eq 0) {
    throw "-Archive requires -Platform"
}
if ($Platform.Length -gt 0 -and -not $entries.ContainsKey($Platform)) {
    throw "${metadataPath}: no hash for '$Platform'"
}
if ($Archive.Length -gt 0) {
    $archivePath = (Resolve-Path -LiteralPath $Archive).Path
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    $expected = $entries[$Platform]
    if ($actual -cne $expected) {
        throw "SHA-256 mismatch for '$archivePath': expected $expected, got $actual"
    }
}

$scope = if ($Platform.Length -gt 0) { $Platform } else { "all platforms" }
Write-Output "MyOwnMesh release metadata verified: $pin at $($entries["commit"]) ($scope)."
