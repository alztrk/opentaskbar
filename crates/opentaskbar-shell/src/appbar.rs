//! Single choke point for every `SHAppBarMessage` call made by the replacement
//! session.
//!
//! `SHAppBarMessage` has no timeout parameter and blocks while Explorer processes
//! the request. Before each call we probe Explorer's validated `Shell_TrayWnd`
//! with `SendMessageTimeout` (250 ms) so a hung or restarting Explorer is
//! detected instead of freezing the UI dispatcher for the shell's ~4 s timeout.
//! Sent-message reentrancy (`ABN_POSCHANGED` arriving while a call is still on
//! the stack) is broken with `NotificationsSuspended()`.

use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{HWND, RECT};
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

/// `ABE_*` values are 0/1/2/3 — never the bogus 0x40/0x42 flags an older
/// secondary-shelf caller used for side docks.
pub const ABE_LEFT: u32 = 0;
pub const ABE_TOP: u32 = 1;
pub const ABE_RIGHT: u32 = 2;
pub const ABE_BOTTOM: u32 = 3;
pub const SUPPORTED_STATE_MASK: u32 = 0x03;

const WM_NULL: u32 = 0;
const EXPLORER_PROBE_TIMEOUT_MS: u32 = 250;

/// Normalizes an edge value to the real `ABE_*` range. Callers that still pass
/// 0x40/0x42 get ABE_LEFT/ABE_RIGHT instead of an invalid shell edge.
#[must_use]
pub const fn abe_edge(edge: u32) -> u32 {
    edge & 0x03
}

/// Pins a shell-resolved `QUERYPOS` rectangle to the requested edge while
/// preserving the thickness Explorer granted. Used between QUERYPOS and SETPOS.
#[must_use]
pub fn pin_rect_to_edge(mut rect: RECT, edge: u32, requested: RECT) -> RECT {
    match abe_edge(edge) {
        ABE_LEFT => {
            let width = (rect.right - rect.left).max(0);
            rect.right = requested.left + width;
            rect.left = requested.left;
        }
        ABE_RIGHT => {
            let width = (rect.right - rect.left).max(0);
            rect.left = requested.right - width;
            rect.right = requested.right;
        }
        ABE_TOP => {
            let top = rect.top.max(requested.top);
            rect.bottom = top + (rect.bottom - rect.top).max(0);
            rect.top = top;
        }
        ABE_BOTTOM => {
            let bottom = rect.bottom.min(requested.bottom);
            rect.top = bottom - (rect.bottom - rect.top).max(0);
            rect.bottom = bottom;
        }
        _ => {}
    }
    rect
}

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
/// when it failed to answer the 250 ms probe.
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

/// True when Explorer reports the requested state (only the two supported bits
/// are compared, matching `SetTaskbarState`'s `SupportedTaskbarState` mask).
#[must_use]
pub const fn state_matches(requested: u32, observed: u32) -> bool {
    (observed & SUPPORTED_STATE_MASK) == (requested & SUPPORTED_STATE_MASK)
}

/// Publishes an AppBar state and verifies it with a fresh `ABM_GETSTATE`.
/// Skips the send when the state already matches, so our own
/// `ABN_STATECHANGE` cannot start a reconcile feedback loop.
pub fn set_and_verify_state(state: u32, reason: &str) -> bool {
    let state = state & SUPPORTED_STATE_MASK;
    if state_matches(state, get_state(&format!("{reason}/GetState"))) {
        return true;
    }
    let Some(taskbar) = native_tray::try_acquire() else {
        return false;
    };
    let mut data = create_data(taskbar.hwnd);
    data.lParam = state as isize;
    send(ABM_SETSTATE, &mut data, reason);
    let verified = state_matches(state, get_state(&format!("{reason}/Verify")));
    log::event(
        "AppBarStateVerified",
        &[
            ("reason", reason.to_owned()),
            ("state", state.to_string()),
            ("verified", verified.to_string()),
        ],
    );
    verified
}

/// Runs the two-step reservation protocol and returns the rectangle Explorer
/// actually assigned. `pin_edge = true` pins the QUERYPOS result to the
/// requested edge before SETPOS (the reference `PositionAdjustment` callback).
pub fn set_position(
    hwnd: usize,
    edge: u32,
    requested: RECT,
    pin_edge: bool,
    reason: &str,
) -> Option<RECT> {
    let edge = abe_edge(edge);
    let mut data = create_data(hwnd);
    data.uEdge = edge;
    data.rc = requested;
    if send(ABM_QUERYPOS, &mut data, &format!("{reason}/QUERYPOS")) == 0 {
        log::event(
            "AppBarQueryPosFailed",
            &[("reason", reason.to_owned()), ("edge", edge.to_string())],
        );
    }
    if pin_edge {
        data.rc = pin_rect_to_edge(data.rc, edge, requested);
    }
    let committed = send(ABM_SETPOS, &mut data, &format!("{reason}/SETPOS"));
    (committed != 0).then_some(data.rc)
}

/// Edge-pinning wrapper used for the secondary dock shelf and vertical docks.
pub fn set_position_pinned(hwnd: usize, edge: u32, requested: RECT, reason: &str) -> Option<RECT> {
    set_position(hwnd, edge, requested, true, reason)
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

    #[test]
    fn side_edges_are_real_abe_values_not_position_flags() {
        assert_eq!(abe_edge(ABE_LEFT), 0);
        assert_eq!(abe_edge(ABE_TOP), 1);
        assert_eq!(abe_edge(ABE_RIGHT), 2);
        assert_eq!(abe_edge(ABE_BOTTOM), 3);
        assert_eq!(abe_edge(0x40), ABE_LEFT);
        assert_eq!(abe_edge(0x42), ABE_RIGHT);
    }

    #[test]
    fn pin_rect_preserves_shell_thickness_and_moves_only_the_edge() {
        let query = RECT {
            left: 60,
            top: 100,
            right: 132,
            bottom: 150,
        };
        let requested = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let left = pin_rect_to_edge(query, 0x40, requested);
        assert_eq!((left.left, left.right), (0, 72));
        let right = pin_rect_to_edge(query, 0x42, requested);
        assert_eq!((right.left, right.right), (1848, 1920));

        let requested_bottom = RECT {
            bottom: 1040,
            ..requested
        };
        let bottom = pin_rect_to_edge(
            RECT {
                top: 1050,
                bottom: 1100,
                ..query
            },
            ABE_BOTTOM,
            requested_bottom,
        );
        assert_eq!((bottom.top, bottom.bottom), (990, 1040));
        // A shell-trimmed rect is never expanded past the requested edge.
        let trimmed = pin_rect_to_edge(
            RECT {
                top: 850,
                bottom: 900,
                ..query
            },
            ABE_BOTTOM,
            requested_bottom,
        );
        assert_eq!((trimmed.top, trimmed.bottom), (850, 900));
    }

    #[test]
    fn state_verification_compares_supported_bits_and_probe_is_250ms() {
        assert!(state_matches(0, 0));
        assert!(state_matches(3, 0x83));
        assert!(!state_matches(0, 2));
        assert_eq!(EXPLORER_PROBE_TIMEOUT_MS, 250);
    }
}
