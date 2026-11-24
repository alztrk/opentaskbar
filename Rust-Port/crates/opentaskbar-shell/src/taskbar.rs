//! Explorer taskbar hiding, restoration, exact delta AppBar reservation, and
//! native Windows shell shortcut triggers (Start, Search, Task View).

use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_CONTROL, VK_LEFT,
    VK_LWIN, VK_RIGHT, VK_TAB,
};
use windows_sys::Win32::UI::Shell::{
    ABE_BOTTOM, ABM_GETSTATE, ABM_GETTASKBARPOS, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS,
    ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetSystemMetrics, HWND_BROADCAST, IsWindowVisible,
    RegisterWindowMessageW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    SendNotifyMessageW, SetWindowPos, ShowWindow, WM_APP,
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
            if (!visible && IsWindowVisible(current) != 0)
                || (visible && IsWindowVisible(current) == 0)
            {
                ShowWindow(current, cmd);
            }
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

/// Returns Explorer's currently reserved bottom taskbar strip height in physical pixels.
fn native_bottom_taskbar_height_px() -> i32 {
    let mut data: APPBARDATA = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<APPBARDATA>() as u32;
    // SAFETY: properly sized APPBARDATA for ABM_GETTASKBARPOS.
    let ok = unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) };
    if ok == 0 || data.uEdge != ABE_BOTTOM {
        return 0;
    }
    (data.rc.bottom - data.rc.top).max(0)
}

/// Reserves ONLY the extra height needed beyond Explorer's existing hidden bottom strip,
/// so the total work-area strip never doubles.
pub fn register_bottom_appbar(dock_hwnd: usize, desired_total_px: i32, prev_state: u32) {
    if dock_hwnd == 0 || desired_total_px <= 0 {
        return;
    }
    if REGISTERED_APPBAR.load(Ordering::SeqCst) == dock_hwnd {
        return;
    }
    let prev_was_autohide = (prev_state & ABS_AUTOHIDE) != 0;
    let native_h = if prev_was_autohide {
        0
    } else {
        native_bottom_taskbar_height_px()
    };
    let extra_px = (desired_total_px - native_h).max(0);
    if extra_px == 0 {
        return;
    }

    // SAFETY: standard Win32 AppBar registration on the window's owning UI thread.
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
            top: (scr_h - extra_px).max(0),
            right: scr_w,
            bottom: scr_h,
        };
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        abd.rc.top = (abd.rc.bottom - extra_px).max(0);
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

pub fn ensure_hidden() -> bool {
    let mut hid = false;
    let tray = find_primary_tray();
    if !tray.is_null() {
        // SAFETY: checking visibility and hiding Explorer's primary taskbar.
        unsafe {
            if IsWindowVisible(tray) != 0 {
                ShowWindow(tray, SW_HIDE);
                hid = true;
            }
        }
    }
    set_secondary_trays_visible(false);
    hid
}

pub fn begin_replacement(dock_hwnd: usize, auto_hide: bool, desired_total_px: i32) {
    let prev = journal::read_state().unwrap_or_else(get_taskbar_state);
    if !journal::has_pending() {
        let _ = journal::write_state(prev);
    }
    watchdog::arm();
    ensure_hidden();

    if auto_hide {
        let target_state = prev | ABS_AUTOHIDE;
        if target_state != prev {
            set_taskbar_state(target_state);
            ensure_hidden();
        }
    } else {
        register_bottom_appbar(dock_hwnd, desired_total_px, prev);
        ensure_hidden();
    }
}

pub fn restore_from_journal() {
    unregister_appbar();
    let prev = journal::read_state().unwrap_or(2); // ABS_ALWAYSONTOP default
    let tray = find_primary_tray();
    if tray.is_null() {
        return;
    }
    // Mirror `watchdog.c` RestoreNativeTaskbars: 3 retries of ABM_SETSTATE + SW_SHOW + SetWindowPos.
    for attempt in 0..3 {
        set_taskbar_state(prev);
        // SAFETY: restoring Explorer's primary taskbar window with SWP_SHOWWINDOW | SWP_FRAMECHANGED.
        unsafe {
            ShowWindow(tray, SW_SHOW);
            SetWindowPos(
                tray,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOZORDER
                    | SWP_NOACTIVATE
                    | SWP_SHOWWINDOW
                    | SWP_FRAMECHANGED,
            );
        }
        set_secondary_trays_visible(true);
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
    }
    // Only clear journal if the native taskbar is confirmed visible.
    let visible = unsafe { IsWindowVisible(tray) != 0 };
    if visible {
        journal::clear();
        let msg_name = wide("TaskbarCreated");
        // SAFETY: notifying apps to re-register tray icons with Explorer.
        unsafe {
            let msg = RegisterWindowMessageW(msg_name.as_ptr());
            if msg != 0 {
                SendNotifyMessageW(HWND_BROADCAST, msg, 0, 0);
            }
        }
    }
}

pub fn end_replacement() {
    restore_from_journal();
    watchdog::notify_clean_exit();
}

// ---- Native Windows shell triggers ----

fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_chord(keys: &[u16]) {
    let mut inputs = Vec::with_capacity(keys.len() * 2);
    for &vk in keys {
        inputs.push(key_input(vk, false));
    }
    for &vk in keys.iter().rev() {
        inputs.push(key_input(vk, true));
    }
    // SAFETY: properly sized INPUT array for SendInput.
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        );
    }
}

pub fn open_start_menu() {
    send_chord(&[VK_LWIN]);
}

pub fn open_search() {
    send_chord(&[VK_LWIN, b'S' as u16]);
}

pub fn open_task_view() {
    send_chord(&[VK_LWIN, VK_TAB]);
}

pub fn show_desktop() {
    send_chord(&[VK_LWIN, b'D' as u16]);
}

pub fn open_run_dialog() {
    send_chord(&[VK_LWIN, b'R' as u16]);
}

pub fn previous_desktop() {
    send_chord(&[VK_CONTROL, VK_LWIN, VK_LEFT]);
}

pub fn next_desktop() {
    send_chord(&[VK_CONTROL, VK_LWIN, VK_RIGHT]);
}

pub fn new_desktop() {
    send_chord(&[VK_CONTROL, VK_LWIN, b'D' as u16]);
}
