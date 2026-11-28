# Rust-only release version gate: tag <-> Cargo workspace version <-> Rust MSIX.
# Runs before any build in release.yml so a mismatched tag cannot produce a
# half-published release. There is no separate crate carve-out: the app version
# is the workspace version, which is the single source of truth.
param (
    [string]$Tag = '',
    [string]$RepoRoot = $PSScriptRoot,
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
if ((Split-Path -Leaf $RepoRoot) -eq 'scripts') { $RepoRoot = Join-Path $RepoRoot '..' }
$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
if (-not $Tag) { $Tag = $env:GITHUB_REF_NAME }
if (-not $Tag) { $Tag = $env:RELEASE_TAG }
if (-not $Tag) { throw 'No release tag supplied (-Tag, GITHUB_REF_NAME or RELEASE_TAG). Release CI supplies it; local runs must pass -Tag explicitly.' }

if ($Tag -notmatch '^v(?<base>\d+\.\d+\.\d+)-(?<channel>dev(?:\.\d+)?|stable)$') {
    throw "Tag must look like v0.3.5-dev, v0.3.5-dev.2 or v0.3.5-stable (got: $Tag)."
}
$tagBaseVersion = $Matches['base']
$tagChannel = $Matches['channel']
$expectedCargoVersion = "$tagBaseVersion-$tagChannel"

function Get-CargoWorkspaceVersion {
    param ([string]$Path)
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

$cargoPath = Join-Path $RepoRoot 'Cargo.toml'
$msixPath = Join-Path $RepoRoot 'packaging\msix\AppxManifest.Rust.xml'
foreach ($path in @($cargoPath, $msixPath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing version source: $path" }
}

# Cargo.toml is the only authoritative version: no fourth hardcoded copy.
$cargoVersion = Get-CargoWorkspaceVersion -Path $cargoPath
[xml]$manifest = Get-Content -Raw -LiteralPath $msixPath
$msixVersion = $manifest.Package.Identity.Version

$failures = [System.Collections.Generic.List[string]]::new()
if ($expectedCargoVersion -ne $cargoVersion) {
    $failures.Add("Tag version $expectedCargoVersion does not match Cargo workspace version $cargoVersion.")
}
if ($msixVersion -ne "$tagBaseVersion.0") {
    $failures.Add("Rust MSIX identity version $msixVersion != $tagBaseVersion.0")
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Error $_ }
    throw "Release version gate failed with $($failures.Count) problem(s)."
}

if (-not $Quiet) {
    Write-Host ("Version gate OK: tag={0} channel={1} cargo={2} rust-msix={3}" -f `
        $Tag, $tagChannel, $cargoVersion, $msixVersion) -ForegroundColor Green
}
