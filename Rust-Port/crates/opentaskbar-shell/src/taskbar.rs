//! Explorer taskbar hiding, restoration, and AppBar work-area reservation.

use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::Shell::{
    ABE_BOTTOM, ABM_GETSTATE, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABM_SETSTATE,
    ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE,
    SW_SHOWNOACTIVATE, ShowWindow, WM_APP,
};

use crate::{journal, watchdog};

const APPBAR_CALLBACK_MSG: u32 = WM_APP + 0x52;

static REGISTERED_APPBAR: AtomicUsize = AtomicUsize::new(0);

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

/// Reserves a bottom work-area strip of `height_px` physical pixels for `dock_hwnd`.
fn register_bottom_appbar(dock_hwnd: usize, height_px: i32) {
    if dock_hwnd == 0 || height_px <= 0 {
        return;
    }
    // SAFETY: standard Win32 AppBar registration for our own dock HWND.
    unsafe {
        let mut abd: APPBARDATA = std::mem::zeroed();
        abd.cbSize = size_of::<APPBARDATA>() as u32;
        abd.hWnd = dock_hwnd as HWND;
        abd.uCallbackMessage = APPBAR_CALLBACK_MSG;
        if SHAppBarMessage(ABM_NEW, &mut abd) == 0 {
            return;
        }
        REGISTERED_APPBAR.store(dock_hwnd, Ordering::SeqCst);

        let scr_w = GetSystemMetrics(SM_CXSCREEN);
        let scr_h = GetSystemMetrics(SM_CYSCREEN);
        abd.uEdge = ABE_BOTTOM;
        abd.rc = RECT {
            left: 0,
            top: (scr_h - height_px).max(0),
            right: scr_w,
            bottom: scr_h,
        };
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        abd.rc.top = (abd.rc.bottom - height_px).max(0);
        SHAppBarMessage(ABM_SETPOS, &mut abd);
    }
}

fn unregister_appbar() {
    let hwnd = REGISTERED_APPBAR.swap(0, Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    // SAFETY: removing our previously registered AppBar.
    unsafe {
        let mut abd: APPBARDATA = std::mem::zeroed();
        abd.cbSize = size_of::<APPBARDATA>() as u32;
        abd.hWnd = hwnd as HWND;
        SHAppBarMessage(ABM_REMOVE, &mut abd);
    }
}

/// Immediately hides Explorer's taskbar windows and runs `ABM_SETSTATE` + AppBar
/// registration on a background thread so GPUI paints its first frame without stalling.
pub fn begin_replacement(dock_hwnd: usize, auto_hide: bool, reserve_height_px: i32) {
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

    // Switch Explorer's hidden taskbar to auto-hide so its old work-area strip is released,
    // then reserve our exact dock height via AppBar when not auto-hiding.
    let target_state = prev | ABS_AUTOHIDE;
    std::thread::Builder::new()
        .name("opentaskbar-abm".into())
        .spawn(move || {
            if target_state != prev {
                set_taskbar_state(target_state);
            }
            if !auto_hide {
                register_bottom_appbar(dock_hwnd, reserve_height_px);
            }
        })
        .ok();
}

pub fn restore_from_journal() {
    unregister_appbar();
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
