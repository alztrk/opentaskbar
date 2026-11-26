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
            "DynamicIslandWidgetLocation",
            &loc::t("DynamicIslandTitle"),
            "dynamic island widget location display dock",
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
