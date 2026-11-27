//! Core Audio (WASAPI) endpoint discovery, default-device switching and per-app sessions.
//!
//! reference parity: `Services/Interop/CoreAudio.cs`.
//! - `list_devices(is_recording)` -> `Vec<(id, name)>` of active endpoints.
//! - `set_default_device(id)` -> bool via the undocumented `PolicyConfigClient`
//!   (`SetDefaultEndpoint` for Console/Multimedia/Communications roles).
//! - `subscribe()` -> shared ref-counted `AudioEndpointMonitor` with an `IMMNotificationClient`
//!   feeding `take_events()`; `AudioEndpointMonitor::acquire_shared()` mirrors the reference ref-count.
//! - `AppAudioSessionMatcher::score` + `find_app_session` for per-app mixer targeting.
//!
//! NOTE for lead/WS-B: `platform::win32_ex::set_default_audio_device(id)` currently forwards to
//! telemetry; it should delegate to `core_audio::set_default_device(id)` after merge.

use std::collections::{HashSet, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, AudioSessionStateInactive, DEVICE_STATE_ACTIVE, EDataFlow,
    IAudioSessionControl, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator, eCapture, eMultimedia, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::core::{GUID, Interface, PCWSTR, PWSTR};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

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

/// Process-wide device generation. Every real `IMMNotificationClient` callback
/// bumps it, so consumers can invalidate cached device lists even when several
/// `subscribe()` handles are alive.
static DEVICE_GENERATION: AtomicUsize = AtomicUsize::new(1);

/// Current endpoint generation (increases on every consumed device event).
pub fn device_generation() -> usize {
    DEVICE_GENERATION.load(Ordering::Acquire)
}

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
    // Notification callbacks really drive the generation, not a fake timer.
    DEVICE_GENERATION.fetch_add(1, Ordering::AcqRel);
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

/// Shared, ref-counted endpoint monitor (`AudioEndpointMonitor.AcquireShared`).
///
/// The COM enumerator + notification client are registered once and kept alive for the
/// process lifetime; the Arc handle only implements the reference ref-count semantics.
#[derive(Debug)]
pub struct AudioEndpointMonitor;

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
        let monitor = Arc::new(AudioEndpointMonitor);
        *guard = Arc::downgrade(&monitor);
        Some(monitor)
    }

    pub fn generation(&self) -> usize {
        device_generation()
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
// App session matcher (`AppAudioSessionMatcher`)
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

// ---------------------------------------------------------------------------------------------
// Real WASAPI app sessions (`CoreAudio.ListAppSessions` / `AppAudioSessionMatcher`)
// ---------------------------------------------------------------------------------------------

/// Stable mixer id shared with the widget/dock layers: `"<pid>:<exe-stem>"`.
pub fn session_id(process_id: u32, exe_stem: &str) -> String {
    let stem: String = exe_stem
        .trim()
        .chars()
        .map(|c| if c == ':' || c.is_control() { '_' } else { c })
        .collect();
    format!("{process_id}:{stem}")
}

/// Parses a [`session_id`] back into `(pid, exe_stem)`.
pub fn parse_session_id(id: &str) -> Option<(u32, &str)> {
    let (pid, stem) = id.split_once(':')?;
    let pid = pid.parse::<u32>().ok()?;
    (!stem.is_empty() && !stem.contains(':')).then_some((pid, stem))
}

fn clamp_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Balances `CoInitializeEx`; a mismatched apartment leaves COM untouched.
struct ComGuard {
    initialized: bool,
}

impl ComGuard {
    fn new() -> Self {
        // SAFETY: no out-pointers; an RPC_E_CHANGED_MODE error means this thread
        // already has another apartment, which we must not uninitialize.
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
        Self { initialized }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: paired with a successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

/// Copies and frees a COM-allocated `PWSTR`.
unsafe fn take_pwstr(text: PWSTR) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: COM owns a NUL-terminated wide string at this pointer.
    unsafe {
        let value = text.to_string().ok();
        CoTaskMemFree(Some(text.as_ptr().cast()));
        value
    }
}

/// Process executable stem via `QueryFullProcessImageNameW`; never dereferences a PID.
fn exe_stem_for_pid(pid: u32) -> Option<String> {
    // SAFETY: OpenProcess returns an owned handle; the buffer outlives the call.
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
        let stem = executable_name(&full);
        (!stem.is_empty()).then(|| stem.to_owned())
    }
}

/// Best-effort AUMID from the session instance identifier (`...|<AUMID>|...`).
fn aumid_from_instance(identifier: &str) -> Option<String> {
    let mut candidate = None;
    for part in identifier.split(['|', '\\']) {
        let part = part.trim();
        if part.len() > 16 && part.contains('!') && is_packaged_aumid(part) {
            candidate = Some(part.to_owned());
            break;
        }
    }
    candidate
}

fn is_packaged_aumid(id: &str) -> bool {
    let Some((family, app)) = id.split_once('!') else {
        return false;
    };
    let Some((_, publisher)) = family.rsplit_once('_') else {
        return false;
    };
    publisher.len() == 13 && publisher.bytes().all(|c| c.is_ascii_alphanumeric()) && !app.is_empty()
}

/// Converts one `IAudioSessionControl` into a mixer row. The caller owns `control`.
unsafe fn session_from_control(
    control: &IAudioSessionControl,
    own_pid: u32,
) -> Option<AppAudioSession> {
    // SAFETY: `control` is a live COM interface from `IAudioSessionEnumerator::GetSession`.
    unsafe {
        let state_value = control.GetState().ok()?.0;
        let state = match state_value {
            x if x == AudioSessionStateActive.0 => AppAudioSessionState::Active,
            x if x == AudioSessionStateInactive.0 => AppAudioSessionState::Inactive,
            _ => return None, // expired session
        };
        let ctl2 = control.cast::<IAudioSessionControl2>().ok()?;
        let process_id = ctl2.GetProcessId().unwrap_or(0);
        let system_sounds = ctl2.IsSystemSoundsSession().is_ok();
        if process_id == 0 || (process_id == own_pid && !system_sounds) {
            return None;
        }
        let volume_interface = control.cast::<ISimpleAudioVolume>().ok()?;
        let volume = clamp_unit(volume_interface.GetMasterVolume().ok()?);
        let muted = volume_interface.GetMute().ok()?.as_bool();
        let instance = ctl2
            .GetSessionInstanceIdentifier()
            .ok()
            .and_then(|text| take_pwstr(text));
        let app_user_model_id = instance.as_deref().and_then(aumid_from_instance);
        let name = if system_sounds {
            "SystemSounds".to_owned()
        } else {
            exe_stem_for_pid(process_id).or_else(|| {
                control
                    .GetDisplayName()
                    .ok()
                    .and_then(|text| take_pwstr(text))
                    .map(|name| name.trim().to_owned())
            })?
        };
        Some(AppAudioSession {
            process_id,
            name,
            app_user_model_id,
            window_app_user_model_id: None,
            state,
            volume,
            muted,
        })
    }
}

/// Enumerates real per-app sessions on all active render endpoints, deduplicated by stable id.
///
/// Returns an empty vector on headless/no-audio machines; always safe to call from any thread.
pub fn list_app_sessions() -> Vec<AppAudioSession> {
    let _com = ComGuard::new();
    let mut sessions = Vec::new();
    let mut seen = HashSet::new();
    // SAFETY: every COM interface below owns its reference and releases it on drop.
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
            let Ok(session_enum) = manager.GetSessionEnumerator() else {
                continue;
            };
            let Ok(session_count) = session_enum.GetCount() else {
                continue;
            };
            for session_index in 0..session_count.clamp(0, 64) {
                let Ok(control) = session_enum.GetSession(session_index) else {
                    continue;
                };
                if let Some(session) = session_from_control(&control, own_pid)
                    && seen.insert(session_id(session.process_id, &session.name))
                {
                    sessions.push(session);
                }
            }
        }
    }
    sessions
}

/// Finds the live `ISimpleAudioVolume` for a stable `"<pid>:<stem>"` id.
unsafe fn find_session_volume(id: &str) -> Option<ISimpleAudioVolume> {
    let (pid, stem) = parse_session_id(id)?;
    // SAFETY: all interfaces are owned locals and release themselves on drop.
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
                let matches = if stem == "SystemSounds" {
                    ctl2.IsSystemSoundsSession().is_ok()
                } else {
                    exe_stem_for_pid(pid).is_some_and(|name| name == stem)
                };
                if matches {
                    return control.cast::<ISimpleAudioVolume>().ok();
                }
            }
        }
    }
    None
}

/// Writes a real per-app volume in `[0, 1]`. `false` for unknown ids / COM failures.
pub fn set_app_volume(id: &str, volume: f32) -> bool {
    if !volume.is_finite() {
        return false;
    }
    let _com = ComGuard::new();
    // SAFETY: `find_session_volume` only returns a live COM interface.
    unsafe {
        let Some(session) = find_session_volume(id) else {
            return false;
        };
        session
            .SetMasterVolume(clamp_unit(volume), std::ptr::null())
            .is_ok()
    }
}

/// Mutes / unmutes a real per-app session. `false` for unknown ids / COM failures.
pub fn set_app_mute(id: &str, muted: bool) -> bool {
    let _com = ComGuard::new();
    // SAFETY: `find_session_volume` only returns a live COM interface.
    unsafe {
        let Some(session) = find_session_volume(id) else {
            return false;
        };
        session.SetMute(muted, std::ptr::null()).is_ok()
    }
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
    fn device_events_are_consumed_and_bump_generation() {
        let before = device_generation();
        push_event(AudioEventKind::DeviceAdded, &PCWSTR::null());
        assert!(device_generation() > before);
        let events = take_events();
        assert!(
            events
                .iter()
                .any(|event| event.kind == AudioEventKind::DeviceAdded)
        );
        assert!(!has_pending_events());
        assert!(device_generation() > before);
    }

    #[test]
    fn matcher_scores_packaged_aumid_source_ids() {
        const AUMID: &str = "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify";
        let session = AppAudioSession {
            process_id: 24,
            name: "Spotify.exe".into(),
            app_user_model_id: Some(AUMID.to_owned()),
            window_app_user_model_id: None,
            state: AppAudioSessionState::Active,
            volume: 0.4,
            muted: false,
        };
        assert_eq!(match_score(&session.name, Some(AUMID), AUMID, None), 2);
        assert_eq!(
            find_app_session(std::slice::from_ref(&session), AUMID).map(|found| found.process_id),
            Some(24)
        );
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

    #[test]
    fn session_ids_round_trip_and_sanitize() {
        assert_eq!(session_id(42, "chrome"), "42:chrome");
        assert_eq!(parse_session_id("42:chrome"), Some((42, "chrome")));
        assert_eq!(session_id(7, "a:b\nc"), "7:a_b_c");
        assert_eq!(parse_session_id("bad"), None);
        assert_eq!(parse_session_id("x:y"), None);
        assert_eq!(parse_session_id("1:a:b"), None);
    }

    #[test]
    fn aumid_is_recovered_from_session_instance_identifier() {
        let id = "Microsoft.Windows.ShellExperienceHost_cw5n1h2txyewy!App";
        let instance = format!(r"\\?\audioendpoint|1|1234|{id}|0");
        assert_eq!(aumid_from_instance(&instance).as_deref(), Some(id));
        assert_eq!(aumid_from_instance("no-aumid-here"), None);
    }

    #[test]
    fn app_session_setters_reject_invalid_ids_without_com() {
        assert!(!set_app_volume("", 0.5));
        assert!(!set_app_mute("not-a-session", true));
        assert!(!set_app_volume("1:app", f32::NAN));
        assert_eq!(clamp_unit(2.0), 1.0);
    }

    #[test]
    fn endpoint_events_are_queued_for_widgets() {
        let before = take_events().len();
        push_event(AudioEventKind::DeviceAdded, &PCWSTR::null());
        assert!(!take_events().is_empty());
        assert!(take_events().is_empty());
        let _ = before;
    }
}
