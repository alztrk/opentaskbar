//! Reads Windows taskbar pinned shortcuts (`.lnk`), resolves their target executable
//! or Store AUMID (so icons have no shortcut arrows and match running windows accurately),
//! and launches applications via `ShellExecuteW` on a helper thread.

use std::fs;
use std::path::PathBuf;

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
    STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
use windows::core::{GUID, HSTRING, Interface};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// `System.AppUserModel.ID` (`{9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5`).
const PKEY_APPUSERMODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x9F4C_2855_9F79_4B39_A8D0_E1D4_2DE1_D5F3),
    pid: 5,
};

#[derive(Clone, Debug)]
pub struct PinnedApp {
    pub name: String,
    /// Original `.lnk` path for launching with arguments/working dir intact.
    pub launch_path: String,
    /// Resolved executable path when available (used to match running windows).
    pub target_exe: Option<String>,
    /// Parsing name for `IShellItemImageFactory` (`shell:AppsFolder\<AUMID>`, resolved `.exe`, or `.lnk`).
    pub icon_source: String,
}

fn pins_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar")
}

fn resolve_shortcut(lnk_path: &str) -> (Option<String>, Option<String>) {
    // SAFETY: COM STA calls to inspect a `.lnk` file's target path and AUMID.
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER)
        else {
            return (None, None);
        };
        let Ok(persist) = link.cast::<IPersistFile>() else {
            return (None, None);
        };
        if persist.Load(&HSTRING::from(lnk_path), STGM_READ).is_err() {
            return (None, None);
        }

        let mut target_buf = [0u16; 1024];
        let target_exe = if link
            .GetPath(&mut target_buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
            .is_ok()
        {
            let len = target_buf
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(target_buf.len());
            let raw = String::from_utf16_lossy(&target_buf[..len]);
            let expanded = expand_env_vars(&raw);
            (!expanded.is_empty() && PathBuf::from(&expanded).exists()).then_some(expanded)
        } else {
            None
        };

        let aumid = link.cast::<IPropertyStore>().ok().and_then(|store| {
            let val = store.GetValue(&PKEY_APPUSERMODEL_ID).ok()?;
            let pwstr =
                windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc(&val)
                    .ok()?;
            let text = pwstr.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0.cast()));
            text.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
        });

        (target_exe, aumid)
    }
}

fn expand_env_vars(input: &str) -> String {
    let mut out = input.to_owned();
    for var in [
        "LOCALAPPDATA",
        "APPDATA",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "SystemRoot",
        "WINDIR",
        "USERPROFILE",
    ] {
        let token = format!("%{var}%");
        if out.to_lowercase().contains(&token.to_lowercase())
            && let Ok(val) = std::env::var(var)
        {
            // Case-insensitive replace
            let lower = out.to_lowercase();
            let tok_lower = token.to_lowercase();
            if let Some(pos) = lower.find(&tok_lower) {
                out.replace_range(pos..pos + token.len(), &val);
            }
        }
    }
    out
}

pub fn load_pins() -> Vec<PinnedApp> {
    let dir = pins_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    // SAFETY: initialize COM STA for resolving `.lnk` shortcuts on this thread.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

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
            let launch_path = path.to_string_lossy().into_owned();
            let (target_exe, aumid) = resolve_shortcut(&launch_path);

            // Prefer Store AUMID or resolved executable so Windows never overlays a shortcut arrow.
            let icon_source = if let Some(ref id) = aumid
                && id.contains('!')
            {
                format!(r"shell:AppsFolder\{id}")
            } else if let Some(ref exe) = target_exe {
                exe.clone()
            } else {
                launch_path.clone()
            };

            Some(PinnedApp {
                name,
                launch_path,
                target_exe,
                icon_source,
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
