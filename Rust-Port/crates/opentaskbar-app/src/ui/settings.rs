//! On-demand GPUI Settings window (`OpenTaskBar.exe --settings`).
//!
//! Grouped settings with a responsive navigation pane and immediate dock updates.
//! Includes the full `.otbackup` archive UI, theme editor/gallery, hotkey capture,
//! dynamic search catalogue and widget panel host (C# `SettingsWindow` parity).

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Instant;

use gpui::{
    AnyElement, App, Application, Bounds, Context, FocusHandle, InteractiveElement, KeyBinding,
    KeyDownEvent, MouseButton, ScrollHandle, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowControlArea, WindowOptions, actions, div, prelude::*, px, rgba, size,
};
use opentaskbar_core::settings::{
    self, AppTheme, AutoHideMode, DockPosition, DockSettings, DockSize, SurfaceBackground,
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

use crate::dock::dcomp::WidgetChipSpec;
use crate::platform::{pins, win32};
use crate::ui::components::{
    ButtonVariant, UiTheme, badge, button, icon_glyph, ios_switch, live_slider, section_card,
    segmented_item, segmented_track, setting_row, sidebar_nav_item,
};
use crate::ui::prompt;
use crate::ui::settings_search::{SearchCatalog, SearchEntry, widget_has_settings};
use crate::ui::text_input::{apply_keystroke, clamp_numeric};
use crate::widgets;

pub const SETTINGS_ARG: &str = "--settings";
pub const RELOAD_MSG_NAME: &str = "OpenTaskBarReloadSettings";

const WIN_W: f32 = 920.0;
const WIN_H: f32 = 680.0;
const MIN_WIDTH: f32 = 540.0;
const MIN_HEIGHT: f32 = 428.0;
const SIDEBAR_BREAKPOINT: f32 = 680.0;
const SIDEBAR_WIDTH: f32 = 196.0;
const WIDGET_COLUMNS_BREAKPOINT: f32 = 520.0;

actions!(settings_navigation, [TabNext, TabPrevious]);
actions!(settings_panels, [CloseWidgetPanel, ToggleFlyoutPreview]);

pub fn spawn_settings_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg(SETTINGS_ARG).spawn();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Dock,
    Widgets,
    Themes,
    Preferences,
    About,
}

impl SettingsPage {
    const TOP: [Self; 3] = [Self::Dock, Self::Widgets, Self::Themes];
    const BOTTOM: [Self; 2] = [Self::Preferences, Self::About];

    fn from_name(name: &str) -> Self {
        match name {
            "Widgets" => Self::Widgets,
            "Themes" => Self::Themes,
            "Preferences" => Self::Preferences,
            "About" => Self::About,
            _ => Self::Dock,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Dock => "Dock",
            Self::Widgets => "Widgets",
            Self::Themes => "Themes",
            Self::Preferences => "Preferences",
            Self::About => "About",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Dock => "\u{E7F4}",
            Self::Widgets => "\u{F0E2}",
            Self::Themes => "\u{E790}",
            Self::Preferences => "\u{E713}",
            Self::About => "\u{E946}",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Dock => "Adjust the dock layout and how it behaves on your displays.",
            Self::Widgets => "Choose which widgets appear on the dock and configure them.",
            Self::Themes => "Customize the dock colors, materials, and effects.",
            Self::Preferences => "Manage startup and settings backups.",
            Self::About => "View the version and open local application files.",
        }
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
        "netspeed" => "\u{E701}",
        "battery" => "\u{E83F}",
        "bluetoothbattery" => "\u{E702}",
        "storage" => "\u{EDA2}",
        "media" => "\u{E8D6}",
        "audio" => "\u{E767}",
        "quicksettings" => "\u{E9E9}",
        "keyboard" => "\u{E765}",
        "focus" => "\u{E708}",
        _ => "\u{F0E2}",
    }
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
    settings: DockSettings,
    widget_runtime: widgets::WidgetRuntime,
    widget_filter: &'static str,
    startup_enabled: bool,
    scroll_handle: ScrollHandle,
    save_failed: bool,
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
            settings,
            widget_runtime,
            widget_filter: "All",
            startup_enabled: win32::is_startup_enabled(),
            scroll_handle: ScrollHandle::new(),
            save_failed: false,
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
        self.sync_dock_pins();
        self.widget_runtime.tick(&self.settings);
        self.save_settings(cx);
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
                                        .text_size(px(16.0))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(format!("{id} options")),
                                )
                                .child(button(
                                    "btn-panel-close",
                                    t,
                                    "Close",
                                    ButtonVariant::Secondary,
                                    cx.listener(|this, _, window, cx| {
                                        this.close_widget_panel(window);
                                        cx.notify();
                                    }),
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgba(t.text_secondary))
                                .child("C# WidgetPanelHost parity: Escape closes, focus returns, re-open suppressed for 300 ms."),
                        )
                        .child(
                            button(
                                "btn-panel-preview",
                                t,
                                if self.flyout_preview {
                                    "Preview: On"
                                } else {
                                    "Preview: Off"
                                },
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, _, cx| {
                                    this.flyout_preview = !this.flyout_preview;
                                    cx.notify();
                                }),
                            ),
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
                                .on_click(cx.listener(|this, _, _, _| {
                                    this.preview_flyout(this.widget_panel.unwrap_or("widget"));
                                }))
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(rgba(t.text_secondary))
                                        .child("Click to preview C2 flyout blocks"),
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
            let seg = segmented_track(
                t,
                options
                    .iter()
                    .map(|option| {
                        let key: &'static str = key;
                        let option: &'static str = option;
                        segmented_item(
                            SharedString::from(format!("{key}-{option}")),
                            t,
                            option,
                            current == option,
                            cx.listener(move |this, _, _, cx| {
                                set_extra_string(&mut this.settings, key, option);
                                this.auto_save(cx);
                            }),
                        )
                    })
                    .collect(),
            );
            rows.push(setting_row(t, Some("\u{E740}"), label, seg).into_any_element());
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
        let country = self.weather_country.clone();
        let city_index = self.weather_city_index;
        let country_count = COUNTRIES.len();
        let city_count = cities.len();
        vec![
            setting_row(
                t,
                Some("\u{E7B0}"),
                "Country",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(button(
                        "btn-country-prev",
                        t,
                        "‹",
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            let idx = COUNTRIES
                                .iter()
                                .position(|c| *c == this.weather_country)
                                .unwrap_or(0);
                            this.weather_country =
                                COUNTRIES[(idx + COUNTRIES.len() - 1) % COUNTRIES.len()].to_owned();
                            this.weather_city_index = 0;
                            cx.notify();
                        }),
                    ))
                    .child(
                        div()
                            .w(px(120.0))
                            .text_align(gpui::TextAlign::Center)
                            .child(self.weather_country.clone()),
                    )
                    .child(button(
                        "btn-country-next",
                        t,
                        "›",
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            let idx = COUNTRIES
                                .iter()
                                .position(|c| *c == this.weather_country)
                                .unwrap_or(0);
                            this.weather_country =
                                COUNTRIES[(idx + 1) % COUNTRIES.len()].to_owned();
                            this.weather_city_index = 0;
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element(),
            setting_row(
                t,
                Some("\u{E1C9}"),
                "City / Province",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(button(
                        "btn-city-prev",
                        t,
                        "‹",
                        ButtonVariant::Secondary,
                        cx.listener(move |this, _, _, cx| {
                            let _ = (&country, city_index, country_count);
                            let len = cities_for(&this.weather_country).len();
                            this.weather_city_index = (this.weather_city_index + len - 1) % len;
                            cx.notify();
                        }),
                    ))
                    .child(
                        div()
                            .w(px(120.0))
                            .text_align(gpui::TextAlign::Center)
                            .child(city.to_owned()),
                    )
                    .child(button(
                        "btn-city-next",
                        t,
                        "›",
                        ButtonVariant::Secondary,
                        cx.listener(move |this, _, _, cx| {
                            let _ = city_count;
                            let len = cities_for(&this.weather_country).len();
                            this.weather_city_index = (this.weather_city_index + 1) % len;
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element(),
            setting_row(
                t,
                Some("\u{E753}"),
                "Weather Location",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(div().text_size(px(12.0)).child(label))
                    .child(button(
                        "btn-weather-apply",
                        t,
                        "Apply",
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            let cities = cities_for(&this.weather_country);
                            let city = cities
                                .get(this.weather_city_index)
                                .copied()
                                .unwrap_or("Istanbul");
                            this.settings.weather_city =
                                format!("{city}, {}", this.weather_country);
                            this.auto_save(cx);
                        }),
                    )),
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
        let seg = segmented_track(
            t,
            THEME_TOKEN_CATEGORIES
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    segmented_item(
                        SharedString::from(format!("token-cat-{name}")),
                        t,
                        *name,
                        self.theme_category == i,
                        cx.listener(move |this, _, _, cx| {
                            this.theme_category = i;
                            cx.notify();
                        }),
                    )
                })
                .collect(),
        );
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

    fn render_widget_preview(&self, id: &str, t: &UiTheme) -> AnyElement {
        let is_dark = match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        };
        let pal = crate::design::palette(is_dark);
        let c_rgba = |c: (u8, u8, u8), a: u8| rgba(crate::design::gpui_rgba(c, a));
        let chip_bg = if is_dark { 0xffffff10 } else { 0x00000008 };
        let chip_bd = if is_dark { 0xffffff1c } else { 0x00000016 };
        let track_bg = if is_dark { 0xffffff1f } else { 0x00000018 };
        let spec = match id {
            "weather" => WidgetChipSpec::Weather {
                available: true,
                icon_color: pal.warning,
                is_rain: false,
                temp: "21°".to_owned(),
                place: if self.settings.weather_city.is_empty() {
                    "Istanbul".to_owned()
                } else {
                    self.settings.weather_city.clone()
                },
                condition: "Clear".to_owned(),
                today_range: Some("24° / 16°".to_owned()),
            },
            "stats" => WidgetChipSpec::SystemStats {
                cpu_text: "24%".to_owned(),
                cpu_color: pal.widget_blue,
                ram_text: "58%".to_owned(),
                ram_color: pal.widget_accent,
            },
            "netspeed" => WidgetChipSpec::NetSpeed {
                down: "18.4 MB/s".to_owned(),
                up: "3.2 MB/s".to_owned(),
            },
            "battery" => WidgetChipSpec::BatteryLiquid {
                present: true,
                percent: 84,
                charging: true,
                wave_phase: 0.35,
            },
            "bluetoothbattery" => WidgetChipSpec::Bluetooth {
                devices: vec![(90, false, false)],
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
            "aiusage_all" => WidgetChipSpec::Simple {
                icon: crate::dock::dcomp::ChipIcon::Sparkle,
                icon_color: pal.widget_accent,
                top: "AI · 6".to_owned(),
                bottom: "Claude 42%".to_owned(),
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
                        vec![("5h".to_owned(), 0.35, pal.widget_accent, "3h".to_owned())],
                    ),
                    (
                        "Claude".to_owned(),
                        vec![("7d".to_owned(), 0.62, pal.widget_accent, "4d".to_owned())],
                    ),
                    (
                        "Argon".to_owned(),
                        vec![("7d".to_owned(), 0.22, pal.widget_accent, "6d".to_owned())],
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
                .h(px(40.0))
                .max_w_full()
                .px(px(10.0))
                .rounded(px(9.0))
                .bg(rgba(chip_bg))
                .border_1()
                .border_color(rgba(chip_bd))
                .flex()
                .items_center()
                .gap(px(7.0))
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
                            .text_size(px(14.0))
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
                                    .text_size(px(10.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(date),
                            )
                            .child(
                                div()
                                    .text_size(px(9.0))
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
                            .text_size(px(11.0))
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
                                .text_size(px(15.0))
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
                                        .text_size(px(10.0))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(place),
                                )
                                .child(
                                    div()
                                        .text_size(px(9.0))
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
                                        .text_size(px(9.0))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child("Today"),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.0))
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
                            .text_size(px(12.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(percent),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
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
                            .text_size(px(12.0))
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
            WidgetChipSpec::SystemStats {
                cpu_text,
                cpu_color,
                ram_text,
                ram_color,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(9.0))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child("CPU"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(c_rgba(cpu_color, 0xff))
                                    .child(cpu_text),
                            ),
                    )
                    .child(vdiv())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(9.0))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child("RAM"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(c_rgba(ram_color, 0xff))
                                    .child(ram_text),
                            ),
                    ),
            ),
            WidgetChipSpec::NetSpeed { down, up } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_glyph("\u{E701}", 13.0, t.dock_preview_sub))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(3.0))
                                    .child(icon_glyph(
                                        "\u{E74B}",
                                        9.0,
                                        crate::design::gpui_rgba(pal.success, 0xff),
                                    ))
                                    .child(
                                        div()
                                            .text_size(px(10.0))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgba(t.dock_preview_fg))
                                            .child(down),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(3.0))
                                    .child(icon_glyph(
                                        "\u{E74A}",
                                        9.0,
                                        crate::design::gpui_rgba(pal.widget_blue, 0xff),
                                    ))
                                    .child(
                                        div()
                                            .text_size(px(10.0))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgba(t.dock_preview_fg))
                                            .child(up),
                                    ),
                            ),
                    ),
            ),
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
                                    .text_size(px(11.0))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child("—"),
                            ),
                    )
                } else {
                    let ratio = (percent as f32 / 100.0).clamp(0.02, 1.0);
                    let fill_col = if percent <= 20 {
                        (0xb2, 0x60, 0x67)
                    } else if percent <= 50 {
                        (0xb9, 0x95, 0x63)
                    } else {
                        (0x58, 0xa8, 0x9c)
                    };
                    let card_w = 94.0_f32;
                    let fill_w = (card_w * ratio).round();
                    div()
                        .relative()
                        .w(px(card_w))
                        .h(px(40.0))
                        .rounded(px(10.0))
                        .bg(rgba(chip_bg))
                        .border_1()
                        .border_color(rgba(chip_bd))
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
                                .gap(px(4.0))
                                .when(charging, |row| {
                                    row.child(icon_glyph("\u{E945}", 11.0, 0xffffffff))
                                })
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(rgba(0xffffffff))
                                        .child(format!("{percent}%")),
                                ),
                        )
                        .into_any_element()
                }
            }
            WidgetChipSpec::Bluetooth { devices } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icon_glyph("\u{E702}", 13.0, t.dock_preview_sub))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(
                                devices
                                    .first()
                                    .map_or_else(|| "—".to_owned(), |(p, _, _)| format!("{p}%")),
                            ),
                    ),
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
                                        .text_size(px(10.0))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(drv),
                                )
                                .child(
                                    div()
                                        .text_size(px(9.0))
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
                                    .text_size(px(10.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(9.0))
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
                            .text_size(px(12.0))
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
                    .gap(px(5.0))
                    .children(pills.into_iter().map(|(glyph, active)| {
                        div()
                            .size(px(24.0))
                            .rounded(px(6.0))
                            .bg(rgba(if active {
                                crate::design::gpui_rgba(pal.selected_fill, 0xff)
                            } else {
                                track_bg
                            }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .font_family("Segoe Fluent Icons")
                                    .text_size(px(11.0))
                                    .text_color(rgba(if active {
                                        crate::design::gpui_rgba(pal.selected_fg, 0xff)
                                    } else {
                                        t.dock_preview_fg
                                    }))
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
                    .text_size(px(12.0))
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
                                            .text_size(px(9.0))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(lbl),
                                    )
                                    .child(mini_bar(used, col, 48.0))
                                    .child(
                                        div()
                                            .text_size(px(9.0))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(reset),
                                    )
                            }),
                        )),
                )
            }
            WidgetChipSpec::AiAntigravity { columns } => {
                chip(div().flex().items_center().gap(px(6.0)).children(
                    columns.into_iter().take(3).map(|(name, rows)| {
                        let (used, col) = rows
                            .first()
                            .map_or((0.0, pal.widget_accent), |(_, u, c, _)| (*u, *c));
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(9.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(name),
                            )
                            .child(mini_bar(used, col, 34.0))
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
                                    .text_size(px(9.0))
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
                                    .text_size(px(11.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(value_text),
                            )
                            .child(
                                div()
                                    .text_size(px(9.0))
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
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(9.0))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(gpui::FontWeight::BOLD)
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
                                    .text_size(px(11.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(top),
                            )
                            .child(
                                div()
                                    .text_size(px(9.0))
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

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        self.save_failed = settings::save_to(&settings::settings_path(), &self.settings).is_err();
        if !self.save_failed {
            notify_docks_reload();
        }
        cx.notify();
    }

    fn render_dock(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let align_seg = segmented_track(
            t,
            [
                ("Left", DockPosition::Left),
                ("Center", DockPosition::Center),
                ("Right", DockPosition::Right),
            ]
            .into_iter()
            .map(|(label, pos)| {
                segmented_item(
                    SharedString::from(format!("pos-{label}")),
                    t,
                    label,
                    self.settings.position == pos,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.position = pos;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let size_seg = segmented_track(
            t,
            [
                ("Compact", DockSize::Compact),
                ("Medium", DockSize::Medium),
                ("Large", DockSize::Large),
            ]
            .into_iter()
            .map(|(label, sz)| {
                segmented_item(
                    SharedString::from(format!("sz-{label}")),
                    t,
                    label,
                    self.settings.size == sz,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.size = sz;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let autohide_seg = segmented_track(
            t,
            [
                ("Off", AutoHideMode::Disabled),
                ("Smart", AutoHideMode::Smart),
                ("Always", AutoHideMode::Always),
            ]
            .into_iter()
            .map(|(label, mode)| {
                segmented_item(
                    SharedString::from(format!("ah-{label}")),
                    t,
                    label,
                    self.settings.auto_hide == mode,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.auto_hide = mode;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let scale_pct = (self.settings.dock_scale * 100.0).round() as i32;
        let width_label = self.settings.dock_custom_width_dips.map_or_else(
            || "Width (Auto)".to_owned(),
            |w| format!("Width ({w:.0}px)"),
        );

        let mut out = vec![
            section_card(
                t,
                "Layout & Dimensions",
                vec![
                    setting_row(t, Some("\u{E8E3}"), "Alignment", align_seg).into_any_element(),
                    setting_row(t, Some("\u{E740}"), "Dock Size", size_seg).into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E8A3}"),
                        "Scale",
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
                    setting_row(
                        t,
                        Some("\u{E74A}"),
                        "Bottom Margin",
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
                    setting_row(
                        t,
                        Some("\u{E9A9}"),
                        "Icon Padding",
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
                    setting_row(
                        t,
                        Some("\u{E740}"),
                        "Full Width",
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
                            "Reset",
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
                "Behavior & Displays",
                vec![
                    setting_row(t, Some("\u{E740}"), "Auto-Hide", autohide_seg).into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E7F4}"),
                        "All Monitors",
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
                    setting_row(
                        t,
                        Some("\u{E708}"),
                        "Focus Mode",
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
                    setting_row(
                        t,
                        Some("\u{E765}"),
                        "Reveal on Win Key",
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
                    setting_row(
                        t,
                        Some("\u{E7F4}"),
                        "Current Desktop Only",
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
                "Dock Utilities",
                vec![
                    setting_row(
                        t,
                        Some("\u{E70E}"),
                        "Background Apps",
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
                    setting_row(
                        t,
                        Some("\u{E74D}"),
                        "Recycle Bin",
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
                    setting_row(
                        t,
                        Some("\u{E7F8}"),
                        "Show Desktop Strip",
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
                ],
            )
            .into_any_element(),
        ];
        out.extend(self.render_dimension_controls(t, cx));
        out.extend(self.render_folder_stacks(t, cx));
        out.push(
            section_card(
                t,
                "Settings Search Catalog",
                vec![
                    div()
                        .w_full()
                        .text_size(px(12.0))
                        .text_color(rgba(t.text_secondary))
                        .child("Use the search box above to find any row by label or WS-A field name. Widget panels open from search results.")
                        .into_any_element(),
                ],
            )
            .into_any_element(),
        );
        out
    }

    fn render_themes(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme_seg = segmented_track(
            t,
            [
                ("System", AppTheme::System),
                ("Dark", AppTheme::Dark),
                ("Light", AppTheme::Light),
            ]
            .into_iter()
            .map(|(label, th)| {
                segmented_item(
                    SharedString::from(format!("theme-{label}")),
                    t,
                    label,
                    self.settings.theme == th,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.theme = th;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let bg_seg = segmented_track(
            t,
            [
                ("Glass", SurfaceBackground::Glass),
                ("Solid", SurfaceBackground::Solid),
                ("None", SurfaceBackground::None),
            ]
            .into_iter()
            .map(|(label, bg)| {
                segmented_item(
                    SharedString::from(format!("bg-{label}")),
                    t,
                    label,
                    self.settings.dock_background == bg,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.dock_background = bg;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let opacity_pct = (self.settings.dock_opacity * 100.0).round() as i32;
        let radius_label = self
            .settings
            .dock_corner_radius
            .map_or_else(|| "Auto".to_owned(), |r| format!("{r} px"));

        let mut out = vec![
            section_card(
                t,
                "Theme & Surfaces",
                vec![
                    setting_row(t, Some("\u{E790}"), "Theme", theme_seg).into_any_element(),
                    setting_row(t, Some("\u{E790}"), "Background", bg_seg).into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E706}"),
                        "Opacity",
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
                    setting_row(
                        t,
                        Some("\u{F158}"),
                        "Corner Radius",
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
                "Motion",
                vec![
                    setting_row(
                        t,
                        Some("\u{E945}"),
                        "Smooth Animations",
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
                    setting_row(
                        t,
                        Some("\u{E71E}"),
                        "Magnification",
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
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ];
        out.extend(self.render_theme_gallery(t, cx));
        out.extend(self.render_theme_editor(t, cx));
        out
    }

    fn render_widget_card(
        &self,
        w: &'static widgets::WidgetDescriptor,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wid = w.id;
        let enabled = self.settings.is_widget_enabled(wid);
        let preview = self.render_widget_preview(wid, t);
        let border_col = if enabled { t.accent } else { t.border };
        let card_hover = t.surface_hover;
        let focus_color = t.text_primary;
        let panel_button = button(
            SharedString::from(format!("wpanel-{wid}")),
            t,
            "Options",
            ButtonVariant::Secondary,
            cx.listener(move |this, _, window, cx| {
                this.open_widget_panel(wid, window);
                cx.notify();
            }),
        );

        let card = div()
            .id(SharedString::from(format!("wcard-{wid}")))
            .flex_1()
            .min_w_0()
            .h(px(120.0))
            .flex()
            .flex_col()
            .justify_between()
            .gap(px(8.0))
            .p(px(11.0))
            .rounded(px(12.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(border_col))
            .cursor_pointer()
            .tab_index(0)
            .focus(move |style| style.border_color(rgba(focus_color)))
            .hover(move |s| s.bg(rgba(card_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let cur = this.settings.is_widget_enabled(wid);
                    if !cur && this.settings.focus_mode && wid != "clock" && wid != "focus" {
                        this.settings.focus_mode = false;
                    }
                    this.settings.set_widget_enabled(wid, !cur);
                    this.auto_save(cx);
                }),
            )
            .child(
                div()
                    .w_full()
                    .h(px(58.0))
                    .px(px(8.0))
                    .rounded(px(9.0))
                    .bg(rgba(t.dock_preview_bg))
                    .border_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .child(preview),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .min_w_0()
                            .child(icon_glyph(
                                widget_glyph(wid),
                                13.0,
                                if enabled { t.accent } else { t.text_secondary },
                            ))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.text_primary))
                                    .child(w.name),
                            ),
                    )
                    .when(enabled, |row| {
                        row.child(icon_glyph("\u{E73E}", 12.0, t.accent))
                    })
                    .when(widget_has_settings(wid), |row| row.child(panel_button)),
            );

        card.into_any_element()
    }

    fn render_widgets(
        &self,
        t: &UiTheme,
        columns: usize,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        out.push(
            section_card(t, "Weather Location", self.render_weather_picker(t, cx))
                .into_any_element(),
        );
        let active_count = widgets::ALL_WIDGETS
            .iter()
            .filter(|w| self.settings.is_widget_enabled(w.id))
            .count();

        let filter_pills = div()
            .w_full()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(6.0))
            .children(
                std::iter::once("All")
                    .chain(std::iter::once("Active"))
                    .chain(widgets::WIDGET_CATEGORIES.iter().copied())
                    .map(|cat| {
                        let active = self.widget_filter == cat;
                        let bg = if active { t.accent } else { t.surface_muted };
                        let fg = if active {
                            t.accent_fg
                        } else {
                            t.text_secondary
                        };
                        let hov = if active { t.accent } else { t.surface_hover };
                        div()
                            .id(SharedString::from(format!("wflt-{cat}")))
                            .px(px(10.0))
                            .py(px(5.0))
                            .rounded(px(999.0))
                            .bg(rgba(bg))
                            .text_size(px(12.0))
                            .font_weight(if active {
                                gpui::FontWeight::SEMIBOLD
                            } else {
                                gpui::FontWeight::MEDIUM
                            })
                            .text_color(rgba(fg))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(hov)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.widget_filter = cat;
                                cx.notify();
                            }))
                            .child(cat)
                    }),
            );

        out.push(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(badge(
                            t,
                            format!("{active_count} / {} active", widgets::ALL_WIDGETS.len()),
                            active_count > 0,
                        ))
                        .child(button(
                            "btn-clear-widgets",
                            t,
                            "Clear All",
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_clock_widget = false;
                                this.settings.show_system_stats_widget = false;
                                this.settings.enabled_widgets.clear();
                                this.auto_save(cx);
                            }),
                        )),
                )
                .child(filter_pills)
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
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgba(t.text_secondary))
                    .child(cat)
                    .into_any_element(),
            );
            for group in items.chunks(columns) {
                let mut row = div().w_full().flex().gap(px(10.0));
                for &w in group {
                    row = row.child(self.render_widget_card(w, t, cx));
                }
                if group.len() < columns {
                    row = row.child(div().flex_1());
                }
                out.push(row.into_any_element());
            }
        }
        out
    }

    fn render_preferences(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![
            section_card(
                t,
                "System",
                vec![
                    setting_row(
                        t,
                        Some("\u{E7E8}"),
                        "Start with Windows",
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
                "Backup",
                vec![
                    setting_row(
                        t,
                        Some("\u{EDE1}"),
                        "Settings Backup",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(button(
                                "btn-export",
                                t,
                                "Export",
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
                                "Restore",
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, _, cx| {
                                    let src = settings::app_data_dir().join("settings.backup.json");
                                    if src.exists() {
                                        this.settings = settings::load_from(&src);
                                        this.save_settings(cx);
                                    }
                                }),
                            )),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E777}"),
                        "Factory Defaults",
                        button(
                            "btn-reset-all",
                            t,
                            "Reset",
                            ButtonVariant::Destructive,
                            cx.listener(|this, _, _, cx| {
                                this.settings = DockSettings::default();
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
                "Application",
                vec![
                    setting_row(t, Some("\u{E946}"), "Version", badge(t, "v0.1.0", true))
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
                "Diagnostics & Recovery",
                vec![
                    setting_row(
                        t,
                        Some("\u{E8B7}"),
                        "Data Folder",
                        button(
                            "btn-open-data",
                            t,
                            "Open",
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(data_dir.clone()),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E9F5}"),
                        "Runtime Log",
                        button(
                            "btn-open-log",
                            t,
                            "Open",
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(log_file.clone()),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E777}"),
                        "Windows Taskbar",
                        button(
                            "btn-restore-tb",
                            t,
                            "Restore",
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

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme();
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
                    if this.page != p {
                        this.page = p;
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

        let content_blocks = match self.page {
            SettingsPage::Dock => self.render_dock(&t, cx),
            SettingsPage::Widgets => {
                let columns = if content_width < px(WIDGET_COLUMNS_BREAKPOINT) {
                    1
                } else {
                    2
                };
                self.render_widgets(&t, columns, cx)
            }
            SettingsPage::Themes => self.render_themes(&t, cx),
            SettingsPage::Preferences => self.render_preferences(&t, cx),
            SettingsPage::About => self.render_about(&t),
        };

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
                            .text_size(px(22.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.page.title()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgba(t.text_secondary))
                            .child(self.page.description()),
                    ),
            )
            .when(self.save_failed, |panel| panel.child(
                div().px(inset).py(px(8.0)).bg(rgba(t.destructive_soft))
                    .text_color(rgba(t.destructive)).text_size(px(13.0))
                    .child("Could not save settings. Check that the settings folder is writable and try again."),
            ))
            .child(scroll_wrap);

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
                            .text_size(px(12.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.text_primary))
                            .child("OpenTaskbar Settings"),
                    ),
            )
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
                            .on_click(|_, window, _| window.remove_window())
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
                this.close_widget_panel(window);
                cx.notify();
            }))
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
            .font_family("Segoe UI")
            .text_size(px(14.0))
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
    Application::new().run(move |cx: &mut App| {
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
                    title: Some("OpenTaskbar Settings".into()),
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
