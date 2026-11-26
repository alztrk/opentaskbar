//! Startup registration for both packaging modes.
//!
//! Portable builds use `HKCU\...\Run\OpenTaskbar`, honour Task Manager's
//! `StartupApproved\Run` disable flag, and repair stale shortcuts in the
//! Startup folder. Packaged (MSIX) builds must use the manifest `StartupTask`
//! instead; we never write a Run key for them and remove stale ones on startup.

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

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

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
            .chunks_exact(2)
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
    const BASE: &str = "OpenTaskbar";
    name.len() >= BASE.len()
        && name[..BASE.len()].eq_ignore_ascii_case(BASE)
        && (name.len() == BASE.len() || name.as_bytes()[BASE.len()] == b'-')
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
        StartupPolicy::MsixStartupTask => {
            // The WinRT `StartupTask` state is owned by the packaged manifest;
            // without the WinRT bridge linked we never claim it is enabled.
            (false, Some("StartupPackagedRequiresStartupTask"))
        }
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

/// Enables or disables startup. Returns an error key on failure (C# parity).
pub fn set_enabled(enabled: bool) -> Option<&'static str> {
    match app_identity::startup_policy() {
        StartupPolicy::MsixStartupTask => {
            if enabled {
                log::warn("MSIX builds must enable the manifest StartupTask via WinRT.");
                Some("StartupPackagedRequiresStartupTask")
            } else {
                remove_stale_unpackaged_run_entry();
                None
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
    let Some(current) = try_normalize_path(current_executable) else {
        return 0;
    };
    // SAFETY: COM STA for IShellLinkW on this thread.
    let initialized = unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    };
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
            let target = String::from_utf16_lossy(&buffer[..used]);
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
    if initialized.is_ok() {
        // SAFETY: balances the successful initialization above.
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
    updated
}

/// Called on every normal startup (dev/inspection modes are skipped).
pub fn sync_on_startup(is_dev: bool, is_inspection: bool) {
    if is_dev || is_inspection {
        return;
    }
    if matches!(
        app_identity::startup_policy(),
        StartupPolicy::MsixStartupTask
    ) {
        remove_stale_unpackaged_run_entry();
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let path = exe.to_string_lossy();
    if !is_valid_startup_executable(&path, Some(&std::env::temp_dir().to_string_lossy())) {
        return;
    }
    let Some(normalized) = try_normalize_path(&path) else {
        return;
    };
    if normalized.len() + 2 > MAX_COMMAND_LENGTH {
        return;
    }
    sync_unpackaged_run_entry(&normalized);
    if let Some(folder) = startup_folder()
        && folder.is_dir()
    {
        sync_startup_folder_shortcuts(&folder, &normalized);
    }
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
    if !is_startup_path(&registered) || registered.eq_ignore_ascii_case(&path) {
        // SAFETY: key handle closed before returning.
        unsafe { RegCloseKey(key) };
        return None;
    }
    if Path::new(&registered).exists() {
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
    fn startup_approved_flag_uses_low_bit() {
        assert!(is_startup_approved_enabled(None));
        assert!(is_startup_approved_enabled(Some(&[2, 0, 0, 0])));
        assert!(!is_startup_approved_enabled(Some(&[3, 0, 0, 0])));
        assert!(is_startup_path(r"C:\Apps\OpenTaskbar-1.exe"));
        assert!(!is_startup_path(r"C:\Apps\Other.exe"));
    }
}
