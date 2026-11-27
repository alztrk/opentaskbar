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

// Test-only event-state inspection (production code only signals/closes).
#[cfg(test)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn WaitForSingleObject(handle: HANDLE, milliseconds: u32) -> u32;
    fn ResetEvent(handle: HANDLE) -> i32;
}

#[cfg(test)]
const WAIT_OBJECT_0: u32 = 0;

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

/// Builds the single `--watchdog=<pid>:<token>` argument understood by the C
/// watchdog and by [`parse_watchdog_arg`].
fn watchdog_arg(pid: u32, token: &str) -> String {
    format!("--watchdog={pid}:{token}")
}

/// Parses and validates the watchdog command-line argument. Rejects missing
/// prefixes, non-numeric/zero PIDs and non-hex tokens before a process is
/// spawned, so a malformed argument can never silently disable recovery.
fn parse_watchdog_arg(arg: &str) -> Option<(u32, String)> {
    let rest = arg.strip_prefix("--watchdog=")?;
    let (pid, token) = rest.split_once(':')?;
    let pid = pid.parse::<u32>().ok()?;
    // Both the legacy 16-hex token and C# GUID "N" (32 hex) are valid.
    if pid == 0 || !(is_hex_token(token) || is_c_sharp_guid_token(token)) {
        return None;
    }
    Some((pid, token.to_owned()))
}

/// Hex token check shared by the Rust armer and the C watchdog parser. Accepts
/// both the historical 16-hex token and C# `Guid.NewGuid().ToString("N")`'s
/// 32-hex token; no other character may ever reach a named-event path.
fn is_hex_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// True for a C# GUID "N" token (exactly 32 hex digits, e.g. `0f3a9c1122334455aabbccddeeff0011`).
fn is_c_sharp_guid_token(token: &str) -> bool {
    token.len() == 32 && is_hex_token(token)
}

/// Creates a named event. The heartbeat MUST be auto-reset: `watchdog.c` waits
/// with a 4 s timeout and counts timeouts, and a manual-reset event that stayed
/// signalled after the first pulse would disable crash recovery entirely.
/// Clean-exit stays manual-reset so the signal cannot be consumed by a stray wait.
fn create_event(name: &str, manual_reset: bool) -> Option<EventHandle> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: fixed name, explicit reset mode, initially unsignalled.
    let handle =
        unsafe { CreateEventW(std::ptr::null(), i32::from(manual_reset), 0, wide.as_ptr()) };
    (!handle.is_null()).then(|| EventHandle {
        raw: handle,
        _owner: Arc::new(EventOwner::new(handle)),
    })
}

/// Byte-exact update check. A file with the right length but different bytes is
/// stale/corrupt and must be rewritten (C# `SequenceEqual` parity).
fn needs_watchdog_refresh(existing: Option<&[u8]>, expected: &[u8]) -> bool {
    existing != Some(expected)
}

/// App-directory priority: a deployed/packaged watchdog is already signed and
/// must be launched directly instead of a copy in `%LOCALAPPDATA%`.
fn select_watchdog_path(app_dir_candidate: Option<PathBuf>, data_dir_path: PathBuf) -> PathBuf {
    app_dir_candidate
        .filter(|path| path.exists())
        .unwrap_or(data_dir_path)
}

fn app_dir_watchdog() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|dir| dir.join("OpenTaskbarWatchdog.exe"))
}

fn ensure_watchdog_exe() -> Option<PathBuf> {
    let data_dir = opentaskbar_core::settings::app_data_dir();
    let data_path = data_dir.join("OpenTaskbarWatchdog.exe");
    let path = select_watchdog_path(app_dir_watchdog(), data_path);
    if path == data_dir.join("OpenTaskbarWatchdog.exe") {
        let _ = fs::create_dir_all(&data_dir);
        let existing = fs::read(&path).ok();
        if needs_watchdog_refresh(existing.as_deref(), WATCHDOG_BYTES) {
            // A kill mid-write must not leave a truncated watchdog behind.
            let tmp = path.with_extension("exe.tmp");
            if fs::write(&tmp, WATCHDOG_BYTES).is_ok()
                && fs::rename(&tmp, &path).is_err()
                && path.exists()
                && fs::remove_file(&path).is_ok()
            {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }
    path.exists().then_some(path)
}

/// Pulses the named heartbeat event until `stop` is set. Factored out of the
/// process spawn so the thread can be exercised directly in tests.
fn heartbeat_loop(heartbeat: EventHandle, stop: Arc<AtomicBool>, interval: Duration) {
    while !stop.load(Ordering::Relaxed) {
        heartbeat.signal();
        thread::sleep(interval);
    }
}

fn spawn_heartbeat(
    heartbeat: EventHandle,
    stop: Arc<AtomicBool>,
    interval: Duration,
) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("opentaskbar-heartbeat".into())
        .spawn(move || heartbeat_loop(heartbeat, stop, interval))
        .ok()
}

/// Stops the heartbeat before signalling clean exit, so no pulse can keep a
/// dock that has already restored Explorer's taskbar "alive".
fn stop_heartbeat_and_signal(state: &mut WatchdogState) -> bool {
    state.stop.store(true, Ordering::Relaxed);
    if let Some(thread) = state.pulse_thread.take() {
        let _ = thread.join();
    }
    state.clean_exit.signal()
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
    // C# `Guid.NewGuid().ToString("N")` parity: a 32-hex token. The random
    // seed prevents a stale event from a recycled PID from keeping us "alive".
    let hasher = RandomState::new();
    let high = hasher.hash_one(pid);
    let low = hasher.hash_one(high ^ u64::from(pid));
    let token = format!("{high:016x}{low:016x}");
    let Some(clean_exit) = create_event(&format!("{EVENT_PREFIX}{pid}.{token}"), true) else {
        return;
    };
    let Some(heartbeat) = create_event(&format!("{HEARTBEAT_PREFIX}{pid}.{token}"), false) else {
        return;
    };
    let arg = watchdog_arg(pid, &token);
    // Defensive: never spawn with an argument the watchdog cannot parse back.
    if parse_watchdog_arg(&arg).is_none() {
        log::warn("Invalid watchdog arguments; recovery not armed.");
        return;
    }
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
            let pulse_thread =
                spawn_heartbeat(heartbeat.clone(), Arc::clone(&stop), HEARTBEAT_INTERVAL);
            *slot = Some(WatchdogState {
                clean_exit,
                heartbeat,
                stop,
                pulse_thread,
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
    if stop_heartbeat_and_signal(&mut watchdog) {
        log::info("Watchdog clean-exit signalled; heartbeat stopped.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn test_event(tag: &str, manual_reset: bool) -> EventHandle {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            r"Local\OpenTaskbar.WatchdogTest.{tag}.{}.{id}",
            std::process::id()
        );
        create_event(&name, manual_reset).expect("named test event must be created")
    }

    fn is_signalled(event: &EventHandle) -> bool {
        // SAFETY: owned event handle; zero timeout never blocks.
        unsafe { WaitForSingleObject(event.raw, 0) == WAIT_OBJECT_0 }
    }

    fn wait_signalled(event: &EventHandle, timeout: Duration) -> bool {
        // SAFETY: owned event handle; bounded wait.
        unsafe {
            WaitForSingleObject(event.raw, timeout.as_millis().min(u32::MAX as u128) as u32)
                == WAIT_OBJECT_0
        }
    }

    fn reset_event(event: &EventHandle) -> bool {
        // SAFETY: manual-reset event owned for the lifetime of this value.
        unsafe { ResetEvent(event.raw) != 0 }
    }

    #[test]
    fn parses_only_well_formed_watchdog_arguments() {
        let arg = watchdog_arg(4242, "0f3a9c1122334455");
        assert_eq!(arg, "--watchdog=4242:0f3a9c1122334455");
        assert_eq!(
            parse_watchdog_arg(&arg),
            Some((4242, "0f3a9c1122334455".to_owned()))
        );
        // C# Guid.NewGuid().ToString("N") must be accepted too.
        let guid = "0f3a9c1122334455aabbccddeeff0011";
        assert!(is_c_sharp_guid_token(guid));
        assert_eq!(
            parse_watchdog_arg(&watchdog_arg(99, guid)),
            Some((99, guid.to_owned()))
        );
        assert!(!is_c_sharp_guid_token(&guid[..31]));
        assert!(!is_c_sharp_guid_token(&format!("{guid}0")));
        // Accepts upper/lower-case hex, but nothing else.
        assert!(parse_watchdog_arg("--watchdog=1:ABCDEF").is_some());
        for bad in [
            "",
            "--watchdog=",
            "watchdog=1:abc",
            "--watchdog=0:abc",
            "--watchdog=pid:abc",
            "--watchdog=1:",
            "--watchdog=1:zzz",
            "--watchdog=1:2:3",
            "--other=1:abc",
        ] {
            assert_eq!(parse_watchdog_arg(bad), None, "must reject {bad:?}");
        }
    }

    #[test]
    fn clean_exit_is_manual_reset_but_heartbeat_is_auto_reset() {
        let manual = test_event("manual", true);
        assert!(!is_signalled(&manual));
        assert!(manual.signal());
        assert!(is_signalled(&manual));
        assert!(is_signalled(&manual), "manual event stays signalled");
        assert!(reset_event(&manual));
        assert!(!is_signalled(&manual));

        let auto = test_event("auto", false);
        assert!(!is_signalled(&auto));
        assert!(auto.signal());
        assert!(wait_signalled(&auto, Duration::from_millis(20)));
        // Auto-reset is consumed by the wait: a second wait times out. The C
        // watchdog relies on this to detect missed heartbeats.
        assert!(!wait_signalled(&auto, Duration::from_millis(20)));
        assert!(auto.signal());
        assert!(is_signalled(&auto));
        assert!(!is_signalled(&auto), "consumed by the liveness check");
    }

    #[test]
    fn watchdog_binary_refresh_is_byte_exact_and_app_dir_wins() {
        assert!(needs_watchdog_refresh(None, WATCHDOG_BYTES));
        assert!(!needs_watchdog_refresh(
            Some(WATCHDOG_BYTES),
            WATCHDOG_BYTES
        ));
        let mut same_length = WATCHDOG_BYTES.to_vec();
        if let Some(byte) = same_length.first_mut() {
            *byte ^= 0xFF;
        }
        assert!(needs_watchdog_refresh(Some(&same_length), WATCHDOG_BYTES));

        let unique = format!(
            "ot-watchdog-select-{}",
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        );
        let app_exe = std::env::temp_dir().join(format!("{unique}-app.exe"));
        let missing = std::env::temp_dir().join(format!("{unique}-missing.exe"));
        let data_exe = std::env::temp_dir().join(format!("{unique}-data.exe"));
        fs::write(&app_exe, b"app").expect("temp app-dir exe must be writable");
        assert_eq!(
            select_watchdog_path(Some(app_exe.clone()), data_exe.clone()),
            app_exe
        );
        // A missing app-dir binary falls back to the materialized copy.
        assert_eq!(
            select_watchdog_path(Some(missing), data_exe.clone()),
            data_exe
        );
        let _ = fs::remove_file(&app_exe);
    }

    #[test]
    fn heartbeat_thread_pulses_until_stopped() {
        let heartbeat = test_event("heartbeat", false);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn_heartbeat(
            heartbeat.clone(),
            Arc::clone(&stop),
            Duration::from_millis(20),
        )
        .expect("heartbeat thread must spawn");

        assert!(
            wait_signalled(&heartbeat, Duration::from_secs(5)),
            "heartbeat must signal the named event"
        );
        // Auto-reset: the C watchdog's timeout loop sees a fresh pulse each time.
        assert!(!is_signalled(&heartbeat), "pulse was consumed by the wait");
        stop.store(true, Ordering::Relaxed);
        thread.join().expect("heartbeat thread must stop cleanly");

        thread::sleep(Duration::from_millis(80));
        assert!(!is_signalled(&heartbeat), "no pulse after stop");
    }

    #[test]
    fn clean_exit_stops_heartbeat_before_signalling() {
        let heartbeat = test_event("clean-hb", false);
        let clean_exit = test_event("clean-exit", true);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn_heartbeat(
            heartbeat.clone(),
            Arc::clone(&stop),
            Duration::from_millis(20),
        );
        let mut state = WatchdogState {
            clean_exit: clean_exit.clone(),
            heartbeat,
            stop: Arc::clone(&stop),
            pulse_thread: thread,
        };

        assert!(!is_signalled(&clean_exit));
        assert!(stop_heartbeat_and_signal(&mut state));
        assert!(stop.load(Ordering::Relaxed), "heartbeat stop flag set");
        assert!(state.pulse_thread.is_none(), "thread joined and taken");
        assert!(is_signalled(&clean_exit), "clean-exit event signalled");
    }
}
