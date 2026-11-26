//! On-demand GPUI Settings window (`OpenTaskBar.exe --settings`).
//!
//! Grouped settings with a responsive navigation pane and immediate dock updates.
//! Includes the full `.otbackup` archive UI, theme editor/gallery, hotkey capture,
//! dynamic search catalogue and widget panel host (C# `SettingsWindow` parity).

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Instant;

use std::ops::Range;
use std::time::Duration;

use gpui::{
    AnyElement, App, Application, Bounds, Context, EntityInputHandler, FocusHandle,
    InteractiveElement, KeyBinding, KeyDownEvent, MouseButton, ScrollAnchor, ScrollHandle,
    SharedString, TitlebarOptions, UTF16Selection, Window, WindowBounds, WindowControlArea,
    WindowOptions, actions, div, prelude::*, px, rgba, size,
};
use opentaskbar_core::hotkey;
use opentaskbar_core::i18n::{self as loc, LANGUAGES};
use opentaskbar_core::settings::{
    self, AppTheme, AutoHideMode, ClockDateFormat, ClockDateLayout, ClockDateStyle, DockPosition,
    DockSettings, DockSize, DynamicIslandVisibilityMode, FisheyeIntensity, MiddleClickAction,
    SurfaceBackground, TemperatureUnit, WidgetDensityMode,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::{Map, Value, json};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, RegisterWindowMessageW, SendNotifyMessageW,
};

use crate::design::fonts;
use crate::dock::dcomp::{SystemStatColumn, WidgetChipSpec};
use crate::platform::{pins, theme_packs, toast, update, win32};
use crate::ui::components::{
    ButtonVariant, DropdownOption, UiTheme, badge, button, dim_when, dropdown, icon_glyph,
    ios_switch, live_slider, section_card, setting_row, setting_row_desc, setting_row_with_id,
    setting_row_with_id_desc, sidebar_nav_item, stepper,
};
use crate::ui::menu::{self, BorderStyle, MenuEntry, MenuIcon};
use crate::ui::prompt;
use crate::ui::settings_search::{
    self, SearchCatalog, SearchEntry, SettingsSearchEntry, widget_has_settings,
};
use crate::ui::text_input::{TextInput, apply_keystroke, clamp_numeric};
use crate::widgets;

pub const SETTINGS_ARG: &str = "--settings";
pub const RELOAD_MSG_NAME: &str = "OpenTaskBarReloadSettings";

const WIN_W: f32 = 1040.0;
const WIN_H: f32 = 740.0;
const MIN_WIDTH: f32 = 540.0;
const MIN_HEIGHT: f32 = 428.0;
const SIDEBAR_BREAKPOINT: f32 = 680.0;
const SIDEBAR_WIDTH: f32 = 196.0;
const WIDGET_COLUMNS_BREAKPOINT: f32 = 520.0;

actions!(settings_navigation, [TabNext, TabPrevious]);
actions!(settings_panels, [CloseWidgetPanel, ToggleFlyoutPreview]);

fn set_border_thickness(settings: &mut DockSettings, surface: usize, value: f64) {
    let value = value.clamp(0.0, 8.0);
    match surface {
        0 => settings.dock_border_thickness = value,
        1 => settings.menu_border_thickness = value,
        2 => settings.widget_border_thickness = value,
        _ => settings.settings_border_thickness = value,
    }
}

/// Parses `#RRGGBB` / `RRGGBB` into the ARGB `u32` used by GPUI surfaces.
fn parse_hex_color(value: &str) -> Option<u32> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(((0xffu32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
}

pub fn spawn_settings_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg(SETTINGS_ARG).spawn();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsPage {
    Dock,
    Widgets,
    DynamicIsland,
    Themes,
    Preferences,
    About,
}

impl SettingsPage {
    const TOP: [Self; 4] = [Self::Dock, Self::Widgets, Self::DynamicIsland, Self::Themes];
    const BOTTOM: [Self; 2] = [Self::Preferences, Self::About];

    fn from_name(name: &str) -> Self {
        match name {
            "Widgets" => Self::Widgets,
            "DynamicIsland" => Self::DynamicIsland,
            "Themes" => Self::Themes,
            "Preferences" => Self::Preferences,
            "About" => Self::About,
            _ => Self::Dock,
        }
    }

    pub(crate) fn title(self) -> String {
        match self {
            Self::Dock => loc::t("Dock"),
            Self::Widgets => loc::t("Widgets"),
            Self::DynamicIsland => loc::t("DynamicIslandTitle"),
            Self::Themes => loc::t("Themes"),
            Self::Preferences => loc::t("Preferences"),
            Self::About => loc::t("About"),
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Dock => "\u{E90E}",
            Self::Widgets => "\u{F246}",
            Self::DynamicIsland => "\u{E8A1}",
            Self::Themes => "\u{E790}",
            Self::Preferences => "\u{E713}",
            Self::About => "\u{E946}",
        }
    }

    fn description(self) -> String {
        match self {
            Self::Dock => loc::t("SettingsPageDockDescription"),
            Self::Widgets => loc::t("SettingsPageWidgetsDescription"),
            Self::DynamicIsland => loc::t("DynamicIslandEnableDescription"),
            Self::Themes => loc::t("SettingsPageThemesDescription"),
            Self::Preferences => loc::t("SettingsPagePreferencesDescription"),
            Self::About => loc::t("SettingsPageAboutDescription"),
        }
    }
}

/// Display label for a widget filter/category value stored in `widget_filter`.
pub(crate) fn category_label(category: &str) -> String {
    match category {
        "All" => loc::t("WidgetCategoryAll"),
        "Active" => loc::t("Active"),
        "Time & Weather" => loc::t("WidgetCategoryTime"),
        "System & Hardware" => loc::t("WidgetCategorySystem"),
        "AI Quotas" => loc::t("WidgetCategoryAi"),
        "Media & Controls" => loc::t("WidgetCategoryMedia"),
        "Productivity" => loc::t("WidgetCategoryProductivity"),
        other => loc::t(other),
    }
}

fn widget_glyph(id: &str) -> &'static str {
    if id.starts_with("aiusage_") {
        return "\u{E99A}";
    }
    match id {
        "clock" => "\u{E823}",
        "weather" => "\u{E753}",
        "dayprogress" => "\u{E706}",
        "stopwatch" => "\u{E916}",
        "countdown" => "\u{E917}",
        "stats" => "\u{E9D9}",
        "netspeed" => "\u{E8AB}",
        "battery" => "\u{E850}",
        "bluetoothbattery" => "\u{E702}",
        "storage" => "\u{EDA2}",
        "media" => "\u{E8D6}",
        "audio" => "\u{E995}",
        "quicksettings" => "\u{E9E9}",
        "keyboard" => "\u{E765}",
        "focus" => "\u{E7C8}",
        _ => "\u{F246}",
    }
}

fn country_flag_path(country: &str) -> Option<SharedString> {
    let locale = match country {
        "Turkey" => "tr-TR",
        "Germany" => "de-DE",
        "United States" => "en-US",
        "France" => "fr-FR",
        "Netherlands" => "nl-NL",
        "Spain" => "es-ES",
        "Italy" => "it-IT",
        "Poland" => "pl-PL",
        "Japan" => "ja-JP",
        "South Korea" => "ko-KR",
        "China" => "zh-CN",
        "Brazil" => "pt-BR",
        "Russia" => "ru-RU",
        "Ukraine" => "uk-UA",
        _ => return None,
    };
    Some(SharedString::from(fonts::flag_path(locale)))
}

fn widget_has_inline_settings(id: &str) -> bool {
    matches!(id, "clock" | "weather" | "stats" | "bluetoothbattery") || id.starts_with("aiusage_")
}

// ---------------------------------------------------------------------------
// SettingsBackdrop — acrylic + DWMWA_BORDER_COLOR=NONE + Win11 system backdrop,
// disabled under High Contrast so text remains readable (C# SettingsBackdrop.cs).
// ---------------------------------------------------------------------------

fn win_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn high_contrast_enabled() -> bool {
    let path = win_wide("Control Panel\\Accessibility\\HighContrast");
    let flags_name = win_wide("Flags");
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: both buffers are null-terminated and `key` is only used when the call succeeds.
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return false;
        }
        let mut flags: u32 = 0;
        let mut len = size_of::<u32>() as u32;
        let status = RegQueryValueExW(
            key,
            flags_name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            (&mut flags as *mut u32).cast(),
            &mut len,
        );
        RegCloseKey(key);
        status == 0 && flags & 1 != 0
    }
}

fn apply_settings_backdrop(window: &Window, is_dark: bool) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(win32_handle) = handle.as_raw() else {
        return;
    };
    let hwnd = win32_handle.hwnd.get() as HWND;
    let dark: u32 = u32::from(is_dark);
    let border_none: u32 = 0xFFFF_FFFF; // DWMWA_COLOR_NONE
    // DWMSBT_NONE (1) for High Contrast, DWMSBT_TRANSIENTWINDOW (3) acrylic otherwise.
    let backdrop: u32 = if high_contrast_enabled() { 1 } else { 3 };
    // SAFETY: `hwnd` belongs to this window and all attribute values are plain u32s.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            (&border_none as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            (&backdrop as *const u32).cast(),
            size_of::<u32>() as u32,
        );
    }
}

// ---------------------------------------------------------------------------
// Full backup (.otbackup) — small stored-ZIP writer/reader, no extra crate.
// C# `FullBackupArchive.cs` packs manifest + settings + pins + active theme folder.
// ---------------------------------------------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Creates a ZIP archive with stored (uncompressed) entries.
fn zip_store(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for (name, data) in entries {
        let name = name.as_bytes();
        let crc = crc32(data);
        offsets.push((out.len() as u32, name.len() as u16, crc));
        put_u32(&mut out, 0x0403_4b50);
        put_u16(&mut out, 20);
        put_u16(&mut out, 0x0800); // UTF-8 names
        put_u16(&mut out, 0); // stored
        put_u16(&mut out, 0);
        put_u16(&mut out, 0x0021); // 1980-01-01
        put_u32(&mut out, crc);
        put_u32(&mut out, data.len() as u32);
        put_u32(&mut out, data.len() as u32);
        put_u16(&mut out, name.len() as u16);
        put_u16(&mut out, 0);
        out.extend_from_slice(name);
        out.extend_from_slice(data);
    }
    let cd_start = out.len() as u32;
    for ((name, data), (offset, name_len, crc)) in entries.iter().zip(&offsets) {
        let name = name.as_bytes();
        put_u32(&mut out, 0x0201_4b50);
        put_u16(&mut out, 20);
        put_u16(&mut out, 20);
        put_u16(&mut out, 0x0800);
        put_u16(&mut out, 0);
        put_u16(&mut out, 0);
        put_u16(&mut out, 0x0021);
        put_u32(&mut out, *crc);
        put_u32(&mut out, data.len() as u32);
        put_u32(&mut out, data.len() as u32);
        put_u16(&mut out, *name_len);
        put_u16(&mut out, 0);
        put_u16(&mut out, 0);
        put_u16(&mut out, 0);
        put_u16(&mut out, 0);
        put_u32(&mut out, 0);
        put_u32(&mut out, *offset);
        out.extend_from_slice(name);
    }
    let cd_size = out.len() as u32 - cd_start;
    put_u32(&mut out, 0x0605_4b50);
    put_u16(&mut out, 0);
    put_u16(&mut out, 0);
    put_u16(&mut out, entries.len() as u16);
    put_u16(&mut out, entries.len() as u16);
    put_u32(&mut out, cd_size);
    put_u32(&mut out, cd_start);
    put_u16(&mut out, 0);
    out
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Reads stored ZIP entries (rejects compressed entries with a clear error).
fn zip_unstore(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut out = BTreeMap::new();
    let mut at = 0usize;
    while at + 30 <= bytes.len() {
        if read_u32(bytes, at) != Some(0x0403_4b50) {
            break;
        }
        let method = read_u16(bytes, at + 8).ok_or("truncated local header")?;
        if method != 0 {
            return Err("compressed entries are not supported".to_owned());
        }
        let size = read_u32(bytes, at + 22).ok_or("truncated size")? as usize;
        let name_len = read_u16(bytes, at + 26).ok_or("truncated name")? as usize;
        let extra_len = read_u16(bytes, at + 28).ok_or("truncated extra")? as usize;
        let name_start = at + 30;
        let data_start = name_start + name_len + extra_len;
        let data_end = data_start + size;
        if data_end > bytes.len() {
            return Err("archive entry exceeds file".to_owned());
        }
        let name = std::str::from_utf8(&bytes[name_start..data_start - extra_len])
            .map_err(|_| "invalid entry name".to_owned())?;
        out.insert(name.to_owned(), bytes[data_start..data_end].to_vec());
        at = data_end;
    }
    Ok(out)
}

fn safe_backup_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(name);
    if path.is_absolute()
        || name.contains('\\')
        || name.contains(':')
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(path.to_path_buf())
}

fn theme_folder() -> std::path::PathBuf {
    settings::app_data_dir().join("themes")
}

fn build_backup(settings: &DockSettings) -> Vec<u8> {
    let mut pins = Map::new();
    pins.insert("PinnedApps".to_owned(), json!(settings.user_pinned_apps));
    pins.insert(
        "UnpinnedApps".to_owned(),
        json!(settings.user_unpinned_apps),
    );
    pins.insert(
        "PinnedBackgroundApps".to_owned(),
        json!(settings.user_pinned_background_apps),
    );
    pins.insert("PinOrder".to_owned(), json!(settings.pin_order));

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    files.push((
        "manifest.json".to_owned(),
        serde_json::to_vec_pretty(&json!({
            "Format": "OpenTaskbarFullBackup",
            "Version": 1,
            "CreatedUtc": chrono_stamp(),
        }))
        .unwrap_or_default(),
    ));
    files.push((
        "settings.json".to_owned(),
        serde_json::to_vec_pretty(settings).unwrap_or_default(),
    ));
    files.push((
        "pins.json".to_owned(),
        serde_json::to_vec_pretty(&pins).unwrap_or_default(),
    ));

    let mut theme_files: Vec<(String, Vec<u8>)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(theme_folder()) {
        for entry in entries.flatten().filter(|e| e.path().is_file()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(rel) = safe_backup_path(&format!("theme/{name}"))
                && let Ok(data) = std::fs::read(entry.path())
            {
                theme_files.push((rel.to_string_lossy().into_owned(), data));
            }
        }
    }
    theme_files.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, data) in &theme_files {
        files.push((name.clone(), data.clone()));
    }
    zip_store(&files)
}

fn chrono_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // ISO-like UTC stamp without a chrono dependency; only used for display in the manifest.
    let days = secs / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    // Howard Hinnant's days-from-civil inverse (Unix epoch = 1970-01-01).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m, d)
}

#[derive(Clone, Debug, Default)]
struct BackupPreview {
    valid: bool,
    error: Option<String>,
    settings: Option<DockSettings>,
    theme_files: usize,
    pinned_apps: usize,
}

fn inspect_backup(bytes: &[u8]) -> BackupPreview {
    let entries = match zip_unstore(bytes) {
        Ok(entries) => entries,
        Err(err) => {
            return BackupPreview {
                valid: false,
                error: Some(err),
                ..Default::default()
            };
        }
    };
    let manifest_ok = entries
        .get("manifest.json")
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
        .and_then(|v| {
            v.get("Format")
                .and_then(Value::as_str)
                .map(|s| s == "OpenTaskbarFullBackup")
        })
        .unwrap_or(false);
    if !manifest_ok {
        return BackupPreview {
            valid: false,
            error: Some("not an OpenTaskbar .otbackup archive".to_owned()),
            ..Default::default()
        };
    }
    let settings = entries
        .get("settings.json")
        .and_then(|raw| serde_json::from_slice::<DockSettings>(raw).ok());
    let pinned_apps = entries
        .get("pins.json")
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
        .and_then(|v| v.get("PinnedApps")?.as_array().map(Vec::len))
        .unwrap_or(0);
    let theme_files = entries.keys().filter(|k| k.starts_with("theme/")).count();
    BackupPreview {
        valid: settings.is_some(),
        error: if settings.is_some() {
            None
        } else {
            Some("settings.json is missing or corrupt".to_owned())
        },
        settings,
        theme_files,
        pinned_apps,
    }
}

fn import_backup(bytes: &[u8]) -> Result<DockSettings, String> {
    let preview = inspect_backup(bytes);
    if !preview.valid {
        return Err(preview
            .error
            .unwrap_or_else(|| "invalid archive".to_owned()));
    }
    let entries = zip_unstore(bytes)?;
    let mut next = preview.settings.ok_or("no settings in archive")?;
    if let Some(pins) = entries.get("pins.json")
        && let Ok(value) = serde_json::from_slice::<Value>(pins)
    {
        let list = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        next.user_pinned_apps = list("PinnedApps");
        next.user_unpinned_apps = list("UnpinnedApps");
        next.user_pinned_background_apps = list("PinnedBackgroundApps");
        next.pin_order = value.get("PinOrder").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
    }
    let root = theme_folder();
    for (name, data) in &entries {
        let Some(rel) = name.strip_prefix("theme/") else {
            continue;
        };
        let Some(path) = safe_backup_path(rel) else {
            continue;
        };
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut file = std::fs::File::create(target).map_err(|e| e.to_string())?;
        file.write_all(data).map_err(|e| e.to_string())?;
    }
    Ok(next)
}

// ---------------------------------------------------------------------------
// WS-A fields that are still in `DockSettings.extra` until the settings worktree lands.
// ---------------------------------------------------------------------------

fn extra_string(settings: &DockSettings, key: &str, fallback: &str) -> String {
    settings
        .extra
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

fn set_extra_string(settings: &mut DockSettings, key: &str, value: &str) {
    settings.extra.insert(key.to_owned(), json!(value));
}

fn extra_number(settings: &DockSettings, key: &str, fallback: i32) -> i32 {
    settings
        .extra
        .get(key)
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .unwrap_or(fallback)
}

fn set_extra_number(settings: &mut DockSettings, key: &str, value: i32) {
    settings.extra.insert(key.to_owned(), json!(value));
}

/// Static 21-country / province picker until WS-G `country_city` ships the API.
const COUNTRIES: &[&str] = &[
    "Turkey",
    "Germany",
    "United States",
    "United Kingdom",
    "France",
    "Netherlands",
    "Spain",
    "Italy",
    "Poland",
    "Sweden",
    "Norway",
    "Finland",
    "Japan",
    "South Korea",
    "China",
    "India",
    "Canada",
    "Australia",
    "Brazil",
    "Mexico",
    "South Africa",
];

const TURKEY_CITIES: &[&str] = &[
    "Adana",
    "Ankara",
    "Antalya",
    "Aydın",
    "Balıkesir",
    "Bursa",
    "Denizli",
    "Diyarbakır",
    "Erzurum",
    "Eskişehir",
    "Gaziantep",
    "Hatay",
    "Isparta",
    "İstanbul",
    "İzmir",
    "Kayseri",
    "Kocaeli",
    "Konya",
    "Malatya",
    "Manisa",
    "Mersin",
    "Muğla",
    "Ordu",
    "Rize",
    "Sakarya",
    "Samsun",
    "Şanlıurfa",
    "Siirt",
    "Sinop",
    "Sivas",
    "Tekirdağ",
    "Tokat",
    "Trabzon",
    "Van",
    "Zonguldak",
    "Afyonkarahisar",
    "Ağrı",
    "Amasya",
    "Ardahan",
    "Artvin",
    "Batman",
    "Bayburt",
    "Bilecik",
    "Bitlis",
    "Bolu",
    "Burdur",
    "Çanakkale",
    "Çankırı",
    "Çorum",
    "Düzce",
    "Edirne",
    "Elazığ",
    "Erzincan",
    "Giresun",
    "Gümüşhane",
    "Hakkâri",
    "Iğdır",
    "Kahramanmaraş",
    "Karabük",
    "Karaman",
    "Kastamonu",
    "Kırıkkale",
    "Kırklareli",
    "Kırşehir",
    "Kütahya",
    "Niğde",
    "Osmaniye",
    "Seyhan",
    "Uşak",
    "Yalova",
    "Yozgat",
    "Bartın",
    "Kilis",
    "Nevşehir",
    "Sırnak",
    "Tunceli",
    "Bingöl",
];

fn cities_for(country: &str) -> &'static [&'static str] {
    if country == "Turkey" {
        TURKEY_CITIES
    } else {
        &["Capital", "Largest City", "North", "South", "East", "West"]
    }
}

type ThemeSwatch = (
    &'static str,
    &'static str,
    crate::design::Rgb,
    crate::design::Rgb,
    crate::design::Rgb,
);

const THEME_GALLERY: &[ThemeSwatch] = &[
    (
        "catppuccin",
        "Catppuccin Mocha",
        (0x1e, 0x1e, 0x2e),
        (0xcb, 0xa6, 0xf7),
        (0x94, 0xe2, 0xd5),
    ),
    (
        "gruvbox",
        "Gruvbox",
        (0x28, 0x28, 0x28),
        (0xfa, 0xbd, 0x2f),
        (0xb8, 0xbb, 0x26),
    ),
    (
        "nord",
        "Nord",
        (0x2e, 0x34, 0x40),
        (0x88, 0xc0, 0xd0),
        (0xa3, 0xbe, 0x8c),
    ),
    (
        "prism",
        "Prism",
        (0x13, 0x13, 0x1c),
        (0xd9, 0x77, 0xf7),
        (0x38, 0xbd, 0xf8),
    ),
    (
        "solarized",
        "Solarized Dark",
        (0x00, 0x2b, 0x36),
        (0x2a, 0xa1, 0x98),
        (0xb5, 0x89, 0x00),
    ),
    (
        "tokyo-night",
        "Tokyo Night",
        (0x1a, 0x1b, 0x26),
        (0xbb, 0x9a, 0xf7),
        (0x7a, 0xe6, 0xd3),
    ),
];

const THEME_TOKEN_CATEGORIES: &[&str] = &[
    "Surfaces", "Text", "Accents", "Borders", "Widgets", "Effects", "Advanced",
];

fn notify_docks_reload() {
    let wide: Vec<u16> = RELOAD_MSG_NAME.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let msg = RegisterWindowMessageW(wide.as_ptr());
        if msg != 0 {
            SendNotifyMessageW(HWND_BROADCAST, msg, 0, 0);
        }
    }
}

struct SettingsView {
    page: SettingsPage,
    focus: FocusHandle,
    /// Header search field. UTF-16 offsets keep the GPUI platform input handler and the
    /// rendered caret in agreement even for non-ASCII (e.g. Turkish) text.
    search_text: String,
    search_selection: Range<usize>,
    search_focus: FocusHandle,
    settings: DockSettings,
    widget_runtime: widgets::WidgetRuntime,
    widget_filter: &'static str,
    expanded_widget: Option<&'static str>,
    open_dropdown: Option<String>,
    startup_enabled: bool,
    scroll_handle: ScrollHandle,
    /// Row targeted by the most recent Settings search result. Rendered with a transient
    /// highlight and used to select which row receives the scroll anchor.
    highlight_row: Option<String>,
    /// Fresh anchor created for each search jump; attached to `highlight_row` during render
    /// and consumed once `ScrollAnchor::scroll_to` has been scheduled.
    scroll_anchor: Option<ScrollAnchor>,
    save_failed: bool,
    font_page: usize,
    hotkey_status: String,
    theme_status: String,
    api_key_status: String,
    weather_suggestions: Vec<widgets::telemetry::GeoLocationMatch>,
    weather_search_status: String,
    /// A burst of slider/toggle events collapses into one disk write + dock broadcast.
    save_pending: bool,
    update_status: String,
    update_release: Option<update::Release>,
    // Search (C# SettingsWindow.Search.cs parity)
    search_query: String,
    search_hits: Vec<SearchEntry>,
    search_focused: bool,
    // WidgetPanelHost (Escape / focus return / 300 ms suppress / preview toggle)
    widget_panel: Option<&'static str>,
    panel_opened_at: Option<Instant>,
    flyout_preview: bool,
    // Settings features
    backup_status: String,
    backup_preview: Option<BackupPreview>,
    hotkey_capture: Option<&'static str>,
    weather_country: String,
    weather_city_index: usize,
    theme_category: usize,
    token_name: String,
    token_value: String,
    ai_status: String,
}

impl SettingsView {
    fn new(settings: DockSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        fonts::apply_settings(&settings);
        let focus = cx.focus_handle();
        focus.focus(window);
        let mut widget_runtime = widgets::WidgetRuntime::default();
        widget_runtime.tick(&settings);
        let is_dark = match settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        };
        apply_settings_backdrop(window, is_dark);
        let weather_city = if settings.weather_city.is_empty() {
            "Istanbul".to_owned()
        } else {
            settings.weather_city.clone()
        };
        let weather_country = if weather_city.contains(", ") {
            weather_city
                .rsplit(", ")
                .next()
                .unwrap_or("Turkey")
                .to_owned()
        } else {
            "Turkey".to_owned()
        };
        let city_name = weather_city.split(", ").next().unwrap_or(&weather_city);
        let weather_city_index = cities_for(&weather_country)
            .iter()
            .position(|c| c.eq_ignore_ascii_case(city_name))
            .unwrap_or(0);
        Self {
            page: SettingsPage::Dock,
            focus,
            search_text: String::new(),
            search_selection: 0..0,
            search_focus: cx.focus_handle(),
            settings,
            widget_runtime,
            widget_filter: "All",
            expanded_widget: None,
            open_dropdown: None,
            startup_enabled: win32::is_startup_enabled(),
            scroll_handle: ScrollHandle::new(),
            highlight_row: None,
            scroll_anchor: None,
            save_failed: false,
            font_page: 0,
            hotkey_status: String::new(),
            theme_status: String::new(),
            api_key_status: String::new(),
            weather_suggestions: Vec::new(),
            weather_search_status: String::new(),
            save_pending: false,
            update_status: String::new(),
            update_release: None,
            search_query: String::new(),
            search_hits: Vec::new(),
            search_focused: false,
            widget_panel: None,
            panel_opened_at: None,
            flyout_preview: true,
            backup_status: String::new(),
            backup_preview: None,
            hotkey_capture: None,
            weather_country,
            weather_city_index,
            theme_category: 0,
            token_name: String::new(),
            token_value: String::new(),
            ai_status: "Not signed in".to_owned(),
        }
    }

    fn search_utf16_len(&self) -> usize {
        self.search_text.encode_utf16().count()
    }

    fn byte_index_for_utf16(text: &str, offset: usize) -> usize {
        let mut units = 0;
        for (index, ch) in text.char_indices() {
            if units >= offset {
                return index;
            }
            units += ch.len_utf16();
        }
        text.len()
    }

    /// Replace a UTF-16 range of the search field and move the caret to the end of the
    /// inserted text. The platform input handler calls this for typing, IME and paste.
    fn replace_search_range(&mut self, range: Range<usize>, text: &str) {
        let len = self.search_utf16_len();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        let start_byte = Self::byte_index_for_utf16(&self.search_text, start);
        let end_byte = Self::byte_index_for_utf16(&self.search_text, end);

        let mut next = String::with_capacity(self.search_text.len() + text.len());
        next.push_str(&self.search_text[..start_byte]);
        next.push_str(text);
        next.push_str(&self.search_text[end_byte..]);

        let cursor = start + text.encode_utf16().count();
        self.search_text = next;
        self.search_selection = cursor..cursor;
        // Any edit means the user is searching again; the previous jump marker is stale.
        self.highlight_row = None;
        self.scroll_anchor = None;
    }

    fn clear_search(&mut self) {
        self.search_text.clear();
        self.search_selection = 0..0;
    }

    /// Render a searchable setting row. The stable `id` is shared with the search catalog;
    /// highlight and scroll anchor are attached only while this row is the active target.
    fn target_row(
        &self,
        id: &'static str,
        t: &UiTheme,
        icon: Option<&'static str>,
        title: impl Into<SharedString>,
        control: impl IntoElement,
    ) -> AnyElement {
        let highlighted = self.highlight_row.as_deref() == Some(id);
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        setting_row_with_id(id, t, anchor, highlighted, icon, title, control).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn target_row_desc(
        &self,
        id: &'static str,
        t: &UiTheme,
        icon: Option<&'static str>,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
        control: impl IntoElement,
    ) -> AnyElement {
        let highlighted = self.highlight_row.as_deref() == Some(id);
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        setting_row_with_id_desc(
            id,
            t,
            anchor,
            highlighted,
            icon,
            title,
            description,
            control,
        )
        .into_any_element()
    }

    /// Same treatment as [`Self::target_row`] for blocks that aren't setting rows
    /// (section descriptions, filter pills, widget placement rows when empty).
    fn mark_target(
        &self,
        id: &'static str,
        t: &UiTheme,
        el: gpui::Div,
    ) -> gpui::Stateful<gpui::Div> {
        let highlighted = self.highlight_row.as_deref() == Some(id);
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        let el = if highlighted {
            el.bg(rgba(t.accent_soft)).rounded(px(8.0))
        } else {
            el
        };
        el.id(SharedString::from(id)).anchor_scroll(anchor)
    }

    fn make_dropdown(
        &self,
        id: impl Into<String>,
        t: &UiTheme,
        options: Vec<DropdownOption>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = id.into();
        let is_open = self.open_dropdown.as_deref() == Some(id.as_str());
        let entity = cx.entity().downgrade();
        let toggle_id = id.clone();
        dropdown(id, t, is_open, options, move |_win, app| {
            let _ = entity.update(app, |this, cx| {
                if this.open_dropdown.as_deref() == Some(toggle_id.as_str()) {
                    this.open_dropdown = None;
                } else {
                    this.open_dropdown = Some(toggle_id.clone());
                }
                cx.notify();
            });
        })
    }

    fn open_search_result(&mut self, entry: SettingsSearchEntry, cx: &mut Context<Self>) {
        self.clear_search();
        self.open_dropdown = None;
        self.page = entry.page;
        if let Some(filter) = entry.widget_filter {
            self.widget_filter = filter;
        }
        if let Some(expand) = entry.expand_widget {
            self.widget_filter = "All";
            self.expanded_widget = Some(expand);
        }
        self.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
        // A fresh anchor is attached to the target row on the next render. Prepaint records
        // that row's origin; the scroll scheduled in `render` then aligns it with the top of
        // the viewport. This works for nested rows, unlike index-based scroll APIs.
        self.highlight_row = Some(entry.row_id.clone());
        self.scroll_anchor = Some(ScrollAnchor::for_handle(self.scroll_handle.clone()));
        cx.notify();

        // Give GPUI a frame to lay out the new page/filter, force the frame that applies the
        // anchor offset, then let the highlight fade so it never sticks around forever.
        let target = entry.row_id;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            // `spawn` hands us an app context; refresh_windows schedules the frame that
            // runs the anchor callback registered during render.
            let _ = cx.update(|app| app.refresh_windows());
            cx.background_executor()
                .timer(Duration::from_millis(1800))
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.highlight_row.as_deref() == Some(target.as_str()) {
                    view.highlight_row = None;
                    view.scroll_anchor = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_search_results(
        &self,
        query: &str,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let catalog = settings_search::catalog();
        let matches = settings_search::search(&catalog, query);
        if matches.is_empty() {
            return vec![
                div()
                    .w_full()
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(rgba(t.border))
                    .bg(rgba(t.surface))
                    .px(px(18.0))
                    .py(px(22.0))
                    .text_size(px(fonts::scaled_size(13.0)))
                    .text_color(rgba(t.text_secondary))
                    .child(loc::t("SettingsSearchEmpty"))
                    .into_any_element(),
            ];
        }

        let mut blocks: Vec<AnyElement> = vec![
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(13.0)))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(loc::t("SettingsSearchResults")),
                )
                .child(badge(t, matches.len().to_string(), true))
                .into_any_element(),
        ];

        let hover_bg = t.surface_hover;
        let hover_border = t.accent;
        for (index, entry) in matches.into_iter().enumerate() {
            let target = entry.clone();
            let title = entry.title.clone();
            let location = format!("{} · {}", entry.page.title(), entry.section);
            blocks.push(
                div()
                    .id(SharedString::from(format!("search-result-{index}")))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(14.0))
                    .py(px(11.0))
                    .rounded(px(10.0))
                    .bg(rgba(t.surface))
                    .border_1()
                    .border_color(rgba(t.border))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgba(hover_bg)).border_color(rgba(hover_border)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_search_result(target.clone(), cx);
                    }))
                    .child(icon_glyph("\u{E721}", 13.0, t.accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(13.0)))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(rgba(t.text_primary))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .text_color(rgba(t.text_secondary))
                                    .child(location),
                            ),
                    )
                    .child(icon_glyph("\u{E76B}", 12.0, t.text_secondary))
                    .into_any_element(),
            );
        }
        blocks
    }

    /// Resolved once per render; `AppTheme::System` reads the registry, so widget preview cards
    /// must reuse this value instead of re-reading it for every card.
    fn resolve_is_dark(&self) -> bool {
        match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        }
    }

    fn refresh_search(&mut self) {
        let catalog = SearchCatalog::from_settings(&self.settings);
        debug_assert!(!catalog.entries().is_empty());
        self.search_hits = catalog
            .search(&self.search_query)
            .into_iter()
            .cloned()
            .collect();
    }

    /// C# `WidgetPanelHost`: Escape/focus return plus 300 ms suppress after close.
    fn close_widget_panel(&mut self, window: &mut Window) {
        if self.widget_panel.take().is_some() {
            self.panel_opened_at = Some(Instant::now());
            self.focus.focus(window);
        }
    }

    fn panel_suppressed(&self) -> bool {
        self.panel_opened_at
            .is_some_and(|at| at.elapsed().as_millis() < 300)
    }

    fn open_widget_panel(&mut self, id: &'static str, window: &mut Window) {
        if self.panel_suppressed() {
            return;
        }
        self.widget_panel = Some(id);
        self.panel_opened_at = None;
        self.focus.focus(window);
    }

    fn theme(&self) -> UiTheme {
        let is_dark = match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        };
        UiTheme::new(is_dark)
    }

    fn auto_save(&mut self, cx: &mut Context<Self>) {
        self.widget_runtime.tick(&self.settings);
        if self.save_pending {
            return;
        }
        self.save_pending = true;
        cx.spawn(async move |this, cx| {
            // Slider drags and toggle bursts can fire dozens of times; persist once at the end.
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.save_pending {
                    view.save_pending = false;
                    view.sync_dock_pins();
                    view.save_settings(cx);
                }
            });
        })
        .detach();
    }

    // ---- Settings search (dynamic catalogue) --------------------------------

    fn render_search(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let query = if self.search_query.is_empty() {
            "Search settings, widget panels, DockTopGapPixels, LauncherHotkey…".to_owned()
        } else {
            self.search_query.clone()
        };
        let border = if self.search_focused {
            t.accent
        } else {
            t.border
        };
        let field = div()
            .id("settings-search-input")
            .tab_index(0)
            .w_full()
            .h(px(34.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded(px(9.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(border))
            .focus(|style| style.border_color(rgba(t.accent)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.search_focused = true;
                this.focus.focus(window);
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let mut buffer = this.search_query.clone();
                if apply_keystroke(
                    &mut buffer,
                    &event.keystroke.key,
                    event.keystroke.key_char.as_deref(),
                    false,
                ) {
                    this.search_query = buffer;
                    this.refresh_search();
                } else if event.keystroke.key == "escape" {
                    this.search_query.clear();
                    this.search_hits.clear();
                }
                cx.notify();
            }))
            .child(icon_glyph("\u{E721}", 14.0, t.text_secondary))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.0))
                    .text_color(rgba(if self.search_query.is_empty() {
                        t.text_secondary
                    } else {
                        t.text_primary
                    }))
                    .child(query),
            )
            .when(!self.search_query.is_empty(), |row| {
                row.child(icon_glyph("\u{E711}", 12.0, t.text_secondary))
            });

        let mut out = vec![field.into_any_element()];
        if !self.search_query.is_empty() {
            let count = self.search_hits.len();
            out.push(
                div()
                    .w_full()
                    .text_size(px(11.0))
                    .text_color(rgba(t.text_secondary))
                    .child(format!("{count} matches"))
                    .into_any_element(),
            );
            let hover_bg = t.surface_hover;
            for (i, hit) in self.search_hits.iter().take(40).enumerate() {
                let page = hit.page;
                let widget_id = hit.widget_id;
                let label = hit.label.clone();
                let section = hit.section.to_owned();
                let field_name = hit.field.to_owned();
                let row = div()
                    .id(SharedString::from(format!("search-hit-{i}")))
                    .w_full()
                    .min_w_0()
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .bg(rgba(t.surface))
                    .border_1()
                    .border_color(rgba(t.border))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgba(hover_bg)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.page = SettingsPage::from_name(page);
                        this.search_query.clear();
                        this.search_hits.clear();
                        this.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
                        if let Some(id) = widget_id
                            && widget_has_settings(id)
                        {
                            this.open_widget_panel(id, window);
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.0))
                            .text_color(rgba(t.text_primary))
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(10.0))
                            .text_color(rgba(t.text_secondary))
                            .child(format!("{section} · {field_name}")),
                    );
                out.push(row.into_any_element());
            }
        }
        out
    }

    // ---- WidgetPanelHost -----------------------------------------------------

    fn preview_flyout(&self, id: &str) {
        if !self.flyout_preview {
            return;
        }
        use crate::ui::flyout::{FlyoutBlock, FlyoutSpec, show_flyout};
        let spec = FlyoutSpec {
            title: format!("{id} preview"),
            subtitle: Some("Live C2 flyout blocks".to_owned()),
            width_dips: 320.0,
            blocks: vec![
                FlyoutBlock::Hero {
                    value: "Preview".to_owned(),
                    caption: Some("C2 contract sample".to_owned()),
                    centered: false,
                    large: true,
                    right_meta: None,
                },
                FlyoutBlock::SeekSlider {
                    id: 1,
                    label: "Position".to_owned(),
                    position_secs: 42,
                    duration_secs: 180,
                    ratio: 42.0 / 180.0,
                },
                FlyoutBlock::Chart {
                    label: "Trend".to_owned(),
                    series: vec![("Live".to_owned(), vec![0.2, 0.5, 0.35, 0.8, 0.6])],
                    accent: (0x2d, 0xd4, 0xbf),
                },
                FlyoutBlock::IconBar {
                    buttons: vec![(2, "Reset".to_owned(), 0xE72C, false)],
                },
            ],
        };
        // Own modal message loop: run it off the GPUI thread to avoid nesting loops.
        std::thread::spawn(move || {
            let _ = show_flyout(0, true, 0, 0, spec);
        });
    }

    fn render_widget_panel_overlay(
        &self,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let id = self.widget_panel?;
        let accent = t.accent;
        let hover = t.surface_hover;
        Some(
            div()
                .id("widget-panel-host")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x9900_0000))
                .child(
                    div()
                        .w(px(460.0))
                        .max_w_full()
                        .p(px(18.0))
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .rounded(px(14.0))
                        .bg(rgba(t.surface))
                        .border_1()
                        .border_color(rgba(accent))
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(16.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(widgets::widget_name(id)),
                                )
                                .child(button(
                                    "btn-panel-close",
                                    t,
                                    loc::t("Close"),
                                    ButtonVariant::Secondary,
                                    cx.listener(|this, _, window, cx| {
                                        this.close_widget_panel(window);
                                        cx.notify();
                                    }),
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(fonts::scaled_size(12.0)))
                                .text_color(rgba(t.text_secondary))
                                .child(loc::t("WidgetPanelHint")),
                        )
                        .child(
                            div()
                                .id("panel-hover-region")
                                .w_full()
                                .h(px(64.0))
                                .rounded(px(10.0))
                                .bg(rgba(hover))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, _| {
                                    this.preview_flyout(this.widget_panel.unwrap_or("widget"));
                                }))
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(12.0)))
                                        .text_color(rgba(t.text_primary))
                                        .child(loc::t("WidgetPreviewFlyout")),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    // ---- Settings feature panels --------------------------------------------

    fn render_dimension_controls(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let units = ["Auto", "Pixels", "Percent"];
        let modes = ["Auto", "Fixed", "Percent"];
        let mut rows: Vec<AnyElement> = Vec::new();
        for (key, label, options) in [
            ("DockTopGapUnit", "Top Gap Unit", &units[..]),
            ("DockWidthMode", "Width Mode", &modes[..]),
            ("DockHeightMode", "Height Mode", &modes[..]),
        ] {
            let current = extra_string(&self.settings, key, options[0]);
            let dd_opts = options
                .iter()
                .map(|option| {
                    let key: &'static str = key;
                    let option: &'static str = option;
                    let entity = cx.entity().downgrade();
                    DropdownOption {
                        label: SharedString::from(option),
                        flag_path: None,
                        glyph: None,
                        selected: current == option,
                        on_select: std::rc::Rc::new(move |_win, app| {
                            let _ = entity.update(app, |this, cx| {
                                this.open_dropdown = None;
                                set_extra_string(&mut this.settings, key, option);
                                this.auto_save(cx);
                            });
                        }),
                    }
                })
                .collect();
            let dd = self.make_dropdown(format!("dim-{key}"), t, dd_opts, cx);
            rows.push(setting_row(t, Some("\u{E740}"), label, dd).into_any_element());
        }
        let weak = cx.entity().downgrade();
        for (key, label, max, unit) in [
            ("DockTopGapPixels", "Top Gap (px)", 64, "px"),
            ("DockTopGapPercent", "Top Gap (%)", 100, "%"),
            ("DockWidthPixels", "Dock Width (px)", 600, "px"),
            ("DockWidthPercent", "Dock Width (%)", 100, "%"),
            ("DockHeightPixels", "Dock Height (px)", 400, "px"),
            ("DockHeightPercent", "Dock Height (%)", 100, "%"),
        ] {
            let value = extra_number(&self.settings, key, 0);
            let text = format!("{value} {unit}");
            let skey: &'static str = key;
            let weak_for_slider = weak.clone();
            rows.push(
                setting_row(
                    t,
                    Some("\u{E8A3}"),
                    label,
                    live_slider(skey, t, value as f32 / max as f32, 16, text, move |i| {
                        let entity = weak_for_slider.clone();
                        let key = skey;
                        std::rc::Rc::new(move |_win, app| {
                            let _ = entity.update(app, |this, cx| {
                                let next = ((i as i32) * max / 16).clamp(0, max);
                                if extra_number(&this.settings, key, 0) != next {
                                    set_extra_number(&mut this.settings, key, next);
                                    this.auto_save(cx);
                                }
                            });
                        })
                    }),
                )
                .into_any_element(),
            );
        }
        rows
    }

    fn render_weather_picker(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let cities = cities_for(&self.weather_country);
        let city = cities
            .get(self.weather_city_index)
            .copied()
            .unwrap_or("Istanbul");
        let label = format!("{city}, {}", self.weather_country);

        let country_opts: Vec<DropdownOption> = COUNTRIES
            .iter()
            .map(|&c| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(c),
                    flag_path: country_flag_path(c),
                    glyph: None,
                    selected: self.weather_country.eq_ignore_ascii_case(c),
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.weather_country = c.to_owned();
                            this.weather_city_index = 0;
                            cx.notify();
                        });
                    }),
                }
            })
            .collect();

        let city_opts: Vec<DropdownOption> = cities
            .iter()
            .enumerate()
            .map(|(idx, &c)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(c),
                    flag_path: None,
                    glyph: None,
                    selected: self.weather_city_index == idx,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.weather_city_index = idx;
                            cx.notify();
                        });
                    }),
                }
            })
            .collect();

        let unit_opts: Vec<DropdownOption> = [
            (TemperatureUnit::Celsius, "°C"),
            (TemperatureUnit::Fahrenheit, "°F"),
        ]
        .into_iter()
        .map(|(unit, lbl)| {
            let entity = cx.entity().downgrade();
            DropdownOption {
                label: SharedString::from(lbl),
                flag_path: None,
                glyph: None,
                selected: self.settings.weather_unit == unit,
                on_select: std::rc::Rc::new(move |_win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        this.settings.weather_unit = unit;
                        this.auto_save(cx);
                    });
                }),
            }
        })
        .collect();

        vec![
            setting_row(
                t,
                Some("\u{E774}"),
                loc::t("WeatherCountry"),
                self.make_dropdown("weather-country", t, country_opts, cx),
            )
            .into_any_element(),
            setting_row(
                t,
                Some("\u{E707}"),
                loc::t("WeatherCityProvince"),
                self.make_dropdown("weather-city", t, city_opts, cx),
            )
            .into_any_element(),
            self.target_row(
                "WeatherUnit",
                t,
                Some("\u{E9CA}"),
                loc::t("WeatherUnit"),
                self.make_dropdown("weather-unit", t, unit_opts, cx),
            ),
            setting_row_desc(
                t,
                Some("\u{E81D}"),
                loc::t("WeatherLocation"),
                label,
                button(
                    "btn-weather-apply",
                    t,
                    loc::t("Apply"),
                    ButtonVariant::Primary,
                    cx.listener(|this, _, _, cx| {
                        let cities = cities_for(&this.weather_country);
                        let city = cities
                            .get(this.weather_city_index)
                            .copied()
                            .unwrap_or("Istanbul");
                        this.settings.weather_city = format!("{city}, {}", this.weather_country);
                        this.auto_save(cx);
                    }),
                ),
            )
            .into_any_element(),
        ]
    }

    fn render_hotkeys(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        for (key, label) in [
            ("LauncherHotkey", "Launcher Hotkey"),
            ("DockHotkey", "Dock Hotkey"),
        ] {
            let value = extra_string(&self.settings, key, "Ctrl+Shift+Space");
            let capturing = self.hotkey_capture == Some(key);
            rows.push(
                setting_row(
                    t,
                    Some("\u{E765}"),
                    label,
                    button(
                        SharedString::from(format!("btn-capture-{key}")),
                        t,
                        if capturing {
                            "Press keys…".to_owned()
                        } else {
                            value
                        },
                        ButtonVariant::Secondary,
                        cx.listener(move |this, _, _, cx| {
                            this.hotkey_capture = Some(key);
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element(),
            );
        }
        rows
    }

    fn render_ai_auth(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let signed_in = self.settings.extra.contains_key("CursorAuthToken");
        vec![
            setting_row(
                t,
                Some("\u{E99A}"),
                "Cursor Account",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(badge(t, self.ai_status.clone(), signed_in))
                    .child(button(
                        "btn-cursor-auth",
                        t,
                        if signed_in { "Sign Out" } else { "Sign In" },
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            // WS-G `platform::cursor_auth` is not present in this worktree yet.
                            // Keep the UI state + token slot; the provider call is wired at merge.
                            if this.settings.extra.contains_key("CursorAuthToken") {
                                this.settings.extra.remove("CursorAuthToken");
                                this.ai_status = "Not signed in".to_owned();
                            } else {
                                pins::launch("https://www.cursor.com/settings".to_owned());
                                this.ai_status = "Sign-in required (provider pending)".to_owned();
                            }
                            this.save_settings(cx);
                        }),
                    )),
            )
            .into_any_element(),
        ]
    }

    fn render_backup_ui(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let status = if self.backup_status.is_empty() {
            "No backup operation yet.".to_owned()
        } else {
            self.backup_status.clone()
        };
        let mut rows: Vec<AnyElement> = vec![
            setting_row(
                t,
                Some("\u{EDE1}"),
                "Full Backup (.otbackup)",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(button(
                        "btn-backup-export",
                        t,
                        "Export",
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            this.sync_dock_pins();
                            let data = build_backup(&this.settings);
                            if let Some(path) = prompt::save_file(
                                "Export full backup",
                                "opentaskbar-backup.otbackup",
                                "OpenTaskbar backup",
                                "*.otbackup",
                            ) {
                                match std::fs::write(&path, data) {
                                    Ok(()) => {
                                        this.backup_status =
                                            format!("Exported {}", path.to_string_lossy());
                                    }
                                    Err(err) => {
                                        this.backup_status = format!("Export failed: {err}");
                                    }
                                }
                            }
                            cx.notify();
                        }),
                    ))
                    .child(button(
                        "btn-backup-inspect",
                        t,
                        "Inspect",
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            if let Some(path) = prompt::open_file(
                                "Inspect full backup",
                                "OpenTaskbar backup",
                                "*.otbackup",
                            ) {
                                match std::fs::read(&path) {
                                    Ok(data) => {
                                        let preview = inspect_backup(&data);
                                        this.backup_status = if preview.valid {
                                            format!("{}: valid backup", path.to_string_lossy())
                                        } else {
                                            format!(
                                                "{}: {}",
                                                path.to_string_lossy(),
                                                preview.error.clone().unwrap_or_default()
                                            )
                                        };
                                        this.backup_preview = Some(preview);
                                    }
                                    Err(err) => {
                                        this.backup_status = format!("Read failed: {err}");
                                    }
                                }
                            }
                            cx.notify();
                        }),
                    ))
                    .child(button(
                        "btn-backup-import",
                        t,
                        "Import",
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            if let Some(path) = prompt::open_file(
                                "Import full backup",
                                "OpenTaskbar backup",
                                "*.otbackup",
                            ) {
                                match std::fs::read(&path)
                                    .map_err(|e| e.to_string())
                                    .and_then(|data| import_backup(&data))
                                {
                                    Ok(settings) => {
                                        this.settings = settings;
                                        this.backup_status =
                                            format!("Imported {}", path.to_string_lossy());
                                        this.save_settings(cx);
                                        notify_docks_reload();
                                    }
                                    Err(err) => {
                                        this.backup_status = format!("Import failed: {err}");
                                    }
                                }
                            }
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element(),
            div()
                .w_full()
                .px(px(16.0))
                .pb(px(8.0))
                .text_size(px(11.0))
                .text_color(rgba(t.text_secondary))
                .child(status)
                .into_any_element(),
        ];
        if let Some(preview) = &self.backup_preview
            && preview.valid
        {
            rows.push(
                div()
                    .w_full()
                    .px(px(16.0))
                    .pb(px(8.0))
                    .text_size(px(11.0))
                    .text_color(rgba(t.text_primary))
                    .child(format!(
                        "Pins: {} · Theme files: {} · Weather: {}",
                        preview.pinned_apps,
                        preview.theme_files,
                        preview
                            .settings
                            .as_ref()
                            .map(|s| s.weather_city.as_str())
                            .unwrap_or("—")
                    ))
                    .into_any_element(),
            );
        }
        rows
    }

    fn render_folder_stacks(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows: Vec<AnyElement> = vec![
            setting_row(
                t,
                Some("\u{E8B7}"),
                "Add Folder Stack",
                button(
                    "btn-add-stack",
                    t,
                    "Choose folder…",
                    ButtonVariant::Secondary,
                    cx.listener(|this, _, _, cx| {
                        if let Some(path) = prompt::open_folder("Choose a folder stack") {
                            let path = path.to_string_lossy().into_owned();
                            if !this.settings.stack_folders.contains(&path) {
                                this.settings.stack_folders.push(path);
                                this.auto_save(cx);
                            }
                        }
                        cx.notify();
                    }),
                ),
            )
            .into_any_element(),
        ];
        for (i, folder) in self.settings.stack_folders.iter().enumerate() {
            let label = std::path::Path::new(folder)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            rows.push(
                setting_row(
                    t,
                    Some("\u{E8B7}"),
                    label,
                    button(
                        SharedString::from(format!("btn-remove-stack-{i}")),
                        t,
                        "Remove",
                        ButtonVariant::Destructive,
                        cx.listener(move |this, _, _, cx| {
                            if i < this.settings.stack_folders.len() {
                                this.settings.stack_folders.remove(i);
                                this.auto_save(cx);
                            }
                        }),
                    ),
                )
                .into_any_element(),
            );
        }
        rows
    }

    fn render_theme_gallery(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let active = extra_string(&self.settings, "ThemePack", "catppuccin");
        let mut cards = div().w_full().flex().flex_wrap().gap(px(10.0));
        for (id, name, bg, primary, secondary) in THEME_GALLERY {
            let c = |color: (u8, u8, u8)| crate::design::gpui_rgba(color, 0xff);
            let selected = active == *id;
            let border = if selected { t.accent } else { t.border };
            let sid: &'static str = id;
            let card = div()
                .id(SharedString::from(format!("theme-{id}")))
                .w(px(132.0))
                .p(px(10.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .rounded(px(10.0))
                .bg(rgba(t.surface))
                .border_1()
                .border_color(rgba(border))
                .cursor_pointer()
                .hover(move |s| s.bg(rgba(t.surface_hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    set_extra_string(&mut this.settings, "ThemePack", sid);
                    this.auto_save(cx);
                }))
                .child(
                    div()
                        .h(px(44.0))
                        .w_full()
                        .rounded(px(7.0))
                        .bg(rgba(c(*bg)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .w(px(14.0))
                                .h(px(14.0))
                                .rounded(px(4.0))
                                .bg(rgba(c(*primary))),
                        )
                        .child(
                            div()
                                .w(px(14.0))
                                .h(px(14.0))
                                .rounded(px(4.0))
                                .bg(rgba(c(*secondary))),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .text_size(px(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgba(t.text_primary))
                        .child(*name),
                );
            cards = cards.child(card);
        }
        vec![
            section_card(
                t,
                "Theme Gallery (bundled swatches)",
                vec![cards.into_any_element()],
            )
            .into_any_element(),
        ]
    }

    fn render_theme_editor(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let category = THEME_TOKEN_CATEGORIES
            .get(self.theme_category)
            .copied()
            .unwrap_or("Advanced");
        let cat_opts: Vec<DropdownOption> = THEME_TOKEN_CATEGORIES
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(*name),
                    flag_path: None,
                    glyph: None,
                    selected: self.theme_category == i,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.theme_category = i;
                            cx.notify();
                        });
                    }),
                }
            })
            .collect();
        let seg = self.make_dropdown("token-cat", t, cat_opts, cx);
        let tokens: Map<String, Value> = self
            .settings
            .extra
            .get("ThemeTokens")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut rows = vec![
            setting_row(t, Some("\u{E790}"), "Token Category", seg).into_any_element(),
            setting_row(
                t,
                Some("\u{E790}"),
                "Token Name",
                div()
                    .id("token-name-input")
                    .tab_index(0)
                    .w(px(180.0))
                    .h(px(28.0))
                    .px(px(8.0))
                    .rounded(px(7.0))
                    .bg(rgba(t.surface_muted))
                    .border_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .items_center()
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        let mut buffer = this.token_name.clone();
                        if apply_keystroke(
                            &mut buffer,
                            &event.keystroke.key,
                            event.keystroke.key_char.as_deref(),
                            false,
                        ) {
                            this.token_name = buffer;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .child(if self.token_name.is_empty() {
                                "e.g. SurfaceStroke".to_owned()
                            } else {
                                self.token_name.clone()
                            }),
                    ),
            )
            .into_any_element(),
            setting_row(
                t,
                Some("\u{E790}"),
                "Token Value (#RRGGBB)",
                div()
                    .id("token-value-input")
                    .tab_index(0)
                    .w(px(180.0))
                    .h(px(28.0))
                    .px(px(8.0))
                    .rounded(px(7.0))
                    .bg(rgba(t.surface_muted))
                    .border_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .items_center()
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        let mut buffer = this.token_value.clone();
                        if apply_keystroke(
                            &mut buffer,
                            &event.keystroke.key,
                            event.keystroke.key_char.as_deref(),
                            true,
                        ) {
                            this.token_value = buffer;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .child(if self.token_value.is_empty() {
                                "38BDF8".to_owned()
                            } else {
                                self.token_value.clone()
                            }),
                    ),
            )
            .into_any_element(),
            setting_row(
                t,
                Some("\u{E74E}"),
                "Save Token",
                button(
                    "btn-save-token",
                    t,
                    "Save",
                    ButtonVariant::Secondary,
                    cx.listener(|this, _, _, cx| {
                        let name = this.token_name.trim().to_owned();
                        let raw = this.token_value.trim().trim_start_matches('#').to_owned();
                        let mut numeric = u32::from_str_radix(&raw, 16)
                            .unwrap_or(0x38_BD_F8)
                            .to_string();
                        clamp_numeric(&mut numeric, 0.0, u32::MAX as f64);
                        let value = format!("{:06X}", numeric.parse::<u32>().unwrap_or(0x38_BD_F8));
                        if !name.is_empty() {
                            let mut map = this
                                .settings
                                .extra
                                .get("ThemeTokens")
                                .and_then(Value::as_object)
                                .cloned()
                                .unwrap_or_default();
                            map.insert(name.clone(), json!(format!("#{value}")));
                            this.settings
                                .extra
                                .insert("ThemeTokens".to_owned(), Value::Object(map));
                            this.token_name.clear();
                            this.token_value.clear();
                            this.auto_save(cx);
                        }
                    }),
                ),
            )
            .into_any_element(),
        ];
        let mut saved: Vec<(String, String)> = tokens
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect();
        saved.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, value) in saved.into_iter().take(24) {
            rows.push(
                setting_row(
                    t,
                    Some("\u{E790}"),
                    format!("{category} · {name}"),
                    div()
                        .text_size(px(12.0))
                        .text_color(rgba(t.text_secondary))
                        .child(value),
                )
                .into_any_element(),
            );
        }
        vec![section_card(t, format!("Theme Editor · {category}"), rows).into_any_element()]
    }

    fn render_widget_preview(&self, id: &str, t: &UiTheme, is_dark: bool) -> AnyElement {
        let pal = crate::design::palette(is_dark);
        let c_rgba = |c: (u8, u8, u8), a: u8| rgba(crate::design::gpui_rgba(c, a));
        let chip_bg = if is_dark { 0xffffff10 } else { 0x00000008 };
        let chip_bd = if is_dark { 0xffffff1c } else { 0x00000016 };
        let track_bg = if is_dark { 0xffffff1f } else { 0x00000018 };
        let spec = match id {
            "weather" => {
                let (temp, range) = match self.settings.weather_unit {
                    TemperatureUnit::Celsius => ("21°C", "24°C / 16°C"),
                    TemperatureUnit::Fahrenheit => ("70°F", "75°F / 61°F"),
                };
                WidgetChipSpec::Weather {
                    available: true,
                    icon_color: pal.warning,
                    is_rain: false,
                    temp: temp.to_owned(),
                    place: if self.settings.weather_city.is_empty() {
                        "Istanbul".to_owned()
                    } else {
                        self.settings.weather_city.clone()
                    },
                    condition: loc::t("WeatherClear"),
                    today_range: Some(range.to_owned()),
                }
            }
            "stats" => {
                let mut cols = Vec::with_capacity(4);
                if self.settings.stats_meters & 1 != 0 {
                    cols.push(SystemStatColumn {
                        label: "CPU".to_owned(),
                        value_text: "24%".to_owned(),
                        ratio: 0.24,
                        bar_color: pal.widget_blue,
                        warm_text: false,
                    });
                }
                if self.settings.stats_meters & 2 != 0 {
                    cols.push(SystemStatColumn {
                        label: "RAM".to_owned(),
                        value_text: "58%".to_owned(),
                        ratio: 0.58,
                        bar_color: pal.widget_accent,
                        warm_text: false,
                    });
                }
                if self.settings.stats_meters & 16 != 0 {
                    cols.push(SystemStatColumn {
                        label: "GPU".to_owned(),
                        value_text: "38%".to_owned(),
                        ratio: 0.38,
                        bar_color: pal.success,
                        warm_text: false,
                    });
                }
                if self.settings.stats_meters & 32 != 0 {
                    let (lbl, val) = match self.settings.weather_unit {
                        TemperatureUnit::Celsius => ("GPU °C", "64°C"),
                        TemperatureUnit::Fahrenheit => ("GPU °F", "147°F"),
                    };
                    cols.push(SystemStatColumn {
                        label: lbl.to_owned(),
                        value_text: val.to_owned(),
                        ratio: 0.64,
                        bar_color: pal.warning,
                        warm_text: true,
                    });
                }
                WidgetChipSpec::SystemStats { columns: cols }
            }
            "netspeed" => WidgetChipSpec::NetSpeed {
                down: "18.4 MB/s".to_owned(),
                up: "3.2 MB/s".to_owned(),
                down_history: vec![0.34, 0.48, 0.40, 0.62, 0.54, 0.73, 0.68, 0.80, 0.74, 0.78],
                up_history: vec![0.14, 0.22, 0.18, 0.28, 0.24, 0.32, 0.29, 0.36, 0.31, 0.33],
            },
            "battery" => WidgetChipSpec::BatteryLiquid {
                present: true,
                percent: 84,
                charging: true,
                wave_phase: 0.35,
            },
            "bluetoothbattery" => WidgetChipSpec::Bluetooth {
                devices: vec![
                    (
                        widgets::telemetry::BluetoothDeviceKind::Headphones,
                        85,
                        false,
                        false,
                    ),
                    (
                        widgets::telemetry::BluetoothDeviceKind::Mouse,
                        64,
                        false,
                        false,
                    ),
                ],
            },
            "media" => WidgetChipSpec::Media {
                title: "Midnight City".to_owned(),
                artist: "M83".to_owned(),
                muted: false,
            },
            "audio" => WidgetChipSpec::Audio {
                volume: 68,
                muted: false,
            },
            "stopwatch" => WidgetChipSpec::Timed {
                is_focus: false,
                running: true,
                text: "04:18".to_owned(),
            },
            "countdown" => WidgetChipSpec::Timed {
                is_focus: false,
                running: false,
                text: "12:30".to_owned(),
            },
            "focus" => WidgetChipSpec::Timed {
                is_focus: true,
                running: true,
                text: "21:45".to_owned(),
            },
            "aiusage_claude" => WidgetChipSpec::AiDual {
                provider_id: "aiusage_claude",
                brand_color: Some((0xd9, 0x77, 0x57)),
                rows: vec![
                    ("5h".to_owned(), 0.42, pal.widget_accent, "2h".to_owned()),
                    ("7d".to_owned(), 0.68, pal.widget_accent, "4d".to_owned()),
                ],
            },
            "aiusage_codex" => WidgetChipSpec::AiDual {
                provider_id: "aiusage_codex",
                brand_color: Some((0x8d, 0x99, 0xff)),
                rows: vec![
                    ("5h".to_owned(), 0.28, pal.widget_accent, "3h".to_owned()),
                    ("7d".to_owned(), 0.54, pal.widget_accent, "5d".to_owned()),
                ],
            },
            "aiusage_cursor" => WidgetChipSpec::AiDual {
                provider_id: "aiusage_cursor",
                brand_color: None,
                rows: vec![("30d".to_owned(), 0.61, pal.widget_accent, "12d".to_owned())],
            },
            "aiusage_antigravity" => WidgetChipSpec::AiAntigravity {
                columns: vec![
                    (
                        "Gemini".to_owned(),
                        vec![
                            ("5h".to_owned(), 0.42, pal.widget_accent, "2h".to_owned()),
                            ("7d".to_owned(), 0.68, pal.widget_accent, "4d".to_owned()),
                        ],
                    ),
                    (
                        "Claude".to_owned(),
                        vec![("7d".to_owned(), 0.54, pal.widget_blue, "3d".to_owned())],
                    ),
                    (
                        "Argon".to_owned(),
                        vec![("7d".to_owned(), 0.31, pal.success, "6d".to_owned())],
                    ),
                ],
            },
            "aiusage_opencode" => WidgetChipSpec::AiOpenCode {
                columns: vec![
                    (
                        "1h".to_owned(),
                        "18%".to_owned(),
                        0.18,
                        pal.success,
                        "42m".to_owned(),
                    ),
                    (
                        "7d".to_owned(),
                        "49%".to_owned(),
                        0.49,
                        pal.success,
                        "3d".to_owned(),
                    ),
                    (
                        "30d".to_owned(),
                        "64%".to_owned(),
                        0.64,
                        pal.success,
                        "11d".to_owned(),
                    ),
                ],
            },
            "aiusage_copilot" => WidgetChipSpec::AiCopilot {
                value_text: "36%".to_owned(),
                meta_text: "14d left".to_owned(),
                used: 0.36,
                bar_color: (0x9f, 0x9f, 0xf2),
            },
            "aiusage_deepseek" => WidgetChipSpec::AiBalance {
                provider_id: "aiusage_deepseek",
                brand_color: (0x4d, 0x6b, 0xfe),
                name: "DeepSeek",
                value_text: "$14.80".to_owned(),
            },
            "aiusage_openrouter" => WidgetChipSpec::AiBalance {
                provider_id: "aiusage_openrouter",
                brand_color: (0xb7, 0x9b, 0xf8),
                name: "OpenRouter",
                value_text: "$22.40".to_owned(),
            },
            _ => self.widget_runtime.chip_spec(id, &self.settings, is_dark),
        };

        let chip = |inner: gpui::Div| -> AnyElement {
            div()
                .h(px(44.0))
                .max_w_full()
                .px(px(11.0))
                .rounded(px(12.0))
                .bg(rgba(chip_bg))
                .border_1()
                .border_color(rgba(chip_bd))
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .child(inner)
                .into_any_element()
        };

        let mini_bar = |ratio: f32, col: (u8, u8, u8), bar_w: f32| {
            let fill_w = (bar_w * ratio.clamp(0.0, 1.0))
                .round()
                .max(if ratio > 0.01 { 3.0 } else { 0.0 });
            div()
                .w(px(bar_w))
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(rgba(track_bg))
                .overflow_hidden()
                .child(
                    div()
                        .w(px(fill_w))
                        .h_full()
                        .rounded(px(2.0))
                        .bg(c_rgba(col, 0xff)),
                )
        };

        let vdiv = || {
            div()
                .w(px(1.0))
                .h(px(18.0))
                .bg(rgba(t.separator))
                .flex_shrink_0()
        };

        match spec {
            WidgetChipSpec::Clock {
                time,
                date,
                weekday,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_glyph("\u{E823}", 13.0, t.dock_preview_sub))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(14.0)))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(time),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(10.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(date),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(weekday),
                            ),
                    ),
            ),
            WidgetChipSpec::Weather {
                available,
                icon_color,
                is_rain,
                temp,
                place,
                condition,
                today_range,
            } => {
                if !available {
                    chip(
                        div()
                            .text_size(px(fonts::scaled_size(11.0)))
                            .text_color(rgba(t.dock_preview_sub))
                            .child(condition),
                    )
                } else {
                    let mut row = div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon_glyph(
                            if is_rain { "\u{E9C4}" } else { "\u{E753}" },
                            15.0,
                            crate::design::gpui_rgba(icon_color, 0xff),
                        ))
                        .child(
                            div()
                                .text_size(px(fonts::scaled_size(15.0)))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgba(t.dock_preview_fg))
                                .child(temp),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(10.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(place),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(condition),
                                ),
                        );
                    if let Some(range) = today_range {
                        row = row.child(vdiv()).child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(loc::t("Today")),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(10.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(range),
                                ),
                        );
                    }
                    chip(row)
                }
            }
            WidgetChipSpec::DayProgress {
                ratio,
                percent,
                unit,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(
                        div()
                            .size(px(18.0))
                            .rounded(px(9.0))
                            .border_2()
                            .border_color(c_rgba(pal.widget_blue, 0xff))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .size(px((8.0 * ratio.clamp(0.25, 1.0)).round()))
                                    .rounded(px(4.0))
                                    .bg(c_rgba(pal.widget_blue, 0xaa)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(percent),
                    )
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(10.0)))
                            .text_color(rgba(t.dock_preview_sub))
                            .child(unit),
                    ),
            ),
            WidgetChipSpec::Timed {
                is_focus,
                running,
                text,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_glyph(
                        if is_focus { "\u{E7C8}" } else { "\u{E916}" },
                        13.0,
                        if running {
                            crate::design::gpui_rgba(pal.selected_fg, 0xff)
                        } else {
                            t.dock_preview_sub
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(text),
                    )
                    .child(
                        div()
                            .size(px(22.0))
                            .rounded(px(6.0))
                            .bg(rgba(if running {
                                crate::design::gpui_rgba(pal.selected_fill, 0xff)
                            } else {
                                track_bg
                            }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_glyph(
                                if running { "\u{E769}" } else { "\u{E768}" },
                                10.0,
                                if running {
                                    crate::design::gpui_rgba(pal.selected_fg, 0xff)
                                } else {
                                    t.dock_preview_fg
                                },
                            )),
                    ),
            ),
            WidgetChipSpec::SystemStats { columns } => {
                if columns.is_empty() {
                    chip(
                        div()
                            .text_size(px(fonts::scaled_size(11.0)))
                            .text_color(rgba(t.dock_preview_sub))
                            .child("—"),
                    )
                } else {
                    let warm_col = if is_dark {
                        crate::design::gpui_rgba(pal.warning, 0xff)
                    } else {
                        0xb45309ff
                    };
                    let mut row = div().flex().items_center().gap(px(9.0));
                    for (idx, col) in columns.into_iter().take(4).enumerate() {
                        if idx > 0 {
                            row = row.child(vdiv());
                        }
                        let fill_w = (36.0 * col.ratio.clamp(0.0, 1.0)).round();
                        row = row.child(
                            div()
                                .min_w(px(36.0))
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(col.label),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(12.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(if col.warm_text {
                                            warm_col
                                        } else {
                                            t.dock_preview_fg
                                        }))
                                        .child(col.value_text),
                                )
                                .child(
                                    div()
                                        .w(px(36.0))
                                        .h(px(2.0))
                                        .rounded(px(1.0))
                                        .bg(rgba(track_bg))
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .w(px(fill_w))
                                                .h_full()
                                                .rounded(px(1.0))
                                                .bg(c_rgba(col.bar_color, 0xff)),
                                        ),
                                ),
                        );
                    }
                    chip(row)
                }
            }
            WidgetChipSpec::NetSpeed {
                down,
                up,
                down_history,
                up_history: _,
            } => {
                let split_speed = |s: &str| -> (String, String) {
                    s.rsplit_once(' ')
                        .map_or_else(|| (s.to_owned(), String::new()), |(n, u)| (n.to_owned(), u.to_owned()))
                };
                let (d_num, d_unit) = split_speed(&down);
                let (u_num, u_unit) = split_speed(&up);
                let spark = div()
                    .w(px(36.0))
                    .h(px(22.0))
                    .flex()
                    .items_end()
                    .gap(px(1.5))
                    .children(down_history.into_iter().take(10).map(|v| {
                        let bh = (20.0 * v.clamp(0.12, 1.0)).round().max(2.0);
                        div()
                            .flex_1()
                            .h(px(bh))
                            .rounded_t(px(1.0))
                            .bg(c_rgba(pal.success, 0xc8))
                    }));
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .child(spark)
                        .child(vdiv())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(4.0))
                                        .child(icon_glyph(
                                            "\u{E74B}",
                                            9.0,
                                            crate::design::gpui_rgba(pal.success, 0xff),
                                        ))
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(11.0)))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgba(t.dock_preview_fg))
                                                .child(d_num),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(9.5)))
                                                .text_color(rgba(t.dock_preview_sub))
                                                .child(d_unit),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(4.0))
                                        .child(icon_glyph(
                                            "\u{E74A}",
                                            9.0,
                                            crate::design::gpui_rgba(pal.widget_blue, 0xff),
                                        ))
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(11.0)))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgba(t.dock_preview_fg))
                                                .child(u_num),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(9.5)))
                                                .text_color(rgba(t.dock_preview_sub))
                                                .child(u_unit),
                                        ),
                                ),
                        ),
                )
            }
            WidgetChipSpec::BatteryLiquid {
                present,
                percent,
                charging,
                ..
            } => {
                if !present {
                    chip(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(icon_glyph("\u{E850}", 14.0, t.dock_preview_sub))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child("—"),
                            ),
                    )
                } else {
                    let ratio = (percent as f32 / 100.0).clamp(0.04, 1.0);
                    let fill_col = if percent <= 20 {
                        (0xec, 0x5e, 0x6c)
                    } else if percent <= 50 {
                        (0xf2, 0xb8, 0x4e)
                    } else {
                        (0x34, 0xbf, 0xa3)
                    };
                    let chamber_bg = if is_dark { 0x0000_0048 } else { 0x1818_1b16 };
                    let chamber_bd = if is_dark { 0xffff_ff1f } else { 0x1818_1b1a };
                    let nub_bg = if percent >= 98 {
                        crate::design::gpui_rgba(fill_col, 0xf2)
                    } else if is_dark {
                        0xffff_ff3d
                    } else {
                        0x1818_1b38
                    };
                    let chamber_w = 82.0_f32;
                    let fill_w = (chamber_w * ratio).round();
                    let use_dark_ink = !is_dark && percent < 50;
                    let text_col = if use_dark_ink {
                        t.text_primary
                    } else {
                        0xffff_ffff
                    };
                    div()
                        .w(px(96.0))
                        .h(px(42.0))
                        .p(px(4.0))
                        .rounded(px(12.0))
                        .bg(rgba(chip_bg))
                        .border_1()
                        .border_color(rgba(chip_bd))
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .child(
                            div()
                                .relative()
                                .w(px(chamber_w))
                                .h_full()
                                .rounded(px(8.0))
                                .bg(rgba(chamber_bg))
                                .border_1()
                                .border_color(rgba(chamber_bd))
                                .overflow_hidden()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top_0()
                                        .bottom_0()
                                        .w(px(fill_w))
                                        .bg(c_rgba(fill_col, 0xf2)),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .flex()
                                        .items_center()
                                        .gap(px(3.0))
                                        .when(charging, |row| {
                                            row.child(icon_glyph("\u{E945}", 11.0, text_col))
                                        })
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(13.0)))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgba(text_col))
                                                .child(format!("{percent}%")),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .w(px(2.5))
                                .h(px(12.0))
                                .rounded(px(1.5))
                                .bg(rgba(nub_bg)),
                        )
                        .into_any_element()
                }
            }
            WidgetChipSpec::Bluetooth { devices } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .when(devices.is_empty(), |row| {
                        row.child(
                            div()
                                .text_size(px(fonts::scaled_size(11.0)))
                                .text_color(rgba(t.dock_preview_sub))
                                .child("—"),
                        )
                    })
                    .children(devices.into_iter().take(2).enumerate().map(
                        |(i, (kind, pct, low, full))| {
                            let val_col = if low {
                                crate::design::gpui_rgba(pal.warning, 0xff)
                            } else if full {
                                0x34d399ff
                            } else {
                                t.dock_preview_fg
                            };
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .when(i > 0, |el| el.child(vdiv()))
                                .child(icon_glyph(kind.fluent_glyph(), 13.0, t.dock_preview_sub))
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(11.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(val_col))
                                        .child(format!("{pct}%")),
                                )
                        },
                    )),
            ),
            WidgetChipSpec::Storage { drives } => {
                let (drv, cap) = drives
                    .first()
                    .cloned()
                    .unwrap_or_else(|| ("C:".to_owned(), "—".to_owned()));
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon_glyph("\u{EDA2}", 13.0, t.dock_preview_sub))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(10.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(drv),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(cap),
                                ),
                        ),
                )
            }
            WidgetChipSpec::Media {
                title,
                artist,
                muted,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(
                        div()
                            .max_w(px(86.0))
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(10.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(if artist.is_empty() {
                                        "Media".to_owned()
                                    } else {
                                        artist
                                    }),
                            ),
                    )
                    .child(vdiv())
                    .child(icon_glyph("\u{E892}", 10.0, t.dock_preview_fg))
                    .child(icon_glyph("\u{E768}", 11.0, t.dock_preview_fg))
                    .child(icon_glyph("\u{E893}", 10.0, t.dock_preview_fg))
                    .child(icon_glyph(
                        if muted { "\u{E74F}" } else { "\u{E767}" },
                        10.0,
                        t.dock_preview_sub,
                    )),
            ),
            WidgetChipSpec::Audio { volume, muted } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icon_glyph(
                        if muted { "\u{E74F}" } else { "\u{E767}" },
                        13.0,
                        if muted {
                            crate::design::gpui_rgba(pal.danger_fg, 0xff)
                        } else {
                            t.dock_preview_fg
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(if muted {
                                "Mute".to_owned()
                            } else {
                                format!("{volume}%")
                            }),
                    ),
            ),
            WidgetChipSpec::QuickSettings { pills } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .children(pills.into_iter().enumerate().map(|(i, (glyph, active))| {
                        let is_night = i == 2;
                        let bg = if active {
                            if is_night {
                                if is_dark { 0xfbbf2428 } else { 0xd9770624 }
                            } else if is_dark {
                                0xffffff24
                            } else {
                                0x18181b1a
                            }
                        } else {
                            0x00000000
                        };
                        let fg = if active {
                            if is_night {
                                if is_dark { 0xfcd34dff } else { 0xb45309ff }
                            } else {
                                t.dock_preview_fg
                            }
                        } else if is_dark {
                            0xf4f4f55c
                        } else {
                            0x18181b5c
                        };
                        div()
                            .size(px(30.0))
                            .rounded(px(15.0))
                            .bg(rgba(bg))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .font_family("Segoe Fluent Icons")
                                    .text_size(px(fonts::scaled_size(13.5)))
                                    .text_color(rgba(fg))
                                    .child(glyph),
                            )
                    })),
            ),
            WidgetChipSpec::Keyboard { code } => chip(
                div()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(5.0))
                    .bg(rgba(track_bg))
                    .text_size(px(fonts::scaled_size(12.0)))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgba(t.dock_preview_fg))
                    .child(code),
            ),
            WidgetChipSpec::AiDual {
                provider_id,
                brand_color,
                rows,
            } => {
                let dot_col =
                    brand_color.unwrap_or_else(|| widgets::ai_provider_color(provider_id));
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .size(px(14.0))
                                .rounded(px(4.0))
                                .bg(c_rgba(dot_col, 0x33))
                                .border_1()
                                .border_color(c_rgba(dot_col, 0xcc)),
                        )
                        .child(div().flex().flex_col().gap(px(3.0)).children(
                            rows.into_iter().take(2).map(|(lbl, used, col, reset)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .child(
                                        div()
                                            .w(px(18.0))
                                            .text_size(px(fonts::scaled_size(9.0)))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(lbl),
                                    )
                                    .child(mini_bar(used, col, 48.0))
                                    .child(
                                        div()
                                            .text_size(px(fonts::scaled_size(9.0)))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(reset),
                                    )
                            }),
                        )),
                )
            }
            WidgetChipSpec::AiAntigravity { columns } => {
                chip(div().flex().items_center().gap(px(7.0)).children(
                    columns.into_iter().take(3).map(|(header, rows)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(header),
                            )
                            .children(rows.into_iter().take(2).map(|(lbl, used, col, _)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .text_size(px(fonts::scaled_size(9.0)))
                                            .text_color(rgba(t.dock_preview_fg))
                                            .child(lbl),
                                    )
                                    .child(mini_bar(used, col, 28.0))
                            }))
                    }),
                ))
            }
            WidgetChipSpec::AiOpenCode { columns } => {
                chip(div().flex().items_center().gap(px(7.0)).children(
                    columns.into_iter().take(3).map(|(lbl, val, used, col, _)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(format!("{lbl} {val}")),
                            )
                            .child(mini_bar(used, col, 36.0))
                    }),
                ))
            }
            WidgetChipSpec::AiCopilot {
                value_text,
                meta_text,
                used,
                bar_color,
            } => chip(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(value_text),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(meta_text),
                            ),
                    )
                    .child(mini_bar(used, bar_color, 88.0)),
            ),
            WidgetChipSpec::AiBalance {
                provider_id,
                name,
                value_text,
                ..
            } => {
                let dot_col = widgets::ai_provider_color(provider_id);
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(
                            div()
                                .size(px(14.0))
                                .rounded(px(4.0))
                                .bg(c_rgba(dot_col, 0x33))
                                .border_1()
                                .border_color(c_rgba(dot_col, 0xcc)),
                        )
                        .child(vdiv())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(11.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(value_text),
                                ),
                        ),
                )
            }
            WidgetChipSpec::Simple {
                icon_color,
                top,
                bottom,
                ..
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_glyph(
                        widget_glyph(id),
                        13.0,
                        crate::design::gpui_rgba(icon_color, 0xff),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(top),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(bottom),
                            ),
                    ),
            ),
        }
    }

    fn sync_dock_pins(&mut self) {
        let saved = settings::load_from(&settings::settings_path());
        self.settings.user_pinned_apps = saved.user_pinned_apps;
        self.settings.user_unpinned_apps = saved.user_unpinned_apps;
        self.settings.user_pinned_background_apps = saved.user_pinned_background_apps;
        self.settings.pin_order = saved.pin_order;
    }

    /// Flushes a debounced write before the window goes away so a quick close cannot lose changes.
    fn flush_pending_save(&mut self, cx: &mut Context<Self>) {
        if self.save_pending {
            self.save_pending = false;
            self.sync_dock_pins();
            self.save_settings(cx);
        }
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        self.save_failed = settings::save_to(&settings::settings_path(), &self.settings).is_err();
        if !self.save_failed {
            notify_docks_reload();
        }
        cx.notify();
    }

    fn window_hwnd(window: &Window) -> usize {
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return 0;
        };
        match handle.as_raw() {
            RawWindowHandle::Win32(raw) => raw.hwnd.get() as usize,
            _ => 0,
        }
    }

    fn is_dark(&self) -> bool {
        match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        }
    }

    fn themes_root() -> std::path::PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("OpenTaskbar")
            .join("Themes")
    }

    fn import_theme(&mut self, cx: &mut Context<Self>) {
        let title = loc::t("ThemePackChooseFile");
        let label = loc::t("ThemePackImport");
        let Some(path) = prompt::ask_name(0, self.is_dark(), &title, &label, "") else {
            return;
        };
        let archive = std::path::PathBuf::from(path.trim());
        if !archive.is_file() {
            self.theme_status = loc::t("ThemePackMissing");
            cx.notify();
            return;
        }
        self.theme_status = match theme_packs::import_theme(&Self::themes_root(), &archive) {
            Ok(id) => loc::t_fmt("ThemePackImported", &[&id]),
            Err(error) => error,
        };
        cx.notify();
    }

    fn export_theme(&mut self, id: &str, cx: &mut Context<Self>) {
        let title = loc::t("ThemePackSaveFile");
        let label = loc::t("ThemePackExport");
        let Some(path) = prompt::ask_name(0, self.is_dark(), &title, &label, "") else {
            return;
        };
        self.theme_status = match theme_packs::export_theme(
            &Self::themes_root(),
            id,
            std::path::Path::new(path.trim()),
        ) {
            Ok(()) => loc::t("ThemePackExported"),
            Err(error) => error,
        };
        cx.notify();
    }

    fn delete_theme(&mut self, id: &str, cx: &mut Context<Self>) {
        self.theme_status = match theme_packs::delete_theme(&Self::themes_root(), id) {
            Ok(()) => loc::t("ThemePackDeleted"),
            Err(error) => error,
        };
        cx.notify();
    }

    fn record_hotkey(&mut self, launcher: bool, cx: &mut Context<Self>) {
        let title = loc::t(if launcher {
            "LauncherHotkey"
        } else {
            "DockHotkey"
        });
        let hint = loc::t("HotkeyCaptureHint");
        let current = if launcher {
            self.settings.launcher_binding()
        } else {
            self.settings.dock_binding()
        }
        .map(|binding| binding.display)
        .unwrap_or_else(|| loc::t("HotkeyDisabled"));
        let Some(text) = prompt::ask_name(0, self.is_dark(), &title, &hint, &current) else {
            return;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Esc") {
            return;
        }
        let value = if trimmed.eq_ignore_ascii_case("Backspace")
            || trimmed.eq_ignore_ascii_case("Disabled")
        {
            "Disabled".to_owned()
        } else {
            let Some(binding) = hotkey::parse(trimmed) else {
                self.hotkey_status = loc::t("HotkeyModifierRequired");
                cx.notify();
                return;
            };
            let other = if launcher {
                self.settings.dock_binding()
            } else {
                self.settings.launcher_binding()
            };
            if other.as_ref() == Some(&binding) {
                let other_name = if launcher {
                    "DockHotkey"
                } else {
                    "LauncherHotkey"
                };
                self.hotkey_status = loc::t_fmt("HotkeyConflict", &[&loc::t(other_name)]);
                cx.notify();
                return;
            }
            binding.display
        };
        if launcher {
            self.settings.custom_launcher_hotkey = Some(value);
        } else {
            self.settings.custom_dock_hotkey = Some(value);
        }
        self.hotkey_status.clear();
        self.auto_save(cx);
    }

    fn edit_weather_city(&mut self, cx: &mut Context<Self>) {
        let title = loc::t("WeatherCity");
        let hint = loc::t("WeatherCityHint");
        let current = self.settings.weather_city.clone();
        let Some(text) = prompt::ask_name(0, self.is_dark(), &title, &hint, &current) else {
            return;
        };
        let query = text.trim().to_owned();
        if query.is_empty() {
            self.weather_suggestions.clear();
            self.weather_search_status.clear();
            return;
        }
        let lang = self.settings.language.clone();
        self.weather_suggestions.clear();
        self.weather_search_status = loc::t("QuickLoading");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(
                    async move { widgets::telemetry::search_weather_locations(&query, &lang, 6) },
                )
                .await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(list) if list.len() == 1 => {
                    view.settings.weather_city = list[0].setting_value();
                    view.weather_suggestions.clear();
                    view.weather_search_status.clear();
                    view.auto_save(cx);
                }
                Ok(list) if !list.is_empty() => {
                    view.weather_suggestions = list;
                    view.weather_search_status.clear();
                    cx.notify();
                }
                Ok(_) => {
                    view.weather_suggestions.clear();
                    view.weather_search_status = loc::t("SettingsSearchEmpty");
                    cx.notify();
                }
                Err(err) => {
                    view.weather_suggestions.clear();
                    view.weather_search_status = err;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn edit_border_color(&mut self, surface: usize, cx: &mut Context<Self>) {
        let current = match surface {
            0 => self.settings.dock_border_color.clone(),
            1 => self.settings.menu_border_color.clone(),
            2 => self.settings.widget_border_color.clone(),
            _ => self.settings.settings_border_color.clone(),
        }
        .unwrap_or_default();
        let title = loc::t("BorderColorLabel");
        let hint = loc::t("BorderStyleHelp");
        let is_dark = self.is_dark();
        let Some(text) = prompt::ask_name(0, is_dark, &title, &hint, &current) else {
            return;
        };
        let text = text.trim().to_owned();
        if !text.is_empty() && parse_hex_color(&text).is_none() {
            return;
        }
        let slot = match surface {
            0 => &mut self.settings.dock_border_color,
            1 => &mut self.settings.menu_border_color,
            2 => &mut self.settings.widget_border_color,
            _ => &mut self.settings.settings_border_color,
        };
        *slot = if text.is_empty() { None } else { Some(text) };
        self.auto_save(cx);
    }

    fn edit_ai_api_key(&mut self, secret_key: &'static str, cx: &mut Context<Self>) {
        let title = loc::t("AiUsageApiKeyLabel");
        let hint = loc::t("AiUsageApiKeyPlaceholder");
        let is_dark = self.is_dark();
        let Some(text) = prompt::ask_name(0, is_dark, &title, &hint, "") else {
            return;
        };
        let value = text.trim();
        self.api_key_status = loc::t(if value.is_empty() {
            "ErrorAiUsageApiKey"
        } else if widgets::telemetry::save_dpapi_secret(secret_key, value) {
            "AiUsageApiKeySaved"
        } else {
            "ErrorAiUsageApiKey"
        });
        cx.notify();
    }

    fn clear_ai_api_key(&mut self, secret_key: &'static str, cx: &mut Context<Self>) {
        widgets::telemetry::clear_dpapi_secret(secret_key);
        self.api_key_status.clear();
        cx.notify();
    }

    /// Native paged font picker: Windows has hundreds of installed families, so the
    /// list is shown 24 at a time with Previous/Next entries instead of one huge menu.
    fn choose_font(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const PAGE_SIZE: usize = 24;
        const NATIVE_ID: u32 = 0;
        const PREV_ID: u32 = u32::MAX - 1;
        const NEXT_ID: u32 = u32::MAX;

        let fonts = fonts::installed_fonts();
        let max_page = fonts.len().saturating_sub(1) / PAGE_SIZE;
        self.font_page = self.font_page.min(max_page);
        let start = self.font_page * PAGE_SIZE;

        let mut entries = Vec::new();
        let native_label = format!("{} (Windows)", fonts::native_ui_face());
        let mut native = MenuEntry::item(NATIVE_ID, MenuIcon::Settings, native_label.clone());
        if self.settings.ui_font_family.trim().is_empty() {
            native = MenuEntry::with_shortcut(
                NATIVE_ID,
                MenuIcon::Settings,
                native_label.clone(),
                "\u{E73E}",
            );
        }
        entries.push(native);
        entries.push(MenuEntry::Separator);
        for (index, name) in fonts.iter().enumerate().skip(start).take(PAGE_SIZE) {
            let id = (index + 1) as u32;
            let mut entry = MenuEntry::item(id, MenuIcon::Settings, name.clone());
            if self.settings.ui_font_family.eq_ignore_ascii_case(name) {
                entry = MenuEntry::with_shortcut(id, MenuIcon::Settings, name.clone(), "\u{E73E}");
            }
            entries.push(entry);
        }
        entries.push(MenuEntry::Separator);
        if self.font_page > 0 {
            entries.push(MenuEntry::item(
                PREV_ID,
                MenuIcon::ChevronLeft,
                loc::t("DesktopPrevious"),
            ));
        }
        if self.font_page < max_page {
            entries.push(MenuEntry::item(
                NEXT_ID,
                MenuIcon::ChevronRight,
                loc::t("DesktopNext"),
            ));
        }

        let hwnd = Self::window_hwnd(window);
        let is_dark = self.is_dark();
        let chosen = menu::show_popup_menu_with_border(
            hwnd,
            is_dark,
            entries,
            BorderStyle::from_menu_settings(is_dark, &self.settings),
        )
        .unwrap_or(u32::MAX - 2);
        match chosen {
            NATIVE_ID => {
                self.settings.ui_font_family.clear();
                fonts::set_selected_family("");
                self.auto_save(cx);
            }
            PREV_ID => {
                self.font_page = self.font_page.saturating_sub(1);
                self.choose_font(window, cx);
            }
            NEXT_ID => {
                self.font_page = (self.font_page + 1).min(max_page);
                self.choose_font(window, cx);
            }
            id if id >= 1 && (id as usize) <= fonts.len() => {
                if let Some(name) = fonts.get(id as usize - 1) {
                    self.settings.ui_font_family = name.clone();
                    fonts::set_selected_family(name);
                    self.auto_save(cx);
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)] // merged WS-E settings row helper signature
    fn surface_row(
        &self,
        t: &UiTheme,
        cx: &mut Context<Self>,
        id: &str,
        row_id: &'static str,
        label: String,
        current: SurfaceBackground,
        apply: fn(&mut DockSettings, SurfaceBackground),
    ) -> AnyElement {
        let opts: Vec<DropdownOption> = [
            (loc::t("SurfaceGlass"), SurfaceBackground::Glass),
            (loc::t("SurfaceSolid"), SurfaceBackground::Solid),
            (loc::t("SurfaceNone"), SurfaceBackground::None),
        ]
        .into_iter()
        .map(|(text, background)| {
            let entity = cx.entity().downgrade();
            DropdownOption {
                label: SharedString::from(text),
                flag_path: None,
                glyph: None,
                selected: current == background,
                on_select: std::rc::Rc::new(move |_win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        apply(&mut this.settings, background);
                        this.auto_save(cx);
                    });
                }),
            }
        })
        .collect();
        let glyph = match id {
            "surf-dock" => "\u{E90E}",
            "surf-menu" => "\u{E700}",
            "surf-widget" => "\u{F246}",
            "surf-settings" => "\u{E713}",
            _ => "\u{E81E}",
        };
        let dd = self.make_dropdown(id, t, opts, cx);
        self.target_row(row_id, t, Some(glyph), label, dd)
    }

    fn new_profile(&mut self, cx: &mut Context<Self>) {
        let title = loc::t("ProfileNew");
        let label = loc::t("ProfileNamePrompt");
        let Some(name) = prompt::ask_name(0, true, &title, &label, "") else {
            return;
        };
        if self
            .settings
            .profiles
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(&name))
        {
            return;
        }
        self.settings
            .profiles
            .push(opentaskbar_core::settings::DockProfile {
                name: name.clone(),
                position: self.settings.position,
                size: self.settings.size,
                auto_hide: self.settings.auto_hide,
                enabled_widgets: self.settings.enabled_widgets.clone(),
                ..Default::default()
            });
        self.settings.active_profile = Some(name);
        self.auto_save(cx);
    }

    fn rename_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(profile) = self.settings.profiles.get(index) else {
            return;
        };
        let old_name = profile.name.clone();
        let title = loc::t_fmt("ProfileRename", &[&old_name]);
        let label = loc::t("ProfileRenamePrompt");
        let Some(name) = prompt::ask_name(0, true, &title, &label, &old_name) else {
            return;
        };
        if name.is_empty()
            || self
                .settings
                .profiles
                .iter()
                .enumerate()
                .any(|(i, p)| i != index && p.name.eq_ignore_ascii_case(&name))
        {
            return;
        }
        if let Some(profile) = self.settings.profiles.get_mut(index) {
            profile.name = name.clone();
        }
        if self
            .settings
            .active_profile
            .as_deref()
            .is_some_and(|a| a == old_name)
        {
            self.settings.active_profile = Some(name);
        }
        self.auto_save(cx);
    }

    fn delete_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.settings.profiles.len() <= 1 {
            return;
        }
        let Some(name) = self.settings.profiles.get(index).map(|p| p.name.clone()) else {
            return;
        };
        self.settings.profiles.remove(index);
        if self
            .settings
            .active_profile
            .as_deref()
            .is_some_and(|a| a == name)
        {
            self.settings.active_profile = self.settings.profiles.first().map(|p| p.name.clone());
        }
        self.auto_save(cx);
    }

    fn check_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_status = loc::t("QuickLoading");
        self.update_release = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { update::find_newer() })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(Some(release)) => {
                        view.update_status = loc::t_fmt("UpdateAvailable", &[&release.version]);
                        view.update_release = Some(release);
                    }
                    Ok(None) => {
                        view.update_status =
                            loc::t_fmt("UpdateCurrent", &[update::CURRENT_VERSION]);
                    }
                    Err(()) => view.update_status = loc::t("ErrorUpdateCheck"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn download_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(release) = self.update_release.clone() else {
            return;
        };
        self.update_status = loc::t_fmt("UpdateDownloading", &[&release.version]);
        cx.notify();
        let task_release = release.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { update::download_and_apply(&task_release) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(()) => {
                        let title = loc::t("UpdateAvailableTitle");
                        let body = loc::t_fmt("UpdateToastBody", &[&release.version]);
                        let action = loc::t_fmt("UpdateDownload", &[&release.version]);
                        toast::show_update_toast(&title, &body, &action, &release.url);
                        // Release this process's lock so the updater can replace the executable.
                        std::process::exit(0);
                    }
                    Err(key) => view.update_status = loc::t(key),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_dock(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let align_dd = self.make_dropdown(
            "dock-align",
            t,
            [
                (loc::t("Left"), DockPosition::Left),
                (loc::t("Center"), DockPosition::Center),
                (loc::t("Right"), DockPosition::Right),
            ]
            .into_iter()
            .map(|(label, pos)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.position == pos,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.position = pos;
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let size_dd = self.make_dropdown(
            "dock-size",
            t,
            [
                (loc::t("SizeCompact"), DockSize::Compact),
                (loc::t("SizeMedium"), DockSize::Medium),
                (loc::t("SizeLarge"), DockSize::Large),
            ]
            .into_iter()
            .map(|(label, sz)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.size == sz,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.size = sz;
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let autohide_dd = self.make_dropdown(
            "dock-autohide",
            t,
            [
                (loc::t("HotkeyDisabled"), AutoHideMode::Disabled),
                (loc::t("AutoHideSmart"), AutoHideMode::Smart),
                (loc::t("AutoHideAlways"), AutoHideMode::Always),
            ]
            .into_iter()
            .map(|(label, mode)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.auto_hide == mode,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.auto_hide = mode;
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let scale_pct = (self.settings.dock_scale * 100.0).round() as i32;
        let width_label = self.settings.dock_custom_width_dips.map_or_else(
            || loc::t("DockWidthAuto"),
            |w| format!("{} ({:.0}px)", loc::t("DockWidthPixels"), w),
        );

        let mut out = vec![
            section_card(
                t,
                format!("{} & {}", loc::t("SectionDockLayout"), loc::t("Dimensions")),
                vec![
                    self.target_row(
                        "DockAlignment",
                        t,
                        Some("\u{E8E3}"),
                        loc::t("DockAlignment"),
                        align_dd,
                    ),
                    self.target_row(
                        "DockSize",
                        t,
                        Some("\u{E9A6}"),
                        loc::t("DockSize"),
                        size_dd,
                    ),
                    self.target_row(
                        "DockScale",
                        t,
                        Some("\u{E8A3}"),
                        loc::t("DockScale"),
                        live_slider(
                            "sc",
                            t,
                            ((self.settings.dock_scale as f32) - 0.75) / 0.75,
                            16,
                            format!("{scale_pct}%"),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (0.75 + (i as f64) * 0.05).clamp(0.75, 1.50);
                                        if (this.settings.dock_scale - next).abs() > 0.001 {
                                            this.settings.dock_scale = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "DockBottomGap",
                        t,
                        Some("\u{E96F}"),
                        loc::t("DockBottomGap"),
                        live_slider(
                            "gap",
                            t,
                            self.settings.dock_bottom_gap_pixels as f32 / 32.0,
                            17,
                            format!("{} px", self.settings.dock_bottom_gap_pixels),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = ((i as i32) * 2).clamp(0, 32);
                                        if this.settings.dock_bottom_gap_pixels != next {
                                            this.settings.dock_bottom_gap_pixels = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "DockIconPadding",
                        t,
                        Some("\u{E96E}"),
                        loc::t("DockIconPadding"),
                        live_slider(
                            "pad",
                            t,
                            self.settings.dock_icon_padding as f32 / 12.0,
                            13,
                            format!("{} px", self.settings.dock_icon_padding),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (i as i32).clamp(0, 12);
                                        if this.settings.dock_icon_padding != next {
                                            this.settings.dock_icon_padding = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "LayoutFullWidth",
                        t,
                        Some("\u{E740}"),
                        loc::t("LayoutFullWidth"),
                        ios_switch(
                            "sw-fullw",
                            t,
                            self.settings.full_width,
                            cx.listener(|this, _, _, cx| {
                                this.settings.full_width = !this.settings.full_width;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E72C}"),
                        width_label,
                        button(
                            "btn-reset-width",
                            t,
                            loc::t("Reset"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_custom_width_dips = None;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                format!(
                    "{} & {}",
                    loc::t("SectionBehavior"),
                    loc::t("SectionDisplays")
                ),
                vec![
                    self.target_row(
                        "AutoHide",
                        t,
                        Some("\u{ED1A}"),
                        loc::t("AutoHide"),
                        autohide_dd,
                    ),
                    self.target_row(
                        "AllMonitors",
                        t,
                        Some("\u{E7F4}"),
                        loc::t("AllMonitors"),
                        ios_switch(
                            "sw-allmon",
                            t,
                            self.settings.show_on_all_monitors,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_on_all_monitors =
                                    !this.settings.show_on_all_monitors;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "FocusMode",
                        t,
                        Some("\u{E708}"),
                        loc::t("FocusMode"),
                        ios_switch(
                            "sw-focus",
                            t,
                            self.settings.focus_mode,
                            cx.listener(|this, _, _, cx| {
                                this.settings.focus_mode = !this.settings.focus_mode;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "RevealDockOnWinKey",
                        t,
                        Some("\u{E765}"),
                        loc::t("RevealDockOnWinKey"),
                        ios_switch(
                            "sw-winkey",
                            t,
                            self.settings.reveal_dock_on_win_key,
                            cx.listener(|this, _, _, cx| {
                                this.settings.reveal_dock_on_win_key =
                                    !this.settings.reveal_dock_on_win_key;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "CurrentDesktopOnly",
                        t,
                        Some("\u{E7C4}"),
                        loc::t("CurrentDesktopOnly"),
                        ios_switch(
                            "sw-vdesk",
                            t,
                            self.settings.current_desktop_only,
                            cx.listener(|this, _, _, cx| {
                                this.settings.current_desktop_only =
                                    !this.settings.current_desktop_only;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("SectionInteractions"),
                vec![
                    self.target_row(
                        "ShowStartButton",
                        t,
                        Some("\u{E8A9}"),
                        loc::t("ShowStartButton"),
                        ios_switch(
                            "sw-start-btn",
                            t,
                            self.settings.show_start_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_start_button = !this.settings.show_start_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "ShowSearchButton",
                        t,
                        Some("\u{E721}"),
                        loc::t("ShowSearchButton"),
                        ios_switch(
                            "sw-search-btn",
                            t,
                            self.settings.show_search_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_search_button =
                                    !this.settings.show_search_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "ShowTaskViewButton",
                        t,
                        Some("\u{E7C4}"),
                        loc::t("ShowTaskViewButton"),
                        ios_switch(
                            "sw-taskview-btn",
                            t,
                            self.settings.show_task_view_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_task_view_button =
                                    !this.settings.show_task_view_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "TrayApps",
                        t,
                        Some("\u{E70E}"),
                        loc::t("TrayApps"),
                        ios_switch(
                            "sw-tray",
                            t,
                            self.settings.show_tray_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_tray_button = !this.settings.show_tray_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "RecycleBin",
                        t,
                        Some("\u{E74D}"),
                        loc::t("RecycleBin"),
                        ios_switch(
                            "sw-bin",
                            t,
                            self.settings.show_recycle_bin,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_recycle_bin = !this.settings.show_recycle_bin;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "WidgetShowDesktop",
                        t,
                        Some("\u{E7F8}"),
                        loc::t("WidgetShowDesktop"),
                        ios_switch(
                            "sw-sd",
                            t,
                            self.settings.show_desktop_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_desktop_button =
                                    !this.settings.show_desktop_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "MiddleClickAction",
                        t,
                        Some("\u{E962}"),
                        loc::t("MiddleClickAction"),
                        self.make_dropdown(
                            "dock-mclick",
                            t,
                            [
                                (MiddleClickAction::NewWindow, loc::t("MiddleClickNewWindow")),
                                (MiddleClickAction::CloseWindow, loc::t("MiddleClickClose")),
                                (MiddleClickAction::Nothing, loc::t("MiddleClickNothing")),
                            ]
                            .into_iter()
                            .map(|(act, label)| {
                                let entity = cx.entity().downgrade();
                                DropdownOption {
                                    label: SharedString::from(label),
                                    flag_path: None,
                                    glyph: None,
                                    selected: self.settings.middle_click_action == act,
                                    on_select: std::rc::Rc::new(move |_win, app| {
                                        let _ = entity.update(app, |this, cx| {
                                            this.open_dropdown = None;
                                            this.settings.middle_click_action = act;
                                            this.auto_save(cx);
                                        });
                                    }),
                                }
                            })
                            .collect(),
                            cx,
                        ),
                    ),
                ],
            )
            .into_any_element(),
        ];
        out.extend(self.render_folder_stacks(t, cx));
        out
    }

    fn render_themes(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme_dd = self.make_dropdown(
            "themes-theme",
            t,
            [
                (loc::t("ThemeWindowsAppearance"), AppTheme::System),
                (loc::t("Dark"), AppTheme::Dark),
                (loc::t("Light"), AppTheme::Light),
            ]
            .into_iter()
            .map(|(label, th)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.theme == th,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.theme = th;
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let bg_dd = self.make_dropdown(
            "themes-dock-bg",
            t,
            [
                (loc::t("SurfaceGlass"), SurfaceBackground::Glass),
                (loc::t("SurfaceSolid"), SurfaceBackground::Solid),
                (loc::t("SurfaceNone"), SurfaceBackground::None),
            ]
            .into_iter()
            .map(|(label, bg)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.dock_background == bg,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.dock_background = bg;
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let opacity_pct = (self.settings.dock_opacity * 100.0).round() as i32;
        let radius_label = self
            .settings
            .dock_corner_radius
            .map_or_else(|| loc::t("Automatic"), |r| format!("{r} px"));

        let mut out = vec![
            section_card(
                t,
                loc::t("SectionTheme"),
                vec![
                    self.target_row("Theme", t, Some("\u{E790}"), loc::t("Theme"), theme_dd),
                    self.target_row(
                        "DockBackground",
                        t,
                        Some("\u{E81E}"),
                        loc::t("DockBackground"),
                        bg_dd,
                    ),
                    self.target_row(
                        "SurfaceOpacityLabel:Dock",
                        t,
                        Some("\u{E793}"),
                        loc::t("SurfaceOpacityLabel"),
                        live_slider(
                            "op",
                            t,
                            ((self.settings.dock_opacity as f32) - 0.2) / 0.8,
                            17,
                            format!("{opacity_pct}%"),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (0.20 + (i as f64) * 0.05).clamp(0.20, 1.0);
                                        if (this.settings.dock_opacity - next).abs() > 0.001 {
                                            this.settings.dock_opacity = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "DockCornerRadius",
                        t,
                        Some("\u{F158}"),
                        loc::t("DockCornerRadius"),
                        live_slider(
                            "rad",
                            t,
                            self.settings.dock_corner_radius.unwrap_or(16) as f32 / 32.0,
                            17,
                            radius_label,
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = Some(((i as i32) * 2).clamp(0, 32));
                                        if this.settings.dock_corner_radius != next {
                                            this.settings.dock_corner_radius = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("Animations"),
                vec![
                    self.target_row(
                        "Animations",
                        t,
                        Some("\u{E945}"),
                        loc::t("Animations"),
                        ios_switch(
                            "sw-anim",
                            t,
                            self.settings.animations_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.animations_enabled =
                                    !this.settings.animations_enabled;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        !self.settings.animations_enabled,
                        self.target_row(
                            "Magnification",
                            t,
                            Some("\u{E71E}"),
                            loc::t("Magnification"),
                            ios_switch(
                                "sw-mag",
                                t,
                                self.settings.magnification_enabled,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.magnification_enabled =
                                        !this.settings.magnification_enabled;
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        !self.settings.animations_enabled || !self.settings.magnification_enabled,
                        self.target_row(
                            "FisheyeIntensity",
                            t,
                            Some("\u{E9E9}"),
                            loc::t("FisheyeIntensity"),
                            self.make_dropdown(
                                "themes-fisheye",
                                t,
                                [
                                    (FisheyeIntensity::Low, loc::t("FisheyeLow")),
                                    (FisheyeIntensity::Normal, loc::t("FisheyeNormal")),
                                    (FisheyeIntensity::Strong, loc::t("FisheyeStrong")),
                                ]
                                .into_iter()
                                .map(|(level, label)| {
                                    let entity = cx.entity().downgrade();
                                    DropdownOption {
                                        label: SharedString::from(label),
                                        flag_path: None,
                                        glyph: None,
                                        selected: self.settings.fisheye_intensity == level,
                                        on_select: std::rc::Rc::new(move |_win, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                this.open_dropdown = None;
                                                this.settings.fisheye_intensity = level;
                                                this.auto_save(cx);
                                            });
                                        }),
                                    }
                                })
                                .collect(),
                                cx,
                            ),
                        ),
                    )
                    .into_any_element(),
                    {
                        let max_hz = win32::max_connected_refresh_hz().max(15);
                        let max_step = (max_hz - 15 + 1) as usize;
                        let current_step = match self.settings.animation_fps {
                            Some(fps) => (fps.clamp(15, max_hz) - 15) as usize,
                            None => max_step,
                        };
                        let fps_label = match self.settings.animation_fps {
                            Some(fps) => format!("{} FPS", fps.clamp(15, max_hz)),
                            None => format!("{} ({max_hz} Hz)", loc::t("AnimationFpsAuto")),
                        };
                        let fps_entity = cx.entity().downgrade();
                        dim_when(
                            !self.settings.animations_enabled,
                            self.target_row(
                                "AnimationFps",
                                t,
                                Some("\u{EC4A}"),
                                loc::t("AnimationFps"),
                                live_slider(
                                    "anim-fps",
                                    t,
                                    current_step as f32 / max_step.max(1) as f32,
                                    max_step + 1,
                                    fps_label,
                                    move |i| {
                                        let entity = fps_entity.clone();
                                        std::rc::Rc::new(move |_win, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                let next = if i >= max_step {
                                                    None
                                                } else {
                                                    Some((15 + i as u32).min(max_hz))
                                                };
                                                if this.settings.animation_fps != next {
                                                    this.settings.animation_fps = next;
                                                    this.auto_save(cx);
                                                }
                                            });
                                        })
                                    },
                                ),
                            ),
                        )
                        .into_any_element()
                    },
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("ThemeSectionTypography"),
                vec![
                    setting_row(
                        t,
                        Some("\u{E8D3}"),
                        loc::t("UiFontFamily"),
                        button(
                            "btn-ui-font",
                            t,
                            if self.settings.ui_font_family.trim().is_empty() {
                                loc::t("UiFontNative")
                            } else {
                                self.settings.ui_font_family.clone()
                            },
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, window, cx| this.choose_font(window, cx)),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E8D3}"),
                        loc::t("UiFontSize"),
                        live_slider(
                            "ui-font-size",
                            t,
                            (self.settings.ui_font_size - 9.0) / 15.0,
                            16,
                            format!("{} px", self.settings.ui_font_size.round() as i32),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (9.0 + i as f32).clamp(9.0, 24.0);
                                        if (this.settings.ui_font_size - next).abs() > 0.01 {
                                            this.settings.ui_font_size = next;
                                            fonts::apply_settings(&this.settings);
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .font_family(SharedString::from(
                            if self.settings.ui_font_family.trim().is_empty() {
                                fonts::native_ui_face().to_owned()
                            } else {
                                self.settings.ui_font_family.clone()
                            },
                        ))
                        .text_size(px(fonts::scaled_size(13.0)))
                        .child(loc::t("UiFontPreview"))
                        .child(" — AaBbÇğÖşü 0123")
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("MyThemes"),
                vec![
                    div()
                        .w_full()
                        .flex()
                        .gap(px(6.0))
                        .child(button(
                            "btn-theme-import",
                            t,
                            loc::t("ThemePackImport"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.import_theme(cx)),
                        ))
                        .child(button(
                            "btn-theme-folder",
                            t,
                            loc::t("ThemePackOpenFolder"),
                            ButtonVariant::Secondary,
                            cx.listener(|_, _, _, _| {
                                pins::launch(Self::themes_root().to_string_lossy().into_owned());
                            }),
                        ))
                        .into_any_element(),
                    {
                        let packs = theme_packs::discover(&Self::themes_root());
                        let mut rows = div().w_full().flex().flex_col().gap(px(6.0));
                        if packs.is_empty() {
                            rows = rows.child(
                                div()
                                    .text_size(px(fonts::scaled_size(12.0)))
                                    .text_color(rgba(t.text_secondary))
                                    .child(loc::t("NoCustomThemes")),
                            );
                        }
                        for pack in packs {
                            let id = pack.id.clone();
                            let export_id = id.clone();
                            let delete_id = id.clone();
                            let use_id = id.clone();
                            let active = self.settings.theme_pack.eq_ignore_ascii_case(&id);
                            rows = rows.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(fonts::scaled_size(12.0)))
                                            .child(pack.manifest.name.clone()),
                                    )
                                    .child(button(
                                        SharedString::from(format!("theme-use-{id}")),
                                        t,
                                        if active {
                                            loc::t("WidgetInstalled")
                                        } else {
                                            loc::t("ThemePackIcons")
                                        },
                                        ButtonVariant::Secondary,
                                        cx.listener(move |this, _, _, cx| {
                                            this.settings.theme_pack = use_id.clone();
                                            crate::platform::icon_pack::set_active_pack(Some(
                                                &Self::themes_root().join(&use_id),
                                            ));
                                            this.auto_save(cx);
                                        }),
                                    ))
                                    .child(button(
                                        SharedString::from(format!("theme-export-{id}")),
                                        t,
                                        loc::t("ThemePackExport"),
                                        ButtonVariant::Secondary,
                                        cx.listener(move |this, _, _, cx| {
                                            this.export_theme(&export_id, cx)
                                        }),
                                    ))
                                    .child(button(
                                        SharedString::from(format!("theme-delete-{id}")),
                                        t,
                                        loc::t("WidgetRemove"),
                                        ButtonVariant::Destructive,
                                        cx.listener(move |this, _, _, cx| {
                                            this.delete_theme(&delete_id, cx)
                                        }),
                                    )),
                            );
                        }
                        rows = rows.child(button(
                            "theme-use-native",
                            t,
                            loc::t("UiFontNative"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                this.settings.theme_pack.clear();
                                crate::platform::icon_pack::set_active_pack(None);
                                this.auto_save(cx);
                            }),
                        ));
                        rows.into_any_element()
                    },
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(self.theme_status.clone())
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("SectionSurfaces"),
                vec![
                    self.surface_row(
                        t,
                        cx,
                        "surf-dock",
                        "SurfaceDock",
                        loc::t("SurfaceDock"),
                        self.settings.dock_background,
                        |settings, value| settings.dock_background = value,
                    ),
                    self.surface_row(
                        t,
                        cx,
                        "surf-menu",
                        "SurfaceMenu",
                        loc::t("SurfaceMenu"),
                        self.settings.menu_background,
                        |settings, value| settings.menu_background = value,
                    ),
                    self.surface_row(
                        t,
                        cx,
                        "surf-widget",
                        "SurfaceWidget",
                        loc::t("SurfaceWidget"),
                        self.settings.widget_background,
                        |settings, value| settings.widget_background = value,
                    ),
                    self.surface_row(
                        t,
                        cx,
                        "surf-settings",
                        "SurfaceSettings",
                        loc::t("SurfaceSettings"),
                        self.settings.settings_background,
                        |settings, value| settings.settings_background = value,
                    ),
                    self.target_row(
                        "SurfaceOpacityLabel:Surfaces",
                        t,
                        Some("\u{E706}"),
                        loc::t("SurfaceOpacityLabel"),
                        live_slider(
                            "surf-op",
                            t,
                            ((self.settings.surface_opacity as f32) - 0.2) / 0.8,
                            17,
                            format!(
                                "{}%",
                                (self.settings.surface_opacity * 100.0).round() as i32
                            ),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (0.20 + (i as f64) * 0.05).clamp(0.20, 1.0);
                                        if (this.settings.surface_opacity - next).abs() > 0.001 {
                                            this.settings.surface_opacity = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            {
                let border_surfaces = [
                    (
                        "border-dock",
                        loc::t("SurfaceDock"),
                        self.settings.dock_border_thickness,
                        self.settings.dock_border_color.clone(),
                        0usize,
                    ),
                    (
                        "border-menu",
                        loc::t("SurfaceMenu"),
                        self.settings.menu_border_thickness,
                        self.settings.menu_border_color.clone(),
                        1usize,
                    ),
                    (
                        "border-widget",
                        loc::t("SurfaceWidget"),
                        self.settings.widget_border_thickness,
                        self.settings.widget_border_color.clone(),
                        2usize,
                    ),
                    (
                        "border-settings",
                        loc::t("SurfaceSettings"),
                        self.settings.settings_border_thickness,
                        self.settings.settings_border_color.clone(),
                        3usize,
                    ),
                ];
                let mut border_rows: Vec<AnyElement> = vec![
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("BorderStyleHelp"))
                        .into_any_element(),
                ];
                for (id, label, thickness, color, index) in border_surfaces {
                    let step = stepper(
                        SharedString::from(format!("{id}-minus")),
                        SharedString::from(format!("{id}-plus")),
                        t,
                        format!("{thickness:.1}"),
                        cx.listener(move |this, _, _, cx| {
                            let next = match index {
                                0 => this.settings.dock_border_thickness,
                                1 => this.settings.menu_border_thickness,
                                2 => this.settings.widget_border_thickness,
                                _ => this.settings.settings_border_thickness,
                            } - 0.5;
                            set_border_thickness(&mut this.settings, index, next);
                            this.auto_save(cx);
                        }),
                        cx.listener(move |this, _, _, cx| {
                            let next = match index {
                                0 => this.settings.dock_border_thickness,
                                1 => this.settings.menu_border_thickness,
                                2 => this.settings.widget_border_thickness,
                                _ => this.settings.settings_border_thickness,
                            } + 0.5;
                            set_border_thickness(&mut this.settings, index, next);
                            this.auto_save(cx);
                        }),
                    );
                    let color_id = SharedString::from(format!("{id}-color"));
                    border_rows.push(
                        setting_row(
                            t,
                            None,
                            label,
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(step)
                                .child(button(
                                    color_id,
                                    t,
                                    color.unwrap_or_else(|| "\u{E7A2}".to_owned()),
                                    ButtonVariant::Secondary,
                                    cx.listener(move |this, _, _, cx| {
                                        this.edit_border_color(index, cx)
                                    }),
                                )),
                        )
                        .into_any_element(),
                    );
                }
                section_card(t, loc::t("BorderStyleLabel"), border_rows).into_any_element()
            },
        ];
        out.extend(self.render_theme_gallery(t, cx));
        out.extend(self.render_theme_editor(t, cx));
        out
    }

    fn widget_summary(&self, id: &str) -> Option<String> {
        match id {
            "clock" => Some(format!(
                "{} · {}",
                if self.settings.clock_24_hour { "24h" } else { "12h" },
                match self.settings.clock_date_layout {
                    ClockDateLayout::Beside => loc::t("ClockDateLayoutBeside"),
                    ClockDateLayout::Below => loc::t("ClockDateLayoutBelow"),
                }
            )),
            "weather" => {
                let city = self
                    .settings
                    .weather_city
                    .split(", ")
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Istanbul");
                let unit = match self.settings.weather_unit {
                    TemperatureUnit::Celsius => "°C",
                    TemperatureUnit::Fahrenheit => "°F",
                };
                Some(format!("{city} · {unit}"))
            }
            "stats" => {
                let mut parts = Vec::new();
                if self.settings.stats_meters & 1 != 0 {
                    parts.push("CPU");
                }
                if self.settings.stats_meters & 2 != 0 {
                    parts.push("RAM");
                }
                if self.settings.stats_meters & 16 != 0 {
                    parts.push("GPU");
                }
                if self.settings.stats_meters & 32 != 0 {
                    parts.push(match self.settings.weather_unit {
                        TemperatureUnit::Celsius => "GPU °C",
                        TemperatureUnit::Fahrenheit => "GPU °F",
                    });
                }
                Some(if parts.is_empty() {
                    "—".to_owned()
                } else {
                    parts.join(" · ")
                })
            }
            "bluetoothbattery" => Some(if self.settings.wireless_battery_low_threshold == 0 {
                loc::t("WirelessBatteryAlertOff")
            } else {
                format!("≤ {}%", self.settings.wireless_battery_low_threshold)
            }),
            other if other.starts_with("aiusage_") => {
                let detected = widgets::telemetry::ai_provider_detected(other);
                Some(loc::t(if detected {
                    "AiUsageAccountConnected"
                } else {
                    "AiUsageAccountSignInRequired"
                }))
            }
            _ => None,
        }
    }

    fn render_widget_inline_settings(
        &self,
        wid: &'static str,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        match wid {
            "clock" => vec![
                self.target_row(
                    "ClockFormat24Hour",
                    t,
                    Some("\u{E823}"),
                    loc::t("ClockFormat24Hour"),
                    ios_switch(
                        "sw-24h",
                        t,
                        self.settings.clock_24_hour,
                        cx.listener(|this, _, _, cx| {
                            this.settings.clock_24_hour = !this.settings.clock_24_hour;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "ClockShowSeconds",
                    t,
                    Some("\u{E916}"),
                    loc::t("ClockShowSeconds"),
                    ios_switch(
                        "sw-seconds",
                        t,
                        self.settings.clock_show_seconds,
                        cx.listener(|this, _, _, cx| {
                            this.settings.clock_show_seconds = !this.settings.clock_show_seconds;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "DateStyleDayShort",
                    t,
                    Some("\u{E787}"),
                    loc::t("DateStyleDayShort"),
                    ios_switch(
                        "sw-weekday",
                        t,
                        self.settings.date_style == ClockDateStyle::DayAndShortDate,
                        cx.listener(|this, _, _, cx| {
                            this.settings.date_style = if this.settings.date_style
                                == ClockDateStyle::DayAndShortDate
                            {
                                ClockDateStyle::ShortDateOnly
                            } else {
                                ClockDateStyle::DayAndShortDate
                            };
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "ClockDateLayout",
                    t,
                    Some("\u{E8A1}"),
                    loc::t("ClockDateLayout"),
                    self.make_dropdown(
                        "clock-date-layout",
                        t,
                        [
                            (loc::t("ClockDateLayoutBeside"), ClockDateLayout::Beside),
                            (loc::t("ClockDateLayoutBelow"), ClockDateLayout::Below),
                        ]
                        .into_iter()
                        .map(|(label, layout)| {
                            let entity = cx.entity().downgrade();
                            DropdownOption {
                                label: SharedString::from(label),
                                flag_path: None,
                                glyph: None,
                                selected: self.settings.clock_date_layout == layout,
                                on_select: std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        this.open_dropdown = None;
                                        this.settings.clock_date_layout = layout;
                                        this.auto_save(cx);
                                    });
                                }),
                            }
                        })
                        .collect(),
                        cx,
                    ),
                ),
                self.target_row(
                    "ClockDateFormat",
                    t,
                    Some("\u{E8BF}"),
                    loc::t("ClockDateFormat"),
                    self.make_dropdown(
                        "clock-date-format",
                        t,
                        [
                            (loc::t("DateFormatSystem"), ClockDateFormat::System),
                            (loc::t("DateFormatDdMmYyyy"), ClockDateFormat::DdMmYyyy),
                            (loc::t("DateFormatYyyyMmDd"), ClockDateFormat::YyyyMmDd),
                        ]
                        .into_iter()
                        .map(|(label, fmt)| {
                            let entity = cx.entity().downgrade();
                            DropdownOption {
                                label: SharedString::from(label),
                                flag_path: None,
                                glyph: None,
                                selected: self.settings.date_format == fmt,
                                on_select: std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        this.open_dropdown = None;
                                        this.settings.date_format = fmt;
                                        this.auto_save(cx);
                                    });
                                }),
                            }
                        })
                        .collect(),
                        cx,
                    ),
                ),
            ],
            "weather" => self.render_weather_picker(t, cx),
            "stats" => vec![
                self.target_row(
                    "Cpu",
                    t,
                    Some("\u{E950}"),
                    loc::t("Cpu"),
                    ios_switch(
                        "sw-meter-cpu",
                        t,
                        self.settings.stats_meters & 1 != 0,
                        cx.listener(|this, _, _, cx| {
                            this.settings.stats_meters ^= 1;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "Memory",
                    t,
                    Some("\u{E964}"),
                    loc::t("Memory"),
                    ios_switch(
                        "sw-meter-mem",
                        t,
                        self.settings.stats_meters & 2 != 0,
                        cx.listener(|this, _, _, cx| {
                            this.settings.stats_meters ^= 2;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "Gpu",
                    t,
                    Some("\u{E7F4}"),
                    loc::t("Gpu"),
                    ios_switch(
                        "sw-meter-gpu",
                        t,
                        self.settings.stats_meters & 16 != 0,
                        cx.listener(|this, _, _, cx| {
                            this.settings.stats_meters ^= 16;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row_desc(
                    "GpuTemperature",
                    t,
                    Some("\u{E9CA}"),
                    loc::t("GpuTemperature"),
                    loc::t("StatsAvailabilityNote"),
                    ios_switch(
                        "sw-meter-gputemp",
                        t,
                        self.settings.stats_meters & 32 != 0,
                        cx.listener(|this, _, _, cx| {
                            this.settings.stats_meters ^= 32;
                            this.auto_save(cx);
                        }),
                    ),
                ),
            ],
            "bluetoothbattery" => vec![self.target_row(
                "WirelessBatteryLowThreshold",
                t,
                Some("\u{E850}"),
                loc::t("WirelessBatteryAlertLabel"),
                self.make_dropdown(
                    "bt-low-threshold",
                    t,
                    [
                        (0, loc::t("WirelessBatteryAlertOff")),
                        (10, "10%".to_owned()),
                        (20, "20%".to_owned()),
                        (30, "30%".to_owned()),
                    ]
                    .into_iter()
                    .map(|(threshold, label)| {
                        let entity = cx.entity().downgrade();
                        DropdownOption {
                            label: SharedString::from(label),
                            flag_path: None,
                            glyph: None,
                            selected: self.settings.wireless_battery_low_threshold == threshold,
                            on_select: std::rc::Rc::new(move |_win, app| {
                                let _ = entity.update(app, |this, cx| {
                                    this.open_dropdown = None;
                                    this.settings.wireless_battery_low_threshold = threshold;
                                    this.auto_save(cx);
                                });
                            }),
                        }
                    })
                    .collect(),
                    cx,
                ),
            )],
            other if other.starts_with("aiusage_") => {
                let mut rows = Vec::new();
                if let Some(provider) = widgets::telemetry::AI_PROVIDERS
                    .iter()
                    .find(|p| p.id == other)
                {
                    let detected = widgets::telemetry::ai_provider_detected(provider.id);
                    let status = loc::t(if detected {
                        "AiUsageAccountConnected"
                    } else {
                        "AiUsageAccountSignInRequired"
                    });
                    let url = provider.dashboard_url.to_owned();
                    let hint = format!(
                        "{}: {}",
                        loc::t("AiUsageCliSignInLabel"),
                        provider.signin_hint
                    );
                    rows.push(
                        setting_row_desc(
                            t,
                            Some("\u{E99A}"),
                            provider.name,
                            hint,
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(badge(t, status, detected))
                                .child(button(
                                    SharedString::from(format!("btn-ai-dash-{}", provider.id)),
                                    t,
                                    loc::t("AiUsageOpenDashboard"),
                                    ButtonVariant::Secondary,
                                    move |_, _, _| {
                                        pins::launch(url.clone());
                                    },
                                )),
                        )
                        .into_any_element(),
                    );
                    if let Some((_, secret_key)) = widgets::telemetry::ai_api_key_providers()
                        .iter()
                        .find(|(pid, _)| *pid == provider.id)
                    {
                        let saved = widgets::telemetry::has_dpapi_secret(secret_key);
                        let value = if saved {
                            loc::t("AiUsageApiKeySaved")
                        } else {
                            loc::t("AiUsageApiKeyPlaceholder")
                        };
                        let key = *secret_key;
                        rows.push(
                            setting_row_desc(
                                t,
                                Some("\u{E8D7}"),
                                loc::t("AiUsageApiKeyLabel"),
                                value,
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(button(
                                        SharedString::from(format!("btn-api-key-{}", provider.id)),
                                        t,
                                        loc::t("AiUsageApiKeySave"),
                                        ButtonVariant::Secondary,
                                        cx.listener(move |this, _, _, cx| {
                                            this.edit_ai_api_key(key, cx)
                                        }),
                                    ))
                                    .child(button(
                                        SharedString::from(format!(
                                            "btn-api-key-clear-{}",
                                            provider.id
                                        )),
                                        t,
                                        loc::t("AiUsageApiKeyClear"),
                                        ButtonVariant::Secondary,
                                        cx.listener(move |this, _, _, cx| {
                                            this.clear_ai_api_key(key, cx)
                                        }),
                                    )),
                            )
                            .into_any_element(),
                        );
                    }
                }
                rows
            }
            _ => Vec::new(),
        }
    }

    fn render_widget_card(
        &self,
        w: &'static widgets::WidgetDescriptor,
        t: &UiTheme,
        is_dark: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wid = w.id;
        let enabled = self.settings.is_widget_enabled(wid);
        let has_inline = widget_has_inline_settings(wid);
        let expanded = has_inline && self.expanded_widget == Some(wid);
        let preview = self.render_widget_preview(wid, t, is_dark);
        let border_col = if enabled || expanded {
            t.accent
        } else {
            t.border
        };
        let card_hover = if expanded {
            t.surface
        } else {
            t.surface_hover
        };
        let target_id = format!("widget-{wid}");
        let highlighted = self.highlight_row.as_deref() == Some(target_id.as_str());
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        let stage_bg = if is_dark { 0x0d0d10ff } else { 0xe9ecf1ff };
        let summary = self.widget_summary(wid);

        let cfg_button = if has_inline {
            let btn_bg = if expanded {
                t.accent_soft
            } else {
                t.surface_muted
            };
            let btn_bd = if expanded { t.accent } else { t.border };
            let btn_fg = if expanded {
                t.accent
            } else {
                t.text_secondary
            };
            let btn_hov = if expanded {
                t.accent_soft
            } else {
                t.surface_hover
            };
            Some(
                div()
                    .id(SharedString::from(format!("wcfg-{wid}")))
                    .h(px(28.0))
                    .px(px(9.0))
                    .rounded(px(7.0))
                    .bg(rgba(btn_bg))
                    .border_1()
                    .border_color(rgba(btn_bd))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgba(btn_hov)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_dropdown = None;
                            this.expanded_widget = if this.expanded_widget == Some(wid) {
                                None
                            } else {
                                Some(wid)
                            };
                            cx.notify();
                        }),
                    )
                    .child(icon_glyph(
                        if expanded { "\u{E70E}" } else { "\u{E713}" },
                        11.5,
                        btn_fg,
                    ))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(11.5)))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(rgba(btn_fg))
                            .child(if expanded {
                                loc::t("Close")
                            } else {
                                loc::t("SettingsTitle")
                            }),
                    ),
            )
        } else {
            None
        };

        let enable_switch = ios_switch(
            SharedString::from(format!("wsw-{wid}")),
            t,
            enabled,
            cx.listener(move |this, _, _, cx| {
                let cur = this.settings.is_widget_enabled(wid);
                if !cur && this.settings.focus_mode && wid != "clock" && wid != "focus" {
                    this.settings.focus_mode = false;
                }
                this.settings.set_widget_enabled(wid, !cur);
                this.auto_save(cx);
            }),
        );

        let preview_box = div()
            .h(px(80.0))
            .px(px(12.0))
            .rounded(px(10.0))
            .bg(rgba(stage_bg))
            .border_1()
            .border_color(rgba(t.border))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .when(expanded, |b| b.min_w(px(260.0)).flex_shrink_0())
            .when(!expanded, |b| b.w_full())
            .child(preview);

        let mut meta = div().flex().flex_col().gap(px(2.0)).min_w_0().child(
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .min_w_0()
                .child(icon_glyph(
                    widget_glyph(wid),
                    13.5,
                    if enabled { t.accent } else { t.text_secondary },
                ))
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.5)))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgba(t.text_primary))
                        .child(widgets::widget_name(w.id)),
                ),
        );
        if let Some(sub) = summary {
            meta = meta.child(
                div()
                    .pl(px(21.0))
                    .text_size(px(fonts::scaled_size(11.0)))
                    .text_color(rgba(t.text_secondary))
                    .child(sub),
            );
        }

        let footer = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(meta)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .flex_shrink_0()
                    .when_some(cfg_button, |row, b| row.child(b))
                    .child(enable_switch),
            );

        let head = if expanded {
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(14.0))
                .child(preview_box)
                .child(footer)
        } else {
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(preview_box)
                .child(footer)
        };

        let mut card = div()
            .when(expanded, |c| c.w_full())
            .when(!expanded, |c| c.flex_1().min_w_0())
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(13.0))
            .rounded(px(13.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(border_col))
            .hover(move |s| s.bg(rgba(card_hover)))
            .child(head);

        if expanded {
            let rows = self.render_widget_inline_settings(wid, t, cx);
            let mut drawer = div()
                .w_full()
                .pt(px(8.0))
                .border_t_1()
                .border_color(rgba(t.separator))
                .flex()
                .flex_col();
            let count = rows.len();
            for (idx, row) in rows.into_iter().enumerate() {
                drawer = drawer.child(row);
                if idx + 1 < count {
                    drawer = drawer.child(div().h(px(1.0)).mx(px(8.0)).bg(rgba(t.separator)));
                }
            }
            card = card.child(drawer);
        }

        let card = if highlighted {
            card.bg(rgba(t.accent_soft)).border_color(rgba(t.accent))
        } else {
            card
        };

        card.id(SharedString::from(format!("wcard-{wid}")))
            .anchor_scroll(anchor)
            .into_any_element()
    }

    fn render_widgets(
        &self,
        t: &UiTheme,
        is_dark: bool,
        columns: usize,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        let active_count = widgets::ALL_WIDGETS
            .iter()
            .filter(|w| self.settings.is_widget_enabled(w.id))
            .count();

        let filter_opts: Vec<DropdownOption> = std::iter::once("All")
            .chain(std::iter::once("Active"))
            .chain(widgets::WIDGET_CATEGORIES.iter().copied())
            .map(|cat| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(category_label(cat)),
                    flag_path: None,
                    glyph: None,
                    selected: self.widget_filter == cat,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.widget_filter = cat;
                            cx.notify();
                        });
                    }),
                }
            })
            .collect();

        out.push(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(10.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(badge(
                            t,
                            loc::t_fmt(
                                "WidgetsActiveCount",
                                &[
                                    &active_count.to_string(),
                                    &widgets::ALL_WIDGETS.len().to_string(),
                                ],
                            ),
                            active_count > 0,
                        ))
                        .child(self.make_dropdown("widget-cat-filter", t, filter_opts, cx)),
                )
                .child(button(
                    "btn-clear-widgets",
                    t,
                    loc::t("ClearAll"),
                    ButtonVariant::Secondary,
                    cx.listener(|this, _, _, cx| {
                        this.settings.show_clock_widget = false;
                        this.settings.show_system_stats_widget = false;
                        this.settings.enabled_widgets.clear();
                        this.auto_save(cx);
                    }),
                ))
                .into_any_element(),
        );

        for &cat in widgets::WIDGET_CATEGORIES {
            if self.widget_filter != "All"
                && self.widget_filter != "Active"
                && self.widget_filter != cat
            {
                continue;
            }
            let items: Vec<&'static widgets::WidgetDescriptor> = widgets::ALL_WIDGETS
                .iter()
                .filter(|w| {
                    w.category == cat
                        && (self.widget_filter != "Active" || self.settings.is_widget_enabled(w.id))
                })
                .collect();
            if items.is_empty() {
                continue;
            }
            out.push(
                div()
                    .w_full()
                    .pt(px(4.0))
                    .px(px(2.0))
                    .text_size(px(fonts::scaled_size(12.0)))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgba(t.text_secondary))
                    .child(category_label(cat))
                    .into_any_element(),
            );

            // Lay out cards in pairs, allowing any expanded card to span full width cleanly.
            let mut pending_row: Vec<&'static widgets::WidgetDescriptor> = Vec::new();
            let flush_row = |pending: &mut Vec<&'static widgets::WidgetDescriptor>,
                             out: &mut Vec<AnyElement>,
                             this: &Self,
                             cx: &mut Context<Self>| {
                if pending.is_empty() {
                    return;
                }
                let mut row = div().w_full().flex().items_start().gap(px(12.0));
                for &w in pending.iter() {
                    row = row.child(this.render_widget_card(w, t, is_dark, cx));
                }
                if pending.len() < columns {
                    row = row.child(div().flex_1());
                }
                out.push(row.into_any_element());
                pending.clear();
            };

            for &w in &items {
                let is_expanded =
                    widget_has_inline_settings(w.id) && self.expanded_widget == Some(w.id);
                if is_expanded {
                    flush_row(&mut pending_row, &mut out, self, cx);
                    out.push(self.render_widget_card(w, t, is_dark, cx));
                } else {
                    pending_row.push(w);
                    if pending_row.len() >= columns {
                        flush_row(&mut pending_row, &mut out, self, cx);
                    }
                }
            }
            flush_row(&mut pending_row, &mut out, self, cx);
        }

        let density_opts: Vec<DropdownOption> = [
            (WidgetDensityMode::Auto, loc::t("WidgetDensityAuto")),
            (WidgetDensityMode::Compact, loc::t("WidgetDensityCompact")),
            (WidgetDensityMode::Full, loc::t("WidgetDensityFull")),
        ]
        .into_iter()
        .map(|(mode, label)| {
            let entity = cx.entity().downgrade();
            DropdownOption {
                label: SharedString::from(label),
                flag_path: None,
                glyph: None,
                selected: self.settings.global_widget_density == mode,
                on_select: std::rc::Rc::new(move |_win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        this.settings.global_widget_density = mode;
                        this.auto_save(cx);
                    });
                }),
            }
        })
        .collect();

        out.push(
            section_card(
                t,
                loc::t("WidgetDensity"),
                vec![
                    setting_row(
                        t,
                        Some("\u{E80A}"),
                        loc::t("WidgetDensity"),
                        self.make_dropdown("widget-density", t, density_opts, cx),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        );
        out
    }

    fn render_dynamic_island(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let monitors = win32::enumerate_monitors();
        let active = widgets::active_widgets(&self.settings);
        let di_opacity_pct = (self.settings.dynamic_island_opacity * 100.0).round() as i32;

        let di_disabled = !self.settings.dynamic_island_enable;
        let mut cards = vec![
            section_card(
                t,
                loc::t("DynamicIslandTitle"),
                vec![
                    setting_row_desc(
                        t,
                        Some("\u{E8A1}"),
                        loc::t("DynamicIslandEnable"),
                        loc::t("DynamicIslandEnableDescription"),
                        ios_switch(
                            "di-enable",
                            t,
                            self.settings.dynamic_island_enable,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dynamic_island_enable =
                                    !this.settings.dynamic_island_enable;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row_desc(
                            "DynamicIslandVisibility",
                            t,
                            Some("\u{E7B3}"),
                            loc::t("DynamicIslandVisibility"),
                            loc::t("DynamicIslandVisibilityDescription"),
                            self.make_dropdown(
                                "di-vis",
                                t,
                                [
                                    (
                                        DynamicIslandVisibilityMode::Always,
                                        loc::t("DynamicIslandVisibilityAlways"),
                                    ),
                                    (
                                        DynamicIslandVisibilityMode::DesktopOnly,
                                        loc::t("DynamicIslandVisibilityDesktopOnly"),
                                    ),
                                ]
                                .into_iter()
                                .map(|(mode, label)| {
                                    let entity = cx.entity().downgrade();
                                    DropdownOption {
                                        label: SharedString::from(label),
                                        flag_path: None,
                                        glyph: None,
                                        selected: self.settings.dynamic_island_visibility == mode,
                                        on_select: std::rc::Rc::new(move |_win, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                this.open_dropdown = None;
                                                this.settings.dynamic_island_visibility = mode;
                                                this.auto_save(cx);
                                            });
                                        }),
                                    }
                                })
                                .collect(),
                                cx,
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row_desc(
                            "DynamicIslandHideFullscreen",
                            t,
                            Some("\u{E740}"),
                            loc::t("DynamicIslandHideFullscreen"),
                            loc::t("DynamicIslandHideFullscreenDescription"),
                            ios_switch(
                                "di-hide-fs",
                                t,
                                self.settings.dynamic_island_hide_in_fullscreen,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.dynamic_island_hide_in_fullscreen =
                                        !this.settings.dynamic_island_hide_in_fullscreen;
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row_desc(
                            "DynamicIslandAutoHide",
                            t,
                            Some("\u{ED1A}"),
                            loc::t("DynamicIslandAutoHide"),
                            loc::t("DynamicIslandAutoHideDescription"),
                            ios_switch(
                                "di-auto-hide",
                                t,
                                self.settings.dynamic_island_auto_hide,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.dynamic_island_auto_hide =
                                        !this.settings.dynamic_island_auto_hide;
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        setting_row_desc(
                            t,
                            Some("\u{E81E}"),
                            loc::t("DynamicIslandBackground"),
                            loc::t("DynamicIslandBackgroundDescription"),
                            self.make_dropdown(
                                "di-bg",
                                t,
                                [
                                    (loc::t("SurfaceGlass"), SurfaceBackground::Glass),
                                    (loc::t("SurfaceSolid"), SurfaceBackground::Solid),
                                    (loc::t("SurfaceNone"), SurfaceBackground::None),
                                ]
                                .into_iter()
                                .map(|(label, bg)| {
                                    let entity = cx.entity().downgrade();
                                    DropdownOption {
                                        label: SharedString::from(label),
                                        flag_path: None,
                                        glyph: None,
                                        selected: self.settings.dynamic_island_background == bg,
                                        on_select: std::rc::Rc::new(move |_win, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                this.open_dropdown = None;
                                                this.settings.dynamic_island_background = bg;
                                                this.auto_save(cx);
                                            });
                                        }),
                                    }
                                })
                                .collect(),
                                cx,
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        setting_row(
                            t,
                            Some("\u{E790}"),
                            loc::t("DynamicIslandUseTheme"),
                            ios_switch(
                                "di-use-theme",
                                t,
                                self.settings.dynamic_island_use_theme,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.dynamic_island_use_theme =
                                        !this.settings.dynamic_island_use_theme;
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row_desc(
                            "DynamicIslandOpacity",
                            t,
                            Some("\u{E793}"),
                            // No `DynamicIslandOpacity` locale key exists; reuse the generic Opacity label.
                            loc::t("SurfaceOpacityLabel"),
                            loc::t("DynamicIslandOpacityDescription"),
                            live_slider(
                                "di-opacity",
                                t,
                                ((self.settings.dynamic_island_opacity as f32) - 0.1) / 0.9,
                                19,
                                format!("{di_opacity_pct}%"),
                                |i| {
                                    let entity = cx.entity().downgrade();
                                    std::rc::Rc::new(move |_win, app| {
                                        let _ = entity.update(app, |this, cx| {
                                            let next = (0.10 + (i as f64) * 0.05).clamp(0.10, 1.0);
                                            if (this.settings.dynamic_island_opacity - next).abs()
                                                > 0.001
                                            {
                                                this.settings.dynamic_island_opacity = next;
                                                this.auto_save(cx);
                                            }
                                        });
                                    })
                                },
                            ),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ];

        let mut placement_rows: Vec<AnyElement> = Vec::new();
        if active.is_empty() {
            placement_rows.push(
                self.mark_target(
                    "DynamicIslandWidgetLocation",
                    t,
                    div()
                        .w_full()
                        .px(px(14.0))
                        .py(px(12.0))
                        .text_size(px(12.5))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("DynamicIslandWidgetLocationDescription")),
                )
                .into_any_element(),
            );
        } else {
            for (index, wd) in active.into_iter().enumerate() {
                let wid = wd.id;
                let current = self
                    .settings
                    .dynamic_island_placements
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(wid))
                    .map(|(_, v)| v.clone());

                let mut choices: Vec<(Option<String>, String)> = Vec::new();
                choices.push((None, loc::t("DynamicIslandDock")));
                choices.push((
                    Some(settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned()),
                    loc::t("DynamicIslandAllDisplays"),
                ));
                for mon in &monitors {
                    choices.push((Some(mon.device_name.clone()), mon.display_label()));
                }

                let dd_opts: Vec<DropdownOption> = choices
                    .into_iter()
                    .map(|(target, label)| {
                        let is_active = match (&current, &target) {
                            (None, None) => true,
                            (Some(cur), Some(tgt)) => cur.eq_ignore_ascii_case(tgt),
                            _ => false,
                        };
                        let wid_owned = wid.to_owned();
                        let entity = cx.entity().downgrade();
                        DropdownOption {
                            label: SharedString::from(label),
                            flag_path: None,
                            glyph: None,
                            selected: is_active,
                            on_select: std::rc::Rc::new(move |_win, app| {
                                let _ = entity.update(app, |this, cx| {
                                    this.open_dropdown = None;
                                    this.settings
                                        .dynamic_island_placements
                                        .retain(|k, _| !k.eq_ignore_ascii_case(&wid_owned));
                                    if let Some(ref dev) = target {
                                        this.settings
                                            .dynamic_island_placements
                                            .insert(wid_owned.clone(), dev.clone());
                                    }
                                    this.auto_save(cx);
                                });
                            }),
                        }
                    })
                    .collect();

                let control = self.make_dropdown(format!("di-place-{wid}"), t, dd_opts, cx);
                placement_rows.push(if index == 0 {
                    // The catalog entry for this section targets its first widget row.
                    self.target_row(
                        "DynamicIslandWidgetLocation",
                        t,
                        Some(widget_glyph(wid)),
                        widgets::widget_name(wid),
                        control,
                    )
                } else {
                    setting_row(
                        t,
                        Some(widget_glyph(wid)),
                        widgets::widget_name(wid),
                        control,
                    )
                    .into_any_element()
                });
            }
        }

        cards.push(
            section_card(t, loc::t("DynamicIslandWidgetLocation"), placement_rows)
                .into_any_element(),
        );
        cards
    }

    fn render_preferences(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let lang_opts: Vec<DropdownOption> = LANGUAGES
            .iter()
            .map(|(tag, _name)| {
                let chosen = *tag;
                let entity = cx.entity().downgrade();
                let flag = fonts::flag_bytes(chosen)
                    .map(|_| SharedString::from(fonts::flag_path(chosen)));
                DropdownOption {
                    label: SharedString::from(loc::display_name(chosen)),
                    flag_path: flag,
                    glyph: None,
                    selected: self.settings.language.eq_ignore_ascii_case(chosen),
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.language = chosen.to_owned();
                            loc::set_language(chosen);
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect();

        let mut out = vec![
            section_card(
                t,
                loc::t("Language"),
                vec![self.target_row_desc(
                    "Language",
                    t,
                    Some("\u{E774}"),
                    loc::t("Language"),
                    loc::t("LanguageDescription"),
                    self.make_dropdown("prefs-language", t, lang_opts, cx),
                )],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("SectionHotkeys"),
                vec![
                    setting_row(
                        t,
                        Some("\u{E721}"),
                        loc::t("LauncherHotkey"),
                        button(
                            "btn-hotkey-launcher",
                            t,
                            self.settings
                                .launcher_binding()
                                .map(|binding| binding.display)
                                .unwrap_or_else(|| loc::t("HotkeyDisabled")),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.record_hotkey(true, cx)),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E765}"),
                        loc::t("DockHotkey"),
                        button(
                            "btn-hotkey-dock",
                            t,
                            self.settings
                                .dock_binding()
                                .map(|binding| binding.display)
                                .unwrap_or_else(|| loc::t("HotkeyDisabled")),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.record_hotkey(false, cx)),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(self.hotkey_status.clone())
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("Profiles"),
                vec![
                    self.mark_target(
                        "Profiles",
                        t,
                        div().w_full().flex().flex_wrap().gap(px(6.0)).children(
                            self.settings
                                .profiles
                                .iter()
                                .enumerate()
                                .map(|(index, profile)| {
                                    let profile_name = profile.name.clone();
                                    let active = self
                                        .settings
                                        .active_profile
                                        .as_deref()
                                        .is_some_and(|name| name == profile.name);
                                    let label = format!(
                                        "{} {}{}",
                                        profile.name,
                                        if active { "•" } else { "" },
                                        ""
                                    );
                                    div()
                                        .id(SharedString::from(format!("profile-{index}")))
                                        .px(px(10.0))
                                        .py(px(5.0))
                                        .rounded(px(8.0))
                                        .bg(rgba(if active { t.accent } else { t.surface_muted }))
                                        .text_color(rgba(if active {
                                            t.accent_fg
                                        } else {
                                            t.text_primary
                                        }))
                                        .text_size(px(fonts::scaled_size(12.0)))
                                        .cursor_pointer()
                                        .child(label)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.settings.active_profile =
                                                Some(profile_name.clone());
                                            this.auto_save(cx);
                                        }))
                                }),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .flex()
                        .gap(px(6.0))
                        .child(button(
                            "btn-profile-new",
                            t,
                            loc::t("ProfileNew"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.new_profile(cx)),
                        ))
                        .children(self.settings.profiles.iter().enumerate().take(12).map(
                            |(index, profile)| {
                                let rename_title = loc::t_fmt("ProfileRename", &[&profile.name]);
                                let delete_title = loc::t_fmt("ProfileDelete", &[&profile.name]);
                                div()
                                    .flex()
                                    .gap(px(4.0))
                                    .child(button(
                                        SharedString::from(format!("profile-rename-{index}")),
                                        t,
                                        rename_title,
                                        ButtonVariant::Secondary,
                                        cx.listener(move |this, _, _, cx| {
                                            this.rename_profile(index, cx)
                                        }),
                                    ))
                                    .child(button(
                                        SharedString::from(format!("profile-delete-{index}")),
                                        t,
                                        delete_title,
                                        ButtonVariant::Destructive,
                                        cx.listener(move |this, _, _, cx| {
                                            this.delete_profile(index, cx)
                                        }),
                                    ))
                            },
                        ))
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("CheckForUpdates"),
                vec![
                    self.target_row(
                        "CheckUpdatesSetting",
                        t,
                        Some("\u{E9C5}"),
                        loc::t("CheckUpdatesSetting"),
                        ios_switch(
                            "sw-check-updates",
                            t,
                            self.settings.check_updates,
                            cx.listener(|this, _, _, cx| {
                                this.settings.check_updates = !this.settings.check_updates;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(self.update_status.clone())
                        .into_any_element(),
                    {
                        let mut controls = div().w_full().flex().gap(px(6.0)).child(button(
                            "btn-check-update",
                            t,
                            loc::t("CheckForUpdates"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, window, cx| this.check_updates(window, cx)),
                        ));
                        if let Some(release) = &self.update_release {
                            controls = controls.child(button(
                                "btn-download-update",
                                t,
                                loc::t_fmt("UpdateDownload", &[&release.version]),
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, window, cx| this.download_update(window, cx)),
                            ));
                        }
                        controls.into_any_element()
                    },
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("SectionSystem"),
                vec![
                    self.target_row(
                        "Startup",
                        t,
                        Some("\u{E7E8}"),
                        loc::t("Startup"),
                        ios_switch(
                            "sw-startup",
                            t,
                            self.startup_enabled,
                            cx.listener(|this, _, _, cx| {
                                let next = !this.startup_enabled;
                                win32::set_startup_enabled(next);
                                this.startup_enabled = win32::is_startup_enabled();
                                cx.notify();
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("Backup"),
                vec![
                    self.target_row(
                        "SettingsBackup",
                        t,
                        Some("\u{EDE1}"),
                        loc::t("SettingsBackup"),
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(button(
                                "btn-export",
                                t,
                                loc::t("ExportSettings"),
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, _, cx| {
                                    this.sync_dock_pins();
                                    let dst = settings::app_data_dir().join("settings.backup.json");
                                    let _ = settings::save_to(&dst, &this.settings);
                                    cx.notify();
                                }),
                            ))
                            .child(button(
                                "btn-import",
                                t,
                                loc::t("ImportSettings"),
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, _, cx| {
                                    let src = settings::app_data_dir().join("settings.backup.json");
                                    if src.exists() {
                                        this.settings = settings::load_from(&src);
                                        fonts::apply_settings(&this.settings);
                                        loc::set_language(&this.settings.language);
                                        this.widget_runtime.tick(&this.settings);
                                        this.save_settings(cx);
                                    }
                                }),
                            )),
                    )
                    .into_any_element(),
                    self.target_row(
                        "ResetDefaults",
                        t,
                        Some("\u{E777}"),
                        loc::t("ResetDefaults"),
                        button(
                            "btn-reset-all",
                            t,
                            loc::t("Reset"),
                            ButtonVariant::Destructive,
                            cx.listener(|this, _, _, cx| {
                                this.settings = DockSettings::default();
                                fonts::apply_settings(&this.settings);
                                loc::set_language(&this.settings.language);
                                this.widget_runtime.tick(&this.settings);
                                this.save_settings(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ];
        out.push(section_card(t, "Hotkeys", self.render_hotkeys(t, cx)).into_any_element());
        out.push(section_card(t, "AI Accounts", self.render_ai_auth(t, cx)).into_any_element());
        out.push(section_card(t, "Full Backup", self.render_backup_ui(t, cx)).into_any_element());
        out
    }

    fn render_about(&self, t: &UiTheme) -> Vec<AnyElement> {
        let data_dir = settings::app_data_dir().to_string_lossy().into_owned();
        let log_file = settings::app_data_dir()
            .join("opentaskbar.log")
            .to_string_lossy()
            .into_owned();
        vec![
            section_card(
                t,
                loc::t("Application"),
                vec![
                    self.target_row(
                        "Version",
                        t,
                        Some("\u{E946}"),
                        loc::t("Version"),
                        badge(t, concat!("v", env!("CARGO_PKG_VERSION")), true),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E1D2}"),
                        "GitHub",
                        button(
                            "btn-github",
                            t,
                            "Open Repository",
                            ButtonVariant::Secondary,
                            |_, _, _| {
                                pins::launch("https://github.com/alztrk/opentaskbar".to_owned());
                            },
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("Diagnostics"),
                vec![
                    self.target_row(
                        "DataFolder",
                        t,
                        Some("\u{E8B7}"),
                        loc::t("DataFolder"),
                        button(
                            "btn-open-data",
                            t,
                            loc::t("Open"),
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(data_dir.clone()),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "OpenLogFile",
                        t,
                        Some("\u{E9F5}"),
                        loc::t("OpenLogFile"),
                        button(
                            "btn-open-log",
                            t,
                            loc::t("Open"),
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(log_file.clone()),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "WindowsTaskbar",
                        t,
                        Some("\u{E777}"),
                        loc::t("WindowsTaskbar"),
                        button(
                            "btn-restore-tb",
                            t,
                            loc::t("Restore"),
                            ButtonVariant::Destructive,
                            |_, _, _| opentaskbar_shell::taskbar::restore_from_journal(),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }
}

/// Backing buffer for the header search field. GPUI uses this to route typing, paste and
/// IME input to the focused input element.
impl EntityInputHandler for SettingsView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let len = self.search_utf16_len();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        if start != range.start || end != range.end {
            *adjusted_range = Some(start..end);
        }
        let from = Self::byte_index_for_utf16(&self.search_text, start);
        let to = Self::byte_index_for_utf16(&self.search_text, end);
        Some(self.search_text[from..to].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.search_selection.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.unwrap_or_else(|| self.search_selection.clone());
        self.replace_search_range(range, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.unwrap_or_else(|| self.search_selection.clone());
        self.replace_search_range(range, new_text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<gpui::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<gpui::Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<gpui::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_dark = self.resolve_is_dark();
        let mut t = UiTheme::new(is_dark);
        t.border_width = self.settings.settings_border_thickness.clamp(0.0, 8.0) as f32;
        if let Some(value) = &self.settings.settings_border_color
            && let Some(color) = parse_hex_color(value)
        {
            t.border = color;
        }
        let width = window.viewport_size().width;
        let narrow = width < px(SIDEBAR_BREAKPOINT);
        let inset = px(if narrow { 20.0 } else { 32.0 });
        let content_width = width - inset * 2.0 - px(if narrow { 0.0 } else { SIDEBAR_WIDTH });
        let make_nav = |p: SettingsPage, cx: &mut Context<Self>| -> AnyElement {
            sidebar_nav_item(
                SharedString::from(format!("nav-{}", p.title())),
                &t,
                p.glyph(),
                p.title(),
                None,
                self.page == p,
                cx.listener(move |this, _, _, cx| {
                    this.open_dropdown = None;
                    if this.page != p {
                        this.page = p;
                        this.highlight_row = None;
                        this.scroll_anchor = None;
                        this.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
                        cx.notify();
                    }
                }),
            )
            .when(narrow, |item| item.w(px(156.0)))
            .into_any_element()
        };
        let top_nav: Vec<AnyElement> = SettingsPage::TOP
            .into_iter()
            .map(|p| make_nav(p, cx))
            .collect();
        let bottom_nav: Vec<AnyElement> = SettingsPage::BOTTOM
            .into_iter()
            .map(|p| make_nav(p, cx))
            .collect();

        let search_query = self.search_text.trim().to_owned();
        let content_blocks = if !search_query.is_empty() {
            self.render_search_results(&search_query, &t, cx)
        } else {
            match self.page {
                SettingsPage::Dock => self.render_dock(&t, cx),
                SettingsPage::Widgets => {
                    let columns = if content_width < px(WIDGET_COLUMNS_BREAKPOINT) {
                        1
                    } else {
                        2
                    };
                    self.render_widgets(&t, is_dark, columns, cx)
                }
                SettingsPage::DynamicIsland => self.render_dynamic_island(&t, cx),
                SettingsPage::Themes => self.render_themes(&t, cx),
                SettingsPage::Preferences => self.render_preferences(&t, cx),
                SettingsPage::About => self.render_about(&t),
            }
        };

        // The target row (if any) now carries a clone of this anchor. Schedule its deferred
        // scroll and consume the anchor so subsequent renders don't keep jumping back.
        // `open_search_result` forces the frame in which the callback runs.
        if let Some(anchor) = self.scroll_anchor.take() {
            let entity = cx.entity().downgrade();
            anchor.scroll_to(window, cx);
            window.on_next_frame(move |_, cx| {
                // Repaint with the offset the anchor just applied.
                let _ = entity.update(cx, |_, cx| cx.notify());
            });
        }

        let max_off: f32 = self.scroll_handle.max_offset().height.into();
        let cur_off: f32 = (-self.scroll_handle.offset().y).into();
        let viewport_h: f32 = self.scroll_handle.bounds().size.height.into();
        let scrollbar_el = if max_off > 2.0 && viewport_h > 40.0 {
            let total_h = viewport_h + max_off;
            let track_h = (viewport_h - 16.0).max(40.0);
            let thumb_h = ((viewport_h / total_h) * track_h).clamp(28.0, track_h);
            let ratio = (cur_off / max_off).clamp(0.0, 1.0);
            let thumb_top = 8.0 + ratio * (track_h - thumb_h);
            Some(
                div()
                    .absolute()
                    .top(px(thumb_top))
                    .right(px(5.0))
                    .w(px(5.0))
                    .h(px(thumb_h))
                    .rounded(px(3.0))
                    .bg(rgba(t.scrollbar_thumb)),
            )
        } else {
            None
        };

        let sidebar = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .justify_between()
            .gap(px(12.0))
            .p(px(14.0))
            .bg(rgba(t.sidebar_bg))
            .border_color(rgba(t.border))
            .when(narrow, |panel| panel.w_full().border_b_1())
            .when(!narrow, |panel| {
                panel.w(px(SIDEBAR_WIDTH)).h_full().border_r_1()
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .gap(px(4.0))
                    .when(narrow, |nav| nav.flex_wrap())
                    .when(!narrow, |nav| nav.flex_col())
                    .children(top_nav),
            )
            .child(
                div()
                    .w_full()
                    .pt(px(10.0))
                    .border_t_1()
                    .border_color(rgba(t.separator))
                    .flex()
                    .gap(px(4.0))
                    .when(narrow, |nav| nav.flex_wrap())
                    .when(!narrow, |nav| nav.flex_col())
                    .children(bottom_nav),
            );

        let mut page_content = div().w_full().flex().flex_col().gap(px(20.0));
        page_content = page_content.children(self.render_search(&t, cx));
        page_content = page_content.children(content_blocks);

        let mut scroll_wrap = div().flex_1().min_h_0().w_full().relative().child(
            div()
                .id("settings-scroll")
                .w_full()
                .h_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_handle)
                .on_scroll_wheel(cx.listener(|_, _, _, cx| {
                    cx.notify();
                }))
                .px(inset)
                .py(px(20.0))
                .child(page_content),
        );

        if let Some(sb) = scrollbar_el {
            scroll_wrap = scroll_wrap.child(sb);
        }

        let header_title = if search_query.is_empty() {
            self.page.title()
        } else {
            loc::t("SettingsSearchResults")
        };
        let header_subtitle = if search_query.is_empty() {
            self.page.description()
        } else {
            search_query.clone()
        };

        let main = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("settings-page-header-drag")
                    .w_full()
                    .flex_shrink_0()
                    .px(inset)
                    .py(px(if narrow { 12.0 } else { 18.0 }))
                    .border_b_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                        win32::start_native_window_drag(window);
                    })
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(22.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(header_title),
                    )
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .text_color(rgba(t.text_secondary))
                            .child(header_subtitle),
                    ),
            )
            .when(self.save_failed, |panel| {
                panel.child(
                    div()
                        .px(inset)
                        .py(px(8.0))
                        .bg(rgba(t.destructive_soft))
                        .text_color(rgba(t.destructive))
                        .text_size(px(fonts::scaled_size(13.0)))
                        .child(loc::t("SettingsSaveError")),
                )
            })
            .child(scroll_wrap);

        // Header search: icon on the left, a native text input in the middle and a clear
        // button on the right. The input is outside the draggable titlebar region.
        let search_focused = self.search_focus.is_focused(window);
        let caret_byte = Self::byte_index_for_utf16(&self.search_text, self.search_selection.start);
        let (text_before, text_after) = self.search_text.split_at(caret_byte);
        let search_value = if self.search_text.is_empty() {
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(fonts::scaled_size(12.0)))
                .text_color(rgba(t.text_secondary))
                .child(loc::t("SettingsSearchPlaceholder"))
        } else {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .overflow_hidden()
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_primary))
                        .child(text_before.to_owned()),
                )
                .when(search_focused, |line| {
                    line.child(
                        div()
                            .flex_shrink_0()
                            .w(px(1.0))
                            .h(px(13.0))
                            .bg(rgba(t.accent)),
                    )
                })
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_primary))
                        .child(text_after.to_owned()),
                )
        };

        let clear_hover = t.surface_hover;
        let mut search_inner = div()
            .id("settings-search-field")
            .track_focus(&self.search_focus)
            .tab_index(0)
            .w(px(230.0))
            .max_w(px(280.0))
            .h(px(26.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .px(px(9.0))
            .rounded(px(7.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(if search_focused { t.accent } else { t.border }))
            .cursor_text()
            .focus({
                let accent = t.accent;
                move |style| style.border_color(rgba(accent))
            })
            .on_click(cx.listener(|this: &mut Self, _, window, cx| {
                this.search_focus.focus(window);
                cx.notify();
            }))
            .on_key_down(
                cx.listener(|this: &mut Self, event: &KeyDownEvent, window, cx| {
                    if !this.search_focus.is_focused(window) {
                        return;
                    }
                    match event.keystroke.key.as_str() {
                        "backspace" => {
                            let selection = this.search_selection.clone();
                            let end = selection.end.min(this.search_utf16_len());
                            if end > selection.start {
                                this.replace_search_range(selection.start..end, "");
                            } else if end > 0 {
                                this.replace_search_range(end - 1..end, "");
                            }
                            cx.notify();
                            cx.stop_propagation();
                        }
                        "escape" => {
                            this.clear_search();
                            this.highlight_row = None;
                            this.scroll_anchor = None;
                            this.focus.focus(window);
                            cx.notify();
                            cx.stop_propagation();
                        }
                        "enter" => {
                            if let Some(entry) = settings_search::search(
                                &settings_search::catalog(),
                                this.search_text.trim(),
                            )
                            .first()
                            .cloned()
                            {
                                this.open_search_result((*entry).clone(), cx);
                            }
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }),
            )
            .child(icon_glyph("\u{E721}", 13.0, t.text_secondary))
            .child(search_value);

        if !self.search_text.is_empty() {
            search_inner = search_inner.child(
                div()
                    .id("settings-search-clear")
                    .flex_shrink_0()
                    .w(px(17.0))
                    .h(px(17.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgba(clear_hover)))
                    .on_click(cx.listener(|this: &mut Self, _, window, cx| {
                        this.clear_search();
                        this.highlight_row = None;
                        this.scroll_anchor = None;
                        this.search_focus.focus(window);
                        cx.notify();
                    }))
                    .child(icon_glyph("\u{E711}", 10.0, t.text_secondary)),
            );
        }

        let search_field =
            TextInput::new(search_inner, self.search_focus.clone(), cx.entity()).into_any_element();

        let hover_cap = t.surface_hover;
        let close_hover = t.destructive;
        let titlebar = div()
            .w_full()
            .h(px(36.0))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .bg(rgba(t.sidebar_bg))
            .border_b_1()
            .border_color(rgba(t.border))
            .child(
                div()
                    .id("settings-titlebar-drag")
                    .flex_1()
                    .h_full()
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                        win32::start_native_window_drag(window);
                    })
                    .child(icon_glyph("\u{E713}", 13.0, t.text_secondary))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.text_primary))
                            .child(loc::t("SettingsTitle")),
                    ),
            )
            .child(div().flex_shrink_0().mx(px(12.0)).child(search_field))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h_full()
                    .child(
                        div()
                            .id("cap-min")
                            .w(px(44.0))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(hover_cap)))
                            .on_click(|_, window, _| window.minimize_window())
                            .child(icon_glyph("\u{E921}", 10.0, t.text_primary)),
                    )
                    .child(
                        div()
                            .id("cap-max")
                            .w(px(44.0))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(hover_cap)))
                            .on_click(|_, window, _| window.zoom_window())
                            .child(icon_glyph("\u{E922}", 10.0, t.text_primary)),
                    )
                    .child(
                        div()
                            .id("cap-close")
                            .w(px(46.0))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(close_hover)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.flush_pending_save(cx);
                                window.remove_window();
                            }))
                            .child(icon_glyph("\u{E8BB}", 10.0, t.text_primary)),
                    ),
            );

        let panel_overlay = self.render_widget_panel_overlay(&t, cx);
        div()
            .id("settings")
            .track_focus(&self.focus)
            .tab_group()
            .on_action(|_: &TabNext, window, _| window.focus_next())
            .on_action(|_: &TabPrevious, window, _| window.focus_prev())
            .on_action(cx.listener(|this, _: &CloseWidgetPanel, window, cx| {
                this.open_dropdown = None;
                this.close_widget_panel(window);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.open_dropdown.is_some() {
                        this.open_dropdown = None;
                        cx.notify();
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleFlyoutPreview, _, cx| {
                this.flyout_preview = !this.flyout_preview;
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                if this.hotkey_capture.is_some() {
                    if key == "escape" {
                        this.hotkey_capture = None;
                        cx.notify();
                        return;
                    }
                    if matches!(
                        key,
                        "control" | "alt" | "shift" | "windows" | "capslock" | "function"
                    ) {
                        return;
                    }
                    let mut parts: Vec<String> = Vec::new();
                    if event.keystroke.modifiers.control {
                        parts.push("Ctrl".to_owned());
                    }
                    if event.keystroke.modifiers.alt {
                        parts.push("Alt".to_owned());
                    }
                    if event.keystroke.modifiers.shift {
                        parts.push("Shift".to_owned());
                    }
                    if event.keystroke.modifiers.platform {
                        parts.push("Win".to_owned());
                    }
                    let raw = event
                        .keystroke
                        .key_char
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(key);
                    let named = if raw.chars().count() == 1 {
                        raw.to_uppercase()
                    } else {
                        let mut chars = raw.chars();
                        chars.next().map_or_else(String::new, |c| {
                            c.to_uppercase().collect::<String>() + chars.as_str()
                        })
                    };
                    parts.push(named);
                    if let Some(which) = this.hotkey_capture.take() {
                        set_extra_string(&mut this.settings, which, &parts.join("+"));
                        this.auto_save(cx);
                    }
                }
            }))
            .relative()
            .w_full()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(rgba(t.canvas_bg))
            .font_family(SharedString::from(
                if self.settings.ui_font_family.trim().is_empty() {
                    fonts::native_ui_face().to_owned()
                } else {
                    self.settings.ui_font_family.clone()
                },
            ))
            .text_size(px(fonts::scaled_size(14.0)))
            .text_color(rgba(t.text_primary))
            .child(titlebar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .when(narrow, |root| root.flex_col())
                    .overflow_hidden()
                    .child(sidebar)
                    .child(main),
            )
            .when(panel_overlay.is_some(), |root| {
                root.child(panel_overlay.unwrap())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DockSettings, build_backup, civil_from_days, inspect_backup, safe_backup_path, zip_store,
        zip_unstore,
    };

    #[test]
    fn stored_zip_round_trips_entries() {
        let entries = vec![
            ("manifest.json".to_owned(), b"{\"Format\":\"x\"}".to_vec()),
            ("theme/a.json".to_owned(), vec![0u8, 1, 2, 255]),
        ];
        let bytes = zip_store(&entries);
        let read = zip_unstore(&bytes).expect("stored zip must be readable");
        assert_eq!(read.len(), 2);
        assert_eq!(read["manifest.json"], entries[0].1);
        assert_eq!(read["theme/a.json"], entries[1].1);
    }

    #[test]
    fn full_backup_includes_settings_pins_and_theme_files() {
        let mut settings = DockSettings::default();
        settings.user_pinned_apps.push("app.exe".to_owned());
        settings.weather_city = "Izmir, Turkey".to_owned();
        let bytes = build_backup(&settings);
        let preview = inspect_backup(&bytes);
        assert!(preview.valid, "{:?}", preview.error);
        assert_eq!(preview.pinned_apps, 1);
        assert_eq!(
            preview.settings.map(|s| s.weather_city),
            Some("Izmir, Turkey".to_owned())
        );
    }

    #[test]
    fn backup_paths_reject_traversal() {
        assert!(safe_backup_path("theme/a.json").is_some());
        assert!(safe_backup_path("../secret").is_none());
        assert!(safe_backup_path("C:\\secret").is_none());
        assert!(safe_backup_path("a/b/../../c").is_none());
    }

    #[test]
    fn utc_stamp_day_conversion_matches_unix_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
    }
}

#[allow(clippy::items_after_test_module)]
pub fn run_settings_window() {
    let loaded = settings::load_from(&settings::settings_path());
    Application::new()
        .with_assets(fonts::EmbeddedAssets)
        .run(move |cx: &mut App| {
        let _ = fonts::load_bundled_fonts(cx);
        cx.bind_keys([
            KeyBinding::new("tab", TabNext, None),
            KeyBinding::new("shift-tab", TabPrevious, None),
            KeyBinding::new("escape", CloseWidgetPanel, None),
            KeyBinding::new("f9", ToggleFlyoutPreview, None),
        ]);
        let bounds = Bounds::centered(None, size(px(WIN_W), px(WIN_H)), cx);
        let _ = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
                titlebar: Some(TitlebarOptions {
                    title: Some(loc::t("SettingsTitle").into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| SettingsView::new(loaded.clone(), window, cx)),
        );
        cx.activate(true);
    });
}
