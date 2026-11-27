//! Per-session single-instance activation for `--files <path>` (reference
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

use opentaskbar_core::settings::DockSettings;

use super::files::{self, COMPUTER_LOCATION};

// @pure-start
/// 128 KiB, identical to the `MaximumPathBytes`.
pub const MAX_PATH_BYTES: usize = 128 * 1024;
/// Canonical reference key (the Settings UI writes a JSON **bool**, not an int).
pub const KEY_FILES_ENABLED: &str = "FjordFilesEnabled";
/// `FjordBarVisible` is also a JSON bool written by the in-app Settings UI.
pub const KEY_BAR_VISIBLE: &str = "FjordBarVisible";
const LEGACY_SETTINGS_KEYS: [&str; 3] = [
    KEY_FILES_ENABLED,
    "fjord_files_enabled",
    "ShowFjordFilesButton",
];
const BAR_SETTINGS_KEYS: [&str; 2] = [KEY_BAR_VISIBLE, "fjord_bar_visible"];

/// Bool-aware reader for `settings.extra`: reference writes `true` / `false`, while the
/// legacy Rust settings UI wrote `1` / `0`. Both (and common string spellings)
/// must resolve to the same value so a saved bool is never mistaken for "off".
fn coerce_bool(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(value) => Some(*value),
        serde_json::Value::Number(number) => Some(number.as_i64().unwrap_or(0) != 0),
        serde_json::Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Reads the first present flag from `keys`; missing/invalid values use `default`.
pub fn settings_flag(settings: &DockSettings, keys: &[&str], default: bool) -> bool {
    keys.iter()
        .find_map(|key| settings.extra.get(*key).and_then(coerce_bool))
        .unwrap_or(default)
}

/// Whether the Fjord file manager feature is enabled for this dock process.
pub fn files_enabled(settings: &DockSettings) -> bool {
    settings_flag(settings, &LEGACY_SETTINGS_KEYS, false)
}

/// Whether the Fjord top bar should be shown (used by the `--fjord-bar` process).
pub fn bar_visible(settings: &DockSettings) -> bool {
    settings_flag(settings, &BAR_SETTINGS_KEYS, false)
}

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

/// Resolves a shortcut target without touching the filesystem (reference
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

/// Explorer argument quoting: drive roots and This PC stay unquoted (reference parity).
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

/// Sends one frame to a named pipe. `true` means it was delivered.
pub fn send_to_pipe(name: &str, path: &str) -> bool {
    #[cfg(windows)]
    {
        let Some(frame) = encode_frame(path) else {
            return false;
        };
        let name = wide(&format!(r"\\.\pipe\{name}"));
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
        let _ = (name, path);
        false
    }
}

/// Sends one frame to the session's dock pipe. `true` means it was delivered.
pub fn try_send(path: &str) -> bool {
    send_to_pipe(&pipe_name(current_session_id()), path)
}

/// Opens the real GPUI file manager on its own thread. GPUI owns the calling
/// thread's message loop, so the Fjord bar / dock UI thread must never call
/// `run_files_window` inline.
pub fn spawn_files_window(path: Option<String>) {
    let initial = path.filter(|value| !value.trim().is_empty());
    let _ = std::thread::Builder::new()
        .name("opentaskbar-fjord-files".into())
        .spawn(move || {
            // The file manager reads known folders through Shell COM.
            unsafe {
                use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
                let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
                crate::ui::fjord_files::run_files_window(initial);
                if initialized {
                    windows::Win32::System::Com::CoUninitialize();
                }
            }
        });
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

/// Server loop on the session pipe. `open` is invoked with `None` for a bare
/// "This PC" request. The cancel flag is only read, never latched: a transient
/// Explorer/pipe error must not permanently kill the listener (see [`PipeServer`]).
pub fn listen(open: impl Fn(Option<String>), cancel: Arc<AtomicBool>) -> io::Result<()> {
    listen_named(&pipe_name(current_session_id()), open, cancel)
}

/// Server loop on an explicitly named pipe (also used by the lifecycle tests).
pub fn listen_named(
    name: &str,
    open: impl Fn(Option<String>),
    cancel: Arc<AtomicBool>,
) -> io::Result<()> {
    #[cfg(windows)]
    {
        let name = wide(&format!(r"\\.\pipe\{name}"));
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
        let stop_cancel = Arc::new(AtomicBool::new(false));
        let stopper = {
            let stop_cancel = Arc::clone(&stop_cancel);
            let watch_cancel = Arc::clone(&cancel);
            std::thread::spawn(move || {
                while !stop_cancel.load(Ordering::SeqCst) && !watch_cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                // SAFETY: disconnecting unblocks a synchronous ConnectNamedPipe.
                unsafe {
                    let _ = DisconnectNamedPipe(handle);
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
        stop_cancel.store(true, Ordering::SeqCst);
        let _ = stopper.join();
        // SAFETY: the only handle to this pipe instance.
        unsafe {
            let _ = CloseHandle(handle);
        }
        result
    }
    #[cfg(not(windows))]
    {
        let _ = (name, open, cancel);
        Ok(())
    }
}

/// Owns the pipe listener thread and its cancel flag. A failed `listen` is
/// retried after `POLL_MS` instead of leaving the dock unreachable for the rest
/// of the session (a second instance or an Explorer restart can own the pipe
/// temporarily). `stop()` is idempotent and `Drop` joins the worker.
pub struct PipeServer {
    cancel: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl PipeServer {
    /// Starts listening on the session pipe, naming it `name` (usually
    /// [`pipe_name`] for the current session).
    pub fn start_named(name: String, open: impl Fn(Option<String>) + Send + 'static) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker = std::thread::Builder::new()
            .name("opentaskbar-fjord-pipe".into())
            .spawn(move || {
                while !worker_cancel.load(Ordering::SeqCst) {
                    match listen_named(&name, &open, Arc::clone(&worker_cancel)) {
                        Ok(()) => break,
                        Err(_) => {
                            if worker_cancel.load(Ordering::SeqCst) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(super::POLL_MS));
                        }
                    }
                }
            });
        Self {
            cancel,
            worker: worker.ok(),
        }
    }

    /// Starts listening on this session's pipe.
    pub fn start(open: impl Fn(Option<String>) + Send + 'static) -> Self {
        Self::start_named(pipe_name(current_session_id()), open)
    }

    pub fn is_running(&self) -> bool {
        !self.cancel.load(Ordering::SeqCst)
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
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

    #[test]
    fn dock_settings_reads_reference_bool_flags() {
        let mut settings = DockSettings::default();
        assert!(!files_enabled(&settings));
        assert!(!bar_visible(&settings));
        settings
            .extra
            .insert(KEY_FILES_ENABLED.into(), serde_json::json!(true));
        settings
            .extra
            .insert(KEY_BAR_VISIBLE.into(), serde_json::json!(false));
        assert!(files_enabled(&settings));
        assert!(!bar_visible(&settings));
        // Legacy ints still work, and bool wins over the legacy alias.
        settings
            .extra
            .insert(KEY_BAR_VISIBLE.into(), serde_json::json!(1));
        settings
            .extra
            .insert("fjord_bar_visible".into(), serde_json::json!(false));
        assert!(bar_visible(&settings));
        settings
            .extra
            .insert("fjord_files_enabled".into(), serde_json::json!(1));
        assert!(files_enabled(&settings));
        settings
            .extra
            .insert(KEY_FILES_ENABLED.into(), serde_json::json!(false));
        assert!(!files_enabled(&settings));
    }

    #[test]
    fn pipe_server_lifecycle_delivers_frames_and_stops_cleanly() {
        let name = format!("OpenTaskbar.FjordFiles.Test.{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let server = PipeServer::start_named(name.clone(), move |path| {
            let _ = tx.send(path);
        });
        assert!(server.is_running());

        #[cfg(windows)]
        {
            let mut delivered = false;
            for _ in 0..40 {
                if send_to_pipe(&name, "C:\\Users\\agnes\\Documents") {
                    delivered = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            assert!(delivered, "pipe server did not come up");
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(3)).unwrap().as_deref(),
                Some("C:\\Users\\agnes\\Documents")
            );
            assert!(send_to_pipe(&name, ""));
            assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), None);
        }

        server.stop();
        assert!(!server.is_running());
        // A second stop must be harmless (Drop stops again).
        server.stop();
    }

    #[test]
    fn main_cli_wires_bar_host_and_the_session_pipe_server() {
        let src = include_str!("../main.rs");
        for needle in [
            "ui::fjord_bar::BAR_ARG",
            "ui::fjord_bar::run_bar_host(",
            "fjord::activation::bar_visible(&settings)",
            "fjord::activation::PipeServer::start(",
        ] {
            assert!(src.contains(needle), "main must contain {needle:?}");
        }
    }

    #[test]
    fn dock_files_command_is_routed_through_the_pipe_with_explorer_fallback() {
        let src = include_str!("activation.rs");
        assert!(src.contains("pub fn try_send(path: &str) -> bool"));
        assert!(src.contains("open_explorer_fallback(Some(path))"));
        assert!(src.contains("pub fn spawn_files_window("));
        assert!(src.contains("crate::ui::fjord_files::run_files_window(initial)"));
        assert!(src.contains("pub struct PipeServer"));
    }
}
