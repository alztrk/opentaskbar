//! Anchors the native Windows Start flyout to the dock's Start button on both
//! Windows 10 (a small `CoreWindow` popup) and Windows 11 (a monitor-sized
//! XAML canvas with the real flyout drawn inside it).
//!
//! The public surface is deliberately tiny so the app crate can drive it from
//! its cloak/foreground hooks:
//!
//! - [`find_start_window`] discovers the live Start window (PID first, class
//!   and localized title as a fallback),
//! - [`reposition_start`] schedules a bounded, generation-cancellable move
//!   loop,
//! - [`is_anchored`] reports whether the last loop actually succeeded and the
//!   window is still open,
//! - [`close_start`] / [`open_start_via_syscommand`] perform the safe
//!   keyboard / `SC_TASKLIST` shell triggers.
//!
//! `flyout_writer::remember()` is called with the *original* position before
//! the first move, so the native watchdog can restore Start on shutdown.

use std::collections::HashSet;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use opentaskbar_core::log;
use opentaskbar_core::settings::{DockEdge, load_from, settings_path};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromRect,
};
use windows_sys::Win32::System::ProcessStatus::EnumProcesses;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClassNameW, GetSystemMetrics, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW, SC_TASKLIST, SM_CXSCREEN,
    SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, WM_KEYDOWN, WM_KEYUP,
    WM_SYSCOMMAND,
};

use crate::flyout_writer;

const START_MENU_EXE: &str = "startmenuexperiencehost.exe";
const CORE_WINDOW_CLASS: &str = "Windows.UI.Core.CoreWindow";
const START_TITLES: [&str; 2] = ["Start", "Başlat"];
const ATTEMPTS: usize = 40;
const ATTEMPT_DELAY_MS: u64 = 20;
const SETTLE_FRAMES: usize = 6;
const SETTLE_DELAY_MS: u64 = 25;
const SWP_FLAGS: u32 = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;

/// Bumped on every new reposition request. Workers capture it and abort as
/// soon as a newer request (or a journal restore) supersedes them.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static ANCHORED: AtomicBool = AtomicBool::new(false);
static ANCHOR_VALID: AtomicBool = AtomicBool::new(false);
static ANCHORED_HWND: AtomicUsize = AtomicUsize::new(0);

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
    let margin = scale_px(8, dpi);
    let gap = scale_px(12, dpi);

    if is_full_screen_canvas(flyout_width, work.width()) {
        // UIA measurement is not available in the shell crate, so use the same
        // nominal flyout size as the C# parity implementation and subtract its
        // local offset inside the canvas.
        let panel_w = scale_px(642, dpi);
        let panel_h = scale_px(724, dpi);
        let local_x = (work.width() - panel_w) / 2;
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
                clamp(anchor.left - margin, min_x, max_x),
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
            clamp(anchor.left - margin, min_x, max_x),
            clamp(anchor.top - flyout_height - gap, min_y, max_y),
        ),
    }
}

fn max(a: i32, b: i32) -> i32 {
    a.max(b)
}

/// `ESC` is delivered as a key-down/key-up pair, never as a `WM_CLOSE`: the
/// Start XAML island only handles the keyboard dismissal.
#[must_use]
pub fn escape_messages() -> [(u32, u32); 2] {
    [(WM_KEYDOWN, VK_ESCAPE as u32), (WM_KEYUP, VK_ESCAPE as u32)]
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

fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// A worker stops when a newer request arrives or when `flyout_writer`
/// restores/clears the journal (its generation is bumped on both paths).
fn cancelled(token: u64, writer_token: u64) -> bool {
    !token_is_current(token, generation()) || !flyout_writer::is_current(writer_token)
}

fn set_anchored(hwnd: usize) {
    ANCHORED_HWND.store(hwnd, Ordering::Release);
    ANCHORED.store(true, Ordering::Release);
}

/// Clears the anchored flag. WS-C can call this from its Start close/focus
/// handler in addition to the built-in liveness monitor.
pub fn mark_closed() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    ANCHORED.store(false, Ordering::Release);
    ANCHOR_VALID.store(false, Ordering::Release);
    ANCHORED_HWND.store(0, Ordering::Release);
}

/// A fresh Start opening invalidates any previous anchoring result until the
/// new flyout has actually been moved.
pub fn mark_opened() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    ANCHORED.store(false, Ordering::Release);
}

/// True only after a completed reposition whose window is still live.
#[must_use]
pub fn is_anchored() -> bool {
    if !ANCHORED.load(Ordering::Acquire) || !ANCHOR_VALID.load(Ordering::Acquire) {
        return false;
    }
    let hwnd = ANCHORED_HWND.load(Ordering::Acquire);
    hwnd != 0 && is_window_live(hwnd)
}

// ---- Win32 helpers -------------------------------------------------------------

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

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

/// Validates a hint before moving it: non-null, live, visible, uncloaked and
/// still owned by the Start shell process (a cloak event HWND may be recycled).
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
        if process_exe(pid).is_some_and(|exe| exe.to_ascii_lowercase().ends_with(START_MENU_EXE)) {
            return true;
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(handle, class.as_mut_ptr(), class.len() as i32);
        if len <= 0 {
            return false;
        }
        let class = String::from_utf16_lossy(&class[..len as usize]);
        if !class.eq_ignore_ascii_case(CORE_WINDOW_CLASS) {
            return false;
        }
        let mut title = [0u16; 128];
        let len = GetWindowTextW(handle, title.as_mut_ptr(), title.len() as i32);
        let title = String::from_utf16_lossy(&title[..len.max(0) as usize]);
        START_TITLES.contains(&title.as_str())
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
        .filter(|&pid| {
            process_exe(pid).is_some_and(|exe| exe.to_ascii_lowercase().ends_with(START_MENU_EXE))
        })
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
        if is_window_live(hwnd as usize)
            && window_rect(hwnd as usize).is_some_and(|r| rect_area(r) > 0)
        {
            return Some(hwnd as usize);
        }
    }
    None
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

/// Schedules the Start flyout reposition on a background worker.
///
/// Returns `false` only when nothing could be scheduled (invalid anchor or
/// thread creation failure). The actual result is reported asynchronously via
/// [`is_anchored`], so the UI thread never blocks on shell animation frames.
/// A new call cancels any in-flight worker via the generation token.
pub fn reposition_start(anchor: Anchor, hwnd_hint: usize) -> bool {
    if !anchor.is_valid() {
        log::warn("StartAnchor rejected an empty anchor rect.");
        mark_closed();
        return false;
    }
    let token = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    ANCHORED.store(false, Ordering::Release);
    ANCHOR_VALID.store(true, Ordering::Release);
    ANCHORED_HWND.store(0, Ordering::Release);
    let writer_token = flyout_writer::generation();
    let edge = load_from(&settings_path()).edge;
    let anchor_copy = anchor;

    let spawned = thread::Builder::new()
        .name("opentaskbar-start-anchor".into())
        .spawn(move || anchor_worker(anchor_copy, hwnd_hint, edge, token, writer_token));
    if spawned.is_err() {
        log::error("StartAnchor could not spawn its reposition worker.");
        mark_closed();
        return false;
    }
    log::info(format!(
        "StartAnchor scheduled [{},{}-{},{}], edge {edge:?}, hint {hwnd_hint:#x}.",
        anchor.left, anchor.top, anchor.right, anchor.bottom
    ));
    true
}

fn anchor_worker(anchor: Anchor, hwnd_hint: usize, edge: DockEdge, token: u64, writer_token: u64) {
    let work = work_area(anchor);
    for _ in 0..ATTEMPTS {
        thread::sleep(Duration::from_millis(ATTEMPT_DELAY_MS));
        if cancelled(token, writer_token) {
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
        let origin = if canvas {
            (work.left, work.top)
        } else {
            (rect.left, rect.top)
        };
        // Journal the *original* position before the first move.
        flyout_writer::remember(hwnd, pid, origin.0, origin.1);

        let dpi = unsafe { GetDpiForWindow(hwnd as HWND) };
        let dpi = if dpi == 0 { 96 } else { dpi };
        let (x, y) = compute_target(anchor, width, height, work, edge, dpi);
        if cancelled(token, writer_token) || !is_valid_hwnd(hwnd) {
            return;
        }
        if !apply_position(hwnd, x, y) {
            continue;
        }
        for _ in 0..SETTLE_FRAMES {
            thread::sleep(Duration::from_millis(SETTLE_DELAY_MS));
            if cancelled(token, writer_token) || !is_valid_hwnd(hwnd) {
                return;
            }
            apply_position(hwnd, x, y);
        }
        // Explorer can re-assert its own Z-order/position right after the
        // entrance transition; one final move wins that race.
        if cancelled(token, writer_token) || !is_valid_hwnd(hwnd) || !apply_position(hwnd, x, y) {
            return;
        }
        set_anchored(hwnd);
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
}

/// Keeps `is_anchored()` honest: once the Start flyout closes (or its handle is
/// recycled), the flag drops without needing a callback from the app crate.
fn monitor_while_open(hwnd: usize, token: u64, writer_token: u64) {
    let mut misses = 0u8;
    loop {
        thread::sleep(Duration::from_millis(100));
        if cancelled(token, writer_token) {
            return;
        }
        if is_valid_hwnd(hwnd) {
            misses = 0;
        } else {
            misses += 1;
            if misses >= 2 {
                ANCHORED.store(false, Ordering::Release);
                ANCHORED_HWND.store(0, Ordering::Release);
                log::info("StartAnchor cleared: Start window closed.");
                return;
            }
        }
    }
}

/// Dismisses the flyout with `VK_ESCAPE` key messages (works even when the
/// Start window is not foreground). Returns `false` for an invalid handle or
/// if either post fails.
pub fn close_start(hwnd: usize) -> bool {
    if !is_window_live(hwnd) {
        log::warn(format!("StartAnchor close_start rejected hwnd {hwnd:#x}."));
        return false;
    }
    let handle = hwnd as HWND;
    let mut ok = true;
    for (message, key) in escape_messages() {
        // SAFETY: Posting a keyboard notification to a validated live window;
        // no memory is passed through WPARAM/LPARAM.
        ok &= unsafe { PostMessageW(handle, message, key as usize, 0) == TRUE };
    }
    if ok {
        mark_closed();
    } else {
        log::warn(format!("StartAnchor ESC post failed for hwnd {hwnd:#x}."));
    }
    ok
}

/// Opens Start by posting `SC_TASKLIST` to the *owner* window (the dock), not
/// to our own hidden window. This keeps Explorer's activation/focus bookkeeping
/// intact and works when the keyboard hook filters injected Win keys.
pub fn open_start_via_syscommand(owner_hwnd: usize) -> bool {
    if owner_hwnd == 0 || unsafe { IsWindow(owner_hwnd as HWND) != TRUE } {
        log::warn(format!(
            "StartAnchor open_start_via_syscommand rejected owner {owner_hwnd:#x}."
        ));
        return false;
    }
    mark_opened();
    // SAFETY: a parameterless system command posted to a validated window.
    let ok =
        unsafe { PostMessageW(owner_hwnd as HWND, WM_SYSCOMMAND, SC_TASKLIST as usize, 0) == TRUE };
    if !ok {
        log::warn(format!(
            "StartAnchor SC_TASKLIST post failed for owner {owner_hwnd:#x}."
        ));
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_LWIN;

    fn work() -> Anchor {
        Anchor {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
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
            compute_target(anchor, 640, 724, work(), DockEdge::Bottom, 96),
            (92, 264)
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
            compute_target(anchor, 600, 700, work(), DockEdge::Left, 96),
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
            compute_target(anchor, 600, 700, work(), DockEdge::Right, 96),
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
            compute_target(anchor, 640, 724, work(), DockEdge::Bottom, 192),
            (84, 252)
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
            compute_target(anchor, 1920, 1080, work(), DockEdge::Bottom, 96),
            (-547, -80)
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
}
