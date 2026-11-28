//! Native Windows 11 bottom-right corner "Show Desktop" edge strip.
//!
//! Uses `UpdateLayeredWindow` for per-pixel alpha:
//! - Idle: 100% invisible (`alpha = 1` hit-testable corner target, zero visible rectangle)
//! - Hover: draws only a crisp Windows 11-style vertical hairline (1.5 DIP wide,
//!   height derived from the dock height and clamped to 16..34 DIP).

use crate::platform::win32_helpers::set_window_long_ptr;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, HWND_TOPMOST,
    IDC_HAND, LoadCursorW, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetCursor,
    SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_SETCURSOR, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

use opentaskbar_core::settings::DockEdge;

use super::layout;
use crate::platform::win32::MonitorInfo;

const WM_MOUSELEAVE: u32 = 0x02A3;
const STRIP_W_DIPS: f32 = 10.0;
const SIDE_STRIP_LENGTH_DIPS: f32 = 40.0;

struct CornerState {
    hwnd: HWND,
    w_px: i32,
    h_px: i32,
    dpi: f32,
    is_dark: bool,
    hovered: bool,
    tracking: bool,
    vertical_edge: bool,
}

impl CornerState {
    fn render(&self) {
        let w = self.w_px.max(1) as usize;
        let h = self.h_px.max(1) as usize;
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..unsafe { std::mem::zeroed() }
            },
            ..unsafe { std::mem::zeroed() }
        };

        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return;
            }
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bmp =
                CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                DeleteDC(dc);
                return;
            }
            let old = SelectObject(dc, bmp as _);
            let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), w * h * 4);

            // Fill with alpha=1 (completely invisible to the eye, still 100% hit-testable).
            for px in pixels.as_chunks_mut::<4>().0 {
                px[0] = 0;
                px[1] = 0;
                px[2] = 0;
                px[3] = 1;
            }

            // On hover, draw only a subtle Windows 11-style 1.5px vertical hairline scaled to dock height.
            if self.hovered {
                let thin = (1.5 * self.dpi).round().max(1.0) as usize;
                let long = ((if self.vertical_edge { w } else { h }) as f32 * 0.45)
                    .round()
                    .clamp(16.0 * self.dpi, 34.0 * self.dpi) as usize;
                let (x0, x1, y0, y1) = if self.vertical_edge {
                    let x0 = w.saturating_sub(long) / 2;
                    (
                        x0,
                        (x0 + long).min(w),
                        h.saturating_sub(thin + (3.0 * self.dpi).round() as usize),
                        h.saturating_sub((3.0 * self.dpi).round() as usize),
                    )
                } else {
                    let x0 = w.saturating_sub(thin + (3.0 * self.dpi).round() as usize);
                    let y0 = h.saturating_sub(long);
                    (x0, (x0 + thin).min(w), y0, h)
                };
                let (r, g, b, a): (u8, u8, u8, u8) = if self.is_dark {
                    (226, 232, 240, 200)
                } else {
                    (30, 41, 59, 185)
                };
                // Premultiplied BGRA
                let pb = ((u16::from(b) * u16::from(a) + 127) / 255) as u8;
                let pg = ((u16::from(g) * u16::from(a) + 127) / 255) as u8;
                let pr = ((u16::from(r) * u16::from(a) + 127) / 255) as u8;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let off = (y * w + x) * 4;
                        pixels[off] = pb;
                        pixels[off + 1] = pg;
                        pixels[off + 2] = pr;
                        pixels[off + 3] = a;
                    }
                }
            }

            let sz = SIZE {
                cx: w as i32,
                cy: h as i32,
            };
            let src_pt = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                self.hwnd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &sz,
                dc,
                &src_pt,
                0,
                &blend,
                ULW_ALPHA,
            );

            SelectObject(dc, old);
            DeleteObject(bmp as _);
            DeleteDC(dc);
        }
    }
}

pub struct ShowDesktopCorner {
    hwnd: HWND,
    state: *mut CornerState,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

impl ShowDesktopCorner {
    /// Edge-aware strip: bottom-right hairline on the bottom dock, a horizontal
    /// strip at the foot of left/right docks.
    pub fn new_for_edge(
        edge: DockEdge,
        monitor: MonitorInfo,
        height_dips: f32,
        is_dark: bool,
        visible: bool,
    ) -> Option<Self> {
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

        let (x, y, w, h) = Self::strip_rect(edge, monitor.clone(), height_dips);

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
            hwnd,
            w_px: w,
            h_px: h,
            dpi: monitor.dpi_scale,
            is_dark,
            hovered: false,
            tracking: false,
            vertical_edge: edge != DockEdge::Bottom,
        }));
        unsafe {
            set_window_long_ptr(hwnd, GWLP_USERDATA, state as isize);
            (*state).render();
            SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
            ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
        Some(Self { hwnd, state })
    }

    pub fn update_for_edge(
        &self,
        edge: DockEdge,
        monitor: MonitorInfo,
        height_dips: f32,
        is_dark: bool,
        visible: bool,
    ) {
        let (x, y, w, h) = Self::strip_rect(edge, monitor.clone(), height_dips);
        unsafe {
            if !self.state.is_null() {
                (*self.state).w_px = w;
                (*self.state).h_px = h;
                (*self.state).dpi = monitor.dpi_scale;
                (*self.state).is_dark = is_dark;
                (*self.state).vertical_edge = edge != DockEdge::Bottom;
                (*self.state).render();
            }
            SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
            ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }

    fn strip_rect(edge: DockEdge, monitor: MonitorInfo, height_dips: f32) -> (i32, i32, i32, i32) {
        let dpi = monitor.dpi_scale;
        let (thick, length) = if edge == DockEdge::Bottom {
            (STRIP_W_DIPS, height_dips.clamp(32.0, 96.0))
        } else {
            (STRIP_W_DIPS, SIDE_STRIP_LENGTH_DIPS)
        };
        let (x, y, w, h) = layout::show_desktop_strip_rect(edge, monitor, thick, length, dpi);
        (x, y, w.max(1), h.max(1))
    }
}

impl Drop for ShowDesktopCorner {
    fn drop(&mut self) {
        unsafe {
            set_window_long_ptr(self.hwnd, GWLP_USERDATA, 0);
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
                state.render();
            }
            0
        }
        WM_MOUSELEAVE => {
            state.tracking = false;
            if state.hovered {
                state.hovered = false;
                state.render();
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
