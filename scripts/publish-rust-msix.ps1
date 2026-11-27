# OpenTaskbar Rust Port MSIX Publisher
# Packages the Rust EXE with the same visual assets and capabilities as the C#
# MSIX build, using packaging/msix/AppxManifest.Rust.xml.
param (
    [ValidateSet('win-x64', 'win-x86')]
    [string]$Runtime = 'win-x64',
    [string]$OutputDir = "$PSScriptRoot\..\release\msix-rust",
    # No hardcoded version: the app release version comes from OpenTaskbar.csproj.
    [string]$Version = '',
    [switch]$SignForLocalTest
)

$ErrorActionPreference = 'Stop'
$arch = ($Runtime -split '-')[-1]
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Version) {
    [xml]$project = Get-Content -LiteralPath (Join-Path $repoRoot 'OpenTaskbar.csproj') -Raw
    $Version = ($project.Project.PropertyGroup.Version | Where-Object { $_ } | Select-Object -First 1).Trim()
}
$manifestTemplate = Join-Path $repoRoot 'packaging\msix\AppxManifest.Rust.xml'
$assetsDir = Join-Path $repoRoot 'packaging\msix\Assets'

if (-not (Test-Path -LiteralPath $manifestTemplate -PathType Leaf)) {
    throw "Missing Rust MSIX manifest: $manifestTemplate"
}

# Produce the portable Rust EXE first (it is the MSIX payload).
$portableDir = Join-Path $OutputDir "portable-$arch"
& (Join-Path $PSScriptRoot 'publish-rust-portable.ps1') -Runtime $Runtime -OutputDir $portableDir
if ($LASTEXITCODE -ne 0) { throw "Rust portable build failed for MSIX ($Runtime)." }
$exe = Join-Path $portableDir "OpenTaskbar-Rust-$arch.exe"

$makeAppx = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter 'makeappx.exe' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -like '*\x64\*' } |
    Sort-Object FullName |
    Select-Object -Last 1 -ExpandProperty FullName
if (-not $makeAppx) { throw "makeappx.exe not found. Install the Windows 10/11 SDK." }

if (-not (Test-Path -LiteralPath $OutputDir -PathType Container)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}
$stagingDir = Join-Path $OutputDir "staging-$arch"
if (Test-Path $stagingDir) { Remove-Item -LiteralPath $stagingDir -Recurse -Force }
New-Item -ItemType Directory -Path $stagingDir -Force | Out-Null

Copy-Item -LiteralPath $exe -Destination (Join-Path $stagingDir 'OpenTaskBar.exe') -Force
# The Rust armer prefers an app-directory watchdog over a %LOCALAPPDATA% copy,
# so the package must carry the architecture-correct PE next to the EXE.
$archWatchdog = Join-Path $portableDir "OpenTaskbarWatchdog-$arch.exe"
if (-not (Test-Path -LiteralPath $archWatchdog -PathType Leaf)) {
    throw "Missing $arch watchdog beside the portable EXE: $archWatchdog"
}
Copy-Item -LiteralPath $archWatchdog -Destination (Join-Path $stagingDir 'OpenTaskbarWatchdog.exe') -Force
Copy-Item -LiteralPath $assetsDir -Destination (Join-Path $stagingDir 'Assets') -Recurse -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'Rust-Port\README.md') -Destination (Join-Path $stagingDir 'Rust-Port-README.md') -Force

$parts = $Version.Split('.')
while ($parts.Length -lt 4) { $parts += '0' }
$msixVersion = ($parts[0..3]) -join '.'

[xml]$manifest = Get-Content -LiteralPath $manifestTemplate -Raw
$manifest.Package.Identity.Version = $msixVersion
$manifest.Package.Identity.ProcessorArchitecture = $arch
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[IO.File]::WriteAllText((Join-Path $stagingDir 'AppxManifest.xml'), $manifest.OuterXml, $utf8NoBom)

$msixPath = Join-Path $OutputDir "OpenTaskbarRust_${msixVersion}_${arch}.msix"
Write-Host "Packing Rust MSIX $msixVersion ($arch)..." -ForegroundColor Cyan
& $makeAppx pack /d $stagingDir /p $msixPath /o
if ($LASTEXITCODE -ne 0) { throw "makeappx pack failed with exit code $LASTEXITCODE" }

if ($SignForLocalTest) {
    $signTool = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter 'signtool.exe' -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -like '*\x64\*' } |
        Sort-Object FullName |
        Select-Object -Last 1 -ExpandProperty FullName
    if (-not $signTool) { throw "signtool.exe not found for local test signing." }
    $publisher = $manifest.Package.Identity.Publisher
    $cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $publisher } | Select-Object -First 1
    if (-not $cert) {
        $cert = New-SelfSignedCertificate -Type Custom -Subject $publisher -KeyUsage DigitalSignature `
            -FriendlyName "OpenTaskbar Rust Local MSIX Test" -CertStoreLocation "Cert:\CurrentUser\My" `
            -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
    }
    & $signTool sign /fd SHA256 /sha1 $cert.Thumbprint $msixPath
    if ($LASTEXITCODE -ne 0) { throw "signtool failed with exit code $LASTEXITCODE" }
    Write-Host "Signed local test Rust MSIX: $msixPath" -ForegroundColor Green
} else {
    Write-Host "Unsigned Store-ready Rust MSIX created: $msixPath" -ForegroundColor Green
}
