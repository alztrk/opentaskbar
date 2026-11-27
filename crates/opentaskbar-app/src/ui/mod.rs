//! Reusable UI surfaces: GPUI design system, Settings window, shared DockFlyout, and popup menu.

pub mod components;
pub mod flyout;
pub mod menu;
pub mod prompt;
pub mod settings;
pub mod settings_search;
pub mod text_input;

use std::cell::RefCell;
use std::collections::HashSet;

use windows_sys::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, IDC_ARROW, LoadCursorW, MSG, RegisterClassExW, TranslateMessage,
    WNDCLASSEXW,
};

/// Result of one iteration of a modal message loop.
pub(crate) enum ModalStep {
    /// Translate and dispatch the message.
    Continue,
    /// Consume the message without native dispatch (handled by the owner).
    Skip,
    /// Leave the loop (the window is done).
    Break,
}

/// Registers a popup window class once per thread. Repeated calls are no-ops,
/// and a class registered on another thread is tolerated (`ERROR_CLASS_ALREADY_EXISTS`).
pub(crate) fn register_popup_class(
    name: &'static str,
    wndproc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    hinstance: HINSTANCE,
    style: u32,
) {
    thread_local! {
        static REGISTERED: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
    }
    REGISTERED.with(|registered| {
        if registered.borrow().contains(name) {
            return;
        }
        let class_name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
            lpszClassName: class_name.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: `wc` is a fully initialized WNDCLASSEXW and `class_name` outlives the call.
        unsafe {
            if RegisterClassExW(&wc) == 0 {
                const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
                debug_assert_eq!(GetLastError(), ERROR_CLASS_ALREADY_EXISTS);
            }
        }
        registered.borrow_mut().insert(name);
    });
}

/// Shared modal `GetMessageW` loop. The callback can filter messages for its
/// own window before native translation/dispatch, and can end the loop.
pub(crate) fn modal_loop(mut step: impl FnMut(&MSG) -> ModalStep) {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        match step(&msg) {
            ModalStep::Continue => unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            },
            ModalStep::Skip => {}
            ModalStep::Break => break,
        }
    }
}
