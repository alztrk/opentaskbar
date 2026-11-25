//! Reads Windows taskbar pinned shortcuts (`.lnk`), mirroring OpenTaskbar C#'s
//! `WindowsTaskbarPinImporter`: resolves `PKEY_AppUserModel_ID`,
//! `PKEY_Link_TargetParsingPath` (`::{4234d49b-0245-4df3-b780-3893943456e1}\<AUMID>`),
//! `IShellLinkW::GetIconLocation`, and `IShellLinkW::GetPath`.

use std::fs;
use std::path::PathBuf;

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, IPersistFile, STGM_READ,
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

/// `System.Link.TargetParsingPath` (`{B9B4B3FC-2B51-4A42-B5D8-324146AFCF25}, 2`).
const PKEY_LINK_TARGET_PARSING_PATH: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xB9B4_B3FC_2B51_4A42_B5D8_3241_46AF_CF25),
    pid: 2,
};

const APPS_FOLDER_CLSID_PREFIX: &str = "::{4234d49b-0245-4df3-b780-3893943456e1}\\";

#[derive(Clone, Debug)]
pub struct PinnedApp {
    pub name: String,
    /// Original `.lnk` path for launching with arguments/working dir intact.
    pub launch_path: String,
    /// Resolved executable path when available (used to match running windows).
    pub target_exe: Option<String>,
    /// Resolved icon source (`shell:AppsFolder\<AUMID>`, custom icon path, resolved `.exe`, or `.lnk`).
    pub icon_source: String,
}

fn pins_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar")
}

unsafe fn read_prop_string(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    // SAFETY: caller ensures COM STA is initialized and `store` is valid.
    unsafe {
        let val = store.GetValue(key).ok()?;
        let pwstr = PropVariantToStringAlloc(&val).ok()?;
        let text = pwstr.to_string().ok();
        CoTaskMemFree(Some(pwstr.0.cast()));
        text.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
    }
}

struct ShortcutMeta {
    target_exe: Option<String>,
    aumid: Option<String>,
    icon_location: Option<String>,
}

fn resolve_shortcut(lnk_path: &str) -> ShortcutMeta {
    // SAFETY: COM STA calls to inspect `.lnk` metadata.
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER)
        else {
            return ShortcutMeta {
                target_exe: None,
                aumid: None,
                icon_location: None,
            };
        };
        let Ok(persist) = link.cast::<IPersistFile>() else {
            return ShortcutMeta {
                target_exe: None,
                aumid: None,
                icon_location: None,
            };
        };
        if persist.Load(&HSTRING::from(lnk_path), STGM_READ).is_err() {
            return ShortcutMeta {
                target_exe: None,
                aumid: None,
                icon_location: None,
            };
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
            let is_windowsapps = expanded.to_lowercase().contains(r"\windowsapps\");
            (!expanded.is_empty() && (is_windowsapps || PathBuf::from(&expanded).exists()))
                .then_some(expanded)
        } else {
            None
        };

        let mut icon_buf = [0u16; 1024];
        let mut icon_idx = 0i32;
        let icon_location = if link.GetIconLocation(&mut icon_buf, &mut icon_idx).is_ok() {
            let len = icon_buf
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(icon_buf.len());
            let raw = String::from_utf16_lossy(&icon_buf[..len]);
            let expanded = expand_env_vars(raw.trim());
            (!expanded.is_empty() && PathBuf::from(&expanded).exists()).then(|| {
                if icon_idx != 0 {
                    format!("{expanded},{icon_idx}")
                } else {
                    expanded
                }
            })
        } else {
            None
        };

        let aumid = link.cast::<IPropertyStore>().ok().and_then(|store| {
            let direct = read_prop_string(&store, &PKEY_APPUSERMODEL_ID);
            if direct.as_ref().is_some_and(|s| s.contains('!')) {
                return direct;
            }
            if let Some(target_parsing) = read_prop_string(&store, &PKEY_LINK_TARGET_PARSING_PATH) {
                let lower = target_parsing.to_lowercase();
                if lower.starts_with(APPS_FOLDER_CLSID_PREFIX) {
                    let rest = &target_parsing[APPS_FOLDER_CLSID_PREFIX.len()..];
                    if !rest.is_empty() {
                        return Some(rest.to_owned());
                    }
                }
                if lower.starts_with("shell:appsfolder\\") {
                    let rest = &target_parsing["shell:appsfolder\\".len()..];
                    if !rest.is_empty() {
                        return Some(rest.to_owned());
                    }
                }
            }
            direct
        });

        ShortcutMeta {
            target_exe,
            aumid,
            icon_location,
        }
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
        let tok_lower = token.to_lowercase();
        while let Some(pos) = out.to_lowercase().find(&tok_lower) {
            let Ok(val) = std::env::var(var) else {
                break;
            };
            out.replace_range(pos..pos + token.len(), &val);
        }
    }
    out
}

/// Resolves a `WindowsApps\<PackageFullName>\...` executable path to `shell:AppsFolder\<PackageFamilyName>!<AppId>`.
/// Mirrors C# `ApplicationIconProvider.TryResolvePackagedAppUserModelId`.
pub fn resolve_windowsapps_aumid(exe_path: &str) -> Option<String> {
    let lower = exe_path.to_lowercase();
    let marker = r"\windowsapps\";
    let pos = lower.find(marker)?;
    let after = &exe_path[pos + marker.len()..];
    let pkg_folder = after.split('\\').next()?;
    let mut parts = pkg_folder.split('_');
    let name = parts.next()?;
    let publisher_id = pkg_folder.rsplit('_').next()?;
    if name.is_empty() || publisher_id.is_empty() || name == publisher_id {
        return None;
    }
    let family = format!("{name}_{publisher_id}");

    // Try reading `<Application Id="...">` from AppxManifest.xml; fall back to "App".
    let manifest_path = format!("{}{}", &exe_path[..pos + marker.len()], pkg_folder);
    let app_id = fs::read_to_string(PathBuf::from(manifest_path).join("AppxManifest.xml"))
        .ok()
        .and_then(|xml| extract_manifest_app_id(&xml))
        .unwrap_or_else(|| "App".to_owned());

    Some(format!("{family}!{app_id}"))
}

fn extract_manifest_app_id(xml: &str) -> Option<String> {
    let app_pos = xml.find("<Application")?;
    let slice = &xml[app_pos..];
    let id_pos = slice.find("Id=\"")?;
    let rest = &slice[id_pos + 4..];
    let end = rest.find('"')?;
    let id = rest[..end].trim();
    (!id.is_empty()).then(|| id.to_owned())
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
            let meta = resolve_shortcut(&launch_path);

            let resolved_aumid = meta.aumid.clone().or_else(|| {
                meta.target_exe
                    .as_deref()
                    .and_then(resolve_windowsapps_aumid)
            });

            // Mirror C# ApplicationIconProvider.GetForPinnedApp priority:
            // 1. Custom shortcut IconLocation (e.g. PWA .ico or custom resource index)
            // 2. Packaged Store app via shell:AppsFolder\<AUMID>
            // 3. Resolved target executable
            // 4. Fallback to .lnk itself
            let icon_source = if let Some(ref icon_loc) = meta.icon_location {
                icon_loc.clone()
            } else if let Some(ref id) = resolved_aumid
                && (id.contains('!') || meta.target_exe.is_none())
            {
                format!(r"shell:AppsFolder\{id}")
            } else if let Some(ref exe) = meta.target_exe {
                exe.clone()
            } else {
                launch_path.clone()
            };

            Some(PinnedApp {
                name,
                launch_path,
                target_exe: meta.target_exe,
                icon_source,
            })
        })
        .collect();
    items.sort_by_key(|a| a.name.to_lowercase());
    items
}

/// Builds a `PinnedApp` from an arbitrary `.exe` or `.lnk` file path (for user pin / file drop).
pub fn pin_from_path(raw_path: &str) -> Option<PinnedApp> {
    let path = PathBuf::from(raw_path);
    if !path.exists() {
        return None;
    }
    let name = path.file_stem()?.to_string_lossy().into_owned();
    let is_lnk = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
    if is_lnk {
        let meta = resolve_shortcut(raw_path);
        let resolved_aumid = meta.aumid.clone().or_else(|| {
            meta.target_exe
                .as_deref()
                .and_then(resolve_windowsapps_aumid)
        });
        let icon_source = if let Some(ref icon_loc) = meta.icon_location {
            icon_loc.clone()
        } else if let Some(ref id) = resolved_aumid
            && (id.contains('!') || meta.target_exe.is_none())
        {
            format!(r"shell:AppsFolder\{id}")
        } else if let Some(ref exe) = meta.target_exe {
            exe.clone()
        } else {
            raw_path.to_owned()
        };
        return Some(PinnedApp {
            name,
            launch_path: raw_path.to_owned(),
            target_exe: meta.target_exe,
            icon_source,
        });
    }
    let icon_source = resolve_windowsapps_aumid(raw_path)
        .map(|id| format!(r"shell:AppsFolder\{id}"))
        .unwrap_or_else(|| raw_path.to_owned());
    Some(PinnedApp {
        name,
        launch_path: raw_path.to_owned(),
        target_exe: Some(raw_path.to_owned()),
        icon_source,
    })
}

pub fn launch(target: String) {
    launch_impl(target, "open", None);
}

pub fn launch_elevated(target: String) {
    launch_impl(target, "runas", None);
}

pub fn launch_with_args(target: String, args: String) {
    launch_impl(target, "open", Some(args));
}

fn launch_impl(target: String, verb_str: &'static str, args: Option<String>) {
    std::thread::Builder::new()
        .name("opentaskbar-launch".into())
        .spawn(move || {
            let verb: Vec<u16> = verb_str.encode_utf16().chain(Some(0)).collect();
            let file: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
            let params: Option<Vec<u16>> = args.map(|a| a.encode_utf16().chain(Some(0)).collect());
            let params_ptr = params.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
            // SAFETY: null-terminated strings alive for the duration of ShellExecuteW.
            unsafe {
                ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    file.as_ptr(),
                    params_ptr,
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                );
            }
        })
        .ok();
}
