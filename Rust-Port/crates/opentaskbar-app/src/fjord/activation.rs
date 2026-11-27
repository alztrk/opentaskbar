//! Per-session single-instance activation for `--files <path>` (C#
//! `FjordFilesActivation` + `FjordFilesAvailability`).
//!
//! A named pipe `OpenTaskbar.FjordFiles.<SessionId>` routes the request to the
//! dock in the same Windows session. When nobody listens, an explicit Explorer
//! fallback opens the folder without invoking a user-defined folder verb.
//! A 750 ms settings watcher closes file windows from other Fjord processes
//! when the feature is switched off.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};

use super::files::{self, COMPUTER_LOCATION};

// @pure-start
/// 128 KiB, identical to the C# `MaximumPathBytes`.
pub const MAX_PATH_BYTES: usize = 128 * 1024;
const LEGACY_SETTINGS_KEYS: [&str; 3] = [
    "FjordFilesEnabled",
    "fjord_files_enabled",
    "ShowFjordFilesButton",
];

/// Pipe base name for a session (the Win32 path is `\\.\pipe\<name>`).
pub fn pipe_name(session_id: u32) -> String {
    format!("OpenTaskbar.FjordFiles.{session_id}")
}

/// Frame: little-endian UTF-8 byte count followed by the payload.
pub fn encode_frame(path: &str) -> Option<Vec<u8>> {
    let payload = path.as_bytes();
    if payload.len() > MAX_PATH_BYTES {
        return None;
    }
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    Some(frame)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    TooShort,
    InvalidLength,
    TooLarge,
    InvalidUtf8,
}

/// Decodes one server-side frame. An empty payload is the "open This PC" request.
pub fn decode_frame(frame: &[u8]) -> Result<Option<String>, FrameError> {
    if frame.len() < 4 {
        return Err(FrameError::TooShort);
    }
    let length = u32::from_le_bytes(frame[..4].try_into().expect("four bytes")) as usize;
    if length > MAX_PATH_BYTES {
        return Err(FrameError::TooLarge);
    }
    if frame.len() < 4 + length {
        return Err(FrameError::InvalidLength);
    }
    if length == 0 {
        return Ok(None);
    }
    String::from_utf8(frame[4..4 + length].to_vec())
        .map(Some)
        .map_err(|_| FrameError::InvalidUtf8)
}

/// Resolves a shortcut target without touching the filesystem (C#
/// `GetExplorerFallbackLocation` before its `Directory.Exists` check).
pub fn resolve_fallback_location(path: Option<&str>) -> Option<String> {
    let input = path?.trim().trim_matches('"');
    if input.is_empty() {
        return None;
    }
    if files::is_computer_location(input) {
        return Some(COMPUTER_LOCATION.to_owned());
    }
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\Users".to_owned());
    let resolved = files::navigation_target(input, None, &home)?;
    if resolved.contains('"') {
        return None;
    }
    // A bare command/relative token is not a folder shortcut.
    Path::new(&resolved).is_absolute().then_some(resolved)
}

/// Explorer argument quoting: drive roots and This PC stay unquoted (C# parity).
pub fn format_explorer_argument(location: &str) -> String {
    let is_root = location.len() == 3
        && location.as_bytes().get(1) == Some(&b':')
        && location
            .as_bytes()
            .get(2)
            .is_some_and(|b| *b == b'\\' || *b == b'/');
    if location == COMPUTER_LOCATION || is_root {
        location.to_owned()
    } else {
        format!("\"{}\"", location.trim_end_matches(['\\', '/']))
    }
}

/// Reads the feature flag from a saved settings JSON document.
pub fn enabled_from_json(raw: &str) -> Option<bool> {
    for key in LEGACY_SETTINGS_KEYS {
        let needle = format!("\"{key}\"");
        let mut rest = raw;
        while let Some(index) = rest.find(&needle) {
            let tail = &rest[index + needle.len()..];
            if let Some(colon) = tail.find(':') {
                let value = tail[colon + 1..].trim_start();
                if value.starts_with("true") {
                    return Some(true);
                }
                if value.starts_with("false") {
                    return Some(false);
                }
            }
            rest = tail;
        }
    }
    None
}

/// Returns true only when the watcher must fire the disable/enable callback.
pub fn enabled_changed(previous: bool, next: bool) -> bool {
    previous != next
}
// @pure-end

/// Final fallback check: only real directories are handed to Explorer.
pub fn explorer_fallback_location(path: Option<&str>) -> Option<String> {
    let location = resolve_fallback_location(path)?;
    if location == COMPUTER_LOCATION || Path::new(&location).is_dir() {
        Some(location)
    } else {
        None
    }
}

/// Opens a folder shortcut in Explorer when no Fjord process is listening.
pub fn open_explorer_fallback(path: Option<&str>) -> bool {
    let Some(location) = explorer_fallback_location(path) else {
        return false;
    };
    files::open_file("explorer.exe", Some(&format_explorer_argument(&location)))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn current_session_id() -> u32 {
    #[cfg(windows)]
    {
        // SAFETY: output pointer points at a live local; both APIs are pure reads.
        unsafe {
            let pid = GetCurrentProcessId();
            let mut session = 0u32;
            if ProcessIdToSessionId(pid, &mut session) != 0 {
                return session;
            }
        }
    }
    0
}

/// Sends one frame to the running dock. `true` means it was delivered.
pub fn try_send(path: &str) -> bool {
    #[cfg(windows)]
    {
        let Some(frame) = encode_frame(path) else {
            return false;
        };
        let name = wide(&format!(r"\\.\pipe\{}", pipe_name(current_session_id())));
        // SAFETY: all handles are closed below and buffers outlive the calls.
        unsafe {
            let handle = CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                0,
            );
            if handle == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut server_pid = 0u32;
            if GetNamedPipeServerProcessId(handle, &mut server_pid) != 0 && server_pid != 0 {
                // ASFW_ANY is u32::MAX; let Windows pick the foreground process.
                let _ = AllowSetForegroundWindow(server_pid);
            }
            let mut written = 0u32;
            let header_ok = WriteFile(
                handle as *mut core::ffi::c_void,
                frame.as_ptr().cast(),
                4,
                &mut written,
                std::ptr::null_mut(),
            ) != 0
                && written == 4;
            written = 0;
            let payload_ok = header_ok
                && WriteFile(
                    handle as *mut core::ffi::c_void,
                    frame[4..].as_ptr().cast(),
                    (frame.len() - 4) as u32,
                    &mut written,
                    std::ptr::null_mut(),
                ) != 0
                && written as usize == frame.len() - 4;
            let _ = CloseHandle(handle);
            payload_ok
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// CLI entry point called from `main.rs` for `--files <path>`.
/// Returns `true` when a running dock took the request.
pub fn handle_cli(path: &str) -> bool {
    if try_send(path) {
        return true;
    }
    // The dock may be disabled or shutting down: keep the shortcut useful.
    open_explorer_fallback(Some(path));
    false
}

/// Parses `--files <path>` and dispatches it. `None` means "not our argument".
pub fn handle_cli_arg(args: &[String]) -> Option<bool> {
    let index = args.iter().position(|arg| arg == super::FILES_CLI_ARG)?;
    let path = args.get(index + 1).map(String::as_str).unwrap_or("");
    Some(handle_cli(path))
}

/// Server loop. `open` is invoked with `None` for a bare "This PC" request.
pub fn listen(
    open: impl Fn(Option<String>) + Send + 'static,
    cancel: Arc<AtomicBool>,
) -> io::Result<()> {
    #[cfg(windows)]
    {
        let name = wide(&format!(r"\\.\pipe\{}", pipe_name(current_session_id())));
        // SAFETY: one server instance, correctly sized name; handle closed on exit.
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                MAX_PATH_BYTES as u32,
                MAX_PATH_BYTES as u32,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let shared = Arc::new(std::sync::Mutex::new(handle));
        let stopper = {
            let shared = Arc::clone(&shared);
            let cancel = Arc::clone(&cancel);
            std::thread::spawn(move || {
                while !cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                let value = *shared.lock().expect("pipe handle mutex poisoned");
                // SAFETY: disconnecting unblocks a synchronous ConnectNamedPipe.
                unsafe {
                    let _ = DisconnectNamedPipe(value);
                }
            })
        };

        let result = (|| -> io::Result<()> {
            while !cancel.load(Ordering::SeqCst) {
                // SAFETY: synchronous byte pipe, one connection at a time.
                let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
                if connected == 0 {
                    let error = io::Error::last_os_error();
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    // ERROR_NO_DATA means a previous client disconnected.
                    if error.raw_os_error() != Some(232) {
                        return Err(error);
                    }
                }
                let mut header = [0u8; 4];
                let mut read = 0u32;
                // SAFETY: buffers are live and sized for the requested byte count.
                let header_ok = unsafe {
                    ReadFile(
                        handle as *mut core::ffi::c_void,
                        header.as_mut_ptr().cast(),
                        4,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                } != 0
                    && read == 4;
                if header_ok {
                    let length = u32::from_le_bytes(header) as usize;
                    if length <= MAX_PATH_BYTES {
                        let mut payload = vec![0u8; length];
                        let mut total = 0u32;
                        while (total as usize) < length {
                            let mut chunk = 0u32;
                            let ok = unsafe {
                                ReadFile(
                                    handle as *mut core::ffi::c_void,
                                    payload.as_mut_ptr().add(total as usize).cast(),
                                    (length - total as usize) as u32,
                                    &mut chunk,
                                    std::ptr::null_mut(),
                                )
                            } != 0;
                            if !ok || chunk == 0 {
                                break;
                            }
                            total += chunk;
                        }
                        if total as usize == length
                            && let Ok(path) =
                                decode_frame(&[header.as_slice(), &payload[..]].concat())
                        {
                            open(path);
                        }
                    }
                }
                // SAFETY: disconnecting a connected pipe instance is idempotent here.
                unsafe {
                    let _ = DisconnectNamedPipe(handle);
                }
            }
            Ok(())
        })();
        cancel.store(true, Ordering::SeqCst);
        let _ = stopper.join();
        // SAFETY: the only handle to this pipe instance.
        unsafe {
            let _ = CloseHandle(handle);
        }
        result
    }
    #[cfg(not(windows))]
    {
        let _ = (open, cancel);
        Ok(())
    }
}

/// Polls the settings file every 750 ms and reports enable/disable transitions.
pub struct FilesAvailability {
    path: std::path::PathBuf,
    enabled: Arc<AtomicBool>,
    last_write: std::sync::Mutex<Option<SystemTime>>,
    cancel: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl FilesAvailability {
    pub fn new(path: impl Into<std::path::PathBuf>, enabled: bool) -> Self {
        Self {
            path: path.into(),
            enabled: Arc::new(AtomicBool::new(enabled)),
            last_write: std::sync::Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// One watcher tick. Returns true after a real transition.
    pub fn refresh(&self) -> bool {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return false;
        };
        let Some(next) = enabled_from_json(&raw) else {
            return false;
        };
        let previous = self.enabled.swap(next, Ordering::SeqCst);
        enabled_changed(previous, next)
    }

    /// Spawns the 750 ms watcher. The callback runs on the watcher thread.
    pub fn start(self, on_change: impl Fn(bool) + Send + 'static) -> Arc<Self> {
        let path = self.path.clone();
        let enabled = Arc::clone(&self.enabled);
        let cancel = Arc::clone(&self.cancel);
        let worker = std::thread::spawn(move || {
            let mut previous_write = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok();
            while !cancel.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(super::POLL_MS));
                let write = std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .ok();
                if write.is_some() && write == previous_write {
                    continue;
                }
                previous_write = write;
                let Ok(raw) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if let Some(next) = enabled_from_json(&raw) {
                    let previous = enabled.swap(next, Ordering::SeqCst);
                    if enabled_changed(previous, next) {
                        on_change(next);
                    }
                }
            }
        });
        let mut this = self;
        *this.last_write.lock().expect("last-write mutex poisoned") = this
            .path
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok());
        this.worker = Some(worker);
        Arc::new(this)
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for FilesAvailability {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Posts WM_CLOSE to every Fjord Files window (used when the feature is disabled).
pub fn close_all_windows() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::LPARAM;
        use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, PostMessageW, WM_CLOSE};
        unsafe extern "system" fn each(
            hwnd: windows_sys::Win32::Foundation::HWND,
            _: LPARAM,
        ) -> i32 {
            // SAFETY: `title` is a stack buffer used only inside this callback.
            unsafe {
                let mut title = [0u16; 512];
                let length = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
                if length > 0 {
                    let text = String::from_utf16_lossy(&title[..length as usize]);
                    if text.starts_with("Fjord Files") || text.contains("OpenTaskbar.FjordFiles") {
                        let _ = PostMessageW(hwnd, WM_CLOSE, 0, 0);
                    }
                }
            }
            1
        }
        // SAFETY: stateless enumeration callback.
        unsafe { EnumWindows(Some(each), 0) };
    }
    #[cfg(not(windows))]
    {}
}

// Handle signatures deliberately match `platform::wireless_battery`'s local
// declarations so rustc's `clashing_extern_declarations` lint stays quiet.
#[cfg(windows)]
type RawHandle = isize;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        attributes: *const core::ffi::c_void,
    ) -> RawHandle;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        attributes: *const core::ffi::c_void,
        disposition: u32,
        flags: u32,
        template: RawHandle,
    ) -> RawHandle;
    fn ConnectNamedPipe(handle: RawHandle, overlapped: *mut core::ffi::c_void) -> i32;
    fn DisconnectNamedPipe(handle: RawHandle) -> i32;
    fn GetNamedPipeServerProcessId(handle: RawHandle, pid: *mut u32) -> i32;
    fn CloseHandle(handle: RawHandle) -> i32;
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn AllowSetForegroundWindow(pid: u32) -> i32;
    #[link_name = "GetWindowTextW"]
    fn GetWindowTextW(hwnd: windows_sys::Win32::Foundation::HWND, text: *mut u16, max: i32) -> i32;
}

#[cfg(windows)]
const GENERIC_WRITE: u32 = 0x4000_0000;
#[cfg(windows)]
const OPEN_EXISTING: u32 = 3;
#[cfg(windows)]
const PIPE_ACCESS_INBOUND: u32 = 0x0001;
#[cfg(windows)]
const PIPE_TYPE_BYTE: u32 = 0;
#[cfg(windows)]
const PIPE_READMODE_BYTE: u32 = 0;
#[cfg(windows)]
const PIPE_WAIT: u32 = 0;
#[cfg(windows)]
const INVALID_HANDLE_VALUE: RawHandle = -1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_is_session_scoped() {
        assert_eq!(pipe_name(7), "OpenTaskbar.FjordFiles.7");
        assert_eq!(pipe_name(0), "OpenTaskbar.FjordFiles.0");
    }

    #[test]
    fn frames_round_trip_unicode_paths_and_empty_request() {
        let frame = encode_frame("C:\\Users\\agnes\\Belgeler\\résumé.txt").unwrap();
        assert_eq!(
            decode_frame(&frame).unwrap().as_deref(),
            Some("C:\\Users\\agnes\\Belgeler\\résumé.txt")
        );
        assert_eq!(decode_frame(&encode_frame("").unwrap()).unwrap(), None);
    }

    #[test]
    fn frames_reject_truncated_and_oversized_payloads() {
        assert_eq!(decode_frame(&[1, 0, 0]), Err(FrameError::TooShort));
        assert_eq!(
            decode_frame(&[9, 0, 0, 0, b'a']),
            Err(FrameError::InvalidLength)
        );
        assert_eq!(
            decode_frame(&[0x01, 0x00, 0x04, 0x00]),
            Err(FrameError::TooLarge)
        );
        let mut huge = (MAX_PATH_BYTES as u32 + 1).to_le_bytes().to_vec();
        huge.extend_from_slice(b"a");
        assert_eq!(decode_frame(&huge), Err(FrameError::TooLarge));
        assert!(encode_frame(&"x".repeat(MAX_PATH_BYTES + 1)).is_none());
    }

    #[test]
    fn explorer_fallback_resolves_quoted_folders_and_this_pc() {
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let folder = format!("{home}\\Documents");
        assert_eq!(
            resolve_fallback_location(Some(&format!("\"{folder}\""))).as_deref(),
            Some(folder.as_str())
        );
        assert_eq!(
            resolve_fallback_location(Some("shell:MyComputerFolder")).as_deref(),
            Some(COMPUTER_LOCATION)
        );
        assert_eq!(
            resolve_fallback_location(Some("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}")).as_deref(),
            Some(COMPUTER_LOCATION)
        );
        assert_eq!(resolve_fallback_location(Some("   ")), None);
        assert_eq!(resolve_fallback_location(None), None);
    }

    #[test]
    fn explorer_argument_avoids_quotes_for_roots() {
        assert_eq!(
            format_explorer_argument(COMPUTER_LOCATION),
            COMPUTER_LOCATION
        );
        assert_eq!(format_explorer_argument("C:\\"), "C:\\");
        assert_eq!(
            format_explorer_argument("C:\\Users\\agnes\\"),
            "\"C:\\Users\\agnes\""
        );
    }

    #[test]
    fn settings_flag_is_read_from_legacy_and_canonical_keys() {
        assert_eq!(
            enabled_from_json(r#"{"FjordFilesEnabled":true}"#),
            Some(true)
        );
        assert_eq!(
            enabled_from_json(r#"{"fjord_files_enabled":false}"#),
            Some(false)
        );
        // Canonical `FjordFilesEnabled` wins when both keys are saved.
        assert_eq!(
            enabled_from_json(r#"{"ShowFjordFilesButton":true,"FjordFilesEnabled":false}"#),
            Some(false)
        );
        assert_eq!(enabled_from_json(r#"{"other":1}"#), None);
        assert!(enabled_changed(true, false));
        assert!(!enabled_changed(false, false));
    }
}
