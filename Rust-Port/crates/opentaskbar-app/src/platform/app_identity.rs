//! Detects whether OpenTaskbar runs inside a packaged MSIX (Microsoft Store)
//! container so Store-specific policies activate automatically while portable
//! GitHub builds keep their standalone behaviour.
//!
//! C4 consumers call `is_packaged()` and `store_update_uri()` directly.

use std::sync::OnceLock;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentPackageFullName(length: *mut u32, full_name: *mut u16) -> i32;
}

pub const STARTUP_TASK_ID: &str = "OpenTaskbarStartup";
pub const STORE_UPDATES_URI: &str = "ms-windows-store://downloadsandupdates";

const ERROR_SUCCESS: i32 = 0;
const ERROR_INSUFFICIENT_BUFFER: i32 = 122;

static PACKAGED: OnceLock<bool> = OnceLock::new();

fn detect_packaged() -> bool {
    // SAFETY: length query with a null output buffer; kernel32 writes the
    // required length only and never dereferences the null pointer.
    let mut length = 0u32;
    let result = unsafe { GetCurrentPackageFullName(&mut length, std::ptr::null_mut()) };
    result == ERROR_SUCCESS || result == ERROR_INSUFFICIENT_BUFFER
}

/// True for an MSIX / Store build, false for portable GitHub builds.
pub fn is_packaged() -> bool {
    *PACKAGED.get_or_init(detect_packaged)
}

/// Full package family name when packaged (best-effort).
pub fn package_full_name() -> Option<String> {
    if !is_packaged() {
        return None;
    }
    // SAFETY: the first call reported the required UTF-16 length; the second
    // writes at most `length` code units into our owned buffer.
    unsafe {
        let mut length = 0u32;
        let _ = GetCurrentPackageFullName(&mut length, std::ptr::null_mut());
        if length == 0 || length > 4096 {
            return None;
        }
        let mut buffer = vec![0u16; length as usize];
        let status = GetCurrentPackageFullName(&mut length, buffer.as_mut_ptr());
        if status != ERROR_SUCCESS {
            return None;
        }
        let used = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..used]))
    }
}

pub fn store_update_uri() -> String {
    STORE_UPDATES_URI.to_owned()
}

pub fn startup_task_id() -> &'static str {
    STARTUP_TASK_ID
}

/// Where startup registration must be written. Packaged builds use the MSIX
/// `StartupTask` declared in the manifest; portable builds keep the `Run` key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupPolicy {
    MsixStartupTask,
    RunKey,
}

pub fn startup_policy() -> StartupPolicy {
    if is_packaged() {
        StartupPolicy::MsixStartupTask
    } else {
        StartupPolicy::RunKey
    }
}

/// Store builds receive updates through the Store; portable builds keep the
/// in-app update checker.
pub fn supports_in_app_updates() -> bool {
    !is_packaged()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_the_manifest_contract() {
        assert_eq!(startup_task_id(), "OpenTaskbarStartup");
        assert_eq!(store_update_uri(), STORE_UPDATES_URI);
        assert_eq!(supports_in_app_updates(), !is_packaged());
    }
}
