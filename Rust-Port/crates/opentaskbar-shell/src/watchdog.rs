//! Out-of-process watchdog mode (`OpenTaskBar.exe --watchdog=<pid>:<token>`).

use std::hash::{BuildHasher, RandomState};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent, WaitForMultipleObjects,
};

use crate::taskbar;

const ARG_PREFIX: &str = "--watchdog=";
const EVENT_PREFIX: &str = r"Local\OpenTaskBar-Rust.CleanExit.";
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
    // SAFETY: null-terminated event name.
    let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    (!handle.is_null()).then_some(OwnedHandle(handle))
}

pub fn is_invocation(args: &[String]) -> bool {
    args.iter().any(|arg| arg.starts_with(ARG_PREFIX))
}

pub fn parse_args(args: &[String]) -> Option<(u32, String)> {
    let raw = args.iter().find_map(|a| a.strip_prefix(ARG_PREFIX))?;
    let (pid, token) = raw.split_once(':')?;
    let pid: u32 = pid.parse().ok().filter(|&p| p > 0)?;
    (!token.is_empty() && token.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| (pid, token.to_owned()))
}

pub fn arm() {
    let Ok(mut slot) = CLEAN_EXIT.lock() else {
        return;
    };
    if slot.is_some() {
        return;
    }
    let pid = std::process::id();
    let token = format!("{:016x}", RandomState::new().hash_one(pid));
    let Some(event) = create_clean_event(pid, &token) else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let arg = format!("{ARG_PREFIX}{pid}:{token}");
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
    if spawned.is_ok() {
        *slot = Some(event);
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

pub fn run(args: &[String]) {
    let Some((pid, token)) = parse_args(args) else {
        return;
    };
    let Some(clean_event) = create_clean_event(pid, &token) else {
        return;
    };
    // SAFETY: synchronize-only handle on parent dock PID.
    let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if parent.is_null() {
        taskbar::restore_from_journal();
        return;
    }
    let parent = OwnedHandle(parent);
    let handles = [clean_event.0, parent.0];
    // SAFETY: waiting on two valid handles owned by this stack frame.
    let wait = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
    if wait != WAIT_OBJECT_0 {
        taskbar::restore_from_journal();
    }
}
