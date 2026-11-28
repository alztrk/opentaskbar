//! Media widget extensions — `MediaWidget.xaml.cs` / `MediaTimeline.cs` / `LibreWolfMediaTimeline.cs` parity.
//!
//! Seek slider (`LiveEvent::Seek`), album cover (`FlyoutBlock::Image`) + tint, vertical volume
//! popover, per-app audio sessions, dynamic-island expanded `GetDetails()` payload, the real UIA
//! LibreWolf timeline reader and the `MediaTimeline` anchor state machine that rejects stale SMTC
//! echoes after a seek.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::DockSettings;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::core::GUID;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::ProcessStatus::EnumProcesses;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
};

use crate::design::Rgb;
use crate::ui::flyout::{FlyoutBlock, FlyoutSpec};

use super::diagnostics;
use super::telemetry::MediaSessionSnapshot;
use super::uia_raw::{self, Automation, Element, ElementArray};

/// `MediaWidget.GetDetails()` payload consumed by the island's 560px expanded panel.
#[derive(Clone, Debug, Default)]
pub struct MediaDetails {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app_name: String,
    pub position_secs: u64,
    pub duration_secs: u64,
    pub playing: bool,
    pub cover_path: Option<String>,
    pub tint: Rgb,
    pub volume: u32,
    pub muted: bool,
}

/// One per-app audio session (`AppAudioSessionMatcher`). `session_id` is the stable
/// `"<pid>:<exe-stem>"` key used by the volume / mute setters.
#[derive(Clone, Debug)]
pub struct AudioAppSession {
    pub session_id: String,
    pub app_name: String,
    pub volume: u32,
    pub muted: bool,
    pub icon_glyph: u16,
}

// ---------------------------------------------------------------------------
// Timeline clock parsing / range validation (production copy of the reference helpers).
// ---------------------------------------------------------------------------

/// Parses `m:ss`, `mm:ss`, `h:mm:ss` (and a plain seconds value) into seconds.
/// Returns `None` for a non-numeric field or a minute/second component >= 60.
pub fn parse_clock(label: &str) -> Option<f64> {
    let parts = label
        .trim()
        .split(':')
        .map(|part| part.trim().parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [secs] => Some(*secs as f64),
        [mins, secs] if *secs < 60 => mins
            .checked_mul(60)
            .and_then(|v| v.checked_add(*secs))
            .map(|v| v as f64),
        [hours, mins, secs] if *mins < 60 && *secs < 60 => hours
            .checked_mul(3600)
            .and_then(|v| v.checked_add(mins.checked_mul(60)?))
            .and_then(|v| v.checked_add(*secs))
            .map(|v| v as f64),
        _ => None,
    }
}

/// Inverse of `parse_clock`: `m:ss` below an hour, `h:mm:ss` otherwise.
pub fn format_clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// Splits a UIA slider label such as `"1:23 / 4:56"` into `(position, duration)` seconds.
pub fn parse_progress_label(label: &str) -> Option<(f64, f64)> {
    let (position, duration) = label.split_once('/')?;
    Some((parse_clock(position.trim())?, parse_clock(duration.trim())?))
}

/// `LibreWolfMediaTimeline.ReadRange`: a progress bar must have a zero minimum, a finite
/// positive duration and a position inside `[0, duration]`; otherwise it may be a volume slider.
pub fn read_range(minimum: f64, duration: f64, position: f64) -> Option<(f64, f64)> {
    const MAX_SECONDS: f64 = 9.0e11; // TimeSpan.MaxValue.TotalSeconds
    (minimum == 0.0
        && duration.is_finite()
        && duration > 0.0
        && duration < MAX_SECONDS
        && position.is_finite()
        && position >= 0.0
        && position <= duration)
        .then_some((position, duration))
}

/// `LibreWolfMediaTimeline.ReadClockRange`: require the displayed clock to agree with the
/// numeric range either in seconds or in milliseconds before accepting it.
pub fn read_clock_range(
    minimum: f64,
    duration: f64,
    position: f64,
    label: &str,
) -> Option<(f64, f64)> {
    let (shown_position, shown_duration) = parse_progress_label(label)?;
    if duration.floor() == shown_duration && position.floor() == shown_position {
        return read_range(minimum, duration, position);
    }
    if (duration / 1000.0).floor() == shown_duration
        && (position / 1000.0).floor() == shown_position
    {
        return read_range(minimum, duration / 1000.0, position / 1000.0);
    }
    None
}

/// Exact-origin host check. Rejects `music.youtube.com.example`, explicit ports and user info.
pub fn url_host(address: &str) -> Option<&str> {
    let rest = address.trim().strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains(':') || authority.contains('@') {
        return None;
    }
    Some(authority)
}

/// `LibreWolfMediaTimeline.MatchesDocument` — only the exact YouTube Music origin may use
/// the named `progress-bar` control, and the tab title must contain the playing media title.
pub fn matches_document(address: &str, document_title: &str, media_title: &str) -> Option<bool> {
    if media_title.trim().is_empty()
        || !document_title
            .to_lowercase()
            .contains(&media_title.to_lowercase())
    {
        return None;
    }
    Some(url_host(address)? == "music.youtube.com")
}

/// Parses the UIA dump shape `host|document title|position / duration|control kind`. This is the
/// production parser formerly duplicated as a local helper in `opentaskbar-core/tests/media_timeline.rs`.
pub fn parse_uia_dump(dump: &str) -> Option<(String, f64, f64)> {
    let fields: Vec<&str> = dump.split('|').collect();
    if fields.len() < 4 || fields[0] != "music.youtube.com" {
        return None;
    }
    let title = fields[1].trim();
    let (position, duration) = parse_progress_label(fields[2])?;
    Some((title.to_owned(), position, duration))
}

/// Pure counterpart of the UIA read used by `librewolf_timeline`; kept public so the reference parity
/// tests can exercise the real production parser without a browser process.
pub fn librewolf_timeline_from_uia(
    address: &str,
    document_title: &str,
    media_title: &str,
    label: &str,
) -> Option<(f64, f64)> {
    if !matches_document(address, document_title, media_title)? {
        return None;
    }
    let (position, duration) = parse_progress_label(label)?;
    read_range(0.0, duration, position)
}

/// `progress_ratio` / `seek_position` helpers used by the seek slider.
pub fn progress_ratio(position: f64, duration: f64) -> f32 {
    if duration <= 0.0 {
        0.0
    } else {
        (position as f32 / duration as f32).clamp(0.0, 1.0)
    }
}

pub fn seek_position(ratio: f32, duration: u64) -> u64 {
    ((ratio.clamp(0.0, 1.0)) * duration as f32).round().max(0.0) as u64
}

/// Provider playback rates are clamped to a safe range (`apply_rate`). Pause is represented
/// separately by `MediaTimeline::update(rate = 0.0)`.
pub fn apply_rate(rate: f64) -> f64 {
    if !rate.is_finite() {
        1.0
    } else {
        rate.clamp(0.25, 4.0)
    }
}

/// `true` when a cached sample is older than its TTL. A clock going backwards is treated as fresh.
pub fn is_stale(last_update: u64, now: u64, ttl_ms: u64) -> bool {
    now.saturating_sub(last_update) > ttl_ms
}

// ---------------------------------------------------------------------------
// Real UIA production reader (`LibreWolfMediaTimeline.Read`).
// ---------------------------------------------------------------------------

struct ComInit {
    initialized: bool,
}

impl ComInit {
    fn new() -> Self {
        // SAFETY: no out-pointer; an already-initialized (possibly MTA) thread simply skips uninit.
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
        Self { initialized }
    }
}

impl Drop for ComInit {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: paired with the successful `CoInitializeEx` above.
            unsafe { CoUninitialize() };
        }
    }
}

fn executable_stem(value: &str) -> &str {
    diagnostics::exe_stem(value)
}

/// Maps live LibreWolf process ids to their executable stem.
fn librewolf_processes() -> HashMap<u32, String> {
    let mut pids = vec![0u32; 1024];
    let mut needed = 0u32;
    // SAFETY: `pids` is a live buffer; `needed` receives the required byte count.
    if unsafe {
        EnumProcesses(
            pids.as_mut_ptr(),
            (pids.len() * size_of::<u32>()) as u32,
            &mut needed,
        )
    } == 0
    {
        return HashMap::new();
    }
    let count = (needed as usize / size_of::<u32>()).min(pids.len());
    let mut out = HashMap::new();
    let mut buffer = [0u16; 260];
    for &pid in &pids[..count] {
        if pid == 0 {
            continue;
        }
        // SAFETY: the handle is closed below and `buffer` outlives the query.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                continue;
            }
            let mut len = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut len) != 0;
            let _ = CloseHandle(process);
            if !ok || len == 0 {
                continue;
            }
            let full = String::from_utf16_lossy(&buffer[..len as usize]);
            let stem = executable_stem(&full);
            if stem.eq_ignore_ascii_case("librewolf") {
                out.insert(pid, stem.to_owned());
            }
        }
    }
    out
}

struct WindowScan<'a> {
    processes: &'a HashMap<u32, String>,
    windows: Vec<(HWND, String)>,
}

/// Top-level visible windows owned by LibreWolf processes.
fn librewolf_windows(processes: &HashMap<u32, String>) -> Vec<(HWND, String)> {
    if processes.is_empty() {
        return Vec::new();
    }
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` points at a live `WindowScan` for the duration of `EnumWindows`.
        unsafe {
            let scan = &mut *(lparam as *mut WindowScan<'_>);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if let Some(stem) = scan.processes.get(&pid)
                && IsWindowVisible(hwnd) != 0
            {
                scan.windows.push((hwnd, stem.clone()));
            }
            1
        }
    }
    let mut scan = WindowScan {
        processes,
        windows: Vec::new(),
    };
    // SAFETY: the callback only reads the live `scan` and returns 1 to continue enumeration.
    unsafe {
        let _ = EnumWindows(Some(collect), &mut scan as *mut _ as LPARAM);
    }
    scan.windows
}

/// Reads the explicit window AppUserModelID (`SHGetPropertyStoreForWindow`, key `{9F4C2855-…} 5`).
unsafe fn window_app_id(hwnd: HWND) -> Option<String> {
    let key = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };
    // SAFETY: `hwnd` is a live top-level window from `EnumWindows`.
    unsafe {
        let store: IPropertyStore =
            SHGetPropertyStoreForWindow(windows::Win32::Foundation::HWND(hwnd)).ok()?;
        let value = store.GetValue(&key).ok()?;
        let vt = value.vt().0;
        // VT_LPWSTR / VT_BSTR are the string variants reference accepts.
        if vt != 31 && vt != 8 {
            return None;
        }
        let text = value.to_string();
        (!text.trim().is_empty()).then_some(text)
    }
}

fn source_matches_window(source_app_id: &str, app_id: Option<&str>, process_stem: &str) -> bool {
    let source = source_app_id.trim();
    if source.is_empty() {
        return true;
    }
    if app_id.is_some_and(|id| id.eq_ignore_ascii_case(source)) {
        return true;
    }
    // LibreWolf publishes its executable name as the SMTC source on some builds.
    let stem = executable_stem(source);
    process_stem.eq_ignore_ascii_case(stem) || process_stem.eq_ignore_ascii_case(source)
}

fn uia_control_timeline(element: &Element, is_youtube_music: bool) -> Option<(f64, f64)> {
    let (minimum, duration, position) = element.range_value()?;
    if is_youtube_music {
        return read_range(minimum, duration, position);
    }
    // A generic slider can also be a volume control: require a clock label agreeing with the range.
    let label = element.value_text()?;
    read_clock_range(minimum, duration, position, &label)
}

/// Reads the real progress bar of the LibreWolf window whose app id matches `source_app_id`.
/// Returns `None` when no unambiguous player is found (two matching players are ambiguous).
pub fn librewolf_timeline(source_app_id: &str, title: &str) -> Option<(f64, f64)> {
    if title.trim().is_empty() {
        return None;
    }
    let _com = ComInit::new();
    let uia = Automation::new()?;
    let processes = librewolf_processes();
    let document_condition = uia.property_condition_i32(
        uia_raw::UIA_CONTROL_TYPE_PROPERTY_ID,
        uia_raw::UIA_DOCUMENT_CONTROL_TYPE_ID,
    )?;
    let mut result: Option<(f64, f64)> = None;

    for (hwnd, process_stem) in librewolf_windows(&processes) {
        // SAFETY: `hwnd` is live for the duration of this iteration.
        let app_id = unsafe { window_app_id(hwnd) };
        if !source_matches_window(source_app_id, app_id.as_deref(), &process_stem) {
            continue;
        }
        let Some(root_com) = uia.element_from_handle(hwnd) else {
            continue;
        };
        let root = Element::from_com(root_com);
        let Some(documents_com) =
            root.find_all(uia_raw::TREE_SCOPE_DESCENDANTS, &document_condition)
        else {
            continue;
        };
        let Some(documents) = ElementArray::from_com(documents_com).elements() else {
            continue;
        };
        for document in documents {
            let address = document.value_text().unwrap_or_default();
            let document_title = document.current_name().unwrap_or_default();
            let Some(is_youtube_music) = matches_document(&address, &document_title, title) else {
                continue;
            };
            let found = find_controls(&uia, &document, is_youtube_music);
            if let Some(timeline) = found {
                if result.is_some() {
                    return None; // Ambiguous: the same title in multiple tabs/players.
                }
                result = Some(timeline);
            }
        }
    }
    result
}

/// Searches one document for progress controls. YouTube Music uses the named `progress-bar`;
/// generic documents use `Slider` controls whose value label must agree with the numeric range.
fn find_controls(
    uia: &Automation,
    document: &Element,
    is_youtube_music: bool,
) -> Option<(f64, f64)> {
    let condition = if is_youtube_music {
        uia.property_condition_text(uia_raw::UIA_AUTOMATION_ID_PROPERTY_ID, "progress-bar")?
    } else {
        uia.property_condition_i32(
            uia_raw::UIA_CONTROL_TYPE_PROPERTY_ID,
            uia_raw::UIA_SLIDER_CONTROL_TYPE_ID,
        )?
    };
    let controls_com = document.find_all(uia_raw::TREE_SCOPE_DESCENDANTS, &condition)?;
    let controls = ElementArray::from_com(controls_com).elements()?;
    let mut found = None;
    for control in controls {
        if let Some(timeline) = uia_control_timeline(&control, is_youtube_music) {
            if found.is_some() {
                return None; // Ambiguous within a document as well.
            }
            found = Some(timeline);
        }
    }
    found
}

// ---------------------------------------------------------------------------
// MediaTimeline state machine (`Widgets/MediaTimeline.cs`).
// ---------------------------------------------------------------------------

/// How long a confirmed seek suppresses a stale SMTC echo of the old position.
pub const SEEK_HOLD_SECS: f64 = 1.5;
/// A reported position farther than this from the confirmed estimate is considered stale.
pub const SEEK_STALE_TOLERANCE_SECS: f64 = 2.0;

#[derive(Clone, Debug, PartialEq)]
pub struct TimelineSample {
    /// Title/artist identity: a different item must be accepted even during a seek hold.
    pub identity: String,
    pub start_secs: f64,
    pub end_secs: f64,
    pub position_secs: f64,
    /// Monotonic seconds on the `WidgetRuntime` clock; `None` means an unstamped (1601) provider.
    pub updated_at: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct MediaTimeline {
    sample: Option<TimelineSample>,
    anchor_position: f64,
    anchor_timestamp: f64,
    rate: f64,
    seek_hold_until: Option<f64>,
    /// Last stale position rejected during a seek hold. Keeping this separate
    /// from `sample` prevents the stale echo from becoming the new comparison
    /// baseline (which could extend the hold or swallow real progress).
    last_rejected_position: Option<f64>,
}

impl MediaTimeline {
    pub fn duration(&self) -> f64 {
        self.sample
            .as_ref()
            .map_or(0.0, |s| (s.end_secs - s.start_secs).max(0.0))
    }

    /// Linear extrapolation from the last anchored sample, clamped to the item duration.
    pub fn position_at(&self, timestamp: f64) -> f64 {
        let elapsed = timestamp - self.anchor_timestamp;
        let advance = if elapsed.is_finite() && self.rate.is_finite() && elapsed > 0.0 {
            elapsed * self.rate
        } else {
            0.0
        };
        (self.anchor_position + advance).clamp(0.0, self.duration())
    }

    pub fn reset(&mut self) {
        self.sample = None;
        self.anchor_position = 0.0;
        self.anchor_timestamp = 0.0;
        self.rate = 0.0;
        self.seek_hold_until = None;
        self.last_rejected_position = None;
    }

    /// Applies an SMTC sample. A repeated sample does not restart the clock; a rate change freezes
    /// the position at that instant (so paused time is not counted); an unavailable timeline
    /// (`end <= start`, i.e. duration 0) clears both position and duration.
    pub fn update(&mut self, sample: TimelineSample, rate: f64, now_secs: f64, timestamp: f64) {
        if sample.end_secs <= sample.start_secs {
            self.reset();
            return;
        }
        let rate = if rate.is_finite() { rate } else { 1.0 };

        // Seek confirmation hold: a stale SMTC echo still reports the pre-seek position. Keep the
        // confirmed anchor, but remember the sample so a genuinely new item is still detectable.
        if let Some(until) = self.seek_hold_until
            && timestamp < until
            && self.sample.as_ref().is_some_and(|prev| {
                prev.identity == sample.identity && prev.end_secs == sample.end_secs
            })
        {
            let expected = self.position_at(timestamp);
            if (sample.position_secs - expected).abs() > SEEK_STALE_TOLERANCE_SECS {
                self.last_rejected_position = Some(sample.position_secs);
                return;
            }
            self.last_rejected_position = None;
        }

        let same = self.sample.as_ref() == Some(&sample);
        if !same {
            let reported = sample.position_secs - sample.start_secs;
            let duration = (sample.end_secs - sample.start_secs).max(0.0);
            let age = sample.updated_at.map_or(-1.0, |at| now_secs - at);
            let advance = age * rate;
            let mut anchor = reported;
            // Unstamped (1601) and future-dated provider clocks must not jump to the end.
            if sample.updated_at.is_some() && age >= 0.0 && advance <= duration - reported {
                anchor += advance;
            }
            self.anchor_position = anchor.clamp(0.0, duration);
            self.sample = Some(sample);
        } else if !self.rate.eq(&rate) {
            self.anchor_position = self.position_at(timestamp);
        } else {
            return;
        }
        self.anchor_timestamp = timestamp;
        self.rate = rate;
    }

    /// `ConfirmSeek`: anchors the new position immediately so a repeated old sample cannot
    /// rewind it, and opens a short hold window for the provider to catch up.
    pub fn confirm_seek(&mut self, seconds: f64, timestamp: f64) {
        self.anchor_position = seconds.clamp(0.0, self.duration());
        self.anchor_timestamp = timestamp;
        self.seek_hold_until = Some(timestamp + SEEK_HOLD_SECS);
        self.last_rejected_position = None;
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }
}

/// Free-function spelling of the contract API (`media_ext::confirm_seek`).
pub fn confirm_seek(timeline: &mut MediaTimeline, seconds: f64, timestamp_secs: f64) {
    timeline.confirm_seek(seconds, timestamp_secs);
}

/// Converts the SMTC snapshot into a `TimelineSample` on a monotonic `WidgetRuntime` clock.
pub fn timeline_from_snapshot(snap: &MediaSessionSnapshot, epoch: Instant) -> TimelineSample {
    let updated_at = snap
        .fetched_at
        .and_then(|at| at.checked_duration_since(epoch))
        .map(|d| d.as_secs_f64());
    TimelineSample {
        identity: format!("{}\u{1f}{}", snap.title, snap.artist),
        start_secs: 0.0,
        end_secs: snap.duration_secs as f64,
        position_secs: snap.position_secs as f64,
        updated_at,
    }
}

/// Position used by the chip/flyout: extrapolated while a timeline is available, otherwise the
/// raw SMTC value (preserves the previous behaviour for providers without a usable timeline).
pub fn displayed_position(
    timeline: &MediaTimeline,
    snap: &MediaSessionSnapshot,
    now_secs: f64,
) -> u64 {
    if timeline.duration() > 0.0 {
        timeline.position_at(now_secs).round().max(0.0) as u64
    } else {
        snap.position_secs
    }
}

// ---------------------------------------------------------------------------
// Flyout / cover / volume helpers.
// ---------------------------------------------------------------------------

/// Pure AUMID scoring used by the media flyout matcher (2 = AUMID, 1 = executable, 0 = none).
pub fn aumid_match_score(
    process_name: &str,
    app_user_model_id: Option<&str>,
    source_id: &str,
) -> u8 {
    crate::platform::core_audio::match_score(process_name, app_user_model_id, source_id, None)
}

/// Resolves the SMTC `SourceAppUserModelId` to a real WASAPI app session (reference
/// `AppAudioSessionMatcher.Select`). Returns the display name of the best AUMID/executable match.
pub fn match_aumid_app_name(source_id: &str) -> Option<String> {
    if source_id.trim().is_empty() {
        return None;
    }
    crate::platform::core_audio::find_app_session(
        &crate::platform::core_audio::list_app_sessions(),
        source_id,
    )
    .map(|session| session.name)
}

/// Builds the island expanded details payload from the SMTC snapshot + current audio state.
pub fn get_details(snap: &MediaSessionSnapshot, volume: u32, muted: bool) -> MediaDetails {
    let cover_path = find_cover_art(&snap.title, &snap.artist);
    let tint = cover_path
        .as_deref()
        .map_or((0x9f, 0x9f, 0xf2), tint_from_path);
    MediaDetails {
        title: snap.title.clone(),
        artist: snap.artist.clone(),
        album: String::new(),
        app_name: match_aumid_app_name(&snap.source_app_id).unwrap_or_default(),
        position_secs: snap.position_secs,
        duration_secs: snap.duration_secs,
        playing: snap.playing,
        cover_path,
        tint,
        volume,
        muted,
    }
}

/// `MediaWidget`缓存 album art under `%LOCALAPPDATA%\OpenTaskbar\Art` as
/// `<artist>-<title>.jpg` (sanitized). Returns the first existing cached image.
fn art_dir() -> PathBuf {
    opentaskbar_core::paths::shared_app_data_dir().join("Art")
}

pub fn find_cover_art(title: &str, artist: &str) -> Option<String> {
    let stem = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect()
    };
    let dir = art_dir();
    for name in [
        format!("-{}-{}.jpg", stem(artist), stem(title)),
        format!("{}.jpg", stem(title)),
    ] {
        let path = dir.join(name.trim_start_matches('-'));
        if path.is_file() {
            return Some(path.to_string_lossy().into_owned());
        }
    }
    None
}

/// Deterministic accent extracted from a cover path — reference uses the dominant pixel, which requires
/// a decoder we cannot call here; hashing gives a stable, tinted fallback.
pub fn tint_from_path(path: &str) -> Rgb {
    let mut h: u32 = 2166136261;
    for b in path.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(16777619);
    }
    let r = ((h >> 16) & 0xff) as u8;
    let g = ((h >> 8) & 0xff) as u8;
    let b = (h & 0xff) as u8;
    // Lift dark values so the tint stays visible on both light and dark surfaces.
    (r.max(0x60), g.max(0x60), b.max(0x60))
}

pub fn format_time(secs: u64) -> String {
    format_clock(secs)
}

/// Segoe Fluent Icons glyph for the transport button, matching reference playing state.
pub fn playing_glyph(playing: bool) -> &'static str {
    if playing { "\u{E769}" } else { "\u{E768}" } // Pause / Play
}

pub fn seek_ratio(position: u64, duration: u64) -> f32 {
    if duration == 0 {
        0.0
    } else {
        progress_ratio(position as f64, duration as f64)
    }
}

/// Full media flyout: cover + tint header, C2 `SeekSlider`, transport controls, vertical volume
/// section and interactive per-app sessions.
pub fn build_media_flyout(
    details: &MediaDetails,
    sessions: &[AudioAppSession],
    _settings: &DockSettings,
) -> FlyoutSpec {
    let mut blocks = Vec::new();
    if let Some(ref path) = details.cover_path {
        // C2 FlyoutBlock::Image — album cover / flag.
        blocks.push(FlyoutBlock::Image {
            glyph: 0xE93C, // MusicNote
            path: Some(path.clone()),
            size_dips: 56.0,
        });
    }
    blocks.push(FlyoutBlock::Hero {
        value: if details.title.is_empty() {
            loc::t("MediaNothingPlaying")
        } else {
            details.title.clone()
        },
        caption: if details.artist.is_empty() {
            Some(loc::t("MediaStartPlaybackHint"))
        } else {
            Some(details.artist.clone())
        },
        centered: false,
        large: false,
        right_meta: None,
    });
    if details.duration_secs > 0 {
        // C2 FlyoutBlock::SeekSlider + LiveEvent::Seek.
        blocks.push(FlyoutBlock::SeekSlider {
            id: 50,
            label: String::new(),
            position_secs: details.position_secs as u32,
            duration_secs: details.duration_secs as u32,
            ratio: seek_ratio(details.position_secs, details.duration_secs),
        });
        blocks.push(FlyoutBlock::StatRow {
            label: format_time(details.position_secs),
            value: format_time(details.duration_secs),
        });
    }
    blocks.push(FlyoutBlock::ButtonBar(vec![
        (1, loc::t("MediaPrevious"), false),
        (
            2,
            if details.playing {
                loc::t("FocusPause")
            } else {
                loc::t("MediaPlay")
            },
            true,
        ),
        (3, loc::t("MediaNext"), false),
    ]));

    // Vertical volume popover (`MediaWidget.VolumeFlyout`), stacked as an AudioSliderSection.
    blocks.push(FlyoutBlock::Separator);
    blocks.push(FlyoutBlock::AudioSliderSection {
        label: loc::t("Volume"),
        device_name: details.app_name.clone(),
        value_text: if details.muted {
            loc::t("AudioMuted")
        } else {
            super::format_percent(details.volume)
        },
        ratio: details.volume as f32 / 100.0,
        muted: details.muted,
        mute_id: 60,
        slider_id: 51,
    });

    // Per-app session volumes (reference per-app mixer). Slider/mute ids start at 120 so they never
    // collide with the transport / master volume ids above.
    if !sessions.is_empty() {
        blocks.push(FlyoutBlock::Separator);
        blocks.push(FlyoutBlock::StatRow {
            label: loc::t("MediaAppVolumes"),
            value: String::new(),
        });
        for (i, session) in sessions.iter().take(6).enumerate() {
            let slot = i as u32;
            blocks.push(FlyoutBlock::AudioSliderSection {
                label: session.app_name.clone(),
                device_name: String::new(),
                value_text: if session.muted {
                    loc::t("AudioMuted")
                } else {
                    super::format_percent(session.volume)
                },
                ratio: session.volume as f32 / 100.0,
                muted: session.muted,
                mute_id: 120 + slot * 2,
                slider_id: 121 + slot * 2,
            });
        }
    }
    FlyoutSpec {
        title: loc::t("DynamicIslandShortMedia"),
        subtitle: None,
        width_dips: 312.0,
        blocks,
    }
}

/// Applies a mouse-wheel remainder: `AudioWidget` uses 120 delta = 10%.
/// Returns `(new_pct, remainder)` so callers keep the fractional step across events.
pub fn volume_from_wheel(current_pct: u32, delta: i32, remainder: &mut i32) -> u32 {
    let total = *remainder + delta;
    let steps = total / 12; // 120 / 10% = 12 delta per percent
    *remainder = total % 12;
    (current_pct as i32 + steps).clamp(0, 100) as u32
}

/// Applies a mouse-wheel delta with a configurable percentage per 120-unit detent
/// (e.g. `pct_per_detent = 5` gives ±5% per standard wheel click with fractional carry).
pub fn volume_from_wheel_step(
    current_pct: u32,
    delta: i32,
    remainder: &mut i32,
    pct_per_detent: i32,
) -> u32 {
    let pct = pct_per_detent.clamp(1, 20);
    let total = *remainder + delta;
    let detents = total / 120;
    *remainder = total % 120;
    (current_pct as i32 + detents * pct).clamp(0, 100) as u32
}

// ---------------------------------------------------------------------------
// Per-app audio sessions.
// ---------------------------------------------------------------------------

/// Maps a `core_audio::AppAudioSession` into the widget-facing session using the
/// same stable id format as the mixer registry.
pub fn session_from_core(
    session: &crate::platform::core_audio::AppAudioSession,
) -> AudioAppSession {
    use crate::platform::core_audio::AppAudioSessionState;
    let volume = if session.volume.is_finite() {
        (session.volume.clamp(0.0, 1.0) * 100.0).round() as u32
    } else {
        0
    };
    let pid = session.process_id;
    let stem = executable_stem(&session.name);
    AudioAppSession {
        session_id: diagnostics::session_id(pid, stem),
        app_name: session.name.clone(),
        volume,
        muted: session.muted || session.state == AppAudioSessionState::Unavailable,
        icon_glyph: 0xE7F5,
    }
}

/// Lists per-app audio sessions from the canonical WASAPI enumerator in `diagnostics`.
/// Volume is clamped through the same finite-value guard as the mixer registry.
pub fn list_app_sessions() -> Vec<AudioAppSession> {
    diagnostics::list_app_audio_sessions()
        .iter()
        .map(|s| AudioAppSession {
            session_id: s.id.clone(),
            app_name: s.label.clone(),
            volume: (s.volume.clamp(0.0, 1.0) * 100.0).round() as u32,
            muted: s.muted,
            icon_glyph: 0xE7F5,
        })
        .collect()
}

/// Writes one app's volume (0-100). Returns `false` for an unknown session or a failed write.
pub fn set_app_volume(session_id: &str, pct: u32) -> bool {
    diagnostics::set_app_volume(session_id, (pct.min(100) as f32) / 100.0)
}

/// Mutes / unmutes one app session.
pub fn set_app_mute(session_id: &str, muted: bool) -> bool {
    diagnostics::set_app_mute(session_id, muted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::core_audio::{AppAudioSession as CoreSession, AppAudioSessionState};

    // --- Timeline clock parsing / reference parity helpers ---

    #[test]
    fn cover_art_uses_the_shared_art_folder() {
        assert_eq!(
            art_dir(),
            opentaskbar_core::paths::shared_app_data_dir().join("Art")
        );
        assert_eq!(art_dir().file_name().unwrap(), "Art");
        assert_eq!(
            art_dir().parent(),
            Some(opentaskbar_core::paths::shared_app_data_dir().as_path())
        );
    }

    #[test]
    fn media_time_formatting() {
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(65), "1:05");
        assert_eq!(format_time(3661), "1:01:01");
    }

    #[test]
    fn clock_labels_accept_minutes_seconds_and_hours() {
        assert_eq!(parse_clock("0:42"), Some(42.0));
        assert_eq!(parse_clock("1:23"), Some(83.0));
        assert_eq!(parse_clock("1:02:03"), Some(3723.0));
        assert_eq!(parse_clock("95"), Some(95.0));
        assert_eq!(parse_clock("1:99"), None);
        assert_eq!(parse_clock("1:02:99"), None);
        assert_eq!(parse_clock("n/a"), None);
    }

    #[test]
    fn clock_formatting_round_trips() {
        assert_eq!(format_clock(83), "1:23");
        assert_eq!(format_clock(3723), "1:02:03");
        assert_eq!(parse_clock(&format_clock(3723)), Some(3723.0));
    }

    #[test]
    fn seek_ratio_and_position_are_clamped() {
        assert!((progress_ratio(30.0, 120.0) - 0.25).abs() < f32::EPSILON);
        assert_eq!(progress_ratio(10.0, 0.0), 0.0);
        assert_eq!(progress_ratio(200.0, 100.0), 1.0);
        assert_eq!(seek_position(0.5, 200), 100);
        assert_eq!(seek_position(2.0, 100), 100);
        assert_eq!(seek_position(-1.0, 100), 0);
    }

    #[test]
    fn playback_rate_is_clamped_to_a_safe_range() {
        assert_eq!(apply_rate(1.0), 1.0);
        assert_eq!(apply_rate(0.1), 0.25);
        assert_eq!(apply_rate(10.0), 4.0);
        assert_eq!(apply_rate(f64::NAN), 1.0);
    }

    #[test]
    fn stale_samples_expire_after_the_time_to_live() {
        assert!(!is_stale(1000, 1200, 200));
        assert!(is_stale(1000, 1201, 200));
        assert!(!is_stale(2000, 1000, 200));
    }

    // --- LibreWolf UIA parser ---

    #[test]
    fn librewolf_timeline_extracts_title_and_progress() {
        let dump = "music.youtube.com|Bohemian Rhapsody|1:23 / 4:56|slider";
        let (title, position, duration) =
            parse_uia_dump(dump).expect("LibreWolf timeline must parse");
        assert_eq!(title, "Bohemian Rhapsody");
        assert_eq!(position, 83.0);
        assert_eq!(duration, 296.0);
        assert!((progress_ratio(position, duration) - 83.0 / 296.0).abs() < 0.001);
    }

    #[test]
    fn librewolf_parser_rejects_foreign_sites_and_bad_labels() {
        assert!(parse_uia_dump("youtube.com|Song|1:00 / 2:00|slider").is_none());
        assert!(parse_uia_dump("music.youtube.com|Song|bad / 2:00|slider").is_none());
        assert!(parse_uia_dump("music.youtube.com|Song").is_none());
    }

    #[test]
    fn only_exact_youtube_music_origin_uses_named_progress_bar() {
        assert_eq!(
            url_host("https://music.youtube.com/watch"),
            Some("music.youtube.com")
        );
        assert_eq!(
            url_host("https://music.youtube.com.example/watch"),
            Some("music.youtube.com.example")
        );
        assert_eq!(
            matches_document(
                "https://music.youtube.com.example/watch",
                "Artist | Track",
                "Track"
            ),
            Some(false)
        );
        assert!(url_host("https://music.youtube.com:444/watch").is_none());
        assert!(url_host("http://music.youtube.com/watch").is_none());
        assert_eq!(
            matches_document("https://music.youtube.com/watch", "Artist | Track", "Track"),
            Some(true)
        );
        assert_eq!(
            matches_document(
                "https://music.youtube.com/watch",
                "Different track",
                "Expected track"
            ),
            None
        );
        assert_eq!(
            matches_document("https://example.com/watch", "Artist | Track", "Track"),
            Some(false)
        );
    }

    #[test]
    fn browser_range_and_clock_range_validation() {
        assert_eq!(read_range(0.0, 294.0, 281.0), Some((281.0, 294.0)));
        assert_eq!(read_range(0.0, 0.0, 0.0), None);
        assert_eq!(read_range(0.0, f64::INFINITY, 20.0), None);
        assert_eq!(read_range(0.0, 60.0, 70.0), None);
        assert_eq!(
            read_clock_range(0.0, 32.835, 10.62, "0:10 / 0:32"),
            read_range(0.0, 32.835, 10.62)
        );
        assert_eq!(
            read_clock_range(0.0, 64800.0, 12400.0, "0:12 / 1:04"),
            read_range(0.0, 64.8, 12.4)
        );
        assert_eq!(read_clock_range(0.0, 100.0, 38.0, "38%"), None);
        assert_eq!(read_clock_range(0.0, 30.0, 10.0, "0:27 / 0:30"), None);
        assert_eq!(
            librewolf_timeline_from_uia(
                "https://music.youtube.com/watch",
                "Artist | Track",
                "Track",
                "4:41 / 4:56"
            ),
            Some((281.0, 296.0))
        );
        assert_eq!(
            librewolf_timeline_from_uia(
                "https://example.com/watch",
                "Artist | Track",
                "Track",
                "4:41 / 4:56"
            ),
            None
        );
    }

    // --- MediaTimeline / confirm_seek: port of MediaVolumeChecks.cs:90-123 ---

    fn sample(
        identity: &str,
        start: f64,
        end: f64,
        position: f64,
        updated: Option<f64>,
    ) -> TimelineSample {
        TimelineSample {
            identity: identity.to_owned(),
            start_secs: start,
            end_secs: end,
            position_secs: position,
            updated_at: updated,
        }
    }

    #[test]
    fn timeline_anchors_and_rate_changes_like_reference() {
        let mut timeline = MediaTimeline::default();
        let unstamped = sample("song", 0.0, 60.0, 0.0, None);
        timeline.update(unstamped.clone(), 1.0, 0.0, 0.0);
        assert_eq!(
            timeline.position_at(0.0),
            0.0,
            "an unstamped timeline must not jump to the end"
        );
        timeline.update(unstamped.clone(), 1.0, 8.0, 8.0);
        assert_eq!(
            timeline.position_at(8.0),
            8.0,
            "polling the same timeline must not restart the clock"
        );
        timeline.update(unstamped.clone(), 0.0, 9.0, 9.0);
        assert_eq!(
            timeline.position_at(20.0),
            9.0,
            "pausing must freeze the last estimated position"
        );
        timeline.update(unstamped.clone(), 2.0, 20.0, 20.0);
        assert_eq!(
            timeline.position_at(22.0),
            13.0,
            "resume must exclude paused time and respect rate"
        );
        confirm_seek(&mut timeline, 40.0, 22.0);
        timeline.update(unstamped.clone(), 2.0, 24.0, 24.0);
        assert_eq!(
            timeline.position_at(24.0),
            44.0,
            "a confirmed seek must survive a repeated old sample"
        );
        assert_eq!(
            timeline.position_at(40.0),
            60.0,
            "progress must stop at the end of the item"
        );
    }

    #[test]
    fn timeline_new_item_stale_and_future_timestamps() {
        let mut timeline = MediaTimeline::default();
        timeline.update(sample("first", 0.0, 60.0, 0.0, None), 1.0, 0.0, 0.0);
        let next = sample("second", 5.0, 30.0, 5.0, Some(41.0));
        timeline.update(next.clone(), 1.0, 43.0, 43.0);
        assert_eq!(timeline.duration(), 25.0);
        assert_eq!(
            timeline.position_at(43.0),
            2.0,
            "a new item starts from its own timestamp"
        );
        timeline.update(
            sample("second", 5.0, 30.0, 12.0, Some(-3600.0)),
            1.0,
            44.0,
            44.0,
        );
        assert_eq!(
            timeline.position_at(44.0),
            7.0,
            "a stale provider timestamp must not jump to the end"
        );
        timeline.update(
            sample("second", 5.0, 30.0, 25.0, Some(35.0)),
            1.0,
            45.0,
            45.0,
        );
        assert_eq!(
            timeline.position_at(45.0),
            20.0,
            "a future timestamp must not fake completion"
        );
        timeline.update(sample("second", 5.0, 5.0, 0.0, Some(46.0)), 0.0, 46.0, 46.0);
        assert_eq!(
            timeline.duration(),
            0.0,
            "duration 0 clears the previous item"
        );
        assert_eq!(timeline.position_at(46.0), 0.0);
    }

    #[test]
    fn confirm_seek_rejects_stale_smtc_echo() {
        let mut timeline = MediaTimeline::default();
        let snapshot = sample("song", 0.0, 100.0, 0.0, Some(0.0));
        timeline.update(snapshot.clone(), 1.0, 0.0, 0.0);
        confirm_seek(&mut timeline, 40.0, 10.0);
        assert_eq!(timeline.position_at(10.0), 40.0);
        // Provider still reports the pre-seek position within the hold window,
        // and keeps echoing it on several consecutive ticks.
        for tick in 0..3 {
            let now = 11.0 + tick as f64 * 0.25;
            timeline.update(snapshot.clone(), 1.0, now, now);
            assert_eq!(
                timeline.position_at(now),
                40.0 + now - 10.0,
                "stale echo must not rewind the seek"
            );
        }
        // Real progress from the same item is accepted immediately afterwards.
        let progressed = sample("song", 0.0, 100.0, 44.0, Some(44.0));
        timeline.update(progressed.clone(), 1.0, 14.0, 14.0);
        assert_eq!(
            timeline.position_at(14.0),
            44.0,
            "real progress after a stale echo must not be treated as 'same'"
        );
        // A genuinely different item is accepted even inside the hold window.
        timeline.update(sample("other", 0.0, 100.0, 0.0, None), 1.0, 15.0, 15.0);
        assert_eq!(timeline.position_at(15.0), 0.0);
    }

    #[test]
    fn pause_and_zero_duration_are_stable() {
        let mut timeline = MediaTimeline::default();
        let item = sample("song", 0.0, 120.0, 60.0, Some(0.0));
        timeline.update(item.clone(), 0.0, 0.0, 0.0);
        timeline.update(item.clone(), 0.0, 10.0, 10.0);
        assert_eq!(
            timeline.position_at(30.0),
            60.0,
            "rate 0 must freeze progress"
        );
        assert_eq!(timeline.rate(), 0.0);
        timeline.update(sample("song", 0.0, 0.0, 0.0, None), 1.0, 31.0, 31.0);
        assert_eq!(timeline.duration(), 0.0);
        assert_eq!(timeline.position_at(31.0), 0.0);
    }

    #[test]
    fn snapshot_to_timeline_is_stable_across_frames() {
        let mut snap = MediaSessionSnapshot {
            title: "Song".into(),
            artist: "Artist".into(),
            playing: true,
            position_secs: 12,
            duration_secs: 100,
            ..Default::default()
        };
        let epoch = Instant::now();
        snap.fetched_at = Some(epoch + std::time::Duration::from_millis(500));
        let first = timeline_from_snapshot(&snap, epoch);
        let second = timeline_from_snapshot(&snap, epoch);
        assert_eq!(
            first, second,
            "the same SMTC fetch must produce the same sample"
        );
        assert_eq!(first.updated_at, Some(0.5));
        assert_eq!(first.identity, "Song\u{1f}Artist");
    }

    // --- Existing helpers ---

    #[test]
    fn seek_ratio_clamps() {
        assert_eq!(seek_ratio(30, 120), 0.25);
        assert_eq!(seek_ratio(10, 0), 0.0);
        assert_eq!(seek_ratio(200, 100), 1.0);
    }

    #[test]
    fn playing_glyph_switches() {
        assert_eq!(playing_glyph(true), "\u{E769}");
        assert_eq!(playing_glyph(false), "\u{E768}");
    }

    #[test]
    fn wheel_remainder_accumulates() {
        let mut rem = 0;
        assert_eq!(volume_from_wheel(50, 5, &mut rem), 50);
        assert_eq!(rem, 5);
        assert_eq!(volume_from_wheel(50, 7, &mut rem), 51);
        assert_eq!(rem, 0);
    }

    #[test]
    fn details_from_snapshot() {
        let snap = MediaSessionSnapshot {
            title: "Song".into(),
            artist: "Artist".into(),
            playing: true,
            position_secs: 10,
            duration_secs: 100,
            ..Default::default()
        };
        let d = get_details(&snap, 42, false);
        assert_eq!(d.title, "Song");
        assert!(d.playing);
        assert_eq!(d.volume, 42);
    }

    #[test]
    fn ws4_session_adapter_maps_real_core_type() {
        let core = CoreSession {
            process_id: 4242,
            name: r"C:\Apps\spotify.exe".to_owned(),
            app_user_model_id: None,
            window_app_user_model_id: None,
            state: AppAudioSessionState::Active,
            volume: 0.73,
            muted: false,
        };
        let mapped = session_from_core(&core);
        assert_eq!(mapped.session_id, "4242:spotify");
        assert_eq!(mapped.volume, 73);
        assert!(!mapped.muted);
    }
}
