//! Win32 bridge for GPUI dock windows:
//! - Extracts `HWND` via `HasWindowHandle`
//! - Applies `WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`
//! - Disables Win11 DWM border (`DWMWA_BORDER_COLOR = DWMWA_COLOR_NONE`),
//!   non-client rendering, and native rounding so transparent popups never show a gray frame
//! - Dynamically resizes/positions the dock window and shows the native context menu

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_NCRENDERING_POLICY, DWMWA_WINDOW_CORNER_PREFERENCE,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GWL_EXSTYLE, GetCursorPos, GetSystemMetrics,
    GetWindowLongPtrW, HWND_TOPMOST, MF_SEPARATOR, MF_STRING, SM_CXSCREEN, SM_CYSCREEN,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenu,
    WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

const DWMWCP_DONOTROUND: u32 = 1;
const DWMNCRP_DISABLED: u32 = 1;
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

const MENU_TASKMGR: u32 = 1001;
const MENU_RESTORE: u32 = 1002;
const MENU_EXIT: u32 = 1003;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextAction {
    None,
    TaskManager,
    RestoreTaskbar,
    Exit,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[allow(dead_code)]
pub fn configure_dock_window(window: &gpui::Window) -> Option<usize> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let hwnd = match handle.as_raw() {
        RawWindowHandle::Win32(win32) => win32.hwnd.get() as HWND,
        _ => return None,
    };
    configure_native_dock_hwnd(hwnd as usize);
    Some(hwnd as usize)
}

pub fn configure_native_dock_hwnd(hwnd_val: usize) {
    if hwnd_val == 0 {
        return;
    }
    let hwnd = hwnd_val as HWND;
    // SAFETY: `hwnd` is a live top-level window owned by the current thread.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as usize;
        let next = (ex & !(WS_EX_APPWINDOW as usize))
            | (WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE) as usize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next as isize);

        let ncrp = DWMNCRP_DISABLED;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY as u32,
            (&ncrp as *const u32).cast(),
            size_of::<u32>() as u32,
        );

        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const u32).cast(),
            size_of::<u32>() as u32,
        );

        // Removes the 1px gray Win11 border drawn around borderless popup windows.
        let border_none = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            (&border_none as *const u32).cast(),
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
}

pub fn dpi_scale(hwnd: usize) -> f32 {
    if hwnd == 0 {
        return 1.0;
    }
    // SAFETY: querying DPI for our own live window.
    let dpi = unsafe { GetDpiForWindow(hwnd as HWND) };
    if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
}

pub fn primary_screen_px() -> (i32, i32) {
    // SAFETY: querying primary monitor physical dimensions.
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

/// Repositions and resizes the dock window asynchronously without stealing focus,
/// overriding Z-order, or triggering synchronous WM_SIZE re-entrancy inside GPUI's borrow.
pub fn reposition_dock_window(hwnd: usize, x: i32, y: i32, w: i32, h: i32) {
    if hwnd == 0 || w <= 0 || h <= 0 {
        return;
    }
    const SWP_ASYNCWINDOWPOS: u32 = 0x4000;
    // SAFETY: moving/resizing our own dock window without changing Z-order.
    unsafe {
        SetWindowPos(
            hwnd as HWND,
            std::ptr::null_mut(),
            x,
            y,
            w,
            h,
            windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                | SWP_NOACTIVATE
                | SWP_ASYNCWINDOWPOS,
        );
    }
}

/// Displays a native Win32 context menu at the cursor position.
pub fn show_dock_context_menu(hwnd: usize) -> ContextAction {
    if hwnd == 0 {
        return ContextAction::None;
    }
    // SAFETY: standard Win32 popup menu owned by our dock window.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return ContextAction::None;
        }
        let tm = wide("Task Manager");
        let rs = wide("Restore Windows Taskbar");
        let ex = wide("Exit OpenTaskbar");
        AppendMenuW(menu, MF_STRING, MENU_TASKMGR as usize, tm.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_RESTORE as usize, rs.as_ptr());
        AppendMenuW(menu, MF_STRING, MENU_EXIT as usize, ex.as_ptr());

        let mut pt: POINT = std::mem::zeroed();
        GetCursorPos(&mut pt);
        // SetForegroundWindow ensures clicking outside dismisses the popup menu cleanly.
        SetForegroundWindow(hwnd as HWND);
        let cmd = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
            pt.x,
            pt.y,
            0,
            hwnd as HWND,
            std::ptr::null(),
        ) as u32;
        DestroyMenu(menu);

        match cmd {
            MENU_TASKMGR => ContextAction::TaskManager,
            MENU_RESTORE => ContextAction::RestoreTaskbar,
            MENU_EXIT => ContextAction::Exit,
            _ => ContextAction::None,
        }
    }
}

pub fn trim_working_set() {
    // SAFETY: pseudo handle for current process.
    unsafe {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}
