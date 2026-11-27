//! WinRT `Windows.Devices.Radios.Radio` bridge for the Quick Settings widget.
//!
//! The `Devices_Radios` Cargo feature is enabled in the workspace root, but the
//! small set of WinRT interfaces we need is still activated directly through
//! their COM vtables (`RoGetActivationFactory` + `RequestAccessAsync` +
//! `GetRadiosAsync` + `SetStateAsync`) to avoid extra SDK dependencies. A
//! background refresh thread keeps the cached Wi-Fi/Bluetooth state current.
//!
//! C4 API: `state(kind: &str) -> bool`, `set(kind: &str, on: bool) -> bool`.
//! `set` reports whether the request was *accepted*; the eventual WinRT result
//! is published through `take_last_set_failure()`, and the cache is rolled back
//! on failure so callers never keep showing an optimistic, false "on".

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicI8, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use windows::core::{GUID, HSTRING};

use opentaskbar_core::log;

const RADIO_ACCESS_STATUS_ALLOWED: i32 = 1;
// Windows.Foundation.AsyncStatus: Started=0, Completed=1, Canceled=2, Error=3.
const ASYNC_STATUS_STARTED: i32 = 0;
const ASYNC_STATUS_COMPLETED: i32 = 1;
// Windows.Devices.Radios.RadioState: Unknown=0, On=1, Off=2, Disabled=3.
const RADIO_STATE_ON: i32 = 1;
const RADIO_STATE_OFF: i32 = 2;
const RADIO_STATE_DISABLED: i32 = 3;
// Windows.Devices.Radios.RadioKind: Other=0, WiFi=1, MobileBroadband=2, Bluetooth=3, FM=4.
const RADIO_KIND_WIFI: i32 = 1;
const RADIO_KIND_BLUETOOTH: i32 = 3;
const RO_INIT_MULTITHREADED: i32 = 1;
// Bound the whole SetStateAsync/GetRadiosAsync/RequestAccessAsync chain to
// ~2 s (3 * 600 ms) so a UI toggle never needs the Settings fallback.
const ASYNC_POLL_LIMIT: usize = 300; // 600 ms at 2 ms
const ASYNC_POLL_INTERVAL: Duration = Duration::from_millis(2);
const REFRESH_INTERVAL: Duration = Duration::from_secs(4);

// IIDs from Windows.Foundation and Windows.Devices.Radios metadata.
const IID_IASYNC_INFO: GUID = GUID::from_u128(0x0000_0036_0000_0000_c000_0000_0000_0046);
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

/// `Windows.Foundation.IAsyncInfo` (`{00000036-0000-0000-C000-000000000046}`),
/// queried separately from `IAsyncOperation<T>` because WinRT interfaces do not
/// flatten required interfaces into a single vtable. `IAsyncInfo` derives from
/// `IInspectable`, so its five methods follow the six IUnknown/IInspectable slots.
/// Omitting those five slots was the reason status always read as `Started(0)`.
#[repr(C)]
struct AsyncInfoVtbl {
    base: WinRtVtbl,
    _get_id: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    get_status: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    get_error_code: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    _cancel: unsafe extern "system" fn(*mut c_void) -> i32,
    close: unsafe extern "system" fn(*mut c_void) -> i32,
}

/// `Windows.Foundation.IAsyncOperation<T>`: IAsyncInfo base + three own methods.
#[repr(C)]
struct AsyncOpPtrVtbl {
    base: AsyncInfoVtbl,
    _put_completed: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    _get_completed: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    get_results: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct AsyncOpEnumVtbl {
    base: AsyncInfoVtbl,
    _put_completed: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    _get_completed: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    get_results: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
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
/// Coalesces fire-and-forget refresh requests so a UI click cannot pile up COM workers.
static REFRESH_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Result of the most recent asynchronous `set()` request, consumed by callers
/// that want to show a real error instead of the optimistic chip state.
static LAST_SET_FAILURE: Mutex<Option<&'static str>> = Mutex::new(None);
/// Cancellation flag for the background monitor loop.
static MONITOR_STOP: Mutex<bool> = Mutex::new(false);
static MONITOR_WAKE: Condvar = Condvar::new();

#[cfg(test)]
static REFRESH_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_SUPPRESS_WRITES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static TEST_SUPPRESS_REFRESH: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static TEST_REFRESH_PANIC: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn record_set_failure(error: &'static str) {
    if let Ok(mut last) = LAST_SET_FAILURE.lock() {
        *last = Some(error);
    }
}

/// Error key for the last completed asynchronous `set()` request, if it failed.
/// The failure stays visible until `take_last_set_failure()` consumes it.
pub fn last_set_failure() -> Option<&'static str> {
    *LAST_SET_FAILURE.lock().ok()?
}

/// Consumes the last asynchronous set failure (error key suitable for i18n).
pub fn take_last_set_failure() -> Option<&'static str> {
    LAST_SET_FAILURE.lock().ok()?.take()
}

/// Clears the in-flight flag even if the refresh worker panics or cannot spawn.
struct RefreshInFlightGuard;

impl Drop for RefreshInFlightGuard {
    fn drop(&mut self) {
        REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    }
}

/// Waits one refresh interval; returns `true` when the monitor was asked to stop.
fn monitor_wait_interval() -> bool {
    let Ok(stop) = MONITOR_STOP.lock() else {
        return false;
    };
    let Ok((stop, _)) = MONITOR_WAKE.wait_timeout(stop, REFRESH_INTERVAL) else {
        return false;
    };
    *stop
}

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

fn wait_async_completed(operation: *mut c_void) -> bool {
    let Some(info) = query(operation, &IID_IASYNC_INFO) else {
        return false;
    };
    let Some(table) = vtbl::<AsyncInfoVtbl>(info) else {
        release(info);
        return false;
    };
    let mut done = false;
    for _ in 0..ASYNC_POLL_LIMIT {
        let mut status = -1i32;
        // SAFETY: `info` is a live `IAsyncInfo` interface pointer obtained via QueryInterface.
        let hr = unsafe { ((*table).get_status)(info, &mut status) };
        if hr != 0 {
            break;
        }
        if status == ASYNC_STATUS_COMPLETED {
            done = true;
            break;
        }
        if status != ASYNC_STATUS_STARTED {
            break;
        }
        std::thread::sleep(ASYNC_POLL_INTERVAL);
    }
    release(info);
    done
}

fn close_async(operation: *mut c_void) {
    if let Some(info) = query(operation, &IID_IASYNC_INFO) {
        if let Some(table) = vtbl::<AsyncInfoVtbl>(info) {
            unsafe {
                let _ = ((*table).close)(info);
            }
        }
        release(info);
    }
}

fn wait_async_ptr(operation: *mut c_void) -> Option<*mut c_void> {
    if !wait_async_completed(operation) {
        return None;
    }
    let table = vtbl::<AsyncOpPtrVtbl>(operation)?;
    let mut result = ptr::null_mut();
    // SAFETY: `operation` is a completed `IAsyncOperation<T>` returning an interface pointer.
    let ok = unsafe { ((*table).get_results)(operation, &mut result) };
    close_async(operation);
    (ok == 0 && !result.is_null()).then_some(result)
}

fn wait_async_enum(operation: *mut c_void) -> Option<i32> {
    if !wait_async_completed(operation) {
        return None;
    }
    let table = vtbl::<AsyncOpEnumVtbl>(operation)?;
    let mut result = 0i32;
    // SAFETY: `operation` is a completed `IAsyncOperation<T>` returning an i32 enum.
    let ok = unsafe { ((*table).get_results)(operation, &mut result) };
    close_async(operation);
    (ok == 0).then_some(result)
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

fn refresh_cache_in_apartment() -> bool {
    #[cfg(test)]
    REFRESH_CALLS.fetch_add(1, Ordering::Relaxed);
    {
        let Some(statics) = activate_statics() else {
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
    }
}

/// Single-shot refresh for callers that are not already on a COM apartment.
fn refresh_cache() -> bool {
    on_worker(refresh_cache_in_apartment).unwrap_or(false)
}

/// Fire-and-forget cache refresh. Spawns a COM/MTA worker but never joins it,
/// so a UI-thread caller cannot be blocked by the (up to ~4 s) async polling.
/// Redundant requests are coalesced while one refresh is in flight.
pub fn refresh_async() {
    if REFRESH_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    // The guard owns the flag: a panic inside the worker (or a failed spawn)
    // can no longer leave refreshes coalesced out forever.
    let guard = RefreshInFlightGuard;
    // On a failed spawn the builder drops the closure (and its guard), which
    // clears the flag without a second, explicit store here.
    let _spawned = std::thread::Builder::new()
        .name("opentaskbar-radio-refresh".into())
        .spawn(move || {
            #[cfg(test)]
            if TEST_REFRESH_PANIC.swap(false, Ordering::Relaxed) {
                panic!("test: refresh worker panic");
            }
            refresh_cache();
            drop(guard);
        });
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

/// True when the requested radio is on, read purely from the cache.
///
/// WS-1: this is called from the dock UI thread, which also owns the
/// WH_MOUSE_LL / WH_KEYBOARD_LL hooks. It must NEVER do `thread::spawn + join`
/// or COM polling here. A cold/unknown cache (`-1`) reads as `false`; the
/// 4-second `install_state_monitor()` thread performs the real refresh.
pub fn state(kind: &str) -> bool {
    let slot = match normalize_kind(kind) {
        Some(RADIO_KIND_WIFI) => &WIFI_STATE,
        Some(RADIO_KIND_BLUETOOTH) => &BLUETOOTH_STATE,
        _ => return false,
    };
    slot.load(Ordering::Relaxed) == 1
}

/// Real WinRT write in the calling COM/MTA apartment. `SetStateAsync` returns
/// `IAsyncOperation<RadioAccessStatus>`: `wait_async_completed` polls
/// `IAsyncInfo::Status` and `wait_async_enum` reads the enum via `GetResults`.
/// There is no void `IAsyncAction` on this path.
fn set_in_apartment(kind_value: i32, target: i32) -> bool {
    #[cfg(test)]
    if TEST_SUPPRESS_WRITES.load(Ordering::Relaxed) {
        return false;
    }
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
        // SAFETY: live IRadio; the async operation is owned by us. The result is
        // an enum (Allowed = 1), read only after the IAsyncInfo reports Completed.
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
}

fn cache_set(kind_value: i32, value: i8) {
    match kind_value {
        RADIO_KIND_WIFI => WIFI_STATE.store(value, Ordering::Relaxed),
        RADIO_KIND_BLUETOOTH => BLUETOOTH_STATE.store(value, Ordering::Relaxed),
        _ => {}
    }
}

fn cache_toggle(kind_value: i32, on: bool) {
    cache_set(kind_value, if on { 1 } else { 0 });
}

/// Blocking variant for background callers and tests: performs the real WinRT
/// toggle on a COM worker and joins it. Never call from the UI/hook thread.
pub fn set_blocking(kind: &str, on: bool) -> bool {
    let Some(kind_value) = normalize_kind(kind) else {
        return false;
    };
    let target = if on { RADIO_STATE_ON } else { RADIO_STATE_OFF };
    let changed = on_worker(move || set_in_apartment(kind_value, target)).unwrap_or(false);
    if changed {
        cache_toggle(kind_value, on);
        refresh_async();
    }
    changed
}

/// Tests suppress network refresh while simulating failed writes; production
/// always refreshes after a completed toggle attempt.
fn should_refresh_after_set() -> bool {
    #[cfg(test)]
    {
        !TEST_SUPPRESS_REFRESH.load(Ordering::Relaxed)
    }
    #[cfg(not(test))]
    {
        true
    }
}

/// UI-thread API. Never spawns+joins here: the WinRT write and the state refresh
/// both run on detached workers. The cache flips optimistically for a responsive
/// chip, but a failed write rolls it back to "unknown" and publishes an error
/// key via `take_last_set_failure()`. Returns `false` for an unknown kind or if
/// the worker could not be started.
pub fn set(kind: &str, on: bool) -> bool {
    let Some(kind_value) = normalize_kind(kind) else {
        record_set_failure("RadioUnknownKind");
        return false;
    };
    if let Ok(mut last) = LAST_SET_FAILURE.lock() {
        *last = None;
    }
    let target = if on { RADIO_STATE_ON } else { RADIO_STATE_OFF };
    // Update before spawning so the worker's failure rollback cannot race with
    // an optimistic write from this thread.
    cache_toggle(kind_value, on);
    let spawned = std::thread::Builder::new()
        .name("opentaskbar-radio-set".into())
        .spawn(move || {
            if set_in_apartment(kind_value, target) {
                if should_refresh_after_set() {
                    refresh_async();
                }
            } else {
                cache_set(kind_value, -1);
                record_set_failure("RadioWriteFailed");
                if should_refresh_after_set() {
                    refresh_async();
                }
            }
        });
    if spawned.is_err() {
        cache_set(kind_value, -1);
        record_set_failure("RadioWorkerFailed");
        return false;
    }
    true
}

/// Starts a background state-refresh loop. Polling at 4 s keeps the cache
/// fresh without a typed `StateChanged` delegate. The apartment is initialized
/// once; `stop_state_monitor()` cancels the loop without joining it.
pub fn install_state_monitor() {
    if MONITOR_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    reset_monitor_stop();
    let spawned = std::thread::Builder::new()
        .name("opentaskbar-radio-watch".into())
        .spawn(|| {
            let api = combase();
            // SAFETY: this thread owns its WinRT apartment for its whole lifetime.
            if let Some(api) = api {
                unsafe { (api.initialize)(RO_INIT_MULTITHREADED) };
            }
            loop {
                refresh_cache_in_apartment();
                if let Ok(mut last) = LAST_REFRESH.lock() {
                    *last = Some(std::time::Instant::now());
                }
                if monitor_wait_interval() {
                    break;
                }
            }
            // SAFETY: balances the RoInitialize above before the thread exits.
            if let Some(api) = api {
                unsafe { (api.uninitialize)() };
            }
            MONITOR_STARTED.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        MONITOR_STARTED.store(false, Ordering::SeqCst);
        return;
    }
    log::info("Radio state monitor started.");
}

fn reset_monitor_stop() {
    if let Ok(mut stop) = MONITOR_STOP.lock() {
        *stop = false;
    }
}

/// Asks the background monitor to exit after its current refresh. Safe to call
/// more than once; a later `install_state_monitor()` restarts the loop.
pub fn stop_state_monitor() {
    if let Ok(mut stop) = MONITOR_STOP.lock() {
        *stop = true;
    }
    MONITOR_WAKE.notify_all();
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
        assert_eq!(RADIO_STATE_ON, 1);
        assert_eq!(RADIO_STATE_OFF, 2);
        assert_eq!(RADIO_KIND_BLUETOOTH, 3);
        assert_eq!(ASYNC_STATUS_COMPLETED, 1);
    }

    #[test]
    fn winrt_radio_refresh_completes_without_crashing() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Exercises the real combase.dll WinRT activation, QueryInterface(IAsyncInfo),
        // and IAsyncOperation::GetResults path that previously faulted with 0xC0000005.
        let _ = refresh_cache();
        let _ = state("wifi");
        let _ = state("bluetooth");
    }

    fn wait_for_last_set_failure() -> Option<&'static str> {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(error) = take_last_set_failure() {
                return Some(error);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn set_is_non_blocking_and_rolls_back_optimistic_cache_on_failure() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Tests must never flip the developer's actual Wi-Fi adapter.
        TEST_SUPPRESS_WRITES.store(true, Ordering::Relaxed);
        TEST_SUPPRESS_REFRESH.store(true, Ordering::Relaxed);
        assert!(!set("nightlight", true), "unknown kinds must be rejected");
        assert_eq!(take_last_set_failure(), Some("RadioUnknownKind"));

        WIFI_STATE.store(-1, Ordering::Relaxed);
        assert!(set("wifi", true), "a valid toggle must not report failure");
        assert_eq!(WIFI_STATE.load(Ordering::Relaxed), 1, "chip flips at once");
        assert_eq!(
            wait_for_last_set_failure(),
            Some("RadioWriteFailed"),
            "a denied WinRT write must reach the caller"
        );
        assert_eq!(
            WIFI_STATE.load(Ordering::Relaxed),
            -1,
            "a failed write must not leave a false 'on' in the cache"
        );

        assert!(set("wi-fi", false));
        assert_eq!(wait_for_last_set_failure(), Some("RadioWriteFailed"));
        WIFI_STATE.store(-1, Ordering::Relaxed);
        assert!(!set_blocking("nightlight", true));
        TEST_SUPPRESS_WRITES.store(false, Ordering::Relaxed);
        TEST_SUPPRESS_REFRESH.store(false, Ordering::Relaxed);
    }

    #[test]
    fn refresh_flag_is_cleared_when_the_worker_panics() {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while REFRESH_IN_FLIGHT.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!REFRESH_IN_FLIGHT.load(Ordering::Acquire));
        TEST_REFRESH_PANIC.store(true, Ordering::Relaxed);
        refresh_async();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while REFRESH_IN_FLIGHT.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !REFRESH_IN_FLIGHT.load(Ordering::Acquire),
            "a panicking refresh worker must not block all future refreshes"
        );
    }

    #[test]
    fn monitor_stop_flag_can_be_raised_and_reset() {
        stop_state_monitor();
        assert!(*MONITOR_STOP.lock().unwrap());
        reset_monitor_stop();
        assert!(!*MONITOR_STOP.lock().unwrap());
    }

    #[test]
    fn state_reads_cache_only_without_refresh_or_join() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = REFRESH_CALLS.load(Ordering::Relaxed);

        // Cached values are returned without touching COM or spawning a worker.
        WIFI_STATE.store(1, Ordering::Relaxed);
        BLUETOOTH_STATE.store(0, Ordering::Relaxed);
        assert!(state("wifi"));
        assert!(!state("bluetooth"));
        assert!(!state("nightlight"));

        // A cold (-1) cache is reported as false; the monitor thread refreshes it.
        WIFI_STATE.store(-1, Ordering::Relaxed);
        BLUETOOTH_STATE.store(-1, Ordering::Relaxed);
        assert!(!state("wifi"));
        assert!(!state("bluetooth"));

        assert_eq!(
            REFRESH_CALLS.load(Ordering::Relaxed),
            before,
            "state() must never call refresh_cache() (spawn + join) on the UI path"
        );
    }
}
