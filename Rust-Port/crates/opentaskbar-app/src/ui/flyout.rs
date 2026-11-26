//! Shared C#-parity `DockFlyout` window rendered with Slate Neutral + Aurora design tokens.
//!
//! Used across Background Apps (System Tray), Folder Stacks, App Groups, and all 23 Dock Widgets.
//! Supports:
//! - Exact C# `DockFlyout.cs` visuals (`#252B37` dark / `#FFFFFF` light, `14px` padding, 190ms ease-out)
//! - In-place live interactive updates (`show_flyout_live`) for Calendar month/year navigation,
//!   Audio Output/Input continuous slider drag & mute, and Focus/Stopwatch/Countdown timers
//! - Full C# widget blocks: `CalendarGrid`, `Slider`, `Sparkline`, `MetricTiles`, `BadgeRow`,
//!   `StatusHeader`, `Hero`, `ToggleGrid`, `ProgressBar`, `StatRow`, `ActionRow`, `AppRow`, `ButtonBar`
//! - Mouse-wheel vertical scrolling + thin scrollbar thumb + toggle-close suppression.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, CreateFontW, CreatePen, CreateSolidBrush,
    DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, GetMonitorInfoW, HDC,
    IntersectClipRect, InvalidateRect, LineTo, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, MoveToEx, PAINTSTRUCT, PS_SOLID, RestoreDC, RoundRect, SaveDC, SelectObject,
    SetBkMode, SetTextColor, StretchDIBits, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VK_BACK, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect,
    GetMessageW, GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IDC_HAND, KillTimer, LWA_ALPHA,
    LoadCursorW, MSG, RegisterClassExW, SW_SHOW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    SWP_SHOWWINDOW, SetCursor, SetForegroundWindow, SetLayeredWindowAttributes, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WM_ACTIVATE, WM_CHAR,
    WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT,
    WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use super::menu::{MenuIcon, draw_menu_icon};
use crate::design::{self, Rgb, gdi_rgb};
use crate::platform::win32;
use crate::widgets::telemetry;

const WM_MOUSELEAVE: u32 = 0x02A3;
const DWMWCP_ROUND: u32 = 2;
const MAX_FLYOUT_H_DIPS: f32 = 460.0;
const TIMER_ANIM: usize = 1;
const OPEN_ANIM_MS: f32 = 175.0;
const CLOSE_ANIM_MS: f32 = 95.0;
const SLIDE_OFFSET_DIPS: f32 = 7.0;

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

/// Straight BGRA32 icon payload for `FlyoutBlock::ActionRow` / `AppRow`.
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

/// Interactive event dispatched to live flyouts (`show_flyout_live`).
///
/// Not `Copy`: `TextCommit` owns the committed text (C2 contract with WS-C widgets).
#[derive(Clone, Debug)]
pub enum LiveEvent {
    Click(u32),
    Slider { id: u32, ratio: f32 },
    /// Media timeline scrubbing — `ratio` is in `0.0..=1.0`.
    #[allow(dead_code)] // consumed by WS-C widgets after merge
    Seek { id: u32, ratio: f32 },
    /// Text input committed with Enter (numeric inputs are clamped to their bounds).
    #[allow(dead_code)] // consumed by WS-C widgets after merge
    TextCommit { id: u32, text: String },
}

/// Result of handling a `LiveEvent`: either replace the flyout spec in place or close cleanly.
pub enum LiveOutcome {
    Update(FlyoutSpec),
    Close,
}

/// Semantic color mode for `FlyoutBlock::ProgressBar`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarTone {
    /// Always calm accent (`WidgetBlue` / `WidgetAccent`).
    Accent,
    /// High-is-critical utilization (`<75%` accent, `75-89%` amber, `>=90%` red).
    Usage,
    /// Low-is-critical remaining level (`<=15%` red, `<=30%` amber, `>30%` green).
    Level,
}

/// Declarative content block rendered inside the shared C#-parity `DockFlyout`.
#[derive(Clone, Debug)]
pub enum FlyoutBlock {
    /// Prominent hero metric (Clock 28px, Focus 44px, Weather 24px) with optional right metadata.
    Hero {
        value: String,
        caption: Option<String>,
        centered: bool,
        large: bool,
        right_meta: Option<String>,
    },
    /// C# `ClockWidget.BuildCalendar`: interactive month/year nav + 7-col calendar grid.
    CalendarGrid {
        year: i32,
        month: u32,
        today: (i32, u32, u32),
        selected: Option<(i32, u32, u32)>,
        prev_year_id: u32,
        prev_month_id: u32,
        title_id: u32,
        next_month_id: u32,
        next_year_id: u32,
        /// Clickable day cell IDs are `day_id_base + cell_index` (`0..42`).
        day_id_base: u32,
    },
    /// C# `AudioWidget` Output/Input mixer section: label + `%`, device pill + mute btn, draggable slider.
    AudioSliderSection {
        label: String,
        device_name: String,
        value_text: String,
        ratio: f32,
        muted: bool,
        mute_id: u32,
        slider_id: u32,
    },
    /// C# `SystemStatsWidget` telemetry row with 45-sample vector sparkline.
    Sparkline {
        label: String,
        value_text: String,
        detail_text: Option<String>,
        samples: Vec<f32>,
        color: Rgb,
    },
    /// C# `WeatherWidget` / `NetSpeedWidget` 2-column metric tiles (`SurfaceSelected`, `r=8`).
    MetricTiles(Vec<(String, String)>),
    /// C# `KeyboardLayoutWidget` selectable row: short-code badge pill + full display name.
    BadgeRow {
        id: u32,
        code: String,
        title: String,
        active: bool,
    },
    /// C# `AiUsageWidget.SectionHeader`: title + status dot + right dashboard button.
    StatusHeader {
        title: String,
        status_label: String,
        status_color: Rgb,
        action_id: Option<u32>,
        action_label: Option<String>,
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
        tone: BarTone,
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
    /// Horizontal button row: `(id, label, primary_selected_style)`.
    ButtonBar(Vec<(u32, String, bool)>),

    // ---- C2 contract blocks (WS-C widget flyouts consume these verbatim) ----
    #[allow(dead_code)] // consumed by WS-C widgets after merge
    /// Media / timeline scrubber. `ratio` is authoritative for rendering; the
    /// `position_secs`/`duration_secs` pair supplies the `m:ss / m:ss` caption.
    SeekSlider {
        id: u32,
        label: String,
        position_secs: u32,
        duration_secs: u32,
        ratio: f32,
    },
    /// Inline editable text field (e.g. custom countdown minutes). Emits
    /// `LiveEvent::TextCommit` on Enter; numeric fields accept only numbers.
    #[allow(dead_code)] // consumed by WS-C widgets after merge
    TextInput {
        id: u32,
        label: String,
        value: String,
        numeric: bool,
        min: f64,
        max: f64,
    },
    /// Multi-series sparkline/area chart: `(series_name, samples)` with an accent color.
    #[allow(dead_code)] // consumed by WS-C widgets after merge
    Chart {
        label: String,
        series: Vec<(String, Vec<f32>)>,
        accent: Rgb,
    },
    /// Cover art / country flag. `glyph` is a Segoe Fluent Icons code point used
    /// when `path` is absent or cannot be decoded; `size_dips` is the square size.
    #[allow(dead_code)] // consumed by WS-C widgets after merge
    Image {
        glyph: u16,
        path: Option<String>,
        size_dips: f32,
    },
    /// Compact toolbar: `(id, accessible label, Segoe Fluent Icons glyph, active)`.
    #[allow(dead_code)] // consumed by WS-C widgets after merge
    IconBar {
        buttons: Vec<(u32, String, u16, bool)>,
    },

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

#[derive(Clone, Copy)]
struct HitTarget {
    id: u32,
    rect: RECT,
    menu_id: u32,
    is_slider: bool,
    /// C2 `SeekSlider`: draggable timeline emitting `LiveEvent::Seek`.
    is_seek: bool,
    /// C2 `TextInput`: click focuses an in-place editor.
    is_text: bool,
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
    on_live: Option<&'a mut dyn FnMut(LiveEvent) -> LiveOutcome>,
    content_top_px: i32,
    viewport_h_px: i32,
    content_h_px: i32,
    scroll_y: i32,
    targets: Vec<HitTarget>,
    hovered_id: Option<u32>,
    /// `(id, track rect, is_seek)` — the third field selects `Seek` vs `Slider` events.
    dragging_slider: Option<(u32, RECT, bool)>,
    /// In-place C2 `TextInput` editor: `(id, buffer)`.
    editing: Option<(u32, String)>,
    keyboard_index: Option<usize>,
    keyboard_navigation: bool,
    tracking: bool,
    selected: Option<FlyoutAction>,
    done: AtomicBool,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Null-terminated, empty-safe wrapper around `user32!DrawTextW`.
///
/// Passing an empty `Vec<u16>` pointer (`NonNull::dangling() == 0x2`) to `DrawTextW`
/// causes an access violation (`0xC0000005`) inside `user32.dll` on Windows 11.
unsafe fn draw_gdi_text(hdc: HDC, text: &str, rc: &mut RECT, format: u32) {
    if text.is_empty() {
        return;
    }
    let mut buf: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let len = (buf.len() - 1) as i32;
    unsafe {
        DrawTextW(hdc, buf.as_mut_ptr(), len, rc, format);
    }
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

fn fill_round_rect_bordered(hdc: HDC, rc: RECT, rad: i32, fill_color: u32, border_color: u32) {
    unsafe {
        let brush = CreateSolidBrush(fill_color);
        let pen = CreatePen(PS_SOLID, 1, border_color);
        let ob = SelectObject(hdc, brush as _);
        let op = SelectObject(hdc, pen as _);
        RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, rad, rad);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        DeleteObject(brush as _);
        DeleteObject(pen as _);
    }
}

fn fill_round_rect(hdc: HDC, l: i32, t: i32, r: i32, b: i32, rad: i32, color: u32) {
    fill_round_rect_bordered(
        hdc,
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        },
        rad,
        color,
        color,
    );
}

/// Computes the layout height and content-space hit targets for `spec`.
fn layout_flyout(spec: &FlyoutSpec, width_px: i32, dpi: f32) -> (i32, i32, Vec<HitTarget>) {
    let s = |v: f32| (v * dpi).round() as i32;
    let pad = s(14.0);
    let content_right_pad = pad + s(4.0);
    let has_header = !spec.title.is_empty();
    let header_h = if !has_header {
        0
    } else if spec.subtitle.is_some() {
        s(44.0)
    } else {
        s(26.0)
    };
    let content_top_px = if has_header {
        pad + header_h + s(6.0)
    } else {
        pad
    };

    let mut y = content_top_px;
    let mut targets = Vec::new();
    let content_w = (width_px - pad - content_right_pad).max(s(120.0));

    for block in &spec.blocks {
        match block {
            FlyoutBlock::Hero { caption, large, .. } => {
                let val_h = if *large { s(48.0) } else { s(34.0) };
                y += val_h + if caption.is_some() { s(20.0) } else { s(6.0) };
            }
            FlyoutBlock::CalendarGrid {
                prev_year_id,
                prev_month_id,
                title_id,
                next_month_id,
                next_year_id,
                day_id_base,
                ..
            } => {
                // Navigation bar (`<<` `<` `Month YYYY` `>` `>>`)
                let nav_h = s(28.0);
                let btn_w = s(26.0);
                let gap = s(2.0);
                let left = pad;
                let right = pad + content_w;
                targets.push(HitTarget {
                    id: *prev_year_id,
                    menu_id: *prev_year_id,
                    is_slider: false,
                    is_seek: false,
                    is_text: false,
                    rect: RECT {
                        left,
                        top: y,
                        right: left + btn_w,
                        bottom: y + nav_h,
                    },
                });
                targets.push(HitTarget {
                    id: *prev_month_id,
                    menu_id: *prev_month_id,
                    is_slider: false,
                    is_seek: false,
                    is_text: false,
                    rect: RECT {
                        left: left + btn_w + gap,
                        top: y,
                        right: left + btn_w * 2 + gap,
                        bottom: y + nav_h,
                    },
                });
                targets.push(HitTarget {
                    id: *title_id,
                    menu_id: *title_id,
                    is_slider: false,
                    is_seek: false,
                    is_text: false,
                    rect: RECT {
                        left: left + btn_w * 2 + gap * 2,
                        top: y,
                        right: right - btn_w * 2 - gap * 2,
                        bottom: y + nav_h,
                    },
                });
                targets.push(HitTarget {
                    id: *next_month_id,
                    menu_id: *next_month_id,
                    is_slider: false,
                    is_seek: false,
                    is_text: false,
                    rect: RECT {
                        left: right - btn_w * 2 - gap,
                        top: y,
                        right: right - btn_w - gap,
                        bottom: y + nav_h,
                    },
                });
                targets.push(HitTarget {
                    id: *next_year_id,
                    menu_id: *next_year_id,
                    is_slider: false,
                    is_seek: false,
                    is_text: false,
                    rect: RECT {
                        left: right - btn_w,
                        top: y,
                        right,
                        bottom: y + nav_h,
                    },
                });
                y += nav_h + s(8.0);
                // Weekday labels row
                y += s(20.0);
                // 6 rows x 7 cols of 31px day cells
                let cell_w = content_w / 7;
                let cell_h = s(31.0);
                for cell in 0..42i32 {
                    let row = cell / 7;
                    let col = cell % 7;
                    let cx = pad + col * cell_w;
                    let cy = y + row * cell_h;
                    let id = day_id_base + cell as u32;
                    targets.push(HitTarget {
                        id,
                        menu_id: id,
                        is_slider: false,
                    is_seek: false,
                    is_text: false,
                        rect: RECT {
                            left: cx + s(1.5),
                            top: cy + s(1.5),
                            right: cx + cell_w - s(1.5),
                            bottom: cy + cell_h - s(1.5),
                        },
                    });
                }
                y += 6 * cell_h + s(4.0);
            }
            FlyoutBlock::AudioSliderSection {
                mute_id, slider_id, ..
            } => {
                // Heading row (20px) + Device/Mute row (32px + 8px) + Slider row (20px + 4px)
                let dev_top = y + s(24.0);
                let mute_w = s(32.0);
                targets.push(HitTarget {
                    id: *mute_id,
                    menu_id: *mute_id,
                    is_slider: false,
                    is_seek: false,
                    is_text: false,
                    rect: RECT {
                        left: pad + content_w - mute_w,
                        top: dev_top,
                        right: pad + content_w,
                        bottom: dev_top + mute_w,
                    },
                });
                let slider_top = dev_top + mute_w + s(8.0);
                targets.push(HitTarget {
                    id: *slider_id,
                    menu_id: *slider_id,
                    is_slider: true,
                    is_seek: false,
                    is_text: false,
                    rect: RECT {
                        left: pad,
                        top: slider_top,
                        right: pad + content_w,
                        bottom: slider_top + s(20.0),
                    },
                });
                y = slider_top + s(24.0);
            }
            FlyoutBlock::Sparkline { .. } => {
                y += s(48.0);
            }
            FlyoutBlock::MetricTiles(tiles) => {
                let rows = tiles.len().div_ceil(2) as i32;
                let tile_h = s(44.0);
                let gap = s(6.0);
                y += rows * tile_h + (rows - 1).max(0) * gap + s(6.0);
            }
            FlyoutBlock::BadgeRow { id, .. } => {
                let h = s(34.0);
                targets.push(HitTarget {
                    id: *id,
                    menu_id: *id,
                    is_slider: false,
                    is_seek: false,
                    is_text: false,
                    rect: RECT {
                        left: pad,
                        top: y,
                        right: pad + content_w,
                        bottom: y + h,
                    },
                });
                y += h + s(4.0);
            }
            FlyoutBlock::StatusHeader { action_id, .. } => {
                let h = s(28.0);
                if let Some(id) = *action_id {
                    let bw = s(64.0);
                    targets.push(HitTarget {
                        id,
                        menu_id: id,
                        is_slider: false,
                    is_seek: false,
                    is_text: false,
                        rect: RECT {
                            left: pad + content_w - bw,
                            top: y,
                            right: pad + content_w,
                            bottom: y + h,
                        },
                    });
                }
                y += h + s(6.0);
            }
            FlyoutBlock::ToggleGrid(tiles) => {
                let rows = tiles.len().div_ceil(2) as i32;
                let tile_h = s(48.0);
                let gap = s(6.0);
                let tw = ((content_w - gap) / 2).max(s(80.0));
                for (i, (id, _, _, _)) in tiles.iter().enumerate() {
                    let col = (i % 2) as i32;
                    let row = (i / 2) as i32;
                    let left = pad + col * (tw + gap);
                    let top = y + row * (tile_h + gap);
                    targets.push(HitTarget {
                        id: *id,
                        menu_id: *id,
                        is_slider: false,
                    is_seek: false,
                    is_text: false,
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
            FlyoutBlock::StatRow { .. } => y += s(24.0),
            FlyoutBlock::ProgressBar { .. } => y += s(34.0),
            FlyoutBlock::ActionRow { id, subtitle, .. } => {
                let h = if subtitle.is_some() { s(40.0) } else { s(32.0) };
                targets.push(HitTarget {
                    id: *id,
                    menu_id: *id,
                    is_slider: false,
                    is_seek: false,
                    is_text: false,
                    rect: RECT {
                        left: pad,
                        top: y,
                        right: pad + content_w,
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
                let rects = app_row_targets(pad, pad + content_w, y, dpi);
                for (id, rect) in [Some(*menu_id), Some(*pin_id), *close_id]
                    .into_iter()
                    .zip(rects)
                {
                    if let Some(id) = id {
                        targets.push(HitTarget {
                            id,
                            rect,
                            menu_id: *menu_id,
                            is_slider: false,
                    is_seek: false,
                    is_text: false,
                        });
                    }
                }
                y += s(42.0) + s(2.0);
            }
            FlyoutBlock::ButtonBar(buttons) => {
                let h = s(31.0);
                let count = buttons.len().max(1) as i32;
                let gap = s(6.0);
                let bw = ((content_w - gap * (count - 1)) / count).max(s(40.0));
                for (i, (id, _, _)) in buttons.iter().enumerate() {
                    let left = pad + (i as i32) * (bw + gap);
                    targets.push(HitTarget {
                        id: *id,
                        menu_id: *id,
                        is_slider: false,
                    is_seek: false,
                    is_text: false,
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
            // ---- C2 blocks ----
            FlyoutBlock::SeekSlider { id, .. } => {
                let track_top = y + s(20.0);
                targets.push(HitTarget {
                    id: *id,
                    menu_id: *id,
                    is_slider: true,
                    is_seek: true,
                    is_text: false,
                    rect: RECT {
                        left: pad,
                        top: track_top,
                        right: pad + content_w,
                        bottom: track_top + s(18.0),
                    },
                });
                y += s(44.0);
            }
            FlyoutBlock::TextInput { id, .. } => {
                let box_top = y + s(17.0);
                targets.push(HitTarget {
                    id: *id,
                    menu_id: *id,
                    is_slider: false,
                    is_seek: false,
                    is_text: true,
                    rect: RECT {
                        left: pad,
                        top: box_top,
                        right: pad + content_w,
                        bottom: box_top + s(30.0),
                    },
                });
                y += s(53.0);
            }
            FlyoutBlock::Chart { series, .. } => {
                // Header + plot + one legend row (regardless of series count).
                let _ = series;
                y += s(80.0);
            }
            FlyoutBlock::Image { size_dips, .. } => {
                y += s(size_dips.clamp(24.0, 160.0)) + s(8.0);
            }
            FlyoutBlock::IconBar { buttons } => {
                let bw = s(34.0);
                let gap = s(6.0);
                for (i, (id, _, _, _)) in buttons.iter().enumerate() {
                    let left = pad + (i as i32) * (bw + gap);
                    if left + bw > pad + content_w {
                        break;
                    }
                    targets.push(HitTarget {
                        id: *id,
                        menu_id: *id,
                        is_slider: false,
                        is_seek: false,
                        is_text: false,
                        rect: RECT {
                            left,
                            top: y,
                            right: left + bw,
                            bottom: y + s(44.0),
                        },
                    });
                }
                y += s(44.0) + s(6.0);
            }
            FlyoutBlock::Separator => y += s(12.0),
            FlyoutBlock::Note(_) => y += s(22.0),
        }
    }
    (content_top_px, (y - content_top_px).max(s(24.0)), targets)
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

/// Shows an interactive flyout that updates in place on clicks and slider drags (C# WPF parity).
pub fn show_flyout_live(
    tag: &str,
    owner_hwnd: usize,
    is_dark: bool,
    anchor_center_x_px: i32,
    anchor_bottom_y_px: i32,
    spec: FlyoutSpec,
    mut on_live: impl FnMut(LiveEvent) -> LiveOutcome,
) {
    let _ = show_flyout_impl(
        tag,
        owner_hwnd,
        is_dark,
        anchor_center_x_px,
        anchor_bottom_y_px,
        spec,
        None,
        Some(&mut on_live),
    );
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
    show_flyout_impl(
        tag,
        owner_hwnd,
        is_dark,
        anchor_center_x_px,
        anchor_bottom_y_px,
        spec,
        on_secondary,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn show_flyout_impl<'a>(
    tag: &str,
    owner_hwnd: usize,
    is_dark: bool,
    anchor_center_x_px: i32,
    anchor_bottom_y_px: i32,
    spec: FlyoutSpec,
    on_secondary: Option<&'a mut dyn FnMut(u32) -> bool>,
    on_live: Option<&'a mut dyn FnMut(LiveEvent) -> LiveOutcome>,
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
    let pad = s(14.0);
    let (content_top_px, content_h_px, targets) = layout_flyout(&spec, width_px, dpi);
    let max_h_px = s(MAX_FLYOUT_H_DIPS);
    let height_px = (content_top_px + content_h_px + pad).min(max_h_px);
    let viewport_h_px = (height_px - content_top_px - pad).max(s(40.0));

    let is_tray = tag == "tray";
    let (scr_w, scr_h) = win32::primary_screen_px();
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let screen = if unsafe {
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

    let animate = win32::client_area_animations_enabled();
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
        on_live,
        content_top_px,
        viewport_h_px,
        content_h_px,
        scroll_y: 0,
        targets,
        hovered_id: None,
        dragging_slider: None,
        editing: None,
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
            0x00CC_0020,
        );
    }
}

/// `m:ss` caption for `FlyoutBlock::SeekSlider`.
fn format_clock(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Decodes an optional cover-art / flag file into the BGRA format used by `draw_bgra_icon`.
fn draw_cover_image(hdc: HDC, path: &str, bg_rgb: u32, dst_x: i32, dst_y: i32, dst_sz: i32) {
    let Ok(img) = image::open(path) else {
        return;
    };
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    if w == 0 || h == 0 {
        return;
    }
    let mut bgra = Vec::with_capacity(w as usize * h as usize * 4);
    for px in rgba.as_raw().chunks_exact(4) {
        bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    draw_bgra_icon(
        hdc,
        &FlyoutIcon {
            width: w,
            height: h,
            bgra,
        },
        bg_rgb,
        dst_x,
        dst_y,
        dst_sz,
    );
}

impl FlyoutState<'_> {
    fn apply_live_spec(&mut self, next: FlyoutSpec) {
        let mut client = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut client) };
        let width_px = client.right.max(1);
        let (ct, ch, targets) = layout_flyout(&next, width_px, self.dpi);
        self.spec = next;
        self.content_top_px = ct;
        self.content_h_px = ch;
        self.targets = targets;
        self.scroll_y = self.scroll_y.clamp(0, self.max_scroll());
        unsafe {
            InvalidateRect(self.hwnd, std::ptr::null(), 0);
        }
    }

    fn dispatch_live(&mut self, event: LiveEvent) -> bool {
        let outcome = if let Some(cb) = self.on_live.as_deref_mut() {
            cb(event)
        } else {
            return false;
        };
        match outcome {
            LiveOutcome::Update(spec) => self.apply_live_spec(spec),
            LiveOutcome::Close => self.begin_close(),
        }
        true
    }

    /// Focus a C2 `TextInput` in place; the buffer starts from the block's current value.
    fn begin_editing(&mut self, id: u32) {
        let initial = self.spec.blocks.iter().find_map(|block| match block {
            FlyoutBlock::TextInput { id: bid, value, .. } if *bid == id => Some(value.clone()),
            _ => None,
        });
        if let Some(value) = initial {
            self.editing = Some((id, value));
            unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
        }
    }

    /// Cancel editing without emitting an event.
    fn cancel_editing(&mut self) {
        if self.editing.take().is_some() {
            unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
        }
    }

    /// Enter a printable character into the active C2 `TextInput` editor.
    fn editing_char(&mut self, ch: char) {
        let Some(id) = self.editing.as_ref().map(|(id, _)| *id) else {
            return;
        };
        let numeric = self.spec.blocks.iter().any(|block| {
            matches!(
                block,
                FlyoutBlock::TextInput { id: bid, numeric: true, .. } if *bid == id
            )
        });
        if numeric && !ch.is_ascii_digit() && ch != '.' && ch != '-' {
            return;
        }
        if let Some((_, buf)) = self.editing.as_mut()
            && buf.len() < 32
            && !ch.is_control()
        {
            buf.push(ch);
            unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
        }
    }

    /// Backspace in the active C2 `TextInput` editor.
    fn editing_backspace(&mut self) {
        if let Some((_, buf)) = self.editing.as_mut()
            && buf.pop().is_some()
        {
            unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
        }
    }

    /// Commit the active C2 `TextInput` as `LiveEvent::TextCommit` (clamped for numeric fields).
    fn commit_editing(&mut self) {
        let Some((id, mut buf)) = self.editing.take() else {
            return;
        };
        let bounds = self.spec.blocks.iter().find_map(|block| match block {
            FlyoutBlock::TextInput {
                id: bid,
                numeric,
                min,
                max,
                ..
            } if *bid == id => Some((*numeric, *min, *max)),
            _ => None,
        });
        if let Some((true, min, max)) = bounds
            && let Ok(mut value) = buf.parse::<f64>()
        {
            value = value.clamp(min, max);
            buf = if value.fract() == 0.0 {
                format!("{value:.0}")
            } else {
                format!("{value}")
            };
        }
        unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
        let handled = self.dispatch_live(LiveEvent::TextCommit { id, text: buf });
        if !handled {
            self.selected = Some(FlyoutAction::Primary(id));
            self.begin_close();
        }
    }

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

    fn hit_at(&self, x: i32, y: i32) -> Option<HitTarget> {
        if y < self.content_top_px || y >= self.content_top_px + self.viewport_h_px {
            return None;
        }
        let cy = y + self.scroll_y;
        self.targets
            .iter()
            .find(|t| pt_in_rect(&t.rect, x, cy))
            .copied()
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
        let pad = s(14.0);
        let right_pad = pad + s(4.0);
        let radius = s(8.0);
        let mut client = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut client) };
        let w = client.right;
        let content_w = (w - pad - right_pad).max(s(120.0));
        let focused_id = self.keyboard_index.map(|index| self.targets[index].id);

        let pal = design::palette(self.is_dark);
        let bg = gdi_rgb(pal.surface);
        let card_bg = gdi_rgb(pal.elevated);
        let hover_bg = gdi_rgb(pal.hover);
        let border_col = gdi_rgb(pal.border_rgb);
        let accent = gdi_rgb(pal.widget_accent);
        let sel_fill = gdi_rgb(pal.selected_fill);
        let sel_stroke = gdi_rgb(pal.selected_stroke);
        let sel_fg = gdi_rgb(pal.selected_fg);
        let text_fg = gdi_rgb(pal.text_primary);
        let muted_fg = gdi_rgb(pal.text_secondary);
        let danger_bg = gdi_rgb(pal.danger_hover_bg);
        let danger_fg = gdi_rgb(pal.danger_fg);
        let sep_col = gdi_rgb(pal.sep_rgb);
        let scroll_col = gdi_rgb(pal.text_secondary);
        let track_col = if self.is_dark {
            rgb(0x3b, 0x44, 0x55)
        } else {
            rgb(0xdc, 0xe1, 0xe8)
        };

        unsafe {
            let bg_brush = CreateSolidBrush(bg);
            FillRect(hdc, &ps.rcPaint, bg_brush);
            DeleteObject(bg_brush as _);

            // Top specular hairline for crisp Apple/Raycast surface elevation.
            let spec_pen = CreatePen(PS_SOLID, 1, gdi_rgb(pal.specular_top));
            let prev_pen = SelectObject(hdc, spec_pen as _);
            MoveToEx(hdc, s(8.0), 0, std::ptr::null_mut());
            LineTo(hdc, (w - s(8.0)).max(s(8.0)), 0);
            SelectObject(hdc, prev_pen);
            DeleteObject(spec_pen as _);

            let fname = wide("Segoe UI Variable Text");
            let mk_font = |pt: f32, weight: i32| {
                CreateFontW(
                    -s(pt),
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    4, // ANTIALIASED_QUALITY (grayscale AA, zero RGB fringing)
                    0,
                    fname.as_ptr(),
                )
            };
            let title_font = mk_font(if self.keyboard_navigation { 16.0 } else { 14.0 }, 600);
            let body_font = mk_font(12.0, 500);
            let bold_font = mk_font(12.0, 600);
            let small_font = mk_font(11.0, 400);
            let tiny_font = mk_font(10.0, 500);
            SetBkMode(hdc, TRANSPARENT as i32);

            let old_f = SelectObject(hdc, title_font as _);
            if !self.spec.title.is_empty() {
                SetTextColor(hdc, text_fg);
                let mut hrc = RECT {
                    left: pad,
                    top: pad,
                    right: w - pad,
                    bottom: pad + s(22.0),
                };
                draw_gdi_text(
                    hdc,
                    &self.spec.title,
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
                    draw_gdi_text(
                        hdc,
                        sub,
                        &mut src,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                    );
                }
            }

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
                    FlyoutBlock::Hero {
                        value,
                        caption,
                        centered,
                        large,
                        right_meta,
                    } => {
                        let pt = if *large { 40.0 } else { 27.0 };
                        let val_h = if *large { s(46.0) } else { s(32.0) };
                        let hero_font = mk_font(pt, 700);
                        let prev_font = SelectObject(hdc, hero_font as _);
                        SetTextColor(hdc, text_fg);
                        let mut hrc = RECT {
                            left: pad,
                            top: y,
                            right: w - right_pad,
                            bottom: y + val_h,
                        };
                        let align = if *centered { DT_CENTER } else { DT_LEFT };
                        draw_gdi_text(hdc, value, &mut hrc, align | DT_VCENTER | DT_SINGLELINE);
                        SelectObject(hdc, prev_font);
                        DeleteObject(hero_font as _);

                        if let Some(meta) = right_meta {
                            SelectObject(hdc, small_font as _);
                            SetTextColor(hdc, muted_fg);
                            let mut mrc = RECT {
                                left: pad + content_w / 2,
                                top: y + s(4.0),
                                right: w - right_pad,
                                bottom: y + s(22.0),
                            };
                            draw_gdi_text(
                                hdc,
                                meta,
                                &mut mrc,
                                DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                        }
                        y += val_h + s(2.0);
                        if let Some(cap) = caption {
                            SelectObject(hdc, small_font as _);
                            SetTextColor(hdc, muted_fg);
                            let mut crc = RECT {
                                left: pad,
                                top: y,
                                right: w - right_pad,
                                bottom: y + s(16.0),
                            };
                            draw_gdi_text(
                                hdc,
                                cap,
                                &mut crc,
                                align | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                            y += s(20.0);
                        } else {
                            y += s(4.0);
                        }
                    }
                    FlyoutBlock::CalendarGrid {
                        year,
                        month,
                        today,
                        selected,
                        prev_year_id,
                        prev_month_id,
                        title_id,
                        next_month_id,
                        next_year_id,
                        day_id_base,
                    } => {
                        let nav_h = s(28.0);
                        let btn_w = s(26.0);
                        let gap = s(2.0);
                        let right = pad + content_w;
                        let nav_btns = [
                            (*prev_year_id, "«", pad, pad + btn_w),
                            (
                                *prev_month_id,
                                "‹",
                                pad + btn_w + gap,
                                pad + btn_w * 2 + gap,
                            ),
                            (
                                *next_month_id,
                                "›",
                                right - btn_w * 2 - gap,
                                right - btn_w - gap,
                            ),
                            (*next_year_id, "»", right - btn_w, right),
                        ];
                        SelectObject(hdc, bold_font as _);
                        for (id, glyph, l, r) in nav_btns {
                            if self.hovered_id == Some(id) {
                                fill_round_rect(hdc, l, y, r, y + nav_h, s(7.0), hover_bg);
                            }
                            SetTextColor(hdc, text_fg);
                            let mut rc = RECT {
                                left: l,
                                top: y,
                                right: r,
                                bottom: y + nav_h,
                            };
                            draw_gdi_text(
                                hdc,
                                glyph,
                                &mut rc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                            );
                        }
                        let tl = pad + btn_w * 2 + gap * 2;
                        let tr = right - btn_w * 2 - gap * 2;
                        if self.hovered_id == Some(*title_id) {
                            fill_round_rect(hdc, tl, y, tr, y + nav_h, s(7.0), hover_bg);
                        }
                        SetTextColor(hdc, text_fg);
                        let mut title_rc = RECT {
                            left: tl,
                            top: y,
                            right: tr,
                            bottom: y + nav_h,
                        };
                        let title_str = format!("{} {}", telemetry::month_name(*month), year);
                        draw_gdi_text(
                            hdc,
                            &title_str,
                            &mut title_rc,
                            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                        );
                        y += nav_h + s(8.0);

                        // Weekday headers (Monday first, matching European/Turkish calendar convention)
                        let cell_w = content_w / 7;
                        SelectObject(hdc, tiny_font as _);
                        SetTextColor(hdc, muted_fg);
                        const DOWS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
                        for (i, dow) in DOWS.iter().enumerate() {
                            let mut rc = RECT {
                                left: pad + (i as i32) * cell_w,
                                top: y,
                                right: pad + ((i as i32) + 1) * cell_w,
                                bottom: y + s(18.0),
                            };
                            draw_gdi_text(
                                hdc,
                                dow,
                                &mut rc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                            );
                        }
                        y += s(20.0);

                        let first_wd = telemetry::weekday_of(*year, *month, 1);
                        let mon_offset = ((first_wd + 6) % 7) as i32; // 0=Mon..6=Sun
                        let prev_m = if *month == 1 { 12 } else { *month - 1 };
                        let prev_y = if *month == 1 { *year - 1 } else { *year };
                        let prev_days = telemetry::days_in_month(prev_y, prev_m) as i32;
                        let cur_days = telemetry::days_in_month(*year, *month) as i32;
                        let next_m = if *month == 12 { 1 } else { *month + 1 };
                        let next_y = if *month == 12 { *year + 1 } else { *year };
                        let cell_h = s(31.0);
                        let cell_r = s(7.0);
                        let dim_fg = if self.is_dark {
                            rgb(0x63, 0x6c, 0x7c)
                        } else {
                            rgb(0xb0, 0xb5, 0xbe)
                        };

                        for cell in 0..42i32 {
                            let row = cell / 7;
                            let col = cell % 7;
                            let d_idx = cell - mon_offset + 1;
                            let (cy, cm, cd, in_month) = if d_idx < 1 {
                                (prev_y, prev_m, (prev_days + d_idx) as u32, false)
                            } else if d_idx > cur_days {
                                (next_y, next_m, (d_idx - cur_days) as u32, false)
                            } else {
                                (*year, *month, d_idx as u32, true)
                            };
                            let is_today = (cy, cm, cd) == *today;
                            let is_sel = selected.is_some_and(|s| s == (cy, cm, cd)) && !is_today;
                            let id = day_id_base + cell as u32;
                            let is_hov = self.hovered_id == Some(id);

                            let l = pad + col * cell_w + s(1.5);
                            let t = y + row * cell_h + s(1.5);
                            let r = pad + (col + 1) * cell_w - s(1.5);
                            let b = y + (row + 1) * cell_h - s(1.5);

                            let cell_rc = RECT {
                                left: l,
                                top: t,
                                right: r,
                                bottom: b,
                            };
                            if is_today {
                                fill_round_rect_bordered(
                                    hdc, cell_rc, cell_r, sel_fill, sel_stroke,
                                );
                            } else if is_sel {
                                fill_round_rect_bordered(
                                    hdc, cell_rc, cell_r, hover_bg, border_col,
                                );
                            } else if is_hov {
                                fill_round_rect(hdc, l, t, r, b, cell_r, hover_bg);
                            }

                            SelectObject(
                                hdc,
                                if is_today || is_sel {
                                    bold_font as _
                                } else {
                                    body_font as _
                                },
                            );
                            SetTextColor(
                                hdc,
                                if is_today {
                                    sel_fg
                                } else if in_month {
                                    text_fg
                                } else {
                                    dim_fg
                                },
                            );
                            let mut rc = RECT {
                                left: l,
                                top: t,
                                right: r,
                                bottom: b,
                            };
                            let ds = cd.to_string();
                            draw_gdi_text(
                                hdc,
                                &ds,
                                &mut rc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                            );
                        }
                        y += 6 * cell_h + s(4.0);
                    }
                    FlyoutBlock::AudioSliderSection {
                        label,
                        device_name,
                        value_text,
                        ratio,
                        muted,
                        mute_id,
                        slider_id,
                    } => {
                        // 1. Heading row (`Output` left, `67%` right)
                        SelectObject(hdc, bold_font as _);
                        SetTextColor(hdc, text_fg);
                        let mut hrc = RECT {
                            left: pad,
                            top: y,
                            right: pad + content_w,
                            bottom: y + s(20.0),
                        };
                        draw_gdi_text(hdc, label, &mut hrc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        draw_gdi_text(
                            hdc,
                            value_text,
                            &mut hrc,
                            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                        );

                        // 2. Device box + 32x32 Mute button
                        let dev_top = y + s(24.0);
                        let dev_h = s(32.0);
                        let mute_w = s(32.0);
                        let dev_r = pad + content_w - mute_w - s(8.0);
                        fill_round_rect_bordered(
                            hdc,
                            RECT {
                                left: pad,
                                top: dev_top,
                                right: dev_r,
                                bottom: dev_top + dev_h,
                            },
                            radius,
                            card_bg,
                            border_col,
                        );
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, text_fg);
                        let mut drc = RECT {
                            left: pad + s(10.0),
                            top: dev_top,
                            right: dev_r - s(8.0),
                            bottom: dev_top + dev_h,
                        };
                        draw_gdi_text(
                            hdc,
                            device_name,
                            &mut drc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );

                        let mute_l = pad + content_w - mute_w;
                        let mute_hov = self.hovered_id == Some(*mute_id);
                        let (m_bg, m_bd, m_fg) = if *muted {
                            (danger_bg, gdi_rgb(pal.danger), danger_fg)
                        } else if mute_hov {
                            (hover_bg, border_col, text_fg)
                        } else {
                            (card_bg, border_col, muted_fg)
                        };
                        fill_round_rect_bordered(
                            hdc,
                            RECT {
                                left: mute_l,
                                top: dev_top,
                                right: mute_l + mute_w,
                                bottom: dev_top + dev_h,
                            },
                            radius,
                            m_bg,
                            m_bd,
                        );
                        let glyph_font = CreateFontW(
                            -s(14.0),
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
                            4,
                            0,
                            wide("Segoe Fluent Icons").as_ptr(),
                        );
                        let prev_font = SelectObject(hdc, glyph_font as _);
                        SetTextColor(hdc, m_fg);
                        let mut mrc = RECT {
                            left: mute_l,
                            top: dev_top,
                            right: mute_l + mute_w,
                            bottom: dev_top + dev_h,
                        };
                        draw_gdi_text(
                            hdc,
                            if *muted { "\u{E74F}" } else { "\u{E767}" },
                            &mut mrc,
                            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                        );
                        SelectObject(hdc, prev_font);
                        DeleteObject(glyph_font as _);

                        // 3. C# `WidgetSlider`: 5px track + Aurora teal fill + 10px circular thumb
                        let slider_top = dev_top + dev_h + s(8.0);
                        let mid_y = slider_top + s(10.0);
                        let tr_h = s(5.0).max(4);
                        fill_round_rect(
                            hdc,
                            pad,
                            mid_y - tr_h / 2,
                            pad + content_w,
                            mid_y + tr_h / 2,
                            tr_h,
                            track_col,
                        );
                        let r_clamped = if *muted { 0.0 } else { ratio.clamp(0.0, 1.0) };
                        let fill_w = (content_w as f32 * r_clamped).round() as i32;
                        if fill_w > s(2.0) {
                            fill_round_rect(
                                hdc,
                                pad,
                                mid_y - tr_h / 2,
                                pad + fill_w,
                                mid_y + tr_h / 2,
                                tr_h,
                                accent,
                            );
                        }
                        let thumb_r = if self.hovered_id == Some(*slider_id)
                            || self
                                .dragging_slider
                                .is_some_and(|(sid, _, _)| sid == *slider_id)
                        {
                            s(6.0)
                        } else {
                            s(5.0)
                        };
                        let tx = (pad + fill_w).clamp(pad + thumb_r, pad + content_w - thumb_r);
                        let tb = CreateSolidBrush(text_fg);
                        let tp = CreatePen(PS_SOLID, 1, text_fg);
                        let ob = SelectObject(hdc, tb as _);
                        let op = SelectObject(hdc, tp as _);
                        Ellipse(
                            hdc,
                            tx - thumb_r,
                            mid_y - thumb_r,
                            tx + thumb_r,
                            mid_y + thumb_r,
                        );
                        SelectObject(hdc, ob);
                        SelectObject(hdc, op);
                        DeleteObject(tb as _);
                        DeleteObject(tp as _);

                        y = slider_top + s(24.0);
                    }
                    FlyoutBlock::Sparkline {
                        label,
                        value_text,
                        detail_text,
                        samples,
                        color,
                    } => {
                        SelectObject(hdc, body_font as _);
                        SetTextColor(hdc, text_fg);
                        let mut lrc = RECT {
                            left: pad,
                            top: y,
                            right: pad + content_w,
                            bottom: y + s(18.0),
                        };
                        draw_gdi_text(hdc, label, &mut lrc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                        let val_full = match detail_text {
                            Some(d) => format!("{d}  {value_text}"),
                            None => value_text.clone(),
                        };
                        SelectObject(hdc, bold_font as _);
                        SetTextColor(hdc, gdi_rgb(*color));
                        draw_gdi_text(
                            hdc,
                            &val_full,
                            &mut lrc,
                            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                        );

                        // Vector sparkline matching C# `TelemetrySparkline.cs` (height 22px)
                        let plot_top = y + s(21.0);
                        let plot_h = s(22.0);
                        let plot_bot = plot_top + plot_h;
                        let base_pen = CreatePen(PS_SOLID, 1, sep_col);
                        let op = SelectObject(hdc, base_pen as _);
                        MoveToEx(hdc, pad, plot_bot, std::ptr::null_mut());
                        LineTo(hdc, pad + content_w, plot_bot);
                        SelectObject(hdc, op);
                        DeleteObject(base_pen as _);

                        if !samples.is_empty() {
                            let line_pen = CreatePen(
                                PS_SOLID,
                                (1.5 * dpi).round().max(1.0) as i32,
                                gdi_rgb(*color),
                            );
                            let op = SelectObject(hdc, line_pen as _);
                            let n = samples.len();
                            let py = |v: f32| {
                                plot_bot
                                    - (v.clamp(0.0, 1.0) * (plot_h - s(3.0)) as f32).round() as i32
                            };
                            if n == 1 {
                                let y0 = py(samples[0]);
                                MoveToEx(hdc, pad, y0, std::ptr::null_mut());
                                LineTo(hdc, pad + content_w, y0);
                            } else {
                                let step = content_w as f32 / (n - 1) as f32;
                                MoveToEx(hdc, pad, py(samples[0]), std::ptr::null_mut());
                                for (i, &val) in samples.iter().enumerate().skip(1) {
                                    let px = pad + (i as f32 * step).round() as i32;
                                    LineTo(hdc, px, py(val));
                                }
                            }
                            SelectObject(hdc, op);
                            DeleteObject(line_pen as _);
                        }
                        y += s(48.0);
                    }
                    FlyoutBlock::MetricTiles(tiles) => {
                        let rows = tiles.len().div_ceil(2) as i32;
                        let tile_h = s(44.0);
                        let gap = s(6.0);
                        let tw = ((content_w - gap) / 2).max(s(80.0));
                        for (i, (lbl, val)) in tiles.iter().enumerate() {
                            let col = (i % 2) as i32;
                            let row = (i / 2) as i32;
                            let l = pad + col * (tw + gap);
                            let t = y + row * (tile_h + gap);
                            fill_round_rect(hdc, l, t, l + tw, t + tile_h, radius, card_bg);
                            SelectObject(hdc, tiny_font as _);
                            SetTextColor(hdc, muted_fg);
                            let mut lrc = RECT {
                                left: l + s(8.0),
                                top: t + s(5.0),
                                right: l + tw - s(8.0),
                                bottom: t + s(20.0),
                            };
                            draw_gdi_text(
                                hdc,
                                lbl,
                                &mut lrc,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                            SelectObject(hdc, bold_font as _);
                            SetTextColor(hdc, text_fg);
                            let mut vrc = RECT {
                                left: l + s(8.0),
                                top: t + s(21.0),
                                right: l + tw - s(8.0),
                                bottom: t + tile_h - s(5.0),
                            };
                            draw_gdi_text(
                                hdc,
                                val,
                                &mut vrc,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                        }
                        y += rows * tile_h + (rows - 1).max(0) * gap + s(6.0);
                    }
                    FlyoutBlock::BadgeRow {
                        id,
                        code,
                        title,
                        active,
                    } => {
                        let h = s(34.0);
                        let hovered = self.hovered_id == Some(*id);
                        let row_bg = if hovered {
                            hover_bg
                        } else if *active {
                            card_bg
                        } else {
                            bg
                        };
                        if hovered || *active {
                            fill_round_rect(hdc, pad, y, pad + content_w, y + h, radius, row_bg);
                        }
                        let bw = s(34.0);
                        let bh = s(22.0);
                        let bx = pad + s(8.0);
                        let by = y + (h - bh) / 2;
                        fill_round_rect(
                            hdc,
                            bx,
                            by,
                            bx + bw,
                            by + bh,
                            s(5.0),
                            if *active { sel_fill } else { card_bg },
                        );
                        SelectObject(hdc, bold_font as _);
                        SetTextColor(hdc, if *active { sel_fg } else { text_fg });
                        let mut brc = RECT {
                            left: bx,
                            top: by,
                            right: bx + bw,
                            bottom: by + bh,
                        };
                        draw_gdi_text(hdc, code, &mut brc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);

                        SelectObject(
                            hdc,
                            if *active {
                                bold_font as _
                            } else {
                                body_font as _
                            },
                        );
                        SetTextColor(hdc, text_fg);
                        let mut trc = RECT {
                            left: bx + bw + s(10.0),
                            top: y,
                            right: pad + content_w - s(8.0),
                            bottom: y + h,
                        };
                        draw_gdi_text(
                            hdc,
                            title,
                            &mut trc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );
                        y += h + s(4.0);
                    }
                    FlyoutBlock::StatusHeader {
                        title,
                        status_label,
                        status_color,
                        action_id,
                        action_label,
                    } => {
                        let h = s(28.0);
                        SelectObject(hdc, bold_font as _);
                        SetTextColor(hdc, text_fg);
                        let mut trc = RECT {
                            left: pad,
                            top: y,
                            right: pad + content_w - s(90.0),
                            bottom: y + h,
                        };
                        draw_gdi_text(
                            hdc,
                            title,
                            &mut trc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );

                        let right_edge = if let (Some(id), Some(lbl)) = (*action_id, action_label) {
                            let bw = s(64.0);
                            let bl = pad + content_w - bw;
                            let hovered = self.hovered_id == Some(id);
                            fill_round_rect_bordered(
                                hdc,
                                RECT {
                                    left: bl,
                                    top: y + s(2.0),
                                    right: pad + content_w,
                                    bottom: y + h - s(2.0),
                                },
                                s(6.0),
                                if hovered { hover_bg } else { card_bg },
                                border_col,
                            );
                            SelectObject(hdc, tiny_font as _);
                            SetTextColor(hdc, text_fg);
                            let mut brc = RECT {
                                left: bl,
                                top: y,
                                right: pad + content_w,
                                bottom: y + h,
                            };
                            draw_gdi_text(
                                hdc,
                                lbl,
                                &mut brc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                            );
                            bl - s(8.0)
                        } else {
                            pad + content_w
                        };

                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut src = RECT {
                            left: pad + content_w / 3,
                            top: y,
                            right: right_edge,
                            bottom: y + h,
                        };
                        draw_gdi_text(
                            hdc,
                            status_label,
                            &mut src,
                            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                        );
                        // Status dot left of status label
                        let dot_r = s(3.0).max(2);
                        let dot_x = right_edge - s(status_label.len() as f32 * 6.5 + 8.0);
                        let dot_y = y + h / 2;
                        let db = CreateSolidBrush(gdi_rgb(*status_color));
                        let dp = CreatePen(PS_SOLID, 1, gdi_rgb(*status_color));
                        let ob = SelectObject(hdc, db as _);
                        let op = SelectObject(hdc, dp as _);
                        Ellipse(
                            hdc,
                            dot_x - dot_r,
                            dot_y - dot_r,
                            dot_x + dot_r,
                            dot_y + dot_r,
                        );
                        SelectObject(hdc, ob);
                        SelectObject(hdc, op);
                        DeleteObject(db as _);
                        DeleteObject(dp as _);

                        y += h + s(6.0);
                    }
                    FlyoutBlock::ToggleGrid(tiles) => {
                        let rows = tiles.len().div_ceil(2) as i32;
                        let tile_h = s(48.0);
                        let gap = s(6.0);
                        let tw = ((content_w - gap) / 2).max(s(80.0));
                        for (i, (id, title, state_lbl, active)) in tiles.iter().enumerate() {
                            let col = (i % 2) as i32;
                            let row = (i / 2) as i32;
                            let left = pad + col * (tw + gap);
                            let top = y + row * (tile_h + gap);
                            let hovered = self.hovered_id == Some(*id);
                            let (fill, bd) = if *active {
                                (sel_fill, sel_stroke)
                            } else if hovered {
                                (hover_bg, border_col)
                            } else {
                                (card_bg, border_col)
                            };
                            fill_round_rect_bordered(
                                hdc,
                                RECT {
                                    left,
                                    top,
                                    right: left + tw,
                                    bottom: top + tile_h,
                                },
                                radius,
                                fill,
                                bd,
                            );
                            SelectObject(hdc, bold_font as _);
                            SetTextColor(hdc, if *active { sel_fg } else { text_fg });
                            let mut trc = RECT {
                                left: left + s(10.0),
                                top: top + s(6.0),
                                right: left + tw - s(8.0),
                                bottom: top + s(24.0),
                            };
                            draw_gdi_text(
                                hdc,
                                title,
                                &mut trc,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                            SelectObject(hdc, small_font as _);
                            SetTextColor(hdc, muted_fg);
                            let mut src = RECT {
                                left: left + s(10.0),
                                top: top + s(25.0),
                                right: left + tw - s(8.0),
                                bottom: top + tile_h - s(6.0),
                            };
                            draw_gdi_text(
                                hdc,
                                state_lbl,
                                &mut src,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                        }
                        y += rows * tile_h + (rows - 1).max(0) * gap + s(6.0);
                    }
                    FlyoutBlock::StatRow { label, value } => {
                        let h = s(24.0);
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut rc = RECT {
                            left: pad,
                            top: y,
                            right: pad + content_w,
                            bottom: y + h,
                        };
                        draw_gdi_text(
                            hdc,
                            label,
                            &mut rc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );
                        SelectObject(hdc, bold_font as _);
                        SetTextColor(hdc, text_fg);
                        draw_gdi_text(hdc, value, &mut rc, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
                        y += h;
                    }
                    FlyoutBlock::ProgressBar {
                        label,
                        value_text,
                        ratio,
                        tone,
                    } => {
                        let h = s(34.0);
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut rc = RECT {
                            left: pad,
                            top: y,
                            right: pad + content_w,
                            bottom: y + s(16.0),
                        };
                        draw_gdi_text(
                            hdc,
                            label,
                            &mut rc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );
                        SetTextColor(hdc, text_fg);
                        draw_gdi_text(
                            hdc,
                            value_text,
                            &mut rc,
                            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                        );

                        let bar_top = y + s(21.0);
                        let bar_h = s(6.0);
                        fill_round_rect(
                            hdc,
                            pad,
                            bar_top,
                            pad + content_w,
                            bar_top + bar_h,
                            s(3.0),
                            track_col,
                        );

                        let fill_w = (content_w as f32 * ratio.clamp(0.0, 1.0)).round() as i32;
                        if fill_w > s(3.0) {
                            let meter_rgb = match tone {
                                BarTone::Accent => pal.widget_blue,
                                BarTone::Usage => design::meter_color(&pal, *ratio),
                                BarTone::Level => design::level_color(&pal, *ratio),
                            };
                            fill_round_rect(
                                hdc,
                                pad,
                                bar_top,
                                pad + fill_w,
                                bar_top + bar_h,
                                s(3.0),
                                gdi_rgb(meter_rgb),
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
                        fill_round_rect(hdc, pad, y, pad + content_w, y + h, radius, fill);

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
                            right: pad + content_w - s(10.0),
                            bottom: y + if subtitle.is_some() { s(21.0) } else { h },
                        };
                        draw_gdi_text(
                            hdc,
                            title,
                            &mut trc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );

                        if let Some(sub) = subtitle {
                            SelectObject(hdc, small_font as _);
                            SetTextColor(hdc, muted_fg);
                            let mut src = RECT {
                                left: text_left,
                                top: y + s(20.0),
                                right: pad + content_w - s(10.0),
                                bottom: y + h - s(3.0),
                            };
                            draw_gdi_text(
                                hdc,
                                sub,
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
                                right: pad + content_w - s(10.0),
                                bottom: y + h,
                            };
                            draw_gdi_text(hdc, b, &mut brc, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
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
                        let [app, pin, close] = app_row_targets(pad, pad + content_w, y, dpi);
                        let row_hovered = self.hovered_id == Some(*id)
                            || focused_id == Some(*id)
                            || self.hovered_id == Some(*pin_id)
                            || (*close_id).is_some_and(|cid| self.hovered_id == Some(cid));
                        // Transparent resting rows that softly illuminate on hover (clean Raycast/macOS list).
                        let app_fill = if row_hovered { hover_bg } else { bg };
                        if row_hovered {
                            fill_round_rect(
                                hdc,
                                app.left,
                                y,
                                close.right,
                                app.bottom,
                                s(8.0),
                                hover_bg,
                            );
                        }
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
                        draw_gdi_text(
                            hdc,
                            title,
                            &mut rect,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                        );

                        for (id, rect, icon, selected, fill, ink) in [
                            (Some(*pin_id), pin, MenuIcon::Pin, *pinned, sel_fill, sel_fg),
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
                                    s(8.0),
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
                        let h = s(31.0);
                        let count = buttons.len().max(1) as i32;
                        let gap = s(6.0);
                        let bw = ((content_w - gap * (count - 1)) / count).max(s(40.0));
                        for (i, (id, label, primary)) in buttons.iter().enumerate() {
                            let left = pad + (i as i32) * (bw + gap);
                            let hovered = self.hovered_id == Some(*id) || focused_id == Some(*id);
                            // Exact C# `WidgetFlyoutPrimaryButton` / `WidgetFlyoutActionButton`
                            let (fill, bd, fg) = if *primary {
                                (sel_fill, sel_stroke, text_fg)
                            } else if hovered {
                                (hover_bg, border_col, text_fg)
                            } else {
                                (card_bg, border_col, text_fg)
                            };
                            fill_round_rect_bordered(
                                hdc,
                                RECT {
                                    left,
                                    top: y,
                                    right: left + bw,
                                    bottom: y + h,
                                },
                                radius,
                                fill,
                                bd,
                            );

                            SelectObject(hdc, bold_font as _);
                            SetTextColor(hdc, fg);
                            let mut brc = RECT {
                                left,
                                top: y,
                                right: left + bw,
                                bottom: y + h,
                            };
                            draw_gdi_text(
                                hdc,
                                label,
                                &mut brc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                        }
                        y += h + s(6.0);
                    }
                    // ---- C2 blocks (WS-C contract) ----
                    FlyoutBlock::SeekSlider {
                        id,
                        label,
                        position_secs,
                        duration_secs,
                        ratio,
                    } => {
                        SelectObject(hdc, bold_font as _);
                        SetTextColor(hdc, text_fg);
                        let mut lrc = RECT {
                            left: pad,
                            top: y,
                            right: pad + content_w / 2,
                            bottom: y + s(18.0),
                        };
                        draw_gdi_text(hdc, label, &mut lrc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                        let times = format!(
                            "{} / {}",
                            format_clock(*position_secs),
                            format_clock(*duration_secs)
                        );
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut trc = RECT {
                            left: pad + content_w / 2,
                            top: y,
                            right: pad + content_w,
                            bottom: y + s(18.0),
                        };
                        draw_gdi_text(
                            hdc,
                            &times,
                            &mut trc,
                            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                        );

                        let track_top = y + s(20.0);
                        let mid_y = track_top + s(9.0);
                        let tr_h = s(5.0).max(4);
                        fill_round_rect(
                            hdc,
                            pad,
                            mid_y - tr_h / 2,
                            pad + content_w,
                            mid_y + tr_h / 2,
                            tr_h,
                            track_col,
                        );
                        let r = ratio.clamp(0.0, 1.0);
                        let fill_w = (content_w as f32 * r).round() as i32;
                        if fill_w > s(2.0) {
                            fill_round_rect(
                                hdc,
                                pad,
                                mid_y - tr_h / 2,
                                pad + fill_w,
                                mid_y + tr_h / 2,
                                tr_h,
                                accent,
                            );
                        }
                        let dragging = self
                            .dragging_slider
                            .is_some_and(|(sid, _, seek)| sid == *id && seek);
                        let thumb_r = if dragging || self.hovered_id == Some(*id) {
                            s(6.0)
                        } else {
                            s(5.0)
                        };
                        let tx = (pad + fill_w).clamp(pad + thumb_r, pad + content_w - thumb_r);
                        let tb = CreateSolidBrush(accent);
                        let tp = CreatePen(PS_SOLID, 1, accent);
                        let ob = SelectObject(hdc, tb as _);
                        let op = SelectObject(hdc, tp as _);
                        Ellipse(hdc, tx - thumb_r, mid_y - thumb_r, tx + thumb_r, mid_y + thumb_r);
                        SelectObject(hdc, ob);
                        SelectObject(hdc, op);
                        DeleteObject(tb as _);
                        DeleteObject(tp as _);
                        y += s(44.0);
                    }
                    FlyoutBlock::TextInput {
                        id,
                        label,
                        value,
                        numeric,
                        ..
                    } => {
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut lrc = RECT {
                            left: pad,
                            top: y,
                            right: pad + content_w,
                            bottom: y + s(16.0),
                        };
                        draw_gdi_text(
                            hdc,
                            label,
                            &mut lrc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );
                        let box_top = y + s(17.0);
                        let editing = self.editing.as_ref().is_some_and(|(eid, _)| eid == id);
                        fill_round_rect_bordered(
                            hdc,
                            RECT {
                                left: pad,
                                top: box_top,
                                right: pad + content_w,
                                bottom: box_top + s(30.0),
                            },
                            radius,
                            if editing { hover_bg } else { card_bg },
                            if editing { accent } else { border_col },
                        );
                        let shown = if editing {
                            let buf = self
                                .editing
                                .as_ref()
                                .map(|(_, b)| b.as_str())
                                .unwrap_or("");
                            format!("{buf}|")
                        } else {
                            value.clone()
                        };
                        SelectObject(hdc, body_font as _);
                        SetTextColor(hdc, if editing { text_fg } else { muted_fg });
                        let mut vrc = RECT {
                            left: pad + s(8.0),
                            top: box_top,
                            right: pad + content_w - s(8.0),
                            bottom: box_top + s(30.0),
                        };
                        draw_gdi_text(
                            hdc,
                            &shown,
                            &mut vrc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );
                        if *numeric {
                            SelectObject(hdc, tiny_font as _);
                            SetTextColor(hdc, muted_fg);
                            let mut hrc = RECT {
                                left: pad + content_w - s(38.0),
                                top: box_top,
                                right: pad + content_w - s(6.0),
                                bottom: box_top + s(30.0),
                            };
                            draw_gdi_text(
                                hdc,
                                "#",
                                &mut hrc,
                                DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                            );
                        }
                        y += s(53.0);
                    }
                    FlyoutBlock::Chart {
                        label,
                        series,
                        accent: series_accent,
                    } => {
                        SelectObject(hdc, bold_font as _);
                        SetTextColor(hdc, text_fg);
                        let mut lrc = RECT {
                            left: pad,
                            top: y,
                            right: pad + content_w,
                            bottom: y + s(16.0),
                        };
                        draw_gdi_text(
                            hdc,
                            label,
                            &mut lrc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );
                        let plot_top = y + s(18.0);
                        let plot_h = s(42.0);
                        let plot_bot = plot_top + plot_h;
                        let baseline = CreatePen(PS_SOLID, 1, sep_col);
                        let op = SelectObject(hdc, baseline as _);
                        MoveToEx(hdc, pad, plot_bot, std::ptr::null_mut());
                        LineTo(hdc, pad + content_w, plot_bot);
                        SelectObject(hdc, op);
                        DeleteObject(baseline as _);

                        let palette_colors = [
                            *series_accent,
                            pal.widget_blue,
                            pal.warning,
                            pal.success,
                            pal.danger,
                        ];
                        for (series_idx, (_, points)) in series.iter().enumerate() {
                            if points.is_empty() {
                                continue;
                            }
                            let peak = points.iter().copied().fold(0.0f32, f32::max).max(0.01);
                            let line = CreatePen(
                                PS_SOLID,
                                (1.5 * dpi).round().max(1.0) as i32,
                                gdi_rgb(palette_colors[series_idx % palette_colors.len()]),
                            );
                            let op = SelectObject(hdc, line as _);
                            let px = |i: usize| {
                                pad + if points.len() == 1 {
                                    content_w
                                } else {
                                    (i as f32 * content_w as f32 / (points.len() - 1) as f32)
                                        .round() as i32
                                }
                            };
                            let py = |v: f32| {
                                plot_bot
                                    - ((v / peak).clamp(0.0, 1.0)
                                        * (plot_h - s(3.0)) as f32)
                                        .round() as i32
                            };
                            MoveToEx(hdc, px(0), py(points[0]), std::ptr::null_mut());
                            for i in 1..points.len() {
                                LineTo(hdc, px(i), py(points[i]));
                            }
                            SelectObject(hdc, op);
                            DeleteObject(line as _);
                        }

                        let mut legend_x = pad;
                        SelectObject(hdc, tiny_font as _);
                        SetTextColor(hdc, muted_fg);
                        for (i, (name, _)) in series.iter().enumerate().take(3) {
                            let dash = CreateSolidBrush(gdi_rgb(
                                palette_colors[i % palette_colors.len()],
                            ));
                            fill_round_rect(
                                hdc,
                                legend_x,
                                plot_bot + s(4.0),
                                legend_x + s(8.0),
                                plot_bot + s(10.0),
                                s(2.0),
                                gdi_rgb(palette_colors[i % palette_colors.len()]),
                            );
                            DeleteObject(dash as _);
                            legend_x += s(12.0);
                            let mut nrc = RECT {
                                left: legend_x,
                                top: plot_bot + s(2.0),
                                right: (legend_x + s(80.0)).min(pad + content_w),
                                bottom: plot_bot + s(14.0),
                            };
                            draw_gdi_text(
                                hdc,
                                name,
                                &mut nrc,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                            legend_x += s(86.0);
                            if legend_x >= pad + content_w {
                                break;
                            }
                        }
                        y += s(80.0);
                    }
                    FlyoutBlock::Image {
                        glyph,
                        path,
                        size_dips,
                    } => {
                        let sz = s(size_dips.clamp(24.0, 160.0));
                        fill_round_rect(hdc, pad, y, pad + sz, y + sz, radius, card_bg);
                        if let Some(path) = path.as_deref().filter(|p| !p.is_empty()) {
                            draw_cover_image(hdc, path, card_bg, pad, y, sz);
                        } else {
                            let gfont = CreateFontW(
                                -((sz as f32 * 0.62) as i32),
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
                                4,
                                0,
                                wide("Segoe Fluent Icons").as_ptr(),
                            );
                            let prev_font = SelectObject(hdc, gfont as _);
                            SetTextColor(hdc, text_fg);
                            let gc = char::from_u32(u32::from(*glyph))
                                .unwrap_or('\u{E9AA}')
                                .to_string();
                            let mut grc = RECT {
                                left: pad,
                                top: y,
                                right: pad + sz,
                                bottom: y + sz,
                            };
                            draw_gdi_text(
                                hdc,
                                &gc,
                                &mut grc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                            );
                            SelectObject(hdc, prev_font);
                            DeleteObject(gfont as _);
                        }
                        y += sz + s(8.0);
                    }
                    FlyoutBlock::IconBar { buttons } => {
                        let bw = s(34.0);
                        let gap = s(6.0);
                        for (i, (id, label, glyph, active)) in buttons.iter().enumerate() {
                            let left = pad + (i as i32) * (bw + gap);
                            if left + bw > pad + content_w {
                                break;
                            }
                            let hot = self.hovered_id == Some(*id) || focused_id == Some(*id);
                            let (fill, bd, fg) = if *active {
                                (sel_fill, sel_stroke, sel_fg)
                            } else if hot {
                                (hover_bg, border_col, text_fg)
                            } else {
                                (card_bg, border_col, muted_fg)
                            };
                            fill_round_rect_bordered(
                                hdc,
                                RECT {
                                    left,
                                    top: y,
                                    right: left + bw,
                                    bottom: y + s(30.0),
                                },
                                radius,
                                fill,
                                bd,
                            );
                            let gfont = CreateFontW(
                                -s(14.0),
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
                                4,
                                0,
                                wide("Segoe Fluent Icons").as_ptr(),
                            );
                            let prev_font = SelectObject(hdc, gfont as _);
                            SetTextColor(hdc, fg);
                            let gc = char::from_u32(u32::from(*glyph))
                                .unwrap_or('\u{E9AA}')
                                .to_string();
                            let mut grc = RECT {
                                left,
                                top: y,
                                right: left + bw,
                                bottom: y + s(30.0),
                            };
                            draw_gdi_text(
                                hdc,
                                &gc,
                                &mut grc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                            );
                            SelectObject(hdc, prev_font);
                            DeleteObject(gfont as _);
                            SelectObject(hdc, tiny_font as _);
                            SetTextColor(hdc, if hot { text_fg } else { muted_fg });
                            let mut lrc = RECT {
                                left: left - s(4.0),
                                top: y + s(30.0),
                                right: left + bw + s(4.0),
                                bottom: y + s(44.0),
                            };
                            draw_gdi_text(
                                hdc,
                                label,
                                &mut lrc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                            );
                        }
                        y += s(44.0) + s(6.0);
                    }
                    FlyoutBlock::Separator => {
                        let pen = CreatePen(PS_SOLID, 1, sep_col);
                        let op = SelectObject(hdc, pen as _);
                        let sy = y + s(6.0);
                        MoveToEx(hdc, pad, sy, std::ptr::null_mut());
                        LineTo(hdc, pad + content_w, sy);
                        SelectObject(hdc, op);
                        DeleteObject(pen as _);
                        y += s(12.0);
                    }
                    FlyoutBlock::Note(text) => {
                        let h = s(22.0);
                        SelectObject(hdc, small_font as _);
                        SetTextColor(hdc, muted_fg);
                        let mut rc = RECT {
                            left: pad,
                            top: y,
                            right: pad + content_w,
                            bottom: y + h,
                        };
                        draw_gdi_text(
                            hdc,
                            text,
                            &mut rc,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                        );
                        y += h;
                    }
                }
            }
            if let Some(index) = self.keyboard_index
                && let Some(t) = self.targets.get(index)
            {
                let mut rect = t.rect;
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
            DeleteObject(bold_font as _);
            DeleteObject(small_font as _);
            DeleteObject(tiny_font as _);
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
            let cid = if state.hovered_id.is_some() || state.dragging_slider.is_some() {
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
            if let Some((sid, rect, seek)) = state.dragging_slider {
                let w = (rect.right - rect.left).max(1) as f32;
                let ratio = ((x - rect.left) as f32 / w).clamp(0.0, 1.0);
                let event = if seek {
                    LiveEvent::Seek { id: sid, ratio }
                } else {
                    LiveEvent::Slider { id: sid, ratio }
                };
                state.dispatch_live(event);
                return 0;
            }
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
            if state.dragging_slider.is_none() && state.hovered_id.take().is_some() {
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
        WM_LBUTTONDOWN => {
            if !state.closing {
                let x = (lparam as u32 & 0xffff) as i16 as i32;
                let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
                if let Some(target) = state.hit_at(x, y) {
                    if target.is_text {
                        // C2 TextInput: clicking the field starts in-place editing.
                        if state.editing.as_ref().is_none_or(|(id, _)| *id != target.id) {
                            state.commit_editing();
                        }
                        state.begin_editing(target.id);
                    } else if (target.is_slider || target.is_seek) && state.on_live.is_some() {
                        unsafe {
                            SetCapture(hwnd);
                        }
                        state.dragging_slider =
                            Some((target.id, target.rect, target.is_seek));
                        let w = (target.rect.right - target.rect.left).max(1) as f32;
                        let ratio = ((x - target.rect.left) as f32 / w).clamp(0.0, 1.0);
                        let event = if target.is_seek {
                            LiveEvent::Seek {
                                id: target.id,
                                ratio,
                            }
                        } else {
                            LiveEvent::Slider {
                                id: target.id,
                                ratio,
                            }
                        };
                        state.dispatch_live(event);
                    } else if state.editing.is_some() {
                        state.commit_editing();
                    }
                } else if state.editing.is_some() {
                    state.commit_editing();
                }
            }
            0
        }
        WM_LBUTTONUP => {
            if state.dragging_slider.take().is_some() {
                unsafe {
                    let _ = ReleaseCapture();
                }
                return 0;
            }
            if !state.closing {
                let x = (lparam as u32 & 0xffff) as i16 as i32;
                let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
                if let Some(target) = state.hit_at(x, y) {
                    if target.is_slider || target.is_text {
                        // Sliders commit on drag; text fields commit on Enter/Escape.
                    } else if state.on_live.is_some() {
                        state.dispatch_live(LiveEvent::Click(target.id));
                    } else {
                        state.selected = Some(FlyoutAction::Primary(target.id));
                        state.begin_close();
                    }
                } else if !state.keyboard_navigation && state.on_live.is_none() {
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
        WM_KEYDOWN if state.editing.is_some() && !state.closing => {
            match wparam as u16 {
                VK_RETURN => state.commit_editing(),
                VK_ESCAPE => state.cancel_editing(),
                VK_BACK => state.editing_backspace(),
                VK_TAB => {
                    state.commit_editing();
                    state.focus_next_target(unsafe { GetKeyState(VK_SHIFT as i32) } < 0);
                }
                _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
            0
        }
        WM_CHAR if state.editing.is_some() && !state.closing => {
            if let Some(ch) = char::from_u32(wparam as u32) {
                state.editing_char(ch);
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
            state.commit_editing();
            if !state.suppress_blur {
                state.begin_close();
            }
            0
        }
        WM_ACTIVATE if (wparam & 0xffff) == 0 => {
            state.commit_editing();
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
    use super::{
        DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FlyoutBlock, FlyoutSpec, LiveEvent,
        RECT, app_row_targets, draw_gdi_text, format_clock, layout_flyout, pt_in_rect,
    };
    use windows_sys::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC};

    #[test]
    fn draw_gdi_text_ignores_empty_strings_without_crashing() {
        unsafe {
            let hdc = CreateCompatibleDC(std::ptr::null_mut());
            assert!(!hdc.is_null());
            let mut rc = RECT {
                left: 0,
                top: 0,
                right: 120,
                bottom: 24,
            };
            draw_gdi_text(
                hdc,
                "",
                &mut rc,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
            draw_gdi_text(
                hdc,
                "Today",
                &mut rc,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
            DeleteDC(hdc);
        }
    }

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

    /// C2 field names/types are part of the WS-E↔WS-C contract; keep them constructible.
    #[test]
    fn c2_blocks_have_the_agreed_field_signatures() {
        let blocks = vec![
            FlyoutBlock::SeekSlider {
                id: 11,
                label: "Position".to_owned(),
                position_secs: 42,
                duration_secs: 180,
                ratio: 42.0 / 180.0,
            },
            FlyoutBlock::TextInput {
                id: 12,
                label: "Minutes".to_owned(),
                value: "5".to_owned(),
                numeric: true,
                min: 1.0,
                max: 1440.0,
            },
            FlyoutBlock::Chart {
                label: "Network".to_owned(),
                series: vec![("Down".to_owned(), vec![0.2, 0.7, 0.4])],
                accent: (0x2d, 0xd4, 0xbf),
            },
            FlyoutBlock::Image {
                glyph: 0xE9C4,
                path: None,
                size_dips: 48.0,
            },
            FlyoutBlock::IconBar {
                buttons: vec![(13, "Reset".to_owned(), 0xE72C, false)],
            },
        ];
        let spec = FlyoutSpec {
            title: "C2".to_owned(),
            subtitle: None,
            width_dips: 320.0,
            blocks,
        };
        let (top, height, targets) = layout_flyout(&spec, 320, 1.0);
        assert!(top > 0 && height > 0);
        // SeekSlider is draggable, TextInput focusable, IconBar has one button.
        assert!(targets.iter().any(|t| t.id == 11 && t.is_slider && t.is_seek));
        assert!(targets.iter().any(|t| t.id == 12 && t.is_text));
        assert!(targets.iter().any(|t| t.id == 13 && !t.is_slider && !t.is_text));

        // LiveEvent owns text, so the enum cannot be Copy.
        let event = LiveEvent::TextCommit {
            id: 12,
            text: "30".to_owned(),
        };
        match event {
            LiveEvent::TextCommit { id, text } => {
                assert_eq!(id, 12);
                assert_eq!(text, "30");
            }
            _ => panic!("wrong event"),
        }
        assert_eq!(format_clock(125), "2:05");
    }
}
