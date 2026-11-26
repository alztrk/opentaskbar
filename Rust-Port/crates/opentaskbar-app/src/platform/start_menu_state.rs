//! Tracks whether the Windows Start menu is open. A closed Start menu often
//! stays the foreground window but is cloaked by DWM, so the open state follows
//! its `EVENT_OBJECT_CLOAKED` / `EVENT_OBJECT_UNCLOAKED` events as well as
//! foreground changes.
//!
//! C4 API: `is_open()` and `install_hook()`. A Start button press within a
//! 60–150 ms grace after a cloak counts as dismissing the menu rather than
//! reopening it (`WasOpenAtPress` in C#).

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
};

use opentaskbar_core::log;

const EVENT_OBJECT_CLOAKED: u32 = 0x8017;
const EVENT_OBJECT_UNCLOAKED: u32 = 0x8018;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
const OBJID_WINDOW: i32 = 0;
const START_MENU_EXE: &str = "startmenuexperiencehost.exe";

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

#[derive(Default)]
struct Inner {
    is_open: bool,
    closed_at: Option<Instant>,
    start_window: usize,
    cached_pid: u32,
    cached_is_start_menu: bool,
}

static STATE: Mutex<Inner> = Mutex::new(Inner {
    is_open: false,
    closed_at: None,
    start_window: 0,
    cached_pid: 0,
    cached_is_start_menu: false,
});
static HOOK: AtomicIsize = AtomicIsize::new(0);

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

fn is_start_menu_process(hwnd: HWND) -> bool {
    let mut pid = 0u32;
    // SAFETY: querying the PID of a live window supplied by the hook.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return false;
    }
    if let Ok(state) = STATE.lock()
        && state.cached_pid == pid
    {
        return state.cached_is_start_menu;
    }
    let is_start_menu =
        process_exe(pid).is_some_and(|exe| exe.to_ascii_lowercase().ends_with(START_MENU_EXE));
    if let Ok(mut state) = STATE.lock() {
        state.cached_pid = pid;
        state.cached_is_start_menu = is_start_menu;
    }
    is_start_menu
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

fn mark_open() {
    if let Ok(mut state) = STATE.lock() {
        state.is_open = true;
        state.closed_at = None;
    }
}

fn mark_closed() {
    if let Ok(mut state) = STATE.lock()
        && state.is_open
    {
        state.is_open = false;
        state.closed_at = Some(Instant::now());
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
    // A panic must never cross the unmanaged WinEvent boundary.
    if object_id != OBJID_WINDOW || hwnd.is_null() || !is_start_menu_process(hwnd) {
        return;
    }
    if event_type == EVENT_OBJECT_UNCLOAKED {
        let mut rect: RECT = unsafe { std::mem::zeroed() };
        // SAFETY: reading the rect of a live window supplied by the OS.
        let got_rect = unsafe { GetWindowRect(hwnd, &mut rect) != 0 };
        if !got_rect || rect.right - rect.left <= 100 || rect.bottom - rect.top <= 100 {
            return;
        }
        if let Ok(mut state) = STATE.lock() {
            state.start_window = hwnd as usize;
        }
        mark_open();
        return;
    }
    if event_type == EVENT_OBJECT_CLOAKED {
        let matches_cached = STATE
            .lock()
            .is_ok_and(|state| state.start_window == hwnd as usize);
        if matches_cached || !is_start_menu_open_now() {
            mark_closed();
        }
    }
}

/// Starts following the Start menu's cloak events. Call on the UI thread once
/// its message loop runs. Idempotent.
pub fn install_hook() {
    if HOOK.load(Ordering::SeqCst) != 0 {
        return;
    }
    // SAFETY: out-of-context hook with a callback whose body never panics.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_CLOAKED,
            EVENT_OBJECT_UNCLOAKED,
            std::ptr::null_mut(),
            Some(on_cloak_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if !hook.is_null() {
        HOOK.store(hook as isize, Ordering::SeqCst);
        log::info("Start menu cloak hook installed.");
    }
}

pub fn uninstall_hook() {
    let hook = HOOK.swap(0, Ordering::SeqCst);
    if hook == 0 {
        return;
    }
    // SAFETY: the hook handle was created by `install_hook` and is removed once.
    unsafe {
        UnhookWinEvent(hook as HANDLE);
    }
}

/// True while the dock should treat the Start menu as open.
pub fn is_open() -> bool {
    STATE.lock().is_ok_and(|state| state.is_open)
}

pub fn on_foreground_changed(is_start_menu: bool) {
    if is_start_menu {
        mark_open();
    } else {
        mark_closed();
    }
}

/// The window hook skips our own process, so a right-click activating the dock
/// is the only foreground sign that the Start menu lost focus to us.
pub fn on_dock_activated() {
    mark_closed();
}

pub fn reset() {
    if let Ok(mut state) = STATE.lock() {
        state.is_open = false;
        state.closed_at = None;
    }
}

/// Returns the signed millisecond delta used by the press grace window.
pub fn closed_delta_ms(closed_at: Instant, press: Instant) -> i128 {
    if closed_at >= press {
        (closed_at - press).as_millis() as i128
    } else {
        -((press - closed_at).as_millis() as i128)
    }
}

/// True when a Start button press should close rather than reopen the menu.
/// Grace is scoped to dismissal within the immediate pointer press gesture.
pub fn was_open_at_press(open_now: bool, press: Instant) -> bool {
    let (is_open, closed_at) = STATE
        .lock()
        .map(|state| (state.is_open, state.closed_at))
        .unwrap_or((false, None));
    was_open_at_press_impl(is_open, open_now, closed_at, press)
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
    let Some(closed_at) = closed_at else {
        return false;
    };
    let delta = closed_delta_ms(closed_at, press);
    (-60..=150).contains(&delta)
}

/// True while the Start menu is actually on screen (visible and not cloaked).
/// Alt+Tab takes the foreground while the menu stays open beneath the switcher,
/// so the cached Start window is checked as well.
pub fn is_start_menu_open_now() -> bool {
    // SAFETY: foreground window query.
    let foreground = unsafe { GetForegroundWindow() };
    if !foreground.is_null() && is_start_menu_process(foreground) && is_on_screen(foreground) {
        if let Ok(mut state) = STATE.lock() {
            state.start_window = foreground as usize;
        }
        return true;
    }
    let cached = STATE.lock().map(|s| s.start_window).unwrap_or(0);
    if cached != 0 {
        let hwnd = cached as HWND;
        // SAFETY: cached window is re-validated by process name and visibility.
        if is_start_menu_process(hwnd) && is_on_screen(hwnd) {
            return true;
        }
    }
    !find_start_menu_windows().is_empty()
}

/// Enumerates on-screen Start menu windows owned by StartMenuExperienceHost.
pub fn find_start_menu_windows() -> Vec<usize> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` points at a Vec alive for the synchronous enumeration.
        unsafe {
            let output = &mut *(lparam as *mut Vec<usize>);
            let mut rect: RECT = std::mem::zeroed();
            if is_start_menu_process(hwnd)
                && is_on_screen(hwnd)
                && GetWindowRect(hwnd, &mut rect) != 0
                && rect.right - rect.left > 100
                && rect.bottom - rect.top > 100
            {
                output.push(hwnd as usize);
            }
            1
        }
    }
    let mut windows = Vec::new();
    // SAFETY: synchronous enumeration with an owned, live context.
    unsafe {
        EnumWindows(Some(visit), (&mut windows as *mut Vec<usize>) as LPARAM);
    }
    windows
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
        assert!(was_open_at_press_impl(
            false,
            false,
            Some(press + Duration::from_millis(60)),
            press
        ));
        assert!(was_open_at_press_impl(
            false,
            false,
            Some(press + Duration::from_millis(150)),
            press
        ));
        assert!(!was_open_at_press_impl(
            false,
            false,
            Some(press + Duration::from_millis(151)),
            press
        ));
        assert!(was_open_at_press_impl(
            false,
            false,
            Some(press - Duration::from_millis(60)),
            press
        ));
        assert!(!was_open_at_press_impl(
            false,
            false,
            Some(press - Duration::from_millis(61)),
            press
        ));
    }
}
