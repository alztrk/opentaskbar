//! Shell tabanlı dosya işlemleri: Masaüstüne taşıma/kopyalama, Geri Dönüş Kutusu.
//!
//! C# referansı: `Services/Interop/DesktopFileTransfer.cs` ve `RecycleBin.cs`.
//! `SHFileOperationW` kullanıldığı için Explorer'ın Geri Al (Ctrl+Z), ilerleme
//! penceresi ve çakışma yeniden adlandırma davranışları korunur. Tüm çağrılar
//! `hwnd = 0` ile yapılır; böylece arka planda çalışan bir iş parçacığı
//! progress/conflict diyaloğunda kilitlenmez.

#![cfg(windows)]

use std::collections::HashSet;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::GetFullPathNameW;
use windows_sys::Win32::UI::Shell::{
    CSIDL_DESKTOPDIRECTORY, SHEmptyRecycleBinW, SHFILEOPSTRUCTW, SHFileOperationW,
    SHGetFolderPathW, SHQUERYRBINFO, SHQueryRecycleBinW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows_sys::core::PCWSTR;

const FO_MOVE: u32 = 0x0001;
const FO_COPY: u32 = 0x0002;
const FO_DELETE: u32 = 0x0003;
const FOF_RENAMEONCOLLISION: u16 = 0x0008;
const FOF_ALLOWUNDO: u16 = 0x0040;
const FOF_NOCONFIRMMKDIR: u16 = 0x0200;
const FOF_WANTNUKEWARNING: u16 = 0x4000;
const ERROR_CANCELLED: i32 = 0x04C7;
const E_UNEXPECTED_HRESULT: i32 = 0x8000_FFFFu32 as i32;
const CANCELLED_HRESULT: i32 = 0x8007_04C7u32 as i32;

/// Kullanıcının gerçek Masaüstü dizini (OneDrive yeniden yönlendirmesi dahil).
fn desktop_directory() -> Option<String> {
    let mut buffer = vec![0u16; 1024];
    // SAFETY: `SHGetFolderPathW` writes at most `buffer.len()` wide chars into our buffer.
    let hr = unsafe {
        SHGetFolderPathW(
            std::ptr::null_mut(),
            CSIDL_DESKTOPDIRECTORY as i32,
            std::ptr::null_mut(),
            0, // SHGFP_TYPE_CURRENT
            buffer.as_mut_ptr(),
        )
    };
    if hr == 0 {
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let path = String::from_utf16_lossy(&buffer[..end]);
        if !path.trim().is_empty() {
            return Some(path);
        }
    }
    // Son çare: profil altındaki varsayılan dizin.
    let home = std::env::var_os("USERPROFILE")?;
    Some(
        Path::new(&home)
            .join("Desktop")
            .to_string_lossy()
            .into_owned(),
    )
}

/// `Path.GetFullPath` + `TrimEndingDirectorySeparator` karşılığı; var olması gerekmez.
fn normalize_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let input: Vec<u16> = trimmed.encode_utf16().chain(Some(0)).collect();
    let mut buffer = vec![0u16; 260];
    loop {
        // SAFETY: `input` is NUL-terminated and `buffer` outlives the call; lpFilePart is optional.
        let needed = unsafe {
            GetFullPathNameW(
                input.as_ptr(),
                buffer.len() as u32,
                buffer.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if needed == 0 {
            return None;
        }
        if needed as usize <= buffer.len() {
            let mut path = String::from_utf16_lossy(&buffer[..needed as usize]);
            while path.len() > 1 && (path.ends_with('\\') || path.ends_with('/')) {
                let without_separator = &path[..path.len() - 1];
                if is_drive_root(without_separator) {
                    break;
                }
                path = without_separator.to_owned();
            }
            return Some(path);
        }
        buffer.resize(needed as usize + 1, 0);
    }
}

fn root_of(path: &str) -> Option<String> {
    let root = Path::new(path).ancestors().last()?;
    let text = root.to_string_lossy();
    let trimmed = text.trim_end_matches(['\\', '/']);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn is_drive_root(path: &str) -> bool {
    let trimmed = path.trim_end_matches(['\\', '/']);
    root_of(trimmed).is_some_and(|root| trimmed.eq_ignore_ascii_case(&root))
}

fn eq_ignore_ascii_case(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// `child`, `parent` ile aynı sürücü/dizin ağacındaysa ve onun altındaysa doğru.
fn is_under(parent: &str, child: &str) -> bool {
    child.len() > parent.len()
        && child
            .as_bytes()
            .get(parent.len())
            .is_some_and(|b| *b == b'\\' || *b == b'/')
        && child[..parent.len()].eq_ignore_ascii_case(parent)
}

fn parent_of(path: &str) -> Option<String> {
    let parent = Path::new(path).parent()?;
    Some(
        parent
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_owned(),
    )
}

/// Sürükle-bırak yollarını Masaüstü aktarımı için filtreler ve normalize eder.
///
/// - Var olmayan, boş, sürücü kökü, tekrar eden yollar atılır.
/// - Masaüstünün kendisi veya bir üst dizini asla içine taşınmaz.
/// - Taşımada Masaüstünün doğrudan çocuğu zaten orada olduğu için atlanır;
///   kopyalamada çakışma adıyla yeni bir kopya oluşması için korunur.
fn filter_transfer_paths(paths: &[String], desktop: &str, copy: bool) -> Vec<String> {
    let Some(normalized_desktop) = normalize_path(desktop) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for candidate in paths {
        let Some(full) = normalize_path(candidate) else {
            continue;
        };
        if !seen.insert(full.to_lowercase()) || is_drive_root(&full) {
            continue;
        }
        if std::fs::metadata(&full).is_err() {
            continue;
        }
        if eq_ignore_ascii_case(&full, &normalized_desktop) || is_under(&full, &normalized_desktop)
        {
            continue;
        }
        if !copy
            && parent_of(&full)
                .is_some_and(|parent| eq_ignore_ascii_case(&parent, &normalized_desktop))
        {
            continue;
        }
        result.push(full);
    }
    result
}

/// SHFileOperationW için çift NUL ile sonlandırılmış UTF-16 tamponu kurar.
fn double_null(values: &[String]) -> Vec<u16> {
    let mut buffer = Vec::new();
    for value in values {
        buffer.extend(value.encode_utf16());
        buffer.push(0);
    }
    buffer.push(0);
    buffer
}

fn run_file_operation(from: &[u16], to: Option<&[u16]>, function: u32, flags: u16) -> (i32, bool) {
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: function,
        pFrom: from.as_ptr(),
        pTo: to.map_or(std::ptr::null(), |buffer| buffer.as_ptr()),
        fFlags: flags,
        ..Default::default()
    };
    // SAFETY: operation buffers outlive this synchronous call; hwnd = 0 avoids cross-thread modality.
    let result = unsafe { SHFileOperationW(&mut operation) };
    (result, operation.fAnyOperationsAborted != 0)
}

/// Dosyaları/klasörleri Explorer üzerinden Masaüstüne taşır veya kopyalar.
///
/// `Ok(())` işlemin tamamlandığı (veya filtre sonrası yapılacak iş kalmadığı)
/// anlamına gelir. Kullanıcı iptal ederse `"cancelled"`, shell hatasında
/// hex hata kodu döner.
pub fn transfer_to_desktop(paths: &[String], copy: bool) -> Result<(), String> {
    let Some(desktop) = desktop_directory() else {
        return Err("ErrorDesktopDropFailed".to_owned());
    };
    let filtered = filter_transfer_paths(paths, &desktop, copy);
    if filtered.is_empty() {
        return Ok(());
    }
    let from = double_null(&filtered);
    let to = double_null(&[desktop]);
    let flags = FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR | FOF_RENAMEONCOLLISION;
    let (result, aborted) = run_file_operation(
        &from,
        Some(&to),
        if copy { FO_COPY } else { FO_MOVE },
        flags,
    );
    if result == 0 && !aborted {
        Ok(())
    } else if aborted || result == ERROR_CANCELLED {
        Err("cancelled".to_owned())
    } else {
        Err(format!("SHFileOperation failed: 0x{result:X}"))
    }
}

/// Masaüstü klasörünü varsayılan kabuk uygulamasıyla (Explorer) açar.
pub fn open_desktop_folder() -> bool {
    let Some(desktop) = desktop_directory() else {
        return false;
    };
    if std::fs::metadata(&desktop).is_err() {
        return false;
    }
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let path: Vec<u16> = desktop.encode_utf16().chain(Some(0)).collect();
    // SAFETY: both strings are NUL-terminated and alive for the duration of the call.
    let instance = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr() as PCWSTR,
            path.as_ptr() as PCWSTR,
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    (instance as isize) > 32
}

/// Tüm sürücülerdeki Geri Dönüş Kutusu öğe sayısı; shell sorgulayamazsa `None`.
pub fn recycle_bin_count() -> Option<u32> {
    let mut info = SHQUERYRBINFO {
        cbSize: size_of::<SHQUERYRBINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is initialized with the correct cbSize and outlives the call.
    let hr = unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) };
    if hr != 0 {
        return None;
    }
    Some(info.i64NumItems.clamp(0, u32::MAX as i64) as u32)
}

/// Geri Dönüş Kutusunu boşaltır. Windows'un onay diyaloğu korunur.
///
/// Kullanıcı onaylamadığında veya kutu zaten boş olduğunda shell'in verdiği
/// `E_UNEXPECTED` / iptal sonuçları da hata sayılmaz ve `true` döner.
pub fn empty_recycle_bin() -> bool {
    // SAFETY: null owner and null root path are valid; all drives are emptied.
    let hr = unsafe { SHEmptyRecycleBinW(std::ptr::null_mut(), std::ptr::null(), 0) };
    matches!(hr, 0 | E_UNEXPECTED_HRESULT | CANCELLED_HRESULT)
}

/// Yolları Explorer üzerinden Geri Dönüş Kutusuna taşır.
///
/// `FOF_ALLOWUNDO` kalıcı silme yerine geri dönüş kutusunu zorunlu kılar;
/// `FOF_WANTNUKEWARNING` kutuya sığmayan dosyalarda uyarı gösterir.
/// İptal (`aborted`) hata değil, `true` olarak raporlanır (C# `RecycleBin.Delete` ile aynı).
pub fn delete_to_recycle_bin(paths: &[String]) -> bool {
    let normalized: Vec<String> = paths.iter().filter_map(|p| normalize_path(p)).collect();
    if normalized.is_empty() {
        return true;
    }
    let from = double_null(&normalized);
    let (result, aborted) =
        run_file_operation(&from, None, FO_DELETE, FOF_ALLOWUNDO | FOF_WANTNUKEWARNING);
    result == 0 || aborted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn test_root(tag: &str) -> std::path::PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ot-shell-files-{}-{tag}-{id}", std::process::id()))
    }

    #[test]
    fn double_null_layout() {
        let buffer = double_null(&["C:\\a".to_owned(), "C:\\b".to_owned()]);
        assert_eq!(buffer.last(), Some(&0));
        assert_eq!(buffer.get(buffer.len() - 2), Some(&0));
        assert!(buffer.windows(2).any(|w| w == [0, 0]));
        assert!(buffer.iter().take(buffer.len() - 2).any(|&c| c == 0));
    }

    #[test]
    fn filter_skips_missing_duplicates_and_desktop_self() {
        let root = test_root("basic");
        let desktop = root.join("Desktop");
        let file = desktop.join("existing.txt");
        std::fs::create_dir_all(&desktop).unwrap();
        std::fs::write(&file, b"x").unwrap();
        let desktop_s = desktop.to_string_lossy().into_owned();
        let file_s = file.to_string_lossy().into_owned();

        let filtered = filter_transfer_paths(
            &[
                file_s.clone(),
                file_s.clone(),
                desktop_s.clone(),
                root.join("missing.txt").to_string_lossy().into_owned(),
                String::new(),
            ],
            &desktop_s,
            true,
        );
        assert_eq!(filtered, vec![normalize_path(&file_s).unwrap()]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn filter_skips_ancestor_and_direct_child_on_move() {
        let root = test_root("ancestor");
        let desktop = root.join("Desktop");
        let child = desktop.join("child.txt");
        let outside = root.join("outside.txt");
        std::fs::create_dir_all(&desktop).unwrap();
        std::fs::write(&child, b"x").unwrap();
        std::fs::write(&outside, b"x").unwrap();
        let desktop_s = desktop.to_string_lossy().into_owned();
        let root_s = root.to_string_lossy().into_owned();
        let child_s = child.to_string_lossy().into_owned();
        let outside_s = outside.to_string_lossy().into_owned();

        let moved = filter_transfer_paths(
            &[root_s, child_s.clone(), outside_s.clone()],
            &desktop_s,
            false,
        );
        assert_eq!(moved, vec![normalize_path(&outside_s).unwrap()]);

        let copied = filter_transfer_paths(&[child_s], &desktop_s, true);
        assert_eq!(copied.len(), 1);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn drive_roots_are_skipped() {
        let root = test_root("roots");
        std::fs::create_dir_all(&root).unwrap();
        let desktop_s = root.to_string_lossy().into_owned();
        let filtered = filter_transfer_paths(&["C:\\".to_owned()], &desktop_s, true);
        assert!(filtered.is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn under_checks_are_case_insensitive() {
        assert!(is_under("C:\\Users\\Me", "c:\\users\\me\\Desktop"));
        assert!(!is_under("C:\\Users\\Me", "C:\\Users\\Me2"));
        assert!(!is_under("C:\\Users\\Me", "C:\\Users\\Me"));
    }
}
