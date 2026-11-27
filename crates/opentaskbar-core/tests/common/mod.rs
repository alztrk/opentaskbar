//! Helpers for the "production bridge" tests.
//!
//! The integration tests live in `opentaskbar-core`, but several behaviours
//! they pin are implemented in private functions of `opentaskbar-shell` /
//! `opentaskbar-app` (both owned by other workstreams, and the app is a binary
//! crate with no library target). Adding a dev-dependency would be circular, so
//! these helpers read the production source files from the workspace checkout
//! and assert their contract there. A production regression therefore fails a
//! test even when the pure reference implementation is unchanged.
//!
//! Nothing here calls a not-yet-existing API: every reference test keeps
//! compiling. Private app/shell helpers now have in-crate unit tests that call
//! the real functions; the sole remaining `#[ignore]` is a local reference
//! vector for `apply_pinned_groups` (the app-side vector test is active).

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Repository/workspace root, derived from the core crate manifest at
/// `crates/opentaskbar-core` (two levels below the root).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root must exist")
}

/// A production source file, relative to the repository root.
pub fn source(relative_path: &str) -> String {
    let path = workspace_root().join(relative_path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("missing production source {}: {error}", path.display()))
}

/// Assert that a production file still contains each required fragment.
pub fn assert_source_has(relative_path: &str, needles: &[&str]) {
    let text = source(relative_path);
    for needle in needles {
        assert!(
            text.contains(needle),
            "production source {relative_path} no longer contains `{needle}`"
        );
    }
}

/// Assert that a production file no longer contains a forbidden fragment.
pub fn assert_source_lacks(relative_path: &str, needles: &[&str]) {
    let text = source(relative_path);
    for needle in needles {
        assert!(
            !text.contains(needle),
            "production source {relative_path} still contains `{needle}`"
        );
    }
}

/// Read a numeric `const NAME: T = <literal>;` from a production source file.
/// Handles `0x` hex and digit separators (`_`).
pub fn numeric_const(text: &str, name: &str) -> Option<u64> {
    let marker = format!("{name}:");
    let after_name = &text[text.find(&marker)? + marker.len()..];
    let after_eq = &after_name[after_name.find('=')? + 1..];
    let literal = after_eq.split(';').next()?.trim();
    let clean: String = literal.chars().filter(|c| *c != '_').collect();
    match clean.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => clean.parse().ok(),
    }
}

/// Read a `const NAME: &str = "value";` from a production source file.
pub fn string_const(text: &str, name: &str) -> Option<String> {
    let marker = format!("{name}:");
    let after_name = &text[text.find(&marker)? + marker.len()..];
    let after_eq = &after_name[after_name.find('=')? + 1..];
    let literal = after_eq.split(';').next()?.trim();
    let value = literal
        .strip_prefix('r')
        .unwrap_or(literal)
        .strip_prefix('"')?;
    Some(value.trim_end_matches('"').to_owned())
}

/// Read a byte-array constant such as `const PREFIX: [u8; 3] = [0x2A, 0x2B, 0x0E];`.
pub fn byte_array_const(text: &str, name: &str) -> Option<Vec<u8>> {
    let at = text.find(name)?;
    // Skip the `[u8; N]` type annotation by starting after the `=`.
    let equals = text[at..].find('=')? + at;
    let open = text[equals..].find('[')? + equals;
    let close = text[open..].find(']')? + open;
    text[open + 1..close]
        .split(',')
        .map(|token| {
            let token = token.trim();
            match token.strip_prefix("0x") {
                Some(hex) => u8::from_str_radix(hex, 16).ok(),
                None => token.parse().ok(),
            }
        })
        .collect()
}
