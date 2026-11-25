//! Win32 running-window enumeration with DWM cloaked filtering and activation helpers.

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW,
    TerminateProcess,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, PostMessageW, SW_MINIMIZE, SW_RESTORE, SetForegroundWindow, ShowWindow,
    WM_CLOSE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows_sys::core::BOOL;

#[derive(Clone, Debug)]
pub struct RunningWindow {
    pub hwnd: usize,
    pub pid: u32,
    pub title: String,
    pub exe_path: String,
    pub is_foreground: bool,
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: querying DWMWA_CLOAKED into a local u32.
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

fn has_app_window_style(hwnd: HWND) -> bool {
    // SAFETY: The HWND is supplied by EnumWindows on the caller's thread.
    let (style, owner) = unsafe {
        (
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32,
            GetWindow(hwnd, GW_OWNER),
        )
    };
    style & WS_EX_NOACTIVATE == 0
        && ((style & WS_EX_TOOLWINDOW == 0 && owner.is_null()) || style & WS_EX_APPWINDOW != 0)
}

fn process_image_path(pid: u32) -> Option<String> {
    // SAFETY: limited query handle closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        (ok != 0 && len > 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: reading window title into buffer.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if copied <= 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buf[..copied as usize])
        }
    }
}

fn is_shell_noise(exe_path: &str) -> bool {
    let lower = exe_path.to_lowercase();
    lower.ends_with(r"\textinputhost.exe")
        || lower.ends_with(r"\shellexperiencehost.exe")
        || lower.ends_with(r"\searchhost.exe")
        || lower.ends_with(r"\startmenuexperiencehost.exe")
        || lower.ends_with(r"\lockapp.exe")
}

struct EnumCtx {
    self_pid: u32,
    foreground: HWND,
    out: Vec<RunningWindow>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` points to `EnumCtx` alive for the duration of `EnumWindows`.
    unsafe {
        let ctx = &mut *(lparam as *mut EnumCtx);
        if IsWindowVisible(hwnd) == 0 || is_cloaked(hwnd) {
            return 1;
        }
        if !has_app_window_style(hwnd) {
            return 1;
        }
        let title = window_title(hwnd);
        if title.is_empty() {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 || pid == ctx.self_pid {
            return 1;
        }
        let Some(exe_path) = process_image_path(pid) else {
            return 1;
        };
        if is_shell_noise(&exe_path) {
            return 1;
        }
        ctx.out.push(RunningWindow {
            hwnd: hwnd as usize,
            pid,
            title,
            exe_path,
            is_foreground: hwnd == ctx.foreground,
        });
        1
    }
}

pub fn enumerate_windows() -> Vec<RunningWindow> {
    // SAFETY: `EnumWindows` invokes `enum_proc` synchronously on this thread.
    let foreground = unsafe { GetForegroundWindow() };
    let mut ctx = EnumCtx {
        self_pid: std::process::id(),
        foreground,
        out: Vec::with_capacity(24),
    };
    unsafe {
        EnumWindows(Some(enum_proc), (&mut ctx as *mut EnumCtx) as LPARAM);
    }
    ctx.out
}

/// Finds a visible or hidden application window for an already verified process.
pub fn find_app_window(pid: u32) -> Option<usize> {
    struct Search {
        pid: u32,
        hwnd: Option<usize>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: EnumWindows invokes this callback synchronously while Search is alive.
        unsafe {
            let search = &mut *(lparam as *mut Search);
            let mut owner_pid = 0;
            GetWindowThreadProcessId(hwnd, &mut owner_pid);
            if owner_pid != search.pid
                || GetWindowTextLengthW(hwnd) == 0
                || !has_app_window_style(hwnd)
                || is_cloaked(hwnd)
            {
                return 1;
            }
            if IsWindowVisible(hwnd) != 0 {
                search.hwnd = Some(hwnd as usize);
                return 0;
            }
            search.hwnd.get_or_insert(hwnd as usize);
            1
        }
    }

    let mut search = Search { pid, hwnd: None };
    // SAFETY: The local Search outlives every synchronous callback.
    unsafe { EnumWindows(Some(visit), (&mut search as *mut Search) as LPARAM) };
    search.hwnd
}

/// Activates and restores a specific window.
pub fn activate_window(hwnd_val: usize) {
    if hwnd_val == 0 {
        return;
    }
    let hwnd = hwnd_val as HWND;
    // SAFETY: standard Win32 activation calls on a live window handle.
    unsafe {
        if IsWindow(hwnd) == 0 {
            return;
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

/// Requests a graceful close (`WM_CLOSE`) for a specific window.
pub fn close_window(hwnd_val: usize) {
    if hwnd_val == 0 {
        return;
    }
    let hwnd = hwnd_val as HWND;
    // SAFETY: asynchronous `WM_CLOSE` message to a live window.
    unsafe {
        if IsWindow(hwnd) != 0 {
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
        }
    }
}

/// Forcefully terminates the process owning `hwnd_val`.
pub fn terminate_window_process(hwnd_val: usize) {
    if hwnd_val == 0 {
        return;
    }
    // SAFETY: queries PID and terminates only the target window's process.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd_val as HWND, &mut pid);
        if pid == 0 || pid == std::process::id() {
            return;
        }
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            let _ = TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

/// Cycles forward or backward through an app's open windows (e.g. mouse wheel on dock icon).
pub fn cycle_windows(hwnds: &[usize], forward: bool) {
    if hwnds.is_empty() {
        return;
    }
    let fg = unsafe { GetForegroundWindow() as usize };
    let next = match hwnds.iter().position(|&h| h == fg) {
        Some(pos) => {
            if forward {
                hwnds[(pos + 1) % hwnds.len()]
            } else {
                hwnds[(pos + hwnds.len() - 1) % hwnds.len()]
            }
        }
        None => hwnds[0],
    };
    activate_window(next);
}

/// Cycles or toggles activation for a group of windows belonging to one app.
/// Returns `true` if a live window was activated/toggled, or `false` if all HWNDs were stale
/// (so the caller can launch a fresh instance instead of doing nothing, fixing issue #173).
pub fn activate_or_toggle(hwnds: &[usize]) -> bool {
    let live: Vec<usize> = hwnds
        .iter()
        .copied()
        .filter(|&h| {
            let hwnd = h as HWND;
            unsafe { h != 0 && IsWindow(hwnd) != 0 && IsWindowVisible(hwnd) != 0 }
        })
        .collect();
    if live.is_empty() {
        return false;
    }
    // SAFETY: standard Win32 activation calls on verified live windows.
    unsafe {
        let fg = GetForegroundWindow() as usize;
        if live.len() == 1 {
            let hwnd = live[0] as HWND;
            if fg == live[0] && IsIconic(hwnd) == 0 {
                ShowWindow(hwnd, SW_MINIMIZE);
            } else {
                activate_window(live[0]);
            }
            return true;
        }
        cycle_windows(&live, true);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW,
    };
    use windows_sys::core::PCWSTR;
    use windows_sys::w;

    struct HiddenWindow(HWND);

    impl HiddenWindow {
        fn new(style: u32, title: PCWSTR) -> Self {
            // SAFETY: STATIC is a system window class. No window is shown or activated.
            let hwnd = unsafe {
                CreateWindowExW(
                    style,
                    w!("STATIC"),
                    title,
                    WS_OVERLAPPEDWINDOW,
                    0,
                    0,
                    200,
                    100,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null());
            Self(hwnd)
        }
    }

    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            // SAFETY: The same test thread owns the fixture window.
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn background_app_lookup_skips_utility_windows_and_finds_a_hidden_app_window() {
        let pid = std::process::id();
        let _utility = HiddenWindow::new(WS_EX_TOOLWINDOW, w!("Utility"));
        let _untitled = HiddenWindow::new(0, w!(""));
        let _no_activate = HiddenWindow::new(WS_EX_NOACTIVATE, w!("Notification"));
        assert_eq!(find_app_window(pid), None);

        let app = HiddenWindow::new(0, w!("Application window"));
        assert_eq!(find_app_window(pid), Some(app.0 as usize));
    }
}
