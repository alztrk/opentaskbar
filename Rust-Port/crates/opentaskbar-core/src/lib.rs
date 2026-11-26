//! Core settings, logging, localization, and single-instance coordination for OpenTaskbar.

pub mod dynamic_island;
pub mod hotkey;
pub mod i18n;
pub mod log;
pub mod settings;
#[cfg(windows)]
pub mod single_instance;

pub use settings::{
    AppTheme, AutoHideMode, ClockDateFormat, ClockDateLayout, ClockDateStyle, DockEdge,
    DockPosition, DockSettings, DockSize, DynamicIslandVisibilityMode, SurfaceBackground,
    WidgetDensityMode,
};
