//! On-demand GPUI Settings window (`OpenTaskBar.exe --settings`).
//!
//! Built with OpenTaskBar's iOS + shadcn/ui reusable component library (`ui_components.rs`).
//! Runs in a short-lived subprocess so GPUI's DX11 context and RAM are 100% freed the moment
//! Settings is closed. Every change atomically saves `settings.json` and broadcasts
//! `OpenTaskBarReloadSettings` so all live DirectComposition docks update immediately.

use std::fs;
use std::path::PathBuf;

use gpui::{
    AnyElement, App, Application, Bounds, Context, SharedString, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, rgba, size,
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
    self as ui, ButtonVariant, UiTheme, badge, button, ios_switch, section_card, segmented_item,
    segmented_track, setting_row, sidebar_nav_item, stepper,
};
use crate::widgets;
use crate::win32;

pub const SETTINGS_ARG: &str = "--settings";
pub const RELOAD_MSG_NAME: &str = "OpenTaskBarReloadSettings";

/// Spawns `OpenTaskBar.exe --settings` as an on-demand subprocess.
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
            Self::Layout => "Layout & Displays",
            Self::Widgets => "Widgets",
            Self::Stacks => "Stacks & Groups",
            Self::Profiles => "Profiles & Backup",
            Self::About => "About",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::General => "Theme, auto-hide, focus mode, and window behavior.",
            Self::Appearance => "Surface material, icon scale, geometry, and motion.",
            Self::Layout => "Alignment, multi-monitor visibility, and dock utilities.",
            Self::Widgets => "Choose which live widgets appear on your dock.",
            Self::Stacks => "Quick-access folder stacks and custom app groups.",
            Self::Profiles => "Switch saved dock presets or export/restore settings.",
            Self::About => "OpenTaskBar hybrid runtime details and recovery tools.",
        }
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
    status: Option<SharedString>,
}

impl SettingsView {
    fn new(settings: DockSettings) -> Self {
        Self {
            page: SettingsPage::General,
            settings,
            status: None,
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

    fn persist(&mut self, note: impl Into<SharedString>, cx: &mut Context<Self>) {
        match settings::save_to(&settings::settings_path(), &self.settings) {
            Ok(()) => {
                notify_docks_reload();
                self.status = Some(note.into());
            }
            Err(err) => {
                self.status = Some(format!("Save failed: {err}").into());
            }
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
                        this.persist("Theme set to System", cx);
                    }),
                ),
                segmented_item(
                    "theme-dark",
                    t,
                    "Dark",
                    self.settings.theme == AppTheme::Dark,
                    cx.listener(|this, _, _, cx| {
                        this.settings.theme = AppTheme::Dark;
                        this.persist("Theme set to Dark", cx);
                    }),
                ),
                segmented_item(
                    "theme-light",
                    t,
                    "Light",
                    self.settings.theme == AppTheme::Light,
                    cx.listener(|this, _, _, cx| {
                        this.settings.theme = AppTheme::Light;
                        this.persist("Theme set to Light", cx);
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
                        this.persist("Auto-Hide disabled", cx);
                    }),
                ),
                segmented_item(
                    "ah-smart",
                    t,
                    "Smart",
                    self.settings.auto_hide == AutoHideMode::Smart,
                    cx.listener(|this, _, _, cx| {
                        this.settings.auto_hide = AutoHideMode::Smart;
                        this.persist("Smart Auto-Hide enabled", cx);
                    }),
                ),
                segmented_item(
                    "ah-always",
                    t,
                    "Always",
                    self.settings.auto_hide == AutoHideMode::Always,
                    cx.listener(|this, _, _, cx| {
                        this.settings.auto_hide = AutoHideMode::Always;
                        this.persist("Auto-Hide set to Always", cx);
                    }),
                ),
            ],
        );

        vec![
            section_card(
                t,
                "Appearance & Visibility",
                Some("Smart Auto-Hide tucks the dock away only when a window overlaps it."),
                vec![
                    setting_row(
                        t,
                        "Theme",
                        Some("Follow Windows dark/light mode or lock a theme."),
                        theme_seg,
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Auto-Hide",
                        Some("Slide dock off the screen edge when not needed."),
                        autohide_seg,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                "Behavior & Focus",
                None::<&str>,
                vec![
                    setting_row(
                        t,
                        "Focus Mode",
                        Some("Hide non-essential widgets while concentrating."),
                        ios_switch(
                            "sw-focus",
                            t,
                            self.settings.focus_mode,
                            cx.listener(|this, _, _, cx| {
                                this.settings.focus_mode = !this.settings.focus_mode;
                                this.persist("Focus Mode updated", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Reveal Dock on Win Key",
                        Some("Show hidden dock automatically when pressing Start."),
                        ios_switch(
                            "sw-winkey",
                            t,
                            self.settings.reveal_dock_on_win_key,
                            cx.listener(|this, _, _, cx| {
                                this.settings.reveal_dock_on_win_key =
                                    !this.settings.reveal_dock_on_win_key;
                                this.persist("Saved", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Current Virtual Desktop Only",
                        Some("Show running apps only from the active Windows desktop."),
                        ios_switch(
                            "sw-vdesk",
                            t,
                            self.settings.current_desktop_only,
                            cx.listener(|this, _, _, cx| {
                                this.settings.current_desktop_only =
                                    !this.settings.current_desktop_only;
                                this.persist("Saved", cx);
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
                        this.persist("Glass surface enabled", cx);
                    }),
                ),
                segmented_item(
                    "bg-solid",
                    t,
                    "Solid",
                    self.settings.dock_background == SurfaceBackground::Solid,
                    cx.listener(|this, _, _, cx| {
                        this.settings.dock_background = SurfaceBackground::Solid;
                        this.persist("Solid surface enabled", cx);
                    }),
                ),
                segmented_item(
                    "bg-none",
                    t,
                    "None",
                    self.settings.dock_background == SurfaceBackground::None,
                    cx.listener(|this, _, _, cx| {
                        this.settings.dock_background = SurfaceBackground::None;
                        this.persist("Transparent surface enabled", cx);
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
                        this.persist("Size set to Compact", cx);
                    }),
                ),
                segmented_item(
                    "sz-med",
                    t,
                    "Medium",
                    self.settings.size == DockSize::Medium,
                    cx.listener(|this, _, _, cx| {
                        this.settings.size = DockSize::Medium;
                        this.persist("Size set to Medium", cx);
                    }),
                ),
                segmented_item(
                    "sz-large",
                    t,
                    "Large",
                    self.settings.size == DockSize::Large,
                    cx.listener(|this, _, _, cx| {
                        this.settings.size = DockSize::Large;
                        this.persist("Size set to Large", cx);
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
                "Surface & Size",
                None::<&str>,
                vec![
                    setting_row(
                        t,
                        "Background Style",
                        Some("Dock pill backdrop material."),
                        bg_seg,
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Icon Size Preset",
                        Some("Base icon footprint in DIPs."),
                        size_seg,
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Opacity",
                        Some("Dock surface translucency."),
                        stepper(
                            "op-minus",
                            "op-plus",
                            t,
                            format!("{opacity_pct}%"),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_opacity =
                                    (this.settings.dock_opacity - 0.05).clamp(0.2, 1.0);
                                this.persist("Opacity updated", cx);
                            }),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_opacity =
                                    (this.settings.dock_opacity + 0.05).clamp(0.2, 1.0);
                                this.persist("Opacity updated", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Dock Scale",
                        Some("Fine-grained icon multiplier (75% - 150%)."),
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
                                this.persist("Scale updated", cx);
                            }),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_scale =
                                    ((this.settings.dock_scale * 20.0).round() + 1.0)
                                        .clamp(15.0, 30.0)
                                        / 20.0;
                                this.persist("Scale updated", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Corner Radius",
                        Some("Pill corner rounding."),
                        stepper(
                            "rad-minus",
                            "rad-plus",
                            t,
                            radius_label,
                            cx.listener(|this, _, _, cx| {
                                let cur = this.settings.dock_corner_radius.unwrap_or(16);
                                this.settings.dock_corner_radius = Some((cur - 2).max(0));
                                this.persist("Corner radius updated", cx);
                            }),
                            cx.listener(|this, _, _, cx| {
                                let cur = this.settings.dock_corner_radius.unwrap_or(16);
                                this.settings.dock_corner_radius = Some((cur + 2).min(32));
                                this.persist("Corner radius updated", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Bottom Margin",
                        Some("Floating gap from screen edge."),
                        stepper(
                            "gap-minus",
                            "gap-plus",
                            t,
                            format!("{} px", self.settings.dock_bottom_gap_pixels),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_bottom_gap_pixels =
                                    (this.settings.dock_bottom_gap_pixels - 2).max(0);
                                this.persist("Margin updated", cx);
                            }),
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_bottom_gap_pixels =
                                    (this.settings.dock_bottom_gap_pixels + 2).min(32);
                                this.persist("Margin updated", cx);
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
                None::<&str>,
                vec![
                    setting_row(
                        t,
                        "Smooth Animations",
                        Some("VSync spring motion for hover, bounce, and auto-hide."),
                        ios_switch(
                            "sw-anim",
                            t,
                            self.settings.animations_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.animations_enabled =
                                    !this.settings.animations_enabled;
                                this.persist("Animations updated", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Fisheye Magnification",
                        Some("macOS-style icon wave on pointer hover."),
                        ios_switch(
                            "sw-mag",
                            t,
                            self.settings.magnification_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.magnification_enabled =
                                    !this.settings.magnification_enabled;
                                this.persist("Magnification updated", cx);
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
                        this.persist("Aligned Left", cx);
                    }),
                ),
                segmented_item(
                    "pos-center",
                    t,
                    "Center",
                    self.settings.position == DockPosition::Center,
                    cx.listener(|this, _, _, cx| {
                        this.settings.position = DockPosition::Center;
                        this.persist("Aligned Center", cx);
                    }),
                ),
                segmented_item(
                    "pos-right",
                    t,
                    "Right",
                    self.settings.position == DockPosition::Right,
                    cx.listener(|this, _, _, cx| {
                        this.settings.position = DockPosition::Right;
                        this.persist("Aligned Right", cx);
                    }),
                ),
            ],
        );

        let custom_width_text = self
            .settings
            .dock_custom_width_dips
            .map_or_else(|| "Auto-fit".to_owned(), |w| format!("{w:.0} DIPs"));

        vec![
            section_card(
                t,
                "Position & Displays",
                Some("You can also drag the left or right edge of the dock pill to resize it."),
                vec![
                    setting_row(
                        t,
                        "Alignment",
                        Some("Horizontal dock placement."),
                        align_seg,
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Full-Width Taskbar",
                        Some("Stretch dock across the entire monitor width."),
                        ios_switch(
                            "sw-fullw",
                            t,
                            self.settings.full_width,
                            cx.listener(|this, _, _, cx| {
                                this.settings.full_width = !this.settings.full_width;
                                this.persist("Width mode updated", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Custom Width",
                        Some(format!("Current: {custom_width_text}")),
                        button(
                            "btn-reset-width",
                            t,
                            "Reset to Auto",
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_custom_width_dips = None;
                                this.persist("Dock width reset to Auto", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Show on All Monitors",
                        Some("Display a DPI-aware dock on every connected screen."),
                        ios_switch(
                            "sw-allmon",
                            t,
                            self.settings.show_on_all_monitors,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_on_all_monitors =
                                    !this.settings.show_on_all_monitors;
                                this.persist("Multi-monitor setting saved", cx);
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
                None::<&str>,
                vec![
                    setting_row(
                        t,
                        "Background Apps Button",
                        Some("Opens OpenTaskBar's custom background apps flyout."),
                        ios_switch(
                            "sw-tray",
                            t,
                            self.settings.show_tray_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_tray_button = !this.settings.show_tray_button;
                                this.persist("Background Apps button updated", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Recycle Bin",
                        Some("Quick access and right-click Empty Recycle Bin."),
                        ios_switch(
                            "sw-bin",
                            t,
                            self.settings.show_recycle_bin,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_recycle_bin = !this.settings.show_recycle_bin;
                                this.persist("Recycle Bin updated", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Show Desktop Strip",
                        Some("Thin edge button at the far right of the dock."),
                        ios_switch(
                            "sw-sd",
                            t,
                            self.settings.show_desktop_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_desktop_button =
                                    !this.settings.show_desktop_button;
                                this.persist("Show Desktop button updated", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    fn render_widgets(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut sections: Vec<AnyElement> = Vec::new();
        let active_count = widgets::ALL_WIDGETS
            .iter()
            .filter(|w| self.settings.is_widget_enabled(w.id))
            .count();

        sections.push(
            section_card(
                t,
                "Active Widgets",
                Some("Widgets are opt-in: enable only the ones you want on your dock. Clicking any widget opens its shared flyout."),
                vec![
                    setting_row(
                        t,
                        format!("{active_count} of {} widgets enabled", widgets::ALL_WIDGETS.len()),
                        Some("Scroll wheel over Audio Volume adjusts master volume directly."),
                        button(
                            "btn-clear-widgets",
                            t,
                            "Disable All",
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_clock_widget = false;
                                this.settings.show_system_stats_widget = false;
                                this.settings.enabled_widgets.clear();
                                this.persist("All widgets disabled", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        );

        let categories = ["Time", "System", "Media", "Input", "Productivity", "Info"];
        for cat in categories {
            let rows: Vec<AnyElement> = widgets::ALL_WIDGETS
                .iter()
                .filter(|w| w.category == cat)
                .map(|w| {
                    let wid = w.id;
                    let enabled = self.settings.is_widget_enabled(wid);
                    setting_row(
                        t,
                        w.name,
                        Some(w.description),
                        ios_switch(
                            SharedString::from(format!("w-{wid}")),
                            t,
                            enabled,
                            cx.listener(move |this, _, _, cx| {
                                let cur = this.settings.is_widget_enabled(wid);
                                this.settings.set_widget_enabled(wid, !cur);
                                this.persist(format!("{} updated", w.name), cx);
                            }),
                        ),
                    )
                    .into_any_element()
                })
                .collect();
            if !rows.is_empty() {
                sections.push(section_card(t, cat, None::<&str>, rows).into_any_element());
            }
        }

        // Weather city selector
        let city_seg = segmented_track(
            t,
            ["Istanbul", "Ankara", "London", "Berlin", "New York"]
                .into_iter()
                .map(|city| {
                    let active = self.settings.weather_city.eq_ignore_ascii_case(city);
                    segmented_item(
                        SharedString::from(format!("city-{city}")),
                        t,
                        city,
                        active,
                        cx.listener(move |this, _, _, cx| {
                            this.settings.weather_city = city.to_owned();
                            this.persist(format!("Weather city set to {city}"), cx);
                        }),
                    )
                })
                .collect(),
        );
        sections.push(
            section_card(
                t,
                "Weather Location",
                None::<&str>,
                vec![
                    setting_row(
                        t,
                        "City",
                        Some(format!("Current: {}", self.settings.weather_city)),
                        city_seg,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        );

        sections
    }

    fn render_stacks(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut stack_rows: Vec<AnyElement> = self
            .settings
            .stack_folders
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let p = path.clone();
                setting_row(
                    t,
                    std::path::Path::new(&p)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&p)
                        .to_owned(),
                    Some(p),
                    button(
                        SharedString::from(format!("rm-stack-{i}")),
                        t,
                        "Remove",
                        ButtonVariant::Destructive,
                        cx.listener(move |this, _, _, cx| {
                            if i < this.settings.stack_folders.len() {
                                this.settings.stack_folders.remove(i);
                                this.persist("Folder stack removed", cx);
                            }
                        }),
                    ),
                )
                .into_any_element()
            })
            .collect();

        if stack_rows.is_empty() {
            stack_rows.push(
                setting_row(
                    t,
                    "No folder stacks yet",
                    Some("Add a common folder below for quick flyout access."),
                    badge(t, "Empty", false),
                )
                .into_any_element(),
            );
        }

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
                                this.persist(format!("{folder} stack added"), cx);
                            }
                        }),
                    )
                }),
        );
        stack_rows.push(
            setting_row(
                t,
                "Quick Add Folder",
                Some("Add standard Windows folders."),
                quick_add,
            )
            .into_any_element(),
        );

        let mut group_rows: Vec<AnyElement> = self
            .settings
            .app_groups
            .iter()
            .enumerate()
            .map(|(i, g)| {
                setting_row(
                    t,
                    g.name.clone(),
                    Some(format!("{} app(s) linked", g.executable_paths.len())),
                    button(
                        SharedString::from(format!("rm-grp-{i}")),
                        t,
                        "Remove",
                        ButtonVariant::Destructive,
                        cx.listener(move |this, _, _, cx| {
                            if i < this.settings.app_groups.len() {
                                this.settings.app_groups.remove(i);
                                this.persist("App group removed", cx);
                            }
                        }),
                    ),
                )
                .into_any_element()
            })
            .collect();

        if group_rows.is_empty() {
            group_rows.push(
                setting_row(
                    t,
                    "No app groups yet",
                    Some("Create a group to organize related apps."),
                    badge(t, "Empty", false),
                )
                .into_any_element(),
            );
        }
        group_rows.push(
            setting_row(
                t,
                "Create Starter Group",
                Some("Adds a customizable Productivity app group."),
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
                            this.persist("Productivity group created", cx);
                        }
                    }),
                ),
            )
            .into_any_element(),
        );

        vec![
            section_card(t, "Folder Stacks", None::<&str>, stack_rows).into_any_element(),
            section_card(t, "App Groups", None::<&str>, group_rows).into_any_element(),
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
                let name = p.name.clone();
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
                                this.persist(format!("Switched to profile '{}'", prof.name), cx);
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
                                this.persist("Profile deleted", cx);
                            }
                        }),
                    ));
                setting_row(
                    t,
                    name,
                    Some(format!(
                        "{:?} · {:?} · {} widget(s)",
                        p.size,
                        p.auto_hide,
                        p.enabled_widgets.len()
                    )),
                    controls,
                )
                .into_any_element()
            })
            .collect();

        let save_presets = div().flex().items_center().gap(px(6.0)).children(
            ["Work", "Gaming", "Minimal"].into_iter().map(|preset| {
                button(
                    SharedString::from(format!("save-prof-{preset}")),
                    t,
                    format!("Save as {preset}"),
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
                        this.persist(format!("Saved current layout as '{preset}'"), cx);
                    }),
                )
            }),
        );
        prof_rows.push(
            setting_row(
                t,
                "Save Current Setup",
                Some("Snapshot position, size, auto-hide, and enabled widgets."),
                save_presets,
            )
            .into_any_element(),
        );

        let backup_path = settings::app_data_dir().join("settings.backup.json");
        vec![
            section_card(t, "Profiles", None::<&str>, prof_rows).into_any_element(),
            section_card(
                t,
                "Backup & Recovery",
                Some(format!("Backup file: {}", backup_path.display())),
                vec![
                    setting_row(
                        t,
                        "Export Backup",
                        Some("Write a snapshot of settings.json to your app data folder."),
                        button(
                            "btn-export",
                            t,
                            "Export JSON",
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                let dst = settings::app_data_dir().join("settings.backup.json");
                                match settings::save_to(&dst, &this.settings) {
                                    Ok(()) => this.status = Some("Backup exported".into()),
                                    Err(e) => {
                                        this.status = Some(format!("Export failed: {e}").into())
                                    }
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Restore Backup",
                        Some("Load settings from settings.backup.json."),
                        button(
                            "btn-import",
                            t,
                            "Restore JSON",
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                let src = settings::app_data_dir().join("settings.backup.json");
                                if src.exists() {
                                    this.settings = settings::load_from(&src);
                                    this.persist("Backup restored", cx);
                                } else {
                                    this.status = Some("No backup file found".into());
                                    cx.notify();
                                }
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Reset All Settings",
                        Some("Restore factory defaults (widgets remain off)."),
                        button(
                            "btn-reset-all",
                            t,
                            "Reset Defaults",
                            ButtonVariant::Destructive,
                            cx.listener(|this, _, _, cx| {
                                this.settings = DockSettings::default();
                                this.persist("Settings reset to defaults", cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    fn render_about(&self, t: &UiTheme, _cx: &mut Context<Self>) -> Vec<AnyElement> {
        let data_dir = settings::app_data_dir();
        let data_dir_str = data_dir.to_string_lossy().into_owned();
        let settings_exists = fs::metadata(settings::settings_path()).is_ok();

        vec![
            section_card(
                t,
                "OpenTaskBar (Rust Hybrid)",
                Some("Native Win32 + DirectComposition dock strip with on-demand GPUI Settings."),
                vec![
                    setting_row(
                        t,
                        "Version",
                        Some("Rust Port 0.1.0"),
                        badge(t, "Hybrid", true),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Configuration",
                        Some(settings::settings_path().to_string_lossy().into_owned()),
                        badge(t, if settings_exists { "Synced" } else { "Default" }, false),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                "Tools & Recovery",
                None::<&str>,
                vec![
                    setting_row(
                        t,
                        "Open Data Folder",
                        Some("Inspect settings.json, backups, and runtime logs."),
                        button(
                            "btn-open-data",
                            t,
                            "Open Folder",
                            ButtonVariant::Secondary,
                            move |_, _, _| {
                                pins::launch(data_dir_str.clone());
                            },
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        "Restore Windows Taskbar",
                        Some("Immediately unhide Explorer's native taskbar."),
                        button(
                            "btn-restore-tb",
                            t,
                            "Restore Taskbar",
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
                    p.title(),
                    count_badge,
                    self.page == p,
                    cx.listener(move |this, _, _, cx| {
                        this.page = p;
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
            SettingsPage::About => self.render_about(&t, cx),
        };

        let mut sidebar = div()
            .w(px(224.0))
            .h_full()
            .flex()
            .flex_col()
            .justify_between()
            .p(px(14.0))
            .bg(rgba(t.sidebar_bg))
            .border_r_1()
            .border_color(rgba(t.border))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
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
                                    .text_size(px(15.0))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgba(t.text_primary))
                                    .child("OpenTaskBar"),
                            )
                            .child(ui::badge(&t, "v0.1", true)),
                    )
                    .children(nav_items),
            );

        if let Some(status) = &self.status {
            sidebar = sidebar.child(
                div()
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded(px(8.0))
                    .bg(rgba(t.surface_muted))
                    .text_size(px(11.5))
                    .text_color(rgba(t.text_secondary))
                    .child(status.clone()),
            );
        }

        let main = div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgba(t.canvas_bg))
            .child(
                div()
                    .px(px(28.0))
                    .pt(px(22.0))
                    .pb(px(14.0))
                    .border_b_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_size(px(20.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgba(t.text_primary))
                            .child(self.page.title()),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgba(t.text_secondary))
                            .child(self.page.subtitle()),
                    ),
            )
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(28.0))
                    .py(px(18.0))
                    .flex()
                    .flex_col()
                    .gap(px(18.0))
                    .children(content_blocks),
            );

        div()
            .w_full()
            .h_full()
            .flex()
            .bg(rgba(t.canvas_bg))
            .text_color(rgba(t.text_primary))
            .child(sidebar)
            .child(main)
    }
}

pub fn run_settings_window() {
    let loaded = settings::load_from(&settings::settings_path());
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(860.0), px(620.0)), cx);
        let _ = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| SettingsView::new(loaded.clone())),
        );
        cx.activate(true);
    });
}
