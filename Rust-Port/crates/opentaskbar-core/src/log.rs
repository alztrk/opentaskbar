//! Minimal zero-thread append-only logger with size-capped rotation.
//!
//! In addition to the level-tagged lines, this module hosts the structured
//! shell telemetry used by the recovery/shell integration layer:
//! `EVENT <name> seq=<n> key=value ...` and
//! `Result(reason/ok/verified/ms/error)` records.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 512 * 1024;

static LOG_PATH: OnceLock<Mutex<PathBuf>> = OnceLock::new();
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

pub fn debug(message: impl AsRef<str>) {
    if std::env::var_os("OT_VERBOSE_LOG").is_some_and(|v| v == "1") {
        write_line("DEBUG", message.as_ref());
    }
}

// ---- Structured shell telemetry -------------------------------------------------

/// Monotonically increasing per-process event/broadcast sequence (never 0).
/// Mirrors C# `ShellTelemetry.NextSequence()` so a broadcast we sent ourselves
/// can be distinguished from a real Explorer restart during log correlation.
pub fn next_sequence() -> u64 {
    SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
}

/// Writes a shell event with named fields in `key=value` form.
/// Use the `shell_event!` macro for the C#-style `EVENT name key=value` sugar.
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
/// optional extra fields. Mirrors C# `ShellTelemetry.Result(...)`.
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

/// Builds `EVENT name key=value` fields with the same shape as C# `ShellTelemetry`.
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

/// Resolves a localization key the way C# `AppLog.ResolveMessage` does. The
/// locale catalog is owned by WS-H; core deliberately falls back to the key
/// itself (and substitutes `{0}`-style arguments) so logging never depends on
/// a UI crate or throws on a missing key.
pub fn resolve_message(message_key: &str, arguments: &[&str]) -> String {
    if message_key.trim().is_empty() {
        return String::new();
    }
    let mut resolved = message_key.to_owned();
    for (index, argument) in arguments.iter().enumerate() {
        resolved = resolved.replace(&format!("{{{index}}}"), argument);
    }
    resolved
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
}
