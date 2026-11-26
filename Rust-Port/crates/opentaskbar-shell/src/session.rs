//! Pure, bounded state-machine logic for taskbar replacement/recovery.
//!
//! Mirrors the important constants and decisions of C#
//! `TaskbarReplacementSession`: hide retries (20 normal / 40 startup-settle
//! attempts with 2 / 10 consecutive clean passes), a 45-attempt degraded
//! reconcile budget, `TaskbarCreated` reconciliation, side-dock work-area
//! reservation, and verified restore with three retries. The window-message
//! loop stays in the app crate; this module exposes the decisions it must make
//! so they can be unit-tested without a UI thread.

use std::time::Duration;

use windows_sys::Win32::Foundation::{HANDLE, HMODULE, HWND};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetClassNameW, GetMessageW, IsWindowVisible, MSG, SW_SHOW, ShowWindow,
};

use opentaskbar_core::log;

use crate::{appbar, native_tray, taskbar};

pub const MAX_DEGRADED_RECONCILE_ATTEMPTS: u32 = 45;
pub const NORMAL_HIDE_REQUIRED_CLEAN_PASSES: u32 = 2;
pub const STARTUP_SETTLE_REQUIRED_CLEAN_PASSES: u32 = 10;
pub const NORMAL_HIDE_MAX_ATTEMPTS: u32 = 20;
pub const STARTUP_SETTLE_MAX_ATTEMPTS: u32 = 40;
pub const RESTORE_ATTEMPTS: u32 = 3;
pub const HIDE_RETRY_SETTLE_SLEEP_MS: u64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HideObservation {
    MissingTaskbar,
    AlreadyHidden,
    HidVisible,
    StillVisible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryDecision {
    pub should_retry: bool,
    pub delay_ms: u32,
    pub exhausted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HideRetryStep {
    pub stop_timer: bool,
    pub settled: bool,
    pub needs_degraded_reconcile: bool,
    pub exhausted: bool,
    pub next_delay_ms: u32,
    pub next_consecutive_clean: u32,
}

/// Bounded backoff for a missing/busy Explorer taskbar (~60 s total), then stop
/// with zero idle cost while `EVENT_OBJECT_SHOW` and `TaskbarCreated` stay armed.
pub fn evaluate_degraded_reconcile_retry(attempt_number: u32) -> RetryDecision {
    let attempt = attempt_number.max(1);
    if attempt > MAX_DEGRADED_RECONCILE_ATTEMPTS {
        return RetryDecision {
            should_retry: false,
            delay_ms: 0,
            exhausted: true,
        };
    }
    let delay_ms = if attempt <= 10 {
        500
    } else if attempt <= 25 {
        1000
    } else {
        2000
    };
    RetryDecision {
        should_retry: true,
        delay_ms,
        exhausted: false,
    }
}

pub fn hide_retry_initial_delay_ms(require_stable_settle: bool) -> u32 {
    if require_stable_settle { 500 } else { 250 }
}

/// Pure step function for bounded hide enforcement. Requires consecutive clean
/// observations so startup / `TaskbarCreated` enforcement does not stop before
/// Explorer finishes XAML shell init, but still stops with zero idle cost once
/// trays stay hidden or the bounded budget ends.
pub fn evaluate_hide_retry_step(
    observation: HideObservation,
    attempt_number: u32,
    consecutive_clean: u32,
    require_stable_settle: bool,
) -> HideRetryStep {
    let attempt = attempt_number.max(1);
    let clean = consecutive_clean;

    if observation == HideObservation::MissingTaskbar {
        return HideRetryStep {
            stop_timer: true,
            settled: false,
            needs_degraded_reconcile: true,
            exhausted: false,
            next_delay_ms: 0,
            next_consecutive_clean: 0,
        };
    }

    let required_clean = if require_stable_settle {
        STARTUP_SETTLE_REQUIRED_CLEAN_PASSES
    } else {
        NORMAL_HIDE_REQUIRED_CLEAN_PASSES
    };
    let max_attempts = if require_stable_settle {
        STARTUP_SETTLE_MAX_ATTEMPTS
    } else {
        NORMAL_HIDE_MAX_ATTEMPTS
    };
    let next_delay_ms = if require_stable_settle {
        if attempt <= 6 {
            500
        } else if attempt <= 20 {
            1000
        } else {
            1500
        }
    } else if attempt <= 6 {
        250
    } else if attempt <= 14 {
        500
    } else {
        1000
    };

    if observation == HideObservation::AlreadyHidden {
        let next_clean = clean + 1;
        if next_clean >= required_clean || attempt >= max_attempts {
            return HideRetryStep {
                stop_timer: true,
                settled: true,
                needs_degraded_reconcile: false,
                exhausted: false,
                next_delay_ms: 0,
                next_consecutive_clean: next_clean,
            };
        }
        return HideRetryStep {
            stop_timer: false,
            settled: false,
            needs_degraded_reconcile: false,
            exhausted: false,
            next_delay_ms,
            next_consecutive_clean: next_clean,
        };
    }

    // A visible tray resets the stability streak; only a successful hide at the
    // budget boundary counts as settled.
    if attempt >= max_attempts {
        let settled = observation == HideObservation::HidVisible;
        return HideRetryStep {
            stop_timer: true,
            settled,
            needs_degraded_reconcile: false,
            exhausted: !settled,
            next_delay_ms: 0,
            next_consecutive_clean: 0,
        };
    }

    HideRetryStep {
        stop_timer: false,
        settled: false,
        needs_degraded_reconcile: false,
        exhausted: false,
        next_delay_ms,
        next_consecutive_clean: 0,
    }
}

pub fn should_re_register_taskbar(
    is_registered: bool,
    registered_hwnd: usize,
    registered_pid: u32,
    current_hwnd: usize,
    current_pid: u32,
) -> bool {
    if !is_registered {
        return true;
    }
    if registered_hwnd == 0 || current_hwnd == 0 {
        return false;
    }
    registered_hwnd != current_hwnd || registered_pid != current_pid
}

/// Work-area strip for a vertical (left/right) dock, expressed in physical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SideReservation {
    pub edge: i32,
    pub coordinate: i32,
    pub width: i32,
}

pub fn compute_side_reservation(edge: i32, screen_width: i32, dock_width: i32) -> SideReservation {
    if dock_width <= 0 {
        return SideReservation::default();
    }
    SideReservation {
        edge,
        coordinate: if edge == 1 {
            0
        } else {
            (screen_width - dock_width).max(0)
        },
        width: dock_width,
    }
}

/// Verifies that Explorer's taskbar really came back, with three bounded
/// `SW_SHOW` retries after the first state/visibility pass (same as the C
/// watchdog). The journal is only cleared by the caller when this returns true.
pub fn verify_restored(tray: usize, attempts: u32) -> bool {
    let tray = tray as HWND;
    for attempt in 0..attempts.max(1) {
        // SAFETY: querying/revealing a validated Explorer taskbar window.
        unsafe {
            if IsWindowVisible(tray) != 0 {
                return true;
            }
            ShowWindow(tray, SW_SHOW);
        }
        for secondary in native_tray::find_secondary_leases() {
            if !secondary.is_valid() {
                continue;
            }
            // SAFETY: secondary tray was lease-validated immediately above.
            unsafe {
                ShowWindow(secondary.hwnd as HWND, SW_SHOW);
            }
        }
        if attempt + 1 < attempts {
            std::thread::sleep(Duration::from_millis(HIDE_RETRY_SETTLE_SLEEP_MS));
        }
    }
    // SAFETY: final liveness/visibility check.
    unsafe { IsWindowVisible(tray) != 0 }
}

/// One bounded recovery attempt: re-assert AppBar state, enforce hiding and
/// return whether the shell answered. Pure from the caller's perspective, so the
/// UI loop can drive it on a timer without blocking.
pub fn reconcile_once(
    dock_hwnd: usize,
    auto_hide: bool,
    desired_total_px: i32,
    reason: &str,
) -> bool {
    let start = std::time::Instant::now();
    let Some(tray) = native_tray::try_acquire() else {
        log::result("Reconcile", reason, false, false, &[]);
        return false;
    };
    let previous = crate::journal::read_state().unwrap_or_else(taskbar::get_taskbar_state);
    let mut state_data = appbar::create_data(tray.hwnd);
    state_data.lParam = previous as isize;
    appbar::send(appbar::ABM_SETSTATE, &mut state_data, reason);
    if auto_hide {
        taskbar::set_taskbar_state(previous | 0x1);
    } else if dock_hwnd != 0 && desired_total_px > 0 {
        taskbar::register_bottom_appbar(dock_hwnd, desired_total_px, previous);
    }
    taskbar::ensure_hidden();
    log::result(
        "Reconcile",
        reason,
        true,
        false,
        &[("ms", log::elapsed_ms(start).to_string())],
    );
    true
}

// ---- WinEvent shell hooks -------------------------------------------------------

const EVENT_OBJECT_CREATE: u32 = 0x8000;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;

type WinEventProc = unsafe extern "system" fn(HANDLE, u32, HWND, i32, i32, u32, u32);

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWinEventHook(
        event_min: u32,
        event_max: u32,
        hmod: HMODULE,
        callback: Option<WinEventProc>,
        id_process: u32,
        id_thread: u32,
        flags: u32,
    ) -> HANDLE;
    fn UnhookWinEvent(hook: HANDLE) -> i32;
}

static SHELL_CHANGE_CALLBACK: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static HOOKS_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe extern "system" fn on_shell_event(
    _hook: HANDLE,
    event: u32,
    hwnd: HWND,
    object_id: i32,
    _child_id: i32,
    _thread: u32,
    _time: u32,
) {
    if object_id != 0
        || hwnd.is_null()
        || (event != EVENT_OBJECT_CREATE && event != EVENT_OBJECT_SHOW)
    {
        return;
    }
    let mut class_buffer = [0u16; 64];
    // SAFETY: bounded UTF-16 buffer on a live window from the OS hook.
    let length =
        unsafe { GetClassNameW(hwnd, class_buffer.as_mut_ptr(), class_buffer.len() as i32) };
    if length <= 0 {
        return;
    }
    let class = String::from_utf16_lossy(&class_buffer[..length as usize]);
    if !class.eq_ignore_ascii_case("Shell_TrayWnd")
        && !class.eq_ignore_ascii_case("Shell_SecondaryTrayWnd")
    {
        return;
    }
    let callback = SHELL_CHANGE_CALLBACK.load(std::sync::atomic::Ordering::Acquire);
    if !callback.is_null() {
        // SAFETY: the pointer was installed by `install_shell_hooks` and stays
        // valid for the process lifetime.
        unsafe { std::mem::transmute::<*mut (), fn()>(callback)() };
    }
}

/// Installs an out-of-context WinEvent watcher for `Shell_TrayWnd` creation and
/// show events on a dedicated message-pump thread. The callback must be cheap
/// and non-blocking: it normally just queues a bounded reconcile pass.
pub fn install_shell_hooks(on_shell_change: fn()) {
    SHELL_CHANGE_CALLBACK.store(
        on_shell_change as *mut (),
        std::sync::atomic::Ordering::Release,
    );
    if HOOKS_INSTALLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("opentaskbar-shell-events".into())
        .spawn(|| {
            // SAFETY: out-of-context hook; the callback body never panics and
            // only invokes the installed fn pointer.
            let hook = unsafe {
                SetWinEventHook(
                    EVENT_OBJECT_CREATE,
                    EVENT_OBJECT_SHOW,
                    std::ptr::null_mut(),
                    Some(on_shell_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if hook.is_null() {
                return;
            }
            let mut message: MSG = unsafe { std::mem::zeroed() };
            // SAFETY: owned MSG on this thread's message queue.
            while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
                // SAFETY: message was retrieved from our own queue.
                unsafe {
                    DispatchMessageW(&message);
                }
            }
            // SAFETY: the hook handle is owned by this thread.
            unsafe {
                UnhookWinEvent(hook);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degraded_budget_covers_45_attempts_then_exhausts() {
        for attempt in 1..=45 {
            let decision = evaluate_degraded_reconcile_retry(attempt);
            assert!(decision.should_retry, "attempt {attempt}");
            assert!(!decision.exhausted);
        }
        let exhausted = evaluate_degraded_reconcile_retry(46);
        assert!(!exhausted.should_retry);
        assert!(exhausted.exhausted);
        assert_eq!(evaluate_degraded_reconcile_retry(1).delay_ms, 500);
        assert_eq!(evaluate_degraded_reconcile_retry(11).delay_ms, 1000);
        assert_eq!(evaluate_degraded_reconcile_retry(26).delay_ms, 2000);
    }

    #[test]
    fn hide_retry_requires_consecutive_clean_passes() {
        let first = evaluate_hide_retry_step(HideObservation::AlreadyHidden, 1, 0, false);
        assert!(!first.stop_timer);
        assert_eq!(first.next_consecutive_clean, 1);
        let second = evaluate_hide_retry_step(HideObservation::AlreadyHidden, 2, 1, false);
        assert!(second.stop_timer && second.settled);

        let startup = evaluate_hide_retry_step(HideObservation::AlreadyHidden, 1, 8, true);
        assert!(!startup.stop_timer);
        let startup = evaluate_hide_retry_step(HideObservation::AlreadyHidden, 2, 9, true);
        assert!(startup.stop_timer && startup.settled);

        let reset = evaluate_hide_retry_step(HideObservation::StillVisible, 3, 9, true);
        assert_eq!(reset.next_consecutive_clean, 0);
        assert!(!reset.settled);

        let missing = evaluate_hide_retry_step(HideObservation::MissingTaskbar, 1, 0, false);
        assert!(missing.needs_degraded_reconcile && missing.stop_timer);
    }

    #[test]
    fn side_reservation_pins_the_requested_edge() {
        assert_eq!(
            compute_side_reservation(1, 1920, 72),
            SideReservation {
                edge: 1,
                coordinate: 0,
                width: 72,
            }
        );
        assert_eq!(compute_side_reservation(2, 1920, 72).coordinate, 1848);
        assert_eq!(
            compute_side_reservation(2, 1920, 0),
            SideReservation::default()
        );
    }

    #[test]
    fn taskbar_re_registration_detects_replaced_window_or_pid() {
        assert!(should_re_register_taskbar(false, 0, 0, 1, 2));
        assert!(!should_re_register_taskbar(true, 1, 2, 1, 2));
        assert!(should_re_register_taskbar(true, 1, 2, 1, 3));
        assert!(!should_re_register_taskbar(true, 0, 2, 1, 3));
    }
}
