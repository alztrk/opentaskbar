//! Watches Explorer's per-user Taskband Favorites with
//! `RegNotifyChangeKeyValue` and reports packaged app pins being added or
//! removed (C4: `start_watch()`).
//!
//! The monitor keeps a baseline AUMID set, re-arms after every notification,
//! and never blocks callers: subscribers are invoked on the dedicated watch
//! thread. A missing or inaccessible Taskband key simply disables the watch.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_BINARY, RegCloseKey, RegNotifyChangeKeyValue,
    RegOpenKeyExW, RegQueryValueExW,
};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, WaitForMultipleObjects};

use opentaskbar_core::log;

const TASKBAND_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Taskband";
const FAVORITES_VALUE: &str = "Favorites";
const REG_NOTIFY_CHANGE_NAME: u32 = 0x1;
const REG_NOTIFY_CHANGE_ATTRIBUTES: u32 = 0x2;
const REG_NOTIFY_CHANGE_LAST_SET: u32 = 0x4;
const NOTIFY_FILTER: u32 =
    REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_ATTRIBUTES | REG_NOTIFY_CHANGE_LAST_SET;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinChangeKind {
    Pinned,
    Unpinned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinChange {
    pub kind: PinChangeKind,
    pub app_user_model_id: String,
}

type Listener = Box<dyn Fn(PinChange) + Send + 'static>;

static LISTENERS: Mutex<Vec<Listener>> = Mutex::new(Vec::new());
static RUNNING: AtomicBool = AtomicBool::new(false);
static CHANGE_COUNT: AtomicU64 = AtomicU64::new(0);
static WATCH_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static STOP_EVENT: Mutex<Option<usize>> = Mutex::new(None);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Raw Win32 handles are safe to move to the single watch thread; this wrapper
/// makes that explicit to the type system.
struct SendHandle(usize);
// SAFETY: each handle is exclusively owned by the watch thread after transfer.
unsafe impl Send for SendHandle {}

/// Registers a callback for pin changes. Multiple listeners are supported; a
/// throwing/panicking listener cannot take down the watch thread because each
/// invocation is isolated by `catch_unwind`.
///
/// Production subscription is self-contained: the first listener also arms the
/// registry watcher, so callers do not have to remember a separate
/// `start_watch()` call. `start_watch()` remains idempotent for existing wiring.
pub fn subscribe(listener: impl Fn(PinChange) + Send + 'static) {
    let first = match LISTENERS.lock() {
        Ok(mut listeners) => {
            let first = listeners.is_empty();
            listeners.push(Box::new(listener));
            first
        }
        Err(poisoned) => {
            let mut listeners = poisoned.into_inner();
            let first = listeners.is_empty();
            listeners.push(Box::new(listener));
            first
        }
    };
    if first && !RUNNING.load(Ordering::SeqCst) {
        let _ = start_watch();
    }
}

pub fn change_count() -> u64 {
    CHANGE_COUNT.load(Ordering::Relaxed)
}

fn notify(change: PinChange) {
    CHANGE_COUNT.fetch_add(1, Ordering::Relaxed);
    let listeners = LISTENERS.lock();
    let Ok(listeners) = listeners else {
        return;
    };
    for listener in listeners.iter() {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(change.clone())));
        if result.is_err() {
            log::warn("Taskbar pin listener failed.");
        }
    }
}

/// Reads and parses `HKCU\...\Taskband\Favorites`.
pub fn read_aumids() -> HashSet<String> {
    let subkey = wide(TASKBAND_KEY);
    let value = wide(FAVORITES_VALUE);
    // SAFETY: read-only registry access with null-terminated UTF-16 strings.
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return HashSet::new();
        }
        let mut size = 0u32;
        if RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) != 0
            || size < 4
        {
            RegCloseKey(key);
            return HashSet::new();
        }
        let mut buffer = vec![0u8; size as usize];
        let mut kind = 0u32;
        let status = RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            buffer.as_mut_ptr(),
            &mut size,
        );
        RegCloseKey(key);
        if status != 0 || kind != REG_BINARY {
            return HashSet::new();
        }
        extract_aumids(&buffer).into_iter().collect()
    }
}

/// Extracts packaged AUMIDs from the binary Taskband blob. PIDL structures are
/// not guaranteed to be 2-byte aligned, so both even and odd UTF-16LE offsets
/// are scanned (C# `ExtractAumidsFromTaskbandBytes` parity).
pub fn extract_aumids(bytes: &[u8]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for offset in 0..=1usize {
        let count = bytes.len().saturating_sub(offset);
        let count = count - count % 2;
        if count == 0 {
            continue;
        }
        let units: Vec<u16> = bytes[offset..offset + count]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let text: String = String::from_utf16_lossy(&units);
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            // Package AUMID: name_publisherId!appId.
            if is_name_start(chars[i]) {
                let name_start = i;
                while i < chars.len() && is_name_char(chars[i]) {
                    i += 1;
                }
                let name_end = i;
                if i < chars.len() && chars[i] == '_' {
                    let publisher_start = i + 1;
                    let mut p = publisher_start;
                    while p < chars.len() && chars[p].is_ascii_alphanumeric() {
                        p += 1;
                    }
                    if p - publisher_start == 13
                        && p < chars.len()
                        && chars[p] == '!'
                        && p + 1 < chars.len()
                        && is_name_start(chars[p + 1])
                    {
                        let app_start = p + 1;
                        let mut a = app_start;
                        while a < chars.len() && is_app_char(chars[a]) {
                            a += 1;
                        }
                        let candidate: String = chars[name_start..a].iter().collect();
                        if seen.insert(candidate.clone()) {
                            output.push(candidate);
                        }
                        i = a;
                        continue;
                    }
                }
                i = name_end.max(i);
                continue;
            }
            i += 1;
        }
        // File Explorer's AUMID is not packaged but is pinned by default.
        const EXPLORER: &str = "Microsoft.Windows.Explorer";
        let target: Vec<char> = EXPLORER.chars().collect();
        if chars.len() < target.len() {
            continue;
        }
        for start in 0..=chars.len() - target.len() {
            if chars[start..start + target.len()] != target[..] {
                continue;
            }
            let before = start
                .checked_sub(1)
                .and_then(|index| chars.get(index).copied());
            let after = chars.get(start + target.len()).copied();
            if before.is_none_or(|c| !is_name_char(c)) && after.is_none_or(|c| !is_name_char(c)) {
                let candidate = EXPLORER.to_owned();
                if seen.insert(candidate.clone()) {
                    output.push(candidate);
                }
            }
        }
    }
    output
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '-'
}

fn is_app_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'
}

fn open_watch_key() -> Option<(HKEY, HANDLE)> {
    let subkey = wide(TASKBAND_KEY);
    // SAFETY: read-only registry key handle and a Win32 event, both closed by
    // the watch thread on exit.
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return None;
        }
        let event = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
        if event.is_null() {
            RegCloseKey(key);
            return None;
        }
        Some((key, event))
    }
}

fn arm(key: HKEY, event: HANDLE) -> bool {
    // SAFETY: asynchronously signal `event` on Taskband subkey changes.
    let status = unsafe { RegNotifyChangeKeyValue(key, 1, NOTIFY_FILTER, event, 1) };
    status == 0
}

/// Starts the registry watcher. Idempotent; returns false if it could not open
/// the key or arm the notification (e.g. no Explorer Taskband on this machine).
pub fn start_watch() -> bool {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return true;
    }
    let Some((key, event)) = open_watch_key() else {
        RUNNING.store(false, Ordering::SeqCst);
        return false;
    };
    if !arm(key, event) {
        // SAFETY: owned handles from `open_watch_key`.
        unsafe {
            RegCloseKey(key);
            CloseHandle(event);
        }
        RUNNING.store(false, Ordering::SeqCst);
        return false;
    }

    let stop = match unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) } {
        handle if !handle.is_null() => handle,
        _ => {
            // SAFETY: owned handles from `open_watch_key`.
            unsafe {
                RegCloseKey(key);
                CloseHandle(event);
            }
            RUNNING.store(false, Ordering::SeqCst);
            return false;
        }
    };
    *STOP_EVENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop as usize);

    let key = SendHandle(key as usize);
    let event = SendHandle(event as usize);
    let stop_for_thread = SendHandle(stop as usize);
    let thread = std::thread::Builder::new()
        .name("opentaskbar-pin-watch".into())
        .spawn(move || {
            let key = key.0 as HKEY;
            let event = event.0 as HANDLE;
            let stop = stop_for_thread.0 as HANDLE;
            let mut baseline = read_aumids();
            let handles = [event, stop];
            loop {
                // SAFETY: both handles are owned by this thread and stay open
                // until the loop exits.
                let result = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
                if result == WAIT_OBJECT_0 + 1 {
                    break;
                }
                if result != WAIT_OBJECT_0 {
                    break;
                }
                let current = read_aumids();
                for aumid in current.difference(&baseline) {
                    notify(PinChange {
                        kind: PinChangeKind::Pinned,
                        app_user_model_id: aumid.clone(),
                    });
                }
                for aumid in baseline.difference(&current) {
                    notify(PinChange {
                        kind: PinChangeKind::Unpinned,
                        app_user_model_id: aumid.clone(),
                    });
                }
                baseline = current;
                // RegNotifyChangeKeyValue is one-shot: rearm after every pass.
                if !arm(key, event) {
                    break;
                }
            }
            // SAFETY: closing handles this thread exclusively owns.
            unsafe {
                RegCloseKey(key);
                CloseHandle(event);
                CloseHandle(stop);
            }
            RUNNING.store(false, Ordering::SeqCst);
        });
    match thread {
        Ok(thread) => {
            *WATCH_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread);
            true
        }
        Err(_) => {
            // SAFETY: closing handles before reporting failure.
            unsafe {
                RegCloseKey(key.0 as HKEY);
                CloseHandle(event.0 as HANDLE);
                CloseHandle(stop);
            }
            RUNNING.store(false, Ordering::SeqCst);
            false
        }
    }
}

pub fn stop_watch() {
    let stop = STOP_EVENT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .unwrap_or(0);
    if stop != 0 {
        // SAFETY: signalling the stop event wakes the watcher's wait.
        unsafe {
            windows_sys::Win32::System::Threading::SetEvent(stop as HANDLE);
        }
    }
    if let Some(thread) = WATCH_THREAD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        let _ = thread.join();
    }
    // The stop event is closed by the watcher itself when its wait wakes.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskband_blob_yields_packaged_aumids_at_both_alignments() {
        let aumid = "Example.Product_0123456789abc!Client";
        let mut blob = vec![0u8; 3];
        for unit in aumid.encode_utf16() {
            blob.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(extract_aumids(&blob).contains(&aumid.to_owned()));

        // Five leading zero bytes force the UTF-16 string to the odd byte offset.
        let mut odd = vec![0u8; 5];
        for unit in aumid.encode_utf16() {
            odd.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(extract_aumids(&odd).contains(&aumid.to_owned()));
    }

    #[test]
    fn subscribe_registers_production_listener() {
        let before = LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).len();
        subscribe(|_| {});
        let after = LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).len();
        assert_eq!(after, before + 1);
    }

    #[test]
    fn explorer_aumid_is_captured_with_word_boundaries() {
        let mut blob = Vec::new();
        for unit in " Microsoft.Windows.Explorer Shell".encode_utf16() {
            blob.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(extract_aumids(&blob).contains(&"Microsoft.Windows.Explorer".to_owned()));
    }
}
