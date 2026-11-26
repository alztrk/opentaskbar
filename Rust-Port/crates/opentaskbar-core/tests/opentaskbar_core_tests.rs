//! C# `SettingsAndAutoHideChecks` / `DockSettingsStore` parity (pure portion).
//!
//! The cross-WS settings implementation lives in WS-A's `settings.rs` and is not
//! part of this worktree's baseline. These tests therefore assert the *contract*
//! (C1) directly: every C# `DockSettings` JSON key must survive a serde round-trip
//! and the mandatory `DockProfile` fields must not be dropped. No private or
//! not-yet-existing API is referenced.

use std::collections::BTreeSet;

use opentaskbar_core::settings::DockSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// 110 public constructor properties of C# `Models/DockSettings.cs` (in source order).
const CS_SETTINGS_KEYS: &[&str] = &[
    "Position",
    "PinOrder",
    "Theme",
    "Language",
    "AnimationsEnabled",
    "PreviewsEnabled",
    "NotificationBadgesEnabled",
    "AutoHide",
    "SurfaceOpacity",
    "DockBackground",
    "DockOpacity",
    "DockBorderThickness",
    "DockBorderColor",
    "MenuBackground",
    "MenuOpacity",
    "MenuBorderThickness",
    "MenuBorderColor",
    "WidgetBackground",
    "WidgetOpacity",
    "WidgetBorderThickness",
    "WidgetBorderColor",
    "SettingsBackground",
    "SettingsOpacity",
    "SettingsBorderThickness",
    "SettingsBorderColor",
    "Widgets",
    "InstalledWidgets",
    "Layout",
    "ContentAlignment",
    "Size",
    "MagnificationEnabled",
    "ShowClock",
    "ShowMedia",
    "ShowSystemStats",
    "ShowFocus",
    "ShowAudio",
    "ShowQuickSettings",
    "FocusMinutes",
    "FocusBreakMinutes",
    "FocusSessions",
    "ShowRecycleBin",
    "LauncherHotkey",
    "DockHotkey",
    "StartAction",
    "StartPlacement",
    "StartWidgetIndex",
    "WheelVolumeEnabled",
    "CheckUpdates",
    "ShowWeather",
    "WeatherCity",
    "CurrentDesktopOnly",
    "ShowOnAllMonitors",
    "TargetDisplay",
    "SecondaryApps",
    "SecondaryWidgets",
    "Edge",
    "StackFolders",
    "Profiles",
    "ActiveProfile",
    "DockScale",
    "WidgetOrder",
    "Clock24Hour",
    "ClockShowSeconds",
    "DateStyle",
    "StatsMeters",
    "ThemePack",
    "ThemeApplicationIcons",
    "GlobalWidgetDensity",
    "WidgetDensityOverrides",
    "DockItemOrder",
    "DockCornerRadius",
    "AiUsageProviders",
    "AiUsageRing",
    "AntigravityGroups",
    "AppGroups",
    "ShowSearchButton",
    "ShowTaskViewButton",
    "ShowTrayButton",
    "ShowDesktopButton",
    "DockWidthMode",
    "DockWidthPixels",
    "DockWidthPercent",
    "DockTopGapUnit",
    "DockTopGapPixels",
    "DockTopGapPercent",
    "DockBottomGapUnit",
    "DockBottomGapPixels",
    "DockBottomGapPercent",
    "DockIconPadding",
    "FocusModeEnabled",
    "CustomLauncherHotkey",
    "CustomDockHotkey",
    "PinnedTrayIcons",
    "DockHeightMode",
    "DockHeightPixels",
    "DockHeightPercent",
    "WirelessBatteryPollSeconds",
    "WirelessBatteryLowThreshold",
    "IgnoredWirelessDevices",
    "WirelessDeviceAliases",
    "WirelessBatteryListenOnly",
    "WeatherUnit",
    "DateFormat",
    "DynamicIslandPlacements",
    "RevealDockOnWinKey",
    "ClockDateLayout",
    "MirrorMainDock",
    "DynamicIslandVisibility",
    "DynamicIslandHideInFullscreen",
    "DynamicIslandWidgetOrders",
];

/// Mandatory `DockProfile` fields from C1 (all defaulted, none may be lost).
const PROFILE_MANDATORY_KEYS: &[&str] = &[
    "HiddenApps",
    "InstalledWidgets",
    "WidgetOrder",
    "DockItemOrder",
    "DockWidthMode",
    "DockWidthPixels",
    "DockWidthPercent",
    "Theme",
    "ThemePack",
    "ThemeApplicationIcons",
    "LauncherHotkey",
    "DockHotkey",
    "CustomLauncherHotkey",
    "CustomDockHotkey",
];

fn representative_value(key: &str) -> Value {
    const BOOLS: &[&str] = &[
        "AnimationsEnabled",
        "MagnificationEnabled",
        "PreviewsEnabled",
        "NotificationBadgesEnabled",
        "ShowClock",
        "ShowMedia",
        "ShowSystemStats",
        "ShowFocus",
        "ShowAudio",
        "ShowQuickSettings",
        "ShowRecycleBin",
        "WheelVolumeEnabled",
        "CheckUpdates",
        "ShowWeather",
        "CurrentDesktopOnly",
        "ShowOnAllMonitors",
        "SecondaryWidgets",
        "Clock24Hour",
        "ClockShowSeconds",
        "ThemeApplicationIcons",
        "ShowSearchButton",
        "ShowTaskViewButton",
        "ShowTrayButton",
        "ShowDesktopButton",
        "FocusModeEnabled",
        "WirelessBatteryListenOnly",
        "RevealDockOnWinKey",
        "MirrorMainDock",
        "DynamicIslandHideInFullscreen",
    ];
    const NUMBERS: &[&str] = &[
        "FocusMinutes",
        "FocusBreakMinutes",
        "FocusSessions",
        "StartWidgetIndex",
        "DockWidthPixels",
        "DockWidthPercent",
        "DockTopGapPixels",
        "DockTopGapPercent",
        "DockBottomGapPixels",
        "DockBottomGapPercent",
        "DockIconPadding",
        "DockCornerRadius",
        "DockHeightPixels",
        "DockHeightPercent",
        "WirelessBatteryPollSeconds",
        "WirelessBatteryLowThreshold",
    ];
    const DECIMALS: &[&str] = &[
        "SurfaceOpacity",
        "DockOpacity",
        "DockBorderThickness",
        "MenuOpacity",
        "MenuBorderThickness",
        "WidgetOpacity",
        "WidgetBorderThickness",
        "SettingsOpacity",
        "SettingsBorderThickness",
        "DockScale",
    ];
    const ARRAYS: &[&str] = &[
        "PinOrder",
        "Widgets",
        "InstalledWidgets",
        "StackFolders",
        "Profiles",
        "WidgetOrder",
        "DockItemOrder",
        "AppGroups",
        "PinnedTrayIcons",
        "IgnoredWirelessDevices",
    ];
    const MAPS: &[&str] = &[
        "WidgetDensityOverrides",
        "WirelessDeviceAliases",
        "DynamicIslandPlacements",
        "DynamicIslandWidgetOrders",
    ];
    if BOOLS.contains(&key) {
        return json!(true);
    }
    if NUMBERS.contains(&key) {
        return json!(30);
    }
    if DECIMALS.contains(&key) {
        return json!(0.9);
    }
    if key == "Profiles" {
        return json!([{
            "Name": "p",
            "Position": "Center",
            "Size": "Compact",
            "AutoHide": "Disabled",
            "EnabledWidgets": []
        }]);
    }
    if key == "AppGroups" {
        return json!([{"Id": "g", "Name": "Group", "ExecutablePaths": ["a.exe"]}]);
    }
    if ARRAYS.contains(&key) {
        return json!(["a", "b"]);
    }
    if MAPS.contains(&key) {
        return json!({"a": "b"});
    }
    // Strings and enums. Enum spellings chosen from the C# declarations so the
    // value deserializes even after WS-A promotes these keys to typed fields.
    match key {
        "Position" | "ContentAlignment" => json!("Left"),
        "Theme" => json!("System"),
        "AutoHide" => json!("Smart"),
        "DockBackground" | "MenuBackground" | "WidgetBackground" | "SettingsBackground" => {
            json!("Glass")
        }
        "Size" => json!("Large"),
        "Edge" => json!("Left"),
        "Layout" => json!("FullWidth"),
        "StartAction" => json!("WindowsStart"),
        "StartPlacement" => json!("Start"),
        "SecondaryApps" => json!("CurrentMonitorOnly"),
        "DateStyle" => json!("DayAndShortDate"),
        "StatsMeters" => json!("All"),
        "GlobalWidgetDensity" => json!("Auto"),
        "AiUsageProviders" => json!("None"),
        "AiUsageRing" => json!("Highest"),
        "AntigravityGroups" => json!("All"),
        "DockWidthMode" | "DockHeightMode" => json!("Auto"),
        "DockTopGapUnit" | "DockBottomGapUnit" => json!("Pixels"),
        "WeatherUnit" => json!("Celsius"),
        "DateFormat" => json!("System"),
        "ClockDateLayout" => json!("Beside"),
        "DynamicIslandVisibility" => json!("Always"),
        "LauncherHotkey" => json!("AltSpace"),
        "DockHotkey" => json!("CtrlAltD"),
        _ => json!("value"),
    }
}

#[test]
fn c1_contract_lists_all_110_csharp_keys() {
    assert_eq!(CS_SETTINGS_KEYS.len(), 110);
    let unique: BTreeSet<_> = CS_SETTINGS_KEYS.iter().copied().collect();
    assert_eq!(
        unique.len(),
        CS_SETTINGS_KEYS.len(),
        "duplicate key in contract"
    );
}

#[test]
fn core_settings_round_trip_preserves_every_csharp_key() {
    let mut input = Map::new();
    for key in CS_SETTINGS_KEYS {
        input.insert((*key).to_owned(), representative_value(key));
    }

    let settings: DockSettings = serde_json::from_value(Value::Object(input.clone()))
        .expect("C# settings.json must deserialize into Rust DockSettings");
    let output = serde_json::to_value(&settings).expect("settings must serialize");
    let object = output
        .as_object()
        .expect("serialized settings must be an object");

    let missing: Vec<_> = CS_SETTINGS_KEYS
        .iter()
        .filter(|key| !object.contains_key(**key))
        .copied()
        .collect();
    assert!(missing.is_empty(), "keys lost on round-trip: {missing:?}");
}

#[test]
fn unknown_future_keys_are_preserved_via_extra() {
    let raw = json!({
        "Position": "Left",
        "FutureCSharpOnlySetting": {"nested": [1, 2, 3]},
        "AnotherFutureKey": true
    });
    let settings: DockSettings = serde_json::from_value(raw).unwrap();
    let back = serde_json::to_value(&settings).unwrap();
    assert_eq!(back["FutureCSharpOnlySetting"]["nested"][2], json!(3));
    assert_eq!(back["AnotherFutureKey"], json!(true));
}

#[test]
fn alias_keys_do_not_disappear() {
    // C1 aliases: Widgets→EnabledWidgets, ShowClock→ShowClockWidget, Layout→FullWidth.
    let raw = json!({
        "Widgets": ["clock", "audio"],
        "ShowClock": true,
        "Layout": "FullWidth",
        "FocusModeEnabled": true,
        "PinnedTrayIcons": ["guid:{1234}"]
    });
    let settings: DockSettings = serde_json::from_value(raw).unwrap();
    let back = serde_json::to_value(&settings).unwrap();
    for key in [
        "Widgets",
        "ShowClock",
        "Layout",
        "FocusModeEnabled",
        "PinnedTrayIcons",
    ] {
        assert!(back.get(key).is_some(), "alias key {key} was dropped");
    }
}

/// Local mirror of the *contract* `DockProfile` (WS-A adds `flatten extra` to the
/// real type; this reference test does not depend on that cross-WS change).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContractProfile {
    name: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[test]
fn dock_profile_mandatory_fields_survive_round_trip() {
    let mut raw = Map::new();
    raw.insert("Name".into(), json!("Focus profile"));
    for key in PROFILE_MANDATORY_KEYS {
        raw.insert((*key).to_owned(), representative_value(key));
    }
    raw.insert("SomeFutureProfileField".into(), json!("keep-me"));

    let profile: ContractProfile = serde_json::from_value(Value::Object(raw)).unwrap();
    let back = serde_json::to_value(&profile).unwrap();
    let object = back.as_object().unwrap();
    for key in PROFILE_MANDATORY_KEYS {
        assert!(object.contains_key(*key), "profile field {key} was dropped");
    }
    assert_eq!(object["SomeFutureProfileField"], json!("keep-me"));
}

/// C# `DockSettingsStore.IsValid()` rule subset (C1 §validate).
fn validate_contract(values: &Map<String, Value>) -> Vec<&'static str> {
    let mut errors = Vec::new();
    if values.get("LauncherHotkey") == values.get("DockHotkey")
        && values.get("LauncherHotkey").is_some()
    {
        errors.push("hotkeys must differ");
    }
    for key in [
        "DockWidthPercent",
        "DockTopGapPercent",
        "DockBottomGapPercent",
        "DockHeightPercent",
    ] {
        if let Some(n) = values.get(key).and_then(Value::as_i64)
            && !(10..=100).contains(&n)
        {
            errors.push("percent out of range");
        }
    }
    if let Some(n) = values
        .get("WirelessBatteryPollSeconds")
        .and_then(Value::as_i64)
        && !(15..=300).contains(&n)
    {
        errors.push("wireless poll out of range");
    }
    if values.get("StartPlacement").and_then(Value::as_str) == Some("InWidgets")
        && values.get("StartWidgetIndex").and_then(Value::as_i64) == Some(-1)
    {
        errors.push("invalid island placement");
    }
    errors
}

#[test]
fn validate_rejects_hotkey_collisions_and_out_of_range_values() {
    let mut bad = Map::new();
    bad.insert("LauncherHotkey".into(), json!("AltSpace"));
    bad.insert("DockHotkey".into(), json!("AltSpace"));
    bad.insert("DockWidthPercent".into(), json!(9));
    bad.insert("WirelessBatteryPollSeconds".into(), json!(5));
    bad.insert("StartPlacement".into(), json!("InWidgets"));
    bad.insert("StartWidgetIndex".into(), json!(-1));
    let errors = validate_contract(&bad);
    assert_eq!(errors.len(), 4, "{errors:?}");
}

#[test]
fn validate_accepts_a_well_formed_settings_object() {
    let mut good = Map::new();
    good.insert("LauncherHotkey".into(), json!("AltSpace"));
    good.insert("DockHotkey".into(), json!("CtrlAltD"));
    good.insert("DockWidthPercent".into(), json!(60));
    good.insert("WirelessBatteryPollSeconds".into(), json!(30));
    assert!(validate_contract(&good).is_empty());
}
