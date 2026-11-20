# Verify that the exact executable uploaded in a release can start without any sidecar DLLs.
param (
    [string]$OutputDir = "$PSScriptRoot\..\release"
)

$ErrorActionPreference = "Stop"

$directory = (Resolve-Path -LiteralPath $OutputDir).Path
$executable = Join-Path $directory "OpenTaskbar.exe"
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Portable executable is missing: $executable"
}

# Our release only ships OpenTaskbar.exe, not the rest of the publish directory.
# If a native DLL remains beside the EXE, the release package would be incomplete.
$sidecarDlls = @(Get-ChildItem -LiteralPath $directory -Filter "*.dll" -File)
if ($sidecarDlls.Count -gt 0) {
    $names = ($sidecarDlls | ForEach-Object Name) -join ", "
    throw "Portable EXE depends on native DLLs that are not in the release package: $names"
}

Write-Host "Starting published executable in safe WPF smoke-test mode..."
$process = Start-Process -FilePath $executable -ArgumentList "--smoke-test" -PassThru -WindowStyle Hidden
try {
    if (-not $process.WaitForExit(30000)) {
        throw "Portable EXE did not exit from smoke-test mode within 30 seconds."
    }
    if ($process.ExitCode -ne 0) {
        throw "Portable EXE failed to start (exit code $($process.ExitCode))."
    }
}
finally {
    if (-not $process.HasExited) {
        $process.Kill()
        $process.WaitForExit()
    }
    $process.Dispose()
}
Write-Host "Portable EXE startup check passed."
