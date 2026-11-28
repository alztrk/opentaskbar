# Builds OpenTaskbar as a portable, self-contained Rust EXE per architecture.
# Builds from the root Cargo.toml; no external SDK or reference artifacts are involved.
param (
    [ValidateSet('win-x64', 'win-x86')]
    [string]$Runtime = 'win-x64',
    [string]$OutputDir = "$PSScriptRoot\..\release",
    [switch]$SkipTargetInstall
)

$ErrorActionPreference = 'Stop'

$arch = ($Runtime -split '-')[-1]
$target = if ($arch -eq 'x64') { 'x86_64-pc-windows-msvc' } else { 'i686-pc-windows-msvc' }
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$manifest = Join-Path $repoRoot 'Cargo.toml'

if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) { throw "Missing Cargo manifest: $manifest" }
if (-not (Test-Path -LiteralPath $OutputDir -PathType Container)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}
$OutputDir = (Resolve-Path -LiteralPath $OutputDir).Path

if (-not $SkipTargetInstall) {
    Write-Host "Ensuring Rust target $target is installed..." -ForegroundColor Cyan
    & rustup target add $target
    if ($LASTEXITCODE -ne 0) { throw "Could not install Rust target $target." }
}

Write-Host "Building OpenTaskbar Rust port for $Runtime..." -ForegroundColor Cyan
& cargo build --manifest-path $manifest --release --target $target
if ($LASTEXITCODE -ne 0) { throw "Rust release build failed for $Runtime." }

$built = Join-Path $repoRoot "target\$target\release\OpenTaskBar.exe"
if (-not (Test-Path -LiteralPath $built -PathType Leaf)) {
    throw "Expected Rust binary not found: $built"
}

$finalExe = Join-Path $OutputDir "OpenTaskbar-$arch.exe"
Copy-Item -LiteralPath $built -Destination $finalExe -Force

# build.rs compiled the watchdog into OUT_DIR for this exact target and embedded
# it into the EXE. Keep a named copy beside the portable EXE so the release
# manifest can record its architecture and SHA-256 (and CI can re-verify it).
$buildRoot = Join-Path $repoRoot "target\$target\release\build"
$watchdogExe = Get-ChildItem -LiteralPath $buildRoot -Filter 'OpenTaskbarWatchdog.exe' -File -Recurse -ErrorAction SilentlyContinue |
    Where-Object { $_.Directory.Name -eq 'watchdog' -and $_.Directory.Parent.Name -eq 'out' } |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $watchdogExe) {
    throw "No architecture-specific watchdog produced by build.rs under $buildRoot"
}
$expectedMachine = if ($arch -eq 'x86') { 0x014C } else { 0x8664 }
$bytes = [IO.File]::ReadAllBytes($watchdogExe)
if ($bytes.Length -lt 64 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
    throw "Watchdog is not a valid PE executable: $watchdogExe"
}
$peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
$actualMachine = [BitConverter]::ToUInt16($bytes, $peOffset + 4)
if ($actualMachine -ne $expectedMachine) {
    throw ("Watchdog PE machine 0x{0:X4} does not match {1} (0x{2:X4})." -f $actualMachine, $arch, $expectedMachine)
}
$watchdogCopy = Join-Path $OutputDir "OpenTaskbarWatchdog-$arch.exe"
Copy-Item -LiteralPath $watchdogExe -Destination $watchdogCopy -Force
Write-Host ("Rust portable EXE ready: {0} (watchdog {1}, PE 0x{2:X4})" -f $finalExe, $arch, $actualMachine) -ForegroundColor Green
