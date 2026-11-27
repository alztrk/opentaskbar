//! Native, keyboard-accessible application window picker grid (issue #174).
//!
//! Port of C# `Views/Popups/AppWindowPicker.cs`. A borderless topmost Win32 popup
//! is centred in the invoking monitor's *work area*, shows one live DWM thumbnail
//! per matching window (3x3 paging at 1080p), and closes on deactivation.
//!
//! Policy decisions (ordering, geometry, paging, navigation) are pure functions in
//! [`crate::window_picker`]; this module owns the window, DWM registrations and the
//! modal message loop. `super::preview` supplies the shared DWM identity type and
//! the Aero-Peek teardown used by the rest of the dock.

use std::cell::RefCell;

use windows::Win32::Foundation::RECT as WinRect;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWriteCreateFactory, IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC as WinHdc;
use windows::core::HSTRING;
use windows_numerics::Vector2;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_VISIBLE,
    DWMWA_BORDER_COLOR, DWMWA_EXCLUDED_FROM_PEEK, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DwmQueryThumbnailSourceSize, DwmRegisterThumbnail,
    DwmSetWindowAttribute, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint, GetMonitorInfoW, HMONITOR, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow, PAINTSTRUCT, SRCCOPY,
    SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GWLP_USERDATA, GetClientRect, GetMessageW, GetWindowLongPtrW, HWND_TOPMOST,
    IDC_ARROW, IDC_HAND, IsWindow, LoadCursorW, MSG, PostMessageW, RegisterClassExW, SW_HIDE,
    SW_SHOW, SWP_NOACTIVATE, SetCursor, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, WA_INACTIVE, WM_ACTIVATE, WM_CLOSE, WM_DESTROY, WM_DPICHANGED,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WNDCLASSEXW,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use opentaskbar_shell::tracker::{self, RunningWindow};

use super::preview::{self, PreviewItem};
use crate::design::{self, Rgb, fonts, gdi_rgb};
use crate::platform::win32::{self, MonitorInfo};
use crate::window_picker::{
    PickerAction, PickerGeometry, PickerKey, center_in_area, geometry_for_area, label, page_count,
};

const WM_MOUSELEAVE_MSG: u32 = 0x02A3;
const DWMWCP_ROUND: u32 = 2;

/// Header band inside the frame (C# title row + its 14px bottom margin).
const HEADER_DIPS: f32 = 58.0;
/// Caption strip at the bottom of every card.
const CAPTION_DIPS: f32 = 28.0;
/// Paging row height (C# `43` in `BuildPage`).
const PAGING_DIPS: f32 = 43.0;
const PAGER_BUTTON_DIPS: f32 = 34.0;
const INNER_PAD_DIPS: f32 = 6.0;
const CORNER_DIPS: f32 = 10.0;

#[derive(Clone)]
struct Card {
    item: PreviewItem,
    rect: RECT,
    preview: RECT,
    thumb: isize,
}

struct PickerState {
    hwnd: HWND,
    dpi: f32,
    is_dark: bool,
    ordered: Vec<RunningWindow>,
    geometry: PickerGeometry,
    /// Client rect `(0, 0, w, h)` used for card layout and pager hit-testing.
    work_area: RECT,
    /// Physical monitor work area, kept for DPI-change recentering.
    monitor_area: RECT,
    page: usize,
    pages: usize,
    /// Index inside the current page (C# `_visible` index, not the HWND).
    selected: usize,
    cards: Vec<Card>,
    hovered: Option<usize>,
    tracking_mouse: bool,
    paging: bool,
    prev_rect: RECT,
    next_rect: RECT,
    closed: bool,
    activating: bool,
}

struct PickerD2d {
    _factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    target: ID2D1DCRenderTarget,
}

thread_local! {
    static PICKER_D2D: RefCell<Option<PickerD2d>> = const { RefCell::new(None) };
}

impl PickerD2d {
    fn create() -> Option<Self> {
        unsafe {
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory.CreateDCRenderTarget(&props).ok()?;
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Some(Self {
                _factory: factory,
                dwrite,
                target,
            })
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn d2d_rgba(c: Rgb, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(c.0) / 255.0,
        g: f32::from(c.1) / 255.0,
        b: f32::from(c.2) / 255.0,
        a: a.clamp(0.0, 1.0),
    }
}

fn pt_in_rect(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn state_ptr(hwnd: HWND) -> *mut PickerState {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PickerState }
}

/// Same monitor identity as C# `DisplayService.GetWindowMonitor`: the `HMONITOR`
/// handle, which is what [`crate::window_picker::order_windows`] compares against.
fn monitor_handle_for_window(hwnd: usize) -> usize {
    if hwnd == 0 {
        return 0;
    }
    // SAFETY: Windows tolerates stale HWNDs here and returns NULL / nearest monitor.
    unsafe { MonitorFromWindow(hwnd as HWND, MONITOR_DEFAULTTONEAREST) as usize }
}

/// Work area of monitor `index` (physical, signed coordinates). Falls back to the
/// display containing the anchor centre when the index is stale or out of range.
fn work_area_for(index: usize, anchor: (i32, i32, i32, i32)) -> (MonitorInfo, RECT) {
    let monitors = win32::enumerate_monitors();
    let center_x = (anchor.0 + anchor.2) / 2;
    let center_y = (anchor.1 + anchor.3) / 2;
    let monitor = monitors
        .get(index)
        .cloned()
        .unwrap_or_else(|| win32::monitor_for_point_px(center_x, center_y));
    let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe {
        GetMonitorInfoW(
            monitor.handle as HMONITOR,
            (&mut info as *mut MONITORINFOEXW).cast(),
        )
    };
    let area = if ok != 0 {
        info.monitorInfo.rcWork
    } else {
        RECT {
            left: monitor.left,
            top: monitor.top,
            right: monitor.right,
            bottom: monitor.bottom,
        }
    };
    (monitor, area)
}

fn release_thumbs(state: &mut PickerState) {
    for card in &mut state.cards {
        if card.thumb != 0 {
            unsafe {
                let _ = DwmUnregisterThumbnail(card.thumb);
            }
            card.thumb = 0;
        }
    }
}

/// Registers one live DWM thumbnail in `preview`, letterboxed to preserve aspect
/// ratio. A failed registration leaves `thumb == 0` so the painter shows the
/// icon/"Preview unavailable" fallback (C# `Visibility.Visible` on the fallback).
fn register_thumbnail(state: &PickerState, card: &mut Card) {
    let mut thumb: isize = 0;
    let ok = unsafe { DwmRegisterThumbnail(state.hwnd, card.item.hwnd as HWND, &mut thumb) };
    if ok != 0 || thumb == 0 {
        card.thumb = 0;
        return;
    }
    let mut src = SIZE { cx: 0, cy: 0 };
    let _ = unsafe { DwmQueryThumbnailSourceSize(thumb, &mut src) };
    let avail_w = (card.preview.right - card.preview.left).max(16);
    let avail_h = (card.preview.bottom - card.preview.top).max(16);
    let (dest_w, dest_h) = if src.cx > 0 && src.cy > 0 {
        let sx = avail_w as f32 / src.cx as f32;
        let sy = avail_h as f32 / src.cy as f32;
        let scale = sx.min(sy).min(1.0);
        (
            ((src.cx as f32 * scale).round() as i32).max(16),
            ((src.cy as f32 * scale).round() as i32).max(16),
        )
    } else {
        (avail_w, avail_h)
    };
    let dx = card.preview.left + (avail_w - dest_w) / 2;
    let dy = card.preview.top + (avail_h - dest_h) / 2;
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
    let _ = unsafe { DwmUpdateThumbnailProperties(thumb, &props) };
    card.thumb = thumb;
}

/// Rebuilds the current page: releases every previous DWM registration, lays out
/// the row-major grid in physical pixels and registers fresh thumbnails. Called
/// on page changes and DPI changes (matching C# `BuildPage` + `QueueThumbnails`).
fn build_page(state: &mut PickerState) {
    release_thumbs(state);
    state.cards.clear();

    let dip = state.dpi;
    let pad = (crate::window_picker::PICKER_FRAME_PADDING_DIPS * dip).round() as i32;
    let gap = (crate::window_picker::PICKER_GAP_DIPS * dip).round() as i32;
    let header = (HEADER_DIPS * dip).round() as i32;
    let paging_h = (PAGING_DIPS * dip).round() as i32;
    let caption = (CAPTION_DIPS * dip).round() as i32;
    let inner = (INNER_PAD_DIPS * dip).round() as i32;
    let card_w = (state.geometry.card_width * dip).round() as i32;
    let card_h = (state.geometry.card_height * dip).round() as i32;
    let page_size = state.geometry.page_size.max(1);
    let start = state
        .page
        .saturating_mul(page_size)
        .min(state.ordered.len());
    let end = start.saturating_add(page_size).min(state.ordered.len());

    for (slot, window) in state.ordered[start..end].iter().enumerate() {
        let row = slot / state.geometry.columns.max(1);
        let col = slot % state.geometry.columns.max(1);
        let left = pad + col as i32 * (card_w + gap) + gap / 2;
        let top = header + row as i32 * (card_h + gap) + gap / 2;
        let rect = RECT {
            left,
            top,
            right: left + card_w,
            bottom: top + card_h,
        };
        let preview = RECT {
            left: rect.left + inner,
            top: rect.top + inner,
            right: rect.right - inner,
            bottom: rect.bottom - caption,
        };
        let mut card = Card {
            item: PreviewItem {
                hwnd: window.hwnd,
                pid: window.pid,
                title: window.title.clone(),
            },
            rect,
            preview,
            thumb: 0,
        };
        register_thumbnail(state, &mut card);
        state.cards.push(card);
    }

    let frame_w = state.work_area.right - state.work_area.left;
    let frame_h = state.work_area.bottom - state.work_area.top;
    let btn_w = (PAGER_BUTTON_DIPS * dip).round() as i32;
    let pager_top = frame_h - pad - paging_h;
    state.prev_rect = RECT {
        left: pad,
        top: pager_top,
        right: pad + btn_w,
        bottom: pager_top + paging_h,
    };
    state.next_rect = RECT {
        left: frame_w - pad - btn_w,
        top: pager_top,
        right: frame_w - pad,
        bottom: pager_top + paging_h,
    };
    if state.selected >= state.cards.len() {
        state.selected = state.cards.len().saturating_sub(1);
    }
}

fn page_label(page: usize, pages: usize) -> String {
    let template = label("WindowPickerPage", "Page {0} of {1}");
    template
        .replace("{0}", &(page + 1).to_string())
        .replace("{1}", &pages.to_string())
}

fn paint(state: &PickerState) {
    let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
    let screen_dc = unsafe { BeginPaint(state.hwnd, &mut ps) };
    if screen_dc.is_null() {
        return;
    }
    let dip = state.dpi;
    let pal = design::palette(state.is_dark);

    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetClientRect(state.hwnd, &mut client) };
    let cw = (client.right - client.left).max(1);
    let ch = (client.bottom - client.top).max(1);

    let mem_dc = unsafe { CreateCompatibleDC(screen_dc) };
    if mem_dc.is_null() {
        unsafe { EndPaint(state.hwnd, &ps) };
        return;
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: cw,
            biHeight: -ch,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        },
        ..unsafe { std::mem::zeroed() }
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let mem_bmp = unsafe {
        CreateDIBSection(
            mem_dc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    if mem_bmp.is_null() {
        unsafe {
            DeleteDC(mem_dc);
            EndPaint(state.hwnd, &ps);
        }
        return;
    }
    let old_bmp = unsafe { SelectObject(mem_dc, mem_bmp as _) };

    PICKER_D2D.with(|slot| {
        let mut guard = slot.borrow_mut();
        if guard.is_none() {
            *guard = PickerD2d::create();
        }
        let Some(d2d) = guard.as_ref() else {
            return;
        };
        let win_rc = WinRect {
            left: 0,
            top: 0,
            right: cw,
            bottom: ch,
        };
        if unsafe { d2d.target.BindDC(WinHdc(mem_dc as _), &win_rc) }.is_err() {
            *guard = None;
            return;
        }
        unsafe {
            d2d.target.BeginDraw();
            d2d.target.Clear(Some(&d2d_rgba(pal.surface, 1.0)));
        }

        // Specular top highlight, matching the dock flyouts.
        if let Ok(brush) = unsafe {
            d2d.target.CreateSolidColorBrush(
                &d2d_rgba(pal.specular_top, if state.is_dark { 0.45 } else { 0.8 }),
                None,
            )
        } {
            let inset = (10.0 * dip).max(1.0);
            unsafe {
                d2d.target.DrawLine(
                    Vector2 { X: inset, Y: 0.5 },
                    Vector2 {
                        X: (cw as f32 - inset).max(inset),
                        Y: 0.5,
                    },
                    &brush,
                    1.0,
                    None,
                );
            }
        }

        let title_fmt = unsafe {
            d2d.dwrite
                .CreateTextFormat(
                    &HSTRING::from(fonts::ui_family_list().as_str()),
                    None,
                    DWRITE_FONT_WEIGHT_MEDIUM,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    (fonts::scaled_size(15.0) * dip).max(9.0),
                    &HSTRING::from(fonts::ui_locale().as_str()),
                )
                .ok()
                .inspect(|fmt| {
                    let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
                    let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                    let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                })
        };
        let caption_fmt = unsafe {
            d2d.dwrite
                .CreateTextFormat(
                    &HSTRING::from(fonts::ui_family_list().as_str()),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    (fonts::scaled_size(11.0) * dip).max(9.0),
                    &HSTRING::from(fonts::ui_locale().as_str()),
                )
                .ok()
                .inspect(|fmt| {
                    let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
                    let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                    let trimming = DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    };
                    let sign = d2d.dwrite.CreateEllipsisTrimmingSign(fmt).ok();
                    let _ = fmt.SetTrimming(&trimming, sign.as_ref());
                })
        };
        let center_fmt = unsafe {
            d2d.dwrite
                .CreateTextFormat(
                    &HSTRING::from(fonts::ui_family_list().as_str()),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    (fonts::scaled_size(11.0) * dip).max(9.0),
                    &HSTRING::from(fonts::ui_locale().as_str()),
                )
                .ok()
                .inspect(|fmt| {
                    let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                    let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                })
        };

        // Header title.
        let header = (HEADER_DIPS * dip).round() as i32;
        let title = label("WindowPickerTitle", "Windows");
        if let Some(ref fmt) = title_fmt
            && let Ok(fg) = unsafe {
                d2d.target
                    .CreateSolidColorBrush(&d2d_rgba(pal.text_primary, 1.0), None)
            }
        {
            let rc = D2D_RECT_F {
                left: (crate::window_picker::PICKER_FRAME_PADDING_DIPS * dip).round() as f32,
                top: 0.0,
                right: cw as f32 - 12.0 * dip,
                bottom: header as f32,
            };
            let text: Vec<u16> = title.encode_utf16().collect();
            unsafe {
                d2d.target.DrawText(
                    &text,
                    fmt,
                    &rc,
                    &fg,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }

        let corner = CORNER_DIPS * dip;
        for (idx, card) in state.cards.iter().enumerate() {
            let is_selected = idx == state.selected;
            let is_hovered = state.hovered == Some(idx);
            let fill = d2d_rgba(
                if is_selected || is_hovered {
                    pal.hover
                } else {
                    pal.elevated
                },
                1.0,
            );
            let stroke = if is_selected {
                d2d_rgba(pal.selected_stroke, 1.0)
            } else {
                d2d_rgba(pal.border_rgb, if state.is_dark { 0.65 } else { 0.9 })
            };
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: card.rect.left as f32 + 0.5,
                    top: card.rect.top as f32 + 0.5,
                    right: card.rect.right as f32 - 0.5,
                    bottom: card.rect.bottom as f32 - 0.5,
                },
                radiusX: corner,
                radiusY: corner,
            };
            if let Ok(bg) = unsafe { d2d.target.CreateSolidColorBrush(&fill, None) } {
                unsafe { d2d.target.FillRoundedRectangle(&rr, &bg) };
            }
            if let Ok(border) = unsafe { d2d.target.CreateSolidColorBrush(&stroke, None) } {
                unsafe { d2d.target.DrawRoundedRectangle(&rr, &border, 1.0, None) };
            }

            // Preview well (the DWM thumbnail composites directly above it).
            let well = D2D_RECT_F {
                left: card.preview.left as f32,
                top: card.preview.top as f32,
                right: card.preview.right as f32,
                bottom: card.preview.bottom as f32,
            };
            if let Ok(bg) = unsafe {
                d2d.target
                    .CreateSolidColorBrush(&d2d_rgba(pal.surface, 1.0), None)
            } {
                unsafe { d2d.target.FillRectangle(&well, &bg) };
            }
            if card.thumb == 0 {
                // C# fallback: app icon + "Preview unavailable".
                let icon_size = 36.0 * dip;
                let cx = (card.preview.left + card.preview.right) as f32 / 2.0;
                let cy = card.preview.top as f32 + icon_size * 0.85;
                let icon = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: cx - icon_size / 2.0,
                        top: cy - icon_size / 2.0,
                        right: cx + icon_size / 2.0,
                        bottom: cy + icon_size / 2.0,
                    },
                    radiusX: 6.0 * dip,
                    radiusY: 6.0 * dip,
                };
                if let Ok(bg) = unsafe {
                    d2d.target
                        .CreateSolidColorBrush(&d2d_rgba(pal.elevated, 1.0), None)
                } {
                    unsafe { d2d.target.FillRoundedRectangle(&icon, &bg) };
                }
                if let Ok(border) = unsafe {
                    d2d.target
                        .CreateSolidColorBrush(&d2d_rgba(pal.border_rgb, 0.9), None)
                } {
                    unsafe { d2d.target.DrawRoundedRectangle(&icon, &border, 1.0, None) };
                }
                let bar = D2D_RECT_F {
                    left: icon.rect.left + 2.0 * dip,
                    top: icon.rect.top + 2.0 * dip,
                    right: icon.rect.right - 2.0 * dip,
                    bottom: icon.rect.top + 8.0 * dip,
                };
                if let Ok(bg) = unsafe {
                    d2d.target
                        .CreateSolidColorBrush(&d2d_rgba(pal.selected_stroke, 0.8), None)
                } {
                    unsafe { d2d.target.FillRectangle(&bar, &bg) };
                }
                if let Some(ref fmt) = center_fmt
                    && let Ok(fg) = unsafe {
                        d2d.target
                            .CreateSolidColorBrush(&d2d_rgba(pal.text_primary, 0.75), None)
                    }
                {
                    let text: Vec<u16> = label("PreviewUnavailable", "Preview unavailable")
                        .encode_utf16()
                        .collect();
                    let rc = D2D_RECT_F {
                        left: card.preview.left as f32,
                        top: cy + icon_size * 0.6,
                        right: card.preview.right as f32,
                        bottom: card.preview.bottom as f32,
                    };
                    unsafe {
                        d2d.target.DrawText(
                            &text,
                            fmt,
                            &rc,
                            &fg,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                }
            }

            // Caption below the preview (C# `Dock.Bottom` text block).
            if let Some(ref fmt) = caption_fmt
                && let Ok(fg) = unsafe {
                    d2d.target
                        .CreateSolidColorBrush(&d2d_rgba(pal.text_primary, 1.0), None)
                }
            {
                let name = if card.item.title.trim().is_empty() {
                    label("WindowPickerTitle", "Windows")
                } else {
                    card.item.title.clone()
                };
                let text: Vec<u16> = name.encode_utf16().collect();
                let caption_px = (CAPTION_DIPS * dip).round() as i32;
                let rc = D2D_RECT_F {
                    left: card.rect.left as f32 + INNER_PAD_DIPS * dip,
                    top: (card.rect.bottom - caption_px) as f32,
                    right: card.rect.right as f32 - INNER_PAD_DIPS * dip,
                    bottom: card.rect.bottom as f32,
                };
                unsafe {
                    d2d.target.DrawText(
                        &text,
                        fmt,
                        &rc,
                        &fg,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }
        }

        if state.paging {
            if let Some(ref fmt) = center_fmt
                && let Ok(fg) = unsafe {
                    d2d.target
                        .CreateSolidColorBrush(&d2d_rgba(pal.text_primary, 1.0), None)
                }
            {
                for (rect, text) in [
                    (state.prev_rect, label("WindowPickerPrevious", "<")),
                    (state.next_rect, label("WindowPickerNext", ">")),
                ] {
                    let rc = D2D_RECT_F {
                        left: rect.left as f32,
                        top: rect.top as f32,
                        right: rect.right as f32,
                        bottom: rect.bottom as f32,
                    };
                    let wide_text: Vec<u16> = text.encode_utf16().collect();
                    unsafe {
                        d2d.target.DrawText(
                            &wide_text,
                            fmt,
                            &rc,
                            &fg,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                }
                let rc = D2D_RECT_F {
                    left: state.prev_rect.right as f32,
                    top: state.prev_rect.top as f32,
                    right: state.next_rect.left as f32,
                    bottom: state.prev_rect.bottom as f32,
                };
                let text: Vec<u16> = page_label(state.page, state.pages).encode_utf16().collect();
                unsafe {
                    d2d.target.DrawText(
                        &text,
                        fmt,
                        &rc,
                        &fg,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }
        }

        if unsafe { d2d.target.EndDraw(None, None) }.is_err() {
            *guard = None;
        }
    });

    unsafe {
        let _ = BitBlt(screen_dc, 0, 0, cw, ch, mem_dc, 0, 0, SRCCOPY);
        SelectObject(mem_dc, old_bmp);
        DeleteObject(mem_bmp as _);
        DeleteDC(mem_dc);
        EndPaint(state.hwnd, &ps);
    }
}

fn close(state: &mut PickerState) {
    if state.closed {
        return;
    }
    state.closed = true;
    release_thumbs(state);
    unsafe {
        ShowWindow(state.hwnd, SW_HIDE);
        DestroyWindow(state.hwnd);
    }
}

fn select_window(state: &mut PickerState) {
    let Some((hwnd, pid)) = state
        .cards
        .get(state.selected)
        .map(|card| (card.item.hwnd, card.item.pid))
    else {
        return;
    };
    let identity = [(hwnd, pid)];
    // Close first: activating the target deactivates the picker, which must not
    // trigger a second close / activation cycle.
    state.activating = true;
    close(state);
    let _ = tracker::activate_or_toggle_verified(&identity);
}

fn apply_action(state: &mut PickerState, action: PickerAction) {
    match action {
        PickerAction::Move(index) => {
            state.selected = index.min(state.cards.len().saturating_sub(1));
        }
        PickerAction::SetPage(page) => {
            state.page = page.min(state.pages.saturating_sub(1));
            state.selected = 0;
            build_page(state);
        }
        PickerAction::Confirm => select_window(state),
        PickerAction::Close => close(state),
        PickerAction::Ignore => {}
    }
}

fn refresh_geometry(state: &mut PickerState) {
    state.dpi = win32::dpi_scale(state.hwnd as usize);
    state.geometry = geometry_for_area(
        state.monitor_area.right - state.monitor_area.left,
        state.monitor_area.bottom - state.monitor_area.top,
        state.ordered.len(),
    );
    state.pages = page_count(state.ordered.len(), state.geometry.page_size);
    state.paging = state.ordered.len() > state.geometry.page_size;
    state.page = state.page.min(state.pages.saturating_sub(1));

    let (w_dips, h_dips) = state.geometry.frame_size(
        state.geometry.page_size.min(state.ordered.len()),
        state.paging,
    );
    let w = ((w_dips as f32) * state.dpi).round() as i32;
    let h = ((h_dips as f32) * state.dpi).round() as i32;
    let (x, y) = center_in_area(
        (
            state.monitor_area.left,
            state.monitor_area.top,
            state.monitor_area.right,
            state.monitor_area.bottom,
        ),
        w,
        h,
    );
    // The layout rect is the window's client area; DWM thumbnails must align
    // with the painted cards exactly.
    state.work_area = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    unsafe {
        SetWindowPos(state.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
    }
    build_page(state);
    unsafe { InvalidateRect(state.hwnd, std::ptr::null(), 1) };
}

unsafe extern "system" fn picker_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = state_ptr(hwnd);
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(state);
            0
        }
        WM_KEYDOWN => {
            let key = match wparam as u16 {
                VK_LEFT => Some(PickerKey::Left),
                VK_RIGHT => Some(PickerKey::Right),
                VK_UP => Some(PickerKey::Up),
                VK_DOWN => Some(PickerKey::Down),
                VK_PRIOR => Some(PickerKey::PageUp),
                VK_NEXT => Some(PickerKey::PageDown),
                VK_HOME => Some(PickerKey::Home),
                VK_END => Some(PickerKey::End),
                VK_ESCAPE => Some(PickerKey::Escape),
                VK_RETURN | VK_SPACE => Some(PickerKey::Confirm),
                _ => None,
            };
            if let Some(key) = key {
                let action = crate::window_picker::navigate(
                    key,
                    state.selected,
                    state.cards.len(),
                    state.geometry.columns,
                    state.page,
                    state.pages,
                );
                apply_action(state, action);
                if !state.closed {
                    unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
                }
            }
            0
        }
        WM_ACTIVATE => {
            if (wparam as u32 & 0xffff) == WA_INACTIVE as u32 && !state.activating && !state.closed
            {
                unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
            }
            0
        }
        WM_CLOSE => {
            close(state);
            0
        }
        WM_DESTROY => {
            release_thumbs(state);
            0
        }
        WM_DPICHANGED => {
            refresh_geometry(state);
            0
        }
        WM_MOUSELEAVE_MSG => {
            state.hovered = None;
            state.tracking_mouse = false;
            unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
            0
        }
        WM_MOUSEMOVE => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            if !state.tracking_mouse {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    TrackMouseEvent(&mut track);
                }
                state.tracking_mouse = true;
            }
            let next = state.cards.iter().position(|c| pt_in_rect(&c.rect, x, y));
            if next != state.hovered {
                state.hovered = next;
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
            }
            0
        }
        WM_LBUTTONUP => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            if state.paging {
                if pt_in_rect(&state.prev_rect, x, y) {
                    let action = crate::window_picker::navigate(
                        PickerKey::PageUp,
                        state.selected,
                        state.cards.len(),
                        state.geometry.columns,
                        state.page,
                        state.pages,
                    );
                    apply_action(state, action);
                    return 0;
                }
                if pt_in_rect(&state.next_rect, x, y) {
                    let action = crate::window_picker::navigate(
                        PickerKey::PageDown,
                        state.selected,
                        state.cards.len(),
                        state.geometry.columns,
                        state.page,
                        state.pages,
                    );
                    apply_action(state, action);
                    return 0;
                }
            }
            if let Some(idx) = state.cards.iter().position(|c| pt_in_rect(&c.rect, x, y)) {
                state.selected = idx;
                select_window(state);
            }
            0
        }
        WM_SETCURSOR => {
            // `lParam` carries the hit-test code, not coordinates; hover state
            // is maintained by WM_MOUSEMOVE / WM_MOUSELEAVE.
            let cursor = if state.hovered.is_some() {
                IDC_HAND
            } else {
                IDC_ARROW
            };
            unsafe {
                SetCursor(LoadCursorW(std::ptr::null_mut(), cursor));
            }
            1
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl Drop for PickerState {
    fn drop(&mut self) {
        release_thumbs(self);
        if !self.hwnd.is_null() && unsafe { IsWindow(self.hwnd) } != 0 {
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Shows the picker on the calling (UI) thread and pumps a nested modal loop
/// until a card is selected, Escape is pressed or the popup loses activation.
pub fn show(monitor: usize, windows: Vec<RunningWindow>, anchor: (i32, i32, i32, i32)) -> bool {
    if windows.is_empty() {
        return false;
    }

    let (target, area) = work_area_for(monitor, anchor);
    let work_w = (area.right - area.left).max(1);
    let work_h = (area.bottom - area.top).max(1);
    let geometry = geometry_for_area(work_w, work_h, windows.len());
    let pages = page_count(windows.len(), geometry.page_size);
    let paging = windows.len() > geometry.page_size;
    let (w_dips, h_dips) = geometry.frame_size(geometry.page_size.min(windows.len()), paging);
    let dpi = if target.dpi_scale > 0.0 {
        target.dpi_scale
    } else {
        1.0
    };
    let width = ((w_dips as f32) * dpi).round() as i32;
    let height = ((h_dips as f32) * dpi).round() as i32;
    let (x, y) = center_in_area(
        (area.left, area.top, area.right, area.bottom),
        width,
        height,
    );

    let class_name = wide("OpenTaskBarWindowPicker");
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(picker_wnd_proc),
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
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name.as_ptr(),
            wide("").as_ptr(),
            WS_POPUP,
            x,
            y,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return false;
    }

    let is_dark = win32::read_system_is_dark();
    unsafe {
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let dark: u32 = u32::from(is_dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let border = gdi_rgb(design::palette(is_dark).border_rgb);
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

    // `monitor` is an index; C# orders by the HMONITOR of the invoking display,
    // so compare window monitors against the resolved monitor's handle.
    let ordered = crate::window_picker::order_windows(
        windows,
        &crate::window_picker::PickerApp::default(),
        target.handle,
        monitor_handle_for_window,
    );
    if ordered.is_empty() {
        unsafe { DestroyWindow(hwnd) };
        return false;
    }

    let state = Box::new(PickerState {
        hwnd,
        dpi: win32::dpi_scale(hwnd as usize),
        is_dark,
        ordered,
        geometry,
        // Layout coordinates are relative to the client area.
        work_area: RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        },
        monitor_area: area,
        page: 0,
        pages,
        selected: 0,
        cards: Vec::new(),
        hovered: None,
        tracking_mouse: false,
        paging,
        prev_rect: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        next_rect: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        closed: false,
        activating: false,
    });
    let ptr = Box::into_raw(state);
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
    }
    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE);
        ShowWindow(hwnd, SW_SHOW);
        // C# `ShowActivated = true`: the popup must own the keyboard focus.
        AllowSetForegroundWindow(ASFW_ANY);
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
    }
    {
        // Register DWM thumbnails only after the destination window is visible,
        // matching C# `ContentRendered` -> `QueueThumbnails`.
        let state = unsafe { &mut *ptr };
        build_page(state);
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 1) };
    }

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let closed = unsafe { (*ptr).closed };
        if closed || unsafe { IsWindow(hwnd) } == 0 {
            break;
        }
        let status = unsafe { GetMessageW(&mut msg, hwnd, 0, 0) };
        if status <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe {
        drop(Box::from_raw(ptr));
        preview::deactivate_live_preview();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_label_formats_csharp_placeholders() {
        // The catalog key may still be missing in worktrees where WS-2 has not
        // landed; `label` falls back to the English template in that case.
        let text = page_label(2, 4);
        assert!(text.contains('3') && text.contains('4'), "{text}");
    }

    #[test]
    fn picker_constants_follow_csharp_layout() {
        assert_eq!(HEADER_DIPS, 58.0);
        assert_eq!(PAGING_DIPS, 43.0);
        assert_eq!(CORNER_DIPS, 10.0);
    }
}
