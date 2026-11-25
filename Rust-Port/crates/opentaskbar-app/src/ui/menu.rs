//! Native Windows 11 Acrylic + Direct2D/DirectWrite popup context menu.
//!
//! Uses `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW` (Windows 11 Acrylic glass)
//! with hardware-accelerated Direct2D rounded hover pills, DirectWrite grayscale anti-aliasing,
//! `Segoe UI Variable Text`, and `Segoe Fluent Icons`.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::HWND as WinHwnd;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Factory,
    ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::HSTRING;
use windows_numerics::Vector2;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, InvalidateRect, PAINTSTRUCT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos,
    GetMessageW, GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IDC_HAND, LoadCursorW, MSG,
    RegisterClassExW, SW_SHOW, SWP_SHOWWINDOW, SetCursor, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, WM_ACTIVATE, WM_ERASEBKGND, WM_KEYDOWN,
    WM_KILLFOCUS, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WNDCLASSEXW,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::design::icons::{self, AppIcon};
use crate::platform::win32;

const WM_MOUSELEAVE: u32 = 0x02A3;
const DWMWCP_ROUND: u32 = 2;
const DWMSBT_TRANSIENTWINDOW: u32 = 3; // Windows 11 Acrylic backdrop
const MENU_W_DIPS: f32 = 256.0;
const ROW_H_DIPS: f32 = 32.0;
const SEP_H_DIPS: f32 = 9.0;
const PAD_X_DIPS: f32 = 5.0;
const PAD_Y_DIPS: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuIcon {
    Search,
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

fn to_app_icon(icon: MenuIcon) -> AppIcon {
    match icon {
        MenuIcon::Search => AppIcon::Search,
        MenuIcon::Settings => AppIcon::Settings,
        MenuIcon::Activity => AppIcon::Activity,
        MenuIcon::Folder => AppIcon::Folder,
        MenuIcon::Terminal => AppIcon::Terminal,
        MenuIcon::Shield => AppIcon::Shield,
        MenuIcon::Play => AppIcon::Play,
        MenuIcon::Monitor => AppIcon::Monitor,
        MenuIcon::Refresh => AppIcon::RefreshCw,
        MenuIcon::Power => AppIcon::Power,
        MenuIcon::Plus => AppIcon::Plus,
        MenuIcon::Pin => AppIcon::Pin,
        MenuIcon::Close => AppIcon::Close,
        MenuIcon::Trash => AppIcon::Trash2,
        MenuIcon::ChevronLeft => AppIcon::ChevronLeft,
        MenuIcon::ChevronRight => AppIcon::ChevronRight,
        MenuIcon::Layers => AppIcon::Layers,
    }
}

#[derive(Clone, Debug)]
pub enum MenuEntry {
    Item {
        id: u32,
        icon: MenuIcon,
        label: String,
        shortcut: Option<String>,
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
        shortcut: impl Into<String>,
    ) -> Self {
        Self::Item {
            id,
            icon,
            label: label.into(),
            shortcut: Some(shortcut.into()),
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

struct MenuD2d {
    rt: ID2D1HwndRenderTarget,
    text_fmt: IDWriteTextFormat,
    hint_fmt: IDWriteTextFormat,
    icon_fmt: IDWriteTextFormat,
}

struct MenuState {
    hwnd: HWND,
    dpi: f32,
    is_dark: bool,
    acrylic: bool,
    entries: Vec<MenuEntry>,
    row_rects: Vec<Option<RECT>>,
    hovered: Option<usize>,
    tracking: bool,
    selected_id: Option<u32>,
    done: AtomicBool,
    d2d: Option<MenuD2d>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn pt_in_rect(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn rgba_f(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a,
    }
}

fn create_menu_d2d(hwnd: HWND, width_px: u32, height_px: u32, dpi: f32) -> Option<MenuD2d> {
    unsafe {
        let factory: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
        let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0 * dpi,
            dpiY: 96.0 * dpi,
            ..Default::default()
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: WinHwnd(hwnd),
            pixelSize: D2D_SIZE_U {
                width: width_px.max(1),
                height: height_px.max(1),
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        let rt = factory
            .CreateHwndRenderTarget(&rt_props, &hwnd_props)
            .ok()?;
        rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
        let ui_font = HSTRING::from("Segoe UI Variable Text");
        let icon_font = HSTRING::from("Segoe Fluent Icons");
        let locale = HSTRING::from("en-US");

        let text_fmt = dwrite
            .CreateTextFormat(
                &ui_font,
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0,
                &locale,
            )
            .ok()?;
        let _ = text_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = text_fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        let hint_fmt = dwrite
            .CreateTextFormat(
                &ui_font,
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                11.5,
                &locale,
            )
            .ok()?;
        let _ = hint_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
        let _ = hint_fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        let icon_fmt = dwrite
            .CreateTextFormat(
                &icon_font,
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                15.0,
                &locale,
            )
            .ok()?;
        let _ = icon_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = icon_fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        Some(MenuD2d {
            rt,
            text_fmt,
            hint_fmt,
            icon_fmt,
        })
    }
}

/// Shared GDI Fluent icon helper used by `ui/flyout.rs`.
pub(super) unsafe fn draw_menu_icon(
    hdc: HDC,
    icon: MenuIcon,
    cx: i32,
    cy: i32,
    dpi: f32,
    color: u32,
) {
    let size_px = (15.0 * dpi).round() as i32;
    unsafe {
        icons::draw_fluent_icon_gdi(hdc, to_app_icon(icon), cx, cy, size_px, color);
    }
}

/// Displays a native Windows 11 Acrylic popup menu at the cursor and returns the chosen item ID.
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

    let acrylic = unsafe {
        let dark_val: u32 = u32::from(is_dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark_val as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        let backdrop = DWMSBT_TRANSIENTWINDOW;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            (&backdrop as *const u32).cast(),
            size_of::<u32>() as u32,
        ) == 0
    };

    let d2d = create_menu_d2d(hwnd, menu_w as u32, menu_h as u32, dpi);

    let mut state = Box::new(MenuState {
        hwnd,
        dpi,
        is_dark,
        acrylic,
        entries,
        row_rects,
        hovered: None,
        tracking: false,
        selected_id: None,
        done: AtomicBool::new(false),
        d2d,
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
    state.d2d = None;
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
        if let Some(d2d) = &self.d2d {
            let _ = self.paint_d2d(d2d);
        }
        unsafe {
            EndPaint(self.hwnd, &ps);
        }
    }

    fn paint_d2d(&self, d2d: &MenuD2d) -> windows::core::Result<()> {
        let rt = &d2d.rt;
        let (
            clear_col,
            hover_col,
            danger_hover_col,
            text_col,
            icon_col,
            hint_col,
            danger_col,
            sep_col,
        ) = if self.is_dark {
            (
                if self.acrylic {
                    // Subtle dark tint over Windows 11 Acrylic backdrop
                    rgba_f(0x18, 0x18, 0x1b, 0.35)
                } else {
                    rgba_f(0x1a, 0x1a, 0x1e, 0.98)
                },
                rgba_f(0xff, 0xff, 0xff, 0.085),
                rgba_f(0xef, 0x44, 0x44, 0.18),
                rgba_f(0xf4, 0xf4, 0xf5, 0.98),
                rgba_f(0xd4, 0xd4, 0xd8, 0.90),
                rgba_f(0xa1, 0xa1, 0xaa, 0.85),
                rgba_f(0xfc, 0xa5, 0xa5, 1.0),
                rgba_f(0xff, 0xff, 0xff, 0.09),
            )
        } else {
            (
                if self.acrylic {
                    rgba_f(0xf9, 0xf9, 0xfb, 0.35)
                } else {
                    rgba_f(0xff, 0xff, 0xff, 0.98)
                },
                rgba_f(0x00, 0x00, 0x00, 0.055),
                rgba_f(0xdc, 0x26, 0x26, 0.12),
                rgba_f(0x18, 0x18, 0x1b, 0.96),
                rgba_f(0x3f, 0x3f, 0x46, 0.90),
                rgba_f(0x71, 0x71, 0x7a, 0.85),
                rgba_f(0xb9, 0x1c, 0x1c, 1.0),
                rgba_f(0x00, 0x00, 0x00, 0.08),
            )
        };

        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&clear_col));

            let hover_br = rt.CreateSolidColorBrush(&hover_col, None)?;
            let danger_hover_br = rt.CreateSolidColorBrush(&danger_hover_col, None)?;
            let text_br = rt.CreateSolidColorBrush(&text_col, None)?;
            let icon_br = rt.CreateSolidColorBrush(&icon_col, None)?;
            let hint_br = rt.CreateSolidColorBrush(&hint_col, None)?;
            let danger_br = rt.CreateSolidColorBrush(&danger_col, None)?;
            let sep_br = rt.CreateSolidColorBrush(&sep_col, None)?;

            let mut y = PAD_Y_DIPS;
            for (idx, entry) in self.entries.iter().enumerate() {
                match entry {
                    MenuEntry::Item {
                        icon,
                        label,
                        shortcut,
                        destructive,
                        ..
                    } => {
                        let hovered = self.hovered == Some(idx);
                        let left = PAD_X_DIPS;
                        let right = MENU_W_DIPS - PAD_X_DIPS;
                        if hovered {
                            let rr = D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left,
                                    top: y,
                                    right,
                                    bottom: y + ROW_H_DIPS,
                                },
                                radiusX: 6.0,
                                radiusY: 6.0,
                            };
                            rt.FillRoundedRectangle(
                                &rr,
                                if *destructive {
                                    &danger_hover_br
                                } else {
                                    &hover_br
                                },
                            );
                        }

                        // Calm neutral text/icon at rest; crisp danger red on hover.
                        let active_danger = *destructive && hovered;
                        let glyph_w: Vec<u16> = icons::fluent_glyph(to_app_icon(*icon))
                            .encode_utf16()
                            .collect();
                        rt.DrawText(
                            &glyph_w,
                            &d2d.icon_fmt,
                            &D2D_RECT_F {
                                left: left + 6.0,
                                top: y,
                                right: left + 30.0,
                                bottom: y + ROW_H_DIPS,
                            },
                            if active_danger {
                                &danger_br
                            } else if hovered {
                                &text_br
                            } else {
                                &icon_br
                            },
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );

                        let label_w: Vec<u16> = label.encode_utf16().collect();
                        let text_rect = D2D_RECT_F {
                            left: left + 36.0,
                            top: y,
                            right: right - 10.0,
                            bottom: y + ROW_H_DIPS,
                        };
                        rt.DrawText(
                            &label_w,
                            &d2d.text_fmt,
                            &text_rect,
                            if active_danger { &danger_br } else { &text_br },
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );

                        if let Some(sc) = shortcut {
                            let sc_w: Vec<u16> = sc.encode_utf16().collect();
                            rt.DrawText(
                                &sc_w,
                                &d2d.hint_fmt,
                                &text_rect,
                                &hint_br,
                                D2D1_DRAW_TEXT_OPTIONS_NONE,
                                DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                        y += ROW_H_DIPS;
                    }
                    MenuEntry::Separator => {
                        let sy = (y + SEP_H_DIPS * 0.5).floor() + 0.5;
                        rt.DrawLine(
                            Vector2 {
                                X: PAD_X_DIPS + 8.0,
                                Y: sy,
                            },
                            Vector2 {
                                X: MENU_W_DIPS - PAD_X_DIPS - 8.0,
                                Y: sy,
                            },
                            &sep_br,
                            1.0 / self.dpi.max(1.0),
                            None,
                        );
                        y += SEP_H_DIPS;
                    }
                }
            }

            rt.EndDraw(None, None)
        }
    }
}

unsafe extern "system" fn menu_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return 1;
    }
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
