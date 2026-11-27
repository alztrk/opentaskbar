//! Small Win32 helpers shared by the platform modules.

/// `CreateProcess` flag that suppresses the console window for child processes.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Null-terminated UTF-16 buffer for Win32/WinHTTP APIs.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// 32-bit-safe `SetWindowLongPtrW`.
///
/// `windows-sys` aliases `SetWindowLongPtrW` to `SetWindowLongW` on x86, whose
/// third argument is `i32` instead of `isize`. Routing every call through this
/// helper keeps the same source compiling for both release architectures.
pub fn set_window_long_ptr(
    hwnd: windows_sys::Win32::Foundation::HWND,
    index: windows_sys::Win32::UI::WindowsAndMessaging::WINDOW_LONG_PTR_INDEX,
    value: isize,
) -> isize {
    #[cfg(target_pointer_width = "64")]
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(hwnd, index, value)
    }
    #[cfg(target_pointer_width = "32")]
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongW(hwnd, index, value as i32)
            as isize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_null_terminated_and_preserves_non_ascii() {
        assert_eq!(wide(""), vec![0]);
        assert_eq!(wide("AB"), vec![b'A' as u16, b'B' as u16, 0]);
        assert_eq!(wide("ğ"), vec![0x011F, 0]);
    }
}
