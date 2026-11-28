//! Reading and invoking Windows Jump Lists.
//!
//! Standalone Rust implementation of the `ApplicationJumpListService` and
//! `NativeJumpListSession`. COM entry points, vtable dispatch and owned data
//! live in this file; only the apartment guard is shared with other platform
//! modules via [`super::com`].
//!
//! # Design notes
//!
//! * The documented `ICustomDestinationList` (IID `6332debf-87b5-4670-90c0-5e57b408a49e`,
//!   CLSID `77f10cf0-3db5-4966-b520-b7c54fd35ed6`) can only *write* jump lists.
//!   Windows 0.61 does not expose `GetAppIDList`/`GetTasks`, so reading uses the
//!   private destination interface (`507101cd-f6ad-46c8-8e20-eeb9e6bac47f`) and
//!   the automatic destination list used by the reference implementation. Methods are
//!   called directly through their vtable slots, exactly like
//!   `ShellComObject.Method<T>(slot)` does in reference.
//! * COM objects never leave this module. A session stores plain, owned launch
//!   descriptors (`target`, `arguments`, `working directory`), so
//!   `invoke_command` is safe on any thread and does not depend on the STA
//!   apartment that created the list.
//! * `invoke_command` uses `ShellExecuteExW` with the target application window
//!   as activation-context owner. This is the documented approximation of the
//!   `IContextMenu::InvokeCommand` path (which additionally handles
//!   activation contexts of packaged apps).
//!
//! Callers should invoke these functions from a worker thread: each call takes
//! a shared `ComApartment::sta()` guard, which balances its own initialization
//! and tolerates a thread that is already initialized (`RPC_E_CHANGED_MODE`).

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use opentaskbar_core::i18n as loc;
use windows::core::GUID;

// ---------------------------------------------------------------------------
// Raw Win32 entry points (kept local so this file is self-contained).
// ---------------------------------------------------------------------------

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoCreateInstance(
        class_id: *const GUID,
        outer: *mut c_void,
        context: u32,
        interface_id: *const GUID,
        result: *mut *mut c_void,
    ) -> i32;
    fn CoTaskMemFree(memory: *mut c_void);
    fn PropVariantClear(value: *mut PropVariant) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHGetPropertyStoreForWindow(
        hwnd: *mut c_void,
        interface_id: *const GUID,
        result: *mut *mut c_void,
    ) -> i32;
    fn SHCreateItemFromParsingName(
        path: *const u16,
        bind_context: *mut c_void,
        interface_id: *const GUID,
        result: *mut *mut c_void,
    ) -> i32;
    fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
}

#[link(name = "shlwapi")]
unsafe extern "system" {
    fn SHLoadIndirectString(
        source: *const u16,
        output: *mut u16,
        capacity: u32,
        reserved: *mut c_void,
    ) -> i32;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const CLSCTX_INPROC_SERVER: u32 = 0x1;

const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
const E_FAIL: i32 = 0x8000_4005u32 as i32;
const E_UNEXPECTED: i32 = 0x8000_ffffu32 as i32;
const REGDB_E_CLASSNOTREG: i32 = 0x8004_0154u32 as i32;
const STG_E_FILENOTFOUND: i32 = 0x8003_0002u32 as i32;
const HRESULT_FROM_WIN32_FILE_NOT_FOUND: i32 = 0x8007_0002u32 as i32;

const VT_BSTR: u16 = 8;
const VT_BOOL: u16 = 11;
const VT_LPWSTR: u16 = 31;
const SLGP_RAWPATH: u32 = 4;
const SIGDN_NORMALDISPLAY: u32 = 0;
const SEE_MASK_FLAG_NO_UI: u32 = 0x0000_0400;
const SW_SHOWNORMAL: i32 = 1;

const MAX_CATEGORIES: u32 = 32;
const MAX_GROUP_ENTRIES: usize = 40;
const MAX_COMMANDS: i32 = 100;
const MAX_SESSIONS: usize = 8;

// Class / interface IDs, verified against windows 0.61 metadata and the reference
// implementation. The documentation-only CLSID is
// 77f10cf0-3db5-4966-b520-b7c54fd35ed6; the private interface below is the
// read side used by Explorer.
const CLSID_DESTINATION: GUID = GUID::from_u128(0x77f1_0cf0_3db5_4966_b520_b7c5_4fd3_5ed6);
const IID_DESTINATION_READ: GUID = GUID::from_u128(0x5071_01cd_f6ad_46c8_8e20_eeb9_e6ba_c47f);
const CLSID_AUTOMATIC: GUID = GUID::from_u128(0xf0ae_1542_f497_484b_a175_a20d_b091_44ba);
const IID_AUTOMATIC_LIST: GUID = GUID::from_u128(0xe9c5_ef8d_fd41_4f72_ba87_eb03_bad5_817c);
const CLSID_APP_RESOLVER: GUID = GUID::from_u128(0x660b_90c8_73a9_4b58_8cae_355b_7f55_341b);
const IID_APP_RESOLVER: GUID = GUID::from_u128(0xde25_675a_72de_44b4_9373_0517_0450_c140);
const IID_IUNKNOWN: GUID = GUID::from_u128(0x0000_0000_0000_0000_c000_0000_0000_0046);
const IID_ISHELLITEM: GUID = GUID::from_u128(0x4382_6d1e_e718_42ee_bc55_a1e2_61c3_7bfe);
const IID_IPROPERTYSTORE: GUID = GUID::from_u128(0x886d_8eeb_8cf2_4446_8d02_cdba_1dbd_cf99);
const IID_ISHELLLINKW: GUID = GUID::from_u128(0x0002_14f9_0000_0000_c000_0000_0000_0046);

// PKEY_AppUserModel_ID, pid 5 / separator pid 6 / host environment pid 14 /
// activation context pid 20.
const FMTID_APP_USER_MODEL: GUID = GUID::from_u128(0x9f4c_2855_9f79_4b39_a8d0_e1d4_2de1_d5f3);
const KEY_APP_ID: PropertyKey = PropertyKey::new(FMTID_APP_USER_MODEL, 5);
const KEY_SEPARATOR: PropertyKey = PropertyKey::new(FMTID_APP_USER_MODEL, 6);
// PKEY_Title.
const KEY_TITLE: PropertyKey = PropertyKey::new(
    GUID::from_u128(0xf29f_85e0_4ff9_1068_ab91_0800_2b27_b3d9),
    2,
);
// PKEY_Link_Arguments — Firefox/LibreWolf tasks carry `-private-window` here
// instead of in IShellLinkW::GetArguments.
const KEY_LINK_ARGUMENTS: PropertyKey = PropertyKey::new(
    GUID::from_u128(0x436f_2667_14e2_4feb_b30a_146c_53b5_b674),
    100,
);

// ---------------------------------------------------------------------------
// Public data model
// ---------------------------------------------------------------------------

/// Outcome of a jump list read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JumpListStatus {
    /// At least one group with entries was read.
    Ready,
    /// The list exists but contains no displayable entries.
    Empty,
    /// No application identity (AppUserModelID) could be resolved.
    IdentityUnavailable,
    /// The shell jump list interfaces are not present on this system.
    Unsupported,
    /// The shell rejected the read with another failure.
    Unavailable,
}

/// One command of a jump list. `command_id` is only valid for the session id
/// returned by [`read_jump_list`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JumpListEntry {
    pub command_id: i32,
    pub title: String,
    /// Reserved for BGRA32 pixels. Icon extraction is deliberately not done
    /// here; the icon location is retained internally for launch fallback.
    pub icon: Option<Vec<u8>>,
    pub is_separator: bool,
}

/// A named category (Tasks, Recent, Frequent, Pinned, or a custom category).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JumpListGroup {
    pub title: String,
    pub entries: Vec<JumpListEntry>,
}

/// An owned snapshot of an application's jump list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationJumpList {
    pub session_id: i64,
    pub status: JumpListStatus,
    pub groups: Vec<JumpListGroup>,
}

/// Reads the jump list for `window_hwnd` (preferred identity/activation source)
/// or, when no window is available, for `shortcut_path`.
///
/// Returns `None` only when neither an owner window nor a shortcut was given;
/// all shell failures are reported through [`JumpListStatus`] instead.
pub fn read_jump_list(
    window_hwnd: usize,
    shortcut_path: Option<&str>,
) -> Option<ApplicationJumpList> {
    if window_hwnd == 0 && shortcut_path.is_none_or(str::is_empty) {
        return None;
    }

    let session_id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let mut commands = HashMap::new();
    let mut groups = Vec::new();
    let mut activation_hwnd = 0usize;

    // Declared first so it is dropped last: every COM object created below is
    // released before CoUninitialize.
    let Some(_sta) = super::com::ComApartment::sta() else {
        return Some(finish(
            session_id,
            JumpListStatus::Unavailable,
            groups,
            commands,
            activation_hwnd,
        ));
    };

    let status = match read_inner(
        window_hwnd,
        shortcut_path,
        &mut groups,
        &mut commands,
        &mut activation_hwnd,
    ) {
        Ok(status) => status,
        Err(hr) if hr == E_NOINTERFACE || hr == REGDB_E_CLASSNOTREG => JumpListStatus::Unsupported,
        Err(_) => JumpListStatus::Unavailable,
    };

    Some(finish(
        session_id,
        status,
        groups,
        commands,
        activation_hwnd,
    ))
}

/// Invokes a command previously returned by [`read_jump_list`].
///
/// Uses the target application's window (captured at read time, falling back to
/// `owner_hwnd`) as the ShellExecute activation-context owner. Returns `false`
/// for unknown/expired sessions and for launch failures.
pub fn invoke_command(session_id: i64, command_id: i32, owner_hwnd: usize) -> bool {
    let (launch, activation_hwnd) = {
        let sessions = lock_sessions();
        let Some(session) = sessions.get(&session_id) else {
            return false;
        };
        let Some(launch) = session.commands.get(&command_id) else {
            return false;
        };
        (launch.clone(), session.activation_hwnd)
    };
    if launch.target.trim().is_empty() {
        return false;
    }

    // ShellExecuteExW does not require COM, but a valid apartment makes shell
    // extensions and activation contexts behave consistently.
    let _sta = super::com::ComApartment::sta();

    let target = wide(&launch.target);
    let arguments = if launch.args.is_empty() {
        Vec::new()
    } else {
        wide(&launch.args)
    };
    let directory = if launch.working_dir.is_empty() {
        Vec::new()
    } else {
        wide(&launch.working_dir)
    };
    let hwnd = if activation_hwnd != 0 {
        activation_hwnd
    } else {
        owner_hwnd
    };

    // SAFETY: all buffers outlive the synchronous ShellExecuteExW call and the
    // structure's field order matches SHELLEXECUTEINFOW on Windows.
    unsafe {
        let mut info: ShellExecuteInfoW = zeroed();
        info.cb_size = size_of::<ShellExecuteInfoW>() as u32;
        info.f_mask = SEE_MASK_FLAG_NO_UI;
        info.hwnd = hwnd as *mut c_void;
        info.lp_file = target.as_ptr();
        info.lp_parameters = if arguments.is_empty() {
            null()
        } else {
            arguments.as_ptr()
        };
        info.lp_directory = if directory.is_empty() {
            null()
        } else {
            directory.as_ptr()
        };
        info.n_show = SW_SHOWNORMAL;
        ShellExecuteExW(&mut info) != 0
    }
}

/// Releases the launch descriptors associated with `session_id`.
pub fn release_session(session_id: i64) {
    lock_sessions().remove(&session_id);
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Launch {
    target: String,
    args: String,
    working_dir: String,
    /// Icon resource retained for diagnostics/future lazy extraction.
    #[allow(dead_code)]
    icon: String,
}

struct Session {
    commands: HashMap<i32, Launch>,
    activation_hwnd: usize,
}

static SESSIONS: std::sync::OnceLock<Mutex<HashMap<i64, Session>>> = std::sync::OnceLock::new();
static NEXT_SESSION: AtomicI64 = AtomicI64::new(1);

fn lock_sessions() -> std::sync::MutexGuard<'static, HashMap<i64, Session>> {
    SESSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn register_session(id: i64, session: Session) {
    let mut sessions = lock_sessions();
    // Keep a small window of valid sessions; older ones cannot be invoked any
    // more but would otherwise accumulate indefinitely.
    while sessions.len() >= MAX_SESSIONS
        && let Some(oldest) = sessions.keys().copied().min()
    {
        sessions.remove(&oldest);
    }
    sessions.insert(id, session);
}

fn finish(
    session_id: i64,
    status: JumpListStatus,
    groups: Vec<JumpListGroup>,
    commands: HashMap<i32, Launch>,
    activation_hwnd: usize,
) -> ApplicationJumpList {
    register_session(
        session_id,
        Session {
            commands,
            activation_hwnd,
        },
    );
    ApplicationJumpList {
        session_id,
        status,
        groups,
    }
}

// ---------------------------------------------------------------------------
// Minimal COM plumbing (no external helper types)
// ---------------------------------------------------------------------------

#[repr(transparent)]
struct ComPtr(*mut c_void);

impl ComPtr {
    /// SAFETY: `class_id`/`interface_id` must describe a real COM class.
    unsafe fn create(class_id: &GUID, interface_id: &GUID) -> Result<Self, i32> {
        let mut raw = null_mut();
        // SAFETY: valid IDs and output pointer; COM is initialized by caller.
        let hr = unsafe {
            CoCreateInstance(
                class_id as *const GUID,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                interface_id as *const GUID,
                &mut raw,
            )
        };
        if hr < 0 {
            return Err(hr);
        }
        if raw.is_null() {
            return Err(E_FAIL);
        }
        Ok(Self(raw))
    }

    /// SAFETY: `raw` must be an add-ref'd pointer to a COM object.
    unsafe fn from_raw(raw: *mut c_void) -> Self {
        Self(raw)
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0
    }

    fn query(&self, interface_id: &GUID) -> Option<Self> {
        if self.0.is_null() {
            return None;
        }
        let query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32 =
            unsafe { com_method(self.0, 0) };
        let mut raw = null_mut();
        // SAFETY: `self.0` is a live COM object for the duration of this call.
        let hr = unsafe { query(self.0, interface_id as *const GUID, &mut raw) };
        if hr >= 0 && !raw.is_null() {
            Some(Self(raw))
        } else {
            if !raw.is_null() {
                release(raw);
            }
            None
        }
    }
}

impl Drop for ComPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            release(self.0);
            self.0 = null_mut();
        }
    }
}

fn release(ptr: *mut c_void) {
    let release_fn: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { com_method(ptr, 2) };
    // SAFETY: `ptr` is a live COM object; Release is slot 2 on all interfaces.
    let _ = unsafe { release_fn(ptr) };
}

/// Reads a function pointer from a COM vtable slot. Shell vtables are stable
/// ABI tables; the function type must match the slot exactly.
unsafe fn com_method<F: Copy>(ptr: *mut c_void, slot: usize) -> F {
    unsafe {
        let vtable = *(ptr as *const *const *const c_void);
        std::ptr::read(vtable.add(slot).cast::<F>())
    }
}

fn hr_ok(hr: i32) -> Result<(), i32> {
    if hr < 0 { Err(hr) } else { Ok(()) }
}

fn missing_list(hr: i32) -> bool {
    hr == STG_E_FILENOTFOUND || hr == HRESULT_FROM_WIN32_FILE_NOT_FOUND
}

// ---------------------------------------------------------------------------
// Native structures
// ---------------------------------------------------------------------------

#[repr(C)]
struct PropertyKey {
    fmtid: GUID,
    pid: u32,
}

impl PropertyKey {
    const fn new(fmtid: GUID, pid: u32) -> Self {
        Self { fmtid, pid }
    }
}

/// `PROPVARIANT`: 16-byte header + 16-byte union (24 bytes on x64).
#[repr(C)]
#[allow(dead_code)]
struct PropVariant {
    vt: u16,
    reserved1: u16,
    reserved2: u16,
    reserved3: u16,
    data: [u64; 2],
}

/// `KNOTIFICATION` layout used by the private destination list's GetCategory.
#[repr(C)]
#[allow(dead_code)]
struct Category {
    category_type: i32,
    padding: i32,
    name_or_kind: usize,
    reserved: [u64; 6],
}

/// `SHELLEXECUTEINFOW` (112 bytes on x64). Reserved fields must be present
/// because Windows reads them by offset.
#[repr(C)]
#[allow(dead_code)]
struct ShellExecuteInfoW {
    cb_size: u32,
    f_mask: u32,
    hwnd: *mut c_void,
    lp_verb: *const u16,
    lp_file: *const u16,
    lp_parameters: *const u16,
    lp_directory: *const u16,
    n_show: i32,
    h_inst_app: *mut c_void,
    lp_id_list: *mut c_void,
    lp_class: *const u16,
    hkey_class: *mut c_void,
    dw_hot_key: u32,
    h_icon: *mut c_void,
    h_process: *mut c_void,
}

// ---------------------------------------------------------------------------
// String helpers
// ---------------------------------------------------------------------------

use crate::platform::win32_helpers::wide;

fn from_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: callers pass a shell-owned, nul-terminated wide string. The
    // length cap prevents runaway reads from malformed output pointers.
    unsafe {
        let mut len = 0usize;
        while len < 32_768 && *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

fn from_wide_buffer(buffer: &[u16]) -> String {
    let len = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// Expands indirect strings such as `@%SystemRoot%\system32\shell32.dll,-id`.
fn resolve_text(text: String) -> String {
    if !text.starts_with('@') {
        return text;
    }
    let source = wide(&text);
    let mut output = vec![0u16; 1024];
    // SAFETY: both buffers outlive the call.
    let hr = unsafe {
        SHLoadIndirectString(
            source.as_ptr(),
            output.as_mut_ptr(),
            output.len() as u32,
            null_mut(),
        )
    };
    if hr < 0 {
        return text;
    }
    let resolved = from_wide_buffer(&output);
    if resolved.is_empty() { text } else { resolved }
}

// ---------------------------------------------------------------------------
// Property store helpers
// ---------------------------------------------------------------------------

/// SAFETY: COM STA is initialized and `store` is a live IPropertyStore.
unsafe fn read_prop_string(store: &ComPtr, key: &PropertyKey) -> Option<String> {
    let get_value: unsafe extern "system" fn(
        *mut c_void,
        *const PropertyKey,
        *mut PropVariant,
    ) -> i32 = unsafe { com_method(store.as_ptr(), 5) };
    let mut value: PropVariant = unsafe { zeroed() };
    // SAFETY: `value` is correctly laid out and cleared below.
    if unsafe { get_value(store.as_ptr(), key as *const PropertyKey, &mut value) } < 0 {
        return None;
    }
    let result = if value.vt == VT_LPWSTR || value.vt == VT_BSTR {
        Some(from_wide(value.data[0] as *const u16))
    } else {
        None
    };
    // SAFETY: PROPVARIANT was zero-initialized before the call.
    let _ = unsafe { PropVariantClear(&mut value) };
    result
}

/// SAFETY: COM STA is initialized and `store` is a live IPropertyStore.
unsafe fn read_prop_bool(store: &ComPtr, key: &PropertyKey) -> Option<bool> {
    let get_value: unsafe extern "system" fn(
        *mut c_void,
        *const PropertyKey,
        *mut PropVariant,
    ) -> i32 = unsafe { com_method(store.as_ptr(), 5) };
    let mut value: PropVariant = unsafe { zeroed() };
    // SAFETY: `value` is correctly laid out and cleared below.
    if unsafe { get_value(store.as_ptr(), key as *const PropertyKey, &mut value) } < 0 {
        return None;
    }
    let result = value.vt == VT_BOOL && (value.data[0] as u16 as i16) != 0;
    // SAFETY: PROPVARIANT was zero-initialized before the call.
    let _ = unsafe { PropVariantClear(&mut value) };
    Some(result)
}

// ---------------------------------------------------------------------------
// Identity resolution
// ---------------------------------------------------------------------------

fn resolve_identity(window: usize, shortcut: Option<&str>) -> Option<String> {
    // SAFETY: COM STA is initialized by `read_jump_list`.
    unsafe {
        if window != 0 {
            let mut raw = null_mut();
            let hr = SHGetPropertyStoreForWindow(
                window as *mut c_void,
                &IID_IPROPERTYSTORE as *const GUID,
                &mut raw,
            );
            if hr >= 0 && !raw.is_null() {
                let store = ComPtr::from_raw(raw);
                if let Some(id) = read_prop_string(&store, &KEY_APP_ID)
                    && !id.trim().is_empty()
                {
                    return Some(id);
                }
            }
        }

        let resolver = ComPtr::create(&CLSID_APP_RESOLVER, &IID_APP_RESOLVER).ok()?;
        let mut id_ptr: *mut u16 = null_mut();

        if window != 0 {
            // IApplicationResolver::ResolveWindow (private shell interface).
            let resolve: unsafe extern "system" fn(
                *mut c_void,
                *mut c_void,
                *mut *mut u16,
                isize,
                isize,
                isize,
            ) -> i32 = com_method(resolver.as_ptr(), 5);
            hr_ok(resolve(
                resolver.as_ptr(),
                window as *mut c_void,
                &mut id_ptr,
                0,
                0,
                0,
            ))
            .ok()?;
        } else {
            let path = shortcut?;
            // IApplicationResolver::ResolveShortcut (private shell interface).
            let path = wide(path);
            let mut item = null_mut();
            let hr = SHCreateItemFromParsingName(
                path.as_ptr(),
                null_mut(),
                &IID_ISHELLITEM as *const GUID,
                &mut item,
            );
            hr_ok(hr).ok()?;
            if item.is_null() {
                return None;
            }
            let item = ComPtr::from_raw(item);
            let resolve: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut u16) -> i32 =
                com_method(resolver.as_ptr(), 3);
            hr_ok(resolve(resolver.as_ptr(), item.as_ptr(), &mut id_ptr)).ok()?;
        }

        if id_ptr.is_null() {
            return None;
        }
        let id = from_wide(id_ptr);
        CoTaskMemFree(id_ptr.cast());
        (!id.trim().is_empty()).then_some(id)
    }
}

// ---------------------------------------------------------------------------
// Reading the list
// ---------------------------------------------------------------------------

fn read_inner(
    window: usize,
    shortcut: Option<&str>,
    groups: &mut Vec<JumpListGroup>,
    commands: &mut HashMap<i32, Launch>,
    activation_hwnd: &mut usize,
) -> Result<JumpListStatus, i32> {
    if size_of::<usize>() != 8 {
        return Ok(JumpListStatus::Unsupported);
    }

    let Some(app_id) = resolve_identity(window, shortcut) else {
        return Ok(JumpListStatus::IdentityUnavailable);
    };
    *activation_hwnd = window;

    let app_id_wide = wide(&app_id);
    let destination = unsafe { ComPtr::create(&CLSID_DESTINATION, &IID_DESTINATION_READ) }?;

    // SetAppID (private destination interface, slot 4). Note: the documented
    // ICustomDestinationList has the same slot, but no read-side methods.
    let set_app_id: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
        unsafe { com_method(destination.as_ptr(), 4) };
    hr_ok(unsafe { set_app_id(destination.as_ptr(), app_id_wide.as_ptr()) })?;

    let mut next_command = 1i32;
    let automatic = unsafe { ComPtr::create(&CLSID_AUTOMATIC, &IID_AUTOMATIC_LIST) }.ok();
    if let Some(auto) = automatic.as_ref() {
        // InitializeAutomatic (slot 3).
        let initialize: unsafe extern "system" fn(
            *mut c_void,
            *const u16,
            *mut c_void,
            *mut c_void,
        ) -> i32 = unsafe { com_method(auto.as_ptr(), 3) };
        let _ = unsafe { initialize(auto.as_ptr(), app_id_wide.as_ptr(), null_mut(), null_mut()) };
        add_automatic(
            auto,
            0,
            "Pinned destinations",
            groups,
            commands,
            &mut next_command,
        );
    }

    // GetCount (slot 6).
    let get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 =
        unsafe { com_method(destination.as_ptr(), 6) };
    let mut count = 0u32;
    let hr = unsafe { get_count(destination.as_ptr(), &mut count) };
    if missing_list(hr) {
        count = 0;
    } else {
        hr_ok(hr)?;
    }
    if count > MAX_CATEGORIES {
        return Err(E_UNEXPECTED);
    }

    let mut has_known_category = false;
    for index in 0..count {
        if next_command >= MAX_COMMANDS {
            break;
        }

        // GetCategory (slot 7). The structure is a union: for known
        // categories `name_or_kind` holds an integer kind, otherwise a
        // taskbar-allocated wide string that must be freed.
        let get_category: unsafe extern "system" fn(*mut c_void, u32, i32, *mut Category) -> i32 =
            unsafe { com_method(destination.as_ptr(), 7) };
        let mut category: Category = unsafe { zeroed() };
        hr_ok(unsafe { get_category(destination.as_ptr(), index, 1, &mut category) })?;

        let mut name_ptr: *mut u16 = null_mut();
        match category.category_type {
            1 => {
                let kind = category.name_or_kind as i32;
                match kind {
                    1 | 2 => {
                        has_known_category = true;
                        if let Some(auto) = automatic.as_ref() {
                            add_automatic(
                                auto,
                                3 - kind,
                                if kind == 1 { "Frequent" } else { "Recent" },
                                groups,
                                commands,
                                &mut next_command,
                            );
                        }
                    }
                    _ => {}
                }
            }
            0 | 2 => {
                name_ptr = category.name_or_kind as *mut u16;
                let title = if category.category_type == 2 {
                    "Tasks".to_owned()
                } else {
                    resolve_text(from_wide(name_ptr))
                };
                // EnumerateCategory (slot 9) returns an IObjectArray.
                let enumerate: unsafe extern "system" fn(
                    *mut c_void,
                    u32,
                    *const GUID,
                    *mut *mut c_void,
                ) -> i32 = unsafe { com_method(destination.as_ptr(), 9) };
                let mut array_ptr = null_mut();
                hr_ok(unsafe {
                    enumerate(
                        destination.as_ptr(),
                        index,
                        &IID_IUNKNOWN as *const GUID,
                        &mut array_ptr,
                    )
                })?;
                if !array_ptr.is_null() {
                    let array = unsafe { ComPtr::from_raw(array_ptr) };
                    add_group(&title, &array, groups, commands, &mut next_command)?;
                }
            }
            _ => {}
        }

        if category.category_type == 0 && !name_ptr.is_null() {
            unsafe { CoTaskMemFree(name_ptr.cast()) };
        }
    }

    if !has_known_category && let Some(auto) = automatic.as_ref() {
        add_automatic(auto, 1, "Recent", groups, commands, &mut next_command);
    }

    Ok(if groups.is_empty() {
        JumpListStatus::Empty
    } else {
        JumpListStatus::Ready
    })
}

/// Reads one of the automatic categories: 0 = pinned, 1 = recent, 2 = frequent.
fn add_automatic(
    automatic: &ComPtr,
    kind: i32,
    title: &str,
    groups: &mut Vec<JumpListGroup>,
    commands: &mut HashMap<i32, Launch>,
    next_command: &mut i32,
) {
    // GetAutomaticList (slot 5).
    let get_list: unsafe extern "system" fn(
        *mut c_void,
        i32,
        u32,
        u32,
        *const GUID,
        *mut *mut c_void,
    ) -> i32 = unsafe { com_method(automatic.as_ptr(), 5) };
    let mut array_ptr = null_mut();
    let hr = unsafe {
        get_list(
            automatic.as_ptr(),
            kind,
            20,
            1,
            &IID_IUNKNOWN as *const GUID,
            &mut array_ptr,
        )
    };
    if missing_list(hr) || hr < 0 || array_ptr.is_null() {
        return;
    }
    let array = unsafe { ComPtr::from_raw(array_ptr) };
    // Errors in optional automatic categories are ignored; the main list is
    // still useful without them.
    let _ = add_group(title, &array, groups, commands, next_command);
}

fn add_group(
    title: &str,
    array: &ComPtr,
    groups: &mut Vec<JumpListGroup>,
    commands: &mut HashMap<i32, Launch>,
    next_command: &mut i32,
) -> Result<(), i32> {
    // IObjectArray::GetCount (slot 3).
    let get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 =
        unsafe { com_method(array.as_ptr(), 3) };
    let mut count = 0u32;
    hr_ok(unsafe { get_count(array.as_ptr(), &mut count) })?;
    let limit = (count as usize).min(MAX_GROUP_ENTRIES);
    let mut entries = Vec::with_capacity(limit);

    for index in 0..limit {
        if *next_command > MAX_COMMANDS {
            break;
        }
        // IObjectArray::GetAt (slot 4).
        let get_at: unsafe extern "system" fn(
            *mut c_void,
            u32,
            *const GUID,
            *mut *mut c_void,
        ) -> i32 = unsafe { com_method(array.as_ptr(), 4) };
        let mut item_ptr = null_mut();
        hr_ok(unsafe {
            get_at(
                array.as_ptr(),
                index as u32,
                &IID_IUNKNOWN as *const GUID,
                &mut item_ptr,
            )
        })?;
        if item_ptr.is_null() {
            continue;
        }
        let item = unsafe { ComPtr::from_raw(item_ptr) };

        // SAFETY: the item is alive for the remainder of this iteration.
        let store = item.query(&IID_IPROPERTYSTORE);
        let is_separator = store
            .as_ref()
            .and_then(|s| unsafe { read_prop_bool(s, &KEY_SEPARATOR) })
            .unwrap_or(false);
        if is_separator {
            if entries
                .last()
                .is_some_and(|entry: &JumpListEntry| !entry.is_separator)
            {
                entries.push(JumpListEntry {
                    command_id: 0,
                    title: String::new(),
                    icon: None,
                    is_separator: true,
                });
            }
            continue;
        }

        let mut label = store
            .as_ref()
            .and_then(|s| unsafe { read_prop_string(s, &KEY_TITLE) })
            .map(resolve_text);
        if label.as_deref().is_none_or(|value| value.trim().is_empty())
            && let Some(shell_item) = item.query(&IID_ISHELLITEM)
        {
            let get_display_name: unsafe extern "system" fn(
                *mut c_void,
                u32,
                *mut *mut u16,
            ) -> i32 = unsafe { com_method(shell_item.as_ptr(), 5) };
            let mut name_ptr = null_mut();
            if unsafe { get_display_name(shell_item.as_ptr(), SIGDN_NORMALDISPLAY, &mut name_ptr) }
                >= 0
                && !name_ptr.is_null()
            {
                label = Some(from_wide(name_ptr));
                unsafe { CoTaskMemFree(name_ptr.cast()) };
            }
        }
        let Some(title) = label.filter(|value| !value.trim().is_empty()) else {
            continue;
        };

        let id = *next_command;
        *next_command += 1;
        let launch = read_launch(&item, store.as_ref());
        if launch.target.is_empty() {
            commands.remove(&id);
        } else {
            commands.insert(id, launch);
        }
        entries.push(JumpListEntry {
            command_id: id,
            title,
            icon: None,
            is_separator: false,
        });
    }

    if entries
        .last()
        .is_some_and(|entry: &JumpListEntry| entry.is_separator)
    {
        entries.pop();
    }
    if !entries.is_empty() && !title.trim().is_empty() {
        groups.push(JumpListGroup {
            title: title.to_owned(),
            entries,
        });
    }
    Ok(())
}

/// Extracts an owned launch description from a shell link task.
fn read_launch(item: &ComPtr, store: Option<&ComPtr>) -> Launch {
    let mut target = String::new();
    let mut args = String::new();
    let mut working_dir = String::new();
    let mut icon_location = String::new();

    if let Some(link) = item.query(&IID_ISHELLLINKW) {
        let mut buffer = vec![0u16; 4096];

        // IShellLinkW::GetPath (slot 3).
        let get_path: unsafe extern "system" fn(
            *mut c_void,
            *mut u16,
            i32,
            *mut c_void,
            u32,
        ) -> i32 = unsafe { com_method(link.as_ptr(), 3) };
        if unsafe {
            get_path(
                link.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as i32,
                null_mut(),
                SLGP_RAWPATH,
            )
        } >= 0
        {
            target = from_wide_buffer(&buffer);
        }

        // IShellLinkW::GetWorkingDirectory (slot 8).
        buffer.fill(0);
        let get_dir: unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32 =
            unsafe { com_method(link.as_ptr(), 8) };
        if unsafe { get_dir(link.as_ptr(), buffer.as_mut_ptr(), buffer.len() as i32) } >= 0 {
            working_dir = from_wide_buffer(&buffer);
        }

        // IShellLinkW::GetArguments (slot 10).
        buffer.fill(0);
        let get_args: unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32 =
            unsafe { com_method(link.as_ptr(), 10) };
        if unsafe { get_args(link.as_ptr(), buffer.as_mut_ptr(), buffer.len() as i32) } >= 0 {
            args = from_wide_buffer(&buffer);
        }

        // IShellLinkW::GetIconLocation (slot 16).
        buffer.fill(0);
        let mut icon_index = 0i32;
        let get_icon: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut i32) -> i32 =
            unsafe { com_method(link.as_ptr(), 16) };
        if unsafe {
            get_icon(
                link.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as i32,
                &mut icon_index,
            )
        } >= 0
        {
            let path = from_wide_buffer(&buffer);
            if !path.is_empty() {
                icon_location = if icon_index == 0 {
                    path
                } else {
                    format!("{path},{icon_index}")
                };
            }
        }
    }

    // The reference uses vtable slots 9/11 for arguments/working directory,
    // which are actually setters; the real getters (10/8) are used above. Keep
    // the property-store fallback for tasks that publish their switches there.
    if args.trim().is_empty()
        && let Some(store) = store
        && let Some(fallback) = unsafe { read_prop_string(store, &KEY_LINK_ARGUMENTS) }
    {
        args = fallback;
    }

    let packaged = store
        .and_then(|s| unsafe { read_prop_string(s, &KEY_APP_ID) })
        .is_some_and(|id| id.contains('!'));
    if packaged {
        let app_id = store
            .and_then(|s| unsafe { read_prop_string(s, &KEY_APP_ID) })
            .unwrap_or_default();
        target = format!("shell:AppsFolder\\{app_id}");
    }

    if target.trim().is_empty() {
        target = icon_executable_fallback(&icon_location);
    }

    Launch {
        target,
        args,
        working_dir,
        icon: icon_location,
    }
}

/// Some synthetic tasks have no `GetPath` but do have an `.exe` icon resource.
fn icon_executable_fallback(icon_location: &str) -> String {
    let path = icon_location.split(',').next().unwrap_or("").trim();
    if path.to_ascii_lowercase().ends_with(".exe") {
        path.to_owned()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Pure tests (no COM involvement)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_variants_are_comparable_and_distinct() {
        let all = [
            JumpListStatus::Ready,
            JumpListStatus::Empty,
            JumpListStatus::IdentityUnavailable,
            JumpListStatus::Unsupported,
            JumpListStatus::Unavailable,
        ];
        for (index, status) in all.iter().enumerate() {
            for (other_index, other) in all.iter().enumerate() {
                assert_eq!(status == other, index == other_index);
            }
        }
    }

    #[test]
    fn entries_and_groups_preserve_titles_and_separators() {
        let entry = JumpListEntry {
            command_id: 7,
            title: "Private window".to_owned(),
            icon: None,
            is_separator: false,
        };
        let separator = JumpListEntry {
            command_id: 0,
            title: String::new(),
            icon: None,
            is_separator: true,
        };
        let group = JumpListGroup {
            title: "Tasks".to_owned(),
            entries: vec![entry.clone(), separator.clone()],
        };
        assert_eq!(group.entries.len(), 2);
        assert_eq!(group.entries[0].title, "Private window");
        assert_eq!(group.entries[0].command_id, 7);
        assert!(group.entries[1].is_separator);
        assert_eq!(group.entries[1].command_id, 0);
        assert_eq!(entry, group.entries[0].clone());
        assert_eq!(separator, group.entries[1].clone());
    }

    #[test]
    fn wide_strings_round_trip_including_non_ascii() {
        let original = "Özel Görevler — özel pencere";
        let encoded = wide(original);
        assert_eq!(encoded.last(), Some(&0));
        assert_eq!(from_wide(encoded.as_ptr()), original);
        assert_eq!(from_wide_buffer(&encoded), original);
    }
}

// ---------------------------------------------------------------------------
// WS-G: launch descriptors / Explorer-compatible argument quoting.
// ---------------------------------------------------------------------------

/// One resolved jump-list/item target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildLaunchInfo {
    pub executable: String,
    pub arguments: String,
    pub working_directory: String,
    pub use_shell_execute: bool,
    pub display_name: String,
}

impl BuildLaunchInfo {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            arguments: String::new(),
            working_directory: String::new(),
            use_shell_execute: true,
            display_name: String::new(),
        }
    }

    /// Whether launching needs shell semantics (document/URL/elevation).
    pub fn needs_shell(&self) -> bool {
        self.use_shell_execute
            || self.executable.starts_with("shell:")
            || self.executable.starts_with("ms-settings:")
            || self.executable.starts_with("http://")
            || self.executable.starts_with("https://")
            || self.executable.eq_ignore_ascii_case("explorer.exe")
    }

    /// Launch via `ShellExecuteW` (non-Shell targets fall back to `CreateProcess`-like `Command`).
    pub fn launch(&self) -> bool {
        let path = self.executable.trim();
        if path.is_empty() {
            return false;
        }
        if !self.needs_shell() {
            let mut command = std::process::Command::new(path);
            if !self.arguments.is_empty() {
                // Split using Explorer quoting rules before handing to Command.
                command.args(split_arguments(&self.arguments));
            }
            if !self.working_directory.is_empty() {
                command.current_dir(&self.working_directory);
            }
            return command.spawn().is_ok();
        }
        let wide_path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let wide_args: Vec<u16> = self.arguments.encode_utf16().chain(Some(0)).collect();
        let wide_dir: Vec<u16> = self
            .working_directory
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let dir_ptr = if self.working_directory.is_empty() {
            std::ptr::null()
        } else {
            wide_dir.as_ptr()
        };
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let rc = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                wide_path.as_ptr(),
                wide_args.as_ptr(),
                dir_ptr,
                SW_SHOWNORMAL,
            )
        };
        rc as isize > 32
    }
}

/// Explorer-style argument splitter: honours `"` and `\"`, drops empty tokens.
pub fn split_arguments(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escape = false;
    let mut started = false;
    let chars: Vec<char> = args.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if escape {
            current.push(c);
            escape = false;
            started = true;
            continue;
        }
        match c {
            '\\' if chars.get(i) == Some(&'"') => {
                escape = true;
                started = true;
            }
            '\\' => {
                current.push('\\');
                started = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if escape {
        current.push('\\');
    }
    if started {
        out.push(current);
    }
    out
}

/// Quotes an argument for a jump-list command line (`BuildQuotedArguments`).
pub fn quote_argument(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c.is_whitespace() || c == '"') {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Derives a working directory for a launcher: explicit value, else the executable's folder.
pub fn resolve_working_directory(executable: &str, explicit: &str) -> String {
    if !explicit.trim().is_empty() {
        return explicit.trim().to_owned();
    }
    Path::new(executable.trim())
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Explorer special-case: `explorer.exe /select,<path>` needs shell execute and `/select,`.
pub fn explorer_select(path: &str) -> BuildLaunchInfo {
    let mut info = BuildLaunchInfo::new("explorer.exe");
    info.arguments = format!("/select,{}", quote_argument(path));
    info.working_directory = resolve_working_directory("explorer.exe", "");
    info.use_shell_execute = true;
    info.display_name = PathBuf::from(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| loc::t("OpenLocation"));
    info
}

#[cfg(test)]
mod ws_g_tests {
    use super::*;

    #[test]
    fn build_launch_info_shell_execute_and_working_dir() {
        let mut info = BuildLaunchInfo::new(r"C:\Program Files\App\app.exe");
        info.arguments = "--flag \"two words\"".into();
        info.working_directory = r"C:\Program Files\App".into();
        assert!(info.needs_shell());
        info.use_shell_execute = false;
        assert!(!info.needs_shell());
        assert_eq!(
            resolve_working_directory(r"C:\Tools\x.exe", ""),
            r"C:\Tools"
        );
        assert_eq!(resolve_working_directory("x.exe", r"D:\work"), r"D:\work");
    }

    #[test]
    fn argument_splitting_and_quoting() {
        assert_eq!(
            split_arguments(r#"--path "C:\Program Files\x" --n 2"#),
            vec!["--path", r"C:\Program Files\x", "--n", "2"]
        );
        assert_eq!(split_arguments(r#"a\"b c"#), vec![r#"a"b"#, "c"]);
        assert_eq!(quote_argument("plain"), "plain");
        assert_eq!(
            quote_argument(r#"a "quoted" path"#),
            r#""a \"quoted\" path""#
        );
    }

    #[test]
    fn explorer_select_uses_shell_execute() {
        let info = explorer_select(r"C:\Users\me\file.txt");
        assert!(info.needs_shell());
        assert!(info.arguments.starts_with("/select,"));
        assert_eq!(info.display_name, "file.txt");
    }
}
