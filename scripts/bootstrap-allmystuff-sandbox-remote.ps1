[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$resultBegin = '__ALLMYSTUFF_SANDBOX_REMOTE_RESULT_BEGIN__'
$resultEnd = '__ALLMYSTUFF_SANDBOX_REMOTE_RESULT_END__'
$requestKind = 'allmystuff-sandbox-remote-request'
$stageRelative = '.allmystuff-sandbox-stage\inbox'

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

function Test-IsAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator
    )
}

function Assert-SafeId {
    param(
        [Parameter(Mandatory = $true)][string]$Value,
        [Parameter(Mandatory = $true)][string]$Role
    )
    if ($Value -cnotmatch '^[a-z0-9][a-z0-9-]{0,63}$') {
        throw "$Role contains unsupported characters: '$Value'"
    }
}

function Assert-Request {
    param([Parameter(Mandatory = $true)][object]$Request)

    if ($Request.schema -ne 1 -or [string]$Request.kind -cne $requestKind) {
        throw 'unsupported sandbox remote request'
    }
    if ([string]$Request.request_id -cnotmatch
        '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$') {
        throw 'sandbox request_id is not a lowercase GUID'
    }
    Assert-SafeId -Value ([string]$Request.instance_id) -Role 'instance_id'
    Assert-SafeId -Value ([string]$Request.run_id) -Role 'run_id'

    $expires = [DateTime]::Parse(
        [string]$Request.expires_utc,
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::AssumeUniversal
    ).ToUniversalTime()
    if ([DateTime]::UtcNow -gt $expires) {
        throw "sandbox request expired at $($expires.ToString('o'))"
    }

    $expectedComputer = [string]$Request.target.computer_name
    if ([string]::IsNullOrWhiteSpace($expectedComputer) -or
        -not [string]::Equals(
            $expectedComputer,
            $env:COMPUTERNAME,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
        throw "target hostname mismatch: expected '$expectedComputer', this host is '$env:COMPUTERNAME'"
    }
    if ([string]::IsNullOrWhiteSpace([string]$Request.target.peer_id)) {
        throw 'target peer_id is empty'
    }
    if ([string]$Request.bundle.manifest_sha256 -cnotmatch
        '^[0-9A-F]{64}$') {
        throw 'bundle manifest_sha256 is invalid'
    }
    $workerIdle = [int]$Request.worker_idle_seconds
    if ($workerIdle -lt 60 -or $workerIdle -gt 3600) {
        throw 'worker_idle_seconds must be between 60 and 3600'
    }
    if ($Request.worker_stop_after -isnot [bool]) {
        throw 'worker_stop_after must be boolean'
    }
    $motionDuration = [int]$Request.motion_duration_seconds
    if ($motionDuration -lt 3 -or $motionDuration -gt 900) {
        throw 'motion_duration_seconds must be between 3 and 900'
    }
}

function Get-FirewallStatus {
    param([Parameter(Mandatory = $true)][string]$Runtime)

    $script = Join-Path $Runtime 'configure-allmystuff-sandbox-firewall.ps1'
    if (-not (Test-Path -LiteralPath $script -PathType Leaf)) {
        return [pscustomobject]@{
            ready = $false
            rules = @()
            reason = 'firewall helper is not staged'
        }
    }
    $raw = & $script -Action Show -RuntimeDir $Runtime
    [object[]]$rules = (($raw | Out-String) | ConvertFrom-Json)
    $meshPath = Get-FullPath (Join-Path $Runtime 'myownmesh.exe')
    $readyRules = @($rules | Where-Object {
        $_.present -eq $true -and
        [string]$_.enabled -ceq 'True' -and
        [string]$_.direction -ceq 'Inbound' -and
        [string]$_.action -ceq 'Allow' -and
        [string]::Equals(
            [string]$_.program,
            $meshPath,
            [System.StringComparison]::OrdinalIgnoreCase
        )
    })
    return [pscustomobject]@{
        ready = ($rules.Count -eq 2 -and $readyRules.Count -eq 2)
        rules = $rules
        reason = if ($rules.Count -eq 2 -and $readyRules.Count -eq 2) {
            $null
        } else {
            'the two exact stable-path inbound rules are not active'
        }
    }
}

function Invoke-Runner {
    param(
        [Parameter(Mandatory = $true)][string]$Runner,
        [Parameter(Mandatory = $true)][hashtable]$Arguments
    )
    $output = & $Runner @Arguments 2>&1
    return (($output | Out-String).Trim())
}

function Get-ProcessStartUnixMilliseconds {
    param([Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process)
    return [DateTimeOffset]::new(
        $Process.StartTime.ToUniversalTime()
    ).ToUnixTimeMilliseconds()
}

function Start-SandboxRemoteWorker {
    param(
        [Parameter(Mandatory = $true)][string]$Runtime,
        [Parameter(Mandatory = $true)][string]$Stage,
        [Parameter(Mandatory = $true)][object]$Request
    )

    $worker = Get-FullPath (Join-Path $Runtime 'sandbox_remote_worker.exe')
    $launcher = Get-FullPath (
        Join-Path $Runtime 'sandbox_process_launcher.exe'
    )
    $recordPath = Join-Path $Runtime 'sandbox-remote-worker.json'
    foreach ($required in @($worker, $launcher)) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
            throw "sandbox worker input is missing: $required"
        }
    }

    if (Test-Path -LiteralPath $recordPath -PathType Leaf) {
        try {
            $existing = Get-Content -LiteralPath $recordPath -Raw |
                ConvertFrom-Json
            $process = Get-Process -Id ([int]$existing.pid) `
                -ErrorAction Stop
            $processPath = Get-FullPath $process.Path
            $started = Get-ProcessStartUnixMilliseconds -Process $process
            $recorded = [int64]$existing.started_unix_ms
            if ([string]$existing.kind -ceq
                    'allmystuff-sandbox-remote-worker' -and
                [string]$existing.manifest_sha256 -ceq
                    [string]$Request.bundle.manifest_sha256 -and
                [string]::Equals(
                    $processPath,
                    $worker,
                    [StringComparison]::OrdinalIgnoreCase
                ) -and
                [math]::Abs($started - $recorded) -le 5000) {
                return [ordered]@{
                    status = 'running'
                    pid = [int]$process.Id
                    started_unix_ms = $recorded
                    reused = $true
                    record = $recordPath
                }
            }
        } catch {
        }
        Remove-Item -LiteralPath $recordPath -Force
    }

    $idleSeconds = [int]$Request.worker_idle_seconds
    if ($idleSeconds -lt 60 -or $idleSeconds -gt 3600) {
        throw 'worker_idle_seconds must be between 60 and 3600'
    }
    $logDir = Join-Path $Runtime 'logs'
    New-Item -ItemType Directory -Path $logDir -Force | Out-Null
    $launchRaw = & $launcher `
        --cwd $Runtime `
        --stdout (Join-Path $logDir 'sandbox-remote-worker.stdout.log') `
        --stderr (Join-Path $logDir 'sandbox-remote-worker.stderr.log') `
        -- $worker `
        --runtime $Runtime `
        --stage-root (Split-Path -Parent $Stage) `
        --idle-seconds ([string]$idleSeconds) `
        --ignore-request-id ([string]$Request.request_id) `
        --manifest-sha256 ([string]$Request.bundle.manifest_sha256)
    if ($LASTEXITCODE -ne 0) {
        throw "sandbox worker launcher failed: $(($launchRaw | Out-String).Trim())"
    }
    $launched = (($launchRaw | Out-String).Trim()) | ConvertFrom-Json
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        if (Test-Path -LiteralPath $recordPath -PathType Leaf) {
            try {
                $record = Get-Content -LiteralPath $recordPath -Raw |
                    ConvertFrom-Json
                if ([int]$record.pid -eq [int]$launched.pid -and
                    [string]$record.kind -ceq
                        'allmystuff-sandbox-remote-worker' -and
                    [string]$record.manifest_sha256 -ceq
                        [string]$Request.bundle.manifest_sha256) {
                    return [ordered]@{
                        status = 'running'
                        pid = [int]$record.pid
                        started_unix_ms = [int64]$record.started_unix_ms
                        reused = $false
                        idle_seconds = [int]$record.idle_seconds
                        record = $recordPath
                    }
                }
            } catch {
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "sandbox worker did not publish its record within 15 seconds"
}

function Copy-IfPresent {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination
    )
    if (Test-Path -LiteralPath $Source) {
        Copy-Item -LiteralPath $Source -Destination $Destination -Recurse
    }
}

function New-ArtifactArchive {
    param(
        [Parameter(Mandatory = $true)][object]$Request,
        [Parameter(Mandatory = $true)][string]$State,
        [Parameter(Mandatory = $true)][string]$Runtime,
        [Parameter(Mandatory = $true)][string]$Stage
    )

    $base = Join-Path $env:LOCALAPPDATA 'AllMyStuffSandboxArtifacts'
    $collection = Join-Path $base (
        "$( [string]$Request.instance_id )\$( [string]$Request.request_id )"
    )
    if (Test-Path -LiteralPath $collection) {
        throw "artifact collection already exists: $collection"
    }
    New-Item -ItemType Directory -Path $collection -Force | Out-Null
    Copy-IfPresent -Source (Join-Path $State 'logs') `
        -Destination (Join-Path $collection 'logs')
    $nodeLog = Join-Path $State '.myownmesh\logs\node.log'
    if (Test-Path -LiteralPath $nodeLog -PathType Leaf) {
        $logCollection = Join-Path $collection 'logs'
        New-Item -ItemType Directory -Path $logCollection -Force | Out-Null
        Copy-Item -LiteralPath $nodeLog `
            -Destination (Join-Path $logCollection 'allmystuff-node.log')
    }
    foreach ($workerLog in @(
        (Join-Path $Runtime 'logs\sandbox-remote-worker.stdout.log'),
        (Join-Path $Runtime 'logs\sandbox-remote-worker.stderr.log')
    )) {
        if (Test-Path -LiteralPath $workerLog -PathType Leaf) {
            $logCollection = Join-Path $collection 'logs'
            New-Item -ItemType Directory -Path $logCollection -Force |
                Out-Null
            Copy-Item -LiteralPath $workerLog -Destination $logCollection
        }
    }
    Copy-IfPresent -Source (Join-Path $State 'artifacts') `
        -Destination (Join-Path $collection 'artifacts')
    Copy-IfPresent -Source (Join-Path $State 'sandbox-runtime.json') `
        -Destination (Join-Path $collection 'sandbox-runtime.json')
    Copy-IfPresent -Source (Join-Path $State 'sandbox-last-run.json') `
        -Destination (Join-Path $collection 'sandbox-last-run.json')
    Copy-IfPresent -Source (Join-Path $Runtime 'sandbox-bundle.json') `
        -Destination (Join-Path $collection 'sandbox-bundle.json')
    Copy-IfPresent -Source (Join-Path $Stage 'sandbox-remote-request.json') `
        -Destination (Join-Path $collection 'sandbox-remote-request.json')

    $files = foreach ($file in @(
        Get-ChildItem -LiteralPath $collection -File -Recurse |
            Sort-Object FullName
    )) {
        [ordered]@{
            path = $file.FullName.Substring($collection.Length + 1)
            size = [int64]$file.Length
            sha256 = Get-Sha256 $file.FullName
        }
    }
    $manifest = [ordered]@{
        schema = 1
        kind = 'allmystuff-sandbox-artifact-collection'
        created_utc = [DateTime]::UtcNow.ToString('o')
        request_id = [string]$Request.request_id
        run_id = [string]$Request.run_id
        instance_id = [string]$Request.instance_id
        host = $env:COMPUTERNAME
        source_commit = [string]$Request.bundle.source_commit
        files = @($files)
        excludes = @(
            'sandbox MyOwnMesh identity and secret state',
            'sandbox network config and credentials',
            'production AllMyStuff state'
        )
    }
    Write-Utf8NoBom -Path (Join-Path $collection 'collection-manifest.json') `
        -Text (($manifest | ConvertTo-Json -Depth 10) + [Environment]::NewLine)

    $outbox = Join-Path (Split-Path -Parent $Stage) 'outbox'
    New-Item -ItemType Directory -Path $outbox -Force | Out-Null
    $archiveName = "$($Request.instance_id)-$($Request.run_id)-$($Request.request_id).zip"
    $archive = Join-Path $outbox $archiveName
    if (Test-Path -LiteralPath $archive) {
        throw "artifact archive already exists: $archive"
    }
    Compress-Archive -Path (Join-Path $collection '*') `
        -DestinationPath $archive -CompressionLevel Optimal
    return [ordered]@{
        path = $archive
        remote_path = ".allmystuff-sandbox-stage\outbox\$archiveName"
        size = [int64](Get-Item -LiteralPath $archive).Length
        sha256 = Get-Sha256 $archive
        collection_manifest = Join-Path $collection 'collection-manifest.json'
    }
}

if ([string]::IsNullOrWhiteSpace($env:USERPROFILE) -or
    [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
    throw 'remote sandbox bootstrap requires USERPROFILE and LOCALAPPDATA'
}
$stage = Get-FullPath (Join-Path $env:USERPROFILE $stageRelative)
$requestPath = Join-Path $stage 'sandbox-remote-request.json'
if (-not (Test-Path -LiteralPath $requestPath -PathType Leaf)) {
    throw "sandbox remote request is missing: $requestPath"
}
$request = Get-Content -LiteralPath $requestPath -Raw | ConvertFrom-Json
Assert-Request -Request $request

$runtime = Get-FullPath (
    Join-Path $env:LOCALAPPDATA 'AllMyStuffSandboxRuntime'
)
$state = Get-FullPath (
    Join-Path (Join-Path $env:LOCALAPPDATA 'AllMyStuffSandbox') `
        ([string]$request.instance_id)
)
$runner = Join-Path $runtime 'allmystuff-sandbox.ps1'
$action = [string]$request.action
$isAdmin = Test-IsAdministrator
$stageResult = $null
$operation = $null

if ($action -in @('Stage', 'Start')) {
    $manifestPath = Join-Path $stage 'sandbox-bundle.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "staged bundle manifest is missing: $manifestPath"
    }
    $manifestHash = Get-Sha256 $manifestPath
    if ($manifestHash -cne [string]$request.bundle.manifest_sha256) {
        throw 'staged bundle manifest hash does not match the request'
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ([string]$manifest.source.commit -cne
        [string]$request.bundle.source_commit) {
        throw 'staged bundle source commit does not match the request'
    }
    $stageScript = Join-Path $stage 'stage-allmystuff-sandbox.ps1'
    $stageRaw = & $stageScript -BundleDir $stage -RuntimeDir $runtime
    $stageResult = (($stageRaw | Out-String).Trim()) | ConvertFrom-Json
}

if (-not (Test-Path -LiteralPath $runner -PathType Leaf)) {
    throw "sandbox runner is not staged: $runner"
}
$stableManifest = Get-Content -LiteralPath (
    Join-Path $runtime 'sandbox-bundle.json'
) -Raw | ConvertFrom-Json
$stableManifestPath = Join-Path $runtime 'sandbox-bundle.json'
if ((Get-Sha256 $stableManifestPath) -cne
    [string]$request.bundle.manifest_sha256) {
    throw 'stable sandbox runtime manifest does not match the request'
}
if ([string]$stableManifest.source.commit -cne
    [string]$request.bundle.source_commit) {
    throw 'stable sandbox runtime source commit does not match the request'
}

$firewall = Get-FirewallStatus -Runtime $runtime
if ($action -eq 'Start' -and -not $firewall.ready) {
    if ($isAdmin -and $request.install_firewall_if_elevated -eq $true) {
        $firewallScript = Join-Path $runtime `
            'configure-allmystuff-sandbox-firewall.ps1'
        & $firewallScript -Action Install -RuntimeDir $runtime `
            -Profile Private,Public -NoElevate | Out-Null
        $firewall = Get-FirewallStatus -Runtime $runtime
    }
    if (-not $firewall.ready) {
        $operation = [ordered]@{
            status = 'staged_firewall_required'
            started = $false
            reason = $firewall.reason
        }
    }
}

$baseRunner = @{
    InstanceId = [string]$request.instance_id
    BundleDir = $runtime
    StateRoot = $state
}
$worker = $null

if ($null -eq $operation) {
    switch ($action) {
        'Stage' {
            $operation = [ordered]@{
                status = 'staged'
                started = $false
            }
        }
        'Start' {
            $startArgs = $baseRunner.Clone()
            $startArgs.Action = 'Start'
            $startArgs.NetworkMode = [string]$request.network_mode
            $startArgs.AllowedNetworkId =
                [string[]]@($request.allowed_network_ids)
            $startArgs.ProtectedPort = [int[]]@($request.protected_ports)
            $startArgs.ProfileMode = [string]$request.profile.mode
            $startArgs.ProfileTraceEvents =
                [int]$request.profile.trace_events
            $startArgs.TelemetrySeconds =
                [int]$request.profile.telemetry_seconds
            $start = Invoke-Runner -Runner $runner -Arguments $startArgs

            $controlArgs = $baseRunner.Clone()
            $controlArgs.Action = 'Control'
            $controlArgs.ControlArguments = @('identity')
            $identity = Invoke-Runner -Runner $runner -Arguments $controlArgs
            $controlArgs.ControlArguments = @('specs')
            $specs = Invoke-Runner -Runner $runner -Arguments $controlArgs
            $probeArgs = $baseRunner.Clone()
            $probeArgs.Action = 'Probe'
            $probeArgs.ProbeArguments = @('--list')
            $videoList = Invoke-Runner -Runner $runner -Arguments $probeArgs

            $artifactRoot = Join-Path $state 'artifacts'
            New-Item -ItemType Directory -Path $artifactRoot -Force |
                Out-Null
            Write-Utf8NoBom -Path (Join-Path $artifactRoot 'identity.json') `
                -Text ($identity + [Environment]::NewLine)
            Write-Utf8NoBom -Path (Join-Path $artifactRoot 'host-specs.json') `
                -Text ($specs + [Environment]::NewLine)
            Write-Utf8NoBom -Path (Join-Path $artifactRoot 'video-list-start.json') `
                -Text ($videoList + [Environment]::NewLine)
            $operation = [ordered]@{
                status = 'running'
                start = $start | ConvertFrom-Json
                identity = $identity | ConvertFrom-Json
                specs = $specs | ConvertFrom-Json
                video = $videoList | ConvertFrom-Json
            }
        }
        'Status' {
            $args = $baseRunner.Clone()
            $args.Action = 'Status'
            $operation = Invoke-Runner -Runner $runner -Arguments $args |
                ConvertFrom-Json
        }
        'Control' {
            $args = $baseRunner.Clone()
            $args.Action = 'Control'
            $args.ControlArguments = [string[]]@($request.control_arguments)
            $control = Invoke-Runner -Runner $runner -Arguments $args
            $operation = $control | ConvertFrom-Json
        }
        'Probe' {
            $probeDir = Join-Path (Join-Path $state 'artifacts') 'probes'
            New-Item -ItemType Directory -Path $probeDir -Force | Out-Null
            $reportPath = Join-Path $probeDir "$($request.run_id).json"
            $stdoutPath = Join-Path $probeDir "$($request.run_id).stdout.txt"
            $probeArguments = [System.Collections.Generic.List[string]]::new()
            foreach ($argument in [string[]]@($request.probe_arguments)) {
                $probeArguments.Add($argument)
            }
            if (-not $probeArguments.Contains('--list') -and
                -not $probeArguments.Contains('--json-out')) {
                $probeArguments.Add('--json-out')
                $probeArguments.Add($reportPath)
            }
            if ($probeArguments.Contains('--motion-palette') -and
                -not $probeArguments.Contains('--motion-evidence-dir')) {
                $probeArguments.Add('--motion-evidence-dir')
                $probeArguments.Add(
                    (Join-Path $probeDir "$($request.run_id)-motion-evidence")
                )
            }
            $args = $baseRunner.Clone()
            $args.Action = 'Probe'
            $args.ProbeArguments = $probeArguments.ToArray()
            $probe = Invoke-Runner -Runner $runner -Arguments $args
            Write-Utf8NoBom -Path $stdoutPath `
                -Text ($probe + [Environment]::NewLine)
            $operation = [ordered]@{
                status = 'probe_complete'
                output = $probe
                report = if (Test-Path -LiteralPath $reportPath) {
                    Get-Content -LiteralPath $reportPath -Raw |
                        ConvertFrom-Json
                } else {
                    $null
                }
            }
        }
        'MotionStart' {
            $motion = Join-Path $runtime 'sandbox-motion-source.ps1'
            if (-not (Test-Path -LiteralPath $motion -PathType Leaf)) {
                throw "sandbox motion source is missing: $motion"
            }
            $motionRoot = Join-Path (Join-Path $state 'artifacts') 'motion'
            $raw = & $motion -Action Start -StateRoot $motionRoot `
                -RunId ([string]$request.run_id) `
                -DurationSeconds ([int]$request.motion_duration_seconds)
            $operation = (($raw | Out-String).Trim()) | ConvertFrom-Json
        }
        'MotionStatus' {
            $motion = Join-Path $runtime 'sandbox-motion-source.ps1'
            $motionRoot = Join-Path (Join-Path $state 'artifacts') 'motion'
            $raw = & $motion -Action Status -StateRoot $motionRoot `
                -RunId ([string]$request.run_id)
            $operation = (($raw | Out-String).Trim()) | ConvertFrom-Json
        }
        'MotionStop' {
            $motion = Join-Path $runtime 'sandbox-motion-source.ps1'
            $motionRoot = Join-Path (Join-Path $state 'artifacts') 'motion'
            $raw = & $motion -Action Stop -StateRoot $motionRoot `
                -RunId ([string]$request.run_id)
            $operation = (($raw | Out-String).Trim()) | ConvertFrom-Json
        }
        'Stop' {
            $args = $baseRunner.Clone()
            $args.Action = 'Stop'
            $operation = Invoke-Runner -Runner $runner -Arguments $args |
                ConvertFrom-Json
        }
        'Collect' {
            $operation = New-ArtifactArchive -Request $request -State $state `
                -Runtime $runtime -Stage $stage
        }
        default {
            throw "unsupported sandbox remote action: $action"
        }
    }
}

if ($action -in @('Stage', 'Start')) {
    $worker = Start-SandboxRemoteWorker -Runtime $runtime -Stage $stage `
        -Request $request
}

$response = [ordered]@{
    schema = 1
    kind = 'allmystuff-sandbox-remote-result'
    request_id = [string]$request.request_id
    run_id = [string]$request.run_id
    action = $action
    host = $env:COMPUTERNAME
    target_peer_id = [string]$request.target.peer_id
    instance_id = [string]$request.instance_id
    source_commit = [string]$request.bundle.source_commit
    is_administrator = $isAdmin
    firewall = $firewall
    stage = $stageResult
    worker = $worker
    operation = $operation
}
$resultOutbox = Join-Path (Split-Path -Parent $stage) 'outbox'
New-Item -ItemType Directory -Path $resultOutbox -Force | Out-Null
$resultName = "sandbox-remote-result-$($request.request_id).json"
$resultPath = Join-Path $resultOutbox $resultName
if (Test-Path -LiteralPath $resultPath) {
    throw "remote result already exists: $resultPath"
}
Write-Utf8NoBom -Path $resultPath -Text (
    ($response | ConvertTo-Json -Depth 24 -Compress) +
        [Environment]::NewLine
)
$resultItem = Get-Item -LiteralPath $resultPath
$resultRemotePath = ".allmystuff-sandbox-stage\outbox\$resultName"
Write-Output $resultBegin
Write-Output "request_id=$($request.request_id)"
Write-Output "host=$env:COMPUTERNAME"
Write-Output "target_peer_id=$($request.target.peer_id)"
Write-Output "result_remote_path=$resultRemotePath"
Write-Output "result_size=$([int64]$resultItem.Length)"
Write-Output "result_sha256=$(Get-Sha256 $resultPath)"
Write-Output $resultEnd
