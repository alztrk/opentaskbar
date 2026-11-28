# OpenTaskbar MSIX Publisher (Rust only)
# Packages the Rust EXE and its architecture-matched watchdog with the visual
# assets and capabilities declared in packaging/msix/AppxManifest.Rust.xml.
param (
    [ValidateSet('win-x64', 'win-x86')]
    [string]$Runtime = 'win-x64',
    [string]$OutputDir = "$PSScriptRoot\..\artifacts\msix-rust",
    # No hardcoded version: the app release version comes from Cargo.toml.
    [string]$Version = '',
    [switch]$SignForLocalTest
)

$ErrorActionPreference = 'Stop'
$arch = ($Runtime -split '-')[-1]
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Version) {
    $cargoPath = Join-Path $repoRoot 'Cargo.toml'
    $text = Get-Content -Raw -LiteralPath $cargoPath
    $marker = '[workspace.package]'
    $start = $text.IndexOf($marker, [StringComparison]::Ordinal)
    if ($start -lt 0) { throw "Missing [workspace.package] in $cargoPath" }
    $next = $text.IndexOf('[', $start + $marker.Length)
    $section = if ($next -gt $start) { $text.Substring($start, $next - $start) } else { $text.Substring($start) }
    $match = [regex]::Match($section, '(?m)^\s*version\s*=\s*"([^"]+)"')
    if (-not $match.Success) { throw "Missing workspace.package version in $cargoPath" }
    $Version = $match.Groups[1].Value
}
$manifestTemplate = Join-Path $repoRoot 'packaging\msix\AppxManifest.Rust.xml'
$assetsDir = Join-Path $repoRoot 'packaging\msix\Assets'

if (-not (Test-Path -LiteralPath $manifestTemplate -PathType Leaf)) {
    throw "Missing Rust MSIX manifest: $manifestTemplate"
}

# Produce the portable Rust EXE first (it is the MSIX payload).
$portableDir = Join-Path $OutputDir "portable-$arch"
& (Join-Path $PSScriptRoot 'publish-portable.ps1') -Runtime $Runtime -OutputDir $portableDir
if ($LASTEXITCODE -ne 0) { throw "Rust portable build failed for MSIX ($Runtime)." }
$exe = Join-Path $portableDir "OpenTaskbar-$arch.exe"

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
Copy-Item -LiteralPath $assetsDir -Destination (Join-Path $stagingDir 'Assets') -Recurse -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'README.md') -Destination (Join-Path $stagingDir 'README.md') -Force

$parts = $Version.Split('.')
while ($parts.Length -lt 4) { $parts += '0' }
$msixVersion = ($parts[0..3]) -join '.'

[xml]$manifest = Get-Content -LiteralPath $manifestTemplate -Raw
$manifest.Package.Identity.Version = $msixVersion
$manifest.Package.Identity.ProcessorArchitecture = $arch
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[IO.File]::WriteAllText((Join-Path $stagingDir 'AppxManifest.xml'), $manifest.OuterXml, $utf8NoBom)

$msixPath = Join-Path $OutputDir "OpenTaskbar_${msixVersion}_${arch}.msix"
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
