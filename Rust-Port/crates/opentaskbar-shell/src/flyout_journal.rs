//! Shell flyout (Start / Search / Control Center) pozisyon journal'ı.
//!
//! C# referansı: `Services/Interop/ShellFlyoutPositionJournal.cs` ve
//! `ShellFlyoutPositionRegistry.cs`. Normal çıkış journal'ı okuyup siler;
//! sert kapanmada dosya kalır ve yerel watchdog geri yüklemeyi tamamlar.
//!
//! Dosya `%LOCALAPPDATA%\OpenTaskbar\flyout-positions.txt`, satır başına
//! `handle pid left top` biçimindedir. Yazma her zaman `*.tmp` dosyasına
//! yapılıp atomik rename ile değiştirilir; böylece yarı yazılmış journal
//! bir sonraki oturumda yanlış geri yüklemeye yol açmaz.
//!
//! Modülün çekirdeği tamamen std'dir. `read_valid()`/`is_current()` yalnızca
//! Windows derlemelerinde HWND'nin hâlâ aynı PID'e ait olduğunu doğrular
//! (Windows HWND'leri geri dönüştürülebilir).

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[cfg(windows)]
use windows_sys::Win32::Foundation::HWND;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};

/// Windows'un flyout'a verdiği orijinal konum ve o anki sahibi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlyoutPosition {
    pub handle: usize,
    pub pid: u32,
    pub left: i32,
    pub top: i32,
}

const MAX_ENTRIES: usize = 16;

/// Tag-keyed positions for our own flyouts (dock flyouts are not shell flyouts, so the
/// HWND/PID journal above cannot describe them). Stored next to the C#-compatible journal.
fn tag_journal_path() -> PathBuf {
    journal_path().with_file_name("dock-flyout-positions.txt")
}

/// Remembers the last position of a dock flyout (`tray`, `clock`, `audio`, ...).
pub fn write_tag(tag: &str, left: i32, top: i32) {
    if tag.is_empty() || tag.contains(|c: char| c.is_whitespace()) {
        return;
    }
    let path = tag_journal_path();
    let mut entries: Vec<(String, i32, i32)> = read_tags_inner(&path);
    if let Some(entry) = entries.iter_mut().find(|(name, _, _)| name == tag) {
        entry.1 = left;
        entry.2 = top;
    } else {
        if entries.len() >= MAX_ENTRIES {
            entries.remove(0);
        }
        entries.push((tag.to_owned(), left, top));
    }
    let mut text = String::new();
    for (name, x, y) in entries {
        text.push_str(&format!("{name} {x} {y}\n"));
    }
    let tmp = tmp_path(&path);
    if fs::write(&tmp, text).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

fn read_tags_inner(path: &Path) -> Vec<(String, i32, i32)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let tag = parts.next()?.to_owned();
            let left = i32::from_str(parts.next()?).ok()?;
            let top = i32::from_str(parts.next()?).ok()?;
            Some((tag, left, top))
        })
        .take(MAX_ENTRIES)
        .collect()
}

/// Returns the remembered position for a dock flyout, if any.
pub fn read_tag(tag: &str) -> Option<(i32, i32)> {
    read_tags_inner(&tag_journal_path())
        .into_iter()
        .find(|(name, _, _)| name == tag)
        .map(|(_, left, top)| (left, top))
}

/// Journal dosyasının yolu (`LOCALAPPDATA` yoksa geçici dizin).
pub fn journal_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("OpenTaskbar")
        .join("flyout-positions.txt")
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    file_name.push(".tmp");
    path.with_file_name(file_name)
}

/// Bir pozisyonu ekler ya da aynı HWND için günceller ve journal'ı yeniden yazar.
pub fn write(handle: usize, pid: u32, left: i32, top: i32) {
    if handle == 0 {
        return;
    }
    let mut positions = read();
    let updated = FlyoutPosition {
        handle,
        pid,
        left,
        top,
    };
    if let Some(existing) = positions.iter_mut().find(|p| p.handle == handle) {
        *existing = updated;
    } else if positions.len() < MAX_ENTRIES {
        positions.push(updated);
    }
    write_all(&positions);
}

/// Tüm pozisyonları tek seferde yazar (en fazla 16 girdi tutulur).
pub fn write_all(positions: &[FlyoutPosition]) {
    write_all_to(&journal_path(), positions);
}

fn write_all_to(path: &Path, positions: &[FlyoutPosition]) {
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut content = String::new();
    for position in positions.iter().take(MAX_ENTRIES) {
        if position.handle == 0 {
            continue;
        }
        content.push_str(&format!(
            "{} {} {} {}\n",
            position.handle, position.pid, position.left, position.top
        ));
    }
    let temporary = tmp_path(path);
    if fs::write(&temporary, content).is_err() {
        return;
    }
    // `std::fs::rename` Windows'ta hedefi MOVEFILE_REPLACE_EXISTING ile değiştirir;
    // yine de eski sürüm davranışına karşı yedek yolu dene.
    if fs::rename(&temporary, path).is_err() && path.exists() && fs::remove_file(path).is_ok() {
        let _ = fs::rename(&temporary, path);
    }
}

/// Journal'daki tüm geçerli pozisyonları okur (bozuk satırlar atlanır).
pub fn read() -> Vec<FlyoutPosition> {
    read_from(&journal_path())
}

fn read_from(path: &Path) -> Vec<FlyoutPosition> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut positions = Vec::with_capacity(MAX_ENTRIES);
    for line in raw.lines().take(MAX_ENTRIES) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 4 {
            continue;
        }
        let (Some(handle), Some(pid), Some(left), Some(top)) = (
            parse_unsigned(fields[0]),
            parse_unsigned(fields[1]),
            parse_signed(fields[2]),
            parse_signed(fields[3]),
        ) else {
            continue;
        };
        // HWND 0 hiçbir zaman geri yüklenemez; C# journal'ı da bunu atar.
        if handle != 0 {
            positions.push(FlyoutPosition {
                handle,
                pid,
                left,
                top,
            });
        }
    }
    positions
}

fn parse_unsigned<T: FromStr>(text: &str) -> Option<T> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_signed(text: &str) -> Option<i32> {
    let digits = text.strip_prefix(|c| c == '+' || c == '-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Journal dosyasını siler (normal çıkışta geri yükleme sonrası çağrılır).
pub fn clear() {
    clear_from(&journal_path());
}

fn clear_from(path: &Path) {
    let _ = fs::remove_file(path);
}

/// HWND hâlâ yaşıyor ve journal'a kaydedilen PID'e hâlâ aitse doğru.
#[cfg(windows)]
pub fn is_current(position: &FlyoutPosition) -> bool {
    if position.handle == 0 || position.pid == 0 {
        return false;
    }
    let hwnd = position.handle as HWND;
    // SAFETY: `IsWindow` ve `GetWindowThreadProcessId` herhangi bir HWND değeri için güvenlidir.
    unsafe {
        if IsWindow(hwnd) == 0 {
            return false;
        }
        let mut current_pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut current_pid);
        current_pid == position.pid
    }
}

/// Sadece hâlâ geçerli (aynı PID'e ait) flyout konumlarını döndürür.
#[cfg(windows)]
pub fn read_valid() -> Vec<FlyoutPosition> {
    read().into_iter().filter(is_current).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn test_path(tag: &str) -> PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "ot-flyout-journal-{}-{tag}-{id}.txt",
            std::process::id()
        ))
    }

    #[test]
    fn round_trip_and_upsert() {
        let path = test_path("round-trip");
        clear_from(&path);
        assert!(read_from(&path).is_empty());

        write_all_to(
            &path,
            &[
                FlyoutPosition {
                    handle: 1234,
                    pid: 56,
                    left: -320,
                    top: 17,
                },
                FlyoutPosition {
                    handle: 9876,
                    pid: 42,
                    left: 0,
                    top: 1080,
                },
            ],
        );
        let positions = read_from(&path);
        assert_eq!(positions.len(), 2);
        assert_eq!(
            positions[0],
            FlyoutPosition {
                handle: 1234,
                pid: 56,
                left: -320,
                top: 17
            }
        );
        assert_eq!(positions[1].top, 1080);

        // Aynı HWND güncellenir, yeni girdi eklenmez.
        write_all_to(
            &path,
            &[
                FlyoutPosition {
                    handle: 1234,
                    pid: 56,
                    left: 10,
                    top: 20,
                },
                FlyoutPosition {
                    handle: 9876,
                    pid: 42,
                    left: 0,
                    top: 1080,
                },
            ],
        );
        assert_eq!(read_from(&path)[0].left, 10);
        clear_from(&path);
        assert!(read_from(&path).is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let path = test_path("malformed");
        let bad = "bogus line\n\
                   0 1 2 3\n\
                   12 not-a-pid 4 5\n\
                   12 7 8 not-a-number\n\
                   -1 7 8 9\n\
                   42 7 -16 24\n";
        fs::write(&path, bad).unwrap();
        let positions = read_from(&path);
        assert_eq!(positions.len(), 1);
        assert_eq!(
            positions[0],
            FlyoutPosition {
                handle: 42,
                pid: 7,
                left: -16,
                top: 24
            }
        );
        clear_from(&path);
    }

    #[test]
    fn write_replaces_via_temporary_file() {
        let path = test_path("atomic");
        clear_from(&path);
        write_all_to(
            &path,
            &[FlyoutPosition {
                handle: 7,
                pid: 8,
                left: 9,
                top: 10,
            }],
        );
        assert!(path.exists());
        assert!(!tmp_path(&path).exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "7 8 9 10\n");
        clear_from(&path);
    }

    #[test]
    fn entry_count_is_capped() {
        let path = test_path("cap");
        let positions: Vec<FlyoutPosition> = (1..=20)
            .map(|i| FlyoutPosition {
                handle: i,
                pid: 1,
                left: 0,
                top: 0,
            })
            .collect();
        write_all_to(&path, &positions);
        assert_eq!(read_from(&path).len(), MAX_ENTRIES);
        clear_from(&path);
    }
}
