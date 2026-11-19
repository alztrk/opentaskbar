# OpenTaskbar Portable Single-File Publisher
# Builds a self-contained single-file executable for Windows x64.
param (
    [string]$OutputDir = "$PSScriptRoot\..\release"
)

$ErrorActionPreference = "Stop"

# Gracefully request running instance to close, allowing it to restore Windows taskbar cleanly
$running = Get-Process -Name OpenTaskbar -ErrorAction SilentlyContinue
if ($running) {
    Write-Host "Closing running OpenTaskbar instance gracefully..." -ForegroundColor Yellow
    foreach ($proc in $running) {
        $proc.CloseMainWindow() | Out-Null
    }
    $closed = $running | ForEach-Object { $_.WaitForExit(2500) }
    $stillRunning = Get-Process -Name OpenTaskbar -ErrorAction SilentlyContinue
    if ($stillRunning) {
        $stillRunning | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 300
    }

    # Ensure Windows native taskbar is restored if any instance was running
    $nativeTaskbarRestorer = @"
using System;
using System.Runtime.InteropServices;
public class PortableRestorer {
    [DllImport("user32.dll")] public static extern IntPtr FindWindow(string lpClassName, string lpWindowName);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr childAfter, string className, string title);
    public static void ShowAll() {
        var p = FindWindow("Shell_TrayWnd", null);
        if (p != IntPtr.Zero) ShowWindow(p, 5);
        var s = IntPtr.Zero;
        while ((s = FindWindowEx(IntPtr.Zero, s, "Shell_SecondaryTrayWnd", null)) != IntPtr.Zero) {
            ShowWindow(s, 5);
        }
    }
}
"@
    Add-Type -TypeDefinition $nativeTaskbarRestorer -ErrorAction SilentlyContinue
    [PortableRestorer]::ShowAll()
}

Write-Host "Publishing OpenTaskbar as self-contained portable executable..." -ForegroundColor Cyan

$watchdogScript = "$PSScriptRoot\..\tools\watchdog\build.ps1"
if (Test-Path $watchdogScript) {
    & powershell -ExecutionPolicy Bypass -File $watchdogScript
}

$projectPath = "$PSScriptRoot\..\OpenTaskbar.csproj"

dotnet publish $projectPath `
    -c Release `
    -r win-x64 `
    --self-contained true `
    -p:PublishSingleFile=true `
    -p:IncludeNativeLibrariesForSelfExtract=true `
    -p:PublishReadyToRun=false `
    -p:EnableCompressionInSingleFile=false `
    -o $OutputDir

if ($LASTEXITCODE -eq 0) {
    Write-Host "Publish succeeded! Portable executable is ready at: $OutputDir\OpenTaskbar.exe" -ForegroundColor Green
} else {
    Write-Error "Publish failed with exit code $LASTEXITCODE"
}
