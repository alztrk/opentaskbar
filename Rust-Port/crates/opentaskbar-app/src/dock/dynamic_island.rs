//! Native Win32 + DirectComposition Dynamic Island window & multi-display manager.
//!
//! Mirrors C# `Views/Windows/DynamicIslandManager.cs` and `DynamicIslandWindow.xaml.cs`:
//! - One top-edge floating island per connected display with assigned widgets
//! - Horizontal paging via Prev/Next buttons, pointer drag/swipe, or Left/Right keys when hovered
//! - Per-display widget ordering (`DynamicIslandWidgetOrders`), placement context menus,
//!   desktop-only visibility mode, and fullscreen app suppression.

use std::collections::HashMap;
use std::time::Instant;

use opentaskbar_core::dynamic_island as policy;
use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::{self, DockSettings};
use windows::Win32::Graphics::DirectComposition::IDCompositionRectangleClip;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_LEFT, VK_RIGHT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, HTCLIENT,
    HWND_TOPMOST, IDC_ARROW, IDC_HAND, KillTimer, LoadCursorW, MA_NOACTIVATE, RegisterClassExW,
    RegisterWindowMessageW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_DESTROY, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST,
    WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::dcomp::{DcompEngine, IslandButton, VisualNode, WidgetInteraction};
use crate::design;
use crate::platform::win32::{self, ForegroundProbe, MonitorInfo, WidgetContextAction};
use crate::ui::settings as settings_ui;
use crate::widgets::{self, WidgetRuntime};

const COMPACT_HEIGHT_DIPS: f32 = 50.0;
const COMPACT_MIN_WIDTH_DIPS: f32 = 360.0;
const MAX_WIDTH_DIPS: f32 = 960.0;
const RIGHT_CONTROLS_DIPS: f32 = 38.0 + 56.0 + 38.0;
const LEFT_CONTROLS_DIPS: f32 = 38.0;
const SLIDE_DURATION_SECS: f32 = 0.190;
const WM_MOUSELEAVE: u32 = 0x02A3;

const TIMER_VISIBILITY: usize = 1;
const TIMER_WIDGETS: usize = 2;
const TIMER_ANIM: usize = 3;

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
    "aiusage_all",
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
    widget_runtime: WidgetRuntime,
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
        dcomp.add_child(&dcomp.root, &chrome_node);
        dcomp.add_child(&dcomp.root, &viewport_node);

        let mut widget_runtime = WidgetRuntime::default();
        widget_runtime.tick(settings);

        let mut boxed = Box::new(Self {
            hwnd: hwnd_val,
            monitor,
            all_monitors: all_monitors.to_vec(),
            settings: settings.clone(),
            is_dark,
            reload_msg,
            dcomp,
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
            widget_runtime,
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
        let ordered = policy::order_widgets(&widget_ids, display_order, None, DEFAULT_WIDGET_ORDER);

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

        self.width_dips = self.width_for_current_page();
        self.position_window();
        self.render_all();
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

    fn has_expandable_content(id: Option<&str>) -> bool {
        let Some(id) = id else {
            return false;
        };
        !id.eq_ignore_ascii_case("aiusage_all") && !id.starts_with("aiusage_")
    }

    fn width_for_current_page(&self) -> f32 {
        let widget_w = self
            .current_page_id()
            .map(|id| self.widget_runtime.widget_width_dips(id))
            .unwrap_or(240.0);
        let preferred = widget_w + LEFT_CONTROLS_DIPS + RIGHT_CONTROLS_DIPS + 16.0;
        let monitor_dips = self.monitor.width() as f32 / self.monitor.dpi_scale.max(0.5);
        policy::clamp_window_width(
            preferred,
            monitor_dips,
            COMPACT_MIN_WIDTH_DIPS,
            MAX_WIDTH_DIPS,
            16.0,
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
        let h_px = (COMPACT_HEIGHT_DIPS * dpi).ceil().max(1.0) as i32;
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
        DcompEngine::update_clip_rect(&self.viewport_clip, vp_left, 0.0, vp_right, h_px as f32);
        if self.slide_anim.is_none() && !self.is_dragging {
            self.slide_offset_px = -(self.current_page as f32) * self.viewport_width_px();
        }
    }

    fn render_all(&self) {
        let dpi = self.monitor.dpi_scale;
        let has_prev_next = self.page_ids.len() > 1;
        let count_text = if has_prev_next {
            format!("{:02} / {:02}", self.current_page + 1, self.page_ids.len())
        } else {
            String::new()
        };
        let has_expand = Self::has_expandable_content(self.current_page_id());
        if let Ok(surf) = self.dcomp.create_island_chrome_surface(
            self.width_dips,
            COMPACT_HEIGHT_DIPS,
            dpi,
            self.is_dark,
            self.settings.dynamic_island_opacity as f32,
            self.settings.dynamic_island_background,
            self.settings.dynamic_island_use_theme,
            has_prev_next,
            &count_text,
            has_expand,
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
        ) {
            self.chrome_node.set_content(Some(&surf));
        }

        let vp_left = LEFT_CONTROLS_DIPS * dpi;
        let vp_w_px = self.viewport_width_px();
        let chip_h_dips = 38.0;
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

    fn set_current_page(&mut self, index: usize, animate: bool) {
        if self.page_ids.is_empty() {
            return;
        }
        let target = index.min(self.page_ids.len() - 1);
        self.current_page = target;
        let target_offset = -(target as f32) * self.viewport_width_px();
        if animate && self.settings.animations_enabled && !self.settings.focus_mode {
            self.slide_anim = Some(SlideAnim {
                from_px: self.slide_offset_px,
                to_px: target_offset,
                start: Instant::now(),
            });
            unsafe {
                SetTimer(self.hwnd as HWND, TIMER_ANIM, 16, None);
            }
        } else {
            self.slide_anim = None;
            self.slide_offset_px = target_offset;
        }
        self.width_dips = self.width_for_current_page();
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

        let should_show = self.settings.dynamic_island_enable
            && !self.page_ids.is_empty()
            && !self.settings.focus_mode
            && policy::should_show(
                self.settings.dynamic_island_visibility,
                !fg_on_display,
                suppress_fullscreen,
                fullscreen,
            );

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
        let dpi = self.monitor.dpi_scale;
        let w_px = (self.width_dips * dpi).ceil() as i32;
        let h_px = (COMPACT_HEIGHT_DIPS * dpi).ceil() as i32;
        let anchor_x = self.monitor.left + self.monitor.width() / 2;
        let anchor_y = self.monitor.top + h_px + (6.0 * dpi).round() as i32;
        let _ = w_px;
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
                match btn {
                    IslandButton::Prev => self.move_page(-1),
                    IslandButton::Next => self.move_page(1),
                    IslandButton::Expand => self.open_current_flyout(),
                }
            }
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
                        self.render_all();
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
            state.handle_mouse_move(loword_i16(lparam), hiword_i16(lparam));
            0
        }
        WM_MOUSELEAVE => {
            state.tracking_mouse = false;
            if state.hovered_btn.is_some() {
                state.hovered_btn = None;
                state.render_all();
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
            if vk == VK_LEFT {
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
