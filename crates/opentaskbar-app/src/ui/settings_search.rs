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
    /// Widget card to auto-expand when jumping to a per-widget inline setting.
    pub(crate) expand_widget: Option<&'static str>,
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
            expand_widget: None,
            row_id: row_id.to_owned(),
        }
    }

    fn widget_setting(
        widget_id: &'static str,
        title_key: &str,
        section_key: &str,
        keywords: &'static str,
        row_id: &str,
    ) -> Self {
        Self {
            page: SettingsPage::Widgets,
            title: loc::t(title_key),
            section: loc::t(section_key),
            keywords,
            widget_filter: Some("All"),
            expand_widget: Some(widget_id),
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
    let mut entries = Vec::with_capacity(96);

    let mut add = |page: SettingsPage, section: &str, items: &[(&str, &str, &'static str)]| {
        for &(title_key, row_id, keywords) in items {
            let id = if row_id.is_empty() { title_key } else { row_id };
            entries.push(SettingsSearchEntry::with_row_id(
                page, title_key, section, keywords, id,
            ));
        }
    };

    // Dock — layout & dimensions
    add(
        SettingsPage::Dock,
        &dock_layout,
        &[
            (
                "DockAlignment",
                "",
                "position edge bottom left right top alignment",
            ),
            ("DockSize", "", "icon size compact medium large"),
            (
                "StartPlacement",
                "",
                "start button placement end hidden in widgets trailing",
            ),
            ("StartWidgetIndex", "", "start widget index position order"),
            ("DockScale", "", "zoom scale percentage"),
            ("DockBottomGap", "", "bottom margin distance pixels"),
            ("DockIconPadding", "", "icon spacing padding pixels"),
            ("LayoutFullWidth", "", "fill screen edge to edge width"),
            (
                "ContentAlignment",
                "",
                "content align start center end icons",
            ),
            ("DockWidthMode", "", "width mode pixels percent fill auto"),
            ("DockHeightMode", "", "height mode pixels percent fill auto"),
            ("DockTopGap", "", "top gap pixels percent margin"),
        ],
    );
    // Dock — behavior & displays
    add(
        SettingsPage::Dock,
        &dock_behavior,
        &[
            ("AutoHide", "", "hide smart always never"),
            ("AllMonitors", "", "multi monitor displays all screens"),
            ("FocusMode", "", "focus mode minimal widgets"),
            ("RevealDockOnWinKey", "", "windows key reveal show hotkey"),
            ("CurrentDesktopOnly", "", "virtual desktop current only"),
            (
                "PreviewsEnabled",
                "",
                "hover thumbnail preview disabled hint",
            ),
            (
                "WheelVolumeEnabled",
                "",
                "mouse wheel over dock change volume",
            ),
        ],
    );
    // Dock — interactions (row_ids must stay identical to `target_row` ids in `ui/settings.rs`)
    add(
        SettingsPage::Dock,
        "SectionInteractions",
        &[
            ("ShowSearchButton", "", "search button show hide ara gizle"),
            (
                "ShowTaskViewButton",
                "",
                "task view virtual desktop gizle gorev gorunumu",
            ),
            ("TrayApps", "", "system tray icons apps"),
            ("RecycleBin", "", "trash delete files"),
            ("WidgetShowDesktop", "", "show desktop button"),
            (
                "MiddleClickAction",
                "",
                "middle click close new window mouse wheel",
            ),
        ],
    );
    // Dynamic Island
    add(
        SettingsPage::DynamicIsland,
        "DynamicIslandTitle",
        &[
            (
                "DynamicIslandStyleLabel",
                "",
                "dynamic island style notch floating pill",
            ),
            (
                "DynamicIslandLayoutLabel",
                "",
                "dynamic island layout paged strip carousel",
            ),
            (
                "DynamicIslandShowFocusWidgets",
                "",
                "dynamic island focus mode widgets show",
            ),
            (
                "DynamicIslandVisibility",
                "",
                "dynamic island visibility desktop always top above windows full screen",
            ),
            (
                "DynamicIslandHideFullscreen",
                "",
                "dynamic island fullscreen hide app display",
            ),
            (
                "DynamicIslandAutoHide",
                "",
                "dynamic island auto hide top edge hover",
            ),
            (
                "SurfaceOpacityLabel",
                "DynamicIslandOpacity",
                "dynamic island transparency opacity",
            ),
            (
                "DynamicIslandIdleMode",
                "",
                "dynamic island idle resting mode full minimal closed hidden",
            ),
            (
                "DynamicIslandClosedDisplay",
                "",
                "dynamic island closed minimal display line clock music ai usage off",
            ),
            (
                "DynamicIslandHoverExpand",
                "",
                "dynamic island hover expand mouse open",
            ),
            (
                "DynamicIslandAutoClose",
                "",
                "dynamic island auto collapse close delay seconds timeout",
            ),
            (
                "DynamicIslandFollowMouse",
                "",
                "dynamic island follow active mouse cursor display monitor",
            ),
            (
                "DynamicIslandScale",
                "",
                "dynamic island scale size zoom percent",
            ),
            (
                "PrivacyIndicatorsEnabled",
                "",
                "privacy indicators camera microphone dots indicator green orange",
            ),
            (
                "DynamicIslandWidgetLocation",
                "",
                "dynamic island widget location display dock",
            ),
        ],
    );
    add(
        SettingsPage::DynamicIsland,
        "SectionIslandHuds",
        &[
            (
                "HudVolumeEnabled",
                "",
                "hud volume audio sound pill dynamic island",
            ),
            (
                "HudBrightnessEnabled",
                "",
                "hud brightness screen backlight display dynamic island",
            ),
            (
                "HudCapsLockEnabled",
                "",
                "hud caps lock num lock keyboard indicator dynamic island",
            ),
            (
                "HudKeyboardLangEnabled",
                "",
                "hud keyboard layout language input switcher dynamic island",
            ),
            (
                "HudPowerEnabled",
                "",
                "hud power charging battery plugged unplugged dynamic island",
            ),
            (
                "HudNetworkEnabled",
                "",
                "hud network wifi ethernet connection status dynamic island",
            ),
            (
                "HudBluetoothEnabled",
                "",
                "hud bluetooth device connected wireless dynamic island",
            ),
            (
                "HudDurationMs",
                "",
                "hud display duration timeout milliseconds dynamic island",
            ),
            (
                "IslandNotificationsEnabled",
                "",
                "notifications mirror toast alert windows dynamic island",
            ),
            (
                "IslandNotificationsDuration",
                "",
                "notification duration timeout milliseconds dynamic island",
            ),
            (
                "IslandNotificationsRespectFocus",
                "",
                "notifications pause focus mode quiet dynamic island",
            ),
        ],
    );
    add(
        SettingsPage::DynamicIsland,
        "WidgetAiAgents",
        &[
            (
                "AgentMonitorEnabled",
                "",
                "ai coding agent monitor claude codex cursor pipe dynamic island",
            ),
            (
                "AgentApprovalBanners",
                "",
                "ai agent approval banners allow deny inline card diff",
            ),
        ],
    );
    // Widgets — inline card settings
    const WIDGET_SETTINGS: &[(&str, &str, &str, &str, &str)] = &[
        (
            "clock",
            "ClockFormat24Hour",
            "WidgetClock",
            "",
            "time 24 hour am pm clock",
        ),
        (
            "clock",
            "ClockShowSeconds",
            "WidgetClock",
            "",
            "clock seconds time",
        ),
        (
            "clock",
            "DateStyleDayShort",
            "WidgetClock",
            "",
            "weekday date day",
        ),
        (
            "clock",
            "ClockDateLayout",
            "WidgetClock",
            "",
            "date beside below layout",
        ),
        (
            "clock",
            "ClockDateFormat",
            "WidgetClock",
            "",
            "date format yyyy mm dd",
        ),
        (
            "stats",
            "Cpu",
            "StatsSettingsHeading",
            "",
            "cpu processor meter percentage",
        ),
        (
            "stats",
            "Memory",
            "StatsSettingsHeading",
            "",
            "ram memory meter percentage",
        ),
        (
            "stats",
            "Gpu",
            "StatsSettingsHeading",
            "",
            "gpu graphics meter percentage",
        ),
        (
            "stats",
            "GpuTemperature",
            "StatsSettingsHeading",
            "",
            "gpu temperature thermal degrees celsius fahrenheit sicaklik",
        ),
        (
            "weather",
            "WeatherUnit",
            "WidgetWeather",
            "",
            "weather temperature unit celsius fahrenheit c f",
        ),
        (
            "weather",
            "WeatherCompactMode",
            "WidgetWeather",
            "",
            "weather compact mode chip",
        ),
        (
            "storage",
            "StorageCompactMode",
            "WidgetStorage",
            "",
            "storage compact mode drive disk",
        ),
        (
            "media",
            "WidgetShowInDockMedia",
            "WidgetMedia",
            "ShowMedia",
            "show media music player dock",
        ),
        (
            "media",
            "MediaWaveformEnabled",
            "WidgetMedia",
            "",
            "media live audio waveform visualizer bars music",
        ),
        (
            "media",
            "MediaLyricsEnabled",
            "WidgetMedia",
            "",
            "media synced song lyrics lrclib music",
        ),
        (
            "media",
            "MediaLyricsPillEnabled",
            "WidgetMedia",
            "",
            "media lyric line closed island pill music",
        ),
        (
            "quicksettings",
            "QuickPowerActionsEnabled",
            "WidgetQuickSettings",
            "",
            "quick settings power lock sleep actions",
        ),
        (
            "shelf",
            "ShelfMaxItems",
            "WidgetFileShelf",
            "",
            "file shelf max items drag drop limit",
        ),
        (
            "clipboard",
            "ClipboardMaxItems",
            "WidgetClipboard",
            "",
            "clipboard history max items limit snippets",
        ),
        (
            "clipboard",
            "ClipboardIgnoreSensitive",
            "WidgetClipboard",
            "",
            "clipboard ignore sensitive password manager secret",
        ),
        (
            "todo",
            "TodoRemindersEnabled",
            "WidgetTodo",
            "",
            "todo tasks reminder alerts dynamic island calendar",
        ),
        (
            "todo",
            "TodoIcalUrl",
            "WidgetTodo",
            "",
            "todo calendar ical ics feed url subscription",
        ),
        (
            "todo",
            "TodoIcalPollMinutes",
            "WidgetTodo",
            "",
            "todo calendar ical refresh interval poll minutes",
        ),
        (
            "countdown",
            "CountdownDefaultMinutes",
            "WidgetCountdown",
            "",
            "countdown timer default minutes duration",
        ),
        (
            "bluetoothbattery",
            "WirelessBatteryAlertLabel",
            "WidgetBluetoothBattery",
            "WirelessBatteryLowThreshold",
            "wireless bluetooth battery low alert threshold percent",
        ),
        (
            "bluetoothbattery",
            "RestoreIgnoredDevices",
            "WidgetBluetoothBattery",
            "",
            "wireless bluetooth restore ignored hidden devices",
        ),
        (
            "bluetoothbattery",
            "WirelessMultiBrandEnabled",
            "WidgetBluetoothBattery",
            "",
            "wireless bluetooth multi brand headset mouse protocols hid logitech razer corsair steelseries",
        ),
        (
            "focus",
            "FocusMinutesLabel",
            "WidgetFocus",
            "FocusMinutes",
            "pomodoro focus minutes duration",
        ),
        (
            "focus",
            "FocusBreakMinutesLabel",
            "WidgetFocus",
            "FocusBreakMinutes",
            "pomodoro break minutes rest",
        ),
        (
            "focus",
            "FocusSessionsLabel",
            "WidgetFocus",
            "FocusSessions",
            "pomodoro sessions count rounds",
        ),
        (
            "aiusage_claude",
            "AiUsageRingLabel",
            "WidgetAiUsage",
            "",
            "ai usage ring quota highest session weekly",
        ),
    ];
    add(
        SettingsPage::Widgets,
        "WidgetDensity",
        &[
            (
                "WidgetDensityCompact",
                "",
                "density override per widget shrink priority compact full",
            ),
            (
                "ShrinkPriority",
                "",
                "shrink priority overflow compact bias",
            ),
        ],
    );
    // Themes
    add(
        SettingsPage::Themes,
        "SectionTheme",
        &[
            (
                "ThemePack",
                "",
                "theme pack gallery presets catppuccin gruvbox nord prism solarized tokyo night",
            ),
            (
                "ThemeStudioTitle",
                "ThemeStudio",
                "theme studio custom preset liquid glass acrylic mica oled flat minimal vibrant",
            ),
            ("Theme", "", "dark light system appearance"),
            (
                "ThemeApplicationIcons",
                "",
                "application icons theme icon pack",
            ),
            ("DockBackground", "", "glass solid none background"),
            (
                "SurfaceOpacityLabel",
                "SurfaceOpacityLabel:Dock",
                "dock transparency opacity",
            ),
            ("DockCornerRadius", "", "round corners radius visual"),
            (
                "ThemeAdvancedTokens",
                "ThemeTokens",
                "theme tokens editor advanced color brush",
            ),
            (
                "ThemeBrushPicker",
                "",
                "hsv gradient alpha hex stops angle brush picker",
            ),
        ],
    );
    add(
        SettingsPage::Themes,
        "SectionGlassEffect",
        &[
            (
                "GlassBlurLabel",
                "",
                "glass blur bulaniklik clear crystal frost",
            ),
            (
                "GlassSaturationLabel",
                "",
                "glass vibrancy saturation canlilik doygunluk clear crystal",
            ),
            (
                "GlassEdgeGlowLabel",
                "",
                "glass edge glow light kenar isigi clear crystal",
            ),
            (
                "GlassPointerLightLabel",
                "",
                "glass pointer cursor reflection imlec yansima clear crystal",
            ),
        ],
    );
    add(
        SettingsPage::Themes,
        "Animations",
        &[
            ("Animations", "", "motion effects animation"),
            ("Magnification", "", "fisheye icon magnification magnify"),
            (
                "FisheyeIntensity",
                "",
                "fisheye magnification strength intensity subtle normal strong",
            ),
            (
                "MagnificationExpandsDock",
                "",
                "magnification expands dock grow widen cursor",
            ),
            ("AnimationFps", "", "fps frame rate hz refresh animations"),
        ],
    );
    add(
        SettingsPage::Themes,
        "SectionSurfaces",
        &[
            ("SurfaceDock", "", "dock acrylic material surface"),
            ("SurfaceMenu", "", "menu acrylic material surface"),
            ("SurfaceWidget", "", "widget acrylic material surface"),
            ("SurfaceSettings", "", "settings acrylic material surface"),
            (
                "SurfaceOpacityLabel",
                "SurfaceOpacityLabel:Surfaces",
                "surface transparent opacity",
            ),
        ],
    );
    // Preferences
    add(
        SettingsPage::Preferences,
        "SectionSmartTools",
        &[
            (
                "SmartOcrEnabled",
                "",
                "offline smart ocr extract text screen capture copy",
            ),
            (
                "QuickScreenshotEnabled",
                "",
                "quick region screenshot capture screen png clipboard",
            ),
        ],
    );
    add(
        SettingsPage::Preferences,
        "SectionHotkeys",
        &[
            (
                "HotkeyIslandToggle",
                "",
                "hotkey shortcut dynamic island toggle show hide",
            ),
            (
                "HotkeySmartOcr",
                "",
                "hotkey shortcut smart ocr capture text",
            ),
            (
                "HotkeyQuickScreenshot",
                "",
                "hotkey shortcut quick region screenshot capture",
            ),
            (
                "HotkeyClipboardHistory",
                "",
                "hotkey shortcut clipboard history open",
            ),
        ],
    );
    add(
        SettingsPage::Preferences,
        "SectionSystem",
        &[
            (
                "WindowPickerTitle",
                "",
                "window picker switch previous next page",
            ),
            ("Startup", "", "run at startup boot launch"),
        ],
    );
    add(
        SettingsPage::Preferences,
        "Preferences",
        &[
            ("Language", "", "locale culture language translation"),
            ("Profiles", "", "profile dock layout preset"),
        ],
    );
    add(
        SettingsPage::Preferences,
        "CheckForUpdates",
        &[("CheckUpdatesSetting", "", "update check automatic version")],
    );
    add(
        SettingsPage::Preferences,
        "Backup",
        &[
            ("SettingsBackup", "", "export import backup restore json"),
            ("ResetDefaults", "", "reset restore defaults factory"),
        ],
    );
    // About
    add(
        SettingsPage::About,
        "About",
        &[("Version", "", "build version")],
    );
    add(
        SettingsPage::About,
        "Diagnostics",
        &[
            ("DataFolder", "", "data folder path files"),
            ("OpenLogFile", "", "log file diagnostics open"),
            ("WindowsTaskbar", "", "restore windows original taskbar"),
        ],
    );

    for &(widget_id, title_key, section_key, row_id, keywords) in WIDGET_SETTINGS {
        let id = if row_id.is_empty() { title_key } else { row_id };
        entries.push(SettingsSearchEntry::widget_setting(
            widget_id,
            title_key,
            section_key,
            keywords,
            id,
        ));
    }

    for widget in widgets::catalog_widgets() {
        entries.push(SettingsSearchEntry {
            page: SettingsPage::Widgets,
            title: widgets::widget_name(widget.id),
            section: super::settings::category_label(widget.category),
            keywords: widget.id,
            widget_filter: Some(widget.category),
            expand_widget: None,
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

    let mut haystack = String::with_capacity(128);
    let mut results = Vec::new();
    for entry in entries {
        haystack.clear();
        normalize_into(&entry.title, &mut haystack);
        haystack.push(' ');
        normalize_into(&entry.section, &mut haystack);
        haystack.push(' ');
        normalize_into(&entry.page.title(), &mut haystack);
        haystack.push(' ');
        normalize_into(entry.keywords, &mut haystack);
        if terms.iter().all(|term| haystack.contains(term.as_str())) {
            results.push(entry);
            if results.len() >= 80 {
                break;
            }
        }
    }
    results
}

fn normalize_into(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            'ı' | 'İ' | 'I' => out.push('i'),
            '\u{0307}' => {}
            c if c.is_ascii() => out.push(c.to_ascii_lowercase()),
            c => {
                for lc in c.to_lowercase() {
                    if lc == 'ı' {
                        out.push('i');
                    } else if lc != '\u{0307}' {
                        out.push(lc);
                    }
                }
            }
        }
    }
}

fn normalize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    normalize_into(value, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit_ids<'a>(entries: &[&'a SettingsSearchEntry]) -> Vec<&'a str> {
        entries.iter().map(|entry| entry.row_id.as_str()).collect()
    }

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
        assert!(
            catalog
                .iter()
                .any(|entry| entry.widget_filter.is_some() && entry.row_id.starts_with("widget-"))
        );
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
        assert_eq!(
            SettingsSearchEntry::new(SettingsPage::Dock, "DockAlignment", "Layout", "alignment")
                .row_id,
            "DockAlignment"
        );
    }

    #[test]
    fn new_start_placement_and_brush_picker_rows_are_searchable() {
        let catalog = catalog();
        for row_id in ["StartPlacement", "StartWidgetIndex", "ThemeBrushPicker"] {
            assert!(
                catalog.iter().any(|entry| entry.row_id == row_id),
                "missing search entry {row_id}"
            );
        }
        let hits = search(&catalog, "start placement");
        assert!(hits.iter().any(|entry| entry.row_id == "StartPlacement"));
        let hits = search(&catalog, "gradient stops alpha");
        assert!(hits.iter().any(|entry| entry.row_id == "ThemeBrushPicker"));
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

    #[test]
    fn new_runtime_rows_are_searchable() {
        let catalog = super::catalog();
        for row_id in [
            "ContentAlignment",
            "DockWidthMode",
            "DockHeightMode",
            "DockTopGap",
            "PreviewsEnabled",
            "WheelVolumeEnabled",
            "MagnificationExpandsDock",
            "FocusMinutes",
            "FocusBreakMinutes",
            "FocusSessions",
            "AiUsageRingLabel",
            "WindowPickerTitle",
            "ShowSearchButton",
            "ShowTaskViewButton",
        ] {
            assert!(
                catalog.iter().any(|entry| entry.row_id == row_id),
                "missing search entry {row_id}"
            );
        }
        let hits = search(&catalog, "ara");
        assert!(
            hits.iter().any(|entry| entry.row_id == "ShowSearchButton"),
            "'ara' must find the search-button hide setting: {:?}",
            hit_ids(&hits)
        );
        let hits = search(&catalog, "gizle");
        assert!(
            hits.iter().any(|entry| entry.row_id == "ShowSearchButton")
                && hits
                    .iter()
                    .any(|entry| entry.row_id == "ShowTaskViewButton"),
            "'gizle' must find both hide settings: {:?}",
            hit_ids(&hits)
        );
        let hits = search(&catalog, "task view");
        assert!(
            hits.iter()
                .any(|entry| entry.row_id == "ShowTaskViewButton"),
            "'task view' must find the task-view-button hide setting: {:?}",
            hit_ids(&hits)
        );
        let needle = concat!("antig", "ravity");
        assert!(
            search(&catalog, needle).is_empty(),
            "the {needle} alias must not surface in search results"
        );
    }
}

// --- Dynamic search catalogue (reference SettingsWindow.Search.cs parity) ---

use opentaskbar_core::settings::DockSettings;

/// One searchable Settings row. `field` is the WS-A JSON/PascalCase setting name (may be empty
/// for non-setting actions), `keywords` holds aliases used by the reference UI and localizations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchEntry {
    pub page: &'static str,
    pub section: &'static str,
    pub label: String,
    pub field: &'static str,
    pub keywords: &'static [&'static str],
    /// Widget panel id (WidgetPanelHost target) when this is a widget entry.
    pub widget_id: Option<&'static str>,
    /// `DockWidgetOption.HasSettings` counterpart.
    pub has_panel: bool,
}

type RowDef = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
);

macro_rules! settings_rows {
    ($(($page:expr, $section:expr) => [$(($label:expr, $field:expr, $kw:expr)),* $(,)?]),* $(,)?) => {
        /// Rows mirrored from `ui/settings.rs`; keep in sync when adding a setting row.
        pub const SETTINGS_ROWS: &[RowDef] = &[
            $($(($page, $section, $label, $field, $kw),)*)*
        ];
    };
}

settings_rows! {
    ("Dock", "Layout & Dimensions") => [
        ("Alignment", "Position", &["dock", "left", "center", "right"]),
        ("Dock Edge", "Edge", &["vertical", "left", "right", "side"]),
        ("Dock Size", "Size", &["compact", "medium", "large"]),
        ("Scale", "DockScale", &["zoom", "icon size"]),
        ("Bottom Margin", "DockBottomGapPixels", &["gap", "margin", "bottom"]),
        ("Top Gap", "DockTopGapPixels", &["top gap", "DockTopGapUnit", "DockTopGapPercent"]),
        ("Icon Padding", "DockIconPadding", &["spacing"]),
        ("Full Width", "Layout", &["FullWidth", "stretch"]),
        ("Width Mode", "DockWidthMode", &["DockWidthPixels", "DockWidthPercent", "fixed", "percent"]),
        ("Height Mode", "DockHeightMode", &["DockHeightPixels", "DockHeightPercent", "side dock"]),
        ("Content Alignment", "ContentAlignment", &["ContentAlignStart", "ContentAlignCenter", "ContentAlignEnd"]),
        ("Reset Width", "DockCustomWidthDips", &["auto", "width"]),
    ],
    ("Dock", "Behavior & Displays") => [
        ("Auto-Hide", "AutoHideMode", &["hide", "smart", "always"]),
        ("All Monitors", "ShowOnAllMonitors", &["display", "multi monitor"]),
        ("Target Display", "TargetDisplay", &["monitor", "screen"]),
        ("Focus Mode", "FocusMode", &["distraction"]),
        ("Reveal on Win Key", "RevealDockOnWinKey", &["winkey", "windows key"]),
        ("Current Desktop Only", "CurrentDesktopOnly", &["virtual desktop"]),
        ("Wheel Volume", "WheelVolumeEnabled", &["mouse wheel", "audio"]),
        ("Previews Enabled", "PreviewsEnabled", &["PreviewDisabledHint", "hover", "thumbnail"]),
    ],
    ("Themes", "Motion") => [
        ("Magnification Expands Dock", "MagnificationExpandsDock", &["MagnificationExpandsDockDescription", "fisheye", "grow"]),
    ],
    ("Dock", "SearchSectionDockUtilities") => [
        ("Background Apps", "ShowTrayButton", &["tray", "system tray"]),
        ("Recycle Bin", "ShowRecycleBin", &["bin", "trash"]),
        ("Show Desktop Strip", "ShowDesktopButton", &["show desktop"]),
        ("Search Button", "ShowSearchButton", &["search"]),
        ("Task View Button", "ShowTaskViewButton", &["task view"]),
        ("App Groups", "AppGroups", &["group"]),
    ],
    ("Themes", "SearchSectionThemeSurfaces") => [
        ("Theme", "Theme", &["dark", "light", "system"]),
        ("Theme Pack", "ThemePack", &["gallery", "swatch", "bundled"]),
        ("Application Icons", "ThemeApplicationIcons", &["icon pack"]),
        ("Background", "DockBackground", &["glass", "solid", "acrylic"]),
        ("Opacity", "DockOpacity", &["transparency"]),
        ("Corner Radius", "DockCornerRadius", &["round"]),
        ("Theme Editor", "ThemeTokens", &["token", "brush", "color", "editor"]),
        ("Theme Brush Picker", "ThemeBrushPicker", &["hsv", "gradient", "alpha", "hex", "stops", "angle"]),
    ],
    ("Themes", "Motion") => [
        ("Smooth Animations", "AnimationsEnabled", &["animation"]),
        ("Magnification", "MagnificationEnabled", &["fisheye", "magnify"]),
    ],
    ("Widgets", "Widgets") => [
        ("Clock & Date", "ShowClockWidget", &["time", "calendar"]),
        ("System Stats", "ShowSystemStatsWidget", &["cpu", "ram", "gpu"]),
        ("Enabled Widgets", "EnabledWidgets", &["catalog", "density"]),
        ("Widget Order", "WidgetOrder", &["reorder"]),
        ("Weather City", "WeatherCity", &["country", "province", "location"]),
        ("Weather Unit", "WeatherUnit", &["celsius", "fahrenheit"]),
        ("Focus Minutes", "FocusMinutes", &["pomodoro"]),
        ("Focus Break Minutes", "FocusBreakMinutes", &["break"]),
        ("Focus Sessions", "FocusSessions", &["session"]),
        ("Wireless Battery Poll", "WirelessBatteryPollSeconds", &["bluetooth", "interval"]),
        ("Wireless Battery Threshold", "WirelessBatteryLowThreshold", &["low", "percent"]),
        ("Extended Wireless Protocols", "WirelessMultiBrandEnabled", &["wireless", "bluetooth", "headset", "mouse", "hid"]),
        ("AI Usage Ring", "AiUsageRing", &["quota", "providers", "AiUsageRingLabel"]),
        ("Show Media", "ShowMedia", &["music", "player"]),
        ("Live Audio Waveform", "MediaWaveformEnabled", &["media", "waveform", "visualizer", "music"]),
        ("Synced Song Lyrics", "MediaLyricsEnabled", &["media", "lyrics", "lrclib", "music"]),
        ("Lyrics Pill in Closed Island", "MediaLyricsPillEnabled", &["media", "lyrics", "island", "pill"]),
        ("Quick Power Actions", "QuickPowerActionsEnabled", &["quicksettings", "lock", "sleep", "power"]),
        ("Maximum Shelf Items", "ShelfMaxItems", &["shelf", "files", "drag", "drop"]),
        ("Clipboard History Limit", "ClipboardMaxItems", &["clipboard", "history", "limit"]),
        ("Ignore Sensitive Clipboard", "ClipboardIgnoreSensitive", &["clipboard", "password", "sensitive"]),
        ("Task Reminder Alerts", "TodoRemindersEnabled", &["todo", "tasks", "reminders"]),
        ("Calendar iCal URL", "TodoIcalUrl", &["todo", "calendar", "ical", "ics"]),
        ("Calendar Refresh Interval", "TodoIcalPollMinutes", &["todo", "calendar", "ical", "poll"]),
    ],
    ("DynamicIsland", "Dynamic Island") => [
        ("Resting Island Mode", "DynamicIslandIdleMode", &["island", "idle", "minimal", "closed", "full"]),
        ("Minimal Mode Content", "DynamicIslandClosedDisplay", &["island", "closed", "line", "clock", "music"]),
        ("Expand on Hover", "DynamicIslandHoverExpand", &["island", "hover", "expand"]),
        ("Auto-Collapse Delay", "DynamicIslandAutoCloseSecs", &["island", "auto close", "collapse", "delay"]),
        ("Follow Active Mouse Display", "DynamicIslandFollowMouse", &["island", "mouse", "monitor", "display"]),
        ("Island Scale", "DynamicIslandScale", &["island", "scale", "zoom"]),
        ("Privacy Indicators", "PrivacyIndicatorsEnabled", &["camera", "microphone", "privacy", "dots"]),
    ],
    ("DynamicIsland", "System HUDs & Notifications") => [
        ("Volume HUD", "HudVolumeEnabled", &["hud", "volume", "audio"]),
        ("Brightness HUD", "HudBrightnessEnabled", &["hud", "brightness", "screen"]),
        ("Caps Lock & Num Lock HUD", "HudCapsLockEnabled", &["hud", "caps lock", "num lock"]),
        ("Keyboard Language HUD", "HudKeyboardLangEnabled", &["hud", "keyboard", "language"]),
        ("Charging & Battery HUD", "HudPowerEnabled", &["hud", "power", "battery", "charging"]),
        ("Wi-Fi Status HUD", "HudNetworkEnabled", &["hud", "wifi", "network"]),
        ("Bluetooth Connected HUD", "HudBluetoothEnabled", &["hud", "bluetooth", "device"]),
        ("HUD Display Duration", "HudDurationMs", &["hud", "duration", "timeout"]),
        ("Mirror Notifications", "IslandNotificationsEnabled", &["notifications", "toast", "island"]),
        ("Notification Duration", "IslandNotificationsDurationMs", &["notifications", "duration", "timeout"]),
        ("Pause Notifications in Focus Mode", "IslandNotificationsRespectFocus", &["notifications", "focus", "pause"]),
    ],
    ("DynamicIsland", "AI Agent Monitor") => [
        ("Enable Coding Agent Monitor", "AgentMonitorEnabled", &["agent", "claude", "codex", "cursor", "monitor"]),
        ("Show Inline Approval Banners", "AgentApprovalBanners", &["agent", "approval", "banner", "diff"]),
    ],
    ("Widgets", "Widget Density") => [
        ("Density Override", "WidgetDensityOverride", &["WidgetDensityCompact", "WidgetDensityFull", "per widget"]),
        ("Shrink Priority", "ShrinkPriority", &["compact", "overflow"]),
    ],
    ("Preferences", "System") => [
        ("Window Picker", "WindowPickerEnabled", &["WindowPickerTitle", "WindowPickerPrevious", "WindowPickerNext"]),
        ("Start with Windows", "Startup", &["startup", "run", "registry"]),
    ],
    ("Preferences", "Smart Capture & OCR") => [
        ("Offline Smart OCR", "SmartOcrEnabled", &["ocr", "extract text", "capture", "smart tools"]),
        ("Quick Region Screenshot", "QuickScreenshotEnabled", &["screenshot", "region", "capture", "png"]),
    ],
    ("Preferences", "Hotkeys") => [
        ("Launcher Hotkey", "LauncherHotkey", &["capture", "shortcut", "key"]),
        ("Dock Hotkey", "DockHotkey", &["capture", "shortcut", "key"]),
        ("Toggle Dynamic Island Hotkey", "IslandToggleHotkey", &["capture", "shortcut", "island", "toggle"]),
        ("Smart OCR Hotkey", "OcrHotkey", &["capture", "shortcut", "ocr"]),
        ("Quick Screenshot Hotkey", "ScreenshotHotkey", &["capture", "shortcut", "screenshot"]),
        ("Clipboard History Hotkey", "ClipboardHotkey", &["capture", "shortcut", "clipboard"]),
    ],
    ("Preferences", "Accounts") => [
        ("Cursor Sign In", "CursorAuth", &["oauth", "pkce", "ai", "account"]),
    ],
    ("Preferences", "Backup") => [
        ("SettingsBackup", "SettingsBackup", &["json", "export", "import"]),
        ("Full Backup (.otbackup)", "FullBackup", &["zip", "pins", "theme", "archive"]),
        ("Factory Defaults", "Reset", &["reset", "defaults"]),
    ],
    ("About", "Application") => [
        ("Version", "Version", &["build"]),
    ],
    ("About", "Diagnostics & Recovery") => [
        ("Data Folder", "DataFolder", &["localappdata", "path"]),
        ("Runtime Log", "LogFile", &["diagnostics"]),
        ("Windows Taskbar", "RestoreTaskbar", &["restore", "recovery"]),
    ],
    ("About", "Links") => [
        ("GitHub", "GitHub", &["source", "repository", "issues"]),
    ],
}

/// `DockWidgetOption.HasSettings` counterpart. Delegates to the registry's
/// `WidgetOption` so search results stay in sync with the authoritative
/// descriptor table (including aliases and custom widgets).
pub fn widget_has_settings(id: &str) -> bool {
    widgets::WidgetOption::for_id(id).is_some_and(|option| option.has_settings)
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

        let widgets: Vec<&'static widgets::WidgetDescriptor> = widgets::catalog_widgets()
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

    /// Adds user-defined rows (profiles) so searches follow the live config.
    pub fn from_settings(settings: &DockSettings) -> Self {
        let mut catalog = Self::new();
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
    use super::{SearchCatalog, widget_has_settings, widgets};

    #[test]
    fn catalog_is_built_from_the_live_widget_catalog() {
        let catalog = SearchCatalog::new();
        let ids: Vec<_> = catalog
            .entries()
            .iter()
            .filter_map(|e| e.widget_id)
            .collect();
        assert!(ids.contains(&"weather"));
        // The AI-usage panel group stays in sync with the live widget list.
        assert!(ids.iter().any(|id| id.starts_with("aiusage_")));
        assert!(ids.contains(&"stacks"));
        assert!(ids.len() >= widgets_len());
    }

    fn widgets_len() -> usize {
        crate::widgets::catalog_widgets().count()
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
        // The predicate must match the registry, not a second hand-written list.
        for widget in crate::widgets::catalog_widgets() {
            assert_eq!(
                widget_has_settings(widget.id),
                widgets::WidgetOption::for_id(widget.id).is_some_and(|option| option.has_settings),
                "settings flag diverged for {}",
                widget.id
            );
        }
        assert!(widget_has_settings("weather"));
        assert!(!widget_has_settings("dayprogress"));
    }
}
