//! Small Win32 helpers shared by the platform modules.

/// `CreateProcess` flag that suppresses the console window for child processes.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Returns `true` on Windows 11 or newer (`RtlGetVersion` build >= 22000), cached per process.
pub fn is_windows_11() -> bool {
    static IS_WIN11: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS_WIN11.get_or_init(|| os_build_number() >= 22000)
}

/// Manifest-independent Windows build number from `ntdll!RtlGetVersion` (`0` on failure).
pub fn os_build_number() -> u32 {
    #[repr(C)]
    struct RtlOsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd_version: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut RtlOsVersionInfoW) -> i32;
    }
    let mut info = RtlOsVersionInfoW {
        size: std::mem::size_of::<RtlOsVersionInfoW>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        csd_version: [0; 128],
    };
    if unsafe { RtlGetVersion(&mut info) } == 0 {
        info.build
    } else {
        0
    }
}

/// Null-terminated UTF-16 buffer for Win32/WinHTTP APIs.
pub use opentaskbar_core::wide;

/// Half-open hit test (`[left, right) x [top, bottom)`) for Win32 `RECT`s.
#[inline]
pub fn pt_in_rect(r: &windows_sys::Win32::Foundation::RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Sets a `u32` DWM window attribute on `hwnd`, returning whether `DwmSetWindowAttribute` succeeded.
pub fn set_dwm_u32(hwnd: windows_sys::Win32::Foundation::HWND, attr: i32, value: u32) -> bool {
    unsafe {
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            attr as u32,
            (&value as *const u32).cast(),
            size_of::<u32>() as u32,
        ) == 0
    }
}

/// Reads an `HKCU` `REG_DWORD` registry value, returning `None` when missing or not 4 bytes.
pub fn read_hkcu_dword(subkey: &str, value_name: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    let path = wide(subkey);
    let name = wide(value_name);
    let mut key = std::ptr::null_mut();
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return None;
        }
        let mut value: u32 = 0;
        let mut len = size_of::<u32>() as u32;
        let status = RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut len,
        );
        RegCloseKey(key);
        (status == 0 && len == size_of::<u32>() as u32).then_some(value)
    }
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
