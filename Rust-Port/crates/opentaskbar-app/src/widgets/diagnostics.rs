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

use std::collections::HashSet;

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

use super::telemetry;

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

/// Stable session key: `"<pid>:<exe-stem>"` (C# used `pid:hwn?`, which is not stable across restarts).
fn session_id(pid: u32, exe_stem: &str) -> String {
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
fn exe_stem(path: &str) -> &str {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path).trim();
    if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

// ---------------------------------------------------------------------------
// GPU snapshot (all sampling lives in telemetry)
// ---------------------------------------------------------------------------

/// Snapshot of GPU utilization and (optionally) temperature.
#[derive(Clone, Debug, Default)]
pub struct GpuSnapshot {
    /// Busiest GPU engine load in `[0, 1]`; `0.0` when unavailable.
    pub usage_01: f32,
    /// Hottest GPU in degrees Celsius via WDDM (or NVML fallback); `None` when unavailable.
    pub temperature_c: Option<f32>,
    /// Engine kind of the busiest instance (e.g. `"3D"`, `"Video Decode"`); empty when unknown.
    pub engine_name: String,
}

/// Fast, non-blocking sample: open errors, lock contention and PDH failures yield `usage_01 = 0`.
pub fn sample_gpu() -> GpuSnapshot {
    GpuSnapshot {
        usage_01: telemetry::sample_gpu_usage().unwrap_or(0.0),
        temperature_c: telemetry::sample_gpu_temperature(),
        engine_name: telemetry::sample_gpu_engine_name().unwrap_or_default(),
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
            "System Sounds".to_owned()
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
