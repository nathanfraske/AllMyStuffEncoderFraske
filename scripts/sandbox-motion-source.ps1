[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Start', 'Status', 'Stop')]
    [string]$Action,

    [Parameter(Mandatory = $true)]
    [string]$StateRoot,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]{0,63}$')]
    [string]$RunId,

    [ValidateRange(3, 900)]
    [int]$DurationSeconds = 300
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

function Write-JsonAtomic {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][object]$Value
    )
    $temp = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        Write-Utf8NoBom -Path $temp -Text (
            ($Value | ConvertTo-Json -Depth 12 -Compress) +
                [Environment]::NewLine
        )
        Move-Item -LiteralPath $temp -Destination $Path -Force
    } finally {
        if (Test-Path -LiteralPath $temp) {
            Remove-Item -LiteralPath $temp -Force
        }
    }
}

function Get-ProcessStartUnixMilliseconds {
    param([Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process)
    return [DateTimeOffset]::new(
        $Process.StartTime.ToUniversalTime()
    ).ToUnixTimeMilliseconds()
}

function Get-ExactMotionProcess {
    param([Parameter(Mandatory = $true)][object]$Record)

    try {
        $process = Get-Process -Id ([int]$Record.pid) -ErrorAction Stop
        $actualPath = Get-FullPath $process.Path
        $expectedPath = Get-FullPath ([string]$Record.program)
        $actualStart = Get-ProcessStartUnixMilliseconds -Process $process
        if (-not [string]::Equals(
                $actualPath,
                $expectedPath,
                [StringComparison]::OrdinalIgnoreCase
            ) -or
            [math]::Abs(
                $actualStart - [int64]$Record.started_unix_ms
            ) -gt 5000) {
            return $null
        }
        return $process
    } catch {
        return $null
    }
}

function Read-JsonIfPresent {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $null
    }
    try {
        return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    } catch {
        return $null
    }
}

function Get-MotionState {
    param(
        [Parameter(Mandatory = $true)][string]$RecordPath,
        [Parameter(Mandatory = $true)][string]$ExpectedRunId
    )

    $record = Read-JsonIfPresent -Path $RecordPath
    if ($null -eq $record) {
        return [pscustomobject][ordered]@{
            status = 'not_running'
            running = $false
            run_id = $ExpectedRunId
            record = $null
            source_status = $null
            done = $null
            child_exit = $null
        }
    }
    if ([string]$record.kind -cne 'allmystuff-sandbox-motion-source' -or
        [string]$record.run_id -cne $ExpectedRunId) {
        throw 'motion source record does not match this run'
    }
    $process = Get-ExactMotionProcess -Record $record
    return [pscustomobject][ordered]@{
        status = if ($null -ne $process) { 'running' } else { 'exited' }
        running = $null -ne $process
        run_id = [string]$record.run_id
        record = $record
        source_status = Read-JsonIfPresent -Path ([string]$record.status_path)
        done = Read-JsonIfPresent -Path ([string]$record.done_path)
        child_exit = Read-JsonIfPresent -Path ([string]$record.exit_path)
    }
}

$state = Get-FullPath $StateRoot
New-Item -ItemType Directory -Path $state -Force | Out-Null
$recordPath = Join-Path $state 'motion-source.json'

if ($Action -ceq 'Status') {
    Get-MotionState -RecordPath $recordPath -ExpectedRunId $RunId |
        ConvertTo-Json -Depth 12 -Compress
    exit 0
}

if ($Action -ceq 'Stop') {
    $motion = Get-MotionState -RecordPath $recordPath -ExpectedRunId $RunId
    if ($null -eq $motion.record) {
        $motion | ConvertTo-Json -Depth 12 -Compress
        exit 0
    }

    $forced = $false
    $process = Get-ExactMotionProcess -Record $motion.record
    if ($null -ne $process) {
        Write-JsonAtomic -Path ([string]$motion.record.stop_path) -Value (
            [ordered]@{
                schema = 1
                kind = 'allmystuff-sandbox-motion-stop'
                run_id = $RunId
                created_utc = [DateTime]::UtcNow.ToString('o')
            }
        )
        $deadline = [DateTime]::UtcNow.AddSeconds(10)
        do {
            Start-Sleep -Milliseconds 100
            $process.Refresh()
        } while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline)
        if (-not $process.HasExited) {
            $process.Kill()
            $process.WaitForExit(5000)
            $forced = $true
        }
    }

    $final = [pscustomobject][ordered]@{
        status = 'stopped'
        running = $false
        run_id = $RunId
        forced = $forced
        record = $motion.record
        source_status = Read-JsonIfPresent -Path (
            [string]$motion.record.status_path
        )
        done = Read-JsonIfPresent -Path ([string]$motion.record.done_path)
        child_exit = Read-JsonIfPresent -Path ([string]$motion.record.exit_path)
    }
    Remove-Item -LiteralPath $recordPath -Force
    $final | ConvertTo-Json -Depth 12 -Compress
    exit 0
}

if (Test-Path -LiteralPath $recordPath -PathType Leaf) {
    $existing = Get-MotionState -RecordPath $recordPath -ExpectedRunId $RunId
    if ($existing.running) {
        throw "motion source is already running as PID $($existing.record.pid)"
    }
    Remove-Item -LiteralPath $recordPath -Force
}

$runDirectory = Join-Path $state $RunId
if (Test-Path -LiteralPath $runDirectory) {
    throw "motion run already exists: $runDirectory"
}
New-Item -ItemType Directory -Path $runDirectory | Out-Null

$statusPath = Join-Path $runDirectory 'status.json'
$goPath = Join-Path $runDirectory 'GO.json'
$stopPath = Join-Path $runDirectory 'STOP.json'
$donePath = Join-Path $runDirectory 'DONE.json'
$exitPath = Join-Path $runDirectory 'child-exit.json'
$errorPath = Join-Path $runDirectory 'child-error.txt'
$stdoutPath = Join-Path $runDirectory 'child.stdout.log'
$stderrPath = Join-Path $runDirectory 'child.stderr.log'
$program = Get-FullPath (
    Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
)

$child = @'
$ErrorActionPreference = 'Stop'
try {
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies @(
    'System.dll',
    'System.Drawing.dll',
    'System.Windows.Forms.dll'
) -TypeDefinition @"
using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Text;
using System.Windows.Forms;

public sealed class AllMyStuffSandboxMotionForm : Form
{
    private readonly Timer timer;
    private readonly Stopwatch clock = new Stopwatch();
    private readonly string statusPath;
    private readonly string goPath;
    private readonly string stopPath;
    private readonly string donePath;
    private readonly string runId;
    private readonly int durationSeconds;
    private Bitmap background;
    private int frame;
    private int paintCount;
    private int initialX;
    private bool validated;
    private bool started;
    private Rectangle oldOrange;
    private Rectangle oldPurple;

    public AllMyStuffSandboxMotionForm(
        int durationSeconds,
        string statusPath,
        string goPath,
        string stopPath,
        string donePath,
        string runId)
    {
        this.durationSeconds = durationSeconds;
        this.statusPath = statusPath;
        this.goPath = goPath;
        this.stopPath = stopPath;
        this.donePath = donePath;
        this.runId = runId;
        Text = "AllMyStuff sandbox motion test";
        FormBorderStyle = FormBorderStyle.None;
        StartPosition = FormStartPosition.Manual;
        Bounds = Screen.PrimaryScreen.Bounds;
        TopMost = true;
        SetStyle(
            ControlStyles.UserPaint |
            ControlStyles.AllPaintingInWmPaint |
            ControlStyles.OptimizedDoubleBuffer,
            true);
        UpdateStyles();

        timer = new Timer();
        timer.Interval = 8;
        timer.Tick += OnTick;
        Shown += OnShown;
        FormClosed += OnClosed;
    }

    protected override void OnPaintBackground(PaintEventArgs e)
    {
    }

    protected override void OnPaint(PaintEventArgs e)
    {
        if (background != null)
            e.Graphics.DrawImageUnscaled(background, 0, 0);
        e.Graphics.FillRectangle(Brushes.Orange, OrangeRect(frame));
        e.Graphics.FillRectangle(Brushes.MediumPurple, PurpleRect(frame));
        paintCount++;
    }

    private void OnShown(object sender, EventArgs args)
    {
        background = new Bitmap(
            Math.Max(1, ClientSize.Width),
            Math.Max(1, ClientSize.Height));
        using (Graphics graphics = Graphics.FromImage(background))
        {
            graphics.Clear(Color.FromArgb(38, 42, 54));
            const int cell = 96;
            using (Brush checker = new SolidBrush(Color.SlateGray))
            {
                for (int y = 0; y < ClientSize.Height; y += cell)
                    for (int x = 0; x < ClientSize.Width; x += cell)
                        if (((x / cell) + (y / cell)) % 2 == 0)
                            graphics.FillRectangle(checker, x, y, cell, cell);
            }
        }

        oldOrange = OrangeRect(frame);
        oldPurple = PurpleRect(frame);
        initialX = oldOrange.X;
        WriteStatus(false, "ready", statusPath);
        timer.Start();
    }

    private void OnTick(object sender, EventArgs args)
    {
        if (File.Exists(stopPath))
        {
            Close();
            return;
        }
        if (!started)
        {
            if (!File.Exists(goPath))
                return;
            string go = File.ReadAllText(goPath);
            if (!go.Contains("\"run_id\":\"" + runId + "\""))
                throw new InvalidDataException(
                    "GO file does not match this motion run");
            started = true;
            clock.Restart();
            WriteStatus(false, "running", statusPath);
            return;
        }

        Rectangle previousOrange = oldOrange;
        Rectangle previousPurple = oldPurple;
        frame++;
        oldOrange = OrangeRect(frame);
        oldPurple = PurpleRect(frame);
        Invalidate(PaddedUnion(previousOrange, oldOrange));
        Invalidate(PaddedUnion(previousPurple, oldPurple));

        if (frame >= 10 && oldOrange.X != initialX && paintCount >= 2)
            validated = true;
        if (frame == 10 || frame % 120 == 0)
            WriteStatus(false, "running", statusPath);
        if (clock.Elapsed.TotalSeconds >= durationSeconds)
            Close();
    }

    private Rectangle OrangeRect(int atFrame)
    {
        int travel = Math.Max(1, ClientSize.Width - 240);
        int x = (atFrame * 23) % travel;
        int y = Math.Max(40, (int)(ClientSize.Height * 0.42));
        return new Rectangle(x, y, 240, 140);
    }

    private Rectangle PurpleRect(int atFrame)
    {
        int travel = Math.Max(1, ClientSize.Width - 240);
        Rectangle orange = OrangeRect(atFrame);
        return new Rectangle(travel - orange.X, orange.Y + 170, 240, 140);
    }

    private static Rectangle PaddedUnion(Rectangle a, Rectangle b)
    {
        Rectangle result = Rectangle.Union(a, b);
        result.Inflate(2, 2);
        return result;
    }

    private void OnClosed(object sender, FormClosedEventArgs args)
    {
        timer.Stop();
        string phase = started ? "done" : "stopped";
        WriteStatus(started, phase, statusPath);
        if (started)
            WriteStatus(true, "done", donePath);
        if (background != null)
            background.Dispose();
    }

    private void WriteStatus(bool finished, string phase, string path)
    {
        double updateRate = clock.Elapsed.TotalSeconds > 0.0
            ? frame / clock.Elapsed.TotalSeconds
            : 0.0;
        string json = String.Format(
            System.Globalization.CultureInfo.InvariantCulture,
            "{{\"run_id\":\"{0}\",\"phase\":\"{1}\",\"validated\":{2},\"finished\":{3},\"frame\":{4},\"paint_count\":{5},\"orange_x\":{6},\"purple_x\":{7},\"elapsed_ms\":{8},\"update_rate_hz\":{9},\"width\":{10},\"height\":{11}}}",
            runId,
            phase,
            validated ? "true" : "false",
            finished ? "true" : "false",
            frame,
            paintCount,
            oldOrange.X,
            oldPurple.X,
            (long)clock.Elapsed.TotalMilliseconds,
            updateRate.ToString(
                "F3",
                System.Globalization.CultureInfo.InvariantCulture),
            ClientSize.Width,
            ClientSize.Height);
        Directory.CreateDirectory(Path.GetDirectoryName(path));
        File.WriteAllText(path, json, new UTF8Encoding(false));
    }
}
"@

$form = [AllMyStuffSandboxMotionForm]::new(
    [int]$env:ALLMYSTUFF_SANDBOX_MOTION_DURATION,
    $env:ALLMYSTUFF_SANDBOX_MOTION_STATUS,
    $env:ALLMYSTUFF_SANDBOX_MOTION_GO,
    $env:ALLMYSTUFF_SANDBOX_MOTION_STOP,
    $env:ALLMYSTUFF_SANDBOX_MOTION_DONE,
    $env:ALLMYSTUFF_SANDBOX_MOTION_RUN_ID)
[System.Windows.Forms.Application]::Run($form)
[ordered]@{
    run_id = $env:ALLMYSTUFF_SANDBOX_MOTION_RUN_ID
    success = $true
    exit_code = 0
} | ConvertTo-Json -Compress |
    Set-Content -LiteralPath $env:ALLMYSTUFF_SANDBOX_MOTION_EXIT -Encoding ascii
exit 0
}
catch {
    ($_ | Out-String) |
        Set-Content -LiteralPath $env:ALLMYSTUFF_SANDBOX_MOTION_ERROR -Encoding utf8
    [ordered]@{
        run_id = $env:ALLMYSTUFF_SANDBOX_MOTION_RUN_ID
        success = $false
        exit_code = 1
    } | ConvertTo-Json -Compress |
        Set-Content -LiteralPath $env:ALLMYSTUFF_SANDBOX_MOTION_EXIT -Encoding ascii
    exit 1
}
'@

$encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($child))
$environment = [ordered]@{
    ALLMYSTUFF_SANDBOX_MOTION_DURATION = "$DurationSeconds"
    ALLMYSTUFF_SANDBOX_MOTION_STATUS = $statusPath
    ALLMYSTUFF_SANDBOX_MOTION_GO = $goPath
    ALLMYSTUFF_SANDBOX_MOTION_STOP = $stopPath
    ALLMYSTUFF_SANDBOX_MOTION_DONE = $donePath
    ALLMYSTUFF_SANDBOX_MOTION_RUN_ID = $RunId
    ALLMYSTUFF_SANDBOX_MOTION_EXIT = $exitPath
    ALLMYSTUFF_SANDBOX_MOTION_ERROR = $errorPath
}
$saved = @{}
foreach ($name in $environment.Keys) {
    $saved[$name] = [Environment]::GetEnvironmentVariable(
        $name,
        [EnvironmentVariableTarget]::Process
    )
    [Environment]::SetEnvironmentVariable(
        $name,
        [string]$environment[$name],
        [EnvironmentVariableTarget]::Process
    )
}

$process = $null
try {
    $launcher = Join-Path $PSScriptRoot 'sandbox_process_launcher.exe'
    if (Test-Path -LiteralPath $launcher -PathType Leaf) {
        $launchRaw = & $launcher --cwd $runDirectory --stdout $stdoutPath `
            --stderr $stderrPath -- $program -NoLogo -NoProfile -STA `
            -EncodedCommand $encoded
        if ($LASTEXITCODE -ne 0) {
            throw "motion process launcher failed: $(($launchRaw | Out-String).Trim())"
        }
        $launched = (($launchRaw | Out-String).Trim()) | ConvertFrom-Json
        $process = Get-Process -Id ([int]$launched.pid) -ErrorAction Stop
    } else {
        $process = Start-Process -FilePath $program -ArgumentList @(
            '-NoLogo',
            '-NoProfile',
            '-STA',
            '-EncodedCommand',
            $encoded
        ) -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath
    }
} finally {
    foreach ($name in $environment.Keys) {
        [Environment]::SetEnvironmentVariable(
            $name,
            $saved[$name],
            [EnvironmentVariableTarget]::Process
        )
    }
}

try {
    $readyDeadline = [DateTime]::UtcNow.AddSeconds(12)
    $sourceStatus = $null
    do {
        Start-Sleep -Milliseconds 100
        if ($process.HasExited) {
            break
        }
        $sourceStatus = Read-JsonIfPresent -Path $statusPath
    } while (
        ($null -eq $sourceStatus -or
            [string]$sourceStatus.phase -cne 'ready') -and
        [DateTime]::UtcNow -lt $readyDeadline
    )
    if ($process.HasExited -or
        $null -eq $sourceStatus -or
        [string]$sourceStatus.run_id -cne $RunId -or
        [string]$sourceStatus.phase -cne 'ready') {
        $detail = @(
            if (Test-Path -LiteralPath $errorPath) {
                Get-Content -LiteralPath $errorPath -Raw
            }
            if (Test-Path -LiteralPath $stderrPath) {
                Get-Content -LiteralPath $stderrPath -Raw
            }
        ) -join [Environment]::NewLine
        throw "motion source did not reach READY: $detail"
    }

    Write-JsonAtomic -Path $goPath -Value ([ordered]@{
        schema = 1
        kind = 'allmystuff-sandbox-motion-go'
        run_id = $RunId
        created_utc = [DateTime]::UtcNow.ToString('o')
    })
    $runningDeadline = [DateTime]::UtcNow.AddSeconds(12)
    do {
        Start-Sleep -Milliseconds 100
        if ($process.HasExited) {
            break
        }
        $sourceStatus = Read-JsonIfPresent -Path $statusPath
    } while (
        ($null -eq $sourceStatus -or
            [string]$sourceStatus.phase -cne 'running' -or
            $sourceStatus.validated -ne $true -or
            [int]$sourceStatus.frame -lt 10 -or
            [int]$sourceStatus.paint_count -lt 2) -and
        [DateTime]::UtcNow -lt $runningDeadline
    )
    if ($process.HasExited -or
        $null -eq $sourceStatus -or
        [string]$sourceStatus.run_id -cne $RunId -or
        [string]$sourceStatus.phase -cne 'running' -or
        $sourceStatus.validated -ne $true -or
        [int]$sourceStatus.frame -lt 10 -or
        [int]$sourceStatus.paint_count -lt 2 -or
        [int]$sourceStatus.orange_x -eq 0) {
        throw 'motion source did not prove that painted positions advanced'
    }

    $record = [ordered]@{
        schema = 1
        kind = 'allmystuff-sandbox-motion-source'
        run_id = $RunId
        pid = [int]$process.Id
        program = $program
        started_unix_ms = Get-ProcessStartUnixMilliseconds -Process $process
        duration_seconds = $DurationSeconds
        script_sha256 = (
            Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256
        ).Hash
        run_directory = $runDirectory
        status_path = $statusPath
        go_path = $goPath
        stop_path = $stopPath
        done_path = $donePath
        exit_path = $exitPath
        created_utc = [DateTime]::UtcNow.ToString('o')
    }
    Write-JsonAtomic -Path $recordPath -Value $record
    [ordered]@{
        status = 'started'
        running = $true
        run_id = $RunId
        record = $record
        source_status = $sourceStatus
    } | ConvertTo-Json -Depth 12 -Compress
} catch {
    if ($null -ne $process -and -not $process.HasExited) {
        $process.Kill()
        $process.WaitForExit(5000)
    }
    throw
}
