# Compiles the lightweight native Win32 watchdog helper (24 KB, ~1 MB RAM)
param (
    [ValidateSet('x64', 'x86')]
    [string]$Architecture = 'x64',
    [Parameter(Mandatory = $true)]
    [string]$OutputDir,
    [switch]$Force,
    # Release/Rust builds use this: never substitute an x64 PE for an x86 build.
    [switch]$NoFallback
)

$ErrorActionPreference = "Stop"

$source = Join-Path $PSScriptRoot "watchdog.c"
$expectedMachine = if ($Architecture -eq 'x86') { 0x014C } else { 0x8664 }

function Get-PeMachine {
    param ([string]$Path)
    $stream = $null
    try {
        $stream = [IO.File]::OpenRead($Path)
        $reader = [IO.BinaryReader]::new($stream)
        if ($reader.ReadUInt16() -ne 0x5A4D) { return 0 } # MZ
        $stream.Position = 0x3C
        $offset = $reader.ReadInt32()
        if ($offset -le 0 -or ($offset + 6) -gt $stream.Length) { return 0 }
        $stream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x00004550) { return 0 } # PE\0\0
        return $reader.ReadUInt16()
    } catch {
        return 0
    } finally {
        if ($stream) { $stream.Dispose() }
    }
}

if (-not (Test-Path -LiteralPath $OutputDir -PathType Container)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}
$outputExe = Join-Path $OutputDir "OpenTaskbarWatchdog.exe"

# An existing binary of the wrong architecture must never be treated as current
# (e.g. a checked-in x64 EXE sitting in an x86 OUT_DIR).
if (-not $Force -and (Test-Path -LiteralPath $outputExe -PathType Leaf) `
    -and (Get-PeMachine -Path $outputExe) -eq $expectedMachine `
    -and (Get-Item $outputExe).LastWriteTimeUtc -ge (Get-Item $source).LastWriteTimeUtc) {
    Write-Host "Native watchdog binary ($Architecture, 0x$('{0:X4}' -f $expectedMachine)) is up to date; skipping rebuild." -ForegroundColor DarkGray
    exit 0
}

$objectFile = Join-Path $env:TEMP ("opentaskbar-watchdog-{0}.obj" -f $Architecture)
if (Test-Path -LiteralPath $objectFile) { Remove-Item -LiteralPath $objectFile -Force }
# Keep the native helper byte-stable too: MinGW/binutils honours SOURCE_DATE_EPOCH,
# MSVC uses /Brepro. Without this the managed deterministic build still changes hash.
$env:SOURCE_DATE_EPOCH = '315532800' # 1980-01-01 UTC

function Test-CompiledBinary {
    if (-not (Test-Path -LiteralPath $outputExe -PathType Leaf)) { return $false }
    $machine = Get-PeMachine -Path $outputExe
    if ($machine -eq $expectedMachine) { return $true }
    Write-Host ("Compiler produced machine 0x{0:X4}, expected 0x{1:X4}; discarding." -f $machine, $expectedMachine) -ForegroundColor Yellow
    Remove-Item -LiteralPath $outputExe -Force -ErrorAction SilentlyContinue
    return $false
}

function Invoke-Compiler {
    param ([string]$Label, [scriptblock]$Compile, [switch]$Quiet)
    try {
        Write-Host "Compiling native watchdog ($Architecture) using $Label..." -ForegroundColor Cyan
        if ($Quiet) {
            $compilerOutput = & $Compile 2>&1
            if ($LASTEXITCODE -ne 0) {
                $compilerOutput | Select-Object -Last 3 | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
            }
        } else {
            & $Compile
        }
        if ($LASTEXITCODE -eq 0 -and (Test-CompiledBinary)) {
            Write-Host "Successfully compiled $outputExe ($((Get-Item $outputExe).Length) bytes, machine 0x$('{0:X4}' -f $expectedMachine))" -ForegroundColor Green
            return $true
        }
    } catch {
        Write-Host "$Label failed: $_" -ForegroundColor Yellow
    } finally {
        Remove-Item -LiteralPath $objectFile -Force -ErrorAction SilentlyContinue
    }
    return $false
}

# GCC is the fastest path for x64. For x86 it needs multilib, which is not always installed.
$gcc = $null
$gccCmd = Get-Command gcc.exe -ErrorAction SilentlyContinue
if ($gccCmd) {
    $gcc = $gccCmd.Source
} elseif (Test-Path "C:\Strawberry\c\bin\gcc.exe") {
    $gcc = "C:\Strawberry\c\bin\gcc.exe"
}
if ($gcc) {
    $gccArch = if ($Architecture -eq 'x86') { '-m32' } else { '' }
    $built = Invoke-Compiler -Label "GCC $gcc" -Quiet -Compile {
        if ($gccArch) { & $gcc -O2 -s $gccArch -mwindows $source -o $outputExe -luser32 -lshell32 }
        else { & $gcc -O2 -s -mwindows $source -o $outputExe -luser32 -lshell32 }
    }
    if ($built) { exit 0 }
}

# MSVC can always target x86 through vcvarsall, even from an x64-hosted runner.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (Test-Path $vswhere) {
    $vsPath = & $vswhere -latest -products '*' -requires 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' -property installationPath
    $vcvarsall = if ($vsPath) { Join-Path $vsPath 'VC\Auxiliary\Build\vcvarsall.bat' }
    if ($vcvarsall -and (Test-Path $vcvarsall)) {
        $cmdFile = Join-Path $env:TEMP ("opentaskbar-watchdog-{0}.cmd" -f $Architecture)
        $platformArg = $Architecture
        $batch = @"
@echo off
call "$vcvarsall" $platformArg
if errorlevel 1 exit /b 1
cl.exe /nologo /O2 /W4 /Brepro /Fo:"$objectFile" /Fe:"$outputExe" "$source" user32.lib shell32.lib /link /subsystem:windows
exit /b %errorlevel%
"@
        $batch | Set-Content -Path $cmdFile -Encoding ASCII
        try {
            $built = Invoke-Compiler -Label "MSVC ($platformArg)" -Compile { & cmd.exe /d /c "`"$cmdFile`"" }
            if ($built) { exit 0 }
        } finally {
            Remove-Item $cmdFile -Force -ErrorAction SilentlyContinue
        }
    }
}

# Last resort: a plain cl.exe already on PATH (works for x64, and for x86 from a VS developer prompt).
$clCmd = Get-Command cl.exe -ErrorAction SilentlyContinue
if ($clCmd) {
    $built = Invoke-Compiler -Label "MSVC cl.exe on PATH" -Compile {
        & $clCmd.Source /nologo /O2 /W4 /Brepro /Fo:"$objectFile" /Fe:"$outputExe" $source user32.lib shell32.lib /link /subsystem:windows
    }
    if ($built) { exit 0 }
}

if ($Architecture -eq 'x86') {
    $fallback = Join-Path $PSScriptRoot "OpenTaskbarWatchdog.exe"
    if ($NoFallback) {
        # A wrong-arch watchdog cannot restore taskbars on 32-bit Windows, so a
        # release/CI build must stop here instead of shipping a WOW64 binary.
        if (Test-Path -LiteralPath $fallback -PathType Leaf) {
            Write-Error "No 32-bit toolchain available (GCC -m32 or MSVC x86.x64); refusing to substitute the x64 watchdog with -NoFallback."
        } else {
            Write-Error "Could not compile an x86 watchdog binary and no fallback binary exists."
        }
        exit 1
    }
    # Legacy reference path only: an x64 watchdog still launches from an x86 process on
    # 64-bit Windows (WOW64). Degrade gracefully instead of breaking the release.
    if (Test-Path -LiteralPath $fallback -PathType Leaf) {
        Copy-Item -LiteralPath $fallback -Destination $outputExe -Force
        Write-Host "WARNING: no 32-bit toolchain available; using the x64 watchdog binary (WOW64 fallback)." -ForegroundColor Yellow
        exit 0
    }
}

Write-Error "Could not compile a $Architecture watchdog binary."
exit 1
