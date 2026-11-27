//! Small Win32 helpers shared by the platform modules.

/// `CreateProcess` flag that suppresses the console window for child processes.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Null-terminated UTF-16 buffer for Win32/WinHTTP APIs.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
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
