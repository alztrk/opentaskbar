//! App shortcut / display-name resolution (`AppShortcutResolver.cs` parity).
//!
//! - `NameCache` maps `displayName` -> `stem` and back, so a pinned shortcut keeps its label
//!   when the executable name differs (e.g. `chrome.exe` -> `Google Chrome`).
//! - Web-helper markers (`--app=`, `msedgewebview2.exe`, `WebHelper`) prevent browser helper
//!   processes from becoming dock items.
//! - `.lnk` argument/index extraction handles Explorer special cases and `WorkingDirectory`.
//! - Ancestor matching associates a child process with its pinned parent.

use std::collections::HashMap;
use std::path::{Component, Path};

/// Bidirectional name cache (`NameCache`).
#[derive(Debug, Default)]
pub struct NameCache {
    by_display: HashMap<String, String>,
    by_stem: HashMap<String, String>,
}

impl NameCache {
    pub fn insert(&mut self, display_name: &str, stem: &str) {
        let d = display_name.trim().to_ascii_lowercase();
        let s = stem.trim().to_ascii_lowercase();
        if d.is_empty() || s.is_empty() {
            return;
        }
        self.by_display.insert(d.clone(), s.clone());
        self.by_stem.insert(s, d);
    }

    pub fn stem_for(&self, display_name: &str) -> Option<&str> {
        self.by_display
            .get(&display_name.trim().to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn display_for(&self, stem: &str) -> Option<&str> {
        self.by_stem
            .get(&stem.trim().to_ascii_lowercase())
            .map(String::as_str)
    }
}

/// True for browser web helpers that must never be pinned on their own.
pub fn is_webhelper(name_or_args: &str) -> bool {
    let s = name_or_args.to_ascii_lowercase();
    s.contains("webhelper")
        || s.contains("msedgewebview2")
        || s.contains("chromewebhelper")
        || s.contains("--type=renderer")
        || s.contains("--type=utility")
        || s.contains("--type=gpu-process")
}

/// Extracts the `--app-id=` / `--app=` index from shortcut arguments (reference WScript.Shell index).
pub fn extract_app_argument_index(args: &str) -> Option<usize> {
    for token in args.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        if let Some(rest) = lower
            .strip_prefix("--app-id=")
            .or_else(|| lower.strip_prefix("--app="))
            && let Ok(v) = rest.trim_matches('"').parse::<usize>()
        {
            return Some(v);
        }
    }
    None
}

/// Explorer special case: `explorer.exe` is a shell host; only explicit folder/file arguments
/// should be treated as launch targets.
pub fn is_explorer_special_case(executable: &str, args: &str) -> bool {
    let exe = Path::new(executable.trim())
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    exe == "explorer"
        && (args.trim().is_empty()
            || args.contains("/select,")
            || args.contains("/root,")
            || args.trim().starts_with("::{"))
}

/// Resolved shortcut target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutTarget {
    pub executable: String,
    pub arguments: String,
    pub working_directory: String,
    pub display_name: String,
    pub app_argument_index: Option<usize>,
}

/// Parses a `.lnk`-style target string (`path|args|workdir|displayName`) without COM, so it is
/// testable and usable on a background thread.
pub fn parse_shortcut_target(raw: &str) -> Option<ShortcutTarget> {
    let parts: Vec<&str> = raw.split('|').collect();
    let executable = parts.first()?.trim();
    if executable.is_empty() {
        return None;
    }
    let arguments = parts.get(1).copied().unwrap_or("").trim().to_owned();
    let working_directory = parts.get(2).copied().unwrap_or("").trim().to_owned();
    let display_name = parts
        .get(3)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            Path::new(executable)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| executable.to_owned())
        });
    let app_argument_index = extract_app_argument_index(&arguments);
    Some(ShortcutTarget {
        executable: executable.to_owned(),
        arguments,
        working_directory,
        display_name,
        app_argument_index,
    })
}

/// True when `child` is inside `ancestor` (case-insensitive, component-wise).
pub fn is_ancestor(ancestor: &str, child: &str) -> bool {
    let a = Path::new(ancestor.trim());
    let c = Path::new(child.trim());
    let ac: Vec<String> = a
        .components()
        .filter_map(|p| match p {
            Component::Normal(s) => Some(s.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    let cc: Vec<String> = c
        .components()
        .filter_map(|p| match p {
            Component::Normal(s) => Some(s.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    !ac.is_empty() && cc.len() > ac.len() && cc[..ac.len()] == ac[..]
}

/// Resolves a display name for an executable path using the cache, falling back to its stem.
pub fn resolve_display_name(path: &str, cache: &NameCache) -> String {
    let stem = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned());
    cache.display_for(&stem).map(str::to_owned).unwrap_or(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_cache_maps_both_directions() {
        let mut cache = NameCache::default();
        cache.insert("Google Chrome", "chrome");
        assert_eq!(cache.stem_for("  GOOGLE CHROME "), Some("chrome"));
        assert_eq!(cache.display_for("CHROME"), Some("google chrome"));
        assert_eq!(
            resolve_display_name(r"C:\App\chrome.exe", &cache),
            "google chrome"
        );
    }

    #[test]
    fn webhelper_marker_detection() {
        assert!(is_webhelper("msedgewebview2.exe"));
        assert!(is_webhelper("chrome.exe --type=renderer"));
        assert!(!is_webhelper("chrome.exe --app-id=3"));
    }

    #[test]
    fn shortcut_argument_index_and_explorer() {
        assert_eq!(extract_app_argument_index("--app-id=42 --flag"), Some(42));
        assert_eq!(extract_app_argument_index("plain"), None);
        assert!(is_explorer_special_case("C:\\Windows\\explorer.exe", ""));
        assert!(is_explorer_special_case(
            "explorer.exe",
            "/select,C:\\a.txt"
        ));
        assert!(!is_explorer_special_case("explorer.exe", r"C:\folder"));
    }

    #[test]
    fn shortcut_target_parsing() {
        let target =
            parse_shortcut_target(r"C:\Program Files\App\app.exe|--app-id=7|D:\work|My App")
                .unwrap();
        assert_eq!(target.arguments, "--app-id=7");
        assert_eq!(target.working_directory, r"D:\work");
        assert_eq!(target.display_name, "My App");
        assert_eq!(target.app_argument_index, Some(7));
        let fallback = parse_shortcut_target(r"C:\Tools\thing.exe").unwrap();
        assert_eq!(fallback.display_name, "thing");
    }

    #[test]
    fn ancestor_match_is_componentwise() {
        assert!(is_ancestor(
            r"C:\Program Files\App",
            r"C:\Program Files\App\a.exe"
        ));
        assert!(!is_ancestor(
            r"C:\Program Files\App",
            r"C:\Program Files\AppOther\a.exe"
        ));
        assert!(!is_ancestor("", r"C:\a.exe"));
    }
}
