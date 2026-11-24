//! Shared reusable iOS/shadcn-style popup flyout component (`DockFlyout`).
//!
//! Designed as a generic, pluggable surface host used across Background Apps (System Tray),
//! Folder Stacks, App Groups, and every Dock Widget (Clock, System Stats, Audio, Media,
//! Net Speed, Battery, Storage, Keyboard Layout, Day Progress, Stopwatch, Countdown, Focus,
//! Weather, Quick Settings). Each consumer simply supplies a `FlyoutSpec` of header + blocks
//! and handles returned action IDs.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect,
    InvalidateRect, LineTo, MoveToEx, PAINTSTRUCT, PS_SOLID, RoundRect, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW,
    GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IDC_HAND, LoadCursorW, MSG, RegisterClassExW,
    SW_SHOW, SWP_SHOWWINDOW, SetCursor, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, WM_ACTIVATE, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::win32;

const WM_MOUSELEAVE: u32 = 0x02A3;
const DWMWCP_ROUND: u32 = 2;

/// Declarative content block rendered inside the shared `DockFlyout`.
#[derive(Clone, Debug)]
pub enum FlyoutBlock {
    /// Key-value metric row (e.g. `CPU` -> `14%`, `Available` -> `118 GB`).
    StatRow { label: String, value: String },
    /// Labeled horizontal progress bar (`0.0..=1.0`).
    ProgressBar {
        label: String,
        value_text: String,
        ratio: f32,
    },
    /// Clickable list row (e.g. background app, folder file, app group member, keyboard layout).
    ActionRow {
        id: u32,
        title: String,
        subtitle: Option<String>,
        badge: Option<String>,
        destructive: bool,
    },
    /// Horizontal row of compact pill buttons (e.g. `Prev | Play/Pause | Next`, `Start | Reset`).
    ButtonBar(Vec<(u32, String, bool)>),
    /// Subtle hairline separator.
    Separator,
    /// Muted empty/info note.
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
    rect: RECT,
}

struct FlyoutState {
    hwnd: HWND,
    dpi: f32,
    is_dark: bool,
    spec: FlyoutSpec,
    targets: Vec<HitTarget>,
    hovered_id: Option<u32>,
    tracking: bool,
    selected_id: Option<u32>,
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

/// Shows the shared `DockFlyout` anchored above `(anchor_center_x_px, anchor_bottom_y_px)`
/// and returns the clicked action ID (if any).
pub fn show_flyout(
    owner_hwnd: usize,
    is_dark: bool,
    anchor_center_x_px: i32,
    anchor_bottom_y_px: i32,
    spec: FlyoutSpec,
) -> Option<u32> {
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
        s(46.0)
    } else {
        s(32.0)
    };

    let mut y = pad + header_h + s(6.0);
    let mut targets = Vec::new();
    for block in &spec.blocks {
        match block {
            FlyoutBlock::StatRow { .. } => y += s(26.0),
            FlyoutBlock::ProgressBar { .. } => y += s(38.0),
            FlyoutBlock::ActionRow { id, subtitle, .. } => {
                let h = if subtitle.is_some() { s(42.0) } else { s(32.0) };
                targets.push(HitTarget {
                    id: *id,
                    rect: RECT {
                        left: pad,
                        top: y,
                        right: width_px - pad,
                        bottom: y + h,
                    },
                });
                y += h + s(4.0);
            }
            FlyoutBlock::ButtonBar(buttons) => {
                let h = s(32.0);
                let count = buttons.len().max(1) as i32;
                let gap = s(6.0);
                let avail = width_px - pad * 2 - gap * (count - 1);
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
    let height_px = (y + pad).min(s(520.0));

    let (scr_w, scr_h) = win32::primary_screen_px();
    let margin = s(10.0);
    let x =
        (anchor_center_x_px - width_px / 2).clamp(margin, (scr_w - width_px - margin).max(margin));
    let top_y =
        (anchor_bottom_y_px - height_px).clamp(margin, (scr_h - height_px - margin).max(margin));

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name.as_ptr(),
            wide("").as_ptr(),
            WS_POPUP,
            x,
            top_y,
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
        targets,
        hovered_id: None,
        tracking: false,
        selected_id: None,
        done: AtomicBool::new(false),
    });
    let ptr = (&mut *state) as *mut FlyoutState as isize;
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            x,
            top_y,
            width_px,
            height_px,
            SWP_SHOWWINDOW,
        );
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
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

    let selected = state.selected_id;
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        DestroyWindow(hwnd);
    }
    selected
}

impl FlyoutState {
    fn paint(&self) {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if hdc.is_null() {
            return;
        }
        let dpi = self.dpi;
        let s = |v: f32| (v * dpi).round() as i32;
        let pad = s(12.0);
        let radius = s(8.0);
        let w = ps.rcPaint.right;

        let (bg, card_bg, hover_bg, accent, text_fg, muted_fg, danger_bg, danger_fg, sep_col) =
            if self.is_dark {
                (
                    rgb(0x14, 0x17, 0x20),
                    rgb(0x1d, 0x22, 0x30),
                    rgb(0x28, 0x30, 0x42),
                    rgb(0x0e, 0xa5, 0xe9),
                    rgb(0xf1, 0xf5, 0xf9),
                    rgb(0x94, 0xa3, 0xb8),
                    rgb(0x7f, 0x1d, 0x1d),
                    rgb(0xfc, 0xa5, 0xa5),
                    rgb(0x27, 0x2d, 0x3d),
                )
            } else {
                (
                    rgb(0xf8, 0xfa, 0xfc),
                    rgb(0xee, 0xf2, 0xf7),
                    rgb(0xe2, 0xe8, 0xf0),
                    rgb(0x02, 0x84, 0xc7),
                    rgb(0x0f, 0x17, 0x2a),
                    rgb(0x47, 0x55, 0x69),
                    rgb(0xfe, 0xe2, 0xe2),
                    rgb(0xb9, 0x1c, 0x1c),
                    rgb(0xe2, 0xe8, 0xf0),
                )
            };

        unsafe {
            let bg_brush = CreateSolidBrush(bg);
            FillRect(hdc, &ps.rcPaint, bg_brush);
            DeleteObject(bg_brush as _);

            let fname = wide("Segoe UI");
            let title_font = CreateFontW(
                -s(15.0),
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

            // Header
            let old_f = SelectObject(hdc, title_font as _);
            SetTextColor(hdc, text_fg);
            let mut hrc = RECT {
                left: pad,
                top: pad,
                right: w - pad,
                bottom: pad + s(24.0),
            };
            let mut tw: Vec<u16> = self.spec.title.encode_utf16().collect();
            DrawTextW(
                hdc,
                tw.as_mut_ptr(),
                tw.len() as i32,
                &mut hrc,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );

            let mut y = pad + s(26.0);
            if let Some(ref sub) = self.spec.subtitle {
                SelectObject(hdc, small_font as _);
                SetTextColor(hdc, muted_fg);
                let mut src = RECT {
                    left: pad,
                    top: y,
                    right: w - pad,
                    bottom: y + s(18.0),
                };
                let mut sw: Vec<u16> = sub.encode_utf16().collect();
                DrawTextW(
                    hdc,
                    sw.as_mut_ptr(),
                    sw.len() as i32,
                    &mut src,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                );
                y += s(20.0);
            }
            y += s(6.0);

            for block in &self.spec.blocks {
                match block {
                    FlyoutBlock::StatRow { label, value } => {
                        let h = s(26.0);
                        SelectObject(hdc, body_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut rc = RECT {
                            left: pad,
                            top: y,
                            right: w - pad,
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
                            right: w - pad,
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
                        RoundRect(hdc, pad, bar_top, w - pad, bar_top + bar_h, s(4.0), s(4.0));
                        SelectObject(hdc, ob);
                        SelectObject(hdc, op);
                        DeleteObject(tb as _);
                        DeleteObject(tp as _);

                        let fill_w =
                            (((w - pad * 2) as f32) * ratio.clamp(0.0, 1.0)).round() as i32;
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
                        destructive,
                    } => {
                        let h = if subtitle.is_some() { s(42.0) } else { s(32.0) };
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
                        RoundRect(hdc, pad, y, w - pad, y + h, radius, radius);
                        SelectObject(hdc, ob);
                        SelectObject(hdc, op);
                        DeleteObject(fb as _);
                        DeleteObject(fp as _);

                        SelectObject(hdc, body_font as _);
                        SetTextColor(hdc, if *destructive { danger_fg } else { text_fg });
                        let mut trc = RECT {
                            left: pad + s(10.0),
                            top: y + if subtitle.is_some() { s(4.0) } else { 0 },
                            right: w - pad - s(10.0),
                            bottom: y + if subtitle.is_some() { s(22.0) } else { h },
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
                                left: pad + s(10.0),
                                top: y + s(21.0),
                                right: w - pad - s(10.0),
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
                                left: pad + s(10.0),
                                top: y,
                                right: w - pad - s(10.0),
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
                        let h = s(32.0);
                        let count = buttons.len().max(1) as i32;
                        let gap = s(6.0);
                        let avail = w - pad * 2 - gap * (count - 1);
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
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                            );
                        }
                        y += h + s(6.0);
                    }
                    FlyoutBlock::Separator => {
                        let sy = y + s(5.0);
                        let sp = CreatePen(PS_SOLID, 1, sep_col);
                        let op = SelectObject(hdc, sp as _);
                        MoveToEx(hdc, pad, sy, std::ptr::null_mut());
                        LineTo(hdc, w - pad, sy);
                        SelectObject(hdc, op);
                        DeleteObject(sp as _);
                        y += s(10.0);
                    }
                    FlyoutBlock::Note(text) => {
                        let h = s(26.0);
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut rc = RECT {
                            left: pad,
                            top: y,
                            right: w - pad,
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
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FlyoutState };
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
            let cur = if state.hovered_id.is_some() {
                IDC_HAND
            } else {
                IDC_ARROW
            };
            unsafe {
                SetCursor(LoadCursorW(std::ptr::null_mut(), cur));
            }
            1
        }
        WM_MOUSEMOVE => {
            if !state.tracking {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    TrackMouseEvent(&mut tme);
                }
                state.tracking = true;
            }
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            let next = state
                .targets
                .iter()
                .find(|t| pt_in_rect(&t.rect, x, y))
                .map(|t| t.id);
            if next != state.hovered_id {
                state.hovered_id = next;
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
        WM_LBUTTONUP => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            if let Some(target) = state.targets.iter().find(|t| pt_in_rect(&t.rect, x, y)) {
                state.selected_id = Some(target.id);
                state.done.store(true, Ordering::Release);
            }
            0
        }
        WM_KEYDOWN if wparam as u16 == VK_ESCAPE => {
            state.done.store(true, Ordering::Release);
            0
        }
        WM_ACTIVATE if (wparam & 0xffff) == 0 => {
            state.done.store(true, Ordering::Release);
            0
        }
        WM_KILLFOCUS => {
            state.done.store(true, Ordering::Release);
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
