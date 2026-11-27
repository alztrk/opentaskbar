//! GPUI top bar per monitor (C# `FjordBarWindow` + `FjordBarHost`).
//!
//! The host owns one session-scoped mutex, reconciles monitors on the 750 ms
//! tick, registers a top-edge AppBar per monitor, hides on fullscreen and shows
//! the Files / CloseBar menu, Wi-Fi flyout and culture-aware clock.
//!
//! NOTE: `ui/mod.rs` and `dock` wiring belong to the orchestrator. The bar is
//! intentionally independent of the dock's DirectComposition window so it can
//! be shown/hidden without restarting the taskbar replacement.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    App, Application, Bounds, Context, Entity, Render, SharedString, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*,
    px, rgba, size,
};
use opentaskbar_core::i18n as loc;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::design::fonts;
use crate::fjord::{activation, bar, wifi_ui};
use crate::platform::win32;
use crate::ui::components::{ButtonVariant, UiTheme, button, icon_glyph};
use crate::ui::menu::{self, MenuEntry, MenuIcon};

const MENU_FILES: u32 = 1;
const MENU_CLOSE_BAR: u32 = 2;
const WIFI_PANEL_W: f32 = 320.0;

/// Starts the bar host. Returns immediately if another host owns the session.
pub fn run_bar_host(files_enabled: bool, settings_path: PathBuf) {
    let Some(_instance) = bar::SingleInstance::acquire() else {
        return;
    };
    let availability =
        activation::FilesAvailability::new(settings_path, files_enabled).start(|enabled| {
            if !enabled {
                activation::close_all_windows();
            }
        });
    let cancel = Arc::new(AtomicBool::new(false));
    let clock_cancel = Arc::clone(&cancel);

    Application::new()
        .with_assets(fonts::EmbeddedAssets)
        .run(move |cx: &mut App| {
            let _ = fonts::load_bundled_fonts(cx);
            let clock = cx.spawn(async move |app| {
                while !clock_cancel.load(Ordering::SeqCst) {
                    let _ = app.update(|app| app.refresh_windows());
                    app.background_executor()
                        .timer(Duration::from_millis(crate::fjord::POLL_MS))
                        .await;
                }
            });
            drop(clock);

            let monitors = bar::poll_monitors(&cancel);
            for monitor in monitors {
                open_bar_window(monitor, availability.enabled(), cx);
            }

            let poll_cancel = Arc::clone(&cancel);
            let poll_availability = Arc::clone(&availability);
            let _poll = cx.spawn(async move |app| {
                let mut known: Vec<bar::MonitorSnapshot> = Vec::new();
                while !poll_cancel.load(Ordering::SeqCst) {
                    // `poll_monitors` itself waits 750 ms (cancellable).
                    let monitors = bar::poll_monitors(&poll_cancel);
                    let hidden: HashSet<usize> = monitors
                        .iter()
                        .filter(|monitor| bar::monitor_is_fullscreen(monitor.key))
                        .map(|monitor| monitor.key)
                        .collect();
                    for event in bar::reconcile(&mut known, &monitors, &hidden) {
                        match event {
                            bar::BarEvent::Open { key } => {
                                if let Some(monitor) = monitors.iter().find(|m| m.key == key) {
                                    let _ = app.update(|app| {
                                        open_bar_window(
                                            monitor.clone(),
                                            poll_availability.enabled(),
                                            app,
                                        )
                                    });
                                }
                            }
                            bar::BarEvent::Close { key } => {
                                let _ = app.update(|app| close_bar_window(key, app));
                            }
                            bar::BarEvent::SetHidden { key, hidden } => {
                                let _ = app.update(|app| set_hidden(key, hidden, app));
                            }
                            bar::BarEvent::SetBounds { key, bounds } => {
                                let _ = app.update(|app| set_bounds(key, bounds, app));
                            }
                        }
                    }
                    app.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                }
            });
        });
}

fn window_bounds(monitor: bar::MonitorSnapshot) -> Bounds<gpui::Pixels> {
    let rect = bar::bar_rect(monitor.bounds, 1.0, bar::BAR_HEIGHT_DIP);
    Bounds::new(
        point(px(rect.left as f32), px(rect.top as f32)),
        size(px(rect.width() as f32), px(rect.height() as f32)),
    )
}

/// GPUI windows cannot be looked up by monitor key, so a small registry maps
/// stable monitor handles to their entities (single-threaded on the UI loop).
type BarRegistry = std::collections::HashMap<usize, Entity<FjordBarView>>;

thread_local! {
    static BARS: std::cell::RefCell<BarRegistry> = std::cell::RefCell::new(BarRegistry::new());
}

fn with_registry<R>(action: impl FnOnce(&mut BarRegistry) -> R) -> R {
    BARS.with(|bars| action(&mut bars.borrow_mut()))
}

fn open_bar_window(monitor: bar::MonitorSnapshot, files_enabled: bool, cx: &mut App) {
    let _ = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(window_bounds(monitor.clone()))),
            window_min_size: Some(size(px(1.0), px(bar::BAR_HEIGHT_DIP))),
            is_resizable: false,
            is_movable: false,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            titlebar: None,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| FjordBarView::new(monitor.clone(), files_enabled, cx));
            let hwnd = match HasWindowHandle::window_handle(window) {
                Ok(handle) => match handle.as_raw() {
                    RawWindowHandle::Win32(raw) => raw.hwnd.get() as usize,
                    _ => 0,
                },
                Err(_) => 0,
            };
            view.update(cx, |view, cx| view.attach(hwnd, cx));
            with_registry(|bars| {
                bars.insert(monitor.key, view.clone());
            });
            view
        },
    );
}

fn close_bar_window(key: usize, cx: &mut App) {
    let view = with_registry(|bars| bars.remove(&key));
    if let Some(view) = view {
        view.update(cx, |view, cx| view.shutdown(cx));
    }
}

fn set_hidden(key: usize, hidden: bool, cx: &mut App) {
    let view = with_registry(|bars| bars.get(&key).cloned());
    if let Some(view) = view {
        view.update(cx, |view, cx| view.set_hidden(hidden, cx));
    }
}

fn set_bounds(key: usize, bounds: bar::Rect, cx: &mut App) {
    let view = with_registry(|bars| bars.get(&key).cloned());
    if let Some(view) = view {
        view.update(cx, |view, cx| view.set_bounds(bounds, cx));
    }
}

struct FjordBarView {
    monitor: bar::MonitorSnapshot,
    hwnd: usize,
    appbar: bar::AppBar,
    hidden: bool,
    files_enabled: bool,
    clock: String,
    theme: UiTheme,
    wifi: wifi_ui::WifiUiState,
    wifi_open: bool,
    wifi_busy: bool,
    wifi_password_focus: gpui::FocusHandle,
}

impl FjordBarView {
    fn new(monitor: bar::MonitorSnapshot, files_enabled: bool, cx: &mut Context<Self>) -> Self {
        Self {
            monitor,
            hwnd: 0,
            appbar: bar::AppBar::new(0, 0),
            hidden: false,
            files_enabled,
            clock: bar::local_clock(loc::language()),
            theme: UiTheme::new(win32::read_system_is_dark()),
            wifi: wifi_ui::WifiUiState::default(),
            wifi_open: false,
            wifi_busy: false,
            wifi_password_focus: cx.focus_handle(),
        }
    }

    fn attach(&mut self, hwnd: usize, _cx: &mut Context<Self>) {
        self.hwnd = hwnd;
        if hwnd == 0 {
            return;
        }
        let messages = bar::registered_messages().unwrap_or((0, 0));
        self.appbar = bar::AppBar::new(hwnd, messages.0);
        self.appbar.register();
        self.reposition();
    }

    fn reposition(&mut self) {
        if self.hwnd == 0 || self.hidden {
            return;
        }
        if let Some(rect) = self.appbar.position(self.monitor.bounds, 1.0) {
            bar::move_window(self.hwnd, rect);
        }
    }

    fn set_hidden(&mut self, hidden: bool, _cx: &mut Context<Self>) {
        if self.hidden == hidden {
            return;
        }
        self.hidden = hidden;
        if hidden {
            self.wifi_open = false;
        }
        bar::show_window(self.hwnd, !hidden);
        if !hidden {
            self.reposition();
        }
    }

    fn set_bounds(&mut self, bounds: bar::Rect, cx: &mut Context<Self>) {
        if self.monitor.bounds == bounds {
            return;
        }
        self.monitor.bounds = bounds;
        self.reposition();
        cx.notify();
    }

    fn shutdown(&mut self, cx: &mut Context<Self>) {
        self.appbar.unregister();
        if self.hwnd != 0 {
            bar::show_window(self.hwnd, false);
        }
        cx.notify();
    }

    /// Called by the dock when Explorer restarts (`TaskbarCreated`).
    pub fn on_taskbar_created(&mut self, cx: &mut Context<Self>) {
        self.appbar.unregister();
        self.appbar.register();
        self.reposition();
        cx.notify();
    }

    fn show_menu(&mut self, cx: &mut Context<Self>) {
        let mut entries = Vec::new();
        if self.files_enabled {
            entries.push(MenuEntry::item(
                MENU_FILES,
                MenuIcon::Folder,
                loc::t("FjordFilesOpen"),
            ));
        }
        entries.push(MenuEntry::destructive(
            MENU_CLOSE_BAR,
            MenuIcon::Close,
            loc::t("FjordBarHide"),
        ));
        let chosen = menu::show_popup_menu(self.hwnd, win32::read_system_is_dark(), entries);
        match chosen {
            Some(MENU_FILES) => {
                if activation::try_send("") {
                    // A running dock received the This PC request.
                } else {
                    // Local fallback opens the manager in this process.
                    super::fjord_files::run_files_window(None);
                }
            }
            Some(MENU_CLOSE_BAR) => {
                self.shutdown(cx);
            }
            _ => {}
        }
    }

    fn toggle_wifi(&mut self, cx: &mut Context<Self>) {
        self.wifi_open = !self.wifi_open;
        if self.wifi_open {
            self.scan_wifi(false, cx);
        }
        cx.notify();
    }

    fn scan_wifi(&mut self, rescan: bool, cx: &mut Context<Self>) {
        if self.wifi_busy {
            return;
        }
        self.wifi_busy = true;
        self.wifi.status = Some("WifiScan");
        cx.notify();
        let task = cx.background_spawn(async move {
            let _ = rescan; // WS-4 owns the actual scan trigger.
            wifi_ui::scan_networks()
        });
        drop(cx.spawn(async move |this, cx| {
            let networks = task.await;
            let _ = this.update(cx, |view, cx| {
                view.wifi_busy = false;
                view.wifi.apply_scan(networks);
                cx.notify();
            });
        }));
    }

    fn connect_wifi(&mut self, cx: &mut Context<Self>) {
        let Some(network) = self.wifi.selected_network().cloned() else {
            return;
        };
        let password = self.wifi.password.clone();
        let remember = self.wifi.remember;
        self.wifi_busy = true;
        self.wifi.status = Some("WifiConnect");
        cx.notify();
        let task = cx.background_spawn(async move {
            if network.connected {
                wifi_ui::disconnect();
                wifi_ui::ConnectionOutcome::Connected
            } else if wifi_ui::connect(&network.ssid, &password, remember) {
                wifi_ui::ConnectionOutcome::Connected
            } else {
                wifi_ui::ConnectionOutcome::Failed
            }
        });
        drop(cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |view, cx| {
                view.wifi_busy = false;
                view.wifi.password.clear();
                view.wifi.status = Some(outcome.status_key());
                cx.notify();
                view.scan_wifi(false, cx);
            });
        }));
    }

    fn wifi_row(
        &self,
        index: usize,
        network: &wifi_ui::NetworkView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let detail = if network.connected {
            loc::t("Connected")
        } else if network.has_profile {
            loc::t("Saved")
        } else {
            loc::t(network.security_key())
        };
        let signal = network.signal_class();
        div()
            .id(SharedString::from(format!("fjord-wifi-{index}")))
            .flex()
            .items_center()
            .justify_between()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .bg(rgba(if self.wifi.selected == Some(index) {
                t.accent_soft
            } else {
                0
            }))
            .text_color(rgba(t.text_primary))
            .hover(|style| style.bg(rgba(t.surface_hover)))
            .on_click(cx.listener(move |this: &mut Self, _, _, cx| {
                this.wifi.select(Some(index));
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(network.name.clone())
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgba(t.text_secondary))
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icon_glyph(signal.glyph(), 13.0, t.accent))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgba(t.text_secondary))
                            .child(signal.percent_text()),
                    ),
            )
    }

    fn wifi_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let password = self.wifi.password.clone();
        let status = self.wifi.status.map(loc::t);
        div()
            .absolute()
            .top(px(bar::BAR_HEIGHT_DIP + 4.0))
            .right(px(12.0))
            .w(px(WIFI_PANEL_W))
            .p(px(12.0))
            .rounded(px(10.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(t.border))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(13.0)).child(loc::t("WifiNetworks")))
                    .child(button(
                        "fjord-wifi-scan",
                        &t,
                        loc::t("WifiScan"),
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| this.scan_wifi(true, cx)),
                    )),
            )
            .when_some(status, |panel, status| {
                panel.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgba(t.text_secondary))
                        .child(status),
                )
            })
            .child(
                div()
                    .id("fjord-wifi-list")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .max_h(px(230.0))
                    .children(
                        self.wifi
                            .networks
                            .iter()
                            .enumerate()
                            .map(|(index, network)| {
                                self.wifi_row(index, network, cx).into_any_element()
                            }),
                    ),
            )
            .when(self.wifi.password_required(), |panel| {
                panel
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgba(t.text_secondary))
                            .child(loc::t("WifiPassword")),
                    )
                    .child(
                        div()
                            .id("fjord-wifi-password")
                            .track_focus(&self.wifi_password_focus)
                            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                                let mut buffer = this.wifi.password.clone();
                                if crate::ui::text_input::apply_keystroke(
                                    &mut buffer,
                                    &event.keystroke.key,
                                    event.keystroke.key_char.as_deref(),
                                    false,
                                ) {
                                    this.wifi.password = buffer;
                                    cx.notify();
                                }
                            }))
                            .px(px(8.0))
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .bg(rgba(t.surface_muted))
                            .border_1()
                            .border_color(rgba(t.border))
                            .child(password.clone()),
                    )
            })
            .child(
                div()
                    .when(!self.wifi.can_connect() || self.wifi_busy, |b| {
                        b.opacity(0.5)
                    })
                    .child(button(
                        "fjord-wifi-connect",
                        &t,
                        loc::t(self.wifi.action_key()),
                        ButtonVariant::Primary,
                        cx.listener(|this, _, _, cx| this.connect_wifi(cx)),
                    )),
            )
    }
}

impl Render for FjordBarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.clock = bar::local_clock(loc::language());
        let t = self.theme;
        let mut root = div()
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .h(px(bar::BAR_HEIGHT_DIP))
            .px(px(12.0))
            .bg(rgba(t.dock_preview_bg))
            .border_b_1()
            .border_color(rgba(t.border))
            .text_color(rgba(t.text_primary))
            .font_family("Segoe UI Variable Text, Segoe UI")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(button(
                        "fjord-bar-menu",
                        &t,
                        "◆",
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| this.show_menu(cx)),
                    ))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgba(t.text_secondary))
                            .child(self.monitor.display_label()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(button(
                        "fjord-bar-wifi",
                        &t,
                        loc::t("WifiNetworks"),
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| this.toggle_wifi(cx)),
                    ))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.clock.clone()),
                    ),
            );
        if self.wifi_open {
            root = root.child(self.wifi_panel(cx));
        }
        root
    }
}
