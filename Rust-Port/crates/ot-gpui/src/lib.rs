//! GPUI-based UI foundation for OpenTaskbar.
//!
//! Phase 0 starts with the Windows HWND/DWM bridge (`win32`) and a runnable
//! dock + diagnostics spike (`examples/dock_spike.rs`) to validate borderless
//! transparency, topmost no-activate behavior, and memory/binary footprint.

#[cfg(windows)]
pub mod win32;
