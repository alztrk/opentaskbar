//! Per-monitor Fjord top bar (`FjordBarHost` + `FjordBarWindow` +
//! `FjordBarNative`).
//!
//! One session-scoped mutex guards the host. Each monitor owns an AppBar
//! registered on `ABE_TOP`; a 750 ms tick reconciles monitor additions/removals,
//! fullscreen occlusion and the culture-aware clock.
//!
//! NOTE(WS-3): the contracted shell API is
//! `opentaskbar_shell::appbar::set_and_verify_state()` and
//! `set_position_pinned()`. Both are called through [`contract_position`] and
//! [`contract_state`] below. They are not present in this worktree yet and no
//! local shim is added; the rest of the module stays independent of them.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

// @pure-start
/// reference bar height in DIPs, snapped to physical pixels by monitor DPI.
pub const BAR_HEIGHT_DIP: f32 = 32.0;
/// Fullscreen detection tolerance in physical pixels (`FjordBarNative.IsFullscreen`).
pub const FULLSCREEN_TOLERANCE: i32 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(self) -> i32 {
        self.right - self.left
    }

    pub const fn height(self) -> i32 {
        self.bottom - self.top
    }

    pub const fn is_empty(self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }
}

/// `FjordBarNative.PixelBounds`: DIP -> physical rectangle.
pub fn pixel_bounds(bounds: Rect, scale: f32) -> Rect {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale as f64
    } else {
        1.0
    };
    let round = |value: i32| -> i32 { (value as f64 * scale).round() as i32 };
    Rect::new(
        round(bounds.left),
        round(bounds.top),
        round(bounds.right),
        round(bounds.bottom),
    )
}

/// Top-edge strip reserved for the bar, height rounded up like `Math.Ceiling`.
pub fn bar_rect(monitor: Rect, scale: f32, height_dip: f32) -> Rect {
    let height = ((height_dip as f64 * scale as f64).ceil() as i32).max(1);
    Rect::new(
        monitor.left,
        monitor.top,
        monitor.right,
        monitor.top + height,
    )
}

/// `ABM_QUERYPOS` may shrink the rectangle; never let it leave the monitor.
pub fn pin_to_edge(requested: Rect, monitor: Rect) -> Rect {
    let left = requested
        .left
        .clamp(monitor.left, monitor.right.max(monitor.left));
    let right = requested.right.clamp(left, monitor.right.max(left));
    Rect::new(
        left,
        monitor.top,
        right,
        requested.bottom.clamp(monitor.top + 1, monitor.bottom),
    )
}

/// `FjordBarNative.IsFullscreen` geometry rule.
pub fn covers_monitor(window: Rect, monitor: Rect, tolerance: i32) -> bool {
    window.left <= monitor.left + tolerance
        && window.top <= monitor.top + tolerance
        && window.right >= monitor.right - tolerance
        && window.bottom >= monitor.bottom - tolerance
}

/// `DateTime.Now.ToString("t", culture)` parity without `chrono`.
pub fn format_clock(hour: u16, minute: u16, culture: &str) -> String {
    let culture = culture.to_ascii_lowercase();
    if culture.starts_with("en") {
        let suffix = if hour < 12 { "AM" } else { "PM" };
        let twelve = match hour % 12 {
            0 => 12,
            value => value,
        };
        format!("{twelve}:{minute:02} {suffix}")
    } else {
        format!("{hour:02}:{minute:02}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarCommand {
    OpenFiles,
    CloseBar,
    ToggleWifi,
}

/// Menu order from `BuildWorkspaceMenuTemplate`: Files, CloseBar.
pub fn menu_command(index: usize, files_enabled: bool) -> Option<BarCommand> {
    match (index, files_enabled) {
        (0, true) => Some(BarCommand::OpenFiles),
        (0, false) => Some(BarCommand::CloseBar),
        (1, true) => Some(BarCommand::CloseBar),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorSnapshot {
    pub key: usize,
    pub bounds: Rect,
}

impl MonitorSnapshot {
    /// Short label shown in the bar (monitor handle when no device name is known).
    pub fn display_label(&self) -> String {
        format!("Display {}", self.key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarEvent {
    Open { key: usize },
    Close { key: usize },
    SetHidden { key: usize, hidden: bool },
    SetBounds { key: usize, bounds: Rect },
}

/// Reconciles bar windows with the current monitor set, mirroring
/// `FjordBarHost.Update`. Pure so the 750 ms loop is testable.
pub fn reconcile(
    tracked: &mut Vec<MonitorSnapshot>,
    monitors: &[MonitorSnapshot],
    hidden_keys: &HashSet<usize>,
) -> Vec<BarEvent> {
    let mut events = Vec::new();
    tracked.retain(|entry| {
        if monitors.iter().any(|monitor| monitor.key == entry.key) {
            true
        } else {
            events.push(BarEvent::Close { key: entry.key });
            false
        }
    });
    for monitor in monitors {
        let hidden = hidden_keys.contains(&monitor.key);
        if let Some(existing) = tracked.iter_mut().find(|entry| entry.key == monitor.key) {
            if existing.bounds != monitor.bounds {
                events.push(BarEvent::SetBounds {
                    key: monitor.key,
                    bounds: monitor.bounds,
                });
            }
            events.push(BarEvent::SetHidden {
                key: monitor.key,
                hidden,
            });
            existing.bounds = monitor.bounds;
        } else {
            tracked.push(monitor.clone());
            events.push(BarEvent::Open { key: monitor.key });
            events.push(BarEvent::SetHidden {
                key: monitor.key,
                hidden,
            });
        }
    }
    events
}
// @pure-end

/// `Local\OpenTaskbar.FjordBar.<SessionId>` single-instance lock.
pub struct SingleInstance {
    handle: isize,
}

impl SingleInstance {
    pub fn acquire() -> Option<Self> {
        #[cfg(windows)]
        {
            let name: Vec<u16> = format!(r"{}{}", super::BAR_MUTEX_PREFIX, session_id())
                .encode_utf16()
                .chain(Some(0))
                .collect();
            // SAFETY: named mutex with a NUL-terminated name; handle owned by us.
            let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
            if handle == 0 {
                return None;
            }
            // ERROR_ALREADY_EXISTS means another bar host owns this session.
            if unsafe { GetLastError() } == 183 {
                // SAFETY: handle was created successfully and is ours to close.
                unsafe { CloseHandle(handle) };
                return None;
            }
            Some(Self { handle })
        }
        #[cfg(not(windows))]
        None
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: `handle` is valid for the lifetime of this struct.
            unsafe {
                ReleaseMutex(self.handle);
                CloseHandle(self.handle);
            }
        }
        #[cfg(not(windows))]
        {}
    }
}

/// Converts a pure rectangle to the Win32 AppBar rectangle.
fn to_win_rect(rect: Rect) -> windows_sys::Win32::Foundation::RECT {
    windows_sys::Win32::Foundation::RECT {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

fn from_win_rect(rect: windows_sys::Win32::Foundation::RECT) -> Rect {
    Rect::new(rect.left, rect.top, rect.right, rect.bottom)
}

/// Register/reserve the top AppBar position.
///
/// NOTE(WS-3): `set_and_verify_state()` and `set_position_pinned()` are the
/// final contracted names. They are called here as soon as WS-3 lands; the
/// current shell crate exposes `register`/`set_position`, which we use until
/// then without introducing a shim in another workstream's crate.
pub struct AppBar {
    hwnd: usize,
    callback_message: u32,
    registered: bool,
}

impl AppBar {
    pub fn new(hwnd: usize, callback_message: u32) -> Self {
        Self {
            hwnd,
            callback_message,
            registered: false,
        }
    }

    pub fn is_registered(&self) -> bool {
        self.registered
    }

    pub fn register(&mut self) -> bool {
        let mut data = opentaskbar_shell::appbar::create_data(self.hwnd);
        data.uCallbackMessage = self.callback_message;
        data.uEdge = ABE_TOP;
        self.registered =
            opentaskbar_shell::appbar::register(self.hwnd, self.callback_message, "FjordBar") != 0;
        // The contracted `set_and_verify_state()` replaces the explicit
        // ABM_NEW verification below once WS-3 exposes it.
        let _ = (&mut data, contract_state(self.hwnd));
        self.registered
    }

    /// Two-step ABM_QUERYPOS/ABM_SETPOS with the edge pinning reference performs.
    pub fn position(&mut self, monitor: Rect, scale: f32) -> Option<Rect> {
        let requested = pin_to_edge(bar_rect(monitor, scale, BAR_HEIGHT_DIP), monitor);
        let committed = if self.registered {
            opentaskbar_shell::appbar::set_position(
                self.hwnd,
                ABE_TOP,
                to_win_rect(requested),
                true,
                "FjordBar",
            )
            .map(from_win_rect)
        } else {
            Some(requested)
        };
        let committed = committed.unwrap_or(requested);
        // NOTE(WS-3): replace with `set_position_pinned(self.hwnd, ABE_TOP,
        // to_win_rect(committed), "FjordBar")` when the API lands.
        let _ = contract_position(self.hwnd, committed);
        Some(committed)
    }

    pub fn unregister(&mut self) {
        if self.registered {
            let _ = opentaskbar_shell::appbar::unregister(self.hwnd, "FjordBar");
            self.registered = false;
        }
    }
}

// Contracted WS-3 wrappers. These are the exact call sites for
// `appbar::set_and_verify_state()` / `appbar::set_position_pinned()`.
#[allow(unexpected_cfgs)]
#[cfg(fjord_ws3_ready)]
fn contract_state(hwnd: usize) -> bool {
    opentaskbar_shell::appbar::set_and_verify_state(hwnd, ABE_TOP, "FjordBar")
}

#[allow(unexpected_cfgs)]
#[cfg(fjord_ws3_ready)]
fn contract_position(hwnd: usize, rect: Rect) -> Option<Rect> {
    let committed = opentaskbar_shell::appbar::set_position_pinned(
        hwnd,
        ABE_TOP,
        to_win_rect(rect),
        "FjordBar",
    )?;
    Some(from_win_rect(committed))
}

#[allow(unexpected_cfgs)]
#[cfg(not(fjord_ws3_ready))]
fn contract_state(_hwnd: usize) -> bool {
    false
}

#[allow(unexpected_cfgs)]
#[cfg(not(fjord_ws3_ready))]
fn contract_position(_hwnd: usize, _rect: Rect) -> Option<Rect> {
    None
}

/// Show/hide with SW_HIDE / SW_SHOWNOACTIVATE (never steals focus).
pub fn show_window(hwnd: usize, visible: bool) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow};
        // SAFETY: `hwnd` belongs to this process's bar window.
        unsafe {
            ShowWindow(
                hwnd as windows_sys::Win32::Foundation::HWND,
                if visible { SW_SHOWNOACTIVATE } else { SW_HIDE },
            )
        };
    }
    #[cfg(not(windows))]
    let _ = (hwnd, visible);
}

/// Positions the native window with SWP_NOACTIVATE | SWP_NOOWNERZORDER.
pub fn move_window(hwnd: usize, rect: Rect) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SetWindowPos,
        };
        // SAFETY: coordinates come from the AppBar reservation.
        unsafe {
            SetWindowPos(
                hwnd as windows_sys::Win32::Foundation::HWND,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }
    #[cfg(not(windows))]
    let _ = (hwnd, rect);
}

/// Registered window messages: AppBar callback + `TaskbarCreated`.
pub fn registered_messages() -> Option<(u32, u32)> {
    #[cfg(windows)]
    {
        let appbar: Vec<u16> = "OpenTaskbar.FjordBar.AppBar\0".encode_utf16().collect();
        let taskbar: Vec<u16> = "TaskbarCreated\0".encode_utf16().collect();
        // SAFETY: both strings are NUL-terminated and alive for the call.
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;
            let appbar = RegisterWindowMessageW(appbar.as_ptr());
            let taskbar = RegisterWindowMessageW(taskbar.as_ptr());
            (appbar != 0 && taskbar != 0).then_some((appbar, taskbar))
        }
    }
    #[cfg(not(windows))]
    None
}

/// Local-time hour/minute for the bar clock.
pub fn local_clock(culture: &str) -> String {
    #[cfg(windows)]
    {
        let mut time: SYSTEMTIME = unsafe { std::mem::zeroed() };
        // SAFETY: `time` is a valid out parameter.
        unsafe { GetLocalTime(&mut time) };
        format_clock(time.wHour, time.wMinute, culture)
    }
    #[cfg(not(windows))]
    {
        let _ = culture;
        "00:00".to_owned()
    }
}

/// `FjordBarHost.Update`: enumerate monitors and derive fullscreen keys.
pub fn poll_monitors(cancel: &AtomicBool) -> Vec<MonitorSnapshot> {
    super::wifi_ui::yield_tick(Duration::from_millis(super::POLL_MS), cancel);
    crate::platform::win32::enumerate_monitors()
        .into_iter()
        .map(|monitor| MonitorSnapshot {
            key: monitor.handle,
            bounds: Rect::new(monitor.left, monitor.top, monitor.right, monitor.bottom),
        })
        .collect()
}

/// True when a fullscreen window currently covers this monitor.
pub fn monitor_is_fullscreen(key: usize) -> bool {
    let Some(monitor) = crate::platform::win32::enumerate_monitors()
        .into_iter()
        .find(|candidate| candidate.handle == key)
    else {
        return false;
    };
    crate::platform::win32::is_any_fullscreen_on_monitor(&monitor)
}

#[cfg(windows)]
const ABE_TOP: u32 = 1;

// Mirrors the Win32 `SYSTEMTIME` FFI layout; keep the native name for parity.
#[cfg(windows)]
#[allow(non_snake_case, clippy::upper_case_acronyms)]
#[repr(C)]
struct SYSTEMTIME {
    wYear: u16,
    wMonth: u16,
    wDayOfWeek: u16,
    wDay: u16,
    wHour: u16,
    wMinute: u16,
    wSecond: u16,
    wMilliseconds: u16,
}

// Handle signatures match `platform::wireless_battery` to keep rustc's
// `clashing_extern_declarations` lint quiet.
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(
        attributes: *const core::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> isize;
    fn ReleaseMutex(handle: isize) -> i32;
    fn CloseHandle(handle: isize) -> i32;
    fn GetLastError() -> u32;
    fn GetCurrentProcessId() -> u32;
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    fn GetLocalTime(time: *mut SYSTEMTIME);
}

fn session_id() -> u32 {
    #[cfg(windows)]
    {
        // SAFETY: both calls only write to our local `session`.
        unsafe {
            let mut session = 0u32;
            if ProcessIdToSessionId(GetCurrentProcessId(), &mut session) != 0 {
                return session;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_bounds_scales_signed_virtual_desktop() {
        let bounds = Rect::new(-1920, 0, 0, 1080);
        assert_eq!(pixel_bounds(bounds, 1.5), Rect::new(-2880, 0, 0, 1620));
        assert_eq!(pixel_bounds(bounds, 0.0), bounds);
    }

    #[test]
    fn bar_rect_is_top_strip_at_dpi() {
        assert_eq!(
            bar_rect(Rect::new(0, 0, 1920, 1080), 1.5, 32.0),
            Rect::new(0, 0, 1920, 48)
        );
        assert_eq!(
            bar_rect(Rect::new(-1920, -500, 0, 580), 1.0, 32.0),
            Rect::new(-1920, -500, 0, -468)
        );
    }

    #[test]
    fn query_position_is_pinned_to_monitor() {
        let monitor = Rect::new(-1920, 0, 0, 1080);
        let requested = Rect::new(-3000, 0, 100, 48);
        assert_eq!(pin_to_edge(requested, monitor), Rect::new(-1920, 0, 0, 48));
    }

    #[test]
    fn fullscreen_geometry_requires_all_edges() {
        let monitor = Rect::new(0, 0, 1920, 1080);
        assert!(covers_monitor(
            Rect::new(0, 0, 1920, 1080),
            monitor,
            FULLSCREEN_TOLERANCE
        ));
        assert!(covers_monitor(
            Rect::new(-1, -2, 1921, 1081),
            monitor,
            FULLSCREEN_TOLERANCE
        ));
        assert!(!covers_monitor(
            Rect::new(0, 0, 1900, 1080),
            monitor,
            FULLSCREEN_TOLERANCE
        ));
    }

    #[test]
    fn clock_uses_culture_specific_format() {
        assert_eq!(format_clock(0, 5, "en-US"), "12:05 AM");
        assert_eq!(format_clock(13, 45, "en-US"), "1:45 PM");
        assert_eq!(format_clock(13, 45, "tr-TR"), "13:45");
    }

    #[test]
    fn reconcile_opens_closes_and_hides() {
        let mut tracked = Vec::new();
        let monitors = [MonitorSnapshot {
            key: 1,
            bounds: Rect::new(0, 0, 1920, 1080),
        }];
        let events = reconcile(&mut tracked, &monitors, &HashSet::new());
        assert_eq!(
            events,
            vec![
                BarEvent::Open { key: 1 },
                BarEvent::SetHidden {
                    key: 1,
                    hidden: false
                }
            ]
        );
        let hidden = HashSet::from([1]);
        assert_eq!(
            reconcile(&mut tracked, &monitors, &hidden),
            vec![BarEvent::SetHidden {
                key: 1,
                hidden: true
            }]
        );
        assert_eq!(
            reconcile(&mut tracked, &[], &HashSet::new()),
            vec![BarEvent::Close { key: 1 }]
        );
    }

    #[test]
    fn menu_commands_follow_files_availability() {
        assert_eq!(menu_command(0, true), Some(BarCommand::OpenFiles));
        assert_eq!(menu_command(1, true), Some(BarCommand::CloseBar));
        assert_eq!(menu_command(0, false), Some(BarCommand::CloseBar));
        assert_eq!(menu_command(2, true), None);
    }
}
