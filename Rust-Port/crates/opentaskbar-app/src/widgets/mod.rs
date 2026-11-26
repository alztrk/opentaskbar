//! Unified 23-widget registry, runtime state, and C#-parity `DockFlyout` builders.
//!
//! Categories (matching C# `DockWidgetService.cs`):
//! - `Time & Weather`    (5): `clock`, `weather`, `dayprogress`, `stopwatch`, `countdown`
//! - `System & Hardware` (5): `stats`, `netspeed`, `battery`, `bluetoothbattery`, `storage`
//! - `AI Quotas`         (7): `aiusage_claude`, `aiusage_codex`,
//!   `aiusage_opencode`, `aiusage_copilot`, `aiusage_deepseek`,
//!   `aiusage_openrouter`, `aiusage_cursor`
//! - `Media & Controls`  (4): `media`, `audio`, `quicksettings`, `keyboard`
//! - `Productivity`      (1): `focus`

pub mod diagnostics;
pub mod telemetry;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::{AppTheme, AutoHideMode, DockSettings};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
};

use crate::design::{self, Rgb};
use crate::dock::dcomp::{ChipIcon, WidgetChipSpec};
use crate::platform::{pins, wireless_battery};
use crate::ui::flyout::{self, BarTone, FlyoutBlock, FlyoutSpec, LiveEvent, LiveOutcome};
use crate::ui::menu::BorderStyle;
use telemetry::{BluetoothBatteryDevice, BluetoothDeviceKind};

pub use telemetry::{
    active_keyboard_code, adjust_audio_volume, day_progress_metrics, empty_recycle_bin,
    get_audio_state, is_recycle_bin_non_empty, sample_battery, sample_drives, set_audio_volume,
    toggle_audio_mute,
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

/// All 23 C# catalog widgets in exact C# category order with exact C# `FullWidthEstimate` widths.
pub const ALL_WIDGETS: &[WidgetDescriptor] = &[
    // Time & Weather
    wd("clock", "Clock & Date", "Time & Weather", 148.0),
    wd("weather", "Weather", "Time & Weather", 240.0),
    wd("dayprogress", "Day Progress", "Time & Weather", 96.0),
    wd("stopwatch", "Stopwatch", "Time & Weather", 124.0),
    wd("countdown", "Countdown", "Time & Weather", 124.0),
    // System & Hardware
    wd("stats", "System Stats", "System & Hardware", 96.0),
    wd("netspeed", "Net Speed", "System & Hardware", 140.0),
    wd("battery", "Battery", "System & Hardware", 96.0),
    wd(
        "bluetoothbattery",
        "Bluetooth Battery",
        "System & Hardware",
        78.0,
    ),
    wd("storage", "Storage", "System & Hardware", 136.0),
    // AI Quotas (unified summary + exact C# `AiUsageWidget.CardWidth` widths)
    wd("aiusage_all", "AI Quotas (All)", "AI Quotas", 156.0),
    wd("aiusage_claude", "Claude", "AI Quotas", 184.0),
    wd("aiusage_codex", "Codex", "AI Quotas", 184.0),
    wd("aiusage_opencode", "OpenCode", "AI Quotas", 296.0),
    wd("aiusage_copilot", "Copilot", "AI Quotas", 192.0),
    wd("aiusage_deepseek", "DeepSeek", "AI Quotas", 139.0),
    wd("aiusage_openrouter", "OpenRouter", "AI Quotas", 139.0),
    wd("aiusage_cursor", "Cursor", "AI Quotas", 164.0),
    // Media & Controls
    wd("media", "Media", "Media & Controls", 256.0),
    wd("audio", "Audio", "Media & Controls", 78.0),
    wd("quicksettings", "Quick Settings", "Media & Controls", 106.0),
    wd("keyboard", "Keyboard Layout", "Media & Controls", 58.0),
    // Productivity
    wd("focus", "Focus", "Productivity", 124.0),
];

pub fn ai_provider_color(id: &str) -> Rgb {
    match id {
        "aiusage_claude" => (0xd9, 0x77, 0x57),
        "aiusage_codex" => (0x8d, 0x99, 0xff),
        "aiusage_opencode" => (0x58, 0xd8, 0xb2),
        "aiusage_copilot" => (0x9f, 0x9f, 0xf2),
        "aiusage_deepseek" => (0x4d, 0x6b, 0xfe),
        "aiusage_openrouter" => (0xb7, 0x9b, 0xf8),
        "aiusage_cursor" => (0xf3, 0xf4, 0xf6),
        _ => (0xaa, 0xb2, 0xc0),
    }
}

/// Result of a direct left-click on a dock widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectClickOutcome {
    Handled,
    SettingsChanged,
    OpenFlyout,
}

const STACKS_WIDGET: WidgetDescriptor = wd("stacks", "Folder Stacks", "System & Hardware", 98.0);
const GROUPS_WIDGET: WidgetDescriptor = wd("groups", "App Groups", "System & Hardware", 98.0);

/// Localized widget name for the settings catalog and widget menus.
pub fn widget_name(id: &str) -> String {
    match id {
        "clock" => loc::t("WidgetClock"),
        "weather" => loc::t("WidgetWeather"),
        "dayprogress" => loc::t("WidgetDayProgress"),
        "stopwatch" => loc::t("WidgetStopwatch"),
        "countdown" => loc::t("WidgetCountdown"),
        "stats" => loc::t("WidgetSystemStats"),
        "netspeed" => loc::t("WidgetNetSpeed"),
        "battery" => loc::t("WidgetBattery"),
        "bluetoothbattery" => loc::t("WidgetBluetoothBattery"),
        "storage" => loc::t("WidgetStorage"),
        "aiusage_all" => loc::t("WidgetAiUsage"),
        "aiusage_claude" => loc::t("WidgetAiUsageClaude"),
        "aiusage_codex" => loc::t("WidgetAiUsageCodex"),
        "aiusage_opencode" => loc::t("WidgetAiUsageOpenCode"),
        "aiusage_copilot" => loc::t("WidgetAiUsageCopilot"),
        "aiusage_deepseek" => loc::t("WidgetAiUsageDeepSeek"),
        "aiusage_openrouter" => loc::t("WidgetAiUsageOpenRouter"),
        "aiusage_cursor" => loc::t("WidgetAiUsageCursor"),
        "media" => loc::t("WidgetMedia"),
        "audio" => loc::t("WidgetAudio"),
        "quicksettings" => loc::t("WidgetQuickSettings"),
        "keyboard" => loc::t("WidgetKeyboardLayout"),
        "focus" => loc::t("WidgetFocus"),
        _ => widget_descriptor(id).map_or_else(|| id.to_owned(), |d| d.name.to_owned()),
    }
}

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

struct TooltipTextCache {
    id: &'static str,
    sub: Option<u8>,
    text: String,
    checked: Instant,
}

pub struct WidgetRuntime {
    stats: telemetry::SystemStatsSampler,
    net: telemetry::NetSpeedSampler,
    cpu_pct: u32,
    ram_pct: u32,
    stats_uptime: String,
    stats_top: Arc<Mutex<Vec<(String, u64)>>>,
    last_stats_top: Option<Instant>,
    audio_vol: u32,
    audio_muted: bool,
    stopwatch_running: bool,
    stopwatch_secs: u64,
    stopwatch_laps: Vec<u64>,
    countdown_running: bool,
    countdown_total_secs: u64,
    countdown_remaining_secs: u64,
    focus_running: bool,
    focus_preset_mins: u64,
    focus_remaining_secs: u64,
    focus_completed: u32,
    weather: Arc<Mutex<telemetry::WeatherCache>>,
    bt_battery: Arc<Mutex<telemetry::BluetoothBatteryCache>>,
    ai_usage: Arc<Mutex<telemetry::AiUsageCache>>,
    media_session: Arc<Mutex<telemetry::MediaSessionSnapshot>>,
    night_light_on: bool,
    battery_anim_start: Instant,
    battery_charging: bool,
    keyboard_override: Option<(String, Instant)>,
    last_tick: Instant,
    /// Sensor polling is capped at 1 Hz; hover/click refreshes only rebuild chip specs.
    last_poll: Option<Instant>,
    last_night_light_check: Option<Instant>,
    tooltip_text: Option<TooltipTextCache>,
}

impl Default for WidgetRuntime {
    fn default() -> Self {
        Self {
            stats: telemetry::SystemStatsSampler::default(),
            net: telemetry::NetSpeedSampler::default(),
            cpu_pct: 0,
            ram_pct: 0,
            stats_uptime: telemetry::system_uptime_string(),
            stats_top: Arc::new(Mutex::new(Vec::new())),
            last_stats_top: None,
            audio_vol: 50,
            audio_muted: false,
            stopwatch_running: false,
            stopwatch_secs: 0,
            stopwatch_laps: Vec::new(),
            countdown_running: false,
            countdown_total_secs: 15 * 60,
            countdown_remaining_secs: 15 * 60,
            focus_running: false,
            focus_preset_mins: 25,
            focus_remaining_secs: 25 * 60,
            focus_completed: 0,
            weather: Arc::new(Mutex::new(telemetry::WeatherCache::default())),
            bt_battery: Arc::new(Mutex::new(telemetry::BluetoothBatteryCache::default())),
            ai_usage: Arc::new(Mutex::new(telemetry::AiUsageCache::default())),
            media_session: Arc::new(Mutex::new(telemetry::MediaSessionSnapshot::default())),
            night_light_on: telemetry::read_night_light().unwrap_or(false),
            battery_anim_start: Instant::now(),
            battery_charging: false,
            keyboard_override: None,
            last_tick: Instant::now(),
            last_poll: None,
            last_night_light_check: None,
            tooltip_text: None,
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

fn format_mm_ss(total_secs: u64) -> String {
    if total_secs >= 3600 {
        format!(
            "{:02}:{:02}:{:02}",
            total_secs / 3600,
            (total_secs % 3600) / 60,
            total_secs % 60
        )
    } else {
        format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
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
                    self.focus_completed = self.focus_completed.saturating_add(1);
                }
            }
        }
        // `refresh_widgets` also runs on hover/click; those calls must not re-sample COM,
        // registry, network and disk state. The 2 s widget timer still drives polling.
        if self
            .last_poll
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
        {
            return;
        }
        self.last_poll = Some(now);

        if settings.is_widget_enabled("stats") {
            (self.cpu_pct, self.ram_pct) = self.stats.sample();
            self.stats_uptime = telemetry::system_uptime_string();
            if self
                .last_stats_top
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(5))
            {
                self.last_stats_top = Some(now);
                let shared = Arc::clone(&self.stats_top);
                std::thread::Builder::new()
                    .name("opentaskbar-stats".to_owned())
                    .spawn(move || {
                        let top = telemetry::sample_top_processes(5);
                        if let Ok(mut guard) = shared.lock() {
                            *guard = top;
                        }
                    })
                    .ok();
            }
        }
        if settings.is_widget_enabled("audio")
            || settings.is_widget_enabled("media")
            || settings.is_widget_enabled("quicksettings")
        {
            (self.audio_vol, self.audio_muted) = get_audio_state();
        }
        if settings.is_widget_enabled("media") {
            telemetry::refresh_media_session(&self.media_session, false);
        }
        if settings.is_widget_enabled("netspeed") {
            self.net.sample();
        }
        if settings.is_widget_enabled("weather") {
            telemetry::refresh_weather(&self.weather, &settings.weather_city, false);
        }
        if settings.is_widget_enabled("bluetoothbattery") {
            let readings = wireless_battery::read_all();
            if readings.is_empty() {
                telemetry::refresh_bluetooth_battery(&self.bt_battery, false);
            } else if let Ok(mut cache) = self.bt_battery.lock() {
                cache.devices = readings
                    .into_iter()
                    .map(|reading| BluetoothBatteryDevice {
                        kind: match reading.source {
                            wireless_battery::BatterySource::GamepadHid
                            | wireless_battery::BatterySource::XInput => {
                                BluetoothDeviceKind::Gamepad
                            }
                            wireless_battery::BatterySource::LogitechHidPlusPlus
                            | wireless_battery::BatterySource::Keyboard24gHid => {
                                BluetoothDeviceKind::Keyboard
                            }
                            wireless_battery::BatterySource::RazerHid
                            | wireless_battery::BatterySource::SteelSeriesHid
                            | wireless_battery::BatterySource::CompxAtkVxe => {
                                BluetoothDeviceKind::Mouse
                            }
                            _ => BluetoothDeviceKind::Headphones,
                        },
                        name: reading.name,
                        percent: (reading.level_01 * 100.0).round().clamp(0.0, 100.0) as u32,
                    })
                    .collect();
                cache.last_fetch = Some(now);
                cache.loading = false;
            }
        }
        if settings.is_widget_enabled("quicksettings")
            && self
                .last_night_light_check
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(10))
        {
            self.last_night_light_check = Some(now);
            self.night_light_on = telemetry::read_night_light().unwrap_or(self.night_light_on);
        }
        if settings.is_widget_enabled("battery") {
            let b = sample_battery();
            self.battery_charging = b.has_battery && b.charging;
        } else {
            self.battery_charging = false;
        }
        let all_ai = settings.is_widget_enabled("aiusage_all");
        for ai in telemetry::AI_PROVIDERS {
            if all_ai || settings.is_widget_enabled(ai.id) {
                telemetry::refresh_ai_provider(&self.ai_usage, ai.id, false);
            }
        }
    }

    /// Returns `true` when any dock widget needs a 15 FPS animation tick (e.g. C# Battery charging wave).
    pub fn needs_animation_tick(&self, settings: &DockSettings) -> bool {
        settings.animations_enabled
            && settings.is_widget_enabled("battery")
            && self.battery_charging
    }

    pub fn cycle_keyboard_layout(&mut self, forward: bool) {
        if let Some(code) = telemetry::cycle_keyboard_layout(forward) {
            self.keyboard_override = Some((code, Instant::now()));
        }
    }

    fn current_keyboard_code(&self) -> String {
        if let Some((ref code, at)) = self.keyboard_override
            && at.elapsed().as_millis() < 450
        {
            return code.clone();
        }
        active_keyboard_code()
    }

    /// Returns the live dynamic width for a dock widget (matching C# `StorageWidget` multi-drive
    /// and `BluetoothBatteryWidget` multi-device width expansion).
    pub fn widget_width_dips(&self, id: &str) -> f32 {
        match id {
            "storage" => {
                let n = sample_drives().len().clamp(1, 3);
                (n as f32 * 116.0).max(124.0)
            }
            "bluetoothbattery" => {
                let n = self
                    .bt_battery
                    .lock()
                    .ok()
                    .map_or(0, |g| g.devices.len().min(3));
                if n == 0 {
                    78.0
                } else {
                    (n as f32 * 64.0 + 12.0).max(78.0)
                }
            }
            _ => widget_descriptor(id).map_or(96.0, |d| d.width_dips),
        }
    }

    /// Builds the exact C# 1-to-1 `WidgetChipSpec` rendered onto the dock pill.
    pub fn chip_spec(&self, id: &str, settings: &DockSettings, is_dark: bool) -> WidgetChipSpec {
        let pal = design::palette(is_dark);
        if let Some(ai) = telemetry::ai_provider_meta(id) {
            let snap = self
                .ai_usage
                .lock()
                .ok()
                .and_then(|g| g.providers.get(ai.id).cloned())
                .unwrap_or_default();
            let bar_accent = |used: f32| -> Rgb {
                if used >= 0.90 {
                    pal.danger
                } else if used >= 0.75 {
                    pal.warning
                } else {
                    pal.widget_accent
                }
            };
            return match ai.id {
                "aiusage_claude" | "aiusage_codex" => {
                    let s_win = snap.windows.iter().find(|w| w.label == "5h");
                    let w_win = snap.windows.iter().find(|w| w.label == "7d");
                    let rows = vec![
                        (
                            "5h".to_owned(),
                            s_win.map_or(0.0, |w| w.used),
                            bar_accent(s_win.map_or(0.0, |w| w.used)),
                            s_win.map_or_else(|| "—".to_owned(), |w| w.reset_text.clone()),
                        ),
                        (
                            "7d".to_owned(),
                            w_win.map_or(0.0, |w| w.used),
                            bar_accent(w_win.map_or(0.0, |w| w.used)),
                            w_win.map_or_else(|| "—".to_owned(), |w| w.reset_text.clone()),
                        ),
                    ];
                    WidgetChipSpec::AiDual {
                        provider_id: ai.id,
                        brand_color: (ai.id == "aiusage_claude").then_some((0xd9, 0x77, 0x57)),
                        rows,
                    }
                }
                "aiusage_cursor" => {
                    // Exact C# AiUsageWidget.cs: Cursor shows a single long window row (`30d` / `7d`).
                    let win = snap.windows.first();
                    let used = win.map_or(0.0, |w| w.used);
                    WidgetChipSpec::AiDual {
                        provider_id: ai.id,
                        brand_color: None,
                        rows: vec![(
                            win.map_or_else(|| "30d".to_owned(), |w| w.label.clone()),
                            used,
                            bar_accent(used),
                            win.map_or_else(|| "—".to_owned(), |w| w.reset_text.clone()),
                        )],
                    }
                }
                "aiusage_opencode" => {
                    let cols = ["1h", "7d", "30d"]
                        .into_iter()
                        .map(|lbl| {
                            if let Some(w) = snap.windows.iter().find(|w| w.label == lbl) {
                                let col = if w.used >= 0.75 {
                                    bar_accent(w.used)
                                } else {
                                    pal.success
                                };
                                (
                                    lbl.to_owned(),
                                    format!("{:.0}%", w.used * 100.0),
                                    w.used,
                                    col,
                                    w.reset_text.clone(),
                                )
                            } else {
                                (
                                    lbl.to_owned(),
                                    "—".to_owned(),
                                    0.0,
                                    pal.success,
                                    "—".to_owned(),
                                )
                            }
                        })
                        .collect();
                    WidgetChipSpec::AiOpenCode { columns: cols }
                }
                "aiusage_copilot" => {
                    if let Some(w) = snap.windows.first() {
                        let col = if w.used >= 0.75 {
                            bar_accent(w.used)
                        } else {
                            (0x9f, 0x9f, 0xf2) // C# CopilotBrandBrush
                        };
                        WidgetChipSpec::AiCopilot {
                            value_text: format!("{:.0}%", w.used * 100.0),
                            meta_text: w.reset_text.clone(),
                            used: w.used,
                            bar_color: col,
                        }
                    } else {
                        WidgetChipSpec::AiCopilot {
                            value_text: "—".to_owned(),
                            meta_text: if snap.has_cred {
                                loc::t("AiUsageMonthly")
                            } else {
                                loc::t("AiUsageCursorSignIn")
                            },
                            used: 0.0,
                            bar_color: (0x9f, 0x9f, 0xf2),
                        }
                    }
                }
                "aiusage_deepseek" | "aiusage_openrouter" => WidgetChipSpec::AiBalance {
                    provider_id: ai.id,
                    name: ai.short,
                    brand_color: ai_provider_color(ai.id),
                    value_text: snap.balance_text.unwrap_or_else(|| "—".to_owned()),
                },
                _ => WidgetChipSpec::AiDual {
                    provider_id: ai.id,
                    brand_color: None,
                    rows: vec![("5h".to_owned(), 0.0, pal.widget_accent, "—".to_owned())],
                },
            };
        }

        match id {
            "clock" => {
                let info = telemetry::local_clock_info();
                let (time, date) = telemetry::format_clock(&info, settings);
                WidgetChipSpec::Clock {
                    time,
                    date,
                    weekday: info.weekday_short,
                }
            }
            "weather" => {
                let snap = self
                    .weather
                    .lock()
                    .ok()
                    .map(|g| g.clone())
                    .unwrap_or_default();
                let available = !snap.temp_c.is_empty();
                let place = if snap.city.is_empty() {
                    settings.weather_city.clone()
                } else {
                    snap.city
                };
                let condition = if available {
                    snap.condition
                } else if snap.loading {
                    "Loading weather…".to_owned()
                } else {
                    loc::t("WeatherUnavailable").to_owned()
                };
                // Exact C# WeatherWidget.cs WarmSunBrush (#E5AE5E) & RainBrush (#6EAAF3)
                let icon_color = if snap.is_rain {
                    (0x6e, 0xaa, 0xf3)
                } else {
                    (0xe5, 0xae, 0x5e)
                };
                WidgetChipSpec::Weather {
                    available,
                    icon_color,
                    is_rain: snap.is_rain,
                    temp: snap.temp_c,
                    place,
                    condition,
                    today_range: (!snap.today_range.is_empty()).then_some(snap.today_range),
                }
            }
            "dayprogress" => {
                let (day, _, _, _) = day_progress_metrics();
                WidgetChipSpec::DayProgress {
                    ratio: day,
                    percent: format!("{:.0}%", day * 100.0),
                    unit: "day".to_owned(),
                }
            }
            "stopwatch" => WidgetChipSpec::Timed {
                is_focus: false,
                running: self.stopwatch_running,
                text: format_mm_ss(self.stopwatch_secs),
            },
            "countdown" => WidgetChipSpec::Timed {
                is_focus: false,
                running: self.countdown_running,
                text: format_mm_ss(self.countdown_remaining_secs),
            },
            "focus" => WidgetChipSpec::Timed {
                is_focus: true,
                running: self.focus_running,
                text: format_mm_ss(self.focus_remaining_secs),
            },
            "stats" => {
                let load_fg = |pct: u32| -> Rgb {
                    if pct >= 90 {
                        pal.danger
                    } else if pct >= 75 {
                        pal.warning
                    } else {
                        pal.text_primary
                    }
                };
                let show_cpu = settings.stats_meters & 1 != 0;
                let show_ram = settings.stats_meters & 2 != 0;
                WidgetChipSpec::SystemStats {
                    cpu_text: if show_cpu {
                        format!("{}%", self.cpu_pct)
                    } else {
                        String::new()
                    },
                    cpu_color: load_fg(self.cpu_pct),
                    ram_text: if show_ram {
                        format!("{}%", self.ram_pct)
                    } else {
                        String::new()
                    },
                    ram_color: load_fg(self.ram_pct),
                }
            }
            "netspeed" => WidgetChipSpec::NetSpeed {
                down: telemetry::format_speed(self.net.rx_bps),
                up: telemetry::format_speed(self.net.tx_bps),
            },
            "battery" => {
                let b = sample_battery();
                let wave_phase = if b.has_battery && b.charging && settings.animations_enabled {
                    (((self.battery_anim_start.elapsed().as_millis() % 1600) / 67) as f32) / 24.0
                } else {
                    0.0
                };
                WidgetChipSpec::BatteryLiquid {
                    present: b.has_battery,
                    percent: b.percent,
                    charging: b.charging,
                    wave_phase,
                }
            }
            "bluetoothbattery" => {
                let devices: Vec<(telemetry::BluetoothDeviceKind, u32, bool, bool)> = self
                    .bt_battery
                    .lock()
                    .ok()
                    .map(|g| {
                        g.devices
                            .iter()
                            .take(3)
                            .map(|d| (d.kind, d.percent, d.percent <= 20, d.percent >= 100))
                            .collect()
                    })
                    .unwrap_or_default();
                WidgetChipSpec::Bluetooth { devices }
            }
            "storage" => {
                let drives: Vec<(String, String)> = sample_drives()
                    .into_iter()
                    .take(3)
                    .map(|d| (format!("{}:", d.letter), d.format_capacity_pair()))
                    .collect();
                WidgetChipSpec::Storage { drives }
            }
            "media" => {
                let snap = self
                    .media_session
                    .lock()
                    .ok()
                    .map(|g| g.clone())
                    .unwrap_or_default();
                let title = if snap.title.is_empty() {
                    loc::t("WidgetMediaDescription").to_owned()
                } else {
                    snap.title
                };
                WidgetChipSpec::Media {
                    title,
                    artist: snap.artist,
                    muted: self.audio_muted,
                }
            }
            "audio" => WidgetChipSpec::Audio {
                volume: self.audio_vol,
                muted: self.audio_muted,
            },
            "quicksettings" => WidgetChipSpec::QuickSettings {
                // Exact C# QuickSettingsWidget.xaml: Wi-Fi, Bluetooth, Night Light!
                pills: [
                    ("\u{E701}".to_owned(), false),
                    ("\u{E702}".to_owned(), false),
                    ("\u{E708}".to_owned(), self.night_light_on),
                ],
            },
            "keyboard" => WidgetChipSpec::Keyboard {
                code: self.current_keyboard_code(),
            },
            "aiusage_all" => {
                let mut active_count = 0usize;
                let mut peak_label: Option<(f32, String)> = None;
                let mut first_bal: Option<String> = None;
                if let Ok(guard) = self.ai_usage.lock() {
                    for ai in telemetry::AI_PROVIDERS {
                        if let Some(snap) = guard.providers.get(ai.id) {
                            if snap.has_cred
                                || snap.balance_text.is_some()
                                || !snap.windows.is_empty()
                            {
                                active_count += 1;
                            }
                            if let Some(w) = snap.windows.first() {
                                let replace =
                                    peak_label.as_ref().is_none_or(|(prev, _)| w.used > *prev);
                                if replace {
                                    peak_label = Some((
                                        w.used,
                                        format!("{} {:.0}%", ai.name, w.used * 100.0),
                                    ));
                                }
                            } else if first_bal.is_none()
                                && let Some(bal) = &snap.balance_text
                            {
                                first_bal = Some(format!("{} {bal}", ai.name));
                            }
                        }
                    }
                }
                let bottom = peak_label.map(|(_, s)| s).or(first_bal).unwrap_or_else(|| {
                    format!("{active_count}/{} ready", telemetry::AI_PROVIDERS.len())
                });
                WidgetChipSpec::Simple {
                    icon: ChipIcon::Sparkle,
                    icon_color: pal.widget_accent,
                    top: format!("AI · {active_count}"),
                    bottom,
                }
            }
            "stacks" => WidgetChipSpec::Simple {
                icon: ChipIcon::Folder,
                icon_color: pal.warning,
                top: "Stacks".to_owned(),
                bottom: format!("{} folders", settings.stack_folders.len()),
            },
            "groups" => WidgetChipSpec::Simple {
                icon: ChipIcon::Grid,
                icon_color: pal.accent,
                top: "Groups".to_owned(),
                bottom: format!("{} groups", settings.app_groups.len()),
            },
            _ => WidgetChipSpec::Keyboard {
                code: id.chars().take(3).collect::<String>().to_uppercase(),
            },
        }
    }

    /// Concise C#-style tooltip above each hovered widget.
    /// Per-frame accessor. The expensive path (COM device name, disk queries, file checks)
    /// runs at most twice per second for the same sub-button instead of on every VSync.
    pub fn hover_tooltip(
        &mut self,
        id: &'static str,
        sub: Option<u8>,
        settings: &DockSettings,
    ) -> &str {
        let now = Instant::now();
        let cached = self.tooltip_text.as_ref().is_some_and(|c| {
            c.id == id && c.sub == sub && now.duration_since(c.checked) < Duration::from_millis(500)
        });
        if !cached {
            let text = self.build_hover_tooltip(id, sub, settings);
            self.tooltip_text = Some(TooltipTextCache {
                id,
                sub,
                text,
                checked: now,
            });
        }
        &self.tooltip_text.as_ref().unwrap().text
    }

    fn build_hover_tooltip(&self, id: &str, sub: Option<u8>, settings: &DockSettings) -> String {
        if let Some(ai) = telemetry::ai_provider_meta(id) {
            let status = if telemetry::ai_provider_detected(ai.id) {
                loc::t("AiUsageAccountConnected")
            } else {
                loc::t("AiUsageCursorSignIn")
            };
            return format!("{} · {status}", ai.name);
        }
        match id {
            "clock" => telemetry::local_clock_info().full_date,
            "media" => match sub {
                Some(0) => loc::t("MediaPrevious").to_owned(),
                Some(1) => "Play / Pause".to_owned(),
                Some(2) => loc::t("MediaNext").to_owned(),
                Some(3) => if self.audio_muted { "Unmute" } else { "Mute" }.to_owned(),
                _ => "Media".to_owned(),
            },
            "quicksettings" => match sub {
                Some(0) => "Wi-Fi".to_owned(),
                Some(1) => "Bluetooth".to_owned(),
                Some(2) => if self.night_light_on {
                    "Night Light · On"
                } else {
                    "Night Light · Off"
                }
                .to_owned(),
                _ => loc::t("WidgetQuickSettings").to_owned(),
            },
            "audio" => {
                let dev = telemetry::default_audio_device_name(false)
                    .unwrap_or_else(|| loc::t("AudioOutput").to_owned());
                if self.audio_muted {
                    format!("{dev} · Muted")
                } else {
                    format!("{dev} · {}%", self.audio_vol)
                }
            }
            "weather" => {
                let snap = self
                    .weather
                    .lock()
                    .ok()
                    .map(|g| g.clone())
                    .unwrap_or_default();
                if !snap.temp_c.is_empty() {
                    format!("{} · {} · {}", snap.city, snap.temp_c, snap.condition)
                } else {
                    format!("Weather · {}", settings.weather_city)
                }
            }
            "stats" => {
                let history = self.stats.cpu_history.as_vec();
                let avg = if history.is_empty() {
                    0.0
                } else {
                    history.iter().sum::<f32>() / history.len() as f32
                };
                format!(
                    "CPU {}% (avg {:.0}%) · RAM {}% · {} {}",
                    self.cpu_pct,
                    avg,
                    self.ram_pct,
                    loc::t("StatsUptime"),
                    self.stats_uptime
                )
            }
            "dayprogress" => {
                let (day, _, _, left) = day_progress_metrics();
                format!("Day Progress: {:.0}% ({left}h left)", day * 100.0)
            }
            "stopwatch" => {
                if sub == Some(1) {
                    if self.stopwatch_running {
                        loc::t("TimedWidgetPause")
                    } else {
                        loc::t("TimedWidgetStart")
                    }
                    .to_owned()
                } else {
                    format!(
                        "{} · {}",
                        loc::t("WidgetStopwatch"),
                        format_mm_ss(self.stopwatch_secs)
                    )
                }
            }
            "countdown" => {
                if sub == Some(1) {
                    if self.countdown_running {
                        loc::t("TimedWidgetPause")
                    } else {
                        loc::t("TimedWidgetStart")
                    }
                    .to_owned()
                } else {
                    format!(
                        "{} · {}",
                        loc::t("WidgetCountdown"),
                        format_mm_ss(self.countdown_remaining_secs)
                    )
                }
            }
            "focus" => {
                if sub == Some(1) {
                    if self.focus_running {
                        loc::t("TimedWidgetPause")
                    } else {
                        loc::t("TimedWidgetStart")
                    }
                    .to_owned()
                } else {
                    format!(
                        "{} · {}",
                        loc::t("WidgetFocus"),
                        format_mm_ss(self.focus_remaining_secs)
                    )
                }
            }
            "keyboard" => format!("Keyboard Layout · {}", self.current_keyboard_code()),
            "storage" => sample_drives()
                .into_iter()
                .take(3)
                .map(|d| format!("{}: {}", d.letter, d.format_capacity_pair()))
                .collect::<Vec<_>>()
                .join("  |  "),
            "netspeed" => format!(
                "↓ {} · ↑ {}",
                telemetry::format_speed(self.net.rx_bps),
                telemetry::format_speed(self.net.tx_bps)
            ),
            "battery" => {
                let b = sample_battery();
                if !b.has_battery {
                    "Battery: Unavailable".to_owned()
                } else if b.charging {
                    format!("Charging: {}%", b.percent)
                } else if b.on_ac {
                    format!("Plugged in: {}%", b.percent)
                } else if let Some(secs) = b.life_secs.filter(|&s| s >= 60) {
                    let h = secs / 3600;
                    let m = (secs % 3600) / 60;
                    if h > 0 {
                        format!("Battery: {}% · {h}h {m}m remaining", b.percent)
                    } else {
                        format!("Battery: {}% · {m}m remaining", b.percent)
                    }
                } else {
                    format!("Battery: {}%", b.percent)
                }
            }
            "bluetoothbattery" => "Bluetooth Battery".to_owned(),
            _ => widget_descriptor(id).map_or(id, |d| d.name).to_owned(),
        }
    }

    /// Exact C# left-click dispatch: inline sub-buttons execute directly; AI, System Stats,
    /// and Net Speed chips have no flyout.
    pub fn handle_direct_click(
        &mut self,
        id: &str,
        rel_x_dips: f32,
        width_dips: f32,
        _settings: &mut DockSettings,
    ) -> DirectClickOutcome {
        if telemetry::ai_provider_meta(id).is_some() {
            return DirectClickOutcome::Handled;
        }
        match id {
            "media" => {
                let controls_l = width_dips - 116.0;
                if rel_x_dips < controls_l {
                    DirectClickOutcome::OpenFlyout
                } else if rel_x_dips < controls_l + 26.0 {
                    telemetry::send_media_vk(VK_MEDIA_PREV_TRACK);
                    DirectClickOutcome::Handled
                } else if rel_x_dips < controls_l + 55.0 {
                    telemetry::send_media_vk(VK_MEDIA_PLAY_PAUSE);
                    DirectClickOutcome::Handled
                } else if rel_x_dips < controls_l + 82.0 {
                    telemetry::send_media_vk(VK_MEDIA_NEXT_TRACK);
                    DirectClickOutcome::Handled
                } else {
                    toggle_audio_mute();
                    (self.audio_vol, self.audio_muted) = get_audio_state();
                    DirectClickOutcome::Handled
                }
            }
            "quicksettings" => {
                // Exact C# QuickSettingsWidget.xaml: Wi-Fi, Bluetooth, Night Light!
                let third = (width_dips / 3.0).max(1.0);
                if rel_x_dips < third {
                    pins::launch("ms-settings:network-wifi".to_owned());
                    DirectClickOutcome::Handled
                } else if rel_x_dips < third * 2.0 {
                    pins::launch("ms-settings:bluetooth".to_owned());
                    DirectClickOutcome::Handled
                } else {
                    if telemetry::toggle_night_light() {
                        self.night_light_on = !self.night_light_on;
                    } else {
                        pins::launch("ms-settings:nightlight".to_owned());
                    }
                    DirectClickOutcome::Handled
                }
            }
            "stopwatch" => {
                if rel_x_dips >= width_dips - 36.0 {
                    self.stopwatch_running = !self.stopwatch_running;
                    DirectClickOutcome::Handled
                } else {
                    DirectClickOutcome::OpenFlyout
                }
            }
            "countdown" => {
                if rel_x_dips >= width_dips - 36.0 {
                    if self.countdown_remaining_secs == 0 {
                        self.countdown_remaining_secs = self.countdown_total_secs;
                    }
                    self.countdown_running = !self.countdown_running;
                    DirectClickOutcome::Handled
                } else {
                    DirectClickOutcome::OpenFlyout
                }
            }
            "focus" => {
                if rel_x_dips >= width_dips - 36.0 {
                    if self.focus_remaining_secs == 0 {
                        self.focus_remaining_secs = self.focus_preset_mins * 60;
                    }
                    self.focus_running = !self.focus_running;
                    DirectClickOutcome::Handled
                } else {
                    DirectClickOutcome::OpenFlyout
                }
            }
            "battery" => {
                // Exact C# BatteryWidget.OpenPowerSettingsAction
                pins::launch("ms-settings:batterysaver".to_owned());
                DirectClickOutcome::Handled
            }
            "clock" | "audio" | "weather" | "bluetoothbattery" | "storage" | "keyboard"
            | "dayprogress" | "stacks" | "groups" | "stats" => DirectClickOutcome::OpenFlyout,
            _ => DirectClickOutcome::Handled,
        }
    }

    /// Quick secondary (right-click) shortcuts matching C# widget context menus.
    pub fn handle_secondary_click(
        &mut self,
        id: &str,
        settings: &mut DockSettings,
    ) -> Option<DirectClickOutcome> {
        match id {
            "stopwatch" => {
                self.stopwatch_running = false;
                self.stopwatch_secs = 0;
                self.stopwatch_laps.clear();
                Some(DirectClickOutcome::Handled)
            }
            "countdown" => {
                self.countdown_running = false;
                self.countdown_remaining_secs = self.countdown_total_secs;
                Some(DirectClickOutcome::Handled)
            }
            "focus" => {
                self.focus_running = false;
                self.focus_remaining_secs = self.focus_preset_mins * 60;
                Some(DirectClickOutcome::Handled)
            }
            "audio" => {
                pins::launch("ms-settings:sound".to_owned());
                Some(DirectClickOutcome::Handled)
            }
            "quicksettings" => {
                settings.focus_mode = !settings.focus_mode;
                Some(DirectClickOutcome::SettingsChanged)
            }
            _ => None,
        }
    }

    fn build_calendar_spec(
        shown_y: i32,
        shown_m: u32,
        selected: Option<(i32, u32, u32)>,
    ) -> FlyoutSpec {
        let info = telemetry::local_clock_info();
        let today = (info.year, info.month, info.day);
        FlyoutSpec {
            title: String::new(),
            subtitle: None,
            width_dips: 312.0,
            blocks: vec![
                FlyoutBlock::Hero {
                    value: info.time_hm,
                    caption: Some(info.full_date.clone()),
                    centered: false,
                    large: false,
                    right_meta: Some(info.tz_label),
                },
                FlyoutBlock::CalendarGrid {
                    year: shown_y,
                    month: shown_m,
                    today,
                    selected,
                    prev_year_id: 1,
                    prev_month_id: 2,
                    title_id: 3,
                    next_month_id: 4,
                    next_year_id: 5,
                    day_id_base: 100,
                },
                FlyoutBlock::Separator,
                FlyoutBlock::StatRow {
                    label: format!("Today · {}", info.full_date),
                    value: String::new(),
                },
                FlyoutBlock::ButtonBar(vec![
                    (10, loc::t("GoToToday").to_owned(), false),
                    (11, "Date & Time".to_owned(), false),
                ]),
            ],
        }
    }

    fn build_audio_mixer_spec(&self) -> FlyoutSpec {
        let (out_vol, out_muted) = get_audio_state();
        let out_name =
            telemetry::default_audio_device_name(false).unwrap_or_else(|| "Speakers".to_owned());
        let mic = telemetry::get_mic_state();
        let (in_vol, in_muted) = mic.unwrap_or((0, true));
        let in_name = telemetry::default_audio_device_name(true)
            .unwrap_or_else(|| loc::t("NoMicDevice").to_owned());

        let mut blocks = vec![
            FlyoutBlock::AudioSliderSection {
                label: "Output".to_owned(),
                device_name: out_name,
                value_text: if out_muted {
                    "0%".to_owned()
                } else {
                    format!("{out_vol}%")
                },
                ratio: out_vol as f32 / 100.0,
                muted: out_muted,
                mute_id: 1,
                slider_id: 2,
            },
            FlyoutBlock::Separator,
            FlyoutBlock::AudioSliderSection {
                label: "Input".to_owned(),
                device_name: in_name,
                value_text: if mic.is_none() {
                    "—".to_owned()
                } else if in_muted {
                    "0%".to_owned()
                } else {
                    format!("{in_vol}%")
                },
                ratio: if mic.is_some() {
                    in_vol as f32 / 100.0
                } else {
                    0.0
                },
                muted: in_muted || mic.is_none(),
                mute_id: 3,
                slider_id: 4,
            },
            FlyoutBlock::Separator,
            FlyoutBlock::ButtonBar(vec![(
                5,
                loc::t_fmt("RadioSettings", &[&loc::t("LauncherSound")]).to_owned(),
                false,
            )]),
        ];
        let sessions = diagnostics::list_app_audio_sessions();
        if !sessions.is_empty() {
            blocks.push(FlyoutBlock::Separator);
            for (index, session) in sessions.iter().take(8).enumerate() {
                blocks.push(FlyoutBlock::AudioSliderSection {
                    label: session.label.clone(),
                    device_name: session.id.clone(),
                    value_text: format!("{}%", (session.volume * 100.0).round() as u32),
                    ratio: session.volume,
                    muted: session.muted,
                    mute_id: 100 + index as u32 * 2,
                    slider_id: 101 + index as u32 * 2,
                });
            }
        }
        FlyoutSpec {
            title: loc::t("WidgetAudio"),
            subtitle: None,
            width_dips: 314.0,
            blocks,
        }
    }

    fn build_focus_spec(&self) -> FlyoutSpec {
        let total = (self.focus_preset_mins * 60).max(1) as f32;
        let ratio = (self.focus_remaining_secs as f32 / total).clamp(0.0, 1.0);
        FlyoutSpec {
            title: String::new(),
            subtitle: None,
            width_dips: 316.0,
            blocks: vec![
                FlyoutBlock::StatRow {
                    label: "Focus".to_owned(),
                    value: format!("{} sessions", self.focus_completed),
                },
                FlyoutBlock::Hero {
                    value: format_mm_ss(self.focus_remaining_secs),
                    caption: None,
                    centered: false,
                    large: true,
                    right_meta: None,
                },
                FlyoutBlock::ProgressBar {
                    label: String::new(),
                    value_text: String::new(),
                    ratio,
                    tone: BarTone::Accent,
                },
                FlyoutBlock::ButtonBar(vec![
                    (15, "15m".to_owned(), self.focus_preset_mins == 15),
                    (25, "25m".to_owned(), self.focus_preset_mins == 25),
                    (50, "50m".to_owned(), self.focus_preset_mins == 50),
                ]),
                FlyoutBlock::ButtonBar(vec![
                    (2, loc::t("Reset").to_owned(), false),
                    (
                        1,
                        if self.focus_running {
                            loc::t("TimedWidgetPause")
                        } else {
                            loc::t("TimedWidgetStart")
                        }
                        .to_owned(),
                        true,
                    ),
                ]),
            ],
        }
    }

    fn build_stopwatch_spec(&self) -> FlyoutSpec {
        let mut blocks = vec![
            FlyoutBlock::Hero {
                value: format_mm_ss(self.stopwatch_secs),
                caption: None,
                centered: true,
                large: true,
                right_meta: None,
            },
            FlyoutBlock::ButtonBar(vec![(3, "Lap".to_owned(), false)]),
        ];
        if self.stopwatch_laps.is_empty() {
            blocks.push(FlyoutBlock::Note(loc::t("StopwatchNoLaps").to_owned()));
        } else {
            for (i, &lap) in self.stopwatch_laps.iter().rev().take(6).enumerate() {
                let num = self.stopwatch_laps.len() - i;
                blocks.push(FlyoutBlock::StatRow {
                    label: format!("{num:02}"),
                    value: format_mm_ss(lap),
                });
            }
        }
        blocks.push(FlyoutBlock::Separator);
        blocks.push(FlyoutBlock::ButtonBar(vec![
            (
                1,
                if self.stopwatch_running {
                    loc::t("TimedWidgetPause")
                } else {
                    loc::t("TimedWidgetStart")
                }
                .to_owned(),
                true,
            ),
            (2, loc::t("Reset").to_owned(), false),
        ]));
        FlyoutSpec {
            title: loc::t("WidgetStopwatch"),
            subtitle: None,
            width_dips: 300.0,
            blocks,
        }
    }

    fn build_countdown_spec(&self) -> FlyoutSpec {
        let ratio = if self.countdown_total_secs > 0 {
            (self.countdown_remaining_secs as f32 / self.countdown_total_secs as f32)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mins = self.countdown_total_secs / 60;
        FlyoutSpec {
            title: loc::t("WidgetCountdown"),
            subtitle: None,
            width_dips: 300.0,
            blocks: vec![
                FlyoutBlock::Hero {
                    value: format_mm_ss(self.countdown_remaining_secs),
                    caption: None,
                    centered: true,
                    large: true,
                    right_meta: None,
                },
                FlyoutBlock::ProgressBar {
                    label: String::new(),
                    value_text: String::new(),
                    ratio,
                    tone: BarTone::Accent,
                },
                FlyoutBlock::ButtonBar(vec![
                    (5, "5 min".to_owned(), mins == 5),
                    (10, "10 min".to_owned(), mins == 10),
                ]),
                FlyoutBlock::ButtonBar(vec![
                    (15, "15 min".to_owned(), mins == 15),
                    (30, "30 min".to_owned(), mins == 30),
                ]),
                FlyoutBlock::Separator,
                FlyoutBlock::ButtonBar(vec![
                    (
                        1,
                        if self.countdown_running {
                            loc::t("TimedWidgetPause")
                        } else {
                            loc::t("TimedWidgetStart")
                        }
                        .to_owned(),
                        true,
                    ),
                    (2, loc::t("Reset").to_owned(), false),
                ]),
            ],
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
        if id == "aiusage_all" || telemetry::ai_provider_meta(id).is_some() {
            return false;
        }

        match id {
            // System stats flyout: uptime, memory breakdown and top processes.
            "stats" => {
                let top = self
                    .stats_top
                    .lock()
                    .map(|guard| guard.clone())
                    .unwrap_or_default();
                let (_, used_gib, total_gib) = telemetry::sample_ram_info();
                let cores = std::thread::available_parallelism()
                    .map(|n| n.get() as u64)
                    .unwrap_or(1);
                let mut blocks = vec![
                    FlyoutBlock::Hero {
                        value: self.stats_uptime.clone(),
                        caption: Some(loc::t("StatsUptime")),
                        centered: false,
                        large: false,
                        right_meta: None,
                    },
                    FlyoutBlock::StatRow {
                        label: "CPU".to_owned(),
                        value: format!(
                            "{}% · {}",
                            self.cpu_pct,
                            loc::t_fmt("StatsLogicalCores", &[&cores.to_string()])
                        ),
                    },
                    FlyoutBlock::StatRow {
                        label: "RAM".to_owned(),
                        value: format!(
                            "{}% · {} · {}",
                            self.ram_pct,
                            loc::t_fmt(
                                "StatsMemoryUsed",
                                &[&telemetry::format_bytes(
                                    (used_gib * 1_073_741_824.0) as u64
                                )]
                            ),
                            loc::t_fmt(
                                "StatsMemoryAvailable",
                                &[&telemetry::format_bytes(
                                    ((total_gib - used_gib).max(0.0) * 1_073_741_824.0) as u64
                                )]
                            )
                        ),
                    },
                    FlyoutBlock::Separator,
                ];
                if settings.stats_meters & 16 != 0 {
                    let gpu = diagnostics::sample_gpu();
                    let value = if let Some(temp) = gpu.temperature_c {
                        format!(
                            "{}% · {}°C · {}",
                            (gpu.usage_01 * 100.0).round() as u32,
                            temp.round() as i32,
                            gpu.engine_name
                        )
                    } else {
                        format!(
                            "{}% · {}",
                            (gpu.usage_01 * 100.0).round() as u32,
                            gpu.engine_name
                        )
                    };
                    blocks.insert(
                        3,
                        FlyoutBlock::StatRow {
                            label: loc::t("Gpu"),
                            value,
                        },
                    );
                } else if settings.stats_meters & 32 != 0 {
                    let gpu = diagnostics::sample_gpu();
                    if let Some(temp) = gpu.temperature_c {
                        blocks.insert(
                            3,
                            FlyoutBlock::StatRow {
                                label: loc::t("GpuTemperature"),
                                value: format!("{}°C", temp.round() as i32),
                            },
                        );
                    }
                }
                if top.is_empty() {
                    let sampling = self
                        .last_stats_top
                        .is_none_or(|t| t.elapsed() < Duration::from_secs(10));
                    blocks.push(FlyoutBlock::Note(loc::t(if sampling {
                        "StatsSamplingProcesses"
                    } else {
                        "StatsTopProcessesEmpty"
                    })));
                } else {
                    blocks.push(FlyoutBlock::StatRow {
                        label: loc::t("StatsTopProcesses"),
                        value: String::new(),
                    });
                    for (name, bytes) in top {
                        blocks.push(FlyoutBlock::StatRow {
                            label: name,
                            value: telemetry::format_bytes(bytes),
                        });
                    }
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::ButtonBar(vec![(
                    1,
                    loc::t("TaskManager"),
                    false,
                )]));
                blocks.push(FlyoutBlock::Note(loc::t("StatsAvailabilityNote")));
                let spec = FlyoutSpec {
                    title: loc::t("WidgetSystemStats"),
                    subtitle: None,
                    width_dips: 320.0,
                    blocks,
                };
                if flyout::show_flyout_with_border(
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    spec,
                    BorderStyle::from_widget_settings(is_dark, settings),
                ) == Some(1)
                {
                    pins::launch("taskmgr.exe".to_owned());
                }
            }
            // Exact C# `ClockWidget.BuildCalendar` (live interactive month/year nav & day selection)
            "clock" => {
                let info = telemetry::local_clock_info();
                let (mut shown_y, mut shown_m) = (info.year, info.month);
                let mut selected: Option<(i32, u32, u32)> = None;
                let init = Self::build_calendar_spec(shown_y, shown_m, selected);
                flyout::show_flyout_live_with_border(
                    "clock",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    BorderStyle::from_widget_settings(is_dark, settings),
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            shown_y = (shown_y - 1).max(1970);
                            LiveOutcome::Update(Self::build_calendar_spec(
                                shown_y, shown_m, selected,
                            ))
                        }
                        LiveEvent::Click(2) => {
                            if shown_m == 1 {
                                shown_m = 12;
                                shown_y = (shown_y - 1).max(1970);
                            } else {
                                shown_m -= 1;
                            }
                            LiveOutcome::Update(Self::build_calendar_spec(
                                shown_y, shown_m, selected,
                            ))
                        }
                        LiveEvent::Click(3) | LiveEvent::Click(10) => {
                            let now = telemetry::local_clock_info();
                            shown_y = now.year;
                            shown_m = now.month;
                            selected = Some((now.year, now.month, now.day));
                            LiveOutcome::Update(Self::build_calendar_spec(
                                shown_y, shown_m, selected,
                            ))
                        }
                        LiveEvent::Click(4) => {
                            if shown_m == 12 {
                                shown_m = 1;
                                shown_y = (shown_y + 1).min(2100);
                            } else {
                                shown_m += 1;
                            }
                            LiveOutcome::Update(Self::build_calendar_spec(
                                shown_y, shown_m, selected,
                            ))
                        }
                        LiveEvent::Click(5) => {
                            shown_y = (shown_y + 1).min(2100);
                            LiveOutcome::Update(Self::build_calendar_spec(
                                shown_y, shown_m, selected,
                            ))
                        }
                        LiveEvent::Click(11) => {
                            pins::launch("ms-settings:dateandtime".to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::Click(cell_id) if (100..142).contains(&cell_id) => {
                            let cell = (cell_id - 100) as i32;
                            let first_wd = telemetry::weekday_of(shown_y, shown_m, 1);
                            let mon_off = ((first_wd + 6) % 7) as i32;
                            let prev_m = if shown_m == 1 { 12 } else { shown_m - 1 };
                            let prev_y = if shown_m == 1 { shown_y - 1 } else { shown_y };
                            let prev_days = telemetry::days_in_month(prev_y, prev_m) as i32;
                            let cur_days = telemetry::days_in_month(shown_y, shown_m) as i32;
                            let next_m = if shown_m == 12 { 1 } else { shown_m + 1 };
                            let next_y = if shown_m == 12 { shown_y + 1 } else { shown_y };
                            let d_idx = cell - mon_off + 1;
                            let picked = if d_idx < 1 {
                                (prev_y, prev_m, (prev_days + d_idx) as u32)
                            } else if d_idx > cur_days {
                                (next_y, next_m, (d_idx - cur_days) as u32)
                            } else {
                                (shown_y, shown_m, d_idx as u32)
                            };
                            selected = Some(picked);
                            shown_y = picked.0;
                            shown_m = picked.1;
                            LiveOutcome::Update(Self::build_calendar_spec(
                                shown_y, shown_m, selected,
                            ))
                        }
                        _ => LiveOutcome::Close,
                    },
                );
            }
            // Exact C# `AudioWidget.PopulateFlyout` (live interactive Output + Input mixer!)
            "audio" => {
                let init = self.build_audio_mixer_spec();
                flyout::show_flyout_live_with_border(
                    "audio",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    BorderStyle::from_widget_settings(is_dark, settings),
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            toggle_audio_mute();
                            (self.audio_vol, self.audio_muted) = get_audio_state();
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        LiveEvent::Slider { id: 2, ratio } => {
                            set_audio_volume((ratio * 100.0).round() as u32);
                            (self.audio_vol, self.audio_muted) = get_audio_state();
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        LiveEvent::Click(3) => {
                            telemetry::toggle_mic_mute();
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        LiveEvent::Slider { id: 4, ratio } => {
                            telemetry::set_mic_volume((ratio * 100.0).round() as u32);
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        LiveEvent::Click(5) => {
                            pins::launch("ms-settings:sound".to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::Click(id) if id >= 100 && id % 2 == 0 => {
                            let sessions = diagnostics::list_app_audio_sessions();
                            if let Some(session) = sessions.get(((id - 100) / 2) as usize) {
                                let _ = diagnostics::set_app_mute(&session.id, !session.muted);
                            }
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        LiveEvent::Slider { id, ratio } if id >= 101 => {
                            let sessions = diagnostics::list_app_audio_sessions();
                            if let Some(session) = sessions.get(((id - 101) / 2) as usize) {
                                let _ = diagnostics::set_app_volume(&session.id, ratio);
                            }
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        _ => LiveOutcome::Close,
                    },
                );
                (self.audio_vol, self.audio_muted) = get_audio_state();
            }
            // Exact C# `WeatherWidget.BuildFlyoutContent`
            "weather" => {
                let snap = self.weather.lock().map(|g| g.clone()).unwrap_or_default();
                let city = if snap.city.is_empty() {
                    settings.weather_city.clone()
                } else {
                    snap.city
                };
                let temp = if snap.temp_c.is_empty() {
                    "—".to_owned()
                } else {
                    snap.temp_c
                };
                let feels = if snap.feels_like.is_empty() {
                    "—".to_owned()
                } else {
                    snap.feels_like
                };
                let hum = if snap.humidity.is_empty() {
                    "—".to_owned()
                } else {
                    snap.humidity
                };
                let wind = if snap.wind.is_empty() {
                    "—".to_owned()
                } else {
                    snap.wind
                };
                let cond = if snap.condition.is_empty() {
                    loc::t("WidgetWeather")
                } else {
                    snap.condition
                };
                let spec = FlyoutSpec {
                    title: String::new(),
                    subtitle: None,
                    width_dips: 296.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: city,
                            caption: Some(cond),
                            centered: false,
                            large: false,
                            right_meta: Some(temp),
                        },
                        FlyoutBlock::MetricTiles(vec![
                            (loc::t("FeelsLikeLabel").to_owned(), feels),
                            ("Humidity".to_owned(), hum),
                            ("Wind".to_owned(), wind),
                            ("Source".to_owned(), "wttr.in".to_owned()),
                        ]),
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(1, loc::t("AiUsageRefresh"), false)]),
                    ],
                };
                if flyout::show_flyout_with_border(
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    spec,
                    BorderStyle::from_widget_settings(is_dark, settings),
                ) == Some(1)
                {
                    telemetry::refresh_weather(&self.weather, &settings.weather_city, true);
                }
            }
            // Exact C# `BluetoothBatteryWidget.PopulateHoverFlyoutPanel`
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
                        "No connected Bluetooth battery devices detected.".to_owned(),
                    ));
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::ButtonBar(vec![
                    (1, loc::t("AiUsageRefresh"), false),
                    (
                        2,
                        loc::t_fmt("RadioSettings", &[&loc::t("Bluetooth")]).to_owned(),
                        false,
                    ),
                ]));
                let spec = FlyoutSpec {
                    title: loc::t("WidgetBluetoothBattery"),
                    subtitle: None,
                    width_dips: 276.0,
                    blocks,
                };
                match flyout::show_flyout_with_border(
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    spec,
                    BorderStyle::from_widget_settings(is_dark, settings),
                ) {
                    Some(1) => telemetry::refresh_bluetooth_battery(&self.bt_battery, true),
                    Some(2) => pins::launch("ms-settings:bluetooth".to_owned()),
                    _ => {}
                }
            }
            // Exact C# `StorageWidget` expanded drives flyout
            "storage" => {
                let drives = sample_drives();
                let mut blocks: Vec<FlyoutBlock> = drives
                    .iter()
                    .take(6)
                    .map(|d| {
                        let used = (d.total_gb - d.free_gb).max(0.0);
                        FlyoutBlock::ProgressBar {
                            label: format!("{}:", d.letter),
                            value_text: d.format_capacity_pair(),
                            ratio: (used / d.total_gb.max(1.0)) as f32,
                            tone: BarTone::Usage,
                        }
                    })
                    .collect();
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note("No fixed drives found.".to_owned()));
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::ButtonBar(vec![(
                    1,
                    loc::t("OpenFileExplorer").to_owned(),
                    false,
                )]));
                let spec = FlyoutSpec {
                    title: loc::t("WidgetStorage"),
                    subtitle: None,
                    width_dips: 286.0,
                    blocks,
                };
                if flyout::show_flyout_with_border(
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    spec,
                    BorderStyle::from_widget_settings(is_dark, settings),
                ) == Some(1)
                {
                    pins::launch("explorer.exe".to_owned());
                }
            }
            // Exact C# `KeyboardLayoutWidget.BuildFlyoutContent` (`BadgeRow` with code pill + full name)
            "keyboard" => {
                let layouts = telemetry::installed_keyboard_layouts();
                let active = active_keyboard_code();
                let blocks: Vec<FlyoutBlock> = layouts
                    .iter()
                    .enumerate()
                    .map(|(i, (_, code, display))| FlyoutBlock::BadgeRow {
                        id: 100 + i as u32,
                        code: code.clone(),
                        title: display.clone(),
                        active: code == &active,
                    })
                    .collect();
                let spec = FlyoutSpec {
                    title: loc::t("WidgetKeyboardLayout"),
                    subtitle: None,
                    width_dips: 240.0,
                    blocks,
                };
                if let Some(act) = flyout::show_flyout_with_border(
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    spec,
                    BorderStyle::from_widget_settings(is_dark, settings),
                ) && act >= 100
                    && let Some((hkl, _, _)) = layouts.get((act - 100) as usize)
                {
                    telemetry::activate_keyboard_layout(*hkl);
                }
            }
            "dayprogress" => {
                let (day, week, year, left) = day_progress_metrics();
                let spec = FlyoutSpec {
                    title: loc::t("WidgetDayProgress"),
                    subtitle: None,
                    width_dips: 272.0,
                    blocks: vec![
                        FlyoutBlock::Hero {
                            value: format!("{:.0}%", day * 100.0),
                            caption: Some(format!("{left}h remaining today")),
                            centered: false,
                            large: false,
                            right_meta: None,
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
                let _ = flyout::show_flyout_with_border(
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    spec,
                    BorderStyle::from_widget_settings(is_dark, settings),
                );
            }
            // Exact C# `FocusWidget.BuildFocusFlyout` (live in-place presets + Reset + Start/Pause)
            "focus" => {
                let init = self.build_focus_spec();
                flyout::show_flyout_live_with_border(
                    "focus",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    BorderStyle::from_widget_settings(is_dark, settings),
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            if self.focus_remaining_secs == 0 {
                                self.focus_remaining_secs = self.focus_preset_mins * 60;
                            }
                            self.focus_running = !self.focus_running;
                            LiveOutcome::Update(self.build_focus_spec())
                        }
                        LiveEvent::Click(2) => {
                            self.focus_running = false;
                            self.focus_remaining_secs = self.focus_preset_mins * 60;
                            LiveOutcome::Update(self.build_focus_spec())
                        }
                        LiveEvent::Click(mins @ (15 | 25 | 50)) => {
                            self.focus_preset_mins = u64::from(mins);
                            self.focus_remaining_secs = self.focus_preset_mins * 60;
                            self.focus_running = false;
                            LiveOutcome::Update(self.build_focus_spec())
                        }
                        _ => LiveOutcome::Close,
                    },
                );
            }
            // Exact C# `TimedWidgetPanel.xaml` Stopwatch (live Lap / Start / Pause / Reset)
            "stopwatch" => {
                let init = self.build_stopwatch_spec();
                flyout::show_flyout_live_with_border(
                    "stopwatch",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    BorderStyle::from_widget_settings(is_dark, settings),
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            self.stopwatch_running = !self.stopwatch_running;
                            LiveOutcome::Update(self.build_stopwatch_spec())
                        }
                        LiveEvent::Click(2) => {
                            self.stopwatch_running = false;
                            self.stopwatch_secs = 0;
                            self.stopwatch_laps.clear();
                            LiveOutcome::Update(self.build_stopwatch_spec())
                        }
                        LiveEvent::Click(3) => {
                            self.stopwatch_laps.push(self.stopwatch_secs);
                            LiveOutcome::Update(self.build_stopwatch_spec())
                        }
                        _ => LiveOutcome::Close,
                    },
                );
            }
            // Exact C# `TimedWidgetPanel.xaml` Countdown (live 5/10/15/30 presets + Start/Pause/Reset)
            "countdown" => {
                let init = self.build_countdown_spec();
                flyout::show_flyout_live_with_border(
                    "countdown",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    BorderStyle::from_widget_settings(is_dark, settings),
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            if self.countdown_remaining_secs == 0 {
                                self.countdown_remaining_secs = self.countdown_total_secs;
                            }
                            self.countdown_running = !self.countdown_running;
                            LiveOutcome::Update(self.build_countdown_spec())
                        }
                        LiveEvent::Click(2) => {
                            self.countdown_running = false;
                            self.countdown_remaining_secs = self.countdown_total_secs;
                            LiveOutcome::Update(self.build_countdown_spec())
                        }
                        LiveEvent::Click(mins @ (5 | 10 | 15 | 30)) => {
                            self.countdown_total_secs = u64::from(mins) * 60;
                            self.countdown_remaining_secs = self.countdown_total_secs;
                            self.countdown_running = false;
                            LiveOutcome::Update(self.build_countdown_spec())
                        }
                        _ => LiveOutcome::Close,
                    },
                );
            }
            "media" => {
                telemetry::refresh_media_session(&self.media_session, true);
                let build_media_spec = |media: &Arc<Mutex<telemetry::MediaSessionSnapshot>>| {
                    let snap = media.lock().ok().map(|g| g.clone()).unwrap_or_default();
                    let has_track = !snap.title.is_empty();
                    let mut blocks = vec![FlyoutBlock::Hero {
                        value: if has_track {
                            snap.title.clone()
                        } else {
                            loc::t("WidgetMediaDescription").to_owned()
                        },
                        caption: if !snap.artist.is_empty() {
                            Some(snap.artist.clone())
                        } else if has_track {
                            Some(if snap.playing { "Playing" } else { "Paused" }.to_owned())
                        } else {
                            Some(loc::t("WidgetMediaDescription").to_owned())
                        },
                        centered: false,
                        large: false,
                        right_meta: if has_track {
                            Some(if snap.playing { "Playing" } else { "Paused" }.to_owned())
                        } else {
                            None
                        },
                    }];
                    if snap.duration_secs > 0 {
                        let ratio =
                            (snap.position_secs as f32 / snap.duration_secs as f32).clamp(0.0, 1.0);
                        blocks.push(FlyoutBlock::ProgressBar {
                            label: format_mm_ss(snap.position_secs),
                            value_text: format_mm_ss(snap.duration_secs),
                            ratio,
                            tone: BarTone::Accent,
                        });
                    }
                    blocks.push(FlyoutBlock::ButtonBar(vec![
                        (1, loc::t("MediaPrevious").to_owned(), false),
                        (
                            2,
                            if snap.playing {
                                loc::t("TimedWidgetPause")
                            } else {
                                loc::t("MediaPlayPause")
                            }
                            .to_owned(),
                            true,
                        ),
                        (3, loc::t("MediaNext").to_owned(), false),
                    ]));
                    blocks.push(FlyoutBlock::Separator);
                    blocks.push(FlyoutBlock::ButtonBar(vec![(
                        4,
                        loc::t_fmt("RadioSettings", &[&loc::t("LauncherSound")]).to_owned(),
                        false,
                    )]));
                    FlyoutSpec {
                        title: loc::t("WidgetMedia"),
                        subtitle: None,
                        width_dips: 300.0,
                        blocks,
                    }
                };
                let init = build_media_spec(&self.media_session);
                flyout::show_flyout_live_with_border(
                    "media",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    BorderStyle::from_widget_settings(is_dark, settings),
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            telemetry::send_media_vk(VK_MEDIA_PREV_TRACK);
                            telemetry::refresh_media_session(&self.media_session, true);
                            LiveOutcome::Update(build_media_spec(&self.media_session))
                        }
                        LiveEvent::Click(2) => {
                            telemetry::send_media_vk(VK_MEDIA_PLAY_PAUSE);
                            telemetry::refresh_media_session(&self.media_session, true);
                            LiveOutcome::Update(build_media_spec(&self.media_session))
                        }
                        LiveEvent::Click(3) => {
                            telemetry::send_media_vk(VK_MEDIA_NEXT_TRACK);
                            telemetry::refresh_media_session(&self.media_session, true);
                            LiveOutcome::Update(build_media_spec(&self.media_session))
                        }
                        LiveEvent::Click(4) => {
                            pins::launch("ms-settings:sound".to_owned());
                            LiveOutcome::Close
                        }
                        _ => LiveOutcome::Close,
                    },
                );
            }
            "quicksettings" => {
                let mut settings_changed = false;
                let build_qs_spec = |settings: &DockSettings, night_light: bool| {
                    let (vol, muted) = get_audio_state();
                    let out_name = telemetry::default_audio_device_name(false)
                        .unwrap_or_else(|| "Speakers".to_owned());
                    let hide_label = match settings.auto_hide {
                        AutoHideMode::Disabled => loc::t("QuickOff"),
                        AutoHideMode::Smart => "Smart".to_owned(),
                        AutoHideMode::Always => "Always".to_owned(),
                    };
                    let theme_label = match settings.theme {
                        AppTheme::System => "System",
                        AppTheme::Dark => "Dark",
                        AppTheme::Light => "Light",
                    };
                    FlyoutSpec {
                        title: loc::t("WidgetQuickSettings"),
                        subtitle: None,
                        width_dips: 312.0,
                        blocks: vec![
                            FlyoutBlock::ToggleGrid(vec![
                                (
                                    1,
                                    loc::t("FocusMode").to_owned(),
                                    if settings.focus_mode {
                                        loc::t("QuickOn")
                                    } else {
                                        loc::t("QuickOff")
                                    }
                                    .to_owned(),
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
                                    7,
                                    loc::t("NightLight").to_owned(),
                                    if night_light {
                                        loc::t("QuickOn")
                                    } else {
                                        loc::t("QuickOff")
                                    }
                                    .to_owned(),
                                    night_light,
                                ),
                            ]),
                            FlyoutBlock::Separator,
                            FlyoutBlock::AudioSliderSection {
                                label: "Volume".to_owned(),
                                device_name: out_name,
                                value_text: if muted {
                                    "Muted".to_owned()
                                } else {
                                    format!("{vol}%")
                                },
                                ratio: vol as f32 / 100.0,
                                muted,
                                mute_id: 4,
                                slider_id: 10,
                            },
                            FlyoutBlock::Separator,
                            FlyoutBlock::ButtonBar(vec![
                                (5, "Wi-Fi".to_owned(), false),
                                (6, "Bluetooth".to_owned(), false),
                            ]),
                            FlyoutBlock::ButtonBar(vec![
                                (8, loc::t("Settings").to_owned(), true),
                                (9, "Show Desktop".to_owned(), false),
                            ]),
                        ],
                    }
                };
                let init = build_qs_spec(settings, self.night_light_on);
                flyout::show_flyout_live_with_border(
                    "quicksettings",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    BorderStyle::from_widget_settings(is_dark, settings),
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            settings.focus_mode = !settings.focus_mode;
                            settings_changed = true;
                            LiveOutcome::Update(build_qs_spec(settings, self.night_light_on))
                        }
                        LiveEvent::Click(2) => {
                            settings.auto_hide = match settings.auto_hide {
                                AutoHideMode::Disabled => AutoHideMode::Smart,
                                AutoHideMode::Smart => AutoHideMode::Always,
                                AutoHideMode::Always => AutoHideMode::Disabled,
                            };
                            settings_changed = true;
                            LiveOutcome::Update(build_qs_spec(settings, self.night_light_on))
                        }
                        LiveEvent::Click(3) => {
                            settings.theme = match settings.theme {
                                AppTheme::System => AppTheme::Dark,
                                AppTheme::Dark => AppTheme::Light,
                                AppTheme::Light => AppTheme::System,
                            };
                            settings_changed = true;
                            LiveOutcome::Update(build_qs_spec(settings, self.night_light_on))
                        }
                        LiveEvent::Click(4) => {
                            toggle_audio_mute();
                            (self.audio_vol, self.audio_muted) = get_audio_state();
                            LiveOutcome::Update(build_qs_spec(settings, self.night_light_on))
                        }
                        LiveEvent::Slider { id: 10, ratio } => {
                            set_audio_volume((ratio * 100.0).round() as u32);
                            (self.audio_vol, self.audio_muted) = get_audio_state();
                            LiveOutcome::Update(build_qs_spec(settings, self.night_light_on))
                        }
                        LiveEvent::Click(5) => {
                            pins::launch("ms-settings:network-wifi".to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::Click(6) => {
                            pins::launch("ms-settings:bluetooth".to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::Click(7) => {
                            if telemetry::toggle_night_light() {
                                self.night_light_on = !self.night_light_on;
                            } else {
                                pins::launch("ms-settings:nightlight".to_owned());
                            }
                            LiveOutcome::Update(build_qs_spec(settings, self.night_light_on))
                        }
                        LiveEvent::Click(8) => {
                            crate::ui::settings::spawn_settings_window();
                            LiveOutcome::Close
                        }
                        LiveEvent::Click(9) => {
                            opentaskbar_shell::taskbar::show_desktop();
                            LiveOutcome::Close
                        }
                        _ => LiveOutcome::Close,
                    },
                );
                if settings_changed {
                    return true;
                }
            }
            "stacks" => {
                let mut blocks: Vec<FlyoutBlock> = settings
                    .stack_folders
                    .iter()
                    .enumerate()
                    .map(|(i, path)| {
                        let name = std::path::Path::new(path)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or(path);
                        action_row(100 + i as u32, name, Some("Folder".to_owned()))
                    })
                    .collect();
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note(
                        "Add folder stacks in Settings -> Extras.".to_owned(),
                    ));
                }
                let spec = FlyoutSpec {
                    title: loc::t("FolderStacks"),
                    subtitle: None,
                    width_dips: 276.0,
                    blocks,
                };
                if let Some(act) = flyout::show_flyout_with_border(
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    spec,
                    BorderStyle::from_widget_settings(is_dark, settings),
                ) && act >= 100
                    && let Some(path) = settings.stack_folders.get((act - 100) as usize)
                {
                    pins::launch(path.clone());
                }
            }
            "groups" => {
                let mut blocks: Vec<FlyoutBlock> = settings
                    .app_groups
                    .iter()
                    .enumerate()
                    .map(|(i, g)| {
                        action_row(
                            100 + i as u32,
                            g.name.clone(),
                            Some(format!("{} apps", g.executable_paths.len())),
                        )
                    })
                    .collect();
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note(
                        "Create app groups in Settings -> Extras.".to_owned(),
                    ));
                }
                let spec = FlyoutSpec {
                    title: loc::t("AppGroups"),
                    subtitle: None,
                    width_dips: 276.0,
                    blocks,
                };
                if let Some(act) = flyout::show_flyout_with_border(
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    spec,
                    BorderStyle::from_widget_settings(is_dark, settings),
                ) && act >= 100
                    && let Some(group) = settings.app_groups.get((act - 100) as usize)
                {
                    for member in &group.executable_paths {
                        pins::launch(member.clone());
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
        let mut seen = std::collections::HashSet::new();
        for w in ALL_WIDGETS {
            assert!(seen.insert(w.id), "duplicate widget id {}", w.id);
            assert!(
                WIDGET_CATEGORIES.contains(&w.category),
                "unknown category {} for {}",
                w.category,
                w.id
            );
            assert!(w.width_dips >= 48.0);
        }
        assert!(widget_descriptor("clock").is_some());
        assert!(widget_descriptor("aiusage_claude").is_some());
        assert!(widget_descriptor("aiusage_cursor").is_some());
        assert!(widget_descriptor("bluetoothbattery").is_some());
        assert!(widget_descriptor("dayprogress").is_some());
        assert!(widget_descriptor("keyboard").is_some());
    }
}
