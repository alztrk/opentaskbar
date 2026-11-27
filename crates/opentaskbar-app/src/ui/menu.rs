//! Native Windows 11 Acrylic + Direct2D/DirectWrite popup context menu with cascading hover submenus.
//!
//! Uses `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW` (Windows 11 Acrylic glass)
//! with hardware-accelerated Direct2D rounded hover pills, DirectWrite grayscale anti-aliasing,
//! `Segoe UI Variable Text`, and `Segoe Fluent Icons`.

use crate::platform::win32_helpers::set_window_long_ptr;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::HWND as WinHwnd;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Factory,
    ID2D1HwndRenderTarget, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::HSTRING;
use windows_numerics::Vector2;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, InvalidateRect, PAINTSTRUCT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT,
    VK_SPACE, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetClientRect, GetCursorPos,
    GetMenu, GetMenuItemCount, GetMenuItemID, GetMenuStringW, GetSystemMenu, GetWindowLongPtrW,
    HWND_TOPMOST, IDC_ARROW, IDC_HAND, LoadCursorW, MF_BYPOSITION, SW_SHOW, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SendMessageW, SetCursor, SetForegroundWindow, SetWindowPos, ShowWindow,
    WM_ACTIVATE, WM_CLOSE, WM_COMMAND, WM_ERASEBKGND, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WM_SYSCOMMAND, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use opentaskbar_core::settings::{DockSettings, SurfaceBackground, clear_crystal_tint_alpha};

use super::components::{StepMode, step_index};
use super::{ModalStep, modal_loop, register_popup_class};
use crate::design::fonts;
use crate::design::icons::{self, AppIcon};
use crate::design::spatial_glass::{
    DCompForeground, SpatialGlassConfig, SpatialGlassHost, configure_host_backdrop_window,
};
use crate::design::{self, Rgb};
use crate::platform::win32;

const WM_MOUSELEAVE: u32 = 0x02A3;
const DWMWCP_ROUND: u32 = 2;
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
const DWMSBT_TRANSIENTWINDOW: u32 = 3; // Windows 11 Acrylic backdrop
const MENU_W_DIPS: f32 = 268.0;
const MENU_MAX_W_DIPS: f32 = 360.0;
const MENU_WIDTH_HEADROOM_DIPS: f32 = 8.0;
const SUBMENU_W_DIPS: f32 = 232.0;
const MENU_LABEL_SHORTCUT_GAP_DIPS: f32 = 24.0;
const DOCK_POPUP_GAP_DIPS: f32 = 8.0;
const ROW_H_DIPS: f32 = 32.0;
const SEP_H_DIPS: f32 = 9.0;
const PAD_X_DIPS: f32 = 5.0;
const PAD_Y_DIPS: f32 = 5.0;
/// Time the pointer is allowed to travel from a parent row into the submenu
/// before the submenu is considered abandoned.
const SUBMENU_HOVER_GRACE_MS: u64 = 200;

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
    Bell,
    Focus,
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
        MenuIcon::Bell => AppIcon::Bell,
        MenuIcon::Focus => AppIcon::Moon,
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
    Submenu {
        icon: MenuIcon,
        label: String,
        items: Vec<MenuEntry>,
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

    pub fn submenu(icon: MenuIcon, label: impl Into<String>, items: Vec<MenuEntry>) -> Self {
        Self::Submenu {
            icon,
            label: label.into(),
            items,
        }
    }
}

/// Stroke style for popup surfaces (menus and flyouts).
///
/// `width_dips <= 0.0` disables the stroke entirely. Values are clamped to the
/// settings UI range (`0..8`) and invalid (NaN/infinite) widths fall back to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderStyle {
    pub rgb: Rgb,
    pub width_dips: f32,
    /// True only when the owning surface (`menu_background` / `widget_background`) is in Glass mode.
    pub glass_mode: bool,
    /// Clear Crystal translucent tint alpha derived from the surface opacity.
    pub glass_tint_alpha: f32,
    /// Full optical Clear Crystal configuration used by `SpatialGlassHost` in Glass mode.
    pub glass_config: SpatialGlassConfig,
}

impl BorderStyle {
    /// Palette hairline used by call sites that do not have `DockSettings`.
    pub fn default_for(is_dark: bool) -> Self {
        Self {
            rgb: design::palette(is_dark).border_rgb,
            width_dips: 1.0,
            glass_mode: true,
            glass_tint_alpha: clear_crystal_tint_alpha(0.9, is_dark),
            glass_config: SpatialGlassConfig::default_glass(is_dark, 0.9),
        }
    }

    /// Popup menus follow the `menu_border_*` and `menu_background` settings.
    pub fn from_menu_settings(is_dark: bool, settings: &DockSettings) -> Self {
        let mut style = Self::from_settings(
            is_dark,
            &settings.menu_border_color,
            settings.menu_border_thickness,
        );
        style.glass_mode = settings.menu_background == SurfaceBackground::Glass;
        style.glass_tint_alpha = clear_crystal_tint_alpha(settings.menu_opacity, is_dark);
        style.glass_config = SpatialGlassConfig::from_settings(
            settings,
            settings.menu_background,
            settings.menu_opacity,
            is_dark,
            false,
        );
        style
    }

    /// Widget flyouts follow the `widget_border_*` and `widget_background` settings.
    pub fn from_widget_settings(is_dark: bool, settings: &DockSettings) -> Self {
        let mut style = Self::from_settings(
            is_dark,
            &settings.widget_border_color,
            settings.widget_border_thickness,
        );
        style.glass_mode = settings.widget_background == SurfaceBackground::Glass;
        style.glass_tint_alpha = clear_crystal_tint_alpha(settings.widget_opacity, is_dark);
        style.glass_config = SpatialGlassConfig::from_settings(
            settings,
            settings.widget_background,
            settings.widget_opacity,
            is_dark,
            false,
        );
        style
    }

    fn from_settings(is_dark: bool, color: &Option<String>, thickness: f64) -> Self {
        let rgb = color
            .as_deref()
            .and_then(design::parse_hex_rgb)
            .unwrap_or_else(|| design::palette(is_dark).border_rgb);
        let width_dips = if thickness.is_finite() {
            thickness.clamp(0.0, 8.0) as f32
        } else {
            1.0
        };
        Self {
            rgb,
            width_dips,
            glass_mode: true,
            glass_tint_alpha: clear_crystal_tint_alpha(0.9, is_dark),
            glass_config: SpatialGlassConfig::default_glass(is_dark, 0.9),
        }
    }
}

/// Public alias matching the anchored popup menu API contract.
pub type MenuBorderStyle = BorderStyle;

/// Screen edge a dock or bar is anchored to, used to place popup menus cleanly
/// outside the visible pill without guessing from raw HWND dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuEdge {
    Bottom,
    Top,
    Left,
    Right,
}

/// Explicit physical screen-pixel anchor for popup menus opened from a dock or top bar.
#[derive(Clone, Copy, Debug)]
pub struct MenuAnchor {
    /// Visible dock/bar pill rect in physical screen pixels (excluding transparent headroom).
    pub visible_rect: windows::Win32::Foundation::RECT,
    /// Known dock edge, or `None` for cursor-anchored popups (e.g., flyouts/trays).
    pub edge: Option<MenuEdge>,
}

/// High bit marks a command as `WM_SYSCOMMAND` (target application's system menu).
/// Consumed by `dock::tray` and the window picker through the public APIs below.
pub const NATIVE_SYS_COMMAND_FLAG: u32 = 0x8000_0000;

fn encode_native_command(id: u32, system: bool) -> u32 {
    if system {
        id | NATIVE_SYS_COMMAND_FLAG
    } else {
        id & !NATIVE_SYS_COMMAND_FLAG
    }
}

/// Reads the target window's own `GetMenu` / `GetSystemMenu` commands (reference
/// `ApplicationWindowMenu.cs`).
///
/// Public entry point used by `dock::tray`, the window picker and their tests.
pub fn native_app_menu_entries(target_hwnd: usize) -> Vec<MenuEntry> {
    if target_hwnd == 0 {
        return Vec::new();
    }
    let hwnd = target_hwnd as HWND;
    let mut out = Vec::new();
    // SAFETY: `hwnd` is a caller-supplied live window; menu handles belong to it.
    unsafe {
        for (menu, system) in [(GetMenu(hwnd), false), (GetSystemMenu(hwnd, 0), true)] {
            if menu.is_null() {
                continue;
            }
            let count = GetMenuItemCount(menu).max(0) as usize;
            for pos in 0..count.min(64) {
                let id = GetMenuItemID(menu, pos as i32);
                if id == u32::MAX {
                    // Separator or submenu entry: expose only separators for now.
                    let mut probe = [0u16; 2];
                    let len =
                        GetMenuStringW(menu, pos as u32, probe.as_mut_ptr(), 2, MF_BYPOSITION);
                    if len == 0 {
                        out.push(MenuEntry::Separator);
                    }
                    continue;
                }
                let mut label = vec![0u16; 128];
                let len = GetMenuStringW(
                    menu,
                    pos as u32,
                    label.as_mut_ptr(),
                    label.len() as i32,
                    MF_BYPOSITION,
                );
                if len <= 0 {
                    continue;
                }
                let text = String::from_utf16_lossy(&label[..len as usize])
                    .replace('&', "")
                    .trim()
                    .to_owned();
                if text.is_empty() {
                    continue;
                }
                out.push(MenuEntry::item(
                    encode_native_command(id, system),
                    MenuIcon::Terminal,
                    text,
                ));
            }
        }
    }
    out
}

/// Executes a native menu command on the target window via `WM_COMMAND` / `WM_SYSCOMMAND`.
/// Used by the same dock/window-picker call sites as [`native_app_menu_entries`].
pub fn invoke_native_app_command(target_hwnd: usize, command_id: u32) -> bool {
    if target_hwnd == 0 {
        return false;
    }
    let system = command_id & NATIVE_SYS_COMMAND_FLAG != 0;
    let command = command_id & !NATIVE_SYS_COMMAND_FLAG;
    // SAFETY: target window is owned by the caller; both messages are standard Win32 commands.
    unsafe {
        let result = if system {
            SendMessageW(target_hwnd as HWND, WM_SYSCOMMAND, command as usize, 0)
        } else {
            SendMessageW(target_hwnd as HWND, WM_COMMAND, command as usize, 0)
        };
        result == 0
    }
}

struct MenuTextFormats {
    dwrite: IDWriteFactory,
    text_fmt: IDWriteTextFormat,
    hint_fmt: IDWriteTextFormat,
    icon_fmt: IDWriteTextFormat,
    chevron_fmt: IDWriteTextFormat,
}

struct MenuD2d {
    rt: ID2D1HwndRenderTarget,
    formats: MenuTextFormats,
}

struct MenuPane {
    hwnd: HWND,
    dpi: f32,
    is_dark: bool,
    acrylic: bool,
    screen_rect: RECT,
    entries: Vec<MenuEntry>,
    row_rects: Vec<Option<RECT>>,
    hovered: Option<usize>,
    /// Keyboard cursor over selectable rows; independent of hover so mouse and
    /// keyboard highlighting can coexist without fighting each other.
    keyboard_index: Option<usize>,
    open_submenu_idx: Option<usize>,
    tracking: bool,
    d2d: Option<MenuD2d>,
    /// Clear Crystal optical glass host (`topmost = false`) + DComp foreground (`topmost = true`),
    /// active strictly when `border.glass_config.enabled` is true.
    spatial_glass: Option<SpatialGlassHost>,
    dcomp_fg: Option<DCompForeground>,
    glass_formats: Option<MenuTextFormats>,
    is_submenu: bool,
    border: BorderStyle,
    session: *mut MenuSession,
}

struct MenuSession {
    hinstance: windows_sys::Win32::Foundation::HINSTANCE,
    class_name: Vec<u16>,
    dpi: f32,
    is_dark: bool,
    border: BorderStyle,
    root: *mut MenuPane,
    submenu: Option<Box<MenuPane>>,
    /// Monotonic open time, used for the parent-to-submenu travel grace window.
    submenu_opened_at: Option<Instant>,
    /// Cached parent geometry (screen coords) so hover checks never need to
    /// hold a borrow of `root` while closing the submenu.
    submenu_parent_rect: Option<RECT>,
    submenu_parent_row: Option<RECT>,
    selected_id: Option<u32>,
    done: AtomicBool,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn pt_in_rect(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Translates a pane-local row rect into screen coordinates using the pane's
/// own stored rect — no `GetWindowRect`/`WindowFromPoint` round trip needed.
fn screen_row_rect(pane_rect: &RECT, row_rect: &RECT) -> RECT {
    RECT {
        left: pane_rect.left + row_rect.left,
        top: pane_rect.top + row_rect.top,
        right: pane_rect.left + row_rect.right,
        bottom: pane_rect.top + row_rect.bottom,
    }
}

/// Pure lifetime policy for a hover submenu. Returns `true` when the submenu
/// must be closed for the current pointer position.
///
/// The pointer is kept alive while it is over the parent submenu row or over
/// the submenu pane itself. While the short grace window is still running,
/// the whole parent pane is also considered "in transit" — this covers the
/// padding strip between the row rect and the submenu's left edge, where the
/// parent receives `WM_MOUSEMOVE` with no hovered row before the pointer ever
/// reaches the submenu HWND. Outside those rects the submenu closes promptly.
fn should_close_submenu(
    submenu_open: bool,
    elapsed_ms_since_open: Option<u64>,
    pointer_screen: Option<(i32, i32)>,
    parent_rect: Option<RECT>,
    parent_row_rect: Option<RECT>,
    submenu_rect: Option<RECT>,
    grace_ms: u64,
) -> bool {
    if !submenu_open {
        return false;
    }
    let Some((x, y)) = pointer_screen else {
        // Pointer position is unavailable (e.g. `GetCursorPos` failed): do not
        // keep an orphaned submenu alive without any hit-test information.
        return true;
    };
    if submenu_rect.is_some_and(|r| pt_in_rect(&r, x, y)) {
        return false;
    }
    if parent_row_rect.is_some_and(|r| pt_in_rect(&r, x, y)) {
        return false;
    }
    if elapsed_ms_since_open.is_some_and(|ms| ms <= grace_ms)
        && parent_rect.is_some_and(|r| pt_in_rect(&r, x, y))
    {
        return false;
    }
    true
}

/// Row indices that can receive keyboard selection. Separators are skipped so
/// arrow keys never park the highlight on dead space.
#[must_use]
fn selectable_indices(entries: &[MenuEntry]) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            MenuEntry::Separator => None,
            MenuEntry::Item { .. } | MenuEntry::Submenu { .. } => Some(index),
        })
        .collect()
}

/// Moves the keyboard cursor over selectable rows, wrapping at both ends.
/// `None` starts at the first row going down, or the last row going up.
#[must_use]
fn keyboard_step(entries: &[MenuEntry], current: Option<usize>, delta: isize) -> Option<usize> {
    let rows = selectable_indices(entries);
    if rows.is_empty() {
        return None;
    }
    let len = rows.len();
    let Some(current) = current else {
        // No cursor yet: down starts at the first row, up at the last one.
        return Some(if delta < 0 { rows[len - 1] } else { rows[0] });
    };
    let Some(position) = rows.iter().position(|&row| row == current) else {
        // Stale cursor on a separator: resume at the nearest selectable row in
        // the requested direction (no second step is applied).
        let insertion = rows.partition_point(|&row| row <= current);
        return Some(if delta < 0 {
            rows[insertion.saturating_sub(1)]
        } else {
            rows[insertion.min(len - 1)]
        });
    };
    Some(rows[step_index(position, len, delta, StepMode::Wrap)])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuKeyAction {
    Escape,
    Up,
    Down,
    Left,
    Right,
    Confirm,
    Other,
}

/// Pure `WM_KEYDOWN` classification (Enter **and** Space confirm, arrows navigate).
#[must_use]
fn menu_key_action(msg: u32, wparam: WPARAM) -> MenuKeyAction {
    if msg != WM_KEYDOWN {
        return MenuKeyAction::Other;
    }
    match wparam as u16 {
        VK_ESCAPE => MenuKeyAction::Escape,
        VK_UP => MenuKeyAction::Up,
        VK_DOWN => MenuKeyAction::Down,
        VK_LEFT => MenuKeyAction::Left,
        VK_RIGHT => MenuKeyAction::Right,
        VK_RETURN | VK_SPACE => MenuKeyAction::Confirm,
        _ => MenuKeyAction::Other,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuCloseAction {
    None,
    EndSession,
}

/// `WM_CLOSE` must never reach `DefWindowProcW` while the modal loop still owns
/// pane pointers; it ends the session so cleanup destroys every pane exactly once.
#[must_use]
fn menu_close_action(msg: u32) -> MenuCloseAction {
    if msg == WM_CLOSE {
        MenuCloseAction::EndSession
    } else {
        MenuCloseAction::None
    }
}

fn rgba_f(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a,
    }
}

fn create_menu_formats() -> Option<MenuTextFormats> {
    unsafe {
        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
        // Single verified installed family name for DirectWrite (never a CSS comma list).
        let ui_font = HSTRING::from(fonts::dwrite_ui_family().as_str());
        let icon_font = HSTRING::from("Segoe Fluent Icons");
        let locale = HSTRING::from(fonts::ui_locale().as_str());

        let text_fmt = dwrite
            .CreateTextFormat(
                &ui_font,
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                fonts::scaled_size(13.0),
                &locale,
            )
            .ok()?;
        let _ = text_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = text_fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = text_fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        let trim = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        if let Ok(ellipsis) = dwrite.CreateEllipsisTrimmingSign(&text_fmt) {
            let _ = text_fmt.SetTrimming(&trim, &ellipsis);
        }

        let hint_fmt = dwrite
            .CreateTextFormat(
                &ui_font,
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                fonts::scaled_size(11.0),
                &locale,
            )
            .ok()?;
        let _ = hint_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
        let _ = hint_fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = hint_fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);

        let icon_fmt = dwrite
            .CreateTextFormat(
                &icon_font,
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                fonts::scaled_size(15.0),
                &locale,
            )
            .ok()?;
        let _ = icon_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = icon_fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        let chevron_fmt = dwrite
            .CreateTextFormat(
                &icon_font,
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                fonts::scaled_size(10.0),
                &locale,
            )
            .ok()?;
        let _ = chevron_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
        let _ = chevron_fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        Some(MenuTextFormats {
            dwrite,
            text_fmt,
            hint_fmt,
            icon_fmt,
            chevron_fmt,
        })
    }
}

fn create_menu_d2d(
    hwnd: HWND,
    width_px: u32,
    height_px: u32,
    dpi: f32,
    formats: Option<MenuTextFormats>,
) -> Option<MenuD2d> {
    let formats = formats.or_else(create_menu_formats)?;
    let dpi = dpi.max(0.5);
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

        Some(MenuD2d { rt, formats })
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

fn measure_text_width_dwrite(
    dwrite: &IDWriteFactory,
    fmt: &IDWriteTextFormat,
    text: &str,
) -> Option<f32> {
    if text.is_empty() {
        return Some(0.0);
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    let layout = unsafe { dwrite.CreateTextLayout(&wide, fmt, 4096.0, 64.0).ok()? };
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe {
        layout.GetMetrics(&mut metrics).ok()?;
    }
    Some(metrics.widthIncludingTrailingWhitespace.ceil())
}

fn estimate_text_width_dips(text: &str, font_size_dips: f32) -> f32 {
    let mut units = 0.0_f32;
    for ch in text.chars() {
        units += match ch {
            ' ' | 'i' | 'l' | 'I' | 'ı' | '.' | ',' | ':' | ';' | '!' | '|' | '\'' => 0.34,
            'f' | 'j' | 'r' | 't' | '(' | ')' | '[' | ']' | '-' | '/' => 0.42,
            'm' | 'w' | 'M' | 'W' => 0.84,
            c if c.is_uppercase() => 0.68,
            c if c as u32 > 0x2E80 => 1.0,
            _ => 0.56,
        };
    }
    (units * font_size_dips).ceil()
}

fn menu_max_width_dips() -> f32 {
    fonts::scaled_size(MENU_MAX_W_DIPS).max(MENU_W_DIPS)
}

fn compute_menu_width_dips_with(
    entries: &[MenuEntry],
    min_w: f32,
    max_w: f32,
    mut measure_label: impl FnMut(&str) -> f32,
    mut measure_hint: impl FnMut(&str) -> f32,
) -> f32 {
    // Horizontal chrome matches `draw_menu_entries_rt`:
    // - Item: PAD_X + 36.0 left icon column, PAD_X + 10.0 right inset (56.0 DIPs),
    //   plus MENU_WIDTH_HEADROOM_DIPS (8.0 DIPs) breathing room for DirectWrite diacritics/overhang.
    // - Submenu: PAD_X + 36.0 left icon column, PAD_X + 24.0 chevron inset (70.0 DIPs),
    //   plus MENU_WIDTH_HEADROOM_DIPS.
    let item_chrome = PAD_X_DIPS * 2.0 + 36.0 + 10.0 + MENU_WIDTH_HEADROOM_DIPS;
    let submenu_chrome = PAD_X_DIPS * 2.0 + 36.0 + 24.0 + MENU_WIDTH_HEADROOM_DIPS;
    let mut needed = min_w;
    for entry in entries {
        let row_w = match entry {
            MenuEntry::Item {
                label, shortcut, ..
            } => {
                let label_w = measure_label(label);
                match shortcut.as_deref() {
                    Some(sc) if !sc.is_empty() => {
                        item_chrome + label_w + MENU_LABEL_SHORTCUT_GAP_DIPS + measure_hint(sc)
                    }
                    _ => item_chrome + label_w,
                }
            }
            MenuEntry::Submenu { label, .. } => submenu_chrome + measure_label(label),
            MenuEntry::Separator => 0.0,
        };
        needed = needed.max(row_w);
    }
    needed.clamp(min_w, max_w.max(min_w)).ceil()
}

fn compute_menu_width_dips_with_formats(
    entries: &[MenuEntry],
    min_w: f32,
    max_w: f32,
    formats: Option<&MenuTextFormats>,
) -> f32 {
    let label_size = fonts::scaled_size(13.0);
    let hint_size = fonts::scaled_size(11.0);
    if let Some(fmts) = formats {
        compute_menu_width_dips_with(
            entries,
            min_w,
            max_w,
            |text| {
                measure_text_width_dwrite(&fmts.dwrite, &fmts.text_fmt, text)
                    .map(|w| w + 4.0)
                    .unwrap_or_else(|| estimate_text_width_dips(text, label_size))
            },
            |text| {
                measure_text_width_dwrite(&fmts.dwrite, &fmts.hint_fmt, text)
                    .map(|w| w + 2.0)
                    .unwrap_or_else(|| estimate_text_width_dips(text, hint_size))
            },
        )
    } else {
        compute_menu_width_dips_with(
            entries,
            min_w,
            max_w,
            |text| estimate_text_width_dips(text, label_size),
            |text| estimate_text_width_dips(text, hint_size),
        )
    }
}

#[cfg(test)]
fn compute_menu_width_dips(entries: &[MenuEntry], min_w: f32, max_w: f32) -> f32 {
    let formats = create_menu_formats();
    compute_menu_width_dips_with_formats(entries, min_w, max_w, formats.as_ref())
}

/// Computes the popup menu top-left screen coordinates in physical pixels.
///
/// When `anchor` carries an explicit [`MenuEdge`], the popup is placed outside
/// `anchor.visible_rect` by `DOCK_POPUP_GAP_DIPS * dpi` (never using raw HWND
/// headroom) and aligned to the cursor along the perpendicular axis with smart
/// edge flipping. When `anchor` is `None` (or `edge` is `None`), the popup opens
/// directly at `cursor`, flipping left/up if needed and clamping safely within
/// the monitor work area.
fn compute_popup_origin(
    cursor: POINT,
    menu_w: i32,
    menu_h: i32,
    monitor: &win32::MonitorInfo,
    dpi: f32,
    anchor: Option<MenuAnchor>,
) -> (i32, i32) {
    let dpi = dpi.max(0.5);
    let margin = (8.0 * dpi).round() as i32;
    let gap = (DOCK_POPUP_GAP_DIPS * dpi).round() as i32;
    let work_left = monitor.left;
    let work_top = monitor.top;
    let work_right = monitor.right;
    let work_bottom = monitor.bottom;
    let min_x = work_left + margin;
    let max_x = (work_right - margin - menu_w).max(min_x);
    let min_y = work_top + margin;
    let max_y = (work_bottom - margin - menu_h).max(min_y);

    let smart_cursor_x = || {
        let raw_x = if cursor.x + menu_w > work_right - margin {
            cursor.x - menu_w
        } else {
            cursor.x
        };
        raw_x.clamp(min_x, max_x)
    };
    let smart_cursor_y = || {
        let raw_y = if cursor.y + menu_h > work_bottom - margin {
            cursor.y - menu_h
        } else {
            cursor.y
        };
        raw_y.clamp(min_y, max_y)
    };

    if let Some(MenuAnchor {
        visible_rect,
        edge: Some(edge),
    }) = anchor
    {
        return match edge {
            MenuEdge::Bottom => {
                let x = smart_cursor_x();
                let above_y = visible_rect.top - gap - menu_h;
                let raw_y = if above_y >= min_y {
                    above_y
                } else {
                    let below_y = visible_rect.bottom + gap;
                    if below_y + menu_h <= work_bottom - margin {
                        below_y
                    } else {
                        above_y
                    }
                };
                (x, raw_y.clamp(min_y, max_y))
            }
            MenuEdge::Top => {
                let x = smart_cursor_x();
                let below_y = visible_rect.bottom + gap;
                let raw_y = if below_y + menu_h <= work_bottom - margin {
                    below_y
                } else {
                    let above_y = visible_rect.top - gap - menu_h;
                    if above_y >= min_y { above_y } else { below_y }
                };
                (x, raw_y.clamp(min_y, max_y))
            }
            MenuEdge::Left => {
                let right_x = visible_rect.right + gap;
                let raw_x = if right_x + menu_w <= work_right - margin {
                    right_x
                } else {
                    let left_x = visible_rect.left - gap - menu_w;
                    if left_x >= min_x { left_x } else { right_x }
                };
                (raw_x.clamp(min_x, max_x), smart_cursor_y())
            }
            MenuEdge::Right => {
                let left_x = visible_rect.left - gap - menu_w;
                let raw_x = if left_x >= min_x {
                    left_x
                } else {
                    let right_x = visible_rect.right + gap;
                    if right_x + menu_w <= work_right - margin {
                        right_x
                    } else {
                        left_x
                    }
                };
                (raw_x.clamp(min_x, max_x), smart_cursor_y())
            }
        };
    }

    (smart_cursor_x(), smart_cursor_y())
}

/// Shared pixel-snapped menu metrics used by both `measure_entries` (hit-testing)
/// and `draw_menu_entries_rt` (rendering) so rows align 1:1 at 125%/150%/200% DPI.
fn snapped_metrics_px(dpi: f32) -> (i32, i32, i32, i32) {
    let dpi = dpi.max(0.5);
    let pad_x = (PAD_X_DIPS * dpi).round() as i32;
    let pad_y = (PAD_Y_DIPS * dpi).round() as i32;
    let row_h = (fonts::scaled_size(ROW_H_DIPS) * dpi).round() as i32;
    let sep_h = (SEP_H_DIPS * dpi).round() as i32;
    (pad_x, pad_y, row_h, sep_h)
}

fn measure_entries(
    entries: &[MenuEntry],
    width_dips: f32,
    dpi: f32,
) -> (i32, i32, Vec<Option<RECT>>) {
    let dpi = dpi.max(0.5);
    let (pad_x, pad_y, row_h, sep_h) = snapped_metrics_px(dpi);
    let menu_w = (width_dips * dpi).round() as i32;

    let mut y_cursor = pad_y;
    let mut row_rects = Vec::with_capacity(entries.len());
    for entry in entries {
        match entry {
            MenuEntry::Item { .. } | MenuEntry::Submenu { .. } => {
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
    (menu_w, menu_h, row_rects)
}

/// Snapshot of immutable session data needed to create a pane.
///
/// Passed by value so `open_submenu` never has to borrow the `MenuSession` it
/// already holds mutably while Win32 synchronously dispatches messages.
#[derive(Clone, Copy)]
struct SessionInfo {
    /// Raw pointer only: never dereferenced by `create_pane_window`, so no borrow
    /// of the (possibly mutably borrowed) `MenuSession` is created there.
    session: *mut MenuSession,
    hinstance: windows_sys::Win32::Foundation::HINSTANCE,
    class_name: *const u16,
    dpi: f32,
    is_dark: bool,
    border: BorderStyle,
}

impl MenuSession {
    fn info(&mut self) -> SessionInfo {
        SessionInfo {
            session: self as *mut MenuSession,
            hinstance: self.hinstance,
            class_name: self.class_name.as_ptr(),
            dpi: self.dpi,
            is_dark: self.is_dark,
            border: self.border,
        }
    }
}

fn create_pane_window(
    info: SessionInfo,
    entries: Vec<MenuEntry>,
    width_dips: f32,
    x: i32,
    y: i32,
    is_submenu: bool,
    formats: Option<MenuTextFormats>,
) -> Option<Box<MenuPane>> {
    let SessionInfo {
        session,
        hinstance,
        class_name: class_ptr,
        dpi,
        is_dark,
        border,
    } = info;
    let dpi = dpi.max(0.5);
    let (menu_w, menu_h, row_rects) = measure_entries(&entries, width_dips, dpi);
    let use_spatial_glass = border.glass_config.enabled;
    let base_ex_style =
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | if is_submenu { WS_EX_NOACTIVATE } else { 0 };
    let create_hwnd = |ex_style: u32| -> HWND {
        unsafe {
            CreateWindowExW(
                ex_style,
                class_ptr,
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
        }
    };
    let mut hwnd = create_hwnd(
        base_ex_style
            | if use_spatial_glass {
                WS_EX_NOREDIRECTIONBITMAP
            } else {
                0
            },
    );
    if hwnd.is_null() {
        return None;
    }

    let (acrylic, d2d, spatial_glass, dcomp_fg, glass_formats) = if use_spatial_glass {
        configure_host_backdrop_window(hwnd as usize, is_dark, true);
        // Apply Windows 11 native rounded corners (with DWM border disabled so only
        // SpatialGlassHost's crisp rim or our rounded Direct2D hairline renders)
        // to get clean OS drop shadow elevation.
        unsafe {
            let corner = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                (&corner as *const u32).cast(),
                size_of::<u32>() as u32,
            );
            let border_none = DWMWA_COLOR_NONE;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR as u32,
                (&border_none as *const u32).cast(),
                size_of::<u32>() as u32,
            );
        }
        let mut host = SpatialGlassHost::attach(hwnd as usize, false, border.glass_config);
        if let Some(ref mut g) = host {
            g.sync_rect(
                0.0,
                0.0,
                menu_w as f32,
                menu_h as f32,
                (8.0 * dpi).round(),
                dpi,
                0.0,
                0.0,
            );
        }
        let fg = DCompForeground::new(hwnd as usize);
        let fmts = formats.or_else(create_menu_formats);
        if host.is_some() && fg.is_some() && fmts.is_some() {
            (true, None, host, fg, fmts)
        } else {
            // Release any partial DComp bindings and recreate the HWND without
            // WS_EX_NOREDIRECTIONBITMAP so ID2D1HwndRenderTarget can actually present.
            drop(fg);
            drop(host);
            unsafe {
                DestroyWindow(hwnd);
            }
            hwnd = create_hwnd(base_ex_style);
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
                let border_none = DWMWA_COLOR_NONE;
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_BORDER_COLOR as u32,
                    (&border_none as *const u32).cast(),
                    size_of::<u32>() as u32,
                );
                let backdrop = DWMSBT_TRANSIENTWINDOW;
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE as u32,
                    (&backdrop as *const u32).cast(),
                    size_of::<u32>() as u32,
                ) == 0
            };
            (
                acrylic,
                create_menu_d2d(hwnd, menu_w as u32, menu_h as u32, dpi, fmts),
                None,
                None,
                None,
            )
        }
    } else {
        unsafe {
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
            let border_none = DWMWA_COLOR_NONE;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR as u32,
                (&border_none as *const u32).cast(),
                size_of::<u32>() as u32,
            );
        }
        (
            false,
            create_menu_d2d(hwnd, menu_w as u32, menu_h as u32, dpi, formats),
            None,
            None,
            None,
        )
    };

    let mut pane = Box::new(MenuPane {
        hwnd,
        dpi,
        is_dark,
        acrylic,
        screen_rect: RECT {
            left: x,
            top: y,
            right: x + menu_w,
            bottom: y + menu_h,
        },
        entries,
        row_rects,
        hovered: None,
        keyboard_index: None,
        open_submenu_idx: None,
        tracking: false,
        d2d,
        spatial_glass,
        dcomp_fg,
        glass_formats,
        is_submenu,
        border,
        session,
    });
    pane.paint_glass_surface();
    let ptr = (&mut *pane) as *mut MenuPane as isize;
    unsafe {
        set_window_long_ptr(hwnd, GWLP_USERDATA, ptr);
        let flags = SWP_SHOWWINDOW | if is_submenu { SWP_NOACTIVATE } else { 0 };
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, menu_w, menu_h, flags);
        if !is_submenu {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        }
    }
    Some(pane)
}

fn destroy_pane(pane: &mut MenuPane) {
    pane.dcomp_fg = None;
    pane.spatial_glass = None;
    pane.glass_formats = None;
    pane.d2d = None;
    unsafe {
        set_window_long_ptr(pane.hwnd, GWLP_USERDATA, 0);
        DestroyWindow(pane.hwnd);
    }
}

/// Displays a native Windows 11 Acrylic popup menu (with hover submenus) at the cursor.
///
/// Call sites without `DockSettings` get the palette hairline; use
/// [`show_popup_menu_with_border`] or [`show_popup_menu_with_anchor`] to apply
/// persisted `menu_border_*` settings and explicit dock/bar anchors.
pub fn show_popup_menu(owner_hwnd: usize, is_dark: bool, entries: Vec<MenuEntry>) -> Option<u32> {
    show_popup_menu_with_border(
        owner_hwnd,
        is_dark,
        entries,
        BorderStyle::default_for(is_dark),
    )
}

/// Same as [`show_popup_menu`] with an explicit border stroke color/width (cursor-anchored).
pub fn show_popup_menu_with_border(
    owner_hwnd: usize,
    is_dark: bool,
    entries: Vec<MenuEntry>,
    border: BorderStyle,
) -> Option<u32> {
    show_popup_menu_with_anchor(owner_hwnd, is_dark, &entries, border, None)
}

/// Displays a popup menu with an explicit border style and optional dock/bar anchor.
pub fn show_popup_menu_with_anchor(
    owner_hwnd: usize,
    is_dark: bool,
    entries: &[MenuEntry],
    border: BorderStyle,
    anchor: Option<MenuAnchor>,
) -> Option<u32> {
    if entries.is_empty() {
        return None;
    }
    let class_name = wide("OpenTaskBarContextMenu");
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    register_popup_class("OpenTaskBarContextMenu", menu_wnd_proc, hinstance, 0);

    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut pt);
    }
    let monitor = win32::monitor_for_point_px(pt.x, pt.y);
    let dpi = if monitor.dpi_scale.is_finite() && monitor.dpi_scale > 0.0 {
        monitor.dpi_scale
    } else {
        win32::dpi_scale(owner_hwnd)
    }
    .max(0.5);

    let formats = create_menu_formats();
    let menu_width_dips = compute_menu_width_dips_with_formats(
        entries,
        MENU_W_DIPS,
        menu_max_width_dips(),
        formats.as_ref(),
    );
    let (menu_w, menu_h, _) = measure_entries(entries, menu_width_dips, dpi);
    let (x, y) = compute_popup_origin(pt, menu_w, menu_h, &monitor, dpi, anchor);

    let mut session = Box::new(MenuSession {
        hinstance,
        class_name,
        dpi,
        is_dark,
        border,
        root: std::ptr::null_mut(),
        submenu: None,
        submenu_opened_at: None,
        submenu_parent_rect: None,
        submenu_parent_row: None,
        selected_id: None,
        done: AtomicBool::new(false),
    });
    let session_info = session.info();
    let mut root = create_pane_window(
        session_info,
        entries.to_vec(),
        menu_width_dips,
        x,
        y,
        false,
        formats,
    )?;
    session.root = (&mut *root) as *mut MenuPane;

    modal_loop(|_| {
        if session.done.load(Ordering::Acquire) {
            ModalStep::Break
        } else {
            ModalStep::Continue
        }
    });

    let selected = session.selected_id;
    if let Some(mut sub) = session.submenu.take() {
        destroy_pane(&mut sub);
    }
    destroy_pane(&mut root);
    selected
}

impl MenuSession {
    fn close_submenu(&mut self) {
        if let Some(mut sub) = self.submenu.take() {
            destroy_pane(&mut sub);
        }
        self.submenu_opened_at = None;
        self.submenu_parent_rect = None;
        self.submenu_parent_row = None;
        if !self.root.is_null() {
            let root = unsafe { &mut *self.root };
            if root.open_submenu_idx.take().is_some() {
                unsafe {
                    InvalidateRect(root.hwnd, std::ptr::null(), 0);
                }
            }
        }
    }

    fn open_submenu(&mut self, parent_idx: usize, items: Vec<MenuEntry>, row_rect: RECT) {
        if self.root.is_null() || items.is_empty() {
            return;
        }
        // Snapshot before borrowing the root pane: `create_pane_window` can
        // synchronously dispatch messages, so it must not borrow `self`.
        let info = self.info();
        let root = unsafe { &mut *self.root };
        if root.open_submenu_idx == Some(parent_idx) && self.submenu.is_some() {
            return;
        }
        if let Some(mut old) = self.submenu.take() {
            destroy_pane(&mut old);
        }
        let sub_formats = create_menu_formats();
        let sub_width_dips = compute_menu_width_dips_with_formats(
            &items,
            SUBMENU_W_DIPS,
            menu_max_width_dips(),
            sub_formats.as_ref(),
        );
        let dpi = self.dpi.max(0.5);
        let (sub_w, sub_h, _) = measure_entries(&items, sub_width_dips, dpi);
        let monitor = win32::monitor_for_point_px(root.screen_rect.left, root.screen_rect.top);
        let margin = (8.0 * dpi).round() as i32;
        let pad_y = (PAD_Y_DIPS * dpi).round() as i32;
        let min_x = monitor.left + margin;
        let max_x = (monitor.right - margin - sub_w).max(min_x);
        let min_y = monitor.top + margin;
        let max_y = (monitor.bottom - margin - sub_h).max(min_y);

        // Open to the right of the parent menu; flip to the left if near the display edge.
        let right_x = root.screen_rect.right - (2.0 * dpi).round() as i32;
        let left_x = root.screen_rect.left - sub_w + (2.0 * dpi).round() as i32;
        let raw_sub_x = if right_x + sub_w <= monitor.right - margin {
            right_x
        } else {
            left_x
        };
        let sub_x = raw_sub_x.clamp(min_x, max_x);
        let ideal_y = root.screen_rect.top + row_rect.top - pad_y;
        let sub_y = ideal_y.clamp(min_y, max_y);

        if let Some(sub_pane) =
            create_pane_window(info, items, sub_width_dips, sub_x, sub_y, true, sub_formats)
        {
            self.submenu = Some(sub_pane);
            root.open_submenu_idx = Some(parent_idx);
            self.submenu_opened_at = Some(Instant::now());
            self.submenu_parent_rect = Some(root.screen_rect);
            self.submenu_parent_row = row_rects_row(root, parent_idx);
            unsafe {
                InvalidateRect(root.hwnd, std::ptr::null(), 0);
            }
        }
    }

    /// Re-evaluates the hovered submenu against the current pointer position.
    ///
    /// Only owned geometry is read from the panes: `root` aliases the pane
    /// pointer used by `WndProc`, so no `&MenuPane` may be alive while
    /// `close_submenu` takes `&mut MenuSession` and mutates the root pane.
    fn refresh_submenu_hover(&mut self, pointer_screen: Option<(i32, i32)>) {
        if self.submenu.is_none() {
            return;
        }
        let elapsed_ms = self
            .submenu_opened_at
            .map(|opened| opened.elapsed().as_millis().min(u64::MAX as u128) as u64);
        let submenu_rect = self.submenu.as_ref().map(|sub| sub.screen_rect);
        if should_close_submenu(
            true,
            elapsed_ms,
            pointer_screen,
            self.submenu_parent_rect,
            self.submenu_parent_row,
            submenu_rect,
            SUBMENU_HOVER_GRACE_MS,
        ) {
            self.close_submenu();
        }
    }
}

/// Copies the screen-space rect of a parent pane's row (if any).
fn row_rects_row(pane: &MenuPane, idx: usize) -> Option<RECT> {
    pane.row_rects
        .get(idx)
        .copied()
        .flatten()
        .map(|row| screen_row_rect(&pane.screen_rect, &row))
}

impl MenuPane {
    fn live_client_size_px(&self) -> (u32, u32) {
        let mut rc: RECT = unsafe { std::mem::zeroed() };
        if !self.hwnd.is_null() && unsafe { GetClientRect(self.hwnd, &mut rc) } != 0 {
            let w = (rc.right - rc.left).max(1) as u32;
            let h = (rc.bottom - rc.top).max(1) as u32;
            return (w, h);
        }
        let w = (self.screen_rect.right - self.screen_rect.left).max(1) as u32;
        let h = (self.screen_rect.bottom - self.screen_rect.top).max(1) as u32;
        (w, h)
    }

    fn paint_glass_surface(&mut self) {
        let Some(ref fmts) = self.glass_formats else {
            return;
        };
        let (w_px, h_px) = self.live_client_size_px();
        let dpi = self.dpi.max(0.5);
        let is_dark = self.is_dark;
        let actual_width_dips = (w_px as f32) / dpi;
        let actual_height_dips = (h_px as f32) / dpi;
        let border = self.border;
        let focused = self.hovered.or(self.keyboard_index);
        let open_submenu_idx = self.open_submenu_idx;
        let entries = &self.entries;
        let row_rects = &self.row_rects;
        if let Some(ref mut fg) = self.dcomp_fg {
            let _ = fg.draw(w_px, h_px, dpi, |dc| {
                let rt: &ID2D1RenderTarget = dc;
                draw_menu_entries_rt(
                    rt,
                    fmts,
                    entries,
                    row_rects,
                    is_dark,
                    true,
                    true,
                    dpi,
                    actual_width_dips,
                    actual_height_dips,
                    border,
                    focused,
                    open_submenu_idx,
                )
            });
        }
    }

    fn paint(&mut self) {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if hdc.is_null() {
            return;
        }
        if self.dcomp_fg.is_some() {
            self.paint_glass_surface();
        } else if let Some(ref d2d) = self.d2d {
            let _ = self.paint_d2d(d2d);
        }
        unsafe {
            EndPaint(self.hwnd, &ps);
        }
    }

    fn paint_d2d(&self, d2d: &MenuD2d) -> windows::core::Result<()> {
        let rt: &ID2D1RenderTarget = &d2d.rt;
        let (w_px, h_px) = self.live_client_size_px();
        let dpi = self.dpi.max(0.5);
        let actual_width_dips = (w_px as f32) / dpi;
        let actual_height_dips = (h_px as f32) / dpi;
        unsafe {
            rt.BeginDraw();
        }
        let res = draw_menu_entries_rt(
            rt,
            &d2d.formats,
            &self.entries,
            &self.row_rects,
            self.is_dark,
            self.acrylic,
            false,
            dpi,
            actual_width_dips,
            actual_height_dips,
            self.border,
            self.hovered.or(self.keyboard_index),
            self.open_submenu_idx,
        );
        unsafe {
            let end_res = rt.EndDraw(None, None);
            res.and(end_res)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_menu_entries_rt(
    rt: &ID2D1RenderTarget,
    fmts: &MenuTextFormats,
    entries: &[MenuEntry],
    row_rects: &[Option<RECT>],
    is_dark: bool,
    acrylic: bool,
    transparent_glass_bg: bool,
    dpi: f32,
    width_dips: f32,
    height_dips: f32,
    border: BorderStyle,
    focused: Option<usize>,
    open_submenu_idx: Option<usize>,
) -> windows::core::Result<()> {
    let (
        clear_col,
        hover_col,
        danger_hover_col,
        danger_hover_border_col,
        text_col,
        icon_col,
        hint_col,
        danger_col,
        danger_idle_col,
        sep_col,
    ) = if is_dark {
        (
            if transparent_glass_bg {
                rgba_f(0, 0, 0, 0.0)
            } else if acrylic {
                rgba_f(0x10, 0x12, 0x18, border.glass_tint_alpha.clamp(0.14, 0.45))
            } else {
                rgba_f(0x1a, 0x1a, 0x1e, 0.98)
            },
            rgba_f(
                0xff,
                0xff,
                0xff,
                if transparent_glass_bg { 0.13 } else { 0.085 },
            ),
            rgba_f(0xef, 0x44, 0x44, 0.26),
            rgba_f(0xf8, 0x71, 0x71, 0.35),
            rgba_f(0xf8, 0xf8, 0xfa, 0.99),
            rgba_f(0xdc, 0xdc, 0xe2, 0.92),
            rgba_f(0xb4, 0xb4, 0xbe, 0.88),
            rgba_f(0xfc, 0xa5, 0xa5, 1.0),
            rgba_f(0xf8, 0x71, 0x71, 0.86),
            rgba_f(0xff, 0xff, 0xff, 0.12),
        )
    } else {
        (
            if transparent_glass_bg {
                rgba_f(0, 0, 0, 0.0)
            } else if acrylic {
                rgba_f(0xf6, 0xf8, 0xfc, border.glass_tint_alpha.clamp(0.18, 0.50))
            } else {
                rgba_f(0xff, 0xff, 0xff, 0.98)
            },
            rgba_f(
                0x00,
                0x00,
                0x00,
                if transparent_glass_bg { 0.08 } else { 0.055 },
            ),
            rgba_f(0xdc, 0x26, 0x26, 0.18),
            rgba_f(0xdc, 0x26, 0x26, 0.28),
            rgba_f(0x14, 0x14, 0x18, 0.98),
            rgba_f(0x36, 0x36, 0x3e, 0.92),
            rgba_f(0x5c, 0x5c, 0x66, 0.88),
            rgba_f(0xb9, 0x1c, 0x1c, 1.0),
            rgba_f(0xc8, 0x1e, 0x1e, 0.86),
            rgba_f(0x00, 0x00, 0x00, 0.10),
        )
    };

    let dpi = dpi.max(0.5);
    let inv_dpi = 1.0 / dpi;
    let (pad_x_px, pad_y_px, row_h_px, sep_h_px) = snapped_metrics_px(dpi);
    let pad_x_dips = (pad_x_px as f32) * inv_dpi;
    let row_h_dips = (row_h_px as f32) * inv_dpi;

    unsafe {
        if !transparent_glass_bg {
            rt.Clear(Some(&clear_col));
        }

        let hover_br = rt.CreateSolidColorBrush(&hover_col, None)?;
        let danger_hover_br = rt.CreateSolidColorBrush(&danger_hover_col, None)?;
        let danger_hover_border_br = rt.CreateSolidColorBrush(&danger_hover_border_col, None)?;
        let text_br = rt.CreateSolidColorBrush(&text_col, None)?;
        let icon_br = rt.CreateSolidColorBrush(&icon_col, None)?;
        let hint_br = rt.CreateSolidColorBrush(&hint_col, None)?;
        let danger_br = rt.CreateSolidColorBrush(&danger_col, None)?;
        let danger_idle_br = rt.CreateSolidColorBrush(&danger_idle_col, None)?;
        let sep_br = rt.CreateSolidColorBrush(&sep_col, None)?;

        let mut y_px = pad_y_px;
        for (idx, entry) in entries.iter().enumerate() {
            match entry {
                MenuEntry::Item {
                    icon,
                    label,
                    shortcut,
                    destructive,
                    ..
                } => {
                    let (left, top, right, bottom) =
                        if let Some(rc) = row_rects.get(idx).copied().flatten() {
                            y_px = rc.bottom;
                            let l = (rc.left as f32) * inv_dpi;
                            let r = (width_dips - pad_x_dips).max(l);
                            (
                                l,
                                (rc.top as f32) * inv_dpi,
                                r,
                                (rc.bottom as f32) * inv_dpi,
                            )
                        } else {
                            let t = (y_px as f32) * inv_dpi;
                            y_px += row_h_px;
                            let r = (width_dips - pad_x_dips).max(pad_x_dips);
                            (pad_x_dips, t, r, t + row_h_dips)
                        };
                    let hovered = focused == Some(idx);
                    if hovered {
                        let rr = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left,
                                top,
                                right,
                                bottom,
                            },
                            radiusX: 7.0,
                            radiusY: 7.0,
                        };
                        rt.FillRoundedRectangle(
                            &rr,
                            if *destructive {
                                &danger_hover_br
                            } else {
                                &hover_br
                            },
                        );
                        if *destructive {
                            rt.DrawRoundedRectangle(&rr, &danger_hover_border_br, 1.0, None);
                        }
                    }

                    let active_danger = *destructive && hovered;
                    let glyph_w: Vec<u16> = icons::fluent_glyph(to_app_icon(*icon))
                        .encode_utf16()
                        .collect();
                    rt.DrawText(
                        &glyph_w,
                        &fmts.icon_fmt,
                        &D2D_RECT_F {
                            left: left + 6.0,
                            top,
                            right: left + 30.0,
                            bottom,
                        },
                        if active_danger {
                            &danger_br
                        } else if *destructive {
                            &danger_idle_br
                        } else if hovered {
                            &text_br
                        } else {
                            &icon_br
                        },
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );

                    let label_w: Vec<u16> = label.encode_utf16().collect();
                    let text_rect = D2D_RECT_F {
                        left: left + 36.0,
                        top,
                        right: (right - 10.0).max(left + 36.0),
                        bottom,
                    };
                    let label_right = match shortcut.as_deref() {
                        Some(sc) if !sc.is_empty() => {
                            let sc_w = measure_text_width_dwrite(&fmts.dwrite, &fmts.hint_fmt, sc)
                                .unwrap_or_else(|| {
                                    estimate_text_width_dips(sc, fonts::scaled_size(11.0))
                                });
                            (text_rect.right - sc_w - MENU_LABEL_SHORTCUT_GAP_DIPS)
                                .max(text_rect.left + 20.0)
                        }
                        _ => text_rect.right,
                    };
                    let label_rect = D2D_RECT_F {
                        right: label_right.min(text_rect.right),
                        ..text_rect
                    };
                    rt.DrawText(
                        &label_w,
                        &fmts.text_fmt,
                        &label_rect,
                        if active_danger {
                            &danger_br
                        } else if *destructive {
                            &danger_idle_br
                        } else {
                            &text_br
                        },
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );

                    if let Some(sc) = shortcut {
                        let sc_w: Vec<u16> = sc.encode_utf16().collect();
                        rt.DrawText(
                            &sc_w,
                            &fmts.hint_fmt,
                            &text_rect,
                            &hint_br,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                }
                MenuEntry::Submenu { icon, label, .. } => {
                    let (left, top, right, bottom) =
                        if let Some(rc) = row_rects.get(idx).copied().flatten() {
                            y_px = rc.bottom;
                            let l = (rc.left as f32) * inv_dpi;
                            let r = (width_dips - pad_x_dips).max(l);
                            (
                                l,
                                (rc.top as f32) * inv_dpi,
                                r,
                                (rc.bottom as f32) * inv_dpi,
                            )
                        } else {
                            let t = (y_px as f32) * inv_dpi;
                            y_px += row_h_px;
                            let r = (width_dips - pad_x_dips).max(pad_x_dips);
                            (pad_x_dips, t, r, t + row_h_dips)
                        };
                    let active = focused == Some(idx) || open_submenu_idx == Some(idx);
                    if active {
                        let rr = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left,
                                top,
                                right,
                                bottom,
                            },
                            radiusX: 7.0,
                            radiusY: 7.0,
                        };
                        rt.FillRoundedRectangle(&rr, &hover_br);
                    }

                    let glyph_w: Vec<u16> = icons::fluent_glyph(to_app_icon(*icon))
                        .encode_utf16()
                        .collect();
                    rt.DrawText(
                        &glyph_w,
                        &fmts.icon_fmt,
                        &D2D_RECT_F {
                            left: left + 6.0,
                            top,
                            right: left + 30.0,
                            bottom,
                        },
                        if active { &text_br } else { &icon_br },
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );

                    let label_w: Vec<u16> = label.encode_utf16().collect();
                    let text_rect = D2D_RECT_F {
                        left: left + 36.0,
                        top,
                        right: (right - 24.0).max(left + 36.0),
                        bottom,
                    };
                    rt.DrawText(
                        &label_w,
                        &fmts.text_fmt,
                        &text_rect,
                        &text_br,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );

                    let chev_w: Vec<u16> = "\u{E76C}".encode_utf16().collect();
                    rt.DrawText(
                        &chev_w,
                        &fmts.chevron_fmt,
                        &D2D_RECT_F {
                            left: (right - 24.0).max(left),
                            top,
                            right: (right - 8.0).max(left),
                            bottom,
                        },
                        &hint_br,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                MenuEntry::Separator => {
                    let sy = ((y_px as f32 + sep_h_px as f32 * 0.5).floor() + 0.5) * inv_dpi;
                    rt.DrawLine(
                        Vector2 {
                            X: pad_x_dips + 8.0,
                            Y: sy,
                        },
                        Vector2 {
                            X: (width_dips - pad_x_dips - 8.0).max(pad_x_dips + 8.0),
                            Y: sy,
                        },
                        &sep_br,
                        inv_dpi,
                        None,
                    );
                    y_px += sep_h_px;
                }
            }
        }

        // Draw a crisp rounded outline matching Win11 `DWMWCP_ROUND` (8 DIP radius):
        // - Always in non-Glass mode (Solid/TrueBlack/fallback) when `width_dips > 0`.
        // - In Glass mode whenever `edge_glow <= 0.01` (e.g. OLED glow reduction disabled
        //   the optical glass rim) OR the user configured a border thicker than 1 DIP.
        let draw_border = border.width_dips > 0.0
            && (!transparent_glass_bg
                || border.glass_config.edge_glow <= 0.01
                || border.width_dips > 1.0);
        if draw_border {
            let stroke_alpha = if transparent_glass_bg {
                if border.width_dips > 1.0 { 0.92 } else { 0.72 }
            } else {
                1.0
            };
            let stroke_col = rgba_f(border.rgb.0, border.rgb.1, border.rgb.2, stroke_alpha);
            let stroke_br = rt.CreateSolidColorBrush(&stroke_col, None)?;
            let inset = border.width_dips * 0.5;
            let radius = (8.0 - inset).max(1.0);
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: inset,
                    top: inset,
                    right: (width_dips - inset).max(inset),
                    bottom: (height_dips - inset).max(inset),
                },
                radiusX: radius,
                radiusY: radius,
            };
            rt.DrawRoundedRectangle(&rr, &stroke_br, border.width_dips, None);
        }
        Ok(())
    }
}

thread_local! {
    /// Menu `WndProc` frames currently active on this UI thread. Synchronous
    /// Win32 calls (`SetWindowPos`, `ShowWindow`, `DestroyWindow`) can dispatch
    /// messages back into the same window; a re-entrant call must not create a
    /// second `&mut` to the same `MenuPane` / `MenuSession`.
    static MENU_WNDPROC_ACTIVE: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// HWND-based re-entrancy guard, mirroring the dock's `DockWndProcGuard`.
struct MenuWndProcGuard {
    hwnd: usize,
}

impl MenuWndProcGuard {
    fn try_enter(hwnd: usize) -> Option<Self> {
        MENU_WNDPROC_ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if active.contains(&hwnd) {
                None
            } else {
                active.push(hwnd);
                Some(Self { hwnd })
            }
        })
    }
}

impl Drop for MenuWndProcGuard {
    fn drop(&mut self) {
        MENU_WNDPROC_ACTIVE.with(|active| {
            active.borrow_mut().retain(|&h| h != self.hwnd);
        });
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
    let Some(_guard) = MenuWndProcGuard::try_enter(hwnd as usize) else {
        // Re-entrant call on this HWND: fall through to the default procedure.
        // No `&mut` to the pane or session is created here.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let pane_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MenuPane };
    if pane_ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // `session.root` aliases the root `MenuPane`, so `&mut *pane_ptr` and
    // `&mut *session_ptr` must never be alive at the same time. Keep raw
    // pointers and create short-lived borrows only inside the match arms;
    // clone any pane data into owned locals before calling into the session
    // (those calls can synchronously re-enter this WndProc).
    let session_ptr = unsafe { (*pane_ptr).session };

    match msg {
        msg if menu_close_action(msg) == MenuCloseAction::EndSession => {
            // End the modal loop rather than default-processing WM_CLOSE (which
            // would destroy one HWND while the session still holds the others).
            if !session_ptr.is_null() {
                unsafe {
                    (*session_ptr).selected_id = None;
                    (*session_ptr).done.store(true, Ordering::Release);
                }
            }
            0
        }
        WM_PAINT => {
            unsafe {
                (&mut *pane_ptr).paint();
            }
            0
        }
        WM_SETCURSOR => {
            let cur = if unsafe { (*pane_ptr).hovered.is_some() } {
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
            if !unsafe { (*pane_ptr).tracking } {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    TrackMouseEvent(&mut tme);
                    (*pane_ptr).tracking = true;
                }
            }
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            unsafe {
                if let Some(ref mut glass) = (*pane_ptr).spatial_glass {
                    glass.set_pointer(x as f32, y as f32);
                }
            }
            let next = unsafe {
                (*pane_ptr)
                    .row_rects
                    .iter()
                    .position(|r| r.as_ref().is_some_and(|rc| pt_in_rect(rc, x, y)))
            };
            if next != unsafe { (*pane_ptr).hovered } {
                unsafe {
                    (*pane_ptr).hovered = next;
                }
                if !unsafe { (*pane_ptr).is_submenu } && !session_ptr.is_null() {
                    // Clone before touching the session: `open_submenu` borrows
                    // `session.root`, which aliases `pane_ptr` for the root menu.
                    let pane = unsafe { pane_ptr.as_ref() }
                        .expect("menu pane pointer must be valid in its own wndproc");
                    let sub_items: Option<Vec<MenuEntry>> = match next {
                        Some(idx) => match pane.entries.get(idx) {
                            Some(MenuEntry::Submenu { items, .. }) => Some(items.clone()),
                            _ => None,
                        },
                        None => None,
                    };
                    let row_rect: Option<RECT> =
                        next.and_then(|idx| pane.row_rects.get(idx).copied().flatten());
                    unsafe {
                        let session = &mut *session_ptr;
                        match (next, sub_items, row_rect) {
                            (Some(idx), Some(items), Some(rc)) => {
                                session.open_submenu(idx, items, rc)
                            }
                            // Moved to a different row: that is an explicit
                            // intent change, not a transition into the submenu.
                            (Some(_), _, _) => session.close_submenu(),
                            // No hovered row: this is the padding gap between
                            // the parent row and the submenu edge. The grace
                            // policy below decides whether to keep it open.
                            (None, _, _) => {}
                        }
                    }
                }
                unsafe {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            // `hovered` stays `None` while the pointer crosses the padding strip
            // on its way to the submenu, so a hover-change-only check would
            // destroy the submenu too early. Re-evaluate every move instead.
            if !unsafe { (*pane_ptr).is_submenu } && !session_ptr.is_null() {
                let origin = unsafe { ((*pane_ptr).screen_rect.left, (*pane_ptr).screen_rect.top) };
                unsafe {
                    (*session_ptr).refresh_submenu_hover(Some((x + origin.0, y + origin.1)));
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            unsafe {
                (*pane_ptr).tracking = false;
                if let Some(ref mut glass) = (*pane_ptr).spatial_glass {
                    glass.clear_pointer();
                }
                if (*pane_ptr).hovered.take().is_some() {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            if !session_ptr.is_null() {
                let mut pt = POINT { x: 0, y: 0 };
                let pointer = unsafe { GetCursorPos(&mut pt) != 0 }.then_some((pt.x, pt.y));
                // Leaving the parent pane is normal when the pointer enters the
                // submenu; the pure policy keeps it alive if the pointer is on
                // the submenu (or still on its parent row during the grace).
                unsafe {
                    (*session_ptr).refresh_submenu_hover(pointer);
                }
            }
            0
        }
        WM_LBUTTONUP => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            let idx = unsafe {
                (*pane_ptr)
                    .row_rects
                    .iter()
                    .position(|r| r.as_ref().is_some_and(|rc| pt_in_rect(rc, x, y)))
            };
            if let Some(idx) = idx {
                enum ClickAction {
                    Select(u32),
                    Submenu(Vec<MenuEntry>, RECT),
                }
                let action = unsafe {
                    match &(&(*pane_ptr).entries)[idx] {
                        MenuEntry::Item { id, .. } => Some(ClickAction::Select(*id)),
                        MenuEntry::Submenu { items, .. } if !(*pane_ptr).is_submenu => {
                            (&(*pane_ptr).row_rects)[idx]
                                .map(|rc| ClickAction::Submenu(items.clone(), rc))
                        }
                        _ => None,
                    }
                };
                if !session_ptr.is_null() {
                    unsafe {
                        let session = &mut *session_ptr;
                        match action {
                            Some(ClickAction::Select(id)) => {
                                session.selected_id = Some(id);
                                session.done.store(true, Ordering::Release);
                            }
                            Some(ClickAction::Submenu(items, rc)) => {
                                session.open_submenu(idx, items, rc);
                            }
                            None => {}
                        }
                    }
                }
            } else if !session_ptr.is_null() {
                unsafe {
                    (*session_ptr).done.store(true, Ordering::Release);
                }
            }
            0
        }
        WM_KEYDOWN => {
            let action = menu_key_action(msg, wparam);
            if action == MenuKeyAction::Other {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            // Snapshot pane data before touching the session: `open_submenu` /
            // `close_submenu` can synchronously re-enter this WndProc, so no
            // borrow of `pane_ptr` may be alive across those calls.
            let is_submenu = unsafe { (*pane_ptr).is_submenu };
            let entries = unsafe { (&*pane_ptr).entries.clone() };
            let cursor = unsafe { (*pane_ptr).keyboard_index.or((*pane_ptr).hovered) };
            let submenu_open =
                !session_ptr.is_null() && unsafe { (*session_ptr).submenu.is_some() };
            let mut invalidate = false;

            match action {
                MenuKeyAction::Escape => {
                    if !session_ptr.is_null() {
                        unsafe {
                            let session = &mut *session_ptr;
                            if session.submenu.is_some() {
                                // First Escape closes the submenu, second ends the menu.
                                session.close_submenu();
                            } else {
                                session.selected_id = None;
                                session.done.store(true, Ordering::Release);
                            }
                        }
                        invalidate = true;
                    }
                }
                MenuKeyAction::Up | MenuKeyAction::Down => {
                    let delta = if action == MenuKeyAction::Up { -1 } else { 1 };
                    let next = keyboard_step(&entries, cursor, delta);
                    unsafe {
                        (*pane_ptr).keyboard_index = next;
                    }
                    // Moving the highlight is explicit intent: abandon any open submenu.
                    if !session_ptr.is_null() && submenu_open {
                        unsafe {
                            (*session_ptr).close_submenu();
                        }
                    }
                    invalidate = true;
                }
                MenuKeyAction::Left => {
                    if !session_ptr.is_null() && submenu_open {
                        unsafe {
                            (*session_ptr).close_submenu();
                        }
                        invalidate = true;
                    }
                }
                MenuKeyAction::Right => {
                    if !session_ptr.is_null()
                        && !is_submenu
                        && let Some(idx) = cursor
                        && let Some(MenuEntry::Submenu { items, .. }) = entries.get(idx)
                        && let Some(rect) =
                            unsafe { (&*pane_ptr).row_rects.get(idx).copied().flatten() }
                    {
                        let items = items.clone();
                        unsafe {
                            (*session_ptr).open_submenu(idx, items, rect);
                        }
                        invalidate = true;
                    }
                }
                MenuKeyAction::Confirm => {
                    if let Some(idx) = cursor {
                        match entries.get(idx) {
                            Some(MenuEntry::Item { id, .. }) if !session_ptr.is_null() => unsafe {
                                (*session_ptr).selected_id = Some(*id);
                                (*session_ptr).done.store(true, Ordering::Release);
                            },
                            Some(MenuEntry::Submenu { items, .. }) if !is_submenu => {
                                if let Some(rect) =
                                    unsafe { (&*pane_ptr).row_rects.get(idx).copied().flatten() }
                                {
                                    let items = items.clone();
                                    if !session_ptr.is_null() {
                                        unsafe {
                                            (*session_ptr).open_submenu(idx, items, rect);
                                        }
                                        invalidate = true;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                MenuKeyAction::Other => unreachable!("classified as Other above"),
            }
            if invalidate {
                unsafe {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }
        WM_ACTIVATE if (wparam & 0xffff) == 0 && unsafe { !(*pane_ptr).is_submenu } => {
            let other = lparam as HWND;
            if !session_ptr.is_null() {
                let is_own_sub = unsafe {
                    (*session_ptr)
                        .submenu
                        .as_ref()
                        .is_some_and(|sub| sub.hwnd == other)
                };
                if !is_own_sub {
                    unsafe {
                        (*session_ptr).done.store(true, Ordering::Release);
                    }
                }
            }
            0
        }
        WM_KILLFOCUS if unsafe { !(*pane_ptr).is_submenu } => {
            let other = wparam as HWND;
            if !session_ptr.is_null() {
                let is_own_sub = unsafe {
                    (*session_ptr)
                        .submenu
                        .as_ref()
                        .is_some_and(|sub| sub.hwnd == other)
                };
                if !is_own_sub {
                    unsafe {
                        (*session_ptr).done.store(true, Ordering::Release);
                    }
                }
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MENU_LABEL_SHORTCUT_GAP_DIPS, MENU_MAX_W_DIPS, MENU_W_DIPS, MenuAnchor, MenuCloseAction,
        MenuEdge, MenuEntry, MenuIcon, MenuKeyAction, MenuWndProcGuard, NATIVE_SYS_COMMAND_FLAG,
        PAD_X_DIPS, SUBMENU_HOVER_GRACE_MS, compute_menu_width_dips, compute_menu_width_dips_with,
        compute_popup_origin, create_menu_formats, encode_native_command, estimate_text_width_dips,
        keyboard_step, measure_entries, measure_text_width_dwrite, menu_close_action,
        menu_key_action, menu_max_width_dips, pt_in_rect, screen_row_rect, selectable_indices,
        should_close_submenu, to_app_icon,
    };
    use crate::design::icons::AppIcon;
    use crate::platform::win32::MonitorInfo;
    use windows::Win32::Foundation::RECT as WinRect;
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_DOWN, VK_END, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{WM_CLOSE, WM_KEYDOWN, WM_PAINT};

    #[test]
    fn wndproc_guard_rejects_same_hwnd_reentry_and_allows_others() {
        let first = MenuWndProcGuard::try_enter(0x1234).expect("first entry");
        assert!(MenuWndProcGuard::try_enter(0x1234).is_none());
        let other = MenuWndProcGuard::try_enter(0x5678).expect("different HWND");
        drop(first);
        let again = MenuWndProcGuard::try_enter(0x1234).expect("released HWND");
        drop(other);
        drop(again);
    }

    #[test]
    fn measure_entries_marks_separators_and_hit_testing_is_half_open() {
        use super::MenuEntry;
        let (w, h, rects) = measure_entries(
            &[
                MenuEntry::item(1, super::MenuIcon::Search, "a"),
                MenuEntry::Separator,
            ],
            256.0,
            1.0,
        );
        assert_eq!(w, 256);
        assert!(h > 32);
        assert_eq!(rects.len(), 2);
        assert!(rects[0].is_some());
        assert!(rects[1].is_none());
        let rc = rects[0].unwrap();
        assert!(pt_in_rect(&rc, rc.left, rc.top));
        assert!(!pt_in_rect(&rc, rc.right, rc.top));
        assert!(!pt_in_rect(&rc, rc.left, rc.bottom));
    }

    #[test]
    fn native_commands_encode_system_flag_without_losing_the_id() {
        let app = encode_native_command(42, false);
        let sys = encode_native_command(0xF020, true);
        assert_eq!(app, 42);
        assert_ne!(sys & NATIVE_SYS_COMMAND_FLAG, 0);
        assert_eq!(sys & !NATIVE_SYS_COMMAND_FLAG, 0xF020);
    }

    #[test]
    fn submenu_lifetime_keeps_pointer_on_parent_row_or_submenu() {
        let parent = RECT {
            left: 100,
            top: 100,
            right: 356,
            bottom: 400,
        };
        let row = RECT {
            left: 105,
            top: 132,
            right: 351,
            bottom: 164,
        };
        let submenu = RECT {
            left: 354,
            top: 132,
            right: 586,
            bottom: 400,
        };
        let close = |elapsed, pointer| {
            should_close_submenu(
                true,
                elapsed,
                pointer,
                Some(parent),
                Some(row),
                Some(submenu),
                SUBMENU_HOVER_GRACE_MS,
            )
        };

        // Parent row and submenu pane are always safe, regardless of grace.
        assert!(!close(None, Some((200, 148))));
        assert!(!close(None, Some((400, 148))));
        // Half-open rect boundaries: submenu.left is inclusive, row.right is not.
        assert!(!close(None, Some((354, 148))));
        assert!(close(None, Some((351, 148))));
        // Outside both rects closes immediately, even during grace.
        assert!(close(Some(0), Some((700, 148))));
        // No pointer information cannot keep an orphaned submenu alive.
        assert!(close(Some(0), None));
        // Not open: nothing to close.
        assert!(!should_close_submenu(
            false,
            None,
            Some((700, 148)),
            Some(parent),
            Some(row),
            Some(submenu),
            SUBMENU_HOVER_GRACE_MS,
        ));
    }

    #[test]
    fn submenu_lifetime_grace_covers_parent_padding_gap() {
        let parent = RECT {
            left: 100,
            top: 100,
            right: 356,
            bottom: 400,
        };
        let row = RECT {
            left: 105,
            top: 132,
            right: 351,
            bottom: 164,
        };
        let submenu = RECT {
            left: 354,
            top: 132,
            right: 586,
            bottom: 400,
        };
        // The 3px strip between the row rect and the submenu's left edge is
        // inside the parent pane but in neither hit-test rect.
        let gap = (352, 148);
        assert!(pt_in_rect(&parent, gap.0, gap.1));
        assert!(!pt_in_rect(&row, gap.0, gap.1));
        assert!(!pt_in_rect(&submenu, gap.0, gap.1));
        let close = |elapsed| {
            should_close_submenu(
                true,
                Some(elapsed),
                Some(gap),
                Some(parent),
                Some(row),
                Some(submenu),
                SUBMENU_HOVER_GRACE_MS,
            )
        };
        assert!(!close(0));
        assert!(!close(SUBMENU_HOVER_GRACE_MS));
        assert!(close(SUBMENU_HOVER_GRACE_MS + 1));
    }

    #[test]
    fn wm_close_ends_the_menu_session_instead_of_destroying_one_pane() {
        assert_eq!(menu_close_action(WM_CLOSE), MenuCloseAction::EndSession);
        assert_eq!(menu_close_action(WM_PAINT), MenuCloseAction::None);
        assert_eq!(menu_close_action(WM_KEYDOWN), MenuCloseAction::None);
    }

    #[test]
    fn keyboard_navigation_skips_separators_and_wraps() {
        let entries = vec![
            MenuEntry::item(1, MenuIcon::Search, "a"),
            MenuEntry::Separator,
            MenuEntry::item(2, MenuIcon::Settings, "b"),
            MenuEntry::submenu(MenuIcon::Folder, "more", vec![]),
        ];
        assert_eq!(selectable_indices(&entries), vec![0, 2, 3]);
        assert_eq!(keyboard_step(&entries, None, 1), Some(0));
        assert_eq!(keyboard_step(&entries, Some(0), 1), Some(2));
        assert_eq!(keyboard_step(&entries, Some(3), 1), Some(0));
        assert_eq!(keyboard_step(&entries, Some(0), -1), Some(3));
        assert_eq!(keyboard_step(&entries, Some(2), -1), Some(0));
        // A stale cursor on a separator is normalised to the nearest row.
        assert_eq!(keyboard_step(&entries, Some(1), 1), Some(2));
        assert_eq!(keyboard_step(&[MenuEntry::Separator], None, 1), None);
    }

    #[test]
    fn keyboard_action_accepts_arrows_enter_space_and_escape() {
        assert_eq!(
            menu_key_action(WM_KEYDOWN, VK_DOWN as usize),
            MenuKeyAction::Down
        );
        assert_eq!(
            menu_key_action(WM_KEYDOWN, VK_UP as usize),
            MenuKeyAction::Up
        );
        assert_eq!(
            menu_key_action(WM_KEYDOWN, VK_LEFT as usize),
            MenuKeyAction::Left
        );
        assert_eq!(
            menu_key_action(WM_KEYDOWN, VK_RIGHT as usize),
            MenuKeyAction::Right
        );
        assert_eq!(
            menu_key_action(WM_KEYDOWN, VK_RETURN as usize),
            MenuKeyAction::Confirm
        );
        assert_eq!(
            menu_key_action(WM_KEYDOWN, VK_SPACE as usize),
            MenuKeyAction::Confirm
        );
        assert_eq!(
            menu_key_action(WM_KEYDOWN, VK_ESCAPE as usize),
            MenuKeyAction::Escape
        );
        assert_eq!(
            menu_key_action(WM_KEYDOWN, VK_END as usize),
            MenuKeyAction::Other
        );
        assert_eq!(
            menu_key_action(WM_PAINT, VK_DOWN as usize),
            MenuKeyAction::Other
        );
    }

    #[test]
    fn screen_row_rect_translates_pane_local_rows() {
        let pane = RECT {
            left: -120,
            top: 40,
            right: 136,
            bottom: 340,
        };
        let row = RECT {
            left: 5,
            top: 32,
            right: 251,
            bottom: 64,
        };
        let screen = screen_row_rect(&pane, &row);
        assert_eq!(screen.left, -115);
        assert_eq!(screen.top, 72);
        assert_eq!(screen.right, 131);
        assert_eq!(screen.bottom, 104);
        assert!(pt_in_rect(&screen, -115, 72));
        assert!(!pt_in_rect(&screen, 131, 72));
    }

    #[test]
    fn new_menu_icons_map_to_expected_app_icons() {
        assert_eq!(to_app_icon(MenuIcon::Bell), AppIcon::Bell);
        assert_eq!(to_app_icon(MenuIcon::Focus), AppIcon::Moon);
    }

    #[test]
    fn dynamic_menu_width_expands_for_long_labels_and_shortcuts_without_exceeding_cap() {
        let short = vec![MenuEntry::item(1, MenuIcon::Settings, "Settings")];
        let short_w = compute_menu_width_dips_with(
            &short,
            MENU_W_DIPS,
            MENU_MAX_W_DIPS,
            |t| estimate_text_width_dips(t, 13.0),
            |t| estimate_text_width_dips(t, 11.0),
        );
        assert_eq!(short_w, MENU_W_DIPS);

        let long_tr = vec![
            MenuEntry::item(2, MenuIcon::Refresh, "Windows görev çubuğunu geri yükle"),
            MenuEntry::with_shortcut(3, MenuIcon::Activity, "Görev Yöneticisi", "Ctrl+Shift+Esc"),
        ];
        let long_w = compute_menu_width_dips_with(
            &long_tr,
            MENU_W_DIPS,
            MENU_MAX_W_DIPS,
            |t| estimate_text_width_dips(t, 13.0),
            |t| estimate_text_width_dips(t, 11.0),
        );
        assert!(
            long_w > MENU_W_DIPS && long_w <= MENU_MAX_W_DIPS,
            "expected dynamic width in ({MENU_W_DIPS}, {MENU_MAX_W_DIPS}], got {long_w}"
        );

        let huge = vec![MenuEntry::item(
            4,
            MenuIcon::Settings,
            "An extraordinarily long localized context menu item label that must cap cleanly",
        )];
        let huge_w = compute_menu_width_dips_with(
            &huge,
            MENU_W_DIPS,
            MENU_MAX_W_DIPS,
            |t| estimate_text_width_dips(t, 13.0),
            |t| estimate_text_width_dips(t, 11.0),
        );
        assert_eq!(huge_w, MENU_MAX_W_DIPS);
    }

    #[test]
    fn popup_origin_anchors_above_bottom_dock_with_exact_8dip_gap() {
        let monitor = MonitorInfo {
            handle: 1,
            device_name: "DISPLAY1".to_owned(),
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
            dpi_scale: 1.0,
            is_primary: true,
        };
        let dock_rect = WinRect {
            left: 400,
            top: 1004,
            right: 1520,
            bottom: 1072,
        };
        let menu_w = 280;
        let menu_h = 380;
        let (x, y) = compute_popup_origin(
            POINT { x: 960, y: 1038 },
            menu_w,
            menu_h,
            &monitor,
            1.0,
            Some(MenuAnchor {
                visible_rect: dock_rect,
                edge: Some(MenuEdge::Bottom),
            }),
        );
        assert_eq!(x, 960);
        assert_eq!(
            y + menu_h,
            dock_rect.top - 8,
            "menu bottom {} must sit exactly 8px above dock top {}",
            y + menu_h,
            dock_rect.top
        );

        // High-DPI monitor: gap scales cleanly with canonical DPI.
        let hidpi_monitor = MonitorInfo {
            dpi_scale: 1.5,
            ..monitor.clone()
        };
        let (_, hy) = compute_popup_origin(
            POINT { x: 960, y: 1038 },
            menu_w,
            menu_h,
            &hidpi_monitor,
            1.5,
            Some(MenuAnchor {
                visible_rect: dock_rect,
                edge: Some(MenuEdge::Bottom),
            }),
        );
        assert_eq!(
            hy + menu_h,
            dock_rect.top - 12,
            "HiDPI menu bottom {} must sit exactly 12px above dock top {}",
            hy + menu_h,
            dock_rect.top
        );

        // Top, left, and right edge docks anchor outside their respective edges.
        let top_dock = WinRect {
            left: 400,
            top: 8,
            right: 1520,
            bottom: 76,
        };
        let (_, top_y) = compute_popup_origin(
            POINT { x: 960, y: 40 },
            menu_w,
            menu_h,
            &monitor,
            1.0,
            Some(MenuAnchor {
                visible_rect: top_dock,
                edge: Some(MenuEdge::Top),
            }),
        );
        assert_eq!(top_y, top_dock.bottom + 8);

        let left_dock = WinRect {
            left: 8,
            top: 200,
            right: 76,
            bottom: 880,
        };
        let (left_x, _) = compute_popup_origin(
            POINT { x: 40, y: 540 },
            menu_w,
            menu_h,
            &monitor,
            1.0,
            Some(MenuAnchor {
                visible_rect: left_dock,
                edge: Some(MenuEdge::Left),
            }),
        );
        assert_eq!(left_x, left_dock.right + 8);

        let right_dock = WinRect {
            left: 1844,
            top: 200,
            right: 1912,
            bottom: 880,
        };
        let (right_x, _) = compute_popup_origin(
            POINT { x: 1878, y: 540 },
            menu_w,
            menu_h,
            &monitor,
            1.0,
            Some(MenuAnchor {
                visible_rect: right_dock,
                edge: Some(MenuEdge::Right),
            }),
        );
        assert_eq!(right_x + menu_w, right_dock.left - 8);
    }

    #[test]
    fn popup_origin_compact_bottom_left_dock_across_dpis_never_jumps_right() {
        for dpi in [1.0_f32, 1.25, 1.5, 2.0] {
            let monitor = MonitorInfo {
                handle: 1,
                device_name: "DISPLAY1".to_owned(),
                left: 0,
                top: 0,
                right: (1920.0 * dpi).round() as i32,
                bottom: (1080.0 * dpi).round() as i32,
                dpi_scale: dpi,
                is_primary: true,
            };
            let dock_w = (220.0 * dpi).round() as i32;
            let dock_h = (56.0 * dpi).round() as i32;
            let margin_edge = (8.0 * dpi).round() as i32;
            let visible_rect = WinRect {
                left: margin_edge,
                top: monitor.bottom - margin_edge - dock_h,
                right: margin_edge + dock_w,
                bottom: monitor.bottom - margin_edge,
            };
            let menu_w = (280.0 * dpi).round() as i32;
            let menu_h = (320.0 * dpi).round() as i32;
            let expected_gap = (8.0 * dpi).round() as i32;
            let anchor = Some(MenuAnchor {
                visible_rect,
                edge: Some(MenuEdge::Bottom),
            });

            // Cursor near the left end of the compact dock: x stays at cursor and never jumps
            // to `dock_rect.right + gap`.
            let cursor_left = POINT {
                x: margin_edge + (60.0 * dpi).round() as i32,
                y: visible_rect.top + dock_h / 2,
            };
            let (x, y) = compute_popup_origin(cursor_left, menu_w, menu_h, &monitor, dpi, anchor);
            assert_eq!(x, cursor_left.x, "dpi={dpi}: x must stay at cursor");
            assert_ne!(
                x,
                visible_rect.right + expected_gap,
                "dpi={dpi}: bottom-left dock must never jump to dock_rect.right + gap"
            );
            assert_eq!(
                visible_rect.top - (y + menu_h),
                expected_gap,
                "dpi={dpi}: gap above visible_rect.top must equal 8 * dpi"
            );

            // Cursor near the right monitor edge: flips left to `cursor.x - menu_w`.
            let cursor_right = POINT {
                x: monitor.right - (40.0 * dpi).round() as i32,
                y: visible_rect.top + dock_h / 2,
            };
            let (rx, ry) =
                compute_popup_origin(cursor_right, menu_w, menu_h, &monitor, dpi, anchor);
            assert_eq!(
                rx,
                cursor_right.x - menu_w,
                "dpi={dpi}: x must flip left when cursor + menu_w exceeds work_right - margin"
            );
            assert_eq!(
                visible_rect.top - (ry + menu_h),
                expected_gap,
                "dpi={dpi}: gap above visible_rect.top must equal 8 * dpi on right-edge flip"
            );
        }
    }

    #[test]
    fn popup_origin_without_anchor_opens_at_cursor_and_flips_near_edges() {
        let monitor = MonitorInfo {
            handle: 1,
            device_name: "DISPLAY1".to_owned(),
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
            dpi_scale: 1.0,
            is_primary: true,
        };
        let menu_w = 280;
        let menu_h = 340;

        let cursor = POINT { x: 520, y: 310 };
        assert_eq!(
            compute_popup_origin(cursor, menu_w, menu_h, &monitor, 1.0, None),
            (520, 310)
        );

        let no_edge_anchor = Some(MenuAnchor {
            visible_rect: WinRect {
                left: 0,
                top: 1000,
                right: 1920,
                bottom: 1080,
            },
            edge: None,
        });
        assert_eq!(
            compute_popup_origin(cursor, menu_w, menu_h, &monitor, 1.0, no_edge_anchor),
            (520, 310)
        );

        let corner = POINT { x: 1850, y: 980 };
        assert_eq!(
            compute_popup_origin(corner, menu_w, menu_h, &monitor, 1.0, None),
            (1850 - menu_w, 980 - menu_h)
        );

        // Narrow/short monitor smaller than menu: clamps safely without panicking.
        let tiny = MonitorInfo {
            right: 200,
            bottom: 200,
            ..monitor
        };
        assert_eq!(
            compute_popup_origin(POINT { x: 150, y: 150 }, menu_w, menu_h, &tiny, 1.0, None),
            (8, 8)
        );
    }

    #[test]
    fn turkish_menu_strings_fit_with_zero_truncation() {
        let entries = vec![
            MenuEntry::item(1, MenuIcon::Refresh, "Windows görev çubuğunu geri yükle"),
            MenuEntry::with_shortcut(2, MenuIcon::Activity, "Görev Yöneticisi", "Ctrl+Shift+Esc"),
            MenuEntry::with_shortcut(3, MenuIcon::Monitor, "Masaüstünü göster", "Win+D"),
        ];
        let max_w = menu_max_width_dips();
        let width = compute_menu_width_dips(&entries, MENU_W_DIPS, max_w);
        assert!(
            width >= MENU_W_DIPS && width <= max_w,
            "width {width} out of [{MENU_W_DIPS}, {max_w}]"
        );

        if let Some(fmts) = create_menu_formats() {
            let content_w = width - PAD_X_DIPS * 2.0 - 36.0 - 10.0;
            for entry in &entries {
                if let MenuEntry::Item {
                    label, shortcut, ..
                } = entry
                {
                    let label_w = measure_text_width_dwrite(&fmts.dwrite, &fmts.text_fmt, label)
                        .expect("DirectWrite label measurement");
                    let needed = match shortcut.as_deref() {
                        Some(sc) if !sc.is_empty() => {
                            let sc_w = measure_text_width_dwrite(&fmts.dwrite, &fmts.hint_fmt, sc)
                                .expect("DirectWrite shortcut measurement");
                            label_w + MENU_LABEL_SHORTCUT_GAP_DIPS + sc_w
                        }
                        _ => label_w,
                    };
                    assert!(
                        needed <= content_w,
                        "Turkish entry {label:?} ({shortcut:?}) needs {needed} DIPs > available {content_w} DIPs (menu width {width})"
                    );
                }
            }
        }
    }
}
