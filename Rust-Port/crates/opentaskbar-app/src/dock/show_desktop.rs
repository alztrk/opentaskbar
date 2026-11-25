//! Native Windows 11 bottom-right corner "Show Desktop" edge strip.
//!
//! Uses `UpdateLayeredWindow` for per-pixel alpha:
//! - Idle: 100% invisible (`alpha = 1` hit-testable corner target, zero visible rectangle)
//! - Hover: draws only a crisp Windows 11-style `2px x 22px` vertical hairline near the edge

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
    IDC_HAND, LoadCursorW, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SetCursor, SetWindowLongPtrW, SetWindowPos, ShowWindow, ULW_ALPHA,
    UpdateLayeredWindow, WM_LBUTTONUP, WM_MOUSEMOVE, WM_SETCURSOR, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::platform::win32::MonitorInfo;

const WM_MOUSELEAVE: u32 = 0x02A3;
const STRIP_W_DIPS: f32 = 10.0;
const STRIP_H_DIPS: f32 = 44.0;

struct CornerState {
    hwnd: HWND,
    w_px: i32,
    h_px: i32,
    dpi: f32,
    is_dark: bool,
    hovered: bool,
    tracking: bool,
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
            for px in pixels.chunks_exact_mut(4) {
                px[0] = 0;
                px[1] = 0;
                px[2] = 0;
                px[3] = 1;
            }

            // On hover, draw only a subtle Windows 11-style 2px vertical hairline.
            if self.hovered {
                let line_w = (1.5 * self.dpi).round().max(1.0) as usize;
                let line_h = (22.0 * self.dpi).round() as usize;
                let x0 = (w.saturating_sub(line_w + (3.0 * self.dpi).round() as usize)).min(w);
                let x1 = (x0 + line_w).min(w);
                let y0 = h.saturating_sub(line_h) / 2;
                let y1 = (y0 + line_h).min(h);
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
            hwnd,
            w_px: w,
            h_px: h,
            dpi: monitor.dpi_scale,
            is_dark,
            hovered: false,
            tracking: false,
        }));
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            (*state).render();
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
                (*self.state).render();
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
