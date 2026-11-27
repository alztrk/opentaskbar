//! Win32 bridge for OpenTaskBar dock windows:
//! - Window styles & Win11 DWM chrome stripping (borderless transparent popups)
//! - Multi-monitor enumeration with signed physical coordinates & per-monitor DPI (Issue #95)
//! - System dark/light theme detection via Windows Personalize registry key (Issue #88)
//! - Native context menus (Dock, Per-App, Task View), file drop extraction, and key modifiers

use std::cell::RefCell;
use std::collections::HashMap;

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::DockSettings;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW,
    HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
};
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, VK_CONTROL, VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOTOWNER, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClassNameW,
    GetForegroundWindow, GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, HTCAPTION, HWND_TOPMOST, IsIconic, IsWindowVisible, SM_CXSCREEN,
    SM_CYSCREEN, SPI_GETCLIENTAREAANIMATION, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetWindowLongPtrW, SetWindowPos, SystemParametersInfoW,
    WM_NCLBUTTONDOWN, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};
use windows_sys::core::BOOL;

const DWMWCP_DONOTROUND: u32 = 1;
const DWMNCRP_DISABLED: u32 = 1;
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
const MONITORINFOF_PRIMARY: u32 = 1;

pub fn client_area_animations_enabled() -> bool {
    let mut enabled: BOOL = 1;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the supplied pointer.
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut BOOL).cast(),
            0,
        )
    };
    enabled != 0
}

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
const MENU_SLEEP_PC: u32 = 1018;
const MENU_NOTIFICATION_CENTER: u32 = 1019;

const MENU_PROFILE_APPLY_BASE: u32 = 6000;
const MENU_PROFILE_APPLY_MAX: u32 = 6199;
const MENU_PROFILE_SAVE_NEW: u32 = 6201;
const MENU_STACK_ADD_DOWNLOADS: u32 = 6301;
const MENU_STACK_ADD_DESKTOP: u32 = 6302;
const MENU_STACK_ADD_DOCUMENTS: u32 = 6303;
const MENU_STACK_REMOVE_BASE: u32 = 6400;
const MENU_STACK_REMOVE_MAX: u32 = 6499;

const APP_MENU_OPEN: u32 = 2001;
const APP_MENU_NEW_WINDOW: u32 = 2002;
const APP_MENU_ADMIN: u32 = 2003;
const APP_MENU_TOGGLE_PIN: u32 = 2004;
const APP_MENU_CLOSE: u32 = 2005;
const APP_MENU_TERMINATE: u32 = 2006;
const APP_MENU_GROUP_CREATE: u32 = 2100;
const APP_MENU_GROUP_TOGGLE_BASE: u32 = 2200;
const APP_MENU_GROUP_TOGGLE_MAX: u32 = 2299;
const JUMPLIST_ID_BASE: u32 = 2300;
const JUMPLIST_ID_MAX: u32 = 2399;

thread_local! {
    static JUMPLIST_COMMANDS: RefCell<HashMap<u32, (i64, i32)>> = RefCell::new(HashMap::new());
}

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
pub enum WidgetContextAction {
    None,
    Secondary,
    Remove,
    PlaceAllDisplays,
    PlaceOnDisplay(usize),
    MoveToTaskbar,
    MoveEarlier,
    MoveLater,
    Dock(ContextAction),
}

#[allow(clippy::too_many_arguments)]
pub fn show_widget_context_menu(
    hwnd: usize,
    is_dark: bool,
    widget_id: &str,
    secondary_label: Option<&str>,
    monitors: &[MonitorInfo],
    in_island: bool,
    can_move_earlier: bool,
    can_move_later: bool,
    settings: &DockSettings,
) -> WidgetContextAction {
    const MENU_WIDGET_SECONDARY: u32 = 5001;
    const MENU_WIDGET_REMOVE: u32 = 5002;
    const MENU_ISLAND_ALL: u32 = 5100;
    const MENU_ISLAND_TASKBAR: u32 = 5101;
    const MENU_ISLAND_MOVE_EARLIER: u32 = 5102;
    const MENU_ISLAND_MOVE_LATER: u32 = 5103;
    const MENU_ISLAND_DISPLAY_BASE: u32 = 5200;
    const MENU_ISLAND_DISPLAY_MAX: u32 = 5299;

    let current_target = settings
        .dynamic_island_placements
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(widget_id))
        .map(|(_, v)| v.as_str());

    let mut placement_items = Vec::new();
    if !monitors.is_empty() {
        let all_checked = current_target.is_some_and(|t| {
            t.eq_ignore_ascii_case(opentaskbar_core::settings::DYNAMIC_ISLAND_ALL_DISPLAYS)
        });
        let all_label = if all_checked {
            format!("✓ {}", loc::t("DynamicIslandAllDisplays"))
        } else {
            loc::t("DynamicIslandAllDisplays")
        };
        placement_items.push(MenuEntry::item(
            MENU_ISLAND_ALL,
            MenuIcon::Monitor,
            all_label,
        ));
        placement_items.push(MenuEntry::Separator);
        for (idx, mon) in monitors.iter().enumerate().take(100) {
            let checked = current_target.is_some_and(|t| t.eq_ignore_ascii_case(&mon.device_name));
            let label = if checked {
                format!("✓ {}", mon.display_label())
            } else {
                mon.display_label()
            };
            placement_items.push(MenuEntry::item(
                MENU_ISLAND_DISPLAY_BASE + idx as u32,
                MenuIcon::Monitor,
                label,
            ));
        }
    }
    if current_target.is_some() {
        if !placement_items.is_empty() {
            placement_items.push(MenuEntry::Separator);
        }
        placement_items.push(MenuEntry::item(
            MENU_ISLAND_TASKBAR,
            MenuIcon::Layers,
            loc::t("DynamicIslandMoveToTaskbar"),
        ));
    }

    let mut entries = Vec::new();
    if !placement_items.is_empty() {
        entries.push(MenuEntry::submenu(
            MenuIcon::Monitor,
            loc::t("DynamicIslandShowOn"),
            placement_items,
        ));
    }
    if in_island && (can_move_earlier || can_move_later) {
        if can_move_earlier {
            entries.push(MenuEntry::item(
                MENU_ISLAND_MOVE_EARLIER,
                MenuIcon::ChevronLeft,
                loc::t("DynamicIslandMoveEarlier"),
            ));
        }
        if can_move_later {
            entries.push(MenuEntry::item(
                MENU_ISLAND_MOVE_LATER,
                MenuIcon::ChevronRight,
                loc::t("DynamicIslandMoveLater"),
            ));
        }
    }
    if let Some(label) = secondary_label {
        entries.push(MenuEntry::item(
            MENU_WIDGET_SECONDARY,
            MenuIcon::Refresh,
            label,
        ));
    }
    entries.push(MenuEntry::destructive(
        MENU_WIDGET_REMOVE,
        MenuIcon::Trash,
        loc::t("RemoveWidget"),
    ));
    if !in_island {
        entries.push(MenuEntry::Separator);
        entries.extend(dock_menu_entries(settings));
    }

    match menu::show_popup_menu_with_border(
        hwnd,
        is_dark,
        entries,
        BorderStyle::from_menu_settings(is_dark, settings),
    )
    .unwrap_or(0)
    {
        MENU_WIDGET_SECONDARY => WidgetContextAction::Secondary,
        MENU_WIDGET_REMOVE => WidgetContextAction::Remove,
        MENU_ISLAND_ALL => WidgetContextAction::PlaceAllDisplays,
        MENU_ISLAND_TASKBAR => WidgetContextAction::MoveToTaskbar,
        MENU_ISLAND_MOVE_EARLIER => WidgetContextAction::MoveEarlier,
        MENU_ISLAND_MOVE_LATER => WidgetContextAction::MoveLater,
        id @ MENU_ISLAND_DISPLAY_BASE..=MENU_ISLAND_DISPLAY_MAX => {
            WidgetContextAction::PlaceOnDisplay((id - MENU_ISLAND_DISPLAY_BASE) as usize)
        }
        id => match map_context_action(id) {
            ContextAction::None => WidgetContextAction::None,
            act => WidgetContextAction::Dock(act),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextAction {
    None,
    Search,
    ApplyProfile(usize),
    SaveNewProfile,
    AddStack(&'static str),
    RemoveStack(usize),
    ToggleFocusMode,
    Settings,
    TaskManager,
    FileExplorer,
    Terminal,
    TerminalAdmin,
    RunDialog,
    Notifications,
    DeviceManager,
    DiskManagement,
    ComputerManagement,
    EventViewer,
    NetworkConnections,
    LockWorkstation,
    SignOut,
    RestartComputer,
    ShutDownComputer,
    SleepComputer,
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
    ToggleGroup(usize),
    CreateGroup,
    Close,
    Terminate,
    Dock(ContextAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskViewAction {
    None,
    PreviousDesktop,
    NextDesktop,
    NewDesktop,
    TaskView,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo {
    pub handle: usize,
    pub device_name: String,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub dpi_scale: f32,
    pub is_primary: bool,
}

impl MonitorInfo {
    pub fn width(&self) -> i32 {
        (self.right - self.left).max(1)
    }

    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(1)
    }

    pub fn display_label(&self) -> String {
        let short = self.device_name.trim_start_matches(r"\\.\").trim();
        if short.is_empty() {
            "Display".to_owned()
        } else {
            short.to_owned()
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Pure Win32->`MonitorInfo` conversion shared by `EnumDisplayMonitors` and the
/// single-monitor fallback. Kept side-effect free so signed virtual-desktop
/// origins and DPI scaling can be tested without a live display.
fn monitor_from_raw(
    handle: usize,
    device_name: String,
    rect: (i32, i32, i32, i32),
    dpi_x: u32,
    is_primary: bool,
) -> MonitorInfo {
    let (left, top, right, bottom) = rect;
    let dpi_scale = if dpi_x == 0 { 1.0 } else { dpi_x as f32 / 96.0 };
    MonitorInfo {
        handle,
        device_name,
        left,
        top,
        right,
        bottom,
        dpi_scale,
        is_primary,
    }
}

/// Primary monitor first; the fallback entry is already primary when present.
fn order_monitors(mut monitors: Vec<MonitorInfo>) -> Vec<MonitorInfo> {
    monitors.sort_by_key(|monitor| !monitor.is_primary);
    monitors
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
            let mut info: MONITORINFOEXW = std::mem::zeroed();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hmon, (&mut info as *mut MONITORINFOEXW).cast()) != 0 {
                let mut dpi_x = 96u32;
                let mut dpi_y = 96u32;
                let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
                let end = info
                    .szDevice
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(info.szDevice.len());
                let device_name = String::from_utf16_lossy(&info.szDevice[..end]);
                list.push(monitor_from_raw(
                    hmon as usize,
                    device_name,
                    (
                        info.monitorInfo.rcMonitor.left,
                        info.monitorInfo.rcMonitor.top,
                        info.monitorInfo.rcMonitor.right,
                        info.monitorInfo.rcMonitor.bottom,
                    ),
                    dpi_x,
                    (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0,
                ));
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
        monitors.push(monitor_from_raw(
            0,
            r"\\.\DISPLAY1".to_owned(),
            (0, 0, w, h),
            96,
            true,
        ));
    }
    order_monitors(monitors)
}

pub fn monitor_refresh_hz(device_name: &str) -> u32 {
    if device_name.is_empty() {
        return 60;
    }
    let name_w = wide(device_name);
    let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
    dm.dmSize = size_of::<DEVMODEW>() as u16;
    let ok = unsafe { EnumDisplaySettingsW(name_w.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) };
    if ok != 0 && dm.dmDisplayFrequency > 1 {
        dm.dmDisplayFrequency.max(15)
    } else {
        60
    }
}

pub fn max_connected_refresh_hz() -> u32 {
    enumerate_monitors()
        .iter()
        .map(|m| monitor_refresh_hz(&m.device_name))
        .max()
        .unwrap_or(60)
        .max(15)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundProbe {
    OwnProcess,
    DesktopOrEmpty,
    Window(usize),
}

pub fn try_get_foreground_application_window() -> ForegroundProbe {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() {
            return ForegroundProbe::DesktopOrEmpty;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, &mut pid);
        if pid == GetCurrentProcessId() {
            return ForegroundProbe::OwnProcess;
        }
        let root = GetAncestor(fg, GA_ROOTOWNER);
        let resolved = if !root.is_null() { root } else { fg };
        ForegroundProbe::Window(resolved as usize)
    }
}

pub fn is_window_on_monitor(hwnd_val: usize, monitor_handle: usize) -> bool {
    if hwnd_val == 0 || monitor_handle == 0 {
        return false;
    }
    unsafe {
        let hmon = MonitorFromWindow(hwnd_val as HWND, MONITOR_DEFAULTTONEAREST);
        (hmon as usize) == monitor_handle
    }
}

pub fn is_any_fullscreen_on_monitor(monitor: &MonitorInfo) -> bool {
    if monitor.handle == 0 {
        return false;
    }
    struct FullscreenScan {
        monitor: MonitorInfo,
        own_pid: u32,
        found: bool,
    }
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let scan = &mut *(lparam as *mut FullscreenScan);
            if scan.found {
                return 0;
            }
            if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                return 1;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == scan.own_pid {
                return 1;
            }
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let app_window = (ex & WS_EX_APPWINDOW) != 0;
            let no_activate = (ex & WS_EX_NOACTIVATE) != 0;
            let tool_window = (ex & WS_EX_TOOLWINDOW) != 0;
            let has_taskbar_btn = if GetWindow(hwnd, GW_OWNER).is_null() {
                (!no_activate || app_window) && !tool_window
            } else {
                app_window
            };
            if !has_taskbar_btn {
                return 1;
            }
            let mut rc: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut rc) == 0 {
                return 1;
            }
            let covers = rc.left <= scan.monitor.left
                && rc.top <= scan.monitor.top
                && rc.right >= scan.monitor.right
                && rc.bottom >= scan.monitor.bottom;
            if !covers {
                return 1;
            }
            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if (hmon as usize) != scan.monitor.handle {
                return 1;
            }
            let mut cls = [0u16; 64];
            let len = GetClassNameW(hwnd, cls.as_mut_ptr(), cls.len() as i32);
            if len > 0 {
                let name = String::from_utf16_lossy(&cls[..len as usize]);
                if matches!(
                    name.as_str(),
                    "Progman"
                        | "WorkerW"
                        | "Shell_TrayWnd"
                        | "Shell_SecondaryTrayWnd"
                        | "Windows.UI.Core.CoreWindow"
                ) {
                    return 1;
                }
            }
            scan.found = true;
            0
        }
    }

    let mut scan = FullscreenScan {
        monitor: monitor.clone(),
        own_pid: unsafe { GetCurrentProcessId() },
        found: false,
    };
    unsafe {
        EnumWindows(Some(enum_cb), (&mut scan as *mut FullscreenScan) as LPARAM);
    }
    scan.found
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

/// Starts a native Win32 caption drag on a borderless GPUI window.
pub fn start_native_window_drag(window: &gpui::Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let hwnd = win32.hwnd.get() as HWND;
    // SAFETY: releases capture and delegates drag loop directly to Windows' native non-client move.
    unsafe {
        let _ = ReleaseCapture();
        SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as WPARAM, 0);
    }
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

        // Enable Windows 11 HostBackdropBrush sampling on transparent WS_EX_NOREDIRECTIONBITMAP
        // dock/island windows without setting DWMWA_SYSTEMBACKDROP_TYPE (which causes a giant
        // rectangular gray backdrop) and without DWMNCRP_DISABLED (which blocks HostBackdropBrush).
        const DWMWA_USE_HOSTBACKDROPBRUSH: u32 = 17;
        let use_host_backdrop: u32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_HOSTBACKDROPBRUSH,
            (&use_host_backdrop as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let margins = windows_sys::Win32::UI::Controls::MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = windows_sys::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea(hwnd, &margins);

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

/// Returns the monitor containing a physical point, falling back to the primary monitor.
/// Popups must clamp against this, not `SM_CXSCREEN`/`SM_CYSCREEN`: on a monitor left of or
/// above the primary one the coordinates are negative, and primary-only clamping teleports
/// the popup to the wrong display.
pub fn monitor_for_point_px(x: i32, y: i32) -> MonitorInfo {
    let monitors = enumerate_monitors();
    monitors
        .iter()
        .find(|m| x >= m.left && x < m.right && y >= m.top && y < m.bottom)
        .cloned()
        .unwrap_or_else(|| monitors.first().cloned().unwrap())
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

use crate::ui::menu::{self, BorderStyle, MenuEntry, MenuIcon};

fn map_context_action(id: u32) -> ContextAction {
    match id {
        MENU_SEARCH => ContextAction::Search,
        MENU_PROFILE_APPLY_BASE..=MENU_PROFILE_APPLY_MAX => {
            ContextAction::ApplyProfile((id - MENU_PROFILE_APPLY_BASE) as usize)
        }
        MENU_PROFILE_SAVE_NEW => ContextAction::SaveNewProfile,
        MENU_STACK_ADD_DOWNLOADS => ContextAction::AddStack("Downloads"),
        MENU_STACK_ADD_DESKTOP => ContextAction::AddStack("Desktop"),
        MENU_STACK_ADD_DOCUMENTS => ContextAction::AddStack("Documents"),
        MENU_STACK_REMOVE_BASE..=MENU_STACK_REMOVE_MAX => {
            ContextAction::RemoveStack((id - MENU_STACK_REMOVE_BASE) as usize)
        }
        MENU_FOCUS_MODE => ContextAction::ToggleFocusMode,
        MENU_SETTINGS => ContextAction::Settings,
        MENU_TASKMGR => ContextAction::TaskManager,
        MENU_EXPLORER => ContextAction::FileExplorer,
        MENU_TERMINAL => ContextAction::Terminal,
        MENU_TERMINAL_ADMIN => ContextAction::TerminalAdmin,
        MENU_RUN => ContextAction::RunDialog,
        MENU_NOTIFICATION_CENTER => ContextAction::Notifications,
        MENU_DEV_MGMT => ContextAction::DeviceManager,
        MENU_DISK_MGMT => ContextAction::DiskManagement,
        MENU_COMP_MGMT => ContextAction::ComputerManagement,
        MENU_EVENT_VWR => ContextAction::EventViewer,
        MENU_NET_CONN => ContextAction::NetworkConnections,
        MENU_LOCK_PC => ContextAction::LockWorkstation,
        MENU_SIGN_OUT => ContextAction::SignOut,
        MENU_RESTART_PC => ContextAction::RestartComputer,
        MENU_SHUTDOWN_PC => ContextAction::ShutDownComputer,
        MENU_SLEEP_PC => ContextAction::SleepComputer,
        MENU_SHOW_DESKTOP => ContextAction::ShowDesktop,
        MENU_RESTORE => ContextAction::RestoreTaskbar,
        MENU_EXIT => ContextAction::Exit,
        _ => ContextAction::None,
    }
}

fn dock_menu_entries(settings: &DockSettings) -> Vec<MenuEntry> {
    let mut profile_items: Vec<MenuEntry> = settings
        .profiles
        .iter()
        .enumerate()
        .take(20)
        .map(|(i, p)| {
            let active = settings
                .active_profile
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(&p.name));
            if active {
                MenuEntry::with_shortcut(
                    MENU_PROFILE_APPLY_BASE + i as u32,
                    MenuIcon::Layers,
                    p.name.clone(),
                    loc::t("Active"),
                )
            } else {
                MenuEntry::item(
                    MENU_PROFILE_APPLY_BASE + i as u32,
                    MenuIcon::Layers,
                    p.name.clone(),
                )
            }
        })
        .collect();
    if !profile_items.is_empty() {
        profile_items.push(MenuEntry::Separator);
    }
    profile_items.push(MenuEntry::item(
        MENU_PROFILE_SAVE_NEW,
        MenuIcon::Plus,
        loc::t("ProfileNew"),
    ));

    let mut stack_items: Vec<MenuEntry> = settings
        .stack_folders
        .iter()
        .enumerate()
        .take(20)
        .map(|(i, path)| {
            let name = std::path::Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path);
            MenuEntry::destructive(
                MENU_STACK_REMOVE_BASE + i as u32,
                MenuIcon::Trash,
                format!("{} {name}", loc::t("WidgetRemove")),
            )
        })
        .collect();
    if !stack_items.is_empty() {
        stack_items.push(MenuEntry::Separator);
    }
    stack_items.push(MenuEntry::item(
        MENU_STACK_ADD_DOWNLOADS,
        MenuIcon::Folder,
        format!("{} {}", loc::t("AddStack"), loc::t("Desktop")),
    ));
    stack_items.push(MenuEntry::item(
        MENU_STACK_ADD_DESKTOP,
        MenuIcon::Folder,
        format!("{} {}", loc::t("AddStack"), loc::t("Desktop")),
    ));
    stack_items.push(MenuEntry::item(
        MENU_STACK_ADD_DOCUMENTS,
        MenuIcon::Folder,
        format!("{} {}", loc::t("AddStack"), loc::t("OpenFolder")),
    ));

    vec![
        MenuEntry::item(MENU_SEARCH, MenuIcon::Search, loc::t("SearchApps")),
        MenuEntry::with_shortcut(
            MENU_NOTIFICATION_CENTER,
            MenuIcon::Activity,
            loc::t("NotificationCenter"),
            "Win+N",
        ),
        MenuEntry::submenu(MenuIcon::Layers, loc::t("Profiles"), profile_items),
        MenuEntry::submenu(MenuIcon::Folder, loc::t("FolderStacks"), stack_items),
        MenuEntry::item(
            MENU_FOCUS_MODE,
            MenuIcon::Layers,
            if settings.focus_mode {
                format!("{} ({})", loc::t("FocusMode"), loc::t("QuickOn"))
            } else {
                loc::t("FocusMode")
            },
        ),
        MenuEntry::item(MENU_SETTINGS, MenuIcon::Settings, loc::t("Settings")),
        MenuEntry::with_shortcut(
            MENU_TASKMGR,
            MenuIcon::Activity,
            loc::t("TaskManager"),
            "Ctrl+Shift+Esc",
        ),
        MenuEntry::Separator,
        MenuEntry::with_shortcut(
            MENU_SHOW_DESKTOP,
            MenuIcon::Monitor,
            loc::t("ShowDesktop"),
            "Win+D",
        ),
        MenuEntry::item(
            MENU_RESTORE,
            MenuIcon::Refresh,
            format!("{} {}", loc::t("Restore"), loc::t("WindowsTaskbar")),
        ),
        MenuEntry::Separator,
        MenuEntry::destructive(MENU_EXIT, MenuIcon::Power, loc::t("Exit")),
    ]
}

/// Power-user / system menu opened only on right-clicking the Windows Start logo.
pub fn show_start_context_menu(
    hwnd: usize,
    is_dark: bool,
    settings: &DockSettings,
) -> ContextAction {
    let entries = vec![
        MenuEntry::with_shortcut(
            MENU_TASKMGR,
            MenuIcon::Activity,
            loc::t("TaskManager"),
            "Ctrl+Shift+Esc",
        ),
        MenuEntry::with_shortcut(
            MENU_EXPLORER,
            MenuIcon::Folder,
            loc::t("OpenFileExplorer"),
            "Win+E",
        ),
        MenuEntry::with_shortcut(MENU_RUN, MenuIcon::Play, loc::t("OpenRunDialog"), "Win+R"),
        MenuEntry::with_shortcut(
            MENU_NOTIFICATION_CENTER,
            MenuIcon::Activity,
            loc::t("NotificationCenter"),
            "Win+N",
        ),
        MenuEntry::item(MENU_TERMINAL, MenuIcon::Terminal, loc::t("OpenTerminal")),
        MenuEntry::item(
            MENU_TERMINAL_ADMIN,
            MenuIcon::Shield,
            loc::t("OpenTerminalAdmin"),
        ),
        MenuEntry::Separator,
        MenuEntry::item(
            MENU_DEV_MGMT,
            MenuIcon::Settings,
            loc::t("OpenDeviceManager"),
        ),
        MenuEntry::item(
            MENU_DISK_MGMT,
            MenuIcon::Layers,
            loc::t("OpenDiskManagement"),
        ),
        MenuEntry::item(
            MENU_COMP_MGMT,
            MenuIcon::Monitor,
            loc::t("OpenComputerManagement"),
        ),
        MenuEntry::item(
            MENU_EVENT_VWR,
            MenuIcon::Activity,
            loc::t("OpenEventViewer"),
        ),
        MenuEntry::item(
            MENU_NET_CONN,
            MenuIcon::Refresh,
            loc::t("OpenNetworkConnections"),
        ),
        MenuEntry::Separator,
        MenuEntry::with_shortcut(
            MENU_SHOW_DESKTOP,
            MenuIcon::Monitor,
            loc::t("ShowDesktop"),
            "Win+D",
        ),
        MenuEntry::Separator,
        MenuEntry::with_shortcut(
            MENU_LOCK_PC,
            MenuIcon::Shield,
            loc::t("LockWorkstation"),
            "Win+L",
        ),
        MenuEntry::item(MENU_SIGN_OUT, MenuIcon::ChevronRight, loc::t("SignOut")),
        MenuEntry::item(MENU_SLEEP_PC, MenuIcon::Power, loc::t("SleepComputer")),
        MenuEntry::item(
            MENU_RESTART_PC,
            MenuIcon::Refresh,
            loc::t("RestartComputer"),
        ),
        MenuEntry::destructive(
            MENU_SHUTDOWN_PC,
            MenuIcon::Power,
            loc::t("ShutDownComputer"),
        ),
    ];
    map_context_action(
        menu::show_popup_menu_with_border(
            hwnd,
            is_dark,
            entries,
            BorderStyle::from_menu_settings(is_dark, settings),
        )
        .unwrap_or(0),
    )
}

pub fn show_dock_context_menu(
    hwnd: usize,
    is_dark: bool,
    settings: &DockSettings,
) -> ContextAction {
    show_dock_context_menu_with_entries(hwnd, is_dark, settings, Vec::new()).1
}

/// Same dock menu with extra entries appended. The raw chosen id is returned
/// alongside the mapped action so integration layers (native window commands)
/// can dispatch their own ids without colliding with the dock's reserved
/// `MENU_*` range.
pub fn show_dock_context_menu_with_entries(
    hwnd: usize,
    is_dark: bool,
    settings: &DockSettings,
    extra: Vec<MenuEntry>,
) -> (u32, ContextAction) {
    let mut entries = dock_menu_entries(settings);
    entries.extend(extra);
    let chosen = menu::show_popup_menu_with_border(
        hwnd,
        is_dark,
        entries,
        BorderStyle::from_menu_settings(is_dark, settings),
    )
    .unwrap_or(0);
    (chosen, map_context_action(chosen))
}

pub fn show_app_context_menu(
    hwnd: usize,
    is_dark: bool,
    app_name: &str,
    app_launch_path: &str,
    pinned: bool,
    running: bool,
    settings: &DockSettings,
) -> AppContextAction {
    let mut group_items: Vec<MenuEntry> = settings
        .app_groups
        .iter()
        .enumerate()
        .take(20)
        .map(|(i, g)| {
            let member = g
                .executable_paths
                .iter()
                .any(|p| p.eq_ignore_ascii_case(app_launch_path));
            if member {
                MenuEntry::with_shortcut(
                    APP_MENU_GROUP_TOGGLE_BASE + i as u32,
                    MenuIcon::Layers,
                    g.name.clone(),
                    loc::t("WidgetInstalled"),
                )
            } else {
                MenuEntry::item(
                    APP_MENU_GROUP_TOGGLE_BASE + i as u32,
                    MenuIcon::Layers,
                    g.name.clone(),
                )
            }
        })
        .collect();
    if !group_items.is_empty() {
        group_items.push(MenuEntry::Separator);
    }
    group_items.push(MenuEntry::item(
        APP_MENU_GROUP_CREATE,
        MenuIcon::Plus,
        loc::t("NewGroup"),
    ));

    let mut entries = vec![
        MenuEntry::item(APP_MENU_OPEN, MenuIcon::Play, app_name),
        MenuEntry::with_shortcut(
            APP_MENU_NEW_WINDOW,
            MenuIcon::Plus,
            loc::t("NewInstance"),
            "Shift+Click",
        ),
        MenuEntry::with_shortcut(
            APP_MENU_ADMIN,
            MenuIcon::Shield,
            loc::t("RunAsAdmin"),
            "Ctrl+Shift",
        ),
        MenuEntry::Separator,
        MenuEntry::item(
            APP_MENU_TOGGLE_PIN,
            MenuIcon::Pin,
            if pinned {
                loc::t("Unpin")
            } else {
                loc::t("PinApplication")
            },
        ),
        MenuEntry::submenu(MenuIcon::Layers, loc::t("AppGroups"), group_items),
    ];
    if running {
        entries.push(MenuEntry::Separator);
        entries.push(MenuEntry::item(
            APP_MENU_CLOSE,
            MenuIcon::Close,
            loc::t("CloseWindow"),
        ));
        entries.push(MenuEntry::destructive(
            APP_MENU_TERMINATE,
            MenuIcon::Power,
            loc::t("Terminate"),
        ));
    }
    JUMPLIST_COMMANDS.with(|commands| commands.borrow_mut().clear());
    if let Some(list) = crate::platform::jumplist::read_jump_list(hwnd, Some(app_launch_path)) {
        let mut jump_items: Vec<MenuEntry> = Vec::new();
        'groups: for group in &list.groups {
            if !group.title.is_empty() && !jump_items.is_empty() {
                jump_items.push(MenuEntry::Separator);
            }
            for entry in &group.entries {
                if entry.is_separator {
                    jump_items.push(MenuEntry::Separator);
                    continue;
                }
                let id = JUMPLIST_ID_BASE + jump_items.len() as u32;
                if id > JUMPLIST_ID_MAX {
                    break 'groups;
                }
                JUMPLIST_COMMANDS.with(|commands| {
                    commands
                        .borrow_mut()
                        .insert(id, (list.session_id, entry.command_id));
                });
                jump_items.push(MenuEntry::item(id, MenuIcon::Folder, entry.title.clone()));
            }
        }
        if !jump_items.is_empty() {
            entries.push(MenuEntry::submenu(
                MenuIcon::Layers,
                loc::t("PinnedDestinations"),
                jump_items,
            ));
        }
    }
    entries.push(MenuEntry::Separator);
    entries.extend(dock_menu_entries(settings));

    let chosen = menu::show_popup_menu_with_border(
        hwnd,
        is_dark,
        entries,
        BorderStyle::from_menu_settings(is_dark, settings),
    )
    .unwrap_or(0);
    match chosen {
        APP_MENU_OPEN => AppContextAction::Open,
        APP_MENU_NEW_WINDOW => AppContextAction::NewWindow,
        APP_MENU_ADMIN => AppContextAction::RunAsAdmin,
        APP_MENU_TOGGLE_PIN => AppContextAction::TogglePin,
        APP_MENU_GROUP_CREATE => AppContextAction::CreateGroup,
        APP_MENU_GROUP_TOGGLE_BASE..=APP_MENU_GROUP_TOGGLE_MAX => {
            AppContextAction::ToggleGroup((chosen - APP_MENU_GROUP_TOGGLE_BASE) as usize)
        }
        APP_MENU_CLOSE => AppContextAction::Close,
        APP_MENU_TERMINATE => AppContextAction::Terminate,
        id if (JUMPLIST_ID_BASE..=JUMPLIST_ID_MAX).contains(&id) => {
            if let Some((session, command)) =
                JUMPLIST_COMMANDS.with(|commands| commands.borrow_mut().remove(&id))
            {
                crate::platform::jumplist::invoke_command(session, command, hwnd);
                crate::platform::jumplist::release_session(session);
            }
            AppContextAction::None
        }
        id => match map_context_action(id) {
            ContextAction::None => AppContextAction::None,
            act => AppContextAction::Dock(act),
        },
    }
}

pub fn show_taskview_context_menu(
    hwnd: usize,
    is_dark: bool,
    settings: &DockSettings,
) -> TaskViewAction {
    let entries = vec![
        MenuEntry::item(
            TV_MENU_PREV,
            MenuIcon::ChevronLeft,
            loc::t("DesktopPrevious"),
        ),
        MenuEntry::item(TV_MENU_NEXT, MenuIcon::ChevronRight, loc::t("DesktopNext")),
        MenuEntry::item(TV_MENU_NEW, MenuIcon::Plus, loc::t("DesktopNew")),
        MenuEntry::Separator,
        MenuEntry::item(TV_MENU_VIEW, MenuIcon::Layers, loc::t("DesktopTaskView")),
    ];
    match menu::show_popup_menu_with_border(
        hwnd,
        is_dark,
        entries,
        BorderStyle::from_menu_settings(is_dark, settings),
    )
    .unwrap_or(0)
    {
        TV_MENU_PREV => TaskViewAction::PreviousDesktop,
        TV_MENU_NEXT => TaskViewAction::NextDesktop,
        TV_MENU_NEW => TaskViewAction::NewDesktop,
        TV_MENU_VIEW => TaskViewAction::TaskView,
        _ => TaskViewAction::None,
    }
}

pub fn show_recycle_bin_context_menu(
    hwnd: usize,
    is_dark: bool,
    settings: &DockSettings,
) -> RecycleBinAction {
    let entries = vec![
        MenuEntry::item(BIN_MENU_OPEN, MenuIcon::Folder, loc::t("OpenRecycleBin")),
        MenuEntry::Separator,
        MenuEntry::destructive(BIN_MENU_EMPTY, MenuIcon::Trash, loc::t("EmptyRecycleBin")),
    ];
    match menu::show_popup_menu_with_border(
        hwnd,
        is_dark,
        entries,
        BorderStyle::from_menu_settings(is_dark, settings),
    )
    .unwrap_or(0)
    {
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

#[cfg(test)]
mod tests {
    use super::{MonitorInfo, monitor_from_raw, order_monitors};

    #[test]
    fn enumerate_monitors_seam_handles_negative_origins_and_dpi() {
        // Left-of-primary monitor: the virtual desktop origin is negative.
        let secondary = monitor_from_raw(
            7,
            r"\\.\DISPLAY2".to_owned(),
            (-1920, -120, 0, 960),
            192,
            false,
        );
        let primary = monitor_from_raw(9, r"\\.\DISPLAY1".to_owned(), (0, 0, 2560, 1440), 96, true);
        assert_eq!((secondary.left, secondary.right), (-1920, 0));
        assert_eq!((secondary.top, secondary.bottom), (-120, 960));
        assert_eq!(secondary.width(), 1920);
        assert_eq!(secondary.height(), 1080);
        assert_eq!(secondary.dpi_scale, 2.0);
        assert_eq!(primary.dpi_scale, 1.0);
        assert_eq!(primary.display_label(), "DISPLAY1");
        assert_eq!(secondary.left + secondary.width(), 0, "no gap at origin");

        // A zero DPI reading must not divide by zero or invert the scale.
        let fallback = monitor_from_raw(0, String::new(), (0, 0, 1366, 768), 0, true);
        assert_eq!(fallback.dpi_scale, 1.0);
        assert_eq!(fallback.display_label(), "Display");
    }

    #[test]
    fn enumerate_monitors_seam_orders_primary_first_without_a_display() {
        let secondary = MonitorInfo {
            handle: 7,
            device_name: "DISPLAY2".to_owned(),
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
            dpi_scale: 1.0,
            is_primary: false,
        };
        let primary = MonitorInfo {
            handle: 9,
            device_name: "DISPLAY1".to_owned(),
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
            dpi_scale: 1.0,
            is_primary: true,
        };
        let ordered = order_monitors(vec![secondary.clone(), primary.clone()]);
        assert_eq!(ordered[0], primary);
        assert_eq!(ordered[1], secondary);
        assert!(order_monitors(Vec::new()).is_empty());
    }
}
