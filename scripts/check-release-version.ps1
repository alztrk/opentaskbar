# Release version gate: tag <-> OpenTaskbar.csproj <-> Rust crate <-> Rust MSIX.
# Runs before any build in release.yml so a mismatched tag cannot produce a
# half-published release. The Rust crate deliberately stays at 0.1.0 (its own
# registry version); that carve-out is registered explicitly below.
param (
    [string]$Tag = '',
    [string]$RepoRoot = $PSScriptRoot,
    # The application version this gate was written for. Bump together with the
    # csproj version, the Rust MSIX manifest and the release tag.
    [string]$ExpectedVersion = '0.3.5',
    # Explicitly allowed crate versions that do not track the app release.
    [string[]]$AllowedCrateVersions = @('0.1.0'),
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
if ((Split-Path -Leaf $RepoRoot) -eq 'scripts') { $RepoRoot = Join-Path $RepoRoot '..' }
$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
if (-not $Tag) { $Tag = $env:GITHUB_REF_NAME }
if (-not $Tag) { $Tag = $env:RELEASE_TAG }
if (-not $Tag) { throw 'No release tag supplied (-Tag, GITHUB_REF_NAME or RELEASE_TAG).' }

if ($Tag -notmatch '^v(?<base>\d+\.\d+\.\d+)(?<pre>-[0-9A-Za-z.-]+)?$') {
    throw "Tag must look like v0.3.5 or v0.3.5-rc.1 (got: $Tag)."
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

$projectPath = Join-Path $RepoRoot 'OpenTaskbar.csproj'
$cargoPath = Join-Path $RepoRoot 'Rust-Port\Cargo.toml'
$msixPath = Join-Path $RepoRoot 'packaging\msix\AppxManifest.Rust.xml'
foreach ($path in @($projectPath, $cargoPath, $msixPath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing version source: $path" }
}

[xml]$project = Get-Content -Raw -LiteralPath $projectPath
$projectVersion = ($project.Project.PropertyGroup.Version | Where-Object { $_ } | Select-Object -First 1).Trim()
$cargoVersion = Get-CargoWorkspaceVersion -Path $cargoPath
[xml]$manifest = Get-Content -Raw -LiteralPath $msixPath
$msixVersion = $manifest.Package.Identity.Version

$failures = [System.Collections.Generic.List[string]]::new()
if ($tagVersion -ne $ExpectedVersion) {
    $failures.Add("Tag $Tag carries $tagVersion; expected $ExpectedVersion.")
}
if ($projectVersion -ne $ExpectedVersion) {
    $failures.Add("OpenTaskbar.csproj version $projectVersion != $ExpectedVersion.")
}
if ($tagVersion -ne $projectVersion) {
    $failures.Add("Tag version $tagVersion does not match csproj version $projectVersion.")
}
$allowedCrateVersions = @($ExpectedVersion) + @($AllowedCrateVersions)
if ($cargoVersion -notin $allowedCrateVersions) {
    $failures.Add("Rust-Port/Cargo.toml version $cargoVersion is neither $ExpectedVersion nor a registered carve-out ($($AllowedCrateVersions -join ', ')).")
}
if ($msixVersion -ne "$ExpectedVersion.0") {
    $failures.Add("Rust MSIX identity version $msixVersion != $ExpectedVersion.0")
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Error $_ }
    throw "Release version gate failed with $($failures.Count) problem(s)."
}

if (-not $Quiet) {
    $crateNote = if ($cargoVersion -eq $ExpectedVersion) { 'matches release' } else { 'registered carve-out' }
    Write-Host ("Version gate OK: tag={0} csproj={1} crate={2} ({3}) rust-msix={4}" -f `
        $Tag, $projectVersion, $cargoVersion, $crateNote, $msixVersion) -ForegroundColor Green
}
