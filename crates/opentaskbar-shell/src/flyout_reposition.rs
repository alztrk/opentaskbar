//! Anchors the native Windows Start flyout to the dock's Start button on both
//! Windows 10 (a small `CoreWindow` popup) and Windows 11 (a monitor-sized
//! XAML canvas with the real flyout drawn inside it).
//!
//! The public surface is deliberately tiny so the app crate can drive it from
//! its cloak/foreground hooks:
//!
//! - [`find_start_window`] discovers the live Start window (PID first, class
//!   and localized title as a fallback),
//! - [`reposition_flyout`] schedules a bounded, generation-cancellable move
//!   loop for [`FlyoutKind::Start`], [`FlyoutKind::Search`] or
//!   [`FlyoutKind::ControlCenter`]; [`reposition_start`] is the Start wrapper,
//! - [`is_anchored`] reports whether the last Start loop actually succeeded and
//!   the window is still open, [`anchor_pending`] reports an in-flight attempt,
//! - [`close_start`] focuses the flyout and injects `VK_ESCAPE` (falling back to
//!   posted key messages when focus cannot be acquired), and
//!   [`open_start_via_syscommand`] posts `SC_TASKLIST` **without blocking**.
//!
//! Generation ownership is strict: `mark_opened()` is fired only by the
//! UNCLOAKED hook, *after* the new anchor worker has been scheduled, so a
//! verification step can never cancel that worker. Verification of a shell
//! open (and its `VK_LWIN` fallback) runs on a worker thread and never touches
//! the generation counter. A delayed close carries a token/HWND guard so it
//! cannot invalidate a newer open.
//!
//! `flyout_writer::remember()` is called with the *original* position before
//! the first move, so the native watchdog can restore every flyout on
//! shutdown.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use opentaskbar_core::log;
use opentaskbar_core::settings::{DockEdge, load_from, settings_path};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromRect,
};
use windows_sys::Win32::System::ProcessStatus::EnumProcesses;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput, VK_ESCAPE, VK_LWIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClassNameW, GetSystemMetrics, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW, SC_TASKLIST, SM_CXSCREEN,
    SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos,
    WM_KEYDOWN, WM_KEYUP, WM_SYSCOMMAND,
};

use crate::flyout_writer;

const START_MENU_EXE: &str = "startmenuexperiencehost.exe";
const SHELL_EXPERIENCE_EXE: &str = "shellexperiencehost.exe";
const CORE_WINDOW_CLASS: &str = "windows.ui.core.corewindow";
/// Localized names of the Start flyout/CoreWindow on Windows 10. Comparison is
/// case-insensitive and trims surrounding whitespace (`START_TITLES` itself is
/// kept in natural casing for the `FindWindowW` fallback).
const START_TITLES: [&str; 24] = [
    "Start",
    "Başlat",
    "Démarrer",
    "Inicio",
    "Iniciar",
    "Avvia",
    "Inizio",
    "Käynnistä",
    "Starten",
    "Пуск",
    "Старт",
    "Έναρξη",
    "התחלה",
    "ابدأ",
    "スタート",
    "시작",
    "开始",
    "開始",
    "เริ่ม",
    "Bắt đầu",
    "Mulai",
    "प्रारंभ",
    "شروع",
    "Štart",
];
const ATTEMPTS: usize = 40;
const ATTEMPT_DELAY_MS: u64 = 20;
const SETTLE_FRAMES: usize = 6;
const SETTLE_DELAY_MS: u64 = 25;
/// `RepositionSearchAsync`: 35 attempts x 20 ms, then 5 settle frames x 30 ms.
const SEARCH_ATTEMPTS: usize = 35;
const SEARCH_DELAY_MS: u64 = 20;
const SEARCH_SETTLE_FRAMES: usize = 5;
const SEARCH_SETTLE_DELAY_MS: u64 = 30;
/// `RepositionControlCenterAsync`: 24 attempts x 25 ms and one 75 ms re-assert.
const CONTROL_CENTER_ATTEMPTS: usize = 24;
const CONTROL_CENTER_DELAY_MS: u64 = 25;
const CONTROL_CENTER_REASSERT_MS: u64 = 75;
const SWP_FLAGS: u32 = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
const CONTROL_CENTER_CLASS: &str = "ControlCenterWindow";
const SEARCH_TITLES: [&str; 3] = ["Search", "Arama", "Ara"];
/// The `is_anchored` → `is_valid_hwnd` → `pid_is_start_host` chain is polled on
/// the dock's 200 ms tick. Reopening the process and reading its image path on
/// every tick is pure disk/process I/O on a hot path, so both successful and
/// failed lookups are memoized for a short window. An empty string is the
/// failure sentinel; a failed lookup keeps returning `false` just like the
/// uncached code, but no longer retries the process query on the next tick.
const PID_EXE_CACHE_TTL: Duration = Duration::from_secs(2);
static PID_EXE_CACHE: OnceLock<Mutex<HashMap<u32, (String, Instant)>>> = OnceLock::new();

/// Which native shell flyout a reposition worker owns. Each kind has its own
/// generation, mirroring the reference per-surface `CancellationTokenSource`: opening
/// Search must not cancel an in-flight Start worker (and vice versa).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlyoutKind {
    #[default]
    Start,
    Search,
    ControlCenter,
}

impl FlyoutKind {
    #[must_use]
    const fn index(self) -> usize {
        match self {
            Self::Start => 0,
            Self::Search => 1,
            Self::ControlCenter => 2,
        }
    }

    #[must_use]
    const fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Search => "Search",
            Self::ControlCenter => "ControlCenter",
        }
    }
}

/// Bumped on every new reposition request for a given flyout. Workers capture
/// it and abort as soon as a newer request (or a journal restore) supersedes
/// them. A single global counter would let one surface cancel another.
static GENERATION: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static ANCHORED: AtomicBool = AtomicBool::new(false);
static ANCHOR_VALID: AtomicBool = AtomicBool::new(false);
static ANCHORED_HWND: AtomicUsize = AtomicUsize::new(0);
/// True between scheduling a reposition attempt and its conclusion (success or
/// exhaustion). WS-C uses this to suppress the auto-hide fallback while an
/// attempt is still running.
static ANCHOR_ATTEMPT: AtomicBool = AtomicBool::new(false);
/// -1 = enabled by default, 0/1 = explicit override from the app settings flag.
/// Deliberately independent of `flyout_writer`'s (journal) flag.
static ENABLED_OVERRIDE: AtomicI8 = AtomicI8::new(-1);

/// Screen-space rectangle of the dock Start button (physical pixels).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Anchor {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Anchor {
    #[must_use]
    pub const fn width(self) -> i32 {
        self.right - self.left
    }

    #[must_use]
    pub const fn height(self) -> i32 {
        self.bottom - self.top
    }

    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.right > self.left && self.bottom > self.top
    }
}

// ---- Pure geometry / protocol helpers ------------------------------------------

fn clamp(value: i32, min: i32, max: i32) -> i32 {
    value.clamp(min, max)
}

fn scale_px(dips: i32, dpi: u32) -> i32 {
    ((dips as f64) * (dpi.max(1) as f64) / 96.0).round() as i32
}

/// True when the reported window is the monitor-sized Windows 11 Start canvas
/// rather than a discrete Windows 10 popup.
#[must_use]
pub fn is_full_screen_canvas(flyout_width: i32, work_width: i32) -> bool {
    work_width > 0 && flyout_width >= work_width - 100
}

/// Vertical placement for a flyout beside a left/right dock: track the Start
/// button near the top/bottom and center it in the middle third so it never
/// clips outside the monitor's work area.
#[must_use]
pub fn side_flyout_y(anchor: Anchor, flyout_height: i32, work: Anchor, dpi: u32) -> i32 {
    let margin = scale_px(8, dpi);
    let min_y = work.top + margin;
    let max_y = max(min_y, work.bottom - flyout_height - margin);
    let work_height = work.height().max(1);
    let center = (anchor.top + anchor.bottom) / 2;
    let relative = f64::from(center - work.top) / f64::from(work_height);
    let desired = if relative <= 0.33 {
        anchor.top
    } else if relative >= 0.67 {
        anchor.bottom - flyout_height
    } else {
        center - flyout_height / 2
    };
    clamp(desired, min_y, max_y)
}

/// Host-window origin that puts the flyout above/beside the dock Start button.
///
/// `work` is the monitor work area containing the anchor, in physical pixels;
/// `flyout_width`/`flyout_height` come from `GetWindowRect`. DPI-scaled 8 px
/// margins and 12 px gaps match the other dock flyouts. For the Windows 11
/// full-screen canvas the result compensates for the flyout's local offset
/// inside the canvas so the *visible panel*, not the canvas, lands on the
/// anchor.
#[must_use]
pub fn compute_target(
    anchor: Anchor,
    flyout_width: i32,
    flyout_height: i32,
    work: Anchor,
    edge: DockEdge,
    dpi: u32,
) -> (i32, i32) {
    compute_target_with_alignment(
        anchor,
        flyout_width,
        flyout_height,
        work,
        edge,
        dpi,
        taskbar_left_aligned(),
    )
}

/// Horizontal offset of the visible panel inside the Windows 11 monitor-sized
/// XAML canvas. Centered by default; `TaskbarAl = 0` (left-aligned taskbar)
/// places the panel one gap from the canvas edge instead.
#[must_use]
pub fn canvas_local_x(work_width: i32, panel_width: i32, gap: i32, left_aligned: bool) -> i32 {
    if left_aligned {
        gap
    } else {
        (work_width - panel_width) / 2
    }
}

fn compute_target_with_alignment(
    anchor: Anchor,
    flyout_width: i32,
    flyout_height: i32,
    work: Anchor,
    edge: DockEdge,
    dpi: u32,
    left_aligned: bool,
) -> (i32, i32) {
    let margin = scale_px(8, dpi);
    let gap = scale_px(12, dpi);
    // #185: Start is anchored from the center of its shell cell (the same
    // center the dock draws/hit-tests), never from the cell's leading edge.
    let anchor_center_x = (anchor.left + anchor.right) / 2;

    if is_full_screen_canvas(flyout_width, work.width()) {
        // UIA measurement is not available in the shell crate, so use the same
        // nominal flyout size as the reference parity implementation and subtract its
        // local offset inside the canvas.
        let panel_w = scale_px(642, dpi);
        let panel_h = scale_px(724, dpi);
        let local_x = canvas_local_x(work.width(), panel_w, gap, left_aligned);
        let local_y = work.height() - panel_h - gap;
        let slack = scale_px((720 - 642) / 2, dpi);
        let min_x = work.left + margin + (if edge == DockEdge::Bottom { slack } else { 0 });
        let max_x = max(min_x, work.right - panel_w - margin);
        let desired = match edge {
            DockEdge::Left => (
                clamp(anchor.right + gap, min_x, max_x),
                side_flyout_y(anchor, panel_h, work, dpi),
            ),
            DockEdge::Right => (
                clamp(anchor.left - panel_w - gap, min_x, max_x),
                side_flyout_y(anchor, panel_h, work, dpi),
            ),
            DockEdge::Bottom => (
                clamp(anchor_center_x - panel_w / 2 - margin, min_x, max_x),
                clamp(
                    anchor.top - panel_h - gap,
                    work.top + margin,
                    max(work.top + margin, work.bottom - panel_h - margin),
                ),
            ),
        };
        return (desired.0 - local_x, desired.1 - local_y);
    }

    let min_x = work.left + margin;
    let max_x = max(min_x, work.right - flyout_width - margin);
    let min_y = work.top + margin;
    let max_y = max(min_y, work.bottom - flyout_height - margin);
    match edge {
        DockEdge::Left => (
            clamp(anchor.right + gap, min_x, max_x),
            side_flyout_y(anchor, flyout_height, work, dpi),
        ),
        DockEdge::Right => (
            clamp(anchor.left - flyout_width - gap, min_x, max_x),
            side_flyout_y(anchor, flyout_height, work, dpi),
        ),
        DockEdge::Bottom => (
            clamp(anchor_center_x - flyout_width / 2 - margin, min_x, max_x),
            clamp(anchor.top - flyout_height - gap, min_y, max_y),
        ),
    }
}

fn max(a: i32, b: i32) -> i32 {
    a.max(b)
}

/// `ComputeSearchPosition` parity. Search uses the margin (not the larger
/// flyout gap) for its bottom-edge vertical offset and tracks the button's
/// horizontal position on a bottom dock.
#[must_use]
pub fn compute_search_target(
    anchor: Anchor,
    flyout_width: i32,
    flyout_height: i32,
    work: Anchor,
    edge: DockEdge,
    dpi: u32,
) -> (i32, i32) {
    let margin = scale_px(8, dpi);
    let gap = scale_px(12, dpi);
    let min_x = work.left + margin;
    let max_x = max(min_x, work.right - flyout_width - margin);
    let min_y = work.top + margin;
    let max_y = max(min_y, work.bottom - flyout_height - margin);
    match edge {
        DockEdge::Left => (
            clamp(anchor.right + gap, min_x, max_x),
            side_flyout_y(anchor, flyout_height, work, dpi),
        ),
        DockEdge::Right => (
            clamp(anchor.left - flyout_width - gap, min_x, max_x),
            side_flyout_y(anchor, flyout_height, work, dpi),
        ),
        DockEdge::Bottom => (
            clamp(anchor.left - margin, min_x, max_x),
            clamp(anchor.top - flyout_height - margin, min_y, max_y),
        ),
    }
}

/// `RepositionControlCenterAsync` parity: the panel is centered on the
/// anchor for a bottom dock, or centered vertically beside a side dock.
#[must_use]
pub fn compute_control_center_target(
    anchor: Anchor,
    flyout_width: i32,
    flyout_height: i32,
    work: Anchor,
    edge: DockEdge,
    dpi: u32,
) -> (i32, i32) {
    let margin = scale_px(8, dpi);
    let gap = scale_px(12, dpi);
    let min_x = work.left + margin;
    let max_x = max(min_x, work.right - flyout_width - margin);
    let min_y = work.top + margin;
    let max_y = max(min_y, work.bottom - flyout_height - margin);
    let center_x = (anchor.left + anchor.right) / 2;
    let center_y = (anchor.top + anchor.bottom) / 2;
    match edge {
        DockEdge::Left => (
            clamp(anchor.right + gap, min_x, max_x),
            clamp(center_y - flyout_height / 2, min_y, max_y),
        ),
        DockEdge::Right => (
            clamp(anchor.left - flyout_width - gap, min_x, max_x),
            clamp(center_y - flyout_height / 2, min_y, max_y),
        ),
        DockEdge::Bottom => (
            clamp(center_x - flyout_width / 2, min_x, max_x),
            clamp(anchor.top - flyout_height - gap, min_y, max_y),
        ),
    }
}

/// Geometry for a specific flyout kind. Start keeps the full-screen canvas
/// compensation; Search and Control Center use their own reference parity formulas.
#[must_use]
pub fn compute_flyout_target(
    kind: FlyoutKind,
    anchor: Anchor,
    width: i32,
    height: i32,
    work: Anchor,
    edge: DockEdge,
    dpi: u32,
) -> (i32, i32) {
    match kind {
        FlyoutKind::Start => compute_target(anchor, width, height, work, edge, dpi),
        FlyoutKind::Search => compute_search_target(anchor, width, height, work, edge, dpi),
        FlyoutKind::ControlCenter => {
            compute_control_center_target(anchor, width, height, work, edge, dpi)
        }
    }
}

/// Reads the Windows 11 taskbar alignment setting (`TaskbarAl` under
/// `HKCU\...\Explorer\Advanced`); `0` means left-aligned. Missing or malformed
/// values fall back to the centered default.
fn taskbar_left_aligned() -> bool {
    let path = wide(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");
    let name = wide("TaskbarAl");
    // SAFETY: fixed null-terminated registry paths and a correctly sized DWORD
    // buffer; the key handle is closed on every path.
    unsafe {
        let mut key = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return false;
        }
        let mut value = 0u32;
        let mut kind = 0u32;
        let mut size = size_of::<u32>() as u32;
        let read = RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            (&mut value as *mut u32).cast(),
            &mut size,
        ) == 0
            && size >= size_of::<u32>() as u32;
        RegCloseKey(key);
        read && value == 0
    }
}

/// `ESC` is delivered as a key-down/key-up pair, never as a `WM_CLOSE`: the
/// Start XAML island only handles the keyboard dismissal. These posted
/// messages are a best-effort fallback and are never reported as a verified
/// close on their own.
#[must_use]
pub fn escape_messages() -> [(u32, u32); 2] {
    [(WM_KEYDOWN, VK_ESCAPE as u32), (WM_KEYUP, VK_ESCAPE as u32)]
}

/// Injected `VK_ESCAPE` pair: physical scan code plus the extended-key flag,
/// which is what the Win10 `CoreWindow` XAML island listens for.
#[must_use]
pub fn escape_inputs(scan: u16) -> [KEYBDINPUT; 2] {
    [
        KEYBDINPUT {
            wVk: VK_ESCAPE,
            wScan: scan,
            dwFlags: KEYEVENTF_EXTENDEDKEY,
            time: 0,
            dwExtraInfo: 0,
        },
        KEYBDINPUT {
            wVk: VK_ESCAPE,
            wScan: scan,
            dwFlags: KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
            time: 0,
            dwExtraInfo: 0,
        },
    ]
}

fn keyboard_input(ki: KEYBDINPUT) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    }
}

/// Pure part of HWND validation; the Win32 part is [`is_valid_hwnd`].
#[must_use]
pub fn hwnd_is_usable(hwnd: usize, is_window: bool, visible: bool, cloaked: bool) -> bool {
    hwnd != 0 && is_window && visible && !cloaked
}

#[must_use]
fn token_is_current(token: u64, current: u64) -> bool {
    token == current
}

fn generation_for(kind: FlyoutKind) -> u64 {
    GENERATION[kind.index()].load(Ordering::Acquire)
}

/// Advances one flyout's generation and returns the freshly captured token.
fn bump_generation(kind: FlyoutKind) -> u64 {
    GENERATION[kind.index()].fetch_add(1, Ordering::AcqRel) + 1
}

/// A worker stops when a newer request for *its* flyout arrives or when
/// `flyout_writer` restores/clears the journal (its generation is bumped on
/// both paths).
fn cancelled(kind: FlyoutKind, token: u64, writer_token: u64) -> bool {
    !token_is_current(token, generation_for(kind)) || !flyout_writer::is_current(writer_token)
}

/// Publishes an anchor only if no newer request/journal restore superseded the
/// worker in the meantime. The final `cancelled` check happens immediately
/// before the state stores so a stale worker cannot win a race.
fn set_anchored(hwnd: usize, token: u64, writer_token: u64) -> bool {
    if cancelled(FlyoutKind::Start, token, writer_token) || !is_valid_hwnd(hwnd) {
        return false;
    }
    ANCHORED_HWND.store(hwnd, Ordering::Release);
    ANCHOR_VALID.store(true, Ordering::Release);
    ANCHORED.store(true, Ordering::Release);
    true
}

fn clear_anchor_state() {
    ANCHORED.store(false, Ordering::Release);
    ANCHOR_VALID.store(false, Ordering::Release);
    ANCHORED_HWND.store(0, Ordering::Release);
    ANCHOR_ATTEMPT.store(false, Ordering::Release);
}

/// The HWND + generation of the close currently in progress. Both fields are
/// guarded by one mutex so the token and the handle are always observed as a
/// pair: a new open clears the session, which makes a delayed close harmless.
static CLOSE_SESSION: Mutex<(u64, usize)> = Mutex::new((0, 0));

fn clear_close_session() {
    if let Ok(mut session) = CLOSE_SESSION.lock() {
        *session = (0, 0);
    }
}

/// Begins a close session. The generation bump invalidates a stale anchor
/// worker immediately; the returned pair is the token the caller must present
/// before it is allowed to call `mark_closed()`.
fn begin_close(hwnd: usize) -> (u64, usize) {
    let token = bump_generation(FlyoutKind::Start);
    let session = (token, hwnd);
    if let Ok(mut current) = CLOSE_SESSION.lock() {
        *current = session;
    }
    session
}

/// True while this close still owns the Start transition. A genuine open
/// (`mark_opened`) clears the session *and* advances the generation, so either
/// signal alone is enough to reject a stale close.
fn close_session_is_current(session: (u64, usize)) -> bool {
    let owner = CLOSE_SESSION
        .lock()
        .is_ok_and(|current| *current == session);
    owner && token_is_current(session.0, generation_for(FlyoutKind::Start))
}

/// Clears the session on drop, but only while it still belongs to this close.
struct CloseSessionGuard((u64, usize));

impl Drop for CloseSessionGuard {
    fn drop(&mut self) {
        if let Ok(mut current) = CLOSE_SESSION.lock()
            && *current == self.0
        {
            *current = (0, 0);
        }
    }
}

/// Invalidates a previous result without claiming a verified open. Used at the
/// start of an open trigger so the dock never sees a stale anchor while the
/// verification window is in progress. Returns the new generation so callers
/// can pass it to a verification worker.
fn invalidate_anchor() -> u64 {
    let token = bump_generation(FlyoutKind::Start);
    clear_anchor_state();
    token
}

/// Clears the anchored flag and cancels any pending close. WS-C calls this from
/// the CLOAKED hook for a genuine dismissal.
pub fn mark_closed() {
    let _ = invalidate_anchor();
    clear_close_session();
}

/// The **only** place that claims a verified open. Called from the UNCLOAKED
/// hook (after the new anchor has been scheduled), never from an open trigger
/// or its verification worker — that double bump used to cancel the fresh
/// worker. It also invalidates any close still in flight.
pub fn mark_opened() {
    let _ = invalidate_anchor();
    clear_close_session();
}

/// How ESC reaches the flyout. A focused window accepts global `SendInput`;
/// otherwise the key is posted directly to its HWND so injected input cannot
/// land in an unrelated foreground window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscDelivery {
    SendInput,
    Posted,
}

#[must_use]
pub fn esc_delivery(foreground_acquired: bool) -> EscDelivery {
    if foreground_acquired {
        EscDelivery::SendInput
    } else {
        EscDelivery::Posted
    }
}

/// The `VK_LWIN` fallback fires only when the shell post failed or the open
/// could not be verified. Pure so the worker policy is testable.
#[must_use]
pub fn open_fallback_needed(posted: bool, verified: bool) -> bool {
    !posted || !verified
}

/// True whether an anchor attempt is currently scheduled but not concluded.
#[must_use]
pub fn anchor_pending() -> bool {
    ANCHOR_ATTEMPT.load(Ordering::Acquire)
}

/// True only after a completed reposition whose window is still a live Start
/// window (PID + class/title validated, not merely any live HWND).
#[must_use]
pub fn is_anchored() -> bool {
    if !ANCHORED.load(Ordering::Acquire) || !ANCHOR_VALID.load(Ordering::Acquire) {
        return false;
    }
    let hwnd = ANCHORED_HWND.load(Ordering::Acquire);
    hwnd != 0 && is_valid_hwnd(hwnd)
}

/// Overrides the settings flag; `None` restores the default (**enabled**).
/// This is deliberately independent of `flyout_writer`'s journal flag.
pub fn set_enabled(value: Option<bool>) {
    ENABLED_OVERRIDE.store(
        match value {
            Some(true) => 1,
            Some(false) => 0,
            None => -1,
        },
        Ordering::Relaxed,
    );
}

#[must_use]
fn override_is_enabled(code: i8) -> bool {
    code != 0
}

#[must_use]
pub fn is_enabled() -> bool {
    override_is_enabled(ENABLED_OVERRIDE.load(Ordering::Relaxed))
}

// ---- Win32 helpers -------------------------------------------------------------

use opentaskbar_core::wide;

fn process_exe(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: a limited-query handle is opened and closed within this block.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        (ok == TRUE && len > 0).then(|| String::from_utf16_lossy(&buffer[..len as usize]))
    }
}

fn pid_exe_cache() -> &'static Mutex<HashMap<u32, (String, Instant)>> {
    PID_EXE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pure TTL check. `Instant::duration_since` saturates at zero, so a timestamp
/// from the future is treated as fresh rather than panicking.
#[must_use]
fn cache_entry_fresh(sampled_at: Instant, now: Instant, ttl: Duration) -> bool {
    now.duration_since(sampled_at) < ttl
}

#[must_use]
fn non_empty_exe(exe: &str) -> Option<String> {
    (!exe.is_empty()).then(|| exe.to_owned())
}

/// Returns the cached image path for `pid`, querying `lookup` only when no fresh
/// entry exists. `now` is injected so the TTL can be verified without sleeping
/// or depending on the wall clock; production always passes `Instant::now()`.
/// A poisoned mutex is treated as a cache miss (and simply bypasses caching),
/// preserving the old fail-closed behavior instead of propagating a panic.
fn cached_process_exe_with(
    pid: u32,
    now: Instant,
    lookup: impl FnOnce(u32) -> Option<String>,
) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let cache = pid_exe_cache();
    if let Ok(guard) = cache.lock()
        && let Some((exe, sampled_at)) = guard.get(&pid)
        && cache_entry_fresh(*sampled_at, now, PID_EXE_CACHE_TTL)
    {
        return non_empty_exe(exe);
    }
    // Failures are cached as an empty string too: a dead PID stays invalid for
    // this short window instead of reopening the process on every 200 ms tick.
    let exe = lookup(pid).unwrap_or_default();
    if let Ok(mut guard) = cache.lock() {
        guard.retain(|_, (_, at)| cache_entry_fresh(*at, now, PID_EXE_CACHE_TTL));
        guard.insert(pid, (exe.clone(), now));
    }
    non_empty_exe(&exe)
}

fn process_exe_cached(pid: u32) -> Option<String> {
    cached_process_exe_with(pid, Instant::now(), process_exe)
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: reading a single DWORD attribute from a live window handle.
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

fn is_window_live(hwnd: usize) -> bool {
    if hwnd == 0 {
        return false;
    }
    let handle = hwnd as HWND;
    // SAFETY: liveness/visibility queries and a DWM attribute read; all Win32
    // calls tolerate stale handles and return an error status instead.
    unsafe { IsWindow(handle) == TRUE && IsWindowVisible(handle) == TRUE && !is_cloaked(handle) }
}

/// Exact executable-name match (not a partial `ends_with`), case-insensitive.
/// Both the Windows 10/11 Start host and the Windows 11 ShellExperienceHost are
/// accepted; a lookalike such as `evil-startmenuexperiencehost.exe` is not.
#[must_use]
pub fn exe_is_start_host(exe: &str) -> bool {
    let name = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    let name = name.trim().to_ascii_lowercase();
    name == START_MENU_EXE || name == SHELL_EXPERIENCE_EXE
}

fn pid_is_start_host(pid: u32) -> bool {
    process_exe_cached(pid).is_some_and(|exe| exe_is_start_host(&exe))
}

#[must_use]
pub fn is_start_title(title: &str) -> bool {
    let needle = title.trim().to_lowercase();
    START_TITLES
        .iter()
        .any(|known| known.to_lowercase() == needle)
}

/// Validates a hint before moving it: non-null, live, visible, uncloaked and
/// owned by a Start host process. The PID check is mandatory on *every* branch,
/// including the localized CoreWindow title fallback (a cloak-event HWND may
/// be recycled by an unrelated process).
#[must_use]
pub fn is_valid_hwnd(hwnd: usize) -> bool {
    if !is_window_live(hwnd) {
        return false;
    }
    let handle = hwnd as HWND;
    // SAFETY: reading the PID and class of a window that just passed liveness.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(handle, &mut pid);
        if pid == 0 || !pid_is_start_host(pid) {
            return false;
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(handle, class.as_mut_ptr(), class.len() as i32);
        if len <= 0 {
            return false;
        }
        let class = String::from_utf16_lossy(&class[..len as usize]);
        if class.trim().to_lowercase() != CORE_WINDOW_CLASS {
            // A non-CoreWindow top-level window of the Start host (the Win11
            // XAML canvas) is still a valid move target.
            return true;
        }
        let mut title = [0u16; 128];
        let len = GetWindowTextW(handle, title.as_mut_ptr(), title.len() as i32);
        let title = String::from_utf16_lossy(&title[..len.max(0) as usize]);
        is_start_title(&title)
    }
}

fn window_rect(hwnd: usize) -> Option<RECT> {
    if hwnd == 0 {
        return None;
    }
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    // SAFETY: reading the rect of a live window handle; a stale handle fails.
    let ok = unsafe { GetWindowRect(hwnd as HWND, &mut rect) == TRUE };
    ok.then_some(rect)
}

fn rect_area(rect: RECT) -> i64 {
    i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top)
}

fn start_menu_pids() -> HashSet<u32> {
    let mut ids = vec![0u32; 1024];
    loop {
        let mut needed = 0u32;
        let capacity = ids.len();
        let bytes = capacity * size_of::<u32>();
        // SAFETY: `ids` is a properly sized u32 buffer for the whole loop.
        let ok = unsafe { EnumProcesses(ids.as_mut_ptr(), bytes as u32, &mut needed) };
        if ok != TRUE {
            return HashSet::new();
        }
        let count = (needed as usize / size_of::<u32>()).min(capacity);
        ids.truncate(count);
        if needed as usize <= bytes || capacity >= 16_384 {
            break;
        }
        ids.resize((capacity * 2).min(16_384), 0);
    }
    ids.into_iter()
        .filter(|&pid| pid_is_start_host(pid))
        .collect()
}

struct Discovery<'a> {
    pids: &'a HashSet<u32>,
    hwnd: usize,
    area: i64,
    is_core: bool,
}

/// Finds the real Start window. A cloaked window or a zero-size rect is never
/// accepted, but a tiny rect *is*: during the Windows 10 entrance animation
/// that small popup is the only real flyout we can move.
#[must_use]
pub fn find_start_window() -> Option<usize> {
    let pids = start_menu_pids();
    let mut discovery = Discovery {
        pids: &pids,
        hwnd: 0,
        area: 0,
        is_core: false,
    };
    // SAFETY: synchronous enumeration; the context outlives every callback.
    unsafe {
        EnumWindows(
            Some(visit_window),
            (&mut discovery as *mut Discovery<'_>) as LPARAM,
        );
    }
    if discovery.hwnd != 0 {
        return Some(discovery.hwnd);
    }
    find_start_window_by_title()
}

unsafe extern "system" fn visit_window(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: `lparam` carries a mutable reference alive for the enumeration.
    unsafe {
        let discovery = &mut *(lparam as *mut Discovery<'_>);
        if IsWindow(hwnd) != TRUE || IsWindowVisible(hwnd) != TRUE || is_cloaked(hwnd) {
            return 1;
        }
        let Some(rect) = window_rect(hwnd as usize) else {
            return 1;
        };
        let area = rect_area(rect);
        if area <= 0 {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if !discovery.pids.contains(&pid) {
            return 1;
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        let is_core = len > 0
            && String::from_utf16_lossy(&class[..len as usize])
                .eq_ignore_ascii_case(CORE_WINDOW_CLASS);
        if area > discovery.area || (area == discovery.area && is_core && !discovery.is_core) {
            discovery.hwnd = hwnd as usize;
            discovery.area = area;
            discovery.is_core = is_core;
        }
        1
    }
}

fn find_start_window_by_title() -> Option<usize> {
    let class = wide(CORE_WINDOW_CLASS);
    for title in START_TITLES {
        let name = wide(title);
        // SAFETY: null-terminated UTF-16 literals; the handle is validated next.
        let hwnd = unsafe { FindWindowW(class.as_ptr(), name.as_ptr()) };
        if hwnd.is_null() {
            continue;
        }
        // PID is mandatory here too: the title/class pair alone must never
        // authorize moving a recycled handle owned by another process.
        if is_valid_hwnd(hwnd as usize)
            && window_rect(hwnd as usize).is_some_and(|r| rect_area(r) > 0)
        {
            return Some(hwnd as usize);
        }
    }
    None
}

/// True when `hwnd` belongs to one of the exact host executable names. Unlike
/// a class/title match this cannot authorize moving a recycled handle owned by
/// an unrelated process.
fn window_belongs_to(hwnd: usize, exe_names: &[&str]) -> bool {
    let mut pid = 0u32;
    // SAFETY: querying the PID of a handle discovered in this enumeration pass.
    unsafe { GetWindowThreadProcessId(hwnd as HWND, &mut pid) };
    if pid == 0 {
        return false;
    }
    process_exe(pid).is_some_and(|exe| {
        let name = flyout_writer::exe_file_name(&exe)
            .trim()
            .to_ascii_lowercase();
        exe_names.iter().any(|candidate| name == *candidate)
    })
}

/// Windows 10 Search fallback (`FindLegacySearchWindows`): a localized
/// `Windows.UI.Core.CoreWindow` whose PID still belongs to a Search host.
fn find_search_window_by_title() -> Option<usize> {
    let class = wide(CORE_WINDOW_CLASS);
    for title in SEARCH_TITLES {
        let name = wide(title);
        // SAFETY: null-terminated UTF-16 literals; the handle is validated next.
        let hwnd = unsafe { FindWindowW(class.as_ptr(), name.as_ptr()) };
        if hwnd.is_null() || !is_window_live(hwnd as usize) {
            continue;
        }
        if window_belongs_to(hwnd as usize, flyout_writer::SEARCH_EXES)
            && window_rect(hwnd as usize).is_some_and(|r| rect_area(r) > 0)
        {
            return Some(hwnd as usize);
        }
    }
    None
}

/// Finds Search windows with the same PID check the journal writer uses. The
/// caller picks the largest rect (matching `OrderByDescending(area)`).
fn find_search_windows() -> Vec<usize> {
    let mut windows: Vec<usize> = flyout_writer::find_flyout_windows(flyout_writer::SEARCH_EXES)
        .into_iter()
        .map(|(hwnd, _)| hwnd)
        .collect();
    if windows.is_empty()
        && let Some(hwnd) = find_search_window_by_title()
    {
        windows.push(hwnd);
    }
    windows
}

/// Control Center is a specific window class; the PID check keeps a spoofed
/// class name from authorizing a move of an unrelated application.
fn find_control_center_window() -> Option<usize> {
    let class = wide(CONTROL_CENTER_CLASS);
    // SAFETY: a fixed null-terminated class name with a null window title.
    let hwnd = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
    if hwnd.is_null() || !is_window_live(hwnd as usize) {
        return None;
    }
    if !window_belongs_to(hwnd as usize, flyout_writer::CONTROL_CENTER_EXES) {
        return None;
    }
    window_rect(hwnd as usize)
        .is_some_and(|r| rect_area(r) > 0)
        .then_some(hwnd as usize)
}

fn work_area(anchor: Anchor) -> Anchor {
    let rect = RECT {
        left: anchor.left,
        top: anchor.top,
        right: anchor.right,
        bottom: anchor.bottom,
    };
    // SAFETY: `rect` is a valid input, `info` is sized correctly for the call.
    unsafe {
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !monitor.is_null()
            && GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()) == TRUE
        {
            let work = info.monitorInfo.rcWork;
            return Anchor {
                left: work.left,
                top: work.top,
                right: work.right,
                bottom: work.bottom,
            };
        }
        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);
        Anchor {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }
}

fn apply_position(hwnd: usize, x: i32, y: i32) -> bool {
    if hwnd == 0 {
        return false;
    }
    // SAFETY: position-only update with no resize, Z-order or focus change.
    unsafe { SetWindowPos(hwnd as HWND, std::ptr::null_mut(), x, y, 0, 0, SWP_FLAGS) == TRUE }
}

// ---- Public entry points -------------------------------------------------------

/// Schedules a native flyout reposition on a background worker for `kind`.
///
/// Returns `false` only when nothing could be scheduled (disabled feature,
/// invalid anchor or thread creation failure). The real result is reported
/// asynchronously via [`is_anchored`] for Start, so the UI thread never blocks
/// on shell animation frames. A new call cancels any in-flight worker for the
/// *same* flyout via its generation token, and journal restore cancels all.
pub fn reposition_flyout(kind: FlyoutKind, anchor: Anchor, hwnd_hint: usize) -> bool {
    if !is_enabled() {
        log::info(format!(
            "{}Anchor disabled by the native flyout flag.",
            kind.label()
        ));
        return false;
    }
    if !anchor.is_valid() {
        log::warn(format!(
            "{}Anchor rejected an empty anchor rect.",
            kind.label()
        ));
        if kind == FlyoutKind::Start {
            mark_closed();
        }
        return false;
    }
    let token = bump_generation(kind);
    if kind == FlyoutKind::Start {
        ANCHORED.store(false, Ordering::Release);
        ANCHOR_VALID.store(true, Ordering::Release);
        ANCHORED_HWND.store(0, Ordering::Release);
        ANCHOR_ATTEMPT.store(true, Ordering::Release);
    }
    let writer_token = flyout_writer::generation();
    let edge = load_from(&settings_path()).edge;
    let name = format!("opentaskbar-{}-anchor", kind.label().to_ascii_lowercase());
    let spawned = thread::Builder::new()
        .name(name)
        .spawn(move || flyout_worker(kind, anchor, hwnd_hint, edge, token, writer_token));
    if spawned.is_err() {
        log::error(format!(
            "{}Anchor could not spawn its reposition worker.",
            kind.label()
        ));
        if kind == FlyoutKind::Start {
            mark_closed();
        }
        return false;
    }
    log::info(format!(
        "{}Anchor scheduled [{},{}-{},{}], edge {edge:?}, hint {hwnd_hint:#x}.",
        kind.label(),
        anchor.left,
        anchor.top,
        anchor.right,
        anchor.bottom
    ));
    true
}

/// Back-compatible Start entry point. Kept for the app's UNCLOAKED callback.
pub fn reposition_start(anchor: Anchor, hwnd_hint: usize) -> bool {
    reposition_flyout(FlyoutKind::Start, anchor, hwnd_hint)
}

/// Clears the Start pending flag on every worker exit path, but never steals it
/// from a newer attempt (or from a close/open transition that bumped the token).
struct PendingGuard {
    token: u64,
    kind: FlyoutKind,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.kind == FlyoutKind::Start
            && token_is_current(self.token, generation_for(FlyoutKind::Start))
        {
            ANCHOR_ATTEMPT.store(false, Ordering::Release);
        }
    }
}

/// Journals a settled original rect. The second sample prevents an entrance
/// animation frame from being recorded; `flyout_writer::remember` enforces the
/// >= 100x100 size gate.
fn journal_original(hwnd: usize, pid: u32, canvas: bool, work: Anchor) {
    thread::sleep(Duration::from_millis(30));
    let Some(rect) = window_rect(hwnd) else {
        return;
    };
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let (left, top) = if canvas {
        (work.left, work.top)
    } else {
        (rect.left, rect.top)
    };
    flyout_writer::remember(hwnd, pid, left, top, width, height);
}

fn flyout_worker(
    kind: FlyoutKind,
    anchor: Anchor,
    hwnd_hint: usize,
    edge: DockEdge,
    token: u64,
    writer_token: u64,
) {
    let work = work_area(anchor);
    match kind {
        FlyoutKind::Start => start_worker(anchor, hwnd_hint, edge, token, writer_token, work),
        FlyoutKind::Search => search_worker(anchor, edge, token, writer_token, work),
        FlyoutKind::ControlCenter => control_center_worker(anchor, edge, token, writer_token, work),
    }
}

fn dpi_for(hwnd: usize) -> u32 {
    // SAFETY: DPI query on a handle validated by the caller.
    let dpi = unsafe { GetDpiForWindow(hwnd as HWND) };
    if dpi == 0 { 96 } else { dpi }
}

fn start_worker(
    anchor: Anchor,
    hwnd_hint: usize,
    edge: DockEdge,
    token: u64,
    writer_token: u64,
    work: Anchor,
) {
    let _pending = PendingGuard {
        token,
        kind: FlyoutKind::Start,
    };
    for _ in 0..ATTEMPTS {
        thread::sleep(Duration::from_millis(ATTEMPT_DELAY_MS));
        if cancelled(FlyoutKind::Start, token, writer_token) {
            return;
        }
        // A cloak-event HWND can be recycled; revalidate every round, then
        // fall back to PID + class/title discovery.
        let hwnd = if is_valid_hwnd(hwnd_hint) {
            hwnd_hint
        } else {
            let Some(found) = find_start_window() else {
                continue;
            };
            found
        };
        if !is_valid_hwnd(hwnd) {
            continue;
        }
        let Some(rect) = window_rect(hwnd) else {
            continue;
        };
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            continue;
        }
        let mut pid = 0u32;
        // SAFETY: reading the PID of a window validated just above.
        unsafe { GetWindowThreadProcessId(hwnd as HWND, &mut pid) };
        let canvas = is_full_screen_canvas(width, work.width());
        // Journal the *original* settled position before the first move. The
        // writer rejects rects smaller than 100x100 (animation frames).
        journal_original(hwnd, pid, canvas, work);

        let dpi = unsafe { GetDpiForWindow(hwnd as HWND) };
        let dpi = if dpi == 0 { 96 } else { dpi };
        let (x, y) = compute_target(anchor, width, height, work, edge, dpi);
        if cancelled(FlyoutKind::Start, token, writer_token) || !is_valid_hwnd(hwnd) {
            return;
        }
        if !apply_position(hwnd, x, y) {
            continue;
        }
        for _ in 0..SETTLE_FRAMES {
            thread::sleep(Duration::from_millis(SETTLE_DELAY_MS));
            if cancelled(FlyoutKind::Start, token, writer_token) || !is_valid_hwnd(hwnd) {
                return;
            }
            apply_position(hwnd, x, y);
        }
        // Explorer can re-assert its own Z-order/position right after the
        // entrance transition; one final move wins that race.
        if cancelled(FlyoutKind::Start, token, writer_token)
            || !is_valid_hwnd(hwnd)
            || !apply_position(hwnd, x, y)
        {
            return;
        }
        if !set_anchored(hwnd, token, writer_token) {
            return;
        }
        log::result(
            "StartAnchor",
            "anchored",
            true,
            true,
            &[
                ("hwnd", format!("{hwnd:#x}")),
                ("x", x.to_string()),
                ("y", y.to_string()),
            ],
        );
        monitor_while_open(hwnd, token, writer_token);
        return;
    }
    log::result("StartAnchor", "window-not-found", false, false, &[]);
    ANCHORED.store(false, Ordering::Release);
    ANCHOR_VALID.store(false, Ordering::Release);
    ANCHORED_HWND.store(0, Ordering::Release);
}

/// Keeps `is_anchored()` honest: once the Start flyout closes (or its handle is
/// recycled), the flag drops without needing a callback from the app crate.
fn monitor_while_open(hwnd: usize, token: u64, writer_token: u64) {
    let mut misses = 0u8;
    loop {
        thread::sleep(Duration::from_millis(100));
        if cancelled(FlyoutKind::Start, token, writer_token) {
            return;
        }
        if is_valid_hwnd(hwnd) {
            misses = 0;
        } else {
            misses += 1;
            if misses >= 2 {
                clear_anchor_state();
                log::info("StartAnchor cleared: Start window closed.");
                return;
            }
        }
    }
}

/// Picks the largest live window (the reference `OrderByDescending(area).First()`).
fn largest_window(candidates: Vec<usize>) -> Option<usize> {
    let mut best: Option<(usize, i64)> = None;
    for hwnd in candidates {
        if !is_window_live(hwnd) {
            continue;
        }
        let Some(rect) = window_rect(hwnd) else {
            continue;
        };
        let area = rect_area(rect);
        if area > 0 && best.is_none_or(|(_, previous)| area > previous) {
            best = Some((hwnd, area));
        }
    }
    best.map(|(hwnd, _)| hwnd)
}

fn window_pid(hwnd: usize) -> u32 {
    let mut pid = 0u32;
    // SAFETY: querying the PID of a handle validated by the caller.
    unsafe { GetWindowThreadProcessId(hwnd as HWND, &mut pid) };
    pid
}

/// `RepositionSearchAsync` parity: bounded discovery, journal the original
/// rect, re-assert across settle frames. Search uses its own generation, so it
/// never cancels a Start worker running at the same time.
fn search_worker(anchor: Anchor, edge: DockEdge, token: u64, writer_token: u64, work: Anchor) {
    for _ in 0..SEARCH_ATTEMPTS {
        thread::sleep(Duration::from_millis(SEARCH_DELAY_MS));
        if cancelled(FlyoutKind::Search, token, writer_token) {
            return;
        }
        let Some(hwnd) = largest_window(find_search_windows()) else {
            continue;
        };
        let Some(rect) = window_rect(hwnd) else {
            continue;
        };
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        // A search flyout smaller than this is an entrance animation frame.
        if width < 100 || height < 100 {
            continue;
        }
        journal_original(hwnd, window_pid(hwnd), false, work);
        let dpi = dpi_for(hwnd);
        let (x, y) =
            compute_flyout_target(FlyoutKind::Search, anchor, width, height, work, edge, dpi);
        if cancelled(FlyoutKind::Search, token, writer_token) || !is_window_live(hwnd) {
            return;
        }
        if !apply_position(hwnd, x, y) {
            continue;
        }
        for _ in 0..SEARCH_SETTLE_FRAMES {
            thread::sleep(Duration::from_millis(SEARCH_SETTLE_DELAY_MS));
            if cancelled(FlyoutKind::Search, token, writer_token) || !is_window_live(hwnd) {
                return;
            }
            apply_position(hwnd, x, y);
        }
        log::result(
            "SearchAnchor",
            "anchored",
            true,
            true,
            &[
                ("hwnd", format!("{hwnd:#x}")),
                ("x", x.to_string()),
                ("y", y.to_string()),
            ],
        );
        return;
    }
    log::result("SearchAnchor", "window-not-found", false, false, &[]);
}

/// `RepositionControlCenterAsync` parity: class + PID discovery, one
/// re-assert after the shell finishes its entrance animation, and the same
/// journal remember/restore path as the other flyouts.
fn control_center_worker(
    anchor: Anchor,
    edge: DockEdge,
    token: u64,
    writer_token: u64,
    work: Anchor,
) {
    for _ in 0..CONTROL_CENTER_ATTEMPTS {
        thread::sleep(Duration::from_millis(CONTROL_CENTER_DELAY_MS));
        if cancelled(FlyoutKind::ControlCenter, token, writer_token) {
            return;
        }
        let Some(hwnd) = find_control_center_window() else {
            continue;
        };
        let Some(rect) = window_rect(hwnd) else {
            continue;
        };
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width < 80 || height < 80 {
            continue;
        }
        journal_original(hwnd, window_pid(hwnd), false, work);
        let dpi = dpi_for(hwnd);
        let (x, y) = compute_flyout_target(
            FlyoutKind::ControlCenter,
            anchor,
            width,
            height,
            work,
            edge,
            dpi,
        );
        if cancelled(FlyoutKind::ControlCenter, token, writer_token)
            || !is_window_live(hwnd)
            || !apply_position(hwnd, x, y)
        {
            return;
        }
        // One real re-assert after the entrance animation (reference line: 75 ms).
        thread::sleep(Duration::from_millis(CONTROL_CENTER_REASSERT_MS));
        if cancelled(FlyoutKind::ControlCenter, token, writer_token) || !is_window_live(hwnd) {
            return;
        }
        apply_position(hwnd, x, y);
        log::result(
            "ControlCenterAnchor",
            "anchored",
            true,
            true,
            &[
                ("hwnd", format!("{hwnd:#x}")),
                ("x", x.to_string()),
                ("y", y.to_string()),
            ],
        );
        return;
    }
    log::result("ControlCenterAnchor", "window-not-found", false, false, &[]);
}

/// Pure close verification: a close is genuine only when the original window
/// is no longer live *and* discovery cannot find that same handle again.
#[must_use]
pub fn close_is_verified(still_live: bool, found_same_hwnd: bool) -> bool {
    !still_live && !found_same_hwnd
}

fn start_is_closed(hwnd: usize) -> bool {
    close_is_verified(is_window_live(hwnd), find_start_window() == Some(hwnd))
}

fn wait_for_close(hwnd: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if start_is_closed(hwnd) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(30));
    }
}

/// Pure open verification: the window we saw before the trigger must not be the
/// one reported afterwards.
#[must_use]
pub fn open_is_verified(before: Option<usize>, found: Option<usize>) -> bool {
    found.is_some() && found != before
}

fn wait_for_open(before: Option<usize>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if open_is_verified(before, find_start_window()) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(30));
    }
}

fn post_escape(handle: HWND) {
    for (message, key) in escape_messages() {
        // SAFETY: posting a keyboard notification with no pointer payload.
        unsafe {
            PostMessageW(handle, message, key as usize, 0);
        }
    }
}

/// Dismisses the flyout with an `VK_ESCAPE` pair and verifies the specific
/// HWND is gone before clearing state.
///
/// `mark_closed()` is fenced by a token/HWND session: a new open clears the
/// session and advances the generation, so a delayed close can no longer
/// invalidate the newer open's worker. If the flyout cannot be focused, no
/// global `SendInput` is injected — the ESC is posted directly to its HWND so
/// it cannot land in an unrelated foreground window.
pub fn close_start(hwnd: usize) -> bool {
    let session = begin_close(hwnd);
    let _guard = CloseSessionGuard(session);
    let still_current = || close_session_is_current(session);

    if !is_window_live(hwnd) {
        // Already gone: only clear state if this close still owns the session.
        if still_current() {
            mark_closed();
        }
        return true;
    }
    let handle = hwnd as HWND;
    // SAFETY: focusing a window that just passed the liveness check.
    let focused = unsafe { SetForegroundWindow(handle) != 0 };
    match esc_delivery(focused) {
        EscDelivery::SendInput => {
            let mapped = unsafe { MapVirtualKeyW(VK_ESCAPE as u32, MAPVK_VK_TO_VSC) };
            let scan = if mapped == 0 { 1 } else { mapped as u16 };
            let inputs = escape_inputs(scan).map(keyboard_input);
            // SAFETY: two properly sized keyboard INPUT structures sent in order.
            let sent = unsafe {
                SendInput(
                    inputs.len() as u32,
                    inputs.as_ptr(),
                    size_of::<INPUT>() as i32,
                ) == inputs.len() as u32
            };
            if !sent {
                log::warn(format!(
                    "StartAnchor ESC SendInput failed for hwnd {hwnd:#x}."
                ));
            }
        }
        EscDelivery::Posted => {
            log::warn(format!(
                "StartAnchor could not focus hwnd {hwnd:#x}; posting ESC directly."
            ));
            post_escape(handle);
        }
    }
    if still_current() && wait_for_close(hwnd, Duration::from_millis(180)) {
        mark_closed();
        return true;
    }
    // Best-effort fallback. A successful post by itself is NOT a success.
    post_escape(handle);
    let verified = wait_for_close(hwnd, Duration::from_millis(120));
    if verified {
        if still_current() {
            mark_closed();
        }
    } else {
        log::warn(format!(
            "StartAnchor ESC could not verify close of hwnd {hwnd:#x}."
        ));
    }
    verified
}

fn anchored_hwnd() -> Option<usize> {
    let hwnd = ANCHORED_HWND.load(Ordering::Acquire);
    (hwnd != 0).then_some(hwnd)
}

/// Verification of a shell open, moved off the UI thread. It only logs and may
/// inject the `VK_LWIN` fallback: the UNCLOAKED hook is the sole owner of
/// `mark_opened()`, and a generation change means that hook already took over.
fn verify_open_worker(
    owner_hwnd: usize,
    before: Option<usize>,
    posted: bool,
    token: u64,
    writer_token: u64,
) {
    if cancelled(FlyoutKind::Start, token, writer_token) {
        return;
    }
    let mut verified = posted && wait_for_open(before, Duration::from_millis(220));
    if open_fallback_needed(posted, verified) {
        if cancelled(FlyoutKind::Start, token, writer_token) {
            return;
        }
        if !crate::taskbar::send_chord(&[VK_LWIN]) {
            log::warn("StartAnchor VK_LWIN fallback could not inject a chord.");
        }
        verified = wait_for_open(before, Duration::from_millis(260));
    }
    if verified {
        log::result(
            "StartAnchor",
            "opened",
            true,
            true,
            &[("owner", format!("{owner_hwnd:#x}"))],
        );
    } else {
        log::warn("StartAnchor open could not be verified (SC_TASKLIST + VK_LWIN).");
    }
}

/// Opens Start by posting `SC_TASKLIST` to the *owner* window (the dock), not
/// to our own hidden window. This keeps Explorer's activation/focus bookkeeping
/// intact and works when the keyboard hook filters injected Win keys.
///
/// **Non-blocking**: it returns the post result immediately. The old UI-thread
/// `find_start_window()` scan and both verification waits now run in
/// [`verify_open_worker`]. `mark_opened()` is never called here — it belongs
/// exclusively to the UNCLOAKED hook, which fires *before* the new anchor
/// worker captures its token. That removes the double generation bump which
/// used to cancel the fresh worker.
pub fn open_start_via_syscommand(owner_hwnd: usize) -> bool {
    if owner_hwnd == 0 || unsafe { IsWindow(owner_hwnd as HWND) != TRUE } {
        log::warn(format!(
            "StartAnchor open_start_via_syscommand rejected owner {owner_hwnd:#x}."
        ));
        return false;
    }
    // Cheap snapshot from the last anchor; no full window/process enumeration
    // on the UI thread.
    let before = anchored_hwnd();
    let token = invalidate_anchor();
    let writer_token = flyout_writer::generation();
    // SAFETY: a parameterless system command posted to a validated window.
    let posted =
        unsafe { PostMessageW(owner_hwnd as HWND, WM_SYSCOMMAND, SC_TASKLIST as usize, 0) == TRUE };
    if !posted {
        log::warn(format!(
            "StartAnchor SC_TASKLIST post failed for owner {owner_hwnd:#x}."
        ));
    }
    let spawned = thread::Builder::new()
        .name("opentaskbar-start-verify".into())
        .spawn(move || verify_open_worker(owner_hwnd, before, posted, token, writer_token));
    if spawned.is_err() {
        log::error("StartAnchor could not spawn its open verification worker.");
    }
    posted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::MutexGuard;

    /// Generation and close-session statics are process-wide, so tests that
    /// advance them must not overlap.
    static TEST_SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn work() -> Anchor {
        Anchor {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }

    /// Deterministic geometry helper: the public entry point reads the live
    /// `TaskbarAl` registry value, which a source-level test must not depend on.
    fn target(
        anchor: Anchor,
        w: i32,
        h: i32,
        work: Anchor,
        edge: DockEdge,
        dpi: u32,
    ) -> (i32, i32) {
        compute_target_with_alignment(anchor, w, h, work, edge, dpi, false)
    }

    #[test]
    fn bottom_edge_places_flyout_above_start_button() {
        let anchor = Anchor {
            left: 100,
            top: 1000,
            right: 160,
            bottom: 1040,
        };
        assert_eq!(
            target(anchor, 640, 724, work(), DockEdge::Bottom, 96),
            (8, 264)
        );
    }

    #[test]
    fn start_uses_cell_center_without_changing_search_or_control_center() {
        let anchor = Anchor {
            left: 100,
            top: 1000,
            right: 160,
            bottom: 1040,
        };
        // Moving the cell right by 20 DIP moves Start by the same 20 DIP.
        let moved = Anchor {
            left: anchor.left + 20,
            right: anchor.right + 20,
            ..anchor
        };
        // A narrow panel is not clamped to the work area, so the 20 DIP shift
        // of the cell center is observable (a 640 px panel would clamp at 8).
        let start = target(anchor, 60, 724, work(), DockEdge::Bottom, 96);
        let start_moved = target(moved, 60, 724, work(), DockEdge::Bottom, 96);
        assert_eq!(start_moved.0 - start.0, 20);
        assert_eq!(
            compute_search_target(anchor, 640, 724, work(), DockEdge::Bottom, 96),
            (92, 268)
        );
        assert_eq!(
            compute_control_center_target(anchor, 640, 724, work(), DockEdge::Bottom, 96),
            (8, 264)
        );
    }

    #[test]
    fn left_edge_places_flyout_right_of_start_button() {
        let anchor = Anchor {
            left: 0,
            top: 400,
            right: 56,
            bottom: 456,
        };
        // Above the middle third: top edge of the button is used directly.
        assert_eq!(
            target(anchor, 600, 700, work(), DockEdge::Left, 96),
            (68, 78)
        );
    }

    #[test]
    fn right_edge_places_flyout_left_of_start_button() {
        let anchor = Anchor {
            left: 1864,
            top: 1000,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(
            target(anchor, 600, 700, work(), DockEdge::Right, 96),
            (1252, 340)
        );
    }

    #[test]
    fn dpi_scales_margin_and_gap() {
        let anchor = Anchor {
            left: 100,
            top: 1000,
            right: 160,
            bottom: 1040,
        };
        assert_eq!(
            target(anchor, 640, 724, work(), DockEdge::Bottom, 192),
            (16, 252)
        );
    }

    #[test]
    fn full_screen_canvas_compensates_local_flyout_offset() {
        let anchor = Anchor {
            left: 100,
            top: 1000,
            right: 160,
            bottom: 1040,
        };
        assert!(is_full_screen_canvas(1920, work().width()));
        assert_eq!(
            target(anchor, 1920, 1080, work(), DockEdge::Bottom, 96),
            (-592, -80)
        );
    }

    #[test]
    fn side_flyout_y_is_clamped_to_work_area() {
        let high = Anchor {
            left: 0,
            top: 10,
            right: 56,
            bottom: 50,
        };
        assert_eq!(side_flyout_y(high, 700, work(), 96), 10);
        let low = Anchor {
            left: 0,
            top: 1040,
            right: 56,
            bottom: 1080,
        };
        assert_eq!(side_flyout_y(low, 700, work(), 96), 372);
    }

    #[test]
    fn anchor_and_hwnd_validation_reject_degenerate_inputs() {
        assert!(!Anchor::default().is_valid());
        assert!(
            Anchor {
                left: 10,
                top: 10,
                right: 11,
                bottom: 11
            }
            .is_valid()
        );
        assert!(!hwnd_is_usable(0, true, true, false));
        assert!(!hwnd_is_usable(42, false, true, false));
        assert!(!hwnd_is_usable(42, true, true, true));
        assert!(hwnd_is_usable(42, true, true, false));
        assert!(!is_window_live(0));
    }

    #[test]
    fn escape_messages_are_keydown_then_keyup() {
        assert_eq!(
            escape_messages(),
            [(WM_KEYDOWN, VK_ESCAPE as u32), (WM_KEYUP, VK_ESCAPE as u32)]
        );
        assert_eq!(WM_SYSCOMMAND, 0x0112);
        assert_eq!(SC_TASKLIST, 0xF130);
        assert_eq!(VK_LWIN, 91);
    }

    #[test]
    fn newer_generation_cancels_older_worker() {
        let mut current = 7u64;
        let captured = current;
        assert!(token_is_current(captured, current));
        current += 1;
        assert!(!token_is_current(captured, current));
        assert!(captured != current);
    }

    #[test]
    fn taskbar_alignment_shifts_canvas_local_x() {
        assert_eq!(canvas_local_x(1920, 642, 12, false), 639);
        assert_eq!(canvas_local_x(1920, 642, 12, true), 12);
        let anchor = Anchor {
            left: 100,
            top: 1000,
            right: 160,
            bottom: 1040,
        };
        let centered =
            compute_target_with_alignment(anchor, 1920, 1080, work(), DockEdge::Bottom, 96, false);
        let left =
            compute_target_with_alignment(anchor, 1920, 1080, work(), DockEdge::Bottom, 96, true);
        assert_eq!(left.0 - centered.0, 639 - 12);
        assert_eq!(left.1, centered.1);
    }

    #[test]
    fn localized_start_titles_are_case_insensitive_and_trimmed() {
        assert_eq!(START_TITLES.len(), 24);
        for title in START_TITLES {
            assert!(is_start_title(title), "{title}");
            assert!(
                is_start_title(&format!("  {}  ", title.to_uppercase())),
                "{title}"
            );
        }
        assert!(!is_start_title("Search"));
        assert!(!is_start_title(""));
    }

    #[test]
    fn start_host_exe_requires_exact_file_name() {
        assert!(exe_is_start_host(
            r"C:\Windows\SystemApps\StartMenuExperienceHost.exe"
        ));
        assert!(exe_is_start_host(
            r"C:\Windows\SystemApps\ShellExperienceHost.exe"
        ));
        assert!(exe_is_start_host("STARTMENUEXPERIENCEHOST.EXE"));
        assert!(!exe_is_start_host(
            r"C:\Temp\evil-startmenuexperiencehost.exe"
        ));
        assert!(!exe_is_start_host(
            r"C:\Temp\startmenuexperiencehost.exe.bak"
        ));
        assert!(!exe_is_start_host("explorer.exe"));
    }

    #[test]
    fn esc_close_requires_both_liveness_checks() {
        assert!(close_is_verified(false, false));
        assert!(!close_is_verified(true, false));
        assert!(!close_is_verified(false, true));
        assert!(!close_is_verified(true, true));
        let inputs = escape_inputs(1);
        assert_eq!(inputs[0].wVk, VK_ESCAPE);
        assert_eq!(inputs[0].wScan, 1);
        assert_eq!(inputs[0].dwFlags, KEYEVENTF_EXTENDEDKEY);
        assert_eq!(inputs[1].dwFlags, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP);
    }

    #[test]
    fn verified_open_needs_a_new_window() {
        assert!(open_is_verified(None, Some(42)));
        assert!(open_is_verified(Some(7), Some(42)));
        assert!(!open_is_verified(Some(42), Some(42)));
        assert!(!open_is_verified(None, None));
    }

    #[test]
    fn state_flags_transition_on_open_and_close() {
        let _serial = serial();
        set_enabled(Some(true));
        assert!(is_enabled());
        mark_opened();
        assert!(!is_anchored());
        assert!(!anchor_pending());
        // A disabled feature must not schedule an attempt, even with a valid
        // anchor; an invalid anchor is rejected independently.
        set_enabled(Some(false));
        assert!(!is_enabled());
        assert!(!reposition_start(Anchor::default(), 0));
        set_enabled(Some(true));
        assert!(!reposition_start(Anchor::default(), 0));
        assert!(!anchor_pending());
        mark_closed();
        assert!(!is_anchored());
        set_enabled(None);
    }

    #[test]
    fn enable_override_defaults_to_true() {
        assert!(override_is_enabled(-1));
        assert!(override_is_enabled(1));
        assert!(!override_is_enabled(0));
        set_enabled(None);
        assert!(is_enabled());
    }

    #[test]
    fn pid_exe_cache_expires_at_two_seconds() {
        assert_eq!(PID_EXE_CACHE_TTL, Duration::from_secs(2));
        let at = Instant::now();
        assert!(cache_entry_fresh(
            at,
            at + Duration::from_millis(1999),
            PID_EXE_CACHE_TTL
        ));
        assert!(!cache_entry_fresh(
            at,
            at + PID_EXE_CACHE_TTL,
            PID_EXE_CACHE_TTL
        ));
        // A future timestamp must not panic the polling chain.
        assert!(cache_entry_fresh(
            at + Duration::from_secs(10),
            at,
            PID_EXE_CACHE_TTL
        ));
    }

    #[test]
    fn pid_exe_lookup_is_skipped_until_ttl_expires() {
        let pid = u32::MAX - 11; // test-only PID, never a real Start host
        let base = Instant::now();
        let calls = Cell::new(0);
        let exe = r"C:\Windows\SystemApps\StartMenuExperienceHost.exe".to_owned();

        let hit = cached_process_exe_with(pid, base, |_| {
            calls.set(calls.get() + 1);
            Some(exe.clone())
        });
        assert!(hit.is_some_and(|path| exe_is_start_host(&path)));
        assert_eq!(calls.get(), 1);

        // Still fresh: the lookup closure must not run at all (no OpenProcess /
        // QueryFullProcessImageNameW on the 200 ms anchor tick).
        let hit = cached_process_exe_with(pid, base + Duration::from_millis(1999), |_| {
            calls.set(calls.get() + 1);
            None
        });
        assert!(hit.is_some_and(|path| exe_is_start_host(&path)));
        assert_eq!(calls.get(), 1);

        // Expired: one fresh lookup, and a failure is cached as an empty string.
        let expired = base + PID_EXE_CACHE_TTL;
        assert!(
            cached_process_exe_with(pid, expired, |_| {
                calls.set(calls.get() + 1);
                None
            })
            .is_none()
        );
        assert_eq!(calls.get(), 2);
        assert!(
            cached_process_exe_with(pid, expired + Duration::from_millis(1), |_| {
                calls.set(calls.get() + 1);
                Some(exe)
            })
            .is_none()
        );
        assert_eq!(calls.get(), 2);

        let _ = pid_exe_cache().lock().map(|mut cache| cache.remove(&pid));
    }

    #[test]
    fn pid_zero_never_reaches_the_lookup() {
        let calls = Cell::new(0);
        assert!(
            cached_process_exe_with(0, Instant::now(), |_| {
                calls.set(calls.get() + 1);
                Some("startmenuexperiencehost.exe".to_owned())
            })
            .is_none()
        );
        assert_eq!(calls.get(), 0);
    }

    // ---- P0 / parity regression tests ----------------------------------------

    #[test]
    fn open_trigger_does_not_cancel_the_new_worker() {
        let _serial = serial();
        set_enabled(Some(true));
        let writer = flyout_writer::generation();

        let base = generation_for(FlyoutKind::Start);
        // Non-blocking open trigger: exactly one bump, no mark_opened().
        let trigger = invalidate_anchor();
        assert_eq!(trigger, base + 1);
        // UNCLOAKED hook (main.rs): mark_opened(), then reposition_start().
        mark_opened();
        let hook = generation_for(FlyoutKind::Start);
        assert_eq!(hook, trigger + 1);
        let worker = bump_generation(FlyoutKind::Start);
        assert_eq!(worker, hook + 1);
        assert!(!cancelled(FlyoutKind::Start, worker, writer));

        // The old P0 bug: a second mark_opened() after the worker was queued
        // advanced the generation and cancelled that worker mid-entrance.
        mark_opened();
        assert!(cancelled(FlyoutKind::Start, worker, writer));
    }

    #[test]
    fn delayed_close_cannot_invalidate_a_new_open() {
        let _serial = serial();
        let old = begin_close(0x1234);
        assert!(close_session_is_current(old));
        assert!(token_is_current(old.0, generation_for(FlyoutKind::Start)));

        // A fresh UNCLOAKED open clears the session and advances the generation.
        mark_opened();
        assert!(!close_session_is_current(old));
        assert!(!token_is_current(old.0, generation_for(FlyoutKind::Start)));
        clear_close_session();
    }

    #[test]
    fn close_session_is_pinned_to_its_hwnd() {
        let _serial = serial();
        let first = begin_close(0x1111);
        let second = begin_close(0x2222);
        assert!(!close_session_is_current(first));
        assert!(close_session_is_current(second));
        clear_close_session();
        assert!(!close_session_is_current(second));
    }

    #[test]
    fn focus_failure_switches_esc_to_the_targeted_post_path() {
        assert_eq!(esc_delivery(true), EscDelivery::SendInput);
        assert_eq!(esc_delivery(false), EscDelivery::Posted);
        // The posted pair is always keydown then keyup.
        assert_eq!(escape_messages().len(), 2);
    }

    #[test]
    fn open_fallback_only_fires_for_an_unverified_post() {
        assert!(!open_fallback_needed(true, true));
        assert!(open_fallback_needed(true, false));
        assert!(open_fallback_needed(false, true));
        assert!(open_fallback_needed(false, false));
    }

    #[test]
    fn flyout_kinds_have_independent_generations() {
        let _serial = serial();
        let start = generation_for(FlyoutKind::Start);
        let search = generation_for(FlyoutKind::Search);
        let cc = generation_for(FlyoutKind::ControlCenter);
        assert_eq!(bump_generation(FlyoutKind::Search), search + 1);
        // Repositioning Search must not cancel a Start or Control Center worker.
        assert_eq!(generation_for(FlyoutKind::Start), start);
        assert_eq!(generation_for(FlyoutKind::ControlCenter), cc);
        assert_eq!(FlyoutKind::Start.index(), 0);
        assert_eq!(FlyoutKind::Search.index(), 1);
        assert_eq!(FlyoutKind::ControlCenter.index(), 2);
    }

    #[test]
    fn all_three_flyout_kinds_use_their_own_geometry() {
        let anchor = Anchor {
            left: 100,
            top: 1000,
            right: 160,
            bottom: 1040,
        };
        assert_eq!(
            compute_flyout_target(
                FlyoutKind::Start,
                anchor,
                640,
                724,
                work(),
                DockEdge::Bottom,
                96
            ),
            compute_target(anchor, 640, 724, work(), DockEdge::Bottom, 96)
        );
        assert_eq!(
            compute_flyout_target(
                FlyoutKind::Search,
                anchor,
                640,
                724,
                work(),
                DockEdge::Bottom,
                96
            ),
            compute_search_target(anchor, 640, 724, work(), DockEdge::Bottom, 96)
        );
        assert_eq!(
            compute_flyout_target(
                FlyoutKind::ControlCenter,
                anchor,
                640,
                724,
                work(),
                DockEdge::Bottom,
                96
            ),
            compute_control_center_target(anchor, 640, 724, work(), DockEdge::Bottom, 96)
        );
        // Search tracks the button with an 8 px margin; Control Center centers.
        assert_eq!(
            compute_search_target(anchor, 640, 724, work(), DockEdge::Bottom, 96),
            (92, 268)
        );
        assert_eq!(
            compute_control_center_target(anchor, 640, 724, work(), DockEdge::Bottom, 96),
            (8, 264)
        );
    }

    #[test]
    fn reposition_flyout_rejects_an_empty_anchor_for_every_kind() {
        let _serial = serial();
        set_enabled(Some(true));
        for kind in [
            FlyoutKind::Start,
            FlyoutKind::Search,
            FlyoutKind::ControlCenter,
        ] {
            assert!(!reposition_flyout(kind, Anchor::default(), 0), "{kind:?}");
        }
        assert_eq!(FlyoutKind::default(), FlyoutKind::Start);
        assert_eq!(FlyoutKind::Start.label(), "Start");
        assert_eq!(FlyoutKind::Search.label(), "Search");
        assert_eq!(FlyoutKind::ControlCenter.label(), "ControlCenter");
    }

    #[test]
    fn search_and_control_center_workers_match_reference_timing() {
        assert_eq!(SEARCH_ATTEMPTS, 35);
        assert_eq!(SEARCH_DELAY_MS, 20);
        assert_eq!(SEARCH_SETTLE_FRAMES, 5);
        assert_eq!(SEARCH_SETTLE_DELAY_MS, 30);
        assert_eq!(CONTROL_CENTER_ATTEMPTS, 24);
        assert_eq!(CONTROL_CENTER_DELAY_MS, 25);
        assert_eq!(CONTROL_CENTER_REASSERT_MS, 75);
    }
}
