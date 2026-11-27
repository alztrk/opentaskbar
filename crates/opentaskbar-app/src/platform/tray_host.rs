//! Sistem tepsisi keşfi, sabit pin kimliği ve `TaskbarCreated` yayını.
//!
//! Başvurular: `TrayHost.cs`, `TrayIconSource.cs`, `TrayIcons.cs`,
//! `TrayPinIdentity.cs`.
//!
//! Tamamlanan kısım: `HKCU\Control Panel\NotifyIconSettings` altındaki ikon
//! kayıtlarını (GUID alt anahtarı, ExecutablePath, InitialTooltip, UID)
//! okumak, KNOWNFOLDERID biçimindeki `{GUID}\relative\path` yollarını
//! `User Shell Folders` kayıt defterinden çözmek, `%DEĞİŞKEN%` genişletmek,
//! (GUID, path, UID) bazında tekrar ayıklamak ve sabit pin anahtarı üretmek.
//!
//! TODO (bilinçli olarak güvenli no-op): gerçek `Shell_NotifyIcon` host'u.
//! referans tarafı ManagedShell ile görünmez/topmost `Shell_TrayWnd` penceresi
//! açıp bildirim ikonu mesajlarını yakalar, UIPI izinlerini kontrol edip
//! tıklamaları sahiplerine iletir ve ABM paylaşımlı bellek çağrılarını
//! Explorer'a yönlendirir. Rust tarafında bu, ayrı bir Win32 mesaj pompası
//! ve `NotifyIcon` protokolünün yeniden uygulanmasını gerektirir; yanlış
//! yapılırsa tüm sistem tepsisi kaybolabilir. Bu yüzden `start_host()`/
//! `stop_host()` şimdilik no-op ve canlı ikon listesi registry keşfine
//! dayanıyor. `broadcast_taskbar_created()` gerçek ve etkindir.

#![cfg(windows)]

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use windows_sys::Win32::Foundation::{LPARAM, WPARAM};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ, RegCloseKey,
    RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegQueryValueExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, RegisterWindowMessageW, SendNotifyMessageW,
};

const SETTINGS_KEY: &str = r"Control Panel\NotifyIconSettings";
const USER_SHELL_FOLDERS: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders";

const SELF_BROADCAST_WPARAM: WPARAM = 1;
const RESTORE_BROADCAST_WPARAM: WPARAM = 2;

static LAST_SELF_SEQUENCE: AtomicI64 = AtomicI64::new(0);
static NEXT_SEQUENCE: AtomicI64 = AtomicI64::new(0);

/// Bir arka plan uygulamasının tepsisindeki ikonu temsil eder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayIconInfo {
    /// `pin_key()` ile üretilen sabit kimlik.
    pub key: String,
    /// Tek satırlık, temizlenmiş gösterim adı.
    pub name: String,
    /// Çözümlenmiş, genişletilmiş yürütülebilir dosya yolu.
    pub executable_path: String,
    /// Vista+ kayıt defteri alt anahtarından gelen GUID (D biçimi).
    pub guid: Option<String>,
    /// Aynı exe'den gelen birden fazla ikonu ayıran UID.
    pub uid: Option<u32>,
}

use crate::platform::win32_helpers::wide;

fn open_key(path: &str) -> Option<HKEY> {
    let path = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: `path` NUL-terminated; `key` is only used when the call succeeds.
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) };
    (status == 0).then_some(key)
}

/// Alt anahtar ve değer sayılarını döndürür (hata durumunda ikisi de 0).
fn key_counts(key: HKEY) -> (u32, u32) {
    let mut subkeys = 0u32;
    let mut values = 0u32;
    // SAFETY: tüm çıkış parametreleri isteğe bağlı; yalnızca iki sayaç isteniyor.
    let _ = unsafe {
        RegQueryInfoKeyW(
            key,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut subkeys,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut values,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (subkeys, values)
}

/// REG_SZ / REG_EXPAND_SZ değerini okur, EXPAND_SZ ise ortam değişkenlerini açar.
fn read_reg_string(key: HKEY, name: &str) -> Option<String> {
    let name = wide(name);
    let mut value_type = 0u32;
    let mut size = 0u32;
    // SAFETY: ilk çağrı yalnızca boyut/tip sorgular (lpdata = null).
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != 0
        || size == 0
        || !size.is_multiple_of(2)
        || size > 131_072
        || (value_type != REG_SZ && value_type != REG_EXPAND_SZ)
    {
        return None;
    }
    let mut bytes = vec![0u8; size as usize];
    // SAFETY: `bytes` yeterli boyutta ve çağrı boyunca geçerli.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            bytes.as_mut_ptr(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let words: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let end = words.iter().position(|&c| c == 0).unwrap_or(words.len());
    let raw = String::from_utf16_lossy(&words[..end]);
    Some(if value_type == REG_EXPAND_SZ {
        expand_env(&raw)
    } else {
        raw
    })
}

fn read_reg_u32(key: HKEY, name: &str) -> Option<u32> {
    let name = wide(name);
    let mut value = 0u32;
    let mut value_type = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: DWORD için 4 baytlık geçerli bir hedef gösteriliyor.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            (&mut value as *mut u32).cast::<u8>(),
            &mut size,
        )
    };
    if status != 0 || value_type != REG_DWORD || size != 4 {
        return None;
    }
    Some(value)
}

fn read_uid(key: HKEY) -> Option<u32> {
    for name in ["UID", "uID", "UId"] {
        if let Some(value) = read_reg_u32(key, name) {
            return Some(value);
        }
        if let Some(text) = read_reg_string(key, name)
            && let Ok(value) = text.trim().parse::<u32>()
        {
            return Some(value);
        }
    }
    None
}

/// `%USERPROFILE%` gibi değişkenleri açar; çözülemeyenler korunur.
fn expand_env(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('%') {
        output.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let variable = &after[..end];
                if let Ok(value) = std::env::var(variable) {
                    output.push_str(&value);
                    rest = &after[end + 1..];
                    continue;
                }
                output.push('%');
                rest = after;
            }
            _ => {
                output.push('%');
                rest = after;
            }
        }
    }
    output.push_str(rest);
    output
}

/// `HKCU\...\User Shell Folders` değerlerinden GUID -> yol eşlemesi kurar.
/// `SHGetKnownFolderPath` ek feature gerektirdiği için kayıt defteri kullanılır.
fn known_folders() -> HashMap<String, String> {
    let mut folders = HashMap::new();
    let Some(key) = open_key(USER_SHELL_FOLDERS) else {
        return folders;
    };
    let (_, value_count) = key_counts(key);
    for index in 0..value_count {
        let mut name = vec![0u16; 256];
        let mut name_len = name.len() as u32;
        // SAFETY: sağlanan tampon geçerli; durum kodu kontrol ediliyor.
        let status = unsafe {
            RegEnumValueW(
                key,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 || name_len == 0 {
            continue;
        }
        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        if let Some(path) = read_reg_string(key, &name)
            && !path.is_empty()
        {
            folders.insert(name.to_lowercase(), path);
        }
    }
    // SAFETY: `key` bu fonksiyonda bir kez açıldı ve bir kez kapatılır.
    unsafe { RegCloseKey(key) };
    folders
}

/// `{KNOWNFOLDERID}\göreli\yol` değerlerini çözümler; düz yolları genişletir.
fn resolve_path(raw: &str, folders: &HashMap<String, String>) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if raw.starts_with('{') {
        let Some(end) = raw.find('}') else {
            return String::new();
        };
        let Some(folder) = folders.get(&raw[..=end].to_lowercase()) else {
            return String::new();
        };
        let relative = raw[end + 1..].trim_start_matches(['\\', '/']);
        return if relative.is_empty() {
            folder.clone()
        } else {
            format!("{}\\{}", folder.trim_end_matches('\\'), relative)
        };
    }
    expand_env(raw)
}

/// GUID değerini `D` biçimine (8-4-4-4-12, küçük harf) getirir.
fn normalize_guid(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches(|c| c == '{' || c == '}');
    let hex: String = trimmed.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 || trimmed.chars().any(|c| !c.is_ascii_hexdigit() && c != '-') {
        return None;
    }
    let hex = hex.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// Bir kayıtlı pin anahtarını üretir: GUID varsa `guid:<D>`, yoksa `path:<küçük yol>`.
///
/// `TrayPinIdentity.KeyFor` path tabanlı anahtara `|uid` ekler; burada
/// istenen biçim korunur ve UID yalnızca `matches_pin()` içinde eski
/// `path:<yol>|<uid>` / `path:<yol>|*` anahtarlarıyla karşılaştırmada kullanılır.
pub fn pin_key(guid: Option<&str>, path: Option<&str>, uid: Option<u32>) -> String {
    let _ = uid;
    if let Some(guid) = guid.and_then(normalize_guid) {
        return format!("guid:{guid}");
    }
    let path = path.unwrap_or("").trim();
    if path.is_empty() {
        return String::new();
    }
    format!("path:{}", path.to_lowercase())
}

/// Kayıtlı bir pin anahtarının bu ikona ait olup olmadığını söyler.
///
/// GUID'li bir ikon, registry fallback'inden gelen path anahtarıyla da eşleşir
/// (`TrayPinIdentity.Matches` davranışı); eski UID'li anahtarlar desteklenir.
pub fn matches_pin(info: &TrayIconInfo, key: &str) -> bool {
    let key = key.trim();
    if key.is_empty() {
        return false;
    }
    if key.eq_ignore_ascii_case(&info.key) {
        return true;
    }
    let path = info.executable_path.trim().to_lowercase();
    if path.is_empty() {
        return false;
    }
    let path_key = format!("path:{path}");
    if key.eq_ignore_ascii_case(&path_key) {
        return true;
    }
    if let Some(uid) = info.uid
        && (key.eq_ignore_ascii_case(&format!("{path_key}|{uid}"))
            || key.eq_ignore_ascii_case(&format!("{path_key}|*")))
    {
        return true;
    }
    false
}

/// İpucundaki ilk anlamlı satırı alır, bidi kontrol karakterlerini temizler.
fn clean_title(raw: Option<&str>, path: &str) -> String {
    if let Some(title) = raw.and_then(|text| {
        text.split(['\r', '\n'])
            .find(|line| {
                !line
                    .trim_matches(|c: char| {
                        c.is_whitespace()
                            || matches!(c, '\u{202a}' | '\u{202c}' | '\u{200e}' | '\u{200f}')
                    })
                    .is_empty()
            })
            .map(|line| {
                line.trim_matches(|c: char| {
                    c.is_whitespace()
                        || matches!(c, '\u{202a}' | '\u{202c}' | '\u{200e}' | '\u{200f}')
                })
                .to_owned()
            })
    }) {
        return title;
    }
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("App")
        .to_owned()
}

/// `NotifyIconSettings` kayıtlarından çalışan/çalışmayan tüm arka plan ikonlarını okur.
///
/// Canlı `Shell_NotifyIcon` host'u olmadığı için ikonları çalışan süreçlerle
/// eşleştirme (`TrayIcons.RunningApps`) bu sürümde yoktur; kayıt defteri
/// anlık görüntüsü döndürülür. Tekrarlar (GUID, küçük yol, UID) üzerinden ayıklanır.
pub fn enumerate_tray_icons() -> Vec<TrayIconInfo> {
    let Some(root) = open_key(SETTINGS_KEY) else {
        return Vec::new();
    };
    let (subkey_count, _) = key_counts(root);
    let folders = known_folders();
    let mut icons = Vec::new();
    let mut seen = HashSet::new();

    for index in 0..subkey_count {
        let mut name = vec![0u16; 256];
        let mut name_len = name.len() as u32;
        // SAFETY: `name` çağrı boyunca geçerli; hata kodu kontrol ediliyor.
        let status = unsafe {
            RegEnumKeyExW(
                root,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 || name_len == 0 {
            continue;
        }
        let subkey = String::from_utf16_lossy(&name[..name_len as usize]);
        let Some(child) = open_key(&format!("{SETTINGS_KEY}\\{subkey}")) else {
            continue;
        };
        let raw_path = read_reg_string(child, "ExecutablePath");
        let tooltip = read_reg_string(child, "InitialTooltip");
        let uid = read_uid(child);
        // SAFETY: `child` bu döngü yinelemesinde bir kez açıldı.
        unsafe { RegCloseKey(child) };

        let Some(raw_path) = raw_path else {
            continue;
        };
        let executable_path = resolve_path(&raw_path, &folders);
        let file_name = Path::new(&executable_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if file_name.is_empty() || file_name.eq_ignore_ascii_case("explorer.exe") {
            continue;
        }
        let guid = normalize_guid(&subkey);
        if !seen.insert((guid.clone(), executable_path.to_lowercase(), uid)) {
            continue;
        }
        icons.push(TrayIconInfo {
            key: pin_key(guid.as_deref(), Some(&executable_path), uid),
            name: clean_title(tooltip.as_deref(), &executable_path),
            executable_path,
            guid,
            uid,
        });
    }

    // SAFETY: `root` bu fonksiyonda bir kez açıldı.
    unsafe { RegCloseKey(root) };
    icons.sort_by_key(|a| a.name.to_lowercase());
    icons
}

/// Starts real `Shell_NotifyIcon` hosting for the given executable paths.
/// Explorer's tray is left untouched; only dock-pinned apps get a second icon.
pub fn start_host(pinned_executables: &[String]) -> bool {
    super::tray_icon_host::start(pinned_executables)
}

/// Removes all hosted icons and stops the host thread.
pub fn stop_host() {
    super::tray_icon_host::stop();
}

pub fn is_running() -> bool {
    super::tray_icon_host::is_running()
}

fn next_sequence() -> i64 {
    NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
}

/// Tüm pencerelere `TaskbarCreated` yayınlar (`TrayHost.BroadcastTaskbarCreated`).
///
/// `wParam = 1` kendi yayınımızı, `lParam` içerilen sıra numarasını taşır;
/// `is_own_broadcast()` bu sıra ile kendi yayınımızı ayırt eder. İletim
/// `SendNotifyMessage` ile fire-and-forget yapılır; kilitli bir Explorer
/// kapatma yolunu bloke edemez.
pub fn broadcast_taskbar_created() {
    let message = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    if message == 0 {
        return;
    }
    let sequence = next_sequence();
    LAST_SELF_SEQUENCE.store(sequence, Ordering::Relaxed);
    // SAFETY: HWND_BROADCAST belgelenmiş bir sabit; wParam/lParam skaler.
    let _ = unsafe {
        SendNotifyMessageW(
            HWND_BROADCAST,
            message,
            SELF_BROADCAST_WPARAM,
            sequence as LPARAM,
        )
    };
}

/// Watchdog / `--restore` yolundan gelen yayın: kendi yayınımız olarak işaretlenmez.
pub fn broadcast_taskbar_created_restore() {
    let message = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    if message == 0 {
        return;
    }
    // SAFETY: HWND_BROADCAST belgelenmiş bir sabit; wParam/lParam skaler.
    let _ = unsafe {
        SendNotifyMessageW(
            HWND_BROADCAST,
            message,
            RESTORE_BROADCAST_WPARAM,
            next_sequence() as LPARAM,
        )
    };
}

/// `lParam` son kendi `TaskbarCreated` yayınımızın sıra numarasını taşıyorsa doğru.
pub fn is_own_broadcast(l_param: LPARAM) -> bool {
    let sequence = LAST_SELF_SEQUENCE.load(Ordering::Relaxed);
    sequence != 0 && l_param != 0 && sequence as isize == l_param
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID: &str = "6D2B4F40-1234-4ABC-9DEF-0123456789AB";

    #[test]
    fn pin_key_prefers_canonical_guid() {
        let key = pin_key(
            Some(&format!("{{{GUID}}}")),
            Some(r"C:\Apps\Tray.EXE"),
            Some(7),
        );
        assert_eq!(key, "guid:6d2b4f40-1234-4abc-9def-0123456789ab");
    }

    #[test]
    fn pin_key_falls_back_to_lowercase_path() {
        assert_eq!(
            pin_key(None, Some(r"C:\Apps\Tray.EXE"), Some(7)),
            r"path:c:\apps\tray.exe"
        );
        assert_eq!(pin_key(None, None, None), "");
        assert_eq!(
            pin_key(Some("not-a-guid"), Some("  X:\\A.EXE "), None),
            "path:x:\\a.exe"
        );
    }

    #[test]
    fn matches_pin_supports_guid_path_and_legacy_uid() {
        let info = TrayIconInfo {
            key: pin_key(Some(GUID), Some(r"C:\Apps\Tray.exe"), Some(7)),
            name: "Tray".to_owned(),
            executable_path: r"C:\Apps\Tray.exe".to_owned(),
            guid: Some(GUID.to_lowercase()),
            uid: Some(7),
        };
        assert!(matches_pin(
            &info,
            "guid:6d2b4f40-1234-4abc-9def-0123456789ab"
        ));
        assert!(matches_pin(&info, r"path:c:\apps\tray.exe"));
        assert!(matches_pin(
            &info,
            "GUID:6D2B4F40-1234-4ABC-9DEF-0123456789AB"
        ));
        assert!(matches_pin(&info, r"path:c:\apps\tray.exe|7"));
        assert!(matches_pin(&info, r"path:c:\apps\tray.exe|*"));
        assert!(!matches_pin(&info, r"path:c:\apps\other.exe"));
        assert!(!matches_pin(&info, r"path:c:\apps\tray.exe|8"));
        assert!(!matches_pin(&info, "  "));
    }

    #[test]
    fn path_only_icon_does_not_match_guid_key() {
        let info = TrayIconInfo {
            key: pin_key(None, Some(r"C:\Apps\Tray.exe"), None),
            name: "Tray".to_owned(),
            executable_path: r"C:\Apps\Tray.exe".to_owned(),
            guid: None,
            uid: None,
        };
        assert!(matches_pin(&info, r"path:c:\apps\tray.exe"));
        assert!(!matches_pin(
            &info,
            "guid:6d2b4f40-1234-4abc-9def-0123456789ab"
        ));
    }

    #[test]
    fn normalize_guid_rejects_junk() {
        assert!(normalize_guid("1234").is_none());
        assert!(normalize_guid("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_none());
        assert!(normalize_guid(GUID).is_some());
    }

    #[test]
    fn clean_title_takes_first_line_and_removes_bidi_marks() {
        let title = clean_title(
            Some("\u{202a} My App \u{202c}\r\nsecond line"),
            r"C:\Apps\fallback.exe",
        );
        assert_eq!(title, "My App");
        assert_eq!(
            clean_title(Some(" \n "), r"C:\Apps\fallback.exe"),
            "fallback"
        );
    }

    #[test]
    fn expand_env_keeps_unknown_variables() {
        assert_eq!(expand_env(r"C:\Apps\Tool.exe"), r"C:\Apps\Tool.exe");
        assert_eq!(
            expand_env(r"%OT_TEST_UNKNOWN_12345%\x"),
            r"%OT_TEST_UNKNOWN_12345%\x"
        );
    }

    #[test]
    fn resolve_path_handles_known_folders_and_unresolved_guids() {
        let mut folders = HashMap::new();
        folders.insert(
            "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}".to_owned(),
            r"C:\Users\Me\AppData".to_owned(),
        );
        assert_eq!(
            resolve_path(
                r"{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}\Local\app.exe",
                &folders
            ),
            r"C:\Users\Me\AppData\Local\app.exe"
        );
        assert_eq!(
            resolve_path(r"{11111111-2222-3333-4444-555555555555}\x.exe", &folders),
            ""
        );
    }
}
