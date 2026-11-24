//! Shared reusable iOS/shadcn-style popup flyout component (`DockFlyout`).
//!
//! Used across Background Apps (System Tray), Folder Stacks, App Groups, and all 14 Dock Widgets.
//! Supports:
//! - Toggle-on-same-button suppression (re-clicking the same trigger closes cleanly)
//! - Mouse-wheel vertical scrolling + visible thin scrollbar thumb on overflow
//! - Crisp anti-aliased BGRA application icons in `ActionRow`
//! - Left-click (`FlyoutAction::Primary`) and right-click (`FlyoutAction::Secondary`)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, CreateFontW, CreatePen, CreateSolidBrush,
    DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FillRect, HDC, IntersectClipRect, InvalidateRect, LineTo,
    MoveToEx, PAINTSTRUCT, PS_SOLID, RestoreDC, RoundRect, SaveDC, SelectObject, SetBkMode,
    SetTextColor, StretchDIBits, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW,
    GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IDC_HAND, KillTimer, LWA_ALPHA, LoadCursorW, MSG,
    RegisterClassExW, SW_SHOW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetCursor,
    SetForegroundWindow, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, WM_ACTIVATE, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::win32;

const WM_MOUSELEAVE: u32 = 0x02A3;
const DWMWCP_ROUND: u32 = 2;
const MAX_FLYOUT_H_DIPS: f32 = 380.0;
const TIMER_ANIM: usize = 1;
const OPEN_ANIM_MS: f32 = 135.0;
const CLOSE_ANIM_MS: f32 = 95.0;
const SLIDE_OFFSET_DIPS: f32 = 10.0;

static LAST_CLOSED: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();

fn last_closed_lock() -> &'static Mutex<Option<(String, Instant)>> {
    LAST_CLOSED.get_or_init(|| Mutex::new(None))
}

fn should_suppress_reopen(tag: &str) -> bool {
    if tag.is_empty() {
        return false;
    }
    if let Ok(mut guard) = last_closed_lock().lock()
        && let Some((prev_tag, at)) = guard.as_ref()
        && prev_tag == tag
        && at.elapsed().as_millis() < 280
    {
        *guard = None;
        return true;
    }
    false
}

fn record_closed(tag: &str) {
    if tag.is_empty() {
        return;
    }
    if let Ok(mut guard) = last_closed_lock().lock() {
        *guard = Some((tag.to_owned(), Instant::now()));
    }
}

/// Straight BGRA32 icon payload for `FlyoutBlock::ActionRow`.
#[derive(Clone, Debug)]
pub struct FlyoutIcon {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Action returned by `show_flyout_interactive`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlyoutAction {
    Primary(u32),
    Secondary(u32),
}

/// Declarative content block rendered inside the shared `DockFlyout`.
#[derive(Clone, Debug)]
pub enum FlyoutBlock {
    StatRow {
        label: String,
        value: String,
    },
    ProgressBar {
        label: String,
        value_text: String,
        ratio: f32,
    },
    ActionRow {
        id: u32,
        title: String,
        subtitle: Option<String>,
        badge: Option<String>,
        icon: Option<FlyoutIcon>,
        destructive: bool,
    },
    ButtonBar(Vec<(u32, String, bool)>),
    Separator,
    Note(String),
}

#[derive(Clone, Debug)]
pub struct FlyoutSpec {
    pub title: String,
    pub subtitle: Option<String>,
    pub width_dips: f32,
    pub blocks: Vec<FlyoutBlock>,
}

struct HitTarget {
    id: u32,
    /// Content-space coordinates (before subtracting `scroll_y`).
    rect: RECT,
}

struct FlyoutState<'a> {
    hwnd: HWND,
    dpi: f32,
    is_dark: bool,
    spec: FlyoutSpec,
    x_px: i32,
    target_y_px: i32,
    slide_px: i32,
    anim_start: Instant,
    closing: bool,
    animating: bool,
    suppress_blur: bool,
    on_secondary: Option<&'a mut dyn FnMut(u32) -> bool>,
    content_top_px: i32,
    viewport_h_px: i32,
    content_h_px: i32,
    scroll_y: i32,
    targets: Vec<HitTarget>,
    hovered_id: Option<u32>,
    tracking: bool,
    selected: Option<FlyoutAction>,
    done: AtomicBool,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}

fn pt_in_rect(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Convenience wrapper for consumers that only handle primary clicks.
pub fn show_flyout(
    owner_hwnd: usize,
    is_dark: bool,
    anchor_center_x_px: i32,
    anchor_bottom_y_px: i32,
    spec: FlyoutSpec,
) -> Option<u32> {
    match show_flyout_interactive(
        &spec.title.clone(),
        owner_hwnd,
        is_dark,
        anchor_center_x_px,
        anchor_bottom_y_px,
        spec,
        None,
    ) {
        Some(FlyoutAction::Primary(id)) => Some(id),
        _ => None,
    }
}

/// Shows the shared `DockFlyout` with toggle-close suppression, scrolling, icons, and in-place right-click handling.
pub fn show_flyout_interactive(
    tag: &str,
    owner_hwnd: usize,
    is_dark: bool,
    anchor_center_x_px: i32,
    anchor_bottom_y_px: i32,
    spec: FlyoutSpec,
    on_secondary: Option<&mut dyn FnMut(u32) -> bool>,
) -> Option<FlyoutAction> {
    if should_suppress_reopen(tag) {
        return None;
    }

    let class_name = wide("OpenTaskBarFlyout");
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(flyout_wnd_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        lpszClassName: class_name.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        RegisterClassExW(&wc);
    }

    let dpi = win32::dpi_scale(owner_hwnd);
    let s = |v: f32| (v * dpi).round() as i32;
    let width_px = s(spec.width_dips.clamp(240.0, 380.0));
    let pad = s(12.0);
    let header_h = if spec.subtitle.is_some() {
        s(44.0)
    } else {
        s(28.0)
    };
    let content_top_px = pad + header_h + s(6.0);

    // Compute total content height and content-space hit targets.
    let mut y = content_top_px;
    let mut targets = Vec::new();
    let content_right_pad = pad + s(6.0); // room for thin scrollbar if needed
    for block in &spec.blocks {
        match block {
            FlyoutBlock::StatRow { .. } => y += s(26.0),
            FlyoutBlock::ProgressBar { .. } => y += s(38.0),
            FlyoutBlock::ActionRow { id, subtitle, .. } => {
                let h = if subtitle.is_some() { s(40.0) } else { s(32.0) };
                targets.push(HitTarget {
                    id: *id,
                    rect: RECT {
                        left: pad,
                        top: y,
                        right: width_px - content_right_pad,
                        bottom: y + h,
                    },
                });
                y += h + s(4.0);
            }
            FlyoutBlock::ButtonBar(buttons) => {
                let h = s(30.0);
                let count = buttons.len().max(1) as i32;
                let gap = s(6.0);
                let avail = width_px - pad - content_right_pad - gap * (count - 1);
                let bw = (avail / count).max(s(40.0));
                for (i, (id, _, _)) in buttons.iter().enumerate() {
                    let left = pad + (i as i32) * (bw + gap);
                    targets.push(HitTarget {
                        id: *id,
                        rect: RECT {
                            left,
                            top: y,
                            right: left + bw,
                            bottom: y + h,
                        },
                    });
                }
                y += h + s(6.0);
            }
            FlyoutBlock::Separator => y += s(10.0),
            FlyoutBlock::Note(_) => y += s(26.0),
        }
    }
    let content_h_px = y - content_top_px;
    let max_h_px = s(MAX_FLYOUT_H_DIPS);
    let height_px = (content_top_px + content_h_px + pad).min(max_h_px);
    let viewport_h_px = (height_px - content_top_px - pad).max(s(40.0));

    let (scr_w, scr_h) = win32::primary_screen_px();
    let margin = s(10.0);
    let x =
        (anchor_center_x_px - width_px / 2).clamp(margin, (scr_w - width_px - margin).max(margin));
    let top_y =
        (anchor_bottom_y_px - height_px).clamp(margin, (scr_h - height_px - margin).max(margin));

    let slide_px = s(SLIDE_OFFSET_DIPS);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            class_name.as_ptr(),
            wide("").as_ptr(),
            WS_POPUP,
            x,
            top_y + slide_px,
            width_px,
            height_px,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return None;
    }

    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let border = if is_dark {
            rgb(0x2e, 0x35, 0x46)
        } else {
            rgb(0xd4, 0xd9, 0xe3)
        };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            (&border as *const u32).cast(),
            size_of::<u32>() as u32,
        );
    }

    let mut state = Box::new(FlyoutState {
        hwnd,
        dpi,
        is_dark,
        spec,
        x_px: x,
        target_y_px: top_y,
        slide_px,
        anim_start: Instant::now(),
        closing: false,
        animating: true,
        suppress_blur: false,
        on_secondary,
        content_top_px,
        viewport_h_px,
        content_h_px,
        scroll_y: 0,
        targets,
        hovered_id: None,
        tracking: false,
        selected: None,
        done: AtomicBool::new(false),
    });
    let ptr = (&mut *state) as *mut FlyoutState<'_> as isize;
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            x,
            top_y + slide_px,
            width_px,
            height_px,
            SWP_SHOWWINDOW,
        );
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetTimer(hwnd, TIMER_ANIM, 8, None);
    }

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while !state.done.load(Ordering::Acquire) {
        let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if ret <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    let selected = state.selected;
    unsafe {
        KillTimer(hwnd, TIMER_ANIM);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        DestroyWindow(hwnd);
    }
    if selected.is_none() {
        record_closed(tag);
    }
    selected
}

fn draw_bgra_icon(hdc: HDC, icon: &FlyoutIcon, bg_rgb: u32, dst_x: i32, dst_y: i32, dst_sz: i32) {
    let w = icon.width as usize;
    let h = icon.height as usize;
    if w == 0 || h == 0 || icon.bgra.len() < w * h * 4 {
        return;
    }
    let bg_r = bg_rgb & 0xff;
    let bg_g = (bg_rgb >> 8) & 0xff;
    let bg_b = (bg_rgb >> 16) & 0xff;

    // Composite straight BGRA onto the row background color for crisp anti-aliased edges.
    let mut flat = vec![0u8; w * h * 4];
    for (src, dst) in icon.bgra.chunks_exact(4).zip(flat.chunks_exact_mut(4)) {
        let a = u32::from(src[3]);
        let inv = 255 - a;
        dst[0] = ((u32::from(src[0]) * a + bg_b * inv + 127) / 255) as u8;
        dst[1] = ((u32::from(src[1]) * a + bg_g * inv + 127) / 255) as u8;
        dst[2] = ((u32::from(src[2]) * a + bg_r * inv + 127) / 255) as u8;
        dst[3] = 255;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: icon.width as i32,
            biHeight: -(icon.height as i32), // top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        },
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        StretchDIBits(
            hdc,
            dst_x,
            dst_y,
            dst_sz,
            dst_sz,
            0,
            0,
            icon.width as i32,
            icon.height as i32,
            flat.as_ptr().cast(),
            &bmi,
            DIB_RGB_COLORS,
            0x00CC_0020, // SRCCOPY
        );
    }
}

impl FlyoutState<'_> {
    fn begin_close(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.animating = true;
        self.anim_start = Instant::now();
        unsafe {
            SetTimer(self.hwnd, TIMER_ANIM, 8, None);
        }
    }

    fn step_animation(&mut self) {
        if !self.animating {
            return;
        }
        let elapsed = self.anim_start.elapsed().as_secs_f32() * 1000.0;
        if !self.closing {
            let t = (elapsed / OPEN_ANIM_MS).clamp(0.0, 1.0);
            // Smooth cubic ease-out: fast start, gentle settle
            let eased = 1.0 - (1.0 - t).powi(3);
            let alpha = (eased * 255.0).round() as u8;
            let cur_y = self.target_y_px + ((1.0 - eased) * self.slide_px as f32).round() as i32;
            unsafe {
                let _ = SetLayeredWindowAttributes(self.hwnd, 0, alpha, LWA_ALPHA);
                SetWindowPos(
                    self.hwnd,
                    std::ptr::null_mut(),
                    self.x_px,
                    cur_y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            if t >= 1.0 {
                self.animating = false;
                unsafe {
                    KillTimer(self.hwnd, TIMER_ANIM);
                }
            }
        } else {
            let t = (elapsed / CLOSE_ANIM_MS).clamp(0.0, 1.0);
            // Smooth quadratic ease-in fade + slight drop
            let eased = t * t;
            let alpha = ((1.0 - eased) * 255.0).round() as u8;
            let cur_y = self.target_y_px + (eased * (self.slide_px as f32 * 0.75)).round() as i32;
            unsafe {
                let _ = SetLayeredWindowAttributes(self.hwnd, 0, alpha, LWA_ALPHA);
                SetWindowPos(
                    self.hwnd,
                    std::ptr::null_mut(),
                    self.x_px,
                    cur_y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            if t >= 1.0 {
                self.animating = false;
                unsafe {
                    KillTimer(self.hwnd, TIMER_ANIM);
                }
                self.done.store(true, Ordering::Release);
            }
        }
    }

    fn max_scroll(&self) -> i32 {
        (self.content_h_px - self.viewport_h_px).max(0)
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<u32> {
        if y < self.content_top_px || y > self.content_top_px + self.viewport_h_px {
            return None;
        }
        let cy = y + self.scroll_y;
        self.targets
            .iter()
            .find(|t| pt_in_rect(&t.rect, x, cy))
            .map(|t| t.id)
    }

    fn paint(&self) {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if hdc.is_null() {
            return;
        }
        let dpi = self.dpi;
        let s = |v: f32| (v * dpi).round() as i32;
        let pad = s(12.0);
        let right_pad = pad + s(6.0);
        let radius = s(8.0);
        let w = ps.rcPaint.right;

        let (
            bg,
            card_bg,
            hover_bg,
            accent,
            text_fg,
            muted_fg,
            danger_bg,
            danger_fg,
            sep_col,
            scroll_col,
        ) = if self.is_dark {
            (
                rgb(0x14, 0x17, 0x20),
                rgb(0x1d, 0x22, 0x30),
                rgb(0x28, 0x30, 0x42),
                rgb(0x0a, 0x84, 0xff),
                rgb(0xf1, 0xf5, 0xf9),
                rgb(0x94, 0xa3, 0xb8),
                rgb(0x7f, 0x1d, 0x1d),
                rgb(0xfc, 0xa5, 0xa5),
                rgb(0x27, 0x2d, 0x3d),
                rgb(0x47, 0x55, 0x69),
            )
        } else {
            (
                rgb(0xf8, 0xfa, 0xfc),
                rgb(0xee, 0xf2, 0xf7),
                rgb(0xe2, 0xe8, 0xf0),
                rgb(0x00, 0x7a, 0xff),
                rgb(0x0f, 0x17, 0x2a),
                rgb(0x47, 0x55, 0x69),
                rgb(0xfe, 0xe2, 0xe2),
                rgb(0xb9, 0x1c, 0x1c),
                rgb(0xe2, 0xe8, 0xf0),
                rgb(0x94, 0xa3, 0xb8),
            )
        };

        unsafe {
            let bg_brush = CreateSolidBrush(bg);
            FillRect(hdc, &ps.rcPaint, bg_brush);
            DeleteObject(bg_brush as _);

            let fname = wide("Segoe UI");
            let title_font = CreateFontW(
                -s(14.5),
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
                5,
                0,
                fname.as_ptr(),
            );
            let body_font = CreateFontW(
                -s(13.0),
                0,
                0,
                0,
                500,
                0,
                0,
                0,
                0,
                0,
                0,
                5,
                0,
                fname.as_ptr(),
            );
            let small_font = CreateFontW(
                -s(11.0),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                0,
                0,
                0,
                5,
                0,
                fname.as_ptr(),
            );
            SetBkMode(hdc, TRANSPARENT as i32);

            // Fixed header
            let old_f = SelectObject(hdc, title_font as _);
            SetTextColor(hdc, text_fg);
            let mut hrc = RECT {
                left: pad,
                top: pad,
                right: w - pad,
                bottom: pad + s(22.0),
            };
            let mut tw: Vec<u16> = self.spec.title.encode_utf16().collect();
            DrawTextW(
                hdc,
                tw.as_mut_ptr(),
                tw.len() as i32,
                &mut hrc,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );

            if let Some(ref sub) = self.spec.subtitle {
                SelectObject(hdc, small_font as _);
                SetTextColor(hdc, muted_fg);
                let mut src = RECT {
                    left: pad,
                    top: pad + s(24.0),
                    right: w - pad,
                    bottom: pad + s(42.0),
                };
                let mut sw: Vec<u16> = sub.encode_utf16().collect();
                DrawTextW(
                    hdc,
                    sw.as_mut_ptr(),
                    sw.len() as i32,
                    &mut src,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                );
            }

            // Clipped scrollable content region
            let saved = SaveDC(hdc);
            IntersectClipRect(
                hdc,
                0,
                self.content_top_px,
                w,
                self.content_top_px + self.viewport_h_px,
            );

            let mut y = self.content_top_px - self.scroll_y;
            for block in &self.spec.blocks {
                match block {
                    FlyoutBlock::StatRow { label, value } => {
                        let h = s(26.0);
                        SelectObject(hdc, body_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut rc = RECT {
                            left: pad,
                            top: y,
                            right: w - right_pad,
                            bottom: y + h,
                        };
                        let mut lw: Vec<u16> = label.encode_utf16().collect();
                        DrawTextW(
                            hdc,
                            lw.as_mut_ptr(),
                            lw.len() as i32,
                            &mut rc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                        );
                        SetTextColor(hdc, text_fg);
                        let mut vw: Vec<u16> = value.encode_utf16().collect();
                        DrawTextW(
                            hdc,
                            vw.as_mut_ptr(),
                            vw.len() as i32,
                            &mut rc,
                            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                        );
                        y += h;
                    }
                    FlyoutBlock::ProgressBar {
                        label,
                        value_text,
                        ratio,
                    } => {
                        let h = s(38.0);
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut rc = RECT {
                            left: pad,
                            top: y,
                            right: w - right_pad,
                            bottom: y + s(18.0),
                        };
                        let mut lw: Vec<u16> = label.encode_utf16().collect();
                        DrawTextW(
                            hdc,
                            lw.as_mut_ptr(),
                            lw.len() as i32,
                            &mut rc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                        );
                        SetTextColor(hdc, text_fg);
                        let mut vw: Vec<u16> = value_text.encode_utf16().collect();
                        DrawTextW(
                            hdc,
                            vw.as_mut_ptr(),
                            vw.len() as i32,
                            &mut rc,
                            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                        );

                        let bar_top = y + s(22.0);
                        let bar_h = s(7.0);
                        let tb = CreateSolidBrush(card_bg);
                        let tp = CreatePen(PS_SOLID, 1, card_bg);
                        let ob = SelectObject(hdc, tb as _);
                        let op = SelectObject(hdc, tp as _);
                        RoundRect(
                            hdc,
                            pad,
                            bar_top,
                            w - right_pad,
                            bar_top + bar_h,
                            s(4.0),
                            s(4.0),
                        );
                        SelectObject(hdc, ob);
                        SelectObject(hdc, op);
                        DeleteObject(tb as _);
                        DeleteObject(tp as _);

                        let fill_w =
                            (((w - pad - right_pad) as f32) * ratio.clamp(0.0, 1.0)).round() as i32;
                        if fill_w > s(4.0) {
                            let fb = CreateSolidBrush(accent);
                            let fp = CreatePen(PS_SOLID, 1, accent);
                            let ob = SelectObject(hdc, fb as _);
                            let op = SelectObject(hdc, fp as _);
                            RoundRect(
                                hdc,
                                pad,
                                bar_top,
                                pad + fill_w,
                                bar_top + bar_h,
                                s(4.0),
                                s(4.0),
                            );
                            SelectObject(hdc, ob);
                            SelectObject(hdc, op);
                            DeleteObject(fb as _);
                            DeleteObject(fp as _);
                        }
                        y += h;
                    }
                    FlyoutBlock::ActionRow {
                        id,
                        title,
                        subtitle,
                        badge,
                        icon,
                        destructive,
                    } => {
                        let h = if subtitle.is_some() { s(40.0) } else { s(32.0) };
                        let hovered = self.hovered_id == Some(*id);
                        let fill = if hovered {
                            if *destructive { danger_bg } else { hover_bg }
                        } else {
                            card_bg
                        };
                        let fb = CreateSolidBrush(fill);
                        let fp = CreatePen(PS_SOLID, 1, fill);
                        let ob = SelectObject(hdc, fb as _);
                        let op = SelectObject(hdc, fp as _);
                        RoundRect(hdc, pad, y, w - right_pad, y + h, radius, radius);
                        SelectObject(hdc, ob);
                        SelectObject(hdc, op);
                        DeleteObject(fb as _);
                        DeleteObject(fp as _);

                        let mut text_left = pad + s(10.0);
                        if let Some(ic) = icon {
                            let isz = s(18.0);
                            draw_bgra_icon(hdc, ic, fill, pad + s(8.0), y + (h - isz) / 2, isz);
                            text_left = pad + s(34.0);
                        }

                        SelectObject(hdc, body_font as _);
                        SetTextColor(hdc, if *destructive { danger_fg } else { text_fg });
                        let mut trc = RECT {
                            left: text_left,
                            top: y + if subtitle.is_some() { s(3.0) } else { 0 },
                            right: w - right_pad - s(10.0),
                            bottom: y + if subtitle.is_some() { s(21.0) } else { h },
                        };
                        let mut tw: Vec<u16> = title.encode_utf16().collect();
                        DrawTextW(
                            hdc,
                            tw.as_mut_ptr(),
                            tw.len() as i32,
                            &mut trc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );

                        if let Some(sub) = subtitle {
                            SelectObject(hdc, small_font as _);
                            SetTextColor(hdc, muted_fg);
                            let mut src = RECT {
                                left: text_left,
                                top: y + s(20.0),
                                right: w - right_pad - s(10.0),
                                bottom: y + h - s(3.0),
                            };
                            let mut sw: Vec<u16> = sub.encode_utf16().collect();
                            DrawTextW(
                                hdc,
                                sw.as_mut_ptr(),
                                sw.len() as i32,
                                &mut src,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                        }
                        if let Some(b) = badge {
                            SelectObject(hdc, small_font as _);
                            SetTextColor(hdc, muted_fg);
                            let mut brc = RECT {
                                left: text_left,
                                top: y,
                                right: w - right_pad - s(10.0),
                                bottom: y + h,
                            };
                            let mut bw: Vec<u16> = b.encode_utf16().collect();
                            DrawTextW(
                                hdc,
                                bw.as_mut_ptr(),
                                bw.len() as i32,
                                &mut brc,
                                DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                            );
                        }
                        y += h + s(4.0);
                    }
                    FlyoutBlock::ButtonBar(buttons) => {
                        let h = s(30.0);
                        let count = buttons.len().max(1) as i32;
                        let gap = s(6.0);
                        let avail = w - pad - right_pad - gap * (count - 1);
                        let bw = (avail / count).max(s(40.0));
                        for (i, (id, label, primary)) in buttons.iter().enumerate() {
                            let left = pad + (i as i32) * (bw + gap);
                            let hovered = self.hovered_id == Some(*id);
                            let fill = if *primary {
                                accent
                            } else if hovered {
                                hover_bg
                            } else {
                                card_bg
                            };
                            let fb = CreateSolidBrush(fill);
                            let fp = CreatePen(PS_SOLID, 1, fill);
                            let ob = SelectObject(hdc, fb as _);
                            let op = SelectObject(hdc, fp as _);
                            RoundRect(hdc, left, y, left + bw, y + h, radius, radius);
                            SelectObject(hdc, ob);
                            SelectObject(hdc, op);
                            DeleteObject(fb as _);
                            DeleteObject(fp as _);

                            SelectObject(hdc, body_font as _);
                            SetTextColor(
                                hdc,
                                if *primary {
                                    rgb(0xff, 0xff, 0xff)
                                } else {
                                    text_fg
                                },
                            );
                            let mut brc = RECT {
                                left,
                                top: y,
                                right: left + bw,
                                bottom: y + h,
                            };
                            let mut lw: Vec<u16> = label.encode_utf16().collect();
                            DrawTextW(
                                hdc,
                                lw.as_mut_ptr(),
                                lw.len() as i32,
                                &mut brc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                        }
                        y += h + s(6.0);
                    }
                    FlyoutBlock::Separator => {
                        let pen = CreatePen(PS_SOLID, 1, sep_col);
                        let op = SelectObject(hdc, pen as _);
                        let sy = y + s(5.0);
                        MoveToEx(hdc, pad, sy, std::ptr::null_mut());
                        LineTo(hdc, w - right_pad, sy);
                        SelectObject(hdc, op);
                        DeleteObject(pen as _);
                        y += s(10.0);
                    }
                    FlyoutBlock::Note(text) => {
                        let h = s(26.0);
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut rc = RECT {
                            left: pad,
                            top: y,
                            right: w - right_pad,
                            bottom: y + h,
                        };
                        let mut nw: Vec<u16> = text.encode_utf16().collect();
                        DrawTextW(
                            hdc,
                            nw.as_mut_ptr(),
                            nw.len() as i32,
                            &mut rc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );
                        y += h;
                    }
                }
            }
            RestoreDC(hdc, saved);

            // Visible thin scrollbar thumb when content overflows
            let max_s = self.max_scroll();
            if max_s > 0 && self.content_h_px > 0 {
                let track_h = self.viewport_h_px;
                let thumb_h = ((track_h as f32 * track_h as f32 / self.content_h_px as f32) as i32)
                    .clamp(s(22.0), track_h);
                let ratio = self.scroll_y as f32 / max_s as f32;
                let thumb_top = self.content_top_px + ((track_h - thumb_h) as f32 * ratio) as i32;
                let sb_w = s(4.0).max(3);
                let sb_left = w - s(8.0);
                let sb_brush = CreateSolidBrush(scroll_col);
                let sb_pen = CreatePen(PS_SOLID, 1, scroll_col);
                let ob = SelectObject(hdc, sb_brush as _);
                let op = SelectObject(hdc, sb_pen as _);
                RoundRect(
                    hdc,
                    sb_left,
                    thumb_top,
                    sb_left + sb_w,
                    thumb_top + thumb_h,
                    sb_w,
                    sb_w,
                );
                SelectObject(hdc, ob);
                SelectObject(hdc, op);
                DeleteObject(sb_brush as _);
                DeleteObject(sb_pen as _);
            }

            SelectObject(hdc, old_f);
            DeleteObject(title_font as _);
            DeleteObject(body_font as _);
            DeleteObject(small_font as _);
            EndPaint(self.hwnd, &ps);
        }
    }
}

unsafe extern "system" fn flyout_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FlyoutState<'_> };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };
    match msg {
        WM_PAINT => {
            state.paint();
            0
        }
        WM_SETCURSOR => {
            let cid = if state.hovered_id.is_some() {
                IDC_HAND
            } else {
                IDC_ARROW
            };
            unsafe {
                SetCursor(LoadCursorW(std::ptr::null_mut(), cid));
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
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            let hit = state.hit_at(x, y);
            if hit != state.hovered_id {
                state.hovered_id = hit;
                unsafe {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            state.tracking = false;
            if state.hovered_id.take().is_some() {
                unsafe {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            let max_s = state.max_scroll();
            if max_s > 0 {
                let delta = ((wparam >> 16) & 0xffff) as i16;
                let step = (36.0 * state.dpi).round() as i32;
                let next = (state.scroll_y - (i32::from(delta) * step) / 120).clamp(0, max_s);
                if next != state.scroll_y {
                    state.scroll_y = next;
                    unsafe {
                        InvalidateRect(hwnd, std::ptr::null(), 0);
                    }
                }
            }
            0
        }
        WM_TIMER if wparam == TIMER_ANIM => {
            state.step_animation();
            0
        }
        WM_LBUTTONUP => {
            if !state.closing {
                let x = (lparam as u32 & 0xffff) as i16 as i32;
                let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
                if let Some(id) = state.hit_at(x, y) {
                    state.selected = Some(FlyoutAction::Primary(id));
                }
                state.begin_close();
            }
            0
        }
        WM_RBUTTONUP => {
            if !state.closing {
                let x = (lparam as u32 & 0xffff) as i16 as i32;
                let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
                if let Some(id) = state.hit_at(x, y) {
                    if let Some(cb) = state.on_secondary.as_deref_mut() {
                        state.suppress_blur = true;
                        let should_close = cb(id);
                        state.suppress_blur = false;
                        if should_close {
                            state.begin_close();
                        } else {
                            unsafe {
                                SetForegroundWindow(hwnd);
                            }
                        }
                    } else {
                        state.selected = Some(FlyoutAction::Secondary(id));
                        state.begin_close();
                    }
                }
            }
            0
        }
        WM_KEYDOWN if wparam == VK_ESCAPE as usize => {
            state.begin_close();
            0
        }
        WM_KILLFOCUS => {
            if !state.suppress_blur {
                state.begin_close();
            }
            0
        }
        WM_ACTIVATE if (wparam & 0xffff) == 0 => {
            if !state.suppress_blur {
                state.begin_close();
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
