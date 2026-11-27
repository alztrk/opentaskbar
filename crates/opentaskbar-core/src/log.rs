//! Minimal zero-thread append-only logger with size-capped rotation.
//!
//! In addition to the level-tagged lines, this module hosts the structured
//! shell telemetry used by the recovery/shell integration layer:
//! `EVENT <name> seq=<n> key=value ...` and
//! `Result(reason/ok/verified/ms/error)` records.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 512 * 1024;
/// Rotation suffix shared with the reference build's log naming convention.
pub const RUST_LOG_BACKUP_FILE_NAME: &str = "opentaskbar.log.old";

static LOG_PATH: OnceLock<Mutex<PathBuf>> = OnceLock::new();
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn init(path: PathBuf) {
    // Honor the caller's path. The historic "legacy name" is the same string as the current
    // Rust log name, so redirecting `...\OpenTaskbar\opentaskbar.log` silently discarded any
    // relocated/LOCALAPPDATA override supplied by the caller.
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if LOG_PATH.set(Mutex::new(path)).is_err() {
        // `eprintln!` rather than `warn!`: the logger is already initialized and writing a
        // warning through it would obscure the fact that this call was ignored.
        eprintln!("log: init called more than once; keeping the first log path");
    }
}

/// Rotation destination. The file is always `opentaskbar.log.old`, never
/// the `log.old.txt`, even when a caller supplies a legacy log path.
fn rotated_log_path(path: &Path) -> PathBuf {
    let is_rust_log = path.file_name().is_some_and(|name| {
        name.eq_ignore_ascii_case(crate::paths::RUST_LOG_FILE_NAME)
            || name.eq_ignore_ascii_case(crate::paths::LEGACY_RUST_LOG_FILE_NAME)
    });
    if is_rust_log {
        path.with_file_name(RUST_LOG_BACKUP_FILE_NAME)
    } else {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        path.with_file_name(format!("{name}.old"))
    }
}

fn write_line(level: &str, message: &str) {
    let Some(lock) = LOG_PATH.get() else {
        return;
    };
    let Ok(path) = lock.lock() else {
        return;
    };
    if fs::metadata(&*path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let backup = rotated_log_path(&path);
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

pub fn debug(message: impl AsRef<str>) {
    if std::env::var_os("OT_VERBOSE_LOG").is_some_and(|v| v == "1") {
        write_line("DEBUG", message.as_ref());
    }
}

// ---- Structured shell telemetry -------------------------------------------------

/// Monotonically increasing per-process event/broadcast sequence (never 0).
/// Mirrors `ShellTelemetry.NextSequence()` so a broadcast we sent ourselves
/// can be distinguished from a real Explorer restart during log correlation.
pub fn next_sequence() -> u64 {
    SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
}

/// Writes a shell event with named fields in `key=value` form.
/// Use the `shell_event!` macro for the reference-style `EVENT name key=value` sugar.
pub fn event(name: &str, fields: &[(&str, String)]) {
    if fields.is_empty() {
        info(format!("EVENT {name}"));
        return;
    }
    let parts: Vec<String> = fields
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    info(format!("EVENT {name} {}", parts.join(" ")));
}

/// Logs a reconciliation/repair result with reason, success, verification and
/// optional extra fields. Mirrors `ShellTelemetry.Result(...)`.
pub fn result(operation: &str, reason: &str, ok: bool, verified: bool, fields: &[(&str, String)]) {
    let mut all = Vec::with_capacity(fields.len() + 3);
    all.push(("reason", reason.to_owned()));
    all.push(("ok", ok.to_string()));
    all.push(("verified", verified.to_string()));
    all.extend(fields.iter().map(|(k, v)| (*k, v.clone())));
    event(operation, &all);
}

/// Returns elapsed whole milliseconds since `start`, for `ms=` telemetry fields.
pub fn elapsed_ms(start: Instant) -> u128 {
    start.elapsed().as_millis()
}

/// Builds `EVENT name key=value` fields with the same shape as `ShellTelemetry`.
#[macro_export]
macro_rules! shell_event {
    ($name:expr $(, $key:ident = $value:expr)* $(,)?) => {{
        let fields: &[(&str, String)] = &[ $( (stringify!($key), ::std::string::ToString::to_string(&$value)) ),* ];
        $crate::log::event($name, fields);
    }};
}

/// Builds a `Result(reason/ok/verified)` event with optional extra fields.
#[macro_export]
macro_rules! shell_result {
    ($operation:expr, $reason:expr, $ok:expr, $verified:expr $(, $key:ident = $value:expr)* $(,)?) => {{
        let fields: &[(&str, String)] = &[ $( (stringify!($key), ::std::string::ToString::to_string(&$value)) ),* ];
        $crate::log::result($operation, $reason, $ok, $verified, fields);
    }};
}

// ---- Failure reporting ----------------------------------------------------------

/// Resolves a localization key the way `AppLog.ResolveMessage` does. The
/// locale catalog is owned by WS-H; core deliberately falls back to the key
/// itself (and substitutes `{0}`-style arguments) so logging never depends on
/// a UI crate or throws on a missing key.
pub fn resolve_message(message_key: &str, arguments: &[&str]) -> String {
    if message_key.trim().is_empty() {
        return String::new();
    }
    // Same single-pass substitution as `i18n::t_fmt`: sequential `str::replace` calls also
    // rewrote `{n}` sequences that arrived inside previously inserted argument text.
    crate::i18n::substitute_placeholders(message_key, arguments)
}

/// Writes one structured failure record (localization key + resolved text).
pub fn report_error(message_key: &str, arguments: &[&str]) {
    error(format!(
        "Failure key={message_key} | message={}",
        resolve_message(message_key, arguments)
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_log_rotation_uses_the_shared_backup_name() {
        let log = Path::new(r"C:\Users\Test\AppData\Local\OpenTaskbar\opentaskbar.log");
        assert_eq!(
            rotated_log_path(log),
            log.with_file_name(RUST_LOG_BACKUP_FILE_NAME)
        );
        // Legacy callers get the same rotation name.
        let legacy = log.with_file_name("opentaskbar.log");
        assert_eq!(
            rotated_log_path(&legacy),
            log.with_file_name(RUST_LOG_BACKUP_FILE_NAME)
        );
        // A relocated/custom log keeps its own name plus `.old`.
        let custom = Path::new(r"C:\Custom\app.log");
        assert_eq!(
            rotated_log_path(custom),
            Path::new(r"C:\Custom\app.log.old")
        );
    }

    #[test]
    fn telemetry_sequence_never_starts_at_zero() {
        assert!(next_sequence() > 0);
        assert!(next_sequence() > 0);
    }

    #[test]
    fn resolve_message_substitutes_indexed_arguments_and_falls_back_to_key() {
        assert_eq!(resolve_message("ErrorFile", &["C:\\x"]), "ErrorFile");
        assert_eq!(resolve_message("Error {0} {1}", &["a", "b"]), "Error a b");
        assert_eq!(resolve_message("", &[]), "");
    }

    #[test]
    fn resolve_message_is_single_pass_like_i18n_t_fmt() {
        // Sequential `replace` calls would rewrite the `{1}` coming from argument 0.
        assert_eq!(resolve_message("{0} {1}", &["{1}", "b"]), "{1} b");
        // Multi-digit indices must not be parsed as index 1 followed by a literal '0'.
        let args: Vec<String> = (0..11).map(|i| format!("a{i}")).collect();
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        assert_eq!(resolve_message("{10} / {1}", &refs), "a10 / a1");
        assert_eq!(resolve_message("{0} {1}", &["{2}", "{3}"]), "{2} {3}");
    }
}
