# OpenTaskbar Portable Single-File Publisher
# Builds a self-contained single-file executable for Windows x64.
param (
    [string]$OutputDir = "$PSScriptRoot\..\dist"
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
}

Write-Host "Publishing OpenTaskbar as self-contained portable executable..." -ForegroundColor Cyan

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
