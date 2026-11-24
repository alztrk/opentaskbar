//! What dock buttons do: the Windows shortcuts behind Start, Search, Task View and Show Desktop, opening the
//! Recycle Bin, launching or activating apps, and the dock menus' commands (system tools, power, close and
//! terminate).

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_D, VK_LWIN, VK_R, VK_S, VK_TAB,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::pins::Pin;

fn key(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Presses `keys` in order and releases them in reverse (a chord such as Win+S).
pub fn send_chord(keys: &[VIRTUAL_KEY]) -> bool {
    let mut inputs: Vec<INPUT> = keys.iter().map(|&k| key(k, 0)).collect();
    inputs.extend(keys.iter().rev().map(|&k| key(k, KEYEVENTF_KEYUP)));
    // SAFETY: a valid array of keyboard inputs.
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    sent as usize == inputs.len()
}

pub fn open_start() -> bool {
    send_chord(&[VK_LWIN])
}

pub fn open_search() -> bool {
    send_chord(&[VK_LWIN, VK_S])
}

pub fn open_task_view() -> bool {
    send_chord(&[VK_LWIN, VK_TAB])
}

pub fn show_desktop() -> bool {
    send_chord(&[VK_LWIN, VK_D])
}

fn shell_execute_verb(verb: &str, target: &str, arguments: Option<&str>) -> bool {
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
    let verb = wide(verb);
    let target = wide(target);
    let arguments = arguments.map(wide);
    // SAFETY: null-terminated strings that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            arguments.as_ref().map_or(std::ptr::null(), |a| a.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    result as isize > 32
}

/// ShellExecute on a helper thread with COM initialized: launching can take long while shell extensions load.
fn spawn_shell_execute(target: String) {
    spawn_verb("open", target, None, "ErrorAppLaunch");
}

/// `failure`: the catalog key logged when the start fails (a cancelled elevation prompt is not a failure).
fn spawn_verb(
    verb: &'static str,
    target: String,
    arguments: Option<String>,
    failure: &'static str,
) {
    std::thread::spawn(move || {
        // SAFETY: COM for this short-lived thread; ShellExecute of shortcuts needs it.
        let _ = unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
        };
        if !shell_execute_verb(verb, &target, arguments.as_deref()) {
            // SAFETY: plain Win32 call.
            let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            if error != 1223 {
                // ERROR_CANCELLED: the user declined the elevation prompt.
                ot_core::log::warn(format!(
                    "{failure}: failed to start {target} (error {error})"
                ));
            }
        }
    });
}

/// On a helper thread: opening a shell folder can block for a while, and the UI thread must stay free so the
/// click's bounce animation starts at once.
pub fn open_recycle_bin() -> bool {
    spawn_verb(
        "open",
        "shell:RecycleBinFolder".to_owned(),
        None,
        "ErrorSystemToolOpen",
    );
    true
}

/// Starts a pinned app through its shortcut, so arguments, working folder and identity match the taskbar's.
pub fn launch_pin(pin: &Pin) {
    spawn_shell_execute(pin.shortcut_path.to_string_lossy().into_owned());
}

/// Starts an executable (a running-only app with no pin).
pub fn launch_executable(path: &str) {
    spawn_shell_execute(path.to_owned());
}

/// Runs a pinned app's shortcut or an executable elevated ("Run as administrator").
pub fn launch_elevated(target: &str) {
    spawn_verb("runas", target.to_owned(), None, "ErrorAppLaunch");
}

/// Starts a system tool (`ShellShortcuts.OpenSystemTool`): an executable or console snap-in plus arguments.
pub fn open_system_tool(file: &str, arguments: Option<&str>, elevated: bool) {
    spawn_verb(
        if elevated { "runas" } else { "open" },
        file.to_owned(),
        arguments.map(str::to_owned),
        "ErrorSystemToolOpen",
    );
}

pub fn open_task_manager() {
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    spawn_verb(
        "open",
        format!(r"{system}\System32\Taskmgr.exe"),
        None,
        "ErrorTaskManagerOpen",
    );
}

pub fn open_run_dialog() -> bool {
    send_chord(&[VK_LWIN, VK_R])
}

/// Asks every window to close (`Process.CloseMainWindow` sends WM_CLOSE). Returns how many accepted the message.
pub fn close_windows(windows: &[isize]) -> usize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    windows
        .iter()
        // SAFETY: posting to windows of other processes; a stale handle just fails.
        .filter(|&&window| unsafe { PostMessageW(window as _, WM_CLOSE, 0, 0) } != 0)
        .count()
}

/// `CloseApplication`: asks each process of `executable` to close its main window (`Process.CloseMainWindow`).
/// Returns (closed, failed); processes without a main window or that cannot be opened count as failed.
pub fn close_main_windows(executable: &str) -> (usize, usize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    let (processes, inaccessible) = processes_of(executable);
    let mut closed = 0;
    let mut failed = inaccessible;
    for pid in processes {
        match ot_shell::win::main_window_of_process(pid) {
            // SAFETY: posting to another process's window; a stale handle just fails.
            Some(window) if unsafe { PostMessageW(window as _, WM_CLOSE, 0, 0) } != 0 => {
                closed += 1
            }
            _ => failed += 1,
        }
    }
    (closed, failed)
}

/// Processes running `executable` (full path, case-insensitive): (process ids we may terminate, inaccessible count).
pub fn processes_of(executable: &str) -> (Vec<u32>, usize) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        QueryFullProcessImageNameW,
    };
    let mut ids = vec![0u32; 4096];
    let mut bytes = 0u32;
    // SAFETY: the buffer size is passed in bytes.
    if unsafe { K32EnumProcesses(ids.as_mut_ptr(), (ids.len() * 4) as u32, &mut bytes) } == 0 {
        return (Vec::new(), 0);
    }
    ids.truncate(bytes as usize / 4);
    let own = std::process::id();
    let mut found = Vec::new();
    let mut inaccessible = 0;
    for pid in ids.into_iter().filter(|&p| p != 0 && p != own) {
        // SAFETY: handles are closed below.
        unsafe {
            let query = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if query.is_null() {
                continue;
            }
            let mut buffer = [0u16; 1024];
            let mut size = buffer.len() as u32;
            let named = QueryFullProcessImageNameW(
                query,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut size,
            ) != 0;
            CloseHandle(query);
            if !named
                || !String::from_utf16_lossy(&buffer[..size as usize])
                    .eq_ignore_ascii_case(executable)
            {
                continue;
            }
            let terminate = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if terminate.is_null() {
                inaccessible += 1;
            } else {
                CloseHandle(terminate);
                found.push(pid);
            }
        }
    }
    (found, inaccessible)
}

/// Terminates the processes; returns how many were ended.
pub fn terminate(pids: &[u32]) -> usize {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    pids.iter()
        .filter(|&&pid| {
            // SAFETY: the handle is closed right after use.
            unsafe {
                let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
                if handle.is_null() {
                    return false;
                }
                let ended = TerminateProcess(handle, 1) != 0;
                CloseHandle(handle);
                ended
            }
        })
        .count()
}
