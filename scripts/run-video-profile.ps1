[CmdletBinding(DefaultParameterSetName = 'Peer')]
param(
    [Parameter(Mandatory, ParameterSetName = 'Peer')]
    [string]$Peer,

    [Parameter(Mandatory, ParameterSetName = 'Source')]
    [string]$Source,

    [Parameter(ParameterSetName = 'Source')]
    [string]$SwitchSources,

    [Parameter(Mandatory, ParameterSetName = 'Remote')]
    [string]$RemoteScript,

    [Parameter(ParameterSetName = 'Remote')]
    [string]$RemoteTransport,

    [ValidateSet('balanced', 'game', 'studio', 'studio-lossless')]
    [string]$Mode = 'balanced',

    [ValidateSet('auto', 'nvdec', 'openh264')]
    [string]$Decoder = 'auto',

    [ValidateRange(1, 120)]
    [int]$Seconds = 8,

    [ValidateRange(1, 20)]
    [int]$Cycles = 1,

    [int]$ResizeEdge = 0,

    [string]$Label = ('video-profile-' + (Get-Date -Format 'yyyyMMdd-HHmmss')),

    [string]$Backend = (Join-Path $env:LOCALAPPDATA 'Temp\amst\release\allmystuff-serve.exe'),

    [string]$MeshDaemon = (Join-Path $env:LOCALAPPDATA 'AllMyStuff\myownmesh.exe'),

    [string]$Gui = (Join-Path $env:LOCALAPPDATA 'AllMyStuff\allmystuff-gui.exe'),

    [string]$Probe,

    [ValidatePattern('^[0-9A-Fa-f]{64}$')]
    [string]$ExpectedProbeHash = '494FBEF9EF5C359D18D6578111440A077D25E2D7366DB38ED77A0A7254C4BB35',

    [string]$OutputRoot,

    [switch]$NoRewatch,
    [switch]$RestartMesh,
    [switch]$NoRestoreGui
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
if ([string]::IsNullOrWhiteSpace($Probe)) {
    $Probe = Join-Path $ScriptRoot '..\artifacts\remote-p2-harness\video_prod_probe.exe'
}
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $ScriptRoot '..\artifacts\profiles'
}
if ($PSCmdlet.ParameterSetName -eq 'Remote' -and
    [string]::IsNullOrWhiteSpace($RemoteTransport)) {
    $RemoteTransport = Join-Path $ScriptRoot '..\artifacts\remote-p2-harness\p2_remote_transport.exe'
}
$Backend = [IO.Path]::GetFullPath($Backend)
$MeshDaemon = [IO.Path]::GetFullPath($MeshDaemon)
$Gui = [IO.Path]::GetFullPath($Gui)
$Probe = [IO.Path]::GetFullPath($Probe)
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
$RunDir = Join-Path $OutputRoot $Label
$Trace = Join-Path $RunDir 'trace.jsonl'
$ProbeLog = Join-Path $RunDir 'probe.log'
$BackendOut = Join-Path $RunDir 'backend.stdout.log'
$BackendErr = Join-Path $RunDir 'backend.stderr.log'
$MeshOut = Join-Path $RunDir 'mesh.stdout.log'
$MeshErr = Join-Path $RunDir 'mesh.stderr.log'
$Summary = Join-Path $RunDir 'summary.txt'
$SourceTargets = @()
if ($PSCmdlet.ParameterSetName -eq 'Source') {
    $SourceTargets += $Source
    if (-not [string]::IsNullOrWhiteSpace($SwitchSources)) {
        $SourceTargets += @($SwitchSources.Split(';') |
            ForEach-Object { $_.Trim() } | Where-Object { $_ })
    }
}

foreach ($required in @($Backend, $MeshDaemon, $Probe)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "required executable is missing: $required"
    }
}
if ($PSCmdlet.ParameterSetName -eq 'Remote') {
    foreach ($required in @($RemoteTransport, $RemoteScript)) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
            throw "required remote harness file is missing: $required"
        }
    }
    $RemoteTransport = [IO.Path]::GetFullPath($RemoteTransport)
    $RemoteScript = [IO.Path]::GetFullPath($RemoteScript)
}
if ((Get-FileHash -LiteralPath $Probe -Algorithm SHA256).Hash -ne $ExpectedProbeHash.ToUpperInvariant()) {
    throw 'production probe hash mismatch; refusing an unreviewed harness binary'
}
New-Item -ItemType Directory -Path $RunDir -Force | Out-Null

$daemonProcess = $null
$backendProcess = $null
$probeExit = $null
$probeResults = @()
$started = Get-Date

try {
    # Profiling must be enabled before the backend process initializes. Stop
    # only this app's local processes; the reusable script restores the normal
    # installed GUI in `finally` unless explicitly told not to.
    Get-Process allmystuff-gui, allmystuff-serve -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
    # The installed backend owns its daemon through a kill-on-close job. Give
    # that child time to disappear before deciding whether a daemon remains.
    Start-Sleep -Milliseconds 500
    $existingDaemon = Get-Process myownmesh -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -ne $existingDaemon -and $RestartMesh) {
        Stop-Process -Id $existingDaemon.Id -Force -ErrorAction SilentlyContinue
        $existingDaemon = $null
    }
    $env:MYOWNMESH_BIN = $MeshDaemon
    $env:ALLMYSTUFF_HOME = Join-Path $HOME '.allmystuff'
    $env:ALLMYSTUFF_CWD_LOG = '0'
    $env:ALLMYSTUFF_VIDEO_STATS = '1'
    $env:ALLMYSTUFF_VIDEO_PROFILE = '1'
    $env:ALLMYSTUFF_VIDEO_PROFILE_TRACE = $Trace
    $env:ALLMYSTUFF_VIDEO_PROFILE_TRACE_EVENTS = '100000'
    $env:RUST_LOG = 'info,allmystuff_node=debug'
    if ($Decoder -eq 'auto') {
        Remove-Item Env:ALLMYSTUFF_H264_DECODER -ErrorAction SilentlyContinue
    } else {
        $env:ALLMYSTUFF_H264_DECODER = $Decoder
    }

    if ($null -eq $existingDaemon) {
        $daemonProcess = Start-Process -FilePath $MeshDaemon -ArgumentList 'serve' -PassThru `
            -WindowStyle Hidden -RedirectStandardOutput $MeshOut -RedirectStandardError $MeshErr
    } elseif (-not [string]::IsNullOrWhiteSpace($existingDaemon.Path) -and
        [IO.Path]::GetFullPath($existingDaemon.Path) -ne $MeshDaemon) {
        throw "running mesh daemon is not the pinned executable: $($existingDaemon.Path)"
    }
    # Do not race the backend's daemon supervisor. If the explicitly launched
    # daemon has not bound its local control socket yet, the backend can mistake
    # that brief gap for absence and launch a second copy under its job object.
    $daemonReady = $false
    $daemonDeadline = (Get-Date).AddSeconds(15)
    do {
        Start-Sleep -Milliseconds 250
        $savedErrorPreference = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        & $MeshDaemon ctl status *> $null
        $daemonStatusExit = $LASTEXITCODE
        $ErrorActionPreference = $savedErrorPreference
        $daemonReady = $daemonStatusExit -eq 0
    } while (-not $daemonReady -and (Get-Date) -lt $daemonDeadline)
    if (-not $daemonReady) {
        throw 'pinned mesh daemon did not bind its local control socket within 15 seconds'
    }
    $backendProcess = Start-Process -FilePath $Backend -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $BackendOut -RedirectStandardError $BackendErr

    $needles = if ($PSCmdlet.ParameterSetName -eq 'Source') {
        @($SourceTargets)
    } elseif ($PSCmdlet.ParameterSetName -eq 'Peer') {
        @($Peer)
    } else {
        @()
    }
    $ready = $false
    $deadline = (Get-Date).AddSeconds(60)
    do {
        Start-Sleep -Milliseconds 500
        $listing = (& $Probe --list 2>&1 | Out-String)
        $listingExit = $LASTEXITCODE
        $missing = @($needles | Where-Object {
            $listing.IndexOf($_, [StringComparison]::Ordinal) -lt 0
        })
        if ($listingExit -eq 0 -and $missing.Count -eq 0) {
            $ready = $true
        }
    } while (-not $ready -and (Get-Date) -lt $deadline)
    if (-not $ready) {
        $listing | Set-Content -LiteralPath (Join-Path $RunDir 'readiness-list.txt') -Encoding utf8
        throw "candidate backend did not advertise requested source/peer within 60 seconds: $($needles -join ', ')"
    }

    if ($PSCmdlet.ParameterSetName -eq 'Remote') {
        $savedErrorPreference = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        & $RemoteTransport exec $RemoteScript 2>&1 | Tee-Object -FilePath $ProbeLog
        $probeExit = $LASTEXITCODE
        $ErrorActionPreference = $savedErrorPreference
        $probeResults += [ordered]@{
            target = 'remote-exec'
            exit = $probeExit
            log = $ProbeLog
        }
        if ($probeExit -ne 0) {
            throw "authenticated remote video harness failed with exit code $probeExit"
        }
    } else {
        $targets = if ($PSCmdlet.ParameterSetName -eq 'Source') { $SourceTargets } else { @($Peer) }
        $targets = @($targets)
        for ($index = 0; $index -lt $targets.Count; $index++) {
            $target = $targets[$index]
            $probeArgs = @('--seconds', "$Seconds", '--cycles', "$Cycles", '--mode', $Mode,
                '--active-timeout', '20', '--first-frame-timeout', '12')
            if ($PSCmdlet.ParameterSetName -eq 'Source') {
                $probeArgs += @('--source', $target)
            } else {
                $probeArgs += @('--peer', $target)
            }
            if ($ResizeEdge -gt 0) { $probeArgs += @('--resize-edge', "$ResizeEdge") }
            if ($NoRewatch) { $probeArgs += '--no-rewatch' }
            $currentProbeLog = if ($targets.Count -eq 1) {
                $ProbeLog
            } else {
                Join-Path $RunDir ('probe-{0:D2}.log' -f ($index + 1))
            }

            # Windows PowerShell 5 promotes a native program's stderr lines into
            # ErrorRecord objects. Keep collecting them and decide from the actual
            # process exit code so a useful probe failure reaches its log intact.
            $savedErrorPreference = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            & $Probe @probeArgs 2>&1 | Tee-Object -FilePath $currentProbeLog
            $probeExit = $LASTEXITCODE
            $ErrorActionPreference = $savedErrorPreference
            $probeResults += [ordered]@{
                target = $target
                exit = $probeExit
                log = $currentProbeLog
            }
            if ($probeExit -ne 0) {
                throw "production video probe failed for $target with exit code $probeExit"
            }
            if ($index + 1 -lt $targets.Count) { Start-Sleep -Milliseconds 500 }
        }
    }
    # The trace writer flushes at least every 250 ms. Give it four intervals
    # before stopping the candidate so the tail is deterministic.
    Start-Sleep -Seconds 1
}
finally {
    foreach ($process in @($backendProcess, $daemonProcess)) {
        if ($null -ne $process -and -not $process.HasExited) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
    }
    if (-not $NoRestoreGui -and (Test-Path -LiteralPath $Gui -PathType Leaf)) {
        Start-Process -FilePath $Gui | Out-Null
    }
}

$analyzer = Join-Path $ScriptRoot 'summarize_video_profile.py'
$python = Get-Command python -ErrorAction SilentlyContinue
if ($null -ne $python -and (Test-Path -LiteralPath $Trace -PathType Leaf)) {
    & $python.Source $analyzer $Trace --top-frames 10 2>&1 |
        Tee-Object -FilePath $Summary
}

$manifest = [ordered]@{
    label = $Label
    started = $started.ToString('o')
    finished = (Get-Date).ToString('o')
    sources = if ($PSCmdlet.ParameterSetName -eq 'Source') { $SourceTargets } else { @() }
    peer = if ($PSCmdlet.ParameterSetName -eq 'Peer') { $Peer } else { $null }
    remote_script = if ($PSCmdlet.ParameterSetName -eq 'Remote') { $RemoteScript } else { $null }
    mode = $Mode
    decoder = $Decoder
    seconds = $Seconds
    cycles = $Cycles
    resize_edge = $ResizeEdge
    rewatch = -not $NoRewatch
    probe_exit = $probeExit
    probes = $probeResults
    backend_sha256 = (Get-FileHash -LiteralPath $Backend -Algorithm SHA256).Hash
    probe_sha256 = (Get-FileHash -LiteralPath $Probe -Algorithm SHA256).Hash
    trace = $Trace
    trace_sha256 = if (Test-Path -LiteralPath $Trace) {
        (Get-FileHash -LiteralPath $Trace -Algorithm SHA256).Hash
    } else { $null }
}
$manifest | ConvertTo-Json -Depth 4 |
    Set-Content -LiteralPath (Join-Path $RunDir 'run.json') -Encoding utf8
Write-Output "profile run complete: $RunDir"
