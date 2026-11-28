//! Native Win32 UI dialogs: a themed text prompt (for naming profiles/groups) and
//! modern IFileDialog file/folder pickers (reference SaveFileDialog / OpenFileDialog /
//! OpenFolderDialog parity) used by Settings backups, theme import/export and folder stacks.
//!
//! The pickers use the modern COM API so dark mode, Acrylic shell and per-monitor DPI of the
//! rest of the app are preserved.

use crate::platform::win32_helpers::set_window_long_ptr;
use opentaskbar_core::i18n as loc;

use std::cell::Cell;
use std::rc::Rc;

use crate::design::fonts;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetMonitorInfoW, GetTextExtentPoint32W,
    HBRUSH, HDC, HFONT, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT,
    PS_SOLID, ReleaseDC, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_FOCUS, ODS_SELECTED};
use windows_sys::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, SetFocus, VK_ESCAPE, VK_RETURN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_OWNERDRAW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
    ES_AUTOHSCROLL, GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW, GetWindowTextLengthW,
    GetWindowTextW, IsDialogMessageW, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW,
    SetForegroundWindow, SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
    WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DRAWITEM, WM_ERASEBKGND,
    WM_KEYDOWN, WM_PAINT, WM_SETFONT, WS_CAPTION, WS_CHILD, WS_EX_DLGMODALFRAME, WS_EX_TOPMOST,
    WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

use crate::design::{self, Rgb};
use crate::platform::win32;
use crate::ui::{ModalStep, modal_loop, register_popup_class};

const DWMWCP_ROUND: u32 = 2;
const ID_EDIT: usize = 101;
const ID_OK: usize = 102;
const ID_CANCEL: usize = 103;
const EM_SETSEL: u32 = 0x00B1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn colorref(c: Rgb) -> COLORREF {
    u32::from(c.0) | (u32::from(c.1) << 8) | (u32::from(c.2) << 16)
}

struct PromptState {
    edit_hwnd: HWND,
    is_dark: bool,
    dpi: f32,
    edit_frame_rect: RECT,
    bg_brush: HBRUSH,
    edit_brush: HBRUSH,
    bg_color: COLORREF,
    edit_bg_color: COLORREF,
    text_color: COLORREF,
    sub_color: COLORREF,
    result: Option<String>,
    done: Rc<Cell<bool>>,
}

fn read_edit_text(edit: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(edit);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let copied = GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..copied.max(0) as usize])
            .trim()
            .to_owned()
    }
}

unsafe extern "system" fn prompt_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptState };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };

    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if !hdc.is_null() {
                let pal = design::active_palette(state.is_dark);
                let mut rc: RECT = unsafe { std::mem::zeroed() };
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
                    FillRect(hdc, &rc, state.bg_brush);
                    let pen = CreatePen(PS_SOLID, 1, colorref(pal.border_rgb));
                    let old_pen = SelectObject(hdc, pen as _);
                    let old_brush = SelectObject(hdc, state.edit_brush as _);
                    let r = (7.0 * state.dpi).round() as i32;
                    RoundRect(
                        hdc,
                        state.edit_frame_rect.left,
                        state.edit_frame_rect.top,
                        state.edit_frame_rect.right,
                        state.edit_frame_rect.bottom,
                        r,
                        r,
                    );
                    SelectObject(hdc, old_brush);
                    SelectObject(hdc, old_pen);
                    DeleteObject(pen as _);
                    EndPaint(hwnd, &ps);
                }
            }
            0
        }
        WM_DRAWITEM => {
            let dis = unsafe { &*(lparam as *const DRAWITEMSTRUCT) };
            let pal = design::active_palette(state.is_dark);
            let is_ok = dis.CtlID == ID_OK as u32;
            let pressed = (dis.itemState & ODS_SELECTED) != 0;
            let focused = (dis.itemState & ODS_FOCUS) != 0;
            let fill_rgb = if is_ok {
                if pressed {
                    (
                        pal.accent.0.saturating_sub(24),
                        pal.accent.1.saturating_sub(24),
                        pal.accent.2.saturating_sub(24),
                    )
                } else {
                    pal.accent
                }
            } else if pressed {
                pal.hover
            } else {
                pal.elevated
            };
            let border_rgb = if focused {
                pal.text_primary
            } else if is_ok {
                pal.accent
            } else {
                pal.border_rgb
            };
            let fg_rgb = if is_ok {
                (0xff, 0xff, 0xff)
            } else {
                pal.text_primary
            };
            unsafe {
                FillRect(dis.hDC, &dis.rcItem, state.bg_brush);
                let fill_brush = CreateSolidBrush(colorref(fill_rgb));
                let border_pen = CreatePen(PS_SOLID, 1, colorref(border_rgb));
                let old_brush = SelectObject(dis.hDC, fill_brush as _);
                let old_pen = SelectObject(dis.hDC, border_pen as _);
                let r = (7.0 * state.dpi).round() as i32;
                RoundRect(
                    dis.hDC,
                    dis.rcItem.left,
                    dis.rcItem.top,
                    dis.rcItem.right,
                    dis.rcItem.bottom,
                    r,
                    r,
                );
                SelectObject(dis.hDC, old_brush);
                SelectObject(dis.hDC, old_pen);
                DeleteObject(fill_brush as _);
                DeleteObject(border_pen as _);

                let text = read_edit_text(dis.hwndItem);
                let mut wtxt: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                let mut trc = dis.rcItem;
                SetBkMode(dis.hDC, TRANSPARENT as i32);
                SetTextColor(dis.hDC, colorref(fg_rgb));
                DrawTextW(
                    dis.hDC,
                    wtxt.as_mut_ptr(),
                    (wtxt.len() - 1) as i32,
                    &mut trc,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
            }
            1
        }
        WM_CTLCOLORSTATIC => {
            let hdc = wparam as HDC;
            unsafe {
                SetTextColor(hdc, state.sub_color);
                SetBkColor(hdc, state.bg_color);
            }
            state.bg_brush as LRESULT
        }
        WM_CTLCOLOREDIT => {
            let hdc = wparam as HDC;
            unsafe {
                SetTextColor(hdc, state.text_color);
                SetBkColor(hdc, state.edit_bg_color);
            }
            state.edit_brush as LRESULT
        }
        WM_CTLCOLORBTN => {
            let hdc = wparam as HDC;
            unsafe {
                SetTextColor(hdc, state.text_color);
                SetBkColor(hdc, state.bg_color);
            }
            state.bg_brush as LRESULT
        }
        WM_COMMAND => {
            let id = wparam & 0xffff;
            if id == ID_OK || id == 1 {
                let text = read_edit_text(state.edit_hwnd);
                if !text.is_empty() {
                    state.result = Some(text);
                }
                state.done.set(true);
                return 0;
            }
            if id == ID_CANCEL || id == 2 {
                state.result = None;
                state.done.set(true);
                return 0;
            }
            0
        }
        WM_KEYDOWN => {
            let vk = wparam as u16;
            if vk == VK_ESCAPE {
                state.result = None;
                state.done.set(true);
                return 0;
            }
            if vk == VK_RETURN {
                let text = read_edit_text(state.edit_hwnd);
                if !text.is_empty() {
                    state.result = Some(text);
                }
                state.done.set(true);
                return 0;
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CLOSE => {
            state.result = None;
            state.done.set(true);
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Static control styles (not exported by `windows-sys` under the enabled features).
const SS_NOPREFIX: u32 = 0x0080;
const SS_ENDELLIPSIS: u32 = 0x4000;

/// Minimum prompt client width (DIP) — the previous fixed 340 px window minus its frame.
const PROMPT_MIN_CLIENT_W: f32 = 324.0;
/// Cap for widening the prompt to fit a long label (DIP); longer labels end in an ellipsis.
const PROMPT_MAX_LABEL_CLIENT_W: f32 = 560.0;
const PROMPT_BUTTON_MIN_W: f32 = 88.0;
/// Horizontal text padding inside a button (14 DIP per side).
const PROMPT_BUTTON_TEXT_PAD: f32 = 28.0;
/// Label line height (DIP): tall enough for Turkish diacritics (ğ, ş, ç, İ) not to clip.
const PROMPT_LABEL_MIN_H: f32 = 24.0;

/// Clamps a window origin on one axis into `[lo + margin, hi - size - margin]`. When the
/// window is larger than the work area the upper bound is raised to the lower one, so the
/// range never inverts (`i32::clamp` panics when `min > max`).
fn clamp_axis(preferred: i32, size: i32, lo: i32, hi: i32, margin: i32) -> i32 {
    let min = lo + margin;
    let max = (hi - size - margin).max(min);
    preferred.clamp(min, max)
}

/// Button width in px: at least 88 DIP, otherwise the measured label plus 28 DIP padding.
fn prompt_button_width(text_w_px: i32, dpi: f32) -> i32 {
    let min = (PROMPT_BUTTON_MIN_W * dpi).round() as i32;
    let padded = text_w_px.max(0) + (PROMPT_BUTTON_TEXT_PAD * dpi).round() as i32;
    min.max(padded)
}

/// Measures `text` with the exact GDI font the owner-drawn buttons render with (`DrawTextW`),
/// so the button width matches what is painted. Falls back to a generous estimate.
fn measure_text_width(font: HFONT, text: &str, font_px: i32) -> i32 {
    let wtext: Vec<u16> = text.encode_utf16().collect();
    if wtext.is_empty() {
        return 0;
    }
    let estimate = (wtext.len() as f32 * font_px as f32 * 0.62).ceil() as i32;
    // SAFETY: screen DC acquired and released here; the previous font is restored before release.
    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return estimate;
        }
        let old = if font.is_null() {
            std::ptr::null_mut()
        } else {
            SelectObject(hdc, font as _)
        };
        let mut extent = SIZE { cx: 0, cy: 0 };
        let ok = GetTextExtentPoint32W(hdc, wtext.as_ptr(), wtext.len() as i32, &mut extent);
        if !old.is_null() {
            SelectObject(hdc, old);
        }
        ReleaseDC(std::ptr::null_mut(), hdc);
        if ok != 0 && extent.cx > 0 {
            extent.cx
        } else {
            estimate
        }
    }
}

/// Outer window size for a desired client size (falls back to a conservative frame estimate).
fn window_size_for_client(
    client_w: i32,
    client_h: i32,
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
    dpi: u32,
) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    // SAFETY: plain rect computation with a valid, writable RECT.
    let ok = unsafe { AdjustWindowRectExForDpi(&mut rect, style, 0, ex_style, dpi.max(96)) } != 0;
    if ok {
        (rect.right - rect.left, rect.bottom - rect.top)
    } else {
        let scale = dpi.max(96) as f32 / 96.0;
        (
            client_w + (16.0 * scale).round() as i32,
            client_h + (40.0 * scale).round() as i32,
        )
    }
}

/// Pixel layout of the name prompt's client area (pure, unit-tested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PromptLayout {
    client_w: i32,
    client_h: i32,
    pad: i32,
    label_top: i32,
    label_h: i32,
    edit_top: i32,
    edit_bottom: i32,
    btn_y: i32,
    btn_h: i32,
    save_x: i32,
    save_w: i32,
    cancel_x: i32,
    cancel_w: i32,
}

impl PromptLayout {
    /// `label_w`, `save_w` and `cancel_w` are in px (measured); `client_w_hint` is the actual
    /// client width once the window exists (`0` before creation).
    fn compute(
        dpi: f32,
        font_px: i32,
        label_w: i32,
        save_w: i32,
        cancel_w: i32,
        client_w_hint: i32,
    ) -> Self {
        let s = |dips: f32| -> i32 { (dips * dpi).round() as i32 };
        let pad = s(16.0);
        let gap = s(8.0);
        let label_top = s(12.0);
        let label_h = s(PROMPT_LABEL_MIN_H).max((font_px as f32 * 1.7).ceil() as i32);
        let edit_top = label_top + label_h + s(4.0);
        let edit_h = s(30.0).max(font_px + s(14.0));
        let edit_bottom = edit_top + edit_h;
        let btn_y = edit_bottom + s(12.0);
        let btn_h = s(30.0).max(font_px + s(14.0));
        let client_h = btn_y + btn_h + s(16.0);

        let buttons_w = save_w + gap + cancel_w + pad * 2;
        let label_fit_w = (label_w.max(0) + pad * 2).min(s(PROMPT_MAX_LABEL_CLIENT_W));
        let client_w = s(PROMPT_MIN_CLIENT_W)
            .max(buttons_w)
            .max(label_fit_w)
            .max(client_w_hint);

        // Right-to-left: primary (Save) at the right edge, Cancel to its left.
        let save_x = client_w - pad - save_w;
        let cancel_x = save_x - gap - cancel_w;
        Self {
            client_w,
            client_h,
            pad,
            label_top,
            label_h,
            edit_top,
            edit_bottom,
            btn_y,
            btn_h,
            save_x,
            save_w,
            cancel_x,
            cancel_w,
        }
    }
}

/// Shows a compact, DPI-aware themed modal text input dialog and returns the trimmed non-empty
/// text entered by the user, or `None` if cancelled or empty.
pub fn ask_name(
    owner_hwnd: usize,
    is_dark: bool,
    title: &str,
    label: &str,
    initial: &str,
) -> Option<String> {
    let class_name = wide("OpenTaskBarNamePrompt");
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    register_popup_class(
        "OpenTaskBarNamePrompt",
        prompt_wnd_proc,
        hinstance,
        CS_HREDRAW | CS_VREDRAW,
    );

    let dpi = win32::dpi_scale(owner_hwnd);
    let s = |dips: f32| -> i32 { (dips * dpi).round() as i32 };

    // The font is created first: the button/label layout below is measured with it.
    let font_px = s(fonts::scaled_size(13.0)).max(1);
    let font_face = wide(fonts::native_ui_face());
    let font: HFONT = unsafe {
        CreateFontW(
            -font_px,
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
            font_face.as_ptr(),
        )
    };

    let save_text = loc::t("ThemeSaveChanges");
    let cancel_text = loc::t("Cancel");
    let save_w = prompt_button_width(measure_text_width(font, &save_text, font_px), dpi);
    let cancel_w = prompt_button_width(measure_text_width(font, &cancel_text, font_px), dpi);
    let label_w = measure_text_width(font, label, font_px);
    let mut layout = PromptLayout::compute(dpi, font_px, label_w, save_w, cancel_w, 0);

    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let ex_style = WS_EX_TOPMOST | WS_EX_DLGMODALFRAME;
    let (mut win_w, mut win_h) = window_size_for_client(
        layout.client_w,
        layout.client_h,
        style,
        ex_style,
        (dpi * 96.0).round() as u32,
    );

    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut pt);
    }
    let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    let work = if unsafe { GetMonitorInfoW(mon, &mut mi) } != 0 {
        mi.rcWork
    } else {
        let (sw, sh) = win32::primary_screen_px();
        RECT {
            left: 0,
            top: 0,
            right: sw,
            bottom: sh,
        }
    };
    let margin = 12;
    let mut x = clamp_axis(pt.x - win_w / 2, win_w, work.left, work.right, margin);
    let mut y = clamp_axis(pt.y - win_h - 16, win_h, work.top, work.bottom, margin);

    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            class_name.as_ptr(),
            wide(title).as_ptr(),
            style,
            x,
            y,
            win_w,
            win_h,
            owner_hwnd as HWND,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        if !font.is_null() {
            unsafe {
                DeleteObject(font as _);
            }
        }
        return None;
    }

    let pal = design::active_palette(is_dark);
    let bg_rgb = pal.surface;
    let edit_rgb = pal.elevated;
    let bg_color = colorref(bg_rgb);
    let edit_bg_color = colorref(edit_rgb);
    let text_color = colorref(pal.text_primary);
    let sub_color = colorref(pal.text_secondary);

    let bg_brush = unsafe { CreateSolidBrush(bg_color) };
    let edit_brush = unsafe { CreateSolidBrush(edit_bg_color) };

    unsafe {
        let dark_flag: u32 = u32::from(is_dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark_flag as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const u32).cast(),
            size_of::<u32>() as u32,
        );
    }

    // The real frame (caption height, DPI of the target monitor) may differ from the estimate:
    // grow the window until the measured client area fits the layout, then re-clamp it.
    let mut client: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
    }
    let grow_w = (layout.client_w - (client.right - client.left)).max(0);
    let grow_h = (layout.client_h - (client.bottom - client.top)).max(0);
    if grow_w > 0 || grow_h > 0 {
        win_w += grow_w;
        win_h += grow_h;
        x = clamp_axis(pt.x - win_w / 2, win_w, work.left, work.right, margin);
        y = clamp_axis(pt.y - win_h - 16, win_h, work.top, work.bottom, margin);
        unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x,
                y,
                win_w,
                win_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
        }
    }
    // Anchor the buttons to the real client width (right-to-left: Save, then Cancel).
    layout = PromptLayout::compute(
        dpi,
        font_px,
        label_w,
        save_w,
        cancel_w,
        client.right - client.left,
    );
    let pad = layout.pad;
    let cw = layout.client_w;

    let label_hwnd = unsafe {
        CreateWindowExW(
            0,
            wide("STATIC").as_ptr(),
            wide(label).as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_NOPREFIX | SS_ENDELLIPSIS,
            pad,
            layout.label_top,
            cw - pad * 2,
            layout.label_h,
            hwnd,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };

    let edit_frame_rect = RECT {
        left: pad,
        top: layout.edit_top,
        right: cw - pad,
        bottom: layout.edit_bottom,
    };
    let edit_pad_x = s(8.0);
    let edit_pad_y = s(5.0);
    let edit_hwnd = unsafe {
        CreateWindowExW(
            0,
            wide("EDIT").as_ptr(),
            wide(initial).as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            edit_frame_rect.left + edit_pad_x,
            edit_frame_rect.top + edit_pad_y,
            (edit_frame_rect.right - edit_frame_rect.left - edit_pad_x * 2).max(s(60.0)),
            (edit_frame_rect.bottom - edit_frame_rect.top - edit_pad_y * 2).max(s(16.0)),
            hwnd,
            ID_EDIT as _,
            hinstance,
            std::ptr::null(),
        )
    };

    let save_hwnd = unsafe {
        CreateWindowExW(
            0,
            wide("BUTTON").as_ptr(),
            wide(&save_text).as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
            layout.save_x,
            layout.btn_y,
            layout.save_w,
            layout.btn_h,
            hwnd,
            ID_OK as _,
            hinstance,
            std::ptr::null(),
        )
    };
    let cancel_hwnd = unsafe {
        CreateWindowExW(
            0,
            wide("BUTTON").as_ptr(),
            wide(&cancel_text).as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
            layout.cancel_x,
            layout.btn_y,
            layout.cancel_w,
            layout.btn_h,
            hwnd,
            ID_CANCEL as _,
            hinstance,
            std::ptr::null(),
        )
    };

    for child in [label_hwnd, edit_hwnd, save_hwnd, cancel_hwnd] {
        if !child.is_null() && !font.is_null() {
            unsafe {
                SendMessageW(child, WM_SETFONT, font as WPARAM, 1);
            }
        }
    }

    let done = Rc::new(Cell::new(false));
    let mut state = Box::new(PromptState {
        edit_hwnd,
        is_dark,
        dpi,
        edit_frame_rect,
        bg_brush,
        edit_brush,
        bg_color,
        edit_bg_color,
        text_color,
        sub_color,
        result: None,
        done: Rc::clone(&done),
    });
    let state_ptr = &mut *state as *mut PromptState;
    unsafe {
        set_window_long_ptr(hwnd, GWLP_USERDATA, state_ptr as isize);
        if owner_hwnd != 0 {
            let _ = EnableWindow(owner_hwnd as HWND, 0);
        }
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(edit_hwnd);
        SendMessageW(edit_hwnd, EM_SETSEL, 0, -1);
    }

    modal_loop(|msg| {
        if done.get() {
            return ModalStep::Break;
        }
        // Only keys delivered to this prompt (or its edit control) may confirm
        // or cancel it. Another window on the same thread must not steer it.
        if msg.message == WM_KEYDOWN
            && (msg.hwnd == hwnd || msg.hwnd == edit_hwnd)
            && matches!(msg.wParam as u16, VK_RETURN | VK_ESCAPE)
        {
            let command = if msg.wParam as u16 == VK_RETURN {
                ID_OK
            } else {
                ID_CANCEL
            };
            unsafe {
                SendMessageW(hwnd, WM_COMMAND, command, 0);
            }
            return ModalStep::Skip;
        }
        if unsafe { IsDialogMessageW(hwnd, msg) } != 0 {
            return ModalStep::Skip;
        }
        ModalStep::Continue
    });

    let out = state.result.take();
    unsafe {
        set_window_long_ptr(hwnd, GWLP_USERDATA, 0);
        if owner_hwnd != 0 {
            let _ = EnableWindow(owner_hwnd as HWND, 1);
        }
        DestroyWindow(hwnd);
        if !font.is_null() {
            DeleteObject(font as _);
        }
        if !bg_brush.is_null() {
            DeleteObject(bg_brush as _);
        }
        if !edit_brush.is_null() {
            DeleteObject(edit_brush as _);
        }
    }
    out
}

// Native Windows file/folder pickers (reference Microsoft.Win32.SaveFileDialog / OpenFileDialog /
// OpenFolderDialog parity) used by Settings backups, theme import/export and folder stacks.
// Uses the modern IFileDialog COM API instead of the legacy common-dialog controls so the
// dark mode, Acrylic shell and per-monitor DPI of the rest of the app are preserved.

use std::path::PathBuf;

use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_OVERWRITEPROMPT, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog,
    IFileDialog, IShellItem, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, PWSTR};

fn wide_hstring(text: &str) -> HSTRING {
    HSTRING::from(text)
}

fn init_com() {
    // RPC_E_CHANGED_MODE is fine: the dialog marshals to the owning STA itself.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
}

fn result_path(dialog: &IFileDialog) -> Option<PathBuf> {
    let item: IShellItem = unsafe { dialog.GetResult().ok()? };
    let path: PWSTR = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH).ok()? };
    // Free the COM-allocated string on every path, including conversion failure.
    let text = unsafe { path.to_string().ok().map(|text| text.to_string()) };
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    let text = text?;
    (!text.is_empty()).then(|| PathBuf::from(text))
}

fn set_filter(dialog: &IFileDialog, name: &str, pattern: &str) -> Option<Vec<HSTRING>> {
    let name = wide_hstring(name);
    let pattern = wide_hstring(pattern);
    let spec = [COMDLG_FILTERSPEC {
        pszName: windows::core::PCWSTR(name.as_ptr()),
        pszSpec: windows::core::PCWSTR(pattern.as_ptr()),
    }];
    unsafe { dialog.SetFileTypes(&spec).ok()? };
    unsafe { dialog.SetFileTypeIndex(1).ok()? };
    Some(vec![name, pattern])
}

fn base_dialog(clsid: &windows::core::GUID, title: &str) -> Option<IFileDialog> {
    init_com();
    let dialog: IFileDialog = unsafe { CoCreateInstance(clsid, None, CLSCTX_ALL).ok()? };
    let title = wide_hstring(title);
    unsafe {
        dialog
            .SetTitle(windows::core::PCWSTR(title.as_ptr()))
            .ok()?
    };
    Some(dialog)
}

/// Native "Open file" picker. Returns `None` when the user cancels.
pub fn open_file(title: &str, filter_name: &str, filter_pattern: &str) -> Option<PathBuf> {
    let dialog = base_dialog(&FileOpenDialog, title)?;
    let _filters = set_filter(&dialog, filter_name, filter_pattern)?;
    let options = unsafe { dialog.GetOptions().ok()? } | FOS_FILEMUSTEXIST;
    unsafe { dialog.SetOptions(options).ok()? };
    unsafe { dialog.Show(None).ok()? };
    result_path(&dialog)
}

/// Native "Save file" picker with overwrite confirmation. Returns `None` when cancelled.
pub fn save_file(
    title: &str,
    default_name: &str,
    filter_name: &str,
    filter_pattern: &str,
) -> Option<PathBuf> {
    let dialog = base_dialog(&FileSaveDialog, title)?;
    let _filters = set_filter(&dialog, filter_name, filter_pattern)?;
    let name = wide_hstring(default_name);
    unsafe {
        dialog
            .SetFileName(windows::core::PCWSTR(name.as_ptr()))
            .ok()?
    };
    let options = unsafe { dialog.GetOptions().ok()? } | FOS_OVERWRITEPROMPT;
    unsafe { dialog.SetOptions(options).ok()? };
    unsafe { dialog.Show(None).ok()? };
    result_path(&dialog)
}

/// Native folder picker (used by Settings folder stacks and theme folder import).
pub fn open_folder(title: &str) -> Option<PathBuf> {
    let dialog = base_dialog(&FileOpenDialog, title)?;
    let options = unsafe { dialog.GetOptions().ok()? } | FOS_PICKFOLDERS;
    unsafe { dialog.SetOptions(options).ok()? };
    unsafe { dialog.Show(None).ok()? };
    result_path(&dialog)
}

#[cfg(test)]
mod tests {
    use super::{PromptLayout, clamp_axis, prompt_button_width};

    #[test]
    fn clamp_axis_never_panics_on_narrow_or_short_work_areas() {
        // Normal: centred on the cursor, inside the 12 px margins.
        assert_eq!(clamp_axis(500, 340, 0, 1920, 12), 500);
        assert_eq!(clamp_axis(-50, 340, 0, 1920, 12), 12);
        assert_eq!(clamp_axis(1800, 340, 0, 1920, 12), 1920 - 340 - 12);
        // Work area narrower than the window + margins: previously `min > max` panicked.
        assert_eq!(clamp_axis(100, 340, 0, 300, 12), 12);
        assert_eq!(clamp_axis(-400, 500, 1000, 1200, 12), 1012);
        // Degenerate / empty work area.
        assert_eq!(clamp_axis(0, 156, 0, 0, 12), 12);
    }

    #[test]
    fn turkish_save_button_is_never_clipped() {
        let dpi = 1.0;
        // Short labels keep the 88 DIP minimum.
        assert_eq!(prompt_button_width(40, dpi), 88);
        // "Değişiklikleri kaydet" measures ~130 px at 13 px Segoe UI → 130 + 28 padding.
        assert_eq!(prompt_button_width(130, dpi), 158);
        assert_eq!(prompt_button_width(130, 1.5), 130 + 42);

        let save_w = prompt_button_width(130, dpi);
        let cancel_w = prompt_button_width(30, dpi);
        let layout = PromptLayout::compute(dpi, 13, 120, save_w, cancel_w, 0);
        // Right-to-left, inside the client area, no overlap.
        assert_eq!(layout.save_x + layout.save_w, layout.client_w - layout.pad);
        assert!(layout.cancel_x + layout.cancel_w <= layout.save_x);
        assert!(layout.cancel_x >= layout.pad);
        // Label tall enough for ğ/ş/ç/İ and the edit/buttons stacked below it.
        assert!(layout.label_h >= 24);
        assert!(layout.edit_top >= layout.label_top + layout.label_h);
        assert!(layout.btn_y >= layout.edit_bottom);
        assert!(layout.btn_y + layout.btn_h < layout.client_h);
    }

    #[test]
    fn prompt_widens_for_wide_buttons_and_respects_real_client_width() {
        let dpi = 1.25;
        let save_w = prompt_button_width(260, dpi);
        let cancel_w = prompt_button_width(120, dpi);
        let layout = PromptLayout::compute(dpi, 16, 0, save_w, cancel_w, 0);
        assert!(layout.client_w >= save_w + cancel_w + layout.pad * 2);
        assert!(layout.cancel_x >= layout.pad);

        let wider = PromptLayout::compute(dpi, 16, 0, save_w, cancel_w, layout.client_w + 40);
        assert_eq!(wider.client_w, layout.client_w + 40);
        assert_eq!(wider.save_x + wider.save_w, wider.client_w - wider.pad);
    }
}
