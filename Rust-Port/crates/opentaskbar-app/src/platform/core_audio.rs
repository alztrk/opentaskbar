//! Core Audio (WASAPI) endpoint discovery, default-device switching and per-app sessions.
//!
//! C# parity: `Services/Interop/CoreAudio.cs`.
//! - `list_devices(is_recording)` -> `Vec<(id, name)>` of active endpoints.
//! - `set_default_device(id)` -> bool via the undocumented `PolicyConfigClient`
//!   (`SetDefaultEndpoint` for Console/Multimedia/Communications roles).
//! - `subscribe()` -> shared ref-counted `AudioEndpointMonitor` with an `IMMNotificationClient`
//!   feeding `take_events()`; `AudioEndpointMonitor::acquire_shared()` mirrors the C# ref-count.
//! - `AppAudioSessionMatcher::score` + `find_app_session` for per-app mixer targeting.
//!
//! NOTE for lead/WS-B: `platform::win32_ex::set_default_audio_device(id)` currently forwards to
//! telemetry; it should delegate to `core_audio::set_default_device(id)` after merge.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eMultimedia,
    eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, STGM_READ};
use windows::core::{GUID, Interface, PCWSTR};

/// Endpoint as `(endpoint id, friendly name)`.
pub type AudioDevice = (String, String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioEventKind {
    DefaultDeviceChanged,
    DefaultMicChanged,
    DeviceAdded,
    DeviceRemoved,
    DeviceStateChanged,
}

#[derive(Clone, Debug)]
pub struct AudioEvent {
    pub kind: AudioEventKind,
    pub device_id: String,
}

static EVENTS: Mutex<VecDeque<AudioEvent>> = Mutex::new(VecDeque::new());
const EVENT_CAP: usize = 128;

fn push_event(kind: AudioEventKind, id: &windows::core::PCWSTR) {
    let device_id = if id.is_null() {
        String::new()
    } else {
        unsafe { id.to_string().unwrap_or_default() }
    };
    if let Ok(mut q) = EVENTS.lock() {
        if q.len() >= EVENT_CAP {
            q.pop_front();
        }
        q.push_back(AudioEvent { kind, device_id });
    }
}

// ---------------------------------------------------------------------------------------------
// IMMNotificationClient (hand-written COM vtable: the `windows::core::implement` proc macro
// requires a direct `windows_core` crate dependency, which is lead-owned in Cargo.toml).
// ---------------------------------------------------------------------------------------------

#[repr(C)]
struct NotifyVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    on_device_state_changed: unsafe extern "system" fn(*mut c_void, PCWSTR, u32) -> i32,
    on_device_added: unsafe extern "system" fn(*mut c_void, PCWSTR) -> i32,
    on_device_removed: unsafe extern "system" fn(*mut c_void, PCWSTR) -> i32,
    on_default_device_changed: unsafe extern "system" fn(*mut c_void, i32, i32, PCWSTR) -> i32,
    on_property_value_changed:
        unsafe extern "system" fn(*mut c_void, PCWSTR, *const PROPERTYKEY) -> i32,
}

#[repr(C)]
struct NotifyObject {
    vtable: *const NotifyVtbl,
    refs: AtomicU32,
}

unsafe extern "system" fn notify_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() || iid.is_null() {
        return -2147467261; // E_INVALIDARG
    }
    let wanted = unsafe { *iid };
    if wanted == GUID::zeroed()
        || wanted == <windows::core::IUnknown as Interface>::IID
        || wanted == <windows::Win32::Media::Audio::IMMNotificationClient as Interface>::IID
    {
        unsafe {
            let obj = &*(this as *const NotifyObject);
            obj.refs.fetch_add(1, Ordering::AcqRel);
            *out = this;
        }
        0 // S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        -2147467262 // E_NOINTERFACE
    }
}

unsafe extern "system" fn notify_add_ref(this: *mut c_void) -> u32 {
    let obj = unsafe { &*(this as *const NotifyObject) };
    obj.refs.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "system" fn notify_release(this: *mut c_void) -> u32 {
    let obj = unsafe { &*(this as *const NotifyObject) };
    let remaining = obj.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if remaining == 0 {
        unsafe { drop(Box::from_raw(this as *mut NotifyObject)) };
    }
    remaining
}

unsafe extern "system" fn notify_device_added(_this: *mut c_void, id: PCWSTR) -> i32 {
    push_event(AudioEventKind::DeviceAdded, &id);
    0
}

unsafe extern "system" fn notify_device_removed(_this: *mut c_void, id: PCWSTR) -> i32 {
    push_event(AudioEventKind::DeviceRemoved, &id);
    0
}

unsafe extern "system" fn notify_default_changed(
    _this: *mut c_void,
    flow: i32,
    role: i32,
    id: PCWSTR,
) -> i32 {
    if role != 1 {
        return 0; // only multimedia role
    }
    let kind = if flow == eRender.0 {
        AudioEventKind::DefaultDeviceChanged
    } else if flow == eCapture.0 {
        AudioEventKind::DefaultMicChanged
    } else {
        return 0;
    };
    push_event(kind, &id);
    0
}

unsafe extern "system" fn notify_property_changed(
    _this: *mut c_void,
    _id: PCWSTR,
    _key: *const PROPERTYKEY,
) -> i32 {
    0
}

fn create_notification_client() -> Option<windows::Win32::Media::Audio::IMMNotificationClient> {
    static VTBL: NotifyVtbl = NotifyVtbl {
        query_interface: notify_query_interface,
        add_ref: notify_add_ref,
        release: notify_release,
        on_device_state_changed: notify_state_changed,
        on_device_added: notify_device_added,
        on_device_removed: notify_device_removed,
        on_default_device_changed: notify_default_changed,
        on_property_value_changed: notify_property_changed,
    };
    unsafe extern "system" fn notify_state_changed(
        _this: *mut c_void,
        id: PCWSTR,
        _state: u32,
    ) -> i32 {
        push_event(AudioEventKind::DeviceStateChanged, &id);
        0
    }
    let boxed = Box::new(NotifyObject {
        vtable: &VTBL,
        refs: AtomicU32::new(1),
    });
    let raw = Box::into_raw(boxed);
    Some(unsafe { windows::Win32::Media::Audio::IMMNotificationClient::from_raw(raw.cast()) })
}

// ---------------------------------------------------------------------------------------------
// PolicyConfigClient (undocumented COM interface; declared locally to avoid Cargo.toml changes)
// ---------------------------------------------------------------------------------------------

const POLICY_CONFIG_CLSID: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

#[repr(C)]
struct PolicyConfigVtbl {
    base__: windows::core::IUnknown_Vtbl,
    get_mix_format: unsafe extern "system" fn(*mut c_void, PCWSTR, *mut *mut c_void) -> i32,
    get_device_format: unsafe extern "system" fn(*mut c_void, PCWSTR, i32, *mut *mut c_void) -> i32,
    reset_device_format: unsafe extern "system" fn(*mut c_void, PCWSTR) -> i32,
    set_device_format:
        unsafe extern "system" fn(*mut c_void, PCWSTR, *mut c_void, *mut c_void) -> i32,
    get_processing_period:
        unsafe extern "system" fn(*mut c_void, PCWSTR, i32, *mut i64, *mut i64) -> i32,
    set_processing_period: unsafe extern "system" fn(*mut c_void, *mut i64) -> i32,
    get_share_mode: unsafe extern "system" fn(*mut c_void, PCWSTR, *mut *mut c_void) -> i32,
    set_share_mode: unsafe extern "system" fn(*mut c_void, PCWSTR, *mut c_void) -> i32,
    get_property_value:
        unsafe extern "system" fn(*mut c_void, PCWSTR, i32, *mut c_void, *mut *mut c_void) -> i32,
    set_property_value:
        unsafe extern "system" fn(*mut c_void, PCWSTR, i32, *mut c_void, *mut c_void) -> i32,
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> i32,
    set_endpoint_visibility: unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> i32,
}

#[repr(transparent)]
#[derive(Clone)]
struct PolicyConfig(windows::core::IUnknown);

unsafe impl Interface for PolicyConfig {
    type Vtable = PolicyConfigVtbl;
    const IID: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
}

impl PolicyConfig {
    unsafe fn set_default_endpoint(&self, id: &windows::core::HSTRING, role: i32) -> bool {
        unsafe {
            let vtbl = Interface::vtable(self);
            let hr = (vtbl.set_default_endpoint)(
                Interface::as_raw(self),
                PCWSTR::from_raw(id.as_ptr()),
                role,
            );
            hr >= 0
        }
    }
}

/// Sets the default endpoint for Console (0), Multimedia (1) and Communications (2) roles.
pub fn set_default_device(id: &str) -> bool {
    if id.trim().is_empty() {
        return false;
    }
    let wide = windows::core::HSTRING::from(id);
    unsafe {
        let Ok(policy) =
            CoCreateInstance::<_, PolicyConfig>(&POLICY_CONFIG_CLSID, None, CLSCTX_ALL)
        else {
            return false;
        };
        [0i32, 1, 2]
            .iter()
            .all(|&role| policy.set_default_endpoint(&wide, role))
    }
}

/// Alias matching C4 (`platform::win32_ex::set_default_audio_device`).
pub fn set_default_audio_device(id: &str) -> bool {
    set_default_device(id)
}

// ---------------------------------------------------------------------------------------------
// Device enumeration
// ---------------------------------------------------------------------------------------------

// PKEY_Device_FriendlyName = {a45c254e-df1c-4efd-8020-67d146a850e0}, pid 14
const FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

fn friendly_name(device: &windows::Win32::Media::Audio::IMMDevice) -> Option<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let value = store.GetValue(&FRIENDLY_NAME).ok()?;
        let text = value.to_string();
        (!text.trim().is_empty()).then(|| text.trim().to_owned())
    }
}

fn enumerator() -> Option<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() }
}

/// All active audio endpoints. `(endpoint_id, friendly_name)` sorted with the default first.
pub fn list_devices(is_recording: bool) -> Vec<AudioDevice> {
    fn collect(flow: EDataFlow) -> Vec<AudioDevice> {
        let Some(en) = enumerator() else {
            return Vec::new();
        };
        unsafe {
            let default_id = en
                .GetDefaultAudioEndpoint(flow, eMultimedia)
                .ok()
                .and_then(|d| d.GetId().ok())
                .and_then(|s| s.to_string().ok())
                .unwrap_or_default();
            let Ok(collection) = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else {
                return Vec::new();
            };
            let Ok(count) = collection.GetCount() else {
                return Vec::new();
            };
            let mut out = Vec::with_capacity(count as usize);
            for i in 0..count {
                let Ok(device) = collection.Item(i) else {
                    continue;
                };
                let Ok(id) = device.GetId() else {
                    continue;
                };
                let id = id.to_string().unwrap_or_default();
                let name = friendly_name(&device).unwrap_or_else(|| {
                    if flow == eCapture {
                        "Microphone".to_owned()
                    } else {
                        "Speaker".to_owned()
                    }
                });
                out.push((id, name));
            }
            out.sort_by(|a, b| {
                let a_def = a.0.eq_ignore_ascii_case(&default_id);
                let b_def = b.0.eq_ignore_ascii_case(&default_id);
                b_def
                    .cmp(&a_def)
                    .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
            });
            out
        }
    }
    collect(if is_recording { eCapture } else { eRender })
}

/// Convenience tuple alias for consumers that prefer `(name, id)` ordering.
pub fn list_devices_named(is_recording: bool) -> Vec<(String, String)> {
    list_devices(is_recording)
        .into_iter()
        .map(|(id, name)| (name, id))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Shared monitor / ref-count
// ---------------------------------------------------------------------------------------------

/// Shared, ref-counted endpoint monitor (C# `AudioEndpointMonitor.AcquireShared`).
///
/// The COM enumerator + notification client are registered once and kept alive for the
/// process lifetime; the Arc handle only implements the C# ref-count semantics.
#[derive(Debug)]
pub struct AudioEndpointMonitor {
    generation: usize,
}

static SHARED: OnceLock<Mutex<Weak<AudioEndpointMonitor>>> = OnceLock::new();
static NOTIFY_REGISTERED: OnceLock<()> = OnceLock::new();

impl AudioEndpointMonitor {
    /// Acquires (or creates) the process-wide monitor.
    pub fn acquire_shared() -> Option<Arc<AudioEndpointMonitor>> {
        let lock = SHARED.get_or_init(|| Mutex::new(Weak::new()));
        let mut guard = lock.lock().ok()?;
        if let Some(existing) = guard.upgrade() {
            return Some(existing);
        }
        if NOTIFY_REGISTERED.get().is_none()
            && let Some(en) = enumerator()
            && let Some(client) = create_notification_client()
            && unsafe { en.RegisterEndpointNotificationCallback(&client) }.is_ok()
        {
            // Keep both COM objects alive for the lifetime of the process.
            std::mem::forget(en);
            std::mem::forget(client);
            let _ = NOTIFY_REGISTERED.set(());
        }
        let monitor = Arc::new(AudioEndpointMonitor { generation: 1 });
        *guard = Arc::downgrade(&monitor);
        Some(monitor)
    }

    pub fn generation(&self) -> usize {
        self.generation
    }
}

/// C4 `subscribe()` — acquires the shared monitor and returns a strong handle.
pub fn subscribe() -> Option<Arc<AudioEndpointMonitor>> {
    AudioEndpointMonitor::acquire_shared()
}

/// Drains queued `IMMNotificationClient` events.
pub fn take_events() -> Vec<AudioEvent> {
    EVENTS
        .lock()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default()
}

/// True when any endpoint notification was received (used by the widget to re-query).
pub fn has_pending_events() -> bool {
    EVENTS.lock().map(|q| !q.is_empty()).unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// App session matcher (C# `AppAudioSessionMatcher`)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppAudioSessionState {
    Inactive,
    Active,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct AppAudioSession {
    pub process_id: u32,
    pub name: String,
    pub app_user_model_id: Option<String>,
    pub window_app_user_model_id: Option<String>,
    pub state: AppAudioSessionState,
    pub volume: f32,
    pub muted: bool,
}

fn executable_name(value: &str) -> &str {
    let name = value.trim();
    let name = name.rsplit(['\\', '/']).next().unwrap_or(name);
    name.strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".EXE"))
        .unwrap_or(name)
}

/// Scoring: 2 = AUMID match, 1 = executable match, 0 = no match.
pub fn match_score(
    process_name: &str,
    app_user_model_id: Option<&str>,
    source_id: &str,
    window_app_user_model_id: Option<&str>,
) -> u8 {
    if source_id.trim().is_empty() {
        return 0;
    }
    if app_user_model_id.is_some_and(|id| id.eq_ignore_ascii_case(source_id))
        || window_app_user_model_id.is_some_and(|id| id.eq_ignore_ascii_case(source_id))
    {
        return 2;
    }
    let app = executable_name(process_name);
    if app.is_empty() {
        return 0;
    }
    if let Some((_, application)) = source_id.split_once('!') {
        return if app_user_model_id.is_none()
            && !application.eq_ignore_ascii_case("App")
            && executable_name(application).eq_ignore_ascii_case(app)
        {
            1
        } else {
            0
        };
    }
    let source = executable_name(source_id);
    if source.eq_ignore_ascii_case(app) {
        return 1;
    }
    // Chromium app/profile IDs append dot-separated components to the browser executable.
    if (app.eq_ignore_ascii_case("chrome") || app.eq_ignore_ascii_case("msedge"))
        && source.len() > app.len()
        && source.as_bytes()[app.len()] == b'.'
        && source[..app.len()].eq_ignore_ascii_case(app)
    {
        return 1;
    }
    0
}

/// Implementation of `AppAudioSessionMatcher.Select`: highest rank wins; ties are ambiguous
/// and resolve to `None` (the caller must not control an arbitrary app).
pub fn find_app_session(sessions: &[AppAudioSession], source_id: &str) -> Option<AppAudioSession> {
    let mut best: Option<&AppAudioSession> = None;
    let mut best_rank = 0i32;
    for session in sessions {
        let score = match_score(
            &session.name,
            session.app_user_model_id.as_deref(),
            source_id,
            session.window_app_user_model_id.as_deref(),
        ) as i32;
        if session.process_id == 0
            || score == 0
            || session.state == AppAudioSessionState::Unavailable
        {
            continue;
        }
        let rank = score * 2 + i32::from(session.state == AppAudioSessionState::Active);
        if rank < best_rank {
            continue;
        }
        best = if rank == best_rank {
            None
        } else {
            Some(session)
        };
        best_rank = rank;
    }
    best.cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_scores_aumid_and_executable() {
        assert_eq!(match_score("chrome", None, "chrome", None), 1);
        assert_eq!(match_score("chrome", Some("chrome"), "CHROME", None), 2);
        assert_eq!(match_score("chrome", None, "chrome.extra.1", None), 1);
        assert_eq!(match_score("app", None, "", None), 0);
        assert_eq!(match_score("app", None, "a!App", None), 0);
    }

    #[test]
    fn matcher_select_rejects_ties() {
        let a = AppAudioSession {
            process_id: 10,
            name: "chrome".into(),
            app_user_model_id: None,
            window_app_user_model_id: None,
            state: AppAudioSessionState::Active,
            volume: 0.5,
            muted: false,
        };
        let b = AppAudioSession {
            process_id: 11,
            name: "chrome".into(),
            app_user_model_id: None,
            window_app_user_model_id: None,
            state: AppAudioSessionState::Active,
            volume: 0.8,
            muted: false,
        };
        assert!(find_app_session(&[a.clone(), b], "chrome").is_none());
        let c = AppAudioSession {
            process_id: 12,
            name: "spotify".into(),
            app_user_model_id: Some("spotify".into()),
            window_app_user_model_id: None,
            state: AppAudioSessionState::Inactive,
            volume: 1.0,
            muted: false,
        };
        let picked = find_app_session(&[a, c], "spotify").unwrap();
        assert_eq!(picked.process_id, 12);
    }

    #[test]
    fn executable_name_strips_path_and_extension() {
        assert_eq!(executable_name(r"C:\Program Files\App.exe"), "App");
        assert_eq!(executable_name("msedge.EXE"), "msedge");
    }

    #[test]
    fn set_default_device_rejects_empty_id() {
        assert!(!set_default_device(""));
    }
}
