//! Extended shell helpers used by the dock and Quick Settings: system tools,
//! guaranteed modifier release, focused-text replacement, drag-time desktop
//! reveal, minimize-all and DWM extended-frame measurement.
//!
//! C4 API: `show_desktop_for_drag()`, `minimize_all()`,
//! `release_stuck_modifiers()`, `replace_focused_text(&str)`.

use std::sync::Mutex;
use std::time::Instant;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL, WM_COMMAND};

const COMMAND_MINIMIZE_ALL: usize = 419;
const WM_COMMAND_U32: u32 = WM_COMMAND;
const VK_DELETE_U16: u16 = 0x2E;
const VK_A_U16: u16 = 0x41;
const DRAG_REVEAL_FALLBACK_COOLDOWN_MS: u128 = 4000;

#[link(name = "user32")]
unsafe extern "system" {
    fn LockWorkStation() -> i32;
}

static HELD_MODIFIERS: Mutex<Vec<u16>> = Mutex::new(Vec::new());
static LAST_DRAG_FALLBACK: Mutex<Option<Instant>> = Mutex::new(None);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn send_key(vk: u16, flags: u32, scan: u16) -> bool {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one well-formed INPUT structure.
    unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) == 1 }
}

fn is_modifier(vk: u16) -> bool {
    matches!(vk, VK_SHIFT | VK_MENU | VK_CONTROL | VK_LWIN | VK_RWIN)
}

/// Remembers a modifier pressed by a chord sender so `release_stuck_modifiers`
/// can clean it up if a keystroke is lost during a session lock transition.
pub fn note_modifier_pressed(vk: u16) {
    if !is_modifier(vk) {
        return;
    }
    if let Ok(mut held) = HELD_MODIFIERS.lock()
        && !held.contains(&vk)
    {
        held.push(vk);
    }
}

pub fn note_modifier_released(vk: u16) {
    if let Ok(mut held) = HELD_MODIFIERS.lock() {
        held.retain(|&key| key != vk);
    }
}

fn physical_modifiers() -> Vec<u16> {
    [VK_SHIFT, VK_MENU, VK_CONTROL, VK_RWIN, VK_LWIN]
        .into_iter()
        // SAFETY: `GetAsyncKeyState` is a read-only key-state query.
        .filter(|&vk| unsafe { GetAsyncKeyState(vk as i32) < 0 })
        .collect()
}

/// Releases every modifier we pressed plus any that are physically stuck down.
/// Called when the interactive desktop returns, so a modifier lost during a
/// lock transition cannot leak into the next keystroke.
pub fn release_stuck_modifiers() {
    let mut keys = physical_modifiers();
    if let Ok(mut held) = HELD_MODIFIERS.lock() {
        for key in held.iter().copied() {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        held.clear();
    }
    // Release in reverse order so Win is the final modifier to go up.
    for key in keys.into_iter().rev() {
        send_key(key, KEYEVENTF_KEYUP, 0);
    }
}

/// Replaces the selected text in the currently focused field (Windows Search)
/// in a single input batch so no physical keystroke can interleave.
pub fn replace_focused_text(text: &str) -> bool {
    if !physical_modifiers().is_empty() {
        return false;
    }
    let mut inputs = Vec::with_capacity(4 + text.chars().count() * 2);
    inputs.push(key_input(VK_CONTROL, 0, 0));
    inputs.push(key_input(VK_A_U16, 0, 0));
    inputs.push(key_input(VK_A_U16, KEYEVENTF_KEYUP, 0));
    inputs.push(key_input(VK_CONTROL, KEYEVENTF_KEYUP, 0));
    if text.is_empty() {
        inputs.push(key_input(VK_DELETE_U16, KEYEVENTF_EXTENDEDKEY, 0));
        inputs.push(key_input(
            VK_DELETE_U16,
            KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
            0,
        ));
    }
    for character in text.chars() {
        inputs.push(key_input(0, KEYEVENTF_UNICODE, character as u16));
        inputs.push(key_input(
            0,
            KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
            character as u16,
        ));
    }
    // SAFETY: all inputs are initialized and the array length matches `inputs.len()`.
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent > 0 && sent < 4 {
        if sent == 2 {
            send_key(VK_A_U16, KEYEVENTF_KEYUP, 0);
        }
        send_key(VK_CONTROL, KEYEVENTF_KEYUP, 0);
    }
    sent as usize == inputs.len()
}

fn key_input(vk: u16, flags: u32, scan: u16) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn tray_hwnd() -> HWND {
    // Skip any fake `Shell_TrayWnd` owned by OpenTaskbar itself: posting
    // WM_COMMAND to it would minimize nothing and break drag-to-show-desktop.
    opentaskbar_shell::native_tray::find().map_or(std::ptr::null_mut(), |lease| lease.hwnd as HWND)
}

/// Reveals the desktop during an active file drag without toggling windows back
/// up on rapid re-entry, and without injecting Win+D while Ctrl/Shift drag
/// modifiers are held (Ctrl+Win+D would create a virtual desktop).
pub fn show_desktop_for_drag() -> bool {
    let tray = tray_hwnd();
    if !tray.is_null()
        && unsafe { PostMessageW(tray, WM_COMMAND_U32, COMMAND_MINIMIZE_ALL, 0) != 0 }
    {
        return true;
    }
    if minimize_all() {
        return true;
    }
    if !physical_modifiers().is_empty() {
        return false;
    }
    let now = Instant::now();
    if let Some(last) = *LAST_DRAG_FALLBACK.lock().unwrap_or_else(|e| e.into_inner())
        && now.duration_since(last).as_millis() < DRAG_REVEAL_FALLBACK_COOLDOWN_MS
    {
        return false;
    }
    *LAST_DRAG_FALLBACK.lock().unwrap_or_else(|e| e.into_inner()) = Some(now);
    // Win+D fallback via the shared shell module.
    opentaskbar_shell::taskbar::show_desktop();
    true
}

/// Minimizes all windows through Explorer's `WM_COMMAND 419` / shell verb.
pub fn minimize_all() -> bool {
    let tray = tray_hwnd();
    if !tray.is_null()
        && unsafe { PostMessageW(tray, WM_COMMAND_U32, COMMAND_MINIMIZE_ALL, 0) != 0 }
    {
        return true;
    }
    // Shell.Application.MinimizeAll via ShellExecute fallback is not available
    // without COM automation; the tray command covers Windows 10/11.
    false
}

fn shell_execute(file: &str, parameters: &str, verb: Option<&str>) -> bool {
    let file = wide(file);
    let parameters = wide(parameters);
    let verb_wide = verb.map(wide);
    // SAFETY: every pointer is to a null-terminated owned buffer or null.
    let result: HINSTANCE = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb_wide.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    result as isize > 32
}

fn system32_file(name: &str) -> String {
    let root = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_owned());
    format!("{root}\\System32\\{name}")
}

pub fn open_system_tool(command: &str) -> bool {
    let command = command.trim();
    let (file, args) = match command.find(' ') {
        Some(split) => (&command[..split], command[split + 1..].trim()),
        None => (command, ""),
    };
    shell_execute(file, args, None)
}

pub fn open_system_tool_elevated(command: &str) -> bool {
    shell_execute(command, "", Some("runas"))
}

pub fn lock_workstation() -> bool {
    // SAFETY: no arguments; the OS locks the current workstation session.
    unsafe { LockWorkStation() != 0 }
}

pub fn sign_out() -> bool {
    shell_execute(&system32_file("shutdown.exe"), "/l", None)
}

pub fn restart_computer() -> bool {
    shell_execute(&system32_file("shutdown.exe"), "/r /t 0", None)
}

pub fn shutdown_computer() -> bool {
    shell_execute(&system32_file("shutdown.exe"), "/s /t 0", None)
}

pub fn open_task_manager() -> bool {
    shell_execute(&system32_file("Taskmgr.exe"), "", None)
}

pub fn open_resource_monitor() -> bool {
    shell_execute(&system32_file("perfmon.exe"), "/res", None)
}

/// DWM's real frame bounds, used by auto-hide overlap checks and click targets.
pub fn dwm_extended_frame_bounds(hwnd: usize) -> Option<RECT> {
    if hwnd == 0 {
        return None;
    }
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    // SAFETY: writing one RECT into our own buffer from a live window.
    let status = unsafe {
        DwmGetWindowAttribute(
            hwnd as HWND,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut rect as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
    };
    (status == 0).then_some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_tool_command_splits_filename_and_arguments() {
        let file = r"C:\Windows\System32\control.exe";
        let command = format!("{file} netconnections");
        let (name, args) = match command.find(' ') {
            Some(split) => (&command[..split], command[split + 1..].trim()),
            None => (command.as_str(), ""),
        };
        assert_eq!(name, file);
        assert_eq!(args, "netconnections");
    }

    #[test]
    fn modifier_bookkeeping_ignores_regular_keys() {
        note_modifier_pressed(VK_A_U16);
        assert!(HELD_MODIFIERS.lock().is_ok_and(|keys| keys.is_empty()));
    }
}
