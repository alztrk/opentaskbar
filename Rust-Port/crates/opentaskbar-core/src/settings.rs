//! Persisted settings compatible with OpenTaskbar's C# `settings.json`.
//! Unknown fields are preserved via `extra` so switching between builds never drops data.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DockAppGroup {
    pub id: String,
    pub name: String,
    pub executable_paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DockProfile {
    pub name: String,
    pub position: DockPosition,
    pub size: DockSize,
    pub auto_hide: AutoHideMode,
    pub enabled_widgets: Vec<String>,
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
            extra: Map::new(),
        }
    }
}

impl DockSettings {
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

/// Loads settings from `path`; backs up corrupt JSON before returning defaults.
pub fn load_from(path: &Path) -> DockSettings {
    let Ok(raw) = fs::read_to_string(path) else {
        return DockSettings::default();
    };
    let clean = raw.trim_start_matches('\u{feff}');
    match serde_json::from_str::<DockSettings>(clean) {
        Ok(mut settings) => {
            settings.dock_opacity = settings.dock_opacity.clamp(0.1, 1.0);
            settings.dock_scale = settings.dock_scale.clamp(0.75, 1.5);
            settings
        }
        Err(_) => {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let backup = path.with_file_name(format!("settings.json.invalid-{stamp}"));
            let _ = fs::rename(path, backup);
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
        assert!(!path.exists());
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
}
