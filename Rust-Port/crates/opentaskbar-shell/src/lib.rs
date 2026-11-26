//! Windows taskbar replacement safety layer: recovery journal, out-of-process
//! watchdog, non-blocking Explorer hide/restore, and running-window tracker.
#![cfg(windows)]

pub mod flyout_journal;
pub mod journal;
pub mod taskbar;
pub mod tracker;
pub mod watchdog;
