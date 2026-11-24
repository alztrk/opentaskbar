//! GPUI floating dock window:
//! - Left native Windows shell buttons (Start, Search, Task View) with crisp embedded SVGs
//! - Stable slot ordering: unpinned running apps never jump positions when clicked/focused
//! - Continuous mouse-X cosine fisheye magnification anchored to resting centers (zero jitter,
//!   zero icon overlap, smooth neighbor parting inside a fixed-size transparent window)
//! - Tactile press squish + damped launch/activation bounce animation
//! - Automatic work-area calculation (5px top + 5px bottom), tooltips, and native context menu

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Application, AssetSource, Bounds, Context, MouseButton, RenderImage, SharedString, Timer,
    Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
    div, hsla, img, point, prelude::*, px, rgba, size, svg,
};
use opentaskbar_core::settings::{AutoHideMode, DockPosition, DockSettings, SurfaceBackground};
use opentaskbar_shell::tracker::{self, RunningWindow};

use crate::icon_cache;
use crate::pins::{self, PinnedApp};
use crate::win32::{self, ContextAction};

const VERTICAL_EDGE_GAP: f32 = 5.0;
const TOOLTIP_HEADROOM: f32 = 42.0;
const MAG_HEADROOM: f32 = 34.0;
const FISHEYE_PEAK: f32 = 0.42;
const FISHEYE_RADIUS_SLOTS: f32 = 2.4;
const BOUNCE_DURATION_SECS: f32 = 0.42;
const DRAG_THRESHOLD_PX: f32 = 6.0;

const SVG_START: &[u8] =
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
  <rect x="2.5" y="2.5" width="8.7" height="8.7" rx="1.6"/>
  <rect x="12.8" y="2.5" width="8.7" height="8.7" rx="1.6"/>
  <rect x="2.5" y="12.8" width="8.7" height="8.7" rx="1.6"/>
  <rect x="12.8" y="12.8" width="8.7" height="8.7" rx="1.6"/>
</svg>"##;

const SVG_SEARCH: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
  <circle cx="11" cy="11" r="7.5"/>
  <path d="M20.5 20.5l-4.2-4.2"/>
</svg>"##;

const SVG_TASKVIEW: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
  <rect x="2.5" y="4" width="13.5" height="10.5" rx="2.2"/>
  <path d="M8.5 18.5h10.8a2.2 2.2 0 0 0 2.2-2.2V8.5"/>
</svg>"##;

struct EmbeddedAssets;

impl AssetSource for EmbeddedAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes = match path {
            "icons/start.svg" => Some(Cow::Borrowed(SVG_START)),
            "icons/search.svg" => Some(Cow::Borrowed(SVG_SEARCH)),
            "icons/taskview.svg" => Some(Cow::Borrowed(SVG_TASKVIEW)),
            _ => None,
        };
        Ok(bytes)
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![
            "icons/start.svg".into(),
            "icons/search.svg".into(),
            "icons/taskview.svg".into(),
        ])
    }
}

fn shell_section_width(base: f32) -> f32 {
    base * 3.0 + 4.0 * 2.0 + 17.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellButton {
    Start,
    Search,
    TaskView,
}

impl ShellButton {
    fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Search => "Search",
            Self::TaskView => "Task View",
        }
    }
}

#[derive(Clone)]
struct DockSlot {
    key: String,
    pinned: bool,
    label: SharedString,
    launch_path: String,
    icon_source: String,
    hwnds: Vec<usize>,
    is_foreground: bool,
    scale: f32,
    /// Smooth horizontal spring offset (DIPs) when neighbors slide aside during drag reorder.
    slide_x: f32,
    bounce_started: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct DragState {
    idx: usize,
    start_x: f32,
    current_x: f32,
    grab_offset_x: f32,
    active: bool,
}

struct DockView {
    hwnd: usize,
    settings: DockSettings,
    pins: Vec<PinnedApp>,
    slots: Vec<DockSlot>,
    icons: HashMap<String, Option<Arc<RenderImage>>>,
    /// Continuous mouse X coordinate inside the window (in logical DIPs) while hovering the app strip.
    hover_mouse_x: Option<f32>,
    hovered_idx: Option<usize>,
    hovered_shell: Option<ShellButton>,
    pressed_idx: Option<usize>,
    pressed_shell: Option<ShellButton>,
    drag: Option<DragState>,
    suppress_next_click: bool,
    last_frame: Option<Instant>,
    last_window_rect: Option<(i32, i32, i32, i32)>,
}

fn exe_display_name(exe_path: &str, fallback_title: &str) -> String {
    Path::new(exe_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => fallback_title.to_owned(),
            }
        })
        .unwrap_or_else(|| fallback_title.to_owned())
}

/// Builds a single unified slot list where pinned and unpinned apps live together and
/// preserve their exact user-ordered positions across refreshes.
fn build_slots(pins: &[PinnedApp], running: &[RunningWindow], prev: &[DockSlot]) -> Vec<DockSlot> {
    let mut by_exe: HashMap<String, Vec<&RunningWindow>> = HashMap::new();
    for win in running {
        by_exe
            .entry(win.exe_path.to_lowercase())
            .or_default()
            .push(win);
    }

    // Resolve running windows matched to each pin.
    let mut pin_matches: HashMap<String, (Vec<usize>, bool, Option<String>)> = HashMap::new();
    let mut claimed_exes: HashMap<String, bool> = HashMap::new();
    for pin in pins {
        let matched_key = pin
            .target_exe
            .as_ref()
            .map(|p| p.to_lowercase())
            .filter(|k| by_exe.contains_key(k))
            .or_else(|| {
                let stem = pin.name.to_lowercase();
                by_exe
                    .keys()
                    .find(|k| {
                        Path::new(k)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|s| s.eq_ignore_ascii_case(&stem))
                    })
                    .cloned()
            });
        let (hwnds, is_fg) = match matched_key {
            Some(ref key) => {
                claimed_exes.insert(key.clone(), true);
                let list = &by_exe[key];
                (
                    list.iter().map(|w| w.hwnd).collect(),
                    list.iter().any(|w| w.is_foreground),
                )
            }
            None => (Vec::new(), false),
        };
        pin_matches.insert(pin.launch_path.to_lowercase(), (hwnds, is_fg, matched_key));
    }

    let mut next: Vec<DockSlot> = Vec::with_capacity(pins.len() + by_exe.len());
    let mut emitted_keys: HashMap<String, bool> = HashMap::new();

    // 1. Walk existing unified order (`prev`) so dragged pinned & unpinned apps stay exactly where placed.
    for old in prev {
        if old.pinned {
            if let Some(pin) = pins
                .iter()
                .find(|p| p.launch_path.eq_ignore_ascii_case(&old.key))
            {
                let key = pin.launch_path.to_lowercase();
                let (hwnds, is_foreground, _) = pin_matches.get(&key).cloned().unwrap_or_default();
                emitted_keys.insert(key.clone(), true);
                next.push(DockSlot {
                    key,
                    pinned: true,
                    label: pin.name.clone().into(),
                    launch_path: pin.launch_path.clone(),
                    icon_source: pin.icon_source.clone(),
                    hwnds,
                    is_foreground,
                    scale: old.scale,
                    slide_x: old.slide_x,
                    bounce_started: old.bounce_started,
                });
            }
        } else if !claimed_exes.contains_key(&old.key)
            && let Some(list) = by_exe.get(&old.key)
        {
            emitted_keys.insert(old.key.clone(), true);
            next.push(DockSlot {
                key: old.key.clone(),
                pinned: false,
                label: old.label.clone(),
                launch_path: old.launch_path.clone(),
                icon_source: old.icon_source.clone(),
                hwnds: list.iter().map(|w| w.hwnd).collect(),
                is_foreground: list.iter().any(|w| w.is_foreground),
                scale: old.scale,
                slide_x: old.slide_x,
                bounce_started: old.bounce_started,
            });
        }
    }

    // 2. Append any pins not yet in `prev` (initial startup or newly pinned).
    for pin in pins {
        let key = pin.launch_path.to_lowercase();
        if emitted_keys.contains_key(&key) {
            continue;
        }
        let (hwnds, is_foreground, _) = pin_matches.get(&key).cloned().unwrap_or_default();
        emitted_keys.insert(key.clone(), true);
        next.push(DockSlot {
            key,
            pinned: true,
            label: pin.name.clone().into(),
            launch_path: pin.launch_path.clone(),
            icon_source: pin.icon_source.clone(),
            hwnds,
            is_foreground,
            scale: 1.0,
            slide_x: 0.0,
            bounce_started: None,
        });
    }

    // 3. Append newly launched unpinned apps at the end.
    for win in running {
        let key = win.exe_path.to_lowercase();
        if claimed_exes.contains_key(&key) || emitted_keys.contains_key(&key) {
            continue;
        }
        emitted_keys.insert(key.clone(), true);
        let list = &by_exe[&key];
        let label = exe_display_name(&win.exe_path, &win.title);
        let icon_source = pins::resolve_windowsapps_aumid(&win.exe_path)
            .map(|id| format!(r"shell:AppsFolder\{id}"))
            .unwrap_or_else(|| win.exe_path.clone());
        next.push(DockSlot {
            key,
            pinned: false,
            label: label.into(),
            launch_path: win.exe_path.clone(),
            icon_source,
            hwnds: list.iter().map(|w| w.hwnd).collect(),
            is_foreground: list.iter().any(|w| w.is_foreground),
            scale: 1.0,
            slide_x: 0.0,
            bounce_started: None,
        });
    }

    next
}

fn pill_height_dips(settings: &DockSettings) -> f32 {
    settings.icon_dips() + 22.0
}

fn slot_gap(settings: &DockSettings) -> f32 {
    9.0 + settings.dock_icon_padding.max(0) as f32
}

/// Computes the fixed resting pill width (used as anchor for continuous fisheye math).
fn resting_pill_width(settings: &DockSettings, slot_count: usize) -> f32 {
    let base = settings.icon_dips();
    let gap = slot_gap(settings);
    let icons_w = (slot_count as f32) * base + gap * (slot_count.saturating_sub(1) as f32);
    (shell_section_width(base) + icons_w + 30.0).max(160.0)
}

/// Pre-allocates horizontal headroom for smooth fisheye neighbor parting so the Win32
/// window never resizes during hover.
fn compute_window_dips(settings: &DockSettings, slot_count: usize) -> (f32, f32) {
    let base = settings.icon_dips();
    let resting_w = resting_pill_width(settings, slot_count);
    let mag_extra = if settings.magnification_enabled {
        base * 1.4
    } else {
        0.0
    };
    let width_dips = (resting_w + mag_extra + 24.0).ceil();
    let pill_h = pill_height_dips(settings);
    let height_dips = (pill_h + TOOLTIP_HEADROOM + MAG_HEADROOM + VERTICAL_EDGE_GAP).ceil();
    (width_dips, height_dips)
}

/// Damped double-hop activation bounce offset in DIPs (matches C# AnimateActivationFeedback).
fn bounce_offset_dips(started: Option<Instant>) -> Option<f32> {
    let t = started?.elapsed().as_secs_f32() / BOUNCE_DURATION_SECS;
    if t >= 1.0 {
        return None;
    }
    let y = if t < 0.62 {
        let u = t / 0.62;
        (u * std::f32::consts::PI).sin() * 11.0
    } else {
        let u = (t - 0.62) / 0.38;
        (u * std::f32::consts::PI).sin() * 4.5
    };
    Some(y)
}

fn render_shell_svg(kind: ShellButton, base: f32) -> impl IntoElement {
    let icon_sz = (base * 0.62).round().max(20.0);
    let (path, color) = match kind {
        ShellButton::Start => ("icons/start.svg", rgba(0x38bdf8ff)),
        ShellButton::Search => ("icons/search.svg", rgba(0xe2e8f0ff)),
        ShellButton::TaskView => ("icons/taskview.svg", rgba(0xe2e8f0ff)),
    };
    svg().path(path).size(px(icon_sz)).text_color(color)
}

impl DockView {
    fn new(
        hwnd: usize,
        settings: DockSettings,
        pins: Vec<PinnedApp>,
        initial_slots: Vec<DockSlot>,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = Self {
            hwnd,
            settings,
            pins,
            slots: initial_slots,
            icons: HashMap::new(),
            hover_mouse_x: None,
            hovered_idx: None,
            hovered_shell: None,
            pressed_idx: None,
            pressed_shell: None,
            drag: None,
            suppress_next_click: false,
            last_frame: None,
            last_window_rect: None,
        };
        view.request_missing_icons(cx);
        view.start_window_poll(cx);
        view
    }

    fn request_missing_icons(&self, cx: &mut Context<Self>) {
        let missing: Vec<String> = self
            .slots
            .iter()
            .map(|s| s.icon_source.clone())
            .filter(|p| !self.icons.contains_key(p))
            .collect();
        if missing.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move { icon_cache::resolve_batch(&missing) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.icons.extend(resolved);
                cx.notify();
            });
        })
        .detach();
    }

    /// Syncs slots from running windows and returns `true` only if visible state changed.
    fn sync_slots(&mut self, cx: &mut Context<Self>) -> bool {
        let running = tracker::enumerate_windows();
        let next = build_slots(&self.pins, &running, &self.slots);
        let changed = next.len() != self.slots.len()
            || next.iter().zip(self.slots.iter()).any(|(a, b)| {
                a.key != b.key
                    || a.is_foreground != b.is_foreground
                    || a.hwnds.is_empty() != b.hwnds.is_empty()
            });
        self.slots = next;
        self.request_missing_icons(cx);
        changed
    }

    fn sync_window_bounds(&mut self) {
        let (width_dips, height_dips) = compute_window_dips(&self.settings, self.slots.len());
        let dpi = win32::dpi_scale(self.hwnd);
        let (scr_w, scr_h) = win32::primary_screen_px();
        let w_px = ((width_dips * dpi).ceil() as i32).min(scr_w);
        let h_px = ((height_dips * dpi).ceil() as i32).min(scr_h);
        let margin_px = (12.0 * dpi).round() as i32;
        let x_px = match self.settings.position {
            DockPosition::Left => margin_px,
            DockPosition::Center => (scr_w - w_px) / 2,
            DockPosition::Right => scr_w - w_px - margin_px,
        };
        let y_px = scr_h - h_px;

        let rect = (x_px, y_px, w_px, h_px);
        if self.last_window_rect != Some(rect) {
            self.last_window_rect = Some(rect);
            win32::reposition_dock_window(self.hwnd, x_px, y_px, w_px, h_px);
        }
    }

    fn start_window_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(800)).await;
                let _ = opentaskbar_shell::taskbar::ensure_hidden();
                let updated = this.update(cx, |view, cx| {
                    let prev_len = view.slots.len();
                    let changed = view.sync_slots(cx);
                    if view.slots.len() != prev_len {
                        view.sync_window_bounds();
                    }
                    // Never redraw the GPU swapchain on idle polls when nothing changed.
                    if changed {
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Resting X center of app slot `idx` in window logical DIPs (immune to current magnification).
    fn resting_slot_center_x(&self, idx: usize) -> f32 {
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let (win_w, _) = compute_window_dips(&self.settings, self.slots.len());
        let pill_w = resting_pill_width(&self.settings, self.slots.len());
        let pill_left = (win_w - pill_w) * 0.5;
        let apps_left = pill_left + 15.0 + shell_section_width(base);
        apps_left + (idx as f32) * (base + gap) + base * 0.5
    }

    /// Live horizontal center X of app slot `idx` in window logical DIPs (accounts for current magnification).
    fn live_slot_center_x(&self, idx: usize) -> f32 {
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let (win_w, _) = compute_window_dips(&self.settings, self.slots.len());
        let icons_w: f32 = self.slots.iter().map(|s| base * s.scale).sum::<f32>()
            + gap * (self.slots.len().saturating_sub(1) as f32);
        let pill_w = (shell_section_width(base) + icons_w + 30.0).max(160.0);
        let pill_left = (win_w - pill_w) * 0.5;
        let mut cursor = pill_left + 15.0 + shell_section_width(base);
        for i in 0..idx.min(self.slots.len()) {
            cursor += base * self.slots[i].scale + gap;
        }
        let w = self.slots.get(idx).map_or(base, |s| base * s.scale);
        cursor + w * 0.5
    }

    /// Live horizontal center X of a left-side Windows shell button in window logical DIPs.
    fn live_shell_center_x(&self, kind: ShellButton) -> f32 {
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let (win_w, _) = compute_window_dips(&self.settings, self.slots.len());
        let icons_w: f32 = self.slots.iter().map(|s| base * s.scale).sum::<f32>()
            + gap * (self.slots.len().saturating_sub(1) as f32);
        let pill_w = (shell_section_width(base) + icons_w + 30.0).max(160.0);
        let pill_left = (win_w - pill_w) * 0.5;
        let index = match kind {
            ShellButton::Start => 0.0,
            ShellButton::Search => 1.0,
            ShellButton::TaskView => 2.0,
        };
        pill_left + 15.0 + index * (base + 4.0) + base * 0.5
    }

    /// Handles pointer motion during an active slot drag: the dragged icon floats smoothly
    /// under the cursor while neighboring slots glide aside with spring animation.
    fn update_drag(&mut self, mx: f32) {
        let Some(mut drag) = self.drag else {
            return;
        };
        drag.current_x = mx;
        if !drag.active && (mx - drag.start_x).abs() >= DRAG_THRESHOLD_PX {
            drag.active = true;
            self.hover_mouse_x = None;
            self.hovered_idx = None;
            self.hovered_shell = None;
        }
        if drag.active && !self.slots.is_empty() {
            let center_x = mx - drag.grab_offset_x;
            let target = self
                .slots
                .iter()
                .enumerate()
                .map(|(i, _)| (i, (center_x - self.resting_slot_center_x(i)).abs()))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
                .unwrap_or(drag.idx);
            if target != drag.idx && target < self.slots.len() && drag.idx < self.slots.len() {
                let step = self.settings.icon_dips() + slot_gap(&self.settings);
                let old_idx = drag.idx;
                let item = self.slots.remove(old_idx);
                self.slots.insert(target, item);
                // Give every shifted neighbor a spring displacement from its old screen position
                // so it visibly glides smoothly into its new slot.
                if target > old_idx {
                    for slot in &mut self.slots[old_idx..target] {
                        slot.slide_x += step;
                    }
                } else {
                    for slot in &mut self.slots[(target + 1)..=old_idx] {
                        slot.slide_x -= step;
                    }
                }
                drag.idx = target;
                self.pressed_idx = Some(target);
            }
        }
        self.drag = Some(drag);
    }

    /// Finishes an active drag and persists the new pinned order to `settings.json`.
    fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if drag.active {
            self.suppress_next_click = true;
            let order: Vec<String> = self
                .slots
                .iter()
                .filter(|s| s.pinned)
                .map(|s| s.launch_path.clone())
                .collect();
            self.settings.pin_order = Some(order);
            let _ = opentaskbar_core::settings::save_to(
                &opentaskbar_core::settings::settings_path(),
                &self.settings,
            );
        }
    }

    /// Smooth cosine bell magnification based on continuous mouse X vs resting slot centers.
    fn target_scale_for(&self, idx: usize) -> f32 {
        if self.drag.is_some_and(|d| d.active) {
            return 1.0;
        }
        if !self.settings.magnification_enabled || !self.settings.animations_enabled {
            return 1.0;
        }
        let Some(mx) = self.hover_mouse_x else {
            return 1.0;
        };
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let radius = (base + gap) * FISHEYE_RADIUS_SLOTS;
        let dist = (mx - self.resting_slot_center_x(idx)).abs();
        if dist >= radius {
            return 1.0;
        }
        let c = (dist / radius * std::f32::consts::FRAC_PI_2).cos();
        1.0 + FISHEYE_PEAK * c * c
    }

    fn update_hovered_index_from_mouse(&mut self) {
        let Some(mx) = self.hover_mouse_x else {
            self.hovered_idx = None;
            return;
        };
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let max_dist = (base + gap) * 0.55;
        self.hovered_idx = self
            .slots
            .iter()
            .enumerate()
            .map(|(i, _)| (i, (mx - self.resting_slot_center_x(i)).abs()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, d)| *d <= max_dist)
            .map(|(i, _)| i);
    }

    /// Advances all spring/bounce animations using framerate-independent delta-time
    /// and returns `true` if another VSync frame is needed.
    fn step_animations(&mut self) -> bool {
        let now = Instant::now();
        let dt = self
            .last_frame
            .map_or(1.0 / 60.0, |prev| now.duration_since(prev).as_secs_f32())
            .clamp(0.001, 0.05);
        self.last_frame = Some(now);

        let scale_k = 1.0 - (-22.0 * dt).exp();
        let slide_k = 1.0 - (-24.0 * dt).exp();
        let mut moving = false;

        for idx in 0..self.slots.len() {
            let target = self.target_scale_for(idx);
            let cur = self.slots[idx].scale;
            let diff = target - cur;
            if diff.abs() > 0.004 {
                self.slots[idx].scale = cur + diff * scale_k;
                moving = true;
            } else if (cur - target).abs() > 0.0 {
                self.slots[idx].scale = target;
            }

            let sx = self.slots[idx].slide_x;
            if sx.abs() > 0.35 {
                self.slots[idx].slide_x = sx * (1.0 - slide_k);
                moving = true;
            } else if sx != 0.0 {
                self.slots[idx].slide_x = 0.0;
            }

            if let Some(started) = self.slots[idx].bounce_started {
                if bounce_offset_dips(Some(started)).is_some() {
                    moving = true;
                } else {
                    self.slots[idx].bounce_started = None;
                }
            }
        }

        if !moving {
            self.last_frame = None;
        }
        moving
    }

    fn ensure_animation(&mut self, cx: &mut Context<Self>) {
        if self.last_frame.is_none() {
            self.last_frame = Some(Instant::now());
            cx.notify();
        }
    }

    fn handle_right_click(&self, cx: &mut Context<Self>) {
        let hwnd = self.hwnd;
        cx.spawn(async move |_this, cx| {
            let action = cx
                .background_executor()
                .spawn(async move { win32::show_dock_context_menu(hwnd) })
                .await;
            let _ = cx.update(|cx| match action {
                ContextAction::TaskManager => pins::launch("taskmgr.exe".to_owned()),
                ContextAction::RestoreTaskbar => opentaskbar_shell::taskbar::restore_from_journal(),
                ContextAction::Exit => cx.quit(),
                ContextAction::None => {}
            });
        })
        .detach();
    }

    fn render_shell_button(&self, kind: ShellButton, cx: &mut Context<Self>) -> impl IntoElement {
        let base = self.settings.icon_dips();
        let hovered = self.hovered_shell == Some(kind);
        let pressed = self.pressed_shell == Some(kind);
        let id = match kind {
            ShellButton::Start => "shell-start",
            ShellButton::Search => "shell-search",
            ShellButton::TaskView => "shell-taskview",
        };
        let visual_base = if pressed { base * 0.88 } else { base };
        div()
            .id(id)
            .w(px(base))
            .h(px(base))
            .mb(px(6.5))
            .rounded(px(9.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(if pressed {
                rgba(0xffffff2e)
            } else if hovered {
                rgba(0xffffff1c)
            } else {
                rgba(0x00000000)
            })
            .on_hover(cx.listener(move |this, active, _window, cx| {
                if *active {
                    this.hovered_shell = Some(kind);
                    this.hover_mouse_x = None;
                    this.hovered_idx = None;
                    this.ensure_animation(cx);
                } else if this.hovered_shell == Some(kind) {
                    this.hovered_shell = None;
                    this.pressed_shell = None;
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _window, cx| {
                    this.pressed_shell = Some(kind);
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.pressed_shell = None;
                cx.notify();
                match kind {
                    ShellButton::Start => opentaskbar_shell::taskbar::open_start_menu(),
                    ShellButton::Search => opentaskbar_shell::taskbar::open_search(),
                    ShellButton::TaskView => opentaskbar_shell::taskbar::open_task_view(),
                }
            }))
            .child(render_shell_svg(kind, visual_base))
    }
}

impl Render for DockView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.step_animations() {
            // Native DWM VSync frame pacing (locked to 60/120/144Hz display refresh rate).
            window.request_animation_frame();
        }
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let radius = self.settings.corner_radius_dips();
        let pill_h = pill_height_dips(&self.settings);

        // Each slot expands proportionally to its magnified icon so icons never overlap,
        // while resting centers drive the fisheye curve so there is zero feedback jitter.
        let icons_w: f32 = self.slots.iter().map(|s| base * s.scale).sum::<f32>()
            + gap * (self.slots.len().saturating_sub(1) as f32);
        let pill_w = (shell_section_width(base) + icons_w + 30.0).max(160.0);

        let alpha = (self.settings.dock_opacity.clamp(0.1, 1.0) * 218.0) as u32;
        let pill_bg = if self.settings.dock_background == SurfaceBackground::None {
            rgba(0x00000000)
        } else {
            rgba(0x14171f00 | alpha)
        };
        let active_drag = self.drag.filter(|d| d.active);
        let is_dragging = active_drag.is_some();
        // Automatically position tooltip 8px above the exact visual top edge of the hovered item:
        // - Shell buttons (no fisheye) -> 8px above the dock pill
        // - App icons (with fisheye) -> 8px above the magnified icon's live top edge
        let hovered_tooltip: Option<(SharedString, f32, f32)> = if is_dragging {
            None
        } else if let Some(shell) = self.hovered_shell {
            Some((
                SharedString::from(shell.label()),
                self.live_shell_center_x(shell),
                VERTICAL_EDGE_GAP + pill_h + 8.0,
            ))
        } else if let Some(idx) = self.hovered_idx
            && let Some(slot) = self.slots.get(idx)
        {
            let bounce = bounce_offset_dips(slot.bounce_started).unwrap_or(0.0);
            let lift = ((slot.scale - 1.0) * 14.0 + bounce).max(0.0);
            let icon_top = VERTICAL_EDGE_GAP + 6.0 + 3.5 + 4.0 + lift + base * slot.scale;
            let top_edge = icon_top.max(VERTICAL_EDGE_GAP + pill_h);
            Some((
                slot.label.clone(),
                self.live_slot_center_x(idx),
                top_edge + 8.0,
            ))
        } else {
            None
        };
        let (win_w, _) = compute_window_dips(&self.settings, self.slots.len());

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_end()
            .pb(px(VERTICAL_EDGE_GAP))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _window, cx| {
                    this.handle_right_click(cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.drag.is_some() {
                    let mx: f32 = event.position.x.into();
                    this.update_drag(mx);
                    this.ensure_animation(cx);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.finish_drag();
                    if this.pressed_idx.is_some() || this.pressed_shell.is_some() {
                        this.pressed_idx = None;
                        this.pressed_shell = None;
                        cx.notify();
                    }
                }),
            )
            // Fixed-height background pill anchored 5px from bottom edge (no per-frame shadow blur shader).
            .child(
                div()
                    .absolute()
                    .bottom(px(VERTICAL_EDGE_GAP))
                    .w(px(pill_w))
                    .h(px(pill_h))
                    .rounded(px(radius))
                    .bg(pill_bg)
                    .when(
                        self.settings.dock_background != SurfaceBackground::None,
                        |el| el.border_1().border_color(rgba(0xffffff28)),
                    ),
            )
            // Shell buttons + divider + app icons float over the fixed-height pill.
            .child(
                div()
                    .relative()
                    .flex()
                    .items_end()
                    .px(px(15.0))
                    .pb(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(self.render_shell_button(ShellButton::Start, cx))
                            .child(self.render_shell_button(ShellButton::Search, cx))
                            .child(self.render_shell_button(ShellButton::TaskView, cx)),
                    )
                    .child(
                        div()
                            .mx(px(8.0))
                            .mb(px(10.0))
                            .w(px(1.0))
                            .h(px(22.0))
                            .bg(rgba(0xffffff26)),
                    )
                    .child(
                        div()
                            .id("dock-apps-strip")
                            .flex()
                            .items_end()
                            .gap(px(gap))
                            .on_mouse_move(cx.listener(
                                |this, event: &gpui::MouseMoveEvent, _, cx| {
                                    let mx: f32 = event.position.x.into();
                                    if this.drag.is_some() {
                                        this.update_drag(mx);
                                        this.ensure_animation(cx);
                                        cx.notify();
                                    } else {
                                        // Ignore sub-pixel jitter and let the 60fps animation loop pace GPU frames
                                        // instead of redrawing on every 500-1000Hz raw mouse event.
                                        if this
                                            .hover_mouse_x
                                            .is_some_and(|prev| (prev - mx).abs() < 1.0)
                                        {
                                            return;
                                        }
                                        let prev_idx = this.hovered_idx;
                                        this.hover_mouse_x = Some(mx);
                                        this.hovered_shell = None;
                                        this.update_hovered_index_from_mouse();
                                        this.ensure_animation(cx);
                                        if this.hovered_idx != prev_idx {
                                            cx.notify();
                                        }
                                    }
                                },
                            ))
                            .on_hover(cx.listener(|this, active: &bool, _, cx| {
                                if !*active && this.drag.is_none() {
                                    this.hover_mouse_x = None;
                                    this.hovered_idx = None;
                                    this.pressed_idx = None;
                                    this.ensure_animation(cx);
                                    cx.notify();
                                }
                            }))
                            .children(self.slots.iter().enumerate().map(|(idx, slot)| {
                                let dragging_this = active_drag.is_some_and(|d| d.idx == idx);
                                let pressed = self.pressed_idx == Some(idx) && !dragging_this;
                                let press_factor = if pressed { 0.88 } else { 1.0 };
                                let slot_w = base * slot.scale;
                                let dim = slot_w * press_factor;
                                let bounce = bounce_offset_dips(slot.bounce_started).unwrap_or(0.0);
                                let lift = ((slot.scale - 1.0) * 14.0 + bounce
                                    - if pressed { 2.0 } else { 0.0 })
                                .max(0.0);
                                let slide_x = slot.slide_x;
                                let icon =
                                    self.icons.get(&slot.icon_source).and_then(|o| o.clone());
                                let hwnds = slot.hwnds.clone();
                                let launch_path = slot.launch_path.clone();
                                let running = !hwnds.is_empty();
                                let active = slot.is_foreground;
                                let fallback_char: SharedString = slot
                                    .label
                                    .chars()
                                    .next()
                                    .unwrap_or('?')
                                    .to_uppercase()
                                    .to_string()
                                    .into();

                                div()
                                    .id(SharedString::from(format!("dock-slot-{idx}")))
                                    .relative()
                                    .left(px(slide_x))
                                    .w(px(slot_w))
                                    // Hide underlying slot while its floating ghost tracks the cursor.
                                    .when(dragging_this, |el| el.opacity(0.0))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap(px(4.0))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |this, event: &gpui::MouseDownEvent, _, cx| {
                                                let mx: f32 = event.position.x.into();
                                                let center = this.live_slot_center_x(idx);
                                                this.pressed_idx = Some(idx);
                                                this.drag = Some(DragState {
                                                    idx,
                                                    start_x: mx,
                                                    current_x: mx,
                                                    grab_offset_x: mx - center,
                                                    active: false,
                                                });
                                                cx.notify();
                                            },
                                        ),
                                    )
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.finish_drag();
                                        this.pressed_idx = None;
                                        if this.suppress_next_click {
                                            this.suppress_next_click = false;
                                            cx.notify();
                                            return;
                                        }
                                        if let Some(slot) = this.slots.get_mut(idx) {
                                            slot.bounce_started = Some(Instant::now());
                                        }
                                        this.ensure_animation(cx);
                                        cx.notify();
                                        if !hwnds.is_empty() {
                                            tracker::activate_or_toggle(&hwnds);
                                        } else {
                                            pins::launch(launch_path.clone());
                                        }
                                    }))
                                    .child(
                                        div()
                                            .w(px(slot_w))
                                            .h(px(base))
                                            .flex()
                                            .items_end()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .mb(px(lift))
                                                    .w(px(dim))
                                                    .h(px(dim))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .child(match icon {
                                                        Some(image) => img(image)
                                                            .w(px(dim))
                                                            .h(px(dim))
                                                            .into_any_element(),
                                                        None => div()
                                                            .size_full()
                                                            .rounded(px(10.0))
                                                            .bg(rgba(0x334155cc))
                                                            .border_1()
                                                            .border_color(hsla(0.0, 0.0, 1.0, 0.18))
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .text_sm()
                                                            .text_color(rgba(0xf8fafcff))
                                                            .child(fallback_char)
                                                            .into_any_element(),
                                                    }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w(px(if active { 14.0 } else { 5.0 }))
                                            .h(px(3.5))
                                            .rounded(px(2.0))
                                            .bg(if !running {
                                                rgba(0x00000000)
                                            } else if active {
                                                rgba(0xffffffff)
                                            } else {
                                                rgba(0xcbd5e1cc)
                                            }),
                                    )
                            })),
                    ),
            )
            // Floating ghost of the dragged icon tracking the cursor smoothly in real time.
            .when_some(
                active_drag.and_then(|d| self.slots.get(d.idx).map(|s| (d, s))),
                |el, (drag, slot)| {
                    let ghost_dim = base * 1.14;
                    let left_x = (drag.current_x - drag.grab_offset_x - ghost_dim * 0.5)
                        .clamp(4.0, (win_w - ghost_dim - 4.0).max(4.0));
                    let bottom_y = VERTICAL_EDGE_GAP + 20.0;
                    let icon = self.icons.get(&slot.icon_source).and_then(|o| o.clone());
                    let fallback_char: SharedString = slot
                        .label
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string()
                        .into();
                    el.child(
                        div()
                            .absolute()
                            .left(px(left_x))
                            .bottom(px(bottom_y))
                            .w(px(ghost_dim))
                            .h(px(ghost_dim))
                            .opacity(0.88)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(match icon {
                                Some(image) => img(image)
                                    .w(px(ghost_dim))
                                    .h(px(ghost_dim))
                                    .into_any_element(),
                                None => div()
                                    .size_full()
                                    .rounded(px(10.0))
                                    .bg(rgba(0x334155cc))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .text_color(rgba(0xf8fafcff))
                                    .child(fallback_char)
                                    .into_any_element(),
                            }),
                    )
                },
            )
            // Tooltip rendered LAST (always on top of magnified icons) at the exact auto-computed height.
            .when_some(hovered_tooltip, |el, (label, center_x, tooltip_bottom)| {
                let est_w = (label.len() as f32 * 6.8 + 22.0).clamp(54.0, 220.0);
                let left_x = (center_x - est_w * 0.5).clamp(6.0, (win_w - est_w - 6.0).max(6.0));
                el.child(
                    div()
                        .absolute()
                        .left(px(left_x))
                        .bottom(px(tooltip_bottom))
                        .w(px(est_w))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .px(px(10.0))
                                .py(px(4.0))
                                .rounded(px(8.0))
                                .bg(rgba(0x181b24f2))
                                .border_1()
                                .border_color(rgba(0xffffff2c))
                                .text_xs()
                                .text_color(rgba(0xf8fafcff))
                                .whitespace_nowrap()
                                .child(label),
                        ),
                )
            })
    }
}

pub fn run(settings: DockSettings) {
    let auto_hide = settings.auto_hide != AutoHideMode::Disabled;
    let mut initial_pins = pins::load_pins();
    if let Some(ref order) = settings.pin_order {
        initial_pins.sort_by_key(|p| {
            order
                .iter()
                .position(|saved| saved.eq_ignore_ascii_case(&p.launch_path))
                .unwrap_or(usize::MAX)
        });
    }
    let initial_running = tracker::enumerate_windows();
    let initial_slots = build_slots(&initial_pins, &initial_running, &[]);
    let (init_w, init_h) = compute_window_dips(&settings, initial_slots.len());

    Application::new()
        .with_assets(EmbeddedAssets)
        .run(move |cx: &mut App| {
            let dock_w = px(init_w);
            let dock_h = px(init_h);
            let bounds = match cx.primary_display() {
                Some(display) => {
                    let area = display.bounds();
                    let x = match settings.position {
                        DockPosition::Left => area.origin.x + px(12.0),
                        DockPosition::Center => area.origin.x + (area.size.width - dock_w) * 0.5,
                        DockPosition::Right => area.origin.x + area.size.width - dock_w - px(12.0),
                    };
                    let y = area.origin.y + area.size.height - dock_h;
                    Bounds::new(point(x, y), size(dock_w, dock_h))
                }
                None => Bounds::centered(None, size(dock_w, dock_h), cx),
            };

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: false,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            };

            let settings_clone = settings.clone();
            let opened = cx.open_window(options, |window, cx| {
                let hwnd = win32::configure_dock_window(window).unwrap_or(0);
                let dpi = win32::dpi_scale(hwnd);
                let desired_total_dips =
                    pill_height_dips(&settings_clone) + VERTICAL_EDGE_GAP * 2.0;
                let desired_total_px = (desired_total_dips * dpi).ceil() as i32;
                opentaskbar_shell::taskbar::begin_replacement(hwnd, auto_hide, desired_total_px);
                win32::trim_working_set();
                cx.new(|cx| DockView::new(hwnd, settings_clone, initial_pins, initial_slots, cx))
            });

            if let Err(err) = opened {
                opentaskbar_core::log::error(format!("Failed to open GPUI dock window: {err}"));
                cx.quit();
            }
        });
}
