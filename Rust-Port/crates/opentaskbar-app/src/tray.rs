//! Native Background Apps (System Tray) enumeration and shared `DockFlyout` integration.
//!
//! Replaces Windows' legacy `Win+B` overflow popup with OpenTaskBar's own shared `DockFlyout`
//! component, allowing users to inspect, focus/open, or inspect background processes cleanly.

use std::collections::HashMap;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
};
use windows_sys::core::BOOL;

use crate::flyout::{self, FlyoutBlock, FlyoutSpec};
use crate::pins;

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
    if lower.starts_with(r"c:\windows\system32\")
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
    {
        return true;
    }
    false
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

/// Enumerates user-space GUI apps currently running in the background (hidden/tray state).
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
    apps.truncate(10);
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

/// Opens our shared `DockFlyout` for Background Apps (System Tray).
pub fn open_background_apps_flyout(
    owner_hwnd: usize,
    is_dark: bool,
    anchor_center_x_px: i32,
    anchor_bottom_y_px: i32,
) {
    let apps = enumerate_background_apps();
    let mut blocks = Vec::new();
    if apps.is_empty() {
        blocks.push(FlyoutBlock::Note(
            "No background tray apps detected.".to_owned(),
        ));
    } else {
        for (i, app) in apps.iter().enumerate() {
            blocks.push(FlyoutBlock::ActionRow {
                id: 100 + i as u32,
                title: app.name.clone(),
                subtitle: Some(format!("PID {}", app.pid)),
                badge: Some("Open".to_owned()),
                destructive: false,
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
        subtitle: Some(format!("{} running in background", apps.len())),
        width_dips: 300.0,
        blocks,
    };

    if let Some(action) = flyout::show_flyout(
        owner_hwnd,
        is_dark,
        anchor_center_x_px,
        anchor_bottom_y_px,
        spec,
    ) {
        match action {
            1 => pins::launch("taskmgr.exe".to_owned()),
            2 => opentaskbar_shell::taskbar::open_tray_overflow(),
            id if id >= 100 => {
                if let Some(app) = apps.get((id - 100) as usize) {
                    activate_or_relaunch(app);
                }
            }
            _ => {}
        }
    }
}
