# Builds the OpenTaskbar Rust port as a portable, self-contained EXE per architecture.
# Mirrors scripts/publish-portable.ps1 but targets Rust-Port/Cargo.toml.
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
$manifest = Join-Path $repoRoot 'Rust-Port\Cargo.toml'

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

$built = Join-Path $repoRoot "Rust-Port\target\$target\release\OpenTaskBar.exe"
if (-not (Test-Path -LiteralPath $built -PathType Leaf)) {
    throw "Expected Rust binary not found: $built"
}

$finalExe = Join-Path $OutputDir "OpenTaskbar-Rust-$arch.exe"
Copy-Item -LiteralPath $built -Destination $finalExe -Force
Write-Host "Rust portable EXE ready: $finalExe" -ForegroundColor Green
