//! Searchable catalog for the Settings window.
//!
//! Every entry carries a stable `row_id` matching the GPUI id of the row (or widget card)
//! that owns the option. Selecting a result switches to the right page, applies the widget
//! category filter, scrolls that exact row into view and briefly highlights it. The page
//! and filter are only a means to make the target row reachable; the `row_id` is what makes
//! the jump precise.

use opentaskbar_core::i18n as loc;

use super::settings::SettingsPage;
use crate::widgets;

#[derive(Clone)]
pub(crate) struct SettingsSearchEntry {
    pub(crate) page: SettingsPage,
    pub(crate) title: String,
    pub(crate) section: String,
    pub(crate) keywords: &'static str,
    pub(crate) widget_filter: Option<&'static str>,
    /// Stable id of the rendered row/card. Defaults to the locale key of `title` and can be
    /// overridden when the same label appears in more than one section.
    pub(crate) row_id: String,
}

impl SettingsSearchEntry {
    fn new(page: SettingsPage, title_key: &str, section_key: &str, keywords: &'static str) -> Self {
        Self::with_row_id(page, title_key, section_key, keywords, title_key)
    }

    fn with_row_id(
        page: SettingsPage,
        title_key: &str,
        section_key: &str,
        keywords: &'static str,
        row_id: &str,
    ) -> Self {
        Self {
            page,
            title: loc::t(title_key),
            section: loc::t(section_key),
            keywords,
            widget_filter: None,
            row_id: row_id.to_owned(),
        }
    }
}

/// All settings that are reachable from the four settings pages plus the widget catalog.
/// Keys mirror the rows rendered in `settings.rs` and are translated with the active locale.
pub(crate) fn catalog() -> Vec<SettingsSearchEntry> {
    let dock_layout = format!("{} & {}", loc::t("SectionDockLayout"), loc::t("Dimensions"));
    let dock_behavior = format!(
        "{} & {}",
        loc::t("SectionBehavior"),
        loc::t("SectionDisplays")
    );
    let mut entries = vec![
        // Dock — layout & dimensions
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "DockAlignment",
            &dock_layout,
            "position edge bottom left right top alignment",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "DockSize",
            &dock_layout,
            "icon size compact medium large",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "DockScale",
            &dock_layout,
            "zoom scale percentage",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "DockBottomGap",
            &dock_layout,
            "bottom margin distance pixels",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "DockIconPadding",
            &dock_layout,
            "icon spacing padding pixels",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "LayoutFullWidth",
            &dock_layout,
            "fill screen edge to edge width",
        ),
        // Dock — behavior & displays
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "AutoHide",
            &dock_behavior,
            "hide smart always never",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "AllMonitors",
            &dock_behavior,
            "multi monitor displays all screens",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "FocusMode",
            &dock_behavior,
            "focus mode minimal widgets",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "RevealDockOnWinKey",
            &dock_behavior,
            "windows key reveal show hotkey",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "CurrentDesktopOnly",
            &dock_behavior,
            "virtual desktop current only",
        ),
        // Dock — interactions
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "TrayApps",
            &loc::t("SectionInteractions"),
            "system tray icons apps",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "RecycleBin",
            &loc::t("SectionInteractions"),
            "trash delete files",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "WidgetShowDesktop",
            &loc::t("SectionInteractions"),
            "show desktop button",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "MiddleClickAction",
            &loc::t("SectionInteractions"),
            "middle click close new window mouse wheel",
        ),
        // Dock — clock & stats
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "ClockFormat24Hour",
            &loc::t("WidgetClock"),
            "time 24 hour am pm clock",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "ClockShowSeconds",
            &loc::t("WidgetClock"),
            "clock seconds time",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "DateStyleDayShort",
            &loc::t("WidgetClock"),
            "weekday date day",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "ClockDateLayout",
            &loc::t("WidgetClock"),
            "date beside below layout",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "ClockDateFormat",
            &loc::t("WidgetClock"),
            "date format yyyy mm dd",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "Cpu",
            &loc::t("StatsSettingsHeading"),
            "cpu processor meter percentage",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Dock,
            "Memory",
            &loc::t("StatsSettingsHeading"),
            "ram memory meter percentage",
        ),
        SettingsSearchEntry::new(
            SettingsPage::DynamicIsland,
            "DynamicIslandVisibility",
            &loc::t("DynamicIslandTitle"),
            "dynamic island visibility desktop always top",
        ),
        SettingsSearchEntry::new(
            SettingsPage::DynamicIsland,
            "DynamicIslandHideFullscreen",
            &loc::t("DynamicIslandTitle"),
            "dynamic island fullscreen hide",
        ),
        SettingsSearchEntry::new(
            SettingsPage::DynamicIsland,
            "DynamicIslandAutoHide",
            &loc::t("DynamicIslandTitle"),
            "dynamic island auto hide top edge hover",
        ),
        SettingsSearchEntry::with_row_id(
            SettingsPage::DynamicIsland,
            "SurfaceOpacityLabel",
            &loc::t("DynamicIslandTitle"),
            "dynamic island transparency opacity",
            "DynamicIslandOpacity",
        ),
        SettingsSearchEntry::new(
            SettingsPage::DynamicIsland,
            "DynamicIslandWidgetLocation",
            &loc::t("DynamicIslandTitle"),
            "dynamic island widget location display dock",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Widgets,
            "WeatherUnit",
            &loc::t("WidgetWeather"),
            "weather temperature unit celsius fahrenheit c f",
        ),
        SettingsSearchEntry::with_row_id(
            SettingsPage::Widgets,
            "WirelessBatteryAlertLabel",
            &loc::t("WidgetBluetoothBattery"),
            "wireless bluetooth battery low alert threshold percent",
            "WirelessBatteryLowThreshold",
        ),
        // Themes
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "Theme",
            &loc::t("SectionTheme"),
            "dark light system appearance",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "DockBackground",
            &loc::t("SectionTheme"),
            "glass solid none background",
        ),
        SettingsSearchEntry::with_row_id(
            SettingsPage::Themes,
            "SurfaceOpacityLabel",
            &loc::t("SectionTheme"),
            "dock transparency opacity",
            "SurfaceOpacityLabel:Dock",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "DockCornerRadius",
            &loc::t("SectionTheme"),
            "round corners radius visual",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "Animations",
            &loc::t("Animations"),
            "motion effects animation",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "Magnification",
            &loc::t("Animations"),
            "fisheye icon magnification magnify",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "FisheyeIntensity",
            &loc::t("Animations"),
            "fisheye magnification strength intensity subtle normal strong",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "AnimationFps",
            &loc::t("Animations"),
            "fps frame rate hz refresh animations",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "SurfaceDock",
            &loc::t("SectionSurfaces"),
            "dock acrylic material surface",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "SurfaceMenu",
            &loc::t("SectionSurfaces"),
            "menu acrylic material surface",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "SurfaceWidget",
            &loc::t("SectionSurfaces"),
            "widget acrylic material surface",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Themes,
            "SurfaceSettings",
            &loc::t("SectionSurfaces"),
            "settings acrylic material surface",
        ),
        SettingsSearchEntry::with_row_id(
            SettingsPage::Themes,
            "SurfaceOpacityLabel",
            &loc::t("SectionSurfaces"),
            "surface transparent opacity",
            "SurfaceOpacityLabel:Surfaces",
        ),
        // Preferences
        SettingsSearchEntry::new(
            SettingsPage::Preferences,
            "Language",
            &loc::t("Preferences"),
            "locale culture language translation",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Preferences,
            "Profiles",
            &loc::t("Preferences"),
            "profile dock layout preset",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Preferences,
            "CheckUpdatesSetting",
            &loc::t("CheckForUpdates"),
            "update check automatic version",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Preferences,
            "Startup",
            &loc::t("SectionSystem"),
            "run at startup boot launch",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Preferences,
            "SettingsBackup",
            &loc::t("Backup"),
            "export import backup restore json",
        ),
        SettingsSearchEntry::new(
            SettingsPage::Preferences,
            "ResetDefaults",
            &loc::t("Backup"),
            "reset restore defaults factory",
        ),
        // About
        SettingsSearchEntry::new(
            SettingsPage::About,
            "Version",
            &loc::t("About"),
            "build version",
        ),
        SettingsSearchEntry::new(
            SettingsPage::About,
            "DataFolder",
            &loc::t("Diagnostics"),
            "data folder path files",
        ),
        SettingsSearchEntry::new(
            SettingsPage::About,
            "OpenLogFile",
            &loc::t("Diagnostics"),
            "log file diagnostics open",
        ),
        SettingsSearchEntry::new(
            SettingsPage::About,
            "WindowsTaskbar",
            &loc::t("Diagnostics"),
            "restore windows original taskbar",
        ),
    ];

    entries.push(SettingsSearchEntry::new(
        SettingsPage::DynamicIsland,
        "DynamicIslandVisibility",
        &loc::t("DynamicIslandTitle"),
        "island visibility above windows desktop full screen",
    ));
    entries.push(SettingsSearchEntry::new(
        SettingsPage::DynamicIsland,
        "DynamicIslandHideFullscreen",
        &loc::t("DynamicIslandTitle"),
        "island hide fullscreen app display",
    ));

    for widget in widgets::ALL_WIDGETS {
        entries.push(SettingsSearchEntry {
            page: SettingsPage::Widgets,
            title: widgets::widget_name(widget.id),
            section: super::settings::category_label(widget.category),
            keywords: widget.id,
            widget_filter: Some(widget.category),
            row_id: format!("widget-{}", widget.id),
        });
    }

    entries
}

/// Match every whitespace-separated term against the entry's title, section, page and
/// extra keywords. Matching is locale-tolerant so Turkish `ı`/`İ` also match plain `i`.
pub(crate) fn search<'a>(
    entries: &'a [SettingsSearchEntry],
    query: &str,
) -> Vec<&'a SettingsSearchEntry> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(normalize)
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }

    entries
        .iter()
        .filter(|entry| {
            let haystack = normalize(&format!(
                "{} {} {} {}",
                entry.title,
                entry.section,
                entry.page.title(),
                entry.keywords
            ));
            terms.iter().all(|term| haystack.contains(term))
        })
        .take(80)
        .collect()
}

fn normalize(value: &str) -> String {
    value
        .to_lowercase()
        .replace(['ı', 'İ'], "i")
        .replace('\u{0307}', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<SettingsSearchEntry> {
        vec![
            SettingsSearchEntry::new(
                SettingsPage::Dock,
                "DockAlignment",
                "Layout",
                "position edge bottom left right",
            ),
            SettingsSearchEntry::new(
                SettingsPage::Themes,
                "Animations",
                "Effects",
                "motion transitions",
            ),
        ]
    }

    #[test]
    fn empty_query_returns_nothing() {
        assert!(search(&fixture(), "  ").is_empty());
    }

    #[test]
    fn all_terms_must_match() {
        let entries = fixture();
        let hits = search(&entries, "dock bottom");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, loc::t("DockAlignment"));
        assert!(search(&entries, "bottom effects").is_empty());
    }

    #[test]
    fn every_entry_has_a_stable_row_id() {
        let catalog = catalog();
        assert!(catalog.iter().all(|entry| !entry.row_id.is_empty()));
        // The two opacity controls live in different sections, so they need distinct ids.
        let opacity: Vec<&str> = catalog
            .iter()
            .filter(|entry| entry.row_id.starts_with("SurfaceOpacityLabel"))
            .map(|entry| entry.row_id.as_str())
            .collect();
        assert!(opacity.contains(&"SurfaceOpacityLabel:Dock"));
        assert!(opacity.contains(&"SurfaceOpacityLabel:Surfaces"));
        // Widget results point at their card, not at a locale key.
        assert!(catalog
            .iter()
            .any(|entry| entry.widget_filter.is_some() && entry.row_id.starts_with("widget-")));
    }

    #[test]
    fn explicit_row_id_overrides_locale_key() {
        let entry = SettingsSearchEntry::with_row_id(
            SettingsPage::Themes,
            "SurfaceOpacityLabel",
            "SectionTheme",
            "opacity",
            "SurfaceOpacityLabel:Dock",
        );
        assert_eq!(entry.row_id, "SurfaceOpacityLabel:Dock");
        assert_eq!(SettingsSearchEntry::new(
            SettingsPage::Dock,
            "DockAlignment",
            "Layout",
            "alignment"
        )
        .row_id, "DockAlignment");
    }

    #[test]
    fn match_is_case_and_locale_insensitive() {
        let entries = vec![SettingsSearchEntry::new(
            SettingsPage::Preferences,
            "Ölçekleme",
            "Görünüm",
            "İstanbul saati",
        )];
        assert_eq!(search(&entries, "ÖLÇEKLEME").len(), 1);
        assert_eq!(search(&entries, "istanbul").len(), 1);
    }
}

// --- Dynamic search catalogue (C# SettingsWindow.Search.cs parity) ---

use opentaskbar_core::settings::DockSettings;

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
mod dynamic_tests {
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
        // WS-C adds `aiusage_antigravity` to `ALL_WIDGETS`; until that merge lands, accept any
        // AI-usage panel so the catalogue stays in sync with the live widget list.
        assert!(ids.iter().any(|id| id.starts_with("aiusage_")));
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
