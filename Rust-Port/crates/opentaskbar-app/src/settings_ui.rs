//! On-demand GPUI Settings window (`OpenTaskBar.exe --settings`).
//!
//! Fixed-size (`880 x 600`) iOS + shadcn/ui window with native Segoe Fluent Icons,
//! zero horizontal overflow, visible custom scrollbar, interactive Widget Preview Cards
//! with inline widget settings, and instant auto-save + live DirectComposition dock reload.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, Application, Bounds, Context, ScrollHandle, SharedString, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, rgba, size,
};
use opentaskbar_core::settings::{
    self, AppTheme, AutoHideMode, DockAppGroup, DockPosition, DockProfile, DockSettings, DockSize,
    SurfaceBackground,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, RegisterWindowMessageW, SendNotifyMessageW,
};

use crate::pins;
use crate::ui_components::{
    self as ui, ButtonVariant, UiTheme, badge, button, icon_glyph, ios_switch, section_card,
    segmented_item, segmented_track, setting_row, sidebar_nav_item, stepper,
};
use crate::widgets;
use crate::win32;

pub const SETTINGS_ARG: &str = "--settings";
pub const RELOAD_MSG_NAME: &str = "OpenTaskBarReloadSettings";

const WIN_W: f32 = 880.0;
const WIN_H: f32 = 600.0;

pub fn spawn_settings_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg(SETTINGS_ARG).spawn();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    General,
    Appearance,
    Layout,
    Widgets,
    Stacks,
    Profiles,
    About,
}

impl SettingsPage {
    const ALL: [Self; 7] = [
        Self::General,
        Self::Appearance,
        Self::Layout,
        Self::Widgets,
        Self::Stacks,
        Self::Profiles,
        Self::About,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Layout => "Layout",
            Self::Widgets => "Widgets",
            Self::Stacks => "Stacks & Groups",
            Self::Profiles => "Profiles",
            Self::About => "About",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::General => "\u{E713}",    // Settings gear
            Self::Appearance => "\u{E790}", // Color palette
            Self::Layout => "\u{E7F4}",     // Display / monitor
            Self::Widgets => "\u{F0E2}",    // Grid / apps
            Self::Stacks => "\u{E8B7}",     // Folder
            Self::Profiles => "\u{E748}",   // Profile / switch
            Self::About => "\u{E946}",      // Info
        }
    }
}

fn widget_glyph(id: &str) -> &'static str {
    match id {
        "clock" => "\u{E823}",
        "stats" => "\u{E9D9}",
        "audio" => "\u{E767}",
        "media" => "\u{E8D6}",
        "netspeed" => "\u{E701}",
        "battery" => "\u{E83F}",
        "storage" => "\u{EDA2}",
        "keyboard" => "\u{E765}",
        "dayprogress" => "\u{E706}",
        "stopwatch" => "\u{E916}",
        "countdown" => "\u{E917}",
        "focus" => "\u{E708}",
        "weather" => "\u{E753}",
        "quicksettings" => "\u{E9E9}",
        _ => "\u{F0E2}",
    }
}

fn widget_preview_lines(id: &str, settings: &DockSettings) -> (String, String) {
    match id {
        "clock" => widgets::local_clock_strings(),
        "stats" => {
            let (ram, _, _) = widgets::sample_ram_info();
            ("CPU 12%".to_owned(), format!("RAM {ram}%"))
        }
        "audio" => {
            let (vol, muted) = widgets::get_audio_state();
            if muted {
                ("VOL --".to_owned(), "Muted".to_owned())
            } else {
                (format!("VOL {vol}%"), "Audio".to_owned())
            }
        }
        "media" => ("MEDIA".to_owned(), "Prev · Play · Next".to_owned()),
        "netspeed" => ("D 2.4M".to_owned(), "U 320K".to_owned()),
        "battery" => {
            let b = widgets::sample_battery();
            if b.has_battery {
                (format!("BAT {}%", b.percent), "Power".to_owned())
            } else {
                ("AC".to_owned(), "Desktop".to_owned())
            }
        }
        "storage" => {
            let d = widgets::sample_drives();
            if let Some(first) = d.first() {
                (
                    format!("{}: {:.0}G", first.letter, first.free_gb),
                    "Free".to_owned(),
                )
            } else {
                ("C: 120G".to_owned(), "Free".to_owned())
            }
        }
        "keyboard" => (widgets::active_keyboard_code(), "Layout".to_owned()),
        "dayprogress" => {
            let (day, _, _, left) = widgets::day_progress_metrics();
            (format!("DAY {:.0}%", day * 100.0), format!("{left}h left"))
        }
        "stopwatch" => ("00:00".to_owned(), "Stopwatch".to_owned()),
        "countdown" => ("15:00".to_owned(), "Timer".to_owned()),
        "focus" => (
            "25:00".to_owned(),
            if settings.focus_mode {
                "Focus On"
            } else {
                "Pomodoro"
            }
            .to_owned(),
        ),
        "weather" => ("21°C".to_owned(), settings.weather_city.clone()),
        "quicksettings" => ("QUICK".to_owned(), "Controls".to_owned()),
        _ => (id.to_uppercase(), "Widget".to_owned()),
    }
}

fn notify_docks_reload() {
    let wide: Vec<u16> = RELOAD_MSG_NAME.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let msg = RegisterWindowMessageW(wide.as_ptr());
        if msg != 0 {
            SendNotifyMessageW(HWND_BROADCAST, msg, 0, 0);
        }
    }
}

fn user_folder(name: &str) -> Option<String> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from)?;
    let path = home.join(name);
    path.exists().then(|| path.to_string_lossy().into_owned())
}

struct SettingsView {
    page: SettingsPage,
    settings: DockSettings,
    scroll_handle: ScrollHandle,
    saved_flash: bool,
}

impl SettingsView {
    fn new(settings: DockSettings) -> Self {
        Self {
            page: SettingsPage::General,
            settings,
            scroll_handle: ScrollHandle::new(),
            saved_flash: false,
        }
    }

    fn theme(&self) -> UiTheme {
        let is_dark = match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        };
        UiTheme::new(is_dark)
    }

    fn auto_save(&mut self, cx: &mut Context<Self>) {
        if settings::save_to(&settings::settings_path(), &self.settings).is_ok() {
            notify_docks_reload();
            self.saved_flash = true;
        }
        cx.notify();
    }

    fn render_general(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme_seg = segmented_track(
            t,
            vec![
                segmented_item(
                    "theme-sys",
                    t,
                    "System",
                    self.settings.theme == AppTheme::System,
                    cx.listener(|this, _, _, cx| {
                        this.settings.theme = AppTheme::System;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "theme-dark",
                    t,
                    "Dark",
                    self.settings.theme == AppTheme::Dark,
                    cx.listener(|this, _, _, cx| {
                        this.settings.theme = AppTheme::Dark;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "theme-light",
                    t,
                    "Light",
                    self.settings.theme == AppTheme::Light,
                    cx.listener(|this, _, _, cx| {
                        this.settings.theme = AppTheme::Light;
                        this.auto_save(cx);
                    }),
                ),
            ],
        );

        let autohide_seg = segmented_track(
            t,
            vec![
                segmented_item(
                    "ah-off",
                    t,
                    "Off",
                    self.settings.auto_hide == AutoHideMode::Disabled,
                    cx.listener(|this, _, _, cx| {
                        this.settings.auto_hide = AutoHideMode::Disabled;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "ah-smart",
                    t,
                    "Smart",
                    self.settings.auto_hide == AutoHideMode::Smart,
                    cx.listener(|this, _, _, cx| {
                        this.settings.auto_hide = AutoHideMode::Smart;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "ah-always",
                    t,
                    "Always",
                    self.settings.auto_hide == AutoHideMode::Always,
                    cx.listener(|this, _, _, cx| {
                        this.settings.auto_hide = AutoHideMode::Always;
                        this.auto_save(cx);
                    }),
                ),
            ],
        );

        vec![
            section_card(
                t,
                "System",
                vec![
                    setting_row(t, Some("\u{E790}"), "Theme", theme_seg).into_any_element(),
                    setting_row(t, Some("\u{E740}"), "Auto-Hide", autohide_seg).into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                "Behavior",
                vec![
                    setting_row(
                        t,
                        Some("\u{E708}"),
                        "Focus Mode",
                        ios_switch(
                            "sw-focus",
                            t,
                            self.settings.focus_mode,
                            cx.listener(|this, _, _, cx| {
                                this.settings.focus_mode = !this.settings.focus_mode;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E765}"),
                        "Reveal on Win Key",
                        ios_switch(
                            "sw-winkey",
                            t,
                            self.settings.reveal_dock_on_win_key,
                            cx.listener(|this, _, _, cx| {
                                this.settings.reveal_dock_on_win_key =
                                    !this.settings.reveal_dock_on_win_key;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E7F4}"),
                        "Current Desktop Only",
                        ios_switch(
                            "sw-vdesk",
                            t,
                            self.settings.current_desktop_only,
                            cx.listener(|this, _, _, cx| {
                                this.settings.current_desktop_only =
                                    !this.settings.current_desktop_only;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    fn render_appearance(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let bg_seg = segmented_track(
            t,
            vec![
                segmented_item(
                    "bg-glass",
                    t,
                    "Glass",
                    self.settings.dock_background == SurfaceBackground::Glass,
                    cx.listener(|this, _, _, cx| {
                        this.settings.dock_background = SurfaceBackground::Glass;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "bg-solid",
                    t,
                    "Solid",
                    self.settings.dock_background == SurfaceBackground::Solid,
                    cx.listener(|this, _, _, cx| {
                        this.settings.dock_background = SurfaceBackground::Solid;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "bg-none",
                    t,
                    "None",
                    self.settings.dock_background == SurfaceBackground::None,
                    cx.listener(|this, _, _, cx| {
                        this.settings.dock_background = SurfaceBackground::None;
                        this.auto_save(cx);
                    }),
                ),
            ],
        );

        let size_seg = segmented_track(
            t,
            vec![
                segmented_item(
                    "sz-compact",
                    t,
                    "Compact",
                    self.settings.size == DockSize::Compact,
                    cx.listener(|this, _, _, cx| {
                        this.settings.size = DockSize::Compact;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "sz-med",
                    t,
                    "Medium",
                    self.settings.size == DockSize::Medium,
                    cx.listener(|this, _, _, cx| {
                        this.settings.size = DockSize::Medium;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "sz-large",
                    t,
                    "Large",
                    self.settings.size == DockSize::Large,
                    cx.listener(|this, _, _, cx| {
                        this.settings.size = DockSize::Large;
                        this.auto_save(cx);
                    }),
                ),
            ],
        );

        let opacity_pct = (self.settings.dock_opacity * 100.0).round() as i32;
        let scale_pct = (self.settings.dock_scale * 100.0).round() as i32;
        let radius_label = self
            .settings
            .dock_corner_radius
            .map_or_else(|| "Auto".to_owned(), |r| format!("{r} px"));

        vec![
            section_card(
                t,
                "Surface",
                vec![
                    setting_row(t, Some("\u{E790}"), "Background", bg_seg).into_any_element(),
                    setting_row(t, Some("\u{E740}"), "Dock Size", size_seg).into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E706}"),
                        "Opacity",
                        stepper(
                            "op-minus",
                            "op-plus",
                            t,
                            format!("{opacity_pct}%"),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_opacity =
                                    (this.settings.dock_opacity - 0.05).clamp(0.2, 1.0);
                                this.auto_save(cx);
                            }),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_opacity =
                                    (this.settings.dock_opacity + 0.05).clamp(0.2, 1.0);
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E8A3}"),
                        "Scale",
                        stepper(
                            "sc-minus",
                            "sc-plus",
                            t,
                            format!("{scale_pct}%"),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_scale =
                                    ((this.settings.dock_scale * 20.0).round() - 1.0)
                                        .clamp(15.0, 30.0)
                                        / 20.0;
                                this.auto_save(cx);
                            }),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_scale =
                                    ((this.settings.dock_scale * 20.0).round() + 1.0)
                                        .clamp(15.0, 30.0)
                                        / 20.0;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{F158}"),
                        "Corner Radius",
                        stepper(
                            "rad-minus",
                            "rad-plus",
                            t,
                            radius_label,
                            cx.listener(|this, _, _, cx| {
                                let cur = this.settings.dock_corner_radius.unwrap_or(16);
                                this.settings.dock_corner_radius = Some((cur - 2).max(0));
                                this.auto_save(cx);
                            }),
                            cx.listener(|this, _, _, cx| {
                                let cur = this.settings.dock_corner_radius.unwrap_or(16);
                                this.settings.dock_corner_radius = Some((cur + 2).min(32));
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E74A}"),
                        "Bottom Margin",
                        stepper(
                            "gap-minus",
                            "gap-plus",
                            t,
                            format!("{} px", self.settings.dock_bottom_gap_pixels),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_bottom_gap_pixels =
                                    (this.settings.dock_bottom_gap_pixels - 2).max(0);
                                this.auto_save(cx);
                            }),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_bottom_gap_pixels =
                                    (this.settings.dock_bottom_gap_pixels + 2).min(32);
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                "Motion",
                vec![
                    setting_row(
                        t,
                        Some("\u{E945}"),
                        "Smooth Animations",
                        ios_switch(
                            "sw-anim",
                            t,
                            self.settings.animations_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.animations_enabled =
                                    !this.settings.animations_enabled;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E71E}"),
                        "Magnification",
                        ios_switch(
                            "sw-mag",
                            t,
                            self.settings.magnification_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.magnification_enabled =
                                    !this.settings.magnification_enabled;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    fn render_layout(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let align_seg = segmented_track(
            t,
            vec![
                segmented_item(
                    "pos-left",
                    t,
                    "Left",
                    self.settings.position == DockPosition::Left,
                    cx.listener(|this, _, _, cx| {
                        this.settings.position = DockPosition::Left;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "pos-center",
                    t,
                    "Center",
                    self.settings.position == DockPosition::Center,
                    cx.listener(|this, _, _, cx| {
                        this.settings.position = DockPosition::Center;
                        this.auto_save(cx);
                    }),
                ),
                segmented_item(
                    "pos-right",
                    t,
                    "Right",
                    self.settings.position == DockPosition::Right,
                    cx.listener(|this, _, _, cx| {
                        this.settings.position = DockPosition::Right;
                        this.auto_save(cx);
                    }),
                ),
            ],
        );

        let width_label = self.settings.dock_custom_width_dips.map_or_else(
            || "Width (Auto)".to_owned(),
            |w| format!("Width ({w:.0}px)"),
        );

        vec![
            section_card(
                t,
                "Position & Displays",
                vec![
                    setting_row(t, Some("\u{E8E3}"), "Alignment", align_seg).into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E740}"),
                        "Full Width",
                        ios_switch(
                            "sw-fullw",
                            t,
                            self.settings.full_width,
                            cx.listener(|this, _, _, cx| {
                                this.settings.full_width = !this.settings.full_width;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E72C}"),
                        width_label,
                        button(
                            "btn-reset-width",
                            t,
                            "Reset",
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_custom_width_dips = None;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E7F4}"),
                        "All Monitors",
                        ios_switch(
                            "sw-allmon",
                            t,
                            self.settings.show_on_all_monitors,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_on_all_monitors =
                                    !this.settings.show_on_all_monitors;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                "Dock Utilities",
                vec![
                    setting_row(
                        t,
                        Some("\u{E70E}"),
                        "Background Apps",
                        ios_switch(
                            "sw-tray",
                            t,
                            self.settings.show_tray_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_tray_button = !this.settings.show_tray_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E74D}"),
                        "Recycle Bin",
                        ios_switch(
                            "sw-bin",
                            t,
                            self.settings.show_recycle_bin,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_recycle_bin = !this.settings.show_recycle_bin;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E7F8}"),
                        "Show Desktop Strip",
                        ios_switch(
                            "sw-sd",
                            t,
                            self.settings.show_desktop_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_desktop_button =
                                    !this.settings.show_desktop_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    fn render_widget_card(
        &self,
        w: &'static widgets::WidgetDescriptor,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wid = w.id;
        let enabled = self.settings.is_widget_enabled(wid);
        let (top_line, bot_line) = widget_preview_lines(wid, &self.settings);

        let border_col = if enabled { t.accent } else { t.border };
        let card_hover = t.surface_hover;

        // Inline widget-specific settings right inside the preview card
        let inline_controls: Option<AnyElement> =
            match wid {
                "weather" => Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .flex_wrap()
                        .children(["Istanbul", "Ankara", "London", "Berlin"].into_iter().map(
                            |city| {
                                let active = self.settings.weather_city.eq_ignore_ascii_case(city);
                                let bg = if active { t.accent } else { t.surface_muted };
                                let fg = if active {
                                    t.accent_fg
                                } else {
                                    t.text_secondary
                                };
                                div()
                                    .id(SharedString::from(format!("wc-{city}")))
                                    .px(px(7.0))
                                    .py(px(2.5))
                                    .rounded(px(6.0))
                                    .bg(rgba(bg))
                                    .text_size(px(10.5))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(rgba(fg))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.settings.weather_city = city.to_owned();
                                        this.auto_save(cx);
                                    }))
                                    .child(city)
                            },
                        ))
                        .into_any_element(),
                ),
                "focus" => {
                    let on = self.settings.focus_mode;
                    let bg = if on { t.accent_soft } else { t.surface_muted };
                    let fg = if on { t.accent } else { t.text_secondary };
                    Some(
                        div()
                            .id("wc-focus-toggle")
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(6.0))
                            .bg(rgba(bg))
                            .text_size(px(10.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(rgba(fg))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.settings.focus_mode = !this.settings.focus_mode;
                                this.auto_save(cx);
                            }))
                            .child(if on {
                                "Focus Mode: On"
                            } else {
                                "Focus Mode: Off"
                            })
                            .into_any_element(),
                    )
                }
                "audio" => Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            div()
                                .id("wc-vol-dn")
                                .px(px(8.0))
                                .py(px(2.5))
                                .rounded(px(6.0))
                                .bg(rgba(t.surface_muted))
                                .text_size(px(10.5))
                                .text_color(rgba(t.text_primary))
                                .cursor_pointer()
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.stop_propagation();
                                    widgets::adjust_audio_volume(-10);
                                    cx.notify();
                                }))
                                .child("-10%"),
                        )
                        .child(
                            div()
                                .id("wc-vol-mute")
                                .px(px(8.0))
                                .py(px(2.5))
                                .rounded(px(6.0))
                                .bg(rgba(t.surface_muted))
                                .text_size(px(10.5))
                                .text_color(rgba(t.text_primary))
                                .cursor_pointer()
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.stop_propagation();
                                    widgets::toggle_audio_mute();
                                    cx.notify();
                                }))
                                .child("Mute"),
                        )
                        .child(
                            div()
                                .id("wc-vol-up")
                                .px(px(8.0))
                                .py(px(2.5))
                                .rounded(px(6.0))
                                .bg(rgba(t.surface_muted))
                                .text_size(px(10.5))
                                .text_color(rgba(t.text_primary))
                                .cursor_pointer()
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.stop_propagation();
                                    widgets::adjust_audio_volume(10);
                                    cx.notify();
                                }))
                                .child("+10%"),
                        )
                        .into_any_element(),
                ),
                _ => None,
            };

        let mut card = div()
            .id(SharedString::from(format!("wcard-{wid}")))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(12.0))
            .rounded(px(12.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(border_col))
            .cursor_pointer()
            .hover(move |s| s.bg(rgba(card_hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                let cur = this.settings.is_widget_enabled(wid);
                this.settings.set_widget_enabled(wid, !cur);
                this.auto_save(cx);
            }))
            // Live Dock Chip Preview
            .child(
                div()
                    .w_full()
                    .h(px(54.0))
                    .rounded(px(9.0))
                    .bg(rgba(t.dock_preview_bg))
                    .border_1()
                    .border_color(rgba(0xffffff18))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .px(px(12.0))
                            .py(px(5.0))
                            .rounded(px(7.0))
                            .bg(rgba(0xffffff10))
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(0xf8fafcff))
                                    .child(top_line),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(rgba(0x94a3b8ff))
                                    .child(bot_line),
                            ),
                    ),
            )
            // Card footer: Icon + Name + Active indicator
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .min_w_0()
                            .child(icon_glyph(
                                widget_glyph(wid),
                                13.5,
                                if enabled { t.accent } else { t.text_secondary },
                            ))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.text_primary))
                                    .child(w.name),
                            ),
                    )
                    .child(if enabled {
                        badge(t, "On", true)
                    } else {
                        badge(t, "Add", false)
                    }),
            );

        if let Some(inline) = inline_controls {
            card = card.child(
                div()
                    .w_full()
                    .pt(px(4.0))
                    .border_t_1()
                    .border_color(rgba(t.separator))
                    .child(inline),
            );
        }

        card.into_any_element()
    }

    fn render_widgets(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        let active_count = widgets::ALL_WIDGETS
            .iter()
            .filter(|w| self.settings.is_widget_enabled(w.id))
            .count();

        out.push(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .child(badge(
                    t,
                    format!("{active_count} / {} active", widgets::ALL_WIDGETS.len()),
                    active_count > 0,
                ))
                .child(button(
                    "btn-clear-widgets",
                    t,
                    "Clear All",
                    ButtonVariant::Secondary,
                    cx.listener(|this, _, _, cx| {
                        this.settings.show_clock_widget = false;
                        this.settings.show_system_stats_widget = false;
                        this.settings.enabled_widgets.clear();
                        this.auto_save(cx);
                    }),
                ))
                .into_any_element(),
        );

        // Strict 2-column rows so cards never overflow horizontally
        for pair in widgets::ALL_WIDGETS.chunks(2) {
            let mut row = div().w_full().flex().gap(px(10.0));
            for w in pair {
                row = row.child(self.render_widget_card(w, t, cx));
            }
            if pair.len() == 1 {
                row = row.child(div().flex_1());
            }
            out.push(row.into_any_element());
        }
        out
    }

    fn render_stacks(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut stack_rows: Vec<AnyElement> = self
            .settings
            .stack_folders
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let name = std::path::Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path)
                    .to_owned();
                setting_row(
                    t,
                    Some("\u{E8B7}"),
                    name,
                    button(
                        SharedString::from(format!("rm-stack-{i}")),
                        t,
                        "Remove",
                        ButtonVariant::Destructive,
                        cx.listener(move |this, _, _, cx| {
                            if i < this.settings.stack_folders.len() {
                                this.settings.stack_folders.remove(i);
                                this.auto_save(cx);
                            }
                        }),
                    ),
                )
                .into_any_element()
            })
            .collect();

        let quick_add = div().flex().items_center().gap(px(6.0)).children(
            ["Downloads", "Desktop", "Documents"]
                .into_iter()
                .map(|folder| {
                    button(
                        SharedString::from(format!("add-st-{folder}")),
                        t,
                        format!("+ {folder}"),
                        ButtonVariant::Secondary,
                        cx.listener(move |this, _, _, cx| {
                            if let Some(p) = user_folder(folder)
                                && !this
                                    .settings
                                    .stack_folders
                                    .iter()
                                    .any(|x| x.eq_ignore_ascii_case(&p))
                            {
                                this.settings.stack_folders.push(p);
                                this.auto_save(cx);
                            }
                        }),
                    )
                }),
        );
        stack_rows
            .push(setting_row(t, Some("\u{E710}"), "Add Folder", quick_add).into_any_element());

        let mut group_rows: Vec<AnyElement> = self
            .settings
            .app_groups
            .iter()
            .enumerate()
            .map(|(i, g)| {
                setting_row(
                    t,
                    Some("\u{F0E2}"),
                    format!("{} ({})", g.name, g.executable_paths.len()),
                    button(
                        SharedString::from(format!("rm-grp-{i}")),
                        t,
                        "Remove",
                        ButtonVariant::Destructive,
                        cx.listener(move |this, _, _, cx| {
                            if i < this.settings.app_groups.len() {
                                this.settings.app_groups.remove(i);
                                this.auto_save(cx);
                            }
                        }),
                    ),
                )
                .into_any_element()
            })
            .collect();

        group_rows.push(
            setting_row(
                t,
                Some("\u{E710}"),
                "Starter Group",
                button(
                    "add-grp-prod",
                    t,
                    "+ Productivity",
                    ButtonVariant::Secondary,
                    cx.listener(|this, _, _, cx| {
                        if !this
                            .settings
                            .app_groups
                            .iter()
                            .any(|g| g.name.eq_ignore_ascii_case("Productivity"))
                        {
                            this.settings.app_groups.push(DockAppGroup {
                                id: "productivity".to_owned(),
                                name: "Productivity".to_owned(),
                                executable_paths: vec![
                                    "explorer.exe".to_owned(),
                                    "wt.exe".to_owned(),
                                ],
                            });
                            this.auto_save(cx);
                        }
                    }),
                ),
            )
            .into_any_element(),
        );

        vec![
            section_card(t, "Folder Stacks", stack_rows).into_any_element(),
            section_card(t, "App Groups", group_rows).into_any_element(),
        ]
    }

    fn render_profiles(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut prof_rows: Vec<AnyElement> = self
            .settings
            .profiles
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let is_active = self
                    .settings
                    .active_profile
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(&p.name));
                let controls = div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(button(
                        SharedString::from(format!("apply-prof-{i}")),
                        t,
                        if is_active { "Active" } else { "Apply" },
                        if is_active {
                            ButtonVariant::Primary
                        } else {
                            ButtonVariant::Secondary
                        },
                        cx.listener(move |this, _, _, cx| {
                            if let Some(prof) = this.settings.profiles.get(i).cloned() {
                                this.settings.position = prof.position;
                                this.settings.size = prof.size;
                                this.settings.auto_hide = prof.auto_hide;
                                this.settings.enabled_widgets = prof.enabled_widgets.clone();
                                this.settings.show_clock_widget =
                                    prof.enabled_widgets.iter().any(|w| w == "clock");
                                this.settings.show_system_stats_widget =
                                    prof.enabled_widgets.iter().any(|w| w == "stats");
                                this.settings.active_profile = Some(prof.name.clone());
                                this.auto_save(cx);
                            }
                        }),
                    ))
                    .child(button(
                        SharedString::from(format!("del-prof-{i}")),
                        t,
                        "Delete",
                        ButtonVariant::Destructive,
                        cx.listener(move |this, _, _, cx| {
                            if i < this.settings.profiles.len() {
                                this.settings.profiles.remove(i);
                                this.auto_save(cx);
                            }
                        }),
                    ));
                setting_row(t, Some("\u{E748}"), p.name.clone(), controls).into_any_element()
            })
            .collect();

        let save_presets = div().flex().items_center().gap(px(6.0)).children(
            ["Work", "Gaming", "Minimal"].into_iter().map(|preset| {
                button(
                    SharedString::from(format!("save-prof-{preset}")),
                    t,
                    preset,
                    ButtonVariant::Secondary,
                    cx.listener(move |this, _, _, cx| {
                        let snap = DockProfile {
                            name: preset.to_owned(),
                            position: this.settings.position,
                            size: this.settings.size,
                            auto_hide: this.settings.auto_hide,
                            enabled_widgets: this.settings.enabled_widgets.clone(),
                        };
                        this.settings
                            .profiles
                            .retain(|p| !p.name.eq_ignore_ascii_case(preset));
                        this.settings.profiles.push(snap);
                        this.settings.active_profile = Some(preset.to_owned());
                        this.auto_save(cx);
                    }),
                )
            }),
        );
        prof_rows.push(
            setting_row(t, Some("\u{E74E}"), "Save Current As", save_presets).into_any_element(),
        );

        vec![
            section_card(t, "Profiles", prof_rows).into_any_element(),
            section_card(
                t,
                "Backup",
                vec![
                    setting_row(
                        t,
                        Some("\u{EDE1}"),
                        "Settings Backup",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(button(
                                "btn-export",
                                t,
                                "Export",
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, _, cx| {
                                    let dst = settings::app_data_dir().join("settings.backup.json");
                                    let _ = settings::save_to(&dst, &this.settings);
                                    this.saved_flash = true;
                                    cx.notify();
                                }),
                            ))
                            .child(button(
                                "btn-import",
                                t,
                                "Restore",
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, _, cx| {
                                    let src = settings::app_data_dir().join("settings.backup.json");
                                    if src.exists() {
                                        this.settings = settings::load_from(&src);
                                        this.auto_save(cx);
                                    }
                                }),
                            )),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E777}"),
                        "Factory Defaults",
                        button(
                            "btn-reset-all",
                            t,
                            "Reset",
                            ButtonVariant::Destructive,
                            cx.listener(|this, _, _, cx| {
                                this.settings = DockSettings::default();
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    fn render_about(&self, t: &UiTheme) -> Vec<AnyElement> {
        let data_dir = settings::app_data_dir().to_string_lossy().into_owned();
        vec![
            section_card(
                t,
                "OpenTaskBar",
                vec![
                    setting_row(
                        t,
                        Some("\u{E946}"),
                        "Version",
                        badge(t, "0.1.0 Hybrid", true),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E8B7}"),
                        "Data Folder",
                        button(
                            "btn-open-data",
                            t,
                            "Open",
                            ButtonVariant::Secondary,
                            move |_, _, _| {
                                pins::launch(data_dir.clone());
                            },
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E777}"),
                        "Windows Taskbar",
                        button(
                            "btn-restore-tb",
                            t,
                            "Restore",
                            ButtonVariant::Destructive,
                            |_, _, _| {
                                opentaskbar_shell::taskbar::restore_from_journal();
                            },
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme();
        let active_widgets_count = widgets::ALL_WIDGETS
            .iter()
            .filter(|w| self.settings.is_widget_enabled(w.id))
            .count();

        let nav_items: Vec<AnyElement> = SettingsPage::ALL
            .into_iter()
            .map(|p| {
                let count_badge = match p {
                    SettingsPage::Widgets => Some(format!(
                        "{active_widgets_count}/{}",
                        widgets::ALL_WIDGETS.len()
                    )),
                    SettingsPage::Profiles if !self.settings.profiles.is_empty() => {
                        Some(self.settings.profiles.len().to_string())
                    }
                    _ => None,
                };
                sidebar_nav_item(
                    SharedString::from(format!("nav-{}", p.title())),
                    &t,
                    p.glyph(),
                    p.title(),
                    count_badge,
                    self.page == p,
                    cx.listener(move |this, _, _, cx| {
                        this.page = p;
                        this.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
                        cx.notify();
                    }),
                )
                .into_any_element()
            })
            .collect();

        let content_blocks = match self.page {
            SettingsPage::General => self.render_general(&t, cx),
            SettingsPage::Appearance => self.render_appearance(&t, cx),
            SettingsPage::Layout => self.render_layout(&t, cx),
            SettingsPage::Widgets => self.render_widgets(&t, cx),
            SettingsPage::Stacks => self.render_stacks(&t, cx),
            SettingsPage::Profiles => self.render_profiles(&t, cx),
            SettingsPage::About => self.render_about(&t),
        };

        // Visible scrollbar thumb calculated from ScrollHandle
        let max_off: f32 = self.scroll_handle.max_offset().height.into();
        let cur_off: f32 = (-self.scroll_handle.offset().y).into();
        let viewport_h: f32 = self.scroll_handle.bounds().size.height.into();
        let scrollbar_el = if max_off > 2.0 && viewport_h > 40.0 {
            let total_h = viewport_h + max_off;
            let track_h = (viewport_h - 16.0).max(40.0);
            let thumb_h = ((viewport_h / total_h) * track_h).clamp(28.0, track_h);
            let ratio = (cur_off / max_off).clamp(0.0, 1.0);
            let thumb_top = 8.0 + ratio * (track_h - thumb_h);
            Some(
                div()
                    .absolute()
                    .top(px(thumb_top))
                    .right(px(5.0))
                    .w(px(5.0))
                    .h(px(thumb_h))
                    .rounded(px(3.0))
                    .bg(rgba(t.scrollbar_thumb)),
            )
        } else {
            None
        };

        let sidebar = div()
            .w(px(208.0))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .justify_between()
            .p(px(12.0))
            .bg(rgba(t.sidebar_bg))
            .border_r_1()
            .border_color(rgba(t.border))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .px(px(10.0))
                            .pt(px(6.0))
                            .pb(px(12.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(14.5))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgba(t.text_primary))
                                    .child("OpenTaskBar"),
                            )
                            .child(ui::badge(&t, "v0.1", true)),
                    )
                    .children(nav_items),
            )
            .child(
                div()
                    .px(px(10.0))
                    .py(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded(px(3.0))
                            .bg(rgba(t.switch_on)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgba(t.text_muted))
                            .child(if self.saved_flash {
                                "Auto-saved"
                            } else {
                                "Auto-save on"
                            }),
                    ),
            );

        let mut scroll_wrap = div().flex_1().min_h_0().w_full().relative().child(
            div()
                .id("settings-scroll")
                .w_full()
                .h_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_handle)
                .on_scroll_wheel(cx.listener(|_, _, _, cx| {
                    cx.notify();
                }))
                .px(px(22.0))
                .py(px(16.0))
                .flex()
                .flex_col()
                .gap(px(14.0))
                .children(content_blocks),
        );

        if let Some(sb) = scrollbar_el {
            scroll_wrap = scroll_wrap.child(sb);
        }

        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgba(t.canvas_bg))
            .child(
                div()
                    .w_full()
                    .px(px(22.0))
                    .py(px(16.0))
                    .border_b_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(icon_glyph(self.page.glyph(), 17.0, t.accent))
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgba(t.text_primary))
                            .child(self.page.title()),
                    ),
            )
            .child(scroll_wrap);

        div()
            .w_full()
            .h_full()
            .flex()
            .overflow_hidden()
            .bg(rgba(t.canvas_bg))
            .text_color(rgba(t.text_primary))
            .child(sidebar)
            .child(main)
    }
}

pub fn run_settings_window() {
    let loaded = settings::load_from(&settings::settings_path());
    Application::new().run(move |cx: &mut App| {
        let fixed = size(px(WIN_W), px(WIN_H));
        let bounds = Bounds::centered(None, fixed, cx);
        let _ = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                is_resizable: false,
                window_min_size: Some(fixed),
                ..Default::default()
            },
            |_, cx| cx.new(|_| SettingsView::new(loaded.clone())),
        );
        cx.activate(true);
    });
}
