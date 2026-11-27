# Locale key-parity gate (WS-H / §C3).
# Compares the 14 C# catalogs with the 14 embedded Rust catalogs and fails when
# key counts or key sets drift within a family, or when a C# key is absent from
# Rust. Mirrors the Rust integration test tests/i18n_parity.rs but needs no
# Rust toolchain, so CI can run it as a cheap preflight.
param (
    [string]$RepoRoot = "$PSScriptRoot\..",
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path

$langs = @('de-DE','en-US','es-ES','fr-FR','it-IT','ja-JP','ko-KR','nl-NL','pl-PL','pt-BR','ru-RU','tr-TR','uk-UA','zh-CN')
# 14 C# catalogs x 999 keys are mirrored by 14 Rust catalogs x 1342 keys; the
# 343 extra Rust keys are the port's feature keys (including the 207 new UI
# localization keys, the 3 keys replacing the last English literals and the 3
# searchable-location keys) and must stay identical across every language.
# These numbers are a hard gate, not just a display string.
$expectedLanguages = 14
$expectedCsKeys = 999
$expectedRustKeys = 1347
$csDir = Join-Path $RepoRoot 'Services\Localization\Languages'
$rustDir = Join-Path $RepoRoot 'Rust-Port\crates\opentaskbar-core\locales'

$required = @(
    # C# surplus keys (§C3).
    'WidgetAiUsageAntigravity','WidgetAiUsageAntigravityDescription',
    'AiUsageGroupGemini','AiUsageGroupClaudeGpt','AiUsageAntigravityGroupsLabel',
    # New Rust-port feature keys.
    'SpeedTestDownload','SpeedTestUpload','SpeedTestJitter','SpeedTestResult','SpeedTestRunning',
    'FocusBreakTitle','FocusBreakStart','FocusBreakSkip','FocusBreakCountdown','FocusBreakComplete','FocusSession',
    'RadioWifi','RadioBluetooth','RadioOn','RadioOff',
    'FullBackupInspect','FullBackupContents',
    'ThemeEditorTitle','ThemeEditorSave','ThemeEditorReset','ThemeEditorToken','ThemeEditorAdvanced','ThemeEditorAutosave',
    'SeekTo','SeekLabel','SeekDuration','SeekUnavailable',
    'HardwareUnavailable',
    'StartMenuOpen','StartMenuClose','StartMenuToggle',
    'ProfileHiddenApps',
    'PlacementInWidgets','OpenLocation','GpuEngine',
    'DropdownSearchPlaceholder','DropdownNoResults','WeatherLocationUsed',
    'SectionGlassEffect','GlassBlurLabel','GlassSaturationLabel','GlassEdgeGlowLabel','GlassPointerLightLabel'
)

function Get-Catalog([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Missing catalog: $Path" }
    $json = Get-Content -Raw -Encoding UTF8 -LiteralPath $Path | ConvertFrom-Json
    @($json.PSObject.Properties.Name)
}

$csSets = @{}
$rustSets = @{}
foreach ($lang in $langs) {
    $csSets[$lang] = Get-Catalog (Join-Path $csDir "$lang.json")
    $rustSets[$lang] = Get-Catalog (Join-Path $rustDir "$lang.json")
}

$referenceCs = @($csSets['en-US'] | Sort-Object)
$referenceRust = @($rustSets['en-US'] | Sort-Object)
$failures = [System.Collections.Generic.List[string]]::new()

if (@($langs).Count -ne $expectedLanguages) {
    $failures.Add("Expected $expectedLanguages catalogs, found $(@($langs).Count).")
}

foreach ($lang in $langs) {
    if (@($csSets[$lang]).Count -ne $expectedCsKeys) {
        $failures.Add("C# catalog $lang has $(@($csSets[$lang]).Count) keys; expected $expectedCsKeys.")
    }
    if (@($rustSets[$lang]).Count -ne $expectedRustKeys) {
        $failures.Add("Rust catalog $lang has $(@($rustSets[$lang]).Count) keys; expected $expectedRustKeys.")
    }
    if ((@($csSets[$lang] | Sort-Object) -join "`n") -ne ($referenceCs -join "`n")) {
        $failures.Add("C# key set differs for $lang")
    }
    if ((@($rustSets[$lang] | Sort-Object) -join "`n") -ne ($referenceRust -join "`n")) {
        $failures.Add("Rust key set differs for $lang")
    }
    foreach ($key in $required) {
        if ($rustSets[$lang] -notcontains $key) { $failures.Add("$lang missing required key $key") }
    }
    foreach ($key in $csSets[$lang]) {
        if ($rustSets[$lang] -notcontains $key) { $failures.Add("$lang missing C# key $key") }
    }
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Error $_ }
    throw "Locale parity failed with $($failures.Count) problem(s)."
}

if (-not $Quiet) {
    Write-Host "Locale parity OK: $($langs.Count) C# catalogs x $($referenceCs.Count) keys; $($langs.Count) Rust catalogs x $($referenceRust.Count) keys; $($required.Count) new keys present everywhere." -ForegroundColor Green
}
