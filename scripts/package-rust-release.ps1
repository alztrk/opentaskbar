# Packages the Rust portable EXEs into Rust-only tagged release artifacts.
# Generates checksums.txt, release-manifest.json (schemaVersion 2) and
# release-notes.md under OutputDir from scratch; no C# artifact is involved.
#
# Attestation notes: checksums.txt is the subject-checksums input of
# actions/attest@v4 (SLSA provenance); the optional VirusTotal step in
# release.yml is report-only. Both are recorded in release-manifest.json.
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
    throw "Tag must look like v0.4.0 or v0.4.0-rc.1 (got: $Tag)."
}
$version = $Matches['base']

function Get-FileSha256([string]$Path) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        ([BitConverter]::ToString($sha.ComputeHash([IO.File]::ReadAllBytes($Path))) -replace '-', '').ToLowerInvariant()
    } finally { $sha.Dispose() }
}
function Get-FileSha256Bytes([byte[]]$Bytes) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        ([BitConverter]::ToString($sha.ComputeHash($Bytes)) -replace '-', '').ToLowerInvariant()
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
function Assert-ZipEntryHash([string]$ZipPath, [string]$EntryName, [string]$ExpectedHash) {
    $stream = [IO.File]::OpenRead($ZipPath)
    try {
        $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Read)
        try {
            $entry = $zip.GetEntry($EntryName)
            if (-not $entry) { throw "ZIP $ZipPath is missing $EntryName" }
            $memory = [IO.MemoryStream]::new()
            try {
                $entry.Open().CopyTo($memory)
                $actual = Get-FileSha256Bytes -Bytes $memory.ToArray()
                if ($actual -ne $ExpectedHash) {
                    throw "ZIP integrity check failed for ${EntryName}: $actual != $ExpectedHash"
                }
            } finally { $memory.Dispose() }
        } finally { $zip.Dispose() }
    } finally { $stream.Dispose() }
}
function Get-PeMachine([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 64 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        throw "Not a valid PE executable: $Path"
    }
    $offset = [BitConverter]::ToInt32($bytes, 0x3C)
    if ($offset -le 0 -or ($offset + 6) -gt $bytes.Length -or
        [BitConverter]::ToUInt32($bytes, $offset) -ne 0x00004550) {
        throw "Missing PE signature: $Path"
    }
    return [BitConverter]::ToUInt16($bytes, $offset + 4)
}
function Get-CargoWorkspaceVersion([string]$Path) {
    $text = Get-Content -Raw -LiteralPath $Path
    $marker = '[workspace.package]'
    $start = $text.IndexOf($marker, [StringComparison]::Ordinal)
    if ($start -lt 0) { throw "Missing [workspace.package] in $Path" }
    $next = $text.IndexOf('[', $start + $marker.Length)
    $section = if ($next -gt $start) { $text.Substring($start, $next - $start) } else { $text.Substring($start) }
    $match = [regex]::Match($section, '(?m)^\s*version\s*=\s*"([^"]+)"')
    if (-not $match.Success) { throw "Missing workspace.package version in $Path" }
    return $match.Groups[1].Value
}
function Get-ReleaseNotes([string]$ChangelogPath, [string]$Version, [string]$TagName) {
    if (Test-Path -LiteralPath $ChangelogPath) {
        $lines = @(Get-Content -LiteralPath $ChangelogPath)
        foreach ($heading in @("^##\s+$([regex]::Escape($Version))(\s|$)", '^##\s+Unreleased(\s|$)')) {
            $start = -1
            for ($i = 0; $i -lt $lines.Count; $i++) {
                if ($lines[$i] -match $heading) { $start = $i; break }
            }
            if ($start -lt 0) { continue }

            $end = $lines.Count
            for ($j = $start + 1; $j -lt $lines.Count; $j++) {
                if ($lines[$j] -match '^##\s+') { $end = $j; break }
            }
            if ($end -le $start + 1) { continue }

            $section = @($lines[($start + 1)..($end - 1)])
            while ($section.Count -gt 0 -and [string]::IsNullOrWhiteSpace($section[0])) {
                $section = @($section[1..($section.Count - 1)])
            }
            while ($section.Count -gt 0 -and [string]::IsNullOrWhiteSpace($section[-1])) {
                $section = @($section[0..($section.Count - 2)])
            }
            if ($section.Count -gt 0) { return "## OpenTaskbar $TagName`n`n" + ($section -join "`n") }
        }
    }
    return "## OpenTaskbar $TagName`n`nNo changelog entry found for this version; see the commit history."
}

$rustReadme = Join-Path $RepoRoot 'README.md'
$license = Join-Path $RepoRoot 'LICENSE'
$cargoManifest = Join-Path $RepoRoot 'Cargo.toml'
$changelog = Join-Path $RepoRoot 'CHANGELOG.md'
foreach ($path in @($rustReadme, $license, $cargoManifest)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing packaging input: $path" }
}
$crateVersion = Get-CargoWorkspaceVersion -Path $cargoManifest

# Remove only artifacts this script owns so repeated runs are reproducible.
Get-ChildItem -LiteralPath $OutputDir -File -Filter "OpenTaskbar-Rust-$Tag-*" | Remove-Item -Force
foreach ($name in @('checksums.txt', 'release-manifest.json', 'release-notes.md')) {
    $path = Join-Path $OutputDir $name
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
}

$newEntries = [System.Collections.Generic.List[object]]::new()
$watchdogByArch = [ordered]@{}
foreach ($arch in @('x64', 'x86')) {
    $builtExe = Join-Path $OutputDir "OpenTaskbar-Rust-$arch.exe"
    if (-not (Test-Path -LiteralPath $builtExe -PathType Leaf)) {
        throw "Missing Rust build output: $builtExe (run scripts/publish-rust-portable.ps1 first)."
    }

    # The exact PE embedded by build.rs. A wrong-arch watchdog would silently
    # break crash recovery on 32-bit Windows, so fail the release instead.
    $watchdogPath = Join-Path $OutputDir "OpenTaskbarWatchdog-$arch.exe"
    if (-not (Test-Path -LiteralPath $watchdogPath -PathType Leaf)) {
        throw "Missing $arch watchdog: $watchdogPath (run scripts/publish-rust-portable.ps1 first)."
    }
    $expectedMachine = if ($arch -eq 'x86') { 0x014C } else { 0x8664 }
    $peMachine = Get-PeMachine -Path $watchdogPath
    if ($peMachine -ne $expectedMachine) {
        throw ("Watchdog for {0} has PE machine 0x{1:X4}, expected 0x{2:X4}." -f $arch, $peMachine, $expectedMachine)
    }
    $watchdogHash = Get-FileSha256 -Path $watchdogPath
    $watchdogByArch[$arch] = [ordered]@{
        architecture = $arch
        peMachine    = ('0x{0:X4}' -f $peMachine)
        sha256       = $watchdogHash
    }

    $exeName = "OpenTaskbar-Rust-$Tag-win-$arch.exe"
    $zipName = "OpenTaskbar-Rust-$Tag-win-$arch.zip"
    $exePath = Join-Path $OutputDir $exeName
    $zipPath = Join-Path $OutputDir $zipName
    Copy-Item -LiteralPath $builtExe -Destination $exePath -Force

    $entries = [ordered]@{
        'OpenTaskBar.exe' = $builtExe
        'README.md'       = $rustReadme
        'LICENSE'         = $license
    }
    New-DeterministicZip -Destination $zipPath -Entries $entries
    Assert-ZipEntryHash -ZipPath $zipPath -EntryName 'OpenTaskBar.exe' `
        -ExpectedHash (Get-FileSha256 -Path $builtExe)

    foreach ($item in @(
        @{ Path = $exePath; Name = $exeName; Kind = 'rust-executable' },
        @{ Path = $zipPath; Name = $zipName; Kind = 'rust-archive' }
    )) {
        $hash = Get-FileSha256 -Path $item.Path
        $newEntries.Add([ordered]@{
            name                 = $item.Name
            architecture         = $arch
            kind                 = $item.Kind
            size                 = (Get-Item -LiteralPath $item.Path).Length
            sha256               = $hash
            port                 = 'rust'
            watchdogArchitecture = $arch
            watchdogPeMachine    = ('0x{0:X4}' -f $peMachine)
            watchdogSha256       = $watchdogHash
        })
    }
}

# Generate checksums.txt from the Rust assets only; no C# rows can survive.
$checksumsPath = Join-Path $OutputDir 'checksums.txt'
$checksumLines = @($newEntries | Sort-Object name | ForEach-Object { "$($_.sha256) *$($_.name)" })
Write-Utf8NoBom -Path $checksumsPath -Text ($checksumLines -join "`n")

# Verify every checksum row against the bytes on disk before those hashes become
# SLSA attestation subjects in release.yml.
foreach ($line in $checksumLines) {
    if ($line -match '^([0-9a-f]{64}) \*(.+)$') {
        $name = $Matches[2]
        $path = Join-Path $OutputDir $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "checksums.txt references a missing artifact: $name"
        }
        $actual = Get-FileSha256 -Path $path
        if ($actual -ne $Matches[1]) {
            throw "Checksum mismatch for ${name}: $actual != $($Matches[1])"
        }
    }
}

$manifestPath = Join-Path $OutputDir 'release-manifest.json'
$manifest = [ordered]@{
    schemaVersion  = 2
    tag            = $Tag
    version        = $version
    prerelease     = [bool]$Prerelease
    commit         = $Commit
    checksumsFile  = 'checksums.txt'
    assets         = @($newEntries)
    rustPort       = [ordered]@{
        manifest     = 'Cargo.toml'
        crateVersion = $crateVersion
        locales      = 14
        keyParity    = $true
        watchdog     = $watchdogByArch
    }
    verification   = [ordered]@{
        checksums  = 'checksums.txt (verified against release artifacts before attestation)'
        slsa       = 'actions/attest@v4 Sigstore provenance; subjects from checksums.txt'
        virusTotal = 'optional report-only scan of release ZIPs (VIRUSTOTAL_API_KEY)'
    }
}
Write-Utf8NoBom -Path $manifestPath -Text ($manifest | ConvertTo-Json -Depth 8)

$notesPath = Join-Path $OutputDir 'release-notes.md'
Write-Utf8NoBom -Path $notesPath -Text (Get-ReleaseNotes -ChangelogPath $changelog -Version $version -TagName $Tag)

Write-Host "Packaged Rust-only release artifacts for $Tag ($($newEntries.Count) assets, crate $crateVersion)." -ForegroundColor Green
foreach ($entry in $newEntries) {
    Write-Host ("  {0} [{1}, watchdog {2}, sha256 {3}]" -f `
        $entry['name'], $entry['architecture'], $entry['watchdogPeMachine'], $entry['watchdogSha256'])
}
