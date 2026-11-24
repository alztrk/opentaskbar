//! GPUI floating dock window:
//! - Exact initial placement (zero startup jump)
//! - Fixed-height background pill so hovered icons pop OUT above the pill
//! - Respects screen-capture overlays (never steals Z-order above Snipping Tool)
//! - Grouped apps, crisp C#-matched icon extraction, tooltips, and native context menu

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Application, Bounds, Context, MouseButton, RenderImage, SharedString, Timer, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions, div,
    hsla, img, point, prelude::*, px, rgba, size,
};
use opentaskbar_core::settings::{AutoHideMode, DockPosition, DockSettings, SurfaceBackground};
use opentaskbar_shell::tracker::{self, RunningWindow};

use crate::icon_cache;
use crate::pins::{self, PinnedApp};
use crate::win32::{self, ContextAction};

const TOOLTIP_HEADROOM: f32 = 34.0;
const MAG_HEADROOM: f32 = 28.0;

#[derive(Clone)]
struct DockSlot {
    key: String,
    label: SharedString,
    launch_path: String,
    icon_source: String,
    hwnds: Vec<usize>,
    is_foreground: bool,
    scale: f32,
}

struct DockView {
    hwnd: usize,
    settings: DockSettings,
    pins: Vec<PinnedApp>,
    slots: Vec<DockSlot>,
    icons: HashMap<String, Option<Arc<RenderImage>>>,
    hovered_idx: Option<usize>,
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
            label: pin.name.clone().into(),
            launch_path: pin.launch_path.clone(),
            icon_source: pin.icon_source.clone(),
            hwnds,
            is_foreground,
            scale: 1.0,
        });
    }

    for win in running {
        let key = win.exe_path.to_lowercase();
        if claimed_exes.contains_key(&key) {
            continue;
        }
        claimed_exes.insert(key.clone(), true);
        let list = &by_exe[&key];
        let label = exe_display_name(&win.exe_path, &win.title);
        next.push(DockSlot {
            key: key.clone(),
            label: label.into(),
            launch_path: win.exe_path.clone(),
            icon_source: win.exe_path.clone(),
            hwnds: list.iter().map(|w| w.hwnd).collect(),
            is_foreground: list.iter().any(|w| w.is_foreground),
            scale: 1.0,
        });
    }

    for slot in &mut next {
        if let Some(old) = prev.iter().find(|p| p.key == slot.key) {
            slot.scale = old.scale;
        }
    }
    next
}

fn compute_window_dips(settings: &DockSettings, slot_count: usize) -> (f32, f32) {
    let base = settings.icon_dips();
    let gap = 8.0 + settings.dock_icon_padding.max(0) as f32;
    let count = slot_count.max(1) as f32;
    let mag_budget = if settings.magnification_enabled {
        base * 1.15
    } else {
        0.0
    };
    let width_dips = (count * base + (count - 1.0) * gap + 40.0 + mag_budget).ceil();
    let bottom_gap = settings.dock_bottom_gap_pixels.max(0) as f32;
    let height_dips = (base + 24.0 + TOOLTIP_HEADROOM + MAG_HEADROOM + bottom_gap).ceil();
    (width_dips, height_dips)
}

impl DockView {
    fn new(
        hwnd: usize,
        settings: DockSettings,
        pins: Vec<PinnedApp>,
        initial_slots: Vec<DockSlot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = Self {
            hwnd,
            settings,
            pins,
            slots: initial_slots,
            icons: HashMap::new(),
            hovered_idx: None,
            animating: false,
            last_window_rect: None,
        };
        view.request_missing_icons(cx);
        view.start_window_poll(window, cx);
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

    fn sync_window_bounds(&mut self, window: &mut Window) {
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
            window.resize(size(px(width_dips), px(height_dips)));
            win32::reposition_dock_window(self.hwnd, x_px, y_px, w_px, h_px);
        }
    }

    fn start_window_poll(&self, window: &mut Window, cx: &mut Context<Self>) {
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(800)).await;
                let _ = opentaskbar_shell::taskbar::ensure_hidden();
                let updated = cx.update_window(window_handle, |_, window, cx| {
                    let _ = this.update(cx, |view, cx| {
                        let prev_len = view.slots.len();
                        view.sync_slots(cx);
                        if view.slots.len() != prev_len {
                            view.sync_window_bounds(window);
                        }
                        cx.notify();
                    });
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn target_scale_for(&self, idx: usize) -> f32 {
        if !self.settings.magnification_enabled || !self.settings.animations_enabled {
            return 1.0;
        }
        match self.hovered_idx {
            Some(hovered) => match idx.abs_diff(hovered) {
                0 => 1.38,
                1 => 1.18,
                2 => 1.07,
                _ => 1.0,
            },
            None => 1.0,
        }
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
                            if diff.abs() > 0.004 {
                                view.slots[idx].scale = cur + diff * 0.28;
                                moving = true;
                            } else if (cur - target).abs() > 0.0 {
                                view.slots[idx].scale = target;
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
        match win32::show_dock_context_menu(self.hwnd) {
            ContextAction::TaskManager => pins::launch("taskmgr.exe".to_owned()),
            ContextAction::RestoreTaskbar => opentaskbar_shell::taskbar::restore_from_journal(),
            ContextAction::Exit => cx.quit(),
            ContextAction::None => {}
        }
    }
}

impl Render for DockView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let base = self.settings.icon_dips();
        let gap = 8.0 + self.settings.dock_icon_padding.max(0) as f32;
        let radius = self.settings.corner_radius_dips();
        let bottom_gap = self.settings.dock_bottom_gap_pixels.max(0) as f32;
        // Fixed background pill height: never stretches vertically when icons magnify above it.
        let pill_h = base + 20.0;
        let icons_w: f32 = self.slots.iter().map(|s| base * s.scale).sum::<f32>()
            + gap * (self.slots.len().saturating_sub(1) as f32);
        let pill_w = (icons_w + 28.0).max(80.0);

        let alpha = (self.settings.dock_opacity.clamp(0.1, 1.0) * 218.0) as u32;
        let pill_bg = if self.settings.dock_background == SurfaceBackground::None {
            rgba(0x00000000)
        } else {
            rgba(0x14171f00 | alpha)
        };
        let hovered_label = self
            .hovered_idx
            .and_then(|i| self.slots.get(i))
            .map(|s| s.label.clone());

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_end()
            .pb(px(bottom_gap))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _window, cx| {
                    this.handle_right_click(cx);
                }),
            )
            // Fixed-height background pill anchored to the bottom of the dock strip.
            .child(
                div()
                    .absolute()
                    .bottom(px(bottom_gap))
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
            // Icon row floats over the fixed-height pill so magnified icons pop OUT above it.
            .child(
                div()
                    .relative()
                    .flex()
                    .items_end()
                    .gap(px(gap))
                    .px(px(14.0))
                    .pb(px(5.0))
                    .children(self.slots.iter().enumerate().map(|(idx, slot)| {
                        let scale = slot.scale;
                        let dim = base * scale;
                        let lift = (scale - 1.0) * 12.0;
                        let icon = self.icons.get(&slot.icon_source).and_then(|o| o.clone());
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
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(3.0))
                            .cursor_pointer()
                            .on_hover(cx.listener(move |this, is_hovered, _window, cx| {
                                if *is_hovered {
                                    this.hovered_idx = Some(idx);
                                } else if this.hovered_idx == Some(idx) {
                                    this.hovered_idx = None;
                                }
                                this.ensure_animation(cx);
                                cx.notify();
                            }))
                            .on_click(move |_, _window, _cx| {
                                if !hwnds.is_empty() {
                                    tracker::activate_or_toggle(&hwnds);
                                } else {
                                    pins::launch(launch_path.clone());
                                }
                            })
                            .child(
                                div()
                                    .mb(px(lift))
                                    .w(px(dim))
                                    .h(px(dim))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(match icon {
                                        Some(image) => {
                                            img(image).w(px(dim)).h(px(dim)).into_any_element()
                                        }
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
            )
    }
}

pub fn run(settings: DockSettings) {
    let auto_hide = settings.auto_hide != AutoHideMode::Disabled;
    // Load initial pins and running windows BEFORE opening the window so it spawns
    // at its exact final position with zero startup jump.
    let initial_pins = pins::load_pins();
    let initial_running = tracker::enumerate_windows();
    let initial_slots = build_slots(&initial_pins, &initial_running, &[]);
    let (init_w, init_h) = compute_window_dips(&settings, initial_slots.len());

    Application::new().run(move |cx: &mut App| {
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
            let reserve_dips = settings_clone.icon_dips()
                + 34.0
                + settings_clone.dock_bottom_gap_pixels.max(0) as f32;
            let reserve_px = (reserve_dips * dpi).ceil() as i32;
            opentaskbar_shell::taskbar::begin_replacement(hwnd, auto_hide, reserve_px);
            win32::trim_working_set();
            cx.new(|cx| {
                DockView::new(
                    hwnd,
                    settings_clone,
                    initial_pins,
                    initial_slots,
                    window,
                    cx,
                )
            })
        });

        if let Err(err) = opened {
            opentaskbar_core::log::error(format!("Failed to open GPUI dock window: {err}"));
            cx.quit();
        }
    });
}
