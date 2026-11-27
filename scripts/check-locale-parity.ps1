# Locale key-parity gate (WS-H / §C3).
# Compares the 14 frozen reference catalogs at tests/reference/locales with
# the 14 embedded Rust catalogs and fails when key counts or key sets drift
# within a family. Mirrors the Rust integration test tests/i18n_parity.rs but
# needs no Rust toolchain, so CI can run it as a cheap preflight.
param (
    [string]$RepoRoot = "$PSScriptRoot\..",
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path

$langs = @('de-DE','en-US','es-ES','fr-FR','it-IT','ja-JP','ko-KR','nl-NL','pl-PL','pt-BR','ru-RU','tr-TR','uk-UA','zh-CN')
# Hard gate numbers after the shared-folder integration union, the removal of
# the retired AI-provider labels and the file-manager/top-bar/Wi-Fi panel cleanup: 14 frozen
# reference catalogs x 1046 keys and 14 Rust catalogs x 1375 keys. Rust keeps
# port-only feature keys (DI/calendar/font, C3 widget features, UI localization,
# window-picker/magnification/settings, OLED/display care and searchable
# location labels); the 50 file-manager/top-bar/Wi-Fi-panel keys were
# removed from all Rust catalogs.
#
# Each family must stay identical across languages. These numbers are a hard
# gate, not just a display string.
$expectedLanguages = 14
$expectedCsKeys = 1046
$expectedRustKeys = 1375
# The legacy source tree was removed in the Rust-only cleanup; these 14 catalogs
# are the frozen reference snapshot of the last legacy release, kept for key parity.
$csDir = Join-Path $RepoRoot 'tests\reference\locales'
$rustDir = Join-Path $RepoRoot 'crates\opentaskbar-core\locales'

$required = @(
    # Window picker and magnification labels.
    'WindowPickerTitle','WindowPickerPrevious','WindowPickerNext','WindowPickerPage',
    'MagnificationExpandsDock','MagnificationExpandsDockDescription',
    # Search, alignment, preview, focus and widget labels.
    'SearchNoResults','ContentAlignStart','ContentAlignCenter','ContentAlignEnd',
    'PreviewDisabledHint','FocusMinutesLabel','FocusBreakMinutesLabel','FocusSessionsLabel',
    'WidgetDensityCompact','WidgetDensityFull','AiUsageRingLabel',
    # New Rust workspace feature keys.
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
    # Folder-stack context menu labels.
    'Downloads','Documents',
    'DropdownSearchPlaceholder','DropdownNoResults','WeatherLocationUsed',
    'SectionGlassEffect','GlassBlurLabel','GlassSaturationLabel','GlassEdgeGlowLabel','GlassPointerLightLabel',
    # OLED and display care.
    'SectionDisplayCare','SurfaceTrueBlack','OledReduceGlow','OledReduceGlowHelp',
    'BurnInShift','BurnInShiftHelp','BurnInShiftInterval',
    'OledAutoTrigger','OledAutoTriggerHelp','OledLowBatteryThreshold',
    'OledAutoOnNightLight','OledAutoOnNightLightHelp',
    'AmbientMode','AmbientModeHelp','AnimationFpsCap','AnimationFpsCapHelp','FpsUnlimited'
)

function Get-Catalog([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Missing catalog: $Path" }
    $json = Get-Content -Raw -Encoding UTF8 -LiteralPath $Path | ConvertFrom-Json
    @($json.PSObject.Properties.Name)
}

function Get-CatalogValues([string]$Path) {
    $json = Get-Content -Raw -Encoding UTF8 -LiteralPath $Path | ConvertFrom-Json
    $values = @{}
    foreach ($property in $json.PSObject.Properties) { $values[$property.Name] = [string]$property.Value }
    $values
}

# Translation-quality gate (no mass edits): high-visibility labels that are already
# translated in all 13 non-English catalogs must never regress to the en-US text.
# Legacy debt (including the Rust-only C3 feature labels) is bounded by the budget below.
$highVisibilityKeys = @(
    'Settings','WindowPickerTitle','WindowPickerPrevious','WindowPickerNext',
    'WindowPickerPage','MagnificationExpandsDock','MagnificationExpandsDockDescription','SearchNoResults',
    'ContentAlignCenter','ContentAlignEnd','PreviewDisabledHint','FocusMinutesLabel',
    'FocusBreakMinutesLabel','WidgetDensityCompact','WidgetDensityFull','AiUsageRingLabel',
    'SectionGlassEffect','GlassBlurLabel','GlassSaturationLabel','GlassEdgeGlowLabel',
    'GlassPointerLightLabel','SectionDisplayCare','SurfaceTrueBlack','OledReduceGlow',
    'OledReduceGlowHelp','BurnInShift','BurnInShiftHelp','BurnInShiftInterval',
    'OledAutoTrigger','OledAutoTriggerHelp','OledLowBatteryThreshold','OledAutoOnNightLight',
    'OledAutoOnNightLightHelp','AmbientMode','AmbientModeHelp','AnimationFpsCap',
    'AnimationFpsCapHelp','FpsUnlimited','PlacementInWidgets','OpenLocation',
    'GpuEngine','DropdownSearchPlaceholder','DropdownNoResults','WeatherLocationUsed'
)
$englishIdenticalBudget = @{
    'de-DE'=203; 'es-ES'=232; 'fr-FR'=252; 'it-IT'=242; 'ja-JP'=221; 'ko-KR'=220;
    'nl-NL'=260; 'pl-PL'=232; 'pt-BR'=241; 'ru-RU'=211; 'tr-TR'=21; 'uk-UA'=210; 'zh-CN'=218
}

$csSets = @{}
$rustSets = @{}
$rustValues = @{}
foreach ($lang in $langs) {
    $csSets[$lang] = Get-Catalog (Join-Path $csDir "$lang.json")
    $rustSets[$lang] = Get-Catalog (Join-Path $rustDir "$lang.json")
    $rustValues[$lang] = Get-CatalogValues (Join-Path $rustDir "$lang.json")
}

$referenceCs = @($csSets['en-US'] | Sort-Object)
$referenceRust = @($rustSets['en-US'] | Sort-Object)
$failures = [System.Collections.Generic.List[string]]::new()

if (@($langs).Count -ne $expectedLanguages) {
    $failures.Add("Expected $expectedLanguages catalogs, found $(@($langs).Count).")
}

foreach ($lang in $langs) {
    if (@($csSets[$lang]).Count -ne $expectedCsKeys) {
        $failures.Add("Frozen reference catalog $lang has $(@($csSets[$lang]).Count) keys; expected $expectedCsKeys.")
    }
    if (@($rustSets[$lang]).Count -ne $expectedRustKeys) {
        $failures.Add("Rust catalog $lang has $(@($rustSets[$lang]).Count) keys; expected $expectedRustKeys.")
    }
    if ((@($csSets[$lang] | Sort-Object) -join "`n") -ne ($referenceCs -join "`n")) {
        $failures.Add("Frozen reference key set differs for $lang")
    }
    if ((@($rustSets[$lang] | Sort-Object) -join "`n") -ne ($referenceRust -join "`n")) {
        $failures.Add("Rust key set differs for $lang")
    }
    foreach ($key in $required) {
        if ($rustSets[$lang] -notcontains $key) { $failures.Add("$lang missing required key $key") }
    }
    if ($lang -ne 'en-US') {
        foreach ($key in $highVisibilityKeys) {
            if ($rustValues[$lang][$key] -ceq $rustValues['en-US'][$key]) {
                $failures.Add("$lang high-visibility key $key regressed to the en-US text")
            }
        }
        $identical = 0
        foreach ($key in $rustValues['en-US'].Keys) {
            if ($rustValues[$lang].ContainsKey($key) -and $rustValues[$lang][$key] -ceq $rustValues['en-US'][$key]) {
                $identical++
            }
        }
        if ($identical -gt $englishIdenticalBudget[$lang]) {
            $failures.Add("$lang has $identical en-US-identical values; budget is $($englishIdenticalBudget[$lang])")
        }
    }
    # reference and Rust catalogs intentionally diverge: Rust carries the feature keys
    # listed above, while the frozen reference catalogs keep their own legacy keys. Parity is
    # enforced per family (exact count + identical key set) plus the $required
    # keys that the Rust UI actually consumes.
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Error $_ }
    throw "Locale parity failed with $($failures.Count) problem(s)."
}

if (-not $Quiet) {
    Write-Host "Locale parity OK: $($langs.Count) frozen reference catalogs x $($referenceCs.Count) keys; $($langs.Count) Rust catalogs x $($referenceRust.Count) keys; $($required.Count) required keys present everywhere; $($highVisibilityKeys.Count) high-visibility labels translated." -ForegroundColor Green
}
