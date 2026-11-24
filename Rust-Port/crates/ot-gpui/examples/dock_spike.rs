//! Phase 0 GPUI Windows spike:
//! - Opens a borderless, transparent, topmost, no-activate floating dock pill at
//!   the bottom center of the primary display.
//! - Applies Win32 dock styles via `ot_gpui::win32::configure_dock_window`.
//! - Prints startup time, HWND, and process private memory (MiB).
//! - Pass `--auto-close=N` to exit automatically after N seconds (for scripted
//!   verification).

use std::time::{Duration, Instant};

use gpui::{
    App, Application, Bounds, Context, SharedString, Timer, Window, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions, div, hsla, prelude::*, px, rgba,
    size,
};

struct DockSpike {
    hovered: Option<usize>,
    hwnd_label: SharedString,
    mem_label: SharedString,
    items: Vec<(&'static str, u32)>,
}

impl DockSpike {
    fn new(hwnd_label: String, mem_label: String) -> Self {
        Self {
            hovered: None,
            hwnd_label: hwnd_label.into(),
            mem_label: mem_label.into(),
            items: vec![
                ("Start", 0x3b82f6ff),
                ("Search", 0x6366f1ff),
                ("Files", 0xf59e0bff),
                ("Term", 0x10b981ff),
                ("Code", 0x0ea5e9ff),
                ("Settings", 0x64748bff),
            ],
        }
    }
}

impl Render for DockSpike {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hovered = self.hovered;
        div()
            .size_full()
            .flex()
            .items_end()
            .justify_center()
            .pb(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(14.0))
                    .py(px(8.0))
                    .rounded(px(18.0))
                    .bg(rgba(0x16181dcc))
                    .border_1()
                    .border_color(rgba(0xffffff26))
                    .children(self.items.iter().enumerate().map(|(idx, (label, color))| {
                        let is_hovered = hovered == Some(idx);
                        let dim = if is_hovered { 48.0 } else { 38.0 };
                        let shift = if is_hovered { -6.0 } else { 0.0 };
                        div()
                            .id(SharedString::from(format!("dock-item-{idx}")))
                            .w(px(dim))
                            .h(px(dim))
                            .mt(px(shift))
                            .rounded(px(11.0))
                            .bg(rgba(*color))
                            .border_1()
                            .border_color(hsla(0.0, 0.0, 1.0, 0.22))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(rgba(0xffffffff))
                            .cursor_pointer()
                            .on_hover(cx.listener(move |this, active, _window, cx| {
                                if *active {
                                    this.hovered = Some(idx);
                                } else if this.hovered == Some(idx) {
                                    this.hovered = None;
                                }
                                cx.notify();
                            }))
                            .child(*label)
                    }))
                    .child(
                        div()
                            .ml(px(8.0))
                            .pl(px(10.0))
                            .border_l_1()
                            .border_color(rgba(0xffffff24))
                            .flex()
                            .flex_col()
                            .text_xs()
                            .text_color(rgba(0xe2e8f0ff))
                            .child(self.mem_label.clone())
                            .child(
                                div()
                                    .text_color(rgba(0x94a3b8ff))
                                    .child(self.hwnd_label.clone()),
                            ),
                    )
                    .child(
                        div()
                            .id("spike-close")
                            .ml(px(6.0))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(8.0))
                            .bg(rgba(0xef4444cc))
                            .text_xs()
                            .text_color(rgba(0xffffffff))
                            .cursor_pointer()
                            .on_click(|_, _window, cx| cx.quit())
                            .child("X"),
                    ),
            )
    }
}

fn parse_auto_close_secs() -> Option<u64> {
    std::env::args()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--auto-close=")?.parse::<u64>().ok())
}

fn main() {
    let started = Instant::now();
    let auto_close = parse_auto_close_secs();

    Application::new().run(move |cx: &mut App| {
        let dock_size = size(px(520.0), px(76.0));
        let bounds = match cx.primary_display() {
            Some(display) => {
                let area = display.bounds();
                let x = area.origin.x + (area.size.width - dock_size.width) * 0.5;
                let y = area.origin.y + area.size.height - dock_size.height - px(48.0);
                Bounds::new(gpui::point(x, y), dock_size)
            }
            None => Bounds::centered(None, dock_size, cx),
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

        let opened = cx.open_window(options, |window, cx| {
            #[cfg(windows)]
            let hwnd_label = match ot_gpui::win32::configure_dock_window(window) {
                Some(hwnd) => format!("HWND 0x{hwnd:X}"),
                None => "HWND none".to_owned(),
            };
            #[cfg(not(windows))]
            let hwnd_label = "non-windows".to_owned();

            #[cfg(windows)]
            let mem_mib = ot_gpui::win32::process_private_memory_mib().unwrap_or(0.0);
            #[cfg(not(windows))]
            let mem_mib = 0.0;

            let mem_label = format!("{mem_mib:.1} MiB");
            println!(
                "gpui_spike_ready elapsed_ms={} private_mib={:.2} hwnd={}",
                started.elapsed().as_millis(),
                mem_mib,
                hwnd_label
            );

            cx.new(|_| DockSpike::new(hwnd_label, mem_label))
        });

        if let Err(err) = opened {
            eprintln!("Failed to open GPUI dock spike window: {err}");
            cx.quit();
            return;
        }

        if let Some(secs) = auto_close {
            cx.spawn(async move |cx| {
                Timer::after(Duration::from_secs(secs)).await;
                #[cfg(windows)]
                if let Some(mib) = ot_gpui::win32::process_private_memory_mib() {
                    println!("gpui_spike_settled private_mib={mib:.2}");
                }
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        }
    });
}
