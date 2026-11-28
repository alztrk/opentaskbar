//! Widget catalog operations — `DockWidgetService` / `DockWidgetOption` parity.
//!
//! Metadata lives in [`super::registry`]; this module keeps persisted-settings
//! access (the `extra` bridge) and the installed-widget view. JSON round-tripping
//! means a key works whether it is a typed `DockSettings` field or an `extra` key.

use opentaskbar_core::settings::DockSettings;

use super::registry::canonical_widget_id;

/// The complete 26-widget catalog, in category order.
pub fn catalog() -> Vec<&'static super::registry::WidgetDescriptor> {
    super::registry::catalog_widgets().collect()
}

/// One normalization rule for settings keys and widget IDs: lowercase, drop `_`/`-`.
pub fn compact_id(id: &str) -> String {
    id.chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

fn keys_match(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    let mut ai = a
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase);
    let mut bi = b
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase);
    loop {
        match (ai.next(), bi.next()) {
            (Some(ac), Some(bc)) if ac == bc => continue,
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// Reads a key from an already serialized settings object. Lets multi-key readers serialize
/// `DockSettings` once instead of once per key.
pub fn setting_value_from(root: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
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

fn compact_ascii_key<'a>(key: &str, buf: &'a mut [u8; 64]) -> Option<&'a str> {
    let mut len = 0usize;
    for b in key.bytes() {
        if b == b'_' || b == b'-' {
            continue;
        }
        if !b.is_ascii() || len >= buf.len() {
            return None;
        }
        buf[len] = b.to_ascii_lowercase();
        len += 1;
    }
    std::str::from_utf8(&buf[..len]).ok()
}

/// Reads a settings key from `DockSettings` without allocating a full JSON snapshot on the hot
/// path. Checks `extra` first, then resolves hot typed fields and known `extra`-only keys
/// directly, falling back to full JSON serialization only for unlisted keys.
pub fn setting_value(settings: &DockSettings, key: &str) -> Option<serde_json::Value> {
    if let Some(v) = settings.extra.get(key) {
        return Some(v.clone());
    }
    if let Some((_, v)) = settings.extra.iter().find(|(k, _)| keys_match(k, key)) {
        return Some(v.clone());
    }
    let mut buf = [0u8; 64];
    if let Some(compact) = compact_ascii_key(key, &mut buf) {
        use opentaskbar_core::settings::TemperatureUnit;
        use serde_json::Value;
        match compact {
            // Known `extra`-only keys: already checked above, so avoid serializing `DockSettings`.
            "aiusageringoverrides"
            | "weathercompactmode"
            | "storagecompactmode"
            | "customwidgetconsent"
            | "customwidgeticons"
            | "audiooutputdevice"
            | "widgetaliases"
            | "dynamicislandidlemode"
            | "dynamicislandcloseddisplay"
            | "dynamicislandhoverexpand"
            | "dynamicislandhoverdelayms"
            | "dynamicislandautoclosesecs"
            | "dynamicislandfollowmouse"
            | "dynamicislandscale"
            | "hudvolumeenabled"
            | "hudbrightnessenabled"
            | "hudcapslockenabled"
            | "hudkeyboardlangenabled"
            | "hudpowerenabled"
            | "hudnetworkenabled"
            | "hudbluetoothenabled"
            | "huddurationms"
            | "islandnotificationsenabled"
            | "islandnotificationsdurationms"
            | "islandnotificationsrespectfocus"
            | "islandnotificationsmutedapps"
            | "shelfenabled"
            | "shelfmaxitems"
            | "clipboardhistoryenabled"
            | "clipboardmaxitems"
            | "clipboardignoresensitive"
            | "smartocrenabled"
            | "quickscreenshotenabled"
            | "mediawaveformenabled"
            | "medialyricsenabled"
            | "medialyricspillenabled"
            | "agentmonitorenabled"
            | "agentapprovalbanners"
            | "todoremindersenabled"
            | "todoicalurl"
            | "todoicalpollminutes"
            | "privacyindicatorsenabled"
            | "quickpoweractionsenabled"
            | "wirelessmultibrandenabled"
            | "wirelessdisabledvendors"
            | "islandtogglehotkey"
            | "ocrhotkey"
            | "screenshothotkey"
            | "clipboardhotkey" => return None,
            // Hot typed fields accessed by widget chip/flyout/layout renderers.
            "weatherunit" => {
                let name = match settings.weather_unit {
                    TemperatureUnit::Celsius => "Celsius",
                    TemperatureUnit::Fahrenheit => "Fahrenheit",
                };
                return Some(Value::String(name.to_owned()));
            }
            "weathercity" => return Some(Value::String(settings.weather_city.clone())),
            "ignoredwirelessdevices" => {
                return Some(Value::Array(
                    settings
                        .ignored_wireless_devices
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ));
            }
            "wirelessbatterylowthreshold" => {
                return Some(Value::from(settings.wireless_battery_low_threshold));
            }
            "wirelessbatterypollseconds" => {
                return Some(Value::from(settings.wireless_battery_poll_seconds));
            }
            "wirelessbatterylistenonly" => {
                return Some(Value::Bool(settings.wireless_battery_listen_only));
            }
            "wirelessdevicealiases" => {
                return serde_json::to_value(&settings.wireless_device_aliases).ok();
            }
            "installedwidgets" => {
                return Some(Value::Array(
                    settings
                        .installed_widgets
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ));
            }
            "widgetdensityoverrides" => {
                return serde_json::to_value(&settings.widget_density_overrides).ok();
            }
            _ => {}
        }
    }
    setting_value_from(&serde_json::to_value(settings).ok()?, key)
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
    fn compact_id_and_cached_value_reader_share_one_rule() {
        assert_eq!(compact_id("AiUsage_Claude"), compact_id("aiusage-claude"));
        let root = serde_json::json!({"WeatherCity": "Izmir", "ExtraNum": 7});
        assert_eq!(
            setting_value_from(&root, "weather_city").and_then(|v| v.as_str().map(str::to_owned)),
            Some("Izmir".to_owned())
        );
        assert_eq!(
            setting_value_from(&root, "extra-num").and_then(|v| v.as_i64()),
            Some(7)
        );
        assert!(setting_value_from(&root, "missing").is_none());
    }

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
