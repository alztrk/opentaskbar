//! Win32 bridge for GPUI dock windows: extracts `HWND` via `HasWindowHandle`,
//! applies `WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`, disables DWM
//! window rounding on transparent popups, and trims startup working set pages.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute};
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

const DWMWCP_DONOTROUND: u32 = 1;

pub fn configure_dock_window(window: &gpui::Window) -> Option<usize> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let hwnd = match handle.as_raw() {
        RawWindowHandle::Win32(win32) => win32.hwnd.get() as HWND,
        _ => return None,
    };
    // SAFETY: `hwnd` is a live top-level window owned by the current thread.
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

pub fn trim_working_set() {
    // SAFETY: pseudo handle for current process.
    unsafe {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}
