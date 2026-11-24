//! Explorer taskbar hiding, restoration, and non-blocking AppBar setup.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{
    ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow,
};

use crate::{journal, watchdog};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn find_primary_tray() -> HWND {
    let class = wide("Shell_TrayWnd");
    // SAFETY: null-terminated class name.
    unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) }
}

fn set_secondary_trays_visible(visible: bool) {
    let class = wide("Shell_SecondaryTrayWnd");
    let cmd = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
    let mut current: HWND = std::ptr::null_mut();
    // SAFETY: iterating top-level secondary taskbar windows owned by Explorer.
    unsafe {
        loop {
            current = FindWindowExW(
                std::ptr::null_mut(),
                current,
                class.as_ptr(),
                std::ptr::null(),
            );
            if current.is_null() {
                break;
            }
            ShowWindow(current, cmd);
        }
    }
}

pub fn get_taskbar_state() -> u32 {
    let mut data: APPBARDATA = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<APPBARDATA>() as u32;
    data.hWnd = find_primary_tray();
    // SAFETY: properly sized APPBARDATA.
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut data) as u32 }
}

pub fn set_taskbar_state(state: u32) {
    let tray = find_primary_tray();
    if tray.is_null() {
        return;
    }
    let mut data: APPBARDATA = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<APPBARDATA>() as u32;
    data.hWnd = tray;
    data.lParam = state as isize;
    // SAFETY: properly sized APPBARDATA.
    unsafe {
        SHAppBarMessage(ABM_SETSTATE, &mut data);
    }
}

/// Immediately hides Explorer's taskbar windows and runs `ABM_SETSTATE` on a
/// background thread so GPUI paints its first frame without stalling.
pub fn begin_replacement(auto_hide: bool) {
    let prev = journal::read_state().unwrap_or_else(get_taskbar_state);
    if !journal::has_pending() {
        let _ = journal::write_state(prev);
    }
    watchdog::arm();

    let tray = find_primary_tray();
    if !tray.is_null() {
        // SAFETY: hiding Explorer's primary taskbar window.
        unsafe {
            ShowWindow(tray, SW_HIDE);
        }
    }
    set_secondary_trays_visible(false);

    let target_state = if auto_hide { prev | ABS_AUTOHIDE } else { prev };
    if target_state != prev {
        std::thread::Builder::new()
            .name("opentaskbar-abm".into())
            .spawn(move || set_taskbar_state(target_state))
            .ok();
    }
}

pub fn restore_from_journal() {
    let prev = journal::read_state().unwrap_or(0);
    set_taskbar_state(prev);
    let tray = find_primary_tray();
    if !tray.is_null() {
        // SAFETY: restoring Explorer's primary taskbar window.
        unsafe {
            ShowWindow(tray, SW_SHOWNOACTIVATE);
        }
    }
    set_secondary_trays_visible(true);
    journal::clear();
}

pub fn end_replacement() {
    restore_from_journal();
    watchdog::notify_clean_exit();
}
