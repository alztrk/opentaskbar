//! Detects whether OpenTaskbar runs inside a packaged MSIX (Microsoft Store)
//! container so Store-specific policies activate automatically while portable
//! GitHub builds keep their standalone behaviour.
//!
//! C4 consumers call `is_packaged()` and `store_update_uri()` directly.
//!
//! WS-4: packaged builds read and write the manifest `StartupTask` through the
//! real WinRT `Windows.ApplicationModel.StartupTask` API (`GetAsync`,
//! `RequestEnableAsync`, `Disable`) instead of always reporting an error.

use std::sync::OnceLock;

use windows::ApplicationModel::{StartupTask, StartupTaskState};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::core::HSTRING;

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

// ---------------------------------------------------------------------------
// MSIX StartupTask (real WinRT read/write)
// ---------------------------------------------------------------------------

/// `StartupTaskState` values that count as "registration enabled" (reference parity).
pub fn is_enabled_startup_state(state: i32) -> bool {
    state == StartupTaskState::Enabled.0 || state == StartupTaskState::EnabledByPolicy.0
}

/// Runs a WinRT call with a balanced COM apartment on this thread.
fn with_com<T>(work: impl FnOnce() -> Option<T>) -> Option<T> {
    // SAFETY: no out-pointers; RPC_E_CHANGED_MODE simply means an apartment is
    // already active and must not be uninitialized by us.
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
    let result = work();
    if initialized {
        // SAFETY: paired with the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    result
}

fn current_task() -> windows::core::Result<StartupTask> {
    // `IAsyncOperation<T>::get()` waits for the WinRT operation to complete;
    // this mirrors the reference `.AsTask().GetAwaiter().GetResult()`.
    StartupTask::GetAsync(&HSTRING::from(STARTUP_TASK_ID))?.get()
}

/// Reads the actual packaged startup registration.
///
/// `Ok(true)` = enabled (or enabled by policy); `Err("StartupReadError")` for
/// portable builds and WinRT failures. Never mutates state.
pub fn read_startup_task() -> Result<bool, &'static str> {
    if !is_packaged() {
        return Err("StartupReadError");
    }
    let state = with_com(|| current_task().ok())
        .and_then(|task| task.State().ok())
        .ok_or("StartupReadError")?;
    Ok(is_enabled_startup_state(state.0))
}

/// Writes the actual packaged startup registration: `RequestEnableAsync` when
/// enabling, `Disable` when disabling. Returns the observed resulting state.
///
/// `Err("StartupWriteError")` on portable builds or WinRT failures; policy-
/// disabled tasks report `Ok(false)` without ever throwing.
pub fn set_startup_task_enabled(enabled: bool) -> Result<bool, &'static str> {
    if !is_packaged() {
        return Err("StartupWriteError");
    }
    let state = with_com(|| {
        let task = current_task().ok()?;
        if enabled {
            Some(task.RequestEnableAsync().ok()?.get().ok()?.0)
        } else {
            if task.Disable().is_err() {
                return None;
            }
            Some(task.State().ok()?.0)
        }
    })
    .ok_or("StartupWriteError")?;
    Ok(is_enabled_startup_state(state))
}

/// Bool convenience wrapper for callers that only need success/failure.
pub fn startup_task_enabled() -> bool {
    read_startup_task().unwrap_or(false)
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

    #[test]
    fn startup_task_enabled_states_match_winrt_enum() {
        assert!(!is_enabled_startup_state(StartupTaskState::Disabled.0));
        assert!(!is_enabled_startup_state(
            StartupTaskState::DisabledByUser.0
        ));
        assert!(is_enabled_startup_state(StartupTaskState::Enabled.0));
        assert!(!is_enabled_startup_state(
            StartupTaskState::DisabledByPolicy.0
        ));
        assert!(is_enabled_startup_state(
            StartupTaskState::EnabledByPolicy.0
        ));
    }

    #[test]
    fn portable_build_reports_startup_task_error_without_writing() {
        if !is_packaged() {
            assert_eq!(read_startup_task(), Err("StartupReadError"));
            assert_eq!(set_startup_task_enabled(true), Err("StartupWriteError"));
            assert_eq!(startup_policy(), StartupPolicy::RunKey);
        }
    }
}
