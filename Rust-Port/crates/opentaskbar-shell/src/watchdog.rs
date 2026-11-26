//! Launches the native 26 KB C `OpenTaskbarWatchdog.exe` (`tools/watchdog/watchdog.c`)
//! as a separate process (`OpenTaskbarWatchdog.exe --watchdog=<pid>:<token>`).
//!
//! Because it has its own process name, force-killing `OpenTaskBar.exe` never kills
//! the watchdog, so Windows' native taskbar is always restored reliably.
//!
//! The C watchdog waits on three handles: a clean-exit event, the parent process
//! handle, and a heartbeat event. `OpenTaskbar.Heartbeat.<pid>.<token>` must be
//! signalled while the dock is alive; after three 4 s timeouts with no pulse the
//! watchdog restores Explorer's taskbar on its own. This module owns that pulse
//! thread and stops it on a clean exit so no stale signal can ever keep a dead
//! dock "alive".

use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

use opentaskbar_core::log;

const WATCHDOG_BYTES: &[u8] = include_bytes!("../../../../tools/watchdog/OpenTaskbarWatchdog.exe");
const EVENT_PREFIX: &str = r"Local\OpenTaskbar.CleanExit.";
const HEARTBEAT_PREFIX: &str = r"Local\OpenTaskbar.Heartbeat.";
/// Well below the C watchdog's 4 s timeout / 3-miss budget.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1000);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// A named-event handle that can be shared with the heartbeat thread and is
/// closed exactly once when the last clone goes away.
#[derive(Clone)]
struct EventHandle {
    raw: HANDLE,
    _owner: Arc<EventOwner>,
}

struct EventOwner(HANDLE);

// SAFETY: Win32 event handles can be signalled and closed from any thread.
unsafe impl Send for EventHandle {}
unsafe impl Sync for EventHandle {}
// SAFETY: the raw handle is immutable and only closed by `EventOwner::drop`.
unsafe impl Send for EventOwner {}
unsafe impl Sync for EventOwner {}

impl EventOwner {
    fn new(raw: HANDLE) -> Self {
        Self(raw)
    }
}

impl Drop for EventOwner {
    fn drop(&mut self) {
        // SAFETY: owned handle closed once, when the last clone is dropped.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

impl EventHandle {
    fn signal(&self) -> bool {
        // SAFETY: valid event handle owned for the lifetime of this value.
        unsafe { SetEvent(self.raw) != 0 }
    }
}

struct WatchdogState {
    clean_exit: EventHandle,
    heartbeat: EventHandle,
    stop: Arc<AtomicBool>,
    pulse_thread: Option<JoinHandle<()>>,
}

static WATCHDOG: Mutex<Option<WatchdogState>> = Mutex::new(None);

fn create_event(name: &str) -> Option<EventHandle> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: manual-reset, initially unsignalled named event matching `watchdog.c`.
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
    (!handle.is_null()).then(|| EventHandle {
        raw: handle,
        _owner: Arc::new(EventOwner::new(handle)),
    })
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

/// Arms the watchdog and starts the heartbeat pulse. Idempotent.
pub fn arm() {
    let Ok(mut slot) = WATCHDOG.lock() else {
        return;
    };
    if slot.is_some() {
        return;
    }
    let Some(exe) = ensure_watchdog_exe() else {
        log::warn("Failed to materialize native OpenTaskbarWatchdog.exe.");
        return;
    };
    let pid = std::process::id();
    let token = format!("{:016x}", RandomState::new().hash_one(pid));
    let Some(clean_exit) = create_event(&format!("{EVENT_PREFIX}{pid}.{token}")) else {
        return;
    };
    let Some(heartbeat) = create_event(&format!("{HEARTBEAT_PREFIX}{pid}.{token}")) else {
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
        Ok(_) => {
            let stop = Arc::new(AtomicBool::new(false));
            let pulse = heartbeat.clone();
            let pulse_stop = Arc::clone(&stop);
            let thread = thread::Builder::new()
                .name("opentaskbar-heartbeat".into())
                .spawn(move || {
                    while !pulse_stop.load(Ordering::Relaxed) {
                        pulse.signal();
                        thread::sleep(HEARTBEAT_INTERVAL);
                    }
                });
            *slot = Some(WatchdogState {
                clean_exit,
                heartbeat,
                stop,
                pulse_thread: thread.ok(),
            });
            log::info("Watchdog armed; heartbeat pulse active.");
        }
        Err(err) => log::warn(format!("Failed to start C watchdog: {err}")),
    }
}

/// Signals one heartbeat pulse manually (useful when the UI loop drives recovery).
/// The dedicated pulse thread already does this once per second; extra pulses are harmless.
pub fn heartbeat() {
    if let Ok(state) = WATCHDOG.lock()
        && let Some(state) = state.as_ref()
    {
        state.heartbeat.signal();
    }
}

pub fn is_armed() -> bool {
    WATCHDOG.lock().is_ok_and(|state| state.is_some())
}

/// Stops the heartbeat first, then tells the watchdog this was a clean exit.
/// Called only after the native taskbar has been verified as restored.
pub fn notify_clean_exit() {
    let Ok(mut state) = WATCHDOG.lock() else {
        return;
    };
    let Some(mut watchdog) = state.take() else {
        return;
    };
    watchdog.stop.store(true, Ordering::Relaxed);
    if let Some(thread) = watchdog.pulse_thread.take() {
        let _ = thread.join();
    }
    watchdog.clean_exit.signal();
    log::info("Watchdog clean-exit signalled; heartbeat stopped.");
}
