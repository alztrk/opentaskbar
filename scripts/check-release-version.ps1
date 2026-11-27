# Rust-only release version gate: tag <-> Cargo workspace version <-> Rust MSIX.
# Runs before any build in release.yml so a mismatched tag cannot produce a
# half-published release. There is no separate crate carve-out: the app version
# is the workspace version.
param (
    [string]$Tag = '',
    [string]$RepoRoot = $PSScriptRoot,
    # Bump together with Cargo.toml, the Rust MSIX manifest and the release tag.
    [string]$ExpectedVersion = '0.4.0',
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
if ((Split-Path -Leaf $RepoRoot) -eq 'scripts') { $RepoRoot = Join-Path $RepoRoot '..' }
$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
if (-not $Tag) { $Tag = $env:GITHUB_REF_NAME }
if (-not $Tag) { $Tag = $env:RELEASE_TAG }
if (-not $Tag) { throw 'No release tag supplied (-Tag, GITHUB_REF_NAME or RELEASE_TAG).' }

if ($Tag -notmatch '^v(?<base>\d+\.\d+\.\d+)(?<pre>-[0-9A-Za-z.-]+)?$') {
    throw "Tag must look like v0.4.0 or v0.4.0-rc.1 (got: $Tag)."
}
$tagVersion = $Matches['base']

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

$cargoVersion = Get-CargoWorkspaceVersion -Path $cargoPath
[xml]$manifest = Get-Content -Raw -LiteralPath $msixPath
$msixVersion = $manifest.Package.Identity.Version

$failures = [System.Collections.Generic.List[string]]::new()
if ($tagVersion -ne $ExpectedVersion) {
    $failures.Add("Tag $Tag carries $tagVersion; expected $ExpectedVersion.")
}
if ($cargoVersion -ne $ExpectedVersion) {
    $failures.Add("Cargo.toml workspace version $cargoVersion != $ExpectedVersion.")
}
if ($tagVersion -ne $cargoVersion) {
    $failures.Add("Tag version $tagVersion does not match Cargo workspace version $cargoVersion.")
}
if ($msixVersion -ne "$ExpectedVersion.0") {
    $failures.Add("Rust MSIX identity version $msixVersion != $ExpectedVersion.0")
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Error $_ }
    throw "Release version gate failed with $($failures.Count) problem(s)."
}

if (-not $Quiet) {
    Write-Host ("Version gate OK: tag={0} cargo={1} rust-msix={2}" -f `
        $Tag, $cargoVersion, $msixVersion) -ForegroundColor Green
}
