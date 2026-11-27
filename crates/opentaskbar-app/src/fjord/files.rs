//! Fjord Files: This PC, favourites, drives, navigation history and entry I/O.
//!
//! reference: `Views/Windows/FjordFilesWindow.xaml.cs`. Everything above the
//! `@pure-end` marker is standard-library only and is exercised by the tests at
//! the bottom; the Win32 layer below it is deliberately thin.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

// @pure-start
/// Canonical "This PC" shell location (matches `FjordFilesWindow.ComputerLocation`).
pub const COMPUTER_LOCATION: &str = "shell:MyComputerFolder";
const COMPUTER_CLSID: &str = "{20d04fe0-3aea-1069-a2d8-08002b30309d}";
const TRAIL_SEPARATOR: &str = "   ›   ";
const ELLIPSIS: &str = "…";

/// Identifies all spellings of This PC accepted by the reference window.
pub fn is_computer_location(path: &str) -> bool {
    let value = path.trim().trim_matches('"');
    let lowered = value.to_ascii_lowercase();
    lowered == COMPUTER_LOCATION.to_ascii_lowercase()
        || lowered == "bu bilgisayar"
        || lowered == "this pc"
        || lowered == format!("::{}", COMPUTER_CLSID)
        || lowered == format!("shell:::{}", COMPUTER_CLSID)
}

/// One row in the file list (`reference FileEntry` record).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub is_folder: bool,
    pub size: Option<u64>,
    pub changed: Option<SystemTime>,
}

impl FileEntry {
    pub fn new(path: impl Into<String>, name: impl Into<String>, is_folder: bool) -> Self {
        Self {
            path: path.into(),
            name: name.into(),
            is_folder,
            size: None,
            changed: None,
        }
    }

    /// `FileEntry.DateLabel` — `dd MMM yyyy` with a culture-aware month table.
    pub fn date_label(&self, culture: &str) -> String {
        self.changed
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|elapsed| format_date(elapsed.as_secs() as i64, culture))
            .unwrap_or_default()
    }

    /// `FileEntry.SizeLabel` with the culture's decimal separator.
    pub fn size_label(&self, decimal_separator: char) -> String {
        match self.size {
            None => String::new(),
            Some(bytes) => format_size(bytes, decimal_separator),
        }
    }
}

const MONTHS_EN: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_TR: [&str; 12] = [
    "Oca", "Şub", "Mar", "Nis", "May", "Haz", "Tem", "Ağu", "Eyl", "Eki", "Kas", "Ara",
];

/// Howard Hinnant's civil-from-days algorithm; no `chrono` dependency needed.
pub fn format_date(unix_seconds: i64, culture: &str) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    let months = if culture.to_ascii_lowercase().starts_with("tr") {
        MONTHS_TR
    } else {
        MONTHS_EN
    };
    let name = months.get((month - 1) as usize).copied().unwrap_or("Jan");
    format!("{day:02} {name} {year:04}")
}

/// `N0`/`N1` style size text with the active culture's decimal separator.
pub fn format_size(bytes: u64, decimal_separator: char) -> String {
    const KIB: f64 = 1024.0;
    let (value, unit) = if bytes < 1024 {
        return format!("{bytes} B");
    } else if bytes < 1024 * 1024 {
        (bytes as f64 / KIB, "KB")
    } else if bytes < 1024 * 1024 * 1024 {
        (bytes as f64 / (KIB * KIB), "MB")
    } else {
        (bytes as f64 / (KIB * KIB * KIB), "GB")
    };
    let text = format!("{value:.1}").replace('.', &decimal_separator.to_string());
    format!("{text} {unit}")
}

/// `FjordFilesWindow.FolderTrail`: last three components with a leading ellipsis
/// when the path is deeper. Handles `\`, `/`, drive roots and UNC roots.
pub fn folder_trail(path: &str) -> String {
    if is_computer_location(path) {
        return "This PC".to_owned();
    }
    let mut current = path.trim().trim_matches('"').replace('/', "\\");
    while current.ends_with('\\') && current.len() > 3 {
        current.pop();
    }
    if current.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    loop {
        parts.push(component_name(&current));
        let Some(parent) = parent_of(&current) else {
            break;
        };
        current = parent;
        if parts.len() == 3 {
            parts.push(ELLIPSIS.to_owned());
            break;
        }
    }
    parts.reverse();
    parts.join(TRAIL_SEPARATOR)
}

/// Final path component, with a drive root rendered as `C:`.
pub fn component_name(path: &str) -> String {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let name = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    if name.is_empty() {
        trimmed.to_owned()
    } else {
        name.to_owned()
    }
}

/// Parent path segment; `None` at a drive or UNC share root.
pub fn parent_of(path: &str) -> Option<String> {
    let normalized = path.replace('/', "\\");
    let trimmed = normalized.trim_end_matches('\\');
    let index = trimmed.rfind('\\')?;
    let parent = &trimmed[..index];
    if parent.is_empty() {
        return None;
    }
    // A drive root (`C:`) is a valid breadcrumb component, but has no parent.
    Some(parent.to_owned())
}

/// `%VAR%` expansion with an injectable environment lookup (testable on any host).
pub fn expand_environment(input: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(end) = input[index + 1..].find('%')
        {
            let name = &input[index + 1..index + 1 + end];
            if let Some(value) = lookup(name)
                && !name.is_empty()
            {
                out.push_str(&value);
                index += end + 2;
                continue;
            }
        }
        let ch = input[index..]
            .chars()
            .next()
            .expect("index is inside the string");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// Case/diacritic-insensitive search fold; mirrors `ui::components::fold_search_text`.
pub fn fold_for_search(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ('\u{0300}'..='\u{036f}').contains(&ch) {
            continue; // combining diacritic from an already-decomposed string
        }
        // Precomposed Latin-1 / Turkish characters need an explicit base mapping
        // because `std` has no Unicode normalisation.
        let base = match ch {
            'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'č' => 'c',
            'ď' | 'đ' => 'd',
            'è'..='ë' | 'ē' | 'ė' | 'ę' => 'e',
            'ğ' => 'g',
            'ì'..='ï' | 'ī' | 'į' | 'ı' => 'i',
            'ł' => 'l',
            'ñ' | 'ń' | 'ň' => 'n',
            'ò'..='ö' | 'ø' | 'ō' | 'ő' => 'o',
            'ř' => 'r',
            'ś' | 'ş' | 'š' => 's',
            'ţ' | 'ť' | 'ț' => 't',
            'ù'..='ü' | 'ū' | 'ů' | 'ű' => 'u',
            'ý' | 'ÿ' | 'ỳ' => 'y',
            'ž' | 'ź' | 'ż' => 'z',
            _ => ch,
        };
        out.push(base);
    }
    out
}

/// Culture-aware filter from `FjordFilesWindow.ApplyFilter`.
pub fn matches_filter(name: &str, term: &str, _culture: &str) -> bool {
    let term = fold_for_search(term.trim());
    if term.is_empty() {
        return true;
    }
    fold_for_search(name).contains(&term)
}

/// `Path.GetFullPath`-like normalization that never touches the filesystem.
pub fn normalize_path_text(raw: &str, base: Option<&str>) -> Option<String> {
    let input = raw.trim().trim_matches('"');
    if input.is_empty() {
        return None;
    }
    let expanded = input.replace('/', "\\");
    let absolute = Path::new(&expanded).is_absolute();
    let combined = if absolute {
        expanded
    } else {
        let root = base.unwrap_or("C:\\");
        format!("{}\\{}", root.trim_end_matches(['\\', '/']), expanded)
    };
    let mut out = PathBuf::new();
    for part in Path::new(&combined).components() {
        match part {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push("\\"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(value) => out.push(value),
        }
    }
    let text = out.to_string_lossy().replace('/', "\\");
    (!text.is_empty()).then_some(text)
}

/// Resolves a location bar entry to This PC or an absolute path.
pub fn navigation_target(raw: &str, base: Option<&str>, home: &str) -> Option<String> {
    let input = raw.trim().trim_matches('"');
    if is_computer_location(input) {
        return Some(COMPUTER_LOCATION.to_owned());
    }
    let expanded = expand_environment(input, |name| {
        if name.eq_ignore_ascii_case("USERPROFILE") || name.eq_ignore_ascii_case("HOME") {
            Some(home.to_owned())
        } else {
            std::env::var(name).ok()
        }
    });
    normalize_path_text(&expanded, Some(base.unwrap_or(home)))
}

/// Back/forward stack with reference semantics: duplicate current entries are ignored
/// and navigating back truncates the redo branch.
#[derive(Clone, Debug, Default)]
pub struct History {
    stack: Vec<String>,
    index: usize,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current(&self) -> Option<&str> {
        self.stack.get(self.index).map(String::as_str)
    }

    pub fn push(&mut self, path: impl Into<String>) -> bool {
        let path = path.into();
        if self.current() == Some(path.as_str()) {
            return false;
        }
        self.stack.truncate(self.index + 1);
        self.stack.push(path);
        self.index = self.stack.len() - 1;
        true
    }

    /// Restores an explicit history slot (used after Alt+Left/Right).
    pub fn restore(&mut self, index: usize) -> Option<String> {
        if index < self.stack.len() {
            self.index = index;
            self.current().map(ToOwned::to_owned)
        } else {
            None
        }
    }

    pub fn step(&self, delta: i32) -> Option<(usize, String)> {
        let next = self.index as i64 + i64::from(delta);
        if next < 0 || next as usize >= self.stack.len() {
            return None;
        }
        let index = next as usize;
        Some((index, self.stack[index].clone()))
    }

    pub fn can_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_forward(&self) -> bool {
        self.index + 1 < self.stack.len()
    }

    /// Parent of the current folder, or This PC at a drive root.
    pub fn up_target(&self) -> Option<String> {
        let current = self.current()?;
        if is_computer_location(current) {
            return None;
        }
        Some(match parent_of(current) {
            Some(parent) if parent.ends_with(':') => format!("{parent}\\"),
            Some(parent) => parent,
            None => COMPUTER_LOCATION.to_owned(),
        })
    }
}

/// Sort key from `ReadEntries`: folders first, then culture-insensitive name.
pub fn entry_sort_key(entry: &FileEntry) -> (bool, String) {
    (!entry.is_folder, fold_for_search(&entry.name))
}
// @pure-end

/// Reads one folder with per-entry error tolerance and cancellation.
pub fn read_entries(path: &str, cancel: &AtomicBool) -> std::io::Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    for item in std::fs::read_dir(path)? {
        if cancel.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        let Ok(item) = item else { continue };
        let Ok(metadata) = item.metadata() else {
            continue;
        };
        let name = item.file_name().to_string_lossy().into_owned();
        entries.push(FileEntry {
            path: item.path().to_string_lossy().into_owned(),
            is_folder: metadata.is_dir(),
            size: (!metadata.is_dir()).then_some(metadata.len()),
            changed: metadata.modified().ok(),
            name,
        });
    }
    entries.sort_by_key(entry_sort_key);
    Ok(entries)
}

/// `DriveInfo.GetDrives()` parity via `GetLogicalDriveStringsW`.
pub fn read_drives() -> Vec<FileEntry> {
    #[cfg(windows)]
    {
        let mut buffer = vec![0u16; 1024];
        // SAFETY: buffer outlives the call and the API writes a double-NUL list.
        let len = unsafe { GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) };
        if len == 0 || len as usize > buffer.len() {
            return fallback_drives();
        }
        buffer
            .split(|c| *c == 0)
            .filter(|part| !part.is_empty())
            .map(|part| {
                let name = String::from_utf16_lossy(part);
                let label = name.trim_end_matches(['\\', '/']).to_owned();
                FileEntry::new(name, label, true)
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        fallback_drives()
    }
}

fn fallback_drives() -> Vec<FileEntry> {
    ('A'..='Z')
        .filter(|letter| Path::new(&format!("{letter}:\\")).exists())
        .map(|letter| FileEntry::new(format!("{letter}:\\"), format!("{letter}:"), true))
        .collect()
}

/// The eight reference favourites. Labels are locale *keys*; the UI translates them.
pub fn favorites(home: &str) -> Vec<(&'static str, &'static str, String)> {
    let join = |name: &str| format!("{}\\{}", home.trim_end_matches(['\\', '/']), name);
    vec![
        (
            "FjordFilesComputer",
            "\u{e770}",
            COMPUTER_LOCATION.to_owned(),
        ),
        ("FjordFilesHome", "\u{e80f}", home.to_owned()),
        ("FjordFilesDesktop", "\u{e7f4}", join("Desktop")),
        ("FjordFilesDownloads", "\u{e896}", downloads_path(home)),
        ("FjordFilesDocuments", "\u{e8a5}", join("Documents")),
        ("FjordFilesPictures", "\u{eb9f}", join("Pictures")),
        ("FjordFilesMusic", "\u{e8d6}", join("Music")),
        ("FjordFilesVideos", "\u{e8b2}", join("Videos")),
    ]
}

/// `SHGetKnownFolderPath` for the Downloads GUID with a profile-relative fallback.
pub fn downloads_path(home: &str) -> String {
    #[cfg(windows)]
    {
        // GUID {374DE290-123F-4565-9164-39C4925E467B} (FOLDERID_Downloads).
        let guid = windows_sys::core::GUID {
            data1: 0x374DE290,
            data2: 0x123F,
            data3: 0x4565,
            data4: [0x91, 0x64, 0x39, 0xC4, 0x92, 0x5E, 0x46, 0x7B],
        };
        let mut pointer = std::ptr::null_mut();
        // SAFETY: `pointer` is owned by us and freed immediately after the copy.
        let hr = unsafe { SHGetKnownFolderPath(&guid, 0, std::ptr::null_mut(), &mut pointer) };
        if hr == 0 && !pointer.is_null() {
            let mut end = 0;
            // SAFETY: SHGetKnownFolderPath returns a NUL-terminated CoTaskMem string.
            while unsafe { *pointer.add(end) } != 0 {
                end += 1;
            }
            let value =
                String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, end) });
            // SAFETY: allocation came from CoTaskMemAlloc in SHGetKnownFolderPath.
            unsafe { CoTaskMemFree(pointer.cast()) };
            if !value.is_empty() {
                return value;
            }
        }
    }
    format!("{}\\Downloads", home.trim_end_matches(['\\', '/']))
}

/// Starts the configured file verb (shell execute, like `ProcessStartInfo`).
pub fn open_file(path: &str, argument: Option<&str>) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let file: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let params = argument
            .map(|value| value.encode_utf16().chain(Some(0)).collect::<Vec<u16>>())
            .unwrap_or_default();
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        // SAFETY: all wide strings are NUL-terminated and alive for the call.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                if params.is_empty() {
                    std::ptr::null()
                } else {
                    params.as_ptr()
                },
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        result.cast::<isize>() as isize > 32
    }
    #[cfg(not(windows))]
    {
        let _ = (path, argument);
        false
    }
}

/// `explorer.exe /select,<path>` from `FjordFilesWindow.RevealClick`.
pub fn reveal_in_explorer(path: &str) -> bool {
    open_file("explorer.exe", Some(&format!("/select,{path}")))
}

/// Copies a path to the clipboard as CF_UNICODETEXT.
pub fn copy_path_to_clipboard(path: &str) -> bool {
    #[cfg(windows)]
    {
        let units: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        // SAFETY: each Win32 call is checked; the global is owned until SetClipboardData.
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return false;
            }
            if EmptyClipboard() == 0 {
                let _ = CloseClipboard();
                return false;
            }
            let bytes = units.len() * size_of::<u16>();
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if handle.is_null() {
                let _ = CloseClipboard();
                return false;
            }
            let memory = GlobalLock(handle).cast::<u16>();
            if memory.is_null() {
                let _ = GlobalFree(handle);
                let _ = CloseClipboard();
                return false;
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), memory, units.len());
            let _ = GlobalUnlock(handle);
            if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                let _ = GlobalFree(handle);
                let _ = CloseClipboard();
                return false;
            }
            let _ = CloseClipboard();
            true
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// Spawns a cancellable folder read. The receiver yields one result at most.
pub fn navigate_async(
    target: String,
    base: Option<String>,
    home: String,
    cancel: std::sync::Arc<AtomicBool>,
) -> mpsc::Receiver<std::io::Result<Vec<FileEntry>>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        if is_computer_location(&target) {
            let _ = sender.send(Ok(read_drives()));
            return;
        }
        let resolved = navigation_target(&target, base.as_deref(), &home)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad path"));
        let result = resolved.and_then(|path| read_entries(&path, &cancel));
        let _ = sender.send(result);
    });
    receiver
}

#[cfg(windows)]
#[allow(clashing_extern_declarations)]
unsafe extern "system" {
    #[link_name = "GetLogicalDriveStringsW"]
    fn GetLogicalDriveStringsW(buffer_length: u32, buffer: *mut u16) -> u32;
    #[link_name = "SHGetKnownFolderPath"]
    fn SHGetKnownFolderPath(
        id: *const windows_sys::core::GUID,
        flags: u32,
        token: *mut core::ffi::c_void,
        path: *mut *mut u16,
    ) -> i32;
    #[link_name = "CoTaskMemFree"]
    fn CoTaskMemFree(block: *mut core::ffi::c_void);
    #[link_name = "OpenClipboard"]
    fn OpenClipboard(window: windows_sys::Win32::Foundation::HWND) -> i32;
    #[link_name = "EmptyClipboard"]
    fn EmptyClipboard() -> i32;
    #[link_name = "CloseClipboard"]
    fn CloseClipboard() -> i32;
    #[link_name = "GlobalAlloc"]
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut core::ffi::c_void;
    #[link_name = "GlobalLock"]
    fn GlobalLock(handle: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    #[link_name = "GlobalUnlock"]
    fn GlobalUnlock(handle: *mut core::ffi::c_void) -> i32;
    #[link_name = "GlobalFree"]
    fn GlobalFree(handle: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    #[link_name = "SetClipboardData"]
    fn SetClipboardData(format: u32, handle: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
}

#[cfg(windows)]
const CF_UNICODETEXT: u32 = 13;
#[cfg(windows)]
const GMEM_MOVEABLE: u32 = 0x0002;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumb_shows_last_three_components() {
        // reference adds the ellipsis once the third segment still has a parent.
        assert_eq!(
            folder_trail(r"C:\Users\agnes\Downloads"),
            format!(
                "{ELLIPSIS}{TRAIL_SEPARATOR}Users{TRAIL_SEPARATOR}agnes{TRAIL_SEPARATOR}Downloads"
            )
        );
        assert!(folder_trail(r"C:\Users\agnes\Downloads\A\B").starts_with(ELLIPSIS));
        assert_eq!(folder_trail(r"C:\Users"), r"C:   ›   Users");
    }

    #[test]
    fn breadcrumb_handles_this_pc_and_forward_slashes() {
        assert_eq!(folder_trail(COMPUTER_LOCATION), "This PC");
        assert_eq!(folder_trail("Bu Bilgisayar"), "This PC");
        assert_eq!(
            folder_trail("C:/Users/agnes"),
            folder_trail(r"C:\Users\agnes")
        );
    }

    #[test]
    fn computer_location_recognizes_all_shell_spellings() {
        assert!(is_computer_location(" shell:mycomputerfolder "));
        assert!(is_computer_location("\"This PC\""));
        assert!(is_computer_location(
            "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"
        ));
        assert!(!is_computer_location(r"C:\Users"));
    }

    #[test]
    fn filter_is_case_and_diacritic_insensitive() {
        assert!(matches_filter("Résumé.pdf", "resume", "en-US"));
        assert!(matches_filter("Résumé.pdf", "RÉSUMÉ", "tr-TR"));
        assert!(!matches_filter("Notes.txt", "photo", "en-US"));
        assert!(matches_filter("Notes.txt", "  ", "en-US"));
    }

    #[test]
    fn history_tracks_back_forward_and_truncates_redo() {
        let mut history = History::new();
        assert!(history.push(r"C:\A"));
        assert!(history.push(r"C:\B"));
        assert!(history.can_back());
        let (index, path) = history.step(-1).unwrap();
        assert_eq!(path, r"C:\A");
        history.restore(index);
        assert!(history.can_forward());
        history.push(r"C:\C");
        assert!(!history.can_forward());
        assert_eq!(history.current(), Some(r"C:\C"));
    }

    #[test]
    fn history_up_stops_at_this_pc() {
        let mut history = History::new();
        history.push(r"C:\Users\agnes");
        assert_eq!(history.up_target().as_deref(), Some(r"C:\Users"));
        history.push(COMPUTER_LOCATION);
        assert_eq!(history.up_target(), None);
    }

    #[test]
    fn environment_expansion_and_navigation_target() {
        let expanded = expand_environment("%USERPROFILE%\\Downloads", |name| {
            (name.eq_ignore_ascii_case("USERPROFILE")).then(|| r"C:\Users\agnes".to_owned())
        });
        assert_eq!(expanded, r"C:\Users\agnes\Downloads");
        assert_eq!(
            navigation_target("%USERPROFILE%\\Music", Some(r"C:\Users"), r"C:\Users\agnes"),
            Some(r"C:\Users\agnes\Music".to_owned())
        );
        assert_eq!(
            navigation_target("shell:MyComputerFolder", None, "C:\\"),
            Some(COMPUTER_LOCATION.to_owned())
        );
    }

    #[test]
    fn normalization_collapses_dot_segments() {
        assert_eq!(
            normalize_path_text(r"C:\Users\.\agnes\..\Downloads", None).as_deref(),
            Some(r"C:\Users\Downloads")
        );
        assert_eq!(
            normalize_path_text("Docs", Some(r"D:\Work")).as_deref(),
            Some(r"D:\Work\Docs")
        );
        assert_eq!(normalize_path_text("   ", None), None);
    }

    #[test]
    fn size_and_date_labels_are_culture_aware() {
        assert_eq!(format_size(512, ','), "512 B");
        assert_eq!(format_size(2048, ','), "2,0 KB");
        assert_eq!(format_size(3 * 1024 * 1024, '.'), "3.0 MB");
        assert_eq!(format_date(0, "en-US"), "01 Jan 1970");
        assert_eq!(format_date(0, "tr-TR"), "01 Oca 1970");
    }

    #[test]
    fn entries_sort_folders_first_then_name() {
        let folder = FileEntry::new(r"C:\A\Z", "Z", true);
        let file = FileEntry::new(r"C:\A\a.txt", "a.txt", false);
        let mut entries = vec![file.clone(), folder.clone()];
        entries.sort_by_key(entry_sort_key);
        assert_eq!(entries, vec![folder, file]);
    }

    #[test]
    fn entry_labels_include_size_and_date() {
        let entry = FileEntry {
            path: "x".into(),
            name: "x".into(),
            is_folder: false,
            size: Some(4096),
            changed: Some(UNIX_EPOCH + std::time::Duration::from_secs(86_400)),
        };
        assert_eq!(entry.size_label(','), "4,0 KB");
        assert_eq!(entry.date_label("en-US"), "02 Jan 1970");
    }
}
