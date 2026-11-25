//! On-demand GPUI Settings window (`OpenTaskBar.exe --settings`).
//!
//! Grouped settings with a responsive navigation pane and immediate dock updates.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, Application, Bounds, Context, FocusHandle, KeyBinding, ScrollHandle,
    SharedString, TitlebarOptions, Window, WindowBounds, WindowControlArea, WindowOptions, actions,
    div, prelude::*, px, rgba, size,
};
use opentaskbar_core::settings::{
    self, AppTheme, AutoHideMode, DockAppGroup, DockPosition, DockProfile, DockSettings, DockSize,
    SurfaceBackground,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, RegisterWindowMessageW, SendNotifyMessageW,
};

use crate::dock::dcomp::WidgetChipSpec;
use crate::platform::{pins, win32};
use crate::ui::components::{
    ButtonVariant, UiTheme, badge, button, icon_glyph, ios_switch, section_card, segmented_item,
    segmented_track, setting_row, sidebar_nav_item, stepper,
};
use crate::widgets;

pub const SETTINGS_ARG: &str = "--settings";
pub const RELOAD_MSG_NAME: &str = "OpenTaskBarReloadSettings";

const WIN_W: f32 = 920.0;
const WIN_H: f32 = 680.0;
const MIN_WIDTH: f32 = 540.0;
const MIN_HEIGHT: f32 = 428.0;
const SIDEBAR_BREAKPOINT: f32 = 680.0;
const SIDEBAR_WIDTH: f32 = 196.0;
const WIDGET_COLUMNS_BREAKPOINT: f32 = 520.0;

actions!(settings_navigation, [TabNext, TabPrevious]);

pub fn spawn_settings_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg(SETTINGS_ARG).spawn();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Dock,
    Widgets,
    Themes,
    Stacks,
    Preferences,
    About,
}

impl SettingsPage {
    const TOP: [Self; 4] = [Self::Dock, Self::Widgets, Self::Themes, Self::Stacks];
    const BOTTOM: [Self; 2] = [Self::Preferences, Self::About];

    fn title(self) -> &'static str {
        match self {
            Self::Dock => "Dock",
            Self::Widgets => "Widgets",
            Self::Themes => "Themes",
            Self::Stacks => "Stacks & Groups",
            Self::Preferences => "Preferences",
            Self::About => "About",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Dock => "\u{E7F4}",
            Self::Widgets => "\u{F0E2}",
            Self::Themes => "\u{E790}",
            Self::Stacks => "\u{E8B7}",
            Self::Preferences => "\u{E713}",
            Self::About => "\u{E946}",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Dock => "Adjust the dock layout and how it behaves on your displays.",
            Self::Widgets => "Choose which widgets appear on the dock and configure them.",
            Self::Themes => "Customize the dock colors, materials, and effects.",
            Self::Stacks => "Organize dock shortcuts into folders and app groups.",
            Self::Preferences => "Manage startup, profiles, and settings backups.",
            Self::About => "View the version and open local application files.",
        }
    }
}

fn widget_glyph(id: &str) -> &'static str {
    if id.starts_with("aiusage_") {
        return "\u{E99A}";
    }
    match id {
        "clock" => "\u{E823}",
        "weather" => "\u{E753}",
        "dayprogress" => "\u{E706}",
        "stopwatch" => "\u{E916}",
        "countdown" => "\u{E917}",
        "stats" => "\u{E9D9}",
        "netspeed" => "\u{E701}",
        "battery" => "\u{E83F}",
        "bluetoothbattery" => "\u{E702}",
        "storage" => "\u{EDA2}",
        "media" => "\u{E8D6}",
        "audio" => "\u{E767}",
        "quicksettings" => "\u{E9E9}",
        "keyboard" => "\u{E765}",
        "focus" => "\u{E708}",
        _ => "\u{F0E2}",
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
    let path = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)?
        .join(name);
    path.exists().then(|| path.to_string_lossy().into_owned())
}

struct SettingsView {
    page: SettingsPage,
    focus: FocusHandle,
    settings: DockSettings,
    widget_runtime: widgets::WidgetRuntime,
    startup_enabled: bool,
    scroll_handle: ScrollHandle,
    save_failed: bool,
}

impl SettingsView {
    fn new(settings: DockSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let mut widget_runtime = widgets::WidgetRuntime::default();
        widget_runtime.tick(&settings);
        Self {
            page: SettingsPage::Dock,
            focus,
            settings,
            widget_runtime,
            startup_enabled: win32::is_startup_enabled(),
            scroll_handle: ScrollHandle::new(),
            save_failed: false,
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
        self.sync_dock_pins();
        self.widget_runtime.tick(&self.settings);
        self.save_settings(cx);
    }

    fn widget_preview_lines(&self, id: &str) -> (String, String) {
        let is_dark = match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        };
        let spec = self.widget_runtime.chip_spec(id, &self.settings, is_dark);
        match spec {
            WidgetChipSpec::Clock {
                time,
                date,
                weekday,
            } => (time, format!("{date} · {weekday}")),
            WidgetChipSpec::Weather {
                available,
                temp,
                place,
                condition,
                ..
            } => {
                if available {
                    (temp, format!("{place} · {condition}"))
                } else {
                    (condition, place)
                }
            }
            WidgetChipSpec::DayProgress { percent, unit, .. } => (percent, unit),
            WidgetChipSpec::Timed { text, running, .. } => (
                text,
                if running {
                    "Running".to_owned()
                } else {
                    "Paused".to_owned()
                },
            ),
            WidgetChipSpec::SystemStats {
                cpu_text, ram_text, ..
            } => (format!("CPU {cpu_text}"), format!("RAM {ram_text}")),
            WidgetChipSpec::NetSpeed { down, up } => (format!("↓ {down}"), format!("↑ {up}")),
            WidgetChipSpec::BatteryLiquid {
                present,
                percent,
                charging,
            } => {
                if present {
                    (
                        format!("{percent}%"),
                        if charging {
                            "Charging".to_owned()
                        } else {
                            "Battery".to_owned()
                        },
                    )
                } else {
                    ("Unavailable".to_owned(), "No battery".to_owned())
                }
            }
            WidgetChipSpec::Bluetooth { devices } => devices.first().map_or_else(
                || ("—".to_owned(), "No connected devices".to_owned()),
                |(percent, _, _)| {
                    (
                        format!("{percent}%"),
                        format!("{} connected", devices.len()),
                    )
                },
            ),
            WidgetChipSpec::Storage { drives } => drives.first().map_or_else(
                || ("Unavailable".to_owned(), "No drives".to_owned()),
                |(name, capacity)| (name.clone(), capacity.clone()),
            ),
            WidgetChipSpec::Media { title, artist, .. } => (
                title,
                if artist.is_empty() {
                    "Not playing".to_owned()
                } else {
                    artist
                },
            ),
            WidgetChipSpec::Audio { volume, muted } => (
                if muted {
                    "Muted".to_owned()
                } else {
                    format!("{volume}%")
                },
                "Audio".to_owned(),
            ),
            WidgetChipSpec::QuickSettings { pills } => (
                pills
                    .iter()
                    .map(|(_, on)| if *on { "On" } else { "Off" })
                    .collect::<Vec<_>>()
                    .join(" · "),
                "Wi-Fi · Bluetooth · Night light".to_owned(),
            ),
            WidgetChipSpec::Keyboard { code } => (code, "Keyboard layout".to_owned()),
            WidgetChipSpec::AiDual { rows, .. } => rows.first().map_or_else(
                || ("—".to_owned(), "Sign in".to_owned()),
                |(label, used, _, reset)| (format!("{label} {:.0}%", used * 100.0), reset.clone()),
            ),
            WidgetChipSpec::AiAntigravity { columns } => columns
                .first()
                .and_then(|(_, rows)| rows.first())
                .map_or_else(
                    || ("—".to_owned(), "Sign in".to_owned()),
                    |(label, used, _, reset)| {
                        (format!("{label} {:.0}%", used * 100.0), reset.clone())
                    },
                ),
            WidgetChipSpec::AiOpenCode { columns } => columns.first().map_or_else(
                || ("—".to_owned(), "Sign in".to_owned()),
                |(label, value, _, _, reset)| (format!("{label} {value}"), reset.clone()),
            ),
            WidgetChipSpec::AiCopilot {
                value_text,
                meta_text,
                ..
            } => (value_text, meta_text),
            WidgetChipSpec::AiBalance {
                name, value_text, ..
            } => (value_text, name.to_owned()),
            WidgetChipSpec::Simple { top, bottom, .. } => (top, bottom),
        }
    }

    fn sync_dock_pins(&mut self) {
        let saved = settings::load_from(&settings::settings_path());
        self.settings.user_pinned_apps = saved.user_pinned_apps;
        self.settings.user_unpinned_apps = saved.user_unpinned_apps;
        self.settings.user_pinned_background_apps = saved.user_pinned_background_apps;
        self.settings.pin_order = saved.pin_order;
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        self.save_failed = settings::save_to(&settings::settings_path(), &self.settings).is_err();
        if !self.save_failed {
            notify_docks_reload();
        }
        cx.notify();
    }

    fn render_dock(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let align_seg = segmented_track(
            t,
            [
                ("Left", DockPosition::Left),
                ("Center", DockPosition::Center),
                ("Right", DockPosition::Right),
            ]
            .into_iter()
            .map(|(label, pos)| {
                segmented_item(
                    SharedString::from(format!("pos-{label}")),
                    t,
                    label,
                    self.settings.position == pos,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.position = pos;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let size_seg = segmented_track(
            t,
            [
                ("Compact", DockSize::Compact),
                ("Medium", DockSize::Medium),
                ("Large", DockSize::Large),
            ]
            .into_iter()
            .map(|(label, sz)| {
                segmented_item(
                    SharedString::from(format!("sz-{label}")),
                    t,
                    label,
                    self.settings.size == sz,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.size = sz;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let autohide_seg = segmented_track(
            t,
            [
                ("Off", AutoHideMode::Disabled),
                ("Smart", AutoHideMode::Smart),
                ("Always", AutoHideMode::Always),
            ]
            .into_iter()
            .map(|(label, mode)| {
                segmented_item(
                    SharedString::from(format!("ah-{label}")),
                    t,
                    label,
                    self.settings.auto_hide == mode,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.auto_hide = mode;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let scale_pct = (self.settings.dock_scale * 100.0).round() as i32;
        let width_label = self.settings.dock_custom_width_dips.map_or_else(
            || "Width (Auto)".to_owned(),
            |w| format!("Width ({w:.0}px)"),
        );

        vec![
            section_card(
                t,
                "Layout & Dimensions",
                vec![
                    setting_row(t, Some("\u{E8E3}"), "Alignment", align_seg).into_any_element(),
                    setting_row(t, Some("\u{E740}"), "Dock Size", size_seg).into_any_element(),
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
                    setting_row(
                        t,
                        Some("\u{E9A9}"),
                        "Icon Padding",
                        stepper(
                            "pad-minus",
                            "pad-plus",
                            t,
                            format!("{} px", self.settings.dock_icon_padding),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_icon_padding =
                                    (this.settings.dock_icon_padding - 1).max(0);
                                this.auto_save(cx);
                            }),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_icon_padding =
                                    (this.settings.dock_icon_padding + 1).min(12);
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
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
                ],
            )
            .into_any_element(),
            section_card(
                t,
                "Behavior & Displays",
                vec![
                    setting_row(t, Some("\u{E740}"), "Auto-Hide", autohide_seg).into_any_element(),
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

    fn render_themes(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme_seg = segmented_track(
            t,
            [
                ("System", AppTheme::System),
                ("Dark", AppTheme::Dark),
                ("Light", AppTheme::Light),
            ]
            .into_iter()
            .map(|(label, th)| {
                segmented_item(
                    SharedString::from(format!("theme-{label}")),
                    t,
                    label,
                    self.settings.theme == th,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.theme = th;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let bg_seg = segmented_track(
            t,
            [
                ("Glass", SurfaceBackground::Glass),
                ("Solid", SurfaceBackground::Solid),
                ("None", SurfaceBackground::None),
            ]
            .into_iter()
            .map(|(label, bg)| {
                segmented_item(
                    SharedString::from(format!("bg-{label}")),
                    t,
                    label,
                    self.settings.dock_background == bg,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.dock_background = bg;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );

        let opacity_pct = (self.settings.dock_opacity * 100.0).round() as i32;
        let radius_label = self
            .settings
            .dock_corner_radius
            .map_or_else(|| "Auto".to_owned(), |r| format!("{r} px"));

        vec![
            section_card(
                t,
                "Theme & Surfaces",
                vec![
                    setting_row(t, Some("\u{E790}"), "Theme", theme_seg).into_any_element(),
                    setting_row(t, Some("\u{E790}"), "Background", bg_seg).into_any_element(),
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

    fn render_widget_card(
        &self,
        w: &'static widgets::WidgetDescriptor,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wid = w.id;
        let enabled = self.settings.is_widget_enabled(wid);
        let (top_line, bot_line) = self.widget_preview_lines(wid);
        let border_col = if enabled { t.accent } else { t.border };
        let card_hover = t.surface_hover;
        let focus_color = t.text_primary;

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
            .gap(px(9.0))
            .p(px(11.0))
            .rounded(px(12.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(border_col))
            .cursor_pointer()
            .tab_index(0)
            .focus(move |style| style.border_color(rgba(focus_color)))
            .hover(move |s| s.bg(rgba(card_hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                let cur = this.settings.is_widget_enabled(wid);
                this.settings.set_widget_enabled(wid, !cur);
                this.auto_save(cx);
            }))
            .child(
                div()
                    .w_full()
                    .h(px(50.0))
                    .rounded(px(8.0))
                    .bg(rgba(t.dock_preview_bg))
                    .border_1()
                    .border_color(rgba(0xffffff18))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .px(px(12.0))
                            .py(px(4.5))
                            .rounded(px(7.0))
                            .bg(rgba(t.surface_muted))
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(top_line),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(bot_line),
                            ),
                    ),
            )
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
                                13.0,
                                if enabled { t.accent } else { t.text_secondary },
                            ))
                            .child(
                                div()
                                    .text_size(px(12.0))
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

    fn render_widgets(
        &self,
        t: &UiTheme,
        columns: usize,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
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

        for &cat in widgets::WIDGET_CATEGORIES {
            let items: Vec<&'static widgets::WidgetDescriptor> = widgets::ALL_WIDGETS
                .iter()
                .filter(|w| w.category == cat)
                .collect();
            if items.is_empty() {
                continue;
            }
            out.push(
                div()
                    .w_full()
                    .pt(px(4.0))
                    .px(px(2.0))
                    .text_size(px(11.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgba(t.text_secondary))
                    .child(cat)
                    .into_any_element(),
            );
            for group in items.chunks(columns) {
                let mut row = div().w_full().flex().gap(px(10.0));
                for &w in group {
                    row = row.child(self.render_widget_card(w, t, cx));
                }
                if group.len() < columns {
                    row = row.child(div().flex_1());
                }
                out.push(row.into_any_element());
            }
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

    fn render_preferences(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
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
            section_card(
                t,
                "System",
                vec![
                    setting_row(
                        t,
                        Some("\u{E7E8}"),
                        "Start with Windows",
                        ios_switch(
                            "sw-startup",
                            t,
                            self.startup_enabled,
                            cx.listener(|this, _, _, cx| {
                                let next = !this.startup_enabled;
                                win32::set_startup_enabled(next);
                                this.startup_enabled = win32::is_startup_enabled();
                                cx.notify();
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
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
                                    this.sync_dock_pins();
                                    let dst = settings::app_data_dir().join("settings.backup.json");
                                    let _ = settings::save_to(&dst, &this.settings);
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
                                        this.save_settings(cx);
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
                                this.save_settings(cx);
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
        let log_file = settings::app_data_dir()
            .join("opentaskbar.log")
            .to_string_lossy()
            .into_owned();
        vec![
            section_card(
                t,
                "Application",
                vec![
                    setting_row(t, Some("\u{E946}"), "Version", badge(t, "v0.1.0", true))
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                "Diagnostics & Recovery",
                vec![
                    setting_row(
                        t,
                        Some("\u{E8B7}"),
                        "Data Folder",
                        button(
                            "btn-open-data",
                            t,
                            "Open",
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(data_dir.clone()),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E9F5}"),
                        "Runtime Log",
                        button(
                            "btn-open-log",
                            t,
                            "Open",
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(log_file.clone()),
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
                            |_, _, _| opentaskbar_shell::taskbar::restore_from_journal(),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme();
        let width = window.viewport_size().width;
        let narrow = width < px(SIDEBAR_BREAKPOINT);
        let inset = px(if narrow { 20.0 } else { 32.0 });
        let content_width = width - inset * 2.0 - px(if narrow { 0.0 } else { SIDEBAR_WIDTH });
        let active_widgets_count = widgets::ALL_WIDGETS
            .iter()
            .filter(|w| self.settings.is_widget_enabled(w.id))
            .count();

        let make_nav = |p: SettingsPage, cx: &mut Context<Self>| -> AnyElement {
            let count_badge = (p == SettingsPage::Widgets)
                .then(|| format!("{active_widgets_count}/{}", widgets::ALL_WIDGETS.len()));
            sidebar_nav_item(
                SharedString::from(format!("nav-{}", p.title())),
                &t,
                p.glyph(),
                p.title(),
                count_badge,
                self.page == p,
                cx.listener(move |this, _, _, cx| {
                    if this.page != p {
                        this.page = p;
                        this.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
                        cx.notify();
                    }
                }),
            )
            .when(narrow, |item| item.w(px(156.0)))
            .into_any_element()
        };
        let top_nav: Vec<AnyElement> = SettingsPage::TOP
            .into_iter()
            .map(|p| make_nav(p, cx))
            .collect();
        let bottom_nav: Vec<AnyElement> = SettingsPage::BOTTOM
            .into_iter()
            .map(|p| make_nav(p, cx))
            .collect();

        let content_blocks = match self.page {
            SettingsPage::Dock => self.render_dock(&t, cx),
            SettingsPage::Widgets => {
                let columns = if content_width < px(WIDGET_COLUMNS_BREAKPOINT) {
                    1
                } else {
                    2
                };
                self.render_widgets(&t, columns, cx)
            }
            SettingsPage::Themes => self.render_themes(&t, cx),
            SettingsPage::Stacks => self.render_stacks(&t, cx),
            SettingsPage::Preferences => self.render_preferences(&t, cx),
            SettingsPage::About => self.render_about(&t),
        };

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
            .flex_shrink_0()
            .flex()
            .flex_col()
            .justify_between()
            .gap(px(12.0))
            .p(px(14.0))
            .bg(rgba(t.sidebar_bg))
            .border_color(rgba(t.border))
            .when(narrow, |panel| panel.w_full().border_b_1())
            .when(!narrow, |panel| {
                panel.w(px(SIDEBAR_WIDTH)).h_full().border_r_1()
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .gap(px(4.0))
                    .when(narrow, |nav| nav.flex_wrap())
                    .when(!narrow, |nav| nav.flex_col())
                    .children(top_nav),
            )
            .child(
                div()
                    .w_full()
                    .pt(px(10.0))
                    .border_t_1()
                    .border_color(rgba(t.separator))
                    .flex()
                    .gap(px(4.0))
                    .when(narrow, |nav| nav.flex_wrap())
                    .when(!narrow, |nav| nav.flex_col())
                    .children(bottom_nav),
            );

        let page_content = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .children(content_blocks);

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
                .px(inset)
                .py(px(20.0))
                .child(page_content),
        );

        if let Some(sb) = scrollbar_el {
            scroll_wrap = scroll_wrap.child(sb);
        }

        let main = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .px(inset)
                    .py(px(if narrow { 12.0 } else { 18.0 }))
                    .border_b_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_size(px(22.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.page.title()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgba(t.text_secondary))
                            .child(self.page.description()),
                    ),
            )
            .when(self.save_failed, |panel| panel.child(
                div().px(inset).py(px(8.0)).bg(rgba(t.destructive_soft))
                    .text_color(rgba(t.destructive)).text_size(px(13.0))
                    .child("Could not save settings. Check that the settings folder is writable and try again."),
            ))
            .child(scroll_wrap);

        let hover_cap = t.surface_hover;
        let close_hover = t.destructive;
        let titlebar = div()
            .w_full()
            .h(px(40.0))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .bg(rgba(t.sidebar_bg))
            .border_b_1()
            .border_color(rgba(t.border))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        div()
                            .size(px(22.0))
                            .rounded(px(6.0))
                            .bg(rgba(t.accent_soft))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_glyph("\u{E713}", 12.0, t.accent)),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.text_primary))
                            .child("OpenTaskbar Settings"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .h_full()
                    .child(
                        div()
                            .id("cap-min")
                            .w(px(44.0))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(hover_cap)))
                            .on_click(|_, window, _| window.minimize_window())
                            .child(icon_glyph("\u{E921}", 10.0, t.text_primary)),
                    )
                    .child(
                        div()
                            .id("cap-max")
                            .w(px(44.0))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(hover_cap)))
                            .on_click(|_, window, _| window.zoom_window())
                            .child(icon_glyph("\u{E922}", 10.0, t.text_primary)),
                    )
                    .child(
                        div()
                            .id("cap-close")
                            .w(px(46.0))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(close_hover)))
                            .on_click(|_, window, _| window.remove_window())
                            .child(icon_glyph("\u{E8BB}", 10.0, t.text_primary)),
                    ),
            );

        div()
            .id("settings")
            .track_focus(&self.focus)
            .tab_group()
            .on_action(|_: &TabNext, window, _| window.focus_next())
            .on_action(|_: &TabPrevious, window, _| window.focus_prev())
            .w_full()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(rgba(t.canvas_bg))
            .font_family("Segoe UI")
            .text_size(px(14.0))
            .text_color(rgba(t.text_primary))
            .child(titlebar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .when(narrow, |root| root.flex_col())
                    .overflow_hidden()
                    .child(sidebar)
                    .child(main),
            )
    }
}

pub fn run_settings_window() {
    let loaded = settings::load_from(&settings::settings_path());
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("tab", TabNext, None),
            KeyBinding::new("shift-tab", TabPrevious, None),
        ]);
        let bounds = Bounds::centered(None, size(px(WIN_W), px(WIN_H)), cx);
        let _ = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
                titlebar: Some(TitlebarOptions {
                    title: Some("OpenTaskbar Settings".into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| SettingsView::new(loaded.clone(), window, cx)),
        );
        cx.activate(true);
    });
}
