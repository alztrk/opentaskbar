# Compiles the lightweight native Win32 watchdog helper (24 KB, ~1 MB RAM)
param (
    [string]$OutputDir = $PSScriptRoot,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$source = Join-Path $PSScriptRoot "watchdog.c"
$outputExe = Join-Path $OutputDir "OpenTaskbarWatchdog.exe"

if (-not $Force -and (Test-Path $outputExe) -and (Get-Item $outputExe).LastWriteTimeUtc -ge (Get-Item $source).LastWriteTimeUtc) {
    Write-Host "Native watchdog binary is up to date; skipping rebuild." -ForegroundColor DarkGray
    exit 0
}

$gcc = $null
$gccCmd = Get-Command gcc.exe -ErrorAction SilentlyContinue
if ($gccCmd) {
    $gcc = $gccCmd.Source
} elseif (Test-Path "C:\Strawberry\c\bin\gcc.exe") {
    $gcc = "C:\Strawberry\c\bin\gcc.exe"
}

if ($gcc) {
    Write-Host "Compiling native watchdog using GCC: $gcc..." -ForegroundColor Cyan
    & $gcc -O2 -s -mwindows $source -o $outputExe -luser32 -lshell32
    if ($LASTEXITCODE -eq 0) {
        Write-Host "Successfully compiled $outputExe ($((Get-Item $outputExe).Length) bytes)" -ForegroundColor Green
        exit 0
    }
}

# Fallback to MSVC cl.exe if gcc is not available
$clCmd = Get-Command cl.exe -ErrorAction SilentlyContinue
if ($clCmd) {
    $cl = $clCmd.Source
    Write-Host "Compiling native watchdog using MSVC cl: $cl..." -ForegroundColor Cyan
    & $cl /O2 /W4 /Fe:$outputExe $source user32.lib shell32.lib /link /subsystem:windows
    if ($LASTEXITCODE -eq 0) {
        Write-Host "Successfully compiled $outputExe with MSVC" -ForegroundColor Green
        exit 0
    }
}

Write-Error "Could not find GCC or MSVC to compile watchdog.c"
exit 1
