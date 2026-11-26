//! Windows taskbar replacement safety layer: recovery journal, out-of-process
//! watchdog with heartbeat, non-blocking Explorer hide/restore, flyout position
//! journal, AppBar choke point and running-window tracker.
#![cfg(windows)]

pub mod appbar;
pub mod flyout_journal;
pub mod flyout_reposition;
pub mod flyout_writer;
pub mod journal;
pub mod modifiers;
pub mod native_tray;
pub mod session;
pub mod taskbar;
pub mod tracker;
pub mod watchdog;
pub mod window_rules;
