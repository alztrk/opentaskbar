//! Win32 rules that decide whether a top-level window is a real taskbar app
//! window. Kept in one place so the dock, the tray app list and tests agree on
//! what counts as a running application; tray-only helper windows must never be
//! promoted to normal app buttons.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, GetPropW, GetWindowLongPtrW};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_rules_match_native_taskbar_eligibility() {
        assert!(has_taskbar_button(0, false, false));
        assert!(!has_taskbar_button(0, true, false));
        assert!(has_taskbar_button(WS_EX_APPWINDOW, true, false));
        assert!(!has_taskbar_button(WS_EX_APPWINDOW, true, true));
        assert!(!has_taskbar_button(WS_EX_TOOLWINDOW, false, false));
        assert!(has_taskbar_button(
            WS_EX_TOOLWINDOW | WS_EX_APPWINDOW,
            false,
            false
        ));
        assert!(!has_taskbar_button(WS_EX_NOACTIVATE, false, false));
    }

    #[test]
    fn cloak_and_immersive_frame_filters() {
        assert!(!is_hidden_cloak(0));
        assert!(!is_hidden_cloak(CLOAKED_SHELL));
        assert!(is_hidden_cloak(CLOAKED_SHELL | 1));
        assert!(is_hidden_immersive_frame("ApplicationFrameWindow", 0));
        assert!(!is_hidden_immersive_frame(
            "ApplicationFrameWindow",
            WS_EX_WINDOWEDGE
        ));
        assert!(!is_hidden_immersive_frame("Notepad", 0));
    }
}
