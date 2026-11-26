//! C# `ShellRecoveryChecks` / `TaskbarRecoveryChecks` / flyout journal parity
//! (pure portion). WS-B owns the journal writer and watchdog process, which are
//! not in this worktree's baseline; these tests pin the wire format and retry
//! policy they must honour.

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

/// Chord helper: modifiers are pressed, the key fires, then `finally` releases
/// every modifier even when the action fails.
fn release_stuck_modifiers(held: &mut HashSet<String>, chord: &[&str], fails: bool) -> Vec<String> {
    for key in chord {
        held.insert((*key).to_owned());
    }
    let action_result = if fails { Err("action failed") } else { Ok(()) };
    let mut released: Vec<String> = held.iter().cloned().collect();
    held.clear();
    let _ = action_result; // `finally` block runs regardless
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
