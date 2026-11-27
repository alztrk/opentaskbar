//! Tracks whether the Windows Start menu is open. A closed Start menu often
//! stays the foreground window but is cloaked by DWM, so the open state follows
//! its `EVENT_OBJECT_CLOAKED` / `EVENT_OBJECT_UNCLOAKED` events as well as
//! `EVENT_SYSTEM_FOREGROUND`.
//!
//! Windows 10 hosts Start inside `ShellExperienceHost.exe` as a
//! `Windows.UI.Core.CoreWindow` with a localized "Start" title, while Windows 11
//! uses `StartMenuExperienceHost.exe`. Both are recognized; the Win10 path is
//! validated by class + title so other shell flyouts do not latch the state.
//!
//! A Start button press within a [-60 ms, +150 ms] grace around the cloak counts
//! as dismissing the menu rather than reopening it (`WasOpenAtPress` in reference).
//! The cloak is authoritative: it stamps `closed_at` even if the cached state
//! was already closed, so repeated cloak events cannot erase the grace window.
//! A genuine cloak also clears the shell's anchored flag instead of relying on
//! the worker liveness monitor alone. Foreground changes never create the grace
//! stamp, so Alt+Tab / dock activation cannot fake a "Start just closed" signal.

use std::collections::HashMap;
use std::panic::catch_unwind;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND, LPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    IsWindow, IsWindowVisible,
};

use opentaskbar_core::log;

const EVENT_OBJECT_CLOAKED: u32 = 0x8017;
const EVENT_OBJECT_UNCLOAKED: u32 = 0x8018;
const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
const OBJID_WINDOW: i32 = 0;
const START_MENU_EXE: &str = "startmenuexperiencehost.exe";
const SHELL_HOST_EXE: &str = "shellexperiencehost.exe";
const CORE_WINDOW_CLASS: &str = "windows.ui.core.corewindow";

/// A press and its click belong to the same gesture for at most this long.
const PRESS_GESTURE_MAX: Duration = Duration::from_millis(1500);
const GRACE_BEFORE_MS: i128 = -60;
const GRACE_AFTER_MS: i128 = 150;

/// Localized Start titles on Windows 10's CoreWindow. Lowercase so a single
/// `to_lowercase()` comparison covers case-insensitive matches, including the
/// Turkish `Başlat` and its ASCII-only `Baslat` variant.
const START_TITLES: [&str; 24] = [
    "start",
    "başlat",
    "baslat",
    "inicio",
    "iniciar",
    "démarrer",
    "demarrer",
    "startseite",
    "início",
    "avvio",
    "starta",
    "käynnistä",
    "nabídka start",
    "пуск",
    "старт",
    "スタート",
    "시작",
    "开始",
    "開始",
    "έναρξη",
    "התחלה",
    "ابدأ",
    "mulai",
    "bắt đầu",
];

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

/// Which Start host owns a window. Cached per PID because the executable path
/// is stable, while the class/title check must stay live for ShellExperienceHost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum HostKind {
    #[default]
    None,
    StartExperience,
    ShellExperience,
}

#[derive(Default)]
struct Inner {
    is_open: bool,
    closed_at: Option<Instant>,
    pressed_at: Option<Instant>,
    start_window: usize,
    /// The window a real CLOAKED event dismissed. A cloaked handle is normally
    /// still alive, so it must be excluded from live "open" checks until an
    /// UNCLOAKED event clears it — otherwise a genuine close would toggle
    /// straight back open on the next press.
    closed_window: usize,
}

static STATE: Mutex<Inner> = Mutex::new(Inner {
    is_open: false,
    closed_at: None,
    pressed_at: None,
    start_window: 0,
    closed_window: 0,
});
static CLOAK_HOOK: AtomicIsize = AtomicIsize::new(0);
static FOREGROUND_HOOK: AtomicIsize = AtomicIsize::new(0);
static INSTALL: Mutex<()> = Mutex::new(());
static ON_OPENED: Mutex<Option<fn(usize)>> = Mutex::new(None);

/// Host identity is stable per PID, so the expensive process query is cached
/// for a short window. `is_open_now()` walks host windows from a WinEvent hook,
/// where a repeated `OpenProcess` / `QueryFullProcessImageNameW` per window
/// would dominate cost. The TTL also ensures a recycled PID is re-queried.
static HOST_CACHE: LazyLock<Mutex<HashMap<u32, (HostKind, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const HOST_CACHE_MAX: usize = 256;
const HOST_CACHE_TTL: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Pure helpers (kept side-effect free so the state rules are testable).
// ---------------------------------------------------------------------------

/// Compares the executable by its file name, never by a bare path suffix.
fn exe_file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn host_kind_from_exe(path: &str) -> HostKind {
    match exe_file_name(path).to_ascii_lowercase().as_str() {
        START_MENU_EXE => HostKind::StartExperience,
        SHELL_HOST_EXE => HostKind::ShellExperience,
        _ => HostKind::None,
    }
}

fn class_is_core_window(class: &str) -> bool {
    class.trim().eq_ignore_ascii_case(CORE_WINDOW_CLASS)
}

fn title_is_start(title: &str) -> bool {
    let title = title.trim();
    if title.is_empty() {
        return false;
    }
    let lower = title.to_lowercase();
    START_TITLES.iter().any(|candidate| lower == *candidate)
}

/// Windows 10 fallback: ShellExperienceHost hosts many shell surfaces, so a
/// window only counts as Start when it is a CoreWindow with a localized title.
fn is_win10_start_window(class: &str, title: &str) -> bool {
    class_is_core_window(class) && title_is_start(title)
}

fn opened_state() -> (bool, Option<Instant>) {
    (true, None)
}

fn closed_state(now: Instant) -> (bool, Option<Instant>) {
    (false, Some(now))
}

/// Cloak is authoritative, but another genuinely visible Start window on the
/// same host process means the menu is still open through that window.
/// `on_closed` runs only for a genuine dismissal: that is where the shell's
/// anchored flag is cleared, alongside stamping the grace window.
fn cloak_transition_with(
    now: Instant,
    another_visible: bool,
    on_closed: impl FnOnce(),
) -> (bool, Option<Instant>) {
    if another_visible {
        opened_state()
    } else {
        on_closed();
        closed_state(now)
    }
}

fn cloak_transition(now: Instant, another_visible: bool) -> (bool, Option<Instant>) {
    cloak_transition_with(now, another_visible, || {})
}

/// Foreground loss and dock activation are not dismissals: refresh the cached
/// open flag while preserving (never creating) the cloak grace stamp.
fn foreground_closed_state(previous: Option<Instant>) -> (bool, Option<Instant>) {
    (false, previous)
}

/// Signed millisecond delta used by the press grace window.
pub fn closed_delta_ms(closed_at: Instant, press: Instant) -> i128 {
    if closed_at >= press {
        (closed_at - press).as_millis() as i128
    } else {
        -((press - closed_at).as_millis() as i128)
    }
}

fn within_grace(closed_at: Instant, press: Instant) -> bool {
    (GRACE_BEFORE_MS..=GRACE_AFTER_MS).contains(&closed_delta_ms(closed_at, press))
}

/// Uses the recorded button-down time while the press/click gesture is fresh;
/// otherwise treats the supplied instant as the press itself.
fn resolve_press_at(pressed_at: Option<Instant>, now: Instant) -> Instant {
    match pressed_at {
        Some(press) if press <= now && now.duration_since(press) <= PRESS_GESTURE_MAX => press,
        _ => now,
    }
}

fn was_open_at_press_impl(
    is_open: bool,
    open_now: bool,
    closed_at: Option<Instant>,
    press: Instant,
) -> bool {
    if is_open || open_now {
        return true;
    }
    closed_at.is_some_and(|closed| within_grace(closed, press))
}

/// Pure TTL check. Future timestamps saturate to fresh rather than panicking.
#[must_use]
fn host_cache_entry_fresh(sampled_at: Instant, now: Instant) -> bool {
    now.duration_since(sampled_at) < HOST_CACHE_TTL
}

/// A cloaked Start window is still open when it belongs to a host process and
/// has not been dismissed by a real CLOAKED event. Alt+Tab hides the flyout via
/// DWM cloaking without closing it, so filtering cloaked windows out broke the
/// Start-button toggle after a switcher session.
#[must_use]
fn window_counts_as_open(
    hwnd: usize,
    dismissed_hwnd: usize,
    alive: bool,
    visible: bool,
    cloaked: bool,
    include_cloaked: bool,
    host_is_start: bool,
) -> bool {
    hwnd != 0
        && hwnd != dismissed_hwnd
        && alive
        && visible
        && host_is_start
        && (!cloaked || include_cloaked)
}

/// Dock activation and foreground changes never enumerate windows: they either
/// keep a genuinely open Start flag or clear it while preserving the cloak
/// grace stamp.
#[must_use]
fn refresh_without_enumeration(
    cached_open: bool,
    closed_at: Option<Instant>,
) -> (bool, Option<Instant>) {
    if cached_open {
        opened_state()
    } else {
        foreground_closed_state(closed_at)
    }
}

// ---------------------------------------------------------------------------
// Win32 inspection.
// ---------------------------------------------------------------------------

fn process_exe(pid: u32) -> Option<String> {
    // SAFETY: limited query handle closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        (ok != 0 && len > 0).then(|| String::from_utf16_lossy(&buffer[..len as usize]))
    }
}

/// Returns the cached host kind for `pid`, calling `lookup` only when no fresh
/// entry exists. `now` is injected so the 2 s TTL is testable without sleeping.
/// A poisoned mutex degrades to a cache miss (fail-closed, as before).
fn cached_host_kind_with(
    pid: u32,
    now: Instant,
    lookup: impl FnOnce(u32) -> Option<String>,
) -> HostKind {
    if pid == 0 {
        return HostKind::None;
    }
    if let Ok(cache) = HOST_CACHE.lock()
        && let Some(&(cached, sampled_at)) = cache.get(&pid)
        && host_cache_entry_fresh(sampled_at, now)
    {
        return cached;
    }
    let kind = lookup(pid)
        .as_deref()
        .map(host_kind_from_exe)
        .unwrap_or(HostKind::None);
    if let Ok(mut cache) = HOST_CACHE.lock() {
        // Long sessions recycle PIDs; cap the cache instead of growing forever
        // and drop entries that have expired.
        cache.retain(|_, (_, sampled_at)| host_cache_entry_fresh(*sampled_at, now));
        if cache.len() >= HOST_CACHE_MAX {
            cache.clear();
        }
        cache.insert(pid, (kind, now));
    }
    kind
}

fn host_kind_for_pid(pid: u32) -> HostKind {
    cached_host_kind_with(pid, Instant::now(), process_exe)
}

/// True when `pid` belongs to a Start / Shell host process. Exposed for
/// `win32_ex::is_shell_overlay`, which must not treat every CoreWindow as Start.
pub fn is_start_host_pid(pid: u32) -> bool {
    host_kind_for_pid(pid) != HostKind::None
}

/// True when an executable path names a Start / Shell host (exact file name,
/// not a bare path suffix). Works with the exe path already held by callers.
pub fn is_start_host_exe(exe_path: &str) -> bool {
    host_kind_from_exe(exe_path) != HostKind::None
}

fn window_class(hwnd: HWND) -> String {
    let mut buffer = [0u16; 128];
    // SAFETY: bounded UTF-16 buffer on a live window handle.
    let length = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..(length as usize).min(buffer.len())])
}

fn window_title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: bounded UTF-16 buffer on a live window handle.
    let length = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..(length as usize).min(buffer.len())])
}

fn is_start_menu_process(hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: querying the PID of a live window supplied by the OS.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return false;
    }
    match host_kind_for_pid(pid) {
        HostKind::None => false,
        HostKind::StartExperience => true,
        // Win10: the host also owns Action Center and other flyouts.
        HostKind::ShellExperience => {
            is_win10_start_window(&window_class(hwnd), &window_title(hwnd))
        }
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: reading one DWORD from a live window.
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    hr == 0 && cloaked != 0
}

fn is_on_screen(hwnd: HWND) -> bool {
    // SAFETY: visibility query and DWM attribute read on a live window.
    unsafe { IsWindowVisible(hwnd) != 0 && !is_cloaked(hwnd) }
}

struct EnumContext {
    windows: Vec<usize>,
    pid: Option<u32>,
    dismissed: usize,
    include_cloaked: bool,
}

/// Enumerates Start host windows. The strict form (`include_cloaked = false`)
/// is used by the CLOAKED hook to decide whether another *visible* Start window
/// keeps the flyout open; the live form includes cloaked-but-live windows and
/// excludes the one an actual CLOAKED event dismissed. No minimum rect is
/// applied: Win10 emits uncloak events with a tiny rect during animation.
fn find_start_windows_with(
    pid: Option<u32>,
    include_cloaked: bool,
    dismissed: usize,
) -> Vec<usize> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        unsafe {
            // SAFETY: `lparam` points at a context alive for the synchronous enumeration.
            let context = &mut *(lparam as *mut EnumContext);
            if let Some(expected) = context.pid {
                let mut window_pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut window_pid);
                if window_pid != expected {
                    return 1;
                }
            }
            let visible = IsWindowVisible(hwnd) != 0;
            if window_counts_as_open(
                hwnd as usize,
                context.dismissed,
                IsWindow(hwnd) != 0,
                visible,
                is_cloaked(hwnd),
                context.include_cloaked,
                is_start_menu_process(hwnd),
            ) {
                context.windows.push(hwnd as usize);
            }
            1
        }
    }

    let mut context = EnumContext {
        windows: Vec::new(),
        pid,
        dismissed,
        include_cloaked,
    };
    // SAFETY: synchronous enumeration with an owned, live context.
    unsafe {
        EnumWindows(Some(visit), (&mut context as *mut EnumContext) as LPARAM);
    }
    context.windows
}

/// Strict, on-screen-only enumeration used by the CLOAKED hook.
fn find_visible_start_windows(pid: Option<u32>) -> Vec<usize> {
    find_start_windows_with(pid, false, 0)
}

/// Live query across all host processes: cloaked-but-live Start windows count
/// as open (Alt+Tab), while a genuinely dismissed handle does not.
pub fn find_start_menu_windows() -> Vec<usize> {
    let dismissed = STATE.lock().map(|state| state.closed_window).unwrap_or(0);
    find_start_windows_with(None, true, dismissed)
}

/// Checks only the cached Start handle — no `EnumWindows`, so dock activation
/// can refresh the toggle state without a live scan on the UI thread.
fn cached_start_is_open() -> bool {
    let (hwnd, dismissed) = STATE
        .lock()
        .map(|state| (state.start_window, state.closed_window))
        .unwrap_or((0, 0));
    if hwnd == 0 {
        return false;
    }
    // SAFETY: revalidate the cached handle before using it as an open signal.
    let alive = unsafe { IsWindow(hwnd as HWND) != 0 };
    let visible = unsafe { IsWindowVisible(hwnd as HWND) != 0 };
    window_counts_as_open(
        hwnd,
        dismissed,
        alive,
        visible,
        is_cloaked(hwnd as HWND),
        true,
        is_start_menu_process(hwnd as HWND),
    )
}

// ---------------------------------------------------------------------------
// State transitions and public API.
// ---------------------------------------------------------------------------

fn mark_open() {
    if let Ok(mut state) = STATE.lock() {
        let (is_open, closed_at) = opened_state();
        state.is_open = is_open;
        state.closed_at = closed_at;
        // The same HWND is alive but cloaked after a genuine close; an
        // UNCLOAKED event is the only signal that clears that exclusion.
        state.closed_window = 0;
    }
}

/// Cloak is authoritative and always refreshes the dismissal timestamp, even
/// when the cached state was already closed. The CLOAKED hook uses
/// `cloak_transition_with` so it can clear the shell anchor in the same step.
pub fn mark_closed() {
    if let Ok(mut state) = STATE.lock() {
        let (is_open, closed_at) = cloak_transition(Instant::now(), false);
        state.is_open = is_open;
        state.closed_at = closed_at;
    }
}

/// Foreground moved away from Start: clear the cached flag only. No grace stamp
/// is created here, so a later press is not mistaken for a reopen.
fn mark_foreground_inactive() {
    if let Ok(mut state) = STATE.lock() {
        let (is_open, closed_at) = foreground_closed_state(state.closed_at);
        state.is_open = is_open;
        state.closed_at = closed_at;
    }
}

fn remember_start_window(hwnd: HWND) {
    if let Ok(mut state) = STATE.lock() {
        state.start_window = hwnd as usize;
    }
}

/// Registers the callback fired on UNCLOAKED (used to reposition the flyout).
pub fn on_opened(cb: fn(usize)) {
    if let Ok(mut callback) = ON_OPENED.lock() {
        *callback = Some(cb);
    }
}

fn notify_opened(hwnd: usize) {
    let callback = ON_OPENED.lock().ok().and_then(|slot| *slot);
    if let Some(callback) = callback {
        // Never let a managed callback panic across the WinEvent boundary.
        let _ = catch_unwind(|| callback(hwnd));
    }
}

unsafe extern "system" fn on_cloak_event(
    _hook: HANDLE,
    event_type: u32,
    hwnd: HWND,
    object_id: i32,
    _child_id: i32,
    _thread: u32,
    _time: u32,
) {
    unsafe {
        if object_id != OBJID_WINDOW || hwnd.is_null() || !is_start_menu_process(hwnd) {
            return;
        }
        if event_type == EVENT_OBJECT_UNCLOAKED {
            // Win10 animates from a tiny rect: accept the event on PID/class
            // identity alone instead of discarding it via a rect>100 filter.
            remember_start_window(hwnd);
            notify_opened(hwnd as usize);
            mark_open();
            return;
        }
        if event_type == EVENT_OBJECT_CLOAKED {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let others = find_visible_start_windows(Some(pid));
            let other = others
                .iter()
                .find(|&&candidate| candidate != hwnd as usize)
                .copied();
            // A real cloak clears the shell's anchored flag now; the worker
            // liveness monitor is only a fallback. A second visible Start
            // window means the flyout is still open and must stay anchored.
            let (is_open, closed_at) =
                cloak_transition_with(Instant::now(), other.is_some(), || {
                    opentaskbar_shell::flyout_reposition::mark_closed()
                });
            if let Some(hwnd) = other {
                remember_start_window(hwnd as HWND);
            }
            if let Ok(mut state) = STATE.lock() {
                state.is_open = is_open;
                state.closed_at = closed_at;
                // Exclude this cloaked handle from live open checks until it
                // uncloaks again, so a real dismissal still toggles correctly.
                state.closed_window = if is_open { 0 } else { hwnd as usize };
            }
        }
    }
}

unsafe extern "system" fn on_foreground_event(
    _hook: HANDLE,
    event_type: u32,
    hwnd: HWND,
    object_id: i32,
    _child_id: i32,
    _thread: u32,
    _time: u32,
) {
    if event_type != EVENT_SYSTEM_FOREGROUND || object_id != OBJID_WINDOW || hwnd.is_null() {
        return;
    }
    let is_start = is_start_menu_process(hwnd) && is_on_screen(hwnd);
    if is_start {
        remember_start_window(hwnd);
    }
    on_foreground_changed(is_start);
}

fn install_hook_handle(min: u32, max: u32, callback: WinEventProc, slot: &AtomicIsize) {
    if slot.load(Ordering::SeqCst) != 0 {
        return;
    }
    // SAFETY: out-of-context hook with a callback whose body never panics.
    let hook = unsafe {
        SetWinEventHook(
            min,
            max,
            std::ptr::null_mut(),
            Some(callback),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if !hook.is_null() {
        slot.store(hook as isize, Ordering::SeqCst);
    }
}

/// Starts following cloak and foreground events. Call on the UI thread once
/// its message loop runs. Idempotent.
pub fn install_hook() {
    let _guard = INSTALL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    install_hook_handle(
        EVENT_OBJECT_CLOAKED,
        EVENT_OBJECT_UNCLOAKED,
        on_cloak_event,
        &CLOAK_HOOK,
    );
    install_hook_handle(
        EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_FOREGROUND,
        on_foreground_event,
        &FOREGROUND_HOOK,
    );
    log::info("Start menu cloak and foreground hooks installed.");
}

fn unhook(slot: &AtomicIsize) {
    let hook = slot.swap(0, Ordering::SeqCst);
    if hook == 0 {
        return;
    }
    // SAFETY: the handle was created by `install_hook` and is removed once.
    unsafe {
        UnhookWinEvent(hook as HANDLE);
    }
}

/// Removes both hooks. Safe to call even when `install_hook` was never used.
pub fn uninstall_hook() {
    let _guard = INSTALL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unhook(&CLOAK_HOOK);
    unhook(&FOREGROUND_HOOK);
}

/// Cached state used by auto-hide and the Start button toggle.
pub fn is_open() -> bool {
    STATE.lock().is_ok_and(|state| state.is_open)
}

/// Live query: alias of `is_start_menu_open_now()`.
pub fn is_open_now() -> bool {
    is_start_menu_open_now()
}

/// Refreshes the cached state when the foreground window changes. A Start
/// foreground means the menu is open; losing focus does *not* mean it closed —
/// Alt+Tab and Task View leave the flyout open beneath the switcher. Validate
/// the cached window before flipping the flag, and never stamp `closed_at`
/// here: only a real CLOAKED event may create the dismissal grace window.
pub fn on_foreground_changed(is_start_menu: bool) {
    if is_start_menu {
        mark_open();
    } else if !is_start_menu_open_now() {
        mark_foreground_inactive();
    }
}

/// Hook point for WS-C's dock activation handler (right-click / focus). Like a
/// foreground change, this is not a dismissal: it refreshes the cached flag
/// without producing a false "Start just closed" grace signal. It uses only
/// the cached HWND — no live `EnumWindows` scan on the UI thread.
pub fn on_dock_activated() {
    let (was_open, closed_at) = STATE
        .lock()
        .map(|state| (state.is_open, state.closed_at))
        .unwrap_or((false, None));
    let (is_open, closed_at) =
        refresh_without_enumeration(was_open && cached_start_is_open(), closed_at);
    if let Ok(mut state) = STATE.lock() {
        state.is_open = is_open;
        state.closed_at = closed_at;
    }
}

/// Snapshots the Start button press time so a click can attribute a cloak that
/// happens just before or just after the press to the same gesture.
pub fn note_press(now: Instant) {
    if let Ok(mut state) = STATE.lock() {
        state.pressed_at = Some(now);
    }
}

/// True when a Start button press should close rather than reopen the menu.
pub fn was_open_at_press(now: Instant) -> bool {
    let (is_open, closed_at, pressed_at) = STATE
        .lock()
        .map(|state| (state.is_open, state.closed_at, state.pressed_at))
        .unwrap_or((false, None, None));
    let press = resolve_press_at(pressed_at, now);
    was_open_at_press_impl(is_open, is_open_now(), closed_at, press)
}

/// Back-compatible form of the `WasOpenAtPress(openNow, pressTimestamp)`
/// API, kept for callers that already supply a live open query.
pub fn was_open_at_press_with(open_now: bool, press: Instant) -> bool {
    let (is_open, closed_at) = STATE
        .lock()
        .map(|state| (state.is_open, state.closed_at))
        .unwrap_or((false, None));
    was_open_at_press_impl(is_open, open_now, closed_at, press)
}

fn reset_inner(state: &mut Inner) {
    state.is_open = false;
    state.closed_at = None;
    state.pressed_at = None;
    // A stale HWND must not be fed to close_start on the next toggle.
    state.start_window = 0;
    state.closed_window = 0;
}

pub fn reset() {
    if let Ok(mut state) = STATE.lock() {
        reset_inner(&mut state);
    }
}

/// Last known Start window handle (0 when none has been observed).
pub fn start_window() -> usize {
    STATE.lock().map(|state| state.start_window).unwrap_or(0)
}

/// True while the Start menu is still logically open. A cloaked-but-live host
/// window counts as open (Alt+Tab hides the flyout without dismissing it), but
/// a window explicitly dismissed by a CLOAKED event does not. The foreground
/// and cached-window checks run first; the full enumeration is only a fallback
/// for a Start window we have not seen yet.
pub fn is_start_menu_open_now() -> bool {
    // SAFETY: foreground window query.
    let foreground = unsafe { GetForegroundWindow() };
    if !foreground.is_null() && is_start_menu_process(foreground) && is_on_screen(foreground) {
        remember_start_window(foreground);
        return true;
    }
    if cached_start_is_open() {
        return true;
    }
    let windows = find_start_menu_windows();
    if let Some(&first) = windows.first() {
        remember_start_window(first as HWND);
    }
    !windows.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn press_grace_covers_dismissal_window_only() {
        let press = Instant::now();
        assert!(was_open_at_press_impl(true, false, None, press));
        assert!(was_open_at_press_impl(false, true, None, press));
        assert!(!was_open_at_press_impl(false, false, None, press));
        assert!(within_grace(press + Duration::from_millis(60), press));
        assert!(within_grace(press + Duration::from_millis(150), press));
        assert!(!within_grace(press + Duration::from_millis(151), press));
        assert!(within_grace(press - Duration::from_millis(60), press));
        assert!(!within_grace(press - Duration::from_millis(61), press));
        assert_eq!(
            closed_delta_ms(press + Duration::from_millis(10), press),
            10
        );
        assert_eq!(
            closed_delta_ms(press - Duration::from_millis(10), press),
            -10
        );
    }

    #[test]
    fn press_gesture_uses_note_time_only_while_fresh() {
        let down = Instant::now();
        let up = down + Duration::from_millis(120);
        assert_eq!(resolve_press_at(Some(down), up), down);
        assert_eq!(resolve_press_at(Some(down), down + PRESS_GESTURE_MAX), down);
        let stale_now = down + PRESS_GESTURE_MAX + Duration::from_millis(1);
        assert_eq!(resolve_press_at(Some(down), stale_now), stale_now);
        assert_eq!(resolve_press_at(None, up), up);
        // A press recorded after the queried instant cannot belong to it.
        assert_eq!(resolve_press_at(Some(up), down), down);
    }

    #[test]
    fn win10_fallback_requires_core_class_and_localized_title() {
        for title in [
            "Start",
            "START",
            "Başlat",
            "baslat",
            "Inicio",
            "Démarrer",
            "开始",
            "スタート",
            "Пуск",
            "시작",
        ] {
            assert!(
                is_win10_start_window("Windows.UI.Core.CoreWindow", title),
                "expected Start title: {title}"
            );
        }
        assert!(is_win10_start_window(
            "windows.ui.core.corewindow",
            "  Başlat  "
        ));
        assert!(!is_win10_start_window(
            "Windows.UI.Core.CoreWindow",
            "Search"
        ));
        assert!(!is_win10_start_window("Windows.UI.Core.CoreWindow", ""));
        assert!(!is_win10_start_window("Shell_TrayWnd", "Start"));
        assert!(!is_win10_start_window(
            "Windows.UI.Core.CoreWindow",
            "Restart"
        ));
        assert!(class_is_core_window("Windows.UI.Core.CoreWindow"));
        assert!(title_is_start("Başlat"));
    }

    #[test]
    fn start_host_exe_matching_is_path_based() {
        assert_eq!(
            host_kind_from_exe(
                r"C:\Windows\SystemApps\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\StartMenuExperienceHost.exe"
            ),
            HostKind::StartExperience
        );
        assert_eq!(
            host_kind_from_exe(
                r"C:\Windows\SystemApps\ShellExperienceHost_cw5n1h2txyewy\ShellExperienceHost.exe"
            ),
            HostKind::ShellExperience
        );
        assert_eq!(
            host_kind_from_exe(r"C:\Temp\STARTMENUEXPERIENCEHOST.EXE"),
            HostKind::StartExperience
        );
        assert_eq!(
            host_kind_from_exe(r"C:\Temp\evilstartmenuexperiencehost.exe"),
            HostKind::None
        );
        assert_eq!(host_kind_from_exe(r"C:\Temp\notepad.exe"), HostKind::None);
        assert_eq!(exe_file_name(r"C:\a\b\App.exe"), "App.exe");
    }

    #[test]
    fn cloak_state_machine_is_authoritative_and_stamps_every_close() {
        let now = Instant::now();

        // UNCLOAKED accepts a tiny rect and clears any pending grace.
        let (open, closed_at) = opened_state();
        assert!(open);
        assert_eq!(closed_at, None);

        // CLOAKED without another visible window closes and always stamps.
        let (open, closed_at) = cloak_transition(now, false);
        assert!(!open);
        assert_eq!(closed_at, Some(now));
        assert_eq!(
            closed_state(now + Duration::from_millis(20)).1,
            Some(now + Duration::from_millis(20))
        );
        assert!(was_open_at_press_impl(false, false, closed_at, now));

        // A repeated cloak on an already closed state must refresh the stamp.
        let refreshed = now + Duration::from_millis(40);
        let (open, closed_at) = cloak_transition(refreshed, false);
        assert!(!open);
        assert_eq!(closed_at, Some(refreshed));

        // A second visible Start window on the same PID keeps the menu open.
        let (open, closed_at) = cloak_transition(refreshed + Duration::from_millis(5), true);
        assert!(open);
        assert_eq!(closed_at, None);
        assert!(!was_open_at_press_impl(false, false, closed_at, refreshed));
    }

    #[test]
    fn cloak_clears_shell_anchor_only_on_genuine_close() {
        use std::cell::Cell;
        let now = Instant::now();
        let shell_cleanups = Cell::new(0);

        let (open, closed_at) = cloak_transition_with(now, false, || {
            shell_cleanups.set(shell_cleanups.get() + 1);
        });
        assert!(!open);
        assert_eq!(closed_at, Some(now));
        assert_eq!(
            shell_cleanups.get(),
            1,
            "real cloak must clear shell anchor"
        );

        // A second visible Start window keeps the flyout open and anchored.
        let (open, closed_at) = cloak_transition_with(now, true, || {
            shell_cleanups.set(shell_cleanups.get() + 1);
        });
        assert!(open);
        assert_eq!(closed_at, None);
        assert_eq!(
            shell_cleanups.get(),
            1,
            "no anchor clear while Start is still visible"
        );
    }

    #[test]
    fn foreground_change_does_not_stamp_close_grace() {
        let cloak_at = Instant::now();
        let (_, cloak_stamp) = cloak_transition(cloak_at, false);
        let press = cloak_at + Duration::from_millis(40);

        // Losing foreground preserves the real cloak stamp: the press still
        // belongs to the dismissal instead of a freshly faked close.
        let (open, closed_at) = foreground_closed_state(cloak_stamp);
        assert!(!open);
        assert_eq!(closed_at, Some(cloak_at));
        assert!(was_open_at_press_impl(false, false, closed_at, press));

        // Without a real cloak there is no grace window at all.
        let (open, closed_at) = foreground_closed_state(None);
        assert!(!open);
        assert_eq!(closed_at, None);
        assert!(!was_open_at_press_impl(false, false, closed_at, press));
    }

    #[test]
    fn reset_clears_cached_start_window() {
        let now = Instant::now();
        let mut inner = Inner {
            is_open: true,
            closed_at: Some(now),
            pressed_at: Some(now),
            start_window: 0x1234,
            closed_window: 0x1234,
        };
        reset_inner(&mut inner);
        assert!(!inner.is_open);
        assert_eq!(inner.closed_at, None);
        assert_eq!(inner.pressed_at, None);
        assert_eq!(inner.start_window, 0);
        assert_eq!(inner.closed_window, 0);
    }

    #[test]
    fn start_host_exe_validation_rejects_arbitrary_uwp_apps() {
        assert!(is_start_host_exe(
            r"C:\Windows\SystemApps\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\StartMenuExperienceHost.exe"
        ));
        assert!(is_start_host_exe(
            r"C:\Windows\SystemApps\ShellExperienceHost_cw5n1h2txyewy\ShellExperienceHost.exe"
        ));
        assert!(!is_start_host_exe(
            r"C:\Windows\SystemApps\Microsoft.WindowsCalculator_8wekyb3d8bbwe\Calculator.exe"
        ));
        assert!(!is_start_host_exe(r"C:\Temp\notepad.exe"));
        assert_eq!(host_kind_for_pid(0), HostKind::None);
    }

    #[test]
    fn host_cache_expires_after_two_seconds() {
        use std::cell::Cell;
        assert_eq!(HOST_CACHE_TTL, Duration::from_secs(2));
        let pid = 4_000_000_123u32; // test-only PID, never a real host process
        let base = Instant::now();
        let calls = Cell::new(0);
        let exe = r"C:\Windows\SystemApps\StartMenuExperienceHost.exe".to_owned();

        assert_eq!(
            cached_host_kind_with(pid, base, |_| {
                calls.set(calls.get() + 1);
                Some(exe.clone())
            }),
            HostKind::StartExperience
        );
        assert_eq!(calls.get(), 1);

        // Still fresh: no OpenProcess / QueryFullProcessImageNameW on the hook.
        assert_eq!(
            cached_host_kind_with(pid, base + Duration::from_millis(1999), |_| {
                calls.set(calls.get() + 1);
                None
            }),
            HostKind::StartExperience
        );
        assert_eq!(calls.get(), 1);

        // Expired: one fresh lookup, and the failure is cached as None too.
        let expired = base + HOST_CACHE_TTL;
        assert_eq!(
            cached_host_kind_with(pid, expired, |_| {
                calls.set(calls.get() + 1);
                None
            }),
            HostKind::None
        );
        assert_eq!(calls.get(), 2);
        assert_eq!(
            cached_host_kind_with(pid, expired + Duration::from_millis(1), |_| {
                calls.set(calls.get() + 1);
                Some(exe)
            }),
            HostKind::None
        );
        assert_eq!(calls.get(), 2);
        let _ = HOST_CACHE.lock().map(|mut cache| cache.remove(&pid));
    }

    #[test]
    fn cloaked_live_window_counts_as_open_until_dismissed() {
        let hwnd = 0x42usize;
        // On-screen Start counts in both the strict hook and the live query.
        assert!(window_counts_as_open(
            hwnd, 0, true, true, false, false, true
        ));
        assert!(window_counts_as_open(
            hwnd, 0, true, true, false, true, true
        ));
        // Cloaked but alive: only the live query counts it (Alt+Tab).
        assert!(!window_counts_as_open(
            hwnd, 0, true, true, true, false, true
        ));
        assert!(window_counts_as_open(hwnd, 0, true, true, true, true, true));
        // A real CLOAKED dismissal excludes that same handle from live checks.
        assert!(!window_counts_as_open(
            hwnd, hwnd, true, true, true, true, true
        ));
        // Dead / hidden / unrelated-process windows never count.
        assert!(!window_counts_as_open(
            hwnd, 0, false, true, true, true, true
        ));
        assert!(!window_counts_as_open(
            hwnd, 0, true, false, true, true, true
        ));
        assert!(!window_counts_as_open(
            hwnd, 0, true, true, true, true, false
        ));
        assert!(!window_counts_as_open(0, 0, true, true, true, true, true));
    }

    #[test]
    fn dock_activation_refresh_preserves_the_close_grace() {
        let cloak_at = Instant::now();
        let stamp = Some(cloak_at);
        // A still-open Start keeps the cached flag; no grace stamp is created.
        assert_eq!(refresh_without_enumeration(true, None), (true, None));
        // Losing Start without a cloak clears the flag but keeps the real stamp.
        let (open, closed_at) = refresh_without_enumeration(false, stamp);
        assert!(!open);
        assert_eq!(closed_at, stamp);
        assert!(was_open_at_press_impl(
            false,
            false,
            closed_at,
            cloak_at + Duration::from_millis(20)
        ));
    }

    #[test]
    fn was_open_at_press_public_api_stays_available_for_ws8() {
        let now = Instant::now();
        // Must remain callable from the dock without a live-query parameter.
        let _ = was_open_at_press(now);
        assert!(was_open_at_press_with(true, now));
        assert!(!was_open_at_press_with(false, now));
    }
}
