//! Persisted settings compatible with OpenTaskbar's `settings.json`.
//!
//! Every `DockSettings` property has either a typed field here or a serde alias that feeds one.
//! Unknown fields (and all reference profile properties that the port does not consume) are preserved in
//! `extra`/`DockProfile::extra` so switching between the reference and Rust builds never drops data.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::hotkey::{HotkeyBinding, HotkeyChoice};
use serde::de::{Deserializer, Error as DeError, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value, json};

// ---- WIP helpers kept from the pre-integration port ----

fn default_language() -> String {
    crate::i18n::system_language()
}

fn default_font_size() -> f32 {
    13.0
}

fn is_disabled_hotkey(value: &str) -> bool {
    let value = value.trim();
    value.is_empty()
        || value.eq_ignore_ascii_case("Disabled")
        || value.eq_ignore_ascii_case("Backspace")
        || value.eq_ignore_ascii_case("Esc")
        || value.eq_ignore_ascii_case("Escape")
}

fn default_true() -> bool {
    true
}

fn default_burn_in_shift_minutes() -> i32 {
    30
}

fn default_dynamic_island_opacity() -> f64 {
    0.9
}

pub const DEFAULT_GLASS_BLUR_SIGMA: f64 = 9.0;
pub const DEFAULT_GLASS_SATURATION: f64 = 1.32;
pub const DEFAULT_GLASS_EDGE_GLOW: f64 = 0.88;

fn default_glass_blur_sigma() -> f64 {
    DEFAULT_GLASS_BLUR_SIGMA
}

fn default_glass_saturation() -> f64 {
    DEFAULT_GLASS_SATURATION
}

fn default_glass_edge_glow() -> f64 {
    DEFAULT_GLASS_EDGE_GLOW
}

/// Maps a per-surface opacity (`0.0..=1.0`, default `0.90`) into Clear Crystal's
/// neutral glass tint alpha so default `0.90` lands on the verified Clear Crystal
/// sweet spot (`0.12` dark / `0.18` light) while remaining user-adjustable.
pub fn clear_crystal_tint_alpha(surface_opacity: f64, is_dark: bool) -> f32 {
    let op = if surface_opacity.is_finite() {
        surface_opacity.clamp(0.0, 1.0) as f32
    } else {
        0.9
    };
    let scale = if is_dark { 0.135 } else { 0.20 };
    (op * scale).clamp(0.0, 0.65)
}

/// `SystemMeters` is written as `"Cpu, Memory, Gpu"` by `JsonStringEnumConverter`, while older
/// builds used a numeric bitmask. Accept both; keep the retired CPU-temperature bit slot unused.
fn deserialize_stats_meters<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Meters {
        Bits(u32),
        Names(String),
    }

    fn bit_for(name: &str) -> u32 {
        match name.trim().to_ascii_lowercase().as_str() {
            "cpu" => 1,
            "memory" => 2,
            "battery" => 4,
            "cputemperature" | "cpu temperature" => 0,
            "gpu" => 16,
            "gputemperature" | "gpu temperature" => 32,
            "all" => 1 | 2 | 16 | 32,
            _ => 0,
        }
    }

    let value = Meters::deserialize(deserializer).map_err(|_| {
        serde::de::Error::custom("StatsMeters must be a bitmask or a comma-separated flag list")
    })?;
    Ok(match value {
        Meters::Bits(bits) => bits & !(1 << 3),
        Meters::Names(names) => names
            .split(',')
            .fold(0u32, |bits, name| bits | bit_for(name)),
    })
}

/// Accepts `AppLanguageConverter` values: BCP 47 strings, legacy English names and numeric codes.
fn deserialize_language<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = Value::deserialize(deserializer)?;
    let candidate = match value {
        Value::String(text) => match text.to_ascii_lowercase().as_str() {
            "turkish" => "tr-TR".to_owned(),
            "english" => "en-US".to_owned(),
            "japanese" => "ja-JP".to_owned(),
            _ => text,
        },
        Value::Number(number) => match number.as_u64() {
            Some(0) => "tr-TR".to_owned(),
            Some(2) => "ja-JP".to_owned(),
            _ => "en-US".to_owned(),
        },
        _ => default_language(),
    };
    Ok(if crate::i18n::is_supported(&candidate) {
        candidate
    } else {
        default_language()
    })
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub const DYNAMIC_ISLAND_ALL_DISPLAYS: &str = "*";

/// reference property names that the port exposes under a different Rust field name. `save_to` writes
/// both spellings so a settings.json written by Rust is readable by the reference build as well; the
/// reference-facing spelling wins when both appear in a file that is loaded again (that is the key the
/// reference build owns, so a conflict must not silently reverse a value written by reference).
const SETTINGS_ALIAS_PAIRS: [(&str, &str); 6] = [
    ("EnabledWidgets", "Widgets"),
    ("ShowClockWidget", "ShowClock"),
    ("ShowSystemStatsWidget", "ShowSystemStats"),
    ("FocusMode", "FocusModeEnabled"),
    ("UserPinnedBackgroundApps", "PinnedTrayIcons"),
    ("DynamicIslandEnable", "DynamicIslandEnabled"),
];

/// Collapses the double-spelled keys written by `save_to`. Serde rejects a JSON object that maps
/// two keys onto the same struct field, so exactly one spelling is kept before deserialization.
/// The reference-facing key wins: `settings.json` is a shared file and reference writes that spelling.
fn dedupe_written_aliases(map: &mut Map<String, Value>) {
    for (canonical, reference) in SETTINGS_ALIAS_PAIRS {
        if map.contains_key(reference) {
            map.remove(canonical);
        }
    }
    // `FullWidth` is a bool in Rust; reference persists the same value as the `DockLayout` enum name.
    if map.contains_key("Layout") {
        map.remove("FullWidth");
    }
    if let Some(Value::Array(profiles)) = map.get_mut("Profiles") {
        for profile in profiles.iter_mut() {
            if let Value::Object(object) = profile
                && object.contains_key("Widgets")
            {
                object.remove("EnabledWidgets");
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockPosition {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockEdge {
    #[default]
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockSize {
    #[default]
    Compact,
    Medium,
    Large,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppTheme {
    Dark,
    Light,
    #[default]
    System,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoHideMode {
    #[default]
    Disabled,
    Smart,
    Always,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfaceBackground {
    #[default]
    Glass,
    Solid,
    None,
    /// Opaque, pure-black fill with no rim, sheen or shadow (OLED display care).
    TrueBlack,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockLayout {
    #[default]
    Floating,
    FullWidth,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockContentAlignment {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartButtonAction {
    #[default]
    WindowsStart,
    Launcher,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartButtonPlacement {
    #[default]
    Start,
    End,
    Hidden,
    InWidgets,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockDateStyle {
    #[default]
    DayAndShortDate,
    ShortDateOnly,
    NumericDate,
    Hidden,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockDateFormat {
    #[default]
    System,
    DdMmYyyy,
    DdMmYy,
    MmDdYyyy,
    YyyyMmDd,
    DdMmYyyyDots,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockDateLayout {
    #[default]
    Beside,
    Below,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TemperatureUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MiddleClickAction {
    #[default]
    NewWindow,
    CloseWindow,
    Nothing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FisheyeIntensity {
    Low,
    #[default]
    Normal,
    Strong,
}

impl FisheyeIntensity {
    /// Additional scale factor at the cursor center (`1.0 + peak`).
    pub fn peak(self) -> f32 {
        match self {
            Self::Low => 0.20,
            Self::Normal => 0.42,
            Self::Strong => 0.70,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecondaryAppScope {
    #[default]
    CurrentMonitorOnly,
    AllMonitors,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetDensityMode {
    #[default]
    Auto,
    Compact,
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetFlyoutTrigger {
    #[default]
    Click,
    Hover,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DynamicIslandVisibilityMode {
    #[default]
    Always,
    DesktopOnly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockWidthMode {
    #[default]
    Auto,
    Pixels,
    Percent,
    Fill,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockHeightMode {
    #[default]
    Auto,
    Pixels,
    Percent,
    Fill,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockGapUnit {
    #[default]
    Pixels,
    Percent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiUsageRing {
    #[default]
    Highest,
    Session,
    Weekly,
}

/// `DockMetrics`: the single source of truth for dock geometry (all values DIPs).
pub mod dock_metrics {
    use super::{DockGapUnit, DockHeightMode, DockSize};

    pub const HEADROOM: f32 = 24.0;
    pub const BOTTOM_GAP: f32 = 6.0;
    pub const SIDE_INSET: f32 = 12.0;
    pub const FULL_WIDTH_SIDE_INSET: f32 = 6.0;
    pub const MAX_ICON_PADDING: f32 = 12.0;
    /// `MainWindow.Resizing`: `DockCellSize` (52) plus the horizontal frame chrome (10 + 10).
    pub const MIN_DOCK_SURFACE_WIDTH: f32 = 72.0;
    /// Extent used by the extent-free helpers; reference resolves against the target monitor.
    pub const DEFAULT_MONITOR_EXTENT: f32 = 1920.0;
    /// `DockMetrics.WidgetDesignHeight`.
    pub const WIDGET_DESIGN_HEIGHT: f32 = 46.0;
    pub const WIDGET_MIN_CARD_HEIGHT: f32 = 34.0;

    /// `DockSizeProfile`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct SizeProfile {
        pub shelf: f32,
        pub bottom_gap: f32,
        pub cell: f32,
        pub vertical_padding: f32,
        pub icon: f32,
        pub radius: f32,
        pub cell_height: f32,
        pub icon_padding: f32,
    }

    impl SizeProfile {
        /// Height used by the layout; icon padding widens cells horizontally only.
        pub fn effective_cell_height(self) -> f32 {
            if self.cell_height > 0.0 {
                self.cell_height
            } else {
                self.cell
            }
        }

        /// `DockSizeProfile.SurfaceHeight`: cell + 2 * vertical padding + 2 px border.
        pub fn surface_height(self) -> f32 {
            self.effective_cell_height() + self.vertical_padding * 2.0 + 2.0
        }

        /// `DockSizeProfile.WidgetScale`.
        pub fn widget_scale(self) -> f32 {
            ((self.effective_cell_height() - 2.0) / WIDGET_DESIGN_HEIGHT).clamp(0.45, 1.35)
        }
    }

    /// `DockMetrics.For(size, scale, padding)`. Compact/Medium/Large shelves are 48/58/66
    /// and their resting icon sizes are 28/34/38 DIPs at 100% scale.
    pub fn for_size(size: DockSize, scale: f64, padding: i32) -> SizeProfile {
        let scale = if scale.is_finite() {
            scale.clamp(0.75, 1.5)
        } else {
            1.0
        };
        let pad = (padding.max(0) as f32).min(MAX_ICON_PADDING);
        let base = match size {
            DockSize::Compact => SizeProfile {
                shelf: 48.0,
                bottom_gap: 3.0,
                cell: 38.0,
                vertical_padding: 2.0,
                icon: 28.0,
                radius: 16.0,
                cell_height: 0.0,
                icon_padding: 0.0,
            },
            DockSize::Medium => SizeProfile {
                shelf: 58.0,
                bottom_gap: 5.0,
                cell: 44.0,
                vertical_padding: 3.0,
                icon: 34.0,
                radius: 19.0,
                cell_height: 0.0,
                icon_padding: 0.0,
            },
            DockSize::Large => SizeProfile {
                shelf: 66.0,
                bottom_gap: 6.0,
                cell: 50.0,
                vertical_padding: 4.0,
                icon: 38.0,
                radius: 22.0,
                cell_height: 0.0,
                icon_padding: 0.0,
            },
        };
        if (scale - 1.0).abs() < 0.001 {
            return SizeProfile {
                cell: base.cell + pad,
                icon_padding: pad,
                ..base
            };
        }
        let factor = scale as f32;
        let cell_height = (base.cell * factor).round();
        let cell = cell_height + pad;
        let icon = (base.icon * factor).round();
        let vertical_padding = (base.vertical_padding * factor).round().max(2.0);
        let bottom_gap = (base.bottom_gap * factor).round().max(2.0);
        let surface = cell_height + vertical_padding * 2.0 + 2.0;
        let shelf = (surface + bottom_gap + 1.0).max(36.0);
        let radius = (base.radius * factor).round().clamp(10.0, 30.0);
        SizeProfile {
            shelf,
            bottom_gap,
            cell,
            vertical_padding,
            icon,
            radius,
            cell_height,
            icon_padding: pad,
        }
    }

    /// `DockMetrics.ResolveGap`.
    pub fn resolve_gap(unit: DockGapUnit, pixels: i32, percent: i32, extent: f32) -> f32 {
        let requested = match unit {
            DockGapUnit::Pixels => pixels as f32,
            DockGapUnit::Percent => extent.max(0.0) * percent.clamp(0, 100) as f32 / 100.0,
        };
        requested.max(0.0)
    }

    /// `DockMetrics.ResolveHeight`; `Auto`/`Fill` are handled by the caller.
    pub fn resolve_height(
        mode: DockHeightMode,
        pixels: i32,
        percent: i32,
        extent: f32,
        minimum: f32,
    ) -> f32 {
        let requested = match mode {
            DockHeightMode::Percent => extent.max(0.0) * percent.clamp(0, 100) as f32 / 100.0,
            _ => pixels as f32,
        };
        requested.max(minimum)
    }
}

/// `SystemMeters` flags. Bit 8 (CpuTemperature) and bit 4 (Battery) are retired but their
/// numeric values must stay stable in existing settings files.
pub mod stats_meters {
    pub const NONE: u64 = 0;
    pub const CPU: u64 = 1;
    pub const MEMORY: u64 = 2;
    pub const GPU: u64 = 16;
    pub const GPU_TEMPERATURE: u64 = 32;
    pub const ALL: u64 = CPU | MEMORY | GPU | GPU_TEMPERATURE;
}

/// `AiProviders` flags; the retired single "AiUsage" widget is split by these bits.
/// Bit 4 belonged to a provider removed from both builds and stays unused so older numeric
/// settings values keep their meaning for every other bit.
pub mod ai_providers {
    pub const CLAUDE: u32 = 1;
    pub const CODEX: u32 = 2;
    pub const OPEN_CODE: u32 = 8;
    pub const COPILOT: u32 = 16;
    pub const DEEP_SEEK: u32 = 32;
    pub const OPEN_ROUTER: u32 = 64;
    pub const CURSOR: u32 = 128;
    pub const ALL: u32 = CLAUDE | CODEX | OPEN_CODE | COPILOT | DEEP_SEEK | OPEN_ROUTER | CURSOR;
    /// Bit retired together with the removed provider; still masked out on load.
    pub const RETIRED_BIT: u32 = 4;

    /// Widget IDs in the same order as `DockWidgetService.AiUsageWidgets`.
    pub const WIDGETS: [(u32, &str); 7] = [
        (CLAUDE, "AiUsageClaude"),
        (CODEX, "AiUsageCodex"),
        (OPEN_CODE, "AiUsageOpenCode"),
        (COPILOT, "AiUsageCopilot"),
        (DEEP_SEEK, "AiUsageDeepSeek"),
        (OPEN_ROUTER, "AiUsageOpenRouter"),
        (CURSOR, "AiUsageCursor"),
    ];
}

/// `AiProviders` flag spellings shared by serde and legacy migration.
const AI_PROVIDER_NAMES: [(u32, &str); 7] = [
    (ai_providers::CLAUDE, "Claude"),
    (ai_providers::CODEX, "Codex"),
    (ai_providers::OPEN_CODE, "OpenCode"),
    (ai_providers::COPILOT, "Copilot"),
    (ai_providers::DEEP_SEEK, "DeepSeek"),
    (ai_providers::OPEN_ROUTER, "OpenRouter"),
    (ai_providers::CURSOR, "Cursor"),
];

/// A per-provider widget ID that is not one of the seven catalogued providers is retired data,
/// even when an old file still lists it. Comparing the compact form also covers aliases written
/// by earlier port builds (`aiusage_*`, `widget:AiUsage*`).
fn is_retired_ai_provider_widget(id: &str) -> bool {
    let compact: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let compact = compact.strip_prefix("widget").unwrap_or(&compact);
    let Some(rest) = compact.strip_prefix("aiusage") else {
        return false;
    };
    !rest.is_empty()
        && !AI_PROVIDER_NAMES.iter().any(|(_, name)| {
            let known: String = name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            known == rest
        })
}

/// All `DockWidgetService` IDs, including the hidden legacy aliases `Date` and `ShowDesktop`.
pub const SUPPORTED_WIDGET_IDS: [&str; 24] = [
    "Clock",
    "Weather",
    "DayProgress",
    "Stopwatch",
    "Countdown",
    "SystemStats",
    "NetSpeed",
    "Battery",
    "BluetoothBattery",
    "Storage",
    "AiUsageClaude",
    "AiUsageCodex",
    "AiUsageOpenCode",
    "AiUsageCopilot",
    "AiUsageDeepSeek",
    "AiUsageOpenRouter",
    "AiUsageCursor",
    "Media",
    "Audio",
    "QuickSettings",
    "KeyboardLayout",
    "Focus",
    "Date",
    "ShowDesktop",
];

/// Catalog cards only (`CatalogDescriptors`): used by island placement limits.
pub const CATALOG_WIDGET_COUNT: usize = 22;

const RETIRED_WIDGET_IDS: [&str; 4] = ["Memory", "Search", "Desktops", "Tray"];
const MAX_PINNED_TRAY_ICONS: usize = 64;
const MAX_PINNED_TRAY_KEY_LEN: usize = 512;
const MAX_ICON_PADDING: i32 = 12;

pub fn is_supported_widget_id(id: &str) -> bool {
    // Retired provider IDs are deliberately absent from the catalog, so old files that still
    // mention one are rejected here instead of being silently accepted by an alias lookup.
    SUPPORTED_WIDGET_IDS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(id))
}

/// Maps a UI catalog ID or legacy alias to the exact ID persisted by reference
/// `DockWidgetService` (`Widgets` / `InstalledWidgets` are validated against case-sensitive
/// PascalCase IDs). The Rust settings page uses snake_case catalog IDs such as `stats`,
/// `aiusage_claude` and `keyboard`; writing those verbatim made the next `load_from()` reject
/// the whole file as `UnsupportedWidgetId`, which is the "widgets vanished after adding a few"
/// failure. Normalizing every ID at the settings boundary keeps both builds reading the same
/// file and keeps `validate()`'s `enabled ⊆ installed` rule satisfiable.
pub fn canonical_widget_setting_id(id: &str) -> &str {
    let id = id.trim();
    if let Some(canonical) = SUPPORTED_WIDGET_IDS
        .iter()
        .find(|known| known.eq_ignore_ascii_case(id))
    {
        return canonical;
    }
    let compact: String = id
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    if let Some(canonical) = SUPPORTED_WIDGET_IDS.iter().find(|known| {
        known
            .chars()
            .filter(|c| *c != '_' && *c != '-')
            .flat_map(char::to_lowercase)
            .eq(compact.chars())
    }) {
        return canonical;
    }
    match compact.as_str() {
        "stats" => "SystemStats",
        "keyboard" => "KeyboardLayout",
        _ => id,
    }
}

/// Canonicalizes a free-form dock-order item. Only `widget:` tokens carry a widget
/// ID; shell/app/stack tokens are returned unchanged.
pub fn canonicalize_widget_token(item: &str) -> String {
    let trimmed = item.trim();
    let Some((prefix, id)) = trimmed.split_once(':') else {
        return trimmed.to_owned();
    };
    if !prefix.eq_ignore_ascii_case("widget") {
        return trimmed.to_owned();
    }
    format!("widget:{}", canonical_widget_setting_id(id))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DockAppGroup {
    pub id: String,
    pub name: String,
    pub executable_paths: Vec<String>,
}

/// A `DockProfile` snapshot. All reference properties are typed with defaults, and any future
/// property is preserved in `extra` for a lossless round-trip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct DockProfile {
    pub name: String,
    pub position: DockPosition,
    pub size: DockSize,
    pub auto_hide: AutoHideMode,
    /// reference calls this `Widgets`; the port's historic name is `EnabledWidgets`.
    #[serde(alias = "Widgets", deserialize_with = "null_as_empty")]
    pub enabled_widgets: Vec<String>,
    #[serde(deserialize_with = "null_as_empty")]
    pub hidden_apps: Vec<String>,
    #[serde(deserialize_with = "null_as_empty")]
    pub installed_widgets: Vec<String>,
    #[serde(deserialize_with = "null_as_empty")]
    pub widget_order: Vec<String>,
    #[serde(deserialize_with = "null_as_empty")]
    pub dock_item_order: Vec<String>,
    pub dock_width_mode: Option<DockWidthMode>,
    pub dock_width_pixels: Option<i32>,
    pub dock_width_percent: Option<i32>,
    pub theme: Option<AppTheme>,
    pub theme_pack: Option<String>,
    pub theme_application_icons: Option<bool>,
    pub launcher_hotkey: Option<HotkeyChoice>,
    pub dock_hotkey: Option<HotkeyChoice>,
    pub custom_launcher_hotkey: Option<String>,
    pub custom_dock_hotkey: Option<String>,
    /// Unknown keys from newer/older builds preserved on round-trip save.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for DockProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            position: DockPosition::default(),
            size: DockSize::default(),
            auto_hide: AutoHideMode::default(),
            enabled_widgets: Vec::new(),
            hidden_apps: Vec::new(),
            installed_widgets: Vec::new(),
            widget_order: Vec::new(),
            dock_item_order: Vec::new(),
            dock_width_mode: None,
            dock_width_pixels: None,
            dock_width_percent: None,
            theme: None,
            theme_pack: None,
            theme_application_icons: None,
            launcher_hotkey: None,
            dock_hotkey: None,
            custom_launcher_hotkey: None,
            custom_dock_hotkey: None,
            extra: Map::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct DockSettings {
    pub position: DockPosition,
    pub edge: DockEdge,
    pub size: DockSize,
    pub theme: AppTheme,
    pub auto_hide: AutoHideMode,
    pub dock_background: SurfaceBackground,
    pub dock_opacity: f64,
    pub dock_border_thickness: f64,
    pub dock_corner_radius: Option<i32>,
    #[serde(deserialize_with = "null_as_i32")]
    pub dock_bottom_gap_pixels: i32,
    pub dock_icon_padding: i32,
    pub dock_scale: f64,
    pub animations_enabled: bool,
    /// Target animation frame rate (`None` = monitor VSync/Hz, `Some(fps)` = `15..=monitor_hz`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation_fps: Option<u32>,
    pub magnification_enabled: bool,
    /// Whether the magnified icon may push its neighbours aside and grow the dock surface.
    /// reference feature request #182: older builds only scaled icons in place.
    #[serde(default = "default_true")]
    pub magnification_expands_dock: bool,
    #[serde(default)]
    pub fisheye_intensity: FisheyeIntensity,
    #[serde(default)]
    pub middle_click_action: MiddleClickAction,
    #[serde(alias = "FocusModeEnabled")]
    pub focus_mode: bool,
    pub show_recycle_bin: bool,
    pub show_tray_button: bool,
    pub show_on_all_monitors: bool,
    pub show_desktop_button: bool,
    /// reference persists `Layout` as the `DockLayout` enum string; the port stores a bool.
    #[serde(alias = "Layout", deserialize_with = "deserialize_layout_bool")]
    pub full_width: bool,
    pub dock_custom_width_dips: Option<f32>,
    pub reveal_dock_on_win_key: bool,
    pub current_desktop_only: bool,

    // ---- WIP settings kept from the pre-integration port ----
    /// BCP 47 UI culture. `AppLanguageConverter` values are normalized on load.
    #[serde(
        default = "default_language",
        deserialize_with = "deserialize_language"
    )]
    pub language: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub ui_font_family: String,
    #[serde(default = "default_font_size")]
    pub ui_font_size: f32,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub theme_pack: String,
    pub clock_24_hour: bool,
    pub clock_show_seconds: bool,
    pub date_style: ClockDateStyle,
    pub date_format: ClockDateFormat,
    pub clock_date_layout: ClockDateLayout,
    /// `SystemMeters` bit flags: Cpu=1, Memory=2, Gpu=16, GpuTemperature=32.
    #[serde(deserialize_with = "deserialize_stats_meters")]
    pub stats_meters: u32,
    pub global_widget_density: WidgetDensityMode,
    #[serde(default)]
    pub widget_flyout_trigger: WidgetFlyoutTrigger,
    #[serde(default = "default_true")]
    pub check_updates: bool,
    pub menu_background: SurfaceBackground,
    pub widget_background: SurfaceBackground,
    pub settings_background: SurfaceBackground,
    pub surface_opacity: f64,
    /// Clear Crystal optical glass blur sigma in DIPs (`0.0..=36.0`, default `9.0`).
    #[serde(default = "default_glass_blur_sigma")]
    pub glass_blur_sigma: f64,
    /// Clear Crystal color vibrancy multiplier (`0.80..=1.80`, default `1.32`).
    #[serde(default = "default_glass_saturation")]
    pub glass_saturation: f64,
    /// Clear Crystal razor 1 px optical edge highlight intensity (`0.0..=1.0`, default `0.88`).
    #[serde(default = "default_glass_edge_glow")]
    pub glass_edge_glow: f64,
    /// Clear Crystal cursor-tracking 3D edge light toggle (default `true`).
    #[serde(default = "default_true")]
    pub glass_pointer_light: bool,
    pub dock_border_color: Option<String>,
    pub menu_border_thickness: f64,
    pub menu_border_color: Option<String>,
    pub widget_border_thickness: f64,
    pub widget_border_color: Option<String>,
    pub settings_border_thickness: f64,
    pub settings_border_color: Option<String>,
    /// Island widget placements: widget id -> display name (`"*"` = all displays).
    #[serde(
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub dynamic_island_placements: HashMap<String, String>,
    pub dynamic_island_visibility: DynamicIslandVisibilityMode,
    #[serde(default = "default_true")]
    pub dynamic_island_hide_in_fullscreen: bool,
    /// reference writes `DynamicIslandEnabled`; the port's historic spelling is `DynamicIslandEnable`.
    #[serde(alias = "DynamicIslandEnabled", default = "default_true")]
    pub dynamic_island_enable: bool,
    pub dynamic_island_auto_hide: bool,
    pub dynamic_island_background: SurfaceBackground,
    /// `DynamicIslandBackgroundColor` (`#RRGGBB`). Kept as text so an invalid value from an
    /// older build is preserved; `dynamic_island_background_hex()` validates before use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_island_background_color: Option<String>,
    #[serde(default = "default_dynamic_island_opacity")]
    pub dynamic_island_opacity: f64,
    #[serde(default = "default_true")]
    pub dynamic_island_use_theme: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub dynamic_island_widget_orders: HashMap<String, Vec<String>>,

    #[serde(alias = "ShowClock")]
    pub show_clock_widget: bool,
    #[serde(alias = "ShowSystemStats")]
    pub show_system_stats_widget: bool,
    /// Explicitly enabled widget IDs. reference writes this key as `Widgets`.
    #[serde(alias = "Widgets", deserialize_with = "null_as_empty")]
    pub enabled_widgets: Vec<String>,
    pub weather_city: String,
    #[serde(deserialize_with = "null_as_empty")]
    pub stack_folders: Vec<String>,
    #[serde(deserialize_with = "null_as_empty_groups")]
    pub app_groups: Vec<DockAppGroup>,
    #[serde(deserialize_with = "null_as_empty_profiles")]
    pub profiles: Vec<DockProfile>,
    pub active_profile: Option<String>,
    pub pin_order: Option<Vec<String>>,
    #[serde(deserialize_with = "null_as_empty")]
    pub user_pinned_apps: Vec<String>,
    #[serde(deserialize_with = "null_as_empty")]
    pub user_unpinned_apps: Vec<String>,
    /// reference persistent tray pins (`PinnedTrayIcons`) map onto this historic port field.
    #[serde(
        alias = "PinnedTrayIcons",
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub user_pinned_background_apps: Vec<String>,

    // ---- 49 reference properties added for full settings parity ----
    pub previews_enabled: bool,
    pub notification_badges_enabled: bool,
    pub menu_opacity: f64,
    pub widget_opacity: f64,
    pub settings_opacity: f64,
    #[serde(deserialize_with = "null_as_empty")]
    pub installed_widgets: Vec<String>,
    pub content_alignment: DockContentAlignment,
    pub show_media: bool,
    pub show_focus: bool,
    pub show_audio: bool,
    pub show_quick_settings: bool,
    pub focus_minutes: i32,
    pub focus_break_minutes: i32,
    pub focus_sessions: i32,
    pub start_action: StartButtonAction,
    pub start_placement: StartButtonPlacement,
    pub start_widget_index: i32,
    pub wheel_volume_enabled: bool,
    pub show_weather: bool,
    pub target_display: Option<String>,
    pub secondary_apps: SecondaryAppScope,
    pub secondary_widgets: bool,
    #[serde(deserialize_with = "null_as_empty")]
    pub widget_order: Vec<String>,
    pub theme_application_icons: bool,
    #[serde(deserialize_with = "null_as_density_map")]
    pub widget_density_overrides: BTreeMap<String, WidgetDensityMode>,
    #[serde(deserialize_with = "null_as_empty")]
    pub dock_item_order: Vec<String>,
    #[serde(
        deserialize_with = "deserialize_ai_providers",
        serialize_with = "serialize_ai_providers"
    )]
    pub ai_usage_providers: u32,
    pub ai_usage_ring: AiUsageRing,
    #[serde(default = "default_true")]
    pub show_start_button: bool,
    pub show_search_button: bool,
    pub show_task_view_button: bool,
    pub dock_width_mode: DockWidthMode,
    pub dock_width_pixels: i32,
    pub dock_width_percent: i32,
    pub dock_top_gap_unit: DockGapUnit,
    pub dock_top_gap_pixels: i32,
    pub dock_top_gap_percent: i32,
    pub dock_bottom_gap_unit: DockGapUnit,
    pub dock_bottom_gap_percent: i32,
    pub dock_height_mode: DockHeightMode,
    pub dock_height_pixels: i32,
    pub dock_height_percent: i32,
    pub wireless_battery_poll_seconds: i32,
    pub wireless_battery_low_threshold: i32,
    #[serde(deserialize_with = "null_as_empty")]
    pub ignored_wireless_devices: Vec<String>,
    #[serde(deserialize_with = "null_as_alias_map")]
    pub wireless_device_aliases: BTreeMap<String, String>,
    pub wireless_battery_listen_only: bool,
    pub weather_unit: TemperatureUnit,
    pub mirror_main_dock: bool,
    /// `show_fjord_files_button`: dock visibility of the Fjord Files button. The dock host
    /// consumes this key in a separate workstream; the schema and Settings UI already own it.
    #[serde(default)]
    pub show_fjord_files_button: bool,

    // Hotkeys are needed by `validate()` (`IsValid` rejects launcher/dock conflicts).
    pub launcher_hotkey: HotkeyChoice,
    pub dock_hotkey: HotkeyChoice,
    pub custom_launcher_hotkey: Option<String>,
    pub custom_dock_hotkey: Option<String>,

    // ---- OLED / display care (reference DockSettings tail properties) ----
    /// Suppresses the spatial glass edge glow and cursor light on OLED panels.
    #[serde(default)]
    pub oled_reduce_glow_enabled: bool,
    /// Periodically nudges static content to reduce burn-in.
    #[serde(default)]
    pub burn_in_shift_enabled: bool,
    /// Interval between burn-in shifts in minutes (`5..=120`, default `30`).
    /// Field-level `serde(default)` must use this helper: a bare `i32::default()` is `0`.
    #[serde(default = "default_burn_in_shift_minutes")]
    pub burn_in_shift_minutes: i32,
    #[serde(default)]
    pub oled_auto_trigger_enabled: bool,
    /// Battery level that automatically enables OLED care; `0` disables the trigger.
    #[serde(default)]
    pub oled_low_battery_threshold: i32,
    #[serde(default)]
    pub oled_auto_on_night_light: bool,
    #[serde(default)]
    pub ambient_mode_enabled: bool,
    /// Separate animation frame cap (`0` = unlimited, `30`/`60`/`120` FPS).
    /// `animation_fps` above is the older VSync-relative setting and stays independent.
    #[serde(default)]
    pub animation_fps_cap: i32,

    /// Unknown keys from other OpenTaskbar builds preserved on round-trip save.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for DockSettings {
    fn default() -> Self {
        Self {
            position: DockPosition::Center,
            edge: DockEdge::Bottom,
            size: DockSize::Compact,
            theme: AppTheme::System,
            auto_hide: AutoHideMode::Disabled,
            dock_background: SurfaceBackground::Glass,
            dock_opacity: 0.9,
            dock_border_thickness: 1.0,
            dock_corner_radius: None,
            dock_bottom_gap_pixels: 6,
            dock_icon_padding: 0,
            dock_scale: 1.0,
            animations_enabled: true,
            animation_fps: None,
            magnification_enabled: true,
            magnification_expands_dock: true,
            fisheye_intensity: FisheyeIntensity::Normal,
            middle_click_action: MiddleClickAction::NewWindow,
            focus_mode: false,
            show_recycle_bin: true,
            show_tray_button: true,
            show_on_all_monitors: true,
            show_desktop_button: true,
            full_width: false,
            dock_custom_width_dips: None,
            reveal_dock_on_win_key: true,
            current_desktop_only: false,

            language: default_language(),
            ui_font_family: String::new(),
            ui_font_size: default_font_size(),
            theme_pack: String::new(),
            clock_24_hour: true,
            clock_show_seconds: false,
            date_style: ClockDateStyle::DayAndShortDate,
            date_format: ClockDateFormat::System,
            clock_date_layout: ClockDateLayout::Beside,
            stats_meters: stats_meters::ALL as u32,
            global_widget_density: WidgetDensityMode::Auto,
            widget_flyout_trigger: WidgetFlyoutTrigger::Click,
            check_updates: true,
            menu_background: SurfaceBackground::Glass,
            widget_background: SurfaceBackground::Glass,
            settings_background: SurfaceBackground::Glass,
            surface_opacity: 0.9,
            glass_blur_sigma: default_glass_blur_sigma(),
            glass_saturation: default_glass_saturation(),
            glass_edge_glow: default_glass_edge_glow(),
            glass_pointer_light: true,
            dock_border_color: None,
            menu_border_thickness: 1.0,
            menu_border_color: None,
            widget_border_thickness: 1.0,
            widget_border_color: None,
            settings_border_thickness: 1.0,
            settings_border_color: None,
            dynamic_island_placements: HashMap::new(),
            dynamic_island_visibility: DynamicIslandVisibilityMode::Always,
            dynamic_island_hide_in_fullscreen: true,
            dynamic_island_enable: true,
            dynamic_island_auto_hide: false,
            dynamic_island_background: SurfaceBackground::Glass,
            dynamic_island_background_color: None,
            dynamic_island_opacity: default_dynamic_island_opacity(),
            dynamic_island_use_theme: true,
            dynamic_island_widget_orders: HashMap::new(),

            show_clock_widget: false,
            show_system_stats_widget: false,
            enabled_widgets: Vec::new(),
            weather_city: String::new(),
            stack_folders: Vec::new(),
            app_groups: Vec::new(),
            profiles: Vec::new(),
            active_profile: None,
            pin_order: None,
            user_pinned_apps: Vec::new(),
            user_unpinned_apps: Vec::new(),
            user_pinned_background_apps: Vec::new(),

            previews_enabled: true,
            notification_badges_enabled: true,
            menu_opacity: 0.9,
            widget_opacity: 0.9,
            settings_opacity: 0.9,
            installed_widgets: Vec::new(),
            content_alignment: DockContentAlignment::Center,
            show_media: false,
            show_focus: false,
            show_audio: false,
            show_quick_settings: false,
            focus_minutes: 25,
            focus_break_minutes: 5,
            focus_sessions: 4,
            start_action: StartButtonAction::WindowsStart,
            start_placement: StartButtonPlacement::Start,
            start_widget_index: 0,
            wheel_volume_enabled: false,
            show_weather: false,
            target_display: None,
            secondary_apps: SecondaryAppScope::CurrentMonitorOnly,
            secondary_widgets: true,
            widget_order: Vec::new(),
            theme_application_icons: true,
            widget_density_overrides: BTreeMap::new(),
            dock_item_order: Vec::new(),
            ai_usage_providers: 0,
            ai_usage_ring: AiUsageRing::Highest,
            show_start_button: true,
            show_search_button: true,
            show_task_view_button: true,
            dock_width_mode: DockWidthMode::Auto,
            dock_width_pixels: 900,
            dock_width_percent: 60,
            dock_top_gap_unit: DockGapUnit::Pixels,
            dock_top_gap_pixels: 0,
            dock_top_gap_percent: 0,
            dock_bottom_gap_unit: DockGapUnit::Pixels,
            dock_bottom_gap_percent: 0,
            dock_height_mode: DockHeightMode::Auto,
            dock_height_pixels: 48,
            dock_height_percent: 7,
            wireless_battery_poll_seconds: 30,
            wireless_battery_low_threshold: 0,
            ignored_wireless_devices: Vec::new(),
            wireless_device_aliases: BTreeMap::new(),
            wireless_battery_listen_only: false,
            weather_unit: TemperatureUnit::Celsius,
            mirror_main_dock: false,
            show_fjord_files_button: false,

            launcher_hotkey: HotkeyChoice::AltSpace,
            dock_hotkey: HotkeyChoice::CtrlAltD,
            custom_launcher_hotkey: None,
            custom_dock_hotkey: None,

            oled_reduce_glow_enabled: false,
            burn_in_shift_enabled: false,
            burn_in_shift_minutes: 30,
            oled_auto_trigger_enabled: false,
            oled_low_battery_threshold: 0,
            oled_auto_on_night_light: false,
            ambient_mode_enabled: false,
            animation_fps_cap: 0,

            extra: Map::new(),
        }
    }
}

// ---- serde helpers: reference writes `null` for unset lists/maps and `Layout` as an enum string ----

fn null_as_empty<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
}

fn null_as_empty_groups<'de, D>(deserializer: D) -> Result<Vec<DockAppGroup>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<Vec<DockAppGroup>>::deserialize(deserializer)?.unwrap_or_default())
}

fn null_as_empty_profiles<'de, D>(deserializer: D) -> Result<Vec<DockProfile>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<Vec<DockProfile>>::deserialize(deserializer)?.unwrap_or_default())
}

fn null_as_i32<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<i32>::deserialize(deserializer)?.unwrap_or(6))
}

fn null_as_density_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, WidgetDensityMode>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        Option::<BTreeMap<String, WidgetDensityMode>>::deserialize(deserializer)?
            .unwrap_or_default(),
    )
}

fn null_as_alias_map<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<BTreeMap<String, String>>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_layout_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    struct LayoutVisitor;
    impl<'de> Visitor<'de> for LayoutVisitor {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a bool or reference DockLayout string (Floating/FullWidth)")
        }
        fn visit_bool<E: DeError>(self, value: bool) -> Result<bool, E> {
            Ok(value)
        }
        fn visit_str<E: DeError>(self, value: &str) -> Result<bool, E> {
            if value.eq_ignore_ascii_case("FullWidth") {
                Ok(true)
            } else if value.eq_ignore_ascii_case("Floating") {
                Ok(false)
            } else {
                Err(E::custom("unknown DockLayout value"))
            }
        }
        fn visit_unit<E: DeError>(self) -> Result<bool, E> {
            Ok(false)
        }
    }
    deserializer.deserialize_any(LayoutVisitor)
}

// `JsonStringEnumConverter` writes `[Flags]` enums as "None", "All" or "A, B"; older files
// sometimes carry raw integers. Both representations must round-trip.
fn parse_flag_names<'de, D>(
    deserializer: D,
    names: &[(u32, &str)],
    all: u32,
) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Number(number) => Ok(number.as_u64().unwrap_or(0) as u32),
        Value::Null => Ok(0),
        Value::String(text) => {
            let mut bits = 0u32;
            for token in text.split(',').map(str::trim) {
                if token.is_empty() || token.eq_ignore_ascii_case("None") {
                    continue;
                }
                if token.eq_ignore_ascii_case("All") {
                    bits |= all;
                    continue;
                }
                match names
                    .iter()
                    .find(|(_, name)| name.eq_ignore_ascii_case(token))
                {
                    Some((bit, _)) => bits |= *bit,
                    None => return Err(D::Error::custom("unknown flag name in settings")),
                }
            }
            Ok(bits)
        }
        _ => Err(D::Error::custom("flags must be a number or enum string")),
    }
}

fn write_flag_names<S>(
    value: &u32,
    serializer: S,
    names: &[(u32, &str)],
    all: u32,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let text = if *value == 0 {
        "None".to_owned()
    } else if *value == all {
        "All".to_owned()
    } else {
        names
            .iter()
            .filter(|(bit, _)| value & *bit != 0)
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    serializer.serialize_str(&text)
}

fn deserialize_ai_providers<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    parse_flag_names(deserializer, &AI_PROVIDER_NAMES, ai_providers::ALL)
}

fn serialize_ai_providers<S>(value: &u32, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    write_flag_names(value, serializer, &AI_PROVIDER_NAMES, ai_providers::ALL)
}

/// Drops flag tokens and bit slots that no longer belong to any provider so an old
/// `AiUsageProviders` value cannot fail deserialization or `validate()`.
fn sanitize_ai_provider_flags(map: &mut Map<String, Value>) {
    let Some(value) = map.get_mut("AiUsageProviders") else {
        return;
    };
    match value {
        Value::Number(number) => {
            if let Some(bits) = number.as_u64() {
                let kept = bits & !(ai_providers::RETIRED_BIT as u64);
                if kept != bits {
                    *number = serde_json::Number::from(kept);
                }
            }
        }
        Value::String(text) => {
            let kept: Vec<&str> = text
                .split(',')
                .map(str::trim)
                .filter(|token| {
                    token.is_empty()
                        || token.eq_ignore_ascii_case("None")
                        || token.eq_ignore_ascii_case("All")
                        || AI_PROVIDER_NAMES
                            .iter()
                            .any(|(_, name)| token.eq_ignore_ascii_case(name))
                })
                .collect();
            let replacement = if kept.is_empty() {
                "None".to_owned()
            } else {
                kept.join(", ")
            };
            if replacement != *text {
                *text = replacement;
            }
        }
        _ => {}
    }
}

impl DockSettings {
    /// Effective launcher hotkey: custom text wins over the persisted enum choice.
    /// `"Disabled"`, `"Backspace"`, `"Esc"` and `"Escape"` are explicit opt-outs and never
    /// fall back to the legacy binding (WIP behavior).
    pub fn launcher_binding(&self) -> Option<crate::hotkey::HotkeyBinding> {
        if self
            .custom_launcher_hotkey
            .as_deref()
            .is_some_and(is_disabled_hotkey)
        {
            return None;
        }
        self.custom_launcher_hotkey
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .and_then(crate::hotkey::parse)
            .or_else(|| HotkeyBinding::from_choice(self.launcher_hotkey))
    }

    /// Effective dock hotkey: custom text wins over the persisted enum choice.
    pub fn dock_binding(&self) -> Option<crate::hotkey::HotkeyBinding> {
        if self
            .custom_dock_hotkey
            .as_deref()
            .is_some_and(is_disabled_hotkey)
        {
            return None;
        }
        self.custom_dock_hotkey
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .and_then(crate::hotkey::parse)
            .or_else(|| HotkeyBinding::from_choice(self.dock_hotkey))
    }

    /// Effective island background colour (`DynamicIslandBackgroundColor`). The typed field
    /// wins; older builds kept the hex string in `extra`, so that key is the fallback. Invalid
    /// values are ignored rather than being handed to a renderer as a colour.
    pub fn dynamic_island_background_hex(&self) -> Option<&str> {
        let raw = self
            .dynamic_island_background_color
            .as_deref()
            .or_else(|| {
                self.extra
                    .get("DynamicIslandBackgroundColor")
                    .and_then(Value::as_str)
            })?
            .trim();
        let digits = raw.strip_prefix('#').unwrap_or(raw);
        (digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(raw)
    }

    pub fn is_widget_enabled(&self, id: &str) -> bool {
        let id = canonical_widget_setting_id(id);
        // `WidgetCoordinator.Apply` deactivates every widget while Focus Mode is on. Clock and
        // Focus stay available in this port because the focus timer is the mode's visible control.
        if self.focus_mode && !id.eq_ignore_ascii_case("Focus") && !id.eq_ignore_ascii_case("Clock")
        {
            return false;
        }
        if id.eq_ignore_ascii_case("Clock") && self.show_clock_widget {
            return true;
        }
        if id.eq_ignore_ascii_case("SystemStats") && self.show_system_stats_widget {
            return true;
        }
        // Compare canonical IDs so files written by older port builds (lowercase/snake_case) and
        // files written by reference (PascalCase) both resolve to the same widget.
        self.enabled_widgets
            .iter()
            .any(|w| canonical_widget_setting_id(w).eq_ignore_ascii_case(id))
    }

    pub fn set_widget_enabled(&mut self, id: &str, enabled: bool) {
        let id = canonical_widget_setting_id(id);
        if id.eq_ignore_ascii_case("Clock") {
            self.show_clock_widget = enabled;
        }
        if id.eq_ignore_ascii_case("SystemStats") {
            self.show_system_stats_widget = enabled;
        }
        let is_same = |w: &String| canonical_widget_setting_id(w).eq_ignore_ascii_case(id);
        if enabled {
            if !self.enabled_widgets.iter().any(is_same) {
                self.enabled_widgets.push(id.to_owned());
            }
            // reference keeps a widget installed when only its switch is turned off, so enabling one
            // must also add it to `InstalledWidgets` or the next `Validate()` rejects the file
            // and `load_from()` resets every setting to defaults.
            if !self.installed_widgets.iter().any(is_same) {
                self.installed_widgets.push(id.to_owned());
            }
        } else {
            self.enabled_widgets.retain(|w| !is_same(w));
        }
    }
}

impl DockSettings {
    /// Icon resting size in DIPs for the active size profile.
    pub fn icon_dips(&self) -> f32 {
        let base = match self.size {
            DockSize::Compact => 36.0,
            DockSize::Medium => 44.0,
            DockSize::Large => 52.0,
        };
        (base * self.dock_scale.clamp(0.75, 1.5) as f32).round()
    }

    /// Corner radius in DIPs.
    pub fn corner_radius_dips(&self) -> f32 {
        match self.dock_corner_radius {
            Some(r) => r.clamp(0, 32) as f32,
            None => match self.size {
                DockSize::Compact => 16.0,
                DockSize::Medium => 18.0,
                DockSize::Large => 20.0,
            },
        }
    }

    /// `DockMetrics.For` for the current size/scale/icon-padding settings.
    pub fn size_profile(&self) -> dock_metrics::SizeProfile {
        dock_metrics::for_size(self.size, self.dock_scale, self.dock_icon_padding)
    }

    pub fn effective_width(&self, monitor_w: f32) -> f32 {
        effective_width(self, monitor_w)
    }

    pub fn effective_height(&self) -> f32 {
        effective_height(self)
    }

    pub fn effective_height_for(&self, extent: f32) -> f32 {
        effective_height_for(self, extent)
    }

    pub fn effective_top_gap(&self) -> i32 {
        effective_top_gap(self)
    }

    pub fn effective_top_gap_for(&self, extent: f32) -> i32 {
        effective_top_gap_for(self, extent)
    }

    pub fn effective_bottom_gap(&self) -> i32 {
        effective_bottom_gap(self)
    }

    pub fn effective_bottom_gap_for(&self, extent: f32) -> i32 {
        effective_bottom_gap_for(self, extent)
    }

    /// Effective density for a widget: an override (case-insensitive) wins over the global mode.
    pub fn resolve_widget_density(&self, id: &str) -> WidgetDensityMode {
        resolve_widget_density(self, id)
    }

    /// Width multiplier used by widget layout for the resolved density.
    pub fn widget_density_for(&self, id: &str) -> f32 {
        widget_density_for(self, id)
    }

    /// Applies every profile-scoped field at once (`DockProfileService.ApplyProfile`).
    pub fn apply_full_profile(&mut self, profile: &DockProfile) {
        apply_full_profile(self, profile);
    }
}

/// Resolves the dock width in DIPs. `Auto` uses the content-fitted width when one was saved,
/// `Fill` spans the whole monitor, and fixed modes clamp to the reference minimum/maximum.
pub fn effective_width(settings: &DockSettings, monitor_w: f32) -> f32 {
    let extent = monitor_w.max(0.0);
    let maximum = extent.max(dock_metrics::MIN_DOCK_SURFACE_WIDTH);
    match settings.dock_width_mode {
        DockWidthMode::Auto => settings
            .dock_custom_width_dips
            .filter(|width| width.is_finite() && *width > 0.0)
            .unwrap_or(dock_metrics::MIN_DOCK_SURFACE_WIDTH),
        DockWidthMode::Pixels => {
            (settings.dock_width_pixels as f32).clamp(dock_metrics::MIN_DOCK_SURFACE_WIDTH, maximum)
        }
        DockWidthMode::Percent => {
            let requested = extent * settings.dock_width_percent.clamp(0, 100) as f32 / 100.0;
            requested.clamp(dock_metrics::MIN_DOCK_SURFACE_WIDTH, maximum)
        }
        DockWidthMode::Fill => extent,
    }
}

/// Resolves the thickness of the horizontal bar in DIPs against the default monitor extent.
pub fn effective_height(settings: &DockSettings) -> f32 {
    effective_height_for(settings, dock_metrics::DEFAULT_MONITOR_EXTENT)
}

/// Same as `effective_height` but resolves percentages against a real monitor extent.
pub fn effective_height_for(settings: &DockSettings, extent: f32) -> f32 {
    let minimum = settings.size_profile().surface_height();
    match settings.dock_height_mode {
        DockHeightMode::Auto | DockHeightMode::Fill => minimum,
        DockHeightMode::Pixels | DockHeightMode::Percent => dock_metrics::resolve_height(
            settings.dock_height_mode,
            settings.dock_height_pixels,
            settings.dock_height_percent,
            extent,
            minimum,
        ),
    }
}

/// External top gap in pixels, resolved against the default monitor extent.
pub fn effective_top_gap(settings: &DockSettings) -> i32 {
    effective_top_gap_for(settings, dock_metrics::DEFAULT_MONITOR_EXTENT)
}

pub fn effective_top_gap_for(settings: &DockSettings, extent: f32) -> i32 {
    dock_metrics::resolve_gap(
        settings.dock_top_gap_unit,
        settings.dock_top_gap_pixels,
        settings.dock_top_gap_percent,
        extent,
    )
    .round()
    .max(0.0) as i32
}

/// External bottom gap in pixels, resolved against the default monitor extent.
pub fn effective_bottom_gap(settings: &DockSettings) -> i32 {
    effective_bottom_gap_for(settings, dock_metrics::DEFAULT_MONITOR_EXTENT)
}

pub fn effective_bottom_gap_for(settings: &DockSettings, extent: f32) -> i32 {
    dock_metrics::resolve_gap(
        settings.dock_bottom_gap_unit,
        settings.dock_bottom_gap_pixels,
        settings.dock_bottom_gap_percent,
        extent,
    )
    .round()
    .max(0.0) as i32
}

/// `WidgetCoordinator.ResolveWidgetDensity`: override first, global mode otherwise.
pub fn resolve_widget_density(settings: &DockSettings, id: &str) -> WidgetDensityMode {
    settings
        .widget_density_overrides
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(id))
        .map(|(_, mode)| *mode)
        .unwrap_or(settings.global_widget_density)
}

/// Layout multiplier for a widget: compact cards are 82% of the full width.
pub fn widget_density_for(settings: &DockSettings, id: &str) -> f32 {
    match resolve_widget_density(settings, id) {
        WidgetDensityMode::Compact => 0.82,
        WidgetDensityMode::Auto | WidgetDensityMode::Full => 1.0,
    }
}

/// `DockWidget.ShrinkPriority`: lower values are compacted first when the dock overflows.
/// 100 is the `DockWidget` base value for widgets without a specific priority.
pub fn widget_shrink_priority(id: &str) -> u8 {
    let compact: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match compact.as_str() {
        "media" => 10,
        "weather" => 15,
        "audio" => 18,
        "quicksettings" => 20,
        "systemstats" | "netspeed" => 30,
        "bluetoothbattery" => 32,
        "storage" => 35,
        "battery" => 36,
        "aiusage" => 40,
        "focus" => 50,
        "dayprogress" => 60,
        "keyboardlayout" => 75,
        "clock" => 80,
        _ if compact.starts_with("aiusage") => 40,
        _ => 100,
    }
}

fn canonical_widget_list(ids: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    ids.iter()
        .map(|id| canonical_widget_setting_id(id).to_owned())
        .filter(|id| seen.insert(ordinal_ignore_case_key(id)))
        .collect()
}

/// Applies all profile-scoped fields (the 15 `DockProfile` properties). A profile whose
/// scoped fields are all unset only activates its name, matching the reference legacy-profile guard.
pub fn apply_full_profile(settings: &mut DockSettings, profile: &DockProfile) {
    let legacy = profile.enabled_widgets.is_empty()
        && profile.installed_widgets.is_empty()
        && profile.widget_order.is_empty()
        && profile.dock_item_order.is_empty()
        && profile.dock_width_mode.is_none()
        && profile.dock_width_pixels.is_none()
        && profile.dock_width_percent.is_none()
        && profile.theme.is_none()
        && profile.theme_pack.is_none()
        && profile.theme_application_icons.is_none()
        && profile.launcher_hotkey.is_none()
        && profile.dock_hotkey.is_none()
        && profile.custom_launcher_hotkey.is_none()
        && profile.custom_dock_hotkey.is_none();

    settings.active_profile = Some(profile.name.clone());
    if legacy {
        return;
    }

    settings.position = profile.position;
    settings.size = profile.size;
    if !profile.enabled_widgets.is_empty() {
        settings.enabled_widgets = canonical_widget_list(&profile.enabled_widgets);
    }
    if !profile.installed_widgets.is_empty() {
        let mut installed = canonical_widget_list(&profile.installed_widgets);
        for id in &settings.enabled_widgets {
            if !installed.iter().any(|known| known.eq_ignore_ascii_case(id)) {
                installed.push(id.clone());
            }
        }
        settings.installed_widgets = installed;
    }
    if settings.installed_widgets.is_empty() {
        settings.installed_widgets = settings.enabled_widgets.clone();
    }
    if !profile.widget_order.is_empty() {
        settings.widget_order = canonical_widget_list(&profile.widget_order);
    }
    if !profile.dock_item_order.is_empty() {
        settings.dock_item_order = profile.dock_item_order.clone();
    }

    if let Some(mode) = profile.dock_width_mode {
        settings.dock_width_mode = mode;
    }
    if let Some(pixels) = profile.dock_width_pixels {
        settings.dock_width_pixels = pixels;
    }
    if let Some(percent) = profile.dock_width_percent {
        settings.dock_width_percent = percent;
    }
    if let Some(theme) = profile.theme {
        settings.theme = theme;
    }
    settings.theme_pack = profile.theme_pack.clone().unwrap_or_default();
    if let Some(icons) = profile.theme_application_icons {
        settings.theme_application_icons = icons;
    }

    let previous_launcher_custom = settings.custom_launcher_hotkey.clone();
    let previous_dock_custom = settings.custom_dock_hotkey.clone();
    if let Some(hotkey) = profile.launcher_hotkey {
        settings.launcher_hotkey = hotkey;
    }
    if let Some(hotkey) = profile.dock_hotkey {
        settings.dock_hotkey = hotkey;
    }
    settings.custom_launcher_hotkey = profile.custom_launcher_hotkey.clone().or_else(|| {
        if profile.launcher_hotkey.is_some() {
            None
        } else {
            previous_launcher_custom
        }
    });
    settings.custom_dock_hotkey = profile.custom_dock_hotkey.clone().or_else(|| {
        if profile.dock_hotkey.is_some() {
            None
        } else {
            previous_dock_custom
        }
    });
    settings.auto_hide = profile.auto_hide;

    repair_widget_lists(
        &mut settings.enabled_widgets,
        &mut settings.installed_widgets,
    );
}

// ---- Legacy migration (reference DockSettingsStore.NormalizeLegacyJson + Migrate*) ----

fn is_retired_widget(id: &str) -> bool {
    RETIRED_WIDGET_IDS
        .iter()
        .any(|retired| id.eq_ignore_ascii_case(retired))
}

fn map_string_array(value: &mut Value, mut convert: impl FnMut(&str) -> Vec<String>) {
    let Some(items) = value.as_array_mut() else {
        return;
    };
    let mut next = Vec::with_capacity(items.len());
    for item in items.iter() {
        match item.as_str() {
            Some(text) => next.extend(convert(text).into_iter().map(Value::String)),
            None => next.push(item.clone()),
        }
    }
    *items = next;
}

fn filter_object_keys(value: &mut Value, mut keep: impl FnMut(&str) -> bool) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    map.retain(|key, _| keep(key));
}

fn array_mentions(value: Option<&Value>, needle: &str) -> bool {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items.iter().any(|item| {
                item.as_str().is_some_and(|s| {
                    s.eq_ignore_ascii_case(needle)
                        || s.eq_ignore_ascii_case(&format!("widget:{needle}"))
                })
            })
        })
        .unwrap_or(false)
}

fn ai_bits(root: &Map<String, Value>) -> u32 {
    match root.get("AiUsageProviders") {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(0) as u32,
        Some(Value::String(text)) => ai_providers::WIDGETS
            .iter()
            .filter(|(_, id)| text.eq_ignore_ascii_case(id))
            .map(|(bit, _)| *bit)
            .fold(0, |a, b| a | b),
        _ => 0,
    }
}

fn ai_replacement(id: &str, enabled: &[String]) -> Vec<String> {
    if !id.eq_ignore_ascii_case("AiUsage") {
        return vec![id.to_owned()];
    }
    enabled.to_vec()
}

fn is_retired_list_widget(id: &str) -> bool {
    is_retired_widget(id) || is_retired_ai_provider_widget(id)
}

fn migrate_widget_lists(map: &mut Map<String, Value>) {
    // Remember what the legacy widget meant before removing it (reference mirrors these into buttons).
    let mentioned = |id: &str| {
        [
            "Widgets",
            "EnabledWidgets",
            "InstalledWidgets",
            "WidgetOrder",
            "DockItemOrder",
        ]
        .iter()
        .any(|key| array_mentions(map.get(*key), id))
    };
    let mentioned_search = mentioned("Search");
    let mentioned_desktops = mentioned("Desktops");
    let mentioned_tray = mentioned("Tray");
    let enabled_search = array_mentions(map.get("Widgets"), "Search")
        || array_mentions(map.get("EnabledWidgets"), "Search");
    let enabled_desktops = array_mentions(map.get("Widgets"), "Desktops")
        || array_mentions(map.get("EnabledWidgets"), "Desktops");
    let enabled_tray = array_mentions(map.get("Widgets"), "Tray")
        || array_mentions(map.get("EnabledWidgets"), "Tray");

    for key in [
        "Widgets",
        "EnabledWidgets",
        "InstalledWidgets",
        "WidgetOrder",
    ] {
        if let Some(value) = map.get_mut(key) {
            map_string_array(value, |id| {
                if is_retired_list_widget(id) {
                    Vec::new()
                } else {
                    vec![id.to_owned()]
                }
            });
        }
    }
    if let Some(value) = map.get_mut("DockItemOrder") {
        map_string_array(value, |id| {
            if is_retired_ai_provider_widget(id)
                || RETIRED_WIDGET_IDS
                    .iter()
                    .any(|r| id.eq_ignore_ascii_case(&format!("widget:{r}")))
            {
                Vec::new()
            } else {
                vec![id.to_owned()]
            }
        });
    }
    if let Some(value) = map.get_mut("WidgetDensityOverrides") {
        filter_object_keys(value, |key| !is_retired_list_widget(key));
    }
    if let Some(profiles) = map.get_mut("Profiles").and_then(Value::as_array_mut) {
        for profile in profiles.iter_mut() {
            if let Some(p) = profile.as_object_mut() {
                for key in [
                    "Widgets",
                    "EnabledWidgets",
                    "InstalledWidgets",
                    "WidgetOrder",
                ] {
                    if let Some(value) = p.get_mut(key) {
                        map_string_array(value, |id| {
                            if is_retired_list_widget(id) {
                                Vec::new()
                            } else {
                                vec![id.to_owned()]
                            }
                        });
                    }
                }
                if let Some(value) = p.get_mut("DockItemOrder") {
                    map_string_array(value, |id| {
                        if is_retired_ai_provider_widget(id)
                            || RETIRED_WIDGET_IDS
                                .iter()
                                .any(|r| id.eq_ignore_ascii_case(&format!("widget:{r}")))
                        {
                            Vec::new()
                        } else {
                            vec![id.to_owned()]
                        }
                    });
                }
            }
        }
    }

    if mentioned_search {
        map.insert("ShowSearchButton".to_owned(), Value::Bool(enabled_search));
    }
    if mentioned_desktops {
        map.insert(
            "ShowTaskViewButton".to_owned(),
            Value::Bool(enabled_desktops),
        );
    }
    if mentioned_tray {
        map.insert("ShowTrayButton".to_owned(), Value::Bool(enabled_tray));
    }
}

fn migrate_ai_usage(map: &mut Map<String, Value>) {
    let mentioned = [
        "Widgets",
        "EnabledWidgets",
        "InstalledWidgets",
        "WidgetOrder",
        "DockItemOrder",
    ]
    .iter()
    .any(|key| array_mentions(map.get(*key), "AiUsage"))
        || map
            .get("WidgetDensityOverrides")
            .and_then(Value::as_object)
            .is_some_and(|o| o.contains_key("AiUsage"));

    let bits = ai_bits(map);
    let enabled: Vec<String> = ai_providers::WIDGETS
        .iter()
        .filter(|(bit, _)| bits & *bit != 0)
        .map(|(_, id)| (*id).to_owned())
        .collect();
    let all: Vec<String> = ai_providers::WIDGETS
        .iter()
        .map(|(_, id)| (*id).to_owned())
        .collect();

    for key in ["Widgets", "EnabledWidgets"] {
        if let Some(value) = map.get_mut(key) {
            map_string_array(value, |id| ai_replacement(id, &enabled));
        }
    }
    for key in ["InstalledWidgets", "WidgetOrder"] {
        if let Some(value) = map.get_mut(key) {
            map_string_array(value, |id| ai_replacement(id, &all));
        }
    }
    if let Some(value) = map.get_mut("DockItemOrder") {
        map_string_array(value, |id| {
            if id.eq_ignore_ascii_case("widget:AiUsage") {
                all.iter().map(|id| format!("widget:{id}")).collect()
            } else {
                vec![id.to_owned()]
            }
        });
    }
    if let Some(densities) = map
        .get_mut("WidgetDensityOverrides")
        .and_then(Value::as_object_mut)
        && let Some(density) = densities.remove("AiUsage")
    {
        for (_, id) in ai_providers::WIDGETS {
            densities.insert(id.to_owned(), density.clone());
        }
    }
    if let Some(profiles) = map.get_mut("Profiles").and_then(Value::as_array_mut) {
        for profile in profiles.iter_mut() {
            if let Some(p) = profile.as_object_mut() {
                for key in ["Widgets", "EnabledWidgets"] {
                    if let Some(value) = p.get_mut(key) {
                        map_string_array(value, |id| ai_replacement(id, &enabled));
                    }
                }
                for key in ["InstalledWidgets", "WidgetOrder"] {
                    if let Some(value) = p.get_mut(key) {
                        map_string_array(value, |id| ai_replacement(id, &all));
                    }
                }
                if let Some(value) = p.get_mut("DockItemOrder") {
                    map_string_array(value, |id| {
                        if id.eq_ignore_ascii_case("widget:AiUsage") {
                            all.iter().map(|id| format!("widget:{id}")).collect()
                        } else {
                            vec![id.to_owned()]
                        }
                    });
                }
            }
        }
    }
    if mentioned {
        map.insert("AiUsageProviders".to_owned(), Value::from(0));
    }
}

fn migrate_stats_meters(map: &mut Map<String, Value>) {
    let replaced = match map.get("StatsMeters") {
        Some(Value::Number(number)) => number
            .as_u64()
            .filter(|bits| bits & !stats_meters::ALL != 0)
            .map(|bits| Value::from(bits & stats_meters::ALL)),
        Some(Value::String(text)) => {
            let kept: Vec<&str> = text
                .split(',')
                .map(str::trim)
                .filter(|token| {
                    !token.eq_ignore_ascii_case("CpuTemperature")
                        && !token.eq_ignore_ascii_case("Battery")
                })
                .collect();
            let replacement = if kept.is_empty() {
                "None".to_owned()
            } else {
                kept.join(", ")
            };
            (replacement != *text).then(|| Value::String(replacement))
        }
        _ => None,
    };
    if let Some(value) = replaced {
        map.insert("StatsMeters".to_owned(), value);
    }
}

fn migrate_pinned_tray_icons(map: &mut Map<String, Value>) {
    let Some(items) = map.get_mut("PinnedTrayIcons").and_then(Value::as_array_mut) else {
        return;
    };
    let mut result = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();
    for item in items.iter() {
        let key = match item {
            Value::String(text) => Some(text.trim()),
            Value::Object(object) => object.get("Key").and_then(Value::as_str).map(str::trim),
            _ => None,
        };
        let Some(key) = key else { continue };
        if key.is_empty() || key.len() > MAX_PINNED_TRAY_KEY_LEN {
            continue;
        }
        if !seen.insert(key.to_ascii_lowercase()) || result.len() >= MAX_PINNED_TRAY_ICONS {
            continue;
        }
        result.push(Value::String(key.to_owned()));
    }
    *items = result;
}

/// Applies `DockSettingsStore` legacy rewrites to raw JSON before deserialization.
/// Unknown future keys are left untouched.
pub fn normalize_legacy_json(raw: &str) -> String {
    let Ok(Value::Object(mut root)) = serde_json::from_str::<Value>(raw) else {
        return raw.to_owned();
    };

    // Files saved by this port carry both the Rust key and the reference alias for the same value.
    dedupe_written_aliases(&mut root);

    // v0.2.x bottom gap: DockBottomGap -> DockBottomGapPixels.
    if !root.contains_key("DockBottomGapPixels")
        && let Some(gap) = root
            .get("DockBottomGap")
            .and_then(Value::as_i64)
            .filter(|gap| (0..=200).contains(gap))
    {
        root.insert("DockBottomGapPixels".to_owned(), Value::from(gap));
        root.remove("DockBottomGap");
    }

    migrate_widget_lists(&mut root);
    migrate_ai_usage(&mut root);
    sanitize_ai_provider_flags(&mut root);
    migrate_stats_meters(&mut root);

    // Retired SurfaceOpacity fed all four surfaces exactly once.
    if !root.contains_key("DockBackground")
        && let Some(opacity) = root
            .get("SurfaceOpacity")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && (0.6..=1.0).contains(value))
    {
        for key in [
            "DockOpacity",
            "MenuOpacity",
            "WidgetOpacity",
            "SettingsOpacity",
        ] {
            root.insert(key.to_owned(), Value::from(opacity));
        }
    }

    // DockIconPaddingX/Y briefly replaced the single horizontal-only padding.
    if let Some(padding) = root
        .get("DockIconPaddingX")
        .and_then(Value::as_i64)
        .map(|value| value.clamp(0, MAX_ICON_PADDING as i64) as i32)
    {
        root.insert("DockIconPadding".to_owned(), Value::from(padding));
        root.remove("DockIconPaddingX");
    }
    root.remove("DockIconPaddingY");

    migrate_pinned_tray_icons(&mut root);

    serde_json::Value::Object(root).to_string()
}

fn replace_ai_usage_in(list: &mut Vec<String>, replacement: &[String]) {
    if !list.iter().any(|id| id.eq_ignore_ascii_case("AiUsage")) {
        return;
    }
    let mut next = Vec::new();
    for id in list.iter() {
        if id.eq_ignore_ascii_case("AiUsage") {
            next.extend(replacement.iter().cloned());
        } else {
            next.push(id.clone());
        }
    }
    *list = next;
}

/// Canonicalizes saved widget IDs and drops case-insensitive duplicates, matching the reference /// `Distinct(StringComparer.OrdinalIgnoreCase)` on widget lists.
fn normalize_widget_ids(list: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut next = Vec::with_capacity(list.len());
    for id in list.drain(..) {
        let canonical = canonical_widget_setting_id(&id).to_owned();
        if seen.insert(ordinal_ignore_case_key(&canonical)) {
            next.push(canonical);
        }
    }
    *list = next;
}

/// Repairs `enabled ⊆ installed` by deriving installed from enabled, mirroring reference
/// `DockProfileService.MergeInstalledWidgets`. Rejecting the whole settings file for this one
/// legacy inconsistency destroyed every other preference; a union preserves them all.
fn repair_widget_lists(enabled: &mut Vec<String>, installed: &mut Vec<String>) {
    normalize_widget_ids(enabled);
    normalize_widget_ids(installed);
    for id in enabled.iter() {
        if !installed.iter().any(|w| w.eq_ignore_ascii_case(id)) {
            installed.push(id.clone());
        }
    }
}

/// Same as `normalize_widget_ids` for free-form order lists: `widget:clock` and
/// `Widget:Clock` both become `widget:Clock`, with no case-insensitive duplicates.
fn normalize_widget_tokens(list: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut next = Vec::with_capacity(list.len());
    for item in list.drain(..) {
        let canonical = canonicalize_widget_token(&item);
        if seen.insert(ordinal_ignore_case_key(&canonical)) {
            next.push(canonical);
        }
    }
    *list = next;
}

fn canonicalize_density_map(map: &mut BTreeMap<String, WidgetDensityMode>) {
    let previous = std::mem::take(map);
    for (key, value) in previous {
        let canonical = canonical_widget_setting_id(&key).to_owned();
        map.entry(canonical).or_insert(value);
    }
}

fn canonicalize_placements(map: &mut HashMap<String, String>) {
    let mut previous: Vec<_> = std::mem::take(map).into_iter().collect();
    previous.sort_by(|a, b| a.0.cmp(&b.0));
    for (key, value) in previous {
        let canonical = canonical_widget_setting_id(&key).to_owned();
        map.entry(canonical).or_insert(value);
    }
}

impl DockSettings {
    /// Normalizes every widget-keyed collection to the IDs reference validates, so a
    /// settings.json written by either build uses the same spellings. Called on
    /// load (repair) and on save (before serialization).
    pub fn canonicalize_widget_keys(&mut self) {
        repair_widget_lists(&mut self.enabled_widgets, &mut self.installed_widgets);
        normalize_widget_ids(&mut self.widget_order);
        normalize_widget_tokens(&mut self.dock_item_order);
        canonicalize_density_map(&mut self.widget_density_overrides);
        canonicalize_placements(&mut self.dynamic_island_placements);
        for profile in &mut self.profiles {
            repair_widget_lists(&mut profile.enabled_widgets, &mut profile.installed_widgets);
            normalize_widget_ids(&mut profile.widget_order);
            normalize_widget_tokens(&mut profile.dock_item_order);
        }
    }

    /// Re-applies the legacy migrations to already-deserialized settings (no raw JSON needed).
    pub fn migrate_legacy(&mut self) {
        let mentioned_search = self
            .enabled_widgets
            .iter()
            .chain(self.installed_widgets.iter())
            .chain(self.widget_order.iter())
            .chain(self.dock_item_order.iter())
            .any(|id| id.eq_ignore_ascii_case("Search"));
        let mentioned_desktops = self
            .enabled_widgets
            .iter()
            .chain(self.installed_widgets.iter())
            .chain(self.widget_order.iter())
            .chain(self.dock_item_order.iter())
            .any(|id| id.eq_ignore_ascii_case("Desktops"));
        let mentioned_tray = self
            .enabled_widgets
            .iter()
            .chain(self.installed_widgets.iter())
            .chain(self.widget_order.iter())
            .chain(self.dock_item_order.iter())
            .any(|id| id.eq_ignore_ascii_case("Tray"));
        let enabled_search = self
            .enabled_widgets
            .iter()
            .any(|id| id.eq_ignore_ascii_case("Search"));
        let enabled_desktops = self
            .enabled_widgets
            .iter()
            .any(|id| id.eq_ignore_ascii_case("Desktops"));
        let enabled_tray = self
            .enabled_widgets
            .iter()
            .any(|id| id.eq_ignore_ascii_case("Tray"));

        self.enabled_widgets
            .retain(|id| !is_retired_list_widget(id));
        self.installed_widgets
            .retain(|id| !is_retired_list_widget(id));
        self.widget_order.retain(|id| !is_retired_list_widget(id));
        self.dock_item_order.retain(|id| {
            !is_retired_ai_provider_widget(id)
                && !RETIRED_WIDGET_IDS
                    .iter()
                    .any(|retired| id.eq_ignore_ascii_case(&format!("widget:{retired}")))
        });
        self.widget_density_overrides
            .retain(|id, _| !is_retired_list_widget(id));
        for profile in &mut self.profiles {
            profile
                .enabled_widgets
                .retain(|id| !is_retired_list_widget(id));
            profile
                .installed_widgets
                .retain(|id| !is_retired_list_widget(id));
            profile
                .widget_order
                .retain(|id| !is_retired_list_widget(id));
            profile.dock_item_order.retain(|id| {
                !is_retired_ai_provider_widget(id)
                    && !RETIRED_WIDGET_IDS
                        .iter()
                        .any(|retired| id.eq_ignore_ascii_case(&format!("widget:{retired}")))
            });
        }
        // A retired provider flag must not survive into `validate()`.
        self.ai_usage_providers &= !ai_providers::RETIRED_BIT;
        if mentioned_search {
            self.show_search_button = enabled_search;
        }
        if mentioned_desktops {
            self.show_task_view_button = enabled_desktops;
        }
        if mentioned_tray {
            self.show_tray_button = enabled_tray;
        }

        // AiUsage -> one widget per provider.
        let has_ai_usage = self
            .enabled_widgets
            .iter()
            .chain(self.installed_widgets.iter())
            .chain(self.widget_order.iter())
            .chain(self.dock_item_order.iter())
            .any(|id| {
                id.eq_ignore_ascii_case("AiUsage") || id.eq_ignore_ascii_case("widget:AiUsage")
            })
            || self.widget_density_overrides.contains_key("AiUsage");
        if has_ai_usage {
            let bits = self.ai_usage_providers;
            let enabled: Vec<String> = ai_providers::WIDGETS
                .iter()
                .filter(|(bit, _)| bits & *bit != 0)
                .map(|(_, id)| (*id).to_owned())
                .collect();
            let all_ids: Vec<String> = ai_providers::WIDGETS
                .iter()
                .map(|(_, id)| (*id).to_owned())
                .collect();
            replace_ai_usage_in(&mut self.enabled_widgets, &enabled);
            replace_ai_usage_in(&mut self.installed_widgets, &all_ids);
            replace_ai_usage_in(&mut self.widget_order, &all_ids);
            let mut next = Vec::new();
            for id in &self.dock_item_order {
                if id.eq_ignore_ascii_case("widget:AiUsage") {
                    next.extend(all_ids.iter().map(|id| format!("widget:{id}")));
                } else {
                    next.push(id.clone());
                }
            }
            self.dock_item_order = next;
            if let Some(density) = self.widget_density_overrides.remove("AiUsage") {
                for (_, id) in ai_providers::WIDGETS {
                    self.widget_density_overrides.insert(id.to_owned(), density);
                }
            }
            for profile in &mut self.profiles {
                replace_ai_usage_in(&mut profile.enabled_widgets, &enabled);
                replace_ai_usage_in(&mut profile.installed_widgets, &all_ids);
                replace_ai_usage_in(&mut profile.widget_order, &all_ids);
                let mut next = Vec::new();
                for id in &profile.dock_item_order {
                    if id.eq_ignore_ascii_case("widget:AiUsage") {
                        next.extend(all_ids.iter().map(|id| format!("widget:{id}")));
                    } else {
                        next.push(id.clone());
                    }
                }
                profile.dock_item_order = next;
            }
            self.ai_usage_providers = 0;
        }

        // reference treats an absent InstalledWidgets list as "exactly the enabled widgets" and merges
        // enabled IDs into it when applying a profile. Older port builds wrote only the enabled
        // list (and snake_case IDs), leaving files that `Validate()` rejected wholesale. Repair
        // the invariant here instead of resetting every setting to defaults.
        self.canonicalize_widget_keys();

        // Tray pins: trim, drop invalid/duplicate keys, cap at the reference limit.
        let mut seen = BTreeSet::new();
        self.user_pinned_background_apps.retain(|key| {
            let trimmed = key.trim();
            !trimmed.is_empty()
                && trimmed.len() <= MAX_PINNED_TRAY_KEY_LEN
                && seen.insert(trimmed.to_ascii_lowercase())
        });
        self.user_pinned_background_apps
            .truncate(MAX_PINNED_TRAY_ICONS);
    }
}

// ---- reference DockSettingsStore.IsValid() parity ----

fn is_finite(value: f64) -> bool {
    value.is_finite()
}

fn clock_date_style_name(style: ClockDateStyle) -> &'static str {
    match style {
        ClockDateStyle::DayAndShortDate => "DayAndShortDate",
        ClockDateStyle::ShortDateOnly => "ShortDateOnly",
        ClockDateStyle::NumericDate => "NumericDate",
        ClockDateStyle::Hidden => "Hidden",
    }
}

fn clock_date_format_name(format: ClockDateFormat) -> &'static str {
    match format {
        ClockDateFormat::System => "System",
        ClockDateFormat::DdMmYyyy => "DdMmYyyy",
        ClockDateFormat::DdMmYy => "DdMmYy",
        ClockDateFormat::MmDdYyyy => "MmDdYyyy",
        ClockDateFormat::YyyyMmDd => "YyyyMmDd",
        ClockDateFormat::DdMmYyyyDots => "DdMmYyyyDots",
    }
}

fn island_visibility_name(mode: DynamicIslandVisibilityMode) -> &'static str {
    match mode {
        DynamicIslandVisibilityMode::Always => "Always",
        DynamicIslandVisibilityMode::DesktopOnly => "DesktopOnly",
    }
}

fn is_safe_theme_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && id.len() <= 120
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !id.contains("..")
}

fn is_border_valid(thickness: f64, color: Option<&str>) -> bool {
    is_finite(thickness)
        && (0.0..=12.0).contains(&thickness)
        && color.is_none_or(|value| {
            value.trim().is_empty()
                || (value.starts_with('#')
                    && value.len() == 9
                    && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit()))
        })
}

/// Key used by `StringComparer.OrdinalIgnoreCase` (invariant simple uppercase, no culture).
fn ordinal_ignore_case_key(value: &str) -> String {
    let mut key = String::with_capacity(value.len());
    for c in value.chars() {
        key.extend(c.to_uppercase());
    }
    key
}

fn has_duplicates_ignore_case(values: &[String]) -> bool {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .any(|value| !seen.insert(ordinal_ignore_case_key(value)))
}

fn has_duplicate_keys_ignore_case<'a>(mut keys: impl Iterator<Item = &'a String>) -> bool {
    let mut seen = BTreeSet::new();
    keys.any(|key| !seen.insert(ordinal_ignore_case_key(key)))
}

/// Key used by `StringComparer.CurrentCultureIgnoreCase`. The full culture table is not
/// available in `core`, so the persisted UI language selects the one casing rule that changes
/// equality decisions: Turkish (`i`/`İ` and `I`/`ı` pairs). Everything else uses invariant
/// uppercase, matching the ordinal comparer reference uses for the remaining settings.
fn culture_ignore_case_key(value: &str, turkish: bool) -> String {
    let mut key = String::with_capacity(value.len());
    for c in value.chars() {
        if turkish {
            match c {
                'i' => key.push('\u{0130}'), // Turkish: i -> İ
                'ı' => key.push('I'),        // Turkish: ı -> I
                _ => key.extend(c.to_uppercase()),
            }
        } else {
            key.extend(c.to_uppercase());
        }
    }
    key
}

fn contains_blank(values: &[String]) -> bool {
    values.iter().any(|value| value.trim().is_empty())
}

impl DockSettings {
    fn extra_f64(&self, key: &str, default: f64) -> f64 {
        self.extra
            .get(key)
            .and_then(Value::as_f64)
            .unwrap_or(default)
    }

    fn extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(Value::as_str)
    }

    fn stats_meter_bits(&self) -> u64 {
        match self.extra.get("StatsMeters") {
            Some(Value::Number(number)) => number.as_u64().unwrap_or(0),
            Some(Value::String(text)) => text.split(',').fold(0u64, |bits, token| {
                bits | match token.trim() {
                    "Cpu" => stats_meters::CPU,
                    "Memory" => stats_meters::MEMORY,
                    "Gpu" => stats_meters::GPU,
                    "GpuTemperature" => stats_meters::GPU_TEMPERATURE,
                    _ => 0,
                }
            }),
            None => self.stats_meters as u64,
            _ => 0,
        }
    }

    /// Returns one stable code per violated reference validation rule; empty means valid.
    pub fn validate(&self) -> Vec<&'static str> {
        let mut errors: Vec<&'static str> = Vec::new();
        let turkish = self.language.trim().to_ascii_lowercase().starts_with("tr");

        // Width / height / spacing ranges (reference sliders).
        if !(100..=5000).contains(&self.dock_width_pixels) {
            errors.push("DockWidthPixelsOutOfRange");
        }
        if !(10..=100).contains(&self.dock_width_percent) {
            errors.push("DockWidthPercentOutOfRange");
        }
        if !(24..=400).contains(&self.dock_height_pixels) {
            errors.push("DockHeightPixelsOutOfRange");
        }
        if !(2..=25).contains(&self.dock_height_percent) {
            errors.push("DockHeightPercentOutOfRange");
        }
        if !(0..=200).contains(&self.dock_top_gap_pixels) {
            errors.push("DockTopGapPixelsOutOfRange");
        }
        if !(0..=100).contains(&self.dock_top_gap_percent) {
            errors.push("DockTopGapPercentOutOfRange");
        }
        if !(0..=200).contains(&self.dock_bottom_gap_pixels) {
            errors.push("DockBottomGapPixelsOutOfRange");
        }
        if !(0..=100).contains(&self.dock_bottom_gap_percent) {
            errors.push("DockBottomGapPercentOutOfRange");
        }

        // Focus timer ranges.
        if !(1..=90).contains(&self.focus_minutes) {
            errors.push("FocusMinutesOutOfRange");
        }
        if !(1..=30).contains(&self.focus_break_minutes) {
            errors.push("FocusBreakMinutesOutOfRange");
        }
        if !(1..=8).contains(&self.focus_sessions) {
            errors.push("FocusSessionsOutOfRange");
        }

        // Surface opacity/border rules. The WIP port has typed fields; reference JSON values that were
        // not claimed by a typed field remain in `extra` and are checked there as well.
        let surface_opacity = if self.extra.contains_key("SurfaceOpacity") {
            self.extra_f64("SurfaceOpacity", 0.9)
        } else {
            self.surface_opacity
        };
        if !is_finite(surface_opacity) || !(0.6..=1.0).contains(&surface_opacity) {
            errors.push("SurfaceOpacityOutOfRange");
        }
        for (key, value) in [
            ("DockOpacity", self.dock_opacity),
            ("MenuOpacity", self.menu_opacity),
            ("WidgetOpacity", self.widget_opacity),
            ("SettingsOpacity", self.settings_opacity),
        ] {
            if !is_finite(value) || !(0.0..=1.0).contains(&value) {
                match key {
                    "DockOpacity" => errors.push("DockOpacityOutOfRange"),
                    "MenuOpacity" => errors.push("MenuOpacityOutOfRange"),
                    "WidgetOpacity" => errors.push("WidgetOpacityOutOfRange"),
                    _ => errors.push("SettingsOpacityOutOfRange"),
                }
            }
        }
        for (thickness, color) in [
            (
                self.dock_border_thickness,
                self.dock_border_color.as_deref(),
            ),
            (
                self.menu_border_thickness,
                self.menu_border_color.as_deref(),
            ),
            (
                self.widget_border_thickness,
                self.widget_border_color.as_deref(),
            ),
            (
                self.settings_border_thickness,
                self.settings_border_color.as_deref(),
            ),
        ] {
            if !is_border_valid(thickness, color) {
                errors.push("BorderStyleInvalid");
                break;
            }
        }
        if !is_finite(self.dock_scale) || !(0.75..=1.5).contains(&self.dock_scale) {
            errors.push("DockScaleOutOfRange");
        }

        // Pin order.
        if let Some(order) = &self.pin_order
            && (contains_blank(order) || has_duplicates_ignore_case(order))
        {
            errors.push("PinOrderInvalid");
        }

        // Widget subset invariant: enabled Γèå installed, both from the reference supported catalog.
        if self
            .enabled_widgets
            .iter()
            .any(|id| !is_supported_widget_id(id))
            || self
                .installed_widgets
                .iter()
                .any(|id| !is_supported_widget_id(id))
        {
            errors.push("UnsupportedWidgetId");
        }
        if has_duplicates_ignore_case(&self.enabled_widgets)
            || has_duplicates_ignore_case(&self.installed_widgets)
        {
            errors.push("DuplicateWidgetId");
        }
        if self.enabled_widgets.iter().any(|id| {
            !self
                .installed_widgets
                .iter()
                .any(|installed| installed.eq_ignore_ascii_case(id))
        }) {
            errors.push("EnabledWidgetNotInstalled");
        }
        if contains_blank(&self.widget_order) || has_duplicates_ignore_case(&self.widget_order) {
            errors.push("WidgetOrderInvalid");
        }
        if contains_blank(&self.stack_folders) {
            errors.push("StackFoldersInvalid");
        }

        // Hotkeys: presets/custom strings and the launcher-vs-dock conflict.
        if !HotkeyBinding::is_valid_custom_string(self.custom_launcher_hotkey.as_deref()) {
            errors.push("CustomLauncherHotkeyInvalid");
        }
        if !HotkeyBinding::is_valid_custom_string(self.custom_dock_hotkey.as_deref()) {
            errors.push("CustomDockHotkeyInvalid");
        }
        let launcher =
            HotkeyBinding::resolve(self.launcher_hotkey, self.custom_launcher_hotkey.as_deref());
        let dock = HotkeyBinding::resolve(self.dock_hotkey, self.custom_dock_hotkey.as_deref());
        if HotkeyBinding::equals_binding(launcher.as_ref(), dock.as_ref()) {
            errors.push("HotkeysConflict");
        }

        // Profiles: unique names (CurrentCultureIgnoreCase), valid widgets/ranges/hotkeys.
        let mut profile_names = BTreeSet::new();
        for profile in &self.profiles {
            if profile.name.trim().is_empty()
                || !profile_names.insert(culture_ignore_case_key(&profile.name, turkish))
            {
                errors.push("ProfileInvalid");
                break;
            }
            if profile
                .enabled_widgets
                .iter()
                .any(|id| !is_supported_widget_id(id))
                || profile
                    .installed_widgets
                    .iter()
                    .any(|id| !is_supported_widget_id(id))
            {
                errors.push("ProfileWidgetUnsupported");
                break;
            }
            if has_duplicates_ignore_case(&profile.enabled_widgets)
                || has_duplicates_ignore_case(&profile.installed_widgets)
                || contains_blank(&profile.widget_order)
                || has_duplicates_ignore_case(&profile.widget_order)
                || profile.enabled_widgets.iter().any(|id| {
                    !profile
                        .installed_widgets
                        .iter()
                        .any(|installed| installed.eq_ignore_ascii_case(id))
                })
            {
                errors.push("ProfileWidgetInvalid");
                break;
            }
            if profile
                .dock_width_pixels
                .is_some_and(|v| !(100..=5000).contains(&v))
                || profile
                    .dock_width_percent
                    .is_some_and(|v| !(10..=100).contains(&v))
            {
                errors.push("ProfileWidthOutOfRange");
                break;
            }
            if !HotkeyBinding::is_valid_custom_string(profile.custom_launcher_hotkey.as_deref())
                || !HotkeyBinding::is_valid_custom_string(profile.custom_dock_hotkey.as_deref())
            {
                errors.push("ProfileHotkeyInvalid");
                break;
            }
            let profile_launcher = HotkeyBinding::resolve(
                profile.launcher_hotkey.unwrap_or(HotkeyChoice::Disabled),
                profile.custom_launcher_hotkey.as_deref(),
            );
            let profile_dock = HotkeyBinding::resolve(
                profile.dock_hotkey.unwrap_or(HotkeyChoice::Disabled),
                profile.custom_dock_hotkey.as_deref(),
            );
            if HotkeyBinding::equals_binding(profile_launcher.as_ref(), profile_dock.as_ref()) {
                errors.push("ProfileHotkeyConflict");
                break;
            }
        }

        // Groups: unique ids (ordinal ignore case), names (culture ignore case) and paths.
        let mut group_ids = BTreeSet::new();
        let mut group_names = BTreeSet::new();
        let mut group_paths = BTreeSet::new();
        for group in &self.app_groups {
            if group.id.trim().is_empty()
                || group.name.trim().is_empty()
                || !group_ids.insert(ordinal_ignore_case_key(&group.id))
                || !group_names.insert(culture_ignore_case_key(&group.name, turkish))
                || group
                    .executable_paths
                    .iter()
                    .any(|path| !group_paths.insert(ordinal_ignore_case_key(path)))
            {
                errors.push("AppGroupsInvalid");
                break;
            }
        }

        // Flags, enum-like extras and misc lengths.
        if self.stats_meter_bits() & !stats_meters::ALL != 0 {
            errors.push("StatsMetersInvalid");
        }
        if self.ai_usage_providers & !ai_providers::ALL != 0 {
            errors.push("AiUsageProvidersInvalid");
        }
        if self
            .dock_corner_radius
            .is_some_and(|radius| !(0..=48).contains(&radius))
        {
            errors.push("DockCornerRadiusOutOfRange");
        }
        if self.weather_city.len() > 80 {
            errors.push("WeatherCityTooLong");
        }
        if !(15..=300).contains(&self.wireless_battery_poll_seconds) {
            errors.push("WirelessPollOutOfRange");
        }
        if ![0, 10, 20, 30].contains(&self.wireless_battery_low_threshold) {
            errors.push("WirelessThresholdInvalid");
        }
        // reference DockSettingsStore.IsValid(): display-care values must stay inside the
        // exact slider/dropdown ranges, otherwise the whole file falls back to defaults.
        if !(5..=120).contains(&self.burn_in_shift_minutes) {
            errors.push("BurnInShiftMinutesOutOfRange");
        }
        if ![0, 10, 20, 30, 50].contains(&self.oled_low_battery_threshold) {
            errors.push("OledLowBatteryThresholdInvalid");
        }
        if ![0, 30, 60, 120].contains(&self.animation_fps_cap) {
            errors.push("AnimationFpsCapInvalid");
        }
        if self.ignored_wireless_devices.len() > 128
            || contains_blank(&self.ignored_wireless_devices)
            || self
                .ignored_wireless_devices
                .iter()
                .any(|id| id.len() > 256)
        {
            errors.push("IgnoredWirelessDevicesInvalid");
        }
        if self.wireless_device_aliases.len() > 128
            || self.wireless_device_aliases.iter().any(|(key, value)| {
                key.trim().is_empty()
                    || key.len() > 256
                    || value.trim().is_empty()
                    || value.len() > 120
            })
        {
            errors.push("WirelessDeviceAliasesInvalid");
        }
        let theme_pack = if self.theme_pack.trim().is_empty() {
            self.extra_str("ThemePack").unwrap_or("")
        } else {
            self.theme_pack.trim()
        };
        if !is_safe_theme_id(theme_pack) && !theme_pack.trim().is_empty() {
            errors.push("ThemePackInvalid");
        }
        if !is_finite(surface_opacity) {
            errors.push("SurfaceOpacityInvalid");
        }
        let island_visibility = self
            .extra_str("DynamicIslandVisibility")
            .unwrap_or_else(|| island_visibility_name(self.dynamic_island_visibility));
        if !island_visibility.eq_ignore_ascii_case("Always")
            && !island_visibility.eq_ignore_ascii_case("DesktopOnly")
        {
            errors.push("IslandVisibilityInvalid");
        }
        let date_style = self
            .extra_str("DateStyle")
            .unwrap_or_else(|| clock_date_style_name(self.date_style));
        if !["DayAndShortDate", "ShortDateOnly", "NumericDate", "Hidden"]
            .iter()
            .any(|valid| date_style.eq_ignore_ascii_case(valid))
        {
            errors.push("DateStyleInvalid");
        }
        let date_format = self
            .extra_str("DateFormat")
            .unwrap_or_else(|| clock_date_format_name(self.date_format));
        if ![
            "System",
            "DdMmYyyy",
            "DdMmYy",
            "MmDdYyyy",
            "YyyyMmDd",
            "DdMmYyyyDots",
        ]
        .iter()
        .any(|valid| date_format.eq_ignore_ascii_case(valid))
        {
            errors.push("DateFormatInvalid");
        }

        // Dynamic Island order/placement maps (keys must be catalog widgets, unique under the reference // OrdinalIgnoreCase comparer — `"Clock"` and `"clock"` are the same placement).
        let typed_placements_invalid = self.dynamic_island_placements.len() > CATALOG_WIDGET_COUNT
            || has_duplicate_keys_ignore_case(self.dynamic_island_placements.keys())
            || self.dynamic_island_placements.iter().any(|(id, display)| {
                !is_supported_widget_id(id) || display.trim().is_empty() || display.len() > 32
            });
        let extra_placements_invalid = match self.extra.get("DynamicIslandPlacements") {
            Some(Value::Object(placements)) => {
                placements.len() > CATALOG_WIDGET_COUNT
                    || has_duplicate_keys_ignore_case(placements.keys())
                    || placements.iter().any(|(id, display)| {
                        !is_supported_widget_id(id)
                            || !display
                                .as_str()
                                .is_some_and(|value| !value.trim().is_empty() && value.len() <= 32)
                    })
            }
            _ => false,
        };
        if typed_placements_invalid || extra_placements_invalid {
            errors.push("IslandPlacementInvalid");
        }

        let typed_orders_invalid = self.dynamic_island_widget_orders.len() > 32
            || self.dynamic_island_widget_orders.iter().any(|(key, ids)| {
                key.trim().is_empty()
                    || key.len() > 32
                    || ids.len() > CATALOG_WIDGET_COUNT
                    || ids.iter().any(|id| !is_supported_widget_id(id))
                    || has_duplicates_ignore_case(ids)
            });
        let extra_orders_invalid = match self.extra.get("DynamicIslandWidgetOrders") {
            Some(Value::Object(orders)) => {
                orders.len() > 32
                    || orders.iter().any(|(key, value)| {
                        key.trim().is_empty()
                            || key.len() > 32
                            || !value.as_array().is_some_and(|ids| {
                                ids.len() <= CATALOG_WIDGET_COUNT
                                    && ids
                                        .iter()
                                        .all(|id| id.as_str().is_some_and(is_supported_widget_id))
                                    && !has_duplicates_ignore_case(
                                        &ids.iter()
                                            .filter_map(Value::as_str)
                                            .map(str::to_owned)
                                            .collect::<Vec<_>>(),
                                    )
                            })
                    })
            }
            _ => false,
        };
        if typed_orders_invalid || extra_orders_invalid {
            errors.push("IslandOrderInvalid");
        }
        if self.user_pinned_background_apps.len() > MAX_PINNED_TRAY_ICONS
            || contains_blank(&self.user_pinned_background_apps)
            || self
                .user_pinned_background_apps
                .iter()
                .any(|key| key.len() > MAX_PINNED_TRAY_KEY_LEN)
        {
            errors.push("PinnedTrayIconsInvalid");
        }

        errors
    }
}

/// Application data directory: `%LOCALAPPDATA%\OpenTaskbar` (shared with reference).
/// Kept here as a facade because every Rust consumer already calls this name.
pub fn app_data_dir() -> PathBuf {
    crate::paths::shared_app_data_dir()
}

pub fn settings_path() -> PathBuf {
    crate::paths::settings_path()
}

/// `FullBackupArchive.CurrentVersion`. Any other manifest version is unsupported.
pub const BACKUP_MANIFEST_VERSION: u64 = 1;

/// Backs up an unusable settings file without replacing the original, like reference
/// `DockSettingsStore.BackupInvalidSettingsFile`.
fn backup_invalid_settings(path: &Path) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let backup = path.with_file_name(format!("settings.json.invalid-{stamp}"));
    // Keep the original in place: one malformed field must not destroy the only copy.
    let _ = fs::copy(path, backup);
}

/// Mirrors the `FullBackupArchive` manifest gate: `inspect_backup` must reject archives whose
/// `manifest.json` carries a `Version` other than 1 (reference rejects the v99 probe archive).
/// `None` means the entry is missing. reference deserializes the manifest case-insensitively.
pub fn backup_manifest_version_supported(manifest: Option<&[u8]>) -> bool {
    let Some(raw) = manifest else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(raw) else {
        return false;
    };
    value
        .get("Version")
        .or_else(|| value.get("version"))
        .and_then(Value::as_u64)
        == Some(BACKUP_MANIFEST_VERSION)
}

/// Loads settings from `path`. Corrupt JSON and settings that fail the `IsValid()` rules both
/// fall back to defaults and leave a timestamped backup of the rejected file behind.
pub fn load_from(path: &Path) -> DockSettings {
    let Ok(raw) = fs::read_to_string(path) else {
        return DockSettings::default();
    };
    let clean = raw.trim_start_matches('\u{feff}');
    let normalized = normalize_legacy_json(clean);
    match serde_json::from_str::<DockSettings>(&normalized) {
        Ok(mut settings) => {
            settings.migrate_legacy();
            settings.dock_opacity = settings.dock_opacity.clamp(0.0, 1.0);
            settings.dock_scale = settings.dock_scale.clamp(0.75, 1.5);
            settings.dynamic_island_opacity = settings.dynamic_island_opacity.clamp(0.1, 1.0);
            settings.surface_opacity = settings.surface_opacity.clamp(0.1, 1.0);
            settings.glass_blur_sigma = settings.glass_blur_sigma.clamp(0.0, 36.0);
            settings.glass_saturation = settings.glass_saturation.clamp(0.8, 1.8);
            settings.glass_edge_glow = settings.glass_edge_glow.clamp(0.0, 1.0);
            settings.ui_font_size = settings.ui_font_size.clamp(9.0, 24.0);
            let errors = settings.validate();
            if errors.is_empty() {
                settings
            } else {
                crate::log::warn(format!(
                    "settings: invalid settings rejected ({errors:?}); using defaults from {}",
                    path.display()
                ));
                crate::log::report_error("ErrorDockSettingsInvalid", &[]);
                backup_invalid_settings(path);
                DockSettings::default()
            }
        }
        Err(error) => {
            crate::log::warn(format!(
                "settings: unreadable settings ({error}); using defaults from {}",
                path.display()
            ));
            crate::log::report_error("ErrorDockSettingsRead", &[]);
            backup_invalid_settings(path);
            DockSettings::default()
        }
    }
}

/// Writes the six reference-facing alias keys next to their Rust canonical keys so a settings.json
/// produced by the port loses nothing when the reference build reads it. Profiles use `Widgets` as their
/// only alias in `DockProfile`.
fn write_settings_aliases(root: &mut Map<String, Value>, settings: &DockSettings) {
    root.insert("EnabledWidgets".to_owned(), json!(settings.enabled_widgets));
    root.insert("Widgets".to_owned(), json!(settings.enabled_widgets));
    root.insert(
        "ShowClockWidget".to_owned(),
        json!(settings.show_clock_widget),
    );
    root.insert("ShowClock".to_owned(), json!(settings.show_clock_widget));
    root.insert(
        "ShowSystemStatsWidget".to_owned(),
        json!(settings.show_system_stats_widget),
    );
    root.insert(
        "ShowSystemStats".to_owned(),
        json!(settings.show_system_stats_widget),
    );
    root.insert("FullWidth".to_owned(), json!(settings.full_width));
    root.insert(
        "Layout".to_owned(),
        json!(if settings.full_width {
            "FullWidth"
        } else {
            "Floating"
        }),
    );
    root.insert("FocusMode".to_owned(), json!(settings.focus_mode));
    root.insert("FocusModeEnabled".to_owned(), json!(settings.focus_mode));
    root.insert(
        "UserPinnedBackgroundApps".to_owned(),
        json!(settings.user_pinned_background_apps),
    );
    root.insert(
        "PinnedTrayIcons".to_owned(),
        json!(settings.user_pinned_background_apps),
    );
    root.insert(
        "DynamicIslandEnabled".to_owned(),
        json!(settings.dynamic_island_enable),
    );

    if let Some(Value::Array(profiles)) = root.get_mut("Profiles") {
        for (entry, profile) in profiles.iter_mut().zip(settings.profiles.iter()) {
            if let Value::Object(object) = entry {
                object.insert("EnabledWidgets".to_owned(), json!(profile.enabled_widgets));
                object.insert("Widgets".to_owned(), json!(profile.enabled_widgets));
            }
        }
    }
}

static TMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Unique temporary file name for one write. The reference build uses the same
/// `<file>.<guid>.tmp` scheme; a fixed `settings.json.tmp` would let two
/// processes race on the same file and corrupt the shared settings.json.
fn unique_settings_tmp(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let salt = &nanos as *const u64 as usize as u64;
    let sequence = TMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&nanos.to_le_bytes());
    bytes[8..12].copy_from_slice(&(std::process::id() as u32).to_le_bytes());
    bytes[12..16].copy_from_slice(&(sequence as u32 ^ salt as u32).to_le_bytes());
    // RFC 4122 version 4 / variant 10 layout; uniqueness matters more than CSPRNG quality here.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let guid = format!(
        "{}-{}-{}-{}-{}",
        hex[0..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..16].concat()
    );
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".to_owned());
    path.with_file_name(format!("{name}.{guid}.tmp"))
}

fn write_settings_to(path: &Path, settings: &DockSettings) -> std::io::Result<()> {
    let errors = settings.validate();
    if !errors.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid settings rejected ({errors:?})"),
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut canonical = settings.clone();
    canonical.canonicalize_widget_keys();
    let mut value = serde_json::to_value(&canonical)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Value::Object(root) = &mut value {
        write_settings_aliases(root, &canonical);
    }
    let json = serde_json::to_string_pretty(&value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let tmp = unique_settings_tmp(path);
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Atomically writes settings to `path`, including the reference alias spellings of aliased keys.
/// Failures are logged instead of being silently swallowed by callers using `let _ = save_to(...)`.
pub fn save_to(path: &Path, settings: &DockSettings) -> std::io::Result<()> {
    write_settings_to(path, settings).inspect_err(|error| {
        crate::log::error(format!(
            "settings: failed to save {}: {error}",
            path.display()
        ));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ot-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip_preserves_unknown_fields() {
        let dir = temp_dir("cfg-unknown");
        let path = dir.join("settings.json");
        let raw = r#"{
            "Position": "Left",
            "Size": "Large",
            "Theme": "Dark",
            "AutoHide": "Smart",
            "DockOpacity": 0.75,
            "CustomReferenceField": "keep-me"
        }"#;
        fs::write(&path, raw).unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.position, DockPosition::Left);
        assert_eq!(loaded.size, DockSize::Large);
        assert_eq!(loaded.theme, AppTheme::Dark);
        assert_eq!(loaded.auto_hide, AutoHideMode::Smart);
        assert_eq!(
            loaded
                .extra
                .get("CustomReferenceField")
                .and_then(Value::as_str),
            Some("keep-me")
        );

        save_to(&path, &loaded).unwrap();
        let reloaded = load_from(&path);
        assert_eq!(
            reloaded
                .extra
                .get("CustomReferenceField")
                .and_then(Value::as_str),
            Some("keep-me")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_json_returns_defaults_and_creates_backup() {
        let dir = temp_dir("cfg-corrupt");
        let path = dir.join("settings.json");
        fs::write(&path, "{not valid json").unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.position, DockPosition::Center);
        // WIP behavior: keep the original file and write a timestamped backup alongside it.
        assert!(path.exists());
        let has_backup = fs::read_dir(&dir).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("settings.json.invalid-")
        });
        assert!(has_backup);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn widgets_are_opt_in_by_default_and_respect_focus_mode() {
        let mut s = DockSettings::default();
        assert!(s.enabled_widgets.is_empty());
        assert!(!s.is_widget_enabled("clock"));
        assert!(!s.is_widget_enabled("stats"));
        assert!(!s.is_widget_enabled("audio"));

        s.set_widget_enabled("clock", true);
        s.set_widget_enabled("audio", true);
        s.set_widget_enabled("focus", true);
        assert!(s.is_widget_enabled("clock"));
        assert!(s.is_widget_enabled("audio"));
        assert!(s.is_widget_enabled("focus"));

        s.focus_mode = true;
        assert!(s.is_widget_enabled("clock"));
        assert!(s.is_widget_enabled("focus"));
        assert!(!s.is_widget_enabled("audio"));

        s.focus_mode = false;
        s.set_widget_enabled("clock", false);
        assert!(!s.is_widget_enabled("clock"));
        assert!(!s.show_clock_widget);
    }

    #[test]
    fn defaults_match_reference() {
        let s = DockSettings::default();
        assert_eq!(s.dock_opacity, 0.9);
        assert_eq!(s.dock_bottom_gap_pixels, 6);
        assert_eq!(s.weather_city, "");
        assert_eq!(s.menu_opacity, 0.9);
        assert_eq!(s.focus_minutes, 25);
        assert_eq!(s.dock_width_pixels, 900);
        assert_eq!(s.dock_height_percent, 7);
        assert!(s.magnification_expands_dock);
        assert!(!s.oled_reduce_glow_enabled);
        assert!(!s.burn_in_shift_enabled);
        assert_eq!(s.burn_in_shift_minutes, 30);
        assert!(!s.oled_auto_trigger_enabled);
        assert_eq!(s.oled_low_battery_threshold, 0);
        assert!(!s.oled_auto_on_night_light);
        assert!(!s.ambient_mode_enabled);
        assert_eq!(s.animation_fps_cap, 0);
        assert!(s.validate().is_empty(), "{:?}", s.validate());
    }

    #[test]
    fn oled_settings_round_trip_with_reference_names() {
        let raw = json!({
            "DockBackground": "TrueBlack",
            "OledReduceGlowEnabled": true,
            "BurnInShiftEnabled": true,
            "BurnInShiftMinutes": 120,
            "OledAutoTriggerEnabled": true,
            "OledLowBatteryThreshold": 50,
            "OledAutoOnNightLight": true,
            "AmbientModeEnabled": true,
            "AnimationFpsCap": 120,
            "UnknownFutureKey": 7
        });
        let settings: DockSettings = serde_json::from_value(raw).unwrap();
        assert_eq!(settings.dock_background, SurfaceBackground::TrueBlack);
        assert!(settings.oled_reduce_glow_enabled);
        assert!(settings.burn_in_shift_enabled);
        assert_eq!(settings.burn_in_shift_minutes, 120);
        assert!(settings.oled_auto_trigger_enabled);
        assert_eq!(settings.oled_low_battery_threshold, 50);
        assert!(settings.oled_auto_on_night_light);
        assert!(settings.ambient_mode_enabled);
        assert_eq!(settings.animation_fps_cap, 120);
        let back = serde_json::to_value(&settings).unwrap();
        for key in [
            "DockBackground",
            "OledReduceGlowEnabled",
            "BurnInShiftEnabled",
            "BurnInShiftMinutes",
            "OledAutoTriggerEnabled",
            "OledLowBatteryThreshold",
            "OledAutoOnNightLight",
            "AmbientModeEnabled",
            "AnimationFpsCap",
            "UnknownFutureKey",
        ] {
            assert!(back.get(key).is_some(), "missing serialized key {key}");
        }
        assert!(settings.validate().is_empty(), "{:?}", settings.validate());
    }

    #[test]
    fn reference_aliases_are_read() {
        let dir = temp_dir("cfg-aliases");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{
                "Widgets": ["Clock", "Media"],
                "ShowClock": true,
                "ShowSystemStats": true,
                "Layout": "FullWidth",
                "FocusModeEnabled": true,
                "PinnedTrayIcons": ["path:app.exe"]
            }"#,
        )
        .unwrap();
        let s = load_from(&path);
        assert_eq!(s.enabled_widgets, ["Clock", "Media"]);
        assert!(s.show_clock_widget);
        assert!(s.show_system_stats_widget);
        assert!(s.full_width);
        assert!(s.focus_mode);
        assert_eq!(s.user_pinned_background_apps, ["path:app.exe"]);

        fs::write(&path, r#"{"Layout": "Floating", "Widgets": null}"#).unwrap();
        let s = load_from(&path);
        assert!(!s.full_width);
        assert!(s.enabled_widgets.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reference_profile_fields_survive_round_trip() {
        let dir = temp_dir("cfg-profile");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{
                "Profiles": [{
                    "Name": "Focus",
                    "HiddenApps": ["app.exe"],
                    "Widgets": ["Clock"],
                    "InstalledWidgets": ["Clock", "Media"],
                    "WidgetOrder": ["Clock", "Media"],
                    "DockItemOrder": ["start", "widget:Clock"],
                    "DockWidthMode": "Pixels",
                    "DockWidthPixels": 640,
                    "DockWidthPercent": 42,
                    "Theme": "Dark",
                    "ThemePack": "nord",
                    "ThemeApplicationIcons": false,
                    "LauncherHotkey": "AltSpace",
                    "DockHotkey": "CtrlAltD",
                    "CustomLauncherHotkey": "Ctrl+Alt+L",
                    "CustomDockHotkey": "Ctrl+Alt+K",
                    "FutureReferenceProfileKey": {"keep": true}
                }]
            }"#,
        )
        .unwrap();
        let loaded = load_from(&path);
        let profile = &loaded.profiles[0];
        assert_eq!(profile.name, "Focus");
        assert_eq!(profile.hidden_apps, ["app.exe"]);
        assert_eq!(profile.enabled_widgets, ["Clock"]);
        assert_eq!(profile.installed_widgets, ["Clock", "Media"]);
        assert_eq!(profile.widget_order, ["Clock", "Media"]);
        assert_eq!(profile.dock_item_order, ["start", "widget:Clock"]);
        assert_eq!(profile.dock_width_mode, Some(DockWidthMode::Pixels));
        assert_eq!(profile.dock_width_pixels, Some(640));
        assert_eq!(profile.theme, Some(AppTheme::Dark));
        assert_eq!(profile.theme_pack.as_deref(), Some("nord"));
        assert_eq!(profile.theme_application_icons, Some(false));
        assert_eq!(profile.launcher_hotkey, Some(HotkeyChoice::AltSpace));
        assert_eq!(
            profile.custom_launcher_hotkey.as_deref(),
            Some("Ctrl+Alt+L")
        );
        assert_eq!(
            profile
                .extra
                .get("FutureReferenceProfileKey")
                .and_then(Value::as_object)
                .and_then(|o| o.get("keep")),
            Some(&Value::Bool(true))
        );

        save_to(&path, &loaded).unwrap();
        let again = load_from(&path);
        assert_eq!(again.profiles[0], *profile);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_reports_reference_rules() {
        let mut s = DockSettings {
            dock_width_pixels: 10,
            dock_width_percent: 200,
            dock_height_pixels: 10,
            dock_height_percent: 99,
            dock_top_gap_pixels: 400,
            dock_top_gap_percent: 200,
            dock_bottom_gap_pixels: 400,
            dock_bottom_gap_percent: 200,
            focus_minutes: 0,
            focus_break_minutes: 99,
            focus_sessions: 99,
            wireless_battery_poll_seconds: 5,
            wireless_battery_low_threshold: 15,
            burn_in_shift_minutes: 4,
            oled_low_battery_threshold: 15,
            animation_fps_cap: 45,
            dock_scale: 2.0,
            widget_order: vec!["Clock".to_owned(), "Clock".to_owned()],
            weather_city: "x".repeat(81),
            ..DockSettings::default()
        };
        s.extra.insert("StatsMeters".to_owned(), Value::from(0xFF));
        let errors = s.validate();
        for rule in [
            "DockWidthPixelsOutOfRange",
            "DockWidthPercentOutOfRange",
            "DockHeightPixelsOutOfRange",
            "DockHeightPercentOutOfRange",
            "DockTopGapPixelsOutOfRange",
            "DockTopGapPercentOutOfRange",
            "DockBottomGapPixelsOutOfRange",
            "DockBottomGapPercentOutOfRange",
            "FocusMinutesOutOfRange",
            "FocusBreakMinutesOutOfRange",
            "FocusSessionsOutOfRange",
            "WirelessPollOutOfRange",
            "WirelessThresholdInvalid",
            "BurnInShiftMinutesOutOfRange",
            "OledLowBatteryThresholdInvalid",
            "AnimationFpsCapInvalid",
            "DockScaleOutOfRange",
            "StatsMetersInvalid",
            "WidgetOrderInvalid",
            "WeatherCityTooLong",
        ] {
            assert!(errors.contains(&rule), "missing {rule}: {errors:?}");
        }
        assert!(errors.len() >= 10);
    }

    #[test]
    fn validate_rejects_hotkey_conflict_and_widget_subset() {
        let mut s = DockSettings {
            custom_launcher_hotkey: Some("Ctrl+Alt+D".to_owned()),
            ..DockSettings::default()
        };
        assert!(s.validate().contains(&"HotkeysConflict"));
        s.custom_launcher_hotkey = Some("Ctrl+Alt+L".to_owned());
        s.custom_dock_hotkey = Some("not a hotkey".to_owned());
        assert!(s.validate().contains(&"CustomDockHotkeyInvalid"));
        s.custom_dock_hotkey = None;

        s.enabled_widgets = vec!["Clock".to_owned(), "RetiredWidget".to_owned()];
        let errors = s.validate();
        assert!(errors.contains(&"UnsupportedWidgetId"));
        assert!(errors.contains(&"EnabledWidgetNotInstalled"));
    }

    #[test]
    fn legacy_json_migrations_are_applied() {
        let raw = r#"{
            "DockBottomGap": 12,
            "SurfaceOpacity": 0.7,
            "DockIconPaddingX": 99,
            "Widgets": ["Memory", "Search", "AiUsage"],
            "InstalledWidgets": ["AiUsage"],
            "WidgetDensityOverrides": {"AiUsage": "Compact", "Memory": "Auto"},
            "AiUsageProviders": 1,
            "StatsMeters": "Cpu, Battery, Memory",
            "PinnedTrayIcons": [" path:a.exe ", "path:a.exe", "   "]
        }"#;
        let normalized = normalize_legacy_json(raw);
        let s: DockSettings = serde_json::from_str(&normalized).unwrap();
        assert_eq!(s.dock_bottom_gap_pixels, 12);
        assert_eq!(s.dock_opacity, 0.7);
        assert_eq!(s.menu_opacity, 0.7);
        assert_eq!(s.dock_icon_padding, MAX_ICON_PADDING);
        assert!(
            !s.enabled_widgets
                .iter()
                .any(|id| id == "Memory" || id == "Search")
        );
        assert!(s.enabled_widgets.iter().any(|id| id == "AiUsageClaude"));
        assert!(s.installed_widgets.iter().any(|id| id == "AiUsageCursor"));
        assert_eq!(s.ai_usage_providers, 0);
        assert_eq!(
            s.widget_density_overrides.get("AiUsageClaude"),
            Some(&WidgetDensityMode::Compact)
        );
        assert!(!s.widget_density_overrides.contains_key("Memory"));
        // The WIP port has a typed `stats_meters` field, so the reference string is decoded there
        // instead of being left in `extra` (the raw normalization above still drops the
        // retired flags).
        assert_eq!(s.stats_meters, 1 | 2);
        assert_eq!(s.user_pinned_background_apps, ["path:a.exe"]);
    }

    #[test]
    fn legacy_struct_migration_is_idempotent() {
        let mut s = DockSettings {
            enabled_widgets: vec!["AiUsage".to_owned(), "Tray".to_owned()],
            installed_widgets: vec!["AiUsage".to_owned()],
            ai_usage_providers: ai_providers::CODEX,
            ..DockSettings::default()
        };
        s.migrate_legacy();
        assert!(s.enabled_widgets.iter().any(|id| id == "AiUsageCodex"));
        assert!(!s.enabled_widgets.iter().any(|id| id == "Tray"));
        let once = s.clone();
        s.migrate_legacy();
        assert_eq!(s, once);
    }

    // ---- WIP behaviors preserved from the pre-integration port ----

    #[test]
    fn explicit_disabled_hotkey_does_not_fall_back_to_legacy_binding() {
        let settings = DockSettings {
            launcher_hotkey: HotkeyChoice::AltSpace,
            custom_launcher_hotkey: Some("Disabled".into()),
            dock_hotkey: HotkeyChoice::CtrlSpace,
            custom_dock_hotkey: Some("Backspace".into()),
            ..Default::default()
        };
        assert!(settings.launcher_binding().is_none());
        assert!(settings.dock_binding().is_none());
    }

    #[test]
    fn legacy_numeric_and_retired_stats_flags_are_sanitized() {
        assert_eq!(
            deserialize_stats_meters(&serde_json::json!(51)).unwrap(),
            51
        );
        assert_eq!(
            deserialize_stats_meters(&serde_json::json!("Cpu, CpuTemperature, Battery")).unwrap(),
            1 | 4
        );
    }

    #[test]
    fn language_round_trips_with_the_reference_json_key() {
        let dir = temp_dir("cfg-language");
        let path = dir.join("settings.json");

        fs::write(&path, r#"{"Language":"tr-TR"}"#).unwrap();
        assert_eq!(load_from(&path).language, "tr-TR");

        fs::write(&path, r#"{"Language":"Turkish"}"#).unwrap();
        assert_eq!(load_from(&path).language, "tr-TR");
        fs::write(&path, r#"{"Language":2}"#).unwrap();
        assert_eq!(load_from(&path).language, "ja-JP");
        fs::write(&path, r#"{"Language":"xx-YY"}"#).unwrap();
        assert!(crate::i18n::is_supported(&load_from(&path).language));

        let mut settings = load_from(&path);
        settings.language = "de-DE".to_owned();
        save_to(&path, &settings).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"Language\": \"de-DE\""), "{raw}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reference_settings_fields_round_trip() {
        let dir = temp_dir("cfg-deep");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{
                "Language":"de-DE",
                "Clock24Hour":false,
                "ClockShowSeconds":true,
                "DateStyle":"NumericDate",
                "DateFormat":"YyyyMmDd",
                "ClockDateLayout":"Below",
                "StatsMeters":3,
                "MenuBackground":"Solid",
                "WidgetBackground":"None",
                "SurfaceOpacity":0.75,
                "LauncherHotkey":"CtrlSpace",
                "DockHotkey":"WinAltD",
                "CheckUpdates":false
            }"#,
        )
        .unwrap();
        let settings = load_from(&path);
        assert_eq!(settings.language, "de-DE");
        assert!(!settings.clock_24_hour);
        assert!(settings.clock_show_seconds);
        assert_eq!(settings.date_style, ClockDateStyle::NumericDate);
        assert_eq!(settings.date_format, ClockDateFormat::YyyyMmDd);
        assert_eq!(settings.clock_date_layout, ClockDateLayout::Below);
        assert_eq!(settings.stats_meters, 3);
        assert_eq!(settings.menu_background, SurfaceBackground::Solid);
        assert_eq!(settings.widget_background, SurfaceBackground::None);
        assert!((settings.surface_opacity - 0.75).abs() < f64::EPSILON);
        assert_eq!(settings.launcher_hotkey, HotkeyChoice::CtrlSpace);
        assert!(!settings.check_updates);

        save_to(&path, &settings).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        for key in [
            "\"Language\"",
            "\"Clock24Hour\"",
            "\"ClockShowSeconds\"",
            "\"DateStyle\"",
            "\"DateFormat\"",
            "\"ClockDateLayout\"",
            "\"StatsMeters\"",
            "\"MenuBackground\"",
            "\"SurfaceOpacity\"",
            "\"LauncherHotkey\"",
            "\"CheckUpdates\"",
        ] {
            assert!(raw.contains(key), "missing {key} in {raw}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reference_profile_and_string_flags_do_not_reset_settings() {
        let dir = temp_dir("cfg-reference-migrate");
        let path = dir.join("settings.json");
        let raw = r#"{
            "Position": "Left",
            "StatsMeters": "Cpu, Memory, Gpu, GpuTemperature",
            "Profiles": [{
                "Name": "Default",
                "HiddenApps": [],
                "Widgets": ["Clock", "SystemStats"],
                "InstalledWidgets": ["Clock", "SystemStats"],
                "Theme": "Dark",
                "AutoHide": "Smart"
            }],
            "ActiveProfile": "Default"
        }"#;
        fs::write(&path, raw).unwrap();

        let loaded = load_from(&path);
        assert_eq!(
            loaded.position,
            DockPosition::Left,
            "reference settings must still load"
        );
        assert_eq!(loaded.stats_meters, 1 | 2 | 16 | 32);
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].name, "Default");
        assert_eq!(loaded.profiles[0].enabled_widgets, ["Clock", "SystemStats"]);
        assert_eq!(loaded.profiles[0].auto_hide, AutoHideMode::Smart);
        assert_eq!(loaded.profiles[0].position, DockPosition::Center);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dynamic_island_settings_round_trip_with_reference_keys() {
        let dir = temp_dir("cfg-island");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r##"{
                "DynamicIslandPlacements": { "clock": "*", "media": "\\\\.\\DISPLAY1" },
                "DynamicIslandVisibility": "DesktopOnly",
                "DynamicIslandHideInFullscreen": false,
                "DynamicIslandEnabled": false,
                "DynamicIslandAutoHide": true,
                "DynamicIslandBackground": "Solid",
                "DynamicIslandBackgroundColor": "#102030",
                "DynamicIslandOpacity": 0.65,
                "DynamicIslandUseTheme": false,
                "DynamicIslandWidgetOrders": { "\\\\.\\DISPLAY1": ["media", "clock"] },
                "ShowFjordFilesButton": true
            }"##,
        )
        .unwrap();

        let loaded = load_from(&path);
        assert_eq!(
            loaded.dynamic_island_visibility,
            DynamicIslandVisibilityMode::DesktopOnly
        );
        assert!(!loaded.dynamic_island_hide_in_fullscreen);
        assert!(
            !loaded.dynamic_island_enable,
            "reference alias must override the default"
        );
        assert!(loaded.dynamic_island_auto_hide);
        assert_eq!(loaded.dynamic_island_background, SurfaceBackground::Solid);
        assert_eq!(loaded.dynamic_island_background_hex(), Some("#102030"));
        assert!((loaded.dynamic_island_opacity - 0.65).abs() < f64::EPSILON);
        assert!(!loaded.dynamic_island_use_theme);
        assert!(loaded.show_fjord_files_button);
        assert_eq!(
            loaded
                .dynamic_island_placements
                .get("Clock")
                .map(String::as_str),
            Some("*")
        );
        assert_eq!(
            loaded
                .dynamic_island_widget_orders
                .get(r"\\.\DISPLAY1")
                .map(Vec::as_slice),
            Some(["media".to_owned(), "clock".to_owned()].as_slice())
        );

        save_to(&path, &loaded).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        for key in [
            "\"DynamicIslandPlacements\"",
            "\"DynamicIslandVisibility\"",
            "\"DynamicIslandHideInFullscreen\"",
            "\"DynamicIslandEnable\"",
            "\"DynamicIslandEnabled\"",
            "\"DynamicIslandAutoHide\"",
            "\"DynamicIslandBackground\"",
            "\"DynamicIslandBackgroundColor\"",
            "\"DynamicIslandOpacity\"",
            "\"DynamicIslandUseTheme\"",
            "\"DynamicIslandWidgetOrders\"",
            "\"ShowFjordFilesButton\"",
        ] {
            assert!(raw.contains(key), "missing {key} in {raw}");
        }
        let reloaded = load_from(&path);
        assert!(!reloaded.dynamic_island_enable);
        assert_eq!(reloaded.dynamic_island_background_hex(), Some("#102030"));
        assert!(reloaded.show_fjord_files_button);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dynamic_island_enable_default_and_reference_alias_agree() {
        // An absent reference key must keep the default; the reference spelling must override it.
        assert!(DockSettings::default().dynamic_island_enable);
        assert!(
            serde_json::from_str::<DockSettings>(r#"{"DynamicIslandEnabled":true}"#)
                .unwrap()
                .dynamic_island_enable
        );
        assert!(
            !serde_json::from_str::<DockSettings>(r#"{"DynamicIslandEnabled":false}"#)
                .unwrap()
                .dynamic_island_enable
        );
        // The reference-facing spelling wins a conflict because settings.json is shared.
        let both = r#"{"DynamicIslandEnable":true,"DynamicIslandEnabled":false}"#;
        let deduped = normalize_legacy_json(both);
        let deduped: Value = serde_json::from_str(&deduped).unwrap();
        assert!(deduped.get("DynamicIslandEnable").is_none());
        assert!(deduped.get("DynamicIslandEnabled").is_some());
        let deduped = serde_json::to_string(&deduped).unwrap();
        assert!(
            !serde_json::from_str::<DockSettings>(&deduped)
                .unwrap()
                .dynamic_island_enable
        );
    }

    #[test]
    fn island_background_hex_reads_typed_field_and_legacy_extra() {
        let mut settings = DockSettings::default();
        assert_eq!(settings.dynamic_island_background_hex(), None);
        settings.extra.insert(
            "DynamicIslandBackgroundColor".to_owned(),
            json!("  #abcdef  "),
        );
        assert_eq!(settings.dynamic_island_background_hex(), Some("#abcdef"));
        settings.dynamic_island_background_color = Some("bad-colour".to_owned());
        assert_eq!(settings.dynamic_island_background_hex(), None);
        settings.dynamic_island_background_color = Some("112233".to_owned());
        assert_eq!(settings.dynamic_island_background_hex(), Some("112233"));
    }

    #[test]
    fn middle_click_and_fisheye_intensity_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "opentaskbar-mclick-fisheye-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        let mut cfg = DockSettings::default();
        assert_eq!(cfg.middle_click_action, MiddleClickAction::NewWindow);
        assert_eq!(cfg.fisheye_intensity, FisheyeIntensity::Normal);
        assert!((FisheyeIntensity::Low.peak() - 0.20).abs() < f32::EPSILON);
        assert!((FisheyeIntensity::Normal.peak() - 0.42).abs() < f32::EPSILON);
        assert!((FisheyeIntensity::Strong.peak() - 0.70).abs() < f32::EPSILON);

        cfg.middle_click_action = MiddleClickAction::CloseWindow;
        cfg.fisheye_intensity = FisheyeIntensity::Strong;
        save_to(&path, &cfg).unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.middle_click_action, MiddleClickAction::CloseWindow);
        assert_eq!(loaded.fisheye_intensity, FisheyeIntensity::Strong);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_settings_fall_back_to_defaults_and_are_backed_up() {
        let dir = temp_dir("cfg-invalid");
        let path = dir.join("settings.json");
        // Both values are outside the reference slider ranges; the JSON itself is perfectly valid.
        fs::write(&path, r#"{"FocusMinutes":0,"DockWidthPixels":10}"#).unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.focus_minutes, 25);
        assert_eq!(loaded.dock_width_pixels, 900);
        assert!(path.exists(), "the rejected original must be kept");
        let has_backup = fs::read_dir(&dir).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("settings.json.invalid-")
        });
        assert!(has_backup, "invalid settings must be backed up");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_settings_load_without_creating_a_backup() {
        let dir = temp_dir("cfg-valid-no-backup");
        let path = dir.join("settings.json");
        fs::write(&path, r#"{"FocusMinutes":30,"DockWidthPixels":1200}"#).unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.focus_minutes, 30);
        assert_eq!(loaded.dock_width_pixels, 1200);
        assert!(!fs::read_dir(&dir).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("settings.json.invalid-")
        }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_writes_reference_aliases_next_to_canonical_keys() {
        let dir = temp_dir("cfg-alias-write");
        let path = dir.join("settings.json");
        let settings = DockSettings {
            enabled_widgets: vec!["Clock".to_owned(), "Media".to_owned()],
            installed_widgets: vec!["Clock".to_owned(), "Media".to_owned()],
            show_clock_widget: true,
            show_system_stats_widget: true,
            full_width: true,
            focus_mode: true,
            user_pinned_background_apps: vec!["path:app.exe".to_owned()],
            profiles: vec![DockProfile {
                name: "Focus".to_owned(),
                enabled_widgets: vec!["Clock".to_owned()],
                installed_widgets: vec!["Clock".to_owned()],
                ..DockProfile::default()
            }],
            ..DockSettings::default()
        };
        save_to(&path, &settings).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let root = value.as_object().unwrap();
        for (canonical, alias) in [
            ("EnabledWidgets", "Widgets"),
            ("ShowClockWidget", "ShowClock"),
            ("ShowSystemStatsWidget", "ShowSystemStats"),
            ("FocusMode", "FocusModeEnabled"),
            ("UserPinnedBackgroundApps", "PinnedTrayIcons"),
        ] {
            assert_eq!(
                root.get(canonical),
                root.get(alias),
                "missing {canonical}/{alias} pair in {raw}"
            );
        }
        assert_eq!(root.get("FullWidth").and_then(Value::as_bool), Some(true));
        assert_eq!(
            root.get("Layout").and_then(Value::as_str),
            Some("FullWidth")
        );
        assert_eq!(
            root.get("Widgets").and_then(Value::as_array).unwrap().len(),
            2
        );
        let profile = root
            .get("Profiles")
            .and_then(Value::as_array)
            .and_then(|p| p.first())
            .and_then(Value::as_object)
            .unwrap();
        assert_eq!(profile.get("EnabledWidgets"), profile.get("Widgets"));
        assert_eq!(
            profile
                .get("Widgets")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            1
        );

        // Rust must still read its own double-spelled file, and keep the canonical values.
        let loaded = load_from(&path);
        assert_eq!(loaded.enabled_widgets, settings.enabled_widgets);
        assert_eq!(loaded.show_clock_widget, settings.show_clock_widget);
        assert_eq!(
            loaded.show_system_stats_widget,
            settings.show_system_stats_widget
        );
        assert!(loaded.full_width && loaded.focus_mode);
        assert_eq!(
            loaded.user_pinned_background_apps,
            settings.user_pinned_background_apps
        );
        assert_eq!(loaded.profiles[0].enabled_widgets, ["Clock"]);
        assert!(loaded.extra.is_empty(), "aliases must not leak into extra");
        assert!(loaded.profiles[0].extra.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_uses_unique_guid_temporary_files() {
        let dir = temp_dir("cfg-guid-tmp");
        let path = dir.join("settings.json");
        let first = unique_settings_tmp(&path);
        let second = unique_settings_tmp(&path);
        assert_ne!(first, second);
        let name = first.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("settings.json.") && name.ends_with(".tmp"));
        let guid = name
            .strip_prefix("settings.json.")
            .and_then(|rest| rest.strip_suffix(".tmp"))
            .unwrap();
        assert_eq!(guid.len(), 36);
        assert_eq!(guid.as_bytes()[14], b'4');
        assert!(matches!(guid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));

        save_to(&path, &DockSettings::default()).unwrap();
        assert!(!dir.join("settings.json.tmp").exists());
        assert_eq!(
            fs::read_dir(&dir).unwrap().filter(|e| e.is_ok()).count(),
            1,
            "no temporary file may be left behind"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn conflicting_aliases_prefer_the_reference_spelling() {
        let dir = temp_dir("cfg-alias-conflict");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{
                "EnabledWidgets": ["Clock"],
                "Widgets": ["Media", "Weather"],
                "FullWidth": true,
                "Layout": "Floating",
                "FocusMode": true,
                "FocusModeEnabled": false,
                "DynamicIslandEnable": true,
                "DynamicIslandEnabled": false,
                "Profiles": [{"Name":"P","EnabledWidgets":["Clock"],"Widgets":["Weather","Focus"]}]
            }"#,
        )
        .unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.enabled_widgets, ["Media", "Weather"]);
        assert!(!loaded.full_width);
        assert!(!loaded.focus_mode);
        assert!(!loaded.dynamic_island_enable);
        assert_eq!(loaded.profiles[0].enabled_widgets, ["Weather", "Focus"]);
        assert!(loaded.extra.is_empty(), "aliases must not leak into extra");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn island_placements_dedupe_ordinal_ignore_case() {
        let mut placements = HashMap::new();
        placements.insert("Clock".to_owned(), "*".to_owned());
        placements.insert("clock".to_owned(), "*".to_owned());
        let mut s = DockSettings {
            dynamic_island_placements: placements,
            ..DockSettings::default()
        };
        assert!(s.validate().contains(&"IslandPlacementInvalid"));
        // The shared-file reader normalizes case variants instead of resetting the file.
        s.canonicalize_widget_keys();
        assert!(s.validate().is_empty());
        assert_eq!(s.dynamic_island_placements.len(), 1);

        let dir = temp_dir("cfg-island-dedupe");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{"DynamicIslandPlacements":{"Clock":"*","clock":"*"}}"#,
        )
        .unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.dynamic_island_placements.len(), 1);
        assert_eq!(
            loaded
                .dynamic_island_placements
                .get("Clock")
                .map(String::as_str),
            Some("*")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_names_dedupe_with_current_culture_rules() {
        let profile = |name: &str| DockProfile {
            name: name.to_owned(),
            ..DockProfile::default()
        };
        // Turkish CurrentCultureIgnoreCase treats i/İ and I/ı as the same letter.
        let tr = DockSettings {
            language: "tr-TR".to_owned(),
            profiles: vec![profile("i"), profile("İ")],
            ..DockSettings::default()
        };
        assert!(tr.validate().contains(&"ProfileInvalid"));
        let tr = DockSettings {
            language: "tr-TR".to_owned(),
            profiles: vec![profile("I"), profile("ı")],
            ..DockSettings::default()
        };
        assert!(tr.validate().contains(&"ProfileInvalid"));
        // Invariant culture keeps i and İ apart but folds I and ı together.
        let en = DockSettings {
            language: "en-US".to_owned(),
            profiles: vec![profile("i"), profile("İ")],
            ..DockSettings::default()
        };
        assert!(en.validate().is_empty(), "{:?}", en.validate());
        let en = DockSettings {
            language: "en-US".to_owned(),
            profiles: vec![profile("I"), profile("ı")],
            ..DockSettings::default()
        };
        assert!(en.validate().contains(&"ProfileInvalid"));
    }

    #[test]
    fn backup_manifest_version_gate_matches_reference() {
        assert!(backup_manifest_version_supported(Some(
            br#"{"Format":"OpenTaskbarFullBackup","Version":1,"CreatedUtc":"2026-01-01T00:00:00Z"}"#
        )));
        assert!(backup_manifest_version_supported(Some(br#"{"version":1}"#)));
        for raw in [
            br#"{"Version":99}"#.as_slice(),
            br#"{"Version":2}"#,
            br#"{"Version":"1"}"#,
            br#"{"Version":null}"#,
            b"{}",
            b"not json",
        ] {
            assert!(!backup_manifest_version_supported(Some(raw)), "{raw:?}");
        }
        assert!(!backup_manifest_version_supported(None));
    }
}
