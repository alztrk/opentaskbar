//! OpenTaskBar (Rust Hybrid: DirectComposition Dock + GPUI Settings) entry point.
//!
//! Modes:
//! - `--extract-icons`: short-lived icon extractor worker (zero log/mutex/UI).
//! - `--settings`: on-demand GPUI Settings window (frees 100% RAM/VRAM on close).
//! - `--restore`: restores Explorer's taskbar from the recovery journal and exits.
//! - default: acquires single-instance lock, loads settings, and runs the native dock.
//!
//! All data locations go through the `opentaskbar_core::paths` contract; the
//! settings/cfg workstream's shared `%LOCALAPPDATA%\OpenTaskbar` layout is now
//! compiled into this crate.
#![cfg_attr(windows, windows_subsystem = "windows")]
// The merged reference-parity modules expose a broad public API (hardware
// vendors, theme tokens, flyout blocks, shell helpers) that is not wired to
// every call site yet; keep the shared surface warning-clean.
#![allow(dead_code)]

// Bundled resource paths, anchored to the crate manifest instead of a chain of
// `../..`. The TypeScript-style `@/` equivalent for this repository; paths are
// relative to `resources/` at the repository root:
//   crate::resource!("themes/nord") -> &'static str            (runtime paths)
//   include_bytes!(concat!(crate::resource!("fonts/Inter-Bold.ttf")))
// `include_bytes!` cannot expand a wrapping `macro_rules!` directly, but it
// does accept a `concat!` whose argument is this macro.
#[cfg(windows)]
#[macro_export]
macro_rules! resource {
    ($path:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/", $path)
    };
}

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
        // Shared (uppercase) `Themes` root — same folder as the reference build.
        let dir = opentaskbar_core::paths::theme_root().join(&settings.theme_pack);
        platform::icon_pack::set_active_pack(Some(dir.as_path()));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(windows)]
    {
        platform::render_mode::apply_cli_args(&args);
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
        if args.iter().any(|a| a == ui::settings::SETTINGS_ARG) {
            opentaskbar_core::log::init(opentaskbar_core::paths::rust_log_path());
            std::panic::set_hook(Box::new(|info| {
                opentaskbar_core::log::error(format!("SETTINGS PANIC: {info}"));
            }));
            let settings =
                opentaskbar_core::settings::load_from(&opentaskbar_core::paths::settings_path());
            opentaskbar_core::i18n::init(Some(&settings.language));
            design::fonts::apply_settings(&settings);
            apply_startup_context(&settings);
            ui::settings::run_settings_window();
            return;
        }
    }

    opentaskbar_core::log::init(opentaskbar_core::paths::rust_log_path());

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
            opentaskbar_core::settings::load_from(&opentaskbar_core::paths::settings_path());
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
        // Non-blocking startup update check with an Action Center toast (reference NotificationService).
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
        // `end_replacement` restores the native taskbar first and only then
        // publishes the verified clean-exit signal to the watchdog.
        opentaskbar_shell::taskbar::end_replacement();
        if panicked {
            opentaskbar_core::log::error(
                "Dock panicked on the UI thread; restoring taskbar from journal.",
            );
            opentaskbar_shell::taskbar::restore_from_journal();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn startup_paths_live_in_the_shared_localappdata_folder() {
        use opentaskbar_core::paths;

        let root = paths::shared_app_data_dir();
        assert_eq!(root.file_name().unwrap(), "OpenTaskbar");
        assert_eq!(paths::settings_path(), root.join("settings.json"));
        assert_eq!(
            paths::rust_log_path(),
            root.join("opentaskbar.log"),
            "Rust log must not collide with the reference log.txt"
        );
        assert_eq!(paths::theme_root(), root.join("Themes"));
        assert_eq!(
            paths::theme_root().file_name().unwrap(),
            Path::new("Themes").file_name().unwrap(),
            "uppercase T is required for reference theme-pack parity"
        );
    }
}
