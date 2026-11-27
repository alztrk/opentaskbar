//! Pure dock geometry, ordering and interaction policy (`DockMetrics`,
//! `DockFisheyeLayout`, `DockItemOrdering`, `MainWindow.Orientation` parity).
//!
//! Everything in this module is side-effect free so it can be unit tested without a
//! desktop session. The Win32/DirectComposition code in `dock/mod.rs` consumes these
//! helpers; no Windows API calls happen here.
//!
//! Several helpers are the Rust port's public contract for policies still owned by
//! other workstreams (WS-B shelf reservation, WS-C hover coordinator, WS-E islands),
//! so they are intentionally available before every call site is merged.
//!
//! Cross-WS APIs absent from this worktree (left as named helpers instead of patched
//! into another workstream's files):
//! - `widgets::WidgetHoverCoordinator` (WS-C) — 350 ms hover intent / slider capture policy
//!   lives in `hover_panel_stays_open`; the dock message loop does not call it yet.
//! - `platform::start_menu_state` (WS-B) — Start open/close + flyout-transition detection;
//!   `evaluate_auto_hide` passes `false` for `shell_menu_open` until it lands.
//! - `SHAppBarMessage` secondary shelf reservation / Explorer shelf reuse (WS-B) —
//!   `needs_own_shelf`, `missing_shelf_pixels`, `appbar_position_flags` are the pure policy.
//! - `FileSystemWatcher` folder-stack watcher and `OpenFolderDialog` (WS-E) — only the
//!   4-column preview, `.crdownload` filter and badge helpers are ported here.
//! - OLE `IDropSource`/`IDataObject` CFSTR shell data package — outbound drag from the
//!   dock still uses the existing `WM_DROPFILES` inbound path only.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use opentaskbar_core::settings::{
    AutoHideMode, DockContentAlignment, DockEdge, DockPosition, DockSettings, DockSize,
    SecondaryAppScope, StartButtonPlacement,
};

use crate::platform::win32::MonitorInfo;

// `DockMetrics` constants.
pub const HEADROOM_DIPS: f32 = 24.0;
pub const BOTTOM_GAP_DIPS: f32 = 6.0;
pub const SIDE_INSET_DIPS: f32 = 12.0;
pub const FULL_WIDTH_SIDE_INSET_DIPS: f32 = 6.0;
pub const EDGE_TRIGGER_DIPS: f32 = 5.0;
pub const HIDDEN_OFFSET_MIN_DIPS: f32 = 72.0;
pub const MIN_CELL_SIZE_DIPS: f32 = 24.0;
pub const HOVER_SCALE: f32 = 1.5;
pub const INFLUENCE_RADIUS_DIPS: f32 = 135.0;
pub const PREVIEW_GAP_DIPS: f32 = 18.0;

// Motion / gesture timings in milliseconds (`DockMetrics`, `WidgetHoverCoordinator`,
// `WindowThumbnailPreview`, `MainWindow.DragDrop`).
pub const REVEAL_MS: u64 = 220;
pub const HIDE_MS: u64 = 220;
pub const REVEALED_GUARD_MS: u64 = 200;
pub const WIN_KEY_GRACE_MS: u64 = 600;
/// `LLKHF_INJECTED` in the low-level keyboard hook flags (`KBDLLHOOKSTRUCT`).
pub const LLKHF_INJECTED: u32 = 0x10;
/// `LLKHF_ALTDOWN`: an Alt modifier was held for the low-level key event.
pub const LLKHF_ALTDOWN: u32 = 0x20;
/// Win32 class name of the shell's per-monitor XAML/CoreWindow island host.
pub const CORE_WINDOW_CLASS: &str = "windows.ui.core.corewindow";
pub const HOVER_INTENT_MS: u64 = 350;
pub const DRAG_HOVER_MS: u64 = 450;
pub const PREVIEW_GRACE_MS: u64 = 250;
pub const PREVIEW_DELAY_MS: u64 = 500;
pub const PREVIEW_FADE_MS: u64 = 140;
pub const ISLAND_ANIM_MS: u64 = 180;
pub const DETAILS_PANEL_WIDTH_DIPS: f32 = 246.0;

// `DockMetrics.For` size profiles: Compact / Medium / Large at 100% scale.
// (Shelf, Cell, Icon, Radius) = (48/58/66, 38/44/50, 28/34/38, 16/19/22).
pub const PROFILE_SHELF_DIPS: [f32; 3] = [48.0, 58.0, 66.0];
pub const PROFILE_CELL_DIPS: [f32; 3] = [38.0, 44.0, 50.0];
pub const PROFILE_ICON_DIPS: [f32; 3] = [28.0, 34.0, 38.0];
pub const PROFILE_RADIUS_DIPS: [f32; 3] = [16.0, 19.0, 22.0];
/// Secondary light-strip shelf reservation per profile (`SecondaryDockWindow`).
pub const PROFILE_SECONDARY_SHELF_DIPS: [f32; 3] = [52.0, 60.0, 68.0];
/// Full dock window height (headroom + shelf) per profile.
pub const PROFILE_HEADROOM_DIPS: [f32; 3] = [76.0, 84.0, 92.0];
/// `DockMetrics.HoverScale` ceiling — magnification never exceeds 1.5x.
pub const MAX_FISHEYE_SCALE: f32 = 1.5;
/// `DockMetrics.AutoHideDelayMs`: delayed hide after the pointer leaves the dock.
pub const AUTOHIDE_DELAY_MS: u64 = 400;
/// `DockScrollViewer`: one wheel notch advances two cells.
pub const SCROLL_STEP_CELLS: f32 = 2.0;
/// An overflow smaller than half a DIP is just scroll-settling noise.
pub const OVERFLOW_THRESHOLD_DIPS: f32 = 0.5;
/// Thumbnail card size (`Design/Tokens/Layout.xaml` PreviewWidth/PreviewHeight).
pub const PREVIEW_CARD_DIPS: (f32, f32) = (228.0, 154.0);
/// Enlarged card used for the Large profile / expanded preview strip.
pub const PREVIEW_CARD_LARGE_DIPS: (f32, f32) = (250.0, 180.0);

/// Resolved geometry for one dock size profile. All values are DIPs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeProfile {
    pub shelf: f32,
    pub cell: f32,
    pub icon: f32,
    pub radius: f32,
    pub secondary_shelf: f32,
    pub headroom: f32,
    pub preview_card: (f32, f32),
}

/// Compact=0, Medium=1, Large=2. Unknown values clamp to Compact.
pub fn profile_index(size: DockSize) -> usize {
    match size {
        DockSize::Compact => 0,
        DockSize::Medium => 1,
        DockSize::Large => 2,
    }
}

/// The typed `DockSettings::magnification_expands_dock` field is the single
/// source of truth. A stale `extra["MagnificationExpandsDock"]` is only a
/// migration input for `normalize_legacy_json` and must never override an
/// explicit typed value (a persisted PascalCase key is deserialized straight
/// into the typed field, so no lookup in `extra` is needed here).
pub fn magnification_expands_dock(settings: &DockSettings) -> bool {
    settings.magnification_expands_dock
}

/// Migration-only view of the legacy `extra` key. Returns `None` when the key is
/// absent; loaders can write the result into the typed field and drop the key.
pub fn legacy_magnification_expands_dock(settings: &DockSettings) -> Option<bool> {
    settings
        .extra
        .get("MagnificationExpandsDock")
        .and_then(serde_json::Value::as_bool)
}

/// `RunningAppProjection.AggregateByExecutablePath`: merge all windows of a
/// shared host into one executable-keyed slot instead of grouping by AUMID.
pub fn aggregate_by_executable_path(settings: &DockSettings) -> bool {
    if let Some(value) = settings
        .extra
        .get("AggregateByExecutablePath")
        .and_then(|v| v.as_bool())
    {
        return value;
    }
    serde_json::to_value(settings)
        .ok()
        .and_then(|value| {
            value
                .get("AggregateByExecutablePath")
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(false)
}

/// `DockMetrics.For(size, scale, padding)` — scaled profile with a fixed extra
/// gap that widens cells horizontally but never grows the dock vertically.
pub fn size_profile(settings: &DockSettings) -> SizeProfile {
    let i = profile_index(settings.size);
    let scale = if settings.dock_scale.is_finite() {
        settings.dock_scale.clamp(0.75, 1.5) as f32
    } else {
        1.0
    };
    let pad = settings.dock_icon_padding.clamp(0, 12) as f32;
    let shelf = PROFILE_SHELF_DIPS[i];
    let cell_h = (PROFILE_CELL_DIPS[i] * scale).round().max(24.0);
    let radius = (PROFILE_RADIUS_DIPS[i] * scale).round().clamp(10.0, 30.0);
    let secondary_shelf = (PROFILE_SECONDARY_SHELF_DIPS[i] * scale).round();
    // `WindowHeight = Headroom + Shelf`. When magnification is allowed to expand
    // the dock itself, the extra headroom strip is no longer reserved.
    let headroom = if magnification_expands_dock(settings) {
        0.0
    } else {
        (PROFILE_HEADROOM_DIPS[i] - shelf + shelf * scale)
            .round()
            .max(0.0)
    };
    SizeProfile {
        shelf: (shelf * scale).round(),
        cell: cell_h + pad,
        icon: (PROFILE_ICON_DIPS[i] * scale).round().max(20.0),
        radius,
        secondary_shelf,
        headroom,
        preview_card: if i == 2 {
            PREVIEW_CARD_LARGE_DIPS
        } else {
            PREVIEW_CARD_DIPS
        },
    }
}

/// `content_alignment` decides where the content strip sits inside a wide pill.
pub fn content_alignment_offset(available: f32, used: f32, alignment: DockContentAlignment) -> f32 {
    let slack = (available - used).max(0.0);
    match alignment {
        DockContentAlignment::Left => 0.0,
        DockContentAlignment::Center => (slack * 0.5).round(),
        DockContentAlignment::Right => slack.round(),
    }
}

/// The 450 ms bring-to-front gesture is for *external* shell file drags only.
/// Reordering a dock icon must never activate an unrelated application.
pub fn drag_hover_allowed(external_file_drag: bool) -> bool {
    external_file_drag
}

/// Plain wheel step for an overflowing dock (two cells per notch, C parity).
pub fn wheel_scroll_step(cell: f32, gap: f32) -> f32 {
    (cell + gap) * SCROLL_STEP_CELLS
}

pub fn has_scroll_overflow(max_scroll: f32) -> bool {
    max_scroll > OVERFLOW_THRESHOLD_DIPS
}

// `DockFisheyeLayout` constants.
pub const SLOT_EXPANSION_FACTOR: f32 = 0.95;
pub const MAX_LIFT_DIPS: f32 = 1.0;
pub const MIN_NO_OVERLAP_GAP_DIPS: f32 = 4.0;
pub const SPRING_STIFFNESS: f32 = 280.0;
pub const SPRING_DAMPING: f32 = 30.0;
const FULL_ARCH_WEIGHT_SUM: f32 = 2.8;

/// Feature flag living in `DockSettings.extra`. Vertical docking is the default;
/// a JSON `"enable_vertical_dock": false` forces every edge back to `Bottom`.
pub fn vertical_dock_enabled(settings: &DockSettings) -> bool {
    match settings.extra.get("enable_vertical_dock") {
        Some(serde_json::Value::Bool(v)) => *v,
        _ => true,
    }
}

/// Risk-register flag: the real `Shell_TrayWnd` / `Shell_NotifyIcon` host is opt-in.
/// WS-B owns the implementation; this helper only reads the flag.
pub fn live_tray_host_enabled(settings: &DockSettings) -> bool {
    matches!(
        settings.extra.get("enable_live_tray_host"),
        Some(serde_json::Value::Bool(true))
    )
}

/// `DockSettings.NotificationBadgesEnabled` (default on). The value is a
/// typed settings field; an unknown `extra` key must never override it.
pub fn notification_badges_enabled(settings: &DockSettings) -> bool {
    settings.notification_badges_enabled
}

/// Secondary monitors get a light strip rather than a full clone by default.
pub fn secondary_light_strip_enabled(settings: &DockSettings) -> bool {
    match settings.extra.get("secondary_light_strip") {
        Some(serde_json::Value::Bool(v)) => *v,
        None => settings.show_on_all_monitors,
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecondaryLayout {
    /// Full dock on the primary monitor.
    PrimaryDock,
    /// Shelf reservation + clock/QS buttons, no widget strip or app list.
    LightStrip,
    /// No window at all (single-monitor mode).
    Hidden,
}

pub fn secondary_layout(settings: &DockSettings, is_primary: bool) -> SecondaryLayout {
    if is_primary {
        return SecondaryLayout::PrimaryDock;
    }
    if !settings.show_on_all_monitors {
        return SecondaryLayout::Hidden;
    }
    if secondary_light_strip_enabled(settings) {
        SecondaryLayout::LightStrip
    } else {
        SecondaryLayout::PrimaryDock
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockAxis {
    Horizontal,
    Vertical,
}

pub fn axis_for_edge(edge: DockEdge) -> DockAxis {
    match edge {
        DockEdge::Bottom => DockAxis::Horizontal,
        DockEdge::Left | DockEdge::Right => DockAxis::Vertical,
    }
}

pub fn is_vertical(edge: DockEdge) -> bool {
    axis_for_edge(edge) == DockAxis::Vertical
}

/// Scale origin: bottom-center on the bottom dock, icon-center on side docks.
pub fn icon_scale_origin(edge: DockEdge) -> (f32, f32) {
    if is_vertical(edge) {
        (0.5, 0.5)
    } else {
        (0.5, 1.0)
    }
}

/// Launch bounce always runs away from the screen edge.
pub fn bounce_direction(edge: DockEdge) -> f32 {
    if edge == DockEdge::Left { -1.0 } else { 1.0 }
}

/// Signed off-screen offset used by auto-hide (negative hides to the left).
pub fn hide_offset(edge: DockEdge, cross_size_dips: f32) -> f32 {
    let magnitude = HIDDEN_OFFSET_MIN_DIPS.max(cross_size_dips + 16.0);
    if edge == DockEdge::Left {
        -magnitude
    } else {
        magnitude
    }
}

/// Picks the effective edge: `enable_vertical_dock = false` degrades to `Bottom`.
pub fn effective_edge(settings: &DockSettings) -> DockEdge {
    if vertical_dock_enabled(settings) {
        settings.edge
    } else {
        DockEdge::Bottom
    }
}

/// Computes exact signed physical window coordinates for a monitor.
/// `along_dips` is the strip length (width on Bottom, height on Left/Right);
/// `cross_dips` is its thickness (height on Bottom, width on Left/Right).
pub fn window_rect_for_edge(
    edge: DockEdge,
    monitor: MonitorInfo,
    position: DockPosition,
    along_dips: f32,
    cross_dips: f32,
    margin_px: i32,
    full_width: bool,
) -> (i32, i32, i32, i32) {
    let dpi = monitor.dpi_scale.max(0.01);
    let along_px = ((along_dips * dpi).ceil() as i32).min(if is_vertical(edge) {
        monitor.height()
    } else {
        monitor.width()
    });
    let cross_px = ((cross_dips * dpi).ceil() as i32).min(if is_vertical(edge) {
        monitor.width()
    } else {
        monitor.height()
    });

    if is_vertical(edge) {
        let w = cross_px;
        let h = if full_width {
            (monitor.height() - margin_px * 2).max(1)
        } else {
            along_px.max(1)
        };
        let x = if edge == DockEdge::Left {
            monitor.left + margin_px
        } else {
            monitor.right - margin_px - w
        };
        let y = monitor.top + (monitor.height() - h) / 2;
        (x, y, w, h)
    } else {
        let w = if full_width {
            (monitor.width() - margin_px * 2).max(1)
        } else {
            along_px.max(1)
        };
        let h = cross_px;
        let x = match position {
            DockPosition::Left => monitor.left + margin_px,
            DockPosition::Center => monitor.left + (monitor.width() - w) / 2,
            DockPosition::Right => monitor.right - w - margin_px,
        };
        (x, monitor.bottom - h, w, h)
    }
}

/// Returns `(start, size)` of the dock's main axis inside a physical rect.
pub fn axis_extent(edge: DockEdge, rect: (i32, i32, i32, i32)) -> (i32, i32) {
    let (x, y, w, h) = rect;
    if is_vertical(edge) { (y, h) } else { (x, w) }
}

/// Returns `(start, size)` of the dock's cross axis inside a physical rect.
pub fn cross_extent(edge: DockEdge, rect: (i32, i32, i32, i32)) -> (i32, i32) {
    let (x, y, w, h) = rect;
    if is_vertical(edge) { (x, w) } else { (y, h) }
}

/// Wheel delta resolution along the dock's main axis. `DockScrollViewer` always
/// applies `offset -= delta / 120 * cell * 2`, independent of the dock edge: a wheel-up
/// notch moves content toward the start on both bottom and side docks. Only the
/// horizontal wheel on a bottom dock keeps the +delta convention (Shift+wheel there
/// stands in for a horizontal swipe).
pub fn wheel_to_axis_delta(delta: i16, horizontal: bool, edge: DockEdge, step: f32) -> f32 {
    let dist = f32::from(delta) / 120.0 * step;
    match edge {
        DockEdge::Bottom => {
            if horizontal {
                dist
            } else {
                -dist
            }
        }
        DockEdge::Left | DockEdge::Right => -dist,
    }
}

/// Show-desktop corner strip. Bottom: 10x~66 at the right edge. Side docks: a thin
/// horizontal strip at the bottom of the vertical dock.
pub fn show_desktop_strip_rect(
    edge: DockEdge,
    monitor: MonitorInfo,
    thick_dips: f32,
    length_dips: f32,
    dpi: f32,
) -> (i32, i32, i32, i32) {
    let t = (thick_dips.max(1.0) * dpi).round() as i32;
    let l = (length_dips.max(1.0) * dpi).round() as i32;
    match edge {
        DockEdge::Bottom => (monitor.right - t, monitor.bottom - l, t, l),
        DockEdge::Left => (monitor.left, monitor.bottom - t, l, t),
        DockEdge::Right => (monitor.right - l, monitor.bottom - t, l, t),
    }
}

/// True when a cursor (physical pixels) lies in the auto-hide edge-trigger strip.
pub fn is_at_edge_trigger(edge: DockEdge, monitor: MonitorInfo, x: i32, y: i32, dpi: f32) -> bool {
    let in_monitor =
        x >= monitor.left && x <= monitor.right && y >= monitor.top && y <= monitor.bottom;
    if !in_monitor {
        return false;
    }
    let band = ((EDGE_TRIGGER_DIPS * dpi).ceil() as i32).max(4) + 2;
    match edge {
        DockEdge::Left => x <= monitor.left + band,
        DockEdge::Right => x >= monitor.right - band,
        DockEdge::Bottom => y >= monitor.bottom - band,
    }
}

/// Height of a side dock derived from content count (`ResizeSideDockToContent`).
pub fn side_dock_height_dips(
    item_count: usize,
    cell_dips: f32,
    chrome_dips: f32,
    show_desktop: bool,
    available_dips: f32,
) -> f32 {
    let count = item_count.max(1) as f32;
    let desktop_inset = if show_desktop { 7.0 } else { 0.0 };
    let natural = cell_dips * count + chrome_dips + 24.0 + desktop_inset;
    natural.clamp(
        MIN_CELL_SIZE_DIPS + chrome_dips,
        available_dips.max(MIN_CELL_SIZE_DIPS),
    )
}

/// Window classes that must never be treated as overlapping application windows.
pub fn is_shell_overlay_class(class_name: &str) -> bool {
    matches!(
        class_name.to_ascii_lowercase().as_str(),
        "progman"
            | "workerw"
            | "shell_traywnd"
            | "shell_secondarytraywnd"
            | "xamlexplorerhostislandwindow"
            | "multitaskingviewframe"
            | "taskswitcherwnd"
            // Windows 10/11 Start flyout and its light-dismiss surface. Without these,
            // smart auto-hide sees Start as an overlapping app window.
            | CORE_WINDOW_CLASS
            | "startmenusizingframe"
            | "shell_lightdismissoverlay"
    )
}

/// `is_shell_overlay_class` with host validation for `Windows.UI.Core.CoreWindow`.
/// Any UWP app can own a CoreWindow, so the class alone must not mark it as a
/// shell overlay: only the Start / ShellExperienceHost processes qualify.
/// WS-B's `win32_ex::is_shell_overlay` is the single Win32 call site; this pure
/// helper documents the policy and keeps it unit testable.
pub fn is_shell_overlay_class_for_host(class: &str, trusted_start_host: bool) -> bool {
    if class.trim().eq_ignore_ascii_case(CORE_WINDOW_CLASS) {
        return trusted_start_host;
    }
    is_shell_overlay_class(class)
}

/// `WM_APP_WINKEY_REVEAL` posts from the low-level hook must ignore injected
/// key events (synthetic Win taps from RemoteDesktop, automation, IME, ...).
pub fn is_injected_key_event(flags: u32) -> bool {
    flags & LLKHF_INJECTED != 0
}

/// A standalone Win tap is not standalone when Alt (hook flag), Ctrl or Shift
/// (`GetAsyncKeyState` high bit) was held. `ctrl_down`/`shift_down` are the raw
/// signed values from `GetAsyncKeyState`.
pub fn win_key_modifier_pressed(flags: u32, ctrl_down: i16, shift_down: i16) -> bool {
    flags & LLKHF_ALTDOWN != 0 || ctrl_down < 0 || shift_down < 0
}

/// Final gate for the low-level hook. The injected/modifier checks run before
/// the feature gate so `WIN_KEY_ALONE` is cleared on every rejected event.
pub fn win_key_reveal_allowed(gate: bool, injected: bool, modifier: bool) -> bool {
    gate && !injected && !modifier
}

/// Single decision table for the Win-key reveal and its grace window: a Win tap
/// only reveals an auto-hidden dock when the option is on *and* auto-hide is active.
pub fn win_key_reveal_enabled(reveal_dock_on_win_key: bool, auto_hide: AutoHideMode) -> bool {
    reveal_dock_on_win_key && auto_hide != AutoHideMode::Disabled
}

/// Start flyout policy: when Start is open but the shell reposition thread could
/// not anchor it above the dock, hide the dock instead of leaving an overlap.
/// An attempt still in flight (`anchor_pending`) is *not* a failure: the ~800 ms
/// reposition window must not make the dock flicker while the shell animates.
pub fn start_menu_hide_fallback(
    menu_open: bool,
    anchored: bool,
    anchor_pending: bool,
    auto_hide: AutoHideMode,
) -> bool {
    menu_open && !anchored && !anchor_pending && auto_hide != AutoHideMode::Disabled
}

/// Converts a DIP button rect inside the dock window into a physical screen rect.
/// `window` is `(left, top, right, bottom)` in physical pixels; `button` is a
/// DIP rect relative to the window's client origin.
pub fn anchor_from_window_rect(
    window: (i32, i32, i32, i32),
    button: (f32, f32, f32, f32),
    dpi: f32,
) -> (i32, i32, i32, i32) {
    let scale = dpi.max(1.0);
    let (left, top, _, _) = window;
    let (bl, bt, br, bb) = button;
    (
        left + (bl * scale).round() as i32,
        top + (bt * scale).round() as i32,
        left + (br * scale).round() as i32,
        top + (bb * scale).round() as i32,
    )
}

/// Fullscreen detection used to suppress the secondary strip / island on a game.
pub fn is_full_screen_on_monitor(
    win: (i32, i32, i32, i32),
    monitor: MonitorInfo,
    tolerance_px: i32,
) -> bool {
    let (x, y, w, h) = win;
    x <= monitor.left + tolerance_px
        && y <= monitor.top + tolerance_px
        && x + w >= monitor.right - tolerance_px
        && y + h >= monitor.bottom - tolerance_px
}

/// Whether the secondary monitor needs its own AppBar shelf (`SecondaryDockWindow.NeedsOwnShelf`).
pub fn needs_own_shelf(existing_shelf_px: i32, dpi: f32, shelf_dips: f32) -> bool {
    needed_shelf_px(shelf_dips, dpi) > existing_shelf_px
}

pub fn needed_shelf_px(shelf_dips: f32, dpi: f32) -> i32 {
    (shelf_dips * dpi.max(0.01)).round() as i32
}

/// Missing shelf pixels the secondary dock must reserve.
pub fn missing_shelf_pixels(shelf_dips: f32, existing_shelf_px: i32, dpi: f32) -> i32 {
    (needed_shelf_px(shelf_dips, dpi) - existing_shelf_px).max(0)
}

/// `SHAppBarMessage` position flags: `ABE_LEFT=0, ABE_TOP=1, ABE_RIGHT=2, ABE_BOTTOM=3`;
/// bit `0x0040` (`ABM_NEW` caller refresh) is set only for side shelves.
pub fn appbar_position_flags(edge: DockEdge) -> u32 {
    match edge {
        DockEdge::Left => 0x0040,
        DockEdge::Right => 0x0042,
        DockEdge::Bottom => 3,
    }
}

// ---------------------------------------------------------------------------
// Magnification: raised cosine + critically damped spring (reference DockFisheyeLayout).
// ---------------------------------------------------------------------------

pub fn influence_radius_for_cell(cell_size: f32, constrained: bool) -> f32 {
    if !cell_size.is_finite() || cell_size <= 0.0 {
        return INFLUENCE_RADIUS_DIPS;
    }
    if constrained {
        95.0_f32.max(cell_size * 2.1)
    } else {
        110.0_f32.max(cell_size * 2.8)
    }
}

/// C1-smooth raised-cosine falloff: 1 at the pointer, 0 at/outside the radius.
pub fn compute_falloff(distance: f32, radius: f32) -> f32 {
    if !distance.is_finite() || !radius.is_finite() || radius <= 0.0 {
        return 0.0;
    }
    let d = distance.abs();
    if d >= radius {
        return 0.0;
    }
    if d <= 0.0 {
        return 1.0;
    }
    0.5 * (1.0 + (std::f32::consts::PI * d / radius).cos())
}

/// Peak scale that keeps neighbours strictly non-overlapping in a constrained dock.
pub fn compute_no_overlap_max_scale(
    cell_size: f32,
    visual_icon_size: f32,
    configured_max: f32,
) -> f32 {
    if !cell_size.is_finite()
        || cell_size <= 0.0
        || !visual_icon_size.is_finite()
        || visual_icon_size <= 0.0
        || configured_max <= 1.0
    {
        return 1.0;
    }
    let radius = influence_radius_for_cell(cell_size, true);
    let neighbour_falloff = compute_falloff(cell_size, radius);
    let combined_slope = 0.5 * (1.0 + neighbour_falloff);
    let margin = (cell_size - MIN_NO_OVERLAP_GAP_DIPS - visual_icon_size).max(0.0);
    let safe_delta = margin / (visual_icon_size * combined_slope);
    (1.0 + safe_delta).clamp(1.08, configured_max)
}

pub fn free_expansion(maximum: f32, used: f32, scrollable_overflow: f32) -> f32 {
    (maximum - used - (scrollable_overflow - 0.5).max(0.0)).max(0.0)
}

pub fn compute_expansion_fraction(cell_size: f32, configured_max: f32, available: f32) -> f32 {
    if !configured_max.is_finite()
        || configured_max <= 1.0
        || !available.is_finite()
        || available <= 0.0
    {
        return 0.0;
    }
    let need =
        FULL_ARCH_WEIGHT_SUM * (configured_max - 1.0) * cell_size.max(1.0) * SLOT_EXPANSION_FACTOR;
    (available / need.max(1.0)).clamp(0.0, 1.0)
}

pub fn compute_effective_max_scale(
    cell_size: f32,
    visual_icon_size: f32,
    configured_max: f32,
    available_expansion: f32,
) -> f32 {
    if !configured_max.is_finite() || configured_max <= 1.0 {
        return 1.0;
    }
    let no_overlap = compute_no_overlap_max_scale(cell_size, visual_icon_size, configured_max);
    let fraction = compute_expansion_fraction(cell_size, configured_max, available_expansion);
    no_overlap + (configured_max - no_overlap) * fraction
}

pub fn influence_radius_for_expansion(cell_size: f32, fraction: f32) -> f32 {
    if !cell_size.is_finite() || cell_size <= 0.0 {
        return INFLUENCE_RADIUS_DIPS;
    }
    let constrained = 95.0_f32.max(cell_size * 2.1);
    let free = 110.0_f32.max(cell_size * 2.8);
    constrained + (free - constrained) * fraction.clamp(0.0, 1.0)
}

pub fn compute_target_scale(distance: f32, radius: f32, max_scale: f32) -> f32 {
    if !max_scale.is_finite() || max_scale <= 1.0 {
        return 1.0;
    }
    1.0 + (max_scale - 1.0) * compute_falloff(distance, radius)
}

/// Closed-form damped spring step: frame-rate independent, no oscillation.
/// `omega = sqrt(k)`, `eta = c/2`; the damped frequency is `sqrt(omega^2 - eta^2)`.
pub fn step_spring(
    current: f32,
    target: f32,
    velocity: f32,
    dt_seconds: f32,
    max_scale: f32,
) -> (f32, f32, bool) {
    let dt = if dt_seconds.is_finite() {
        dt_seconds.clamp(0.001, 0.05)
    } else {
        0.016
    };
    let omega = SPRING_STIFFNESS.sqrt();
    let eta = SPRING_DAMPING * 0.5;
    let omega_d = (omega * omega - eta * eta).max(1.0).sqrt();
    let decay = (-eta * dt).exp();
    let cos = (omega_d * dt).cos();
    let sin = (omega_d * dt).sin();
    let offset = current - target;
    let temp = (velocity + eta * offset) / omega_d;
    let next = target + decay * (offset * cos + temp * sin);
    let next_velocity = decay * (velocity * cos - (omega_d * offset + eta * temp) * sin);
    let next = next.clamp(1.0, max_scale.max(1.0) + 0.02);
    if (target - next).abs() < 0.004 && next_velocity.abs() < 0.02 {
        (target, 0.0, true)
    } else {
        (next, next_velocity, false)
    }
}

/// Distributes slot expansions without ever exceeding the available budget.
pub fn compute_slot_extras(scales: &[f32], base_sizes: &[f32], available: f32) -> Vec<f32> {
    let count = scales.len().min(base_sizes.len());
    let mut extras = vec![0.0; count];
    let mut total = 0.0;
    for i in 0..count {
        let desired = (scales[i] - 1.0).max(0.0) * base_sizes[i].max(0.0) * SLOT_EXPANSION_FACTOR;
        extras[i] = desired;
        total += desired;
    }
    if total <= 0.001 || !available.is_finite() || available <= 0.0 {
        return vec![0.0; count];
    }
    if total > available {
        let ratio = available / total;
        for extra in &mut extras {
            *extra *= ratio;
        }
    }
    extras
}

pub fn compute_safe_lift(scale: f32, max_scale: f32, icon_size: f32, headroom: f32) -> f32 {
    if scale <= 1.001 || max_scale <= 1.0 {
        return 0.0;
    }
    let progress = ((scale - 1.0) / (max_scale - 1.0)).clamp(0.0, 1.0);
    let desired = MAX_LIFT_DIPS * progress;
    let growth = icon_size.max(0.0) * (scale - 1.0);
    let remaining = (headroom - growth - 1.0).max(0.0);
    desired.min(remaining)
}

pub fn compute_preview_clearance(icon_size: f32, effective_max_scale: f32) -> f32 {
    if !icon_size.is_finite()
        || icon_size <= 0.0
        || !effective_max_scale.is_finite()
        || effective_max_scale <= 1.0
    {
        return 0.0;
    }
    let peak_growth = icon_size * (effective_max_scale - 1.0) + MAX_LIFT_DIPS;
    (peak_growth + 10.0 - PREVIEW_GAP_DIPS).max(0.0).ceil()
}

#[derive(Clone, Copy, Debug)]
pub struct ExpansionBudget {
    pub available_dips: f32,
    pub fraction: f32,
    pub effective_max_scale: f32,
}

/// `CaptureMagnificationExpansionBudget` + `_sessionMaxScale`: the peak scale is
/// capped for the whole hover session so a scroll cannot make icons jump.
pub fn capture_expansion_budget(
    cell_size: f32,
    icon_size: f32,
    configured_max: f32,
    cross_extent_dips: f32,
    used_dips: f32,
    overflow_dips: f32,
    session_max_scale: f32,
) -> ExpansionBudget {
    let permitted = if session_max_scale > 1.0 {
        configured_max.min(session_max_scale)
    } else {
        configured_max
    };
    let available = free_expansion(cross_extent_dips, used_dips, overflow_dips);
    let fraction = compute_expansion_fraction(cell_size, permitted, available);
    ExpansionBudget {
        available_dips: available,
        fraction,
        effective_max_scale: compute_effective_max_scale(
            cell_size, icon_size, permitted, available,
        ),
    }
}

/// The headroom strip and trailing shell group must never start a magnification arch.
pub fn is_pointer_over_non_magnified_zone(
    edge: DockEdge,
    cross_position: f32,
    strip_start: f32,
    strip_end: f32,
) -> bool {
    let _ = edge;
    cross_position < strip_start || cross_position > strip_end
}

pub fn is_native_shell_button(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "start" | "search" | "taskview" | "tray" | "bin"
    )
}

/// True when the pointer's main-axis coordinate is inside the non-magnified rest span.
pub fn is_axis_in_non_magnified_rest_span(
    edge: DockEdge,
    axis_position: f32,
    viewport_start: f32,
    viewport_end: f32,
    rest_start: f32,
    rest_end: f32,
) -> bool {
    if axis_position < viewport_start || axis_position > viewport_end {
        return true;
    }
    let _ = edge;
    axis_position < rest_start || axis_position > rest_end
}

/// `ApplicationIconProvider.UseHoverIcon`: only swap to the hover variant when
/// one exists and the icon is actually magnified.
pub fn use_hover_icon(hovered: bool, magnified: bool, has_hover_variant: bool) -> bool {
    hovered && magnified && has_hover_variant
}

// ---------------------------------------------------------------------------
// Ordering (reference DockItemOrdering).
// ---------------------------------------------------------------------------

pub const PINNED_LEAD_TOKENS: [&str; 3] = ["start", "search", "taskview"];
pub const PINNED_TRAIL_TOKENS: [&str; 2] = ["tray", "bin"];

pub fn is_pinned_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    PINNED_LEAD_TOKENS
        .iter()
        .chain(PINNED_TRAIL_TOKENS.iter())
        .any(|t| lower == *t)
}

/// Insertion index with hysteresis to stop a drag from flickering between slots.
pub fn insertion_index(
    slots: &[(f32, f32)],
    pointer: f32,
    current_index: isize,
    hysteresis: f32,
) -> usize {
    if current_index < 0 || current_index as usize > slots.len() {
        let mut index = 0;
        for &(start, size) in slots {
            if pointer > start + size / 2.0 {
                index += 1;
            } else {
                break;
            }
        }
        return index;
    }
    let mut index = current_index as usize;
    while index < slots.len() && pointer > slots[index].0 + slots[index].1 / 2.0 + hysteresis {
        index += 1;
    }
    while index > 0 && pointer <= slots[index - 1].0 + slots[index - 1].1 / 2.0 - hysteresis {
        index -= 1;
    }
    index
}

/// How far a non-dragged item slides aside to open the landing slot.
pub fn slide_for(item: usize, dragged: usize, insertion_index: usize, dragged_size: f32) -> f32 {
    if item == dragged {
        return 0.0;
    }
    let destination = if insertion_index > dragged {
        insertion_index - 1
    } else {
        insertion_index
    };
    if item > dragged && item <= destination {
        return -dragged_size;
    }
    if item < dragged && item >= destination {
        return dragged_size;
    }
    0.0
}

/// Resolves the persisted placement. Settings written before the typed field
/// used `extra["StartPlacement"]` as a string; unknown values fall back to the
/// typed field instead of silently disabling the button.
pub fn normalized_start_placement(settings: &DockSettings) -> StartButtonPlacement {
    if let Some(raw) = settings
        .extra
        .get("StartPlacement")
        .and_then(|v| v.as_str())
    {
        match raw.trim().to_ascii_lowercase().as_str() {
            "hidden" => return StartButtonPlacement::Hidden,
            "end" => return StartButtonPlacement::End,
            "inwidgets" => return StartButtonPlacement::InWidgets,
            "start" => return StartButtonPlacement::Start,
            _ => {}
        }
    }
    settings.start_placement
}

/// Rewrites the persisted order so pinned shell groups lead and trail the strip.
/// `End` / `InWidgets` move the Start token into the trailing (widget/tray)
/// group instead of leaving the setting ineffective at the head of the strip.
pub fn apply_pinned_groups(order: &[String], settings: &DockSettings) -> Vec<String> {
    let placement = normalized_start_placement(settings);
    let mut lead = Vec::new();
    if placement == StartButtonPlacement::Start {
        lead.push("start".to_owned());
    }
    if settings
        .extra
        .get("ShowSearchButton")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        lead.push("search".to_owned());
    }
    if settings
        .extra
        .get("ShowTaskViewButton")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        lead.push("taskview".to_owned());
    }

    let mut result = lead;
    result.extend(
        order
            .iter()
            .filter(|token| !is_pinned_token(token))
            .cloned(),
    );
    if placement == StartButtonPlacement::End {
        result.push("start".to_owned());
    }
    if settings.show_tray_button {
        result.push("tray".to_owned());
    }
    if settings.show_recycle_bin {
        result.push("bin".to_owned());
    }
    if placement == StartButtonPlacement::InWidgets {
        result.push("start".to_owned());
    }
    result
}

/// Applies a visible drag result to the saved order, keeping hidden items in place.
pub fn move_visible_item(
    saved_order: &[String],
    visible_order: &[String],
    moved_token: &str,
    insertion: usize,
) -> Vec<String> {
    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let mut visible: Vec<String> = Vec::new();
    for token in visible_order {
        if !visible.iter().any(|v| eq(v, token)) {
            visible.push(token.clone());
        }
    }
    let Some(from) = visible.iter().position(|v| eq(v, moved_token)) else {
        return saved_order.to_vec();
    };
    let mut destination = insertion.min(visible.len());
    if from < destination {
        destination -= 1;
    }
    let moved = visible.remove(from);
    visible.insert(destination, moved);

    let visible_set: Vec<String> = visible_order.to_vec();
    let mut result = Vec::with_capacity(saved_order.len() + visible.len());
    let mut seen: Vec<String> = Vec::new();
    let mut next_visible = 0;
    for token in saved_order {
        if seen.iter().any(|v| eq(v, token)) {
            continue;
        }
        seen.push(token.clone());
        if visible_set.iter().any(|v| eq(v, token)) {
            if next_visible < visible.len() {
                result.push(visible[next_visible].clone());
                next_visible += 1;
            }
        } else {
            result.push(token.clone());
        }
    }
    while next_visible < visible.len() {
        result.push(visible[next_visible].clone());
        next_visible += 1;
    }
    result
}

// ---------------------------------------------------------------------------
// Drag & drop / stacks / tray identity.
// ---------------------------------------------------------------------------

/// Windows `CommandLineToArgvW`-compatible quoting (`BuildQuotedArguments`).
pub fn build_quoted_arguments(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.is_empty() {
                "\"\"".to_owned()
            } else if !arg.contains([' ', '\t', '"']) {
                arg.clone()
            } else {
                let mut out = String::with_capacity(arg.len() + 2);
                out.push('"');
                let mut backslashes = 0usize;
                for ch in arg.chars() {
                    if ch == '\\' {
                        backslashes += 1;
                    } else if ch == '"' {
                        out.push_str(&"\\".repeat(backslashes * 2 + 1));
                        out.push('"');
                        backslashes = 0;
                    } else {
                        out.push_str(&"\\".repeat(backslashes));
                        out.push(ch);
                        backslashes = 0;
                    }
                }
                out.push_str(&"\\".repeat(backslashes * 2));
                out.push('"');
                out
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayPinKeyKind {
    Guid,
    Uid,
    Path,
    Wildcard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayPinKey {
    pub kind: TrayPinKeyKind,
    pub value: String,
}

/// Parses the `TrayPinIdentity` string form (`guid:{...}`, `uid:42|{GUID}`,
/// `path:C:\...`, `path:*TrayIcon*`).
pub fn parse_tray_pin_key(raw: &str) -> TrayPinKey {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix("guid:") {
        return TrayPinKey {
            kind: TrayPinKeyKind::Guid,
            value: rest.trim().to_ascii_lowercase(),
        };
    }
    if let Some(rest) = raw.strip_prefix("uid:") {
        return TrayPinKey {
            kind: TrayPinKeyKind::Uid,
            value: rest.trim().to_owned(),
        };
    }
    if let Some(rest) = raw.strip_prefix("path:") {
        let value = rest.trim();
        return TrayPinKey {
            kind: if value.contains('*') {
                TrayPinKeyKind::Wildcard
            } else {
                TrayPinKeyKind::Path
            },
            value: value.to_ascii_lowercase(),
        };
    }
    TrayPinKey {
        kind: TrayPinKeyKind::Path,
        value: raw.to_ascii_lowercase(),
    }
}

/// Wildcard pin keys (`|*...*|` in reference) compare by glob segment.
pub fn tray_pin_matches(key: &TrayPinKey, guid: &str, uid: u32, path: &str) -> bool {
    match key.kind {
        TrayPinKeyKind::Guid => key.value == guid.to_ascii_lowercase(),
        TrayPinKeyKind::Uid => key.value.split('|').any(|part| part == uid.to_string()),
        TrayPinKeyKind::Path => path.to_ascii_lowercase() == key.value,
        TrayPinKeyKind::Wildcard => {
            let pattern = key.value.trim_matches('*').to_ascii_lowercase();
            !pattern.is_empty() && path.to_ascii_lowercase().contains(&pattern)
        }
    }
}

/// Dropping files onto the Recycle Bin icon deletes them; other drops are ignored.
pub fn drop_targets_recycle_bin(hit_bin: bool, paths: &[String]) -> bool {
    hit_bin && !paths.is_empty() && paths.iter().all(|p| !p.trim().is_empty())
}

/// Positions of the first `count` files in a `DockStack` 4-column / 2-row preview.
pub fn folder_stack_preview_cells(count: usize) -> Vec<(usize, usize)> {
    (0..count.min(8)).map(|i| (i % 4, i / 4)).collect()
}

pub fn stack_badge_text(new_items: usize) -> String {
    if new_items == 0 {
        String::new()
    } else {
        format!("+{new_items}")
    }
}

/// `MainWindow.Groups`: live member count shown on a group icon.
pub fn group_live_count(members: &[String], running_executables: &[String]) -> usize {
    members
        .iter()
        .filter(|member| {
            running_executables
                .iter()
                .any(|exe| exe.eq_ignore_ascii_case(member))
        })
        .count()
}

pub fn group_flyout_title(name: &str, live_count: usize, total: usize) -> String {
    if live_count == 0 {
        name.to_owned()
    } else {
        format!("{name} ({live_count}/{total})")
    }
}

pub fn group_reorder(order: &[String], from: usize, to: usize) -> Vec<String> {
    let mut order = order.to_vec();
    if from >= order.len() || to >= order.len() {
        return order;
    }
    let item = order.remove(from);
    order.insert(to, item);
    order
}

/// Partial/incomplete downloads must not appear in folder stacks.
pub fn is_stack_visible_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !(lower.ends_with(".crdownload") || lower.ends_with(".part") || lower.starts_with('~'))
}

// ---------------------------------------------------------------------------
// Auto-hide / hover / keyboard policy helpers.
// ---------------------------------------------------------------------------

/// `CubicEase.EaseOut` / `EaseInOut` as plain functions.
pub fn cubic_ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

pub fn cubic_ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// `RevealedGuardTimer` decision core (`DockAutoHideController.ShouldHide`).
pub fn revealed_guard_should_hide(
    mode: AutoHideMode,
    pointer_over_dock: bool,
    pointer_at_edge: bool,
    overlap: bool,
    shell_menu_open: bool,
    interaction_locked: bool,
    win_key_grace_active: bool,
) -> bool {
    if mode == AutoHideMode::Disabled
        || pointer_over_dock
        || pointer_at_edge
        || shell_menu_open
        || interaction_locked
        || win_key_grace_active
    {
        return false;
    }
    match mode {
        AutoHideMode::Always => true,
        AutoHideMode::Smart => overlap,
        AutoHideMode::Disabled => false,
    }
}

pub fn keyboard_next_index(current: Option<usize>, len: usize, reverse: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match current {
        None => Some(if reverse { len - 1 } else { 0 }),
        Some(i) if reverse && i > 0 => Some(i - 1),
        Some(i) if !reverse && i + 1 < len => Some(i + 1),
        Some(_) => None,
    }
}

pub fn escape_closes_transient(flyout_open: bool, stack_open: bool, island_expanded: bool) -> bool {
    flyout_open || stack_open || island_expanded
}

/// Where focus goes after a flyout/stack closes (reference focus-return handling).
pub fn focus_return_target(flyout_open: bool, dock_item: Option<usize>) -> Option<usize> {
    if flyout_open { dock_item } else { None }
}

/// A slider capture keeps the hover panel open even when the pointer leaves briefly.
pub fn hover_panel_stays_open(
    intent_elapsed_ms: u64,
    pointer_valid: bool,
    slider_captured: bool,
    suppressed: bool,
) -> bool {
    if suppressed || intent_elapsed_ms < HOVER_INTENT_MS {
        return false;
    }
    pointer_valid || slider_captured
}

// ---- Dock message-loop hot-path policies (WS-2) --------------------------------

/// Minimum age of the last Smart-overlap scan. The 200 ms revealed-guard tick and
/// mouse events may never shorten it, even when a real shell/foreground change
/// raised `dirty`.
pub const OVERLAP_SCAN_MIN_INTERVAL: Duration = Duration::from_secs(1);
/// Repeating `ensure_animation()` calls inside one VSync frame are dropped.
pub const ANIMATION_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(8);
/// Minimum age of a cached anchored answer; the Start-window process validation
/// (owned by WS-3) must not run on the 200 ms guard tick.
pub const ANCHOR_CACHE_TTL: Duration = Duration::from_secs(2);

/// Pure throttle decision for the cross-process overlap scan.
///
/// `dirty` is only raised by a genuine shell/foreground change (never by a mouse
/// event or the revealed-guard tick). It *requests* a rescan but cannot bypass
/// the minimum interval. `periodic_due` is the ordinary ~1 s deadline.
pub fn overlap_scan_due(
    last_scan: Option<Instant>,
    dirty: bool,
    periodic_due: bool,
    now: Instant,
) -> bool {
    let min_age_met = last_scan
        .is_none_or(|last| now.saturating_duration_since(last) >= OVERLAP_SCAN_MIN_INTERVAL);
    min_age_met && (dirty || periodic_due)
}

/// `DockSettings.EffectiveAnimationsEnabled`: the app toggle plus Focus Mode.
pub fn effective_animations_enabled(animations_enabled: bool, focus_mode: bool) -> bool {
    animations_enabled && !focus_mode
}

/// Whether the VSync producer should keep posting frames.
///
/// The dock renders with DirectComposition transforms, which are *not* governed by
/// `SPI_GETCLIENTAREAANIMATION`. That OS flag only gates OS-native transitions;
/// consulting it here made the whole dock freeze when the OS flag was off while the
/// in-app toggle was on (the reported bug). Core motion therefore depends only on
/// the effective setting, and `moving` must be true to keep the producer awake.
pub fn vsync_tick_allowed(effective: bool, moving: bool) -> bool {
    effective && moving
}

/// Number of animation frames in an elapsed window, used for the pure tick counter
/// tests and for `WM_APP_VSYNC` cadence checks.
pub fn animation_tick_count(elapsed: Duration, interval: Duration) -> u32 {
    if interval.is_zero() || elapsed.is_zero() {
        return 0;
    }
    (elapsed.as_nanos() / interval.as_nanos()).min(u32::MAX as u128) as u32
}

/// Rate limit for animation wake-ups: repeated calls within a single frame would
/// otherwise post a fresh `WM_APP_VSYNC` on every high-polling-rate mouse event.
pub fn animation_request_due(last_request: Option<Instant>, now: Instant) -> bool {
    last_request
        .is_none_or(|last| now.saturating_duration_since(last) >= ANIMATION_REQUEST_MIN_INTERVAL)
}

/// Coalesces identical `WM_MOUSEMOVE` positions. Returns `true` only for a
/// genuinely new point, and records it so a 1000 Hz mouse cannot re-enter the
/// dock hit-testing/animation path with the same coordinates.
pub fn mouse_move_coalesce(last: &mut Option<(f32, f32)>, mx: f32, my: f32) -> bool {
    if *last == Some((mx, my)) {
        return false;
    }
    *last = Some((mx, my));
    true
}

/// Claims the deferred-settings-reload request. Only the first claim posts
/// `WM_APP_SETTINGS_RELOAD`; further ones coalesce into the pending flag.
pub fn claim_deferred_reload(pending: &mut bool) -> bool {
    if *pending {
        return false;
    }
    *pending = true;
    true
}

/// Consumes the pending deferred reload flag on the idle message.
pub fn take_deferred_reload(pending: &mut bool) -> bool {
    std::mem::replace(pending, false)
}

// ---------------------------------------------------------------------------
// WS-8 live dock policies. Pure so draw / hit-test / host wiring share one
// decision table and every gate is unit-testable without a desktop session.
// ---------------------------------------------------------------------------

/// `DwmEnableAeroPeek`: the REG_DWORD is absent by default (enabled). Only an
/// explicit `0` disables Aero Peek; any other value (including REG_SZ garbage
/// coerced to a number by the caller) keeps the feature on.
pub fn aero_peek_enabled(registry_value: Option<u32>) -> bool {
    registry_value.unwrap_or(1) != 0
}

/// Where the Start button lives in the live strip. Derived from the very same
/// `apply_pinned_groups` policy the settings UI previews, so `End` / `InWidgets`
/// cannot drift between the saved order, the DComp draw pass and hit testing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartStripGroup {
    Head,
    AfterApps,
    InWidgets,
}

pub fn start_strip_group(settings: &DockSettings) -> StartStripGroup {
    let order = apply_pinned_groups(&[], settings);
    let start = order.iter().position(|token| token == "start");
    let trail = order
        .iter()
        .position(|token| token == "tray" || token == "bin");
    match (start, trail) {
        (Some(start), Some(trail)) if start > trail => StartStripGroup::InWidgets,
        (Some(_), _) if normalized_start_placement(settings) == StartButtonPlacement::End => {
            StartStripGroup::AfterApps
        }
        _ => StartStripGroup::Head,
    }
}

/// `StartWidgetIndex` is the zero-based insertion slot inside the widget strip
/// when Start is placed `InWidgets`. Negative / out-of-range values clamp to the
/// strip so the button always lands on a real cell.
pub fn resolved_start_widget_index(settings: &DockSettings, widget_count: usize) -> usize {
    (settings.start_widget_index.max(0) as usize).min(widget_count)
}

/// Mirroring wins over the light-strip default: `mirror_main_dock` explicitly
/// asks for a full clone on every monitor. Otherwise the secondary policy applies.
pub fn secondary_dock_layout(settings: &DockSettings, is_primary: bool) -> SecondaryLayout {
    // Mirroring wins over the light-strip default. `show_on_all_monitors = false`
    // still keeps the single-monitor policy here; `run()` targets every monitor
    // when `mirror_main_dock` is set, so the mirror path remains a full clone.
    if !is_primary && settings.mirror_main_dock && settings.show_on_all_monitors {
        SecondaryLayout::PrimaryDock
    } else {
        secondary_layout(settings, is_primary)
    }
}

/// `secondary_apps = AllMonitors` keeps every running window on a secondary
/// strip; the default scopes notifications to the monitor hosting the window.
pub fn secondary_apps_all_monitors(settings: &DockSettings) -> bool {
    matches!(settings.secondary_apps, SecondaryAppScope::AllMonitors)
}

/// Notification badge accumulation for an EVENT_SYSTEM_ALERT. Counts saturate
/// at 99 (`+99` is the widest badge the chip can paint).
pub fn notification_badge_count(current: u32, increment: u32) -> u32 {
    current.saturating_add(increment.max(1)).min(99)
}

/// Packs a physical cursor point into `LPARAM` for the coordinate-only wheel
/// route. The hook never calls `WindowFromPoint`; the UI thread unpacks and
/// hit-tests against its own dock rects.
pub fn pack_cursor_point(x: i32, y: i32) -> isize {
    (((y as i64) << 32) | (x as u32 as i64)) as isize
}

pub fn unpack_cursor_point(packed: isize) -> (i32, i32) {
    let bits = packed as u64;
    (bits as u32 as i32, (bits >> 32) as u32 as i32)
}

/// A reentrant `WM_APP_VSYNC` may be re-posted at most once. A second re-entry
/// must drop the tick or the guard/ping-pong would spin forever.
pub fn vsync_repost_allowed(already_reposted: bool) -> bool {
    !already_reposted
}

/// Minimum age of a cross-process window snapshot. `sync_slots` runs on the
/// 800 ms TIMER_POLL; it reuses the last snapshot between due deadlines and
/// never performs `EnumWindows + OpenProcess` directly on the UI thread.
pub const WINDOW_SNAPSHOT_MIN_INTERVAL: Duration = Duration::from_millis(750);
pub fn window_snapshot_due(last: Option<Instant>, in_flight: bool, now: Instant) -> bool {
    !in_flight
        && last.is_none_or(|at| now.saturating_duration_since(at) >= WINDOW_SNAPSHOT_MIN_INTERVAL)
}

/// Mouse-driven `refresh_widgets` calls collapse into at most one chip rebuild
/// per frame budget. Settings reloads and animation ticks call the unthrottled
/// entry point explicitly.
pub const WIDGET_REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(120);
pub fn widget_refresh_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.saturating_duration_since(at) >= WIDGET_REFRESH_MIN_INTERVAL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn monitor() -> MonitorInfo {
        MonitorInfo {
            handle: 1,
            device_name: r"\\.\DISPLAY1".to_owned(),
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
            dpi_scale: 1.0,
            is_primary: true,
        }
    }

    #[test]
    fn vertical_dock_flag_defaults_true_and_can_be_disabled() {
        let mut settings = DockSettings::default();
        assert!(vertical_dock_enabled(&settings));
        settings
            .extra
            .insert("enable_vertical_dock".into(), json!(false));
        assert!(!vertical_dock_enabled(&settings));
        assert_eq!(effective_edge(&settings), DockEdge::Bottom);
        settings
            .extra
            .insert("enable_vertical_dock".into(), json!(true));
        settings.edge = DockEdge::Left;
        assert_eq!(effective_edge(&settings), DockEdge::Left);
    }

    #[test]
    fn live_tray_host_flag_defaults_false() {
        assert!(!live_tray_host_enabled(&DockSettings::default()));
        let mut settings = DockSettings::default();
        settings
            .extra
            .insert("enable_live_tray_host".into(), json!(true));
        assert!(live_tray_host_enabled(&settings));
    }

    #[test]
    fn side_edges_swap_along_and_cross_axes() {
        let rect = window_rect_for_edge(
            DockEdge::Left,
            monitor(),
            DockPosition::Center,
            700.0,
            60.0,
            12,
            false,
        );
        assert_eq!(rect, (12, 190, 60, 700));
        let rect = window_rect_for_edge(
            DockEdge::Right,
            monitor(),
            DockPosition::Center,
            700.0,
            60.0,
            12,
            false,
        );
        assert_eq!(rect, (1920 - 12 - 60, 190, 60, 700));
        assert_eq!(axis_extent(DockEdge::Left, rect), (190, 700));
        assert_eq!(cross_extent(DockEdge::Left, rect), (1848, 60));
    }

    #[test]
    fn bottom_edge_stays_bottom_and_honours_position() {
        let rect = window_rect_for_edge(
            DockEdge::Bottom,
            monitor(),
            DockPosition::Right,
            600.0,
            60.0,
            12,
            false,
        );
        assert_eq!(rect, (1920 - 600 - 12, 1020, 600, 60));
        let rect = window_rect_for_edge(
            DockEdge::Bottom,
            monitor(),
            DockPosition::Left,
            600.0,
            60.0,
            12,
            false,
        );
        assert_eq!(rect.0, 12);
    }

    #[test]
    fn full_width_side_dock_fills_monitor_height() {
        let rect = window_rect_for_edge(
            DockEdge::Left,
            monitor(),
            DockPosition::Center,
            700.0,
            60.0,
            12,
            true,
        );
        assert_eq!(rect.1, 12);
        assert_eq!(rect.3, 1080 - 24);
    }

    #[test]
    fn scale_origin_bounce_and_hide_follow_edge() {
        assert_eq!(icon_scale_origin(DockEdge::Bottom), (0.5, 1.0));
        assert_eq!(icon_scale_origin(DockEdge::Right), (0.5, 0.5));
        assert_eq!(bounce_direction(DockEdge::Left), -1.0);
        assert_eq!(bounce_direction(DockEdge::Right), 1.0);
        assert!(hide_offset(DockEdge::Left, 60.0) < 0.0);
        assert!(hide_offset(DockEdge::Bottom, 60.0) > 0.0);
    }

    #[test]
    fn wheel_scroll_direction_matches_reference_on_every_edge() {
        let step = 40.0;
        assert!(wheel_to_axis_delta(120, false, DockEdge::Bottom, step) < 0.0);
        assert!(wheel_to_axis_delta(120, false, DockEdge::Left, step) < 0.0);
        assert!(wheel_to_axis_delta(120, false, DockEdge::Right, step) < 0.0);
        // reference applies `offset -= delta / 120 * cell * 2` for both wheel axes.
        assert!(wheel_to_axis_delta(120, true, DockEdge::Left, step) < 0.0);
    }

    #[test]
    fn show_desktop_strip_and_edge_trigger_follow_edge() {
        let m = monitor();
        let bottom = show_desktop_strip_rect(DockEdge::Bottom, m.clone(), 10.0, 66.0, 1.0);
        assert_eq!(bottom, (1910, 1014, 10, 66));
        let left = show_desktop_strip_rect(DockEdge::Left, m.clone(), 10.0, 40.0, 1.0);
        assert_eq!(left, (0, 1070, 40, 10));
        assert!(is_at_edge_trigger(DockEdge::Left, m.clone(), 3, 500, 1.0));
        assert!(is_at_edge_trigger(
            DockEdge::Bottom,
            m.clone(),
            900,
            1078,
            1.0
        ));
        assert!(!is_at_edge_trigger(DockEdge::Right, m, 900, 500, 1.0));
    }

    #[test]
    fn side_dock_height_clamps_to_available_space() {
        let h = side_dock_height_dips(5, 38.0, 12.0, true, 900.0);
        assert!(h > 38.0 * 5.0);
        assert_eq!(side_dock_height_dips(30, 38.0, 12.0, false, 900.0), 900.0);
    }

    #[test]
    fn shell_overlay_and_fullscreen_filters() {
        assert!(is_shell_overlay_class("XamlExplorerHostIslandWindow"));
        assert!(is_shell_overlay_class("Shell_TrayWnd"));
        assert!(!is_shell_overlay_class("Notepad"));
        // Windows 10/11 Start surfaces must never count as overlapping app windows.
        assert!(is_shell_overlay_class("Windows.UI.Core.CoreWindow"));
        assert!(is_shell_overlay_class("StartMenuSizingFrame"));
        assert!(is_shell_overlay_class("Shell_LightDismissOverlay"));
        // A random UWP CoreWindow is not a shell overlay until its host PID/exe
        // is validated by `win32_ex::is_shell_overlay`.
        assert!(is_shell_overlay_class_for_host(
            "Windows.UI.Core.CoreWindow",
            true
        ));
        assert!(!is_shell_overlay_class_for_host(
            "Windows.UI.Core.CoreWindow",
            false
        ));
        assert!(is_shell_overlay_class_for_host("Shell_TrayWnd", false));
        assert!(!is_shell_overlay_class_for_host("Notepad", true));
        let m = monitor();
        assert!(is_full_screen_on_monitor((0, 0, 1920, 1080), m.clone(), 2));
        assert!(!is_full_screen_on_monitor((100, 0, 1820, 1080), m, 2));
    }

    #[test]
    fn injected_key_events_are_filtered() {
        assert!(!is_injected_key_event(0));
        assert!(is_injected_key_event(LLKHF_INJECTED));
        assert!(is_injected_key_event(LLKHF_INJECTED | 0x01));
        assert!(!is_injected_key_event(0x08));
    }

    #[test]
    fn win_key_gate_rejects_injected_and_modified_taps() {
        // Gate, injected and modifier are all checked before the hook's flag reset.
        assert!(win_key_reveal_allowed(true, false, false));
        assert!(!win_key_reveal_allowed(true, true, false));
        assert!(!win_key_reveal_allowed(true, false, true));
        assert!(!win_key_reveal_allowed(false, false, false));
        // Alt comes from the hook flags; Ctrl/Shift from GetAsyncKeyState.
        assert!(!win_key_modifier_pressed(0, 0, 0));
        assert!(win_key_modifier_pressed(LLKHF_ALTDOWN, 0, 0));
        assert!(win_key_modifier_pressed(0, -32768, 0));
        assert!(win_key_modifier_pressed(0, 0, -1));
    }

    #[test]
    fn win_key_reveal_gate_decision_table() {
        assert!(win_key_reveal_enabled(true, AutoHideMode::Always));
        assert!(win_key_reveal_enabled(true, AutoHideMode::Smart));
        assert!(!win_key_reveal_enabled(true, AutoHideMode::Disabled));
        assert!(!win_key_reveal_enabled(false, AutoHideMode::Always));
        assert!(!win_key_reveal_enabled(false, AutoHideMode::Smart));
        assert!(!win_key_reveal_enabled(false, AutoHideMode::Disabled));
    }

    #[test]
    fn start_menu_hide_fallback_waits_for_a_failed_anchor_attempt() {
        assert!(start_menu_hide_fallback(
            true,
            false,
            false,
            AutoHideMode::Always
        ));
        assert!(!start_menu_hide_fallback(
            true,
            true,
            false,
            AutoHideMode::Always
        ));
        // An attempt still running (~800 ms) must not flicker the dock.
        assert!(!start_menu_hide_fallback(
            true,
            false,
            true,
            AutoHideMode::Always
        ));
        assert!(!start_menu_hide_fallback(
            true,
            false,
            false,
            AutoHideMode::Disabled
        ));
        assert!(!start_menu_hide_fallback(
            false,
            false,
            false,
            AutoHideMode::Smart
        ));
    }

    #[test]
    fn start_menu_sequence_pending_anchored_close_reopen_never_flickers() {
        // UNCLOAKED arrives while the reposition worker is still in flight.
        let pending = start_menu_hide_fallback(true, false, true, AutoHideMode::Always);
        assert!(!pending, "dock must stay put during the ~800 ms attempt");
        // Worker succeeded: anchored, still no hide.
        assert!(!start_menu_hide_fallback(
            true,
            true,
            false,
            AutoHideMode::Always
        ));
        // Attempt completed without anchoring: genuine overlap risk, hide.
        assert!(start_menu_hide_fallback(
            true,
            false,
            false,
            AutoHideMode::Always
        ));
        // Cloak closes Start; the next reopen is pending again, not a hide.
        assert!(!start_menu_hide_fallback(
            false,
            false,
            false,
            AutoHideMode::Always
        ));
        assert!(!start_menu_hide_fallback(
            true,
            false,
            true,
            AutoHideMode::Smart
        ));
    }

    #[test]
    fn start_button_anchor_scales_with_dpi() {
        let window = (100, 900, 800, 1000);
        let button = (20.0, 4.0, 50.0, 40.0);
        assert_eq!(
            anchor_from_window_rect(window, button, 1.0),
            (120, 904, 150, 940)
        );
        assert_eq!(
            anchor_from_window_rect(window, button, 2.0),
            (140, 908, 200, 980)
        );
        // Negative X monitors are preserved by the window origin.
        assert_eq!(
            anchor_from_window_rect((-1920, 900, -1200, 1000), button, 1.0),
            (-1900, 904, -1870, 940)
        );
    }

    #[test]
    fn secondary_shelf_reservation_matches_reference() {
        assert!(needs_own_shelf(0, 1.0, 66.0));
        assert!(!needs_own_shelf(66, 1.0, 66.0));
        assert!(needs_own_shelf(66, 2.0, 66.0));
        assert_eq!(missing_shelf_pixels(66.0, 54, 1.0), 12);
        assert_eq!(missing_shelf_pixels(66.0, 132, 2.0), 0);
        assert_ne!(appbar_position_flags(DockEdge::Bottom) & 0x0040, 0x0040);
        assert_ne!(appbar_position_flags(DockEdge::Left) & 0x0040, 0);
    }

    #[test]
    fn secondary_layout_prefers_light_strip() {
        let mut settings = DockSettings::default();
        assert_eq!(
            secondary_layout(&settings, true),
            SecondaryLayout::PrimaryDock
        );
        assert_eq!(
            secondary_layout(&settings, false),
            SecondaryLayout::LightStrip
        );
        settings.show_on_all_monitors = false;
        assert_eq!(secondary_layout(&settings, false), SecondaryLayout::Hidden);
    }

    #[test]
    fn raised_cosine_falloff_is_c1_and_bounded() {
        assert_eq!(compute_falloff(0.0, 100.0), 1.0);
        assert_eq!(compute_falloff(100.0, 100.0), 0.0);
        assert_eq!(compute_falloff(150.0, 100.0), 0.0);
        assert!((compute_falloff(50.0, 100.0) - 0.5).abs() < 0.0001);
        assert!(compute_falloff(10.0, 0.0) == 0.0);
    }

    #[test]
    fn no_overlap_max_scale_never_exceeds_configured() {
        let max = compute_no_overlap_max_scale(45.0, 38.0, 1.5);
        assert!((1.08..=1.5).contains(&max));
        assert_eq!(compute_no_overlap_max_scale(45.0, 38.0, 1.0), 1.0);
        assert_eq!(compute_no_overlap_max_scale(0.0, 38.0, 1.5), 1.0);
    }

    #[test]
    fn expansion_budget_is_continuous_under_overflow() {
        assert_eq!(free_expansion(100.0, 20.0, 0.0), 80.0);
        assert!((free_expansion(100.0, 20.0, 10.0) - 70.5).abs() < 0.001);
        assert_eq!(compute_expansion_fraction(40.0, 1.5, 0.0), 0.0);
        let budget = capture_expansion_budget(40.0, 34.0, 1.5, 500.0, 100.0, 0.0, 1.35);
        assert!(budget.effective_max_scale <= 1.35);
        assert!(compute_effective_max_scale(40.0, 34.0, 1.5, 0.0) <= 1.5);
    }

    #[test]
    fn spring_step_settles_without_overshoot() {
        let mut value = 1.0;
        let mut velocity = 0.0;
        for _ in 0..120 {
            let (v, vel, settled) = step_spring(value, 1.5, velocity, 1.0 / 60.0, 1.5);
            value = v;
            velocity = vel;
            if settled {
                break;
            }
        }
        assert!((value - 1.5).abs() < 0.01);
        assert!(value <= 1.52);
        assert!(compute_target_scale(1000.0, 100.0, 1.5) == 1.0);
    }

    #[test]
    fn slot_extras_never_exceed_budget() {
        let extras = compute_slot_extras(&[1.5, 1.25, 1.0], &[40.0, 40.0, 40.0], 10.0);
        assert!(extras.iter().sum::<f32>() <= 10.0001);
        assert!(extras[0] > extras[1]);
        assert_eq!(compute_slot_extras(&[1.5], &[40.0], 0.0), vec![0.0]);
        assert!(compute_safe_lift(1.5, 1.5, 40.0, 24.0) >= 0.0);
        assert!(compute_preview_clearance(40.0, 1.5) > 0.0);
    }

    #[test]
    fn magnification_zone_guards() {
        assert!(is_pointer_over_non_magnified_zone(
            DockEdge::Bottom,
            4.0,
            15.0,
            500.0
        ));
        assert!(!is_pointer_over_non_magnified_zone(
            DockEdge::Bottom,
            100.0,
            15.0,
            500.0
        ));
        assert!(is_native_shell_button("START"));
        assert!(!is_native_shell_button("app:chrome"));
        assert!(is_axis_in_non_magnified_rest_span(
            DockEdge::Left,
            5.0,
            0.0,
            900.0,
            20.0,
            800.0
        ));
        assert!(use_hover_icon(true, true, true));
        assert!(!use_hover_icon(true, false, true));
    }

    #[test]
    fn ordering_insertion_has_hysteresis() {
        let slots = [(0.0, 40.0), (49.0, 40.0), (98.0, 40.0)];
        assert_eq!(insertion_index(&slots, 60.0, -1, 0.0), 1);
        assert_eq!(insertion_index(&slots, 60.0, 2, 5.0), 1);
        assert_eq!(insertion_index(&slots, 40.0, 2, 5.0), 1);
        assert_eq!(insertion_index(&slots, 20.0, 0, 0.0), 0);
    }

    #[test]
    fn ordering_slide_and_pinned_groups() {
        assert_eq!(slide_for(1, 0, 2, 40.0), -40.0);
        assert_eq!(slide_for(0, 2, 0, 40.0), 40.0);
        assert_eq!(slide_for(1, 1, 2, 40.0), 0.0);
        let mut settings = DockSettings::default();
        settings
            .extra
            .insert("ShowSearchButton".into(), json!(true));
        settings
            .extra
            .insert("ShowTaskViewButton".into(), json!(true));
        let order = ["app:b", "bin", "app:a"].map(str::to_owned);
        let applied = apply_pinned_groups(&order, &settings);
        assert_eq!(
            applied,
            [
                "start", "search", "taskview", "app:b", "app:a", "tray", "bin"
            ]
        );
    }

    #[test]
    fn typed_start_placement_hides_the_start_button() {
        let mut settings = DockSettings::default();
        let order = ["app:a"].map(str::to_owned);
        assert!(
            apply_pinned_groups(&order, &settings)
                .first()
                .is_some_and(|s| s == "start")
        );
        settings.start_placement = StartButtonPlacement::Hidden;
        assert!(
            !apply_pinned_groups(&order, &settings)
                .iter()
                .any(|s| s == "start")
        );
        // Legacy extra key remains a compatible fallback.
        let mut settings = DockSettings::default();
        settings
            .extra
            .insert("StartPlacement".into(), json!("Hidden"));
        assert!(
            !apply_pinned_groups(&order, &settings)
                .iter()
                .any(|s| s == "start")
        );
    }

    #[test]
    fn start_placement_end_and_in_widgets_move_the_token_to_the_trail() {
        let mut settings = DockSettings {
            show_tray_button: true,
            show_recycle_bin: true,
            ..DockSettings::default()
        };
        let order = ["app:a", "widget:clock"].map(str::to_owned);

        settings.start_placement = StartButtonPlacement::Start;
        let head = apply_pinned_groups(&order, &settings);
        assert_eq!(head.first().map(String::as_str), Some("start"));

        settings.start_placement = StartButtonPlacement::End;
        let end = apply_pinned_groups(&order, &settings);
        assert!(end.first().is_none_or(|s| s != "start"));
        assert_eq!(end, ["app:a", "widget:clock", "start", "tray", "bin"]);

        settings.start_placement = StartButtonPlacement::InWidgets;
        let widgets = apply_pinned_groups(&order, &settings);
        assert_eq!(widgets, ["app:a", "widget:clock", "tray", "bin", "start"]);

        // Legacy string values keep working, and unknown strings normalize to
        // the typed field rather than silently dropping the button.
        let mut legacy = DockSettings::default();
        legacy.extra.insert("StartPlacement".into(), json!("End"));
        assert_eq!(
            normalized_start_placement(&legacy),
            StartButtonPlacement::End
        );
        legacy
            .extra
            .insert("StartPlacement".into(), json!("InWidgets"));
        assert_eq!(
            normalized_start_placement(&legacy),
            StartButtonPlacement::InWidgets
        );
        legacy
            .extra
            .insert("StartPlacement".into(), json!("SomethingElse"));
        assert_eq!(
            normalized_start_placement(&legacy),
            StartButtonPlacement::Start
        );
    }

    /// `DockLayoutChecks` vectors driven through the shipped function, not
    /// through a local copy of the grouping rule.
    #[test]
    fn reference_pinned_group_vectors_call_the_production_function() {
        let mut settings = DockSettings {
            show_tray_button: true,
            show_recycle_bin: true,
            ..DockSettings::default()
        };
        settings
            .extra
            .insert("ShowSearchButton".into(), json!(true));
        settings
            .extra
            .insert("ShowTaskViewButton".into(), json!(true));

        let cases: [(&[&str], &[&str]); 3] = [
            (
                &[
                    "app:b", "bin", "search", "app:a", "tray", "start", "taskview",
                ],
                &[
                    "start", "search", "taskview", "app:b", "app:a", "tray", "bin",
                ],
            ),
            (
                &["bin", "app:x", "tray", "start"],
                &["start", "search", "taskview", "app:x", "tray", "bin"],
            ),
            (
                &["app:a", "widget:clock"],
                &[
                    "start",
                    "search",
                    "taskview",
                    "app:a",
                    "widget:clock",
                    "tray",
                    "bin",
                ],
            ),
        ];
        for (input, expected) in cases {
            let order: Vec<String> = input.iter().map(|token| (*token).to_owned()).collect();
            let expected: Vec<String> = expected.iter().map(|token| (*token).to_owned()).collect();
            assert_eq!(apply_pinned_groups(&order, &settings), expected);
        }

        // Mirrored input keeps the same group edges; the flags decide presence.
        let mirrored: Vec<String> = ["start", "tray", "app:x", "bin"]
            .iter()
            .map(|token| (*token).to_owned())
            .collect();
        let applied = apply_pinned_groups(&mirrored, &settings);
        assert_eq!(applied.first().map(String::as_str), Some("start"));
        assert_eq!(applied.last().map(String::as_str), Some("bin"));

        settings
            .extra
            .insert("ShowSearchButton".into(), json!(false));
        settings
            .extra
            .insert("ShowTaskViewButton".into(), json!(false));
        let applied = apply_pinned_groups(&mirrored, &settings);
        assert!(!applied.iter().any(|token| token == "search"));
        assert!(!applied.iter().any(|token| token == "taskview"));
    }

    #[test]
    fn move_visible_item_keeps_hidden_items_in_place() {
        let saved = [
            "app:a",
            "app:closed",
            "widget:clock",
            "widget:disabled",
            "app:b",
        ]
        .map(str::to_owned);
        let shown = ["app:a", "widget:clock", "app:b"].map(str::to_owned);
        let moved = move_visible_item(&saved, &shown, "widget:clock", 0);
        assert_eq!(
            moved,
            [
                "widget:clock",
                "app:closed",
                "app:a",
                "widget:disabled",
                "app:b"
            ]
        );
        assert_eq!(move_visible_item(&saved, &saved, "nope", 0), saved);
    }

    #[test]
    fn quoted_arguments_follow_windows_argv_rules() {
        let args = [
            "C:\\Program Files\\App.exe".to_owned(),
            "say \"hi\"".to_owned(),
            "".to_owned(),
        ];
        assert_eq!(
            build_quoted_arguments(&args),
            "\"C:\\Program Files\\App.exe\" \"say \\\"hi\\\"\" \"\""
        );
        assert_eq!(build_quoted_arguments(&["plain".to_owned()]), "plain");
    }

    #[test]
    fn tray_pin_identity_supports_guid_uid_path_and_wildcard() {
        let guid = parse_tray_pin_key("guid:{AAAA}");
        assert!(tray_pin_matches(&guid, "{aaaa}", 0, "x"));
        let uid = parse_tray_pin_key("uid:42|{BBBB}");
        assert!(tray_pin_matches(&uid, "", 42, "x"));
        let path = parse_tray_pin_key("path:C:\\Apps\\X.exe");
        assert_eq!(path.kind, TrayPinKeyKind::Path);
        assert!(tray_pin_matches(&path, "", 0, "c:\\apps\\x.exe"));
        let wildcard = parse_tray_pin_key("path:*trayicon*");
        assert_eq!(wildcard.kind, TrayPinKeyKind::Wildcard);
        assert!(tray_pin_matches(&wildcard, "", 0, "custom trayicon host"));
    }

    #[test]
    fn stack_preview_badge_and_drop_rules() {
        let cells = folder_stack_preview_cells(10);
        assert_eq!(cells.len(), 8);
        assert_eq!(cells[3], (3, 0));
        assert_eq!(cells[4], (0, 1));
        assert_eq!(stack_badge_text(0), "");
        assert_eq!(stack_badge_text(3), "+3");
        assert!(is_stack_visible_file("report.pdf"));
        assert!(!is_stack_visible_file("movie.crdownload"));
        assert!(!is_stack_visible_file("part.part"));
        assert!(drop_targets_recycle_bin(true, &["a.txt".into()]));
        assert!(!drop_targets_recycle_bin(false, &["a.txt".into()]));
    }

    #[test]
    fn group_helpers_report_live_count_and_reorder() {
        let members = ["explorer.exe", "wt.exe", "code.exe"]
            .map(str::to_owned)
            .to_vec();
        let running = ["WT.EXE".to_owned(), "code.exe".to_owned()];
        assert_eq!(group_live_count(&members, &running), 2);
        assert_eq!(group_flyout_title("Dev", 2, 3), "Dev (2/3)");
        assert_eq!(group_flyout_title("Dev", 0, 3), "Dev");
        assert_eq!(
            group_reorder(&members, 2, 0),
            ["code.exe", "explorer.exe", "wt.exe"]
        );
        assert_eq!(group_reorder(&members, 0, 9), members);
    }

    #[test]
    fn cubic_ease_and_auto_hide_guard() {
        assert_eq!(cubic_ease_out(0.0), 0.0);
        assert_eq!(cubic_ease_out(1.0), 1.0);
        assert!(cubic_ease_out(0.5) > 0.5);
        assert!((cubic_ease_in_out(0.5) - 0.5).abs() < 0.0001);
        assert!(!revealed_guard_should_hide(
            AutoHideMode::Always,
            false,
            false,
            false,
            false,
            false,
            true
        ));
        assert!(revealed_guard_should_hide(
            AutoHideMode::Always,
            false,
            false,
            false,
            false,
            false,
            false
        ));
        assert!(revealed_guard_should_hide(
            AutoHideMode::Smart,
            false,
            false,
            true,
            false,
            false,
            false
        ));
        assert!(!revealed_guard_should_hide(
            AutoHideMode::Smart,
            false,
            false,
            false,
            false,
            false,
            false
        ));
    }

    #[test]
    fn keyboard_navigation_and_hover_panel_policy() {
        assert_eq!(keyboard_next_index(None, 3, false), Some(0));
        assert_eq!(keyboard_next_index(None, 3, true), Some(2));
        assert_eq!(keyboard_next_index(Some(2), 3, false), None);
        assert_eq!(focus_return_target(true, Some(2)), Some(2));
        assert!(escape_closes_transient(false, true, false));
        assert!(!hover_panel_stays_open(349, true, false, false));
        assert!(hover_panel_stays_open(350, false, true, false));
        assert!(!hover_panel_stays_open(500, true, true, true));
    }

    #[test]
    fn overlap_scan_throttle_decision_table() {
        let now = Instant::now();
        // With no prior scan the first request (dirty or the initial periodic
        // deadline) is due; a scan still needs an actual request.
        assert!(overlap_scan_due(None, false, true, now));
        assert!(overlap_scan_due(None, true, false, now));
        assert!(overlap_scan_due(None, true, true, now));
        assert!(!overlap_scan_due(None, false, false, now));
        // A real shell/foreground change (dirty) may NEVER bypass the 1 s minimum.
        assert!(!overlap_scan_due(
            Some(now - Duration::from_millis(100)),
            true,
            true,
            now
        ));
        assert!(!overlap_scan_due(
            Some(now - Duration::from_millis(999)),
            true,
            false,
            now
        ));
        // A periodic deadline is equally throttled.
        assert!(!overlap_scan_due(
            Some(now - Duration::from_millis(200)),
            false,
            true,
            now
        ));
        // At the deadline either a dirty request or the periodic tick scans once.
        assert!(overlap_scan_due(
            Some(now - OVERLAP_SCAN_MIN_INTERVAL),
            false,
            true,
            now
        ));
        assert!(overlap_scan_due(
            Some(now - Duration::from_secs(2)),
            true,
            false,
            now
        ));
    }

    #[test]
    fn animation_request_rate_limit_decision_table() {
        let now = Instant::now();
        assert!(animation_request_due(None, now));
        assert!(!animation_request_due(
            Some(now - Duration::from_millis(1)),
            now
        ));
        assert!(!animation_request_due(
            Some(now - Duration::from_millis(7)),
            now
        ));
        assert!(animation_request_due(
            Some(now - ANIMATION_REQUEST_MIN_INTERVAL),
            now
        ));
        assert!(animation_request_due(
            Some(now - Duration::from_millis(100)),
            now
        ));
    }

    /// Pure decision table for the animation toggle / VSync chain:
    /// (in-app toggle, focus mode, motion pending) -> tick allowed.
    #[test]
    fn animation_toggle_decision_table_and_tick_counter() {
        let table = [
            (true, false, true, true),   // normal fisheye/spring/bounce
            (true, false, false, false), // at rest: producer parks (0 CPU)
            (true, true, true, false), // focus mode suppresses motion (reference EffectiveAnimationsEnabled)
            (false, false, true, false), // toggle off: no VSync wake-ups
            (false, true, true, false),
        ];
        for (enabled, focus_mode, moving, expected) in table {
            let effective = effective_animations_enabled(enabled, focus_mode);
            assert_eq!(
                vsync_tick_allowed(effective, moving),
                expected,
                "animations_enabled={enabled} focus_mode={focus_mode} moving={moving}"
            );
        }

        // 190 ms slide at a 60 Hz VSync cadence gets ~11 ticks.
        assert_eq!(
            animation_tick_count(Duration::from_millis(190), Duration::from_millis(16)),
            11
        );
        // Disabled animations: producer never wakes, so no ticks are accumulated.
        let effective = effective_animations_enabled(false, false);
        assert!(!vsync_tick_allowed(effective, true));
        assert_eq!(
            animation_tick_count(Duration::from_secs(5), Duration::from_millis(16)),
            312
        );
    }

    #[test]
    fn identical_mouse_moves_are_coalesced() {
        let mut last = None;
        assert!(mouse_move_coalesce(&mut last, 120.0, 40.0));
        assert_eq!(last, Some((120.0, 40.0)));
        // A 1000 Hz mouse delivering the same point does no hit-test work.
        assert!(!mouse_move_coalesce(&mut last, 120.0, 40.0));
        assert!(!mouse_move_coalesce(&mut last, 120.0, 40.0));
        assert!(mouse_move_coalesce(&mut last, 120.5, 40.0));
        assert!(mouse_move_coalesce(&mut last, 120.5, 40.5));
        assert_eq!(last, Some((120.5, 40.5)));
    }

    #[test]
    fn deferred_settings_reload_flag_coalesces_and_consumes() {
        let mut pending = false;
        assert!(claim_deferred_reload(&mut pending));
        assert!(pending);
        // Many hover-close events in one frame produce a single posted message.
        assert!(!claim_deferred_reload(&mut pending));
        assert!(!claim_deferred_reload(&mut pending));
        assert!(take_deferred_reload(&mut pending));
        assert!(!pending);
        assert!(!take_deferred_reload(&mut pending));
        // A later change can arm a fresh reload.
        assert!(claim_deferred_reload(&mut pending));
        assert!(take_deferred_reload(&mut pending));
    }

    #[test]
    fn aero_peek_registry_gate_defaults_on_and_explicit_zero_disables() {
        assert!(aero_peek_enabled(None));
        assert!(aero_peek_enabled(Some(1)));
        assert!(aero_peek_enabled(Some(0xffff_ffff)));
        assert!(!aero_peek_enabled(Some(0)));
    }

    #[test]
    fn start_strip_group_comes_from_the_live_pinned_group_policy() {
        let mut settings = DockSettings::default();
        assert_eq!(start_strip_group(&settings), StartStripGroup::Head);
        settings.start_placement = StartButtonPlacement::End;
        assert_eq!(start_strip_group(&settings), StartStripGroup::AfterApps);
        settings.start_placement = StartButtonPlacement::InWidgets;
        assert_eq!(start_strip_group(&settings), StartStripGroup::InWidgets);
        // Legacy extra string drives the same decision.
        let mut legacy = DockSettings::default();
        legacy
            .extra
            .insert("StartPlacement".into(), json!("InWidgets"));
        assert_eq!(start_strip_group(&legacy), StartStripGroup::InWidgets);
        legacy.extra.insert("StartPlacement".into(), json!("End"));
        assert_eq!(start_strip_group(&legacy), StartStripGroup::AfterApps);
    }

    #[test]
    fn start_widget_index_clamps_to_the_widget_strip() {
        let mut settings = DockSettings {
            start_widget_index: 5,
            ..DockSettings::default()
        };
        assert_eq!(resolved_start_widget_index(&settings, 3), 3);
        assert_eq!(resolved_start_widget_index(&settings, 8), 5);
        settings.start_widget_index = -2;
        assert_eq!(resolved_start_widget_index(&settings, 3), 0);
    }

    #[test]
    fn mirror_overrides_light_strip_but_not_single_monitor_mode() {
        let mut settings = DockSettings::default();
        assert_eq!(
            secondary_dock_layout(&settings, false),
            SecondaryLayout::LightStrip
        );
        settings.mirror_main_dock = true;
        assert_eq!(
            secondary_dock_layout(&settings, false),
            SecondaryLayout::PrimaryDock
        );
        assert_eq!(
            secondary_dock_layout(&settings, true),
            SecondaryLayout::PrimaryDock
        );
        settings.show_on_all_monitors = false;
        assert_eq!(
            secondary_dock_layout(&settings, false),
            SecondaryLayout::Hidden
        );
        assert!(!secondary_apps_all_monitors(&settings));
        settings.secondary_apps = SecondaryAppScope::AllMonitors;
        assert!(secondary_apps_all_monitors(&settings));
    }

    #[test]
    fn notification_badges_saturate_at_99() {
        assert_eq!(notification_badge_count(0, 1), 1);
        assert_eq!(notification_badge_count(5, 3), 8);
        assert_eq!(notification_badge_count(98, 5), 99);
        assert_eq!(notification_badge_count(0, 0), 1);
    }

    #[test]
    fn cursor_point_survives_lparam_round_trip() {
        for &(x, y) in &[(0, 0), (120, 40), (-1920, 1080), (3839, -12)] {
            assert_eq!(unpack_cursor_point(pack_cursor_point(x, y)), (x, y));
        }
    }

    #[test]
    fn vsync_repost_is_single_shot() {
        assert!(vsync_repost_allowed(false));
        assert!(!vsync_repost_allowed(true));
    }

    #[test]
    fn window_snapshot_and_widget_refresh_are_throttled() {
        let now = Instant::now();
        assert!(window_snapshot_due(None, false, now));
        assert!(!window_snapshot_due(None, true, now));
        assert!(!window_snapshot_due(
            Some(now - Duration::from_millis(100)),
            false,
            now
        ));
        assert!(window_snapshot_due(
            Some(now - WINDOW_SNAPSHOT_MIN_INTERVAL),
            false,
            now
        ));
        assert!(widget_refresh_due(None, now));
        assert!(!widget_refresh_due(
            Some(now - Duration::from_millis(20)),
            now
        ));
        assert!(widget_refresh_due(
            Some(now - WIDGET_REFRESH_MIN_INTERVAL),
            now
        ));
    }

    #[test]
    fn reference_size_profile_vectors_are_exact() {
        let expected = [
            (48.0, 38.0, 28.0, 16.0, 52.0, 76.0),
            (58.0, 44.0, 34.0, 19.0, 60.0, 84.0),
            (66.0, 50.0, 38.0, 22.0, 68.0, 92.0),
        ];
        for (size, vector) in [DockSize::Compact, DockSize::Medium, DockSize::Large]
            .into_iter()
            .zip(expected)
        {
            let settings = DockSettings {
                size,
                magnification_expands_dock: false,
                ..DockSettings::default()
            };
            let profile = size_profile(&settings);
            assert_eq!(
                (
                    profile.shelf,
                    profile.cell,
                    profile.icon,
                    profile.radius,
                    profile.secondary_shelf,
                    profile.headroom
                ),
                vector
            );
            assert_eq!(
                profile.preview_card,
                if size == DockSize::Large {
                    PREVIEW_CARD_LARGE_DIPS
                } else {
                    PREVIEW_CARD_DIPS
                }
            );
        }
        let large = DockSettings {
            size: DockSize::Large,
            ..DockSettings::default()
        };
        assert_eq!(size_profile(&large).preview_card, PREVIEW_CARD_LARGE_DIPS);
    }

    #[test]
    fn magnification_expands_dock_removes_headroom() {
        let settings = DockSettings::default();
        // reference #182 default: magnification may expand the dock surface.
        assert!(magnification_expands_dock(&settings));
        assert_eq!(size_profile(&settings).headroom, 0.0);
        let mut fixed = settings.clone();
        fixed.magnification_expands_dock = false;
        assert!(!magnification_expands_dock(&fixed));
        assert!(size_profile(&fixed).headroom > 0.0);
    }

    #[test]
    fn typed_magnification_setting_wins_over_stale_extra_key() {
        let mut settings = DockSettings {
            magnification_expands_dock: false,
            ..DockSettings::default()
        };
        settings
            .extra
            .insert("MagnificationExpandsDock".into(), serde_json::json!(true));
        assert!(!magnification_expands_dock(&settings));
        assert_eq!(legacy_magnification_expands_dock(&settings), Some(true));
        assert!(size_profile(&settings).headroom > 0.0);

        settings.magnification_expands_dock = true;
        settings
            .extra
            .insert("MagnificationExpandsDock".into(), serde_json::json!(false));
        assert!(magnification_expands_dock(&settings));
        assert_eq!(legacy_magnification_expands_dock(&settings), Some(false));
        assert_eq!(size_profile(&settings).headroom, 0.0);

        settings.extra.clear();
        assert_eq!(legacy_magnification_expands_dock(&settings), None);
    }

    #[test]
    fn wheel_direction_and_step_match_reference() {
        let step = wheel_scroll_step(38.0, 9.0);
        assert_eq!(step, 94.0);
        for edge in [DockEdge::Bottom, DockEdge::Left, DockEdge::Right] {
            assert_eq!(wheel_to_axis_delta(120, false, edge, step), -step);
            assert_eq!(wheel_to_axis_delta(-120, false, edge, step), step);
        }
        assert_eq!(wheel_to_axis_delta(120, true, DockEdge::Bottom, step), step);
        assert!(has_scroll_overflow(0.6));
        assert!(!has_scroll_overflow(0.4));
    }

    #[test]
    fn content_alignment_and_gesture_toggles_are_decided_by_policy() {
        assert_eq!(
            content_alignment_offset(500.0, 300.0, DockContentAlignment::Left),
            0.0
        );
        assert_eq!(
            content_alignment_offset(500.0, 300.0, DockContentAlignment::Center),
            100.0
        );
        assert_eq!(
            content_alignment_offset(500.0, 300.0, DockContentAlignment::Right),
            200.0
        );
        assert_eq!(
            content_alignment_offset(300.0, 500.0, DockContentAlignment::Center),
            0.0
        );
        assert!(drag_hover_allowed(true));
        assert!(!drag_hover_allowed(false));
        assert!(use_hover_icon(true, true, true));
        assert!(!use_hover_icon(true, false, true));
        assert!(!use_hover_icon(true, true, false));
    }

    #[test]
    fn mirror_dock_overrides_light_strip_but_not_single_monitor_policy() {
        let mut settings = DockSettings {
            show_on_all_monitors: false,
            mirror_main_dock: true,
            ..DockSettings::default()
        };
        assert_eq!(
            secondary_dock_layout(&settings, false),
            SecondaryLayout::Hidden
        );
        settings.show_on_all_monitors = true;
        assert_eq!(
            secondary_dock_layout(&settings, false),
            SecondaryLayout::PrimaryDock
        );
        settings.mirror_main_dock = false;
        assert_eq!(
            secondary_dock_layout(&settings, false),
            SecondaryLayout::LightStrip
        );
        settings
            .extra
            .insert("secondary_light_strip".into(), serde_json::json!(false));
        assert_eq!(
            secondary_dock_layout(&settings, false),
            SecondaryLayout::PrimaryDock
        );
    }
}
