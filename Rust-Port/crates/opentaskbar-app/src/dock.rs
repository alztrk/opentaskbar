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
const TOOLTIP_HEADROOM: f32 = 34.0;
const MAG_HEADROOM: f32 = 30.0;
const FISHEYE_PEAK: f32 = 0.42;
const FISHEYE_RADIUS_SLOTS: f32 = 2.4;
const BOUNCE_DURATION_SECS: f32 = 0.42;

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
    bounce_started: Option<Instant>,
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
    animating: bool,
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

/// Builds dock slots while preserving the exact order of previously seen unpinned apps
/// so clicking/focusing an app never causes icons to jump or reorder.
fn build_slots(pins: &[PinnedApp], running: &[RunningWindow], prev: &[DockSlot]) -> Vec<DockSlot> {
    let mut by_exe: HashMap<String, Vec<&RunningWindow>> = HashMap::new();
    for win in running {
        by_exe
            .entry(win.exe_path.to_lowercase())
            .or_default()
            .push(win);
    }

    let mut next: Vec<DockSlot> = Vec::with_capacity(pins.len() + by_exe.len());
    let mut claimed_exes: HashMap<String, bool> = HashMap::new();

    // 1. Pinned apps in their stable pin order.
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

        let (hwnds, is_foreground) = match matched_key {
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

        next.push(DockSlot {
            key: pin.launch_path.to_lowercase(),
            pinned: true,
            label: pin.name.clone().into(),
            launch_path: pin.launch_path.clone(),
            icon_source: pin.icon_source.clone(),
            hwnds,
            is_foreground,
            scale: 1.0,
            bounce_started: None,
        });
    }

    // 2. Keep already-open unpinned apps in their exact previous order (immune to Z-order changes).
    for old in prev.iter().filter(|s| !s.pinned) {
        if claimed_exes.contains_key(&old.key) {
            continue;
        }
        if let Some(list) = by_exe.get(&old.key) {
            claimed_exes.insert(old.key.clone(), true);
            next.push(DockSlot {
                key: old.key.clone(),
                pinned: false,
                label: old.label.clone(),
                launch_path: old.launch_path.clone(),
                icon_source: old.icon_source.clone(),
                hwnds: list.iter().map(|w| w.hwnd).collect(),
                is_foreground: list.iter().any(|w| w.is_foreground),
                scale: old.scale,
                bounce_started: old.bounce_started,
            });
        }
    }

    // 3. Append newly launched unpinned apps at the end.
    for win in running {
        let key = win.exe_path.to_lowercase();
        if claimed_exes.contains_key(&key) {
            continue;
        }
        claimed_exes.insert(key.clone(), true);
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
            bounce_started: None,
        });
    }

    // Preserve animation state for pinned slots across refreshes.
    for slot in &mut next {
        if slot.pinned
            && let Some(old) = prev.iter().find(|p| p.key == slot.key)
        {
            slot.scale = old.scale;
            slot.bounce_started = old.bounce_started;
        }
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
            animating: false,
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

    fn sync_slots(&mut self, cx: &mut Context<Self>) {
        let running = tracker::enumerate_windows();
        self.slots = build_slots(&self.pins, &running, &self.slots);
        self.request_missing_icons(cx);
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
                    view.sync_slots(cx);
                    if view.slots.len() != prev_len {
                        view.sync_window_bounds();
                    }
                    cx.notify();
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

    /// Smooth cosine bell magnification based on continuous mouse X vs resting slot centers.
    fn target_scale_for(&self, idx: usize) -> f32 {
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

    fn ensure_animation(&mut self, cx: &mut Context<Self>) {
        if self.animating {
            return;
        }
        self.animating = true;
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(16)).await;
                let keep_going = this
                    .update(cx, |view, cx| {
                        let mut moving = false;
                        for idx in 0..view.slots.len() {
                            let target = view.target_scale_for(idx);
                            let cur = view.slots[idx].scale;
                            let diff = target - cur;
                            if diff.abs() > 0.003 {
                                view.slots[idx].scale = cur + diff * 0.30;
                                moving = true;
                            } else if (cur - target).abs() > 0.0 {
                                view.slots[idx].scale = target;
                            }
                            if let Some(started) = view.slots[idx].bounce_started {
                                if bounce_offset_dips(Some(started)).is_some() {
                                    moving = true;
                                } else {
                                    view.slots[idx].bounce_started = None;
                                }
                            }
                        }
                        if moving {
                            cx.notify();
                        } else {
                            view.animating = false;
                            cx.notify();
                        }
                        moving
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let hovered_label: Option<SharedString> = self
            .hovered_shell
            .map(|b| SharedString::from(b.label()))
            .or_else(|| {
                self.hovered_idx
                    .and_then(|i| self.slots.get(i))
                    .map(|s| s.label.clone())
            });

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
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    if this.pressed_idx.is_some() || this.pressed_shell.is_some() {
                        this.pressed_idx = None;
                        this.pressed_shell = None;
                        cx.notify();
                    }
                }),
            )
            // Fixed-height background pill anchored 5px from bottom edge.
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
                        |el| el.border_1().border_color(rgba(0xffffff28)).shadow_md(),
                    ),
            )
            .when_some(hovered_label, |el, label| {
                el.child(
                    div()
                        .mb(px(6.0))
                        .px(px(10.0))
                        .py(px(4.0))
                        .rounded(px(8.0))
                        .bg(rgba(0x181b24eb))
                        .border_1()
                        .border_color(rgba(0xffffff24))
                        .text_xs()
                        .text_color(rgba(0xf1f5f9ff))
                        .child(label),
                )
            })
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
                                    this.hover_mouse_x = Some(mx);
                                    this.hovered_shell = None;
                                    this.update_hovered_index_from_mouse();
                                    this.ensure_animation(cx);
                                    cx.notify();
                                },
                            ))
                            .on_hover(cx.listener(|this, active: &bool, _, cx| {
                                if !*active {
                                    this.hover_mouse_x = None;
                                    this.hovered_idx = None;
                                    this.pressed_idx = None;
                                    this.ensure_animation(cx);
                                    cx.notify();
                                }
                            }))
                            .children(self.slots.iter().enumerate().map(|(idx, slot)| {
                                let pressed = self.pressed_idx == Some(idx);
                                let press_factor = if pressed { 0.88 } else { 1.0 };
                                let slot_w = base * slot.scale;
                                let dim = slot_w * press_factor;
                                let bounce = bounce_offset_dips(slot.bounce_started).unwrap_or(0.0);
                                let lift = ((slot.scale - 1.0) * 14.0 + bounce
                                    - if pressed { 2.0 } else { 0.0 })
                                .max(0.0);
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
                                    .w(px(slot_w))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap(px(4.0))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.pressed_idx = Some(idx);
                                            cx.notify();
                                        }),
                                    )
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.pressed_idx = None;
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
    }
}

pub fn run(settings: DockSettings) {
    let auto_hide = settings.auto_hide != AutoHideMode::Disabled;
    let initial_pins = pins::load_pins();
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
