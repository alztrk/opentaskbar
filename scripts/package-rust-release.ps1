# Packages the Rust portable EXEs into tagged release artifacts and merges their
# checksums + asset list into the files produced by scripts/package-release.ps1.
# Run AFTER package-release.ps1 so checksums.txt / release-manifest.json exist.
param (
    [Parameter(Mandatory = $true)]
    [string]$Tag,
    [string]$RepoRoot = '',
    [string]$OutputDir = '',
    [string]$Commit = '',
    [switch]$Prerelease
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression -ErrorAction SilentlyContinue

if (-not $RepoRoot) { $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path }
if (-not $OutputDir) { $OutputDir = Join-Path $RepoRoot 'release' }
$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
if (-not (Test-Path -LiteralPath $OutputDir -PathType Container)) {
    throw "Output directory does not exist: $OutputDir"
}
$OutputDir = (Resolve-Path -LiteralPath $OutputDir).Path

if ($Tag -notmatch '^v(?<base>\d+\.\d+\.\d+)(?<pre>-[0-9A-Za-z.-]+)?$') {
    throw "Tag must look like v0.3.2 or v0.3.2-rc.1 (got: $Tag)."
}
$version = $Matches['base']

function Get-FileSha256([string]$Path) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        ([BitConverter]::ToString($sha.ComputeHash([IO.File]::ReadAllBytes($Path))) -replace '-', '').ToLowerInvariant()
    } finally { $sha.Dispose() }
}
function Write-Utf8NoBom([string]$Path, [string]$Text) {
    [IO.File]::WriteAllText($Path, $Text + "`n", [Text.UTF8Encoding]::new($false))
}
function New-DeterministicZip([string]$Destination, $Entries) {
    if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Force }
    $stream = [IO.File]::Create($Destination)
    try {
        $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $false)
        try {
            $fixed = [DateTimeOffset]::new(1980, 1, 1, 0, 0, 0, [TimeSpan]::Zero)
            foreach ($name in $Entries.Keys) {
                $entry = $zip.CreateEntry($name, [IO.Compression.CompressionLevel]::Optimal)
                $entry.LastWriteTime = $fixed
                $target = $entry.Open()
                try {
                    $bytes = [IO.File]::ReadAllBytes($Entries[$name])
                    $target.Write($bytes, 0, $bytes.Length)
                } finally { $target.Dispose() }
            }
        } finally { $zip.Dispose() }
    } finally { $stream.Dispose() }
}

$rustReadme = Join-Path $RepoRoot 'Rust-Port\README.md'
$license = Join-Path $RepoRoot 'LICENSE'
foreach ($path in @($rustReadme, $license)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing packaging input: $path" }
}

# Remove only artifacts this script owns so repeated runs are reproducible.
Get-ChildItem -LiteralPath $OutputDir -File -Filter "OpenTaskbar-Rust-$Tag-*" | Remove-Item -Force

$newEntries = [System.Collections.Generic.List[object]]::new()
foreach ($arch in @('x64', 'x86')) {
    $builtExe = Join-Path $OutputDir "OpenTaskbar-Rust-$arch.exe"
    if (-not (Test-Path -LiteralPath $builtExe -PathType Leaf)) {
        throw "Missing Rust build output: $builtExe (run scripts/publish-rust-portable.ps1 first)."
    }

    $exeName = "OpenTaskbar-Rust-$Tag-win-$arch.exe"
    $zipName = "OpenTaskbar-Rust-$Tag-win-$arch.zip"
    $exePath = Join-Path $OutputDir $exeName
    $zipPath = Join-Path $OutputDir $zipName
    Copy-Item -LiteralPath $builtExe -Destination $exePath -Force

    $entries = [ordered]@{
        'OpenTaskBar.exe'  = $builtExe
        'Rust-Port/README.md' = $rustReadme
        'LICENSE'          = $license
    }
    New-DeterministicZip -Destination $zipPath -Entries $entries

    foreach ($item in @(
        @{ Path = $exePath; Name = $exeName; Kind = 'rust-executable' },
        @{ Path = $zipPath; Name = $zipName; Kind = 'rust-archive' }
    )) {
        $hash = Get-FileSha256 -Path $item.Path
        $newEntries.Add([ordered]@{
            name         = $item.Name
            architecture = $arch
            kind         = $item.Kind
            size         = (Get-Item -LiteralPath $item.Path).Length
            sha256       = $hash
            port         = 'rust'
        })
    }
}

# Merge checksums: keep C# entries, replace/append Rust entries, sort by file name.
$checksumsPath = Join-Path $OutputDir 'checksums.txt'
$checksums = [System.Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
if (Test-Path -LiteralPath $checksumsPath -PathType Leaf) {
    foreach ($line in (Get-Content -LiteralPath $checksumsPath)) {
        if ($line -match '^([0-9a-f]{64})\s+\*?(.+)$') {
            $checksums[$Matches[2].Trim()] = $Matches[1].ToLowerInvariant()
        }
    }
    # Drop rows whose artifact is not in this release directory: a stale
    # checksums.txt can carry C# rows from an older tag, and publishing them
    # next to the current assets breaks reproducibility checks.
    foreach ($name in @($checksums.Keys)) {
        if (-not (Test-Path -LiteralPath (Join-Path $OutputDir $name) -PathType Leaf)) {
            $checksums.Remove($name)
        }
    }
}
foreach ($entry in $newEntries) { $checksums[$entry.name] = $entry.sha256 }
$checksumLines = @($checksums.GetEnumerator() | Sort-Object Key | ForEach-Object { "$($_.Value) *$($_.Key)" })
Write-Utf8NoBom -Path $checksumsPath -Text ($checksumLines -join "`n")

# Merge manifest assets; keep the C# section untouched.
$manifestPath = Join-Path $OutputDir 'release-manifest.json'
$manifest = if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
    Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath | ConvertFrom-Json
} else {
    [ordered]@{
        schemaVersion = 1
        tag = $Tag
        version = $version
        prerelease = [bool]$Prerelease
        commit = $Commit
        checksumsFile = 'checksums.txt'
        assets = @()
    }
}
if ($manifest.PSObject.Properties.Name -notcontains 'assets') {
    $manifest | Add-Member -NotePropertyName assets -NotePropertyValue @()
}
$existing = @($manifest.assets | Where-Object { $_.port -ne 'rust' })
$allAssets = @($existing) + @($newEntries)
$manifest | Add-Member -NotePropertyName assets -NotePropertyValue $allAssets -Force
$manifest | Add-Member -NotePropertyName rustPort -NotePropertyValue ([ordered]@{
    manifest = 'Rust-Port/Cargo.toml'
    locales  = 14
    keyParity = $true
}) -Force
Write-Utf8NoBom -Path $manifestPath -Text ($manifest | ConvertTo-Json -Depth 8)

Write-Host "Packaged Rust port artifacts for $Tag ($($newEntries.Count) new assets)." -ForegroundColor Green
$newEntries | Select-Object name, sha256 | Format-Table -AutoSize
