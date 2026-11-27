//! Reads Windows taskbar pinned shortcuts (`.lnk`), mirroring OpenTaskbar the reference //! `WindowsTaskbarPinImporter`: resolves `PKEY_AppUserModel_ID`,
//! `PKEY_Link_TargetParsingPath` (`::{4234d49b-0245-4df3-b780-3893943456e1}\<AUMID>`),
//! `IShellLinkW::GetIconLocation`, and `IShellLinkW::GetPath`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
use windows::core::{GUID, HSTRING, Interface};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_READ, REG_BINARY, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};

use super::taskbar_pin_monitor::extract_aumids;

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
        // windows-rs 0.61 PROPVARIANT has no Drop impl: the caller must clear the variant itself.
        let mut val = store.GetValue(key).ok()?;
        let pwstr = PropVariantToStringAlloc(&val).ok()?;
        let text = pwstr.to_string().ok();
        CoTaskMemFree(Some(pwstr.0.cast()));
        let _ = PropVariantClear(&mut val);
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
/// Mirrors `ApplicationIconProvider.TryResolvePackagedAppUserModelId`.
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

// ---------------------------------------------------------------------------
// Windows 11 packaged pins (Explorer Taskband\Favorites, reference parity)
// ---------------------------------------------------------------------------

const TASKBAND_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Taskband";
const FAVORITES_VALUE: &str = "Favorites";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Raw `HKCU\...\Taskband\Favorites` blob; absent keys simply mean no packaged pins.
fn taskband_favorites() -> Option<Vec<u8>> {
    let key_path = wide(TASKBAND_KEY);
    let value = wide(FAVORITES_VALUE);
    // SAFETY: read-only registry access with NUL-terminated UTF-16 names.
    unsafe {
        let mut key = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, key_path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return None;
        }
        let mut size = 0u32;
        let status = RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        );
        if status != 0 || size < 4 {
            let _ = RegCloseKey(key);
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        let mut kind = 0u32;
        let status = RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            buffer.as_mut_ptr(),
            &mut size,
        );
        let _ = RegCloseKey(key);
        (status == 0 && kind == REG_BINARY).then_some(buffer)
    }
}

/// `(display name, relative executable)` for an `<Application Id="...">` entry.
fn manifest_application(xml: &str, app_id: &str) -> Option<(Option<String>, Option<String>)> {
    let manifest = roxmltree::Document::parse(xml).ok()?;
    let mut display = None;
    let mut executable = None;
    let mut found = false;
    for node in manifest
        .descendants()
        .filter(|n| n.tag_name().name() == "Application")
    {
        if !node
            .attribute("Id")
            .is_some_and(|id| id.eq_ignore_ascii_case(app_id))
        {
            continue;
        }
        let relative = node
            .attribute("Executable")
            .map(str::trim)
            .filter(|exe| !exe.is_empty() && !exe.contains('$') && exe.ends_with(".exe"));
        executable = relative.map(str::to_owned);
        display = node
            .children()
            .find(|n| n.tag_name().name() == "VisualElements")
            .and_then(|n| n.attribute("DisplayName"))
            .map(str::to_owned);
        found = true;
        break;
    }
    if !found {
        return None;
    }
    if display.is_none() {
        display = manifest
            .descendants()
            .find(|n| n.tag_name().name() == "DisplayName")
            .and_then(|n| n.text())
            .map(str::to_owned);
    }
    Some((display, executable))
}

fn is_resource_reference(value: &str) -> bool {
    value
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("ms-resource:")
}

/// AUMID → readable exe + icon source. Packaged apps with ACL-protected or
/// virtual executables fall back to `shell:AppsFolder\<AUMID>` for both launch
/// and icon (`WindowsTaskbarPinImporter.TryResolvePackagedApp`).
pub fn resolve_packaged_app(aumid: &str) -> Option<PinnedApp> {
    let (family, app) = aumid.split_once('!')?;
    let (package, publisher) = family.rsplit_once('_')?;
    if !is_packaged_aumid(aumid) {
        return None;
    }
    let root = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
        .join("WindowsApps");

    let mut fallback_display = None;
    let mut result = None;
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let dir = entry.path();
            let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let prefix = format!("{package}_");
            if !name.starts_with(&prefix) || !name.ends_with(&format!("_{publisher}")) {
                continue;
            }
            let Ok(xml) = fs::read_to_string(dir.join("AppxManifest.xml")) else {
                continue;
            };
            let Some((display, relative)) = manifest_application(&xml, app) else {
                continue;
            };
            if let Some(display) = display
                && !is_resource_reference(&display)
            {
                fallback_display = Some(display);
            }
            let exe = relative
                .map(|relative| dir.join(relative.replace('/', "\\")))
                .filter(|path| path.is_file())
                .map(|path| path.to_string_lossy().into_owned());
            if let Some(exe) = exe {
                result = Some(PinnedApp {
                    name: fallback_display.clone().unwrap_or_else(|| app.to_owned()),
                    launch_path: exe.clone(),
                    target_exe: Some(exe.clone()),
                    icon_source: exe,
                });
                break;
            }
        }
    }
    Some(result.unwrap_or_else(|| {
        let launch_path = format!("{APPS_FOLDER_PREFIX}{aumid}");
        PinnedApp {
            name: fallback_display.unwrap_or_else(|| app.to_owned()),
            launch_path: launch_path.clone(),
            target_exe: None,
            icon_source: launch_path,
        }
    }))
}

/// Imports packaged taskbar pins from the Explorer Taskband registry blob.
/// Windows 11 no longer creates `.lnk` files for Store apps, so this is the
/// only source for their AUMIDs. Missing/unreadable keys yield an empty list.
pub fn import_packaged_pins() -> Vec<PinnedApp> {
    let Some(blob) = taskband_favorites() else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut pins = Vec::new();
    for aumid in extract_aumids(&blob) {
        if !seen.insert(aumid.to_ascii_lowercase()) {
            continue;
        }
        if let Some(pin) = resolve_packaged_app(&aumid) {
            pins.push(pin);
        }
    }
    pins.sort_by_key(|a| a.name.to_lowercase());
    pins
}

pub fn load_pins() -> Vec<PinnedApp> {
    let dir = pins_dir();
    let entries = fs::read_dir(&dir).ok();
    // SAFETY: initialize COM STA for resolving `.lnk` shortcuts on this thread.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

    let mut items: Vec<PinnedApp> = entries
        .into_iter()
        .flatten()
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

            // Mirror reference ApplicationIconProvider.GetForPinnedApp priority:
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

    // Windows 11 stores packaged pins only in the Taskband blob; merge them
    // with `.lnk` pins and keep one row per launch target.
    let mut seen: HashSet<String> = items
        .iter()
        .map(|item| item.launch_path.to_ascii_lowercase())
        .collect();
    for pin in import_packaged_pins() {
        if seen.insert(pin.launch_path.to_ascii_lowercase()) {
            items.push(pin);
        }
    }
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

    #[test]
    fn manifest_application_resolves_display_name_and_executable() {
        let xml = r#"<Package xmlns="urn:test">
            <Properties><DisplayName>Example App</DisplayName></Properties>
            <Applications>
                <Application Id="Client" Executable="Client.exe">
                    <VisualElements DisplayName="Example Client"/>
                </Application>
            </Applications>
        </Package>"#;
        assert_eq!(
            manifest_application(xml, "client"),
            Some((
                Some("Example Client".to_owned()),
                Some("Client.exe".to_owned())
            ))
        );
        assert_eq!(manifest_application(xml, "Missing"), None);
    }

    #[test]
    fn packaged_pin_import_degrades_gracefully_without_taskband() {
        assert!(resolve_packaged_app("not-an-aumid").is_none());
        // No Explorer Taskband on CI: must return an empty list, never panic.
        let _ = import_packaged_pins();
    }
}
