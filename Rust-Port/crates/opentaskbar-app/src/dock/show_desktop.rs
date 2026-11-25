//! Native Windows bottom-right corner "Show Desktop" edge strip (Fitts's Law).
//!
//! Sits at the far bottom-right corner of the monitor (exact Windows taskbar location),
//! stays unobtrusive when idle, highlights cleanly on hover, and triggers `Show Desktop`
//! on click without stealing focus.

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, HWND_TOPMOST,
    IDC_HAND, LWA_ALPHA, LoadCursorW, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SetCursor, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::platform::win32::MonitorInfo;

const WM_MOUSELEAVE: u32 = 0x02A3;
const STRIP_W_DIPS: f32 = 8.0;
const STRIP_H_DIPS: f32 = 44.0;
const IDLE_ALPHA: u8 = 28;
const HOVER_ALPHA: u8 = 185;

struct CornerState {
    is_dark: bool,
    hovered: bool,
    tracking: bool,
}

pub struct ShowDesktopCorner {
    hwnd: HWND,
    state: *mut CornerState,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}

impl ShowDesktopCorner {
    pub fn new(monitor: MonitorInfo, is_dark: bool, visible: bool) -> Option<Self> {
        let class_name = wide("OpenTaskBarShowDesktop");
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(corner_wnd_proc),
            hInstance: hinstance,
            hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_HAND) },
            lpszClassName: class_name.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        unsafe {
            RegisterClassExW(&wc);
        }

        let w = (STRIP_W_DIPS * monitor.dpi_scale).round() as i32;
        let h = (STRIP_H_DIPS * monitor.dpi_scale).round() as i32;
        let x = monitor.right - w;
        let y = monitor.bottom - h;

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                class_name.as_ptr(),
                wide("").as_ptr(),
                WS_POPUP,
                x,
                y,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return None;
        }

        let state = Box::into_raw(Box::new(CornerState {
            is_dark,
            hovered: false,
            tracking: false,
        }));
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            let _ = SetLayeredWindowAttributes(hwnd, 0, IDLE_ALPHA, LWA_ALPHA);
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
        Some(Self { hwnd, state })
    }

    pub fn update(&self, is_dark: bool, visible: bool) {
        unsafe {
            if !self.state.is_null() {
                (*self.state).is_dark = is_dark;
            }
            ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            if visible {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                InvalidateRect(self.hwnd, std::ptr::null(), 0);
            }
        }
    }
}

impl Drop for ShowDesktopCorner {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            DestroyWindow(self.hwnd);
            if !self.state.is_null() {
                drop(Box::from_raw(self.state));
            }
        }
    }
}

unsafe extern "system" fn corner_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CornerState };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if !hdc.is_null() {
                let col = if state.is_dark {
                    if state.hovered {
                        rgb(0xe2, 0xe8, 0xf0)
                    } else {
                        rgb(0x94, 0xa3, 0xb8)
                    }
                } else if state.hovered {
                    rgb(0x0f, 0x17, 0x2a)
                } else {
                    rgb(0x47, 0x55, 0x69)
                };
                unsafe {
                    let brush = CreateSolidBrush(col);
                    FillRect(hdc, &ps.rcPaint, brush);
                    DeleteObject(brush as _);
                    EndPaint(hwnd, &ps);
                }
            }
            0
        }
        WM_SETCURSOR => {
            unsafe {
                SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND));
            }
            1
        }
        WM_MOUSEMOVE => {
            if !state.tracking {
                state.tracking = true;
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    TrackMouseEvent(&mut tme);
                }
            }
            if !state.hovered {
                state.hovered = true;
                unsafe {
                    let _ = SetLayeredWindowAttributes(hwnd, 0, HOVER_ALPHA, LWA_ALPHA);
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            state.tracking = false;
            if state.hovered {
                state.hovered = false;
                unsafe {
                    let _ = SetLayeredWindowAttributes(hwnd, 0, IDLE_ALPHA, LWA_ALPHA);
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }
        WM_LBUTTONUP => {
            opentaskbar_shell::taskbar::show_desktop();
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
