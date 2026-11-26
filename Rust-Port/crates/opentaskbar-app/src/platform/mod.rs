//! Windows platform interop, app pin discovery, and background icon extraction.
//!
//! The `allow(dead_code)` attributes are deliberate: these modules provide the
//! C4 platform API to other workstreams (WS-C/D/E/G) whose call sites land in
//! the lead's `main.rs` / widget modules at merge time.

#[allow(dead_code)]
pub mod app_identity;
#[allow(dead_code)]
pub mod gpu_preference;
pub mod icon_cache;
pub mod icon_pack;
pub mod jumplist;
pub mod pins;
#[allow(dead_code)]
pub mod radio;
#[allow(dead_code)]
pub mod render_mode;
#[allow(dead_code)]
pub mod shell_shortcuts_ex;
pub mod shell_files;
#[allow(dead_code)]
pub mod start_menu_state;
#[allow(dead_code)]
pub mod startup_registration;
#[allow(dead_code)]
pub mod taskbar_pin_monitor;
pub mod theme_packs;
pub mod toast;
pub mod tray_host;
pub mod tray_icon_host;
pub mod update;
pub mod win32;
#[allow(dead_code)]
pub mod win32_ex;
pub mod wireless_battery;

// WS-G provider modules (kept with the same dead-code allowance).
#[allow(dead_code)]
pub mod core_audio;
#[allow(dead_code)]
pub mod country_city;
#[allow(dead_code)]
pub mod cursor_auth;
#[allow(dead_code)]
pub mod app_shortcut_resolver;
