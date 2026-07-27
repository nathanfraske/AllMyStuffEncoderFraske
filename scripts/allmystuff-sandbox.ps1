[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Start', 'Status', 'Probe', 'Control', 'Stop')]
    [string]$Action,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]*$')]
    [string]$InstanceId,

    [string]$BundleDir = $PSScriptRoot,

    [string]$StateRoot,

    [int[]]$ProtectedPort = @(),

    [string[]]$ProbeArguments = @('--list'),

    [string[]]$ControlArguments = @('identity'),

    [string]$LogFilter =
        'info,allmystuff_node=debug,allmystuff_serve=debug,myownmesh=debug',

    [ValidateSet('Isolated', 'LocalClaim', 'TestNetwork')]
    [string]$NetworkMode = 'Isolated',

    [string[]]$AllowedNetworkId = @(),

    [ValidateSet('Off', 'Summary', 'Trace')]
    [string]$ProfileMode = 'Trace',

    [ValidateRange(100, 1000000)]
    [int]$ProfileTraceEvents = 100000,

    [ValidateRange(1, 60)]
    [int]$TelemetrySeconds = 1,

    [ValidateRange(1, 300)]
    [int]$StartupTimeoutSeconds = 15,

    [ValidateRange(1, 120)]
    [int]$ShutdownTimeoutSeconds = 8
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

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Text
    )
    $encoding = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($Path, $Text, $encoding)
}

function Set-ObjectProperty {
    param(
        [Parameter(Mandatory = $true)][object]$Object,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][AllowNull()][object]$Value
    )
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value
    } else {
        $property.Value = $Value
    }
}

function Get-OrAddObjectProperty {
    param(
        [Parameter(Mandatory = $true)][object]$Object,
        [Parameter(Mandatory = $true)][string]$Name
    )
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property -or $null -eq $property.Value) {
        $value = [pscustomobject]@{}
        Set-ObjectProperty -Object $Object -Name $Name -Value $value
        return $value
    }
    return $property.Value
}

function Assert-Bundle {
    param([Parameter(Mandatory = $true)][string]$Path)
    $manifestPath = Join-Path $Path 'sandbox-bundle.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "sandbox manifest is missing: $manifestPath"
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.schema -ne 1 -or $manifest.kind -cne 'allmystuff-sandbox-bundle') {
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
        $actual = Get-Sha256 $filePath
        if ($actual -cne [string]$file.sha256) {
            throw "bundle hash mismatch for $name"
        }
        if ((Get-Item -LiteralPath $filePath).Length -ne [int64]$file.size) {
            throw "bundle size mismatch for $name"
        }
    }
    return $manifest
}

function Get-ProcessRecord {
    param([Parameter(Mandatory = $true)][int]$Id)
    $process = Get-Process -Id $Id -ErrorAction Stop
    return [ordered]@{
        pid = [int]$process.Id
        start_filetime_utc = [int64]$process.StartTime.ToUniversalTime().ToFileTimeUtc()
        path = [string]$process.Path
        sha256 = Get-Sha256 $process.Path
    }
}

function Assert-ProcessRecord {
    param(
        [Parameter(Mandatory = $true)][object]$Record,
        [Parameter(Mandatory = $true)][string]$Role
    )
    $current = Get-ProcessRecord -Id ([int]$Record.pid)
    if ($current.start_filetime_utc -ne [int64]$Record.start_filetime_utc -or
        $current.path -cne [string]$Record.path -or
        $current.sha256 -cne [string]$Record.sha256) {
        throw "$Role process identity changed; refusing to act on PID $($Record.pid)"
    }
    return $current
}

function Get-BaselineProcesses {
    param([string[]]$ExcludedPath = @())

    $excluded = @($ExcludedPath | ForEach-Object { Get-FullPath $_ })
    $names = @('allmystuff-gui', 'allmystuff-serve', 'myownmesh', 'allmyagents-desktop')
    $records = foreach ($name in $names) {
        foreach ($process in @(Get-Process -Name $name -ErrorAction SilentlyContinue)) {
            try {
                $processPath = [string]$process.Path
                $isExcluded = @($excluded | Where-Object {
                    [string]::Equals(
                        $_,
                        $processPath,
                        [System.StringComparison]::OrdinalIgnoreCase
                    )
                }).Count -ne 0
                if ($isExcluded) {
                    continue
                }
                [ordered]@{
                    name = $process.ProcessName
                    pid = [int]$process.Id
                    start_filetime_utc = [int64]$process.StartTime.ToUniversalTime().ToFileTimeUtc()
                    path = $processPath
                }
            } catch {
                throw "could not record protected process $name PID $($process.Id): $($_.Exception.Message)"
            }
        }
    }
    return @($records)
}

function Assert-BaselineProcesses {
    param([object[]]$Expected)
    foreach ($record in @($Expected)) {
        $process = Get-Process -Id ([int]$record.pid) -ErrorAction SilentlyContinue
        if ($null -eq $process) {
            throw "protected process exited: $($record.name) PID $($record.pid)"
        }
        $start = [int64]$process.StartTime.ToUniversalTime().ToFileTimeUtc()
        if ($start -ne [int64]$record.start_filetime_utc -or
            [string]$process.Path -cne [string]$record.path) {
            throw "protected process identity changed: $($record.name) PID $($record.pid)"
        }
    }
}

function Get-ListenerSnapshot {
    param([int[]]$Ports)
    if (@($Ports).Count -eq 0) {
        return @()
    }
    if (-not $script:IsWindows) {
        throw 'protected-port snapshots are currently implemented only on Windows'
    }
    $records = foreach ($port in @($Ports | Sort-Object -Unique)) {
        $connections = @(Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue)
        if ($connections.Count -eq 0) {
            throw "protected port $port is not listening"
        }
        foreach ($connection in $connections) {
            $process = Get-Process -Id ([int]$connection.OwningProcess) -ErrorAction Stop
            [pscustomobject][ordered]@{
                address = [string]$connection.LocalAddress
                port = [int]$connection.LocalPort
                pid = [int]$connection.OwningProcess
                process = [string]$process.ProcessName
                start_filetime_utc = [int64]$process.StartTime.ToUniversalTime().ToFileTimeUtc()
                path = [string]$process.Path
            }
        }
    }
    return @($records | Sort-Object port, address, pid)
}

function Assert-ListenerSnapshot {
    param([object[]]$Expected)
    if (@($Expected).Count -eq 0) {
        return
    }
    $ports = @($Expected | ForEach-Object { [int]$_.port } | Sort-Object -Unique)
    $current = @(Get-ListenerSnapshot -Ports $ports)
    $expectedJson = @($Expected | Sort-Object port, address, pid) |
        ConvertTo-Json -Depth 6 -Compress
    $currentJson = @($current) | ConvertTo-Json -Depth 6 -Compress
    if ($expectedJson -cne $currentJson) {
        throw "protected listener ownership changed: expected $expectedJson, got $currentJson"
    }
}

function Invoke-WithEnvironment {
    param(
        [Parameter(Mandatory = $true)][hashtable]$Environment,
        [Parameter(Mandatory = $true)][scriptblock]$Script
    )
    $saved = @{}
    foreach ($name in $Environment.Keys) {
        $saved[$name] = [System.Environment]::GetEnvironmentVariable(
            $name,
            [System.EnvironmentVariableTarget]::Process
        )
        [System.Environment]::SetEnvironmentVariable(
            $name,
            [string]$Environment[$name],
            [System.EnvironmentVariableTarget]::Process
        )
    }
    try {
        return & $Script
    } finally {
        foreach ($name in $Environment.Keys) {
            [System.Environment]::SetEnvironmentVariable(
                $name,
                $saved[$name],
                [System.EnvironmentVariableTarget]::Process
            )
        }
    }
}

function Invoke-Probe {
    param(
        [Parameter(Mandatory = $true)][string]$ProbePath,
        [Parameter(Mandatory = $true)][hashtable]$Environment,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )
    return Invoke-WithEnvironment -Environment $Environment -Script {
        $output = & $ProbePath @Arguments 2>&1
        [pscustomobject]@{
            exit_code = [int]$LASTEXITCODE
            output = (($output | Out-String).Trim())
        }
    }
}

function Set-SandboxMeshConfig {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$ControlSocket,
        [Parameter(Mandatory = $true)][string]$NetworkMode,
        [string[]]$AllowedNetworkId = @()
    )
    New-Item -ItemType Directory -Force -Path $Root | Out-Null
    $path = Join-Path $Root 'config.json'
    if (Test-Path -LiteralPath $path) {
        $config = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    } else {
        $config = [pscustomobject]@{}
    }
    $autoUpdate = Get-OrAddObjectProperty -Object $config -Name 'auto_update'
    Set-ObjectProperty -Object $autoUpdate -Name 'enabled' -Value $false
    $daemon = Get-OrAddObjectProperty -Object $config -Name 'daemon'
    Set-ObjectProperty -Object $daemon -Name 'enabled' -Value $true
    Set-ObjectProperty -Object $daemon -Name 'control_socket' -Value $ControlSocket

    $localClaimId = 'allmystuff-local-claim-v1'
    $networksProperty = $config.PSObject.Properties['networks']
    $configuredNetworks = @(
        if ($null -ne $networksProperty) {
            $networksProperty.Value
        }
    )
    $disabledDir = Join-Path $Root '.myownmesh'
    New-Item -ItemType Directory -Force -Path $disabledDir | Out-Null
    $disabledPath = Join-Path $disabledDir 'allmystuff-networks.json'
    if (Test-Path -LiteralPath $disabledPath) {
        $disabledStore = Get-Content -LiteralPath $disabledPath -Raw | ConvertFrom-Json
    } else {
        $disabledStore = [pscustomobject]@{ disabled = @() }
    }
    $disabledProperty = $disabledStore.PSObject.Properties['disabled']
    $disabledNetworks = @(
        if ($null -ne $disabledProperty) {
            $disabledProperty.Value
        }
    )

    $allNetworks = [ordered]@{}
    foreach ($network in @($disabledNetworks) + @($configuredNetworks)) {
        $networkId = [string]$network.network_id
        if ([string]::IsNullOrWhiteSpace($networkId)) {
            $networkId = [string]$network.id
        }
        if ([string]::IsNullOrWhiteSpace($networkId)) {
            throw 'sandbox network config has no id or network_id'
        }
        $allNetworks[$networkId.ToLowerInvariant()] = $network
    }
    if (-not $allNetworks.Contains($localClaimId)) {
        $allNetworks[$localClaimId] = [pscustomobject][ordered]@{
            id = $localClaimId
            network_id = $localClaimId
            label = 'Local claiming (this LAN)'
            kind = 'open'
            auto_approve = $true
            signaling = [pscustomobject][ordered]@{
                strategy = 'none'
                mdns = $true
            }
            stun_servers = @()
            turn_servers = @()
        }
    }

    $wanted = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    switch ($NetworkMode) {
        'Isolated' {
            if (@($AllowedNetworkId).Count -ne 0) {
                throw 'Isolated mode cannot allow a network'
            }
        }
        'LocalClaim' {
            if (@($AllowedNetworkId).Count -ne 0) {
                throw 'LocalClaim mode cannot allow an additional network'
            }
            [void]$wanted.Add($localClaimId)
        }
        'TestNetwork' {
            if (@($AllowedNetworkId).Count -eq 0) {
                throw 'TestNetwork mode requires at least one AllowedNetworkId'
            }
            foreach ($networkId in @($AllowedNetworkId)) {
                if ([string]::IsNullOrWhiteSpace($networkId) -or
                    $networkId -ceq $localClaimId) {
                    throw "invalid test-network allow entry: '$networkId'"
                }
                $key = $networkId.ToLowerInvariant()
                if (-not $allNetworks.Contains($key)) {
                    throw "allowed test network is not saved in sandbox state: $networkId"
                }
                [void]$wanted.Add($key)
            }
        }
    }

    $enabledOut = @()
    $disabledOut = @()
    foreach ($entry in $allNetworks.GetEnumerator()) {
        if ($wanted.Contains([string]$entry.Key)) {
            $enabledOut += $entry.Value
        } else {
            $disabledOut += $entry.Value
        }
    }
    Set-ObjectProperty -Object $config -Name 'networks' `
        -Value ([object[]]@($enabledOut))
    Set-ObjectProperty -Object $disabledStore -Name 'disabled' `
        -Value ([object[]]@($disabledOut))

    $json = $config | ConvertTo-Json -Depth 32
    $temp = "$path.tmp-$PID"
    Write-Utf8NoBom -Path $temp -Text ($json + [Environment]::NewLine)
    Move-Item -LiteralPath $temp -Destination $path -Force

    $disabledJson = $disabledStore | ConvertTo-Json -Depth 32
    $disabledTemp = "$disabledPath.tmp-$PID"
    Write-Utf8NoBom -Path $disabledTemp -Text (
        $disabledJson + [Environment]::NewLine
    )
    Move-Item -LiteralPath $disabledTemp -Destination $disabledPath -Force
}

function Stop-ExactProcess {
    param(
        [Parameter(Mandatory = $true)][object]$Record,
        [Parameter(Mandatory = $true)][string]$Role,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )
    $process = Get-Process -Id ([int]$Record.pid) -ErrorAction SilentlyContinue
    if ($null -eq $process) {
        return
    }
    [void](Assert-ProcessRecord -Record $Record -Role $Role)
    Stop-Process -Id ([int]$Record.pid) -ErrorAction Stop
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ($null -ne (Get-Process -Id ([int]$Record.pid) -ErrorAction SilentlyContinue)) {
        if ([DateTime]::UtcNow -ge $deadline) {
            throw "$Role process did not exit within $TimeoutSeconds seconds"
        }
        Start-Sleep -Milliseconds 200
    }
}

$script:IsWindows = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
$bundle = Get-FullPath $BundleDir
$bundleManifest = Assert-Bundle -Path $bundle
$suffix = if ($script:IsWindows) { '.exe' } else { '' }
$servePath = Join-Path $bundle "allmystuff-serve$suffix"
$probePath = Join-Path $bundle "video_prod_probe$suffix"
$controlPath = Join-Path $bundle "sandbox_node_control$suffix"
$launcherPath = Join-Path $bundle "sandbox_process_launcher$suffix"
$meshPath = Join-Path $bundle "myownmesh$suffix"

if ([string]::IsNullOrWhiteSpace($StateRoot)) {
    $base = if ($script:IsWindows) {
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
            throw 'LOCALAPPDATA is not set; pass -StateRoot explicitly'
        }
        Join-Path $env:LOCALAPPDATA 'AllMyStuffSandbox'
    } else {
        Join-Path ([System.IO.Path]::GetTempPath()) 'allmystuff-sandbox'
    }
    $StateRoot = Join-Path $base $InstanceId
}
$state = Get-FullPath $StateRoot

if ($script:IsWindows -and -not [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
    $productionRoot = Get-FullPath (Join-Path $env:LOCALAPPDATA 'AllMyStuff')
    if ($state.StartsWith($productionRoot + [System.IO.Path]::DirectorySeparatorChar,
            [System.StringComparison]::OrdinalIgnoreCase) -or
        $state -ceq $productionRoot) {
        throw "sandbox state cannot live under the production install: $productionRoot"
    }
}
if ($state -ceq $bundle) {
    throw 'sandbox state root cannot equal the sealed bundle directory'
}

$meshSocket = if ($script:IsWindows) {
    "myownmesh-sandbox-$InstanceId"
} else {
    Join-Path $state 'myownmesh-daemon.sock'
}
$nodeSocket = if ($script:IsWindows) {
    "allmystuff-node-sandbox-$InstanceId"
} else {
    Join-Path $state 'allmystuff-node.sock'
}
$meshConfigSocket = if ($script:IsWindows) {
    "\\.\pipe\$meshSocket"
} else {
    $meshSocket
}

$environment = @{
    MYOWNMESH_HOME = $state
    MYOWNMESH_BIN = $meshPath
    ALLMYSTUFF_HOME = (Join-Path $state 'allmystuff')
    ALLMYSTUFF_MESH_SOCKET = $meshSocket
    ALLMYSTUFF_NODE_SOCKET = $nodeSocket
    ALLMYSTUFF_AUTOUPDATE = '0'
    ALLMYSTUFF_LOG = $LogFilter
    ALLMYSTUFF_CWD_LOG = '0'
    ALLMYSTUFF_VIDEO_STATS = '1'
    ALLMYSTUFF_H264_DECODER = ''
    ALLMYSTUFF_VIDEO_ENCODE_ADAPTER = ''
    ALLMYSTUFF_TELEMETRY =
        $(if ($ProfileMode -ceq 'Off') { '0' } else { '1' })
    ALLMYSTUFF_TELEMETRY_SECS = [string]$TelemetrySeconds
    ALLMYSTUFF_VIDEO_PROFILE_TRACE = '0'
    ALLMYSTUFF_VIDEO_PROFILE_TRACE_EVENTS = [string]$ProfileTraceEvents
    ALLMYSTUFF_VIDEO_PROFILE_INTERVAL_MS = '5000'
    ALLMYSTUFF_VIDEO_PROFILE_MAX_SERIES = '256'
    RUST_LOG = $LogFilter
}
$artifactRoot = Join-Path $state 'artifacts'
$tracePath = Join-Path $artifactRoot 'video-profile.jsonl'
switch ($ProfileMode) {
    'Off' {
        $environment.ALLMYSTUFF_VIDEO_PROFILE = '0'
    }
    'Summary' {
        $environment.ALLMYSTUFF_VIDEO_PROFILE = '1'
    }
    'Trace' {
        $environment.ALLMYSTUFF_VIDEO_PROFILE = '1'
        $environment.ALLMYSTUFF_VIDEO_PROFILE_TRACE = $tracePath
    }
}
$runtimePath = Join-Path $state 'sandbox-runtime.json'
$lastRunPath = Join-Path $state 'sandbox-last-run.json'

switch ($Action) {
    'Start' {
        if (Test-Path -LiteralPath $runtimePath) {
            $existing = Get-Content -LiteralPath $runtimePath -Raw | ConvertFrom-Json
            $live = Get-Process -Id ([int]$existing.node.pid) -ErrorAction SilentlyContinue
            if ($null -ne $live) {
                [void](Assert-ProcessRecord -Record $existing.node -Role 'sandbox node')
                throw "sandbox instance '$InstanceId' is already running"
            }
            Move-Item -LiteralPath $runtimePath -Destination $lastRunPath -Force
        }

        Set-SandboxMeshConfig -Root $state -ControlSocket $meshConfigSocket `
            -NetworkMode $NetworkMode -AllowedNetworkId $AllowedNetworkId
        $protectedProcesses = @(
            Get-BaselineProcesses -ExcludedPath @($servePath, $meshPath)
        )
        $protectedListeners = @(Get-ListenerSnapshot -Ports $ProtectedPort)
        $logs = Join-Path $state 'logs'
        New-Item -ItemType Directory -Force -Path $logs | Out-Null
        New-Item -ItemType Directory -Force -Path $artifactRoot | Out-Null
        $stdout = Join-Path $logs 'sandbox-node.stdout.log'
        $stderr = Join-Path $logs 'sandbox-node.stderr.log'

        if (-not (Test-Path -LiteralPath $launcherPath -PathType Leaf)) {
            throw "sealed sandbox process launcher is missing: $launcherPath"
        }
        $launch = Invoke-WithEnvironment -Environment $environment -Script {
            $output = & $launcherPath --cwd $bundle --stdout $stdout `
                --stderr $stderr -- $servePath --log $LogFilter 2>&1
            if ($LASTEXITCODE -ne 0) {
                throw "sandbox process launcher failed: $($output | Out-String)"
            }
            return (($output | Out-String).Trim()) | ConvertFrom-Json
        }
        $nodePid = [int]$launch.pid
        if ($nodePid -le 0) {
            throw 'sandbox process launcher returned an invalid PID'
        }
        $nodeRecord = Get-ProcessRecord -Id $nodePid
        $runtime = [ordered]@{
            schema = 1
            kind = 'allmystuff-sandbox-runtime'
            instance_id = $InstanceId
            started_utc = [DateTime]::UtcNow.ToString('o')
            bundle = $bundle
            bundle_source_commit = [string]$bundleManifest.source.commit
            state_root = $state
            mesh_socket = $meshSocket
            network_mode = $NetworkMode
            allowed_network_ids = @($AllowedNetworkId)
            node_socket = $nodeSocket
            node = $nodeRecord
            mesh = $null
            protected_processes = $protectedProcesses
            protected_listeners = $protectedListeners
            profile = [ordered]@{
                mode = $ProfileMode
                trace_path = if ($ProfileMode -ceq 'Trace') {
                    $tracePath
                } else {
                    $null
                }
                trace_events = if ($ProfileMode -ceq 'Trace') {
                    $ProfileTraceEvents
                } else {
                    0
                }
                telemetry_seconds = $TelemetrySeconds
                log_filter = $LogFilter
            }
            startup_timeout_seconds = $StartupTimeoutSeconds
            shutdown_timeout_seconds = $ShutdownTimeoutSeconds
        }
        Write-Utf8NoBom -Path $runtimePath -Text (
            ($runtime | ConvertTo-Json -Depth 10) + [Environment]::NewLine
        )

        try {
            $deadline = [DateTime]::UtcNow.AddSeconds($StartupTimeoutSeconds)
            $probe = $null
            do {
                if ($null -eq (Get-Process -Id $nodePid -ErrorAction SilentlyContinue)) {
                    throw "sandbox node exited during startup; inspect $stderr"
                }
                $probe = Invoke-Probe -ProbePath $probePath -Environment $environment -Arguments @('--list')
                if ($probe.exit_code -eq 0) {
                    break
                }
                Start-Sleep -Milliseconds 200
            } while ([DateTime]::UtcNow -lt $deadline)
            if ($null -eq $probe -or $probe.exit_code -ne 0) {
                throw "sandbox node did not pass the external probe within $StartupTimeoutSeconds seconds"
            }

            if ($script:IsWindows) {
                $meshChild = Get-CimInstance Win32_Process |
                    Where-Object {
                        $_.ParentProcessId -eq $nodePid -and
                        $_.Name -ieq "myownmesh$suffix"
                    } |
                    Select-Object -First 1
                if ($null -ne $meshChild) {
                    $runtime.mesh = Get-ProcessRecord -Id ([int]$meshChild.ProcessId)
                    Write-Utf8NoBom -Path $runtimePath -Text (
                        ($runtime | ConvertTo-Json -Depth 10) + [Environment]::NewLine
                    )
                }
            }
            Assert-BaselineProcesses -Expected $protectedProcesses
            Assert-ListenerSnapshot -Expected $protectedListeners
            [ordered]@{
                status = 'running'
                instance_id = $InstanceId
                state_root = $state
                node = $runtime.node
                mesh = $runtime.mesh
                node_socket = $nodeSocket
                mesh_socket = $meshSocket
                network_mode = $NetworkMode
                allowed_network_ids = @($AllowedNetworkId)
                profile = $runtime.profile
                probe = $probe
            } | ConvertTo-Json -Depth 10
        } catch {
            try {
                Stop-ExactProcess -Record $nodeRecord -Role 'sandbox node' `
                    -TimeoutSeconds $ShutdownTimeoutSeconds
                if ($null -ne $runtime.mesh) {
                    Stop-ExactProcess -Record $runtime.mesh -Role 'sandbox mesh' `
                        -TimeoutSeconds $ShutdownTimeoutSeconds
                }
            } finally {
                Assert-BaselineProcesses -Expected $protectedProcesses
                Assert-ListenerSnapshot -Expected $protectedListeners
            }
            throw
        }
    }

    'Status' {
        if (-not (Test-Path -LiteralPath $runtimePath -PathType Leaf)) {
            throw "sandbox runtime record is missing: $runtimePath"
        }
        $runtime = Get-Content -LiteralPath $runtimePath -Raw | ConvertFrom-Json
        [void](Assert-ProcessRecord -Record $runtime.node -Role 'sandbox node')
        if ($null -ne $runtime.mesh) {
            [void](Assert-ProcessRecord -Record $runtime.mesh -Role 'sandbox mesh')
        }
        Assert-BaselineProcesses -Expected @($runtime.protected_processes)
        Assert-ListenerSnapshot -Expected @($runtime.protected_listeners)
        $probe = Invoke-Probe -ProbePath $probePath -Environment $environment -Arguments @('--list')
        if ($probe.exit_code -ne 0) {
            throw "sandbox external probe failed: $($probe.output)"
        }
        [ordered]@{
            status = 'running'
            instance_id = $InstanceId
            state_root = $state
            node = $runtime.node
            mesh = $runtime.mesh
            node_socket = $runtime.node_socket
            mesh_socket = $runtime.mesh_socket
            network_mode = $runtime.network_mode
            allowed_network_ids = @($runtime.allowed_network_ids)
            profile = $runtime.profile
            probe = $probe
        } | ConvertTo-Json -Depth 10
    }

    'Probe' {
        if (-not (Test-Path -LiteralPath $runtimePath -PathType Leaf)) {
            throw "sandbox runtime record is missing: $runtimePath"
        }
        $runtime = Get-Content -LiteralPath $runtimePath -Raw | ConvertFrom-Json
        [void](Assert-ProcessRecord -Record $runtime.node -Role 'sandbox node')
        Assert-BaselineProcesses -Expected @($runtime.protected_processes)
        Assert-ListenerSnapshot -Expected @($runtime.protected_listeners)
        $probe = Invoke-Probe -ProbePath $probePath -Environment $environment `
            -Arguments $ProbeArguments
        if ($probe.exit_code -ne 0) {
            throw "sandbox probe failed with exit code $($probe.exit_code): $($probe.output)"
        }
        $probe.output
    }

    'Control' {
        if (-not (Test-Path -LiteralPath $runtimePath -PathType Leaf)) {
            throw "sandbox runtime record is missing: $runtimePath"
        }
        $runtime = Get-Content -LiteralPath $runtimePath -Raw | ConvertFrom-Json
        [void](Assert-ProcessRecord -Record $runtime.node -Role 'sandbox node')
        Assert-BaselineProcesses -Expected @($runtime.protected_processes)
        Assert-ListenerSnapshot -Expected @($runtime.protected_listeners)
        $control = Invoke-Probe -ProbePath $controlPath -Environment $environment `
            -Arguments $ControlArguments
        if ($control.exit_code -ne 0) {
            throw "sandbox control failed with exit code $($control.exit_code): $($control.output)"
        }
        $controlCommand = [string]@($ControlArguments)[0]
        if ($controlCommand -ceq 'join') {
            $networkId = [string]@($ControlArguments)[1]
            Set-ObjectProperty -Object $runtime -Name 'network_mode' `
                -Value 'TestNetwork'
            Set-ObjectProperty -Object $runtime -Name 'allowed_network_ids' `
                -Value ([object[]]@($networkId))
            Write-Utf8NoBom -Path $runtimePath -Text (
                ($runtime | ConvertTo-Json -Depth 12) +
                    [Environment]::NewLine
            )
        } elseif ($controlCommand -ceq 'leave') {
            Set-ObjectProperty -Object $runtime -Name 'network_mode' `
                -Value 'Isolated'
            Set-ObjectProperty -Object $runtime -Name 'allowed_network_ids' `
                -Value ([object[]]@())
            Write-Utf8NoBom -Path $runtimePath -Text (
                ($runtime | ConvertTo-Json -Depth 12) +
                    [Environment]::NewLine
            )
        }
        $control.output
    }

    'Stop' {
        if (-not (Test-Path -LiteralPath $runtimePath -PathType Leaf)) {
            throw "sandbox runtime record is missing: $runtimePath"
        }
        $runtime = Get-Content -LiteralPath $runtimePath -Raw | ConvertFrom-Json
        if ([string]$runtime.profile.mode -ceq 'Trace') {
            # The bounded trace writer flushes at least every 250 ms. With no
            # live probe route, two intervals let its final queued batch reach
            # disk before the exact process termination below.
            Start-Sleep -Milliseconds 500
        }
        Stop-ExactProcess -Record $runtime.node -Role 'sandbox node' `
            -TimeoutSeconds $ShutdownTimeoutSeconds
        if ($null -ne $runtime.mesh) {
            Stop-ExactProcess -Record $runtime.mesh -Role 'sandbox mesh' `
                -TimeoutSeconds $ShutdownTimeoutSeconds
        }
        Assert-BaselineProcesses -Expected @($runtime.protected_processes)
        Assert-ListenerSnapshot -Expected @($runtime.protected_listeners)
        Set-ObjectProperty -Object $runtime -Name 'stopped_utc' `
            -Value ([DateTime]::UtcNow.ToString('o'))
        Move-Item -LiteralPath $runtimePath -Destination $lastRunPath -Force
        Write-Utf8NoBom -Path $lastRunPath -Text (
            ($runtime | ConvertTo-Json -Depth 10) + [Environment]::NewLine
        )
        [ordered]@{
            status = 'stopped'
            instance_id = $InstanceId
            state_root = $state
            artifact_root = $artifactRoot
            protected_processes_unchanged = $true
            protected_listeners_unchanged = $true
        } | ConvertTo-Json -Depth 6
    }
}
