//! Win32 bridge for GPUI windows: extracts the native `HWND`, applies dock
//! extended styles (`WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`),
//! disables native DWM rounding on transparent surfaces so GPUI owns the pill
//! silhouette, and queries process private memory.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute};
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

const DWMWCP_DONOTROUND: u32 = 1;

/// Extracts the Win32 `HWND` from any GPUI `Window`.
pub fn hwnd_from_window(window: &gpui::Window) -> Option<HWND> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get() as HWND),
        _ => None,
    }
}

/// Applies floating-dock Win32 extended styles to a GPUI popup window so it
/// stays above normal windows, never steals focus, and never appears in Alt-Tab
/// or the Windows taskbar. Returns the raw `HWND` value if available.
pub fn configure_dock_window(window: &gpui::Window) -> Option<usize> {
    let hwnd = hwnd_from_window(window)?;
    // SAFETY: `hwnd` comes from `window`'s live Win32 handle on the current thread.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as usize;
        let next = (ex & !(WS_EX_APPWINDOW as usize))
            | (WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE) as usize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next as isize);

        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const u32).cast(),
            size_of::<u32>() as u32,
        );

        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    Some(hwnd as usize)
}

/// Returns the current process private usage (commit charge) in MiB.
pub fn process_private_memory_mib() -> Option<f64> {
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    // SAFETY: `counters` is properly sized and owned by this stack frame.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            counters.cb,
        )
    };
    (ok != 0).then_some(counters.PrivateUsage as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_nonzero_private_memory() {
        let mib = process_private_memory_mib().expect("memory query should succeed");
        assert!(mib > 0.0, "expected positive private memory, got {mib}");
    }
}
