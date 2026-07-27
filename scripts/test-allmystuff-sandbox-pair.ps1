[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Full', 'Prepare', 'Teardown')]
    [string]$Action,

    [Parameter(Mandatory = $true)]
    [string]$BundleDir,

    [string]$FirstPeerId,

    [string]$SecondPeerId,

    [ValidatePattern('^[a-z0-9][a-z0-9-]{0,63}$')]
    [string]$FirstInstanceId,

    [ValidatePattern('^[a-z0-9][a-z0-9-]{0,63}$')]
    [string]$SecondInstanceId,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]{0,23}$')]
    [string]$RunId,

    [Parameter(Mandatory = $true)]
    [string]$PolicyPath,

    [string]$ArtifactDir = 'C:\t\ams-sandbox-results',

    [string]$SessionPath,

    [ValidateRange(1, 300)]
    [int]$Seconds = 5,

    [ValidateRange(1, 100)]
    [int]$Cycles = 2,

    [ValidateSet('native', 'compressed', 'both')]
    [string]$Delivery = 'both',

    [ValidateRange(1, 300)]
    [int]$PeerTimeoutSeconds = 60,

    [switch]$MotionPalette,

    [switch]$Execute
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-FullPath {
    param([Parameter(Mandatory = $true)][string]$Path)
    return [System.IO.Path]::GetFullPath($Path)
}

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Text
    )
    $encoding = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($Path, $Text, $encoding)
}

function Get-CanonicalNode {
    param([Parameter(Mandatory = $true)][string]$Node)
    $trimmed = $Node.Trim()
    if ($trimmed -match '^(?<key>.+)-[0-9A-Fa-f]{5}$') {
        return $Matches.key.ToLowerInvariant()
    }
    return $trimmed.ToLowerInvariant()
}

function Invoke-Remote {
    param(
        [Parameter(Mandatory = $true)][string]$Peer,
        [Parameter(Mandatory = $true)][string]$Instance,
        [Parameter(Mandatory = $true)][string]$RemoteAction,
        [Parameter(Mandatory = $true)][string]$OperationRunId,
        [string[]]$Control = @('identity'),
        [string[]]$Probe = @('--list'),
        [string]$NetworkMode = 'Isolated',
        [string[]]$AllowedNetwork = @()
    )
    $arguments = @{
        Action = $RemoteAction
        BundleDir = $script:Bundle
        PeerId = $Peer
        InstanceId = $Instance
        RunId = $OperationRunId
        PolicyPath = $script:Policy
        ArtifactDir = $script:Artifacts
        NetworkMode = $NetworkMode
        AllowedNetworkId = $AllowedNetwork
        ControlArguments = $Control
        ProbeArguments = $Probe
        ProfileMode = 'Trace'
        ProfileTraceEvents = 100000
        TelemetrySeconds = 1
        BootstrapMode = if ($RemoteAction -in @('Stage', 'Start')) {
            'Terminal'
        } else {
            'Worker'
        }
        WorkerIdleSeconds = 900
        Execute = $true
    }
    $raw = & $script:Deploy @arguments
    return (($raw | Out-String).Trim()) | ConvertFrom-Json
}

function Get-SandboxNodeId {
    param([Parameter(Mandatory = $true)][object]$StartResult)
    $node = [string]$StartResult.operation.identity.scan.node_id
    if ([string]::IsNullOrWhiteSpace($node)) {
        throw 'sandbox start result did not contain its node_id'
    }
    return $node
}

function Get-ExactScreen {
    param(
        [Parameter(Mandatory = $true)][object]$Listing,
        [Parameter(Mandatory = $true)][string]$ExpectedNode
    )
    $canonical = Get-CanonicalNode $ExpectedNode
    $candidates = @($Listing.remote_screen_sources | Where-Object {
        (Get-CanonicalNode ([string]$_.node)) -ceq $canonical
    })
    $general = @($candidates | Where-Object {
        [string]$_.id -match ':screen$'
    })
    if ($general.Count -ne 1) {
        throw "expected one general screen for sandbox peer $ExpectedNode, found $($general.Count)"
    }
    return [string]$general[0].id
}

function Expand-AndSummarizeArtifact {
    param([Parameter(Mandatory = $true)][string]$Archive)

    if (-not (Test-Path -LiteralPath $Archive -PathType Leaf)) {
        throw "sandbox artifact archive is missing: $Archive"
    }
    $destination = Join-Path (Split-Path -Parent $Archive) `
        ([IO.Path]::GetFileNameWithoutExtension($Archive))
    if (Test-Path -LiteralPath $destination) {
        throw "refusing to overwrite expanded sandbox artifact: $destination"
    }
    Expand-Archive -LiteralPath $Archive -DestinationPath $destination
    $trace = Join-Path $destination 'artifacts\video-profile.jsonl'
    $summaryPath = Join-Path $destination 'profile-summary.txt'
    if (Test-Path -LiteralPath $trace -PathType Leaf) {
        if ((Get-Item -LiteralPath $trace).Length -gt 0) {
            $python = Get-Command python -ErrorAction Stop
            $summary = & $python.Source (
                Join-Path $script:Bundle 'summarize_video_profile.py'
            ) $trace 2>&1
            if ($LASTEXITCODE -ne 0) {
                throw "profile analyzer failed: $(($summary | Out-String).Trim())"
            }
            Write-Utf8NoBom -Path $summaryPath -Text (
                (($summary | Out-String).Trim()) + [Environment]::NewLine
            )
        } else {
            $summaryPath = $null
        }
    } else {
        $trace = $null
        $summaryPath = $null
    }
    return [pscustomobject]@{
        directory = $destination
        trace = $trace
        summary = $summaryPath
    }
}

function Invoke-Cleanup {
    param(
        [Parameter(Mandatory = $true)][object]$Session,
        [switch]$Collect
    )
    $errors = [System.Collections.Generic.List[string]]::new()
    foreach ($side in @('first', 'second')) {
        $entry = $Session.$side
        if ($null -eq $entry) {
            continue
        }
        $otherSide = if ($side -ceq 'first') { 'second' } else { 'first' }
        $other = $Session.$otherSide
        if ($null -ne $other -and
            -not [string]::IsNullOrWhiteSpace([string]$other.node_id)) {
            try {
                [void](Invoke-Remote -Peer ([string]$entry.peer_id) `
                    -Instance ([string]$entry.instance_id) `
                    -RemoteAction 'Control' -OperationRunId $RunId `
                    -Control @(
                        'stop-sharing-with',
                        [string]$other.node_id
                    ))
            } catch {
                $errors.Add("$side remove screen-share grant: $($_.Exception.Message)")
            }
        }
        if (-not [string]::IsNullOrWhiteSpace([string]$Session.network_id)) {
            try {
                [void](Invoke-Remote -Peer ([string]$entry.peer_id) `
                    -Instance ([string]$entry.instance_id) `
                    -RemoteAction 'Control' -OperationRunId $RunId `
                    -Control @('leave', [string]$Session.network_id))
            } catch {
                $errors.Add("$side leave: $($_.Exception.Message)")
            }
        }
        try {
            [void](Invoke-Remote -Peer ([string]$entry.peer_id) `
                -Instance ([string]$entry.instance_id) `
                -RemoteAction 'Stop' -OperationRunId $RunId)
        } catch {
            $errors.Add("$side stop: $($_.Exception.Message)")
        }
    }
    if ($Collect) {
        foreach ($side in @('first', 'second')) {
            $entry = $Session.$side
            if ($null -eq $entry) {
                continue
            }
            try {
                $result = Invoke-Remote -Peer ([string]$entry.peer_id) `
                    -Instance ([string]$entry.instance_id) `
                    -RemoteAction 'Collect' -OperationRunId $RunId
                $expanded = Expand-AndSummarizeArtifact `
                    -Archive ([string]$result.local_archive)
                $entry | Add-Member -NotePropertyName 'artifact_archive' `
                    -NotePropertyValue ([string]$result.local_archive) -Force
                $entry | Add-Member -NotePropertyName 'artifact_directory' `
                    -NotePropertyValue ([string]$expanded.directory) -Force
                $entry | Add-Member -NotePropertyName 'profile_trace' `
                    -NotePropertyValue ([string]$expanded.trace) -Force
                $entry | Add-Member -NotePropertyName 'profile_summary' `
                    -NotePropertyValue ([string]$expanded.summary) -Force
            } catch {
                $errors.Add("$side collect: $($_.Exception.Message)")
            }
        }
        $traces = @(
            @($Session.first.profile_trace, $Session.second.profile_trace) |
                Where-Object {
                    -not [string]::IsNullOrWhiteSpace([string]$_) -and
                    (Test-Path -LiteralPath ([string]$_) -PathType Leaf) -and
                    (Get-Item -LiteralPath ([string]$_)).Length -gt 0
                }
        )
        if ($traces.Count -eq 2) {
            try {
                $python = Get-Command python -ErrorAction Stop
                $summary = & $python.Source (
                    Join-Path $script:Bundle 'summarize_video_profile.py'
                ) @traces 2>&1
                if ($LASTEXITCODE -ne 0) {
                    throw "pair profile analyzer failed: $(($summary | Out-String).Trim())"
                }
                $pairSummary = Join-Path (Split-Path -Parent $SessionPath) `
                    'pair-profile-summary.txt'
                Write-Utf8NoBom -Path $pairSummary -Text (
                    (($summary | Out-String).Trim()) +
                        [Environment]::NewLine
                )
                $Session | Add-Member -NotePropertyName `
                    'pair_profile_summary' -NotePropertyValue $pairSummary `
                    -Force
            } catch {
                $errors.Add("pair profile summary: $($_.Exception.Message)")
            }
        }
    }
    return @($errors)
}

if (-not $Execute) {
    throw 'sandbox pair actions require the explicit -Execute switch'
}
$script:Bundle = Get-FullPath $BundleDir
$script:Policy = Get-FullPath $PolicyPath
$script:Artifacts = Get-FullPath $ArtifactDir
$script:Deploy = Join-Path $script:Bundle `
    'deploy-allmystuff-sandbox-remote.ps1'
if (-not (Test-Path -LiteralPath $script:Deploy -PathType Leaf)) {
    throw "bundle is missing the pair deployer: $script:Deploy"
}

if ([string]::IsNullOrWhiteSpace($SessionPath)) {
    $SessionPath = Join-Path (
        Join-Path $script:Artifacts "pair-$RunId"
    ) 'sandbox-pair-session.json'
}
$SessionPath = Get-FullPath $SessionPath

if ($Action -ceq 'Teardown') {
    if (-not (Test-Path -LiteralPath $SessionPath -PathType Leaf)) {
        throw "sandbox pair session is missing: $SessionPath"
    }
    $session = Get-Content -LiteralPath $SessionPath -Raw | ConvertFrom-Json
    $cleanupErrors = @(Invoke-Cleanup -Session $session -Collect)
    $session.status = if ($cleanupErrors.Count -eq 0) {
        'stopped_and_collected'
    } else {
        'cleanup_incomplete'
    }
    $session | Add-Member -NotePropertyName 'cleanup_errors' `
        -NotePropertyValue @($cleanupErrors) -Force
    Write-Utf8NoBom -Path $SessionPath -Text (
        ($session | ConvertTo-Json -Depth 20) + [Environment]::NewLine
    )
    $session | ConvertTo-Json -Depth 20
    exit $(if ($cleanupErrors.Count -eq 0) { 0 } else { 1 })
}

foreach ($required in @(
    $FirstPeerId,
    $SecondPeerId,
    $FirstInstanceId,
    $SecondInstanceId
)) {
    if ([string]::IsNullOrWhiteSpace([string]$required)) {
        throw 'Full and Prepare require both peer IDs and both instance IDs'
    }
}
if ((Get-CanonicalNode $FirstPeerId) -ceq
    (Get-CanonicalNode $SecondPeerId)) {
    throw 'sandbox pair endpoints must be different peers'
}

$sessionDir = Split-Path -Parent $SessionPath
New-Item -ItemType Directory -Path $sessionDir -Force | Out-Null
$session = [pscustomobject][ordered]@{
    schema = 1
    kind = 'allmystuff-sandbox-pair-session'
    run_id = $RunId
    status = 'starting'
    created_utc = [DateTime]::UtcNow.ToString('o')
    bundle = $script:Bundle
    policy = $script:Policy
    network_id = $null
    authorization = [pscustomobject][ordered]@{
        kind = 'mutual_ephemeral_screen_share'
        first_grant = $null
        second_grant = $null
    }
    first = [pscustomobject][ordered]@{
        peer_id = $FirstPeerId
        instance_id = $FirstInstanceId
        node_id = $null
        artifact_archive = $null
        artifact_directory = $null
        profile_trace = $null
        profile_summary = $null
    }
    second = [pscustomobject][ordered]@{
        peer_id = $SecondPeerId
        instance_id = $SecondInstanceId
        node_id = $null
        artifact_archive = $null
        artifact_directory = $null
        profile_trace = $null
        profile_summary = $null
    }
    tests = @()
    cleanup_errors = @()
}
Write-Utf8NoBom -Path $SessionPath -Text (
    ($session | ConvertTo-Json -Depth 20) + [Environment]::NewLine
)

$completed = $false
try {
    $firstStart = Invoke-Remote -Peer $FirstPeerId `
        -Instance $FirstInstanceId -RemoteAction 'Start' `
        -OperationRunId $RunId
    $secondStart = Invoke-Remote -Peer $SecondPeerId `
        -Instance $SecondInstanceId -RemoteAction 'Start' `
        -OperationRunId $RunId
    if ([string]$firstStart.operation.status -cne 'running' -or
        [string]$secondStart.operation.status -cne 'running') {
        throw 'one or both sandbox endpoints did not start'
    }
    if ([string]$firstStart.worker.status -cne 'running' -or
        [string]$secondStart.worker.status -cne 'running') {
        throw 'one or both sandbox workers did not start'
    }
    $session.first.node_id = Get-SandboxNodeId $firstStart
    $session.second.node_id = Get-SandboxNodeId $secondStart

    $generated = Invoke-Remote -Peer $FirstPeerId `
        -Instance $FirstInstanceId -RemoteAction 'Control' `
        -OperationRunId $RunId `
        -Control @('generate-test-network-id')
    $session.network_id = [string]$generated.operation.network_id
    if ([string]::IsNullOrWhiteSpace([string]$session.network_id)) {
        throw 'sandbox network generator returned no network_id'
    }

    [void](Invoke-Remote -Peer $FirstPeerId -Instance $FirstInstanceId `
        -RemoteAction 'Control' -OperationRunId $RunId `
        -Control @('join', [string]$session.network_id, "sandbox-$RunId"))
    [void](Invoke-Remote -Peer $SecondPeerId -Instance $SecondInstanceId `
        -RemoteAction 'Control' -OperationRunId $RunId `
        -Control @('join', [string]$session.network_id, "sandbox-$RunId"))
    [void](Invoke-Remote -Peer $FirstPeerId -Instance $FirstInstanceId `
        -RemoteAction 'Control' -OperationRunId $RunId `
        -Control @(
            'wait-exact-peer',
            [string]$session.network_id,
            [string]$session.second.node_id,
            [string]$PeerTimeoutSeconds
        ))
    [void](Invoke-Remote -Peer $SecondPeerId -Instance $SecondInstanceId `
        -RemoteAction 'Control' -OperationRunId $RunId `
        -Control @(
            'wait-exact-peer',
            [string]$session.network_id,
            [string]$session.first.node_id,
            [string]$PeerTimeoutSeconds
        ))
    $session.authorization.first_grant = Invoke-Remote `
        -Peer $FirstPeerId -Instance $FirstInstanceId `
        -RemoteAction 'Control' -OperationRunId $RunId `
        -Control @(
            'grant-screen-view',
            [string]$session.second.node_id
        )
    $session.authorization.second_grant = Invoke-Remote `
        -Peer $SecondPeerId -Instance $SecondInstanceId `
        -RemoteAction 'Control' -OperationRunId $RunId `
        -Control @(
            'grant-screen-view',
            [string]$session.first.node_id
        )
    if ([string]$session.authorization.first_grant.operation.status -cne
            'granted' -or
        [string]$session.authorization.second_grant.operation.status -cne
            'granted') {
        throw 'one or both sandbox screen-share grants were not created'
    }
    $session.status = 'ready'
    Write-Utf8NoBom -Path $SessionPath -Text (
        ($session | ConvertTo-Json -Depth 20) + [Environment]::NewLine
    )

    if ($Action -ceq 'Prepare') {
        $completed = $true
        $session | ConvertTo-Json -Depth 20
        exit 0
    }

    $firstListResult = Invoke-Remote -Peer $FirstPeerId `
        -Instance $FirstInstanceId -RemoteAction 'Probe' `
        -OperationRunId "$RunId-first-list" -Probe @('--list')
    $secondListResult = Invoke-Remote -Peer $SecondPeerId `
        -Instance $SecondInstanceId -RemoteAction 'Probe' `
        -OperationRunId "$RunId-second-list" -Probe @('--list')
    $firstListing = [string]$firstListResult.operation.output |
        ConvertFrom-Json
    $secondListing = [string]$secondListResult.operation.output |
        ConvertFrom-Json
    $firstSource = Get-ExactScreen -Listing $secondListing `
        -ExpectedNode ([string]$session.first.node_id)
    $secondSource = Get-ExactScreen -Listing $firstListing `
        -ExpectedNode ([string]$session.second.node_id)

    $deliveries = if ($Delivery -ceq 'both') {
        @('native', 'compressed')
    } else {
        @($Delivery)
    }
    $tests = [System.Collections.Generic.List[object]]::new()
    foreach ($mode in $deliveries) {
        foreach ($direction in @(
            [pscustomobject]@{
                viewer_peer = $FirstPeerId
                viewer_instance = $FirstInstanceId
                source = $secondSource
                name = 'first-views-second'
            },
            [pscustomobject]@{
                viewer_peer = $SecondPeerId
                viewer_instance = $SecondInstanceId
                source = $firstSource
                name = 'second-views-first'
            }
        )) {
            $probeArgs = @(
                '--source', [string]$direction.source,
                '--seconds', [string]$Seconds,
                '--cycles', [string]$Cycles,
                '--delivery', $mode
            )
            if ($MotionPalette -and $mode -ceq 'native') {
                $probeArgs += '--motion-palette'
            }
            $probeRun = "$RunId-$($direction.name)-$mode"
            $result = Invoke-Remote -Peer ([string]$direction.viewer_peer) `
                -Instance ([string]$direction.viewer_instance) `
                -RemoteAction 'Probe' -OperationRunId $probeRun `
                -Probe $probeArgs
            $tests.Add([pscustomobject][ordered]@{
                direction = [string]$direction.name
                delivery = $mode
                source = [string]$direction.source
                result = $result.operation.report
            })
        }
    }
    $session.tests = $tests.ToArray()
    $session.status = 'tests_complete'
    Write-Utf8NoBom -Path $SessionPath -Text (
        ($session | ConvertTo-Json -Depth 30) + [Environment]::NewLine
    )
    $completed = $true
} finally {
    if ($Action -ceq 'Full') {
        $cleanupErrors = @(Invoke-Cleanup -Session $session -Collect)
        $session.cleanup_errors = @($cleanupErrors)
        $session.status = if ($completed -and $cleanupErrors.Count -eq 0) {
            'complete'
        } elseif ($cleanupErrors.Count -eq 0) {
            'test_failed_cleaned'
        } else {
            'cleanup_incomplete'
        }
        Write-Utf8NoBom -Path $SessionPath -Text (
            ($session | ConvertTo-Json -Depth 30) + [Environment]::NewLine
        )
    }
}

$session | ConvertTo-Json -Depth 30
