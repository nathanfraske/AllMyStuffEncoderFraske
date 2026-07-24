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

    [ValidateSet('native', 'compressed')]
    [string]$Delivery = 'native',

    [ValidateRange(1, 120)]
    [int]$Seconds = 8,

    [ValidateRange(1, 20)]
    [int]$Cycles = 1,

    [int]$ResizeEdge = 0,

    [ValidateRange(0, 240)]
    [int]$Fps = 0,

    [string]$Label = ('video-profile-' + (Get-Date -Format 'yyyyMMdd-HHmmss')),

    [string]$Backend = (Join-Path $env:LOCALAPPDATA 'Temp\amst\release\allmystuff-serve.exe'),

    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9A-Fa-f]{64}$')]
    [string]$ExpectedBackendHash,

    [string]$MeshDaemon = (Join-Path $env:LOCALAPPDATA 'AllMyStuff\myownmesh.exe'),

    [string]$Gui = (Join-Path $env:LOCALAPPDATA 'AllMyStuff\allmystuff-gui.exe'),

    [string]$Probe,

    [ValidatePattern('^[0-9A-Fa-f]{64}$')]
    [string]$ExpectedProbeHash = '494FBEF9EF5C359D18D6578111440A077D25E2D7366DB38ED77A0A7254C4BB35',

    [Parameter(Mandatory)]
    [ValidatePattern('^[A-Za-z0-9]{32,}-[A-Za-z0-9]{5}$')]
    [string]$ExpectedLocalNode,

    [string]$OutputRoot,

    [switch]$NoRewatch,
    [switch]$MotionPalette,
    [switch]$RestartMesh,
    [switch]$NoRestoreGui
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ProfileParameterSet = $PSCmdlet.ParameterSetName
if ($Label -cnotmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$') {
    throw 'profile label contains unsupported characters'
}
if ($MotionPalette -and $Delivery -ne 'native') {
    throw '-MotionPalette requires -Delivery native'
}

if ($ProfileParameterSet -eq 'Remote') {
    throw 'Remote mode is disabled because it cannot bind a remote run to this local trace. Use matched source or peer runs with an explicit run ID.'
}

$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
if ([string]::IsNullOrWhiteSpace($Probe)) {
    $Probe = Join-Path $ScriptRoot '..\artifacts\remote-p2-harness\video_prod_probe.exe'
}
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $ScriptRoot '..\artifacts\profiles'
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
$RunId = [Guid]::NewGuid().ToString('N')
$WatchdogCancel = Join-Path $RunDir "watchdog-cancel-$RunId"
$WatchdogTrigger = Join-Path $RunDir "watchdog-trigger-$RunId"
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
if ((Get-FileHash -LiteralPath $Probe -Algorithm SHA256).Hash -ne $ExpectedProbeHash.ToUpperInvariant()) {
    throw 'production probe hash mismatch; refusing an unreviewed harness binary'
}
if ((Get-FileHash -LiteralPath $Backend -Algorithm SHA256).Hash -ne
    $ExpectedBackendHash.ToUpperInvariant()) {
    throw 'candidate backend hash mismatch; refusing an unreviewed executable'
}
if (Test-Path -LiteralPath $RunDir) {
    throw "profile label already exists; choose a new label: $RunDir"
}
New-Item -ItemType Directory -Path $RunDir | Out-Null

$daemonProcess = $null
$activeDaemonProcess = $null
$backendProcess = $null
$watchdogProcess = $null
$probeExit = $null
$probeResults = @()
$started = Get-Date
$runFailure = $null
$traceHash = $null
$meshDaemonHash = (Get-FileHash -LiteralPath $MeshDaemon -Algorithm SHA256).Hash
$profileEnvironmentNames = @(
    'MYOWNMESH_BIN',
    'ALLMYSTUFF_HOME',
    'ALLMYSTUFF_CWD_LOG',
    'ALLMYSTUFF_VIDEO_STATS',
    'ALLMYSTUFF_VIDEO_PROFILE',
    'ALLMYSTUFF_VIDEO_PROFILE_TRACE',
    'ALLMYSTUFF_VIDEO_PROFILE_TRACE_EVENTS',
    'RUST_LOG',
    'ALLMYSTUFF_H264_DECODER'
)
$profileEnvironment = @{}
foreach ($name in $profileEnvironmentNames) {
    $profileEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

function Restore-ProfileEnvironment {
    foreach ($name in $profileEnvironmentNames) {
        $value = $profileEnvironment[$name]
        if ($null -eq $value) {
            Remove-Item "Env:$name" -ErrorAction SilentlyContinue
        } else {
            Set-Item "Env:$name" -Value $value
        }
    }
}

function Clear-ProfileEnvironment {
    foreach ($name in $profileEnvironmentNames) {
        Remove-Item "Env:$name" -ErrorAction SilentlyContinue
    }
}

function Get-Sha256([string]$Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Assert-CandidateToolHashes([string]$Phase) {
    if ((Get-Sha256 $Backend) -ne $ExpectedBackendHash.ToUpperInvariant()) {
        throw "candidate backend changed on disk $Phase"
    }
    if ((Get-Sha256 $Probe) -ne $ExpectedProbeHash.ToUpperInvariant()) {
        throw "production video probe changed on disk $Phase"
    }
}

function Get-CanonicalNode([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return ''
    }
    $node = ($Value -split ':', 2)[0]
    $separator = $node.LastIndexOf('-')
    if ($separator -gt 31) {
        $suffix = $node.Substring($separator + 1)
        if ($suffix.Length -eq 5 -and $suffix -cmatch '^[A-Za-z0-9]{5}$') {
            return $node.Substring(0, $separator)
        }
    }
    return $node
}

function Invoke-ParsedProbeList {
    $savedErrorPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $lines = @(& $Probe --list 2>&1)
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $savedErrorPreference
    }
    $raw = ($lines | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine
    $snapshot = $null
    if ($exitCode -eq 0) {
        try {
            $snapshot = $raw | ConvertFrom-Json
        }
        catch {
            $snapshot = $null
        }
    }
    [pscustomobject]@{
        ExitCode = $exitCode
        Raw = $raw
        Snapshot = $snapshot
    }
}

function Test-AuthenticatedIcePath($Snapshot, [string]$PeerNode) {
    if ($null -eq $Snapshot -or
        -not ($Snapshot.PSObject.Properties.Name -ccontains 'selected_ice_paths')) {
        return $false
    }
    $expectedPeer = Get-CanonicalNode $PeerNode
    foreach ($path in @($Snapshot.selected_ice_paths)) {
        if ($null -eq $path) { continue }
        $names = @($path.PSObject.Properties.Name)
        if (-not ($names -ccontains 'peer') -or
            -not ($names -ccontains 'authenticated') -or
            -not ($names -ccontains 'status') -or
            -not ($names -ccontains 'selected_pair')) {
            continue
        }
        $pair = $path.selected_pair
        if ($null -eq $pair) { continue }
        $pairNames = @($pair.PSObject.Properties.Name)
        if ((Get-CanonicalNode ([string]$path.peer)) -ceq $expectedPeer -and
            $path.authenticated -eq $true -and
            ([string]$path.status) -ceq 'active' -and
            $pairNames -ccontains 'local' -and
            $pairNames -ccontains 'remote' -and
            -not [string]::IsNullOrWhiteSpace([string]$pair.local) -and
            -not [string]::IsNullOrWhiteSpace([string]$pair.remote)) {
            return $true
        }
    }
    return $false
}

function Test-ExactReadiness($Snapshot) {
    if ($null -eq $Snapshot) { return $false }
    $names = @($Snapshot.PSObject.Properties.Name)
    if (-not ($names -ccontains 'local_node') -or
        -not ($names -ccontains 'remote_screen_sources') -or
        ([string]$Snapshot.local_node) -cne $ExpectedLocalNode) {
        return $false
    }
    $sources = @($Snapshot.remote_screen_sources)
    if ($ProfileParameterSet -eq 'Source') {
        foreach ($target in $SourceTargets) {
            $sourceMatches = @($sources | Where-Object {
                $null -ne $_ -and
                $_.PSObject.Properties.Name -ccontains 'id' -and
                ([string]$_.id) -ceq $target
            })
            if ($sourceMatches.Count -ne 1) { return $false }
            if (-not ($sourceMatches[0].PSObject.Properties.Name -ccontains 'node') -or
                -not (Test-AuthenticatedIcePath $Snapshot ([string]$sourceMatches[0].node))) {
                return $false
            }
        }
        return $true
    }
    $expectedPeer = Get-CanonicalNode $Peer
    $matchingSources = @($sources | Where-Object {
        $null -ne $_ -and
        $_.PSObject.Properties.Name -ccontains 'node' -and
        (Get-CanonicalNode ([string]$_.node)) -ceq $expectedPeer
    })
    return $matchingSources.Count -gt 0 -and
        (Test-AuthenticatedIcePath $Snapshot $Peer)
}

function Stop-ProcessAndWait($Process, [string]$Label) {
    if ($null -eq $Process) { return }
    if (-not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force -ErrorAction Stop
    }
    if (-not $Process.WaitForExit(10000)) {
        throw "$Label process $($Process.Id) did not exit within 10 seconds"
    }
}

function Start-CleanupWatchdog {
    param(
        [int]$OwnerPid,
        [int]$BackendPid,
        [int]$DaemonPid,
        [string]$CancelPath,
        [string]$TriggerPath
    )
    $watchdog = @'
param(
    [int]$OwnerPid,
    [int]$BackendPid,
    [int]$DaemonPid,
    [string]$CancelPath,
    [string]$TriggerPath,
    [string]$GuiPath,
    [string]$ProfileEnvironmentNames
)
$ErrorActionPreference = 'SilentlyContinue'
while (-not (Test-Path -LiteralPath $CancelPath -PathType Leaf)) {
    if ((Test-Path -LiteralPath $TriggerPath -PathType Leaf) -or
        $null -eq (Get-Process -Id $OwnerPid -ErrorAction SilentlyContinue)) {
        Start-Sleep -Seconds 1
        foreach ($candidatePid in @($BackendPid, $DaemonPid)) {
            $candidate = Get-Process -Id $candidatePid -ErrorAction SilentlyContinue
            if ($null -ne $candidate) {
                Stop-Process -Id $candidatePid -Force -ErrorAction SilentlyContinue
                [void]$candidate.WaitForExit(10000)
            }
        }
        foreach ($name in $ProfileEnvironmentNames.Split(',')) {
            Remove-Item "Env:$name" -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $GuiPath -PathType Leaf) {
            Start-Process -FilePath $GuiPath | Out-Null
        }
        exit 0
    }
    Start-Sleep -Milliseconds 500
}
'@
    $watchdogPath = Join-Path $RunDir "cleanup-watchdog-$RunId.ps1"
    $watchdog | Set-Content -LiteralPath $watchdogPath -Encoding utf8
    Start-Process -FilePath (
        Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    ) -ArgumentList @(
        '-NoLogo',
        '-NoProfile',
        '-NonInteractive',
        '-File',
        "`"$watchdogPath`"",
        '-OwnerPid',
        "$OwnerPid",
        '-BackendPid',
        "$BackendPid",
        '-DaemonPid',
        "$DaemonPid",
        '-CancelPath',
        "`"$CancelPath`"",
        '-TriggerPath',
        "`"$TriggerPath`"",
        '-GuiPath',
        "`"$Gui`"",
        '-ProfileEnvironmentNames',
        ($profileEnvironmentNames -join ',')
    ) -WindowStyle Hidden -PassThru
}

function Start-And-ProveNormalGui {
    if (-not (Test-Path -LiteralPath $Gui -PathType Leaf)) {
        throw "installed GUI is missing after profile cleanup: $Gui"
    }
    $installedBackend = Join-Path (Split-Path -Parent $Gui) 'allmystuff-serve.exe'
    Clear-ProfileEnvironment
    try {
        Start-Process -FilePath $Gui | Out-Null
        $deadline = (Get-Date).AddSeconds(60)
        do {
            Start-Sleep -Milliseconds 500
            $listResult = Invoke-ParsedProbeList
            $serve = Get-Process allmystuff-serve -ErrorAction SilentlyContinue |
                Where-Object {
                    -not [string]::IsNullOrWhiteSpace($_.Path) -and
                    [IO.Path]::GetFullPath($_.Path) -eq
                        [IO.Path]::GetFullPath($installedBackend)
                } |
                Select-Object -First 1
            $localNodeExact = $null -ne $listResult.Snapshot -and
                $listResult.Snapshot.PSObject.Properties.Name -ccontains 'local_node' -and
                ([string]$listResult.Snapshot.local_node) -ceq $ExpectedLocalNode
            $healthy = $listResult.ExitCode -eq 0 -and
                $localNodeExact -and
                $null -ne $serve
        } while (-not $healthy -and (Get-Date) -lt $deadline)
        if (-not $healthy) {
            throw 'installed GUI/backend did not recover after the profile run'
        }
    }
    finally {
        Restore-ProfileEnvironment
    }
}

try {
    # Profiling must be enabled before the backend process initializes. Stop
    # only this app's local processes; the reusable script restores the normal
    # installed GUI in `finally` unless explicitly told not to.
    foreach ($existingApp in @(Get-Process allmystuff-gui, allmystuff-serve `
        -ErrorAction SilentlyContinue)) {
        Stop-ProcessAndWait $existingApp $existingApp.ProcessName
    }
    # The installed backend owns its daemon through a kill-on-close job. Give
    # that child time to disappear before deciding whether a daemon remains.
    Start-Sleep -Milliseconds 500
    # A process path and the file currently at that path cannot prove which
    # bytes an already-running Windows process mapped. Always launch the pinned
    # daemon after hashing it so a stale loaded image cannot contaminate an
    # otherwise exact profile.
    foreach ($existingDaemon in @(Get-Process myownmesh -ErrorAction SilentlyContinue)) {
        Stop-ProcessAndWait $existingDaemon 'existing mesh daemon'
    }
    if ($RestartMesh) {
        Write-Verbose '-RestartMesh is retained for compatibility; exact profiles always restart the pinned daemon.'
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

    $daemonProcess = Start-Process -FilePath $MeshDaemon -ArgumentList 'serve' -PassThru `
        -WindowStyle Hidden -RedirectStandardOutput $MeshOut -RedirectStandardError $MeshErr
    $activeDaemonProcess = $daemonProcess
    # Do not race the backend's daemon supervisor. If the explicitly launched
    # daemon has not bound its local control socket yet, the backend can mistake
    # that brief gap for absence and launch a second copy under its job object.
    $daemonReady = $false
    $daemonDeadline = (Get-Date).AddSeconds(15)
    do {
        Start-Sleep -Milliseconds 250
        if ($null -ne $activeDaemonProcess -and $activeDaemonProcess.HasExited) {
            throw 'pinned mesh daemon exited during control-socket readiness'
        }
        if ((Get-Sha256 $MeshDaemon) -ne $meshDaemonHash) {
            throw 'pinned mesh daemon changed on disk during readiness'
        }
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
    $watchdogProcess = Start-CleanupWatchdog -OwnerPid $PID `
        -BackendPid $backendProcess.Id `
        -DaemonPid $(if ($null -ne $daemonProcess) { $daemonProcess.Id } else { 0 }) `
        -CancelPath $WatchdogCancel `
        -TriggerPath $WatchdogTrigger

    $ready = $false
    $deadline = (Get-Date).AddSeconds(60)
    $listResult = $null
    do {
        Start-Sleep -Milliseconds 500
        if ($backendProcess.HasExited) {
            throw "candidate backend exited during readiness with code $($backendProcess.ExitCode)"
        }
        if ($null -ne $activeDaemonProcess -and $activeDaemonProcess.HasExited) {
            throw 'pinned mesh daemon exited during backend readiness'
        }
        if ((Get-Sha256 $MeshDaemon) -ne $meshDaemonHash) {
            throw 'pinned mesh daemon changed on disk during backend readiness'
        }
        if ((Get-Sha256 $Backend) -ne $ExpectedBackendHash.ToUpperInvariant()) {
            throw 'candidate backend changed on disk during readiness'
        }
        $listResult = Invoke-ParsedProbeList
        $ready = $listResult.ExitCode -eq 0 -and
            (Test-ExactReadiness $listResult.Snapshot)
    } while (-not $ready -and (Get-Date) -lt $deadline)
    if (-not $ready) {
        if ($null -ne $listResult) {
            $listResult.Raw |
                Set-Content -LiteralPath (Join-Path $RunDir 'readiness-list.txt') -Encoding utf8
        }
        throw 'candidate backend failed exact local node, remote source or peer, and authenticated selected ICE readiness within 60 seconds'
    }

    $targets = if ($PSCmdlet.ParameterSetName -eq 'Source') { $SourceTargets } else { @($Peer) }
    $targets = @($targets)
    for ($index = 0; $index -lt $targets.Count; $index++) {
        $target = $targets[$index]
        $probeArgs = @('--seconds', "$Seconds", '--cycles', "$Cycles", '--mode', $Mode,
            '--delivery', $Delivery,
            '--active-timeout', '20', '--first-frame-timeout', '12')
        if ($PSCmdlet.ParameterSetName -eq 'Source') {
            $probeArgs += @('--source', $target)
        } else {
            $probeArgs += @('--peer', $target)
        }
        if ($ResizeEdge -gt 0) { $probeArgs += @('--resize-edge', "$ResizeEdge") }
        if ($Fps -gt 0) { $probeArgs += @('--fps', "$Fps") }
        if ($MotionPalette) { $probeArgs += '--motion-palette' }
        if ($NoRewatch) { $probeArgs += '--no-rewatch' }
        $currentProbeLog = if ($targets.Count -eq 1) {
            $ProbeLog
        } else {
            Join-Path $RunDir ('probe-{0:D2}.log' -f ($index + 1))
        }

        # Windows PowerShell 5 promotes a native program's stderr lines into
        # ErrorRecord objects. Keep collecting them and decide from the actual
        # process exit code so a useful probe failure reaches its log intact.
        Assert-CandidateToolHashes "before probing $target"
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
        if ($backendProcess.HasExited) {
            throw "candidate backend exited after probing $target with code $($backendProcess.ExitCode)"
        }
        if ($null -ne $activeDaemonProcess -and $activeDaemonProcess.HasExited) {
            throw "pinned mesh daemon exited after probing $target"
        }
        if ((Get-Sha256 $MeshDaemon) -ne $meshDaemonHash) {
            throw "pinned mesh daemon changed on disk after probing $target"
        }
        if ($index + 1 -lt $targets.Count) { Start-Sleep -Milliseconds 500 }
    }
    # The probe owns the route and has exited, so the route is quiesced here.
    # The trace writer flushes at least every 250 ms. Give it four intervals.
    Start-Sleep -Seconds 1
}
catch {
    $runFailure = ($_ | Out-String)
}
finally {
    $cleanupFailures = @()
    # On an error the route teardown can still be in flight. Preserve at least
    # one writer-flush interval before forcing either candidate process down.
    if ($null -ne $backendProcess) {
        Start-Sleep -Milliseconds 250
    }
    foreach ($entry in @(
        [pscustomobject]@{ Process = $backendProcess; Label = 'candidate backend' },
        [pscustomobject]@{ Process = $daemonProcess; Label = 'pinned mesh daemon' }
    )) {
        try {
            Stop-ProcessAndWait $entry.Process $entry.Label
        }
        catch {
            $cleanupFailures += ($_ | Out-String)
        }
    }
    try {
        Assert-CandidateToolHashes 'after candidate shutdown'
    }
    catch {
        $cleanupFailures += ($_ | Out-String)
    }
    $traceHash = if (Test-Path -LiteralPath $Trace -PathType Leaf) {
        Get-Sha256 $Trace
    } else { $null }
    if ($null -eq $traceHash -or (Get-Item -LiteralPath $Trace).Length -eq 0) {
        $cleanupFailures += 'profile trace is missing or empty after the candidate stopped'
    }
    try {
        if (-not $NoRestoreGui) {
            Start-And-ProveNormalGui
            Start-Sleep -Seconds 1
            $traceHashAfterRestore = if (Test-Path -LiteralPath $Trace -PathType Leaf) {
                Get-Sha256 $Trace
            } else { $null }
            if ($traceHashAfterRestore -ne $traceHash) {
                throw 'profile trace changed after the clean installed GUI was restored'
            }
        } else {
            Restore-ProfileEnvironment
        }
    }
    catch {
        $cleanupFailures += ($_ | Out-String)
    }
    if ($cleanupFailures.Count -eq 0) {
        New-Item -ItemType File -Path $WatchdogCancel | Out-Null
        if ($null -ne $watchdogProcess -and
            -not $watchdogProcess.WaitForExit(10000)) {
            $cleanupFailures += 'cleanup watchdog did not acknowledge cancellation within 10 seconds'
        } elseif ($null -ne $watchdogProcess -and $watchdogProcess.ExitCode -ne 0) {
            $cleanupFailures += "cleanup watchdog exited with code $($watchdogProcess.ExitCode)"
        }
    }
    if ($cleanupFailures.Count -gt 0) {
        if ($null -ne $watchdogProcess -and -not $watchdogProcess.HasExited) {
            New-Item -ItemType File -Path $WatchdogTrigger -ErrorAction SilentlyContinue |
                Out-Null
        }
        $cleanupText = $cleanupFailures -join "`r`n"
        if ($null -eq $runFailure) {
            $runFailure = $cleanupText
        } else {
            $runFailure += "`r`nCleanup failure:`r`n$cleanupText"
        }
    }
}

if ($null -ne $runFailure) {
    throw $runFailure
}

$analyzer = Join-Path $ScriptRoot 'summarize_video_profile.py'
$python = Get-Command python -ErrorAction SilentlyContinue
if ($null -ne $python -and (Test-Path -LiteralPath $Trace -PathType Leaf)) {
    try {
        & $python.Source $analyzer $Trace --top-frames 10 2>&1 |
            Tee-Object -FilePath $Summary
        if ($LASTEXITCODE -ne 0) {
            "optional profile summarizer exited with code $LASTEXITCODE" |
                Add-Content -LiteralPath $Summary -Encoding utf8
        }
    } catch {
        "optional profile summarizer unavailable: $($_.Exception.Message)" |
            Set-Content -LiteralPath $Summary -Encoding utf8
    }
}

$manifest = [ordered]@{
    run_id = $RunId
    label = $Label
    started = $started.ToString('o')
    finished = (Get-Date).ToString('o')
    sources = if ($PSCmdlet.ParameterSetName -eq 'Source') { $SourceTargets } else { @() }
    peer = if ($PSCmdlet.ParameterSetName -eq 'Peer') { $Peer } else { $null }
    expected_local_node = $ExpectedLocalNode
    mode = $Mode
    decoder = $Decoder
    delivery = $Delivery
    seconds = $Seconds
    cycles = $Cycles
    resize_edge = $ResizeEdge
    fps = $Fps
    rewatch = -not $NoRewatch
    motion_palette = [bool]$MotionPalette
    probe_exit = $probeExit
    probes = $probeResults
    backend_sha256 = Get-Sha256 $Backend
    expected_backend_sha256 = $ExpectedBackendHash.ToUpperInvariant()
    mesh_daemon_sha256 = $meshDaemonHash
    probe_sha256 = (Get-FileHash -LiteralPath $Probe -Algorithm SHA256).Hash
    trace = $Trace
    trace_sha256 = $traceHash
}
$manifest | ConvertTo-Json -Depth 4 |
    Set-Content -LiteralPath (Join-Path $RunDir 'run.json') -Encoding utf8
Write-Output "profile run complete: $RunDir"
$global:LASTEXITCODE = 0
