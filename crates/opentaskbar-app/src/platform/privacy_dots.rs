//! N9: camera / microphone privacy dots and quick power & capture actions.
//!
//! Privacy state comes from the Capability Access Manager consent store
//! (`HKCU\...\CapabilityAccessManager\ConsentStore\{webcam,microphone}`), the same data the
//! Windows privacy indicator uses. An app is using the device while its subkey has
//! `LastUsedTimeStart > 0` and `LastUsedTimeStop == 0`. Both packaged app subkeys and the
//! `NonPackaged` subtree are scanned.
//!
//! [`PrivacyProbe::poll`] makes zero registry calls while disabled and rescans at most every
//! 1.5 s; every opened `HKEY` is closed by an RAII guard. No threads, no notifications.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FreeLibrary};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_QWORD, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    RegQueryValueExW,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::win32_helpers::wide;

/// Minimum time between two consent-store scans.
const SCAN_INTERVAL: Duration = Duration::from_millis(1500);
const CONSENT_STORE: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";
const NON_PACKAGED: &str = "NonPackaged";
/// Hard cap on subkeys visited per capability level (the store only grows with installs).
const MAX_SUBKEYS: u32 = 512;
const MAX_APP_NAME_CHARS: usize = 48;

#[link(name = "user32")]
unsafe extern "system" {
    fn LockWorkStation() -> i32;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrivacyState {
    pub camera_active: bool,
    pub mic_active: bool,
    pub camera_app: Option<String>,
    pub mic_app: Option<String>,
}

impl PrivacyState {
    /// `bit 0` = microphone, `bit 1` = camera.
    pub fn bitmask(&self) -> u8 {
        u8::from(self.mic_active) | (u8::from(self.camera_active) << 1)
    }
}

#[derive(Debug, Default)]
pub struct PrivacyProbe {
    last_scan: Option<Instant>,
    state: PrivacyState,
}

impl PrivacyProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current camera/microphone use. Disabled: returns the default state with no registry
    /// access and drops the cached app names.
    pub fn poll(&mut self, enabled: bool) -> PrivacyState {
        self.poll_with(enabled, Instant::now(), scan_consent_store)
    }

    fn poll_with(
        &mut self,
        enabled: bool,
        now: Instant,
        scan: impl FnOnce() -> PrivacyState,
    ) -> PrivacyState {
        if !enabled {
            if self.last_scan.is_some() {
                *self = Self::default();
            }
            return PrivacyState::default();
        }
        let due = self
            .last_scan
            .is_none_or(|last| now.saturating_duration_since(last) >= SCAN_INTERVAL);
        if due {
            self.last_scan = Some(now);
            self.state = scan();
        }
        self.state.clone()
    }
}

/// Locks the workstation (same as Win+L).
pub fn lock_workstation() -> bool {
    // SAFETY: no arguments; returns nonzero on success.
    unsafe { LockWorkStation() != 0 }
}

/// Puts the system to sleep via `powrprof!SetSuspendState(FALSE, FALSE, FALSE)`.
///
/// Loaded dynamically so the binary needs no `powrprof.lib` import. Returns after resume.
pub fn sleep_system() -> bool {
    type SetSuspendStateFn = unsafe extern "system" fn(u8, u8, u8) -> u8;
    let name = wide("powrprof.dll");
    // SAFETY: NUL-terminated system DLL name.
    let module = unsafe { LoadLibraryW(name.as_ptr()) };
    if module.is_null() {
        return false;
    }
    // SAFETY: `module` is loaded and the export name is NUL-terminated.
    let proc = unsafe { GetProcAddress(module, c"SetSuspendState".as_ptr().cast()) };
    let result = match proc {
        Some(proc) => {
            // SAFETY: `SetSuspendState(BOOLEAN, BOOLEAN, BOOLEAN) -> BOOLEAN`.
            let set_suspend_state = unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, SetSuspendStateFn>(proc)
            };
            // SAFETY: plain value arguments.
            unsafe { set_suspend_state(0, 0, 0) != 0 }
        }
        None => false,
    };
    // SAFETY: balances `LoadLibraryW`; the function pointer is not used afterwards.
    unsafe { FreeLibrary(module) };
    result
}

/// Opens the Windows screen-snip overlay (`ms-screenclip:`), falling back to Snipping Tool.
pub fn launch_windows_snipping() -> bool {
    shell_open("ms-screenclip:", None) || shell_open("SnippingTool.exe", Some("/clip"))
}

fn shell_open(target: &str, parameters: Option<&str>) -> bool {
    let verb = wide("open");
    let file = wide(target);
    let params = parameters.map(wide);
    // SAFETY: all strings are NUL-terminated and outlive the call.
    let instance = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute reports success with a value greater than 32.
    instance as isize > 32
}

/// Owned registry key closed on drop.
struct RegKey(HKEY);

impl RegKey {
    fn open(parent: HKEY, path: &str) -> Option<Self> {
        let path = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `path` is NUL-terminated and `key` is a valid out pointer.
        let status = unsafe { RegOpenKeyExW(parent, path.as_ptr(), 0, KEY_READ, &mut key) };
        (status == ERROR_SUCCESS && !key.is_null()).then_some(Self(key))
    }

    /// Visits subkey names until `visit` returns `true` or the cap is reached.
    fn any_subkey(&self, mut visit: impl FnMut(&str) -> bool) -> bool {
        let mut buffer = [0u16; 256];
        for index in 0..MAX_SUBKEYS {
            let mut len = buffer.len() as u32;
            // SAFETY: `buffer` holds `len` UTF-16 units; unused out pointers are null.
            let status = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    buffer.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status != ERROR_SUCCESS {
                // Name too long or a transient error: skip this entry.
                continue;
            }
            let len = (len as usize).min(buffer.len());
            let name = String::from_utf16_lossy(&buffer[..len]);
            if visit(&name) {
                return true;
            }
        }
        false
    }

    fn qword(&self, value: &str) -> u64 {
        let value = wide(value);
        let mut kind = 0u32;
        let mut data = 0u64;
        let mut size = std::mem::size_of::<u64>() as u32;
        // SAFETY: `data` is 8 writable bytes and `size` says so.
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                value.as_ptr(),
                std::ptr::null(),
                &mut kind,
                (&raw mut data).cast::<u8>(),
                &mut size,
            )
        };
        if status == ERROR_SUCCESS && kind == REG_QWORD && size == 8 {
            data
        } else {
            0
        }
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: the key was opened by `RegOpenKeyExW` and is closed exactly once.
        unsafe { RegCloseKey(self.0) };
    }
}

fn is_active(start: u64, stop: u64) -> bool {
    start > 0 && stop == 0
}

fn subkey_active(parent: &RegKey, name: &str) -> bool {
    RegKey::open(parent.0, name).is_some_and(|key| {
        is_active(
            key.qword("LastUsedTimeStart"),
            key.qword("LastUsedTimeStop"),
        )
    })
}

/// First app currently using `capability` (`"webcam"` / `"microphone"`).
fn active_app(capability: &str) -> Option<String> {
    let root = RegKey::open(HKEY_CURRENT_USER, &format!(r"{CONSENT_STORE}\{capability}"))?;
    let mut found: Option<String> = None;
    root.any_subkey(|name| {
        if name.eq_ignore_ascii_case(NON_PACKAGED) {
            if let Some(non_packaged) = RegKey::open(root.0, NON_PACKAGED) {
                non_packaged.any_subkey(|exe| {
                    let hit = subkey_active(&non_packaged, exe);
                    if hit {
                        found = Some(friendly_app_name(exe));
                    }
                    hit
                });
            }
            found.is_some()
        } else {
            let hit = subkey_active(&root, name);
            if hit {
                found = Some(friendly_app_name(name));
            }
            hit
        }
    });
    found
}

fn scan_consent_store() -> PrivacyState {
    let camera_app = active_app("webcam");
    let mic_app = active_app("microphone");
    PrivacyState {
        camera_active: camera_app.is_some(),
        mic_active: mic_app.is_some(),
        camera_app: camera_app.filter(|name| !name.is_empty()),
        mic_app: mic_app.filter(|name| !name.is_empty()),
    }
}

/// Consent-store subkey -> short display name.
///
/// - `C:#Program Files#Zoom#bin#Zoom.exe` -> `Zoom`
/// - `Microsoft.WindowsCamera_8wekyb3d8bbwe` -> `WindowsCamera`
fn friendly_app_name(key: &str) -> String {
    let key = key.trim();
    let name = if key.contains('#') || key.contains('\\') {
        let file = key.rsplit(['#', '\\', '/']).next().unwrap_or(key);
        let lower = file.to_ascii_lowercase();
        if lower.ends_with(".exe") {
            &file[..file.len() - 4]
        } else {
            file
        }
    } else {
        // Package family name: `Name_PublisherId`; package names never contain `_`.
        let package = key.split('_').next().unwrap_or(key);
        package
            .rsplit('.')
            .find(|segment| !segment.is_empty())
            .unwrap_or(package)
    };
    let name = name.trim();
    if name.chars().count() <= MAX_APP_NAME_CHARS {
        name.to_owned()
    } else {
        let mut out: String = name.chars().take(MAX_APP_NAME_CHARS - 1).collect();
        out.push('\u{2026}');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmask_encodes_mic_and_camera() {
        let mut state = PrivacyState::default();
        assert_eq!(state.bitmask(), 0);
        state.mic_active = true;
        assert_eq!(state.bitmask(), 0b01);
        state.camera_active = true;
        assert_eq!(state.bitmask(), 0b11);
        state.mic_active = false;
        assert_eq!(state.bitmask(), 0b10);
    }

    #[test]
    fn friendly_names_are_normalized() {
        assert_eq!(
            friendly_app_name("C:#Program Files#Zoom#bin#Zoom.exe"),
            "Zoom"
        );
        assert_eq!(friendly_app_name(r"C:\Tools\OBS64.EXE"), "OBS64");
        assert_eq!(
            friendly_app_name("Microsoft.WindowsCamera_8wekyb3d8bbwe"),
            "WindowsCamera"
        );
        assert_eq!(
            friendly_app_name("5319275A.WhatsAppDesktop_cv1g1gvanyjgm"),
            "WhatsAppDesktop"
        );
        assert_eq!(friendly_app_name("Discord"), "Discord");
        let long = format!("C:#{}.exe", "a".repeat(300));
        assert!(friendly_app_name(&long).chars().count() <= MAX_APP_NAME_CHARS);
    }

    #[test]
    fn activity_rule() {
        assert!(is_active(133_000_000_000_000_000, 0));
        assert!(!is_active(0, 0));
        assert!(!is_active(5, 6));
    }

    #[test]
    fn disabled_fast_path_never_scans() {
        let mut probe = PrivacyProbe::new();
        let state = probe.poll_with(false, Instant::now(), || panic!("must not scan"));
        assert_eq!(state, PrivacyState::default());
        assert!(probe.last_scan.is_none());
        assert_eq!(probe.poll(false), PrivacyState::default());
    }

    #[test]
    fn scans_are_throttled_and_disable_clears_cache() {
        let mut probe = PrivacyProbe::new();
        let start = Instant::now();
        let active = PrivacyState {
            camera_active: true,
            mic_active: false,
            camera_app: Some("Zoom".to_owned()),
            mic_app: None,
        };
        let scans = std::cell::Cell::new(0u32);
        let scan = || {
            scans.set(scans.get() + 1);
            active.clone()
        };
        assert_eq!(probe.poll_with(true, start, scan), active);
        assert_eq!(
            probe.poll_with(true, start + Duration::from_millis(500), scan),
            active
        );
        assert_eq!(scans.get(), 1);
        probe.poll_with(true, start + SCAN_INTERVAL, scan);
        assert_eq!(scans.get(), 2);

        assert_eq!(
            probe.poll_with(false, start + SCAN_INTERVAL * 2, || unreachable!()),
            PrivacyState::default()
        );
        assert!(probe.last_scan.is_none());
        assert_eq!(probe.state, PrivacyState::default());
    }
}
