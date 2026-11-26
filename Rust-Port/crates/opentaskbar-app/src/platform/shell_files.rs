//! Shell file operations (C# `DesktopFileTransfer` / `Drop.cs` parity).
//!
//! - Rejects `\\.\` and `\\?\` device/NT paths before any copy/move.
//! - Uses `SHFileOperationW` with `FOF_ALLOWUNDO | FOF_NOCONFIRMATION` plus
//!   `UIOption.AllDialogs` equivalent (`FOF_NOERRORUI` omitted => progress/error dialogs).
//! - Distinguishes user cancellation (`0x0000004C` / `ERROR_CANCELLED`, or `fAnyOperationsAborted`)
//!   from real failures, so the UI does not present a cancelled drop as an error.

use std::path::{Path, PathBuf};

use windows_sys::Win32::UI::Shell::{
    FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FO_COPY, FO_DELETE, FO_MOVE,
    SHFILEOPSTRUCTW, SHFileOperationW,
};

/// Outcome of a shell copy/move/delete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileOperationOutcome {
    Success,
    Cancelled,
    Failed(i32),
}

/// UIOption parity: `AllDialogs` shows progress and error dialogs, `OnlyErrorDialogs` does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UIOption {
    AllDialogs,
    OnlyErrorDialogs,
}

/// True for `\\.\` (device) and `\\?\` (extended NT) paths, which must be rejected.
pub fn is_device_path(path: &str) -> bool {
    let p = path.trim_start_matches('"');
    p.starts_with(r"\\.\") || p.starts_with(r"\\?\") || p.starts_with("//./") || p.starts_with("//?/")
}

/// Validates a source/destination list: no device paths, no empty entries.
pub fn validate_paths(paths: &[String]) -> Result<(), &'static str> {
    if paths.is_empty() {
        return Err("empty");
    }
    for path in paths {
        let path = path.trim();
        if path.is_empty() {
            return Err("empty-entry");
        }
        if is_device_path(path) {
            return Err("device-path");
        }
    }
    Ok(())
}

/// Double-NUL terminator required by `SHFILEOPSTRUCTW::pFrom` / `pTo`.
pub fn double_nul(paths: &[String]) -> Vec<u16> {
    let mut out = Vec::new();
    for p in paths {
        out.extend(p.trim().encode_utf16());
        out.push(0);
    }
    out.push(0);
    out
}

/// Classifies a `SHFileOperationW` return code and `fAnyOperationsAborted` flag.
pub fn classify_result(hr: i32, aborted: bool) -> FileOperationOutcome {
    // 0x4C = ERROR_CANCELLED; DE_OPCANCELLED is surfaced through `fAnyOperationsAborted`.
    if aborted || hr == 0x4C {
        FileOperationOutcome::Cancelled
    } else if hr == 0 {
        FileOperationOutcome::Success
    } else {
        FileOperationOutcome::Failed(hr)
    }
}

fn shell_operation(
    operation: u32,
    from: &[String],
    to: &[String],
    ui: UIOption,
) -> FileOperationOutcome {
    if let Err(_reason) = validate_paths(from) {
        return FileOperationOutcome::Failed(-1);
    }
    if operation == FO_COPY || operation == FO_MOVE {
        if let Err(_reason) = validate_paths(to) {
            return FileOperationOutcome::Failed(-1);
        }
    }
    let from_w = double_nul(from);
    let to_w = if to.is_empty() { Vec::new() } else { double_nul(to) };
    let mut flags = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOCONFIRMMKDIR;
    if ui == UIOption::OnlyErrorDialogs {
        // AllDialogs keeps the progress UI; OnlyErrorDialogs suppresses it.
        flags |= windows_sys::Win32::UI::Shell::FOF_SILENT;
    }
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.hwnd = std::ptr::null_mut();
    op.wFunc = operation;
    op.pFrom = from_w.as_ptr();
    op.pTo = if to_w.is_empty() {
        std::ptr::null()
    } else {
        to_w.as_ptr()
    };
    op.fFlags = flags as u16;
    let hr = unsafe { SHFileOperationW(&mut op) };
    classify_result(hr, op.fAnyOperationsAborted != 0)
}

pub fn copy_files(from: &[String], to: &[String], ui: UIOption) -> FileOperationOutcome {
    shell_operation(FO_COPY, from, to, ui)
}

pub fn move_files(from: &[String], to: &[String], ui: UIOption) -> FileOperationOutcome {
    shell_operation(FO_MOVE, from, to, ui)
}

pub fn delete_files(from: &[String], ui: UIOption) -> FileOperationOutcome {
    shell_operation(FO_DELETE, from, &[], ui)
}

/// Resolves a destination folder for a drop, rejecting device paths early.
pub fn resolve_destination(base: &str, name: &str) -> Option<PathBuf> {
    if is_device_path(base) || is_device_path(name) {
        return None;
    }
    let mut path = PathBuf::from(base);
    path.push(Path::new(name).file_name()?);
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_device_and_extended_paths() {
        assert!(is_device_path(r"\\.\PhysicalDrive0"));
        assert!(is_device_path(r"\\?\C:\secret"));
        assert!(is_device_path("//./COM1"));
        assert!(!is_device_path(r"C:\Users\me\file.txt"));
        assert_eq!(validate_paths(&[r"\\.\pipe\x".into()]), Err("device-path"));
        assert_eq!(validate_paths(&[]), Err("empty"));
        assert!(validate_paths(&[r"C:\a.txt".into()]).is_ok());
    }

    #[test]
    fn double_nul_layout() {
        let buf = double_nul(&["a".into(), "bb".into()]);
        assert_eq!(buf, vec![b'a' as u16, 0, b'b' as u16, b'b' as u16, 0, 0]);
    }

    #[test]
    fn cancellation_is_not_an_error() {
        assert_eq!(classify_result(0, false), FileOperationOutcome::Success);
        assert_eq!(classify_result(0, true), FileOperationOutcome::Cancelled);
        assert_eq!(classify_result(0x4C, false), FileOperationOutcome::Cancelled);
        assert_eq!(classify_result(5, false), FileOperationOutcome::Failed(5));
    }

    #[test]
    fn destination_resolution_uses_file_name() {
        let p = resolve_destination(r"C:\Drop", r"C:\Other\file.txt").unwrap();
        assert_eq!(p, PathBuf::from(r"C:\Drop\file.txt"));
        assert!(resolve_destination(r"\\?\C:\Drop", "file.txt").is_none());
    }
}
