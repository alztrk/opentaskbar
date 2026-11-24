//! Minimal zero-thread append-only logger with size-capped rotation.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 512 * 1024;

static LOG_PATH: OnceLock<Mutex<PathBuf>> = OnceLock::new();

pub fn init(path: PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = LOG_PATH.set(Mutex::new(path));
}

fn write_line(level: &str, message: &str) {
    let Some(lock) = LOG_PATH.get() else {
        return;
    };
    let Ok(path) = lock.lock() else {
        return;
    };
    if fs::metadata(&*path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let backup = path.with_extension("log.old");
        let _ = fs::rename(&*path, backup);
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&*path) {
        let _ = writeln!(file, "[{ts}] [{level}] {message}");
    }
}

pub fn info(message: impl AsRef<str>) {
    write_line("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write_line("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write_line("ERROR", message.as_ref());
}
