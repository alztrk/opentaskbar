//! Single source of truth for the files shared by the C# and Rust builds.
//!
//! Both builds write under `%LOCALAPPDATA%\OpenTaskbar`. The Rust-only files keep
//! distinct names (for example `opentaskbar-rust.log`) so they never collide with
//! the C# `log.txt` / `settings.json` files inside that folder.

use std::path::{Path, PathBuf};

/// Folder name used by the C# build since v0.1.
pub const APP_DATA_FOLDER_NAME: &str = "OpenTaskbar";
pub const SETTINGS_FILE_NAME: &str = "settings.json";
pub const RUST_LOG_FILE_NAME: &str = "opentaskbar-rust.log";
pub const LEGACY_RUST_LOG_FILE_NAME: &str = "opentaskbar.log";
pub const THEMES_FOLDER_NAME: &str = "Themes";
pub const SECRETS_FOLDER_NAME: &str = "secrets";
pub const WATCHDOG_FILE_NAME: &str = "OpenTaskbarWatchdog.exe";
pub const RENDER_MODE_FILE_NAME: &str = "render-mode.json";
pub const RUST_CACHE_DIR: &str = "rust-cache/icon-cache";

fn from_base(base: &Path) -> PathBuf {
    base.join(APP_DATA_FOLDER_NAME)
}

/// `%LOCALAPPDATA%\OpenTaskbar`; falls back to `%TEMP%\OpenTaskbar` when the
/// environment variable is absent (e.g. tests or a stripped-down service host).
pub fn shared_app_data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    from_base(&base)
}

pub fn settings_path() -> PathBuf {
    shared_app_data_dir().join(SETTINGS_FILE_NAME)
}

pub fn rust_log_path() -> PathBuf {
    shared_app_data_dir().join(RUST_LOG_FILE_NAME)
}

pub fn theme_root() -> PathBuf {
    shared_app_data_dir().join(THEMES_FOLDER_NAME)
}

pub fn secrets_dir() -> PathBuf {
    shared_app_data_dir().join(SECRETS_FOLDER_NAME)
}

pub fn watchdog_path() -> PathBuf {
    shared_app_data_dir().join(WATCHDOG_FILE_NAME)
}

pub fn render_mode_path() -> PathBuf {
    shared_app_data_dir().join(RENDER_MODE_FILE_NAME)
}

pub fn rust_cache_dir() -> PathBuf {
    shared_app_data_dir().join(RUST_CACHE_DIR)
}

/// Journal files (`taskbar-state.txt`, `flyout-positions.txt`, ...) live in the
/// same root as the C# build so the native watchdog can read them unchanged.
pub fn journal_path(name: &str) -> PathBuf {
    shared_app_data_dir().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_shared_paths_live_under_the_csharp_folder_name() {
        let base = Path::new(r"C:\Users\Test\AppData\Local");
        let root = from_base(base);
        assert_eq!(root, base.join("OpenTaskbar"));
        assert_eq!(
            root.join("settings.json"),
            from_base(base).join(SETTINGS_FILE_NAME)
        );
        assert_eq!(
            root.join("opentaskbar-rust.log"),
            from_base(base).join(RUST_LOG_FILE_NAME)
        );
        assert_eq!(
            root.join("Themes"),
            from_base(base).join(THEMES_FOLDER_NAME)
        );
        assert_eq!(
            root.join("secrets"),
            from_base(base).join(SECRETS_FOLDER_NAME)
        );
        assert_eq!(
            root.join("rust-cache/icon-cache"),
            from_base(base).join(RUST_CACHE_DIR)
        );
        assert_eq!(
            root.join("taskbar-state.txt"),
            from_base(base).join("taskbar-state.txt")
        );
        assert_eq!(
            root.join("OpenTaskbarWatchdog.exe"),
            from_base(base).join(WATCHDOG_FILE_NAME)
        );
        assert_eq!(
            root.join("render-mode.json"),
            from_base(base).join(RENDER_MODE_FILE_NAME)
        );
        assert_eq!(
            shared_app_data_dir().file_name().unwrap(),
            APP_DATA_FOLDER_NAME
        );
    }
}
