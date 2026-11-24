//! On-demand GPUI Settings window (`OpenTaskBar.exe --settings`).
//!
//! Runs in a short-lived subprocess so GPUI's DX11 context and RAM are 100% freed the moment
//! Settings is closed. Saving any option atomically writes `settings.json` and broadcasts
//! `OpenTaskBarReloadSettings` so all live DirectComposition docks reload immediately.

use gpui::{
    App, Application, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgba, size,
};
use opentaskbar_core::settings::{
    self, AppTheme, AutoHideMode, DockPosition, DockSettings, DockSize, SurfaceBackground,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, RegisterWindowMessageW, SendNotifyMessageW,
};

use crate::win32;

pub const SETTINGS_ARG: &str = "--settings";
pub const RELOAD_MSG_NAME: &str = "OpenTaskBarReloadSettings";

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Appearance,
    Layout,
    Behavior,
    Widgets,
}

impl SettingsTab {
    const ALL: [Self; 4] = [
        Self::Appearance,
        Self::Layout,
        Self::Behavior,
        Self::Widgets,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Layout => "Layout & Displays",
            Self::Behavior => "Behavior",
            Self::Widgets => "Widgets",
        }
    }
}

struct Palette {
    bg: u32,
    sidebar: u32,
    card: u32,
    card_border: u32,
    text_primary: u32,
    text_secondary: u32,
    accent: u32,
    accent_text: u32,
    pill_bg: u32,
    pill_hover: u32,
}

fn palette_for(theme: AppTheme) -> Palette {
    let dark = match theme {
        AppTheme::Dark => true,
        AppTheme::Light => false,
        AppTheme::System => win32::read_system_is_dark(),
    };
    if dark {
        Palette {
            bg: 0x13161eff,
            sidebar: 0x181c26ff,
            card: 0x1c212dff,
            card_border: 0xffffff18,
            text_primary: 0xf1f5f9ff,
            text_secondary: 0x94a3b8ff,
            accent: 0x0ea5e9ff,
            accent_text: 0xffffffff,
            pill_bg: 0xffffff10,
            pill_hover: 0xffffff1e,
        }
    } else {
        Palette {
            bg: 0xf5f6faff,
            sidebar: 0xebedf4ff,
            card: 0xffffffff,
            card_border: 0x0f172a18,
            text_primary: 0x0f172aff,
            text_secondary: 0x475569ff,
            accent: 0x0284c7ff,
            accent_text: 0xffffffff,
            pill_bg: 0x0f172a0d,
            pill_hover: 0x0f172a18,
        }
    }
}

fn notify_docks_reload() {
    let wide: Vec<u16> = RELOAD_MSG_NAME.encode_utf16().chain(Some(0)).collect();
    // SAFETY: broadcasting a registered application message to trigger dock settings reload.
    unsafe {
        let msg = RegisterWindowMessageW(wide.as_ptr());
        if msg != 0 {
            SendNotifyMessageW(HWND_BROADCAST, msg, 0, 0);
        }
    }
}

struct SettingsView {
    tab: SettingsTab,
    settings: DockSettings,
    status: Option<SharedString>,
}

impl SettingsView {
    fn new(settings: DockSettings) -> Self {
        Self {
            tab: SettingsTab::Appearance,
            settings,
            status: None,
        }
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        match settings::save_to(&settings::settings_path(), &self.settings) {
            Ok(()) => {
                notify_docks_reload();
                self.status = Some("Saved".into());
            }
            Err(err) => {
                self.status = Some(format!("Save failed: {err}").into());
            }
        }
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn toggle_row(
        &self,
        id: &'static str,
        title: &'static str,
        subtitle: &'static str,
        checked: bool,
        pal: &Palette,
        on_toggle: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(14.0))
            .py(px(11.0))
            .rounded(px(10.0))
            .bg(rgba(pal.card))
            .border_1()
            .border_color(rgba(pal.card_border))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgba(pal.text_primary))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgba(pal.text_secondary))
                            .child(subtitle),
                    ),
            )
            .child(
                div()
                    .id(id)
                    .w(px(40.0))
                    .h(px(22.0))
                    .rounded(px(11.0))
                    .p(px(2.0))
                    .flex()
                    .items_center()
                    .when(checked, |el| el.justify_end().bg(rgba(pal.accent)))
                    .when(!checked, |el| el.justify_start().bg(rgba(pal.pill_hover)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        on_toggle(this, cx);
                        this.persist(cx);
                    }))
                    .child(
                        div()
                            .w(px(18.0))
                            .h(px(18.0))
                            .rounded(px(9.0))
                            .bg(rgba(0xffffffff)),
                    ),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn segmented_row<T: Copy + PartialEq + 'static>(
        &self,
        title: &'static str,
        subtitle: &'static str,
        current: T,
        options: &[(&'static str, &'static str, T)],
        pal: &Palette,
        apply: fn(&mut DockSettings, T),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pills = options.iter().map(|&(id, label, val)| {
            let active = current == val;
            div()
                .id(id)
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(7.0))
                .text_xs()
                .cursor_pointer()
                .when(active, |el| {
                    el.bg(rgba(pal.accent)).text_color(rgba(pal.accent_text))
                })
                .when(!active, |el| {
                    el.bg(rgba(pal.pill_bg)).text_color(rgba(pal.text_primary))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    apply(&mut this.settings, val);
                    this.persist(cx);
                }))
                .child(label)
        });

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(11.0))
            .rounded(px(10.0))
            .bg(rgba(pal.card))
            .border_1()
            .border_color(rgba(pal.card_border))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgba(pal.text_primary))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgba(pal.text_secondary))
                            .child(subtitle),
                    ),
            )
            .child(div().flex().items_center().gap(px(4.0)).children(pills))
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette_for(self.settings.theme);

        let content = match self.tab {
            SettingsTab::Appearance => div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(self.segmented_row(
                    "Theme",
                    "Match Windows system appearance or lock a palette",
                    self.settings.theme,
                    &[
                        ("theme-sys", "System", AppTheme::System),
                        ("theme-dark", "Dark", AppTheme::Dark),
                        ("theme-light", "Light", AppTheme::Light),
                    ],
                    &pal,
                    |s, v| s.theme = v,
                    cx,
                ))
                .child(self.segmented_row(
                    "Dock Surface",
                    "Frosted glass, solid pill, or transparent icons only",
                    self.settings.dock_background,
                    &[
                        ("bg-glass", "Glass", SurfaceBackground::Glass),
                        ("bg-solid", "Solid", SurfaceBackground::Solid),
                        ("bg-none", "None", SurfaceBackground::None),
                    ],
                    &pal,
                    |s, v| s.dock_background = v,
                    cx,
                ))
                .child(self.segmented_row(
                    "Dock Size",
                    "Resting icon size and dock pill height",
                    self.settings.size,
                    &[
                        ("sz-compact", "Compact", DockSize::Compact),
                        ("sz-medium", "Medium", DockSize::Medium),
                        ("sz-large", "Large", DockSize::Large),
                    ],
                    &pal,
                    |s, v| s.size = v,
                    cx,
                ))
                .child(self.segmented_row(
                    "Background Opacity",
                    "Transparency of the dock pill surface",
                    (self.settings.dock_opacity * 100.0).round() as i32,
                    &[
                        ("op-70", "70%", 70),
                        ("op-82", "82%", 82),
                        ("op-88", "88%", 88),
                        ("op-96", "96%", 96),
                    ],
                    &pal,
                    |s, v| s.dock_opacity = f64::from(v) / 100.0,
                    cx,
                )),
            SettingsTab::Layout => div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(self.segmented_row(
                    "Horizontal Alignment",
                    "Position the floating dock along the screen edge",
                    self.settings.position,
                    &[
                        ("pos-left", "Left", DockPosition::Left),
                        ("pos-center", "Center", DockPosition::Center),
                        ("pos-right", "Right", DockPosition::Right),
                    ],
                    &pal,
                    |s, v| s.position = v,
                    cx,
                ))
                .child(self.toggle_row(
                    "tog-multimon",
                    "Show on All Monitors",
                    "Display a synchronized dock on every connected screen",
                    self.settings.show_on_all_monitors,
                    &pal,
                    |this, _| {
                        this.settings.show_on_all_monitors = !this.settings.show_on_all_monitors
                    },
                    cx,
                ))
                .child(self.toggle_row(
                    "tog-fullwidth",
                    "Full-Width Bar",
                    "Stretch the dock across the full monitor width",
                    self.settings.full_width,
                    &pal,
                    |this, _| this.settings.full_width = !this.settings.full_width,
                    cx,
                ))
                .child(self.toggle_row(
                    "tog-showdesktop",
                    "Show Desktop Button",
                    "Include a quick Show Desktop trigger on the right edge",
                    self.settings.show_desktop_button,
                    &pal,
                    |this, _| {
                        this.settings.show_desktop_button = !this.settings.show_desktop_button
                    },
                    cx,
                )),
            SettingsTab::Behavior => div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(self.segmented_row(
                    "Auto-Hide",
                    "Slide the dock off-screen when covered or idle",
                    self.settings.auto_hide,
                    &[
                        ("ah-off", "Off", AutoHideMode::Disabled),
                        ("ah-smart", "Smart", AutoHideMode::Smart),
                        ("ah-always", "Always", AutoHideMode::Always),
                    ],
                    &pal,
                    |s, v| s.auto_hide = v,
                    cx,
                ))
                .child(self.toggle_row(
                    "tog-mag",
                    "Fisheye Magnification",
                    "Smoothly magnify dock icons under the pointer",
                    self.settings.magnification_enabled,
                    &pal,
                    |this, _| {
                        this.settings.magnification_enabled = !this.settings.magnification_enabled
                    },
                    cx,
                ))
                .child(self.toggle_row(
                    "tog-anim",
                    "Animations",
                    "Enable spring motion, bounce feedback, and auto-hide glide",
                    self.settings.animations_enabled,
                    &pal,
                    |this, _| this.settings.animations_enabled = !this.settings.animations_enabled,
                    cx,
                )),
            SettingsTab::Widgets => div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(self.toggle_row(
                    "tog-clock",
                    "Clock & Date",
                    "Display local time and date on the right side of the dock",
                    self.settings.show_clock_widget,
                    &pal,
                    |this, _| this.settings.show_clock_widget = !this.settings.show_clock_widget,
                    cx,
                ))
                .child(self.toggle_row(
                    "tog-sysstats",
                    "System Stats (CPU / RAM)",
                    "Display lightweight live CPU and memory usage",
                    self.settings.show_system_stats_widget,
                    &pal,
                    |this, _| {
                        this.settings.show_system_stats_widget =
                            !this.settings.show_system_stats_widget
                    },
                    cx,
                ))
                .child(self.toggle_row(
                    "tog-recycle",
                    "Recycle Bin",
                    "Show the Recycle Bin icon on the dock",
                    self.settings.show_recycle_bin,
                    &pal,
                    |this, _| this.settings.show_recycle_bin = !this.settings.show_recycle_bin,
                    cx,
                )),
        };

        div()
            .size_full()
            .flex()
            .bg(rgba(pal.bg))
            .text_color(rgba(pal.text_primary))
            // Clean macOS-style sidebar.
            .child(
                div()
                    .w(px(192.0))
                    .h_full()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .p(px(14.0))
                    .bg(rgba(pal.sidebar))
                    .border_r_1()
                    .border_color(rgba(pal.card_border))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .px(px(10.0))
                                    .pb(px(10.0))
                                    .text_base()
                                    .text_color(rgba(pal.text_primary))
                                    .child("OpenTaskbar"),
                            )
                            .children(SettingsTab::ALL.into_iter().map(|tab| {
                                let selected = self.tab == tab;
                                div()
                                    .id(SharedString::from(format!("tab-{}", tab.label())))
                                    .px(px(10.0))
                                    .py(px(8.0))
                                    .rounded(px(8.0))
                                    .text_sm()
                                    .cursor_pointer()
                                    .when(selected, |el| {
                                        el.bg(rgba(pal.accent)).text_color(rgba(pal.accent_text))
                                    })
                                    .when(!selected, |el| el.text_color(rgba(pal.text_primary)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.tab = tab;
                                        cx.notify();
                                    }))
                                    .child(tab.label())
                            })),
                    )
                    .child(
                        div()
                            .px(px(10.0))
                            .text_xs()
                            .text_color(rgba(pal.text_secondary))
                            .child(
                                self.status
                                    .clone()
                                    .unwrap_or_else(|| "Hybrid DComp + GPUI".into()),
                            ),
                    ),
            )
            // Main settings pane.
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .p(px(22.0))
                    .gap(px(14.0))
                    .child(
                        div()
                            .text_lg()
                            .text_color(rgba(pal.text_primary))
                            .child(self.tab.label()),
                    )
                    .child(content),
            )
    }
}

pub fn run_settings_window() {
    let settings = settings::load_from(&settings::settings_path());
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(680.0), px(440.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("OpenTaskbar Settings".into()),
                ..Default::default()
            }),
            focus: true,
            show: true,
            is_movable: true,
            is_resizable: false,
            is_minimizable: true,
            ..Default::default()
        };
        let _ = cx.open_window(options, |_window, cx| {
            cx.new(|_| SettingsView::new(settings))
        });
    });
}
