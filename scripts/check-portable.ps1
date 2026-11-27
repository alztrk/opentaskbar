# Verifies that the exact Rust EXE uploaded in a release is self-contained and
# can start its short-lived icon worker without shipping sidecar DLLs.
param (
    [string]$OutputDir = "$PSScriptRoot\..\release",
    [string]$ExecutableName = 'OpenTaskbar-x64.exe',
    # PE COFF machine (0x014C = x86, 0x8664 = x64). Derived from the file name
    # when omitted, so an "x86" asset can never hide an x64 payload.
    [ValidateSet('', 'x86', 'x64')]
    [string]$Architecture = ''
)

$ErrorActionPreference = 'Stop'
$directory = (Resolve-Path -LiteralPath $OutputDir).Path
$executable = Join-Path $directory $ExecutableName
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Rust portable executable is missing: $executable"
}

if (-not $Architecture) {
    if ($ExecutableName -match 'x86') { $Architecture = 'x86' }
    elseif ($ExecutableName -match 'x64') { $Architecture = 'x64' }
    else { throw "Cannot infer architecture from $ExecutableName; pass -Architecture." }
}
$expectedMachine = if ($Architecture -eq 'x86') { 0x014C } else { 0x8664 }

function Get-PeMachine {
    param ([string]$Path)
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 64 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        throw "Not a valid Windows executable: $Path"
    }
    $offset = [BitConverter]::ToInt32($bytes, 0x3C)
    if ($offset -le 0 -or ($offset + 6) -gt $bytes.Length -or
        [BitConverter]::ToUInt32($bytes, $offset) -ne 0x00004550) {
        throw "Missing PE signature: $Path"
    }
    return [BitConverter]::ToUInt16($bytes, $offset + 4)
}

# A PE/COFF header check catches truncated or wrong-architecture artifacts even
# when the file name looks correct.
$machine = Get-PeMachine -Path $executable
if ($machine -ne $expectedMachine) {
    throw ("Wrong PE machine for {0}: 0x{1:X4}, expected 0x{2:X4} ({3})." -f $ExecutableName, $machine, $expectedMachine, $Architecture)
}
Write-Host ("PE machine OK for {0}: 0x{1:X4} ({2})" -f $ExecutableName, $machine, $Architecture) -ForegroundColor DarkGray

# publish-portable.ps1 always drops the exact watchdog embedded by build.rs next
# to the EXE. Its hash is recorded in the release manifest, so a missing file
# must fail the gate instead of silently producing an unverifiable manifest.
$watchdog = Join-Path $directory "OpenTaskbarWatchdog-$Architecture.exe"
if (-not (Test-Path -LiteralPath $watchdog -PathType Leaf)) {
    throw "Embedded watchdog is missing: $watchdog (run scripts/publish-portable.ps1 first)."
}
$watchdogMachine = Get-PeMachine -Path $watchdog
if ($watchdogMachine -ne $expectedMachine) {
    throw ("Wrong PE machine for embedded watchdog ({0}): 0x{1:X4}, expected 0x{2:X4}." -f $Architecture, $watchdogMachine, $expectedMachine)
}

# The release ships a single EXE. Any third-party DLL beside it would mean the
# Rust port is not actually portable (OS-provided api-ms-win-* DLLs are fine).
$sidecars = @(Get-ChildItem -LiteralPath $directory -Filter '*.dll' -File | Where-Object {
    $_.Name -notlike 'api-ms-win-*'
})
if ($sidecars.Count -gt 0) {
    throw "Unexpected sidecar DLLs next to the Rust EXE: $(($sidecars.Name) -join ', ')"
}

# --extract-icons exits immediately when stdin is closed, making it a safe smoke
# test that never opens the dock or touches the taskbar.
Write-Host "Starting Rust EXE in icon-worker smoke-test mode..."
$nullIn = Join-Path $env:TEMP 'opentaskbar-stdin-null'
[IO.File]::WriteAllText($nullIn, '')
$process = $null
try {
    $process = Start-Process -FilePath $executable -ArgumentList '--extract-icons' -PassThru `
        -WindowStyle Hidden -RedirectStandardInput $nullIn
    if (-not $process.WaitForExit(20000)) {
        throw "Rust EXE did not exit from icon-worker mode within 20 seconds."
    }
    if ($process.ExitCode -ne 0) {
        throw "Rust EXE smoke test failed (exit code $($process.ExitCode))."
    }
} finally {
    if ($process -and -not $process.HasExited) {
        $process.Kill()
        $process.WaitForExit()
    }
    if ($process) { $process.Dispose() }
    Remove-Item -LiteralPath $nullIn -Force -ErrorAction SilentlyContinue
}

Write-Host "Rust portable EXE startup check passed ($Architecture)."
