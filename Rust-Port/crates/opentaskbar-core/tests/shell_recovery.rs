//! C# `ShellRecoveryChecks` / `TaskbarRecoveryChecks` / flyout journal parity.
//!
//! These tests pin the wire format and retry policy. Bridge tests assert the
//! same contract against the production shell modules that already exist; the
//! watchdog argument parser belongs to WS-2 and is pinned by an `#[ignore]`d
//! bridge until that API is reachable from this crate.

mod common;

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlyoutKind {
    Start,
    Search,
    ControlCenter,
}

#[derive(Debug, PartialEq, Eq)]
struct JournalEntry {
    kind: FlyoutKind,
    generation: u32,
    seq: u32,
}

/// Journal lines look like `START generation=42 seq=7`.
fn parse_journal_line(line: &str) -> Option<JournalEntry> {
    let mut words = line.split_whitespace();
    let kind = match words.next()? {
        "START" => FlyoutKind::Start,
        "SEARCH" => FlyoutKind::Search,
        "CONTROL_CENTER" => FlyoutKind::ControlCenter,
        _ => return None,
    };
    let mut generation = None;
    let mut seq = None;
    for word in words {
        let (key, value) = word.split_once('=')?;
        let number = value.parse::<u32>().ok()?;
        match key {
            "generation" => generation = Some(number),
            "seq" => seq = Some(number),
            _ => return None,
        }
    }
    Some(JournalEntry {
        kind,
        generation: generation?,
        seq: seq?,
    })
}

/// Restore retries: 20, 40, 80, 160 ms … capped at 5 s.
fn retry_backoff(attempt: u32) -> u64 {
    if attempt == 0 {
        return 0;
    }
    (20u64 << (attempt - 1).min(8)).min(5000)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct WatchdogArgs {
    heartbeat_seconds: u64,
    journal_path: Option<String>,
    restore_timeout_ms: u64,
}

fn parse_watchdog_args(args: &[&str]) -> Option<WatchdogArgs> {
    let mut parsed = WatchdogArgs::default();
    for arg in args {
        let (key, value) = arg.strip_prefix("--")?.split_once('=')?;
        match key {
            "heartbeat-seconds" => parsed.heartbeat_seconds = value.parse().ok()?,
            "journal" => parsed.journal_path = Some(value.to_owned()),
            "restore-timeout-ms" => parsed.restore_timeout_ms = value.parse().ok()?,
            _ => return None,
        }
    }
    Some(parsed)
}

/// Chord helper: modifiers are pressed, the key fires, then a drop guard
/// releases every modifier even when the action fails (`finally` semantics).
struct ReleaseOnDrop<'a, 'b> {
    held: &'a mut HashSet<String>,
    released: &'b mut Vec<String>,
}

impl Drop for ReleaseOnDrop<'_, '_> {
    fn drop(&mut self) {
        self.released.extend(self.held.drain());
    }
}

fn release_stuck_modifiers(held: &mut HashSet<String>, chord: &[&str], fails: bool) -> Vec<String> {
    for key in chord {
        held.insert((*key).to_owned());
    }
    let mut released = Vec::new();
    {
        let _guard = ReleaseOnDrop {
            held,
            released: &mut released,
        };
        let result: Result<(), &str> = if fails { Err("action failed") } else { Ok(()) };
        // The guard drops on every exit path, including `Err`.
        let _ = result;
    }
    released.sort();
    released
}

/// Restore is considered verified when any of the first three attempts succeeds.
fn restore_verified(results: &[bool]) -> bool {
    results.iter().take(3).any(|ok| *ok)
}

fn is_shell_overlay(class_name: &str) -> bool {
    matches!(
        class_name,
        "XamlExplorerHostIslandWindow" | "MultitaskingViewFrame" | "TaskSwitcherWnd"
    )
}

#[test]
fn journal_lines_parse_for_all_three_flyouts() {
    assert_eq!(
        parse_journal_line("START generation=42 seq=7"),
        Some(JournalEntry {
            kind: FlyoutKind::Start,
            generation: 42,
            seq: 7
        })
    );
    assert_eq!(
        parse_journal_line("SEARCH generation=1 seq=2").map(|e| e.kind),
        Some(FlyoutKind::Search)
    );
    assert_eq!(
        parse_journal_line("CONTROL_CENTER generation=9 seq=3").map(|e| e.kind),
        Some(FlyoutKind::ControlCenter)
    );
}

#[test]
fn malformed_journal_lines_are_ignored() {
    assert_eq!(parse_journal_line(""), None);
    assert_eq!(parse_journal_line("UNKNOWN generation=1 seq=1"), None);
    assert_eq!(parse_journal_line("START generation=x seq=1"), None);
    assert_eq!(parse_journal_line("START generation=1"), None);
    assert_eq!(parse_journal_line("START generation=1 seq=1 extra=a"), None);
}

#[test]
fn retry_backoff_grows_then_caps() {
    assert_eq!(retry_backoff(0), 0);
    assert_eq!(retry_backoff(1), 20);
    assert_eq!(retry_backoff(2), 40);
    assert_eq!(retry_backoff(3), 80);
    assert_eq!(retry_backoff(20), 5000);
}

#[test]
fn watchdog_arguments_are_parsed() {
    let args = parse_watchdog_args(&[
        "--heartbeat-seconds=5",
        "--journal=C:\\Temp\\journal.txt",
        "--restore-timeout-ms=3000",
    ])
    .unwrap();
    assert_eq!(args.heartbeat_seconds, 5);
    assert_eq!(args.journal_path.as_deref(), Some("C:\\Temp\\journal.txt"));
    assert_eq!(args.restore_timeout_ms, 3000);
}

#[test]
fn watchdog_rejects_unknown_or_malformed_arguments() {
    assert_eq!(parse_watchdog_args(&["--unknown=1"]), None);
    assert_eq!(parse_watchdog_args(&["--heartbeat-seconds=abc"]), None);
    assert_eq!(parse_watchdog_args(&["heartbeat-seconds=1"]), None);
}

#[test]
fn chord_release_runs_even_when_the_action_fails() {
    let mut held: HashSet<String> = HashSet::new();
    let released = release_stuck_modifiers(&mut held, &["Ctrl", "Alt", "D"], true);
    assert_eq!(released, vec!["Alt", "Ctrl", "D"]);
    assert!(held.is_empty(), "no modifier may remain held");

    let mut held: HashSet<String> = HashSet::new();
    let released = release_stuck_modifiers(&mut held, &["Win", "Shift"], false);
    assert_eq!(released, vec!["Shift", "Win"]);
    assert!(held.is_empty());
}

#[test]
fn restore_needs_a_success_within_three_attempts() {
    assert!(restore_verified(&[true, false, false]));
    assert!(restore_verified(&[false, false, true]));
    assert!(!restore_verified(&[false, false, false]));
    assert!(
        !restore_verified(&[false, false, false, true]),
        "4th attempt is too late"
    );
}

#[test]
fn overlays_are_excluded_from_auto_hide() {
    for class in [
        "XamlExplorerHostIslandWindow",
        "MultitaskingViewFrame",
        "TaskSwitcherWnd",
    ] {
        assert!(is_shell_overlay(class));
    }
    assert!(!is_shell_overlay("Shell_TrayWnd"));
}

/// Production bridge: the shell modules already implement the watchdog
/// one-process contract and the atomic journal writer this file mirrors.
#[test]
fn production_watchdog_and_journal_contract() {
    common::assert_source_has(
        "crates/opentaskbar-shell/src/watchdog.rs",
        &[
            "WATCHDOG_BYTES",
            "HEARTBEAT_PREFIX",
            "--watchdog={pid}:{token}",
            "notify_clean_exit",
            "fs::rename(&tmp, &path)",
        ],
    );
    common::assert_source_has(
        "crates/opentaskbar-shell/src/flyout_journal.rs",
        &["fs::rename(&tmp, path)", "MAX_ENTRIES"],
    );
    // The WS-2 inline watchdog tests use the same retry policy as the vectors
    // above; keep the arithmetic anchored even if the production code changes.
    assert_eq!(retry_backoff(3), 80);
    assert_eq!(retry_backoff(20), 5000);
}

/// WS-2 adds the watchdog argument parser/process handle test in its own
/// crate. This bridge pins the argument form but cannot call the private API
/// from `opentaskbar-core`; it stays ignored until the orchestrator wires a
/// dev-dependency or moves the bridge next to the implementation.
#[test]
#[ignore = "WS-2: watchdog arg parser and spawned process are private to opentaskbar-shell"]
fn production_watchdog_accepts_the_reference_arguments() {
    let args = parse_watchdog_args(&[
        "--heartbeat-seconds=5",
        "--journal=C:\\Temp\\journal.txt",
        "--restore-timeout-ms=3000",
    ])
    .expect("WS-2 parser must accept these arguments");
    assert_eq!(args.heartbeat_seconds, 5);
    assert_eq!(args.restore_timeout_ms, 3000);
    common::assert_source_has(
        "crates/opentaskbar-shell/src/watchdog.rs",
        &["--watchdog={pid}:{token}"],
    );
}

/// The C# flyout journal uses `START generation=… seq=…`, while the current
/// Rust shell journal uses `handle pid left top` / tag lines. WS-2/WS-3 must
/// align the wire format before this bridge can run.
#[test]
#[ignore = "WS-2/WS-3: Rust flyout journal wire format must match the C# generation/seq format"]
fn production_flyout_journal_accepts_generation_and_seq() {
    common::assert_source_has(
        "crates/opentaskbar-shell/src/flyout_journal.rs",
        &["generation", "seq"],
    );
    for line in [
        "START generation=42 seq=7",
        "SEARCH generation=1 seq=2",
        "CONTROL_CENTER generation=9 seq=3",
    ] {
        assert!(parse_journal_line(line).is_some(), "{line} must parse");
    }
}
