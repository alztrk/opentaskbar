<#
.SYNOPSIS
    Repeatable CPU / memory / allocation baseline for OpenTaskbar magnification and idle work.

.DESCRIPTION
    Launches the app with a deterministic settings file, then measures two phases:

      Phase A (idle)  : no pointer movement over the dock.
      Phase B (sweep) : a synthetic pointer sweeps across the dock at ~80 Hz, exercising the
                        fisheye magnification MouseMove path continuously.

    The delta between the two phases isolates the magnification cost from background timers.
    Raw dotnet-counters CSVs plus a compact summary.json are written to artifacts/perf/<Label>.

    No production code is modified. The settings file and the Windows taskbar are restored on exit.
    The app runs in its normal replacement mode during measurement.

.PARAMETER Label
    Output folder name. Use e.g. "baseline", then "after-p0" for the comparison run.

.PARAMETER IdleSeconds
    Duration of the idle phase (default 12).

.PARAMETER SweepSeconds
    Duration of the pointer-sweep phase (default 12).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\scripts\measure-baseline.ps1 -Label baseline
#>
param(
    [string]$Label = "baseline",
    [int]$IdleSeconds = 12,
    [int]$SweepSeconds = 12,
    [string]$MagnificationEnabled = "true"
)

$ErrorActionPreference = 'Stop'

# --------------------------------------------------------------------------- helpers

function Get-CounterExe {
    $cmd = Get-Command dotnet-counters -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $installed = Join-Path $env:USERPROFILE '.dotnet\tools\dotnet-counters.exe'
    if (Test-Path $installed) { return $installed }
    throw "dotnet-counters not found. Install with: dotnet tool install --global dotnet-counters"
}

function Format-Duration([int]$seconds) { '{0:00}:{1:00}:{2:00}:{3:00}' -f 0, 0, 0, $seconds }

function Get-ProcessSample($process) {
    $process.Refresh()
    [pscustomobject]@{
        CpuSeconds    = $process.CPU
        WorkingSet    = $process.WorkingSet64
        PrivateBytes  = $process.PrivateMemorySize64
        ThreadCount   = $process.Threads.Count
        HandleCount   = $process.HandleCount
    }
}

function Read-CounterSummary([string]$csvPath) {
    if (-not (Test-Path $csvPath)) { return $null }
    $rows = @(Import-Csv $csvPath)
    if ($rows.Count -eq 0) { return $null }

    function Average-For([string]$like) {
        $values = @($rows | Where-Object { $_.'Counter Name' -like $like } |
            ForEach-Object { [double]::Parse($_.'Mean/Increment', [cultureinfo]::InvariantCulture) })
        if ($values.Count -eq 0) { return $null }
        return [math]::Round(($values | Measure-Object -Average).Average, 2)
    }

    [pscustomobject]@{
        AllocationBytesPerSec = Average-For 'dotnet.gc.heap.total_allocated*'
        Gen0CollectionsPerSec = Average-For 'dotnet.gc.collections*gen0*'
        Gen1CollectionsPerSec = Average-For 'dotnet.gc.collections*gen1*'
        Gen2CollectionsPerSec = Average-For 'dotnet.gc.collections*gen2*'
        GcPauseSecondsPerSec  = Average-For 'dotnet.gc.pause.time*'
        CpuUserSecondsPerSec  = Average-For '*cpu.time*user*'
        CpuSystemSecondsPerSec = Average-For '*cpu.time*system*'
        WorkingSetBytes       = Average-For 'dotnet.process.memory.working_set*'
        TimerCount            = Average-For 'dotnet.timer.count*'
        ThreadPoolThreads     = Average-For 'dotnet.thread_pool.thread.count*'
    }
}

function Start-CounterPhase {
    param([string]$Phase, [int]$Seconds, [int]$ProcessId, [string]$OutputDir, [string]$CounterExe)

    $csv = Join-Path $OutputDir "counters-$Phase.csv"
    $log = Join-Path $OutputDir "counters-$Phase.log"
    $err = Join-Path $OutputDir "counters-$Phase.err"
    $args = @(
        'collect', '-p', "$ProcessId", '--counters', 'System.Runtime',
        '--refresh-interval', '1', '--format', 'csv', '-o', $csv,
        '--duration', (Format-Duration $Seconds)
    )
    $counterProcess = Start-Process -FilePath $CounterExe -ArgumentList $args -PassThru `
        -WindowStyle Hidden -RedirectStandardOutput $log -RedirectStandardError $err
    return [pscustomobject]@{ Process = $counterProcess; Csv = $csv; ErrorLog = $err; Phase = $Phase; Seconds = $Seconds }
}

function Wait-CounterPhase {
    param($session)
    if (-not $session.Process.WaitForExit(($session.Seconds + 30) * 1000)) {
        $session.Process.Kill()
        throw "dotnet-counters did not exit after the $($session.Phase) phase."
    }
    $session.Process.Refresh()
    $csvRows = if (Test-Path $session.Csv) { @(Get-Content $session.Csv).Count } else { 0 }
    if ($csvRows -lt 2) {
        throw "dotnet-counters produced no data in $($session.Phase) phase (exit $($session.Process.ExitCode)): $(Get-Content $session.ErrorLog -Raw)"
    }
    return $session.Csv
}

# --------------------------------------------------------------------------- P/Invoke

if (-not ('PerfNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class PerfNative
{
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
}
'@
}
[PerfNative]::SetProcessDPIAware() | Out-Null   # no-op when the process is already DPI aware

# --------------------------------------------------------------------------- paths

$repoRoot = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repoRoot 'bin\x64\Release\net10.0-windows10.0.19041.0\OpenTaskbar.exe'
$existing = @(Get-Process OpenTaskbar -ErrorAction SilentlyContinue)
if ($existing.Count -gt 0) {
    throw "OpenTaskbar is already running (PID $($existing.Id -join ', ')). Close it first: Get-Process OpenTaskbar | Stop-Process -Force"
}
if (-not (Test-Path $exe)) { throw "Build first: dotnet build $repoRoot\OpenTaskbar.csproj -c Release" }
$counterExe = Get-CounterExe

$outputDir = Join-Path $repoRoot "artifacts\perf\$Label"
New-Item -ItemType Directory -Force -Path $outputDir | Out-Null

$settingsDir = Join-Path $env:LOCALAPPDATA 'OpenTaskbar'
$settingsPath = Join-Path $settingsDir 'settings.json'
$settingsBackup = Join-Path $outputDir 'settings.original.json'
$settingsExisted = Test-Path $settingsPath
if ($settingsExisted) { Copy-Item $settingsPath $settingsBackup -Force }

New-Item -ItemType Directory -Force -Path $settingsDir | Out-Null
@"
{
  "Language": "en-US",
  "Theme": "Dark",
  "Position": "Center",
  "AnimationsEnabled": true,
  "MagnificationEnabled": $($MagnificationEnabled.ToLowerInvariant()),
  "PreviewsEnabled": false,
  "NotificationBadgesEnabled": true,
  "AutoHide": "Disabled",
  "DockBackground": "Glass",
  "DockOpacity": 0.9,
  "MenuBackground": "Glass",
  "MenuOpacity": 0.9,
  "WidgetBackground": "Glass",
  "WidgetOpacity": 0.9,
  "SettingsBackground": "Glass",
  "SettingsOpacity": 0.9,
  "Widgets": ["SystemStats", "Clock", "Focus", "Audio", "QuickSettings", "Tray", "Desktops", "Media"],
  "InstalledWidgets": ["SystemStats", "Clock", "Focus", "Audio", "QuickSettings", "Tray", "Desktops", "Media"],
  "Layout": "Floating",
  "Size": "Compact",
  "ShowRecycleBin": true,
  "CheckUpdates": false,
  "WheelVolume": true,
  "StatsMeters": "All",
  "Edge": "Bottom",
  "Clock24Hour": true,
  "DateStyle": "DayAndShortDate",
  "DockScale": 1.0
}
"@ | Set-Content -Path $settingsPath -Encoding UTF8

# Running apps give the fisheye real buttons to magnify; the Start button and Recycle Bin are always present.
$measurementStartedAt = Get-Date
$notepads = @()
1..6 | ForEach-Object {
    $notepads += Start-Process -FilePath (Join-Path $env:WINDIR 'System32\notepad.exe') -PassThru
}
Start-Sleep -Seconds 2
$timeline = Join-Path $outputDir 'timeline.log'
function Write-Timeline([string]$message) {
    "$(Get-Date -Format 'HH:mm:ss.fff') $message" | Add-Content -Path $timeline
}
$notepadWindows = @(Get-Process notepad -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 })
Write-Timeline "measurement started; notepad launch PIDs: $($notepads.Id -join ', '); notepad windows: $($notepadWindows.Count)"

$app = $null
try {
    $app = Start-Process -FilePath $exe -PassThru
    Write-Timeline "app launched PID $($app.Id)"

    $deadline = (Get-Date).AddSeconds(15)
    do {
        Start-Sleep -Milliseconds 250
        $app.Refresh()
    } while ($app.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline)
    if ($app.MainWindowHandle -eq 0) { throw 'Dock window handle could not be found.' }

    $hwnd = $app.MainWindowHandle
    [PerfNative]::SetForegroundWindow($hwnd) | Out-Null
    # Keep the dock above the console so synthetic cursor moves actually reach it.
    $swpNoMoveNoSize = 0x0001 -bor 0x0002
    [PerfNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, $swpNoMoveNoSize) | Out-Null

    $rect = New-Object PerfNative+RECT
    [PerfNative]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
    $centerX = [int](($rect.Left + $rect.Right) / 2)
    $sweepY = [int]($rect.Top + ($rect.Bottom - $rect.Top) * 0.62)
    $span = [int][math]::Min(520, ($rect.Right - $rect.Left) / 3)

    $commandLine = (Get-CimInstance Win32_Process -Filter "ProcessId=$($app.Id)").CommandLine
    Write-Timeline "command line: $commandLine"
    Write-Host "PID $($app.Id) | window $($rect.Left),$($rect.Top)-$($rect.Right),$($rect.Bottom) | sweep X $($centerX - $span)..$($centerX + $span) @ Y=$sweepY"
    Write-Host "Phase A: idle $IdleSeconds s ..."
    Write-Timeline "phase A start"
    $idleBefore = Get-ProcessSample $app
    $idleStarted = [Diagnostics.Stopwatch]::StartNew()
    $idleCounters = Start-CounterPhase -Phase 'idle' -Seconds $IdleSeconds -ProcessId $app.Id -OutputDir $outputDir -CounterExe $counterExe
    $idleCsv = Wait-CounterPhase $idleCounters
    $idleElapsed = $idleStarted.Elapsed.TotalSeconds
    $app.Refresh()
    if ($app.HasExited) { throw "App exited during the idle phase (code $($app.ExitCode)). See $timeline and the app log." }
    $idleAfter = Get-ProcessSample $app
    Write-Timeline "phase A end; alive=True"

    Write-Host "Phase B: pointer sweep $SweepSeconds s ..."
    Write-Timeline "phase B start"
    $sweepBefore = Get-ProcessSample $app
    $sweepStarted = [Diagnostics.Stopwatch]::StartNew()
    $sweepCounters = Start-CounterPhase -Phase 'sweep' -Seconds $SweepSeconds -ProcessId $app.Id -OutputDir $outputDir -CounterExe $counterExe

    $tick = 0
    $period = 72   # ~0.9 s per full left-right-left pass at 12 ms per step
    while ($sweepStarted.Elapsed.TotalSeconds -lt $SweepSeconds) {
        if ($tick % 80 -eq 0) {
            $app.Refresh()
            if ($app.HasExited) { throw "App exited during the sweep phase after $tick moves (code $($app.ExitCode))." }
            Write-Timeline "sweep tick $tick; alive=True"
        }
        $phase = 2 * [math]::PI * $tick / $period
        $x = [int]($centerX + $span * [math]::Sin($phase))
        [PerfNative]::SetCursorPos($x, $sweepY) | Out-Null
        $tick++
        Start-Sleep -Milliseconds 12
    }
    $sweepCsv = Wait-CounterPhase $sweepCounters
    $sweepElapsed = $sweepStarted.Elapsed.TotalSeconds
    $app.Refresh()
    if ($app.HasExited) { throw "App exited at the end of the sweep phase (code $($app.ExitCode))." }
    $sweepAfter = Get-ProcessSample $app
    Write-Timeline "phase B end; alive=True; moves=$tick"

    [PerfNative]::SetWindowPos($hwnd, [IntPtr](-2), 0, 0, 0, 0, $swpNoMoveNoSize) | Out-Null

    $cores = [Environment]::ProcessorCount
    function Phase-ProcessDelta($before, $after, [double]$elapsed) {
        [pscustomobject]@{
            CpuSeconds        = [math]::Round($after.CpuSeconds - $before.CpuSeconds, 2)
            CpuPercentOfOneCore = [math]::Round(($after.CpuSeconds - $before.CpuSeconds) / $elapsed * 100, 1)
            CpuPercentOfMachine = [math]::Round(($after.CpuSeconds - $before.CpuSeconds) / ($elapsed * $cores) * 100, 1)
            WorkingSetDeltaBytes = $after.WorkingSet - $before.WorkingSet
            PrivateBytesDelta   = $after.PrivateBytes - $before.PrivateBytes
            ThreadCount         = $after.ThreadCount
            HandleCount         = $after.HandleCount
        }
    }

    $summary = [ordered]@{
        Label          = $Label
        CapturedAtUtc  = (Get-Date).ToUniversalTime().ToString('o')
        DurationSeconds = [ordered]@{ Idle = $IdleSeconds; Sweep = $SweepSeconds }
        PointerMoves   = $tick
        Cores          = $cores
        WindowRect     = [ordered]@{ Left = $rect.Left; Top = $rect.Top; Right = $rect.Right; Bottom = $rect.Bottom }
        Idle = [ordered]@{
            Process = Phase-ProcessDelta $idleBefore $idleAfter $idleElapsed
            Counters = Read-CounterSummary $idleCsv
        }
        Sweep = [ordered]@{
            Process = Phase-ProcessDelta $sweepBefore $sweepAfter $sweepElapsed
            Counters = Read-CounterSummary $sweepCsv
        }
    }
    $summaryPath = Join-Path $outputDir 'summary.json'
    $summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath -Encoding UTF8

    Write-Host ''
    Write-Host "Summary: $summaryPath"
    $summary | ConvertTo-Json -Depth 8
}
finally {
    if ($app -and -not $app.HasExited) { $app.Kill() }
    # Notepad can forward a launch to an existing instance, so clean up by start time instead of launch PID.
    Get-Process notepad -ErrorAction SilentlyContinue |
        Where-Object { $_.StartTime -ge $measurementStartedAt.AddSeconds(-2) } |
        Stop-Process -Force -ErrorAction SilentlyContinue
    if ($settingsExisted) { Copy-Item $settingsBackup $settingsPath -Force }
    else { Remove-Item $settingsPath -Force -ErrorAction SilentlyContinue }
    Write-Timeline "cleanup done"
}
