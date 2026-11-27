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
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MapVirtualKeyW,
    SendInput, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL,
    VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, keybd_event,
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

use crate::platform::win32_helpers::wide;

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

/// Generic `VK_SHIFT` / `VK_CONTROL` / `VK_MENU` are aliases: a key-up for the
/// alias does not reliably release a physical left/right key after a secure
/// desktop transition. Track the real keys instead.
const PHYSICAL_MODIFIERS: [u16; 8] = [
    VK_LSHIFT,
    VK_RSHIFT,
    VK_LCONTROL,
    VK_RCONTROL,
    VK_LMENU,
    VK_RMENU,
    VK_LWIN,
    VK_RWIN,
];

/// Maps a generic or physical modifier to the physical key we can release.
fn physical_modifier(vk: u16) -> Option<u16> {
    match vk {
        VK_SHIFT => Some(VK_LSHIFT),
        VK_CONTROL => Some(VK_LCONTROL),
        VK_MENU => Some(VK_LMENU),
        vk if PHYSICAL_MODIFIERS.contains(&vk) => Some(vk),
        _ => None,
    }
}

fn is_modifier(vk: u16) -> bool {
    physical_modifier(vk).is_some()
}

/// Extended-key flag: right Ctrl/Alt, both Win keys and the right Shift are
/// delivered with `KEYEVENTF_EXTENDEDKEY` on a standard US layout.
fn modifier_keyup(vk: u16) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) as u16 };
    let extended = matches!(vk, VK_RCONTROL | VK_RMENU | VK_LWIN | VK_RWIN);
    let flags =
        KEYEVENTF_KEYUP | KEYEVENTF_SCANCODE | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
    key_input(0, flags, scan)
}

/// Releases the most recently noted modifier first and drops duplicates, so a
/// key tracked both by `note_modifier_pressed` and `GetAsyncKeyState` gets one
/// physical key-up, not two.
fn release_order(keys: Vec<u16>) -> Vec<u16> {
    let mut out = Vec::with_capacity(keys.len());
    for vk in keys.into_iter().rev() {
        if !out.contains(&vk) {
            out.push(vk);
        }
    }
    out
}

/// Remembers a modifier pressed by a chord sender so `release_stuck_modifiers`
/// can clean it up if a keystroke is lost during a session lock transition.
pub fn note_modifier_pressed(vk: u16) {
    let Some(vk) = physical_modifier(vk) else {
        return;
    };
    if let Ok(mut held) = HELD_MODIFIERS.lock()
        && !held.contains(&vk)
    {
        held.push(vk);
    }
}

pub fn note_modifier_released(vk: u16) {
    let Some(vk) = physical_modifier(vk) else {
        return;
    };
    if let Ok(mut held) = HELD_MODIFIERS.lock() {
        held.retain(|&key| key != vk);
    }
}

fn physical_modifiers() -> Vec<u16> {
    PHYSICAL_MODIFIERS
        .into_iter()
        // SAFETY: `GetAsyncKeyState` is a read-only key-state query.
        .filter(|&vk| unsafe { GetAsyncKeyState(vk as i32) < 0 })
        .collect()
}

fn send_inputs(inputs: &[INPUT]) -> usize {
    // SAFETY: every INPUT is initialized and the count matches the slice length.
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        ) as usize
    }
}

fn keybd_fallback(vk: u16, scan: u16, flags: u32, extra: usize) {
    // SAFETY: plain scalars, no pointers.
    unsafe { keybd_event(vk as u8, scan as u8, flags, extra) };
}

/// Shared release implementation with injectable input sinks so the emitted
/// key-ups can be verified without synthesising real keystrokes in tests.
fn release_stuck_modifiers_with<H, F, G>(
    physical: &H,
    held: &Mutex<Vec<u16>>,
    send: &F,
    fallback: &G,
) where
    H: Fn() -> Vec<u16>,
    F: Fn(&[INPUT]) -> usize,
    G: Fn(u16, u16, u32, usize),
{
    let mut keys = physical();
    if let Ok(mut tracked) = held.lock() {
        for key in tracked.iter().copied() {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        tracked.clear();
    }
    let keys = release_order(keys);
    if keys.is_empty() {
        return;
    }
    let inputs: Vec<INPUT> = keys.iter().copied().map(modifier_keyup).collect();
    if send(&inputs) != inputs.len() {
        // Legacy fallback: same physical key-ups, accepted in more contexts.
        // `modifier_keyup` deliberately zeroes wVk and uses a scan code, so the
        // original virtual key must be passed through to `keybd_event`.
        for (vk, input) in keys.iter().zip(inputs.iter()) {
            // SAFETY: the INPUT was produced by `modifier_keyup` on this stack.
            let ki = unsafe { input.Anonymous.ki };
            fallback(*vk, ki.wScan, ki.dwFlags, ki.dwExtraInfo);
        }
    }
}

/// Releases every modifier we pressed plus any that are physically stuck down.
/// Called on session unlock, so a modifier lost during a secure-desktop
/// transition cannot leak into the next keystroke. `SendInput` emits real
/// scan-code key-ups; `keybd_event` is the fallback if UIPI blocks injection.
pub fn release_stuck_modifiers() {
    release_stuck_modifiers_with(
        &physical_modifiers,
        &HELD_MODIFIERS,
        &send_inputs,
        &keybd_fallback,
    );
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

    #[test]
    fn generic_modifiers_are_canonicalized_to_physical_keys() {
        assert_eq!(physical_modifier(VK_SHIFT), Some(VK_LSHIFT));
        assert_eq!(physical_modifier(VK_CONTROL), Some(VK_LCONTROL));
        assert_eq!(physical_modifier(VK_MENU), Some(VK_LMENU));
        assert_eq!(physical_modifier(VK_RWIN), Some(VK_RWIN));
        assert_eq!(physical_modifier(VK_A_U16), None);
        assert_eq!(PHYSICAL_MODIFIERS.len(), 8);

        HELD_MODIFIERS.lock().unwrap().clear();
        note_modifier_pressed(VK_SHIFT);
        assert!(
            HELD_MODIFIERS
                .lock()
                .is_ok_and(|keys| keys.as_slice() == [VK_LSHIFT])
        );
        note_modifier_released(VK_SHIFT);
        assert!(HELD_MODIFIERS.lock().is_ok_and(|keys| keys.is_empty()));
    }

    #[test]
    fn release_order_reverses_and_deduplicates_physical_modifiers() {
        assert_eq!(
            release_order(vec![VK_LSHIFT, VK_LWIN, VK_LSHIFT, VK_RCONTROL]),
            vec![VK_RCONTROL, VK_LSHIFT, VK_LWIN]
        );
        assert!(release_order(Vec::new()).is_empty());

        // Release through test sinks instead of asserting on source text: the
        // physical key-ups (and the fallback) are what actually matter.
        use std::cell::RefCell;
        let held = Mutex::new(vec![VK_LSHIFT, VK_LWIN, VK_LSHIFT]);
        let sent: RefCell<Vec<(u16, u32)>> = RefCell::new(Vec::new());
        release_stuck_modifiers_with(
            &|| Vec::new(),
            &held,
            &|inputs| {
                for input in inputs {
                    // SAFETY: INPUT produced by `modifier_keyup` above.
                    let ki = unsafe { input.Anonymous.ki };
                    sent.borrow_mut().push((ki.wVk, ki.dwFlags));
                }
                inputs.len()
            },
            &|_, _, _, _| {},
        );
        assert_eq!(sent.borrow().len(), 2, "duplicates must be dropped");
        assert!(held.lock().is_ok_and(|keys| keys.is_empty()));
        assert!(
            sent.borrow()
                .iter()
                .all(|(_, flags)| flags & KEYEVENTF_KEYUP != 0)
        );

        // When SendInput is blocked, the legacy fallback sends the same key-ups.
        let held = Mutex::new(vec![VK_RCONTROL]);
        let fallback: RefCell<Vec<u16>> = RefCell::new(Vec::new());
        release_stuck_modifiers_with(&|| Vec::new(), &held, &|_| 0, &|vk, _, _, _| {
            fallback.borrow_mut().push(vk)
        });
        assert_eq!(*fallback.borrow(), vec![VK_RCONTROL]);
    }
}
