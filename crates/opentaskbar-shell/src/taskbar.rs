//! Explorer taskbar hiding, restoration, exact delta AppBar reservation, and
//! native Windows shell shortcut triggers (Start, Search, Task View).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
    SetWindowPos, ShowWindow,
};

use crate::{appbar, flyout_writer, journal, modifiers, native_tray, session, watchdog};

/// `TrayHost.SelfBroadcastWParam` / `RestoreBroadcastWParam`: the dock must
/// not mistake its own restore broadcast for an icon-collection ping.
const SELF_BROADCAST_WPARAM: usize = 1;
const RESTORE_BROADCAST_WPARAM: usize = 2;

static REGISTERED_APPBAR: AtomicUsize = AtomicUsize::new(0);
static BROADCAST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
/// Result of the most recent restore attempt. Kept internal because app call
/// sites use `restore_from_journal()` as a statement; only `end_replacement`
/// consumes it to authorize the clean-exit signal.
static LAST_RESTORE_VERIFIED: AtomicBool = AtomicBool::new(false);

/// `TaskbarCreated` broadcasts carry a source tag in `wParam`, matching reference
/// `TrayHost.BroadcastTaskbarCreated(source)`.
#[must_use]
pub fn broadcast_wparam(source: &str) -> usize {
    if source.eq_ignore_ascii_case("restore") {
        RESTORE_BROADCAST_WPARAM
    } else {
        SELF_BROADCAST_WPARAM
    }
}

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
    appbar::get_state("GetState") & appbar::SUPPORTED_STATE_MASK
}

/// `IsNativeTaskbarVisible`: primary *and* every secondary tray must be
/// visible before a journal may be deleted.
fn native_taskbar_is_visible() -> bool {
    let Some(tray) = native_tray::try_acquire() else {
        return false;
    };
    // SAFETY: lease-validated Explorer taskbar.
    if unsafe { IsWindowVisible(tray.hwnd as HWND) } == 0 {
        return false;
    }
    native_tray::find_secondary_leases()
        .iter()
        .all(|lease| !lease.is_valid() || unsafe { IsWindowVisible(lease.hwnd as HWND) } != 0)
}

/// Journal + live shell state, applying stale-journal detection. This replaces
/// the old `unwrap_or(2)` guess: default state is 0 (visible taskbar).
pub fn active_state() -> u32 {
    journal::effective_state(get_taskbar_state(), native_taskbar_is_visible())
}

pub fn set_taskbar_state(state: u32) {
    // GetState -> SETSTATE -> GetState, no fake HWND fallback when Explorer is absent.
    let _ = appbar::set_and_verify_state(state, "SetState");
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

    // No ABN_* WndProc is registered by the Rust dock (reconcile is driven by
    // WinEvent hooks), so advertise no callback message instead of a dead one.
    let mut abd = appbar::create_data(dock_hwnd);
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
    let prev = active_state();
    // Arm the shell-hook drivers used by the 20/40 hide budget and the
    // 45-attempt degraded budget before any Explorer state is changed.
    session::configure_replacement(dock_hwnd, auto_hide, desired_total_px);
    if !journal::has_pending()
        && let Err(err) = journal::write_state(prev)
    {
        opentaskbar_core::log::warn(format!(
            "Could not write taskbar-state recovery journal; crash recovery may be degraded: {err}"
        ));
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
    // Bounded startup settle: 40 attempts / 10 consecutive clean passes.
    session::start_hide_retry(true);
}

/// Restores Explorer from the journal. The outcome is recorded internally for
/// `end_replacement` and is deliberately not returned: the existing app call
/// sites (startup heal, panic path and the live "Restore taskbar" action) use
/// this as a statement. A manual restore never signals the watchdog.
pub fn restore_from_journal() {
    unregister_appbar();
    // Flyouts are hosted by long-lived shell processes, so put them back first.
    flyout_writer::restore_from_journal();
    // Default state 0, stale journal ignored when the tray is already visible
    // with a different state (reference ForceRestore).
    let prev = active_state();
    let tray = find_primary_tray();
    let verified = if tray.is_null() {
        false
    } else {
        // Mirror `watchdog.c` RestoreNativeTaskbars: at most 3 rounds of
        // ABM_SETSTATE + SW_SHOW + SetWindowPos, requiring two consecutive visible
        // observations. There is NO sleep after the final round.
        session::verify_restored_with(tray as usize, session::RESTORE_ATTEMPTS, || {
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
        })
    };
    opentaskbar_core::shell_result!("Restore", "Journal", true, verified, ms = 0);
    LAST_RESTORE_VERIFIED.store(verified, Ordering::SeqCst);
    // Never publish a clean-exit authorization here: a live "Restore taskbar"
    // action uses this same function, and must leave the watchdog armed. Only
    // `end_replacement` (the real shutdown path) publishes verification.
    // Never drop the only recovery clue while Explorer is still re-hiding the tray.
    if !verified {
        return;
    }
    journal::clear();
    broadcast_taskbar_created("restore");
}

fn broadcast_taskbar_created(source: &str) {
    let msg_name = wide("TaskbarCreated");
    // SAFETY: notifying apps to re-register tray icons with Explorer.
    unsafe {
        let msg = RegisterWindowMessageW(msg_name.as_ptr());
        if msg != 0 {
            let sequence = BROADCAST_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
            SendNotifyMessageW(
                HWND_BROADCAST,
                msg,
                broadcast_wparam(source),
                sequence as isize,
            );
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
    let _ = (dock_hwnd, auto_hide, desired_total_px);
    // Two passes, cache/identity reset and degraded-budget reset in one place.
    let _ = session::reconcile_after_taskbar_created();
}

/// Clears the stale work-area reservation registered against a taskbar that no
/// longer exists. Called by `TaskbarCreated` before the fresh registration.
pub fn clear_appbar_registration() {
    let hwnd = REGISTERED_APPBAR.swap(0, Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    let mut abd = appbar::create_data(hwnd);
    // Zero-thickness SETPOS releases the old strip before ABM_REMOVE.
    abd.rc = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    appbar::send(appbar::ABM_SETPOS, &mut abd, "ClearRegistration/SETPOS");
    appbar::send(appbar::ABM_REMOVE, &mut abd, "ClearRegistration/REMOVE");
}

/// Pure classification of one hide-enforcement observation.
#[must_use]
pub const fn classify_hide_observation(
    has_taskbar: bool,
    was_visible: bool,
    hid: bool,
) -> session::HideObservation {
    if !has_taskbar {
        session::HideObservation::MissingTaskbar
    } else if !was_visible {
        session::HideObservation::AlreadyHidden
    } else if hid {
        session::HideObservation::HidVisible
    } else {
        session::HideObservation::StillVisible
    }
}

/// Live observation consumed by `session::run_hide_retry_step`.
pub fn hide_observation() -> session::HideObservation {
    let Some(tray) = native_tray::try_acquire() else {
        return session::HideObservation::MissingTaskbar;
    };
    // SAFETY: lease-validated Explorer taskbar; `ensure_hidden` revalidates.
    let was_visible = unsafe { IsWindowVisible(tray.hwnd as HWND) != 0 };
    let hid = was_visible && ensure_hidden();
    classify_hide_observation(true, was_visible, hid)
}

pub fn end_replacement() {
    // Restore first, verify second, signal third. The legacy notify call in
    // main.rs runs before this function and is intentionally a no-op.
    restore_from_journal();
    let verified = LAST_RESTORE_VERIFIED.swap(false, Ordering::SeqCst);
    watchdog::set_restore_verified(verified);
    flyout_writer::restore();
    watchdog::notify_clean_exit();
    // Do not leave a stale authorization for a later premature call.
    watchdog::set_restore_verified(false);
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
///
/// Every successful keydown/keyup is recorded in [`modifiers`]. If any
/// `SendInput` fails (partial keydown or keyup), the finally path drains the
/// tracker and injects key-ups for the still-held keys, so a stuck modifier
/// can never survive a failed chord.
pub(crate) fn send_chord(keys: &[u16]) -> bool {
    if keys.is_empty() {
        return false;
    }
    let mut ok = true;

    for &vk in keys {
        if !send_key(vk, false) {
            ok = false;
            break;
        }
        modifiers::note_down(vk);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Reverse key-ups, always attempted even if a keydown or keyup failed.
    for &vk in keys.iter().rev() {
        if send_key(vk, true) {
            modifiers::note_up(vk);
        } else {
            ok = false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    if !ok {
        release_held_keys();
    }
    ok
}

/// Releases every key still tracked as held by an injected chord and clears
/// the tracker. Wire this to the session-unlock handler (alongside
/// `session::install_shell_hooks`) so a lock that swallowed key-ups cannot
/// leave Win/Ctrl/Alt stuck down. Safe to call repeatedly: a second call with
/// nothing held is a no-op.
pub fn release_held_keys() {
    for vk in modifiers::release_all() {
        let _ = send_key(vk, true);
    }
}

/// Preferred Start entry point: `SC_TASKLIST` posted to the dock (owner) HWND.
/// `open_start_via_syscommand` verifies the result with `find_start_window()`
/// and runs the injected `VK_LWIN` chord fallback itself when the post does
/// not open Start, so the fallback is used for every real open attempt.
pub fn open_start_menu_with_owner(owner_hwnd: usize) -> bool {
    if owner_hwnd != 0 {
        return crate::flyout_reposition::open_start_via_syscommand(owner_hwnd);
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
    let prev = active_state();
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
    fn empty_chord_is_rejected_without_injecting_keys() {
        assert!(!send_chord(&[]));
    }

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

    #[test]
    fn restore_broadcast_is_tagged_and_own_broadcast_is_not() {
        assert_eq!(broadcast_wparam("restore"), 2);
        assert_eq!(broadcast_wparam("RESTORE"), 2);
        assert_eq!(broadcast_wparam("self"), 1);
        assert_eq!(broadcast_wparam(""), 1);
        assert_eq!(SELF_BROADCAST_WPARAM, 1);
        assert_eq!(RESTORE_BROADCAST_WPARAM, 2);
    }

    #[test]
    fn hide_observation_distinguishes_missing_hidden_and_visible_trays() {
        use session::HideObservation;
        assert_eq!(
            classify_hide_observation(false, false, false),
            HideObservation::MissingTaskbar
        );
        assert_eq!(
            classify_hide_observation(true, false, false),
            HideObservation::AlreadyHidden
        );
        assert_eq!(
            classify_hide_observation(true, true, true),
            HideObservation::HidVisible
        );
        assert_eq!(
            classify_hide_observation(true, true, false),
            HideObservation::StillVisible
        );
    }
}
