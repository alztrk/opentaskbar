//! Core settings, logging, localization, and single-instance coordination for OpenTaskbar.

pub mod dynamic_island;
pub mod hotkey;
pub mod i18n;
pub mod log;
pub mod paths;
pub mod settings;
#[cfg(windows)]
pub mod single_instance;

pub use settings::{
    AppTheme, AutoHideMode, ClockDateFormat, ClockDateLayout, ClockDateStyle, DockEdge,
    DockPosition, DockSettings, DockSize, DynamicIslandVisibilityMode, SurfaceBackground,
    WidgetDensityMode,
};

/// Null-terminated UTF-16 buffer for Win32 / WinHTTP APIs.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Days since 1970-01-01 (Howard Hinnant's civil-to-days algorithm).
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: days since 1970-01-01 -> `(year, month, day)`.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(month <= 2), month, day)
}
