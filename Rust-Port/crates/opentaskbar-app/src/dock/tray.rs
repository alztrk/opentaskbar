//! Native Background Apps (System Tray) enumeration and shared `DockFlyout` integration.
//!
//! Compact icon-bearing rows, scrollable overflow, toggle-close on the tray button,
//! and right-click context menu to Open, Close Window, or End Process.

use std::collections::HashMap;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW,
    TerminateProcess,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, MB_ICONWARNING, MB_OK, MessageBoxW, PostMessageW, SW_RESTORE,
    SW_SHOW, SetForegroundWindow, ShowWindow, WM_CLOSE,
};
use windows_sys::core::BOOL;

use crate::platform::{
    icon_cache,
    pins::{self, PinnedApp},
};
use crate::ui::flyout::{self, FlyoutAction, FlyoutBlock, FlyoutIcon, FlyoutSpec};
use crate::ui::menu::{self, MenuEntry, MenuIcon};

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

fn friendly_name(exe_path: &str) -> String {
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
    let hwnd = app.hwnd as HWND;
    let mut brought = false;
    unsafe {
        if app.hwnd != 0 && IsWindow(hwnd) != 0 {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            } else {
                ShowWindow(hwnd, SW_SHOW);
            }
            brought = SetForegroundWindow(hwnd) != 0;
        }
    }
    if !brought {
        pins::launch(app.exe_path.clone());
    }
}

fn close_background_app(owner_hwnd: usize, app: &BackgroundApp) {
    let hwnd = app.hwnd as HWND;
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    let same_process = pid == app.pid
        && process_image_path(pid).is_some_and(|path| path.eq_ignore_ascii_case(&app.exe_path));
    if !same_process || unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } == 0 {
        show_action_error(
            owner_hwnd,
            "Could not close the app window. Reopen Background Apps and try again.",
        );
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

fn terminate_pid(pid: u32) {
    if pid == 0 || pid == std::process::id() {
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
    anchor_center_x_px: i32,
    anchor_bottom_y_px: i32,
    dock_pins: &[PinnedApp],
) -> Option<(String, bool)> {
    let apps = enumerate_background_apps();
    let matching_pins: Vec<_> = apps
        .iter()
        .map(|app| {
            dock_pins.iter().find(|pin| {
                pin.target_exe
                    .as_ref()
                    .is_some_and(|path| path.eq_ignore_ascii_case(&app.exe_path))
                    || pin.launch_path.eq_ignore_ascii_case(&app.exe_path)
            })
        })
        .collect();
    let paths: Vec<String> = apps.iter().map(|a| a.exe_path.clone()).collect();
    let icon_map: HashMap<String, FlyoutIcon> = icon_cache::resolve_batch_raw(&paths)
        .into_iter()
        .filter_map(|(path, raw)| {
            raw.map(|r| {
                (
                    path,
                    FlyoutIcon {
                        width: r.width,
                        height: r.height,
                        bgra: r.pixels,
                    },
                )
            })
        })
        .collect();

    let mut blocks = Vec::new();
    if apps.is_empty() {
        blocks.push(FlyoutBlock::Note("No background apps running.".to_owned()));
    } else {
        for (i, app) in apps.iter().enumerate() {
            let id = 100 + i as u32 * 3;
            blocks.push(FlyoutBlock::AppRow {
                id,
                title: app.name.clone(),
                icon: icon_map.get(&app.exe_path).cloned(),
                pin_id: id + 1,
                pinned: matching_pins[i].is_some(),
                close_id: id + 2,
            });
        }
    }
    blocks.push(FlyoutBlock::Separator);
    blocks.push(FlyoutBlock::ButtonBar(vec![
        (1, "Task Manager".to_owned(), false),
        (2, "Windows Tray".to_owned(), false),
    ]));

    let spec = FlyoutSpec {
        title: "Background Apps".to_owned(),
        subtitle: Some(match apps.len() {
            1 => "1 app running in the background".to_owned(),
            count => format!("{count} apps running in the background"),
        }),
        width_dips: 336.0,
        blocks,
    };

    let mut handle_right_click = |id: u32| -> bool {
        if id < 100 {
            return false;
        }
        let Some(app) = apps.get(((id - 100) / 3) as usize) else {
            return false;
        };
        let entries = vec![
            MenuEntry::item(1, MenuIcon::Play, format!("Open {}", app.name)),
            MenuEntry::item(2, MenuIcon::Close, "Close Window"),
            MenuEntry::Separator,
            MenuEntry::destructive(3, MenuIcon::Power, "End Process"),
        ];
        match menu::show_popup_menu(owner_hwnd, is_dark, entries).unwrap_or(0) {
            1 => {
                activate_or_relaunch(app);
                true
            }
            2 => {
                close_background_app(owner_hwnd, app);
                true
            }
            3 => {
                terminate_pid(app.pid);
                false
            }
            _ => false,
        }
    };

    match flyout::show_flyout_interactive(
        "tray",
        owner_hwnd,
        is_dark,
        anchor_center_x_px,
        anchor_bottom_y_px,
        spec,
        Some(&mut handle_right_click),
    ) {
        Some(FlyoutAction::Primary(1)) => pins::launch("taskmgr.exe".to_owned()),
        Some(FlyoutAction::Primary(2)) => opentaskbar_shell::taskbar::open_tray_overflow(),
        Some(FlyoutAction::Primary(id)) if id >= 100 => {
            let offset = id - 100;
            let index = (offset / 3) as usize;
            if let Some(app) = apps.get(index) {
                match offset % 3 {
                    0 => activate_or_relaunch(app),
                    1 => {
                        return Some(match matching_pins[index] {
                            Some(pin) => (pin.launch_path.clone(), false),
                            None => (app.exe_path.clone(), true),
                        });
                    }
                    _ => close_background_app(owner_hwnd, app),
                }
            }
        }
        _ => {}
    }
    None
}
