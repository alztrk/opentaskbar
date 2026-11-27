//! Extra Win32 helpers used by auto-hide, target display selection, virtual
//! desktops and audio device selection.
//!
//! C4 API: `is_shell_overlay(hwnd)`, `is_shell_overlay_for(hwnd, exe_path)`,
//! `display_for(name: &str)`, `set_default_audio_device(id: &str)`.
//! This module is the single source of truth for shell overlay classes; the
//! dock/layout list is merged here and CoreWindow is validated by host exe/PID.

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowThreadProcessId};

/// Transient shell surfaces that can take the foreground: Alt+Tab / Task View,
/// the desktop shell plus the Win10/Win11 Start flyout and its light-dismiss
/// helpers. Union of the former `win32_ex` and `dock::layout` lists so callers
/// share one source. `Windows.UI.Core.CoreWindow` is gated by host exe/PID:
/// arbitrary UWP applications use the same class and are *not* overlays.
const SHELL_OVERLAY_CLASSES: [&str; 11] = [
    "XamlExplorerHostIslandWindow",
    "MultitaskingViewFrame",
    "TaskSwitcherWnd",
    "Windows.UI.Core.CoreWindow",
    "StartMenuSizingFrame",
    "Shell_LightDismissOverlay",
    "StartMenuExperienceHost",
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];
const CORE_WINDOW_CLASS: &str = "windows.ui.core.corewindow";
const MONITORINFOF_PRIMARY: u32 = 1;
const VIRTUAL_DESKTOPS_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops";
const SESSION_VIRTUAL_DESKTOPS_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\{}\VirtualDesktops";

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ProcessIdToSessionId(process_id: u32, session_id: *mut u32) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Pure class-list membership. Callers that may see a `CoreWindow` must use
/// `is_shell_overlay_for_class` (or `is_shell_overlay*`) with host validation;
/// this function deliberately does not claim every CoreWindow is an overlay.
pub fn is_shell_overlay_class(class: &str) -> bool {
    SHELL_OVERLAY_CLASSES
        .iter()
        .any(|known| class.trim().eq_ignore_ascii_case(known))
}

fn class_is_core_window(class: &str) -> bool {
    class.trim().eq_ignore_ascii_case(CORE_WINDOW_CLASS)
}

/// Class-list check with the CoreWindow exception. `start_host` must be true
/// only for StartMenuExperienceHost.exe / ShellExperienceHost.exe; otherwise a
/// regular UWP CoreWindow would make auto-hide look past a real app window.
pub fn is_shell_overlay_for_class(class: &str, start_host: bool) -> bool {
    is_shell_overlay_class(class) && (!class_is_core_window(class) || start_host)
}

fn window_class(hwnd: HWND) -> Option<String> {
    let mut buffer = [0u16; 64];
    // SAFETY: bounded UTF-16 buffer on a live window handle.
    let length = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    (length > 0).then(|| String::from_utf16_lossy(&buffer[..(length as usize).min(buffer.len())]))
}

/// Recognises transient shell overlays (Alt+Tab / Task View / Start) that take
/// the foreground without being applications. Auto-hide must look past them to
/// the application beneath. Resolves the executable from the window's PID.
pub fn is_shell_overlay(hwnd: usize) -> bool {
    if hwnd == 0 {
        return false;
    }
    let handle = hwnd as HWND;
    let Some(class) = window_class(handle) else {
        return false;
    };
    if !is_shell_overlay_class(&class) {
        return false;
    }
    if !class_is_core_window(&class) {
        return true;
    }
    let mut pid = 0u32;
    // SAFETY: querying the PID of a window supplied by the caller/OS.
    unsafe { GetWindowThreadProcessId(handle, &mut pid) };
    crate::platform::start_menu_state::is_start_host_pid(pid)
}

/// Overlay check for callers that already hold the window's executable path
/// (e.g. the dock's running-window snapshot). Avoids a second process query.
pub fn is_shell_overlay_for(hwnd: usize, exe_path: &str) -> bool {
    if hwnd == 0 {
        return false;
    }
    let Some(class) = window_class(hwnd as HWND) else {
        return false;
    };
    is_shell_overlay_for_class(
        &class,
        crate::platform::start_menu_state::is_start_host_exe(exe_path),
    )
}

/// A display in device-independent pixels with stable 1-based index.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInfo {
    pub handle: usize,
    pub device_name: String,
    pub index: u32,
    pub left_dip: f64,
    pub top_dip: f64,
    pub width_dip: f64,
    pub height_dip: f64,
    pub work_left_dip: f64,
    pub work_top_dip: f64,
    pub work_width_dip: f64,
    pub work_height_dip: f64,
    pub is_primary: bool,
    pub dpi: u32,
}

/// Enumerates displays, converting physical pixels to DIPs with `96 / dpi`
/// (`DisplayService` parity). The primary monitor is always first.
pub fn displays() -> Vec<DisplayInfo> {
    unsafe extern "system" fn visit(
        monitor: HMONITOR,
        _hdc: HDC,
        _clip: *mut RECT,
        lparam: LPARAM,
    ) -> i32 {
        // SAFETY: `lparam` points at a Vec alive during the synchronous enumeration.
        unsafe {
            let output = &mut *(lparam as *mut Vec<(HMONITOR, MONITORINFOEXW, u32)>);
            let mut info: MONITORINFOEXW = std::mem::zeroed();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()) != 0 {
                let mut dpi_x = 96u32;
                let mut dpi_y = 96u32;
                let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
                output.push((monitor, info, dpi_x.max(96)));
            }
            1
        }
    }

    let mut monitors: Vec<(HMONITOR, MONITORINFOEXW, u32)> = Vec::new();
    // SAFETY: synchronous Win32 enumeration; the Vec outlives the callback.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(visit),
            (&mut monitors as *mut Vec<_>) as LPARAM,
        );
    }
    monitors.sort_by_key(|(_, info, _)| {
        // Primary first, then enumeration order (stable sort keeps index order).
        info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY == 0
    });

    monitors
        .into_iter()
        .enumerate()
        .map(|(index, (handle, info, dpi))| {
            let scale = 96.0 / dpi as f64;
            let device_name = String::from_utf16_lossy(
                &info.szDevice[..info.szDevice.iter().position(|&c| c == 0).unwrap_or(32)],
            );
            DisplayInfo {
                handle: handle as usize,
                device_name,
                index: index as u32 + 1,
                left_dip: info.monitorInfo.rcMonitor.left as f64 * scale,
                top_dip: info.monitorInfo.rcMonitor.top as f64 * scale,
                width_dip: (info.monitorInfo.rcMonitor.right - info.monitorInfo.rcMonitor.left)
                    as f64
                    * scale,
                height_dip: (info.monitorInfo.rcMonitor.bottom - info.monitorInfo.rcMonitor.top)
                    as f64
                    * scale,
                work_left_dip: info.monitorInfo.rcWork.left as f64 * scale,
                work_top_dip: info.monitorInfo.rcWork.top as f64 * scale,
                work_width_dip: (info.monitorInfo.rcWork.right - info.monitorInfo.rcWork.left)
                    as f64
                    * scale,
                work_height_dip: (info.monitorInfo.rcWork.bottom - info.monitorInfo.rcWork.top)
                    as f64
                    * scale,
                is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                dpi,
            }
        })
        .collect()
}

/// Resolves a target display by device name, 1-based index string, or primary
/// fallback (`DisplayService.GetTargetDisplay`).
pub fn display_for(name: &str) -> Option<DisplayInfo> {
    let all = displays();
    if let Some(match_) = all.iter().find(|display| {
        display.device_name.eq_ignore_ascii_case(name) || display.index.to_string() == name
    }) {
        return Some(match_.clone());
    }
    all.iter()
        .find(|display| display.is_primary)
        .or_else(|| all.first())
        .cloned()
}

pub fn display_for_index(index: u32) -> Option<DisplayInfo> {
    displays()
        .into_iter()
        .find(|display| display.index == index)
}

pub fn secondary_monitors(name: &str) -> Vec<usize> {
    let target = display_for(name).map(|display| display.handle).unwrap_or(0);
    displays()
        .into_iter()
        .filter(|display| display.handle != target)
        .map(|display| display.handle)
        .collect()
}

fn read_binary(subkey: &str, value: &str) -> Option<Vec<u8>> {
    let subkey = wide(subkey);
    let value = wide(value);
    // SAFETY: read-only registry query with owned UTF-16 strings and a handle
    // opened on this call.
    unsafe {
        let mut key = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return None;
        }
        let mut size = 0u32;
        if RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) != 0
            || size < 16
        {
            RegCloseKey(key);
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        let status = RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            &mut size,
        );
        RegCloseKey(key);
        (status == 0).then_some(buffer)
    }
}

/// 1-based current virtual desktop index and total count from Explorer's
/// registry state; falls back to the per-session key on Windows 10.
pub fn virtual_desktops() -> (u32, u32) {
    let ids = match read_binary(VIRTUAL_DESKTOPS_KEY, "VirtualDesktopIDs") {
        Some(ids) => ids,
        None => return (1, 1),
    };
    let count = (ids.len() / 16) as u32;
    let current = read_binary(VIRTUAL_DESKTOPS_KEY, "CurrentVirtualDesktop").or_else(|| {
        let mut session = 0u32;
        // SAFETY: output parameter is a local u32.
        if unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } == 0 {
            return None;
        }
        read_binary(
            &SESSION_VIRTUAL_DESKTOPS_KEY.replace("{}", &session.to_string()),
            "CurrentVirtualDesktop",
        )
    });
    let Some(current) = current else {
        return (1, count);
    };
    if current.len() != 16 {
        return (1, count);
    }
    for index in 0..count as usize {
        if ids[index * 16..index * 16 + 16] == current[..16] {
            return (index as u32 + 1, count);
        }
    }
    (1, count)
}

/// Sets the default audio endpoint. The Win32-facing alias delegates to the
/// C4 `platform::core_audio::set_default_device(id)` bridge (WS-G ownership),
/// which applies the endpoint to all three roles via `IPolicyConfig`.
pub fn set_default_audio_device(id: &str) -> bool {
    crate::platform::core_audio::set_default_device(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_overlay_classes_merge_layout_and_start_lists() {
        for class in [
            "XamlExplorerHostIslandWindow",
            "MultitaskingViewFrame",
            "TaskSwitcherWnd",
            "windows.ui.core.corewindow",
            "Windows.UI.Core.CoreWindow",
            "StartMenuSizingFrame",
            "shell_lightdismissoverlay",
            "StartMenuExperienceHost",
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
        ] {
            assert!(is_shell_overlay_class(class), "expected overlay: {class}");
        }
        assert!(!is_shell_overlay_class("Notepad"));
        assert!(!is_shell_overlay_class(""));
        assert!(!is_shell_overlay(0));
        assert!(!is_shell_overlay_for(0, "notepad.exe"));
    }

    #[test]
    fn core_window_is_overlay_only_for_start_shell_hosts() {
        // A random UWP app shares the CoreWindow class but is a real app window.
        assert!(is_shell_overlay_for_class(
            "Windows.UI.Core.CoreWindow",
            true
        ));
        assert!(!is_shell_overlay_for_class(
            "Windows.UI.Core.CoreWindow",
            false
        ));
        assert!(!is_shell_overlay_for_class(
            "windows.ui.core.corewindow",
            false
        ));
        assert!(is_shell_overlay_for_class("Shell_TrayWnd", false));
        assert!(is_shell_overlay_for_class(
            "XamlExplorerHostIslandWindow",
            false
        ));
        assert!(!is_shell_overlay_for_class("Notepad", true));
    }

    #[test]
    fn display_for_falls_back_to_primary_when_unknown() {
        let all = displays();
        if let Some(primary) = all.iter().find(|display| display.is_primary) {
            let resolved = display_for("__unknown_display__").expect("primary display");
            assert_eq!(resolved.handle, primary.handle);
        }
    }
}
