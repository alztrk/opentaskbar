//! Minimal Win32 running-window enumeration and activation helpers.

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SW_MINIMIZE, SW_RESTORE, SetForegroundWindow, ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
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

struct EnumCtx {
    self_pid: u32,
    foreground: HWND,
    out: Vec<RunningWindow>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` points to `EnumCtx` alive for the duration of `EnumWindows`.
    unsafe {
        let ctx = &mut *(lparam as *mut EnumCtx);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if (ex & WS_EX_TOOLWINDOW) != 0 && (ex & WS_EX_APPWINDOW) == 0 {
            return 1;
        }
        let owner = GetWindow(hwnd, 4); // GW_OWNER
        if !owner.is_null() && (ex & WS_EX_APPWINDOW) == 0 {
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

pub fn toggle_window(hwnd: usize) {
    let hwnd = hwnd as HWND;
    // SAFETY: standard Win32 activation calls on a tracked window.
    unsafe {
        if GetForegroundWindow() == hwnd && IsIconic(hwnd) == 0 {
            ShowWindow(hwnd, SW_MINIMIZE);
        } else {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
    }
}
