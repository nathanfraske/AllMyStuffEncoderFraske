[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Install', 'Show', 'Remove')]
    [string]$Action,

    [string]$RuntimeDir,

    [ValidateSet('Private', 'Public', 'Domain')]
    [string[]]$Profile = @('Private', 'Public'),

    [switch]$NoElevate
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ruleGroup = 'AllMyStuff Sandbox Harness'
$tcpRule = 'AllMyStuff-Sandbox-MyOwnMesh-TCP'
$udpRule = 'AllMyStuff-Sandbox-MyOwnMesh-UDP'

function Get-FullPath {
    param([Parameter(Mandatory = $true)][string]$Path)
    return [System.IO.Path]::GetFullPath($Path)
}

function Test-IsAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator
    )
}

function Quote-Single {
    param([Parameter(Mandatory = $true)][string]$Value)
    return "'" + $Value.Replace("'", "''") + "'"
}

function Invoke-Elevated {
    $profiles = '@(' + (($Profile | ForEach-Object { Quote-Single $_ }) -join ',') + ')'
    $command = "& $(Quote-Single $PSCommandPath) " +
        "-Action $(Quote-Single $Action) " +
        "-RuntimeDir $(Quote-Single $RuntimeDir) " +
        "-Profile $profiles -NoElevate"
    $encoded = [Convert]::ToBase64String(
        [Text.Encoding]::Unicode.GetBytes($command)
    )
    $process = Start-Process -FilePath 'powershell.exe' `
        -ArgumentList @('-NoProfile', '-EncodedCommand', $encoded) `
        -Verb RunAs -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "elevated firewall action failed with exit code $($process.ExitCode)"
    }
}

function Get-RuleSummary {
    $rows = foreach ($name in @($tcpRule, $udpRule)) {
        $rule = Get-NetFirewallRule -Name $name -PolicyStore ActiveStore `
            -ErrorAction SilentlyContinue
        if ($null -eq $rule) {
            [pscustomobject]@{
                name = $name
                present = $false
            }
            continue
        }
        $app = Get-NetFirewallApplicationFilter `
            -AssociatedNetFirewallRule $rule
        $port = Get-NetFirewallPortFilter -AssociatedNetFirewallRule $rule
        [pscustomobject]@{
            name = $name
            present = $true
            enabled = [string]$rule.Enabled
            direction = [string]$rule.Direction
            action = [string]$rule.Action
            profile = [string]$rule.Profile
            program = [string]$app.Program
            protocol = [string]$port.Protocol
            local_port = [string]$port.LocalPort
        }
    }
    return @($rows)
}

if ([string]::IsNullOrWhiteSpace($RuntimeDir)) {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw 'LOCALAPPDATA is not set; pass -RuntimeDir explicitly'
    }
    $RuntimeDir = Join-Path $env:LOCALAPPDATA 'AllMyStuffSandboxRuntime'
}
$RuntimeDir = Get-FullPath $RuntimeDir
$meshPath = Join-Path $RuntimeDir 'myownmesh.exe'

if ($Action -eq 'Show') {
    Get-RuleSummary | ConvertTo-Json -Depth 6
    exit 0
}

if (-not (Test-IsAdministrator)) {
    if ($NoElevate) {
        throw 'Install and Remove require an elevated PowerShell process'
    }
    Invoke-Elevated
    exit 0
}

if ($Action -eq 'Remove') {
    foreach ($name in @($tcpRule, $udpRule)) {
        Get-NetFirewallRule -Name $name -PolicyStore PersistentStore `
            -ErrorAction SilentlyContinue |
            Remove-NetFirewallRule
    }
    Get-RuleSummary | ConvertTo-Json -Depth 6
    exit 0
}

$manifestPath = Join-Path $RuntimeDir 'sandbox-bundle.json'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf) -or
    -not (Test-Path -LiteralPath $meshPath -PathType Leaf)) {
    throw "stage a sealed sandbox runtime before installing rules: $RuntimeDir"
}
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$meshEntry = @($manifest.files | Where-Object {
    [string]$_.name -ceq 'myownmesh.exe'
}) | Select-Object -First 1
if ($null -eq $meshEntry) {
    throw 'sandbox manifest does not contain myownmesh.exe'
}
$actualHash = (Get-FileHash -LiteralPath $meshPath -Algorithm SHA256).Hash
if ($actualHash -cne [string]$meshEntry.sha256 -or
    (Get-Item -LiteralPath $meshPath).Length -ne [int64]$meshEntry.size) {
    throw 'stable MyOwnMesh binary does not match its sealed manifest'
}

$created = @()
try {
    foreach ($spec in @(
        [pscustomobject]@{ name = $tcpRule; protocol = 'TCP' },
        [pscustomobject]@{ name = $udpRule; protocol = 'UDP' }
    )) {
        $existing = Get-NetFirewallRule -Name $spec.name `
            -PolicyStore PersistentStore -ErrorAction SilentlyContinue
        if ($null -ne $existing) {
            $app = Get-NetFirewallApplicationFilter `
                -AssociatedNetFirewallRule $existing
            $port = Get-NetFirewallPortFilter `
                -AssociatedNetFirewallRule $existing
            $actualProfiles = @(
                ([string]$existing.Profile -split ',') |
                    ForEach-Object { $_.Trim() } |
                    Sort-Object -Unique
            )
            $wantedProfiles = @($Profile | Sort-Object -Unique)
            $sameProfiles = (
                ($actualProfiles -join ',') -ceq ($wantedProfiles -join ',')
            )
            if ($existing.Enabled -ne 'True' -or
                $existing.Direction -ne 'Inbound' -or
                $existing.Action -ne 'Allow' -or
                -not $sameProfiles -or
                -not [string]::Equals(
                    [string]$app.Program,
                    $meshPath,
                    [System.StringComparison]::OrdinalIgnoreCase
                ) -or
                [string]$port.Protocol -cne $spec.protocol -or
                [string]$port.LocalPort -cne 'Any') {
                throw "firewall rule exists with a different scope: $($spec.name)"
            }
            continue
        }
        New-NetFirewallRule `
            -Name $spec.name `
            -DisplayName "AllMyStuff Sandbox MyOwnMesh ($($spec.protocol))" `
            -Group $ruleGroup `
            -Enabled True `
            -Direction Inbound `
            -Action Allow `
            -Profile $Profile `
            -Program $meshPath `
            -Protocol $spec.protocol | Out-Null
        $created += $spec.name
    }
} catch {
    foreach ($name in $created) {
        Get-NetFirewallRule -Name $name -PolicyStore PersistentStore `
            -ErrorAction SilentlyContinue |
            Remove-NetFirewallRule
    }
    throw
}

Get-RuleSummary | ConvertTo-Json -Depth 6
