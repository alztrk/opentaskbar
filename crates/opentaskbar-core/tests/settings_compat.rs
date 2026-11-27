//! `settings.json` compatibility checks against the public core API.

use std::fs;
use std::path::PathBuf;

use opentaskbar_core::hotkey::HotkeyChoice;
use opentaskbar_core::paths;
use opentaskbar_core::settings::{
    AppTheme, AutoHideMode, DockGapUnit, DockHeightMode, DockPosition, DockProfile, DockSettings,
    DockSize, DockWidthMode, SurfaceBackground, WidgetDensityMode, apply_full_profile,
    canonical_widget_setting_id, dock_metrics, effective_bottom_gap, effective_bottom_gap_for,
    effective_height_for, effective_top_gap, effective_top_gap_for, effective_width,
    is_supported_widget_id, load_from, normalize_legacy_json, resolve_widget_density, save_to,
    widget_density_for, widget_shrink_priority,
};

fn temp_settings(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ot-cfg-compat-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    (dir.clone(), dir.join("settings.json"))
}

#[test]
fn reference_settings_file_survives_load_save_load() {
    let (dir, path) = temp_settings("roundtrip");
    let raw = r#"{
  "Position": "Center",
  "Edge": "Bottom",
  "Theme": "Dark",
  "Language": "tr-TR",
  "AnimationsEnabled": true,
  "PreviewsEnabled": true,
  "NotificationBadgesEnabled": true,
  "SurfaceOpacity": 0.9,
  "DockBackground": "Glass",
  "DockOpacity": 0.9,
  "DockBorderThickness": 1.0,
  "MenuBackground": "Solid",
  "MenuOpacity": 0.85,
  "WidgetOpacity": 0.8,
  "SettingsOpacity": 0.95,
  "Widgets": ["Clock", "Media"],
  "InstalledWidgets": ["Clock", "Media", "Focus"],
  "Layout": "FullWidth",
  "ContentAlignment": "Right",
  "Size": "Compact",
  "MagnificationEnabled": true,
  "ShowClock": true,
  "ShowMedia": true,
  "ShowSystemStats": false,
  "ShowFocus": false,
  "ShowAudio": false,
  "ShowQuickSettings": false,
  "FocusMinutes": 25,
  "FocusBreakMinutes": 5,
  "FocusSessions": 4,
  "ShowRecycleBin": true,
  "LauncherHotkey": "AltSpace",
  "DockHotkey": "CtrlAltD",
  "StartAction": "WindowsStart",
  "StartPlacement": "Start",
  "StartWidgetIndex": 0,
  "WheelVolumeEnabled": false,
  "ShowWeather": true,
  "WeatherCity": "Ankara",
  "CurrentDesktopOnly": false,
  "ShowOnAllMonitors": true,
  "TargetDisplay": "DISPLAY2",
  "SecondaryApps": "AllMonitors",
  "SecondaryWidgets": true,
  "StackFolders": ["C:\\Downloads"],
  "Profiles": [{
    "Name": "Work",
    "HiddenApps": ["old.exe"],
    "Widgets": ["Clock"],
    "InstalledWidgets": ["Clock", "Media"],
    "WidgetOrder": ["Clock", "Media"],
    "DockItemOrder": ["start", "widget:Clock"],
    "DockWidthMode": "Percent",
    "DockWidthPixels": 800,
    "DockWidthPercent": 50,
    "Theme": "Light",
    "ThemePack": "nord",
    "ThemeApplicationIcons": false,
    "LauncherHotkey": "CtrlSpace",
    "DockHotkey": "CtrlAltD",
    "AutoHide": "Smart",
    "CustomLauncherHotkey": "Ctrl+Alt+L",
    "CustomDockHotkey": "Ctrl+Alt+K",
    "ProfileSpecificFutureKey": 42
  }],
  "ActiveProfile": "Work",
  "DockScale": 1.0,
  "WidgetOrder": ["Clock", "Media"],
  "Clock24Hour": true,
  "ClockShowSeconds": false,
  "DateStyle": "DayAndShortDate",
  "StatsMeters": "Cpu, Memory, Gpu, GpuTemperature",
  "ThemePack": null,
  "ThemeApplicationIcons": true,
  "WidgetDensityOverrides": {"Clock": "Compact"},
  "DockItemOrder": ["start", "search", "widget:Clock"],
  "DockCornerRadius": null,
  "AiUsageProviders": "None",
  "AiUsageRing": "Highest",
  "MagnificationExpandsDock": false,
  "AppGroups": null,
  "ShowSearchButton": true,
  "ShowTaskViewButton": true,
  "ShowTrayButton": true,
  "ShowDesktopButton": true,
  "DockWidthMode": "Auto",
  "DockWidthPixels": 900,
  "DockWidthPercent": 60,
  "DockTopGapUnit": "Pixels",
  "DockTopGapPixels": 0,
  "DockTopGapPercent": 0,
  "DockBottomGapUnit": "Pixels",
  "DockBottomGapPixels": 6,
  "DockBottomGapPercent": 0,
  "DockIconPadding": 0,
  "FocusModeEnabled": false,
  "CustomLauncherHotkey": null,
  "CustomDockHotkey": null,
  "PinnedTrayIcons": ["guid:{12345678-1234-1234-1234-123456789ABC}"],
  "DockHeightMode": "Auto",
  "DockHeightPixels": 48,
  "DockHeightPercent": 7,
  "WirelessBatteryPollSeconds": 30,
  "WirelessBatteryLowThreshold": 0,
  "IgnoredWirelessDevices": null,
  "WirelessDeviceAliases": {"mouse": "Mouse"},
  "WirelessBatteryListenOnly": false,
  "WeatherUnit": "Celsius",
  "DateFormat": "System",
  "DynamicIslandPlacements": {"Clock": "*"},
  "RevealDockOnWinKey": true,
  "ClockDateLayout": "Beside",
  "MirrorMainDock": false,
  "DynamicIslandVisibility": "Always",
  "DynamicIslandHideInFullscreen": true,
  "DynamicIslandWidgetOrders": null,
  "FutureReferenceOnlySetting": {"enabled": true}
}
"#;
    fs::write(&path, raw).unwrap();

    let first = load_from(&path);
    assert_eq!(first.theme, AppTheme::Dark);
    assert!(first.full_width);
    assert_eq!(first.enabled_widgets, ["Clock", "Media"]);
    assert_eq!(first.installed_widgets, ["Clock", "Media", "Focus"]);
    assert_eq!(first.content_alignment as i32, 2); // Right
    assert_eq!(first.launcher_hotkey, HotkeyChoice::AltSpace);
    assert_eq!(first.dock_width_mode, DockWidthMode::Auto);
    assert!(!first.magnification_expands_dock);
    assert_eq!(
        first.widget_density_overrides.get("Clock"),
        Some(&WidgetDensityMode::Compact)
    );
    assert_eq!(
        first
            .wireless_device_aliases
            .get("mouse")
            .map(String::as_str),
        Some("Mouse")
    );
    assert_eq!(first.weather_city, "Ankara");
    assert_eq!(first.user_pinned_background_apps.len(), 1);
    assert_eq!(first.profiles[0].name, "Work");
    assert_eq!(
        first.profiles[0].dock_width_mode,
        Some(DockWidthMode::Percent)
    );
    assert_eq!(
        first.profiles[0].launcher_hotkey,
        Some(HotkeyChoice::CtrlSpace)
    );
    assert_eq!(
        first.profiles[0]
            .extra
            .get("ProfileSpecificFutureKey")
            .and_then(|value| value.as_u64()),
        Some(42)
    );
    assert!(first.extra.get("FutureReferenceOnlySetting").is_some());
    assert!(first.validate().is_empty(), "{:?}", first.validate());

    save_to(&path, &first).unwrap();
    let second = load_from(&path);
    assert_eq!(second, first);
    assert!(
        second.extra.get("FutureReferenceOnlySetting").is_some(),
        "unknown reference keys must survive a save/load cycle"
    );
    assert!(
        second.profiles[0]
            .extra
            .get("ProfileSpecificFutureKey")
            .is_some()
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enabling_a_widget_installs_it_and_uses_reference_ids() {
    let mut settings = DockSettings::default();

    // The settings page passes its snake_case catalog IDs; reference validates PascalCase IDs.
    settings.set_widget_enabled("aiusage_claude", true);
    assert!(settings.is_widget_enabled("aiusage_claude"));
    assert!(settings.is_widget_enabled("AiUsageClaude"));
    assert_eq!(settings.enabled_widgets, ["AiUsageClaude"]);
    assert_eq!(settings.installed_widgets, ["AiUsageClaude"]);
    assert!(settings.validate().is_empty(), "{:?}", settings.validate());

    // Repeated toggles must not create case-insensitive duplicates.
    settings.set_widget_enabled("AiUsageClaude", true);
    settings.set_widget_enabled("aiusage_claude", true);
    assert_eq!(settings.enabled_widgets, ["AiUsageClaude"]);
    assert_eq!(settings.installed_widgets, ["AiUsageClaude"]);

    // Turning the switch off deactivates but never uninstalls (reference keeps `InstalledWidgets`).
    settings.set_widget_enabled("aiusage_claude", false);
    assert!(!settings.is_widget_enabled("aiusage_claude"));
    assert!(settings.enabled_widgets.is_empty());
    assert_eq!(settings.installed_widgets, ["AiUsageClaude"]);
    assert!(settings.validate().is_empty(), "{:?}", settings.validate());
}

#[test]
fn clock_and_stats_toggles_keep_special_flags_consistent() {
    let mut settings = DockSettings::default();

    settings.set_widget_enabled("stats", true);
    assert!(settings.show_system_stats_widget);
    assert!(settings.is_widget_enabled("stats"));
    assert_eq!(settings.enabled_widgets, ["SystemStats"]);
    assert_eq!(settings.installed_widgets, ["SystemStats"]);
    settings.set_widget_enabled("stats", false);
    assert!(!settings.show_system_stats_widget);
    assert!(!settings.is_widget_enabled("stats"));
    assert_eq!(settings.installed_widgets, ["SystemStats"]);

    settings.set_widget_enabled("clock", true);
    assert!(settings.show_clock_widget);
    assert!(settings.is_widget_enabled("clock"));
    assert_eq!(settings.enabled_widgets, ["Clock"]);
    assert!(settings.installed_widgets.iter().any(|id| id == "Clock"));
    settings.set_widget_enabled("clock", false);
    assert!(!settings.show_clock_widget);
    assert!(!settings.is_widget_enabled("clock"));
    assert!(
        settings
            .installed_widgets
            .iter()
            .any(|id| id.eq_ignore_ascii_case("Clock"))
    );
    assert!(settings.validate().is_empty(), "{:?}", settings.validate());
}

#[test]
fn legacy_settings_with_missing_installed_widgets_are_repaired_not_reset() {
    let (dir, path) = temp_settings("repair-subset");
    fs::write(
        &path,
        r#"{
          "Theme": "Dark",
          "WeatherCity": "Izmir",
          "Widgets": ["Clock", "Media", "Focus"],
          "InstalledWidgets": ["Clock"]
        }"#,
    )
    .unwrap();

    let loaded = load_from(&path);
    assert_eq!(loaded.theme, AppTheme::Dark);
    assert_eq!(loaded.weather_city, "Izmir");
    assert_eq!(loaded.enabled_widgets, ["Clock", "Media", "Focus"]);
    for id in ["Clock", "Media", "Focus"] {
        assert!(
            loaded
                .installed_widgets
                .iter()
                .any(|installed| installed.eq_ignore_ascii_case(id)),
            "{id} must be derived into InstalledWidgets: {:?}",
            loaded.installed_widgets
        );
    }
    assert!(loaded.validate().is_empty(), "{:?}", loaded.validate());
    assert!(
        !fs::read_dir(&dir).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with("settings.json.invalid-")),
        "a repairable file must not be backed up as invalid"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn legacy_lowercase_catalog_ids_are_canonicalized_on_load() {
    let (dir, path) = temp_settings("repair-ids");
    fs::write(
        &path,
        r#"{
          "Widgets": ["stats", "aiusage_claude", "keyboard"],
          "InstalledWidgets": []
        }"#,
    )
    .unwrap();

    let loaded = load_from(&path);
    assert_eq!(
        loaded.enabled_widgets,
        ["SystemStats", "AiUsageClaude", "KeyboardLayout"]
    );
    assert_eq!(loaded.installed_widgets, loaded.enabled_widgets);
    for id in ["stats", "aiusage_claude", "keyboard"] {
        assert!(loaded.is_widget_enabled(id), "{id} must stay active");
    }
    assert!(loaded.validate().is_empty(), "{:?}", loaded.validate());

    save_to(&path, &loaded).unwrap();
    let reloaded = load_from(&path);
    assert_eq!(reloaded, loaded);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn adding_three_widgets_survives_a_reload() {
    let (dir, path) = temp_settings("three-widgets");
    let mut settings = DockSettings::default();
    for id in ["weather", "aiusage_claude", "media"] {
        settings.set_widget_enabled(id, true);
    }
    assert!(settings.validate().is_empty(), "{:?}", settings.validate());
    save_to(&path, &settings).unwrap();

    let loaded = load_from(&path);
    for id in ["Weather", "AiUsageClaude", "Media"] {
        assert!(
            loaded.is_widget_enabled(id),
            "{id} must be active after reload"
        );
        assert!(
            loaded
                .installed_widgets
                .iter()
                .any(|installed| installed.eq_ignore_ascii_case(id)),
            "{id} must be installed after reload"
        );
    }
    assert_eq!(loaded.enabled_widgets.len(), 3);
    assert_eq!(loaded.installed_widgets.len(), 3);
    assert!(loaded.validate().is_empty(), "{:?}", loaded.validate());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn focus_mode_hides_widgets_like_reference() {
    let mut settings = DockSettings::default();
    settings.set_widget_enabled("weather", true);
    settings.set_widget_enabled("focus", true);

    settings.focus_mode = true;
    assert!(
        !settings.is_widget_enabled("weather"),
        "reference WidgetCoordinator deactivates widgets in Focus Mode"
    );
    assert!(
        settings.is_widget_enabled("focus"),
        "the Focus timer stays visible in this port"
    );

    settings.focus_mode = false;
    assert!(settings.is_widget_enabled("weather"));
    assert!(settings.is_widget_enabled("focus"));
}

#[test]
fn all_new_reference_properties_deserialize() {
    let raw = r#"{
        "PreviewsEnabled": false,
        "NotificationBadgesEnabled": false,
        "MenuOpacity": 0.9,
        "WidgetOpacity": 0.9,
        "SettingsOpacity": 0.9,
        "InstalledWidgets": ["Clock"],
        "ContentAlignment": "Center",
        "ShowMedia": true,
        "ShowFocus": true,
        "ShowAudio": true,
        "ShowQuickSettings": true,
        "FocusMinutes": 30,
        "FocusBreakMinutes": 10,
        "FocusSessions": 2,
        "StartAction": "Launcher",
        "StartPlacement": "InWidgets",
        "StartWidgetIndex": 3,
        "WheelVolumeEnabled": true,
        "ShowWeather": true,
        "TargetDisplay": "DISPLAY1",
        "SecondaryApps": "AllMonitors",
        "SecondaryWidgets": false,
        "WidgetOrder": ["Clock"],
        "ThemeApplicationIcons": false,
        "WidgetDensityOverrides": {"Clock": "Full"},
        "DockItemOrder": ["start"],
        "AiUsageProviders": 251,
        "AiUsageRing": "Session",
        "MagnificationExpandsDock": false,
        "ShowSearchButton": false,
        "ShowTaskViewButton": false,
        "DockWidthMode": "Fill",
        "DockWidthPixels": 1000,
        "DockWidthPercent": 75,
        "DockTopGapUnit": "Percent",
        "DockTopGapPixels": 10,
        "DockTopGapPercent": 5,
        "DockBottomGapUnit": "Percent",
        "DockBottomGapPercent": 3,
        "DockHeightMode": "Fill",
        "DockHeightPixels": 60,
        "DockHeightPercent": 10,
        "WirelessBatteryPollSeconds": 60,
        "WirelessBatteryLowThreshold": 20,
        "IgnoredWirelessDevices": ["mouse"],
        "WirelessDeviceAliases": {"mouse": "Mouse"},
        "WirelessBatteryListenOnly": true,
        "WeatherUnit": "Fahrenheit",
        "MirrorMainDock": true
    }"#;
    let parsed: DockSettings = serde_json::from_str(raw).unwrap();
    assert!(!parsed.previews_enabled);
    assert!(!parsed.notification_badges_enabled);
    assert!(
        parsed.show_media && parsed.show_focus && parsed.show_audio && parsed.show_quick_settings
    );
    assert_eq!(parsed.focus_minutes, 30);
    assert_eq!(parsed.start_widget_index, 3);
    assert!(parsed.wheel_volume_enabled);
    assert_eq!(parsed.secondary_apps as i32, 1);
    assert_eq!(parsed.ai_usage_providers, 251);
    assert!(!parsed.magnification_expands_dock);
    assert_eq!(parsed.dock_width_mode, DockWidthMode::Fill);
    assert_eq!(parsed.dock_height_percent, 10);
    assert_eq!(parsed.wireless_battery_poll_seconds, 60);
    assert!(parsed.wireless_battery_listen_only);
    assert!(parsed.mirror_main_dock);
    assert!(parsed.validate().is_empty(), "{:?}", parsed.validate());
}

#[test]
fn magnification_expands_dock_defaults_to_true_and_round_trips() {
    let (dir, path) = temp_settings("magnification-expands");
    assert!(DockSettings::default().magnification_expands_dock);

    let settings = DockSettings {
        magnification_expands_dock: false,
        ..DockSettings::default()
    };
    save_to(&path, &settings).unwrap();
    let raw = fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"MagnificationExpandsDock\": false"), "{raw}");
    let loaded = load_from(&path);
    assert!(!loaded.magnification_expands_dock);
    assert!(loaded.validate().is_empty(), "{:?}", loaded.validate());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn dock_metrics_and_effective_layout_helpers_match_reference() {
    let compact = dock_metrics::for_size(DockSize::Compact, 1.0, 0);
    let medium = dock_metrics::for_size(DockSize::Medium, 1.0, 0);
    let large = dock_metrics::for_size(DockSize::Large, 1.0, 0);
    assert_eq!((compact.shelf, compact.icon), (48.0, 28.0));
    assert_eq!((medium.shelf, medium.icon), (58.0, 34.0));
    assert_eq!((large.shelf, large.icon), (66.0, 38.0));
    assert_eq!(compact.surface_height(), 44.0);

    let mut settings = DockSettings::default();
    // Auto has no saved content width: the reference minimum surface width is used.
    assert_eq!(
        settings.effective_width(1920.0),
        dock_metrics::MIN_DOCK_SURFACE_WIDTH
    );
    settings.dock_width_mode = DockWidthMode::Pixels;
    settings.dock_width_pixels = 900;
    assert_eq!(settings.effective_width(1920.0), 900.0);
    settings.dock_width_mode = DockWidthMode::Percent;
    settings.dock_width_percent = 60;
    assert_eq!(effective_width(&settings, 1920.0), 1152.0);
    settings.dock_width_mode = DockWidthMode::Fill;
    assert_eq!(settings.effective_width(1366.0), 1366.0);

    settings.dock_width_mode = DockWidthMode::Auto;
    assert_eq!(settings.effective_height(), 44.0);
    settings.dock_height_mode = DockHeightMode::Fill;
    assert_eq!(settings.effective_height(), 44.0);
    settings.dock_height_mode = DockHeightMode::Pixels;
    settings.dock_height_pixels = 80;
    assert_eq!(effective_height_for(&settings, 1080.0), 80.0);
    settings.dock_height_mode = DockHeightMode::Percent;
    settings.dock_height_percent = 5;
    assert_eq!(effective_height_for(&settings, 1920.0), 96.0);

    settings.dock_top_gap_unit = DockGapUnit::Pixels;
    settings.dock_top_gap_pixels = 12;
    assert_eq!(effective_top_gap(&settings), 12);
    settings.dock_top_gap_unit = DockGapUnit::Percent;
    settings.dock_top_gap_percent = 25;
    assert_eq!(effective_top_gap_for(&settings, 1920.0), 480);
    settings.dock_bottom_gap_unit = DockGapUnit::Pixels;
    settings.dock_bottom_gap_pixels = -4;
    assert_eq!(effective_bottom_gap(&settings), 0);
    settings.dock_bottom_gap_unit = DockGapUnit::Percent;
    settings.dock_bottom_gap_percent = 3;
    assert_eq!(effective_bottom_gap_for(&settings, 1920.0), 58);
}

#[test]
fn widget_density_override_and_shrink_priority_are_exposed() {
    let mut settings = DockSettings::default();
    assert_eq!(widget_density_for(&settings, "Clock"), 1.0);
    settings
        .widget_density_overrides
        .insert("clock".to_owned(), WidgetDensityMode::Compact);
    assert_eq!(
        resolve_widget_density(&settings, "CLOCK"),
        WidgetDensityMode::Compact
    );
    assert_eq!(widget_density_for(&settings, "Clock"), 0.82);
    settings.global_widget_density = WidgetDensityMode::Full;
    assert_eq!(widget_density_for(&settings, "Media"), 1.0);

    assert_eq!(widget_shrink_priority("media"), 10);
    assert_eq!(widget_shrink_priority("Weather"), 15);
    assert_eq!(widget_shrink_priority("Clock"), 80);
    assert_eq!(widget_shrink_priority("nothing-special"), 100);
}

#[test]
fn full_profile_applies_all_fifteen_scoped_fields() {
    let mut settings = DockSettings {
        theme_pack: "previous".to_owned(),
        custom_launcher_hotkey: Some("Ctrl+Alt+L".to_owned()),
        ..DockSettings::default()
    };
    let profile = DockProfile {
        name: "Work".to_owned(),
        position: DockPosition::Center,
        size: DockSize::Compact,
        hidden_apps: vec!["old.exe".to_owned()],
        enabled_widgets: vec!["clock".to_owned(), "media".to_owned()],
        installed_widgets: vec!["Clock".to_owned()],
        widget_order: vec!["media".to_owned(), "clock".to_owned()],
        dock_item_order: vec!["start".to_owned(), "widget:Clock".to_owned()],
        dock_width_mode: Some(DockWidthMode::Percent),
        dock_width_pixels: Some(1200),
        dock_width_percent: Some(70),
        theme: Some(AppTheme::Dark),
        theme_pack: Some("nord".to_owned()),
        theme_application_icons: Some(false),
        launcher_hotkey: Some(HotkeyChoice::CtrlSpace),
        dock_hotkey: Some(HotkeyChoice::WinAltD),
        auto_hide: AutoHideMode::Smart,
        custom_launcher_hotkey: Some("Ctrl+Alt+W".to_owned()),
        custom_dock_hotkey: None,
        extra: serde_json::Map::new(),
    };

    apply_full_profile(&mut settings, &profile);

    assert_eq!(settings.active_profile.as_deref(), Some("Work"));
    assert_eq!(settings.enabled_widgets, ["Clock", "Media"]);
    assert_eq!(settings.installed_widgets, ["Clock", "Media"]);
    assert_eq!(settings.widget_order, ["Media", "Clock"]);
    assert_eq!(settings.dock_item_order, ["start", "widget:Clock"]);
    assert_eq!(settings.dock_width_mode, DockWidthMode::Percent);
    assert_eq!(settings.dock_width_pixels, 1200);
    assert_eq!(settings.dock_width_percent, 70);
    assert_eq!(settings.theme, AppTheme::Dark);
    assert_eq!(settings.theme_pack, "nord");
    assert!(!settings.theme_application_icons);
    assert_eq!(settings.launcher_hotkey, HotkeyChoice::CtrlSpace);
    assert_eq!(
        settings.custom_launcher_hotkey.as_deref(),
        Some("Ctrl+Alt+W")
    );
    assert_eq!(settings.dock_hotkey, HotkeyChoice::WinAltD);
    assert_eq!(settings.custom_dock_hotkey, None);
    assert_eq!(settings.auto_hide, AutoHideMode::Smart);
    assert!(settings.validate().is_empty(), "{:?}", settings.validate());
}

#[test]
fn legacy_profile_only_switches_the_active_name() {
    let mut settings = DockSettings {
        theme: AppTheme::Dark,
        auto_hide: AutoHideMode::Always,
        ..DockSettings::default()
    };
    apply_full_profile(
        &mut settings,
        &DockProfile {
            name: "Legacy".to_owned(),
            ..DockProfile::default()
        },
    );
    assert_eq!(settings.active_profile.as_deref(), Some("Legacy"));
    assert_eq!(settings.theme, AppTheme::Dark);
    assert_eq!(settings.auto_hide, AutoHideMode::Always);
}

#[test]
fn old_settings_with_retired_provider_data_are_migrated() {
    // Built from parts so the retired name is not reintroduced into the source tree.
    let retired_name = "Anti".to_owned() + "Gravity";
    let retired_widget = format!("AiUsage{retired_name}");
    let retired_groups_key = format!("{retired_name}Groups");
    let raw = format!(
        r#"{{
            "Widgets": ["Clock", "{retired_widget}"],
            "InstalledWidgets": ["{retired_widget}"],
            "WidgetDensityOverrides": {{"{retired_widget}": "Compact"}},
            "AiUsageProviders": "Claude, {retired_name}",
            "{retired_groups_key}": "All"
        }}"#
    );
    let normalized = normalize_legacy_json(&raw);
    let mut settings: DockSettings = serde_json::from_str(&normalized).unwrap();
    assert_eq!(settings.enabled_widgets, ["Clock"]);
    assert!(settings.installed_widgets.is_empty());
    assert!(settings.widget_density_overrides.is_empty());
    assert_eq!(settings.ai_usage_providers, 1);
    assert!(!is_supported_widget_id(&retired_widget));
    assert!(!settings.is_widget_enabled(&retired_widget));
    settings.migrate_legacy();
    assert!(settings.validate().is_empty(), "{:?}", settings.validate());

    // A numeric flag with the retired bit set must survive normalization as well.
    let numeric = normalize_legacy_json(r#"{"AiUsageProviders":255}"#);
    let numeric: DockSettings = serde_json::from_str(&numeric).unwrap();
    assert_eq!(numeric.ai_usage_providers & 4, 0);
    assert!(numeric.validate().is_empty(), "{:?}", numeric.validate());
}

#[test]
fn struct_migration_drops_retired_provider_data() {
    let retired_widget = format!("AiUsage{}", "Anti".to_owned() + "Gravity");
    let raw = format!(r#"{{"Widgets":["Clock","{retired_widget}"],"AiUsageProviders":255}}"#);
    let mut settings: DockSettings = serde_json::from_str(&raw).unwrap();
    settings.migrate_legacy();
    assert_eq!(settings.enabled_widgets, ["Clock"]);
    assert_eq!(settings.installed_widgets, ["Clock"]);
    assert_eq!(settings.ai_usage_providers & 4, 0);
    assert!(settings.validate().is_empty(), "{:?}", settings.validate());
}

#[test]
fn shared_paths_point_at_the_reference_folder() {
    assert_eq!(paths::APP_DATA_FOLDER_NAME, "OpenTaskbar");
    assert_eq!(paths::settings_path().file_name().unwrap(), "settings.json");
    assert_eq!(
        paths::rust_log_path().file_name().unwrap(),
        "opentaskbar.log"
    );
    assert_eq!(paths::theme_root().file_name().unwrap(), "Themes");
    assert_eq!(paths::secrets_dir().file_name().unwrap(), "secrets");
    assert_eq!(
        paths::watchdog_path().file_name().unwrap(),
        "OpenTaskbarWatchdog.exe"
    );
    assert_eq!(
        paths::render_mode_path().file_name().unwrap(),
        "render-mode.json"
    );
    assert_eq!(
        paths::journal_path("taskbar-state.txt")
            .file_name()
            .unwrap(),
        "taskbar-state.txt"
    );
    for path in [
        paths::settings_path(),
        paths::rust_log_path(),
        paths::theme_root(),
        paths::journal_path("flyout-positions.txt"),
    ] {
        assert!(
            path.to_string_lossy().contains("OpenTaskbar"),
            "{} must use the shared folder",
            path.display()
        );
    }
}

#[test]
fn reference_save_preserves_rust_only_keys_across_a_rust_round_trip() {
    // Shape of a settings.json after `[JsonExtensionData]` has written it back.
    let (dir, path) = temp_settings("reference-extension-data");
    let raw = r#"{
        "Widgets": ["Clock", "Media"],
        "InstalledWidgets": ["Clock", "Media"],
        "EnabledWidgets": ["Clock", "Media"],
        "ThemeTokens": {"shell": {"accent": "aurora"}},
        "AnimationFps": 45,
        "UiFontFamily": "Segoe UI",
        "FisheyeIntensity": "Strong",
        "MiddleClickAction": "CloseWindow",
        "UserPinnedApps": ["C:\\Apps\\a.exe"],
        "ShowStartButton": true,
        "DynamicIslandEnable": true,
        "DynamicIslandEnabled": true,
        "FutureReferenceSwitch": {"on": true}
    }"#;
    fs::write(&path, raw).unwrap();

    let loaded = load_from(&path);
    assert_eq!(loaded.animation_fps, Some(45));
    assert_eq!(loaded.ui_font_family, "Segoe UI");
    assert!(loaded.extra.get("ThemeTokens").is_some());
    assert!(loaded.extra.get("FutureReferenceSwitch").is_some());

    save_to(&path, &loaded).unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for key in [
        "ThemeTokens",
        "AnimationFps",
        "UiFontFamily",
        "FisheyeIntensity",
        "MiddleClickAction",
        "UserPinnedApps",
        "ShowStartButton",
        "DynamicIslandEnable",
        "DynamicIslandEnabled",
        "FutureReferenceSwitch",
    ] {
        assert!(
            written.get(key).is_some(),
            "Rust dropped reference key {key}"
        );
    }
    let reloaded = load_from(&path);
    assert_eq!(reloaded, loaded);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rust_save_emits_the_keys_the_reference_loader_needs() {
    let (dir, path) = temp_settings("rust-to-reference");
    let mut settings = DockSettings::default();
    settings.set_widget_enabled("stats", true);
    settings.full_width = true;
    settings.focus_mode = true;
    settings.dynamic_island_enable = false;
    settings
        .dynamic_island_placements
        .insert("clock".to_owned(), "*".to_owned());
    save_to(&path, &settings).unwrap();

    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(root["Widgets"], root["EnabledWidgets"]);
    assert_eq!(root["ShowClock"], root["ShowClockWidget"]);
    assert_eq!(root["ShowSystemStats"], root["ShowSystemStatsWidget"]);
    assert_eq!(root["FocusModeEnabled"], root["FocusMode"]);
    assert_eq!(root["PinnedTrayIcons"], root["UserPinnedBackgroundApps"]);
    assert_eq!(root["Layout"], "FullWidth");
    assert_eq!(root["DynamicIslandEnabled"], false);
    assert_eq!(root["DynamicIslandEnable"], false);
    assert!(root["DynamicIslandPlacements"].get("Clock").is_some());

    // The reference loader repairs and validates the same object without resetting it.
    let reloaded = load_from(&path);
    assert!(reloaded.validate().is_empty(), "{:?}", reloaded.validate());
    assert!(reloaded.full_width && reloaded.focus_mode);
    assert!(!reloaded.dynamic_island_enable);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn widget_map_keys_are_canonicalized_for_both_builds() {
    let (dir, path) = temp_settings("widget-map-keys");
    fs::write(
        &path,
        r#"{
            "Widgets": ["stats", "aiusage_claude", "keyboard"],
            "InstalledWidgets": ["stats", "aiusage_claude", "keyboard"],
            "WidgetDensityOverrides": {"stats": "Compact", "keyboard": "Full"},
            "DynamicIslandPlacements": {"stats": "*"},
            "DockItemOrder": ["start", "widget:stats", "widget:aiusage_claude"],
            "Profiles": [{"Name":"P","Widgets":["stats"],"InstalledWidgets":["stats"],"DockItemOrder":["widget:keyboard"]}]
        }"#,
    )
    .unwrap();
    let loaded = load_from(&path);
    assert_eq!(
        loaded.enabled_widgets,
        ["SystemStats", "AiUsageClaude", "KeyboardLayout"]
    );
    assert_eq!(
        loaded.widget_density_overrides.get("SystemStats"),
        Some(&WidgetDensityMode::Compact)
    );
    assert_eq!(
        loaded.widget_density_overrides.get("KeyboardLayout"),
        Some(&WidgetDensityMode::Full)
    );
    assert_eq!(
        loaded
            .dynamic_island_placements
            .get("SystemStats")
            .map(String::as_str),
        Some("*")
    );
    assert_eq!(
        loaded.dock_item_order,
        ["start", "widget:SystemStats", "widget:AiUsageClaude"]
    );
    assert_eq!(
        loaded.profiles[0].dock_item_order,
        ["widget:KeyboardLayout"]
    );
    assert!(loaded.validate().is_empty(), "{:?}", loaded.validate());

    save_to(&path, &loaded).unwrap();
    let raw = fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"SystemStats\": \"Compact\""), "{raw}");
    assert!(raw.contains("\"SystemStats\": \"*\""), "{raw}");
    assert!(raw.contains("widget:SystemStats"), "{raw}");
    assert!(!raw.contains("\"stats\""), "{raw}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_saves_use_unique_temporary_files_and_keep_valid_json() {
    let (dir, path) = temp_settings("concurrent-writes");
    let handles: Vec<_> = (0..8)
        .map(|index| {
            let path = path.clone();
            std::thread::spawn(move || {
                let settings = DockSettings {
                    weather_city: format!("City-{index}"),
                    ..DockSettings::default()
                };
                save_to(&path, &settings)
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    let raw = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(parsed.get("WeatherCity").is_some());
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.")
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporary files must be renamed away: {leftovers:?}"
    );
    assert!(load_from(&path).validate().is_empty());
    let _ = fs::remove_dir_all(&dir);
}
#[test]
fn display_care_settings_survive_reference_load_save_load() {
    let (dir, path) = temp_settings("display-care");
    fs::write(
        &path,
        r#"{
          "DockBackground": "TrueBlack",
          "OledReduceGlowEnabled": true,
          "BurnInShiftEnabled": true,
          "BurnInShiftMinutes": 60,
          "OledAutoTriggerEnabled": true,
          "OledLowBatteryThreshold": 20,
          "OledAutoOnNightLight": true,
          "AmbientModeEnabled": true,
          "AnimationFpsCap": 60,
          "FutureDisplayCareKey": "keep-me"
        }"#,
    )
    .unwrap();

    let first = load_from(&path);
    assert_eq!(first.dock_background, SurfaceBackground::TrueBlack);
    assert!(first.oled_reduce_glow_enabled);
    assert!(first.burn_in_shift_enabled);
    assert_eq!(first.burn_in_shift_minutes, 60);
    assert!(first.oled_auto_trigger_enabled);
    assert_eq!(first.oled_low_battery_threshold, 20);
    assert!(first.oled_auto_on_night_light);
    assert!(first.ambient_mode_enabled);
    assert_eq!(first.animation_fps_cap, 60);
    assert!(first.validate().is_empty(), "{:?}", first.validate());
    assert_eq!(
        first
            .extra
            .get("FutureDisplayCareKey")
            .and_then(|value| value.as_str()),
        Some("keep-me")
    );

    save_to(&path, &first).unwrap();
    let saved = fs::read_to_string(&path).unwrap();
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
    ] {
        assert!(saved.contains(&format!("\"{key}\"")), "{key} was not saved");
    }

    let second = load_from(&path);
    assert_eq!(second, first);
    assert!(second.extra.contains_key("FutureDisplayCareKey"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn invalid_display_care_values_fall_back_to_defaults() {
    for (name, key, value) in [
        ("minutes-low", "BurnInShiftMinutes", "4"),
        ("minutes-high", "BurnInShiftMinutes", "121"),
        ("threshold", "OledLowBatteryThreshold", "15"),
        ("fps-cap", "AnimationFpsCap", "90"),
    ] {
        let (dir, path) = temp_settings(name);
        fs::write(&path, format!(r#"{{"Theme":"Dark","{key}":{value}}}"#)).unwrap();
        let loaded = load_from(&path);
        assert_eq!(
            loaded,
            DockSettings::default(),
            "{key}={value} was accepted"
        );
        assert!(
            fs::read_dir(&dir).unwrap().flatten().any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.invalid-")),
            "an invalid {key} must back up the rejected file"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn boundary_display_care_values_are_accepted() {
    let (dir, path) = temp_settings("display-care-bounds");
    fs::write(
        &path,
        r#"{
          "BurnInShiftMinutes": 5,
          "OledLowBatteryThreshold": 50,
          "AnimationFpsCap": 120
        }"#,
    )
    .unwrap();
    let loaded = load_from(&path);
    assert_eq!(loaded.burn_in_shift_minutes, 5);
    assert_eq!(loaded.oled_low_battery_threshold, 50);
    assert_eq!(loaded.animation_fps_cap, 120);
    assert!(loaded.validate().is_empty(), "{:?}", loaded.validate());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn custom_widget_ids_are_supported_but_constrained() {
    assert!(is_supported_widget_id("custom:usd-try"));
    assert!(is_supported_widget_id("CUSTOM:Clock_2"));
    assert!(!is_supported_widget_id("custom:"));
    assert!(!is_supported_widget_id("custom:../evil"));
    assert!(!is_supported_widget_id("custom:a/b"));
    assert_eq!(
        canonical_widget_setting_id("custom:usd-try"),
        "custom:usd-try"
    );

    let mut settings = DockSettings::default();
    settings.set_widget_enabled("custom:usd-try", true);
    assert!(settings.validate().is_empty(), "{:?}", settings.validate());
    assert!(settings.is_widget_enabled("custom:usd-try"));
}
