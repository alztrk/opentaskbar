//! Core settings, logging, and single-instance coordination for OpenTaskbar.

pub mod hotkey;
pub mod log;
pub mod settings;
#[cfg(windows)]
pub mod single_instance;

pub use settings::{
    AppTheme, AutoHideMode, DockEdge, DockPosition, DockSettings, DockSize, SurfaceBackground,
};
