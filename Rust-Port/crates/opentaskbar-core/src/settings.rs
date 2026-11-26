//! Persisted settings compatible with OpenTaskbar's C# `settings.json`.
//! Unknown fields are preserved via `extra` so switching between builds never drops data.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

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

/// C# persists `SystemMeters` as a JSON string (`"Cpu, Memory, Gpu"`) through
/// `JsonStringEnumConverter`, while older builds used the numeric bitmask. Accept both so a
/// C# settings file is never rejected for a single field.
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
            // Retired flag: keep the numeric slot (8) unused for compatibility.
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

/// Accepts C# `AppLanguageConverter` values: BCP 47 strings, legacy English
/// names and legacy numeric codes. Unknown values fall back to the system UI language.
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
pub enum WidgetDensityMode {
    #[default]
    Auto,
    Compact,
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DynamicIslandVisibilityMode {
    #[default]
    Always,
    DesktopOnly,
}

pub const DYNAMIC_ISLAND_ALL_DISPLAYS: &str = "*";

fn default_true() -> bool {
    true
}

fn default_dynamic_island_opacity() -> f64 {
    0.9
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DockAppGroup {
    pub id: String,
    pub name: String,
    pub executable_paths: Vec<String>,
}

/// C# `DockProfileService.Capture` only writes `Name`, `HiddenApps` and a handful of optional
/// profile-scoped fields. Missing fields must fall back instead of rejecting the whole settings file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct DockProfile {
    pub name: String,
    pub position: DockPosition,
    pub size: DockSize,
    pub auto_hide: AutoHideMode,
    #[serde(alias = "Widgets")]
    pub enabled_widgets: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct DockSettings {
    /// BCP 47 UI culture, e.g. `"tr-TR"`. Same JSON key as C# `DockSettings.Language`.
    #[serde(
        default = "default_language",
        deserialize_with = "deserialize_language"
    )]
    pub language: String,
    /// User-selected Windows font family. Empty means "use the Windows native UI font".
    #[serde(default)]
    pub ui_font_family: String,
    /// Base UI font size in pixels (9..24). Scales menus, flyouts, tooltips and dialogs.
    #[serde(default = "default_font_size")]
    pub ui_font_size: f32,
    /// Active theme pack id (C# `ThemePack`). Empty means the built-in palette.
    #[serde(default)]
    pub theme_pack: String,
    pub position: DockPosition,
    pub edge: DockEdge,
    pub size: DockSize,
    pub theme: AppTheme,
    pub auto_hide: AutoHideMode,
    pub dock_background: SurfaceBackground,
    pub dock_opacity: f64,
    pub dock_border_thickness: f64,
    pub dock_corner_radius: Option<i32>,
    pub dock_bottom_gap_pixels: i32,
    pub dock_icon_padding: i32,
    pub dock_scale: f64,
    pub animations_enabled: bool,
    pub magnification_enabled: bool,
    pub focus_mode: bool,
    pub show_recycle_bin: bool,
    pub show_tray_button: bool,
    pub show_on_all_monitors: bool,
    pub show_desktop_button: bool,
    pub full_width: bool,
    pub dock_custom_width_dips: Option<f32>,
    pub reveal_dock_on_win_key: bool,
    pub current_desktop_only: bool,
    /// C# `Clock24Hour` / `ClockShowSeconds` / `DateStyle` / `DateFormat` / `ClockDateLayout`.
    pub clock_24_hour: bool,
    pub clock_show_seconds: bool,
    pub date_style: ClockDateStyle,
    pub date_format: ClockDateFormat,
    pub clock_date_layout: ClockDateLayout,
    /// C# `SystemMeters` bit flags: Cpu=1, Memory=2, Gpu=16, GpuTemperature=32.
    #[serde(deserialize_with = "deserialize_stats_meters")]
    pub stats_meters: u32,
    pub global_widget_density: WidgetDensityMode,
    /// Independent materials for the four UI surfaces (C# `SurfaceKind`).
    pub menu_background: SurfaceBackground,
    pub widget_background: SurfaceBackground,
    pub settings_background: SurfaceBackground,
    pub surface_opacity: f64,
    pub dock_border_color: Option<String>,
    pub menu_border_thickness: f64,
    pub menu_border_color: Option<String>,
    pub widget_border_thickness: f64,
    pub widget_border_color: Option<String>,
    pub settings_border_thickness: f64,
    pub settings_border_color: Option<String>,
    /// Persisted C# `HotkeyChoice` names; global registration happens in the dock process.
    pub launcher_hotkey: String,
    pub dock_hotkey: String,
    /// Custom hotkey text (C# `CustomLauncherHotkey` / `CustomDockHotkey`).
    #[serde(default)]
    pub custom_launcher_hotkey: Option<String>,
    #[serde(default)]
    pub custom_dock_hotkey: Option<String>,
    pub check_updates: bool,
    pub show_clock_widget: bool,
    pub show_system_stats_widget: bool,
    /// Explicitly user-enabled widget IDs (e.g. `"clock"`, `"stats"`, `"audio"`, `"media"`,
    /// `"netspeed"`, `"battery"`, `"storage"`, `"keyboard"`, `"dayprogress"`, `"stopwatch"`,
    /// `"countdown"`, `"focus"`, `"weather"`, `"quicksettings"`). Empty by default.
    pub enabled_widgets: Vec<String>,
    pub weather_city: String,
    pub stack_folders: Vec<String>,
    pub app_groups: Vec<DockAppGroup>,
    pub profiles: Vec<DockProfile>,
    pub active_profile: Option<String>,
    pub pin_order: Option<Vec<String>>,
    pub user_pinned_apps: Vec<String>,
    pub user_unpinned_apps: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub user_pinned_background_apps: Vec<String>,
    /// Dynamic Island widget placements keyed by widget ID -> display device name (`"*"` = all displays).
    #[serde(
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub dynamic_island_placements: HashMap<String, String>,
    pub dynamic_island_visibility: DynamicIslandVisibilityMode,
    #[serde(default = "default_true")]
    pub dynamic_island_hide_in_fullscreen: bool,
    /// Master switch for the floating island windows (C# `DynamicIslandEnable`).
    #[serde(default = "default_true")]
    pub dynamic_island_enable: bool,
    /// Hide the island while a full-screen app occupies its display (C# `DynamicIslandAutoHide`).
    #[serde(default)]
    pub dynamic_island_auto_hide: bool,
    /// Island material (C# `DynamicIslandBackground`).
    #[serde(default)]
    pub dynamic_island_background: SurfaceBackground,
    /// Island chrome opacity (C# `DynamicIslandOpacity`).
    #[serde(default = "default_dynamic_island_opacity")]
    pub dynamic_island_opacity: f64,
    /// Use theme-derived colors for the island material (C# `DynamicIslandUseTheme`).
    #[serde(default = "default_true")]
    pub dynamic_island_use_theme: bool,
    /// Per-display Dynamic Island widget ordering keyed by display device name.
    #[serde(
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub dynamic_island_widget_orders: HashMap<String, Vec<String>>,
    /// Unknown keys from other OpenTaskbar builds preserved on round-trip save.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for DockSettings {
    fn default() -> Self {
        Self {
            language: default_language(),
            ui_font_family: String::new(),
            ui_font_size: default_font_size(),
            theme_pack: String::new(),
            position: DockPosition::Center,
            edge: DockEdge::Bottom,
            size: DockSize::Compact,
            theme: AppTheme::System,
            auto_hide: AutoHideMode::Disabled,
            dock_background: SurfaceBackground::Glass,
            dock_opacity: 0.88,
            dock_border_thickness: 1.0,
            dock_corner_radius: None,
            dock_bottom_gap_pixels: 8,
            dock_icon_padding: 0,
            dock_scale: 1.0,
            animations_enabled: true,
            magnification_enabled: true,
            focus_mode: false,
            show_recycle_bin: true,
            show_tray_button: true,
            show_on_all_monitors: true,
            show_desktop_button: true,
            full_width: false,
            dock_custom_width_dips: None,
            reveal_dock_on_win_key: true,
            current_desktop_only: false,
            clock_24_hour: true,
            clock_show_seconds: false,
            date_style: ClockDateStyle::DayAndShortDate,
            date_format: ClockDateFormat::System,
            clock_date_layout: ClockDateLayout::Beside,
            stats_meters: 1 | 2 | 16 | 32,
            global_widget_density: WidgetDensityMode::Auto,
            menu_background: SurfaceBackground::Glass,
            widget_background: SurfaceBackground::Glass,
            settings_background: SurfaceBackground::Glass,
            surface_opacity: 0.9,
            dock_border_color: None,
            menu_border_thickness: 1.0,
            menu_border_color: None,
            widget_border_thickness: 1.0,
            widget_border_color: None,
            settings_border_thickness: 1.0,
            settings_border_color: None,
            launcher_hotkey: "AltSpace".to_owned(),
            dock_hotkey: "CtrlAltD".to_owned(),
            custom_launcher_hotkey: None,
            custom_dock_hotkey: None,
            check_updates: true,
            show_clock_widget: false,
            show_system_stats_widget: false,
            enabled_widgets: Vec::new(),
            weather_city: "Istanbul".to_owned(),
            stack_folders: Vec::new(),
            app_groups: Vec::new(),
            profiles: Vec::new(),
            active_profile: None,
            pin_order: None,
            user_pinned_apps: Vec::new(),
            user_unpinned_apps: Vec::new(),
            user_pinned_background_apps: Vec::new(),
            dynamic_island_placements: HashMap::new(),
            dynamic_island_visibility: DynamicIslandVisibilityMode::Always,
            dynamic_island_hide_in_fullscreen: true,
            dynamic_island_enable: true,
            dynamic_island_auto_hide: false,
            dynamic_island_background: SurfaceBackground::Glass,
            dynamic_island_opacity: 0.9,
            dynamic_island_use_theme: true,
            dynamic_island_widget_orders: HashMap::new(),
            extra: Map::new(),
        }
    }
}

impl DockSettings {
    /// Effective launcher hotkey: custom text wins over the persisted enum name.
    /// `"Disabled"` is an explicit opt-out, not an invalid value that falls back to the legacy binding.
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
            .and_then(crate::hotkey::parse)
            .or_else(|| crate::hotkey::parse(&self.launcher_hotkey))
    }

    /// Effective dock hotkey: custom text wins over the persisted enum name.
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
            .and_then(crate::hotkey::parse)
            .or_else(|| crate::hotkey::parse(&self.dock_hotkey))
    }

    pub fn is_widget_enabled(&self, id: &str) -> bool {
        if self.focus_mode && id != "focus" && id != "clock" {
            return false;
        }
        if id == "clock" && self.show_clock_widget {
            return true;
        }
        if id == "stats" && self.show_system_stats_widget {
            return true;
        }
        self.enabled_widgets
            .iter()
            .any(|w| w.eq_ignore_ascii_case(id))
    }

    pub fn set_widget_enabled(&mut self, id: &str, enabled: bool) {
        if id == "clock" {
            self.show_clock_widget = enabled;
        }
        if id == "stats" {
            self.show_system_stats_widget = enabled;
        }
        if enabled {
            if !self
                .enabled_widgets
                .iter()
                .any(|w| w.eq_ignore_ascii_case(id))
            {
                self.enabled_widgets.push(id.to_owned());
            }
        } else {
            self.enabled_widgets.retain(|w| !w.eq_ignore_ascii_case(id));
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
}

/// Application data directory: `%LOCALAPPDATA%\OpenTaskBar-Rust`.
pub fn app_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("OpenTaskBar-Rust")
}

pub fn settings_path() -> PathBuf {
    app_data_dir().join("settings.json")
}

/// Loads settings from `path`; copies corrupt JSON to a timestamped backup before returning defaults.
/// The original is deliberately left in place: a single malformed field must not destroy the only copy,
/// and the next successful save replaces it atomically while the backup survives.
pub fn load_from(path: &Path) -> DockSettings {
    let Ok(raw) = fs::read_to_string(path) else {
        return DockSettings::default();
    };
    let clean = raw.trim_start_matches('\u{feff}');
    match serde_json::from_str::<DockSettings>(clean) {
        Ok(mut settings) => {
            settings.dock_opacity = settings.dock_opacity.clamp(0.1, 1.0);
            settings.dynamic_island_opacity = settings.dynamic_island_opacity.clamp(0.1, 1.0);
            settings.surface_opacity = settings.surface_opacity.clamp(0.1, 1.0);
            settings.ui_font_size = settings.ui_font_size.clamp(9.0, 24.0);
            settings.dock_scale = settings.dock_scale.clamp(0.75, 1.5);
            settings
        }
        Err(_) => {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let backup = path.with_file_name(format!("settings.json.invalid-{stamp}"));
            let _ = fs::copy(path, backup);
            DockSettings::default()
        }
    }
}

/// Atomically writes settings to `path`.
pub fn save_to(path: &Path, settings: &DockSettings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_unknown_fields() {
        let dir = std::env::temp_dir().join(format!("ot-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let raw = r#"{
            "Position": "Left",
            "Size": "Large",
            "Theme": "Dark",
            "AutoHide": "Smart",
            "DockOpacity": 0.75,
            "CustomCSharpField": "keep-me"
        }"#;
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, raw).unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.position, DockPosition::Left);
        assert_eq!(loaded.size, DockSize::Large);
        assert_eq!(loaded.theme, AppTheme::Dark);
        assert_eq!(loaded.auto_hide, AutoHideMode::Smart);
        assert_eq!(
            loaded
                .extra
                .get("CustomCSharpField")
                .and_then(|v| v.as_str()),
            Some("keep-me")
        );

        save_to(&path, &loaded).unwrap();
        let reloaded = load_from(&path);
        assert_eq!(
            reloaded
                .extra
                .get("CustomCSharpField")
                .and_then(|v| v.as_str()),
            Some("keep-me")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_json_returns_defaults_and_creates_backup() {
        let dir = std::env::temp_dir().join(format!("ot-corrupt-{}", std::process::id()));
        let path = dir.join("settings.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "{not valid json").unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.position, DockPosition::Center);
        // The original is kept so a single bad field cannot destroy the user's only copy.
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
    fn csharp_profile_and_string_flags_do_not_reset_settings() {
        let dir = std::env::temp_dir().join(format!("ot-csharp-migrate-{}", std::process::id()));
        let path = dir.join("settings.json");
        // Exactly the shape C# DockProfileService.Capture + JsonStringEnumConverter write.
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
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, raw).unwrap();

        let loaded = load_from(&path);
        assert_eq!(
            loaded.position,
            DockPosition::Left,
            "C# settings must still load"
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
    fn explicit_disabled_hotkey_does_not_fall_back_to_legacy_binding() {
        let settings = DockSettings {
            launcher_hotkey: "AltSpace".into(),
            custom_launcher_hotkey: Some("Disabled".into()),
            dock_hotkey: "CtrlSpace".into(),
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
    fn language_round_trips_with_the_csharp_json_key() {
        let dir = std::env::temp_dir().join(format!("ot-lang-{}", std::process::id()));
        let path = dir.join("settings.json");
        fs::create_dir_all(&dir).unwrap();

        fs::write(&path, r#"{"Language":"tr-TR"}"#).unwrap();
        assert_eq!(load_from(&path).language, "tr-TR");

        // Legacy C# AppLanguageConverter values remain readable.
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
    fn csharp_settings_fields_round_trip() {
        let dir = std::env::temp_dir().join(format!("ot-deep-{}", std::process::id()));
        let path = dir.join("settings.json");
        fs::create_dir_all(&dir).unwrap();
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
        assert_eq!(settings.launcher_hotkey, "CtrlSpace");
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

        // Focus mode keeps only clock and focus visible.
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
    fn dynamic_island_settings_round_trip_with_csharp_keys() {
        let dir = std::env::temp_dir().join(format!("ot-island-{}", std::process::id()));
        let path = dir.join("settings.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &path,
            r#"{
                "DynamicIslandPlacements": { "clock": "*", "media": "\\\\.\\DISPLAY1" },
                "DynamicIslandVisibility": "DesktopOnly",
                "DynamicIslandHideInFullscreen": false,
                "DynamicIslandEnable": false,
                "DynamicIslandAutoHide": true,
                "DynamicIslandBackground": "Solid",
                "DynamicIslandOpacity": 0.65,
                "DynamicIslandUseTheme": false,
                "DynamicIslandWidgetOrders": { "\\\\.\\DISPLAY1": ["media", "clock"] }
            }"#,
        )
        .unwrap();

        let loaded = load_from(&path);
        assert_eq!(
            loaded.dynamic_island_visibility,
            DynamicIslandVisibilityMode::DesktopOnly
        );
        assert!(!loaded.dynamic_island_hide_in_fullscreen);
        assert!(!loaded.dynamic_island_enable);
        assert!(loaded.dynamic_island_auto_hide);
        assert_eq!(loaded.dynamic_island_background, SurfaceBackground::Solid);
        assert!((loaded.dynamic_island_opacity - 0.65).abs() < f64::EPSILON);
        assert!(!loaded.dynamic_island_use_theme);
        assert_eq!(
            loaded
                .dynamic_island_placements
                .get("clock")
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
            "\"DynamicIslandAutoHide\"",
            "\"DynamicIslandBackground\"",
            "\"DynamicIslandOpacity\"",
            "\"DynamicIslandUseTheme\"",
            "\"DynamicIslandWidgetOrders\"",
        ] {
            assert!(raw.contains(key), "missing {key} in {raw}");
        }

        // Absent keys fall back to the C#-compatible defaults.
        fs::write(&path, r#"{}"#).unwrap();
        let defaults = load_from(&path);
        assert!(defaults.dynamic_island_enable);
        assert!(!defaults.dynamic_island_auto_hide);
        assert_eq!(defaults.dynamic_island_background, SurfaceBackground::Glass);
        assert!((defaults.dynamic_island_opacity - 0.9).abs() < f64::EPSILON);
        assert!(defaults.dynamic_island_use_theme);
        let _ = fs::remove_dir_all(&dir);
    }
}
