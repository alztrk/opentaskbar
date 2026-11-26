//! §C3 locale-contract tests: all 14 embedded catalogs must expose exactly the
//! same key set, including the C#-surplus Antigravity keys and the new feature
//! keys consumed by the Rust port. This is the CI-visible half of the key-parity
//! check; `scripts/check-locale-parity.ps1` performs the same check over source
//! JSON (C# vs Rust) without compiling.

use opentaskbar_core::i18n::{self, DEFAULT_LANGUAGE};

const LANGS: &[&str] = &[
    "de-DE", "en-US", "es-ES", "fr-FR", "it-IT", "ja-JP", "ko-KR", "nl-NL", "pl-PL", "pt-BR",
    "ru-RU", "tr-TR", "uk-UA", "zh-CN",
];

const EXPECTED_KEY_COUNT: usize = 1336;

const C3_NEW_KEYS: &[&str] = &[
    // The five keys C# has and the pre-port Rust catalogs lacked.
    "WidgetAiUsageAntigravity",
    "WidgetAiUsageAntigravityDescription",
    "AiUsageGroupGemini",
    "AiUsageGroupClaudeGpt",
    "AiUsageAntigravityGroupsLabel",
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
fn antigravity_values_are_real_translations_not_key_echoes() {
    assert_eq!(
        i18n::tr("en-US", "WidgetAiUsageAntigravity"),
        "Antigravity usage"
    );
    assert_eq!(
        i18n::tr("en-US", "AiUsageAntigravityGroupsLabel"),
        "Visible model groups"
    );
    // The Turkish value comes straight from the C# tr-TR catalog; we only assert
    // that it is a genuine translation rather than a key echo.
    let tr_value = i18n::tr("tr-TR", "WidgetAiUsageAntigravity");
    assert_ne!(tr_value, "WidgetAiUsageAntigravity");
    assert!(!tr_value.is_empty());
}

#[test]
fn missing_key_falls_back_to_the_key_itself_and_formatting_works() {
    assert_eq!(
        i18n::tr("de-DE", "DefinitelyMissingKey_12345"),
        "DefinitelyMissingKey_12345"
    );
    assert!(i18n::t_fmt("SeekTo", &["02:30"]).contains("02:30"));
}
