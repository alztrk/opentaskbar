//! N5: Offline Smart OCR & Quick Region Screenshot.
//!
//! Zero steady-state RAM architecture:
//! - Screen capture buffers (`HBITMAP`, `HDC`, BGRA pixels, WinRT `SoftwareBitmap`) are allocated
//!   strictly during an active capture session and freed immediately before invoking callbacks.
//! - Concurrent overlapping capture sessions are guarded by [`CAPTURE_ACTIVE`].
//! - OCR runs 100% locally via Windows Runtime `Windows.Media.Ocr.OcrEngine`.

use std::ffi::c_void;
use std::fs;
use std::io::Cursor;
use std::mem::size_of;
use std::path::PathBuf;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use image::{DynamicImage, ImageFormat, RgbaImage};
use opentaskbar_core::paths;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, CreatePen, CreateSolidBrush, DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, FrameRect, GetDC, HBITMAP, HDC,
    InvalidateRect, PAINTSTRUCT, PS_SOLID, ReleaseDC, RoundRect, SRCCOPY, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWLP_USERDATA, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, HWND_TOPMOST, IDC_CROSS,
    LWA_ALPHA, LoadCursorW, MSG, PM_REMOVE, PeekMessageW, PostQuitMessage, RegisterClassExW,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOW,
    SWP_SHOWWINDOW, SetCursor, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, TranslateMessage, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::platform::com::ComApartment;
use crate::platform::win32_helpers::{set_window_long_ptr, wide};

/// Minimum selection width/height in pixels; smaller drags are treated as cancellations.
pub const MIN_CAPTURE_PX: i32 = 4;
/// Upper safety bound per axis to prevent integer overflow or runaway capture allocations.
const MAX_CAPTURE_AXIS_PX: i32 = 16_384;
const CAPTUREBLT: u32 = 0x4000_0000;
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;
const GMEM_ZEROINIT: u32 = 0x0040;
const OVERLAY_CLASS_NAME: &str = "OpenTaskBarRegionSelector";

static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static OVERLAY_CLASS_READY: OnceLock<bool> = OnceLock::new();

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(hwnd_new_owner: *mut c_void) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
    fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(mem: *mut c_void) -> i32;
    fn GlobalFree(mem: *mut c_void) -> *mut c_void;
}

/// Summary returned after a quick region screenshot or smart capture finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureResult {
    pub kind: &'static str,
    pub summary: String,
    pub saved_path: Option<PathBuf>,
}

/// Returns `true` while a region selector, OCR pass, or screenshot capture is running.
pub fn is_capture_active() -> bool {
    CAPTURE_ACTIVE.load(Ordering::Acquire)
}

struct CaptureGuard;

impl CaptureGuard {
    fn try_acquire() -> Option<Self> {
        CAPTURE_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURE_ACTIVE.store(false, Ordering::Release);
    }
}

/// Normalizes two drag endpoints `(x0, y0) -> (x1, y1)` into `(left, top, width, height)`.
///
/// Handles right-to-left and bottom-to-top drags cleanly and returns `None` when either
/// dimension is smaller than [`MIN_CAPTURE_PX`] (zero-size click / accidental drag).
pub fn normalize_rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Option<(i32, i32, i32, i32)> {
    let left = x0.min(x1);
    let top = y0.min(y1);
    let width = x0.abs_diff(x1).min(MAX_CAPTURE_AXIS_PX as u32) as i32;
    let height = y0.abs_diff(y1).min(MAX_CAPTURE_AXIS_PX as u32) as i32;
    if width < MIN_CAPTURE_PX || height < MIN_CAPTURE_PX {
        return None;
    }
    Some((left, top, width, height))
}

/// Forces every pixel's alpha byte in a BGRA32 buffer to `255` (GDI `BitBlt` from the
/// desktop screen DC leaves alpha at `0`).
pub fn normalize_bgra_opaque(bgra: &mut [u8]) {
    for px in bgra.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
}

/// Converts a packed top-down BGRA32 buffer into packed RGBA32 with opaque alpha.
pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.as_chunks::<4>().0 {
        rgba.push(px[2]);
        rgba.push(px[1]);
        rgba.push(px[0]);
        rgba.push(255);
    }
    rgba
}

/// Encodes a top-down BGRA32 buffer as a compact PNG byte vector.
pub fn encode_bgra_as_png(bgra: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "Image dimensions overflow".to_owned())?;
    if width == 0 || height == 0 || bgra.len() != expected {
        return Err("Invalid BGRA buffer dimensions".to_owned());
    }
    let rgba = bgra_to_rgba(bgra);
    let img = RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "Failed to build RGBA image".to_owned())?;
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|err| format!("PNG encode failed: {err}"))?;
    Ok(out)
}

/// Builds a standard Windows `CF_DIB` payload (40-byte `BITMAPINFOHEADER` + bottom-up 32-bit BGRA).
pub fn build_cf_dib_payload(bgra: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let stride = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| "Stride overflow".to_owned())?;
    let expected = stride
        .checked_mul(height as usize)
        .ok_or_else(|| "Image size overflow".to_owned())?;
    if width == 0 || height == 0 || bgra.len() != expected {
        return Err("Invalid BGRA buffer for CF_DIB".to_owned());
    }
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        biHeight: height as i32, // Positive => bottom-up DIB for maximum app compatibility.
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: expected as u32,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    let header_len = size_of::<BITMAPINFOHEADER>();
    let mut dib = Vec::with_capacity(header_len + expected);
    // SAFETY: `BITMAPINFOHEADER` is a packed `#[repr(C)]` POD struct.
    let header_bytes =
        unsafe { std::slice::from_raw_parts((&raw const header).cast::<u8>(), header_len) };
    dib.extend_from_slice(header_bytes);
    for row in (0..height as usize).rev() {
        let start = row * stride;
        dib.extend_from_slice(&bgra[start..start + stride]);
    }
    Ok(dib)
}

// ---------------------------------------------------------------------------
// Interactive Win32 translucent fullscreen region selector
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct SelectorState {
    virtual_left: i32,
    virtual_top: i32,
    dragging: bool,
    start_x: i32,
    start_y: i32,
    curr_x: i32,
    curr_y: i32,
    result: Option<(i32, i32, i32, i32)>,
    finished: bool,
}

fn ensure_selector_class() -> bool {
    *OVERLAY_CLASS_READY.get_or_init(|| {
        let class_name = wide(OVERLAY_CLASS_NAME);
        // SAFETY: registering a process-wide window class once with valid WndProc.
        unsafe {
            let hinst = GetModuleHandleW(ptr::null());
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(selector_wnd_proc),
                hInstance: hinst,
                hCursor: LoadCursorW(ptr::null_mut(), IDC_CROSS),
                lpszClassName: class_name.as_ptr(),
                ..Default::default()
            };
            RegisterClassExW(&wc) != 0
        }
    })
}

fn loword_i32(v: isize) -> i32 {
    (v & 0xFFFF) as i16 as i32
}

fn hiword_i32(v: isize) -> i32 {
    ((v >> 16) & 0xFFFF) as i16 as i32
}

unsafe extern "system" fn selector_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SelectorState;
    if state_ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    match msg {
        WM_SETCURSOR => {
            unsafe {
                let cross = LoadCursorW(ptr::null_mut(), IDC_CROSS);
                if !cross.is_null() {
                    SetCursor(cross);
                }
            }
            1
        }
        WM_ERASEBKGND => 1,
        WM_LBUTTONDOWN => {
            let x = loword_i32(lparam);
            let y = hiword_i32(lparam);
            // SAFETY: short-lived mutable borrow does not cross any Win32 call that could re-enter `selector_wnd_proc`.
            {
                let state = unsafe { &mut *state_ptr };
                state.dragging = true;
                state.start_x = x;
                state.start_y = y;
                state.curr_x = x;
                state.curr_y = y;
            }
            unsafe {
                SetCapture(hwnd);
                let _ = InvalidateRect(hwnd, ptr::null(), 0);
            }
            0
        }
        WM_MOUSEMOVE => {
            let needs_invalidate = {
                let state = unsafe { &mut *state_ptr };
                if state.dragging {
                    state.curr_x = loword_i32(lparam);
                    state.curr_y = hiword_i32(lparam);
                    true
                } else {
                    false
                }
            };
            if needs_invalidate {
                unsafe {
                    let _ = InvalidateRect(hwnd, ptr::null(), 0);
                }
            }
            0
        }
        WM_LBUTTONUP => {
            let was_dragging = {
                let state = unsafe { &mut *state_ptr };
                if state.dragging {
                    state.dragging = false;
                    state.curr_x = loword_i32(lparam);
                    state.curr_y = hiword_i32(lparam);
                    true
                } else {
                    false
                }
            };
            if was_dragging {
                unsafe {
                    let _ = ReleaseCapture();
                }
                {
                    let state = unsafe { &mut *state_ptr };
                    state.result =
                        normalize_rect(state.start_x, state.start_y, state.curr_x, state.curr_y)
                            .map(|(l, t, w, h)| {
                                (state.virtual_left + l, state.virtual_top + t, w, h)
                            });
                    state.finished = true;
                }
                unsafe { PostQuitMessage(0) };
            }
            0
        }
        WM_RBUTTONDOWN | WM_RBUTTONUP => {
            let was_dragging = {
                let state = unsafe { &mut *state_ptr };
                let dragging = state.dragging;
                state.dragging = false;
                dragging
            };
            if was_dragging {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            {
                let state = unsafe { &mut *state_ptr };
                state.result = None;
                state.finished = true;
            }
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_KEYDOWN => {
            if wparam == VK_ESCAPE as usize {
                let was_dragging = {
                    let state = unsafe { &mut *state_ptr };
                    let dragging = state.dragging;
                    state.dragging = false;
                    dragging
                };
                if was_dragging {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                {
                    let state = unsafe { &mut *state_ptr };
                    state.result = None;
                    state.finished = true;
                }
                unsafe { PostQuitMessage(0) };
            }
            0
        }
        WM_PAINT => {
            // Copy `SelectorState` by value before `BeginPaint` so no reference aliases across `WM_ERASEBKGND`.
            let snapshot = unsafe { *state_ptr };
            unsafe { paint_selector(hwnd, &snapshot) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn paint_selector(hwnd: HWND, state: &SelectorState) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc.is_null() {
        return;
    }

    // Dimmed dark backdrop (#0C1018).
    let bg_brush = unsafe { CreateSolidBrush(0x0018_100C) };
    if !bg_brush.is_null() {
        unsafe {
            let _ = FillRect(hdc, &ps.rcPaint, bg_brush);
            let _ = DeleteObject(bg_brush);
        }
    }

    if state.dragging
        && let Some((left, top, width, height)) =
            normalize_rect(state.start_x, state.start_y, state.curr_x, state.curr_y)
    {
        let sel_rect = RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        };
        // Subtle highlighted selection interior (#28364A).
        let fill_brush = unsafe { CreateSolidBrush(0x004A_3628) };
        if !fill_brush.is_null() {
            unsafe {
                let _ = FillRect(hdc, &sel_rect, fill_brush);
                let _ = DeleteObject(fill_brush);
            }
        }
        // Crisp 2px white selection border.
        let border_brush = unsafe { CreateSolidBrush(0x00FF_FFFF) };
        if !border_brush.is_null() {
            let inner = RECT {
                left: sel_rect.left + 1,
                top: sel_rect.top + 1,
                right: (sel_rect.right - 1).max(sel_rect.left + 1),
                bottom: (sel_rect.bottom - 1).max(sel_rect.top + 1),
            };
            unsafe {
                let _ = FrameRect(hdc, &sel_rect, border_brush);
                let _ = FrameRect(hdc, &inner, border_brush);
                let _ = DeleteObject(border_brush);
            }
        }

        // Compact live `W × H` size pill above (or inside) the top-left of the selection.
        let pill_w = 104;
        let pill_h = 26;
        let pill_top = if top >= pill_h + 8 {
            top - pill_h - 6
        } else {
            top + 8
        };
        let pill_left = left.max(8);
        let pill_rect = RECT {
            left: pill_left,
            top: pill_top,
            right: pill_left + pill_w,
            bottom: pill_top + pill_h,
        };
        let pill_brush = unsafe { CreateSolidBrush(0x0024_1E18) };
        let pill_pen = unsafe { CreatePen(PS_SOLID, 1, 0x00FF_FFFF) };
        if !pill_brush.is_null() && !pill_pen.is_null() {
            unsafe {
                let old_brush = SelectObject(hdc, pill_brush);
                let old_pen = SelectObject(hdc, pill_pen);
                let _ = RoundRect(
                    hdc,
                    pill_rect.left,
                    pill_rect.top,
                    pill_rect.right,
                    pill_rect.bottom,
                    12,
                    12,
                );
                let _ = SelectObject(hdc, old_brush);
                let _ = SelectObject(hdc, old_pen);
            }
        }
        if !pill_brush.is_null() {
            unsafe {
                let _ = DeleteObject(pill_brush);
            }
        }
        if !pill_pen.is_null() {
            unsafe {
                let _ = DeleteObject(pill_pen);
            }
        }

        let label = wide(&format!("{width} × {height}"));
        let font_face = wide("Segoe UI");
        let font = unsafe {
            CreateFontW(
                -14,
                0,
                0,
                0,
                600,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                font_face.as_ptr(),
            )
        };
        unsafe {
            let old_font = if !font.is_null() {
                SelectObject(hdc, font)
            } else {
                ptr::null_mut()
            };
            let _ = SetBkMode(hdc, TRANSPARENT as i32);
            let _ = SetTextColor(hdc, 0x00FF_FFFF);
            let mut text_rect = pill_rect;
            let _ = DrawTextW(
                hdc,
                label.as_ptr(),
                (label.len().saturating_sub(1)) as i32,
                &mut text_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if !old_font.is_null() {
                let _ = SelectObject(hdc, old_font);
            }
            if !font.is_null() {
                let _ = DeleteObject(font);
            }
        }
    }

    unsafe {
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Opens a temporary translucent fullscreen overlay spanning the virtual desktop so the user
/// can drag-select a region. Returns `Some((x, y, width, height))` in virtual-screen physical
/// coordinates, or `None` if cancelled (`Esc`, right-click, or tiny click).
///
/// The overlay window is destroyed immediately before returning.
pub fn select_screen_region() -> Option<(i32, i32, i32, i32)> {
    if !ensure_selector_class() {
        return None;
    }

    // SAFETY: GetSystemMetrics queries virtual desktop bounds with no pointer arguments.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if vw <= 0 || vh <= 0 {
        return None;
    }

    let state = Box::new(SelectorState {
        virtual_left: vx,
        virtual_top: vy,
        dragging: false,
        start_x: 0,
        start_y: 0,
        curr_x: 0,
        curr_y: 0,
        result: None,
        finished: false,
    });

    let class_name = wide(OVERLAY_CLASS_NAME);
    // SAFETY: temporary modal Win32 popup window owned and destroyed within this function.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class_name.as_ptr(),
            ptr::null(),
            WS_POPUP,
            vx,
            vy,
            vw,
            vh,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        )
    };
    if hwnd.is_null() {
        return None;
    }

    // Transfer ownership out of `Box` into a raw pointer before storing in `GWLP_USERDATA`
    // so `selector_wnd_proc` never aliases a live `&mut Box<SelectorState>` on this stack.
    let state_ptr: *mut SelectorState = Box::into_raw(state);
    set_window_long_ptr(hwnd, GWLP_USERDATA, state_ptr as isize);

    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, 0, 110, LWA_ALPHA);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, vx, vy, vw, vh, SWP_SHOWWINDOW);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = InvalidateRect(hwnd, ptr::null(), 0);
    }

    let mut msg = MSG::default();
    loop {
        // SAFETY: `state_ptr` is owned on this thread and no `selector_wnd_proc` call is active here.
        if unsafe { (*state_ptr).finished } {
            break;
        }
        // SAFETY: standard Win32 modal message pump scoped to this thread's temporary overlay.
        let ret = unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if ret <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }

    // Clear `GWLP_USERDATA` and destroy the overlay window BEFORE reclaiming `Box<SelectorState>`
    // so no `WM_DESTROY`/`WM_NCDESTROY` callback can touch `state_ptr`.
    set_window_long_ptr(hwnd, GWLP_USERDATA, 0);
    unsafe {
        let _ = DestroyWindow(hwnd);
        // Drain any trailing destroy/quit messages so the thread queue stays spotless.
        while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {}
        let _ = DwmFlush();
    }
    // SAFETY: `GWLP_USERDATA` is zeroed and `hwnd` is destroyed; this thread holds sole ownership.
    let state = unsafe { Box::from_raw(state_ptr) };
    let result = state.result;
    drop(state);
    // Brief settle so DWM finishes presenting the frame without the overlay before BitBlt.
    if result.is_some() {
        std::thread::sleep(Duration::from_millis(24));
    }
    result
}

// ---------------------------------------------------------------------------
// Immediate-free GDI region capture + clipboard + WinRT OCR
// ---------------------------------------------------------------------------

struct ScreenDcGuard(HDC);
impl Drop for ScreenDcGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: balances `GetDC(null)`.
            unsafe {
                let _ = ReleaseDC(ptr::null_mut(), self.0);
            }
        }
    }
}

struct MemDcGuard(HDC);
impl Drop for MemDcGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: balances `CreateCompatibleDC`.
            unsafe {
                let _ = DeleteDC(self.0);
            }
        }
    }
}

struct BitmapGuard(HBITMAP);
impl Drop for BitmapGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: balances `CreateDIBSection`.
            unsafe {
                let _ = DeleteObject(self.0);
            }
        }
    }
}

/// Captures `(x, y, width, height)` from the desktop into a top-down BGRA32 buffer,
/// immediately freeing all GDI handles (`HDC`, `HBITMAP`) before returning.
fn capture_region_bgra(x: i32, y: i32, width: i32, height: i32) -> Result<Vec<u8>, String> {
    if width < MIN_CAPTURE_PX
        || height < MIN_CAPTURE_PX
        || width > MAX_CAPTURE_AXIS_PX
        || height > MAX_CAPTURE_AXIS_PX
    {
        return Err("Invalid capture dimensions".to_owned());
    }
    let byte_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "Capture buffer size overflow".to_owned())?;

    // SAFETY: every acquired GDI handle is wrapped in an RAII guard and freed on scope exit.
    unsafe {
        let screen_dc = ScreenDcGuard(GetDC(ptr::null_mut()));
        if screen_dc.0.is_null() {
            return Err("GetDC failed".to_owned());
        }
        let mem_dc = MemDcGuard(CreateCompatibleDC(screen_dc.0));
        if mem_dc.0.is_null() {
            return Err("CreateCompatibleDC failed".to_owned());
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // Negative => top-down BGRA32 layout.
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = ptr::null_mut();
        let hbm = BitmapGuard(CreateDIBSection(
            screen_dc.0,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            ptr::null_mut(),
            0,
        ));
        if hbm.0.is_null() || bits.is_null() {
            return Err("CreateDIBSection failed".to_owned());
        }
        let old_obj = SelectObject(mem_dc.0, hbm.0);
        let blt_ok = BitBlt(
            mem_dc.0,
            0,
            0,
            width,
            height,
            screen_dc.0,
            x,
            y,
            SRCCOPY | CAPTUREBLT,
        ) != 0;
        let _ = SelectObject(mem_dc.0, old_obj);
        if !blt_ok {
            return Err("BitBlt failed".to_owned());
        }
        let mut out = vec![0u8; byte_len];
        ptr::copy_nonoverlapping(bits.cast::<u8>(), out.as_mut_ptr(), byte_len);
        normalize_bgra_opaque(&mut out);
        Ok(out)
    }
}

fn recognize_bgra_with_winrt_ocr(bgra: Vec<u8>, width: i32, height: i32) -> Result<String, String> {
    let _com = ComApartment::mta().or_else(ComApartment::sta);
    let writer = DataWriter::new().map_err(|e| format!("DataWriter::new failed: {e}"))?;
    writer
        .WriteBytes(&bgra)
        .map_err(|e| format!("DataWriter::WriteBytes failed: {e}"))?;
    let buffer = writer
        .DetachBuffer()
        .map_err(|e| format!("DataWriter::DetachBuffer failed: {e}"))?;
    // Immediately release raw pixel buffer once copied into WinRT IBuffer.
    drop(bgra);
    drop(writer);

    let bitmap =
        SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, width, height)
            .map_err(|e| format!("SoftwareBitmap::CreateCopyFromBuffer failed: {e}"))?;
    drop(buffer);

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| format!("OcrEngine unavailable: {e}"))?;
    let async_op = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| format!("OcrEngine::RecognizeAsync failed: {e}"))?;
    let result = async_op
        .join()
        .map_err(|e| format!("OCR recognition failed: {e}"))?;

    let text = match result.Lines() {
        Ok(lines) => {
            let mut collected = Vec::new();
            for line in lines {
                if let Ok(line_text) = line.Text() {
                    let s = line_text.to_string();
                    if !s.trim().is_empty() {
                        collected.push(s);
                    }
                }
            }
            if collected.is_empty() {
                result.Text().map(|t| t.to_string()).unwrap_or_default()
            } else {
                collected.join("\r\n")
            }
        }
        Err(_) => result.Text().map(|t| t.to_string()).unwrap_or_default(),
    };

    drop(result);
    drop(engine);
    drop(bitmap);
    Ok(text)
}

fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    // SAFETY: GlobalAlloc + SetClipboardData transfers ownership on success; frees on failure.
    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len());
        if hmem.is_null() {
            return false;
        }
        let locked = GlobalLock(hmem);
        if locked.is_null() {
            let _ = GlobalFree(hmem);
            return false;
        }
        ptr::copy_nonoverlapping(bytes.as_ptr(), locked.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(hmem);
        if OpenClipboard(ptr::null_mut()) == 0 {
            let _ = GlobalFree(hmem);
            return false;
        }
        let _ = EmptyClipboard();
        let ok = !SetClipboardData(format, hmem).is_null();
        let _ = CloseClipboard();
        if !ok {
            let _ = GlobalFree(hmem);
        }
        ok
    }
}

fn copy_text_to_clipboard(text: &str) -> bool {
    let units = wide(text);
    let byte_len = units.len() * size_of::<u16>();
    // SAFETY: `units` is contiguous u16 memory of `byte_len` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(units.as_ptr().cast::<u8>(), byte_len) };
    set_clipboard_bytes(CF_UNICODETEXT, bytes)
}

fn default_captures_dir() -> PathBuf {
    paths::shared_app_data_dir().join("Captures")
}

fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Interactive region selection followed by offline Windows WinRT OCR.
///
/// Copies recognized text to the clipboard, frees every GDI/WinRT image buffer immediately,
/// and delivers the result to `on_done`. Guarded against overlapping sessions via `CAPTURE_ACTIVE`.
pub fn capture_and_ocr_async(on_done: impl FnOnce(Result<String, String>) + Send + 'static) {
    let Some(guard) = CaptureGuard::try_acquire() else {
        on_done(Err("Capture already in progress".to_owned()));
        return;
    };
    let on_done = std::sync::Arc::new(std::sync::Mutex::new(Some(on_done)));
    let worker_on_done = std::sync::Arc::clone(&on_done);
    let spawned = std::thread::Builder::new()
        .name("opentaskbar-smart-ocr".into())
        .spawn(move || {
            let _guard = guard;
            let outcome = (|| -> Result<String, String> {
                let (x, y, w, h) =
                    select_screen_region().ok_or_else(|| "Capture cancelled".to_owned())?;
                let bgra = capture_region_bgra(x, y, w, h)?;
                let text = recognize_bgra_with_winrt_ocr(bgra, w, h)?;
                if !text.trim().is_empty() {
                    let _ = copy_text_to_clipboard(&text);
                }
                Ok(text)
            })();
            if let Ok(mut cb) = worker_on_done.lock()
                && let Some(cb) = cb.take()
            {
                cb(outcome);
            }
        });
    if let Err(err) = spawned {
        let msg = format!("Smart OCR worker spawn failed: {err}");
        opentaskbar_core::log::warn(msg.clone());
        if let Ok(mut cb) = on_done.lock()
            && let Some(cb) = cb.take()
        {
            cb(Err(msg));
        }
    }
}

/// Interactive region selection followed by clipboard (`CF_DIB`) copy + compact PNG export.
///
/// Saves PNG to `save_dir` (default `%LOCALAPPDATA%\OpenTaskbar\Captures\capture-<ts>.png`),
/// frees all GDI and pixel buffers immediately, and invokes `on_done`.
pub fn capture_region_screenshot_async(
    save_dir: Option<PathBuf>,
    on_done: impl FnOnce(Result<CaptureResult, String>) + Send + 'static,
) {
    let Some(guard) = CaptureGuard::try_acquire() else {
        on_done(Err("Capture already in progress".to_owned()));
        return;
    };
    let on_done = std::sync::Arc::new(std::sync::Mutex::new(Some(on_done)));
    let worker_on_done = std::sync::Arc::clone(&on_done);
    let spawned = std::thread::Builder::new()
        .name("opentaskbar-region-shot".into())
        .spawn(move || {
            let _guard = guard;
            let outcome = (|| -> Result<CaptureResult, String> {
                let (x, y, w, h) =
                    select_screen_region().ok_or_else(|| "Capture cancelled".to_owned())?;
                let bgra = capture_region_bgra(x, y, w, h)?;
                let dib = build_cf_dib_payload(&bgra, w as u32, h as u32)?;
                let copied = set_clipboard_bytes(CF_DIB, &dib);
                drop(dib);

                let png = encode_bgra_as_png(&bgra, w as u32, h as u32)?;
                drop(bgra);

                let dir = save_dir.unwrap_or_else(default_captures_dir);
                fs::create_dir_all(&dir)
                    .map_err(|err| format!("Failed to create captures directory: {err}"))?;
                let file_path = dir.join(format!("capture-{}.png", now_unix_millis()));
                fs::write(&file_path, &png)
                    .map_err(|err| format!("Failed to save screenshot PNG: {err}"))?;
                drop(png);

                let summary = if copied {
                    format!("{w} × {h} copied & saved")
                } else {
                    format!("{w} × {h} saved")
                };
                Ok(CaptureResult {
                    kind: "screenshot",
                    summary,
                    saved_path: Some(file_path),
                })
            })();
            if let Ok(mut cb) = worker_on_done.lock()
                && let Some(cb) = cb.take()
            {
                cb(outcome);
            }
        });
    if let Err(err) = spawned {
        let msg = format!("Region screenshot worker spawn failed: {err}");
        opentaskbar_core::log::warn(msg.clone());
        if let Ok(mut cb) = on_done.lock()
            && let Some(cb) = cb.take()
        {
            cb(Err(msg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rect_handles_all_drag_directions_and_cancellation() {
        // Normal top-left -> bottom-right drag.
        assert_eq!(normalize_rect(10, 20, 110, 70), Some((10, 20, 100, 50)));
        // Right-to-left drag.
        assert_eq!(normalize_rect(110, 20, 10, 70), Some((10, 20, 100, 50)));
        // Bottom-to-top drag.
        assert_eq!(normalize_rect(10, 70, 110, 20), Some((10, 20, 100, 50)));
        // Reversed diagonal across negative multi-monitor coordinates.
        assert_eq!(
            normalize_rect(-50, -10, -250, -110),
            Some((-250, -110, 200, 100))
        );
        // Zero-size or tiny accidental clicks cancel cleanly.
        assert_eq!(normalize_rect(40, 40, 40, 40), None);
        assert_eq!(normalize_rect(40, 40, 42, 90), None);
        assert_eq!(normalize_rect(40, 40, 90, 43), None);
    }

    #[test]
    fn bgra_opaque_rgba_png_and_dib_helpers() {
        // 2x2 image: top row = red (BGRA: 0,0,255,0), bottom row = blue (BGRA: 255,0,0,0).
        let mut bgra = vec![
            0, 0, 255, 0, 0, 0, 255, 0, // row 0 (top)
            255, 0, 0, 0, 255, 0, 0, 0, // row 1 (bottom)
        ];
        normalize_bgra_opaque(&mut bgra);
        assert!(bgra.as_chunks::<4>().0.iter().all(|px| px[3] == 255));

        let rgba = bgra_to_rgba(&bgra);
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]); // Red
        assert_eq!(&rgba[8..12], &[0, 0, 255, 255]); // Blue

        // PNG output starts with PNG signature and round-trips via `image`.
        let png = encode_bgra_as_png(&bgra, 2, 2).expect("encode PNG");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&png)
            .expect("decode PNG")
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 2));
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(decoded.get_pixel(0, 1).0, [0, 0, 255, 255]);

        // CF_DIB header is 40 bytes + bottom-up pixel order (row 1 first, then row 0).
        let dib = build_cf_dib_payload(&bgra, 2, 2).expect("build CF_DIB");
        assert_eq!(dib.len(), size_of::<BITMAPINFOHEADER>() + bgra.len());
        assert_eq!(&dib[40..44], &[255, 0, 0, 255]); // Bottom row (blue) comes first
        assert_eq!(&dib[48..52], &[0, 0, 255, 255]); // Top row (red) comes second

        // Dimension mismatch errors cleanly.
        assert!(encode_bgra_as_png(&bgra[..4], 2, 2).is_err());
        assert!(build_cf_dib_payload(&bgra, 0, 2).is_err());
    }
}
