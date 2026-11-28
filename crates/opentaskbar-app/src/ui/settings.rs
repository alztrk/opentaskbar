//! On-demand GPUI Settings window (`OpenTaskBar.exe --settings`).
//!
//! Grouped settings with a responsive navigation pane and immediate dock updates.
//! Includes the full `.otbackup` archive UI, theme editor/gallery, hotkey capture,
//! dynamic search catalogue and widget panel host (`SettingsWindow` parity).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use std::ops::Range;
use std::time::Duration;

use gpui::{
    AnyElement, App, Application, Bounds, Context, EntityInputHandler, FocusHandle,
    InteractiveElement, KeyBinding, KeyDownEvent, MouseButton, ScrollAnchor, ScrollHandle,
    SharedString, TitlebarOptions, UTF16Selection, Window, WindowBackgroundAppearance,
    WindowBounds, WindowControlArea, WindowOptions, actions, div, img, prelude::*, px, rgba, size,
};
use opentaskbar_core::dynamic_island;
use opentaskbar_core::hotkey;
use opentaskbar_core::i18n::{self as loc, LANGUAGES};
use opentaskbar_core::paths;
use opentaskbar_core::settings::{
    self, AiUsageRing, AppTheme, AutoHideMode, ClockDateFormat, ClockDateLayout, ClockDateStyle,
    DockContentAlignment, DockGapUnit, DockHeightMode, DockPosition, DockSettings, DockSize,
    DockWidthMode, DynamicIslandLayout, DynamicIslandStyle, DynamicIslandVisibilityMode,
    FisheyeIntensity, MiddleClickAction, StartButtonPlacement, SurfaceBackground, TemperatureUnit,
    WidgetDensityMode,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::{Map, Value, json};
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, HWND_BROADCAST, IsWindowVisible, PostMessageW,
    RegisterWindowMessageW, SendNotifyMessageW, WM_CLOSE,
};

use crate::design::brush::{BrushSpec, GradientSpec, Hsv, MAX_GRADIENT_STOPS};
use crate::design::fonts;
use crate::design::spatial_glass::{
    SpatialGlassConfig, SpatialGlassHost, configure_host_backdrop_window,
};
use crate::design::theme::{
    SurfaceKind, ThemeManager, apply_token_overrides, sync_from_settings, theme_surface_tint,
    token_overrides_from_json,
};
use crate::design::tokens::{EDITOR_TOKENS, Rgba as TokenRgba, TokenKind};
use crate::design::widget_pack::WidgetPackManifest;
use crate::dock::dcomp::{SystemStatColumn, WidgetChipSpec, WidgetIcon};
use crate::platform::cursor_auth;
use crate::platform::theme_packs::{self, ThemePackManifest, ThemePreviewSwatches};
use crate::platform::{country_city, pins, startup_registration, toast, update, win32};
use crate::ui::components::{
    ButtonVariant, DropdownOption, DropdownSelectHandler, SearchableDropdownHandlers,
    SearchableDropdownState, UiTheme, badge, button, button_with_state, card_button,
    compact_card_button, dim_when, dropdown, icon_action_menu, icon_glyph, icon_glyph_owned,
    ios_switch, live_slider, searchable_dropdown, section_card, setting_row, setting_row_desc,
    setting_row_with_id, setting_row_with_id_desc, sidebar_nav_item, stepper,
};
use crate::ui::prompt;
use crate::ui::settings_search::{self, SettingsSearchEntry};
use crate::ui::text_input::{TextInput, apply_keystroke};
use crate::widgets;
use crate::widgets::telemetry::AiAuthSource;

pub const SETTINGS_ARG: &str = "--settings";
pub const RELOAD_MSG_NAME: &str = "OpenTaskBarReloadSettings";

/// The settings window is a fixed-size dialog: both bounds and minimum size use these values,
/// and `WindowOptions::is_resizable` is false, so no resize handle is rendered.
const WIN_W: f32 = 1040.0;
const WIN_H: f32 = 740.0;
const SIDEBAR_BREAKPOINT: f32 = 680.0;
const SIDEBAR_WIDTH: f32 = 196.0;
const WIDGET_COLUMNS_BREAKPOINT: f32 = 520.0;
/// Focus timer bounds exposed for the Widgets page steppers and their regression tests.
const FOCUS_MINUTES_MAX: i32 = 90;
const FOCUS_BREAK_MINUTES_MAX: i32 = 30;
const FOCUS_SESSIONS_MAX: i32 = 8;

actions!(settings_navigation, [TabNext, TabPrevious]);
actions!(settings_panels, [CloseWidgetPanel, ToggleFlyoutPreview]);

fn set_border_thickness(settings: &mut DockSettings, surface: usize, value: f64) {
    let value = value.clamp(0.0, 8.0);
    match surface {
        0 => settings.dock_border_thickness = value,
        1 => settings.menu_border_thickness = value,
        2 => settings.widget_border_thickness = value,
        _ => settings.settings_border_thickness = value,
    }
}

/// Parses `#RRGGBB` / `RRGGBB` / `#AARRGGBB` / `AARRGGBB` into the `0xRRGGBBAA` `u32`
/// used by GPUI `rgba()`.
fn parse_hex_color(value: &str) -> Option<u32> {
    let hex = value.trim().trim_start_matches('#');
    if !matches!(hex.len(), 6 | 8) {
        return None;
    }
    TokenRgba::from_hex(value).map(TokenRgba::to_gpui)
}

/// Settings runs in its own `--settings` process so GPUI memory is released when the window
/// closes. Keep the children so dock shutdown can close them too (`CloseForShutdown` parity).
static SETTINGS_CHILDREN: Mutex<Vec<std::process::Child>> = Mutex::new(Vec::new());

pub fn spawn_settings_window() {
    // Single-instance Settings: focus the live window instead of starting a second process.
    // (The `--settings` process also holds a named mutex, so a racing launch still dedups.)
    if window_chrome::focus_existing_settings_window() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        opentaskbar_core::log::error("Cannot open Settings: current_exe failed.");
        return;
    };
    match std::process::Command::new(exe).arg(SETTINGS_ARG).spawn() {
        Ok(child) => {
            if let Ok(mut children) = SETTINGS_CHILDREN.lock() {
                // Drop exited children: a reused PID must never receive our WM_CLOSE.
                children.retain_mut(|existing| matches!(existing.try_wait(), Ok(None)));
                children.push(child);
            }
        }
        Err(error) => opentaskbar_core::log::error(format!("Cannot open Settings: {error}")),
    }
}

/// Posts WM_CLOSE to the Settings windows spawned by this dock. Called after the dock message
/// loop ends, so quitting from the taskbar cannot leave a stray Settings window/process behind.
pub fn close_spawned_settings_windows() {
    let Ok(mut children) = SETTINGS_CHILDREN.lock() else {
        return;
    };
    // Keep the Child handles alive through the enumeration: holding them prevents PID reuse.
    children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
    let pids: Vec<u32> = children.iter().map(|child| child.id()).collect();
    if pids.is_empty() {
        return;
    }

    struct CloseContext {
        pids: Vec<u32>,
        closed: Vec<bool>,
    }
    unsafe extern "system" fn close_settings_window(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` points at a live `CloseContext` for the duration of `EnumWindows`.
        unsafe {
            let context = &mut *(lparam as *mut CloseContext);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if let Some(index) = context.pids.iter().position(|candidate| *candidate == pid)
                && IsWindowVisible(hwnd) != 0
                && PostMessageW(hwnd, WM_CLOSE, 0, 0) != 0
            {
                context.closed[index] = true;
            }
        }
        1
    }

    let closed = vec![false; pids.len()];
    let mut context = CloseContext { pids, closed };
    // SAFETY: the callback only touches `context`, which outlives the enumeration.
    unsafe {
        EnumWindows(
            Some(close_settings_window),
            &mut context as *mut CloseContext as LPARAM,
        );
    }
    // A Settings process with no window yet (opened just before Exit) would otherwise survive
    // and show a window after the dock is gone. No window means nothing to save gracefully.
    for (child, closed) in children.iter_mut().zip(context.closed) {
        if !closed {
            let _ = child.kill();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsPage {
    Dock,
    Widgets,
    DynamicIsland,
    Themes,
    Preferences,
    About,
}

impl SettingsPage {
    const TOP: [Self; 4] = [Self::Dock, Self::Widgets, Self::DynamicIsland, Self::Themes];
    const BOTTOM: [Self; 2] = [Self::Preferences, Self::About];

    pub(crate) fn title(self) -> String {
        match self {
            Self::Dock => loc::t("Dock"),
            Self::Widgets => loc::t("Widgets"),
            Self::DynamicIsland => loc::t("DynamicIslandTitle"),
            Self::Themes => loc::t("Themes"),
            Self::Preferences => loc::t("Preferences"),
            Self::About => loc::t("About"),
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Dock => "\u{E90E}",
            Self::Widgets => "\u{F246}",
            Self::DynamicIsland => "\u{E8A1}",
            Self::Themes => "\u{E790}",
            Self::Preferences => "\u{E713}",
            Self::About => "\u{E946}",
        }
    }

    fn description(self) -> String {
        match self {
            Self::Dock => loc::t("SettingsPageDockDescription"),
            Self::Widgets => loc::t("SettingsPageWidgetsDescription"),
            Self::DynamicIsland => loc::t("DynamicIslandEnableDescription"),
            Self::Themes => loc::t("SettingsPageThemesDescription"),
            Self::Preferences => loc::t("SettingsPagePreferencesDescription"),
            Self::About => loc::t("SettingsPageAboutDescription"),
        }
    }
}

/// Display label for a widget filter/category value stored in `widget_filter`.
pub(crate) fn category_label(category: &str) -> String {
    match category {
        "All" => loc::t("WidgetCategoryAll"),
        "Active" => loc::t("Active"),
        other => loc::t(widgets::category_i18n_key(other)),
    }
}

/// Segoe Fluent Icons: horizontal resize for width, vertical resize for height, and a
/// spacing bar for gaps, so the Dock layout rows no longer repeat one glyph.
const DOCK_WIDTH_GLYPH: &str = "\u{E745}";
const DOCK_HEIGHT_GLYPH: &str = "\u{E746}";
const DOCK_GAP_GLYPH: &str = "\u{E76F}";

/// Width value stepper only makes sense for explicit pixel/percent sizes.
fn dock_width_has_value(mode: DockWidthMode) -> bool {
    matches!(mode, DockWidthMode::Pixels | DockWidthMode::Percent)
}

/// Height value stepper only makes sense for explicit pixel/percent sizes.
fn dock_height_has_value(mode: DockHeightMode) -> bool {
    matches!(mode, DockHeightMode::Pixels | DockHeightMode::Percent)
}

/// Unit suffix shown on dock size/gap stepper readouts.
fn unit_suffix(percent: bool) -> &'static str {
    if percent { "%" } else { " px" }
}

/// `(min, max, step)` bounds for the dock width stepper, matching `DockSettings::validate`.
fn dock_width_stepper_bounds(mode: DockWidthMode) -> (i32, i32, i32) {
    match mode {
        DockWidthMode::Percent => (10, 100, 1),
        _ => (100, 5000, 10),
    }
}

/// `(min, max, step)` bounds for the dock height stepper, matching `DockSettings::validate`.
fn dock_height_stepper_bounds(mode: DockHeightMode) -> (i32, i32, i32) {
    match mode {
        DockHeightMode::Percent => (2, 25, 1),
        _ => (24, 400, 2),
    }
}

/// `(min, max, step)` bounds for the dock top gap stepper, matching `DockSettings::validate`.
fn dock_top_gap_stepper_bounds(unit: DockGapUnit) -> (i32, i32, i32) {
    match unit {
        DockGapUnit::Percent => (0, 100, 1),
        DockGapUnit::Pixels => (0, 200, 2),
    }
}

/// Glyph table for built-in widgets. Registry metadata does not carry a glyph
/// field, so the single UI-owned table is the authoritative mapping.
const WIDGET_GLYPHS: &[(&str, &str)] = &[
    ("clock", "\u{E823}"),
    ("weather", "\u{E753}"),
    ("dayprogress", "\u{E706}"),
    ("stopwatch", "\u{E916}"),
    ("countdown", "\u{EA8F}"),
    ("stats", "\u{E9D9}"),
    ("netspeed", "\u{E8AB}"),
    ("battery", "\u{E850}"),
    ("bluetoothbattery", "\u{E702}"),
    ("storage", "\u{EDA2}"),
    ("media", "\u{E8D6}"),
    ("audio", "\u{E995}"),
    ("quicksettings", "\u{E9E9}"),
    ("keyboard", "\u{E765}"),
    ("focus", "\u{E7C8}"),
];
const AI_USAGE_GLYPH: &str = "\u{E99A}";
const FALLBACK_WIDGET_GLYPH: &str = "\u{F246}";

fn widget_glyph(id: &str) -> &'static str {
    if id.starts_with("aiusage_") {
        return AI_USAGE_GLYPH;
    }
    WIDGET_GLYPHS
        .iter()
        .find(|(key, _)| *key == id)
        .map_or(FALLBACK_WIDGET_GLYPH, |(_, glyph)| *glyph)
}

/// Uses a locale key when the translation catalog provides one, otherwise the
/// current English fallback. Lets new keys be added to core locales without a
/// UI-side behaviour change in the meantime.
fn localized_action(key: &str, fallback: &str) -> String {
    let translated = loc::t(key);
    if translated == key {
        fallback.to_owned()
    } else {
        translated
    }
}

fn country_flag_path(country: &str) -> Option<SharedString> {
    let locale = match country {
        "Turkey" => "tr-TR",
        "Germany" => "de-DE",
        "United States" => "en-US",
        "France" => "fr-FR",
        "Netherlands" => "nl-NL",
        "Spain" => "es-ES",
        "Italy" => "it-IT",
        "Poland" => "pl-PL",
        "Japan" => "ja-JP",
        "South Korea" => "ko-KR",
        "China" => "zh-CN",
        "Brazil" => "pt-BR",
        "Russia" => "ru-RU",
        "Ukraine" => "uk-UA",
        _ => return None,
    };
    Some(SharedString::from(fonts::flag_path(locale)))
}

/// Every widget card in the Settings UI exposes an inline Settings drawer
/// (widget-specific options, AI auth, density override, Dynamic Island placement, flyout preview).
fn widget_has_card_drawer(_id: &str) -> bool {
    true
}

/// Configures the GPUI Settings HWND (`WS_EX_NOREDIRECTIONBITMAP`) for `HostBackdropBrush`
/// strictly in Glass mode so `SpatialGlassHost` renders live Clear Crystal glass underneath.
fn configure_settings_glass_hwnd(window: &Window, is_dark: bool, glass_mode: bool) -> usize {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return 0;
    };
    let RawWindowHandle::Win32(win32_handle) = handle.as_raw() else {
        return 0;
    };
    let hwnd = win32_handle.hwnd.get() as usize;
    configure_host_backdrop_window(hwnd, is_dark, glass_mode);
    hwnd
}

/// Outer corner radius (DIP) of the Settings surface: Windows 11 rounds top-level windows
/// (`DWMWCP_ROUND` = 8 px); Windows 10 has square window corners, so GPUI must not round either.
fn settings_corner_radius(is_windows_11: bool) -> f32 {
    if is_windows_11 { 8.0 } else { 0.0 }
}

/// Exactly one crisp outer border: the Clear Crystal glass host already paints a 1 px optical
/// rim when Glass is active and its edge glow is not suppressed (OLED glow reduction / 0 glow),
/// so GPUI only draws its own border when that rim is absent.
fn settings_root_draws_border(glass_rim_active: bool) -> bool {
    !glass_rim_active
}

/// Clamps the fixed 1040×740 dialog so it always fits inside the monitor work area (e.g.
/// 1080p at 150% leaves only ~1280×672 DIP of work area). `work` is in DIP.
fn fit_settings_size(work: Option<gpui::Size<gpui::Pixels>>) -> gpui::Size<gpui::Pixels> {
    const MARGIN: f32 = 16.0;
    const FLOOR_W: f32 = 360.0;
    const FLOOR_H: f32 = 320.0;
    let Some(work) = work else {
        return size(px(WIN_W), px(WIN_H));
    };
    let work_w = f32::from(work.width);
    let work_h = f32::from(work.height);
    if !(work_w.is_finite() && work_h.is_finite()) || work_w <= 0.0 || work_h <= 0.0 {
        return size(px(WIN_W), px(WIN_H));
    }
    let w = WIN_W.min((work_w - MARGIN * 2.0).max(FLOOR_W.min(work_w)));
    let h = WIN_H.min((work_h - MARGIN * 2.0).max(FLOOR_H.min(work_h)));
    size(px(w.floor()), px(h.floor()))
}

/// Win32 chrome for the GPUI Settings window: Windows 11 rounded corners without the DWM
/// accent border, removal of GPUI's phantom non-client skirt, the real OpenTaskbar icon,
/// screen-space anchor lookup and the cross-process single-instance Settings lookup.
pub(crate) mod window_chrome {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{
        FARPROC, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
    };
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
        DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Controls::MARGINS;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::Shell::{
        DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GWL_STYLE, GetPropW, GetSystemMetrics, GetWindowLongPtrW,
        GetWindowRect, GetWindowThreadProcessId, HTBORDER, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT,
        HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, ICON_BIG, ICON_SMALL, IMAGE_ICON,
        IsIconic, IsWindowVisible, IsZoomed, LR_DEFAULTCOLOR, LoadImageW, PostMessageW,
        RemovePropW, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SW_RESTORE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetForegroundWindow,
        SetPropW, SetWindowPos, ShowWindow, WM_APP, WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST,
        WM_SETICON, WS_BORDER, WS_CAPTION, WS_DLGFRAME, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
        WS_THICKFRAME,
    };

    use crate::platform::win32_helpers::{set_window_long_ptr, wide};

    /// Window property that marks the live Settings HWND (cross-process, via a global atom).
    const SETTINGS_WINDOW_PROP: &str = "OpenTaskbarSettingsWindow";
    /// Per-session named mutex owned by the running `--settings` process.
    const SETTINGS_MUTEX_NAME: &str = r"Local\OpenTaskbar.SettingsWindow";
    /// `SetWindowSubclass` id for the non-client subclass.
    const NC_SUBCLASS_ID: usize = 0x4F54_4E43; // "OTNC"
    /// Private message (WM_APP range; GPUI only uses WM_USER-based ids) that re-runs
    /// WM_NCCALCSIZE from the message loop after the subclass is installed.
    const WM_APP_REFRAME: u32 = WM_APP + 0x0A11;
    /// Application icon resource id embedded in `OpenTaskBar.exe` (`1 ICON "OpenTaskBar.ico"`).
    const APP_ICON_RESOURCE_ID: usize = 1;
    const ERROR_ALREADY_EXISTS: u32 = 183;
    const WCA_ACCENT_POLICY: i32 = 19;
    const ACCENT_DISABLED: u32 = 0;

    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }

    #[repr(C)]
    struct WindowCompositionAttributeData {
        attribute: i32,
        data: *mut c_void,
        size: u32,
    }

    type SetWindowCompositionAttributeFn =
        unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> i32;

    // `windows-sys` gates `CreateMutexW` behind `Win32_Security` (not enabled), so bind it
    // directly; a null `SECURITY_ATTRIBUTES` pointer is all we ever pass.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "CreateMutexW"]
        fn create_mutex_w(
            attributes: *const core::ffi::c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> isize;
    }

    /// `RtlGetVersion` build >= 22000 (manifest-independent), cached for the process.
    pub(crate) fn is_windows_11() -> bool {
        crate::platform::win32_helpers::is_windows_11()
    }

    /// GPUI creates non-resizable `WindowKind::Normal` HWNDs with `WS_SYSMENU | WS_MINIMIZEBOX`
    /// only (neither `WS_POPUP` nor `WS_CAPTION | WS_THICKFRAME`), which causes Windows 11 DWM
    /// to ignore `DWMWCP_ROUND` and leave sharp 90-degree outer corners. Switching the fixed-size
    /// borderless dialog to `WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX` (clearing
    /// `WS_CAPTION | WS_THICKFRAME | WS_BORDER | WS_DLGFRAME` while preserving `WS_VISIBLE` and
    /// GPUI's `WS_EX_APPWINDOW | WS_EX_NOREDIRECTIONBITMAP`) enables native Win11 `DWMWCP_ROUND`
    /// corner clipping and drop shadow while keeping taskbar, Alt-Tab, minimize, and titlebar
    /// drag working cleanly just like `menu.rs`.
    fn ensure_roundable_style(hwnd: HWND) {
        // SAFETY: reading and updating the style flags of our own live window.
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        let updated = (style & !(WS_CAPTION | WS_THICKFRAME | WS_BORDER | WS_DLGFRAME))
            | WS_POPUP
            | WS_SYSMENU
            | WS_MINIMIZEBOX;
        if updated != style {
            set_window_long_ptr(hwnd, GWL_STYLE, updated as isize);
        }
    }

    /// Clears GPUI's `ACCENT_ENABLE_TRANSPARENTGRADIENT` (`gradient_color = 0x00000000`),
    /// which on Windows 11 paints opaque black behind transparent `WS_EX_NOREDIRECTIONBITMAP`
    /// swapchain pixels outside rounded corners when `HostBackdropBrush` is inactive.
    fn disable_accent_policy(hwnd: HWND) {
        let module_name = wide("user32.dll");
        // SAFETY: `user32.dll` is loaded in this process and `SetWindowCompositionAttribute`
        // is an ASCII null-terminated export name.
        let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
        if module.is_null() {
            return;
        }
        let address: FARPROC =
            unsafe { GetProcAddress(module, c"SetWindowCompositionAttribute".as_ptr().cast()) };
        let Some(address) = address else {
            return;
        };
        // SAFETY: matches the undocumented `SetWindowCompositionAttribute` Win32 ABI.
        let function: SetWindowCompositionAttributeFn = unsafe { std::mem::transmute(address) };
        let mut policy = AccentPolicy {
            state: ACCENT_DISABLED,
            flags: 0,
            gradient_color: 0,
            animation_id: 0,
        };
        let mut data = WindowCompositionAttributeData {
            attribute: WCA_ACCENT_POLICY,
            data: (&mut policy as *mut AccentPolicy).cast(),
            size: size_of::<AccentPolicy>() as u32,
        };
        unsafe {
            let _ = function(hwnd, &mut data);
        }
    }

    /// Applies Windows 11 rounded corners, suppresses the DWM border, and extends the glass
    /// frame across the whole client area so transparent swapchain corners never turn black.
    fn apply_dwm_chrome(hwnd: HWND) {
        if !is_windows_11() {
            return;
        }
        let corner = DWMWCP_ROUND;
        let border = DWMWA_COLOR_NONE;
        // Sheet-of-glass margins: DWM never paints opaque black behind the anti-aliased
        // rounded edges, even when the GPUI surface is Solid / True Black.
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        // SAFETY: plain DWM attribute writes on a live HWND with correctly sized values.
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                (&corner as *const i32).cast(),
                size_of::<i32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR as u32,
                (&border as *const u32).cast(),
                size_of::<u32>() as u32,
            );
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        }
    }

    /// GPUI's `WM_NCCALCSIZE` handler insets `frame_thickness` (~8 px) on the left, right and
    /// bottom even for non-resizable windows, exposing the class' black background brush as a
    /// sharp skirt around the rounded surface. Returning 0 without touching `rgrc[0]` (when not
    /// maximized) makes the client area cover the full window rect; `WM_NCHITTEST` suppresses
    /// `WS_THICKFRAME` resize borders so the dialog stays fixed-size.
    unsafe extern "system" fn nc_subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        _ref_data: usize,
    ) -> LRESULT {
        match msg {
            // SAFETY: `hwnd` is a live window on this thread.
            WM_NCCALCSIZE if wparam != 0 && unsafe { IsZoomed(hwnd) } == 0 => 0,
            WM_NCHITTEST => {
                // SAFETY: forwarding to GPUI's window proc first preserves HTCAPTION drag
                // regions while stripping resize edge hit-tests introduced by WS_THICKFRAME.
                let hit = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
                if matches!(
                    hit as u32,
                    HTLEFT
                        | HTRIGHT
                        | HTTOP
                        | HTTOPLEFT
                        | HTTOPRIGHT
                        | HTBOTTOM
                        | HTBOTTOMLEFT
                        | HTBOTTOMRIGHT
                        | HTBORDER
                ) {
                    HTCLIENT as LRESULT
                } else {
                    hit
                }
            }
            WM_APP_REFRAME => {
                // Runs from the Win32 message loop (outside any GPUI `App` borrow), so the
                // WM_SIZE this triggers reaches GPUI's resize callback and the viewport grows
                // to the full window rect. Re-apply DWM attributes right after SWP_FRAMECHANGED
                // so DWM picks up `DWMWCP_ROUND` against the updated `GWL_STYLE`.
                // SAFETY: frame-only SetWindowPos on our own live window.
                unsafe {
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        0,
                        0,
                        0,
                        0,
                        SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                apply_dwm_chrome(hwnd);
                0
            }
            WM_NCDESTROY => {
                let prop = wide(SETTINGS_WINDOW_PROP);
                // SAFETY: `hwnd` is still valid during WM_NCDESTROY; removing a missing
                // property / our own subclass is a harmless no-op.
                unsafe {
                    RemovePropW(hwnd, prop.as_ptr());
                    RemoveWindowSubclass(hwnd, Some(nc_subclass_proc), id);
                    DefSubclassProc(hwnd, msg, wparam, lparam)
                }
            }
            // SAFETY: forwarding to the next handler in the subclass chain.
            _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
        }
    }

    /// Installs the non-client subclass once per HWND (must run on the window's UI thread).
    fn install_nc_subclass(hwnd: HWND) {
        let mut ref_data = 0usize;
        // SAFETY: `hwnd` is a live window owned by the calling (GPUI) thread.
        unsafe {
            if GetWindowSubclass(hwnd, Some(nc_subclass_proc), NC_SUBCLASS_ID, &mut ref_data) != 0 {
                return;
            }
            if SetWindowSubclass(hwnd, Some(nc_subclass_proc), NC_SUBCLASS_ID, 0) == 0 {
                return;
            }
            // Re-run WM_NCCALCSIZE once (SWP_FRAMECHANGED) so the full-window client rect takes
            // effect. It is posted rather than called here: this runs inside GPUI's view
            // construction / render (the `App` is borrowed), where the synchronous WM_SIZE
            // could not update GPUI's viewport and the layout would stay at the inset size.
            PostMessageW(hwnd, WM_APP_REFRAME, 0, 0);
        }
    }

    /// Applies the outer window chrome. Safe to call repeatedly (every glass transition):
    /// `configure_host_backdrop_window` resets the DWM frame margins, so this must run after it.
    pub(crate) fn apply_window_chrome(hwnd: usize) {
        if hwnd == 0 {
            return;
        }
        let hwnd = hwnd as HWND;
        disable_accent_policy(hwnd);
        if is_windows_11() {
            ensure_roundable_style(hwnd);
        }
        install_nc_subclass(hwnd);
        // Windows 10: square corners, no DWM border attribute; GPUI uses radius 0.
        apply_dwm_chrome(hwnd);
    }

    /// Sets the embedded OpenTaskbar icon (resource id 1) as the window's big and small icon.
    /// A missing resource is a silent no-op (the GPUI class icon stays).
    pub(crate) fn apply_app_icon(hwnd: usize) {
        if hwnd == 0 {
            return;
        }
        let hwnd = hwnd as HWND;
        // SAFETY: loading an icon resource from our own module and sending it to our window.
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            if module.is_null() {
                return;
            }
            let dpi = GetDpiForWindow(hwnd);
            let metric = |index| {
                if dpi > 0 {
                    GetSystemMetricsForDpi(index, dpi)
                } else {
                    GetSystemMetrics(index)
                }
            };
            for (which, cx, cy) in [
                (ICON_BIG, metric(SM_CXICON), metric(SM_CYICON)),
                (ICON_SMALL, metric(SM_CXSMICON), metric(SM_CYSMICON)),
            ] {
                // MAKEINTRESOURCEW(1). Not LR_SHARED: the shared cache ignores the requested
                // size, which would hand the big icon to ICON_SMALL.
                let icon = LoadImageW(
                    module,
                    APP_ICON_RESOURCE_ID as *const u16,
                    IMAGE_ICON,
                    cx,
                    cy,
                    LR_DEFAULTCOLOR,
                );
                if icon.is_null() {
                    continue;
                }
                SendMessageW(hwnd, WM_SETICON, which as WPARAM, icon as LPARAM);
            }
        }
    }

    /// Marks `hwnd` as the live Settings window so other processes can find and focus it.
    pub(crate) fn mark_settings_window(hwnd: usize) {
        if hwnd == 0 {
            return;
        }
        let prop = wide(SETTINGS_WINDOW_PROP);
        // SAFETY: the property is removed again in `nc_subclass_proc` on WM_NCDESTROY.
        unsafe {
            SetPropW(
                hwnd as HWND,
                prop.as_ptr(),
                std::ptr::dangling_mut::<core::ffi::c_void>(),
            );
        }
    }

    /// Finds a live Settings window owned by another process.
    fn find_settings_window() -> Option<HWND> {
        struct Search {
            prop: Vec<u16>,
            own_pid: u32,
            found: HWND,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
            // SAFETY: `lparam` points at the live `Search` for the duration of EnumWindows.
            unsafe {
                let search = &mut *(lparam as *mut Search);
                if GetPropW(hwnd, search.prop.as_ptr()).is_null() {
                    return 1;
                }
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                if pid == search.own_pid {
                    return 1;
                }
                search.found = hwnd;
            }
            0
        }
        let mut search = Search {
            prop: wide(SETTINGS_WINDOW_PROP),
            // SAFETY: trivial process id query.
            own_pid: unsafe { GetCurrentProcessId() },
            found: std::ptr::null_mut(),
        };
        // SAFETY: the callback only touches `search`, which outlives the enumeration.
        unsafe {
            EnumWindows(Some(visit), &mut search as *mut Search as LPARAM);
        }
        (!search.found.is_null()).then_some(search.found)
    }

    /// Restores (if minimized) and focuses an existing Settings window. Returns `true` when one
    /// was found, in which case no second Settings process/window must be created.
    pub(crate) fn focus_existing_settings_window() -> bool {
        let Some(hwnd) = find_settings_window() else {
            return false;
        };
        // SAFETY: plain show/focus calls on a window that was alive during enumeration;
        // a window that closed in between makes these calls fail harmlessly.
        unsafe {
            if IsIconic(hwnd) != 0 || IsWindowVisible(hwnd) == 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
        }
        true
    }

    /// Per-session ownership of the Settings window, held for the `--settings` process lifetime.
    pub(crate) struct SettingsInstanceLock {
        _handle: isize,
    }

    /// Acquires the Settings single-instance lock. `None` means another `--settings` process
    /// already owns it; that process' window has then been focused (it may still be starting,
    /// so the lookup is retried briefly).
    pub(crate) fn acquire_settings_instance() -> Option<SettingsInstanceLock> {
        let name = wide(SETTINGS_MUTEX_NAME);
        // SAFETY: named mutex with a NUL-terminated name; the handle stays open (and owned) for
        // the rest of the process, released by the OS on exit.
        let (handle, already_exists) = unsafe {
            let handle = create_mutex_w(std::ptr::null(), 0, name.as_ptr());
            (handle, GetLastError() == ERROR_ALREADY_EXISTS)
        };
        if handle == 0 {
            // Cannot coordinate: fall back to the window lookup only.
            return if focus_existing_settings_window() {
                None
            } else {
                Some(SettingsInstanceLock { _handle: 0 })
            };
        }
        if !already_exists {
            return Some(SettingsInstanceLock { _handle: handle });
        }
        for _ in 0..30 {
            if focus_existing_settings_window() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // Keep the handle open: closing it would let a racing third launch believe it owns
        // Settings. It is released when this (exiting) process ends.
        None
    }

    /// Returns `(center_x, anchor_y)` in screen pixels for the given HWND so flyout previews
    /// open centered over the Settings window on its current monitor.
    pub(crate) fn window_flyout_anchor(hwnd: usize) -> Option<(i32, i32)> {
        if hwnd == 0 {
            return None;
        }
        let mut rect: RECT = unsafe { std::mem::zeroed() };
        // SAFETY: querying the live window rectangle into a stack-allocated `RECT`.
        if unsafe { GetWindowRect(hwnd as HWND, &mut rect) } == 0 {
            return None;
        }
        let cx = (rect.left + rect.right) / 2;
        let anchor_y = rect.top + ((rect.bottom - rect.top) * 3) / 4;
        Some((cx, anchor_y))
    }

    /// Primary monitor work area in DIP, derived from GPUI's logical display bounds and the
    /// physical monitor/work rectangles (so taskbars and docks are excluded).
    pub(crate) fn primary_work_area(
        display: gpui::Bounds<gpui::Pixels>,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<MONITORINFO>() as u32;
        // SAFETY: querying the primary monitor into a correctly sized MONITORINFO.
        let ok = unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            !monitor.is_null() && GetMonitorInfoW(monitor, &mut info) != 0
        };
        if !ok {
            return None;
        }
        let mon_w = (info.rcMonitor.right - info.rcMonitor.left) as f32;
        let mon_h = (info.rcMonitor.bottom - info.rcMonitor.top) as f32;
        let dip_w = f32::from(display.size.width);
        let dip_h = f32::from(display.size.height);
        if mon_w <= 0.0 || mon_h <= 0.0 || dip_w <= 0.0 || dip_h <= 0.0 {
            return None;
        }
        let sx = dip_w / mon_w;
        let sy = dip_h / mon_h;
        let left =
            f32::from(display.origin.x) + (info.rcWork.left - info.rcMonitor.left) as f32 * sx;
        let top = f32::from(display.origin.y) + (info.rcWork.top - info.rcMonitor.top) as f32 * sy;
        let width = (info.rcWork.right - info.rcWork.left) as f32 * sx;
        let height = (info.rcWork.bottom - info.rcWork.top) as f32 * sy;
        Some(gpui::Bounds {
            origin: gpui::point(gpui::px(left), gpui::px(top)),
            size: gpui::size(gpui::px(width), gpui::px(height)),
        })
    }
}

// ---------------------------------------------------------------------------
// Full backup (.otbackup) stored-ZIP writer/reader via the workspace `zip` crate.
// `FullBackupArchive.cs` packs manifest + settings + pins + active theme folder.
// ---------------------------------------------------------------------------

/// Creates a ZIP archive with stored (uncompressed) entries.
fn zip_store(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    use std::io::Write as _;
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            if writer.start_file(name, opts).is_ok() {
                let _ = writer.write_all(data);
            }
        }
        let _ = writer.finish();
    }
    cursor.into_inner()
}

/// Reads stored ZIP entries (rejects compressed entries with a clear error).
fn zip_unstore(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    use std::io::Read as _;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        if file.compression() != zip::CompressionMethod::Stored {
            return Err("compressed entries are not supported".to_owned());
        }
        let name = file.name().map_err(|e| e.to_string())?.into_owned();
        let mut data = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut data).map_err(|e| e.to_string())?;
        out.insert(name, data);
    }
    Ok(out)
}

fn safe_backup_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(name);
    if path.is_absolute()
        || name.contains('\\')
        || name.contains(':')
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(path.to_path_buf())
}

fn theme_folder() -> std::path::PathBuf {
    paths::theme_root()
}

fn build_backup(settings: &DockSettings) -> Vec<u8> {
    let mut pins = Map::new();
    pins.insert("PinnedApps".to_owned(), json!(settings.user_pinned_apps));
    pins.insert(
        "UnpinnedApps".to_owned(),
        json!(settings.user_unpinned_apps),
    );
    pins.insert(
        "PinnedBackgroundApps".to_owned(),
        json!(settings.user_pinned_background_apps),
    );
    pins.insert("PinOrder".to_owned(), json!(settings.pin_order));

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    files.push((
        "manifest.json".to_owned(),
        serde_json::to_vec_pretty(&json!({
            "Format": "OpenTaskbarFullBackup",
            "Version": 1,
            "CreatedUtc": chrono_stamp(),
        }))
        .unwrap_or_default(),
    ));
    files.push((
        "settings.json".to_owned(),
        serde_json::to_vec_pretty(settings).unwrap_or_default(),
    ));
    files.push((
        "pins.json".to_owned(),
        serde_json::to_vec_pretty(&pins).unwrap_or_default(),
    ));

    let mut theme_files: Vec<(String, Vec<u8>)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(theme_folder()) {
        for entry in entries.flatten().filter(|e| e.path().is_file()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(rel) = safe_backup_path(&format!("theme/{name}"))
                && let Ok(data) = std::fs::read(entry.path())
            {
                theme_files.push((rel.to_string_lossy().into_owned(), data));
            }
        }
    }
    theme_files.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, data) in &theme_files {
        files.push((name.clone(), data.clone()));
    }
    zip_store(&files)
}

fn chrono_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // ISO-like UTC stamp without a chrono dependency; only used for display in the manifest.
    let days = secs / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

use opentaskbar_core::civil_from_days;

#[derive(Clone, Debug, Default)]
struct BackupPreview {
    valid: bool,
    error: Option<String>,
    settings: Option<DockSettings>,
    theme_files: usize,
    pinned_apps: usize,
}

struct ThemeCardSpec {
    id: Option<String>,
    title: String,
    is_built_in: bool,
    swatches: ThemePreviewSwatches,
    preserve_original_icons: bool,
}

fn inspect_backup(bytes: &[u8]) -> BackupPreview {
    let entries = match zip_unstore(bytes) {
        Ok(entries) => entries,
        Err(err) => {
            return BackupPreview {
                valid: false,
                error: Some(err),
                ..Default::default()
            };
        }
    };
    let manifest_ok = entries
        .get("manifest.json")
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
        .and_then(|v| {
            v.get("Format")
                .and_then(Value::as_str)
                .map(|s| s == "OpenTaskbarFullBackup")
        })
        .unwrap_or(false);
    if !manifest_ok {
        return BackupPreview {
            valid: false,
            error: Some("not an OpenTaskbar .otbackup archive".to_owned()),
            ..Default::default()
        };
    }
    let settings = entries
        .get("settings.json")
        .and_then(|raw| serde_json::from_slice::<DockSettings>(raw).ok());
    let pinned_apps = entries
        .get("pins.json")
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
        .and_then(|v| v.get("PinnedApps")?.as_array().map(Vec::len))
        .unwrap_or(0);
    let theme_files = entries.keys().filter(|k| k.starts_with("theme/")).count();
    BackupPreview {
        valid: settings.is_some(),
        error: if settings.is_some() {
            None
        } else {
            Some("settings.json is missing or corrupt".to_owned())
        },
        settings,
        theme_files,
        pinned_apps,
    }
}

/// Prefix of the temporary staging folder used while importing theme files.
/// A `.otbackup` is unpacked here first so a failure can never leave a
/// half-written theme file in the live theme folder.
const THEME_STAGING_PREFIX: &str = ".staging-";

fn staging_dir(root: &std::path::Path) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    root.join(format!(
        "{THEME_STAGING_PREFIX}{}-{unique}",
        std::process::id()
    ))
}

/// Validated `theme/…` entries shared by the staging and commit passes.
fn theme_entries(entries: &BTreeMap<String, Vec<u8>>) -> Vec<(std::path::PathBuf, &[u8])> {
    entries
        .iter()
        .filter_map(|(name, data)| {
            let rel = name.strip_prefix("theme/")?;
            let path = safe_backup_path(rel)?;
            (!path.as_os_str().is_empty()).then_some((path, data.as_slice()))
        })
        .collect()
}

fn import_backup(bytes: &[u8]) -> Result<DockSettings, String> {
    import_backup_into(bytes, &theme_folder())
}

/// Real import path with an injectable theme root. All theme payloads land in
/// a unique staging directory first, are committed with a second pass, and the
/// staging directory is removed on success, failure and cancellation alike.
fn import_backup_into(bytes: &[u8], root: &std::path::Path) -> Result<DockSettings, String> {
    let preview = inspect_backup(bytes);
    if !preview.valid {
        return Err(preview
            .error
            .unwrap_or_else(|| "invalid archive".to_owned()));
    }
    let entries = zip_unstore(bytes)?;
    let mut next = preview.settings.ok_or("no settings in archive")?;
    if let Some(pins) = entries.get("pins.json")
        && let Ok(value) = serde_json::from_slice::<Value>(pins)
    {
        let list = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        next.user_pinned_apps = list("PinnedApps");
        next.user_unpinned_apps = list("UnpinnedApps");
        next.user_pinned_background_apps = list("PinnedBackgroundApps");
        next.pin_order = value.get("PinOrder").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
    }

    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let staging = staging_dir(root);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let cleanup = || {
        let _ = std::fs::remove_dir_all(&staging);
    };

    let themes = theme_entries(&entries);
    let staged = (|| -> Result<(), String> {
        for (rel, data) in &themes {
            let target = staging.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&target, data).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(err) = staged {
        cleanup();
        return Err(err);
    }

    let committed = (|| -> Result<(), String> {
        for (rel, _) in &themes {
            let source = staging.join(rel);
            let target = root.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let data = std::fs::read(&source).map_err(|e| e.to_string())?;
            std::fs::write(&target, data).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    cleanup();
    committed?;
    Ok(next)
}

// ---------------------------------------------------------------------------
// WS-A fields that are still in `DockSettings.extra` until the settings worktree lands.
// ---------------------------------------------------------------------------

fn extra_number(settings: &DockSettings, key: &str, fallback: i32) -> i32 {
    settings
        .extra
        .get(key)
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .unwrap_or(fallback)
}

fn set_extra_number(settings: &mut DockSettings, key: &str, value: i32) {
    settings.extra.insert(key.to_owned(), json!(value));
}

/// Resolves a startup toggle against the observed registry state. A failed write is shown even
/// when `read()` still reports the old value; an unreadable state keeps the requested position so
/// the switch never lies about a known target. A successful write that `read()` cannot confirm is
/// reported as a write error.
fn resolve_startup_toggle(
    requested: bool,
    write_error: Option<&'static str>,
    observed: (bool, Option<&'static str>),
) -> (bool, Option<&'static str>) {
    let (enabled, read_error) = observed;
    if let Some(error) = write_error {
        (enabled, Some(error))
    } else if let Some(error) = read_error {
        (requested, Some(error))
    } else if enabled != requested {
        (enabled, Some("StartupWriteError"))
    } else {
        (enabled, None)
    }
}

/// Snapshot the full profile contract (19 fields incl. name, i.e. the 15+ payload fields the
/// reference profile service persists) so creating/saving a profile never drops layout, widget,
/// theme or hotkey state. Applying is delegated to the cfg workstream's
/// `settings::apply_full_profile`, which is the single authoritative mapper.
fn snapshot_profile(
    settings: &DockSettings,
    name: String,
) -> opentaskbar_core::settings::DockProfile {
    opentaskbar_core::settings::DockProfile {
        name,
        position: settings.position,
        size: settings.size,
        auto_hide: settings.auto_hide,
        enabled_widgets: settings.enabled_widgets.clone(),
        hidden_apps: settings.user_unpinned_apps.clone(),
        installed_widgets: settings.installed_widgets.clone(),
        widget_order: settings.widget_order.clone(),
        dock_item_order: settings.dock_item_order.clone(),
        dock_width_mode: Some(settings.dock_width_mode),
        dock_width_pixels: Some(settings.dock_width_pixels),
        dock_width_percent: Some(settings.dock_width_percent),
        theme: Some(settings.theme),
        theme_pack: Some(settings.theme_pack.clone()),
        theme_application_icons: Some(settings.theme_application_icons),
        launcher_hotkey: Some(settings.launcher_hotkey),
        dock_hotkey: Some(settings.dock_hotkey),
        custom_launcher_hotkey: settings.custom_launcher_hotkey.clone(),
        custom_dock_hotkey: settings.custom_dock_hotkey.clone(),
        extra: settings.extra.clone(),
    }
}

/// Maps a cell of the 2D saturation/value field to HSV (x = saturation, y = 1 - value).
fn field_cell_hsv(hue: f64, col: usize, row: usize, cols: usize, rows: usize) -> Hsv {
    Hsv::new(
        hue,
        if cols == 0 {
            0.0
        } else {
            (col as f64 + 0.5) / cols as f64
        },
        if rows == 0 {
            1.0
        } else {
            1.0 - (row as f64 + 0.5) / rows as f64
        },
    )
}

/// Reorders a gradient stop and returns its new index (bounded by the stop count).
fn reorder_gradient_stop(
    stops: &mut Vec<crate::design::tokens::Rgba>,
    from: usize,
    to: usize,
) -> usize {
    let len = stops.len();
    if len == 0 {
        return 0;
    }
    let target = to.min(len - 1);
    if from < len && from != target {
        let color = stops.remove(from);
        stops.insert(target, color);
    }
    target
}

/// Nearest stop index for a normalized position on the gradient strip.
fn gradient_stop_at(position: f64, len: usize) -> usize {
    ((position * len as f64).round() as usize).min(len.saturating_sub(1))
}

/// Fallback city shown when no weather city is configured, spelled the way the UI language
/// writes it ("İstanbul" in Turkish).
fn weather_fallback_city_for(language: &str) -> &'static str {
    if language.starts_with("tr") {
        "İstanbul"
    } else {
        "Istanbul"
    }
}

fn weather_fallback_city() -> &'static str {
    weather_fallback_city_for(loc::language())
}

/// Short clock-format label for widget summaries, derived from the existing
/// `ClockFormat24Hour` / `ClockFormat12Hour` strings by dropping their "(14:32)" example.
fn short_clock_format_label(full: &str) -> &str {
    full.split(" (").next().unwrap_or(full).trim()
}

/// True when the clock's date line already carries the weekday (e.g. "Cum 27 Kas"),
/// so a separate weekday line would repeat it.
fn clock_weekday_is_redundant(date: &str, weekday: &str) -> bool {
    let weekday = weekday.trim();
    !weekday.is_empty() && date.split_whitespace().any(|word| word == weekday)
}

/// Circular progress ring drawn via GPUI `canvas` + `PathBuilder`, matching DComp's arc meters.
fn progress_ring(
    size_px: f32,
    stroke_px: f32,
    ratio: f32,
    track_color: u32,
    fill_color: u32,
) -> impl IntoElement {
    let clamped = ratio.clamp(0.0, 1.0);
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let cx = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5;
            let cy = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;
            let rad = ((f32::from(bounds.size.width).min(f32::from(bounds.size.height))
                - stroke_px)
                * 0.5)
                .max(1.0);
            let arc = |start: f32, end: f32| {
                let mut pb = gpui::PathBuilder::stroke(px(stroke_px));
                pb.move_to(gpui::point(
                    px(cx + start.cos() * rad),
                    px(cy + start.sin() * rad),
                ));
                pb.arc_to(
                    gpui::point(px(rad), px(rad)),
                    px(0.0),
                    (end - start).abs() > std::f32::consts::PI,
                    true,
                    gpui::point(px(cx + end.cos() * rad), px(cy + end.sin() * rad)),
                );
                pb.build()
            };
            let top = -std::f32::consts::FRAC_PI_2;
            let bot = std::f32::consts::FRAC_PI_2;
            if let Ok(path) = arc(top, bot) {
                window.paint_path(path, rgba(track_color));
            }
            if let Ok(path) = arc(bot, top + std::f32::consts::TAU) {
                window.paint_path(path, rgba(track_color));
            }
            if clamped >= 0.995 {
                if let Ok(path) = arc(top, bot) {
                    window.paint_path(path, rgba(fill_color));
                }
                if let Ok(path) = arc(bot, top + std::f32::consts::TAU) {
                    window.paint_path(path, rgba(fill_color));
                }
            } else if clamped > 0.005 {
                let end = top + clamped * std::f32::consts::TAU;
                if let Ok(path) = arc(top, end) {
                    window.paint_path(path, rgba(fill_color));
                }
            }
        },
    )
    .size(px(size_px))
    .flex_shrink_0()
}

/// Crisp tinted monogram badge for AI Usage preview cards.
fn ai_brand_badge(provider_id: &str, brand_color: (u8, u8, u8), is_dark: bool) -> gpui::Div {
    let monogram = match provider_id {
        "aiusage_claude" => "C",
        "aiusage_codex" => "Cx",
        "aiusage_cursor" => "Cu",
        "aiusage_opencode" => "OC",
        "aiusage_copilot" => "Co",
        "aiusage_deepseek" => "DS",
        "aiusage_openrouter" => "OR",
        _ => "AI",
    };
    let tint = if !is_dark && brand_color == (0xf3, 0xf4, 0xf6) {
        (0x3f, 0x3f, 0x46)
    } else {
        brand_color
    };
    div()
        .size(px(20.0))
        .rounded(px(6.0))
        .bg(rgba(crate::design::gpui_rgba(tint, 0x26)))
        .border_1()
        .border_color(rgba(crate::design::gpui_rgba(tint, 0x7a)))
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .child(
            div()
                .text_size(px(fonts::scaled_size(9.5)))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(rgba(crate::design::gpui_rgba(tint, 0xff)))
                .child(monogram),
        )
}

/// Reads a widget density override using its canonical PascalCase key (`"Clock"`, `"Weather"`,
/// `"SystemStats"`), falling back to any legacy casing stored before canonicalization.
fn read_density_override(settings: &DockSettings, canonical_id: &str) -> WidgetDensityMode {
    settings
        .widget_density_overrides
        .get(canonical_id)
        .copied()
        .or_else(|| {
            settings
                .widget_density_overrides
                .iter()
                .find(|(k, _)| settings::canonical_widget_setting_id(k) == canonical_id)
                .map(|(_, &v)| v)
        })
        .unwrap_or(WidgetDensityMode::Auto)
}

/// Persists a widget density override under its canonical PascalCase key, removing any stale
/// legacy-cased entry and omitting `Auto` entries so the map stays clean.
fn write_density_override(
    settings: &mut DockSettings,
    canonical_id: &str,
    mode: WidgetDensityMode,
) {
    settings
        .widget_density_overrides
        .retain(|k, _| settings::canonical_widget_setting_id(k) != canonical_id);
    if mode != WidgetDensityMode::Auto {
        settings
            .widget_density_overrides
            .insert(canonical_id.to_owned(), mode);
    }
}

/// Country/city data now comes from `platform::country_city` (offline fallback
/// catalogue plus the countriesnow.space API). No static UI-side copy remains.
fn city_list(country: &str) -> Vec<String> {
    country_city::cities(country)
}

/// Builds the persisted `"City, Country"` value for an automatic location fix. A saved city
/// (including one typed while the probe was running) is never overwritten.
fn location_auto_fill_value(
    saved_city: &str,
    detected: &Option<(String, String)>,
) -> Option<String> {
    if !country_city::should_auto_geolocate(saved_city, country_city::LocationAccess::Granted) {
        return None;
    }
    detected
        .as_ref()
        .map(|(country, city)| format!("{city}, {country}"))
}

/// Resolves an executable on `PATH` (`AiUsageService.FindExecutableOnPath`).
fn find_executable_on_path(command: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for extension in [".exe", ".cmd", ".bat"] {
            let candidate = dir.join(format!("{command}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// CLI sign-in command per provider (`AiUsageService.CliSignInArguments`).
fn ai_cli_sign_in_command(provider_id: &str) -> Option<(&'static str, Vec<&'static str>)> {
    match provider_id {
        "aiusage_claude" => Some(("claude", vec!["claude", "auth", "login"])),
        "aiusage_codex" => Some(("codex", vec!["codex", "login"])),
        "aiusage_copilot" => Some(("gh", vec!["gh", "auth", "login"])),
        "aiusage_opencode" => Some(("opencode", vec!["opencode", "auth", "login"])),
        _ => None,
    }
}

/// Launches the provider's CLI sign-in in Windows Terminal when available,
/// otherwise in a visible `cmd.exe`. Returns `false` when the CLI is missing.
fn launch_ai_cli_sign_in(provider_id: &str) -> bool {
    let Some((executable, args)) = ai_cli_sign_in_command(provider_id) else {
        return false;
    };
    if find_executable_on_path(executable).is_none() {
        return false;
    }
    let mut command = if find_executable_on_path("wt").is_some() {
        let mut terminal = std::process::Command::new("wt");
        terminal.args(["-w", "new", "--", "cmd.exe", "/c"]);
        terminal
    } else {
        let mut shell = std::process::Command::new("cmd.exe");
        shell.arg("/c");
        shell
    };
    command.args(args.iter().copied()).arg("& pause");
    command.spawn().is_ok()
}

/// Builds the brush currently edited by the picker widgets.
fn build_brush(
    mode: BrushMode,
    hsv: Hsv,
    alpha: u8,
    angle: f64,
    stops: &[crate::design::tokens::Rgba],
) -> BrushSpec {
    match mode {
        BrushMode::Solid => BrushSpec::Solid(hsv.to_rgba(alpha)),
        BrushMode::Gradient => {
            let mut list = stops.to_vec();
            if list.is_empty() {
                list.push(hsv.to_rgba(alpha));
            }
            BrushSpec::Gradient(GradientSpec::new(list, angle)).with_alpha(alpha)
        }
    }
}

/// Serializes a brush into the JSON shape understood by `token_overrides_from_json`.
fn brush_to_json(spec: &BrushSpec) -> Value {
    match spec {
        BrushSpec::Solid(color) => json!(color.hex()),
        BrushSpec::Gradient(gradient) => json!({
            "stops": gradient
                .stops
                .iter()
                .map(|color| color.hex())
                .collect::<Vec<_>>(),
            "angle": gradient.angle_deg,
        }),
    }
}

/// Persists manual token overrides under `extra["ThemeTokens"]` and applies them to the
/// highest `overrides` layer without disturbing the active theme-pack layer.
fn persist_theme_tokens(settings: &mut DockSettings, map: Map<String, Value>) {
    apply_token_overrides(token_overrides_from_json(&map));
    if map.is_empty() {
        settings.extra.remove("ThemeTokens");
    } else {
        settings
            .extra
            .insert("ThemeTokens".to_owned(), Value::Object(map));
    }
}

/// Maps an editor token key into one of the `THEME_TOKEN_CATEGORIES` indices so the
/// Theme Studio advanced token inspector filters all 51+ `EDITOR_TOKENS` accurately.
fn token_category_index(key: &str) -> usize {
    let lower = key.to_ascii_lowercase();
    if lower.contains("text") || lower.contains("font") {
        1 // ThemeCategoryText
    } else if lower.contains("accent")
        || lower.contains("indicator")
        || lower.contains("notification")
        || lower.contains("logo")
        || lower.contains("blue")
        || lower.contains("green")
        || lower.contains("warning")
        || lower.contains("danger")
    {
        2 // ThemeCategoryAccents
    } else if lower.contains("border") || lower.contains("stroke") || lower.contains("divider") {
        3 // ThemeCategoryBorders
    } else if lower.starts_with("widget") || lower.starts_with("dockwidget") {
        4 // Widgets
    } else if lower.contains("shadow")
        || lower.contains("glow")
        || lower.contains("highlight")
        || lower.contains("sheen")
        || lower.contains("bevel")
        || lower.contains("scrim")
    {
        5 // ThemeCategoryEffects
    } else if lower.contains("material")
        || lower.contains("surface")
        || lower.contains("background")
        || lower.contains("sidebar")
        || lower.contains("control")
        || lower.contains("tile")
    {
        0 // SectionSurfaces
    } else {
        6 // ThemeSectionAdvanced
    }
}

/// Filters `EDITOR_TOKENS` to those belonging to `category_index` in `THEME_TOKEN_CATEGORIES`.
fn editor_tokens_for_category(category_index: usize) -> Vec<&'static str> {
    EDITOR_TOKENS
        .iter()
        .filter(|tok| {
            matches!(tok.kind, TokenKind::Brush | TokenKind::Color)
                && token_category_index(tok.key) == category_index
        })
        .map(|tok| tok.key)
        .collect()
}

/// Resolves the country for a persisted `"City, Country"` string, falling back
/// to the offline reverse lookup when the suffix is not in the catalogue.
fn weather_country_for(city_label: &str) -> String {
    if let Some((_, country)) = city_label.rsplit_once(", ") {
        return country.to_owned();
    }
    let name = city_label.split(", ").next().unwrap_or(city_label);
    let code = country_city::find_cached_country_for_city(name).unwrap_or("TR");
    country_city::countries()
        .into_iter()
        .find(|(iso, _)| iso == code)
        .map(|(_, display)| display)
        .unwrap_or_else(|| "Turkey".to_owned())
}

/// Shared handler shape used by the theme-editor sliders.
type UiHandler = std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

const THEME_TOKEN_CATEGORIES: &[&str] = &[
    "SectionSurfaces",
    "ThemeCategoryText",
    "ThemeCategoryAccents",
    "ThemeCategoryBorders",
    "Widgets",
    "ThemeCategoryEffects",
    "ThemeSectionAdvanced",
];

fn notify_docks_reload() {
    let wide: Vec<u16> = RELOAD_MSG_NAME.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let msg = RegisterWindowMessageW(wide.as_ptr());
        if msg != 0 {
            SendNotifyMessageW(HWND_BROADCAST, msg, 0, 0);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveTextField {
    Search,
    CustomThemeName,
    AiSecret(&'static str),
}

struct SettingsView {
    page: SettingsPage,
    focus: FocusHandle,
    /// Header search field. UTF-16 offsets keep the GPUI platform input handler and the
    /// rendered caret in agreement even for non-ASCII (e.g. Turkish) text.
    search_text: String,
    search_selection: Range<usize>,
    search_focus: FocusHandle,
    /// Custom theme name input inside Theme Studio (routed through `EntityInputHandler`).
    custom_theme_name: String,
    custom_theme_name_selection: Range<usize>,
    custom_theme_name_focus: FocusHandle,
    /// Collapsible advanced token inspector in Theme Studio.
    advanced_tokens_open: bool,
    /// Per-provider API key/token input buffers & focus handles (masked in UI, never persisted
    /// to settings.json — written only to Windows DPAPI and immediately cleared).
    ai_secret_buffers: HashMap<&'static str, String>,
    ai_secret_selections: HashMap<&'static str, Range<usize>>,
    ai_secret_focus: HashMap<&'static str, FocusHandle>,
    ai_test_status: HashMap<&'static str, (bool, String)>,
    ai_testing: HashSet<&'static str>,
    settings: DockSettings,
    widget_runtime: widgets::WidgetRuntime,
    widget_filter: &'static str,
    expanded_widget: Option<&'static str>,
    open_dropdown: Option<String>,
    startup_enabled: bool,
    /// Localized error key from the last startup write/read (empty when both succeeded).
    startup_status: String,
    scroll_handle: ScrollHandle,
    /// Row targeted by the most recent Settings search result. Rendered with a transient
    /// highlight and used to select which row receives the scroll anchor.
    highlight_row: Option<String>,
    /// Fresh anchor created for each search jump; attached to `highlight_row` during render
    /// and consumed once `ScrollAnchor::scroll_to` has been scheduled.
    scroll_anchor: Option<ScrollAnchor>,
    save_error: Option<String>,
    /// Cached installed Windows font families for the in-app searchable font dropdown.
    installed_fonts: Vec<String>,
    font_picker: SearchableDropdownState,
    font_picker_focus: FocusHandle,
    hotkey_status: String,
    theme_status: String,
    /// True trailing-edge debounce: every edit bumps `save_generation` and only the latest
    /// timer flushes to disk + broadcasts reload.
    save_pending: bool,
    save_generation: u64,
    update_status: String,
    update_release: Option<update::Release>,
    // Search (reference SettingsWindow.Search.cs parity)
    // WidgetPanelHost (Escape / focus return / 300 ms suppress / preview toggle)
    widget_panel: Option<&'static str>,
    panel_opened_at: Option<Instant>,
    flyout_preview: bool,
    // Settings features
    backup_status: String,
    backup_preview: Option<BackupPreview>,
    weather_country: String,
    /// City list for the selected country, refreshed from `country_city`.
    weather_cities: Vec<String>,
    weather_city_index: usize,
    /// Filter buffers for the searchable country/city pickers (keyed by dropdown id).
    country_picker: SearchableDropdownState,
    city_picker: SearchableDropdownState,
    country_picker_focus: FocusHandle,
    city_picker_focus: FocusHandle,
    /// True while the displayed location came from the automatic geolocation probe.
    weather_auto_applied: bool,
    theme_category: usize,
    token_name: String,
    token_apply_status: String,
    // Shared Figma-style color & gradient brush picker modal state.
    brush_picker_open: bool,
    brush_picker_target: BrushPickerTarget,
    brush_hex_focus: FocusHandle,
    brush_mode: BrushMode,
    brush_hsv: Hsv,
    brush_alpha: u8,
    brush_angle: f64,
    brush_stops: Vec<crate::design::tokens::Rgba>,
    brush_selected_stop: usize,
    brush_hex: String,
    /// Clear Crystal optical glass host (`topmost = false`) underneath GPUI's DComp swapchain.
    spatial_glass: Option<SpatialGlassHost>,
    last_glass_hwnd_state: Option<(bool, bool)>,
    ai_status: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BrushMode {
    Solid,
    Gradient,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BrushPickerTarget {
    ThemeToken,
    BorderSurface(usize),
}

impl SettingsView {
    fn new(settings: DockSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        fonts::apply_settings(&settings);
        sync_from_settings(&settings);
        let focus = cx.focus_handle();
        focus.focus(window);
        let mut widget_runtime = widgets::WidgetRuntime::default();
        widget_runtime.tick(&settings);
        let is_dark = match settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        };
        let (tint_rgb, rim_rgb) = theme_surface_tint("SettingsBackground", "AccentBrush");
        let glass_cfg = SpatialGlassConfig::from_settings(
            &settings,
            settings.settings_background,
            settings.settings_opacity,
            is_dark,
            false,
        )
        .with_theme_tint(tint_rgb, rim_rgb);
        let hwnd = configure_settings_glass_hwnd(window, is_dark, glass_cfg.enabled);
        // Rounded Win11 chrome / no phantom NC skirt (after the backdrop margins are set),
        // the real app icon and the cross-process single-instance marker.
        window_chrome::apply_window_chrome(hwnd);
        window_chrome::apply_app_icon(hwnd);
        window_chrome::mark_settings_window(hwnd);
        let initial_glass_hwnd_state = Some((is_dark, glass_cfg.enabled));
        let mut spatial_glass = if glass_cfg.enabled {
            SpatialGlassHost::attach(hwnd, false, glass_cfg)
        } else {
            None
        };
        if let Some(ref mut g) = spatial_glass {
            let dpi = win32::dpi_scale(hwnd);
            let vp = window.viewport_size();
            // The window may be clamped below WIN_W×WIN_H to fit the work area: use the
            // real viewport so the glass rim matches the visible window edge.
            let w_px = (f32::from(vp.width) * dpi).max(2.0);
            let h_px = (f32::from(vp.height) * dpi).max(2.0);
            let radius_px = (settings_corner_radius(window_chrome::is_windows_11()) * dpi).round();
            g.sync_rect(0.0, 0.0, w_px, h_px, radius_px, dpi, 0.0, 0.0);
        }
        let installed_fonts = fonts::installed_fonts().to_vec();
        let saved_city = settings.weather_city.clone();
        let weather_city = if saved_city.trim().is_empty() {
            weather_fallback_city().to_owned()
        } else {
            saved_city.clone()
        };
        let weather_country = weather_country_for(&weather_city);
        let weather_cities = city_list(&weather_country);
        let city_name = weather_city.split(", ").next().unwrap_or(&weather_city);
        let weather_city_index = weather_cities
            .iter()
            .position(|c| c.eq_ignore_ascii_case(city_name))
            .unwrap_or(0);
        let accent = crate::design::active_palette(is_dark).accent;
        let brush_color = crate::design::tokens::Rgba::from_rgb(accent.0, accent.1, accent.2);
        let (startup_enabled, startup_error) = startup_registration::read();
        let mut ai_secret_focus = HashMap::new();
        for meta in widgets::telemetry::AI_PROVIDERS {
            ai_secret_focus.insert(meta.id, cx.focus_handle());
        }
        let initial_theme_name = if !settings.theme_pack.trim().is_empty()
            && !theme_packs::is_built_in(&settings.theme_pack)
        {
            theme_packs::ThemePack::load(&Self::themes_root().join(&settings.theme_pack))
                .map(|p| p.display_name().to_owned())
                .unwrap_or_else(|_| settings.theme_pack.clone())
        } else {
            String::new()
        };
        let initial_theme_name_len = initial_theme_name.encode_utf16().count();
        let mut view = Self {
            page: SettingsPage::Dock,
            focus,
            search_text: String::new(),
            search_selection: 0..0,
            search_focus: cx.focus_handle(),
            custom_theme_name: initial_theme_name,
            custom_theme_name_selection: initial_theme_name_len..initial_theme_name_len,
            custom_theme_name_focus: cx.focus_handle(),
            advanced_tokens_open: false,
            ai_secret_buffers: HashMap::new(),
            ai_secret_selections: HashMap::new(),
            ai_secret_focus,
            ai_test_status: HashMap::new(),
            ai_testing: HashSet::new(),
            settings,
            widget_runtime,
            widget_filter: "All",
            expanded_widget: None,
            open_dropdown: None,
            startup_enabled,
            startup_status: startup_error.map(loc::t).unwrap_or_default(),
            scroll_handle: ScrollHandle::new(),
            highlight_row: None,
            scroll_anchor: None,
            save_error: None,
            installed_fonts,
            font_picker: SearchableDropdownState::new(),
            font_picker_focus: cx.focus_handle(),
            hotkey_status: String::new(),
            theme_status: String::new(),
            save_pending: false,
            save_generation: 0,
            update_status: String::new(),
            update_release: None,
            widget_panel: None,
            panel_opened_at: None,
            flyout_preview: true,
            backup_status: String::new(),
            backup_preview: None,
            weather_country,
            weather_cities,
            weather_city_index,
            country_picker: SearchableDropdownState::new(),
            city_picker: SearchableDropdownState::new(),
            country_picker_focus: cx.focus_handle(),
            city_picker_focus: cx.focus_handle(),
            weather_auto_applied: false,
            theme_category: 0,
            token_name: "AccentBrush".to_owned(),
            token_apply_status: loc::t("TokenApplyHint"),
            brush_picker_open: false,
            brush_picker_target: BrushPickerTarget::ThemeToken,
            brush_hex_focus: cx.focus_handle(),
            brush_mode: BrushMode::Solid,
            brush_hsv: Hsv::from(brush_color),
            brush_alpha: brush_color.a,
            brush_angle: 90.0,
            brush_stops: GradientSpec::from_endpoints(brush_color.darken(0.35), brush_color, 90.0)
                .stops,
            brush_selected_stop: 0,
            brush_hex: brush_color.hex(),
            spatial_glass,
            last_glass_hwnd_state: initial_glass_hwnd_state,
            ai_status: loc::t(if cursor_auth::has_session() {
                "AiCursorSignedIn"
            } else {
                "AiCursorNotSignedIn"
            }),
        };
        if saved_city.trim().is_empty() {
            view.auto_detect_location(cx);
        }
        view
    }

    fn focused_text_field(&self, window: &Window) -> ActiveTextField {
        if self.custom_theme_name_focus.is_focused(window) {
            return ActiveTextField::CustomThemeName;
        }
        for (&provider_id, handle) in &self.ai_secret_focus {
            if handle.is_focused(window) {
                return ActiveTextField::AiSecret(provider_id);
            }
        }
        ActiveTextField::Search
    }

    fn text_field_ref(&self, field: ActiveTextField) -> (&str, Range<usize>) {
        match field {
            ActiveTextField::Search => (self.search_text.as_str(), self.search_selection.clone()),
            ActiveTextField::CustomThemeName => (
                self.custom_theme_name.as_str(),
                self.custom_theme_name_selection.clone(),
            ),
            ActiveTextField::AiSecret(pid) => {
                let text = self
                    .ai_secret_buffers
                    .get(pid)
                    .map(String::as_str)
                    .unwrap_or("");
                let len = text.encode_utf16().count();
                let sel = self
                    .ai_secret_selections
                    .get(pid)
                    .cloned()
                    .unwrap_or(len..len);
                (text, sel)
            }
        }
    }

    fn replace_text_field_range(
        &mut self,
        field: ActiveTextField,
        range: Range<usize>,
        text: &str,
    ) {
        // Strip newlines from single-line inputs (e.g. pasted API keys or theme names).
        let clean: String = text
            .chars()
            .filter(|ch| !matches!(ch, '\r' | '\n'))
            .collect();
        let (buf, sel) = match field {
            ActiveTextField::Search => (&mut self.search_text, &mut self.search_selection),
            ActiveTextField::CustomThemeName => (
                &mut self.custom_theme_name,
                &mut self.custom_theme_name_selection,
            ),
            ActiveTextField::AiSecret(pid) => {
                let buf = self.ai_secret_buffers.entry(pid).or_default();
                let len = buf.encode_utf16().count();
                let sel = self.ai_secret_selections.entry(pid).or_insert(len..len);
                (buf, sel)
            }
        };
        let len = buf.encode_utf16().count();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        let start_byte = Self::byte_index_for_utf16(buf, start);
        let end_byte = Self::byte_index_for_utf16(buf, end);

        let mut next = String::with_capacity(buf.len() + clean.len());
        next.push_str(&buf[..start_byte]);
        next.push_str(&clean);
        next.push_str(&buf[end_byte..]);

        let cursor = start + clean.encode_utf16().count();
        *buf = next;
        *sel = cursor..cursor;
        if field == ActiveTextField::Search {
            // Any edit means the user is searching again; the previous jump marker is stale.
            self.highlight_row = None;
            self.scroll_anchor = None;
        }
    }

    fn handle_text_field_key_down(
        &mut self,
        field: ActiveTextField,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        if event.keystroke.modifiers.control {
            if key.eq_ignore_ascii_case("v") {
                if let Some(pasted) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let (_, sel) = self.text_field_ref(field);
                    self.replace_text_field_range(field, sel, &pasted);
                    cx.notify();
                }
                return true;
            }
            if key.eq_ignore_ascii_case("a") {
                let (text, _) = self.text_field_ref(field);
                let len = text.encode_utf16().count();
                match field {
                    ActiveTextField::Search => self.search_selection = 0..len,
                    ActiveTextField::CustomThemeName => self.custom_theme_name_selection = 0..len,
                    ActiveTextField::AiSecret(pid) => {
                        self.ai_secret_selections.insert(pid, 0..len);
                    }
                }
                cx.notify();
                return true;
            }
        }
        match key {
            "backspace" => {
                let (text, sel) = self.text_field_ref(field);
                let len = text.encode_utf16().count();
                let end = sel.end.min(len);
                if end > sel.start {
                    self.replace_text_field_range(field, sel.start..end, "");
                } else if end > 0 {
                    self.replace_text_field_range(field, end - 1..end, "");
                }
                cx.notify();
                true
            }
            "delete" => {
                let (text, sel) = self.text_field_ref(field);
                let len = text.encode_utf16().count();
                let start = sel.start.min(len);
                let end = sel.end.clamp(start, len);
                if end > start {
                    self.replace_text_field_range(field, start..end, "");
                } else if start < len {
                    self.replace_text_field_range(field, start..start + 1, "");
                }
                cx.notify();
                true
            }
            _ => false,
        }
    }

    fn byte_index_for_utf16(text: &str, offset: usize) -> usize {
        let mut units = 0;
        for (index, ch) in text.char_indices() {
            if units >= offset {
                return index;
            }
            units += ch.len_utf16();
        }
        text.len()
    }

    fn clear_search(&mut self) {
        self.search_text.clear();
        self.search_selection = 0..0;
    }

    /// Render a searchable setting row. The stable `id` is shared with the search catalog;
    /// highlight and scroll anchor are attached only while this row is the active target.
    fn target_row(
        &self,
        id: &'static str,
        t: &UiTheme,
        icon: Option<&'static str>,
        title: impl Into<SharedString>,
        control: impl IntoElement,
    ) -> AnyElement {
        let highlighted = self.highlight_row.as_deref() == Some(id);
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        setting_row_with_id(id, t, anchor, highlighted, icon, title, control).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn target_row_desc(
        &self,
        id: &'static str,
        t: &UiTheme,
        icon: Option<&'static str>,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
        control: impl IntoElement,
    ) -> AnyElement {
        let highlighted = self.highlight_row.as_deref() == Some(id);
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        setting_row_with_id_desc(
            id,
            t,
            anchor,
            highlighted,
            icon,
            title,
            description,
            control,
        )
        .into_any_element()
    }

    /// Same treatment as [`Self::target_row`] for blocks that aren't setting rows
    /// (section descriptions, filter pills, widget placement rows when empty).
    fn mark_target(
        &self,
        id: &'static str,
        t: &UiTheme,
        el: gpui::Div,
    ) -> gpui::Stateful<gpui::Div> {
        let highlighted = self.highlight_row.as_deref() == Some(id);
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        let el = if highlighted {
            el.bg(rgba(t.accent_soft)).rounded(px(8.0))
        } else {
            el
        };
        el.id(SharedString::from(id)).anchor_scroll(anchor)
    }

    fn make_dropdown(
        &self,
        id: impl Into<String>,
        t: &UiTheme,
        options: Vec<DropdownOption>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = id.into();
        let is_open = self.open_dropdown.as_deref() == Some(id.as_str());
        let entity = cx.entity().downgrade();
        let toggle_id = id.clone();
        dropdown(id, t, is_open, options, move |_win, app| {
            let _ = entity.update(app, |this, cx| {
                if this.open_dropdown.as_deref() == Some(toggle_id.as_str()) {
                    this.open_dropdown = None;
                } else {
                    this.open_dropdown = Some(toggle_id.clone());
                }
                cx.notify();
            });
        })
    }

    /// Builds a dropdown for a copyable enum without repeating the entity/closure boilerplate.
    /// Used by the newly wired runtime rows (content alignment, width/height/gap modes, density,
    /// AI ring) so every one of them auto-saves and closes the menu consistently.
    fn enum_dropdown<T, F>(
        &self,
        id: impl Into<String>,
        t: &UiTheme,
        options: Vec<(String, T)>,
        current: T,
        on_select: F,
        cx: &mut Context<Self>,
    ) -> gpui::Div
    where
        T: Copy + Eq + 'static,
        F: Fn(&mut Self, T, &mut Context<Self>) + 'static,
    {
        let entity = cx.entity().downgrade();
        let callback = std::rc::Rc::new(on_select);
        let opts = options
            .into_iter()
            .map(|(label, value)| {
                let entity = entity.clone();
                let callback = callback.clone();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: current == value,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let callback = callback.clone();
                        let entity = entity.clone();
                        let _ = entity.update(app, move |this, app| {
                            this.open_dropdown = None;
                            callback(this, value, app);
                        });
                    }),
                }
            })
            .collect();
        self.make_dropdown(id, t, opts, cx)
    }

    /// Bounded integer stepper with one debounced save per click. Bounds are inclusive and
    /// the read/write pair keeps each row tied to its own `DockSettings` field.
    #[allow(clippy::too_many_arguments)]
    fn number_stepper(
        &self,
        id: &'static str,
        t: &UiTheme,
        value: i32,
        min: i32,
        max: i32,
        read: impl Fn(&DockSettings) -> i32 + 'static,
        write: impl Fn(&mut DockSettings, i32) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        self.number_stepper_with_suffix(id, t, value, "", min, max, 1, read, write, cx)
    }

    /// [`Self::number_stepper`] whose readout carries a unit suffix (`px` / `%`) and step increment.
    #[allow(clippy::too_many_arguments)]
    fn number_stepper_with_suffix(
        &self,
        id: &'static str,
        t: &UiTheme,
        value: i32,
        suffix: &str,
        min: i32,
        max: i32,
        step: i32,
        read: impl Fn(&DockSettings) -> i32 + 'static,
        write: impl Fn(&mut DockSettings, i32) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let step = step.max(1);
        let read = std::rc::Rc::new(read);
        let write = std::rc::Rc::new(write);
        let minus_read = read.clone();
        let minus_write = write.clone();
        let plus_read = read;
        let plus_write = write;
        stepper(
            SharedString::from(format!("{id}-minus")),
            SharedString::from(format!("{id}-plus")),
            t,
            SharedString::from(format!("{value}{suffix}")),
            cx.listener(move |this, _, _, cx| {
                let next = (minus_read(&this.settings) - step).clamp(min, max);
                if next != minus_read(&this.settings) {
                    minus_write(&mut this.settings, next);
                    this.auto_save(cx);
                }
            }),
            cx.listener(move |this, _, _, cx| {
                let next = (plus_read(&this.settings) + step).clamp(min, max);
                if next != plus_read(&this.settings) {
                    plus_write(&mut this.settings, next);
                    this.auto_save(cx);
                }
            }),
        )
    }

    /// Compact vertical three-dots (`⋮`) icon-only action menu (no prefix/chevron clutter).
    fn make_icon_action_menu(
        &self,
        id: impl Into<String>,
        t: &UiTheme,
        options: Vec<DropdownOption>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = id.into();
        let is_open = self.open_dropdown.as_deref() == Some(id.as_str());
        let entity = cx.entity().downgrade();
        let toggle_id = id.clone();
        icon_action_menu(id, t, is_open, options, move |_win, app| {
            let _ = entity.update(app, |this, cx| {
                if this.open_dropdown.as_deref() == Some(toggle_id.as_str()) {
                    this.open_dropdown = None;
                } else {
                    this.open_dropdown = Some(toggle_id.clone());
                }
                cx.notify();
            });
        })
    }

    /// Synchronizes the Settings window's Clear Crystal optical glass host (`topmost = false`)
    /// underneath GPUI's premultiplied DComp swapchain strictly when `settings_background == Glass`.
    fn sync_settings_glass(&mut self, window: &Window) {
        let is_dark = self.resolve_is_dark();
        let (tint_rgb, rim_rgb) = theme_surface_tint("SettingsBackground", "AccentBrush");
        let glass_cfg = SpatialGlassConfig::from_settings(
            &self.settings,
            self.settings.settings_background,
            self.settings.settings_opacity,
            is_dark,
            false,
        )
        .with_theme_tint(tint_rgb, rim_rgb);
        let hwnd = if self.last_glass_hwnd_state != Some((is_dark, glass_cfg.enabled)) {
            self.last_glass_hwnd_state = Some((is_dark, glass_cfg.enabled));
            let hwnd = configure_settings_glass_hwnd(window, is_dark, glass_cfg.enabled);
            // `configure_host_backdrop_window` just reset the DWM frame margins: re-apply the
            // rounded chrome on top (idempotent; the NC subclass is installed only once).
            window_chrome::apply_window_chrome(hwnd);
            hwnd
        } else {
            Self::window_hwnd(window)
        };
        if glass_cfg.enabled {
            if self.spatial_glass.is_none() {
                self.spatial_glass = SpatialGlassHost::attach(hwnd, false, glass_cfg);
            }
            if let Some(ref mut g) = self.spatial_glass {
                g.apply_config(glass_cfg);
                let dpi = win32::dpi_scale(hwnd);
                let vp = window.viewport_size();
                let w_px = (f32::from(vp.width) * dpi).max(2.0);
                let h_px = (f32::from(vp.height) * dpi).max(2.0);
                let radius_px =
                    (settings_corner_radius(window_chrome::is_windows_11()) * dpi).round();
                g.sync_rect(0.0, 0.0, w_px, h_px, radius_px, dpi, 0.0, 0.0);
            }
        } else {
            self.spatial_glass = None;
        }
    }

    /// Synchronizes `ThemeManager`, Clear Crystal glass tint, and persists the settings change.
    fn apply_theme_change(
        &mut self,
        window: Option<&Window>,
        immediate: bool,
        cx: &mut Context<Self>,
    ) {
        sync_from_settings(&self.settings);
        if let Some(win) = window {
            self.sync_settings_glass(win);
        }
        if immediate {
            self.save_immediately(cx);
        } else {
            self.auto_save(cx);
        }
    }

    /// Opens the shared Brush Picker modal preloaded with the live color of `token_key`.
    fn open_token_brush_picker(&mut self, token_key: &str, cx: &mut Context<Self>) {
        self.token_name = token_key.to_owned();
        let resolved = ThemeManager::resolve(token_key)
            .and_then(|val| crate::design::theme::token_rgba(&val))
            .unwrap_or_else(|| {
                let accent = crate::design::active_palette(self.is_dark()).accent;
                TokenRgba::from_rgb(accent.0, accent.1, accent.2)
            });
        self.brush_mode = BrushMode::Solid;
        self.brush_hsv = Hsv::from(resolved);
        self.brush_alpha = resolved.a;
        self.brush_hex = resolved.hex();
        self.open_brush_picker(BrushPickerTarget::ThemeToken, cx);
    }

    /// Opens the shared Figma-style Color & Gradient Brush Picker modal.
    fn open_brush_picker(&mut self, target: BrushPickerTarget, cx: &mut Context<Self>) {
        self.open_dropdown = None;
        self.brush_picker_target = target;
        match target {
            BrushPickerTarget::ThemeToken => {}
            BrushPickerTarget::BorderSurface(index) => {
                self.brush_mode = BrushMode::Solid;
                let existing = match index {
                    0 => self.settings.dock_border_color.as_deref(),
                    1 => self.settings.menu_border_color.as_deref(),
                    2 => self.settings.widget_border_color.as_deref(),
                    _ => self.settings.settings_border_color.as_deref(),
                };
                let parsed = existing
                    .and_then(crate::design::brush::parse_hex)
                    .unwrap_or_else(|| {
                        let rgb = crate::design::active_palette(self.is_dark()).border_rgb;
                        crate::design::tokens::Rgba::from_rgb(rgb.0, rgb.1, rgb.2)
                    });
                self.brush_hsv = Hsv::from(parsed);
                self.brush_alpha = parsed.a;
                self.brush_hex = parsed.hex();
            }
        }
        self.brush_picker_open = true;
        cx.notify();
    }

    fn apply_brush_picker_selection(&mut self, window: &Window, cx: &mut Context<Self>) {
        match self.brush_picker_target {
            BrushPickerTarget::BorderSurface(index) => {
                let color = self.brush_active_color();
                let hex = format!(
                    "#{:02X}{:02X}{:02X}{:02X}",
                    color.a, color.r, color.g, color.b
                );
                let slot = match index {
                    0 => &mut self.settings.dock_border_color,
                    1 => &mut self.settings.menu_border_color,
                    2 => &mut self.settings.widget_border_color,
                    _ => &mut self.settings.settings_border_color,
                };
                *slot = Some(hex);
                self.brush_picker_open = false;
                self.apply_theme_change(Some(window), false, cx);
            }
            BrushPickerTarget::ThemeToken => {
                let name = self.token_name.trim().to_owned();
                if !name.is_empty() {
                    let brush = build_brush(
                        self.brush_mode,
                        self.brush_hsv,
                        self.brush_alpha,
                        self.brush_angle,
                        &self.brush_stops,
                    );
                    let mut map = self
                        .settings
                        .extra
                        .get("ThemeTokens")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    map.insert(name.clone(), brush_to_json(&brush));
                    persist_theme_tokens(&mut self.settings, map);
                    self.token_apply_status = loc::t_fmt("BrushTokenSaved", &[&name]);
                    self.sync_settings_glass(window);
                    self.auto_save(cx);
                }
                self.brush_picker_open = false;
                cx.notify();
            }
        }
    }

    fn reset_border_picker_color(&mut self, index: usize, window: &Window, cx: &mut Context<Self>) {
        let slot = match index {
            0 => &mut self.settings.dock_border_color,
            1 => &mut self.settings.menu_border_color,
            2 => &mut self.settings.widget_border_color,
            _ => &mut self.settings.settings_border_color,
        };
        *slot = None;
        self.brush_picker_open = false;
        self.apply_theme_change(Some(window), false, cx);
    }

    fn picker_state(&self, id: &str) -> &SearchableDropdownState {
        match id {
            "weather-city" => &self.city_picker,
            "ui-font-family" => &self.font_picker,
            _ => &self.country_picker,
        }
    }

    fn picker_state_mut(&mut self, id: &str) -> &mut SearchableDropdownState {
        match id {
            "weather-city" => &mut self.city_picker,
            "ui-font-family" => &mut self.font_picker,
            _ => &mut self.country_picker,
        }
    }

    fn clear_picker_state(&mut self, id: &str) {
        self.picker_state_mut(id).clear();
    }

    /// Searchable variant of [`Self::make_dropdown`]: text edits and arrow navigation update the
    /// shared state; Enter/Escape are resolved by the component against the filtered list.
    fn make_searchable_dropdown(
        &self,
        id: &'static str,
        t: &UiTheme,
        focus: &FocusHandle,
        options: &[DropdownOption],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let entity = cx.entity().downgrade();
        let toggle_id = id.to_owned();
        let toggle_focus = focus.clone();
        let on_toggle: DropdownSelectHandler = std::rc::Rc::new(move |window, app| {
            let mut opened = false;
            let _ = entity.update(app, |this, cx| {
                opened = this.open_dropdown.as_deref() != Some(toggle_id.as_str());
                if opened {
                    this.open_dropdown = Some(toggle_id.clone());
                } else {
                    this.open_dropdown = None;
                    this.clear_picker_state(&toggle_id);
                }
                cx.notify();
            });
            if opened {
                toggle_focus.focus(window);
            }
        });

        let entity = cx.entity().downgrade();
        let key_id = id.to_owned();
        let key_options = options.to_vec();
        let on_key_down = std::rc::Rc::new(
            move |event: &KeyDownEvent, _window: &mut gpui::Window, app: &mut gpui::App| {
                let _ = entity.update(app, |this, cx| {
                    let key = event.keystroke.key.as_str();
                    match key {
                        "up" | "down" => {
                            let len = this
                                .picker_state(&key_id)
                                .filtered_indices(&key_options)
                                .len();
                            this.picker_state_mut(&key_id)
                                .move_active(if key == "up" { -1 } else { 1 }, len);
                        }
                        _ => {
                            let mut buffer = this.picker_state(&key_id).query.clone();
                            if apply_keystroke(
                                &mut buffer,
                                key,
                                event.keystroke.key_char.as_deref(),
                                false,
                            ) {
                                this.picker_state_mut(&key_id).set_query(buffer);
                            }
                        }
                    }
                    cx.notify();
                });
            },
        );

        let entity = cx.entity().downgrade();
        let close_id = id.to_owned();
        let on_close: DropdownSelectHandler = std::rc::Rc::new(move |_window, app| {
            let _ = entity.update(app, |this, cx| {
                this.open_dropdown = None;
                this.clear_picker_state(&close_id);
                cx.notify();
            });
        });

        let entity = cx.entity().downgrade();
        let active_id = id.to_owned();
        let read_active: crate::ui::components::SearchableDropdownActiveHandler =
            std::rc::Rc::new(move |app| {
                entity
                    .update(app, |this, _| this.picker_state(&active_id).active)
                    .unwrap_or(0)
            });

        searchable_dropdown(
            id,
            t,
            self.open_dropdown.as_deref() == Some(id),
            self.picker_state(id),
            focus,
            options,
            loc::t("DropdownSearchPlaceholder"),
            loc::t("DropdownNoResults"),
            SearchableDropdownHandlers {
                on_toggle,
                on_key_down,
                on_close,
                active: read_active,
            },
        )
    }

    /// Persists `"City, Country"` as soon as a picker selection changes.
    fn apply_weather_location(&mut self, cx: &mut Context<Self>) {
        let Some(city) = self.weather_cities.get(self.weather_city_index).cloned() else {
            return;
        };
        self.settings.weather_city = format!("{city}, {}", self.weather_country);
        self.auto_save(cx);
    }

    /// Fills an empty saved location from Windows geolocation. The probe runs off the UI thread;
    /// if the user types a city before it finishes, the result is discarded.
    fn auto_detect_location(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let detected = cx
                .background_executor()
                .spawn(async { country_city::geolocate() })
                .await;
            let summary = detected
                .as_ref()
                .map(|(country, city)| format!("{city}, {country}"))
                .unwrap_or_else(|| "denied or unavailable".to_owned());
            let _ = this.update(cx, |view, cx| {
                if let Some(value) =
                    location_auto_fill_value(&view.settings.weather_city, &detected)
                {
                    let (country, city) = detected.expect("auto-fill requires a fix");
                    view.weather_country = country_city::country_display_name(&country);
                    view.weather_cities = city_list(&view.weather_country);
                    view.weather_city_index = view
                        .weather_cities
                        .iter()
                        .position(|candidate| candidate.eq_ignore_ascii_case(&city))
                        .unwrap_or(0);
                    view.settings.weather_city = value.clone();
                    view.weather_auto_applied = true;
                    view.auto_save(cx);
                    opentaskbar_core::log::info(format!(
                        "Automatic weather location applied: {value}"
                    ));
                } else {
                    opentaskbar_core::log::info(format!(
                        "Automatic weather location skipped (saved city or permission): {summary}"
                    ));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_search_result(&mut self, entry: SettingsSearchEntry, cx: &mut Context<Self>) {
        self.clear_search();
        self.open_dropdown = None;
        self.page = entry.page;
        if let Some(filter) = entry.widget_filter {
            self.widget_filter = filter;
        }
        if let Some(expand) = entry.expand_widget {
            self.widget_filter = "All";
            self.expanded_widget = Some(expand);
        }
        self.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
        // A fresh anchor is attached to the target row on the next render. Prepaint records
        // that row's origin; the scroll scheduled in `render` then aligns it with the top of
        // the viewport. This works for nested rows, unlike index-based scroll APIs.
        self.highlight_row = Some(entry.row_id.clone());
        self.scroll_anchor = Some(ScrollAnchor::for_handle(self.scroll_handle.clone()));
        cx.notify();

        // Give GPUI a frame to lay out the new page/filter, force the frame that applies the
        // anchor offset, then let the highlight fade so it never sticks around forever.
        let target = entry.row_id;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            // `spawn` hands us an app context; refresh_windows schedules the frame that
            // runs the anchor callback registered during render.
            let _ = cx.update(|app| app.refresh_windows());
            cx.background_executor()
                .timer(Duration::from_millis(1800))
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.highlight_row.as_deref() == Some(target.as_str()) {
                    view.highlight_row = None;
                    view.scroll_anchor = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_search_results(
        &self,
        query: &str,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let catalog = settings_search::catalog();
        let matches = settings_search::search(&catalog, query);
        if matches.is_empty() {
            return vec![
                div()
                    .w_full()
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(rgba(t.border))
                    .bg(rgba(t.surface))
                    .px(px(18.0))
                    .py(px(22.0))
                    .text_size(px(fonts::scaled_size(13.0)))
                    .text_color(rgba(t.text_secondary))
                    .child(loc::t("SettingsSearchEmpty"))
                    .into_any_element(),
            ];
        }

        let mut blocks: Vec<AnyElement> = vec![
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(13.0)))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(loc::t("SettingsSearchResults")),
                )
                .child(badge(t, matches.len().to_string(), true))
                .into_any_element(),
        ];

        let hover_bg = t.surface_hover;
        let hover_border = t.accent;
        for (index, entry) in matches.into_iter().enumerate() {
            let target = entry.clone();
            let title = entry.title.clone();
            let location = format!("{} · {}", entry.page.title(), entry.section);
            blocks.push(
                div()
                    .id(SharedString::from(format!("search-result-{index}")))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(14.0))
                    .py(px(11.0))
                    .rounded(px(10.0))
                    .bg(rgba(t.surface))
                    .border_1()
                    .border_color(rgba(t.border))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgba(hover_bg)).border_color(rgba(hover_border)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_search_result(target.clone(), cx);
                    }))
                    .child(icon_glyph("\u{E721}", 13.0, t.accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(13.0)))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(rgba(t.text_primary))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .text_color(rgba(t.text_secondary))
                                    .child(location),
                            ),
                    )
                    .child(icon_glyph("\u{E76B}", 12.0, t.text_secondary))
                    .into_any_element(),
            );
        }
        blocks
    }

    /// Resolved once per render; `AppTheme::System` reads the registry, so widget preview cards
    /// must reuse this value instead of re-reading it for every card.
    fn resolve_is_dark(&self) -> bool {
        match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        }
    }

    /// `WidgetPanelHost`: Escape/focus return plus 300 ms suppress after close.
    fn close_widget_panel(&mut self, window: &mut Window) {
        if self.widget_panel.take().is_some() {
            self.panel_opened_at = Some(Instant::now());
            self.focus.focus(window);
        }
    }

    fn auto_save(&mut self, cx: &mut Context<Self>) {
        self.widget_runtime.tick(&self.settings);
        self.save_pending = true;
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            // True trailing-edge debounce: only the final generation after a 250 ms quiet window flushes.
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.save_pending && view.save_generation == generation {
                    view.save_pending = false;
                    view.sync_dock_pins();
                    view.save_settings(cx);
                }
            });
        })
        .detach();
    }

    fn save_immediately(&mut self, cx: &mut Context<Self>) {
        self.widget_runtime.tick(&self.settings);
        self.save_pending = false;
        self.save_generation = self.save_generation.wrapping_add(1);
        self.sync_dock_pins();
        self.save_settings(cx);
    }

    // ---- WidgetPanelHost -----------------------------------------------------

    fn preview_flyout(&self, id: &str, owner_hwnd: usize) {
        if !self.flyout_preview {
            return;
        }
        use crate::ui::flyout::{FlyoutBlock, FlyoutSpec, show_flyout};
        let is_dark = self.resolve_is_dark();
        let (anchor_x, anchor_y) =
            window_chrome::window_flyout_anchor(owner_hwnd).unwrap_or((0, 0));
        let spec = FlyoutSpec {
            title: loc::t_fmt("FlyoutDemoTitle", &[id]),
            subtitle: Some(loc::t("FlyoutDemoSubtitle")),
            width_dips: 320.0,
            blocks: vec![
                FlyoutBlock::Hero {
                    value: loc::t("UiFontPreview"),
                    caption: Some(loc::t("FlyoutDemoCaption")),
                    centered: false,
                    large: true,
                    right_meta: None,
                },
                FlyoutBlock::SeekSlider {
                    id: 1,
                    label: loc::t("Position"),
                    position_secs: 42,
                    duration_secs: 180,
                    ratio: 42.0 / 180.0,
                },
                FlyoutBlock::Chart {
                    label: loc::t("FlyoutDemoTrend"),
                    series: vec![(loc::t("FlyoutDemoLive"), vec![0.2, 0.5, 0.35, 0.8, 0.6])],
                    accent: (0x2d, 0xd4, 0xbf),
                },
                FlyoutBlock::IconBar {
                    buttons: vec![(2, loc::t("Reset"), 0xE72C, false)],
                },
            ],
        };
        // Own modal message loop: run it off the GPUI thread to avoid nesting loops.
        std::thread::spawn(move || {
            let _ = show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec);
        });
    }

    fn preview_widget_flyout(&mut self, id: &str, owner_hwnd: usize) {
        let is_dark = self.resolve_is_dark();
        let (anchor_x, anchor_y) =
            window_chrome::window_flyout_anchor(owner_hwnd).unwrap_or((0, 0));
        self.widget_runtime.tick(&self.settings);
        let spec = self.widget_runtime.live_spec(id, &self.settings, is_dark);
        std::thread::spawn(move || {
            let _ = crate::ui::flyout::show_flyout(owner_hwnd, is_dark, anchor_x, anchor_y, spec);
        });
    }

    fn render_widget_panel_overlay(
        &self,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let id = self.widget_panel?;
        let accent = t.accent;
        let hover = t.surface_hover;
        Some(
            div()
                .id("widget-panel-host")
                .absolute()
                .inset_0()
                // Keep the scrim inside the rounded window outline.
                .rounded(px(settings_corner_radius(window_chrome::is_windows_11())))
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x9900_0000))
                .child(
                    div()
                        .w(px(460.0))
                        .max_w_full()
                        .p(px(18.0))
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .rounded(px(14.0))
                        .bg(rgba(t.surface))
                        .border_1()
                        .border_color(rgba(accent))
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(16.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(widgets::widget_name(id)),
                                )
                                .child(button(
                                    "btn-panel-close",
                                    t,
                                    loc::t("Close"),
                                    ButtonVariant::Secondary,
                                    cx.listener(|this, _, window, cx| {
                                        this.close_widget_panel(window);
                                        cx.notify();
                                    }),
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(fonts::scaled_size(12.0)))
                                .text_color(rgba(t.text_secondary))
                                .child(loc::t("WidgetPanelHint")),
                        )
                        .child(
                            div()
                                .id("panel-hover-region")
                                .w_full()
                                .h(px(64.0))
                                .rounded(px(10.0))
                                .bg(rgba(hover))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, window, _| {
                                    this.preview_flyout(
                                        this.widget_panel.unwrap_or("widget"),
                                        Self::window_hwnd(window),
                                    );
                                }))
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(12.0)))
                                        .text_color(rgba(t.text_primary))
                                        .child(loc::t("WidgetPreviewFlyout")),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    // ---- Settings feature panels --------------------------------------------

    /// Refreshes the city list from `country_city::fetch_cities` off the UI thread.
    /// The offline catalogue is shown immediately; the live list replaces it on arrival.
    fn refresh_weather_cities(&mut self, cx: &mut Context<Self>) {
        let country = self.weather_country.clone();
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move { country_city::fetch_cities(&country) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if !fetched.is_empty() {
                    let selected = view.weather_cities.get(view.weather_city_index).cloned();
                    view.weather_cities = fetched;
                    view.weather_city_index = selected
                        .and_then(|name| {
                            view.weather_cities
                                .iter()
                                .position(|c| c.eq_ignore_ascii_case(&name))
                        })
                        .unwrap_or(0);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_weather_picker(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let cities = &self.weather_cities;
        let city = cities
            .get(self.weather_city_index)
            .map(String::as_str)
            .unwrap_or("Istanbul");
        let mut label = format!("{city}, {}", self.weather_country);
        if self.weather_auto_applied {
            label.push_str(" · ");
            label.push_str(&loc::t("WeatherLocationUsed"));
        }

        let country_opts: Vec<DropdownOption> = country_city::countries()
            .into_iter()
            .map(|(code, name)| {
                let entity = cx.entity().downgrade();
                let flag_name = name.clone();
                let country_name = name.clone();
                DropdownOption {
                    label: SharedString::from(name),
                    flag_path: country_flag_path(&flag_name).or_else(|| country_flag_path(&code)),
                    glyph: None,
                    selected: self.weather_country.eq_ignore_ascii_case(&country_name),
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.weather_country = country_name.clone();
                            this.weather_cities = city_list(&this.weather_country);
                            this.weather_city_index = 0;
                            this.weather_auto_applied = false;
                            this.apply_weather_location(cx);
                            this.refresh_weather_cities(cx);
                            cx.notify();
                        });
                    }),
                }
            })
            .collect();

        let city_opts: Vec<DropdownOption> = cities
            .iter()
            .enumerate()
            .map(|(idx, c)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(c.clone()),
                    flag_path: None,
                    glyph: None,
                    selected: self.weather_city_index == idx,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.weather_city_index = idx;
                            this.weather_auto_applied = false;
                            this.apply_weather_location(cx);
                            cx.notify();
                        });
                    }),
                }
            })
            .collect();

        let unit_opts: Vec<DropdownOption> = [
            (TemperatureUnit::Celsius, "°C"),
            (TemperatureUnit::Fahrenheit, "°F"),
        ]
        .into_iter()
        .map(|(unit, lbl)| {
            let entity = cx.entity().downgrade();
            DropdownOption {
                label: SharedString::from(lbl),
                flag_path: None,
                glyph: None,
                selected: self.settings.weather_unit == unit,
                on_select: std::rc::Rc::new(move |_win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        this.settings.weather_unit = unit;
                        this.auto_save(cx);
                    });
                }),
            }
        })
        .collect();

        vec![
            setting_row(
                t,
                Some("\u{E774}"),
                loc::t("WeatherCountry"),
                self.make_searchable_dropdown(
                    "weather-country",
                    t,
                    &self.country_picker_focus,
                    &country_opts,
                    cx,
                ),
            )
            .into_any_element(),
            setting_row(
                t,
                Some("\u{E707}"),
                loc::t("WeatherCityProvince"),
                self.make_searchable_dropdown(
                    "weather-city",
                    t,
                    &self.city_picker_focus,
                    &city_opts,
                    cx,
                ),
            )
            .into_any_element(),
            setting_row(
                t,
                Some("\u{E896}"),
                loc::t("WeatherRefreshCities"),
                button(
                    "btn-weather-fetch-cities",
                    t,
                    loc::t("WeatherFetchFromApi"),
                    ButtonVariant::Secondary,
                    cx.listener(|this, _, _, cx| this.refresh_weather_cities(cx)),
                ),
            )
            .into_any_element(),
            self.target_row(
                "WeatherUnit",
                t,
                Some("\u{E9CA}"),
                loc::t("WeatherUnit"),
                self.make_dropdown("weather-unit", t, unit_opts, cx),
            ),
            setting_row_desc(
                t,
                Some("\u{E81D}"),
                loc::t("WeatherLocation"),
                label,
                button(
                    "btn-weather-apply",
                    t,
                    loc::t("Apply"),
                    ButtonVariant::Primary,
                    cx.listener(|this, _, _, cx| {
                        this.weather_auto_applied = false;
                        this.apply_weather_location(cx);
                    }),
                ),
            )
            .into_any_element(),
        ]
    }

    fn render_ai_auth(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // The badge reflects the encrypted token file, not a stale settings slot.
        let signed_in = cursor_auth::has_session();
        let rows = vec![
            setting_row(
                t,
                Some("\u{E99A}"),
                loc::t("AiUsageCursorAccountLabel"),
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(badge(t, self.ai_status.clone(), signed_in))
                    .child(button(
                        "btn-cursor-auth",
                        t,
                        if signed_in {
                            loc::t("AiUsageCursorSignOut")
                        } else {
                            loc::t("AiUsageCursorSignIn")
                        },
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            if cursor_auth::has_session() {
                                let _ = cursor_auth::sign_out();
                                this.settings.extra.remove("CursorAuthToken");
                                this.ai_status = loc::t("AiCursorNotSignedIn");
                                this.save_settings(cx);
                                return;
                            }
                            let start = cursor_auth::sign_in_start();
                            pins::launch(start.login_url);
                            this.ai_status = loc::t("AiUsageCursorWaiting");
                            cx.notify();
                            // `sign_in_start` stores the pending PKCE challenge;
                            // `sign_in_finish` polls it on the background executor.
                            cx.spawn(async move |this, cx| {
                                let ok = cx
                                    .background_executor()
                                    .spawn(async { cursor_auth::sign_in_finish() })
                                    .await;
                                let _ = this.update(cx, |view, cx| {
                                    if ok && cursor_auth::has_session() {
                                        view.settings.extra.remove("CursorAuthToken");
                                        view.ai_status = loc::t("AiCursorSignedIn");
                                        view.save_settings(cx);
                                    } else {
                                        view.ai_status = loc::t("AiUsageCursorSignInTimedOut");
                                        cx.notify();
                                    }
                                });
                            })
                            .detach();
                        }),
                    ))
                    .child(button(
                        "btn-cursor-status-refresh",
                        t,
                        loc::t("AiUsageRefresh"),
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            this.ai_status = if cursor_auth::has_session() {
                                loc::t("AiCursorSignedIn")
                            } else {
                                loc::t("AiCursorNotSignedIn")
                            };
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element(),
        ];
        // Per-provider CLI account rows were removed; only the Cursor PKCE row stays.
        rows
    }

    fn render_backup_ui(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let status = if self.backup_status.is_empty() {
            loc::t("BackupNoOperation")
        } else {
            self.backup_status.clone()
        };
        let mut rows: Vec<AnyElement> = vec![
            setting_row(
                t,
                Some("\u{EDE1}"),
                loc::t("FullBackupArchive"),
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(button(
                        "btn-backup-export",
                        t,
                        loc::t("ThemePackExport"),
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            this.sync_dock_pins();
                            let data = build_backup(&this.settings);
                            if let Some(path) = prompt::save_file(
                                &loc::t("FullBackupExport"),
                                "opentaskbar-backup.otbackup",
                                &loc::t("BackupArchiveName"),
                                "*.otbackup",
                            ) {
                                match std::fs::write(&path, data) {
                                    Ok(()) => {
                                        this.backup_status = loc::t_fmt(
                                            "BackupExported",
                                            &[path.to_string_lossy().as_ref()],
                                        );
                                    }
                                    Err(err) => {
                                        this.backup_status =
                                            loc::t_fmt("BackupExportFailed", &[&err.to_string()]);
                                    }
                                }
                            }
                            cx.notify();
                        }),
                    ))
                    .child(button(
                        "btn-backup-inspect",
                        t,
                        loc::t("FullBackupInspect"),
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            if let Some(path) = prompt::open_file(
                                &loc::t("FullBackupInspect"),
                                &loc::t("BackupArchiveName"),
                                "*.otbackup",
                            ) {
                                match std::fs::read(&path) {
                                    Ok(data) => {
                                        let preview = inspect_backup(&data);
                                        this.backup_status = if preview.valid {
                                            loc::t_fmt(
                                                "BackupValid",
                                                &[path.to_string_lossy().as_ref()],
                                            )
                                        } else {
                                            format!(
                                                "{}: {}",
                                                path.to_string_lossy(),
                                                preview.error.clone().unwrap_or_default()
                                            )
                                        };
                                        this.backup_preview = Some(preview);
                                    }
                                    Err(err) => {
                                        this.backup_status =
                                            loc::t_fmt("BackupReadFailed", &[&err.to_string()]);
                                    }
                                }
                            }
                            cx.notify();
                        }),
                    ))
                    .child(button(
                        "btn-backup-import",
                        t,
                        loc::t("ThemePackImport"),
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, window, cx| {
                            if let Some(path) = prompt::open_file(
                                &loc::t("FullBackupImport"),
                                &loc::t("BackupArchiveName"),
                                "*.otbackup",
                            ) {
                                match std::fs::read(&path)
                                    .map_err(|e| e.to_string())
                                    .and_then(|data| import_backup(&data))
                                {
                                    Ok(settings) => {
                                        this.settings = settings;
                                        fonts::apply_settings(&this.settings);
                                        loc::set_language(&this.settings.language);
                                        sync_from_settings(&this.settings);
                                        this.sync_settings_glass(window);
                                        this.backup_status = loc::t_fmt(
                                            "BackupImported",
                                            &[path.to_string_lossy().as_ref()],
                                        );
                                        this.save_immediately(cx);
                                    }
                                    Err(err) => {
                                        this.backup_status =
                                            loc::t_fmt("BackupImportFailed", &[&err.to_string()]);
                                    }
                                }
                            }
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element(),
            div()
                .w_full()
                .px(px(16.0))
                .pb(px(8.0))
                .text_size(px(11.0))
                .text_color(rgba(t.text_secondary))
                .child(status)
                .into_any_element(),
        ];
        if let Some(preview) = &self.backup_preview
            && preview.valid
        {
            rows.push(
                div()
                    .w_full()
                    .px(px(16.0))
                    .pb(px(8.0))
                    .text_size(px(11.0))
                    .text_color(rgba(t.text_primary))
                    .child(loc::t_fmt(
                        "BackupSummary",
                        &[
                            &preview.pinned_apps.to_string(),
                            &preview.theme_files.to_string(),
                            preview
                                .settings
                                .as_ref()
                                .map(|s| s.weather_city.as_str())
                                .unwrap_or("—"),
                        ],
                    ))
                    .into_any_element(),
            );
        }
        rows
    }

    fn apply_theme_preset(&mut self, preset_id: &str, window: &Window, cx: &mut Context<Self>) {
        match preset_id {
            "liquid_glass" => {
                self.settings.dock_background = SurfaceBackground::Glass;
                self.settings.menu_background = SurfaceBackground::Glass;
                self.settings.widget_background = SurfaceBackground::Glass;
                self.settings.settings_background = SurfaceBackground::Glass;
                self.settings.dock_opacity = 0.90;
                self.settings.menu_opacity = 0.90;
                self.settings.widget_opacity = 0.90;
                self.settings.settings_opacity = 0.90;
                self.settings.glass_blur_sigma = 9.0;
                self.settings.glass_saturation = 1.32;
                self.settings.glass_edge_glow = 0.88;
                self.settings.glass_pointer_light = true;
                self.settings.oled_reduce_glow_enabled = false;
            }
            "frosted_acrylic" => {
                self.settings.dock_background = SurfaceBackground::Glass;
                self.settings.menu_background = SurfaceBackground::Glass;
                self.settings.widget_background = SurfaceBackground::Glass;
                self.settings.settings_background = SurfaceBackground::Glass;
                self.settings.dock_opacity = 0.85;
                self.settings.menu_opacity = 0.85;
                self.settings.widget_opacity = 0.85;
                self.settings.settings_opacity = 0.85;
                self.settings.glass_blur_sigma = 24.0;
                self.settings.glass_saturation = 1.45;
                self.settings.glass_edge_glow = 0.48;
                self.settings.glass_pointer_light = true;
                self.settings.oled_reduce_glow_enabled = false;
            }
            "dark_mica" => {
                self.settings.dock_background = SurfaceBackground::Glass;
                self.settings.menu_background = SurfaceBackground::Glass;
                self.settings.widget_background = SurfaceBackground::Glass;
                self.settings.settings_background = SurfaceBackground::Glass;
                self.settings.dock_opacity = 0.95;
                self.settings.menu_opacity = 0.95;
                self.settings.widget_opacity = 0.95;
                self.settings.settings_opacity = 0.95;
                self.settings.glass_blur_sigma = 32.0;
                self.settings.glass_saturation = 1.12;
                self.settings.glass_edge_glow = 0.22;
                self.settings.glass_pointer_light = false;
                self.settings.oled_reduce_glow_enabled = false;
            }
            "oled_black" => {
                self.settings.dock_background = SurfaceBackground::TrueBlack;
                self.settings.menu_background = SurfaceBackground::TrueBlack;
                self.settings.widget_background = SurfaceBackground::TrueBlack;
                self.settings.settings_background = SurfaceBackground::TrueBlack;
                self.settings.dock_opacity = 1.0;
                self.settings.menu_opacity = 1.0;
                self.settings.widget_opacity = 1.0;
                self.settings.settings_opacity = 1.0;
                self.settings.oled_reduce_glow_enabled = true;
                self.settings.glass_edge_glow = 0.0;
                self.settings.glass_pointer_light = false;
            }
            "solid_flat" => {
                self.settings.dock_background = SurfaceBackground::Solid;
                self.settings.menu_background = SurfaceBackground::Solid;
                self.settings.widget_background = SurfaceBackground::Solid;
                self.settings.settings_background = SurfaceBackground::Solid;
                self.settings.dock_opacity = 1.0;
                self.settings.menu_opacity = 1.0;
                self.settings.widget_opacity = 1.0;
                self.settings.settings_opacity = 1.0;
                self.settings.dock_border_thickness = 1.0;
                self.settings.menu_border_thickness = 1.0;
                self.settings.widget_border_thickness = 1.0;
                self.settings.settings_border_thickness = 1.0;
                self.settings.oled_reduce_glow_enabled = false;
            }
            "minimal" => {
                self.settings.dock_background = SurfaceBackground::None;
                self.settings.widget_background = SurfaceBackground::None;
                self.settings.dock_opacity = 0.25;
                self.settings.widget_opacity = 0.25;
                self.settings.dock_border_thickness = 0.0;
                self.settings.widget_border_thickness = 0.0;
                self.settings.oled_reduce_glow_enabled = false;
            }
            "vibrant" => {
                self.settings.dock_background = SurfaceBackground::Glass;
                self.settings.menu_background = SurfaceBackground::Glass;
                self.settings.widget_background = SurfaceBackground::Glass;
                self.settings.settings_background = SurfaceBackground::Glass;
                self.settings.dock_opacity = 0.82;
                self.settings.menu_opacity = 0.82;
                self.settings.widget_opacity = 0.82;
                self.settings.settings_opacity = 0.82;
                self.settings.glass_blur_sigma = 14.0;
                self.settings.glass_saturation = 1.72;
                self.settings.glass_edge_glow = 1.0;
                self.settings.glass_pointer_light = true;
                self.settings.oled_reduce_glow_enabled = false;
            }
            "high_contrast" => {
                self.settings.dock_background = SurfaceBackground::Solid;
                self.settings.menu_background = SurfaceBackground::Solid;
                self.settings.widget_background = SurfaceBackground::Solid;
                self.settings.settings_background = SurfaceBackground::Solid;
                self.settings.dock_opacity = 1.0;
                self.settings.menu_opacity = 1.0;
                self.settings.widget_opacity = 1.0;
                self.settings.settings_opacity = 1.0;
                self.settings.dock_border_thickness = 2.0;
                self.settings.menu_border_thickness = 2.0;
                self.settings.widget_border_thickness = 2.0;
                self.settings.settings_border_thickness = 2.0;
                self.settings.oled_reduce_glow_enabled = false;
                let mut map = self
                    .settings
                    .extra
                    .get("ThemeTokens")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let is_dark = self.is_dark();
                map.insert(
                    "TextPrimary".to_owned(),
                    json!(if is_dark { "#FFFFFFFF" } else { "#FF000000" }),
                );
                map.insert(
                    "StrokeFocus".to_owned(),
                    json!(if is_dark { "#FFFFFF00" } else { "#FF0000CC" }),
                );
                map.insert(
                    "DockBorderMaterial".to_owned(),
                    json!(if is_dark { "#FFFFFFFF" } else { "#FF000000" }),
                );
                persist_theme_tokens(&mut self.settings, map);
            }
            _ => return,
        }
        self.apply_theme_change(Some(window), false, cx);
    }

    fn apply_accent_swatch(&mut self, hex: &str, window: &Window, cx: &mut Context<Self>) {
        let mut map = self
            .settings
            .extra
            .get("ThemeTokens")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for key in ["AccentBrush", "WidgetAccent", "StrokeFocus"] {
            map.insert(key.to_owned(), json!(hex));
        }
        persist_theme_tokens(&mut self.settings, map);
        self.sync_settings_glass(window);
        self.auto_save(cx);
    }

    fn save_studio_custom_theme(
        &mut self,
        update_existing: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let active_id = self.settings.theme_pack.trim().to_owned();
        let target_id =
            if update_existing && !active_id.is_empty() && !theme_packs::is_built_in(&active_id) {
                Some(active_id.as_str())
            } else {
                None
            };
        let raw_name = self.custom_theme_name.trim().to_owned();
        let display_name = if !raw_name.is_empty() {
            raw_name
        } else if let Some(id) = target_id {
            theme_packs::ThemePack::load(&Self::themes_root().join(id))
                .map(|p| p.display_name().to_owned())
                .unwrap_or_else(|_| id.to_owned())
        } else {
            loc::t("ThemePackNewDefault")
        };

        let mut widgets = if !active_id.is_empty() {
            theme_packs::ThemePack::load(&Self::themes_root().join(&active_id))
                .ok()
                .and_then(|pack| pack.widgets)
                .unwrap_or_default()
        } else {
            WidgetPackManifest::default()
        };
        widgets.name = Some(format!(
            "{} {}",
            display_name,
            loc::t("ThemeWidgetPackSuffix")
        ));

        let is_dark = self.is_dark();
        let theme_layer = if is_dark {
            &mut widgets.dark
        } else {
            &mut widgets.light
        };
        if let Some(overrides) = self
            .settings
            .extra
            .get("ThemeTokens")
            .and_then(Value::as_object)
        {
            for (k, v) in overrides {
                theme_layer.insert(k.clone(), v.clone());
            }
        }

        let manifest = ThemePackManifest {
            name: Some(display_name.clone()),
            author: None,
            description: None,
            preserve_original_icons: true,
        };
        match theme_packs::save_custom_theme(&Self::themes_root(), target_id, &manifest, &widgets) {
            Ok(saved) => {
                self.settings.theme_pack = saved.id.clone();
                self.custom_theme_name = saved.display_name().to_owned();
                let len = self.custom_theme_name.encode_utf16().count();
                self.custom_theme_name_selection = len..len;
                persist_theme_tokens(&mut self.settings, Map::new());
                self.theme_status = loc::t("ThemeSavedMessage");
                self.apply_theme_change(Some(window), true, cx);
            }
            Err(err) => {
                self.theme_status = loc::t_fmt("ThemeSaveFailed", &[&err]);
                cx.notify();
            }
        }
    }

    fn render_theme_studio_preview(&self, t: &UiTheme, is_dark: bool) -> AnyElement {
        let pal = crate::design::active_palette(is_dark);
        let backdrop_top = if is_dark { 0x1018_28ff } else { 0xdbeafeff };
        let backdrop_bot = if is_dark { 0x1e29_3bff } else { 0xe0e7_ffff };
        let dock_alpha: u8 = match self.settings.dock_background {
            SurfaceBackground::None => 0x18,
            SurfaceBackground::TrueBlack | SurfaceBackground::Solid => 0xff,
            SurfaceBackground::Glass => {
                ((self.settings.dock_opacity.clamp(0.2, 1.0) * 215.0).round() as u8).max(0x38)
            }
        };
        let dock_bg = match self.settings.dock_background {
            SurfaceBackground::TrueBlack => 0x0000_00ff,
            _ => crate::design::gpui_rgba(pal.dock_pill, dock_alpha),
        };
        let dock_border = self
            .settings
            .dock_border_color
            .as_deref()
            .and_then(parse_hex_color)
            .unwrap_or_else(|| crate::design::gpui_rgba(pal.border_rgb, 0xcc));
        let dock_radius = self.settings.dock_corner_radius.unwrap_or(14).clamp(0, 22) as f32;
        let dock_border_w = self.settings.dock_border_thickness.clamp(0.0, 3.0) as f32;

        div()
            .w_full()
            .h(px(120.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgba(t.border))
            .bg(rgba(backdrop_top))
            .overflow_hidden()
            .relative()
            .flex()
            .flex_col()
            .justify_between()
            .p(px(12.0))
            .child(
                // Mini Settings Card preview
                div()
                    .w(px(230.0))
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded(px(9.0))
                    .bg(rgba(t.surface))
                    .border_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.text_primary))
                                    .child(loc::t("SettingsTitle")),
                            )
                            .child(
                                div()
                                    .text_size(px(9.0))
                                    .text_color(rgba(t.text_secondary))
                                    .child(loc::t("ThemeStudioSubtitle")),
                            ),
                    )
                    .child(
                        div()
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(5.0))
                            .bg(rgba(t.accent))
                            .text_size(px(9.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.accent_fg))
                            .child(loc::t("Apply")),
                    ),
            )
            .child(
                // Mini Dock pill + widget chip preview
                div().w_full().flex().justify_center().child(
                    div()
                        .h(px(34.0))
                        .px(px(10.0))
                        .rounded(px(dock_radius))
                        .bg(rgba(dock_bg))
                        .when(dock_border_w > 0.0, |pill| {
                            pill.border(px(dock_border_w))
                                .border_color(rgba(dock_border))
                        })
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(div().size(px(16.0)).rounded(px(4.0)).bg(rgba(t.accent)))
                        .child(div().size(px(16.0)).rounded(px(4.0)).bg(rgba(backdrop_bot)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .size(px(16.0))
                                        .rounded(px(4.0))
                                        .bg(rgba(t.surface_muted)),
                                )
                                .child(
                                    div()
                                        .w(px(8.0))
                                        .h(px(2.0))
                                        .rounded(px(1.0))
                                        .bg(rgba(t.accent)),
                                ),
                        )
                        .child(div().w(px(1.0)).h(px(16.0)).bg(rgba(t.separator)))
                        .child(
                            div()
                                .px(px(7.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .bg(rgba(t.surface))
                                .border_1()
                                .border_color(rgba(t.border))
                                .text_size(px(9.5))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgba(t.text_primary))
                                .child("14:32 · 21°C"),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_theme_studio(
        &self,
        t: &UiTheme,
        is_dark: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body: Vec<AnyElement> = Vec::new();
        body.push(
            self.mark_target(
                "ThemeStudio",
                t,
                div()
                    .w_full()
                    .px(px(16.0))
                    .pt(px(12.0))
                    .pb(px(10.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .text_color(rgba(t.text_secondary))
                            .child(loc::t("ThemeStudioSubtitle")),
                    )
                    .child(self.render_theme_studio_preview(t, is_dark)),
            )
            .into_any_element(),
        );

        // 8 One-Click Material & Look Presets
        let presets: [(&str, &str, &str); 8] = [
            ("liquid_glass", "ThemePresetLiquidGlass", "\u{EB42}"),
            ("frosted_acrylic", "ThemePresetFrostedAcrylic", "\u{E706}"),
            ("dark_mica", "ThemePresetDarkMica", "\u{E708}"),
            ("oled_black", "ThemePresetOledBlack", "\u{E793}"),
            ("solid_flat", "ThemePresetSolidFlat", "\u{E81E}"),
            ("minimal", "ThemePresetMinimal", "\u{E738}"),
            ("vibrant", "ThemePresetVibrant", "\u{E790}"),
            ("high_contrast", "ThemePresetHighContrast", "\u{E7A1}"),
        ];
        let mut preset_wrap = div().w_full().flex().flex_wrap().gap(px(6.0));
        for (preset_id, label_key, glyph) in presets {
            let pid: &'static str = preset_id;
            let hover_bg = t.surface_hover;
            let hover_bd = t.accent;
            preset_wrap = preset_wrap.child(
                div()
                    .id(SharedString::from(format!("theme-preset-{preset_id}")))
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .bg(rgba(t.surface_muted))
                    .border_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgba(hover_bg)).border_color(rgba(hover_bd)))
                    .active(|s| s.opacity(0.86))
                    .on_click(cx.listener(move |this, _, win, cx| {
                        this.apply_theme_preset(pid, win, cx);
                    }))
                    .child(icon_glyph(glyph, 11.5, t.accent))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(11.5)))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(rgba(t.text_primary))
                            .child(loc::t(label_key)),
                    ),
            );
        }
        body.push(
            div()
                .w_full()
                .px(px(16.0))
                .py(px(10.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgba(t.text_secondary))
                        .child(loc::t("ThemeMaterialPresets")),
                )
                .child(preset_wrap)
                .into_any_element(),
        );

        // 10 Curated Accent Swatch Pills
        let mono_hex = if is_dark { "#FFE4E4E7" } else { "#FF27272A" };
        let accents: [(&str, &str); 10] = [
            ("aurora", "#FF38BDF8"),
            ("mauve", "#FFCBA6F7"),
            ("nord", "#FF88C0D0"),
            ("gruvbox", "#FFFABD2F"),
            ("solarized", "#FF2AA198"),
            ("tokyo", "#FF7AA2F7"),
            ("emerald", "#FF10B981"),
            ("rose", "#FFF43F5E"),
            ("coral", "#FFF97316"),
            ("mono", mono_hex),
        ];
        let mut swatch_bar = div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(7.0));
        for (slug, hex) in accents {
            let color_u32 = parse_hex_color(hex).unwrap_or(t.accent);
            let hex_owned = hex.to_owned();
            swatch_bar = swatch_bar.child(
                div()
                    .id(SharedString::from(format!("studio-accent-{slug}")))
                    .size(px(24.0))
                    .rounded(px(12.0))
                    .bg(rgba(color_u32))
                    .border_2()
                    .border_color(rgba(t.border))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.88))
                    .on_click(cx.listener(move |this, _, win, cx| {
                        this.apply_accent_swatch(&hex_owned, win, cx);
                    })),
            );
        }
        body.push(
            setting_row(t, Some("\u{E790}"), loc::t("ColorSwatchAccent"), swatch_bar)
                .into_any_element(),
        );

        // 8 Direct Visual Color Picker Swatch Triggers (2-column grid)
        let quick_tokens: [(&str, &str); 8] = [
            ("DockMaterial", "ThemeFieldDockMaterial"),
            ("AccentBrush", "ThemeFieldAccentBrush"),
            ("TextPrimary", "ThemeFieldTextPrimary"),
            ("TextSecondary", "ThemeFieldTextSecondary"),
            ("DockBorderMaterial", "ThemeFieldDockBorderMaterial"),
            ("WidgetCard", "ThemeFieldWidgetCard"),
            ("MenuMaterial", "ThemeFieldMenuMaterial"),
            ("SettingsBackground", "ThemeFieldSettingsBackground"),
        ];
        let mut grid = div().w_full().flex().flex_col().gap(px(8.0));
        for pair in quick_tokens.chunks(2) {
            let mut row = div().w_full().flex().gap(px(8.0));
            for &(token_key, label_key) in pair {
                let resolved = ThemeManager::resolve(token_key)
                    .and_then(|v| crate::design::theme::token_rgba(&v))
                    .unwrap_or_else(|| TokenRgba::from_rgb(0x38, 0xbd, 0xf8));
                let hover_bg = t.surface_hover;
                let hover_bd = t.accent;
                row = row.child(
                    div()
                        .id(SharedString::from(format!("studio-pick-{token_key}")))
                        .flex_1()
                        .min_w_0()
                        .px(px(10.0))
                        .py(px(7.0))
                        .rounded(px(8.0))
                        .bg(rgba(t.surface_muted))
                        .border_1()
                        .border_color(rgba(t.border))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgba(hover_bg)).border_color(rgba(hover_bd)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_token_brush_picker(token_key, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .min_w_0()
                                .child(
                                    div()
                                        .size(px(18.0))
                                        .rounded(px(5.0))
                                        .flex_shrink_0()
                                        .border_1()
                                        .border_color(rgba(t.border))
                                        .bg(rgba(resolved.to_gpui())),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .text_size(px(fonts::scaled_size(11.5)))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(rgba(t.text_primary))
                                        .child(loc::t(label_key)),
                                ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(fonts::scaled_size(10.5)))
                                .text_color(rgba(t.text_secondary))
                                .child(resolved.hex()),
                        ),
                );
            }
            grid = grid.child(row);
        }
        body.push(
            div()
                .w_full()
                .px(px(16.0))
                .py(px(10.0))
                .child(grid)
                .into_any_element(),
        );

        // Custom Theme Pack Saver Bar (native EntityInputHandler text field + Save / Update / Reset)
        let name_focused = self.custom_theme_name_focus.is_focused(window);
        let name_caret = Self::byte_index_for_utf16(
            &self.custom_theme_name,
            self.custom_theme_name_selection.start,
        );
        let (name_before, name_after) = self.custom_theme_name.split_at(name_caret);
        let name_display = if self.custom_theme_name.is_empty() {
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(fonts::scaled_size(12.0)))
                .text_color(rgba(t.text_secondary))
                .child(loc::t("ThemePackNewDefault"))
        } else {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .overflow_hidden()
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_primary))
                        .child(name_before.to_owned()),
                )
                .when(name_focused, |line| {
                    line.child(
                        div()
                            .flex_shrink_0()
                            .w(px(1.0))
                            .h(px(13.0))
                            .bg(rgba(t.accent)),
                    )
                })
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_primary))
                        .child(name_after.to_owned()),
                )
        };
        let name_input_inner = div()
            .id("studio-theme-name-input")
            .track_focus(&self.custom_theme_name_focus)
            .tab_index(0)
            .flex_1()
            .min_w(px(160.0))
            .h(px(30.0))
            .px(px(10.0))
            .rounded(px(8.0))
            .bg(rgba(t.surface_muted))
            .border_1()
            .border_color(rgba(if name_focused { t.accent } else { t.border }))
            .flex()
            .items_center()
            .cursor_text()
            .on_click(cx.listener(|this, _, win, cx| {
                this.custom_theme_name_focus.focus(win);
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, win, cx| {
                if !this.custom_theme_name_focus.is_focused(win) {
                    return;
                }
                if this.handle_text_field_key_down(ActiveTextField::CustomThemeName, event, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(name_display);
        let name_input = TextInput::new(
            name_input_inner,
            self.custom_theme_name_focus.clone(),
            cx.entity(),
        );

        let is_active_custom = !self.settings.theme_pack.trim().is_empty()
            && !theme_packs::is_built_in(&self.settings.theme_pack);
        let mut saver_bar = div()
            .w_full()
            .px(px(16.0))
            .py(px(10.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .child(name_input)
            .child(button(
                "btn-theme-save-new",
                t,
                loc::t("ThemeSaveAsNew"),
                ButtonVariant::Primary,
                cx.listener(|this, _, win, cx| this.save_studio_custom_theme(false, win, cx)),
            ));
        if is_active_custom {
            saver_bar = saver_bar.child(button(
                "btn-theme-update-custom",
                t,
                loc::t("ThemeUpdateCustom"),
                ButtonVariant::Secondary,
                cx.listener(|this, _, win, cx| this.save_studio_custom_theme(true, win, cx)),
            ));
        }
        saver_bar = saver_bar.child(button(
            "btn-theme-reset-overrides",
            t,
            loc::t("ThemeEditorReset"),
            ButtonVariant::Secondary,
            cx.listener(|this, _, win, cx| {
                persist_theme_tokens(&mut this.settings, Map::new());
                this.token_apply_status = loc::t("TokenApplyHint");
                this.apply_theme_change(Some(win), false, cx);
            }),
        ));
        body.push(saver_bar.into_any_element());

        // Collapsible Advanced Token Inspector (preserves ThemeTokens & ThemeBrushPicker targets)
        let tokens_map: Map<String, Value> = self
            .settings
            .extra
            .get("ThemeTokens")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let force_open = matches!(
            self.highlight_row.as_deref(),
            Some("ThemeTokens" | "ThemeBrushPicker")
        );
        let show_advanced = self.advanced_tokens_open || force_open;

        body.push(
            self.mark_target(
                "ThemeTokens",
                t,
                div()
                    .w_full()
                    .px(px(16.0))
                    .py(px(9.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon_glyph("\u{E790}", 13.0, t.accent))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(12.5)))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(rgba(t.text_primary))
                                    .child(loc::t("ThemeAdvancedTokens")),
                            )
                            .when(!tokens_map.is_empty(), |row| {
                                row.child(badge(t, tokens_map.len().to_string(), true))
                            }),
                    )
                    .child(button(
                        "btn-toggle-advanced-tokens",
                        t,
                        if show_advanced {
                            loc::t("Close")
                        } else {
                            loc::t("Open")
                        },
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| {
                            this.advanced_tokens_open = !this.advanced_tokens_open;
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element(),
        );

        if show_advanced {
            let cat_opts: Vec<DropdownOption> = THEME_TOKEN_CATEGORIES
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let entity = cx.entity().downgrade();
                    DropdownOption {
                        label: SharedString::from(loc::t(name)),
                        flag_path: None,
                        glyph: None,
                        selected: self.theme_category == i,
                        on_select: std::rc::Rc::new(move |_win, app| {
                            let _ = entity.update(app, |this, cx| {
                                this.open_dropdown = None;
                                this.theme_category = i;
                                let keys = editor_tokens_for_category(i);
                                if let Some(first) = keys.first() {
                                    this.token_name = (*first).to_owned();
                                }
                                cx.notify();
                            });
                        }),
                    }
                })
                .collect();
            let category_keys = editor_tokens_for_category(self.theme_category);
            let selected_token = if category_keys
                .iter()
                .any(|k| k.eq_ignore_ascii_case(&self.token_name))
            {
                self.token_name.clone()
            } else {
                category_keys
                    .first()
                    .copied()
                    .unwrap_or("AccentBrush")
                    .to_owned()
            };
            let token_opts: Vec<DropdownOption> = category_keys
                .iter()
                .map(|&key| {
                    let entity = cx.entity().downgrade();
                    DropdownOption {
                        label: SharedString::from(key),
                        flag_path: None,
                        glyph: None,
                        selected: selected_token.eq_ignore_ascii_case(key),
                        on_select: std::rc::Rc::new(move |_win, app| {
                            let _ = entity.update(app, |this, cx| {
                                this.open_dropdown = None;
                                this.token_name = key.to_owned();
                                cx.notify();
                            });
                        }),
                    }
                })
                .collect();

            let active_token_color = ThemeManager::resolve(&selected_token)
                .and_then(|val| crate::design::theme::token_rgba(&val))
                .unwrap_or_else(|| self.brush_active_color());
            let trigger_hover = t.surface_hover;
            let pick_token = selected_token.clone();
            let brush_trigger = div()
                .id("btn-open-brush-picker")
                .h(px(32.0))
                .px(px(10.0))
                .rounded(px(8.0))
                .bg(rgba(t.surface_muted))
                .border_1()
                .border_color(rgba(t.border))
                .flex()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .hover(move |s| s.bg(rgba(trigger_hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_token_brush_picker(&pick_token, cx);
                }))
                .child(
                    div()
                        .w(px(28.0))
                        .h(px(18.0))
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(rgba(t.border))
                        .bg(rgba(active_token_color.to_gpui())),
                )
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(11.5)))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgba(t.text_primary))
                        .child(active_token_color.hex()),
                )
                .child(icon_glyph("\u{E790}", 12.0, t.text_secondary));

            body.push(
                setting_row(
                    t,
                    Some("\u{E790}"),
                    loc::t("ThemeTokenCategory"),
                    self.make_dropdown("token-cat", t, cat_opts, cx),
                )
                .into_any_element(),
            );
            body.push(
                setting_row(
                    t,
                    Some("\u{E790}"),
                    loc::t("ThemeTokenName"),
                    self.make_dropdown("token-key", t, token_opts, cx),
                )
                .into_any_element(),
            );
            body.push(self.target_row(
                "ThemeBrushPicker",
                t,
                Some("\u{E790}"),
                loc::t("ThemeBrushPicker"),
                brush_trigger,
            ));

            let mut saved: Vec<(String, String)> = tokens_map
                .iter()
                .map(|(k, v)| {
                    let text = match v {
                        Value::String(s) => s.clone(),
                        _ => v.to_string(),
                    };
                    (k.clone(), text)
                })
                .collect();
            saved.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, value) in saved.into_iter().take(24) {
                let sw_u32 = parse_hex_color(&value).unwrap_or(t.accent);
                let remove_key = name.clone();
                let edit_key = name.clone();
                body.push(
                    setting_row(
                        t,
                        Some("\u{E790}"),
                        name.clone(),
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .id(SharedString::from(format!("tok-sw-{name}")))
                                    .px(px(7.0))
                                    .py(px(3.0))
                                    .rounded(px(6.0))
                                    .bg(rgba(t.surface_muted))
                                    .border_1()
                                    .border_color(rgba(t.border))
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_token_brush_picker(&edit_key, cx);
                                    }))
                                    .child(div().size(px(12.0)).rounded(px(3.0)).bg(rgba(sw_u32)))
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(rgba(t.text_secondary))
                                            .child(value),
                                    ),
                            )
                            .child(compact_card_button(
                                SharedString::from(format!("tok-clear-{name}")),
                                t,
                                "×",
                                false,
                                cx.listener(move |this, _, win, cx| {
                                    let mut map = this
                                        .settings
                                        .extra
                                        .get("ThemeTokens")
                                        .and_then(Value::as_object)
                                        .cloned()
                                        .unwrap_or_default();
                                    map.remove(&remove_key);
                                    persist_theme_tokens(&mut this.settings, map);
                                    this.sync_settings_glass(win);
                                    this.auto_save(cx);
                                }),
                            )),
                    )
                    .into_any_element(),
                );
            }
        }

        section_card(t, loc::t("ThemeStudioTitle"), body).into_any_element()
    }

    /// Shared Figma-style floating Color & Gradient Brush Picker modal (used by both
    /// `ThemeBrushPicker` tokens and surface border colors).
    fn render_brush_picker_overlay(
        &self,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.brush_picker_open {
            return None;
        }
        let allow_gradient = self.brush_picker_target == BrushPickerTarget::ThemeToken;
        let is_gradient = allow_gradient && self.brush_mode == BrushMode::Gradient;
        let active_color = self.brush_active_color();
        let active_hsv = Hsv::from(active_color);
        let alpha_pct = ((f32::from(self.brush_alpha) / 255.0) * 100.0).round() as i32;

        let modal_title = match self.brush_picker_target {
            BrushPickerTarget::ThemeToken => loc::t("ThemeBrushPicker"),
            BrushPickerTarget::BorderSurface(_) => loc::t("BorderColorLabel"),
        };

        // 2D Saturation / Value field (Figma-style crisp interactive square).
        const FIELD_COLS: usize = 24;
        const FIELD_ROWS: usize = 12;
        const FIELD_W: f32 = 312.0;
        const FIELD_H: f32 = 144.0;
        let field_hue = active_hsv.h;
        let mut sv_field = div()
            .id("figma-sv-field")
            .relative()
            .w(px(FIELD_W))
            .h(px(FIELD_H))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgba(t.border))
            .overflow_hidden();
        for row in 0..FIELD_ROWS {
            let mut line = div().w_full().h(px(FIELD_H / FIELD_ROWS as f32)).flex();
            for col in 0..FIELD_COLS {
                let cell = field_cell_hsv(field_hue, col, row, FIELD_COLS, FIELD_ROWS);
                let cell_color = cell.to_rgba(255).to_gpui();
                line = line.child(
                    div()
                        .id(SharedString::from(format!("figma-sv-{row}-{col}")))
                        .flex_1()
                        .h_full()
                        .cursor_pointer()
                        .bg(rgba(cell_color))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.set_brush_hsv(cell, cx);
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                                if ev.dragging() {
                                    this.set_brush_hsv(cell, cx);
                                }
                            },
                        )),
                );
            }
            sv_field = sv_field.child(line);
        }
        let marker_x =
            (active_hsv.s * f64::from(FIELD_W) - 7.0).clamp(0.0, f64::from(FIELD_W) - 14.0) as f32;
        let marker_y = ((1.0 - active_hsv.v) * f64::from(FIELD_H) - 7.0)
            .clamp(0.0, f64::from(FIELD_H) - 14.0) as f32;
        sv_field = sv_field.child(
            div()
                .absolute()
                .left(px(marker_x))
                .top(px(marker_y))
                .w(px(14.0))
                .h(px(14.0))
                .rounded(px(7.0))
                .border_2()
                .border_color(rgba(0xFFFF_FFFF))
                .bg(rgba(active_color.to_gpui())),
        );

        // Rainbow Hue spectrum bar (36 steps) with circular Figma thumb.
        const BAR_W: f32 = 312.0;
        const HUE_STEPS: usize = 36;
        let mut hue_bar = div()
            .id("figma-hue-bar")
            .relative()
            .w(px(BAR_W))
            .h(px(14.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgba(t.border))
            .overflow_hidden()
            .flex();
        for i in 0..HUE_STEPS {
            let hue = (i as f64 * 359.0) / (HUE_STEPS - 1) as f64;
            let seg_color = Hsv::new(hue, 1.0, 1.0).to_rgba(255).to_gpui();
            hue_bar = hue_bar.child(
                div()
                    .id(SharedString::from(format!("figma-hue-{i}")))
                    .flex_1()
                    .h_full()
                    .cursor_pointer()
                    .bg(rgba(seg_color))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            let cur = this.brush_active_hsv();
                            this.set_brush_hsv(Hsv::new(hue, cur.s, cur.v), cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                        if ev.dragging() {
                            let cur = this.brush_active_hsv();
                            this.set_brush_hsv(Hsv::new(hue, cur.s, cur.v), cx);
                        }
                    })),
            );
        }
        let hue_thumb_x = ((active_hsv.h / 359.0) * f64::from(BAR_W) - 7.0)
            .clamp(0.0, f64::from(BAR_W) - 14.0) as f32;
        hue_bar = hue_bar.child(
            div()
                .absolute()
                .left(px(hue_thumb_x))
                .top(px(0.0))
                .w(px(14.0))
                .h(px(14.0))
                .rounded(px(7.0))
                .border_2()
                .border_color(rgba(0xFFFF_FFFF))
                .bg(rgba(
                    Hsv::new(active_hsv.h, 1.0, 1.0).to_rgba(255).to_gpui(),
                )),
        );

        // Opacity gradient bar (24 steps) with circular thumb.
        const ALPHA_STEPS: usize = 24;
        let mut alpha_bar = div()
            .id("figma-alpha-bar")
            .relative()
            .w(px(BAR_W))
            .h(px(14.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgba(t.border))
            .bg(rgba(t.surface_muted))
            .overflow_hidden()
            .flex();
        for i in 0..ALPHA_STEPS {
            let alpha = ((i as f32 / (ALPHA_STEPS - 1) as f32) * 255.0).round() as u8;
            let seg_color = crate::design::tokens::Rgba::new(
                active_color.r,
                active_color.g,
                active_color.b,
                alpha,
            )
            .to_gpui();
            alpha_bar = alpha_bar.child(
                div()
                    .id(SharedString::from(format!("figma-alpha-{i}")))
                    .flex_1()
                    .h_full()
                    .cursor_pointer()
                    .bg(rgba(seg_color))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.set_brush_alpha(alpha, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                        if ev.dragging() {
                            this.set_brush_alpha(alpha, cx);
                        }
                    })),
            );
        }
        let alpha_thumb_x =
            ((f32::from(self.brush_alpha) / 255.0) * BAR_W - 7.0).clamp(0.0, BAR_W - 14.0);
        alpha_bar = alpha_bar.child(
            div()
                .absolute()
                .left(px(alpha_thumb_x))
                .top(px(0.0))
                .w(px(14.0))
                .h(px(14.0))
                .rounded(px(7.0))
                .border_2()
                .border_color(rgba(0xFFFF_FFFF))
                .bg(rgba(active_color.to_gpui())),
        );

        let mut modal = div()
            .id("figma-brush-modal")
            .w(px(348.0))
            .max_w_full()
            .p(px(18.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .rounded(px(14.0))
            .bg(rgba(t.popover_bg))
            .border_1()
            .border_color(rgba(t.border))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        // Header
        let hover_bg = t.surface_hover;
        modal = modal.child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(icon_glyph("\u{E790}", 14.0, t.accent))
                        .child(
                            div()
                                .text_size(px(fonts::scaled_size(14.0)))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgba(t.text_primary))
                                .child(modal_title),
                        ),
                )
                .child(
                    div()
                        .id("figma-brush-close-x")
                        .w(px(28.0))
                        .h(px(28.0))
                        .rounded(px(7.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgba(hover_bg)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.brush_picker_open = false;
                            cx.notify();
                        }))
                        .child(icon_glyph("\u{E8BB}", 11.0, t.text_secondary)),
                ),
        );

        // Mode segmented bar (Solid | Gradient) — Figma fill-type switcher
        if allow_gradient {
            modal = modal.child(
                div()
                    .w_full()
                    .p(px(3.0))
                    .rounded(px(9.0))
                    .bg(rgba(t.surface_muted))
                    .border_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id("figma-mode-solid")
                            .flex_1()
                            .h(px(28.0))
                            .rounded(px(6.0))
                            .bg(rgba(if !is_gradient { t.accent } else { 0x0000_0000 }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(rgba(if !is_gradient {
                                t.accent_fg
                            } else {
                                t.text_secondary
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.brush_mode = BrushMode::Solid;
                                cx.notify();
                            }))
                            .child(loc::t("SurfaceSolid")),
                    )
                    .child(
                        div()
                            .id("figma-mode-gradient")
                            .flex_1()
                            .h(px(28.0))
                            .rounded(px(6.0))
                            .bg(rgba(if is_gradient { t.accent } else { 0x0000_0000 }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(rgba(if is_gradient {
                                t.accent_fg
                            } else {
                                t.text_secondary
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.brush_mode = BrushMode::Gradient;
                                cx.notify();
                            }))
                            .child(loc::t("Gradient")),
                    ),
            );
        }

        // Gradient stop strip + stop add/remove + angle slider (when Gradient is selected)
        if is_gradient {
            let stop_count = self.brush_stops.len().max(2);
            let mut strip = div()
                .id("figma-gradient-strip")
                .relative()
                .w(px(BAR_W))
                .h(px(24.0))
                .rounded(px(7.0))
                .border_1()
                .border_color(rgba(t.border))
                .overflow_hidden()
                .flex();
            const STRIP_SEGMENTS: usize = 48;
            for segment in 0..STRIP_SEGMENTS {
                let pos = (segment as f64 + 0.5) / STRIP_SEGMENTS as f64;
                let scaled = pos * (stop_count - 1) as f64;
                let index = scaled.floor() as usize;
                let frac = scaled - index as f64;
                let a = self.brush_stops.get(index).copied().unwrap_or_default();
                let b = self
                    .brush_stops
                    .get((index + 1).min(stop_count - 1))
                    .copied()
                    .unwrap_or(a);
                let target = gradient_stop_at(pos, stop_count);
                let segment_color = a.mix(b, frac).to_gpui();
                strip = strip.child(
                    div()
                        .id(SharedString::from(format!("figma-strip-{segment}")))
                        .flex_1()
                        .h_full()
                        .cursor_pointer()
                        .bg(rgba(segment_color))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                if let Some(stop) = this.brush_stops.get(target).copied() {
                                    this.brush_selected_stop = target;
                                    this.brush_hsv = Hsv::from(stop);
                                    this.brush_alpha = stop.a;
                                    this.brush_hex = stop.hex();
                                    cx.notify();
                                }
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                                if ev.dragging() {
                                    let from = this.brush_selected_stop;
                                    this.move_brush_stop(from, target, cx);
                                }
                            },
                        )),
                );
            }
            for (index, color) in self.brush_stops.iter().enumerate() {
                let left = (index as f32 / (stop_count - 1) as f32) * (BAR_W - 14.0);
                let selected = index == self.brush_selected_stop;
                strip = strip.child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(0.0))
                        .w(px(14.0))
                        .h(px(24.0))
                        .rounded(px(4.0))
                        .border_2()
                        .border_color(rgba(if selected { t.accent } else { 0xFFFF_FFFF }))
                        .bg(rgba(color.to_gpui())),
                );
            }

            let angle_handler = |i: usize| -> UiHandler {
                let entity = cx.entity().downgrade();
                std::rc::Rc::new(move |_win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.brush_angle = (i as f64 * 360.0 / 36.0).min(360.0);
                        cx.notify();
                    });
                })
            };

            modal = modal
                .child(strip)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(button(
                                    "figma-stop-add",
                                    t,
                                    loc::t("ColorAddStop"),
                                    ButtonVariant::Secondary,
                                    cx.listener(|this, _, _, cx| this.add_brush_stop(cx)),
                                ))
                                .child(button(
                                    "figma-stop-remove",
                                    t,
                                    loc::t("ColorRemove"),
                                    ButtonVariant::Destructive,
                                    cx.listener(|this, _, _, cx| this.remove_brush_stop(cx)),
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgba(t.text_secondary))
                                .child(format!(
                                    "{}/{}",
                                    self.brush_stops.len(),
                                    MAX_GRADIENT_STOPS
                                )),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(fonts::scaled_size(12.0)))
                                .text_color(rgba(t.text_secondary))
                                .child(loc::t("ColorAngle")),
                        )
                        .child(live_slider(
                            "figma-brush-angle",
                            t,
                            self.brush_angle as f32 / 360.0,
                            37,
                            format!("{:.0}\u{00B0}", self.brush_angle),
                            angle_handler,
                        )),
                );
        }

        // 2D SV Field + Hue + Opacity
        modal = modal.child(sv_field).child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(hue_bar)
                .child(alpha_bar),
        );

        // Editable HEX box + Opacity % readout
        let hex_focus = self.brush_hex_focus.clone();
        modal = modal.child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .w(px(30.0))
                        .h(px(30.0))
                        .rounded(px(7.0))
                        .border_1()
                        .border_color(rgba(t.border))
                        .bg(rgba(active_color.to_gpui())),
                )
                .child(
                    div()
                        .id("figma-hex-input")
                        .track_focus(&self.brush_hex_focus)
                        .tab_index(0)
                        .flex_1()
                        .h(px(30.0))
                        .px(px(10.0))
                        .rounded(px(7.0))
                        .bg(rgba(t.surface_muted))
                        .border_1()
                        .border_color(rgba(t.border))
                        .flex()
                        .items_center()
                        .justify_between()
                        .cursor_text()
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            hex_focus.focus(window);
                        })
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            let mut buffer = this.brush_hex.clone();
                            if apply_keystroke(
                                &mut buffer,
                                &event.keystroke.key,
                                event.keystroke.key_char.as_deref(),
                                false,
                            ) {
                                this.brush_hex = buffer;
                                if let Some(color) =
                                    crate::design::brush::parse_hex(&this.brush_hex)
                                {
                                    this.brush_hsv = Hsv::from(color);
                                    this.brush_alpha = color.a;
                                    if this.brush_mode == BrushMode::Gradient
                                        && let Some(stop) =
                                            this.brush_stops.get_mut(this.brush_selected_stop)
                                    {
                                        *stop = color;
                                    }
                                }
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgba(t.text_secondary))
                                .child(loc::t("ColorHEX")),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(rgba(t.text_primary))
                                .child(self.brush_hex.clone()),
                        ),
                )
                .child(
                    div()
                        .w(px(54.0))
                        .h(px(30.0))
                        .rounded(px(7.0))
                        .bg(rgba(t.surface_muted))
                        .border_1()
                        .border_color(rgba(t.border))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.0))
                        .text_color(rgba(t.text_secondary))
                        .child(format!("{alpha_pct}%")),
                ),
        );

        // Quick swatches row
        const SWATCHES: [(u8, u8, u8); 8] = [
            (0x3B, 0x82, 0xF6),
            (0x63, 0x66, 0xF1),
            (0x8B, 0x5C, 0xF6),
            (0xEC, 0x48, 0x99),
            (0xEF, 0x44, 0x44),
            (0xF5, 0x9E, 0x0B),
            (0x10, 0xB9, 0x81),
            (0xF4, 0xF4, 0xF5),
        ];
        let mut swatch_row = div().w_full().flex().items_center().justify_between();
        for (idx, (r, g, b)) in SWATCHES.into_iter().enumerate() {
            let sw = crate::design::tokens::Rgba::from_rgb(r, g, b);
            swatch_row = swatch_row.child(
                div()
                    .id(SharedString::from(format!("figma-swatch-{idx}")))
                    .w(px(28.0))
                    .h(px(22.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgba(t.border))
                    .bg(rgba(sw.to_gpui()))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_brush_hsv(Hsv::from(sw), cx);
                    })),
            );
        }
        modal = modal.child(swatch_row);

        // Footer actions
        let mut footer = div()
            .w_full()
            .pt(px(4.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0));
        if let BrushPickerTarget::BorderSurface(index) = self.brush_picker_target {
            footer = footer.child(button(
                "figma-brush-reset",
                t,
                loc::t("Reset"),
                ButtonVariant::Secondary,
                cx.listener(move |this, _, window, cx| {
                    this.reset_border_picker_color(index, window, cx)
                }),
            ));
        } else {
            footer = footer.child(div());
        }
        footer = footer.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(button(
                    "figma-brush-cancel",
                    t,
                    loc::t("Close"),
                    ButtonVariant::Secondary,
                    cx.listener(|this, _, _, cx| {
                        this.brush_picker_open = false;
                        cx.notify();
                    }),
                ))
                .child(button(
                    "figma-brush-apply",
                    t,
                    loc::t("Apply"),
                    ButtonVariant::Primary,
                    cx.listener(|this, _, window, cx| {
                        this.apply_brush_picker_selection(window, cx)
                    }),
                )),
        );
        modal = modal.child(footer);

        Some(
            div()
                .id("figma-brush-backdrop")
                .absolute()
                .inset_0()
                // Keep the scrim inside the rounded window outline.
                .rounded(px(settings_corner_radius(window_chrome::is_windows_11())))
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x7800_0000))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.brush_picker_open = false;
                        cx.notify();
                    }),
                )
                .child(modal)
                .into_any_element(),
        )
    }

    fn render_widget_preview(&self, id: &str, t: &UiTheme, is_dark: bool) -> AnyElement {
        let pal = crate::design::palette(is_dark);
        let c_rgba = |c: (u8, u8, u8), a: u8| rgba(crate::design::gpui_rgba(c, a));
        let chip_bg = if is_dark { 0xffffff10 } else { 0x00000008 };
        let chip_bd = if is_dark { 0xffffff1c } else { 0x00000016 };
        let track_bg = if is_dark { 0xffffff1f } else { 0x00000018 };
        let spec = match id {
            "weather" => {
                let (temp, range) = match self.settings.weather_unit {
                    TemperatureUnit::Celsius => ("21°C", "24°C / 16°C"),
                    TemperatureUnit::Fahrenheit => ("70°F", "75°F / 61°F"),
                };
                WidgetChipSpec::Weather {
                    available: true,
                    icon_color: pal.warning,
                    is_rain: false,
                    temp: temp.to_owned(),
                    place: if self.settings.weather_city.is_empty() {
                        weather_fallback_city().to_owned()
                    } else {
                        self.settings.weather_city.clone()
                    },
                    condition: loc::t("WeatherClear"),
                    today_range: Some(range.to_owned()),
                }
            }
            "stats" => {
                let mut cols = Vec::with_capacity(4);
                if self.settings.stats_meters & 1 != 0 {
                    cols.push(SystemStatColumn {
                        label: "CPU".to_owned(),
                        value_text: "24%".to_owned(),
                        ratio: 0.24,
                        bar_color: pal.widget_blue,
                        warm_text: false,
                    });
                }
                if self.settings.stats_meters & 2 != 0 {
                    cols.push(SystemStatColumn {
                        label: "RAM".to_owned(),
                        value_text: "58%".to_owned(),
                        ratio: 0.58,
                        bar_color: pal.widget_accent,
                        warm_text: false,
                    });
                }
                if self.settings.stats_meters & 16 != 0 {
                    cols.push(SystemStatColumn {
                        label: "GPU".to_owned(),
                        value_text: "38%".to_owned(),
                        ratio: 0.38,
                        bar_color: pal.success,
                        warm_text: false,
                    });
                }
                if self.settings.stats_meters & 32 != 0 {
                    let (lbl, val) = match self.settings.weather_unit {
                        TemperatureUnit::Celsius => ("GPU °C", "64°C"),
                        TemperatureUnit::Fahrenheit => ("GPU °F", "147°F"),
                    };
                    cols.push(SystemStatColumn {
                        label: lbl.to_owned(),
                        value_text: val.to_owned(),
                        ratio: 0.64,
                        bar_color: pal.warning,
                        warm_text: true,
                    });
                }
                WidgetChipSpec::SystemStats { columns: cols }
            }
            "netspeed" => WidgetChipSpec::NetSpeed {
                down: "18.4 MB/s".to_owned(),
                up: "3.2 MB/s".to_owned(),
                down_history: vec![0.34, 0.48, 0.40, 0.62, 0.54, 0.73, 0.68, 0.80, 0.74, 0.78],
                up_history: vec![0.14, 0.22, 0.18, 0.28, 0.24, 0.32, 0.29, 0.36, 0.31, 0.33],
            },
            "battery" => WidgetChipSpec::BatteryLiquid {
                present: true,
                percent: 84,
                charging: true,
                wave_phase: 0.35,
            },
            "bluetoothbattery" => WidgetChipSpec::Bluetooth {
                devices: vec![
                    (
                        widgets::telemetry::BluetoothDeviceKind::Headphones,
                        85,
                        false,
                        false,
                    ),
                    (
                        widgets::telemetry::BluetoothDeviceKind::Mouse,
                        64,
                        false,
                        false,
                    ),
                ],
            },
            "media" => WidgetChipSpec::Media {
                title: "Midnight City".to_owned(),
                artist: "M83".to_owned(),
                muted: false,
            },
            "audio" => WidgetChipSpec::Audio {
                volume: 68,
                muted: false,
            },
            "stopwatch" => WidgetChipSpec::Timed {
                is_focus: false,
                running: true,
                text: "04:18".to_owned(),
            },
            "countdown" => WidgetChipSpec::Timed {
                is_focus: false,
                running: false,
                text: "12:30".to_owned(),
            },
            "focus" => WidgetChipSpec::Timed {
                is_focus: true,
                running: true,
                text: "21:45".to_owned(),
            },
            "aiusage_claude" => WidgetChipSpec::AiDual {
                provider_id: "aiusage_claude",
                brand_color: Some((0xd9, 0x77, 0x57)),
                rows: vec![
                    ("5h".to_owned(), 0.42, pal.widget_accent, "2h".to_owned()),
                    ("7d".to_owned(), 0.68, pal.widget_accent, "4d".to_owned()),
                ],
            },
            "aiusage_codex" => WidgetChipSpec::AiDual {
                provider_id: "aiusage_codex",
                brand_color: Some((0x8d, 0x99, 0xff)),
                rows: vec![
                    ("5h".to_owned(), 0.28, pal.widget_accent, "3h".to_owned()),
                    ("7d".to_owned(), 0.54, pal.widget_accent, "5d".to_owned()),
                ],
            },
            "aiusage_cursor" => WidgetChipSpec::AiDual {
                provider_id: "aiusage_cursor",
                brand_color: None,
                rows: vec![("30d".to_owned(), 0.61, pal.widget_accent, "12d".to_owned())],
            },
            "aiusage_opencode" => WidgetChipSpec::AiOpenCode {
                columns: vec![
                    (
                        "1h".to_owned(),
                        widgets::format_percent(18),
                        0.18,
                        pal.success,
                        "42m".to_owned(),
                    ),
                    (
                        "7d".to_owned(),
                        widgets::format_percent(49),
                        0.49,
                        pal.success,
                        "3d".to_owned(),
                    ),
                    (
                        "30d".to_owned(),
                        widgets::format_percent(64),
                        0.64,
                        pal.success,
                        "11d".to_owned(),
                    ),
                ],
            },
            "aiusage_copilot" => {
                let is_tr = loc::language().to_ascii_lowercase().starts_with("tr");
                let days = if is_tr { "14g" } else { "14d" };
                WidgetChipSpec::AiCopilot {
                    value_text: widgets::format_percent(36),
                    meta_text: loc::t_fmt("AiUsageBalanceLeft", &[days]),
                    used: 0.36,
                    bar_color: (0x9f, 0x9f, 0xf2),
                }
            }
            "aiusage_deepseek" => WidgetChipSpec::AiBalance {
                provider_id: "aiusage_deepseek",
                brand_color: (0x4d, 0x6b, 0xfe),
                name: "DeepSeek",
                value_text: "$14.80".to_owned(),
            },
            "aiusage_openrouter" => WidgetChipSpec::AiBalance {
                provider_id: "aiusage_openrouter",
                brand_color: (0xb7, 0x9b, 0xf8),
                name: "OpenRouter",
                value_text: "$22.40".to_owned(),
            },
            _ => self.widget_runtime.chip_spec(id, &self.settings, is_dark),
        };

        let chip = |inner: gpui::Div| -> AnyElement {
            div()
                .h(px(44.0))
                .max_w_full()
                .px(px(11.0))
                .rounded(px(12.0))
                .bg(rgba(chip_bg))
                .border_1()
                .border_color(rgba(chip_bd))
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .child(inner)
                .into_any_element()
        };

        let mini_bar = |ratio: f32, col: (u8, u8, u8), bar_w: f32| {
            let fill_w = (bar_w * ratio.clamp(0.0, 1.0))
                .round()
                .max(if ratio > 0.01 { 3.0 } else { 0.0 });
            div()
                .w(px(bar_w))
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(rgba(track_bg))
                .overflow_hidden()
                .child(
                    div()
                        .w(px(fill_w))
                        .h_full()
                        .rounded(px(2.0))
                        .bg(c_rgba(col, 0xff)),
                )
        };

        let vdiv = || {
            div()
                .w(px(1.0))
                .h(px(18.0))
                .bg(rgba(t.separator))
                .flex_shrink_0()
        };

        match spec {
            WidgetChipSpec::Clock {
                time,
                date,
                weekday,
            } => {
                let show_weekday = !clock_weekday_is_redundant(&date, &weekday);
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon_glyph("\u{E823}", 13.0, t.dock_preview_sub))
                        .child(
                            div()
                                .text_size(px(fonts::scaled_size(14.0)))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgba(t.dock_preview_fg))
                                .child(time),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(10.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(date),
                                )
                                .when(show_weekday, |col| {
                                    col.child(
                                        div()
                                            .text_size(px(fonts::scaled_size(9.0)))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(weekday),
                                    )
                                }),
                        ),
                )
            }
            WidgetChipSpec::Weather {
                available,
                icon_color,
                is_rain,
                temp,
                place,
                condition,
                today_range,
            } => {
                if !available {
                    chip(
                        div()
                            .text_size(px(fonts::scaled_size(11.0)))
                            .text_color(rgba(t.dock_preview_sub))
                            .child(condition),
                    )
                } else {
                    let mut row = div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon_glyph(
                            if is_rain { "\u{E9C4}" } else { "\u{E706}" },
                            15.0,
                            crate::design::gpui_rgba(icon_color, 0xff),
                        ))
                        .child(
                            div()
                                .text_size(px(fonts::scaled_size(15.0)))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgba(t.dock_preview_fg))
                                .child(temp),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(10.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(place),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(condition),
                                ),
                        );
                    if let Some(range) = today_range {
                        // Secondary info: extra breathing room around the divider and muted
                        // high/low so the current temperature + city stay the focal point.
                        row = row.child(vdiv().mx(px(4.0))).child(
                            div()
                                .flex()
                                .flex_col()
                                .opacity(0.85)
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(loc::t("Today")),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(10.0)))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(range),
                                ),
                        );
                    }
                    chip(row)
                }
            }
            WidgetChipSpec::DayProgress {
                ratio,
                percent: _,
                unit,
            } => {
                let pct = format!("{:.0}", (ratio * 100.0).clamp(0.0, 100.0));
                let pct_text = loc::t_fmt("OpacityPercent", &[&pct]);
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(progress_ring(
                            20.0,
                            2.5,
                            ratio,
                            crate::design::gpui_rgba(pal.widget_blue, 0x38),
                            crate::design::gpui_rgba(pal.widget_blue, 0xff),
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(12.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(pct_text),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(unit),
                                ),
                        ),
                )
            }
            WidgetChipSpec::Timed {
                is_focus,
                running,
                text,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(if is_focus {
                        let ring_col = if running {
                            crate::design::gpui_rgba(pal.selected_fg, 0xff)
                        } else {
                            crate::design::gpui_rgba(pal.widget_accent, 0xcc)
                        };
                        progress_ring(18.0, 2.4, 0.72, track_bg, ring_col).into_any_element()
                    } else {
                        icon_glyph(
                            if id == "countdown" {
                                "\u{EA8F}"
                            } else {
                                "\u{E916}"
                            },
                            13.0,
                            if running {
                                crate::design::gpui_rgba(pal.selected_fg, 0xff)
                            } else {
                                t.dock_preview_sub
                            },
                        )
                        .into_any_element()
                    })
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(text),
                    )
                    .child(
                        div()
                            .size(px(22.0))
                            .rounded(px(6.0))
                            .bg(rgba(if running {
                                crate::design::gpui_rgba(pal.selected_fill, 0xff)
                            } else {
                                track_bg
                            }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_glyph(
                                if running { "\u{E769}" } else { "\u{E768}" },
                                10.0,
                                if running {
                                    crate::design::gpui_rgba(pal.selected_fg, 0xff)
                                } else {
                                    t.dock_preview_fg
                                },
                            )),
                    ),
            ),
            WidgetChipSpec::SystemStats { columns } => {
                if columns.is_empty() {
                    chip(
                        div()
                            .text_size(px(fonts::scaled_size(11.0)))
                            .text_color(rgba(t.dock_preview_sub))
                            .child("—"),
                    )
                } else {
                    let warm_col = if is_dark {
                        crate::design::gpui_rgba(pal.warning, 0xff)
                    } else {
                        0xb45309ff
                    };
                    let mut row = div().flex().items_center().gap(px(9.0));
                    for (idx, col) in columns.into_iter().take(4).enumerate() {
                        if idx > 0 {
                            row = row.child(vdiv());
                        }
                        let fill_w = (36.0 * col.ratio.clamp(0.0, 1.0)).round();
                        row = row.child(
                            div()
                                .min_w(px(36.0))
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .whitespace_nowrap()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(col.label),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(12.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(if col.warm_text {
                                            warm_col
                                        } else {
                                            t.dock_preview_fg
                                        }))
                                        .child(col.value_text),
                                )
                                .child(
                                    div()
                                        .w(px(36.0))
                                        .h(px(2.0))
                                        .rounded(px(1.0))
                                        .bg(rgba(track_bg))
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .w(px(fill_w))
                                                .h_full()
                                                .rounded(px(1.0))
                                                .bg(c_rgba(col.bar_color, 0xff)),
                                        ),
                                ),
                        );
                    }
                    chip(row)
                }
            }
            WidgetChipSpec::NetSpeed {
                down,
                up,
                down_history,
                up_history: _,
            } => {
                let split_speed = |s: &str| -> (String, String) {
                    s.rsplit_once(' ').map_or_else(
                        || (s.to_owned(), String::new()),
                        |(n, u)| (n.to_owned(), u.to_owned()),
                    )
                };
                let (d_num, d_unit) = split_speed(&down);
                let (u_num, u_unit) = split_speed(&up);
                let spark = div()
                    .w(px(36.0))
                    .h(px(22.0))
                    .flex()
                    .items_end()
                    .gap(px(1.5))
                    .children(down_history.into_iter().take(10).map(|v| {
                        let bh = (20.0 * v.clamp(0.12, 1.0)).round().max(2.0);
                        div()
                            .flex_1()
                            .h(px(bh))
                            .rounded_t(px(1.0))
                            .bg(c_rgba(pal.success, 0xc8))
                    }));
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .child(spark)
                        .child(vdiv())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(4.0))
                                        .child(icon_glyph(
                                            "\u{E74B}",
                                            9.0,
                                            crate::design::gpui_rgba(pal.success, 0xff),
                                        ))
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(11.0)))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgba(t.dock_preview_fg))
                                                .child(d_num),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(9.5)))
                                                .text_color(rgba(t.dock_preview_sub))
                                                .child(d_unit),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(4.0))
                                        .child(icon_glyph(
                                            "\u{E74A}",
                                            9.0,
                                            crate::design::gpui_rgba(pal.widget_blue, 0xff),
                                        ))
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(11.0)))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgba(t.dock_preview_fg))
                                                .child(u_num),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(9.5)))
                                                .text_color(rgba(t.dock_preview_sub))
                                                .child(u_unit),
                                        ),
                                ),
                        ),
                )
            }
            WidgetChipSpec::BatteryLiquid {
                present,
                percent,
                charging,
                ..
            } => {
                if !present {
                    chip(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(icon_glyph("\u{E850}", 14.0, t.dock_preview_sub))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child("—"),
                            ),
                    )
                } else {
                    let ratio = (percent as f32 / 100.0).clamp(0.04, 1.0);
                    let fill_col = if percent <= 20 {
                        (0xec, 0x5e, 0x6c)
                    } else if percent <= 50 {
                        (0xf2, 0xb8, 0x4e)
                    } else {
                        (0x34, 0xbf, 0xa3)
                    };
                    let chamber_bg = if is_dark { 0x0000_0048 } else { 0x1818_1b16 };
                    let chamber_bd = if is_dark { 0xffff_ff1f } else { 0x1818_1b1a };
                    let nub_bg = if percent >= 98 {
                        crate::design::gpui_rgba(fill_col, 0xf2)
                    } else if is_dark {
                        0xffff_ff3d
                    } else {
                        0x1818_1b38
                    };
                    let chamber_w = 82.0_f32;
                    let fill_w = (chamber_w * ratio).round();
                    let use_dark_ink = !is_dark && percent < 50;
                    let text_col = if use_dark_ink {
                        t.text_primary
                    } else {
                        0xffff_ffff
                    };
                    div()
                        .w(px(96.0))
                        .h(px(42.0))
                        .p(px(4.0))
                        .rounded(px(12.0))
                        .bg(rgba(chip_bg))
                        .border_1()
                        .border_color(rgba(chip_bd))
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .child(
                            div()
                                .relative()
                                .w(px(chamber_w))
                                .h_full()
                                .rounded(px(8.0))
                                .bg(rgba(chamber_bg))
                                .border_1()
                                .border_color(rgba(chamber_bd))
                                .overflow_hidden()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top_0()
                                        .bottom_0()
                                        .w(px(fill_w))
                                        .bg(c_rgba(fill_col, 0xf2)),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .flex()
                                        .items_center()
                                        .gap(px(3.0))
                                        .when(charging, |row| {
                                            row.child(icon_glyph("\u{E945}", 11.0, text_col))
                                        })
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(13.0)))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgba(text_col))
                                                .child(loc::t_fmt(
                                                    "OpacityPercent",
                                                    &[&percent.to_string()],
                                                )),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .w(px(2.5))
                                .h(px(12.0))
                                .rounded(px(1.5))
                                .bg(rgba(nub_bg)),
                        )
                        .into_any_element()
                }
            }
            WidgetChipSpec::Bluetooth { devices } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .when(devices.is_empty(), |row| {
                        row.child(
                            div()
                                .text_size(px(fonts::scaled_size(11.0)))
                                .text_color(rgba(t.dock_preview_sub))
                                .child("—"),
                        )
                    })
                    .children(devices.into_iter().take(2).enumerate().map(
                        |(i, (kind, pct, low, full))| {
                            let val_col = if low {
                                crate::design::gpui_rgba(pal.warning, 0xff)
                            } else if full {
                                0x34d399ff
                            } else {
                                t.dock_preview_fg
                            };
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .when(i > 0, |el| el.child(vdiv()))
                                .child(icon_glyph(kind.fluent_glyph(), 13.0, t.dock_preview_sub))
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(11.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(val_col))
                                        .child(loc::t_fmt("OpacityPercent", &[&pct.to_string()])),
                                )
                        },
                    )),
            ),
            WidgetChipSpec::Storage { drives } => {
                let sampled = widgets::sample_drives();
                let (drv, used_pct, free_text, used_ratio) =
                    if let Some(d) = sampled.iter().find(|d| d.total_gb > 0.0) {
                        let used = (d.total_gb - d.free_gb).max(0.0);
                        let ratio = (used / d.total_gb).clamp(0.0, 1.0) as f32;
                        let pct = (ratio * 100.0).round() as u32;
                        let free = if d.total_gb >= 972.8 {
                            format!(
                                "{} TB",
                                widgets::telemetry::culture_number(d.free_gb / 1024.0, 1)
                            )
                        } else {
                            format!("{} GB", widgets::telemetry::culture_number(d.free_gb, 0))
                        };
                        (format!("{}:", d.letter), pct, free, ratio)
                    } else if let Some((drv, cap)) = drives.first() {
                        let free = cap.split(" / ").next().unwrap_or(cap).to_owned();
                        (drv.clone(), 55, free, 0.55)
                    } else {
                        ("C:".to_owned(), 55, "213 GB".to_owned(), 0.55)
                    };
                let pct_text = loc::t_fmt("OpacityPercent", &[&used_pct.to_string()]);
                let bar_col = if used_pct >= 90 {
                    pal.danger
                } else if used_pct >= 75 {
                    pal.warning
                } else {
                    pal.widget_blue
                };
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon_glyph("\u{EDA2}", 13.0, t.dock_preview_sub))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(5.0))
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(10.0)))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgba(t.dock_preview_fg))
                                                .child(format!("{drv} {pct_text}")),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(9.0)))
                                                .text_color(rgba(t.dock_preview_sub))
                                                .child(free_text),
                                        ),
                                )
                                .child(mini_bar(used_ratio, bar_col, 72.0)),
                        ),
                )
            }
            WidgetChipSpec::Media {
                title,
                artist,
                muted,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(
                        div()
                            .max_w(px(86.0))
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(10.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(if artist.is_empty() {
                                        "Media".to_owned()
                                    } else {
                                        artist
                                    }),
                            ),
                    )
                    .child(vdiv())
                    .child(icon_glyph("\u{E892}", 10.0, t.dock_preview_fg))
                    .child(icon_glyph("\u{E768}", 11.0, t.dock_preview_fg))
                    .child(icon_glyph("\u{E893}", 10.0, t.dock_preview_fg))
                    .child(icon_glyph(
                        if muted { "\u{E74F}" } else { "\u{E767}" },
                        10.0,
                        t.dock_preview_sub,
                    )),
            ),
            WidgetChipSpec::Audio { volume, muted } => {
                let silent = muted || volume == 0;
                let glyph = if silent {
                    "\u{E74F}"
                } else if volume < 50 {
                    "\u{E993}"
                } else {
                    "\u{E995}"
                };
                let display_pct = if muted { 0 } else { volume };
                let ratio = if silent {
                    0.0
                } else {
                    (volume as f32 / 100.0).clamp(0.0, 1.0)
                };
                chip(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(icon_glyph(
                                    glyph,
                                    13.0,
                                    if silent {
                                        t.dock_preview_sub
                                    } else {
                                        t.dock_preview_fg
                                    },
                                ))
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(12.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(if silent {
                                            t.dock_preview_sub
                                        } else {
                                            t.dock_preview_fg
                                        }))
                                        .child(loc::t_fmt(
                                            "OpacityPercent",
                                            &[&display_pct.to_string()],
                                        )),
                                ),
                        )
                        .child(mini_bar(ratio, pal.widget_blue, 52.0)),
                )
            }
            WidgetChipSpec::QuickSettings { pills } => {
                chip(div().flex().items_center().gap(px(4.0)).children(
                    pills.into_iter().enumerate().map(|(i, (glyph, active))| {
                        let is_night = i == 2;
                        let bg = if active {
                            if is_night {
                                if is_dark { 0xfbbf2428 } else { 0xd9770624 }
                            } else if is_dark {
                                0xffffff24
                            } else {
                                0x18181b1a
                            }
                        } else {
                            0x00000000
                        };
                        let fg = if active {
                            if is_night {
                                if is_dark { 0xfcd34dff } else { 0xb45309ff }
                            } else {
                                t.dock_preview_fg
                            }
                        } else if is_dark {
                            0xf4f4f55c
                        } else {
                            0x18181b5c
                        };
                        div()
                            .size(px(30.0))
                            .rounded(px(15.0))
                            .bg(rgba(bg))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .font_family("Segoe Fluent Icons")
                                    .text_size(px(fonts::scaled_size(13.5)))
                                    .text_color(rgba(fg))
                                    .child(glyph),
                            )
                    }),
                ))
            }
            WidgetChipSpec::Keyboard { code } => chip(
                div()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(5.0))
                    .bg(rgba(track_bg))
                    .text_size(px(fonts::scaled_size(12.0)))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgba(t.dock_preview_fg))
                    .child(code),
            ),
            WidgetChipSpec::AiDual {
                provider_id,
                brand_color,
                rows,
            } => {
                let dot_col =
                    brand_color.unwrap_or_else(|| widgets::ai_provider_color(provider_id));
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(ai_brand_badge(provider_id, dot_col, is_dark))
                        .child(div().flex().flex_col().gap(px(3.0)).children(
                            rows.into_iter().take(2).map(|(lbl, used, col, reset)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .child(
                                        div()
                                            .w(px(18.0))
                                            .text_size(px(fonts::scaled_size(9.0)))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(lbl),
                                    )
                                    .child(mini_bar(used, col, 48.0))
                                    .child(
                                        div()
                                            .text_size(px(fonts::scaled_size(9.0)))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(reset),
                                    )
                            }),
                        )),
                )
            }
            WidgetChipSpec::AiOpenCode { columns } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(ai_brand_badge(
                        "aiusage_opencode",
                        widgets::ai_provider_color("aiusage_opencode"),
                        is_dark,
                    ))
                    .child(div().flex().items_center().gap(px(7.0)).children(
                        columns.into_iter().take(3).map(|(lbl, val, used, col, _)| {
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .w(px(40.0))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(3.0))
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(8.5)))
                                                .text_color(rgba(t.dock_preview_sub))
                                                .child(lbl),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(fonts::scaled_size(9.0)))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgba(t.dock_preview_fg))
                                                .child(val),
                                        ),
                                )
                                .child(mini_bar(used, col, 40.0))
                        }),
                    )),
            ),
            WidgetChipSpec::AiCopilot {
                value_text,
                meta_text,
                used,
                bar_color,
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(ai_brand_badge("aiusage_copilot", bar_color, is_dark))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_size(px(fonts::scaled_size(11.0)))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgba(t.dock_preview_fg))
                                            .child(value_text),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(fonts::scaled_size(9.0)))
                                            .text_color(rgba(t.dock_preview_sub))
                                            .child(meta_text),
                                    ),
                            )
                            .child(mini_bar(used, bar_color, 72.0)),
                    ),
            ),
            WidgetChipSpec::AiBalance {
                provider_id,
                name,
                value_text,
                ..
            } => {
                let dot_col = widgets::ai_provider_color(provider_id);
                chip(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(ai_brand_badge(provider_id, dot_col, is_dark))
                        .child(vdiv())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(9.0)))
                                        .text_color(rgba(t.dock_preview_sub))
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .text_size(px(fonts::scaled_size(11.0)))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgba(t.dock_preview_fg))
                                        .child(value_text),
                                ),
                        ),
                )
            }
            WidgetChipSpec::Simple {
                icon_color,
                top,
                bottom,
                ..
            } => chip(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_glyph(
                        widget_glyph(id),
                        13.0,
                        crate::design::gpui_rgba(icon_color, 0xff),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(11.0)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.dock_preview_fg))
                                    .child(top),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(9.0)))
                                    .text_color(rgba(t.dock_preview_sub))
                                    .child(bottom),
                            ),
                    ),
            ),
            WidgetChipSpec::Custom {
                label,
                value,
                color,
                ratio,
                icon,
                details,
            } => {
                let tint = crate::design::gpui_rgba(color, 0xff);
                let icon_node: AnyElement = match icon {
                    WidgetIcon::Glyph(code) => icon_glyph_owned(
                        char::from_u32(u32::from(code))
                            .unwrap_or('\u{E81E}')
                            .to_string(),
                        13.0,
                        tint,
                    )
                    .into_any_element(),
                    WidgetIcon::Asset(path) => {
                        img(SharedString::from(path.to_string_lossy().into_owned()))
                            .w(px(13.0))
                            .h(px(13.0))
                            .rounded(px(2.0))
                            .flex_shrink_0()
                            .into_any_element()
                    }
                    // PathData is rendered by the native D2D chip; keep the
                    // familiar fallback glyph in the lightweight GPUI preview.
                    WidgetIcon::PathData(_) => {
                        icon_glyph("\u{E81E}", 13.0, tint).into_any_element()
                    }
                };
                let mut column = div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(11.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.dock_preview_fg))
                            .child(value),
                    )
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(9.0)))
                            .text_color(rgba(t.dock_preview_sub))
                            .child(label),
                    );
                if let Some((_, detail)) = details.first() {
                    column = column.child(
                        div()
                            .text_size(px(fonts::scaled_size(9.0)))
                            .text_color(rgba(t.dock_preview_sub))
                            .child(detail.clone()),
                    );
                }
                let mut inner = div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(icon_node)
                    .child(column);
                if let Some(ratio) = ratio {
                    inner = inner.child(mini_bar(ratio, color, 24.0));
                }
                chip(inner)
            }
        }
    }

    fn sync_dock_pins(&mut self) {
        if let Ok(saved) = settings::try_load_from(&settings::settings_path()) {
            self.settings.user_pinned_apps = saved.user_pinned_apps;
            self.settings.user_unpinned_apps = saved.user_unpinned_apps;
            self.settings.user_pinned_background_apps = saved.user_pinned_background_apps;
            self.settings.pin_order = saved.pin_order;
        }
    }

    /// Flushes a debounced write before the window goes away so a quick close cannot lose changes.
    fn flush_pending_save(&mut self, cx: &mut Context<Self>) {
        if self.save_pending {
            self.save_pending = false;
            self.save_generation = self.save_generation.wrapping_add(1);
            self.sync_dock_pins();
            self.save_settings(cx);
        }
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        match settings::save_to(&settings::settings_path(), &self.settings) {
            Ok(()) => {
                self.save_error = None;
                notify_docks_reload();
            }
            Err(err) => {
                opentaskbar_core::log::error(format!("Settings save failed: {err}"));
                self.save_error = Some(err.to_string());
            }
        }
        cx.notify();
    }

    fn window_hwnd(window: &Window) -> usize {
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return 0;
        };
        match handle.as_raw() {
            RawWindowHandle::Win32(raw) => raw.hwnd.get() as usize,
            _ => 0,
        }
    }

    fn is_dark(&self) -> bool {
        match self.settings.theme {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => win32::read_system_is_dark(),
        }
    }

    fn themes_root() -> std::path::PathBuf {
        paths::theme_root()
    }

    fn import_theme(&mut self, cx: &mut Context<Self>) {
        let title = loc::t("ThemePackChooseFile");
        let filter_name = loc::t("ThemePack");
        let Some(archive) = prompt::open_file(&title, &filter_name, "*.ottheme;*.zip") else {
            return;
        };
        if !archive.is_file() {
            self.theme_status = loc::t("ThemePackMissing");
            cx.notify();
            return;
        }
        self.theme_status = match theme_packs::import_theme(&Self::themes_root(), &archive) {
            Ok(id) => loc::t_fmt("ThemePackImported", &[&id]),
            Err(error) => error,
        };
        cx.notify();
    }

    fn export_theme(&mut self, id: &str, cx: &mut Context<Self>) {
        let title = loc::t("ThemePackSaveFile");
        let filter_name = loc::t("ThemePack");
        let default_name = format!("{id}.ottheme");
        let Some(path) = prompt::save_file(&title, &default_name, &filter_name, "*.ottheme") else {
            return;
        };
        self.theme_status = match theme_packs::export_theme(&Self::themes_root(), id, &path) {
            Ok(()) => loc::t("ThemePackExported"),
            Err(error) => error,
        };
        cx.notify();
    }

    fn delete_theme(&mut self, id: &str, window: Option<&Window>, cx: &mut Context<Self>) {
        if theme_packs::is_built_in(id) {
            return;
        }
        let was_active = self.settings.theme_pack.eq_ignore_ascii_case(id);
        match theme_packs::delete_theme(&Self::themes_root(), id) {
            Ok(()) => {
                self.theme_status = loc::t("ThemePackDeleted");
                if was_active {
                    self.settings.theme_pack.clear();
                    self.custom_theme_name.clear();
                    self.custom_theme_name_selection = 0..0;
                    self.apply_theme_change(window, true, cx);
                    return;
                }
            }
            Err(error) => {
                self.theme_status = error;
            }
        }
        cx.notify();
    }

    fn record_hotkey(&mut self, launcher: bool, cx: &mut Context<Self>) {
        let title = loc::t(if launcher {
            "LauncherHotkey"
        } else {
            "DockHotkey"
        });
        let hint = loc::t("HotkeyCaptureHint");
        let current = if launcher {
            self.settings.launcher_binding()
        } else {
            self.settings.dock_binding()
        }
        .map(|binding| binding.display)
        .unwrap_or_else(|| loc::t("HotkeyDisabled"));
        let Some(text) = prompt::ask_name(0, self.is_dark(), &title, &hint, &current) else {
            return;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Esc") {
            return;
        }
        let value = if trimmed.eq_ignore_ascii_case("Backspace")
            || trimmed.eq_ignore_ascii_case("Disabled")
        {
            "Disabled".to_owned()
        } else {
            let Some(binding) = hotkey::parse(trimmed) else {
                self.hotkey_status = loc::t("HotkeyModifierRequired");
                cx.notify();
                return;
            };
            let other = if launcher {
                self.settings.dock_binding()
            } else {
                self.settings.launcher_binding()
            };
            if other.as_ref() == Some(&binding) {
                let other_name = if launcher {
                    "DockHotkey"
                } else {
                    "LauncherHotkey"
                };
                self.hotkey_status = loc::t_fmt("HotkeyConflict", &[&loc::t(other_name)]);
                cx.notify();
                return;
            }
            binding.display
        };
        if launcher {
            self.settings.custom_launcher_hotkey = Some(value);
        } else {
            self.settings.custom_dock_hotkey = Some(value);
        }
        self.hotkey_status.clear();
        self.auto_save(cx);
    }

    // ---- Theme brush picker value helpers -----------------------------------
    // The visual picker edits HSV directly; these helpers keep the preview hex, the
    // selected gradient stop and `design::brush` state in sync.

    fn brush_active_color(&self) -> crate::design::tokens::Rgba {
        if self.brush_mode == BrushMode::Gradient
            && let Some(stop) = self.brush_stops.get(self.brush_selected_stop)
        {
            *stop
        } else {
            self.brush_hsv.to_rgba(self.brush_alpha)
        }
    }

    fn brush_active_hsv(&self) -> Hsv {
        Hsv::from(self.brush_active_color())
    }

    fn set_brush_hsv(&mut self, hsv: Hsv, cx: &mut Context<Self>) {
        self.brush_hsv = hsv;
        let color = hsv.to_rgba(self.brush_alpha);
        if self.brush_mode == BrushMode::Gradient
            && let Some(stop) = self.brush_stops.get_mut(self.brush_selected_stop)
        {
            *stop = color;
        }
        self.brush_hex = color.hex();
        cx.notify();
    }

    fn set_brush_alpha(&mut self, alpha: u8, cx: &mut Context<Self>) {
        self.brush_alpha = alpha;
        self.brush_hex = self.brush_hsv.to_rgba(alpha).hex();
        cx.notify();
    }

    fn move_brush_stop(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let target = reorder_gradient_stop(&mut self.brush_stops, from, to);
        self.brush_selected_stop = target;
        if let Some(stop) = self.brush_stops.get(target).copied() {
            self.brush_hsv = Hsv::from(stop);
            self.brush_hex = stop.hex();
        }
        cx.notify();
    }

    fn add_brush_stop(&mut self, cx: &mut Context<Self>) {
        if self.brush_stops.len() >= MAX_GRADIENT_STOPS {
            return;
        }
        let color = self.brush_hsv.to_rgba(self.brush_alpha);
        let at = (self.brush_selected_stop + 1).min(self.brush_stops.len());
        self.brush_stops.insert(at, color);
        self.brush_selected_stop = at;
        cx.notify();
    }

    fn remove_brush_stop(&mut self, cx: &mut Context<Self>) {
        if self.brush_stops.len() <= 2 {
            return;
        }
        let at = self.brush_selected_stop.min(self.brush_stops.len() - 1);
        self.brush_stops.remove(at);
        self.brush_selected_stop = at.saturating_sub(1);
        if let Some(stop) = self.brush_stops.get(self.brush_selected_stop) {
            self.brush_hsv = Hsv::from(*stop);
        }
        cx.notify();
    }

    fn edit_border_color(&mut self, surface: usize, cx: &mut Context<Self>) {
        self.open_brush_picker(BrushPickerTarget::BorderSurface(surface), cx);
    }

    /// Edits the `DynamicIslandBackgroundColor` hex string. Reads go through the typed field
    /// with the legacy `extra` fallback so an older settings.json stays visible in this editor.
    fn edit_island_background_color(&mut self, cx: &mut Context<Self>) {
        let current = self
            .settings
            .dynamic_island_background_hex()
            .unwrap_or_default()
            .to_owned();
        let title = loc::t("DynamicIslandBackground");
        let hint = loc::t("DynamicIslandBackgroundDescription");
        let is_dark = self.is_dark();
        let Some(text) = prompt::ask_name(0, is_dark, &title, &hint, &current) else {
            return;
        };
        let text = text.trim().to_owned();
        if text.is_empty() {
            self.settings.dynamic_island_background_color = None;
            self.auto_save(cx);
            return;
        }
        let Some(parsed) = TokenRgba::from_hex(&text).filter(|_| parse_hex_color(&text).is_some())
        else {
            return;
        };
        let normalized = format!("#{:02X}{:02X}{:02X}", parsed.r, parsed.g, parsed.b);
        self.settings.dynamic_island_background_color = Some(normalized);
        self.auto_save(cx);
    }

    #[allow(clippy::too_many_arguments)] // merged WS-E settings row helper signature
    fn surface_row(
        &self,
        t: &UiTheme,
        cx: &mut Context<Self>,
        id: &str,
        row_id: &'static str,
        label: String,
        current: SurfaceBackground,
        apply: fn(&mut DockSettings, SurfaceBackground),
    ) -> AnyElement {
        let opts: Vec<DropdownOption> = [
            (loc::t("SurfaceGlass"), SurfaceBackground::Glass),
            (loc::t("SurfaceSolid"), SurfaceBackground::Solid),
            (loc::t("SurfaceNone"), SurfaceBackground::None),
            (loc::t("SurfaceTrueBlack"), SurfaceBackground::TrueBlack),
        ]
        .into_iter()
        .map(|(text, background)| {
            let entity = cx.entity().downgrade();
            DropdownOption {
                label: SharedString::from(text),
                flag_path: None,
                glyph: None,
                selected: current == background,
                on_select: std::rc::Rc::new(move |win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        apply(&mut this.settings, background);
                        this.sync_settings_glass(win);
                        this.auto_save(cx);
                    });
                }),
            }
        })
        .collect();
        let glyph = match id {
            "surf-dock" => "\u{E90E}",
            "surf-menu" => "\u{E700}",
            "surf-widget" => "\u{F246}",
            "surf-settings" => "\u{E713}",
            _ => "\u{E81E}",
        };
        let dd = self.make_dropdown(id, t, opts, cx);
        self.target_row(row_id, t, Some(glyph), label, dd)
    }

    /// One independent opacity row per surface (dock / menu / widget / settings).
    ///
    /// NOTE(WS-D): WS-B adds `SurfaceStyle::set_opacity(kind, f64)`. Until that API lands in
    /// this worktree, the live preview goes through the existing per-surface
    /// `ThemeManager::set_surface(kind, background, opacity)`; persistence uses the matching
    /// `DockSettings` typed opacity field read by `design::theme::style_from_settings`.
    #[allow(clippy::too_many_arguments)]
    fn surface_opacity_row(
        &self,
        t: &UiTheme,
        cx: &mut Context<Self>,
        row_id: &'static str,
        slider_id: &'static str,
        kind: SurfaceKind,
        label: String,
        background: SurfaceBackground,
        current: f64,
    ) -> AnyElement {
        let glyph = match kind {
            SurfaceKind::Dock => "\u{E90E}",
            SurfaceKind::Menu => "\u{E700}",
            SurfaceKind::Widget => "\u{F246}",
            SurfaceKind::Settings => "\u{E713}",
        };
        self.target_row(
            row_id,
            t,
            Some(glyph),
            label,
            live_slider(
                slider_id,
                t,
                ((current as f32) - 0.2) / 0.8,
                17,
                {
                    let pct = (current * 100.0).round() as i32;
                    loc::t_fmt("OpacityPercent", &[&pct.to_string()])
                },
                move |i| {
                    let entity = cx.entity().downgrade();
                    std::rc::Rc::new(move |win, app| {
                        let _ = entity.update(app, |this, cx| {
                            let next = (0.20 + (i as f64) * 0.05).clamp(0.20, 1.0);
                            let (old, target) = match kind {
                                SurfaceKind::Dock => {
                                    (this.settings.dock_opacity, &mut this.settings.dock_opacity)
                                }
                                SurfaceKind::Menu => {
                                    (this.settings.menu_opacity, &mut this.settings.menu_opacity)
                                }
                                SurfaceKind::Widget => (
                                    this.settings.widget_opacity,
                                    &mut this.settings.widget_opacity,
                                ),
                                SurfaceKind::Settings => (
                                    this.settings.settings_opacity,
                                    &mut this.settings.settings_opacity,
                                ),
                            };
                            let changed = (old - next).abs() > 0.001;
                            *target = next;
                            let _ = ThemeManager::set_surface(kind, background, next);
                            if kind == SurfaceKind::Settings {
                                this.sync_settings_glass(win);
                            }
                            if changed {
                                this.auto_save(cx);
                            } else {
                                cx.notify();
                            }
                        });
                    })
                },
            ),
        )
    }

    fn new_profile(&mut self, cx: &mut Context<Self>) {
        let title = loc::t("ProfileNew");
        let label = loc::t("ProfileNamePrompt");
        let Some(name) = prompt::ask_name(0, true, &title, &label, "") else {
            return;
        };
        if self
            .settings
            .profiles
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(&name))
        {
            return;
        }
        self.settings
            .profiles
            .push(snapshot_profile(&self.settings, name.clone()));
        self.settings.active_profile = Some(name);
        self.auto_save(cx);
    }

    /// Overwrite the active profile with the current layout/widget/theme/hotkey state.
    fn save_active_profile(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.settings.active_profile.clone() else {
            return;
        };
        let Some(index) = self.settings.profiles.iter().position(|p| p.name == active) else {
            return;
        };
        let snapshot = snapshot_profile(&self.settings, active);
        self.settings.profiles[index] = snapshot;
        self.auto_save(cx);
    }

    fn rename_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(profile) = self.settings.profiles.get(index) else {
            return;
        };
        let old_name = profile.name.clone();
        let title = loc::t_fmt("ProfileRename", &[&old_name]);
        let label = loc::t("ProfileRenamePrompt");
        let Some(name) = prompt::ask_name(0, true, &title, &label, &old_name) else {
            return;
        };
        if name.is_empty()
            || self
                .settings
                .profiles
                .iter()
                .enumerate()
                .any(|(i, p)| i != index && p.name.eq_ignore_ascii_case(&name))
        {
            return;
        }
        if let Some(profile) = self.settings.profiles.get_mut(index) {
            profile.name = name.clone();
        }
        if self
            .settings
            .active_profile
            .as_deref()
            .is_some_and(|a| a == old_name)
        {
            self.settings.active_profile = Some(name);
        }
        self.auto_save(cx);
    }

    fn delete_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.settings.profiles.len() <= 1 {
            return;
        }
        let Some(name) = self.settings.profiles.get(index).map(|p| p.name.clone()) else {
            return;
        };
        self.settings.profiles.remove(index);
        if self
            .settings
            .active_profile
            .as_deref()
            .is_some_and(|a| a == name)
        {
            self.settings.active_profile = self.settings.profiles.first().map(|p| p.name.clone());
        }
        self.auto_save(cx);
    }

    fn check_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_status = loc::t("QuickLoading");
        self.update_release = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { update::find_newer_packaged_guard() })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(Some(release)) => {
                        view.update_status = loc::t_fmt("UpdateAvailable", &[&release.version]);
                        view.update_release = Some(release);
                    }
                    Ok(None) => {
                        view.update_status =
                            loc::t_fmt("UpdateCurrent", &[update::CURRENT_VERSION]);
                    }
                    Err(()) => view.update_status = loc::t("ErrorUpdateCheck"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn download_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(release) = self.update_release.clone() else {
            return;
        };
        self.update_status = loc::t_fmt("UpdateDownloading", &[&release.version]);
        cx.notify();
        let task_release = release.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    update::apply_with_elevation(&update::DetailedRelease::from(&task_release))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if result.success {
                    let title = loc::t("UpdateAvailableTitle");
                    let body = loc::t_fmt("UpdateToastBody", &[&release.version]);
                    let action = loc::t_fmt("UpdateDownload", &[&release.version]);
                    toast::show_update_toast(&title, &body, &action, &release.url);
                    // Release this process's lock so the updater can replace the executable.
                    std::process::exit(0);
                } else {
                    let key = result.error_key.unwrap_or("UpdateFailed");
                    view.update_status = if key.is_empty() {
                        loc::t("UpdateFailed")
                    } else {
                        loc::t(key)
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_dock(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let align_dd = self.make_dropdown(
            "dock-align",
            t,
            [
                (loc::t("Left"), DockPosition::Left),
                (loc::t("Center"), DockPosition::Center),
                (loc::t("Right"), DockPosition::Right),
            ]
            .into_iter()
            .map(|(label, pos)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.position == pos,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.position = pos;
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let size_dd = self.make_dropdown(
            "dock-size",
            t,
            [
                (loc::t("SizeCompact"), DockSize::Compact),
                (loc::t("SizeMedium"), DockSize::Medium),
                (loc::t("SizeLarge"), DockSize::Large),
            ]
            .into_iter()
            .map(|(label, sz)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.size == sz,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.size = sz;
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let autohide_dd = self.make_dropdown(
            "dock-autohide",
            t,
            [
                (loc::t("HotkeyDisabled"), AutoHideMode::Disabled),
                (loc::t("AutoHideSmart"), AutoHideMode::Smart),
                (loc::t("AutoHideAlways"), AutoHideMode::Always),
            ]
            .into_iter()
            .map(|(label, mode)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.auto_hide == mode,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.auto_hide = mode;
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let start_placement_dd = self.make_dropdown(
            "start-placement",
            t,
            [
                (loc::t("PlacementStart"), StartButtonPlacement::Start),
                (loc::t("PlacementEnd"), StartButtonPlacement::End),
                (loc::t("PlacementHidden"), StartButtonPlacement::Hidden),
                (
                    loc::t("PlacementInWidgets"),
                    StartButtonPlacement::InWidgets,
                ),
            ]
            .into_iter()
            .map(|(label, placement)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.start_placement == placement,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.start_placement = placement;
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let scale_pct = (self.settings.dock_scale * 100.0).round() as i32;
        let width_label = self.settings.dock_custom_width_dips.map_or_else(
            || loc::t("DockWidthAuto"),
            |w| format!("{} ({:.0}px)", loc::t("DockWidthPixels"), w),
        );

        // Runtime layout modes (WS-1 fields). These feed `dock::apply_runtime_settings`
        // through the saved `DockSettings`, so no dock restart is needed.
        let content_align = self.enum_dropdown(
            "dock-content-align",
            t,
            vec![
                (loc::t("ContentAlignStart"), DockContentAlignment::Left),
                (loc::t("ContentAlignCenter"), DockContentAlignment::Center),
                (loc::t("ContentAlignEnd"), DockContentAlignment::Right),
            ],
            self.settings.content_alignment,
            |this, value, cx| {
                this.settings.content_alignment = value;
                this.auto_save(cx);
            },
            cx,
        );
        let width_mode = self.enum_dropdown(
            "dock-width-mode",
            t,
            vec![
                (loc::t("UnitAuto"), DockWidthMode::Auto),
                (loc::t("UnitPixels"), DockWidthMode::Pixels),
                (loc::t("UnitPercent"), DockWidthMode::Percent),
                (loc::t("DockWidthFill"), DockWidthMode::Fill),
            ],
            self.settings.dock_width_mode,
            |this, value, cx| {
                this.settings.dock_width_mode = value;
                this.auto_save(cx);
            },
            cx,
        );
        let height_mode = self.enum_dropdown(
            "dock-height-mode",
            t,
            vec![
                (loc::t("UnitAuto"), DockHeightMode::Auto),
                (loc::t("UnitPixels"), DockHeightMode::Pixels),
                (loc::t("UnitPercent"), DockHeightMode::Percent),
                (loc::t("DockHeightFill"), DockHeightMode::Fill),
            ],
            self.settings.dock_height_mode,
            |this, value, cx| {
                this.settings.dock_height_mode = value;
                this.auto_save(cx);
            },
            cx,
        );
        let top_gap_unit = self.enum_dropdown(
            "dock-top-gap-unit",
            t,
            vec![
                (loc::t("UnitPixels"), DockGapUnit::Pixels),
                (loc::t("UnitPercent"), DockGapUnit::Percent),
            ],
            self.settings.dock_top_gap_unit,
            |this, value, cx| {
                this.settings.dock_top_gap_unit = value;
                this.auto_save(cx);
            },
            cx,
        );
        let width_value_label = match self.settings.dock_width_mode {
            DockWidthMode::Pixels => loc::t("DockWidthPixels"),
            DockWidthMode::Percent => loc::t("DockWidthPercent"),
            _ => loc::t("DockWidthMode"),
        };
        let (width_min, width_max, width_step) =
            dock_width_stepper_bounds(self.settings.dock_width_mode);
        let width_value = self.number_stepper_with_suffix(
            "dock-width-value",
            t,
            match self.settings.dock_width_mode {
                DockWidthMode::Percent => self.settings.dock_width_percent,
                _ => self.settings.dock_width_pixels,
            },
            unit_suffix(self.settings.dock_width_mode == DockWidthMode::Percent),
            width_min,
            width_max,
            width_step,
            |s| {
                if s.dock_width_mode == DockWidthMode::Percent {
                    s.dock_width_percent
                } else {
                    s.dock_width_pixels
                }
            },
            |s, value| {
                if s.dock_width_mode == DockWidthMode::Percent {
                    s.dock_width_percent = value;
                } else {
                    s.dock_width_pixels = value;
                }
            },
            cx,
        );
        let height_value_label = match self.settings.dock_height_mode {
            DockHeightMode::Pixels => loc::t("DockHeightPixels"),
            DockHeightMode::Percent => loc::t("DockHeightPercent"),
            _ => loc::t("DockHeightMode"),
        };
        let (height_min, height_max, height_step) =
            dock_height_stepper_bounds(self.settings.dock_height_mode);
        let height_value = self.number_stepper_with_suffix(
            "dock-height-value",
            t,
            match self.settings.dock_height_mode {
                DockHeightMode::Percent => self.settings.dock_height_percent,
                _ => self.settings.dock_height_pixels,
            },
            unit_suffix(self.settings.dock_height_mode == DockHeightMode::Percent),
            height_min,
            height_max,
            height_step,
            |s| {
                if s.dock_height_mode == DockHeightMode::Percent {
                    s.dock_height_percent
                } else {
                    s.dock_height_pixels
                }
            },
            |s, value| {
                if s.dock_height_mode == DockHeightMode::Percent {
                    s.dock_height_percent = value;
                } else {
                    s.dock_height_pixels = value;
                }
            },
            cx,
        );
        let (top_gap_min, top_gap_max, top_gap_step) =
            dock_top_gap_stepper_bounds(self.settings.dock_top_gap_unit);
        let top_gap_value = self.number_stepper_with_suffix(
            "dock-top-gap-value",
            t,
            match self.settings.dock_top_gap_unit {
                DockGapUnit::Percent => self.settings.dock_top_gap_percent,
                DockGapUnit::Pixels => self.settings.dock_top_gap_pixels,
            },
            unit_suffix(self.settings.dock_top_gap_unit == DockGapUnit::Percent),
            top_gap_min,
            top_gap_max,
            top_gap_step,
            |s| {
                if s.dock_top_gap_unit == DockGapUnit::Percent {
                    s.dock_top_gap_percent
                } else {
                    s.dock_top_gap_pixels
                }
            },
            |s, value| {
                if s.dock_top_gap_unit == DockGapUnit::Percent {
                    s.dock_top_gap_percent = value;
                } else {
                    s.dock_top_gap_pixels = value;
                }
            },
            cx,
        );

        // Auto/Fill sizes are computed by the dock, so their numeric steppers are hidden.
        let mut layout_rows = vec![
            self.target_row(
                "DockAlignment",
                t,
                Some("\u{E8E3}"),
                loc::t("DockAlignment"),
                align_dd,
            ),
            self.target_row(
                "ContentAlignment",
                t,
                Some("\u{E8E3}"),
                loc::t("ContentAlignment"),
                content_align,
            ),
            self.target_row(
                "DockWidthMode",
                t,
                Some(DOCK_WIDTH_GLYPH),
                loc::t("DockWidthMode"),
                width_mode,
            ),
        ];
        if dock_width_has_value(self.settings.dock_width_mode) {
            layout_rows.push(self.target_row(
                "DockWidthValue",
                t,
                Some(DOCK_WIDTH_GLYPH),
                width_value_label,
                width_value,
            ));
        }
        layout_rows.push(self.target_row(
            "DockHeightMode",
            t,
            Some(DOCK_HEIGHT_GLYPH),
            loc::t("DockHeightMode"),
            height_mode,
        ));
        if dock_height_has_value(self.settings.dock_height_mode) {
            layout_rows.push(self.target_row(
                "DockHeightValue",
                t,
                Some(DOCK_HEIGHT_GLYPH),
                height_value_label,
                height_value,
            ));
        }
        layout_rows.extend([
            self.target_row(
                "DockTopGap",
                t,
                Some(DOCK_GAP_GLYPH),
                loc::t("DockTopGapUnit"),
                top_gap_unit,
            ),
            self.target_row(
                "DockTopGapValue",
                t,
                Some(DOCK_GAP_GLYPH),
                loc::t("DockTopGap"),
                top_gap_value,
            ),
            self.target_row("DockSize", t, Some("\u{E9A6}"), loc::t("DockSize"), size_dd),
            self.target_row(
                "StartPlacement",
                t,
                Some("\u{E7C5}"),
                loc::t("StartButtonPlacement"),
                start_placement_dd,
            ),
            self.target_row(
                "StartWidgetIndex",
                t,
                Some("\u{E9A6}"),
                loc::t("StartWidgetIndex"),
                stepper(
                    "start-widget-index-minus",
                    "start-widget-index-plus",
                    t,
                    self.settings.start_widget_index.to_string(),
                    cx.listener(|this, _, _, cx| {
                        this.settings.start_widget_index =
                            (this.settings.start_widget_index - 1).max(0);
                        this.auto_save(cx);
                    }),
                    cx.listener(|this, _, _, cx| {
                        this.settings.start_widget_index =
                            (this.settings.start_widget_index + 1).min(32);
                        this.auto_save(cx);
                    }),
                ),
            ),
            self.target_row(
                "DockScale",
                t,
                Some("\u{E8A3}"),
                loc::t("DockScale"),
                live_slider(
                    "sc",
                    t,
                    ((self.settings.dock_scale as f32) - 0.75) / 0.75,
                    16,
                    loc::t_fmt("OpacityPercent", &[&scale_pct.to_string()]),
                    |i| {
                        let entity = cx.entity().downgrade();
                        std::rc::Rc::new(move |_win, app| {
                            let _ = entity.update(app, |this, cx| {
                                let next = (0.75 + (i as f64) * 0.05).clamp(0.75, 1.50);
                                if (this.settings.dock_scale - next).abs() > 0.001 {
                                    this.settings.dock_scale = next;
                                    this.auto_save(cx);
                                }
                            });
                        })
                    },
                ),
            )
            .into_any_element(),
            self.target_row(
                "DockBottomGap",
                t,
                Some(DOCK_GAP_GLYPH),
                loc::t("DockBottomGap"),
                live_slider(
                    "gap",
                    t,
                    self.settings.dock_bottom_gap_pixels as f32 / 32.0,
                    17,
                    loc::t_fmt(
                        "PixelsValue",
                        &[&self.settings.dock_bottom_gap_pixels.to_string()],
                    ),
                    |i| {
                        let entity = cx.entity().downgrade();
                        std::rc::Rc::new(move |_win, app| {
                            let _ = entity.update(app, |this, cx| {
                                let next = ((i as i32) * 2).clamp(0, 32);
                                if this.settings.dock_bottom_gap_pixels != next {
                                    this.settings.dock_bottom_gap_pixels = next;
                                    this.auto_save(cx);
                                }
                            });
                        })
                    },
                ),
            )
            .into_any_element(),
            self.target_row(
                "DockIconPadding",
                t,
                Some("\u{E96E}"),
                loc::t("DockIconPadding"),
                live_slider(
                    "pad",
                    t,
                    self.settings.dock_icon_padding as f32 / 12.0,
                    13,
                    loc::t_fmt(
                        "PixelsValue",
                        &[&self.settings.dock_icon_padding.to_string()],
                    ),
                    |i| {
                        let entity = cx.entity().downgrade();
                        std::rc::Rc::new(move |_win, app| {
                            let _ = entity.update(app, |this, cx| {
                                let next = (i as i32).clamp(0, 12);
                                if this.settings.dock_icon_padding != next {
                                    this.settings.dock_icon_padding = next;
                                    this.auto_save(cx);
                                }
                            });
                        })
                    },
                ),
            )
            .into_any_element(),
            self.target_row(
                "LayoutFullWidth",
                t,
                Some("\u{E740}"),
                loc::t("LayoutFullWidth"),
                ios_switch(
                    "sw-fullw",
                    t,
                    self.settings.full_width,
                    cx.listener(|this, _, _, cx| {
                        this.settings.full_width = !this.settings.full_width;
                        this.auto_save(cx);
                    }),
                ),
            )
            .into_any_element(),
            setting_row(
                t,
                Some("\u{E72C}"),
                width_label,
                button(
                    "btn-reset-width",
                    t,
                    loc::t("Reset"),
                    ButtonVariant::Secondary,
                    cx.listener(|this, _, _, cx| {
                        this.settings.dock_custom_width_dips = None;
                        this.auto_save(cx);
                    }),
                ),
            )
            .into_any_element(),
        ]);

        let out = vec![
            section_card(t, loc::t("SectionDockLayout"), layout_rows).into_any_element(),
            section_card(
                t,
                format!(
                    "{} & {}",
                    loc::t("SectionBehavior"),
                    loc::t("SectionDisplays")
                ),
                vec![
                    self.target_row(
                        "AutoHide",
                        t,
                        Some("\u{ED1A}"),
                        loc::t("AutoHide"),
                        autohide_dd,
                    ),
                    self.target_row(
                        "AllMonitors",
                        t,
                        Some("\u{E7F4}"),
                        loc::t("AllMonitors"),
                        ios_switch(
                            "sw-allmon",
                            t,
                            self.settings.show_on_all_monitors,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_on_all_monitors =
                                    !this.settings.show_on_all_monitors;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row_desc(
                        "PreviewsEnabled",
                        t,
                        Some("\u{E7C4}"),
                        loc::t("PreviewsEnabled"),
                        if self.settings.previews_enabled {
                            SharedString::from("")
                        } else {
                            SharedString::from(loc::t("PreviewDisabledHint"))
                        },
                        ios_switch(
                            "sw-previews",
                            t,
                            self.settings.previews_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.previews_enabled = !this.settings.previews_enabled;
                                this.auto_save(cx);
                            }),
                        ),
                    ),
                    self.target_row(
                        "WheelVolumeEnabled",
                        t,
                        Some("\u{E767}"),
                        loc::t("WheelVolume"),
                        ios_switch(
                            "sw-wheel-volume",
                            t,
                            self.settings.wheel_volume_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.wheel_volume_enabled =
                                    !this.settings.wheel_volume_enabled;
                                this.auto_save(cx);
                            }),
                        ),
                    ),
                    self.target_row(
                        "FocusMode",
                        t,
                        Some("\u{E708}"),
                        loc::t("FocusMode"),
                        ios_switch(
                            "sw-focus",
                            t,
                            self.settings.focus_mode,
                            cx.listener(|this, _, _, cx| {
                                this.settings.focus_mode = !this.settings.focus_mode;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "RevealDockOnWinKey",
                        t,
                        Some("\u{E765}"),
                        loc::t("RevealDockOnWinKey"),
                        ios_switch(
                            "sw-winkey",
                            t,
                            self.settings.reveal_dock_on_win_key,
                            cx.listener(|this, _, _, cx| {
                                this.settings.reveal_dock_on_win_key =
                                    !this.settings.reveal_dock_on_win_key;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "CurrentDesktopOnly",
                        t,
                        Some("\u{E7C4}"),
                        loc::t("CurrentDesktopOnly"),
                        ios_switch(
                            "sw-vdesk",
                            t,
                            self.settings.current_desktop_only,
                            cx.listener(|this, _, _, cx| {
                                this.settings.current_desktop_only =
                                    !this.settings.current_desktop_only;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("SectionInteractions"),
                vec![
                    self.target_row(
                        "ShowStartButton",
                        t,
                        Some("\u{E8A9}"),
                        loc::t("ShowStartButton"),
                        ios_switch(
                            "sw-start-btn",
                            t,
                            self.settings.show_start_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_start_button = !this.settings.show_start_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "ShowSearchButton",
                        t,
                        Some("\u{E721}"),
                        loc::t("ShowSearchButton"),
                        ios_switch(
                            "sw-search-btn",
                            t,
                            self.settings.show_search_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_search_button =
                                    !this.settings.show_search_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "ShowTaskViewButton",
                        t,
                        Some("\u{E7C4}"),
                        loc::t("ShowTaskViewButton"),
                        ios_switch(
                            "sw-taskview-btn",
                            t,
                            self.settings.show_task_view_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_task_view_button =
                                    !this.settings.show_task_view_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "TrayApps",
                        t,
                        Some("\u{E70E}"),
                        loc::t("TrayApps"),
                        ios_switch(
                            "sw-tray",
                            t,
                            self.settings.show_tray_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_tray_button = !this.settings.show_tray_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "RecycleBin",
                        t,
                        Some("\u{E74D}"),
                        loc::t("RecycleBin"),
                        ios_switch(
                            "sw-bin",
                            t,
                            self.settings.show_recycle_bin,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_recycle_bin = !this.settings.show_recycle_bin;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "WidgetShowDesktop",
                        t,
                        Some("\u{E7F8}"),
                        loc::t("WidgetShowDesktop"),
                        ios_switch(
                            "sw-sd",
                            t,
                            self.settings.show_desktop_button,
                            cx.listener(|this, _, _, cx| {
                                this.settings.show_desktop_button =
                                    !this.settings.show_desktop_button;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "MiddleClickAction",
                        t,
                        Some("\u{E962}"),
                        loc::t("MiddleClickAction"),
                        self.make_dropdown(
                            "dock-mclick",
                            t,
                            [
                                (MiddleClickAction::NewWindow, loc::t("MiddleClickNewWindow")),
                                (MiddleClickAction::CloseWindow, loc::t("MiddleClickClose")),
                                (MiddleClickAction::Nothing, loc::t("MiddleClickNothing")),
                            ]
                            .into_iter()
                            .map(|(act, label)| {
                                let entity = cx.entity().downgrade();
                                DropdownOption {
                                    label: SharedString::from(label),
                                    flag_path: None,
                                    glyph: None,
                                    selected: self.settings.middle_click_action == act,
                                    on_select: std::rc::Rc::new(move |_win, app| {
                                        let _ = entity.update(app, |this, cx| {
                                            this.open_dropdown = None;
                                            this.settings.middle_click_action = act;
                                            this.auto_save(cx);
                                        });
                                    }),
                                }
                            })
                            .collect(),
                            cx,
                        ),
                    ),
                ],
            )
            .into_any_element(),
        ];
        out
    }

    fn render_theme_card(
        &self,
        t: &UiTheme,
        spec: ThemeCardSpec,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ThemeCardSpec {
            id,
            title,
            is_built_in,
            swatches,
            preserve_original_icons,
        } = spec;
        let active = match &id {
            None => self.settings.theme_pack.trim().is_empty(),
            Some(pack_id) => self.settings.theme_pack.eq_ignore_ascii_case(pack_id),
        };
        let card_id = id.as_deref().unwrap_or("windows-default").to_owned();
        let border_col = if active { t.accent } else { t.border };
        let card_hover = t.surface_hover;

        // Mini dock preview bar computed from the theme's 5 swatches
        let mini_dock = div()
            .w_full()
            .h(px(46.0))
            .rounded(px(9.0))
            .bg(rgba(swatches.card.to_gpui()))
            .border_1()
            .border_color(rgba(swatches.border.to_gpui()))
            .flex()
            .items_center()
            .justify_center()
            .px(px(10.0))
            .child(
                div()
                    .h(px(26.0))
                    .px(px(8.0))
                    .rounded(px(8.0))
                    .bg(rgba(swatches.surface.to_gpui()))
                    .border_1()
                    .border_color(rgba(swatches.border.to_gpui()))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .child(
                        div()
                            .size(px(10.0))
                            .rounded(px(3.0))
                            .bg(rgba(swatches.accent.to_gpui())),
                    )
                    .child(
                        div()
                            .size(px(10.0))
                            .rounded(px(3.0))
                            .bg(rgba(swatches.text.with_alpha(0x55).to_gpui())),
                    )
                    .child(
                        div()
                            .size(px(10.0))
                            .rounded(px(3.0))
                            .bg(rgba(swatches.text.with_alpha(0x33).to_gpui())),
                    )
                    .child(
                        div()
                            .w(px(1.0))
                            .h(px(12.0))
                            .bg(rgba(swatches.border.to_gpui())),
                    )
                    .child(
                        div()
                            .px(px(5.0))
                            .h(px(14.0))
                            .rounded(px(4.0))
                            .bg(rgba(swatches.card.to_gpui()))
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .w(px(18.0))
                                    .h(px(3.0))
                                    .rounded(px(1.5))
                                    .bg(rgba(swatches.text.to_gpui())),
                            ),
                    ),
            );

        // 5-color swatch strip
        let swatch_strip = div()
            .w_full()
            .h(px(7.0))
            .rounded(px(3.5))
            .overflow_hidden()
            .border_1()
            .border_color(rgba(t.border))
            .flex()
            .children(
                [
                    swatches.surface,
                    swatches.card,
                    swatches.accent,
                    swatches.text,
                    swatches.border,
                ]
                .into_iter()
                .map(|c| div().flex_1().h_full().bg(rgba(c.to_gpui()))),
            );

        // Direct one-click Apply / Active control
        let apply_control: AnyElement = if active {
            badge(t, format!("✓ {}", loc::t("ThemeActive")), true).into_any_element()
        } else {
            let apply_id = id.clone();
            let apply_title = title.clone();
            compact_card_button(
                SharedString::from(format!("theme-apply-{card_id}")),
                t,
                loc::t("ThemeApply"),
                true,
                cx.listener(move |this, _, win, cx| {
                    this.open_dropdown = None;
                    this.settings.theme_pack = apply_id.clone().unwrap_or_default();
                    if let Some(ref pid) = apply_id
                        && !theme_packs::is_built_in(pid)
                    {
                        this.custom_theme_name = apply_title.clone();
                        let len = this.custom_theme_name.encode_utf16().count();
                        this.custom_theme_name_selection = len..len;
                    } else {
                        this.custom_theme_name.clear();
                        this.custom_theme_name_selection = 0..0;
                    }
                    persist_theme_tokens(&mut this.settings, Map::new());
                    this.apply_theme_change(Some(win), true, cx);
                }),
            )
            .into_any_element()
        };

        // Compact `⋮` action menu for installed theme packs
        let menu_control: Option<AnyElement> = id.as_ref().map(|pack_id| {
            let entity = cx.entity().downgrade();
            let edit_action: DropdownSelectHandler = {
                let entity = entity.clone();
                let pid = pack_id.clone();
                let display_title = title.clone();
                std::rc::Rc::new(move |win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        if is_built_in {
                            let clone_name = loc::t_fmt("ThemeNewEditableName", &[&display_title]);
                            match theme_packs::create_theme_named(
                                &clone_name,
                                None,
                                Some(&pid),
                                true,
                            ) {
                                Ok(cloned) => {
                                    this.settings.theme_pack = cloned.id.clone();
                                    this.custom_theme_name = cloned.display_name().to_owned();
                                    let len = this.custom_theme_name.encode_utf16().count();
                                    this.custom_theme_name_selection = len..len;
                                    this.theme_status = loc::t("ThemePackCreated");
                                    this.apply_theme_change(Some(win), true, cx);
                                }
                                Err(err) => {
                                    this.theme_status = loc::t_fmt("ThemeCreationFailed", &[&err]);
                                    cx.notify();
                                }
                            }
                        } else {
                            this.settings.theme_pack = pid.clone();
                            this.custom_theme_name = display_title.clone();
                            let len = this.custom_theme_name.encode_utf16().count();
                            this.custom_theme_name_selection = len..len;
                            this.apply_theme_change(Some(win), true, cx);
                        }
                        this.custom_theme_name_focus.focus(win);
                    });
                })
            };
            let duplicate_action: DropdownSelectHandler = {
                let entity = entity.clone();
                let pid = pack_id.clone();
                std::rc::Rc::new(move |_win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        this.theme_status =
                            match theme_packs::create_theme(Some(&pid), preserve_original_icons) {
                                Ok(_) => loc::t("ThemePackDuplicated"),
                                Err(err) => loc::t_fmt("ThemeCreationFailed", &[&err]),
                            };
                        cx.notify();
                    });
                })
            };
            let export_action: DropdownSelectHandler = {
                let entity = entity.clone();
                let pid = pack_id.clone();
                std::rc::Rc::new(move |_win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        this.export_theme(&pid, cx);
                    });
                })
            };
            let mut options = vec![
                DropdownOption {
                    label: SharedString::from(loc::t("Edit")),
                    flag_path: None,
                    glyph: Some("\u{E70F}"),
                    selected: false,
                    on_select: edit_action,
                },
                DropdownOption {
                    label: SharedString::from(loc::t("ThemePackDuplicate")),
                    flag_path: None,
                    glyph: Some("\u{E8C8}"),
                    selected: false,
                    on_select: duplicate_action,
                },
                DropdownOption {
                    label: SharedString::from(loc::t("ThemePackExport")),
                    flag_path: None,
                    glyph: Some("\u{EDE1}"),
                    selected: false,
                    on_select: export_action,
                },
            ];
            if !is_built_in {
                let pid = pack_id.clone();
                let delete_action: DropdownSelectHandler = std::rc::Rc::new(move |win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        this.delete_theme(&pid, Some(win), cx);
                    });
                });
                options.push(DropdownOption {
                    label: SharedString::from(loc::t("ThemePackDelete")),
                    flag_path: None,
                    glyph: Some("\u{E74D}"),
                    selected: false,
                    on_select: delete_action,
                });
            }
            self.make_icon_action_menu(format!("theme-actions-{pack_id}"), t, options, cx)
                .into_any_element()
        });

        div()
            .id(SharedString::from(format!("theme-card-{card_id}")))
            .flex_1()
            .min_w_0()
            .p(px(11.0))
            .rounded(px(11.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(border_col))
            .hover(move |s| s.bg(rgba(card_hover)))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(mini_dock)
            .child(swatch_strip)
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(12.5)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(t.text_primary))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(fonts::scaled_size(10.0)))
                                    .text_color(rgba(t.text_secondary))
                                    .child(loc::t(if is_built_in {
                                        "ThemeBuiltInBadge"
                                    } else {
                                        "ThemeCustomBadge"
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .child(apply_control)
                            .when_some(menu_control, |row, menu| row.child(menu)),
                    ),
            )
            .into_any_element()
    }

    fn render_themes(
        &self,
        t: &UiTheme,
        is_dark: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let effective_theme = if is_dark {
            AppTheme::Dark
        } else {
            AppTheme::Light
        };

        // ---------------------------------------------------------------------
        // SECTION 1 (AT THE VERY TOP): Theme Presets Gallery (`ThemePack`)
        // ---------------------------------------------------------------------
        let mut gallery_rows: Vec<AnyElement> = Vec::new();
        let mut action_bar = div()
            .w_full()
            .px(px(16.0))
            .py(px(10.0))
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.0))
                    .child(button(
                        "btn-theme-new",
                        t,
                        format!("+ {}", loc::t("ThemePackNew")),
                        ButtonVariant::Primary,
                        cx.listener(|this, _, win, cx| {
                            this.custom_theme_name = loc::t("ThemePackNewDefault");
                            let len = this.custom_theme_name.encode_utf16().count();
                            this.custom_theme_name_selection = len..len;
                            this.custom_theme_name_focus.focus(win);
                            cx.notify();
                        }),
                    ))
                    .child(button(
                        "btn-theme-import",
                        t,
                        loc::t("ThemePackImport"),
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| this.import_theme(cx)),
                    ))
                    .child(button(
                        "btn-theme-folder",
                        t,
                        loc::t("ThemePackOpenFolder"),
                        ButtonVariant::Secondary,
                        cx.listener(|_, _, _, _| {
                            pins::launch(Self::themes_root().to_string_lossy().into_owned());
                        }),
                    )),
            );
        if !self.theme_status.is_empty() {
            action_bar = action_bar.child(badge(t, self.theme_status.clone(), true));
        }
        gallery_rows.push(
            self.mark_target("ThemePack", t, action_bar)
                .into_any_element(),
        );

        let all_packs = theme_packs::discover(&Self::themes_root());
        let (built_in_packs, custom_packs): (Vec<_>, Vec<_>) = all_packs
            .into_iter()
            .partition(|pack| theme_packs::is_built_in(&pack.id));

        let stock_pal = crate::design::palette(is_dark);
        let win_accent = crate::design::windows_palette::read_for(is_dark)
            .tokens()
            .get("AccentBrush")
            .and_then(crate::design::theme::token_rgba)
            .unwrap_or_else(|| {
                TokenRgba::from_rgb(stock_pal.accent.0, stock_pal.accent.1, stock_pal.accent.2)
            });
        let win_default_swatches = ThemePreviewSwatches {
            surface: TokenRgba::from_rgb(
                stock_pal.dock_pill.0,
                stock_pal.dock_pill.1,
                stock_pal.dock_pill.2,
            ),
            card: TokenRgba::from_rgb(
                stock_pal.elevated.0,
                stock_pal.elevated.1,
                stock_pal.elevated.2,
            ),
            accent: win_accent,
            text: TokenRgba::from_rgb(
                stock_pal.text_primary.0,
                stock_pal.text_primary.1,
                stock_pal.text_primary.2,
            ),
            border: TokenRgba::from_rgb(
                stock_pal.border_rgb.0,
                stock_pal.border_rgb.1,
                stock_pal.border_rgb.2,
            ),
        };

        let mut theme_cards: Vec<AnyElement> = Vec::new();
        theme_cards.push(self.render_theme_card(
            t,
            ThemeCardSpec {
                id: None,
                title: loc::t("ThemeWindowsAppearance"),
                is_built_in: true,
                swatches: win_default_swatches,
                preserve_original_icons: true,
            },
            cx,
        ));
        for pack in built_in_packs.into_iter().chain(custom_packs) {
            let swatches = theme_packs::preview_swatches(&pack, effective_theme);
            let is_built_in = theme_packs::is_built_in(&pack.id);
            let title = pack.display_name().to_owned();
            let preserve_original_icons = pack.manifest.preserve_original_icons;
            theme_cards.push(self.render_theme_card(
                t,
                ThemeCardSpec {
                    id: Some(pack.id),
                    title,
                    is_built_in,
                    swatches,
                    preserve_original_icons,
                },
                cx,
            ));
        }

        let mut grid = div()
            .w_full()
            .px(px(16.0))
            .py(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0));
        let mut iter = theme_cards.into_iter();
        while let Some(first) = iter.next() {
            let mut row = div().w_full().flex().gap(px(10.0)).child(first);
            if let Some(second) = iter.next() {
                row = row.child(second);
            } else {
                row = row.child(div().flex_1().min_w_0());
            }
            grid = grid.child(row);
        }
        gallery_rows.push(grid.into_any_element());

        let mut out: Vec<AnyElement> = vec![
            section_card(t, loc::t("ThemePack"), gallery_rows).into_any_element(),
            // -----------------------------------------------------------------
            // SECTION 2: Live Theme Studio & Custom Theme Creator
            // -----------------------------------------------------------------
            self.render_theme_studio(t, is_dark, window, cx),
        ];

        // ---------------------------------------------------------------------
        // SECTION 3: Color Mode & Glass Optics
        // ---------------------------------------------------------------------
        let theme_dd = self.make_dropdown(
            "themes-theme",
            t,
            [
                (loc::t("ThemeWindowsAppearance"), AppTheme::System),
                (loc::t("Dark"), AppTheme::Dark),
                (loc::t("Light"), AppTheme::Light),
            ]
            .into_iter()
            .map(|(label, th)| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected: self.settings.theme == th,
                    on_select: std::rc::Rc::new(move |win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.theme = th;
                            this.apply_theme_change(Some(win), false, cx);
                        });
                    }),
                }
            })
            .collect(),
            cx,
        );

        let radius_label = self.settings.dock_corner_radius.map_or_else(
            || loc::t("Automatic"),
            |r| loc::t_fmt("PixelsValue", &[&r.to_string()]),
        );

        let mut color_mode_rows: Vec<AnyElement> = vec![
            self.target_row("Theme", t, Some("\u{E790}"), loc::t("Theme"), theme_dd),
            self.target_row(
                "ThemeApplicationIcons",
                t,
                Some("\u{E91B}"),
                loc::t("ThemeSectionIcons"),
                ios_switch(
                    "sw-theme-app-icons",
                    t,
                    self.settings.theme_application_icons,
                    cx.listener(|this, _, win, cx| {
                        this.settings.theme_application_icons =
                            !this.settings.theme_application_icons;
                        this.apply_theme_change(Some(win), false, cx);
                    }),
                ),
            ),
            self.target_row(
                "DockCornerRadius",
                t,
                Some("\u{F158}"),
                loc::t("DockCornerRadius"),
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(live_slider(
                        "rad",
                        t,
                        self.settings.dock_corner_radius.unwrap_or(16) as f32 / 32.0,
                        17,
                        radius_label,
                        |i| {
                            let entity = cx.entity().downgrade();
                            std::rc::Rc::new(move |_win, app| {
                                let _ = entity.update(app, |this, cx| {
                                    let next = Some(((i as i32) * 2).clamp(0, 32));
                                    if this.settings.dock_corner_radius != next {
                                        this.settings.dock_corner_radius = next;
                                        this.auto_save(cx);
                                    }
                                });
                            })
                        },
                    ))
                    .when(self.settings.dock_corner_radius.is_some(), |row| {
                        row.child(compact_card_button(
                            "btn-radius-auto",
                            t,
                            loc::t("Automatic"),
                            false,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dock_corner_radius = None;
                                this.auto_save(cx);
                            }),
                        ))
                    }),
            ),
        ];

        let any_glass = self.settings.dock_background == SurfaceBackground::Glass
            || self.settings.dynamic_island_background == SurfaceBackground::Glass
            || self.settings.menu_background == SurfaceBackground::Glass
            || self.settings.widget_background == SurfaceBackground::Glass
            || self.settings.settings_background == SurfaceBackground::Glass;
        if any_glass {
            let blur_px = self.settings.glass_blur_sigma.clamp(0.0, 36.0).round() as i32;
            let sat_pct = (self.settings.glass_saturation.clamp(0.8, 1.8) * 100.0).round() as i32;
            let edge_pct = (self.settings.glass_edge_glow.clamp(0.0, 1.0) * 100.0).round() as i32;
            color_mode_rows.extend([
                self.target_row(
                    "GlassBlurLabel",
                    t,
                    Some("\u{EB42}"),
                    loc::t("GlassBlurLabel"),
                    live_slider(
                        "glass-blur",
                        t,
                        (self.settings.glass_blur_sigma.clamp(0.0, 36.0) as f32) / 36.0,
                        37,
                        loc::t_fmt("PixelsValue", &[&blur_px.to_string()]),
                        |i| {
                            let entity = cx.entity().downgrade();
                            std::rc::Rc::new(move |win, app| {
                                let _ = entity.update(app, |this, cx| {
                                    let next = (i as f64).clamp(0.0, 36.0);
                                    if (this.settings.glass_blur_sigma - next).abs() > 0.01 {
                                        this.settings.glass_blur_sigma = next;
                                        this.sync_settings_glass(win);
                                        this.auto_save(cx);
                                    }
                                });
                            })
                        },
                    ),
                ),
                self.target_row(
                    "GlassSaturationLabel",
                    t,
                    Some("\u{E771}"),
                    loc::t("GlassSaturationLabel"),
                    live_slider(
                        "glass-sat",
                        t,
                        ((self.settings.glass_saturation.clamp(0.8, 1.8) as f32) - 0.8) / 1.0,
                        26,
                        loc::t_fmt("OpacityPercent", &[&sat_pct.to_string()]),
                        |i| {
                            let entity = cx.entity().downgrade();
                            std::rc::Rc::new(move |win, app| {
                                let _ = entity.update(app, |this, cx| {
                                    let next = (0.80 + (i as f64) * 0.04).clamp(0.80, 1.80);
                                    if (this.settings.glass_saturation - next).abs() > 0.001 {
                                        this.settings.glass_saturation = next;
                                        this.sync_settings_glass(win);
                                        this.auto_save(cx);
                                    }
                                });
                            })
                        },
                    ),
                ),
                self.target_row(
                    "GlassEdgeGlowLabel",
                    t,
                    Some("\u{E706}"),
                    loc::t("GlassEdgeGlowLabel"),
                    live_slider(
                        "glass-edge",
                        t,
                        self.settings.glass_edge_glow.clamp(0.0, 1.0) as f32,
                        26,
                        loc::t_fmt("OpacityPercent", &[&edge_pct.to_string()]),
                        |i| {
                            let entity = cx.entity().downgrade();
                            std::rc::Rc::new(move |win, app| {
                                let _ = entity.update(app, |this, cx| {
                                    let next = ((i as f64) * 0.04).clamp(0.0, 1.0);
                                    if (this.settings.glass_edge_glow - next).abs() > 0.001 {
                                        this.settings.glass_edge_glow = next;
                                        this.sync_settings_glass(win);
                                        this.auto_save(cx);
                                    }
                                });
                            })
                        },
                    ),
                ),
                self.target_row(
                    "GlassPointerLightLabel",
                    t,
                    Some("\u{E8B0}"),
                    loc::t("GlassPointerLightLabel"),
                    ios_switch(
                        "sw-glass-ptr",
                        t,
                        self.settings.glass_pointer_light,
                        cx.listener(|this, _, win, cx| {
                            this.settings.glass_pointer_light = !this.settings.glass_pointer_light;
                            this.sync_settings_glass(win);
                            this.auto_save(cx);
                        }),
                    ),
                ),
            ]);
        }
        out.push(section_card(t, loc::t("SectionTheme"), color_mode_rows).into_any_element());

        // ---------------------------------------------------------------------
        // SECTION 4: Consolidated Surfaces & Borders (ZERO duplication!)
        // Preserves ALL search targets: DockBackground, DockOpacity, SurfaceDock/Menu/Widget/Settings,
        // SurfaceOpacityLabel:Dock/Menu/Widget/Settings/Surfaces.
        // ---------------------------------------------------------------------
        let mut surface_rows: Vec<AnyElement> = vec![
            self.mark_target(
                "DockBackground",
                t,
                div().child(self.surface_row(
                    t,
                    cx,
                    "surf-dock",
                    "SurfaceDock",
                    loc::t("SurfaceDock"),
                    self.settings.dock_background,
                    |settings, value| settings.dock_background = value,
                )),
            )
            .into_any_element(),
            self.surface_row(
                t,
                cx,
                "surf-menu",
                "SurfaceMenu",
                loc::t("SurfaceMenu"),
                self.settings.menu_background,
                |settings, value| settings.menu_background = value,
            ),
            self.surface_row(
                t,
                cx,
                "surf-widget",
                "SurfaceWidget",
                loc::t("SurfaceWidget"),
                self.settings.widget_background,
                |settings, value| settings.widget_background = value,
            ),
            self.surface_row(
                t,
                cx,
                "surf-settings",
                "SurfaceSettings",
                loc::t("SurfaceSettings"),
                self.settings.settings_background,
                |settings, value| settings.settings_background = value,
            ),
            self.mark_target(
                "DockOpacity",
                t,
                div().child(self.surface_opacity_row(
                    t,
                    cx,
                    "SurfaceOpacityLabel:Dock",
                    "surf-op-dock",
                    SurfaceKind::Dock,
                    format!(
                        "{} · {}",
                        loc::t("SurfaceDock"),
                        loc::t("SurfaceOpacityLabel")
                    ),
                    self.settings.dock_background,
                    self.settings.dock_opacity,
                )),
            )
            .into_any_element(),
            self.mark_target(
                "SurfaceOpacityLabel:Surfaces",
                t,
                div().child(self.surface_opacity_row(
                    t,
                    cx,
                    "SurfaceOpacityLabel:Menu",
                    "surf-op-menu",
                    SurfaceKind::Menu,
                    format!(
                        "{} · {}",
                        loc::t("SurfaceMenu"),
                        loc::t("SurfaceOpacityLabel")
                    ),
                    self.settings.menu_background,
                    self.settings.menu_opacity,
                )),
            )
            .into_any_element(),
            self.surface_opacity_row(
                t,
                cx,
                "SurfaceOpacityLabel:Widget",
                "surf-op-widget",
                SurfaceKind::Widget,
                format!(
                    "{} · {}",
                    loc::t("SurfaceWidget"),
                    loc::t("SurfaceOpacityLabel")
                ),
                self.settings.widget_background,
                self.settings.widget_opacity,
            ),
            self.surface_opacity_row(
                t,
                cx,
                "SurfaceOpacityLabel:Settings",
                "surf-op-settings",
                SurfaceKind::Settings,
                format!(
                    "{} · {}",
                    loc::t("SurfaceSettings"),
                    loc::t("SurfaceOpacityLabel")
                ),
                self.settings.settings_background,
                self.settings.settings_opacity,
            ),
        ];

        let border_surfaces = [
            (
                "border-dock",
                loc::t("SurfaceDock"),
                self.settings.dock_border_thickness,
                self.settings.dock_border_color.clone(),
                0usize,
            ),
            (
                "border-menu",
                loc::t("SurfaceMenu"),
                self.settings.menu_border_thickness,
                self.settings.menu_border_color.clone(),
                1usize,
            ),
            (
                "border-widget",
                loc::t("SurfaceWidget"),
                self.settings.widget_border_thickness,
                self.settings.widget_border_color.clone(),
                2usize,
            ),
            (
                "border-settings",
                loc::t("SurfaceSettings"),
                self.settings.settings_border_thickness,
                self.settings.settings_border_color.clone(),
                3usize,
            ),
        ];
        for (id, label, thickness, color, index) in border_surfaces {
            let step = stepper(
                SharedString::from(format!("{id}-minus")),
                SharedString::from(format!("{id}-plus")),
                t,
                format!("{thickness:.1}"),
                cx.listener(move |this, _, _, cx| {
                    let next = match index {
                        0 => this.settings.dock_border_thickness,
                        1 => this.settings.menu_border_thickness,
                        2 => this.settings.widget_border_thickness,
                        _ => this.settings.settings_border_thickness,
                    } - 0.5;
                    set_border_thickness(&mut this.settings, index, next);
                    this.auto_save(cx);
                }),
                cx.listener(move |this, _, _, cx| {
                    let next = match index {
                        0 => this.settings.dock_border_thickness,
                        1 => this.settings.menu_border_thickness,
                        2 => this.settings.widget_border_thickness,
                        _ => this.settings.settings_border_thickness,
                    } + 0.5;
                    set_border_thickness(&mut this.settings, index, next);
                    this.auto_save(cx);
                }),
            );
            let has_custom_color = color.is_some();
            let color_id = SharedString::from(format!("{id}-color"));
            let swatch_rgba = color
                .as_deref()
                .and_then(parse_hex_color)
                .unwrap_or(t.border);
            let color_hover = t.surface_hover;
            let color_btn = div()
                .id(color_id)
                .h(px(30.0))
                .px(px(9.0))
                .rounded(px(8.0))
                .bg(rgba(t.surface_muted))
                .border_1()
                .border_color(rgba(t.border))
                .flex()
                .items_center()
                .gap(px(7.0))
                .cursor_pointer()
                .hover(move |s| s.bg(rgba(color_hover)))
                .active(|s| s.opacity(0.86))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit_border_color(index, cx);
                }))
                .child(
                    div()
                        .w(px(16.0))
                        .h(px(16.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgba(t.border))
                        .bg(rgba(swatch_rgba)),
                )
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(11.5)))
                        .text_color(rgba(t.text_primary))
                        .child(color.unwrap_or_else(|| loc::t("Automatic"))),
                );
            surface_rows.push(
                setting_row(
                    t,
                    Some("\u{E790}"),
                    format!("{label} · {}", loc::t("BorderStyleLabel")),
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(step)
                        .child(color_btn)
                        .when(has_custom_color, |row| {
                            row.child(compact_card_button(
                                SharedString::from(format!("{id}-color-clear")),
                                t,
                                "×",
                                false,
                                cx.listener(move |this, _, win, cx| {
                                    this.reset_border_picker_color(index, win, cx);
                                }),
                            ))
                        }),
                )
                .into_any_element(),
            );
        }
        out.push(section_card(t, loc::t("SectionSurfaces"), surface_rows).into_any_element());

        // ---------------------------------------------------------------------
        // SECTION 5: Typography
        // ---------------------------------------------------------------------
        out.push(
            section_card(
                t,
                loc::t("ThemeSectionTypography"),
                vec![
                    {
                        let mut font_options: Vec<DropdownOption> =
                            Vec::with_capacity(self.installed_fonts.len() + 1);
                        let native_entity = cx.entity().downgrade();
                        font_options.push(DropdownOption {
                            label: SharedString::from(loc::t_fmt(
                                "FontNativeWithWindows",
                                &[fonts::native_ui_face()],
                            )),
                            flag_path: None,
                            glyph: None,
                            selected: self.settings.ui_font_family.trim().is_empty(),
                            on_select: std::rc::Rc::new(move |_win, app| {
                                let _ = native_entity.update(app, |this, cx| {
                                    this.open_dropdown = None;
                                    this.clear_picker_state("ui-font-family");
                                    this.settings.ui_font_family.clear();
                                    fonts::set_selected_family("");
                                    this.auto_save(cx);
                                });
                            }),
                        });
                        for name in &self.installed_fonts {
                            let selected = self.settings.ui_font_family.eq_ignore_ascii_case(name);
                            let family = name.clone();
                            let entity = cx.entity().downgrade();
                            font_options.push(DropdownOption {
                                label: SharedString::from(name.clone()),
                                flag_path: None,
                                glyph: None,
                                selected,
                                on_select: std::rc::Rc::new(move |_win, app| {
                                    let family = family.clone();
                                    let _ = entity.update(app, |this, cx| {
                                        this.open_dropdown = None;
                                        this.clear_picker_state("ui-font-family");
                                        this.settings.ui_font_family = family.clone();
                                        fonts::set_selected_family(&family);
                                        this.auto_save(cx);
                                    });
                                }),
                            });
                        }
                        setting_row(
                            t,
                            Some("\u{E8D3}"),
                            loc::t("UiFontFamily"),
                            self.make_searchable_dropdown(
                                "ui-font-family",
                                t,
                                &self.font_picker_focus,
                                &font_options,
                                cx,
                            ),
                        )
                        .into_any_element()
                    },
                    setting_row(
                        t,
                        Some("\u{E8D3}"),
                        loc::t("UiFontSize"),
                        live_slider(
                            "ui-font-size",
                            t,
                            (self.settings.ui_font_size - 9.0) / 15.0,
                            16,
                            loc::t_fmt(
                                "PixelsValue",
                                &[&(self.settings.ui_font_size.round() as i32).to_string()],
                            ),
                            |i| {
                                let entity = cx.entity().downgrade();
                                std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        let next = (9.0 + i as f32).clamp(9.0, 24.0);
                                        if (this.settings.ui_font_size - next).abs() > 0.01 {
                                            this.settings.ui_font_size = next;
                                            fonts::apply_settings(&this.settings);
                                            this.auto_save(cx);
                                        }
                                    });
                                })
                            },
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        );

        // ---------------------------------------------------------------------
        // SECTION 6: Animations (all 5 controls intact)
        // ---------------------------------------------------------------------
        out.push(
            section_card(
                t,
                loc::t("Animations"),
                vec![
                    self.target_row(
                        "Animations",
                        t,
                        Some("\u{E945}"),
                        loc::t("Animations"),
                        ios_switch(
                            "sw-anim",
                            t,
                            self.settings.animations_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.animations_enabled =
                                    !this.settings.animations_enabled;
                                this.auto_save(cx);
                            }),
                        ),
                    ),
                    dim_when(
                        !self.settings.animations_enabled,
                        self.target_row(
                            "Magnification",
                            t,
                            Some("\u{E71E}"),
                            loc::t("Magnification"),
                            ios_switch(
                                "sw-mag",
                                t,
                                self.settings.magnification_enabled,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.magnification_enabled =
                                        !this.settings.magnification_enabled;
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        !self.settings.magnification_enabled,
                        self.target_row_desc(
                            "MagnificationExpandsDock",
                            t,
                            Some("\u{E71E}"),
                            loc::t("MagnificationExpandsDock"),
                            loc::t("MagnificationExpandsDockDescription"),
                            ios_switch(
                                "sw-mag-expands-dock",
                                t,
                                self.settings.magnification_expands_dock,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.magnification_expands_dock =
                                        !this.settings.magnification_expands_dock;
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        !self.settings.animations_enabled || !self.settings.magnification_enabled,
                        self.target_row(
                            "FisheyeIntensity",
                            t,
                            Some("\u{E9E9}"),
                            loc::t("FisheyeIntensity"),
                            self.make_dropdown(
                                "themes-fisheye",
                                t,
                                [
                                    (FisheyeIntensity::Low, loc::t("FisheyeLow")),
                                    (FisheyeIntensity::Normal, loc::t("FisheyeNormal")),
                                    (FisheyeIntensity::Strong, loc::t("FisheyeStrong")),
                                ]
                                .into_iter()
                                .map(|(level, label)| {
                                    let entity = cx.entity().downgrade();
                                    DropdownOption {
                                        label: SharedString::from(label),
                                        flag_path: None,
                                        glyph: None,
                                        selected: self.settings.fisheye_intensity == level,
                                        on_select: std::rc::Rc::new(move |_win, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                this.open_dropdown = None;
                                                this.settings.fisheye_intensity = level;
                                                this.auto_save(cx);
                                            });
                                        }),
                                    }
                                })
                                .collect(),
                                cx,
                            ),
                        ),
                    )
                    .into_any_element(),
                    {
                        let max_hz = win32::max_connected_refresh_hz().max(15);
                        let max_step = (max_hz - 15 + 1) as usize;
                        let current_step = match self.settings.animation_fps {
                            Some(fps) => (fps.clamp(15, max_hz) - 15) as usize,
                            None => max_step,
                        };
                        let fps_label = match self.settings.animation_fps {
                            Some(fps) => loc::t_fmt(
                                "AnimationFpsValue",
                                &[&fps.clamp(15, max_hz).to_string()],
                            ),
                            None => format!("{} ({max_hz} Hz)", loc::t("AnimationFpsAuto")),
                        };
                        let fps_entity = cx.entity().downgrade();
                        dim_when(
                            !self.settings.animations_enabled,
                            self.target_row(
                                "AnimationFps",
                                t,
                                Some("\u{EC4A}"),
                                loc::t("AnimationFps"),
                                live_slider(
                                    "anim-fps",
                                    t,
                                    current_step as f32 / max_step.max(1) as f32,
                                    max_step + 1,
                                    fps_label,
                                    move |i| {
                                        let entity = fps_entity.clone();
                                        std::rc::Rc::new(move |_win, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                let next = if i >= max_step {
                                                    None
                                                } else {
                                                    Some((15 + i as u32).min(max_hz))
                                                };
                                                if this.settings.animation_fps != next {
                                                    this.settings.animation_fps = next;
                                                    this.auto_save(cx);
                                                }
                                            });
                                        })
                                    },
                                ),
                            ),
                        )
                        .into_any_element()
                    },
                ],
            )
            .into_any_element(),
        );

        // ---------------------------------------------------------------------
        // SECTION 7: OLED & Display Care (all 8 controls intact)
        // ---------------------------------------------------------------------
        let threshold_options: Vec<(String, i32)> = [0, 10, 20, 30, 50]
            .into_iter()
            .map(|value| {
                let label = if value == 0 {
                    format!("{} · 0%", loc::t("WirelessBatteryAlertOff"))
                } else {
                    loc::t_fmt("OpacityPercent", &[&value.to_string()])
                };
                (label, value)
            })
            .collect();
        let fps_cap_options: Vec<(String, i32)> = [0, 30, 60, 120]
            .into_iter()
            .map(|value| {
                let label = if value == 0 {
                    loc::t("FpsUnlimited")
                } else {
                    loc::t_fmt("AnimationFpsValue", &[&value.to_string()])
                };
                (label, value)
            })
            .collect();
        let shift_minutes = self.settings.burn_in_shift_minutes.clamp(5, 120);
        out.push(
            section_card(
                t,
                loc::t("SectionDisplayCare"),
                vec![
                    self.mark_target(
                        "SurfaceTrueBlack",
                        t,
                        div()
                            .w_full()
                            .px(px(16.0))
                            .pt(px(10.0))
                            .pb(px(12.0))
                            .text_size(px(fonts::scaled_size(12.0)))
                            .text_color(rgba(t.text_primary))
                            .child(loc::t("SurfaceTrueBlack")),
                    )
                    .into_any_element(),
                    setting_row_desc(
                        t,
                        Some("\u{E706}"),
                        loc::t("OledReduceGlow"),
                        loc::t("OledReduceGlowHelp"),
                        ios_switch(
                            "sw-oled-reduce-glow",
                            t,
                            self.settings.oled_reduce_glow_enabled,
                            cx.listener(|this, _, win, cx| {
                                this.settings.oled_reduce_glow_enabled =
                                    !this.settings.oled_reduce_glow_enabled;
                                this.sync_settings_glass(win);
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row_desc(
                        t,
                        Some("\u{EC4A}"),
                        loc::t("BurnInShift"),
                        loc::t("BurnInShiftHelp"),
                        ios_switch(
                            "sw-burn-in-shift",
                            t,
                            self.settings.burn_in_shift_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.burn_in_shift_enabled =
                                    !this.settings.burn_in_shift_enabled;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        !self.settings.burn_in_shift_enabled,
                        setting_row_desc(
                            t,
                            Some("\u{EC4A}"),
                            loc::t("BurnInShiftInterval"),
                            loc::t("BurnInShiftHelp"),
                            live_slider(
                                "burn-in-interval",
                                t,
                                (shift_minutes - 5) as f32 / 115.0,
                                116,
                                loc::t_fmt("MinutesShort", &[&shift_minutes.to_string()]),
                                |i| {
                                    let entity = cx.entity().downgrade();
                                    std::rc::Rc::new(move |_win, app| {
                                        let _ = entity.update(app, |this, cx| {
                                            let next = (5 + i as i32).clamp(5, 120);
                                            if this.settings.burn_in_shift_minutes != next {
                                                this.settings.burn_in_shift_minutes = next;
                                                this.auto_save(cx);
                                            }
                                        });
                                    })
                                },
                            ),
                        ),
                    )
                    .into_any_element(),
                    setting_row_desc(
                        t,
                        Some("\u{E81E}"),
                        loc::t("OledAutoTrigger"),
                        loc::t("OledAutoTriggerHelp"),
                        ios_switch(
                            "sw-oled-auto-trigger",
                            t,
                            self.settings.oled_auto_trigger_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.oled_auto_trigger_enabled =
                                    !this.settings.oled_auto_trigger_enabled;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        !self.settings.oled_auto_trigger_enabled,
                        setting_row_desc(
                            t,
                            Some("\u{E81E}"),
                            loc::t("OledLowBatteryThreshold"),
                            loc::t("OledAutoTriggerHelp"),
                            self.enum_dropdown(
                                "oled-low-battery-threshold",
                                t,
                                threshold_options,
                                self.settings.oled_low_battery_threshold,
                                |this, value, cx| {
                                    this.settings.oled_low_battery_threshold = value;
                                    this.auto_save(cx);
                                },
                                cx,
                            ),
                        ),
                    )
                    .into_any_element(),
                    setting_row_desc(
                        t,
                        Some("\u{E706}"),
                        loc::t("OledAutoOnNightLight"),
                        loc::t("OledAutoOnNightLightHelp"),
                        ios_switch(
                            "sw-oled-night-light",
                            t,
                            self.settings.oled_auto_on_night_light,
                            cx.listener(|this, _, _, cx| {
                                this.settings.oled_auto_on_night_light =
                                    !this.settings.oled_auto_on_night_light;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row_desc(
                        t,
                        Some("\u{E81E}"),
                        loc::t("AmbientMode"),
                        loc::t("AmbientModeHelp"),
                        ios_switch(
                            "sw-ambient-mode",
                            t,
                            self.settings.ambient_mode_enabled,
                            cx.listener(|this, _, _, cx| {
                                this.settings.ambient_mode_enabled =
                                    !this.settings.ambient_mode_enabled;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    setting_row_desc(
                        t,
                        Some("\u{E945}"),
                        loc::t("AnimationFpsCap"),
                        loc::t("AnimationFpsCapHelp"),
                        self.enum_dropdown(
                            "animation-fps-cap",
                            t,
                            fps_cap_options,
                            self.settings.animation_fps_cap,
                            |this, value, cx| {
                                this.settings.animation_fps_cap = value;
                                this.auto_save(cx);
                            },
                            cx,
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        );
        out
    }

    fn widget_summary(&self, id: &str) -> Option<String> {
        match id {
            "clock" => Some(format!(
                "{} · {}",
                short_clock_format_label(&loc::t(if self.settings.clock_24_hour {
                    "ClockFormat24Hour"
                } else {
                    "ClockFormat12Hour"
                })),
                match self.settings.clock_date_layout {
                    ClockDateLayout::Beside => loc::t("ClockDateLayoutBeside"),
                    ClockDateLayout::Below => loc::t("ClockDateLayoutBelow"),
                }
            )),
            "weather" => {
                let city = self
                    .settings
                    .weather_city
                    .split(", ")
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(weather_fallback_city());
                let unit = match self.settings.weather_unit {
                    TemperatureUnit::Celsius => "°C",
                    TemperatureUnit::Fahrenheit => "°F",
                };
                Some(format!("{city} · {unit}"))
            }
            "stats" => {
                let mut parts = Vec::new();
                if self.settings.stats_meters & 1 != 0 {
                    parts.push("CPU");
                }
                if self.settings.stats_meters & 2 != 0 {
                    parts.push("RAM");
                }
                if self.settings.stats_meters & 16 != 0 {
                    parts.push("GPU");
                }
                if self.settings.stats_meters & 32 != 0 {
                    parts.push(match self.settings.weather_unit {
                        TemperatureUnit::Celsius => "GPU °C",
                        TemperatureUnit::Fahrenheit => "GPU °F",
                    });
                }
                Some(if parts.is_empty() {
                    "—".to_owned()
                } else {
                    parts.join(" · ")
                })
            }
            "bluetoothbattery" => Some(if self.settings.wireless_battery_low_threshold == 0 {
                loc::t("WirelessBatteryAlertOff")
            } else {
                format!("≤ {}%", self.settings.wireless_battery_low_threshold)
            }),
            id if id.starts_with("aiusage_") => {
                let probe = widgets::telemetry::ai_auth_probe(id);
                Some(if probe.env_override {
                    loc::t("AiUsageEnvActive")
                } else if probe.has_token {
                    loc::t("AiUsageAccountConnected")
                } else {
                    loc::t("AiUsageAccountSignInRequired")
                })
            }
            _ => None,
        }
    }

    fn render_widget_shared_drawer_rows(
        &self,
        wid: &'static str,
        t: &UiTheme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows: Vec<AnyElement> = Vec::new();
        let canonical_id = settings::canonical_widget_setting_id(wid);
        let current_density = read_density_override(&self.settings, &canonical_id);
        let canonical_for_write = canonical_id.clone();
        let density_control = self.enum_dropdown(
            format!("w-density-{wid}"),
            t,
            vec![
                (loc::t("WidgetDensityAuto"), WidgetDensityMode::Auto),
                (loc::t("WidgetDensityCompact"), WidgetDensityMode::Compact),
                (loc::t("WidgetDensityFull"), WidgetDensityMode::Full),
            ],
            current_density,
            move |this, value, cx| {
                write_density_override(&mut this.settings, &canonical_for_write, value);
                this.auto_save(cx);
            },
            cx,
        );
        let density_target = match wid {
            "clock" => Some("WidgetDensityOverrideClock"),
            "weather" => Some("WidgetDensityOverrideWeather"),
            "stats" => Some("WidgetDensityOverrideStats"),
            _ => None,
        };
        rows.push(if let Some(target_id) = density_target {
            self.target_row(
                target_id,
                t,
                Some("\u{E80A}"),
                loc::t("WidgetDensity"),
                density_control,
            )
        } else {
            setting_row(
                t,
                Some("\u{E80A}"),
                loc::t("WidgetDensity"),
                density_control,
            )
            .into_any_element()
        });

        let monitors = win32::enumerate_monitors();
        let current_placement = self
            .settings
            .dynamic_island_placements
            .iter()
            .find(|(k, _)| dynamic_island::same_widget_id(k, wid))
            .map(|(_, v)| v.clone());
        let mut choices: Vec<(Option<String>, String)> = vec![
            (None, loc::t("DynamicIslandDock")),
            (
                Some(settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned()),
                loc::t("DynamicIslandAllDisplays"),
            ),
        ];
        for mon in &monitors {
            choices.push((Some(mon.device_name.clone()), mon.display_label()));
        }
        let place_opts: Vec<DropdownOption> = choices
            .into_iter()
            .map(|(target, label)| {
                let selected = match (&current_placement, &target) {
                    (None, None) => true,
                    (Some(cur), Some(tgt)) => cur.eq_ignore_ascii_case(tgt),
                    _ => false,
                };
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(label),
                    flag_path: None,
                    glyph: None,
                    selected,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings
                                .dynamic_island_placements
                                .retain(|k, _| !dynamic_island::same_widget_id(k, wid));
                            if let Some(ref dev) = target {
                                this.settings.set_widget_enabled(wid, true);
                                if wid == "media" {
                                    this.settings.show_media = true;
                                }
                                let canonical = settings::canonical_widget_setting_id(wid);
                                this.settings
                                    .dynamic_island_placements
                                    .insert(canonical, dev.clone());
                            }
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect();
        rows.push(
            setting_row(
                t,
                Some("\u{E8A1}"),
                loc::t("DynamicIslandWidgetLocation"),
                self.make_dropdown(format!("w-di-place-{wid}"), t, place_opts, cx),
            )
            .into_any_element(),
        );

        if widgets::widget_has_flyout(wid) {
            rows.push(
                setting_row(
                    t,
                    Some("\u{E8B0}"),
                    loc::t("WidgetPreviewFlyout"),
                    button(
                        SharedString::from(format!("w-preview-flyout-{wid}")),
                        t,
                        loc::t("WidgetPreviewFlyout"),
                        ButtonVariant::Secondary,
                        cx.listener(move |this, _, window, _| {
                            this.preview_widget_flyout(wid, Self::window_hwnd(window));
                        }),
                    ),
                )
                .into_any_element(),
            );
        }
        rows
    }

    fn render_ai_provider_drawer(
        &self,
        wid: &'static str,
        t: &UiTheme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows: Vec<AnyElement> = Vec::new();
        let meta = widgets::telemetry::ai_provider_meta(wid);
        let probe = widgets::telemetry::ai_auth_probe(wid);
        let is_testing = self.ai_testing.contains(wid);

        let status_label = if probe.env_override {
            if probe.env_var_hint.is_empty() {
                loc::t("AiUsageEnvActive")
            } else {
                format!("{} ({})", loc::t("AiUsageEnvActive"), probe.env_var_hint)
            }
        } else if probe.has_token {
            let source_tag = match probe.source {
                AiAuthSource::Dpapi => "DPAPI",
                AiAuthSource::CliFile => "CLI",
                AiAuthSource::GhKeyring => "gh",
                AiAuthSource::CursorPkce => "PKCE",
                AiAuthSource::Env => "ENV",
                AiAuthSource::None => "",
            };
            if source_tag.is_empty() {
                loc::t("AiUsageAccountConnected")
            } else {
                format!("{} · {source_tag}", loc::t("AiUsageAccountConnected"))
            }
        } else {
            loc::t("AiUsageAccountSignInRequired")
        };

        let mut status_controls = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(px(6.0))
            .child(badge(
                t,
                status_label,
                probe.has_token || probe.env_override,
            ))
            .child(button_with_state(
                SharedString::from(format!("ai-test-{wid}")),
                t,
                loc::t("AiUsageTestConnection"),
                ButtonVariant::Secondary,
                !is_testing,
                cx.listener(move |this, _, _, cx| {
                    if this.ai_testing.contains(wid) {
                        return;
                    }
                    this.ai_testing.insert(wid);
                    this.ai_test_status.remove(wid);
                    cx.notify();
                    cx.spawn(async move |this, cx| {
                        let result = cx
                            .background_executor()
                            .spawn(async move { widgets::telemetry::test_ai_connection(wid) })
                            .await;
                        let _ = this.update(cx, |view, cx| {
                            view.ai_testing.remove(wid);
                            view.ai_test_status.insert(wid, result);
                            cx.notify();
                        });
                    })
                    .detach();
                }),
            ));
        if let Some(meta) = meta {
            let dashboard_url = meta.dashboard_url.to_owned();
            status_controls = status_controls.child(button(
                SharedString::from(format!("ai-dash-{wid}")),
                t,
                loc::t("AiUsageOpenDashboard"),
                ButtonVariant::Secondary,
                move |_, _, _| {
                    pins::launch(dashboard_url.clone());
                },
            ));
        }
        rows.push(
            setting_row(
                t,
                Some("\u{E99A}"),
                loc::t("AiUsageAccountLabel"),
                status_controls,
            )
            .into_any_element(),
        );

        if let Some((ok, msg)) = self.ai_test_status.get(wid) {
            rows.push(
                div()
                    .w_full()
                    .px(px(14.0))
                    .py(px(6.0))
                    .child(
                        div()
                            .w_full()
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(7.0))
                            .bg(rgba(if *ok {
                                t.accent_soft
                            } else {
                                t.destructive_soft
                            }))
                            .text_size(px(fonts::scaled_size(11.5)))
                            .text_color(rgba(if *ok { t.accent } else { t.destructive }))
                            .child(msg.clone()),
                    )
                    .into_any_element(),
            );
        }

        if wid == "aiusage_cursor" {
            let signed_in = cursor_auth::has_session();
            rows.push(
                setting_row_desc(
                    t,
                    Some("\u{E77B}"),
                    loc::t("AiUsageCursorAccountLabel"),
                    self.ai_status.clone(),
                    button(
                        "btn-drawer-cursor-auth",
                        t,
                        if signed_in {
                            loc::t("AiUsageCursorSignOut")
                        } else {
                            loc::t("AiUsageCursorSignIn")
                        },
                        if signed_in {
                            ButtonVariant::Secondary
                        } else {
                            ButtonVariant::Primary
                        },
                        cx.listener(move |this, _, _, cx| {
                            if cursor_auth::has_session() {
                                let _ = cursor_auth::sign_out();
                                this.settings.extra.remove("CursorAuthToken");
                                this.ai_status = loc::t("AiCursorNotSignedIn");
                                this.ai_test_status.remove(wid);
                                this.save_immediately(cx);
                                return;
                            }
                            let start = cursor_auth::sign_in_start();
                            pins::launch(start.login_url);
                            this.ai_status = loc::t("AiUsageCursorWaiting");
                            cx.notify();
                            cx.spawn(async move |this, cx| {
                                let ok = cx
                                    .background_executor()
                                    .spawn(async { cursor_auth::sign_in_finish() })
                                    .await;
                                let _ = this.update(cx, |view, cx| {
                                    if ok && cursor_auth::has_session() {
                                        view.settings.extra.remove("CursorAuthToken");
                                        view.ai_status = loc::t("AiCursorSignedIn");
                                        view.ai_test_status.remove(wid);
                                        view.save_immediately(cx);
                                    } else {
                                        view.ai_status = loc::t("AiUsageCursorSignInTimedOut");
                                        cx.notify();
                                    }
                                });
                            })
                            .detach();
                        }),
                    ),
                )
                .into_any_element(),
            );
        } else if matches!(
            wid,
            "aiusage_claude" | "aiusage_codex" | "aiusage_copilot" | "aiusage_opencode"
        ) {
            let hint = meta
                .map(|m| loc::t_fmt("AiUsageSignInHint", &[m.signin_hint]))
                .unwrap_or_default();
            let cli_control: AnyElement = if probe.cli_installed {
                button(
                    SharedString::from(format!("ai-cli-signin-{wid}")),
                    t,
                    loc::t("AiUsageCliSignInLabel"),
                    ButtonVariant::Secondary,
                    cx.listener(move |this, _, _, cx| {
                        let launched = launch_ai_cli_sign_in(wid);
                        this.ai_test_status.insert(
                            wid,
                            (
                                launched,
                                if launched {
                                    loc::t("AiCliSignInLaunched")
                                } else {
                                    loc::t("AiUsageCliMissing")
                                },
                            ),
                        );
                        cx.notify();
                    }),
                )
                .into_any_element()
            } else {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(badge(t, loc::t("AiUsageCliMissing"), false))
                    .when_some(meta, |row, m| {
                        row.child(
                            div()
                                .px(px(7.0))
                                .py(px(3.0))
                                .rounded(px(5.0))
                                .bg(rgba(t.surface_muted))
                                .text_size(px(fonts::scaled_size(11.0)))
                                .text_color(rgba(t.text_secondary))
                                .child(m.signin_hint),
                        )
                    })
                    .into_any_element()
            };
            rows.push(
                setting_row_desc(
                    t,
                    Some("\u{E756}"),
                    loc::t("AiUsageCliSignInLabel"),
                    hint,
                    cli_control,
                )
                .into_any_element(),
            );
        }

        if let (Some(secret_key), Some(focus_handle)) = (
            widgets::telemetry::ai_secret_key_for_provider(wid),
            self.ai_secret_focus.get(wid).cloned(),
        ) {
            let has_dpapi = widgets::telemetry::has_dpapi_secret(secret_key);
            let raw_buf = self
                .ai_secret_buffers
                .get(wid)
                .map(String::as_str)
                .unwrap_or("");
            let is_focused = focus_handle.is_focused(window);
            let has_input = !raw_buf.trim().is_empty();

            let input_display = if !raw_buf.is_empty() {
                let masked = "•".repeat(raw_buf.chars().count());
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .text_color(rgba(t.text_primary))
                            .child(masked),
                    )
                    .when(is_focused, |line| {
                        line.child(
                            div()
                                .flex_shrink_0()
                                .w(px(1.0))
                                .h(px(13.0))
                                .bg(rgba(t.accent)),
                        )
                    })
            } else {
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(fonts::scaled_size(11.5)))
                    .text_color(rgba(t.text_secondary))
                    .child(if has_dpapi {
                        loc::t("AiUsageApiKeySaved")
                    } else {
                        loc::t("AiUsageApiKeyPlaceholder")
                    })
            };

            let focus_for_click = focus_handle.clone();
            let focus_for_keys = focus_handle.clone();
            let secret_inner = div()
                .id(SharedString::from(format!("ai-secret-input-{wid}")))
                .track_focus(&focus_handle)
                .tab_index(0)
                .w(px(210.0))
                .h(px(28.0))
                .px(px(9.0))
                .flex()
                .items_center()
                .rounded(px(7.0))
                .bg(rgba(t.surface))
                .border_1()
                .border_color(rgba(if is_focused { t.accent } else { t.border }))
                .cursor_text()
                .on_click(cx.listener(move |_, _, window, cx| {
                    focus_for_click.focus(window);
                    cx.notify();
                }))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    if !focus_for_keys.is_focused(window) {
                        return;
                    }
                    if this.handle_text_field_key_down(ActiveTextField::AiSecret(wid), event, cx) {
                        cx.stop_propagation();
                    }
                }))
                .child(input_display);

            let secret_input = TextInput::new(secret_inner, focus_handle, cx.entity());
            let key_desc = if probe.env_override && !probe.env_var_hint.is_empty() {
                format!("{} ({})", loc::t("AiUsageEnvActive"), probe.env_var_hint)
            } else if !probe.env_var_hint.is_empty() {
                format!(
                    "{} · {}",
                    loc::t("AiUsageApiKeyPlaceholder"),
                    probe.env_var_hint
                )
            } else {
                loc::t("AiUsageApiKeyPlaceholder")
            };

            rows.push(
                setting_row_desc(
                    t,
                    Some("\u{E8D7}"),
                    loc::t("AiUsageApiKeyLabel"),
                    key_desc,
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_end()
                        .gap(px(6.0))
                        .child(secret_input)
                        .child(button_with_state(
                            SharedString::from(format!("ai-key-save-{wid}")),
                            t,
                            loc::t("AiUsageApiKeySave"),
                            ButtonVariant::Primary,
                            has_input && !is_testing,
                            cx.listener(move |this, _, _, cx| {
                                let trimmed = this
                                    .ai_secret_buffers
                                    .get(wid)
                                    .map(|s| s.trim().to_owned())
                                    .unwrap_or_default();
                                if trimmed.is_empty() {
                                    return;
                                }
                                if widgets::telemetry::save_dpapi_secret(secret_key, &trimmed) {
                                    this.ai_secret_buffers.remove(wid);
                                    this.ai_secret_selections.remove(wid);
                                    this.ai_testing.insert(wid);
                                    this.ai_test_status.remove(wid);
                                    notify_docks_reload();
                                    cx.notify();
                                    cx.spawn(async move |this, cx| {
                                        let result = cx
                                            .background_executor()
                                            .spawn(async move {
                                                widgets::telemetry::test_ai_connection(wid)
                                            })
                                            .await;
                                        let _ = this.update(cx, |view, cx| {
                                            view.ai_testing.remove(wid);
                                            view.ai_test_status.insert(wid, result);
                                            notify_docks_reload();
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                } else {
                                    this.ai_test_status
                                        .insert(wid, (false, loc::t("ErrorAiUsageApiKey")));
                                    cx.notify();
                                }
                            }),
                        ))
                        .child(button_with_state(
                            SharedString::from(format!("ai-key-clear-{wid}")),
                            t,
                            loc::t("AiUsageApiKeyClear"),
                            ButtonVariant::Secondary,
                            has_dpapi || !raw_buf.is_empty(),
                            cx.listener(move |this, _, _, cx| {
                                widgets::telemetry::clear_dpapi_secret(secret_key);
                                this.ai_secret_buffers.remove(wid);
                                this.ai_secret_selections.remove(wid);
                                this.ai_test_status.remove(wid);
                                notify_docks_reload();
                                cx.notify();
                            }),
                        )),
                )
                .into_any_element(),
            );
        }

        let current_ring = widgets::resolved_ai_usage_ring(&self.settings, wid);
        let ring_dropdown = self.enum_dropdown(
            format!("ai-ring-{wid}"),
            t,
            vec![
                (loc::t("AiUsageRingHighest"), AiUsageRing::Highest),
                (loc::t("AiUsageRingSession"), AiUsageRing::Session),
                (loc::t("AiUsageRingWeekly"), AiUsageRing::Weekly),
            ],
            current_ring,
            move |this, value, cx| {
                this.settings.ai_usage_ring = value;
                let mut overrides = widgets::setting_value(&this.settings, "AiUsageRingOverrides")
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                overrides.retain(|k, _| !dynamic_island::same_widget_id(k, wid));
                let ring_str = match value {
                    AiUsageRing::Highest => "Highest",
                    AiUsageRing::Session => "Session",
                    AiUsageRing::Weekly => "Weekly",
                };
                overrides.insert(
                    widgets::canonical_widget_id(wid).to_owned(),
                    serde_json::Value::String(ring_str.to_owned()),
                );
                widgets::set_setting(
                    &mut this.settings,
                    "AiUsageRingOverrides",
                    serde_json::Value::Object(overrides),
                );
                this.auto_save(cx);
            },
            cx,
        );
        rows.push(if wid == "aiusage_claude" {
            self.target_row(
                "AiUsageRingLabel",
                t,
                Some("\u{E99A}"),
                loc::t("AiUsageRingLabel"),
                ring_dropdown,
            )
        } else {
            setting_row(
                t,
                Some("\u{E99A}"),
                loc::t("AiUsageRingLabel"),
                ring_dropdown,
            )
            .into_any_element()
        });
        rows
    }

    fn render_widget_inline_settings(
        &self,
        wid: &'static str,
        t: &UiTheme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows: Vec<AnyElement> = match wid {
            "aiusage_claude" | "aiusage_codex" | "aiusage_opencode" | "aiusage_copilot"
            | "aiusage_deepseek" | "aiusage_openrouter" | "aiusage_cursor" => {
                self.render_ai_provider_drawer(wid, t, window, cx)
            }
            "clock" => vec![
                self.target_row(
                    "ClockFormat24Hour",
                    t,
                    Some("\u{E823}"),
                    loc::t("ClockFormat24Hour"),
                    ios_switch(
                        "sw-24h",
                        t,
                        self.settings.clock_24_hour,
                        cx.listener(|this, _, _, cx| {
                            this.settings.clock_24_hour = !this.settings.clock_24_hour;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "ClockShowSeconds",
                    t,
                    Some("\u{E916}"),
                    loc::t("ClockShowSeconds"),
                    ios_switch(
                        "sw-seconds",
                        t,
                        self.settings.clock_show_seconds,
                        cx.listener(|this, _, _, cx| {
                            this.settings.clock_show_seconds = !this.settings.clock_show_seconds;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "DateStyleDayShort",
                    t,
                    Some("\u{E787}"),
                    loc::t("DateStyleDayShort"),
                    ios_switch(
                        "sw-weekday",
                        t,
                        self.settings.date_style == ClockDateStyle::DayAndShortDate,
                        cx.listener(|this, _, _, cx| {
                            this.settings.date_style =
                                if this.settings.date_style == ClockDateStyle::DayAndShortDate {
                                    ClockDateStyle::ShortDateOnly
                                } else {
                                    ClockDateStyle::DayAndShortDate
                                };
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "ClockDateLayout",
                    t,
                    Some("\u{E8A1}"),
                    loc::t("ClockDateLayout"),
                    self.make_dropdown(
                        "clock-date-layout",
                        t,
                        [
                            (loc::t("ClockDateLayoutBeside"), ClockDateLayout::Beside),
                            (loc::t("ClockDateLayoutBelow"), ClockDateLayout::Below),
                        ]
                        .into_iter()
                        .map(|(label, layout)| {
                            let entity = cx.entity().downgrade();
                            DropdownOption {
                                label: SharedString::from(label),
                                flag_path: None,
                                glyph: None,
                                selected: self.settings.clock_date_layout == layout,
                                on_select: std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        this.open_dropdown = None;
                                        this.settings.clock_date_layout = layout;
                                        this.auto_save(cx);
                                    });
                                }),
                            }
                        })
                        .collect(),
                        cx,
                    ),
                ),
                self.target_row(
                    "ClockDateFormat",
                    t,
                    Some("\u{E8BF}"),
                    loc::t("ClockDateFormat"),
                    self.make_dropdown(
                        "clock-date-format",
                        t,
                        [
                            (loc::t("DateFormatSystem"), ClockDateFormat::System),
                            (loc::t("DateFormatDdMmYyyy"), ClockDateFormat::DdMmYyyy),
                            (loc::t("DateFormatYyyyMmDd"), ClockDateFormat::YyyyMmDd),
                        ]
                        .into_iter()
                        .map(|(label, fmt)| {
                            let entity = cx.entity().downgrade();
                            DropdownOption {
                                label: SharedString::from(label),
                                flag_path: None,
                                glyph: None,
                                selected: self.settings.date_format == fmt,
                                on_select: std::rc::Rc::new(move |_win, app| {
                                    let _ = entity.update(app, |this, cx| {
                                        this.open_dropdown = None;
                                        this.settings.date_format = fmt;
                                        this.auto_save(cx);
                                    });
                                }),
                            }
                        })
                        .collect(),
                        cx,
                    ),
                ),
            ],
            "weather" => {
                let mut weather_rows = self.render_weather_picker(t, cx);
                let compact = widgets::setting_bool(&self.settings, "WeatherCompactMode", false);
                weather_rows.push(self.target_row(
                    "WeatherCompactMode",
                    t,
                    Some("\u{E753}"),
                    loc::t("WeatherCompactMode"),
                    ios_switch(
                        "sw-weather-compact",
                        t,
                        compact,
                        cx.listener(|this, _, _, cx| {
                            let next =
                                !widgets::setting_bool(&this.settings, "WeatherCompactMode", false);
                            widgets::set_setting(
                                &mut this.settings,
                                "WeatherCompactMode",
                                serde_json::Value::Bool(next),
                            );
                            this.auto_save(cx);
                        }),
                    ),
                ));
                weather_rows
            }
            "stats" => vec![
                self.target_row(
                    "Cpu",
                    t,
                    Some("\u{E950}"),
                    loc::t("Cpu"),
                    ios_switch(
                        "sw-meter-cpu",
                        t,
                        self.settings.stats_meters & 1 != 0,
                        cx.listener(|this, _, _, cx| {
                            this.settings.stats_meters ^= 1;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "Memory",
                    t,
                    Some("\u{E964}"),
                    loc::t("Memory"),
                    ios_switch(
                        "sw-meter-mem",
                        t,
                        self.settings.stats_meters & 2 != 0,
                        cx.listener(|this, _, _, cx| {
                            this.settings.stats_meters ^= 2;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row(
                    "Gpu",
                    t,
                    Some("\u{E7F4}"),
                    loc::t("Gpu"),
                    ios_switch(
                        "sw-meter-gpu",
                        t,
                        self.settings.stats_meters & 16 != 0,
                        cx.listener(|this, _, _, cx| {
                            this.settings.stats_meters ^= 16;
                            this.auto_save(cx);
                        }),
                    ),
                ),
                self.target_row_desc(
                    "GpuTemperature",
                    t,
                    Some("\u{E9CA}"),
                    loc::t("GpuTemperature"),
                    loc::t("StatsAvailabilityNote"),
                    ios_switch(
                        "sw-meter-gputemp",
                        t,
                        self.settings.stats_meters & 32 != 0,
                        cx.listener(|this, _, _, cx| {
                            this.settings.stats_meters ^= 32;
                            this.auto_save(cx);
                        }),
                    ),
                ),
            ],
            "bluetoothbattery" => {
                let extra_ignored = self
                    .settings
                    .extra
                    .get("IgnoredWirelessDevices")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let ignored_count = self
                    .settings
                    .ignored_wireless_devices
                    .len()
                    .max(extra_ignored);
                vec![
                    self.target_row(
                        "WirelessBatteryLowThreshold",
                        t,
                        Some("\u{E850}"),
                        loc::t("WirelessBatteryAlertLabel"),
                        self.make_dropdown(
                            "bt-low-threshold",
                            t,
                            [
                                (0, loc::t("WirelessBatteryAlertOff")),
                                (10, "10%".to_owned()),
                                (20, "20%".to_owned()),
                                (30, "30%".to_owned()),
                            ]
                            .into_iter()
                            .map(|(threshold, label)| {
                                let entity = cx.entity().downgrade();
                                DropdownOption {
                                    label: SharedString::from(label),
                                    flag_path: None,
                                    glyph: None,
                                    selected: self.settings.wireless_battery_low_threshold
                                        == threshold,
                                    on_select: std::rc::Rc::new(move |_win, app| {
                                        let _ = entity.update(app, |this, cx| {
                                            this.open_dropdown = None;
                                            this.settings.wireless_battery_low_threshold =
                                                threshold;
                                            this.auto_save(cx);
                                        });
                                    }),
                                }
                            })
                            .collect(),
                            cx,
                        ),
                    ),
                    self.target_row(
                        "WirelessBatteryPollSeconds",
                        t,
                        Some("\u{E916}"),
                        loc::t_fmt(
                            "WirelessBatteryPollInterval",
                            &[&self.settings.wireless_battery_poll_seconds.to_string()],
                        ),
                        self.make_dropdown(
                            "bt-poll-interval",
                            t,
                            [15, 30, 60, 120, 300]
                                .into_iter()
                                .map(|secs| {
                                    let entity = cx.entity().downgrade();
                                    DropdownOption {
                                        label: SharedString::from(format!("{secs}s")),
                                        flag_path: None,
                                        glyph: None,
                                        selected: self.settings.wireless_battery_poll_seconds
                                            == secs,
                                        on_select: std::rc::Rc::new(move |_win, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                this.open_dropdown = None;
                                                this.settings.wireless_battery_poll_seconds = secs;
                                                this.auto_save(cx);
                                            });
                                        }),
                                    }
                                })
                                .collect(),
                            cx,
                        ),
                    ),
                    self.target_row(
                        "WirelessBatteryListenOnly",
                        t,
                        Some("\u{E702}"),
                        loc::t_fmt(
                            "WirelessBatteryListenOnly",
                            &[&loc::t(if self.settings.wireless_battery_listen_only {
                                "WirelessBatteryListenOnlyOn"
                            } else {
                                "WirelessBatteryListenOnlyOff"
                            })],
                        ),
                        ios_switch(
                            "sw-bt-listen-only",
                            t,
                            self.settings.wireless_battery_listen_only,
                            cx.listener(|this, _, _, cx| {
                                this.settings.wireless_battery_listen_only =
                                    !this.settings.wireless_battery_listen_only;
                                this.auto_save(cx);
                            }),
                        ),
                    ),
                    self.target_row(
                        "RestoreIgnoredDevices",
                        t,
                        Some("\u{E777}"),
                        loc::t("RestoreIgnoredDevices"),
                        button_with_state(
                            "btn-bt-restore-ignored",
                            t,
                            if ignored_count > 0 {
                                format!("{} ({ignored_count})", loc::t("Restore"))
                            } else {
                                loc::t("Restore")
                            },
                            ButtonVariant::Secondary,
                            ignored_count > 0,
                            cx.listener(|this, _, _, cx| {
                                this.settings.ignored_wireless_devices.clear();
                                this.settings.extra.remove("IgnoredWirelessDevices");
                                this.auto_save(cx);
                            }),
                        ),
                    ),
                ]
            }
            "storage" => {
                let compact = widgets::setting_bool(&self.settings, "StorageCompactMode", false);
                vec![self.target_row(
                    "StorageCompactMode",
                    t,
                    Some("\u{EDA2}"),
                    loc::t("StorageCompactMode"),
                    ios_switch(
                        "sw-storage-compact",
                        t,
                        compact,
                        cx.listener(|this, _, _, cx| {
                            let next =
                                !widgets::setting_bool(&this.settings, "StorageCompactMode", false);
                            widgets::set_setting(
                                &mut this.settings,
                                "StorageCompactMode",
                                serde_json::Value::Bool(next),
                            );
                            this.auto_save(cx);
                        }),
                    ),
                )]
            }
            "media" => vec![self.target_row(
                "ShowMedia",
                t,
                Some("\u{E8D6}"),
                loc::t("WidgetShowInDockMedia"),
                ios_switch(
                    "sw-show-media",
                    t,
                    self.settings.show_media,
                    cx.listener(|this, _, _, cx| {
                        let next = !this.settings.show_media;
                        this.settings.show_media = next;
                        if next && this.settings.focus_mode {
                            this.settings.focus_mode = false;
                        }
                        this.settings.set_widget_enabled("media", next);
                        this.auto_save(cx);
                    }),
                ),
            )],
            "audio" => vec![
                setting_row_desc(
                    t,
                    Some("\u{E767}"),
                    loc::t("WheelVolume"),
                    loc::t("WheelVolumeDescription"),
                    ios_switch(
                        "sw-audio-wheel-volume",
                        t,
                        self.settings.wheel_volume_enabled,
                        cx.listener(|this, _, _, cx| {
                            this.settings.wheel_volume_enabled =
                                !this.settings.wheel_volume_enabled;
                            this.auto_save(cx);
                        }),
                    ),
                )
                .into_any_element(),
            ],
            "focus" => vec![
                self.target_row(
                    "FocusMinutes",
                    t,
                    Some("\u{E7C8}"),
                    loc::t("FocusMinutesLabel"),
                    self.number_stepper(
                        "focus-minutes",
                        t,
                        self.settings.focus_minutes,
                        1,
                        FOCUS_MINUTES_MAX,
                        |s| s.focus_minutes,
                        |s, value| s.focus_minutes = value,
                        cx,
                    ),
                ),
                self.target_row(
                    "FocusBreakMinutes",
                    t,
                    Some("\u{E7C8}"),
                    loc::t("FocusBreakMinutesLabel"),
                    self.number_stepper(
                        "focus-break-minutes",
                        t,
                        self.settings.focus_break_minutes,
                        1,
                        FOCUS_BREAK_MINUTES_MAX,
                        |s| s.focus_break_minutes,
                        |s, value| s.focus_break_minutes = value,
                        cx,
                    ),
                ),
                self.target_row(
                    "FocusSessions",
                    t,
                    Some("\u{E7C8}"),
                    loc::t("FocusSessionsLabel"),
                    self.number_stepper(
                        "focus-sessions",
                        t,
                        self.settings.focus_sessions,
                        1,
                        FOCUS_SESSIONS_MAX,
                        |s| s.focus_sessions,
                        |s, value| s.focus_sessions = value,
                        cx,
                    ),
                ),
            ],
            "countdown" => vec![self.target_row(
                "CountdownDefaultMinutes",
                t,
                Some("\u{EA8F}"),
                loc::t("CountdownDefaultMinutes"),
                self.number_stepper_with_suffix(
                    "countdown-default-minutes",
                    t,
                    self.settings.countdown_default_minutes() as i32,
                    " min",
                    1,
                    180,
                    1,
                    |s| s.countdown_default_minutes() as i32,
                    |s, value| s.set_countdown_default_minutes(value.clamp(1, 180) as u32),
                    cx,
                ),
            )],
            _ => Vec::new(),
        };
        rows.extend(self.render_widget_shared_drawer_rows(wid, t, cx));
        rows
    }

    fn render_widget_card(
        &self,
        w: &'static widgets::WidgetDescriptor,
        t: &UiTheme,
        is_dark: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wid = w.id;
        let enabled = self.settings.is_widget_enabled(wid);
        let has_inline = widget_has_card_drawer(wid);
        let expanded = has_inline && self.expanded_widget == Some(wid);
        let preview = self.render_widget_preview(wid, t, is_dark);
        let border_col = if enabled || expanded {
            t.accent
        } else {
            t.border
        };
        let card_hover = if expanded { t.surface } else { t.surface_hover };
        let target_id = format!("widget-{wid}");
        let highlighted = self.highlight_row.as_deref() == Some(target_id.as_str());
        let anchor = if highlighted {
            self.scroll_anchor.clone()
        } else {
            None
        };
        let stage_bg = if is_dark { 0x0d0d10ff } else { 0xe9ecf1ff };
        let summary = self.widget_summary(wid);

        let cfg_button = if has_inline {
            Some(card_button(
                SharedString::from(format!("wcfg-{wid}")),
                t,
                Some(if expanded { "\u{E70E}" } else { "\u{E713}" }),
                if expanded {
                    loc::t("Close")
                } else {
                    loc::t("Settings")
                },
                expanded,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_dropdown = None;
                    this.expanded_widget = if this.expanded_widget == Some(wid) {
                        None
                    } else {
                        Some(wid)
                    };
                    cx.notify();
                }),
            ))
        } else {
            None
        };

        let custom_actions = if widgets::custom_widgets::is_custom(wid) {
            let approved = widgets::custom_widgets::is_approved(wid, &self.settings);
            let mut row = div().flex().items_center().gap(px(6.0));
            if !approved {
                row = row.child(compact_card_button(
                    SharedString::from(format!("wapprove-{wid}")),
                    t,
                    localized_action("WidgetApprove", "Approve"),
                    true,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        widgets::custom_widgets::approve(&mut this.settings, wid);
                        this.auto_save(cx);
                    }),
                ));
            }
            #[allow(clippy::type_complexity)]
            let actions: [(&str, String, fn(&Path)); 3] = [
                ("wfolder", loc::t("OpenFolder"), |path| {
                    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
                }),
                ("wedit", loc::t("Edit"), |path| {
                    let _ = std::process::Command::new("notepad.exe").arg(path).spawn();
                }),
                ("wremove", loc::t("WidgetRemove"), |path| {
                    let _ = std::fs::remove_dir_all(path);
                    widgets::custom_widgets::reload();
                }),
            ];
            for (suffix, label, action) in actions {
                row = row.child(compact_card_button(
                    SharedString::from(format!("{suffix}-{wid}")),
                    t,
                    label,
                    false,
                    cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        if let Some(path) =
                            widgets::custom_widgets::widget_dir(wid).map(|dir| match suffix {
                                "wedit" => dir.join("widget.json"),
                                _ => dir,
                            })
                        {
                            action(&path);
                            cx.notify();
                        }
                    }),
                ));
            }
            Some(row)
        } else {
            None
        };

        let icon_button = if widgets::custom_widgets::is_custom(wid) {
            Some(card_button(
                SharedString::from(format!("wicon-{wid}")),
                t,
                Some("\u{E91B}"),
                loc::t("ThemeSectionIcons"),
                false,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some(path) =
                        crate::ui::prompt::open_file("Choose widget icon", "PNG image", "*.png")
                    {
                        let mut overrides =
                            widgets::setting_value(&this.settings, "CustomWidgetIcons")
                                .and_then(|v| v.as_object().cloned())
                                .unwrap_or_default();
                        overrides.insert(wid.to_owned(), serde_json::json!(path.to_string_lossy()));
                        widgets::set_setting(
                            &mut this.settings,
                            "CustomWidgetIcons",
                            serde_json::Value::Object(overrides),
                        );
                        this.auto_save(cx);
                    }
                }),
            ))
        } else {
            None
        };

        let enable_switch = ios_switch(
            SharedString::from(format!("wsw-{wid}")),
            t,
            enabled,
            cx.listener(move |this, _, _, cx| {
                let next = !this.settings.is_widget_enabled(wid);
                if next && this.settings.focus_mode && wid != "clock" && wid != "focus" {
                    this.settings.focus_mode = false;
                }
                this.settings.set_widget_enabled(wid, next);
                if wid == "media" {
                    this.settings.show_media = next;
                }
                this.auto_save(cx);
            }),
        );

        let preview_box = div()
            .h(px(80.0))
            .px(px(12.0))
            .rounded(px(10.0))
            .bg(rgba(stage_bg))
            .border_1()
            .border_color(rgba(t.border))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .when(expanded, |b| b.min_w(px(260.0)).flex_shrink_0())
            .when(!expanded, |b| b.w_full())
            .child(preview);

        let mut meta = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .min_w_0()
                    .child(icon_glyph(
                        widget_glyph(wid),
                        13.5,
                        if enabled { t.accent } else { t.text_secondary },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(fonts::scaled_size(12.5)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.text_primary))
                            .child(widgets::widget_name(w.id)),
                    ),
            );
        if let Some(sub) = summary {
            meta = meta.child(
                div()
                    .w_full()
                    .min_w_0()
                    .pl(px(21.0))
                    .text_size(px(fonts::scaled_size(11.0)))
                    .text_color(rgba(t.text_secondary))
                    .child(sub),
            );
        }
        if widgets::custom_widgets::is_custom(wid) {
            let status = if !widgets::custom_widgets::is_approved(wid, &self.settings) {
                Some(localized_action("ApprovalRequired", "Approval required"))
            } else {
                widgets::custom_widgets::error(wid)
            };
            if let Some(status) = status {
                meta = meta.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .pl(px(21.0))
                        .text_size(px(fonts::scaled_size(10.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(status),
                );
            }
        }

        // Only flex-grow horizontally when laid out beside the preview in the expanded header;
        // inside the vertical collapsed `head` column, `.flex_1()` would collapse `flex-basis`
        // height to 0 and cause cards to overlap subsequent rows.
        let footer = div()
            .when(expanded, |f| f.flex_1())
            .when(!expanded, |f| f.w_full())
            .min_w_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(meta)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .flex_shrink_0()
                    .when_some(custom_actions, |row, b| row.child(b))
                    .when_some(icon_button, |row, b| row.child(b))
                    .when_some(cfg_button, |row, b| row.child(b))
                    .child(enable_switch),
            );

        let head = if expanded {
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(14.0))
                .child(preview_box)
                .child(footer)
        } else {
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(preview_box)
                .child(footer)
        };

        let mut card = div()
            .when(expanded, |c| c.w_full().flex_shrink_0())
            .when(!expanded, |c| c.flex_1().min_w_0())
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(13.0))
            .rounded(px(13.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(border_col))
            .hover(move |s| s.bg(rgba(card_hover)))
            .child(head);

        if expanded {
            let rows = self.render_widget_inline_settings(wid, t, window, cx);
            let mut drawer = div()
                .w_full()
                .pt(px(8.0))
                .border_t_1()
                .border_color(rgba(t.separator))
                .flex()
                .flex_col();
            let count = rows.len();
            for (idx, row) in rows.into_iter().enumerate() {
                drawer = drawer.child(row);
                if idx + 1 < count {
                    drawer = drawer.child(div().h(px(1.0)).mx(px(8.0)).bg(rgba(t.separator)));
                }
            }
            card = card.child(drawer);
        }

        let card = if highlighted {
            card.bg(rgba(t.accent_soft)).border_color(rgba(t.accent))
        } else {
            card
        };

        card.id(SharedString::from(format!("wcard-{wid}")))
            .anchor_scroll(anchor)
            .into_any_element()
    }

    fn render_widgets(
        &self,
        t: &UiTheme,
        is_dark: bool,
        columns: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        let active_count = widgets::catalog_widgets()
            .filter(|w| self.settings.is_widget_enabled(w.id))
            .count();

        let filter_opts: Vec<DropdownOption> = std::iter::once("All")
            .chain(std::iter::once("Active"))
            .chain(widgets::WIDGET_CATEGORIES.iter().copied())
            .map(|cat| {
                let entity = cx.entity().downgrade();
                DropdownOption {
                    label: SharedString::from(category_label(cat)),
                    flag_path: None,
                    glyph: None,
                    selected: self.widget_filter == cat,
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.widget_filter = cat;
                            cx.notify();
                        });
                    }),
                }
            })
            .collect();

        out.push(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(10.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(badge(
                            t,
                            loc::t_fmt(
                                "WidgetsActiveCount",
                                &[
                                    &active_count.to_string(),
                                    &widgets::catalog_widgets().count().to_string(),
                                ],
                            ),
                            active_count > 0,
                        ))
                        .child(self.make_dropdown("widget-cat-filter", t, filter_opts, cx)),
                )
                .child(button(
                    "btn-reload-custom-widgets",
                    t,
                    loc::t("ThemePackReload"),
                    ButtonVariant::Secondary,
                    cx.listener(|_, _, _, cx| {
                        widgets::custom_widgets::reload();
                        cx.notify();
                    }),
                ))
                .child(button_with_state(
                    "btn-clear-widgets",
                    t,
                    loc::t("ClearAll"),
                    ButtonVariant::Secondary,
                    active_count > 0,
                    cx.listener(|this, _, _, cx| {
                        this.settings.show_clock_widget = false;
                        this.settings.show_system_stats_widget = false;
                        this.settings.show_media = false;
                        this.settings.enabled_widgets.clear();
                        this.auto_save(cx);
                    }),
                ))
                .into_any_element(),
        );

        for &cat in widgets::WIDGET_CATEGORIES.iter() {
            if self.widget_filter != "All"
                && self.widget_filter != "Active"
                && self.widget_filter != cat
            {
                continue;
            }
            let items: Vec<&'static widgets::WidgetDescriptor> = widgets::catalog_widgets()
                .filter(|w| {
                    w.category == cat
                        && (self.widget_filter != "Active" || self.settings.is_widget_enabled(w.id))
                })
                .collect();
            if items.is_empty() {
                continue;
            }
            out.push(
                div()
                    .w_full()
                    .pt(px(4.0))
                    .px(px(2.0))
                    .text_size(px(fonts::scaled_size(12.0)))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgba(t.text_secondary))
                    .child(category_label(cat))
                    .into_any_element(),
            );

            // Lay out cards in pairs, allowing any expanded card to span full width cleanly.
            let mut pending_row: Vec<&'static widgets::WidgetDescriptor> = Vec::new();
            let flush_row = |pending: &mut Vec<&'static widgets::WidgetDescriptor>,
                             out: &mut Vec<AnyElement>,
                             this: &Self,
                             cx: &mut Context<Self>| {
                if pending.is_empty() {
                    return;
                }
                let mut row = div().w_full().flex_shrink_0().flex().gap(px(12.0));
                for &w in pending.iter() {
                    row = row.child(this.render_widget_card(w, t, is_dark, window, cx));
                }
                if pending.len() < columns {
                    row = row.child(div().flex_1());
                }
                out.push(row.into_any_element());
                pending.clear();
            };

            for &w in &items {
                let is_expanded =
                    widget_has_card_drawer(w.id) && self.expanded_widget == Some(w.id);
                if is_expanded {
                    flush_row(&mut pending_row, &mut out, self, cx);
                    out.push(self.render_widget_card(w, t, is_dark, window, cx));
                } else {
                    pending_row.push(w);
                    if pending_row.len() >= columns {
                        flush_row(&mut pending_row, &mut out, self, cx);
                    }
                }
            }
            flush_row(&mut pending_row, &mut out, self, cx);
        }

        let density_opts: Vec<DropdownOption> = [
            (WidgetDensityMode::Auto, loc::t("WidgetDensityAuto")),
            (WidgetDensityMode::Compact, loc::t("WidgetDensityCompact")),
            (WidgetDensityMode::Full, loc::t("WidgetDensityFull")),
        ]
        .into_iter()
        .map(|(mode, label)| {
            let entity = cx.entity().downgrade();
            DropdownOption {
                label: SharedString::from(label),
                flag_path: None,
                glyph: None,
                selected: self.settings.global_widget_density == mode,
                on_select: std::rc::Rc::new(move |_win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        this.settings.global_widget_density = mode;
                        this.auto_save(cx);
                    });
                }),
            }
        })
        .collect();

        let trigger_opts: Vec<DropdownOption> = [
            (
                opentaskbar_core::settings::WidgetFlyoutTrigger::Click,
                loc::t("WidgetFlyoutTriggerClick"),
            ),
            (
                opentaskbar_core::settings::WidgetFlyoutTrigger::Hover,
                loc::t("WidgetFlyoutTriggerHover"),
            ),
        ]
        .into_iter()
        .map(|(mode, label)| {
            let entity = cx.entity().downgrade();
            DropdownOption {
                label: SharedString::from(label),
                flag_path: None,
                glyph: None,
                selected: self.settings.widget_flyout_trigger == mode,
                on_select: std::rc::Rc::new(move |_win, app| {
                    let _ = entity.update(app, |this, cx| {
                        this.open_dropdown = None;
                        this.settings.widget_flyout_trigger = mode;
                        this.auto_save(cx);
                    });
                }),
            }
        })
        .collect();

        let shrink_priority = self.number_stepper(
            "shrink-priority",
            t,
            self.settings.shrink_priority_bias(),
            -10,
            10,
            |s| s.shrink_priority_bias(),
            |s, value| s.set_shrink_priority_bias(value),
            cx,
        );

        out.push(
            section_card(
                t,
                loc::t("WidgetDensity"),
                vec![
                    setting_row(
                        t,
                        Some("\u{E8B0}"),
                        loc::t("WidgetFlyoutTrigger"),
                        self.make_dropdown("widget-flyout-trigger", t, trigger_opts, cx),
                    )
                    .into_any_element(),
                    self.target_row(
                        "WidgetDensityCompact",
                        t,
                        Some("\u{E80A}"),
                        loc::t("WidgetDensity"),
                        self.make_dropdown("widget-density", t, density_opts, cx),
                    ),
                    self.target_row_desc(
                        "ShrinkPriority",
                        t,
                        Some("\u{E80A}"),
                        loc::t("ShrinkPriority"),
                        loc::t("ShrinkPriorityDesc"),
                        shrink_priority,
                    ),
                ],
            )
            .into_any_element(),
        );
        out
    }

    fn render_dynamic_island(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let monitors = win32::enumerate_monitors();
        let di_opacity_pct = (self.settings.dynamic_island_opacity * 100.0).round() as i32;

        let di_disabled = !self.settings.dynamic_island_enable;
        let mut cards = vec![
            section_card(
                t,
                loc::t("DynamicIslandTitle"),
                vec![
                    setting_row_desc(
                        t,
                        Some("\u{E8A1}"),
                        loc::t("DynamicIslandEnable"),
                        loc::t("DynamicIslandEnableDescription"),
                        ios_switch(
                            "di-enable",
                            t,
                            self.settings.dynamic_island_enable,
                            cx.listener(|this, _, _, cx| {
                                this.settings.dynamic_island_enable =
                                    !this.settings.dynamic_island_enable;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row(
                            "DynamicIslandStyleLabel",
                            t,
                            Some("\u{E8A1}"),
                            loc::t("DynamicIslandStyleLabel"),
                            self.enum_dropdown(
                                "di-style",
                                t,
                                vec![
                                    (loc::t("DynamicIslandStyleNotch"), DynamicIslandStyle::Notch),
                                    (
                                        loc::t("DynamicIslandStyleFloating"),
                                        DynamicIslandStyle::Floating,
                                    ),
                                ],
                                self.settings.dynamic_island_style(),
                                |this, value, cx| {
                                    this.settings.set_dynamic_island_style(value);
                                    this.auto_save(cx);
                                },
                                cx,
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row(
                            "DynamicIslandLayoutLabel",
                            t,
                            Some("\u{E80A}"),
                            loc::t("DynamicIslandLayoutLabel"),
                            self.enum_dropdown(
                                "di-layout",
                                t,
                                vec![
                                    (
                                        loc::t("DynamicIslandLayoutPaged"),
                                        DynamicIslandLayout::Paged,
                                    ),
                                    (
                                        loc::t("DynamicIslandLayoutStrip"),
                                        DynamicIslandLayout::Strip,
                                    ),
                                ],
                                self.settings.dynamic_island_layout(),
                                |this, value, cx| {
                                    this.settings.set_dynamic_island_layout(value);
                                    this.auto_save(cx);
                                },
                                cx,
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row(
                            "DynamicIslandShowFocusWidgets",
                            t,
                            Some("\u{E7C8}"),
                            loc::t("DynamicIslandShowFocusWidgets"),
                            ios_switch(
                                "di-show-focus-widgets",
                                t,
                                self.settings.dynamic_island_show_focus_widgets(),
                                cx.listener(|this, _, _, cx| {
                                    let next = !this.settings.dynamic_island_show_focus_widgets();
                                    this.settings.set_dynamic_island_show_focus_widgets(next);
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row_desc(
                            "DynamicIslandVisibility",
                            t,
                            Some("\u{E7B3}"),
                            loc::t("DynamicIslandVisibility"),
                            loc::t("DynamicIslandVisibilityDescription"),
                            self.make_dropdown(
                                "di-vis",
                                t,
                                [
                                    (
                                        DynamicIslandVisibilityMode::Always,
                                        loc::t("DynamicIslandVisibilityAlways"),
                                    ),
                                    (
                                        DynamicIslandVisibilityMode::DesktopOnly,
                                        loc::t("DynamicIslandVisibilityDesktopOnly"),
                                    ),
                                ]
                                .into_iter()
                                .map(|(mode, label)| {
                                    let entity = cx.entity().downgrade();
                                    DropdownOption {
                                        label: SharedString::from(label),
                                        flag_path: None,
                                        glyph: None,
                                        selected: self.settings.dynamic_island_visibility == mode,
                                        on_select: std::rc::Rc::new(move |_win, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                this.open_dropdown = None;
                                                this.settings.dynamic_island_visibility = mode;
                                                this.auto_save(cx);
                                            });
                                        }),
                                    }
                                })
                                .collect(),
                                cx,
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row_desc(
                            "DynamicIslandHideFullscreen",
                            t,
                            Some("\u{E740}"),
                            loc::t("DynamicIslandHideFullscreen"),
                            loc::t("DynamicIslandHideFullscreenDescription"),
                            ios_switch(
                                "di-hide-fs",
                                t,
                                self.settings.dynamic_island_hide_in_fullscreen,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.dynamic_island_hide_in_fullscreen =
                                        !this.settings.dynamic_island_hide_in_fullscreen;
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row_desc(
                            "DynamicIslandAutoHide",
                            t,
                            Some("\u{ED1A}"),
                            loc::t("DynamicIslandAutoHide"),
                            loc::t("DynamicIslandAutoHideDescription"),
                            ios_switch(
                                "di-auto-hide",
                                t,
                                self.settings.dynamic_island_auto_hide,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.dynamic_island_auto_hide =
                                        !this.settings.dynamic_island_auto_hide;
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        setting_row_desc(
                            t,
                            Some("\u{E81E}"),
                            loc::t("DynamicIslandBackground"),
                            loc::t("DynamicIslandBackgroundDescription"),
                            self.make_dropdown(
                                "di-bg",
                                t,
                                [
                                    (loc::t("SurfaceGlass"), SurfaceBackground::Glass),
                                    (loc::t("SurfaceSolid"), SurfaceBackground::Solid),
                                    (loc::t("SurfaceNone"), SurfaceBackground::None),
                                    (loc::t("SurfaceTrueBlack"), SurfaceBackground::TrueBlack),
                                ]
                                .into_iter()
                                .map(|(label, bg)| {
                                    let entity = cx.entity().downgrade();
                                    DropdownOption {
                                        label: SharedString::from(label),
                                        flag_path: None,
                                        glyph: None,
                                        selected: self.settings.dynamic_island_background == bg,
                                        on_select: std::rc::Rc::new(move |_win, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                this.open_dropdown = None;
                                                this.settings.dynamic_island_background = bg;
                                                this.auto_save(cx);
                                            });
                                        }),
                                    }
                                })
                                .collect(),
                                cx,
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row_desc(
                            "DynamicIslandBackgroundColor",
                            t,
                            Some("\u{E790}"),
                            loc::t("DynamicIslandBackground"),
                            self.settings
                                .dynamic_island_background_hex()
                                .unwrap_or_default()
                                .to_owned(),
                            button(
                                "btn-island-color",
                                t,
                                loc::t("Edit"),
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, _, cx| this.edit_island_background_color(cx)),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        setting_row(
                            t,
                            Some("\u{E790}"),
                            loc::t("DynamicIslandUseTheme"),
                            ios_switch(
                                "di-use-theme",
                                t,
                                self.settings.dynamic_island_use_theme,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.dynamic_island_use_theme =
                                        !this.settings.dynamic_island_use_theme;
                                    this.auto_save(cx);
                                }),
                            ),
                        ),
                    )
                    .into_any_element(),
                    dim_when(
                        di_disabled,
                        self.target_row_desc(
                            "DynamicIslandOpacity",
                            t,
                            Some("\u{E7B3}"),
                            // No `DynamicIslandOpacity` locale key exists; reuse the generic Opacity label.
                            loc::t("SurfaceOpacityLabel"),
                            loc::t("DynamicIslandOpacityDescription"),
                            live_slider(
                                "di-opacity",
                                t,
                                ((self.settings.dynamic_island_opacity as f32) - 0.1) / 0.9,
                                19,
                                loc::t_fmt("OpacityPercent", &[&di_opacity_pct.to_string()]),
                                |i| {
                                    let entity = cx.entity().downgrade();
                                    std::rc::Rc::new(move |_win, app| {
                                        let _ = entity.update(app, |this, cx| {
                                            let next = (0.10 + (i as f64) * 0.05).clamp(0.10, 1.0);
                                            if (this.settings.dynamic_island_opacity - next).abs()
                                                > 0.001
                                            {
                                                this.settings.dynamic_island_opacity = next;
                                                this.auto_save(cx);
                                            }
                                        });
                                    })
                                },
                            ),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ];

        let all_widgets: Vec<&'static widgets::WidgetDescriptor> =
            widgets::catalog_widgets().collect();
        let placed_ids_raw: Vec<String> = all_widgets
            .iter()
            .filter(|wd| {
                self.settings
                    .dynamic_island_placements
                    .keys()
                    .any(|k| dynamic_island::same_widget_id(k, wd.id))
            })
            .map(|wd| settings::canonical_widget_setting_id(wd.id))
            .collect();
        let saved_global_order = self
            .settings
            .dynamic_island_widget_orders
            .get(settings::DYNAMIC_ISLAND_ALL_DISPLAYS)
            .map(Vec::as_slice);
        let placed_order =
            dynamic_island::order_widgets(&placed_ids_raw, saved_global_order, None, &[]);

        let mut ordered_widgets: Vec<&'static widgets::WidgetDescriptor> =
            Vec::with_capacity(all_widgets.len());
        for pid in &placed_order {
            if let Some(&wd) = all_widgets
                .iter()
                .find(|wd| dynamic_island::same_widget_id(wd.id, pid))
            {
                ordered_widgets.push(wd);
            }
        }
        for &wd in &all_widgets {
            if !ordered_widgets
                .iter()
                .any(|existing| dynamic_island::same_widget_id(existing.id, wd.id))
            {
                ordered_widgets.push(wd);
            }
        }

        let mut placement_rows: Vec<AnyElement> = Vec::with_capacity(ordered_widgets.len());
        for (index, wd) in ordered_widgets.into_iter().enumerate() {
            let wid = wd.id;
            let current = self
                .settings
                .dynamic_island_placements
                .iter()
                .find(|(k, _)| dynamic_island::same_widget_id(k, wid))
                .map(|(_, v)| v.clone());
            let in_island = current.is_some();
            let placed_pos = placed_order
                .iter()
                .position(|id| dynamic_island::same_widget_id(id, wid));

            let mut choices: Vec<(Option<String>, String)> = Vec::new();
            choices.push((None, loc::t("DynamicIslandDock")));
            choices.push((
                Some(settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned()),
                loc::t("DynamicIslandAllDisplays"),
            ));
            for mon in &monitors {
                choices.push((Some(mon.device_name.clone()), mon.display_label()));
            }

            let dd_opts: Vec<DropdownOption> = choices
                .into_iter()
                .map(|(target, label)| {
                    let is_active = match (&current, &target) {
                        (None, None) => true,
                        (Some(cur), Some(tgt)) => cur.eq_ignore_ascii_case(tgt),
                        _ => false,
                    };
                    let entity = cx.entity().downgrade();
                    DropdownOption {
                        label: SharedString::from(label),
                        flag_path: None,
                        glyph: None,
                        selected: is_active,
                        on_select: std::rc::Rc::new(move |_win, app| {
                            let _ = entity.update(app, |this, cx| {
                                this.open_dropdown = None;
                                this.settings
                                    .dynamic_island_placements
                                    .retain(|k, _| !dynamic_island::same_widget_id(k, wid));
                                if let Some(ref dev) = target {
                                    this.settings.set_widget_enabled(wid, true);
                                    if wid == "media" {
                                        this.settings.show_media = true;
                                    }
                                    let canonical = settings::canonical_widget_setting_id(wid);
                                    this.settings
                                        .dynamic_island_placements
                                        .insert(canonical, dev.clone());
                                }
                                this.auto_save(cx);
                            });
                        }),
                    }
                })
                .collect();

            let quick_switch = ios_switch(
                SharedString::from(format!("di-sw-{wid}")),
                t,
                in_island,
                cx.listener(move |this, _, _, cx| {
                    let currently_in = this
                        .settings
                        .dynamic_island_placements
                        .keys()
                        .any(|k| dynamic_island::same_widget_id(k, wid));
                    this.settings
                        .dynamic_island_placements
                        .retain(|k, _| !dynamic_island::same_widget_id(k, wid));
                    if !currently_in {
                        this.settings.set_widget_enabled(wid, true);
                        if wid == "media" {
                            this.settings.show_media = true;
                        }
                        let canonical = settings::canonical_widget_setting_id(wid);
                        this.settings
                            .dynamic_island_placements
                            .insert(canonical, settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned());
                    }
                    this.auto_save(cx);
                }),
            );

            let mut control = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(quick_switch)
                .child(self.make_dropdown(format!("di-place-{wid}"), t, dd_opts, cx));

            if let Some(pos) = placed_pos
                && placed_order.len() > 1
            {
                let order_up = placed_order.clone();
                let target_dev_up = current.clone();
                let order_down = placed_order.clone();
                let target_dev_down = current.clone();
                control = control
                    .child(button_with_state(
                        SharedString::from(format!("di-up-{wid}")),
                        t,
                        "▲",
                        ButtonVariant::Secondary,
                        pos > 0,
                        cx.listener(move |this, _, _, cx| {
                            let next = dynamic_island::move_widget(&order_up, wid, -1);
                            this.settings.dynamic_island_widget_orders.insert(
                                settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned(),
                                next.clone(),
                            );
                            if let Some(ref dev) = target_dev_up
                                && !dev.eq_ignore_ascii_case(settings::DYNAMIC_ISLAND_ALL_DISPLAYS)
                            {
                                this.settings
                                    .dynamic_island_widget_orders
                                    .insert(dev.clone(), next);
                            }
                            this.auto_save(cx);
                        }),
                    ))
                    .child(button_with_state(
                        SharedString::from(format!("di-down-{wid}")),
                        t,
                        "▼",
                        ButtonVariant::Secondary,
                        pos + 1 < placed_order.len(),
                        cx.listener(move |this, _, _, cx| {
                            let next = dynamic_island::move_widget(&order_down, wid, 1);
                            this.settings.dynamic_island_widget_orders.insert(
                                settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned(),
                                next.clone(),
                            );
                            if let Some(ref dev) = target_dev_down
                                && !dev.eq_ignore_ascii_case(settings::DYNAMIC_ISLAND_ALL_DISPLAYS)
                            {
                                this.settings
                                    .dynamic_island_widget_orders
                                    .insert(dev.clone(), next);
                            }
                            this.auto_save(cx);
                        }),
                    ));
            }

            placement_rows.push(if index == 0 {
                // The catalog entry for this section targets its first widget row.
                self.target_row_desc(
                    "DynamicIslandWidgetLocation",
                    t,
                    Some(widget_glyph(wid)),
                    widgets::widget_name(wid),
                    loc::t("DynamicIslandShowInIsland"),
                    control,
                )
            } else {
                setting_row(
                    t,
                    Some(widget_glyph(wid)),
                    widgets::widget_name(wid),
                    control,
                )
                .into_any_element()
            });
        }

        cards.push(
            section_card(t, loc::t("DynamicIslandWidgetLocation"), placement_rows)
                .into_any_element(),
        );
        cards
    }

    fn render_preferences(&self, t: &UiTheme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let lang_opts: Vec<DropdownOption> = LANGUAGES
            .iter()
            .map(|(tag, _name)| {
                let chosen = *tag;
                let entity = cx.entity().downgrade();
                let flag =
                    fonts::flag_bytes(chosen).map(|_| SharedString::from(fonts::flag_path(chosen)));
                DropdownOption {
                    label: SharedString::from(loc::display_name(chosen)),
                    flag_path: flag,
                    glyph: None,
                    selected: self.settings.language.eq_ignore_ascii_case(chosen),
                    on_select: std::rc::Rc::new(move |_win, app| {
                        let _ = entity.update(app, |this, cx| {
                            this.open_dropdown = None;
                            this.settings.language = chosen.to_owned();
                            loc::set_language(chosen);
                            this.auto_save(cx);
                        });
                    }),
                }
            })
            .collect();

        let mut out = vec![
            section_card(
                t,
                loc::t("Language"),
                vec![self.target_row_desc(
                    "Language",
                    t,
                    Some("\u{E774}"),
                    loc::t("Language"),
                    loc::t("LanguageDescription"),
                    self.make_dropdown("prefs-language", t, lang_opts, cx),
                )],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("SectionHotkeys"),
                vec![
                    setting_row(
                        t,
                        Some("\u{E721}"),
                        loc::t("LauncherHotkey"),
                        button(
                            "btn-hotkey-launcher",
                            t,
                            self.settings
                                .launcher_binding()
                                .map(|binding| binding.display)
                                .unwrap_or_else(|| loc::t("HotkeyDisabled")),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.record_hotkey(true, cx)),
                        ),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E765}"),
                        loc::t("DockHotkey"),
                        button(
                            "btn-hotkey-dock",
                            t,
                            self.settings
                                .dock_binding()
                                .map(|binding| binding.display)
                                .unwrap_or_else(|| loc::t("HotkeyDisabled")),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.record_hotkey(false, cx)),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(self.hotkey_status.clone())
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("Profiles"),
                vec![
                    self.mark_target(
                        "Profiles",
                        t,
                        div().w_full().flex().flex_wrap().gap(px(6.0)).children(
                            self.settings
                                .profiles
                                .iter()
                                .enumerate()
                                .map(|(index, profile)| {
                                    let profile_name = profile.name.clone();
                                    let active = self
                                        .settings
                                        .active_profile
                                        .as_deref()
                                        .is_some_and(|name| name == profile.name);
                                    let label = format!(
                                        "{} {}{}",
                                        profile.name,
                                        if active { "•" } else { "" },
                                        ""
                                    );
                                    div()
                                        .id(SharedString::from(format!("profile-{index}")))
                                        .px(px(10.0))
                                        .py(px(5.0))
                                        .rounded(px(8.0))
                                        .bg(rgba(if active { t.accent } else { t.surface_muted }))
                                        .text_color(rgba(if active {
                                            t.accent_fg
                                        } else {
                                            t.text_primary
                                        }))
                                        .text_size(px(fonts::scaled_size(12.0)))
                                        .cursor_pointer()
                                        .child(label)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            if let Some(profile) = this
                                                .settings
                                                .profiles
                                                .iter()
                                                .find(|p| p.name == profile_name)
                                                .cloned()
                                            {
                                                settings::apply_full_profile(
                                                    &mut this.settings,
                                                    &profile,
                                                );
                                            }
                                            this.settings.active_profile =
                                                Some(profile_name.clone());
                                            fonts::apply_settings(&this.settings);
                                            loc::set_language(&this.settings.language);
                                            sync_from_settings(&this.settings);
                                            this.sync_settings_glass(window);
                                            this.widget_runtime.tick(&this.settings);
                                            this.auto_save(cx);
                                        }))
                                }),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .flex()
                        .gap(px(6.0))
                        .child(button(
                            "btn-profile-new",
                            t,
                            loc::t("ProfileNew"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.new_profile(cx)),
                        ))
                        .child(button(
                            "btn-profile-save",
                            t,
                            loc::t("Apply"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.save_active_profile(cx)),
                        ))
                        .children(self.settings.profiles.iter().enumerate().take(12).map(
                            |(index, profile)| {
                                let rename_title = loc::t_fmt("ProfileRename", &[&profile.name]);
                                let delete_title = loc::t_fmt("ProfileDelete", &[&profile.name]);
                                div()
                                    .flex()
                                    .gap(px(4.0))
                                    .child(button(
                                        SharedString::from(format!("profile-rename-{index}")),
                                        t,
                                        rename_title,
                                        ButtonVariant::Secondary,
                                        cx.listener(move |this, _, _, cx| {
                                            this.rename_profile(index, cx)
                                        }),
                                    ))
                                    .child(button(
                                        SharedString::from(format!("profile-delete-{index}")),
                                        t,
                                        delete_title,
                                        ButtonVariant::Destructive,
                                        cx.listener(move |this, _, _, cx| {
                                            this.delete_profile(index, cx)
                                        }),
                                    ))
                            },
                        ))
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("CheckForUpdates"),
                vec![
                    self.target_row(
                        "CheckUpdatesSetting",
                        t,
                        Some("\u{E9C5}"),
                        loc::t("CheckUpdatesSetting"),
                        ios_switch(
                            "sw-check-updates",
                            t,
                            self.settings.check_updates,
                            cx.listener(|this, _, _, cx| {
                                this.settings.check_updates = !this.settings.check_updates;
                                this.auto_save(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(self.update_status.clone())
                        .into_any_element(),
                    {
                        let mut controls = div().w_full().flex().gap(px(6.0)).child(button(
                            "btn-check-update",
                            t,
                            loc::t("CheckForUpdates"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, window, cx| this.check_updates(window, cx)),
                        ));
                        if let Some(release) = &self.update_release {
                            controls = controls.child(button(
                                "btn-download-update",
                                t,
                                loc::t_fmt("UpdateDownload", &[&release.version]),
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, window, cx| this.download_update(window, cx)),
                            ));
                        }
                        controls.into_any_element()
                    },
                ],
            )
            .into_any_element(),
            section_card(t, loc::t("SectionSystem"), {
                let rows: Vec<AnyElement> = vec![
                    self.target_row(
                        "Startup",
                        t,
                        Some("\u{E7E8}"),
                        loc::t("Startup"),
                        ios_switch(
                            "sw-startup",
                            t,
                            self.startup_enabled,
                            cx.listener(|this, _, _, cx| {
                                let next = !this.startup_enabled;
                                // set_enabled writes either the portable Run key (honouring
                                // StartupApproved) or the MSIX StartupTask. sync_all only repairs
                                // an existing registration and must stay on the startup path.
                                let write_error = startup_registration::set_enabled(next);
                                let observed = startup_registration::read();
                                let (enabled, error) =
                                    resolve_startup_toggle(next, write_error, observed);
                                this.startup_enabled = enabled;
                                this.startup_status = error.map(loc::t).unwrap_or_default();
                                cx.notify();
                            }),
                        ),
                    )
                    .into_any_element(),
                    div()
                        .w_full()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_secondary))
                        .child(self.startup_status.clone())
                        .into_any_element(),
                    self.target_row_desc(
                        "WindowPickerTitle",
                        t,
                        Some("\u{E7C4}"),
                        loc::t("WindowPickerTitle"),
                        loc::t("WindowPickerEnabledDesc"),
                        ios_switch(
                            "sw-window-picker",
                            t,
                            extra_number(&self.settings, "WindowPickerEnabled", 1) != 0,
                            cx.listener(|this, _, _, cx| {
                                let next =
                                    extra_number(&this.settings, "WindowPickerEnabled", 1) == 0;
                                set_extra_number(
                                    &mut this.settings,
                                    "WindowPickerEnabled",
                                    i32::from(next),
                                );
                                this.auto_save(cx);
                            }),
                        ),
                    ),
                ];
                rows
            })
            .into_any_element(),
            section_card(
                t,
                loc::t("Backup"),
                vec![
                    self.target_row(
                        "SettingsBackup",
                        t,
                        Some("\u{EDE1}"),
                        loc::t("SettingsBackup"),
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(button(
                                "btn-export",
                                t,
                                loc::t("ExportSettings"),
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, _, cx| {
                                    this.sync_dock_pins();
                                    let dst = settings::app_data_dir().join("settings.backup.json");
                                    let _ = settings::save_to(&dst, &this.settings);
                                    cx.notify();
                                }),
                            ))
                            .child(button(
                                "btn-import",
                                t,
                                loc::t("ImportSettings"),
                                ButtonVariant::Secondary,
                                cx.listener(|this, _, window, cx| {
                                    let src = settings::app_data_dir().join("settings.backup.json");
                                    if src.exists() {
                                        this.settings = settings::load_from(&src);
                                        fonts::apply_settings(&this.settings);
                                        loc::set_language(&this.settings.language);
                                        sync_from_settings(&this.settings);
                                        this.sync_settings_glass(window);
                                        this.save_immediately(cx);
                                    }
                                }),
                            )),
                    )
                    .into_any_element(),
                    self.target_row(
                        "ResetDefaults",
                        t,
                        Some("\u{E777}"),
                        loc::t("ResetDefaults"),
                        button(
                            "btn-reset-all",
                            t,
                            loc::t("Reset"),
                            ButtonVariant::Destructive,
                            cx.listener(|this, _, window, cx| {
                                this.settings = DockSettings::default();
                                this.custom_theme_name.clear();
                                this.custom_theme_name_selection = 0..0;
                                this.theme_status.clear();
                                this.token_apply_status.clear();
                                fonts::apply_settings(&this.settings);
                                loc::set_language(&this.settings.language);
                                sync_from_settings(&this.settings);
                                this.sync_settings_glass(window);
                                this.save_immediately(cx);
                            }),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ];
        out.push(
            section_card(t, loc::t("AiAccountsHeading"), self.render_ai_auth(t, cx))
                .into_any_element(),
        );
        out.push(
            section_card(t, loc::t("FullBackup"), self.render_backup_ui(t, cx)).into_any_element(),
        );
        out
    }

    fn render_about(&self, t: &UiTheme) -> Vec<AnyElement> {
        let data_dir = settings::app_data_dir().to_string_lossy().into_owned();
        let log_file = paths::rust_log_path().to_string_lossy().into_owned();
        vec![
            section_card(
                t,
                loc::t("Application"),
                vec![
                    self.target_row(
                        "Version",
                        t,
                        Some("\u{E946}"),
                        loc::t("Version"),
                        badge(t, concat!("v", env!("CARGO_PKG_VERSION")), true),
                    )
                    .into_any_element(),
                    setting_row(
                        t,
                        Some("\u{E1D2}"),
                        loc::t("GitHubRepository"),
                        button(
                            "btn-github",
                            t,
                            loc::t("OpenRepository"),
                            ButtonVariant::Secondary,
                            |_, _, _| {
                                pins::launch("https://github.com/alztrk/opentaskbar".to_owned());
                            },
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            section_card(
                t,
                loc::t("Diagnostics"),
                vec![
                    self.target_row(
                        "DataFolder",
                        t,
                        Some("\u{E8B7}"),
                        loc::t("DataFolder"),
                        button(
                            "btn-open-data",
                            t,
                            loc::t("Open"),
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(data_dir.clone()),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "OpenLogFile",
                        t,
                        Some("\u{E9F5}"),
                        loc::t("OpenLogFile"),
                        button(
                            "btn-open-log",
                            t,
                            loc::t("Open"),
                            ButtonVariant::Secondary,
                            move |_, _, _| pins::launch(log_file.clone()),
                        ),
                    )
                    .into_any_element(),
                    self.target_row(
                        "WindowsTaskbar",
                        t,
                        Some("\u{E777}"),
                        loc::t("WindowsTaskbar"),
                        button(
                            "btn-restore-tb",
                            t,
                            loc::t("RestoreWindowsTaskbar"),
                            ButtonVariant::Destructive,
                            |_, _, _| opentaskbar_shell::taskbar::restore_from_journal(),
                        ),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }
}

/// Backing buffer for Settings text inputs (header search, custom theme name, and masked
/// AI provider DPAPI secret fields). GPUI uses this to route typing, paste and IME input
/// to whichever `TextInput` currently holds focus.
impl EntityInputHandler for SettingsView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let field = self.focused_text_field(window);
        let (buf, _) = self.text_field_ref(field);
        let len = buf.encode_utf16().count();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        if start != range.start || end != range.end {
            *adjusted_range = Some(start..end);
        }
        let from = Self::byte_index_for_utf16(buf, start);
        let to = Self::byte_index_for_utf16(buf, end);
        Some(buf[from..to].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let field = self.focused_text_field(window);
        let (_, range) = self.text_field_ref(field);
        Some(UTF16Selection {
            range,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = self.focused_text_field(window);
        let range = range.unwrap_or_else(|| self.text_field_ref(field).1);
        self.replace_text_field_range(field, range, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = self.focused_text_field(window);
        let range = range.unwrap_or_else(|| self.text_field_ref(field).1);
        self.replace_text_field_range(field, range, new_text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<gpui::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<gpui::Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<gpui::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_dark = self.resolve_is_dark();
        self.sync_settings_glass(window);
        let mut t = UiTheme::for_settings_background(is_dark, self.settings.settings_background);
        t.border_width = self.settings.settings_border_thickness.clamp(0.0, 8.0) as f32;
        if let Some(value) = &self.settings.settings_border_color
            && let Some(color) = parse_hex_color(value)
        {
            t.border = color;
        }
        // Outer chrome: 8 DIP rounded surface on Windows 11 (matching DWMWCP_ROUND and the
        // SpatialGlassHost clip), square on Windows 10. Exactly one crisp border: the glass
        // host's optical rim when it is active, otherwise a GPUI border on the root.
        let corner_radius = settings_corner_radius(window_chrome::is_windows_11());
        let glass_rim_active = self.spatial_glass.is_some() && {
            let cfg = SpatialGlassConfig::from_settings(
                &self.settings,
                self.settings.settings_background,
                self.settings.settings_opacity,
                is_dark,
                false,
            );
            cfg.enabled && cfg.edge_glow > 0.0
        };
        let root_border_width = if settings_root_draws_border(glass_rim_active) {
            Some(t.border_width.max(1.0))
        } else {
            None
        };
        let root_border_color = t.border;
        let width = window.viewport_size().width;
        let narrow = width < px(SIDEBAR_BREAKPOINT);
        let inset = px(if narrow { 20.0 } else { 32.0 });
        let content_width = width - inset * 2.0 - px(if narrow { 0.0 } else { SIDEBAR_WIDTH });
        let make_nav = |p: SettingsPage, cx: &mut Context<Self>| -> AnyElement {
            sidebar_nav_item(
                SharedString::from(format!("nav-{}", p.title())),
                &t,
                p.glyph(),
                p.title(),
                None,
                self.page == p,
                cx.listener(move |this, _, _, cx| {
                    this.open_dropdown = None;
                    if this.page != p {
                        this.page = p;
                        this.highlight_row = None;
                        this.scroll_anchor = None;
                        this.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
                        cx.notify();
                    }
                }),
            )
            .when(narrow, |item| item.w(px(156.0)))
            .into_any_element()
        };
        let top_nav: Vec<AnyElement> = SettingsPage::TOP
            .into_iter()
            .map(|p| make_nav(p, cx))
            .collect();
        let bottom_nav: Vec<AnyElement> = SettingsPage::BOTTOM
            .into_iter()
            .map(|p| make_nav(p, cx))
            .collect();

        let search_query = self.search_text.trim().to_owned();
        let content_blocks = if !search_query.is_empty() {
            self.render_search_results(&search_query, &t, cx)
        } else {
            match self.page {
                SettingsPage::Dock => self.render_dock(&t, cx),
                SettingsPage::Widgets => {
                    let columns = if content_width < px(WIDGET_COLUMNS_BREAKPOINT) {
                        1
                    } else {
                        2
                    };
                    self.render_widgets(&t, is_dark, columns, window, cx)
                }
                SettingsPage::DynamicIsland => self.render_dynamic_island(&t, cx),
                SettingsPage::Themes => self.render_themes(&t, is_dark, window, cx),
                SettingsPage::Preferences => self.render_preferences(&t, cx),
                SettingsPage::About => self.render_about(&t),
            }
        };

        // The target row (if any) now carries a clone of this anchor. Schedule its deferred
        // scroll and consume the anchor so subsequent renders don't keep jumping back.
        // `open_search_result` forces the frame in which the callback runs.
        if let Some(anchor) = self.scroll_anchor.take() {
            let entity = cx.entity().downgrade();
            anchor.scroll_to(window, cx);
            window.on_next_frame(move |_, cx| {
                // Repaint with the offset the anchor just applied.
                let _ = entity.update(cx, |_, cx| cx.notify());
            });
        }

        let max_off: f32 = self.scroll_handle.max_offset().height.into();
        let cur_off: f32 = (-self.scroll_handle.offset().y).into();
        let viewport_h: f32 = self.scroll_handle.bounds().size.height.into();
        let scrollbar_el = if max_off > 2.0 && viewport_h > 40.0 {
            let total_h = viewport_h + max_off;
            let track_h = (viewport_h - 16.0).max(40.0);
            let thumb_h = ((viewport_h / total_h) * track_h).clamp(28.0, track_h);
            let ratio = (cur_off / max_off).clamp(0.0, 1.0);
            let thumb_top = 8.0 + ratio * (track_h - thumb_h);
            Some(
                div()
                    .absolute()
                    .top(px(thumb_top))
                    .right(px(5.0))
                    .w(px(5.0))
                    .h(px(thumb_h))
                    .rounded(px(3.0))
                    .bg(rgba(t.scrollbar_thumb)),
            )
        } else {
            None
        };

        let sidebar = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .justify_between()
            .gap(px(12.0))
            .p(px(14.0))
            .bg(rgba(t.sidebar_bg))
            .border_color(rgba(t.border))
            .when(narrow, |panel| panel.w_full().border_b_1())
            .when(!narrow, |panel| {
                panel
                    .w(px(SIDEBAR_WIDTH))
                    .h_full()
                    .border_r_1()
                    .rounded_bl(px(corner_radius))
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .gap(px(4.0))
                    .when(narrow, |nav| nav.flex_wrap())
                    .when(!narrow, |nav| nav.flex_col())
                    .children(top_nav),
            )
            .child(
                div()
                    .w_full()
                    .pt(px(10.0))
                    .border_t_1()
                    .border_color(rgba(t.separator))
                    .flex()
                    .gap(px(4.0))
                    .when(narrow, |nav| nav.flex_wrap())
                    .when(!narrow, |nav| nav.flex_col())
                    .children(bottom_nav),
            );

        let mut page_content = div().w_full().flex().flex_col().gap(px(20.0));
        page_content = page_content.children(content_blocks);

        let mut scroll_wrap = div().flex_1().min_h_0().w_full().relative().child(
            div()
                .id("settings-scroll")
                .w_full()
                .h_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_handle)
                .on_scroll_wheel(cx.listener(|_, _, _, cx| {
                    cx.notify();
                }))
                .px(inset)
                .pt(px(20.0))
                .pb(px(40.0))
                .child(page_content),
        );

        if let Some(sb) = scrollbar_el {
            scroll_wrap = scroll_wrap.child(sb);
        }

        let header_title = if search_query.is_empty() {
            self.page.title()
        } else {
            loc::t("SettingsSearchResults")
        };
        let header_subtitle = if search_query.is_empty() {
            self.page.description()
        } else {
            search_query.clone()
        };

        let main = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("settings-page-header-drag")
                    .w_full()
                    .flex_shrink_0()
                    .px(inset)
                    .py(px(if narrow { 12.0 } else { 18.0 }))
                    .border_b_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                        win32::start_native_window_drag(window);
                    })
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(22.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(header_title),
                    )
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .text_color(rgba(t.text_secondary))
                            .child(header_subtitle),
                    ),
            )
            .when_some(self.save_error.clone(), |panel, reason| {
                panel.child(
                    div()
                        .px(inset)
                        .py(px(8.0))
                        .bg(rgba(t.destructive_soft))
                        .text_color(rgba(t.destructive))
                        .text_size(px(fonts::scaled_size(13.0)))
                        .child(format!("{} — {reason}", loc::t("SettingsSaveError"))),
                )
            })
            .child(scroll_wrap);

        // Header search: icon on the left, a native text input in the middle and a clear
        // button on the right. The input is outside the draggable titlebar region.
        let search_focused = self.search_focus.is_focused(window);
        let caret_byte = Self::byte_index_for_utf16(&self.search_text, self.search_selection.start);
        let (text_before, text_after) = self.search_text.split_at(caret_byte);
        let search_value = if self.search_text.is_empty() {
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(fonts::scaled_size(12.0)))
                .text_color(rgba(t.text_secondary))
                .child(loc::t("SettingsSearchPlaceholder"))
        } else {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .overflow_hidden()
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_primary))
                        .child(text_before.to_owned()),
                )
                .when(search_focused, |line| {
                    line.child(
                        div()
                            .flex_shrink_0()
                            .w(px(1.0))
                            .h(px(13.0))
                            .bg(rgba(t.accent)),
                    )
                })
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(12.0)))
                        .text_color(rgba(t.text_primary))
                        .child(text_after.to_owned()),
                )
        };

        let clear_hover = t.surface_hover;
        let mut search_inner = div()
            .id("settings-search-field")
            .track_focus(&self.search_focus)
            .tab_index(0)
            .w(px(230.0))
            .max_w(px(280.0))
            .h(px(26.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .px(px(9.0))
            .rounded(px(7.0))
            .bg(rgba(t.surface))
            .border_1()
            .border_color(rgba(if search_focused { t.accent } else { t.border }))
            .cursor_text()
            .focus({
                let accent = t.accent;
                move |style| style.border_color(rgba(accent))
            })
            .on_click(cx.listener(|this: &mut Self, _, window, cx| {
                this.search_focus.focus(window);
                cx.notify();
            }))
            .on_key_down(
                cx.listener(|this: &mut Self, event: &KeyDownEvent, window, cx| {
                    if !this.search_focus.is_focused(window) {
                        return;
                    }
                    if this.handle_text_field_key_down(ActiveTextField::Search, event, cx) {
                        cx.stop_propagation();
                        return;
                    }
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            this.clear_search();
                            this.highlight_row = None;
                            this.scroll_anchor = None;
                            this.focus.focus(window);
                            cx.notify();
                            cx.stop_propagation();
                        }
                        "enter" => {
                            if let Some(entry) = settings_search::search(
                                &settings_search::catalog(),
                                this.search_text.trim(),
                            )
                            .first()
                            .cloned()
                            {
                                this.open_search_result((*entry).clone(), cx);
                            }
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }),
            )
            .child(icon_glyph("\u{E721}", 13.0, t.text_secondary))
            .child(search_value);

        if !self.search_text.is_empty() {
            search_inner = search_inner.child(
                div()
                    .id("settings-search-clear")
                    .flex_shrink_0()
                    .w(px(17.0))
                    .h(px(17.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgba(clear_hover)))
                    .on_click(cx.listener(|this: &mut Self, _, window, cx| {
                        this.clear_search();
                        this.highlight_row = None;
                        this.scroll_anchor = None;
                        this.search_focus.focus(window);
                        cx.notify();
                    }))
                    .child(icon_glyph("\u{E711}", 10.0, t.text_secondary)),
            );
        }

        let search_field =
            TextInput::new(search_inner, self.search_focus.clone(), cx.entity()).into_any_element();

        let hover_cap = t.surface_hover;
        let close_hover = t.destructive;
        let titlebar = div()
            .w_full()
            .h(px(36.0))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            // GPUI content masks are rectangular: round the corner-touching fills themselves.
            .rounded_t(px(corner_radius))
            .bg(rgba(t.sidebar_bg))
            .border_b_1()
            .border_color(rgba(t.border))
            .child(
                div()
                    .id("settings-titlebar-drag")
                    .flex_1()
                    .h_full()
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                        win32::start_native_window_drag(window);
                    })
                    .child(icon_glyph("\u{E713}", 13.0, t.text_secondary))
                    .child(
                        div()
                            .text_size(px(fonts::scaled_size(12.0)))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(t.text_primary))
                            .child(loc::t("SettingsTitle")),
                    ),
            )
            .child(div().flex_shrink_0().mx(px(12.0)).child(search_field))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h_full()
                    .child(
                        div()
                            .id("cap-min")
                            .w(px(44.0))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(hover_cap)))
                            .on_click(|_, window, _| window.minimize_window())
                            .child(icon_glyph("\u{E921}", 10.0, t.text_primary)),
                    )
                    .child(
                        div()
                            .id("cap-max")
                            .w(px(44.0))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(hover_cap)))
                            .on_click(|_, window, _| window.zoom_window())
                            .child(icon_glyph("\u{E922}", 10.0, t.text_primary)),
                    )
                    .child(
                        div()
                            .id("cap-close")
                            .w(px(46.0))
                            .h_full()
                            .rounded_tr(px(corner_radius))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(close_hover)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.flush_pending_save(cx);
                                window.remove_window();
                            }))
                            .child(icon_glyph("\u{E8BB}", 10.0, t.text_primary)),
                    ),
            );

        let panel_overlay = self.render_widget_panel_overlay(&t, cx);
        let brush_overlay = self.render_brush_picker_overlay(&t, cx);
        div()
            .id("settings")
            .track_focus(&self.focus)
            .tab_group()
            .on_action(|_: &TabNext, window, _| window.focus_next())
            .on_action(|_: &TabPrevious, window, _| window.focus_prev())
            .on_action(cx.listener(|this, _: &CloseWidgetPanel, window, cx| {
                this.open_dropdown = None;
                this.brush_picker_open = false;
                this.close_widget_panel(window);
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, window, _| {
                if let Some(ref mut glass) = this.spatial_glass
                    && glass.is_enabled()
                {
                    let dpi = win32::dpi_scale(Self::window_hwnd(window));
                    let x_px = f32::from(ev.position.x) * dpi;
                    let y_px = f32::from(ev.position.y) * dpi;
                    glass.set_pointer(x_px, y_px);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.open_dropdown.is_some() {
                        this.open_dropdown = None;
                        cx.notify();
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleFlyoutPreview, _, cx| {
                this.flyout_preview = !this.flyout_preview;
                cx.notify();
            }))
            .relative()
            .w_full()
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(corner_radius))
            .overflow_hidden()
            .when_some(root_border_width, |root, width| {
                root.border(px(width)).border_color(rgba(root_border_color))
            })
            .bg(rgba(t.canvas_bg))
            .font_family(SharedString::from(
                if self.settings.ui_font_family.trim().is_empty() {
                    fonts::native_ui_face().to_owned()
                } else {
                    self.settings.ui_font_family.clone()
                },
            ))
            .text_size(px(fonts::scaled_size(14.0)))
            .text_color(rgba(t.text_primary))
            .child(titlebar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .when(narrow, |root| root.flex_col())
                    .overflow_hidden()
                    .child(sidebar)
                    .child(main),
            )
            .when(panel_overlay.is_some(), |root| {
                root.child(panel_overlay.unwrap())
            })
            .when(brush_overlay.is_some(), |root| {
                root.child(brush_overlay.unwrap())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BrushMode, DockSettings, FALLBACK_WIDGET_GLYPH, SettingsPage, WIN_H, WIN_W,
        ai_cli_sign_in_command, brush_to_json, build_backup, build_brush, city_list,
        civil_from_days, close_spawned_settings_windows, field_cell_hsv, fit_settings_size,
        gradient_stop_at, import_backup_into, inspect_backup, location_auto_fill_value,
        persist_theme_tokens, read_density_override, reorder_gradient_stop, resolve_startup_toggle,
        safe_backup_path, settings_corner_radius, settings_root_draws_border, settings_search,
        settings_window_options, weather_country_for, widget_glyph, write_density_override,
        zip_store, zip_unstore,
    };
    use crate::design::brush::Hsv;
    use crate::design::theme::{
        apply_token_overrides, theme_test_guard, token_overrides_from_json,
    };
    use crate::design::tokens::{Rgba, TokenMap, TokenValue};
    use gpui::{WindowBounds, px, size};
    use opentaskbar_core::i18n as loc;
    use serde_json::Map;

    #[test]
    fn widget_glyphs_avoid_unassigned_codepoints() {
        // U+E917 has no glyph in Segoe Fluent Icons and rendered as a tofu box.
        assert_ne!(widget_glyph("countdown"), "\u{E917}");
        assert_eq!(widget_glyph("countdown"), "\u{EA8F}");
        assert_ne!(widget_glyph("countdown"), widget_glyph("stopwatch"));
    }

    #[test]
    fn dock_value_steppers_hidden_for_computed_sizes() {
        use opentaskbar_core::settings::{DockHeightMode, DockWidthMode};
        assert!(!super::dock_width_has_value(DockWidthMode::Auto));
        assert!(!super::dock_width_has_value(DockWidthMode::Fill));
        assert!(super::dock_width_has_value(DockWidthMode::Pixels));
        assert!(super::dock_width_has_value(DockWidthMode::Percent));
        assert!(!super::dock_height_has_value(DockHeightMode::Auto));
        assert!(!super::dock_height_has_value(DockHeightMode::Fill));
        assert!(super::dock_height_has_value(DockHeightMode::Pixels));
        assert!(super::dock_height_has_value(DockHeightMode::Percent));
        assert_eq!(super::unit_suffix(true), "%");
        assert_eq!(super::unit_suffix(false), " px");
    }

    #[test]
    fn dock_stepper_bounds_match_core_validation() {
        use opentaskbar_core::settings::{DockGapUnit, DockHeightMode, DockWidthMode};

        let (w_px_min, w_px_max, w_px_step) =
            super::dock_width_stepper_bounds(DockWidthMode::Pixels);
        let (w_pct_min, w_pct_max, w_pct_step) =
            super::dock_width_stepper_bounds(DockWidthMode::Percent);
        assert_eq!((w_px_min, w_px_max, w_px_step), (100, 5000, 10));
        assert_eq!((w_pct_min, w_pct_max, w_pct_step), (10, 100, 1));

        let (h_px_min, h_px_max, h_px_step) =
            super::dock_height_stepper_bounds(DockHeightMode::Pixels);
        let (h_pct_min, h_pct_max, h_pct_step) =
            super::dock_height_stepper_bounds(DockHeightMode::Percent);
        assert_eq!((h_px_min, h_px_max, h_px_step), (24, 400, 2));
        assert_eq!((h_pct_min, h_pct_max, h_pct_step), (2, 25, 1));

        let (g_px_min, g_px_max, g_px_step) =
            super::dock_top_gap_stepper_bounds(DockGapUnit::Pixels);
        let (g_pct_min, g_pct_max, g_pct_step) =
            super::dock_top_gap_stepper_bounds(DockGapUnit::Percent);
        assert_eq!((g_px_min, g_px_max, g_px_step), (0, 200, 2));
        assert_eq!((g_pct_min, g_pct_max, g_pct_step), (0, 100, 1));

        let min_valid = DockSettings {
            dock_width_pixels: w_px_min,
            dock_width_percent: w_pct_min,
            dock_height_pixels: h_px_min,
            dock_height_percent: h_pct_min,
            dock_top_gap_pixels: g_px_min,
            dock_top_gap_percent: g_pct_min,
            ..Default::default()
        };
        assert!(
            min_valid.validate().is_empty(),
            "{:?}",
            min_valid.validate()
        );

        let max_valid = DockSettings {
            dock_width_pixels: w_px_max,
            dock_width_percent: w_pct_max,
            dock_height_pixels: h_px_max,
            dock_height_percent: h_pct_max,
            dock_top_gap_pixels: g_px_max,
            dock_top_gap_percent: g_pct_max,
            ..Default::default()
        };
        assert!(
            max_valid.validate().is_empty(),
            "{:?}",
            max_valid.validate()
        );

        let below_min = DockSettings {
            dock_width_pixels: w_px_min - 1,
            dock_width_percent: w_pct_min - 1,
            dock_height_pixels: h_px_min - 1,
            dock_height_percent: h_pct_min - 1,
            ..Default::default()
        };
        let below_errors = below_min.validate();
        for rule in [
            "DockWidthPixelsOutOfRange",
            "DockWidthPercentOutOfRange",
            "DockHeightPixelsOutOfRange",
            "DockHeightPercentOutOfRange",
        ] {
            assert!(
                below_errors.contains(&rule),
                "missing {rule}: {below_errors:?}"
            );
        }

        let above_max = DockSettings {
            dock_width_pixels: w_px_max + 1,
            dock_width_percent: w_pct_max + 1,
            dock_height_pixels: h_px_max + 1,
            dock_height_percent: h_pct_max + 1,
            dock_top_gap_pixels: g_px_max + 1,
            dock_top_gap_percent: g_pct_max + 1,
            ..Default::default()
        };
        let above_errors = above_max.validate();
        for rule in [
            "DockWidthPixelsOutOfRange",
            "DockWidthPercentOutOfRange",
            "DockHeightPixelsOutOfRange",
            "DockHeightPercentOutOfRange",
            "DockTopGapPixelsOutOfRange",
            "DockTopGapPercentOutOfRange",
        ] {
            assert!(
                above_errors.contains(&rule),
                "missing {rule}: {above_errors:?}"
            );
        }
    }

    #[test]
    fn weather_fallback_city_follows_language() {
        assert_eq!(super::weather_fallback_city_for("tr-TR"), "İstanbul");
        assert_eq!(super::weather_fallback_city_for("en-US"), "Istanbul");
    }

    #[test]
    fn short_clock_format_label_drops_example() {
        assert_eq!(
            super::short_clock_format_label("24 saat (14:32)"),
            "24 saat"
        );
        assert_eq!(
            super::short_clock_format_label("12-hour (02:32 PM)"),
            "12-hour"
        );
        assert_eq!(super::short_clock_format_label("24h"), "24h");
    }

    #[test]
    fn clock_weekday_redundant_only_when_date_contains_it() {
        assert!(super::clock_weekday_is_redundant("Cum 27 Kas", "Cum"));
        assert!(super::clock_weekday_is_redundant("Fri 27 Nov", "Fri"));
        assert!(!super::clock_weekday_is_redundant("27 Kas", "Cum"));
        assert!(!super::clock_weekday_is_redundant(
            "Cumartesi 28 Kas",
            "Cum"
        ));
    }

    #[test]
    fn widget_density_override_uses_canonical_pascal_case_keys() {
        use opentaskbar_core::settings::WidgetDensityMode;

        let mut settings = DockSettings::default();
        write_density_override(&mut settings, "Clock", WidgetDensityMode::Compact);
        write_density_override(&mut settings, "SystemStats", WidgetDensityMode::Full);
        assert_eq!(
            settings.widget_density_overrides.get("Clock"),
            Some(&WidgetDensityMode::Compact)
        );
        assert_eq!(
            settings.widget_density_overrides.get("SystemStats"),
            Some(&WidgetDensityMode::Full)
        );
        assert_eq!(
            read_density_override(&settings, "Clock"),
            WidgetDensityMode::Compact
        );
        assert_eq!(
            read_density_override(&settings, "SystemStats"),
            WidgetDensityMode::Full
        );

        // Legacy lowercase keys still read cleanly and are replaced on write.
        settings
            .widget_density_overrides
            .insert("weather".to_owned(), WidgetDensityMode::Compact);
        assert_eq!(
            read_density_override(&settings, "Weather"),
            WidgetDensityMode::Compact
        );
        write_density_override(&mut settings, "Weather", WidgetDensityMode::Auto);
        assert!(!settings.widget_density_overrides.contains_key("weather"));
        assert!(!settings.widget_density_overrides.contains_key("Weather"));
    }

    #[test]
    fn settings_window_is_transparent_and_fits_small_work_areas() {
        let options = settings_window_options(None);
        // Opaque tells GPUI to pass ACCENT_DISABLED directly (avoiding
        // ACCENT_ENABLE_TRANSPARENTGRADIENT / 0x00000000 black corner bleed on Win11)
        // while GPUI still keeps its DXGI_ALPHA_MODE_PREMULTIPLIED swapchain.
        assert!(matches!(
            options.window_background,
            gpui::WindowBackgroundAppearance::Opaque
        ));

        // Unknown work area keeps the fixed dialog size.
        assert_eq!(fit_settings_size(None), size(px(WIN_W), px(WIN_H)));
        // Large work area: unchanged.
        assert_eq!(
            fit_settings_size(Some(size(px(2560.0), px(1400.0)))),
            size(px(WIN_W), px(WIN_H))
        );
        // 1080p at 150% with a 48 px taskbar ≈ 1280×688 DIP: the height must shrink.
        let fitted = fit_settings_size(Some(size(px(1280.0), px(688.0))));
        assert_eq!(fitted.width, px(WIN_W));
        assert!(fitted.height <= px(688.0 - 32.0));
        // Pathologically tiny work areas never produce a window larger than the work area.
        let tiny = fit_settings_size(Some(size(px(300.0), px(200.0))));
        assert!(tiny.width <= px(300.0) && tiny.height <= px(200.0));
        assert!(tiny.width > px(0.0) && tiny.height > px(0.0));

        // A clamped initial size lowers the minimum size with it (min == initial).
        let bounds = gpui::Bounds {
            origin: gpui::point(px(0.0), px(0.0)),
            size: fitted,
        };
        let clamped = settings_window_options(Some(WindowBounds::Windowed(bounds)));
        let minimum = clamped.window_min_size.expect("minimum size");
        assert_eq!(minimum, fitted);
        assert!(!clamped.is_resizable);
    }

    #[test]
    fn settings_chrome_draws_exactly_one_rounded_border() {
        assert_eq!(settings_corner_radius(true), 8.0);
        assert_eq!(settings_corner_radius(false), 0.0);
        // Active glass rim → GPUI root draws no border (no double outline).
        assert!(!settings_root_draws_border(true));
        // Solid / True Black / None, or Glass with OLED glow reduction → GPUI border.
        assert!(settings_root_draws_border(false));
    }

    #[test]
    fn every_catalog_widget_has_a_non_fallback_glyph() {
        for widget in crate::widgets::catalog_widgets() {
            assert_ne!(
                widget_glyph(widget.id),
                FALLBACK_WIDGET_GLYPH,
                "missing glyph for catalog widget {}",
                widget.id
            );
            assert_eq!(widget_glyph(widget.id).chars().count(), 1);
        }
        assert_eq!(
            widget_glyph("aiusage_claude"),
            widget_glyph("aiusage_cursor")
        );
        assert_eq!(widget_glyph("unknown-widget"), FALLBACK_WIDGET_GLYPH);
    }

    #[test]
    fn stored_zip_round_trips_entries() {
        let entries = vec![
            ("manifest.json".to_owned(), b"{\"Format\":\"x\"}".to_vec()),
            ("theme/a.json".to_owned(), vec![0u8, 1, 2, 255]),
        ];
        let bytes = zip_store(&entries);
        let read = zip_unstore(&bytes).expect("stored zip must be readable");
        assert_eq!(read.len(), 2);
        assert_eq!(read["manifest.json"], entries[0].1);
        assert_eq!(read["theme/a.json"], entries[1].1);
    }

    #[test]
    fn full_backup_includes_settings_pins_and_theme_files() {
        let mut settings = DockSettings::default();
        settings.user_pinned_apps.push("app.exe".to_owned());
        settings.weather_city = "Izmir, Turkey".to_owned();
        let bytes = build_backup(&settings);
        let preview = inspect_backup(&bytes);
        assert!(preview.valid, "{:?}", preview.error);
        assert_eq!(preview.pinned_apps, 1);
        assert_eq!(
            preview.settings.map(|s| s.weather_city),
            Some("Izmir, Turkey".to_owned())
        );
    }

    fn backup_with_themes(themes: &[(&str, &[u8])]) -> Vec<u8> {
        let mut entries = vec![
            (
                "manifest.json".to_owned(),
                br#"{"Format":"OpenTaskbarFullBackup","Version":1}"#.to_vec(),
            ),
            (
                "settings.json".to_owned(),
                br#"{"ThemePack":"catppuccin","ExtraFutureKey":true}"#.to_vec(),
            ),
            (
                "pins.json".to_owned(),
                br#"{"PinnedApps":["app:a"],"UnpinnedApps":[]}"#.to_vec(),
            ),
        ];
        entries.extend(
            themes
                .iter()
                .map(|(name, data)| (format!("theme/{name}"), (*data).to_vec())),
        );
        zip_store(&entries)
    }

    fn temp_theme_root(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("ot-import-{}-{tag}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn staging_folders(root: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".staging-"))
            .collect()
    }

    #[test]
    fn import_backup_stages_themes_and_cleans_staging_on_success() {
        let root = temp_theme_root("success");
        let bytes = backup_with_themes(&[
            ("current.json", br#"{"Id":"catppuccin"}"#),
            ("nested/accent.json", br##"{"Accent":"#A0B0C0"}"##),
            // Unsafe entries must be skipped, never written inside the staging folder.
            ("../escape.json", b"escape"),
        ]);

        let imported = import_backup_into(&bytes, &root).expect("import must succeed");
        assert_eq!(imported.theme_pack, "catppuccin");
        assert_eq!(imported.user_pinned_apps, ["app:a"]);
        assert_eq!(imported.extra["ExtraFutureKey"], serde_json::json!(true));
        assert_eq!(
            std::fs::read(root.join("current.json")).unwrap(),
            br#"{"Id":"catppuccin"}"#
        );
        assert_eq!(
            std::fs::read(root.join("nested").join("accent.json")).unwrap(),
            br##"{"Accent":"#A0B0C0"}"##
        );
        assert!(!root.join("escape.json").exists());
        assert!(
            staging_folders(&root).is_empty(),
            "staging directory must be removed after a successful import"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn import_backup_cleans_staging_when_commit_fails() {
        let root = temp_theme_root("failure");
        // A directory at the target path makes the commit pass fail after staging succeeded.
        std::fs::create_dir(root.join("blocked.json")).unwrap();
        let bytes = backup_with_themes(&[("blocked.json", b"theme payload")]);

        assert!(import_backup_into(&bytes, &root).is_err());
        assert!(
            staging_folders(&root).is_empty(),
            "staging directory must be removed when a theme write fails"
        );
        assert!(
            std::fs::read(root.join("blocked.json")).is_err(),
            "the commit must not turn a target directory into a file"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn backup_paths_reject_traversal() {
        assert!(safe_backup_path("theme/a.json").is_some());
        assert!(safe_backup_path("../secret").is_none());
        assert!(safe_backup_path("C:\\secret").is_none());
        assert!(safe_backup_path("a/b/../../c").is_none());
    }

    #[test]
    fn utc_stamp_day_conversion_matches_unix_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
    }

    #[test]
    fn city_picker_uses_platform_country_city_api() {
        assert_eq!(city_list("Turkey").len(), 81);
        assert!(city_list("Turkey").iter().any(|c| c == "İstanbul"));
        assert_eq!(weather_country_for("İzmir, Turkey"), "Turkey");
        assert_eq!(weather_country_for("Istanbul"), "Turkey");
    }

    #[test]
    fn automatic_location_only_fills_an_empty_saved_city() {
        let detected = Some(("Turkey".to_owned(), "İstanbul".to_owned()));
        assert_eq!(
            location_auto_fill_value("", &detected),
            Some("İstanbul, Turkey".to_owned())
        );
        assert_eq!(
            location_auto_fill_value("  ", &detected),
            Some("İstanbul, Turkey".to_owned())
        );
        assert_eq!(location_auto_fill_value("Izmir, Turkey", &detected), None);
        assert_eq!(location_auto_fill_value("", &None), None);
    }

    #[test]
    fn ai_cli_sign_in_wiring_maps_cli_providers_only() {
        assert_eq!(
            ai_cli_sign_in_command("aiusage_claude"),
            Some(("claude", vec!["claude", "auth", "login"]))
        );
        assert_eq!(
            ai_cli_sign_in_command("aiusage_opencode"),
            Some(("opencode", vec!["opencode", "auth", "login"]))
        );
        // API-key providers have no CLI sign-in command.
        assert!(ai_cli_sign_in_command("aiusage_deepseek").is_none());
        assert!(ai_cli_sign_in_command("aiusage_cursor").is_none());
    }

    #[test]
    fn saved_brush_token_reaches_live_palette() {
        let _guard = theme_test_guard();
        apply_token_overrides(TokenMap::new());
        let color = Rgba::from_rgb(7, 8, 9);
        let brush = build_brush(BrushMode::Solid, Hsv::from(color), color.a, 90.0, &[color]);
        let mut map = Map::new();
        map.insert("AccentBrush".to_owned(), brush_to_json(&brush));
        let mut settings = DockSettings::default();
        persist_theme_tokens(&mut settings, map.clone());
        assert_eq!(
            crate::design::theme::ThemeManager::resolve("AccentBrush"),
            Some(TokenValue::Solid(color))
        );
        // The gradient shape round-trips through the JSON parser used at startup.
        let gradient = build_brush(
            BrushMode::Gradient,
            Hsv::from(color),
            0x80,
            45.0,
            &[color, Rgba::from_rgb(40, 50, 60)],
        );
        map.insert("DockMaterial".to_owned(), brush_to_json(&gradient));
        let parsed = token_overrides_from_json(&map);
        assert!(matches!(
            parsed.get("DockMaterial"),
            Some(TokenValue::Gradient(_))
        ));
        apply_token_overrides(TokenMap::new());
    }

    #[test]
    fn settings_window_uses_the_fixed_dialog_size_and_disables_resize() {
        assert_eq!(WIN_W, 1040.0);
        assert_eq!(WIN_H, 740.0);
        let options = settings_window_options(None);
        let minimum = options
            .window_min_size
            .expect("the dialog must have a minimum size");
        assert_eq!(minimum.width, px(WIN_W));
        assert_eq!(minimum.height, px(WIN_H));
        assert!(!options.is_resizable);
        assert!(options.titlebar.is_some());
        // The retired folder-stack row must not be reachable from the search catalogue.
        let catalog = settings_search::SearchCatalog::from_settings(&DockSettings::default());
        assert!(catalog.search("StackFolders").is_empty());
        assert!(catalog.search("Add Folder Stack").is_empty());
        // Both dock hide switches stay discoverable through the WS-A parity catalogue too.
        for field in ["ShowSearchButton", "ShowTaskViewButton"] {
            assert!(
                catalog
                    .search(field)
                    .iter()
                    .any(|entry| entry.field == field),
                "{field} must be discoverable from the settings search catalogue"
            );
            assert!(
                settings_search::SETTINGS_ROWS
                    .iter()
                    .any(|row| row.3 == field),
                "{field} must be mirrored in SETTINGS_ROWS"
            );
        }
    }

    #[test]
    fn dock_hide_toggle_target_rows_match_search_row_ids() {
        // Search results jump to a rendered row by `row_id`; a mismatch silently lands nowhere.
        let source = include_str!("settings.rs");
        let rendered_ids: Vec<&str> = source
            .match_indices("self.target_row(")
            .filter_map(|(offset, _)| {
                let after = &source[offset + "self.target_row(".len()..];
                let first_arg = after.split(',').next()?.trim();
                first_arg.strip_prefix('"')?.split('"').next()
            })
            .collect();
        let catalog = settings_search::catalog();
        for row_id in ["ShowSearchButton", "ShowTaskViewButton"] {
            assert!(
                rendered_ids.contains(&row_id),
                "{row_id} is not a rendered target_row id"
            );
            let hits: Vec<_> = catalog
                .iter()
                .filter(|entry| entry.row_id == row_id)
                .collect();
            assert_eq!(hits.len(), 1, "expected one search entry for {row_id}");
            assert!(hits[0].page == SettingsPage::Dock);
            assert_eq!(hits[0].title, loc::t(row_id));
            assert_eq!(hits[0].section, loc::t("SectionInteractions"));
        }
    }

    #[test]
    fn settings_window_is_closed_when_the_dock_exits() {
        // Closing with no Settings spawned yet is a no-op and never panics; the registry is left
        // empty so a later shutdown cannot post WM_CLOSE to a recycled PID.
        close_spawned_settings_windows();
        close_spawned_settings_windows();
        assert!(
            super::SETTINGS_CHILDREN
                .lock()
                .is_ok_and(|children| children.is_empty())
        );
    }

    #[test]
    fn color_field_maps_cells_to_saturation_and_value() {
        let top_left = field_cell_hsv(210.0, 0, 0, 20, 10);
        let bottom_right = field_cell_hsv(210.0, 19, 9, 20, 10);
        assert!((top_left.h - 210.0).abs() < f64::EPSILON);
        assert!(top_left.s < 0.1 && top_left.v > 0.9);
        assert!(bottom_right.s > 0.9 && bottom_right.v < 0.1);
        assert_eq!(field_cell_hsv(0.0, 0, 0, 0, 0), Hsv::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn gradient_stop_drag_is_bounded_and_keeps_all_stops() {
        let mut stops: Vec<Rgba> = (0..16).map(|i| Rgba::from_rgb(i as u8, 0, 0)).collect();
        let target = reorder_gradient_stop(&mut stops, 2, 9);
        assert_eq!(target, 9);
        assert_eq!(stops.len(), 16);
        assert_eq!(stops[9].r, 2);
        assert_eq!(reorder_gradient_stop(&mut stops, 4, 99), 15);
        assert_eq!(gradient_stop_at(0.0, 16), 0);
        assert_eq!(gradient_stop_at(1.0, 16), 15);
        assert_eq!(gradient_stop_at(0.5, 16), 8);
    }

    #[test]
    fn retired_ai_provider_ui_is_gone() {
        // Split needle so this regression test does not reintroduce the token it forbids.
        let needle = concat!("antig", "ravity");
        assert!(settings_search::SETTINGS_ROWS.iter().all(|row| {
            !row.2.to_ascii_lowercase().contains(needle)
                && !row.3.to_ascii_lowercase().contains(needle)
                && row
                    .4
                    .iter()
                    .all(|alias| !alias.to_ascii_lowercase().contains(needle))
        }));
        let catalog = settings_search::SearchCatalog::from_settings(&DockSettings::default());
        assert!(catalog.search(needle).is_empty());
        assert!(ai_cli_sign_in_command(&format!("aiusage_{needle}")).is_none());
    }

    #[test]
    fn runtime_settings_round_trip_through_the_persisted_schema() {
        let defaults = serde_json::to_value(DockSettings::default()).unwrap();
        for key in [
            "MagnificationExpandsDock",
            "ContentAlignment",
            "PreviewsEnabled",
            "WheelVolumeEnabled",
            "FocusMinutes",
            "FocusBreakMinutes",
            "FocusSessions",
            "WidgetDensityOverrides",
            "AiUsageRing",
            "ShowMedia",
            "DockWidthMode",
            "DockHeightMode",
            "DockTopGapUnit",
        ] {
            assert!(defaults.get(key).is_some(), "missing persisted key {key}");
        }

        let settings = DockSettings {
            focus_minutes: 42,
            focus_break_minutes: 9,
            focus_sessions: 6,
            dynamic_island_background_color: Some("#010203".to_owned()),
            ..Default::default()
        };
        let value = serde_json::to_value(&settings).unwrap();
        assert_eq!(value["FocusMinutes"], 42);
        assert_eq!(value["FocusBreakMinutes"], 9);
        assert_eq!(value["FocusSessions"], 6);
        assert_eq!(value["DynamicIslandBackgroundColor"], "#010203");
        assert_eq!(settings.dynamic_island_background_hex(), Some("#010203"));

        assert_eq!(super::FOCUS_MINUTES_MAX, 90);
        assert_eq!(super::FOCUS_BREAK_MINUTES_MAX, 30);
        assert_eq!(super::FOCUS_SESSIONS_MAX, 8);
    }

    #[test]
    fn profile_snapshot_covers_full_field_contract() {
        use opentaskbar_core::settings::{DockPosition, DockWidthMode};

        let settings = DockSettings {
            position: DockPosition::Right,
            enabled_widgets: vec!["clock".to_owned()],
            user_unpinned_apps: vec!["hidden.exe".to_owned()],
            dock_width_mode: DockWidthMode::Pixels,
            dock_width_pixels: 240,
            theme_pack: "catppuccin".to_owned(),
            ..Default::default()
        };
        let profile = super::snapshot_profile(&settings, "Work".to_owned());
        assert_eq!(profile.position, DockPosition::Right);
        assert_eq!(profile.hidden_apps, ["hidden.exe"]);
        assert_eq!(profile.dock_width_pixels, Some(240));
        assert_eq!(profile.theme_pack.as_deref(), Some("catppuccin"));
        assert!(profile.launcher_hotkey.is_some() && profile.dock_hotkey.is_some());
        assert!(profile.theme_application_icons.is_some());
        assert!(profile.dock_item_order.is_empty());

        let mut target = DockSettings::default();
        opentaskbar_core::settings::apply_full_profile(&mut target, &profile);
        assert_eq!(target.position, DockPosition::Right);
        assert!(target.is_widget_enabled("clock"));
        assert_eq!(target.dock_width_pixels, 240);
    }

    #[test]
    fn update_startup_and_theme_use_wired_apis() {
        // Compile-time wiring checks: the Settings view calls these exact crate APIs. No source
        // text is inspected; an API rename fails to compile here.
        let _guard: fn() -> Result<Option<crate::platform::update::Release>, ()> =
            crate::platform::update::find_newer_packaged_guard;
        let _apply: fn(
            &crate::platform::update::DetailedRelease,
        ) -> crate::platform::update::UpdateOutcome = crate::platform::update::apply_with_elevation;
        let _set_enabled: fn(bool) -> Option<&'static str> =
            crate::platform::startup_registration::set_enabled;

        // Persisted theme tokens reach the live palette.
        let _guard = theme_test_guard();
        apply_token_overrides(TokenMap::new());
        let color = Rgba::from_rgb(11, 22, 33);
        let brush = build_brush(BrushMode::Solid, Hsv::from(color), color.a, 90.0, &[color]);
        let mut tokens = Map::new();
        tokens.insert("AccentBrush".to_owned(), brush_to_json(&brush));
        let mut settings = DockSettings::default();
        persist_theme_tokens(&mut settings, tokens);
        assert_eq!(
            crate::design::theme::ThemeManager::resolve("AccentBrush"),
            Some(TokenValue::Solid(color))
        );
        apply_token_overrides(TokenMap::new());

        // Startup toggles use set_enabled, verify with read(), and surface the error key.
        assert_eq!(
            resolve_startup_toggle(true, None, (true, None)),
            (true, None)
        );
        assert_eq!(
            resolve_startup_toggle(true, Some("StartupWriteError"), (false, None)),
            (false, Some("StartupWriteError"))
        );
        assert_eq!(
            resolve_startup_toggle(false, None, (true, Some("StartupReadError"))),
            (false, Some("StartupReadError"))
        );
        assert_eq!(
            resolve_startup_toggle(true, None, (false, None)),
            (false, Some("StartupWriteError")),
            "an unconfirmed write must not leave the switch in the requested position"
        );
    }

    #[test]
    fn persist_theme_tokens_preserves_active_theme_pack_layer() {
        use crate::design::theme::apply_pack_tokens_to_palette;

        let _guard = theme_test_guard();
        apply_pack_tokens_to_palette(TokenMap::new());
        apply_token_overrides(TokenMap::new());

        let pack_surface = Rgba::from_rgb(24, 34, 44);
        let mut pack_tokens = TokenMap::new();
        pack_tokens.insert(
            "SurfaceBackground".to_owned(),
            TokenValue::Solid(pack_surface),
        );
        apply_pack_tokens_to_palette(pack_tokens);

        let accent = Rgba::from_rgb(99, 120, 240);
        let brush = build_brush(
            BrushMode::Solid,
            Hsv::from(accent),
            accent.a,
            90.0,
            &[accent],
        );
        let mut user_tokens = Map::new();
        user_tokens.insert("AccentBrush".to_owned(), brush_to_json(&brush));
        let mut settings = DockSettings::default();
        persist_theme_tokens(&mut settings, user_tokens);

        assert_eq!(
            crate::design::theme::ThemeManager::resolve("SurfaceBackground"),
            Some(TokenValue::Solid(pack_surface)),
            "persisting user token overrides must not wipe the active theme pack layer"
        );
        assert_eq!(
            crate::design::theme::ThemeManager::resolve("AccentBrush"),
            Some(TokenValue::Solid(accent))
        );

        apply_token_overrides(TokenMap::new());
        apply_pack_tokens_to_palette(TokenMap::new());
    }

    #[test]
    fn parse_hex_color_accepts_rgb_and_argb() {
        assert_eq!(super::parse_hex_color("#112233"), Some(0x1122_33FF));
        assert_eq!(super::parse_hex_color("#80112233"), Some(0x1122_3380));
        assert_eq!(super::parse_hex_color("  #AA010203  "), Some(0x0102_03AA));
        assert_eq!(super::parse_hex_color("112233"), Some(0x1122_33FF));
        assert_eq!(super::parse_hex_color("#12345"), None);
        assert_eq!(super::parse_hex_color("#GGGGGG"), None);
    }

    #[test]
    fn widget_has_card_drawer_covers_all_7_ai_and_22_builtin_widgets() {
        assert_eq!(crate::widgets::telemetry::AI_PROVIDERS.len(), 7);
        for provider in crate::widgets::telemetry::AI_PROVIDERS {
            assert!(
                super::widget_has_card_drawer(provider.id),
                "missing card drawer for AI provider {}",
                provider.id
            );
        }
        let catalog: Vec<_> = crate::widgets::catalog_widgets().collect();
        assert!(catalog.len() >= 22);
        for widget in catalog {
            assert!(
                super::widget_has_card_drawer(widget.id),
                "missing card drawer for catalog widget {}",
                widget.id
            );
        }
    }
}

/// Window options for the Settings dialog, kept as a function so the fixed-size/resize contract
/// can be tested directly instead of by scanning this file's source text.
fn settings_window_options(window_bounds: Option<WindowBounds>) -> WindowOptions {
    // Fixed dialog: min == initial size. The initial size may have been clamped below
    // WIN_W×WIN_H to fit a small work area, so the minimum never exceeds it.
    let min_size = match window_bounds {
        Some(WindowBounds::Windowed(bounds)) => size(
            bounds.size.width.min(px(WIN_W)),
            bounds.size.height.min(px(WIN_H)),
        ),
        _ => size(px(WIN_W), px(WIN_H)),
    };
    WindowOptions {
        window_bounds,
        window_min_size: Some(min_size),
        // Opaque makes GPUI pass ACCENT_DISABLED directly instead of
        // ACCENT_ENABLE_TRANSPARENTGRADIENT (0x00000000), while keeping GPUI's
        // DXGI_ALPHA_MODE_PREMULTIPLIED swapchain so rounded corners never turn black.
        window_background: WindowBackgroundAppearance::Opaque,
        // Resizing (edge/corner handle) stays off.
        is_resizable: false,
        titlebar: Some(TitlebarOptions {
            title: Some(loc::t("SettingsTitle").into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        ..Default::default()
    }
}

#[allow(clippy::items_after_test_module)]
pub fn run_settings_window() {
    // Single-instance Settings even for external launches: a second `--settings` process
    // focuses the existing window and exits. The lock lives until this function returns.
    let Some(_instance_lock) = window_chrome::acquire_settings_instance() else {
        return;
    };
    let loaded = settings::load_from(&settings::settings_path());
    Application::new()
        .with_assets(fonts::EmbeddedAssets)
        .run(move |cx: &mut App| {
            let _ = fonts::load_bundled_fonts(cx);
            cx.bind_keys([
                KeyBinding::new("tab", TabNext, None),
                KeyBinding::new("shift-tab", TabPrevious, None),
                KeyBinding::new("escape", CloseWidgetPanel, None),
                KeyBinding::new("f9", ToggleFlyoutPreview, None),
            ]);
            // Fit the fixed dialog into the primary work area (high DPI on small screens) and
            // center it there instead of on the full display (which ignores the taskbar).
            let work = cx
                .primary_display()
                .and_then(|display| window_chrome::primary_work_area(display.bounds()));
            let win_size = fit_settings_size(work.map(|w| w.size));
            let bounds = match work {
                Some(work) => Bounds::centered_at(work.center(), win_size),
                None => Bounds::centered(None, win_size, cx),
            };
            let _ = cx.open_window(
                settings_window_options(Some(WindowBounds::Windowed(bounds))),
                |window, cx| cx.new(|cx| SettingsView::new(loaded.clone(), window, cx)),
            );
            cx.activate(true);
        });
}
