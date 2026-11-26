//! Win32 rules that decide whether a top-level window is a real taskbar app
//! window. Kept in one place so the dock, the tray app list and tests agree on
//! what counts as a running application; tray-only helper windows must never be
//! promoted to normal app buttons.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetPropW, GetWindow, GetWindowLongPtrW,
};

pub const WS_EX_APPWINDOW: usize = 0x0004_0000;
pub const WS_EX_TOOLWINDOW: usize = 0x0000_0080;
pub const WS_EX_NOACTIVATE: usize = 0x0800_0000;
pub const WS_EX_WINDOWEDGE: usize = 0x0000_0100;
/// DWM's virtual-desktop cloak; unlike other cloak reasons it does not hide a
/// window from the taskbar.
pub const CLOAKED_SHELL: u32 = 0x0000_0002;
pub const DELETED_FROM_TASKBAR_PROPERTY: &str = "ITaskList_Deleted";

/// Mirrors the shell's own eligibility check: an owned window needs
/// `WS_EX_APPWINDOW` to opt in, tool and non-activatable windows are hidden
/// unless they opt in, and `ITaskbarList::DeleteTab` overrides everything.
pub fn has_taskbar_button(
    extended_style: usize,
    has_owner: bool,
    deleted_from_taskbar: bool,
) -> bool {
    let opted_in = extended_style & WS_EX_APPWINDOW != 0;
    !deleted_from_taskbar
        && (!has_owner || opted_in)
        && (opted_in || extended_style & WS_EX_TOOLWINDOW == 0)
        && (opted_in || extended_style & WS_EX_NOACTIVATE == 0)
}

/// A cloaked window is hidden from the taskbar, except for the shell cloak used
/// by virtual desktops.
pub fn is_hidden_cloak(cloaked: u32) -> bool {
    cloaked & !CLOAKED_SHELL != 0
}

/// Immersive shell frames without `WS_EX_WINDOWEDGE` are internal surfaces,
/// not app windows.
pub fn is_hidden_immersive_frame(class_name: &str, extended_style: usize) -> bool {
    extended_style & WS_EX_WINDOWEDGE == 0
        && matches!(
            class_name,
            "Windows.UI.Core.CoreWindow"
                | "ApplicationFrameWindow"
                | "StartMenuSizingFrame"
                | "Shell_LightDismissOverlay"
        )
}

/// True when `ITaskbarList::DeleteTab` marked the window as excluded.
pub fn is_deleted_from_taskbar(hwnd: usize) -> bool {
    if hwnd == 0 {
        return false;
    }
    let name: Vec<u16> = DELETED_FROM_TASKBAR_PROPERTY
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: reading a window property by its registered string name.
    !unsafe { GetPropW(hwnd as HWND, name.as_ptr()) }.is_null()
}

/// Reads the extended window style; used by both the dock and tray app list.
pub fn extended_style(hwnd: usize) -> usize {
    if hwnd == 0 {
        return 0;
    }
    // SAFETY: querying style bits of a live window handle supplied by the caller.
    unsafe { GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE) as usize }
}

/// Full taskbar-button eligibility for a live HWND. This is the single check the
/// running-window tracker uses, mirroring the C# `CollectWindowCore` order:
/// `ITaskbarList::DeleteTab` + WS_EX rules, hidden DWM cloak (with the
/// `CLOAKED_SHELL` virtual-desktop exception) and hidden immersive frames.
///
/// `cloaked` is the raw `DWMWA_CLOAKED` value (0 when the query failed): passing
/// the value rather than a bool keeps the shell-cloak exception intact.
pub fn has_taskbar_button_for_hwnd(hwnd: usize, cloaked: u32) -> bool {
    if hwnd == 0 || is_hidden_cloak(cloaked) || is_deleted_from_taskbar(hwnd) {
        return false;
    }
    let style = extended_style(hwnd);
    // SAFETY: `hwnd` comes from a synchronous top-level enumeration.
    let has_owner = unsafe { !GetWindow(hwnd as HWND, GW_OWNER).is_null() };
    let mut class_buffer = [0u16; 64];
    // SAFETY: bounded UTF-16 buffer on a live HWND from the caller.
    let length = unsafe {
        GetClassNameW(
            hwnd as HWND,
            class_buffer.as_mut_ptr(),
            class_buffer.len() as i32,
        )
    };
    if length > 0 {
        let class = String::from_utf16_lossy(&class_buffer[..length as usize]);
        if is_hidden_immersive_frame(&class, style) {
            return false;
        }
    }
    has_taskbar_button(style, has_owner, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ports every `TaskbarWindowRules` vector from C#
    /// `tests/OpenTaskbar.Tests/TaskbarWindowChecks.cs`.
    #[test]
    fn shell_rules_match_native_taskbar_eligibility() {
        assert!(has_taskbar_button(0, false, false));
        assert!(!has_taskbar_button(WS_EX_TOOLWINDOW, false, false));
        assert!(has_taskbar_button(
            WS_EX_TOOLWINDOW | WS_EX_APPWINDOW,
            false,
            false
        ));
        assert!(!has_taskbar_button(WS_EX_NOACTIVATE, false, false));
        assert!(!has_taskbar_button(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            false,
            false
        ));
        assert!(has_taskbar_button(
            WS_EX_NOACTIVATE | WS_EX_APPWINDOW,
            false,
            false
        ));
        assert!(!has_taskbar_button(0, true, false));
        assert!(has_taskbar_button(WS_EX_APPWINDOW, true, false));
        assert!(!has_taskbar_button(0, false, true));
        assert!(!has_taskbar_button(WS_EX_APPWINDOW, true, true));
    }

    #[test]
    fn cloak_and_immersive_frame_filters() {
        // App-cloaked (1) and inherited-cloaked (4) windows are hidden; the
        // virtual-desktop shell cloak (2) is explicitly kept.
        assert!(is_hidden_cloak(1));
        assert!(is_hidden_cloak(4));
        assert!(!is_hidden_cloak(0));
        assert!(!is_hidden_cloak(CLOAKED_SHELL));
        assert!(is_hidden_cloak(CLOAKED_SHELL | 1));

        assert!(is_hidden_immersive_frame("Windows.UI.Core.CoreWindow", 0));
        assert!(is_hidden_immersive_frame("ApplicationFrameWindow", 0));
        assert!(is_hidden_immersive_frame("StartMenuSizingFrame", 0));
        assert!(is_hidden_immersive_frame("Shell_LightDismissOverlay", 0));
        assert!(!is_hidden_immersive_frame(
            "ApplicationFrameWindow",
            WS_EX_WINDOWEDGE
        ));
        assert!(!is_hidden_immersive_frame("MozillaWindowClass", 0));
        assert!(!is_hidden_immersive_frame("Notepad", 0));
    }

    #[test]
    fn hwnd_check_rejects_null_deleted_and_hidden_cloak_without_touching_a_window() {
        // The cheap guards are evaluated before any Win32 style/prop/class call.
        assert!(!has_taskbar_button_for_hwnd(0, 0));
        assert!(!has_taskbar_button_for_hwnd(0, CLOAKED_SHELL));
        assert!(!has_taskbar_button_for_hwnd(0, CLOAKED_SHELL | 1));
    }
}
