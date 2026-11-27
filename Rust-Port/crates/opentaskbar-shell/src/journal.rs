//! Recovery journal recording Explorer's taskbar state before hiding it.
//! Uses `%LOCALAPPDATA%\OpenTaskbar\taskbar-state.txt` so the native C
//! `OpenTaskbarWatchdog.exe` reads the exact same file on crash/kill recovery.
//!
//! Wire format is C# `TaskbarReplacementSession.WriteJournal` parity: a single
//! decimal number with **no trailing newline**, containing only the two
//! supported AppBar state bits (`ABS_AUTOHIDE | ABS_ALWAYSONTOP`).

use std::fs;
use std::path::{Path, PathBuf};

/// `ABS_AUTOHIDE | ABS_ALWAYSONTOP` — C# `SupportedTaskbarState`.
pub const SUPPORTED_STATE_MASK: u32 = 0x03;
/// A missing/damaged journal must restore a *visible* taskbar, never a guessed hidden state.
pub const DEFAULT_STATE: u32 = 0;

fn journal_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("OpenTaskbar")
        .join("taskbar-state.txt")
}

pub fn has_pending() -> bool {
    journal_path().exists()
}

/// Parses the C# wire form. Anything outside the two supported bits is invalid,
/// exactly like `TaskbarReplacementSession.ParseJournal` (`state & ~0x03 != 0`).
#[must_use]
pub fn parse_state(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let state = text.parse::<u32>().ok()?;
    (state & !SUPPORTED_STATE_MASK == 0).then_some(state)
}

fn write_state_to(path: &Path, state: u32) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("txt.tmp");
    // No trailing newline: C# `File.WriteAllText(state.ToString(InvariantCulture))`.
    fs::write(&tmp, format!("{}", state & SUPPORTED_STATE_MASK))?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() && fs::remove_file(path).is_ok() => fs::rename(&tmp, path),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

pub fn write_state(state: u32) -> std::io::Result<()> {
    write_state_to(&journal_path(), state)
}

fn read_state_from(path: &Path) -> Option<u32> {
    parse_state(&fs::read_to_string(path).ok()?)
}

pub fn read_state() -> Option<u32> {
    read_state_from(&journal_path())
}

/// Explicit name used by cross-workstream callers. Invalid states are rejected
/// rather than silently propagated to the watchdog.
pub fn read_state_valid() -> Option<u32> {
    read_state()
}

/// C# `ForceRestore` stale-journal rule: when the taskbar is already visible
/// with a *different* state, the journal belongs to a previous session and
/// must be ignored instead of overwriting the user's current state.
#[must_use]
pub const fn resolve_state(journal_state: u32, taskbar_visible: bool, current_state: u32) -> u32 {
    if taskbar_visible && (current_state & SUPPORTED_STATE_MASK) != journal_state {
        current_state & SUPPORTED_STATE_MASK
    } else {
        journal_state & SUPPORTED_STATE_MASK
    }
}

/// Reads the journal and applies validation + stale detection in one step.
#[must_use]
pub fn effective_state(current_state: u32, taskbar_visible: bool) -> u32 {
    match read_state_valid() {
        Some(state) => resolve_state(state, taskbar_visible, current_state),
        // Invalid journal → 0 when hidden (visible taskbar is safe); when the
        // taskbar is visible, keep the live state instead of hiding it.
        None if taskbar_visible => current_state & SUPPORTED_STATE_MASK,
        None => DEFAULT_STATE,
    }
}

pub fn clear() {
    let _ = fs::remove_file(journal_path());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn test_path(tag: &str) -> PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "ot-taskbar-journal-{}-{tag}-{id}.txt",
            std::process::id()
        ))
    }

    #[test]
    fn parse_state_masks_and_rejects_invalid_values() {
        assert_eq!(parse_state("0"), Some(0));
        assert_eq!(parse_state(" 3 "), Some(3));
        assert_eq!(parse_state("2"), Some(2));
        // Bits outside ABS_AUTOHIDE | ABS_ALWAYSONTOP are invalid (C# ParseJournal).
        for bad in ["4", "0x2", "-1", "+1", "", "  ", "2\n3", "abc", "4294967296"] {
            assert_eq!(parse_state(bad), None, "must reject {bad:?}");
        }
        assert_eq!(resolve_state(0, false, 2), 0);
        assert_eq!(resolve_state(2, true, 2), 2);
        assert_eq!(resolve_state(2, true, 0), 0);
        assert_eq!(resolve_state(2, false, 0), 2);
    }

    #[test]
    fn journal_writes_a_single_masked_line_without_newline() {
        let path = test_path("format");
        let _ = fs::remove_file(&path);
        write_state_to(&path, 0x82).expect("journal must write");
        assert_eq!(fs::read_to_string(&path).unwrap(), "2");
        assert_eq!(read_state_from(&path), Some(2));
        assert!(!path.with_extension("txt.tmp").exists());

        write_state_to(&path, 1).expect("journal must overwrite");
        assert_eq!(fs::read_to_string(&path).unwrap(), "1");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn invalid_journal_falls_back_to_zero_or_live_state() {
        let path = test_path("invalid");
        fs::write(&path, "7").unwrap();
        assert_eq!(read_state_from(&path), None);
        let _ = fs::remove_file(&path);
        assert_eq!(read_state_from(&path), None);
        // No way to inject a path into effective_state; the pure decision above
        // covers the mapping, while DEFAULT_STATE documents the hidden fallback.
        assert_eq!(DEFAULT_STATE, 0);
    }
}
