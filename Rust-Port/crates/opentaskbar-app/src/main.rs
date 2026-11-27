//! OpenTaskBar (Rust Hybrid: DirectComposition Dock + GPUI Settings) entry point.
//!
//! Modes:
//! - `--extract-icons`: short-lived icon extractor worker (zero log/mutex/UI).
//! - `--settings`: on-demand GPUI Settings window (frees 100% RAM/VRAM on close).
//! - `--fjord-bar`: on-demand GPUI Fjord top bar host (one per Windows session).
//! - `--files <path>`: routes a folder request to the dock's Fjord pipe (Explorer fallback).
//! - `--restore`: restores Explorer's taskbar from the recovery journal and exits.
//! - default: acquires single-instance lock, loads settings, and runs the native dock.
#![cfg_attr(windows, windows_subsystem = "windows")]
// The merged C#-parity modules expose a deliberately broad public API (hardware
// vendors, theme tokens, flyout blocks, shell helpers). Individual call sites are
// wired incrementally; keep the surface warning-clean without deleting code.
#![allow(dead_code)]

#[cfg(windows)]
mod design;
#[cfg(windows)]
mod dock;
#[cfg(windows)]
mod fjord;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod widgets;
#[cfg(windows)]
mod window_picker;

#[cfg(windows)]
fn enable_dpi_awareness() {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Cheap, non-blocking shell-change callback for the WinEvent watcher.
#[cfg(windows)]
fn on_shell_change() {
    opentaskbar_core::log::event("ShellChanged", &[]);
}

/// Wires the native Search and Control Center flyouts to the same per-monitor
/// dock anchors as Start. The dock posts `WM_APP_SHELL_FLYOUT` from every open
/// path; this handler resolves the anchor on the UI thread and hands the actual
/// move to `reposition_flyout`, which runs it on a background worker.
///
/// Start is deliberately excluded: its verified-open hook stays in
/// `platform::start_menu_state::on_opened`, and each `FlyoutKind` owns its own
/// generation, so scheduling Search/CC never bumps (or is bumped by) Start.
/// `hwnd == 0` means the flyout HWND is not known yet; the worker discovers it.
#[cfg(windows)]
pub(crate) fn on_shell_flyout_opened(
    kind: opentaskbar_shell::flyout_reposition::FlyoutKind,
    hwnd: usize,
) {
    use opentaskbar_shell::flyout_reposition::{Anchor, FlyoutKind, reposition_flyout};

    if kind == FlyoutKind::Start {
        return;
    }
    let (left, top, right, bottom) = dock::shell_button_anchor_for(hwnd, kind);
    let anchor = Anchor {
        left,
        top,
        right,
        bottom,
    };
    let _ = reposition_flyout(kind, anchor, hwnd);
}

/// Single switch for native Start-flyout repositioning. PascalCase is the
/// legacy settings key; the snake_case key is canonical and defaults to on.
#[cfg(windows)]
pub(crate) fn native_flyout_reposition_enabled(
    settings: &opentaskbar_core::settings::DockSettings,
) -> bool {
    settings
        .extra
        .get("native_flyout_reposition")
        .or_else(|| settings.extra.get("NativeFlyoutReposition"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

/// Applies the same value to both shell writers so a disabled setting cannot
/// leave the window-writer active while the reposition thread is stopped.
#[cfg(windows)]
pub(crate) fn apply_native_flyout_reposition(settings: &opentaskbar_core::settings::DockSettings) {
    let enabled = native_flyout_reposition_enabled(settings);
    opentaskbar_shell::flyout_writer::set_enabled(Some(enabled));
    opentaskbar_shell::flyout_reposition::set_enabled(Some(enabled));
}

/// Shared startup wiring for the Settings process and the dock process.
#[cfg(windows)]
fn apply_startup_context(settings: &opentaskbar_core::settings::DockSettings) {
    let _ = platform::theme_packs::install_bundled();
    design::theme::ThemeManager::apply(settings.theme, Some(settings));
    // Theme-pack token overrides saved by the in-app editor must reach the
    // live palette before any window is created.
    design::theme::apply_saved_token_overrides(settings);
    let _ = design::windows_palette::install_listener();
    platform::icon_pack::set_active_pack(None);
    if !settings.theme_pack.trim().is_empty() {
        let dir = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("OpenTaskbar")
            .join("Themes")
            .join(&settings.theme_pack);
        platform::icon_pack::set_active_pack(Some(dir.as_path()));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(windows)]
    {
        platform::render_mode::apply_cli_args(&args);
        if let Some(handled) = fjord::activation::handle_cli_arg(&args)
            && handled
        {
            return;
        }
        if let Some(arg) = args
            .iter()
            .find(|a| a.starts_with("--update") || a.starts_with("action=update"))
            && platform::toast::handle_activation_argument(arg)
        {
            return;
        }
        if args.iter().any(|a| a == platform::icon_cache::WORKER_ARG) {
            platform::icon_cache::run_worker();
            return;
        }
        if args.iter().any(|a| a == "--restore") {
            opentaskbar_shell::taskbar::restore_from_journal();
            return;
        }
        enable_dpi_awareness();
        if args.iter().any(|a| a == ui::fjord_bar::BAR_ARG) {
            opentaskbar_core::log::init(
                opentaskbar_core::settings::app_data_dir().join("opentaskbar.log"),
            );
            std::panic::set_hook(Box::new(|info| {
                opentaskbar_core::log::error(format!("FJORD BAR PANIC: {info}"));
            }));
            let settings =
                opentaskbar_core::settings::load_from(&opentaskbar_core::settings::settings_path());
            opentaskbar_core::i18n::init(Some(&settings.language));
            design::fonts::apply_settings(&settings);
            apply_startup_context(&settings);
            if fjord::activation::bar_visible(&settings) {
                ui::fjord_bar::run_bar_host(
                    fjord::activation::files_enabled(&settings),
                    opentaskbar_core::settings::settings_path(),
                );
            }
            return;
        }
        if args.iter().any(|a| a == ui::settings::SETTINGS_ARG) {
            opentaskbar_core::log::init(
                opentaskbar_core::settings::app_data_dir().join("opentaskbar.log"),
            );
            std::panic::set_hook(Box::new(|info| {
                opentaskbar_core::log::error(format!("SETTINGS PANIC: {info}"));
            }));
            let settings =
                opentaskbar_core::settings::load_from(&opentaskbar_core::settings::settings_path());
            opentaskbar_core::i18n::init(Some(&settings.language));
            design::fonts::apply_settings(&settings);
            apply_startup_context(&settings);
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
        apply_startup_context(&settings);
        platform::startup_registration::sync_on_startup(false, false);
        opentaskbar_shell::watchdog::arm();
        apply_native_flyout_reposition(&settings);
        // UNCLOAKED Start flyout: pin it above the dock Start button. If the
        // reposition thread cannot anchor it, `evaluate_auto_hide` hides the dock.
        platform::start_menu_state::on_opened(|hwnd| {
            // Invalidate the previous anchor first, even when no dock anchor is
            // available yet, so cached anchored state never hides a fresh menu.
            opentaskbar_shell::flyout_reposition::mark_opened();
            // Multi-monitor: the dock on the Start window's monitor owns the
            // anchor; a single global rect would target the wrong screen.
            let (left, top, right, bottom) = dock::start_button_anchor_for(hwnd);
            if left == right || top == bottom {
                return;
            }
            let anchor = opentaskbar_shell::flyout_reposition::Anchor {
                left,
                top,
                right,
                bottom,
            };
            let _ = opentaskbar_shell::flyout_reposition::reposition_start(anchor, hwnd);
        });
        platform::start_menu_state::install_hook();
        platform::taskbar_pin_monitor::start_watch();
        platform::radio::install_state_monitor();
        opentaskbar_shell::session::install_shell_hooks(on_shell_change);
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
        // Fjord files activation: the dock is the single per-session pipe server.
        // Keep the guard alive until the message loop exits; requests are gated
        // by the current on-disk flag so disabling Fjord stops new windows.
        let pipe_settings_path = opentaskbar_core::settings::settings_path();
        let _fjord_pipe = fjord::activation::PipeServer::start(move |path| {
            let snapshot = opentaskbar_core::settings::load_from(&pipe_settings_path);
            if fjord::activation::files_enabled(&snapshot) {
                fjord::activation::spawn_files_window(path);
            }
        });
        opentaskbar_core::log::info("Starting OpenTaskBar hybrid dock.");
        let panicked = std::panic::catch_unwind(|| dock::run(settings)).is_err();
        opentaskbar_shell::watchdog::notify_clean_exit();
        opentaskbar_shell::taskbar::end_replacement();
        if panicked {
            opentaskbar_core::log::error(
                "Dock panicked on the UI thread; restoring taskbar from journal.",
            );
            opentaskbar_shell::taskbar::restore_from_journal();
        }
    }
}
