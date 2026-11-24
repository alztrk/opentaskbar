//! GPUI floating dock window:
//! - Borderless transparent `WindowKind::PopUp` styled with Win32
//!   `WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`.
//! - Fisheye hover magnification, running indicators, click-to-launch/toggle,
//!   and asynchronous icon batch resolution via `icon_cache`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Application, Bounds, Context, Image, SharedString, Timer, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions, div,
    hsla, img, point, prelude::*, px, rgba, size,
};
use opentaskbar_core::settings::{AutoHideMode, DockPosition, DockSettings, SurfaceBackground};
use opentaskbar_shell::tracker::{self, RunningWindow};

use crate::icon_cache;
use crate::pins::{self, PinnedApp};
use crate::win32;

#[derive(Clone)]
struct DockSlot {
    label: SharedString,
    launch_path: String,
    icon_path: String,
    hwnd: Option<usize>,
    is_foreground: bool,
}

struct DockView {
    settings: DockSettings,
    pins: Vec<PinnedApp>,
    slots: Vec<DockSlot>,
    icons: HashMap<String, Option<Arc<Image>>>,
    hovered_idx: Option<usize>,
}

impl DockView {
    fn new(settings: DockSettings, cx: &mut Context<Self>) -> Self {
        let pins = pins::load_pins();
        let mut view = Self {
            settings,
            pins,
            slots: Vec::new(),
            icons: HashMap::new(),
            hovered_idx: None,
        };
        view.sync_slots(cx);
        view.start_window_poll(cx);
        view
    }

    fn sync_slots(&mut self, cx: &mut Context<Self>) {
        let running: Vec<RunningWindow> = tracker::enumerate_windows();
        let mut next: Vec<DockSlot> = Vec::with_capacity(self.pins.len() + running.len());

        for pin in &self.pins {
            let stem = pin.name.to_lowercase();
            let matched = running.iter().find(|w| {
                w.exe_path.to_lowercase().contains(&stem) || w.title.to_lowercase().contains(&stem)
            });
            next.push(DockSlot {
                label: pin.name.clone().into(),
                launch_path: pin.path.clone(),
                icon_path: matched
                    .map(|w| w.exe_path.clone())
                    .unwrap_or_else(|| pin.path.clone()),
                hwnd: matched.map(|w| w.hwnd),
                is_foreground: matched.is_some_and(|w| w.is_foreground),
            });
        }

        for win in &running {
            if next.iter().any(|s| s.hwnd == Some(win.hwnd)) {
                continue;
            }
            next.push(DockSlot {
                label: win.title.clone().into(),
                launch_path: win.exe_path.clone(),
                icon_path: win.exe_path.clone(),
                hwnd: Some(win.hwnd),
                is_foreground: win.is_foreground,
            });
        }

        let missing: Vec<String> = next
            .iter()
            .map(|s| s.icon_path.clone())
            .filter(|p| !self.icons.contains_key(p))
            .collect();

        self.slots = next;

        if !missing.is_empty() {
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
    }

    fn start_window_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(1000)).await;
                let updated = this.update(cx, |view, cx| {
                    view.sync_slots(cx);
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn scale_for(&self, idx: usize) -> f32 {
        if !self.settings.magnification_enabled || !self.settings.animations_enabled {
            return 1.0;
        }
        match self.hovered_idx {
            Some(hovered) => {
                let dist = idx.abs_diff(hovered);
                match dist {
                    0 => 1.42,
                    1 => 1.20,
                    2 => 1.08,
                    _ => 1.0,
                }
            }
            None => 1.0,
        }
    }
}

impl Render for DockView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let base = self.settings.icon_dips();
        let radius = self.settings.corner_radius_dips();
        let alpha = (self.settings.dock_opacity.clamp(0.1, 1.0) * 230.0) as u32;
        let pill_bg = if self.settings.dock_background == SurfaceBackground::None {
            rgba(0x00000000)
        } else {
            rgba(0x16181e00 | alpha)
        };

        div()
            .size_full()
            .flex()
            .items_end()
            .justify_center()
            .pb(px(self.settings.dock_bottom_gap_pixels.max(0) as f32))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(8.0 + self.settings.dock_icon_padding.max(0) as f32))
                    .px(px(14.0))
                    .py(px(8.0))
                    .rounded(px(radius))
                    .bg(pill_bg)
                    .when(
                        self.settings.dock_background != SurfaceBackground::None,
                        |el| el.border_1().border_color(rgba(0xffffff26)),
                    )
                    .children(self.slots.iter().enumerate().map(|(idx, slot)| {
                        let scale = self.scale_for(idx);
                        let dim = base * scale;
                        let lift = (scale - 1.0) * 14.0;
                        let icon = self.icons.get(&slot.icon_path).and_then(|o| o.clone());
                        let hwnd = slot.hwnd;
                        let launch_path = slot.launch_path.clone();
                        let running = hwnd.is_some();
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
                            .mb(px(lift))
                            .cursor_pointer()
                            .on_hover(cx.listener(move |this, is_hovered, _window, cx| {
                                if *is_hovered {
                                    this.hovered_idx = Some(idx);
                                } else if this.hovered_idx == Some(idx) {
                                    this.hovered_idx = None;
                                }
                                cx.notify();
                            }))
                            .on_click(move |_, _window, _cx| {
                                if let Some(hwnd) = hwnd {
                                    tracker::toggle_window(hwnd);
                                } else {
                                    pins::launch(launch_path.clone());
                                }
                            })
                            .child(match icon {
                                Some(image) => div()
                                    .w(px(dim))
                                    .h(px(dim))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(img(image).w(px(dim)).h(px(dim))),
                                None => div()
                                    .w(px(dim))
                                    .h(px(dim))
                                    .rounded(px(10.0))
                                    .bg(rgba(0x334155cc))
                                    .border_1()
                                    .border_color(hsla(0.0, 0.0, 1.0, 0.18))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .text_color(rgba(0xf8fafcff))
                                    .child(fallback_char),
                            })
                            .child(
                                div()
                                    .w(px(4.0))
                                    .h(px(4.0))
                                    .rounded(px(2.0))
                                    .bg(if !running {
                                        rgba(0x00000000)
                                    } else if active {
                                        rgba(0xffffffff)
                                    } else {
                                        rgba(0xcbd5e1aa)
                                    }),
                            )
                    }))
                    .child(
                        div()
                            .id("dock-quit")
                            .ml(px(6.0))
                            .mb(px(8.0))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(8.0))
                            .bg(rgba(0xffffff18))
                            .text_xs()
                            .text_color(rgba(0xe2e8f0ff))
                            .cursor_pointer()
                            .on_click(|_, _window, cx| cx.quit())
                            .child("Exit"),
                    ),
            )
    }
}

pub fn run(settings: DockSettings) {
    let auto_hide = settings.auto_hide != AutoHideMode::Disabled;
    Application::new().run(move |cx: &mut App| {
        let dock_w = px(960.0);
        let dock_h = px(108.0);
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
            let _ = win32::configure_dock_window(window);
            opentaskbar_shell::taskbar::begin_replacement(auto_hide);
            win32::trim_working_set();
            cx.new(|cx| DockView::new(settings_clone, cx))
        });

        if let Err(err) = opened {
            opentaskbar_core::log::error(format!("Failed to open GPUI dock window: {err}"));
            cx.quit();
        }
    });
}
