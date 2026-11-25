//! Custom modern iOS/shadcn-inspired Win32 popup context menu with crisp vector icons.
//!
//! Replaces Win32's legacy `TrackPopupMenu` with a crisp Win11-rounded dark/light surface,
//! left vector icons, rounded hover highlights, right-aligned shortcut hints, thin separators,
//! and destructive red hover tints. Opens in < 2ms with zero GPU overhead.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DT_LEFT, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, HDC, InvalidateRect, LineTo,
    MoveToEx, PAINTSTRUCT, PS_SOLID, Rectangle, RoundRect, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos,
    GetMessageW, GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IDC_HAND, LoadCursorW, MSG,
    RegisterClassExW, SW_SHOW, SWP_SHOWWINDOW, SetCursor, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, WM_ACTIVATE, WM_KEYDOWN, WM_KILLFOCUS,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::design::{self, gdi_rgb};
use crate::platform::win32;

const WM_MOUSELEAVE: u32 = 0x02A3;
const DWMWCP_ROUND: u32 = 2;
const MENU_W_DIPS: f32 = 248.0;
const ROW_H_DIPS: f32 = 30.0;
const SEP_H_DIPS: f32 = 8.0;
const PAD_X_DIPS: f32 = 6.0;
const PAD_Y_DIPS: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuIcon {
    None,
    Settings,
    Activity,
    Folder,
    Terminal,
    Shield,
    Play,
    Monitor,
    Refresh,
    Power,
    Plus,
    Pin,
    Close,
    Trash,
    ChevronLeft,
    ChevronRight,
    Layers,
}

#[derive(Clone)]
pub enum MenuEntry {
    Item {
        id: u32,
        icon: MenuIcon,
        label: String,
        shortcut: Option<&'static str>,
        destructive: bool,
    },
    Separator,
}

impl MenuEntry {
    pub fn item(id: u32, icon: MenuIcon, label: impl Into<String>) -> Self {
        Self::Item {
            id,
            icon,
            label: label.into(),
            shortcut: None,
            destructive: false,
        }
    }

    pub fn with_shortcut(
        id: u32,
        icon: MenuIcon,
        label: impl Into<String>,
        shortcut: &'static str,
    ) -> Self {
        Self::Item {
            id,
            icon,
            label: label.into(),
            shortcut: Some(shortcut),
            destructive: false,
        }
    }

    pub fn destructive(id: u32, icon: MenuIcon, label: impl Into<String>) -> Self {
        Self::Item {
            id,
            icon,
            label: label.into(),
            shortcut: None,
            destructive: true,
        }
    }
}

struct MenuState {
    hwnd: HWND,
    dpi: f32,
    is_dark: bool,
    entries: Vec<MenuEntry>,
    row_rects: Vec<Option<RECT>>,
    hovered: Option<usize>,
    tracking: bool,
    selected_id: Option<u32>,
    done: AtomicBool,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn pt_in_rect(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub(super) unsafe fn draw_menu_icon(
    hdc: HDC,
    icon: MenuIcon,
    cx: i32,
    cy: i32,
    dpi: f32,
    color: u32,
) {
    if icon == MenuIcon::None {
        return;
    }
    let s = |v: f32| (v * dpi).round() as i32;
    let stroke_w = (1.4 * dpi).round().max(1.0) as i32;
    unsafe {
        let pen = CreatePen(PS_SOLID, stroke_w, color);
        let null_brush = windows_sys::Win32::Graphics::Gdi::GetStockObject(
            windows_sys::Win32::Graphics::Gdi::NULL_BRUSH,
        );
        let old_pen = SelectObject(hdc, pen as _);
        let old_brush = SelectObject(hdc, null_brush);

        match icon {
            MenuIcon::None => {}
            MenuIcon::Settings => {
                Ellipse(hdc, cx - s(5.0), cy - s(5.0), cx + s(5.0), cy + s(5.0));
                Ellipse(hdc, cx - s(2.0), cy - s(2.0), cx + s(2.0), cy + s(2.0));
            }
            MenuIcon::Activity => {
                MoveToEx(hdc, cx - s(6.0), cy, std::ptr::null_mut());
                LineTo(hdc, cx - s(3.0), cy);
                LineTo(hdc, cx - s(1.0), cy - s(4.5));
                LineTo(hdc, cx + s(1.5), cy + s(4.0));
                LineTo(hdc, cx + s(3.5), cy);
                LineTo(hdc, cx + s(6.0), cy);
            }
            MenuIcon::Folder => {
                RoundRect(
                    hdc,
                    cx - s(6.0),
                    cy - s(4.0),
                    cx + s(6.0),
                    cy + s(5.0),
                    s(2.0),
                    s(2.0),
                );
                MoveToEx(hdc, cx - s(5.5), cy - s(4.0), std::ptr::null_mut());
                LineTo(hdc, cx - s(4.0), cy - s(5.5));
                LineTo(hdc, cx - s(0.5), cy - s(5.5));
                LineTo(hdc, cx + s(1.0), cy - s(4.0));
            }
            MenuIcon::Terminal => {
                MoveToEx(hdc, cx - s(5.5), cy - s(3.5), std::ptr::null_mut());
                LineTo(hdc, cx - s(1.5), cy);
                LineTo(hdc, cx - s(5.5), cy + s(3.5));
                MoveToEx(hdc, cx + s(0.5), cy + s(3.5), std::ptr::null_mut());
                LineTo(hdc, cx + s(5.5), cy + s(3.5));
            }
            MenuIcon::Shield => {
                MoveToEx(hdc, cx, cy - s(6.0), std::ptr::null_mut());
                LineTo(hdc, cx + s(5.0), cy - s(3.5));
                LineTo(hdc, cx + s(4.5), cy + s(1.5));
                LineTo(hdc, cx, cy + s(6.0));
                LineTo(hdc, cx - s(4.5), cy + s(1.5));
                LineTo(hdc, cx - s(5.0), cy - s(3.5));
                LineTo(hdc, cx, cy - s(6.0));
            }
            MenuIcon::Play => {
                MoveToEx(hdc, cx - s(3.5), cy - s(5.0), std::ptr::null_mut());
                LineTo(hdc, cx + s(5.0), cy);
                LineTo(hdc, cx - s(3.5), cy + s(5.0));
                LineTo(hdc, cx - s(3.5), cy - s(5.0));
            }
            MenuIcon::Monitor => {
                RoundRect(
                    hdc,
                    cx - s(6.0),
                    cy - s(5.0),
                    cx + s(6.0),
                    cy + s(2.5),
                    s(2.0),
                    s(2.0),
                );
                MoveToEx(hdc, cx - s(2.5), cy + s(5.0), std::ptr::null_mut());
                LineTo(hdc, cx + s(2.5), cy + s(5.0));
                MoveToEx(hdc, cx, cy + s(2.5), std::ptr::null_mut());
                LineTo(hdc, cx, cy + s(5.0));
            }
            MenuIcon::Refresh => {
                Ellipse(hdc, cx - s(5.0), cy - s(5.0), cx + s(5.0), cy + s(5.0));
                MoveToEx(hdc, cx + s(2.0), cy - s(5.5), std::ptr::null_mut());
                LineTo(hdc, cx + s(5.5), cy - s(3.0));
            }
            MenuIcon::Power => {
                Ellipse(hdc, cx - s(5.0), cy - s(4.0), cx + s(5.0), cy + s(5.5));
                MoveToEx(hdc, cx, cy - s(6.0), std::ptr::null_mut());
                LineTo(hdc, cx, cy);
            }
            MenuIcon::Plus => {
                MoveToEx(hdc, cx - s(5.0), cy, std::ptr::null_mut());
                LineTo(hdc, cx + s(5.0), cy);
                MoveToEx(hdc, cx, cy - s(5.0), std::ptr::null_mut());
                LineTo(hdc, cx, cy + s(5.0));
            }
            MenuIcon::Pin => {
                MoveToEx(hdc, cx - s(3.5), cy - s(5.0), std::ptr::null_mut());
                LineTo(hdc, cx + s(3.5), cy - s(5.0));
                MoveToEx(hdc, cx - s(2.5), cy - s(5.0), std::ptr::null_mut());
                LineTo(hdc, cx - s(4.0), cy + s(1.0));
                LineTo(hdc, cx + s(4.0), cy + s(1.0));
                LineTo(hdc, cx + s(2.5), cy - s(5.0));
                MoveToEx(hdc, cx, cy + s(1.0), std::ptr::null_mut());
                LineTo(hdc, cx, cy + s(6.0));
            }
            MenuIcon::Close => {
                MoveToEx(hdc, cx - s(4.5), cy - s(4.5), std::ptr::null_mut());
                LineTo(hdc, cx + s(4.5), cy + s(4.5));
                MoveToEx(hdc, cx + s(4.5), cy - s(4.5), std::ptr::null_mut());
                LineTo(hdc, cx - s(4.5), cy + s(4.5));
            }
            MenuIcon::Trash => {
                MoveToEx(hdc, cx - s(5.5), cy - s(4.0), std::ptr::null_mut());
                LineTo(hdc, cx + s(5.5), cy - s(4.0));
                MoveToEx(hdc, cx - s(2.0), cy - s(6.0), std::ptr::null_mut());
                LineTo(hdc, cx + s(2.0), cy - s(6.0));
                Rectangle(hdc, cx - s(4.0), cy - s(4.0), cx + s(4.0), cy + s(5.5));
            }
            MenuIcon::ChevronLeft => {
                MoveToEx(hdc, cx + s(2.5), cy - s(4.5), std::ptr::null_mut());
                LineTo(hdc, cx - s(2.5), cy);
                LineTo(hdc, cx + s(2.5), cy + s(4.5));
            }
            MenuIcon::ChevronRight => {
                MoveToEx(hdc, cx - s(2.5), cy - s(4.5), std::ptr::null_mut());
                LineTo(hdc, cx + s(2.5), cy);
                LineTo(hdc, cx - s(2.5), cy + s(4.5));
            }
            MenuIcon::Layers => {
                Rectangle(hdc, cx - s(5.5), cy - s(5.0), cx + s(2.0), cy + s(1.5));
                MoveToEx(hdc, cx - s(2.0), cy + s(4.5), std::ptr::null_mut());
                LineTo(hdc, cx + s(5.5), cy + s(4.5));
                LineTo(hdc, cx + s(5.5), cy - s(1.5));
            }
        }

        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        DeleteObject(pen as _);
    }
}

/// Displays a modern rounded popup menu at the current cursor position and returns the chosen item ID.
pub fn show_popup_menu(owner_hwnd: usize, is_dark: bool, entries: Vec<MenuEntry>) -> Option<u32> {
    if entries.is_empty() {
        return None;
    }
    let class_name = wide("OpenTaskBarContextMenu");
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(menu_wnd_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        lpszClassName: class_name.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        RegisterClassExW(&wc);
    }

    let dpi = win32::dpi_scale(owner_hwnd);
    let pad_x = (PAD_X_DIPS * dpi).round() as i32;
    let pad_y = (PAD_Y_DIPS * dpi).round() as i32;
    let row_h = (ROW_H_DIPS * dpi).round() as i32;
    let sep_h = (SEP_H_DIPS * dpi).round() as i32;
    let menu_w = (MENU_W_DIPS * dpi).round() as i32;

    let mut y_cursor = pad_y;
    let mut row_rects = Vec::with_capacity(entries.len());
    for entry in &entries {
        match entry {
            MenuEntry::Item { .. } => {
                row_rects.push(Some(RECT {
                    left: pad_x,
                    top: y_cursor,
                    right: menu_w - pad_x,
                    bottom: y_cursor + row_h,
                }));
                y_cursor += row_h;
            }
            MenuEntry::Separator => {
                row_rects.push(None);
                y_cursor += sep_h;
            }
        }
    }
    let menu_h = y_cursor + pad_y;

    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut pt);
    }
    let (scr_w, scr_h) = win32::primary_screen_px();
    let margin = (8.0 * dpi).round() as i32;
    let x = pt.x.clamp(margin, (scr_w - menu_w - margin).max(margin));
    let y = (pt.y - menu_h).clamp(margin, (scr_h - menu_h - margin).max(margin));

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name.as_ptr(),
            wide("").as_ptr(),
            WS_POPUP,
            x,
            y,
            menu_w,
            menu_h,
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
        let pal = design::palette(is_dark);
        let border = gdi_rgb(pal.border_rgb);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            (&border as *const u32).cast(),
            size_of::<u32>() as u32,
        );
    }

    let mut state = Box::new(MenuState {
        hwnd,
        dpi,
        is_dark,
        entries,
        row_rects,
        hovered: None,
        tracking: false,
        selected_id: None,
        done: AtomicBool::new(false),
    });
    let ptr = (&mut *state) as *mut MenuState as isize;
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, menu_w, menu_h, SWP_SHOWWINDOW);
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

impl MenuState {
    fn paint(&self) {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if hdc.is_null() {
            return;
        }
        let dpi = self.dpi;
        let pad_x = (PAD_X_DIPS * dpi).round() as i32;
        let pad_y = (PAD_Y_DIPS * dpi).round() as i32;
        let row_h = (ROW_H_DIPS * dpi).round() as i32;
        let sep_h = (SEP_H_DIPS * dpi).round() as i32;
        let radius = (7.0 * dpi).round() as i32;
        let icon_cx_off = (16.0 * dpi).round() as i32;
        let text_left_off = (32.0 * dpi).round() as i32;
        let text_right_off = (10.0 * dpi).round() as i32;

        let pal = design::palette(self.is_dark);
        let (bg, hover_bg, danger_bg, text_fg, icon_fg, hint_fg, danger_fg, sep_col) = (
            gdi_rgb(pal.surface),
            gdi_rgb(pal.hover),
            gdi_rgb(pal.danger_hover_bg),
            gdi_rgb(pal.text_primary),
            gdi_rgb(pal.text_secondary),
            gdi_rgb(pal.text_secondary),
            gdi_rgb(pal.danger_fg),
            gdi_rgb(pal.sep_rgb),
        );

        unsafe {
            let bg_brush = CreateSolidBrush(bg);
            FillRect(hdc, &ps.rcPaint, bg_brush);
            DeleteObject(bg_brush as _);

            let font_name = wide("Segoe UI");
            let main_font = CreateFontW(
                -((13.0 * dpi).round() as i32),
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
                font_name.as_ptr(),
            );
            let hint_font = CreateFontW(
                -((11.0 * dpi).round() as i32),
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
                font_name.as_ptr(),
            );
            SetBkMode(hdc, TRANSPARENT as i32);

            let mut y_cursor = pad_y;
            for (idx, entry) in self.entries.iter().enumerate() {
                match entry {
                    MenuEntry::Item {
                        icon,
                        label,
                        shortcut,
                        destructive,
                        ..
                    } => {
                        let rc = self.row_rects[idx].unwrap_or(RECT {
                            left: pad_x,
                            top: y_cursor,
                            right: ps.rcPaint.right - pad_x,
                            bottom: y_cursor + row_h,
                        });
                        let is_hovered = self.hovered == Some(idx);
                        if is_hovered {
                            let hcol = if *destructive { danger_bg } else { hover_bg };
                            let hb = CreateSolidBrush(hcol);
                            let hp = CreatePen(PS_SOLID, 1, hcol);
                            let ob = SelectObject(hdc, hb as _);
                            let op = SelectObject(hdc, hp as _);
                            RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, radius, radius);
                            SelectObject(hdc, ob);
                            SelectObject(hdc, op);
                            DeleteObject(hb as _);
                            DeleteObject(hp as _);
                        }

                        let ic_col = if *destructive {
                            danger_fg
                        } else if is_hovered {
                            text_fg
                        } else {
                            icon_fg
                        };
                        draw_menu_icon(
                            hdc,
                            *icon,
                            rc.left + icon_cx_off,
                            (rc.top + rc.bottom) / 2,
                            dpi,
                            ic_col,
                        );

                        let old_font = SelectObject(hdc, main_font as _);
                        SetTextColor(hdc, if *destructive { danger_fg } else { text_fg });
                        let mut text_rc = RECT {
                            left: rc.left + text_left_off,
                            top: rc.top,
                            right: rc.right - text_right_off,
                            bottom: rc.bottom,
                        };
                        let mut label_w: Vec<u16> = label.encode_utf16().collect();
                        DrawTextW(
                            hdc,
                            label_w.as_mut_ptr(),
                            label_w.len() as i32,
                            &mut text_rc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                        );
                        SelectObject(hdc, old_font);

                        if let Some(sc) = shortcut {
                            let of2 = SelectObject(hdc, hint_font as _);
                            SetTextColor(hdc, hint_fg);
                            let mut sc_w: Vec<u16> = sc.encode_utf16().collect();
                            DrawTextW(
                                hdc,
                                sc_w.as_mut_ptr(),
                                sc_w.len() as i32,
                                &mut text_rc,
                                DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                            );
                            SelectObject(hdc, of2);
                        }
                        y_cursor += row_h;
                    }
                    MenuEntry::Separator => {
                        let sep_y = y_cursor + sep_h / 2;
                        let sp = CreatePen(PS_SOLID, 1, sep_col);
                        let op = SelectObject(hdc, sp as _);
                        MoveToEx(hdc, pad_x + text_right_off, sep_y, std::ptr::null_mut());
                        LineTo(hdc, ps.rcPaint.right - pad_x - text_right_off, sep_y);
                        SelectObject(hdc, op);
                        DeleteObject(sp as _);
                        y_cursor += sep_h;
                    }
                }
            }

            DeleteObject(main_font as _);
            DeleteObject(hint_font as _);
            EndPaint(self.hwnd, &ps);
        }
    }
}

unsafe extern "system" fn menu_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MenuState };
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
            let cur = if state.hovered.is_some() {
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
                .row_rects
                .iter()
                .position(|r| r.as_ref().is_some_and(|rc| pt_in_rect(rc, x, y)));
            if next != state.hovered {
                state.hovered = next;
                unsafe {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            state.tracking = false;
            if state.hovered.take().is_some() {
                unsafe {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }
        WM_LBUTTONUP => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            if let Some(idx) = state
                .row_rects
                .iter()
                .position(|r| r.as_ref().is_some_and(|rc| pt_in_rect(rc, x, y)))
                && let MenuEntry::Item { id, .. } = state.entries[idx]
            {
                state.selected_id = Some(id);
            }
            state.done.store(true, Ordering::Release);
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
