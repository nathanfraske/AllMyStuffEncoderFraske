[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Stage', 'Start', 'Status', 'Control', 'Probe', 'Stop', 'Collect')]
    [string]$Action,

    [Parameter(Mandatory = $true)]
    [string]$BundleDir,

    [Parameter(Mandatory = $true)]
    [string]$PeerId,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]{0,63}$')]
    [string]$InstanceId,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]{0,63}$')]
    [string]$RunId,

    [Parameter(Mandatory = $true)]
    [string]$PolicyPath,

    [string]$ArtifactDir = 'C:\t\ams-sandbox-results',

    [ValidateSet('Isolated', 'LocalClaim', 'TestNetwork')]
    [string]$NetworkMode = 'Isolated',

    [string[]]$AllowedNetworkId = @(),

    [string[]]$ControlArguments = @('identity'),

    [string[]]$ProbeArguments = @('--list'),

    [ValidateSet('Off', 'Summary', 'Trace')]
    [string]$ProfileMode = 'Trace',

    [ValidateRange(100, 1000000)]
    [int]$ProfileTraceEvents = 100000,

    [ValidateRange(1, 60)]
    [int]$TelemetrySeconds = 1,

    [ValidateRange(2, 120)]
    [int]$RequestTtlMinutes = 15,

    [ValidateSet('Auto', 'Terminal', 'Worker')]
    [string]$BootstrapMode = 'Auto',

    [ValidateRange(60, 3600)]
    [int]$WorkerIdleSeconds = 900,

    [switch]$Execute
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$remoteStage = '.allmystuff-sandbox-stage\inbox'
$resultPattern =
    '(?s)__ALLMYSTUFF_SANDBOX_REMOTE_RESULT_BEGIN__\s*(.*?)\s*__ALLMYSTUFF_SANDBOX_REMOTE_RESULT_END__'

function Get-FullPath {
    param([Parameter(Mandatory = $true)][string]$Path)
    return [System.IO.Path]::GetFullPath($Path)
}

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Text
    )
    $encoding = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($Path, $Text, $encoding)
}

function Remove-TerminalControl {
    param([Parameter(Mandatory = $true)][string]$Text)

    $escape = [string][char]27
    $clean = [regex]::Replace(
        $Text,
        "$escape\[[0-?]*[ -/]*[@-~]",
        ''
    )
    return [regex]::Replace(
        $clean,
        "$escape\][^\a]*(?:\a|$escape\\)",
        ''
    )
}

function Get-CanonicalNode {
    param([Parameter(Mandatory = $true)][string]$Node)
    $trimmed = $Node.Trim()
    if ($trimmed -match '^(?<key>.+)-[0-9A-Fa-f]{5}$') {
        return $Matches.key.ToLowerInvariant()
    }
    return $trimmed.ToLowerInvariant()
}

function Assert-Bundle {
    param([Parameter(Mandatory = $true)][string]$Path)
    $manifestPath = Join-Path $Path 'sandbox-bundle.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "sandbox manifest is missing: $manifestPath"
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw |
        ConvertFrom-Json
    if ($manifest.schema -ne 1 -or
        [string]$manifest.kind -cne 'allmystuff-sandbox-bundle') {
        throw 'unsupported sandbox bundle manifest'
    }
    foreach ($file in @($manifest.files)) {
        $name = [string]$file.name
        if ([IO.Path]::GetFileName($name) -cne $name) {
            throw "unsafe bundle file name: '$name'"
        }
        $filePath = Join-Path $Path $name
        if (-not (Test-Path -LiteralPath $filePath -PathType Leaf)) {
            throw "bundle file is missing: $filePath"
        }
        if ((Get-Item -LiteralPath $filePath).Length -ne
            [int64]$file.size -or
            (Get-Sha256 $filePath) -cne [string]$file.sha256) {
            throw "bundle seal mismatch for $name"
        }
    }
    return $manifest
}

function Get-TargetPolicy {
    param(
        [Parameter(Mandatory = $true)][object]$Policy,
        [Parameter(Mandatory = $true)][string]$RequestedPeer
    )
    if ($Policy.schema -ne 1 -or
        [string]$Policy.kind -cne 'allmystuff-sandbox-fleet-policy') {
        throw 'unsupported sandbox fleet policy'
    }
    $canonical = Get-CanonicalNode $RequestedPeer
    $matches = @($Policy.targets | Where-Object {
        (Get-CanonicalNode ([string]$_.peer_id)) -ceq $canonical
    })
    if ($matches.Count -ne 1) {
        throw "peer is not an exact, unique fleet-policy target: $RequestedPeer"
    }
    $target = $matches[0]
    if ([string]::IsNullOrWhiteSpace([string]$target.computer_name)) {
        throw 'fleet-policy target has no computer_name'
    }
    if ([string]::IsNullOrWhiteSpace([string]$target.peer_id)) {
        throw 'fleet-policy target has no peer_id'
    }
    return $target
}

function Invoke-P2 {
    param(
        [Parameter(Mandatory = $true)][string]$Transport,
        [Parameter(Mandatory = $true)][string]$ExactPeer,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )
    $savedPeer = [Environment]::GetEnvironmentVariable(
        'ALLMYSTUFF_P2_PEER',
        [EnvironmentVariableTarget]::Process
    )
    $savedInventory = [Environment]::GetEnvironmentVariable(
        'ALLMYSTUFF_P2_NO_INVENTORY',
        [EnvironmentVariableTarget]::Process
    )
    try {
        $env:ALLMYSTUFF_P2_PEER = $ExactPeer
        $env:ALLMYSTUFF_P2_NO_INVENTORY = '1'
        $output = & $Transport @Arguments 2>&1
        $exitCode = $LASTEXITCODE
        $text = (($output | Out-String).Trim())
        if ($exitCode -ne 0) {
            throw "p2 transport failed with exit code $exitCode`: $text"
        }
        return $text
    } finally {
        [Environment]::SetEnvironmentVariable(
            'ALLMYSTUFF_P2_PEER',
            $savedPeer,
            [EnvironmentVariableTarget]::Process
        )
        [Environment]::SetEnvironmentVariable(
            'ALLMYSTUFF_P2_NO_INVENTORY',
            $savedInventory,
            [EnvironmentVariableTarget]::Process
        )
    }
}

if (-not $Execute) {
    throw 'remote sandbox actions require the explicit -Execute switch'
}
if ($NetworkMode -ceq 'TestNetwork' -and
    @($AllowedNetworkId).Count -eq 0) {
    throw 'TestNetwork mode requires AllowedNetworkId'
}
if ($NetworkMode -cne 'TestNetwork' -and
    @($AllowedNetworkId).Count -ne 0) {
    throw "$NetworkMode mode cannot carry AllowedNetworkId"
}
if ($BootstrapMode -ceq 'Worker' -and $Action -in @('Stage', 'Start')) {
    throw 'Stage and Start require the one terminal bootstrap that launches the worker'
}
$useWorker = $BootstrapMode -ceq 'Worker' -or (
    $BootstrapMode -ceq 'Auto' -and $Action -notin @('Stage', 'Start')
)

$bundle = Get-FullPath $BundleDir
$policyFile = Get-FullPath $PolicyPath
if (-not (Test-Path -LiteralPath $policyFile -PathType Leaf)) {
    throw "sandbox fleet policy is missing: $policyFile"
}
$policy = Get-Content -LiteralPath $policyFile -Raw | ConvertFrom-Json
$target = Get-TargetPolicy -Policy $policy -RequestedPeer $PeerId
$exactPeer = [string]$target.peer_id
$manifest = Assert-Bundle -Path $bundle
$manifestPath = Join-Path $bundle 'sandbox-bundle.json'
$transport = Join-Path $bundle 'p2_remote_transport.exe'
$bootstrap = Join-Path $bundle 'bootstrap-allmystuff-sandbox-remote.ps1'
foreach ($required in @($transport, $bootstrap)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "bundle is missing remote harness input: $required"
    }
}

$requestId = [Guid]::NewGuid().ToString('D').ToLowerInvariant()
$now = [DateTime]::UtcNow
$request = [ordered]@{
    schema = 1
    kind = 'allmystuff-sandbox-remote-request'
    request_id = $requestId
    created_utc = $now.ToString('o')
    expires_utc = $now.AddMinutes($RequestTtlMinutes).ToString('o')
    action = $Action
    target = [ordered]@{
        peer_id = $exactPeer
        computer_name = [string]$target.computer_name
        label = [string]$target.label
    }
    bundle = [ordered]@{
        manifest_sha256 = Get-Sha256 $manifestPath
        source_commit = [string]$manifest.source.commit
    }
    instance_id = $InstanceId
    run_id = $RunId
    protected_ports = [int[]]@($target.protected_ports)
    network_mode = $NetworkMode
    allowed_network_ids = [string[]]@($AllowedNetworkId)
    profile = [ordered]@{
        mode = $ProfileMode
        trace_events = $ProfileTraceEvents
        telemetry_seconds = $TelemetrySeconds
    }
    install_firewall_if_elevated = $true
    worker_idle_seconds = $WorkerIdleSeconds
    worker_stop_after = ($Action -ceq 'Collect')
    control_arguments = [string[]]@($ControlArguments)
    probe_arguments = [string[]]@($ProbeArguments)
}

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) "ams-sandbox-$requestId"
$requestPath = Join-Path $tempRoot 'sandbox-remote-request.json'
New-Item -ItemType Directory -Path $tempRoot | Out-Null
try {
    Write-Utf8NoBom -Path $requestPath -Text (
        ($request | ConvertTo-Json -Depth 12) + [Environment]::NewLine
    )

    if ($Action -in @('Stage', 'Start')) {
        $uploadFiles = [System.Collections.Generic.List[string]]::new()
        foreach ($file in @($manifest.files)) {
            $uploadFiles.Add((Join-Path $bundle ([string]$file.name)))
        }
        $uploadFiles.Add($manifestPath)
        [void](Invoke-P2 -Transport $transport -ExactPeer $exactPeer `
            -Arguments (@('upload', $remoteStage) + $uploadFiles.ToArray()))
    }
    [void](Invoke-P2 -Transport $transport -ExactPeer $exactPeer `
        -Arguments @('upload', $remoteStage, $requestPath))

    $expectedResultPath =
        ".allmystuff-sandbox-stage\outbox\sandbox-remote-result-$requestId.json"
    $downloadedResult = Join-Path $tempRoot 'remote-result.json'
    $declaredResultSize = [int64]0
    $declaredResultHash = $null
    $raw = $null
    if ($useWorker) {
        $expectedSealPath =
            ".allmystuff-sandbox-stage\outbox\sandbox-remote-result-$requestId.seal.json"
        $downloadedSeal = Join-Path $tempRoot 'remote-result-seal.json'
        [void](Invoke-P2 -Transport $transport -ExactPeer $exactPeer `
            -Arguments @(
                'download-wait',
                $expectedSealPath,
                $downloadedSeal,
                [string]$WorkerIdleSeconds
            ))
        $seal = Get-Content -LiteralPath $downloadedSeal -Raw |
            ConvertFrom-Json
        if ($seal.schema -ne 1 -or
            [string]$seal.kind -cne
                'allmystuff-sandbox-worker-result-seal' -or
            [string]$seal.request_id -cne $requestId -or
            [string]$seal.target_peer_id -cne $exactPeer -or
            [string]$seal.result_remote_path -cne $expectedResultPath -or
            [string]$seal.result_sha256 -cnotmatch '^[0-9A-F]{64}$' -or
            [string]$seal.manifest_sha256 -cne
                [string]$request.bundle.manifest_sha256 -or
            -not [string]::Equals(
                [string]$seal.host,
                [string]$target.computer_name,
                [StringComparison]::OrdinalIgnoreCase
            )) {
            throw 'sandbox worker result seal does not match the request'
        }
        if (-not [int64]::TryParse(
            [string]$seal.result_size,
            [ref]$declaredResultSize
        ) -or $declaredResultSize -le 0) {
            throw 'sandbox worker result seal carries an invalid size'
        }
        $declaredResultHash = [string]$seal.result_sha256
        [void](Invoke-P2 -Transport $transport -ExactPeer $exactPeer `
            -Arguments @('download', $expectedResultPath, $downloadedResult))
        $raw = 'WORKER_RESULT_SEAL=' + (
            $seal | ConvertTo-Json -Depth 6 -Compress
        )
    } else {
        $raw = Invoke-P2 -Transport $transport -ExactPeer $exactPeer `
            -Arguments @('exec-bootstrap')
        $cleanRaw = Remove-TerminalControl -Text $raw
        $match = [regex]::Match($cleanRaw, $resultPattern)
        if (-not $match.Success) {
            throw "remote bootstrap returned no bounded result marker: $raw"
        }
        $envelope = @{}
        foreach ($rawLine in @($match.Groups[1].Value -split '\r?\n')) {
            $line = $rawLine.Trim()
            if ([string]::IsNullOrWhiteSpace($line)) {
                continue
            }
            if ($line -cnotmatch
                '^(?<key>[a-z0-9_]+)=(?<value>[^\r\n]*)$') {
                throw "remote result envelope has an invalid line: '$line'"
            }
            $key = [string]$Matches.key
            if ($envelope.ContainsKey($key)) {
                throw "remote result envelope repeats '$key'"
            }
            $envelope[$key] = [string]$Matches.value
        }
        foreach ($requiredKey in @(
            'request_id',
            'host',
            'target_peer_id',
            'result_remote_path',
            'result_size',
            'result_sha256'
        )) {
            if (-not $envelope.ContainsKey($requiredKey) -or
                [string]::IsNullOrWhiteSpace(
                    [string]$envelope[$requiredKey]
                )) {
                throw "remote result envelope is missing '$requiredKey'"
            }
        }
        if ([string]$envelope.request_id -cne $requestId -or
            [string]$envelope.target_peer_id -cne $exactPeer -or
            -not [string]::Equals(
                [string]$envelope.host,
                [string]$target.computer_name,
                [StringComparison]::OrdinalIgnoreCase
            )) {
            throw 'remote result envelope identity does not match the request'
        }
        if ([string]$envelope.result_remote_path -cne
                $expectedResultPath -or
            [string]$envelope.result_sha256 -cnotmatch
                '^[0-9A-F]{64}$') {
            throw 'remote result envelope carries an invalid seal or path'
        }
        if (-not [int64]::TryParse(
            [string]$envelope.result_size,
            [ref]$declaredResultSize
        ) -or $declaredResultSize -le 0) {
            throw 'remote result envelope carries an invalid size'
        }
        $declaredResultHash = [string]$envelope.result_sha256
        [void](Invoke-P2 -Transport $transport -ExactPeer $exactPeer `
            -Arguments @('download', $expectedResultPath, $downloadedResult))
    }
    if ((Get-Item -LiteralPath $downloadedResult).Length -ne
        $declaredResultSize -or
        (Get-Sha256 $downloadedResult) -cne
        $declaredResultHash) {
        throw 'downloaded remote result does not match its envelope seal'
    }
    $result = Get-Content -LiteralPath $downloadedResult -Raw |
        ConvertFrom-Json
    if ([string]$result.request_id -cne $requestId -or
        [string]$result.run_id -cne $RunId -or
        [string]$result.action -cne $Action -or
        [string]$result.target_peer_id -cne $exactPeer -or
        [string]$result.instance_id -cne $InstanceId -or
        [string]$result.source_commit -cne [string]$manifest.source.commit -or
        -not [string]::Equals(
            [string]$result.host,
            [string]$target.computer_name,
            [StringComparison]::OrdinalIgnoreCase
        )) {
        throw 'downloaded remote result identity does not match the request'
    }

    $resultDir = Get-FullPath (
        Join-Path (Join-Path $ArtifactDir ([string]$target.computer_name)) `
            $RunId
    )
    New-Item -ItemType Directory -Path $resultDir -Force | Out-Null
    Write-Utf8NoBom -Path (Join-Path $resultDir "$requestId-result.json") `
        -Text (($result | ConvertTo-Json -Depth 24) + [Environment]::NewLine)
    Write-Utf8NoBom -Path (Join-Path $resultDir "$requestId-terminal.txt") `
        -Text ($raw + [Environment]::NewLine)
    if ([string]$result.kind -ceq
        'allmystuff-sandbox-remote-worker-error') {
        throw "remote sandbox worker failed: $([string]$result.error)"
    }

    if ($Action -ceq 'Collect') {
        $remoteArchive = [string]$result.operation.remote_path
        if ([string]::IsNullOrWhiteSpace($remoteArchive)) {
            throw 'collect result did not provide a remote archive path'
        }
        $archiveName = [IO.Path]::GetFileName($remoteArchive)
        $localArchive = Join-Path $resultDir $archiveName
        if (Test-Path -LiteralPath $localArchive) {
            throw "refusing to overwrite downloaded artifact: $localArchive"
        }
        [void](Invoke-P2 -Transport $transport -ExactPeer $exactPeer `
            -Arguments @('download', $remoteArchive, $localArchive))
        if ((Get-Item -LiteralPath $localArchive).Length -ne
            [int64]$result.operation.size -or
            (Get-Sha256 $localArchive) -cne
            [string]$result.operation.sha256) {
            throw 'downloaded sandbox artifact does not match the remote seal'
        }
        $result | Add-Member -NotePropertyName local_archive `
            -NotePropertyValue $localArchive
    }

    $result | ConvertTo-Json -Depth 24
} finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}
