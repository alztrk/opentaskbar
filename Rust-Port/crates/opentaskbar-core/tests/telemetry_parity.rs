//! C# telemetry/locale completeness parity. Complements `i18n_parity.rs` by
//! checking that every locale can actually *render* the AI-usage, speed-test,
//! focus and media labels emitted by the Rust widgets — a missing key would
//! otherwise surface as a raw key name in the flyout.

use opentaskbar_core::i18n;

const LANGS: &[&str] = &[
    "de-DE", "en-US", "es-ES", "fr-FR", "it-IT", "ja-JP", "ko-KR", "nl-NL", "pl-PL", "pt-BR",
    "ru-RU", "tr-TR", "uk-UA", "zh-CN",
];

const AI_KEYS: &[&str] = &[
    "WidgetAiUsage",
    "WidgetAiUsageClaude",
    "WidgetAiUsageCodex",
    "WidgetAiUsageCopilot",
    "WidgetAiUsageCursor",
    "WidgetAiUsageDeepSeek",
    "WidgetAiUsageOpenCode",
    "WidgetAiUsageOpenRouter",
    "AiUsageMonthly",
    "AiUsageRefresh",
    "AiUsageSettingsHeading",
];

const FEATURE_KEYS: &[&str] = &[
    "SpeedTestTitle",
    "SpeedTestStart",
    "SpeedTestDownload",
    "SpeedTestUpload",
    "SpeedTestResult",
    "FocusBreakTitle",
    "FocusBreakStart",
    "FocusSession",
    "RadioWifi",
    "RadioBluetooth",
    "FullBackupExport",
    "FullBackupInspect",
    "ThemeEditorTitle",
    "SeekTo",
    "SeekDuration",
    "HardwareUnavailable",
    "StartMenuToggle",
    "ProfileHiddenApps",
    "GpuEngine",
    "OpenLocation",
    "PlacementInWidgets",
    "DropdownSearchPlaceholder",
    "DropdownNoResults",
    "WeatherLocationUsed",
    // Window picker, magnification and Fjord labels.
    "WindowPickerTitle",
    "WindowPickerPrevious",
    "WindowPickerNext",
    "WindowPickerPage",
    "MagnificationExpandsDock",
    "MagnificationExpandsDockDescription",
    "FjordFiles",
    "FjordFilesOpen",
    "FjordBarShow",
    "FjordBarHide",
    // Wi-Fi flyout labels.
    "WifiScan",
    "WifiConnect",
    "WifiDisconnect",
    "WifiPassword",
    "WifiNoNetworks",
    "WifiConnectFailed",
    // Search, alignment, preview, focus and widget labels.
    "SearchNoResults",
    "ContentAlignStart",
    "ContentAlignCenter",
    "ContentAlignEnd",
    "PreviewDisabledHint",
    "FocusMinutesLabel",
    "FocusBreakMinutesLabel",
    "FocusSessionsLabel",
    "WidgetDensityCompact",
    "WidgetDensityFull",
    "AiUsageRingLabel",
    // Fjord / Wi-Fi / preview labels consumed by the ported UI.
    "Connected",
    "Copied",
    "CopyFailed",
    "ConnectionEstablished",
    "FjordFilesComputer",
    "FjordFilesCopyPath",
    "FjordFilesCount",
    "FjordFilesDesktop",
    "FjordFilesDocuments",
    "FjordFilesDownloads",
    "FjordFilesFilteredCount",
    "FjordFilesFolder",
    "FjordFilesHome",
    "FjordFilesMusic",
    "FjordFilesPictures",
    "FjordFilesPreview",
    "FjordFilesReveal",
    "FjordFilesSearch",
    "FjordFilesVideos",
    "FjordFilesVolumes",
    "Loading",
    "LocationDenied",
    "Missing",
    "OpenItemFailed",
    "OpenNetwork",
    "PdfPages",
    "PreviewFailed",
    "Revealed",
    "Saved",
    "SecuredNetwork",
    "SelectPreview",
    "TextLimit",
    "UnsupportedPreview",
    "WindowsError",
    "WifiNetworks",
    "WifiOff",
    "WlanStopped",
    "Wpa2Personal",
    "Wpa3Personal",
    "WpaPersonal",
];

#[test]
fn every_ai_usage_label_exists_in_every_locale() {
    for tag in LANGS {
        for key in AI_KEYS {
            assert_ne!(i18n::tr(tag, key), *key, "{tag} missing AI key {key}");
        }
    }
}

#[test]
fn every_new_feature_label_exists_in_every_locale() {
    for tag in LANGS {
        for key in FEATURE_KEYS {
            assert_ne!(i18n::tr(tag, key), *key, "{tag} missing feature key {key}");
        }
    }
}

#[test]
fn formatted_labels_keep_their_placeholders_replaceable() {
    for tag in LANGS {
        let formatted = i18n::tr(tag, "FocusBreakCountdown").replace("{0}", "15");
        // All 14 tags are valid, so the localized template must be substituted.
        assert!(formatted.contains("15"), "{tag}: {formatted}");
        assert!(!formatted.contains("{0}"), "{tag}: {formatted}");
    }
}

#[test]
fn locale_key_counts_stay_at_the_contract_value() {
    for tag in LANGS {
        assert_eq!(i18n::key_count(tag), 1406, "{tag} key count drifted");
    }
}

#[test]
fn unsupported_cultures_fall_back_without_panicking() {
    assert_eq!(i18n::tr("xx-YY", "Settings"), "Settings");
    assert_eq!(i18n::key_count("xx-YY"), 0);
    assert!(i18n::keys("xx-YY").is_empty());
}
