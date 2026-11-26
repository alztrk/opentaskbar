//! WinRT `Windows.Devices.Radios.Radio` bridge for the Quick Settings widget.
//!
//! The typed `windows::Devices::Radios` bindings require the `Devices_Radios`
//! Cargo feature, which lives in the lead-owned workspace `Cargo.toml`. To keep
//! this worktree buildable without touching that file, the small set of WinRT
//! interfaces we need is activated and called directly through their COM
//! vtables (`RoGetActivationFactory` + `RequestAccessAsync` +
//! `GetRadiosAsync` + `SetStateAsync`). A background refresh thread keeps the
//! cached Wi-Fi/Bluetooth state current; typed `StateChanged` handlers can be
//! layered on by the lead once the feature is available.
//!
//! C4 API: `state(kind: &str) -> bool`, `set(kind: &str, on: bool) -> bool`.

use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI8, Ordering};
use std::time::Duration;

use windows::core::{GUID, HSTRING};

use opentaskbar_core::log;

const RADIO_ACCESS_STATUS_ALLOWED: i32 = 1;
const ASYNC_STATUS_COMPLETED: i32 = 4;
const RADIO_STATE_OFF: i32 = 1;
const RADIO_STATE_ON: i32 = 2;
const RADIO_STATE_DISABLED: i32 = 3;
const RADIO_KIND_WIFI: i32 = 1;
const RADIO_KIND_BLUETOOTH: i32 = 2;
const RO_INIT_MULTITHREADED: i32 = 1;
const ASYNC_POLL_LIMIT: usize = 200; // 2 s at 10 ms
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

// IIDs from Windows.Devices.Radios metadata.
const IID_RADIO_STATICS: GUID = GUID::from_u128(0x5fb6_a12e_67cb_46ae_aae9_6591_9f86_eff4);

// WinRT activation entry points come from combase.dll. Loading them dynamically
// avoids a `combase.lib` import-library dependency (absent from some SDKs).
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(file_name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const i8) -> *mut c_void;
}

type RoInitializeFn = unsafe extern "system" fn(i32) -> i32;
type RoUninitializeFn = unsafe extern "system" fn();
type RoGetActivationFactoryFn =
    unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32;

struct Combase {
    initialize: RoInitializeFn,
    uninitialize: RoUninitializeFn,
    get_activation_factory: RoGetActivationFactoryFn,
}

fn combase() -> Option<&'static Combase> {
    static API: std::sync::OnceLock<Option<Combase>> = std::sync::OnceLock::new();
    API.get_or_init(|| {
        let name: Vec<u16> = "combase.dll\0".encode_utf16().collect();
        // SAFETY: null-terminated module name; the module stays loaded for the
        // lifetime of the process (deliberately never freed).
        let module = unsafe { LoadLibraryW(name.as_ptr()) };
        if module.is_null() {
            return None;
        }
        // SAFETY: function pointers are cast to their exact WinRT signatures.
        unsafe {
            let symbol = |name: &core::ffi::CStr| {
                let address = GetProcAddress(module, name.as_ptr());
                (!address.is_null()).then_some(address)
            };
            let initialize: RoInitializeFn = std::mem::transmute(symbol(c"RoInitialize")?);
            let uninitialize: RoUninitializeFn = std::mem::transmute(symbol(c"RoUninitialize")?);
            let get_activation_factory: RoGetActivationFactoryFn =
                std::mem::transmute(symbol(c"RoGetActivationFactory")?);
            Some(Combase {
                initialize,
                uninitialize,
                get_activation_factory,
            })
        }
    })
    .as_ref()
}

/// COM vtable prefix shared by all WinRT interfaces (IUnknown + IInspectable).
#[repr(C)]
struct WinRtVtbl {
    _query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    _add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    _get_iids: unsafe extern "system" fn(*mut c_void, *mut u32, *mut *mut GUID) -> i32,
    _get_runtime_class_name: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    _get_trust_level: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
}

#[repr(C)]
struct RadioStaticsVtbl {
    base: WinRtVtbl,
    get_radios_async: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    _get_device_selector: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    _from_id_async: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32,
    request_access_async: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct RadioVtbl {
    base: WinRtVtbl,
    set_state_async: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> i32,
    _state_changed: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i64) -> i32,
    _remove_state_changed: unsafe extern "system" fn(*mut c_void, i64) -> i32,
    state: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    _name: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    kind: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
}

/// `IAsyncInfo` methods plus the first `IAsyncOperation<T>` method. The result
/// pointer is interpreted by the caller according to the operation's T.
#[repr(C)]
struct AsyncVtbl {
    base: WinRtVtbl,
    get_status: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    _get_error_code: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    _cancel: unsafe extern "system" fn(*mut c_void) -> i32,
    _close: unsafe extern "system" fn(*mut c_void) -> i32,
    _get_id: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    get_results_ptr: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct AsyncEnumVtbl {
    base: WinRtVtbl,
    get_status: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    _get_error_code: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    _cancel: unsafe extern "system" fn(*mut c_void) -> i32,
    _close: unsafe extern "system" fn(*mut c_void) -> i32,
    _get_id: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    get_results_enum: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
}

#[repr(C)]
struct VectorViewVtbl {
    base: WinRtVtbl,
    get_at: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
    get_size: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    _index_of: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut u32) -> i32,
    _get_many: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> i32,
}

/// -1 = unknown, 0 = off/disabled, 1 = on.
static WIFI_STATE: AtomicI8 = AtomicI8::new(-1);
static BLUETOOTH_STATE: AtomicI8 = AtomicI8::new(-1);
static MONITOR_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LAST_REFRESH: Mutex<Option<std::time::Instant>> = Mutex::new(None);

fn vtbl<T>(object: *mut c_void) -> Option<*const T> {
    if object.is_null() {
        return None;
    }
    // SAFETY: WinRT objects start with a pointer to their COM vtable.
    Some(unsafe { *(object as *const *const T) })
}

fn release(object: *mut c_void) {
    if object.is_null() {
        return;
    }
    // SAFETY: every released pointer was produced by a WinRT call that handed
    // us a reference.
    unsafe {
        let table = *(object as *const *const WinRtVtbl);
        ((*table).release)(object);
    }
}

fn query(object: *mut c_void, iid: &GUID) -> Option<*mut c_void> {
    if object.is_null() {
        return None;
    }
    let mut result = ptr::null_mut();
    // SAFETY: QueryInterface on a live WinRT object with an owned output slot.
    unsafe {
        let table = *(object as *const *const WinRtVtbl);
        (((*table)._query_interface)(object, iid, &mut result) == 0 && !result.is_null())
            .then_some(result)
    }
}

fn activate_statics() -> Option<*mut c_void> {
    let class = HSTRING::from("Windows.Devices.Radios.Radio");
    let raw: *mut c_void = unsafe { std::mem::transmute_copy(&class) };
    let mut factory = ptr::null_mut();
    let api = combase()?;
    // SAFETY: `raw` is a live HSTRING and `factory` is an owned output slot.
    let hr = unsafe {
        (api.get_activation_factory)(raw, &IID_RADIO_STATICS as *const GUID, &mut factory)
    };
    (hr == 0).then_some(factory)
}

fn wait_async_ptr(operation: *mut c_void) -> Option<*mut c_void> {
    let table = vtbl::<AsyncVtbl>(operation)?;
    for _ in 0..ASYNC_POLL_LIMIT {
        let mut status = 0i32;
        // SAFETY: live async operation, out parameter is owned.
        let hr = unsafe { ((*table).get_status)(operation, &mut status) };
        if hr != 0 {
            return None;
        }
        if status == ASYNC_STATUS_COMPLETED {
            let mut result = ptr::null_mut();
            // SAFETY: `get_results` signature matches an interface-result T.
            let ok = unsafe { ((*table).get_results_ptr)(operation, &mut result) };
            return (ok == 0).then_some(result);
        }
        if status != 0 {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

fn wait_async_enum(operation: *mut c_void) -> Option<i32> {
    let table = vtbl::<AsyncEnumVtbl>(operation)?;
    for _ in 0..ASYNC_POLL_LIMIT {
        let mut status = 0i32;
        // SAFETY: live async operation, out parameter is owned.
        let hr = unsafe { ((*table).get_status)(operation, &mut status) };
        if hr != 0 {
            return None;
        }
        if status == ASYNC_STATUS_COMPLETED {
            let mut result = 0i32;
            // SAFETY: `get_results` signature matches an enum-result T.
            let ok = unsafe { ((*table).get_results_enum)(operation, &mut result) };
            return (ok == 0).then_some(result);
        }
        if status != 0 {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// Runs a closure on a COM/MTA-initialized worker thread and joins it, so the
/// caller's apartment is never disturbed.
fn on_worker<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let handle = std::thread::Builder::new()
        .name("opentaskbar-radio".into())
        .spawn(move || {
            let api = combase();
            // SAFETY: this thread owns its WinRT apartment and uninitializes it
            // before exiting.
            if let Some(api) = api {
                unsafe { (api.initialize)(RO_INIT_MULTITHREADED) };
            }
            let result = work();
            // SAFETY: balances RoInitialize above.
            if let Some(api) = api {
                unsafe { (api.uninitialize)() };
            }
            result
        })
        .ok()?;
    handle.join().ok()
}

fn radio_kind(radio: *mut c_void) -> Option<i32> {
    let table = vtbl::<RadioVtbl>(radio)?;
    let mut kind = 0i32;
    // SAFETY: live IRadio; out parameter is owned.
    (unsafe { ((*table).kind)(radio, &mut kind) } == 0).then_some(kind)
}

fn radio_state(radio: *mut c_void) -> Option<i32> {
    let table = vtbl::<RadioVtbl>(radio)?;
    let mut state = 0i32;
    // SAFETY: live IRadio; out parameter is owned.
    (unsafe { ((*table).state)(radio, &mut state) } == 0).then_some(state)
}

fn collect_radios(statics: *mut c_void) -> Vec<*mut c_void> {
    let Some(table) = vtbl::<RadioStaticsVtbl>(statics) else {
        return Vec::new();
    };
    // SAFETY: live IRadioStatics; async result is owned by us.
    let mut async_op = ptr::null_mut();
    let started = unsafe { ((*table).get_radios_async)(statics, &mut async_op) } == 0;
    if !started || async_op.is_null() {
        return Vec::new();
    }
    let operation = async_op;
    let Some(view) = wait_async_ptr(operation) else {
        release(operation);
        return Vec::new();
    };
    release(operation);
    let Some(vector_table) = vtbl::<VectorViewVtbl>(view) else {
        release(view);
        return Vec::new();
    };
    let mut size = 0u32;
    // SAFETY: live IVectorView; out parameter is owned.
    let ok = unsafe { ((*vector_table).get_size)(view, &mut size) } == 0;
    let mut radios = Vec::new();
    if ok {
        for index in 0..size.min(32) {
            let mut radio = ptr::null_mut();
            // SAFETY: live IVectorView; returned reference is owned by us.
            if unsafe { ((*vector_table).get_at)(view, index, &mut radio) } == 0 && !radio.is_null()
            {
                radios.push(radio);
            }
        }
    }
    release(view);
    radios
}

fn request_access(statics: *mut c_void) -> bool {
    let Some(table) = vtbl::<RadioStaticsVtbl>(statics) else {
        return false;
    };
    // SAFETY: live IRadioStatics; async result is owned by us.
    let mut operation = ptr::null_mut();
    if unsafe { ((*table).request_access_async)(statics, &mut operation) } != 0
        || operation.is_null()
    {
        return false;
    }
    let status = wait_async_enum(operation);
    release(operation);
    status == Some(RADIO_ACCESS_STATUS_ALLOWED)
}

fn refresh_cache() -> bool {
    on_worker(|| {
        let Some(statics) = activate_statics() else {
            return false;
        };
        let Some(statics) = query(statics, &IID_RADIO_STATICS) else {
            release(statics);
            return false;
        };
        let allowed = request_access(statics);
        let mut wifi = -1i8;
        let mut bluetooth = -1i8;
        if allowed {
            for radio in collect_radios(statics) {
                if let Some(kind) = radio_kind(radio) {
                    let value = match radio_state(radio) {
                        Some(RADIO_STATE_ON) => 1i8,
                        Some(RADIO_STATE_OFF | RADIO_STATE_DISABLED) => 0,
                        _ => -1,
                    };
                    match kind {
                        RADIO_KIND_WIFI if value >= 0 => wifi = value,
                        RADIO_KIND_BLUETOOTH if value >= 0 => bluetooth = value,
                        _ => {}
                    }
                }
                release(radio);
            }
        }
        release(statics);
        WIFI_STATE.store(wifi, Ordering::Relaxed);
        BLUETOOTH_STATE.store(bluetooth, Ordering::Relaxed);
        allowed
    })
    .unwrap_or(false)
}

/// Normalizes a kind string from the Quick Settings widget.
pub fn normalize_kind(kind: &str) -> Option<i32> {
    let kind = kind.trim().to_ascii_lowercase();
    match kind.as_str() {
        "wifi" | "wi-fi" | "wireless" | "wlan" => Some(RADIO_KIND_WIFI),
        "bluetooth" | "bt" => Some(RADIO_KIND_BLUETOOTH),
        _ => None,
    }
}

/// True when the requested radio is on. Unknown/denied state is reported as
/// false so the widget never claims a radio is on without WinRT confirmation.
pub fn state(kind: &str) -> bool {
    let slot = match normalize_kind(kind) {
        Some(RADIO_KIND_WIFI) => &WIFI_STATE,
        Some(RADIO_KIND_BLUETOOTH) => &BLUETOOTH_STATE,
        _ => return false,
    };
    let cached = slot.load(Ordering::Relaxed);
    if cached < 0 {
        refresh_cache();
    }
    slot.load(Ordering::Relaxed) == 1
}

/// Requests Access + enumerates radios + sets the target state on every radio of
/// the requested kind, then refreshes the cached state.
pub fn set(kind: &str, on: bool) -> bool {
    let Some(kind_value) = normalize_kind(kind) else {
        return false;
    };
    let target = if on { RADIO_STATE_ON } else { RADIO_STATE_OFF };
    let changed = on_worker(move || {
        let Some(factory) = activate_statics() else {
            return false;
        };
        let Some(statics) = query(factory, &IID_RADIO_STATICS) else {
            release(factory);
            return false;
        };
        release(factory);
        if !request_access(statics) {
            release(statics);
            return false;
        }
        let mut any = false;
        for radio in collect_radios(statics) {
            if radio_kind(radio) != Some(kind_value) {
                release(radio);
                continue;
            }
            if !matches!(radio_state(radio), Some(RADIO_STATE_ON | RADIO_STATE_OFF)) {
                release(radio);
                continue;
            }
            let Some(table) = vtbl::<RadioVtbl>(radio) else {
                release(radio);
                continue;
            };
            // SAFETY: live IRadio; the async operation is owned by us.
            let mut operation = ptr::null_mut();
            let started = unsafe { ((*table).set_state_async)(radio, target, &mut operation) } == 0
                && !operation.is_null();
            if started {
                let status = wait_async_enum(operation);
                release(operation);
                any |= status == Some(RADIO_ACCESS_STATUS_ALLOWED);
            }
            release(radio);
        }
        release(statics);
        any
    })
    .unwrap_or(false);
    refresh_cache();
    changed
}

/// Starts a background state-refresh loop. C# uses WinRT `StateChanged`;
/// polling at 2 s keeps the same cache freshness without needing a typed
/// delegate (which requires the lead-owned Cargo feature).
pub fn install_state_monitor() {
    if MONITOR_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("opentaskbar-radio-watch".into())
        .spawn(|| {
            loop {
                refresh_cache();
                if let Ok(mut last) = LAST_REFRESH.lock() {
                    *last = Some(std::time::Instant::now());
                }
                std::thread::sleep(REFRESH_INTERVAL);
            }
        });
    log::info("Radio state monitor started.");
}

/// Last successful refresh timestamp (telemetry/diagnostics).
pub fn last_refresh() -> Option<std::time::Instant> {
    *LAST_REFRESH.lock().ok()?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_kind_strings_are_normalized() {
        assert_eq!(normalize_kind(" Wi-Fi "), Some(RADIO_KIND_WIFI));
        assert_eq!(normalize_kind("wlan"), Some(RADIO_KIND_WIFI));
        assert_eq!(normalize_kind("BT"), Some(RADIO_KIND_BLUETOOTH));
        assert_eq!(normalize_kind("nightlight"), None);
    }
}
