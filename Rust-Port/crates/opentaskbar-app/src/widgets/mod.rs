//! Unified 23-widget registry, runtime state, and C#-parity `DockFlyout` builders.
//!
//! Categories (matching C# `DockWidgetService.cs`):
//! - `Time & Weather`    (5): `clock`, `weather`, `dayprogress`, `stopwatch`, `countdown`
//! - `System & Hardware` (5): `stats`, `netspeed`, `battery`, `bluetoothbattery`, `storage`
//! - `AI Quotas`         (8): `aiusage_claude`, `aiusage_codex`, `aiusage_antigravity`,
//!   `aiusage_opencode`, `aiusage_copilot`, `aiusage_deepseek`,
//!   `aiusage_openrouter`, `aiusage_cursor`
//! - `Media & Controls`  (4): `media`, `audio`, `quicksettings`, `keyboard`
//! - `Productivity`      (1): `focus`

// Many items here are public API consumed by WS-D (dock) / WS-E (UI) after merge; a standalone
// binary build sees them as unused. Keep the worktree warning-clean without changing semantics.
#![allow(dead_code)]

pub mod ai_flyout;
pub mod catalog_ops;
pub mod focus_break;
pub mod hover;
pub mod media_ext;
pub mod speedtest;
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
use crate::ui::flyout::{self, BarTone, FlyoutBlock, FlyoutSpec, LiveEvent, LiveOutcome};

#[allow(unused_imports)]
pub use catalog_ops::{
    canonical_widget_id, catalog, hardware_available, set_setting, setting_bool, setting_f64,
    setting_i64, setting_str, setting_string_list, setting_value, widget_layout, WidgetDensity,
    WidgetLayout, WidgetOption, ShrinkPriority, WIDGET_ALIASES,
};
#[allow(unused_imports)]
pub use focus_break::{
    play_break_chime, FocusEvent, FocusPhase, FocusSession, SharedFocusSession,
};
#[allow(unused_imports)]
pub use hover::{
    HoverChange, HoverPhase, WidgetHoverCoordinator, HOVER_INTENT_MS, SUPPRESS_MS,
};
#[allow(unused_imports)]
pub use media_ext::{
    AudioAppSession, MediaDetails, format_time as format_media_time, get_details as get_media_details,
    playing_glyph, seek_ratio, volume_from_wheel,
};
#[allow(unused_imports)]
pub use speedtest::{
    SpeedTestController, SpeedTestResult, SpeedTestState, STREAM_COUNT,
};
pub use telemetry::{
    active_keyboard_code, adjust_audio_volume, day_progress_metrics, empty_recycle_bin,
    get_audio_state, is_recycle_bin_non_empty, sample_battery, sample_drives, sample_ram_info,
    set_audio_volume, toggle_audio_mute,
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
    wd("aiusage_antigravity", "Antigravity", "AI Quotas", 262.0),
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
        "aiusage_antigravity" => (0x38, 0xbd, 0xf8),
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
    /// GPU utilization percent (C# `SystemStatsWidget` GPU meter).
    gpu_pct: u32,
    /// GPU die temperature in °C (C# GPU temperature meter).
    gpu_temp_c: Option<u32>,
    /// Consecutive failed GPU probes; C# hides the GPU meters after 5 misses.
    gpu_misses: u32,
    /// Consecutive successful GPU probes; C# requires 2 samples before showing meters.
    gpu_samples: u32,
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
    ai_usage: Arc<Mutex<telemetry::AiUsageCache>>,
    media_session: Arc<Mutex<telemetry::MediaSessionSnapshot>>,
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
    battery_anim_start: Instant,
    battery_charging: bool,
    last_tick: Instant,
}

impl Default for WidgetRuntime {
    fn default() -> Self {
        Self {
            stats: telemetry::SystemStatsSampler::default(),
            net: telemetry::NetSpeedSampler::default(),
            cpu_pct: 0,
            ram_pct: 0,
            gpu_pct: 0,
            gpu_temp_c: None,
            gpu_misses: 0,
            gpu_samples: 0,
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
            ai_usage: Arc::new(Mutex::new(telemetry::AiUsageCache::default())),
            media_session: Arc::new(Mutex::new(telemetry::MediaSessionSnapshot::default())),
            speedtest: SpeedTestController::new(),
            speedtest_started_at: None,
            hover: WidgetHoverCoordinator::new(),
            night_light_on: telemetry::read_night_light().unwrap_or(false),
            wifi_on: false,
            bluetooth_on: false,
            radio_loading: true,
            radio_unavailable: false,
            battery_anim_start: Instant::now(),
            battery_charging: false,
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

/// C# `StopwatchWidget` centisecond display: `mm:ss.cc`.
fn format_mm_ss_cs(total_secs: u64, centis: u32) -> String {
    format!("{:02}:{:02}.{:02}", total_secs / 60, total_secs % 60, centis % 100)
}

/// GPU availability hysteresis (C# `SystemStatsWidget`): 5 consecutive misses hide the meters
/// and 2 consecutive samples bring them back. Returns `None` while the meters must stay hidden.
fn gpu_metrics_visible(samples: u32, misses: u32) -> bool {
    misses < 5 && samples >= 2
}

/// C# `StorageWidget`: `DriveType.Fixed && IsReady` filter (excludes CD/DVD, USB and network
/// drives). `GetDiskFreeSpaceExW` already implies readiness, so only the drive-type check is
/// needed on top of the telemetry sampler.
fn fixed_ready_drives() -> Vec<telemetry::DriveSpace> {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_FIXED: u32 = 3;
    sample_drives()
        .into_iter()
        .filter(|d| {
            let root: Vec<u16> = format!("{}:\\\0", d.letter).encode_utf16().collect();
            unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_FIXED }
        })
        .collect()
}

/// C# `QuickSettingsWidget` radio state labels: `Loading`, `Unavailable`, `On`, `Off`.
fn radio_label(on: bool, loading: bool) -> &'static str {
    if loading {
        "Loading"
    } else {
        if on { "On" } else { "Off" }
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

/// C# `WeatherUnit` conversion: telemetry caches Celsius, but the widget can show Fahrenheit.
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

fn weather_icon_glyph(condition: &str) -> u16 {
    let c = condition.to_ascii_lowercase();
    if c.contains("thunder") {
        0xFD3E // Thunderstorm
    } else if c.contains("snow") {
        0xF7D2 // Snow
    } else if c.contains("rain") || c.contains("showers") || c.contains("drizzle") {
        0xF743 // Rain
    } else if c.contains("fog") {
        0xF75E // Fog
    } else if c.contains("cloud") || c.contains("overcast") {
        0xF83D // Cloudy
    } else {
        0xF7B5 // Sunny
    }
}

/// C4 `platform::core_audio::list_devices(false)` adapter. Assumed shape is `(id, display_name)`;
/// keeping the conversion in one place makes a signature change a one-line fix at merge time.
fn audio_output_devices() -> Vec<(String, String)> {
    crate::platform::core_audio::list_devices(false)
}

/// Maps a Quick Settings `IconBar` shortcut ID to its C# `ms-settings:` URI.
fn shortcut_uri(id: u32) -> &'static str {
    match id {
        100 => "ms-settings:network-wifi",
        101 => "ms-settings:bluetooth",
        102 => "ms-settings:network-airplanemode",
        103 => "ms-settings:nightlight",
        104 => "ms-settings:batterysaver",
        105 => "ms-settings:quietmomentshome",
        106 => "ms-settings:display",
        107 => "ms-settings:sound",
        108 => "ms-settings:network-status",
        109 => "ms-settings:network-vpn",
        110 => "ms-settings:nearby",
        111 => "ms-settings:display-advanced",
        112 => "ms-settings:tabletmode",
        113 => "ms-settings:network-mobilehotspot",
        114 => "ms-settings:dateandtime",
        115 => "ms-settings:taskbar",
        _ => "ms-settings:",
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

fn weather_icon_glyph_text(condition: &str) -> &'static str {
    let c = condition.to_ascii_lowercase();
    if c.contains("thunder") {
        "⚡"
    } else if c.contains("snow") {
        "❄"
    } else if c.contains("rain") || c.contains("showers") || c.contains("drizzle") {
        "🌧"
    } else if c.contains("fog") {
        "🌫"
    } else if c.contains("cloud") || c.contains("overcast") {
        "☁"
    } else {
        "☀"
    }
}

impl WidgetRuntime {
    pub fn tick(&mut self, settings: &DockSettings) {
        let now = Instant::now();
        let dt_ms = now.duration_since(self.last_tick).as_millis() as u64;
        let dt = (dt_ms / 1000).min(60);
        if dt_ms > 0 {
            self.last_tick = now;
            if self.stopwatch_running {
                self.stopwatch_secs = self.stopwatch_secs.saturating_add(dt);
                self.stopwatch_cs = self.stopwatch_cs.saturating_add((dt_ms % 1000) as u32 % 100);
            }
            if self.countdown_running {
                self.countdown_remaining_secs = self.countdown_remaining_secs.saturating_sub(dt);
                if self.countdown_remaining_secs == 0 {
                    self.countdown_running = false;
                }
            }
            // Shared focus/break session: crossing a phase boundary plays the Asterisk chime.
            if settings.is_widget_enabled("focus") {
                let event = self.focus.with_mut(|s| s.tick(dt)).unwrap_or(FocusEvent::None);
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
        if settings.is_widget_enabled("stats") {
            (self.cpu_pct, self.ram_pct) = self.stats.sample();
            // C4: WS-G provides `sample_gpu_temp()`; 5 misses / 2 samples hysteresis.
            if let Some((pct, temp)) = telemetry::sample_gpu_temp() {
                self.gpu_pct = pct;
                self.gpu_temp_c = temp;
                self.gpu_samples = self.gpu_samples.saturating_add(1);
                self.gpu_misses = 0;
            } else {
                self.gpu_misses = self.gpu_misses.saturating_add(1);
                if self.gpu_misses >= 5 {
                    self.gpu_samples = 0;
                    self.gpu_temp_c = None;
                    self.gpu_pct = 0;
                }
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
            if self.speedtest.poll(now, self.speedtest_started_at) {
                self.speedtest_started_at = None;
            }
        }
        if settings.is_widget_enabled("weather") {
            let can_fetch = self
                .weather_retry_at
                .is_none_or(|at| now >= at);
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
            // Poll interval / ignored devices / listen-only are applied in the flyout builder via
            // `catalog_ops::setting_*`; WS-G owns the low-level provider refresh cadence.
            telemetry::refresh_bluetooth_battery(&self.bt_battery, false);
        }
        if settings.is_widget_enabled("quicksettings") {
            self.night_light_on = telemetry::read_night_light().unwrap_or(self.night_light_on);
            // C4: platform::radio::{state,set} — WinRT Radio control.
            self.wifi_on = crate::platform::radio::state("wifi");
            self.bluetooth_on = crate::platform::radio::state("bluetooth");
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
            }
        }
    }

    /// Returns `true` when any dock widget needs a 15 FPS animation tick (e.g. C# Battery charging wave).
    pub fn needs_animation_tick(&self, settings: &DockSettings) -> bool {
        settings.animations_enabled
            && settings.is_widget_enabled("battery")
            && self.battery_charging
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

    /// C# `AudioWidget` wheel handler: 120 delta = 10% with fractional remainder carried across
    /// events. WS-D calls this from the dock's `WM_MOUSEWHEEL`.
    pub fn adjust_volume_wheel(&mut self, delta: i32) -> u32 {
        let next = volume_from_wheel(self.audio_vol, delta, &mut self.volume_wheel_remainder);
        set_audio_volume(next);
        (self.audio_vol, self.audio_muted) = get_audio_state();
        self.audio_vol
    }

    /// C# `KeyboardLayoutWidget`: send `WM_INPUTLANGCHANGEREQUEST` to the last *external*
    /// foreground window (never our own dock/island/flyout windows).
    pub fn activate_keyboard_on_last_external_window(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId, PostMessageW, WM_INPUTLANGCHANGEREQUEST,
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
            PostMessageW(hwnd, WM_INPUTLANGCHANGEREQUEST, 0, hkl);
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

    /// Exposes the hover coordinator's public API (WS-D already reaches it via the pub field).
    pub fn hover(&self) -> &WidgetHoverCoordinator {
        &self.hover
    }

    /// Builds the dynamic-island expanded details payload for the media widget.
    pub fn island_media_details(&self) -> MediaDetails {
        let snap = self
            .media_session
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default();
        media_ext::get_details(&snap, self.audio_vol, self.audio_muted)
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
                "aiusage_antigravity" => {
                    // C# AiUsageWidget.cs: Gemini (5h,7d) | Claude (7d) | Argon (7d), with live
                    // pool values from the C4 full fetch and configurable `AntigravityGroups`.
                    let groups = ai_flyout::antigravity_groups(settings);
                    let snap = self
                        .ai_usage
                        .lock()
                        .ok()
                        .and_then(|g| g.providers.get(ai.id).cloned())
                        .unwrap_or_default();
                    let colors = [pal.widget_accent, pal.widget_blue, pal.success];
                    let columns = groups
                        .into_iter()
                        .enumerate()
                        .take(3)
                        .map(|(i, name)| {
                            let color = colors[i % colors.len()];
                            let reset = snap
                                .windows
                                .first()
                                .map_or_else(|| "—".to_owned(), |w| w.reset_text.clone());
                            let used = snap.windows.first().map_or(0.0, |w| w.used);
                            let rows = if name.eq_ignore_ascii_case("Gemini") {
                                vec![
                                    ("5h".to_owned(), used, color, reset.clone()),
                                    ("7d".to_owned(), used, color, reset),
                                ]
                            } else {
                                vec![("7d".to_owned(), used, color, reset)]
                            };
                            (name, rows)
                        })
                        .collect();
                    WidgetChipSpec::AiAntigravity { columns }
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
                            meta_text: if snap.has_cred { "Monthly" } else { "Sign in" }.to_owned(),
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
                WidgetChipSpec::Clock {
                    time: info.time_hm,
                    date: info.short_date,
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
                    "Weather unavailable".to_owned()
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
                    "Break".to_owned()
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
                let load_fg = |pct: u32| -> Rgb {
                    if pct >= 90 {
                        pal.danger
                    } else if pct >= 75 {
                        pal.warning
                    } else {
                        pal.text_primary
                    }
                };
                // C# SystemStatsWidget: GPU meter appears only after 2 samples and hides after
                // 5 consecutive misses; its value is appended to the RAM column when available.
                let gpu_suffix = if gpu_metrics_visible(self.gpu_samples, self.gpu_misses) {
                    format!(" · GPU {}%", self.gpu_pct)
                } else {
                    String::new()
                };
                WidgetChipSpec::SystemStats {
                    cpu_text: format!("{}%", self.cpu_pct),
                    cpu_color: load_fg(self.cpu_pct),
                    ram_text: format!("{}%{}", self.ram_pct, gpu_suffix),
                    ram_color: load_fg(self.ram_pct),
                }
            }
            "netspeed" => {
                let down = if self.speedtest.state() == SpeedTestState::Downloading {
                    speedtest::format_throughput(self.net.rx_bps)
                } else {
                    telemetry::format_speed(self.net.rx_bps)
                };
                WidgetChipSpec::NetSpeed {
                    down,
                    up: telemetry::format_speed(self.net.tx_bps),
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
                let low = setting_i64(settings, "WirelessBatteryLowThreshold", 20) as u32;
                let devices: Vec<(u32, bool, bool)> = self
                    .bt_battery
                    .lock()
                    .ok()
                    .map(|g| {
                        g.devices
                            .iter()
                            .filter(|d| !ignored.iter().any(|n| n.eq_ignore_ascii_case(&d.name)))
                            .take(3)
                            .map(|d| {
                                // Tuple bools: low_warning | "full/charging bolt" state.
                                (d.percent, d.percent <= low, d.percent >= 95)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                WidgetChipSpec::Bluetooth { devices }
            }
            "storage" => {
                let compact = setting_bool(settings, "StorageCompactMode", false);
                let drives: Vec<(String, String)> = fixed_ready_drives()
                    .into_iter()
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
                let snap = self
                    .media_session
                    .lock()
                    .ok()
                    .map(|g| g.clone())
                    .unwrap_or_default();
                let title = if snap.title.is_empty() {
                    "Not playing".to_owned()
                } else {
                    // C# `MediaWidget` shows a play/pause glyph before the title.
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
                // C# QuickSettingsWidget.xaml: Wi-Fi, Bluetooth, Night Light — real radio states.
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
                code: active_keyboard_code(),
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
    pub fn hover_tooltip(&self, id: &str, sub: Option<u8>, settings: &DockSettings) -> String {
        if let Some(ai) = telemetry::ai_provider_meta(id) {
            let status = if telemetry::ai_provider_detected(ai.id) {
                "Ready"
            } else {
                "Sign in"
            };
            return format!("{} · {status}", ai.name);
        }
        match id {
            "clock" => telemetry::local_clock_info().full_date,
            "media" => match sub {
                Some(0) => "Previous".to_owned(),
                Some(1) => "Play / Pause".to_owned(),
                Some(2) => "Next".to_owned(),
                Some(3) => if self.audio_muted { "Unmute" } else { "Mute" }.to_owned(),
                _ => "Media".to_owned(),
            },
            "quicksettings" => match sub {
                Some(0) => format!("Wi-Fi · {}", radio_label(self.wifi_on, self.radio_loading)),
                Some(1) => format!(
                    "Bluetooth · {}",
                    radio_label(self.bluetooth_on, self.radio_loading)
                ),
                Some(2) => if self.night_light_on {
                    "Night Light · On"
                } else {
                    "Night Light · Off"
                }
                .to_owned(),
                _ => "Quick Settings".to_owned(),
            },
            "audio" => {
                let dev = telemetry::default_audio_device_name(false)
                    .unwrap_or_else(|| "Audio Output".to_owned());
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
                let gpu = if gpu_metrics_visible(self.gpu_samples, self.gpu_misses) {
                    match self.gpu_temp_c {
                        Some(t) => format!(" · GPU {}% {t}°C", self.gpu_pct),
                        None => format!(" · GPU {}%", self.gpu_pct),
                    }
                } else {
                    String::new()
                };
                format!("CPU {}% · RAM {}%{}", self.cpu_pct, self.ram_pct, gpu)
            }
            "dayprogress" => {
                let (day, _, _, left) = day_progress_metrics();
                format!("Day Progress: {:.0}% ({left}h left)", day * 100.0)
            }
            "stopwatch" => {
                if sub == Some(1) {
                    if self.stopwatch_running {
                        "Pause"
                    } else {
                        "Start"
                    }
                    .to_owned()
                } else {
                    format!("Stopwatch · {}", format_mm_ss(self.stopwatch_secs))
                }
            }
            "countdown" => {
                if sub == Some(1) {
                    if self.countdown_running {
                        "Pause"
                    } else {
                        "Start"
                    }
                    .to_owned()
                } else {
                    format!(
                        "Countdown · {}",
                        format_mm_ss(self.countdown_remaining_secs)
                    )
                }
            }
            "focus" => {
                if sub == Some(1) {
                    if self.focus_running { "Pause" } else { "Start" }.to_owned()
                } else {
                    format!("Focus · {}", format_mm_ss(self.focus_remaining_secs))
                }
            }
            "keyboard" => format!("Keyboard Layout · {}", active_keyboard_code()),
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

    /// Exact C# left-click dispatch: inline sub-buttons execute directly; clicking cards opens C# flyouts.
    pub fn handle_direct_click(
        &mut self,
        id: &str,
        rel_x_dips: f32,
        width_dips: f32,
        _settings: &mut DockSettings,
    ) -> DirectClickOutcome {
        if telemetry::ai_provider_meta(id).is_some() {
            return DirectClickOutcome::OpenFlyout;
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
                // C# QuickSettingsWidget.xaml: Wi-Fi, Bluetooth, Night Light — real radio toggles.
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
                // Exact C# BatteryWidget.OpenPowerSettingsAction
                pins::launch("ms-settings:batterysaver".to_owned());
                DirectClickOutcome::Handled
            }
            // Exact C# flyout triggers:
            "aiusage_all" | "clock" | "audio" | "stats" | "weather" | "netspeed"
            | "bluetoothbattery" | "storage" | "keyboard" | "dayprogress" | "stacks" | "groups" => {
                DirectClickOutcome::OpenFlyout
            }
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
                    (10, "Go to Today".to_owned(), false),
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
            .unwrap_or_else(|| "No microphone".to_owned());

        let devices = audio_output_devices();
        let mut device_blocks: Vec<FlyoutBlock> = devices
            .iter()
            .enumerate()
            .take(6)
            .map(|(i, (_, name))| FlyoutBlock::BadgeRow {
                id: 100 + i as u32,
                code: "\u{E7F5}".to_owned(), // Headphone glyph (C# AudioWidget device selector)
                title: name.clone(),
                active: name.eq_ignore_ascii_case(&out_name),
            })
            .collect();
        if !device_blocks.is_empty() {
            device_blocks.insert(
                0,
                FlyoutBlock::StatRow {
                    label: "Output device".to_owned(),
                    value: String::new(),
                },
            );
            device_blocks.push(FlyoutBlock::Separator);
        }
        FlyoutSpec {
            title: "Audio".to_owned(),
            subtitle: None,
            width_dips: 314.0,
            blocks: vec![
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
            ]
            .into_iter()
            .chain(device_blocks)
            .chain(std::iter::once(FlyoutBlock::ButtonBar(vec![(
                5,
                "Sound Settings".to_owned(),
                false,
            )])))
            .collect(),
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
                    (2, "Reset".to_owned(), false),
                    (
                        1,
                        if self.focus_running { "Pause" } else { "Start" }.to_owned(),
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
                caption: Some("Quick action: click the tile play/pause button".to_owned()),
                centered: true,
                large: true,
                right_meta: None,
            },
            FlyoutBlock::ButtonBar(vec![(3, "Lap".to_owned(), false)]),
        ];
        if self.stopwatch_laps.is_empty() {
            blocks.push(FlyoutBlock::Note("No laps recorded yet".to_owned()));
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
                    "Pause"
                } else {
                    "Start"
                }
                .to_owned(),
                true,
            ),
            (2, "Reset".to_owned(), false),
        ]));
        FlyoutSpec {
            title: "Stopwatch".to_owned(),
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
            title: "Countdown".to_owned(),
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
                // C# `TimedWidgetPanel` custom duration (C2 TextInput, validated 1-1440 minutes).
                FlyoutBlock::TextInput {
                    id: 50,
                    label: "Custom minutes (1-1440)".to_owned(),
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
                            "Configured".to_owned()
                        } else {
                            "Sign in".to_owned()
                        },
                    });
                }
            }
            blocks.push(FlyoutBlock::Separator);
            blocks.push(FlyoutBlock::ButtonBar(vec![
                (1, "Refresh All".to_owned(), true),
                (2, "Full Refresh (all providers)".to_owned(), false),
            ]));
            let spec = FlyoutSpec {
                title: "AI Quotas".to_owned(),
                subtitle: Some("Live usage across all configured providers".to_owned()),
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
                        ai_flyout::refresh_full(ai.id);
                    }
                }
                _ => {}
            }
        }
        // Full C# `AiUsageWidget.BuildFlyout` — spend / breakdown / reset / plan chip /
        // severity dot / refresh. Antigravity (8th provider) gets group/pool rows.
        else if let Some(ai) = telemetry::ai_provider_meta(id) {
            let snap = self
                .ai_usage
                .lock()
                .ok()
                .and_then(|g| g.providers.get(ai.id).cloned())
                .unwrap_or_default();
            let has_data = snap.balance_text.is_some() || !snap.windows.is_empty();
            let peak = snap.windows.iter().map(|w| w.used).fold(0.0f32, f32::max);
            let detail = ai_flyout::AiFullDetail {
                severity: ai_flyout::severity_for_usage(peak, false),
                ..Default::default()
            };
            let spec = ai_flyout::build_ai_flyout(ai.id, &snap, &detail, settings, is_dark);
            match flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) {
                Some(1) => pins::launch(ai.dashboard_url.to_owned()),
                Some(2) => telemetry::refresh_ai_provider(&self.ai_usage, ai.id, true),
                Some(3) => ai_flyout::refresh_full(ai.id),
                _ => {}
            }
            let _ = (has_data, &pal);
        }

        match id {
            // Exact C# `ClockWidget.BuildCalendar` (live interactive month/year nav & day selection)
            "clock" => {
                let info = telemetry::local_clock_info();
                let (mut shown_y, mut shown_m) = (info.year, info.month);
                let mut selected: Option<(i32, u32, u32)> = None;
                let init = Self::build_calendar_spec(shown_y, shown_m, selected);
                flyout::show_flyout_live(
                    "clock",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
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
                flyout::show_flyout_live(
                    "audio",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
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
                        // C4 `platform::core_audio::set_default_device` — output selector.
                        LiveEvent::Click(id @ 100..=105) => {
                            if let Some((device_id, _)) =
                                audio_output_devices().get((id - 100) as usize)
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
            // Exact C# `SystemStatsWidget.BuildTelemetryFlyout` (sparklines + top 5 processes!)
            "stats" => {
                let (ram_pct, used_gb, total_gb) = sample_ram_info();
                let cpu_col = if self.cpu_pct >= 90 {
                    pal.danger
                } else if self.cpu_pct >= 75 {
                    pal.warning
                } else {
                    pal.widget_blue
                };
                let ram_col = if ram_pct >= 90 {
                    pal.danger
                } else if ram_pct >= 75 {
                    pal.warning
                } else {
                    pal.widget_accent
                };
                let mut blocks = vec![
                    FlyoutBlock::StatRow {
                        label: "System Stats".to_owned(),
                        value: telemetry::system_uptime_string(),
                    },
                    // C2 `Chart` blocks with named series (CPU / RAM / GPU).
                    FlyoutBlock::Chart {
                        label: "CPU".to_owned(),
                        series: vec![("CPU".to_owned(), self.stats.cpu_history.as_vec())],
                        accent: cpu_col,
                    },
                    FlyoutBlock::Chart {
                        label: format!("Memory · {used_gb:.1} / {total_gb:.0} GB"),
                        series: vec![("RAM".to_owned(), self.stats.ram_history.as_vec())],
                        accent: ram_col,
                    },
                    // Compact C# legacy sparkline retained for the small card preview.
                    FlyoutBlock::Sparkline {
                        label: "CPU trend".to_owned(),
                        value_text: format!("{}%", self.cpu_pct),
                        detail_text: None,
                        samples: self.stats.cpu_history.as_vec(),
                        color: cpu_col,
                    },
                ];
                // C# GPU meters behind the 5-miss / 2-sample hysteresis.
                if gpu_metrics_visible(self.gpu_samples, self.gpu_misses) {
                    blocks.push(FlyoutBlock::Chart {
                        label: "GPU".to_owned(),
                        series: vec![("GPU".to_owned(), vec![self.gpu_pct as f32 / 100.0; 45])],
                        accent: pal.widget_blue,
                    });
                    blocks.push(FlyoutBlock::ProgressBar {
                        label: "GPU Load".to_owned(),
                        value_text: format!("{}%", self.gpu_pct),
                        ratio: self.gpu_pct as f32 / 100.0,
                        tone: BarTone::Usage,
                    });
                    if let Some(temp) = self.gpu_temp_c {
                        blocks.push(FlyoutBlock::ProgressBar {
                            label: "GPU Temperature".to_owned(),
                            value_text: format!("{temp}°C"),
                            ratio: (temp as f32 / 100.0).clamp(0.0, 1.0),
                            tone: if temp >= 85 {
                                BarTone::Usage
                            } else {
                                BarTone::Level
                            },
                        });
                    }
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::StatRow {
                    label: "Top Processes".to_owned(),
                    value: "Memory".to_owned(),
                });
                let procs = telemetry::sample_top_processes(5);
                if procs.is_empty() {
                    blocks.push(FlyoutBlock::Note(
                        "No process telemetry available".to_owned(),
                    ));
                } else {
                    for (name, bytes) in procs {
                        blocks.push(FlyoutBlock::StatRow {
                            label: name,
                            value: telemetry::format_bytes(bytes),
                        });
                    }
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::ButtonBar(vec![
                    (1, "Task Manager".to_owned(), false),
                    (2, "Resource Monitor".to_owned(), false),
                    (3, "Refresh".to_owned(), false),
                ]));

                let spec_blocks = blocks;
                let build = || FlyoutSpec {
                    title: String::new(),
                    subtitle: None,
                    width_dips: 296.0,
                    blocks: spec_blocks.clone(),
                };
                // Live flyout: Refresh re-samples CPU/RAM/GPU and updates in place.
                flyout::show_flyout_live(
                    "stats",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    build(),
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
                            let (cpu, ram) = self.stats.sample();
                            self.cpu_pct = cpu;
                            self.ram_pct = ram;
                            LiveOutcome::Update(build())
                        }
                        _ => LiveOutcome::Close,
                    },
                );
            }
            // Exact C# `WeatherWidget.BuildFlyoutContent`
            "weather" => {
                let snap = self.weather.lock().map(|g| g.clone()).unwrap_or_default();
                let city = if snap.city.is_empty() {
                    settings.weather_city.clone()
                } else {
                    snap.city
                };
                let temp = weather_temp(&snap.temp_c, settings);
                let feels = weather_temp(&snap.feels_like, settings);
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
                    "Weather".to_owned()
                } else {
                    snap.condition.clone()
                };
                let mut blocks = vec![
                    // C2 `Image` block for the weather icon (Segoe glyph fallback).
                    FlyoutBlock::Image {
                        glyph: weather_icon_glyph(&snap.condition),
                        path: None,
                        size_dips: 32.0,
                    },
                    FlyoutBlock::Hero {
                        value: city,
                        caption: Some(cond),
                        centered: false,
                        large: false,
                        right_meta: Some(temp),
                    },
                    FlyoutBlock::MetricTiles(vec![
                        ("Feels like".to_owned(), feels),
                        ("Humidity".to_owned(), hum),
                        ("Wind".to_owned(), wind),
                        ("Source".to_owned(), "Open-Meteo".to_owned()),
                    ]),
                ];
                // C# `WeatherWidget` 3-day forecast (data already collected in telemetry).
                if !snap.forecast_days.is_empty() {
                    blocks.push(FlyoutBlock::Separator);
                    blocks.push(FlyoutBlock::StatRow {
                        label: "3-Day Forecast".to_owned(),
                        value: String::new(),
                    });
                    for (day, condition, high_low) in snap.forecast_days.iter().take(3) {
                        blocks.push(FlyoutBlock::StatRow {
                            label: format!("{} {}", weather_icon_glyph_text(condition), day),
                            value: format!("{} · {}", high_low, condition),
                        });
                    }
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::ButtonBar(vec![(1, "Refresh".to_owned(), false)]));
                let spec = FlyoutSpec {
                    title: String::new(),
                    subtitle: None,
                    width_dips: 296.0,
                    blocks,
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    telemetry::refresh_weather(&self.weather, &settings.weather_city, true);
                    self.weather_retry_at =
                        Some(Instant::now() + std::time::Duration::from_secs(30 * 60));
                }
            }
            // C# `NetSpeedWidget`: live throughput + 6-stream speed test with cancel/retry/restart.
            "netspeed" => {
                let adapter = if self.net.adapter_name.is_empty() {
                    None
                } else {
                    Some(format!("Adapter: {}", self.net.adapter_name))
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
                                "↓ Download".to_owned(),
                                telemetry::format_speed(runtime.net.rx_bps),
                            ),
                            (
                                "↑ Upload".to_owned(),
                                telemetry::format_speed(runtime.net.tx_bps),
                            ),
                            (
                                "Session received".to_owned(),
                                telemetry::format_bytes(runtime.net.session_rx),
                            ),
                            (
                                "Session sent".to_owned(),
                                telemetry::format_bytes(runtime.net.session_tx),
                            ),
                        ]),
                        FlyoutBlock::Separator,
                    ];
                    blocks.append(&mut spec.blocks);
                    blocks.push(FlyoutBlock::ButtonBar(vec![(
                        5,
                        "Network Settings".to_owned(),
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
                            self.speedtest.poll(Instant::now(), self.speedtest_started_at);
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
                            self.speedtest.poll(Instant::now(), self.speedtest_started_at);
                            LiveOutcome::Update(build(self))
                        }
                    },
                );
            }
            // C# `BluetoothBatteryWidget.PopulateHoverFlyoutPanel` + device management.
            "bluetoothbattery" => {
                let ignored = setting_string_list(settings, "IgnoredWirelessDevices");
                let aliases = setting_value(settings, "WirelessDeviceAliases").and_then(|v| v.as_object().cloned()).unwrap_or_default();
                let low = setting_i64(settings, "WirelessBatteryLowThreshold", 20) as u32;
                let interval = setting_i64(settings, "WirelessBatteryPollSeconds", 60) as u64;
                let listen_only = setting_bool(settings, "WirelessBatteryListenOnly", false);
                let build = |_settings: &DockSettings, selected: usize, rename_value: String| {
                    let devices: Vec<_> = self
                        .bt_battery
                        .lock()
                        .map(|g| g.devices.clone())
                        .unwrap_or_default();
                    let mut blocks: Vec<FlyoutBlock> = devices
                        .iter()
                        .enumerate()
                        .filter(|(_, d)| !ignored.iter().any(|n| n.eq_ignore_ascii_case(&d.name)))
                        .map(|(i, d)| {
                            let shown = aliases
                                .get(&d.name)
                                .and_then(|v| v.as_str())
                                .unwrap_or(&d.name);
                            let bolt = if d.percent >= 95 { " ⚡" } else { "" };
                            let stale = self
                                .bt_battery
                                .lock()
                                .ok()
                                .and_then(|g| g.last_fetch)
                                .is_some_and(|t| t.elapsed().as_secs() > interval * 2);
                            FlyoutBlock::ActionRow {
                                id: 100 + i as u32,
                                title: format!("{shown}{bolt}"),
                                subtitle: Some(if stale {
                                    format!("{}% · stale", d.percent)
                                } else {
                                    format!("{}%", d.percent)
                                }),
                                badge: Some(format!("{}%", d.percent)),
                                icon: None,
                                destructive: false,
                            }
                        })
                        .collect();
                    if blocks.is_empty() {
                        blocks.push(FlyoutBlock::Note(
                            "No connected Bluetooth battery devices detected.".to_owned(),
                        ));
                    }
                    let _ = selected;
                    // C2 TextInput — device rename (1-64 chars).
                    blocks.push(FlyoutBlock::TextInput {
                        id: 50,
                        label: "Rename selected device".to_owned(),
                        value: rename_value,
                        numeric: false,
                        min: 0.0,
                        max: 0.0,
                    });
                    blocks.push(FlyoutBlock::StatRow {
                        label: "Poll interval".to_owned(),
                        value: format!("{interval}s"),
                    });
                    blocks.push(FlyoutBlock::StatRow {
                        label: "Low threshold".to_owned(),
                        value: format!("{low}%"),
                    });
                    blocks.push(FlyoutBlock::ToggleGrid(vec![(
                        7,
                        "Listen only".to_owned(),
                        if listen_only { "On" } else { "Off" }.to_owned(),
                        listen_only,
                    )]));
                    blocks.push(FlyoutBlock::ButtonBar(vec![
                        (1, "Refresh".to_owned(), false),
                        (2, "Hide".to_owned(), false),
                        (3, "Restore ignored".to_owned(), false),
                        (4, "Interval".to_owned(), false),
                        (5, "Threshold".to_owned(), false),
                    ]));
                    blocks.push(FlyoutBlock::ButtonBar(vec![
                        (6, "Copy Diagnostics".to_owned(), false),
                        (8, "Bluetooth Settings".to_owned(), false),
                    ]));
                    FlyoutSpec {
                        title: "Bluetooth Battery".to_owned(),
                        subtitle: Some(format!("{interval}s · low {low}%")),
                        width_dips: 296.0,
                        blocks,
                    }
                };
                let init = build(settings, 0, String::new());
                flyout::show_flyout_live(
                    "bluetoothbattery",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
                    |ev| match ev {
                        LiveEvent::Click(1) => {
                            telemetry::refresh_bluetooth_battery(&self.bt_battery, true);
                            LiveOutcome::Update(build(settings, 0, String::new()))
                        }
                        LiveEvent::Click(2) => {
                            // Hide the currently selected device.
                            if let Ok(g) = self.bt_battery.lock()
                                && let Some(d) = g.devices.first()
                            {
                                let mut list = setting_string_list(settings, "IgnoredWirelessDevices");
                                if !list.iter().any(|n| n.eq_ignore_ascii_case(&d.name)) {
                                    list.push(d.name.clone());
                                    set_setting(settings, "IgnoredWirelessDevices", serde_json::json!(list));
                                }
                            }
                            LiveOutcome::Update(build(settings, 0, String::new()))
                        }
                        LiveEvent::Click(3) => {
                            set_setting(settings, "IgnoredWirelessDevices", serde_json::json!([]));
                            LiveOutcome::Update(build(settings, 0, String::new()))
                        }
                        LiveEvent::Click(4) => {
                            const STEPS: [i64; 5] = [15, 30, 60, 120, 300];
                            let cur = setting_i64(settings, "WirelessBatteryPollSeconds", 60);
                            let next = STEPS.iter().find(|&&s| s > cur).copied().unwrap_or(STEPS[0]);
                            set_setting(settings, "WirelessBatteryPollSeconds", serde_json::json!(next));
                            LiveOutcome::Update(build(settings, 0, String::new()))
                        }
                        LiveEvent::Click(5) => {
                            let cur = setting_i64(settings, "WirelessBatteryLowThreshold", 20);
                            let next = if cur >= 50 { 5 } else { cur + 5 };
                            set_setting(settings, "WirelessBatteryLowThreshold", serde_json::json!(next));
                            LiveOutcome::Update(build(settings, 0, String::new()))
                        }
                        LiveEvent::Click(6) => {
                            let diag = self
                                .bt_battery
                                .lock()
                                .map(|g| {
                                    g.devices
                                        .iter()
                                        .map(|d| format!("{}={}%", d.name, d.percent))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_default();
                            copy_to_clipboard(&diag);
                            LiveOutcome::Update(build(settings, 0, String::new()))
                        }
                        LiveEvent::Click(7) => {
                            let next = !setting_bool(settings, "WirelessBatteryListenOnly", false);
                            set_setting(settings, "WirelessBatteryListenOnly", serde_json::json!(next));
                            LiveOutcome::Update(build(settings, 0, String::new()))
                        }
                        LiveEvent::Click(8) => {
                            pins::launch("ms-settings:bluetooth".to_owned());
                            LiveOutcome::Close
                        }
                        LiveEvent::TextCommit { id: 50, text } => {
                            if (1..=64).contains(&text.len())
                                && let Ok(g) = self.bt_battery.lock()
                                && let Some(d) = g.devices.first()
                            {
                                let mut map = setting_value(settings, "WirelessDeviceAliases")
                                    .and_then(|v| v.as_object().cloned())
                                    .unwrap_or_default();
                                map.insert(d.name.clone(), serde_json::json!(text));
                                set_setting(settings, "WirelessDeviceAliases", serde_json::json!(map));
                            }
                            LiveOutcome::Update(build(settings, 0, String::new()))
                        }
                        _ => LiveOutcome::Close,
                    },
                );
                return true;
            }
            // C# `StorageWidget` expanded drives flyout (DriveType.Fixed && IsReady).
            "storage" => {
                let drives = fixed_ready_drives();
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
                    title: "Keyboard Layout".to_owned(),
                    subtitle: None,
                    width_dips: 240.0,
                    blocks,
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                    && act >= 100
                    && let Some((hkl, _, _)) = layouts.get((act - 100) as usize)
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
                let _ = flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec);
            }
            // C# `FocusWidget.BuildFocusFlyout` — Break phase + session number + pulse.
            "focus" => {
                let build = || {
                    focus_break::build_focus_flyout(&self.focus.snapshot())
                };
                let init = build();
                flyout::show_flyout_live(
                    "focus",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
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
                        _ => LiveOutcome::Close,
                    },
                );
            }
            // Exact C# `TimedWidgetPanel.xaml` Stopwatch (live Lap / Start / Pause / Reset)
            "stopwatch" => {
                let init = self.build_stopwatch_spec();
                flyout::show_flyout_live(
                    "stopwatch",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
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
                flyout::show_flyout_live(
                    "countdown",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
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
                        _ => LiveOutcome::Close,
                    },
                );
            }
            "media" => {
                telemetry::refresh_media_session(&self.media_session, true);
                let build = |runtime: &Self| {
                    let snap = runtime
                        .media_session
                        .lock()
                        .ok()
                        .map(|g| g.clone())
                        .unwrap_or_default();
                    let details = media_ext::get_details(&snap, runtime.audio_vol, runtime.audio_muted);
                    let sessions = media_ext::list_app_sessions();
                    let mut spec = media_ext::build_media_flyout(&details, &sessions, settings);
                    spec.blocks.push(FlyoutBlock::Separator);
                    spec.blocks.push(FlyoutBlock::ButtonBar(vec![(
                        4,
                        "Sound Settings".to_owned(),
                        false,
                    )]));
                    spec
                };
                let init = build(self);
                flyout::show_flyout_live(
                    "media",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
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
                                .map_or(0, |g| g.duration_secs as u32);
                            telemetry::media_seek((ratio * dur as f32).round() as u32);
                            telemetry::refresh_media_session(&self.media_session, true);
                            LiveOutcome::Update(build(self))
                        }
                        LiveEvent::Slider { id: 51, ratio } => {
                            set_audio_volume((ratio * 100.0).round() as u32);
                            (self.audio_vol, self.audio_muted) = get_audio_state();
                            LiveOutcome::Update(build(self))
                        }
                        _ => LiveOutcome::Close,
                    },
                );
            }
            "quicksettings" => {
                let mut settings_changed = false;
                // C# `Radio*` states: Loading / TurningOn / TurningOff / On / Off / Unavailable.
                let wifi_state = radio_label(self.wifi_on, self.radio_loading).to_owned();
                let bt_state = radio_label(self.bluetooth_on, self.radio_loading).to_owned();
                let build_qs_spec = |settings: &DockSettings,
                                     night_light: bool,
                                     wifi_state: &str,
                                     bt_state: &str,
                                     wifi_on: bool,
                                     bt_on: bool| {
                    let (vol, muted) = get_audio_state();
                    let out_name = telemetry::default_audio_device_name(false)
                        .unwrap_or_else(|| "Speakers".to_owned());
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
                    FlyoutSpec {
                        title: "Quick Settings".to_owned(),
                        subtitle: None,
                        width_dips: 312.0,
                        blocks: vec![
                            FlyoutBlock::ToggleGrid(vec![
                                (
                                    5,
                                    "Wi-Fi".to_owned(),
                                    wifi_state.to_owned(),
                                    wifi_on,
                                ),
                                (
                                    6,
                                    "Bluetooth".to_owned(),
                                    bt_state.to_owned(),
                                    bt_on,
                                ),
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
                                    7,
                                    "Night Light".to_owned(),
                                    if night_light { "On" } else { "Off" }.to_owned(),
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
                            // C2 `IconBar` — 16 C# Control Center shortcuts (ms-settings URIs).
                            FlyoutBlock::IconBar {
                                buttons: vec![
                                    (100, "Wi-Fi".to_owned(), 0xE701, wifi_on),
                                    (101, "Bluetooth".to_owned(), 0xE702, bt_on),
                                    (102, "Airplane mode".to_owned(), 0xF71D, true),
                                    (103, "Night Light".to_owned(), 0xE708, night_light),
                                    (104, "Battery saver".to_owned(), 0xE7EF, true),
                                    (105, "Focus assist".to_owned(), 0xF4B9, settings.focus_mode),
                                    (106, "Display".to_owned(), 0xE7F3, true),
                                    (107, "Sound".to_owned(), 0xE7F5, true),
                                    (108, "Network".to_owned(), 0xE705, true),
                                    (109, "VPN".to_owned(), 0xE7A2, true),
                                    (110, "Nearby sharing".to_owned(), 0xE8D2, true),
                                    (111, "Project".to_owned(), 0xE7F6, true),
                                    (112, "Tablet mode".to_owned(), 0xE7F9, true),
                                    (113, "Mobile hotspot".to_owned(), 0xE7B7, true),
                                    (114, "Date & time".to_owned(), 0xE7F2, true),
                                    (115, "Taskbar".to_owned(), 0xE7F7, true),
                                ],
                            },
                            FlyoutBlock::ButtonBar(vec![
                                (8, "Settings".to_owned(), true),
                                (9, "Show Desktop".to_owned(), false),
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
                flyout::show_flyout_live(
                    "quicksettings",
                    owner_hwnd,
                    is_dark,
                    anchor_x,
                    anchor_y,
                    init,
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
                            if crate::platform::radio::set("wifi", target) {
                                self.wifi_on = target;
                            } else {
                                self.radio_unavailable = true;
                                pins::launch("ms-settings:network-wifi".to_owned());
                            }
                            let cur_wifi = if self.radio_unavailable {
                                "Unavailable".to_owned()
                            } else {
                                radio_label(self.wifi_on, false).to_owned()
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
                            if crate::platform::radio::set("bluetooth", target) {
                                self.bluetooth_on = target;
                            } else {
                                self.radio_unavailable = true;
                                pins::launch("ms-settings:bluetooth".to_owned());
                            }
                            let cur_bt = if self.radio_unavailable {
                                "Unavailable".to_owned()
                            } else {
                                radio_label(self.bluetooth_on, false).to_owned()
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
                        LiveEvent::Click(id @ 100..=115) => {
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
                    title: "Folder Stacks".to_owned(),
                    subtitle: None,
                    width_dips: 276.0,
                    blocks,
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                    && act >= 100
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
                    title: "App Groups".to_owned(),
                    subtitle: None,
                    width_dips: 276.0,
                    blocks,
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                    && act >= 100
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
        assert_eq!(ALL_WIDGETS.len(), 24);
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
