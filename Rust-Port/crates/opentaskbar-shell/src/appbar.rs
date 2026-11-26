//! Single choke point for every `SHAppBarMessage` call made by the replacement
//! session.
//!
//! `SHAppBarMessage` has no timeout parameter and blocks while Explorer processes
//! the request. Before each call we probe Explorer's validated `Shell_TrayWnd`
//! with `SendMessageTimeout` (500 ms) so a hung or restarting Explorer is
//! detected instead of freezing the UI dispatcher for the shell's ~4 s timeout.
//! Sent-message reentrancy (`ABN_POSCHANGED` arriving while a call is still on
//! the stack) is broken with `NotificationsSuspended()`.

use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{APPBARDATA, SHAppBarMessage};
use windows_sys::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW};

use opentaskbar_core::log;

use crate::native_tray;

pub const ABM_NEW: u32 = 0;
pub const ABM_REMOVE: u32 = 1;
pub const ABM_QUERYPOS: u32 = 2;
pub const ABM_SETPOS: u32 = 3;
pub const ABM_GETSTATE: u32 = 4;
pub const ABM_GETTASKBARPOS: u32 = 5;
pub const ABM_SETSTATE: u32 = 10;

const WM_NULL: u32 = 0;
const EXPLORER_PROBE_TIMEOUT_MS: u32 = 500;

static NOTIFICATION_GATE: AtomicU32 = AtomicU32::new(0);

/// True while an AppBar call is in flight; shell callbacks must be deferred,
/// not handled inline.
pub fn notifications_suspended() -> bool {
    NOTIFICATION_GATE.load(Ordering::SeqCst) != 0
}

/// Defer inline `ABN_*` handling for the lifetime of an AppBar call. Windows
/// dispatches sent messages while a thread waits for a `SendMessage` reply, so
/// a callback can reenter our window procedure before `SHAppBarMessage` returns.
#[must_use]
pub struct NotificationScope;

impl NotificationScope {
    pub fn suspend() -> Self {
        NOTIFICATION_GATE.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for NotificationScope {
    fn drop(&mut self) {
        NOTIFICATION_GATE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Creates a correctly sized `APPBARDATA` for a given window.
pub fn create_data(hwnd: usize) -> APPBARDATA {
    let mut data: APPBARDATA = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<APPBARDATA>() as u32;
    data.hWnd = hwnd as HWND;
    data
}

/// Sends an AppBar message with a hung-Explorer probe, bounded telemetry and
/// reentrancy suppression. Returns 0 when no real Explorer taskbar exists or
/// when it failed to answer the 500 ms probe.
pub fn send(message: u32, data: &mut APPBARDATA, reason: &str) -> usize {
    let Some(taskbar) = native_tray::try_acquire() else {
        log::event(
            "AppBarSkipped",
            &[
                ("reason", reason.to_owned()),
                ("message", message.to_string()),
                ("cause", "no-taskbar".to_owned()),
            ],
        );
        return 0;
    };

    // SAFETY: a validated live Explorer window; the probe is bounded and read-only.
    let probe = unsafe {
        SendMessageTimeoutW(
            taskbar.hwnd as HWND,
            WM_NULL,
            0,
            0,
            SMTO_ABORTIFHUNG,
            EXPLORER_PROBE_TIMEOUT_MS,
            std::ptr::null_mut(),
        )
    };
    if probe == 0 {
        log::event(
            "AppBarSkipped",
            &[
                ("reason", reason.to_owned()),
                ("message", message.to_string()),
                ("cause", "explorer-hung".to_owned()),
                ("pid", taskbar.pid.to_string()),
            ],
        );
        return 0;
    }

    let sequence = log::next_sequence();
    let result = {
        let _scope = NotificationScope::suspend();
        // SAFETY: properly sized `APPBARDATA`, sent on the caller's UI thread.
        unsafe { SHAppBarMessage(message, data) }
    };

    log::event(
        "AppBarMessage",
        &[
            ("seq", sequence.to_string()),
            ("reason", reason.to_owned()),
            ("message", message.to_string()),
            ("result", result.to_string()),
            ("pid", taskbar.pid.to_string()),
        ],
    );
    result
}

pub fn get_state(reason: &str) -> u32 {
    let mut data = create_data(0);
    send(ABM_GETSTATE, &mut data, reason) as u32
}

/// Runs the two-step reservation protocol and returns the rectangle Explorer
/// actually assigned. The caller moves its window only after this returns, so
/// a synchronous `ABN_POSCHANGED` can never interleave with a half-committed
/// reservation.
pub fn set_position(
    hwnd: usize,
    edge: u32,
    requested: windows_sys::Win32::Foundation::RECT,
    pin_edge: bool,
    reason: &str,
) -> Option<windows_sys::Win32::Foundation::RECT> {
    let mut data = create_data(hwnd);
    data.uEdge = edge;
    data.rc = requested;
    if send(ABM_QUERYPOS, &mut data, &format!("{reason}/QUERYPOS")) == 0 {
        log::event(
            "AppBarQueryPosFailed",
            &[("reason", reason.to_owned()), ("edge", edge.to_string())],
        );
    }
    // The caller pins the rect to its requested edge between QUERYPOS and SETPOS.
    let _ = pin_edge;
    let committed = send(ABM_SETPOS, &mut data, &format!("{reason}/SETPOS"));
    (committed != 0).then_some(data.rc)
}

pub fn register(hwnd: usize, callback_message: u32, reason: &str) -> usize {
    let mut data = create_data(hwnd);
    data.uCallbackMessage = callback_message;
    send(ABM_NEW, &mut data, reason)
}

pub fn unregister(hwnd: usize, reason: &str) -> usize {
    let mut data = create_data(hwnd);
    send(ABM_REMOVE, &mut data, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_gate_nests_balanced() {
        assert!(!notifications_suspended());
        {
            let _outer = NotificationScope::suspend();
            let _inner = NotificationScope::suspend();
            assert!(notifications_suspended());
        }
        assert!(!notifications_suspended());
    }
}
