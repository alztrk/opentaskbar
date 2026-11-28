//! Native Win32 + DirectComposition Dynamic Island window & multi-display manager.
//!
//! Mirrors `Views/Windows/DynamicIslandManager.cs` and `DynamicIslandWindow.xaml.cs`:
//! - One top-edge island per connected display with assigned widgets, either a `Notch`
//!   (flush with the monitor top, rounded bottom corners) or a `Floating` capsule
//! - `Paged` (one widget per page) or `Strip` (several widgets side by side) layouts with
//!   symmetric `‹`/`›` navigation, clickable page dots, pointer drag/swipe, horizontal
//!   wheel / `Shift`+wheel paging and Left/Right keys when hovered
//! - A rich, live expanded detail card for every widget (`WidgetRuntime::island_detail_spec`)
//! - Per-display widget ordering (`DynamicIslandWidgetOrders`), placement context menus,
//!   desktop-only visibility mode, and fullscreen app suppression.
//!
//! The island shares the dock's single `WidgetRuntime` (see [`SharedRuntime`]): no second
//! set of GPU/stats/audio/SMTC/HTTP/Bluetooth/AI pollers runs per monitor, and timer
//! widgets (Stopwatch, Countdown, Focus) show one state in dock and island alike.
//! Surfaces are cached per page/chrome/detail key, hidden islands do no rendering work,
//! and an idle island issues zero DirectComposition commits.
//!
//! Smart behavior (N4): `DynamicIslandIdleMode` `Full`, `Minimal` (a slim resting capsule with
//! the `DynamicIslandClosedDisplay` line, live waveform and privacy dots) or `Closed` (hidden
//! until an edge hover, the global toggle hotkey, a file drag or a transient overlay), hover
//! expand, auto-close, pinning (`Ctrl+P` / pin button), follow-mouse (one window relocated
//! between monitors) and `DynamicIslandScale`. One lazily allocated overlay node shows the
//! transient N7 agent approval banner, N1 system HUDs and O5 notification pills, in that
//! priority. The services behind them (HUD monitor, notification mirror, privacy probe,
//! agent / to-do polls) ride the visibility timer of a single owner island; disabled
//! features arm no timers.

use crate::platform::win32_helpers::set_window_long_ptr;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use opentaskbar_core::dynamic_island as policy;
use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::{
    self, DockSettings, DynamicIslandClosedDisplay, DynamicIslandIdleMode, DynamicIslandLayout,
    DynamicIslandStyle,
};
use windows::Win32::Graphics::DirectComposition::IDCompositionRectangleClip;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_ESCAPE, VK_LBUTTON, VK_LEFT, VK_RIGHT, VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::DragAcceptFiles;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW,
    HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, IDC_HAND, IsIconic, IsWindow, KillTimer,
    LoadCursorW, MA_NOACTIVATE, RegisterClassExW, RegisterWindowMessageW, SW_HIDE, SW_RESTORE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetCursor, SetTimer, SetWindowPos,
    ShowWindow, WM_CAPTURECHANGED, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST, WM_RBUTTONUP, WM_SETCURSOR,
    WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use super::dcomp::{
    ChipChrome, DcompEngine, IslandButton, IslandChromeRender, IslandDetailRender,
    IslandOverlayButton, IslandOverlayContent, IslandOverlayRender, VisualNode, WidgetChipSpec,
    WidgetInteraction,
};
use super::layout::{
    ISLAND_ANIM_MS, WheelDedup, WheelSource, cubic_ease_out, effective_animations_enabled,
};
use crate::design::{self, Rgb};
use crate::platform::hud_monitor::{self, HudEvent, HudMonitor};
use crate::platform::notification_listener::{IslandNotification, NotificationMirror};
use crate::platform::privacy_dots::PrivacyProbe;
use crate::platform::win32::{self, ForegroundProbe, MonitorInfo, WidgetContextAction};
use crate::ui::settings as settings_ui;
use crate::widgets::agents::AgentApprovalRequest;
use crate::widgets::{self, IslandDetailSpec, WidgetRuntime, registry};

// All geometry comes from the reference vectors in `opentaskbar_core::dynamic_island::geometry`.
use opentaskbar_core::dynamic_island::geometry as island_geo;

const COMPACT_HEIGHT_DIPS: f32 = island_geo::COMPACT_HEIGHT_DIPS;
const EXPANDED_HEIGHT_DIPS: f32 = island_geo::EXPANDED_HEIGHT_DIPS;
const MAX_WIDTH_DIPS: f32 = island_geo::MAX_WIDTH_DIPS;
/// Chip height when the header has no page dots (single page).
const CHIP_H_DIPS: f32 = 38.0;
/// Chip height when the bottom of the header carries the page dots.
const PAGED_CHIP_H_DIPS: f32 = 34.0;
/// Narrowest compact island: a single small chip plus symmetric padding.
const MIN_COMPACT_WIDTH_DIPS: f32 = 150.0;
/// Horizontal space between two packed Strip widgets (the hairline sits in its middle).
pub const STRIP_GAP_DIPS: f32 = 13.0;
/// Absolute Strip pill cap and its share of the monitor width.
pub const STRIP_MAX_WIDTH_DIPS: f32 = 760.0;
const STRIP_MONITOR_FRACTION: f32 = 0.72;
/// `Floating` style: gap between the monitor top and the capsule, plus the transparent
/// margin that gives the glass drop shadow room instead of a DWM-clipped box.
const FLOATING_GAP_DIPS: f32 = 8.0;
const FLOATING_MARGIN_DIPS: f32 = 12.0;
const FLOATING_EXPANDED_RADIUS_DIPS: f32 = 24.0;
/// Symmetric header geometry.
pub const HEADER_PAD_X_DIPS: f32 = 10.0;
pub const NAV_BUTTON_DIPS: f32 = 28.0;
pub const EXPAND_BUTTON_DIPS: f32 = 24.0;
pub const CONTROL_GAP_DIPS: f32 = 4.0;
pub const DOT_DIPS: f32 = 6.0;
pub const ACTIVE_DOT_W_DIPS: f32 = 14.0;
pub const DOT_GAP_DIPS: f32 = 5.0;
pub const MAX_VISIBLE_DOTS: usize = 9;
const SLIDE_DURATION_SECS: f32 = 0.190;
/// Gap between the island's bottom edge and a flyout opened below it
/// (`FlyoutEdge::Top` opens downward directly from the anchor).
const FLYOUT_GAP_DIPS: f32 = 8.0;
/// `EnumWindows`-based fullscreen detection runs at most once per this interval.
const FULLSCREEN_SCAN_INTERVAL: Duration = Duration::from_millis(1000);
/// Trackpads emit bursts of horizontal wheel notches; one page per burst.
const WHEEL_PAGE_DEBOUNCE: Duration = Duration::from_millis(220);
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_MOUSEHWHEEL: u32 = 0x020E;
const MK_SHIFT: usize = 0x0004;
/// `WH_MOUSE_LL` wheel route for island windows (posted by `dock::mouse_wheel_hook`
/// with the same payload as the dock's own route), so wheel input reaches the
/// `WS_EX_NOACTIVATE` island even when "scroll inactive windows" is off.
pub(super) const WM_APP_ISLAND_WHEEL_ROUTE: u32 =
    windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 0x20;

const TIMER_VISIBILITY: usize = 1;
/// 1 s tick, armed only while the visible page is time-sensitive (seconds clock,
/// running stopwatch/countdown/focus) or the detail card is expanded. Everything
/// else follows the dock's own 2 s widget tick through `DynamicIslandManager::tick`.
const TIMER_WIDGETS: usize = 2;
const TIMER_ANIM: usize = 3;
const TIMER_HIDE_ANIM: usize = 4;
/// One-shot: expires the active HUD / notification overlay.
const TIMER_OVERLAY: usize = 5;
/// One-shot: hover-to-expand delay (`DynamicIslandHoverDelayMs`).
const TIMER_HOVER_EXPAND: usize = 6;
/// One-shot: auto-collapse after the pointer left (`DynamicIslandAutoCloseSecs`).
const TIMER_AUTO_CLOSE: usize = 7;
/// 15 Hz waveform, armed only while a visible waveform slot has a playing session.
const TIMER_WAVEFORM: usize = 8;
/// Short "gulp" bounce after files were dropped on the island.
const TIMER_GULP: usize = 9;
const WAVEFORM_INTERVAL_MS: u32 = 66;
/// Minimal (slim resting) capsule geometry.
pub const MINIMAL_HEIGHT_DIPS: f32 = 30.0;
pub const MINIMAL_MIN_WIDTH_DIPS: f32 = 120.0;
pub const MINIMAL_MAX_WIDTH_DIPS: f32 = 340.0;
/// Overlay capsule sizes `(width, height)` in DIPs.
pub const HUD_OVERLAY_SIZE: (f32, f32) = (320.0, 50.0);
pub const NOTIFICATION_OVERLAY_SIZE: (f32, f32) = (380.0, 66.0);
pub const AGENT_OVERLAY_SIZE: (f32, f32) = (460.0, 124.0);
/// `Closed` idle mode: a pointer this close to the monitor top reveals the island.
const EDGE_REVEAL_DIPS: f32 = 6.0;
/// The owner island polls HUD / notifications / privacy / agents at most this often.
const SERVICES_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Reveal auto-close used when `DynamicIslandAutoCloseSecs` is `0` (never collapse details).
const REVEAL_FALLBACK_CLOSE_MS: u32 = 1500;
/// An expiring overlay under the pointer is checked again after this delay.
const OVERLAY_HOVER_RECHECK_MS: u32 = 800;
const GULP_DURATION_SECS: f32 = 0.32;
const GULP_SCALE: f32 = 0.08;
/// Lower-priority overlays waiting behind the active one.
const MAX_QUEUED_OVERLAYS: usize = 4;
const WM_DROPFILES: u32 = 0x0233;
/// Manager key of the single follow-mouse island (never a device name).
const FOLLOW_MOUSE_KEY: &str = "*follow-mouse";

/// The island's default order is the registry's catalog order, not a second
/// hand-maintained list: adding a catalog widget makes it appear automatically.
const fn catalog_widget_count() -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < registry::BUILTINS.len() {
        if registry::BUILTINS[i].catalog {
            count += 1;
        }
        i += 1;
    }
    count
}

const fn build_default_widget_order() -> [&'static str; catalog_widget_count()] {
    let mut order = [""; catalog_widget_count()];
    let mut i = 0;
    let mut j = 0;
    while i < registry::BUILTINS.len() {
        if registry::BUILTINS[i].catalog {
            order[j] = registry::BUILTINS[i].id;
            j += 1;
        }
        i += 1;
    }
    order
}

const DEFAULT_WIDGET_ORDER_ARRAY: [&str; catalog_widget_count()] = build_default_widget_order();
const DEFAULT_WIDGET_ORDER: &[&str] = &DEFAULT_WIDGET_ORDER_ARRAY;

use opentaskbar_core::wide;

fn loword_i16(v: isize) -> f32 {
    (v as u32 & 0xffff) as i16 as f32
}

fn hiword_i16(v: isize) -> f32 {
    ((v as u32 >> 16) & 0xffff) as i16 as f32
}

/// Island-local rectangle in DIPs: `(left, top, right, bottom)`.
pub type DipRect = (f32, f32, f32, f32);

fn rect_contains(rect: DipRect, x: f32, y: f32) -> bool {
    (rect.0..=rect.2).contains(&x) && (rect.1..=rect.3).contains(&y)
}

thread_local! {
    /// Reference-counted depth of modal flyouts currently pumping a nested Win32
    /// message loop on this UI thread while a caller holds `&mut WidgetRuntime`
    /// (or `&mut DockState` / `&mut DynamicIslandWindow`).
    static MODAL_FLYOUT_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Re-entrancy guard for `island_wnd_proc`, preventing nested Win32 callbacks
    /// (modal flyouts, context menus, `SetWindowPos`, `ReleaseCapture`) from
    /// creating a second `&mut DynamicIslandWindow`.
    static IN_ISLAND_WNDPROC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// N9 privacy bitmask published by the owner island (`bit 0` mic, `bit 1` camera).
    static PRIVACY_BITS: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Returns `true` while any dock or Dynamic Island modal flyout is pumping a
/// nested Win32 message loop on this UI thread.
pub(crate) fn is_modal_flyout_active() -> bool {
    MODAL_FLYOUT_DEPTH.with(std::cell::Cell::get) > 0
}

/// RAII guard that tracks active modal flyouts on the UI thread so timers and
/// cross-window callbacks never alias `&mut WidgetRuntime` or `&mut DynamicIslandWindow`.
pub(crate) struct ModalFlyoutGuard {
    _private: (),
}

impl ModalFlyoutGuard {
    pub(crate) fn enter() -> Self {
        MODAL_FLYOUT_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self { _private: () }
    }
}

impl Drop for ModalFlyoutGuard {
    fn drop(&mut self) {
        MODAL_FLYOUT_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// RAII re-entrancy guard for `island_wnd_proc` (mirrors `DockWndProcGuard`).
struct IslandWndProcGuard {
    _private: (),
}

impl IslandWndProcGuard {
    fn try_enter() -> Option<Self> {
        IN_ISLAND_WNDPROC.with(|active| {
            if active.replace(true) {
                None
            } else {
                Some(Self { _private: () })
            }
        })
    }
}

impl Drop for IslandWndProcGuard {
    fn drop(&mut self) {
        IN_ISLAND_WNDPROC.with(|active| active.set(false));
    }
}

/// Re-bases a slide offset across a viewport width change so `prev_page` starts
/// at the same visual drag delta (`0.0` when resting) in the new viewport metric.
fn rebase_slide_offset(
    slide_offset_px: f32,
    prev_page: usize,
    prev_vp_px: f32,
    new_vp_px: f32,
) -> f32 {
    let drag_delta = slide_offset_px + (prev_page as f32) * prev_vp_px;
    -(prev_page as f32) * new_vp_px + drag_delta
}

/// Non-owning handle to the dock's single `WidgetRuntime`.
///
/// The dock keeps its runtime in a `Box` (stable heap address) for as long as its
/// `DynamicIslandManager` lives and drops the manager first; dock and islands run on the
/// same UI thread. Re-entrant modal flyout loops are guarded by `ModalFlyoutGuard` and
/// `IslandWndProcGuard` so no timer or callback aliases `&mut WidgetRuntime` while a
/// flyout holds it.
#[derive(Clone, Copy)]
pub struct SharedRuntime(std::ptr::NonNull<WidgetRuntime>);

impl SharedRuntime {
    /// # Safety
    /// `runtime` must stay at the same address and outlive every island that receives
    /// this handle, and must only be used from the thread that owns those windows.
    pub unsafe fn new(runtime: &mut WidgetRuntime) -> Self {
        Self(std::ptr::NonNull::from(runtime))
    }

    fn get(&mut self) -> &mut WidgetRuntime {
        // SAFETY: guaranteed by the `SharedRuntime::new` contract (single UI thread,
        // runtime outlives the islands).
        unsafe { self.0.as_mut() }
    }
}

/// What an island button / chip click must do. Pure so the flyout dispatch can be
/// unit tested without a HWND (WS-C: no island click may end in a dead end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandClickAction {
    PrevPage,
    NextPage,
    /// Toggles the in-place expanded detail card.
    ToggleDetails,
    /// Page indicator dot: jump straight to that page.
    GoToPage(usize),
    /// Pin toggle: a pinned island never auto-collapses.
    TogglePin,
}

/// True when the widget opens a real flyout (`WidgetRuntime::open_widget_flyout_on_edge`).
/// Delegates to the registry, so every flyout-capable catalog widget, legacy alias
/// (`SystemStats`, `KeyboardLayout`, `AiUsageClaude`, ...) and `custom:*` widget opens
/// its flyout from the island.
pub fn has_real_widget_flyout(widget_id: &str) -> bool {
    let id = widget_id.trim();
    if id.is_empty() {
        return false;
    }
    // `custom:<name>` (a bare `custom:` prefix names no widget).
    let is_custom = id.len() > 7
        && id
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("custom:"));
    is_custom || registry::widget_has_flyout(registry::canonical_widget_id(id))
}

/// `true` when two widget IDs name the same widget (`stats` / `SystemStats`, ...).
pub fn island_widget_matches(a: &str, b: &str) -> bool {
    policy::same_widget_id(a, b)
}

/// Clock and Focus stay visible in the island during Focus Mode when
/// `DynamicIslandShowFocusWidgets` is on.
pub fn is_focus_mode_widget(widget_id: &str) -> bool {
    island_widget_matches(widget_id, "clock") || island_widget_matches(widget_id, "focus")
}

/// Every widget has an expanded live detail card (`WidgetRuntime::island_detail_spec`);
/// only an empty island has nothing to expand.
pub fn has_expandable_content(widget_id: Option<&str>) -> bool {
    widget_id.is_some_and(|id| !id.trim().is_empty())
}

/// `Previous_Click` / `Next_Click` / `Expand_Click` / page dot click.
pub fn island_button_click_action(button: IslandButton) -> IslandClickAction {
    match button {
        IslandButton::Prev => IslandClickAction::PrevPage,
        IslandButton::Next => IslandClickAction::NextPage,
        IslandButton::Expand => IslandClickAction::ToggleDetails,
        IslandButton::Pin => IslandClickAction::TogglePin,
        IslandButton::Dot(page) => IslandClickAction::GoToPage(usize::from(page)),
    }
}

/// `PageCount.Text` uses culture-aware `{0:00} / {1:00}` formatting. The
/// catalog key is requested first; locales that omit it fall back to the
/// numeric format instead of displaying a raw key.
pub fn page_count_text(current_page: usize, total_pages: usize) -> String {
    const KEY: &str = "DynamicIslandPageCount";
    let template = loc::t(KEY);
    if template == KEY {
        format!("{:02} / {:02}", current_page + 1, total_pages)
    } else {
        loc::t_fmt(
            KEY,
            &[
                &format!("{:02}", current_page + 1),
                &format!("{:02}", total_pages),
            ],
        )
    }
}

/// Approximates `_details.Measure(new Size(640, ...)).DesiredSize.Width`.
pub fn details_desired_width(details: &WidgetDetails) -> f32 {
    let longest = details
        .lines
        .iter()
        .map(|line| line.label.chars().count() + line.value.chars().count() + 2)
        .max()
        .unwrap_or(0)
        .max(details.title.chars().count());
    (longest as f32 * 7.0).clamp(0.0, 640.0)
}

/// Screen anchor for a top-edge island flyout. Flyouts open with
/// `FlyoutEdge::Top` (downward from the anchor), so the anchor sits a small gap
/// below the island's visible bottom edge, horizontally centered on the island.
pub fn island_flyout_anchor(
    monitor_left: i32,
    monitor_width: i32,
    island_bottom_y: i32,
    dpi: f32,
) -> (i32, i32) {
    let x = monitor_left + monitor_width / 2;
    let y = island_bottom_y + (FLYOUT_GAP_DIPS * dpi.max(0.5)).round() as i32;
    (x, y)
}

/// Returns whether `widget_id` is enabled and placed on at least one connected display.
pub fn is_widget_placed_on_connected_display(
    settings: &DockSettings,
    widget_id: &str,
    monitors: &[MonitorInfo],
) -> bool {
    // A disabled island must not suppress widgets in the dock.
    if !settings.dynamic_island_enable {
        return false;
    }
    // Focus Mode keeps only Clock/Focus in the island, and only when the user opted in;
    // anything else returns to the dock instead of vanishing with the island.
    if settings.focus_mode
        && (!is_focus_mode_widget(widget_id) || !settings.dynamic_island_show_focus_widgets())
    {
        return false;
    }
    if !settings.is_widget_enabled(widget_id) {
        return false;
    }
    let names: Vec<&str> = monitors.iter().map(|m| m.device_name.as_str()).collect();
    policy::is_assigned_to_any_display(&settings.dynamic_island_placements, widget_id, &names)
}

/// Widest Strip pill for a monitor of `monitor_dips` width.
pub fn strip_max_width_dips(monitor_dips: f32) -> f32 {
    let monitor = if monitor_dips.is_finite() && monitor_dips > 0.0 {
        monitor_dips
    } else {
        1280.0
    };
    (monitor * STRIP_MONITOR_FRACTION).clamp(MIN_COMPACT_WIDTH_DIPS, STRIP_MAX_WIDTH_DIPS)
}

fn sanitize_width(width: f32) -> f32 {
    if width.is_finite() && width > 1.0 {
        width
    } else {
        island_geo::FALLBACK_WIDGET_WIDTH_DIPS * 0.5
    }
}

/// `Paged` layout: one widget per page.
pub fn paged_groups(count: usize) -> Vec<Vec<usize>> {
    (0..count).map(|idx| vec![idx]).collect()
}

/// `Strip` layout: greedily packs widgets side by side into pages whose content
/// (chip widths plus `gap` between neighbours) fits `max_content_dips`. A widget
/// wider than the budget gets a page of its own. Never returns an empty page.
pub fn pack_strip_pages(widths: &[f32], max_content_dips: f32, gap: f32) -> Vec<Vec<usize>> {
    let budget = if max_content_dips.is_finite() && max_content_dips > 1.0 {
        max_content_dips
    } else {
        STRIP_MAX_WIDTH_DIPS
    };
    let gap = if gap.is_finite() { gap.max(0.0) } else { 0.0 };
    let mut pages: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut used = 0.0_f32;
    for (idx, raw) in widths.iter().enumerate() {
        let w = sanitize_width(*raw).min(budget);
        let needed = if current.is_empty() {
            w
        } else {
            used + gap + w
        };
        if !current.is_empty() && needed > budget + 0.01 {
            pages.push(std::mem::take(&mut current));
            used = w;
        } else {
            used = needed;
        }
        current.push(idx);
    }
    if !current.is_empty() {
        pages.push(current);
    }
    pages
}

/// Content width of one page: its chip widths plus the gaps between them.
pub fn group_content_width(widths: &[f32], group: &[usize], gap: f32) -> f32 {
    let chips: f32 = group
        .iter()
        .filter_map(|idx| widths.get(*idx))
        .map(|w| sanitize_width(*w))
        .sum();
    chips + gap * group.len().saturating_sub(1) as f32
}

/// Width of each symmetric control cluster (`‹` on the left, `›` + expand on the right).
/// Both sides reserve the same width so the content is always centered.
pub fn island_side_cluster_dips(has_paging: bool, has_expand: bool) -> f32 {
    let nav = if has_paging { NAV_BUTTON_DIPS } else { 0.0 };
    let expand = if has_expand { EXPAND_BUTTON_DIPS } else { 0.0 };
    // Left: `‹`. Right: `›` + expand. The wider side sets both.
    nav + expand
}

/// Compact island width for `content_dips` of chips: symmetric padding and clusters.
pub fn island_width_for_content(content_dips: f32, has_paging: bool, has_expand: bool) -> f32 {
    let side = island_side_cluster_dips(has_paging, has_expand);
    let gaps = if side > 0.0 {
        CONTROL_GAP_DIPS * 2.0
    } else {
        0.0
    };
    HEADER_PAD_X_DIPS * 2.0 + side * 2.0 + gaps + sanitize_width(content_dips)
}

/// Header geometry shared by the renderer and the hit tester.
#[derive(Clone, Debug, PartialEq)]
pub struct IslandHeaderLayout {
    pub content_left: f32,
    pub content_right: f32,
    pub prev: Option<DipRect>,
    pub next: Option<DipRect>,
    pub expand: Option<DipRect>,
    /// Pin toggle slot mirrored opposite the expand chevron. Always reserved (no layout
    /// shift); drawn only while the island is expanded, hovered or pinned, and otherwise
    /// home of the media waveform.
    pub pin: Option<DipRect>,
    /// `(absolute page index, visual rect)`; the active page is a 14×6 DIP pill.
    pub dots: Vec<(usize, DipRect)>,
    pub chip_top: f32,
    pub chip_h: f32,
}

impl IslandHeaderLayout {
    pub fn content_width(&self) -> f32 {
        (self.content_right - self.content_left).max(1.0)
    }
}

/// Symmetric header: `[pad][‹ 28][gap][content][gap][› 28][expand 24][pad]`, with the
/// left cluster as wide as the right one. Page dots sit centered under the content.
pub fn island_header_layout(
    width_dips: f32,
    header_h: f32,
    page_count: usize,
    current_page: usize,
    has_expand: bool,
) -> IslandHeaderLayout {
    let width = if width_dips.is_finite() {
        width_dips.max(1.0)
    } else {
        MIN_COMPACT_WIDTH_DIPS
    };
    let header_h = if header_h.is_finite() && header_h > 8.0 {
        header_h
    } else {
        COMPACT_HEIGHT_DIPS
    };
    let has_paging = page_count > 1;
    let side = island_side_cluster_dips(has_paging, has_expand);
    let gap = if side > 0.0 { CONTROL_GAP_DIPS } else { 0.0 };
    let content_left = (HEADER_PAD_X_DIPS + side + gap).min(width * 0.5);
    let content_right = (width - HEADER_PAD_X_DIPS - side - gap).max(content_left + 1.0);
    let btn_h = 32.0_f32.min(header_h - 8.0);
    let btn_top = (header_h - btn_h) * 0.5;
    let prev = has_paging.then_some((
        HEADER_PAD_X_DIPS,
        btn_top,
        HEADER_PAD_X_DIPS + NAV_BUTTON_DIPS,
        btn_top + btn_h,
    ));
    let pin = has_expand.then(|| {
        let left = HEADER_PAD_X_DIPS + if has_paging { NAV_BUTTON_DIPS } else { 0.0 };
        (left, btn_top, left + EXPAND_BUTTON_DIPS, btn_top + btn_h)
    });
    let mut right_x = width - HEADER_PAD_X_DIPS;
    let expand = has_expand.then(|| {
        let rect = (
            right_x - EXPAND_BUTTON_DIPS,
            btn_top,
            right_x,
            btn_top + btn_h,
        );
        right_x -= EXPAND_BUTTON_DIPS;
        rect
    });
    let next = has_paging.then_some((right_x - NAV_BUTTON_DIPS, btn_top, right_x, btn_top + btn_h));

    let (chip_top, chip_h) = if has_paging {
        (4.0, PAGED_CHIP_H_DIPS.min(header_h - 12.0))
    } else {
        let h = CHIP_H_DIPS.min(header_h - 4.0);
        ((header_h - h) * 0.5, h)
    };

    let mut dots = Vec::new();
    if has_paging {
        let visible = page_count.min(MAX_VISIBLE_DOTS);
        let current = current_page.min(page_count - 1);
        let first = current
            .saturating_sub(visible / 2)
            .min(page_count - visible);
        let total_w = ACTIVE_DOT_W_DIPS
            + DOT_DIPS * (visible - 1) as f32
            + DOT_GAP_DIPS * (visible - 1) as f32;
        let mut x = (content_left + content_right - total_w) * 0.5;
        let cy = header_h - 7.0;
        for page in first..first + visible {
            let w = if page == current {
                ACTIVE_DOT_W_DIPS
            } else {
                DOT_DIPS
            };
            dots.push((page, (x, cy - DOT_DIPS * 0.5, x + w, cy + DOT_DIPS * 0.5)));
            x += w + DOT_GAP_DIPS;
        }
    }

    IslandHeaderLayout {
        content_left,
        content_right,
        prev,
        next,
        expand,
        pin,
        dots,
        chip_top,
        chip_h,
    }
}

/// Which transient overlay the island shows. Priority: agent approval > HUD > notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandOverlayKind {
    AgentApproval,
    Hud,
    Notification,
}

impl IslandOverlayKind {
    pub fn priority(self) -> u8 {
        match self {
            Self::AgentApproval => 3,
            Self::Hud => 2,
            Self::Notification => 1,
        }
    }
}

/// `true` when an incoming overlay replaces the active one right away (equal or higher
/// priority); otherwise it waits in the overlay queue.
pub fn overlay_preempts(active: Option<IslandOverlayKind>, incoming: IslandOverlayKind) -> bool {
    active.is_none_or(|kind| incoming.priority() >= kind.priority())
}

/// One transient island overlay.
#[derive(Clone, Debug, PartialEq)]
pub enum IslandOverlay {
    AgentApproval(AgentApprovalRequest),
    Hud(HudEvent),
    Notification(IslandNotification),
}

impl IslandOverlay {
    pub fn kind(&self) -> IslandOverlayKind {
        match self {
            Self::AgentApproval(_) => IslandOverlayKind::AgentApproval,
            Self::Hud(_) => IslandOverlayKind::Hud,
            Self::Notification(_) => IslandOverlayKind::Notification,
        }
    }

    /// On-screen time; `None` keeps the overlay until it is answered (agent approval).
    pub fn duration_ms(&self, settings: &DockSettings) -> Option<u32> {
        match self {
            Self::AgentApproval(_) => None,
            Self::Hud(event) => Some(event.duration_ms.max(500)),
            Self::Notification(_) => Some(settings.island_notifications_duration_ms()),
        }
    }

    fn has_progress(&self) -> bool {
        matches!(self, Self::Hud(event) if event.progress.is_some())
    }
}

/// Overlay geometry shared by the renderer and the hit tester (capsule-local DIPs).
#[derive(Clone, Debug, PartialEq)]
pub struct IslandOverlayLayout {
    pub width: f32,
    pub height: f32,
    pub icon: DipRect,
    /// Small secondary line (app name, `agent · project`) or the HUD value / badge.
    pub subtitle: DipRect,
    pub title: DipRect,
    pub body: DipRect,
    /// HUD level bar.
    pub progress: Option<DipRect>,
    /// Agent diff summary (`+N −M`).
    pub meta: Option<DipRect>,
    pub buttons: Vec<(IslandOverlayButton, DipRect)>,
}

/// Overlay capsule layout, narrowed to `max_width` on small monitors.
pub fn island_overlay_layout(
    kind: IslandOverlayKind,
    has_progress: bool,
    max_width: f32,
) -> IslandOverlayLayout {
    let fit = |preferred: f32| {
        if max_width.is_finite() && max_width > 200.0 {
            preferred.min(max_width)
        } else {
            preferred
        }
    };
    match kind {
        IslandOverlayKind::Hud => {
            let (width, height) = (fit(HUD_OVERLAY_SIZE.0), HUD_OVERLAY_SIZE.1);
            let text_l = 56.0;
            let text_r = (width - 18.0).max(text_l + 40.0);
            let (title, subtitle, progress) = if has_progress {
                let value_l = (text_r - 52.0).max(text_l + 20.0);
                (
                    (text_l, 6.0, value_l, 26.0),
                    (value_l, 6.0, text_r, 26.0),
                    Some((text_l, 31.0, text_r, 37.0)),
                )
            } else {
                (
                    (text_l, 6.0, text_r, 25.0),
                    (text_l, 26.0, text_r, 43.0),
                    None,
                )
            };
            IslandOverlayLayout {
                width,
                height,
                icon: (14.0, 9.0, 46.0, 41.0),
                subtitle,
                title,
                body: subtitle,
                progress,
                meta: None,
                buttons: Vec::new(),
            }
        }
        IslandOverlayKind::Notification => {
            let (width, height) = (
                fit(NOTIFICATION_OVERLAY_SIZE.0),
                NOTIFICATION_OVERLAY_SIZE.1,
            );
            let text_l = 60.0;
            let text_r = (width - 44.0).max(text_l + 40.0);
            let dismiss = (width - 38.0, 10.0, width - 14.0, 34.0);
            IslandOverlayLayout {
                width,
                height,
                icon: (14.0, 15.0, 50.0, 51.0),
                subtitle: (text_l, 7.0, text_r, 21.0),
                title: (text_l, 21.0, text_r, 40.0),
                body: (text_l, 40.0, (width - 18.0).max(text_l + 40.0), 58.0),
                progress: None,
                meta: None,
                buttons: vec![(IslandOverlayButton::Dismiss, dismiss)],
            }
        }
        IslandOverlayKind::AgentApproval => {
            let (width, height) = (fit(AGENT_OVERLAY_SIZE.0), AGENT_OVERLAY_SIZE.1);
            let text_l = 56.0;
            let text_r = (width - 16.0).max(text_l + 80.0);
            let meta_l = text_r - 76.0;
            let btn_h = 30.0;
            let btn_top = height - 14.0 - btn_h;
            let gap = 8.0;
            let order = [
                IslandOverlayButton::Allow,
                IslandOverlayButton::Deny,
                IslandOverlayButton::Always,
                IslandOverlayButton::Jump,
            ];
            let bw = ((width - 28.0 - gap * 3.0) / 4.0).max(40.0);
            let mut x = 14.0;
            let buttons = order
                .into_iter()
                .map(|button| {
                    let rect = (x, btn_top, x + bw, btn_top + btn_h);
                    x += bw + gap;
                    (button, rect)
                })
                .collect();
            IslandOverlayLayout {
                width,
                height,
                icon: (14.0, 14.0, 46.0, 46.0),
                subtitle: (text_l, 10.0, text_r, 26.0),
                title: (text_l, 26.0, text_r, 46.0),
                body: (text_l, 46.0, meta_l - 6.0, 64.0),
                progress: None,
                meta: Some((meta_l, 46.0, text_r, 64.0)),
                buttons,
            }
        }
    }
}

/// `(added, removed)` line counts of a unified diff preview (`+++` / `---` headers excluded).
pub fn diff_stats(preview: &str) -> (usize, usize) {
    preview.lines().fold((0, 0), |(added, removed), line| {
        if line.starts_with("+++") || line.starts_with("---") {
            (added, removed)
        } else if line.starts_with('+') {
            (added + 1, removed)
        } else if line.starts_with('-') {
            (added, removed + 1)
        } else {
            (added, removed)
        }
    })
}

/// N6: the album-art square at the leading edge of a Media chip (`dcomp` draws it at
/// `x = 7 DIP`, `min(28, h - 8)` DIP wide). A click there cycles the SMTC session.
pub fn media_album_art_hit(widget_id: &str, rel_x: f32, chip_h: f32) -> bool {
    if registry::canonical_widget_id(widget_id) != "media" {
        return false;
    }
    let art = 28.0_f32.min(chip_h - 8.0).max(20.0);
    (3.0..=art + 10.0).contains(&rel_x)
}

/// Cycles the active SMTC media session off the UI thread (`cycle_media_session` joins a
/// cross-process WinRT call).
pub(super) fn cycle_media_session_async() {
    let spawned = std::thread::Builder::new()
        .name("opentaskbar-media-cycle".to_owned())
        .spawn(|| {
            let _ = widgets::media_ext::cycle_media_session();
        });
    if let Err(err) = spawned {
        opentaskbar_core::log::warn(format!("Media session cycle spawn failed: {err}"));
    }
}

/// Minimal capsule width for its line of text (`None` = bare slim line).
pub fn minimal_width_for(text: Option<&str>, side_reserve_dips: f32) -> f32 {
    let text_w = text.map_or(0.0, |t| t.chars().count().min(64) as f32 * 6.6);
    let side = if side_reserve_dips.is_finite() {
        side_reserve_dips.max(0.0)
    } else {
        0.0
    };
    (text_w + 2.0 * (side + 18.0)).clamp(MINIMAL_MIN_WIDTH_DIPS, MINIMAL_MAX_WIDTH_DIPS)
}

/// Page index after `delta` steps with wrap-around (`Ctrl+Left` / `Ctrl+Right`).
pub fn cycle_page_index(current: usize, count: usize, delta: isize) -> usize {
    if count == 0 {
        return 0;
    }
    let count = count as isize;
    (current as isize + delta).rem_euclid(count) as usize
}

/// `Ctrl+1..9`: virtual key `'1'..='9'` to a zero-based page index.
pub fn ctrl_digit_page(vk: u16) -> Option<usize> {
    (u16::from(b'1')..=u16::from(b'9'))
        .contains(&vk)
        .then(|| usize::from(vk - u16::from(b'1')))
}

fn truncate_line(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Localized string with an English fallback while a catalog lacks the key.
fn localized_or(key: &str, fallback: &str) -> String {
    let text = loc::t(key);
    if text == key || text.trim().is_empty() {
        fallback.to_owned()
    } else {
        text
    }
}

fn cursor_pos() -> Option<POINT> {
    let mut pt = POINT { x: 0, y: 0 };
    // SAFETY: `pt` is a valid out pointer.
    (unsafe { GetCursorPos(&mut pt) } != 0).then_some(pt)
}

/// `HMONITOR` (as `usize`) under the cursor, `0` when unknown.
fn cursor_monitor_handle() -> usize {
    cursor_pos().map_or(0, |pt| {
        // SAFETY: plain value query.
        unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) as usize }
    })
}

/// Brings a notification / agent terminal window to the foreground (restores it if minimized).
fn focus_target_window(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    let target = hwnd as HWND;
    // SAFETY: value queries / show on a validated top-level window handle.
    unsafe {
        if IsWindow(target) == 0 {
            return;
        }
        if IsIconic(target) != 0 {
            ShowWindow(target, SW_RESTORE);
        }
    }
    super::restore_app_foreground(hwnd as usize, 0);
}

/// Expanded detail card geometry shared by the renderer and the hit tester.
#[derive(Clone, Debug, PartialEq)]
pub struct IslandDetailLayout {
    pub panel: DipRect,
    pub title: DipRect,
    pub headline: DipRect,
    pub subtitle: DipRect,
    pub rows: Vec<DipRect>,
    pub buttons: Vec<DipRect>,
}

/// `[title / bold headline / subtitle | up to 4 stat rows]` above a row of buttons.
pub fn island_detail_layout(
    width_dips: f32,
    header_h: f32,
    total_h: f32,
    rows: usize,
    buttons: usize,
) -> IslandDetailLayout {
    let width = if width_dips.is_finite() {
        width_dips.max(80.0)
    } else {
        island_geo::EXPANDED_PREFERRED_MIN_DIPS
    };
    let px = island_geo::DETAILS_PADDING_X_DIPS;
    let top = header_h + island_geo::DETAILS_PADDING_TOP_DIPS;
    let bottom = (total_h - island_geo::DETAILS_PADDING_BOTTOM_DIPS).max(top + 40.0);
    let panel = (px, top, (width - px).max(px + 40.0), bottom);
    let inner_l = panel.0 + 14.0;
    let inner_r = (panel.2 - 14.0).max(inner_l + 20.0);
    let inner_t = panel.1 + 12.0;
    let inner_b = (panel.3 - 12.0).max(inner_t + 20.0);
    let inner_w = inner_r - inner_l;

    let rows = rows.min(4);
    let buttons = buttons.min(4);
    let btn_h = 28.0;
    let mut button_rects = Vec::with_capacity(buttons);
    let content_b = if buttons > 0 {
        let btn_top = inner_b - btn_h;
        let gap = 8.0;
        let bw = ((inner_w - gap * (buttons - 1) as f32) / buttons as f32).clamp(40.0, 150.0);
        let mut x = inner_l;
        for _ in 0..buttons {
            button_rects.push((x, btn_top, x + bw, btn_top + btn_h));
            x += bw + gap;
        }
        (btn_top - 10.0).max(inner_t + 10.0)
    } else {
        inner_b
    };

    let left_w = if rows > 0 {
        (inner_w * 0.38).clamp(120.0, 220.0).min(inner_w)
    } else {
        inner_w
    };
    let title = (inner_l, inner_t, inner_l + left_w, inner_t + 16.0);
    let headline = (inner_l, inner_t + 18.0, inner_l + left_w, inner_t + 48.0);
    let subtitle = (
        inner_l,
        inner_t + 50.0,
        inner_l + left_w,
        (inner_t + 68.0).min(content_b).max(inner_t + 51.0),
    );
    let mut row_rects = Vec::with_capacity(rows);
    if rows > 0 {
        let col_l = (inner_l + left_w + 18.0).min(inner_r - 10.0);
        let row_h = ((content_b - inner_t) / rows as f32).clamp(18.0, 30.0);
        for i in 0..rows {
            let t = inner_t + i as f32 * row_h;
            row_rects.push((col_l, t, inner_r, t + row_h - 2.0));
        }
    }
    IslandDetailLayout {
        panel,
        title,
        headline,
        subtitle,
        rows: row_rects,
        buttons: button_rects,
    }
}

/// Hover/press sub-zone inside a chip, mirroring the dock (`media` transport buttons,
/// Quick Settings toggles, timer play/pause).
fn chip_sub_zone(widget_id: &str, rel_x: f32, width: f32) -> Option<u8> {
    match registry::canonical_widget_id(widget_id) {
        "media" => {
            let controls_l = width - 116.0;
            if rel_x < controls_l {
                None
            } else if rel_x < controls_l + 26.0 {
                Some(0)
            } else if rel_x < controls_l + 55.0 {
                Some(1)
            } else if rel_x < controls_l + 82.0 {
                Some(2)
            } else {
                Some(3)
            }
        }
        "quicksettings" => Some(super::layout::quick_settings_zone(rel_x, width)),
        "focus" | "stopwatch" | "countdown" => Some(u8::from(rel_x >= width - 36.0)),
        _ => None,
    }
}

/// Whether a chip must refresh every second (seconds clock, running timers).
fn chip_is_time_sensitive(spec: &WidgetChipSpec, settings: &DockSettings) -> bool {
    match spec {
        WidgetChipSpec::Timed { running, .. } => *running,
        WidgetChipSpec::Clock { .. } => settings.clock_show_seconds,
        _ => false,
    }
}

/// Custom island colour from `DynamicIslandBackgroundColor` (`#RRGGBB` / `#AARRGGBB`).
fn custom_island_rgb(settings: &DockSettings) -> Option<Rgb> {
    settings
        .dynamic_island_background_hex()
        .and_then(design::parse_hex_rgb)
        .or_else(|| {
            settings
                .dynamic_island_background_color
                .as_deref()
                .and_then(design::parse_hex_rgb)
        })
}

/// `(glass tint, rim)` for the island. `UseTheme` follows the active theme surface
/// (`DockMaterial`/`DockBorderMaterial`, then `WidgetIsland`/`WidgetIslandStroke`);
/// otherwise the custom background colour tints the glass.
fn island_theme_tint(settings: &DockSettings) -> (Option<Rgb>, Option<Rgb>) {
    if settings.dynamic_island_use_theme {
        let (tint, rim) = design::theme::theme_surface_tint("DockMaterial", "DockBorderMaterial");
        if tint.is_some() && rim.is_some() {
            return (tint, rim);
        }
        let (island_tint, island_rim) =
            design::theme::theme_surface_tint("WidgetIsland", "WidgetIslandStroke");
        (tint.or(island_tint), rim.or(island_rim))
    } else {
        (custom_island_rgb(settings), None)
    }
}

/// Solid / glass-fallback fill colour of the island capsule.
fn island_fill_rgb(settings: &DockSettings, is_dark: bool) -> Rgb {
    let neutral = if is_dark {
        (0x2b, 0x2b, 0x30)
    } else {
        (0xf7, 0xf7, 0xfa)
    };
    if settings.dynamic_island_use_theme {
        island_theme_tint(settings)
            .0
            .unwrap_or_else(|| design::active_palette(is_dark).dock_pill)
    } else {
        custom_island_rgb(settings).unwrap_or(neutral)
    }
}

pub struct DynamicIslandManager {
    windows: HashMap<String, Box<DynamicIslandWindow>>,
    reload_msg: u32,
    runtime: SharedRuntime,
    /// Island that owns the shared services (HUD / notifications / privacy / agents) and
    /// shows the transient overlays: the primary monitor's island, else the first one.
    owner_key: Option<String>,
}

impl DynamicIslandManager {
    pub fn new(runtime: SharedRuntime) -> Self {
        register_island_class();
        let reload_msg =
            unsafe { RegisterWindowMessageW(wide(settings_ui::RELOAD_MSG_NAME).as_ptr()) };
        Self {
            windows: HashMap::new(),
            reload_msg,
            runtime,
            owner_key: None,
        }
    }

    pub fn apply(&mut self, settings: &DockSettings, monitors: &[MonitorInfo], is_dark: bool) {
        if !settings.dynamic_island_enable {
            // Tear down any existing islands: a disabled feature owns no windows and renders nothing.
            self.destroy_all();
            return;
        }

        let focus_filter = settings.focus_mode;
        let show_focus_widgets = settings.dynamic_island_show_focus_widgets();
        let selected: Vec<String> = widgets::active_widgets(settings)
            .into_iter()
            .map(|w| w.id.to_owned())
            .filter(|id| !focus_filter || (show_focus_widgets && is_focus_mode_widget(id)))
            .collect();

        let mut desired: HashMap<String, (MonitorInfo, Vec<String>)> = HashMap::new();
        for mon in monitors {
            let assigned = policy::widgets_for_display(
                &selected,
                &settings.dynamic_island_placements,
                &mon.device_name,
            );
            if !assigned.is_empty() {
                desired.insert(
                    mon.device_name.to_ascii_lowercase(),
                    (mon.clone(), assigned),
                );
            }
        }
        // Keep the island visible when the feature is enabled even before any widget is assigned.
        if desired.is_empty() {
            let fallback_monitors: Vec<&MonitorInfo> = if settings.show_on_all_monitors {
                monitors.iter().collect()
            } else {
                monitors
                    .iter()
                    .find(|m| m.is_primary)
                    .or_else(|| monitors.first())
                    .into_iter()
                    .collect()
            };
            for mon in fallback_monitors {
                desired.insert(
                    mon.device_name.to_ascii_lowercase(),
                    (mon.clone(), Vec::new()),
                );
            }
        }

        // Follow-mouse: one island (one HWND, one DComp engine) carrying every assigned widget,
        // placed on the monitor under the cursor and relocated by its visibility timer.
        if settings.dynamic_island_follow_mouse() && !desired.is_empty() {
            let mut merged: Vec<String> = Vec::new();
            for (_, ids) in desired.values() {
                for id in ids {
                    if !merged.iter().any(|m| island_widget_matches(m, id)) {
                        merged.push(id.clone());
                    }
                }
            }
            let cursor_monitor = cursor_monitor_handle();
            let monitor = monitors
                .iter()
                .find(|m| cursor_monitor != 0 && m.handle == cursor_monitor)
                .or_else(|| monitors.iter().find(|m| m.is_primary))
                .or_else(|| monitors.first())
                .cloned();
            desired.clear();
            if let Some(monitor) = monitor {
                desired.insert(FOLLOW_MOUSE_KEY.to_owned(), (monitor, merged));
            }
        }

        let stale_keys: Vec<String> = self
            .windows
            .keys()
            .filter(|k| !desired.contains_key(*k))
            .cloned()
            .collect();
        for k in stale_keys {
            if let Some(win) = self.windows.remove(&k) {
                win.destroy();
            }
        }

        for (key, (mon, widget_ids)) in desired {
            if let Some(win) = self.windows.get_mut(&key) {
                win.apply(mon, widget_ids, settings, monitors, is_dark);
            } else if let Some(win) = DynamicIslandWindow::create(
                mon,
                widget_ids,
                settings,
                monitors,
                is_dark,
                self.reload_msg,
                self.runtime,
            ) {
                self.windows.insert(key, win);
            }
        }
        self.assign_owner();
    }

    /// Keeps exactly one island holding the shared services (moved, not duplicated).
    fn assign_owner(&mut self) {
        let mut keys: Vec<&String> = self.windows.keys().collect();
        keys.sort();
        let preferred = keys
            .iter()
            .find(|k| {
                self.windows
                    .get(k.as_str())
                    .is_some_and(|w| w.monitor.is_primary)
            })
            .or_else(|| keys.first())
            .map(|k| k.as_str().to_owned());
        let mut services: Option<Box<IslandServices>> = None;
        for (key, win) in &mut self.windows {
            if Some(key) != preferred.as_ref()
                && let Some(moved) = win.services.take()
            {
                services.get_or_insert(moved);
            }
        }
        if let Some(key) = &preferred
            && let Some(win) = self.windows.get_mut(key)
            && win.services.is_none()
        {
            win.services = Some(services.unwrap_or_else(|| Box::new(IslandServices::new())));
        }
        self.owner_key = preferred;
    }

    fn owner_mut(&mut self) -> Option<&mut DynamicIslandWindow> {
        let key = self.owner_key.as_ref()?;
        self.windows.get_mut(key).map(|win| &mut **win)
    }

    /// N1 / N5 / N8: shows a transient HUD (OCR, screenshot, reminders, shelf drops).
    pub fn push_hud_event(&mut self, ev: HudEvent) {
        if let Some(win) = self.owner_mut() {
            win.push_overlay(IslandOverlay::Hud(ev));
        }
    }

    /// O5: shows a mirrored notification pill.
    pub fn push_notification(&mut self, n: IslandNotification) {
        if let Some(win) = self.owner_mut() {
            win.push_overlay(IslandOverlay::Notification(n));
        }
    }

    /// O5 portable fallback: a flashing / alerting background window becomes a notification
    /// pill (filtered by the mirror's mute list, Focus and duplicate cooldown).
    pub fn push_shell_alert(&mut self, app_name: String, title: String, hwnd: isize) {
        if let Some(win) = self.owner_mut() {
            win.push_shell_alert(app_name, title, hwnd);
        }
    }

    /// Global `IslandToggleHotkey`: opens (reveal + expand) or closes the island on the
    /// monitor under the cursor, falling back to the owner island.
    pub fn toggle_open(&mut self) {
        if is_modal_flyout_active() {
            return;
        }
        let cursor_monitor = cursor_monitor_handle();
        let key = self
            .windows
            .iter()
            .find(|(_, w)| cursor_monitor != 0 && w.monitor.handle == cursor_monitor)
            .map(|(k, _)| k.clone())
            .or_else(|| self.owner_key.clone());
        if let Some(key) = key
            && let Some(win) = self.windows.get_mut(&key)
        {
            win.toggle_open();
        }
    }

    /// `true` while at least one island window exists (the feature is enabled).
    pub fn has_islands(&self) -> bool {
        !self.windows.is_empty()
    }

    /// Called from the dock's widget tick right after it advanced the shared runtime:
    /// visible islands re-render dirty surfaces, hidden islands do nothing.
    pub fn tick(&mut self) {
        if is_modal_flyout_active() {
            return;
        }
        for win in self.windows.values_mut() {
            win.tick();
        }
    }

    pub fn destroy_all(&mut self) {
        for (_, win) in self.windows.drain() {
            win.destroy();
        }
        self.owner_key = None;
    }
}

/// HUD monitor, notification mirror and privacy probe of the owner island. Every probe is
/// gated by its own setting, so a disabled feature does no work and keeps no state.
struct IslandServices {
    hud: HudMonitor,
    notifications: NotificationMirror,
    privacy: PrivacyProbe,
    last_poll: Option<Instant>,
}

impl IslandServices {
    fn new() -> Self {
        Self {
            hud: HudMonitor::new(),
            notifications: NotificationMirror::new(),
            privacy: PrivacyProbe::new(),
            last_poll: None,
        }
    }
}

/// What the island window currently draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presentation {
    /// Header with widget pages (and the expanded detail card).
    Full,
    /// Slim resting capsule (`DynamicIslandIdleMode::Minimal`).
    Minimal,
    /// Transient overlay (agent approval / HUD / notification).
    Overlay,
}

impl Drop for DynamicIslandManager {
    fn drop(&mut self) {
        self.destroy_all();
    }
}

struct SlideAnim {
    from_px: f32,
    to_px: f32,
    start: Instant,
}

struct DynamicIslandWindow {
    hwnd: usize,
    monitor: MonitorInfo,
    all_monitors: Vec<MonitorInfo>,
    settings: DockSettings,
    is_dark: bool,
    reload_msg: u32,
    runtime: SharedRuntime,
    dcomp: DcompEngine,
    spatial_glass: Option<design::spatial_glass::SpatialGlassHost>,
    chrome_node: VisualNode,
    viewport_node: VisualNode,
    viewport_clip: IDCompositionRectangleClip,
    /// One visual per widget (`page_ids`), positioned by its page group.
    page_nodes: Vec<VisualNode>,
    page_ids: Vec<String>,
    /// Pages: `Paged` = one widget each, `Strip` = packed widget indices.
    groups: Vec<Vec<usize>>,
    current_page: usize,
    /// Widget (index inside the current page) whose details the expanded card shows.
    focus_slot: usize,
    width_dips: f32,
    slide_offset_px: f32,
    slide_anim: Option<SlideAnim>,
    pointer_down: bool,
    is_dragging: bool,
    drag_start_x: f32,
    drag_start_page: usize,
    hovered_btn: Option<IslandButton>,
    pressed_btn: Option<IslandButton>,
    hovered_chip: Option<usize>,
    hovered_widget_sub: Option<u8>,
    pressed_chip: Option<usize>,
    hovered_action: Option<usize>,
    pressed_action: Option<usize>,
    tracking_mouse: bool,
    is_visible: bool,
    target_hidden: bool,
    hide_progress: f32,
    hide_last_tick: Option<Instant>,
    /// Expanded-details state machine. Shared with the dock host so Escape and
    /// paging follow one policy.
    island: IslandState,
    details_node: VisualNode,
    detail_page_id: Option<String>,
    detail_spec: Option<IslandDetailSpec>,
    detail_layout: Option<IslandDetailLayout>,
    /// Pairs the hook-routed wheel notch with its `WM_MOUSEWHEEL` twin.
    wheel_dedup: WheelDedup,
    last_wheel_page: Option<Instant>,
    /// Monitor refresh rate cached at creation / display change (no per-frame
    /// `EnumDisplaySettingsW`).
    refresh_hz: u32,
    last_fullscreen_scan: Option<Instant>,
    fullscreen_cached: bool,
    /// Surface cache keys: an unchanged key skips the redraw, and a render pass that
    /// changed nothing skips `IDCompositionDevice::Commit`.
    page_keys: Vec<String>,
    page_pos: Vec<(f32, f32)>,
    chrome_key: String,
    detail_key: String,
    fast_tick: bool,
    /// A modal widget flyout opened from this island is running.
    flyout_open: bool,
    /// `DynamicIslandScale` (`0.8..=1.35`), folded into the effective DPI.
    scale: f32,
    idle_mode: DynamicIslandIdleMode,
    closed_display: DynamicIslandClosedDisplay,
    /// Shared services; `Some` only on the owner island.
    services: Option<Box<IslandServices>>,
    /// Lazily created overlay visual; its surface is released whenever no overlay shows.
    overlay_node: Option<VisualNode>,
    active_overlay: Option<Box<IslandOverlay>>,
    overlay_queue: VecDeque<IslandOverlay>,
    overlay_layout: Option<IslandOverlayLayout>,
    overlay_key: String,
    hovered_overlay_btn: Option<IslandOverlayButton>,
    pressed_overlay_btn: Option<IslandOverlayButton>,
    /// Agent request dismissed with Escape or answered; not shown again while pending.
    dismissed_agent_request: Option<String>,
    /// Presentation whose visuals are currently attached (viewport / details / overlay).
    presentation: Presentation,
    /// Opened by hover, click, edge hover, file drag or the toggle hotkey; cleared by
    /// auto-close. Minimal shows Full content and Closed is visible while revealed.
    revealed: bool,
    pinned: bool,
    pointer_inside: bool,
    privacy_dots: u8,
    waveform_bars: [u8; 4],
    waveform_timer: bool,
    minimal_text: Option<String>,
    minimal_is_lyric: bool,
    gulp_start: Option<Instant>,
}

impl DynamicIslandWindow {
    fn create(
        monitor: MonitorInfo,
        widget_ids: Vec<String>,
        settings: &DockSettings,
        all_monitors: &[MonitorInfo],
        is_dark: bool,
        reload_msg: u32,
        runtime: SharedRuntime,
    ) -> Option<Box<Self>> {
        let class_name = wide("OpenTaskBarDynamicIsland");
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let ex_style =
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP;
        let init_w = (MIN_COMPACT_WIDTH_DIPS * monitor.dpi_scale).ceil() as i32;
        let init_h = (COMPACT_HEIGHT_DIPS * monitor.dpi_scale).ceil() as i32;
        let init_x = monitor.left + (monitor.width() - init_w) / 2;
        let init_y = monitor.top;

        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                class_name.as_ptr(),
                wide("Dynamic Island").as_ptr(),
                WS_POPUP,
                init_x,
                init_y,
                init_w,
                init_h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return None;
        }
        let hwnd_val = hwnd as usize;
        win32::configure_native_dock_hwnd(hwnd_val);

        let dcomp = match DcompEngine::new(hwnd_val) {
            Ok(engine) => engine,
            Err(err) => {
                opentaskbar_core::log::error(format!(
                    "DynamicIsland DcompEngine init failed on {}: {err}",
                    monitor.device_name
                ));
                unsafe {
                    DestroyWindow(hwnd);
                }
                return None;
            }
        };

        let chrome_node = dcomp.create_node().ok()?;
        let viewport_node = dcomp.create_node().ok()?;
        let viewport_clip = dcomp.create_rect_clip().ok()?;
        unsafe {
            let _ = viewport_node.visual.SetClip(&viewport_clip);
        }
        dcomp.add_child(&dcomp.root, &chrome_node);
        dcomp.add_child(&dcomp.root, &viewport_node);
        let details_node = dcomp.create_node().ok()?;
        details_node.hide();
        dcomp.add_child(&dcomp.root, &details_node);

        let top_flush = settings.dynamic_island_style() == DynamicIslandStyle::Notch;
        let (tint, rim) = island_theme_tint(settings);
        let glass_cfg = design::spatial_glass::SpatialGlassConfig::from_settings(
            settings,
            settings.dynamic_island_background,
            settings.dynamic_island_opacity,
            is_dark,
            top_flush,
        )
        .with_theme_tint(tint, rim);
        let spatial_glass = if glass_cfg.enabled {
            design::spatial_glass::SpatialGlassHost::attach(hwnd_val, false, glass_cfg)
        } else {
            None
        };
        let refresh_hz = win32::monitor_refresh_hz(&monitor.device_name);

        let mut boxed = Box::new(Self {
            hwnd: hwnd_val,
            monitor,
            all_monitors: all_monitors.to_vec(),
            settings: settings.clone(),
            is_dark,
            reload_msg,
            runtime,
            dcomp,
            spatial_glass,
            chrome_node,
            viewport_node,
            viewport_clip,
            page_nodes: Vec::new(),
            page_ids: Vec::new(),
            groups: Vec::new(),
            current_page: 0,
            focus_slot: 0,
            width_dips: MIN_COMPACT_WIDTH_DIPS,
            slide_offset_px: 0.0,
            slide_anim: None,
            pointer_down: false,
            is_dragging: false,
            drag_start_x: 0.0,
            drag_start_page: 0,
            hovered_btn: None,
            pressed_btn: None,
            hovered_chip: None,
            hovered_widget_sub: None,
            pressed_chip: None,
            hovered_action: None,
            pressed_action: None,
            tracking_mouse: false,
            is_visible: false,
            target_hidden: true,
            hide_progress: 1.0,
            hide_last_tick: None,
            island: IslandState::new(widget_ids.clone()),
            details_node,
            detail_page_id: None,
            detail_spec: None,
            detail_layout: None,
            wheel_dedup: WheelDedup::default(),
            last_wheel_page: None,
            refresh_hz,
            last_fullscreen_scan: None,
            fullscreen_cached: false,
            page_keys: Vec::new(),
            page_pos: Vec::new(),
            chrome_key: String::new(),
            detail_key: String::new(),
            fast_tick: false,
            flyout_open: false,
            scale: settings.dynamic_island_scale(),
            idle_mode: settings.dynamic_island_idle_mode(),
            closed_display: settings.dynamic_island_closed_display(),
            services: None,
            overlay_node: None,
            active_overlay: None,
            overlay_queue: VecDeque::new(),
            overlay_layout: None,
            overlay_key: String::new(),
            hovered_overlay_btn: None,
            pressed_overlay_btn: None,
            dismissed_agent_request: None,
            presentation: Presentation::Full,
            revealed: false,
            pinned: false,
            pointer_inside: false,
            privacy_dots: 0,
            waveform_bars: [0; 4],
            waveform_timer: false,
            minimal_text: None,
            minimal_is_lyric: false,
            gulp_start: None,
        });

        let ptr = (&mut *boxed as *mut Self) as isize;
        unsafe {
            set_window_long_ptr(hwnd, GWLP_USERDATA, ptr);
            SetTimer(hwnd, TIMER_VISIBILITY, 400, None);
        }
        // The island lives on the dock UI thread: let the dock's `WH_MOUSE_LL`
        // hook route wheel notches here as well.
        super::register_island_wheel_hwnd(hwnd_val);

        boxed.apply_internal(widget_ids, settings, is_dark);
        Some(boxed)
    }

    fn apply(
        &mut self,
        monitor: MonitorInfo,
        widget_ids: Vec<String>,
        settings: &DockSettings,
        all_monitors: &[MonitorInfo],
        is_dark: bool,
    ) {
        self.monitor = monitor;
        self.all_monitors = all_monitors.to_vec();
        // Settings reloads and display changes are the only events that can change
        // the refresh rate; never query it per animation frame.
        self.refresh_hz = win32::monitor_refresh_hz(&self.monitor.device_name);
        self.apply_internal(widget_ids, settings, is_dark);
    }

    fn apply_internal(&mut self, widget_ids: Vec<String>, settings: &DockSettings, is_dark: bool) {
        self.settings = settings.clone();
        self.is_dark = is_dark;
        self.scale = settings.dynamic_island_scale();
        self.idle_mode = settings.dynamic_island_idle_mode();
        self.closed_display = settings.dynamic_island_closed_display();
        if self.idle_mode != DynamicIslandIdleMode::Minimal {
            self.minimal_text = None;
            self.minimal_is_lyric = false;
        }
        // N2: the island takes file drops only while the Shelf is enabled.
        // SAFETY: `self.hwnd` is this island's live window.
        unsafe {
            DragAcceptFiles(self.hwnd as HWND, i32::from(settings.shelf_enabled()));
            if !settings.dynamic_island_hover_expand() {
                KillTimer(self.hwnd as HWND, TIMER_HOVER_EXPAND);
            }
        }
        // Theme, font, colour or style may have changed: every cached surface is stale.
        self.invalidate_surfaces();

        let display_order = settings
            .dynamic_island_widget_orders
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&self.monitor.device_name))
            .map(|(_, v)| v.as_slice());
        // reference passes displayOrder, widgetOrder, defaultOrder and timedOrder. The
        // timed widgets are already part of DEFAULT_WIDGET_ORDER; adding the
        // persisted widget order restores user reordering in the island.
        let ordered = policy::order_widgets(
            &widget_ids,
            display_order,
            Some(&settings.widget_order),
            DEFAULT_WIDGET_ORDER,
        );

        let selected_id = self.current_widget_id().map(str::to_owned);
        if ordered != self.page_ids {
            self.page_ids = ordered;
            self.groups.clear();
            self.hovered_chip = None;
            self.pressed_chip = None;
            self.rebuild_page_nodes();
            self.relayout();
            let target = selected_id
                .and_then(|sel| {
                    self.page_ids
                        .iter()
                        .position(|id| island_widget_matches(id, &sel))
                })
                .and_then(|idx| self.groups.iter().position(|g| g.contains(&idx)))
                .unwrap_or(0);
            self.current_page = target.min(self.groups.len().saturating_sub(1));
            self.focus_slot = 0;
            self.slide_anim = None;
        }
        // Keep the shared state machine's page view in sync without losing the
        // expanded flag: a settings reload must not silently collapse the island.
        self.island.page_ids = self.page_ids.clone();
        self.island.page_index = self.current_page;

        self.apply_glass_config();
        self.relayout();
        self.slide_offset_px = -(self.current_page as f32) * self.viewport_width_px();
        self.position_window();
        self.render_all();
        let vis_interval_ms = if settings.dynamic_island_auto_hide {
            120
        } else {
            400
        };
        unsafe {
            SetTimer(self.hwnd as HWND, TIMER_VISIBILITY, vis_interval_ms, None);
        }
        self.refresh_visibility();
    }

    fn invalidate_surfaces(&mut self) {
        self.chrome_key.clear();
        self.detail_key.clear();
        for key in &mut self.page_keys {
            key.clear();
        }
    }

    fn top_flush(&self) -> bool {
        self.settings.dynamic_island_style() == DynamicIslandStyle::Notch
    }

    fn glass_config(&self) -> design::spatial_glass::SpatialGlassConfig {
        let (tint, rim) = island_theme_tint(&self.settings);
        design::spatial_glass::SpatialGlassConfig::from_settings(
            &self.settings,
            self.settings.dynamic_island_background,
            self.settings.dynamic_island_opacity,
            self.is_dark,
            self.top_flush(),
        )
        .with_theme_tint(tint, rim)
    }

    fn apply_glass_config(&mut self) {
        let cfg = self.glass_config();
        if cfg.enabled {
            if let Some(ref mut glass) = self.spatial_glass {
                glass.apply_config(cfg);
            } else {
                self.spatial_glass =
                    design::spatial_glass::SpatialGlassHost::attach(self.hwnd, false, cfg);
            }
        } else {
            self.spatial_glass = None;
        }
    }

    fn glass_host_active(&self) -> bool {
        self.spatial_glass
            .as_ref()
            .is_some_and(design::spatial_glass::SpatialGlassHost::is_enabled)
    }

    /// Capsule origin inside the window (DIPs): `Notch` is flush at `(0, 0)`, `Floating`
    /// sits `8 DIP` below the monitor top with a transparent shadow margin around it.
    fn origin_dips(&self) -> (f32, f32) {
        if self.top_flush() {
            (0.0, 0.0)
        } else {
            (FLOATING_MARGIN_DIPS, FLOATING_GAP_DIPS)
        }
    }

    /// Effective DPI: monitor scale × `DynamicIslandScale` (pill, chips and fonts scale
    /// together; hit testing divides by the same factor).
    fn dpi(&self) -> f32 {
        (self.monitor.dpi_scale * self.scale).max(0.5)
    }

    fn current_presentation(&self) -> Presentation {
        if self.active_overlay.is_some() {
            Presentation::Overlay
        } else if self.idle_mode == DynamicIslandIdleMode::Minimal
            && !self.revealed
            && !self.pinned
            && !self.island.expanded
            && !self.flyout_open
        {
            Presentation::Minimal
        } else {
            Presentation::Full
        }
    }

    /// Side reserve of the Minimal capsule (privacy dots left, waveform right).
    fn minimal_side_reserve(&self) -> f32 {
        let dots: f32 = match self.privacy_dots & 0b11 {
            0 => 0.0,
            0b11 => 22.0,
            _ => 12.0,
        };
        let wave = if self.waveform_timer { 22.0 } else { 0.0 };
        dots.max(wave)
    }

    /// Visible capsule width for the current presentation.
    fn capsule_width(&self) -> f32 {
        match self.current_presentation() {
            Presentation::Full => self.width_dips,
            Presentation::Minimal => {
                minimal_width_for(self.minimal_text.as_deref(), self.minimal_side_reserve())
            }
            Presentation::Overlay => self
                .overlay_layout
                .as_ref()
                .map_or(self.width_dips, |layout| layout.width),
        }
    }

    fn height_dips(&self) -> f32 {
        match self.current_presentation() {
            Presentation::Full => policy::island_height_dips(self.island.expanded),
            Presentation::Minimal => MINIMAL_HEIGHT_DIPS,
            Presentation::Overlay => self
                .overlay_layout
                .as_ref()
                .map_or(COMPACT_HEIGHT_DIPS, |layout| layout.height),
        }
    }

    fn corner_radius_dips(&self) -> f32 {
        match self.current_presentation() {
            Presentation::Full => {
                if self.top_flush() {
                    island_geo::BOTTOM_RADIUS_DIPS
                } else if self.island.expanded {
                    FLOATING_EXPANDED_RADIUS_DIPS
                } else {
                    COMPACT_HEIGHT_DIPS * 0.5
                }
            }
            Presentation::Minimal | Presentation::Overlay => {
                let h = self.height_dips();
                if self.top_flush() {
                    island_geo::BOTTOM_RADIUS_DIPS.min(h * 0.5)
                } else {
                    (h * 0.5).min(FLOATING_EXPANDED_RADIUS_DIPS)
                }
            }
        }
    }

    /// Pin toggle is drawn and clickable while hovered, expanded or pinned (Full only).
    fn pin_visible(&self) -> bool {
        self.current_presentation() == Presentation::Full
            && (self.pointer_inside || self.island.expanded || self.pinned)
    }

    fn rebuild_page_nodes(&mut self) {
        self.dcomp.remove_all_children(&self.viewport_node.visual);
        self.page_nodes.clear();
        self.page_keys.clear();
        self.page_pos.clear();
        for _ in &self.page_ids {
            if let Ok(node) = self.dcomp.create_node() {
                self.dcomp.add_child(&self.viewport_node.visual, &node);
                self.page_nodes.push(node);
                self.page_keys.push(String::new());
                self.page_pos.push((f32::NAN, f32::NAN));
            }
        }
    }

    fn widget_widths(&self) -> Vec<f32> {
        let mut runtime = self.runtime;
        let rt = runtime.get();
        self.page_ids
            .iter()
            .map(|id| sanitize_width(rt.widget_width_dips(id)))
            .collect()
    }

    fn monitor_width_dips(&self) -> f32 {
        self.monitor.width() as f32 / self.dpi()
    }

    fn layout_mode(&self) -> DynamicIslandLayout {
        self.settings.dynamic_island_layout()
    }

    /// Widget index (into `page_ids`) the detail card / wheel / context menu target.
    fn current_widget_index(&self) -> Option<usize> {
        let group = self.groups.get(self.current_page)?;
        if let Some(hovered) = self.hovered_chip.filter(|idx| group.contains(idx))
            && !self.island.expanded
        {
            return Some(hovered);
        }
        group
            .get(self.focus_slot.min(group.len().saturating_sub(1)))
            .copied()
    }

    fn current_widget_id(&self) -> Option<&str> {
        self.current_widget_index()
            .and_then(|idx| self.page_ids.get(idx))
            .map(String::as_str)
    }

    /// Re-packs pages (Strip widths can change at runtime) and resizes the window
    /// when the island width changed. Returns `true` when the geometry changed.
    fn relayout(&mut self) -> bool {
        let widths = self.widget_widths();
        let groups = match self.layout_mode() {
            DynamicIslandLayout::Paged => paged_groups(widths.len()),
            DynamicIslandLayout::Strip => {
                let overhead = island_width_for_content(0.0, true, true) - sanitize_width(0.0);
                let budget = (strip_max_width_dips(self.monitor_width_dips()) - overhead).max(80.0);
                pack_strip_pages(&widths, budget, STRIP_GAP_DIPS)
            }
        };
        let mut changed = false;
        if groups != self.groups {
            self.groups = groups;
            self.current_page = self.current_page.min(self.groups.len().saturating_sub(1));
            self.focus_slot = 0;
            self.slide_anim = None;
            changed = true;
        }
        let width = self.width_for_current_page(&widths);
        if (width - self.width_dips).abs() > 0.5 {
            self.width_dips = width;
            changed = true;
        }
        if changed {
            self.chrome_key.clear();
            self.detail_key.clear();
            if self.slide_anim.is_none() && !self.is_dragging {
                self.slide_offset_px = -(self.current_page as f32) * self.viewport_width_px();
            }
        }
        changed
    }

    fn width_for_current_page(&self, widths: &[f32]) -> f32 {
        let monitor_dips = self.monitor_width_dips();
        // An island with no assigned widgets stays a compact hint chip.
        if self.page_ids.is_empty() {
            return policy::clamp_window_width(
                220.0,
                monitor_dips,
                180.0,
                MAX_WIDTH_DIPS,
                island_geo::EDGE_INSET_DIPS,
            );
        }
        let strip = self.layout_mode() == DynamicIslandLayout::Strip;
        let content = if strip {
            // A stable Strip width: paging never makes the pill jump.
            self.groups
                .iter()
                .map(|g| group_content_width(widths, g, STRIP_GAP_DIPS))
                .fold(0.0_f32, f32::max)
        } else {
            self.groups
                .get(self.current_page)
                .map_or(0.0, |g| group_content_width(widths, g, STRIP_GAP_DIPS))
        };
        let has_paging = self.groups.len() > 1;
        let mut width = island_width_for_content(content, has_paging, true);
        let (min, max) = if self.island.expanded {
            width = width.max(island_geo::EXPANDED_PREFERRED_MIN_DIPS);
            (island_geo::EXPANDED_MIN_WIDTH_DIPS, MAX_WIDTH_DIPS)
        } else if strip {
            (
                MIN_COMPACT_WIDTH_DIPS,
                strip_max_width_dips(monitor_dips).max(MIN_COMPACT_WIDTH_DIPS),
            )
        } else {
            (MIN_COMPACT_WIDTH_DIPS, MAX_WIDTH_DIPS)
        };
        policy::clamp_window_width(width, monitor_dips, min, max, island_geo::EDGE_INSET_DIPS)
    }

    fn header_layout(&self) -> IslandHeaderLayout {
        island_header_layout(
            self.width_dips,
            COMPACT_HEIGHT_DIPS,
            self.groups.len(),
            self.current_page,
            has_expandable_content(self.page_ids.first().map(String::as_str)),
        )
    }

    fn viewport_width_dips(&self) -> f32 {
        self.header_layout().content_width()
    }

    fn viewport_width_px(&self) -> f32 {
        self.viewport_width_dips() * self.dpi()
    }

    /// Window size in physical pixels: the capsule plus the Floating shadow margin.
    fn window_size_px(&self) -> (i32, i32) {
        let dpi = self.dpi();
        let (ox, oy) = self.origin_dips();
        let w = ((self.capsule_width() + ox * 2.0) * dpi).ceil().max(1.0) as i32;
        let h = ((self.height_dips() + oy + ox) * dpi).ceil().max(1.0) as i32;
        (w, h)
    }

    fn hide_slide_px(&self) -> f32 {
        let (_, oy) = self.origin_dips();
        (self.height_dips() + oy + 6.0) * self.dpi()
    }

    fn position_window(&mut self) {
        let dpi = self.dpi();
        let (win_w, win_h) = self.window_size_px();
        let x = self.monitor.left + (self.monitor.width() - win_w) / 2;
        let y = self.monitor.top;
        unsafe {
            SetWindowPos(
                self.hwnd as HWND,
                HWND_TOPMOST,
                x,
                y,
                win_w,
                win_h,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        // Flat page viewport clip: sliding chips are never diagonally cut by the
        // capsule's rounded corners (the chips never reach the corners anyway).
        let header = self.header_layout();
        DcompEngine::update_rounded_clip_rect(
            &self.viewport_clip,
            header.content_left * dpi,
            0.0,
            header.content_right * dpi,
            COMPACT_HEIGHT_DIPS * dpi,
            0.0,
            0.0,
        );
        if self.slide_anim.is_none() && !self.is_dragging {
            self.slide_offset_px = -(self.current_page as f32) * self.viewport_width_px();
        }
        self.sync_glass_rect();
        self.apply_hide_offset();
    }

    fn sync_glass_rect(&mut self) {
        let dpi = self.dpi();
        let (ox, oy) = self.origin_dips();
        let w_px = (self.capsule_width() * dpi).ceil().max(1.0);
        let h_px = (self.height_dips() * dpi).ceil().max(1.0);
        let radius_px = self.corner_radius_dips() * dpi;
        let offset_y = -(self.hide_progress * self.hide_slide_px()).round();
        if let Some(ref mut glass) = self.spatial_glass {
            glass.sync_rect(
                (ox * dpi).round(),
                (oy * dpi).round(),
                w_px,
                h_px,
                radius_px,
                dpi,
                0.0,
                offset_y,
            );
        }
    }

    fn render_all(&mut self) {
        if !self.is_visible {
            // Hidden islands do zero rendering work; the keys stay stale until shown.
            return;
        }
        let presentation = self.current_presentation();
        let mut dirty = self.sync_presentation_nodes(presentation);
        dirty |= self.render_chrome();
        match presentation {
            Presentation::Full => {
                dirty |= self.render_pages(true);
                dirty |= self.render_details();
            }
            Presentation::Minimal => {}
            Presentation::Overlay => dirty |= self.render_overlay(),
        }
        if dirty {
            self.dcomp.commit();
        }
    }

    /// Attaches the visuals of `presentation`: page viewport + detail card for Full, the
    /// overlay node for Overlay, chrome only for Minimal. The overlay surface is released as
    /// soon as no overlay shows (zero overlay memory and zero commits while idle).
    fn sync_presentation_nodes(&mut self, presentation: Presentation) -> bool {
        if presentation == self.presentation {
            return false;
        }
        let previous = self.presentation;
        self.presentation = presentation;
        if previous == Presentation::Full {
            self.viewport_node.hide();
            self.details_node.hide();
            self.detail_key.clear();
        }
        if previous == Presentation::Overlay {
            self.release_overlay_surface();
        }
        if presentation == Presentation::Full {
            self.viewport_node.set_transform(1.0, 1.0, 0.0, 0.0);
            self.detail_key.clear();
            for key in &mut self.page_keys {
                key.clear();
            }
        }
        self.chrome_key.clear();
        true
    }

    fn release_overlay_surface(&mut self) {
        if let Some(node) = &self.overlay_node {
            node.set_content(None);
            node.hide();
        }
        self.overlay_key.clear();
    }

    /// Draws the active overlay into the lazily created overlay node.
    fn render_overlay(&mut self) -> bool {
        if self.active_overlay.is_none() {
            return false;
        }
        let Some(layout) = self.overlay_layout.clone() else {
            return false;
        };
        let dpi = self.dpi();
        let mut key = String::with_capacity(256);
        if let Some(overlay) = self.active_overlay.as_deref() {
            let _ = write!(
                key,
                "{overlay:?}|{layout:?}|{dpi}|{}|{:?}|{:?}",
                self.is_dark, self.hovered_overlay_btn, self.pressed_overlay_btn,
            );
        }
        if key == self.overlay_key {
            return false;
        }
        if self.overlay_node.is_none() {
            let Ok(node) = self.dcomp.create_node() else {
                return false;
            };
            // Added last: always above chrome, pages and the detail card.
            self.dcomp.add_child(&self.dcomp.root, &node);
            self.overlay_node = Some(node);
        }
        let Some(overlay) = self.active_overlay.as_deref() else {
            return false;
        };
        let content = match overlay {
            IslandOverlay::AgentApproval(req) => {
                let (added, removed) = diff_stats(&req.diff_preview);
                IslandOverlayContent::AgentApproval {
                    agent: &req.agent,
                    project: &req.project,
                    action_title: &req.action_title,
                    detail: &req.detail,
                    added,
                    removed,
                }
            }
            IslandOverlay::Hud(event) => IslandOverlayContent::Hud {
                icon: event.icon,
                title: &event.title,
                subtitle: &event.subtitle,
                progress: event.progress,
                accent_rgb: event.accent_rgb,
            },
            IslandOverlay::Notification(n) => IslandOverlayContent::Notification {
                app_name: &n.app_name,
                title: &n.title,
                body: &n.body,
            },
        };
        let render = IslandOverlayRender {
            content,
            layout: &layout,
            dpi,
            is_dark: self.is_dark,
            hovered: self.hovered_overlay_btn,
            pressed: self.pressed_overlay_btn,
        };
        let result = self.dcomp.create_island_overlay_surface(&render);
        let Some(node) = &self.overlay_node else {
            return false;
        };
        match result {
            Ok(surface) => {
                node.set_content(Some(&surface));
                node.set_transform(1.0, 1.0, 0.0, 0.0);
                self.overlay_key = key;
            }
            Err(_) => {
                node.set_content(None);
                node.hide();
                self.overlay_key.clear();
            }
        }
        true
    }

    fn overlay_max_width(&self) -> f32 {
        self.monitor_width_dips() - 2.0 * island_geo::EDGE_INSET_DIPS
    }

    /// Shows `overlay` now when it outranks (or equals) the active one, otherwise queues it.
    /// Wakes a hidden / Closed island; never renders while a modal flyout pumps messages.
    fn push_overlay(&mut self, overlay: IslandOverlay) {
        let active = self.active_overlay.as_ref().map(|o| o.kind());
        if is_modal_flyout_active() || !overlay_preempts(active, overlay.kind()) {
            self.enqueue_overlay(overlay);
            return;
        }
        self.start_overlay(overlay);
    }

    fn enqueue_overlay(&mut self, overlay: IslandOverlay) {
        // Bursts of the same HUD (volume ticks) coalesce to the newest reading.
        if let IslandOverlay::Hud(event) = &overlay {
            self.overlay_queue
                .retain(|queued| !matches!(queued, IslandOverlay::Hud(q) if q.kind == event.kind));
        }
        while self.overlay_queue.len() >= MAX_QUEUED_OVERLAYS {
            self.overlay_queue.pop_front();
        }
        self.overlay_queue.push_back(overlay);
    }

    fn start_overlay(&mut self, overlay: IslandOverlay) {
        let layout = island_overlay_layout(
            overlay.kind(),
            overlay.has_progress(),
            self.overlay_max_width(),
        );
        let duration = overlay.duration_ms(&self.settings);
        let geometry_changed =
            self.active_overlay.is_none() || self.overlay_layout.as_ref() != Some(&layout);
        self.overlay_layout = Some(layout);
        self.active_overlay = Some(Box::new(overlay));
        self.hovered_overlay_btn = None;
        self.pressed_overlay_btn = None;
        self.overlay_key.clear();
        // SAFETY: `self.hwnd` is this island's live window.
        unsafe {
            if let Some(ms) = duration {
                SetTimer(self.hwnd as HWND, TIMER_OVERLAY, ms.max(500), None);
            } else {
                KillTimer(self.hwnd as HWND, TIMER_OVERLAY);
            }
        }
        if geometry_changed {
            self.on_geometry_changed();
        } else {
            self.render_all();
        }
        self.refresh_visibility();
    }

    /// Ends the active overlay; the next queued one (if any) takes its place, otherwise the
    /// island returns to its resting presentation (or hides again in `Closed` mode).
    fn finish_overlay(&mut self) {
        // SAFETY: `self.hwnd` is this island's live window.
        unsafe {
            KillTimer(self.hwnd as HWND, TIMER_OVERLAY);
        }
        if self.active_overlay.take().is_none() {
            return;
        }
        self.overlay_layout = None;
        self.hovered_overlay_btn = None;
        self.pressed_overlay_btn = None;
        if let Some(next) = self.overlay_queue.pop_front() {
            self.start_overlay(next);
            return;
        }
        self.on_geometry_changed();
        self.refresh_visibility();
    }

    fn on_overlay_timer(&mut self) {
        let sticky = self
            .active_overlay
            .as_ref()
            .is_none_or(|o| o.kind() == IslandOverlayKind::AgentApproval);
        if self.pointer_inside && !sticky {
            // Never yank a pill away from under the pointer; look again shortly.
            // SAFETY: `self.hwnd` is this island's live window.
            unsafe {
                SetTimer(
                    self.hwnd as HWND,
                    TIMER_OVERLAY,
                    OVERLAY_HOVER_RECHECK_MS,
                    None,
                );
            }
            return;
        }
        self.finish_overlay();
    }

    /// Presentation or size change: re-layout, resize and redraw.
    fn on_geometry_changed(&mut self) {
        self.chrome_key.clear();
        self.detail_key.clear();
        self.apply_glass_config();
        self.relayout();
        self.position_window();
        self.render_all();
        self.update_fast_tick();
    }

    fn push_shell_alert(&mut self, app_name: String, title: String, hwnd: isize) {
        let Some(services) = self.services.as_mut() else {
            return;
        };
        let accepted = services.notifications.push_shell_alert(
            app_name,
            title,
            hwnd,
            &self.settings,
            self.settings.focus_mode,
        );
        if let Some(notification) = accepted {
            self.push_overlay(IslandOverlay::Notification(notification));
        }
    }

    /// Owner island only: N9 privacy, N1 HUD, O5 Action Center, N7 agent approval and N8
    /// to-do polls on the existing visibility tick (rate-limited, each gated by its setting).
    fn poll_services(&mut self) {
        if self.services.is_none() || is_modal_flyout_active() {
            return;
        }
        let now = Instant::now();
        let Some(mut services) = self.services.take() else {
            return;
        };
        if services
            .last_poll
            .is_some_and(|last| now.saturating_duration_since(last) < SERVICES_POLL_INTERVAL)
        {
            self.services = Some(services);
            return;
        }
        services.last_poll = Some(now);
        let mut incoming: Vec<IslandOverlay> = Vec::new();

        // N9: zero registry access while disabled.
        let bits = services
            .privacy
            .poll(self.settings.privacy_indicators_enabled())
            .bitmask();
        PRIVACY_BITS.with(|cell| cell.set(bits));

        // N1: the monitor drops all baselines when every HUD is off.
        let volume = if self.settings.hud_volume_enabled() {
            self.runtime.get().audio_volume_snapshot()
        } else {
            None
        };
        let bluetooth = if self.settings.hud_bluetooth_enabled() {
            self.runtime.get().wireless_battery_summary_pairs()
        } else {
            Vec::new()
        };
        if let Some(event) = services.hud.poll(&self.settings, volume, &bluetooth) {
            incoming.push(IslandOverlay::Hud(event));
        }

        // O5: packaged builds only; returns without WinRT work otherwise.
        if let Some(notification) = services
            .notifications
            .poll_msix(&self.settings, self.settings.focus_mode)
        {
            incoming.push(IslandOverlay::Notification(notification));
        }
        self.services = Some(services);

        self.poll_agent_approval(&mut incoming);

        // N8: due to-do reminders become a toast HUD.
        if self.settings.todo_reminders_enabled()
            && let Some(task) = self.runtime.get().poll_todo_reminder(&self.settings)
        {
            let subtitle = task.due_hhmm.clone().unwrap_or_default();
            incoming.push(IslandOverlay::Hud(hud_monitor::make_toast_hud(
                "\u{E823}",
                task.title.clone(),
                subtitle,
                6000,
            )));
        }

        for overlay in incoming {
            self.push_overlay(overlay);
        }
    }

    /// N7: a pending approval shows a sticky banner; a resolved one removes it.
    fn poll_agent_approval(&mut self, incoming: &mut Vec<IslandOverlay>) {
        let enabled =
            self.settings.agent_monitor_enabled() && self.settings.agent_approval_banners();
        let pending = if enabled {
            self.runtime.get().pending_agent_approval()
        } else {
            None
        };
        let showing = match self.active_overlay.as_deref() {
            Some(IslandOverlay::AgentApproval(req)) => Some(req.request_id.clone()),
            _ => None,
        };
        match pending {
            Some(req) => {
                let id = req.request_id.as_str();
                let known = self.dismissed_agent_request.as_deref() == Some(id)
                    || showing.as_deref() == Some(id)
                    || self.overlay_queue.iter().any(
                        |q| matches!(q, IslandOverlay::AgentApproval(r) if r.request_id == id),
                    );
                if !known {
                    incoming.push(IslandOverlay::AgentApproval(req));
                }
            }
            None => {
                self.dismissed_agent_request = None;
                self.overlay_queue
                    .retain(|q| !matches!(q, IslandOverlay::AgentApproval(_)));
                if showing.is_some() {
                    self.finish_overlay();
                }
            }
        }
    }

    /// Mirrors the owner's privacy bitmask into this island's chrome (redraw only on change).
    fn sync_privacy_dots(&mut self) {
        let bits = if self.settings.privacy_indicators_enabled() {
            PRIVACY_BITS.with(std::cell::Cell::get) & 0b11
        } else {
            0
        };
        if bits == self.privacy_dots {
            return;
        }
        self.privacy_dots = bits;
        self.chrome_key.clear();
        if self.current_presentation() == Presentation::Minimal {
            self.on_geometry_changed();
        } else {
            self.render_all();
        }
    }

    /// Minimal capsule line: synced lyric first, then the `DynamicIslandClosedDisplay`
    /// content. Returns `true` when the text changed.
    fn refresh_minimal_text(&mut self) -> bool {
        let (text, is_lyric) = if self.idle_mode == DynamicIslandIdleMode::Minimal {
            self.compute_minimal_text()
        } else {
            (None, false)
        };
        if text == self.minimal_text && is_lyric == self.minimal_is_lyric {
            return false;
        }
        self.minimal_text = text;
        self.minimal_is_lyric = is_lyric;
        self.chrome_key.clear();
        true
    }

    fn compute_minimal_text(&mut self) -> (Option<String>, bool) {
        if self.settings.media_lyrics_pill_enabled()
            && let Some(line) = self.runtime.get().current_lyric_line(&self.settings)
            && !line.trim().is_empty()
        {
            return (Some(truncate_line(&line, 48)), true);
        }
        let text = match self.closed_display {
            DynamicIslandClosedDisplay::MinimalLine | DynamicIslandClosedDisplay::Hidden => None,
            DynamicIslandClosedDisplay::Clock => {
                match self
                    .runtime
                    .get()
                    .chip_spec("clock", &self.settings, self.is_dark)
                {
                    WidgetChipSpec::Clock { time, .. } if !time.trim().is_empty() => Some(time),
                    _ => None,
                }
            }
            DynamicIslandClosedDisplay::Music => {
                match self
                    .runtime
                    .get()
                    .chip_spec("media", &self.settings, self.is_dark)
                {
                    WidgetChipSpec::Media { title, artist, .. } if !title.trim().is_empty() => {
                        let line = if artist.trim().is_empty() {
                            title
                        } else {
                            format!("{} \u{2014} {}", title.trim(), artist.trim())
                        };
                        Some(truncate_line(&line, 40))
                    }
                    _ => None,
                }
            }
            DynamicIslandClosedDisplay::AiUsage => {
                let id = self
                    .page_ids
                    .iter()
                    .find(|id| registry::canonical_widget_id(id).starts_with("aiusage"))
                    .cloned()
                    .unwrap_or_else(|| "aiusage_claude".to_owned());
                match self
                    .runtime
                    .get()
                    .chip_spec(&id, &self.settings, self.is_dark)
                {
                    WidgetChipSpec::AiDual { rows, .. } => rows
                        .first()
                        .map(|row| truncate_line(&format!("{} {:.0}%", row.0, row.1 * 100.0), 32)),
                    _ => None,
                }
            }
        };
        (text, false)
    }

    /// Where the waveform would be drawn right now.
    fn waveform_slot_visible(&self) -> bool {
        match self.current_presentation() {
            Presentation::Minimal => {
                self.minimal_is_lyric || self.closed_display == DynamicIslandClosedDisplay::Music
            }
            Presentation::Full => {
                !self.pin_visible()
                    && self.header_layout().pin.is_some()
                    && self
                        .current_widget_id()
                        .is_some_and(|id| registry::canonical_widget_id(id) == "media")
            }
            Presentation::Overlay => false,
        }
    }

    /// 15 Hz waveform timer: armed only while visible, enabled, in a waveform slot and
    /// a media session is playing.
    fn update_waveform_timer(&mut self) {
        let wanted = self.is_visible
            && !self.flyout_open
            && !is_modal_flyout_active()
            && self.settings.media_waveform_enabled()
            && self.waveform_slot_visible()
            && self.runtime.get().is_media_playing();
        if wanted == self.waveform_timer {
            return;
        }
        self.waveform_timer = wanted;
        // SAFETY: `self.hwnd` is this island's live window.
        unsafe {
            if wanted {
                SetTimer(
                    self.hwnd as HWND,
                    TIMER_WAVEFORM,
                    WAVEFORM_INTERVAL_MS,
                    None,
                );
            } else {
                KillTimer(self.hwnd as HWND, TIMER_WAVEFORM);
            }
        }
        if !wanted {
            self.waveform_bars = [0; 4];
        }
        self.chrome_key.clear();
        if self.current_presentation() == Presentation::Minimal {
            // The waveform reserve changes the Minimal capsule width.
            self.position_window();
        }
        self.render_all();
    }

    fn step_waveform(&mut self) {
        if !self.waveform_timer || !self.is_visible || is_modal_flyout_active() {
            return;
        }
        let bars = self.runtime.get().media_waveform_bars(&self.settings);
        if bars != self.waveform_bars {
            self.waveform_bars = bars;
            if self.render_chrome() {
                self.dcomp.commit();
            }
        }
    }

    /// Follow-mouse: moves this single island to the monitor under the cursor.
    fn follow_cursor_monitor(&mut self) {
        if self.pointer_down || self.is_dragging || self.flyout_open || self.pointer_inside {
            return;
        }
        let handle = cursor_monitor_handle();
        if handle == 0 || handle == self.monitor.handle {
            return;
        }
        let Some(monitor) = self
            .all_monitors
            .iter()
            .find(|m| m.handle == handle)
            .cloned()
        else {
            return;
        };
        self.monitor = monitor;
        self.refresh_hz = win32::monitor_refresh_hz(&self.monitor.device_name);
        self.last_fullscreen_scan = None;
        self.invalidate_surfaces();
        self.relayout();
        self.position_window();
        self.render_all();
    }

    /// `Closed` / hidden-resting island: an edge hover or a file drag near the top edge of
    /// this monitor reveals it.
    fn edge_reveal_requested(&self) -> bool {
        let Some(pt) = cursor_pos() else {
            return false;
        };
        if pt.x < self.monitor.left
            || pt.x >= self.monitor.right
            || pt.y < self.monitor.top
            || pt.y >= self.monitor.bottom
        {
            return false;
        }
        let dpi = self.dpi();
        let half = ((self.width_dips.max(MIN_COMPACT_WIDTH_DIPS) * 0.5 + 24.0) * dpi) as i32;
        let cx = self.monitor.left + self.monitor.width() / 2;
        if (pt.x - cx).abs() > half {
            return false;
        }
        if pt.y <= self.monitor.top + (EDGE_REVEAL_DIPS * dpi).round().max(3.0) as i32 {
            return true;
        }
        // N2: dragging files toward the island opens it so it can take the drop.
        let drag_zone = self.monitor.top + (COMPACT_HEIGHT_DIPS * dpi).round() as i32;
        // SAFETY: plain key state query.
        let lbutton = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) } < 0;
        pt.y <= drag_zone && lbutton && super::external_file_drag_active()
    }

    /// Opens / closes the user reveal (Minimal shows Full content, Closed becomes visible).
    fn set_revealed(&mut self, revealed: bool) {
        if self.revealed == revealed {
            return;
        }
        self.revealed = revealed;
        if revealed && !self.pointer_inside {
            self.arm_auto_close();
        }
        self.on_geometry_changed();
        self.refresh_visibility();
    }

    fn auto_close_delay_ms(&self) -> Option<u32> {
        if self.pinned || self.flyout_open {
            return None;
        }
        let secs = self.settings.dynamic_island_auto_close_secs();
        match (secs, self.revealed, self.island.expanded) {
            (0, true, _) => Some(REVEAL_FALLBACK_CLOSE_MS),
            (0, false, _) | (_, false, false) => None,
            (secs, _, _) => Some(secs.saturating_mul(1000)),
        }
    }

    fn arm_auto_close(&mut self) {
        let delay = self.auto_close_delay_ms();
        // SAFETY: `self.hwnd` is this island's live window.
        unsafe {
            if let Some(ms) = delay {
                SetTimer(self.hwnd as HWND, TIMER_AUTO_CLOSE, ms, None);
            } else {
                KillTimer(self.hwnd as HWND, TIMER_AUTO_CLOSE);
            }
        }
    }

    fn on_auto_close_timer(&mut self) {
        // SAFETY: one-shot timer on this island's live window.
        unsafe {
            KillTimer(self.hwnd as HWND, TIMER_AUTO_CLOSE);
        }
        if self.pointer_inside || self.pinned || self.flyout_open || self.pointer_down {
            return;
        }
        if self.island.expanded {
            self.set_expanded(false);
        }
        if self.revealed {
            self.set_revealed(false);
        }
    }

    fn on_pointer_enter(&mut self) {
        // SAFETY: `self.hwnd` is this island's live window.
        unsafe {
            KillTimer(self.hwnd as HWND, TIMER_AUTO_CLOSE);
        }
        let presentation = self.current_presentation();
        let hover_expand = match presentation {
            // The slim capsule always grows to Full on hover.
            Presentation::Minimal => true,
            Presentation::Full => {
                self.settings.dynamic_island_hover_expand()
                    && !self.island.expanded
                    && !self.page_ids.is_empty()
            }
            Presentation::Overlay => false,
        };
        if hover_expand {
            let delay = self.settings.dynamic_island_hover_delay_ms();
            // SAFETY: `self.hwnd` is this island's live window.
            unsafe {
                SetTimer(self.hwnd as HWND, TIMER_HOVER_EXPAND, delay, None);
            }
        }
        // The pin toggle appears on hover.
        self.chrome_key.clear();
        self.render_all();
        self.update_waveform_timer();
    }

    fn on_hover_expand_timer(&mut self) {
        // SAFETY: one-shot timer on this island's live window.
        unsafe {
            KillTimer(self.hwnd as HWND, TIMER_HOVER_EXPAND);
        }
        if !self.pointer_inside || self.pointer_down || self.flyout_open {
            return;
        }
        match self.current_presentation() {
            Presentation::Minimal => self.set_revealed(true),
            Presentation::Full
                if self.settings.dynamic_island_hover_expand()
                    && !self.island.expanded
                    && !self.page_ids.is_empty() =>
            {
                self.set_expanded(true);
            }
            _ => {}
        }
    }

    fn toggle_pin(&mut self) {
        self.pinned = !self.pinned;
        if self.pinned {
            // SAFETY: `self.hwnd` is this island's live window.
            unsafe {
                KillTimer(self.hwnd as HWND, TIMER_AUTO_CLOSE);
            }
            if self.idle_mode != DynamicIslandIdleMode::Full {
                self.revealed = true;
            }
        } else if !self.pointer_inside {
            self.arm_auto_close();
        }
        self.on_geometry_changed();
        self.refresh_visibility();
    }

    /// Global toggle hotkey: reveal + expand, or collapse + hide again.
    fn toggle_open(&mut self) {
        let open = self.is_visible
            && self.current_presentation() == Presentation::Full
            && (self.island.expanded || self.revealed);
        if open {
            self.pinned = false;
            if self.island.expanded {
                self.set_expanded(false);
            }
            self.revealed = false;
            self.on_geometry_changed();
            self.refresh_visibility();
            return;
        }
        self.revealed = true;
        self.on_geometry_changed();
        self.refresh_visibility();
        if !self.page_ids.is_empty() {
            self.set_expanded(true);
        }
        if !self.pointer_inside {
            self.arm_auto_close();
        }
    }

    /// N2: files dropped on the island go to the Shelf, with a "gulp" bounce and a HUD.
    fn handle_drop_files(&mut self, hdrop: usize) {
        let paths: Vec<PathBuf> = win32::extract_dropped_files(hdrop)
            .into_iter()
            .map(PathBuf::from)
            .collect();
        if paths.is_empty() || !self.settings.shelf_enabled() || is_modal_flyout_active() {
            return;
        }
        let added = self.runtime.get().add_shelf_files(&paths, &self.settings);
        self.start_gulp();
        let title = localized_or("ShelfAddedTitle", "Added to Shelf");
        let subtitle = if added == 0 {
            localized_or("ShelfAlreadyThere", "Already on the Shelf")
        } else {
            format!("+{added}")
        };
        // Package glyph (E7B8).
        self.push_overlay(IslandOverlay::Hud(hud_monitor::make_toast_hud(
            "\u{E7B8}", title, subtitle, 1600,
        )));
        // Land on the Shelf page so the user sees where the files went.
        if let Some(idx) = self
            .page_ids
            .iter()
            .position(|id| registry::canonical_widget_id(id) == "shelf")
            && let Some(page) = self.groups.iter().position(|g| g.contains(&idx))
            && page != self.current_page
        {
            self.set_current_page(page, true);
        }
    }

    fn start_gulp(&mut self) {
        if !effective_animations_enabled(self.settings.animations_enabled, self.settings.focus_mode)
        {
            return;
        }
        self.gulp_start = Some(Instant::now());
        let interval_ms = self.anim_interval_ms().max(8);
        // SAFETY: `self.hwnd` is this island's live window.
        unsafe {
            SetTimer(self.hwnd as HWND, TIMER_GULP, interval_ms, None);
        }
    }

    /// Quick squeeze then a small overshoot around the capsule center.
    fn step_gulp(&mut self) {
        let Some(start) = self.gulp_start else {
            // SAFETY: `self.hwnd` is this island's live window.
            unsafe {
                KillTimer(self.hwnd as HWND, TIMER_GULP);
            }
            return;
        };
        let t = start.elapsed().as_secs_f32() / GULP_DURATION_SECS;
        let done = t >= 1.0;
        let scale = if done {
            1.0
        } else {
            1.0 - GULP_SCALE * (std::f32::consts::TAU * t).sin() * (1.0 - t)
        };
        let dpi = self.dpi();
        let tx = self.capsule_width() * dpi * (1.0 - scale) * 0.5;
        let ty = self.height_dips() * dpi * (1.0 - scale) * 0.5;
        self.chrome_node.set_transform(scale, scale, tx, ty);
        match self.presentation {
            Presentation::Full => self.viewport_node.set_transform(scale, scale, tx, ty),
            Presentation::Overlay => {
                if let Some(node) = &self.overlay_node {
                    node.set_transform(scale, scale, tx, ty);
                }
            }
            Presentation::Minimal => {}
        }
        if done {
            self.gulp_start = None;
            // SAFETY: `self.hwnd` is this island's live window.
            unsafe {
                KillTimer(self.hwnd as HWND, TIMER_GULP);
            }
        }
        self.dcomp.commit();
    }

    /// Overlay buttons under the pointer.
    fn hit_overlay_button(&self, mx: f32, my: f32) -> Option<IslandOverlayButton> {
        if self.current_presentation() != Presentation::Overlay {
            return None;
        }
        self.overlay_layout
            .as_ref()?
            .buttons
            .iter()
            .find(|(_, rect)| rect_contains(*rect, mx, my))
            .map(|(button, _)| *button)
    }

    fn dispatch_overlay_button(&mut self, button: IslandOverlayButton) {
        match button {
            IslandOverlayButton::Dismiss => self.finish_overlay(),
            IslandOverlayButton::Jump => {
                if let Some(IslandOverlay::AgentApproval(req)) = self.active_overlay.as_deref() {
                    focus_target_window(req.terminal_hwnd);
                }
            }
            IslandOverlayButton::Allow
            | IslandOverlayButton::Deny
            | IslandOverlayButton::Always => {
                let Some(IslandOverlay::AgentApproval(req)) = self.active_overlay.as_deref() else {
                    return;
                };
                let id = req.request_id.clone();
                let decision = match button {
                    IslandOverlayButton::Allow => "allow",
                    IslandOverlayButton::Deny => "deny",
                    _ => "always",
                };
                if !self.runtime.get().respond_agent_approval(&id, decision) {
                    opentaskbar_core::log::warn(format!(
                        "Agent approval {id} ({decision}) was not accepted by the runtime"
                    ));
                }
                // Never re-show the answered request while the runtime catches up.
                self.dismissed_agent_request = Some(id);
                self.finish_overlay();
            }
        }
    }

    /// Click on the overlay body (outside its buttons).
    fn activate_overlay_body(&mut self) {
        match self.active_overlay.as_deref() {
            Some(IslandOverlay::Notification(n)) => {
                focus_target_window(n.target_hwnd);
                self.finish_overlay();
            }
            Some(IslandOverlay::Hud(_)) => self.finish_overlay(),
            Some(IslandOverlay::AgentApproval(_)) | None => {}
        }
    }

    fn render_chrome(&mut self) -> bool {
        let dpi = self.dpi();
        let header = self.header_layout();
        let animating = self.slide_anim.is_some() || self.is_dragging;
        let dividers: Vec<f32> = if animating || self.layout_mode() != DynamicIslandLayout::Strip {
            Vec::new()
        } else {
            let widths = self.widget_widths();
            let rects = self.group_chip_rects(self.current_page, &widths, &header);
            rects
                .windows(2)
                .map(|pair| {
                    let (_, left) = pair[0];
                    let (_, right) = pair[1];
                    (left.2 + right.0) * 0.5
                })
                .collect()
        };
        let fill_rgb = island_fill_rgb(&self.settings, self.is_dark);
        let border_rgb = self
            .settings
            .widget_border_color
            .as_deref()
            .and_then(design::parse_hex_rgb)
            .unwrap_or(if self.is_dark {
                (0xff, 0xff, 0xff)
            } else {
                (0x18, 0x18, 0x1b)
            });
        let border_width = self.settings.widget_border_thickness.clamp(0.0, 8.0) as f32;
        let glass_host_active = self.glass_host_active();
        let presentation = self.current_presentation();
        let show_pin = self.pin_visible();
        let waveform_bars = if self.waveform_timer {
            self.waveform_bars
        } else {
            [0; 4]
        };
        let waveform_rect = if presentation == Presentation::Full && !show_pin {
            header.pin
        } else {
            None
        };
        let minimal = presentation == Presentation::Minimal;
        let lyric_pill = if minimal {
            self.minimal_text.as_deref()
        } else {
            None
        };
        let mut key = String::with_capacity(320);
        let _ = write!(
            key,
            "{presentation:?}|{show_pin}|{}|{}|{waveform_bars:?}|{waveform_rect:?}|{lyric_pill:?}|",
            self.pinned, self.privacy_dots,
        );
        let _ = write!(
            key,
            "{:.1}|{:.1}|{dpi}|{}|{:.3}|{:?}|{fill_rgb:?}|{}|{:.1}|{header:?}|{}|{}|{:?}|{:?}|{border_rgb:?}|{border_width:.2}|{glass_host_active}|{dividers:?}",
            self.capsule_width(),
            self.height_dips(),
            self.is_dark,
            self.settings.dynamic_island_opacity,
            self.settings.dynamic_island_background,
            self.top_flush(),
            self.corner_radius_dips(),
            self.current_page,
            self.island.expanded,
            self.hovered_btn,
            self.pressed_btn,
        );
        if key == self.chrome_key {
            return false;
        }
        let render = IslandChromeRender {
            width_dips: self.capsule_width(),
            height_dips: self.height_dips(),
            dpi,
            is_dark: self.is_dark,
            opacity: self.settings.dynamic_island_opacity as f32,
            background: self.settings.dynamic_island_background,
            fill_rgb,
            top_flush: self.top_flush(),
            radius_dips: self.corner_radius_dips(),
            layout: &header,
            current_page: self.current_page,
            expanded: self.island.expanded,
            hovered_btn: self.hovered_btn,
            pressed_btn: self.pressed_btn,
            border_rgb,
            border_width_dips: border_width,
            glass_host_active,
            dividers_dips: &dividers,
            show_pin,
            pinned: self.pinned,
            privacy_dots: self.privacy_dots,
            waveform_bars,
            waveform_rect,
            minimal,
            lyric_pill,
            bare: presentation == Presentation::Overlay,
        };
        match self.dcomp.create_island_chrome_surface(&render) {
            Ok(surf) => {
                self.chrome_node.set_content(Some(&surf));
                self.chrome_key = key;
                true
            }
            Err(_) => false,
        }
    }

    /// Resting chip rects (island DIPs, no slide offset) of one page.
    fn group_chip_rects(
        &self,
        page: usize,
        widths: &[f32],
        header: &IslandHeaderLayout,
    ) -> Vec<(usize, DipRect)> {
        let Some(group) = self.groups.get(page) else {
            return Vec::new();
        };
        let vp_w = header.content_width();
        let content_w = group_content_width(widths, group, STRIP_GAP_DIPS);
        let mut x = header.content_left + ((vp_w - content_w) * 0.5).max(0.0);
        let top = header.chip_top;
        group
            .iter()
            .map(|idx| {
                let w = widths
                    .get(*idx)
                    .copied()
                    .map_or(96.0, sanitize_width)
                    .min(vp_w);
                let rect = (x, top, x + w, top + header.chip_h);
                x += w + STRIP_GAP_DIPS;
                (*idx, rect)
            })
            .collect()
    }

    /// Positions every page and (re)draws the visible ones. `redraw == false` only
    /// draws pages that were never drawn (adjacent pages entering a slide).
    fn render_pages(&mut self, redraw: bool) -> bool {
        if is_modal_flyout_active() {
            return false;
        }
        let dpi = self.dpi();
        let header = self.header_layout();
        let widths = self.widget_widths();
        let vp_w_px = header.content_width() * dpi;
        let animating = self.slide_anim.is_some() || self.is_dragging;
        let border_rgb = self
            .settings
            .widget_border_color
            .as_deref()
            .and_then(design::parse_hex_rgb);
        let border_width = self.settings.widget_border_thickness.clamp(0.0, 8.0) as f32;
        let mut dirty = false;
        let mut key = String::with_capacity(256);
        for page in 0..self.groups.len() {
            let rects = self.group_chip_rects(page, &widths, &header);
            let near =
                page == self.current_page || (animating && page.abs_diff(self.current_page) <= 1);
            let page_shift_px = (page as f32) * vp_w_px + self.slide_offset_px;
            for (idx, (l, t, r, b)) in rects {
                if idx >= self.page_nodes.len() {
                    continue;
                }
                let pos = ((l * dpi + page_shift_px).round(), (t * dpi).round());
                if !near {
                    // Off-screen pages stay parked; they are drawn when they slide in.
                    if self.page_pos[idx] != pos {
                        self.page_nodes[idx].set_transform(1.0, 1.0, pos.0, pos.1);
                        self.page_pos[idx] = pos;
                        dirty = true;
                    }
                    continue;
                }
                if redraw || self.page_keys[idx].is_empty() {
                    let id = &self.page_ids[idx];
                    let spec = self
                        .runtime
                        .get()
                        .chip_spec(id, &self.settings, self.is_dark);
                    let interaction = if self.pressed_chip == Some(idx) {
                        WidgetInteraction::Pressed(self.hovered_widget_sub)
                    } else if self.hovered_chip == Some(idx) {
                        WidgetInteraction::Hovered(self.hovered_widget_sub)
                    } else {
                        WidgetInteraction::Idle
                    };
                    spec.write_cache_key(&mut key);
                    let (w, h) = (r - l, b - t);
                    let _ = write!(key, "|{w:.1}|{h:.1}|{dpi}|{}|{interaction:?}", self.is_dark);
                    if key != self.page_keys[idx]
                        && let Ok(surf) = self.dcomp.create_widget_chip_surface_with_chrome(
                            &spec,
                            w,
                            h,
                            dpi,
                            self.is_dark,
                            interaction,
                            border_rgb,
                            border_width,
                            ChipChrome::Bare,
                        )
                    {
                        self.page_nodes[idx].set_content(Some(&surf));
                        self.page_keys[idx].clone_from(&key);
                        dirty = true;
                    }
                }
                if self.page_pos[idx] != pos {
                    self.page_nodes[idx].set_transform(1.0, 1.0, pos.0, pos.1);
                    self.page_pos[idx] = pos;
                    dirty = true;
                }
            }
        }
        dirty
    }

    fn update_page_positions_only(&mut self) {
        if self.render_pages(false) {
            self.dcomp.commit();
        }
    }

    /// Expanded live detail card (rebuilt from `island_detail_spec` on every tick, cached
    /// by its content) or the "no widgets" hint.
    fn render_details(&mut self) -> bool {
        if is_modal_flyout_active() {
            return false;
        }
        let dpi = self.dpi();
        if self.island.expanded
            && let Some(id) = self.current_widget_id().map(str::to_owned)
        {
            let spec = self.runtime.get().island_detail_spec(&id, &self.settings);
            let open_full = spec.has_full_flyout && has_real_widget_flyout(&id);
            let layout = island_detail_layout(
                self.width_dips,
                COMPACT_HEIGHT_DIPS,
                EXPANDED_HEIGHT_DIPS,
                spec.rows.len(),
                spec.actions.len().min(3) + usize::from(open_full),
            );
            let mut key = String::with_capacity(512);
            let _ = write!(
                key,
                "detail|{id}|{spec:?}|{open_full}|{layout:?}|{dpi}|{}|{}|{:?}|{:?}",
                self.is_dark,
                self.glass_host_active(),
                self.hovered_action,
                self.pressed_action,
            );
            let mut dirty = false;
            if key != self.detail_key {
                let mut shown = spec.clone();
                shown.rows.truncate(4);
                shown.actions.truncate(3);
                let label = loc::t("WidgetOpenDetails");
                let render = IslandDetailRender {
                    spec: &shown,
                    layout: &layout,
                    dpi,
                    is_dark: self.is_dark,
                    glass_host_active: self.glass_host_active(),
                    hovered_button: self.hovered_action,
                    pressed_button: self.pressed_action,
                    open_full_label: &label,
                };
                match self.dcomp.create_island_detail_surface(&render) {
                    Ok(surf) => {
                        self.details_node.set_content(Some(&surf));
                        self.details_node.set_transform(
                            1.0,
                            1.0,
                            (layout.panel.0 * dpi).round(),
                            (layout.panel.1 * dpi).round(),
                        );
                        self.detail_key = key;
                    }
                    Err(_) => {
                        self.details_node.hide();
                        self.detail_key.clear();
                    }
                }
                dirty = true;
            }
            self.detail_page_id = Some(id);
            self.detail_spec = Some(spec);
            self.detail_layout = Some(layout);
            return dirty;
        }

        self.detail_page_id = None;
        self.detail_spec = None;
        self.detail_layout = None;
        if self.page_ids.is_empty() {
            let key = format!("empty|{:.1}|{dpi}|{}", self.width_dips, self.is_dark);
            if key == self.detail_key {
                return false;
            }
            let label = loc::t("DynamicIslandEmpty");
            let text_w = (self.width_dips - 28.0).max(80.0);
            let text_h = COMPACT_HEIGHT_DIPS - 10.0;
            if let Ok(text) = self.dcomp.create_widget_text_surface(
                &label,
                None,
                text_w,
                text_h,
                dpi,
                self.is_dark,
            ) {
                self.details_node.set_content(Some(&text));
                self.details_node.set_transform(
                    1.0,
                    1.0,
                    (14.0 * dpi).round(),
                    (5.0 * dpi).round(),
                );
                self.detail_key = key;
            } else {
                self.details_node.hide();
                self.detail_key.clear();
            }
            return true;
        }
        if self.detail_key.as_str() == "hidden" {
            return false;
        }
        self.details_node.hide();
        "hidden".clone_into(&mut self.detail_key);
        true
    }

    /// `true` while the visible page needs a 1 s refresh.
    fn needs_fast_tick(&mut self) -> bool {
        if !self.is_visible || self.flyout_open || is_modal_flyout_active() {
            return false;
        }
        if self.current_presentation() != Presentation::Full {
            return false;
        }
        if self.island.expanded {
            return true;
        }
        let Some(group) = self.groups.get(self.current_page).cloned() else {
            return false;
        };
        group.iter().any(|idx| {
            let Some(id) = self.page_ids.get(*idx) else {
                return false;
            };
            let spec = self
                .runtime
                .get()
                .chip_spec(id, &self.settings, self.is_dark);
            chip_is_time_sensitive(&spec, &self.settings)
        })
    }

    fn update_fast_tick(&mut self) {
        self.update_waveform_timer();
        let wanted = self.needs_fast_tick();
        if wanted == self.fast_tick {
            return;
        }
        self.fast_tick = wanted;
        unsafe {
            if wanted {
                SetTimer(self.hwnd as HWND, TIMER_WIDGETS, 1000, None);
            } else {
                KillTimer(self.hwnd as HWND, TIMER_WIDGETS);
            }
        }
    }

    fn anim_interval_ms(&self) -> u32 {
        if !effective_animations_enabled(self.settings.animations_enabled, self.settings.focus_mode)
        {
            return 0;
        }
        let hz = self.refresh_hz.max(15);
        let mut fps = self
            .settings
            .animation_fps
            .map(|f| f.clamp(15, hz))
            .unwrap_or(hz);
        if self.settings.animation_fps_cap > 0 {
            fps = fps.min(self.settings.animation_fps_cap as u32);
        }
        (1000 / fps.max(15)).max(4)
    }

    fn apply_hide_offset(&self) {
        let dpi = self.dpi();
        let (ox, oy) = self.origin_dips();
        let offset_y = -(self.hide_progress * self.hide_slide_px()).round();
        self.dcomp.set_root_offset_x((ox * dpi).round());
        self.dcomp.set_root_offset_y((oy * dpi).round() + offset_y);
        if let Some(ref glass) = self.spatial_glass {
            glass.set_root_offset(0.0, offset_y);
        }
        self.dcomp.commit();
    }

    fn step_hide_animation(&mut self) {
        let now = Instant::now();
        let dt = self
            .hide_last_tick
            .map_or(1.0 / 60.0, |prev| now.duration_since(prev).as_secs_f32())
            .clamp(0.001, 0.05);
        self.hide_last_tick = Some(now);
        let target = if self.target_hidden { 1.0 } else { 0.0 };
        let k = 1.0 - (-18.0 * dt).exp();
        let diff = target - self.hide_progress;
        if diff.abs() <= 0.005 {
            self.hide_progress = target;
            self.hide_last_tick = None;
            self.apply_hide_offset();
            unsafe {
                KillTimer(self.hwnd as HWND, TIMER_HIDE_ANIM);
            }
            if self.target_hidden && self.is_visible {
                unsafe {
                    ShowWindow(self.hwnd as HWND, SW_HIDE);
                }
                self.is_visible = false;
                self.update_fast_tick();
            }
        } else {
            self.hide_progress = (self.hide_progress + diff * k).clamp(0.0, 1.0);
            self.apply_hide_offset();
        }
    }

    fn set_current_page(&mut self, index: usize, animate: bool) {
        if self.groups.is_empty() {
            return;
        }
        let target = index.min(self.groups.len() - 1);
        // Capture the previous page and visual drag offset before relayout may
        // change the viewport width (Paged mode with differing adjacent widths).
        let prev_page = self.current_page;
        let prev_vp_px = self.viewport_width_px();
        let prev_slide_offset = self.slide_offset_px;
        self.current_page = target;
        self.focus_slot = 0;
        self.island.page_index = target.min(self.page_ids.len().saturating_sub(1));
        self.hovered_chip = None;
        self.hovered_widget_sub = None;
        self.hovered_action = None;
        self.pressed_action = None;
        self.relayout();
        self.position_window();
        let new_vp_px = self.viewport_width_px();
        let from_px = rebase_slide_offset(prev_slide_offset, prev_page, prev_vp_px, new_vp_px);
        let target_offset = -(target as f32) * new_vp_px;
        if animate
            && effective_animations_enabled(
                self.settings.animations_enabled,
                self.settings.focus_mode,
            )
        {
            self.slide_offset_px = from_px;
            self.slide_anim = Some(SlideAnim {
                from_px,
                to_px: target_offset,
                start: Instant::now(),
            });
            let interval_ms = self.anim_interval_ms();
            unsafe {
                SetTimer(self.hwnd as HWND, TIMER_ANIM, interval_ms, None);
            }
        } else {
            self.slide_anim = None;
            self.slide_offset_px = target_offset;
        }
        self.render_all();
        self.update_fast_tick();
    }

    fn move_page(&mut self, delta: isize) {
        if self.groups.len() <= 1 {
            return;
        }
        let target = (self.current_page as isize + delta).clamp(0, (self.groups.len() - 1) as isize)
            as usize;
        if target != self.current_page {
            self.set_current_page(target, true);
        }
    }

    /// Horizontal wheel / `Shift`+wheel paging (one page per gesture burst).
    fn wheel_page(&mut self, direction: isize) {
        let now = Instant::now();
        if self
            .last_wheel_page
            .is_some_and(|last| now.duration_since(last) < WHEEL_PAGE_DEBOUNCE)
        {
            return;
        }
        self.last_wheel_page = Some(now);
        self.move_page(direction);
    }

    fn step_slide_animation(&mut self) {
        let Some(anim) = &self.slide_anim else {
            unsafe {
                KillTimer(self.hwnd as HWND, TIMER_ANIM);
            }
            return;
        };
        let t = anim.start.elapsed().as_secs_f32() / SLIDE_DURATION_SECS;
        if t >= 1.0 {
            self.slide_offset_px = anim.to_px;
            self.slide_anim = None;
            unsafe {
                KillTimer(self.hwnd as HWND, TIMER_ANIM);
            }
            // Settled: restore the Strip dividers and redraw the now-current page.
            self.render_all();
        } else {
            let e = cubic_ease_out(t);
            self.slide_offset_px = anim.from_px + (anim.to_px - anim.from_px) * e;
            self.update_page_positions_only();
        }
    }

    /// In-place expand/collapse: the window grows from 50 to 246 DIP and shows the
    /// live detail card of the current widget.
    fn set_expanded(&mut self, expanded: bool) {
        if !self.island.set_expanded(expanded) {
            return;
        }
        self.hovered_action = None;
        self.pressed_action = None;
        self.detail_key.clear();
        self.chrome_key.clear();
        self.apply_glass_config();
        self.relayout();
        self.position_window();
        self.render_all();
        self.update_fast_tick();
    }

    fn tick(&mut self) {
        if !self.settings.dynamic_island_enable || is_modal_flyout_active() {
            return;
        }
        if !self.is_visible || self.flyout_open {
            self.update_fast_tick();
            return;
        }
        if self.relayout() {
            self.position_window();
        }
        if self.idle_mode == DynamicIslandIdleMode::Minimal
            && self.refresh_minimal_text()
            && self.current_presentation() == Presentation::Minimal
        {
            self.position_window();
        }
        self.render_all();
        self.update_fast_tick();
    }

    /// Own 1 s timer for time-sensitive pages: advances the shared runtime (cheap:
    /// sensor polls are gated inside) and redraws only what changed.
    fn fast_tick(&mut self) {
        if is_modal_flyout_active() {
            return;
        }
        if !self.is_visible || self.flyout_open {
            self.update_fast_tick();
            return;
        }
        self.runtime.get().tick(&self.settings);
        self.tick();
    }

    fn fullscreen_on_monitor(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last_fullscreen_scan
            .is_none_or(|last| now.duration_since(last) >= FULLSCREEN_SCAN_INTERVAL)
        {
            self.fullscreen_cached = win32::is_any_fullscreen_on_monitor(&self.monitor);
            self.last_fullscreen_scan = Some(now);
        }
        self.fullscreen_cached
    }

    fn refresh_visibility(&mut self) {
        if self.settings.dynamic_island_follow_mouse() {
            self.follow_cursor_monitor();
        }
        // Owner island: HUD / notification / privacy / agent / to-do polls (may start an
        // overlay, which re-enters this function once with the overlay already active).
        self.poll_services();
        if self.active_overlay.is_none()
            && !is_modal_flyout_active()
            && let Some(next) = self.overlay_queue.pop_front()
        {
            self.start_overlay(next);
        }
        self.sync_privacy_dots();
        if self.is_visible
            && self.idle_mode == DynamicIslandIdleMode::Minimal
            && !is_modal_flyout_active()
            && self.refresh_minimal_text()
        {
            if self.current_presentation() == Presentation::Minimal {
                self.position_window();
            }
            self.render_all();
        }

        let fg_probe = win32::try_get_foreground_application_window();
        let fg_on_display = match fg_probe {
            ForegroundProbe::Window(hwnd) => win32::is_window_on_monitor(hwnd, self.monitor.handle),
            ForegroundProbe::DesktopOrEmpty | ForegroundProbe::OwnProcess => false,
        };
        // Both opt-ins suppress the island while a full-screen app owns the display.
        let suppress_fullscreen = self.settings.dynamic_island_hide_in_fullscreen
            || self.settings.dynamic_island_auto_hide;
        let fullscreen = suppress_fullscreen && self.fullscreen_on_monitor();

        let auto_hide_allows = if self.settings.dynamic_island_auto_hide {
            if self.is_dragging || self.pressed_chip.is_some() || self.pressed_btn.is_some() {
                true
            } else {
                let mut pt = POINT { x: 0, y: 0 };
                if unsafe { GetCursorPos(&mut pt) } != 0 {
                    let dpi = self.dpi();
                    let (_, oy) = self.origin_dips();
                    let w_px = (self.capsule_width() * dpi).ceil() as i32;
                    let h_px = ((self.height_dips() + oy) * dpi).ceil() as i32;
                    let left = self.monitor.left + (self.monitor.width() - w_px) / 2;
                    let pad_x = (24.0 * dpi).round() as i32;
                    let in_x = pt.x >= left - pad_x && pt.x <= left + w_px + pad_x;
                    let top_trigger = self.monitor.top + (6.0 * dpi).round().max(4.0) as i32;
                    let keep_bottom = self.monitor.top + h_px + (8.0 * dpi).round() as i32;
                    in_x && pt.y >= self.monitor.top
                        && if self.is_visible {
                            pt.y <= keep_bottom
                        } else {
                            pt.y <= top_trigger
                        }
                } else {
                    false
                }
            }
        } else {
            true
        };

        // Focus Mode hides the island unless Clock/Focus are allowed to stay.
        let focus_allows = !self.settings.focus_mode
            || (self.settings.dynamic_island_show_focus_widgets() && !self.page_ids.is_empty());
        // N4 idle modes: `Closed` (and a `Hidden` Minimal display) rest hidden until revealed.
        let mut idle_visible = match self.idle_mode {
            DynamicIslandIdleMode::Full => true,
            DynamicIslandIdleMode::Minimal => {
                self.closed_display != DynamicIslandClosedDisplay::Hidden
                    || self.revealed
                    || self.pinned
                    || self.privacy_dots != 0
            }
            DynamicIslandIdleMode::Closed => self.revealed || self.pinned,
        };
        if !idle_visible && !is_modal_flyout_active() && self.edge_reveal_requested() {
            self.set_revealed(true);
            idle_visible = true;
        }
        let base_visible = idle_visible
            && auto_hide_allows
            && policy::should_show(
                self.settings.dynamic_island_visibility,
                !fg_on_display,
                suppress_fullscreen,
                fullscreen,
            );
        // Overlays and an explicit open (hotkey, pin) wake the island even when resting
        // hidden, desktop-only or auto-hidden; a full-screen app still wins.
        let woken = self.active_overlay.is_some() || self.revealed || self.pinned;
        let fullscreen_blocks = suppress_fullscreen && fullscreen;
        let should_show = self.settings.dynamic_island_enable
            && focus_allows
            && (base_visible || (woken && !fullscreen_blocks));

        let next_hidden = !should_show;
        let animate = effective_animations_enabled(
            self.settings.animations_enabled,
            self.settings.focus_mode,
        );
        self.target_hidden = next_hidden;

        if !animate {
            unsafe {
                KillTimer(self.hwnd as HWND, TIMER_HIDE_ANIM);
            }
            self.hide_last_tick = None;
            let progress = if next_hidden { 1.0 } else { 0.0 };
            // Only touch (and commit) the visual tree when something actually changed:
            // an idle island issues no DirectComposition commits on this poll.
            let mut changed = (self.hide_progress - progress).abs() > f32::EPSILON;
            self.hide_progress = progress;
            if should_show && !self.is_visible {
                self.show_window();
                changed = true;
            } else if !should_show && self.is_visible {
                unsafe {
                    ShowWindow(self.hwnd as HWND, SW_HIDE);
                }
                self.is_visible = false;
                self.update_fast_tick();
                changed = true;
            }
            if changed {
                self.apply_hide_offset();
            }
            return;
        }

        if should_show && !self.is_visible {
            self.hide_progress = 1.0;
            self.show_window();
            self.apply_hide_offset();
        }

        let target = if next_hidden { 1.0 } else { 0.0 };
        if self.is_visible && (self.hide_progress - target).abs() > 0.005 {
            if self.hide_last_tick.is_none() {
                self.hide_last_tick = Some(Instant::now());
            }
            let interval_ms = self.anim_interval_ms();
            unsafe {
                SetTimer(self.hwnd as HWND, TIMER_HIDE_ANIM, interval_ms, None);
            }
        }
    }

    fn show_window(&mut self) {
        unsafe {
            ShowWindow(self.hwnd as HWND, SW_SHOWNOACTIVATE);
            SetWindowPos(
                self.hwnd as HWND,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE
                    | SWP_NOMOVE
                    | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOSIZE,
            );
        }
        self.is_visible = true;
        // Rendering was skipped while hidden: catch up once, then keep ticking.
        if self.relayout() {
            self.position_window();
        }
        self.render_all();
        self.update_fast_tick();
    }

    /// Converts window-client pixels to capsule-local DIPs.
    fn to_local_dips(&self, mx_px: f32, my_px: f32) -> (f32, f32) {
        let dpi = self.dpi();
        let (ox, oy) = self.origin_dips();
        (mx_px / dpi - ox, my_px / dpi - oy)
    }

    fn hit_button(&self, mx: f32, my: f32) -> Option<IslandButton> {
        if self.current_presentation() != Presentation::Full {
            return None;
        }
        let header = self.header_layout();
        if let Some(rect) = header.expand
            && rect_contains(rect, mx, my)
        {
            return Some(IslandButton::Expand);
        }
        if self.pin_visible()
            && let Some(rect) = header.pin
            && rect_contains(rect, mx, my)
        {
            return Some(IslandButton::Pin);
        }
        if let Some(rect) = header.prev
            && rect_contains(rect, mx, my)
        {
            return Some(IslandButton::Prev);
        }
        if let Some(rect) = header.next
            && rect_contains(rect, mx, my)
        {
            return Some(IslandButton::Next);
        }
        header.dots.iter().find_map(|(page, (l, t, r, b))| {
            let hit = (l - 2.5, t - 4.0, r + 2.5, b + 4.0);
            (rect_contains(hit, mx, my) && *page <= usize::from(u8::MAX))
                .then_some(IslandButton::Dot(*page as u8))
        })
    }

    /// Chip under the pointer on the current page: `(widget index, rect)`.
    fn hit_chip(&self, mx: f32, my: f32) -> Option<(usize, DipRect)> {
        if self.slide_anim.is_some()
            || self.is_dragging
            || self.current_presentation() != Presentation::Full
        {
            return None;
        }
        let header = self.header_layout();
        if mx < header.content_left || mx > header.content_right {
            return None;
        }
        let widths = self.widget_widths();
        self.group_chip_rects(self.current_page, &widths, &header)
            .into_iter()
            .find(|(_, rect)| rect_contains(*rect, mx, my))
    }

    fn hit_action(&self, mx: f32, my: f32) -> Option<usize> {
        if !self.island.expanded || self.current_presentation() != Presentation::Full {
            return None;
        }
        self.detail_layout
            .as_ref()?
            .buttons
            .iter()
            .position(|rect| rect_contains(*rect, mx, my))
    }

    fn open_flyout_for(&mut self, wid: &str) {
        if !has_real_widget_flyout(wid) || self.flyout_open || is_modal_flyout_active() {
            return;
        }
        let dpi = self.dpi();
        let visible = self.visible_island_rect();
        let (anchor_x, anchor_y) = island_flyout_anchor(
            visible.left,
            visible.right - visible.left,
            visible.bottom,
            dpi,
        );
        self.flyout_open = true;
        self.update_fast_tick();
        // Dispatches to the real `widgets::WidgetRuntime::open_widget_flyout_on_edge`
        // implementation (calendar, mixer, stats, AI quotas, custom widgets, ...).
        // `FlyoutEdge::Top` opens the popup downward, directly below the island.
        let changed = {
            let _modal_guard = ModalFlyoutGuard::enter();
            self.runtime.get().open_widget_flyout_on_edge(
                wid,
                self.hwnd,
                self.is_dark,
                anchor_x,
                anchor_y,
                crate::ui::flyout::FlyoutEdge::Top,
                &mut self.settings,
            )
        };
        self.flyout_open = false;
        if changed {
            self.save_and_broadcast();
        } else {
            self.render_all();
            self.update_fast_tick();
        }
    }

    fn open_current_flyout(&mut self) {
        if let Some(wid) = self.current_widget_id().map(str::to_owned) {
            self.open_flyout_for(&wid);
        }
    }

    /// Visible capsule rect in physical screen pixels: the window rect minus the
    /// Floating margin and the auto-hide slide (the content slides, the HWND stays).
    fn visible_island_rect(&self) -> windows::Win32::Foundation::RECT {
        let dpi = self.dpi();
        let (win_w, win_h) = self.window_size_px();
        let (ox, oy) = self.origin_dips();
        let mut rc = windows_sys::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: `self.hwnd` is this island's live window handle.
        let ok = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(self.hwnd as HWND, &mut rc)
        } != 0;
        if !ok || rc.right <= rc.left || rc.bottom <= rc.top {
            rc.left = self.monitor.left + (self.monitor.width() - win_w) / 2;
            rc.top = self.monitor.top;
            rc.right = rc.left + win_w;
            rc.bottom = rc.top + win_h;
        }
        let left = rc.left + (ox * dpi).round() as i32;
        let top = rc.top + (oy * dpi).round() as i32;
        let right = left + (self.capsule_width() * dpi).ceil() as i32;
        let bottom = top + (self.height_dips() * dpi).ceil() as i32;
        let offset_y = (self.hide_progress * self.hide_slide_px()).round() as i32;
        windows::Win32::Foundation::RECT {
            left,
            top: top - offset_y,
            right,
            bottom: (bottom - offset_y).max(top - offset_y),
        }
    }

    /// Context menus open below the visible island (`MenuEdge::Top`).
    fn island_menu_anchor(&self) -> Option<crate::ui::menu::MenuAnchor> {
        Some(crate::ui::menu::MenuAnchor {
            visible_rect: self.visible_island_rect(),
            edge: Some(crate::ui::menu::MenuEdge::Top),
        })
    }

    /// Wheel over the island: horizontal wheel or `Shift`+wheel pages; otherwise
    /// Audio/Media adjust the volume (always, the `wheel_volume_enabled` toggle only
    /// governs dock-wide background wheel) and Keyboard cycles the input layout.
    fn handle_wheel(&mut self, delta: i16, horizontal: bool, shift: bool) {
        if delta == 0 {
            return;
        }
        if horizontal {
            self.wheel_page(if delta > 0 { 1 } else { -1 });
            return;
        }
        if shift {
            self.wheel_page(if delta > 0 { -1 } else { 1 });
            return;
        }
        let Some(wid) = self.current_widget_id().map(str::to_owned) else {
            return;
        };
        match registry::canonical_widget_id(&wid) {
            "audio" | "media" => {
                self.runtime.get().adjust_volume_wheel(i32::from(delta));
                self.render_all();
            }
            "keyboard" => {
                let foreground =
                    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }
                        as usize;
                if foreground != 0 && foreground != self.hwnd {
                    self.runtime.get().set_keyboard_target_hwnd(foreground);
                }
                self.runtime.get().cycle_keyboard_layout(delta < 0);
                self.runtime.get().tick(&self.settings);
                self.render_all();
            }
            _ => {}
        }
    }

    fn save_and_broadcast(&self) {
        if let Err(err) = settings::save_to(&settings::settings_path(), &self.settings) {
            opentaskbar_core::log::error(format!("DynamicIsland settings save failed: {err}"));
            return;
        }
        unsafe {
            if self.reload_msg != 0 {
                windows_sys::Win32::UI::WindowsAndMessaging::SendNotifyMessageW(
                    windows_sys::Win32::UI::WindowsAndMessaging::HWND_BROADCAST,
                    self.reload_msg,
                    0,
                    0,
                );
            }
        }
    }

    fn handle_mouse_move(&mut self, mx_px: f32, my_px: f32) {
        if !self.tracking_mouse {
            self.tracking_mouse = true;
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd as HWND,
                dwHoverTime: 0,
            };
            unsafe {
                TrackMouseEvent(&mut tme);
            }
        }

        let dpi = self.dpi();
        let (mx, my) = self.to_local_dips(mx_px, my_px);
        if let Some(ref mut glass) = self.spatial_glass {
            glass.set_pointer(mx_px, my_px);
        }
        if !self.pointer_inside {
            self.pointer_inside = true;
            self.on_pointer_enter();
        }
        match self.current_presentation() {
            Presentation::Overlay => {
                let next = self.hit_overlay_button(mx, my);
                if next != self.hovered_overlay_btn {
                    self.hovered_overlay_btn = next;
                    self.render_all();
                }
                return;
            }
            Presentation::Minimal => return,
            Presentation::Full => {}
        }

        if self.is_dragging {
            let dx_px = mx_px - self.drag_start_x;
            let vp_w_px = self.viewport_width_px();
            let base = -(self.drag_start_page as f32) * vp_w_px;
            let at_edge = (self.drag_start_page == 0 && dx_px > 0.0)
                || (self.drag_start_page + 1 >= self.groups.len() && dx_px < 0.0);
            let resisted = if at_edge { dx_px * 0.22 } else { dx_px };
            self.slide_offset_px = base + resisted.clamp(-vp_w_px, vp_w_px);
            self.update_page_positions_only();
            return;
        }

        if self.pointer_down
            && self.pressed_btn.is_none()
            && self.pressed_action.is_none()
            && self.groups.len() > 1
            && (mx_px - self.drag_start_x).abs() > 6.0 * dpi
        {
            self.is_dragging = true;
            self.slide_anim = None;
            self.pressed_chip = None;
            self.hovered_chip = None;
            self.chrome_key.clear();
            return;
        }

        let next_btn = self.hit_button(mx, my);
        let next_action = self.hit_action(mx, my);
        let chip_hit = if next_btn.is_none() && next_action.is_none() {
            self.hit_chip(mx, my)
        } else {
            None
        };
        let next_chip = chip_hit.map(|(idx, _)| idx);
        let next_sub = chip_hit.and_then(|(idx, rect)| {
            let id = self.page_ids.get(idx)?;
            chip_sub_zone(id, mx - rect.0, (rect.2 - rect.0).max(1.0))
        });
        if next_btn != self.hovered_btn
            || next_action != self.hovered_action
            || next_chip != self.hovered_chip
            || next_sub != self.hovered_widget_sub
        {
            self.hovered_btn = next_btn;
            self.hovered_action = next_action;
            self.hovered_chip = next_chip;
            self.hovered_widget_sub = next_sub;
            // Strip: the expanded card follows the hovered widget.
            if let Some(idx) = next_chip
                && let Some(slot) = self
                    .groups
                    .get(self.current_page)
                    .and_then(|g| g.iter().position(|i| *i == idx))
            {
                self.focus_slot = slot;
            }
            self.render_all();
        }
    }

    fn handle_mouse_leave(&mut self) {
        if let Some(ref mut glass) = self.spatial_glass {
            glass.clear_pointer();
        }
        self.tracking_mouse = false;
        if self.pointer_inside {
            self.pointer_inside = false;
            // SAFETY: `self.hwnd` is this island's live window.
            unsafe {
                KillTimer(self.hwnd as HWND, TIMER_HOVER_EXPAND);
            }
            self.hovered_overlay_btn = None;
            self.chrome_key.clear();
            self.overlay_key.clear();
            self.render_all();
            self.update_waveform_timer();
            self.arm_auto_close();
        }
        if self.hovered_btn.is_some()
            || self.hovered_chip.is_some()
            || self.hovered_action.is_some()
        {
            self.hovered_btn = None;
            self.hovered_chip = None;
            self.hovered_widget_sub = None;
            self.hovered_action = None;
            self.render_all();
        }
        if self.settings.dynamic_island_auto_hide {
            self.refresh_visibility();
        }
    }

    fn handle_lbutton_down(&mut self, mx_px: f32, my_px: f32) {
        let (mx, my) = self.to_local_dips(mx_px, my_px);
        if let Some(ref mut glass) = self.spatial_glass {
            glass.set_pointer(mx_px, my_px);
        }
        self.pointer_down = true;
        unsafe {
            SetCapture(self.hwnd as HWND);
        }
        match self.current_presentation() {
            Presentation::Overlay => {
                self.pressed_overlay_btn = self.hit_overlay_button(mx, my);
                self.drag_start_x = 0.0;
                self.render_all();
                return;
            }
            Presentation::Minimal => {
                self.drag_start_x = 0.0;
                return;
            }
            Presentation::Full => {}
        }
        if let Some(btn) = self.hit_button(mx, my) {
            self.pressed_btn = Some(btn);
            self.drag_start_x = 0.0;
            self.render_all();
            return;
        }
        if let Some(action) = self.hit_action(mx, my) {
            self.pressed_action = Some(action);
            self.drag_start_x = 0.0;
            self.render_all();
            return;
        }
        self.drag_start_x = mx_px;
        self.drag_start_page = self.current_page;
        if let Some((idx, _)) = self.hit_chip(mx, my) {
            self.pressed_chip = Some(idx);
            self.render_all();
        }
    }

    fn handle_capture_changed(&mut self) {
        let was_dragging = self.is_dragging;
        let had_pressed = self.pressed_btn.is_some()
            || self.pressed_chip.is_some()
            || self.pressed_action.is_some();
        self.pointer_down = false;
        self.is_dragging = false;
        self.drag_start_x = 0.0;
        self.pressed_btn = None;
        self.pressed_chip = None;
        self.pressed_action = None;
        let had_pressed = had_pressed || self.pressed_overlay_btn.take().is_some();
        if was_dragging {
            self.slide_offset_px = -(self.current_page as f32) * self.viewport_width_px();
            self.chrome_key.clear();
        }
        if (was_dragging || had_pressed) && !is_modal_flyout_active() {
            self.render_all();
        }
    }

    fn handle_lbutton_up(&mut self, mx_px: f32, my_px: f32) {
        self.pointer_down = false;
        unsafe {
            ReleaseCapture();
        }
        let dpi = self.dpi();
        let (mx, my) = self.to_local_dips(mx_px, my_px);
        if let Some(ref mut glass) = self.spatial_glass {
            glass.set_pointer(mx_px, my_px);
        }
        match self.current_presentation() {
            Presentation::Overlay => {
                let pressed = self.pressed_overlay_btn.take();
                let hit = self.hit_overlay_button(mx, my);
                match (pressed, hit) {
                    (Some(button), Some(over)) if button == over => {
                        self.dispatch_overlay_button(button);
                    }
                    (None, None) => self.activate_overlay_body(),
                    _ => self.render_all(),
                }
                return;
            }
            Presentation::Minimal => {
                // A click opens the slim capsule to the Full island.
                self.set_revealed(true);
                return;
            }
            Presentation::Full => {}
        }

        if self.is_dragging {
            self.is_dragging = false;
            let dx_px = mx_px - self.drag_start_x;
            let threshold = (36.0 * dpi).max(self.viewport_width_px() * 0.18);
            self.drag_start_x = 0.0;
            self.pressed_chip = None;
            let target = if dx_px <= -threshold {
                (self.drag_start_page + 1).min(self.groups.len().saturating_sub(1))
            } else if dx_px >= threshold {
                self.drag_start_page.saturating_sub(1)
            } else {
                self.drag_start_page
            };
            self.set_current_page(target, true);
            return;
        }
        self.drag_start_x = 0.0;

        if let Some(btn) = self.pressed_btn.take() {
            self.render_all();
            if self.hit_button(mx, my) == Some(btn) {
                match island_button_click_action(btn) {
                    IslandClickAction::PrevPage => self.move_page(-1),
                    IslandClickAction::NextPage => self.move_page(1),
                    IslandClickAction::GoToPage(page) => {
                        if page != self.current_page {
                            self.set_current_page(page, true);
                        }
                    }
                    IslandClickAction::ToggleDetails => {
                        let expanded = self.island.expanded;
                        self.set_expanded(!expanded);
                    }
                    IslandClickAction::TogglePin => self.toggle_pin(),
                }
            }
            return;
        }

        if let Some(action) = self.pressed_action.take() {
            self.render_all();
            if self.hit_action(mx, my) == Some(action) {
                self.dispatch_detail_button(action);
            }
            return;
        }

        if self.page_ids.is_empty() {
            settings_ui::spawn_settings_window();
            return;
        }

        let Some(pressed) = self.pressed_chip.take() else {
            return;
        };
        self.render_all();
        let Some((idx, (l, t, r, b))) = self.hit_chip(mx, my) else {
            return;
        };
        if idx != pressed {
            return;
        }
        let Some(wid) = self.page_ids.get(idx).cloned() else {
            return;
        };
        let rel_x = mx - l;
        let width = (r - l).max(1.0);
        // N6: the album art switches between SMTC sessions (Spotify, browser, ...).
        if media_album_art_hit(&wid, rel_x, b - t) {
            cycle_media_session_async();
            self.render_all();
            return;
        }
        let on_control = chip_sub_zone(&wid, rel_x, width).is_some_and(|zone| {
            // Timers: only the trailing play/pause zone is a control.
            !matches!(
                registry::canonical_widget_id(&wid),
                "focus" | "stopwatch" | "countdown"
            ) || zone == 1
        });
        let outcome =
            self.runtime
                .get()
                .handle_direct_click(&wid, rel_x, width, &mut self.settings);
        match outcome {
            widgets::DirectClickOutcome::SettingsChanged => {
                self.save_and_broadcast();
            }
            widgets::DirectClickOutcome::Handled => {
                // AI chips and a few others return Handled even when a full widget
                // flyout exists; a click on the chip body still opens it (the island
                // never dead-ends), while inline controls stay inline.
                self.render_all();
                self.update_fast_tick();
                if !on_control && has_real_widget_flyout(&wid) {
                    self.open_flyout_for(&wid);
                }
            }
            widgets::DirectClickOutcome::OpenFlyout => {
                self.open_flyout_for(&wid);
            }
        }
    }

    /// Quick action or "open full flyout" button of the expanded detail card.
    fn dispatch_detail_button(&mut self, index: usize) {
        let (Some(spec), Some(page_id)) = (self.detail_spec.clone(), self.detail_page_id.clone())
        else {
            return;
        };
        if let Some(action) = spec.actions.get(index).filter(|_| index < 3) {
            let handled = self.runtime.get().handle_island_detail_action(
                &page_id,
                &action.id,
                &self.settings,
            );
            if handled {
                self.runtime.get().tick(&self.settings);
                self.detail_key.clear();
                self.render_all();
                self.update_fast_tick();
            }
        } else if spec.has_full_flyout {
            self.open_flyout_for(&page_id);
        }
    }

    fn handle_rbutton_up(&mut self) {
        match self.current_presentation() {
            Presentation::Overlay => {
                // Right-click dismisses a timed pill; agent banners need an explicit answer.
                if self
                    .active_overlay
                    .as_ref()
                    .is_some_and(|o| o.kind() != IslandOverlayKind::AgentApproval)
                {
                    self.finish_overlay();
                }
                return;
            }
            Presentation::Minimal => {
                self.set_revealed(true);
                return;
            }
            Presentation::Full => {}
        }
        let Some(wid) = self.current_widget_id().map(str::to_owned) else {
            settings_ui::spawn_settings_window();
            return;
        };
        let reset_label = loc::t("Reset");
        let sound_label = loc::t_fmt("RadioSettings", &[&loc::t("LauncherSound")]);
        let focus_label = loc::t("FocusMode");
        let secondary_label = match registry::canonical_widget_id(&wid) {
            "stopwatch" | "countdown" | "focus" => Some(reset_label.as_str()),
            "audio" => Some(sound_label.as_str()),
            "quicksettings" => Some(focus_label.as_str()),
            _ => None,
        };
        let position = self
            .page_ids
            .iter()
            .position(|id| island_widget_matches(id, &wid));
        let can_earlier = position.is_some_and(|pos| pos > 0);
        let can_later = position.is_some_and(|pos| pos + 1 < self.page_ids.len());
        let placement_key = settings::canonical_widget_setting_id(&wid);

        match win32::show_widget_context_menu(
            self.hwnd,
            self.is_dark,
            &wid,
            secondary_label,
            &self.all_monitors,
            true,
            can_earlier,
            can_later,
            &self.settings,
            self.island_menu_anchor(),
        ) {
            WidgetContextAction::PlaceAllDisplays => {
                self.settings
                    .dynamic_island_placements
                    .retain(|k, _| !island_widget_matches(k, &wid));
                self.settings.dynamic_island_placements.insert(
                    placement_key,
                    settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned(),
                );
                self.save_and_broadcast();
            }
            WidgetContextAction::PlaceOnDisplay(idx) => {
                if let Some(mon) = self.all_monitors.get(idx) {
                    let device = mon.device_name.clone();
                    self.settings
                        .dynamic_island_placements
                        .retain(|k, _| !island_widget_matches(k, &wid));
                    self.settings
                        .dynamic_island_placements
                        .insert(placement_key, device);
                    self.save_and_broadcast();
                }
            }
            WidgetContextAction::MoveToTaskbar => {
                self.settings
                    .dynamic_island_placements
                    .retain(|k, _| !island_widget_matches(k, &wid));
                self.save_and_broadcast();
            }
            WidgetContextAction::MoveEarlier => self.move_widget_order(&wid, -1),
            WidgetContextAction::MoveLater => self.move_widget_order(&wid, 1),
            WidgetContextAction::Remove => {
                self.settings.set_widget_enabled(&wid, false);
                self.save_and_broadcast();
            }
            WidgetContextAction::Secondary => {
                let outcome = self
                    .runtime
                    .get()
                    .handle_secondary_click(&wid, &mut self.settings);
                match outcome {
                    Some(widgets::DirectClickOutcome::SettingsChanged) => {
                        self.save_and_broadcast();
                    }
                    Some(_) => {
                        self.render_all();
                        self.update_fast_tick();
                    }
                    None => {}
                }
            }
            WidgetContextAction::Dock(_) | WidgetContextAction::None => {}
        }
    }

    fn move_widget_order(&mut self, wid: &str, offset: i32) {
        let moved = policy::move_widget(&self.page_ids, wid, offset);
        if moved == self.page_ids {
            return;
        }
        self.settings
            .dynamic_island_widget_orders
            .insert(self.monitor.device_name.clone(), moved);
        self.save_and_broadcast();
    }

    /// Island-local keys: `Ctrl+1..9` jump to a page, `Ctrl+Left/Right` cycle pages,
    /// `Ctrl+P` toggles the pin, `Esc` dismisses an overlay / collapses / unpins / closes,
    /// plain `Left/Right` page.
    fn handle_key(&mut self, vk: u16) {
        if vk == VK_ESCAPE {
            self.handle_escape();
            return;
        }
        // SAFETY: plain key state query.
        let ctrl = unsafe { GetKeyState(i32::from(VK_CONTROL)) } < 0;
        if ctrl {
            if self.current_presentation() == Presentation::Overlay {
                return;
            }
            if let Some(page) = ctrl_digit_page(vk) {
                if page < self.groups.len() && page != self.current_page {
                    self.set_current_page(page, true);
                }
            } else if vk == VK_LEFT || vk == VK_RIGHT {
                let delta = if vk == VK_LEFT { -1 } else { 1 };
                let target = cycle_page_index(self.current_page, self.groups.len(), delta);
                if target != self.current_page {
                    self.set_current_page(target, true);
                }
            } else if vk == u16::from(b'P') {
                self.toggle_pin();
            }
            return;
        }
        if self.current_presentation() != Presentation::Full {
            return;
        }
        if vk == VK_LEFT {
            self.move_page(-1);
        } else if vk == VK_RIGHT {
            self.move_page(1);
        }
    }

    fn handle_escape(&mut self) {
        if let Some(overlay) = self.active_overlay.as_deref() {
            if let IslandOverlay::AgentApproval(req) = overlay {
                self.dismissed_agent_request = Some(req.request_id.clone());
            }
            self.finish_overlay();
            return;
        }
        // Escape collapses the in-place details panel (shared state machine).
        if self.island.pre_process_input(IslandInput::Escape) == InputDisposition::Consumed {
            self.hovered_action = None;
            self.pressed_action = None;
            self.apply_glass_config();
            self.relayout();
            self.position_window();
            self.render_all();
            self.update_fast_tick();
            return;
        }
        if self.pinned {
            self.toggle_pin();
        } else if self.revealed {
            self.set_revealed(false);
        }
    }

    fn destroy(self) {
        super::unregister_island_wheel_hwnd(self.hwnd);
        unsafe {
            KillTimer(self.hwnd as HWND, TIMER_VISIBILITY);
            KillTimer(self.hwnd as HWND, TIMER_WIDGETS);
            KillTimer(self.hwnd as HWND, TIMER_ANIM);
            KillTimer(self.hwnd as HWND, TIMER_HIDE_ANIM);
            KillTimer(self.hwnd as HWND, TIMER_OVERLAY);
            KillTimer(self.hwnd as HWND, TIMER_HOVER_EXPAND);
            KillTimer(self.hwnd as HWND, TIMER_AUTO_CLOSE);
            KillTimer(self.hwnd as HWND, TIMER_WAVEFORM);
            KillTimer(self.hwnd as HWND, TIMER_GULP);
            set_window_long_ptr(self.hwnd as HWND, GWLP_USERDATA, 0);
            DestroyWindow(self.hwnd as HWND);
        }
    }
}

fn register_island_class() {
    let class_name = wide("OpenTaskBarDynamicIsland");
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(island_wnd_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        lpszClassName: class_name.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        RegisterClassExW(&wc);
    }
}

unsafe extern "system" fn island_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(_wnd_guard) = IslandWndProcGuard::try_enter() else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut DynamicIslandWindow;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_NCHITTEST => {
            if state.settings.dynamic_island_style() == DynamicIslandStyle::Floating {
                let x = loword_i16(lparam) as i32;
                let y = hiword_i16(lparam) as i32;
                let visible = state.visible_island_rect();
                if x < visible.left || x >= visible.right || y < visible.top || y >= visible.bottom
                {
                    return HTTRANSPARENT as LRESULT;
                }
            }
            HTCLIENT as LRESULT
        }
        WM_SETCURSOR => {
            if state.hovered_btn.is_some()
                || state.hovered_action.is_some()
                || state.hovered_chip.is_some()
                || state.hovered_overlay_btn.is_some()
            {
                unsafe {
                    SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND));
                }
                return 1;
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CAPTURECHANGED => {
            state.handle_capture_changed();
            0
        }
        WM_MOUSEMOVE => {
            if is_modal_flyout_active() {
                return 0;
            }
            state.handle_mouse_move(loword_i16(lparam), hiword_i16(lparam));
            0
        }
        WM_MOUSELEAVE => {
            if is_modal_flyout_active() {
                return 0;
            }
            state.handle_mouse_leave();
            0
        }
        WM_LBUTTONDOWN => {
            if is_modal_flyout_active() {
                return 0;
            }
            state.handle_lbutton_down(loword_i16(lparam), hiword_i16(lparam));
            0
        }
        WM_LBUTTONUP => {
            if is_modal_flyout_active() {
                return 0;
            }
            state.handle_lbutton_up(loword_i16(lparam), hiword_i16(lparam));
            0
        }
        WM_RBUTTONUP => {
            if is_modal_flyout_active() {
                return 0;
            }
            state.handle_rbutton_up();
            0
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            if is_modal_flyout_active() {
                return 0;
            }
            let horizontal = msg == WM_MOUSEHWHEEL;
            let delta = ((wparam >> 16) & 0xffff) as i16;
            let shift = (wparam & MK_SHIFT) != 0;
            if state
                .wheel_dedup
                .accept(WheelSource::Window, delta, horizontal, Instant::now())
            {
                state.handle_wheel(delta, horizontal, shift);
            }
            0
        }
        WM_APP_ISLAND_WHEEL_ROUTE => {
            if is_modal_flyout_active() {
                return 0;
            }
            // Same payload as the dock route: bit 0 = horizontal, the rest = delta.
            let horizontal = (wparam & 1) != 0;
            let delta = (wparam >> 1) as u32 as i16;
            let (x, y) = super::layout::unpack_cursor_point(lparam);
            let visible = state.visible_island_rect();
            let inside =
                x >= visible.left && x < visible.right && y >= visible.top && y < visible.bottom;
            if inside
                && state.is_visible
                && state
                    .wheel_dedup
                    .accept(WheelSource::Hook, delta, horizontal, Instant::now())
            {
                let shift = unsafe { GetKeyState(i32::from(VK_SHIFT)) } < 0;
                state.handle_wheel(delta, horizontal, shift);
            }
            0
        }
        WM_KEYDOWN => {
            if is_modal_flyout_active() {
                return 0;
            }
            state.handle_key(wparam as u16);
            0
        }
        WM_TIMER => {
            if is_modal_flyout_active() {
                return 0;
            }
            match wparam {
                TIMER_VISIBILITY => state.refresh_visibility(),
                TIMER_WIDGETS => state.fast_tick(),
                TIMER_HIDE_ANIM => state.step_hide_animation(),
                TIMER_ANIM => state.step_slide_animation(),
                TIMER_OVERLAY => state.on_overlay_timer(),
                TIMER_HOVER_EXPAND => state.on_hover_expand_timer(),
                TIMER_AUTO_CLOSE => state.on_auto_close_timer(),
                TIMER_WAVEFORM => state.step_waveform(),
                TIMER_GULP => state.step_gulp(),
                _ => {}
            }
            0
        }
        WM_DROPFILES => {
            if is_modal_flyout_active() {
                // Still release the HDROP; the drop is ignored while a flyout is modal.
                let _ = win32::extract_dropped_files(wparam);
                return 0;
            }
            state.handle_drop_files(wparam);
            0
        }
        WM_DESTROY => 0,
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ---- WS-D expanded-details policy / state machine ------------------------------
// `DynamicIslandPolicy`, `TimedWidgetTile.DetailsRequested`, `GetDetails()`.
// Kept alongside the native Win32 island host above so both renderers share one API.
// Island visibility uses `opentaskbar_core::settings::DynamicIslandVisibilityMode`
// directly; the old local `IslandVisibility` enum duplicated it and was only
// used by a private copy of `should_show`.

/// A single line of the 246px details panel (`WidgetChipDetails` / `GetDetails()`).
#[derive(Clone, Debug, PartialEq)]
pub struct DetailLine {
    pub label: String,
    pub value: String,
    pub accent: bool,
}

impl DetailLine {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            accent: false,
        }
    }

    pub fn accent(mut self) -> Self {
        self.accent = true;
        self
    }
}

/// Per-widget details provider result (`GetDetails()` on TimedWidgetTile / Clock / etc.).
#[derive(Clone, Debug, PartialEq)]
pub struct WidgetDetails {
    pub widget_id: String,
    pub title: String,
    pub lines: Vec<DetailLine>,
}

impl WidgetDetails {
    pub fn new(widget_id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            widget_id: widget_id.into(),
            title: title.into(),
            lines: Vec::new(),
        }
    }

    pub fn line(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.lines.push(DetailLine::new(label, value));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Global input pre-process outcome. `Consumed` means the dock must not forward
/// the key to the focused widget or the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDisposition {
    PassThrough,
    Consumed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandInput {
    Escape,
    Left,
    Right,
    Other,
}

/// Island page specification: collapsed chip width and optional expanded width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageWidth {
    pub collapsed: f32,
    pub expanded: f32,
}

const STANDARD_PAGE: PageWidth = PageWidth {
    collapsed: 168.0,
    expanded: island_geo::EXPANDED_PREFERRED_MIN_DIPS,
};
const TIMED_PAGE: PageWidth = PageWidth {
    collapsed: 196.0,
    expanded: island_geo::EXPANDED_PREFERRED_MIN_DIPS,
};
const MEDIA_PAGE: PageWidth = PageWidth {
    collapsed: 314.0,
    expanded: 560.0,
};

/// `WidthForCurrentPage`: reference grows the island *height* to 246 DIP and picks an
/// expanded width of at least 540 DIP (560 for media), never "collapsed + 246".
pub fn width_for_page(widget_id: &str, expanded: bool) -> f32 {
    let page = page_width(widget_id);
    if expanded {
        page.expanded
    } else {
        page.collapsed
    }
}

pub fn page_width(widget_id: &str) -> PageWidth {
    match widget_id.to_ascii_lowercase().as_str() {
        "media" => MEDIA_PAGE,
        "focus" | "stopwatch" | "countdown" => TIMED_PAGE,
        _ => STANDARD_PAGE,
    }
}

// Policy functions live only in `opentaskbar_core::dynamic_island` (`policy::*`).
// The former local copies silently diverged from production (different arity,
// different `move_widget` offset type) and were tested instead of the real code.

/// Mutable island state. `SetExpanded` starts a 180ms eased width transition;
/// `pre_process_input` consumes navigation keys while expanded.
pub struct IslandState {
    pub visible: bool,
    pub page_index: usize,
    pub expanded: bool,
    pub page_ids: Vec<String>,
    pub details: Option<WidgetDetails>,
    animation_started: Option<Instant>,
    details_requested: bool,
}

impl IslandState {
    pub fn new(page_ids: Vec<String>) -> Self {
        Self {
            visible: true,
            page_index: 0,
            expanded: false,
            page_ids,
            details: None,
            animation_started: None,
            details_requested: false,
        }
    }

    pub fn current_widget(&self) -> Option<&str> {
        self.page_ids.get(self.page_index).map(String::as_str)
    }

    /// `SetExpanded()`.
    pub fn set_expanded(&mut self, expanded: bool) -> bool {
        if self.expanded == expanded {
            return false;
        }
        self.expanded = expanded;
        self.animation_started = Some(Instant::now());
        if !expanded {
            self.details = None;
            self.details_requested = false;
        }
        true
    }

    pub fn animation_progress(&self, now: Instant) -> f32 {
        let Some(started) = self.animation_started else {
            return 1.0;
        };
        (now.saturating_duration_since(started).as_millis() as f32 / ISLAND_ANIM_MS as f32)
            .clamp(0.0, 1.0)
    }

    pub fn animation_done(&self, now: Instant) -> bool {
        self.animation_started.is_some_and(|started| {
            now.saturating_duration_since(started) >= Duration::from_millis(ISLAND_ANIM_MS)
        })
    }

    pub fn current_width(&self, now: Instant) -> f32 {
        let id = self.current_widget().unwrap_or("");
        let collapsed = width_for_page(id, false);
        if !self.expanded {
            return collapsed;
        }
        let expanded = width_for_page(id, true);
        let t = cubic_ease_out(self.animation_progress(now));
        collapsed + (expanded - collapsed) * t
    }

    pub fn go_page(&mut self, direction: isize) -> bool {
        if self.page_ids.is_empty() {
            return false;
        }
        let len = self.page_ids.len() as isize;
        let next = (self.page_index as isize + direction).rem_euclid(len) as usize;
        if next == self.page_index {
            return false;
        }
        self.page_index = next;
        self.details = None;
        self.details_requested = false;
        true
    }

    /// Global input pre-process: Escape collapses, arrows switch pages while expanded.
    pub fn pre_process_input(&mut self, input: IslandInput) -> InputDisposition {
        if !self.visible {
            return InputDisposition::PassThrough;
        }
        match input {
            IslandInput::Escape if self.expanded => {
                self.set_expanded(false);
                InputDisposition::Consumed
            }
            IslandInput::Left if self.expanded && self.page_ids.len() > 1 => {
                self.go_page(-1);
                InputDisposition::Consumed
            }
            IslandInput::Right if self.expanded && self.page_ids.len() > 1 => {
                self.go_page(1);
                InputDisposition::Consumed
            }
            _ => InputDisposition::PassThrough,
        }
    }

    /// `TimedWidgetTile.DetailsRequested` hook: expand and ask the provider once per page.
    pub fn request_details(
        &mut self,
        provider: impl FnOnce(&str) -> Option<WidgetDetails>,
    ) -> bool {
        let Some(id) = self.current_widget().map(str::to_owned) else {
            return false;
        };
        self.set_expanded(true);
        if self.details_requested {
            return false;
        }
        self.details_requested = true;
        self.details = provider(&id).filter(|d| !d.is_empty());
        true
    }

    pub fn reset_transient(&mut self) {
        self.expanded = false;
        self.details = None;
        self.details_requested = false;
        self.animation_started = None;
        self.page_index = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Tests exercise the same `policy::*` functions production calls, not copies.
    use opentaskbar_core::settings::DynamicIslandVisibilityMode;

    fn pages() -> Vec<String> {
        ["clock", "focus", "media"].map(str::to_owned).to_vec()
    }

    #[test]
    fn default_order_is_derived_from_the_registry_catalog() {
        let expected: Vec<&str> = registry::BUILTINS
            .iter()
            .filter(|widget| widget.catalog)
            .map(|widget| widget.id)
            .collect();
        assert_eq!(DEFAULT_WIDGET_ORDER, expected.as_slice());
        assert_eq!(DEFAULT_WIDGET_ORDER.first(), Some(&"clock"));
        assert!(DEFAULT_WIDGET_ORDER.contains(&"focus"));
    }

    #[test]
    fn reference_246_expand_is_vertical_and_width_is_content_driven() {
        assert_eq!(width_for_page("clock", false), 168.0);
        assert_eq!(width_for_page("clock", true), 540.0);
        assert_eq!(width_for_page("media", true), 560.0);
        assert_eq!(
            policy::island_height_dips(false),
            island_geo::COMPACT_HEIGHT_DIPS
        );
        assert_eq!(
            policy::island_height_dips(true),
            island_geo::EXPANDED_HEIGHT_DIPS
        );
        assert_eq!(island_geo::BOTTOM_RADIUS_DIPS, 20.0);
        assert_eq!(island_geo::BUTTON_RADIUS_DIPS, 5.0);
    }

    #[test]
    fn clamp_window_width_respects_monitor_and_bounds() {
        assert_eq!(
            policy::clamp_window_width(500.0, 1920.0, 240.0, 960.0, 16.0),
            500.0
        );
        assert_eq!(
            policy::clamp_window_width(2000.0, 1024.0, 240.0, 960.0, 16.0),
            960.0
        );
        assert_eq!(
            policy::clamp_window_width(f32::NAN, 1920.0, 240.0, 960.0, 16.0),
            240.0
        );
    }

    #[test]
    fn visibility_modes() {
        use DynamicIslandVisibilityMode as V;
        assert!(policy::should_show(V::Always, false, true, false));
        assert!(!policy::should_show(V::Always, true, true, true));
        assert!(policy::should_show(V::DesktopOnly, true, false, true));
        assert!(!policy::should_show(V::DesktopOnly, false, false, false));
    }

    #[test]
    fn order_widgets_includes_timed_order_and_deduplicates() {
        let available = ["clock", "focus", "media"].map(str::to_owned).to_vec();
        // Timed widgets are already part of `DEFAULT_WIDGET_ORDER`, which the
        // production call passes as `default_order`; no duplicate policy needed.
        let order = policy::order_widgets(
            &available,
            Some(&["media".into()]),
            Some(&["clock".into(), "hidden".into()]),
            &["focus", "clock"],
        );
        assert_eq!(order, ["media", "clock", "focus"]);
    }

    #[test]
    fn move_widget_only_swaps_one_step() {
        let order = ["a", "b", "c"].map(str::to_owned).to_vec();
        assert_eq!(policy::move_widget(&order, "b", -1), ["b", "a", "c"]);
        assert_eq!(policy::move_widget(&order, "a", -1), order);
        assert_eq!(policy::move_widget(&order, "a", 2), order);
    }

    #[test]
    fn set_expanded_starts_180ms_animation() {
        let mut island = IslandState::new(pages());
        let now = Instant::now();
        assert!(!island.expanded);
        assert!(island.set_expanded(true));
        assert!(!island.set_expanded(true));
        assert!(island.current_width(now) < 540.0);
        assert!(island.animation_progress(now + Duration::from_millis(90)) <= 0.51);
        assert!(island.animation_done(now + Duration::from_millis(181)));
        assert!(island.current_width(now + Duration::from_millis(200)) >= 540.0 - 0.01);
    }

    #[test]
    fn pre_process_consumes_escape_and_arrows_only_when_expanded() {
        let mut island = IslandState::new(pages());
        assert_eq!(
            island.pre_process_input(IslandInput::Escape),
            InputDisposition::PassThrough
        );
        assert_eq!(
            island.pre_process_input(IslandInput::Right),
            InputDisposition::PassThrough
        );
        island.set_expanded(true);
        assert_eq!(
            island.pre_process_input(IslandInput::Right),
            InputDisposition::Consumed
        );
        assert_eq!(island.current_widget(), Some("focus"));
        assert_eq!(
            island.pre_process_input(IslandInput::Left),
            InputDisposition::Consumed
        );
        assert_eq!(island.current_widget(), Some("clock"));
        assert_eq!(
            island.pre_process_input(IslandInput::Escape),
            InputDisposition::Consumed
        );
        assert!(!island.expanded);
    }

    #[test]
    fn details_requested_fires_once_and_populates_panel() {
        let mut island = IslandState::new(pages());
        assert!(island.request_details(|id| {
            Some(
                WidgetDetails::new(id, "Date and time")
                    .line("Timezone", "Europe/Istanbul")
                    .line("Format", "24h"),
            )
        }));
        assert!(island.expanded);
        assert_eq!(island.details.as_ref().unwrap().lines.len(), 2);
        // A second request on the same page must not fire the provider again.
        assert!(!island.request_details(|_| panic!("provider must not be called twice")));
    }

    #[test]
    fn in_place_expand_grows_by_details_panel_and_escape_collapses() {
        let mut island = IslandState::new(pages());
        let collapsed = width_for_page("clock", false);
        island.request_details(|id| {
            Some(WidgetDetails::new(id, "Date and time").line("Timezone", "Europe/Istanbul"))
        });
        let now = Instant::now();
        assert!(island.current_width(now + Duration::from_millis(200)) >= collapsed + 372.0 - 0.01);
        assert_eq!(
            island.pre_process_input(IslandInput::Escape),
            InputDisposition::Consumed
        );
        assert!(!island.expanded);
        assert!(island.details.is_none());
        assert_eq!(island.current_width(now), collapsed);
    }

    #[test]
    fn session_switch_resets_transient_island_ui() {
        let mut island = IslandState::new(pages());
        island.set_expanded(true);
        island.go_page(1);
        island.reset_transient();
        assert!(!island.expanded);
        assert_eq!(island.page_index, 0);
        assert!(island.details.is_none());
    }

    /// No island click may dead-end: Prev/Next page, Expand toggles the reference
    /// in-place details panel (the chip's own click opens the real flyout).
    #[test]
    fn island_click_dispatch_table() {
        assert_eq!(
            island_button_click_action(IslandButton::Prev),
            IslandClickAction::PrevPage
        );
        assert_eq!(
            island_button_click_action(IslandButton::Next),
            IslandClickAction::NextPage
        );
        assert_eq!(
            island_button_click_action(IslandButton::Expand),
            IslandClickAction::ToggleDetails,
        );
        assert!(has_real_widget_flyout("Clock")); // case-insensitive
        assert!(!has_real_widget_flyout("unknown"));
    }

    #[test]
    fn island_flyout_anchor_is_below_the_island_and_centered() {
        // Top-edge monitor at x=1920..3840, DPI=1.5, island bottom at y=75.
        let (x, y) = island_flyout_anchor(1920, 1920, 75, 1.5);
        assert_eq!(x, 2880);
        // `FlyoutEdge::Top` opens downward from the anchor: a small gap below the
        // island, no longer the 246 DIP expanded-height workaround.
        assert_eq!(y, 75 + 12); // 8 DIP * 1.5
    }

    #[test]
    fn island_flyouts_and_menus_open_below_the_island() {
        let src = include_str!("dynamic_island.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(code.contains("open_widget_flyout_on_edge("));
        assert!(code.contains("crate::ui::flyout::FlyoutEdge::Top"));
        assert!(code.contains("crate::ui::menu::MenuEdge::Top"));
        assert!(code.contains("self.island_menu_anchor()"));
        assert!(!code.contains(&["adjust_audio", "_volume("].concat()));
        assert!(code.contains("adjust_volume_wheel(i32::from(delta))"));
    }

    #[test]
    fn every_widget_has_an_expanded_detail_card() {
        for widget in registry::BUILTINS.iter().filter(|w| w.catalog) {
            assert!(
                has_expandable_content(Some(widget.id)),
                "{} must expand in the island",
                widget.id
            );
        }
        for id in [
            "battery",
            "storage",
            "keyboard",
            "quicksettings",
            "custom:demo",
        ] {
            assert!(has_expandable_content(Some(id)), "{id}");
        }
        assert!(!has_expandable_content(None));
        assert!(!has_expandable_content(Some("  ")));
    }

    #[test]
    fn island_flyout_dispatch_matches_the_registry_for_all_builtins() {
        let catalog: Vec<_> = registry::BUILTINS.iter().filter(|w| w.catalog).collect();
        assert_eq!(
            catalog.len(),
            opentaskbar_core::settings::CATALOG_WIDGET_COUNT,
            "every built-in catalog widget"
        );
        for widget in catalog {
            assert_eq!(
                has_real_widget_flyout(widget.id),
                widget.has_flyout,
                "{} island flyout gate must follow the registry",
                widget.id
            );
        }
        for id in [
            "stats",
            "quicksettings",
            "clock",
            "weather",
            "audio",
            // Legacy PascalCase spellings resolve to the same widget.
            "SystemStats",
            "QuickSettings",
            "custom:demo",
            "Custom:Other",
        ] {
            assert!(has_real_widget_flyout(id), "{id} must open its flyout");
        }
        for id in [
            "keyboard",
            "KeyboardLayout",
            "media",
            "aiusage_claude",
            "AiUsageClaude",
            "aiusage_all",
        ] {
            assert!(
                !has_real_widget_flyout(id),
                "{id} must not open a flyout after inline-only update"
            );
        }
        assert!(!has_real_widget_flyout(""));
        assert!(!has_real_widget_flyout("custom:"));
    }

    #[test]
    fn island_widget_matching_canonicalizes_legacy_ids() {
        assert!(island_widget_matches("stats", "SystemStats"));
        assert!(island_widget_matches("keyboard", "KeyboardLayout"));
        assert!(island_widget_matches("aiusage_claude", "AiUsageClaude"));
        assert!(island_widget_matches("Clock", "clock"));
        assert!(!island_widget_matches("clock", "focus"));
        assert!(is_focus_mode_widget("Clock"));
        assert!(is_focus_mode_widget("focus"));
        assert!(!is_focus_mode_widget("weather"));
    }

    #[test]
    fn focus_mode_keeps_only_opted_in_focus_widgets_on_the_island() {
        let mut settings = DockSettings {
            dynamic_island_enable: true,
            focus_mode: true,
            ..DockSettings::default()
        };
        settings.set_dynamic_island_show_focus_widgets(false);
        assert!(!is_widget_placed_on_connected_display(
            &settings,
            "clock",
            &[]
        ));
        assert!(!is_widget_placed_on_connected_display(
            &settings,
            "weather",
            &[]
        ));
    }

    #[test]
    fn strip_layout_packs_widgets_side_by_side_within_budget() {
        let widths = [120.0, 168.0, 314.0, 96.0, 184.0];
        let budget = 500.0;
        let pages = pack_strip_pages(&widths, budget, STRIP_GAP_DIPS);
        // Every widget lands on exactly one page, in order.
        let flat: Vec<usize> = pages.iter().flatten().copied().collect();
        assert_eq!(flat, (0..widths.len()).collect::<Vec<_>>());
        assert!(pages.iter().all(|page| !page.is_empty()));
        assert!(
            pages.len() < widths.len(),
            "Strip must pack several per page"
        );
        for page in &pages {
            let content = group_content_width(&widths, page, STRIP_GAP_DIPS);
            assert!(content.is_finite());
            assert!(content <= budget + 0.01 || page.len() == 1, "{page:?}");
        }
        assert_eq!(pages[0], vec![0, 1]);
        // A widget wider than the whole budget gets its own page, never a panic.
        let wide = pack_strip_pages(&[900.0, 80.0], 300.0, STRIP_GAP_DIPS);
        assert_eq!(wide, vec![vec![0], vec![1]]);
        // Garbage widths are sanitized instead of producing NaN geometry.
        let odd = pack_strip_pages(&[f32::NAN, f32::INFINITY, -4.0], f32::NAN, f32::NAN);
        assert_eq!(odd.iter().flatten().count(), 3);
        for page in &odd {
            assert!(group_content_width(&[f32::NAN, f32::INFINITY, -4.0], page, 0.0).is_finite());
        }
        assert!(pack_strip_pages(&[], 400.0, STRIP_GAP_DIPS).is_empty());
        assert_eq!(paged_groups(3), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn strip_width_cap_follows_monitor_and_absolute_limit() {
        assert_eq!(strip_max_width_dips(3840.0), STRIP_MAX_WIDTH_DIPS);
        assert!((strip_max_width_dips(1000.0) - 720.0).abs() < 0.01);
        assert!(strip_max_width_dips(f32::NAN).is_finite());
    }

    #[test]
    fn single_widget_header_is_centered_without_a_dead_gutter() {
        let content = 120.0;
        let width = island_width_for_content(content, false, true);
        let layout = island_header_layout(width, COMPACT_HEIGHT_DIPS, 1, 0, true);
        assert!(layout.prev.is_none() && layout.next.is_none());
        assert!(layout.dots.is_empty());
        assert!((layout.content_width() - content).abs() < 0.01);
        let left_pad = layout.content_left;
        let right_pad = width - layout.content_right;
        assert!((left_pad - right_pad).abs() < 0.01, "symmetric padding");
        assert!(right_pad < 50.0, "no 132 DIP right gutter");
        assert_eq!(layout.chip_h, CHIP_H_DIPS);
    }

    #[test]
    fn paged_header_has_symmetric_arrows_and_clickable_dots() {
        let width = island_width_for_content(200.0, true, true);
        let layout = island_header_layout(width, COMPACT_HEIGHT_DIPS, 4, 2, true);
        let prev = layout.prev.expect("prev arrow");
        let next = layout.next.expect("next arrow");
        assert!((prev.2 - prev.0 - NAV_BUTTON_DIPS).abs() < 0.01);
        assert!((next.2 - next.0 - NAV_BUTTON_DIPS).abs() < 0.01);
        assert!(
            (layout.content_left - (width - layout.content_right)).abs() < 0.01,
            "content stays centered"
        );
        assert_eq!(layout.dots.len(), 4);
        let active = layout.dots.iter().find(|(page, _)| *page == 2).unwrap().1;
        assert!((active.2 - active.0 - ACTIVE_DOT_W_DIPS).abs() < 0.01);
        let other = layout.dots.iter().find(|(page, _)| *page == 0).unwrap().1;
        assert!((other.2 - other.0 - DOT_DIPS).abs() < 0.01);
        // Dots sit below the chip, inside the header.
        assert!(other.1 >= layout.chip_top + layout.chip_h);
        assert!(other.3 <= COMPACT_HEIGHT_DIPS);
        // Many pages show a bounded window of dots around the current page.
        let many = island_header_layout(width, COMPACT_HEIGHT_DIPS, 30, 29, true);
        assert_eq!(many.dots.len(), MAX_VISIBLE_DOTS);
        assert!(many.dots.iter().any(|(page, _)| *page == 29));
        assert_eq!(
            island_button_click_action(IslandButton::Dot(3)),
            IslandClickAction::GoToPage(3)
        );
    }

    #[test]
    fn detail_layout_fits_the_expanded_island() {
        let layout = island_detail_layout(
            island_geo::EXPANDED_PREFERRED_MIN_DIPS,
            COMPACT_HEIGHT_DIPS,
            EXPANDED_HEIGHT_DIPS,
            6,
            4,
        );
        assert_eq!(layout.rows.len(), 4);
        assert_eq!(layout.buttons.len(), 4);
        let inside = |r: DipRect| {
            r.0 >= layout.panel.0 - 0.01
                && r.2 <= layout.panel.2 + 0.01
                && r.1 >= layout.panel.1 - 0.01
                && r.3 <= layout.panel.3 + 0.01
        };
        assert!(layout.rows.iter().copied().all(inside));
        assert!(layout.buttons.iter().copied().all(inside));
        assert!(layout.panel.3 <= EXPANDED_HEIGHT_DIPS);
        let last_row = layout.rows.last().unwrap();
        assert!(
            last_row.3 <= layout.buttons[0].1,
            "rows never overlap buttons"
        );
        let narrow =
            island_detail_layout(f32::NAN, COMPACT_HEIGHT_DIPS, EXPANDED_HEIGHT_DIPS, 1, 0);
        assert!(narrow.panel.2.is_finite() && narrow.rows.len() == 1);
    }

    #[test]
    fn details_width_stays_within_reference_measure_budget() {
        let details = WidgetDetails::new("clock", "Date and time")
            .line("Timezone", "Europe/Istanbul")
            .line("Format", "24h");
        let width = details_desired_width(&details);
        assert!(width > 0.0 && width <= 640.0);
        assert_eq!(details_desired_width(&WidgetDetails::new("x", "")), 0.0);
    }

    #[test]
    fn modal_flyout_and_island_wndproc_guards_nest_and_drop_cleanly() {
        assert!(!is_modal_flyout_active());
        {
            let outer = ModalFlyoutGuard::enter();
            assert!(is_modal_flyout_active());
            {
                let inner = ModalFlyoutGuard::enter();
                assert!(is_modal_flyout_active());
                drop(inner);
                assert!(
                    is_modal_flyout_active(),
                    "outer guard must remain active after inner drops"
                );
            }
            drop(outer);
        }
        assert!(
            !is_modal_flyout_active(),
            "depth must return to zero when all guards drop"
        );

        let first = IslandWndProcGuard::try_enter().expect("initial wndproc entry");
        assert!(
            IslandWndProcGuard::try_enter().is_none(),
            "re-entrant island_wnd_proc must be blocked while guard is held"
        );
        drop(first);
        let second = IslandWndProcGuard::try_enter().expect("entry succeeds after drop");
        drop(second);
    }

    #[test]
    fn rebase_slide_offset_preserves_visual_position_when_viewport_width_changes() {
        // Resting on page 2 with viewport width 160 px -> slide_offset_px = -320 px.
        // Switching to page 3 widens viewport to 310 px: re-based from_px must place
        // page 2 cleanly at drag_delta == 0.0 (not jumped by 2 * 150 px).
        let prev_page = 2usize;
        let prev_vp_px = 160.0_f32;
        let new_vp_px = 310.0_f32;
        let resting_offset = -(prev_page as f32) * prev_vp_px;
        let from_resting = rebase_slide_offset(resting_offset, prev_page, prev_vp_px, new_vp_px);
        assert!(
            (from_resting + (prev_page as f32) * new_vp_px).abs() < 1e-4,
            "resting page 2 must start at 0 px shift in the new viewport, got {from_resting}"
        );

        // Dragged by -45 px from page 2 before releasing to page 3 with a narrower
        // viewport (120 px): page 2 must still sit at -45 px in the new viewport.
        let drag_delta = -45.0_f32;
        let dragged_offset = -(prev_page as f32) * prev_vp_px + drag_delta;
        let narrow_vp_px = 120.0_f32;
        let from_dragged = rebase_slide_offset(dragged_offset, prev_page, prev_vp_px, narrow_vp_px);
        let prev_page_shift = (prev_page as f32) * narrow_vp_px + from_dragged;
        assert!(
            (prev_page_shift - drag_delta).abs() < 1e-4,
            "dragged page 2 must preserve drag_delta={drag_delta}, got {prev_page_shift}"
        );

        // Equal viewport width is an exact identity.
        let same_vp = rebase_slide_offset(dragged_offset, prev_page, prev_vp_px, prev_vp_px);
        assert!((same_vp - dragged_offset).abs() < 1e-4);
    }

    #[test]
    fn pin_slot_mirrors_the_expand_button_without_moving_content() {
        for pages in [1, 4] {
            let width = island_width_for_content(200.0, pages > 1, true);
            let layout = island_header_layout(width, COMPACT_HEIGHT_DIPS, pages, 0, true);
            let pin = layout.pin.expect("pin slot");
            let expand = layout.expand.expect("expand");
            assert!((pin.2 - pin.0 - (expand.2 - expand.0)).abs() < 0.01);
            if pages == 1 {
                assert!((pin.0 - (width - expand.2)).abs() < 0.5, "mirrored inset");
            }
            assert!(pin.2 <= layout.content_left, "pin never overlaps the chips");
            if let Some(prev) = layout.prev {
                assert!(pin.0 >= prev.2 - 0.01, "pin sits after the prev arrow");
            }
        }
        let no_expand = island_header_layout(300.0, COMPACT_HEIGHT_DIPS, 1, 0, false);
        assert!(no_expand.pin.is_none());
        assert_eq!(
            island_button_click_action(IslandButton::Pin),
            IslandClickAction::TogglePin
        );
    }

    #[test]
    fn overlay_priority_is_agent_then_hud_then_notification() {
        use IslandOverlayKind::{AgentApproval, Hud, Notification};
        assert!(overlay_preempts(None, Notification));
        assert!(overlay_preempts(Some(Notification), Hud));
        assert!(
            overlay_preempts(Some(Hud), Hud),
            "a newer HUD replaces the old one"
        );
        assert!(overlay_preempts(Some(Hud), AgentApproval));
        assert!(!overlay_preempts(Some(AgentApproval), Hud));
        assert!(!overlay_preempts(Some(Hud), Notification));
    }

    #[test]
    fn overlay_layouts_keep_every_element_inside_the_capsule() {
        let inside = |rect: DipRect, w: f32, h: f32| {
            rect.0 >= 0.0 && rect.1 >= 0.0 && rect.2 <= w + 0.01 && rect.3 <= h + 0.01
        };
        for kind in [
            IslandOverlayKind::AgentApproval,
            IslandOverlayKind::Hud,
            IslandOverlayKind::Notification,
        ] {
            for max_width in [f32::NAN, 1920.0, 300.0] {
                for has_progress in [false, true] {
                    let layout = island_overlay_layout(kind, has_progress, max_width);
                    let (w, h) = (layout.width, layout.height);
                    assert!(w.is_finite() && w > 0.0 && h > 0.0);
                    for rect in [layout.icon, layout.title, layout.subtitle, layout.body] {
                        assert!(inside(rect, w, h), "{kind:?} {rect:?} in {w}x{h}");
                    }
                    for (_, rect) in &layout.buttons {
                        assert!(inside(*rect, w, h), "{kind:?} button {rect:?}");
                    }
                }
            }
        }
        let agent = island_overlay_layout(IslandOverlayKind::AgentApproval, false, 1920.0);
        let buttons: Vec<_> = agent.buttons.iter().map(|(b, _)| *b).collect();
        assert_eq!(
            buttons,
            [
                IslandOverlayButton::Allow,
                IslandOverlayButton::Deny,
                IslandOverlayButton::Always,
                IslandOverlayButton::Jump,
            ]
        );
        let notification = island_overlay_layout(IslandOverlayKind::Notification, false, 1920.0);
        assert_eq!(notification.buttons.len(), 1);
        assert_eq!(notification.buttons[0].0, IslandOverlayButton::Dismiss);
        let hud = island_overlay_layout(IslandOverlayKind::Hud, true, 1920.0);
        assert!(hud.progress.is_some() && hud.buttons.is_empty());
    }

    #[test]
    fn diff_stats_skip_file_headers() {
        let preview = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-old\n+new\n+more\n context";
        assert_eq!(diff_stats(preview), (2, 1));
        assert_eq!(diff_stats(""), (0, 0));
    }

    #[test]
    fn island_keyboard_shortcut_helpers() {
        assert_eq!(ctrl_digit_page(u16::from(b'1')), Some(0));
        assert_eq!(ctrl_digit_page(u16::from(b'9')), Some(8));
        assert_eq!(ctrl_digit_page(u16::from(b'0')), None);
        assert_eq!(cycle_page_index(0, 3, -1), 2);
        assert_eq!(cycle_page_index(2, 3, 1), 0);
        assert_eq!(cycle_page_index(1, 3, 1), 2);
        assert_eq!(cycle_page_index(0, 0, 1), 0);
    }

    #[test]
    fn media_album_art_zone_and_minimal_width() {
        assert!(media_album_art_hit("media", 12.0, 38.0));
        assert!(media_album_art_hit("Media", 30.0, 38.0));
        assert!(!media_album_art_hit("media", 80.0, 38.0));
        assert!(!media_album_art_hit("clock", 12.0, 38.0));
        assert!((minimal_width_for(None, 0.0) - MINIMAL_MIN_WIDTH_DIPS).abs() < 0.01);
        let long = "x".repeat(500);
        assert!((minimal_width_for(Some(&long), 22.0) - MINIMAL_MAX_WIDTH_DIPS).abs() < 0.01);
        let mid = minimal_width_for(Some("12:45"), 0.0);
        assert!((MINIMAL_MIN_WIDTH_DIPS..=MINIMAL_MAX_WIDTH_DIPS).contains(&mid));
        assert_eq!(truncate_line("  short  ", 10), "short");
        assert_eq!(truncate_line("abcdefghijkl", 5).chars().count(), 5);
    }
}
