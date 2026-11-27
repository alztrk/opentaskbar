//! Native Win32 + DirectComposition Dynamic Island window & multi-display manager.
//!
//! Mirrors C# `Views/Windows/DynamicIslandManager.cs` and `DynamicIslandWindow.xaml.cs`:
//! - One top-edge floating island per connected display with assigned widgets
//! - Horizontal paging via Prev/Next buttons, pointer drag/swipe, or Left/Right keys when hovered
//! - Per-display widget ordering (`DynamicIslandWidgetOrders`), placement context menus,
//!   desktop-only visibility mode, and fullscreen app suppression.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use opentaskbar_core::dynamic_island as policy;
use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::{self, DockSettings};
use windows::Win32::Graphics::DirectComposition::IDCompositionRectangleClip;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_LEFT,
    VK_RIGHT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW,
    HTCLIENT, HWND_TOPMOST, IDC_ARROW, IDC_HAND, KillTimer, LoadCursorW, MA_NOACTIVATE,
    RegisterClassExW, RegisterWindowMessageW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOZORDER, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_NCHITTEST, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::dcomp::{DcompEngine, IslandButton, VisualNode, WidgetInteraction};
use super::layout::{ISLAND_ANIM_MS, effective_animations_enabled};
use crate::design;
use crate::platform::win32::{self, ForegroundProbe, MonitorInfo, WidgetContextAction};
use crate::ui::settings as settings_ui;
use crate::widgets::{self, WidgetRuntime};

// All geometry comes from the C# vectors in `opentaskbar_core::dynamic_island::geometry`.
use opentaskbar_core::dynamic_island::geometry as island_geo;

const COMPACT_HEIGHT_DIPS: f32 = island_geo::COMPACT_HEIGHT_DIPS;
const EXPANDED_HEIGHT_DIPS: f32 = island_geo::EXPANDED_HEIGHT_DIPS;
const COMPACT_MIN_WIDTH_DIPS: f32 = island_geo::COMPACT_MIN_WIDTH_DIPS;
const MAX_WIDTH_DIPS: f32 = island_geo::MAX_WIDTH_DIPS;
const RIGHT_CONTROLS_DIPS: f32 = island_geo::RIGHT_CONTROLS_DIPS;
const LEFT_CONTROLS_DIPS: f32 = island_geo::LEFT_CONTROLS_DIPS;
const CHIP_H_DIPS: f32 = 38.0;
const SLIDE_DURATION_SECS: f32 = 0.190;
/// `FlyoutEdge::Bottom` places the popup *above* the anchor. The island is on the
/// top edge, so the anchor is shifted down by the C# expanded height to make the
/// popup appear just below the island instead of being clamped to the top margin.
const FLYOUT_ANCHOR_OFFSET_DIPS: f32 = island_geo::EXPANDED_HEIGHT_DIPS;
const WM_MOUSELEAVE: u32 = 0x02A3;

const TIMER_VISIBILITY: usize = 1;
const TIMER_WIDGETS: usize = 2;
const TIMER_ANIM: usize = 3;
const TIMER_HIDE_ANIM: usize = 4;

const DEFAULT_WIDGET_ORDER: &[&str] = &[
    "clock",
    "weather",
    "dayprogress",
    "stopwatch",
    "countdown",
    "stats",
    "netspeed",
    "battery",
    "bluetoothbattery",
    "storage",
    "aiusage_claude",
    "aiusage_codex",
    "aiusage_opencode",
    "aiusage_copilot",
    "aiusage_deepseek",
    "aiusage_openrouter",
    "aiusage_cursor",
    "media",
    "audio",
    "quicksettings",
    "keyboard",
    "focus",
];

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn loword_i16(v: isize) -> f32 {
    (v as u32 & 0xffff) as i16 as f32
}

fn hiword_i16(v: isize) -> f32 {
    ((v as u32 >> 16) & 0xffff) as i16 as f32
}

fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

/// What an island button / chip click must do. Pure so the flyout dispatch can be
/// unit tested without a HWND (WS-C: no island click may end in a dead end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandClickAction {
    PrevPage,
    NextPage,
    /// Call `WidgetRuntime::open_widget_flyout` (the real widget flyout).
    OpenFlyout,
    /// C# in-place `DetailsPanel` for timed widgets without a separate flyout.
    ToggleDetails,
}

/// True when the widget has a real flyout implementation in `widgets::WidgetRuntime`.
pub fn has_real_widget_flyout(widget_id: &str) -> bool {
    let id = widget_id.to_ascii_lowercase();
    matches!(
        id.as_str(),
        "clock"
            | "weather"
            | "dayprogress"
            | "stopwatch"
            | "countdown"
            | "stats"
            | "netspeed"
            | "bluetoothbattery"
            | "storage"
            | "keyboard"
            | "media"
            | "audio"
            | "focus"
            | "aiusage_all"
    ) || id.starts_with("aiusage_")
}

/// C# `Previous_Click` / `Next_Click` / `Expand_Click`: the expand chevron toggles
/// the island's in-place expanded details panel (`50 -> 246 DIP`), while clicking
/// the center widget chip opens the full interactive flyout.
pub fn island_button_click_action(button: IslandButton, _widget_id: &str) -> IslandClickAction {
    match button {
        IslandButton::Prev => IslandClickAction::PrevPage,
        IslandButton::Next => IslandClickAction::NextPage,
        IslandButton::Expand => IslandClickAction::ToggleDetails,
    }
}

/// C# `PageCount.Text` uses culture-aware `{0:00} / {1:00}` formatting. The
/// catalog key is requested first; until WS-F adds it to all 14 locales the
/// numeric fallback keeps the old behaviour instead of showing a raw key.
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

/// Approximates C# `_details.Measure(new Size(640, ...)).DesiredSize.Width`.
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

/// Screen anchor for a top-edge island flyout. `ui::flyout::FlyoutEdge::Bottom`
/// places the popup above the anchor, so we offset by the C# expanded height to
/// land below the island. Horizontally centered on the island/monitor.
pub fn island_flyout_anchor(
    monitor_left: i32,
    monitor_width: i32,
    island_bottom_y: i32,
    dpi: f32,
) -> (i32, i32) {
    let x = monitor_left + monitor_width / 2;
    let y = island_bottom_y + (FLYOUT_ANCHOR_OFFSET_DIPS * dpi.max(0.5)).round() as i32;
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
    if settings.focus_mode && widget_id != "focus" && widget_id != "clock" {
        return false;
    }
    if !settings.is_widget_enabled(widget_id) {
        return false;
    }
    let names: Vec<&str> = monitors.iter().map(|m| m.device_name.as_str()).collect();
    policy::is_assigned_to_any_display(&settings.dynamic_island_placements, widget_id, &names)
}

pub struct DynamicIslandManager {
    windows: HashMap<String, Box<DynamicIslandWindow>>,
    reload_msg: u32,
}

impl DynamicIslandManager {
    pub fn new() -> Self {
        register_island_class();
        let reload_msg =
            unsafe { RegisterWindowMessageW(wide(settings_ui::RELOAD_MSG_NAME).as_ptr()) };
        Self {
            windows: HashMap::new(),
            reload_msg,
        }
    }

    pub fn apply(&mut self, settings: &DockSettings, monitors: &[MonitorInfo], is_dark: bool) {
        if !settings.dynamic_island_enable {
            // Tear down any existing islands: a disabled feature owns no windows and renders nothing.
            self.destroy_all();
            return;
        }

        let selected: Vec<String> = widgets::active_widgets(settings)
            .into_iter()
            .map(|w| w.id.to_owned())
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
            ) {
                self.windows.insert(key, win);
            }
        }
    }

    pub fn tick(&mut self) {
        for win in self.windows.values_mut() {
            win.tick();
        }
    }

    pub fn destroy_all(&mut self) {
        for (_, win) in self.windows.drain() {
            win.destroy();
        }
    }
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
    dcomp: DcompEngine,
    spatial_glass: Option<design::spatial_glass::SpatialGlassHost>,
    chrome_node: VisualNode,
    viewport_node: VisualNode,
    viewport_clip: IDCompositionRectangleClip,
    page_nodes: Vec<VisualNode>,
    page_ids: Vec<String>,
    current_page: usize,
    width_dips: f32,
    slide_offset_px: f32,
    slide_anim: Option<SlideAnim>,
    is_dragging: bool,
    drag_start_x: f32,
    drag_start_page: usize,
    hovered_btn: Option<IslandButton>,
    pressed_btn: Option<IslandButton>,
    hovered_widget_sub: Option<u8>,
    pressed_widget: bool,
    tracking_mouse: bool,
    is_visible: bool,
    target_hidden: bool,
    hide_progress: f32,
    hide_last_tick: Option<Instant>,
    widget_runtime: WidgetRuntime,
    /// Expanded-details state machine. Shared with the dock host so Escape,
    /// paging and `request_details()` follow one policy.
    island: IslandState,
    details_bg: VisualNode,
    details_node: VisualNode,
}

impl DynamicIslandWindow {
    fn create(
        monitor: MonitorInfo,
        widget_ids: Vec<String>,
        settings: &DockSettings,
        all_monitors: &[MonitorInfo],
        is_dark: bool,
        reload_msg: u32,
    ) -> Option<Box<Self>> {
        let class_name = wide("OpenTaskBarDynamicIsland");
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let ex_style =
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP;
        let init_w = (COMPACT_MIN_WIDTH_DIPS * monitor.dpi_scale).ceil() as i32;
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
        // C# `IslandSurface` has CornerRadius="0,0,20,20" and ClipToBounds=true;
        // use the same bottom radius on the page viewport clip so chips cannot bleed
        // through the rounded corners.
        DcompEngine::update_rounded_clip_rect(
            &viewport_clip,
            LEFT_CONTROLS_DIPS * monitor.dpi_scale,
            0.0,
            (COMPACT_MIN_WIDTH_DIPS - RIGHT_CONTROLS_DIPS) * monitor.dpi_scale,
            init_h as f32,
            0.0,
            island_geo::BOTTOM_RADIUS_DIPS * monitor.dpi_scale,
        );
        dcomp.add_child(&dcomp.root, &chrome_node);
        dcomp.add_child(&dcomp.root, &viewport_node);
        let details_bg = dcomp.create_node().ok()?;
        let details_node = dcomp.create_node().ok()?;
        details_bg.hide();
        details_node.hide();
        dcomp.add_child(&dcomp.root, &details_bg);
        dcomp.add_child(&dcomp.root, &details_node);

        let mut widget_runtime = WidgetRuntime::default();
        widget_runtime.tick(settings);

        let glass_cfg = design::spatial_glass::SpatialGlassConfig::from_settings(
            settings,
            settings.dynamic_island_background,
            settings.dynamic_island_opacity,
            is_dark,
            true,
        );
        let spatial_glass =
            design::spatial_glass::SpatialGlassHost::attach(hwnd_val, false, glass_cfg);

        let mut boxed = Box::new(Self {
            hwnd: hwnd_val,
            monitor,
            all_monitors: all_monitors.to_vec(),
            settings: settings.clone(),
            is_dark,
            reload_msg,
            dcomp,
            spatial_glass,
            chrome_node,
            viewport_node,
            viewport_clip,
            page_nodes: Vec::new(),
            page_ids: Vec::new(),
            current_page: 0,
            width_dips: COMPACT_MIN_WIDTH_DIPS,
            slide_offset_px: 0.0,
            slide_anim: None,
            is_dragging: false,
            drag_start_x: 0.0,
            drag_start_page: 0,
            hovered_btn: None,
            pressed_btn: None,
            hovered_widget_sub: None,
            pressed_widget: false,
            tracking_mouse: false,
            is_visible: false,
            target_hidden: true,
            hide_progress: 1.0,
            hide_last_tick: None,
            widget_runtime,
            island: IslandState::new(widget_ids.clone()),
            details_bg,
            details_node,
        });

        let ptr = (&mut *boxed as *mut Self) as isize;
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);
            SetTimer(hwnd, TIMER_VISIBILITY, 400, None);
            SetTimer(hwnd, TIMER_WIDGETS, 1000, None);
        }

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
        self.apply_internal(widget_ids, settings, is_dark);
    }

    fn apply_internal(&mut self, widget_ids: Vec<String>, settings: &DockSettings, is_dark: bool) {
        self.settings = settings.clone();
        self.is_dark = is_dark;

        let display_order = settings
            .dynamic_island_widget_orders
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&self.monitor.device_name))
            .map(|(_, v)| v.as_slice());
        // C# passes displayOrder, widgetOrder, defaultOrder and timedOrder. The
        // timed widgets are already part of DEFAULT_WIDGET_ORDER; adding the
        // persisted widget order restores user reordering in the island.
        let ordered = policy::order_widgets(
            &widget_ids,
            display_order,
            Some(&settings.widget_order),
            DEFAULT_WIDGET_ORDER,
        );

        let selected_id = self
            .page_ids
            .get(self.current_page)
            .cloned()
            .or_else(|| ordered.first().cloned());

        if ordered != self.page_ids {
            self.page_ids = ordered;
            self.current_page = selected_id
                .and_then(|sel| {
                    self.page_ids
                        .iter()
                        .position(|id| id.eq_ignore_ascii_case(&sel))
                })
                .unwrap_or(0);
            self.slide_anim = None;
            self.slide_offset_px = -((self.current_page as f32) * self.viewport_width_px());
            self.rebuild_page_nodes();
        }
        // Keep the shared state machine's page view in sync without losing the
        // expanded flag: a settings reload must not silently collapse the island.
        self.island.page_ids = self.page_ids.clone();
        self.island.page_index = self.current_page.min(self.page_ids.len().saturating_sub(1));

        self.width_dips = self.width_for_current_page();
        let glass_cfg = design::spatial_glass::SpatialGlassConfig::from_settings(
            &self.settings,
            self.settings.dynamic_island_background,
            self.settings.dynamic_island_opacity,
            self.is_dark,
            true,
        );
        if let Some(ref mut glass) = self.spatial_glass {
            glass.apply_config(glass_cfg);
        }
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

    fn rebuild_page_nodes(&mut self) {
        self.dcomp.remove_all_children(&self.viewport_node.visual);
        self.page_nodes.clear();
        for _ in &self.page_ids {
            if let Ok(node) = self.dcomp.create_node() {
                self.dcomp.add_child(&self.viewport_node.visual, &node);
                self.page_nodes.push(node);
            }
        }
    }

    fn current_page_id(&self) -> Option<&str> {
        self.page_ids.get(self.current_page).map(String::as_str)
    }

    /// C# `HasExpandableContent`: timed widgets plus Clock/Focus/SystemStats/
    /// Weather/Media/Audio/NetSpeed/BluetoothBattery and every `AiUsage*` widget.
    fn has_expandable_content(id: Option<&str>) -> bool {
        let Some(id) = id else {
            return false;
        };
        let id = id.to_ascii_lowercase();
        matches!(
            id.as_str(),
            "clock"
                | "focus"
                | "stats"
                | "weather"
                | "media"
                | "audio"
                | "netspeed"
                | "bluetoothbattery"
                | "stopwatch"
                | "countdown"
        ) || id.starts_with("aiusage") // C#: id.StartsWith("AiUsage", OrdinalIgnoreCase)
    }

    fn width_for_current_page(&self) -> f32 {
        // Ours: an island with no assigned widgets stays a compact chip instead
        // of reserving the full C# widget estimate.
        if self.page_ids.is_empty() {
            let monitor_dips = self.monitor.width() as f32 / self.monitor.dpi_scale.max(0.5);
            return policy::clamp_window_width(220.0, monitor_dips, 180.0, MAX_WIDTH_DIPS, 16.0);
        }
        // WS-C: C# `WidthForCurrentPage` uses `widget.FullWidthEstimate` and the
        // measured details width. The Rust chip already carries 168-314 DIP of
        // content, so the extra 112 DIP approximates the full WPF widget inside
        // the island. Expansion is vertical (50 -> 246 DIP), never "width + 246".
        let chip_w = self
            .current_page_id()
            .map(|id| self.widget_runtime.widget_width_dips(id))
            .unwrap_or(island_geo::FALLBACK_WIDGET_WIDTH_DIPS - 112.0);
        let full_widget_w = chip_w + 112.0;
        let details_w = self
            .island
            .details
            .as_ref()
            .map(details_desired_width)
            .unwrap_or(0.0);
        let monitor_dips = self.monitor.width() as f32 / self.monitor.dpi_scale.max(0.5);
        policy::window_width_for_page(
            Some(full_widget_w),
            details_w,
            monitor_dips,
            self.island.expanded,
        )
    }

    fn viewport_width_dips(&self) -> f32 {
        (self.width_dips - LEFT_CONTROLS_DIPS - RIGHT_CONTROLS_DIPS).max(1.0)
    }

    fn viewport_width_px(&self) -> f32 {
        self.viewport_width_dips() * self.monitor.dpi_scale
    }

    fn position_window(&mut self) {
        let dpi = self.monitor.dpi_scale;
        let w_px = (self.width_dips * dpi).ceil().max(1.0) as i32;
        // C# expands the window HEIGHT from 50 to 246 DIP (not the width).
        let h_px = (policy::island_height_dips(self.island.expanded) * dpi)
            .ceil()
            .max(1.0) as i32;
        let x = self.monitor.left + (self.monitor.width() - w_px) / 2;
        let y = self.monitor.top;
        unsafe {
            SetWindowPos(
                self.hwnd as HWND,
                HWND_TOPMOST,
                x,
                y,
                w_px,
                h_px,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        let vp_left = LEFT_CONTROLS_DIPS * dpi;
        let vp_right = (self.width_dips - RIGHT_CONTROLS_DIPS) * dpi;
        DcompEngine::update_rounded_clip_rect(
            &self.viewport_clip,
            vp_left,
            0.0,
            vp_right,
            h_px as f32,
            0.0,
            island_geo::BOTTOM_RADIUS_DIPS * dpi,
        );
        if self.slide_anim.is_none() && !self.is_dragging {
            self.slide_offset_px = -(self.current_page as f32) * self.viewport_width_px();
        }
        let slide_h = (COMPACT_HEIGHT_DIPS + 6.0) * dpi.max(0.5);
        let offset_y = -(self.hide_progress * slide_h).round();
        if let Some(ref mut glass) = self.spatial_glass {
            glass.sync_rect(
                0.0,
                0.0,
                w_px as f32,
                h_px as f32,
                island_geo::BOTTOM_RADIUS_DIPS * dpi,
                dpi,
                0.0,
                offset_y,
            );
        }
    }

    fn render_all(&self) {
        let dpi = self.monitor.dpi_scale;
        let has_prev_next = self.page_ids.len() > 1;
        let count_text = if has_prev_next {
            page_count_text(self.current_page, self.page_ids.len())
        } else {
            String::new()
        };
        let has_expand = Self::has_expandable_content(self.current_page_id());
        let glass_host_active = self
            .spatial_glass
            .as_ref()
            .is_some_and(design::spatial_glass::SpatialGlassHost::is_enabled);
        if let Ok(surf) = self.dcomp.create_island_chrome_surface(
            self.width_dips,
            policy::island_height_dips(self.island.expanded),
            dpi,
            self.is_dark,
            self.settings.dynamic_island_opacity as f32,
            self.settings.dynamic_island_background,
            self.settings.dynamic_island_use_theme,
            has_prev_next,
            &count_text,
            has_expand,
            self.island.expanded,
            self.hovered_btn,
            self.pressed_btn,
            self.settings
                .widget_border_color
                .as_deref()
                .and_then(design::parse_hex_rgb)
                .unwrap_or(if self.is_dark {
                    (0xff, 0xff, 0xff)
                } else {
                    (0x18, 0x18, 0x1b)
                }),
            self.settings.widget_border_thickness.clamp(0.0, 8.0) as f32,
            glass_host_active,
        ) {
            self.chrome_node.set_content(Some(&surf));
        }

        let vp_left = LEFT_CONTROLS_DIPS * dpi;
        let vp_w_px = self.viewport_width_px();
        let chip_h_dips = CHIP_H_DIPS;
        let top_px = ((COMPACT_HEIGHT_DIPS - chip_h_dips) * 0.5) * dpi;

        for (idx, id) in self.page_ids.iter().enumerate() {
            let Some(node) = self.page_nodes.get(idx) else {
                continue;
            };
            let spec = self
                .widget_runtime
                .chip_spec(id, &self.settings, self.is_dark);
            let chip_w_dips = self
                .widget_runtime
                .widget_width_dips(id)
                .min(self.viewport_width_dips());
            let interaction = if idx == self.current_page && self.pressed_widget {
                WidgetInteraction::Pressed(self.hovered_widget_sub)
            } else if idx == self.current_page && self.hovered_widget_sub.is_some() {
                WidgetInteraction::Hovered(self.hovered_widget_sub)
            } else {
                WidgetInteraction::Idle
            };
            if let Ok(surf) = self.dcomp.create_widget_chip_surface(
                &spec,
                chip_w_dips,
                chip_h_dips,
                dpi,
                self.is_dark,
                interaction,
                self.settings
                    .widget_border_color
                    .as_deref()
                    .and_then(design::parse_hex_rgb),
                self.settings.widget_border_thickness.clamp(0.0, 8.0) as f32,
            ) {
                node.set_content(Some(&surf));
            }
            let chip_w_px = chip_w_dips * dpi;
            let x_px = vp_left
                + (idx as f32) * vp_w_px
                + (vp_w_px - chip_w_px) * 0.5
                + self.slide_offset_px;
            node.set_transform(1.0, 1.0, x_px.round(), top_px.round());
        }

        // C# `DetailsPanel Padding="14,2,14,12"` inside a window that grows from
        // 50 to 246 DIP. The details occupy the whole second row, not a 42 DIP
        // overlay squeezed into the header (the old port's geometry bug).
        if self.island.expanded {
            let details = self.island.details.clone().unwrap_or_else(|| {
                let mut fallback = WidgetDetails::new(
                    self.current_page_id().unwrap_or("widget"),
                    loc::t("WidgetDetails"),
                );
                fallback.lines.push(DetailLine::new(
                    loc::t("WidgetDetails"),
                    loc::t("NoDetailsAvailable"),
                ));
                fallback
            });
            let summary = details
                .lines
                .iter()
                .take(3)
                .map(|line| format!("{}: {}", line.label, line.value))
                .collect::<Vec<_>>()
                .join(" · ");
            let panel_w = (self.width_dips - island_geo::DETAILS_PADDING_X_DIPS * 2.0).max(1.0);
            let panel_h = (EXPANDED_HEIGHT_DIPS
                - COMPACT_HEIGHT_DIPS
                - island_geo::DETAILS_PADDING_TOP_DIPS
                - island_geo::DETAILS_PADDING_BOTTOM_DIPS)
                .max(1.0);
            let panel_x = island_geo::DETAILS_PADDING_X_DIPS * dpi;
            let panel_y = (COMPACT_HEIGHT_DIPS + island_geo::DETAILS_PADDING_TOP_DIPS) * dpi;
            let bg = self.dcomp.create_rounded_rect_surface(
                (panel_w * dpi).ceil() as u32,
                (panel_h * dpi).ceil() as u32,
                island_geo::CHIP_RADIUS_DIPS * dpi,
                if glass_host_active {
                    if self.is_dark {
                        (0xff, 0xff, 0xff, 0.08)
                    } else {
                        (0x00, 0x00, 0x00, 0.06)
                    }
                } else if self.is_dark {
                    (0x1b, 0x21, 0x2d, 0.97)
                } else {
                    (0xf2, 0xf4, 0xfa, 0.97)
                },
            );
            let text = self.dcomp.create_widget_text_surface(
                &details.title,
                Some(&summary),
                panel_w - 10.0,
                panel_h - 2.0,
                dpi,
                self.is_dark,
            );
            match (bg, text) {
                (Ok(bg), Ok(text)) => {
                    self.details_bg.set_content(Some(&bg));
                    self.details_bg
                        .set_transform(1.0, 1.0, panel_x.round(), panel_y.round());
                    self.details_node.set_content(Some(&text));
                    self.details_node.set_transform(
                        1.0,
                        1.0,
                        (panel_x + 5.0 * dpi).round(),
                        (panel_y + 1.0 * dpi).round(),
                    );
                }
                _ => {
                    self.details_bg.hide();
                    self.details_node.hide();
                }
            }
        } else if self.page_ids.is_empty() {
            self.details_bg.hide();
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
            } else {
                self.details_node.hide();
            }
        } else {
            self.details_bg.hide();
            self.details_node.hide();
        }
        self.dcomp.commit();
    }

    fn update_page_positions_only(&self) {
        let dpi = self.monitor.dpi_scale;
        let vp_left = LEFT_CONTROLS_DIPS * dpi;
        let vp_w_px = self.viewport_width_px();
        let top_px = ((COMPACT_HEIGHT_DIPS - 38.0) * 0.5) * dpi;
        for (idx, id) in self.page_ids.iter().enumerate() {
            let Some(node) = self.page_nodes.get(idx) else {
                continue;
            };
            let chip_w_px = self
                .widget_runtime
                .widget_width_dips(id)
                .min(self.viewport_width_dips())
                * dpi;
            let x_px = vp_left
                + (idx as f32) * vp_w_px
                + (vp_w_px - chip_w_px) * 0.5
                + self.slide_offset_px;
            node.set_transform(1.0, 1.0, x_px.round(), top_px.round());
        }
        self.dcomp.commit();
    }

    fn anim_interval_ms(&self) -> u32 {
        if !effective_animations_enabled(self.settings.animations_enabled, self.settings.focus_mode)
        {
            return 0;
        }
        let hz = win32::monitor_refresh_hz(&self.monitor.device_name);
        let fps = self
            .settings
            .animation_fps
            .map(|f| f.clamp(15, hz))
            .unwrap_or(hz);
        (1000 / fps.max(15)).max(4)
    }

    fn apply_hide_offset(&self) {
        let slide_h = (COMPACT_HEIGHT_DIPS + 6.0) * self.monitor.dpi_scale.max(0.5);
        let offset_y = -(self.hide_progress * slide_h).round();
        self.dcomp.set_root_offset_y(offset_y);
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
            }
        } else {
            self.hide_progress = (self.hide_progress + diff * k).clamp(0.0, 1.0);
            self.apply_hide_offset();
        }
    }

    fn set_current_page(&mut self, index: usize, animate: bool) {
        if self.page_ids.is_empty() {
            return;
        }
        let target = index.min(self.page_ids.len() - 1);
        self.current_page = target;
        self.island.page_index = target;
        self.island.details = None;
        self.island.details_requested = false;
        if self.island.expanded {
            self.request_details();
        }
        let target_offset = -(target as f32) * self.viewport_width_px();
        if animate
            && effective_animations_enabled(
                self.settings.animations_enabled,
                self.settings.focus_mode,
            )
        {
            self.slide_anim = Some(SlideAnim {
                from_px: self.slide_offset_px,
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
        self.width_dips = self.width_for_current_page();
        let glass_cfg = design::spatial_glass::SpatialGlassConfig::from_settings(
            &self.settings,
            self.settings.dynamic_island_background,
            self.settings.dynamic_island_opacity,
            self.is_dark,
            true,
        );
        if let Some(ref mut glass) = self.spatial_glass {
            glass.apply_config(glass_cfg);
        }
        self.position_window();
        self.render_all();
    }

    fn move_page(&mut self, delta: isize) {
        if self.page_ids.len() <= 1 {
            return;
        }
        let target = (self.current_page as isize + delta)
            .clamp(0, (self.page_ids.len() - 1) as isize) as usize;
        if target != self.current_page {
            self.set_current_page(target, true);
        }
    }

    /// In-place expand/collapse. The island window grows by the 246 DIP details
    /// panel (see `IslandState::current_width`); no separate flyout is opened.
    fn set_expanded(&mut self, expanded: bool) {
        if !self.island.set_expanded(expanded) {
            return;
        }
        if expanded {
            self.request_details();
        }
        self.width_dips = self.width_for_current_page();
        let glass_cfg = design::spatial_glass::SpatialGlassConfig::from_settings(
            &self.settings,
            self.settings.dynamic_island_background,
            self.settings.dynamic_island_opacity,
            self.is_dark,
            true,
        );
        if let Some(ref mut glass) = self.spatial_glass {
            glass.apply_config(glass_cfg);
        }
        self.position_window();
        self.render_all();
    }

    /// `TimedWidgetTile.DetailsRequested`: ask the shared `IslandState` for a
    /// one-shot payload from the live chip spec, so the panel always mirrors
    /// what the collapsed chip is showing.
    fn request_details(&mut self) {
        let Some(id) = self.current_page_id().map(str::to_owned) else {
            return;
        };
        let spec = self
            .widget_runtime
            .chip_spec(&id, &self.settings, self.is_dark);
        let lines = spec.details_lines();
        self.island.request_details(|widget_id| {
            let mut details = WidgetDetails::new(widget_id, widget_id);
            for (label, value) in lines {
                details.lines.push(DetailLine::new(label, value));
            }
            Some(details).filter(|details| !details.is_empty())
        });
    }

    fn tick(&mut self) {
        if !self.settings.dynamic_island_enable || self.page_ids.is_empty() {
            return;
        }
        self.widget_runtime.tick(&self.settings);
        self.render_all();
    }

    fn refresh_visibility(&mut self) {
        let fg_probe = win32::try_get_foreground_application_window();
        let fg_on_display = match fg_probe {
            ForegroundProbe::Window(hwnd) => win32::is_window_on_monitor(hwnd, self.monitor.handle),
            ForegroundProbe::DesktopOrEmpty | ForegroundProbe::OwnProcess => false,
        };
        // Both opt-ins suppress the island while a full-screen app owns the display.
        let suppress_fullscreen = self.settings.dynamic_island_hide_in_fullscreen
            || self.settings.dynamic_island_auto_hide;
        let fullscreen = suppress_fullscreen && win32::is_any_fullscreen_on_monitor(&self.monitor);

        let auto_hide_allows = if self.settings.dynamic_island_auto_hide {
            if self.is_dragging || self.pressed_widget || self.pressed_btn.is_some() {
                true
            } else {
                let mut pt = POINT { x: 0, y: 0 };
                if unsafe { GetCursorPos(&mut pt) } != 0 {
                    let dpi = self.monitor.dpi_scale.max(0.5);
                    let w_px = (self.width_dips * dpi).ceil() as i32;
                    let h_px =
                        (policy::island_height_dips(self.island.expanded) * dpi).ceil() as i32;
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

        let should_show = self.settings.dynamic_island_enable
            && !self.settings.focus_mode
            && auto_hide_allows
            && policy::should_show(
                self.settings.dynamic_island_visibility,
                !fg_on_display,
                suppress_fullscreen,
                fullscreen,
            );

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
            self.hide_progress = if next_hidden { 1.0 } else { 0.0 };
            self.apply_hide_offset();
            if should_show && !self.is_visible {
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
            } else if !should_show && self.is_visible {
                unsafe {
                    ShowWindow(self.hwnd as HWND, SW_HIDE);
                }
                self.is_visible = false;
            }
            return;
        }

        if should_show && !self.is_visible {
            self.hide_progress = 1.0;
            self.apply_hide_offset();
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

    fn hit_button(&self, mx_dips: f32, my_dips: f32) -> Option<IslandButton> {
        if !(9.0..=41.0).contains(&my_dips) {
            return None;
        }
        if (3.0..=35.0).contains(&mx_dips) {
            return (self.page_ids.len() > 1).then_some(IslandButton::Prev);
        }
        let next_x = self.width_dips - RIGHT_CONTROLS_DIPS + 3.0;
        if (next_x..=next_x + 32.0).contains(&mx_dips) {
            return (self.page_ids.len() > 1).then_some(IslandButton::Next);
        }
        let expand_x = self.width_dips - 35.0;
        if Self::has_expandable_content(self.current_page_id())
            && (expand_x..=expand_x + 32.0).contains(&mx_dips)
        {
            return Some(IslandButton::Expand);
        }
        None
    }

    fn current_chip_rect_dips(&self) -> Option<(f32, f32, f32, f32)> {
        let id = self.current_page_id()?;
        let vp_w = self.viewport_width_dips();
        let chip_w = self.widget_runtime.widget_width_dips(id).min(vp_w);
        let left = LEFT_CONTROLS_DIPS + (vp_w - chip_w) * 0.5;
        let top = (COMPACT_HEIGHT_DIPS - 38.0) * 0.5;
        Some((left, top, left + chip_w, top + 38.0))
    }

    fn open_current_flyout(&mut self) {
        let Some(wid) = self.current_page_id().map(str::to_owned) else {
            return;
        };
        if !has_real_widget_flyout(&wid) {
            return;
        }
        let dpi = self.monitor.dpi_scale;
        let island_bottom_y = self.monitor.top
            + (policy::island_height_dips(self.island.expanded) * dpi).ceil() as i32;
        let (anchor_x, anchor_y) = island_flyout_anchor(
            self.monitor.left,
            self.monitor.width(),
            island_bottom_y,
            dpi,
        );
        // Dispatches to the real `widgets::WidgetRuntime::open_widget_flyout`
        // implementation (calendar, mixer, stats, AI quotas, ...). The old port
        // only toggled the in-place details panel, so no flyout ever appeared.
        let changed = self.widget_runtime.open_widget_flyout(
            &wid,
            self.hwnd,
            self.is_dark,
            anchor_x,
            anchor_y,
            &mut self.settings,
        );
        if changed {
            self.save_and_broadcast();
        } else {
            self.render_all();
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

        let dpi = self.monitor.dpi_scale;
        let mx = mx_px / dpi;
        let my = my_px / dpi;
        if let Some(ref mut glass) = self.spatial_glass {
            glass.set_pointer(mx_px, my_px);
        }

        if self.is_dragging {
            let dx_px = mx_px - self.drag_start_x;
            let vp_w_px = self.viewport_width_px();
            let base = -(self.drag_start_page as f32) * vp_w_px;
            let at_edge = (self.drag_start_page == 0 && dx_px > 0.0)
                || (self.drag_start_page + 1 >= self.page_ids.len() && dx_px < 0.0);
            let resisted = if at_edge { dx_px * 0.22 } else { dx_px };
            self.slide_offset_px = base + resisted.clamp(-vp_w_px, vp_w_px);
            self.update_page_positions_only();
            return;
        }

        if self.pressed_btn.is_none()
            && !self.pressed_widget
            && self.page_ids.len() > 1
            && (mx_px - self.drag_start_x).abs() > 6.0 * dpi
            && self.drag_start_x > 0.0
        {
            self.is_dragging = true;
            self.slide_anim = None;
            return;
        }

        let next_btn = self.hit_button(mx, my);
        if next_btn != self.hovered_btn {
            self.hovered_btn = next_btn;
            self.render_all();
        }
    }

    fn handle_lbutton_down(&mut self, mx_px: f32, my_px: f32) {
        let dpi = self.monitor.dpi_scale;
        let mx = mx_px / dpi;
        let my = my_px / dpi;
        if let Some(ref mut glass) = self.spatial_glass {
            glass.set_pointer(mx_px, my_px);
        }
        unsafe {
            SetCapture(self.hwnd as HWND);
        }
        if let Some(btn) = self.hit_button(mx, my) {
            self.pressed_btn = Some(btn);
            self.drag_start_x = 0.0;
            self.render_all();
            return;
        }
        self.drag_start_x = mx_px;
        self.drag_start_page = self.current_page;
        if let Some((l, t, r, b)) = self.current_chip_rect_dips()
            && mx >= l
            && mx <= r
            && my >= t
            && my <= b
        {
            self.pressed_widget = true;
            self.render_all();
        }
    }

    fn handle_lbutton_up(&mut self, mx_px: f32, my_px: f32) {
        unsafe {
            ReleaseCapture();
        }
        let dpi = self.monitor.dpi_scale;
        let mx = mx_px / dpi;
        let my = my_px / dpi;
        if let Some(ref mut glass) = self.spatial_glass {
            glass.set_pointer(mx_px, my_px);
        }

        if self.is_dragging {
            self.is_dragging = false;
            let dx_px = mx_px - self.drag_start_x;
            let threshold = (36.0 * dpi).max(self.viewport_width_px() * 0.18);
            self.drag_start_x = 0.0;
            self.pressed_widget = false;
            let target = if dx_px <= -threshold {
                (self.drag_start_page + 1).min(self.page_ids.len().saturating_sub(1))
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
                let wid = self.current_page_id().unwrap_or("").to_owned();
                match island_button_click_action(btn, &wid) {
                    IslandClickAction::PrevPage => self.move_page(-1),
                    IslandClickAction::NextPage => self.move_page(1),
                    IslandClickAction::OpenFlyout => self.open_current_flyout(),
                    IslandClickAction::ToggleDetails => {
                        let expanded = self.island.expanded;
                        self.set_expanded(!expanded);
                    }
                }
            }
            return;
        }

        if self.page_ids.is_empty() {
            settings_ui::spawn_settings_window();
            return;
        }

        if self.pressed_widget {
            self.pressed_widget = false;
            self.render_all();
            if let Some(wid) = self.current_page_id().map(str::to_owned)
                && let Some((l, t, r, b)) = self.current_chip_rect_dips()
                && mx >= l
                && mx <= r
                && my >= t
                && my <= b
            {
                let rel_x = mx - l;
                let width = (r - l).max(1.0);
                match self.widget_runtime.handle_direct_click(
                    &wid,
                    rel_x,
                    width,
                    &mut self.settings,
                ) {
                    widgets::DirectClickOutcome::SettingsChanged => {
                        self.save_and_broadcast();
                    }
                    widgets::DirectClickOutcome::Handled => {
                        // AI chips and a few others return Handled even when a
                        // full widget flyout exists. The island must still open
                        // it on a chip click (C# island never showed a dead end).
                        self.render_all();
                        if has_real_widget_flyout(&wid) {
                            self.open_current_flyout();
                        }
                    }
                    widgets::DirectClickOutcome::OpenFlyout => {
                        self.open_current_flyout();
                    }
                }
            }
        }
    }

    fn handle_rbutton_up(&mut self) {
        let Some(wid) = self.current_page_id().map(str::to_owned) else {
            settings_ui::spawn_settings_window();
            return;
        };
        let reset_label = loc::t("Reset");
        let sound_label = loc::t_fmt("RadioSettings", &[&loc::t("LauncherSound")]);
        let focus_label = loc::t("FocusMode");
        let secondary_label = match wid.as_str() {
            "stopwatch" | "countdown" | "focus" => Some(reset_label.as_str()),
            "audio" => Some(sound_label.as_str()),
            "quicksettings" => Some(focus_label.as_str()),
            _ => None,
        };
        let can_earlier = self.current_page > 0;
        let can_later = self.current_page + 1 < self.page_ids.len();

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
        ) {
            WidgetContextAction::PlaceAllDisplays => {
                self.settings
                    .dynamic_island_placements
                    .insert(wid, settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned());
                self.save_and_broadcast();
            }
            WidgetContextAction::PlaceOnDisplay(idx) => {
                if let Some(mon) = self.all_monitors.get(idx) {
                    self.settings
                        .dynamic_island_placements
                        .insert(wid, mon.device_name.clone());
                    self.save_and_broadcast();
                }
            }
            WidgetContextAction::MoveToTaskbar => {
                self.settings
                    .dynamic_island_placements
                    .retain(|k, _| !k.eq_ignore_ascii_case(&wid));
                self.save_and_broadcast();
            }
            WidgetContextAction::MoveEarlier => self.move_widget_order(&wid, -1),
            WidgetContextAction::MoveLater => self.move_widget_order(&wid, 1),
            WidgetContextAction::Remove => {
                self.settings.set_widget_enabled(&wid, false);
                self.save_and_broadcast();
            }
            WidgetContextAction::Secondary => {
                if let Some(outcome) = self
                    .widget_runtime
                    .handle_secondary_click(&wid, &mut self.settings)
                {
                    if matches!(outcome, widgets::DirectClickOutcome::SettingsChanged) {
                        self.save_and_broadcast();
                    } else {
                        self.render_all();
                    }
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

    fn destroy(self) {
        unsafe {
            KillTimer(self.hwnd as HWND, TIMER_VISIBILITY);
            KillTimer(self.hwnd as HWND, TIMER_WIDGETS);
            KillTimer(self.hwnd as HWND, TIMER_ANIM);
            KillTimer(self.hwnd as HWND, TIMER_HIDE_ANIM);
            SetWindowLongPtrW(self.hwnd as HWND, GWLP_USERDATA, 0);
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
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut DynamicIslandWindow;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_NCHITTEST => HTCLIENT as isize,
        WM_SETCURSOR => {
            if state.hovered_btn.is_some() {
                unsafe {
                    SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND));
                }
                return 1;
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_MOUSEMOVE => {
            let mx = loword_i16(lparam);
            let my = hiword_i16(lparam);
            if let Some(ref mut glass) = state.spatial_glass {
                glass.set_pointer(mx, my);
            }
            state.handle_mouse_move(mx, my);
            0
        }
        WM_MOUSELEAVE => {
            if let Some(ref mut glass) = state.spatial_glass {
                glass.clear_pointer();
            }
            state.tracking_mouse = false;
            if state.hovered_btn.is_some() {
                state.hovered_btn = None;
                state.render_all();
            }
            if state.settings.dynamic_island_auto_hide {
                state.refresh_visibility();
            }
            0
        }
        WM_LBUTTONDOWN => {
            state.handle_lbutton_down(loword_i16(lparam), hiword_i16(lparam));
            0
        }
        WM_LBUTTONUP => {
            state.handle_lbutton_up(loword_i16(lparam), hiword_i16(lparam));
            0
        }
        WM_RBUTTONUP => {
            state.handle_rbutton_up();
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam >> 16) & 0xffff) as i16;
            if let Some(wid) = state.current_page_id()
                && (wid == "audio" || wid == "media")
            {
                widgets::adjust_audio_volume(if delta > 0 { 4 } else { -4 });
                state.widget_runtime.tick(&state.settings);
                state.render_all();
            }
            0
        }
        WM_KEYDOWN => {
            let vk = wparam as u16;
            if vk == VK_ESCAPE {
                // Escape collapses the in-place details panel (shared state machine).
                if state.island.pre_process_input(IslandInput::Escape) == InputDisposition::Consumed
                {
                    state.width_dips = state.width_for_current_page();
                    state.position_window();
                    state.render_all();
                }
            } else if vk == VK_LEFT {
                state.move_page(-1);
            } else if vk == VK_RIGHT {
                state.move_page(1);
            }
            0
        }
        WM_TIMER => {
            match wparam {
                TIMER_VISIBILITY => state.refresh_visibility(),
                TIMER_WIDGETS => state.tick(),
                TIMER_HIDE_ANIM => state.step_hide_animation(),
                TIMER_ANIM => {
                    if let Some(anim) = &state.slide_anim {
                        let t = anim.start.elapsed().as_secs_f32() / SLIDE_DURATION_SECS;
                        if t >= 1.0 {
                            state.slide_offset_px = anim.to_px;
                            state.slide_anim = None;
                            unsafe {
                                KillTimer(hwnd, TIMER_ANIM);
                            }
                        } else {
                            let e = ease_out_cubic(t);
                            state.slide_offset_px = anim.from_px + (anim.to_px - anim.from_px) * e;
                        }
                        state.update_page_positions_only();
                    } else {
                        unsafe {
                            KillTimer(hwnd, TIMER_ANIM);
                        }
                    }
                }
                _ => {}
            }
            0
        }
        WM_DESTROY => 0,
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ---- WS-D expanded-details policy / state machine ------------------------------
// C# `DynamicIslandPolicy`, `TimedWidgetTile.DetailsRequested`, `GetDetails()`.
// Kept alongside the native Win32 island host above so both renderers share one API.
/// C# `DynamicIslandVisibilityMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandVisibility {
    Never,
    Always,
    DesktopOnly,
}

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

/// Per-widget details provider result (C# `GetDetails()` on TimedWidgetTile / Clock / etc.).
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

/// `WidthForCurrentPage`: C# grows the island *height* to 246 DIP and picks an
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

/// C# `DynamicIslandPolicy.ClampWindowWidth`.
pub fn clamp_window_width(
    preferred: f32,
    monitor_width: f32,
    requested_minimum: f32,
    requested_maximum: f32,
    edge_inset: f32,
) -> f32 {
    if !monitor_width.is_finite() || monitor_width <= 0.0 {
        return if preferred.is_finite() {
            preferred.clamp(240.0, 960.0)
        } else {
            240.0
        };
    }
    let available = (monitor_width - edge_inset.max(0.0) * 2.0).max(240.0);
    let maximum = requested_maximum.max(240.0).min(available);
    let minimum = requested_minimum.max(240.0).min(maximum);
    if preferred.is_finite() {
        preferred.clamp(minimum, maximum)
    } else {
        minimum
    }
}

pub fn should_show(
    visibility: IslandVisibility,
    desktop_visible: bool,
    hide_in_fullscreen: bool,
    fullscreen: bool,
) -> bool {
    !(hide_in_fullscreen && fullscreen)
        && match visibility {
            IslandVisibility::Always => true,
            IslandVisibility::DesktopOnly => desktop_visible,
            IslandVisibility::Never => false,
        }
}

/// C# `DynamicIslandPolicy.OrderWidgets(... timedOrder)` ΓÇö the Rust signature omitted
/// `timedOrder`; this helper restores it.
pub fn order_widgets(
    available: &[String],
    display_order: &[String],
    widget_order: &[String],
    default_order: &[String],
    timed_order: &[String],
) -> Vec<String> {
    let available: HashSet<String> = available.iter().map(|w| w.to_ascii_lowercase()).collect();
    let mut ordered: Vec<String> = Vec::new();
    for candidate in display_order
        .iter()
        .chain(widget_order)
        .chain(default_order)
        .chain(timed_order)
    {
        let key = candidate.to_ascii_lowercase();
        if available.contains(&key) && !ordered.iter().any(|w| w.eq_ignore_ascii_case(candidate)) {
            ordered.push(candidate.clone());
        }
    }
    ordered
}

pub fn move_widget(order: &[String], widget_id: &str, offset: isize) -> Vec<String> {
    if offset != -1 && offset != 1 {
        return order.to_vec();
    }
    let Some(index) = order.iter().position(|w| w.eq_ignore_ascii_case(widget_id)) else {
        return order.to_vec();
    };
    let target = index as isize + offset;
    if target < 0 || target >= order.len() as isize {
        return order.to_vec();
    }
    let mut moved = order.to_vec();
    moved.swap(index, target as usize);
    moved
}

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

    /// C# `SetExpanded()`.
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
        let t = super::layout::cubic_ease_out(self.animation_progress(now));
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

    fn pages() -> Vec<String> {
        ["clock", "focus", "media"].map(str::to_owned).to_vec()
    }

    #[test]
    fn csharp_246_expand_is_vertical_and_width_is_content_driven() {
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
        assert_eq!(clamp_window_width(500.0, 1920.0, 240.0, 960.0, 16.0), 500.0);
        assert_eq!(
            clamp_window_width(2000.0, 1024.0, 240.0, 960.0, 16.0),
            960.0
        );
        assert_eq!(clamp_window_width(f32::NAN, 0.0, 240.0, 960.0, 16.0), 240.0);
    }

    #[test]
    fn visibility_modes() {
        assert!(should_show(IslandVisibility::Always, false, true, false));
        assert!(!should_show(IslandVisibility::Always, true, true, true));
        assert!(should_show(
            IslandVisibility::DesktopOnly,
            true,
            false,
            true
        ));
        assert!(!should_show(IslandVisibility::Never, true, false, false));
    }

    #[test]
    fn order_widgets_includes_timed_order_and_deduplicates() {
        let available = ["clock", "focus", "media"].map(str::to_owned).to_vec();
        let order = order_widgets(
            &available,
            &["media".into()],
            &["clock".into(), "hidden".into()],
            &["focus".into(), "clock".into()],
            &["focus".into()],
        );
        assert_eq!(order, ["media", "clock", "focus"]);
    }

    #[test]
    fn move_widget_only_swaps_one_step() {
        let order = ["a", "b", "c"].map(str::to_owned).to_vec();
        assert_eq!(move_widget(&order, "b", -1), ["b", "a", "c"]);
        assert_eq!(move_widget(&order, "a", -1), order);
        assert_eq!(move_widget(&order, "a", 2), order);
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

    /// WS-C: no island click may dead-end. Prev/Next page, Expand opens a *real*
    /// widget flyout when the widget has one, otherwise the C# in-place panel.
    #[test]
    fn island_click_dispatch_table() {
        assert_eq!(
            island_button_click_action(IslandButton::Prev, "clock"),
            IslandClickAction::PrevPage
        );
        assert_eq!(
            island_button_click_action(IslandButton::Next, "clock"),
            IslandClickAction::NextPage
        );
        for id in [
            "clock",
            "stats",
            "media",
            "aiusage_claude",
            "audio",
            "timer",
        ] {
            assert_eq!(
                island_button_click_action(IslandButton::Expand, id),
                IslandClickAction::ToggleDetails,
                "Expand chevron on {id} must toggle in-place Dynamic Island expansion"
            );
        }
        assert!(has_real_widget_flyout("Clock")); // case-insensitive
        assert!(!has_real_widget_flyout("unknown"));
    }

    #[test]
    fn island_flyout_anchor_is_below_the_island_and_centered() {
        // Top-edge monitor at x=1920..3840, DPI=1.5, island bottom at y=75.
        let (x, y) = island_flyout_anchor(1920, 1920, 75, 1.5);
        assert_eq!(x, 2880);
        assert_eq!(y, 75 + 369); // 246 DIP * 1.5
    }

    #[test]
    fn expandable_widget_set_matches_csharp() {
        for id in [
            "clock",
            "focus",
            "stats",
            "weather",
            "media",
            "audio",
            "netspeed",
            "bluetoothbattery",
            "stopwatch",
            "countdown",
            "aiusage_claude",
            "AiUsageAll",
        ] {
            assert!(
                DynamicIslandWindow::has_expandable_content(Some(id)),
                "{id} must have C# expandable content"
            );
        }
        for id in ["battery", "storage", "keyboard", "quicksettings"] {
            assert!(!DynamicIslandWindow::has_expandable_content(Some(id)));
        }
        assert!(!DynamicIslandWindow::has_expandable_content(None));
    }

    #[test]
    fn details_width_stays_within_csharp_measure_budget() {
        let details = WidgetDetails::new("clock", "Date and time")
            .line("Timezone", "Europe/Istanbul")
            .line("Format", "24h");
        let width = details_desired_width(&details);
        assert!(width > 0.0 && width <= 640.0);
        assert_eq!(details_desired_width(&WidgetDetails::new("x", "")), 0.0);
    }
}
