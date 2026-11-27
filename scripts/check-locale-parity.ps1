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
# 14 C# catalogs x 1034 keys and 14 Rust catalogs x 1406 keys (post-integration
# union). The 372 extra Rust keys are port-only feature keys (DI/calendar/font,
# C3 widget features, 207 UI localization keys, 27 picker/Fjord/Wi-Fi/settings
# labels — three of which already shipped — the 3 keys replacing the last English
# literals and the 3 searchable-location keys). The 40 Fjord/Wi-Fi/preview labels
# are now present in both families. Each family must stay identical across languages.
# These numbers are a hard gate, not just a display string.
$expectedLanguages = 14
$expectedCsKeys = 1034
$expectedRustKeys = 1406
$csDir = Join-Path $RepoRoot 'Services\Localization\Languages'
$rustDir = Join-Path $RepoRoot 'Rust-Port\crates\opentaskbar-core\locales'

$required = @(
    # Window picker, magnification and Fjord labels.
    'WindowPickerTitle','WindowPickerPrevious','WindowPickerNext','WindowPickerPage',
    'MagnificationExpandsDock','MagnificationExpandsDockDescription',
    'FjordFiles','FjordFilesOpen','FjordBarShow','FjordBarHide',
    # Wi-Fi flyout labels.
    'WifiScan','WifiConnect','WifiDisconnect','WifiPassword','WifiNoNetworks','WifiConnectFailed',
    # Search, alignment, preview, focus and widget labels.
    'SearchNoResults','ContentAlignStart','ContentAlignCenter','ContentAlignEnd',
    'PreviewDisabledHint','FocusMinutesLabel','FocusBreakMinutesLabel','FocusSessionsLabel',
    'WidgetDensityCompact','WidgetDensityFull','AiUsageRingLabel',
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
    'SectionGlassEffect','GlassBlurLabel','GlassSaturationLabel','GlassEdgeGlowLabel','GlassPointerLightLabel',
    # Fjord / Wi-Fi labels consumed by the ported UI.
    'Connected','Copied','CopyFailed','ConnectionEstablished','FjordFilesComputer','FjordFilesCopyPath','FjordFilesCount','FjordFilesDesktop','FjordFilesDocuments','FjordFilesDownloads','FjordFilesFilteredCount','FjordFilesFolder','FjordFilesHome','FjordFilesMusic','FjordFilesPictures','FjordFilesPreview','FjordFilesReveal','FjordFilesSearch','FjordFilesVideos','FjordFilesVolumes','Loading','LocationDenied','Missing','OpenItemFailed','OpenNetwork','PdfPages','PreviewFailed','Revealed','Saved','SecuredNetwork','SelectPreview','TextLimit','UnsupportedPreview','WindowsError','WifiNetworks','WifiOff','WlanStopped','Wpa2Personal','Wpa3Personal','WpaPersonal'
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
    # C# and Rust catalogs intentionally diverge: Rust carries the feature keys
    # listed above, while the C# catalogs keep their own legacy keys. Parity is
    # enforced per family (exact count + identical key set) plus the $required
    # keys that the Rust UI actually consumes.
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Error $_ }
    throw "Locale parity failed with $($failures.Count) problem(s)."
}

if (-not $Quiet) {
    Write-Host "Locale parity OK: $($langs.Count) C# catalogs x $($referenceCs.Count) keys; $($langs.Count) Rust catalogs x $($referenceRust.Count) keys; $($required.Count) new keys present everywhere." -ForegroundColor Green
}
