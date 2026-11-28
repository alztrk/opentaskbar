//! Native Win32 + DWM live window thumbnail preview popup (`DwmRegisterThumbnail`).
//!
//! Renders up to 6 live hardware-composited DWM window thumbnails side by side above the hovered
//! dock icon with crisp window titles, hover states, click-to-activate, middle-click close,
//! and an `X` close button per card. Uses 0% 3D GPU overhead because DWM composites thumbnails
//! directly into the desktop pass.

use super::layout;
use crate::design::spatial_glass::{
    DCompForeground, SpatialGlassConfig, SpatialGlassHost, configure_host_backdrop_window,
};
use crate::design::{self, Rgb, fonts, gdi_rgb};
use crate::platform::win32_helpers::set_window_long_ptr;
use opentaskbar_shell::tracker;
use std::cell::RefCell;
use std::sync::OnceLock;
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
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmSetWindowAttribute,
    DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint, InvalidateRect, PAINTSTRUCT, SRCCOPY,
    SelectObject,
};

use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetClientRect,
    GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IDC_HAND, LoadCursorW, RegisterClassExW, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetCursor, SetWindowPos, ShowWindow,
    WM_ERASEBKGND, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::platform::win32;

type DwmActivateFn = unsafe extern "system" fn(HWND, u32, i32, u32) -> i32;
type DwmDeactivateFn = unsafe extern "system" fn() -> i32;

// Resolve optional DWM live-preview exports dynamically so linking succeeds against standard
// Windows SDK `dwmapi.lib`, which does not export these symbols by name.
fn dwm_live_preview_fns() -> (Option<DwmActivateFn>, Option<DwmDeactivateFn>) {
    static FNS: OnceLock<(Option<DwmActivateFn>, Option<DwmDeactivateFn>)> = OnceLock::new();
    *FNS.get_or_init(|| unsafe {
        let dll: Vec<u16> = "dwmapi.dll\0".encode_utf16().collect();
        let hmod = GetModuleHandleW(dll.as_ptr());
        if hmod.is_null() {
            return (None, None);
        }
        let act = GetProcAddress(hmod, c"DwmActivateLivePreview".as_ptr().cast())
            .map(|f| std::mem::transmute::<unsafe extern "system" fn() -> isize, DwmActivateFn>(f));
        let deact = GetProcAddress(hmod, c"DwmDeactivateLivePreview".as_ptr().cast()).map(|f| {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, DwmDeactivateFn>(f)
        });
        (act, deact)
    })
}

/// Activates Aero Peek on the first thumbnail's source window when the export is present.
pub fn activate_live_preview(hwnd: usize) -> bool {
    let Some(f) = dwm_live_preview_fns().0 else {
        return false;
    };
    unsafe { f(hwnd as HWND, 0, 0, 0) == 0 }
}

pub fn deactivate_live_preview() {
    if let Some(f) = dwm_live_preview_fns().1 {
        unsafe {
            let _ = f();
        }
    }
}

/// Honours the user's `HKCU\Software\Microsoft\Windows\DWM\EnableAeroPeek`
/// setting. A missing value, a failed query or a non-DWORD value defaults to
/// enabled; only an explicit REG_DWORD `0` suppresses Aero Peek. The registry is
/// read once per preview show (an infrequent, hover-settled event), never from
/// the VSync or mouse hot path.
pub fn aero_peek_enabled() -> bool {
    let path: Vec<u16> = "Software\\Microsoft\\Windows\\DWM\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "EnableAeroPeek\0".encode_utf16().collect();
    let mut key = std::ptr::null_mut();
    // SAFETY: both buffers are NUL-terminated and `key` is only used on success.
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return true;
        }
        let mut value: u32 = 0;
        let mut len = size_of::<u32>() as u32;
        let status = RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut len,
        );
        RegCloseKey(key);
        if status != 0 || len != size_of::<u32>() as u32 {
            return true;
        }
        super::layout::aero_peek_enabled(Some(value))
    }
}

/// Clamps an overlay window to the primary screen (`PositionOverlayWindow`).
pub fn position_overlay_window(
    width: i32,
    height: i32,
    center_x: i32,
    bottom_y: i32,
    screen: (i32, i32),
    margin: i32,
) -> (i32, i32) {
    let x = (center_x - width / 2).clamp(margin, (screen.0 - width - margin).max(margin));
    let y = (bottom_y - height).clamp(margin, (screen.1 - height - margin).max(margin));
    (x, y)
}

/// Physical card size so `count` cards, their gaps, the outer padding and the overlay
/// margins all fit inside the work-area width `work_w`. When the preferred `desired`
/// size would overflow (e.g. 6 previews on 1920 px at 150%), cards shrink
/// proportionally, keeping the thumbnail aspect and every close button on screen.
fn fit_card_size(
    desired: (i32, i32),
    count: i32,
    work_w: i32,
    pad: i32,
    gap: i32,
    margin: i32,
    min_h: i32,
) -> (i32, i32) {
    let count = count.max(1);
    let (want_w, want_h) = (desired.0.max(1), desired.1.max(1));
    let available = work_w - 2 * (margin + pad) - gap * (count - 1);
    let fit_w = (available / count).max(1);
    if want_w <= fit_w {
        return (want_w, want_h);
    }
    let scaled_h = (i64::from(want_h) * i64::from(fit_w) / i64::from(want_w)) as i32;
    (fit_w, scaled_h.max(min_h.min(want_h)).max(1))
}

const WM_MOUSELEAVE: u32 = 0x02A3;
const MAX_CARDS: usize = 6;
/// `Design/Tokens/Layout.xaml`: PreviewWidth/Height = 228x154 DIP.
/// The Large profile uses 250x180 (set by the dock at show time).
const CARD_W_DIPS: f32 = layout::PREVIEW_CARD_DIPS.0;
const CARD_H_DIPS: f32 = layout::PREVIEW_CARD_DIPS.1;
const HEADER_H_DIPS: f32 = 32.0;
const CARD_GAP_DIPS: f32 = 8.0;
const OUTER_PAD_DIPS: f32 = 8.0;
const CLOSE_BTN_DIPS: f32 = 22.0;

#[derive(Clone, Debug)]
pub struct PreviewItem {
    pub hwnd: usize,
    /// Process captured with the HWND, used to reject recycled handles before acting on them.
    pub pid: u32,
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
    card_w_dips: f32,
    card_h_dips: f32,
    is_dark: bool,
    glass_mode: bool,
    glass_tint_alpha: f32,
    glass_config: SpatialGlassConfig,
    spatial_glass: Option<SpatialGlassHost>,
    dcomp_fg: Option<DCompForeground>,
    visible: bool,
    pointer_inside: bool,
    tracking_mouse: bool,
    owner_key: Option<String>,
    cards: Vec<RegisteredCard>,
    hovered_card: Option<usize>,
    hovered_close: Option<usize>,
    on_pointer_leave: Box<dyn FnMut()>,
}

struct PreviewD2d {
    _factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    target: ID2D1DCRenderTarget,
}

thread_local! {
    static PREVIEW_D2D: RefCell<Option<PreviewD2d>> = const { RefCell::new(None) };
}

impl PreviewD2d {
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

fn d2d_rgba(c: Rgb, a: f32) -> D2D1_COLOR_F {
    design::d2d::d2d_color(c, a)
}

use opentaskbar_core::wide;

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
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
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

        let default_glass = SpatialGlassConfig::disabled(true);
        configure_host_backdrop_window(hwnd as usize, true, false);
        let spatial_glass = None;
        let dcomp_fg = None;

        unsafe {
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
            card_w_dips: CARD_W_DIPS,
            card_h_dips: CARD_H_DIPS,
            is_dark: true,
            glass_mode: false,
            glass_tint_alpha: default_glass.tint_alpha,
            glass_config: default_glass,
            spatial_glass,
            dcomp_fg,
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
        set_window_long_ptr(hwnd, GWLP_USERDATA, ptr);
        Some(popup)
    }

    pub fn set_glass_config(&mut self, config: SpatialGlassConfig) {
        // Active theme packs tint the preview glass body and rim; stock themes stay neutral.
        let (tint, rim) = design::theme::theme_surface_tint("MenuMaterial", "DockBorderMaterial");
        let config = config.with_theme_tint(tint, rim);
        self.glass_mode = config.enabled;
        self.glass_tint_alpha = config.tint_alpha;
        self.glass_config = config;
        configure_host_backdrop_window(self.hwnd as usize, config.is_dark, config.enabled);
        if config.enabled {
            if let Some(ref mut glass) = self.spatial_glass {
                glass.apply_config(config);
            } else {
                self.spatial_glass = SpatialGlassHost::attach(self.hwnd as usize, false, config);
            }
            if self.dcomp_fg.is_none() {
                self.dcomp_fg = DCompForeground::new(self.hwnd as usize);
            }
        } else {
            self.spatial_glass = None;
            self.dcomp_fg = None;
        }
    }

    /// Applies the reference size-profile card dimensions (228x154 / 250x180 DIP).
    pub fn set_card_size(&mut self, width_dips: f32, height_dips: f32) {
        if width_dips.is_finite() && width_dips >= 120.0 {
            self.card_w_dips = width_dips;
        }
        if height_dips.is_finite() && height_dips >= 80.0 {
            self.card_h_dips = height_dips;
        }
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
        is_dark: bool,
    ) {
        let count = items.len().min(MAX_CARDS);
        if count == 0 {
            self.hide();
            return;
        }

        self.clear_thumbnails();
        self.is_dark = is_dark;
        let pal = design::active_palette(is_dark);
        unsafe {
            let dark_val: u32 = u32::from(is_dark);
            let _ = DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                (&dark_val as *const u32).cast(),
                size_of::<u32>() as u32,
            );
            let border = gdi_rgb(pal.border_rgb);
            let _ = DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_BORDER_COLOR as u32,
                (&border as *const u32).cast(),
                size_of::<u32>() as u32,
            );
        }
        // Resolve the DPI of the monitor the cards will appear on *before* sizing
        // them: the popup HWND may still sit on another display from its last show.
        let (work, monitor_dpi) =
            crate::ui::flyout::monitor_work_area_at(anchor_center_x_px, anchor_bottom_y_px);
        self.dpi = monitor_dpi
            .filter(|d| d.is_finite() && *d > 0.0)
            .unwrap_or_else(|| win32::dpi_scale(self.hwnd as usize))
            .max(0.5);
        let dpi = self.dpi;

        let header_h = (HEADER_H_DIPS * dpi).round() as i32;
        let gap = (CARD_GAP_DIPS * dpi).round() as i32;
        let pad = (OUTER_PAD_DIPS * dpi).round() as i32;
        let close_sz = (CLOSE_BTN_DIPS * dpi).round() as i32;
        let inner_pad = (6.0 * dpi).round() as i32;
        let margin = (8.0 * dpi).round() as i32;
        let work_w = (work.right - work.left).max(1);
        let work_h = (work.bottom - work.top).max(1);

        let (card_w, card_h) = fit_card_size(
            (
                (self.card_w_dips * dpi).round() as i32,
                (self.card_h_dips * dpi).round() as i32,
            ),
            count as i32,
            work_w,
            pad,
            gap,
            margin,
            header_h + inner_pad + (40.0 * dpi).round() as i32,
        );

        let total_w = pad * 2 + card_w * (count as i32) + gap * ((count as i32) - 1);
        let total_h = pad * 2 + card_h;

        let (rel_x, rel_y) = position_overlay_window(
            total_w,
            total_h,
            anchor_center_x_px - work.left,
            anchor_bottom_y_px - work.top,
            (work_w, work_h),
            margin,
        );
        let x = work.left + rel_x;
        let y = work.top + rel_y;

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

    fn paint(&mut self) {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        let screen_hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if screen_hdc.is_null() {
            return;
        }
        let dpi = self.dpi;
        let header_h = HEADER_H_DIPS * dpi;
        let inner_pad = 9.0 * dpi;
        let corner = 10.0 * dpi;
        let pal = design::active_palette(self.is_dark);

        let mut client = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        unsafe {
            GetClientRect(self.hwnd, &mut client);
        }
        let cw = (client.right - client.left).max(1);
        let ch = (client.bottom - client.top).max(1);
        let use_spatial_glass = self.dcomp_fg.is_some() && self.glass_config.enabled;
        if let Some(ref mut glass) = self.spatial_glass {
            glass.sync_rect(
                0.0,
                0.0,
                cw as f32,
                ch as f32,
                (14.0 * dpi).round(),
                dpi,
                0.0,
                0.0,
            );
        }

        let mem_dc = unsafe { CreateCompatibleDC(screen_hdc) };
        if mem_dc.is_null() {
            unsafe { EndPaint(self.hwnd, &ps) };
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
                EndPaint(self.hwnd, &ps);
            }
            return;
        }
        let old_bmp = unsafe { SelectObject(mem_dc, mem_bmp as _) };

        PREVIEW_D2D.with(|slot| {
            let mut guard = slot.borrow_mut();
            if guard.is_none() {
                *guard = PreviewD2d::create();
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
                let bg = if use_spatial_glass {
                    d2d_rgba((0, 0, 0), 0.0)
                } else if self.glass_mode {
                    let tint = if self.is_dark {
                        (0x10_u8, 0x12_u8, 0x18_u8)
                    } else {
                        (0xf6_u8, 0xf8_u8, 0xfc_u8)
                    };
                    d2d_rgba(tint, self.glass_tint_alpha.clamp(0.18, 0.48))
                } else {
                    d2d_rgba(pal.surface, 1.0)
                };
                d2d.target.Clear(Some(&bg));
            }

            // Subtle top specular highlight matching flyouts and menus.
            if let Ok(spec_brush) = unsafe {
                d2d.target.CreateSolidColorBrush(
                    &d2d_rgba(pal.specular_top, if self.is_dark { 0.45 } else { 0.8 }),
                    None,
                )
            } {
                let inset = 10.0 * dpi;
                unsafe {
                    d2d.target.DrawLine(
                        Vector2 { X: inset, Y: 0.5 },
                        Vector2 {
                            X: (cw as f32 - inset).max(inset),
                            Y: 0.5,
                        },
                        &spec_brush,
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
                        (fonts::scaled_size(12.0) * dpi).max(9.0),
                        &HSTRING::from(fonts::ui_locale().as_str()),
                    )
                    .ok()
                    .inspect(|fmt| {
                        let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
                        let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                        let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                        let trimming = DWRITE_TRIMMING {
                            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                            delimiter: 0,
                            delimiterCount: 0,
                        };
                        let sign = d2d.dwrite.CreateEllipsisTrimmingSign(fmt).ok();
                        let _ = fmt.SetTrimming(&trimming, sign.as_ref());
                    })
            };
            let close_fmt = unsafe {
                d2d.dwrite
                    .CreateTextFormat(
                        &HSTRING::from(design::icons::icon_font_family()),
                        None,
                        DWRITE_FONT_WEIGHT_NORMAL,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        (10.5 * dpi).max(9.0),
                        &HSTRING::from(design::fonts::ui_locale()),
                    )
                    .ok()
                    .inspect(|fmt| {
                        let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                        let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                    })
            };

            for (idx, card) in self.cards.iter().enumerate() {
                let is_hovered = self.hovered_card == Some(idx);
                let is_close_hovered = self.hovered_close == Some(idx);

                let fill_col = d2d_rgba(if is_hovered { pal.hover } else { pal.elevated }, 1.0);
                let stroke_col = if is_hovered {
                    d2d_rgba(pal.selected_stroke, 1.0)
                } else {
                    d2d_rgba(pal.border_rgb, if self.is_dark { 0.65 } else { 0.9 })
                };
                let card_rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: card.card_rect.left as f32 + 0.5,
                        top: card.card_rect.top as f32 + 0.5,
                        right: card.card_rect.right as f32 - 0.5,
                        bottom: card.card_rect.bottom as f32 - 0.5,
                    },
                    radiusX: corner,
                    radiusY: corner,
                };
                if let Ok(bg_b) = unsafe { d2d.target.CreateSolidColorBrush(&fill_col, None) } {
                    unsafe { d2d.target.FillRoundedRectangle(&card_rr, &bg_b) };
                }
                if let Ok(bd_b) = unsafe { d2d.target.CreateSolidColorBrush(&stroke_col, None) } {
                    unsafe { d2d.target.DrawRoundedRectangle(&card_rr, &bd_b, 1.0, None) };
                }

                // Window title.
                if !card.item.title.is_empty()
                    && let Some(ref fmt) = title_fmt
                    && let Ok(fg_b) = unsafe {
                        d2d.target
                            .CreateSolidColorBrush(&d2d_rgba(pal.text_primary, 1.0), None)
                    }
                {
                    let wtxt: Vec<u16> = card.item.title.encode_utf16().collect();
                    let title_rc = D2D_RECT_F {
                        left: card.card_rect.left as f32 + inner_pad,
                        top: card.card_rect.top as f32,
                        right: card.close_rect.left as f32 - 4.0 * dpi,
                        bottom: card.card_rect.top as f32 + header_h,
                    };
                    unsafe {
                        d2d.target.DrawText(
                            &wtxt,
                            fmt,
                            &title_rc,
                            &fg_b,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                }

                // Anti-aliased close button (semantic `AppIcon::Close` glyph).
                if is_hovered || is_close_hovered {
                    let btn_fill = if is_close_hovered {
                        d2d_rgba(pal.danger, 1.0)
                    } else {
                        d2d_rgba(pal.elevated, 0.9)
                    };
                    let btn_stroke = if is_close_hovered {
                        d2d_rgba(pal.danger, 1.0)
                    } else {
                        d2d_rgba(pal.border_rgb, 0.85)
                    };
                    let btn_r = 6.0 * dpi;
                    let btn_rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: card.close_rect.left as f32 + 0.5,
                            top: card.close_rect.top as f32 + 0.5,
                            right: card.close_rect.right as f32 - 0.5,
                            bottom: card.close_rect.bottom as f32 - 0.5,
                        },
                        radiusX: btn_r,
                        radiusY: btn_r,
                    };
                    if let Ok(bb) = unsafe { d2d.target.CreateSolidColorBrush(&btn_fill, None) } {
                        unsafe { d2d.target.FillRoundedRectangle(&btn_rr, &bb) };
                    }
                    if let Ok(bp) = unsafe { d2d.target.CreateSolidColorBrush(&btn_stroke, None) } {
                        unsafe { d2d.target.DrawRoundedRectangle(&btn_rr, &bp, 1.0, None) };
                    }
                    let x_col = if is_close_hovered {
                        d2d_rgba((0xff, 0xff, 0xff), 1.0)
                    } else {
                        d2d_rgba(pal.text_primary, 1.0)
                    };
                    if let Some(ref fmt) = close_fmt
                        && let Ok(xb) = unsafe { d2d.target.CreateSolidColorBrush(&x_col, None) }
                    {
                        let glyph: Vec<u16> =
                            design::icons::glyph_for_icon(design::icons::AppIcon::Close)
                                .encode_utf16()
                                .collect();
                        let xrc = D2D_RECT_F {
                            left: card.close_rect.left as f32,
                            top: card.close_rect.top as f32,
                            right: card.close_rect.right as f32,
                            bottom: card.close_rect.bottom as f32,
                        };
                        unsafe {
                            d2d.target.DrawText(
                                &glyph,
                                fmt,
                                &xrc,
                                &xb,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                    }
                }
            }

            if unsafe { d2d.target.EndDraw(None, None) }.is_err() {
                *guard = None;
            }
        });

        if let Some(ref mut fg) = self.dcomp_fg
            && !bits.is_null()
        {
            let byte_len = (cw as usize).saturating_mul(ch as usize).saturating_mul(4);
            let pixels = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), byte_len) };
            let _ = fg.upload_bgra(cw as u32, ch as u32, pixels);
        } else {
            unsafe {
                let _ = BitBlt(screen_hdc, 0, 0, cw, ch, mem_dc, 0, 0, SRCCOPY);
            }
        }

        unsafe {
            SelectObject(mem_dc, old_bmp);
            DeleteObject(mem_bmp as _);
            DeleteDC(mem_dc);
            EndPaint(self.hwnd, &ps);
        }
    }

    fn handle_mouse_move(&mut self, x: i32, y: i32) {
        self.pointer_inside = true;
        if let Some(ref mut glass) = self.spatial_glass {
            glass.set_pointer(x as f32, y as f32);
        }
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

    /// Closes a preview card and rebuilds the popup for the remaining windows.
    /// Shared by the `X` button and middle-click so neither leaves a dead card
    /// (and its DWM thumbnail) on screen.
    fn close_card_and_reflow(&mut self, idx: usize) {
        let target_hwnd = self.cards[idx].item.hwnd;
        tracker::close_window(target_hwnd);
        self.hovered_card = None;
        self.hovered_close = None;
        if self.cards.len() <= 1 {
            self.hide();
            return;
        }
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
        self.show(&key, &remaining, center_x, bottom_y, self.is_dark);
    }

    fn handle_lbutton_up(&mut self, x: i32, y: i32) {
        let Some(idx) = self
            .cards
            .iter()
            .position(|c| pt_in_rect(&c.card_rect, x, y))
        else {
            return;
        };
        if pt_in_rect(&self.cards[idx].close_rect, x, y) {
            self.close_card_and_reflow(idx);
        } else {
            let target_hwnd = self.cards[idx].item.hwnd;
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
        self.close_card_and_reflow(idx);
    }
}

impl Drop for PreviewPopup {
    fn drop(&mut self) {
        self.clear_thumbnails();
        self.dcomp_fg = None;
        self.spatial_glass = None;
        if !self.hwnd.is_null() {
            unsafe {
                set_window_long_ptr(self.hwnd, GWLP_USERDATA, 0);
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
        WM_ERASEBKGND => 1,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_window_stays_inside_screen() {
        assert_eq!(
            position_overlay_window(400, 200, 500, 300, (1920, 1080), 8),
            (300, 100)
        );
        assert_eq!(
            position_overlay_window(400, 200, -100, -50, (1920, 1080), 8),
            (8, 8)
        );
        assert_eq!(
            position_overlay_window(400, 200, 5000, 5000, (1920, 1080), 8),
            (1512, 872)
        );
    }

    #[test]
    fn six_cards_fit_a_1080p_work_area_at_150_percent() {
        let dpi = 1.5_f32;
        let px = |v: f32| (v * dpi).round() as i32;
        let (pad, gap, margin) = (px(OUTER_PAD_DIPS), px(CARD_GAP_DIPS), px(8.0));
        let desired = (px(CARD_W_DIPS), px(CARD_H_DIPS));
        let work_w = 1920;
        let (w, h) = fit_card_size(desired, 6, work_w, pad, gap, margin, px(78.0));
        let total_w = pad * 2 + w * 6 + gap * 5;
        assert!(total_w <= work_w - 2 * margin, "{total_w}");
        assert!(w < desired.0 && h < desired.1, "{w}x{h}");
        // Aspect is preserved (within integer rounding).
        let aspect = |(a, b): (i32, i32)| a as f32 / b as f32;
        assert!((aspect((w, h)) - aspect(desired)).abs() < 0.02);
        // The close button (top-right, 22 DIP + 6 DIP inset) still fits on each card.
        assert!(w > px(CLOSE_BTN_DIPS) + px(6.0) * 2);

        // When they already fit, the preferred size is kept unchanged.
        assert_eq!(
            fit_card_size(desired, 2, work_w, pad, gap, margin, px(78.0)),
            desired
        );
    }

    #[test]
    fn peek_timing_matches_reference() {
        assert_eq!(super::super::layout::PREVIEW_GRACE_MS, 250);
        assert_eq!(super::super::layout::PREVIEW_DELAY_MS, 500);
        assert_eq!(super::super::layout::PREVIEW_FADE_MS, 140);
    }
}
