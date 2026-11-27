//! Global rendering preference kept outside the profile-aware settings file so
//! startup can read it before any GPUI window exists. Software rendering saves
//! roughly 100 MB of private memory at the cost of less fluid magnification and
//! flyouts; the choice takes effect at the next start.

use std::sync::atomic::{AtomicI8, Ordering};

use opentaskbar_core::log;

pub const SOFTWARE_RENDER_ARG: &str = "--software-render";
const ENV_FLAG: &str = "OT_SOFTWARE_RENDER";

/// Runtime override, used by `--software-render` before the preference file is
/// read. -1 = from file, 0 = auto, 1 = software.
static OVERRIDE: AtomicI8 = AtomicI8::new(-1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderPreference {
    Auto,
    Software,
}

impl RenderPreference {
    pub fn is_software(self) -> bool {
        self == Self::Software
    }
}

fn preference_path() -> std::path::PathBuf {
    opentaskbar_core::paths::render_mode_path()
}

/// True when `--software-render` was passed on the command line.
pub fn software_arg_present(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg.eq_ignore_ascii_case(SOFTWARE_RENDER_ARG))
}

/// Honours `--software-render` for the current process (call before `apply()`).
pub fn apply_cli_args(args: &[String]) {
    if software_arg_present(args) {
        set_software(true);
    }
}

pub fn preference() -> RenderPreference {
    match OVERRIDE.load(Ordering::Relaxed) {
        1 => RenderPreference::Software,
        0 => RenderPreference::Auto,
        _ => {
            let software = std::fs::read_to_string(preference_path())
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|json| json.get("software")?.as_bool())
                .unwrap_or(false);
            if software {
                RenderPreference::Software
            } else {
                RenderPreference::Auto
            }
        }
    }
}

pub fn software_requested() -> bool {
    preference().is_software()
}

/// Persists the preference to `render-mode.json` (reference parity).
pub fn set_software(value: bool) {
    OVERRIDE.store(if value { 1 } else { 0 }, Ordering::Relaxed);
    let path = preference_path();
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    if let Err(error) = std::fs::write(&path, format!("{{\"software\":{value}}}")) {
        log::warn(format!(
            "Render mode preference could not be saved: {error}"
        ));
    }
}

/// Applies the resolved preference for this process. GPUI and the dock can read
/// `software_requested()` to pick a software adapter; the environment flag is a
/// convenience for backend selection performed by the lead's entry point.
pub fn apply() {
    let software = software_requested();
    // SAFETY: called once at startup before extra threads are spawned.
    unsafe {
        std::env::set_var(ENV_FLAG, if software { "1" } else { "0" });
    }
    log::info(format!(
        "Render mode: {}",
        if software { "software" } else { "auto" }
    ));
}

pub fn clear_override() {
    OVERRIDE.store(-1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_file_is_in_the_shared_folder() {
        assert_eq!(
            preference_path(),
            opentaskbar_core::paths::render_mode_path()
        );
        assert_eq!(preference_path().file_name().unwrap(), "render-mode.json");
        assert_eq!(
            preference_path().parent(),
            Some(opentaskbar_core::paths::shared_app_data_dir().as_path())
        );
    }

    #[test]
    fn software_render_arg_is_recognized_case_insensitively() {
        let args = vec!["--settings".to_owned(), "--SOFTWARE-RENDER".to_owned()];
        assert!(software_arg_present(&args));
        assert!(!software_arg_present(&["--restore".to_owned()]));
        assert!(RenderPreference::Software.is_software());
        assert!(!RenderPreference::Auto.is_software());
    }
}
