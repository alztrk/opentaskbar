//! Win32 bridge for OpenTaskBar dock windows:
//! - Window styles & Win11 DWM chrome stripping (borderless transparent popups)
//! - Multi-monitor enumeration with signed physical coordinates & per-monitor DPI (Issue #95)
//! - System dark/light theme detection via Windows Personalize registry key (Issue #88)
//! - Native context menus (Dock, Per-App, Task View), file drop extraction, and key modifiers

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_NCRENDERING_POLICY, DWMWA_WINDOW_CORNER_PREFERENCE,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
use windows_sys::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetSystemMetrics, GetWindowLongPtrW, HWND_TOPMOST, SM_CXSCREEN, SM_CYSCREEN,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW,
    SetWindowPos, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};
use windows_sys::core::BOOL;

const DWMWCP_DONOTROUND: u32 = 1;
const DWMNCRP_DISABLED: u32 = 1;
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
const MONITORINFOF_PRIMARY: u32 = 1;

const MENU_SEARCH: u32 = 999;
const MENU_FOCUS_MODE: u32 = 998;
const MENU_SETTINGS: u32 = 1000;
const MENU_TASKMGR: u32 = 1001;
const MENU_EXPLORER: u32 = 1002;
const MENU_TERMINAL: u32 = 1003;
const MENU_TERMINAL_ADMIN: u32 = 1004;
const MENU_RUN: u32 = 1005;
const MENU_SHOW_DESKTOP: u32 = 1006;
const MENU_RESTORE: u32 = 1007;
const MENU_EXIT: u32 = 1008;
const MENU_DEV_MGMT: u32 = 1009;
const MENU_DISK_MGMT: u32 = 1010;
const MENU_COMP_MGMT: u32 = 1011;
const MENU_EVENT_VWR: u32 = 1012;
const MENU_NET_CONN: u32 = 1013;
const MENU_LOCK_PC: u32 = 1014;
const MENU_SIGN_OUT: u32 = 1015;
const MENU_RESTART_PC: u32 = 1016;
const MENU_SHUTDOWN_PC: u32 = 1017;

const APP_MENU_OPEN: u32 = 2001;
const APP_MENU_NEW_WINDOW: u32 = 2002;
const APP_MENU_ADMIN: u32 = 2003;
const APP_MENU_TOGGLE_PIN: u32 = 2004;
const APP_MENU_CLOSE: u32 = 2005;
const APP_MENU_TERMINATE: u32 = 2006;

const TV_MENU_PREV: u32 = 3001;
const TV_MENU_NEXT: u32 = 3002;
const TV_MENU_NEW: u32 = 3003;
const TV_MENU_VIEW: u32 = 3004;

const BIN_MENU_OPEN: u32 = 4001;
const BIN_MENU_EMPTY: u32 = 4002;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecycleBinAction {
    None,
    Open,
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextAction {
    None,
    Search,
    ToggleFocusMode,
    Settings,
    TaskManager,
    FileExplorer,
    Terminal,
    TerminalAdmin,
    RunDialog,
    DeviceManager,
    DiskManagement,
    ComputerManagement,
    EventViewer,
    NetworkConnections,
    LockWorkstation,
    SignOut,
    RestartComputer,
    ShutDownComputer,
    ShowDesktop,
    RestoreTaskbar,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppContextAction {
    None,
    Open,
    NewWindow,
    RunAsAdmin,
    TogglePin,
    Close,
    Terminate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskViewAction {
    None,
    PreviousDesktop,
    NextDesktop,
    NewDesktop,
    TaskView,
}

#[derive(Clone, Copy, Debug)]
pub struct MonitorInfo {
    pub handle: usize,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub dpi_scale: f32,
    pub is_primary: bool,
}

impl MonitorInfo {
    pub fn width(self) -> i32 {
        (self.right - self.left).max(1)
    }

    pub fn height(self) -> i32 {
        (self.bottom - self.top).max(1)
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Enumerates all active monitors with signed physical coordinates and per-monitor DPI.
/// Primary monitor is always placed first; supports negative coordinates (left-of-primary monitors).
pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    unsafe extern "system" fn enum_cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        // SAFETY: `lparam` points to `Vec<MonitorInfo>` alive for the duration of `EnumDisplayMonitors`.
        unsafe {
            let list = &mut *(lparam as *mut Vec<MonitorInfo>);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(hmon, &mut info) != 0 {
                let mut dpi_x = 96u32;
                let mut dpi_y = 96u32;
                let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
                let dpi_scale = if dpi_x == 0 { 1.0 } else { dpi_x as f32 / 96.0 };
                list.push(MonitorInfo {
                    handle: hmon as usize,
                    left: info.rcMonitor.left,
                    top: info.rcMonitor.top,
                    right: info.rcMonitor.right,
                    bottom: info.rcMonitor.bottom,
                    dpi_scale,
                    is_primary: (info.dwFlags & MONITORINFOF_PRIMARY) != 0,
                });
            }
            1
        }
    }

    let mut monitors: Vec<MonitorInfo> = Vec::with_capacity(4);
    // SAFETY: synchronous Win32 monitor enumeration.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(enum_cb),
            (&mut monitors as *mut Vec<MonitorInfo>) as LPARAM,
        );
    }
    if monitors.is_empty() {
        let (w, h) = primary_screen_px();
        monitors.push(MonitorInfo {
            handle: 0,
            left: 0,
            top: 0,
            right: w,
            bottom: h,
            dpi_scale: 1.0,
            is_primary: true,
        });
    } else {
        monitors.sort_by_key(|m| !m.is_primary);
    }
    monitors
}

/// Reads Windows' `AppsUseLightTheme` registry value (returns `true` for Dark mode, fixing Issue #88).
pub fn read_system_is_dark() -> bool {
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut data: u32 = 0;
    let mut size: u32 = size_of::<u32>() as u32;
    // SAFETY: reading a single DWORD from HKCU.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    if status == 0 { data == 0 } else { true }
}

pub fn is_shift_down() -> bool {
    unsafe { (GetKeyState(i32::from(VK_SHIFT)) as u16 & 0x8000) != 0 }
}

pub fn is_ctrl_down() -> bool {
    unsafe { (GetKeyState(i32::from(VK_CONTROL)) as u16 & 0x8000) != 0 }
}

/// Extracts all file paths from a `WM_DROPFILES` `HDROP` handle and releases it.
pub fn extract_dropped_files(hdrop_val: usize) -> Vec<String> {
    let hdrop = hdrop_val as HDROP;
    if hdrop.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // SAFETY: querying dropped file count and paths before calling `DragFinish`.
    unsafe {
        let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
            if len > 0 {
                let mut buf = vec![0u16; (len + 1) as usize];
                let copied = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
                if copied > 0 {
                    out.push(String::from_utf16_lossy(&buf[..copied as usize]));
                }
            }
        }
        DragFinish(hdrop);
    }
    out
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

pub fn reposition_dock_window(hwnd: usize, x: i32, y: i32, w: i32, h: i32) {
    if hwnd == 0 || w <= 0 || h <= 0 {
        return;
    }
    // SAFETY: moving/resizing our own dock window without changing Z-order or stealing focus.
    unsafe {
        SetWindowPos(
            hwnd as HWND,
            std::ptr::null_mut(),
            x,
            y,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

use crate::ui::menu::{self, MenuEntry, MenuIcon};

pub fn show_dock_context_menu(hwnd: usize, is_dark: bool, focus_mode: bool) -> ContextAction {
    let entries = vec![
        MenuEntry::item(MENU_SEARCH, MenuIcon::Play, "Search / Launcher"),
        MenuEntry::item(
            MENU_FOCUS_MODE,
            MenuIcon::Layers,
            if focus_mode {
                "Focus Mode (On)"
            } else {
                "Focus Mode"
            },
        ),
        MenuEntry::Separator,
        MenuEntry::item(MENU_TASKMGR, MenuIcon::Activity, "Task Manager"),
        MenuEntry::item(MENU_EXPLORER, MenuIcon::Folder, "File Explorer"),
        MenuEntry::item(MENU_RUN, MenuIcon::Play, "Run..."),
        MenuEntry::item(MENU_TERMINAL, MenuIcon::Terminal, "Terminal"),
        MenuEntry::item(MENU_TERMINAL_ADMIN, MenuIcon::Shield, "Terminal (Admin)"),
        MenuEntry::Separator,
        MenuEntry::item(MENU_DEV_MGMT, MenuIcon::Settings, "Device Manager"),
        MenuEntry::item(MENU_DISK_MGMT, MenuIcon::Layers, "Disk Management"),
        MenuEntry::item(MENU_COMP_MGMT, MenuIcon::Monitor, "Computer Management"),
        MenuEntry::item(MENU_EVENT_VWR, MenuIcon::Activity, "Event Viewer"),
        MenuEntry::item(MENU_NET_CONN, MenuIcon::Refresh, "Network Connections"),
        MenuEntry::Separator,
        MenuEntry::item(MENU_LOCK_PC, MenuIcon::Shield, "Lock"),
        MenuEntry::item(MENU_SIGN_OUT, MenuIcon::ChevronRight, "Sign Out"),
        MenuEntry::item(MENU_RESTART_PC, MenuIcon::Refresh, "Restart"),
        MenuEntry::destructive(MENU_SHUTDOWN_PC, MenuIcon::Power, "Shut Down"),
        MenuEntry::Separator,
        MenuEntry::item(MENU_SETTINGS, MenuIcon::Settings, "Settings..."),
        MenuEntry::item(MENU_SHOW_DESKTOP, MenuIcon::Monitor, "Show Desktop"),
        MenuEntry::item(MENU_RESTORE, MenuIcon::Refresh, "Restore Windows Taskbar"),
        MenuEntry::Separator,
        MenuEntry::destructive(MENU_EXIT, MenuIcon::Power, "Exit OpenTaskbar"),
    ];
    match menu::show_popup_menu(hwnd, is_dark, entries).unwrap_or(0) {
        MENU_SEARCH => ContextAction::Search,
        MENU_FOCUS_MODE => ContextAction::ToggleFocusMode,
        MENU_SETTINGS => ContextAction::Settings,
        MENU_TASKMGR => ContextAction::TaskManager,
        MENU_EXPLORER => ContextAction::FileExplorer,
        MENU_TERMINAL => ContextAction::Terminal,
        MENU_TERMINAL_ADMIN => ContextAction::TerminalAdmin,
        MENU_RUN => ContextAction::RunDialog,
        MENU_DEV_MGMT => ContextAction::DeviceManager,
        MENU_DISK_MGMT => ContextAction::DiskManagement,
        MENU_COMP_MGMT => ContextAction::ComputerManagement,
        MENU_EVENT_VWR => ContextAction::EventViewer,
        MENU_NET_CONN => ContextAction::NetworkConnections,
        MENU_LOCK_PC => ContextAction::LockWorkstation,
        MENU_SIGN_OUT => ContextAction::SignOut,
        MENU_RESTART_PC => ContextAction::RestartComputer,
        MENU_SHUTDOWN_PC => ContextAction::ShutDownComputer,
        MENU_SHOW_DESKTOP => ContextAction::ShowDesktop,
        MENU_RESTORE => ContextAction::RestoreTaskbar,
        MENU_EXIT => ContextAction::Exit,
        _ => ContextAction::None,
    }
}

pub fn show_app_context_menu(
    hwnd: usize,
    is_dark: bool,
    app_name: &str,
    pinned: bool,
    running: bool,
) -> AppContextAction {
    let mut entries = vec![
        MenuEntry::item(APP_MENU_OPEN, MenuIcon::Play, app_name),
        MenuEntry::with_shortcut(
            APP_MENU_NEW_WINDOW,
            MenuIcon::Plus,
            "New Window",
            "Shift+Click",
        ),
        MenuEntry::with_shortcut(
            APP_MENU_ADMIN,
            MenuIcon::Shield,
            "Run as Admin",
            "Ctrl+Shift",
        ),
        MenuEntry::Separator,
        MenuEntry::item(
            APP_MENU_TOGGLE_PIN,
            MenuIcon::Pin,
            if pinned {
                "Unpin from Dock"
            } else {
                "Pin to Dock"
            },
        ),
    ];
    if running {
        entries.push(MenuEntry::Separator);
        entries.push(MenuEntry::item(
            APP_MENU_CLOSE,
            MenuIcon::Close,
            "Close Window",
        ));
        entries.push(MenuEntry::destructive(
            APP_MENU_TERMINATE,
            MenuIcon::Power,
            "Terminate Process",
        ));
    }
    match menu::show_popup_menu(hwnd, is_dark, entries).unwrap_or(0) {
        APP_MENU_OPEN => AppContextAction::Open,
        APP_MENU_NEW_WINDOW => AppContextAction::NewWindow,
        APP_MENU_ADMIN => AppContextAction::RunAsAdmin,
        APP_MENU_TOGGLE_PIN => AppContextAction::TogglePin,
        APP_MENU_CLOSE => AppContextAction::Close,
        APP_MENU_TERMINATE => AppContextAction::Terminate,
        _ => AppContextAction::None,
    }
}

pub fn show_taskview_context_menu(hwnd: usize, is_dark: bool) -> TaskViewAction {
    let entries = vec![
        MenuEntry::item(TV_MENU_PREV, MenuIcon::ChevronLeft, "Previous Desktop"),
        MenuEntry::item(TV_MENU_NEXT, MenuIcon::ChevronRight, "Next Desktop"),
        MenuEntry::item(TV_MENU_NEW, MenuIcon::Plus, "New Desktop"),
        MenuEntry::Separator,
        MenuEntry::item(TV_MENU_VIEW, MenuIcon::Layers, "Task View"),
    ];
    match menu::show_popup_menu(hwnd, is_dark, entries).unwrap_or(0) {
        TV_MENU_PREV => TaskViewAction::PreviousDesktop,
        TV_MENU_NEXT => TaskViewAction::NextDesktop,
        TV_MENU_NEW => TaskViewAction::NewDesktop,
        TV_MENU_VIEW => TaskViewAction::TaskView,
        _ => TaskViewAction::None,
    }
}

pub fn show_recycle_bin_context_menu(hwnd: usize, is_dark: bool) -> RecycleBinAction {
    let entries = vec![
        MenuEntry::item(BIN_MENU_OPEN, MenuIcon::Folder, "Open Recycle Bin"),
        MenuEntry::Separator,
        MenuEntry::destructive(BIN_MENU_EMPTY, MenuIcon::Trash, "Empty Recycle Bin"),
    ];
    match menu::show_popup_menu(hwnd, is_dark, entries).unwrap_or(0) {
        BIN_MENU_OPEN => RecycleBinAction::Open,
        BIN_MENU_EMPTY => RecycleBinAction::Empty,
        _ => RecycleBinAction::None,
    }
}

pub fn trim_working_set() {
    // SAFETY: pseudo handle for current process.
    unsafe {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}

/// Checks whether OpenTaskBar is registered under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
pub fn is_startup_enabled() -> bool {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "OpenTaskbar\0".encode_utf16().collect();
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return false;
        }
        let mut size = 0u32;
        let ok = RegQueryValueExW(
            hkey,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) == 0
            && size > 2;
        RegCloseKey(hkey);
        ok
    }
}

/// Enables or disables automatic launch at Windows sign-in (`HKCU\...\Run`).
pub fn set_startup_enabled(enabled: bool) {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
        RegOpenKeyExW, RegSetValueExW,
    };
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "OpenTaskbar\0".encode_utf16().collect();
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        ) != 0
        {
            return;
        }
        if enabled {
            if let Ok(exe) = std::env::current_exe() {
                let cmd: Vec<u16> = format!("\"{}\"\0", exe.display()).encode_utf16().collect();
                let _ = RegSetValueExW(
                    hkey,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    cmd.as_ptr().cast(),
                    (cmd.len() * 2) as u32,
                );
            }
        } else {
            let _ = RegDeleteValueW(hkey, name.as_ptr());
        }
        RegCloseKey(hkey);
    }
}
