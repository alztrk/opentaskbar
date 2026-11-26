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

/// C# `ShellFlyoutPositionRegistry.Generation` and the per-record sequence.
/// The native watchdog only consumes the first four fields of a position line,
/// so these tokens are appended by the opt-in `write_all_to_with_meta` writer;
/// the reader accepts both the legacy four-field form and the extended one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JournalMeta {
    pub generation: u64,
    pub seq: u64,
}

/// The C# flyout kinds whose intent journal carries `generation`/`seq` tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlyoutKind {
    Start,
    Search,
    ControlCenter,
}

impl FlyoutKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "START",
            Self::Search => "SEARCH",
            Self::ControlCenter => "CONTROL_CENTER",
        }
    }
}

/// One `START generation=42 seq=7` style journal line (C# parity).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlyoutKindRecord {
    pub kind: FlyoutKind,
    pub generation: u32,
    pub seq: u32,
}

impl FlyoutKindRecord {
    /// Serializes the C# wire form, e.g. `CONTROL_CENTER generation=9 seq=3`.
    pub fn to_line(self) -> String {
        format!(
            "{} generation={} seq={}",
            self.kind.as_str(),
            self.generation,
            self.seq
        )
    }

    /// Parses the C# wire form. Unknown keys, missing fields and extra tokens
    /// are rejected so a truncated journal line never restores stale metadata.
    #[must_use]
    pub fn from_line(line: &str) -> Option<Self> {
        let mut words = line.split_whitespace();
        let kind = match words.next()? {
            "START" => FlyoutKind::Start,
            "SEARCH" => FlyoutKind::Search,
            "CONTROL_CENTER" => FlyoutKind::ControlCenter,
            _ => return None,
        };
        let (mut generation, mut seq) = (None, None);
        for word in words {
            let (key, value) = word.split_once('=')?;
            let number = parse_unsigned(value)?;
            match key {
                "generation" => generation = Some(number),
                "seq" => seq = Some(number),
                _ => return None,
            }
        }
        Some(Self {
            kind,
            generation: generation?,
            seq: seq?,
        })
    }
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

/// Separate file for the C#-style flyout intent lines. Keeping it apart from
/// `flyout-positions.txt` preserves the native watchdog's four-field format.
fn kind_journal_path() -> PathBuf {
    journal_path().with_file_name("flyout-kind-journal.txt")
}

/// Persists the flyout kind/generation/sequence lines atomically (`*.tmp` + rename).
pub fn write_kind_records(records: &[FlyoutKindRecord]) {
    write_kind_records_to(&kind_journal_path(), records);
}

/// Testable/injectable variant of [`write_kind_records`].
pub fn write_kind_records_to(path: &Path, records: &[FlyoutKindRecord]) {
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut content = String::new();
    for record in records.iter().take(MAX_ENTRIES) {
        content.push_str(&record.to_line());
        content.push('\n');
    }
    let temporary = tmp_path(path);
    if fs::write(&temporary, content).is_err() {
        return;
    }
    if fs::rename(&temporary, path).is_err() && path.exists() && fs::remove_file(path).is_ok() {
        let _ = fs::rename(&temporary, path);
    }
}

/// Reads the C#-style flyout intent lines, skipping malformed ones.
#[must_use]
pub fn read_kind_records() -> Vec<FlyoutKindRecord> {
    read_kind_records_from(&kind_journal_path())
}

/// Testable/injectable variant of [`read_kind_records`].
#[must_use]
pub fn read_kind_records_from(path: &Path) -> Vec<FlyoutKindRecord> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .take(MAX_ENTRIES)
        .filter_map(FlyoutKindRecord::from_line)
        .collect()
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
    write_all_to_with_meta_inner(path, positions, None);
}

/// Writes the four-field journal plus `generation=<n> seq=<n>` tokens. The
/// legacy writer/reader and the native watchdog keep working unchanged: the
/// first four whitespace fields are identical.
pub fn write_all_with_meta(positions: &[FlyoutPosition], meta: JournalMeta) {
    write_all_to_with_meta(&journal_path(), positions, meta);
}

/// Injectable-path variant of [`write_all_with_meta`].
pub fn write_all_to_with_meta(path: &Path, positions: &[FlyoutPosition], meta: JournalMeta) {
    write_all_to_with_meta_inner(path, positions, Some(meta));
}

fn write_all_to_with_meta_inner(
    path: &Path,
    positions: &[FlyoutPosition],
    meta: Option<JournalMeta>,
) {
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
        match meta {
            Some(meta) => content.push_str(&format!(
                "{} {} {} {} generation={} seq={}\n",
                position.handle,
                position.pid,
                position.left,
                position.top,
                meta.generation,
                meta.seq
            )),
            None => content.push_str(&format!(
                "{} {} {} {}\n",
                position.handle, position.pid, position.left, position.top
            )),
        }
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
#[must_use]
pub fn read() -> Vec<FlyoutPosition> {
    read_from(&journal_path())
}

fn read_from(path: &Path) -> Vec<FlyoutPosition> {
    read_from_with_meta(path)
        .into_iter()
        .map(|(position, _)| position)
        .collect()
}

/// Reads positions together with their optional `generation`/`seq` tokens.
/// A legacy four-field line yields [`JournalMeta::default`].
#[must_use]
pub fn read_with_meta() -> Vec<(FlyoutPosition, JournalMeta)> {
    read_from_with_meta(&journal_path())
}

/// Injectable-path variant of [`read_with_meta`].
#[must_use]
pub fn read_from_with_meta(path: &Path) -> Vec<(FlyoutPosition, JournalMeta)> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .take(MAX_ENTRIES)
        .filter_map(parse_position_line)
        .collect()
}

fn parse_position_line(line: &str) -> Option<(FlyoutPosition, JournalMeta)> {
    let mut words = line.split_whitespace();
    let handle: usize = parse_unsigned(words.next()?)?;
    let pid: u32 = parse_unsigned(words.next()?)?;
    let left = parse_signed(words.next()?)?;
    let top = parse_signed(words.next()?)?;
    let mut meta = JournalMeta::default();
    let (mut saw_generation, mut saw_seq) = (false, false);
    for word in words {
        let (key, value) = word.split_once('=')?;
        let number: u64 = parse_unsigned(value)?;
        match key {
            "generation" => {
                meta.generation = number;
                saw_generation = true;
            }
            "seq" => {
                meta.seq = number;
                saw_seq = true;
            }
            _ => return None,
        }
    }
    // A half-written metadata pair is as unusable as a truncated C# line.
    if saw_generation != saw_seq {
        return None;
    }
    // HWND 0 hiçbir zaman geri yüklenemez; C# journal'ı da bunu atar.
    (handle != 0).then_some((
        FlyoutPosition {
            handle,
            pid,
            left,
            top,
        },
        meta,
    ))
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

    #[test]
    fn kind_records_format_and_parse_all_three_c_sharp_lines() {
        for (line, kind, generation, seq) in [
            ("START generation=42 seq=7", FlyoutKind::Start, 42, 7),
            ("SEARCH generation=1 seq=2", FlyoutKind::Search, 1, 2),
            (
                "CONTROL_CENTER generation=9 seq=3",
                FlyoutKind::ControlCenter,
                9,
                3,
            ),
        ] {
            let record = FlyoutKindRecord::from_line(line).expect("C# line must parse");
            assert_eq!(record.kind, kind);
            assert_eq!((record.generation, record.seq), (generation, seq));
            assert_eq!(record.to_line(), line);
        }
        for bad in [
            "",
            "UNKNOWN generation=1 seq=1",
            "START generation=x seq=1",
            "START generation=1",
            "START generation=1 seq=1 extra=a",
            "start generation=1 seq=1",
        ] {
            assert_eq!(FlyoutKindRecord::from_line(bad), None, "{bad}");
        }
    }

    #[test]
    fn kind_journal_file_round_trips_generation_and_seq() {
        let path = test_path("kinds");
        clear_from(&path);
        let records = [
            FlyoutKindRecord {
                kind: FlyoutKind::Start,
                generation: 42,
                seq: 7,
            },
            FlyoutKindRecord {
                kind: FlyoutKind::Search,
                generation: 42,
                seq: 8,
            },
            FlyoutKindRecord {
                kind: FlyoutKind::ControlCenter,
                generation: 43,
                seq: 1,
            },
        ];
        write_kind_records_to(&path, &records);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "START generation=42 seq=7\nSEARCH generation=42 seq=8\nCONTROL_CENTER generation=43 seq=1\n"
        );
        assert_eq!(read_kind_records_from(&path), records);
        assert!(!tmp_path(&path).exists());
        clear_from(&path);
    }

    #[test]
    fn position_journal_meta_is_backward_compatible() {
        let path = test_path("meta");
        clear_from(&path);

        // A legacy four-field line must still read, with default metadata.
        fs::write(&path, "1234 56 -320 17\n").unwrap();
        let (position, meta) = &read_from_with_meta(&path)[0];
        assert_eq!(
            *position,
            FlyoutPosition {
                handle: 1234,
                pid: 56,
                left: -320,
                top: 17
            }
        );
        assert_eq!(*meta, JournalMeta::default());
        assert_eq!(read_from(&path).len(), 1);

        // The extended line keeps the first four fields intact for the native
        // watchdog and round-trips generation/seq through the Rust reader.
        let meta = JournalMeta {
            generation: 11,
            seq: 29,
        };
        write_all_to_with_meta(&path, &[*position], meta);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "1234 56 -320 17 generation=11 seq=29\n");
        assert!(text.starts_with("1234 56 -320 17 "));
        let (read_position, read_meta) = &read_from_with_meta(&path)[0];
        assert_eq!(*read_position, *position);
        assert_eq!(*read_meta, meta);
        assert_eq!(read_from(&path).len(), 1);

        // Rewriting with the legacy writer restores the shipped format exactly.
        write_all_to(&path, &[*position]);
        assert_eq!(fs::read_to_string(&path).unwrap(), "1234 56 -320 17\n");
        clear_from(&path);
    }

    #[test]
    fn malformed_meta_tokens_are_rejected() {
        assert!(parse_position_line("1234 56 1 2 generation=1 seq=2").is_some());
        assert!(parse_position_line("0 56 1 2 generation=1 seq=2").is_none());
        assert!(parse_position_line("1234 56 1 2 generation=x seq=2").is_none());
        assert!(parse_position_line("1234 56 1 2 generation=1").is_none());
        assert!(parse_position_line("1234 56 1 2 generation=1 seq=2 other=3").is_none());
    }
}
