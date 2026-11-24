//! Native Win32 telemetry, controls, and shared `DockFlyout` builders for all 14 dock widgets.
//!
//! Widgets:
//! 1. `clock`         - Clock & Date
//! 2. `stats`         - CPU & RAM System Stats
//! 3. `audio`         - Core Audio Master Volume & Mute (supports scroll wheel!)
//! 4. `media`         - Media Transport Controls (Prev / Play-Pause / Next)
//! 5. `netspeed`      - Live Network Download / Upload Speed (`GetIfTable2`)
//! 6. `battery`       - Battery & Power Status (`GetSystemPowerStatus`)
//! 7. `storage`       - Fixed Drive Free / Used Space (`GetDiskFreeSpaceExW`)
//! 8. `keyboard`      - Active Keyboard Layout & Switcher (`ActivateKeyboardLayout`)
//! 9. `dayprogress`   - Day / Week / Year Progress
//! 10. `stopwatch`    - Live Stopwatch
//! 11. `countdown`    - Live Countdown Timer (5m / 15m / 25m presets)
//! 12. `focus`        - Pomodoro & Focus Mode Toggle
//! 13. `weather`      - Non-blocking Cached Weather (`wttr.in`)
//! 14. `quicksettings`- Quick Settings Panel (Focus, Auto-Hide, Theme, Mute, Lock, Settings)

use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use opentaskbar_core::settings::{AppTheme, AutoHideMode, DockSettings};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eMultimedia, eRender};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::Globalization::{GetLocaleInfoW, LOCALE_SISO639LANGNAME};
use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDrives};
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::SystemInformation::{
    GetLocalTime, GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX,
};
use windows_sys::Win32::System::Threading::GetSystemTimes;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ActivateKeyboardLayout, GetKeyboardLayout, GetKeyboardLayoutList, INPUT, INPUT_0,
    INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_MEDIA_NEXT_TRACK,
    VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
};
use windows_sys::Win32::UI::Shell::{
    SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW, SHQUERYRBINFO,
    SHQueryRecycleBinW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::flyout::{self, FlyoutBlock, FlyoutSpec};
use crate::pins;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Metadata for the Settings Widget Catalog and dock strip.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct WidgetDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub width_dips: f32,
}

pub const ALL_WIDGETS: &[WidgetDescriptor] = &[
    WidgetDescriptor {
        id: "clock",
        name: "Clock & Date",
        category: "Time",
        description: "Local time, date, and uptime flyout.",
        width_dips: 66.0,
    },
    WidgetDescriptor {
        id: "stats",
        name: "System Stats",
        category: "System",
        description: "Live CPU and memory utilization bars.",
        width_dips: 68.0,
    },
    WidgetDescriptor {
        id: "audio",
        name: "Audio Volume",
        category: "Media",
        description: "Master volume with scroll-wheel and mute control.",
        width_dips: 64.0,
    },
    WidgetDescriptor {
        id: "media",
        name: "Media Controls",
        category: "Media",
        description: "Quick previous, play/pause, and next track.",
        width_dips: 60.0,
    },
    WidgetDescriptor {
        id: "netspeed",
        name: "Network Speed",
        category: "System",
        description: "Real-time download and upload throughput.",
        width_dips: 72.0,
    },
    WidgetDescriptor {
        id: "battery",
        name: "Battery",
        category: "System",
        description: "Battery percentage, AC status, and remaining time.",
        width_dips: 64.0,
    },
    WidgetDescriptor {
        id: "storage",
        name: "Storage",
        category: "System",
        description: "Free space overview across all fixed drives.",
        width_dips: 68.0,
    },
    WidgetDescriptor {
        id: "keyboard",
        name: "Keyboard Layout",
        category: "Input",
        description: "Active input language with instant layout switcher.",
        width_dips: 50.0,
    },
    WidgetDescriptor {
        id: "dayprogress",
        name: "Day Progress",
        category: "Time",
        description: "Percentage of day, week, and year elapsed.",
        width_dips: 66.0,
    },
    WidgetDescriptor {
        id: "stopwatch",
        name: "Stopwatch",
        category: "Productivity",
        description: "Start, pause, and reset elapsed timer.",
        width_dips: 62.0,
    },
    WidgetDescriptor {
        id: "countdown",
        name: "Countdown",
        category: "Productivity",
        description: "Quick 5m, 15m, and 25m countdown timer.",
        width_dips: 62.0,
    },
    WidgetDescriptor {
        id: "focus",
        name: "Focus & Pomodoro",
        category: "Productivity",
        description: "25m focus session with distraction-free Focus Mode.",
        width_dips: 66.0,
    },
    WidgetDescriptor {
        id: "weather",
        name: "Weather",
        category: "Info",
        description: "Current temperature and conditions for your city.",
        width_dips: 68.0,
    },
    WidgetDescriptor {
        id: "quicksettings",
        name: "Quick Settings",
        category: "System",
        description: "Fast toggles for Focus, Auto-Hide, Theme, and Audio.",
        width_dips: 58.0,
    },
];

pub fn widget_descriptor(id: &str) -> Option<&'static WidgetDescriptor> {
    if id.eq_ignore_ascii_case("stacks") {
        return Some(&STACKS_WIDGET);
    }
    if id.eq_ignore_ascii_case("groups") {
        return Some(&GROUPS_WIDGET);
    }
    ALL_WIDGETS.iter().find(|w| w.id.eq_ignore_ascii_case(id))
}

/// Ordered list of active widget descriptors according to `settings`.
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

const STACKS_WIDGET: WidgetDescriptor = WidgetDescriptor {
    id: "stacks",
    name: "Folder Stacks",
    category: "System",
    description: "Quick-access folder stacks.",
    width_dips: 58.0,
};

const GROUPS_WIDGET: WidgetDescriptor = WidgetDescriptor {
    id: "groups",
    name: "App Groups",
    category: "System",
    description: "Custom grouped application launchers.",
    width_dips: 58.0,
};

fn ft_to_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

#[derive(Default)]
pub struct SystemStatsSampler {
    prev_idle: u64,
    prev_kernel: u64,
    prev_user: u64,
}

impl SystemStatsSampler {
    pub fn sample(&mut self) -> (u32, u32) {
        let cpu = self.sample_cpu();
        let ram = sample_ram_info().0;
        (cpu, ram)
    }

    fn sample_cpu(&mut self) -> u32 {
        let mut idle = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
        if ok == 0 {
            return 0;
        }
        let idle_v = ft_to_u64(idle);
        let kernel_v = ft_to_u64(kernel);
        let user_v = ft_to_u64(user);

        let pct = if self.prev_kernel != 0 || self.prev_user != 0 {
            let d_idle = idle_v.saturating_sub(self.prev_idle);
            let d_kernel = kernel_v.saturating_sub(self.prev_kernel);
            let d_user = user_v.saturating_sub(self.prev_user);
            let total = d_kernel.saturating_add(d_user);
            if total > 0 {
                let busy = total.saturating_sub(d_idle);
                ((busy.saturating_mul(100)) / total).min(100) as u32
            } else {
                0
            }
        } else {
            0
        };

        self.prev_idle = idle_v;
        self.prev_kernel = kernel_v;
        self.prev_user = user_v;
        pct
    }
}

/// Returns `(ram_percent, used_gb, total_gb)`.
pub fn sample_ram_info() -> (u32, f32, f32) {
    let mut mem: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    mem.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut mem) } != 0 {
        let total = mem.ullTotalPhys as f64 / 1_073_741_824.0;
        let avail = mem.ullAvailPhys as f64 / 1_073_741_824.0;
        let used = (total - avail).max(0.0);
        (mem.dwMemoryLoad.min(100), used as f32, total as f32)
    } else {
        (0, 0.0, 0.0)
    }
}

/// Returns `(HH:MM, dd.MM.yyyy)` in the local system timezone.
pub fn local_clock_strings() -> (String, String) {
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        GetLocalTime(&mut st);
    }
    let time = format!("{:02}:{:02}", st.wHour, st.wMinute);
    let date = format!("{:02}.{:02}.{}", st.wDay, st.wMonth, st.wYear);
    (time, date)
}

fn system_uptime_string() -> String {
    let secs = unsafe { GetTickCount64() } / 1000;
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else {
        format!("{hours}h {mins}m")
    }
}

pub fn is_recycle_bin_non_empty() -> bool {
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHQUERYRBINFO>() as u32;
    let hr = unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) };
    hr == 0 && info.i64NumItems > 0
}

pub fn empty_recycle_bin() {
    std::thread::Builder::new()
        .name("opentaskbar-empty-bin".into())
        .spawn(|| unsafe {
            let _ = SHEmptyRecycleBinW(
                std::ptr::null_mut(),
                std::ptr::null(),
                SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
            );
        })
        .ok();
}

// --- Audio (Core Audio IAudioEndpointVolume) ---

fn get_endpoint_volume() -> Option<IAudioEndpointVolume> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .ok()?;
        device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .ok()
    }
}

pub fn get_audio_state() -> (u32, bool) {
    let Some(ep) = get_endpoint_volume() else {
        return (0, false);
    };
    unsafe {
        let vol = ep
            .GetMasterVolumeLevelScalar()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let muted = ep.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        ((vol * 100.0).round() as u32, muted)
    }
}

pub fn adjust_audio_volume(delta_pct: i32) {
    let Some(ep) = get_endpoint_volume() else {
        return;
    };
    unsafe {
        let cur = ep.GetMasterVolumeLevelScalar().unwrap_or(0.5);
        let next = (cur + delta_pct as f32 / 100.0).clamp(0.0, 1.0);
        let _ = ep.SetMasterVolumeLevelScalar(next, std::ptr::null());
        if delta_pct > 0 {
            let _ = ep.SetMute(false, std::ptr::null());
        }
    }
}

pub fn toggle_audio_mute() {
    let Some(ep) = get_endpoint_volume() else {
        return;
    };
    unsafe {
        let muted = ep.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        let _ = ep.SetMute(!muted, std::ptr::null());
    }
}

// --- Media Transport Keys ---

fn send_media_vk(vk: u16) {
    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: 0,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ];
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        );
    }
}

// --- Network Speed (`GetIfTable2`) ---

#[derive(Default)]
pub struct NetSpeedSampler {
    prev_rx: u64,
    prev_tx: u64,
    prev_at: Option<Instant>,
    pub rx_bps: u64,
    pub tx_bps: u64,
}

impl NetSpeedSampler {
    pub fn sample(&mut self) -> (u64, u64) {
        let (rx, tx) = read_total_net_bytes();
        let now = Instant::now();
        if let Some(prev) = self.prev_at {
            let dt = now.duration_since(prev).as_secs_f64().max(0.2);
            self.rx_bps = (rx.saturating_sub(self.prev_rx) as f64 / dt) as u64;
            self.tx_bps = (tx.saturating_sub(self.prev_tx) as f64 / dt) as u64;
        }
        self.prev_rx = rx;
        self.prev_tx = tx;
        self.prev_at = Some(now);
        (self.rx_bps, self.tx_bps)
    }
}

fn read_total_net_bytes() -> (u64, u64) {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    unsafe {
        if GetIfTable2(&mut table) != 0 || table.is_null() {
            return (0, 0);
        }
        let count = (*table).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count);
        let mut rx = 0u64;
        let mut tx = 0u64;
        for row in rows {
            if row.OperStatus == IfOperStatusUp && row.Type != 24 {
                rx = rx.saturating_add(row.InOctets);
                tx = tx.saturating_add(row.OutOctets);
            }
        }
        FreeMibTable(table.cast());
        (rx, tx)
    }
}

fn format_speed(bps: u64) -> String {
    if bps >= 1_048_576 {
        format!("{:.1}M", bps as f64 / 1_048_576.0)
    } else if bps >= 1024 {
        format!("{}K", bps / 1024)
    } else {
        format!("{bps}B")
    }
}

// --- Battery ---

pub struct BatteryInfo {
    pub has_battery: bool,
    pub percent: u32,
    pub on_ac: bool,
    pub charging: bool,
    pub life_secs: Option<u32>,
}

pub fn sample_battery() -> BatteryInfo {
    let mut st: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut st) } == 0 {
        return BatteryInfo {
            has_battery: false,
            percent: 100,
            on_ac: true,
            charging: false,
            life_secs: None,
        };
    }
    let has_battery = (st.BatteryFlag & 128) == 0 && st.BatteryLifePercent <= 100;
    let charging = (st.BatteryFlag & 8) != 0;
    let life_secs = (st.BatteryLifeTime != u32::MAX).then_some(st.BatteryLifeTime);
    BatteryInfo {
        has_battery,
        percent: if has_battery {
            u32::from(st.BatteryLifePercent)
        } else {
            100
        },
        on_ac: st.ACLineStatus == 1,
        charging,
        life_secs,
    }
}

// --- Storage ---

pub struct DriveSpace {
    pub letter: char,
    pub free_gb: f64,
    pub total_gb: f64,
}

pub fn sample_drives() -> Vec<DriveSpace> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if (mask & (1 << i)) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root: Vec<u16> = format!("{letter}:\\\0").encode_utf16().collect();
        let mut free_avail = 0u64;
        let mut total = 0u64;
        let mut total_free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(root.as_ptr(), &mut free_avail, &mut total, &mut total_free)
        };
        if ok != 0 && total > 0 {
            out.push(DriveSpace {
                letter,
                free_gb: free_avail as f64 / 1_073_741_824.0,
                total_gb: total as f64 / 1_073_741_824.0,
            });
        }
    }
    out
}

// --- Keyboard Layout ---

fn hkl_iso_code(hkl: isize) -> String {
    let lcid = (hkl as u32) & 0xffff;
    let mut buf = [0u16; 16];
    let len = unsafe {
        GetLocaleInfoW(
            lcid,
            LOCALE_SISO639LANGNAME,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    if len > 1 {
        String::from_utf16_lossy(&buf[..(len - 1) as usize]).to_uppercase()
    } else {
        format!("{lcid:04X}")
    }
}

pub fn active_keyboard_code() -> String {
    unsafe {
        let fg = GetForegroundWindow();
        let tid = if !fg.is_null() {
            GetWindowThreadProcessId(fg, std::ptr::null_mut())
        } else {
            0
        };
        let hkl = GetKeyboardLayout(tid);
        hkl_iso_code(hkl as isize)
    }
}

pub fn installed_keyboard_layouts() -> Vec<(isize, String)> {
    let mut list = [std::ptr::null_mut(); 16];
    let count = unsafe { GetKeyboardLayoutList(list.len() as i32, list.as_mut_ptr()) };
    if count <= 0 {
        return Vec::new();
    }
    list[..count as usize]
        .iter()
        .map(|&h| (h as isize, hkl_iso_code(h as isize)))
        .collect()
}

// --- Day Progress ---

pub fn day_progress_metrics() -> (f32, f32, f32, u32) {
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        GetLocalTime(&mut st);
    }
    let mins_today = u32::from(st.wHour) * 60 + u32::from(st.wMinute);
    let day_ratio = (mins_today as f32 / 1440.0).clamp(0.0, 1.0);
    let hours_left = (1440u32.saturating_sub(mins_today)) / 60;
    let dow = if st.wDayOfWeek == 0 {
        6
    } else {
        u32::from(st.wDayOfWeek - 1)
    };
    let week_ratio = ((dow as f32 + day_ratio) / 7.0).clamp(0.0, 1.0);
    let day_of_year = (u32::from(st.wMonth.saturating_sub(1)) * 30 + u32::from(st.wDay)).min(365);
    let year_ratio = (day_of_year as f32 / 365.0).clamp(0.0, 1.0);
    (day_ratio, week_ratio, year_ratio, hours_left)
}

// --- Weather (non-blocking background cache) ---

#[derive(Clone, Default)]
pub struct WeatherCache {
    pub city: String,
    pub temp_c: String,
    pub condition: String,
    pub feels_like: String,
    pub humidity: String,
    pub last_fetch: Option<Instant>,
    pub loading: bool,
}

// --- Stateful Widget Runtime ---

pub struct WidgetRuntime {
    pub stats: SystemStatsSampler,
    pub net: NetSpeedSampler,
    pub cpu_pct: u32,
    pub ram_pct: u32,
    pub audio_vol: u32,
    pub audio_muted: bool,
    pub stopwatch_running: bool,
    pub stopwatch_secs: u64,
    pub countdown_running: bool,
    pub countdown_total_secs: u64,
    pub countdown_remaining_secs: u64,
    pub focus_running: bool,
    pub focus_remaining_secs: u64,
    pub weather: Arc<Mutex<WeatherCache>>,
    last_tick: Instant,
}

impl Default for WidgetRuntime {
    fn default() -> Self {
        Self {
            stats: SystemStatsSampler::default(),
            net: NetSpeedSampler::default(),
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
            weather: Arc::new(Mutex::new(WeatherCache::default())),
            last_tick: Instant::now(),
        }
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
            let (c, r) = self.stats.sample();
            self.cpu_pct = c;
            self.ram_pct = r;
        }
        if settings.is_widget_enabled("audio") {
            let (v, m) = get_audio_state();
            self.audio_vol = v;
            self.audio_muted = m;
        }
        if settings.is_widget_enabled("netspeed") {
            self.net.sample();
        }
        if settings.is_widget_enabled("weather") {
            self.maybe_refresh_weather(&settings.weather_city, false);
        }
    }

    pub fn maybe_refresh_weather(&self, city: &str, force: bool) {
        let city = if city.trim().is_empty() {
            "Istanbul"
        } else {
            city.trim()
        };
        let should_fetch = if let Ok(mut guard) = self.weather.lock() {
            let stale = guard
                .last_fetch
                .is_none_or(|t| t.elapsed().as_secs() >= 1800);
            let city_changed = !guard.city.eq_ignore_ascii_case(city);
            if !guard.loading && (force || stale || city_changed) {
                guard.loading = true;
                guard.city = city.to_owned();
                true
            } else {
                false
            }
        } else {
            false
        };
        if !should_fetch {
            return;
        }
        let cache = Arc::clone(&self.weather);
        let city_owned = city.to_owned();
        let _ = std::thread::Builder::new()
            .name("opentaskbar-weather".into())
            .spawn(move || {
                let url = format!(
                    "https://wttr.in/{}?format=%t|%C|%f|%h",
                    city_owned.replace(' ', "%20")
                );
                let output = Command::new("curl.exe")
                    .args(["-s", "--max-time", "5", &url])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
                if let Ok(mut guard) = cache.lock() {
                    guard.loading = false;
                    guard.last_fetch = Some(Instant::now());
                    if let Ok(out) = output
                        && out.status.success()
                    {
                        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                        let parts: Vec<&str> = text.split('|').collect();
                        if parts.len() >= 4 {
                            guard.temp_c = parts[0].trim().trim_start_matches('+').to_owned();
                            guard.condition = parts[1].trim().to_owned();
                            guard.feels_like = parts[2].trim().trim_start_matches('+').to_owned();
                            guard.humidity = parts[3].trim().to_owned();
                        }
                    }
                }
            });
    }

    /// Returns `(top_line, bottom_line)` rendered on the dock pill chip for widget `id`.
    pub fn chip_lines(&self, id: &str, settings: &DockSettings) -> (String, String) {
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
                format!("D {}", format_speed(self.net.rx_bps)),
                format!("U {}", format_speed(self.net.tx_bps)),
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
            "storage" => {
                let drives = sample_drives();
                if let Some(d) = drives.first() {
                    (
                        format!("{}: {:.0}G", d.letter, d.free_gb),
                        "Free".to_owned(),
                    )
                } else {
                    ("DISK".to_owned(), "--".to_owned())
                }
            }
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
            "weather" => {
                if let Ok(guard) = self.weather.lock() {
                    let temp = if guard.temp_c.is_empty() {
                        "--°C".to_owned()
                    } else {
                        guard.temp_c.clone()
                    };
                    let city = if guard.city.is_empty() {
                        settings.weather_city.clone()
                    } else {
                        guard.city.clone()
                    };
                    (temp, city)
                } else {
                    ("--°C".to_owned(), settings.weather_city.clone())
                }
            }
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

    /// Opens the shared `DockFlyout` for widget `id`. Returns `true` if `settings` changed.
    pub fn open_widget_flyout(
        &mut self,
        id: &str,
        owner_hwnd: usize,
        is_dark: bool,
        anchor_x: i32,
        anchor_y: i32,
        settings: &mut DockSettings,
    ) -> bool {
        match id {
            "clock" => {
                let (time, date) = local_clock_strings();
                let spec = FlyoutSpec {
                    title: time,
                    subtitle: Some(date),
                    width_dips: 270.0,
                    blocks: vec![
                        FlyoutBlock::StatRow {
                            label: "System Uptime".to_owned(),
                            value: system_uptime_string(),
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![
                            (1, "Notifications".to_owned(), false),
                            (2, "Date & Time".to_owned(), false),
                        ]),
                    ],
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                {
                    match act {
                        1 => opentaskbar_shell::taskbar::open_notifications(),
                        2 => pins::launch("ms-settings:dateandtime".to_owned()),
                        _ => {}
                    }
                }
                false
            }
            "stats" => {
                let (ram_pct, used_gb, total_gb) = sample_ram_info();
                let spec = FlyoutSpec {
                    title: "System Stats".to_owned(),
                    subtitle: Some(format!("Uptime {}", system_uptime_string())),
                    width_dips: 280.0,
                    blocks: vec![
                        FlyoutBlock::ProgressBar {
                            label: "CPU Usage".to_owned(),
                            value_text: format!("{}%", self.cpu_pct),
                            ratio: self.cpu_pct as f32 / 100.0,
                        },
                        FlyoutBlock::ProgressBar {
                            label: "Memory".to_owned(),
                            value_text: format!("{used_gb:.1} / {total_gb:.1} GB ({ram_pct}%)"),
                            ratio: ram_pct as f32 / 100.0,
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(1, "Open Task Manager".to_owned(), true)]),
                    ],
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    pins::launch("taskmgr.exe".to_owned());
                }
                false
            }
            "audio" => {
                let (vol, muted) = get_audio_state();
                self.audio_vol = vol;
                self.audio_muted = muted;
                let spec = FlyoutSpec {
                    title: "Audio Volume".to_owned(),
                    subtitle: Some("Tip: Scroll wheel over widget to adjust".to_owned()),
                    width_dips: 280.0,
                    blocks: vec![
                        FlyoutBlock::ProgressBar {
                            label: "Master Output".to_owned(),
                            value_text: if muted {
                                "Muted".to_owned()
                            } else {
                                format!("{vol}%")
                            },
                            ratio: if muted { 0.0 } else { vol as f32 / 100.0 },
                        },
                        FlyoutBlock::ButtonBar(vec![
                            (1, "-10%".to_owned(), false),
                            (2, if muted { "Unmute" } else { "Mute" }.to_owned(), muted),
                            (3, "+10%".to_owned(), false),
                        ]),
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(4, "Sound Settings".to_owned(), false)]),
                    ],
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                {
                    match act {
                        1 => adjust_audio_volume(-10),
                        2 => toggle_audio_mute(),
                        3 => adjust_audio_volume(10),
                        4 => pins::launch("ms-settings:sound".to_owned()),
                        _ => {}
                    }
                    let (v, m) = get_audio_state();
                    self.audio_vol = v;
                    self.audio_muted = m;
                }
                false
            }
            "media" => {
                let spec = FlyoutSpec {
                    title: "Media Controls".to_owned(),
                    subtitle: Some("System media transport".to_owned()),
                    width_dips: 270.0,
                    blocks: vec![FlyoutBlock::ButtonBar(vec![
                        (1, "Prev".to_owned(), false),
                        (2, "Play / Pause".to_owned(), true),
                        (3, "Next".to_owned(), false),
                    ])],
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                {
                    match act {
                        1 => send_media_vk(VK_MEDIA_PREV_TRACK),
                        2 => send_media_vk(VK_MEDIA_PLAY_PAUSE),
                        3 => send_media_vk(VK_MEDIA_NEXT_TRACK),
                        _ => {}
                    }
                }
                false
            }
            "netspeed" => {
                let spec = FlyoutSpec {
                    title: "Network Speed".to_owned(),
                    subtitle: Some("Active non-loopback adapters".to_owned()),
                    width_dips: 275.0,
                    blocks: vec![
                        FlyoutBlock::StatRow {
                            label: "Download".to_owned(),
                            value: format!("{}/s", format_speed(self.net.rx_bps)),
                        },
                        FlyoutBlock::StatRow {
                            label: "Upload".to_owned(),
                            value: format!("{}/s", format_speed(self.net.tx_bps)),
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(1, "Network Settings".to_owned(), false)]),
                    ],
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    pins::launch("ms-settings:network".to_owned());
                }
                false
            }
            "battery" => {
                let b = sample_battery();
                let status = if !b.has_battery {
                    "Desktop AC Power".to_owned()
                } else if b.charging {
                    "Charging".to_owned()
                } else if b.on_ac {
                    "Plugged in".to_owned()
                } else {
                    "On battery".to_owned()
                };
                let remain = b
                    .life_secs
                    .map(|s| format!("{}h {}m", s / 3600, (s % 3600) / 60))
                    .unwrap_or_else(|| "Calculating / AC".to_owned());
                let spec = FlyoutSpec {
                    title: "Battery & Power".to_owned(),
                    subtitle: Some(status),
                    width_dips: 275.0,
                    blocks: vec![
                        FlyoutBlock::ProgressBar {
                            label: "Charge Level".to_owned(),
                            value_text: format!("{}%", b.percent),
                            ratio: b.percent as f32 / 100.0,
                        },
                        FlyoutBlock::StatRow {
                            label: "Estimated Time".to_owned(),
                            value: remain,
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(1, "Power Settings".to_owned(), false)]),
                    ],
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    pins::launch("ms-settings:powersleep".to_owned());
                }
                false
            }
            "storage" => {
                let drives = sample_drives();
                let mut blocks: Vec<FlyoutBlock> = drives
                    .iter()
                    .take(5)
                    .map(|d| {
                        let used = (d.total_gb - d.free_gb).max(0.0);
                        let ratio = (used / d.total_gb.max(1.0)) as f32;
                        FlyoutBlock::ProgressBar {
                            label: format!("Drive {}:", d.letter),
                            value_text: format!("{:.0} GB free / {:.0} GB", d.free_gb, d.total_gb),
                            ratio,
                        }
                    })
                    .collect();
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note("No fixed drives detected.".to_owned()));
                }
                blocks.push(FlyoutBlock::Separator);
                blocks.push(FlyoutBlock::ButtonBar(vec![(
                    1,
                    "Open File Explorer".to_owned(),
                    false,
                )]));
                let spec = FlyoutSpec {
                    title: "Storage".to_owned(),
                    subtitle: Some(format!("{} drive(s) mounted", drives.len())),
                    width_dips: 290.0,
                    blocks,
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    pins::launch("explorer.exe".to_owned());
                }
                false
            }
            "keyboard" => {
                let layouts = installed_keyboard_layouts();
                let active = active_keyboard_code();
                let mut blocks: Vec<FlyoutBlock> = layouts
                    .iter()
                    .enumerate()
                    .map(|(i, (_, code))| FlyoutBlock::ActionRow {
                        id: 100 + i as u32,
                        title: format!("Keyboard Layout: {code}"),
                        subtitle: None,
                        badge: if code == &active {
                            Some("Active".to_owned())
                        } else {
                            None
                        },
                        icon: None,
                        destructive: false,
                    })
                    .collect();
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note(format!("Active layout: {active}")));
                }
                let spec = FlyoutSpec {
                    title: "Keyboard Layout".to_owned(),
                    subtitle: Some("Click a layout to switch immediately".to_owned()),
                    width_dips: 270.0,
                    blocks,
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                    && act >= 100
                    && let Some((hkl, _)) = layouts.get((act - 100) as usize)
                {
                    unsafe {
                        ActivateKeyboardLayout((*hkl) as _, 0);
                    }
                }
                false
            }
            "dayprogress" => {
                let (day, week, year, left) = day_progress_metrics();
                let spec = FlyoutSpec {
                    title: "Day Progress".to_owned(),
                    subtitle: Some(format!("{left} hours remaining today")),
                    width_dips: 275.0,
                    blocks: vec![
                        FlyoutBlock::ProgressBar {
                            label: "Today".to_owned(),
                            value_text: format!("{:.0}%", day * 100.0),
                            ratio: day,
                        },
                        FlyoutBlock::ProgressBar {
                            label: "This Week".to_owned(),
                            value_text: format!("{:.0}%", week * 100.0),
                            ratio: week,
                        },
                        FlyoutBlock::ProgressBar {
                            label: "This Year".to_owned(),
                            value_text: format!("{:.0}%", year * 100.0),
                            ratio: year,
                        },
                    ],
                };
                let _ = flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec);
                false
            }
            "stopwatch" => {
                let h = self.stopwatch_secs / 3600;
                let m = (self.stopwatch_secs % 3600) / 60;
                let s = self.stopwatch_secs % 60;
                let spec = FlyoutSpec {
                    title: "Stopwatch".to_owned(),
                    subtitle: Some(if self.stopwatch_running {
                        "Running".to_owned()
                    } else {
                        "Paused".to_owned()
                    }),
                    width_dips: 260.0,
                    blocks: vec![
                        FlyoutBlock::StatRow {
                            label: "Elapsed".to_owned(),
                            value: format!("{h:02}:{m:02}:{s:02}"),
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
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                {
                    match act {
                        1 => self.stopwatch_running = !self.stopwatch_running,
                        2 => {
                            self.stopwatch_running = false;
                            self.stopwatch_secs = 0;
                        }
                        _ => {}
                    }
                }
                false
            }
            "countdown" => {
                let ratio = if self.countdown_total_secs > 0 {
                    1.0 - (self.countdown_remaining_secs as f32 / self.countdown_total_secs as f32)
                } else {
                    0.0
                };
                let spec = FlyoutSpec {
                    title: "Countdown".to_owned(),
                    subtitle: Some(if self.countdown_running {
                        "Counting down".to_owned()
                    } else {
                        "Ready".to_owned()
                    }),
                    width_dips: 270.0,
                    blocks: vec![
                        FlyoutBlock::ProgressBar {
                            label: "Remaining".to_owned(),
                            value_text: format!(
                                "{:02}:{:02}",
                                self.countdown_remaining_secs / 60,
                                self.countdown_remaining_secs % 60
                            ),
                            ratio,
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
                            (3, "5 min".to_owned(), false),
                            (4, "15 min".to_owned(), false),
                            (5, "25 min".to_owned(), false),
                        ]),
                    ],
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                {
                    match act {
                        1 => self.countdown_running = !self.countdown_running,
                        2 => {
                            self.countdown_running = false;
                            self.countdown_remaining_secs = self.countdown_total_secs;
                        }
                        3 => {
                            self.countdown_total_secs = 5 * 60;
                            self.countdown_remaining_secs = 5 * 60;
                            self.countdown_running = true;
                        }
                        4 => {
                            self.countdown_total_secs = 15 * 60;
                            self.countdown_remaining_secs = 15 * 60;
                            self.countdown_running = true;
                        }
                        5 => {
                            self.countdown_total_secs = 25 * 60;
                            self.countdown_remaining_secs = 25 * 60;
                            self.countdown_running = true;
                        }
                        _ => {}
                    }
                }
                false
            }
            "focus" => {
                let ratio = 1.0 - (self.focus_remaining_secs as f32 / (25.0 * 60.0));
                let spec = FlyoutSpec {
                    title: "Focus & Pomodoro".to_owned(),
                    subtitle: Some(if settings.focus_mode {
                        "Focus Mode active (distracting widgets hidden)".to_owned()
                    } else {
                        "25-minute deep work session".to_owned()
                    }),
                    width_dips: 290.0,
                    blocks: vec![
                        FlyoutBlock::ProgressBar {
                            label: "Session".to_owned(),
                            value_text: format!(
                                "{:02}:{:02}",
                                self.focus_remaining_secs / 60,
                                self.focus_remaining_secs % 60
                            ),
                            ratio,
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
                        FlyoutBlock::ActionRow {
                            id: 3,
                            title: if settings.focus_mode {
                                "Disable Focus Mode".to_owned()
                            } else {
                                "Enable Focus Mode".to_owned()
                            },
                            subtitle: Some("Hide non-essential widgets on the dock".to_owned()),
                            badge: Some(if settings.focus_mode { "On" } else { "Off" }.to_owned()),
                            icon: None,
                            destructive: false,
                        },
                    ],
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                {
                    match act {
                        1 => self.focus_running = !self.focus_running,
                        2 => {
                            self.focus_running = false;
                            self.focus_remaining_secs = 25 * 60;
                        }
                        3 => {
                            settings.focus_mode = !settings.focus_mode;
                            return true;
                        }
                        _ => {}
                    }
                }
                false
            }
            "weather" => {
                let snapshot = self.weather.lock().map(|g| g.clone()).unwrap_or_default();
                let city = if snapshot.city.is_empty() {
                    settings.weather_city.clone()
                } else {
                    snapshot.city
                };
                let spec = FlyoutSpec {
                    title: format!("Weather — {city}"),
                    subtitle: Some(if snapshot.condition.is_empty() {
                        "Fetching live conditions...".to_owned()
                    } else {
                        snapshot.condition
                    }),
                    width_dips: 275.0,
                    blocks: vec![
                        FlyoutBlock::StatRow {
                            label: "Temperature".to_owned(),
                            value: if snapshot.temp_c.is_empty() {
                                "--".to_owned()
                            } else {
                                snapshot.temp_c
                            },
                        },
                        FlyoutBlock::StatRow {
                            label: "Feels Like".to_owned(),
                            value: if snapshot.feels_like.is_empty() {
                                "--".to_owned()
                            } else {
                                snapshot.feels_like
                            },
                        },
                        FlyoutBlock::StatRow {
                            label: "Humidity".to_owned(),
                            value: if snapshot.humidity.is_empty() {
                                "--".to_owned()
                            } else {
                                snapshot.humidity
                            },
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![(1, "Refresh Now".to_owned(), false)]),
                    ],
                };
                if flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec) == Some(1) {
                    self.maybe_refresh_weather(&settings.weather_city, true);
                }
                false
            }
            "quicksettings" => {
                let (_, muted) = get_audio_state();
                let spec = FlyoutSpec {
                    title: "Quick Settings".to_owned(),
                    subtitle: Some("Instant dock & system controls".to_owned()),
                    width_dips: 290.0,
                    blocks: vec![
                        FlyoutBlock::ActionRow {
                            id: 1,
                            title: "Focus Mode".to_owned(),
                            subtitle: Some("Hide distracting dock widgets".to_owned()),
                            badge: Some(if settings.focus_mode { "On" } else { "Off" }.to_owned()),
                            icon: None,
                            destructive: false,
                        },
                        FlyoutBlock::ActionRow {
                            id: 2,
                            title: "Auto-Hide Dock".to_owned(),
                            subtitle: None,
                            badge: Some(format!("{:?}", settings.auto_hide)),
                            icon: None,
                            destructive: false,
                        },
                        FlyoutBlock::ActionRow {
                            id: 3,
                            title: "Theme".to_owned(),
                            subtitle: None,
                            badge: Some(format!("{:?}", settings.theme)),
                            icon: None,
                            destructive: false,
                        },
                        FlyoutBlock::ActionRow {
                            id: 4,
                            title: if muted { "Unmute Audio" } else { "Mute Audio" }.to_owned(),
                            subtitle: None,
                            badge: Some(if muted { "Muted" } else { "Active" }.to_owned()),
                            icon: None,
                            destructive: false,
                        },
                        FlyoutBlock::Separator,
                        FlyoutBlock::ButtonBar(vec![
                            (5, "Open Settings".to_owned(), true),
                            (6, "Show Desktop".to_owned(), false),
                        ]),
                    ],
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                {
                    match act {
                        1 => {
                            settings.focus_mode = !settings.focus_mode;
                            return true;
                        }
                        2 => {
                            settings.auto_hide = match settings.auto_hide {
                                AutoHideMode::Disabled => AutoHideMode::Smart,
                                AutoHideMode::Smart => AutoHideMode::Always,
                                AutoHideMode::Always => AutoHideMode::Disabled,
                            };
                            return true;
                        }
                        3 => {
                            settings.theme = match settings.theme {
                                AppTheme::System => AppTheme::Dark,
                                AppTheme::Dark => AppTheme::Light,
                                AppTheme::Light => AppTheme::System,
                            };
                            return true;
                        }
                        4 => toggle_audio_mute(),
                        5 => crate::settings_ui::spawn_settings_window(),
                        6 => opentaskbar_shell::taskbar::show_desktop(),
                        _ => {}
                    }
                }
                false
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
                    blocks.push(FlyoutBlock::ActionRow {
                        id,
                        title: name,
                        subtitle: Some(folder.clone()),
                        badge: Some("Open".to_owned()),
                        icon: None,
                        destructive: false,
                    });
                    if let Ok(entries) = std::fs::read_dir(folder) {
                        for entry in entries.flatten().take(3) {
                            let p = entry.path();
                            let fname = p
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("File")
                                .to_owned();
                            let fid = 100 + paths.len() as u32;
                            paths.push(p.to_string_lossy().into_owned());
                            blocks.push(FlyoutBlock::ActionRow {
                                id: fid,
                                title: format!("  {fname}"),
                                subtitle: None,
                                badge: None,
                                icon: None,
                                destructive: false,
                            });
                        }
                    }
                }
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note("No folder stacks configured.".to_owned()));
                }
                let spec = FlyoutSpec {
                    title: "Folder Stacks".to_owned(),
                    subtitle: Some(format!("{} pinned folder(s)", settings.stack_folders.len())),
                    width_dips: 310.0,
                    blocks,
                };
                if let Some(act) =
                    flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec)
                    && act >= 100
                    && let Some(target) = paths.get((act - 100) as usize)
                {
                    pins::launch(target.clone());
                }
                false
            }
            "groups" => {
                let mut blocks = Vec::new();
                let mut launch_list: Vec<Vec<String>> = Vec::new();
                for group in &settings.app_groups {
                    let id = 100 + launch_list.len() as u32;
                    launch_list.push(group.executable_paths.clone());
                    blocks.push(FlyoutBlock::ActionRow {
                        id,
                        title: group.name.clone(),
                        subtitle: Some(format!("{} app(s)", group.executable_paths.len())),
                        badge: Some("Launch".to_owned()),
                        icon: None,
                        destructive: false,
                    });
                }
                if blocks.is_empty() {
                    blocks.push(FlyoutBlock::Note("No app groups configured.".to_owned()));
                }
                let spec = FlyoutSpec {
                    title: "App Groups".to_owned(),
                    subtitle: Some("Click a group to launch its apps".to_owned()),
                    width_dips: 290.0,
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
                false
            }
            _ => false,
        }
    }
}
