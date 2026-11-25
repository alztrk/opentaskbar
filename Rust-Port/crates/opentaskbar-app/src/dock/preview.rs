//! Native Win32 + DWM live window thumbnail preview popup (`DwmRegisterThumbnail`).
//!
//! Renders up to 6 live hardware-composited DWM window thumbnails side by side above the hovered
//! dock icon with crisp window titles, hover states, click-to-activate, middle-click close,
//! and an `X` close button per card. Uses 0% 3D GPU overhead because DWM composites thumbnails
//! directly into the desktop pass.

use opentaskbar_shell::tracker;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_VISIBLE,
    DWMWA_BORDER_COLOR, DWMWA_EXCLUDED_FROM_PEEK, DWMWA_WINDOW_CORNER_PREFERENCE,
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmSetWindowAttribute,
    DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect, LineTo, MoveToEx,
    PAINTSTRUCT, PS_SOLID, RoundRect, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, HWND_TOPMOST,
    IDC_ARROW, IDC_HAND, LoadCursorW, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetCursor, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_LBUTTONUP,
    WM_MBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::platform::win32;

const WM_MOUSELEAVE: u32 = 0x02A3;
const DWMWCP_ROUND: u32 = 2;
const MAX_CARDS: usize = 6;
const CARD_W_DIPS: f32 = 236.0;
const CARD_H_DIPS: f32 = 164.0;
const HEADER_H_DIPS: f32 = 32.0;
const CARD_GAP_DIPS: f32 = 8.0;
const OUTER_PAD_DIPS: f32 = 8.0;
const CLOSE_BTN_DIPS: f32 = 22.0;

#[derive(Clone, Debug)]
pub struct PreviewItem {
    pub hwnd: usize,
    pub title: String,
}

struct RegisteredCard {
    item: PreviewItem,
    thumb: isize,
    card_rect: RECT,
    close_rect: RECT,
}

pub struct PreviewPopup {
    hwnd: HWND,
    dpi: f32,
    visible: bool,
    pointer_inside: bool,
    tracking_mouse: bool,
    owner_key: Option<String>,
    cards: Vec<RegisteredCard>,
    hovered_card: Option<usize>,
    hovered_close: Option<usize>,
    on_pointer_leave: Box<dyn FnMut()>,
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

impl PreviewPopup {
    pub fn new(on_pointer_leave: impl FnMut() + 'static) -> Option<Box<Self>> {
        let class_name = wide("OpenTaskBarPreview");
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(preview_wnd_proc),
            hInstance: hinstance,
            hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
            lpszClassName: class_name.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        unsafe {
            RegisterClassExW(&wc);
        }

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                wide("").as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
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
            let border = rgb(0x36, 0x3b, 0x48);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR as u32,
                (&border as *const u32).cast(),
                size_of::<u32>() as u32,
            );
            let exclude_peek: u32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_EXCLUDED_FROM_PEEK as u32,
                (&exclude_peek as *const u32).cast(),
                size_of::<u32>() as u32,
            );
        }

        let dpi = win32::dpi_scale(hwnd as usize);
        let mut popup = Box::new(Self {
            hwnd,
            dpi,
            visible: false,
            pointer_inside: false,
            tracking_mouse: false,
            owner_key: None,
            cards: Vec::new(),
            hovered_card: None,
            hovered_close: None,
            on_pointer_leave: Box::new(on_pointer_leave),
        });
        let ptr = (&mut *popup) as *mut Self as isize;
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);
        }
        Some(popup)
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_pointer_inside(&self) -> bool {
        self.pointer_inside
    }

    pub fn owner_key(&self) -> Option<&str> {
        self.owner_key.as_deref()
    }

    pub fn window_rect(&self) -> Option<RECT> {
        if !self.visible {
            return None;
        }
        let mut rc: RECT = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(self.hwnd, &mut rc)
        };
        (ok != 0).then_some(rc)
    }

    fn clear_thumbnails(&mut self) {
        for card in self.cards.drain(..) {
            if card.thumb != 0 {
                unsafe {
                    let _ = DwmUnregisterThumbnail(card.thumb);
                }
            }
        }
    }

    pub fn hide(&mut self) {
        if !self.visible && self.cards.is_empty() {
            return;
        }
        self.clear_thumbnails();
        self.visible = false;
        self.pointer_inside = false;
        self.tracking_mouse = false;
        self.owner_key = None;
        self.hovered_card = None;
        self.hovered_close = None;
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Shows live DWM thumbnails for `items` centered horizontally at `anchor_center_x_px`
    /// with the popup bottom edge positioned at `anchor_bottom_y_px`.
    pub fn show(
        &mut self,
        owner_key: &str,
        items: &[PreviewItem],
        anchor_center_x_px: i32,
        anchor_bottom_y_px: i32,
    ) {
        let count = items.len().min(MAX_CARDS);
        if count == 0 {
            self.hide();
            return;
        }

        self.clear_thumbnails();
        self.dpi = win32::dpi_scale(self.hwnd as usize);
        let dpi = self.dpi;

        let card_w = (CARD_W_DIPS * dpi).round() as i32;
        let card_h = (CARD_H_DIPS * dpi).round() as i32;
        let header_h = (HEADER_H_DIPS * dpi).round() as i32;
        let gap = (CARD_GAP_DIPS * dpi).round() as i32;
        let pad = (OUTER_PAD_DIPS * dpi).round() as i32;
        let close_sz = (CLOSE_BTN_DIPS * dpi).round() as i32;
        let inner_pad = (6.0 * dpi).round() as i32;

        let total_w = pad * 2 + card_w * (count as i32) + gap * ((count as i32) - 1);
        let total_h = pad * 2 + card_h;

        let (scr_w, scr_h) = win32::primary_screen_px();
        let margin = (8.0 * dpi).round() as i32;
        let x = (anchor_center_x_px - total_w / 2)
            .clamp(margin, (scr_w - total_w - margin).max(margin));
        let y =
            (anchor_bottom_y_px - total_h).clamp(margin, (scr_h - total_h - margin).max(margin));

        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x,
                y,
                total_w,
                total_h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }

        for (i, item) in items.iter().take(count).enumerate() {
            let left = pad + (i as i32) * (card_w + gap);
            let top = pad;
            let card_rect = RECT {
                left,
                top,
                right: left + card_w,
                bottom: top + card_h,
            };
            let close_rect = RECT {
                left: card_rect.right - close_sz - inner_pad,
                top: card_rect.top + (header_h - close_sz) / 2,
                right: card_rect.right - inner_pad,
                bottom: card_rect.top + (header_h - close_sz) / 2 + close_sz,
            };

            let thumb_avail_w = (card_w - inner_pad * 2).max(16);
            let thumb_avail_h = (card_h - header_h - inner_pad).max(16);
            let thumb_left = card_rect.left + inner_pad;
            let thumb_top = card_rect.top + header_h;

            let mut thumb_handle: isize = 0;
            let reg_hr =
                unsafe { DwmRegisterThumbnail(self.hwnd, item.hwnd as HWND, &mut thumb_handle) };
            if reg_hr == 0 && thumb_handle != 0 {
                let mut src_size = SIZE { cx: 0, cy: 0 };
                let _ = unsafe { DwmQueryThumbnailSourceSize(thumb_handle, &mut src_size) };
                let (dest_w, dest_h) = if src_size.cx > 0 && src_size.cy > 0 {
                    let sx = thumb_avail_w as f32 / src_size.cx as f32;
                    let sy = thumb_avail_h as f32 / src_size.cy as f32;
                    let scale = sx.min(sy).min(1.0);
                    (
                        ((src_size.cx as f32 * scale).round() as i32).max(16),
                        ((src_size.cy as f32 * scale).round() as i32).max(16),
                    )
                } else {
                    (thumb_avail_w, thumb_avail_h)
                };
                let dx = thumb_left + (thumb_avail_w - dest_w) / 2;
                let dy = thumb_top + (thumb_avail_h - dest_h) / 2;
                let props = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY,
                    rcDestination: RECT {
                        left: dx,
                        top: dy,
                        right: dx + dest_w,
                        bottom: dy + dest_h,
                    },
                    rcSource: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                    opacity: 255,
                    fVisible: 1,
                    fSourceClientAreaOnly: 0,
                };
                unsafe {
                    let _ = DwmUpdateThumbnailProperties(thumb_handle, &props);
                }
            } else {
                thumb_handle = 0;
            }

            self.cards.push(RegisteredCard {
                item: item.clone(),
                thumb: thumb_handle,
                card_rect,
                close_rect,
            });
        }

        self.visible = true;
        self.owner_key = Some(owner_key.to_owned());
        self.hovered_card = None;
        self.hovered_close = None;
        unsafe {
            InvalidateRect(self.hwnd, std::ptr::null(), 1);
        }
    }

    fn paint(&self) {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if hdc.is_null() {
            return;
        }
        let dpi = self.dpi;
        let header_h = (HEADER_H_DIPS * dpi).round() as i32;
        let inner_pad = (8.0 * dpi).round() as i32;
        let corner = (8.0 * dpi).round() as i32;

        unsafe {
            // Fill window background.
            let bg_brush = CreateSolidBrush(rgb(0x14, 0x17, 0x1f));
            FillRect(hdc, &ps.rcPaint, bg_brush);
            DeleteObject(bg_brush as _);

            let font_h = -((12.0 * dpi).round() as i32);
            let font_name = wide("Segoe UI");
            let font = CreateFontW(
                font_h,
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
                5, // CLEARTYPE_QUALITY
                0,
                font_name.as_ptr(),
            );
            let old_font = SelectObject(hdc, font as _);
            SetBkMode(hdc, TRANSPARENT as i32);

            for (idx, card) in self.cards.iter().enumerate() {
                let is_hovered = self.hovered_card == Some(idx);
                let is_close_hovered = self.hovered_close == Some(idx);

                let card_bg = if is_hovered {
                    CreateSolidBrush(rgb(0x22, 0x27, 0x34))
                } else {
                    CreateSolidBrush(rgb(0x1b, 0x1f, 0x2a))
                };
                let card_pen = if is_hovered {
                    CreatePen(PS_SOLID, 1, rgb(0x4b, 0x55, 0x6b))
                } else {
                    CreatePen(PS_SOLID, 1, rgb(0x2b, 0x30, 0x3e))
                };
                let old_brush = SelectObject(hdc, card_bg as _);
                let old_pen = SelectObject(hdc, card_pen as _);
                RoundRect(
                    hdc,
                    card.card_rect.left,
                    card.card_rect.top,
                    card.card_rect.right,
                    card.card_rect.bottom,
                    corner,
                    corner,
                );
                SelectObject(hdc, old_brush);
                SelectObject(hdc, old_pen);
                DeleteObject(card_bg as _);
                DeleteObject(card_pen as _);

                // Window title.
                SetTextColor(hdc, rgb(0xf1, 0xf5, 0xf9));
                let mut title_rc = RECT {
                    left: card.card_rect.left + inner_pad,
                    top: card.card_rect.top,
                    right: card.close_rect.left - (4.0 * dpi).round() as i32,
                    bottom: card.card_rect.top + header_h,
                };
                if !card.item.title.is_empty() {
                    let mut title_w: Vec<u16> =
                        card.item.title.encode_utf16().chain(Some(0)).collect();
                    DrawTextW(
                        hdc,
                        title_w.as_mut_ptr(),
                        (title_w.len() - 1) as i32,
                        &mut title_rc,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                    );
                }

                // Close button (`X`).
                if is_hovered || is_close_hovered {
                    let btn_bg = if is_close_hovered {
                        CreateSolidBrush(rgb(0xdc, 0x26, 0x26))
                    } else {
                        CreateSolidBrush(rgb(0x2e, 0x34, 0x44))
                    };
                    let btn_pen = CreatePen(
                        PS_SOLID,
                        1,
                        if is_close_hovered {
                            rgb(0xdc, 0x26, 0x26)
                        } else {
                            rgb(0x2e, 0x34, 0x44)
                        },
                    );
                    let ob = SelectObject(hdc, btn_bg as _);
                    let op = SelectObject(hdc, btn_pen as _);
                    let r = (5.0 * dpi).round() as i32;
                    RoundRect(
                        hdc,
                        card.close_rect.left,
                        card.close_rect.top,
                        card.close_rect.right,
                        card.close_rect.bottom,
                        r,
                        r,
                    );
                    SelectObject(hdc, ob);
                    SelectObject(hdc, op);
                    DeleteObject(btn_bg as _);
                    DeleteObject(btn_pen as _);

                    let x_pen = CreatePen(
                        PS_SOLID,
                        (1.5 * dpi).round().max(1.0) as i32,
                        rgb(0xf8, 0xfa, 0xfc),
                    );
                    let ox = SelectObject(hdc, x_pen as _);
                    let pad_x = (6.0 * dpi).round() as i32;
                    MoveToEx(
                        hdc,
                        card.close_rect.left + pad_x,
                        card.close_rect.top + pad_x,
                        std::ptr::null_mut(),
                    );
                    LineTo(
                        hdc,
                        card.close_rect.right - pad_x,
                        card.close_rect.bottom - pad_x,
                    );
                    MoveToEx(
                        hdc,
                        card.close_rect.right - pad_x,
                        card.close_rect.top + pad_x,
                        std::ptr::null_mut(),
                    );
                    LineTo(
                        hdc,
                        card.close_rect.left + pad_x,
                        card.close_rect.bottom - pad_x,
                    );
                    SelectObject(hdc, ox);
                    DeleteObject(x_pen as _);
                }
            }

            SelectObject(hdc, old_font);
            DeleteObject(font as _);
            EndPaint(self.hwnd, &ps);
        }
    }

    fn handle_mouse_move(&mut self, x: i32, y: i32) {
        self.pointer_inside = true;
        if !self.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                TrackMouseEvent(&mut tme);
            }
            self.tracking_mouse = true;
        }
        let next_card = self
            .cards
            .iter()
            .position(|c| pt_in_rect(&c.card_rect, x, y));
        let next_close = next_card.filter(|&i| pt_in_rect(&self.cards[i].close_rect, x, y));
        if next_card != self.hovered_card || next_close != self.hovered_close {
            self.hovered_card = next_card;
            self.hovered_close = next_close;
            unsafe {
                InvalidateRect(self.hwnd, std::ptr::null(), 0);
            }
        }
    }

    fn handle_mouse_leave(&mut self) {
        self.pointer_inside = false;
        self.tracking_mouse = false;
        self.hovered_card = None;
        self.hovered_close = None;
        unsafe {
            InvalidateRect(self.hwnd, std::ptr::null(), 0);
        }
        (self.on_pointer_leave)();
    }

    fn handle_lbutton_up(&mut self, x: i32, y: i32) {
        let Some(idx) = self
            .cards
            .iter()
            .position(|c| pt_in_rect(&c.card_rect, x, y))
        else {
            return;
        };
        let target_hwnd = self.cards[idx].item.hwnd;
        if pt_in_rect(&self.cards[idx].close_rect, x, y) {
            tracker::close_window(target_hwnd);
            if self.cards.len() <= 1 {
                self.hide();
            } else {
                let remaining: Vec<PreviewItem> = self
                    .cards
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| i != idx)
                    .map(|(_, c)| c.item.clone())
                    .collect();
                let key = self.owner_key.clone().unwrap_or_default();
                let mut rc: RECT = unsafe { std::mem::zeroed() };
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(self.hwnd, &mut rc);
                }
                let center_x = (rc.left + rc.right) / 2;
                let bottom_y = rc.bottom;
                self.show(&key, &remaining, center_x, bottom_y);
            }
        } else {
            self.hide();
            tracker::activate_window(target_hwnd);
        }
    }

    fn handle_mbutton_up(&mut self, x: i32, y: i32) {
        let Some(idx) = self
            .cards
            .iter()
            .position(|c| pt_in_rect(&c.card_rect, x, y))
        else {
            return;
        };
        let target_hwnd = self.cards[idx].item.hwnd;
        tracker::close_window(target_hwnd);
        if self.cards.len() <= 1 {
            self.hide();
        }
    }
}

impl Drop for PreviewPopup {
    fn drop(&mut self) {
        self.clear_thumbnails();
        if !self.hwnd.is_null() {
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
        }
    }
}

unsafe extern "system" fn preview_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PreviewPopup };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let popup = unsafe { &mut *ptr };
    match msg {
        WM_PAINT => {
            popup.paint();
            0
        }
        WM_SETCURSOR => {
            let cur = if popup.hovered_card.is_some() {
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
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            popup.handle_mouse_move(x, y);
            0
        }
        WM_MOUSELEAVE => {
            popup.handle_mouse_leave();
            0
        }
        WM_LBUTTONUP => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            popup.handle_lbutton_up(x, y);
            0
        }
        WM_MBUTTONUP => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            popup.handle_mbutton_up(x, y);
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
