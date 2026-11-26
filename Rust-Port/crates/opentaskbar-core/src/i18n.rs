//! Embedded UI localization catalogs (14 languages, C# `LanguageCatalog` parity).
//!
//! The JSON files live in `crates/opentaskbar-core/locales` and are compiled into
//! the binary, so no loose resource files are needed at runtime. Lookup order
//! mirrors `LanguageCatalog.Get`: exact culture, other cultures with the same
//! two-letter language code, then `en-US`, then the key itself. Product names,
//! keyboard shortcuts, URI schemes and OS identifiers stay untranslated.
//!
//! The catalogs intentionally mirror the C# `Services/Localization/Languages` files.
//! Not every key is consumed by the Rust dock yet (theme packs, launcher, C#-only
//! dialogs, etc.); unused keys are kept for parity and for the en-US fallback chain,
//! so they must not be pruned just because no Rust call site references them.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

/// BCP 47 tag and native display name, in the order shown by the language picker.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("de-DE", "Deutsch"),
    ("en-US", "English"),
    ("es-ES", "Español"),
    ("fr-FR", "Français"),
    ("it-IT", "Italiano"),
    ("ja-JP", "日本語"),
    ("ko-KR", "한국어"),
    ("nl-NL", "Nederlands"),
    ("pl-PL", "Polski"),
    ("pt-BR", "Português (Brasil)"),
    ("ru-RU", "Русский"),
    ("tr-TR", "Türkçe"),
    ("uk-UA", "Українська"),
    ("zh-CN", "中文(简体)"),
];

const CATALOG_SOURCES: &[(&str, &str)] = &[
    ("de-DE", include_str!("../locales/de-DE.json")),
    ("en-US", include_str!("../locales/en-US.json")),
    ("es-ES", include_str!("../locales/es-ES.json")),
    ("fr-FR", include_str!("../locales/fr-FR.json")),
    ("it-IT", include_str!("../locales/it-IT.json")),
    ("ja-JP", include_str!("../locales/ja-JP.json")),
    ("ko-KR", include_str!("../locales/ko-KR.json")),
    ("nl-NL", include_str!("../locales/nl-NL.json")),
    ("pl-PL", include_str!("../locales/pl-PL.json")),
    ("pt-BR", include_str!("../locales/pt-BR.json")),
    ("ru-RU", include_str!("../locales/ru-RU.json")),
    ("tr-TR", include_str!("../locales/tr-TR.json")),
    ("uk-UA", include_str!("../locales/uk-UA.json")),
    ("zh-CN", include_str!("../locales/zh-CN.json")),
];

pub const DEFAULT_LANGUAGE: &str = "en-US";

type Catalog = HashMap<String, String>;

struct Catalogs {
    /// Parsed on demand. Keeping all 14 catalogs alive costs ~2–3 MiB of heap for a
    /// process that only ever displays one language.
    by_culture: RwLock<HashMap<&'static str, Arc<Catalog>>>,
}

static CATALOGS: OnceLock<Catalogs> = OnceLock::new();
static CURRENT: OnceLock<RwLock<&'static str>> = OnceLock::new();

fn catalogs() -> &'static Catalogs {
    CATALOGS.get_or_init(|| Catalogs {
        by_culture: RwLock::new(HashMap::with_capacity(2)),
    })
}

fn catalog_for(culture: &str) -> Option<Arc<Catalog>> {
    let tag = canonical(culture)?;
    let catalogs = catalogs();
    if let Some(catalog) = catalogs.by_culture.read().ok()?.get(tag) {
        return Some(Arc::clone(catalog));
    }
    let (_, source) = CATALOG_SOURCES
        .iter()
        .find(|(candidate, _)| *candidate == tag)?;
    let parsed: Catalog = match serde_json::from_str(source) {
        Ok(catalog) => catalog,
        Err(error) => {
            // A broken catalog must never take down the dock; callers fall back to en-US.
            if tag == DEFAULT_LANGUAGE {
                eprintln!("localization: invalid en-US catalog: {error}");
            }
            return None;
        }
    };
    let mut by_culture = catalogs.by_culture.write().ok()?;
    let entry = by_culture.entry(tag).or_insert_with(|| Arc::new(parsed));
    Some(Arc::clone(entry))
}

fn current_slot() -> &'static RwLock<&'static str> {
    CURRENT.get_or_init(|| RwLock::new(DEFAULT_LANGUAGE))
}

fn two_letter(culture: &str) -> &str {
    culture.split(['-', '_']).next().unwrap_or(culture)
}

fn canonical(culture: &str) -> Option<&'static str> {
    let needle = culture.trim();
    LANGUAGES
        .iter()
        .find(|(tag, _)| tag.eq_ignore_ascii_case(needle))
        .map(|(tag, _)| *tag)
}

/// True when a catalog exists for this BCP 47 tag (case insensitive).
pub fn is_supported(culture: &str) -> bool {
    canonical(culture).is_some()
}

/// The currently active BCP 47 tag, e.g. `"tr-TR"`.
pub fn language() -> &'static str {
    *current_slot().read().expect("localization lock poisoned")
}

/// Switch the UI language. Returns false for unsupported tags.
pub fn set_language(culture: &str) -> bool {
    let Some(tag) = canonical(culture) else {
        return false;
    };
    *current_slot().write().expect("localization lock poisoned") = tag;
    true
}

/// Initialize from a persisted settings value, falling back to the Windows UI language.
pub fn init(culture: Option<&str>) {
    if let Some(tag) = culture.and_then(canonical) {
        let _ = set_language(tag);
        return;
    }
    let _ = set_language(&system_language());
}

pub fn display_name(culture: &str) -> String {
    canonical(culture)
        .and_then(|tag| {
            LANGUAGES
                .iter()
                .find(|(t, _)| *t == tag)
                .map(|(_, name)| *name)
        })
        .map(str::to_owned)
        .unwrap_or_else(|| culture.to_owned())
}

/// Windows UI language mapped to the closest bundled catalog.
pub fn system_language() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
        // Primary language IDs (low byte of a LANGID) for the 14 bundled cultures.
        let id = (unsafe { GetUserDefaultUILanguage() } & 0xff) as u16;
        let primary = match id {
            0x07 => "de",
            0x09 => "en",
            0x0a => "es",
            0x0c => "fr",
            0x10 => "it",
            0x11 => "ja",
            0x12 => "ko",
            0x13 => "nl",
            0x15 => "pl",
            0x16 => "pt",
            0x19 => "ru",
            0x1f => "tr",
            0x17 => "uk",
            0x04 => "zh",
            _ => "en",
        };
        if let Some((tag, _)) = LANGUAGES
            .iter()
            .find(|(tag, _)| two_letter(tag).eq_ignore_ascii_case(primary))
        {
            return (*tag).to_owned();
        }
    }
    DEFAULT_LANGUAGE.to_owned()
}

fn lookup(culture: &str, key: &str) -> Option<String> {
    if let Some(catalog) = canonical(culture).and_then(catalog_for)
        && let Some(value) = catalog.get(key)
    {
        return Some(value.clone());
    }
    let prefix = two_letter(culture);
    for (tag, _) in CATALOG_SOURCES {
        if tag.eq_ignore_ascii_case(culture) || !two_letter(tag).eq_ignore_ascii_case(prefix) {
            continue;
        }
        if let Some(catalog) = catalog_for(tag)
            && let Some(value) = catalog.get(key)
        {
            return Some(value.clone());
        }
    }
    if let Some(catalog) = catalog_for(DEFAULT_LANGUAGE)
        && let Some(value) = catalog.get(key)
    {
        return Some(value.clone());
    }
    None
}

/// Translate `key` using the active language.
pub fn t(key: &str) -> String {
    lookup(language(), key).unwrap_or_else(|| key.to_owned())
}

/// Translate `key` using an explicit culture (useful for previews/list boxes).
pub fn tr(culture: &str, key: &str) -> String {
    lookup(culture, key).unwrap_or_else(|| key.to_owned())
}

/// Translate and replace `{0}`, `{1}`, ... placeholders with `args`.
pub fn t_fmt(key: &str, args: &[&str]) -> String {
    let template = t(key);
    let mut result = String::with_capacity(template.len() + args.len() * 8);
    let mut rest = template.as_str();
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            result.push('{');
            rest = after;
            continue;
        };
        if let Ok(index) = after[..end].parse::<usize>() {
            if let Some(value) = args.get(index) {
                result.push_str(value);
            } else {
                result.push_str(&after[..=end]);
            }
        } else {
            result.push('{');
            result.push_str(&after[..=end]);
        }
        rest = &after[end + 1..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_fourteen_catalogs_are_embedded() {
        assert_eq!(LANGUAGES.len(), 14);
        for (tag, _) in LANGUAGES {
            assert!(is_supported(tag), "{tag} missing");
            assert!(!tr(tag, "Settings").is_empty(), "{tag} has no Settings key");
        }
    }

    #[test]
    fn english_catalog_covers_more_than_a_thousand_keys() {
        assert!(catalog_for(DEFAULT_LANGUAGE).unwrap().len() > 1000);
    }

    #[test]
    fn fallback_goes_to_english_for_a_missing_key() {
        assert_eq!(
            tr("tr-TR", "DefinitelyMissingKey_12345"),
            "DefinitelyMissingKey_12345"
        );
        assert_eq!(tr("tr-TR", "Settings"), "Ayarlar");
    }

    #[test]
    fn format_replaces_placeholders() {
        let template = t("CountdownEditingHelp");
        let formatted = t_fmt("CountdownEditingHelp", &["1", "1440"]);
        assert!(formatted.contains("1"));
        assert!(formatted.contains("1440"));
        assert!(!formatted.contains("{0}"));
        assert!(template.contains("minutes"));
    }

    #[test]
    fn set_language_normalizes_and_rejects_unknown_tags() {
        assert!(set_language("TR-tr"));
        assert_eq!(language(), "tr-TR");
        assert!(!set_language("xx-YY"));
        assert_eq!(language(), "tr-TR");
        let _ = set_language(DEFAULT_LANGUAGE);
    }
}
