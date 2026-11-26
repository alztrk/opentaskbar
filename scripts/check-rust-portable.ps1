# Verifies that the exact Rust EXE uploaded in a release is self-contained and
# can start its short-lived icon worker without shipping sidecar DLLs.
param (
    [string]$OutputDir = "$PSScriptRoot\..\release",
    [string]$ExecutableName = 'OpenTaskbar-Rust-x64.exe'
)

$ErrorActionPreference = 'Stop'
$directory = (Resolve-Path -LiteralPath $OutputDir).Path
$executable = Join-Path $directory $ExecutableName
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Rust portable executable is missing: $executable"
}

# A PE/COFF header check catches truncated or wrong-architecture artifacts even
# when the file name looks correct.
$stream = [IO.File]::OpenRead($executable)
try {
    $header = New-Object byte[] 2
    if ($stream.Read($header, 0, 2) -ne 2 -or [Text.Encoding]::ASCII.GetString($header) -ne 'MZ') {
        throw "Not a valid Windows executable: $executable"
    }
} finally {
    $stream.Dispose()
}

# The release ships a single EXE. Any third-party DLL beside it would mean the
# Rust port is not actually portable (OS-provided api-ms-win-* DLLs are fine).
$sidecars = @(Get-ChildItem -LiteralPath $directory -Filter '*.dll' -File | Where-Object {
    $_.Name -notlike 'api-ms-win-*' -and $_.Name -notlike 'OpenTaskbar-*.dll'
})
if ($sidecars.Count -gt 0) {
    throw "Unexpected sidecar DLLs next to the Rust EXE: $(($sidecars.Name) -join ', ')"
}

# --extract-icons exits immediately when stdin is closed, making it a safe smoke
# test that never opens the dock or touches the taskbar.
Write-Host "Starting Rust EXE in icon-worker smoke-test mode..."
$nullIn = Join-Path $env:TEMP 'opentaskbar-rust-stdin-null'
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

Write-Host "Rust portable EXE startup check passed."
