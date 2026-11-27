//! §C3 locale-contract tests: all 14 embedded catalogs must expose exactly the
//! same key set, including the new feature keys consumed by the Rust port.
//! This is the CI-visible half of the key-parity
//! check; `scripts/check-locale-parity.ps1` performs the same check over source
//! JSON (reference vs Rust) without compiling.

use opentaskbar_core::i18n::{self, DEFAULT_LANGUAGE};

const LANGS: &[&str] = &[
    "de-DE", "en-US", "es-ES", "fr-FR", "it-IT", "ja-JP", "ko-KR", "nl-NL", "pl-PL", "pt-BR",
    "ru-RU", "tr-TR", "uk-UA", "zh-CN",
];

const EXPECTED_KEY_COUNT: usize = 1378;

const C3_NEW_KEYS: &[&str] = &[
    // Window picker and magnification labels.
    "WindowPickerTitle",
    "WindowPickerPrevious",
    "WindowPickerNext",
    "WindowPickerPage",
    "MagnificationExpandsDock",
    "MagnificationExpandsDockDescription",
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
    // New feature keys (C3 prefixes).
    "SpeedTestDownload",
    "SpeedTestUpload",
    "SpeedTestJitter",
    "SpeedTestResult",
    "SpeedTestRunning",
    "FocusBreakTitle",
    "FocusBreakStart",
    "FocusBreakSkip",
    "FocusBreakCountdown",
    "FocusBreakComplete",
    "FocusSession",
    "RadioWifi",
    "RadioBluetooth",
    "RadioOn",
    "RadioOff",
    "FullBackupInspect",
    "FullBackupContents",
    "ThemeEditorTitle",
    "ThemeEditorSave",
    "ThemeEditorReset",
    "ThemeEditorToken",
    "ThemeEditorAdvanced",
    "ThemeEditorAutosave",
    "SeekTo",
    "SeekLabel",
    "SeekDuration",
    "SeekUnavailable",
    "HardwareUnavailable",
    "StartMenuOpen",
    "StartMenuClose",
    "StartMenuToggle",
    "ProfileHiddenApps",
    // Replacements for the remaining user-visible English literals.
    "PlacementInWidgets",
    "OpenLocation",
    "GpuEngine",
    // Searchable country/city pickers and automatic geolocation status.
    "DropdownSearchPlaceholder",
    "DropdownNoResults",
    "WeatherLocationUsed",
    // Clear Crystal optical glass settings.
    "SectionGlassEffect",
    "GlassBlurLabel",
    "GlassSaturationLabel",
    "GlassEdgeGlowLabel",
    "GlassPointerLightLabel",
    // OLED and display care.
    "SectionDisplayCare",
    "SurfaceTrueBlack",
    "OledReduceGlow",
    "OledReduceGlowHelp",
    "BurnInShift",
    "BurnInShiftHelp",
    "BurnInShiftInterval",
    "OledAutoTrigger",
    "OledAutoTriggerHelp",
    "OledLowBatteryThreshold",
    "OledAutoOnNightLight",
    "OledAutoOnNightLightHelp",
    "AmbientMode",
    "AmbientModeHelp",
    "AnimationFpsCap",
    "AnimationFpsCapHelp",
    "FpsUnlimited",
];

#[test]
fn fourteen_languages_are_supported() {
    assert_eq!(LANGS.len(), 14);
    for tag in LANGS {
        assert!(i18n::is_supported(tag), "{tag} is not supported");
        assert!(i18n::key_count(tag) > 0, "{tag} has an empty catalog");
    }
    assert!(!i18n::is_supported("xx-YY"));
}

#[test]
fn every_catalog_has_the_same_key_count() {
    for tag in LANGS {
        assert_eq!(
            i18n::key_count(tag),
            EXPECTED_KEY_COUNT,
            "{tag} key count drifted",
        );
    }
}

#[test]
fn every_catalog_has_the_identical_key_set() {
    let reference = i18n::keys(DEFAULT_LANGUAGE);
    assert_eq!(reference.len(), EXPECTED_KEY_COUNT);
    for tag in LANGS {
        assert_eq!(
            i18n::keys(tag),
            reference,
            "{tag} key set differs from {DEFAULT_LANGUAGE}",
        );
    }
}

#[test]
fn c3_new_keys_resolve_in_all_languages() {
    for tag in LANGS {
        for key in C3_NEW_KEYS {
            assert_ne!(i18n::tr(tag, key), *key, "{tag} is missing C3 key {key}");
        }
    }
}

#[test]
fn missing_key_falls_back_to_the_key_itself_and_formatting_works() {
    assert_eq!(
        i18n::tr("de-DE", "DefinitelyMissingKey_12345"),
        "DefinitelyMissingKey_12345"
    );
    assert!(i18n::t_fmt("SeekTo", &["02:30"]).contains("02:30"));
}

/// The C3 merge overwrote nine genuine tr-TR labels with en-US (or ASCII-only)
/// text. Keep the Turkish values so a later merge cannot silently regress them.
#[test]
fn turkish_focus_and_radio_labels_stay_translated() {
    const TURKISH: &[(&str, &str)] = &[
        ("FocusBreakTitle", "Mola zamanı"),
        ("FocusBreakStart", "Molayı başlat"),
        ("FocusBreakSkip", "Molayı atla"),
        ("FocusBreakCountdown", "Mola {0} sn içinde başlıyor"),
        ("FocusBreakComplete", "Mola tamamlandı"),
        ("FocusSession", "Oturum {0}"),
        ("RadioOn", "Açık"),
        ("RadioOff", "Kapalı"),
        ("DockBottomGap", "Alt boşluk"),
    ];
    for (key, expected) in TURKISH {
        assert_eq!(
            i18n::tr("tr-TR", key),
            *expected,
            "{key} regressed to the {DEFAULT_LANGUAGE} value",
        );
        assert_ne!(
            i18n::tr("tr-TR", key),
            i18n::tr(DEFAULT_LANGUAGE, key),
            "{key} is no longer translated",
        );
    }
}

fn placeholders(value: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            break;
        };
        let token = &after[..=end];
        if token[1..token.len() - 1].parse::<usize>().is_ok() {
            found.push(token);
        }
        rest = &after[end + 1..];
    }
    // Translations may reorder arguments (e.g. TerminatePrompt); only the
    // multiset of placeholders must survive.
    found.sort_unstable();
    found
}

/// User-visible labels that are already translated in every non-English catalog. They must
/// never regress to the en-US text (a C3-style catalog merge once did exactly that).
const TRANSLATED_EVERYWHERE: &[&str] = &[
    "Settings",
    "WindowPickerTitle",
    "WindowPickerPrevious",
    "WindowPickerNext",
    "WindowPickerPage",
    "MagnificationExpandsDock",
    "MagnificationExpandsDockDescription",
    "SearchNoResults",
    "ContentAlignCenter",
    "ContentAlignEnd",
    "PreviewDisabledHint",
    "FocusMinutesLabel",
    "FocusBreakMinutesLabel",
    "WidgetDensityCompact",
    "WidgetDensityFull",
    "AiUsageRingLabel",
    "SectionGlassEffect",
    "GlassBlurLabel",
    "GlassSaturationLabel",
    "GlassEdgeGlowLabel",
    "GlassPointerLightLabel",
    "SectionDisplayCare",
    "SurfaceTrueBlack",
    "OledReduceGlow",
    "OledReduceGlowHelp",
    "BurnInShift",
    "BurnInShiftHelp",
    "BurnInShiftInterval",
    "OledAutoTrigger",
    "OledAutoTriggerHelp",
    "OledLowBatteryThreshold",
    "OledAutoOnNightLight",
    "OledAutoOnNightLightHelp",
    "AmbientMode",
    "AmbientModeHelp",
    "AnimationFpsCap",
    "AnimationFpsCapHelp",
    "FpsUnlimited",
    "PlacementInWidgets",
    "OpenLocation",
    "GpuEngine",
    "DropdownSearchPlaceholder",
    "DropdownNoResults",
    "WeatherLocationUsed",
];

/// Baseline number of values that are byte-identical to en-US in each catalog. This is a
/// regression budget, not a translation mandate: legacy debt keys (notably the Rust-only C3
/// feature labels) are intentionally unchanged here. A mass overwrite with English text raises
/// the count and fails the gate; adding a real translation only lowers it.
const ENGLISH_IDENTICAL_BUDGET: &[(&str, usize)] = &[
    ("de-DE", 203),
    ("es-ES", 232),
    ("fr-FR", 252),
    ("it-IT", 242),
    ("ja-JP", 221),
    ("ko-KR", 220),
    ("nl-NL", 260),
    ("pl-PL", 232),
    ("pt-BR", 241),
    ("ru-RU", 211),
    ("tr-TR", 21),
    ("uk-UA", 210),
    ("zh-CN", 218),
];

#[test]
fn high_visibility_labels_stay_translated_in_every_language() {
    for tag in LANGS {
        if *tag == DEFAULT_LANGUAGE {
            continue;
        }
        for key in TRANSLATED_EVERYWHERE {
            assert_ne!(
                i18n::tr(tag, key),
                i18n::tr(DEFAULT_LANGUAGE, key),
                "{tag}/{key} regressed to the en-US text",
            );
        }
    }
}

#[test]
fn no_catalog_regresses_to_more_english_identical_values() {
    for (tag, budget) in ENGLISH_IDENTICAL_BUDGET {
        let identical = i18n::keys(DEFAULT_LANGUAGE)
            .iter()
            .filter(|key| i18n::tr(tag, key) == i18n::tr(DEFAULT_LANGUAGE, key))
            .count();
        assert!(
            identical <= *budget,
            "{tag} has {identical} en-US-identical values (budget {budget})",
        );
    }
}

#[test]
fn no_catalog_has_empty_values_or_lost_placeholders() {
    for tag in LANGS {
        for key in i18n::keys(DEFAULT_LANGUAGE) {
            let value = i18n::tr(tag, &key);
            assert!(!value.trim().is_empty(), "{tag}/{key} is empty");
            assert_eq!(
                placeholders(&value),
                placeholders(&i18n::tr(DEFAULT_LANGUAGE, &key)),
                "{tag}/{key} lost or gained a placeholder",
            );
        }
    }
}
