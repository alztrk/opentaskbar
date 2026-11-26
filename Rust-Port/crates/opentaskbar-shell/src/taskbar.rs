//! Explorer taskbar hiding, restoration, exact delta AppBar reservation, and
//! native Windows shell shortcut triggers (Start, Search, Task View).

use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput,
    VK_CONTROL, VK_LEFT, VK_LWIN, VK_RIGHT, VK_RWIN, VK_TAB,
};
use windows_sys::Win32::UI::Shell::{ABE_BOTTOM, ABS_AUTOHIDE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetSystemMetrics, HWND_BROADCAST, IsWindowVisible, RegisterWindowMessageW,
    SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SendNotifyMessageW,
    SetWindowPos, ShowWindow, WM_APP,
};

use crate::{appbar, flyout_writer, journal, native_tray, session, watchdog};

const APPBAR_CALLBACK_MSG: u32 = WM_APP + 0x52;

static REGISTERED_APPBAR: AtomicUsize = AtomicUsize::new(0);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn find_primary_tray() -> HWND {
    // Never act on a fake Shell_TrayWnd owned by OpenTaskbar itself.
    native_tray::find().map_or(std::ptr::null_mut(), |lease| lease.hwnd as HWND)
}

fn set_secondary_trays_visible(visible: bool) {
    let cmd = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
    for lease in native_tray::find_secondary_leases() {
        if !lease.is_valid() {
            continue;
        }
        // SAFETY: lease-validated Explorer-owned secondary taskbar.
        unsafe {
            if (!visible && IsWindowVisible(lease.hwnd as HWND) != 0)
                || (visible && IsWindowVisible(lease.hwnd as HWND) == 0)
            {
                ShowWindow(lease.hwnd as HWND, cmd);
            }
        }
    }
}

pub fn get_taskbar_state() -> u32 {
    appbar::get_state("GetState")
}

pub fn set_taskbar_state(state: u32) {
    let tray = find_primary_tray();
    if tray.is_null() {
        return;
    }
    let mut data = appbar::create_data(tray as usize);
    data.lParam = state as isize;
    appbar::send(appbar::ABM_SETSTATE, &mut data, "SetState");
}

/// Returns Explorer's currently reserved bottom taskbar strip height in physical pixels.
fn native_bottom_taskbar_height_px() -> i32 {
    let mut data = appbar::create_data(0);
    // SAFETY: properly sized APPBARDATA for ABM_GETTASKBARPOS through the
    // hung-probe choke point.
    let ok = appbar::send(appbar::ABM_GETTASKBARPOS, &mut data, "GetTaskbarPos");
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

    let mut abd = appbar::create_data(dock_hwnd);
    abd.uCallbackMessage = APPBAR_CALLBACK_MSG;
    if appbar::send(appbar::ABM_NEW, &mut abd, "Register") == 0 {
        return;
    }
    REGISTERED_APPBAR.store(dock_hwnd, Ordering::SeqCst);

    // SAFETY: querying primary screen dimensions.
    let (scr_w, scr_h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    abd.uEdge = ABE_BOTTOM;
    abd.rc = RECT {
        left: 0,
        top: (scr_h - extra_px).max(0),
        right: scr_w,
        bottom: scr_h,
    };
    appbar::send(appbar::ABM_QUERYPOS, &mut abd, "Register/QUERYPOS");
    abd.rc.top = (abd.rc.bottom - extra_px).max(0);
    appbar::send(appbar::ABM_SETPOS, &mut abd, "Register/SETPOS");
}

fn unregister_appbar() {
    let hwnd = REGISTERED_APPBAR.swap(0, Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    let mut abd = appbar::create_data(hwnd);
    appbar::send(appbar::ABM_REMOVE, &mut abd, "Unregister");
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
    // Record where Windows put its own flyouts *before* we anchor them. The
    // writer is a no-op unless `native_flyout_reposition` is enabled.
    flyout_writer::write_start();
    flyout_writer::write_search();
    flyout_writer::write_control_center();
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
    // Flyouts are hosted by long-lived shell processes, so put them back first.
    flyout_writer::restore_from_journal();
    let prev = journal::read_state().unwrap_or(2); // ABS_ALWAYSONTOP default
    let tray = find_primary_tray();
    if tray.is_null() {
        return;
    }
    // Mirror `watchdog.c` RestoreNativeTaskbars: 3 retries of ABM_SETSTATE +
    // SW_SHOW + SetWindowPos, then verify visibility before ever clearing the journal.
    for _ in 0..session::RESTORE_ATTEMPTS {
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
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    let verified = session::verify_restored(tray as usize, session::RESTORE_ATTEMPTS);
    opentaskbar_core::shell_result!("Restore", "Journal", true, verified, ms = 0);
    // Never drop the only recovery clue while Explorer is still re-hiding the tray.
    if !verified {
        return;
    }
    journal::clear();
    // A clean restore is the point where the watchdog must stop watching us.
    // `end_replacement` also calls this, but it is idempotent.
    watchdog::notify_clean_exit();
    broadcast_taskbar_created();
}

fn broadcast_taskbar_created() {
    let msg_name = wide("TaskbarCreated");
    // SAFETY: notifying apps to re-register tray icons with Explorer.
    unsafe {
        let msg = RegisterWindowMessageW(msg_name.as_ptr());
        if msg != 0 {
            SendNotifyMessageW(HWND_BROADCAST, msg, 0, 0);
        }
    }
}

/// Called when Explorer broadcasts `TaskbarCreated`: re-assert the reservation
/// and hide state through the same bounded reconcile path used by recovery.
pub fn handle_taskbar_created(dock_hwnd: usize, auto_hide: bool, desired_total_px: i32) {
    opentaskbar_core::log::event(
        "TaskbarCreated",
        &[
            ("seq", opentaskbar_core::log::next_sequence().to_string()),
            ("state", "reconcile".to_owned()),
        ],
    );
    session::reconcile_once(dock_hwnd, auto_hide, desired_total_px, "TaskbarCreated");
}

pub fn end_replacement() {
    restore_from_journal();
    flyout_writer::restore();
    watchdog::notify_clean_exit();
}

// ---- Native Windows shell triggers ----

/// The Windows key must carry `KEYEVENTF_EXTENDEDKEY`; without it injected
/// keydowns are swallowed by Explorer's shell keyboard handling.
fn is_extended_key(vk: u16) -> bool {
    matches!(vk, VK_LWIN | VK_RWIN)
}

fn key_input(vk: u16, up: bool, extended: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_key(vk: u16, up: bool) -> bool {
    let input = key_input(vk, up, is_extended_key(vk));
    // SAFETY: one properly sized INPUT structure for SendInput.
    unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) == 1 }
}

/// Sends the keys with a ~20 ms down/up separation: a single batched
/// `SendInput` of all events makes Explorer treat Win-key chords as a single
/// synthetic press instead of a shell shortcut.
fn send_chord(keys: &[u16]) -> bool {
    if keys.is_empty() {
        return false;
    }
    let mut ok = true;
    for &vk in keys {
        ok &= send_key(vk, false);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for &vk in keys.iter().rev() {
        ok &= send_key(vk, true);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    ok
}

/// Preferred Start entry point: `SC_TASKLIST` posted to the dock (owner) HWND.
/// Falls back to the injected Win-key chord when no valid owner is supplied.
pub fn open_start_menu_with_owner(owner_hwnd: usize) -> bool {
    if owner_hwnd != 0 && crate::flyout_reposition::open_start_via_syscommand(owner_hwnd) {
        return true;
    }
    send_chord(&[VK_LWIN])
}

/// Backwards-compatible wrapper for callers that do not have an owner HWND.
pub fn open_start_menu() {
    let _ = open_start_menu_with_owner(0);
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

/// Updates the registered AppBar height in-place without restoring/flashing Explorer's taskbar
/// (fixes Issue #86 when changing dock size/scale in Settings).
pub fn update_appbar_height(dock_hwnd: usize, auto_hide: bool, desired_total_px: i32) {
    if auto_hide {
        unregister_appbar();
        return;
    }
    let prev = journal::read_state().unwrap_or(2);
    let prev_was_autohide = (prev & ABS_AUTOHIDE) != 0;
    let native_h = if prev_was_autohide {
        0
    } else {
        native_bottom_taskbar_height_px()
    };
    let extra_px = (desired_total_px - native_h).max(0);
    if extra_px == 0 {
        unregister_appbar();
        return;
    }
    if REGISTERED_APPBAR.load(Ordering::SeqCst) != dock_hwnd {
        register_bottom_appbar(dock_hwnd, desired_total_px, prev);
        return;
    }
    let mut abd = appbar::create_data(dock_hwnd);
    // SAFETY: querying primary screen dimensions.
    let (scr_w, scr_h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    abd.uEdge = ABE_BOTTOM;
    abd.rc = RECT {
        left: 0,
        top: (scr_h - extra_px).max(0),
        right: scr_w,
        bottom: scr_h,
    };
    appbar::send(appbar::ABM_QUERYPOS, &mut abd, "UpdateHeight/QUERYPOS");
    abd.rc.top = (abd.rc.bottom - extra_px).max(0);
    appbar::send(appbar::ABM_SETPOS, &mut abd, "UpdateHeight/SETPOS");
}

pub fn new_desktop() {
    send_chord(&[VK_CONTROL, VK_LWIN, b'D' as u16]);
}

pub fn open_notifications() {
    send_chord(&[VK_LWIN, b'N' as u16]);
}

/// Opens Windows' hidden background-apps tray overflow popup.
pub fn open_tray_overflow() {
    let win11_cls = wide("TopLevelWindowForOverflowXamlIsland");
    let win10_cls = wide("NotifyIconOverflowWindow");
    unsafe {
        let mut overflow = FindWindowW(win11_cls.as_ptr(), std::ptr::null());
        if overflow.is_null() {
            overflow = FindWindowW(win10_cls.as_ptr(), std::ptr::null());
        }
        if !overflow.is_null() && IsWindowVisible(overflow) != 0 {
            ShowWindow(overflow, SW_HIDE);
            return;
        }
    }
    // Trigger Explorer's native notification-area chevron via Win+B then Space.
    send_chord(&[VK_LWIN, b'B' as u16]);
    std::thread::Builder::new()
        .name("opentaskbar-tray".into())
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(60));
            send_chord(&[0x20]); // VK_SPACE
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_key_is_marked_extended() {
        assert!(is_extended_key(VK_LWIN));
        assert!(is_extended_key(VK_RWIN));
        assert!(!is_extended_key(b'S' as u16));
        assert!(!is_extended_key(VK_TAB));
    }

    #[test]
    fn key_input_sets_extended_and_keyup_flags() {
        let down = key_input(VK_LWIN, false, true);
        let up = key_input(VK_LWIN, true, true);
        let plain = key_input(b'S' as u16, false, false);
        // SAFETY: these INPUT unions were just built by `key_input` with the
        // keyboard variant active, so reading `ki` is well defined.
        unsafe {
            assert_eq!(down.Anonymous.ki.dwFlags, KEYEVENTF_EXTENDEDKEY);
            assert_eq!(
                up.Anonymous.ki.dwFlags,
                KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP
            );
            assert_eq!(down.Anonymous.ki.wVk, VK_LWIN);
            assert_eq!(plain.Anonymous.ki.dwFlags, 0);
        }
    }
}
