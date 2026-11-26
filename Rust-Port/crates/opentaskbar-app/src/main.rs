//! OpenTaskBar (Rust Hybrid: DirectComposition Dock + GPUI Settings) entry point.
//!
//! Modes:
//! - `--extract-icons`: short-lived icon extractor worker (zero log/mutex/UI).
//! - `--settings`: on-demand GPUI Settings window (frees 100% RAM/VRAM on close).
//! - `--restore`: restores Explorer's taskbar from the recovery journal and exits.
//! - default: acquires single-instance lock, loads settings, and runs the native dock.
#![cfg_attr(windows, windows_subsystem = "windows")]
// WS-E/F/G/H modules merged ahead of their WS-D call sites and WS-H tests/docs. Keep the
// intermediate integration tree warning-clean without deleting the staged public API.
#![allow(dead_code)]

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
            let settings =
                opentaskbar_core::settings::load_from(&opentaskbar_core::settings::settings_path());
            opentaskbar_core::i18n::init(Some(&settings.language));
            design::fonts::apply_settings(&settings);
            let theme_dir = if settings.theme_pack.trim().is_empty() {
                None
            } else {
                Some(
                    std::env::var_os("LOCALAPPDATA")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(std::env::temp_dir)
                        .join("OpenTaskbar")
                        .join("Themes")
                        .join(&settings.theme_pack),
                )
            };
            platform::icon_pack::set_active_pack(theme_dir.as_deref());
            ui::settings::run_settings_window();
            return;
        }
    }

    opentaskbar_core::log::init(opentaskbar_core::settings::app_data_dir().join("opentaskbar.log"));

    // Never restore Explorer from the hook: it can run on worker threads (no COM apartment) while the
    // dock is still alive, which unregisters a live AppBar and clears the recovery journal. The main
    // thread handles restoration after `dock::run` unwinds; other threads are left to the watchdog.
    std::panic::set_hook(Box::new(|info| {
        opentaskbar_core::log::error(format!("PANIC: {info}"));
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
        opentaskbar_core::i18n::init(Some(&settings.language));
        design::fonts::apply_settings(&settings);
        let theme_dir = if settings.theme_pack.trim().is_empty() {
            None
        } else {
            Some(
                std::env::var_os("LOCALAPPDATA")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir)
                    .join("OpenTaskbar")
                    .join("Themes")
                    .join(&settings.theme_pack),
            )
        };
        platform::icon_pack::set_active_pack(theme_dir.as_deref());
        // Non-blocking startup update check with an Action Center toast (C# NotificationService).
        if settings.check_updates {
            std::thread::spawn(|| {
                if let Ok(Some(release)) = platform::update::find_newer() {
                    let title = opentaskbar_core::i18n::t("UpdateAvailableTitle");
                    let body =
                        opentaskbar_core::i18n::t_fmt("UpdateToastBody", &[&release.version]);
                    let action =
                        opentaskbar_core::i18n::t_fmt("UpdateDownload", &[&release.version]);
                    platform::toast::show_update_toast(&title, &body, &action, &release.url);
                }
            });
        }
        opentaskbar_core::log::info("Starting OpenTaskBar hybrid dock.");
        let panicked = std::panic::catch_unwind(|| dock::run(settings)).is_err();
        opentaskbar_shell::taskbar::end_replacement();
        if panicked {
            opentaskbar_core::log::error(
                "Dock panicked on the UI thread; restoring taskbar from journal.",
            );
            opentaskbar_shell::taskbar::restore_from_journal();
        }
    }
}
