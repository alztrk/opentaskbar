// Cross-workstream gates are evaluated by the orchestrator when the final
// shell/platform APIs land; do not warn about them in the meantime.
#![allow(unexpected_cfgs)]

//! Fjord: a lightweight file manager, a per-monitor top bar and a Wi-Fi flyout.
//!
//! Rust implementation of `FjordFilesWindow`, `FjordBarWindow`, `FjordFilesActivation`,
//! `FjordFilesAvailability`, `FjordFilePreview` and `WifiNetworks` services.
//!
//! Wiring notes (cross-workstream contracts):
//! - `wifi_ui` calls `crate::platform::wifi::{scan_networks, connect, disconnect}`
//!   (WS-4). That module is not present in this worktree yet; no local shim is
//!   added here. `main.rs`/`ui/mod.rs` registration belongs to the orchestrator.
//! - `bar` calls `opentaskbar_shell::appbar::{set_and_verify_state,
//!   set_position_pinned}` (WS-3) for the reserved top-edge rectangle.
//! - Both UI surfaces use `crate::ui::components` (`UiTheme`, `button`,
//!   `icon_glyph`, `fold_search_text`) by their contracted names.
//! - Locale keys `FjordFiles`, `FjordFilesOpen`, `FjordBarShow`, `FjordBarHide`,
//!   `WifiScan`, `WifiConnect`, `WifiDisconnect`, `WifiPassword`,
//!   `WifiNoNetworks`, `WifiConnectFailed`, `PreviewDisabledHint`,
//!   `SearchNoResults` are supplied by WS-2; `loc::t` falls back to the key
//!   itself until they land, so a missing catalog never breaks a render.

pub mod activation;
pub mod bar;
pub mod files;
pub mod preview;
pub mod wifi_ui;

/// Settings-change / monitor-reconciliation cadence from `FjordBarHost` and
/// `FjordFilesAvailability` (750 ms in the reference original).
pub const POLL_MS: u64 = 750;

/// Named-pipe prefix. The session id is appended by `activation`.
pub const PIPE_PREFIX: &str = r"\\.\pipe\OpenTaskbar.FjordFiles.";

/// Session-scoped AppBar mutex prefix (`Local\` namespace).
pub const BAR_MUTEX_PREFIX: &str = r"Local\OpenTaskbar.FjordBar.";

/// Shortcut command handled by `activation::handle_cli`.
pub const FILES_CLI_ARG: &str = "--files";
