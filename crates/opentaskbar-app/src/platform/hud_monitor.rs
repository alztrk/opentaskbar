//! N1: system HUD (volume, brightness, Caps/Num Lock, keyboard layout, power, network,
//! Bluetooth) change detection for the Dynamic Island.
//!
//! Design rules:
//! - No global keyboard hooks. [`HudMonitor::poll`] is driven by the existing UI timer and
//!   only samples the HUD kinds the user enabled; every probe it runs inline is a cheap
//!   value query, so a poll stays in the tens of microseconds.
//! - DDC/CI brightness (`GetMonitorBrightness`, 40-200 ms on external monitors) never runs
//!   on the UI thread: a single lazily spawned worker answers at most one request at a time
//!   (`AtomicBool` in-flight guard) and publishes the result through atomics.
//! - With every HUD disabled `poll` returns `None` after a handful of settings lookups and
//!   drops all cached baselines, so a disabled feature keeps no state alive.
//! - The first sample of each kind only seeds a baseline; nothing is emitted at startup or
//!   right after a kind is switched on.
//! - Expensive probes are throttled: brightness (DDC/CI) is requested every 2.5 s with a 60 s
//!   back-off on monitors that do not answer, network interfaces every 2 s.
//!
//! The flags come from the typed `DockSettings::hud_*_enabled` accessors through
//! [`HudFlags::from_settings`]; callers that already have flags use
//! [`HudMonitor::poll_with_flags`].

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use opentaskbar_core::settings::DockSettings;

/// Brightness is read over DDC/CI, which is slow; never request it more often than this.
const BRIGHTNESS_POLL_INTERVAL: Duration = Duration::from_millis(2500);
/// Back-off after a monitor fails to report brightness (internal panels, no DDC/CI).
const BRIGHTNESS_FAILURE_BACKOFF: Duration = Duration::from_secs(60);
/// `GetIfTable2` allocates a table per call; keep it off the hot path.
const NETWORK_POLL_INTERVAL: Duration = Duration::from_millis(2000);
/// Events produced in one tick are shown one after another; anything beyond this is dropped.
const MAX_PENDING_EVENTS: usize = 4;
/// Upper bound on tracked Bluetooth device keys.
const MAX_BT_KEYS: usize = 32;
/// Upper bound on any user-visible text built from device or adapter names.
const MAX_LABEL_CHARS: usize = 48;
/// Default on-screen time for a HUD.
const DEFAULT_DURATION_MS: u32 = 1600;

const ACCENT_GREEN: (u8, u8, u8) = (52, 199, 89);
const ACCENT_AMBER: (u8, u8, u8) = (255, 159, 10);
const ACCENT_RED: (u8, u8, u8) = (255, 69, 58);
const ACCENT_BLUE: (u8, u8, u8) = (10, 132, 255);

// Segoe Fluent Icons / Segoe MDL2 Assets glyphs.
const ICON_VOLUME_MUTE: &str = "\u{E74F}";
const ICON_VOLUME_LOW: &str = "\u{E993}";
const ICON_VOLUME_MID: &str = "\u{E994}";
const ICON_VOLUME_HIGH: &str = "\u{E995}";
const ICON_BRIGHTNESS: &str = "\u{E706}";
const ICON_KEYBOARD: &str = "\u{E765}";
const ICON_LANGUAGE: &str = "\u{E774}";
const ICON_BATTERY_CHARGING: &str = "\u{EBB5}";
const ICON_BATTERY: &str = "\u{EBAA}";
const ICON_WIFI: &str = "\u{E701}";
const ICON_ETHERNET: &str = "\u{E839}";
const ICON_NETWORK_OFF: &str = "\u{F384}";
const ICON_BLUETOOTH: &str = "\u{E702}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudKind {
    Volume,
    Brightness,
    CapsLock,
    KeyboardLayout,
    Power,
    Network,
    Bluetooth,
    Custom,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HudEvent {
    pub kind: HudKind,
    pub icon: &'static str,
    pub title: String,
    pub subtitle: String,
    /// `0.0..=1.0` for level HUDs (volume, brightness, battery).
    pub progress: Option<f32>,
    pub accent_rgb: Option<(u8, u8, u8)>,
    pub duration_ms: u32,
}

/// Which HUD kinds the user enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HudFlags {
    pub volume: bool,
    pub brightness: bool,
    pub capslock: bool,
    pub keyboard_lang: bool,
    pub power: bool,
    pub network: bool,
    pub bluetooth: bool,
}

impl HudFlags {
    pub const ALL: Self = Self {
        volume: true,
        brightness: true,
        capslock: true,
        keyboard_lang: true,
        power: true,
        network: true,
        bluetooth: true,
    };

    /// Reads the typed `hud_*_enabled` settings accessors.
    pub fn from_settings(settings: &DockSettings) -> Self {
        Self {
            volume: settings.hud_volume_enabled(),
            brightness: settings.hud_brightness_enabled(),
            capslock: settings.hud_capslock_enabled(),
            keyboard_lang: settings.hud_keyboard_lang_enabled(),
            power: settings.hud_power_enabled(),
            network: settings.hud_network_enabled(),
            bluetooth: settings.hud_bluetooth_enabled(),
        }
    }

    pub fn any(&self) -> bool {
        self.volume
            || self.brightness
            || self.capslock
            || self.keyboard_lang
            || self.power
            || self.network
            || self.bluetooth
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyboardSample {
    hkl: isize,
    foreground: isize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PowerSample {
    ac_online: bool,
    battery_pct: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NetworkSample {
    connected: bool,
    wireless: bool,
    name: String,
}

/// One tick's worth of raw readings; `None` means "not sampled this tick".
#[derive(Debug, Clone, Default)]
struct HudSample<'a> {
    volume: Option<(u8, bool)>,
    brightness: Option<u8>,
    capslock: Option<bool>,
    numlock: Option<bool>,
    keyboard: Option<KeyboardSample>,
    power: Option<PowerSample>,
    network: Option<NetworkSample>,
    bluetooth: Option<&'a [(String, u8)]>,
}

#[derive(Debug, Default)]
pub struct HudMonitor {
    last_volume: Option<(u8, bool)>,
    last_brightness: Option<u8>,
    last_capslock: Option<bool>,
    last_numlock: Option<bool>,
    last_hkl: Option<isize>,
    last_foreground: Option<isize>,
    last_ac_online: Option<bool>,
    last_wifi_connected: Option<bool>,
    last_network_name: String,
    last_bt_keys: HashSet<String>,
    bt_seeded: bool,
    last_brightness_poll: Option<Instant>,
    brightness_backoff_until: Option<Instant>,
    /// Generation of the last brightness probe result consumed by this monitor.
    brightness_seen_generation: u32,
    last_network_poll: Option<Instant>,
    pending: VecDeque<HudEvent>,
    active: bool,
}

impl HudMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Samples every enabled HUD kind and returns at most one event per call.
    ///
    /// `volume_pct` is `(percent, muted)` from the audio widget; `bt_devices` is
    /// `(device name, battery %)` for currently connected Bluetooth devices. System HUDs
    /// stay on screen for the user's `HudDurationMs`.
    pub fn poll(
        &mut self,
        settings: &DockSettings,
        volume_pct: Option<(u8, bool)>,
        bt_devices: &[(String, u8)],
    ) -> Option<HudEvent> {
        let mut event =
            self.poll_with_flags(HudFlags::from_settings(settings), volume_pct, bt_devices)?;
        if event.kind != HudKind::Custom {
            event.duration_ms = settings.hud_duration_ms();
        }
        Some(event)
    }

    /// Same as [`Self::poll`] with explicit flags.
    pub fn poll_with_flags(
        &mut self,
        flags: HudFlags,
        volume_pct: Option<(u8, bool)>,
        bt_devices: &[(String, u8)],
    ) -> Option<HudEvent> {
        if !flags.any() {
            if self.active {
                *self = Self::default();
            }
            return None;
        }
        self.active = true;
        let now = Instant::now();
        let sample = self.sample(flags, volume_pct, bt_devices, now);
        self.evaluate(flags, sample)
    }

    /// Drops every baseline so the next poll re-seeds silently (e.g. after resume).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn sample<'a>(
        &mut self,
        flags: HudFlags,
        volume_pct: Option<(u8, bool)>,
        bt_devices: &'a [(String, u8)],
        now: Instant,
    ) -> HudSample<'a> {
        let mut sample = HudSample {
            volume: volume_pct.filter(|_| flags.volume),
            bluetooth: flags.bluetooth.then_some(bt_devices),
            ..HudSample::default()
        };
        if flags.capslock {
            sample.capslock = Some(win::toggle_state(win::VK_CAPS_LOCK));
            sample.numlock = Some(win::toggle_state(win::VK_NUM_LOCK));
        }
        if flags.keyboard_lang {
            sample.keyboard = win::keyboard_sample();
        }
        if flags.power {
            sample.power = win::power_sample();
        }
        if flags.network && is_due(self.last_network_poll, NETWORK_POLL_INTERVAL, now) {
            self.last_network_poll = Some(now);
            sample.network = win::network_sample();
        }
        if flags.brightness {
            // Consume a finished probe first: two atomic loads, never DDC/CI on this thread.
            match brightness_probe::take(&mut self.brightness_seen_generation) {
                Some(Some(pct)) => sample.brightness = Some(pct),
                Some(None) => {
                    self.brightness_backoff_until = Some(now + BRIGHTNESS_FAILURE_BACKOFF);
                }
                None => {}
            }
            if is_due(self.last_brightness_poll, BRIGHTNESS_POLL_INTERVAL, now)
                && self
                    .brightness_backoff_until
                    .is_none_or(|until| now >= until)
            {
                self.last_brightness_poll = Some(now);
                brightness_probe::request();
            }
        }
        sample
    }

    fn evaluate(&mut self, flags: HudFlags, sample: HudSample<'_>) -> Option<HudEvent> {
        let mut events: Vec<HudEvent> = Vec::new();

        if flags.capslock {
            if let Some(on) = sample.capslock
                && delta(&mut self.last_capslock, on)
            {
                events.push(lock_event(true, on));
            }
            if let Some(on) = sample.numlock
                && delta(&mut self.last_numlock, on)
            {
                events.push(lock_event(false, on));
            }
        } else {
            self.last_capslock = None;
            self.last_numlock = None;
        }

        if flags.volume {
            if let Some((pct, muted)) = sample.volume {
                let value = (pct.min(100), muted);
                if delta(&mut self.last_volume, value) {
                    events.push(volume_event(value.0, value.1));
                }
            }
        } else {
            self.last_volume = None;
        }

        if flags.keyboard_lang {
            if let Some(keyboard) = sample.keyboard {
                let same_window = self.last_foreground == Some(keyboard.foreground);
                self.last_foreground = Some(keyboard.foreground);
                // A foreground change with a per-app layout is not a user switch: re-seed.
                if delta(&mut self.last_hkl, keyboard.hkl) && same_window {
                    events.push(keyboard_event(keyboard.hkl));
                }
            }
        } else {
            self.last_hkl = None;
            self.last_foreground = None;
        }

        if flags.brightness {
            if let Some(pct) = sample.brightness
                && delta(&mut self.last_brightness, pct.min(100))
            {
                events.push(brightness_event(pct.min(100)));
            }
        } else {
            self.last_brightness = None;
            self.last_brightness_poll = None;
            self.brightness_backoff_until = None;
            self.brightness_seen_generation = brightness_probe::generation();
        }

        if flags.bluetooth {
            if let Some(devices) = sample.bluetooth
                && let Some(event) = self.bluetooth_delta(devices)
            {
                events.push(event);
            }
        } else if self.bt_seeded || !self.last_bt_keys.is_empty() {
            self.last_bt_keys = HashSet::new();
            self.bt_seeded = false;
        }

        if flags.power {
            if let Some(power) = sample.power
                && delta(&mut self.last_ac_online, power.ac_online)
            {
                events.push(power_event(power));
            }
        } else {
            self.last_ac_online = None;
        }

        if flags.network {
            if let Some(network) = sample.network {
                let previous_name = std::mem::take(&mut self.last_network_name);
                self.last_network_name = if network.connected {
                    network.name.clone()
                } else {
                    previous_name.clone()
                };
                if delta(&mut self.last_wifi_connected, network.connected) {
                    let name = if network.connected {
                        network.name.as_str()
                    } else {
                        previous_name.as_str()
                    };
                    events.push(network_event(network.connected, network.wireless, name));
                }
            }
        } else {
            self.last_wifi_connected = None;
            self.last_network_name = String::new();
            self.last_network_poll = None;
        }

        // Keep arrival order; once the queue is full newer events of the same burst are dropped.
        for event in events {
            if self.pending.len() >= MAX_PENDING_EVENTS {
                break;
            }
            self.pending.push_back(event);
        }
        self.pending.pop_front()
    }

    fn bluetooth_delta(&mut self, devices: &[(String, u8)]) -> Option<HudEvent> {
        let mut current: HashSet<String> = HashSet::new();
        let mut newcomer: Option<(&str, u8)> = None;
        for (name, battery) in devices {
            let key = name.trim();
            if key.is_empty() || current.len() >= MAX_BT_KEYS {
                continue;
            }
            if current.insert(key.to_owned())
                && newcomer.is_none()
                && self.bt_seeded
                && !self.last_bt_keys.contains(key)
            {
                newcomer = Some((key, *battery));
            }
        }
        self.last_bt_keys = current;
        self.bt_seeded = true;
        newcomer.map(|(name, battery)| bluetooth_event(name, battery))
    }
}

/// Off-thread DDC/CI brightness probe.
///
/// `GetMonitorBrightness` talks to the monitor over I2C and takes 40-200 ms on external
/// displays, so it never runs on the UI thread. One worker thread is spawned the first time a
/// brightness HUD is polled and then blocks in `recv` (no CPU) between requests. The UI
/// thread only performs atomic loads/stores and a non-blocking `try_send`.
mod brightness_probe {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
    use std::sync::mpsc::{SyncSender, sync_channel};

    /// `0..=100` = last reading, `-1` = the last probe failed (no DDC/CI).
    static LAST_BRIGHTNESS: AtomicI32 = AtomicI32::new(-1);
    /// Bumped after every finished probe so each monitor consumes a result exactly once.
    static GENERATION: AtomicU32 = AtomicU32::new(0);
    static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

    fn worker() -> Option<&'static SyncSender<()>> {
        static WORKER: OnceLock<Option<SyncSender<()>>> = OnceLock::new();
        WORKER
            .get_or_init(|| {
                let (tx, rx) = sync_channel::<()>(1);
                std::thread::Builder::new()
                    .name("hud-brightness".to_owned())
                    .spawn(move || {
                        while rx.recv().is_ok() {
                            let value = super::win::brightness_percent().map_or(-1, i32::from);
                            LAST_BRIGHTNESS.store(value, Ordering::Release);
                            GENERATION.fetch_add(1, Ordering::AcqRel);
                            IN_FLIGHT.store(false, Ordering::Release);
                        }
                    })
                    .ok()
                    .map(|_| tx)
            })
            .as_ref()
    }

    /// Queues one probe unless one is already running. Never blocks.
    pub(super) fn request() {
        if IN_FLIGHT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let queued = worker().is_some_and(|tx| tx.try_send(()).is_ok());
        if !queued {
            IN_FLIGHT.store(false, Ordering::Release);
        }
    }

    /// Generation of the newest finished probe.
    pub(super) fn generation() -> u32 {
        GENERATION.load(Ordering::Acquire)
    }

    /// Returns the newest finished probe (`Some(None)` = it failed) when `seen` is stale.
    pub(super) fn take(seen: &mut u32) -> Option<Option<u8>> {
        let generation = generation();
        if generation == *seen {
            return None;
        }
        *seen = generation;
        let value = LAST_BRIGHTNESS.load(Ordering::Acquire);
        Some(u8::try_from(value).ok().map(|pct| pct.min(100)))
    }
}

/// Plain toast-style HUD for Smart OCR, screenshots, reminders and similar one-shot messages.
pub fn make_toast_hud(
    icon: &'static str,
    title: String,
    subtitle: String,
    duration_ms: u32,
) -> HudEvent {
    HudEvent {
        kind: HudKind::Custom,
        icon,
        title: truncate_chars(&title, 96),
        subtitle: truncate_chars(&subtitle, 160),
        progress: None,
        accent_rgb: None,
        duration_ms: duration_ms.clamp(500, 15_000),
    }
}

/// Seeds `slot` silently on first use; afterwards reports whether `value` differs.
fn delta<T: PartialEq + Copy>(slot: &mut Option<T>, value: T) -> bool {
    match slot.replace(value) {
        Some(previous) => previous != value,
        None => false,
    }
}

fn is_due(last: Option<Instant>, interval: Duration, now: Instant) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= interval)
}

/// Localized string with an English fallback while the catalog lacks the key.
fn localized(key: &str, fallback: &str) -> String {
    let text = opentaskbar_core::i18n::t(key);
    if text == key || text.trim().is_empty() {
        fallback.to_owned()
    } else {
        text
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

fn pct_progress(pct: u8) -> Option<f32> {
    Some(f32::from(pct.min(100)) / 100.0)
}

fn volume_event(pct: u8, muted: bool) -> HudEvent {
    let (icon, title, progress) = if muted || pct == 0 {
        (
            ICON_VOLUME_MUTE,
            localized("AudioMuted", "Muted"),
            Some(0.0),
        )
    } else {
        let icon = match pct {
            0..=33 => ICON_VOLUME_LOW,
            34..=66 => ICON_VOLUME_MID,
            _ => ICON_VOLUME_HIGH,
        };
        (icon, localized("Volume", "Volume"), pct_progress(pct))
    };
    HudEvent {
        kind: HudKind::Volume,
        icon,
        title,
        subtitle: format!("{pct}%"),
        progress,
        accent_rgb: None,
        duration_ms: DEFAULT_DURATION_MS,
    }
}

fn brightness_event(pct: u8) -> HudEvent {
    HudEvent {
        kind: HudKind::Brightness,
        icon: ICON_BRIGHTNESS,
        title: localized("QuickBrightness", "Brightness"),
        subtitle: format!("{pct}%"),
        progress: pct_progress(pct),
        accent_rgb: None,
        duration_ms: DEFAULT_DURATION_MS,
    }
}

fn lock_event(caps: bool, on: bool) -> HudEvent {
    let title = match (caps, on) {
        (true, true) => localized("HudCapsLockOn", "Caps Lock ON"),
        (true, false) => localized("HudCapsLockOff", "Caps Lock OFF"),
        (false, true) => localized("HudNumLockOn", "Num Lock ON"),
        (false, false) => localized("HudNumLockOff", "Num Lock OFF"),
    };
    HudEvent {
        kind: HudKind::CapsLock,
        icon: ICON_KEYBOARD,
        title,
        subtitle: String::new(),
        progress: None,
        accent_rgb: on.then_some(ACCENT_GREEN),
        duration_ms: 1200,
    }
}

fn keyboard_event(hkl: isize) -> HudEvent {
    let locale = win::locale_name_for_hkl(hkl);
    let tag = short_layout_tag(&locale);
    HudEvent {
        kind: HudKind::KeyboardLayout,
        icon: ICON_LANGUAGE,
        title: if tag.is_empty() {
            localized("WidgetKeyboardLayout", "Keyboard layout")
        } else {
            tag
        },
        subtitle: locale,
        progress: None,
        accent_rgb: Some(ACCENT_BLUE),
        duration_ms: 1200,
    }
}

/// `"tr-TR"` -> `"TR"`, `"en-US"` -> `"EN"`, `"zh-Hans-CN"` -> `"ZH"`.
fn short_layout_tag(locale: &str) -> String {
    locale
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(3)
        .collect::<String>()
        .to_ascii_uppercase()
}

fn power_event(power: PowerSample) -> HudEvent {
    let pct_text = power
        .battery_pct
        .map(|pct| format!("{pct}%"))
        .unwrap_or_default();
    let (icon, title, accent) = if power.ac_online {
        (
            ICON_BATTERY_CHARGING,
            localized("Charging", "Charging"),
            ACCENT_GREEN,
        )
    } else {
        (
            ICON_BATTERY,
            localized("WidgetDetailOnBattery", "On battery"),
            ACCENT_AMBER,
        )
    };
    HudEvent {
        kind: HudKind::Power,
        icon,
        title,
        subtitle: pct_text,
        progress: power.battery_pct.and_then(pct_progress),
        accent_rgb: Some(accent),
        duration_ms: DEFAULT_DURATION_MS,
    }
}

fn network_event(connected: bool, wireless: bool, name: &str) -> HudEvent {
    let (icon, title, accent) = if connected {
        (
            if wireless { ICON_WIFI } else { ICON_ETHERNET },
            localized("BluetoothConnected", "Connected"),
            ACCENT_GREEN,
        )
    } else {
        (
            ICON_NETWORK_OFF,
            localized("BluetoothDisconnected", "Disconnected"),
            ACCENT_RED,
        )
    };
    HudEvent {
        kind: HudKind::Network,
        icon,
        title,
        subtitle: truncate_chars(name, MAX_LABEL_CHARS),
        progress: None,
        accent_rgb: Some(accent),
        duration_ms: DEFAULT_DURATION_MS,
    }
}

fn bluetooth_event(name: &str, battery: u8) -> HudEvent {
    let connected = localized("BluetoothConnected", "Connected");
    let known_battery = (1..=100).contains(&battery);
    HudEvent {
        kind: HudKind::Bluetooth,
        icon: ICON_BLUETOOTH,
        title: truncate_chars(name, MAX_LABEL_CHARS),
        subtitle: if known_battery {
            format!("{connected} \u{00B7} {battery}%")
        } else {
            connected
        },
        progress: if known_battery {
            pct_progress(battery)
        } else {
            None
        },
        accent_rgb: Some(ACCENT_BLUE),
        duration_ms: DEFAULT_DURATION_MS,
    }
}

/// Raw Win32 probes. Every function is synchronous and leaks no handles; all are cheap except
/// [`win::brightness_percent`], which only runs on the brightness probe worker.
mod win {
    use std::ffi::{CStr, c_void};
    use std::sync::OnceLock;

    use super::{KeyboardSample, MAX_LABEL_CHARS, NetworkSample, PowerSample, truncate_chars};
    use windows_sys::Win32::Foundation::{FreeLibrary, HANDLE, HMODULE, POINT};
    use windows_sys::Win32::Globalization::LCIDToLocaleName;
    use windows_sys::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, GetKeyboardLayout, VK_CAPITAL, VK_NUMLOCK,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    pub(super) const VK_CAPS_LOCK: i32 = VK_CAPITAL as i32;
    pub(super) const VK_NUM_LOCK: i32 = VK_NUMLOCK as i32;

    const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
    const IF_TYPE_IEEE80211: u32 = 71;
    const IF_OPER_STATUS_UP: i32 = 1;
    const MEDIA_CONNECT_STATE_CONNECTED: i32 = 1;
    const MAX_PHYSICAL_MONITORS: usize = 4;

    /// Low bit of `GetKeyState` is the toggle state for Caps/Num Lock.
    pub(super) fn toggle_state(vk: i32) -> bool {
        // SAFETY: plain value query, no pointers.
        (unsafe { GetKeyState(vk) } & 1) != 0
    }

    pub(super) fn keyboard_sample() -> Option<KeyboardSample> {
        // SAFETY: value queries; the process-id out pointer is optional and passed as null.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return None;
            }
            let thread = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
            if thread == 0 {
                return None;
            }
            let hkl = GetKeyboardLayout(thread);
            (!hkl.is_null()).then_some(KeyboardSample {
                hkl: hkl as isize,
                foreground: hwnd as isize,
            })
        }
    }

    /// BCP 47 name of the HKL's input language (`LANGID` is the HKL low word).
    pub(super) fn locale_name_for_hkl(hkl: isize) -> String {
        let lcid = (hkl as usize & 0xFFFF) as u32;
        let mut buffer = [0u16; 85];
        // SAFETY: `buffer` is valid for `buffer.len()` UTF-16 units.
        let written =
            unsafe { LCIDToLocaleName(lcid, buffer.as_mut_ptr(), buffer.len() as i32, 0) };
        if written <= 1 {
            return String::new();
        }
        let len = (written as usize - 1).min(buffer.len());
        String::from_utf16_lossy(&buffer[..len])
    }

    pub(super) fn power_sample() -> Option<PowerSample> {
        let mut status = SYSTEM_POWER_STATUS {
            ACLineStatus: 0,
            BatteryFlag: 0,
            BatteryLifePercent: 0,
            SystemStatusFlag: 0,
            BatteryLifeTime: 0,
            BatteryFullLifeTime: 0,
        };
        // SAFETY: `status` is a valid out struct.
        if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
            return None;
        }
        // 128 = no system battery: AC never changes on desktops, so stay silent.
        if status.BatteryFlag & 128 != 0 || status.BatteryFlag == 255 {
            return None;
        }
        let ac_online = match status.ACLineStatus {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(PowerSample {
            ac_online,
            battery_pct: (status.BatteryLifePercent <= 100).then_some(status.BatteryLifePercent),
        })
    }

    struct MibTable(*mut MIB_IF_TABLE2);

    impl Drop for MibTable {
        fn drop(&mut self) {
            // SAFETY: the pointer came from a successful `GetIfTable2`.
            unsafe { FreeMibTable(self.0.cast_const().cast::<c_void>()) };
        }
    }

    /// Any physical Ethernet/Wi-Fi adapter that is up and media-connected.
    pub(super) fn network_sample() -> Option<NetworkSample> {
        let mut raw: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: `raw` receives a table allocated by iphlpapi, freed by `MibTable`.
        if unsafe { GetIfTable2(&mut raw) } != 0 || raw.is_null() {
            return None;
        }
        let table = MibTable(raw);
        // SAFETY: the table holds `NumEntries` contiguous rows starting at `Table`.
        let rows: &[MIB_IF_ROW2] = unsafe {
            let count = (*table.0).NumEntries as usize;
            let first = std::ptr::addr_of!((*table.0).Table).cast::<MIB_IF_ROW2>();
            std::slice::from_raw_parts(first, count)
        };
        let mut sample = NetworkSample {
            connected: false,
            wireless: false,
            name: String::new(),
        };
        for row in rows {
            let hardware = row.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
            let kind_ok = row.Type == IF_TYPE_ETHERNET_CSMACD || row.Type == IF_TYPE_IEEE80211;
            if !hardware || !kind_ok {
                continue;
            }
            if row.OperStatus == IF_OPER_STATUS_UP
                && row.MediaConnectState == MEDIA_CONNECT_STATE_CONNECTED
            {
                let wireless = row.Type == IF_TYPE_IEEE80211;
                // Prefer reporting Wi-Fi when both are connected.
                if !sample.connected || (wireless && !sample.wireless) {
                    sample = NetworkSample {
                        connected: true,
                        wireless,
                        name: wide_to_string(&row.Alias, MAX_LABEL_CHARS),
                    };
                }
            }
        }
        drop(table);
        Some(sample)
    }

    fn wide_to_string(buffer: &[u16], max_chars: usize) -> String {
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        truncate_chars(&String::from_utf16_lossy(&buffer[..len]), max_chars)
    }

    // ---- dxva2 (Monitor Configuration API), loaded on first brightness probe ----

    /// `PHYSICAL_MONITOR` is declared under `#pragma pack(1)`, but `HANDLE` + 128 UTF-16 units
    /// is already 8-byte aligned (264 / 260 bytes), so the C layout is identical.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PhysicalMonitor {
        handle: HANDLE,
        description: [u16; 128],
    }

    type RawProc = unsafe extern "system" fn() -> isize;
    type GetCountFn = unsafe extern "system" fn(HMONITOR, *mut u32) -> i32;
    type GetMonitorsFn = unsafe extern "system" fn(HMONITOR, u32, *mut PhysicalMonitor) -> i32;
    type GetBrightnessFn = unsafe extern "system" fn(HANDLE, *mut u32, *mut u32, *mut u32) -> i32;
    type DestroyFn = unsafe extern "system" fn(u32, *const PhysicalMonitor) -> i32;

    struct Dxva2 {
        get_count: GetCountFn,
        get_monitors: GetMonitorsFn,
        get_brightness: GetBrightnessFn,
        destroy: DestroyFn,
    }

    fn dxva2() -> Option<&'static Dxva2> {
        static API: OnceLock<Option<Dxva2>> = OnceLock::new();
        API.get_or_init(load_dxva2).as_ref()
    }

    fn proc_address(module: HMODULE, name: &CStr) -> Option<RawProc> {
        // SAFETY: `module` is a loaded module and `name` is NUL-terminated.
        unsafe { GetProcAddress(module, name.as_ptr().cast()) }
    }

    fn load_dxva2() -> Option<Dxva2> {
        let path = super::super::win32_helpers::wide("dxva2.dll");
        // SAFETY: NUL-terminated system DLL name.
        let module = unsafe { LoadLibraryW(path.as_ptr()) };
        if module.is_null() {
            return None;
        }
        let resolve = || -> Option<Dxva2> {
            let get_count = proc_address(module, c"GetNumberOfPhysicalMonitorsFromHMONITOR")?;
            let get_monitors = proc_address(module, c"GetPhysicalMonitorsFromHMONITOR")?;
            let get_brightness = proc_address(module, c"GetMonitorBrightness")?;
            let destroy = proc_address(module, c"DestroyPhysicalMonitors")?;
            // SAFETY: the signatures match the documented dxva2 exports.
            unsafe {
                Some(Dxva2 {
                    get_count: std::mem::transmute::<RawProc, GetCountFn>(get_count),
                    get_monitors: std::mem::transmute::<RawProc, GetMonitorsFn>(get_monitors),
                    get_brightness: std::mem::transmute::<RawProc, GetBrightnessFn>(get_brightness),
                    destroy: std::mem::transmute::<RawProc, DestroyFn>(destroy),
                })
            }
        };
        let api = resolve();
        if api.is_none() {
            // SAFETY: balances the `LoadLibraryW` above; no pointers into it escaped.
            unsafe { FreeLibrary(module) };
        }
        // On success the module stays loaded for the process lifetime (fn pointers are cached).
        api
    }

    struct PhysicalMonitors<'a> {
        api: &'static Dxva2,
        monitors: &'a [PhysicalMonitor],
    }

    impl Drop for PhysicalMonitors<'_> {
        fn drop(&mut self) {
            // SAFETY: the handles came from `GetPhysicalMonitorsFromHMONITOR`.
            unsafe { (self.api.destroy)(self.monitors.len() as u32, self.monitors.as_ptr()) };
        }
    }

    /// Primary monitor brightness in percent via DDC/CI, or `None` when unsupported.
    ///
    /// Slow (40-200 ms on external monitors): call only from the brightness probe worker.
    pub(super) fn brightness_percent() -> Option<u8> {
        let api = dxva2()?;
        // SAFETY: value query.
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        if monitor.is_null() {
            return None;
        }
        let mut count = 0u32;
        // SAFETY: `count` is a valid out pointer.
        if unsafe { (api.get_count)(monitor, &mut count) } == 0
            || count == 0
            || count as usize > MAX_PHYSICAL_MONITORS
        {
            return None;
        }
        let mut storage = [PhysicalMonitor {
            handle: std::ptr::null_mut(),
            description: [0; 128],
        }; MAX_PHYSICAL_MONITORS];
        // SAFETY: `storage` has room for `count` entries.
        if unsafe { (api.get_monitors)(monitor, count, storage.as_mut_ptr()) } == 0 {
            return None;
        }
        let guard = PhysicalMonitors {
            api,
            monitors: &storage[..count as usize],
        };
        let mut result = None;
        for physical in guard.monitors {
            let (mut min, mut current, mut max) = (0u32, 0u32, 0u32);
            // SAFETY: valid physical monitor handle and out pointers.
            let ok =
                unsafe { (api.get_brightness)(physical.handle, &mut min, &mut current, &mut max) }
                    != 0;
            if ok && max > min {
                let pct = (current.clamp(min, max) - min) * 100 / (max - min);
                result = Some(pct.min(100) as u8);
                break;
            }
        }
        drop(guard);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(monitor: &mut HudMonitor, flags: HudFlags, sample: HudSample<'_>) -> Option<HudEvent> {
        monitor.evaluate(flags, sample)
    }

    fn bt_sample(bt: &[(String, u8)]) -> HudSample<'_> {
        HudSample {
            bluetooth: Some(bt),
            ..HudSample::default()
        }
    }

    fn full_sample(bt: &[(String, u8)]) -> HudSample<'_> {
        HudSample {
            volume: Some((40, false)),
            brightness: Some(70),
            capslock: Some(false),
            numlock: Some(true),
            keyboard: Some(KeyboardSample {
                hkl: 0x041F_041F,
                foreground: 0x10,
            }),
            power: Some(PowerSample {
                ac_online: true,
                battery_pct: Some(80),
            }),
            network: Some(NetworkSample {
                connected: true,
                wireless: true,
                name: "Wi-Fi".to_owned(),
            }),
            bluetooth: Some(bt),
        }
    }

    #[test]
    fn first_poll_seeds_baseline_silently() {
        let mut monitor = HudMonitor::default();
        let bt = vec![("Buds".to_owned(), 80)];
        assert_eq!(tick(&mut monitor, HudFlags::ALL, full_sample(&bt)), None);
        // Same readings again: still nothing.
        assert_eq!(tick(&mut monitor, HudFlags::ALL, full_sample(&bt)), None);
        assert!(monitor.pending.is_empty());
    }

    #[test]
    fn volume_delta_emits_exact_event() {
        let mut monitor = HudMonitor::default();
        let flags = HudFlags {
            volume: true,
            ..HudFlags::default()
        };
        let sample = |pct| HudSample {
            volume: Some((pct, false)),
            ..HudSample::default()
        };
        assert_eq!(tick(&mut monitor, flags, sample(50)), None);
        let event = tick(&mut monitor, flags, sample(60)).expect("volume HUD");
        assert_eq!(
            event,
            HudEvent {
                kind: HudKind::Volume,
                icon: ICON_VOLUME_MID,
                title: localized("Volume", "Volume"),
                subtitle: "60%".to_owned(),
                progress: Some(0.6),
                accent_rgb: None,
                duration_ms: DEFAULT_DURATION_MS,
            }
        );
        let muted = tick(
            &mut monitor,
            flags,
            HudSample {
                volume: Some((60, true)),
                ..HudSample::default()
            },
        )
        .expect("mute HUD");
        assert_eq!(muted.icon, ICON_VOLUME_MUTE);
        assert_eq!(muted.progress, Some(0.0));
    }

    #[test]
    fn capslock_delta_emits_exact_event() {
        let mut monitor = HudMonitor::default();
        let flags = HudFlags {
            capslock: true,
            ..HudFlags::default()
        };
        let sample = |caps| HudSample {
            capslock: Some(caps),
            numlock: Some(true),
            ..HudSample::default()
        };
        assert_eq!(tick(&mut monitor, flags, sample(false)), None);
        assert_eq!(
            tick(&mut monitor, flags, sample(true)),
            Some(HudEvent {
                kind: HudKind::CapsLock,
                icon: ICON_KEYBOARD,
                title: localized("HudCapsLockOn", "Caps Lock ON"),
                subtitle: String::new(),
                progress: None,
                accent_rgb: Some(ACCENT_GREEN),
                duration_ms: 1200,
            })
        );
        let off = tick(&mut monitor, flags, sample(false)).expect("caps off HUD");
        assert_eq!(off.title, localized("HudCapsLockOff", "Caps Lock OFF"));
        assert_eq!(off.accent_rgb, None);
    }

    #[test]
    fn bluetooth_newcomer_emits_event() {
        let mut monitor = HudMonitor::default();
        let flags = HudFlags {
            bluetooth: true,
            ..HudFlags::default()
        };
        let first = vec![("Buds".to_owned(), 80)];
        let second = vec![("Buds".to_owned(), 80), ("MX Mouse".to_owned(), 55)];
        assert_eq!(tick(&mut monitor, flags, bt_sample(&first)), None);
        let event = tick(&mut monitor, flags, bt_sample(&second)).expect("bt HUD");
        assert_eq!(event.kind, HudKind::Bluetooth);
        assert_eq!(event.title, "MX Mouse");
        assert!(event.subtitle.ends_with("55%"));
        assert_eq!(event.progress, Some(0.55));
        // Already known: no repeat.
        assert_eq!(tick(&mut monitor, flags, bt_sample(&second)), None);
        // Disconnect then reconnect fires again.
        assert_eq!(tick(&mut monitor, flags, bt_sample(&first)), None);
        assert!(tick(&mut monitor, flags, bt_sample(&second)).is_some());
    }

    #[test]
    fn bluetooth_keys_are_bounded() {
        let mut monitor = HudMonitor::default();
        let devices: Vec<(String, u8)> = (0..200).map(|i| (format!("dev{i}"), 50)).collect();
        monitor.bluetooth_delta(&devices);
        assert!(monitor.last_bt_keys.len() <= MAX_BT_KEYS);
    }

    fn all_huds_off() -> DockSettings {
        let mut settings = DockSettings::default();
        settings.set_hud_volume_enabled(false);
        settings.set_hud_brightness_enabled(false);
        settings.set_hud_capslock_enabled(false);
        settings.set_hud_keyboard_lang_enabled(false);
        settings.set_hud_power_enabled(false);
        settings.set_hud_network_enabled(false);
        settings.set_hud_bluetooth_enabled(false);
        settings
    }

    #[test]
    fn disabled_flags_emit_nothing_and_drop_state() {
        let mut monitor = HudMonitor::default();
        let settings = all_huds_off();
        assert!(!HudFlags::from_settings(&settings).any());
        assert_eq!(monitor.poll(&settings, Some((10, false)), &[]), None);
        assert_eq!(monitor.poll(&settings, Some((90, true)), &[]), None);
        assert_eq!(monitor.last_volume, None);

        // A kind that is off never reports even when its reading changes.
        let flags = HudFlags {
            volume: true,
            ..HudFlags::default()
        };
        let changing = |caps| HudSample {
            volume: Some((30, false)),
            capslock: Some(caps),
            ..HudSample::default()
        };
        assert_eq!(tick(&mut monitor, flags, changing(false)), None);
        assert_eq!(tick(&mut monitor, flags, changing(true)), None);
    }

    #[test]
    fn settings_flag_enables_volume_poll() {
        let mut settings = all_huds_off();
        settings.set_hud_volume_enabled(true);
        settings.set_hud_duration_ms(2400);
        let mut monitor = HudMonitor::default();
        assert_eq!(monitor.poll(&settings, Some((10, false)), &[]), None);
        let event = monitor
            .poll(&settings, Some((20, false)), &[])
            .expect("volume HUD");
        assert_eq!(event.kind, HudKind::Volume);
        assert_eq!(event.subtitle, "20%");
        assert_eq!(event.duration_ms, 2400);

        // Turning everything off clears baselines; re-enabling seeds silently again.
        let off = all_huds_off();
        assert_eq!(monitor.poll(&off, Some((30, false)), &[]), None);
        assert_eq!(monitor.poll(&settings, Some((40, false)), &[]), None);
    }

    #[test]
    fn keyboard_change_in_other_window_reseeds() {
        let mut monitor = HudMonitor::default();
        let flags = HudFlags {
            keyboard_lang: true,
            ..HudFlags::default()
        };
        let sample = |hkl, foreground| HudSample {
            keyboard: Some(KeyboardSample { hkl, foreground }),
            ..HudSample::default()
        };
        assert_eq!(tick(&mut monitor, flags, sample(0x0409_0409, 1)), None);
        // Different window with its own layout: silent.
        assert_eq!(tick(&mut monitor, flags, sample(0x041F_041F, 2)), None);
        // Same window, layout switched: HUD.
        let event = tick(&mut monitor, flags, sample(0x0409_0409, 2)).expect("layout HUD");
        assert_eq!(event.kind, HudKind::KeyboardLayout);
    }

    #[test]
    fn multiple_changes_are_queued_and_bounded() {
        let mut monitor = HudMonitor::default();
        let bt: Vec<(String, u8)> = Vec::new();
        tick(&mut monitor, HudFlags::ALL, full_sample(&bt));
        let mut changed = full_sample(&bt);
        changed.volume = Some((90, false));
        changed.capslock = Some(true);
        changed.brightness = Some(20);
        changed.power = Some(PowerSample {
            ac_online: false,
            battery_pct: Some(79),
        });
        changed.network = Some(NetworkSample {
            connected: false,
            wireless: false,
            name: String::new(),
        });
        let first = tick(&mut monitor, HudFlags::ALL, changed).expect("first HUD");
        assert_eq!(first.kind, HudKind::CapsLock);
        assert!(monitor.pending.len() < MAX_PENDING_EVENTS);
        let mut drained = 1;
        while monitor
            .evaluate(HudFlags::ALL, HudSample::default())
            .is_some()
        {
            drained += 1;
        }
        assert!(drained <= MAX_PENDING_EVENTS);
    }

    #[test]
    fn network_disconnect_reports_last_adapter_name() {
        let mut monitor = HudMonitor::default();
        let flags = HudFlags {
            network: true,
            ..HudFlags::default()
        };
        let sample = |connected: bool| HudSample {
            network: Some(NetworkSample {
                connected,
                wireless: connected,
                name: if connected {
                    "Wi-Fi".to_owned()
                } else {
                    String::new()
                },
            }),
            ..HudSample::default()
        };
        assert_eq!(tick(&mut monitor, flags, sample(true)), None);
        let event = tick(&mut monitor, flags, sample(false)).expect("network HUD");
        assert_eq!(event.icon, ICON_NETWORK_OFF);
        assert_eq!(event.subtitle, "Wi-Fi");
        assert_eq!(event.accent_rgb, Some(ACCENT_RED));
    }

    #[test]
    fn layout_tag_and_toast_helper() {
        assert_eq!(short_layout_tag("tr-TR"), "TR");
        assert_eq!(short_layout_tag("en-US"), "EN");
        assert_eq!(short_layout_tag("zh-Hans-CN"), "ZH");
        assert_eq!(short_layout_tag(""), "");
        let toast = make_toast_hud(
            ICON_BRIGHTNESS,
            "Copied".to_owned(),
            "x".repeat(500),
            100_000,
        );
        assert_eq!(toast.kind, HudKind::Custom);
        assert!(toast.subtitle.chars().count() <= 160);
        assert_eq!(toast.duration_ms, 15_000);
    }
}
