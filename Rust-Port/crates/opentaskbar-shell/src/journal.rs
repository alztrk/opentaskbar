//! Recovery journal recording Explorer's taskbar state before hiding it.
//! Uses `%LOCALAPPDATA%\OpenTaskbar\taskbar-state.txt` so the native C
//! `OpenTaskbarWatchdog.exe` reads the exact same file on crash/kill recovery.

use std::fs;
use std::path::PathBuf;

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

pub fn write_state(state: u32) -> std::io::Result<()> {
    let path = journal_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{state}\n"))?;
    fs::rename(&tmp, &path)
}

pub fn read_state() -> Option<u32> {
    let raw = fs::read_to_string(journal_path()).ok()?;
    raw.trim().parse::<u32>().ok()
}

pub fn clear() {
    let _ = fs::remove_file(journal_path());
}
