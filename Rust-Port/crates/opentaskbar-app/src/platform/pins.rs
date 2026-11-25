//! Reads Windows taskbar pinned shortcuts (`.lnk`), mirroring OpenTaskbar C#'s
//! `WindowsTaskbarPinImporter`: resolves `PKEY_AppUserModel_ID`,
//! `PKEY_Link_TargetParsingPath` (`::{4234d49b-0245-4df3-b780-3893943456e1}\<AUMID>`),
//! `IShellLinkW::GetIconLocation`, and `IShellLinkW::GetPath`.

use std::fs;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
use windows::core::{GUID, HSTRING, Interface};

mod launch;
pub use launch::{launch, launch_app, launch_elevated, launch_with_args};

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
const APPS_FOLDER_PREFIX: &str = "shell:AppsFolder\\";

fn is_packaged_aumid(id: &str) -> bool {
    let Some((family, app)) = id.split_once('!') else {
        return false;
    };
    let Some((name, publisher)) = family.rsplit_once('_') else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
        && publisher.len() == 13
        && publisher.bytes().all(|c| c.is_ascii_alphanumeric())
        && !app.is_empty()
        && app
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
}

fn apps_folder_aumid<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if !value.get(..prefix.len())?.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let id = value[prefix.len()..].trim();
    is_packaged_aumid(id).then_some(id)
}

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

#[derive(Default)]
struct ShortcutMeta {
    target_exe: Option<String>,
    aumid: Option<String>,
    icon_location: Option<String>,
    has_arguments: bool,
}

fn resolve_shortcut(lnk_path: &str) -> ShortcutMeta {
    // SAFETY: COM STA calls to inspect `.lnk` metadata.
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER)
        else {
            return ShortcutMeta::default();
        };
        let Ok(persist) = link.cast::<IPersistFile>() else {
            return ShortcutMeta::default();
        };
        if persist.Load(&HSTRING::from(lnk_path), STGM_READ).is_err() {
            return ShortcutMeta::default();
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

        let mut arguments_buf = [0u16; 2048];
        let arguments = if link.GetArguments(&mut arguments_buf).is_ok() {
            String::from_utf16_lossy(arguments_buf.split(|&c| c == 0).next().unwrap_or_default())
        } else {
            String::new()
        };
        let aumid = link
            .cast::<IPropertyStore>()
            .ok()
            .and_then(|store| {
                let direct = read_prop_string(&store, &PKEY_APPUSERMODEL_ID);
                if direct.as_deref().is_some_and(is_packaged_aumid) {
                    return direct;
                }
                let parsing_path = read_prop_string(&store, &PKEY_LINK_TARGET_PARSING_PATH)?;
                apps_folder_aumid(&parsing_path, APPS_FOLDER_CLSID_PREFIX)
                    .or_else(|| apps_folder_aumid(&parsing_path, APPS_FOLDER_PREFIX))
                    .map(str::to_owned)
            })
            .or_else(|| {
                let exe = Path::new(target_exe.as_deref()?).file_name()?.to_str()?;
                if !exe.eq_ignore_ascii_case("explorer.exe") {
                    return None;
                }
                explorer_arguments_aumid(&arguments).map(str::to_owned)
            });

        ShortcutMeta {
            target_exe,
            aumid,
            icon_location,
            has_arguments: !arguments.trim().is_empty(),
        }
    }
}

fn explorer_arguments_aumid(arguments: &str) -> Option<&str> {
    let arguments_lower = arguments.to_ascii_lowercase();
    let start = arguments_lower.find("shell:appsfolder\\")?;
    let target = arguments[start..].split([' ', '"', '\t']).next()?;
    apps_folder_aumid(target, APPS_FOLDER_PREFIX)
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
        while let Some(pos) = out.to_ascii_lowercase().find(&tok_lower) {
            let Ok(val) = std::env::var(var) else {
                break;
            };
            out.replace_range(pos..pos + token.len(), &val);
        }
    }
    out
}

/// Resolves a `WindowsApps\<PackageFullName>\...` executable path to a packaged app ID.
/// Mirrors C# `ApplicationIconProvider.TryResolvePackagedAppUserModelId`.
pub fn resolve_windowsapps_aumid(exe_path: &str) -> Option<String> {
    let exe = Path::new(exe_path);
    let package_root = exe.ancestors().skip(1).find(|dir| {
        dir.parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("WindowsApps"))
    })?;
    let [name, _, _, _, publisher] = package_root
        .file_name()?
        .to_str()?
        .split('_')
        .collect::<Vec<_>>()[..]
    else {
        return None;
    };
    let xml = fs::read_to_string(package_root.join("AppxManifest.xml")).ok()?;
    let relative_exe = exe.strip_prefix(package_root).ok()?.to_str()?;
    let app = manifest_app_id(&xml, relative_exe)?;
    let id = format!("{name}_{publisher}!{app}");
    is_packaged_aumid(&id).then_some(id)
}

fn manifest_app_id(xml: &str, relative_exe: &str) -> Option<String> {
    let manifest = roxmltree::Document::parse(xml).ok()?;
    let apps = manifest
        .root_element()
        .children()
        .find(|node| node.tag_name().name() == "Applications")?;
    let entries = apps
        .children()
        .filter(|node| node.tag_name().name() == "Application")
        .filter_map(|node| Some((node.attribute("Id")?, node.attribute("Executable"))));
    let relative_exe = relative_exe.replace('/', "\\");
    let matching_id = entries.clone().find_map(|(id, exe)| {
        exe.filter(|exe| exe.replace('/', "\\").eq_ignore_ascii_case(&relative_exe))
            .map(|_| id)
    });
    matching_id
        .or_else(|| {
            let mut entries = entries;
            let (only_id, _) = entries.next()?;
            entries.next().is_none().then_some(only_id)
        })
        .map(str::to_owned)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_store_shortcut_preserves_non_default_application_id() {
        let args = r#"/separate,"SHELL:AppsFolder\Example.Product_0123456789abc!Client""#;
        assert_eq!(
            explorer_arguments_aumid(args),
            Some("Example.Product_0123456789abc!Client")
        );
        assert_eq!(
            explorer_arguments_aumid(r"shell:AppsFolder\Chrome.Profile"),
            None
        );
    }

    #[test]
    fn package_manifest_selects_the_application_for_the_executable() {
        let xml = r#"<Package xmlns="urn:test">
            <Applications>
                <Application Id="Main" Executable="Main.exe"/>
                <Application Executable="tools/Second.exe" Id="Second"/>
            </Applications>
        </Package>"#;
        assert_eq!(
            manifest_app_id(xml, r"tools\SECOND.EXE").as_deref(),
            Some("Second")
        );
        assert_eq!(manifest_app_id(xml, "Unknown.exe"), None);
        assert_eq!(manifest_app_id("<invalid>", "Unknown.exe"), None);
    }
}
