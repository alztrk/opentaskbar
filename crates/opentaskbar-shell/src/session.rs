//! Pure, bounded state-machine logic for taskbar replacement/recovery.
//!
//! Mirrors the important constants and decisions of reference
//! `TaskbarReplacementSession`: hide retries (20 normal / 40 startup-settle
//! attempts with 2 / 10 consecutive clean passes), a 45-attempt degraded
//! reconcile budget, `TaskbarCreated` reconciliation, side-dock work-area
//! reservation, and verified restore requiring two consecutive clean passes.
//! The pure decisions stay unit-testable; this module also owns the bounded
//! production drivers so the budgets are not left as dead constants.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HANDLE, HMODULE, HWND};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetClassNameW, GetMessageW, IsWindowVisible, MSG, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, SW_SHOW, ShowWindow, WM_QUIT,
};

use opentaskbar_core::log;

use crate::{appbar, native_tray, taskbar};

pub const MAX_DEGRADED_RECONCILE_ATTEMPTS: u32 = 45;
pub const NORMAL_HIDE_REQUIRED_CLEAN_PASSES: u32 = 2;
pub const STARTUP_SETTLE_REQUIRED_CLEAN_PASSES: u32 = 10;
pub const NORMAL_HIDE_MAX_ATTEMPTS: u32 = 20;
pub const STARTUP_SETTLE_MAX_ATTEMPTS: u32 = 40;
pub const RESTORE_ATTEMPTS: u32 = 3;
pub const RESTORE_REQUIRED_CLEAN_PASSES: u32 = 2;
pub const RECONCILE_PASS_COUNT: u32 = 2;
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

/// Pure restore-streak update. `RestoreWithVerification` only declares
/// success after two consecutive clean passes (a transient re-hide resets it).
#[must_use]
pub const fn restore_observation(visible: bool, consecutive: u32) -> (u32, bool) {
    let next = if visible { consecutive + 1 } else { 0 };
    (next, next >= RESTORE_REQUIRED_CLEAN_PASSES)
}

/// Reveals the tray and asks for verification in one bounded pass. This is the
/// shared engine for [`verify_restored`] and the restore loop in `taskbar`.
/// `restore_once` re-applies AppBar state/geometry before each observation.
pub fn verify_restored_with(tray: usize, attempts: u32, mut restore_once: impl FnMut()) -> bool {
    let tray = tray as HWND;
    let total = attempts.max(1);
    let mut consecutive = 0u32;
    for attempt in 0..total {
        restore_once();
        // SAFETY: querying/revealing a validated Explorer taskbar window.
        unsafe {
            if IsWindowVisible(tray) == 0 {
                ShowWindow(tray, SW_SHOW);
            }
        }
        let mut secondaries_ok = true;
        for secondary in native_tray::find_secondary_leases() {
            if !secondary.is_valid() {
                continue;
            }
            // SAFETY: secondary tray was lease-validated immediately above.
            unsafe {
                ShowWindow(secondary.hwnd as HWND, SW_SHOW);
            }
            secondaries_ok &= unsafe { IsWindowVisible(secondary.hwnd as HWND) != 0 };
        }
        // SAFETY: checking the windows we just revealed (reference IsNativeTaskbarVisible).
        let visible = secondaries_ok && unsafe { IsWindowVisible(tray) != 0 };
        let (next, verified) = restore_observation(visible, consecutive);
        consecutive = next;
        if verified {
            return true;
        }
        // Never sleep after the last round: shutdown must not pay a 60 ms tax
        // for a pass that cannot change the answer.
        if attempt + 1 < total {
            std::thread::sleep(Duration::from_millis(HIDE_RETRY_SETTLE_SLEEP_MS));
        }
    }
    false
}

/// Verifies that Explorer's taskbar really came back. Two consecutive visible
/// observations are required before the caller may clear the journal.
pub fn verify_restored(tray: usize, attempts: u32) -> bool {
    verify_restored_with(tray, attempts, || {})
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
    // After teardown a late TaskbarCreated / hook pass must not re-register
    // the AppBar or hide the taskbar that shutdown just restored.
    if !replacement_active() {
        return false;
    }
    if native_tray::try_acquire().is_none() {
        log::result("Reconcile", reason, false, false, &[]);
        return false;
    };
    let previous = taskbar::active_state();
    // Verified state publication (GetState -> SETSTATE -> GetState) replaces the
    // old blind send so a failed Explorer reply cannot silently leave auto-hide off.
    let _ = appbar::set_and_verify_state(previous, &format!("{reason}/SetState"));
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

// ---- Bounded production drivers --------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ReplacementConfig {
    dock_hwnd: usize,
    auto_hide: bool,
    desired_total_px: i32,
}

#[derive(Debug)]
struct RuntimeState {
    config: ReplacementConfig,
    hide_attempts: u32,
    hide_clean: u32,
    hide_stable_settle: bool,
    degraded_attempts: u32,
    /// Identity of the AppBar registration bound to the previous Explorer tray.
    registered_hwnd: usize,
    registered_pid: u32,
}

impl RuntimeState {
    const fn new() -> Self {
        Self {
            config: ReplacementConfig {
                dock_hwnd: 0,
                auto_hide: false,
                desired_total_px: 0,
            },
            hide_attempts: 0,
            hide_clean: 0,
            hide_stable_settle: false,
            degraded_attempts: 0,
            registered_hwnd: 0,
            registered_pid: 0,
        }
    }
}

static RUNTIME: Mutex<RuntimeState> = Mutex::new(RuntimeState::new());
static HIDE_RETRY_ACTIVE: AtomicBool = AtomicBool::new(false);
static DEGRADED_ACTIVE: AtomicBool = AtomicBool::new(false);
/// True between `configure_replacement` (with a dock) and
/// `end_replacement_session`. Every hide path checks it, so nothing can hide
/// Explorer's taskbar again once shutdown has started restoring it.
static REPLACEMENT_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Slice used by the background workers so a teardown is observed quickly
/// instead of after a full 250-2000 ms backoff sleep.
const WORKER_SLEEP_SLICE_MS: u64 = 25;
/// Upper bound `end_replacement_session` waits for in-flight workers.
const WORKER_STOP_TIMEOUT_MS: u64 = 1500;

/// Whether a replacement session currently owns Explorer's taskbar.
pub fn replacement_active() -> bool {
    REPLACEMENT_ACTIVE.load(Ordering::SeqCst)
}

/// Sleeps up to `ms`, returning `false` as soon as the session is torn down.
fn sleep_while_active(ms: u64) -> bool {
    let mut remaining = ms;
    while remaining > 0 {
        if !replacement_active() {
            return false;
        }
        let slice = remaining.min(WORKER_SLEEP_SLICE_MS);
        std::thread::sleep(Duration::from_millis(slice));
        remaining -= slice;
    }
    replacement_active()
}

/// Tears the replacement session down before Explorer's taskbar is restored:
/// disarms every hide path, unhooks the WinEvent watcher and waits (bounded)
/// for the hide-retry / degraded-reconcile workers to leave. Without this the
/// `EVENT_OBJECT_SHOW` raised by the restore itself re-armed the hide timer
/// and hid the taskbar again right after shutdown. Idempotent.
pub fn end_replacement_session() {
    REPLACEMENT_ACTIVE.store(false, Ordering::SeqCst);
    {
        let mut state = runtime();
        state.config = ReplacementConfig::default();
        state.hide_attempts = 0;
        state.hide_clean = 0;
        state.hide_stable_settle = false;
        state.degraded_attempts = 0;
    }
    uninstall_shell_hooks();
    let deadline = std::time::Instant::now() + Duration::from_millis(WORKER_STOP_TIMEOUT_MS);
    while (HIDE_RETRY_ACTIVE.load(Ordering::Acquire)
        || DEGRADED_ACTIVE.load(Ordering::Acquire)
        || SHELL_HOOK_RUNNING.load(Ordering::Acquire))
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(WORKER_SLEEP_SLICE_MS));
    }
}

fn runtime() -> std::sync::MutexGuard<'static, RuntimeState> {
    RUNTIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Records the live dock geometry when replacement starts so the hook-driven
/// reconcile can re-assert it without app-crate wiring.
pub fn configure_replacement(dock_hwnd: usize, auto_hide: bool, desired_total_px: i32) {
    let mut state = runtime();
    state.config = ReplacementConfig {
        dock_hwnd,
        auto_hide,
        desired_total_px,
    };
    state.hide_attempts = 0;
    state.hide_clean = 0;
    state.degraded_attempts = 0;
    REPLACEMENT_ACTIVE.store(dock_hwnd != 0, Ordering::SeqCst);
}

/// Drops the HWND/PID bound to the *old* Explorer instance. A recycled HWND
/// must never be treated as a still-valid registration (`MarkUnregistered`).
pub fn clear_cached_taskbar_identity() -> (usize, u32) {
    let mut state = runtime();
    let identity = (state.registered_hwnd, state.registered_pid);
    state.registered_hwnd = 0;
    state.registered_pid = 0;
    identity
}

pub fn note_taskbar_identity(hwnd: usize, pid: u32) {
    let mut state = runtime();
    state.registered_hwnd = hwnd;
    state.registered_pid = pid;
}

/// Pure budget reset used by `TaskbarCreated`: one new shell instance means the
/// degraded budget and the hide streak both start over. The counts are ignored
/// by design, so they are not parameters.
#[must_use]
pub const fn reset_budgets() -> (u32, u32) {
    (0, 0)
}

/// `TaskbarCreated` / tray-show path: clear the caches tied to the old taskbar,
/// reset the 45-attempt degraded budget, then run the two bounded reconcile
/// passes reference uses so a secondary tray recreated mid-pass is caught too.
pub fn reconcile_after_taskbar_created() -> bool {
    if !replacement_active() {
        return false;
    }
    let config = {
        let mut state = runtime();
        let (degraded, hide) = reset_budgets();
        state.degraded_attempts = degraded;
        state.hide_attempts = hide;
        state.hide_clean = 0;
        state.hide_stable_settle = true;
        state.config
    };
    let _ = clear_cached_taskbar_identity();
    taskbar::clear_appbar_registration();
    let mut succeeded = false;
    for _ in 0..RECONCILE_PASS_COUNT {
        succeeded = reconcile_once(
            config.dock_hwnd,
            config.auto_hide,
            config.desired_total_px,
            "TaskbarCreated",
        ) || succeeded;
    }
    if let Some(tray) = native_tray::try_acquire() {
        note_taskbar_identity(tray.hwnd, tray.pid);
    }
    succeeded
}

/// Executes one hide-enforcement step against the live taskbar and advances the
/// 20/40-attempt budgets. Returns the decision so a timer can schedule the next
/// delay; `stop_timer` means the streak settled or the budget ended.
pub fn run_hide_retry_step() -> HideRetryStep {
    if !replacement_active() {
        return HideRetryStep {
            stop_timer: true,
            settled: false,
            needs_degraded_reconcile: false,
            exhausted: false,
            next_delay_ms: 0,
            next_consecutive_clean: 0,
        };
    }
    let (attempt, clean, stable) = {
        let state = runtime();
        (
            state.hide_attempts,
            state.hide_clean,
            state.hide_stable_settle,
        )
    };
    let step = evaluate_hide_retry_step(taskbar::hide_observation(), attempt, clean, stable);
    {
        let mut state = runtime();
        state.hide_clean = step.next_consecutive_clean;
        if step.stop_timer || step.needs_degraded_reconcile {
            state.hide_attempts = 0;
            state.hide_stable_settle = false;
        } else {
            state.hide_attempts = attempt + 1;
        }
    }
    if step.needs_degraded_reconcile {
        start_degraded_reconcile("HideRetry");
    }
    step
}

/// Starts the bounded hide timer (250/500 ms initial delay). Idempotent: a
/// second trigger while the worker is running only refreshes the config.
pub fn start_hide_retry(require_stable_settle: bool) {
    if !replacement_active() {
        return;
    }
    {
        let mut state = runtime();
        state.hide_attempts = 0;
        state.hide_clean = 0;
        state.hide_stable_settle = require_stable_settle;
    }
    if HIDE_RETRY_ACTIVE.swap(true, Ordering::SeqCst) {
        return;
    }
    if thread::Builder::new()
        .name("opentaskbar-hide-retry".into())
        .spawn(|| {
            let initial = hide_retry_initial_delay_ms(runtime().hide_stable_settle);
            let mut active = sleep_while_active(u64::from(initial));
            while active {
                let step = run_hide_retry_step();
                if step.stop_timer {
                    break;
                }
                active = sleep_while_active(u64::from(step.next_delay_ms));
            }
            HIDE_RETRY_ACTIVE.store(false, Ordering::Release);
        })
        .is_err()
    {
        HIDE_RETRY_ACTIVE.store(false, Ordering::Release);
    }
}

/// One degraded reconcile attempt, bounded at 45 tries. When exhausted the
/// worker stops (zero idle cost); the shell hooks remain armed and reset the
/// budget on a new `TaskbarCreated`.
pub fn run_degraded_reconcile_step() -> RetryDecision {
    let (attempt, config) = {
        let mut state = runtime();
        state.degraded_attempts += 1;
        (state.degraded_attempts, state.config)
    };
    let decision = evaluate_degraded_reconcile_retry(attempt);
    if decision.should_retry {
        let _ = reconcile_once(
            config.dock_hwnd,
            config.auto_hide,
            config.desired_total_px,
            "DegradedReconcile",
        );
    }
    decision
}

fn start_degraded_reconcile(_reason: &str) {
    if !replacement_active() {
        return;
    }
    if DEGRADED_ACTIVE.swap(true, Ordering::SeqCst) {
        return;
    }
    if thread::Builder::new()
        .name("opentaskbar-degraded-reconcile".into())
        .spawn(|| {
            while replacement_active() {
                let decision = run_degraded_reconcile_step();
                if !decision.should_retry || !sleep_while_active(u64::from(decision.delay_ms)) {
                    break;
                }
            }
            DEGRADED_ACTIVE.store(false, Ordering::Release);
        })
        .is_err()
    {
        DEGRADED_ACTIVE.store(false, Ordering::Release);
    }
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

/// Stored as a plain fn pointer in a mutex instead of a `*mut ()` + transmute:
/// a fn pointer is `Send`/`Copy`, needs no cast, and cannot dangle for the
/// lifetime of the process-owned hook.
static SHELL_CHANGE_CALLBACK: Mutex<Option<fn()>> = Mutex::new(None);
static HOOKS_INSTALLED: AtomicBool = AtomicBool::new(false);
static HOOK_INSTALL_PENDING: AtomicBool = AtomicBool::new(false);
/// True while the WinEvent pump thread is alive (until `UnhookWinEvent` returns).
static SHELL_HOOK_RUNNING: AtomicBool = AtomicBool::new(false);
/// Thread id of the WinEvent pump, so shutdown can post `WM_QUIT` to it.
static SHELL_HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
/// Set by `uninstall_shell_hooks` so a concurrent install stops before entering `GetMessageW`.
static HOOK_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Raw WinEvent handle. The pump thread installs it and unhooks it exactly
/// once when its message loop ends, so sharing it through a mutex is safe.
struct HookHandle(HANDLE);

// SAFETY: the handle is immutable once installed and closed only by the
// owning message-pump thread via `uninstall_win_event_hook`.
unsafe impl Send for HookHandle {}
unsafe impl Sync for HookHandle {}

static HOOK_HANDLE: Mutex<Option<HookHandle>> = Mutex::new(None);

/// Test-only seam for `SetWinEventHook` failure: the 20/40 hide budgets and the
/// 45-attempt degraded budget depend on this hook, so a failed installation
/// must leave `HOOKS_INSTALLED` false and remain retryable.
#[cfg(test)]
static FORCE_HOOK_FAILURE: AtomicBool = AtomicBool::new(false);

/// Real hook state, not a "we tried once" flag.
pub fn are_shell_hooks_installed() -> bool {
    HOOKS_INSTALLED.load(Ordering::SeqCst)
}

fn set_shell_change_callback(callback: Option<fn()>) {
    if let Ok(mut slot) = SHELL_CHANGE_CALLBACK.lock() {
        *slot = callback;
    }
}

/// Default reaction to a tray create/show event: a new Explorer instance needs
/// the full two-pass reconcile; a re-shown tray of the *same* instance only
/// needs the bounded hide-retry timer (20/40 attempts).
fn on_shell_tray_change() {
    // Before `configure_replacement` or after `end_replacement_session` there is
    // no active replacement session: do not arm any background budget or re-hide.
    if !replacement_active() || runtime().config.dock_hwnd == 0 {
        return;
    }
    let Some(tray) = native_tray::try_acquire() else {
        start_degraded_reconcile("ShellChange");
        return;
    };
    let replaced = {
        let state = runtime();
        should_re_register_taskbar(
            state.registered_hwnd != 0,
            state.registered_hwnd,
            state.registered_pid,
            tray.hwnd,
            tray.pid,
        )
    };
    if replaced {
        note_taskbar_identity(tray.hwnd, tray.pid);
        let _ = reconcile_after_taskbar_created();
    } else {
        let stable = runtime().hide_stable_settle;
        start_hide_retry(stable);
    }
}

/// Invokes the built-in reconcile driver and the app callback, containing any
/// panic: unwinding across an `extern "system"` WinEvent callback is undefined
/// behaviour and would abort the process, so the hook must swallow it.
fn invoke_shell_change_callback() {
    let _ = std::panic::catch_unwind(on_shell_tray_change);
    let callback = SHELL_CHANGE_CALLBACK.lock().ok().and_then(|slot| *slot);
    if let Some(callback) = callback {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback));
    }
}

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
    invoke_shell_change_callback();
}

/// Registers the OS hook and publishes `HOOKS_INSTALLED` only when
/// `SetWinEventHook` actually returned a handle. Kept separate from the message
/// pump so the failure path can be exercised directly by unit tests.
fn install_win_event_hook() -> bool {
    #[cfg(test)]
    if FORCE_HOOK_FAILURE.load(Ordering::SeqCst) {
        log::warn("Injected SetWinEventHook failure; shell hooks not installed.");
        return false;
    }
    // SAFETY: out-of-context hook; `on_shell_event` contains any callback panic
    // with `catch_unwind` before it can cross the FFI boundary.
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
        log::warn("SetWinEventHook failed; Explorer-restart reconcile is disabled for now.");
        return false;
    }
    if let Ok(mut slot) = HOOK_HANDLE.lock() {
        *slot = Some(HookHandle(hook));
    }
    HOOKS_INSTALLED.store(true, Ordering::SeqCst);
    true
}

fn uninstall_win_event_hook() {
    HOOKS_INSTALLED.store(false, Ordering::SeqCst);
    let handle = HOOK_HANDLE.lock().ok().and_then(|mut slot| slot.take());
    if let Some(hook) = handle {
        // SAFETY: the hook handle is owned by this thread.
        unsafe {
            UnhookWinEvent(hook.0);
        }
    }
}

/// Installs an out-of-context WinEvent watcher for `Shell_TrayWnd` creation and
/// show events on a dedicated message-pump thread. The callback must be cheap
/// and non-blocking: it normally just queues a bounded reconcile pass.
pub fn install_shell_hooks(on_shell_change: fn()) {
    set_shell_change_callback(Some(on_shell_change));
    if HOOKS_INSTALLED.load(Ordering::SeqCst) || HOOK_INSTALL_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    HOOK_STOP_REQUESTED.store(false, Ordering::SeqCst);
    SHELL_HOOK_RUNNING.store(true, Ordering::SeqCst);
    let spawned = std::thread::Builder::new()
        .name("opentaskbar-shell-events".into())
        .spawn(|| {
            struct RunningGuard;
            impl Drop for RunningGuard {
                fn drop(&mut self) {
                    SHELL_HOOK_THREAD.store(0, Ordering::SeqCst);
                    SHELL_HOOK_RUNNING.store(false, Ordering::Release);
                    HOOK_INSTALL_PENDING.store(false, Ordering::Release);
                }
            }
            let _running = RunningGuard;
            let mut message: MSG = unsafe { std::mem::zeroed() };
            // Force OS message queue creation before publishing our thread ID
            // so `PostThreadMessageW(WM_QUIT)` from `uninstall_shell_hooks`
            // never races a queue-less thread.
            // SAFETY: owned MSG on this thread's message queue.
            unsafe {
                PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
            }
            // SAFETY: plain thread-id query.
            SHELL_HOOK_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
            if HOOK_STOP_REQUESTED.load(Ordering::SeqCst) || !install_win_event_hook() {
                return;
            }
            HOOK_INSTALL_PENDING.store(false, Ordering::Release);
            // A teardown that raced the hook installation is observed here.
            // SAFETY: owned MSG on this thread's message queue.
            while HOOKS_INSTALLED.load(Ordering::SeqCst)
                && !HOOK_STOP_REQUESTED.load(Ordering::SeqCst)
                && unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0
            {
                // SAFETY: message was retrieved from our own queue.
                unsafe {
                    DispatchMessageW(&message);
                }
            }
            uninstall_win_event_hook();
        });
    if let Err(err) = spawned {
        log::warn(format!(
            "Shell-event hook thread could not be started: {err}"
        ));
        HOOK_INSTALL_PENDING.store(false, Ordering::Release);
        SHELL_HOOK_RUNNING.store(false, Ordering::Release);
    }
}

/// Stops the WinEvent watcher installed by [`install_shell_hooks`]: clears the
/// app callback and posts `WM_QUIT` to the pump thread, which unhooks itself.
pub fn uninstall_shell_hooks() {
    set_shell_change_callback(None);
    HOOK_STOP_REQUESTED.store(true, Ordering::SeqCst);
    let was_installed = HOOKS_INSTALLED.swap(false, Ordering::SeqCst);
    let thread_id = SHELL_HOOK_THREAD.swap(0, Ordering::SeqCst);
    if thread_id != 0 {
        // SAFETY: posting WM_QUIT to our own pump thread; failure (no queue
        // yet) is covered by the HOOK_STOP_REQUESTED / HOOKS_INSTALLED checks.
        unsafe {
            PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
        }
    } else if !was_installed {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime config/identity cache is process-wide; tests that touch it
    /// must not overlap with the shell-hook callback test.
    static TEST_SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner())
    }

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

    static CALLBACK_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn counting_callback() {
        CALLBACK_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn panicking_callback() {
        panic!("shell callback must never unwind across the WinEvent boundary");
    }

    #[test]
    fn shell_hook_install_failure_keeps_the_flag_false_and_retryable() {
        let _serial = serial();
        FORCE_HOOK_FAILURE.store(true, Ordering::SeqCst);
        assert!(!install_win_event_hook());
        assert!(
            !are_shell_hooks_installed(),
            "a failed SetWinEventHook must not be reported as installed"
        );
        FORCE_HOOK_FAILURE.store(false, Ordering::SeqCst);
    }

    #[test]
    fn shell_hook_callback_runs_and_its_panic_is_contained() {
        let _serial = serial();
        CALLBACK_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);

        set_shell_change_callback(Some(counting_callback));
        invoke_shell_change_callback();
        invoke_shell_change_callback();
        assert_eq!(CALLBACK_CALLS.load(std::sync::atomic::Ordering::SeqCst), 2);

        // The callback is stored as Option<fn()>; None disables it.
        set_shell_change_callback(None);
        invoke_shell_change_callback();
        assert_eq!(CALLBACK_CALLS.load(std::sync::atomic::Ordering::SeqCst), 2);

        // A panicking callback must be swallowed by catch_unwind, not abort.
        set_shell_change_callback(Some(panicking_callback));
        let result = std::panic::catch_unwind(invoke_shell_change_callback);
        assert!(result.is_ok(), "callback panic escaped catch_unwind");
        set_shell_change_callback(None);
    }

    #[test]
    fn restore_verification_needs_two_consecutive_clean_passes() {
        assert_eq!(restore_observation(false, 0), (0, false));
        assert_eq!(restore_observation(true, 0), (1, false));
        assert_eq!(restore_observation(true, 1), (2, true));
        // A re-hide between two passes resets the streak.
        assert_eq!(restore_observation(false, 1), (0, false));
        assert_eq!(restore_observation(true, 0), (1, false));
        assert_eq!(RESTORE_REQUIRED_CLEAN_PASSES, 2);
    }

    #[test]
    fn taskbar_created_resets_both_budgets_and_identity_cache() {
        let _serial = serial();
        assert_eq!(reset_budgets(), (0, 0));
        configure_replacement(0x1234, true, 72);
        note_taskbar_identity(0xABCD, 4242);
        assert_eq!(clear_cached_taskbar_identity(), (0xABCD, 4242));
        assert_eq!(clear_cached_taskbar_identity(), (0, 0));
        assert_eq!(
            runtime().config,
            ReplacementConfig {
                dock_hwnd: 0x1234,
                auto_hide: true,
                desired_total_px: 72,
            }
        );
        // Leave no dock configured for other tests / the shipped process.
        configure_replacement(0, false, 0);
    }

    #[test]
    fn hide_budgets_match_the_reference_session() {
        assert_eq!(NORMAL_HIDE_MAX_ATTEMPTS, 20);
        assert_eq!(STARTUP_SETTLE_MAX_ATTEMPTS, 40);
        assert_eq!(MAX_DEGRADED_RECONCILE_ATTEMPTS, 45);
        assert_eq!(RECONCILE_PASS_COUNT, 2);
        assert_eq!(hide_retry_initial_delay_ms(false), 250);
        assert_eq!(hide_retry_initial_delay_ms(true), 500);
        let exhausted = evaluate_hide_retry_step(
            HideObservation::StillVisible,
            STARTUP_SETTLE_MAX_ATTEMPTS,
            0,
            true,
        );
        assert!(exhausted.stop_timer && exhausted.exhausted && !exhausted.settled);
    }

    #[test]
    fn end_replacement_session_disarms_all_hide_and_reconcile_paths() {
        let _serial = serial();
        configure_replacement(0x1234, false, 64);
        assert!(replacement_active());

        end_replacement_session();
        assert!(!replacement_active());
        assert_eq!(runtime().config, ReplacementConfig::default());

        // Every hide/reconcile entry point must be a no-op once torn down.
        assert!(!reconcile_once(0x1234, false, 64, "LateHook"));
        assert!(!reconcile_after_taskbar_created());
        let step = run_hide_retry_step();
        assert!(step.stop_timer);
        assert!(!step.needs_degraded_reconcile);
        start_hide_retry(true);
        assert!(!HIDE_RETRY_ACTIVE.load(Ordering::Acquire));

        // Repeated teardown is idempotent.
        end_replacement_session();
        assert!(!replacement_active());
    }
}
