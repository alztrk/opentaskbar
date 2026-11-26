//! Native Windows file/folder pickers (C# `Microsoft.Win32.SaveFileDialog` / `OpenFileDialog` /
//! `OpenFolderDialog` parity) used by Settings backups, theme import/export and folder stacks.
//!
//! Uses the modern `IFileDialog` COM API instead of the legacy common-dialog controls so the
//! dark mode, Acrylic shell and per-monitor DPI of the rest of the app are preserved.

use std::path::PathBuf;

use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_OVERWRITEPROMPT, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog,
    IFileDialog, IShellItem, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, PWSTR};

fn wide(text: &str) -> HSTRING {
    HSTRING::from(text)
}

fn init_com() {
    // RPC_E_CHANGED_MODE is fine: the dialog marshals to the owning STA itself.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
}

fn result_path(dialog: &IFileDialog) -> Option<PathBuf> {
    let item: IShellItem = unsafe { dialog.GetResult().ok()? };
    let path: PWSTR = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH).ok()? };
    let text = unsafe { path.to_string().ok()?.to_string() };
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    if text.is_empty() {
        None
    } else {
        Some(PathBuf::from(text))
    }
}

fn set_filter(dialog: &IFileDialog, name: &str, pattern: &str) -> Option<Vec<HSTRING>> {
    let name = wide(name);
    let pattern = wide(pattern);
    let spec = [COMDLG_FILTERSPEC {
        pszName: windows::core::PCWSTR(name.as_ptr()),
        pszSpec: windows::core::PCWSTR(pattern.as_ptr()),
    }];
    unsafe { dialog.SetFileTypes(&spec).ok()? };
    unsafe { dialog.SetFileTypeIndex(1).ok()? };
    Some(vec![name, pattern])
}

fn base_dialog(clsid: &windows::core::GUID, title: &str) -> Option<IFileDialog> {
    init_com();
    let dialog: IFileDialog = unsafe { CoCreateInstance(clsid, None, CLSCTX_ALL).ok()? };
    let title = wide(title);
    unsafe {
        dialog
            .SetTitle(windows::core::PCWSTR(title.as_ptr()))
            .ok()?
    };
    Some(dialog)
}

/// Native "Open file" picker. Returns `None` when the user cancels.
pub fn open_file(title: &str, filter_name: &str, filter_pattern: &str) -> Option<PathBuf> {
    let dialog = base_dialog(&FileOpenDialog, title)?;
    let _filters = set_filter(&dialog, filter_name, filter_pattern)?;
    let options = unsafe { dialog.GetOptions().ok()? } | FOS_FILEMUSTEXIST;
    unsafe { dialog.SetOptions(options).ok()? };
    unsafe { dialog.Show(None).ok()? };
    result_path(&dialog)
}

/// Native "Save file" picker with overwrite confirmation. Returns `None` when cancelled.
pub fn save_file(
    title: &str,
    default_name: &str,
    filter_name: &str,
    filter_pattern: &str,
) -> Option<PathBuf> {
    let dialog = base_dialog(&FileSaveDialog, title)?;
    let _filters = set_filter(&dialog, filter_name, filter_pattern)?;
    let name = wide(default_name);
    unsafe {
        dialog
            .SetFileName(windows::core::PCWSTR(name.as_ptr()))
            .ok()?
    };
    let options = unsafe { dialog.GetOptions().ok()? } | FOS_OVERWRITEPROMPT;
    unsafe { dialog.SetOptions(options).ok()? };
    unsafe { dialog.Show(None).ok()? };
    result_path(&dialog)
}

/// Native folder picker (used by Settings folder stacks and theme folder import).
pub fn open_folder(title: &str) -> Option<PathBuf> {
    let dialog = base_dialog(&FileOpenDialog, title)?;
    let options = unsafe { dialog.GetOptions().ok()? } | FOS_PICKFOLDERS;
    unsafe { dialog.SetOptions(options).ok()? };
    unsafe { dialog.Show(None).ok()? };
    result_path(&dialog)
}
