//! Unified 23-widget registry, runtime state, and shared `DockFlyout` builders matching C# OpenTaskbar.
//!
//! Categories (matching `DockWidgetService.cs`):
//! - `Time & Weather`   (5): `clock`, `weather`, `dayprogress`, `stopwatch`, `countdown`
//! - `System & Hardware`(5): `stats`, `netspeed`, `battery`, `bluetoothbattery`, `storage`
//! - `AI Quotas` (8): `aiusage_claude`, `aiusage_codex`, `aiusage_antigravity`,
//!   `aiusage_opencode`, `aiusage_copilot`, `aiusage_deepseek`,
//!   `aiusage_openrouter`, `aiusage_cursor`
//! - `Media & Controls` (4): `media`, `audio`, `quicksettings`, `keyboard`
//! - `Productivity`     (1): `focus`

pub mod telemetry;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use opentaskbar_core::settings::{AppTheme, AutoHideMode, DockSettings};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
};

use crate::design::{self, Rgb};
use crate::dock::dcomp::{ChipIcon, WidgetChipSpec};
use crate::platform::pins;
use crate::ui::flyout::{self, BarTone, FlyoutBlock, FlyoutSpec};

pub use telemetry::{
    active_keyboard_code, adjust_audio_volume, day_progress_metrics, empty_recycle_bin,
    get_audio_state, is_recycle_bin_non_empty, local_clock_strings, sample_battery, sample_drives,
    sample_ram_info, toggle_audio_mute,
};

pub const WIDGET_CATEGORIES: &[&str] = &[
    "Time & Weather",
    "System & Hardware",
    "AI Quotas",
    "Media & Controls",
    "Productivity",
];

#[derive(Clone, Copy, Debug)]
pub struct WidgetDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub width_dips: f32,
}

const fn wd(
    id: &'static str,
    name: &'static str,
    category: &'static str,
    width_dips: f32,
) -> WidgetDescriptor {
    WidgetDescriptor {
        id,
        name,
        category,
        width_dips,
    }
}

/// All 23 C# catalog widgets in exact C# category and declaration order, with archetype-matched widths.
pub const ALL_WIDGETS: &[WidgetDescriptor] = &[
    // Time & Weather
    wd("clock", "Clock & Date", "Time & Weather", 78.0),
    wd("weather", "Weather", "Time & Weather", 68.0),
    wd("dayprogress", "Day Progress", "Time & Weather", 66.0),
    wd("stopwatch", "Stopwatch", "Time & Weather", 72.0),
    wd("countdown", "Countdown", "Time & Weather", 72.0),
    // System & Hardware
    wd("stats", "System Stats", "System & Hardware", 80.0),
    wd("netspeed", "Net Speed", "System & Hardware", 78.0),
    wd("battery", "Battery", "System & Hardware", 64.0),
    wd(
        "bluetoothbattery",
        "Bluetooth Battery",
        "System & Hardware",
        66.0,
    ),
    wd("storage", "Storage", "System & Hardware", 72.0),
    // AI Quotas
    wd("aiusage_claude", "Claude", "AI Quotas", 70.0),
    wd("aiusage_codex", "Codex", "AI Quotas", 68.0),
    wd("aiusage_antigravity", "Antigravity", "AI Quotas", 84.0),
    wd("aiusage_opencode", "OpenCode", "AI Quotas", 80.0),
    wd("aiusage_copilot", "Copilot", "AI Quotas", 74.0),
    wd("aiusage_deepseek", "DeepSeek", "AI Quotas", 80.0),
    wd("aiusage_openrouter", "OpenRouter", "AI Quotas", 86.0),
    wd("aiusage_cursor", "Cursor", "AI Quotas", 70.0),
    // Media & Controls
    wd("media", "Media", "Media & Controls", 40.0),
    wd("audio", "Audio", "Media & Controls", 66.0),
    wd("quicksettings", "Quick Settings", "Media & Controls", 40.0),
    wd("keyboard", "Keyboard Layout", "Media & Controls", 44.0),
    // Productivity
    wd("focus", "Focus", "Productivity", 72.0),
];

pub fn ai_provider_color(id: &str) -> Rgb {
    match id {
        "aiusage_claude" => (0xd9, 0x77, 0x57),
        "aiusage_codex" => (0x8d, 0x99, 0xff),
        "aiusage_antigravity" => (0x38, 0xbd, 0xf8),
        "aiusage_opencode" => (0x22, 0xc5, 0x5e),
        "aiusage_copilot" => (0x9f, 0x9f, 0xf2),
        "aiusage_deepseek" => (0x4d, 0x6b, 0xfe),
        "aiusage_openrouter" => (0xb7, 0x9b, 0xf8),
        "aiusage_cursor" => (0xe4, 0xe4, 0xe7),
        _ => (0xa1, 0xa1, 0xaa),
    }
}

const STACKS_WIDGET: WidgetDescriptor = wd("stacks", "Folder Stacks", "System & Hardware", 58.0);
const GROUPS_WIDGET: WidgetDescriptor = wd("groups", "App Groups", "System & Hardware", 58.0);

pub fn widget_descriptor(id: &str) -> Option<&'static WidgetDescriptor> {
    if id.eq_ignore_ascii_case("stacks") {
        return Some(&STACKS_WIDGET);
    }
    if id.eq_ignore_ascii_case("groups") {
        return Some(&GROUPS_WIDGET);
    }
    ALL_WIDGETS.iter().find(|w| w.id.eq_ignore_ascii_case(id))
}

pub fn active_widgets(settings: &DockSettings) -> Vec<&'static WidgetDescriptor> {
    let mut out: Vec<&'static WidgetDescriptor> = ALL_WIDGETS
        .iter()
        .filter(|w| settings.is_widget_enabled(w.id))
        .collect();
    if !settings.stack_folders.is_empty() {
        out.push(&STACKS_WIDGET);
    }
    if !settings.app_groups.is_empty() {
        out.push(&GROUPS_WIDGET);
    }
    out
}

pub struct WidgetRuntime {
    stats: telemetry::SystemStatsSampler,
    net: telemetry::NetSpeedSampler,
    cpu_pct: u32,
    ram_pct: u32,
    audio_vol: u32,
    audio_muted: bool,
    stopwatch_running: bool,
    stopwatch_secs: u64,
    countdown_running: bool,
    countdown_total_secs: u64,
    countdown_remaining_secs: u64,
    focus_running: bool,
    focus_remaining_secs: u64,
    weather: Arc<Mutex<telemetry::WeatherCache>>,
    bt_battery: Arc<Mutex<telemetry::BluetoothBatteryCache>>,
    last_tick: Instant,
}

impl Default for WidgetRuntime {
    fn default() -> Self {
        Self {
            stats: telemetry::SystemStatsSampler::default(),
            net: telemetry::NetSpeedSampler::default(),
            cpu_pct: 0,
            ram_pct: 0,
            audio_vol: 50,
            audio_muted: false,
            stopwatch_running: false,
            stopwatch_secs: 0,
            countdown_running: false,
            countdown_total_secs: 15 * 60,
            countdown_remaining_secs: 15 * 60,
            focus_running: false,
            focus_remaining_secs: 25 * 60,
            weather: Arc::new(Mutex::new(telemetry::WeatherCache::default())),
            bt_battery: Arc::new(Mutex::new(telemetry::BluetoothBatteryCache::default())),
            last_tick: Instant::now(),
        }
    }
}

fn action_row(id: u32, title: impl Into<String>, badge: Option<String>) -> FlyoutBlock {
    FlyoutBlock::ActionRow {
        id,
        title: title.into(),
        subtitle: None,
        badge,
        icon: None,
        destructive: false,
    }
}

impl WidgetRuntime {
    pub fn tick(&mut self, settings: &DockSettings) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs();
        if dt > 0 {
            self.last_tick = now;
            if self.stopwatch_running {
                self.stopwatch_secs = self.stopwatch_secs.saturating_add(dt);
            }
            if self.countdown_running {
                self.countdown_remaining_secs = self.countdown_remaining_secs.saturating_sub(dt);
                if self.countdown_remaining_secs == 0 {
                    self.countdown_running = false;
                }
            }
            if self.focus_running {
                self.focus_remaining_secs = self.focus_remaining_secs.saturating_sub(dt);
                if self.focus_remaining_secs == 0 {
                    self.focus_running = false;
                }
            }
        }
        if settings.is_widget_enabled("stats") {
            (self.cpu_pct, self.ram_pct) = self.stats.sample();
        }
        if settings.is_widget_enabled("audio") {
            (self.audio_vol, self.audio_muted) = get_audio_state();
        }
        if settings.is_widget_enabled("netspeed") {
            self.net.sample();
        }
        if settings.is_widget_enabled("weather") {
            telemetry::refresh_weather(&self.weather, &settings.weather_city, false);
        }
        if settings.is_widget_enabled("bluetoothbattery") {
            telemetry::refresh_bluetooth_battery(&self.bt_battery, false);
        }
    }

    /// Builds the archetype-matched `WidgetChipSpec` rendered onto the dock pill.
    pub fn chip_spec(&self, id: &str, settings: &DockSettings, is_dark: bool) -> WidgetChipSpec {
        let pal = design::palette(is_dark);
        if let Some(ai) = telemetry::ai_provider_meta(id) {
            return WidgetChipSpec::Provider {
                label: ai.name.to_owned(),
                color: ai_provider_color(ai.id),
                detected: telemetry::ai_provider_detected(ai.id),
            };
        }
        match id {
            "clock" => {
                let (time, date) = local_clock_strings();
                WidgetChipSpec::DateTime { time, date }
            }
            "weather" => {
                let temp = self
                    .weather
                    .lock()
                    .ok()
                    .map(|g| g.temp_c.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "--°".to_owned());
                WidgetChipSpec::IconValue {
                    icon: ChipIcon::SunCloud,
                    value: temp,
                    active_tint: false,
                }
            }
            "dayprogress" => {
                let (day, _, _, _) = day_progress_metrics();
                WidgetChipSpec::ProgressRing {
                    ratio: day,
                    value: format!("{:.0}%", day * 100.0),
                    color: pal.accent,
                    active_tint: false,
                }
            }
            "stopwatch" => WidgetChipSpec::IconValue {
                icon: ChipIcon::Timer,
                value: format!(
                    "{:02}:{:02}",
                    self.stopwatch_secs / 60,
                    self.stopwatch_secs % 60
                ),
                active_tint: self.stopwatch_running,
            },
            "countdown" => {
                let ratio = if self.countdown_total_secs > 0 {
                    self.countdown_remaining_secs as f32 / self.countdown_total_secs as f32
                } else {
                    0.0
                };
                WidgetChipSpec::ProgressRing {
                    ratio,
                    value: format!(
                        "{:02}:{:02}",
                        self.countdown_remaining_secs / 60,
                        self.countdown_remaining_secs % 60
                    ),
                    color: pal.accent,
                    active_tint: self.countdown_running,
                }
            }
            "focus" => {
                let ratio = self.focus_remaining_secs as f32 / (25.0 * 60.0);
                WidgetChipSpec::ProgressRing {
                    ratio,
                    value: format!(
                        "{:02}:{:02}",
                        self.focus_remaining_secs / 60,
                        self.focus_remaining_secs % 60
                    ),
                    color: if settings.focus_mode {
                        pal.warning
                    } else {
                        pal.accent
                    },
                    active_tint: self.focus_running || settings.focus_mode,
                }
            }
            "stats" => WidgetChipSpec::DualMetric {
                top: format!("CPU {:>2}%", self.cpu_pct),
                bot: format!("RAM {:>2}%", self.ram_pct),
                top_color: None,
                bot_color: None,
                bars: Some((self.cpu_pct as f32 / 100.0, self.ram_pct as f32 / 100.0)),
            },
            "netspeed" => WidgetChipSpec::DualMetric {
                top: format!("↓ {}", telemetry::format_speed(self.net.rx_bps)),
                bot: format!("↑ {}", telemetry::format_speed(self.net.tx_bps)),
                top_color: None,
                bot_color: None,
                bars: None,
            },
            "battery" => {
                let b = sample_battery();
                let val = if !b.has_battery {
                    "AC".to_owned()
                } else {
                    format!("{}%", b.percent)
                };
                WidgetChipSpec::IconValue {
                    icon: ChipIcon::Battery,
                    value: val,
                    active_tint: b.charging,
                }
            }
            "bluetoothbattery" => {
                let val = self
                    .bt_battery
                    .lock()
                    .ok()
                    .and_then(|g| g.devices.first().map(|d| format!("{}%", d.percent)))
                    .unwrap_or_else(|| "--".to_owned());
                WidgetChipSpec::IconValue {
                    icon: ChipIcon::Bluetooth,
                    value: val,
                    active_tint: false,
                }
            }
            "storage" => {
                let val = sample_drives()
                    .first()
                    .map(|d| format!("{:.0}G", d.free_gb))
                    .unwrap_or_else(|| "--".to_owned());
                WidgetChipSpec::IconValue {
                    icon: ChipIcon::Drive,
                    value: val,
                    active_tint: false,
                }
            }
            "media" => WidgetChipSpec::IconOnly(ChipIcon::Music),
            "audio" => WidgetChipSpec::IconValue {
                icon: if self.audio_muted {
                    ChipIcon::SpeakerMuted
                } else {
                    ChipIcon::Speaker
                },
                value: if self.audio_muted {
                    "Mute".to_owned()
                } else {
                    format!("{}%", self.audio_vol)
                },
                active_tint: false,
            },
            "quicksettings" => WidgetChipSpec::IconOnly(ChipIcon::Sliders),
            "keyboard" => WidgetChipSpec::Keycap(active_keyboard_code()),
            "stacks" => WidgetChipSpec::IconValue {
                icon: ChipIcon::Folder,
                value: settings.stack_folders.len().to_string(),
                active_tint: false,
            },
            "groups" => WidgetChipSpec::IconValue {
                icon: ChipIcon::Grid,
                value: settings.app_groups.len().to_string(),
                active_tint: false,
            },
            _ => WidgetChipSpec::Keycap(id.chars().take(3).collect::<String>().to_uppercase()),
        }
    }

    #[allow(dead_code)]
    pub fn chip_lines(&self, id: &str, settings: &DockSettings) -> (String, String) {
        if let Some(ai) = telemetry::ai_provider_meta(id) {
            let status = if telemetry::ai_provider_detected(ai.id) {
                "Ready"
            } else {
                "Sign in"
            };
            return (ai.short.to_owned(), status.to_owned());
        }
        match id {
            "clock" => local_clock_strings(),
            "stats" => (
                format!("CPU {:>2}%", self.cpu_pct),
                format!("RAM {:>2}%", self.ram_pct),
            ),
            "audio" => {
                if self.audio_muted {
                    ("VOL --".to_owned(), "Muted".to_owned())
                } else {
                    (format!("VOL {}%", self.audio_vol), "Audio".to_owned())
                }
            }
            "media" => ("MEDIA".to_owned(), "Controls".to_owned()),
            "netspeed" => (
                format!("D {}", telemetry::format_speed(self.net.rx_bps)),
                format!("U {}", telemetry::format_speed(self.net.tx_bps)),
            ),
            "battery" => {
                let b = sample_battery();
                if !b.has_battery {
                    ("AC".to_owned(), "Desktop".to_owned())
                } else {
                    let sub = if b.charging {
                        "Charging"
                    } else if b.on_ac {
                        "Plugged"
                    } else {
                        "Battery"
                    };
                    (format!("BAT {}%", b.percent), sub.to_owned())
                }
            }
            "bluetoothbattery" => {
                if let Ok(g) = self.bt_battery.lock()
                    && let Some(first) = g.devices.first()
                {
                    return (
                        format!("BT {}%", first.percent),
                        first.name.chars().take(9).collect(),
                    );
                }
                ("BT --".to_owned(), "Devices".to_owned())
            }
            "storage" => sample_drives().first().map_or_else(
                || ("DISK".to_owned(), "--".to_owned()),
                |d| {
                    (
                        format!("{}: {:.0}G", d.letter, d.free_gb),
                        "Free".to_owned(),
                    )
                },
            ),
            "keyboard" => (active_keyboard_code(), "Layout".to_owned()),
            "dayprogress" => {
                let (day, _, _, left) = day_progress_metrics();
                (format!("DAY {:.0}%", day * 100.0), format!("{left}h left"))
            }
            "stopwatch" => (
                format!(
                    "{:02}:{:02}",
                    self.stopwatch_secs / 60,
                    self.stopwatch_secs % 60
                ),
                if self.stopwatch_running {
                    "Run"
                } else {
                    "Stop"
                }
                .to_owned(),
            ),
            "countdown" => (
                format!(
                    "{:02}:{:02}",
                    self.countdown_remaining_secs / 60,
                    self.countdown_remaining_secs % 60
                ),
                if self.countdown_running {
                    "Active"
                } else {
                    "Timer"
                }
                .to_owned(),
            ),
            "focus" => (
                format!(
                    "{:02}:{:02}",
                    self.focus_remaining_secs / 60,
                    self.focus_remaining_secs % 60
                ),
                if settings.focus_mode {
                    "Focus On"
                } else {
                    "Pomodoro"
                }
                .to_owned(),
            ),
            "weather" => self.weather.lock().map_or_else(
                |_| ("--°C".to_owned(), settings.weather_city.clone()),
                |g| {
                    (
                        if g.temp_c.is_empty() {
                            "--°C".to_owned()
                        } else {
                            g.temp_c.clone()
                        },
                        if g.city.is_empty() {
                            settings.weather_city.clone()
                        } else {
                            g.city.clone()
                        },
                    )
                },
            ),
            "quicksettings" => ("QUICK".to_owned(), "Panel".to_owned()),
            "stacks" => (
                "STACKS".to_owned(),
                format!("{} dir", settings.stack_folders.len()),
            ),
            "groups" => (
                "GROUPS".to_owned(),
                format!("{} grp", settings.app_groups.len()),
            ),
            _ => (id.to_uppercase(), "Widget".to_owned()),
        }
    }

    pub fn open_widget_flyout(
        &mut self,
        id: &str,
        owner_hwnd: usize,
        is_dark: bool,
        anchor_x: i32,
        anchor_y: i32,
        settings: &mut DockSettings,
    ) -> bool {
        if let Some(ai) = telemetry::ai_provider_meta(id) {
            let detected = telemetry::ai_provider_detected(ai.id);
            let spec = FlyoutSpec {
                title: ai.name.to_owned(),
                subtitle: None,
                width_dips: 276.0,
                blocks: vec![
                    FlyoutBlock::Hero {
                        value: if detected { "Connected" } else { "Sign In" }.to_owned(),
                        caption: Some(
                            if detected {
                                "Local session credentials detected"
                            } else {
                                "No local session found on this device"
                            }
                            .to_owned(),
                        ),
                    },
                    FlyoutBlock::Separator,
                    FlyoutBlock::ButtonBar(vec![(1, "Open Dashboard".to_owned(), true)]),
                ],
            };
            if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                pins::launch(ai.dashboard_url.to_owned());
            }
            return false;
        }

        match id {
            "clock" => {
                let (time, date) = local_clock_strings();
                let spec = FlyoutSpec {
                    title: "Clock".to_owned(),
                    subtitle: None,
                    width_dips: 276.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: time,
                            caption: Some(date),
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![
                            (1, "Notifications".to_owned(), false),
                            (2, "Date & Time".to_owned(), false),
                        ]),
                    ],
                };
                match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                    Some(1) => opentaskbar_shell::taskbar::open_notifications(),
                    Some(2) => pins::launch("ms-settings:dateandtime".to_owned()),
                    _ => {}
                }
            }
            "stats" => {
                let (ram_pct, used_gb, total_gb) = sample_ram_info();
                let spec = FlyoutSpec {
                    title: "System Stats".to_owned(),
                    subtitle: Some(format!("Uptime {}", telemetry::system_uptime_string())),
                    width_dips: 278.0,
                    blocks: vec![
                        FlyoutBlock::ProgressBar {
                            label: "CPU".to_owned(),
                            value_text: format!("{}%", self.cpu_pct),
                            ratio: self.cpu_pct as f32 / 100.0,
                            tone: BarTone::Usage,
                        },
                        FlyoutBlock::ProgressBar {
                            label: "Memory".to_owned(),
                            value_text: format!("{used_gb:.1} / {total_gb:.1} GB ({ram_pct}%)"),
                            ratio: ram_pct as f32 / 100.0,
                            tone: BarTone::Usage,
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(1, "Task Manager".to_owned(), true)]),
                    ],
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    pins::launch("taskmgr.exe".to_owned());
                }
            }
            "audio" => {
                (self.audio_vol, self.audio_muted) = get_audio_state();
                let spec = FlyoutSpec {
                    title: "Audio".to_owned(),
                    subtitle: None,
                    width_dips: 274.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: if self.audio_muted {
                                "Muted".to_owned()
                            } else {
                                format!("{}%", self.audio_vol)
                            },
                            caption: Some("Master Volume".to_owned()),
                        },
                        FlyoutBlock::ProgressBar {
                            label: "Level".to_owned(),
                            value_text: if self.audio_muted {
                                "0%".to_owned()
                            } else {
                                format!("{}%", self.audio_vol)
                            },
                            ratio: if self.audio_muted {
                                0.0
                            } else {
                                self.audio_vol as f32 / 100.0
                            },
                            tone: BarTone::Accent,
                        },
                        FlyoutBlock::ButtonBar(vec![
                            (1, "-10%".to_owned(), false),
                            (
                                2,
                                if self.audio_muted { "Unmute" } else { "Mute" }.to_owned(),
                                self.audio_muted,
                            ),
                            (3, "+10%".to_owned(), false),
                        ]),
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(4, "Sound Settings".to_owned(), false)]),
                    ],
                };
                match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                    Some(1) => adjust_audio_volume(-10),
                    Some(2) => toggle_audio_mute(),
                    Some(3) => adjust_audio_volume(10),
                    Some(4) => pins::launch("ms-settings:sound".to_owned()),
                    _ => {}
                }
                (self.audio_vol, self.audio_muted) = get_audio_state();
            }
            "media" => {
                let spec = FlyoutSpec {
                    title: "Media".to_owned(),
                    subtitle: Some("System playback controls".to_owned()),
                    width_dips: 274.0,
                    blocks: vec![
                        FlyoutBlock::ButtonBar(vec![
                            (1, "Previous".to_owned(), false),
                            (2, "Play / Pause".to_owned(), true),
                            (3, "Next".to_owned(), false),
                        ]),
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(4, "Sound Settings".to_owned(), false)]),
                    ],
                };
                match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                    Some(1) => telemetry::send_media_vk(VK_MEDIA_PREV_TRACK),
                    Some(2) => telemetry::send_media_vk(VK_MEDIA_PLAY_PAUSE),
                    Some(3) => telemetry::send_media_vk(VK_MEDIA_NEXT_TRACK),
                    Some(4) => pins::launch("ms-settings:sound".to_owned()),
                    _ => {}
                }
            }
            "netspeed" => {
                let spec = FlyoutSpec {
                    title: "Network".to_owned(),
                    subtitle: None,
                    width_dips: 272.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: format!("{}/s", telemetry::format_speed(self.net.rx_bps)),
                            caption: Some("Current download speed".to_owned()),
                        },
                        FlyoutBlock::StatRow {
                            label: "Upload".to_owned(),
                            value: format!("{}/s", telemetry::format_speed(self.net.tx_bps)),
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(1, "Network Settings".to_owned(), false)]),
                    ],
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    pins::launch("ms-settings:network".to_owned());
                }
            }
            "battery" => {
                let b = sample_battery();
                let status = if !b.has_battery {
                    "Desktop AC"
                } else if b.charging {
                    "Charging"
                } else if b.on_ac {
                    "Plugged in"
                } else {
                    "On battery"
                };
                let remain = b
                    .life_secs
                    .map(|s| format!("{}h {}m", s / 3600, (s % 3600) / 60))
                    .unwrap_or_else(|| "AC".to_owned());
                let spec = FlyoutSpec {
                    title: "Battery".to_owned(),
                    subtitle: Some(status.to_owned()),
                    width_dips: 268.0,
                    blocks: vec![
                        FlyoutBlock::ProgressBar {
                            label: "Charge".to_owned(),
                            value_text: format!("{}%", b.percent),
                            ratio: b.percent as f32 / 100.0,
                            tone: BarTone::Level,
                        },
                        FlyoutBlock::StatRow {
                            label: "Remaining".to_owned(),
                            value: remain,
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(1, "Power Settings".to_owned(), false)]),
                    ],
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    pins::launch("ms-settings:powersleep".to_owned());
                }
            }
            "bluetoothbattery" => {
                let devices = self
                    .bt_battery
                    .lock()
                    .map(|g| g.devices.clone())
                    .unwrap_or_default();
                let mut blocks: Vec<FlyoutBlock> = devices
                    .iter()
                    .map(|d| FlyoutBlock::ProgressBar {
                        label: d.name.clone(),
                        value_text: format!("{}%", d.percent),
                        ratio: d.percent as f32 / 100.0,
                        tone: BarTone::Level,
                    })
                    .collect();
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note(
                        "No Bluetooth battery devices reported.".to_owned(),
                    ));
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::ButtonBar(vec![
                    (1, "Refresh".to_owned(), false),
                    (2, "Bluetooth".to_owned(), false),
                ]));
                let spec = FlyoutSpec {
                    title: "Bluetooth Battery".to_owned(),
                    subtitle: None,
                    width_dips: 280.0,
                    blocks,
                };
                match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                    Some(1) => telemetry::refresh_bluetooth_battery(&self.bt_battery, true),
                    Some(2) => pins::launch("ms-settings:bluetooth".to_owned()),
                    _ => {}
                }
            }
            "storage" => {
                let drives = sample_drives();
                let mut blocks: Vec<FlyoutBlock> = drives
                    .iter()
                    .take(6)
                    .map(|d| {
                        let used = (d.total_gb - d.free_gb).max(0.0);
                        FlyoutBlock::ProgressBar {
                            label: format!("{}:", d.letter),
                            value_text: format!("{:.0} GB free / {:.0} GB", d.free_gb, d.total_gb),
                            ratio: (used / d.total_gb.max(1.0)) as f32,
                            tone: BarTone::Usage,
                        }
                    })
                    .collect();
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note("No drives found.".to_owned()));
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::ButtonBar(vec![(
                    1,
                    "File Explorer".to_owned(),
                    false,
                )]));
                let spec = FlyoutSpec {
                    title: "Storage".to_owned(),
                    subtitle: None,
                    width_dips: 286.0,
                    blocks,
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    pins::launch("explorer.exe".to_owned());
                }
            }
            "keyboard" => {
                let layouts = telemetry::installed_keyboard_layouts();
                let active = active_keyboard_code();
                let blocks: Vec<FlyoutBlock> = layouts
                    .iter()
                    .enumerate()
                    .map(|(i, (_, code))| {
                        action_row(
                            100 + i as u32,
                            code.clone(),
                            (code == &active).then(|| "Active".to_owned()),
                        )
                    })
                    .collect();
                let spec = FlyoutSpec {
                    title: "Keyboard Layout".to_owned(),
                    subtitle: None,
                    width_dips: 256.0,
                    blocks,
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                    && act >= 100
                    && let Some((hkl, _)) = layouts.get((act - 100) as usize)
                {
                    telemetry::activate_keyboard_layout(*hkl);
                }
            }
            "dayprogress" => {
                let (day, week, year, left) = day_progress_metrics();
                let spec = FlyoutSpec {
                    title: "Day Progress".to_owned(),
                    subtitle: None,
                    width_dips: 272.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: format!("{:.0}%", day * 100.0),
                            caption: Some(format!("{left}h remaining today")),
                        },
                        FlyoutBlock::ProgressBar {
                            label: "Day".to_owned(),
                            value_text: format!("{:.0}%", day * 100.0),
                            ratio: day,
                            tone: BarTone::Accent,
                        },
                        FlyoutBlock::ProgressBar {
                            label: "Week".to_owned(),
                            value_text: format!("{:.0}%", week * 100.0),
                            ratio: week,
                            tone: BarTone::Accent,
                        },
                        FlyoutBlock::ProgressBar {
                            label: "Year".to_owned(),
                            value_text: format!("{:.0}%", year * 100.0),
                            ratio: year,
                            tone: BarTone::Accent,
                        },
                    ],
                };
                let _ = flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec);
            }
            "stopwatch" => {
                let (h, m, s) = (
                    self.stopwatch_secs / 3600,
                    (self.stopwatch_secs % 3600) / 60,
                    self.stopwatch_secs % 60,
                );
                let spec = FlyoutSpec {
                    title: "Stopwatch".to_owned(),
                    subtitle: None,
                    width_dips: 256.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: format!("{h:02}:{m:02}:{s:02}"),
                            caption: Some(
                                if self.stopwatch_running {
                                    "Running"
                                } else {
                                    "Paused"
                                }
                                .to_owned(),
                            ),
                        },
                        FlyoutBlock::ButtonBar(vec![
                            (
                                1,
                                if self.stopwatch_running {
                                    "Pause"
                                } else {
                                    "Start"
                                }
                                .to_owned(),
                                true,
                            ),
                            (2, "Reset".to_owned(), false),
                        ]),
                    ],
                };
                match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                    Some(1) => self.stopwatch_running = !self.stopwatch_running,
                    Some(2) => {
                        self.stopwatch_running = false;
                        self.stopwatch_secs = 0;
                    }
                    _ => {}
                }
            }
            "countdown" => {
                let ratio = if self.countdown_total_secs > 0 {
                    1.0 - (self.countdown_remaining_secs as f32 / self.countdown_total_secs as f32)
                } else {
                    0.0
                };
                let spec = FlyoutSpec {
                    title: "Countdown".to_owned(),
                    subtitle: None,
                    width_dips: 266.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: format!(
                                "{:02}:{:02}",
                                self.countdown_remaining_secs / 60,
                                self.countdown_remaining_secs % 60
                            ),
                            caption: Some(
                                if self.countdown_running {
                                    "Counting down"
                                } else {
                                    "Ready"
                                }
                                .to_owned(),
                            ),
                        },
                        FlyoutBlock::ProgressBar {
                            label: "Progress".to_owned(),
                            value_text: format!("{:.0}%", ratio * 100.0),
                            ratio,
                            tone: BarTone::Accent,
                        },
                        FlyoutBlock::ButtonBar(vec![
                            (
                                1,
                                if self.countdown_running {
                                    "Pause"
                                } else {
                                    "Start"
                                }
                                .to_owned(),
                                true,
                            ),
                            (2, "Reset".to_owned(), false),
                        ]),
                        FlyoutBlock::ButtonBar(vec![
                            (3, "5m".to_owned(), false),
                            (4, "15m".to_owned(), false),
                            (5, "25m".to_owned(), false),
                        ]),
                    ],
                };
                match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                    Some(1) => self.countdown_running = !self.countdown_running,
                    Some(2) => {
                        self.countdown_running = false;
                        self.countdown_remaining_secs = self.countdown_total_secs;
                    }
                    Some(3) => {
                        self.countdown_total_secs = 5 * 60;
                        self.countdown_remaining_secs = 5 * 60;
                        self.countdown_running = true;
                    }
                    Some(4) => {
                        self.countdown_total_secs = 15 * 60;
                        self.countdown_remaining_secs = 15 * 60;
                        self.countdown_running = true;
                    }
                    Some(5) => {
                        self.countdown_total_secs = 25 * 60;
                        self.countdown_remaining_secs = 25 * 60;
                        self.countdown_running = true;
                    }
                    _ => {}
                }
            }
            "focus" => {
                let ratio = 1.0 - (self.focus_remaining_secs as f32 / (25.0 * 60.0));
                let spec = FlyoutSpec {
                    title: "Focus".to_owned(),
                    subtitle: None,
                    width_dips: 274.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: format!(
                                "{:02}:{:02}",
                                self.focus_remaining_secs / 60,
                                self.focus_remaining_secs % 60
                            ),
                            caption: Some(
                                if self.focus_running {
                                    "Pomodoro active"
                                } else {
                                    "25-minute session"
                                }
                                .to_owned(),
                            ),
                        },
                        FlyoutBlock::ProgressBar {
                            label: "Session".to_owned(),
                            value_text: format!("{:.0}%", ratio * 100.0),
                            ratio,
                            tone: BarTone::Accent,
                        },
                        FlyoutBlock::ButtonBar(vec![
                            (
                                1,
                                if self.focus_running { "Pause" } else { "Start" }.to_owned(),
                                true,
                            ),
                            (2, "Reset".to_owned(), false),
                        ]),
                        FlyoutBlock::Separator,
                        action_row(
                            3,
                            "Focus Mode",
                            Some(if settings.focus_mode { "On" } else { "Off" }.to_owned()),
                        ),
                    ],
                };
                match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                    Some(1) => self.focus_running = !self.focus_running,
                    Some(2) => {
                        self.focus_running = false;
                        self.focus_remaining_secs = 25 * 60;
                    }
                    Some(3) => {
                        settings.focus_mode = !settings.focus_mode;
                        return true;
                    }
                    _ => {}
                }
            }
            "weather" => {
                let snap = self.weather.lock().map(|g| g.clone()).unwrap_or_default();
                let city = if snap.city.is_empty() {
                    settings.weather_city.clone()
                } else {
                    snap.city
                };
                let spec = FlyoutSpec {
                    title: city,
                    subtitle: None,
                    width_dips: 268.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: if snap.temp_c.is_empty() {
                                "--"
                            } else {
                                &snap.temp_c
                            }
                            .to_owned(),
                            caption: (!snap.condition.is_empty()).then_some(snap.condition),
                        },
                        FlyoutBlock::StatRow {
                            label: "Feels Like".to_owned(),
                            value: if snap.feels_like.is_empty() {
                                "--"
                            } else {
                                &snap.feels_like
                            }
                            .to_owned(),
                        },
                        FlyoutBlock::StatRow {
                            label: "Humidity".to_owned(),
                            value: if snap.humidity.is_empty() {
                                "--"
                            } else {
                                &snap.humidity
                            }
                            .to_owned(),
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(1, "Refresh".to_owned(), false)]),
                    ],
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    telemetry::refresh_weather(&self.weather, &settings.weather_city, true);
                }
            }
            "quicksettings" => {
                let (vol, muted) = get_audio_state();
                let hide_label = match settings.auto_hide {
                    AutoHideMode::Disabled => "Off",
                    AutoHideMode::Smart => "Smart",
                    AutoHideMode::Always => "Always",
                };
                let theme_label = match settings.theme {
                    AppTheme::System => "System",
                    AppTheme::Dark => "Dark",
                    AppTheme::Light => "Light",
                };
                let spec = FlyoutSpec {
                    title: "Quick Settings".to_owned(),
                    subtitle: None,
                    width_dips: 292.0,
                    blocks: vec![
                        FlyoutBlock::ToggleGrid(vec![
                            (
                                1,
                                "Focus Mode".to_owned(),
                                if settings.focus_mode { "On" } else { "Off" }.to_owned(),
                                settings.focus_mode,
                            ),
                            (
                                2,
                                "Auto-Hide".to_owned(),
                                hide_label.to_owned(),
                                settings.auto_hide != AutoHideMode::Disabled,
                            ),
                            (
                                3,
                                "Theme".to_owned(),
                                theme_label.to_owned(),
                                settings.theme != AppTheme::System,
                            ),
                            (
                                4,
                                "Sound".to_owned(),
                                if muted {
                                    "Muted".to_owned()
                                } else {
                                    format!("{vol}%")
                                },
                                !muted,
                            ),
                        ]),
                        FlyoutBlock::ProgressBar {
                            label: "Master Volume".to_owned(),
                            value_text: if muted {
                                "Muted".to_owned()
                            } else {
                                format!("{vol}%")
                            },
                            ratio: if muted { 0.0 } else { vol as f32 / 100.0 },
                            tone: BarTone::Accent,
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![
                            (5, "Settings".to_owned(), true),
                            (6, "Show Desktop".to_owned(), false),
                        ]),
                    ],
                };
                match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                    Some(1) => {
                        settings.focus_mode = !settings.focus_mode;
                        return true;
                    }
                    Some(2) => {
                        settings.auto_hide = match settings.auto_hide {
                            AutoHideMode::Disabled => AutoHideMode::Smart,
                            AutoHideMode::Smart => AutoHideMode::Always,
                            AutoHideMode::Always => AutoHideMode::Disabled,
                        };
                        return true;
                    }
                    Some(3) => {
                        settings.theme = match settings.theme {
                            AppTheme::System => AppTheme::Dark,
                            AppTheme::Dark => AppTheme::Light,
                            AppTheme::Light => AppTheme::System,
                        };
                        return true;
                    }
                    Some(4) => toggle_audio_mute(),
                    Some(5) => crate::ui::settings::spawn_settings_window(),
                    Some(6) => opentaskbar_shell::taskbar::show_desktop(),
                    _ => {}
                }
            }
            "stacks" => {
                let mut blocks = Vec::new();
                let mut paths = Vec::new();
                for folder in &settings.stack_folders {
                    let name = std::path::Path::new(folder)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(folder)
                        .to_owned();
                    let id = 100 + paths.len() as u32;
                    paths.push(folder.clone());
                    blocks.push(action_row(id, name, None));
                    if let Ok(entries) = std::fs::read_dir(folder) {
                        for entry in entries.flatten().take(4) {
                            let p = entry.path();
                            let fname = p
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("File")
                                .to_owned();
                            let fid = 100 + paths.len() as u32;
                            paths.push(p.to_string_lossy().into_owned());
                            blocks.push(action_row(fid, format!("  {fname}"), None));
                        }
                    }
                }
                let spec = FlyoutSpec {
                    title: "Folder Stacks".to_owned(),
                    subtitle: None,
                    width_dips: 280.0,
                    blocks,
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                    && act >= 100
                    && let Some(target) = paths.get((act - 100) as usize)
                {
                    pins::launch(target.clone());
                }
            }
            "groups" => {
                let mut blocks = Vec::new();
                let mut launch_list: Vec<Vec<String>> = Vec::new();
                for group in &settings.app_groups {
                    let id = 100 + launch_list.len() as u32;
                    launch_list.push(group.executable_paths.clone());
                    blocks.push(action_row(
                        id,
                        group.name.clone(),
                        Some(format!("{} apps", group.executable_paths.len())),
                    ));
                }
                let spec = FlyoutSpec {
                    title: "App Groups".to_owned(),
                    subtitle: None,
                    width_dips: 274.0,
                    blocks,
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                    && act >= 100
                    && let Some(exes) = launch_list.get((act - 100) as usize)
                {
                    for exe in exes {
                        pins::launch(exe.clone());
                    }
                }
            }
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_23_csharp_widgets_registered_with_valid_categories() {
        assert_eq!(ALL_WIDGETS.len(), 23);
        for w in ALL_WIDGETS {
            assert!(
                WIDGET_CATEGORIES.contains(&w.category),
                "unknown category for {}",
                w.id
            );
            assert!(widget_descriptor(w.id).is_some());
        }
        assert!(active_widgets(&DockSettings::default()).is_empty());
    }
}
