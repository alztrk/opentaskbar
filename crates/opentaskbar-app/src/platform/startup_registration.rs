//! Startup registration for both packaging modes.
//!
//! Portable builds use `HKCU\...\Run\OpenTaskbar`, honour Task Manager's
//! `StartupApproved\Run` disable flag, and repair stale shortcuts in the
//! Startup folder. Packaged (MSIX) builds must use the manifest `StartupTask`
//! instead; we never write a Run key for them and remove stale ones on startup.

use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

use opentaskbar_core::log;

use super::app_identity::{self, StartupPolicy};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_APPROVED_RUN_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const VALUE_NAME: &str = "OpenTaskbar";
const EXECUTABLE_FILE_NAME: &str = "OpenTaskbar.exe";
const MAX_COMMAND_LENGTH: usize = 1024;
const ENABLED_STARTUP_APPROVED_BYTES: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

use crate::platform::win32_helpers::wide;

fn wide_name(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(Some(0)).collect()
}

fn open_key(subkey: &str, writable: bool) -> Option<HKEY> {
    let subkey = wide(subkey);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: null-terminated UTF-16 path, output handle owned by the caller.
    let status = unsafe {
        if writable {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        } else {
            RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key)
        }
    };
    (status == 0).then_some(key)
}

fn read_string(key: HKEY, name: &str) -> Option<String> {
    let name = wide_name(name);
    // SAFETY: size query followed by a bounded read from a valid key handle.
    unsafe {
        let mut size = 0u32;
        if RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) != 0
            || size == 0
        {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        if RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            &mut size,
        ) != 0
        {
            return None;
        }
        let units: Vec<u16> = buffer
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Some(String::from_utf16_lossy(
            &units[..units.iter().position(|&c| c == 0).unwrap_or(units.len())],
        ))
    }
}

fn write_string(key: HKEY, name: &str, value: &str) -> bool {
    let name = wide_name(name);
    let value = wide(value);
    // SAFETY: null-terminated UTF-16 value written to a writable key handle.
    unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * 2) as u32,
        ) == 0
    }
}

fn delete_value(key: HKEY, name: &str) {
    let name = wide_name(name);
    // SAFETY: deleting a value from our own writable key handle.
    unsafe {
        RegDeleteValueW(key, name.as_ptr());
    }
}

fn read_binary(key: HKEY, name: &str) -> Option<Vec<u8>> {
    let name = wide_name(name);
    // SAFETY: size query followed by a bounded binary read.
    unsafe {
        let mut size = 0u32;
        if RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) != 0
            || size == 0
        {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        if RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            &mut size,
        ) != 0
        {
            return None;
        }
        Some(buffer)
    }
}

fn write_binary(key: HKEY, name: &str, value: &[u8]) -> bool {
    let name = wide_name(name);
    // SAFETY: binary bytes written to a writable key handle.
    unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_BINARY,
            value.as_ptr(),
            value.len() as u32,
        ) == 0
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLongPathNameW(short_path: *const u16, long_path: *mut u16, buffer_length: u32) -> u32;
    fn ExpandEnvironmentStringsW(source: *const u16, target: *mut u16, target_size: u32) -> u32;
}

#[repr(C)]
struct VsFixedFileInfo {
    signature: u32,
    stride: u32,
    file_version_ms: u32,
    file_version_ls: u32,
    product_version_ms: u32,
    product_version_ls: u32,
    file_flags_mask: u32,
    file_flags: u32,
    file_os: u32,
    file_type: u32,
    file_subtype: u32,
    file_date_ms: u32,
    file_date_ls: u32,
}

#[link(name = "version")]
unsafe extern "system" {
    fn GetFileVersionInfoSizeW(file_name: *const u16, handle: *mut u32) -> u32;
    fn GetFileVersionInfoW(
        file_name: *const u16,
        handle: u32,
        length: u32,
        data: *mut c_void,
    ) -> i32;
    fn VerQueryValueW(
        data: *const c_void,
        sub_block: *const u16,
        buffer: *mut *mut c_void,
        length: *mut u32,
    ) -> i32;
}

/// Parses a Run command, including unquoted paths containing spaces
/// (e.g. `C:\Program Files\OpenTaskbar\OpenTaskbar.exe --flag`).
pub fn parse_run_command(command_line: &str) -> (Option<String>, String) {
    let trimmed = command_line.trim();
    if trimmed.is_empty() {
        return (None, String::new());
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        return match rest.find('"') {
            Some(closing) => {
                let exe = rest[..closing].trim();
                let args = rest[closing + 1..].trim();
                ((!exe.is_empty()).then(|| exe.to_owned()), args.to_owned())
            }
            None => {
                let exe = rest.trim();
                ((!exe.is_empty()).then(|| exe.to_owned()), String::new())
            }
        };
    }
    let mut search_from = 0usize;
    while search_from < trimmed.len() {
        let Some(found) = trimmed[search_from..].to_ascii_lowercase().find(".exe") else {
            break;
        };
        let after = search_from + found + 4;
        if after == trimmed.len() || trimmed[after..].starts_with(char::is_whitespace) {
            let exe = trimmed[..after].trim();
            return (
                (!exe.is_empty()).then(|| exe.to_owned()),
                trimmed[after..].trim().to_owned(),
            );
        }
        search_from = after;
    }
    match trimmed.find(char::is_whitespace) {
        Some(split) => (
            Some(trimmed[..split].to_owned()),
            trimmed[split + 1..].trim().to_owned(),
        ),
        None => (Some(trimmed.to_owned()), String::new()),
    }
}

pub fn is_startup_approved_enabled(data: Option<&[u8]>) -> bool {
    data.is_none_or(|data| data.is_empty() || data[0] & 1 == 0)
}

/// Normalizes a fully qualified path and resolves 8.3 short names.
pub fn try_normalize_path(path: &str) -> Option<String> {
    let trimmed = path.trim();
    let candidate = Path::new(trimmed);
    if !candidate.is_absolute() {
        return None;
    }
    let full = candidate.to_path_buf();
    let raw = full.as_os_str().encode_wide();
    let mut input: Vec<u16> = raw.chain(Some(0)).collect();
    let mut buffer = vec![0u16; 1024];
    // SAFETY: UTF-16 input and bounded output buffers.
    let length =
        unsafe { GetLongPathNameW(input.as_mut_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) };
    if length == 0 || length as usize >= buffer.len() {
        return full.to_str().map(str::to_owned);
    }
    Some(String::from_utf16_lossy(&buffer[..length as usize]))
}

/// Pure `%VAR%` expansion used by shortcut and Run-command repair. Unknown
/// variables are left untouched so an existing path is not mangled.
pub fn expand_env_with(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < text.len() {
        let rest = &text[index..];
        if let Some(stripped) = rest.strip_prefix('%')
            && let Some(end) = stripped.find('%')
        {
            let name = &stripped[..end];
            if !name.is_empty()
                && let Some(value) = lookup(name)
            {
                out.push_str(&value);
                index += 1 + end + 1;
                continue;
            }
        }
        let ch = rest.chars().next().unwrap_or('\0');
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// Expands environment variables using the Win32 process environment (which
/// also sees registry-defined user/system variables).
pub fn expand_environment_variables(text: &str) -> String {
    let source: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    // SAFETY: size query with a null output buffer is documented and safe.
    let size = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), std::ptr::null_mut(), 0) };
    if size == 0 {
        return expand_env_with(text, |name| std::env::var(name).ok());
    }
    let mut buffer = vec![0u16; size as usize];
    // SAFETY: `size` covers the null terminator reported by the query.
    let written = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), buffer.as_mut_ptr(), size) };
    if written == 0 || written > size {
        return expand_env_with(text, |name| std::env::var(name).ok());
    }
    let used = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..used])
}

fn file_version(path: &Path) -> Option<(u32, u32, u32, u32)> {
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: null-terminated UTF-16 path and valid out-handle.
    let mut handle = 0u32;
    let size = unsafe { GetFileVersionInfoSizeW(name.as_ptr(), &mut handle) };
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    // SAFETY: buffer size is exactly the value reported by the size query.
    if unsafe { GetFileVersionInfoW(name.as_ptr(), handle, size, buffer.as_mut_ptr().cast()) } == 0
    {
        return None;
    }
    let sub_block = wide("\\");
    let mut result: *mut c_void = std::ptr::null_mut();
    let mut length = 0u32;
    // SAFETY: the buffer stays alive for the duration of the query.
    if unsafe {
        VerQueryValueW(
            buffer.as_ptr().cast(),
            sub_block.as_ptr(),
            &mut result,
            &mut length,
        )
    } == 0
        || result.is_null()
        || (length as usize) < size_of::<VsFixedFileInfo>()
    {
        return None;
    }
    // SAFETY: VerQueryValueW returned a VS_FIXEDFILEINFO of at least that size.
    let info = unsafe { &*(result as *const VsFixedFileInfo) };
    Some((
        info.file_version_ms >> 16,
        info.file_version_ms & 0xFFFF,
        info.file_version_ls >> 16,
        info.file_version_ls & 0xFFFF,
    ))
}

/// True when `registered` is older than `current`. Falls back to the file
/// modification time for builds that do not embed a version resource.
fn is_older_executable(registered: &str, current: &str) -> bool {
    match (
        file_version(Path::new(registered)),
        file_version(Path::new(current)),
    ) {
        (Some(a), Some(b)) => a < b,
        _ => match (
            std::fs::metadata(registered).and_then(|meta| meta.modified()),
            std::fs::metadata(current).and_then(|meta| meta.modified()),
        ) {
            (Ok(a), Ok(b)) => a < b,
            _ => false,
        },
    }
}

fn is_under_directory(path: &str, directory: &str) -> bool {
    let Some(path) = try_normalize_path(path) else {
        return false;
    };
    let Some(directory) = try_normalize_path(directory) else {
        return false;
    };
    let prefix = format!(
        "{}{}",
        directory.trim_end_matches(['\\', '/']),
        std::path::MAIN_SEPARATOR
    );
    path.to_ascii_lowercase()
        .starts_with(&prefix.to_ascii_lowercase())
}

fn is_startup_path(executable: &str) -> bool {
    if executable.len() + 2 > MAX_COMMAND_LENGTH
        || !Path::new(executable).is_absolute()
        || executable.contains('"')
        || !Path::new(executable)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return false;
    }
    let Some(name) = Path::new(executable).file_stem().and_then(|n| n.to_str()) else {
        return false;
    };
    const BASE: &[u8] = b"OpenTaskbar";
    // Compare bytes rather than slicing `name`: a multi-byte character can
    // straddle byte 11 (e.g. `OpenTaskbağ.exe`), which would panic on a char
    // boundary while validating a malformed Run registration at startup.
    name.as_bytes()
        .get(..BASE.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(BASE))
        && name
            .as_bytes()
            .get(BASE.len())
            .is_none_or(|next| *next == b'-')
}

pub fn is_valid_startup_executable(path: &str, disallowed_root: Option<&str>) -> bool {
    if path.len() + 2 > MAX_COMMAND_LENGTH || path.contains(['"', '\r', '\n', '\0']) {
        return false;
    }
    let Some(normalized) = try_normalize_path(path) else {
        return false;
    };
    if !is_startup_path(&normalized) || !Path::new(&normalized).is_file() {
        return false;
    }
    disallowed_root.is_none_or(|root| !is_under_directory(&normalized, root))
}

pub fn is_registered_and_active(
    existing_run_value: Option<&str>,
    approved_data: Option<&[u8]>,
) -> bool {
    let Some(command) = existing_run_value else {
        return false;
    };
    if command.trim().is_empty() || !is_startup_approved_enabled(approved_data) {
        return false;
    }
    let (exe, _) = parse_run_command(command);
    let Some(exe) = exe else {
        return false;
    };
    try_normalize_path(&exe).is_some_and(|path| Path::new(&path).is_file())
}

pub fn should_update_run_registration(
    existing_run_value: Option<&str>,
    approved_data: Option<&[u8]>,
    current_executable: &str,
) -> bool {
    let Some(command) = existing_run_value else {
        return false;
    };
    if command.trim().is_empty() || !is_startup_approved_enabled(approved_data) {
        return false;
    }
    let Some(current) = try_normalize_path(current_executable) else {
        return false;
    };
    let (registered, _) = parse_run_command(command);
    match registered.and_then(|exe| try_normalize_path(&exe)) {
        None => true,
        Some(registered) => !registered.eq_ignore_ascii_case(&current),
    }
}

fn reenable_startup_approved_if_disabled() {
    let Some(read_key) = open_key(STARTUP_APPROVED_RUN_KEY, false) else {
        return;
    };
    let data = read_binary(read_key, VALUE_NAME);
    // SAFETY: key handle closed before returning.
    unsafe { RegCloseKey(read_key) };
    if data
        .as_deref()
        .is_some_and(|bytes| is_startup_approved_enabled(Some(bytes)))
    {
        return;
    }
    if let Some(write_key) = open_key(STARTUP_APPROVED_RUN_KEY, true) {
        write_binary(write_key, VALUE_NAME, &ENABLED_STARTUP_APPROVED_BYTES);
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(write_key) };
    }
}

fn delete_startup_approved_entry() {
    if let Some(key) = open_key(STARTUP_APPROVED_RUN_KEY, true) {
        delete_value(key, VALUE_NAME);
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(key) };
    }
}

/// Reads the effective startup state. Returns `(enabled, error_key)`.
pub fn read() -> (bool, Option<&'static str>) {
    match app_identity::startup_policy() {
        StartupPolicy::MsixStartupTask => match app_identity::read_startup_task() {
            Ok(enabled) => (enabled, None),
            Err(_) => (false, Some("StartupReadError")),
        },
        StartupPolicy::RunKey => {
            let Some(key) = open_key(RUN_KEY, false) else {
                return (false, Some("StartupReadError"));
            };
            let command = read_string(key, VALUE_NAME);
            // SAFETY: key handle closed before returning.
            unsafe { RegCloseKey(key) };
            let approved = open_key(STARTUP_APPROVED_RUN_KEY, false).and_then(|key| {
                let data = read_binary(key, VALUE_NAME);
                // SAFETY: key handle closed before returning.
                unsafe { RegCloseKey(key) };
                data
            });
            (
                is_registered_and_active(command.as_deref(), approved.as_deref()),
                None,
            )
        }
    }
}

/// Enables or disables startup. Returns an error key on failure (reference parity).
pub fn set_enabled(enabled: bool) -> Option<&'static str> {
    match app_identity::startup_policy() {
        StartupPolicy::MsixStartupTask => {
            // Real WinRT StartupTask read/write (GetAsync / RequestEnableAsync / Disable).
            match app_identity::set_startup_task_enabled(enabled) {
                Ok(observed) if observed == enabled => {
                    if !enabled {
                        remove_stale_unpackaged_run_entry();
                    }
                    None
                }
                Ok(_) => Some("StartupDisabledByPolicy"),
                Err(_) => Some("StartupWriteError"),
            }
        }
        StartupPolicy::RunKey => {
            let exe = std::env::current_exe().ok()?;
            let path = exe.to_string_lossy();
            let normalized = if enabled {
                if !is_valid_startup_executable(&path, None) {
                    return Some("StartupPathError");
                }
                match try_normalize_path(&path) {
                    Some(path) if path.len() + 2 <= MAX_COMMAND_LENGTH => path,
                    _ => return Some("StartupPathError"),
                }
            } else {
                String::new()
            };
            let Some(key) = open_key(RUN_KEY, true) else {
                return Some("StartupWriteError");
            };
            let ok = if enabled {
                write_string(key, VALUE_NAME, &format!("\"{normalized}\""))
            } else {
                delete_value(key, VALUE_NAME);
                true
            };
            // SAFETY: key handle closed before returning.
            unsafe { RegCloseKey(key) };
            if !ok {
                return Some("StartupWriteError");
            }
            if enabled {
                reenable_startup_approved_if_disabled();
            } else {
                delete_startup_approved_entry();
            }
            None
        }
    }
}

fn remove_stale_unpackaged_run_entry() {
    if let Some(key) = open_key(RUN_KEY, true) {
        delete_value(key, VALUE_NAME);
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(key) };
        delete_startup_approved_entry();
        log::info("Removed stale unpackaged startup Run registration for packaged OpenTaskbar.");
    }
}

fn sync_unpackaged_run_entry(normalized: &str) {
    let Some(read_key) = open_key(RUN_KEY, false) else {
        return;
    };
    let command = read_string(read_key, VALUE_NAME);
    // SAFETY: key handle closed before returning.
    unsafe { RegCloseKey(read_key) };
    let approved = open_key(STARTUP_APPROVED_RUN_KEY, false).and_then(|key| {
        let data = read_binary(key, VALUE_NAME);
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(key) };
        data
    });
    if !should_update_run_registration(command.as_deref(), approved.as_deref(), normalized) {
        return;
    }
    if let Some(key) = open_key(RUN_KEY, true) {
        write_string(key, VALUE_NAME, &format!("\"{normalized}\""));
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(key) };
        log::info("Updated startup Run registration to the current executable.");
    }
}

/// Ensures the `StartupApproved\Run` byte 0 is in the enabled state when our Run
/// entry exists but Task Manager's flag was left disabled by an older build.
fn ensure_startup_approved_byte0() {
    let Some(read_key) = open_key(RUN_KEY, false) else {
        return;
    };
    let command = read_string(read_key, VALUE_NAME);
    // SAFETY: key handle closed before returning.
    unsafe { RegCloseKey(read_key) };
    let Some(command) = command else {
        return;
    };
    let (exe, _) = parse_run_command(&command);
    let Some(exe) = exe.and_then(|exe| try_normalize_path(&expand_environment_variables(&exe)))
    else {
        return;
    };
    if !Path::new(&exe).is_file() {
        return;
    }
    let Some(approved_key) = open_key(STARTUP_APPROVED_RUN_KEY, false) else {
        return;
    };
    let data = read_binary(approved_key, VALUE_NAME);
    // SAFETY: key handle closed before returning.
    unsafe { RegCloseKey(approved_key) };
    if data
        .as_deref()
        .is_some_and(|bytes| is_startup_approved_enabled(Some(bytes)))
    {
        return;
    }
    if let Some(key) = open_key(STARTUP_APPROVED_RUN_KEY, true) {
        write_binary(key, VALUE_NAME, &ENABLED_STARTUP_APPROVED_BYTES);
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(key) };
    }
}

/// Single entry point used on every normal startup (and by the Settings UI).
///
/// Repairs the portable Run key and its `StartupApproved\Run` byte 0, refreshes
/// a stale portable target using PE file versions, and rewrites Startup-folder
/// shortcuts whose target contains environment variables. Packaged builds only
/// observe the manifest `StartupTask` and remove stale unpackaged Run entries.
pub fn sync_all() -> Option<&'static str> {
    match app_identity::startup_policy() {
        StartupPolicy::MsixStartupTask => {
            remove_stale_unpackaged_run_entry();
            if app_identity::read_startup_task().is_err() {
                return Some("StartupReadError");
            }
            None
        }
        StartupPolicy::RunKey => {
            let Ok(exe) = std::env::current_exe() else {
                return Some("StartupPathError");
            };
            let raw = exe.to_string_lossy();
            if !is_valid_startup_executable(&raw, Some(&std::env::temp_dir().to_string_lossy())) {
                return Some("StartupPathError");
            }
            let Some(normalized) = try_normalize_path(&raw) else {
                return Some("StartupPathError");
            };
            if normalized.len() + 2 > MAX_COMMAND_LENGTH {
                return Some("StartupPathError");
            }
            sync_unpackaged_run_entry(&normalized);
            ensure_startup_approved_byte0();
            let result = refresh_portable_target();
            if let Some(folder) = startup_folder()
                && folder.is_dir()
            {
                sync_startup_folder_shortcuts(&folder, &normalized);
            }
            result
        }
    }
}

fn startup_folder() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| {
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
    })
}

/// Repairs Startup-folder shortcuts that still point at an older install.
pub fn sync_startup_folder_shortcuts(folder: &Path, current_executable: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return 0;
    };
    let Some(current) = try_normalize_path(&expand_environment_variables(current_executable))
    else {
        return 0;
    };
    // COM STA for IShellLinkW on this thread; the shared guard balances the
    // initialization and tolerates a thread already in another apartment.
    let _com = super::com::ComApartment::sta();
    let mut updated = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_shortcut = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"));
        if !is_shortcut {
            continue;
        }
        let Some(link_path) = path.to_str() else {
            continue;
        };
        // SAFETY: COM STA initialized above; the shell link is released before
        // leaving the scope.
        unsafe {
            let Ok(link) = windows::Win32::System::Com::CoCreateInstance::<
                _,
                windows::Win32::UI::Shell::IShellLinkW,
            >(
                &windows::Win32::UI::Shell::ShellLink,
                None,
                windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
            ) else {
                continue;
            };
            let Ok(persist) = link.cast::<windows::Win32::System::Com::IPersistFile>() else {
                continue;
            };
            use windows::core::Interface;
            if persist
                .Load(
                    &windows::core::HSTRING::from(link_path),
                    windows::Win32::System::Com::STGM_READ,
                )
                .is_err()
            {
                continue;
            }
            let mut buffer = [0u16; 1024];
            if link.GetPath(&mut buffer, std::ptr::null_mut(), 0).is_err() {
                continue;
            }
            let used = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            let target = expand_environment_variables(&String::from_utf16_lossy(&buffer[..used]));
            let Some(normalized_target) = try_normalize_path(&target) else {
                continue;
            };
            let is_our_exe = Path::new(&normalized_target)
                .file_name()
                .is_some_and(|name| {
                    name.to_string_lossy()
                        .eq_ignore_ascii_case(EXECUTABLE_FILE_NAME)
                });
            if !is_our_exe || normalized_target.eq_ignore_ascii_case(&current) {
                continue;
            }
            let _ = link.SetPath(&windows::core::HSTRING::from(&current));
            if let Some(parent) = Path::new(&current).parent() {
                let _ = link.SetWorkingDirectory(&windows::core::HSTRING::from(
                    parent.to_string_lossy().as_ref(),
                ));
            }
            if persist
                .Save(&windows::core::HSTRING::from(link_path), true)
                .is_ok()
            {
                updated += 1;
                log::info(format!("Updated startup shortcut '{link_path}'."));
            }
        }
    }
    updated
}

/// Called on every normal startup (dev/inspection modes are skipped).
pub fn sync_on_startup(is_dev: bool, is_inspection: bool) {
    if is_dev || is_inspection {
        return;
    }
    let _ = sync_all();
}

/// Repairs a stale portable Run target whose executable is missing or older.
pub fn refresh_portable_target() -> Option<&'static str> {
    if !matches!(app_identity::startup_policy(), StartupPolicy::RunKey) {
        return None;
    }
    let Ok(current) = std::env::current_exe() else {
        return None;
    };
    let path = current.to_string_lossy();
    if !is_startup_path(&path) || !Path::new(path.as_ref()).is_file() {
        return None;
    }
    let key = open_key(RUN_KEY, true)?;
    let command = read_string(key, VALUE_NAME);
    let (registered, _) = command
        .as_deref()
        .map(parse_run_command)
        .unwrap_or((None, String::new()));
    let Some(registered) = registered else {
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(key) };
        return None;
    };
    let registered = expand_environment_variables(&registered);
    if !is_startup_path(&registered) || registered.eq_ignore_ascii_case(&path) {
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(key) };
        return None;
    }
    let exists = Path::new(&registered).is_file();
    if exists && !is_older_executable(&registered, &path) {
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(key) };
        return None;
    }
    let ok = write_string(key, VALUE_NAME, &format!("\"{path}\""));
    // SAFETY: key handle closed before returning.
    unsafe { RegCloseKey(key) };
    if ok { None } else { Some("StartupWriteError") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_commands_parse_quoted_and_unquoted_paths() {
        assert_eq!(
            parse_run_command(r#""C:\Program Files\OpenTaskbar\OpenTaskbar.exe" --flag"#),
            (
                Some(r"C:\Program Files\OpenTaskbar\OpenTaskbar.exe".to_owned()),
                "--flag".to_owned()
            )
        );
        assert_eq!(
            parse_run_command(r"C:\Program Files\OpenTaskbar\OpenTaskbar.exe --flag"),
            (
                Some(r"C:\Program Files\OpenTaskbar\OpenTaskbar.exe".to_owned()),
                "--flag".to_owned()
            )
        );
        assert_eq!(parse_run_command(""), (None, String::new()));
    }

    #[test]
    fn shortcut_targets_expand_environment_variables() {
        assert_eq!(
            expand_env_with(r"%LOCALAPPDATA%\OpenTaskbar\OpenTaskbar.exe", |name| {
                (name == "LOCALAPPDATA").then(|| r"C:\Users\Test\AppData\Local".to_owned())
            }),
            r"C:\Users\Test\AppData\Local\OpenTaskbar\OpenTaskbar.exe"
        );
        assert_eq!(
            expand_env_with("%MISSING%-kept", |_| None),
            "%MISSING%-kept"
        );
        assert_eq!(expand_env_with("100%%", |_| None), "100%%");
    }

    #[test]
    fn version_tuples_compare_component_wise() {
        assert!((0, 1, 2, 0) < (0, 1, 3, 0));
        assert!((1, 0, 0, 0) > (0, 9, 9, 9));
        assert!((1, 0, 0, 0) >= (1, 0, 0, 0));
    }

    #[test]
    fn sync_all_is_safe_to_call_repeatedly() {
        // The real registry path must be idempotent and never panic, even when
        // the current executable is the test runner (not a startup target).
        let _ = sync_all();
        let _ = sync_all();
    }

    #[test]
    fn startup_approved_flag_uses_low_bit() {
        assert!(is_startup_approved_enabled(None));
        assert!(is_startup_approved_enabled(Some(&[2, 0, 0, 0])));
        assert!(!is_startup_approved_enabled(Some(&[3, 0, 0, 0])));
        assert!(is_startup_path(r"C:\Apps\OpenTaskbar-1.exe"));
        assert!(!is_startup_path(r"C:\Apps\Other.exe"));
    }

    #[test]
    fn startup_paths_with_multibyte_stems_do_not_panic() {
        // `ğ` occupies bytes 10..12, so a naive `name[..11]` slice panics here.
        assert!(!is_startup_path(r"C:\Apps\OpenTaskbağ.exe"));
        assert!(!is_startup_path(r"C:\Apps\OpenTaskbarğ.exe"));
        assert!(!is_startup_path(r"C:\Apps\OpenTaskb.exe"));
        assert!(!is_startup_path("C:\\Apps\\OpenTaskba\u{0131}.exe"));
        assert!(is_startup_path(r"C:\Apps\OPENTASKBAR.exe"));
        assert!(is_startup_path(r"C:\Apps\OpenTaskbar-2.exe"));
        assert!(!is_startup_path(r"C:\Apps\OpenTaskbarX.exe"));
    }
}
