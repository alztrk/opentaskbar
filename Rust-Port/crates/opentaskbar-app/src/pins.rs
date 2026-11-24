//! Reads Windows taskbar pinned shortcuts (`.lnk`) and launches executables
//! via `ShellExecuteW` on a helper thread so the GPUI thread never stalls.

use std::fs;
use std::path::PathBuf;

use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

#[derive(Clone, Debug)]
pub struct PinnedApp {
    pub name: String,
    pub path: String,
}

fn pins_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar")
}

pub fn load_pins() -> Vec<PinnedApp> {
    let dir = pins_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut items: Vec<PinnedApp> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let is_lnk = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
            if !is_lnk {
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Some(PinnedApp {
                name,
                path: path.to_string_lossy().into_owned(),
            })
        })
        .collect();
    items.sort_by_key(|a| a.name.to_lowercase());
    items
}

pub fn launch(target: String) {
    std::thread::Builder::new()
        .name("opentaskbar-launch".into())
        .spawn(move || {
            let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
            let file: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
            // SAFETY: null-terminated strings alive for the duration of ShellExecuteW.
            unsafe {
                ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    file.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                );
            }
        })
        .ok();
}
