//! Single source of truth for widget metadata.
//!
//! Everything that describes a widget — catalog card, dock chip, shell action,
//! dock-only helper or virtual aggregate — is declared once in [`BUILTINS`].
//! The catalog, settings UI, dock layout, hover/flyout gate and search index all
//! read from here, so a widget can no longer be "known" in one place and missing
//! in another.
//!
//! This is metadata only. Rendering (`chip_spec`) and interaction dispatch stay in
//! `WidgetRuntime`; every ID lookup (including legacy PascalCase and custom widget IDs)
//! goes through [`descriptor_for`] so they cannot disagree about what a widget is.

use std::sync::LazyLock;

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::DockSettings;

use super::catalog_ops::{compact_id, setting_value};

/// `DockWidget.ShrinkPriority` — lower values shrink first when the dock overflows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShrinkPriority {
    First = 0,
    Early = 1,
    Normal = 2,
    Late = 3,
    Never = 4,
}

/// Per-widget density matching `WidgetDensity` (`Compact` / `Normal` / `Comfortable`).
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

    /// Width multiplier relative to the `FullWidthEstimate` baseline.
    pub fn width_scale(self) -> f32 {
        match self {
            Self::Compact => 0.82,
            Self::Normal => 1.0,
            Self::Comfortable => 1.18,
        }
    }
}

/// Capability a widget needs before it should be shown on the dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareNeed {
    None,
    Network,
    Battery,
    Audio,
    Bluetooth,
}

/// All metadata for one built-in widget. `name` is the English fallback and
/// search keyword; `name_key` resolves the localized name through `i18n`.
#[derive(Clone, Copy, Debug)]
pub struct WidgetDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub name_key: &'static str,
    pub category: &'static str,
    pub width_dips: f32,
    /// Has a Settings catalog card.
    pub catalog: bool,
    /// Has a dock chip (`widget_descriptor` resolves it).
    pub dock_descriptor: bool,
    /// Virtual aggregate (AI Usage) with no chip or catalog card of its own.
    pub virtual_widget: bool,
    pub has_flyout: bool,
    pub has_settings: bool,
    pub live_preview: bool,
    pub shrink_priority: ShrinkPriority,
    pub hardware: HardwareNeed,
}

// Const table builder: a named-field literal would repeat all 13 fields for every widget.
#[allow(clippy::too_many_arguments)]
const fn catalog_wd(
    id: &'static str,
    name: &'static str,
    name_key: &'static str,
    category: &'static str,
    width_dips: f32,
    has_flyout: bool,
    has_settings: bool,
    hardware: HardwareNeed,
    shrink_priority: ShrinkPriority,
) -> WidgetDescriptor {
    WidgetDescriptor {
        id,
        name,
        name_key,
        category,
        width_dips,
        catalog: true,
        dock_descriptor: true,
        virtual_widget: false,
        live_preview: true,
        has_flyout,
        has_settings,
        hardware,
        shrink_priority,
    }
}

const fn dock_wd(
    id: &'static str,
    name: &'static str,
    name_key: &'static str,
    category: &'static str,
    width_dips: f32,
) -> WidgetDescriptor {
    WidgetDescriptor {
        id,
        name,
        name_key,
        category,
        width_dips,
        catalog: false,
        dock_descriptor: true,
        virtual_widget: false,
        live_preview: false,
        has_flyout: true,
        has_settings: false,
        hardware: HardwareNeed::None,
        shrink_priority: ShrinkPriority::Never,
    }
}

const fn shell_wd(
    id: &'static str,
    name: &'static str,
    name_key: &'static str,
    category: &'static str,
    width_dips: f32,
) -> WidgetDescriptor {
    WidgetDescriptor {
        id,
        name,
        name_key,
        category,
        width_dips,
        catalog: false,
        dock_descriptor: false,
        virtual_widget: false,
        live_preview: false,
        has_flyout: false,
        has_settings: false,
        hardware: HardwareNeed::None,
        shrink_priority: ShrinkPriority::First,
    }
}

const fn virtual_wd(
    id: &'static str,
    name: &'static str,
    name_key: &'static str,
    category: &'static str,
    width_dips: f32,
) -> WidgetDescriptor {
    WidgetDescriptor {
        id,
        name,
        name_key,
        category,
        width_dips,
        catalog: false,
        dock_descriptor: false,
        virtual_widget: true,
        live_preview: false,
        has_flyout: true,
        has_settings: true,
        hardware: HardwareNeed::None,
        shrink_priority: ShrinkPriority::Never,
    }
}

/// `(alias, canonical_id)` pairs accepted by `DockWidgetService.ResolveId`.
pub const WIDGET_ALIASES: &[(&str, &str)] = &[
    ("date", "clock"),
    ("show_desktop", "showdesktop"),
    ("ai", "aiusage_all"),
    ("aiusage", "aiusage_all"),
    ("sysstats", "stats"),
    ("systemstats", "stats"),
    ("net", "netspeed"),
    ("network", "netspeed"),
    ("btbattery", "bluetoothbattery"),
    ("keyboardlayout", "keyboard"),
    ("claude", "aiusage_claude"),
    ("codex", "aiusage_codex"),
    ("opencode", "aiusage_opencode"),
    ("copilot", "aiusage_copilot"),
    ("deepseek", "aiusage_deepseek"),
    ("openrouter", "aiusage_openrouter"),
    ("cursor", "aiusage_cursor"),
];

/// Every built-in widget descriptor in one table: 22 catalog cards, two
/// dock-only helpers, one shell action and one virtual aggregate.
pub const BUILTINS: &[WidgetDescriptor] = &[
    // Time & Weather
    catalog_wd(
        "clock",
        "Clock & Date",
        "WidgetClock",
        "Time & Weather",
        148.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Early,
    ),
    catalog_wd(
        "weather",
        "Weather",
        "WidgetWeather",
        "Time & Weather",
        240.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Early,
    ),
    catalog_wd(
        "dayprogress",
        "Day Progress",
        "WidgetDayProgress",
        "Time & Weather",
        96.0,
        true,
        false,
        HardwareNeed::None,
        ShrinkPriority::First,
    ),
    catalog_wd(
        "stopwatch",
        "Stopwatch",
        "WidgetStopwatch",
        "Time & Weather",
        124.0,
        true,
        false,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "countdown",
        "Countdown",
        "WidgetCountdown",
        "Time & Weather",
        124.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    // System & Hardware
    catalog_wd(
        "stats",
        "System Stats",
        "WidgetSystemStats",
        "System & Hardware",
        114.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Early,
    ),
    catalog_wd(
        "netspeed",
        "Net Speed",
        "WidgetNetSpeed",
        "System & Hardware",
        156.0,
        true,
        false,
        HardwareNeed::Network,
        ShrinkPriority::Early,
    ),
    catalog_wd(
        "battery",
        "Battery",
        "WidgetBattery",
        "System & Hardware",
        96.0,
        false,
        false,
        HardwareNeed::Battery,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "bluetoothbattery",
        "Bluetooth Battery",
        "WidgetBluetoothBattery",
        "System & Hardware",
        78.0,
        true,
        true,
        HardwareNeed::Bluetooth,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "storage",
        "Storage",
        "WidgetStorage",
        "System & Hardware",
        136.0,
        true,
        false,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    // AI Quotas
    catalog_wd(
        "aiusage_claude",
        "Claude",
        "WidgetAiUsageClaude",
        "AI Quotas",
        184.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "aiusage_codex",
        "Codex",
        "WidgetAiUsageCodex",
        "AI Quotas",
        184.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "aiusage_opencode",
        "OpenCode",
        "WidgetAiUsageOpenCode",
        "AI Quotas",
        296.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "aiusage_copilot",
        "Copilot",
        "WidgetAiUsageCopilot",
        "AI Quotas",
        192.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "aiusage_deepseek",
        "DeepSeek",
        "WidgetAiUsageDeepSeek",
        "AI Quotas",
        139.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "aiusage_openrouter",
        "OpenRouter",
        "WidgetAiUsageOpenRouter",
        "AI Quotas",
        139.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "aiusage_cursor",
        "Cursor",
        "WidgetAiUsageCursor",
        "AI Quotas",
        164.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    // Media & Controls
    catalog_wd(
        "media",
        "Media",
        "WidgetMedia",
        "Media & Controls",
        256.0,
        true,
        false,
        HardwareNeed::Audio,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "audio",
        "Audio",
        "WidgetAudio",
        "Media & Controls",
        78.0,
        true,
        false,
        HardwareNeed::Audio,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "quicksettings",
        "Quick Settings",
        "WidgetQuickSettings",
        "Media & Controls",
        112.0,
        true,
        true,
        HardwareNeed::Audio,
        ShrinkPriority::Normal,
    ),
    catalog_wd(
        "keyboard",
        "Keyboard Layout",
        "WidgetKeyboardLayout",
        "Media & Controls",
        58.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    // Productivity
    catalog_wd(
        "focus",
        "Focus",
        "WidgetFocus",
        "Productivity",
        124.0,
        true,
        true,
        HardwareNeed::None,
        ShrinkPriority::Normal,
    ),
    // Dock-only helpers (no Settings card, but a real dock chip).
    dock_wd(
        "stacks",
        "Folder Stacks",
        "FolderStacks",
        "System & Hardware",
        98.0,
    ),
    dock_wd(
        "groups",
        "App Groups",
        "AppGroups",
        "System & Hardware",
        98.0,
    ),
    // Shell action exposed through the catalog resolver, with no chip of its own.
    shell_wd(
        "showdesktop",
        "Show Desktop",
        "WidgetShowDesktop",
        "Media & Controls",
        58.0,
    ),
    // Virtual aggregate for the seven AI providers.
    virtual_wd(
        "aiusage_all",
        "AI Usage",
        "WidgetAiUsage",
        "AI Quotas",
        184.0,
    ),
];

/// Categories in catalog order, derived from [`BUILTINS`].
pub static WIDGET_CATEGORIES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut out: Vec<&'static str> = Vec::new();
    for w in BUILTINS.iter().filter(|w| w.catalog) {
        if !out.contains(&w.category) {
            out.push(w.category);
        }
    }
    out
});

/// The 22 widgets shown in the Settings catalog.
pub fn catalog_widgets() -> impl Iterator<Item = &'static WidgetDescriptor> {
    BUILTINS
        .iter()
        .filter(|w| w.catalog)
        .chain(super::custom_widgets::descriptors())
}

/// Resolves a widget that owns a dock chip. Case-insensitive, matching the old
/// `widget_descriptor` behavior for `stacks` / `groups`.
pub fn widget_descriptor(id: &str) -> Option<&'static WidgetDescriptor> {
    BUILTINS
        .iter()
        .find(|w| w.dock_descriptor && w.id.eq_ignore_ascii_case(id))
        .or_else(|| {
            super::custom_widgets::descriptors()
                .into_iter()
                .find(|w| w.id.eq_ignore_ascii_case(id))
        })
}

/// Resolves a user-facing / legacy widget ID to its canonical catalog ID.
pub fn canonical_widget_id(id: &str) -> &str {
    let lower = id.to_ascii_lowercase();
    if let Some((_, canonical)) = WIDGET_ALIASES
        .iter()
        .find(|(alias, _)| alias.eq_ignore_ascii_case(&lower))
    {
        return canonical;
    }
    // Reference writes `AiUsageClaude` / `KeyboardLayout` while the catalog uses
    // `aiusage_claude` / `keyboard`; ignore word separators so both spellings
    // resolve to the same card. Only catalog IDs participate, exactly as before.
    let compact = compact_id(&lower);
    BUILTINS
        .iter()
        .find(|w| compact_id(w.id) == compact)
        .map_or(id, |w| w.id)
}

/// The single ID lookup used by names, flyout gates, options and hardware gating.
/// Accepts canonical IDs, legacy PascalCase spellings and custom widget IDs.
pub fn descriptor_for(id: &str) -> Option<&'static WidgetDescriptor> {
    let canonical = canonical_widget_id(id);
    let matches = |w: &&'static WidgetDescriptor| {
        w.id.eq_ignore_ascii_case(id) || w.id.eq_ignore_ascii_case(canonical)
    };
    BUILTINS.iter().find(|w| matches(w)).or_else(|| {
        super::custom_widgets::descriptors()
            .into_iter()
            .find(|w| matches(w))
    })
}

/// Authoritative `DockWidgetOption.HasSettings` answer. Settings UI call sites that still
/// keep a local list should delegate here (see `FIXES-M.md` for the cross-agent call site).
pub fn descriptor_has_settings(id: &str) -> bool {
    descriptor_for(id).is_some_and(|w| w.has_settings)
}

/// The five widgets with inline settings rows inside their catalog card. Distinct from
/// `has_settings`, which drives the Configure button and covers more widgets.
pub fn widget_has_inline_settings(id: &str) -> bool {
    matches!(
        canonical_widget_id(id),
        "clock" | "weather" | "stats" | "bluetoothbattery" | "focus"
    )
}

/// Category → i18n key, shared with `ui::settings::category_label`.
pub fn category_i18n_key(category: &str) -> &str {
    match category {
        "Time & Weather" => "WidgetCategoryTime",
        "System & Hardware" => "WidgetCategorySystem",
        "AI Quotas" => "WidgetCategoryAi",
        "Media & Controls" => "WidgetCategoryMedia",
        "Productivity" => "WidgetCategoryProductivity",
        other => other,
    }
}

/// Localized widget name for the settings catalog and widget menus.
pub fn widget_name(id: &str) -> String {
    if super::custom_widgets::is_custom(id) {
        return super::custom_widgets::display_name(id);
    }
    descriptor_for(id).map_or_else(|| id.to_owned(), |w| loc::t(w.name_key))
}

/// Single source of truth for widgets that open a popup flyout. Used by the
/// dock hover coordinator and direct-click dispatch.
pub fn widget_has_flyout(id: &str) -> bool {
    // Custom widgets always expose a flyout (see `custom_widgets::descriptor_for`), even
    // before the first folder scan has produced a descriptor.
    super::custom_widgets::is_custom(id) || descriptor_for(id).is_some_and(|w| w.has_flyout)
}

/// Widgets currently rendered on the dock. Reads the effective enabled state
/// from `DockSettings::is_widget_enabled`; Focus Mode hides widgets here just
/// as `WidgetCoordinator.Apply` does.
pub fn active_widgets(settings: &DockSettings) -> Vec<&'static WidgetDescriptor> {
    let mut out: Vec<&'static WidgetDescriptor> = catalog_widgets()
        .filter(|w| settings.is_widget_enabled(w.id))
        .collect();
    if !settings.stack_folders.is_empty()
        && let Some(stacks) = widget_descriptor("stacks")
    {
        out.push(stacks);
    }
    if !settings.app_groups.is_empty()
        && let Some(groups) = widget_descriptor("groups")
    {
        out.push(groups);
    }
    out
}

/// `DockWidgetOption` metadata consumed by the settings catalog and dock layout.
#[derive(Clone, Copy, Debug)]
pub struct WidgetOption {
    pub descriptor: &'static WidgetDescriptor,
    pub has_settings: bool,
    /// `LivePreview` — settings catalog shows a live chip render.
    pub live_preview: bool,
    pub shrink_priority: ShrinkPriority,
    pub hardware_dependent: bool,
}

impl WidgetOption {
    pub fn for_id(id: &str) -> Option<Self> {
        let descriptor = descriptor_for(id).filter(|w| w.catalog || w.id == "showdesktop")?;
        Some(Self {
            descriptor,
            has_settings: descriptor.has_settings,
            live_preview: descriptor.live_preview,
            shrink_priority: descriptor.shrink_priority,
            hardware_dependent: descriptor.hardware != HardwareNeed::None,
        })
    }

    /// Category label for the current locale (same keys as `ui::settings::category_label`).
    fn localized_category(&self) -> String {
        loc::t(category_i18n_key(self.descriptor.category))
    }

    pub fn matches_search(&self, query: &str) -> bool {
        let q = query.trim().to_ascii_lowercase();
        let localized_name = super::widget_name(self.descriptor.id);
        let localized_category = self.localized_category();
        q.is_empty()
            || self.descriptor.name.to_ascii_lowercase().contains(&q)
            || localized_name.to_ascii_lowercase().contains(&q)
            || self.descriptor.id.to_ascii_lowercase().contains(&q)
            || self.descriptor.category.to_ascii_lowercase().contains(&q)
            || localized_category.to_ascii_lowercase().contains(&q)
    }
}

/// Resolved layout for a widget: effective density, shrink priority, width and visibility.
#[derive(Clone, Copy, Debug)]
pub struct WidgetLayout {
    pub density: WidgetDensity,
    pub shrink_priority: ShrinkPriority,
    pub width_dips: f32,
    pub visible: bool,
}

/// Resolves `WidgetDensityOverrides` plus hardware gating.
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

/// `DockWidgetService.IsHardwareAvailable`: capability-based visibility gating.
pub fn hardware_available(id: &str, network: bool, battery: bool, audio: bool, bt: bool) -> bool {
    let need = descriptor_for(id).map_or(HardwareNeed::None, |w| w.hardware);
    match need {
        HardwareNeed::Network => network,
        HardwareNeed::Battery => battery,
        HardwareNeed::Bluetooth => bt,
        HardwareNeed::Audio => audio,
        HardwareNeed::None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_table_is_the_only_metadata_source() {
        // Derive counts from the table: adding a widget must not require editing three tests.
        assert_eq!(
            catalog_widgets().count(),
            BUILTINS.iter().filter(|w| w.catalog).count()
                + super::super::custom_widgets::descriptors().len()
        );
        let categories: std::collections::HashSet<_> = BUILTINS
            .iter()
            .filter(|w| w.catalog)
            .map(|w| w.category)
            .collect();
        assert_eq!(WIDGET_CATEGORIES.len(), categories.len());
        let mut seen = std::collections::HashSet::new();
        for w in BUILTINS {
            assert!(seen.insert(w.id), "duplicate widget id {}", w.id);
            assert!(w.width_dips >= 48.0);
            assert!(!loc::t(w.name_key).is_empty());
        }
        assert!(seen.contains("showdesktop") && seen.contains("aiusage_all"));
    }

    #[test]
    fn aliases_and_canonical_ids_resolve() {
        assert_eq!(canonical_widget_id("Date"), "clock");
        assert_eq!(canonical_widget_id("ShowDesktop"), "showdesktop");
        assert_eq!(canonical_widget_id("sysstats"), "stats");
        assert_eq!(canonical_widget_id("CLAUDE"), "aiusage_claude");
        assert_eq!(canonical_widget_id("btbattery"), "bluetoothbattery");
        assert_eq!(canonical_widget_id("unknown"), "unknown");
    }

    #[test]
    fn shell_helpers_keep_old_visibility_rules() {
        assert!(widget_has_flyout("clock"));
        assert!(widget_has_flyout("aiusage_all"));
        assert!(widget_has_flyout("stacks"));
        assert!(!widget_has_flyout("battery"));
        assert!(!widget_has_flyout("showdesktop"));
        assert!(WidgetOption::for_id("showdesktop").is_some());
        assert!(WidgetOption::for_id("clock").unwrap().has_settings);
        assert!(!WidgetOption::for_id("netspeed").unwrap().has_settings);
        assert!(WidgetOption::for_id("weather").unwrap().live_preview);
        assert!(widget_descriptor("stacks").is_some_and(|d| !d.catalog && d.dock_descriptor));
        assert!(WidgetOption::for_id("stacks").is_none());
        // Case-insensitive + canonical lookup through the one shared helper.
        assert_eq!(descriptor_for("Clock").unwrap().id, "clock");
        assert_eq!(
            descriptor_for("AiUsageClaude").unwrap().id,
            "aiusage_claude"
        );
        assert_eq!(descriptor_for("ShowDesktop").unwrap().id, "showdesktop");
        assert!(descriptor_has_settings("Clock"));
        assert!(!descriptor_has_settings("Dns"));
        assert!(widget_has_inline_settings("Stats"));
        assert!(!widget_has_inline_settings("Countdown"));
        assert_eq!(category_i18n_key("AI Quotas"), "WidgetCategoryAi");
    }

    #[test]
    fn hardware_gating_uses_descriptor_capabilities() {
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
        assert!(stats.width_dips > 114.0);
    }
}
