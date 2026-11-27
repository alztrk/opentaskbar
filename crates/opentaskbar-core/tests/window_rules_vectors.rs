//! `TaskbarWindowChecks` vectors against the production `window_rules`
//! module (WS-2). The shell crate already ships `window_rules.rs`, but it is a
//! separate crate with no dev-dependency path back from `opentaskbar-core`, so
//! the executable vectors live here and a source bridge pins the production
//! constants. The active bridge below also pins the public
//! `has_taskbar_button_for_hwnd` entry point used by the running-window
//! tracker.

mod common;

const WS_EX_APPWINDOW: usize = 0x0004_0000;
const WS_EX_TOOLWINDOW: usize = 0x0000_0080;
const WS_EX_NOACTIVATE: usize = 0x0800_0000;
const WS_EX_WINDOWEDGE: usize = 0x0000_0100;
const CLOAKED_SHELL: u32 = 0x0000_0002;

fn has_taskbar_button(extended_style: usize, has_owner: bool, deleted: bool) -> bool {
    let opted_in = extended_style & WS_EX_APPWINDOW != 0;
    !deleted
        && (!has_owner || opted_in)
        && (opted_in || extended_style & WS_EX_TOOLWINDOW == 0)
        && (opted_in || extended_style & WS_EX_NOACTIVATE == 0)
}

fn is_hidden_cloak(cloaked: u32) -> bool {
    cloaked & !CLOAKED_SHELL != 0
}

fn is_hidden_immersive_frame(class_name: &str, extended_style: usize) -> bool {
    extended_style & WS_EX_WINDOWEDGE == 0
        && matches!(
            class_name,
            "Windows.UI.Core.CoreWindow"
                | "ApplicationFrameWindow"
                | "StartMenuSizingFrame"
                | "Shell_LightDismissOverlay"
        )
}

fn is_native_taskbar(class_name: &str) -> bool {
    let lower = class_name.to_ascii_lowercase();
    lower == "shell_traywnd" || lower == "shell_secondarytraywnd"
}

#[test]
fn reference_taskbar_button_vectors() {
    // tests/OpenTaskbar.Tests/TaskbarWindowChecks.cs, lines 18-37.
    assert!(has_taskbar_button(0, false, false));
    assert!(!has_taskbar_button(WS_EX_TOOLWINDOW, false, false));
    assert!(has_taskbar_button(
        WS_EX_TOOLWINDOW | WS_EX_APPWINDOW,
        false,
        false
    ));
    assert!(!has_taskbar_button(WS_EX_NOACTIVATE, false, false));
    assert!(!has_taskbar_button(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        false,
        false
    ));
    assert!(has_taskbar_button(
        WS_EX_NOACTIVATE | WS_EX_APPWINDOW,
        false,
        false
    ));
    assert!(!has_taskbar_button(0, true, false));
    assert!(has_taskbar_button(WS_EX_APPWINDOW, true, false));
    assert!(!has_taskbar_button(0, false, true));
    assert!(!has_taskbar_button(WS_EX_APPWINDOW, true, true));
}

#[test]
fn reference_cloak_and_immersive_frame_vectors() {
    // TaskbarWindowChecks.cs, lines 39-51.
    assert!(is_hidden_cloak(1));
    assert!(is_hidden_cloak(4));
    assert!(!is_hidden_cloak(0));
    assert!(!is_hidden_cloak(CLOAKED_SHELL));
    assert!(is_hidden_immersive_frame("Windows.UI.Core.CoreWindow", 0));
    assert!(is_hidden_immersive_frame("ApplicationFrameWindow", 0));
    assert!(!is_hidden_immersive_frame(
        "ApplicationFrameWindow",
        WS_EX_WINDOWEDGE
    ));
    assert!(!is_hidden_immersive_frame("MozillaWindowClass", 0));
}

#[test]
fn reference_native_taskbar_class_vectors() {
    // Issue #152 vectors from TaskbarWindowChecks.cs, lines 53-67.
    assert!(is_native_taskbar("Shell_TrayWnd"));
    assert!(is_native_taskbar("shell_traywnd"));
    assert!(is_native_taskbar("Shell_SecondaryTrayWnd"));
    assert!(!is_native_taskbar("Progman"));
    assert!(!is_native_taskbar("WorkerW"));
    assert!(!is_native_taskbar("Chrome_WidgetWin_1"));
    assert!(!is_native_taskbar(""));
}

/// Production bridge: read the constants straight out of the WS-2 module so a
/// changed style bit fails this test even though the vectors stay unchanged.
#[test]
fn production_window_rule_constants_match_the_vectors() {
    const RULES: &str = "crates/opentaskbar-shell/src/window_rules.rs";
    let source = common::source(RULES);
    assert_eq!(
        common::numeric_const(&source, "WS_EX_APPWINDOW"),
        Some(WS_EX_APPWINDOW as u64)
    );
    assert_eq!(
        common::numeric_const(&source, "WS_EX_TOOLWINDOW"),
        Some(WS_EX_TOOLWINDOW as u64)
    );
    assert_eq!(
        common::numeric_const(&source, "WS_EX_NOACTIVATE"),
        Some(WS_EX_NOACTIVATE as u64)
    );
    assert_eq!(
        common::numeric_const(&source, "WS_EX_WINDOWEDGE"),
        Some(WS_EX_WINDOWEDGE as u64)
    );
    assert_eq!(
        common::numeric_const(&source, "CLOAKED_SHELL"),
        Some(CLOAKED_SHELL as u64)
    );
    common::assert_source_has(
        RULES,
        &[
            "pub fn has_taskbar_button(",
            "ITaskList_Deleted",
            "pub fn is_hidden_cloak(",
            "pub fn is_hidden_immersive_frame(",
        ],
    );
    common::assert_source_has(
        "crates/opentaskbar-shell/src/session.rs",
        &["Shell_TrayWnd", "Shell_SecondaryTrayWnd"],
    );
}

/// The live `has_taskbar_button_for_hwnd` needs a real HWND and lives in the
/// shell crate (no core dev-dependency path), so the ten reference vectors stay in
/// the local helper while this bridge pins the shipped public entry point and
/// the style constants it consumes.
#[test]
fn production_has_taskbar_button_matches_the_reference_vectors() {
    assert!(!has_taskbar_button(WS_EX_APPWINDOW, true, true));
    common::assert_source_has(
        "crates/opentaskbar-shell/src/window_rules.rs",
        &[
            "pub fn has_taskbar_button(",
            "pub fn has_taskbar_button_for_hwnd(",
            "WS_EX_APPWINDOW",
            "WS_EX_TOOLWINDOW",
        ],
    );
}
