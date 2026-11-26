//! C# `settings.json` compatibility checks against the public core API.

use std::fs;
use std::path::PathBuf;

use opentaskbar_core::hotkey::HotkeyChoice;
use opentaskbar_core::settings::{
    AppTheme, DockSettings, DockWidthMode, WidgetDensityMode, load_from, save_to,
};

fn temp_settings(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ot-cfg-compat-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    (dir.clone(), dir.join("settings.json"))
}

#[test]
fn csharp_settings_file_survives_load_save_load() {
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
  "AntigravityGroups": "All",
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
  "FutureCSharpOnlySetting": {"enabled": true}
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
    assert!(first.extra.get("FutureCSharpOnlySetting").is_some());
    assert!(first.validate().is_empty(), "{:?}", first.validate());

    save_to(&path, &first).unwrap();
    let second = load_from(&path);
    assert_eq!(second, first);
    assert!(
        second.extra.get("FutureCSharpOnlySetting").is_some(),
        "unknown C# keys must survive a save/load cycle"
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
fn all_new_csharp_properties_deserialize() {
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
        "AiUsageProviders": 255,
        "AiUsageRing": "Session",
        "AntigravityGroups": 7,
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
    assert_eq!(parsed.ai_usage_providers, 255);
    assert_eq!(parsed.dock_width_mode, DockWidthMode::Fill);
    assert_eq!(parsed.dock_height_percent, 10);
    assert_eq!(parsed.wireless_battery_poll_seconds, 60);
    assert!(parsed.wireless_battery_listen_only);
    assert!(parsed.mirror_main_dock);
    assert!(parsed.validate().is_empty(), "{:?}", parsed.validate());
}
