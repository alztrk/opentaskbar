//! Native Background Apps (System Tray) enumeration and shared `DockFlyout` integration.
//!
//! Compact icon-bearing rows, scrollable overflow, toggle-close on the tray button,
//! and right-click context menu to Open, Close Window, or End Process.

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::DockSettings;

use std::collections::HashMap;
use std::path::Path;

use opentaskbar_shell::tracker;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW,
    TerminateProcess,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, MB_ICONWARNING, MB_OK, MessageBoxW, PostMessageW, SW_RESTORE, SW_SHOW,
    SetForegroundWindow, ShowWindow, WM_CLOSE,
};
use windows_sys::core::BOOL;

use crate::platform::{icon_cache, pins};
use crate::ui::flyout::{self, FlyoutAction, FlyoutBlock, FlyoutIcon, FlyoutOptions, FlyoutSpec};
use crate::ui::menu::{self, BorderStyle, MenuEntry, MenuIcon};

/// Remapped ids for the target app's own menu so its command ids (often `1`,
/// `2`, ...) cannot collide with tray row ids or the footer ButtonBar ids.
const NATIVE_MENU_COMMAND_BASE: u32 = 5000;

/// Pure id remapping for the target's native menu. Kept separate from Win32 so
/// the no-collision contract can be unit tested.
fn remap_native_menu_entries(entries: Vec<MenuEntry>) -> (Vec<(u32, u32)>, Vec<MenuEntry>) {
    let mut commands = Vec::new();
    let mut items = Vec::new();
    for (offset, entry) in entries.into_iter().enumerate() {
        let remapped = NATIVE_MENU_COMMAND_BASE + offset as u32;
        match entry {
            MenuEntry::Item {
                id,
                icon,
                label,
                shortcut,
                destructive,
            } => {
                commands.push((remapped, id));
                items.push(MenuEntry::Item {
                    id: remapped,
                    icon,
                    label,
                    shortcut,
                    destructive,
                });
            }
            other => items.push(other),
        }
    }
    (commands, items)
}

#[derive(Clone, Debug)]
pub struct BackgroundApp {
    pub name: String,
    pub exe_path: String,
    pub hwnd: usize,
    pub pid: u32,
}

fn process_image_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        (ok != 0 && len > 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if copied <= 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buf[..copied as usize])
        }
    }
}

fn is_system_noise(exe_path: &str) -> bool {
    let lower = exe_path.to_lowercase();
    lower.starts_with(r"c:\windows\system32\")
        || lower.starts_with(r"c:\windows\syswow64\")
        || lower.starts_with(r"c:\windows\systemapps\")
        || lower.starts_with(r"c:\windows\immersivecontrolpanel\")
        || lower.ends_with(r"\explorer.exe")
        || lower.ends_with(r"\opentaskbar.exe")
        || lower.ends_with(r"\applicationframehost.exe")
        || lower.ends_with(r"\runtimebroker.exe")
        || lower.ends_with(r"\dllhost.exe")
        || lower.ends_with(r"\conhost.exe")
        || lower.ends_with(r"\sihost.exe")
        || lower.ends_with(r"\ctfmon.exe")
        || lower.ends_with(r"\taskhostw.exe")
        || lower.ends_with(r"\widgets.exe")
        || lower.ends_with(r"\widgetservice.exe")
        || lower.ends_with(r"\msedgewebview2.exe")
        || lower.ends_with(r"\crashpad_handler.exe")
}

#[link(name = "version")]
unsafe extern "system" {
    fn GetFileVersionInfoSizeW(path: *const u16, handle: *mut u32) -> u32;
    fn GetFileVersionInfoW(
        path: *const u16,
        handle: u32,
        size: u32,
        data: *mut std::ffi::c_void,
    ) -> i32;
    fn VerQueryValueW(
        data: *const std::ffi::c_void,
        key: *const u16,
        value: *mut *mut std::ffi::c_void,
        len: *mut u32,
    ) -> i32;
}

/// Reads the PE `VERSIONINFO` `FileDescription` (then `ProductName`) for an
/// executable, trying its own translation table before the en-US fallbacks.
fn version_name(path: &str) -> Option<String> {
    let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `path` is a NUL-terminated UTF-16 buffer that outlives the call.
    let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), std::ptr::null_mut()) };
    if size == 0 {
        return None;
    }
    // `u32` storage keeps the version block 4-byte aligned for `VerQueryValueW`.
    let mut data = vec![0u32; (size as usize).div_ceil(4)];
    // SAFETY: `data` holds at least `size` bytes.
    if unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let query = |key: &str| {
        let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
        let mut value = std::ptr::null_mut();
        let mut len = 0;
        // SAFETY: `data` is a valid version block; the returned pointer points into it.
        let ok =
            unsafe { VerQueryValueW(data.as_ptr().cast(), key.as_ptr(), &mut value, &mut len) };
        (ok != 0 && !value.is_null()).then_some((value.cast::<u16>(), len as usize))
    };
    let mut translations = Vec::new();
    if let Some((ptr, bytes)) = query(r"\VarFileInfo\Translation") {
        // SAFETY: the translation value is `bytes` long and lives inside `data`.
        for pair in unsafe { std::slice::from_raw_parts(ptr, bytes / 2) }.chunks_exact(2) {
            translations.push((pair[0], pair[1]));
        }
    }
    translations.extend([(0x0409, 0x04b0), (0x0409, 0x04e4)]);
    select_version_name(|field| {
        translations.iter().find_map(|(lang, page)| {
            let key = format!(r"\StringFileInfo\{lang:04X}{page:04X}\{field}");
            let (ptr, len) = query(&key)?;
            // SAFETY: string values are `len` UTF-16 units inside `data`.
            let units = unsafe { std::slice::from_raw_parts(ptr, len) };
            let name = String::from_utf16_lossy(units)
                .trim_end_matches('\0')
                .trim()
                .to_owned();
            (!name.is_empty()).then_some(name)
        })
    })
}

/// `FileDescription` wins over `ProductName`; blank values are skipped.
fn select_version_name(mut lookup: impl FnMut(&str) -> Option<String>) -> Option<String> {
    ["FileDescription", "ProductName"]
        .into_iter()
        .find_map(|key| lookup(key).filter(|s| !s.trim().is_empty()))
}

/// Friendly app name for tray rows and pinned tray icons: the executable's PE
/// `FileDescription` / `ProductName`, cached per path, falling back to the
/// capitalized file stem.
pub(super) fn friendly_name(exe_path: &str) -> String {
    use std::sync::{Mutex, OnceLock, PoisonError};
    static NAMES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let names = NAMES.get_or_init(Default::default);
    let key = exe_path.to_lowercase();
    if let Some(name) = names
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return name.clone();
    }
    // Resolve outside the lock: version-info reads touch the disk.
    let name = version_name(exe_path).unwrap_or_else(|| stem_name(exe_path));
    names
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, name.clone());
    name
}

fn stem_name(exe_path: &str) -> String {
    let stem = Path::new(exe_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("App");
    let mut chars = stem.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "App".to_owned(),
    }
}

struct EnumTrayCtx {
    self_pid: u32,
    visible_exes: HashMap<String, bool>,
    candidates: HashMap<String, BackgroundApp>,
}

unsafe extern "system" fn enum_tray_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let ctx = &mut *(lparam as *mut EnumTrayCtx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 || pid == ctx.self_pid {
            return 1;
        }
        let Some(exe_path) = process_image_path(pid) else {
            return 1;
        };
        if is_system_noise(&exe_path) {
            return 1;
        }
        let key = exe_path.to_lowercase();
        let visible = IsWindowVisible(hwnd) != 0 && !window_title(hwnd).is_empty();
        if visible {
            ctx.visible_exes.insert(key, true);
            return 1;
        }
        ctx.candidates.entry(key).or_insert_with(|| BackgroundApp {
            name: friendly_name(&exe_path),
            exe_path,
            hwnd: hwnd as usize,
            pid,
        });
        1
    }
}

/// Enumerates all user-space GUI apps currently running in the background.
pub fn enumerate_background_apps() -> Vec<BackgroundApp> {
    let mut ctx = EnumTrayCtx {
        self_pid: std::process::id(),
        visible_exes: HashMap::new(),
        candidates: HashMap::new(),
    };
    unsafe {
        EnumWindows(
            Some(enum_tray_proc),
            (&mut ctx as *mut EnumTrayCtx) as LPARAM,
        );
    }
    let mut apps: Vec<BackgroundApp> = ctx
        .candidates
        .into_iter()
        .filter(|(k, _)| !ctx.visible_exes.contains_key(k))
        .map(|(_, app)| app)
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

fn activate_or_relaunch(app: &BackgroundApp) {
    let same_process =
        process_image_path(app.pid).is_some_and(|path| path.eq_ignore_ascii_case(&app.exe_path));
    let mut identity_window = None;
    if same_process {
        if let Some(window) = tracker::find_app_window(app.pid) {
            let hwnd = window as HWND;
            identity_window = Some(window);
            // SAFETY: The window was selected from the application's current top-level windows.
            let brought = unsafe {
                let show = if IsIconic(hwnd) != 0 {
                    SW_RESTORE
                } else {
                    SW_SHOW
                };
                ShowWindow(hwnd, show);
                SetForegroundWindow(hwnd) != 0 && IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0
            };
            if brought {
                return;
            }
        } else {
            let mut owner_pid = 0;
            // SAFETY: Windows validates the stored handle before returning its owning process.
            unsafe { GetWindowThreadProcessId(app.hwnd as HWND, &mut owner_pid) };
            if owner_pid == app.pid {
                identity_window = Some(app.hwnd);
            }
        }
    }
    pins::launch_app(app.exe_path.clone(), identity_window, false);
}

pub(super) fn open_pinned_app(owner_hwnd: usize, path: &str) {
    if let Some(window) = tracker::enumerate_windows()
        .into_iter()
        .find(|window| window.exe_path.eq_ignore_ascii_case(path))
    {
        activate_or_relaunch(&BackgroundApp {
            name: window.title,
            exe_path: window.exe_path,
            hwnd: window.hwnd,
            pid: window.pid,
        });
    } else if let Some(app) = enumerate_background_apps()
        .into_iter()
        .find(|app| app.exe_path.eq_ignore_ascii_case(path))
    {
        activate_or_relaunch(&app);
    } else if Path::new(path).is_file() {
        pins::launch_app(path.to_owned(), None, false);
    } else {
        show_action_error(owner_hwnd, &loc::t("ErrorOpenBackgroundApp"));
    }
}

/// Middle-click on a pinned tray icon closes that app (`PinnedTrayStrip` orientation/middle click).
pub(super) fn close_pinned_app(owner_hwnd: usize, path: &str) {
    if let Some(app) = enumerate_background_apps()
        .into_iter()
        .find(|app| app.exe_path.eq_ignore_ascii_case(path))
    {
        close_background_app(owner_hwnd, &app);
    } else if let Some(window) = tracker::enumerate_windows()
        .into_iter()
        .find(|window| window.exe_path.eq_ignore_ascii_case(path))
    {
        tracker::close_window(window.hwnd);
    }
}

pub(super) fn show_pinned_app_menu(
    owner_hwnd: usize,
    is_dark: bool,
    settings: &DockSettings,
    path: &str,
    anchor: Option<menu::MenuAnchor>,
) -> bool {
    let entries = vec![
        MenuEntry::item(
            1,
            MenuIcon::Play,
            format!("{} · {}", loc::t("Open"), friendly_name(path)),
        ),
        MenuEntry::item(2, MenuIcon::Pin, loc::t("UnpinTrayIcon")),
    ];
    // The pinned icon lives on the dock pill: anchor the menu to the visible
    // pill so it opens beside the dock instead of at the raw cursor position.
    match menu::show_popup_menu_with_anchor(
        owner_hwnd,
        is_dark,
        &entries,
        BorderStyle::from_menu_settings(is_dark, settings),
        anchor,
    ) {
        Some(1) => open_pinned_app(owner_hwnd, path),
        Some(2) => return true,
        _ => {}
    }
    false
}

fn close_background_app(owner_hwnd: usize, app: &BackgroundApp) {
    let hwnd = app.hwnd as HWND;
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    let same_process = pid == app.pid
        && process_image_path(pid).is_some_and(|path| path.eq_ignore_ascii_case(&app.exe_path));
    if !same_process || unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } == 0 {
        show_action_error(owner_hwnd, &loc::t("ErrorAppGracefulClose"));
    }
}

pub(super) fn show_action_error(owner_hwnd: usize, message: &str) {
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "OpenTaskbar".encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            owner_hwnd as HWND,
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONWARNING,
        )
    };
}

fn terminate_pid(pid: u32, expected_exe: &str) {
    if pid == 0
        || pid == std::process::id()
        || process_image_path(pid).is_none_or(|path| !path.eq_ignore_ascii_case(expected_exe))
    {
        return;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            let _ = TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

/// Opens our shared `DockFlyout` for Background Apps with icons, scroll overflow, and right-click menu.
pub(super) fn open_background_apps_flyout(
    owner_hwnd: usize,
    is_dark: bool,
    settings: &DockSettings,
    anchor_center_x_px: i32,
    anchor_bottom_y_px: i32,
    edge: flyout::FlyoutEdge,
    pinned_paths: &[String],
) -> Option<(String, bool)> {
    let apps = enumerate_background_apps();
    let mut rows: Vec<_> = apps
        .iter()
        .map(|app| (app.exe_path.as_str(), Some(app)))
        .collect();
    for path in pinned_paths {
        if !rows
            .iter()
            .any(|(entry, _)| entry.eq_ignore_ascii_case(path))
        {
            rows.push((path, None));
        }
    }
    let is_pinned = |path: &str| {
        pinned_paths
            .iter()
            .any(|pin| pin.eq_ignore_ascii_case(path))
    };
    let paths: Vec<String> = rows.iter().map(|(path, _)| (*path).to_owned()).collect();
    let icon_map: HashMap<String, FlyoutIcon> = icon_cache::resolve_batch_raw(&paths)
        .into_iter()
        .filter_map(|(path, raw)| {
            raw.map(|r| {
                (
                    path,
                    FlyoutIcon {
                        width: r.width,
                        height: r.height,
                        bgra: r.pixels.to_vec(),
                    },
                )
            })
        })
        .collect();

    let mut blocks = Vec::new();
    if rows.is_empty() {
        blocks.push(FlyoutBlock::Note(loc::t("TrayEmpty")));
    } else {
        for (i, (path, app)) in rows.iter().enumerate() {
            let id = 100 + i as u32 * 3;
            blocks.push(FlyoutBlock::AppRow {
                id,
                title: app.map_or_else(|| friendly_name(path), |app| app.name.clone()),
                icon: icon_map.get(*path).cloned(),
                pin_id: id + 1,
                pinned: is_pinned(path),
                close_id: app.map(|_| id + 2),
            });
        }
    }
    // WS-C's `WindowsTray` is the canonical key; older catalogs still ship the
    // `TraySettings` translation, so keep it as a fallback until WS-F lands the
    // new key in all 14 locales (same pattern as `page_count_text`).
    let tray_settings_label = {
        let label = loc::t("WindowsTray");
        if label == "WindowsTray" {
            loc::t("TraySettings")
        } else {
            label
        }
    };
    blocks.push(FlyoutBlock::Separator);
    blocks.push(FlyoutBlock::ButtonBar(vec![
        (1, loc::t("TaskManager"), false),
        (2, tray_settings_label, false),
    ]));

    let apps_word = loc::t("LauncherApps").to_lowercase();
    let subtitle = if apps.len() == 1 {
        let label = loc::t("TrayAppsRunningOne");
        if label == "TrayAppsRunningOne" {
            format!("1 {apps_word}")
        } else {
            label
        }
    } else {
        const KEY: &str = "TrayAppsRunningMany";
        let template = loc::t(KEY);
        if template == KEY {
            format!("{} {apps_word}", apps.len())
        } else {
            loc::t_fmt(KEY, &[&apps.len().to_string()])
        }
    };
    let spec = FlyoutSpec {
        title: loc::t("TrayApps"),
        subtitle: Some(subtitle),
        width_dips: 336.0,
        blocks,
    };

    let mut open_from_menu = None;
    let mut handle_right_click = |id: u32| -> bool {
        if id < 100 {
            return false;
        }
        let Some((_, Some(app))) = rows.get(((id - 100) / 3) as usize) else {
            return false;
        };
        let mut entries = vec![
            MenuEntry::item(
                1,
                MenuIcon::Play,
                format!("{} · {}", loc::t("Open"), app.name),
            ),
            MenuEntry::item(2, MenuIcon::Close, loc::t("CloseWindow")),
            MenuEntry::Separator,
            MenuEntry::destructive(3, MenuIcon::Power, loc::t("Terminate")),
        ];
        // Expose the target's own `GetMenu` / `GetSystemMenu` commands (reference
        // `ApplicationWindowMenu`) as a submenu of this row's context menu.
        let (native_commands, native_items) =
            remap_native_menu_entries(menu::native_app_menu_entries(app.hwnd));
        if !native_items.is_empty() {
            entries.push(MenuEntry::Separator);
            entries.push(MenuEntry::submenu(
                MenuIcon::Terminal,
                "System menu",
                native_items,
            ));
        }
        // A row inside the flyout is not on the dock pill: `None` keeps this
        // menu cursor-anchored next to the row that was right-clicked.
        match menu::show_popup_menu_with_anchor(
            owner_hwnd,
            is_dark,
            &entries,
            BorderStyle::from_menu_settings(is_dark, settings),
            None,
        )
        .unwrap_or(0)
        {
            1 => {
                open_from_menu = Some(app);
                true
            }
            2 => {
                close_background_app(owner_hwnd, app);
                true
            }
            3 => {
                terminate_pid(app.pid, &app.exe_path);
                false
            }
            id => {
                // Native commands must be sent to the *target* window, not to
                // our popup, and never keep a stale menu id after the click.
                if let Some((_, command)) = native_commands.iter().find(|(item, _)| *item == id) {
                    menu::invoke_native_app_command(app.hwnd, *command);
                }
                false
            }
        }
    };

    // `FlyoutOptions::tray()` arms the #32768 retention hook so the target
    // application's native context menu can stay open over this flyout.
    let action = flyout::show_flyout_interactive_with_options(
        "tray",
        owner_hwnd,
        is_dark,
        anchor_center_x_px,
        anchor_bottom_y_px,
        spec,
        Some(&mut handle_right_click),
        BorderStyle::from_menu_settings(is_dark, settings),
        FlyoutOptions::tray().with_edge(edge),
    );
    if let Some(app) = open_from_menu {
        activate_or_relaunch(app);
        return None;
    }
    match action {
        Some(FlyoutAction::Primary(1)) => pins::launch("taskmgr.exe".to_owned()),
        Some(FlyoutAction::Primary(2)) => opentaskbar_shell::taskbar::open_tray_overflow(),
        Some(FlyoutAction::Primary(id)) if id >= 100 => {
            let offset = id - 100;
            let index = (offset / 3) as usize;
            if let Some(&(path, app)) = rows.get(index) {
                match offset % 3 {
                    0 => {
                        if let Some(app) = app {
                            activate_or_relaunch(app);
                        } else {
                            open_pinned_app(owner_hwnd, path);
                        }
                    }
                    1 => return Some((path.to_owned(), !is_pinned(path))),
                    _ => {
                        if let Some(app) = app {
                            close_background_app(owner_hwnd, app);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_flyout_options_arm_external_native_menu_retention() {
        use crate::ui::flyout::FlyoutOptions;
        let default = FlyoutOptions::default();
        let tray = FlyoutOptions::tray();
        assert!(!default.retain_for_external_context_menu);
        assert!(tray.retain_for_external_context_menu);
        assert!(!tray.stays_open);
    }

    #[test]
    fn version_name_prefers_description_then_product() {
        assert_eq!(
            select_version_name(|k| Some(k.to_owned())).as_deref(),
            Some("FileDescription")
        );
        assert_eq!(
            select_version_name(|k| Some(
                if k == "FileDescription" {
                    " "
                } else {
                    "VMware Tools"
                }
                .to_owned()
            ))
            .as_deref(),
            Some("VMware Tools")
        );
    }

    #[test]
    fn missing_version_name_uses_capitalized_stem() {
        assert_eq!(select_version_name(|_| None), None);
        assert_eq!(friendly_name(r"Z:\missing\vmtoolsd.exe"), "Vmtoolsd");
        // Cached per path (case-insensitively) without changing the answer.
        assert_eq!(friendly_name(r"Z:\MISSING\VMTOOLSD.EXE"), "Vmtoolsd");
        assert_eq!(stem_name(""), "App");
    }

    #[test]
    fn native_menu_commands_are_remapped_without_colliding_with_rows() {
        let entries = vec![
            MenuEntry::item(1, MenuIcon::Terminal, "Restore".to_owned()),
            MenuEntry::Separator,
            MenuEntry::item(0xF020, MenuIcon::Close, "Minimize".to_owned()),
        ];
        let (commands, items) = remap_native_menu_entries(entries);
        assert_eq!(
            commands,
            vec![
                (NATIVE_MENU_COMMAND_BASE, 1),
                (NATIVE_MENU_COMMAND_BASE + 2, 0xF020),
            ]
        );
        assert!(matches!(items[1], MenuEntry::Separator));
        let MenuEntry::Item { id, .. } = items[0] else {
            panic!("first native entry must stay selectable");
        };
        assert_eq!(id, NATIVE_MENU_COMMAND_BASE);
        assert!(id >= 5000);
        // Native command ids must never collide with a widget row id (100..).
        assert!(
            items
                .iter()
                .all(|entry| !matches!(entry, MenuEntry::Item { id: 1..=99, .. }))
        );
    }
}
