//! Widget catalog operations — C# `DockWidgetService` / `DockWidgetOption` parity.
//!
//! Provides: alias widget IDs (`Date`, `ShowDesktop`), `HasSettings` + live-preview flags,
//! per-widget density overrides and shrink priority, hardware-availability hiding, and the
//! complete 24-widget catalog plus settings-access helpers that are resilient to WS-A moving a
//! key from `DockSettings.extra` into a typed field.

use opentaskbar_core::settings::DockSettings;

use super::{ALL_WIDGETS, WidgetDescriptor};

/// `(alias, canonical_id)` pairs accepted by C# `DockWidgetService.ResolveId`.
pub const WIDGET_ALIASES: &[(&str, &str)] = &[
    ("date", "clock"),
    ("showdesktop", "showdesktop"),
    ("show_desktop", "showdesktop"),
    ("ai", "aiusage_all"),
    ("aiusage", "aiusage_all"),
    ("sysstats", "stats"),
    ("systemstats", "stats"),
    ("net", "netspeed"),
    ("network", "netspeed"),
    ("btbattery", "bluetoothbattery"),
    ("keyboardlayout", "keyboard"),
    ("quicksettings", "quicksettings"),
    ("claude", "aiusage_claude"),
    ("codex", "aiusage_codex"),
    ("antigravity", "aiusage_antigravity"),
    ("opencode", "aiusage_opencode"),
    ("copilot", "aiusage_copilot"),
    ("deepseek", "aiusage_deepseek"),
    ("openrouter", "aiusage_openrouter"),
    ("cursor", "aiusage_cursor"),
];

/// Resolves a user-facing / legacy widget ID to its canonical catalog ID.
pub fn canonical_widget_id(id: &str) -> &str {
    let lower = id.to_ascii_lowercase();
    if let Some((_, canonical)) = WIDGET_ALIASES
        .iter()
        .find(|(alias, _)| alias.eq_ignore_ascii_case(&lower))
    {
        return canonical;
    }
    ALL_WIDGETS
        .iter()
        .find(|w| w.id.eq_ignore_ascii_case(&lower))
        .map_or(id, |w| w.id)
}

/// `ShowDesktop` is a shell action exposed as a catalog entry but has no chip of its own.
pub const SHOW_DESKTOP_WIDGET: WidgetDescriptor = WidgetDescriptor {
    id: "showdesktop",
    name: "Show Desktop",
    category: "Media & Controls",
    width_dips: 58.0,
};

/// The complete C# 24-widget catalog (23 baseline + Antigravity). `ShowDesktop` is a shell alias
/// resolved through `canonical_widget_id`, not an additional catalog entry.
pub fn catalog() -> Vec<&'static WidgetDescriptor> {
    ALL_WIDGETS.iter().collect()
}

/// Per-widget density matching C# `WidgetDensity` (`Compact` / `Normal` / `Comfortable`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetDensity {
    Compact,
    Normal,
    Comfortable,
}

impl WidgetDensity {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "compact" | "small" => Self::Compact,
            "comfortable" | "large" => Self::Comfortable,
            _ => Self::Normal,
        }
    }

    /// Width multiplier relative to the C# `FullWidthEstimate` baseline.
    pub fn width_scale(self) -> f32 {
        match self {
            Self::Compact => 0.82,
            Self::Normal => 1.0,
            Self::Comfortable => 1.18,
        }
    }
}

/// C# `DockWidgetOption.ShrinkPriority` — lower values shrink first when the dock overflows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShrinkPriority {
    First = 0,
    Early = 1,
    Normal = 2,
    Late = 3,
    Never = 4,
}

/// C# `DockWidgetOption` metadata consumed by the settings catalog and the dock layout.
#[derive(Clone, Copy, Debug)]
pub struct WidgetOption {
    pub descriptor: &'static WidgetDescriptor,
    pub has_settings: bool,
    /// C# `LivePreview` — settings catalog shows a live chip render.
    pub live_preview: bool,
    pub shrink_priority: ShrinkPriority,
    pub hardware_dependent: bool,
}

impl WidgetOption {
    pub fn for_id(id: &str) -> Option<Self> {
        let canonical = canonical_widget_id(id);
        let descriptor = if canonical.eq_ignore_ascii_case("showdesktop") {
            Some(&SHOW_DESKTOP_WIDGET)
        } else {
            ALL_WIDGETS.iter().find(|w| w.id == canonical)
        }?;
        let hardware_dependent = matches!(
            descriptor.id,
            "netspeed" | "battery" | "bluetoothbattery" | "audio" | "media" | "quicksettings"
        );
        Some(Self {
            descriptor,
            has_settings: matches!(
                descriptor.id,
                "clock"
                    | "weather"
                    | "stats"
                    | "countdown"
                    | "focus"
                    | "bluetoothbattery"
                    | "quicksettings"
                    | "keyboard"
            ) || descriptor.id.starts_with("aiusage_"),
            live_preview: !matches!(descriptor.id, "showdesktop" | "stacks" | "groups"),
            shrink_priority: match descriptor.id {
                "showdesktop" | "dayprogress" => ShrinkPriority::First,
                "clock" | "weather" | "stats" | "netspeed" => ShrinkPriority::Early,
                "aiusage_all" | "stacks" | "groups" => ShrinkPriority::Never,
                _ => ShrinkPriority::Normal,
            },
            hardware_dependent,
        })
    }

    pub fn matches_search(&self, query: &str) -> bool {
        let q = query.trim().to_ascii_lowercase();
        q.is_empty()
            || self.descriptor.name.to_ascii_lowercase().contains(&q)
            || self.descriptor.id.to_ascii_lowercase().contains(&q)
            || self.descriptor.category.to_ascii_lowercase().contains(&q)
    }
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
    // Serialized C#-style keys are PascalCase; callers may use snake_case or lowercase aliases.
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

/// Resolved layout for a widget: effective density, shrink priority, and hardware visibility.
#[derive(Clone, Copy, Debug)]
pub struct WidgetLayout {
    pub density: WidgetDensity,
    pub shrink_priority: ShrinkPriority,
    pub width_dips: f32,
    pub visible: bool,
}

/// Resolves `WidgetDensityOverrides` (C# `Dictionary<string, string>`) plus hardware gating.
pub fn widget_layout(id: &str, settings: &DockSettings, hardware_available: bool) -> WidgetLayout {
    let option = WidgetOption::for_id(id);
    let density = setting_value(settings, "WidgetDensityOverrides")
        .and_then(|v| {
            v.as_object().cloned().and_then(|m| {
                m.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(id))
                    .and_then(|(_, v)| v.as_str())
                    .map(WidgetDensity::parse)
            })
        })
        .unwrap_or(WidgetDensity::Normal);
    let (priority, width) = option.map_or((ShrinkPriority::Normal, 96.0), |o| {
        (
            o.shrink_priority,
            o.descriptor.width_dips * density.width_scale(),
        )
    });
    WidgetLayout {
        density,
        shrink_priority: priority,
        width_dips: width,
        visible: !option.is_some_and(|o| o.hardware_dependent) || hardware_available,
    }
}

/// C# `DockWidgetService.IsHardwareAvailable`: capability-based visibility gating. The caller
/// supplies the live probes so this stays a pure function.
pub fn hardware_available(id: &str, network: bool, battery: bool, audio: bool, bt: bool) -> bool {
    match canonical_widget_id(id) {
        "netspeed" => network,
        "battery" => battery,
        "bluetoothbattery" => bt,
        "audio" | "media" | "quicksettings" => audio,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_ids_resolve() {
        assert_eq!(canonical_widget_id("Date"), "clock");
        assert_eq!(canonical_widget_id("ShowDesktop"), "showdesktop");
        assert_eq!(canonical_widget_id("sysstats"), "stats");
        assert_eq!(canonical_widget_id("CLAUDE"), "aiusage_claude");
    }

    #[test]
    fn catalog_has_23_entries() {
        assert_eq!(catalog().len(), 23);
        assert!(WidgetOption::for_id("showdesktop").is_some());
        assert!(WidgetOption::for_id("clock").unwrap().has_settings);
        assert!(!WidgetOption::for_id("netspeed").unwrap().has_settings);
        assert!(WidgetOption::for_id("weather").unwrap().live_preview);
    }

    #[test]
    fn density_override_applies_scale() {
        let mut s = DockSettings::default();
        s.extra.insert(
            "WidgetDensityOverrides".to_owned(),
            serde_json::json!({ "clock": "Compact", "stats": "Comfortable" }),
        );
        let clock = widget_layout("clock", &s, true);
        assert_eq!(clock.density, WidgetDensity::Compact);
        assert!(clock.width_dips < 148.0);
        let stats = widget_layout("stats", &s, true);
        assert_eq!(stats.density, WidgetDensity::Comfortable);
        assert!(stats.width_dips > 96.0);
    }

    #[test]
    fn hardware_gating_hides_unavailable_widgets() {
        assert!(!hardware_available("netspeed", false, true, true, true));
        assert!(hardware_available("netspeed", true, true, true, true));
        assert!(!hardware_available(
            "bluetoothbattery",
            true,
            true,
            true,
            false
        ));
        assert!(hardware_available("clock", false, false, false, false));
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
}
