//! Flyout position journal writer with a generation counter and PID-checked
//! restore. Mirrors C# `ShellFlyoutPositionRegistry` +
//! `ShellFlyoutPositionJournal`: the dock records where Windows placed Start,
//! Search and Control Center *before* it anchors them, and restores those
//! positions on shutdown or hard-kill recovery.
//!
//! The journal format is shared with the native C watchdog (`flyout-positions.txt`,
//! `<hwnd> <pid> <left> <top>` per line). Writing is gated by the
//! `native_flyout_reposition` flag (default **false**) because an incorrect
//! reposition loop can disturb Explorer; recovery reading always stays active.

use crate::flyout_journal;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI8, AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindow, IsWindowVisible, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, SetWindowPos,
};

/// -1 = follow `DockSettings.extra["native_flyout_reposition"]`, 0/1 = override.
static ENABLED_OVERRIDE: AtomicI8 = AtomicI8::new(-1);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

const MAX_ENTRIES: usize = 16;
const FLAG_NAME: &str = "native_flyout_reposition";

/// Exact executable file names of each shell flyout host. Shared with the
/// reposition workers so discovery and journaling cannot drift apart.
pub const START_EXE: &str = "startmenuexperiencehost.exe";
pub const SEARCH_EXES: &[&str] = &["searchhost.exe", "searchapp.exe"];
pub const CONTROL_CENTER_EXES: &[&str] = &["shellexperiencehost.exe", "systemsettings.exe"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlyoutPosition {
    pub hwnd: usize,
    pub pid: u32,
    pub left: i32,
    pub top: i32,
}

#[derive(Default)]
struct Registry {
    positions: Vec<FlyoutPosition>,
}

impl Registry {
    const fn new() -> Self {
        Self {
            positions: Vec::new(),
        }
    }
}

fn journal_path() -> PathBuf {
    opentaskbar_core::paths::journal_path("flyout-positions.txt")
}

fn flag_enabled() -> bool {
    match ENABLED_OVERRIDE.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => opentaskbar_core::settings::load_from(&opentaskbar_core::paths::settings_path())
            .extra
            .get(FLAG_NAME)
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    }
}

/// Overrides the settings flag; `None` returns to reading `DockSettings.extra`.
pub fn set_enabled(value: Option<bool>) {
    ENABLED_OVERRIDE.store(
        match value {
            Some(true) => 1,
            Some(false) => 0,
            None => -1,
        },
        Ordering::Relaxed,
    );
}

pub fn is_enabled() -> bool {
    flag_enabled()
}

/// Changes when the flyouts are restored; in-flight reposition loops capture it
/// before moving a window and must stop when it differs.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub fn is_current(captured: u64) -> bool {
    captured == generation()
}

/// Only a settled flyout (>= 100x100) is journalable; small entrance-animation
/// rects must never overwrite the original position.
#[must_use]
pub fn rect_is_journalable(width: i32, height: i32) -> bool {
    width >= 100 && height >= 100
}

/// Records the original position of a single flyout handle and persists the
/// journal. No-op when `native_flyout_reposition` is off or the rect is a
/// transient animation frame.
pub fn remember(hwnd: usize, pid: u32, left: i32, top: i32, width: i32, height: i32) {
    if hwnd == 0 || pid == 0 || !rect_is_journalable(width, height) || !flag_enabled() {
        return;
    }
    let Ok(mut registry) = REGISTRY.lock() else {
        return;
    };
    // Only the first position seen for a handle is trustworthy: after that the
    // dock itself may already have moved it.
    if registry
        .positions
        .iter()
        .any(|position| position.hwnd == hwnd)
    {
        return;
    }
    prune_dead(&mut registry);
    registry.positions.push(FlyoutPosition {
        hwnd,
        pid,
        left,
        top,
    });
    persist(&registry);
}

fn prune_dead(registry: &mut Registry) {
    registry.positions.retain(|position| {
        // SAFETY: checking liveness/ownership of a previously observed window.
        unsafe {
            let hwnd = position.hwnd as HWND;
            let mut current = 0u32;
            GetWindowThreadProcessId(hwnd, &mut current);
            IsWindow(hwnd) != 0 && current == position.pid
        }
    });
}

fn persist(registry: &Registry) {
    let path = journal_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut contents = String::new();
    for position in registry.positions.iter().take(MAX_ENTRIES) {
        contents.push_str(&format!(
            "{} {} {} {}\n",
            position.hwnd, position.pid, position.left, position.top
        ));
    }
    let tmp = temp_path(&path);
    if fs::write(&tmp, contents).is_ok()
        && fs::rename(&tmp, &path).is_err()
        && path.exists()
        && fs::remove_file(&path).is_ok()
    {
        let _ = fs::rename(&tmp, &path);
    }
}

/// Must stay identical to `flyout_journal::tmp_path`: both writers target the
/// same `flyout-positions.txt.tmp` file so a killed process leaves at most one
/// partial journal that the next recovery pass can overwrite atomically.
#[must_use]
pub fn temp_path(path: &Path) -> PathBuf {
    flyout_journal::tmp_path(path)
}

/// Reads the on-disk journal (also used after a hard kill, when the in-process
/// registry is gone).
pub fn read() -> Vec<FlyoutPosition> {
    let Ok(raw) = fs::read_to_string(journal_path()) else {
        return Vec::new();
    };
    raw.lines()
        .take(MAX_ENTRIES)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() != 4 {
                return None;
            }
            Some(FlyoutPosition {
                hwnd: fields[0].parse().ok()?,
                pid: fields[1].parse().ok()?,
                left: fields[2].parse().ok()?,
                top: fields[3].parse().ok()?,
            })
        })
        .filter(|position| position.hwnd != 0)
        .collect()
}

pub fn clear() {
    let _ = fs::remove_file(journal_path());
}

fn move_back(position: FlyoutPosition) {
    // SAFETY: restoring position of a window we previously recorded, without
    // resize/Z-order/focus side effects.
    unsafe {
        SetWindowPos(
            position.hwnd as HWND,
            std::ptr::null_mut(),
            position.left,
            position.top,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn restore_positions(positions: Vec<FlyoutPosition>) -> bool {
    let mut owned_by_someone_else = false;
    for position in positions {
        // SAFETY: PID check prevents a recycled HWND from moving an unrelated app.
        unsafe {
            if IsWindow(position.hwnd as HWND) == 0 {
                continue;
            }
            let mut current = 0u32;
            GetWindowThreadProcessId(position.hwnd as HWND, &mut current);
            if current != position.pid {
                owned_by_someone_else = true;
                continue;
            }
        }
        move_back(position);
    }
    if !owned_by_someone_else {
        clear();
    }
    !owned_by_someone_else
}

/// Stops any pending moves (bumping the generation) and returns every still-valid
/// window to its original position. Called on every taskbar shutdown path.
pub fn restore() -> bool {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    let positions = {
        let Ok(mut registry) = REGISTRY.lock() else {
            return false;
        };
        let positions = registry.positions.clone();
        registry.positions.clear();
        positions
    };
    if positions.is_empty() {
        return restore_positions(read());
    }
    restore_positions(positions)
}

/// Recovery path for a hard kill: use only the journal, since the in-process
/// registry died with the app. Idempotent; a recycled handle keeps the journal
/// for a later recovery pass.
pub fn restore_from_journal() -> bool {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut registry) = REGISTRY.lock() {
        registry.positions.clear();
    }
    restore_positions(read())
}

// ---- Window discovery -----------------------------------------------------------

fn process_exe(pid: u32) -> Option<String> {
    // SAFETY: limited query handle closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        use windows_sys::Win32::Foundation::CloseHandle;
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        (ok != 0 && len > 0).then(|| String::from_utf16_lossy(&buffer[..len as usize]))
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: reading one DWORD from a live window.
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    hr == 0 && cloaked != 0
}

/// Exact executable file name (never a partial `ends_with`).
#[must_use]
pub fn exe_file_name(exe: &str) -> &str {
    exe.rsplit(['\\', '/']).next().unwrap_or(exe)
}

fn exe_name_matches(exe: &str, names: &[&str]) -> bool {
    let name = exe_file_name(exe).trim().to_ascii_lowercase();
    names.iter().any(|candidate| name == *candidate)
}

/// Finds visible, uncloaked shell flyout windows belonging to one of `exe_names`.
pub fn find_flyout_windows(exe_names: &[&str]) -> Vec<(usize, u32)> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` carries a borrowed context alive for the enumeration.
        unsafe {
            let (names, output): &mut (&[&str], Vec<(usize, u32)>) = &mut *(lparam as *mut _);
            if IsWindowVisible(hwnd) == 0 || is_cloaked(hwnd) {
                return 1;
            }
            let mut rect: RECT = std::mem::zeroed();
            let got_rect =
                windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) != 0;
            if got_rect && rect_is_journalable(rect.right - rect.left, rect.bottom - rect.top) {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                if let Some(exe) = process_exe(pid)
                    && exe_name_matches(&exe, names)
                {
                    output.push((hwnd as usize, pid));
                }
            }
            1
        }
    }

    let mut context: (&[&str], Vec<(usize, u32)>) = (exe_names, Vec::new());
    // SAFETY: synchronous enumeration; the context outlives every callback.
    unsafe {
        EnumWindows(Some(visit), (&mut context as *mut _) as LPARAM);
    }
    context.1
}

/// Records Start's original position (Windows 10/11 `StartMenuExperienceHost`).
pub fn write_start() {
    for (hwnd, pid) in find_flyout_windows(&[START_EXE]) {
        if let Some((left, top, width, height)) = window_origin(hwnd) {
            remember(hwnd, pid, left, top, width, height);
        }
    }
}

/// Records Windows Search's original position (`SearchHost.exe` / SearchApp on
/// Windows 10).
pub fn write_search() {
    for (hwnd, pid) in find_flyout_windows(SEARCH_EXES) {
        if let Some((left, top, width, height)) = window_origin(hwnd) {
            remember(hwnd, pid, left, top, width, height);
        }
    }
}

/// Records Control Center / Quick Settings' original position. Windows 11 hosts
/// it in `ShellExperienceHost.exe`, Windows 10 in `SystemSettings.exe`.
pub fn write_control_center() {
    for (hwnd, pid) in find_flyout_windows(CONTROL_CENTER_EXES) {
        if let Some((left, top, width, height)) = window_origin(hwnd) {
            remember(hwnd, pid, left, top, width, height);
        }
    }
}

fn window_origin(hwnd: usize) -> Option<(i32, i32, i32, i32)> {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    // SAFETY: reading the rect of a window discovered in this enumeration pass.
    if unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd as HWND, &mut rect)
    } != 0
    {
        Some((
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journals_are_atomic_and_generation_only_advances_on_restore() {
        let before = generation();
        assert!(is_current(before));
        set_enabled(Some(true));
        assert!(is_enabled());
        set_enabled(None);
        let position = FlyoutPosition {
            hwnd: 0,
            pid: 0,
            left: 0,
            top: 0,
        };
        assert_eq!(position.hwnd, 0);
    }

    #[test]
    fn journals_only_settled_rects() {
        assert!(!rect_is_journalable(99, 100));
        assert!(!rect_is_journalable(100, 99));
        assert!(rect_is_journalable(100, 100));
        assert!(rect_is_journalable(1920, 1080));
    }

    #[test]
    fn journaled_flyout_hosts_are_shared_with_reposition_discovery() {
        assert_eq!(START_EXE, "startmenuexperiencehost.exe");
        assert_eq!(SEARCH_EXES, &["searchhost.exe", "searchapp.exe"]);
        assert_eq!(
            CONTROL_CENTER_EXES,
            &["shellexperiencehost.exe", "systemsettings.exe"]
        );
        assert!(exe_name_matches(
            r"C:\Windows\SystemApps\SearchHost.exe",
            SEARCH_EXES
        ));
        assert!(exe_name_matches(
            r"C:\Windows\SystemApps\SystemSettings.exe",
            CONTROL_CENTER_EXES
        ));
        assert!(!exe_name_matches(
            r"C:\Temp\evil-searchhost.exe",
            SEARCH_EXES
        ));
    }

    #[test]
    fn both_flyout_writers_use_the_same_temporary_file_name() {
        let journal = journal_path();
        assert_eq!(
            journal,
            opentaskbar_core::paths::journal_path("flyout-positions.txt")
        );
        assert_eq!(journal.file_name().unwrap(), "flyout-positions.txt");
        assert_eq!(temp_path(&journal), flyout_journal::tmp_path(&journal));
        assert!(
            temp_path(&journal)
                .file_name()
                .is_some_and(|name| name.to_string_lossy() == "flyout-positions.txt.tmp")
        );
        assert_eq!(
            opentaskbar_core::paths::settings_path(),
            opentaskbar_core::paths::shared_app_data_dir().join("settings.json")
        );
    }

    #[test]
    fn executable_names_match_whole_file_name() {
        assert_eq!(
            exe_file_name(r"C:\Windows\SystemApps\StartMenuExperienceHost.exe"),
            "StartMenuExperienceHost.exe"
        );
        let names = ["startmenuexperiencehost.exe"];
        assert!(exe_name_matches(
            r"C:\Windows\SystemApps\StartMenuExperienceHost.exe",
            &names
        ));
        assert!(!exe_name_matches(
            r"C:\Temp\evil-startmenuexperiencehost.exe",
            &names
        ));
        assert!(!exe_name_matches(
            r"C:\Temp\startmenuexperiencehost.exe.bak",
            &names
        ));
    }
}
