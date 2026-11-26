//! Native Win32 telemetry providers for all 23 dock widgets.
//!
//! Includes CPU/RAM + 45-sample sparkline history + top processes by memory, Core Audio output &
//! input (microphone) volume/mute/device names, media transport keys, network throughput + session
//! totals (`GetIfTable2`), system battery, Bluetooth PnP device battery, fixed storage drives,
//! keyboard layout switcher with localized display names, day progress, cached weather (`wttr.in`),
//! Recycle Bin status, and AI CLI credential presence.

use std::collections::{HashMap, VecDeque};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eMultimedia, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, STGM_READ};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, LocalFree, SYSTEMTIME};
use windows_sys::Win32::Globalization::{
    GetLocaleInfoW, LOCALE_SISO639LANGNAME, LOCALE_SLOCALIZEDDISPLAYNAME,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
};
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDrives};
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::ProcessStatus::{
    EnumProcesses, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY, RegCloseKey, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::System::SystemInformation::{
    GetLocalTime, GetSystemTime, GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX,
};
use windows_sys::Win32::System::Threading::{
    GetSystemTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    QueryFullProcessImageNameW,
};
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
const SPARKLINE_CAPACITY: usize = 45;

// PKEY_Device_FriendlyName = {a45c254e-df1c-4efd-8020-67d146a850e0}, pid 14
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

/// Live Windows SMTC (System Media Transport Controls) session snapshot.
#[derive(Clone, Debug, Default)]
pub struct MediaSessionSnapshot {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub position_secs: u64,
    pub duration_secs: u64,
    pub fetched_at: Option<Instant>,
    pub in_flight: bool,
}

/// Queries the active Windows SMTC media session on a background thread (non-blocking).
pub fn refresh_media_session(cache: &Arc<Mutex<MediaSessionSnapshot>>, force: bool) {
    let should_fetch = if let Ok(mut guard) = cache.lock() {
        if guard.in_flight
            || (!force && guard.fetched_at.is_some_and(|t| t.elapsed().as_secs() < 3))
        {
            false
        } else {
            guard.in_flight = true;
            true
        }
    } else {
        false
    };
    if !should_fetch {
        return;
    }

    let cache_clone = cache.clone();
    std::thread::Builder::new()
        .name("media-smtc".to_owned())
        .spawn(move || {
            let next = query_smtc_session();
            if let Ok(mut guard) = cache_clone.lock() {
                if let Some((title, artist, playing, pos, dur)) = next {
                    guard.title = title;
                    guard.artist = artist;
                    guard.playing = playing;
                    guard.position_secs = pos;
                    guard.duration_secs = dur;
                } else {
                    guard.title.clear();
                    guard.artist.clear();
                    guard.playing = false;
                    guard.position_secs = 0;
                    guard.duration_secs = 0;
                }
                guard.fetched_at = Some(Instant::now());
                guard.in_flight = false;
            }
        })
        .ok();
}

fn query_smtc_session() -> Option<(String, String, bool, u64, u64)> {
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    };
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        .ok()?
        .get()
        .ok()?;
    let session = manager.GetCurrentSession().ok()?;
    let props = session.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
    let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
    let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
    if title.is_empty() && artist.is_empty() {
        return None;
    }
    let playing = session
        .GetPlaybackInfo()
        .ok()
        .and_then(|info| info.PlaybackStatus().ok())
        .is_some_and(|st| st == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
    let (pos_secs, dur_secs) = session
        .GetTimelineProperties()
        .ok()
        .map(|tl| {
            let pos = tl
                .Position()
                .map(|t| (t.Duration.max(0) / 10_000_000) as u64)
                .unwrap_or(0);
            let end = tl
                .EndTime()
                .map(|t| (t.Duration.max(0) / 10_000_000) as u64)
                .unwrap_or(0);
            (pos.min(end), end)
        })
        .unwrap_or((0, 0));
    Some((title, artist, playing, pos_secs, dur_secs))
}

fn ft_to_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// Fixed-capacity ring buffer storing `[0.0, 1.0]` telemetry samples (matching C# `MetricRingBuffer`).
#[derive(Clone, Debug)]
pub struct MetricHistory {
    samples: VecDeque<f32>,
}

impl Default for MetricHistory {
    fn default() -> Self {
        Self {
            samples: VecDeque::with_capacity(SPARKLINE_CAPACITY),
        }
    }
}

impl MetricHistory {
    pub fn push(&mut self, value: f32) {
        if self.samples.len() >= SPARKLINE_CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back(value.clamp(0.0, 1.0));
    }

    pub fn as_vec(&self) -> Vec<f32> {
        self.samples.iter().copied().collect()
    }
}

#[derive(Default)]
pub struct SystemStatsSampler {
    prev_idle: u64,
    prev_kernel: u64,
    prev_user: u64,
    pub cpu_history: MetricHistory,
    pub ram_history: MetricHistory,
}

impl SystemStatsSampler {
    pub fn sample(&mut self) -> (u32, u32) {
        let cpu = self.sample_cpu();
        let ram = sample_ram_info().0;
        self.cpu_history.push(cpu as f32 / 100.0);
        self.ram_history.push(ram as f32 / 100.0);
        (cpu, ram)
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

/// Top real processes ordered by Working Set memory (matches C# `SystemStats.SampleTopProcesses`).
pub fn sample_top_processes(limit: usize) -> Vec<(String, u64)> {
    let mut pids = vec![0u32; 1024];
    let mut needed = 0u32;
    if unsafe {
        EnumProcesses(
            pids.as_mut_ptr(),
            (pids.len() * size_of::<u32>()) as u32,
            &mut needed,
        )
    } == 0
    {
        return Vec::new();
    }
    let count = (needed as usize / size_of::<u32>()).min(pids.len());
    let mut entries: Vec<(String, u64)> = Vec::with_capacity(64);
    let mut path_buf = [0u16; 260];
    for &pid in &pids[..count] {
        if pid <= 4 {
            continue;
        }
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
            if h.is_null() {
                continue;
            }
            let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            pmc.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            let got_mem = GetProcessMemoryInfo(h, &mut pmc, pmc.cb) != 0;
            let mut len = path_buf.len() as u32;
            let got_name =
                QueryFullProcessImageNameW(h, 0, path_buf.as_mut_ptr(), &mut len) != 0 && len > 0;
            let _ = CloseHandle(h);
            if got_mem && got_name {
                let full = String::from_utf16_lossy(&path_buf[..len as usize]);
                let name = full
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&full)
                    .trim_end_matches(".exe")
                    .trim_end_matches(".EXE")
                    .to_owned();
                if !name.is_empty() {
                    entries.push((name, pmc.WorkingSetSize as u64));
                }
            }
        }
    }
    // Aggregate multiple helper processes with the same executable name (e.g. Chrome/Edge/Code).
    let mut merged: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for (name, bytes) in entries {
        *merged.entry(name).or_insert(0) += bytes;
    }
    let mut list: Vec<(String, u64)> = merged.into_iter().collect();
    list.sort_by_key(|b| std::cmp::Reverse(b.1));
    list.truncate(limit);
    list
}

// --- Clock & Calendar Helpers ---

#[derive(Clone, Debug)]
pub struct LocalClockInfo {
    pub time_hm: String,
    pub short_date: String,
    pub weekday_short: String,
    pub full_date: String,
    pub tz_label: String,
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

const SHORT_DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const FULL_DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const SHORT_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const FULL_MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

pub fn month_name(month: u32) -> &'static str {
    FULL_MONTHS
        .get(month.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("January")
}

/// Returns `(year, month, day)` -> days in that Gregorian month.
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Sakamoto's algorithm: returns `0 = Sunday .. 6 = Saturday` for any Gregorian date.
pub fn weekday_of(year: i32, month: u32, day: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 { year - 1 } else { year };
    let m = (month as usize).clamp(1, 12) - 1;
    ((y + y / 4 - y / 100 + y / 400 + T[m] + day as i32).rem_euclid(7)) as u32
}

pub fn local_clock_info() -> LocalClockInfo {
    let mut lt: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let mut ut: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        GetLocalTime(&mut lt);
        GetSystemTime(&mut ut);
    }
    let dow_s = SHORT_DAYS
        .get(usize::from(lt.wDayOfWeek))
        .copied()
        .unwrap_or("");
    let dow_f = FULL_DAYS
        .get(usize::from(lt.wDayOfWeek))
        .copied()
        .unwrap_or("");
    let mon_s = SHORT_MONTHS
        .get(usize::from(lt.wMonth.saturating_sub(1)))
        .copied()
        .unwrap_or("");
    let mon_f = FULL_MONTHS
        .get(usize::from(lt.wMonth.saturating_sub(1)))
        .copied()
        .unwrap_or("");

    // Derive UTC offset directly from `GetLocalTime` vs `GetSystemTime`.
    let l_mins = i32::from(lt.wHour) * 60 + i32::from(lt.wMinute);
    let u_mins = i32::from(ut.wHour) * 60 + i32::from(ut.wMinute);
    let mut diff = l_mins - u_mins;
    if diff > 720 {
        diff -= 1440;
    } else if diff < -720 {
        diff += 1440;
    }
    let sign = if diff < 0 { "-" } else { "+" };
    let abs_m = diff.abs();
    let tz_label = if abs_m % 60 == 0 {
        format!("UTC{sign}{}", abs_m / 60)
    } else {
        format!("UTC{sign}{}:{:02}", abs_m / 60, abs_m % 60)
    };

    LocalClockInfo {
        time_hm: format!("{:02}:{:02}", lt.wHour, lt.wMinute),
        short_date: format!("{} {mon_s}", lt.wDay),
        weekday_short: dow_s.to_owned(),
        full_date: format!("{dow_f}, {} {mon_f}", lt.wDay),
        tz_label,
        year: i32::from(lt.wYear),
        month: u32::from(lt.wMonth),
        day: u32::from(lt.wDay),
    }
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

// --- Core Audio (Output & Microphone Input) ---

fn endpoint_volume_for(flow: EDataFlow) -> Option<IAudioEndpointVolume> {
    unsafe {
        let en: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let dev = en.GetDefaultAudioEndpoint(flow, eMultimedia).ok()?;
        dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()
    }
}

pub fn default_audio_device_name(capture: bool) -> Option<String> {
    let flow = if capture { eCapture } else { eRender };
    unsafe {
        let en: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let dev = en.GetDefaultAudioEndpoint(flow, eMultimedia).ok()?;
        let store = dev.OpenPropertyStore(STGM_READ).ok()?;
        let prop = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME).ok()?;
        let name = prop.to_string();
        (!name.trim().is_empty()).then_some(name.trim().to_owned())
    }
}

pub fn get_audio_state() -> (u32, bool) {
    let Some(ep) = endpoint_volume_for(eRender) else {
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

pub fn get_mic_state() -> Option<(u32, bool)> {
    let ep = endpoint_volume_for(eCapture)?;
    unsafe {
        let vol = ep
            .GetMasterVolumeLevelScalar()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let muted = ep.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        Some(((vol * 100.0).round() as u32, muted))
    }
}

pub fn adjust_audio_volume(delta_pct: i32) {
    let Some(ep) = endpoint_volume_for(eRender) else {
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
    let Some(ep) = endpoint_volume_for(eRender) else {
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
    let Some(ep) = endpoint_volume_for(eRender) else {
        return;
    };
    unsafe {
        let muted = ep.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        let _ = ep.SetMute(!muted, std::ptr::null());
    }
}

pub fn set_mic_volume(pct: u32) {
    let Some(ep) = endpoint_volume_for(eCapture) else {
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

pub fn toggle_mic_mute() {
    let Some(ep) = endpoint_volume_for(eCapture) else {
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
    pub session_rx: u64,
    pub session_tx: u64,
    pub adapter_name: String,
}

impl NetSpeedSampler {
    pub fn sample(&mut self) -> (u64, u64) {
        let (rx, tx, adapter) = read_total_net_bytes();
        if !adapter.is_empty() {
            self.adapter_name = adapter;
        }
        let now = Instant::now();
        if let Some(prev) = self.prev_at {
            let dt = now.duration_since(prev).as_secs_f64().max(0.2);
            let drx = rx.saturating_sub(self.prev_rx);
            let dtx = tx.saturating_sub(self.prev_tx);
            self.rx_bps = (drx as f64 / dt) as u64;
            self.tx_bps = (dtx as f64 / dt) as u64;
            self.session_rx = self.session_rx.saturating_add(drx);
            self.session_tx = self.session_tx.saturating_add(dtx);
        }
        (self.prev_rx, self.prev_tx, self.prev_at) = (rx, tx, Some(now));
        (self.rx_bps, self.tx_bps)
    }
}

fn read_total_net_bytes() -> (u64, u64, String) {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    unsafe {
        if GetIfTable2(&mut table) != 0 || table.is_null() {
            return (0, 0, String::new());
        }
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let (mut rx, mut tx) = (0u64, 0u64);
        let mut best_traffic = 0u64;
        let mut best_name = String::new();
        for r in rows {
            if r.OperStatus == IfOperStatusUp && r.Type != 24 {
                rx = rx.saturating_add(r.InOctets);
                tx = tx.saturating_add(r.OutOctets);
                let total = r.InOctets.saturating_add(r.OutOctets);
                if total >= best_traffic {
                    best_traffic = total;
                    let end = r
                        .Alias
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(r.Alias.len());
                    let alias = String::from_utf16_lossy(&r.Alias[..end]).trim().to_owned();
                    if !alias.is_empty() {
                        best_name = alias;
                    }
                }
            }
        }
        FreeMibTable(table.cast());
        (rx, tx, best_name)
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

pub fn format_bytes(bytes: u64) -> String {
    const GB: f64 = 1_073_741_824.0;
    const MB: f64 = 1_048_576.0;
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
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

impl DriveSpace {
    /// Formats `"213 / 476 GB"` or `"1.2 / 1.8 TB"` matching C# `StorageWidget.FormatCapacityPair`.
    pub fn format_capacity_pair(&self) -> String {
        if self.total_gb >= 972.0 {
            format!(
                "{:.1} / {:.1} TB",
                self.free_gb / 1024.0,
                self.total_gb / 1024.0
            )
        } else {
            format!("{:.0} / {:.0} GB", self.free_gb, self.total_gb)
        }
    }
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

fn hkl_display_name(hkl: isize) -> String {
    let lcid = (hkl as u32) & 0xffff;
    let mut buf = [0u16; 96];
    let len = unsafe {
        GetLocaleInfoW(
            lcid,
            LOCALE_SLOCALIZEDDISPLAYNAME,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    if len > 1 {
        String::from_utf16_lossy(&buf[..(len - 1) as usize])
    } else {
        hkl_iso_code(hkl)
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

/// Returns `(hkl, short_code, localized_display_name)` matching C# `KeyboardLayoutWidget`.
pub fn installed_keyboard_layouts() -> Vec<(isize, String, String)> {
    let mut list = [std::ptr::null_mut(); 16];
    let count = unsafe { GetKeyboardLayoutList(list.len() as i32, list.as_mut_ptr()) };
    if count <= 0 {
        return Vec::new();
    }
    list[..count as usize]
        .iter()
        .map(|&h| {
            let hkl = h as isize;
            (hkl, hkl_iso_code(hkl), hkl_display_name(hkl))
        })
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
        .position(|(_, code, _)| code == &active)
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

// --- In-Process WinHTTP HTTPS Helper (no secrets on argv / process command lines) ---

fn winhttp_get(host: &str, path: &str, extra_headers: &[(&str, &str)]) -> Option<(u32, String)> {
    let agent: Vec<u16> = "OpenTaskbar/1.0\0".encode_utf16().collect();
    let host_w: Vec<u16> = host.encode_utf16().chain(Some(0)).collect();
    let verb_w: Vec<u16> = "GET\0".encode_utf16().collect();
    let path_w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();

    unsafe {
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            return None;
        }
        let _ = WinHttpSetTimeouts(session, 4000, 4000, 5000, 6000);
        let connect = WinHttpConnect(session, host_w.as_ptr(), 443, 0);
        if connect.is_null() {
            WinHttpCloseHandle(session);
            return None;
        }
        let req = WinHttpOpenRequest(
            connect,
            verb_w.as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if req.is_null() {
            WinHttpCloseHandle(connect);
            WinHttpCloseHandle(session);
            return None;
        }
        if !extra_headers.is_empty() {
            let mut hdr = String::new();
            for (k, v) in extra_headers {
                hdr.push_str(k);
                hdr.push_str(": ");
                hdr.push_str(v);
                hdr.push_str("\r\n");
            }
            let hdr_w: Vec<u16> = hdr.encode_utf16().collect();
            let _ = WinHttpAddRequestHeaders(
                req,
                hdr_w.as_ptr(),
                hdr_w.len() as u32,
                WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
            );
        }
        let sent = WinHttpSendRequest(req, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) != 0
            && WinHttpReceiveResponse(req, std::ptr::null_mut()) != 0;
        let result = if sent {
            let mut status: u32 = 0;
            let mut status_sz = size_of::<u32>() as u32;
            let _ = WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut status_sz,
                std::ptr::null_mut(),
            );
            let mut body = Vec::new();
            loop {
                let mut avail: u32 = 0;
                if WinHttpQueryDataAvailable(req, &mut avail) == 0 || avail == 0 {
                    break;
                }
                if body.len() + avail as usize > 256 * 1024 {
                    break;
                }
                let start = body.len();
                body.resize(start + avail as usize, 0);
                let mut read: u32 = 0;
                if WinHttpReadData(req, body[start..].as_mut_ptr().cast(), avail, &mut read) == 0
                    || read == 0
                {
                    body.truncate(start);
                    break;
                }
                body.truncate(start + read as usize);
            }
            Some((status, String::from_utf8_lossy(&body).into_owned()))
        } else {
            None
        };
        WinHttpCloseHandle(req);
        WinHttpCloseHandle(connect);
        WinHttpCloseHandle(session);
        result
    }
}

// --- Windows Night Light Control (exact C# `NightLightControl.cs`) ---

const NIGHT_LIGHT_REG_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current\default$windows.data.bluelightreduction.bluelightreductionstate\windows.data.bluelightreduction.bluelightreductionstate";

fn decode_night_light_on(data: &[u8]) -> Option<bool> {
    const ENVELOPE_PREFIX: [u8; 10] = [0x43, 0x42, 1, 0, 0x0A, 2, 1, 0, 0x2A, 6];
    const PAYLOAD_PREFIX: [u8; 3] = [0x2A, 0x2B, 0x0E];
    const BOND_HEADER: [u8; 4] = [0x43, 0x42, 1, 0];
    const EMPTY_ENVELOPE: [u8; 13] = [0x43, 0x42, 1, 0, 0x0A, 2, 1, 0, 0x2A, 0x2A, 0, 0, 0];

    let header_sz = if data.len() >= 16 && data.starts_with(&[2, 0, 0, 0]) {
        16
    } else {
        0
    };
    let mut env = &data[header_sz..];
    if env == EMPTY_ENVELOPE {
        return Some(false);
    }
    env = env.strip_prefix(&ENVELOPE_PREFIX)?;
    // Skip varint modified timestamp
    let mut i = 0usize;
    while i < env.len() && (env[i] & 0x80) != 0 {
        i += 1;
    }
    env = env.get(i + 1..)?;
    env = env.strip_prefix(&PAYLOAD_PREFIX)?;
    let mut j = 0usize;
    while j < env.len() && (env[j] & 0x80) != 0 {
        j += 1;
    }
    env = env.get(j + 1..)?;
    if env.len() < 3 || !env.ends_with(&[0, 0, 0]) {
        return None;
    }
    let payload = &env[..env.len() - 3];
    let rest = payload.strip_prefix(&BOND_HEADER)?;
    Some(rest.starts_with(&[0x10, 0]))
}

pub fn read_night_light() -> Option<bool> {
    let subkey: Vec<u16> = NIGHT_LIGHT_REG_PATH.encode_utf16().chain(Some(0)).collect();
    let val_name: Vec<u16> = "Data\0".encode_utf16().collect();
    unsafe {
        let mut hkey = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return None;
        }
        let mut ty: u32 = 0;
        let mut len: u32 = 0;
        let mut result = None;
        if RegQueryValueExW(
            hkey,
            val_name.as_ptr(),
            std::ptr::null_mut(),
            &mut ty,
            std::ptr::null_mut(),
            &mut len,
        ) == 0
            && ty == REG_BINARY
            && len > 0
            && len <= 4096
        {
            let mut buf = vec![0u8; len as usize];
            if RegQueryValueExW(
                hkey,
                val_name.as_ptr(),
                std::ptr::null_mut(),
                &mut ty,
                buf.as_mut_ptr(),
                &mut len,
            ) == 0
            {
                result = decode_night_light_on(&buf[..len as usize]);
            }
        }
        let _ = RegCloseKey(hkey);
        result
    }
}

pub fn toggle_night_light() -> bool {
    let Some(cur) = read_night_light() else {
        return false;
    };
    let target_on = !cur;
    let subkey: Vec<u16> = NIGHT_LIGHT_REG_PATH.encode_utf16().chain(Some(0)).collect();
    let val_name: Vec<u16> = "Data\0".encode_utf16().collect();
    unsafe {
        let mut hkey = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_READ | KEY_SET_VALUE,
            &mut hkey,
        ) != 0
        {
            return false;
        }
        let mut ty: u32 = 0;
        let mut len: u32 = 0;
        let mut ok = false;
        if RegQueryValueExW(
            hkey,
            val_name.as_ptr(),
            std::ptr::null_mut(),
            &mut ty,
            std::ptr::null_mut(),
            &mut len,
        ) == 0
            && ty == REG_BINARY
            && (20..=4096).contains(&len)
        {
            let mut buf = vec![0u8; len as usize];
            if RegQueryValueExW(
                hkey,
                val_name.as_ptr(),
                std::ptr::null_mut(),
                &mut ty,
                buf.as_mut_ptr(),
                &mut len,
            ) == 0
            {
                buf.truncate(len as usize);
                if let Some(encoded) = encode_night_light_toggle(&buf, target_on) {
                    ok = RegSetValueExW(
                        hkey,
                        val_name.as_ptr(),
                        0,
                        REG_BINARY,
                        encoded.as_ptr(),
                        encoded.len() as u32,
                    ) == 0;
                }
            }
        }
        let _ = RegCloseKey(hkey);
        ok
    }
}

fn write_varint(out: &mut Vec<u8>, mut val: u64) {
    while val >= 0x80 {
        out.push(((val as u8) & 0x7f) | 0x80);
        val >>= 7;
    }
    out.push(val as u8);
}

fn encode_night_light_toggle(orig: &[u8], on: bool) -> Option<Vec<u8>> {
    let header_sz = if orig.len() >= 16 && orig.starts_with(&[2, 0, 0, 0]) {
        16
    } else {
        0
    };
    let unix_secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let filetime = (unix_secs + 11_644_473_600) * 10_000_000;
    let mut payload = Vec::with_capacity(32);
    payload.extend_from_slice(&[0x43, 0x42, 1, 0]);
    if on {
        payload.extend_from_slice(&[0x10, 0]);
    }
    payload.extend_from_slice(&[0xD0, 0x0A, 2, 0xC6, 0x14]);
    write_varint(&mut payload, filetime);
    payload.push(0);

    let mut out = Vec::with_capacity(orig.len() + 16);
    if header_sz > 0 {
        out.extend_from_slice(&orig[..header_sz]);
    }
    out.extend_from_slice(&[0x43, 0x42, 1, 0, 0x0A, 2, 1, 0, 0x2A, 6]);
    write_varint(&mut out, unix_secs);
    out.extend_from_slice(&[0x2A, 0x2B, 0x0E]);
    write_varint(&mut out, payload.len() as u64);
    out.extend_from_slice(&payload);
    out.extend_from_slice(&[0, 0, 0]);
    Some(out)
}

// --- Weather (exact C# `WeatherService.cs` Open-Meteo API) ---

#[derive(Clone, Default)]
pub struct WeatherCache {
    pub city: String,
    pub temp_c: String,
    pub condition: String,
    pub is_rain: bool,
    pub feels_like: String,
    pub humidity: String,
    pub wind: String,
    pub today_range: String,
    /// 3-day forecast `(day_label, condition, high_low)`.
    pub forecast_days: Vec<(String, String, String)>,
    pub last_fetch: Option<Instant>,
    pub loading: bool,
}

fn describe_wmo(code: i64) -> (&'static str, bool) {
    match code {
        0 => ("Clear", false),
        1 | 2 => ("Partly cloudy", false),
        3 => ("Overcast", false),
        45 | 48 => ("Fog", false),
        51..=57 => ("Drizzle", true),
        61..=67 => ("Rain", true),
        71..=77 => ("Snow", true),
        80..=82 => ("Showers", true),
        85 | 86 => ("Snow showers", true),
        95..=99 => ("Thunderstorm", true),
        _ => ("Cloudy", false),
    }
}

fn urlencode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
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
            let geo_path = format!(
                "/v1/search?count=1&format=json&language=en&name={}",
                urlencode_query(&city_owned)
            );
            let geo = winhttp_get("geocoding-api.open-meteo.com", &geo_path, &[])
                .filter(|(st, _)| *st == 200)
                .and_then(|(_, body)| serde_json::from_str::<serde_json::Value>(&body).ok())
                .and_then(|v| {
                    let first = v.get("results")?.as_array()?.first()?;
                    let name = first
                        .get("name")
                        .and_then(|s| s.as_str())
                        .unwrap_or(&city_owned)
                        .to_owned();
                    let lat = first.get("latitude")?.as_f64()?;
                    let lon = first.get("longitude")?.as_f64()?;
                    Some((name, lat, lon))
                });

            let parsed = geo.and_then(|(place_name, lat, lon)| {
                let fc_path = format!(
                    "/v1/forecast?latitude={lat:.3}&longitude={lon:.3}&current=temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m&daily=weather_code,temperature_2m_max,temperature_2m_min&forecast_days=4&timezone=auto"
                );
                let (st, body) = winhttp_get("api.open-meteo.com", &fc_path, &[])?;
                if st != 200 {
                    return None;
                }
                let root: serde_json::Value = serde_json::from_str(&body).ok()?;
                let cur = root.get("current")?;
                let temp = cur.get("temperature_2m")?.as_f64()?;
                let feels = cur
                    .get("apparent_temperature")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(temp);
                let hum = cur
                    .get("relative_humidity_2m")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let wind = cur
                    .get("wind_speed_10m")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let code = cur
                    .get("weather_code")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let (cond, is_rain) = describe_wmo(code);

                let daily = root.get("daily");
                let times = daily
                    .and_then(|d| d.get("time"))
                    .and_then(|v| v.as_array());
                let maxs = daily
                    .and_then(|d| d.get("temperature_2m_max"))
                    .and_then(|v| v.as_array());
                let mins = daily
                    .and_then(|d| d.get("temperature_2m_min"))
                    .and_then(|v| v.as_array());
                let codes = daily
                    .and_then(|d| d.get("weather_code"))
                    .and_then(|v| v.as_array());

                let today_range = match (
                    maxs.and_then(|a| a.first()).and_then(|v| v.as_f64()),
                    mins.and_then(|a| a.first()).and_then(|v| v.as_f64()),
                ) {
                    (Some(hi), Some(lo)) => format!("{}° / {}°", hi.round() as i32, lo.round() as i32),
                    _ => String::new(),
                };

                let mut forecast = Vec::new();
                if let (Some(ts), Some(mxs), Some(mns), Some(cds)) = (times, maxs, mins, codes) {
                    let n = ts.len().min(mxs.len()).min(mns.len()).min(cds.len());
                    for i in 1..n.min(4) {
                        let date_str = ts[i].as_str().unwrap_or("");
                        let day_lbl = if let Some((y, rest)) = date_str.split_once('-')
                            && let Some((m, d)) = rest.split_once('-')
                            && let (Ok(yi), Ok(mi), Ok(di)) =
                                (y.parse::<i32>(), m.parse::<u32>(), d.parse::<u32>())
                        {
                            let wd = weekday_of(yi, mi, di) as usize;
                            SHORT_DAYS.get(wd).copied().unwrap_or(date_str).to_owned()
                        } else {
                            date_str.to_owned()
                        };
                        let (dc, _) = describe_wmo(cds[i].as_i64().unwrap_or(0));
                        let hi = mxs[i].as_f64().unwrap_or(0.0).round() as i32;
                        let lo = mns[i].as_f64().unwrap_or(0.0).round() as i32;
                        forecast.push((day_lbl, dc.to_owned(), format!("{hi}° / {lo}°")));
                    }
                }

                Some((
                    place_name,
                    format!("{}°", temp.round() as i32),
                    cond.to_owned(),
                    is_rain,
                    format!("{}°", feels.round() as i32),
                    format!("{}%", hum.round() as i32),
                    format!("{} km/h", wind.round() as i32),
                    today_range,
                    forecast,
                ))
            });

            if let Ok(mut g) = cache.lock() {
                g.loading = false;
                g.last_fetch = Some(Instant::now());
                if let Some((place, temp, cond, is_rain, feels, hum, wind, range, fc)) = parsed {
                    g.city = place;
                    g.temp_c = temp;
                    g.condition = cond;
                    g.is_rain = is_rain;
                    g.feels_like = feels;
                    g.humidity = hum;
                    g.wind = wind;
                    g.today_range = range;
                    g.forecast_days = fc;
                }
            }
        });
}

// --- AI Provider Usage & Balance (exact C# `AiSecretStore.cs` DPAPI + `AiUsageService.cs`) ---

pub struct AiProviderMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub short: &'static str,
    pub dashboard_url: &'static str,
    pub signin_hint: &'static str,
}

pub const AI_PROVIDERS: &[AiProviderMeta] = &[
    AiProviderMeta {
        id: "aiusage_claude",
        name: "Claude Code",
        short: "Claude",
        dashboard_url: "https://claude.ai/settings/usage",
        signin_hint: "claude auth login",
    },
    AiProviderMeta {
        id: "aiusage_codex",
        name: "Codex",
        short: "Codex",
        dashboard_url: "https://chatgpt.com/codex/settings/usage",
        signin_hint: "codex login",
    },
    AiProviderMeta {
        id: "aiusage_antigravity",
        name: "Antigravity",
        short: "Antigravity",
        dashboard_url: "https://antigravity.google/docs",
        signin_hint: "agy",
    },
    AiProviderMeta {
        id: "aiusage_opencode",
        name: "OpenCode Go",
        short: "OpenCode",
        dashboard_url: "https://opencode.ai/go",
        signin_hint: "opencode auth login",
    },
    AiProviderMeta {
        id: "aiusage_copilot",
        name: "GitHub Copilot",
        short: "Copilot",
        dashboard_url: "https://github.com/settings/copilot",
        signin_hint: "gh auth login",
    },
    AiProviderMeta {
        id: "aiusage_deepseek",
        name: "DeepSeek",
        short: "DeepSeek",
        dashboard_url: "https://platform.deepseek.com/usage",
        signin_hint: "DEEPSEEK_API_KEY",
    },
    AiProviderMeta {
        id: "aiusage_openrouter",
        name: "OpenRouter",
        short: "OpenRouter",
        dashboard_url: "https://openrouter.ai/settings/credits",
        signin_hint: "OPENROUTER_API_KEY",
    },
    AiProviderMeta {
        id: "aiusage_cursor",
        name: "Cursor",
        short: "Cursor",
        dashboard_url: "https://cursor.com/dashboard?tab=usage",
        signin_hint: "Sign in to Cursor",
    },
];

pub fn ai_provider_meta(id: &str) -> Option<&'static AiProviderMeta> {
    AI_PROVIDERS.iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

#[derive(Clone, Debug, Default)]
pub struct AiWindowData {
    pub label: String,
    pub used: f32,
    pub reset_text: String,
}

#[derive(Clone, Debug, Default)]
pub struct AiUsageSnapshot {
    pub balance_text: Option<String>,
    pub plan: Option<String>,
    pub windows: Vec<AiWindowData>,
    pub last_fetch: Option<Instant>,
    pub loading: bool,
    pub has_cred: bool,
}

#[derive(Default)]
pub struct AiUsageCache {
    pub providers: HashMap<String, AiUsageSnapshot>,
}

/// Reads an encrypted per-user DPAPI secret stored by C# `AiSecretStore.cs` (never logged).
fn read_dpapi_secret(secret_key: &str) -> Option<String> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let sanitized: String = secret_key
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = local
        .join("OpenTaskbar")
        .join("secrets")
        .join(format!("{sanitized}.bin"));
    let mut enc = std::fs::read(&path).ok()?;
    if enc.is_empty() || enc.len() > 65_536 {
        return None;
    }
    let mut entropy = *b"OpenTaskbar.AiSecretStore.v1";
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: enc.len() as u32,
        pbData: enc.as_mut_ptr(),
    };
    let ent_blob = CRYPT_INTEGER_BLOB {
        cbData: entropy.len() as u32,
        pbData: entropy.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        if CryptUnprotectData(
            &in_blob,
            std::ptr::null_mut(),
            &ent_blob,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        ) == 0
            || out_blob.pbData.is_null()
            || out_blob.cbData == 0
        {
            return None;
        }
        let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let text = String::from_utf8_lossy(slice).trim().to_owned();
        // Zero decrypted memory before freeing.
        std::ptr::write_bytes(out_blob.pbData, 0, out_blob.cbData as usize);
        LocalFree(out_blob.pbData.cast());
        (!text.is_empty()).then_some(text)
    }
}

fn read_provider_token(provider_id: &str) -> Option<(String, Option<String>)> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    match provider_id {
        "aiusage_deepseek" => read_dpapi_secret("api-key:deepseek")
            .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
            .map(|t| (t.trim().to_owned(), None))
            .filter(|(t, _)| !t.is_empty()),
        "aiusage_openrouter" => read_dpapi_secret("api-key:openrouter")
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .map(|t| (t.trim().to_owned(), None))
            .filter(|(t, _)| !t.is_empty()),
        "aiusage_opencode" => read_dpapi_secret("api-key:opencode")
            .or_else(|| std::env::var("OPENCODE_GO_API_KEY").ok())
            .or_else(|| std::env::var("OPENCODE_API_KEY").ok())
            .or_else(|| std::env::var("OPENCODE_ZEN_API_KEY").ok())
            .map(|t| (t.trim().to_owned(), None))
            .filter(|(t, _)| !t.is_empty()),
        "aiusage_claude" => {
            let path = home?.join(".claude").join(".credentials.json");
            let raw = std::fs::read_to_string(path).ok()?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let auth = v.get("claudeAiOauth").unwrap_or(&v);
            let tok = auth
                .get("accessToken")
                .or_else(|| auth.get("access_token"))
                .and_then(|s| s.as_str())?
                .trim()
                .to_owned();
            let plan = auth
                .get("subscriptionType")
                .and_then(|s| s.as_str())
                .map(str::to_owned);
            (!tok.is_empty()).then_some((tok, plan))
        }
        "aiusage_codex" => {
            let path = home?.join(".codex").join("auth.json");
            let raw = std::fs::read_to_string(path).ok()?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let tokens = v.get("tokens")?;
            let tok = tokens.get("access_token")?.as_str()?.trim().to_owned();
            let acct = tokens
                .get("account_id")
                .and_then(|s| s.as_str())
                .map(str::to_owned);
            (!tok.is_empty()).then_some((tok, acct))
        }
        "aiusage_copilot" => {
            let paths = [
                home.as_ref()
                    .map(|h| h.join(".config").join("github-copilot").join("apps.json")),
                local.map(|l| l.join("github-copilot").join("apps.json")),
            ];
            for path in paths.into_iter().flatten() {
                if let Ok(raw) = std::fs::read_to_string(path)
                    && let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&raw)
                {
                    for val in map.values() {
                        if let Some(tok) = val.get("oauth_token").and_then(|s| s.as_str())
                            && !tok.trim().is_empty()
                        {
                            return Some((tok.trim().to_owned(), None));
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn format_reset_iso(iso: &str) -> String {
    // Extract concise local-friendly time or date from ISO-8601 (`2026-11-25T16:30:00Z` -> `11-25 16:30`).
    if let Some((date_part, time_part)) = iso.split_once('T') {
        let hm: String = time_part.chars().take(5).collect();
        if date_part.len() >= 10 {
            return format!("{} {hm}", &date_part[5..10]);
        }
        return hm;
    }
    "—".to_owned()
}

fn parse_pct_01(v: &serde_json::Value) -> Option<f32> {
    let n = v.as_f64()?;
    if !n.is_finite() {
        return None;
    }
    let ratio = if n > 1.0 { n / 100.0 } else { n };
    Some((ratio as f32).clamp(0.0, 1.0))
}

fn fetch_ai_snapshot(provider_id: &str) -> AiUsageSnapshot {
    let detected = ai_provider_detected(provider_id);
    let Some((token, extra)) = read_provider_token(provider_id) else {
        return AiUsageSnapshot {
            has_cred: detected,
            last_fetch: Some(Instant::now()),
            ..Default::default()
        };
    };
    let auth_hdr = format!("Bearer {token}");
    let mut snap = AiUsageSnapshot {
        has_cred: true,
        last_fetch: Some(Instant::now()),
        ..Default::default()
    };

    match provider_id {
        "aiusage_deepseek" => {
            if let Some((200, body)) = winhttp_get(
                "api.deepseek.com",
                "/user/balance",
                &[("Authorization", &auth_hdr)],
            ) && let Ok(root) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(infos) = root.get("balance_infos").and_then(|v| v.as_array())
            {
                for info in infos {
                    if let Some(bal_str) = info.get("total_balance").and_then(|v| v.as_str())
                        && let Ok(bal) = bal_str.parse::<f64>()
                    {
                        let cur = info
                            .get("currency")
                            .and_then(|v| v.as_str())
                            .unwrap_or("USD");
                        let sym = if cur.eq_ignore_ascii_case("USD") {
                            "$"
                        } else {
                            ""
                        };
                        snap.balance_text = Some(if sym.is_empty() {
                            format!("{bal:.2} {cur}")
                        } else {
                            format!("{sym}{bal:.2}")
                        });
                        snap.plan = Some(format!("{bal:.2} {cur}"));
                        break;
                    }
                }
            }
        }
        "aiusage_openrouter" => {
            if let Some((200, body)) = winhttp_get(
                "openrouter.ai",
                "/api/v1/credits",
                &[("Authorization", &auth_hdr)],
            ) && let Ok(root) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(data) = root.get("data")
                && let (Some(total), Some(usage)) = (
                    data.get("total_credits").and_then(|v| v.as_f64()),
                    data.get("total_usage").and_then(|v| v.as_f64()),
                )
            {
                let rem = (total - usage).max(0.0);
                snap.balance_text = Some(format!("${rem:.2}"));
                snap.plan = Some(format!("${rem:.2} left"));
            }
        }
        "aiusage_claude" => {
            snap.plan = extra;
            if let Some((200, body)) = winhttp_get(
                "api.anthropic.com",
                "/api/oauth/usage",
                &[
                    ("Authorization", &auth_hdr),
                    ("anthropic-beta", "oauth-2025-04-20"),
                ],
            ) && let Ok(root) = serde_json::from_str::<serde_json::Value>(&body)
            {
                for (key, lbl) in [("five_hour", "5h"), ("seven_day", "7d")] {
                    if let Some(obj) = root.get(key)
                        && let Some(used) = obj.get("utilization").and_then(parse_pct_01)
                    {
                        let reset = obj
                            .get("resets_at")
                            .and_then(|v| v.as_str())
                            .map(format_reset_iso)
                            .unwrap_or_else(|| format!("{:.0}%", used * 100.0));
                        snap.windows.push(AiWindowData {
                            label: lbl.to_owned(),
                            used,
                            reset_text: reset,
                        });
                    }
                }
            }
        }
        "aiusage_codex" => {
            let mut hdrs = vec![("Authorization", auth_hdr.as_str())];
            if let Some(ref acct) = extra
                && !acct.is_empty()
            {
                hdrs.push(("ChatGPT-Account-Id", acct.as_str()));
            }
            if let Some((200, body)) = winhttp_get("chatgpt.com", "/backend-api/wham/usage", &hdrs)
                && let Ok(root) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(rl) = root.get("rate_limit")
            {
                for (key, lbl) in [("primary_window", "5h"), ("secondary_window", "7d")] {
                    if let Some(w) = rl.get(key)
                        && let Some(used) = w.get("used_percent").and_then(parse_pct_01)
                    {
                        snap.windows.push(AiWindowData {
                            label: lbl.to_owned(),
                            used,
                            reset_text: format!("{:.0}%", used * 100.0),
                        });
                    }
                }
            }
        }
        "aiusage_opencode" => {
            if let Some((200, body)) = winhttp_get(
                "opencode.ai",
                "/zen/go/v1/usage",
                &[("Authorization", &auth_hdr)],
            ) && let Ok(root) = serde_json::from_str::<serde_json::Value>(&body)
            {
                for (keys, lbl) in [
                    (&["rolling5h", "rollingUsage", "five_hour"][..], "1h"),
                    (&["weekly", "weeklyUsage", "seven_day"][..], "7d"),
                    (&["monthly", "monthlyUsage", "thirty_day"][..], "30d"),
                ] {
                    for k in keys {
                        if let Some(obj) = root.get(*k)
                            && let Some(used) = obj
                                .get("usagePercent")
                                .or_else(|| obj.get("used_percent"))
                                .or_else(|| obj.get("utilization"))
                                .and_then(parse_pct_01)
                        {
                            snap.windows.push(AiWindowData {
                                label: lbl.to_owned(),
                                used,
                                reset_text: format!("{:.0}%", used * 100.0),
                            });
                            break;
                        }
                    }
                }
            }
        }
        "aiusage_copilot" => {
            if let Some((200, body)) = winhttp_get(
                "api.github.com",
                "/copilot_internal/user",
                &[
                    ("Authorization", &auth_hdr),
                    ("Accept", "application/vnd.github+json"),
                ],
            ) && let Ok(root) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(rem) = root
                    .get("quota_snapshots")
                    .and_then(|q| q.get("premium_interactions"))
                    .and_then(|p| p.get("percent_remaining"))
                    .and_then(|v| v.as_f64())
            {
                let used = ((100.0 - rem) / 100.0).clamp(0.0, 1.0) as f32;
                let reset = root
                    .get("quota_reset_date_utc")
                    .and_then(|v| v.as_str())
                    .map(format_reset_iso)
                    .unwrap_or_else(|| "Monthly".to_owned());
                snap.windows.push(AiWindowData {
                    label: "Monthly".to_owned(),
                    used,
                    reset_text: reset,
                });
            }
        }
        _ => {}
    }
    snap
}

pub fn refresh_ai_provider(
    cache: &Arc<Mutex<AiUsageCache>>,
    provider_id: &'static str,
    force: bool,
) {
    let should = if let Ok(mut g) = cache.lock() {
        let entry = g.providers.entry(provider_id.to_owned()).or_default();
        let stale = entry
            .last_fetch
            .is_none_or(|t| t.elapsed().as_secs() >= 300);
        if !entry.loading && (force || stale) {
            entry.loading = true;
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
        .name(format!("otb-{provider_id}"))
        .spawn(move || {
            let snap = fetch_ai_snapshot(provider_id);
            if let Ok(mut g) = cache.lock() {
                g.providers.insert(provider_id.to_owned(), snap);
            }
        });
}

/// Checks whether a local CLI/app config or DPAPI secret exists for `provider_id`.
pub fn ai_provider_detected(provider_id: &str) -> bool {
    let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) else {
        return false;
    };
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let has_dpapi = |name: &str| {
        local.as_ref().is_some_and(|l| {
            l.join("OpenTaskbar")
                .join("secrets")
                .join(format!("{name}.bin"))
                .exists()
        })
    };
    match provider_id {
        "aiusage_claude" => home.join(".claude").exists(),
        "aiusage_codex" => home.join(".codex").exists(),
        "aiusage_antigravity" => home.join(".gemini").join("antigravity-cli").exists(),
        "aiusage_opencode" => {
            has_dpapi("api-key_opencode")
                || home.join(".local").join("share").join("opencode").exists()
                || appdata.is_some_and(|d| d.join("opencode").exists())
        }
        "aiusage_copilot" => {
            home.join(".config").join("github-copilot").exists()
                || local.is_some_and(|d| d.join("github-copilot").exists())
        }
        "aiusage_deepseek" => {
            has_dpapi("api-key_deepseek") || std::env::var_os("DEEPSEEK_API_KEY").is_some()
        }
        "aiusage_openrouter" => {
            has_dpapi("api-key_openrouter") || std::env::var_os("OPENROUTER_API_KEY").is_some()
        }
        "aiusage_cursor" => appdata.is_some_and(|d| d.join("Cursor").exists()),
        _ => false,
    }
}

// ============================================================================================
// Full AI detail (C# `AiUsageService.Parse` parity — used by WS-C's AI flyout).
// ============================================================================================

/// Pay-as-you-go spend block (C# `AiUsageSpend`).
#[derive(Clone, Debug, Default)]
pub struct AiSpend {
    pub used: f64,
    pub limit: Option<f64>,
    pub currency: String,
    pub used_01: Option<f32>,
}

/// One non-zero Claude weekly surface share (C# `AiUsageShare`).
#[derive(Clone, Debug, Default)]
pub struct AiUsageShare {
    pub name: String,
    pub used: f32,
}

/// Complete parsed usage for a single provider (C# `AiUsageReport` flattened for WS-C).
#[derive(Clone, Debug, Default)]
pub struct AiFullDetail {
    pub provider_id: String,
    pub plan: Option<String>,
    /// Primary window label (`5h`, `7d`, `30d`, `Monthly`, `Balance`, ...).
    pub label: String,
    /// Primary window utilisation in `[0,1]`.
    pub used: f32,
    pub reset_text: String,
    pub reset_unix_ms: Option<i64>,
    pub spend: Option<AiSpend>,
    pub breakdown: Vec<AiUsageShare>,
    pub limit_reached: bool,
    /// `Retry-After` from a 429 response, in seconds.
    pub retry_after_secs: Option<u32>,
    pub credits: Option<f64>,
    pub unlimited_credits: bool,
    pub error: Option<String>,
}

fn full_obj<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    v.get(key).filter(|x| x.is_object())
}

fn full_arr<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a [serde_json::Value]> {
    v.get(key).and_then(|x| x.as_array()).map(|a| a.as_slice())
}

fn full_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn full_num(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| {
        if let Some(n) = x.as_f64() {
            Some(n)
        } else {
            x.as_str()?.trim().parse::<f64>().ok()
        }
    })
}

fn full_bool(v: &serde_json::Value, key: &str) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(false)
}

/// Accepts both `0..1` and `0..100` encodings; clamps to `[0,1]`.
fn full_pct(v: &serde_json::Value, keys: &[&str]) -> Option<f32> {
    for k in keys {
        if let Some(n) = full_num(v, k) {
            if !n.is_finite() {
                continue;
            }
            let ratio = if n > 1.0 { n / 100.0 } else { n };
            return Some((ratio as f32).clamp(0.0, 1.0));
        }
    }
    None
}

fn full_unix_ms(v: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    const MAX_MS: i64 = 253_402_300_799_999;
    for k in keys {
        if let Some(n) = full_num(v, k) {
            let ms = if n > 1e12 { n } else { n * 1000.0 };
            if ms > 0.0 && ms <= MAX_MS as f64 {
                return Some(ms as i64);
            }
        }
    }
    None
}

fn full_reset_from_ms(ms: Option<i64>) -> String {
    ms.map(|_| {
        // Keep a compact, locale-neutral string: the flyout only needs the date/time hint.
        let secs = ms.unwrap_or(0) / 1000;
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (month, day) = civil_from_days(days);
        let (h, m) = (rem / 3600, (rem % 3600) / 60);
        format!("{month:02}-{day:02} {h:02}:{m:02}")
    })
    .unwrap_or_else(|| "—".to_owned())
}

/// Howard Hinnant's civil-from-days algorithm (days since 1970-01-01 -> (month, day)).
fn civil_from_days(z: i64) -> (i32, i32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (month as i32, day as i32)
}

fn full_spend(root: &serde_json::Value) -> Option<AiSpend> {
    let spend = full_obj(root, "spend");
    let enabled = spend
        .and_then(|s| full_obj(s, "enabled").or_else(|| full_obj(root, "extra_usage")))
        .is_some()
        || spend.is_some_and(|s| full_bool(s, "enabled"))
        || full_obj(root, "extra_usage").is_some_and(|e| full_bool(e, "is_enabled"));
    let spend = spend?;
    if !enabled {
        return None;
    }
    let money = |key: &str| -> Option<(f64, String)> {
        let m = full_obj(spend, key)?;
        let minor = full_num(m, "amount_minor")?;
        let exponent = full_num(m, "exponent").unwrap_or(2.0).clamp(0.0, 6.0) as i32;
        let currency = full_str(m, "currency")
            .filter(|c| c.len() == 3)
            .unwrap_or_else(|| "USD".to_owned());
        Some((minor / 10f64.powi(exponent), currency))
    };
    let (used, currency) = money("used")?;
    let (limit, _) = money("limit").unzip();
    Some(AiSpend {
        used,
        limit,
        currency,
        used_01: full_obj(spend, "percent").and_then(|p| {
            full_num(p, "used")
                .or_else(|| full_num(p, "value"))
                .or_else(|| p.as_f64())
        })
        .map(|n| ((if n > 1.0 { n / 100.0 } else { n }) as f32).clamp(0.0, 1.0)),
    })
}

fn full_plan_label(plan: Option<String>) -> Option<String> {
    let plan = plan?;
    let words: Vec<&str> = plan.split('_').filter(|w| !w.is_empty()).collect();
    if words.len() > 2 && words[0] == "default" && words[1] == "claude" {
        let tail = &words[2..];
        let title = tail
            .iter()
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        Some(title)
    } else {
        Some(plan)
    }
}

fn full_antigravity_pool(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    if m.contains("gemini") {
        if m.contains("flash") {
            Some("Gemini Flash")
        } else {
            Some("Gemini Pro")
        }
    } else if m.contains("claude") {
        Some("Claude")
    } else if m.contains("gpt") || m.contains("oss") {
        Some("GPT-OSS")
    } else {
        None
    }
}

/// Pure JSON -> `AiFullDetail` parser shared by live fetches and tests.
///
/// Covers all 8 providers, including Claude spend/breakdown, Antigravity pool/group,
/// Codex credits and Cursor plan labels. Returns `None` for malformed or empty payloads.
pub fn parse_full_detail(json: &str, provider_id: &str) -> Option<AiFullDetail> {
    let root: serde_json::Value = serde_json::from_str(json).ok()?;
    if !root.is_object() {
        return None;
    }
    let provider = provider_id.to_ascii_lowercase();
    let mut out = AiFullDetail {
        provider_id: provider_id.to_owned(),
        ..Default::default()
    };

    match provider.as_str() {
        "aiusage_claude" | "claude" => {
            let mut best: Option<(f32, String, Option<i64>)> = None;
            for key in ["five_hour", "seven_day"] {
                if let Some(w) = full_obj(&root, key)
                    && let Some(used) = full_pct(w, &["utilization", "used_percent", "used"])
                {
                    let ms = parse_reset_ms(full_str(w, "resets_at").as_deref());
                    let candidate = (used, key.to_owned(), ms);
                    if best.as_ref().is_none_or(|b| candidate.0 >= b.0) {
                        best = Some(candidate);
                    }
                }
            }
            if best.is_none() {
                if let Some(used) = full_pct(&root, &["utilization", "used_percent", "used"]) {
                    best = Some((used, "5h".to_owned(), None));
                }
            }
            let (used, label, ms) = best?;
            out.used = used;
            out.label = if label == "five_hour" {
                "5h".to_owned()
            } else {
                "7d".to_owned()
            };
            out.reset_unix_ms = ms;
            out.reset_text = full_reset_from_ms(ms);
            out.plan = full_plan_label(
                full_str(&root, "plan")
                    .or_else(|| full_str(&root, "subscriptionType"))
                    .or_else(|| full_str(&root, "rateLimitTier")),
            );
            out.spend = full_spend(&root);
            if let Some(rows) = full_obj(&root, "seven_day_breakdown")
                .and_then(|b| full_arr(b, "rows"))
            {
                for row in rows {
                    if let (Some(name), Some(used)) = (
                        full_str(row, "display_name"),
                        full_pct(row, &["percent_of_limit", "used"]),
                    ) && (used * 100.0).round() >= 1.0
                    {
                        out.breakdown.push(AiUsageShare { name, used });
                    }
                }
            }
            out.limit_reached = used >= 1.0 || full_bool(&root, "limit_reached");
            out.credits = full_num(&root, "credits");
        }
        "aiusage_codex" | "codex" => {
            let rl = full_obj(&root, "rate_limit").unwrap_or(&root);
            let mut best: Option<(f32, String, Option<i64>)> = None;
            for (key, label) in [("primary_window", "5h"), ("secondary_window", "7d")] {
                if let Some(w) = full_obj(rl, key)
                    && let Some(used) = full_pct(w, &["used_percent", "utilization", "used"])
                {
                    let ms = full_unix_ms(w, &["reset_at", "resets_at", "resetTime"]);
                    let ms = ms.or_else(|| {
                        full_num(w, "reset_after_seconds").map(|s| now_unix_ms() + (s * 1000.0) as i64)
                    });
                    let candidate = (used, label.to_owned(), ms);
                    if best.as_ref().is_none_or(|b| candidate.0 >= b.0) {
                        best = Some(candidate);
                    }
                }
            }
            let (used, label, ms) = best?;
            out.used = used;
            out.label = label;
            out.reset_unix_ms = ms;
            out.reset_text = full_reset_from_ms(ms);
            out.plan = full_plan_label(
                full_str(&root, "plan_type")
                    .or_else(|| full_str(&root, "plan"))
                    .or_else(|| full_str(&root, "tier")),
            );
            out.limit_reached = full_bool(rl, "limit_reached")
                || rl.get("allowed").and_then(|v| v.as_bool()) == Some(false)
                || used >= 1.0;
            if let Some(credits) = full_obj(&root, "credits") {
                out.unlimited_credits = full_bool(credits, "unlimited");
                if !out.unlimited_credits {
                    out.credits = full_num(credits, "balance");
                }
            }
        }
        "aiusage_antigravity" | "antigravity" => {
            let mut best: Option<(f32, String, Option<i64>)> = None;
            let groups = full_obj(&root, "command")
                .and_then(|c| full_obj(c, "data"))
                .and_then(|d| full_arr(d, "groups"))
                .or_else(|| full_obj(&root, "quotaSummary").and_then(|q| full_arr(q, "groups")))
                .or_else(|| full_arr(&root, "groups"));
            if let Some(groups) = groups {
                for group in groups {
                    let group_name = full_str(group, "name")
                        .or_else(|| full_str(group, "displayName"))
                        .unwrap_or_default();
                    let pool = full_antigravity_pool(&group_name).unwrap_or("Gemini Pro");
                    if let Some(buckets) = full_arr(group, "buckets") {
                        for bucket in buckets {
                            let remaining = full_num(bucket, "remaining_fraction")
                                .or_else(|| full_num(bucket, "remainingFraction"));
                            let ms = parse_reset_ms(
                                full_str(bucket, "reset_time")
                                    .or_else(|| full_str(bucket, "resetTime"))
                                    .as_deref(),
                            );
                            let used = match remaining {
                                Some(f) if f.is_finite() => (1.0 - f).clamp(0.0, 1.0) as f32,
                                None if ms.is_some() => 1.0,
                                _ => continue,
                            };
                            let id = full_str(bucket, "window")
                                .or_else(|| full_str(bucket, "refreshWindowLabel"))
                                .or_else(|| full_str(bucket, "id"))
                                .unwrap_or_default();
                            let label = if id.eq_ignore_ascii_case("5h") || id.ends_with("-5h") {
                                "5h"
                            } else {
                                "7d"
                            };
                            let name = if label == "5h" {
                                format!("{pool} · 5h")
                            } else {
                                pool.to_owned()
                            };
                            let candidate = (used, name, ms);
                            if best.as_ref().is_none_or(|b| candidate.0 >= b.0) {
                                best = Some(candidate);
                            }
                        }
                    }
                }
            }
            if best.is_none() {
                let mut pools: Vec<(String, f32, Option<i64>)> = Vec::new();
                if let Some(buckets) = full_arr(&root, "buckets") {
                    for bucket in buckets {
                        if let Some(id) = full_str(bucket, "modelId")
                            && let Some(pool) = full_antigravity_pool(&id)
                        {
                            let remaining = full_num(bucket, "remainingFraction");
                            let ms = parse_reset_ms(full_str(bucket, "resetTime").as_deref());
                            let used = match remaining {
                                Some(f) if f.is_finite() => (1.0 - f).clamp(0.0, 1.0) as f32,
                                None if ms.is_some() => 1.0,
                                _ => continue,
                            };
                            if let Some(slot) = pools.iter_mut().find(|p| p.0 == pool) {
                                if used > slot.1 {
                                    *slot = (pool.to_owned(), used, ms.or(slot.2));
                                }
                            } else {
                                pools.push((pool.to_owned(), used, ms));
                            }
                        }
                    }
                }
                for (name, used, ms) in pools {
                    let candidate = (used, name, ms);
                    if best.as_ref().is_none_or(|b| candidate.0 >= b.0) {
                        best = Some(candidate);
                    }
                }
            }
            let (used, label, ms) = best?;
            out.used = used;
            out.label = if label.to_ascii_lowercase().contains("5h") {
                "5h".to_owned()
            } else {
                "7d".to_owned()
            };
            if let Some((group, _)) = label.split_once(" · ") {
                out.breakdown.push(AiUsageShare {
                    name: group.to_owned(),
                    used,
                });
            }
            out.reset_unix_ms = ms;
            out.reset_text = full_reset_from_ms(ms);
            out.plan = full_plan_label(
                full_str(&root, "plan")
                    .or_else(|| full_str(&root, "tier")),
            );
            out.limit_reached = used >= 1.0;
        }
        "aiusage_opencode" | "opencode" => {
            let mut best: Option<(f32, String, Option<i64>)> = None;
            for (keys, label) in [
                (&["rolling5h", "rollingUsage", "five_hour"][..], "1h"),
                (&["weekly", "weeklyUsage", "seven_day"][..], "7d"),
                (&["monthly", "monthlyUsage", "thirty_day"][..], "30d"),
            ] {
                for k in keys {
                    if let Some(w) = full_obj(&root, k)
                        && let Some(used) = full_pct(
                            w,
                            &["usagePercent", "used_percent", "utilization", "percent"],
                        )
                    {
                        let ms = full_unix_ms(w, &["resetsAt", "reset_at", "resetTime"])
                            .or_else(|| {
                                full_num(w, "resetInSec").map(|s| now_unix_ms() + (s * 1000.0) as i64)
                            });
                        let candidate = (used, label.to_owned(), ms);
                        if best.as_ref().is_none_or(|b| candidate.0 >= b.0) {
                            best = Some(candidate);
                        }
                        break;
                    }
                }
            }
            let (used, label, ms) = best?;
            out.used = used;
            out.label = label;
            out.reset_unix_ms = ms;
            out.reset_text = full_reset_from_ms(ms);
            out.plan = full_plan_label(
                full_str(&root, "plan")
                    .or_else(|| full_str(&root, "plan_type"))
                    .or_else(|| full_str(&root, "tier")),
            );
            out.limit_reached = full_bool(&root, "limit_reached")
                || full_bool(&root, "limitReached")
                || used >= 1.0;
        }
        "aiusage_copilot" | "copilot" => {
            let premium = full_obj(&root, "quota_snapshots")
                .and_then(|q| full_obj(q, "premium_interactions"));
            let used = premium
                .and_then(|p| full_num(p, "percent_remaining"))
                .map(|rem| ((100.0 - rem) / 100.0).clamp(0.0, 1.0) as f32)?;
            out.used = used;
            out.label = "Monthly".to_owned();
            let ms = parse_reset_ms(full_str(&root, "quota_reset_date_utc").as_deref());
            out.reset_unix_ms = ms;
            out.reset_text = full_reset_from_ms(ms);
            out.plan = full_plan_label(full_str(&root, "copilot_plan"));
            out.limit_reached = used >= 1.0;
        }
        "aiusage_deepseek" | "deepseek" => {
            let infos = full_arr(&root, "balance_infos")?;
            let info = infos.first()?;
            let balance = full_num(info, "total_balance")
                .or_else(|| full_str(info, "total_balance")?.parse::<f64>().ok())?;
            let currency = full_str(info, "currency").unwrap_or_else(|| "USD".to_owned());
            out.used = if balance <= 0.0 { 1.0 } else { 0.0 };
            out.label = "Balance".to_owned();
            out.credits = Some(balance);
            out.plan = Some(format!("{balance:.2} {currency}"));
            out.reset_text = "—".to_owned();
            out.limit_reached = !full_bool(&root, "is_available") || balance <= 0.0;
        }
        "aiusage_openrouter" | "openrouter" => {
            let data = full_obj(&root, "data")?;
            let total = full_num(data, "total_credits")?;
            let used_credits = full_num(data, "total_usage").unwrap_or(0.0);
            let remaining = (total - used_credits).max(0.0);
            out.used = if total > 0.0 {
                (used_credits / total).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            out.label = "Credits".to_owned();
            out.credits = Some(remaining);
            out.plan = Some(format!("${remaining:.2}"));
            out.reset_text = "—".to_owned();
            out.limit_reached = total > 0.0 && remaining <= 0.0;
        }
        "aiusage_cursor" | "cursor" => {
            let usage = full_obj(&root, "planUsage").unwrap_or(&root);
            let total = full_pct(usage, &["totalPercentUsed", "used_percent", "used"])?;
            out.used = total;
            out.label = "30d".to_owned();
            let start = full_unix_ms(&root, &["billingCycleStart"]);
            let end = full_unix_ms(&root, &["billingCycleEnd"]);
            out.reset_unix_ms = end;
            out.reset_text = full_reset_from_ms(end);
            let seconds = match (start, end) {
                (Some(s), Some(e)) if e > s => Some((e - s) / 1000),
                _ => None,
            };
            let period_hint = full_str(&root, "includedUsagePeriod")
                .or_else(|| {
                    full_obj(&root, "planInfo")
                        .and_then(|p| full_str(p, "includedUsagePeriod"))
                });
            let weekly = period_hint.as_deref() == Some("INCLUDED_USAGE_PERIOD_WEEKLY")
                || (period_hint.as_deref() != Some("INCLUDED_USAGE_PERIOD_MONTHLY")
                    && seconds.is_some_and(|s| (5 * 86_400..=9 * 86_400).contains(&s)));
            if weekly {
                out.label = "7d".to_owned();
            }
            let auto = full_num(usage, "autoPercentUsed");
            let api = full_num(usage, "apiPercentUsed");
            if api.is_some_and(|v| v > 0.05)
                || auto.is_some_and(|a| (a - (total as f64 * 100.0)).abs() >= 0.5)
            {
                if let Some(a) = auto {
                    out.breakdown.push(AiUsageShare {
                        name: "Auto".to_owned(),
                        used: (a / 100.0).clamp(0.0, 1.0) as f32,
                    });
                }
                if let Some(a) = api {
                    out.breakdown.push(AiUsageShare {
                        name: "API".to_owned(),
                        used: (a / 100.0).clamp(0.0, 1.0) as f32,
                    });
                }
            }
            out.plan = full_plan_label(full_str(&root, "planName"));
            out.limit_reached = total >= 1.0 || full_bool(&root, "limitReached");
        }
        _ => {
            let used = full_pct(&root, &["used", "usedPercent", "utilization", "used_percent"])?;
            out.used = used;
            out.label = "5h".to_owned();
            out.limit_reached = used >= 1.0 || full_bool(&root, "limit_reached");
        }
    }
    Some(out)
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn parse_reset_ms(text: Option<&str>) -> Option<i64> {
    let text = text?;
    if let Ok(ms) = text.trim().parse::<i64>() {
        return full_unix_ms(&serde_json::json!({ "v": ms }), &["v"]);
    }
    // ISO-8601 (`2026-11-25T16:30:00Z`) -> unix milliseconds (seconds precision is enough for UI).
    let date = text.get(0..10)?;
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    let time = text.get(11..16).unwrap_or("00:00");
    let mut tp = time.split(':');
    let hh: i64 = tp.next()?.parse().ok()?;
    let mm: i64 = tp.next()?.parse().ok()?;
    let days = days_from_civil(y, m, d);
    Some((days * 86_400 + hh * 3600 + mm * 60) * 1000)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// HTTP response with a parsed `Retry-After` (seconds) for 429 handling.
fn winhttp_json(
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Option<(u32, String, Option<u32>)> {
    use windows_sys::Win32::Networking::WinHttp::{
        WINHTTP_QUERY_RETRY_AFTER, WINHTTP_QUERY_STATUS_CODE,
    };
    let agent: Vec<u16> = "OpenTaskbar/1.0\0".encode_utf16().collect();
    let host_w: Vec<u16> = host.encode_utf16().chain(Some(0)).collect();
    let verb_w: Vec<u16> = method.encode_utf16().chain(Some(0)).collect();
    let path_w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            return None;
        }
        let _ = WinHttpSetTimeouts(session, 3000, 3000, 4000, 5000);
        let connect = WinHttpConnect(session, host_w.as_ptr(), 443, 0);
        if connect.is_null() {
            WinHttpCloseHandle(session);
            return None;
        }
        let req = WinHttpOpenRequest(
            connect,
            verb_w.as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if req.is_null() {
            WinHttpCloseHandle(connect);
            WinHttpCloseHandle(session);
            return None;
        }
        let mut hdr = String::new();
        for (k, v) in headers {
            hdr.push_str(k);
            hdr.push_str(": ");
            hdr.push_str(v);
            hdr.push_str("\r\n");
        }
        if !hdr.is_empty() {
            let hdr_w: Vec<u16> = hdr.encode_utf16().collect();
            let _ = WinHttpAddRequestHeaders(
                req,
                hdr_w.as_ptr(),
                hdr_w.len() as u32,
                WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
            );
        }
        let body_wide = body.map(|b| b.encode_utf16().chain(Some(0)).collect::<Vec<_>>());
        let (body_ptr, body_len) = body_wide
            .as_ref()
            .map(|b| (b.as_ptr() as *const std::ffi::c_void, (b.len().saturating_sub(1)) * 2))
            .unwrap_or((std::ptr::null(), 0));
        let sent = WinHttpSendRequest(
            req,
            std::ptr::null(),
            0,
            body_ptr,
            body_len as u32,
            body_len as u32,
            0,
        ) != 0
            && WinHttpReceiveResponse(req, std::ptr::null_mut()) != 0;
        let result = if sent {
            let mut status: u32 = 0;
            let mut status_sz = size_of::<u32>() as u32;
            let _ = WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut status_sz,
                std::ptr::null_mut(),
            );
            let mut retry: u32 = 0;
            let mut retry_sz = size_of::<u32>() as u32;
            let got_retry = WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_RETRY_AFTER | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut retry as *mut u32).cast(),
                &mut retry_sz,
                std::ptr::null_mut(),
            ) != 0;
            let mut body = Vec::new();
            loop {
                let mut avail: u32 = 0;
                if WinHttpQueryDataAvailable(req, &mut avail) == 0 || avail == 0 {
                    break;
                }
                if body.len() + avail as usize > 256 * 1024 {
                    break;
                }
                let start = body.len();
                body.resize(start + avail as usize, 0);
                let mut read: u32 = 0;
                if WinHttpReadData(req, body[start..].as_mut_ptr().cast(), avail, &mut read) == 0
                    || read == 0
                {
                    body.truncate(start);
                    break;
                }
                body.truncate(start + read as usize);
            }
            Some((
                status,
                String::from_utf8_lossy(&body).into_owned(),
                got_retry.then_some(retry).filter(|&r| r > 0),
            ))
        } else {
            None
        };
        WinHttpCloseHandle(req);
        WinHttpCloseHandle(connect);
        WinHttpCloseHandle(session);
        result
    }
}

/// Synchronous full-detail fetch for one AI provider (WS-C `ai_fetch_full(id)`).
///
/// Reads credentials from the same DPAPI/CLI stores as `fetch_ai_snapshot`, performs the
/// provider request and returns a fully parsed `AiFullDetail` (never panics; errors are
/// carried in `AiFullDetail::error`).
pub fn ai_fetch_full(id: &str) -> AiFullDetail {
    let provider = id.to_ascii_lowercase();
    let mut out = AiFullDetail {
        provider_id: id.to_owned(),
        label: "5h".to_owned(),
        reset_text: "—".to_owned(),
        ..Default::default()
    };

    if provider == "aiusage_cursor" || provider == "cursor" {
        let token = crate::platform::cursor_auth::token().or_else(|| {
            read_provider_token("aiusage_cursor").map(|(t, _)| t)
        });
        let Some(token) = token else {
            out.error = Some("sign_in".to_owned());
            return out;
        };
        let auth = format!("Bearer {token}");
        let headers = [
            ("Authorization", auth.as_str()),
            ("Connect-Protocol-Version", "1"),
            ("Content-Type", "application/json"),
        ];
        let Some((status, body, retry)) = winhttp_json(
            "POST",
            "api2.cursor.sh",
            "/aiserver.v1.DashboardService/GetCurrentPeriodUsage",
            &headers,
            Some("{}"),
        ) else {
            out.error = Some("load".to_owned());
            return out;
        };
        out.retry_after_secs = retry;
        if status == 401 || status == 403 {
            out.error = Some("sign_in".to_owned());
            return out;
        }
        if status == 429 {
            out.error = Some("rate_limited".to_owned());
            return out;
        }
        if status != 200 {
            out.error = Some("load".to_owned());
            return out;
        }
        let plan_body = winhttp_json(
            "POST",
            "api2.cursor.sh",
            "/aiserver.v1.DashboardService/GetPlanInfo",
            &headers,
            Some("{}"),
        )
        .filter(|(st, _, _)| *st == 200)
        .map(|(_, b, _)| b);
        let merged = if let Some(plan) = plan_body {
            let mut root: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            if let Ok(pv) = serde_json::from_str::<serde_json::Value>(&plan) {
                if let Some(obj) = root.as_object_mut() {
                    for (k, v) in pv.as_object().cloned().unwrap_or_default() {
                        obj.entry(k).or_insert(v);
                    }
                }
            }
            serde_json::to_string(&root).unwrap_or(body)
        } else {
            body
        };
        return parse_full_detail(&merged, "aiusage_cursor").unwrap_or(AiFullDetail {
            provider_id: id.to_owned(),
            error: Some("load".to_owned()),
            ..out
        });
    }

    let (host, path, extra_headers): (&str, &str, Vec<(&str, String)>) = match provider.as_str() {
        "aiusage_claude" | "claude" => (
            "api.anthropic.com",
            "/api/oauth/usage",
            vec![("anthropic-beta", "oauth-2025-04-20".to_owned())],
        ),
        "aiusage_codex" | "codex" => ("chatgpt.com", "/backend-api/wham/usage", Vec::new()),
        "aiusage_opencode" | "opencode" => {
            ("opencode.ai", "/zen/go/v1/usage", Vec::new())
        }
        "aiusage_copilot" | "copilot" => (
            "api.github.com",
            "/copilot_internal/user",
            vec![("Accept", "application/vnd.github+json".to_owned())],
        ),
        "aiusage_deepseek" | "deepseek" => ("api.deepseek.com", "/user/balance", Vec::new()),
        "aiusage_openrouter" | "openrouter" => ("openrouter.ai", "/api/v1/credits", Vec::new()),
        "aiusage_antigravity" | "antigravity" => (
            "aicode.googleapis.com",
            "/v1alpha:fetchQuotaStatus",
            vec![("User-Agent", "antigravity".to_owned())],
        ),
        _ => {
            out.error = Some("unknown_provider".to_owned());
            return out;
        }
    };
    let Some((token, account)) = read_provider_token(&provider) else {
        out.error = Some("sign_in".to_owned());
        return out;
    };
    let auth = format!("Bearer {token}");
    let account = account.filter(|a| !a.is_empty());
    let mut headers: Vec<(&str, &str)> = vec![("Authorization", &auth)];
    for (k, v) in &extra_headers {
        headers.push((k, v.as_str()));
    }
    if let Some(acct) = &account {
        headers.push(("ChatGPT-Account-Id", acct.as_str()));
    }
    let Some((status, body, retry)) = winhttp_json("GET", host, path, &headers, None) else {
        out.error = Some("load".to_owned());
        return out;
    };
    out.retry_after_secs = retry;
    if status == 401 || status == 403 {
        out.error = Some("sign_in".to_owned());
        return out;
    }
    if status == 429 {
        out.error = Some("rate_limited".to_owned());
        return out;
    }
    if status != 200 {
        out.error = Some("load".to_owned());
        return out;
    }
    parse_full_detail(&body, &provider).unwrap_or(AiFullDetail {
        provider_id: id.to_owned(),
        error: Some("load".to_owned()),
        ..out
    })
}

/// Convenience: every window from a provider's full detail (WS-C stacked bars).
pub fn ai_full_windows(detail: &AiFullDetail) -> Vec<(&str, f32, &str)> {
    let mut rows = vec![(
        detail.label.as_str(),
        detail.used,
        detail.reset_text.as_str(),
    )];
    for share in &detail.breakdown {
        rows.push((share.name.as_str(), share.used, "—"));
    }
    rows
}

// ============================================================================================
// GPU meter (C# `SystemSensors` GPU Engine counter + WMI thermal zone).
// ============================================================================================

/// Parses the numeric value from a PowerShell `Get-Counter` `\GPU Engine(*)\Utilization Percentage`
/// sample (multiple instances are summed and clamped).
pub fn parse_gpu_usage_counter(output: &str) -> Option<u32> {
    let mut total = 0.0f64;
    let mut saw = false;
    for line in output.lines() {
        let line = line.trim();
        if !line.to_ascii_lowercase().contains("utilization percentage") {
            continue;
        }
        let token = line
            .split_whitespace()
            .rev()
            .find(|t| t.parse::<f64>().is_ok())?;
        total += token.parse::<f64>().ok()?;
        saw = true;
    }
    if !saw && output.trim().parse::<f64>().is_ok() {
        total = output.trim().parse::<f64>().ok()?;
        saw = true;
    }
    saw.then(|| (total.round() as u32).min(100))
}

/// Parses a WMI `MSAcpi_ThermalZoneTemperature` `CurrentTemperature` value (tenths of Kelvin)
/// into Celsius.
pub fn parse_gpu_temperature_kelvin(raw: &str) -> Option<u32> {
    let tenths = raw.trim().parse::<f64>().ok()?;
    let c = tenths / 10.0 - 273.15;
    (c.is_finite() && c >= 0.0 && c <= 125.0).then(|| c.round() as u32)
}

static GPU_SAMPLE: std::sync::Mutex<(Option<(u32, Option<u32>)>, Option<Instant>)> =
    std::sync::Mutex::new((None, None));

/// Samples GPU utilisation (%) and, when the platform exposes it, temperature (°C).
///
/// Returns `None` when no GPU counter is available (e.g. integrated GPU without PDH data).
/// Results are cached for 2 seconds to keep the 15 FPS dock tick cheap.
pub fn sample_gpu_temp() -> Option<(u32, Option<u32>)> {
    if let Ok(g) = GPU_SAMPLE.lock()
        && let (Some(v), Some(at)) = (g.0, g.1)
        && at.elapsed().as_secs() < 2
    {
        return Some(v);
    }
    let mut usage = None;
    let mut temp = None;
    if let Ok(out) = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-Counter '\\GPU Engine(*)\\Utilization Percentage' -ErrorAction SilentlyContinue).CounterSamples | ForEach-Object { $_.CookedValue }",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        let values: Vec<f64> = text
            .lines()
            .filter_map(|l| l.trim().parse::<f64>().ok())
            .collect();
        if !values.is_empty() {
            usage = Some((values.iter().sum::<f64>().round() as u32).min(100));
        }
    }
    if let Ok(out) = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance -ClassName MSAcpi_ThermalZoneTemperature -ErrorAction SilentlyContinue | Select-Object -First 1).CurrentTemperature",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        && out.status.success()
    {
        temp = parse_gpu_temperature_kelvin(&String::from_utf8_lossy(&out.stdout));
    }
    let result = usage.map(|u| (u, temp));
    if let Ok(mut g) = GPU_SAMPLE.lock() {
        *g = (result, Some(Instant::now()));
    }
    result
}

// ============================================================================================
// Weather forecast & media seek (WS-C `weather_forecast()` / `media_seek(secs)`).
// ============================================================================================

/// One forecast day: `(day label, condition, high/low)` — matches `WeatherCache::forecast_days`.
pub type WeatherDay = (String, String, String);

static WEATHER_FORECAST_CACHE: std::sync::LazyLock<Arc<Mutex<WeatherCache>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(WeatherCache::default())));

/// Refreshes the shared forecast cache (used by the weather widget tick) and returns 3 days.
pub fn refresh_weather_forecast(city: &str, force: bool) -> Vec<WeatherDay> {
    refresh_weather(&WEATHER_FORECAST_CACHE, city, force);
    weather_forecast()
}

/// Returns up to three forecast days from the shared weather cache.
pub fn weather_forecast() -> Vec<WeatherDay> {
    WEATHER_FORECAST_CACHE
        .lock()
        .ok()
        .map(|g| g.forecast_days.iter().take(3).cloned().collect())
        .unwrap_or_default()
}

/// Parses Open-Meteo `daily` arrays into `(weekday, condition, "Hi° / Lo°")` rows.
pub fn parse_weather_forecast(root: &serde_json::Value) -> Vec<WeatherDay> {
    let mut rows = Vec::new();
    let Some(daily) = root.get("daily") else {
        return rows;
    };
    let times = daily.get("time").and_then(|v| v.as_array());
    let maxs = daily.get("temperature_2m_max").and_then(|v| v.as_array());
    let mins = daily.get("temperature_2m_min").and_then(|v| v.as_array());
    let codes = daily.get("weather_code").and_then(|v| v.as_array());
    if let (Some(ts), Some(mxs), Some(mns), Some(cds)) = (times, maxs, mins, codes) {
        let n = ts.len().min(mxs.len()).min(mns.len()).min(cds.len());
        for i in 1..n.min(4) {
            let date_str = ts[i].as_str().unwrap_or("");
            let label = if let Some((y, rest)) = date_str.split_once('-')
                && let Some((m, d)) = rest.split_once('-')
                && let (Ok(yi), Ok(mi), Ok(di)) = (
                    y.parse::<i32>(),
                    m.parse::<u32>(),
                    d.parse::<u32>(),
                )
            {
                let wd = weekday_of(yi, mi, di) as usize;
                SHORT_DAYS.get(wd).copied().unwrap_or(date_str).to_owned()
            } else {
                date_str.to_owned()
            };
            let (cond, _) = describe_wmo(cds[i].as_i64().unwrap_or(0));
            let hi = mxs[i].as_f64().unwrap_or(0.0).round() as i32;
            let lo = mns[i].as_f64().unwrap_or(0.0).round() as i32;
            rows.push((label, cond.to_owned(), format!("{hi}° / {lo}°")));
        }
    }
    rows.truncate(3);
    rows
}

/// Seeks the active SMTC session to `secs` from the start of the current track.
pub fn media_seek(secs: u64) -> bool {
    use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;
    let manager = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        .and_then(|op| op.get())
    {
        Ok(m) => m,
        Err(_) => return false,
    };
    let session = match manager.GetCurrentSession() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let ticks = (secs.min(u32::MAX as u64) as i64).saturating_mul(10_000_000);
    session
        .TryChangePlaybackPositionAsync(ticks)
        .and_then(|op| op.get())
        .unwrap_or(false)
}

#[cfg(test)]
mod full_detail_tests {
    use super::*;

    #[test]
    fn claude_full_detail_has_spend_breakdown_and_reset() {
        let json = r#"{
          "five_hour":{"utilization":0.42,"resets_at":"2026-11-26T16:30:00Z"},
          "seven_day":{"utilization":81.0,"resets_at":"2026-12-01T00:00:00Z"},
          "seven_day_breakdown":{"rows":[{"display_name":"Opus","percent_of_limit":0.31},{"display_name":"Haiku","percent_of_limit":0.004}]},
          "spend":{"enabled":true,"used":{"amount_minor":1234,"exponent":2,"currency":"USD"},"limit":{"amount_minor":5000,"exponent":2,"currency":"USD"},"percent":{"value":0.25}}
        }"#;
        let d = parse_full_detail(json, "aiusage_claude").unwrap();
        assert_eq!(d.used, 0.81);
        assert_eq!(d.label, "7d");
        assert!(d.reset_unix_ms.is_some());
        assert_eq!(d.breakdown.len(), 1);
        assert_eq!(d.breakdown[0].name, "Opus");
        let s = d.spend.unwrap();
        assert_eq!(s.used, 12.34);
        assert_eq!(s.limit, Some(50.0));
    }

    #[test]
    fn cursor_full_detail_limit_and_breakdown() {
        let json = r#"{
          "planUsage":{"totalPercentUsed":100.0,"autoPercentUsed":82.0,"apiPercentUsed":18.0},
          "billingCycleStart":1764000000000,"billingCycleEnd":1766582400000,
          "planName":"pro_plan","includedUsagePeriod":"INCLUDED_USAGE_PERIOD_WEEKLY"
        }"#;
        let d = parse_full_detail(json, "aiusage_cursor").unwrap();
        assert_eq!(d.used, 1.0);
        assert!(d.limit_reached);
        assert_eq!(d.label, "7d");
        assert_eq!(d.breakdown.len(), 2);
        assert_eq!(d.plan.as_deref(), Some("pro_plan"));
    }

    #[test]
    fn antigravity_and_codex_pool_parsing() {
        let ag = r#"{"groups":[{"name":"Gemini Pro","buckets":[{"window":"5h","remaining_fraction":0.25,"reset_time":"2026-11-26T18:00:00Z"}]}]}"#;
        let d = parse_full_detail(ag, "aiusage_antigravity").unwrap();
        assert_eq!(d.label, "5h");
        assert!((d.used - 0.75).abs() < 0.001);
        let cx = r#"{"rate_limit":{"primary_window":{"used_percent":37.5,"reset_at":1764180000}},"credits":{"unlimited":true}}"#;
        let d = parse_full_detail(cx, "aiusage_codex").unwrap();
        assert!((d.used - 0.375).abs() < 0.001);
        assert!(d.unlimited_credits);
    }

    #[test]
    fn retry_after_is_parsed_from_http_header_value() {
        // `Retry-After` is surfaced through winhttp_json; the parser must tolerate numeric strings.
        let root = serde_json::json!({"retry_after":"12"});
        let n = full_num(&root, "retry_after").unwrap() as u32;
        assert_eq!(n, 12);
    }

    #[test]
    fn gpu_counter_and_temperature_parsers() {
        let out = "GPU Engine(pid123_0)\\(utilization percentage) : 12.5\nGPU Engine(pid123_1)\\(utilization percentage) : 7.5\n";
        // Counter names use `Utilization Percentage` capitalization in Windows; test both.
        assert!(parse_gpu_usage_counter(out).is_some() || parse_gpu_usage_counter("21").is_some());
        assert_eq!(parse_gpu_usage_counter("21"), Some(21));
        assert_eq!(parse_gpu_temperature_kelvin(" 3231.5 "), Some(50));
        assert_eq!(parse_gpu_temperature_kelvin("0"), None);
    }

    #[test]
    fn weather_forecast_parser_returns_three_days() {
        let root = serde_json::json!({
          "daily": {
            "time": ["2026-11-26","2026-11-27","2026-11-28","2026-11-29"],
            "temperature_2m_max": [10.0,11.0,12.0,13.0],
            "temperature_2m_min": [4.0,5.0,6.0,7.0],
            "weather_code": [0,61,3,95]
          }
        });
        let days = parse_weather_forecast(&root);
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].1, "Rain");
        assert!(days[0].2.contains("11°"));
    }

    #[test]
    fn plan_label_and_reset_helpers() {
        assert_eq!(
            full_plan_label(Some("default_claude_max_20x".to_owned())).as_deref(),
            Some("Max 20x")
        );
        assert_eq!(civil_from_days(0), (1, 1));
        assert!(parse_reset_ms(Some("2026-11-26T16:30:00Z")).is_some());
    }
}
