//! Windows platform interop, app pin discovery, background icon extraction,
//! update checks, notifications and vendor integrations.
//!
//! Shared helpers live in [`com`], [`http`], [`json_ext`] and
//! [`win32_helpers`]; modules that own a specific API keep their own plumbing.

pub mod app_identity;
pub mod com;
pub mod core_audio;
pub mod country_city;
pub mod cursor_auth;
pub mod gpu_preference;
pub mod http;
pub mod hud_monitor;
pub mod icon_cache;
pub mod icon_pack;
pub mod json_ext;
pub mod jumplist;
pub mod notification_listener;
pub mod pins;
pub mod privacy_dots;
pub mod radio;
pub mod render_mode;
pub mod shell_files;
pub mod shell_shortcuts_ex;
pub mod smart_tools;
pub mod start_menu_state;
pub mod startup_registration;
pub mod taskbar_pin_monitor;
pub mod theme_packs;
pub mod toast;
pub mod tray_host;
pub mod tray_icon_host;
pub mod update;
pub mod win32;
pub mod win32_ex;
pub mod win32_helpers;
pub mod wireless_battery;
pub mod wireless_vendors_ext;
