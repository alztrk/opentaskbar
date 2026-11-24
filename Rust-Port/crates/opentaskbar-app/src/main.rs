//! OpenTaskBar (Rust + GPUI) entry point.
//!
//! Modes:
//! - `--extract-icons`: short-lived icon extractor worker (zero log/mutex/UI).
//! - `--watchdog=<pid>:<token>`: out-of-process taskbar recovery watchdog.
//! - `--restore`: restores Explorer's taskbar from the recovery journal and exits.
//! - default: acquires single-instance lock, loads settings, and launches the GPUI dock.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod dock;
#[cfg(windows)]
mod icon_cache;
#[cfg(windows)]
mod pins;
#[cfg(windows)]
mod win32;

#[cfg(windows)]
fn enable_dpi_awareness() {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // SAFETY: called once at startup before any window is created.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(windows)]
    {
        if args.iter().any(|a| a == icon_cache::WORKER_ARG) {
            icon_cache::run_worker();
            return;
        }
        if opentaskbar_shell::watchdog::is_invocation(&args) {
            opentaskbar_shell::watchdog::run(&args);
            return;
        }
        if args.iter().any(|a| a == "--restore") {
            opentaskbar_shell::taskbar::restore_from_journal();
            return;
        }
        enable_dpi_awareness();
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
        opentaskbar_core::log::info("Starting OpenTaskBar GPUI dock.");
        dock::run(settings);
        opentaskbar_shell::taskbar::end_replacement();
    }
}
