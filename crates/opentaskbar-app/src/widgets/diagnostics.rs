//! GPU snapshot and per-application audio mixer data providers (data layer only, no UI).
//!
//! GPU utilization, engine name and temperature come from `telemetry`; this module only assembles
//! them into `GpuSnapshot` for the stats flyout. Application audio sessions are enumerated through
//! WASAPI (`IMMDeviceEnumerator` -> `IAudioSessionManager2` -> `IAudioSessionEnumerator`) with
//! `ISimpleAudioVolume` for control.
//!
//! All COM objects are reference-counted `windows` interfaces and are released by their `Drop`
//! implementation. Strings returned by COM are freed with `CoTaskMemFree`, and COM initialization
//! is balanced by `ComGuard`.

use std::collections::{HashMap, HashSet};

use opentaskbar_core::i18n as loc;

use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IAudioSessionControl, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::core::{Interface, PWSTR};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

use crate::platform::core_audio::{
    AppAudioSession as CoreAppAudioSession, AppAudioSessionState, AudioEvent, AudioEventKind,
    find_app_session,
};

// ---------------------------------------------------------------------------
// Small pure helpers (also used by the unit tests at the bottom).
// ---------------------------------------------------------------------------

/// Clamps to `[0, 1]`; non-finite input becomes `0.0` rather than panicking.
fn clamp_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Stable session key: `"<pid>:<exe-stem>"` (reference used `pid:hwn?`, which is not stable across restarts).
pub(crate) fn session_id(pid: u32, exe_stem: &str) -> String {
    format!("{pid}:{}", sanitize_stem(exe_stem))
}

fn parse_session_id(id: &str) -> Option<(u32, &str)> {
    let (pid, stem) = id.split_once(':')?;
    let pid = pid.parse::<u32>().ok()?;
    if stem.is_empty() || stem.contains(':') {
        return None;
    }
    Some((pid, stem))
}

fn sanitize_stem(stem: &str) -> String {
    stem.trim()
        .chars()
        .map(|c| if c == ':' || c.is_control() { '_' } else { c })
        .collect()
}

/// File-name stem without the `.exe` suffix (case-insensitive).
pub(crate) fn exe_stem(path: &str) -> &str {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path).trim();
    if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

// ---------------------------------------------------------------------------
// Per-application audio sessions (WASAPI)
// ---------------------------------------------------------------------------

/// One mixer row. `id` is the stable `"<pid>:<exe-stem>"` key used by the setters.
#[derive(Clone, Debug)]
pub struct AppAudioSession {
    pub id: String,
    pub label: String,
    pub volume: f32,
    pub muted: bool,
}

/// Balances `CoInitializeEx` for callers that are not already initialized on this thread.
struct ComGuard {
    initialized: bool,
}

impl ComGuard {
    fn new() -> Self {
        // SAFETY: no out-pointer; `RPC_E_CHANGED_MODE` is handled by skipping the matching uninit.
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
        Self { initialized }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: paired with a successful `CoInitializeEx` above.
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// Copies a COM-allocated `PWSTR` and frees it with `CoTaskMemFree`.
unsafe fn take_pwstr(text: PWSTR) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: COM owns a NUL-terminated wide string at this pointer.
    unsafe {
        let value = text.to_string().ok();
        CoTaskMemFree(Some(text.as_ptr().cast()));
        value.filter(|value| !value.is_empty())
    }
}

/// Reads a process' executable stem with `QueryFullProcessImageNameW`.
fn exe_stem_for_pid(pid: u32) -> Option<String> {
    // SAFETY: `OpenProcess` returns a handle we close below; the buffer outlives the call.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut len) != 0;
        let _ = CloseHandle(process);
        if !ok || len == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buffer[..len as usize]);
        let stem = exe_stem(&full).to_owned();
        (!stem.is_empty()).then_some(stem)
    }
}

/// Human/machine name of a session: system sounds, executable stem, or display name fallback.
unsafe fn session_stem(
    control: &IAudioSessionControl,
    ctl2: &IAudioSessionControl2,
    pid: u32,
) -> Option<String> {
    // SAFETY: all interfaces are live COM objects obtained from the session enumerator.
    unsafe {
        if pid == 0 || ctl2.IsSystemSoundsSession().is_ok() {
            return Some("SystemSounds".to_owned());
        }
        if let Some(stem) = exe_stem_for_pid(pid) {
            return Some(stem);
        }
        match control.GetDisplayName() {
            Ok(name) => take_pwstr(name),
            Err(_) => None,
        }
    }
}

unsafe fn session_from_control(
    control: &IAudioSessionControl,
    own_pid: u32,
) -> Option<AppAudioSession> {
    // SAFETY: `control` is a live `IAudioSessionControl` from `IAudioSessionEnumerator::GetSession`.
    unsafe {
        let state = control.GetState().ok()?.0;
        if !(0..=1).contains(&state) {
            return None; // inactive/active only; skip expired sessions.
        }
        let ctl2 = control.cast::<IAudioSessionControl2>().ok()?;
        let pid = ctl2.GetProcessId().unwrap_or(0);
        if pid == own_pid && ctl2.IsSystemSoundsSession().is_err() {
            return None;
        }
        let volume_interface = control.cast::<ISimpleAudioVolume>().ok()?;
        let volume = clamp_unit(volume_interface.GetMasterVolume().ok()?);
        let muted = volume_interface.GetMute().ok()?.as_bool();
        let stem = session_stem(control, &ctl2, pid).unwrap_or_else(|| format!("pid-{pid}"));
        let label = if stem == "SystemSounds" {
            loc::t("SystemSounds")
        } else {
            stem.clone()
        };
        Some(AppAudioSession {
            id: session_id(pid, &stem),
            label,
            volume,
            muted,
        })
    }
}

/// Lists app mixer rows from all active render endpoints (de-duplicated by stable id).
pub fn list_app_audio_sessions() -> Vec<AppAudioSession> {
    let _com = ComGuard::new();
    let mut sessions = Vec::new();
    let mut seen = HashSet::new();

    // SAFETY: COM interfaces manage their own reference counts and are released on Drop.
    unsafe {
        let Ok(enumerator) =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return sessions;
        };
        let Ok(devices) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else {
            return sessions;
        };
        let Ok(count) = devices.GetCount() else {
            return sessions;
        };
        let own_pid = std::process::id();
        for device_index in 0..count.min(64) {
            let Ok(device) = devices.Item(device_index) else {
                continue;
            };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(enumerator) = manager.GetSessionEnumerator() else {
                continue;
            };
            let Ok(session_count) = enumerator.GetCount() else {
                continue;
            };
            for session_index in 0..session_count.clamp(0, 64) {
                let Ok(control) = enumerator.GetSession(session_index) else {
                    continue;
                };
                if let Some(session) = session_from_control(&control, own_pid)
                    && seen.insert(session.id.clone())
                {
                    sessions.push(session);
                }
            }
        }
    }
    sessions
}

/// Locates the live `ISimpleAudioVolume` for a stable `"<pid>:<exe-stem>"` id.
unsafe fn find_session_volume(pid: u32, stem: &str) -> Option<ISimpleAudioVolume> {
    // SAFETY: all COM interfaces are owned by local variables and release on Drop.
    unsafe {
        let enumerator =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .ok()?;
        let devices = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .ok()?;
        for device_index in 0..devices.GetCount().ok()?.min(64) {
            let Ok(device) = devices.Item(device_index) else {
                continue;
            };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(sessions) = manager.GetSessionEnumerator() else {
                continue;
            };
            let Ok(count) = sessions.GetCount() else {
                continue;
            };
            for session_index in 0..count.clamp(0, 64) {
                let Ok(control) = sessions.GetSession(session_index) else {
                    continue;
                };
                let Ok(ctl2) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                if ctl2.GetProcessId().unwrap_or(0) != pid {
                    continue;
                }
                let matches = session_stem(&control, &ctl2, pid)
                    .is_some_and(|name| sanitize_stem(&name) == stem);
                if !matches {
                    continue;
                }
                if let Ok(volume) = control.cast::<ISimpleAudioVolume>() {
                    return Some(volume);
                }
            }
        }
        None
    }
}

/// Sets an app's volume in `[0, 1]`. Returns `false` for an unknown id or invalid value.
pub fn set_app_volume(id: &str, volume_01: f32) -> bool {
    if !volume_01.is_finite() {
        return false;
    }
    let Some((pid, stem)) = parse_session_id(id) else {
        return false;
    };
    let _com = ComGuard::new();
    // SAFETY: `find_session_volume` only returns a live COM interface.
    unsafe {
        let Some(volume) = find_session_volume(pid, stem) else {
            return false;
        };
        volume
            .SetMasterVolume(clamp_unit(volume_01), std::ptr::null())
            .is_ok()
    }
}

/// Mutes / unmutes an app session. Returns `false` for an unknown id.
pub fn set_app_mute(id: &str, muted: bool) -> bool {
    let Some((pid, stem)) = parse_session_id(id) else {
        return false;
    };
    let _com = ComGuard::new();
    // SAFETY: `find_session_volume` only returns a live COM interface.
    unsafe {
        let Some(volume) = find_session_volume(pid, stem) else {
            return false;
        };
        volume.SetMute(muted, std::ptr::null()).is_ok()
    }
}

// ---------------------------------------------------------------------------
// Pure unit tests (no COM / PDH required).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_is_clamped_to_unit_range() {
        assert_eq!(clamp_unit(-0.25), 0.0);
        assert_eq!(clamp_unit(0.37), 0.37);
        assert_eq!(clamp_unit(2.5), 1.0);
        assert_eq!(clamp_unit(f32::NAN), 0.0);
        assert_eq!(clamp_unit(f32::INFINITY), 0.0);
    }

    #[test]
    fn session_id_round_trips() {
        let id = session_id(4242, "chrome");
        assert_eq!(id, "4242:chrome");
        assert_eq!(parse_session_id(&id), Some((4242, "chrome")));
        assert_eq!(parse_session_id("4242:"), None);
        assert_eq!(parse_session_id("not-a-pid:chrome"), None);
        assert_eq!(parse_session_id("no-separator"), None);
    }

    #[test]
    fn exe_stem_strips_path_and_extension() {
        assert_eq!(exe_stem(r"C:\Program Files\App\chrome.EXE"), "chrome");
        assert_eq!(exe_stem("app"), "app");
    }
}

// ---------------------------------------------------------------------------
// WS-G: session lifetime / optimistic volume rollback (core_audio based).
// ---------------------------------------------------------------------------

/// A single optimistic volume change with rollback support.
#[derive(Clone, Debug)]
pub struct VolumeRollback {
    pub session_key: String,
    pub previous_volume: f32,
    pub previous_mute: bool,
    pub requested_volume: f32,
    committed: bool,
}

impl VolumeRollback {
    pub fn begin(
        session_key: &str,
        previous_volume: f32,
        previous_mute: bool,
        requested: f32,
    ) -> Self {
        Self {
            session_key: session_key.to_owned(),
            previous_volume,
            previous_mute,
            requested_volume: clamp_unit(requested),
            committed: false,
        }
    }

    /// Commits the change; returns the value the UI should show.
    pub fn commit(&mut self, write_ok: bool) -> f32 {
        self.committed = write_ok;
        if write_ok {
            self.requested_volume
        } else {
            self.previous_volume
        }
    }

    /// Restores the original level if the write failed (or the session vanished).
    pub fn rollback(&self) -> Option<(f32, bool)> {
        (!self.committed).then_some((self.previous_volume, self.previous_mute))
    }

    pub fn needs_rollback(&self) -> bool {
        !self.committed
    }
}

/// Maps mixer slider IDs to released session handles.
#[derive(Default)]
pub struct SessionRegistry {
    sessions: HashMap<u32, CoreAppAudioSession>,
    rollbacks: HashMap<u32, VolumeRollback>,
}

impl SessionRegistry {
    pub fn attach(&mut self, slider_id: u32, session: CoreAppAudioSession) {
        self.release(slider_id);
        self.sessions.insert(slider_id, session);
        self.rollbacks.remove(&slider_id);
    }

    /// Releases the COM session held by a slider. Idempotent (`Dispose` guard).
    /// Any pending optimistic rollback is dropped with the session so a reused
    /// slider cannot apply a stale level from a previous owner.
    pub fn release(&mut self, slider_id: u32) -> bool {
        let removed = self.sessions.remove(&slider_id).is_some();
        self.rollbacks.remove(&slider_id);
        removed
    }

    pub fn release_all(&mut self) {
        self.sessions.clear();
        self.rollbacks.clear();
    }

    pub fn get(&self, slider_id: u32) -> Option<&CoreAppAudioSession> {
        self.sessions.get(&slider_id)
    }

    /// Starts an optimistic volume change for a session.
    pub fn begin_volume_change(
        &mut self,
        slider_id: u32,
        requested: f32,
    ) -> Option<VolumeRollback> {
        let session = self.sessions.get(&slider_id)?;
        let rollback =
            VolumeRollback::begin(&session.name, session.volume, session.muted, requested);
        self.rollbacks.insert(slider_id, rollback.clone());
        Some(rollback)
    }

    /// Completes a volume change: on failure the original level is restored in the snapshot.
    pub fn finish_volume_change(&mut self, slider_id: u32, write_ok: bool) -> Option<f32> {
        let mut rollback = self.rollbacks.remove(&slider_id)?;
        // The session may have been released while the write was in flight.
        if !self.sessions.contains_key(&slider_id) {
            return None;
        }
        let shown = rollback.commit(write_ok);
        if !write_ok {
            if let Some(session) = self.sessions.get_mut(&slider_id) {
                session.volume = rollback.previous_volume;
                session.muted = rollback.previous_mute;
            }
        } else if let Some(session) = self.sessions.get_mut(&slider_id) {
            session.volume = shown;
        }
        Some(shown)
    }

    pub fn active_count(&self) -> usize {
        self.sessions.len()
    }

    /// Drops sessions whose audio event says the endpoint/app went away.
    pub fn on_events(&mut self, events: &[AudioEvent]) -> usize {
        let mut removed = 0usize;
        for event in events {
            if !matches!(
                event.kind,
                AudioEventKind::DefaultDeviceChanged
                    | AudioEventKind::DeviceRemoved
                    | AudioEventKind::DeviceStateChanged
            ) {
                continue;
            }
            let stale: Vec<u32> = self
                .sessions
                .iter()
                .filter(|(_, s)| s.state == AppAudioSessionState::Unavailable)
                .map(|(id, _)| *id)
                .collect();
            for id in stale {
                self.release(id);
                removed += 1;
            }
        }
        removed
    }
}

/// Picks the unambiguous session for a media source, then releases all rejected sessions.
pub fn select_and_release(
    sessions: Vec<CoreAppAudioSession>,
    source_id: &str,
) -> Option<CoreAppAudioSession> {
    let selected = find_app_session(&sessions, source_id);
    // In Rust, dropping the unused `CoreAppAudioSession` values releases their handles.
    drop(sessions);
    selected
}

#[cfg(test)]
mod ws_g_tests {
    use super::*;

    fn session(pid: u32, name: &str, volume: f32) -> CoreAppAudioSession {
        CoreAppAudioSession {
            process_id: pid,
            name: name.to_owned(),
            app_user_model_id: None,
            window_app_user_model_id: None,
            state: AppAudioSessionState::Active,
            volume,
            muted: false,
        }
    }

    #[test]
    fn volume_rollback_on_failure() {
        let mut r = VolumeRollback::begin("app", 0.3, false, 0.8);
        assert_eq!(r.commit(false), 0.3);
        assert_eq!(r.rollback(), Some((0.3, false)));
        let mut ok = VolumeRollback::begin("app", 0.3, false, 0.8);
        assert_eq!(ok.commit(true), 0.8);
        assert!(ok.rollback().is_none());
    }

    #[test]
    fn registry_releases_sessions_idempotently() {
        let mut reg = SessionRegistry::default();
        reg.attach(1, session(10, "chrome", 0.5));
        assert_eq!(reg.active_count(), 1);
        assert!(reg.release(1));
        assert!(!reg.release(1));
        assert_eq!(reg.active_count(), 0);
    }

    #[test]
    fn rollback_is_dropped_when_a_session_is_released() {
        let mut reg = SessionRegistry::default();
        reg.attach(3, session(10, "spotify", 0.4));
        assert!(reg.begin_volume_change(3, 0.9).is_some());
        assert!(reg.release(3));
        assert!(reg.rollbacks.is_empty());
        // Reattaching a new session must not inherit the stale rollback.
        reg.attach(3, session(11, "chrome", 0.2));
        assert!(reg.rollbacks.is_empty());
        assert_eq!(reg.finish_volume_change(3, true), None);
    }

    #[test]
    fn non_finite_requested_volume_becomes_zero() {
        let rollback = VolumeRollback::begin("app", 0.5, false, f32::NAN);
        assert_eq!(rollback.requested_volume, 0.0);
        let mut rollback = rollback;
        assert_eq!(rollback.commit(true), 0.0);
    }

    #[test]
    fn registry_failed_write_restores_snapshot() {
        let mut reg = SessionRegistry::default();
        reg.attach(7, session(10, "spotify", 0.4));
        let rollback = reg.begin_volume_change(7, 0.9).unwrap();
        assert_eq!(rollback.previous_volume, 0.4);
        assert_eq!(reg.finish_volume_change(7, false), Some(0.4));
        assert_eq!(reg.get(7).unwrap().volume, 0.4);
    }

    #[test]
    fn media_source_selection_drops_rejected_sessions() {
        let picked = select_and_release(
            vec![session(1, "chrome", 0.5), session(2, "spotify", 0.7)],
            "spotify",
        );
        assert_eq!(picked.unwrap().process_id, 2);
    }

    #[test]
    fn endpoint_events_release_unavailable_sessions() {
        let mut reg = SessionRegistry::default();
        let mut s = session(10, "app", 0.5);
        s.state = AppAudioSessionState::Unavailable;
        reg.attach(2, s);
        let events = vec![AudioEvent {
            kind: AudioEventKind::DefaultDeviceChanged,
            device_id: String::new(),
        }];
        assert_eq!(reg.on_events(&events), 1);
        assert_eq!(reg.active_count(), 0);
    }
}
