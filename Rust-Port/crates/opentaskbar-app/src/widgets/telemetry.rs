//! Native Win32 telemetry providers for all 23 dock widgets.
//!
//! Includes CPU/RAM, Core Audio volume, media transport keys, network throughput (`GetIfTable2`),
//! system battery, Bluetooth PnP device battery, fixed storage drives, keyboard layout switcher,
//! day progress, cached weather (`wttr.in`), Recycle Bin status, and AI CLI credential status.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
};
use windows_sys::Win32::UI::Shell::{
    SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW, SHQUERYRBINFO,
    SHQueryRecycleBinW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
        (self.sample_cpu(), sample_ram_info().0)
    }

    fn sample_cpu(&mut self) -> u32 {
        let (mut idle, mut kernel, mut user) = (
            FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
        );
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return 0;
        }
        let (iv, kv, uv) = (ft_to_u64(idle), ft_to_u64(kernel), ft_to_u64(user));
        let pct = if self.prev_kernel != 0 || self.prev_user != 0 {
            let total = kv
                .saturating_sub(self.prev_kernel)
                .saturating_add(uv.saturating_sub(self.prev_user));
            if total > 0 {
                let busy = total.saturating_sub(iv.saturating_sub(self.prev_idle));
                ((busy.saturating_mul(100)) / total).min(100) as u32
            } else {
                0
            }
        } else {
            0
        };
        (self.prev_idle, self.prev_kernel, self.prev_user) = (iv, kv, uv);
        pct
    }
}

pub fn sample_ram_info() -> (u32, f32, f32) {
    let mut mem: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    mem.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut mem) } != 0 {
        let total = mem.ullTotalPhys as f64 / 1_073_741_824.0;
        let used = (total - mem.ullAvailPhys as f64 / 1_073_741_824.0).max(0.0);
        (mem.dwMemoryLoad.min(100), used as f32, total as f32)
    } else {
        (0, 0.0, 0.0)
    }
}

pub fn local_clock_strings() -> (String, String) {
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        GetLocalTime(&mut st);
    }
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let dow = DAYS.get(usize::from(st.wDayOfWeek)).copied().unwrap_or("");
    let mon = MONTHS
        .get(usize::from(st.wMonth.saturating_sub(1)))
        .copied()
        .unwrap_or("");
    (
        format!("{:02}:{:02}", st.wHour, st.wMinute),
        format!("{dow}, {} {mon}", st.wDay),
    )
}

pub fn system_uptime_string() -> String {
    let secs = unsafe { GetTickCount64() } / 1000;
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else {
        format!("{h}h {m}m")
    }
}

static RECYCLE_BIN_FULL: AtomicBool = AtomicBool::new(false);
static RECYCLE_BIN_QUERYING: AtomicBool = AtomicBool::new(false);

/// Non-blocking Recycle Bin check: returns cached state immediately and refreshes on a background
/// thread so slow/sleeping drives never stall dock startup or UI frames.
pub fn is_recycle_bin_non_empty() -> bool {
    if !RECYCLE_BIN_QUERYING.swap(true, Ordering::AcqRel) {
        let _ = std::thread::Builder::new()
            .name("opentaskbar-bin-query".into())
            .spawn(|| {
                let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
                info.cbSize = size_of::<SHQUERYRBINFO>() as u32;
                let full = unsafe {
                    SHQueryRecycleBinW(std::ptr::null(), &mut info) == 0 && info.i64NumItems > 0
                };
                RECYCLE_BIN_FULL.store(full, Ordering::Release);
                RECYCLE_BIN_QUERYING.store(false, Ordering::Release);
            });
    }
    RECYCLE_BIN_FULL.load(Ordering::Acquire)
}

pub fn empty_recycle_bin() {
    let _ = std::thread::Builder::new()
        .name("opentaskbar-empty-bin".into())
        .spawn(|| unsafe {
            let _ = SHEmptyRecycleBinW(
                std::ptr::null_mut(),
                std::ptr::null(),
                SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
            );
        });
}

// --- Core Audio ---

fn endpoint_volume() -> Option<IAudioEndpointVolume> {
    unsafe {
        let en: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
        dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()
    }
}

pub fn get_audio_state() -> (u32, bool) {
    let Some(ep) = endpoint_volume() else {
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
    let Some(ep) = endpoint_volume() else {
        return;
    };
    unsafe {
        let next = (ep.GetMasterVolumeLevelScalar().unwrap_or(0.5) + delta_pct as f32 / 100.0)
            .clamp(0.0, 1.0);
        let _ = ep.SetMasterVolumeLevelScalar(next, std::ptr::null());
        if delta_pct > 0 {
            let _ = ep.SetMute(false, std::ptr::null());
        }
    }
}

pub fn set_audio_volume(pct: u32) {
    let Some(ep) = endpoint_volume() else {
        return;
    };
    let scalar = (pct.min(100) as f32) / 100.0;
    unsafe {
        let _ = ep.SetMasterVolumeLevelScalar(scalar, std::ptr::null());
        if pct > 0 {
            let _ = ep.SetMute(false, std::ptr::null());
        }
    }
}

pub fn toggle_audio_mute() {
    let Some(ep) = endpoint_volume() else {
        return;
    };
    unsafe {
        let muted = ep.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        let _ = ep.SetMute(!muted, std::ptr::null());
    }
}

pub fn send_media_vk(vk: u16) {
    let mk = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [mk(0), mk(KEYEVENTF_KEYUP)];
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        );
    }
}

// --- Network Speed ---

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
        (self.prev_rx, self.prev_tx, self.prev_at) = (rx, tx, Some(now));
        (self.rx_bps, self.tx_bps)
    }
}

fn read_total_net_bytes() -> (u64, u64) {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    unsafe {
        if GetIfTable2(&mut table) != 0 || table.is_null() {
            return (0, 0);
        }
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let (mut rx, mut tx) = (0u64, 0u64);
        for r in rows {
            if r.OperStatus == IfOperStatusUp && r.Type != 24 {
                rx = rx.saturating_add(r.InOctets);
                tx = tx.saturating_add(r.OutOctets);
            }
        }
        FreeMibTable(table.cast());
        (rx, tx)
    }
}

pub fn format_speed(bps: u64) -> String {
    if bps >= 1_048_576 {
        format!("{:.1} MB/s", bps as f64 / 1_048_576.0)
    } else if bps >= 1024 {
        format!("{} KB/s", bps / 1024)
    } else {
        format!("{bps} B/s")
    }
}

// --- System & Bluetooth Battery ---

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
    BatteryInfo {
        has_battery,
        percent: if has_battery {
            u32::from(st.BatteryLifePercent)
        } else {
            100
        },
        on_ac: st.ACLineStatus == 1,
        charging: (st.BatteryFlag & 8) != 0,
        life_secs: (st.BatteryLifeTime != u32::MAX).then_some(st.BatteryLifeTime),
    }
}

#[derive(Clone, Debug)]
pub struct BluetoothBatteryDevice {
    pub name: String,
    pub percent: u32,
}

#[derive(Clone, Default)]
pub struct BluetoothBatteryCache {
    pub devices: Vec<BluetoothBatteryDevice>,
    pub last_fetch: Option<Instant>,
    pub loading: bool,
}

/// Non-blocking PnP Bluetooth battery query matching C# `{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2`.
pub fn refresh_bluetooth_battery(cache: &Arc<Mutex<BluetoothBatteryCache>>, force: bool) {
    let should = if let Ok(mut g) = cache.lock() {
        let stale = g.last_fetch.is_none_or(|t| t.elapsed().as_secs() >= 60);
        if !g.loading && (force || stale) {
            g.loading = true;
            true
        } else {
            false
        }
    } else {
        false
    };
    if !should {
        return;
    }
    let cache = Arc::clone(cache);
    let _ = std::thread::Builder::new()
        .name("opentaskbar-bt-bat".into())
        .spawn(move || {
            let ps = r#"Get-PnpDevice -Class Bluetooth -Status OK -ErrorAction SilentlyContinue | ForEach-Object { $p = Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName '{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2' -ErrorAction SilentlyContinue; if ($p -and $null -ne $p.Data) { "$($_.FriendlyName)|$($p.Data)" } }"#;
            let output = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", ps])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
            let mut found = Vec::new();
            if let Ok(out) = output
                && out.status.success()
            {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Some((name, pct)) = line.trim().rsplit_once('|')
                        && let Ok(val) = pct.trim().parse::<u32>()
                        && val <= 100
                        && !name.trim().is_empty()
                    {
                        found.push(BluetoothBatteryDevice {
                            name: name.trim().to_owned(),
                            percent: val,
                        });
                    }
                }
            }
            if let Ok(mut g) = cache.lock() {
                g.devices = found;
                g.loading = false;
                g.last_fetch = Some(Instant::now());
            }
        });
}

// --- Storage ---

pub struct DriveSpace {
    pub letter: char,
    pub free_gb: f64,
    pub total_gb: f64,
}

pub fn sample_drives() -> Vec<DriveSpace> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u32)
        .filter(|i| (mask & (1 << i)) != 0)
        .filter_map(|i| {
            let letter = (b'A' + i as u8) as char;
            let root: Vec<u16> = format!("{letter}:\\\0").encode_utf16().collect();
            let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
            let ok =
                unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut avail, &mut total, &mut free) };
            (ok != 0 && total > 0).then(|| DriveSpace {
                letter,
                free_gb: avail as f64 / 1_073_741_824.0,
                total_gb: total as f64 / 1_073_741_824.0,
            })
        })
        .collect()
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
        hkl_iso_code(GetKeyboardLayout(tid) as isize)
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

pub fn activate_keyboard_layout(hkl: isize) {
    unsafe {
        ActivateKeyboardLayout(hkl as _, 0);
    }
}

pub fn cycle_keyboard_layout() {
    let layouts = installed_keyboard_layouts();
    if layouts.len() < 2 {
        return;
    }
    let active = active_keyboard_code();
    let idx = layouts
        .iter()
        .position(|(_, code)| code == &active)
        .unwrap_or(0);
    let next = (idx + 1) % layouts.len();
    activate_keyboard_layout(layouts[next].0);
}

// --- Day Progress ---

pub fn day_progress_metrics() -> (f32, f32, f32, u32) {
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        GetLocalTime(&mut st);
    }
    let mins = u32::from(st.wHour) * 60 + u32::from(st.wMinute);
    let day = (mins as f32 / 1440.0).clamp(0.0, 1.0);
    let dow = if st.wDayOfWeek == 0 {
        6
    } else {
        u32::from(st.wDayOfWeek - 1)
    };
    let week = ((dow as f32 + day) / 7.0).clamp(0.0, 1.0);
    let doy = (u32::from(st.wMonth.saturating_sub(1)) * 30 + u32::from(st.wDay)).min(365);
    (
        day,
        week,
        (doy as f32 / 365.0).clamp(0.0, 1.0),
        (1440u32.saturating_sub(mins)) / 60,
    )
}

// --- Weather ---

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

pub fn refresh_weather(cache: &Arc<Mutex<WeatherCache>>, city: &str, force: bool) {
    let city = if city.trim().is_empty() {
        "Istanbul"
    } else {
        city.trim()
    };
    let should = if let Ok(mut g) = cache.lock() {
        let stale = g.last_fetch.is_none_or(|t| t.elapsed().as_secs() >= 1800);
        let changed = !g.city.eq_ignore_ascii_case(city);
        if !g.loading && (force || stale || changed) {
            g.loading = true;
            g.city = city.to_owned();
            true
        } else {
            false
        }
    } else {
        false
    };
    if !should {
        return;
    }
    let cache = Arc::clone(cache);
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
            if let Ok(mut g) = cache.lock() {
                g.loading = false;
                g.last_fetch = Some(Instant::now());
                if let Ok(out) = output
                    && out.status.success()
                {
                    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                    let parts: Vec<&str> = text.split('|').collect();
                    if parts.len() >= 4 {
                        g.temp_c = parts[0].trim().trim_start_matches('+').to_owned();
                        g.condition = parts[1].trim().to_owned();
                        g.feels_like = parts[2].trim().trim_start_matches('+').to_owned();
                        g.humidity = parts[3].trim().to_owned();
                    }
                }
            }
        });
}

// --- AI Provider Local Session Presence (never reads/exposes tokens) ---

pub struct AiProviderMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub short: &'static str,
    pub dashboard_url: &'static str,
}

pub const AI_PROVIDERS: &[AiProviderMeta] = &[
    AiProviderMeta {
        id: "aiusage_claude",
        name: "Claude",
        short: "CLAUDE",
        dashboard_url: "https://claude.ai/settings/usage",
    },
    AiProviderMeta {
        id: "aiusage_codex",
        name: "Codex",
        short: "CODEX",
        dashboard_url: "https://chatgpt.com/codex/settings/usage",
    },
    AiProviderMeta {
        id: "aiusage_antigravity",
        name: "Antigravity",
        short: "AGY",
        dashboard_url: "https://aistudio.google.com/",
    },
    AiProviderMeta {
        id: "aiusage_opencode",
        name: "OpenCode",
        short: "OPENC",
        dashboard_url: "https://opencode.ai/",
    },
    AiProviderMeta {
        id: "aiusage_copilot",
        name: "Copilot",
        short: "COPILOT",
        dashboard_url: "https://github.com/settings/copilot",
    },
    AiProviderMeta {
        id: "aiusage_deepseek",
        name: "DeepSeek",
        short: "DEEP",
        dashboard_url: "https://platform.deepseek.com/usage",
    },
    AiProviderMeta {
        id: "aiusage_openrouter",
        name: "OpenRouter",
        short: "OR",
        dashboard_url: "https://openrouter.ai/activity",
    },
    AiProviderMeta {
        id: "aiusage_cursor",
        name: "Cursor",
        short: "CURSOR",
        dashboard_url: "https://www.cursor.com/settings",
    },
];

pub fn ai_provider_meta(id: &str) -> Option<&'static AiProviderMeta> {
    AI_PROVIDERS.iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

/// Checks only whether a local CLI/app config directory exists for `provider_id` (no secrets read).
pub fn ai_provider_detected(provider_id: &str) -> bool {
    let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) else {
        return false;
    };
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    match provider_id {
        "aiusage_claude" => home.join(".claude").exists(),
        "aiusage_codex" => home.join(".codex").exists(),
        "aiusage_antigravity" => home.join(".gemini").join("antigravity-cli").exists(),
        "aiusage_opencode" => {
            home.join(".local").join("share").join("opencode").exists()
                || appdata.is_some_and(|d| d.join("opencode").exists())
        }
        "aiusage_copilot" => {
            home.join(".config").join("github-copilot").exists()
                || local.is_some_and(|d| d.join("github-copilot").exists())
        }
        "aiusage_deepseek" => std::env::var_os("DEEPSEEK_API_KEY").is_some(),
        "aiusage_openrouter" => std::env::var_os("OPENROUTER_API_KEY").is_some(),
        "aiusage_cursor" => appdata.is_some_and(|d| d.join("Cursor").exists()),
        _ => false,
    }
}
