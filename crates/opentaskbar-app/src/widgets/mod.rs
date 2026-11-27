//! Unified 22-widget registry, runtime state, and reference parity `DockFlyout` builders.
//!
//! Categories (matching `DockWidgetService.cs`):
//! - `Time & Weather`    (5): `clock`, `weather`, `dayprogress`, `stopwatch`, `countdown`
//! - `System & Hardware` (5): `stats`, `netspeed`, `battery`, `bluetoothbattery`, `storage`
//! - `AI Quotas`         (7): `aiusage_claude`, `aiusage_codex`,
//!   `aiusage_opencode`, `aiusage_copilot`, `aiusage_deepseek`,
//!   `aiusage_openrouter`, `aiusage_cursor`
//! - `Media & Controls`  (4): `media`, `audio`, `quicksettings`, `keyboard`
//! - `Productivity`      (1): `focus`

// Public API consumed by WS-D (dock) / WS-E (UI) after merge; keep the module warning-clean.
#![allow(dead_code)]

pub mod ai_flyout;
pub mod catalog_ops;
pub mod diagnostics;
pub mod focus_break;
pub mod hover;
pub mod media_ext;
pub mod registry;
pub mod speedtest;
pub mod telemetry;
pub mod uia_raw;
pub mod widget_icons;
pub mod widget_snapshot;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::{AppTheme, AutoHideMode, DockSettings};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
};

use crate::design::{self, Rgb};
use crate::dock::dcomp::{SystemStatColumn, WidgetChipSpec, WidgetIcon};
use crate::platform::{pins, toast, wireless_battery};
use crate::ui::flyout::{self, BarTone, FlyoutBlock, FlyoutSpec, LiveEvent, LiveOutcome};
use crate::ui::menu::BorderStyle;
use telemetry::{BluetoothBatteryDevice, BluetoothDeviceKind};

#[allow(unused_imports)]
pub use catalog_ops::{
    catalog, installed_catalog_widget_ids, is_widget_installed, set_setting, setting_bool,
    setting_f64, setting_i64, setting_str, setting_string_list, setting_value,
};
#[allow(unused_imports)]
pub use focus_break::{FocusEvent, FocusPhase, FocusSession, SharedFocusSession, play_break_chime};
#[allow(unused_imports)]
pub use hover::{HOVER_INTENT_MS, HoverChange, HoverPhase, SUPPRESS_MS, WidgetHoverCoordinator};
#[allow(unused_imports)]
pub use media_ext::{
    AudioAppSession, MediaDetails, MediaTimeline, TimelineSample, confirm_seek,
    format_time as format_media_time, get_details as get_media_details, playing_glyph, seek_ratio,
    volume_from_wheel,
};
#[allow(unused_imports)]
pub use registry::{
    WIDGET_ALIASES, WIDGET_CATEGORIES, WidgetDensity, WidgetDescriptor, WidgetLayout, WidgetOption,
    active_widgets, canonical_widget_id, catalog_widgets, hardware_available, widget_descriptor,
    widget_has_flyout, widget_layout, widget_name,
};
#[allow(unused_imports)]
pub use speedtest::{STREAM_COUNT, SpeedTestController, SpeedTestResult, SpeedTestState};
#[allow(unused_imports)]
pub use telemetry::{
    active_keyboard_code, adjust_audio_volume, day_progress_metrics, empty_recycle_bin,
    get_audio_state, is_recycle_bin_non_empty, sample_battery, sample_drives, set_audio_volume,
    toggle_audio_mute,
};
#[allow(unused_imports)]
pub use widget_snapshot::{POLL_TTL, WidgetPollSnapshot};

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
    /// GPU utilization percent (`SystemStatsWidget` GPU meter).
    gpu_pct: u32,
    /// GPU die temperature in °C (reference GPU temperature meter).
    gpu_temp_c: Option<u32>,
    /// Consecutive failed GPU probes; reference hides the GPU meters after 5 misses.
    gpu_misses: u32,
    /// Consecutive successful GPU probes; reference requires 2 samples before showing meters.
    gpu_samples: u32,
    /// Thermal state 0 = normal, 1 = warning (>=70 °C), 2 = critical (>=85 °C).
    gpu_thermal_state: u8,
    /// `SystemStatsWidget`: 5 samples to enter a hotter band, 2 to leave it.
    gpu_thermal_enter: u8,
    gpu_thermal_exit: u8,
    /// Number of active stat columns on the Dock chip (1..=4), synced on `tick` for dynamic width.
    stats_col_count: u8,
    /// Rolling 10-sample throughput history (bytes/sec) for the NetSpeed sparkline chip.
    net_down_history: VecDeque<u64>,
    net_up_history: VecDeque<u64>,
    audio_vol: u32,
    audio_muted: bool,
    /// Accumulated wheel delta for 120-delta = 10% volume steps.
    volume_wheel_remainder: i32,
    stopwatch_running: bool,
    stopwatch_secs: u64,
    stopwatch_laps: Vec<u64>,
    /// Centiseconds elapsed within the current second (mm:ss.cc display).
    stopwatch_cs: u32,
    countdown_running: bool,
    countdown_total_secs: u64,
    countdown_remaining_secs: u64,
    /// Shared focus/break session (chip, flyout and island see the same state).
    focus: SharedFocusSession,
    /// Legacy mirror kept in sync for callers that read these directly.
    focus_running: bool,
    focus_preset_mins: u64,
    focus_remaining_secs: u64,
    focus_completed: u32,
    weather: Arc<Mutex<telemetry::WeatherCache>>,
    weather_fail_count: u32,
    weather_retry_at: Option<Instant>,
    bt_battery: Arc<Mutex<telemetry::BluetoothBatteryCache>>,
    wb_in_flight: Arc<std::sync::atomic::AtomicBool>,
    last_wb_poll: Option<Instant>,
    notified_low_bt: HashSet<String>,
    ai_usage: Arc<Mutex<telemetry::AiUsageCache>>,
    /// Full `ai_fetch_full` payloads keyed by provider id. Never rendered as `default()`: the
    /// flyout reads the last successfully fetched spend / breakdown / reset / plan / limit data.
    ai_full: Arc<Mutex<HashMap<String, (Instant, telemetry::AiFullDetail)>>>,
    ai_full_in_flight: Arc<Mutex<HashSet<String>>>,
    media_session: Arc<Mutex<telemetry::MediaSessionSnapshot>>,
    /// Seek-anchored media clock; rejects stale SMTC echoes after a confirmed seek.
    media_timeline: MediaTimeline,
    /// Monotonic epoch backing every `MediaTimeline` timestamp.
    timeline_epoch: Instant,
    /// 1 s TTL cache for disk / keyboard reads made by chip renderers on every VSync.
    poll_snapshot: WidgetPollSnapshot,
    /// Multi-stream NetSpeed test controller.
    speedtest: SpeedTestController,
    speedtest_started_at: Option<Instant>,
    /// Shared hover intent coordinator (WS-D calls the pub API from its mouse handlers).
    pub hover: WidgetHoverCoordinator,
    night_light_on: bool,
    /// Real Wi-Fi / Bluetooth radio states (C4 `platform::radio`).
    wifi_on: bool,
    bluetooth_on: bool,
    radio_loading: bool,
    radio_unavailable: bool,
    /// Pending target of a radio toggle, reconciled against the real state on the next poll.
    wifi_target: Option<bool>,
    bluetooth_target: Option<bool>,
    radio_denied: bool,
    radio_failed: bool,
    battery_anim_start: Instant,
    battery_charging: bool,
    keyboard_override: Option<(String, Instant)>,
    /// Sticky external foreground target: our own dock/flyout can take focus between scroll
    /// events, but the layout change must still go to the app the user was typing in.
    keyboard_target_hwnd: Option<usize>,
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
            gpu_pct: 0,
            gpu_temp_c: None,
            gpu_misses: 0,
            gpu_samples: 0,
            gpu_thermal_state: 0,
            gpu_thermal_enter: 0,
            gpu_thermal_exit: 0,
            stats_col_count: 2,
            net_down_history: VecDeque::from([0u64; 10]),
            net_up_history: VecDeque::from([0u64; 10]),
            audio_vol: 50,
            audio_muted: false,
            volume_wheel_remainder: 0,
            stopwatch_running: false,
            stopwatch_secs: 0,
            stopwatch_laps: Vec::new(),
            stopwatch_cs: 0,
            countdown_running: false,
            countdown_total_secs: 15 * 60,
            countdown_remaining_secs: 15 * 60,
            focus: SharedFocusSession::default(),
            focus_running: false,
            focus_preset_mins: 25,
            focus_remaining_secs: 25 * 60,
            focus_completed: 0,
            weather: Arc::new(Mutex::new(telemetry::WeatherCache::default())),
            weather_fail_count: 0,
            weather_retry_at: None,
            bt_battery: Arc::new(Mutex::new(telemetry::BluetoothBatteryCache::default())),
            wb_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_wb_poll: None,
            notified_low_bt: HashSet::new(),
            ai_usage: Arc::new(Mutex::new(telemetry::AiUsageCache::default())),
            ai_full: Arc::new(Mutex::new(HashMap::new())),
            ai_full_in_flight: Arc::new(Mutex::new(HashSet::new())),
            media_session: Arc::new(Mutex::new(telemetry::MediaSessionSnapshot::default())),
            media_timeline: MediaTimeline::default(),
            timeline_epoch: Instant::now(),
            poll_snapshot: WidgetPollSnapshot::default(),
            speedtest: SpeedTestController::new(),
            speedtest_started_at: None,
            hover: WidgetHoverCoordinator::new(),
            night_light_on: telemetry::read_night_light().unwrap_or(false),
            wifi_on: false,
            bluetooth_on: false,
            radio_loading: true,
            radio_unavailable: false,
            wifi_target: None,
            bluetooth_target: None,
            radio_denied: false,
            radio_failed: false,
            battery_anim_start: Instant::now(),
            battery_charging: false,
            keyboard_override: None,
            keyboard_target_hwnd: None,
            last_tick: Instant::now(),
            last_poll: None,
            last_night_light_check: None,
            tooltip_text: None,
        }
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

/// `StopwatchWidget` centisecond display: `mm:ss.cc`.
fn format_mm_ss_cs(total_secs: u64, centis: u32) -> String {
    format!(
        "{:02}:{:02}.{:02}",
        total_secs / 60,
        total_secs % 60,
        centis % 100
    )
}

/// GPU availability hysteresis (`SystemStatsWidget`): 5 consecutive misses hide the meters
/// and 2 consecutive samples bring them back. Returns `None` while the meters must stay hidden.
/// `BluetoothBatteryWidget` low-alert threshold cycle (0 disables the alert).
fn bt_next_threshold(current: i64) -> i64 {
    const STEPS: [i64; 4] = [0, 10, 20, 30];
    STEPS
        .iter()
        .find(|&&value| value > current)
        .copied()
        .unwrap_or(STEPS[0])
}

/// `SystemStatsWidget` thermal bands: 70 °C warning, 85 °C critical.
fn thermal_band(temp_c: u32) -> u8 {
    if temp_c >= 85 {
        2
    } else if temp_c >= 70 {
        1
    } else {
        0
    }
}

fn gpu_metrics_visible(samples: u32, misses: u32) -> bool {
    misses < 5 && samples >= 2
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

/// Copies diagnostics text to the Windows clipboard without showing a console window.
fn copy_to_clipboard(text: &str) {
    use std::os::windows::process::CommandExt;
    let escaped = text.replace('\'', "''");
    let script = format!("Set-Clipboard -Value '{escaped}'");
    let _ = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x0800_0000)
        .spawn();
}

/// `QuickSettingsWidget` radio state labels: `Loading`, `Unavailable`, `On`, `Off`.
fn radio_label(on: bool, loading: bool) -> String {
    if loading {
        loc::t("AiUsageStatusLoading")
    } else if on {
        loc::t("QuickOn")
    } else {
        loc::t("QuickOff")
    }
}

/// Applies a settings-controlled alias ID so rename/alias entries are resolved consistently.
fn resolved_widget_id(id: &str, settings: &DockSettings) -> String {
    let canonical = canonical_widget_id(id).to_owned();
    let aliases = setting_value(settings, "WidgetAliases");
    aliases
        .and_then(|v| v.as_object().cloned())
        .and_then(|m| {
            m.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(id))
                .and_then(|(_, v)| v.as_str())
                .map(str::to_owned)
        })
        .unwrap_or(canonical)
}

/// `WeatherUnit` conversion: telemetry caches Celsius, but the widget can show Fahrenheit.
fn weather_temp(celsius: &str, settings: &DockSettings) -> String {
    if celsius.is_empty() {
        return "—".to_owned();
    }
    let unit = setting_str(settings, "WeatherUnit").unwrap_or_else(|| "Celsius".to_owned());
    if unit.eq_ignore_ascii_case("fahrenheit") || unit == "°F" || unit.eq_ignore_ascii_case("f") {
        let c = celsius.trim_end_matches('°').parse::<f64>().unwrap_or(0.0);
        format!("{}°", (c * 9.0 / 5.0 + 32.0).round() as i32)
    } else {
        celsius.to_owned()
    }
}

/// Native Windows 10/11 `Segoe Fluent Icons` weather glyph matching both English and Turkish conditions.
fn weather_icon_glyph(condition: &str) -> u16 {
    let c = condition.to_lowercase();
    if c.contains("thunder") || c.contains("storm") || c.contains("fırtına") || c.contains("gök")
    {
        0xE945 // LightningBolt
    } else if c.contains("snow") || c.contains("kar") || c.contains("sleet") || c.contains("ice") {
        0xE9CA // Frigid / Snowflake
    } else if c.contains("rain")
        || c.contains("shower")
        || c.contains("drizzle")
        || c.contains("yağmur")
        || c.contains("sağanak")
    {
        0xEB42 // Drop / Rain
    } else if c.contains("fog")
        || c.contains("mist")
        || c.contains("haze")
        || c.contains("sis")
        || c.contains("cloud")
        || c.contains("overcast")
        || c.contains("bulut")
    {
        0xE753 // Cloud
    } else {
        0xE706 // Brightness / Sun
    }
}

/// Semantic accent color for weather glyphs (warm amber sun, cool sky rain/snow, slate cloud, amber storm).
fn weather_icon_color(condition: &str, is_dark: bool) -> Rgb {
    let pal = design::palette(is_dark);
    let c = condition.to_lowercase();
    if c.contains("thunder") || c.contains("storm") || c.contains("fırtına") || c.contains("gök")
    {
        pal.warning
    } else if c.contains("snow")
        || c.contains("kar")
        || c.contains("rain")
        || c.contains("shower")
        || c.contains("drizzle")
        || c.contains("yağmur")
        || c.contains("sağanak")
    {
        pal.accent
    } else if c.contains("fog")
        || c.contains("mist")
        || c.contains("haze")
        || c.contains("sis")
        || c.contains("cloud")
        || c.contains("overcast")
        || c.contains("bulut")
    {
        pal.widget_blue
    } else {
        pal.warning
    }
}

/// C4 `platform::core_audio::list_devices(false)` adapter. Assumed shape is `(id, display_name)`;
/// keeping the conversion in one place makes a signature change a one-line fix at merge time.
fn audio_output_devices() -> Vec<(String, String)> {
    crate::platform::core_audio::list_devices(false)
}

/// Capture endpoints for the Audio flyout's input device selector.
fn audio_input_devices() -> Vec<(String, String)> {
    crate::platform::core_audio::list_devices(true)
}

/// Maps a Quick Settings `IconBar` shortcut ID to its `ms-settings:` URI.
fn shortcut_uri(id: u32) -> &'static str {
    match id {
        100 => "ms-settings:network-wifi",
        101 => "ms-settings:bluetooth",
        102 => "ms-settings:nightlight",
        103 => "ms-settings:network-airplanemode",
        104 => "ms-settings:network-cellular",
        105 => "ms-settings:display",
        106 => "ms-settings:screenrotation",
        107 => "ms-settings:batterysaver",
        108 => "ms-settings:crossdevice",
        109 => "ms-settings:easeofaccess-closedcaptioning",
        110 => "ms-settings:quiethours",
        111 => "ms-settings:camera",
        112 => "ms-settings:easeofaccess-hearingaids",
        113 => "ms-settings:display-advanced",
        114 => "ms-settings:network-mobilehotspot",
        115 => "ms-settings:easeofaccess",
        116 => "ms-settings:taskbar",
        _ => "ms-settings:",
    }
}

/// Number of reference Control Center shortcut tiles in the Quick Settings flyout.
pub const QUICK_SETTINGS_SHORTCUT_COUNT: u32 = 17;

impl WidgetRuntime {
    /// Monotonic seconds on the media timeline clock.
    fn now_secs(&self) -> f64 {
        self.timeline_epoch.elapsed().as_secs_f64()
    }

    /// Disk data for chip renderers. Never samples a disk here: `tick` refreshes the 1 s snapshot.
    fn poll_drives(&self) -> &[telemetry::DriveSpace] {
        &self.poll_snapshot.drives
    }

    /// SMTC snapshot with the extrapolated playback position applied.
    fn media_snapshot(&self) -> telemetry::MediaSessionSnapshot {
        let mut snap = self
            .media_session
            .lock()
            .ok()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        snap.position_secs =
            media_ext::displayed_position(&self.media_timeline, &snap, self.now_secs());
        snap
    }

    /// Animation-only tick. The battery wave phase is derived from `battery_anim_start` inside
    /// `chip_spec`, so this advances no sensor and leaves the snapshot untouched. WS-8 can call
    /// this from `TIMER_WIDGET_ANIM` instead of a full `refresh_widgets` / chip surface rebuild.
    pub fn animation_tick(&mut self, _settings: &DockSettings) -> bool {
        // No sensor work: DComp transforms are driven by dock::dcomp from the wave phase alone.
        false
    }

    pub fn tick(&mut self, settings: &DockSettings) {
        let now = Instant::now();
        let dt_ms = now.duration_since(self.last_tick).as_millis() as u64;
        let dt = (dt_ms / 1000).min(60);
        if dt_ms > 0 {
            self.last_tick = now;
            if self.stopwatch_running {
                self.stopwatch_secs = self.stopwatch_secs.saturating_add(dt);
                self.stopwatch_cs = self
                    .stopwatch_cs
                    .saturating_add((dt_ms % 1000) as u32 % 100);
            }
            if self.countdown_running {
                self.countdown_remaining_secs = self.countdown_remaining_secs.saturating_sub(dt);
                if self.countdown_remaining_secs == 0 {
                    self.countdown_running = false;
                }
            }
            // Shared focus/break session: crossing a phase boundary plays the Asterisk chime.
            if settings.is_widget_enabled("focus") {
                let event = self
                    .focus
                    .with_mut(|s| s.tick(dt))
                    .unwrap_or(FocusEvent::None);
                match event {
                    FocusEvent::PhaseEnded(FocusPhase::Focus) => play_break_chime(),
                    FocusEvent::CycleCompleted => play_break_chime(),
                    _ => {}
                }
                let s = self.focus.snapshot();
                self.focus_running = s.running;
                self.focus_preset_mins = s.focus_minutes;
                self.focus_remaining_secs = s.remaining_secs;
                self.focus_completed = s.sessions_completed as u32;
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
        // Single cached read of the expensive chip inputs. Every renderer below (including the
        // 144 Hz animation path) reads this snapshot instead of touching the disk / locale APIs.
        self.poll_snapshot.refresh();

        if settings.is_widget_enabled("stats") {
            let show_cpu = settings.stats_meters & 1 != 0;
            let show_ram = settings.stats_meters & 2 != 0;
            let show_gpu = settings.stats_meters & 16 != 0;
            let show_gpu_temp = settings.stats_meters & 32 != 0;
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
            if (show_gpu || show_gpu_temp)
                && let Some((pct, temp)) = telemetry::sample_gpu_temp()
            {
                self.gpu_pct = pct;
                self.gpu_temp_c = temp;
                self.stats.gpu_history.push(pct as f32 / 100.0);
                self.gpu_samples = self.gpu_samples.saturating_add(1);
                self.gpu_misses = 0;
            } else if show_gpu || show_gpu_temp {
                self.gpu_misses = self.gpu_misses.saturating_add(1);
                if self.gpu_misses >= 5 {
                    self.gpu_samples = 0;
                    self.gpu_temp_c = None;
                    self.gpu_pct = 0;
                    self.stats.gpu_history.push(0.0);
                }
            }
            self.update_gpu_thermal(self.gpu_temp_c);
            let cols = u8::from(show_cpu)
                + u8::from(show_ram)
                + u8::from(show_gpu)
                + u8::from(show_gpu_temp);
            self.stats_col_count = cols.max(1);
        }
        if settings.is_widget_enabled("audio")
            || settings.is_widget_enabled("media")
            || settings.is_widget_enabled("quicksettings")
        {
            (self.audio_vol, self.audio_muted) = get_audio_state();
        }
        if settings.is_widget_enabled("media") {
            telemetry::refresh_media_session(&self.media_session, false);
            // Re-anchor the extrapolated clock from the latest SMTC fetch. A repeated sample
            // leaves the anchor alone; a confirmed seek survives stale echoes (WS-6 contract).
            if let Ok(guard) = self.media_session.lock() {
                let now = self.now_secs();
                let sample = media_ext::timeline_from_snapshot(&guard, self.timeline_epoch);
                let rate = if guard.playing { 1.0 } else { 0.0 };
                self.media_timeline.update(sample, rate, now, now);
            }
        }
        if settings.is_widget_enabled("netspeed") {
            let (rx, tx) = self.net.sample();
            if self.net_down_history.len() >= 10 {
                self.net_down_history.pop_front();
            }
            self.net_down_history.push_back(rx);
            if self.net_up_history.len() >= 10 {
                self.net_up_history.pop_front();
            }
            self.net_up_history.push_back(tx);
            if self.speedtest.poll(now, self.speedtest_started_at) {
                self.speedtest_started_at = None;
            }
        }
        if settings.is_widget_enabled("weather") {
            let can_fetch = self.weather_retry_at.is_none_or(|at| now >= at);
            if can_fetch {
                let had_data = self
                    .weather
                    .lock()
                    .ok()
                    .is_some_and(|g| !g.temp_c.is_empty());
                telemetry::refresh_weather(&self.weather, &settings.weather_city, false);
                self.weather_retry_at = if had_data {
                    Some(now + std::time::Duration::from_secs(30 * 60))
                } else {
                    // 2-minute retry while there is no usable data.
                    Some(now + std::time::Duration::from_secs(120))
                };
            }
        }
        if settings.is_widget_enabled("bluetoothbattery") {
            // Wireless HID/GATT probes can take up to PROVIDER_TIMEOUT (1.2 s), so keep polling
            // strictly off the UI thread and read the cached snapshot non-blockingly.
            if self
                .last_wb_poll
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(10))
            {
                self.last_wb_poll = Some(now);
                self.refresh_bluetooth_devices(settings);
            }
            // Ignored devices, aliases and listen-only were applied by `read_all_with_settings`;
            // the low-level provider cadence and flyout remain WS-G owned.
            let threshold = settings.wireless_battery_low_threshold.max(0) as u32;
            if threshold == 0 {
                self.notified_low_bt.clear();
            } else if let Ok(cache) = self.bt_battery.lock() {
                for dev in &cache.devices {
                    if dev.percent <= threshold {
                        if self.notified_low_bt.insert(dev.name.clone()) {
                            let msg = loc::t_fmt(
                                "WirelessBatteryLowToast",
                                &[&dev.name, &dev.percent.to_string()],
                            );
                            let _ = toast::show_info_toast(&msg);
                        }
                    } else if dev.percent > threshold.saturating_add(5) {
                        self.notified_low_bt.remove(&dev.name);
                    }
                }
            }
        }
        if settings.is_widget_enabled("quicksettings")
            && self
                .last_night_light_check
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(10))
        {
            self.last_night_light_check = Some(now);
            self.night_light_on = telemetry::read_night_light().unwrap_or(self.night_light_on);
            // C4: platform::radio::{state,set} — WinRT Radio control.
            self.wifi_on = crate::platform::radio::state("wifi");
            self.bluetooth_on = crate::platform::radio::state("bluetooth");
            // Reconcile a previous toggle: access denied has no refresh at all, a failed write
            // leaves a refresh but the state does not match the requested target (reference ChangeResult).
            if let Some(target) = self.wifi_target.take()
                && self.wifi_on != target
            {
                self.radio_failed = true;
                self.radio_denied = crate::platform::radio::last_refresh().is_none();
            }
            if let Some(target) = self.bluetooth_target.take()
                && self.bluetooth_on != target
            {
                self.radio_failed = true;
                self.radio_denied = crate::platform::radio::last_refresh().is_none();
            }
            self.radio_loading = false;
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
                if self.ai_full_detail(ai.id).is_none() {
                    self.refresh_ai_full(ai.id);
                }
            }
        }
    }

    /// Returns `true` when any dock widget needs a 15 FPS animation tick (e.g. reference Battery charging wave).
    pub fn needs_animation_tick(&self, settings: &DockSettings) -> bool {
        settings.animations_enabled
            && settings.is_widget_enabled("battery")
            && self.battery_charging
    }

    pub fn cycle_keyboard_layout(&mut self, forward: bool) {
        if let Some(code) = telemetry::cycle_keyboard_layout(forward, self.keyboard_target_hwnd) {
            self.keyboard_override = Some((code, Instant::now()));
        }
    }

    fn current_keyboard_code(&self) -> String {
        if let Some((ref code, at)) = self.keyboard_override
            && at.elapsed().as_millis() < 450
        {
            return code.clone();
        }
        // Cached by `tick` behind the 1 s poll gate; a freshly booted runtime falls back once.
        let cached = self.poll_snapshot.keyboard_code.clone();
        if cached.is_empty() {
            active_keyboard_code()
        } else {
            cached
        }
    }

    /// Returns the live dynamic width for a dock widget (matching `StorageWidget` multi-drive
    /// and `BluetoothBatteryWidget` multi-device width expansion).
    pub fn widget_width_dips(&self, id: &str) -> f32 {
        match id {
            "stats" => match self.stats_col_count {
                1 => 64.0,
                2 => 114.0,
                3 => 168.0,
                _ => 226.0,
            },
            "storage" => {
                let n = self.poll_drives().len().clamp(1, 3);
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

    /// `AudioWidget` wheel handler: 120 delta = 10% with fractional remainder carried across
    /// events. WS-D calls this from the dock's `WM_MOUSEWHEEL`.
    pub fn adjust_volume_wheel(&mut self, delta: i32) -> u32 {
        let next = volume_from_wheel(self.audio_vol, delta, &mut self.volume_wheel_remainder);
        set_audio_volume(next);
        (self.audio_vol, self.audio_muted) = get_audio_state();
        self.audio_vol
    }

    /// `KeyboardLayoutWidget`: send `WM_INPUTLANGCHANGEREQUEST` to the last *external*
    /// foreground window (never our own dock/island/flyout windows).
    pub fn activate_keyboard_on_last_external_window(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == std::process::id() {
                return;
            }
            // Remember the last external target so a later wheel event that lands while our own
            // window has focus still switches the app the user was actually typing in.
            self.keyboard_target_hwnd = Some(hwnd as usize);
            let layouts = telemetry::installed_keyboard_layouts();
            if layouts.len() < 2 {
                return;
            }
            let active = active_keyboard_code();
            let idx = layouts
                .iter()
                .position(|(_, code, _)| code == &active)
                .unwrap_or(0);
            let hkl = layouts[(idx + 1) % layouts.len()].0;
            telemetry::activate_keyboard_layout_for(hwnd as usize, hkl);
        }
    }

    /// Exposes the shared focus session to WS-D (island expanded details) and flyouts.
    pub fn focus_session(&self) -> SharedFocusSession {
        self.focus.clone()
    }

    /// Exposes the NetSpeed speed-test controller to the live flyout.
    pub fn speedtest(&mut self) -> &mut SpeedTestController {
        &mut self.speedtest
    }

    /// Off-thread wireless battery read used by both the 10 s tick and the flyout's Refresh
    /// button. Honours the persisted ignored-device / alias / listen-only settings in the platform
    /// provider, so a manual refresh never wakes a device that the user asked to listen to only.
    pub fn refresh_bluetooth_devices(&self, settings: &DockSettings) {
        if self
            .wb_in_flight
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let settings_clone = settings.clone();
        let bt_cache = Arc::clone(&self.bt_battery);
        let in_flight = Arc::clone(&self.wb_in_flight);
        let spawned = std::thread::Builder::new()
            .name("opentaskbar-wb".to_owned())
            .spawn(move || {
                let readings = wireless_battery::read_all_with_settings(&settings_clone);
                if readings.is_empty() {
                    telemetry::refresh_bluetooth_battery(&bt_cache, true);
                } else if let Ok(mut cache) = bt_cache.lock() {
                    cache.devices = readings
                        .into_iter()
                        .map(|reading| BluetoothBatteryDevice {
                            id: reading.id.clone(),
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
                            source_label: reading.source.label().to_owned(),
                            charging: reading.is_charging,
                            stale: reading.stale,
                            sampled_at: None,
                        })
                        .collect();
                    cache.last_fetch = Some(Instant::now());
                    cache.loading = false;
                }
                in_flight.store(false, std::sync::atomic::Ordering::Release);
            })
            .is_ok();
        if !spawned {
            self.wb_in_flight
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }

    /// Rebuilds a flyout spec from the *current* widget state. Called by the live flyout WM_TIMER
    /// update path (WS-7 `LiveEvent::Update`) so Clock / Stats / Focus / Countdown / Stopwatch /
    /// NetSpeed show live data instead of a frozen snapshot.
    pub fn live_spec(&mut self, id: &str, settings: &DockSettings, is_dark: bool) -> FlyoutSpec {
        match id {
            "clock" => {
                let info = telemetry::local_clock_info();
                Self::build_calendar_spec(
                    info.year,
                    info.month,
                    Some((info.year, info.month, info.day)),
                )
            }
            "stats" => {
                (self.cpu_pct, self.ram_pct) = self.stats.sample();
                self.stats_uptime = telemetry::system_uptime_string();
                self.last_stats_top = None;
                self.build_stats_spec(settings, is_dark)
            }
            "focus" => focus_break::build_focus_flyout(&self.focus.snapshot()),
            "stopwatch" => self.build_stopwatch_spec(),
            "countdown" => self.build_countdown_spec(),
            "netspeed" => {
                self.speedtest
                    .poll(Instant::now(), self.speedtest_started_at);
                let mut spec = speedtest::build_speedtest_flyout(
                    self.speedtest.state(),
                    self.speedtest.result(),
                );
                spec.blocks.insert(
                    0,
                    FlyoutBlock::MetricTiles(vec![
                        (
                            loc::t("NetworkDownload"),
                            telemetry::format_speed(self.net.rx_bps),
                        ),
                        (
                            loc::t("NetworkUpload"),
                            telemetry::format_speed(self.net.tx_bps),
                        ),
                    ]),
                );
                spec
            }
            _ => FlyoutSpec {
                title: String::new(),
                subtitle: None,
                width_dips: 280.0,
                blocks: vec![FlyoutBlock::Note(String::new())],
            },
        }
    }

    /// Applies 5-sample enter / 2-sample exit hysteresis to the GPU temperature badge so a single
    /// driver spike cannot flip the colour band.
    fn update_gpu_thermal(&mut self, temp_c: Option<u32>) -> u8 {
        let target = temp_c.map_or(0, thermal_band);
        if target == self.gpu_thermal_state {
            self.gpu_thermal_enter = 0;
            self.gpu_thermal_exit = 0;
            return self.gpu_thermal_state;
        }
        if target > self.gpu_thermal_state {
            self.gpu_thermal_exit = 0;
            self.gpu_thermal_enter = self.gpu_thermal_enter.saturating_add(1);
            if self.gpu_thermal_enter >= 5 {
                self.gpu_thermal_state = target;
                self.gpu_thermal_enter = 0;
            }
        } else {
            self.gpu_thermal_enter = 0;
            self.gpu_thermal_exit = self.gpu_thermal_exit.saturating_add(1);
            if self.gpu_thermal_exit >= 2 {
                self.gpu_thermal_state = target;
                self.gpu_thermal_exit = 0;
            }
        }
        self.gpu_thermal_state
    }

    /// Last successfully fetched full AI detail. The flyout never substitutes an empty default
    /// when a cached payload exists.
    pub fn ai_full_detail(&self, id: &str) -> Option<telemetry::AiFullDetail> {
        self.ai_full.lock().ok()?.get(id).and_then(|(at, detail)| {
            (at.elapsed() < Duration::from_secs(600)).then(|| detail.clone())
        })
    }

    /// Fetches `ai_fetch_full` off the UI thread and writes the result into the shared cache.
    /// Concurrent refreshes for the same provider are coalesced.
    pub fn refresh_ai_full(&self, id: &'static str) {
        let claimed = match self.ai_full_in_flight.lock() {
            Ok(mut guard) => guard.insert(id.to_owned()),
            Err(_) => false,
        };
        if !claimed {
            return;
        }
        let cache = Arc::clone(&self.ai_full);
        let in_flight = Arc::clone(&self.ai_full_in_flight);
        let worker_in_flight = Arc::clone(&in_flight);
        let spawned = std::thread::Builder::new()
            .name(format!("otb-ai-full-{id}"))
            .spawn(move || {
                let detail = telemetry::ai_fetch_full(id);
                if detail.error.is_none()
                    && let Ok(mut guard) = cache.lock()
                {
                    guard.insert(id.to_owned(), (Instant::now(), detail));
                }
                if let Ok(mut guard) = worker_in_flight.lock() {
                    guard.remove(id);
                }
            })
            .is_ok();
        if !spawned && let Ok(mut guard) = in_flight.lock() {
            guard.remove(id);
        }
    }

    /// Builds flyout detail from the cache, falling back to a severity-only snapshot while the
    /// first full fetch is still in flight.
    fn ai_flyout_detail(
        &self,
        id: &str,
        snap: &telemetry::AiUsageSnapshot,
    ) -> ai_flyout::AiFullDetail {
        if let Some(full) = self.ai_full_detail(id) {
            return ai_flyout::from_telemetry(&full);
        }
        let peak = snap.windows.iter().map(|w| w.used).fold(0.0f32, f32::max);
        ai_flyout::AiFullDetail {
            severity: ai_flyout::severity_for_usage(peak, false),
            ..Default::default()
        }
    }

    /// Exposes the hover coordinator's public API (WS-D already reaches it via the pub field).
    pub fn hover(&self) -> &WidgetHoverCoordinator {
        &self.hover
    }

    /// Builds the dynamic-island expanded details payload for the media widget.
    pub fn island_media_details(&self) -> MediaDetails {
        let snap = self.media_snapshot();
        media_ext::get_details(&snap, self.audio_vol, self.audio_muted)
    }

    /// Builds the exact reference 1-to-1 `WidgetChipSpec` rendered onto the dock pill.
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
                    // Exact reference AiUsageWidget.cs: Cursor shows a single long window row (`30d` / `7d`).
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
                            (0x9f, 0x9f, 0xf2) // reference CopilotBrandBrush
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
                let formatted_temp = snap.formatted_temp(settings.weather_unit);
                let formatted_range = snap.formatted_today_range(settings.weather_unit);
                let place = if snap.city.is_empty() {
                    settings.weather_city.clone()
                } else {
                    snap.city
                };
                let condition = if available {
                    snap.condition
                } else if snap.loading {
                    loc::t("WeatherLoading").to_owned()
                } else {
                    loc::t("WeatherUnavailable").to_owned()
                };
                // Exact reference WeatherWidget.cs WarmSunBrush (#E5AE5E) & RainBrush (#6EAAF3)
                let icon_color = if snap.is_rain {
                    (0x6e, 0xaa, 0xf3)
                } else {
                    (0xe5, 0xae, 0x5e)
                };
                WidgetChipSpec::Weather {
                    available,
                    icon_color,
                    is_rain: snap.is_rain,
                    temp: formatted_temp,
                    place,
                    condition,
                    today_range: (!formatted_range.is_empty()).then_some(formatted_range),
                }
            }
            "dayprogress" => {
                let (day, _, _, _) = day_progress_metrics();
                WidgetChipSpec::DayProgress {
                    ratio: day,
                    percent: format!("{:.0}%", day * 100.0),
                    unit: loc::t("DayProgressUnit"),
                }
            }
            "stopwatch" => WidgetChipSpec::Timed {
                is_focus: false,
                running: self.stopwatch_running,
                text: format_mm_ss_cs(self.stopwatch_secs, self.stopwatch_cs),
            },
            "countdown" => WidgetChipSpec::Timed {
                is_focus: false,
                running: self.countdown_running,
                text: format_mm_ss(self.countdown_remaining_secs),
            },
            "focus" => {
                let snapshot = self.focus.snapshot();
                let label = if snapshot.phase == FocusPhase::Break {
                    loc::t("Break").to_owned()
                } else {
                    snapshot.session_label()
                };
                let _ = label; // chip text stays tabular; phase is exposed in the flyout/island.
                WidgetChipSpec::Timed {
                    is_focus: true,
                    running: snapshot.running,
                    text: format_mm_ss(snapshot.remaining_secs),
                }
            }
            "stats" => {
                let show_cpu = settings.stats_meters & 1 != 0;
                let show_ram = settings.stats_meters & 2 != 0;
                let show_gpu = settings.stats_meters & 16 != 0;
                let show_gpu_temp = settings.stats_meters & 32 != 0;
                let mut columns = Vec::with_capacity(4);
                if show_cpu {
                    columns.push(SystemStatColumn {
                        label: "CPU".to_owned(),
                        value_text: format!("{}%", self.cpu_pct),
                        ratio: (self.cpu_pct as f32 / 100.0).clamp(0.0, 1.0),
                        bar_color: pal.widget_blue,
                        warm_text: self.cpu_pct >= 85,
                    });
                }
                if show_ram {
                    columns.push(SystemStatColumn {
                        label: "RAM".to_owned(),
                        value_text: format!("{}%", self.ram_pct),
                        ratio: (self.ram_pct as f32 / 100.0).clamp(0.0, 1.0),
                        bar_color: pal.widget_accent,
                        warm_text: self.ram_pct >= 85,
                    });
                }
                if show_gpu {
                    let gpu_ready = gpu_metrics_visible(self.gpu_samples, self.gpu_misses);
                    columns.push(SystemStatColumn {
                        label: "GPU".to_owned(),
                        value_text: if gpu_ready {
                            format!("{}%", self.gpu_pct)
                        } else {
                            "—".to_owned()
                        },
                        ratio: if gpu_ready {
                            (self.gpu_pct as f32 / 100.0).clamp(0.0, 1.0)
                        } else {
                            0.0
                        },
                        bar_color: pal.success,
                        warm_text: gpu_ready && self.gpu_pct >= 85,
                    });
                }
                if show_gpu_temp {
                    let unit_lbl = match settings.weather_unit {
                        opentaskbar_core::settings::TemperatureUnit::Celsius => {
                            loc::t("GpuCelsius")
                        }
                        opentaskbar_core::settings::TemperatureUnit::Fahrenheit => {
                            loc::t("GpuFahrenheit")
                        }
                    };
                    let (val_txt, ratio, warm) = match self.gpu_temp_c {
                        Some(c) => (
                            telemetry::format_temperature(f64::from(c), settings.weather_unit),
                            (c as f32 / 100.0).clamp(0.0, 1.0),
                            c >= 60,
                        ),
                        None => ("—".to_owned(), 0.0, false),
                    };
                    columns.push(SystemStatColumn {
                        label: unit_lbl,
                        value_text: val_txt,
                        ratio,
                        bar_color: pal.warning,
                        warm_text: warm,
                    });
                }
                WidgetChipSpec::SystemStats { columns }
            }
            "netspeed" => {
                let down = if self.speedtest.state() == SpeedTestState::Downloading {
                    speedtest::format_throughput(self.net.rx_bps)
                } else {
                    telemetry::format_speed(self.net.rx_bps)
                };
                let peak_down = self
                    .net_down_history
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0)
                    .max(64 * 1024) as f32;
                let peak_up = self
                    .net_up_history
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0)
                    .max(64 * 1024) as f32;
                let down_history: Vec<f32> = self
                    .net_down_history
                    .iter()
                    .map(|&v| (v as f32 / peak_down).clamp(0.04, 1.0))
                    .collect();
                let up_history: Vec<f32> = self
                    .net_up_history
                    .iter()
                    .map(|&v| (v as f32 / peak_up).clamp(0.04, 1.0))
                    .collect();
                WidgetChipSpec::NetSpeed {
                    down,
                    up: telemetry::format_speed(self.net.tx_bps),
                    down_history,
                    up_history,
                }
            }
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
                let ignored = setting_string_list(settings, "IgnoredWirelessDevices");
                let low_threshold = setting_i64(
                    settings,
                    "WirelessBatteryLowThreshold",
                    i64::from(settings.wireless_battery_low_threshold.max(0)),
                )
                .max(0) as u32;
                let devices: Vec<(telemetry::BluetoothDeviceKind, u32, bool, bool)> = self
                    .bt_battery
                    .lock()
                    .ok()
                    .map(|g| {
                        g.devices
                            .iter()
                            .filter(|d| !ignored.iter().any(|n| n.eq_ignore_ascii_case(&d.name)))
                            .take(3)
                            .map(|d| {
                                (
                                    d.kind,
                                    d.percent,
                                    low_threshold > 0 && d.percent <= low_threshold,
                                    d.percent >= 95,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                WidgetChipSpec::Bluetooth { devices }
            }
            "storage" => {
                let compact = setting_bool(settings, "StorageCompactMode", false);
                let drives: Vec<(String, String)> = self
                    .poll_snapshot
                    .fixed_drives()
                    .iter()
                    .take(if compact { 1 } else { 3 })
                    .map(|d| {
                        let pair = d.format_capacity_pair();
                        (
                            format!("{}:", d.letter),
                            if compact {
                                pair.split(" / ").next().unwrap_or(&pair).to_owned()
                            } else {
                                pair
                            },
                        )
                    })
                    .collect();
                WidgetChipSpec::Storage { drives }
            }
            "media" => {
                let snap = self.media_snapshot();
                let title = if snap.title.is_empty() {
                    loc::t("WidgetMediaDescription").to_owned()
                } else {
                    // `MediaWidget` shows a play/pause glyph before the title.
                    format!("{} {}", playing_glyph(snap.playing), snap.title)
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
            "quicksettings" => {
                // reference QuickSettingsWidget.xaml: Wi-Fi, Bluetooth, Night Light — real radio states.
                let (wifi, bt) = if self.radio_loading {
                    (false, false)
                } else {
                    (self.wifi_on, self.bluetooth_on)
                };
                WidgetChipSpec::QuickSettings {
                    pills: [
                        ("\u{E701}".to_owned(), wifi),
                        ("\u{E702}".to_owned(), bt),
                        ("\u{E708}".to_owned(), self.night_light_on),
                    ],
                }
            }
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
                    loc::t_fmt(
                        "AiUsageReadyCount",
                        &[
                            &active_count.to_string(),
                            &telemetry::AI_PROVIDERS.len().to_string(),
                        ],
                    )
                });
                WidgetChipSpec::Simple {
                    // AppIcon::Layers
                    icon: widget_icons::resolve_widget_icon(
                        id,
                        settings,
                        WidgetIcon::Glyph(0xE81E),
                    ),
                    icon_color: pal.widget_accent,
                    top: loc::t_fmt("AiUsageActiveCount", &[&active_count.to_string()]),
                    bottom,
                }
            }
            "stacks" => WidgetChipSpec::Simple {
                // AppIcon::Folder
                icon: widget_icons::resolve_widget_icon(id, settings, WidgetIcon::Glyph(0xE8B7)),
                icon_color: pal.warning,
                top: loc::t("StacksShort"),
                bottom: loc::t_fmt("FolderCount", &[&settings.stack_folders.len().to_string()]),
            },
            "groups" => WidgetChipSpec::Simple {
                // AppIcon::LayoutGrid
                icon: widget_icons::resolve_widget_icon(id, settings, WidgetIcon::Glyph(0xF0E2)),
                icon_color: pal.accent,
                top: loc::t("GroupsShort"),
                bottom: loc::t_fmt("GroupCount", &[&settings.app_groups.len().to_string()]),
            },
            _ => WidgetChipSpec::Keyboard {
                code: id.chars().take(3).collect::<String>().to_uppercase(),
            },
        }
    }

    /// Concise reference-style tooltip above each hovered widget.
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
                Some(1) => loc::t("MediaPlayPause").to_owned(),
                Some(2) => loc::t("MediaNext").to_owned(),
                Some(3) => if self.audio_muted {
                    loc::t("AudioUnmute")
                } else {
                    loc::t("AudioMute")
                }
                .to_owned(),
                _ => loc::t("DynamicIslandShortMedia").to_owned(),
            },
            "quicksettings" => match sub {
                Some(0) => loc::t_fmt(
                    "RadioWifiStatus",
                    &[&radio_label(self.wifi_on, self.radio_loading)],
                ),
                Some(1) => loc::t_fmt(
                    "RadioBluetoothStatus",
                    &[&radio_label(self.bluetooth_on, self.radio_loading)],
                ),
                Some(2) => loc::t_fmt(
                    "NightLightStatus",
                    &[&if self.night_light_on {
                        loc::t("QuickOn")
                    } else {
                        loc::t("QuickOff")
                    }],
                ),
                _ => loc::t("WidgetQuickSettings").to_owned(),
            },
            "audio" => {
                let dev = telemetry::default_audio_device_name(false)
                    .unwrap_or_else(|| loc::t("AudioOutput").to_owned());
                if self.audio_muted {
                    loc::t_fmt("AudioDeviceMuted", &[&dev])
                } else {
                    loc::t_fmt("AudioDeviceVolume", &[&dev, &self.audio_vol.to_string()])
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
                    format!(
                        "{} · {} · {}",
                        snap.city,
                        snap.formatted_temp(settings.weather_unit),
                        snap.condition
                    )
                } else {
                    loc::t_fmt("WeatherTooltip", &[&settings.weather_city])
                }
            }
            "stats" => {
                let history = self.stats.cpu_history.as_vec();
                let avg = if history.is_empty() {
                    0.0
                } else {
                    history.iter().sum::<f32>() / history.len() as f32
                };
                let gpu = if gpu_metrics_visible(self.gpu_samples, self.gpu_misses) {
                    match self.gpu_temp_c {
                        Some(t) => loc::t_fmt(
                            "StatsGpuTooltipWithTemp",
                            &[&self.gpu_pct.to_string(), &t.to_string()],
                        ),
                        None => loc::t_fmt("StatsGpuTooltip", &[&self.gpu_pct.to_string()]),
                    }
                } else {
                    String::new()
                };
                loc::t_fmt(
                    "StatsTooltip",
                    &[
                        &self.cpu_pct.to_string(),
                        &format!("{avg:.0}"),
                        &self.ram_pct.to_string(),
                        &gpu,
                        &loc::t("StatsUptime"),
                        &self.stats_uptime,
                    ],
                )
            }
            "dayprogress" => {
                let (day, _, _, left) = day_progress_metrics();
                loc::t_fmt(
                    "WidgetDayProgressTooltip",
                    &[&format!("{:.0}", day * 100.0), &left.to_string()],
                )
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
            "keyboard" => loc::t_fmt("KeyboardLayoutTooltip", &[&self.current_keyboard_code()]),
            "storage" => self
                .poll_drives()
                .iter()
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
                    loc::t("BatteryUnavailable").to_owned()
                } else if b.charging {
                    loc::t_fmt("BatteryChargingPercent", &[&b.percent.to_string()])
                } else if b.on_ac {
                    loc::t_fmt("BatteryPluggedIn", &[&b.percent.to_string()])
                } else if let Some(secs) = b.life_secs.filter(|&s| s >= 60) {
                    let h = secs / 3600;
                    let m = (secs % 3600) / 60;
                    if h > 0 {
                        loc::t_fmt(
                            "BatteryRemainingHours",
                            &[&b.percent.to_string(), &h.to_string(), &m.to_string()],
                        )
                    } else {
                        loc::t_fmt(
                            "BatteryRemainingMinutes",
                            &[&b.percent.to_string(), &m.to_string()],
                        )
                    }
                } else {
                    loc::t_fmt("BatteryPercent", &[&b.percent.to_string()])
                }
            }
            "bluetoothbattery" => loc::t("BluetoothBattery").to_owned(),
            _ => widget_name(id),
        }
    }

    /// Exact reference left-click dispatch: inline sub-buttons execute directly; AI, System Stats,
    /// and Net Speed chips have no flyout.
    pub fn handle_direct_click(
        &mut self,
        id: &str,
        rel_x_dips: f32,
        width_dips: f32,
        _settings: &mut DockSettings,
    ) -> DirectClickOutcome {
        if let Some(ai) = telemetry::ai_provider_meta(id) {
            telemetry::refresh_ai_provider(&self.ai_usage, ai.id, true);
            return DirectClickOutcome::Handled;
        }
        match id {
            "media" => {
                let controls_l = width_dips - 116.0;
                if rel_x_dips < controls_l {
                    telemetry::send_media_vk(VK_MEDIA_PLAY_PAUSE);
                    DirectClickOutcome::Handled
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
                // reference QuickSettingsWidget.xaml: Wi-Fi, Bluetooth, Night Light — real radio toggles.
                let third = (width_dips / 3.0).max(1.0);
                if rel_x_dips < third {
                    let kind = "wifi";
                    let target = !self.wifi_on;
                    if crate::platform::radio::set(kind, target) {
                        self.wifi_on = target;
                    } else {
                        self.radio_unavailable = true;
                        pins::launch("ms-settings:network-wifi".to_owned());
                    }
                    DirectClickOutcome::Handled
                } else if rel_x_dips < third * 2.0 {
                    let kind = "bluetooth";
                    let target = !self.bluetooth_on;
                    if crate::platform::radio::set(kind, target) {
                        self.bluetooth_on = target;
                    } else {
                        self.radio_unavailable = true;
                        pins::launch("ms-settings:bluetooth".to_owned());
                    }
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
                    self.focus.with_mut(|s| s.toggle());
                    let snap = self.focus.snapshot();
                    self.focus_running = snap.running;
                    self.focus_remaining_secs = snap.remaining_secs;
                    DirectClickOutcome::Handled
                } else {
                    DirectClickOutcome::OpenFlyout
                }
            }
            "battery" => {
                // Exact reference BatteryWidget.OpenPowerSettingsAction
                pins::launch("ms-settings:batterysaver".to_owned());
                DirectClickOutcome::Handled
            }
            "stats" => {
                pins::launch("taskmgr.exe".to_owned());
                DirectClickOutcome::Handled
            }
            "netspeed" => {
                pins::launch("ms-settings:network".to_owned());
                DirectClickOutcome::Handled
            }
            "bluetoothbattery" => {
                pins::launch("ms-settings:bluetooth".to_owned());
                DirectClickOutcome::Handled
            }
            "storage" => {
                pins::launch("explorer.exe".to_owned());
                DirectClickOutcome::Handled
            }
            "clock" | "audio" | "weather" | "keyboard" | "stacks" | "groups" => {
                DirectClickOutcome::OpenFlyout
            }
            _ => DirectClickOutcome::Handled,
        }
    }

    /// Quick secondary (right-click) shortcuts matching reference widget context menus.
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

    /// Maps a clicked calendar grid cell (`day_id_base + cell`, `0..42`) to a date.
    /// The leading blanks come from the culture's first day of week, matching the
    /// flyout grid; a hard-coded Monday offset silently shifted every click in
    /// Sunday-first (en-US) and Saturday-first (Arabic) cultures.
    pub(crate) fn calendar_cell_date(
        year: i32,
        month: u32,
        cell: i32,
        first_day_of_week: u32,
    ) -> (i32, u32, u32) {
        let first_wd = telemetry::weekday_of(year, month, 1);
        let leading = telemetry::calendar_leading_blanks(first_day_of_week, first_wd) as i32;
        let prev_m = if month == 1 { 12 } else { month - 1 };
        let prev_y = if month == 1 { year - 1 } else { year };
        let prev_days = telemetry::days_in_month(prev_y, prev_m) as i32;
        let cur_days = telemetry::days_in_month(year, month) as i32;
        let next_m = if month == 12 { 1 } else { month + 1 };
        let next_y = if month == 12 { year + 1 } else { year };
        let d_idx = cell - leading + 1;
        if d_idx < 1 {
            (prev_y, prev_m, (prev_days + d_idx) as u32)
        } else if d_idx > cur_days {
            (next_y, next_m, (d_idx - cur_days) as u32)
        } else {
            (year, month, d_idx as u32)
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
            width_dips: 316.0,
            blocks: vec![
                FlyoutBlock::Hero {
                    value: info.time_hm,
                    caption: Some(info.full_date.clone()),
                    centered: false,
                    large: false,
                    right_meta: Some(format!("{} · {}", info.tz_name, info.tz_label)),
                },
                FlyoutBlock::Separator,
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
                    label: loc::t_fmt("TodayDate", &[&info.full_date]),
                    value: String::new(),
                },
                FlyoutBlock::ButtonBar(vec![
                    (10, loc::t("GoToToday"), false),
                    (11, loc::t("LauncherDateTime"), false),
                ]),
            ],
        }
    }

    fn build_audio_mixer_spec(&self) -> FlyoutSpec {
        let (out_vol, out_muted) = get_audio_state();
        let out_name =
            telemetry::default_audio_device_name(false).unwrap_or_else(|| loc::t("Speakers"));
        let mic = telemetry::get_mic_state();
        let (in_vol, in_muted) = mic.unwrap_or((0, true));
        let in_name =
            telemetry::default_audio_device_name(true).unwrap_or_else(|| loc::t("NoMicDevice"));

        let mut blocks = vec![
            FlyoutBlock::AudioSliderSection {
                label: loc::t("AudioOutput"),
                device_name: out_name.clone(),
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
                label: loc::t("AudioInput"),
                device_name: in_name.clone(),
                value_text: if mic.is_none() {
                    "-".to_owned()
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
        ];
        // Per-app rows carry a real display name + Fluent icon from the AUMID-aware adapter.
        let sessions = media_ext::list_app_sessions();
        if !sessions.is_empty() {
            blocks.push(FlyoutBlock::Separator);
            blocks.push(FlyoutBlock::StatRow {
                label: loc::t("AudioApps"),
                value: String::new(),
            });
            for (index, session) in sessions.iter().take(6).enumerate() {
                blocks.push(FlyoutBlock::GlyphStatRow {
                    glyph: session.icon_glyph,
                    glyph_color: (0x9f, 0x9f, 0xf2),
                    label: session.app_name.clone(),
                    detail: None,
                    value: format!("{}%", session.volume),
                });
                blocks.push(FlyoutBlock::AudioSliderSection {
                    label: session.app_name.clone(),
                    device_name: session.session_id.clone(),
                    value_text: format!("{}%", session.volume),
                    ratio: session.volume as f32 / 100.0,
                    muted: session.muted,
                    mute_id: 100 + index as u32 * 2,
                    slider_id: 101 + index as u32 * 2,
                });
            }
        }
        let devices = audio_output_devices();
        let mut device_blocks: Vec<FlyoutBlock> = devices
            .iter()
            .enumerate()
            .take(6)
            .map(|(i, (_, name))| FlyoutBlock::BadgeRow {
                id: 200 + i as u32,
                code: "\u{E7F5}".to_owned(), // Headphone glyph (reference AudioWidget device selector)
                title: name.clone(),
                active: name.eq_ignore_ascii_case(&out_name),
            })
            .collect();
        if !device_blocks.is_empty() {
            device_blocks.insert(
                0,
                FlyoutBlock::StatRow {
                    label: loc::t("AudioOutputDevices"),
                    value: String::new(),
                },
            );
            device_blocks.push(FlyoutBlock::Separator);
        }
        let input_devices = audio_input_devices();
        let mut input_blocks: Vec<FlyoutBlock> = input_devices
            .iter()
            .enumerate()
            .take(6)
            .map(|(i, (_, name))| FlyoutBlock::BadgeRow {
                id: 300 + i as u32,
                code: "\u{E720}".to_owned(), // microphone glyph
                title: name.clone(),
                active: name.eq_ignore_ascii_case(&in_name),
            })
            .collect();
        if !input_blocks.is_empty() {
            input_blocks.insert(
                0,
                FlyoutBlock::StatRow {
                    label: loc::t("AudioInputDevices"),
                    value: String::new(),
                },
            );
            input_blocks.push(FlyoutBlock::Separator);
        }
        device_blocks.extend(input_blocks);
        FlyoutSpec {
            title: loc::t("WidgetAudio"),
            subtitle: None,
            width_dips: 314.0,
            blocks: {
                blocks.extend(device_blocks);
                blocks.push(FlyoutBlock::ButtonBar(vec![(
                    5,
                    loc::t_fmt("RadioSettings", &[&loc::t("LauncherSound")]),
                    false,
                )]));
                blocks
            },
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
                    label: loc::t("Focus"),
                    value: loc::t_fmt("FocusSessionsCount", &[&self.focus_completed.to_string()]),
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
                value: format_mm_ss_cs(self.stopwatch_secs, self.stopwatch_cs),
                caption: Some(loc::t("StopwatchQuickAction")),
                centered: true,
                large: true,
                right_meta: None,
            },
            FlyoutBlock::ButtonBar(vec![(3, loc::t("StopwatchLap"), false)]),
        ];
        if self.stopwatch_laps.is_empty() {
            blocks.push(FlyoutBlock::Note(loc::t("StopwatchNoLaps")));
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
                },
                true,
            ),
            (2, loc::t("Reset"), false),
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
                    (5, loc::t_fmt("CountdownMinutesShort", &["5"]), mins == 5),
                    (10, loc::t_fmt("CountdownMinutesShort", &["10"]), mins == 10),
                ]),
                FlyoutBlock::ButtonBar(vec![
                    (15, loc::t_fmt("CountdownMinutesShort", &["15"]), mins == 15),
                    (30, loc::t_fmt("CountdownMinutesShort", &["30"]), mins == 30),
                ]),
                // `TimedWidgetPanel` custom duration (C2 TextInput, validated 1-1440 minutes).
                FlyoutBlock::TextInput {
                    id: 50,
                    label: loc::t("CountdownCustomMinutes"),
                    value: format!("{mins}"),
                    numeric: true,
                    min: 1.0,
                    max: 1440.0,
                },
                FlyoutBlock::Separator,
                FlyoutBlock::ButtonBar(vec![
                    (
                        1,
                        if self.countdown_running {
                            loc::t("TimedWidgetPause")
                        } else {
                            loc::t("TimedWidgetStart")
                        },
                        true,
                    ),
                    (2, loc::t("Reset"), false),
                ]),
            ],
        }
    }

    /// Unified `SystemStatsWidget` flyout: localized metrics (WIP), GPU meters with
    /// hysteresis (WS-G), C2 `Chart`/`Sparkline` series (WS-C) and cached top processes.
    fn build_stats_spec(&self, settings: &DockSettings, is_dark: bool) -> FlyoutSpec {
        let pal = design::palette(is_dark);
        let top = self
            .stats_top
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let (_, used_gib, total_gib) = telemetry::sample_ram_info();
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as u64)
            .unwrap_or(1);
        let cpu_col = if self.cpu_pct >= 90 {
            pal.danger
        } else if self.cpu_pct >= 75 {
            pal.warning
        } else {
            pal.widget_blue
        };
        let ram_col = if self.ram_pct >= 90 {
            pal.danger
        } else if self.ram_pct >= 75 {
            pal.warning
        } else {
            pal.widget_accent
        };
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
            // C2 `Chart` blocks with named CPU / RAM series (WS-C contract).
            FlyoutBlock::Chart {
                label: "CPU".to_owned(),
                series: vec![("CPU".to_owned(), self.stats.cpu_history.as_vec())],
                accent: cpu_col,
            },
            FlyoutBlock::Chart {
                label: loc::t_fmt(
                    "StatsMemoryChart",
                    &[&format!("{used_gib:.1}"), &format!("{total_gib:.0}")],
                ),
                series: vec![("RAM".to_owned(), self.stats.ram_history.as_vec())],
                accent: ram_col,
            },
            FlyoutBlock::Sparkline {
                label: loc::t("StatsCpuTrend"),
                value_text: format!("{}%", self.cpu_pct),
                detail_text: None,
                samples: self.stats.cpu_history.as_vec(),
                color: cpu_col,
            },
        ];
        if gpu_metrics_visible(self.gpu_samples, self.gpu_misses) {
            let gpu_history = if self.stats.gpu_history.as_vec().is_empty() {
                vec![self.gpu_pct as f32 / 100.0; 45]
            } else {
                self.stats.gpu_history.as_vec()
            };
            blocks.push(FlyoutBlock::Chart {
                label: "GPU".to_owned(),
                series: vec![("GPU".to_owned(), gpu_history)],
                accent: pal.widget_blue,
            });
            blocks.push(FlyoutBlock::ProgressBar {
                label: loc::t("StatsGpuLoad"),
                value_text: format!("{}%", self.gpu_pct),
                ratio: self.gpu_pct as f32 / 100.0,
                tone: BarTone::Usage,
            });
            if let Some(temp) = self.gpu_temp_c {
                // Hysteresis-gated band: warning turns the bar amber, critical red.
                let tone = match self.gpu_thermal_state {
                    2 => BarTone::Usage,
                    1 => BarTone::Accent,
                    _ => BarTone::Level,
                };
                blocks.push(FlyoutBlock::ProgressBar {
                    label: loc::t("GpuTemperature"),
                    value_text: format!("{temp}°C"),
                    ratio: (temp as f32 / 100.0).clamp(0.0, 1.0),
                    tone,
                });
            }
        }
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
        blocks.push(FlyoutBlock::Separator);
        if top.is_empty() {
            let sampling = self
                .last_stats_top
                .is_none_or(|t| t.elapsed() < Duration::from_secs(10));
            blocks.push(FlyoutBlock::Note(loc::t(if sampling {
                "StatsSamplingProcesses"
            } else {
                "StatsTopProcessesEmpty"
            })));
            // WS-C fallback: sample directly while the background sampler has no data yet.
            let procs = telemetry::sample_top_processes(5);
            if !procs.is_empty() {
                blocks.push(FlyoutBlock::StatRow {
                    label: loc::t("StatsTopProcesses"),
                    value: String::new(),
                });
                for (name, bytes) in procs {
                    blocks.push(FlyoutBlock::GlyphStatRow {
                        glyph: 0xE7FC,
                        glyph_color: pal.widget_accent,
                        label: name,
                        detail: None,
                        value: telemetry::format_bytes(bytes),
                    });
                }
            }
        } else {
            blocks.push(FlyoutBlock::StatRow {
                label: loc::t("StatsTopProcesses"),
                value: String::new(),
            });
            for (name, bytes) in top {
                blocks.push(FlyoutBlock::GlyphStatRow {
                    glyph: 0xE7FC,
                    glyph_color: pal.widget_accent,
                    label: name,
                    detail: None,
                    value: telemetry::format_bytes(bytes),
                });
            }
        }
        blocks.push(FlyoutBlock::Separator);
        blocks.push(FlyoutBlock::ButtonBar(vec![
            (1, loc::t("TaskManager"), false),
            (2, loc::t("ResourceMonitor"), false),
            (3, loc::t("AiUsageRefresh"), false),
        ]));
        blocks.push(FlyoutBlock::Note(loc::t("StatsAvailabilityNote")));
        FlyoutSpec {
            title: loc::t("WidgetSystemStats"),
            subtitle: None,
            width_dips: 320.0,
            blocks,
        }
    }

    /// WS-1: `open_widget_flyout`'s return value means "persist settings and reload
    /// them live". Only a real `LiveEvent::Click` / `TextCommit` that actually
    /// changed a setting may return `true`; opening a flyout on hover must never
    /// do so (on the dock UI thread that chain blocks the WH_MOUSE_LL hook owner).
    fn flyout_settings_changed(id: &str, live_change: bool) -> bool {
        live_change && matches!(id, "quicksettings" | "bluetoothbattery")
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
        if !widget_has_flyout(id) {
            return false;
        }

        let pal = design::palette(is_dark);

        if id == "aiusage_all" {
            let mut blocks = Vec::new();
            let guard_map = self
                .ai_usage
                .lock()
                .ok()
                .map(|g| g.providers.clone())
                .unwrap_or_default();
            for ai in telemetry::AI_PROVIDERS {
                let snap = guard_map.get(ai.id).cloned().unwrap_or_default();
                if let Some(w) = snap.windows.first() {
                    blocks.push(FlyoutBlock::ProgressBar {
                        label: format!("{} ({})", ai.name, w.label),
                        value_text: format!("{:.0}% · {}", w.used * 100.0, w.reset_text),
                        ratio: w.used,
                        tone: BarTone::Usage,
                    });
                } else if let Some(bal) = snap.balance_text {
                    blocks.push(FlyoutBlock::StatRow {
                        label: ai.name.to_owned(),
                        value: bal,
                    });
                } else {
                    blocks.push(FlyoutBlock::StatRow {
                        label: ai.name.to_owned(),
                        value: if snap.has_cred {
                            loc::t("AiUsageConfigured")
                        } else {
                            loc::t("AiUsageStatusSignIn")
                        },
                    });
                }
            }
            blocks.push(FlyoutBlock::Separator);
            blocks.push(FlyoutBlock::ButtonBar(vec![
                (1, loc::t("AiUsageRefreshAll"), true),
                (2, loc::t("AiUsageFullRefreshAll"), false),
            ]));
            let spec = FlyoutSpec {
                title: loc::t("WidgetCategoryAi"),
                subtitle: Some(loc::t("AiUsageLiveUsageNote")),
                width_dips: 316.0,
                blocks,
            };
            match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                Some(1) => {
                    for ai in telemetry::AI_PROVIDERS {
                        telemetry::refresh_ai_provider(&self.ai_usage, ai.id, true);
                    }
                }
                Some(2) => {
                    for ai in telemetry::AI_PROVIDERS {
                        self.refresh_ai_full(ai.id);
                    }
                }
                _ => {}
            }
        }
        // Full `AiUsageWidget.BuildFlyout` — spend / breakdown / reset / plan chip /
        // severity dot / refresh. Full detail is read from the shared AI fetch cache.
        else if let Some(ai) = telemetry::ai_provider_meta(id) {
            let snap = self
                .ai_usage
                .lock()
                .ok()
                .and_then(|g| g.providers.get(ai.id).cloned())
                .unwrap_or_default();
            let has_data = snap.balance_text.is_some() || !snap.windows.is_empty();
            // Kick off a full fetch when there is no cached payload yet; the returned flyout uses
            // the cache as soon as the background fetch lands (no `AiFullDetail::default()`).
            if self.ai_full_detail(ai.id).is_none() {
                self.refresh_ai_full(ai.id);
            }
            let detail = self.ai_flyout_detail(ai.id, &snap);
            let spec = ai_flyout::build_ai_flyout(ai.id, &snap, &detail, settings, is_dark);
            match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                Some(1) => pins::launch(ai.dashboard_url.to_owned()),
                Some(2) => telemetry::refresh_ai_provider(&self.ai_usage, ai.id, true),
                Some(3) => self.refresh_ai_full(ai.id),
                _ => {}
            }
            let _ = (has_data, &pal);
        }

        match id {
            // System stats flyout: uptime, memory, GPU, charts and top processes (union).
            "stats" => {
                let init = self.build_stats_spec(settings, is_dark);
                flyout::show_flyout_live_with_border(
                    "stats",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    BorderStyle::from_widget_settings(is_dark, settings),
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            pins::launch("taskmgr.exe".to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::Click(2) => {
                            pins::launch("perfmon.exe".to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::Click(3) => {
                            (self.cpu_pct, self.ram_pct) = self.stats.sample();
                            self.stats_uptime = telemetry::system_uptime_string();
                            self.last_stats_top = None;
                            LiveOutcome::Update(self.build_stats_spec(settings, is_dark))
                        }
                        // WM_TIMER-driven LiveEvent::Update (WS-7) refreshes live values.
                        _ => LiveOutcome::Update(self.live_spec("stats", settings, is_dark)),
                    },
                );
            }
            // Exact `ClockWidget.BuildCalendar` (live interactive month/year nav & day selection)
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
                            let first_day = telemetry::first_day_of_week();
                            let picked =
                                Self::calendar_cell_date(shown_y, shown_m, cell, first_day);
                            selected = Some(picked);
                            shown_y = picked.0;
                            shown_m = picked.1;
                            LiveOutcome::Update(Self::build_calendar_spec(
                                shown_y, shown_m, selected,
                            ))
                        }
                        // Live tick: keep the clock/time header current.
                        _ => LiveOutcome::Update(self.live_spec("clock", settings, is_dark)),
                    },
                );
            }
            // Exact `AudioWidget.PopulateFlyout` (live interactive Output + Input mixer!)
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
                        // Per-app mixers from WS-G diagnostics (ids 100..199).
                        LiveEvent::Click(id) if (100..200).contains(&id) && id % 2 == 0 => {
                            let sessions = media_ext::list_app_sessions();
                            if let Some(session) = sessions.get(((id - 100) / 2) as usize) {
                                let _ =
                                    media_ext::set_app_mute(&session.session_id, !session.muted);
                            }
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        LiveEvent::Slider { id, ratio }
                            if (101..200).contains(&id) && id % 2 == 1 =>
                        {
                            let sessions = media_ext::list_app_sessions();
                            if let Some(session) = sessions.get(((id - 101) / 2) as usize) {
                                let _ = media_ext::set_app_volume(
                                    &session.session_id,
                                    (ratio * 100.0).round() as u32,
                                );
                            }
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        // C4 `platform::core_audio::set_default_device` — output selector (ids 200..=205).
                        LiveEvent::Click(id @ 200..=205) => {
                            if let Some((device_id, _)) =
                                audio_output_devices().get((id - 200) as usize)
                            {
                                crate::platform::core_audio::set_default_device(device_id);
                            }
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        // Input device selector (300..=305).
                        LiveEvent::Click(id @ 300..=305) => {
                            if let Some((device_id, _)) =
                                audio_input_devices().get((id - 300) as usize)
                            {
                                crate::platform::core_audio::set_default_device(device_id);
                            }
                            LiveOutcome::Update(self.build_audio_mixer_spec())
                        }
                        _ => LiveOutcome::Close,
                    },
                );
                (self.audio_vol, self.audio_muted) = get_audio_state();
            }
            // Exact `WeatherWidget.BuildFlyoutContent`
            "weather" => {
                let snap = self.weather.lock().map(|g| g.clone()).unwrap_or_default();
                let city = if snap.city.is_empty() {
                    settings.weather_city.clone()
                } else {
                    snap.city.clone()
                };
                let temp = if snap.temp_c.is_empty() {
                    "—".to_owned()
                } else {
                    snap.formatted_temp(settings.weather_unit)
                };
                let feels = if snap.feels_like.is_empty() {
                    "—".to_owned()
                } else {
                    snap.formatted_feels_like(settings.weather_unit)
                };
                // `WeatherUnit` override from the settings catalog (WS-C), when configured.
                let temp = if setting_str(settings, "WeatherUnit").is_some() {
                    weather_temp(&snap.temp_c, settings)
                } else {
                    temp
                };
                let feels = if setting_str(settings, "WeatherUnit").is_some() {
                    weather_temp(&snap.feels_like, settings)
                } else {
                    feels
                };
                let wind = snap.formatted_wind(settings.weather_unit);
                let wind = if wind.is_empty() {
                    "—".to_owned()
                } else {
                    wind
                };
                let hum = if snap.humidity.is_empty() {
                    "—".to_owned()
                } else {
                    snap.humidity.clone()
                };
                let cond = if snap.condition.is_empty() {
                    loc::t("WidgetWeather")
                } else {
                    snap.condition.clone()
                };
                let unit_label = match settings.weather_unit {
                    opentaskbar_core::settings::TemperatureUnit::Celsius => "°C",
                    opentaskbar_core::settings::TemperatureUnit::Fahrenheit => "°F",
                };
                let mut blocks = vec![
                    FlyoutBlock::HeroHeader {
                        glyph: weather_icon_glyph(&snap.condition),
                        glyph_color: weather_icon_color(&snap.condition, is_dark),
                        title: if city.is_empty() {
                            loc::t("WidgetWeather")
                        } else {
                            city.clone()
                        },
                        subtitle: Some(cond),
                        right_value: Some(temp),
                    },
                    FlyoutBlock::MetricTiles(vec![
                        (loc::t("FeelsLikeLabel"), feels),
                        (loc::t("WeatherHumidity"), hum),
                        (loc::t("WeatherWind"), wind),
                        (loc::t("WeatherTemperatureUnit"), unit_label.to_owned()),
                        // Data source: the Rust port fetches Open-Meteo (geocoding + forecast).
                        (loc::t("WeatherSource"), "Open-Meteo".to_owned()),
                    ]),
                    FlyoutBlock::StatRow {
                        label: loc::t("WeatherToday"),
                        value: snap.formatted_today_range(settings.weather_unit),
                    },
                ];
                if !snap.forecast_days.is_empty() {
                    blocks.push(FlyoutBlock::Separator);
                    blocks.push(FlyoutBlock::StatRow {
                        label: loc::t("WeatherForecast3Days"),
                        value: String::new(),
                    });
                    for (i, (day, condition, high_low)) in
                        snap.forecast_days.iter().take(3).enumerate()
                    {
                        let value = if let Some((Some(hi), Some(lo))) = snap.forecast_hi_lo_c.get(i)
                        {
                            format!(
                                "{} / {}",
                                telemetry::format_temperature(*hi, settings.weather_unit),
                                telemetry::format_temperature(*lo, settings.weather_unit)
                            )
                        } else {
                            high_low.clone()
                        };
                        blocks.push(FlyoutBlock::GlyphStatRow {
                            glyph: weather_icon_glyph(condition),
                            glyph_color: weather_icon_color(condition, is_dark),
                            label: day.clone(),
                            detail: Some(condition.clone()),
                            value,
                        });
                    }
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::ButtonBar(vec![(
                    1,
                    loc::t("AiUsageRefresh"),
                    false,
                )]));
                let spec = FlyoutSpec {
                    title: String::new(),
                    subtitle: None,
                    width_dips: 312.0,
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
                    telemetry::refresh_weather(&self.weather, &settings.weather_city, true);
                    self.weather_retry_at =
                        Some(Instant::now() + std::time::Duration::from_secs(30 * 60));
                }
            }
            // `NetSpeedWidget`: live throughput + 6-stream speed test with cancel/retry/restart.
            "netspeed" => {
                let adapter = if self.net.adapter_name.is_empty() {
                    None
                } else {
                    Some(loc::t_fmt("NetworkAdapterLabel", &[&self.net.adapter_name]))
                };
                let build = |runtime: &Self| {
                    let mut spec = speedtest::build_speedtest_flyout(
                        runtime.speedtest.state(),
                        runtime.speedtest.result(),
                    );
                    spec.subtitle = adapter.clone();
                    // Live throughput + session totals above the test grid.
                    let mut blocks = vec![
                        FlyoutBlock::MetricTiles(vec![
                            (
                                loc::t_fmt("NetworkDirectionDown", &[&loc::t("NetworkDownload")]),
                                telemetry::format_speed(runtime.net.rx_bps),
                            ),
                            (
                                loc::t_fmt("NetworkDirectionUp", &[&loc::t("NetworkUpload")]),
                                telemetry::format_speed(runtime.net.tx_bps),
                            ),
                            (
                                loc::t("NetworkSessionReceived"),
                                telemetry::format_bytes(runtime.net.session_rx),
                            ),
                            (
                                loc::t("NetworkSessionSent"),
                                telemetry::format_bytes(runtime.net.session_tx),
                            ),
                        ]),
                        FlyoutBlock::Separator,
                    ];
                    blocks.append(&mut spec.blocks);
                    blocks.push(FlyoutBlock::ButtonBar(vec![(
                        5,
                        loc::t("NetworkSettings"),
                        false,
                    )]));
                    spec.blocks = blocks;
                    spec
                };
                let init = build(self);
                flyout::show_flyout_live(
                    "netspeed",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            self.speedtest.start();
                            self.speedtest_started_at = Some(Instant::now());
                            self.speedtest
                                .poll(Instant::now(), self.speedtest_started_at);
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Click(2) => {
                            self.speedtest.cancel();
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Click(3) => {
                            self.speedtest.retry();
                            self.speedtest_started_at = Some(Instant::now());
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Click(4) => {
                            self.speedtest.restart();
                            self.speedtest_started_at = Some(Instant::now());
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Click(5) => {
                            pins::launch("ms-settings:network".to_owned());
                            LiveOutcome::Close
                        }
                        _ => {
                            self.speedtest
                                .poll(Instant::now(), self.speedtest_started_at);
                            LiveOutcome::Update(build(self))
                        }
                    },
                );
            }
            // `BluetoothBatteryWidget.PopulateHoverFlyoutPanel` + device management.
            "bluetoothbattery" => {
                // WS-1: only a real live-event click/commit may request a settings save.
                // Hover-generated flyouts must leave this `false`.
                let mut settings_changed = false;
                let mut selected = 0usize;
                let mut feedback: Option<String> = None;
                // Visible rows use 100+i ids; the same list resolves a click back to a device so
                // rename / hide always act on the *selected* row, not blindly on `devices[0]`.
                let visible = |cfg: &DockSettings| -> Vec<BluetoothBatteryDevice> {
                    let ignored = setting_string_list(cfg, "IgnoredWirelessDevices");
                    self.bt_battery
                        .lock()
                        .map(|g| {
                            g.devices
                                .iter()
                                .filter(|d| {
                                    !ignored.iter().any(|n| n.eq_ignore_ascii_case(&d.name))
                                })
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let alias_for = |cfg: &DockSettings, name: &str| -> String {
                    setting_value(cfg, "WirelessDeviceAliases")
                        .and_then(|v| {
                            v.as_object()
                                .and_then(|m| m.get(name))
                                .and_then(|s| s.as_str())
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| name.to_owned())
                };
                let build = |cfg: &DockSettings,
                             selected: usize,
                             rename_value: String,
                             feedback: Option<&str>| {
                    let devices = visible(cfg);
                    let low = setting_i64(cfg, "WirelessBatteryLowThreshold", 20) as u32;
                    let interval = setting_i64(cfg, "WirelessBatteryPollSeconds", 60) as u64;
                    let listen_only = setting_bool(cfg, "WirelessBatteryListenOnly", false);
                    let last_fetch = self.bt_battery.lock().ok().and_then(|g| g.last_fetch);
                    let mut blocks: Vec<FlyoutBlock> = devices
                        .iter()
                        .enumerate()
                        .map(|(i, d)| {
                            let shown = alias_for(cfg, &d.name);
                            let charging = d.charging || d.percent >= 95;
                            let stale = d.stale
                                || d.sampled_at
                                    .is_some_and(|t| t.elapsed().as_secs() > interval * 2)
                                || (d.sampled_at.is_none()
                                    && last_fetch
                                        .is_some_and(|t| t.elapsed().as_secs() > interval * 2));
                            let subtitle = if stale {
                                loc::t_fmt("WirelessBatteryStale", &[&d.percent.to_string()])
                            } else if charging {
                                loc::t("WirelessBatteryCharging")
                            } else {
                                format!("{} · {}", d.source_label, d.percent)
                            };
                            FlyoutBlock::ActionRow {
                                id: 100 + i as u32,
                                title: format!("{shown}{}", if charging { " ⚡" } else { "" }),
                                subtitle: Some(subtitle),
                                badge: Some(format!(
                                    "{}%{}",
                                    d.percent,
                                    if charging { " ⚡" } else { "" }
                                )),
                                icon: None,
                                destructive: false,
                            }
                        })
                        .collect();
                    if blocks.is_empty() {
                        blocks.push(FlyoutBlock::Note(loc::t("WirelessBatteryNoDevices")));
                    }
                    if let Some(d) = devices.get(selected.min(devices.len().saturating_sub(1))) {
                        blocks.push(FlyoutBlock::Separator);
                        blocks.push(FlyoutBlock::StatRow {
                            label: "Source".to_owned(),
                            value: d.source_label.clone(),
                        });
                    }
                    if let Some(note) = feedback {
                        blocks.push(FlyoutBlock::Note(note.to_owned()));
                    }
                    // C2 TextInput — device rename (1-64 chars).
                    blocks.push(FlyoutBlock::TextInput {
                        id: 50,
                        label: loc::t("WirelessBatteryRename"),
                        value: rename_value,
                        numeric: false,
                        min: 0.0,
                        max: 0.0,
                    });
                    blocks.push(FlyoutBlock::StatRow {
                        label: loc::t("WirelessBatteryPollIntervalLabel"),
                        value: format!("{interval}s"),
                    });
                    blocks.push(FlyoutBlock::StatRow {
                        label: loc::t("WirelessBatteryLowThresholdLabel"),
                        value: format!("{low}%"),
                    });
                    blocks.push(FlyoutBlock::ToggleGrid(vec![(
                        7,
                        loc::t("WirelessBatteryListenOnlyLabel"),
                        if listen_only {
                            loc::t("WirelessBatteryListenOnlyOn")
                        } else {
                            loc::t("WirelessBatteryListenOnlyOff")
                        },
                        listen_only,
                    )]));
                    blocks.push(FlyoutBlock::ButtonBar(vec![
                        (1, loc::t("WirelessBatteryRefresh"), false),
                        (2, loc::t("WirelessBatteryHideDevice"), false),
                        (3, loc::t("WirelessBatteryRestoreIgnored"), false),
                        (4, loc::t("WirelessBatteryIntervalButton"), false),
                        (5, loc::t("WirelessBatteryThresholdButton"), false),
                    ]));
                    blocks.push(FlyoutBlock::ButtonBar(vec![
                        (6, loc::t("WirelessBatteryCopyDiagnostics"), false),
                        (8, loc::t("QuickBluetoothSettings"), false),
                    ]));
                    FlyoutSpec {
                        title: loc::t("BluetoothBattery"),
                        subtitle: Some(loc::t_fmt(
                            "WirelessBatterySubtitle",
                            &[&interval.to_string(), &low.to_string()],
                        )),
                        width_dips: 296.0,
                        blocks,
                    }
                };
                let prefill = visible(settings)
                    .first()
                    .map(|d| alias_for(settings, &d.name))
                    .unwrap_or_default();
                let init = build(settings, 0, prefill, None);
                flyout::show_flyout_live_with_border(
                    "bluetoothbattery",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    BorderStyle::from_widget_settings(is_dark, settings),
                    |ev| match ev {
                        // Device row selection: prefill the rename box from the selected device.
                        LiveEvent::Click(id @ 100..=199) => {
                            let idx = (id - 100) as usize;
                            if let Some(d) = visible(settings).get(idx) {
                                selected = idx;
                                feedback = None;
                                LiveOutcome::Update(build(
                                    settings,
                                    selected,
                                    alias_for(settings, &d.name),
                                    feedback.as_deref(),
                                ))
                            } else {
                                LiveOutcome::Close
                            }
                        }
                        LiveEvent::Click(1) => {
                            // Listen-only devices are refreshed passively by the platform provider.
                            self.refresh_bluetooth_devices(settings);
                            feedback = Some(loc::t("WirelessBatteryRefresh"));
                            LiveOutcome::Update(build(
                                settings,
                                selected,
                                visible(settings)
                                    .get(selected)
                                    .map(|d| alias_for(settings, &d.name))
                                    .unwrap_or_default(),
                                feedback.as_deref(),
                            ))
                        }
                        LiveEvent::Click(2) => {
                            // Hide the *selected* device (not simply the first one).
                            if let Some(d) = visible(settings).get(selected) {
                                let mut list =
                                    setting_string_list(settings, "IgnoredWirelessDevices");
                                if !list.iter().any(|n| n.eq_ignore_ascii_case(&d.name)) {
                                    list.push(d.name.clone());
                                    set_setting(
                                        settings,
                                        "IgnoredWirelessDevices",
                                        serde_json::json!(list),
                                    );
                                    settings_changed = true;
                                }
                                selected = selected.saturating_sub(1);
                            }
                            let prefill = visible(settings)
                                .get(selected)
                                .map(|d| alias_for(settings, &d.name))
                                .unwrap_or_default();
                            LiveOutcome::Update(build(
                                settings,
                                selected,
                                prefill,
                                feedback.as_deref(),
                            ))
                        }
                        LiveEvent::Click(3) => {
                            set_setting(settings, "IgnoredWirelessDevices", serde_json::json!([]));
                            settings_changed = true;
                            selected = 0;
                            let prefill = visible(settings)
                                .first()
                                .map(|d| alias_for(settings, &d.name))
                                .unwrap_or_default();
                            LiveOutcome::Update(build(settings, 0, prefill, feedback.as_deref()))
                        }
                        LiveEvent::Click(4) => {
                            const STEPS: [i64; 5] = [15, 30, 60, 120, 300];
                            let cur = setting_i64(settings, "WirelessBatteryPollSeconds", 60);
                            let next = STEPS
                                .iter()
                                .find(|&&s| s > cur)
                                .copied()
                                .unwrap_or(STEPS[0]);
                            set_setting(
                                settings,
                                "WirelessBatteryPollSeconds",
                                serde_json::json!(next),
                            );
                            settings_changed = true;
                            LiveOutcome::Update(build(
                                settings,
                                selected,
                                visible(settings)
                                    .get(selected)
                                    .map(|d| alias_for(settings, &d.name))
                                    .unwrap_or_default(),
                                feedback.as_deref(),
                            ))
                        }
                        LiveEvent::Click(5) => {
                            // reference cycles 0 / 10 / 20 / 30 percent (0 disables the low alert).
                            let cur = setting_i64(settings, "WirelessBatteryLowThreshold", 20);
                            let next = bt_next_threshold(cur);
                            set_setting(
                                settings,
                                "WirelessBatteryLowThreshold",
                                serde_json::json!(next),
                            );
                            settings_changed = true;
                            LiveOutcome::Update(build(
                                settings,
                                selected,
                                visible(settings)
                                    .get(selected)
                                    .map(|d| alias_for(settings, &d.name))
                                    .unwrap_or_default(),
                                feedback.as_deref(),
                            ))
                        }
                        LiveEvent::Click(6) => {
                            let diag = self
                                .bt_battery
                                .lock()
                                .map(|g| {
                                    g.devices
                                        .iter()
                                        .map(|d| {
                                            format!(
                                                "{}|{}|{}%|{}|{}",
                                                d.name,
                                                d.id,
                                                d.percent,
                                                d.source_label,
                                                if d.charging {
                                                    "charging"
                                                } else {
                                                    "discharging"
                                                }
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_default();
                            copy_to_clipboard(&diag);
                            // Visible feedback instead of a silent clipboard write.
                            feedback = Some(loc::t("WirelessBatteryDiagnosticsCopied"));
                            LiveOutcome::Update(build(
                                settings,
                                selected,
                                visible(settings)
                                    .get(selected)
                                    .map(|d| alias_for(settings, &d.name))
                                    .unwrap_or_default(),
                                feedback.as_deref(),
                            ))
                        }
                        LiveEvent::Click(7) => {
                            let next = !setting_bool(settings, "WirelessBatteryListenOnly", false);
                            set_setting(
                                settings,
                                "WirelessBatteryListenOnly",
                                serde_json::json!(next),
                            );
                            settings_changed = true;
                            self.refresh_bluetooth_devices(settings);
                            LiveOutcome::Update(build(
                                settings,
                                selected,
                                visible(settings)
                                    .get(selected)
                                    .map(|d| alias_for(settings, &d.name))
                                    .unwrap_or_default(),
                                feedback.as_deref(),
                            ))
                        }
                        LiveEvent::Click(8) => {
                            pins::launch("ms-settings:bluetooth".to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::TextCommit { id: 50, text } => {
                            if (1..=64).contains(&text.len())
                                && let Some(d) = visible(settings).get(selected)
                            {
                                let mut map = setting_value(settings, "WirelessDeviceAliases")
                                    .and_then(|v| v.as_object().cloned())
                                    .unwrap_or_default();
                                map.insert(d.name.clone(), serde_json::json!(text));
                                set_setting(
                                    settings,
                                    "WirelessDeviceAliases",
                                    serde_json::json!(map),
                                );
                                settings_changed = true;
                            }
                            LiveOutcome::Update(build(
                                settings,
                                selected,
                                text,
                                feedback.as_deref(),
                            ))
                        }
                        _ => LiveOutcome::Close,
                    },
                );
                // WS-1: no unconditional `true` — a hover-opened flyout must not
                // trigger settings save + live reload on the hook-owning UI thread.
                return Self::flyout_settings_changed("bluetoothbattery", settings_changed);
            }
            // `StorageWidget` expanded drives flyout (DriveType.Fixed && IsReady).
            "storage" => {
                let drives = self.poll_snapshot.fixed_drives();
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
                    blocks.push(FlyoutBlock::Note(loc::t("StorageNoFixedDrives")));
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
            // Exact `KeyboardLayoutWidget.BuildFlyoutContent` (`BadgeRow` with code pill + full name)
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
                    width_dips: 268.0,
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
                            caption: Some(loc::t_fmt("DayProgressRemaining", &[&left.to_string()])),
                            centered: false,
                            large: false,
                            right_meta: None,
                        },
                        FlyoutBlock::ProgressBar {
                            label: loc::t("DayProgressDay"),
                            value_text: format!("{:.0}%", day * 100.0),
                            ratio: day,
                            tone: BarTone::Accent,
                        },
                        FlyoutBlock::ProgressBar {
                            label: loc::t("DayProgressWeek"),
                            value_text: format!("{:.0}%", week * 100.0),
                            ratio: week,
                            tone: BarTone::Accent,
                        },
                        FlyoutBlock::ProgressBar {
                            label: loc::t("DayProgressYear"),
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
            // `FocusWidget.BuildFocusFlyout` — Break phase + session number + pulse.
            "focus" => {
                let build = || focus_break::build_focus_flyout(&self.focus.snapshot());
                let init = build();
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
                            self.focus.with_mut(|s| s.toggle());
                            let snap = self.focus.snapshot();
                            self.focus_running = snap.running;
                            self.focus_remaining_secs = snap.remaining_secs;
                            LiveOutcome::Update(build())
                        }
                        LiveEvent::Click(2) => {
                            self.focus.with_mut(|s| s.reset());
                            LiveOutcome::Update(build())
                        }
                        LiveEvent::Click(4) => {
                            let event = self.focus.with_mut(|s| {
                                s.skip_phase();
                                FocusEvent::PhaseEnded(s.phase)
                            });
                            if matches!(event, Some(FocusEvent::PhaseEnded(_))) {
                                play_break_chime();
                            }
                            LiveOutcome::Update(build())
                        }
                        LiveEvent::Click(mins @ (15 | 25 | 50)) => {
                            self.focus
                                .with_mut(|s| s.set_focus_minutes(u64::from(mins)));
                            LiveOutcome::Update(build())
                        }
                        _ => LiveOutcome::Update(focus_break::build_focus_flyout(
                            &self.focus.snapshot(),
                        )),
                    },
                );
            }
            // Exact `TimedWidgetPanel.xaml` Stopwatch (live Lap / Start / Pause / Reset)
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
                        _ => LiveOutcome::Update(self.live_spec("stopwatch", settings, is_dark)),
                    },
                );
            }
            // Exact `TimedWidgetPanel.xaml` Countdown (live 5/10/15/30 presets + Start/Pause/Reset)
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
                        // C2 TextCommit — custom duration with 1-1440 minute validation.
                        LiveEvent::TextCommit { id: 50, text } => {
                            if let Ok(mins) = text.trim().parse::<u64>()
                                && (1..=1440).contains(&mins)
                            {
                                self.countdown_total_secs = mins * 60;
                                self.countdown_remaining_secs = self.countdown_total_secs;
                                self.countdown_running = false;
                            }
                            LiveOutcome::Update(self.build_countdown_spec())
                        }
                        _ => LiveOutcome::Update(self.live_spec("countdown", settings, is_dark)),
                    },
                );
            }
            // `MediaWidget` live flyout: seek anchor + per-app mixer (WS-E).
            "media" => {
                telemetry::refresh_media_session(&self.media_session, true);
                let build = |runtime: &Self| {
                    let snap = runtime.media_snapshot();
                    let details =
                        media_ext::get_details(&snap, runtime.audio_vol, runtime.audio_muted);
                    let sessions = media_ext::list_app_sessions();
                    let mut spec = media_ext::build_media_flyout(&details, &sessions, settings);
                    spec.blocks.push(FlyoutBlock::Separator);
                    spec.blocks.push(FlyoutBlock::ButtonBar(vec![(
                        4,
                        loc::t_fmt("RadioSettings", &[&loc::t("LauncherSound")]),
                        false,
                    )]));
                    spec
                };
                let init = build(self);
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
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Click(2) => {
                            telemetry::send_media_vk(VK_MEDIA_PLAY_PAUSE);
                            telemetry::refresh_media_session(&self.media_session, true);
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Click(3) => {
                            telemetry::send_media_vk(VK_MEDIA_NEXT_TRACK);
                            telemetry::refresh_media_session(&self.media_session, true);
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Click(4) => {
                            pins::launch("ms-settings:sound".to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::Click(60) => {
                            toggle_audio_mute();
                            (self.audio_vol, self.audio_muted) = get_audio_state();
                            LiveOutcome::Update(build(self))
                        }
                        // C2 LiveEvent::Seek — C4 `telemetry::media_seek(secs)`.
                        LiveEvent::Seek { id: 50, ratio } => {
                            let dur = self
                                .media_session
                                .lock()
                                .ok()
                                .map_or(0, |g| g.duration_secs);
                            let target = media_ext::seek_position(ratio, dur);
                            if telemetry::media_seek(target) {
                                // Anchor locally at once; a stale SMTC echo of the old position
                                // must not rewind the slider until the provider catches up.
                                let now = self.now_secs();
                                confirm_seek(&mut self.media_timeline, target as f64, now);
                            }
                            telemetry::refresh_media_session(&self.media_session, true);
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Slider { id: 51, ratio } => {
                            set_audio_volume((ratio * 100.0).round() as u32);
                            (self.audio_vol, self.audio_muted) = get_audio_state();
                            LiveOutcome::Update(build(self))
                        }
                        // Per-app mixer rows (ids 120..=131) from `media_ext::list_app_sessions`.
                        LiveEvent::Click(id) if (120..132).contains(&id) && id % 2 == 0 => {
                            let sessions = media_ext::list_app_sessions();
                            if let Some(session) = sessions.get(((id - 120) / 2) as usize) {
                                media_ext::set_app_mute(&session.session_id, !session.muted);
                            }
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Slider { id, ratio }
                            if (121..132).contains(&id) && id % 2 == 1 =>
                        {
                            let sessions = media_ext::list_app_sessions();
                            if let Some(session) = sessions.get(((id - 121) / 2) as usize) {
                                media_ext::set_app_volume(
                                    &session.session_id,
                                    (ratio * 100.0).round() as u32,
                                );
                            }
                            LiveOutcome::Update(build(self))
                        }
                        _ => LiveOutcome::Close,
                    },
                );
            }
            "quicksettings" => {
                let mut settings_changed = false;
                let change_error = || {
                    if self.radio_failed {
                        Some(loc::t(if self.radio_denied {
                            "QuickPermissionDenied"
                        } else {
                            "QuickChangeFailed"
                        }))
                    } else {
                        None
                    }
                };
                // `ChangeResult`: Applied / Denied / Failed are distinct user-visible states.
                let wifi_state =
                    change_error().unwrap_or_else(|| radio_label(self.wifi_on, self.radio_loading));
                let bt_state = change_error()
                    .unwrap_or_else(|| radio_label(self.bluetooth_on, self.radio_loading));
                // Ours: surface the WinRT probe state while the radios are still being read.
                let loading_note = if self.radio_loading {
                    loc::t("QuickLoading")
                } else {
                    String::new()
                };
                let build_qs_spec = |settings: &DockSettings,
                                     night_light: bool,
                                     wifi_state: &str,
                                     bt_state: &str,
                                     wifi_on: bool,
                                     bt_on: bool| {
                    let (vol, muted) = get_audio_state();
                    let out_name = telemetry::default_audio_device_name(false)
                        .unwrap_or_else(|| loc::t("Speakers"));
                    let hide_label = match settings.auto_hide {
                        AutoHideMode::Disabled => loc::t("QuickOff"),
                        AutoHideMode::Smart => loc::t("AutoHideSmart"),
                        AutoHideMode::Always => loc::t("AutoHideAlways"),
                    };
                    let theme_label = match settings.theme {
                        AppTheme::System => loc::t("ThemeSystem"),
                        AppTheme::Dark => loc::t("Dark"),
                        AppTheme::Light => loc::t("Light"),
                    };
                    FlyoutSpec {
                        title: loc::t("WidgetQuickSettings"),
                        subtitle: (!loading_note.is_empty()).then(|| loading_note.clone()),
                        width_dips: 312.0,
                        blocks: vec![
                            FlyoutBlock::ToggleGrid(vec![
                                (5, loc::t("WiFi"), wifi_state.to_owned(), wifi_on),
                                (6, loc::t("Bluetooth"), bt_state.to_owned(), bt_on),
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
                                    loc::t("AutoHide"),
                                    hide_label.to_owned(),
                                    settings.auto_hide != AutoHideMode::Disabled,
                                ),
                                (
                                    3,
                                    loc::t("Theme"),
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
                                label: loc::t("Volume"),
                                device_name: out_name,
                                value_text: if muted {
                                    loc::t("AudioMuted")
                                } else {
                                    format!("{vol}%")
                                },
                                ratio: vol as f32 / 100.0,
                                muted,
                                mute_id: 4,
                                slider_id: 10,
                            },
                            FlyoutBlock::Separator,
                            // `QuickSettingsWidget.AddShortcuts`: 17 Control Center shortcuts.
                            FlyoutBlock::IconBar {
                                buttons: vec![
                                    (100, loc::t("QuickAvailableNetworks"), 0xE701, wifi_on),
                                    (101, loc::t("QuickBluetoothSettings"), 0xE702, bt_on),
                                    (102, loc::t("QuickNightLightSettings"), 0xE708, night_light),
                                    (103, loc::t("QuickAirplaneMode"), 0xF71D, true),
                                    (104, loc::t("QuickCellular"), 0xE839, true),
                                    (105, loc::t("QuickBrightness"), 0xE7F3, true),
                                    (106, loc::t("QuickRotationLock"), 0xE7F9, true),
                                    (107, loc::t("QuickEnergySaver"), 0xE7EF, true),
                                    (108, loc::t("QuickNearbySharing"), 0xE8D2, true),
                                    (109, loc::t("QuickLiveCaptions"), 0xF4B9, true),
                                    (110, loc::t("QuickNotificationMode"), 0xE7ED, true),
                                    (111, loc::t("QuickStudioEffects"), 0xE7F6, true),
                                    (112, loc::t("QuickHearingDevices"), 0xE7F5, true),
                                    (113, loc::t("QuickCast"), 0xE7F6, true),
                                    (114, loc::t("QuickMobileHotspot"), 0xE7B7, true),
                                    (115, loc::t("QuickAccessibility"), 0xE7F7, true),
                                    (116, loc::t("QuickOpenWindowsPanel"), 0xE7FC, true),
                                ],
                            },
                            FlyoutBlock::ButtonBar(vec![
                                (8, loc::t("Settings").to_owned(), true),
                                (9, loc::t("WidgetShowDesktop"), false),
                            ]),
                        ],
                    }
                };
                let init = build_qs_spec(
                    settings,
                    self.night_light_on,
                    &wifi_state,
                    &bt_state,
                    self.wifi_on,
                    self.bluetooth_on,
                );
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
                            LiveOutcome::Update(build_qs_spec(
                                settings,
                                self.night_light_on,
                                &wifi_state,
                                &bt_state,
                                self.wifi_on,
                                self.bluetooth_on,
                            ))
                        }
                        LiveEvent::Click(2) => {
                            settings.auto_hide = match settings.auto_hide {
                                AutoHideMode::Disabled => AutoHideMode::Smart,
                                AutoHideMode::Smart => AutoHideMode::Always,
                                AutoHideMode::Always => AutoHideMode::Disabled,
                            };
                            settings_changed = true;
                            LiveOutcome::Update(build_qs_spec(
                                settings,
                                self.night_light_on,
                                &wifi_state,
                                &bt_state,
                                self.wifi_on,
                                self.bluetooth_on,
                            ))
                        }
                        LiveEvent::Click(3) => {
                            settings.theme = match settings.theme {
                                AppTheme::System => AppTheme::Dark,
                                AppTheme::Dark => AppTheme::Light,
                                AppTheme::Light => AppTheme::System,
                            };
                            settings_changed = true;
                            LiveOutcome::Update(build_qs_spec(
                                settings,
                                self.night_light_on,
                                &wifi_state,
                                &bt_state,
                                self.wifi_on,
                                self.bluetooth_on,
                            ))
                        }
                        LiveEvent::Click(4) => {
                            toggle_audio_mute();
                            (self.audio_vol, self.audio_muted) = get_audio_state();
                            LiveOutcome::Update(build_qs_spec(
                                settings,
                                self.night_light_on,
                                &wifi_state,
                                &bt_state,
                                self.wifi_on,
                                self.bluetooth_on,
                            ))
                        }
                        LiveEvent::Slider { id: 10, ratio } => {
                            set_audio_volume((ratio * 100.0).round() as u32);
                            (self.audio_vol, self.audio_muted) = get_audio_state();
                            LiveOutcome::Update(build_qs_spec(
                                settings,
                                self.night_light_on,
                                &wifi_state,
                                &bt_state,
                                self.wifi_on,
                                self.bluetooth_on,
                            ))
                        }
                        LiveEvent::Click(5) => {
                            // C4 `platform::radio::set` — real WinRT Wi-Fi toggle.
                            let target = !self.wifi_on;
                            self.radio_failed = false;
                            self.radio_denied = false;
                            if crate::platform::radio::set("wifi", target) {
                                self.wifi_on = target;
                                self.wifi_target = Some(target);
                            } else {
                                self.radio_unavailable = true;
                                self.radio_denied = true;
                                self.radio_failed = true;
                                pins::launch("ms-settings:network-wifi".to_owned());
                            }
                            let cur_wifi = if self.radio_unavailable {
                                loc::t("QuickUnavailable")
                            } else {
                                radio_label(self.wifi_on, false)
                            };
                            LiveOutcome::Update(build_qs_spec(
                                settings,
                                self.night_light_on,
                                &cur_wifi,
                                &bt_state,
                                self.wifi_on,
                                self.bluetooth_on,
                            ))
                        }
                        LiveEvent::Click(6) => {
                            // C4 `platform::radio::set` — real WinRT Bluetooth toggle.
                            let target = !self.bluetooth_on;
                            self.radio_failed = false;
                            self.radio_denied = false;
                            if crate::platform::radio::set("bluetooth", target) {
                                self.bluetooth_on = target;
                                self.bluetooth_target = Some(target);
                            } else {
                                self.radio_unavailable = true;
                                self.radio_denied = true;
                                self.radio_failed = true;
                                pins::launch("ms-settings:bluetooth".to_owned());
                            }
                            let cur_bt = if self.radio_unavailable {
                                loc::t("QuickUnavailable")
                            } else {
                                radio_label(self.bluetooth_on, false)
                            };
                            LiveOutcome::Update(build_qs_spec(
                                settings,
                                self.night_light_on,
                                &wifi_state,
                                &cur_bt,
                                self.wifi_on,
                                self.bluetooth_on,
                            ))
                        }
                        LiveEvent::Click(id @ 100..=116) => {
                            let uri = shortcut_uri(id);
                            pins::launch(uri.to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::Click(7) => {
                            if telemetry::toggle_night_light() {
                                self.night_light_on = !self.night_light_on;
                            } else {
                                pins::launch("ms-settings:nightlight".to_owned());
                            }
                            LiveOutcome::Update(build_qs_spec(
                                settings,
                                self.night_light_on,
                                &wifi_state,
                                &bt_state,
                                self.wifi_on,
                                self.bluetooth_on,
                            ))
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
                return Self::flyout_settings_changed("quicksettings", settings_changed);
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
                        action_row(100 + i as u32, name, Some(loc::t("FolderLabel")))
                    })
                    .collect();
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note(loc::t("WidgetStacksHint")));
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
                            Some(loc::t_fmt(
                                "AppGroupAppsCount",
                                &[&g.executable_paths.len().to_string()],
                            )),
                        )
                    })
                    .collect();
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note(loc::t("WidgetGroupsHint")));
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
    fn all_22_widgets_registered_with_valid_categories() {
        assert_eq!(catalog().len(), 22);
        let mut seen = std::collections::HashSet::new();
        for w in catalog() {
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

    #[test]
    fn flyout_gate_covers_every_flyout_widget() {
        // WS-D hover dispatch and direct-click dispatch share this gate. It must stay in sync
        // with the branches in `open_widget_flyout`, including the WS-E flyouts.
        for enabled in [
            "clock",
            "weather",
            "stopwatch",
            "countdown",
            "focus",
            "audio",
            "quicksettings",
            "keyboard",
            "stacks",
            "groups",
            "stats",
            "netspeed",
            "bluetoothbattery",
            "storage",
            "dayprogress",
            "media",
            "aiusage_all",
            "aiusage_claude",
            "aiusage_codex",
            "aiusage_opencode",
            "aiusage_copilot",
            "aiusage_deepseek",
            "aiusage_openrouter",
            "aiusage_cursor",
        ] {
            assert!(widget_has_flyout(enabled), "{enabled} should have a flyout");
        }
        for disabled in ["battery", "unknown", "showdesktop"] {
            assert!(
                !widget_has_flyout(disabled),
                "{disabled} should not have a flyout"
            );
        }
    }

    #[test]
    fn calendar_cell_mapping_follows_culture_first_day() {
        // 2024-01-01 was a Monday (.NET weekday 1).
        // en-US: Sunday-first -> one leading blank.
        let en_us = 0;
        assert_eq!(telemetry::calendar_leading_blanks(en_us, 1), 1);
        assert_eq!(
            WidgetRuntime::calendar_cell_date(2024, 1, 0, en_us),
            (2023, 12, 31)
        );
        assert_eq!(
            WidgetRuntime::calendar_cell_date(2024, 1, 1, en_us),
            (2024, 1, 1)
        );
        // Arabic: Saturday-first -> two leading blanks.
        let ar_sa = 6;
        assert_eq!(telemetry::calendar_leading_blanks(ar_sa, 1), 2);
        assert_eq!(
            WidgetRuntime::calendar_cell_date(2024, 1, 0, ar_sa),
            (2023, 12, 30)
        );
        assert_eq!(
            WidgetRuntime::calendar_cell_date(2024, 1, 2, ar_sa),
            (2024, 1, 1)
        );
        // Monday-first (de-DE) keeps the historical no-blank mapping for Monday.
        assert_eq!(telemetry::calendar_leading_blanks(1, 1), 0);
        assert_eq!(
            WidgetRuntime::calendar_cell_date(2024, 1, 0, 1),
            (2024, 1, 1)
        );
    }

    #[test]
    fn calendar_cell_mapping_handles_sunday_first_months_and_month_edges() {
        // 2023-10-01 was a Sunday: no blanks in en-US, six in Monday-first.
        assert_eq!(telemetry::weekday_of(2023, 10, 1), 0);
        assert_eq!(telemetry::calendar_leading_blanks(0, 0), 0);
        assert_eq!(telemetry::calendar_leading_blanks(1, 0), 6);
        assert_eq!(
            WidgetRuntime::calendar_cell_date(2023, 10, 0, 0),
            (2023, 10, 1)
        );
        assert_eq!(
            WidgetRuntime::calendar_cell_date(2023, 10, 0, 1),
            (2023, 9, 25)
        );
        // Trailing cells still roll into the next month.
        assert_eq!(
            WidgetRuntime::calendar_cell_date(2024, 1, 32, 0),
            (2024, 2, 1)
        );
        assert!(telemetry::first_day_of_week() <= 6);
    }

    #[test]
    fn hover_does_not_request_settings_reload() {
        // WS-1 regression: opening a flyout on hover (no live user event) must
        // never report "settings changed" to the dock, even for the
        // bluetoothbattery widget that used to return `true` unconditionally.
        assert!(!WidgetRuntime::flyout_settings_changed(
            "bluetoothbattery",
            false
        ));
        assert!(!WidgetRuntime::flyout_settings_changed(
            "quicksettings",
            false
        ));
        assert!(!WidgetRuntime::flyout_settings_changed("stats", false));
        // Real click/commit paths keep the previous save + live-reload behaviour.
        assert!(WidgetRuntime::flyout_settings_changed(
            "bluetoothbattery",
            true
        ));
        assert!(WidgetRuntime::flyout_settings_changed(
            "quicksettings",
            true
        ));
        // Widgets without a click-driven settings mutation never request a reload.
        assert!(!WidgetRuntime::flyout_settings_changed("stats", true));
    }

    #[test]
    fn widget_poll_snapshot_has_one_second_ttl() {
        assert_eq!(POLL_TTL, std::time::Duration::from_secs(1));
        let now = Instant::now();
        let mut snap = WidgetPollSnapshot::with_samples(Vec::new(), "EN".to_owned(), now);
        assert!(snap.is_fresh(now + std::time::Duration::from_millis(900)));
        assert!(!snap.is_fresh(now + POLL_TTL));
        // A subsequent refresh produces an empty drive list without touching the cache timestamp.
        snap.drives.clear();
        assert!(snap.keyboard_code == "EN");
    }

    #[test]
    fn animation_tick_does_not_change_poll_snapshot() {
        let mut runtime = WidgetRuntime::default();
        let settings = DockSettings::default();
        let before = runtime.poll_snapshot.sampled_at();
        runtime.poll_snapshot.refresh();
        let after = runtime.poll_snapshot.sampled_at();
        assert!(!runtime.animation_tick(&settings));
        assert_eq!(runtime.poll_snapshot.sampled_at(), after);
        assert_ne!(before, after);
    }

    #[test]
    fn media_runtime_exposes_timeline_position_without_rebuilding_state() {
        let mut runtime = WidgetRuntime::default();
        let snapshot = telemetry::MediaSessionSnapshot {
            title: "Song".into(),
            artist: "Artist".into(),
            playing: true,
            position_secs: 10,
            duration_secs: 100,
            ..Default::default()
        };
        {
            let mut guard = runtime.media_session.lock().unwrap();
            *guard = snapshot;
        }
        let sample = media_ext::timeline_from_snapshot(
            &runtime.media_session.lock().unwrap(),
            runtime.timeline_epoch,
        );
        runtime
            .media_timeline
            .update(sample, 1.0, runtime.now_secs(), runtime.now_secs());
        let shown = runtime.media_snapshot().position_secs;
        assert!(
            shown >= 10,
            "extrapolated position must not rewind the SMTC sample"
        );
    }

    #[test]
    fn bluetooth_threshold_cycles_zero_ten_twenty_thirty() {
        assert_eq!(bt_next_threshold(20), 30);
        assert_eq!(bt_next_threshold(30), 0);
        assert_eq!(bt_next_threshold(0), 10);
        assert_eq!(bt_next_threshold(-1), 0);
    }

    #[test]
    fn gpu_thermal_hysteresis_uses_five_entries_and_two_exits() {
        let mut runtime = WidgetRuntime::default();
        assert_eq!(thermal_band(69), 0);
        assert_eq!(thermal_band(70), 1);
        assert_eq!(thermal_band(85), 2);
        for _ in 0..4 {
            assert_eq!(runtime.update_gpu_thermal(Some(72)), 0);
        }
        assert_eq!(runtime.update_gpu_thermal(Some(72)), 1);
        assert_eq!(runtime.update_gpu_thermal(Some(64)), 1);
        assert_eq!(runtime.update_gpu_thermal(Some(64)), 0);
    }

    #[test]
    fn live_spec_rebuilds_clock_and_stats_from_current_state() {
        let mut runtime = WidgetRuntime::default();
        let settings = DockSettings::default();
        let clock = runtime.live_spec("clock", &settings, true);
        assert!(
            clock
                .blocks
                .iter()
                .any(|b| matches!(b, FlyoutBlock::Hero { .. }))
        );
        assert!(
            clock
                .blocks
                .iter()
                .any(|b| matches!(b, FlyoutBlock::CalendarGrid { .. }))
        );
        let stats = runtime.live_spec("stats", &settings, true);
        assert!(
            stats
                .blocks
                .iter()
                .any(|b| matches!(b, FlyoutBlock::Chart { .. }))
        );
        let stopwatch = runtime.live_spec("stopwatch", &settings, true);
        assert!(!stopwatch.blocks.is_empty());
        let countdown = runtime.live_spec("countdown", &settings, true);
        assert!(!countdown.blocks.is_empty());
    }

    #[test]
    fn quick_settings_registers_seventeen_distinct_shortcuts() {
        assert_eq!(QUICK_SETTINGS_SHORTCUT_COUNT, 17);
        let mut uris = std::collections::HashSet::new();
        for id in 100..=116 {
            let uri = shortcut_uri(id);
            assert_ne!(uri, "ms-settings:", "shortcut {id} must have a URI");
            assert!(uris.insert(uri), "duplicate shortcut URI for {id}");
        }
    }

    #[test]
    fn catalog_and_ai_flyout_use_the_seven_remaining_providers() {
        assert!(
            !catalog()
                .iter()
                .any(|w| w.id.eq_ignore_ascii_case("aiusage_ag"))
        );
        assert_eq!(
            catalog()
                .iter()
                .filter(|w| w.category == "AI Quotas")
                .count(),
            7
        );
        assert!(canonical_widget_id("btbattery") == "bluetoothbattery");
    }

    #[test]
    fn cached_ai_full_detail_is_rendered_instead_of_default() {
        let runtime = WidgetRuntime::default();
        let full = telemetry::AiFullDetail {
            provider_id: "aiusage_claude".to_owned(),
            plan: Some("Pro".to_owned()),
            used: 0.5,
            ..Default::default()
        };
        runtime
            .ai_full
            .lock()
            .unwrap()
            .insert("aiusage_claude".to_owned(), (Instant::now(), full));
        let snap = telemetry::AiUsageSnapshot::default();
        let detail = runtime.ai_flyout_detail("aiusage_claude", &snap);
        assert_eq!(detail.plan.as_deref(), Some("Pro"));
        assert!(runtime.ai_full_detail("aiusage_claude").is_some());
        assert!(runtime.ai_full_detail("aiusage_codex").is_none());
    }
}
