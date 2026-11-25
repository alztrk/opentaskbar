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

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, CreateFontW, CreatePen, CreateSolidBrush,
    DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, GetMonitorInfoW, HDC,
    IntersectClipRect, InvalidateRect, LineTo, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, MoveToEx, PAINTSTRUCT, PS_SOLID, RestoreDC, RoundRect, SaveDC, SelectObject,
    SetBkMode, SetTextColor, StretchDIBits, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_RETURN, VK_SHIFT,
    VK_SPACE, VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect,
    GetMessageW, GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IDC_HAND, KillTimer, LWA_ALPHA,
    LoadCursorW, MSG, RegisterClassExW, SW_SHOW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    SWP_SHOWWINDOW, SetCursor, SetForegroundWindow, SetLayeredWindowAttributes, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WM_ACTIVATE, WM_KEYDOWN,
    WM_KILLFOCUS, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR,
    WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::menu::{MenuIcon, draw_menu_icon};
use crate::design::{self, gdi_rgb};
use crate::platform::win32;

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
    /// Large 28px hero metric (Clock time, Weather temp, Pomodoro/Stopwatch timer).
    Hero {
        value: String,
        caption: Option<String>,
    },
    /// 2-column Control Center toggle tile grid (`(id, title, state_label, active)`).
    ToggleGrid(Vec<(u32, String, String, bool)>),
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
    AppRow {
        id: u32,
        title: String,
        icon: Option<FlyoutIcon>,
        pin_id: u32,
        pinned: bool,
        close_id: Option<u32>,
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
    menu_id: u32,
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
    keyboard_index: Option<usize>,
    keyboard_navigation: bool,
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

fn app_row_targets(left: i32, right: i32, top: i32, dpi: f32) -> [RECT; 3] {
    let action_w = (36.0 * dpi).round() as i32;
    let bottom = top + (42.0 * dpi).round() as i32;
    let pin_left = right - action_w * 2;
    let close_left = right - action_w;
    [
        (left, pin_left),
        (pin_left, close_left),
        (close_left, right),
    ]
    .map(|(left, right)| RECT {
        left,
        top,
        right,
        bottom,
    })
}

fn fill_round_rect(hdc: HDC, l: i32, t: i32, r: i32, b: i32, rad: i32, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let pen = CreatePen(PS_SOLID, 1, color);
        let ob = SelectObject(hdc, brush as _);
        let op = SelectObject(hdc, pen as _);
        RoundRect(hdc, l, t, r, b, rad, rad);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        DeleteObject(brush as _);
        DeleteObject(pen as _);
    }
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
            FlyoutBlock::Hero { caption, .. } => {
                y += if caption.is_some() { s(54.0) } else { s(38.0) };
            }
            FlyoutBlock::ToggleGrid(tiles) => {
                let rows = tiles.len().div_ceil(2) as i32;
                let tile_h = s(48.0);
                let gap = s(6.0);
                let avail = width_px - pad - content_right_pad - gap;
                let tw = (avail / 2).max(s(80.0));
                for (i, (id, _, _, _)) in tiles.iter().enumerate() {
                    let col = (i % 2) as i32;
                    let row = (i / 2) as i32;
                    let left = pad + col * (tw + gap);
                    let top = y + row * (tile_h + gap);
                    targets.push(HitTarget {
                        id: *id,
                        menu_id: *id,
                        rect: RECT {
                            left,
                            top,
                            right: left + tw,
                            bottom: top + tile_h,
                        },
                    });
                }
                y += rows * tile_h + (rows - 1).max(0) * gap + s(6.0);
            }
            FlyoutBlock::StatRow { .. } => y += s(26.0),
            FlyoutBlock::ProgressBar { .. } => y += s(38.0),
            FlyoutBlock::ActionRow { id, subtitle, .. } => {
                let h = if subtitle.is_some() { s(40.0) } else { s(32.0) };
                targets.push(HitTarget {
                    id: *id,
                    menu_id: *id,
                    rect: RECT {
                        left: pad,
                        top: y,
                        right: width_px - content_right_pad,
                        bottom: y + h,
                    },
                });
                y += h + s(4.0);
            }
            FlyoutBlock::AppRow {
                id: menu_id,
                pin_id,
                close_id,
                ..
            } => {
                let rects = app_row_targets(pad, width_px - content_right_pad, y, dpi);
                for (id, rect) in [Some(*menu_id), Some(*pin_id), *close_id]
                    .into_iter()
                    .zip(rects)
                {
                    if let Some(id) = id {
                        targets.push(HitTarget {
                            id,
                            rect,
                            menu_id: *menu_id,
                        });
                    }
                }
                y += s(42.0) + s(2.0);
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
                        menu_id: *id,
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

    let is_tray = tag == "tray";
    let (scr_w, scr_h) = win32::primary_screen_px();
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let screen = if is_tray
        && unsafe {
            let anchor = POINT {
                x: anchor_center_x_px,
                y: anchor_bottom_y_px,
            };
            GetMonitorInfoW(
                MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            ) != 0
        } {
        monitor.rcMonitor
    } else {
        RECT {
            right: scr_w,
            bottom: scr_h,
            ..Default::default()
        }
    };
    let margin = s(10.0);
    let left = screen.left + margin;
    let top = screen.top + margin;
    let x = (anchor_center_x_px - width_px / 2)
        .clamp(left, (screen.right - width_px - margin).max(left));
    let top_y =
        (anchor_bottom_y_px - height_px).clamp(top, (screen.bottom - height_px - margin).max(top));

    let animate = !is_tray || win32::client_area_animations_enabled();
    let slide_px = if animate { s(SLIDE_OFFSET_DIPS) } else { 0 };
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            class_name.as_ptr(),
            wide(if is_tray { &spec.title } else { "" }).as_ptr(),
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
        let _ = SetLayeredWindowAttributes(hwnd, 0, if animate { 0 } else { 255 }, LWA_ALPHA);
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let border = gdi_rgb(design::palette(is_dark).border_rgb);
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
        animating: animate,
        suppress_blur: false,
        on_secondary,
        content_top_px,
        viewport_h_px,
        content_h_px,
        scroll_y: 0,
        targets,
        hovered_id: None,
        keyboard_index: None,
        keyboard_navigation: is_tray,
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
        if animate {
            SetTimer(hwnd, TIMER_ANIM, 8, None);
        }
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
    let sw = icon.width as usize;
    let sh = icon.height as usize;
    let dw = dst_sz.max(1) as usize;
    let dh = dw;
    if sw == 0 || sh == 0 || icon.bgra.len() < sw * sh * 4 {
        return;
    }
    let bg_r = (bg_rgb & 0xff) as f32;
    let bg_g = ((bg_rgb >> 8) & 0xff) as f32;
    let bg_b = ((bg_rgb >> 16) & 0xff) as f32;

    // Premultiplied-alpha area-box filter for razor-sharp downsampling (replaces jagged GDI StretchDIBits).
    let mut flat = vec![0u8; dw * dh * 4];
    let scale_x = sw as f32 / dw as f32;
    let scale_y = sh as f32 / dh as f32;

    for dy in 0..dh {
        let sy0 = (dy as f32 * scale_y).floor() as usize;
        let sy1 = (((dy + 1) as f32 * scale_y).ceil() as usize).clamp(sy0 + 1, sh);
        for dx in 0..dw {
            let sx0 = (dx as f32 * scale_x).floor() as usize;
            let sx1 = (((dx + 1) as f32 * scale_x).ceil() as usize).clamp(sx0 + 1, sw);
            let (mut pb, mut pg, mut pr, mut pa) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let mut count = 0.0f32;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let off = (sy * sw + sx) * 4;
                    let a = f32::from(icon.bgra[off + 3]) / 255.0;
                    pb += f32::from(icon.bgra[off]) * a;
                    pg += f32::from(icon.bgra[off + 1]) * a;
                    pr += f32::from(icon.bgra[off + 2]) * a;
                    pa += a;
                    count += 1.0;
                }
            }
            let inv_n = 1.0 / count.max(1.0);
            let a = (pa * inv_n).clamp(0.0, 1.0);
            let inv_a = 1.0 - a;
            let out_b = (pb * inv_n + bg_b * inv_a).round().clamp(0.0, 255.0) as u8;
            let out_g = (pg * inv_n + bg_g * inv_a).round().clamp(0.0, 255.0) as u8;
            let out_r = (pr * inv_n + bg_r * inv_a).round().clamp(0.0, 255.0) as u8;
            let dst = (dy * dw + dx) * 4;
            flat[dst] = out_b;
            flat[dst + 1] = out_g;
            flat[dst + 2] = out_r;
            flat[dst + 3] = 255;
        }
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: dw as i32,
            biHeight: -(dh as i32),
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
            dw as i32,
            dh as i32,
            0,
            0,
            dw as i32,
            dh as i32,
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
        if self.slide_px == 0 {
            self.done.store(true, Ordering::Release);
            return;
        }
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

    fn hit_at(&self, x: i32, y: i32) -> Option<&HitTarget> {
        if y < self.content_top_px || y >= self.content_top_px + self.viewport_h_px {
            return None;
        }
        let cy = y + self.scroll_y;
        self.targets.iter().find(|t| pt_in_rect(&t.rect, x, cy))
    }

    fn focus_next_target(&mut self, backwards: bool) {
        let count = self.targets.len();
        if count == 0 {
            return;
        }
        let index = match (self.keyboard_index, backwards) {
            (Some(index), false) => (index + 1) % count,
            (Some(index), true) => (index + count - 1) % count,
            (None, false) => 0,
            (None, true) => count - 1,
        };
        self.keyboard_index = Some(index);
        let rect = self.targets[index].rect;
        let start = rect.bottom - self.content_top_px - self.viewport_h_px;
        let end = rect.top - self.content_top_px;
        self.scroll_y = self
            .scroll_y
            .max(start)
            .min(end)
            .clamp(0, self.max_scroll());
        unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
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
        let mut client = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut client) };
        let w = client.right;
        let focused_id = self.keyboard_index.map(|index| self.targets[index].id);

        let pal = design::palette(self.is_dark);
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
        ) = (
            gdi_rgb(pal.surface),
            gdi_rgb(pal.elevated),
            gdi_rgb(pal.hover),
            gdi_rgb(pal.accent),
            gdi_rgb(pal.text_primary),
            gdi_rgb(pal.text_secondary),
            gdi_rgb(pal.danger_hover_bg),
            gdi_rgb(pal.danger_fg),
            gdi_rgb(pal.sep_rgb),
            gdi_rgb(pal.text_secondary),
        );

        unsafe {
            let bg_brush = CreateSolidBrush(bg);
            FillRect(hdc, &ps.rcPaint, bg_brush);
            DeleteObject(bg_brush as _);

            let fname = wide("Segoe UI");
            let title_font = CreateFontW(
                -s(if self.keyboard_navigation { 18.0 } else { 14.5 }),
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

            let action_label = self.spec.blocks.iter().find_map(|block| match block {
                FlyoutBlock::AppRow {
                    pin_id,
                    close_id,
                    pinned,
                    ..
                } => {
                    let active = self.hovered_id.or(focused_id);
                    if active == Some(*pin_id) {
                        Some(if *pinned {
                            "Unpin background app"
                        } else {
                            "Pin next to Background Apps"
                        })
                    } else if active.is_some() && active == *close_id {
                        Some("Close app window")
                    } else {
                        None
                    }
                }
                _ => None,
            });
            if let Some(sub) = action_label.or(self.spec.subtitle.as_deref()) {
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
                    FlyoutBlock::Hero { value, caption } => {
                        let hero_font = CreateFontW(
                            -s(28.0),
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
                        SelectObject(hdc, hero_font as _);
                        SetTextColor(hdc, text_fg);
                        let mut hrc = RECT {
                            left: pad,
                            top: y,
                            right: w - right_pad,
                            bottom: y + s(34.0),
                        };
                        let mut vw: Vec<u16> = value.encode_utf16().collect();
                        DrawTextW(
                            hdc,
                            vw.as_mut_ptr(),
                            vw.len() as i32,
                            &mut hrc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                        );
                        DeleteObject(hero_font as _);
                        y += s(36.0);
                        if let Some(cap) = caption {
                            SelectObject(hdc, small_font as _);
                            SetTextColor(hdc, muted_fg);
                            let mut crc = RECT {
                                left: pad,
                                top: y,
                                right: w - right_pad,
                                bottom: y + s(16.0),
                            };
                            let mut cw: Vec<u16> = cap.encode_utf16().collect();
                            DrawTextW(
                                hdc,
                                cw.as_mut_ptr(),
                                cw.len() as i32,
                                &mut crc,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                            y += s(18.0);
                        }
                    }
                    FlyoutBlock::ToggleGrid(tiles) => {
                        let rows = tiles.len().div_ceil(2) as i32;
                        let tile_h = s(48.0);
                        let gap = s(6.0);
                        let avail = w - pad - right_pad - gap;
                        let tw = (avail / 2).max(s(80.0));
                        for (i, (id, title, state_lbl, active)) in tiles.iter().enumerate() {
                            let col = (i % 2) as i32;
                            let row = (i / 2) as i32;
                            let left = pad + col * (tw + gap);
                            let top = y + row * (tile_h + gap);
                            let hovered = self.hovered_id == Some(*id);
                            let fill = if *active {
                                accent
                            } else if hovered {
                                hover_bg
                            } else {
                                card_bg
                            };
                            fill_round_rect(hdc, left, top, left + tw, top + tile_h, radius, fill);
                            SelectObject(hdc, body_font as _);
                            SetTextColor(
                                hdc,
                                if *active {
                                    rgb(0xff, 0xff, 0xff)
                                } else {
                                    text_fg
                                },
                            );
                            let mut trc = RECT {
                                left: left + s(10.0),
                                top: top + s(6.0),
                                right: left + tw - s(8.0),
                                bottom: top + s(24.0),
                            };
                            let mut tw_u16: Vec<u16> = title.encode_utf16().collect();
                            DrawTextW(
                                hdc,
                                tw_u16.as_mut_ptr(),
                                tw_u16.len() as i32,
                                &mut trc,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                            SelectObject(hdc, small_font as _);
                            SetTextColor(
                                hdc,
                                if *active {
                                    rgb(0xe2, 0xe8, 0xf0)
                                } else {
                                    muted_fg
                                },
                            );
                            let mut src = RECT {
                                left: left + s(10.0),
                                top: top + s(25.0),
                                right: left + tw - s(8.0),
                                bottom: top + tile_h - s(6.0),
                            };
                            let mut sw_u16: Vec<u16> = state_lbl.encode_utf16().collect();
                            DrawTextW(
                                hdc,
                                sw_u16.as_mut_ptr(),
                                sw_u16.len() as i32,
                                &mut src,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                        }
                        y += rows * tile_h + (rows - 1).max(0) * gap + s(6.0);
                    }
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

                        let bar_top = y + s(23.0);
                        let bar_h = s(5.0);
                        fill_round_rect(
                            hdc,
                            pad,
                            bar_top,
                            w - right_pad,
                            bar_top + bar_h,
                            s(3.0),
                            card_bg,
                        );

                        let fill_w =
                            (((w - pad - right_pad) as f32) * ratio.clamp(0.0, 1.0)).round() as i32;
                        if fill_w > s(3.0) {
                            let meter_col = gdi_rgb(design::meter_color(&pal, *ratio));
                            fill_round_rect(
                                hdc,
                                pad,
                                bar_top,
                                pad + fill_w,
                                bar_top + bar_h,
                                s(3.0),
                                meter_col,
                            );
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
                        fill_round_rect(hdc, pad, y, w - right_pad, y + h, radius, fill);

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
                    FlyoutBlock::AppRow {
                        id,
                        title,
                        icon,
                        pin_id,
                        pinned,
                        close_id,
                    } => {
                        let [app, pin, close] = app_row_targets(pad, w - right_pad, y, dpi);
                        fill_round_rect(
                            hdc,
                            app.left,
                            y,
                            close.right,
                            app.bottom,
                            s(13.0),
                            card_bg,
                        );

                        let app_fill = if self.hovered_id == Some(*id) || focused_id == Some(*id) {
                            hover_bg
                        } else {
                            card_bg
                        };
                        fill_round_rect(hdc, app.left, y, app.right, app.bottom, s(13.0), app_fill);
                        let icon_size = s(22.0);
                        let icon_x = app.left + s(10.0);
                        let icon_y = y + (app.bottom - y - icon_size) / 2;
                        if let Some(icon) = icon {
                            draw_bgra_icon(hdc, icon, app_fill, icon_x, icon_y, icon_size);
                        } else {
                            draw_menu_icon(
                                hdc,
                                MenuIcon::Layers,
                                icon_x + icon_size / 2,
                                icon_y + icon_size / 2,
                                dpi,
                                muted_fg,
                            );
                        }
                        SelectObject(hdc, body_font as _);
                        SetTextColor(hdc, text_fg);
                        let mut rect = RECT {
                            left: app.left + s(42.0),
                            right: app.right - s(6.0),
                            ..app
                        };
                        let mut title: Vec<u16> = title.encode_utf16().collect();
                        DrawTextW(
                            hdc,
                            title.as_mut_ptr(),
                            title.len() as i32,
                            &mut rect,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                        );

                        for (id, rect, icon, selected, fill, ink) in [
                            (Some(*pin_id), pin, MenuIcon::Pin, *pinned, hover_bg, accent),
                            (
                                *close_id,
                                close,
                                MenuIcon::Close,
                                false,
                                danger_bg,
                                danger_fg,
                            ),
                        ] {
                            let Some(id) = id else { continue };
                            let active = self.hovered_id == Some(id) || focused_id == Some(id);
                            let inset = s(3.0);
                            if active || selected {
                                fill_round_rect(
                                    hdc,
                                    rect.left + inset,
                                    y + inset,
                                    rect.right - inset,
                                    rect.bottom - inset,
                                    s(11.0),
                                    fill,
                                );
                            }
                            draw_menu_icon(
                                hdc,
                                icon,
                                (rect.left + rect.right) / 2,
                                (y + rect.bottom) / 2,
                                dpi,
                                if active || selected { ink } else { muted_fg },
                            );
                        }
                        y = app.bottom + s(2.0);
                    }
                    FlyoutBlock::ButtonBar(buttons) => {
                        let h = s(30.0);
                        let count = buttons.len().max(1) as i32;
                        let gap = s(6.0);
                        let avail = w - pad - right_pad - gap * (count - 1);
                        let bw = (avail / count).max(s(40.0));
                        for (i, (id, label, primary)) in buttons.iter().enumerate() {
                            let left = pad + (i as i32) * (bw + gap);
                            let hovered = self.hovered_id == Some(*id) || focused_id == Some(*id);
                            let fill = if *primary {
                                accent
                            } else if hovered {
                                hover_bg
                            } else {
                                card_bg
                            };
                            fill_round_rect(hdc, left, y, left + bw, y + h, radius, fill);

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
            if let Some(index) = self.keyboard_index {
                let mut rect = self.targets[index].rect;
                rect.left += s(2.0);
                rect.right -= s(2.0);
                rect.top += s(2.0) - self.scroll_y;
                rect.bottom -= s(2.0) + self.scroll_y;
                let brush = CreateSolidBrush(accent);
                windows_sys::Win32::Graphics::Gdi::FrameRect(hdc, &rect, brush);
                DeleteObject(brush as _);
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
                fill_round_rect(
                    hdc,
                    sb_left,
                    thumb_top,
                    sb_left + sb_w,
                    thumb_top + thumb_h,
                    sb_w,
                    scroll_col,
                );
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
            let hit = state.hit_at(x, y).map(|target| target.id);
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
                    if state.keyboard_navigation {
                        state.hovered_id = None;
                    }
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
                if let Some(target) = state.hit_at(x, y) {
                    state.selected = Some(FlyoutAction::Primary(target.id));
                    state.begin_close();
                } else if !state.keyboard_navigation {
                    state.begin_close();
                }
            }
            0
        }
        WM_RBUTTONUP => {
            if !state.closing {
                let x = (lparam as u32 & 0xffff) as i16 as i32;
                let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
                if let Some(id) = state.hit_at(x, y).map(|target| target.menu_id) {
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
        WM_KEYDOWN if state.keyboard_navigation && !state.closing => {
            match wparam as u16 {
                VK_TAB => state.focus_next_target(unsafe { GetKeyState(VK_SHIFT as i32) } < 0),
                VK_RETURN | VK_SPACE => {
                    if let Some(index) = state.keyboard_index {
                        state.selected = Some(FlyoutAction::Primary(state.targets[index].id));
                        state.begin_close();
                    }
                }
                _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
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

#[cfg(test)]
mod tests {
    use super::{app_row_targets, pt_in_rect};

    #[test]
    fn background_app_targets_do_not_overlap_at_supported_scales() {
        for dpi in [1.0, 1.25, 1.5, 2.0] {
            let right = (300.0 * dpi) as i32;
            let [app, pin, close] = app_row_targets(12, right, 40, dpi);

            assert!(pt_in_rect(&app, pin.left - 1, 40));
            assert!(!pt_in_rect(&app, pin.left, 40));
            assert!(pt_in_rect(&pin, pin.left, 40));
            assert!(!pt_in_rect(&pin, close.left, 40));
            assert!(pt_in_rect(&close, close.left, 40));
            assert!(!pt_in_rect(&close, right, 40));
            assert!(!pt_in_rect(&close, close.left, close.bottom));
        }
    }
}
