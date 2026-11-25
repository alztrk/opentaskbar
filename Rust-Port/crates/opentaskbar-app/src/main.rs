//! OpenTaskBar (Rust Hybrid: DirectComposition Dock + GPUI Settings) entry point.
//!
//! Modes:
//! - `--extract-icons`: short-lived icon extractor worker (zero log/mutex/UI).
//! - `--settings`: on-demand GPUI Settings window (frees 100% RAM/VRAM on close).
//! - `--restore`: restores Explorer's taskbar from the recovery journal and exits.
//! - default: acquires single-instance lock, loads settings, and runs the native dock.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod design;
#[cfg(windows)]
mod dock;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod widgets;

#[cfg(windows)]
fn enable_dpi_awareness() {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(windows)]
    {
        if args.iter().any(|a| a == platform::icon_cache::WORKER_ARG) {
            platform::icon_cache::run_worker();
            return;
        }
        if args.iter().any(|a| a == "--restore") {
            opentaskbar_shell::taskbar::restore_from_journal();
            return;
        }
        enable_dpi_awareness();
        if args.iter().any(|a| a == ui::settings::SETTINGS_ARG) {
            ui::settings::run_settings_window();
            return;
        }
    }

    opentaskbar_core::log::init(opentaskbar_core::settings::app_data_dir().join("opentaskbar.log"));

    std::panic::set_hook(Box::new(|info| {
        opentaskbar_core::log::error(format!("PANIC: {info}"));
        #[cfg(windows)]
        opentaskbar_shell::taskbar::restore_from_journal();
    }));

    #[cfg(windows)]
    {
        let Some(_lock) = opentaskbar_core::single_instance::SingleInstance::acquire(
            opentaskbar_core::single_instance::MUTEX_NAME,
        ) else {
            opentaskbar_core::log::info("Another OpenTaskBar instance is already running.");
            return;
        };

        if opentaskbar_shell::journal::has_pending() {
            opentaskbar_shell::taskbar::restore_from_journal();
        }

        let settings =
            opentaskbar_core::settings::load_from(&opentaskbar_core::settings::settings_path());
        opentaskbar_core::log::info("Starting OpenTaskBar hybrid dock.");
        dock::run(settings);
        opentaskbar_shell::taskbar::end_replacement();
    }
}
