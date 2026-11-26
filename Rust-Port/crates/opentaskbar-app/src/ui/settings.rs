//! On-demand GPUI Settings window (`OpenTaskBar.exe --settings`).
//!
//! Grouped settings with a responsive navigation pane and immediate dock updates.

use std::ops::Range;
use std::time::Duration;

use gpui::{
    AnyElement, App, Application, Bounds, Context, EntityInputHandler, FocusHandle, KeyBinding,
    KeyDownEvent, MouseButton, ScrollAnchor, ScrollHandle, SharedString, TitlebarOptions,
    UTF16Selection, Window, WindowBounds, WindowControlArea, WindowOptions, actions, div,
    prelude::*, px, rgba, size,
};
use opentaskbar_core::hotkey;
use opentaskbar_core::i18n::{self as loc, LANGUAGES};
use opentaskbar_core::settings::{
    self, AppTheme, AutoHideMode, ClockDateFormat, ClockDateLayout, ClockDateStyle, DockPosition,
    DockSettings, DockSize, DynamicIslandVisibilityMode, SurfaceBackground, WidgetDensityMode,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, RegisterWindowMessageW, SendNotifyMessageW,
};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::design::fonts;
use crate::dock::dcomp::WidgetChipSpec;
use crate::platform::{pins, theme_packs, toast, update, win32};
use crate::ui::components::{
    ButtonVariant, UiTheme, badge, button, icon_glyph, ios_switch, live_slider, section_card,
    segmented_item, segmented_track, setting_row, setting_row_with_id, sidebar_nav_item, stepper,
};
use crate::ui::menu::{self, BorderStyle, MenuEntry, MenuIcon};
use crate::ui::prompt;
use crate::ui::settings_search::{self, SettingsSearchEntry};
use crate::ui::text_input::TextInput;
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

fn set_border_thickness(settings: &mut DockSettings, surface: usize, value: f64) {
    let value = value.clamp(0.0, 8.0);
    match surface {
        0 => settings.dock_border_thickness = value,
        1 => settings.menu_border_thickness = value,
        2 => settings.widget_border_thickness = value,
        _ => settings.settings_border_thickness = value,
    }
}

/// Parses `#RRGGBB` / `RRGGBB` into the ARGB `u32` used by GPUI surfaces.
fn parse_hex_color(value: &str) -> Option<u32> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(((0xffu32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
}

pub fn spawn_settings_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg(SETTINGS_ARG).spawn();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsPage {
    Dock,
    Widgets,
    DynamicIsland,
    Themes,
    Preferences,
    About,
}

impl SettingsPage {
    const TOP: [Self; 4] = [Self::Dock, Self::Widgets, Self::DynamicIsland, Self::Themes];
    const BOTTOM: [Self; 2] = [Self::Preferences, Self::About];

    pub(crate) fn title(self) -> String {
        match self {
            Self::Dock => loc::t("Dock"),
            Self::Widgets => loc::t("Widgets"),
            Self::DynamicIsland => loc::t("DynamicIslandTitle"),
            Self::Themes => loc::t("Themes"),
            Self::Preferences => loc::t("Preferences"),
            Self::About => loc::t("About"),
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Dock => "\u{E7F4}",
            Self::Widgets => "\u{F0E2}",
            Self::DynamicIsland => "\u{E70A}",
            Self::Themes => "\u{E790}",
            Self::Preferences => "\u{E713}",
            Self::About => "\u{E946}",
        }
    }

    fn description(self) -> String {
        match self {
            Self::Dock => loc::t("SettingsPageDockDescription"),
            Self::Widgets => loc::t("SettingsPageWidgetsDescription"),
            Self::DynamicIsland => loc::t("DynamicIslandEnableDescription"),
            Self::Themes => loc::t("SettingsPageThemesDescription"),
            Self::Preferences => loc::t("SettingsPagePreferencesDescription"),
            Self::About => loc::t("SettingsPageAboutDescription"),
        }
    }
}

/// Display label for a widget filter/category value stored in `widget_filter`.
pub(crate) fn category_label(category: &str) -> String {
    match category {
        "All" => loc::t("WidgetCategoryAll"),
        "Active" => loc::t("Active"),
        "Time & Weather" => loc::t("WidgetCategoryTime"),
        "System & Hardware" => loc::t("WidgetCategorySystem"),
        "AI Quotas" => loc::t("WidgetCategoryAi"),
        "Media & Controls" => loc::t("WidgetCategoryMedia"),
        "Productivity" => loc::t("WidgetCategoryProductivity"),
        other => loc::t(other),
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

struct SettingsView {
    page: SettingsPage,
    focus: FocusHandle,
    /// Header search field. UTF-16 offsets keep the GPUI platform input handler and the
    /// rendered caret in agreement even for non-ASCII (e.g. Turkish) text.
    search_text: String,
    search_selection: Range<usize>,
    search_focus: FocusHandle,
    settings: DockSettings,
    widget_runtime: widgets::WidgetRuntime,
    widget_filter: &'static str,
    startup_enabled: bool,
    scroll_handle: ScrollHandle,
    /// Row targeted by the most recent Settings search result. Rendered with a transient
    /// highlight and used to select which row receives the scroll anchor.
    highlight_row: Option<String>,
    /// Fresh anchor created for each search jump; attached to `highlight_row` during render
    /// and consumed once `ScrollAnchor::scroll_to` has been scheduled.
    scroll_anchor: Option<ScrollAnchor>,
    save_failed: bool,
    font_page: usize,
    hotkey_status: String,
    theme_status: String,
    api_key_status: String,
    weather_suggestions: Vec<widgets::telemetry::GeoLocationMatch>,
    weather_search_status: String,
    /// A burst of slider/toggle events collapses into one disk write + dock broadcast.
    save_pending: bool,
    update_status: String,
    update_release: Option<update::Release>,
}

impl SettingsView {
    fn new(settings: DockSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        fonts::apply_settings(&settings);
        let focus = cx.focus_handle();
        focus.focus(window);
        let mut widget_runtime = widgets::WidgetRuntime::default();
        widget_runtime.tick(&settings);
        Self {
            page: SettingsPage::Dock,
            focus,
            search_text: String::new(),
            search_selection: 0..0,
            search_focus: cx.focus_handle(),
            settings,
            widget_runtime,
            widget_filter: "All",
            startup_enabled: win32::is_startup_enabled(),
            scroll_handle: ScrollHandle::new(),
            highlight_row: None,
            scroll_anchor: None,
            save_failed: false,
            font_page: 0,
            hotkey_status: String::new(),
            theme_status: String::new(),
            api_key_status: String::new(),
            weather_suggestions: Vec::new(),
            weather_search_status: String::new(),
            save_pending: false,
            update_status: String::new(),
            update_release: None,
        }
    }

    fn search_utf16_len(&self) -> usize {
        self.search_text.encode_utf16().count()
    }

    fn byte_index_for_utf16(text: &str, offset: usize) -> usize {
        let mut units = 0;
        for (index, ch) in text.char_indices() {
            if units >= offset {
                return index;
            }
            units += ch.len_utf16();
        }
        text.len()
    }

    /// Replace a UTF-16 range of the search field and move the caret to the end of the
    /// inserted text. The platform input handler calls this for typing, IME and paste.
    fn replace_search_range(&mut self, range: Range<usize>, text: &str) {
        let len = self.search_utf16_len();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        let start_byte = Self::byte_index_for_utf16(&self.search_text, start);
        let end_byte = Self::byte_index_for_utf16(&self.search_text, end);

        let mut next = String::with_capacity(self.search_text.len() + text.len());
        next.push_str(&self.search_text[..start_byte]);
        next.push_str(text);
        next.push_str(&self.search_text[end_byte..]);

        let cursor = start + text.encode_utf16().count();
        self.search_text = next;
        self.search_selection = cursor..cursor;
        // Any edit means the user is searching again; the previous jump marker is stale.
        self.highlight_row = None;
        self.scroll_anchor = None;
    }

    fn clear_search(&mut self) {
        self.search_text.clear();
        self.search_selection = 0..0;
    }

    /// Render a searchable setting row. The stable `id` is shared with the search catalog;
    /// highlight and scroll anchor are attached only while this row is the active target.
    fn target_row(
        &self,
        id: &'static str,
        t: &UiTheme,
        icon: Option<&'static str>,
        title: impl Into<SharedString>,
        control: impl IntoElement,
    ) -> AnyElement {
        let highlighted = self.highlight_row.as_deref() == Some(id);
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        setting_row_with_id(id, t, anchor, highlighted, icon, title, control).into_any_element()
    }

    /// Same treatment as [`Self::target_row`] for blocks that aren't setting rows
    /// (section descriptions, filter pills, widget placement rows when empty).
    fn mark_target(
        &self,
        id: &'static str,
        t: &UiTheme,
        el: gpui::Div,
    ) -> gpui::Stateful<gpui::Div> {
        let highlighted = self.highlight_row.as_deref() == Some(id);
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        let el = if highlighted {
            el.bg(rgba(t.accent_soft)).rounded(px(8.0))
        } else {
            el
        };
        el.id(SharedString::from(id)).anchor_scroll(anchor)
    }

    fn open_search_result(&mut self, entry: SettingsSearchEntry, cx: &mut Context<Self>) {
        self.clear_search();
        self.page = entry.page;
        if let Some(filter) = entry.widget_filter {
            self.widget_filter = filter;
        }
        self.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
        // A fresh anchor is attached to the target row on the next render. Prepaint records
        // that row's origin; the scroll scheduled in `render` then aligns it with the top of
        // the viewport. This works for nested rows, unlike index-based scroll APIs.
        self.highlight_row = Some(entry.row_id.clone());
        self.scroll_anchor = Some(ScrollAnchor::for_handle(self.scroll_handle.clone()));
        cx.notify();

        // Give GPUI a frame to lay out the new page/filter, force the frame that applies the
        // anchor offset, then let the highlight fade so it never sticks around forever.
        let target = entry.row_id;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            // `spawn` hands us an app context; refresh_windows schedules the frame that
            // runs the anchor callback registered during render.
            let _ = cx.update(|app| app.refresh_windows());
            cx.background_executor()
                .timer(Duration::from_millis(1800))
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.highlight_row.as_deref() == Some(target.as_str()) {
                    view.highlight_row = None;
                    view.scroll_anchor = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_search_results(
        &self,
        query: &str,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let catalog = settings_search::catalog();
        let matches = settings_search::search(&catalog, query);
        if matches.is_empty() {
            return vec![
                div()
                    .w_full()
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(rgba(t.border))
                    .bg(rgba(t.surface))
                    .px(px(18.0))
                    .py(px(22.0))
                    .text_size(px(fonts::scaled_size(13.0)))
                    .text_color(rgba(t.text_secondary))
                    .child(loc::t("SettingsSearchEmpty"))
                    .into_any_element(),
            ];
        }

        let mut blocks: Vec<AnyElement> = vec![
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(13.0)))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(loc::t("SettingsSearchResults")),
                )
                .child(badge(t, matches.len().to_string(), true))
                .into_any_element(),
        ];

        let hover_bg = t.surface_hover;
        let hover_border = t.accent;
        for (index, entry) in matches.into_iter().enumerate() {
            let target = entry.clone();
            let title = entry.title.clone();
            let location = format!("{} · {}", entry.page.title(), entry.section);
            blocks.push(
                div()
                    .id(SharedString::from(format!("search-result-{index}")))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(14.0))
                    .py(px(11.0))
                    .rounded(px(10.0))
                    .bg(rgba(t.surface))
                    .border_1()
                    .border_color(rgba(t.border))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgba(hover_bg)).border_color(rgba(hover_border)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_search_result(target.clone(), cx);
                    }))
                    .child(icon_glyph("\u{E721}", 13.0, t.accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(13.0)))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(rgba(t.text_primary))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .text_color(rgba(t.text_secondary))
                                    .child(location),
                            ),
                    )
                    .child(icon_glyph("\u{E76B}", 12.0, t.text_secondary))
                    .into_any_element(),
            );
        }
        blocks
    }

    /// Resolved once per render; `AppTheme::System` reads the registry, so widget preview cards
    /// must reuse this value instead of re-reading it for every card.
    fn resolve_is_dark(&self) -> bool {
        match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        }
    }

    fn auto_save(&mut self, cx: &mut Context<Self>) {
        self.widget_runtime.tick(&self.settings);
        if self.save_pending {
            return;
        }
        self.save_pending = true;
        cx.spawn(async move |this, cx| {
            // Slider drags and toggle bursts can fire dozens of times; persist once at the end.
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.save_pending {
                    view.save_pending = false;
                    view.sync_dock_pins();
                    view.save_settings(cx);
                }
            });
        })
        .detach();
    }

    fn render_widget_preview(&self, id: &str, t: &UiTheme, is_dark: bool) -> AnyElement {
        let pal = crate::design::palette(is_dark);
        let c_rgba = |c: (u8, u8, u8), a: u8| rgba(crate::design::gpui_rgba(c, a));
        let chip_bg = if is_dark { 0xffffff10 } else { 0x00000008 };
        let chip_bd = if is_dark { 0xffffff1c } else { 0x00000016 };
        let track_bg = if is_dark { 0xffffff1f } else { 0x00000018 };
        let spec = match id {
            "weather" => WidgetChipSpec::Weather {
                available: true,
                icon_color: pal.warning,
                is_rain: false,
                temp: "21°".to_owned(),
                place: if self.settings.weather_city.is_empty() {
                    "Istanbul".to_owned()
                } else {
                    self.settings.weather_city.clone()
                },
                condition: loc::t("WeatherClear"),
                today_range: Some("24° / 16°".to_owned()),
            },
            "stats" => WidgetChipSpec::SystemStats {
                cpu_text: "24%".to_owned(),
                cpu_color: pal.widget_blue,
                ram_text: "58%".to_owned(),
                ram_color: pal.widget_accent,
            },
            "netspeed" => WidgetChipSpec::NetSpeed {
                down: "18.4 MB/s".to_owned(),
                up: "3.2 MB/s".to_owned(),
            },
            "battery" => WidgetChipSpec::BatteryLiquid {
                present: true,
                percent: 84,
                charging: true,
                wave_phase: 0.35,
            },
            "bluetoothbattery" => WidgetChipSpec::Bluetooth {
                devices: vec![
                    (
                        widgets::telemetry::BluetoothDeviceKind::Headphones,
                        85,
                        false,
                        false,
                    ),
                    (
                        widgets::telemetry::BluetoothDeviceKind::Mouse,
                        64,
                        false,
                        false,
                    ),
                ],
            },
            "media" => WidgetChipSpec::Media {
                title: "Midnight City".to_owned(),
                artist: "M83".to_owned(),
                muted: false,
            },
            "audio" => WidgetChipSpec::Audio {
                volume: 68,
                muted: false,
            },
            "stopwatch" => WidgetChipSpec::Timed {
                is_focus: false,
                running: true,
                text: "04:18".to_owned(),
            },
            "countdown" => WidgetChipSpec::Timed {
                is_focus: false,
                running: false,
                text: "12:30".to_owned(),
            },
            "focus" => WidgetChipSpec::Timed {
                is_focus: true,
                running: true,
                text: "21:45".to_owned(),
            },
            "aiusage_all" => WidgetChipSpec::Simple {
                icon: crate::dock::dcomp::ChipIcon::Sparkle,
                icon_color: pal.widget_accent,
                top: "AI · 5".to_owned(),
                bottom: "Claude 42%".to_owned(),
            },
            "aiusage_claude" => WidgetChipSpec::AiDual {
                provider_id: "aiusage_claude",
                brand_color: Some((0xd9, 0x77, 0x57)),
                rows: vec![
                    ("5h".to_owned(), 0.42, pal.widget_accent, "2h".to_owned()),
                    ("7d".to_owned(), 0.68, pal.widget_accent, "4d".to_owned()),
                ],
            },
            "aiusage_codex" => WidgetChipSpec::AiDual {
                provider_id: "aiusage_codex",
                brand_color: Some((0x8d, 0x99, 0xff)),
                rows: vec![
                    ("5h".to_owned(), 0.28, pal.widget_accent, "3h".to_owned()),
                    ("7d".to_owned(), 0.54, pal.widget_accent, "5d".to_owned()),
                ],
            },
            "aiusage_cursor" => WidgetChipSpec::AiDual {
                provider_id: "aiusage_cursor",
                brand_color: None,
                rows: vec![("30d".to_owned(), 0.61, pal.widget_accent, "12d".to_owned())],
            },
            "aiusage_opencode" => WidgetChipSpec::AiOpenCode {
                columns: vec![
                    (
                        "1h".to_owned(),
                        "18%".to_owned(),
                        0.18,
                        pal.success,
                        "42m".to_owned(),
                    ),
                    (
                        "7d".to_owned(),
                        "49%".to_owned(),
                        0.49,
                        pal.success,
                        "3d".to_owned(),
                    ),
                    (
                        "30d".to_owned(),
                        "64%".to_owned(),
                        0.64,
                        pal.success,
                        "11d".to_owned(),
                    ),
                ],
            },
            "aiusage_copilot" => WidgetChipSpec::AiCopilot {
                value_text: "36%".to_owned(),
                meta_text: "14d left".to_owned(),
                used: 0.36,
                bar_color: (0x9f, 0x9f, 0xf2),
            },
            "aiusage_deepseek" => WidgetChipSpec::AiBalance {
                provider_id: "aiusage_deepseek",
                brand_color: (0x4d, 0x6b, 0xfe),
                name: "DeepSeek",
                value_text: "$14.80".to_owned(),
            },
            "aiusage_openrouter" => WidgetChipSpec::AiBalance {
                provider_id: "aiusage_openrouter",
                brand_color: (0xb7, 0x9b, 0xf8),
                name: "OpenRouter",
                value_text: "$22.40".to_owned(),
            },
            _ => self.widget_runtime.chip_spec(id, &self.settings, is_dark),
        };

        let chip = |inner: gpui::Div| -> AnyElement {
            div()
                .h(px(40.0))
                .max_w_full()
                .px(px(10.0))
                .rounded(px(9.0))
                .bg(rgba(chip_bg))
                .border_1()
                .border_color(rgba(chip_bd))
                .flex()
                .items_center()
                .gap(px(7.0))
                .overflow_hidden()
                .child(inner)
                .into_any_element()
        };

        let mini_bar = |ratio: f32, col: (u8, u8, u8), bar_w: f32| {
            let fill_w = (bar_w * ratio.clamp(0.0, 1.0))
                .round()
                .max(if ratio > 0.01 { 3.0 } else { 0.0 });
            div()
                .w(px(bar_w))
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(rgba(track_bg))
                .overflow_hidden()
                .child(
                    div()
                        .w(px(fill_w))
                        .h_full()
                        .rounded(px(2.0))
                        .bg(c_rgba(col, 0xff)),
                )
        };

        let vdiv = || {
            div()
                .w(px(1.0))
                .h(px(18.0))
                .bg(rgba(t.separator))
                .flex_shrink_0()
        };

        match spec {
            WidgetChipSpec::Clock {
                time,
                date,
                weekday,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_glyph("\u{E823}", 13.0, t.dock_preview_sub))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(14.0)))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(time),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(10.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(date),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(weekday),
                            ),
                    ),
            ),
            WidgetChipSpec::Weather {
                available,
                icon_color,
                is_rain,
                temp,
                place,
                condition,
                today_range,
            } => {
                if !available {
                    chip(
                        div()
                            .text_size(px(fonts::scaled_size(11.0)))
                            .text_color(rgba(t.dock_preview_sub))
                            .child(condition),
                    )
                } else {
                    let mut row = div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon_glyph(
                            if is_rain { "\u{E9C4}" } else { "\u{E753}" },
                            15.0,
                            crate::design::gpui_rgba(icon_color, 0xff),
                        ))
                        .child(
                            div()
                                .text_size(px(fonts::scaled_size(15.0)))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgba(t.dock_preview_fg))
                                .child(temp),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(10.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(place),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(condition),
                                ),
                        );
                    if let Some(range) = today_range {
                        row = row.child(vdiv()).child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(loc::t("Today")),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(10.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(range),
                                ),
                        );
                    }
                    chip(row)
                }
            }
            WidgetChipSpec::DayProgress {
                ratio,
                percent,
                unit,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(
                        div()
                            .size(px(18.0))
                            .rounded(px(9.0))
                            .border_2()
                            .border_color(c_rgba(pal.widget_blue, 0xff))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .size(px((8.0 * ratio.clamp(0.25, 1.0)).round()))
                                    .rounded(px(4.0))
                                    .bg(c_rgba(pal.widget_blue, 0xaa)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(percent),
                    )
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(10.0)))
                            .text_color(rgba(t.dock_preview_sub))
                            .child(unit),
                    ),
            ),
            WidgetChipSpec::Timed {
                is_focus,
                running,
                text,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_glyph(
                        if is_focus { "\u{E7C8}" } else { "\u{E916}" },
                        13.0,
                        if running {
                            crate::design::gpui_rgba(pal.selected_fg, 0xff)
                        } else {
                            t.dock_preview_sub
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(text),
                    )
                    .child(
                        div()
                            .size(px(22.0))
                            .rounded(px(6.0))
                            .bg(rgba(if running {
                                crate::design::gpui_rgba(pal.selected_fill, 0xff)
                            } else {
                                track_bg
                            }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_glyph(
                                if running { "\u{E769}" } else { "\u{E768}" },
                                10.0,
                                if running {
                                    crate::design::gpui_rgba(pal.selected_fg, 0xff)
                                } else {
                                    t.dock_preview_fg
                                },
                            )),
                    ),
            ),
            WidgetChipSpec::SystemStats {
                cpu_text,
                cpu_color,
                ram_text,
                ram_color,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child("CPU"),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(c_rgba(cpu_color, 0xff))
                                    .child(cpu_text),
                            ),
                    )
                    .child(vdiv())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child("RAM"),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(c_rgba(ram_color, 0xff))
                                    .child(ram_text),
                            ),
                    ),
            ),
            WidgetChipSpec::NetSpeed { down, up } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_glyph("\u{E701}", 13.0, t.dock_preview_sub))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(3.0))
                                    .child(icon_glyph(
                                        "\u{E74B}",
                                        9.0,
                                        crate::design::gpui_rgba(pal.success, 0xff),
                                    ))
                                    .child(
                                        div()
                                            .text_size(px(fonts::scaled_size(10.0)))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgba(t.dock_preview_fg))
                                            .child(down),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(3.0))
                                    .child(icon_glyph(
                                        "\u{E74A}",
                                        9.0,
                                        crate::design::gpui_rgba(pal.widget_blue, 0xff),
                                    ))
                                    .child(
                                        div()
                                            .text_size(px(fonts::scaled_size(10.0)))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgba(t.dock_preview_fg))
                                            .child(up),
                                    ),
                            ),
                    ),
            ),
            WidgetChipSpec::BatteryLiquid {
                present,
                percent,
                charging,
                ..
            } => {
                if !present {
                    chip(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(icon_glyph("\u{E850}", 14.0, t.dock_preview_sub))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child("—"),
                            ),
                    )
                } else {
                    let ratio = (percent as f32 / 100.0).clamp(0.02, 1.0);
                    let fill_col = if percent <= 20 {
                        (0xb2, 0x60, 0x67)
                    } else if percent <= 50 {
                        (0xb9, 0x95, 0x63)
                    } else {
                        (0x58, 0xa8, 0x9c)
                    };
                    let card_w = 94.0_f32;
                    let fill_w = (card_w * ratio).round();
                    div()
                        .relative()
                        .w(px(card_w))
                        .h(px(40.0))
                        .rounded(px(10.0))
                        .bg(rgba(chip_bg))
                        .border_1()
                        .border_color(rgba(chip_bd))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(px(fill_w))
                                .bg(c_rgba(fill_col, 0xf2)),
                        )
                        .child(
                            div()
                                .relative()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .when(charging, |row| {
                                    row.child(icon_glyph("\u{E945}", 11.0, 0xffffffff))
                                })
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(13.0)))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(rgba(0xffffffff))
                                        .child(format!("{percent}%")),
                                ),
                        )
                        .into_any_element()
                }
            }
            WidgetChipSpec::Bluetooth { devices } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .when(devices.is_empty(), |row| {
                        row.child(
                            div()
                                .text_size(px(fonts::scaled_size(11.0)))
                                .text_color(rgba(t.dock_preview_sub))
                                .child("—"),
                        )
                    })
                    .children(devices.into_iter().take(2).enumerate().map(
                        |(i, (kind, pct, low, full))| {
                            let val_col = if low {
                                crate::design::gpui_rgba(pal.warning, 0xff)
                            } else if full {
                                0x34d399ff
                            } else {
                                t.dock_preview_fg
                            };
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .when(i > 0, |el| el.child(vdiv()))
                                .child(icon_glyph(kind.fluent_glyph(), 13.0, t.dock_preview_sub))
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(11.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(val_col))
                                        .child(format!("{pct}%")),
                                )
                        },
                    )),
            ),
            WidgetChipSpec::Storage { drives } => {
                let (drv, cap) = drives
                    .first()
                    .cloned()
                    .unwrap_or_else(|| ("C:".to_owned(), "—".to_owned()));
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon_glyph("\u{EDA2}", 13.0, t.dock_preview_sub))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(10.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(drv),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(cap),
                                ),
                        ),
                )
            }
            WidgetChipSpec::Media {
                title,
                artist,
                muted,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(
                        div()
                            .max_w(px(86.0))
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(10.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(if artist.is_empty() {
                                        "Media".to_owned()
                                    } else {
                                        artist
                                    }),
                            ),
                    )
                    .child(vdiv())
                    .child(icon_glyph("\u{E892}", 10.0, t.dock_preview_fg))
                    .child(icon_glyph("\u{E768}", 11.0, t.dock_preview_fg))
                    .child(icon_glyph("\u{E893}", 10.0, t.dock_preview_fg))
                    .child(icon_glyph(
                        if muted { "\u{E74F}" } else { "\u{E767}" },
                        10.0,
                        t.dock_preview_sub,
                    )),
            ),
            WidgetChipSpec::Audio { volume, muted } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icon_glyph(
                        if muted { "\u{E74F}" } else { "\u{E767}" },
                        13.0,
                        if muted {
                            crate::design::gpui_rgba(pal.danger_fg, 0xff)
                        } else {
                            t.dock_preview_fg
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(if muted {
                                "Mute".to_owned()
                            } else {
                                format!("{volume}%")
                            }),
                    ),
            ),
            WidgetChipSpec::QuickSettings { pills } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .children(pills.into_iter().map(|(glyph, active)| {
                        div()
                            .size(px(24.0))
                            .rounded(px(6.0))
                            .bg(rgba(if active {
                                crate::design::gpui_rgba(pal.selected_fill, 0xff)
                            } else {
                                track_bg
                            }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .font_family("Segoe Fluent Icons")
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .text_color(rgba(if active {
                                        crate::design::gpui_rgba(pal.selected_fg, 0xff)
                                    } else {
                                        t.dock_preview_fg
                                    }))
                                    .child(glyph),
                            )
                    })),
            ),
            WidgetChipSpec::Keyboard { code } => chip(
                div()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(5.0))
                    .bg(rgba(track_bg))
                    .text_size(px(fonts::scaled_size(12.0)))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgba(t.dock_preview_fg))
                    .child(code),
            ),
            WidgetChipSpec::AiDual {
                provider_id,
                brand_color,
                rows,
            } => {
                let dot_col =
                    brand_color.unwrap_or_else(|| widgets::ai_provider_color(provider_id));
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .size(px(14.0))
                                .rounded(px(4.0))
                                .bg(c_rgba(dot_col, 0x33))
                                .border_1()
                                .border_color(c_rgba(dot_col, 0xcc)),
                        )
                        .child(div().flex().flex_col().gap(px(3.0)).children(
                            rows.into_iter().take(2).map(|(lbl, used, col, reset)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .child(
                                        div()
                                            .w(px(18.0))
                                            .text_size(px(fonts::scaled_size(9.0)))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(lbl),
                                    )
                                    .child(mini_bar(used, col, 48.0))
                                    .child(
                                        div()
                                            .text_size(px(fonts::scaled_size(9.0)))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(reset),
                                    )
                            }),
                        )),
                )
            }
            WidgetChipSpec::AiOpenCode { columns } => {
                chip(div().flex().items_center().gap(px(7.0)).children(
                    columns.into_iter().take(3).map(|(lbl, val, used, col, _)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(format!("{lbl} {val}")),
                            )
                            .child(mini_bar(used, col, 36.0))
                    }),
                ))
            }
            WidgetChipSpec::AiCopilot {
                value_text,
                meta_text,
                used,
                bar_color,
            } => chip(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(value_text),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(meta_text),
                            ),
                    )
                    .child(mini_bar(used, bar_color, 88.0)),
            ),
            WidgetChipSpec::AiBalance {
                provider_id,
                name,
                value_text,
                ..
            } => {
                let dot_col = widgets::ai_provider_color(provider_id);
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(
                            div()
                                .size(px(14.0))
                                .rounded(px(4.0))
                                .bg(c_rgba(dot_col, 0x33))
                                .border_1()
                                .border_color(c_rgba(dot_col, 0xcc)),
                        )
                        .child(vdiv())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(11.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(value_text),
                                ),
                        ),
                )
            }
            WidgetChipSpec::Simple {
                icon_color,
                top,
                bottom,
                ..
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_glyph(
                        widget_glyph(id),
                        13.0,
                        crate::design::gpui_rgba(icon_color, 0xff),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(top),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(bottom),
                            ),
                    ),
            ),
        }
    }

    fn sync_dock_pins(&mut self) {
        let saved = settings::load_from(&settings::settings_path());
        self.settings.user_pinned_apps = saved.user_pinned_apps;
        self.settings.user_unpinned_apps = saved.user_unpinned_apps;
        self.settings.user_pinned_background_apps = saved.user_pinned_background_apps;
        self.settings.pin_order = saved.pin_order;
    }

    /// Flushes a debounced write before the window goes away so a quick close cannot lose changes.
    fn flush_pending_save(&mut self, cx: &mut Context<Self>) {
        if self.save_pending {
            self.save_pending = false;
            self.sync_dock_pins();
            self.save_settings(cx);
        }
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        self.save_failed = settings::save_to(&settings::settings_path(), &self.settings).is_err();
        if !self.save_failed {
            notify_docks_reload();
        }
        cx.notify();
    }

    fn window_hwnd(window: &Window) -> usize {
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return 0;
        };
        match handle.as_raw() {
            RawWindowHandle::Win32(raw) => raw.hwnd.get() as usize,
            _ => 0,
        }
    }

    fn is_dark(&self) -> bool {
        match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        }
    }

    fn themes_root() -> std::path::PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("OpenTaskbar")
            .join("Themes")
    }

    fn import_theme(&mut self, cx: &mut Context<Self>) {
        let title = loc::t("ThemePackChooseFile");
        let label = loc::t("ThemePackImport");
        let Some(path) = prompt::ask_name(0, self.is_dark(), &title, &label, "") else {
            return;
        };
        let archive = std::path::PathBuf::from(path.trim());
        if !archive.is_file() {
            self.theme_status = loc::t("ThemePackMissing");
            cx.notify();
            return;
        }
        self.theme_status = match theme_packs::import_theme(&Self::themes_root(), &archive) {
            Ok(id) => loc::t_fmt("ThemePackImported", &[&id]),
            Err(error) => error,
        };
        cx.notify();
    }

    fn export_theme(&mut self, id: &str, cx: &mut Context<Self>) {
        let title = loc::t("ThemePackSaveFile");
        let label = loc::t("ThemePackExport");
        let Some(path) = prompt::ask_name(0, self.is_dark(), &title, &label, "") else {
            return;
        };
        self.theme_status = match theme_packs::export_theme(
            &Self::themes_root(),
            id,
            std::path::Path::new(path.trim()),
        ) {
            Ok(()) => loc::t("ThemePackExported"),
            Err(error) => error,
        };
        cx.notify();
    }

    fn delete_theme(&mut self, id: &str, cx: &mut Context<Self>) {
        self.theme_status = match theme_packs::delete_theme(&Self::themes_root(), id) {
            Ok(()) => loc::t("ThemePackDeleted"),
            Err(error) => error,
        };
        cx.notify();
    }

    fn record_hotkey(&mut self, launcher: bool, cx: &mut Context<Self>) {
        let title = loc::t(if launcher {
            "LauncherHotkey"
        } else {
            "DockHotkey"
        });
        let hint = loc::t("HotkeyCaptureHint");
        let current = if launcher {
            self.settings.launcher_binding()
        } else {
            self.settings.dock_binding()
        }
        .map(|binding| binding.display)
        .unwrap_or_else(|| loc::t("HotkeyDisabled"));
        let Some(text) = prompt::ask_name(0, self.is_dark(), &title, &hint, &current) else {
            return;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Esc") {
            return;
        }
        let value = if trimmed.eq_ignore_ascii_case("Backspace")
            || trimmed.eq_ignore_ascii_case("Disabled")
        {
            "Disabled".to_owned()
        } else {
            let Some(binding) = hotkey::parse(trimmed) else {
                self.hotkey_status = loc::t("HotkeyModifierRequired");
                cx.notify();
                return;
            };
            let other = if launcher {
                self.settings.dock_binding()
            } else {
                self.settings.launcher_binding()
            };
            if other.as_ref() == Some(&binding) {
                let other_name = if launcher {
                    "DockHotkey"
                } else {
                    "LauncherHotkey"
                };
                self.hotkey_status = loc::t_fmt("HotkeyConflict", &[&loc::t(other_name)]);
                cx.notify();
                return;
            }
            binding.display
        };
        if launcher {
            self.settings.custom_launcher_hotkey = Some(value);
        } else {
            self.settings.custom_dock_hotkey = Some(value);
        }
        self.hotkey_status.clear();
        self.auto_save(cx);
    }

    fn edit_weather_city(&mut self, cx: &mut Context<Self>) {
        let title = loc::t("WeatherCity");
        let hint = loc::t("WeatherCityHint");
        let current = self.settings.weather_city.clone();
        let Some(text) = prompt::ask_name(0, self.is_dark(), &title, &hint, &current) else {
            return;
        };
        let query = text.trim().to_owned();
        if query.is_empty() {
            self.weather_suggestions.clear();
            self.weather_search_status.clear();
            return;
        }
        let lang = self.settings.language.clone();
        self.weather_suggestions.clear();
        self.weather_search_status = loc::t("QuickLoading");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { widgets::telemetry::search_weather_locations(&query, &lang, 6) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(list) if list.len() == 1 => {
                        view.settings.weather_city = list[0].setting_value();
                        view.weather_suggestions.clear();
                        view.weather_search_status.clear();
                        view.auto_save(cx);
                    }
                    Ok(list) if !list.is_empty() => {
                        view.weather_suggestions = list;
                        view.weather_search_status.clear();
                        cx.notify();
                    }
                    Ok(_) => {
                        view.weather_suggestions.clear();
                        view.weather_search_status = loc::t("SettingsSearchEmpty");
                        cx.notify();
                    }
                    Err(err) => {
                        view.weather_suggestions.clear();
                        view.weather_search_status = err;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn edit_border_color(&mut self, surface: usize, cx: &mut Context<Self>) {
        let current = match surface {
            0 => self.settings.dock_border_color.clone(),
            1 => self.settings.menu_border_color.clone(),
            2 => self.settings.widget_border_color.clone(),
            _ => self.settings.settings_border_color.clone(),
        }
        .unwrap_or_default();
        let title = loc::t("BorderColorLabel");
        let hint = loc::t("BorderStyleHelp");
        let is_dark = self.is_dark();
        let Some(text) = prompt::ask_name(0, is_dark, &title, &hint, &current) else {
            return;
        };
        let text = text.trim().to_owned();
        if !text.is_empty() && parse_hex_color(&text).is_none() {
            return;
        }
        let slot = match surface {
            0 => &mut self.settings.dock_border_color,
            1 => &mut self.settings.menu_border_color,
            2 => &mut self.settings.widget_border_color,
            _ => &mut self.settings.settings_border_color,
        };
        *slot = if text.is_empty() { None } else { Some(text) };
        self.auto_save(cx);
    }

    fn edit_ai_api_key(&mut self, secret_key: &'static str, cx: &mut Context<Self>) {
        let title = loc::t("AiUsageApiKeyLabel");
        let hint = loc::t("AiUsageApiKeyPlaceholder");
        let is_dark = self.is_dark();
        let Some(text) = prompt::ask_name(0, is_dark, &title, &hint, "") else {
            return;
        };
        let value = text.trim();
        self.api_key_status = loc::t(if value.is_empty() {
            "ErrorAiUsageApiKey"
        } else if widgets::telemetry::save_dpapi_secret(secret_key, value) {
            "AiUsageApiKeySaved"
        } else {
            "ErrorAiUsageApiKey"
        });
        cx.notify();
    }

    fn clear_ai_api_key(&mut self, secret_key: &'static str, cx: &mut Context<Self>) {
        widgets::telemetry::clear_dpapi_secret(secret_key);
        self.api_key_status.clear();
        cx.notify();
    }

    /// Native paged font picker: Windows has hundreds of installed families, so the
    /// list is shown 24 at a time with Previous/Next entries instead of one huge menu.
    fn choose_font(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const PAGE_SIZE: usize = 24;
        const NATIVE_ID: u32 = 0;
        const PREV_ID: u32 = u32::MAX - 1;
        const NEXT_ID: u32 = u32::MAX;

        let fonts = fonts::installed_fonts();
        let max_page = fonts.len().saturating_sub(1) / PAGE_SIZE;
        self.font_page = self.font_page.min(max_page);
        let start = self.font_page * PAGE_SIZE;

        let mut entries = Vec::new();
        let native_label = format!("{} (Windows)", fonts::native_ui_face());
        let mut native = MenuEntry::item(NATIVE_ID, MenuIcon::Settings, native_label.clone());
        if self.settings.ui_font_family.trim().is_empty() {
            native = MenuEntry::with_shortcut(
                NATIVE_ID,
                MenuIcon::Settings,
                native_label.clone(),
                "\u{E73E}",
            );
        }
        entries.push(native);
        entries.push(MenuEntry::Separator);
        for (index, name) in fonts.iter().enumerate().skip(start).take(PAGE_SIZE) {
            let id = (index + 1) as u32;
            let mut entry = MenuEntry::item(id, MenuIcon::Settings, name.clone());
            if self.settings.ui_font_family.eq_ignore_ascii_case(name) {
                entry = MenuEntry::with_shortcut(id, MenuIcon::Settings, name.clone(), "\u{E73E}");
            }
            entries.push(entry);
        }
        entries.push(MenuEntry::Separator);
        if self.font_page > 0 {
            entries.push(MenuEntry::item(
                PREV_ID,
                MenuIcon::ChevronLeft,
                loc::t("DesktopPrevious"),
            ));
        }
        if self.font_page < max_page {
            entries.push(MenuEntry::item(
                NEXT_ID,
                MenuIcon::ChevronRight,
                loc::t("DesktopNext"),
            ));
        }

        let hwnd = Self::window_hwnd(window);
        let is_dark = self.is_dark();
        let chosen = menu::show_popup_menu_with_border(
            hwnd,
            is_dark,
            entries,
            BorderStyle::from_menu_settings(is_dark, &self.settings),
        )
        .unwrap_or(u32::MAX - 2);
        match chosen {
            NATIVE_ID => {
                self.settings.ui_font_family.clear();
                fonts::set_selected_family("");
                self.auto_save(cx);
            }
            PREV_ID => {
                self.font_page = self.font_page.saturating_sub(1);
                self.choose_font(window, cx);
            }
            NEXT_ID => {
                self.font_page = (self.font_page + 1).min(max_page);
                self.choose_font(window, cx);
            }
            id if id >= 1 && (id as usize) <= fonts.len() => {
                if let Some(name) = fonts.get(id as usize - 1) {
                    self.settings.ui_font_family = name.clone();
                    fonts::set_selected_family(name);
                    self.auto_save(cx);
                }
            }
            _ => {}
        }
    }

    fn surface_row(
        &self,
        t: &UiTheme,
        cx: &mut Context<Self>,
        id: &str,
        row_id: &'static str,
        label: String,
        current: SurfaceBackground,
        apply: fn(&mut DockSettings, SurfaceBackground),
    ) -> AnyElement {
        let track = segmented_track(
            t,
            [
                (loc::t("SurfaceGlass"), SurfaceBackground::Glass),
                (loc::t("SurfaceSolid"), SurfaceBackground::Solid),
                (loc::t("SurfaceNone"), SurfaceBackground::None),
            ]
            .into_iter()
            .map(|(text, background)| {
                segmented_item(
                    SharedString::from(format!("{id}-{background:?}")),
                    t,
                    text,
                    current == background,
                    cx.listener(move |this, _, _, cx| {
                        apply(&mut this.settings, background);
                        this.auto_save(cx);
                    }),
                )
            })
            .collect(),
        );
        self.target_row(row_id, t, Some("\u{E790}"), label, track)
    }

    fn new_profile(&mut self, cx: &mut Context<Self>) {
        let title = loc::t("ProfileNew");
        let label = loc::t("ProfileNamePrompt");
        let Some(name) = prompt::ask_name(0, true, &title, &label, "") else {
            return;
        };
        if self
            .settings
            .profiles
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(&name))
        {
            return;
        }
        self.settings
            .profiles
            .push(opentaskbar_core::settings::DockProfile {
                name: name.clone(),
                position: self.settings.position,
                size: self.settings.size,
                auto_hide: self.settings.auto_hide,
                enabled_widgets: self.settings.enabled_widgets.clone(),
            });
        self.settings.active_profile = Some(name);
        self.auto_save(cx);
    }

    fn rename_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(profile) = self.settings.profiles.get(index) else {
            return;
        };
        let old_name = profile.name.clone();
        let title = loc::t_fmt("ProfileRename", &[&old_name]);
        let label = loc::t("ProfileRenamePrompt");
        let Some(name) = prompt::ask_name(0, true, &title, &label, &old_name) else {
            return;
        };
        if name.is_empty()
            || self
                .settings
                .profiles
                .iter()
                .enumerate()
                .any(|(i, p)| i != index && p.name.eq_ignore_ascii_case(&name))
        {
            return;
        }
        if let Some(profile) = self.settings.profiles.get_mut(index) {
            profile.name = name.clone();
        }
        if self
            .settings
            .active_profile
            .as_deref()
            .is_some_and(|a| a == old_name)
        {
            self.settings.active_profile = Some(name);
        }
        self.auto_save(cx);
    }

    fn delete_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.settings.profiles.len() <= 1 {
            return;
        }
        let Some(name) = self.settings.profiles.get(index).map(|p| p.name.clone()) else {
            return;
        };
        self.settings.profiles.remove(index);
        if self
            .settings
            .active_profile
            .as_deref()
            .is_some_and(|a| a == name)
        {
            self.settings.active_profile = self.settings.profiles.first().map(|p| p.name.clone());
        }
        self.auto_save(cx);
    }

    fn check_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_status = loc::t("QuickLoading");
        self.update_release = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { update::find_newer() })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(Some(release)) => {
                        view.update_status = loc::t_fmt("UpdateAvailable", &[&release.version]);
                        view.update_release = Some(release);
                    }
                    Ok(None) => {
                        view.update_status =
                            loc::t_fmt("UpdateCurrent", &[update::CURRENT_VERSION]);
                    }
                    Err(()) => view.update_status = loc::t("ErrorUpdateCheck"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn download_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(release) = self.update_release.clone() else {
            return;
        };
        self.update_status = loc::t_fmt("UpdateDownloading", &[&release.version]);
        cx.notify();
        let task_release = release.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { update::download_and_apply(&task_release) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(()) => {
                        let title = loc::t("UpdateAvailableTitle");
                        let body = loc::t_fmt("UpdateToastBody", &[&release.version]);
                        let action = loc::t_fmt("UpdateDownload", &[&release.version]);
                        toast::show_update_toast(&title, &body, &action, &release.url);
                        // Release this process's lock so the updater can replace the executable.
                        std::process::exit(0);
                    }
                    Err(key) => view.update_status = loc::t(key),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_dock(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let align_seg = segmented_track(
            t,
            [
                (loc::t("Left"), DockPosition::Left),
                (loc::t("Center"), DockPosition::Center),
                (loc::t("Right"), DockPosition::Right),
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
                (loc::t("SizeCompact"), DockSize::Compact),
                (loc::t("SizeMedium"), DockSize::Medium),
                (loc::t("SizeLarge"), DockSize::Large),
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
                (loc::t("HotkeyDisabled"), AutoHideMode::Disabled),
                (loc::t("AutoHideSmart"), AutoHideMode::Smart),
                (loc::t("AutoHideAlways"), AutoHideMode::Always),
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
            || loc::t("DockWidthAuto"),
            |w| format!("{} ({:.0}px)", loc::t("DockWidthPixels"), w),
        );

        vec![
            section_card(
                t,
                format!("{} & {}", loc::t("SectionDockLayout"), loc::t("Dimensions")),
                vec![
                    self.target_row("DockAlignment", t, Some("\u{E8E3}"), loc::t("DockAlignment"), align_seg)
                        .into_any_element(),
                    self.target_row("DockSize", t, Some("\u{E740}"), loc::t("DockSize"), size_seg)
                        .into_any_element(),
                    self.target_row("DockScale", t, Some("\u{E8A3}"), loc::t("DockScale"),
                        live_slider(
                            "sc",
                            t,
                            ((self.settings.dock_scale as f32) - 0.75) / 0.75,
                            16,
                            format!("{scale_pct}%"),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (0.75 + (i as f64) * 0.05).clamp(0.75, 1.50);
                                        if (this.settings.dock_scale - next).abs() > 0.001 {
                                            this.settings.dock_scale = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    self.target_row("DockBottomGap", t, Some("\u{E74A}"), loc::t("DockBottomGap"),
                        live_slider(
                            "gap",
                            t,
                            self.settings.dock_bottom_gap_pixels as f32 / 32.0,
                            17,
                            format!("{} px", self.settings.dock_bottom_gap_pixels),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = ((i as i32) * 2).clamp(0, 32);
                                        if this.settings.dock_bottom_gap_pixels != next {
                                            this.settings.dock_bottom_gap_pixels = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    self.target_row("DockIconPadding", t, Some("\u{E9A9}"), loc::t("DockIconPadding"),
                        live_slider(
                            "pad",
                            t,
                            self.settings.dock_icon_padding as f32 / 12.0,
                            13,
                            format!("{} px", self.settings.dock_icon_padding),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (i as i32).clamp(0, 12);
                                        if this.settings.dock_icon_padding != next {
                                            this.settings.dock_icon_padding = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    self.target_row("LayoutFullWidth", t, Some("\u{E740}"), loc::t("LayoutFullWidth"),
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
                            loc::t("Reset"),
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
                format!(
                    "{} & {}",
                    loc::t("SectionBehavior"),
                    loc::t("SectionDisplays")
                ),
                vec![
                    self.target_row("AutoHide", t, Some("\u{E740}"), loc::t("AutoHide"), autohide_seg)
                        .into_any_element(),
                    self.target_row("AllMonitors", t, Some("\u{E7F4}"), loc::t("AllMonitors"),
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
                    self.target_row("FocusMode", t, Some("\u{E708}"), loc::t("FocusMode"),
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
                    self.target_row("RevealDockOnWinKey", t, Some("\u{E765}"), loc::t("RevealDockOnWinKey"),
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
                    self.target_row("CurrentDesktopOnly", t, Some("\u{E7F4}"), loc::t("CurrentDesktopOnly"),
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
                loc::t("SectionInteractions"),
                vec![
                    self.target_row("TrayApps", t, Some("\u{E70E}"), loc::t("TrayApps"),
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
                    self.target_row("RecycleBin", t, Some("\u{E74D}"), loc::t("RecycleBin"),
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
                    self.target_row("WidgetShowDesktop", t, Some("\u{E7F8}"), loc::t("WidgetShowDesktop"),
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
            section_card(
                t,
                loc::t("WidgetClock"),
                vec![
                    self.target_row("ClockFormat24Hour", t, Some("\u{E823}"), loc::t("ClockFormat24Hour"),
                        ios_switch(
                            "sw-24h",
                            t,
                            self.settings.clock_24_hour,
                            cx.listener(|this, _, _, cx| {
                                this.settings.clock_24_hour = !this.settings.clock_24_hour;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row("ClockShowSeconds", t, Some("\u{E823}"), loc::t("ClockShowSeconds"),
                        ios_switch(
                            "sw-seconds",
                            t,
                            self.settings.clock_show_seconds,
                            cx.listener(|this, _, _, cx| {
                                this.settings.clock_show_seconds =
                                    !this.settings.clock_show_seconds;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row("DateStyleDayShort", t, Some("\u{E823}"), loc::t("DateStyleDayShort"),
                        ios_switch(
                            "sw-weekday",
                            t,
                            self.settings.date_style == ClockDateStyle::DayAndShortDate,
                            cx.listener(|this, _, _, cx| {
                                this.settings.date_style = if this.settings.date_style
                                    == ClockDateStyle::DayAndShortDate
                                {
                                    ClockDateStyle::ShortDateOnly
                                } else {
                                    ClockDateStyle::DayAndShortDate
                                };
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row("ClockDateLayout", t, Some("\u{E823}"), loc::t("ClockDateLayout"),
                        segmented_track(
                            t,
                            [
                                (loc::t("ClockDateLayoutBeside"), ClockDateLayout::Beside),
                                (loc::t("ClockDateLayoutBelow"), ClockDateLayout::Below),
                            ]
                            .into_iter()
                            .map(|(label, layout)| {
                                segmented_item(
                                    SharedString::from(format!("date-layout-{label}")),
                                    t,
                                    label,
                                    self.settings.clock_date_layout == layout,
                                    cx.listener(move |this, _, _, cx| {
                                        this.settings.clock_date_layout = layout;
                                        this.auto_save(cx);
                                    }),
                                )
                            })
                            .collect(),
                        ),
                    )
                    .into_any_element(),
                    self.target_row("ClockDateFormat", t, Some("\u{E823}"), loc::t("ClockDateFormat"),
                        segmented_track(
                            t,
                            [
                                (loc::t("DateFormatSystem"), ClockDateFormat::System),
                                (loc::t("DateFormatDdMmYyyy"), ClockDateFormat::DdMmYyyy),
                                (loc::t("DateFormatYyyyMmDd"), ClockDateFormat::YyyyMmDd),
                            ]
                            .into_iter()
                            .map(|(label, format)| {
                                segmented_item(
                                    SharedString::from(format!("date-format-{label}")),
                                    t,
                                    label,
                                    self.settings.date_format == format,
                                    cx.listener(move |this, _, _, cx| {
                                        this.settings.date_format = format;
                                        this.auto_save(cx);
                                    }),
                                )
                            })
                            .collect(),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("StatsSettingsHeading"),
                vec![
                    self.target_row("Cpu", t, Some("\u{E9D9}"), loc::t("Cpu"),
                        ios_switch(
                            "sw-meter-cpu",
                            t,
                            self.settings.stats_meters & 1 != 0,
                            cx.listener(|this, _, _, cx| {
                                this.settings.stats_meters ^= 1;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row("Memory", t, Some("\u{E8B2}"), loc::t("Memory"),
                        ios_switch(
                            "sw-meter-mem",
                            t,
                            self.settings.stats_meters & 2 != 0,
                            cx.listener(|this, _, _, cx| {
                                this.settings.stats_meters ^= 2;
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
                (loc::t("ThemeWindowsAppearance"), AppTheme::System),
                (loc::t("Dark"), AppTheme::Dark),
                (loc::t("Light"), AppTheme::Light),
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
                (loc::t("SurfaceGlass"), SurfaceBackground::Glass),
                (loc::t("SurfaceSolid"), SurfaceBackground::Solid),
                (loc::t("SurfaceNone"), SurfaceBackground::None),
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
            .map_or_else(|| loc::t("Automatic"), |r| format!("{r} px"));

        vec![
            section_card(
                t,
                loc::t("SectionTheme"),
                vec![
                    self.target_row("Theme", t, Some("\u{E790}"), loc::t("Theme"), theme_seg).into_any_element(),
                    self.target_row("DockBackground", t, Some("\u{E790}"), loc::t("DockBackground"), bg_seg)
                        .into_any_element(),
                    self.target_row(
                        "SurfaceOpacityLabel:Dock",
                        t,
                        Some("\u{E706}"),
                        loc::t("SurfaceOpacityLabel"),
                        live_slider(
                            "op",
                            t,
                            ((self.settings.dock_opacity as f32) - 0.2) / 0.8,
                            17,
                            format!("{opacity_pct}%"),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (0.20 + (i as f64) * 0.05).clamp(0.20, 1.0);
                                        if (this.settings.dock_opacity - next).abs() > 0.001 {
                                            this.settings.dock_opacity = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    self.target_row("DockCornerRadius", t, Some("\u{F158}"), loc::t("DockCornerRadius"),
                        live_slider(
                            "rad",
                            t,
                            self.settings.dock_corner_radius.unwrap_or(16) as f32 / 32.0,
                            17,
                            radius_label,
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = Some(((i as i32) * 2).clamp(0, 32));
                                        if this.settings.dock_corner_radius != next {
                                            this.settings.dock_corner_radius = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("Animations"),
                vec![
                    self.target_row("Animations", t, Some("\u{E945}"), loc::t("Animations"),
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
                    self.target_row("Magnification", t, Some("\u{E71E}"), loc::t("Magnification"),
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
            section_card(
                t,
                loc::t("ThemeSectionTypography"),
                vec![
                    setting_row(
                        t,
                        Some("\u{E8D3}"),
                        loc::t("UiFontFamily"),
                        button(
                            "btn-ui-font",
                            t,
                            if self.settings.ui_font_family.trim().is_empty() {
                                loc::t("UiFontNative")
                            } else {
                                self.settings.ui_font_family.clone()
                            },
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, window, cx| this.choose_font(window, cx)),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E8D3}"),
                        loc::t("UiFontSize"),
                        live_slider(
                            "ui-font-size",
                            t,
                            (self.settings.ui_font_size - 9.0) / 15.0,
                            16,
                            format!("{} px", self.settings.ui_font_size.round() as i32),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (9.0 + i as f32).clamp(9.0, 24.0);
                                        if (this.settings.ui_font_size - next).abs() > 0.01 {
                                            this.settings.ui_font_size = next;
                                            fonts::apply_settings(&this.settings);
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .font_family(SharedString::from(
                            if self.settings.ui_font_family.trim().is_empty() {
                                fonts::native_ui_face().to_owned()
                            } else {
                                self.settings.ui_font_family.clone()
                            },
                        ))
                        .text_size(px(fonts::scaled_size(13.0)))
                        .child(loc::t("UiFontPreview"))
                        .child(" — AaBbÇğÖşü 0123")
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("MyThemes"),
                vec![
                    div()
                        .w_full()
                        .flex()
                        .gap(px(6.0))
                        .child(button(
                            "btn-theme-import",
                            t,
                            loc::t("ThemePackImport"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.import_theme(cx)),
                        ))
                        .child(button(
                            "btn-theme-folder",
                            t,
                            loc::t("ThemePackOpenFolder"),
                            ButtonVariant::Secondary,
                            cx.listener(|_, _, _, _| {
                                pins::launch(Self::themes_root().to_string_lossy().into_owned());
                            }),
                        ))
                        .into_any_element(),
                    {
                        let packs = theme_packs::discover(&Self::themes_root());
                        let mut rows = div().w_full().flex().flex_col().gap(px(6.0));
                        if packs.is_empty() {
                            rows = rows.child(
                                div()
                                    .text_size(px(fonts::scaled_size(12.0)))
                                    .text_color(rgba(t.text_secondary))
                                    .child(loc::t("NoCustomThemes")),
                            );
                        }
                        for pack in packs {
                            let id = pack.id.clone();
                            let export_id = id.clone();
                            let delete_id = id.clone();
                            let use_id = id.clone();
                            let active = self.settings.theme_pack.eq_ignore_ascii_case(&id);
                            rows = rows.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(fonts::scaled_size(12.0)))
                                            .child(pack.manifest.name.clone()),
                                    )
                                    .child(button(
                                        SharedString::from(format!("theme-use-{id}")),
                                        t,
                                        if active { loc::t("WidgetInstalled") } else { loc::t("ThemePackIcons") },
                                        ButtonVariant::Secondary,
                                        cx.listener(move |this, _, _, cx| {
                                            this.settings.theme_pack = use_id.clone();
                                            crate::platform::icon_pack::set_active_pack(Some(
                                                &Self::themes_root().join(&use_id),
                                            ));
                                            this.auto_save(cx);
                                        }),
                                    ))
                                    .child(button(
                                        SharedString::from(format!("theme-export-{id}")),
                                        t,
                                        loc::t("ThemePackExport"),
                                        ButtonVariant::Secondary,
                                        cx.listener(move |this, _, _, cx| {
                                            this.export_theme(&export_id, cx)
                                        }),
                                    ))
                                    .child(button(
                                        SharedString::from(format!("theme-delete-{id}")),
                                        t,
                                        loc::t("WidgetRemove"),
                                        ButtonVariant::Destructive,
                                        cx.listener(move |this, _, _, cx| {
                                            this.delete_theme(&delete_id, cx)
                                        }),
                                    )),
                            );
                        }
                        rows = rows.child(button(
                            "theme-use-native",
                            t,
                            loc::t("UiFontNative"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                this.settings.theme_pack.clear();
                                crate::platform::icon_pack::set_active_pack(None);
                                this.auto_save(cx);
                            }),
                        ));
                        rows.into_any_element()
                    },
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(self.theme_status.clone())
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("SectionSurfaces"),
                vec![
                    self.surface_row(
                        t,
                        cx,
                        "surf-dock",
                        "SurfaceDock",
                        loc::t("SurfaceDock"),
                        self.settings.dock_background,
                        |settings, value| settings.dock_background = value,
                    ),
                    self.surface_row(
                        t,
                        cx,
                        "surf-menu",
                        "SurfaceMenu",
                        loc::t("SurfaceMenu"),
                        self.settings.menu_background,
                        |settings, value| settings.menu_background = value,
                    ),
                    self.surface_row(
                        t,
                        cx,
                        "surf-widget",
                        "SurfaceWidget",
                        loc::t("SurfaceWidget"),
                        self.settings.widget_background,
                        |settings, value| settings.widget_background = value,
                    ),
                    self.surface_row(
                        t,
                        cx,
                        "surf-settings",
                        "SurfaceSettings",
                        loc::t("SurfaceSettings"),
                        self.settings.settings_background,
                        |settings, value| settings.settings_background = value,
                    ),
                    self.target_row(
                        "SurfaceOpacityLabel:Surfaces",
                        t,
                        Some("\u{E706}"),
                        loc::t("SurfaceOpacityLabel"),
                        live_slider(
                            "surf-op",
                            t,
                            ((self.settings.surface_opacity as f32) - 0.2) / 0.8,
                            17,
                            format!(
                                "{}%",
                                (self.settings.surface_opacity * 100.0).round() as i32
                            ),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (0.20 + (i as f64) * 0.05).clamp(0.20, 1.0);
                                        if (this.settings.surface_opacity - next).abs() > 0.001 {
                                            this.settings.surface_opacity = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            {
                let border_surfaces = [
                    (
                        "border-dock",
                        loc::t("SurfaceDock"),
                        self.settings.dock_border_thickness,
                        self.settings.dock_border_color.clone(),
                        0usize,
                    ),
                    (
                        "border-menu",
                        loc::t("SurfaceMenu"),
                        self.settings.menu_border_thickness,
                        self.settings.menu_border_color.clone(),
                        1usize,
                    ),
                    (
                        "border-widget",
                        loc::t("SurfaceWidget"),
                        self.settings.widget_border_thickness,
                        self.settings.widget_border_color.clone(),
                        2usize,
                    ),
                    (
                        "border-settings",
                        loc::t("SurfaceSettings"),
                        self.settings.settings_border_thickness,
                        self.settings.settings_border_color.clone(),
                        3usize,
                    ),
                ];
                let mut border_rows: Vec<AnyElement> = vec![
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("BorderStyleHelp"))
                        .into_any_element(),
                ];
                for (id, label, thickness, color, index) in border_surfaces {
                    let step = stepper(
                        SharedString::from(format!("{id}-minus")),
                        SharedString::from(format!("{id}-plus")),
                        t,
                        format!("{thickness:.1}"),
                        cx.listener(move |this, _, _, cx| {
                            let next = match index {
                                0 => this.settings.dock_border_thickness,
                                1 => this.settings.menu_border_thickness,
                                2 => this.settings.widget_border_thickness,
                                _ => this.settings.settings_border_thickness,
                            } - 0.5;
                            set_border_thickness(&mut this.settings, index, next);
                            this.auto_save(cx);
                        }),
                        cx.listener(move |this, _, _, cx| {
                            let next = match index {
                                0 => this.settings.dock_border_thickness,
                                1 => this.settings.menu_border_thickness,
                                2 => this.settings.widget_border_thickness,
                                _ => this.settings.settings_border_thickness,
                            } + 0.5;
                            set_border_thickness(&mut this.settings, index, next);
                            this.auto_save(cx);
                        }),
                    );
                    let color_id = SharedString::from(format!("{id}-color"));
                    border_rows.push(
                        setting_row(
                            t,
                            None,
                            label,
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(step)
                                .child(button(
                                    color_id,
                                    t,
                                    color.unwrap_or_else(|| "\u{E7A2}".to_owned()),
                                    ButtonVariant::Secondary,
                                    cx.listener(move |this, _, _, cx| {
                                        this.edit_border_color(index, cx)
                                    }),
                                )),
                        )
                        .into_any_element(),
                    );
                }
                section_card(t, loc::t("BorderStyleLabel"), border_rows).into_any_element()
            },
        ]
    }

    fn render_widget_card(
        &self,
        w: &'static widgets::WidgetDescriptor,
        t: &UiTheme,
        is_dark: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wid = w.id;
        let enabled = self.settings.is_widget_enabled(wid);
        let preview = self.render_widget_preview(wid, t, is_dark);
        let border_col = if enabled { t.accent } else { t.border };
        let card_hover = t.surface_hover;
        let focus_color = t.text_primary;
        let target_id = format!("widget-{wid}");
        let highlighted = self.highlight_row.as_deref() == Some(target_id.as_str());
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };

        let card = div()
            .flex_1()
            .min_w_0()
            .h(px(120.0))
            .flex()
            .flex_col()
            .justify_between()
            .gap(px(8.0))
            .p(px(11.0))
            .rounded(px(12.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(border_col))
            .cursor_pointer()
            .tab_index(0)
            .focus(move |style| style.border_color(rgba(focus_color)))
            .hover(move |s| s.bg(rgba(card_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let cur = this.settings.is_widget_enabled(wid);
                    if !cur && this.settings.focus_mode && wid != "clock" && wid != "focus" {
                        this.settings.focus_mode = false;
                    }
                    this.settings.set_widget_enabled(wid, !cur);
                    this.auto_save(cx);
                }),
            )
            .child(
                div()
                    .w_full()
                    .h(px(58.0))
                    .px(px(8.0))
                    .rounded(px(9.0))
                    .bg(rgba(t.dock_preview_bg))
                    .border_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .child(preview),
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
                                    .text_size(px(fonts::scaled_size(12.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.text_primary))
                                    .child(widgets::widget_name(w.id)),
                            ),
                    )
                    .when(enabled, |row| {
                        row.child(icon_glyph("\u{E73E}", 12.0, t.accent))
                    }),
            );
        // Applied last so the highlight wins over the normal surface background.
        let card = if highlighted {
            card.bg(rgba(t.accent_soft)).border_color(rgba(t.accent))
        } else {
            card
        };

        card.id(SharedString::from(format!("wcard-{wid}")))
            .anchor_scroll(anchor)
            .into_any_element()
    }

    fn render_widgets(
        &self,
        t: &UiTheme,
        is_dark: bool,
        columns: usize,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        let active_count = widgets::ALL_WIDGETS
            .iter()
            .filter(|w| self.settings.is_widget_enabled(w.id))
            .count();

        let filter_pills = div()
            .w_full()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(6.0))
            .children(
                std::iter::once("All")
                    .chain(std::iter::once("Active"))
                    .chain(widgets::WIDGET_CATEGORIES.iter().copied())
                    .map(|cat| {
                        let active = self.widget_filter == cat;
                        let bg = if active { t.accent } else { t.surface_muted };
                        let fg = if active {
                            t.accent_fg
                        } else {
                            t.text_secondary
                        };
                        let hov = if active { t.accent } else { t.surface_hover };
                        div()
                            .id(SharedString::from(format!("wflt-{cat}")))
                            .px(px(10.0))
                            .py(px(5.0))
                            .rounded(px(999.0))
                            .bg(rgba(bg))
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(if active {
                                gpui::FontWeight::SEMIBOLD
                            } else {
                                gpui::FontWeight::MEDIUM
                            })
                            .text_color(rgba(fg))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(hov)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.widget_filter = cat;
                                cx.notify();
                            }))
                            .child(category_label(cat))
                    }),
            );

        out.push(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(badge(
                            t,
                            loc::t_fmt(
                                "WidgetsActiveCount",
                                &[
                                    &active_count.to_string(),
                                    &widgets::ALL_WIDGETS.len().to_string(),
                                ],
                            ),
                            active_count > 0,
                        ))
                        .child(button(
                            "btn-clear-widgets",
                            t,
                            loc::t("ClearAll"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_clock_widget = false;
                                this.settings.show_system_stats_widget = false;
                                this.settings.enabled_widgets.clear();
                                this.auto_save(cx);
                            }),
                        )),
                )
                .child(filter_pills)
                .into_any_element(),
        );

        for &cat in widgets::WIDGET_CATEGORIES {
            if self.widget_filter != "All"
                && self.widget_filter != "Active"
                && self.widget_filter != cat
            {
                continue;
            }
            let items: Vec<&'static widgets::WidgetDescriptor> = widgets::ALL_WIDGETS
                .iter()
                .filter(|w| {
                    w.category == cat
                        && (self.widget_filter != "Active" || self.settings.is_widget_enabled(w.id))
                })
                .collect();
            if items.is_empty() {
                continue;
            }
            out.push(
                div()
                    .w_full()
                    .pt(px(4.0))
                    .px(px(2.0))
                    .text_size(px(fonts::scaled_size(12.0)))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgba(t.text_secondary))
                    .child(category_label(cat))
                    .into_any_element(),
            );
            for group in items.chunks(columns) {
                let mut row = div().w_full().flex().gap(px(10.0));
                for &w in group {
                    row = row.child(self.render_widget_card(w, t, is_dark, cx));
                }
                if group.len() < columns {
                    row = row.child(div().flex_1());
                }
                out.push(row.into_any_element());
            }
        }

        let density_options = [
            (WidgetDensityMode::Auto, loc::t("WidgetDensityAuto")),
            (WidgetDensityMode::Compact, loc::t("WidgetDensityCompact")),
            (WidgetDensityMode::Full, loc::t("WidgetDensityFull")),
        ];
        let density_segments = density_options
            .iter()
            .map(|(mode, label)| {
                let chosen = *mode;
                segmented_item(
                    SharedString::from(format!("density-{chosen:?}")),
                    t,
                    label.clone(),
                    self.settings.global_widget_density == chosen,
                    cx.listener(move |this, _, _, cx| {
                        this.settings.global_widget_density = chosen;
                        this.auto_save(cx);
                    }),
                )
            })
            .collect();
        out.push(
            section_card(
                t,
                loc::t("WidgetDensity"),
                vec![
                    setting_row(
                        t,
                        None,
                        loc::t("WidgetDensity"),
                        segmented_track(t, density_segments),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        );

        // AI provider account status, CLI sign-in hints and usage dashboards.
        let mut provider_rows: Vec<AnyElement> = vec![
            div()
                .w_full()
                .text_size(px(fonts::scaled_size(12.0)))
                .text_color(rgba(t.text_secondary))
                .child(loc::t("AiUsageProviderHint"))
                .into_any_element(),
        ];
        for provider in widgets::telemetry::AI_PROVIDERS {
            let detected = widgets::telemetry::ai_provider_detected(provider.id);
            let status = loc::t(if detected {
                "AiUsageAccountConnected"
            } else {
                "AiUsageAccountSignInRequired"
            });
            let url = provider.dashboard_url.to_owned();
            let id = SharedString::from(format!("btn-ai-dash-{}", provider.id));
            provider_rows.push(
                setting_row(
                    t,
                    None,
                    provider.name,
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(badge(t, status, detected))
                        .child(button(
                            id,
                            t,
                            loc::t("AiUsageOpenDashboard"),
                            ButtonVariant::Secondary,
                            move |_, _, _| {
                                pins::launch(url.clone());
                            },
                        )),
                )
                .into_any_element(),
            );
            provider_rows.push(
                div()
                    .w_full()
                    .pl(px(34.0))
                    .text_size(px(fonts::scaled_size(11.0)))
                    .text_color(rgba(t.text_secondary))
                    .child(format!(
                        "{}: {}",
                        loc::t("AiUsageCliSignInLabel"),
                        provider.signin_hint
                    ))
                    .into_any_element(),
            );
        }
        provider_rows.push(
            div()
                .h(px(1.0))
                .w_full()
                .bg(rgba(t.separator))
                .into_any_element(),
        );
        for (provider_id, secret_key) in widgets::telemetry::ai_api_key_providers() {
            let saved = widgets::telemetry::has_dpapi_secret(secret_key);
            let value = if saved {
                loc::t("AiUsageApiKeySaved")
            } else {
                loc::t("AiUsageApiKeyPlaceholder")
            };
            let key = *secret_key;
            provider_rows.push(
                setting_row(
                    t,
                    None,
                    format!("{} · {}", loc::t("AiUsageApiKeyLabel"), provider_id),
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .text_size(px(fonts::scaled_size(11.0)))
                                .text_color(rgba(t.text_secondary))
                                .child(value),
                        )
                        .child(button(
                            SharedString::from(format!("btn-api-key-{provider_id}")),
                            t,
                            loc::t("AiUsageApiKeySave"),
                            ButtonVariant::Secondary,
                            cx.listener(move |this, _, _, cx| this.edit_ai_api_key(key, cx)),
                        ))
                        .child(button(
                            SharedString::from(format!("btn-api-key-clear-{provider_id}")),
                            t,
                            loc::t("AiUsageApiKeyClear"),
                            ButtonVariant::Secondary,
                            cx.listener(move |this, _, _, cx| this.clear_ai_api_key(key, cx)),
                        )),
                )
                .into_any_element(),
            );
        }
        provider_rows.push(
            div()
                .w_full()
                .text_size(px(fonts::scaled_size(11.0)))
                .text_color(rgba(t.text_secondary))
                .child(self.api_key_status.clone())
                .into_any_element(),
        );
        provider_rows.push(
            div()
                .w_full()
                .text_size(px(fonts::scaled_size(11.0)))
                .text_color(rgba(t.text_secondary))
                .child(loc::t("AiUsageUnofficialNote"))
                .into_any_element(),
        );
        out.push(
            section_card(t, loc::t("AiUsageSettingsHeading"), provider_rows).into_any_element(),
        );
        out
    }

    fn render_dynamic_island(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let monitors = win32::enumerate_monitors();
        let active = widgets::active_widgets(&self.settings);
        let di_opacity_pct = (self.settings.dynamic_island_opacity * 100.0).round() as i32;

        let mut cards = vec![
            section_card(
                t,
                loc::t("DynamicIslandTitle"),
                vec![
                    setting_row(
                        t,
                        Some("\u{E7F4}"),
                        loc::t("DynamicIslandEnable"),
                        ios_switch(
                            "di-enable",
                            t,
                            self.settings.dynamic_island_enable,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dynamic_island_enable =
                                    !this.settings.dynamic_island_enable;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .pl(px(34.0))
                        .text_size(px(fonts::scaled_size(11.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("DynamicIslandEnableDescription"))
                        .into_any_element(),
                    self.target_row("DynamicIslandVisibility", t, Some("\u{E7F4}"), loc::t("DynamicIslandVisibility"),
                        segmented_track(
                            t,
                            [
                                (
                                    DynamicIslandVisibilityMode::Always,
                                    loc::t("DynamicIslandVisibilityAlways"),
                                ),
                                (
                                    DynamicIslandVisibilityMode::DesktopOnly,
                                    loc::t("DynamicIslandVisibilityDesktopOnly"),
                                ),
                            ]
                            .into_iter()
                            .map(|(mode, label)| {
                                segmented_item(
                                    SharedString::from(format!("di-vis-{mode:?}")),
                                    t,
                                    label,
                                    self.settings.dynamic_island_visibility == mode,
                                    cx.listener(move |this, _, _, cx| {
                                        this.settings.dynamic_island_visibility = mode;
                                        this.auto_save(cx);
                                    }),
                                )
                            })
                            .collect(),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .pl(px(34.0))
                        .text_size(px(fonts::scaled_size(11.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("DynamicIslandVisibilityDescription"))
                        .into_any_element(),
                    self.target_row("DynamicIslandHideFullscreen", t, Some("\u{E740}"), loc::t("DynamicIslandHideFullscreen"),
                        ios_switch(
                            "di-hide-fs",
                            t,
                            self.settings.dynamic_island_hide_in_fullscreen,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dynamic_island_hide_in_fullscreen =
                                    !this.settings.dynamic_island_hide_in_fullscreen;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .pl(px(34.0))
                        .text_size(px(fonts::scaled_size(11.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("DynamicIslandHideFullscreenDescription"))
                        .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E740}"),
                        loc::t("DynamicIslandAutoHide"),
                        ios_switch(
                            "di-auto-hide",
                            t,
                            self.settings.dynamic_island_auto_hide,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dynamic_island_auto_hide =
                                    !this.settings.dynamic_island_auto_hide;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .pl(px(34.0))
                        .text_size(px(fonts::scaled_size(11.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("DynamicIslandAutoHideDescription"))
                        .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E790}"),
                        loc::t("DynamicIslandBackground"),
                        segmented_track(
                            t,
                            [
                                (loc::t("SurfaceGlass"), SurfaceBackground::Glass),
                                (loc::t("SurfaceSolid"), SurfaceBackground::Solid),
                                (loc::t("SurfaceNone"), SurfaceBackground::None),
                            ]
                            .into_iter()
                            .map(|(label, bg)| {
                                segmented_item(
                                    SharedString::from(format!("di-bg-{bg:?}")),
                                    t,
                                    label,
                                    self.settings.dynamic_island_background == bg,
                                    cx.listener(move |this, _, _, cx| {
                                        this.settings.dynamic_island_background = bg;
                                        this.auto_save(cx);
                                    }),
                                )
                            })
                            .collect(),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .pl(px(34.0))
                        .text_size(px(fonts::scaled_size(11.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("DynamicIslandBackgroundDescription"))
                        .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E790}"),
                        loc::t("DynamicIslandUseTheme"),
                        ios_switch(
                            "di-use-theme",
                            t,
                            self.settings.dynamic_island_use_theme,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dynamic_island_use_theme =
                                    !this.settings.dynamic_island_use_theme;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E706}"),
                        // No `DynamicIslandOpacity` locale key exists; reuse the generic Opacity label.
                        loc::t("SurfaceOpacityLabel"),
                        live_slider(
                            "di-opacity",
                            t,
                            ((self.settings.dynamic_island_opacity as f32) - 0.1) / 0.9,
                            19,
                            format!("{di_opacity_pct}%"),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (0.10 + (i as f64) * 0.05).clamp(0.10, 1.0);
                                        if (this.settings.dynamic_island_opacity - next).abs()
                                            > 0.001
                                        {
                                            this.settings.dynamic_island_opacity = next;
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .pl(px(34.0))
                        .text_size(px(fonts::scaled_size(11.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("DynamicIslandOpacityDescription"))
                        .into_any_element(),
                ],
            )
            .into_any_element(),
        ];

        let mut placement_rows: Vec<AnyElement> = Vec::new();
        if active.is_empty() {
            placement_rows.push(
                self.mark_target(
                    "DynamicIslandWidgetLocation",
                    t,
                    div()
                        .w_full()
                        .px(px(14.0))
                        .py(px(12.0))
                        .text_size(px(12.5))
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("DynamicIslandWidgetLocationDescription")),
                )
                .into_any_element(),
            );
        } else {
            for (index, wd) in active.into_iter().enumerate() {
                let wid = wd.id;
                let current = self
                    .settings
                    .dynamic_island_placements
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(wid))
                    .map(|(_, v)| v.clone());

                let mut choices: Vec<(Option<String>, String)> = Vec::new();
                choices.push((None, loc::t("DynamicIslandDock")));
                choices.push((
                    Some(settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned()),
                    loc::t("DynamicIslandAllDisplays"),
                ));
                for mon in &monitors {
                    choices.push((Some(mon.device_name.clone()), mon.display_label()));
                }

                let track_items: Vec<AnyElement> = choices
                    .into_iter()
                    .enumerate()
                    .map(|(idx, (target, label))| {
                        let is_active = match (&current, &target) {
                            (None, None) => true,
                            (Some(cur), Some(tgt)) => cur.eq_ignore_ascii_case(tgt),
                            _ => false,
                        };
                        let wid_owned = wid.to_owned();
                        segmented_item(
                            SharedString::from(format!("di-place-{wid}-{idx}")),
                            t,
                            label,
                            is_active,
                            cx.listener(move |this, _, _, cx| {
                                this.settings
                                    .dynamic_island_placements
                                    .retain(|k, _| !k.eq_ignore_ascii_case(&wid_owned));
                                if let Some(ref dev) = target {
                                    this.settings
                                        .dynamic_island_placements
                                        .insert(wid_owned.clone(), dev.clone());
                                }
                                this.auto_save(cx);
                            }),
                        )
                    })
                    .collect();

                let control = segmented_track(t, track_items);
                placement_rows.push(if index == 0 {
                    // The catalog entry for this section targets its first widget row.
                    self.target_row(
                        "DynamicIslandWidgetLocation",
                        t,
                        Some(widget_glyph(wid)),
                        widgets::widget_name(wid),
                        control,
                    )
                } else {
                    setting_row(t, Some(widget_glyph(wid)), widgets::widget_name(wid), control)
                        .into_any_element()
                });
            }
        }

        cards.push(
            section_card(t, loc::t("DynamicIslandWidgetLocation"), placement_rows)
                .into_any_element(),
        );
        cards
    }

    fn render_preferences(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            section_card(
                t,
                loc::t("Language"),
                vec![
                    self.mark_target(
                        "Language",
                        t,
                        div()
                            .w_full()
                            .px(px(6.0))
                            .py(px(4.0))
                            .text_size(px(fonts::scaled_size(12.0)))
                            .text_color(rgba(t.text_secondary))
                            .child(loc::t("LanguageDescription")),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.0))
                        .children(LANGUAGES.iter().map(|(tag, _name)| {
                            let active = self.settings.language.eq_ignore_ascii_case(tag);
                            let bg = if active { t.accent } else { t.surface_muted };
                            let fg = if active { t.accent_fg } else { t.text_primary };
                            let hov = if active { t.accent } else { t.surface_hover };
                            let chosen = *tag;
                            div()
                                .id(SharedString::from(format!("lang-{tag}")))
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(px(8.0))
                                .bg(rgba(bg))
                                .text_size(px(fonts::scaled_size(12.0)))
                                .text_color(rgba(fg))
                                .cursor_pointer()
                                .hover(move |s| s.bg(rgba(hov)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.settings.language = chosen.to_owned();
                                    loc::set_language(chosen);
                                    this.auto_save(cx);
                                }))
                                .child(loc::display_name(tag))
                        }))
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("WeatherSettingsHeading"),
                vec![
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(loc::t("WeatherCity"))
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(11.0)))
                                        .text_color(rgba(t.text_secondary))
                                        .child(loc::t("WeatherCityHint")),
                                ),
                        )
                        .child(button(
                            "btn-weather-city",
                            t,
                            if self.settings.weather_city.trim().is_empty() {
                                loc::t("Edit")
                            } else {
                                self.settings.weather_city.clone()
                            },
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.edit_weather_city(cx)),
                        ))
                        .into_any_element(),
                    if !self.weather_search_status.is_empty() {
                        div()
                            .w_full()
                            .px(px(14.0))
                            .pb(px(10.0))
                            .text_size(px(fonts::scaled_size(11.5)))
                            .text_color(rgba(t.text_secondary))
                            .child(self.weather_search_status.clone())
                            .into_any_element()
                    } else if !self.weather_suggestions.is_empty() {
                        div()
                            .w_full()
                            .px(px(14.0))
                            .pb(px(12.0))
                            .flex()
                            .flex_wrap()
                            .gap(px(6.0))
                            .children(self.weather_suggestions.iter().enumerate().map(|(idx, loc_match)| {
                                let label = loc_match.display_label();
                                let value = loc_match.setting_value();
                                button(
                                    SharedString::from(format!("btn-weather-loc-{idx}")),
                                    t,
                                    label,
                                    ButtonVariant::Secondary,
                                    cx.listener(move |this, _, _, cx| {
                                        this.settings.weather_city = value.clone();
                                        this.weather_suggestions.clear();
                                        this.weather_search_status.clear();
                                        this.auto_save(cx);
                                    }),
                                )
                            }))
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    },
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("SectionHotkeys"),
                vec![
                    setting_row(
                        t,
                        Some("\u{E765}"),
                        loc::t("LauncherHotkey"),
                        button(
                            "btn-hotkey-launcher",
                            t,
                            self.settings
                                .launcher_binding()
                                .map(|binding| binding.display)
                                .unwrap_or_else(|| loc::t("HotkeyDisabled")),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.record_hotkey(true, cx)),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E765}"),
                        loc::t("DockHotkey"),
                        button(
                            "btn-hotkey-dock",
                            t,
                            self.settings
                                .dock_binding()
                                .map(|binding| binding.display)
                                .unwrap_or_else(|| loc::t("HotkeyDisabled")),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.record_hotkey(false, cx)),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(self.hotkey_status.clone())
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("Profiles"),
                vec![
                    self.mark_target(
                        "Profiles",
                        t,
                        div()
                            .w_full()
                            .flex()
                            .flex_wrap()
                            .gap(px(6.0))
                            .children(self.settings.profiles.iter().enumerate().map(
                            |(index, profile)| {
                                let profile_name = profile.name.clone();
                                let active = self
                                    .settings
                                    .active_profile
                                    .as_deref()
                                    .is_some_and(|name| name == profile.name);
                                let label = format!(
                                    "{} {}{}",
                                    profile.name,
                                    if active { "•" } else { "" },
                                    ""
                                );
                                div()
                                    .id(SharedString::from(format!("profile-{index}")))
                                    .px(px(10.0))
                                    .py(px(5.0))
                                    .rounded(px(8.0))
                                    .bg(rgba(if active { t.accent } else { t.surface_muted }))
                                    .text_color(rgba(if active {
                                        t.accent_fg
                                    } else {
                                        t.text_primary
                                    }))
                                    .text_size(px(fonts::scaled_size(12.0)))
                                    .cursor_pointer()
                                    .child(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.settings.active_profile = Some(profile_name.clone());
                                        this.auto_save(cx);
                                    }))
                            },
                        )),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .flex()
                        .gap(px(6.0))
                        .child(button(
                            "btn-profile-new",
                            t,
                            loc::t("ProfileNew"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.new_profile(cx)),
                        ))
                        .children(self.settings.profiles.iter().enumerate().take(12).map(
                            |(index, profile)| {
                                let rename_title = loc::t_fmt("ProfileRename", &[&profile.name]);
                                let delete_title = loc::t_fmt("ProfileDelete", &[&profile.name]);
                                div()
                                    .flex()
                                    .gap(px(4.0))
                                    .child(button(
                                        SharedString::from(format!("profile-rename-{index}")),
                                        t,
                                        rename_title,
                                        ButtonVariant::Secondary,
                                        cx.listener(move |this, _, _, cx| {
                                            this.rename_profile(index, cx)
                                        }),
                                    ))
                                    .child(button(
                                        SharedString::from(format!("profile-delete-{index}")),
                                        t,
                                        delete_title,
                                        ButtonVariant::Destructive,
                                        cx.listener(move |this, _, _, cx| {
                                            this.delete_profile(index, cx)
                                        }),
                                    ))
                            },
                        ))
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("CheckForUpdates"),
                vec![
                    self.target_row("CheckUpdatesSetting", t, Some("\u{E9C5}"), loc::t("CheckUpdatesSetting"),
                        ios_switch(
                            "sw-check-updates",
                            t,
                            self.settings.check_updates,
                            cx.listener(|this, _, _, cx| {
                                this.settings.check_updates = !this.settings.check_updates;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(self.update_status.clone())
                        .into_any_element(),
                    {
                        let mut controls = div().w_full().flex().gap(px(6.0)).child(button(
                            "btn-check-update",
                            t,
                            loc::t("CheckForUpdates"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, window, cx| this.check_updates(window, cx)),
                        ));
                        if let Some(release) = &self.update_release {
                            controls = controls.child(button(
                                "btn-download-update",
                                t,
                                loc::t_fmt("UpdateDownload", &[&release.version]),
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, window, cx| this.download_update(window, cx)),
                            ));
                        }
                        controls.into_any_element()
                    },
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("SectionSystem"),
                vec![
                    self.target_row("Startup", t, Some("\u{E7E8}"), loc::t("Startup"),
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
            section_card(
                t,
                loc::t("Backup"),
                vec![
                    self.target_row("SettingsBackup", t, Some("\u{EDE1}"), loc::t("SettingsBackup"),
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(button(
                                "btn-export",
                                t,
                                loc::t("ExportSettings"),
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
                                loc::t("ImportSettings"),
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, _, cx| {
                                    let src = settings::app_data_dir().join("settings.backup.json");
                                    if src.exists() {
                                        this.settings = settings::load_from(&src);
                                        fonts::apply_settings(&this.settings);
                                        loc::set_language(&this.settings.language);
                                        this.widget_runtime.tick(&this.settings);
                                        this.save_settings(cx);
                                    }
                                }),
                            )),
                    )
                    .into_any_element(),
                    self.target_row("ResetDefaults", t, Some("\u{E777}"), loc::t("ResetDefaults"),
                        button(
                            "btn-reset-all",
                            t,
                            loc::t("Reset"),
                            ButtonVariant::Destructive,
                            cx.listener(|this, _, _, cx| {
                                this.settings = DockSettings::default();
                                fonts::apply_settings(&this.settings);
                                loc::set_language(&this.settings.language);
                                this.widget_runtime.tick(&this.settings);
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
                loc::t("Application"),
                vec![
                    self.target_row("Version", t, Some("\u{E946}"), loc::t("Version"),
                        badge(t, concat!("v", env!("CARGO_PKG_VERSION")), true),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("Diagnostics"),
                vec![
                    self.target_row("DataFolder", t, Some("\u{E8B7}"), loc::t("DataFolder"),
                        button(
                            "btn-open-data",
                            t,
                            loc::t("Open"),
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(data_dir.clone()),
                        ),
                    )
                    .into_any_element(),
                    self.target_row("OpenLogFile", t, Some("\u{E9F5}"), loc::t("OpenLogFile"),
                        button(
                            "btn-open-log",
                            t,
                            loc::t("Open"),
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(log_file.clone()),
                        ),
                    )
                    .into_any_element(),
                    self.target_row("WindowsTaskbar", t, Some("\u{E777}"), loc::t("WindowsTaskbar"),
                        button(
                            "btn-restore-tb",
                            t,
                            loc::t("Restore"),
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

/// Backing buffer for the header search field. GPUI uses this to route typing, paste and
/// IME input to the focused input element.
impl EntityInputHandler for SettingsView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let len = self.search_utf16_len();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        if start != range.start || end != range.end {
            *adjusted_range = Some(start..end);
        }
        let from = Self::byte_index_for_utf16(&self.search_text, start);
        let to = Self::byte_index_for_utf16(&self.search_text, end);
        Some(self.search_text[from..to].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.search_selection.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.unwrap_or_else(|| self.search_selection.clone());
        self.replace_search_range(range, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.unwrap_or_else(|| self.search_selection.clone());
        self.replace_search_range(range, new_text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<gpui::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<gpui::Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<gpui::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_dark = self.resolve_is_dark();
        let mut t = UiTheme::new(is_dark);
        t.border_width = self.settings.settings_border_thickness.clamp(0.0, 8.0) as f32;
        if let Some(value) = &self.settings.settings_border_color
            && let Some(color) = parse_hex_color(value)
        {
            t.border = color;
        }
        let width = window.viewport_size().width;
        let narrow = width < px(SIDEBAR_BREAKPOINT);
        let inset = px(if narrow { 20.0 } else { 32.0 });
        let content_width = width - inset * 2.0 - px(if narrow { 0.0 } else { SIDEBAR_WIDTH });
        let make_nav = |p: SettingsPage, cx: &mut Context<Self>| -> AnyElement {
            sidebar_nav_item(
                SharedString::from(format!("nav-{}", p.title())),
                &t,
                p.glyph(),
                p.title(),
                None,
                self.page == p,
                cx.listener(move |this, _, _, cx| {
                    if this.page != p {
                        this.page = p;
                        this.highlight_row = None;
                        this.scroll_anchor = None;
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

        let search_query = self.search_text.trim().to_owned();
        let content_blocks = if !search_query.is_empty() {
            self.render_search_results(&search_query, &t, cx)
        } else {
            match self.page {
                SettingsPage::Dock => self.render_dock(&t, cx),
                SettingsPage::Widgets => {
                    let columns = if content_width < px(WIDGET_COLUMNS_BREAKPOINT) {
                        1
                    } else {
                        2
                    };
                    self.render_widgets(&t, is_dark, columns, cx)
                }
                SettingsPage::DynamicIsland => self.render_dynamic_island(&t, cx),
                SettingsPage::Themes => self.render_themes(&t, cx),
                SettingsPage::Preferences => self.render_preferences(&t, cx),
                SettingsPage::About => self.render_about(&t),
            }
        };

        // The target row (if any) now carries a clone of this anchor. Schedule its deferred
        // scroll and consume the anchor so subsequent renders don't keep jumping back.
        // `open_search_result` forces the frame in which the callback runs.
        if let Some(anchor) = self.scroll_anchor.take() {
            let entity = cx.entity().downgrade();
            anchor.scroll_to(window, cx);
            window.on_next_frame(move |_, cx| {
                // Repaint with the offset the anchor just applied.
                let _ = entity.update(cx, |_, cx| cx.notify());
            });
        }

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

        let header_title = if search_query.is_empty() {
            self.page.title()
        } else {
            loc::t("SettingsSearchResults")
        };
        let header_subtitle = if search_query.is_empty() {
            self.page.description()
        } else {
            search_query.clone()
        };

        let main = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("settings-page-header-drag")
                    .w_full()
                    .flex_shrink_0()
                    .px(inset)
                    .py(px(if narrow { 12.0 } else { 18.0 }))
                    .border_b_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                        win32::start_native_window_drag(window);
                    })
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(22.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(header_title),
                    )
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .text_color(rgba(t.text_secondary))
                            .child(header_subtitle),
                    ),
            )
            .when(self.save_failed, |panel| panel.child(
                div().px(inset).py(px(8.0)).bg(rgba(t.destructive_soft))
                    .text_color(rgba(t.destructive)).text_size(px(fonts::scaled_size(13.0)))
                    .child("Could not save settings. Check that the settings folder is writable and try again."),
            ))
            .child(scroll_wrap);

        // Header search: icon on the left, a native text input in the middle and a clear
        // button on the right. The input is outside the draggable titlebar region.
        let search_focused = self.search_focus.is_focused(window);
        let caret_byte = Self::byte_index_for_utf16(&self.search_text, self.search_selection.start);
        let (text_before, text_after) = self.search_text.split_at(caret_byte);
        let search_value = if self.search_text.is_empty() {
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(fonts::scaled_size(12.0)))
                .text_color(rgba(t.text_secondary))
                .child(loc::t("SettingsSearchPlaceholder"))
        } else {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .overflow_hidden()
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_primary))
                        .child(text_before.to_owned()),
                )
                .when(search_focused, |line| {
                    line.child(
                        div()
                            .flex_shrink_0()
                            .w(px(1.0))
                            .h(px(13.0))
                            .bg(rgba(t.accent)),
                    )
                })
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_primary))
                        .child(text_after.to_owned()),
                )
        };

        let clear_hover = t.surface_hover;
        let mut search_inner = div()
            .id("settings-search-field")
            .track_focus(&self.search_focus)
            .tab_index(0)
            .w(px(230.0))
            .max_w(px(280.0))
            .h(px(26.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .px(px(9.0))
            .rounded(px(7.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(if search_focused { t.accent } else { t.border }))
            .cursor_text()
            .focus({
                let accent = t.accent;
                move |style| style.border_color(rgba(accent))
            })
            .on_click(cx.listener(|this: &mut Self, _, window, cx| {
                this.search_focus.focus(window);
                cx.notify();
            }))
            .on_key_down(
                cx.listener(|this: &mut Self, event: &KeyDownEvent, window, cx| {
                    if !this.search_focus.is_focused(window) {
                        return;
                    }
                    match event.keystroke.key.as_str() {
                        "backspace" => {
                            let selection = this.search_selection.clone();
                            let end = selection.end.min(this.search_utf16_len());
                            if end > selection.start {
                                this.replace_search_range(selection.start..end, "");
                            } else if end > 0 {
                                this.replace_search_range(end - 1..end, "");
                            }
                            cx.notify();
                            cx.stop_propagation();
                        }
                        "escape" => {
                            this.clear_search();
                            this.highlight_row = None;
                            this.scroll_anchor = None;
                            this.focus.focus(window);
                            cx.notify();
                            cx.stop_propagation();
                        }
                        "enter" => {
                            if let Some(entry) = settings_search::search(
                                &settings_search::catalog(),
                                this.search_text.trim(),
                            )
                            .first()
                            .cloned()
                            {
                                this.open_search_result((*entry).clone(), cx);
                            }
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }),
            )
            .child(icon_glyph("\u{E721}", 13.0, t.text_secondary))
            .child(search_value);

        if !self.search_text.is_empty() {
            search_inner = search_inner.child(
                div()
                    .id("settings-search-clear")
                    .flex_shrink_0()
                    .w(px(17.0))
                    .h(px(17.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgba(clear_hover)))
                    .on_click(cx.listener(|this: &mut Self, _, window, cx| {
                        this.clear_search();
                        this.highlight_row = None;
                        this.scroll_anchor = None;
                        this.search_focus.focus(window);
                        cx.notify();
                    }))
                    .child(icon_glyph("\u{E711}", 10.0, t.text_secondary)),
            );
        }

        let search_field =
            TextInput::new(search_inner, self.search_focus.clone(), cx.entity()).into_any_element();

        let hover_cap = t.surface_hover;
        let close_hover = t.destructive;
        let titlebar = div()
            .w_full()
            .h(px(36.0))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .bg(rgba(t.sidebar_bg))
            .border_b_1()
            .border_color(rgba(t.border))
            .child(
                div()
                    .id("settings-titlebar-drag")
                    .flex_1()
                    .h_full()
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                        win32::start_native_window_drag(window);
                    })
                    .child(icon_glyph("\u{E713}", 13.0, t.text_secondary))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.text_primary))
                            .child(loc::t("SettingsTitle")),
                    ),
            )
            .child(div().flex_shrink_0().mx(px(12.0)).child(search_field))
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
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.flush_pending_save(cx);
                                window.remove_window();
                            }))
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
            .font_family(SharedString::from(
                if self.settings.ui_font_family.trim().is_empty() {
                    fonts::native_ui_face().to_owned()
                } else {
                    self.settings.ui_font_family.clone()
                },
            ))
            .text_size(px(fonts::scaled_size(14.0)))
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
                    title: Some(loc::t("SettingsTitle").into()),
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
