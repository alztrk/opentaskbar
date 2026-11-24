//! Launches the native 26 KB C `OpenTaskbarWatchdog.exe` (`tools/watchdog/watchdog.c`)
//! as a separate process (`OpenTaskbarWatchdog.exe --watchdog=<pid>:<token>`).
//!
//! Because it has its own process name, force-killing `OpenTaskBar.exe` never kills
//! the watchdog, so Windows' native taskbar is always restored reliably.

use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

const WATCHDOG_BYTES: &[u8] = include_bytes!("../../../../tools/watchdog/OpenTaskbarWatchdog.exe");
const EVENT_PREFIX: &str = r"Local\OpenTaskbar.CleanExit.";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

struct OwnedHandle(HANDLE);

// SAFETY: Win32 event handles can be signalled and closed from any thread.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: owned handle closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

static CLEAN_EXIT: Mutex<Option<OwnedHandle>> = Mutex::new(None);

fn create_clean_event(pid: u32, token: &str) -> Option<OwnedHandle> {
    let name: Vec<u16> = format!("{EVENT_PREFIX}{pid}.{token}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: manual-reset, initially unsignalled named event matching `watchdog.c`.
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) };
    (!handle.is_null()).then_some(OwnedHandle(handle))
}

fn ensure_watchdog_exe() -> Option<PathBuf> {
    let dir = opentaskbar_core::settings::app_data_dir();
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("OpenTaskbarWatchdog.exe");
    let up_to_date = fs::metadata(&path)
        .ok()
        .is_some_and(|m| m.len() == WATCHDOG_BYTES.len() as u64);
    if !up_to_date {
        let tmp = path.with_extension("exe.tmp");
        if fs::write(&tmp, WATCHDOG_BYTES).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
    path.exists().then_some(path)
}

pub fn arm() {
    let Ok(mut slot) = CLEAN_EXIT.lock() else {
        return;
    };
    if slot.is_some() {
        return;
    }
    let Some(exe) = ensure_watchdog_exe() else {
        opentaskbar_core::log::warn("Failed to materialize native OpenTaskbarWatchdog.exe.");
        return;
    };
    let pid = std::process::id();
    let token = format!("{:016x}", RandomState::new().hash_one(pid));
    let Some(event) = create_clean_event(pid, &token) else {
        return;
    };
    let arg = format!("--watchdog={pid}:{token}");
    let spawned = Command::new(&exe)
        .arg(&arg)
        .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .or_else(|_| {
            Command::new(&exe)
                .arg(&arg)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
        });
    match spawned {
        Ok(_) => *slot = Some(event),
        Err(err) => opentaskbar_core::log::warn(format!("Failed to start C watchdog: {err}")),
    }
}

pub fn notify_clean_exit() {
    if let Ok(mut slot) = CLEAN_EXIT.lock()
        && let Some(event) = slot.take()
    {
        // SAFETY: valid event handle.
        unsafe {
            SetEvent(event.0);
        }
    }
}
