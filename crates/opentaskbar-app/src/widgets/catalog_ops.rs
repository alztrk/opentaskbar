//! Widget catalog operations — `DockWidgetService` / `DockWidgetOption` parity.
//!
//! Metadata lives in [`super::registry`]; this module keeps persisted-settings
//! access (the `extra` bridge) and the installed-widget view. JSON round-tripping
//! means a key works whether it is a typed `DockSettings` field or an `extra` key.

use opentaskbar_core::settings::DockSettings;

use super::registry::canonical_widget_id;

/// The complete 22-widget catalog, in category order.
pub fn catalog() -> Vec<&'static super::registry::WidgetDescriptor> {
    super::registry::catalog_widgets().collect()
}

/// Reads a settings key from the serialized `DockSettings`. WS-A is converting several keys
/// from `extra` into typed fields; serializing first means we see either representation without
/// this crate needing to depend on the exact struct layout.
fn keys_match(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| *c != '_' && *c != '-')
            .flat_map(char::to_lowercase)
            .collect()
    };
    norm(a) == norm(b)
}

pub fn setting_value(settings: &DockSettings, key: &str) -> Option<serde_json::Value> {
    let root = serde_json::to_value(settings).ok()?;
    if let Some(v) = root.get(key) {
        return Some(v.clone());
    }
    // Serialized reference-style keys are PascalCase; callers may use snake_case or lowercase aliases.
    root.as_object().and_then(|m| {
        m.iter()
            .find(|(k, _)| keys_match(k, key))
            .map(|(_, v)| v.clone())
    })
}

pub fn setting_str(settings: &DockSettings, key: &str) -> Option<String> {
    setting_value(settings, key)
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
}

pub fn setting_bool(settings: &DockSettings, key: &str, default: bool) -> bool {
    setting_value(settings, key)
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

pub fn setting_i64(settings: &DockSettings, key: &str, default: i64) -> i64 {
    setting_value(settings, key)
        .and_then(|v| v.as_i64())
        .unwrap_or(default)
}

pub fn setting_f64(settings: &DockSettings, key: &str, default: f64) -> f64 {
    setting_value(settings, key)
        .and_then(|v| v.as_f64())
        .unwrap_or(default)
}

/// Writes a settings key through JSON so it lands in a typed field when WS-A has added one, or
/// in `extra` otherwise. Falls back to a direct `extra` insert if the round-trip fails.
pub fn set_setting(settings: &mut DockSettings, key: &str, value: serde_json::Value) {
    if let Ok(mut v) = serde_json::to_value(&*settings) {
        let actual = v
            .as_object()
            .and_then(|m| m.keys().find(|k| keys_match(k, key)).cloned())
            .unwrap_or_else(|| key.to_owned());
        v[actual] = value.clone();
        if let Ok(next) = serde_json::from_value::<DockSettings>(v) {
            *settings = next;
            return;
        }
    }
    settings.extra.insert(key.to_owned(), value);
}

pub fn setting_string_list(settings: &DockSettings, key: &str) -> Vec<String> {
    setting_value(settings, key)
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.into_iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// `InstalledWidgets` IDs normalized to the lowercase catalog space used by the registry.
/// `InstalledWidgets` is written in reference PascalCase (`SystemStats`, `AiUsageClaude`, ...),
/// so comparing it directly with a catalog ID would always report a widget as missing.
pub fn installed_catalog_widget_ids(settings: &DockSettings) -> Vec<String> {
    setting_string_list(settings, "InstalledWidgets")
        .into_iter()
        .map(|id| canonical_widget_id(&id).to_owned())
        .collect()
}

/// Whether a catalog ID is present in the persisted `InstalledWidgets` list.
pub fn is_widget_installed(id: &str, settings: &DockSettings) -> bool {
    let canonical = canonical_widget_id(id);
    installed_catalog_widget_ids(settings)
        .iter()
        .any(|installed| installed.eq_ignore_ascii_case(canonical))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_helper_reads_typed_and_extra_keys() {
        let mut s = DockSettings::default();
        s.extra.insert("ProbeBool".into(), serde_json::json!(true));
        s.extra.insert("ProbeNum".into(), serde_json::json!(42));
        assert!(setting_bool(&s, "ProbeBool", false));
        assert_eq!(setting_i64(&s, "ProbeNum", 0), 42);
        // Typed key is read through the same catalog helper as the extra JSON keys.
        s.weather_city = "Istanbul".to_owned();
        assert_eq!(setting_str(&s, "weather_city").as_deref(), Some("Istanbul"));
    }

    #[test]
    fn installing_a_widget_updates_both_catalog_and_installed_views() {
        let mut s = DockSettings::default();
        s.set_widget_enabled("aiusage_claude", true);
        assert!(is_widget_installed("aiusage_claude", &s));
        assert!(is_widget_installed("AiUsageClaude", &s));
        assert_eq!(installed_catalog_widget_ids(&s), ["aiusage_claude"]);
        assert!(
            crate::widgets::active_widgets(&s)
                .iter()
                .any(|w| w.id == "aiusage_claude"),
            "an installed+enabled widget must be active in the dock"
        );

        // Disabling keeps it installed, exactly like the reference widget option switch.
        s.set_widget_enabled("aiusage_claude", false);
        assert!(is_widget_installed("aiusage_claude", &s));
        assert!(
            !crate::widgets::active_widgets(&s)
                .iter()
                .any(|w| w.id == "aiusage_claude")
        );
    }
}
