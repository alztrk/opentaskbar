//! Dynamic Settings search catalogue.
//!
//! C# `SettingsWindow.Search.cs` walks the live WPF visual tree, so every rendered row is
//! searchable. The Rust build previously kept a hard-coded ~50-line list that drifted from the
//! real UI. This module derives entries from the *same* sources the Settings window renders:
//! the static row descriptors below plus `widgets::ALL_WIDGETS` (panel entries). Entries carry the
//! WS-A `DockSettings` PascalCase field names so searching `DockTopGapPixels`, `LauncherHotkey`,
//! `WeatherCity`, etc. resolves to the correct row even when the visible label differs.

use opentaskbar_core::settings::DockSettings;

use crate::widgets;

/// One searchable Settings row. `field` is the WS-A JSON/PascalCase setting name (may be empty
/// for non-setting actions), `keywords` holds aliases used by the C# UI and localizations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchEntry {
    pub page: &'static str,
    pub section: &'static str,
    pub label: String,
    pub field: &'static str,
    pub keywords: &'static [&'static str],
    /// Widget panel id (WidgetPanelHost target) when this is a widget entry.
    pub widget_id: Option<&'static str>,
    /// C# `DockWidgetOption.HasSettings` counterpart.
    pub has_panel: bool,
}

type RowDef = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
);

/// Rows mirrored from `ui/settings.rs`; keep in sync when adding a setting row.
pub const SETTINGS_ROWS: &[RowDef] = &[
    // Dock — Layout & Dimensions
    (
        "Dock",
        "Layout & Dimensions",
        "Alignment",
        "Position",
        &["dock", "left", "center", "right"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Dock Edge",
        "Edge",
        &["vertical", "left", "right", "side"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Dock Size",
        "Size",
        &["compact", "medium", "large"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Scale",
        "DockScale",
        &["zoom", "icon size"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Bottom Margin",
        "DockBottomGapPixels",
        &["gap", "margin", "bottom"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Top Gap",
        "DockTopGapPixels",
        &["top gap", "DockTopGapUnit", "DockTopGapPercent"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Icon Padding",
        "DockIconPadding",
        &["spacing"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Full Width",
        "Layout",
        &["FullWidth", "stretch"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Width Mode",
        "DockWidthMode",
        &["DockWidthPixels", "DockWidthPercent", "fixed", "percent"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Height Mode",
        "DockHeightMode",
        &["DockHeightPixels", "DockHeightPercent", "side dock"],
    ),
    (
        "Dock",
        "Layout & Dimensions",
        "Reset Width",
        "DockCustomWidthDips",
        &["auto", "width"],
    ),
    // Dock — Behavior & Displays
    (
        "Dock",
        "Behavior & Displays",
        "Auto-Hide",
        "AutoHideMode",
        &["hide", "smart", "always"],
    ),
    (
        "Dock",
        "Behavior & Displays",
        "All Monitors",
        "ShowOnAllMonitors",
        &["display", "multi monitor"],
    ),
    (
        "Dock",
        "Behavior & Displays",
        "Target Display",
        "TargetDisplay",
        &["monitor", "screen"],
    ),
    (
        "Dock",
        "Behavior & Displays",
        "Focus Mode",
        "FocusMode",
        &["distraction"],
    ),
    (
        "Dock",
        "Behavior & Displays",
        "Reveal on Win Key",
        "RevealDockOnWinKey",
        &["winkey", "windows key"],
    ),
    (
        "Dock",
        "Behavior & Displays",
        "Current Desktop Only",
        "CurrentDesktopOnly",
        &["virtual desktop"],
    ),
    (
        "Dock",
        "Behavior & Displays",
        "Wheel Volume",
        "WheelVolumeEnabled",
        &["mouse wheel", "audio"],
    ),
    // Dock — Utilities
    (
        "Dock",
        "Dock Utilities",
        "Background Apps",
        "ShowTrayButton",
        &["tray", "system tray"],
    ),
    (
        "Dock",
        "Dock Utilities",
        "Recycle Bin",
        "ShowRecycleBin",
        &["bin", "trash"],
    ),
    (
        "Dock",
        "Dock Utilities",
        "Show Desktop Strip",
        "ShowDesktopButton",
        &["show desktop"],
    ),
    (
        "Dock",
        "Dock Utilities",
        "Search Button",
        "ShowSearchButton",
        &["search"],
    ),
    (
        "Dock",
        "Dock Utilities",
        "Task View Button",
        "ShowTaskViewButton",
        &["task view"],
    ),
    (
        "Dock",
        "Dock Utilities",
        "Folder Stack",
        "StackFolders",
        &["folder", "stack", "watcher"],
    ),
    (
        "Dock",
        "Dock Utilities",
        "App Groups",
        "AppGroups",
        &["group"],
    ),
    // Themes
    (
        "Themes",
        "Theme & Surfaces",
        "Theme",
        "Theme",
        &["dark", "light", "system"],
    ),
    (
        "Themes",
        "Theme & Surfaces",
        "Theme Pack",
        "ThemePack",
        &["gallery", "swatch", "bundled"],
    ),
    (
        "Themes",
        "Theme & Surfaces",
        "Application Icons",
        "ThemeApplicationIcons",
        &["icon pack"],
    ),
    (
        "Themes",
        "Theme & Surfaces",
        "Background",
        "DockBackground",
        &["glass", "solid", "acrylic"],
    ),
    (
        "Themes",
        "Theme & Surfaces",
        "Opacity",
        "DockOpacity",
        &["transparency"],
    ),
    (
        "Themes",
        "Theme & Surfaces",
        "Corner Radius",
        "DockCornerRadius",
        &["round"],
    ),
    (
        "Themes",
        "Theme & Surfaces",
        "Theme Editor",
        "ThemeTokens",
        &["token", "brush", "color", "editor"],
    ),
    (
        "Themes",
        "Motion",
        "Smooth Animations",
        "AnimationsEnabled",
        &["animation"],
    ),
    (
        "Themes",
        "Motion",
        "Magnification",
        "MagnificationEnabled",
        &["fisheye", "magnify"],
    ),
    // Widgets
    (
        "Widgets",
        "Widgets",
        "Clock & Date",
        "ShowClockWidget",
        &["time", "calendar"],
    ),
    (
        "Widgets",
        "Widgets",
        "System Stats",
        "ShowSystemStatsWidget",
        &["cpu", "ram", "gpu"],
    ),
    (
        "Widgets",
        "Widgets",
        "Enabled Widgets",
        "EnabledWidgets",
        &["catalog", "density"],
    ),
    (
        "Widgets",
        "Widgets",
        "Widget Order",
        "WidgetOrder",
        &["reorder"],
    ),
    (
        "Widgets",
        "Widgets",
        "Weather City",
        "WeatherCity",
        &["country", "province", "location"],
    ),
    (
        "Widgets",
        "Widgets",
        "Weather Unit",
        "WeatherUnit",
        &["celsius", "fahrenheit"],
    ),
    (
        "Widgets",
        "Widgets",
        "Focus Minutes",
        "FocusMinutes",
        &["pomodoro"],
    ),
    (
        "Widgets",
        "Widgets",
        "Focus Break Minutes",
        "FocusBreakMinutes",
        &["break"],
    ),
    (
        "Widgets",
        "Widgets",
        "Focus Sessions",
        "FocusSessions",
        &["session"],
    ),
    (
        "Widgets",
        "Widgets",
        "Wireless Battery Poll",
        "WirelessBatteryPollSeconds",
        &["bluetooth", "interval"],
    ),
    (
        "Widgets",
        "Widgets",
        "Wireless Battery Threshold",
        "WirelessBatteryLowThreshold",
        &["low", "percent"],
    ),
    (
        "Widgets",
        "Widgets",
        "AI Usage Ring",
        "AiUsageRing",
        &["quota", "providers"],
    ),
    // Preferences
    (
        "Preferences",
        "System",
        "Start with Windows",
        "Startup",
        &["startup", "run", "registry"],
    ),
    (
        "Preferences",
        "Hotkeys",
        "Launcher Hotkey",
        "LauncherHotkey",
        &["capture", "shortcut", "key"],
    ),
    (
        "Preferences",
        "Hotkeys",
        "Dock Hotkey",
        "DockHotkey",
        &["capture", "shortcut", "key"],
    ),
    (
        "Preferences",
        "Accounts",
        "Cursor Sign In",
        "CursorAuth",
        &["oauth", "pkce", "ai", "account"],
    ),
    (
        "Preferences",
        "Backup",
        "Settings Backup",
        "SettingsBackup",
        &["json", "export", "import"],
    ),
    (
        "Preferences",
        "Backup",
        "Full Backup (.otbackup)",
        "FullBackup",
        &["zip", "pins", "theme", "archive"],
    ),
    (
        "Preferences",
        "Backup",
        "Factory Defaults",
        "Reset",
        &["reset", "defaults"],
    ),
    // About
    ("About", "Application", "Version", "Version", &["build"]),
    (
        "About",
        "Diagnostics & Recovery",
        "Data Folder",
        "DataFolder",
        &["localappdata", "path"],
    ),
    (
        "About",
        "Diagnostics & Recovery",
        "Runtime Log",
        "LogFile",
        &["diagnostics"],
    ),
    (
        "About",
        "Diagnostics & Recovery",
        "Windows Taskbar",
        "RestoreTaskbar",
        &["restore", "recovery"],
    ),
    (
        "About",
        "Links",
        "GitHub",
        "GitHub",
        &["source", "repository", "issues"],
    ),
];

/// C# `DockWidgetOption.HasSettings`: only widgets with a dedicated options panel get one.
/// WS-C `catalog_ops` will expose this authoritatively; until then use this local source.
pub fn widget_has_settings(id: &str) -> bool {
    matches!(
        id,
        "weather"
            | "stats"
            | "netspeed"
            | "battery"
            | "bluetoothbattery"
            | "storage"
            | "audio"
            | "quicksettings"
            | "focus"
            | "countdown"
            | "media"
    ) || id.starts_with("aiusage_")
}

#[derive(Clone, Debug)]
pub struct SearchCatalog {
    entries: Vec<SearchEntry>,
}

impl Default for SearchCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchCatalog {
    /// Builds the catalogue from the row descriptors and the live widget catalog.
    pub fn new() -> Self {
        let mut entries: Vec<SearchEntry> = SETTINGS_ROWS
            .iter()
            .map(|(page, section, label, field, keywords)| SearchEntry {
                page,
                section,
                label: (*label).to_owned(),
                field,
                keywords,
                widget_id: None,
                has_panel: false,
            })
            .collect();

        let widgets: Vec<&'static widgets::WidgetDescriptor> = widgets::ALL_WIDGETS
            .iter()
            .chain([
                widgets::widget_descriptor("stacks").unwrap(),
                widgets::widget_descriptor("groups").unwrap(),
            ])
            .collect();
        for widget in widgets {
            entries.push(SearchEntry {
                page: "Widgets",
                section: widget.category,
                label: widget.name.to_owned(),
                field: "EnabledWidgets",
                keywords: &["widget", "panel"],
                widget_id: Some(widget.id),
                has_panel: widget_has_settings(widget.id),
            });
        }
        Self { entries }
    }

    /// Adds user-defined rows (folder stacks, profiles) so searches follow the live config.
    pub fn from_settings(settings: &DockSettings) -> Self {
        let mut catalog = Self::new();
        for folder in &settings.stack_folders {
            catalog.entries.push(SearchEntry {
                page: "Dock",
                section: "Dock Utilities",
                label: format!(
                    "Folder: {}",
                    std::path::Path::new(folder)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
                field: "StackFolders",
                keywords: &["folder", "stack", "path"],
                widget_id: Some("stacks"),
                has_panel: true,
            });
        }
        for profile in &settings.profiles {
            catalog.entries.push(SearchEntry {
                page: "Dock",
                section: "Profiles",
                label: format!("Profile: {}", profile.name),
                field: "Profiles",
                keywords: &["profile", "switch"],
                widget_id: None,
                has_panel: false,
            });
        }
        catalog
    }

    pub fn entries(&self) -> &[SearchEntry] {
        &self.entries
    }

    /// Case-insensitive token search over label, section, field name and aliases.
    /// Prefix matches rank above sub-string matches; empty query yields all entries.
    pub fn search(&self, query: &str) -> Vec<&SearchEntry> {
        let query = query.trim();
        if query.is_empty() {
            return self.entries.iter().collect();
        }
        let tokens: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        let mut scored: Vec<(u8, &SearchEntry)> = Vec::new();
        for entry in &self.entries {
            let label = entry.label.to_ascii_lowercase();
            let section = entry.section.to_ascii_lowercase();
            let field = entry.field.to_ascii_lowercase();
            let mut total = 0u8;
            let mut matched = true;
            for token in &tokens {
                let score = if label.starts_with(token) || field.starts_with(token) {
                    3
                } else if label.contains(token) {
                    2
                } else if field.contains(token)
                    || section.contains(token)
                    || entry
                        .keywords
                        .iter()
                        .any(|k| k.to_ascii_lowercase().contains(token))
                {
                    1
                } else {
                    matched = false;
                    break;
                };
                total = total.saturating_add(score);
            }
            if matched {
                scored.push((total, entry));
            }
        }
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored.into_iter().map(|(_, e)| e).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{SearchCatalog, widget_has_settings};

    #[test]
    fn catalog_is_built_from_the_live_widget_catalog() {
        let catalog = SearchCatalog::new();
        let ids: Vec<_> = catalog
            .entries()
            .iter()
            .filter_map(|e| e.widget_id)
            .collect();
        assert!(ids.contains(&"weather"));
        assert!(ids.contains(&"aiusage_antigravity"));
        assert!(ids.contains(&"stacks"));
        assert!(ids.len() >= widgets_len());
    }

    fn widgets_len() -> usize {
        crate::widgets::ALL_WIDGETS.len()
    }

    #[test]
    fn search_matches_ws_a_field_names_and_visible_labels() {
        let catalog = SearchCatalog::new();
        let by_field = catalog.search("docktopgappixels");
        assert!(by_field.iter().any(|e| e.field == "DockTopGapPixels"));
        let by_alias = catalog.search("LauncherHotkey");
        assert!(by_alias.iter().any(|e| e.field == "LauncherHotkey"));
        let by_label = catalog.search("weather");
        assert!(by_label.iter().any(|e| e.label == "Weather"));
        let multi = catalog.search("theme editor");
        assert!(multi.iter().any(|e| e.label == "Theme Editor"));
        assert!(catalog.search("zzzz-not-present").is_empty());
        assert!(widget_has_settings("weather") && !widget_has_settings("clock"));
    }
}
