//! Locates Explorer's taskbar and secondary trays without ever mistaking an
//! OpenTaskbar-owned fake `Shell_TrayWnd` for the real thing.
//!
//! Every returned handle is wrapped in a re-validated lease: a candidate can be
//! destroyed and its HWND recycled between `FindWindowExW` and the PID query, so
//! recovery code must verify liveness and ownership immediately before acting.
//! Creating a live tray host stays behind `enable_live_tray_host` (default off);
//! this module only *finds and leases* windows, it never creates one.

use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GetClassNameW, GetWindowThreadProcessId, IsWindow,
};

const TRAY_WINDOW_CLASS: &str = "Shell_TrayWnd";
const SECONDARY_TRAY_WINDOW_CLASS: &str = "Shell_SecondaryTrayWnd";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn process_id(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: querying the PID of a live window handle.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 64];
    // SAFETY: bounded UTF-16 buffer with explicit length.
    let length = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..length as usize])
}

/// A validated handle to a tray window owned by a process other than ours.
#[derive(Clone, Copy, Debug)]
pub struct TaskbarLease {
    pub hwnd: usize,
    pub pid: u32,
}

impl TaskbarLease {
    /// Re-validates liveness and ownership. False means the HWND was recycled or
    /// destroyed, so callers must discard it and re-discover the taskbar.
    pub fn is_valid(&self) -> bool {
        if self.hwnd == 0 || self.pid == 0 || self.pid == std::process::id() {
            return false;
        }
        let hwnd = self.hwnd as HWND;
        // SAFETY: HWND came from an earlier validated enumeration.
        unsafe { IsWindow(hwnd) != 0 && process_id(hwnd) == self.pid }
    }

    pub fn capture(hwnd: HWND) -> Option<Self> {
        if hwnd.is_null() {
            return None;
        }
        let pid = process_id(hwnd);
        let lease = Self {
            hwnd: hwnd as usize,
            pid,
        };
        lease.is_valid().then_some(lease)
    }
}

/// Explorer's primary taskbar, skipping any `Shell_TrayWnd` owned by this process.
pub fn find() -> Option<TaskbarLease> {
    let class = wide(TRAY_WINDOW_CLASS);
    let mut candidate: HWND = std::ptr::null_mut();
    // SAFETY: top-level window iteration with null-terminated class name.
    unsafe {
        loop {
            candidate = FindWindowExW(
                std::ptr::null_mut(),
                candidate,
                class.as_ptr(),
                std::ptr::null(),
            );
            if candidate.is_null() {
                return None;
            }
            if let Some(lease) = TaskbarLease::capture(candidate) {
                return Some(lease);
            }
        }
    }
}

/// Acquires a re-validated lease. False means Explorer's taskbar is absent or
/// was replaced mid-scan (cold boot / shell restart).
pub fn try_acquire() -> Option<TaskbarLease> {
    find().filter(TaskbarLease::is_valid)
}

/// Re-validated secondary taskbars; primary tray visibility alone is not enough
/// to confirm that Explorer's full taskbar is back after a restart.
pub fn find_secondary_leases() -> Vec<TaskbarLease> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` points at a Vec alive for the synchronous enumeration.
        unsafe {
            let output = &mut *(lparam as *mut Vec<TaskbarLease>);
            if class_name(hwnd).eq_ignore_ascii_case(SECONDARY_TRAY_WINDOW_CLASS)
                && let Some(lease) = TaskbarLease::capture(hwnd)
            {
                output.push(lease);
            }
            1
        }
    }

    let mut leases = Vec::new();
    // SAFETY: synchronous Win32 enumeration; the Vec outlives the callback.
    unsafe {
        EnumWindows(
            Some(visit),
            (&mut leases as *mut Vec<TaskbarLease>) as LPARAM,
        );
    }
    leases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_rejects_our_own_process_and_null_handles() {
        let ours = TaskbarLease {
            hwnd: 1,
            pid: std::process::id(),
        };
        assert!(!ours.is_valid());
        let null = TaskbarLease { hwnd: 0, pid: 4 };
        assert!(!null.is_valid());
    }
}
