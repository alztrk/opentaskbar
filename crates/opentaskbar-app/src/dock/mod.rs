//! Native Win32 + DirectComposition floating dock window (Hybrid Architecture):
//! - Dock strip runs on pure DirectComposition visual transforms (0-1% GPU at 60-180 FPS)
//! - Settings window runs on demand in GPUI (`--settings`), freeing 100% of GPUI RAM on close
//! - Full multi-monitor support (per-monitor DPI + signed coordinates for left-of-primary screens)
//! - Auto-Hide (`Always` & `Smart` overlap detection) with smooth VSync slide-in/out
//! - Native live DWM window thumbnail previews (`DwmRegisterThumbnail`)
//! - Per-app / Task View / Dock context menus, Shift+Click / Ctrl+Shift+Click / Middle-click / Wheel
//! - File drag-and-drop (`WM_DROPFILES`), Dark/Light/System theme, Clock & CPU/RAM widgets, Show Desktop
//!
//! Edge parity (WS-D): `layout::window_rect_for_edge` places Left/Right docks along the
//! monitor's Y axis, and scroll, magnification, auto-hide, resize, show-desktop strip and
//! edge trigger all follow `DockEdge`. The DirectComposition visual tree is still laid out
//! along X, so a full 90-degree visual rotation of the pill contents remains a follow-up.
//!
//! Integration notes:
//! - `window_picker::show(...)` is opened from app-click, group-member and
//!   secondary-mirror activation paths.
//! - `settings::{effective_width, effective_height, effective_top_gap,
//!   effective_bottom_gap, widget_density_for}` are resolved once per settings
//!   load into `EffectiveMetrics` so the mouse hot path never re-reads disk.

use crate::platform::win32_helpers::set_window_long_ptr;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::{
    self, AppTheme, AutoHideMode, DockContentAlignment, DockHeightMode, DockSettings,
    DockWidthMode, MiddleClickAction, StartButtonAction, StartButtonPlacement, SurfaceBackground,
};
use opentaskbar_shell::flyout_reposition::FlyoutKind;
use opentaskbar_shell::tracker::{self, RunningWindow};
use windows::Win32::Graphics::DirectComposition::{
    IDCompositionRectangleClip, IDCompositionSurface,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmFlush, DwmGetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    MONITOR_DEFAULTTONEAREST, MonitorFromWindow, ScreenToClient,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, UnregisterHotKey, VK_CONTROL, VK_ESCAPE, VK_LEFT, VK_LWIN, VK_RETURN,
    VK_RIGHT, VK_RWIN, VK_SHIFT, VK_TAB,
};
use windows_sys::Win32::UI::Shell::DragAcceptFiles;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CS_DBLCLKS, CallNextHookEx, CreateWindowExW,
    DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    HHOOK, HTCLIENT, HTTRANSPARENT, HWND_BROADCAST, IDC_ARROW, IDC_HAND, IDC_SIZEWE, IsIconic,
    IsWindow, IsWindowVisible, IsZoomed, KillTimer, LoadCursorW, MSG, MSLLHOOKSTRUCT, PostMessageW,
    PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetCursor, SetForegroundWindow,
    SetTimer, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_APP, WM_DESTROY, WM_DROPFILES, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WS_VISIBLE, WindowFromPoint,
};

pub mod dcomp;
pub mod dynamic_island;
pub mod layout;
pub mod preview;
pub mod show_desktop;
pub mod tray;
pub mod window_picker;

use self::dcomp::{DcompEngine, PillNodes, ShellGlyph, VisualNode, WidgetInteraction};
use self::dynamic_island::{InputDisposition, IslandInput, IslandState};
use self::layout::{
    CONTENT_EDGE_PAD_DIPS, MAX_FISHEYE_SCALE, anchor_from_window_rect, bounce_direction,
    capture_expansion_budget, centered_shell_rect, compute_preview_clearance, compute_safe_lift,
    compute_slot_extras, content_alignment_offset, content_strip_width, drop_targets_recycle_bin,
    effective_animations_enabled, effective_edge, escape_closes_transient, fisheye_enabled,
    fisheye_target_scale, folder_stack_preview_cells, group_reorder, has_scroll_overflow,
    hide_offset, insertion_index, is_full_screen_on_monitor, is_injected_key_event,
    keyboard_next_index, magnification_expands_dock, revealed_guard_should_hide, section_width,
    shell_gap, show_search_button, show_task_view_button, slot_gap, start_menu_hide_fallback,
    step_shell_spring, use_hover_icon, vsync_tick_allowed, wheel_scroll_step, wheel_to_axis_delta,
    widget_gap, win_key_modifier_pressed, win_key_reveal_allowed, win_key_reveal_enabled,
};
use self::preview::{PreviewItem, PreviewPopup};
use self::show_desktop::ShowDesktopCorner;
use crate::design;
use crate::platform::icon_cache::{self, RawIcon};
use crate::platform::pins::{self, PinnedApp};
use crate::platform::shell_files;
use crate::platform::tray_host;
use crate::platform::win32::{
    self, AppContextAction, ContextAction, MonitorInfo, RecycleBinAction, TaskViewAction,
    WidgetContextAction,
};
use crate::ui::flyout::{FlyoutBlock, FlyoutEdge, FlyoutIcon, FlyoutOptions, FlyoutSpec};
use crate::ui::menu::{self, BorderStyle, MenuEntry};
use crate::ui::settings as settings_ui;
use crate::widgets::{self, HOVER_INTENT_MS, HoverChange, HoverPhase, WidgetRuntime, registry};

const VERTICAL_EDGE_GAP: f32 = 5.0;
const TOOLTIP_HEADROOM: f32 = 36.0;
const MAG_HEADROOM: f32 = 26.0;
// The magnification peak is read from `settings.fisheye_intensity.peak()`;
// WS-D's expansion budget trims it to the free pill space at runtime.
const BOUNCE_DURATION_SECS: f32 = 0.42;
const CLOSE_DURATION: Duration = Duration::from_millis(240);
const DRAG_THRESHOLD_PX: f32 = 6.0;

const TIMER_POLL: usize = 1;
const TIMER_PREVIEW_SHOW: usize = 2;
const TIMER_PREVIEW_HIDE: usize = 3;
const TIMER_WIDGETS: usize = 4;
const TIMER_PREVIEW_WATCH: usize = 5;
const TIMER_WINDOW_CLOSE: usize = 6;
const TIMER_WIDGET_ANIM: usize = 7;
const TIMER_REVEALED_GUARD: usize = 8;
const TIMER_DRAG_HOVER: usize = 9;
const TIMER_HOVER_INTENT: usize = 10;
/// `DockMetrics.AutoHideDelayMs`: delayed hide after the pointer leaves.
const TIMER_HIDE_DELAY: usize = 11;
const RESIZE_EDGE_DIPS: f32 = 8.0;
const SCROLL_EDGE_W_DIPS: f32 = 28.0;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MOUSEHWHEEL: u32 = 0x020E;
const WM_APP_VSYNC: u32 = WM_APP + 1;
const WM_APP_ICONS: u32 = WM_APP + 2;
const WM_APP_PREVIEW_LEAVE: u32 = WM_APP + 3;
const WM_APP_WINKEY_REVEAL: u32 = WM_APP + 4;
/// Idle-side settings reload: keeps disk I/O and the full DComp rebuild out of
/// the hover/mouse/timer hot path.
const WM_APP_SETTINGS_RELOAD: u32 = WM_APP + 5;
/// `WH_MOUSE_LL` posts only the cursor point; the UI thread hit-tests it against
/// its own dock rects (no `WindowFromPoint` from a low-level hook).
const WM_APP_WHEEL_ROUTE: u32 = WM_APP + 6;
/// EVENT_SYSTEM_ALERT queue drain: refresh notification badges on every dock.
const WM_APP_NOTIFICATION_BADGE: u32 = WM_APP + 7;
/// Posted by Search / Control Center open paths. The UI thread resolves the
/// per-monitor anchor and schedules the reposition worker; the open path only
/// posts, so it never blocks on settings I/O or shell animation frames.
const WM_APP_SHELL_FLYOUT: u32 = WM_APP + 8;
const WM_HOTKEY: u32 = 0x0312;
const WM_CLOSE: u32 = 0x0010;
const HOTKEY_LAUNCHER_ID: i32 = 0x4F01;
const HOTKEY_DOCK_ID: i32 = 0x4F02;
/// Integration menu ids live above every dock/app menu id so a native command
/// can never be mistaken for a dock context action.
const NATIVE_APP_MENU_ID_BASE: u32 = 7000;
const GROUP_FLYOUT_ROW_ID_BASE: u32 = 100;
/// Folder-stack flyout: footer "Open in File Explorer" button and item rows.
const STACK_FLYOUT_OPEN_FOLDER_ID: u32 = 1;
const STACK_FLYOUT_ROW_ID_BASE: u32 = 100;
/// The scroll viewport sits 1 DIP inside each pill edge (`vp_left = pill_left + 1`).
/// The pill must include both insets or a dock that fits on screen reports a
/// bogus 2 DIP overflow (and a right scroll chevron) at rest.
const VIEWPORT_INSET_DIPS: f32 = 1.0;
/// `FILE_ATTRIBUTE_HIDDEN` / `FILE_ATTRIBUTE_SYSTEM` for folder-stack filtering.
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
/// Runtime-resolved settings applied once per settings load (never on the mouse
/// hot path). The core `effective_*` functions own the resolution; the dock
/// caches their answers so a 1000 Hz mouse stream never re-reads disk.
#[derive(Clone, Debug, Default)]
struct EffectiveMetrics {
    width_dips: Option<f32>,
    height_dips: Option<f32>,
    top_gap_dips: Option<i32>,
    bottom_gap_dips: Option<i32>,
    widget_densities: HashMap<String, f32>,
    /// Live resting content width (shell, unmagnified apps, live widget widths,
    /// trail, and viewport insets). Lets the window grow with dynamic widget widths
    /// (Clock seconds, Stats columns, ...) that the static descriptors don't know.
    live_rest_content_dips: Option<f32>,
}

impl EffectiveMetrics {
    fn for_monitor(settings: &DockSettings, mon_w_dips: f32) -> Self {
        let positive = |v: f32| (v.is_finite() && v > 0.0).then_some(v);
        let mut widget_densities = HashMap::new();
        for wd in widgets::active_widgets(settings) {
            let density = settings::widget_density_for(settings, wd.id);
            if density.is_finite() && density > 0.0 {
                widget_densities.insert(wd.id.to_owned(), density);
            }
        }
        // Only override content-fitted dimensions when the user selected an explicit
        // fixed/fill mode; `Auto` lets `resting_pill_width` / `pill_height_dips` size
        // the dock naturally so icons and widgets never trigger false scrollbars.
        let width_dips = if settings.dock_width_mode == DockWidthMode::Auto {
            None
        } else {
            positive(settings::effective_width(settings, mon_w_dips))
        };
        let height_dips = match settings.dock_height_mode {
            DockHeightMode::Pixels | DockHeightMode::Percent => {
                positive(settings::effective_height(settings))
            }
            DockHeightMode::Auto | DockHeightMode::Fill => None,
        };
        Self {
            width_dips,
            height_dips,
            top_gap_dips: Some(settings::effective_top_gap(settings)),
            bottom_gap_dips: Some(settings::effective_bottom_gap(settings)),
            widget_densities,
            live_rest_content_dips: None,
        }
    }

    fn bottom_inset_dips(&self) -> f32 {
        self.bottom_gap_dips
            .map_or(VERTICAL_EDGE_GAP, |gap| gap.max(0) as f32)
    }

    fn widget_density(&self, id: &str) -> f32 {
        self.widget_densities
            .get(id)
            .copied()
            .unwrap_or(1.0)
            .clamp(0.45, 1.35)
    }
}

/// Vertically centers app icons inside the dock pill (matching shell buttons), while reserving
/// clean room at the bottom for the running/active indicator dot inside the pill clip.
fn app_icon_vertical_metrics(pill_top: f32, pill_h: f32, icon_size: f32) -> (f32, f32, f32) {
    let top = pill_top + ((pill_h - icon_size) * 0.5 - 1.5).max(4.0);
    let bottom = top + icon_size;
    // Running/active indicator sits 2 DIP higher than before so it never hugs
    // (or gets clipped by) the pill's rounded bottom edge.
    let dot_top = (bottom + 3.0).min(pill_top + pill_h - 7.5);
    (top, bottom, dot_top)
}

/// Returns `(show_clock_widget, show_system_stats_widget)` for a profile's
/// `enabled_widgets` list, matching canonical, legacy and mixed-case IDs
/// (`Clock`, `clock`, `SystemStats`, `stats`, ...).
fn profile_widget_flags(enabled_widgets: &[String]) -> (bool, bool) {
    let matches = |target: &str| {
        enabled_widgets
            .iter()
            .any(|w| settings::canonical_widget_setting_id(w).eq_ignore_ascii_case(target))
    };
    (matches("Clock"), matches("SystemStats"))
}

/// Reads one folder stack: visible (non-hidden, non-system, non-temp) children.
/// `None` when the folder cannot be read.
fn read_stack_entries(path: &str) -> Option<Vec<layout::StackEntry>> {
    use std::os::windows::fs::MetadataExt;
    let dir = Path::new(path);
    let entries = std::fs::read_dir(dir).ok()?;
    let mut out = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let attrs = meta.file_attributes();
        if !layout::stack_entry_visible(
            &name,
            attrs & FILE_ATTRIBUTE_HIDDEN != 0,
            attrs & FILE_ATTRIBUTE_SYSTEM != 0,
        ) {
            continue;
        }
        let modified_secs = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_secs());
        out.push(layout::StackEntry {
            path: entry.path().to_string_lossy().into_owned(),
            name,
            is_dir: meta.is_dir(),
            modified_secs,
        });
    }
    Some(out)
}

/// Number of visible items in a folder stack (tooltip suffix).
fn stack_item_count(path: &str) -> Option<usize> {
    read_stack_entries(path).map(|entries| entries.len())
}

fn open_folder_in_explorer(path: &str) {
    pins::launch_with_args("explorer.exe".to_owned(), format!("\"{path}\""));
}

/// Registered `WM_APP_SETTINGS_RELOAD` value, shared by every dock window.
static RELOAD_MSG: AtomicU32 = AtomicU32::new(0);

/// Applies all runtime settings to the live dock windows. Called by the settings
/// process / orchestrator after a settings write; it resolves the WS-1 effective
/// width/height/gap/density helpers and asks each dock to rebuild on its UI thread.
pub fn apply_runtime_settings() {
    let settings = settings::load_from(&settings::settings_path());
    let msg = RELOAD_MSG.load(Ordering::Relaxed);
    if msg != 0 {
        unsafe {
            let _ = PostMessageW(HWND_BROADCAST, msg, 0, 0);
        }
    }
    // Settings can be written by another process while the picker's nested
    // modal loop owns this process's dispatcher. Broadcast the close signal so
    // the picker HWND receives it directly (registered messages only reach our
    // own windows; WM_APP+9 is private to this app family).
    unsafe {
        let _ = PostMessageW(HWND_BROADCAST, window_picker::PICKER_CLOSE_MSG, 0, 0);
    }
    window_picker::set_picker_enabled(settings.previews_enabled);
    // Keep the application language in sync even when no dock is currently open.
    loc::set_language(&settings.language);
}

/// One dock window's shell-button rect, in physical screen coordinates. The
/// `FlyoutKind` tag keeps Start/Search/ControlCenter anchors independent: each
/// native flyout owns its own generation and its own per-monitor selection.
type AnchorEntry = (usize, FlyoutKind, (i32, i32, i32, i32));

/// Physical rects of every dock's shell buttons, keyed by (dock HWND, kind).
/// `on_opened` (WS-B) runs on the WinEvent thread, so the registry is shared
/// rather than thread-local; the primary dock is registered first.
static SHELL_BUTTON_ANCHORS: Mutex<Vec<AnchorEntry>> = Mutex::new(Vec::new());
/// Gate shared with the global low-level keyboard hook, which has no `DockState`.
static WIN_KEY_REVEAL_GATE: AtomicBool = AtomicBool::new(true);
/// A Start-button toggle is ignored within this window to stop a double click
/// (or a cloak + pointer-up race) from opening and immediately closing Start.
const START_TOGGLE_DEBOUNCE: Duration = Duration::from_millis(300);

fn anchor_is_valid(anchor: (i32, i32, i32, i32)) -> bool {
    anchor.2 > anchor.0 && anchor.3 > anchor.1
}

/// Hover must agree with the island's case-insensitive flyout lookup: legacy
/// ids (`Clock`, `AiUsageClaude`, `CUSTOM:Usd-Try`) are valid settings keys but
/// the registry's `widget_has_flyout` lookup is case-sensitive.
fn widget_has_flyout_ci(id: &str) -> bool {
    let canonical = registry::canonical_widget_id(id);
    widgets::widget_has_flyout(canonical)
        || registry::widget_descriptor(canonical).is_some_and(|widget| widget.has_flyout)
        || widgets::widget_has_flyout(&id.to_ascii_lowercase())
}

fn first_valid_anchor(anchors: &[AnchorEntry], kind: FlyoutKind) -> (i32, i32, i32, i32) {
    anchors
        .iter()
        .find_map(|(_, k, rect)| (*k == kind && anchor_is_valid(*rect)).then_some(*rect))
        .unwrap_or((0, 0, 0, 0))
}

/// Picks the button rect of the dock on the monitor hosting `hwnd`, for one
/// specific flyout kind. `monitor_of` returns 0 for an invalid handle, which
/// falls back to the first valid dock of that kind. Kept pure so multi-monitor
/// and per-kind selection are unit testable.
fn select_anchor_for(
    anchors: &[AnchorEntry],
    kind: FlyoutKind,
    hwnd: usize,
    monitor_of: impl Fn(usize) -> usize,
) -> (i32, i32, i32, i32) {
    let fallback = first_valid_anchor(anchors, kind);
    if hwnd == 0 {
        return fallback;
    }
    let target = monitor_of(hwnd);
    if target == 0 {
        return fallback;
    }
    anchors
        .iter()
        .find(|(dock_hwnd, k, rect)| {
            *k == kind && anchor_is_valid(*rect) && monitor_of(*dock_hwnd) == target
        })
        .map(|(_, _, rect)| *rect)
        .unwrap_or(fallback)
}

/// Screen rect of the button for `kind` most recently laid out by any dock.
/// `(0, 0, 0, 0)` means no visible button has been laid out yet.
pub fn shell_button_anchor(kind: FlyoutKind) -> (i32, i32, i32, i32) {
    SHELL_BUTTON_ANCHORS
        .lock()
        .map(|anchors| first_valid_anchor(&anchors, kind))
        .unwrap_or((0, 0, 0, 0))
}

/// Per-monitor variant used by the flyout callbacks: a global single anchor
/// puts the flyout on the wrong screen when docks exist on several monitors.
/// `hwnd == 0` (unknown flyout HWND) falls back to the first valid dock.
pub fn shell_button_anchor_for(hwnd: usize, kind: FlyoutKind) -> (i32, i32, i32, i32) {
    let Ok(anchors) = SHELL_BUTTON_ANCHORS.lock() else {
        return (0, 0, 0, 0);
    };
    let monitor_of = |handle: usize| -> usize {
        if handle == 0 {
            return 0;
        }
        // SAFETY: monitor queries tolerate stale handles and return NULL.
        unsafe { MonitorFromWindow(handle as HWND, MONITOR_DEFAULTTONEAREST) as usize }
    };
    select_anchor_for(&anchors, kind, hwnd, monitor_of)
}

/// Screen rect of the Start button most recently laid out by any dock window.
pub fn start_button_anchor() -> (i32, i32, i32, i32) {
    shell_button_anchor(FlyoutKind::Start)
}

/// Back-compatible per-monitor Start lookup used by `on_opened`.
pub fn start_button_anchor_for(hwnd: usize) -> (i32, i32, i32, i32) {
    shell_button_anchor_for(hwnd, FlyoutKind::Start)
}

fn publish_shell_button_anchor(hwnd: usize, kind: FlyoutKind, anchor: (i32, i32, i32, i32)) {
    if let Ok(mut anchors) = SHELL_BUTTON_ANCHORS.lock() {
        if let Some(entry) = anchors
            .iter_mut()
            .find(|(owner, k, _)| *owner == hwnd && *k == kind)
        {
            entry.2 = anchor;
        } else {
            anchors.push((hwnd, kind, anchor));
        }
    }
}

/// Wire codes used by `WM_APP_SHELL_FLYOUT`: stable, distinct per kind, and
/// deliberately small so the message payload stays obvious at every call site.
fn flyout_code(kind: FlyoutKind) -> usize {
    match kind {
        FlyoutKind::Start => 0,
        FlyoutKind::Search => 1,
        FlyoutKind::ControlCenter => 2,
    }
}

fn flyout_kind_from_code(code: usize) -> FlyoutKind {
    match code {
        1 => FlyoutKind::Search,
        2 => FlyoutKind::ControlCenter,
        _ => FlyoutKind::Start,
    }
}

fn set_win_key_reveal_gate(settings: &DockSettings) {
    WIN_KEY_REVEAL_GATE.store(
        win_key_reveal_enabled(settings.reveal_dock_on_win_key, settings.auto_hide),
        Ordering::Relaxed,
    );
}

/// Restores focus to the app that owned the foreground before Start was opened.
/// `AllowSetForegroundWindow` + `AttachThreadInput` cover the usual foreground
/// lock when the shell currently owns the foreground. Both fallback results are
/// checked so a failed attach is never reported as a successful focus return.
fn restore_app_foreground(hwnd: usize, start_hwnd: usize) {
    // Never hand focus back to the Start flyout itself.
    if hwnd == 0 || hwnd == start_hwnd {
        return;
    }
    unsafe {
        let target = hwnd as HWND;
        if IsWindow(target) == 0 {
            return;
        }
        AllowSetForegroundWindow(ASFW_ANY);
        let mut granted = SetForegroundWindow(target) != 0;
        if !granted {
            let foreground = GetForegroundWindow();
            if foreground != target {
                let target_thread = GetWindowThreadProcessId(target, std::ptr::null_mut());
                let foreground_thread = GetWindowThreadProcessId(foreground, std::ptr::null_mut());
                if target_thread != 0
                    && foreground_thread != 0
                    && target_thread != foreground_thread
                    && AttachThreadInput(foreground_thread, target_thread, 1) != 0
                {
                    granted = SetForegroundWindow(target) != 0;
                    let _ = AttachThreadInput(foreground_thread, target_thread, 0);
                }
            }
        }
        if GetForegroundWindow() != target {
            opentaskbar_core::log::warn(format!(
                "Focus return failed for {hwnd:#x} (SetForegroundWindow={granted})."
            ));
        }
    }
}

/// A Start window that was already foreground when the button went down is not
/// a valid restore target; keep the real app saved before the menu opened.
fn restore_target_for_press(foreground: usize, start_hwnd: usize, dock_hwnd: usize) -> usize {
    if foreground == 0 || foreground == start_hwnd || foreground == dock_hwnd {
        0
    } else {
        foreground
    }
}

fn start_toggle_allowed(last_toggle: Option<Instant>, now: Instant) -> bool {
    last_toggle.is_none_or(|last| now.saturating_duration_since(last) >= START_TOGGLE_DEBOUNCE)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn loword_i16(v: isize) -> f32 {
    (v as u32 & 0xffff) as i16 as f32
}

fn hiword_i16(v: isize) -> f32 {
    ((v as u32 >> 16) & 0xffff) as i16 as f32
}

fn is_shell_button_visible(settings: &DockSettings, kind: ShellButton) -> bool {
    match kind {
        ShellButton::Start => {
            settings.show_start_button && settings.start_placement != StartButtonPlacement::Hidden
        }
        ShellButton::Search => show_search_button(settings),
        ShellButton::TaskView => show_task_view_button(settings),
    }
}

fn is_lead_shell_button(settings: &DockSettings, kind: ShellButton) -> bool {
    is_shell_button_visible(settings, kind)
        && (kind != ShellButton::Start
            || layout::start_strip_group(settings) == layout::StartStripGroup::Head)
}

fn lead_shell_count(settings: &DockSettings) -> usize {
    ShellButton::ALL
        .iter()
        .filter(|&&kind| is_lead_shell_button(settings, kind))
        .count()
}

/// Width of the lead shell group. When Start is `End` / `InWidgets` it leaves
/// this group and is laid out with the trailing / widget group instead.
fn shell_lead_width(settings: &DockSettings) -> f32 {
    let base = layout::size_profile(settings).cell;
    let count = lead_shell_count(settings);
    if count == 0 {
        0.0
    } else {
        section_width(base, shell_gap(settings), count) + 17.0
    }
}

fn shell_section_width(settings: &DockSettings) -> f32 {
    let base = layout::size_profile(settings).cell;
    let count = ShellButton::ALL
        .iter()
        .filter(|&&k| is_shell_button_visible(settings, k))
        .count();
    if count == 0 {
        0.0
    } else {
        section_width(base, shell_gap(settings), count) + 17.0
    }
}

fn widgets_strip_width(settings: &DockSettings, has_apps: bool) -> f32 {
    // `active_widgets` already honours the Settings toggle for every widget
    // (Media included); a second `show_media` gate hid a freshly enabled Media chip.
    let active = widgets::active_widgets(settings);
    let start_in_widgets = layout::start_strip_group(settings)
        == layout::StartStripGroup::InWidgets
        && is_shell_button_visible(settings, ShellButton::Start);
    if active.is_empty() && !start_in_widgets {
        return 0.0;
    }
    let gap_before = if has_apps { 8.0 } else { 0.0 };
    let cards_w: f32 = active
        .iter()
        .map(|wd| wd.width_dips + widget_gap(settings))
        .sum();
    gap_before
        + cards_w
        + if start_in_widgets {
            layout::size_profile(settings).cell + widget_gap(settings)
        } else {
            0.0
        }
}

fn base_trail_width(settings: &DockSettings) -> f32 {
    base_trail_width_for(settings, stack_folder_count(settings))
}

/// Folder stacks rendered as individual dock icons (one per non-empty path).
fn stack_folder_count(settings: &DockSettings) -> usize {
    settings
        .stack_folders
        .iter()
        .filter(|path| !path.trim().is_empty())
        .count()
}

/// Trailing group `| [Folder...] [PinnedTray...] [Tray] [Bin]` without the
/// pinned tray icons: every folder stack, Tray and Bin is one `base x base` cell.
fn base_trail_width_for(settings: &DockSettings, stack_count: usize) -> f32 {
    let mut w = 0.0;
    let base = layout::size_profile(settings).cell;
    if settings.show_tray_button {
        w += base + widget_gap(settings);
    }
    if settings.show_recycle_bin {
        w += base + widget_gap(settings);
    }
    w += stack_count as f32 * (base + widget_gap(settings));
    if w > 0.0 {
        w + 13.0 // left TrayGroupDivider + breathing room
    } else {
        0.0
    }
}

fn tray_pin_cell(base: f32) -> f32 {
    (base * 0.78).max(24.0)
}

fn visible_tray_pin_count(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> usize {
    if !settings.show_tray_button {
        return 0;
    }
    let base = layout::size_profile(settings).cell;
    let apps = section_width(base, slot_gap(settings), slot_count);
    let magnification = if slot_count > 0 && fisheye_enabled(settings) {
        base * (1.0 + settings.fisheye_intensity.peak())
    } else {
        0.0
    };
    let available = mon_w_dips
        - 16.0
        - shell_section_width(settings)
        - widgets_strip_width(settings, slot_count > 0)
        - base_trail_width(settings)
        - apps
        - magnification
        - 30.0;
    ((available.max(0.0) / tray_pin_cell(base)).floor() as usize)
        .min(settings.user_pinned_background_apps.len())
}

fn widgets_section_width(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> f32 {
    widgets_strip_width(settings, slot_count > 0)
        + base_trail_width(settings)
        + visible_tray_pin_count(settings, slot_count, mon_w_dips) as f32
            * tray_pin_cell(layout::size_profile(settings).cell)
}

fn tray_pin_rect(tray: DipRect, cell: f32, gap: f32, count: usize, index: usize) -> DipRect {
    let right = tray.0 - gap - (count - 1 - index) as f32 * cell;
    (right - cell, tray.1, right, tray.3)
}

struct WidgetVisual {
    id: &'static str,
    width_dips: f32,
    bg: VisualNode,
    node: VisualNode,
    last_key: String,
}

/// One folder stack drawn as its own native folder icon in the trailing group.
struct StackVisual {
    path: String,
    label: String,
    /// Hover/press backdrop (inside the pill clip).
    bg: VisualNode,
    /// Folder icon (in the headroom-capable scroll container so magnification
    /// can lift it above the pill).
    icon: VisualNode,
    bound_source: String,
    bound_has_icon: bool,
    scale: f32,
    scale_velocity: f32,
    /// `(read_at, visible item count)` for the hover tooltip (2 s TTL).
    cached_count: Option<(Instant, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeEdge {
    Left,
    Right,
    /// Side docks resize vertically by dragging their top/bottom edge.
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug)]
struct ScrollMetrics {
    vp_left: f32,
    vp_right: f32,
    max_scroll: f32,
    offset: f32,
    shell_start_x: f32,
    apps_start_x: f32,
    right_anchor_x: f32,
    can_scroll_left: bool,
    can_scroll_right: bool,
}

/// Computes full-pill horizontal scroll viewport & non-overlapping shell/apps/widgets layout.
/// `vp_left..vp_right` sits flush inside the dock pill (`pill_left + 1 .. pill_left + pill_w - 1`),
/// with `CONTENT_EDGE_PAD_DIPS` inner padding on both ends. Content width comes from the same
/// [`content_strip_width`] helper as pill auto-sizing (#185).
fn compute_scroll_metrics(
    vp_left: f32,
    vp_right: f32,
    apps_w: f32,
    right_w: f32,
    shell_w: f32,
    scroll_offset: f32,
    alignment: DockContentAlignment,
) -> ScrollMetrics {
    let vp_w = (vp_right - vp_left).max(1.0);
    let content_w = content_strip_width(shell_w, apps_w, right_w, 0.0);
    let max_scroll = (content_w - vp_w).max(0.0);
    let offset = scroll_offset.clamp(0.0, max_scroll);
    // The leading shell group (and therefore Start) is pinned to the inner edge.
    // `content_alignment` only shifts the *trailing* anchor, so growing spacing
    // can never drag Start left. A scrolling strip always starts at the edge.
    let shell_start_x = vp_left + CONTENT_EDGE_PAD_DIPS - offset;
    let apps_start_x = shell_start_x + shell_w;
    let trailing_shift = content_alignment_offset(vp_w, content_w, alignment);
    let right_anchor_x = if max_scroll > 0.5 {
        vp_left + content_w - CONTENT_EDGE_PAD_DIPS - offset
    } else {
        (vp_left + content_w - CONTENT_EDGE_PAD_DIPS - offset + trailing_shift)
            .min(vp_right - CONTENT_EDGE_PAD_DIPS)
    };
    ScrollMetrics {
        vp_left,
        vp_right,
        max_scroll,
        offset,
        shell_start_x,
        apps_start_x,
        right_anchor_x,
        can_scroll_left: max_scroll > 0.5 && offset > 1.0,
        can_scroll_right: max_scroll > 0.5 && offset < max_scroll - 1.0,
    }
}

/// Pill width that exactly holds a content strip plus both viewport insets
/// (P0-4). Shared by window sizing, live pill geometry and the tests.
fn pill_width_for_content(content_strip_w: f32) -> f32 {
    (content_strip_w + VIEWPORT_INSET_DIPS * 2.0).max(160.0)
}

/// Scroll viewport inside a pill: flush with its rounded edges, 1 DIP inset.
fn pill_viewport(pill_left: f32, pill_w: f32) -> (f32, f32) {
    let vp_left = pill_left + VIEWPORT_INSET_DIPS;
    let vp_right = (pill_left + pill_w - VIEWPORT_INSET_DIPS).max(vp_left + 1.0);
    (vp_left, vp_right)
}

fn scroll_min_pill_width(settings: &DockSettings) -> f32 {
    let base = layout::size_profile(settings).cell;
    (base * 2.5 + 36.0).max(160.0)
}

#[derive(Clone, Copy, Debug)]
struct ResizeDragState {
    edge: ResizeEdge,
    start_screen_x: i32,
    start_screen_y: i32,
    start_width_dips: f32,
}

fn resolve_is_dark(theme: AppTheme) -> bool {
    match theme {
        AppTheme::Dark => true,
        AppTheme::Light => false,
        AppTheme::System => win32::read_system_is_dark(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellButton {
    Start,
    Search,
    TaskView,
}

impl ShellButton {
    const ALL: [Self; 3] = [Self::Start, Self::Search, Self::TaskView];

    fn label(self) -> String {
        match self {
            Self::Start => loc::t("Start"),
            Self::Search => loc::t("SearchApps"),
            Self::TaskView => loc::t("DesktopTaskView"),
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Start => 0,
            Self::Search => 1,
            Self::TaskView => 2,
        }
    }
}

#[derive(Clone)]
struct DockSlot {
    key: String,
    pinned: bool,
    label: String,
    launch_path: String,
    icon_source: String,
    windows: Vec<PreviewItem>,
    is_foreground: bool,
    scale: f32,
    scale_velocity: f32,
    slide_x: f32,
    bounce_started: Option<Instant>,
    close_started: Option<Instant>,
    /// Notification badge count painted on the icon (settings gate: `NotificationBadgesEnabled`).
    badge: u32,
}

impl DockSlot {
    fn hwnds(&self) -> Vec<usize> {
        self.windows.iter().map(|w| w.hwnd).collect()
    }

    fn is_exiting(&self) -> bool {
        !self.pinned && self.close_started.is_some()
    }

    fn layout_fraction(&self, now: Instant) -> f32 {
        if self.is_exiting() {
            1.0 - close_progress(self.close_started, now)
        } else {
            1.0
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DragState {
    idx: usize,
    start_x: f32,
    current_x: f32,
    grab_offset_x: f32,
    active: bool,
}

struct SlotVisuals {
    icon: VisualNode,
    dot: VisualNode,
    badge: Option<VisualNode>,
    badge_count: u32,
    bound_source: String,
    bound_has_icon: bool,
    bound_dot_state: u8,
    fading: bool,
}

struct TrayPinVisual {
    bg: VisualNode,
    icon: VisualNode,
    source: String,
    has_icon: bool,
    label: String,
}

struct TooltipCache {
    text: String,
    width_dips: f32,
    height_dips: f32,
}

type IconBatchQueue = Arc<Mutex<Vec<HashMap<String, Option<RawIcon>>>>>;
type DipRect = (f32, f32, f32, f32);

struct DockState {
    hwnd: usize,
    monitor: MonitorInfo,
    all_monitors: Vec<MonitorInfo>,
    dpi: f32,
    is_dark: bool,
    reload_msg: u32,
    settings: DockSettings,
    /// Cached WS-1 effective geometry/density for this monitor.
    effective: EffectiveMetrics,
    pins: Vec<PinnedApp>,
    slots: Vec<DockSlot>,
    /// Window snapshot from the last async `sync_slots` call, reused by auto-hide overlap checks.
    running_windows: Vec<RunningWindow>,
    /// Worker replies to `enumerate_windows_async`; the UI thread only drains them.
    window_snapshot_rx: Option<mpsc::Receiver<Vec<RunningWindow>>>,
    window_snapshot_in_flight: bool,
    last_window_snapshot: Option<Instant>,
    /// Mouse-driven chip rebuilds collapse into one per frame budget.
    last_widget_refresh: Option<Instant>,
    /// Secondary docks either mirror the main strip or reserve a light shelf.
    is_secondary: bool,
    light_strip: bool,
    appbar_registered: bool,
    /// Reason/tag passed to `appbar::register` so shutdown removes the same
    /// AppBar instance (light shelf vs #178 mirror dock).
    appbar_reason: &'static str,
    dcomp: DcompEngine,
    spatial_glass: Option<design::spatial_glass::SpatialGlassHost>,
    pill: PillNodes,
    shell_bgs: [VisualNode; 3],
    shell_icons: [VisualNode; 3],
    shell_bg_hover: IDCompositionSurface,
    shell_bg_press: IDCompositionSurface,
    divider: VisualNode,
    scroll_container: VisualNode,
    scroll_clip: IDCompositionRectangleClip,
    /// Rounded capsule clip matching the dock pill inner geometry for widgets and pill-bound
    /// controls so overflowing cards never poke past the pill's rounded corners.
    pill_scroll_container: VisualNode,
    pill_scroll_clip: IDCompositionRectangleClip,
    scroll_prev_node: VisualNode,
    scroll_next_node: VisualNode,
    scroll_prev_surf: IDCompositionSurface,
    scroll_prev_hover_surf: IDCompositionSurface,
    scroll_next_surf: IDCompositionSurface,
    scroll_next_hover_surf: IDCompositionSurface,
    widget_divider: VisualNode,
    tray_bg: VisualNode,
    tray_node: VisualNode,
    tray_pin_visuals: Vec<TrayPinVisual>,
    bin_bg: VisualNode,
    bin_node: VisualNode,
    bin_empty_surf: IDCompositionSurface,
    bin_full_surf: IDCompositionSurface,
    bin_surf_px: f32,
    bin_is_full: bool,
    widget_visuals: Vec<WidgetVisual>,
    /// One native folder icon per `settings.stack_folders` entry (none on a light strip).
    stack_visuals: Vec<StackVisual>,
    /// `imageres.dll,-3` folder icon used until a stack's own icon resolves.
    folder_fallback_surf: Option<IDCompositionSurface>,
    hovered_stack: Option<usize>,
    pressed_stack: Option<usize>,
    /// Pointer x while over the folder stacks (in-place magnification only).
    stack_hover_x: Option<f32>,
    widget_runtime: WidgetRuntime,
    dynamic_island: Option<dynamic_island::DynamicIslandManager>,
    show_desktop_corner: Option<ShowDesktopCorner>,
    dot_running: IDCompositionSurface,
    dot_active: IDCompositionSurface,
    icon_surfaces: HashMap<String, Option<IDCompositionSurface>>,
    fallback_surfaces: HashMap<String, IDCompositionSurface>,
    in_flight_icons: HashSet<String>,
    pending_icons: IconBatchQueue,
    slot_visuals: Vec<SlotVisuals>,
    drag_ghost: VisualNode,
    drag_ghost_source: Option<String>,
    tooltip_node: VisualNode,
    tooltip_cache: Option<TooltipCache>,
    preview: Option<Box<PreviewPopup>>,
    preview_pending_idx: Option<usize>,
    hover_mouse_x: Option<f32>,
    hovered_idx: Option<usize>,
    hovered_shell: Option<ShellButton>,
    hovered_tray: bool,
    hovered_tray_pin: Option<usize>,
    hovered_bin: bool,
    hovered_widget: Option<&'static str>,
    hovered_widget_sub: Option<u8>,
    /// Widget whose 350 ms hover-intent timer is currently armed.
    hover_intent_armed_for: Option<&'static str>,
    /// Hover intent fired; the rich flyout is opened on the next message-loop step.
    hover_flyout_pending: Option<String>,
    /// A flyout owns the UI thread; hover must not arm/open a second one.
    hover_flyout_open: bool,
    /// Widget whose pointer capture must be released on `WM_LBUTTONUP`.
    hover_capture_id: Option<String>,
    hovered_scroll: Option<bool>,
    hovered_resize: Option<ResizeEdge>,
    pressed_idx: Option<usize>,
    pressed_shell: Option<ShellButton>,
    /// Start-menu state sampled on pointer down (cached, no UI-thread EnumWindows).
    /// The open/close decision is made from this snapshot alone on pointer up.
    pressed_shell_start_was_open: bool,
    /// Last real application foreground before Start opened. A Start window that
    /// is foreground at press time is never stored as a restore target.
    start_restore_hwnd: usize,
    /// Foreground application captured before any flyout takes focus; restored
    /// when the flyout is dismissed (`DockFlyout` focus lifecycle).
    flyout_focus_return: usize,
    /// Debounce stamp for the Start toggle (open + immediate close race).
    start_toggle_last: Option<Instant>,
    pressed_tray: bool,
    pressed_tray_pin: Option<String>,
    pressed_bin: bool,
    pressed_widget: Option<&'static str>,
    pressed_scroll: Option<bool>,
    drag: Option<DragState>,
    resize_drag: Option<ResizeDragState>,
    /// Keyboard navigation cursor over dock slots.
    keyboard_idx: Option<usize>,
    /// Cached peak magnification scale for the current hover session.
    session_max_scale: f32,
    /// Resting scroll metrics frozen when a hover session starts, so the fisheye
    /// targets (rest centers) cannot drift while the strip magnifies. Cleared on
    /// mouse leave / layout changes and once the arch has fully decayed.
    hover_rest_sm: Option<ScrollMetrics>,
    /// macOS fixed-point shift `p - f(p)` applied to the live strip (expanding docks).
    fisheye_shift: f32,
    /// Last pointer used for the fixed-point anchor, kept while the arch decays.
    fisheye_anchor_x: Option<f32>,
    /// Pairs `WH_MOUSE_LL`-routed wheel notches with their `WM_MOUSEWHEEL` twins.
    wheel_dedup: layout::WheelDedup,
    /// Last foreground window outside OpenTaskbar (Keyboard Layout widget target).
    keyboard_target_hwnd: usize,
    /// Fisheye scale/velocity of the three shell glyphs (Start/Search/TaskView).
    /// They share the app-icon spring and gate but have their own spring state.
    shell_scales: [f32; 3],
    shell_velocities: [f32; 3],
    /// 450 ms drag-hover bring-to-front gesture.
    drag_hover_started: Option<Instant>,
    /// Smart auto-hide rescans only on a genuine shell/foreground change, never
    /// from a mouse event or the 200 ms revealed-guard tick. Even then the 1 s
    /// `last_overlap_scan` throttle applies.
    overlap_dirty: bool,
    last_overlap_scan: Option<Instant>,
    cached_overlap: bool,
    /// Cached result of the throttled fullscreen scan used to hide a secondary
    /// dock while a game owns **its own** monitor (#179). A fullscreen window on
    /// a different display must never hide this dock.
    last_fullscreen_scan: Option<Instant>,
    fullscreen_on_own_monitor: bool,
    /// Cached `flyout_reposition::is_anchored()` answer (>=2 s TTL). The 200 ms
    /// guard tick reads the cache instead of validating the Start process.
    cached_anchored: Option<(Instant, bool)>,
    last_anchor_pending: bool,
    last_start_menu_open: bool,
    /// Deduplicates 1000 Hz `WM_MOUSEMOVE` floods at identical coordinates.
    last_mouse_move: Option<(f32, f32)>,
    /// `WM_APP_SETTINGS_RELOAD` is posted once; the idle message performs the save + reload.
    settings_reload_pending: bool,
    /// Rate-limits animation wake-ups to one per VSync frame (>=8 ms).
    last_animation_request: Option<Instant>,
    /// Dynamic Island page/expanded state (headless policy, shared with the dock host).
    island: IslandState,
    scroll_offset: f32,
    scroll_target: f32,
    scroll_pan: Option<(f32, f32, bool)>,
    tracking_mouse: bool,
    /// Auto-hide smooth animation (`0.0` = fully shown, `1.0` = slid off bottom edge).
    hide_progress: f32,
    target_hidden: bool,
    /// Single Win-key grace deadline shared by the low-level hook, keyboard
    /// navigation and direct Win32 key paths (#164). Always armed through
    /// `arm_win_key_reveal` so the setting gate cannot be bypassed.
    win_key_reveal_until: Option<Instant>,
    last_frame: Option<Instant>,
    last_window_rect: Option<(i32, i32, i32, i32)>,
    anim_signal: Arc<(Mutex<bool>, Condvar)>,
    tick_pending: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    target_fps: Arc<AtomicU32>,
}

fn exe_display_name(exe_path: &str, fallback_title: &str) -> String {
    Path::new(exe_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => fallback_title.to_owned(),
            }
        })
        .unwrap_or_else(|| fallback_title.to_owned())
}

/// Tooltip suffix for a folder-stack pin: number of visible files that fit in
/// the reference 4x2 preview grid (`.crdownload` / `.part` files are hidden).
fn folder_stack_preview_count(path: &str) -> Option<usize> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return None;
    }
    let visible = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(layout::is_stack_visible_file)
        })
        .count();
    Some(folder_stack_preview_cells(visible).len())
}

fn load_effective_pins(settings: &DockSettings) -> Vec<PinnedApp> {
    let mut items = pins::load_pins();
    for extra_path in &settings.user_pinned_apps {
        let exists = items
            .iter()
            .any(|p| p.launch_path.eq_ignore_ascii_case(extra_path));
        if !exists && let Some(pin) = pins::pin_from_path(extra_path) {
            items.push(pin);
        }
    }
    if !settings.user_unpinned_apps.is_empty() {
        items.retain(|p| {
            !settings
                .user_unpinned_apps
                .iter()
                .any(|u| u.eq_ignore_ascii_case(&p.launch_path))
        });
    }
    if let Some(ref order) = settings.pin_order {
        items.sort_by_key(|p| {
            order
                .iter()
                .position(|saved| saved.eq_ignore_ascii_case(&p.launch_path))
                .unwrap_or(usize::MAX)
        });
    }
    // WS-8 live placement: resolve the saved item order through the same
    // pinned-group policy the settings UI previews. `End` / `InWidgets` move the
    // Start token into the trailing group instead of leaving the setting inert.
    let mut tokens: Vec<String> = settings.dock_item_order.clone();
    tokens.extend(items.iter().map(|pin| pin.launch_path.clone()));
    let grouped = layout::apply_pinned_groups(&tokens, settings);
    items.sort_by_key(|pin| {
        grouped
            .iter()
            .position(|token| token.eq_ignore_ascii_case(&pin.launch_path))
            .unwrap_or(usize::MAX)
    });
    items
}

fn update_background_pin(settings: &mut DockSettings, path: &str, pinned: bool) {
    let matches = |entry: &String| entry.eq_ignore_ascii_case(path);
    if pinned {
        if !settings.user_pinned_background_apps.iter().any(matches) {
            settings.user_pinned_background_apps.push(path.to_owned());
        }
        // Older Rust builds stored background pins as ordinary executable pins.
        settings.user_pinned_apps.retain(|entry| !matches(entry));
        if let Some(order) = &mut settings.pin_order {
            order.retain(|entry| !matches(entry));
        }
    } else {
        settings
            .user_pinned_background_apps
            .retain(|entry| !matches(entry));
    }
}

/// Groups running windows the way `RunningAppProjection` does: by AUMID when
/// available, falling back to the executable for shared hosts / missing AUMIDs.
/// `AggregateByExecutablePath` forces executable grouping for every window.
pub fn running_group_key(win: &RunningWindow, aggregate_by_exe: bool) -> String {
    let exe = win.exe_path.to_lowercase();
    let shared_host = [
        "explorer.exe",
        "applicationframehost.exe",
        "systemsettings.exe",
    ]
    .iter()
    .any(|host| exe.ends_with(host));
    if aggregate_by_exe || shared_host || win.aumid.trim().is_empty() {
        format!("exe:{exe}")
    } else {
        format!("aumid:{}", win.aumid.to_lowercase())
    }
}

fn build_slots_for(
    pins: &[PinnedApp],
    running: &[RunningWindow],
    prev: &[DockSlot],
    settings: &DockSettings,
) -> Vec<DockSlot> {
    let aggregate = layout::aggregate_by_executable_path(settings);
    let mut by_exe: HashMap<String, Vec<&RunningWindow>> = HashMap::new();
    for win in running {
        by_exe
            .entry(running_group_key(win, aggregate))
            .or_default()
            .push(win);
    }

    let mut pin_matches: HashMap<String, (Vec<PreviewItem>, bool)> = HashMap::new();
    let mut claimed_exes: HashMap<String, bool> = HashMap::new();
    for pin in pins {
        let exe_key = pin
            .target_exe
            .as_ref()
            .map(|p| format!("exe:{}", p.to_lowercase()));
        let aumid_key = pins::resolve_windowsapps_aumid(&pin.launch_path)
            .map(|id| format!("aumid:{}", id.to_lowercase()));
        let matched_key = exe_key
            .filter(|key| by_exe.contains_key(key))
            .or_else(|| aumid_key.filter(|key| by_exe.contains_key(key)))
            .or_else(|| {
                let stem = pin.name.to_lowercase();
                by_exe
                    .keys()
                    .filter_map(|key| key.strip_prefix("exe:"))
                    .find(|path| {
                        Path::new(path)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|s| s.eq_ignore_ascii_case(&stem))
                    })
                    .map(|path| format!("exe:{path}"))
            });
        let (windows, is_fg) = match matched_key {
            Some(ref key) => {
                claimed_exes.insert(key.clone(), true);
                let list = &by_exe[key];
                (
                    list.iter()
                        .map(|w| PreviewItem {
                            hwnd: w.hwnd,
                            pid: w.pid,
                            title: w.title.clone(),
                        })
                        .collect(),
                    list.iter().any(|w| w.is_foreground),
                )
            }
            None => (Vec::new(), false),
        };
        pin_matches.insert(pin.launch_path.to_lowercase(), (windows, is_fg));
    }

    let mut next: Vec<DockSlot> = Vec::with_capacity(pins.len() + by_exe.len());
    let mut emitted_keys: HashMap<String, bool> = HashMap::new();

    for old in prev {
        if old.pinned {
            if let Some(pin) = pins
                .iter()
                .find(|p| p.launch_path.eq_ignore_ascii_case(&old.key))
            {
                let key = pin.launch_path.to_lowercase();
                let (windows, is_foreground) = pin_matches.get(&key).cloned().unwrap_or_default();
                emitted_keys.insert(key.clone(), true);
                next.push(DockSlot {
                    key,
                    pinned: true,
                    label: pin.name.clone(),
                    launch_path: pin.launch_path.clone(),
                    icon_source: pin.icon_source.clone(),
                    windows,
                    is_foreground,
                    scale: old.scale,
                    scale_velocity: old.scale_velocity,
                    slide_x: old.slide_x,
                    bounce_started: old.bounce_started,
                    close_started: None,
                    badge: old.badge,
                });
            }
        } else if !claimed_exes.contains_key(&old.key)
            && let Some(list) = by_exe.get(&old.key)
        {
            emitted_keys.insert(old.key.clone(), true);
            next.push(DockSlot {
                key: old.key.clone(),
                pinned: false,
                label: old.label.clone(),
                launch_path: old.launch_path.clone(),
                icon_source: old.icon_source.clone(),
                windows: list
                    .iter()
                    .map(|w| PreviewItem {
                        hwnd: w.hwnd,
                        pid: w.pid,
                        title: w.title.clone(),
                    })
                    .collect(),
                is_foreground: list.iter().any(|w| w.is_foreground),
                scale: old.scale,
                scale_velocity: old.scale_velocity,
                slide_x: old.slide_x,
                bounce_started: old.bounce_started,
                close_started: None,
                badge: old.badge,
            });
        }
    }

    for pin in pins {
        let key = pin.launch_path.to_lowercase();
        if emitted_keys.contains_key(&key) {
            continue;
        }
        let (windows, is_foreground) = pin_matches.get(&key).cloned().unwrap_or_default();
        emitted_keys.insert(key.clone(), true);
        next.push(DockSlot {
            key,
            pinned: true,
            label: pin.name.clone(),
            launch_path: pin.launch_path.clone(),
            icon_source: pin.icon_source.clone(),
            windows,
            is_foreground,
            scale: 1.0,
            scale_velocity: 0.0,
            slide_x: 0.0,
            bounce_started: None,
            close_started: None,
            badge: 0,
        });
    }

    for win in running {
        let key = running_group_key(win, aggregate);
        if claimed_exes.contains_key(&key) || emitted_keys.contains_key(&key) {
            continue;
        }
        emitted_keys.insert(key.clone(), true);
        let list = &by_exe[&key];
        let label = exe_display_name(&win.exe_path, &win.title);
        let icon_source = pins::resolve_windowsapps_aumid(&win.exe_path)
            .map(|id| format!(r"shell:AppsFolder\{id}"))
            .unwrap_or_else(|| win.exe_path.clone());
        next.push(DockSlot {
            key,
            pinned: false,
            label,
            launch_path: win.exe_path.clone(),
            icon_source,
            windows: list
                .iter()
                .map(|w| PreviewItem {
                    hwnd: w.hwnd,
                    pid: w.pid,
                    title: w.title.clone(),
                })
                .collect(),
            is_foreground: list.iter().any(|w| w.is_foreground),
            scale: 1.0,
            scale_velocity: 0.0,
            slide_x: 0.0,
            bounce_started: None,
            close_started: None,
            badge: 0,
        });
    }

    next
}

/// Test/back-compatibility wrapper using default settings (executable grouping).
fn build_slots(pins: &[PinnedApp], running: &[RunningWindow], prev: &[DockSlot]) -> Vec<DockSlot> {
    build_slots_for(pins, running, prev, &DockSettings::default())
}

fn close_progress(started: Option<Instant>, now: Instant) -> f32 {
    started.map_or(0.0, |started| {
        let time = (now.saturating_duration_since(started).as_secs_f32()
            / CLOSE_DURATION.as_secs_f32())
        .min(1.0);
        // Smooth cubic ease-out so app-close shrink/fade decelerates naturally.
        1.0 - (1.0 - time).powi(3)
    })
}

fn last_window_closed(slot: &DockSlot, is_visible: impl Fn(usize) -> bool) -> bool {
    !slot.windows.is_empty() && slot.windows.iter().all(|window| !is_visible(window.hwnd))
}

fn tracked_window_is_visible(hwnd: usize) -> bool {
    // SAFETY: Windows checks the handle. Minimizing leaves WS_VISIBLE set.
    unsafe { IsWindowVisible(hwnd as HWND) != 0 }
}

fn keep_closing_slots(
    previous: &[DockSlot],
    next: &mut Vec<DockSlot>,
    now: Instant,
    is_visible: impl Fn(usize) -> bool,
) {
    for (index, old) in previous.iter().enumerate() {
        let started = match old.close_started {
            Some(started) if now.saturating_duration_since(started) < CLOSE_DURATION => started,
            Some(_) => continue,
            None if last_window_closed(old, &is_visible) => now,
            None => continue,
        };
        if let Some(slot) = next.iter_mut().find(|slot| slot.key == old.key) {
            if slot.windows.is_empty() {
                slot.close_started = Some(started);
                slot.bounce_started = None;
            }
        } else if !old.pinned {
            let mut exiting = old.clone();
            exiting.windows.clear();
            exiting.is_foreground = false;
            exiting.bounce_started = None;
            exiting.close_started = Some(started);
            next.insert(index.min(next.len()), exiting);
        }
    }
}

/// Visible pill thickness. `DockMetrics.For(...).Shelf` = 48/58/66 DIP.
fn pill_height_dips(settings: &DockSettings) -> f32 {
    layout::size_profile(settings).shelf
}

fn content_min_pill_width(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> f32 {
    let profile = layout::size_profile(settings);
    let icons_w = section_width(profile.cell, slot_gap(settings), slot_count);
    pill_width_for_content(content_strip_width(
        shell_section_width(settings),
        icons_w,
        widgets_section_width(settings, slot_count, mon_w_dips),
        0.0,
    ))
}

fn resting_pill_width(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> f32 {
    let max_w = (mon_w_dips - 16.0).max(200.0);
    if settings.full_width {
        return max_w;
    }
    let natural_w = content_min_pill_width(settings, slot_count, mon_w_dips);
    let floor_w = scroll_min_pill_width(settings).min(natural_w);
    let target = settings
        .dock_custom_width_dips
        .map_or(natural_w, |custom| custom.max(floor_w));
    target.min(max_w)
}

fn compute_window_dips_with(
    settings: &DockSettings,
    slot_count: usize,
    mon_w_dips: f32,
    effective: &EffectiveMetrics,
) -> (f32, f32) {
    let profile = layout::size_profile(settings);
    let base = profile.cell;
    let mut resting_w = resting_pill_width(settings, slot_count, mon_w_dips);
    // WS-1 `effective_width` overrides Auto/Pixels/Percent width modes.
    if let Some(width) = effective.width_dips {
        resting_w = width.min((mon_w_dips - 16.0).max(200.0));
    } else if !settings.full_width
        && settings.dock_custom_width_dips.is_none()
        && let Some(live) = effective.live_rest_content_dips
    {
        // Live widget widths can exceed the static descriptors: never size the
        // window below the real resting content.
        resting_w = resting_w.max(live).min((mon_w_dips - 16.0).max(200.0));
    }
    let peak = profile_peak(settings);
    let expands = magnification_expands_dock(settings);
    // Fixed-width magnification happens in place (zero slot extras), so it
    // needs no horizontal room. Only an expanding dock reserves a full arch so
    // a hover can never pop scroll chevrons or clip the Tray/Recycle Bin.
    let mag_extra =
        if fisheye_enabled(settings) && !settings.full_width && expands && slot_count > 0 {
            layout::fisheye_expansion_reserve_dips(base + slot_gap(settings), peak) + 8.0
        } else {
            0.0
        };
    let mag_headroom = if expands {
        // `MagnificationExpandsDock` grows the surface itself: no headroom strip.
        0.0
    } else if settings.magnification_enabled {
        (profile.icon * peak + 14.0).max(MAG_HEADROOM)
    } else {
        MAG_HEADROOM
    };
    let width_dips = (resting_w + mag_extra + 24.0)
        .clamp(160.0, mon_w_dips)
        .ceil();
    let mut pill_h = pill_height_dips(settings);
    if let Some(h) = effective.height_dips {
        pill_h = h;
    }
    let bottom_inset = effective.bottom_inset_dips();
    let height_dips = (pill_h + TOOLTIP_HEADROOM + mag_headroom + bottom_inset).ceil();
    (width_dips, height_dips)
}

fn compute_window_dips(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> (f32, f32) {
    compute_window_dips_with(
        settings,
        slot_count,
        mon_w_dips,
        &EffectiveMetrics::default(),
    )
}

/// Fisheye extra-scale ceiling above 1.0: never above `DockMetrics.HoverScale - 1.0 = 0.5`.
fn profile_peak(settings: &DockSettings) -> f32 {
    settings
        .fisheye_intensity
        .peak()
        .clamp(0.0, MAX_FISHEYE_SCALE - 1.0)
}

/// Absolute fisheye peak scale factor (`1.0..=MAX_FISHEYE_SCALE`, e.g. 1.20 / 1.42 / 1.50).
fn profile_max_scale(settings: &DockSettings) -> f32 {
    (1.0 + profile_peak(settings)).clamp(1.0, MAX_FISHEYE_SCALE)
}

/// Computes exact signed physical coordinates for a monitor (supports negative X on left monitors).
fn compute_window_rect(
    settings: &DockSettings,
    slot_count: usize,
    monitor: MonitorInfo,
) -> (i32, i32, i32, i32) {
    compute_window_rect_with(settings, slot_count, monitor, &EffectiveMetrics::default())
}

/// Same as `compute_window_rect` with the runtime-resolved WS-1 width/height/gaps.
fn compute_window_rect_with(
    settings: &DockSettings,
    slot_count: usize,
    monitor: MonitorInfo,
    effective: &EffectiveMetrics,
) -> (i32, i32, i32, i32) {
    let dpi = monitor.dpi_scale;
    let mon_w_dips = monitor.width() as f32 / dpi;
    let (width_dips, height_dips) =
        compute_window_dips_with(settings, slot_count, mon_w_dips, effective);
    // `DockMetrics.SideInset` for side docks; bottom gap is baked into the window's
    // internal `bottom_inset_dips()` so the HWND stays flush with `monitor.bottom` (never
    // doubling `VERTICAL_EDGE_GAP` and never clipping the bottom Clear Crystal shadow).
    let margin_px = (12.0 * dpi).round() as i32;
    let edge = effective_edge(settings);
    if layout::is_vertical(edge) {
        // Side docks: the strip length runs along Y, thickness along X. Height is content
        // driven (`ResizeSideDockToContent`); width keeps the magnification headroom.
        let side_h = settings.dock_custom_width_dips.unwrap_or_else(|| {
            layout::side_dock_height_dips(
                slot_count + 3,
                layout::size_profile(settings).cell + slot_gap(settings),
                22.0,
                settings.show_desktop_button,
                monitor.height() as f32 / dpi - 24.0,
            )
        });
        let cross = if magnification_expands_dock(settings) {
            layout::size_profile(settings).shelf
        } else {
            layout::size_profile(settings).shelf + layout::HEADROOM_DIPS
        };
        return layout::window_rect_for_edge(
            edge,
            monitor,
            settings.position,
            side_h,
            cross,
            margin_px,
            settings.full_width,
        );
    }
    let monitor_top = monitor.top;
    let (x, y, w, h) = layout::window_rect_for_edge(
        edge,
        monitor,
        settings.position,
        width_dips,
        height_dips,
        margin_px,
        false,
    );
    let y = if effective.top_gap_dips.unwrap_or(0) > 0 && y == monitor_top {
        y + ((effective.top_gap_dips.unwrap_or(0).max(0) as f32) * dpi).round() as i32
    } else {
        y
    };
    (x, y, w, h)
}

fn bounce_offset_dips(started: Option<Instant>) -> Option<f32> {
    let t = started?.elapsed().as_secs_f32() / BOUNCE_DURATION_SECS;
    if t >= 1.0 {
        return None;
    }
    let y = if t < 0.62 {
        let u = t / 0.62;
        (u * std::f32::consts::PI).sin() * 11.0
    } else {
        let u = (t - 0.62) / 0.38;
        (u * std::f32::consts::PI).sin() * 4.5
    };
    Some(y)
}

/// Persisted `settings.extra` toggle (`1` = on, `0` = off). The default keeps
/// the WS-10 picker available.
fn extra_flag(settings: &DockSettings, key: &str, default: bool) -> bool {
    settings
        .extra
        .get(key)
        .and_then(|value| value.as_i64())
        .map(|value| value != 0)
        .unwrap_or(default)
}

/// Native commands use small ids (`1`, `2`, ...) that collide with dock rows;
/// remap them before adding them to our context menu and keep the original id
/// for `menu::invoke_native_app_command`.
fn remap_native_app_menu_entries(entries: Vec<MenuEntry>) -> (Vec<(u32, u32)>, Vec<MenuEntry>) {
    let mut commands = Vec::new();
    let mut items = Vec::new();
    for (offset, entry) in entries.into_iter().enumerate() {
        let remapped = NATIVE_APP_MENU_ID_BASE + offset as u32;
        match entry {
            MenuEntry::Item {
                id,
                icon,
                label,
                shortcut,
                destructive,
            } => {
                commands.push((remapped, id));
                items.push(MenuEntry::Item {
                    id: remapped,
                    icon,
                    label,
                    shortcut,
                    destructive,
                });
            }
            other => items.push(other),
        }
    }
    (commands, items)
}

impl DockState {
    #[allow(clippy::too_many_arguments)]
    fn new(
        hwnd: usize,
        monitor: MonitorInfo,
        reload_msg: u32,
        settings: DockSettings,
        pins: Vec<PinnedApp>,
        initial_slots: Vec<DockSlot>,
        initial_running: Vec<RunningWindow>,
        initial_rect: (i32, i32, i32, i32),
        is_secondary: bool,
        light_strip: bool,
    ) -> windows::core::Result<Self> {
        let dpi = monitor.dpi_scale;
        let is_dark = resolve_is_dark(settings.theme);
        set_win_key_reveal_gate(&settings);
        let dcomp = DcompEngine::new(hwnd)?;
        let profile = layout::size_profile(&settings);
        let base = profile.cell;
        let pill_h = pill_height_dips(&settings);
        // `IconPack.CornerRadius` = 16/19/22 per profile; an explicit user
        // override in settings still wins.
        let radius = settings
            .dock_corner_radius
            .map(|r| r.clamp(0, 32) as f32)
            .unwrap_or(profile.radius);

        let pal = design::palette(is_dark);
        let (pill_rgb, bd_rgb, hover_rgb, press_rgb, div_rgb, dot_run_rgb, dot_act_rgb) = if is_dark
        {
            (
                pal.dock_pill,
                (0xff, 0xff, 0xff, 0.11),
                (0xff, 0xff, 0xff, 0.10),
                (0xff, 0xff, 0xff, 0.16),
                (0xff, 0xff, 0xff, 0.14),
                (0xd4, 0xd4, 0xd8, 0.94),
                (0xff, 0xff, 0xff, 1.0),
            )
        } else {
            (
                pal.dock_pill,
                (0x18, 0x18, 0x1b, 0.11),
                (0x18, 0x18, 0x1b, 0.07),
                (0x18, 0x18, 0x1b, 0.13),
                (0x18, 0x18, 0x1b, 0.14),
                (0x52, 0x52, 0x5b, 0.85),
                (0x18, 0x18, 0x1b, 0.95),
            )
        };

        let glass_cfg = design::spatial_glass::SpatialGlassConfig::from_settings(
            &settings,
            settings.dock_background,
            settings.dock_opacity,
            is_dark,
            false,
        );
        let spatial_glass = design::spatial_glass::SpatialGlassHost::attach(hwnd, false, glass_cfg);
        let glass_host_active = spatial_glass
            .as_ref()
            .is_some_and(design::spatial_glass::SpatialGlassHost::is_enabled);
        let has_custom_border = settings.dock_border_color.is_some()
            || (settings.dock_border_thickness - 1.0).abs() > 0.05;

        let true_black = settings.dock_background == SurfaceBackground::TrueBlack;
        let bg_alpha = match settings.dock_background {
            SurfaceBackground::None => 0.0_f32,
            SurfaceBackground::Solid => 0.98_f32,
            SurfaceBackground::Glass if glass_host_active => 0.0_f32,
            SurfaceBackground::Glass => (settings.dock_opacity.clamp(0.1, 1.0) * 0.88) as f32,
            // OLED care: a fully opaque black capsule with no tint, rim or shadow.
            SurfaceBackground::TrueBlack => 1.0_f32,
        };
        let bd_alpha = if settings.dock_background == SurfaceBackground::None
            || true_black
            || (glass_host_active && !has_custom_border)
        {
            0.0
        } else {
            bd_rgb.3
        };
        let user_bd = settings
            .dock_border_color
            .as_deref()
            .and_then(design::parse_hex_rgb)
            .unwrap_or((bd_rgb.0, bd_rgb.1, bd_rgb.2));
        let pill_bg = if true_black {
            (0_u8, 0_u8, 0_u8)
        } else {
            (pill_rgb.0, pill_rgb.1, pill_rgb.2)
        };
        let pill = dcomp.create_pill_nodes(
            radius * dpi,
            pill_h * dpi,
            (pill_bg.0, pill_bg.1, pill_bg.2, bg_alpha),
            (user_bd.0, user_bd.1, user_bd.2, bd_alpha),
            dpi,
            settings.dock_border_thickness.clamp(0.0, 8.0) as f32,
            true_black,
        )?;

        let shell_px = (base * dpi).ceil() as u32;
        let shell_bg_hover =
            dcomp.create_rounded_rect_surface(shell_px, shell_px, 9.0 * dpi, hover_rgb)?;
        let shell_bg_press =
            dcomp.create_rounded_rect_surface(shell_px, shell_px, 9.0 * dpi, press_rgb)?;
        let shell_bgs = [
            dcomp.create_node()?,
            dcomp.create_node()?,
            dcomp.create_node()?,
        ];
        let glyph_px = ((base * 0.68).max(22.0) * dpi).ceil() as u32;
        let shell_icons = [
            {
                let n = dcomp.create_node()?;
                let s = dcomp.create_shell_glyph_surface(ShellGlyph::Start, glyph_px, is_dark)?;
                n.set_content(Some(&s));
                n
            },
            {
                let n = dcomp.create_node()?;
                let s = dcomp.create_shell_glyph_surface(ShellGlyph::Search, glyph_px, is_dark)?;
                n.set_content(Some(&s));
                n
            },
            {
                let n = dcomp.create_node()?;
                let s =
                    dcomp.create_shell_glyph_surface(ShellGlyph::TaskView, glyph_px, is_dark)?;
                n.set_content(Some(&s));
                n
            },
        ];

        let div_surf = dcomp.create_rounded_rect_surface(
            (1.5 * dpi).ceil() as u32,
            (26.0 * dpi).ceil() as u32,
            0.75 * dpi,
            div_rgb,
        )?;
        let divider = dcomp.create_node()?;
        divider.set_content(Some(&div_surf));
        let scroll_container = dcomp.create_node()?;
        let scroll_clip = dcomp.create_rect_clip()?;
        scroll_container.set_clip(&scroll_clip);
        let pill_scroll_container = dcomp.create_node()?;
        let pill_scroll_clip = dcomp.create_rect_clip()?;
        pill_scroll_container.set_clip(&pill_scroll_clip);
        let se_w_px = (SCROLL_EDGE_W_DIPS * dpi).ceil() as u32;
        let se_h_px = (pill_h * dpi).ceil() as u32;
        let scroll_prev_surf = dcomp.create_scroll_edge_surface(
            true,
            se_w_px,
            se_h_px,
            radius * dpi,
            pill_rgb,
            is_dark,
            false,
            dpi,
        )?;
        let scroll_prev_hover_surf = dcomp.create_scroll_edge_surface(
            true,
            se_w_px,
            se_h_px,
            radius * dpi,
            pill_rgb,
            is_dark,
            true,
            dpi,
        )?;
        let scroll_next_surf = dcomp.create_scroll_edge_surface(
            false,
            se_w_px,
            se_h_px,
            radius * dpi,
            pill_rgb,
            is_dark,
            false,
            dpi,
        )?;
        let scroll_next_hover_surf = dcomp.create_scroll_edge_surface(
            false,
            se_w_px,
            se_h_px,
            radius * dpi,
            pill_rgb,
            is_dark,
            true,
            dpi,
        )?;
        let scroll_prev_node = dcomp.create_node()?;
        scroll_prev_node.set_content(Some(&scroll_prev_surf));
        scroll_prev_node.hide();
        let scroll_next_node = dcomp.create_node()?;
        scroll_next_node.set_content(Some(&scroll_next_surf));
        scroll_next_node.hide();
        let widget_divider = dcomp.create_node()?;
        widget_divider.set_content(Some(&div_surf));

        let tray_bg = dcomp.create_node()?;
        tray_bg.hide();
        let tray_node = dcomp.create_node()?;
        let tray_surf =
            dcomp.create_shell_glyph_surface(ShellGlyph::TrayChevron, glyph_px, is_dark)?;
        tray_node.set_content(Some(&tray_surf));

        let bin_bg = dcomp.create_node()?;
        bin_bg.hide();
        let bin_node = dcomp.create_node()?;
        let (bin_empty_surf, bin_full_surf, bin_surf_px) = match (
            icon_cache::extract_recycle_bin_icon(false)
                .and_then(|raw| dcomp.create_app_icon_surface(&raw).ok()),
            icon_cache::extract_recycle_bin_icon(true)
                .and_then(|raw| dcomp.create_app_icon_surface(&raw).ok()),
        ) {
            // The Recycle Bin is extracted at 128 px for crispness: scale by its
            // real surface size, not the 64 px app-icon size.
            (Some(empty), Some(full)) => (empty, full, icon_cache::RECYCLE_BIN_ICON_PX as f32),
            _ => (
                dcomp.create_shell_glyph_surface(ShellGlyph::RecycleBinEmpty, glyph_px, is_dark)?,
                dcomp.create_shell_glyph_surface(ShellGlyph::RecycleBinFull, glyph_px, is_dark)?,
                glyph_px as f32,
            ),
        };
        let bin_is_full = widgets::is_recycle_bin_non_empty();
        bin_node.set_content(Some(if bin_is_full {
            &bin_full_surf
        } else {
            &bin_empty_surf
        }));
        let folder_fallback_surf = icon_cache::extract_default_folder_icon()
            .and_then(|raw| dcomp.create_app_icon_surface(&raw).ok());

        let corner_h = pill_h + settings.dock_bottom_gap_pixels.max(0) as f32;
        let all_monitors = win32::enumerate_monitors();
        let dynamic_island = if monitor.is_primary {
            let mut mgr = dynamic_island::DynamicIslandManager::new();
            mgr.apply(&settings, &all_monitors, is_dark);
            Some(mgr)
        } else {
            None
        };
        let show_desktop_corner = ShowDesktopCorner::new_for_edge(
            effective_edge(&settings),
            monitor.clone(),
            corner_h,
            is_dark,
            settings.show_desktop_button,
        );

        let dot_h_px = (3.5 * dpi).ceil() as u32;
        let dot_running = dcomp.create_rounded_rect_surface(
            (6.0 * dpi).ceil() as u32,
            dot_h_px,
            2.0 * dpi,
            dot_run_rgb,
        )?;
        let dot_active = dcomp.create_rounded_rect_surface(
            (16.0 * dpi).ceil() as u32,
            dot_h_px,
            2.0 * dpi,
            dot_act_rgb,
        )?;

        let drag_ghost = dcomp.create_node()?;
        drag_ghost.hide();
        let tooltip_node = dcomp.create_node()?;
        tooltip_node.hide();

        let preview = PreviewPopup::new(move || unsafe {
            let _ = PostMessageW(hwnd as HWND, WM_APP_PREVIEW_LEAVE, 0, 0);
        });

        let anim_signal = Arc::new((Mutex::new(false), Condvar::new()));
        let tick_pending = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        let initial_fps = Self::compute_target_fps(&settings, &monitor.device_name);
        crate::ui::flyout::set_motion_prefs(
            effective_animations_enabled(settings.animations_enabled, settings.focus_mode),
            initial_fps,
        );
        let target_fps = Arc::new(AtomicU32::new(initial_fps));
        let effective = EffectiveMetrics::for_monitor(&settings, monitor.width() as f32 / dpi);
        let mut state = Self {
            hwnd,
            monitor,
            all_monitors,
            dpi,
            is_dark,
            reload_msg,
            settings,
            effective,
            pins,
            slots: initial_slots,
            running_windows: initial_running,
            window_snapshot_rx: None,
            window_snapshot_in_flight: false,
            last_window_snapshot: None,
            last_widget_refresh: None,
            is_secondary,
            light_strip,
            appbar_registered: false,
            appbar_reason: "secondary-light-strip",
            dcomp,
            spatial_glass,
            pill,
            shell_bgs,
            shell_icons,
            shell_bg_hover,
            shell_bg_press,
            divider,
            scroll_container,
            scroll_clip,
            pill_scroll_container,
            pill_scroll_clip,
            scroll_prev_node,
            scroll_next_node,
            scroll_prev_surf,
            scroll_prev_hover_surf,
            scroll_next_surf,
            scroll_next_hover_surf,
            widget_divider,
            tray_bg,
            tray_node,
            tray_pin_visuals: Vec::new(),
            bin_bg,
            bin_node,
            bin_empty_surf,
            bin_full_surf,
            bin_surf_px,
            bin_is_full,
            widget_visuals: Vec::new(),
            stack_visuals: Vec::new(),
            folder_fallback_surf,
            hovered_stack: None,
            pressed_stack: None,
            stack_hover_x: None,
            widget_runtime: WidgetRuntime::default(),
            dynamic_island,
            show_desktop_corner,
            dot_running,
            dot_active,
            icon_surfaces: HashMap::new(),
            fallback_surfaces: HashMap::new(),
            in_flight_icons: HashSet::new(),
            pending_icons: Arc::new(Mutex::new(Vec::new())),
            slot_visuals: Vec::new(),
            drag_ghost,
            drag_ghost_source: None,
            tooltip_node,
            tooltip_cache: None,
            preview,
            preview_pending_idx: None,
            hover_mouse_x: None,
            hovered_idx: None,
            hovered_shell: None,
            hovered_tray: false,
            hovered_tray_pin: None,
            hovered_bin: false,
            hovered_widget: None,
            hovered_widget_sub: None,
            hover_intent_armed_for: None,
            hover_flyout_pending: None,
            hover_flyout_open: false,
            hover_capture_id: None,
            hovered_scroll: None,
            hovered_resize: None,
            pressed_idx: None,
            pressed_shell: None,
            pressed_shell_start_was_open: false,
            start_restore_hwnd: 0,
            flyout_focus_return: 0,
            start_toggle_last: None,
            pressed_tray: false,
            pressed_tray_pin: None,
            pressed_bin: false,
            pressed_widget: None,
            pressed_scroll: None,
            drag: None,
            resize_drag: None,
            keyboard_idx: None,
            session_max_scale: layout::HOVER_SCALE,
            hover_rest_sm: None,
            fisheye_shift: 0.0,
            fisheye_anchor_x: None,
            wheel_dedup: layout::WheelDedup::default(),
            keyboard_target_hwnd: 0,
            shell_scales: [1.0; 3],
            shell_velocities: [0.0; 3],
            drag_hover_started: None,
            overlap_dirty: true,
            last_overlap_scan: None,
            cached_overlap: false,
            last_fullscreen_scan: None,
            fullscreen_on_own_monitor: false,
            cached_anchored: None,
            last_anchor_pending: false,
            last_start_menu_open: false,
            last_mouse_move: None,
            settings_reload_pending: false,
            last_animation_request: None,
            island: IslandState::new(Vec::new()),
            scroll_offset: 0.0,
            scroll_target: 0.0,
            scroll_pan: None,
            tracking_mouse: false,
            hide_progress: 0.0,
            target_hidden: false,
            win_key_reveal_until: None,
            last_frame: None,
            last_window_rect: Some(initial_rect),
            anim_signal,
            tick_pending,
            shutdown,
            target_fps,
        };
        state.rebuild_visual_tree();
        state.refresh_widgets();
        state.request_missing_icons();
        state.update_visuals();
        state.spawn_vsync_thread();
        Ok(state)
    }

    fn compute_target_fps(settings: &DockSettings, device_name: &str) -> u32 {
        // 0 parks the VSync producer at its condvar (DwmFlush when awake).
        if !effective_animations_enabled(settings.animations_enabled, settings.focus_mode) {
            return 0;
        }
        let hz = win32::monitor_refresh_hz(device_name);
        match settings.animation_fps {
            Some(fps) if fps < hz => fps.clamp(15, hz),
            _ => 0,
        }
    }

    fn widget_anim_interval_ms(&self) -> u32 {
        let hz = win32::monitor_refresh_hz(&self.monitor.device_name);
        let fps = self
            .settings
            .animation_fps
            .map(|f| f.clamp(15, hz))
            .unwrap_or(hz);
        (1000 / fps.max(15)).max(4)
    }

    fn spawn_vsync_thread(&self) {
        let hwnd_val = self.hwnd;
        let signal = self.anim_signal.clone();
        let tick_pending = self.tick_pending.clone();
        let shutdown = self.shutdown.clone();
        let target_fps = self.target_fps.clone();
        std::thread::Builder::new()
            .name("dock-vsync".to_owned())
            .spawn(move || {
                let (lock, cvar) = &*signal;
                loop {
                    {
                        let mut active = lock.lock().unwrap_or_else(|e| e.into_inner());
                        while !*active && !shutdown.load(Ordering::Relaxed) {
                            active = cvar.wait(active).unwrap_or_else(|e| e.into_inner());
                        }
                    }
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    let fps = target_fps.load(Ordering::Relaxed);
                    if fps == 0 {
                        unsafe {
                            let _ = DwmFlush();
                        }
                    } else {
                        std::thread::sleep(Duration::from_nanos(
                            1_000_000_000u64 / u64::from(fps.max(15)),
                        ));
                    }
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    if !tick_pending.swap(true, Ordering::AcqRel) {
                        unsafe {
                            let _ = PostMessageW(hwnd_val as HWND, WM_APP_VSYNC, 0, 0);
                        }
                    }
                }
            })
            .ok();
    }

    fn is_animating(&self) -> bool {
        let (lock, _) = &*self.anim_signal;
        *lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_animating(&self, active: bool) {
        let (lock, cvar) = &*self.anim_signal;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if *guard != active {
            *guard = active;
            if active {
                cvar.notify_one();
            }
        }
    }

    /// One gate for all motion decisions: the app toggle plus Focus Mode.
    fn animations_effective(&self) -> bool {
        effective_animations_enabled(self.settings.animations_enabled, self.settings.focus_mode)
    }

    fn ensure_animation(&mut self) {
        // `EffectiveAnimationsEnabled` gate. With the toggle off (or in Focus
        // Mode) the VSync producer must stay parked at its condvar, and auto-hide
        // must snap instantly instead of waiting for frames that never arrive.
        if !self.animations_effective() {
            self.set_animating(false);
            self.last_frame = None;
            self.last_animation_request = None;
            let snapped = if self.target_hidden { 1.0 } else { 0.0 };
            if (self.hide_progress - snapped).abs() > f32::EPSILON {
                self.hide_progress = snapped;
                self.update_visuals();
            }
            return;
        }

        let now = Instant::now();
        // Rate-limit repeated wake-ups only while the VSync loop is already running;
        // if VSync is idle, never drop a wake-up (e.g. a window close right after hover).
        if self.is_animating() && !layout::animation_request_due(self.last_animation_request, now) {
            return;
        }
        self.last_animation_request = Some(now);
        if self.last_frame.is_none() {
            self.last_frame = Some(now);
        }
        // `tick_pending` belongs to the VSync producer and is cleared ONLY by the
        // `WM_APP_VSYNC` handler. Clearing it here would make the VSync thread post
        // a fresh message for every animation request on the mouse hot path.
        self.set_animating(true);
    }

    fn rebuild_visual_tree(&mut self) {
        self.dcomp.remove_all_children(&self.dcomp.root);
        self.dcomp.add_child(&self.dcomp.root, &self.pill.left);
        self.dcomp.add_child(&self.dcomp.root, &self.pill.center);
        self.dcomp.add_child(&self.dcomp.root, &self.pill.right);
        self.pill_scroll_container.remove_all_children();
        self.dcomp
            .add_child(&self.dcomp.root, &self.pill_scroll_container);
        self.scroll_container.remove_all_children();
        self.dcomp
            .add_child(&self.dcomp.root, &self.scroll_container);

        for i in 0..3 {
            self.pill_scroll_container.add_child(&self.shell_bgs[i]);
            self.pill_scroll_container.add_child(&self.shell_icons[i]);
        }
        self.pill_scroll_container.add_child(&self.divider);

        while self.slot_visuals.len() < self.slots.len() {
            if let (Ok(icon), Ok(dot), Ok(badge)) = (
                self.dcomp.create_node(),
                self.dcomp.create_node(),
                self.dcomp.create_node(),
            ) {
                icon.hide();
                dot.hide();
                badge.hide();
                self.slot_visuals.push(SlotVisuals {
                    icon,
                    dot,
                    badge: Some(badge),
                    badge_count: 0,
                    bound_source: String::new(),
                    bound_has_icon: false,
                    bound_dot_state: 0,
                    fading: false,
                });
            } else {
                break;
            }
        }
        self.slot_visuals.truncate(self.slots.len());

        for sv in &self.slot_visuals {
            self.scroll_container.add_child(&sv.icon);
            self.scroll_container.add_child(&sv.dot);
            if let Some(badge) = &sv.badge {
                self.scroll_container.add_child(badge);
            }
        }
        for wv in &self.widget_visuals {
            self.pill_scroll_container.add_child(&wv.bg);
            self.pill_scroll_container.add_child(&wv.node);
        }

        self.sync_stack_visuals();
        for sv in &self.stack_visuals {
            // Backdrop stays inside the rounded pill clip; the folder icon lives in
            // the headroom-capable container so magnification can lift it.
            self.pill_scroll_container.add_child(&sv.bg);
            self.scroll_container.add_child(&sv.icon);
        }

        self.pill_scroll_container.add_child(&self.widget_divider);
        let count = visible_tray_pin_count(&self.settings, self.slots.len(), self.mon_w_dips());
        self.tray_pin_visuals.truncate(count);
        if self.hovered_tray_pin.is_some_and(|index| index >= count) {
            self.hovered_tray_pin = None;
        }
        while self.tray_pin_visuals.len() < count {
            let (Ok(bg), Ok(icon)) = (self.dcomp.create_node(), self.dcomp.create_node()) else {
                break;
            };
            bg.hide();
            icon.hide();
            self.tray_pin_visuals.push(TrayPinVisual {
                bg,
                icon,
                source: String::new(),
                has_icon: false,
                label: String::new(),
            });
        }
        for pin in &self.tray_pin_visuals {
            self.pill_scroll_container.add_child(&pin.bg);
            self.pill_scroll_container.add_child(&pin.icon);
        }
        self.pill_scroll_container.add_child(&self.tray_bg);
        self.pill_scroll_container.add_child(&self.tray_node);
        self.pill_scroll_container.add_child(&self.bin_bg);
        self.pill_scroll_container.add_child(&self.bin_node);

        self.dcomp
            .add_child(&self.dcomp.root, &self.scroll_prev_node);
        self.dcomp
            .add_child(&self.dcomp.root, &self.scroll_next_node);
        self.dcomp.add_child(&self.dcomp.root, &self.drag_ghost);
        self.dcomp.add_child(&self.dcomp.root, &self.tooltip_node);
        // Any structural rebuild invalidates the frozen resting layout.
        self.hover_rest_sm = None;
    }

    /// Reconciles `stack_visuals` with `settings.stack_folders`, reusing the
    /// DComp nodes of folders that stay. A light strip carries no folder stacks.
    fn sync_stack_visuals(&mut self) {
        let desired: Vec<String> = if self.light_strip {
            Vec::new()
        } else {
            self.settings
                .stack_folders
                .iter()
                .map(|path| path.trim())
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let same = self.stack_visuals.len() == desired.len()
            && self
                .stack_visuals
                .iter()
                .zip(desired.iter())
                .all(|(sv, path)| sv.path == *path);
        if same {
            return;
        }
        let mut previous = std::mem::take(&mut self.stack_visuals);
        for path in desired {
            if let Some(pos) = previous.iter().position(|sv| sv.path == path) {
                self.stack_visuals.push(previous.remove(pos));
                continue;
            }
            let (Ok(bg), Ok(icon)) = (self.dcomp.create_node(), self.dcomp.create_node()) else {
                break;
            };
            bg.hide();
            icon.hide();
            self.stack_visuals.push(StackVisual {
                label: layout::stack_label(&path),
                path,
                bg,
                icon,
                bound_source: String::new(),
                bound_has_icon: false,
                scale: 1.0,
                scale_velocity: 0.0,
                cached_count: None,
            });
        }
        let count = self.stack_visuals.len();
        if self.hovered_stack.is_some_and(|index| index >= count) {
            self.hovered_stack = None;
        }
        if self.pressed_stack.is_some_and(|index| index >= count) {
            self.pressed_stack = None;
        }
    }

    /// Visible dock widgets in `settings.widget_order` (canonical, case-insensitive),
    /// unlisted widgets keeping their catalog order.
    fn ordered_dock_widgets(&self) -> Vec<&'static widgets::WidgetDescriptor> {
        let active: Vec<&'static widgets::WidgetDescriptor> =
            widgets::active_widgets(&self.settings)
                .into_iter()
                .filter(|wd| {
                    // A secondary light strip reserves shelf space but carries no
                    // widget cards; `secondary_widgets = false` suppresses them on
                    // any secondary dock, while island placement keeps its priority.
                    // `active_widgets` already applies the Settings toggles (Media
                    // included), so there is no second `show_media` gate here.
                    !self.light_strip
                        && (!self.is_secondary || self.settings.secondary_widgets)
                        && !dynamic_island::is_widget_placed_on_connected_display(
                            &self.settings,
                            wd.id,
                            &self.all_monitors,
                        )
                })
                .collect();
        let ids: Vec<&'static str> = active.iter().map(|wd| wd.id).collect();
        layout::apply_widget_order(&ids, &self.settings.widget_order)
            .into_iter()
            .filter_map(|id| active.iter().find(|wd| wd.id == id).copied())
            .collect()
    }

    /// Live chip width: settings-driven dynamic width times the density override.
    fn widget_width_for(&self, id: &str) -> f32 {
        self.widget_runtime
            .widget_width_dips_with_settings(id, &self.settings)
            * self.effective.widget_density(id)
    }

    fn sync_widget_visuals(&mut self) {
        let active = self.ordered_dock_widgets();
        let same = self.widget_visuals.len() == active.len()
            && self
                .widget_visuals
                .iter()
                .zip(active.iter())
                .all(|(v, d)| v.id == d.id);
        if same {
            return;
        }
        self.widget_visuals.clear();
        for wd in active {
            if let (Ok(bg), Ok(node)) = (self.dcomp.create_node(), self.dcomp.create_node()) {
                bg.hide();
                let width_dips = self.widget_width_for(wd.id);
                self.widget_visuals.push(WidgetVisual {
                    id: wd.id,
                    width_dips,
                    bg,
                    node,
                    last_key: String::new(),
                });
            }
        }
        self.rebuild_visual_tree();
    }

    fn refresh_widgets(&mut self) -> bool {
        self.sync_widget_visuals();
        self.widget_runtime.tick(&self.settings);
        if let Some(island) = &mut self.dynamic_island {
            island.tick();
        }
        let mut changed = false;
        let mut width_changed = false;
        let widget_h = design::widget_height_dips(pill_height_dips(&self.settings));
        let mut key_scratch = String::with_capacity(256);
        let live_widths: Vec<f32> = self
            .widget_visuals
            .iter()
            .map(|wv| self.widget_width_for(wv.id))
            .collect();
        for (wv, live_w) in self.widget_visuals.iter_mut().zip(live_widths) {
            if (live_w - wv.width_dips).abs() > 0.5 {
                wv.width_dips = live_w;
                wv.last_key.clear();
                width_changed = true;
            }
            let interaction = if self.pressed_widget == Some(wv.id) {
                WidgetInteraction::Pressed(self.hovered_widget_sub)
            } else if self.hovered_widget == Some(wv.id) {
                WidgetInteraction::Hovered(self.hovered_widget_sub)
            } else {
                WidgetInteraction::Idle
            };
            let spec = self
                .widget_runtime
                .chip_spec(wv.id, &self.settings, self.is_dark);
            spec.write_cache_key(&mut key_scratch);
            let border_rgb = self
                .settings
                .widget_border_color
                .as_deref()
                .and_then(design::parse_hex_rgb);
            let border_width = self.settings.widget_border_thickness.clamp(0.0, 8.0) as f32;
            let _ = write!(
                key_scratch,
                "|{:.1}|{widget_h:.1}|{}|{:?}|{}|{:?}",
                wv.width_dips, self.is_dark, border_rgb, border_width, interaction,
            );
            let key = key_scratch.as_str();
            if key != wv.last_key
                && let Ok(surf) = self.dcomp.create_widget_chip_surface(
                    &spec,
                    wv.width_dips,
                    widget_h,
                    self.dpi,
                    self.is_dark,
                    interaction,
                    border_rgb,
                    border_width,
                )
            {
                wv.node.set_content(Some(&surf));
                wv.last_key.clone_from(&key_scratch);
                changed = true;
            }
        }
        if width_changed {
            self.sync_window_bounds();
        }
        if self.settings.show_recycle_bin {
            let full = widgets::is_recycle_bin_non_empty();
            if full != self.bin_is_full {
                self.bin_is_full = full;
                self.bin_node.set_content(Some(if full {
                    &self.bin_full_surf
                } else {
                    &self.bin_empty_surf
                }));
                changed = true;
            }
        }
        changed
    }

    /// Mouse / hover paths run at up to 1000 Hz. Rebuilding widget chips is
    /// cheap but not free; coalesce those requests into one per frame budget.
    /// Settings reloads and animation ticks call `refresh_widgets` directly.
    fn refresh_widgets_throttled(&mut self) -> bool {
        let now = Instant::now();
        if !layout::widget_refresh_due(self.last_widget_refresh, now) {
            return false;
        }
        self.last_widget_refresh = Some(now);
        self.refresh_widgets()
    }

    fn evaluate_auto_hide(&mut self) {
        self.evaluate_auto_hide_inner(true);
    }

    /// 200 ms revealed-guard tick: only cached shell data may be read here.
    /// No overlap rescan, no Start-process validation (WS-2 hot path).
    fn evaluate_auto_hide_guard(&mut self) {
        self.evaluate_auto_hide_inner(false);
    }

    /// Reads `is_anchored()` at most once every `ANCHOR_CACHE_TTL`, and never from
    /// the 200 ms guard tick. WS-3 owns the >=2 s PID -> exe cache inside the
    /// shell API; this snapshot keeps its process query off the guard tick too.
    fn anchored_for_decision(&mut self, now: Instant, allow_cross_process: bool) -> bool {
        if !allow_cross_process {
            return self.cached_anchored.is_some_and(|(_, value)| value);
        }
        let stale = self
            .cached_anchored
            .is_none_or(|(at, _)| now.saturating_duration_since(at) >= layout::ANCHOR_CACHE_TTL);
        if stale {
            let anchored = opentaskbar_shell::flyout_reposition::is_anchored();
            self.cached_anchored = Some((now, anchored));
        }
        self.cached_anchored.is_some_and(|(_, value)| value)
    }

    /// A secondary strip hides only for a fullscreen game on **its own** monitor
    /// (#179). Scans are throttled with the same 1 s budget as the overlap scan
    /// and never run from the 200 ms revealed-guard tick.
    fn refresh_fullscreen_state(&mut self, now: Instant) {
        if !self.is_secondary {
            return;
        }
        if self
            .last_fullscreen_scan
            .is_none_or(|t| now.saturating_duration_since(t) >= layout::OVERLAP_SCAN_MIN_INTERVAL)
        {
            self.last_fullscreen_scan = Some(now);
            let was_hidden = self.fullscreen_on_own_monitor;
            self.fullscreen_on_own_monitor = win32::is_any_fullscreen_on_monitor(&self.monitor);
            if self.fullscreen_on_own_monitor && !was_hidden {
                // A modal picker covers the game while its window is alive.
                window_picker::request_close_all();
            }
        }
    }

    /// Arms the one Win-key grace window. Every producer (low-level hook, direct
    /// Win32 key path, keyboard navigation) goes through this gate, so the
    /// `reveal_dock_on_win_key` / auto-hide settings can never be bypassed.
    fn arm_win_key_reveal(&mut self) {
        if win_key_reveal_enabled(
            self.settings.reveal_dock_on_win_key,
            self.settings.auto_hide,
        ) {
            self.win_key_reveal_until =
                Some(Instant::now() + Duration::from_millis(layout::WIN_KEY_GRACE_MS));
        }
    }

    fn evaluate_auto_hide_inner(&mut self, allow_cross_process: bool) {
        let now = Instant::now();
        if allow_cross_process {
            self.refresh_fullscreen_state(now);
        }
        let pointer_busy = self.tracking_mouse
            || self.drag.is_some()
            || self
                .preview
                .as_deref()
                .is_some_and(|p| p.is_visible() || p.is_pointer_inside());

        let win_key_hold = win_key_reveal_enabled(
            self.settings.reveal_dock_on_win_key,
            self.settings.auto_hide,
        ) && self
            .win_key_reveal_until
            .is_some_and(|until| Instant::now() < until);
        let edge = effective_edge(&self.settings);
        let mut pt = POINT { x: 0, y: 0 };
        let at_edge = unsafe { GetCursorPos(&mut pt) != 0 }
            && layout::is_at_edge_trigger(edge, self.monitor.clone(), pt.x, pt.y, self.dpi);
        let overlap = if self.settings.auto_hide == AutoHideMode::Smart {
            if allow_cross_process {
                // `dirty` comes only from a real shell/foreground change; even then
                // it cannot shorten the 1 s minimum between cross-process scans.
                let periodic_due = self.last_overlap_scan.is_none_or(|t| {
                    now.saturating_duration_since(t) >= layout::OVERLAP_SCAN_MIN_INTERVAL
                });
                if layout::overlap_scan_due(
                    self.last_overlap_scan,
                    self.overlap_dirty,
                    periodic_due,
                    now,
                ) {
                    self.overlap_dirty = false;
                    self.last_overlap_scan = Some(now);
                    self.cached_overlap = self.is_overlapped_by_window();
                }
            }
            // The 200 ms guard tick deliberately reuses the cached result.
            self.cached_overlap
        } else {
            false
        };
        // Cached state only: `is_open_now()` walks windows and is reserved for
        // the WinEvent hook, where it does not block the UI thread.
        let start_menu_open = crate::platform::start_menu_state::is_open();
        let anchor_pending = opentaskbar_shell::flyout_reposition::anchor_pending();
        if allow_cross_process
            && (anchor_pending != self.last_anchor_pending
                || start_menu_open != self.last_start_menu_open)
        {
            // An attempt concluded or Start visibility changed: the cached
            // answer is stale and must be re-read on this (non-guard) scan.
            self.last_anchor_pending = anchor_pending;
            self.last_start_menu_open = start_menu_open;
            self.cached_anchored = None;
        }
        let anchored = self.anchored_for_decision(now, allow_cross_process);
        // If the shell's reposition attempt finished without anchoring Start,
        // hiding the dock is the only way to guarantee no overlap remains.
        // While the attempt is pending the dock must stay put (no ~800 ms flicker).
        let should_hide = self.fullscreen_on_own_monitor
            || start_menu_hide_fallback(
                start_menu_open,
                anchored,
                anchor_pending,
                self.settings.auto_hide,
            )
            || revealed_guard_should_hide(
                self.settings.auto_hide,
                pointer_busy,
                at_edge,
                overlap,
                start_menu_open,
                self.drag.is_some() || self.resize_drag.is_some() || self.scroll_pan.is_some(),
                win_key_hold,
            );

        if self.target_hidden != should_hide {
            self.target_hidden = should_hide;
            self.ensure_animation();
        }
        // 200 ms revealed guard: catches missed WM_MOUSELEAVE and foreground shifts.
        unsafe {
            if self.settings.auto_hide != AutoHideMode::Disabled && !self.target_hidden {
                SetTimer(
                    self.hwnd as HWND,
                    TIMER_REVEALED_GUARD,
                    layout::REVEALED_GUARD_MS as u32,
                    None,
                );
            } else {
                KillTimer(self.hwnd as HWND, TIMER_REVEALED_GUARD);
            }
        }
    }

    /// Checks whether any visible non-minimized, non-cloaked application window overlaps
    /// this monitor's dock shelf (`DockAutoHideController.EvaluateWindowOverlap`).
    /// Uses `DWMWA_EXTENDED_FRAME_BOUNDS` for the true visible frame and filters shell
    /// overlays (Start/Alt+Tab/Task View) via `platform::win32_ex::is_shell_overlay` parity.
    fn is_overlapped_by_window(&self) -> bool {
        let rect = compute_window_rect_with(
            &self.settings,
            self.slots.len(),
            self.monitor.clone(),
            &self.effective,
        );
        let edge = effective_edge(&self.settings);
        let headroom_px = (layout::HEADROOM_DIPS * self.dpi).round() as i32;
        let (dl, dt, dr, db) = match edge {
            opentaskbar_core::settings::DockEdge::Bottom => (
                rect.0,
                rect.1 + headroom_px,
                rect.0 + rect.2,
                rect.1 + rect.3,
            ),
            opentaskbar_core::settings::DockEdge::Left => (
                rect.0,
                rect.1,
                rect.0 + rect.2 - headroom_px,
                rect.1 + rect.3,
            ),
            opentaskbar_core::settings::DockEdge::Right => (
                rect.0 + headroom_px,
                rect.1,
                rect.0 + rect.2,
                rect.1 + rect.3,
            ),
        };
        // Reuse the snapshot taken by `sync_slots`; a second full EnumWindows + OpenProcess pass
        // per 800 ms poll was the single most expensive part of idle auto-hide evaluation.
        for win in &self.running_windows {
            let hwnd = win.hwnd as HWND;
            unsafe {
                if hwnd as usize == self.hwnd
                    || IsWindowVisible(hwnd) == 0
                    || IsIconic(hwnd) != 0
                    || win.pid == std::process::id()
                {
                    continue;
                }
                let mut cloaked: i32 = 0;
                if DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED as u32,
                    (&mut cloaked as *mut i32).cast(),
                    size_of::<i32>() as u32,
                ) == 0
                    && cloaked != 0
                {
                    continue;
                }
                // Single source of truth (WS-B): class list plus exe/PID check
                // for CoreWindow hosts, so a random UWP overlay is not ignored.
                // The snapshot already holds the exe path, avoiding a second query.
                if crate::platform::win32_ex::is_shell_overlay_for(win.hwnd, &win.exe_path) {
                    continue;
                }
                let mut rc: RECT = std::mem::zeroed();
                let hr = DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                    (&mut rc as *mut RECT).cast(),
                    size_of::<RECT>() as u32,
                );
                if hr != 0 && GetWindowRect(hwnd, &mut rc) == 0 {
                    continue;
                }
                if is_full_screen_on_monitor(
                    (rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top),
                    self.monitor.clone(),
                    2,
                ) || IsZoomed(hwnd) != 0
                {
                    return true;
                }
                if rc.right > dl && rc.left < dr && rc.bottom > dt && rc.top < db {
                    return true;
                }
            }
        }
        false
    }

    fn rebuild_theme_surfaces(&mut self) {
        let dpi = self.dpi;
        let is_dark = self.is_dark;
        let base = layout::size_profile(&self.settings).cell;
        let pill_h = pill_height_dips(&self.settings);

        // The DComp dock used to ignore `ThemeManager` entirely: editor/pack changes
        // to DockMaterial, DockBorderMaterial, WidgetCard(Hover/Pressed), DockHighlight
        // and DockRadius were invisible here. Resolve those materials now while
        // keeping the stock dock look until a token actually diverges from default.
        let materials = design::theme::ThemeManager::materials(design::theme::SurfaceKind::Dock);
        let tokens = &materials.tokens;
        let themed_rgb = |key: &str, legacy: design::Rgb| -> design::Rgb {
            tokens
                .get(key)
                .filter(|value| !design::theme::is_default_token(key, is_dark, value))
                .and_then(design::theme::token_rgba)
                .map(|color| (color.r, color.g, color.b))
                .unwrap_or(legacy)
        };
        let radius = if self.settings.dock_corner_radius.is_some() {
            self.settings.corner_radius_dips()
        } else {
            materials
                .radii
                .get("DockRadius")
                .map(|corner| corner.top_left as f32)
                .filter(|value| (*value - 20.0).abs() > 0.5) // stock reference radius
                .unwrap_or_else(|| self.settings.corner_radius_dips())
        };
        let glass_fill = design::glass::glass_surface_color_for(
            design::theme::SurfaceKind::Dock,
            is_dark,
            self.settings.dock_opacity,
        );
        let pill_rgb = design::theme::token_rgba(&materials.fill)
            .map(|color| (color.r, color.g, color.b))
            .unwrap_or_else(|| {
                themed_rgb("DockMaterial", (glass_fill.r, glass_fill.g, glass_fill.b))
            });
        let rgba = |rgb: design::Rgb, alpha: f32| (rgb.0, rgb.1, rgb.2, alpha);
        let (bd_rgb, hover_rgb, press_rgb, div_rgb, dot_run_rgb, dot_act_rgb) = if is_dark {
            (
                rgba(themed_rgb("DockBorderMaterial", (0xff, 0xff, 0xff)), 0.11),
                rgba(themed_rgb("WidgetCardHover", (0xff, 0xff, 0xff)), 0.10),
                rgba(themed_rgb("WidgetCardPressed", (0xff, 0xff, 0xff)), 0.16),
                rgba(themed_rgb("DockHighlight", (0xff, 0xff, 0xff)), 0.14),
                (0xd4, 0xd4, 0xd8, 0.94),
                (0xff, 0xff, 0xff, 1.0),
            )
        } else {
            (
                rgba(themed_rgb("DockBorderMaterial", (0x18, 0x18, 0x1b)), 0.11),
                rgba(themed_rgb("WidgetCardHover", (0x18, 0x18, 0x1b)), 0.07),
                rgba(themed_rgb("WidgetCardPressed", (0x18, 0x18, 0x1b)), 0.13),
                rgba(themed_rgb("DockHighlight", (0x18, 0x18, 0x1b)), 0.14),
                (0x52, 0x52, 0x5b, 0.85),
                (0x18, 0x18, 0x1b, 0.95),
            )
        };

        let glass_cfg = design::spatial_glass::SpatialGlassConfig::from_settings(
            &self.settings,
            self.settings.dock_background,
            self.settings.dock_opacity,
            is_dark,
            false,
        );
        if let Some(ref mut glass) = self.spatial_glass {
            glass.apply_config(glass_cfg);
        }
        let glass_host_active = self
            .spatial_glass
            .as_ref()
            .is_some_and(design::spatial_glass::SpatialGlassHost::is_enabled);
        let has_custom_border = self.settings.dock_border_color.is_some()
            || (self.settings.dock_border_thickness - 1.0).abs() > 0.05;

        let true_black = self.settings.dock_background == SurfaceBackground::TrueBlack;
        // Use the same opacity→mode decision as the theme/material layer so the
        // generated materials and the DComp pill can never disagree.
        let effective_background = design::theme::background_for_opacity(
            self.settings.dock_background,
            self.settings.dock_opacity,
        );
        let bg_alpha = match self.settings.dock_background {
            SurfaceBackground::None => 0.0_f32,
            SurfaceBackground::Solid => 0.98_f32,
            SurfaceBackground::Glass if glass_host_active => 0.0_f32,
            SurfaceBackground::Glass => match effective_background {
                SurfaceBackground::None => 0.0_f32,
                SurfaceBackground::Solid => 0.98_f32,
                _ => (self.settings.dock_opacity.clamp(0.1, 1.0) * 0.88) as f32,
            },
            SurfaceBackground::TrueBlack => 1.0_f32,
        };
        let bd_alpha = if self.settings.dock_background == SurfaceBackground::None
            || true_black
            || (glass_host_active && !has_custom_border)
        {
            0.0
        } else {
            bd_rgb.3
        };
        let user_bd = self
            .settings
            .dock_border_color
            .as_deref()
            .and_then(design::parse_hex_rgb)
            .unwrap_or((bd_rgb.0, bd_rgb.1, bd_rgb.2));
        let pill_bg = if true_black {
            (0_u8, 0_u8, 0_u8)
        } else {
            (pill_rgb.0, pill_rgb.1, pill_rgb.2)
        };
        if let Ok(pill) = self.dcomp.create_pill_nodes(
            radius * dpi,
            pill_h * dpi,
            (pill_bg.0, pill_bg.1, pill_bg.2, bg_alpha),
            (user_bd.0, user_bd.1, user_bd.2, bd_alpha),
            dpi,
            self.settings.dock_border_thickness.clamp(0.0, 8.0) as f32,
            true_black,
        ) {
            self.pill = pill;
        }
        let shell_px = (base * dpi).ceil() as u32;
        if let Ok(s) =
            self.dcomp
                .create_rounded_rect_surface(shell_px, shell_px, 9.0 * dpi, hover_rgb)
        {
            self.shell_bg_hover = s;
        }
        if let Ok(s) =
            self.dcomp
                .create_rounded_rect_surface(shell_px, shell_px, 9.0 * dpi, press_rgb)
        {
            self.shell_bg_press = s;
        }
        let glyph_px = ((base * 0.68).max(22.0) * dpi).ceil() as u32;
        for (idx, glyph) in [ShellGlyph::Start, ShellGlyph::Search, ShellGlyph::TaskView]
            .into_iter()
            .enumerate()
        {
            if let Ok(s) = self
                .dcomp
                .create_shell_glyph_surface(glyph, glyph_px, is_dark)
            {
                self.shell_icons[idx].set_content(Some(&s));
            }
        }
        if let Ok(s) = self.dcomp.create_rounded_rect_surface(
            (1.5 * dpi).ceil() as u32,
            (26.0 * dpi).ceil() as u32,
            0.75 * dpi,
            div_rgb,
        ) {
            self.divider.set_content(Some(&s));
            self.widget_divider.set_content(Some(&s));
        }
        let se_w_px = (SCROLL_EDGE_W_DIPS * dpi).ceil() as u32;
        let se_h_px = (pill_h * dpi).ceil() as u32;
        if let Ok(s) = self.dcomp.create_scroll_edge_surface(
            true,
            se_w_px,
            se_h_px,
            radius * dpi,
            pill_rgb,
            is_dark,
            false,
            dpi,
        ) {
            self.scroll_prev_surf = s;
        }
        if let Ok(s) = self.dcomp.create_scroll_edge_surface(
            true,
            se_w_px,
            se_h_px,
            radius * dpi,
            pill_rgb,
            is_dark,
            true,
            dpi,
        ) {
            self.scroll_prev_hover_surf = s;
        }
        if let Ok(s) = self.dcomp.create_scroll_edge_surface(
            false,
            se_w_px,
            se_h_px,
            radius * dpi,
            pill_rgb,
            is_dark,
            false,
            dpi,
        ) {
            self.scroll_next_surf = s;
        }
        if let Ok(s) = self.dcomp.create_scroll_edge_surface(
            false,
            se_w_px,
            se_h_px,
            radius * dpi,
            pill_rgb,
            is_dark,
            true,
            dpi,
        ) {
            self.scroll_next_hover_surf = s;
        }
        if let Ok(s) =
            self.dcomp
                .create_shell_glyph_surface(ShellGlyph::TrayChevron, glyph_px, is_dark)
        {
            self.tray_node.set_content(Some(&s));
        }
        if let (Some(empty), Some(full)) = (
            icon_cache::extract_recycle_bin_icon(false)
                .and_then(|raw| self.dcomp.create_app_icon_surface(&raw).ok()),
            icon_cache::extract_recycle_bin_icon(true)
                .and_then(|raw| self.dcomp.create_app_icon_surface(&raw).ok()),
        ) {
            self.bin_empty_surf = empty;
            self.bin_full_surf = full;
            self.bin_surf_px = icon_cache::RECYCLE_BIN_ICON_PX as f32;
        } else {
            if let Ok(s) = self.dcomp.create_shell_glyph_surface(
                ShellGlyph::RecycleBinEmpty,
                glyph_px,
                is_dark,
            ) {
                self.bin_empty_surf = s;
            }
            if let Ok(s) =
                self.dcomp
                    .create_shell_glyph_surface(ShellGlyph::RecycleBinFull, glyph_px, is_dark)
            {
                self.bin_full_surf = s;
            }
            self.bin_surf_px = glyph_px as f32;
        }
        self.bin_node.set_content(Some(if self.bin_is_full {
            &self.bin_full_surf
        } else {
            &self.bin_empty_surf
        }));
        let dot_h_px = (3.5 * dpi).ceil() as u32;
        if let Ok(s) = self.dcomp.create_rounded_rect_surface(
            (6.0 * dpi).ceil() as u32,
            dot_h_px,
            2.0 * dpi,
            dot_run_rgb,
        ) {
            self.dot_running = s;
        }
        if let Ok(s) = self.dcomp.create_rounded_rect_surface(
            (16.0 * dpi).ceil() as u32,
            dot_h_px,
            2.0 * dpi,
            dot_act_rgb,
        ) {
            self.dot_active = s;
        }
        for sv in &mut self.slot_visuals {
            sv.bound_dot_state = 255;
        }
        if let Some(surf) = icon_cache::extract_default_folder_icon()
            .and_then(|raw| self.dcomp.create_app_icon_surface(&raw).ok())
        {
            self.folder_fallback_surf = Some(surf);
        }
        for sv in &mut self.stack_visuals {
            sv.bound_source.clear();
            sv.bound_has_icon = false;
        }
        self.rebuild_visual_tree();
    }

    /// Requests the disk save + full DComp rebuild on the idle `WM_APP_SETTINGS_RELOAD`
    /// message instead of inside a hover timer / `WM_MOUSEMOVE` handler.
    fn defer_settings_reload(&mut self) {
        if layout::claim_deferred_reload(&mut self.settings_reload_pending) {
            let posted = unsafe { PostMessageW(self.hwnd as HWND, WM_APP_SETTINGS_RELOAD, 0, 0) };
            if posted == 0 {
                // No idle message will ever consume the flag; roll it back so a
                // later hover-close can arm a fresh reload instead of being
                // coalesced into a request that was never delivered.
                self.settings_reload_pending = false;
            }
        }
    }

    /// Idle half of the deferred reload. The flyout already mutated `self.settings`
    /// in memory, so persist that change before re-reading it from disk.
    fn flush_deferred_settings_reload(&mut self) {
        if !layout::take_deferred_reload(&mut self.settings_reload_pending) {
            return;
        }
        let _ = settings::save_to(&settings::settings_path(), &self.settings);
        self.reload_settings_live();
    }

    fn reload_settings_live(&mut self) {
        // A settings change may disable previews or reorder the dock: close any
        // modal picker instead of leaving a stale snapshot on screen.
        window_picker::request_close_all();
        let next = settings::load_from(&settings::settings_path());
        // Re-resolve WS-1 effective geometry/density for this monitor before any
        // rebuild; the cached values feed every subsequent hot-path frame.
        self.effective = EffectiveMetrics::for_monitor(&next, self.mon_w_dips());
        self.settings = next;
        window_picker::set_picker_enabled(self.settings.previews_enabled);
        set_win_key_reveal_gate(&self.settings);
        crate::apply_native_flyout_reposition(&self.settings);
        self.refresh_shell_button_anchors();
        // The Settings window runs in a separate process: apply its language change here too.
        loc::set_language(&self.settings.language);
        design::fonts::apply_settings(&self.settings);
        let theme_dir = if self.settings.theme_pack.trim().is_empty() {
            None
        } else {
            Some(
                std::env::var_os("LOCALAPPDATA")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir)
                    .join("OpenTaskbar")
                    .join("Themes")
                    .join(&self.settings.theme_pack),
            )
        };
        crate::platform::icon_pack::set_active_pack(theme_dir.as_deref());
        register_global_hotkeys(self.hwnd as HWND, &self.settings);
        let registry_icons = tray_host::enumerate_tray_icons();
        let pinned_paths: Vec<String> = self
            .settings
            .user_pinned_background_apps
            .iter()
            .filter_map(|key| {
                registry_icons
                    .iter()
                    .find(|icon| tray_host::matches_pin(icon, key))
                    .map(|icon| icon.executable_path.clone())
            })
            .collect();
        if pinned_paths.is_empty() {
            tray_host::stop_host();
        } else {
            tray_host::start_host(&pinned_paths);
        }
        self.hovered_tray_pin = None;
        self.pressed_tray_pin = None;
        self.all_monitors = win32::enumerate_monitors();
        let updated_fps = Self::compute_target_fps(&self.settings, &self.monitor.device_name);
        self.target_fps.store(updated_fps, Ordering::Relaxed);
        crate::ui::flyout::set_motion_prefs(self.animations_effective(), updated_fps);
        self.is_dark = resolve_is_dark(self.settings.theme);
        if let Some(ref mut island) = self.dynamic_island {
            island.apply(&self.settings, &self.all_monitors, self.is_dark);
        }
        self.pins = load_effective_pins(&self.settings);
        for wv in &mut self.widget_visuals {
            wv.last_key.clear();
        }
        self.tooltip_cache = None;
        self.rebuild_theme_surfaces();
        if let Some(ref corner) = self.show_desktop_corner {
            let corner_h = pill_height_dips(&self.settings)
                + self.settings.dock_bottom_gap_pixels.max(0) as f32;
            corner.update_for_edge(
                effective_edge(&self.settings),
                self.monitor.clone(),
                corner_h,
                self.is_dark,
                self.settings.show_desktop_button,
            );
        }
        if self.monitor.is_primary {
            let auto_hide = self.settings.auto_hide != AutoHideMode::Disabled;
            let desired_total_dips = pill_height_dips(&self.settings)
                + self.effective.bottom_inset_dips()
                + VERTICAL_EDGE_GAP;
            let desired_total_px = (desired_total_dips * self.dpi).ceil() as i32;
            opentaskbar_shell::taskbar::update_appbar_height(
                self.hwnd,
                auto_hide,
                desired_total_px,
            );
        }
        self.sync_slots(false);
        self.sync_window_bounds();
        // Settings reload is a deliberate full rebuild, not a hot-path wake-up.
        let _ = self.refresh_widgets();
        self.evaluate_auto_hide();
        self.update_visuals();
    }

    fn request_missing_icons(&mut self) {
        let mut missing = Vec::new();
        let sources = self
            .slots
            .iter()
            .map(|slot| &slot.icon_source)
            .chain(
                self.settings
                    .user_pinned_background_apps
                    .iter()
                    .take(self.tray_pin_visuals.len()),
            )
            .chain(self.stack_visuals.iter().map(|sv| &sv.path));
        for source in sources {
            if !self.icon_surfaces.contains_key(source) {
                if let Some(raw) = icon_cache::read_cached_raw(source)
                    && let Ok(surf) = self.dcomp.create_app_icon_surface(&raw)
                {
                    self.icon_surfaces.insert(source.clone(), Some(surf));
                    continue;
                }
                if self.in_flight_icons.insert(source.clone()) {
                    missing.push(source.clone());
                }
            }
        }
        if missing.is_empty() {
            return;
        }
        let pending = self.pending_icons.clone();
        let hwnd_val = self.hwnd;
        std::thread::Builder::new()
            .name("dock-icons".to_owned())
            .spawn(move || {
                let batch = icon_cache::resolve_batch_raw(&missing);
                if let Ok(mut queue) = pending.lock() {
                    queue.push(batch);
                }
                unsafe {
                    let _ = PostMessageW(hwnd_val as HWND, WM_APP_ICONS, 0, 0);
                }
            })
            .ok();
    }

    fn drain_loaded_icons(&mut self) {
        let batches: Vec<HashMap<String, Option<RawIcon>>> = {
            let Ok(mut queue) = self.pending_icons.lock() else {
                return;
            };
            std::mem::take(&mut *queue)
        };
        if batches.is_empty() {
            return;
        }
        for batch in batches {
            for (spec, raw_opt) in batch {
                self.in_flight_icons.remove(&spec);
                let surf = raw_opt.and_then(|raw| self.dcomp.create_app_icon_surface(&raw).ok());
                self.icon_surfaces.insert(spec, surf);
            }
        }
        self.update_visuals();
    }

    fn sync_slots(&mut self, animate_closes: bool) -> bool {
        if self.drag.is_some() {
            return false;
        }
        let now = Instant::now();
        // The UI thread never runs EnumWindows/OpenProcess. It only drains a
        // worker reply and starts one throttled request when none is in flight.
        if let Some(rx) = &self.window_snapshot_rx {
            match rx.try_recv() {
                Ok(windows) => {
                    self.running_windows = windows;
                    self.last_window_snapshot = Some(now);
                    self.window_snapshot_in_flight = false;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.window_snapshot_in_flight = false;
                    self.window_snapshot_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let mut running = self.running_windows.clone();
        let mut pruned_closed = false;
        if animate_closes {
            let before = running.len();
            running.retain(|window| tracked_window_is_visible(window.hwnd));
            pruned_closed = running.len() != before;
        }
        if !self.window_snapshot_in_flight
            && (pruned_closed || layout::window_snapshot_due(self.last_window_snapshot, false, now))
        {
            let (tx, rx) = mpsc::channel();
            self.window_snapshot_rx = Some(rx);
            self.window_snapshot_in_flight = true;
            opentaskbar_shell::tracker::enumerate_windows_async(tx);
        }
        if self.light_strip {
            // A shelf strip reserves space and hosts shell buttons only.
            running.clear();
        } else if self.is_secondary
            && !self.settings.mirror_main_dock
            && !layout::secondary_apps_all_monitors(&self.settings)
        {
            running.retain(|window| win32::is_window_on_monitor(window.hwnd, self.monitor.handle));
        }
        if self.settings.current_desktop_only {
            running.retain(|window| tracker::is_on_current_desktop(window.hwnd));
        }
        self.running_windows = running.clone();
        let mut next = build_slots_for(&self.pins, &running, &self.slots, &self.settings);
        // DComp close motion does not use the reference UI's client-area animation APIs, so the
        // OS `SPI_GETCLIENTAREAANIMATION` flag must not veto it (it is frequently
        // off in VMs / battery-saver even when the in-app toggle is on).
        if animate_closes && self.animations_effective() {
            keep_closing_slots(
                &self.slots,
                &mut next,
                Instant::now(),
                tracked_window_is_visible,
            );
        }
        let len_changed = next.len() != self.slots.len();
        let changed = len_changed
            || next.iter().zip(self.slots.iter()).any(|(a, b)| {
                a.key != b.key
                    || a.is_foreground != b.is_foreground
                    || a.windows.len() != b.windows.len()
                    || a.close_started != b.close_started
            });
        self.slots = next;
        if len_changed {
            self.rebuild_visual_tree();
            self.sync_window_bounds();
        }
        if changed {
            // A window set / foreground change is a genuine shell change: request
            // one overlap rescan (still gated by the 1 s minimum interval).
            self.overlap_dirty = true;
            self.update_hovered_index_from_mouse();
            self.sync_preview_hover();
        }
        if self.slots.iter().any(|slot| slot.close_started.is_some()) {
            self.ensure_animation();
        }
        self.request_missing_icons();
        self.prune_icon_surfaces();
        changed
    }

    /// Drops DComp surfaces for icons that are no longer pinned. Without this, the cache grows
    /// with every icon source ever seen (each surface is 64×64 BGRA ≈ 16 KiB of VRAM).
    /// `None` entries are kept: they record a failed extraction and avoid re-spawning the worker.
    fn prune_icon_surfaces(&mut self) {
        let keep: HashSet<&str> = self
            .slots
            .iter()
            .map(|slot| slot.icon_source.as_str())
            .chain(
                self.settings
                    .user_pinned_background_apps
                    .iter()
                    .map(String::as_str),
            )
            .chain(self.stack_visuals.iter().map(|sv| sv.path.as_str()))
            .chain(self.in_flight_icons.iter().map(String::as_str))
            .collect();
        self.icon_surfaces
            .retain(|source, surface| surface.is_none() || keep.contains(source.as_str()));
    }

    fn sync_window_bounds(&mut self) {
        // Feed the live resting content width (dynamic widget widths, stacks) into
        // the window sizing so the HWND always holds the whole resting strip.
        let live = self.rest_content_width_at(Instant::now());
        self.effective.live_rest_content_dips = Some(live);
        self.hover_rest_sm = None;
        let rect = compute_window_rect_with(
            &self.settings,
            self.slots.len(),
            self.monitor.clone(),
            &self.effective,
        );
        if self.last_window_rect != Some(rect) {
            self.last_window_rect = Some(rect);
            win32::reposition_dock_window(self.hwnd, rect.0, rect.1, rect.2, rect.3);
        }
        self.refresh_shell_button_anchors();
    }

    fn mon_w_dips(&self) -> f32 {
        self.monitor.width() as f32 / self.dpi
    }

    fn live_widgets_strip_width(&self, has_apps: bool) -> f32 {
        if self.widget_visuals.is_empty() {
            return 0.0;
        }
        let gap_before = if has_apps { 8.0 } else { 0.0 };
        let gap = widget_gap(&self.settings);
        let cards_w: f32 = self
            .widget_visuals
            .iter()
            .map(|wv| wv.width_dips + gap)
            .sum();
        gap_before + cards_w
    }

    fn live_trail_section_width(&self) -> f32 {
        let stacks = self.stack_visuals.len();
        let base_w = base_trail_width_for(&self.settings, stacks);
        if base_w > 0.0 {
            let pins = if self.settings.show_tray_button {
                self.tray_pin_visuals.len()
            } else {
                0
            };
            let pins_w = pins as f32 * tray_pin_cell(layout::size_profile(&self.settings).cell);
            // Pinned tray icons carry no trailing gap of their own; a folder stack
            // to their left needs one.
            let pins_gap = if pins > 0 && stacks > 0 {
                widget_gap(&self.settings)
            } else {
                0.0
            };
            base_w + pins_w + pins_gap
        } else {
            0.0
        }
    }

    fn live_widgets_section_width(&self) -> f32 {
        let group = layout::start_strip_group(&self.settings);
        let start_w = if is_shell_button_visible(&self.settings, ShellButton::Start) {
            layout::size_profile(&self.settings).cell + widget_gap(&self.settings)
        } else {
            0.0
        };
        self.live_widgets_strip_width(!self.slots.is_empty())
            + self.live_trail_section_width()
            + match group {
                layout::StartStripGroup::Head => 0.0,
                layout::StartStripGroup::AfterApps | layout::StartStripGroup::InWidgets => start_w,
            }
    }

    fn live_pill_geometry(&self) -> (f32, f32, f32, f32) {
        self.live_pill_geometry_at(Instant::now())
    }

    fn live_pill_geometry_at(&self, now: Instant) -> (f32, f32, f32, f32) {
        self.pill_geometry_impl(now, false)
    }

    /// Pill geometry with every slot at rest (scale 1, no fixed-point shift).
    fn rest_pill_geometry_at(&self, now: Instant) -> (f32, f32, f32, f32) {
        self.pill_geometry_impl(now, true)
    }

    /// Resting content strip width including both viewport insets. This is the
    /// exact width the pill needs so `compute_scroll_metrics` reports no
    /// overflow at rest (P0-4: the old pill was 2 DIP short of its viewport).
    fn rest_content_width_at(&self, now: Instant) -> f32 {
        pill_width_for_content(content_strip_width(
            shell_lead_width(&self.settings),
            self.rest_apps_width_at(now),
            self.live_widgets_section_width(),
            0.0,
        ))
    }

    fn pill_geometry_impl(&self, now: Instant, resting: bool) -> (f32, f32, f32, f32) {
        let mon_w = self.mon_w_dips();
        let (win_w, win_h) =
            compute_window_dips_with(&self.settings, self.slots.len(), mon_w, &self.effective);
        let max_w = (mon_w - 16.0).max(200.0);
        let pill_w = if self.settings.full_width {
            max_w
        } else if let Some(fixed_w) = self.effective.width_dips {
            fixed_w.min(win_w).min(max_w)
        } else {
            // Match `scroll_metrics_for_pill_at` exactly through `content_strip_width`
            // (+ both viewport insets) so a dock that fits on the monitor never
            // triggers false scroll overflow chevrons.
            let icons_w = if resting {
                self.rest_apps_width_at(now)
            } else {
                self.live_apps_width_dips_at(now)
            };
            let live_content = pill_width_for_content(content_strip_width(
                shell_lead_width(&self.settings),
                icons_w,
                self.live_widgets_section_width(),
                0.0,
            ));
            let floor_w = scroll_min_pill_width(&self.settings).min(live_content);
            let target = self
                .settings
                .dock_custom_width_dips
                .map_or(live_content, |custom| custom.max(floor_w));
            target.min(win_w).min(max_w)
        };
        let pill_h = self
            .effective
            .height_dips
            .unwrap_or_else(|| pill_height_dips(&self.settings));
        let mut pill_left = (win_w - pill_w) * 0.5;
        if !resting && self.fisheye_shift != 0.0 {
            // macOS fixed-point anchoring: the whole strip moves so the point under
            // the cursor stays put, while the pill stays inside the window.
            pill_left = (pill_left + self.fisheye_shift).clamp(0.0, (win_w - pill_w).max(0.0));
        }
        let pill_top = win_h - self.effective.bottom_inset_dips() - pill_h;
        (pill_left, pill_top, pill_w, pill_h)
    }

    fn live_apps_width_dips(&self) -> f32 {
        self.live_apps_width_dips_at(Instant::now())
    }

    fn live_apps_width_dips_at(&self, now: Instant) -> f32 {
        if self.slots.is_empty() {
            return 0.0;
        }
        let widths = self.slot_widths(now);
        self.apps_width_from(&widths, now)
    }

    /// Strip width for per-slot widths, honouring closing slots' layout fraction.
    fn apps_width_from(&self, widths: &[f32], now: Instant) -> f32 {
        let gap = slot_gap(&self.settings);
        let mut w = 0.0;
        let mut largest_fraction = 0.0_f32;
        for (slot, width) in self.slots.iter().zip(widths) {
            let fraction = slot.layout_fraction(now);
            w += (width + gap) * fraction;
            largest_fraction = largest_fraction.max(fraction);
        }
        (w - gap * largest_fraction).max(0.0)
    }

    /// Un-magnified strip width (every slot at `base`). Never depends on the
    /// expansion budget, so the budget can be derived from it without recursion.
    fn rest_apps_width_at(&self, now: Instant) -> f32 {
        if self.slots.is_empty() {
            return 0.0;
        }
        let base = layout::size_profile(&self.settings).cell;
        let widths = vec![base; self.slots.len()];
        self.apps_width_from(&widths, now)
    }

    /// Horizontal growth available to a hover arch (P0-1/P0-2).
    ///
    /// Fixed-width docks magnify in place (`0`). Expanding docks may grow up to
    /// the largest pill the window/monitor can hold, measured against the
    /// *resting* content, so a hover never overflows into scrolling and never
    /// clips the trailing Tray / Recycle Bin.
    fn fisheye_growth_available(&self, now: Instant) -> f32 {
        let fisheye = fisheye_enabled(&self.settings);
        let expands = magnification_expands_dock(&self.settings);
        if !fisheye || !expands || self.slots.is_empty() {
            return 0.0;
        }
        let mon_w = self.mon_w_dips();
        let (win_w, _) =
            compute_window_dips_with(&self.settings, self.slots.len(), mon_w, &self.effective);
        let max_w = (mon_w - 16.0).max(200.0);
        let rest_content = self.rest_content_width_at(now);
        let content_driven = !self.settings.full_width
            && self.effective.width_dips.is_none()
            && self.settings.dock_custom_width_dips.is_none();
        let max_extent = if content_driven {
            // The window keeps 12 DIP on each side for the pill's soft shadow.
            (win_w - 24.0).max(rest_content).min(max_w).min(win_w)
        } else {
            // A fixed pill never grows: only its internal slack can absorb the arch.
            self.rest_pill_geometry_at(now).2
        };
        layout::fisheye_growth_budget(expands, fisheye, max_extent, rest_content)
    }

    /// The hover session's expansion budget, computed once per frame.
    fn fisheye_budget_at(&self, now: Instant) -> layout::ExpansionBudget {
        let profile = layout::size_profile(&self.settings);
        let cell = profile.cell + slot_gap(&self.settings);
        capture_expansion_budget(
            cell,
            profile.icon,
            profile_max_scale(&self.settings),
            self.fisheye_growth_available(now),
            0.0,
            0.0,
            self.session_max_scale,
        )
    }

    /// `DockFisheyeLayout.ComputeSlotExtras`: each slot grows by at most 95% of
    /// its growth delta and the whole arch is clamped to the free budget. A
    /// fixed-width dock has a zero budget, so its slots never widen (P0-2).
    fn slot_widths(&self, now: Instant) -> Vec<f32> {
        let base = layout::size_profile(&self.settings).cell;
        let available = self.fisheye_growth_available(now);
        let bases = vec![base; self.slots.len()];
        if available <= 0.0 {
            return bases;
        }
        let scales: Vec<f32> = self.slots.iter().map(|slot| slot.scale).collect();
        let extras = compute_slot_extras(&scales, &bases, available);
        bases
            .into_iter()
            .zip(extras)
            .map(|(b, extra)| b + extra)
            .collect()
    }

    /// `ComputeSafeLift` + launch bounce, capped at 1 DIP by `MAX_LIFT_DIPS`.
    fn slot_lift(&self, slot: &DockSlot) -> f32 {
        let profile = layout::size_profile(&self.settings);
        let max_scale = profile_max_scale(&self.settings).min(self.session_max_scale);
        let headroom = (pill_height_dips(&self.settings) - profile.icon - 4.0).max(0.0);
        compute_safe_lift(slot.scale, max_scale, profile.icon, headroom)
            + bounce_offset_dips(slot.bounce_started).unwrap_or(0.0)
    }

    /// Same as `live_scroll_metrics`, but reuses pill geometry the caller has already computed.
    fn scroll_metrics_for_pill(&self, pill_left: f32, pill_w: f32) -> ScrollMetrics {
        self.scroll_metrics_for_pill_at(pill_left, pill_w, Instant::now())
    }

    fn scroll_metrics_for_pill_at(
        &self,
        pill_left: f32,
        pill_w: f32,
        now: Instant,
    ) -> ScrollMetrics {
        self.scroll_metrics_impl(pill_left, pill_w, now, false)
    }

    fn scroll_metrics_impl(
        &self,
        pill_left: f32,
        pill_w: f32,
        now: Instant,
        resting: bool,
    ) -> ScrollMetrics {
        // Flush with the outer rounded edges of the dock pill (matching `MainWindow.xaml`).
        let (vp_left, vp_right) = pill_viewport(pill_left, pill_w);
        let apps_w = if resting {
            self.rest_apps_width_at(now)
        } else {
            self.live_apps_width_dips_at(now)
        };
        let mut widgets_w = self.live_widgets_strip_width(apps_w > 0.0);
        let mut trail_w = self.live_trail_section_width();
        let group = layout::start_strip_group(&self.settings);
        let start_w = if is_shell_button_visible(&self.settings, ShellButton::Start) {
            layout::size_profile(&self.settings).cell + widget_gap(&self.settings)
        } else {
            0.0
        };
        match group {
            layout::StartStripGroup::InWidgets => widgets_w += start_w,
            layout::StartStripGroup::AfterApps => trail_w += start_w,
            layout::StartStripGroup::Head => {}
        }
        compute_scroll_metrics(
            vp_left,
            vp_right,
            apps_w + widgets_w,
            trail_w,
            shell_lead_width(&self.settings),
            self.scroll_offset,
            self.settings.content_alignment,
        )
    }

    fn live_scroll_metrics(&self) -> ScrollMetrics {
        let (pill_left, _, pill_w, _) = self.live_pill_geometry();
        self.scroll_metrics_for_pill(pill_left, pill_w)
    }

    /// Unmagnified layout (P0-3/P1-5): fisheye targets, hover index, keyboard
    /// focus and Start/preview anchors all read slot centers from here. The
    /// value frozen at the start of a hover session is reused while it still
    /// matches the current scroll offset, so rest centers cannot drift mid-hover.
    fn rest_scroll_metrics(&self) -> ScrollMetrics {
        if let Some(sm) = self.hover_rest_sm
            && (sm.offset - self.scroll_offset.clamp(0.0, sm.max_scroll)).abs() < 0.01
        {
            return sm;
        }
        self.compute_rest_scroll_metrics(Instant::now())
    }

    fn compute_rest_scroll_metrics(&self, now: Instant) -> ScrollMetrics {
        let (pill_left, _, pill_w, _) = self.rest_pill_geometry_at(now);
        self.scroll_metrics_impl(pill_left, pill_w, now, true)
    }

    /// Freezes the resting layout for the hover session that is starting.
    fn begin_hover_session(&mut self) {
        if self.hover_rest_sm.is_none() {
            self.hover_rest_sm = Some(self.compute_rest_scroll_metrics(Instant::now()));
        }
    }

    /// Slot centers in one O(N) pass. `resting` matches `resting_slot_center_x` (unscaled
    /// geometry used for hit-testing and fisheye targets); otherwise matches `live_slot_center_x`
    /// (current animated scales). Callers that iterate all slots must use this instead of calling
    /// the per-index helpers, which would rebuild scroll metrics and re-walk prefixes O(N²) times.
    fn slot_centers(&self, sm: &ScrollMetrics, resting: bool) -> Vec<f32> {
        let base = layout::size_profile(&self.settings).cell;
        let gap = slot_gap(&self.settings);
        let now = Instant::now();
        let widths = if resting {
            Vec::new()
        } else {
            self.slot_widths(now)
        };
        let mut cursor = sm.apps_start_x;
        let mut centers = Vec::with_capacity(self.slots.len());
        for (idx, slot) in self.slots.iter().enumerate() {
            let w = if resting { base } else { widths[idx] };
            centers.push(cursor + w * 0.5);
            cursor += (w + gap) * slot.layout_fraction(now);
        }
        centers
    }

    /// Returns `Some(false)` for left `<` scroll button, `Some(true)` for right `>` scroll button.
    fn hit_scroll_button(&self, mx: f32, my: f32) -> Option<bool> {
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        if my < pill_top || my > pill_top + pill_h {
            return None;
        }
        let sm = self.scroll_metrics_for_pill(pill_left, pill_w);
        if sm.can_scroll_left && mx >= sm.vp_left && mx <= sm.vp_left + SCROLL_EDGE_W_DIPS {
            return Some(false);
        }
        if sm.can_scroll_right && mx >= sm.vp_right - SCROLL_EDGE_W_DIPS && mx <= sm.vp_right {
            return Some(true);
        }
        None
    }

    fn scroll_by_step(&mut self, forward: bool) {
        let sm = self.live_scroll_metrics();
        if sm.max_scroll <= 0.5 {
            return;
        }
        let step = wheel_scroll_step(
            layout::size_profile(&self.settings).cell,
            slot_gap(&self.settings),
        );
        let base = if (self.scroll_offset - self.scroll_target).abs() > 1.0 {
            self.scroll_target
        } else {
            self.scroll_offset
        };
        let next = if forward { base + step } else { base - step };
        self.scroll_target = next.clamp(0.0, sm.max_scroll);
        self.ensure_animation();
    }

    fn hit_resize_edge(&self, mx: f32, my: f32) -> Option<ResizeEdge> {
        if self.settings.full_width {
            return None;
        }
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        if layout::is_vertical(effective_edge(&self.settings)) {
            if mx < pill_left || mx > pill_left + pill_w {
                return None;
            }
            if my >= pill_top - 2.0 && my <= pill_top + RESIZE_EDGE_DIPS {
                return Some(ResizeEdge::Top);
            }
            if my >= pill_top + pill_h - RESIZE_EDGE_DIPS && my <= pill_top + pill_h + 2.0 {
                return Some(ResizeEdge::Bottom);
            }
            return None;
        }
        if my < pill_top || my > pill_top + pill_h {
            return None;
        }
        if mx >= pill_left - 2.0 && mx <= pill_left + RESIZE_EDGE_DIPS {
            return Some(ResizeEdge::Left);
        }
        if mx >= pill_left + pill_w - RESIZE_EDGE_DIPS && mx <= pill_left + pill_w + 2.0 {
            return Some(ResizeEdge::Right);
        }
        None
    }

    fn live_slot_center_x(&self, idx: usize) -> f32 {
        let gap = slot_gap(&self.settings);
        let sm = self.live_scroll_metrics();
        let now = Instant::now();
        let widths = self.slot_widths(now);
        let mut cursor = sm.apps_start_x;
        for (i, &width) in widths.iter().enumerate().take(idx.min(self.slots.len())) {
            let slot = &self.slots[i];
            cursor += (width + gap) * slot.layout_fraction(now);
        }
        let w = widths
            .get(idx)
            .copied()
            .unwrap_or(layout::size_profile(&self.settings).cell);
        cursor + w * 0.5
    }

    /// Draw, hit-test and the Start anchor all resolve the same center. `End`
    /// places Start immediately before the trailing tray/bin group; `InWidgets`
    /// inserts it at `StartWidgetIndex` inside the widget strip.
    fn shell_center_x_in(&self, kind: ShellButton, sm: &ScrollMetrics) -> f32 {
        self.shell_center_x_with(kind, sm, self.live_apps_width_dips())
    }

    /// Same as `shell_center_x_in` for an explicit apps width (resting layouts
    /// pass the unmagnified width so `InWidgets` Start matches its rest slot).
    fn shell_center_x_with(&self, kind: ShellButton, sm: &ScrollMetrics, apps_w: f32) -> f32 {
        let base = layout::size_profile(&self.settings).cell;
        if !is_shell_button_visible(&self.settings, kind) {
            return sm.shell_start_x + base * 0.5;
        }
        let group = layout::start_strip_group(&self.settings);
        if kind == ShellButton::Start && group != layout::StartStripGroup::Head {
            match group {
                layout::StartStripGroup::AfterApps => {
                    sm.right_anchor_x - self.live_trail_section_width() - base * 0.5 - 2.0
                }
                layout::StartStripGroup::InWidgets => {
                    let mut wx = sm.apps_start_x + if apps_w > 0.0 { apps_w + 8.0 } else { 0.0 };
                    let index = layout::resolved_start_widget_index(
                        &self.settings,
                        self.widget_visuals.len(),
                    );
                    for wv in self.widget_visuals.iter().take(index) {
                        wx += wv.width_dips + widget_gap(&self.settings);
                    }
                    wx + base * 0.5
                }
                layout::StartStripGroup::Head => sm.shell_start_x + base * 0.5,
            }
        } else {
            let slot = ShellButton::ALL
                .iter()
                .take_while(|&&k| k != kind)
                .filter(|&&k| is_lead_shell_button(&self.settings, k))
                .count();
            sm.shell_start_x + (slot as f32) * (base + shell_gap(&self.settings)) + base * 0.5
        }
    }

    fn live_shell_center_x(&self, kind: ShellButton) -> f32 {
        let sm = self.live_scroll_metrics();
        self.shell_center_x_in(kind, &sm)
    }

    fn right_widget_rects_dips(
        &self,
    ) -> (
        Option<DipRect>,
        Option<DipRect>,
        Vec<(&'static str, DipRect)>,
    ) {
        let (_, pill_top, _, pill_h) = self.live_pill_geometry();
        let sm = self.live_scroll_metrics();
        // Widgets live in the same main strip right after the apps (matching `RebuildDockOrder`),
        // while Background Apps (`tray`) and Recycle Bin (`bin`) stay pinned to the trailing edge.
        let apps_w = self.live_apps_width_dips();
        let mut wx = sm.apps_start_x + if apps_w > 0.0 { apps_w + 8.0 } else { 0.0 };
        let start_in_widgets = layout::start_strip_group(&self.settings)
            == layout::StartStripGroup::InWidgets
            && is_shell_button_visible(&self.settings, ShellButton::Start);
        let start_index =
            layout::resolved_start_widget_index(&self.settings, self.widget_visuals.len());
        let gap = widget_gap(&self.settings);
        let mut widget_rects = Vec::with_capacity(self.widget_visuals.len());
        for (index, wv) in self.widget_visuals.iter().enumerate() {
            if start_in_widgets && index == start_index {
                wx += layout::size_profile(&self.settings).cell + gap;
            }
            let l = wx;
            let r = wx + wv.width_dips;
            widget_rects.push((wv.id, (l, pill_top + 6.0, r, pill_top + pill_h - 6.0)));
            wx = r + gap;
        }
        let _ = wx;
        let mut right_cursor = sm.right_anchor_x;
        let bin = if self.settings.show_recycle_bin {
            let bw = layout::size_profile(&self.settings).cell;
            let l = right_cursor - bw;
            let r = right_cursor;
            right_cursor -= bw + gap;
            Some((l, pill_top + 6.0, r, pill_top + pill_h - 6.0))
        } else {
            None
        };
        let tray = if self.settings.show_tray_button {
            let tw = layout::size_profile(&self.settings).cell;
            let l = right_cursor - tw;
            let r = right_cursor;
            Some((l, pill_top + 6.0, r, pill_top + pill_h - 6.0))
        } else {
            None
        };
        (tray, bin, widget_rects)
    }

    /// Folder-stack cells, left to right in `stack_visuals` order (`0..len`).
    /// Trailing layout: `| [Folder...] [PinnedTray...] [Tray] [Bin]`.
    fn stack_rects_dips(&self) -> Vec<DipRect> {
        let count = self.stack_visuals.len();
        if count == 0 {
            return Vec::new();
        }
        let (_, pill_top, _, pill_h) = self.live_pill_geometry();
        let sm = self.live_scroll_metrics();
        self.stack_rects_in(&sm, pill_top, pill_h)
    }

    fn stack_rects_in(&self, sm: &ScrollMetrics, pill_top: f32, pill_h: f32) -> Vec<DipRect> {
        let count = self.stack_visuals.len();
        if count == 0 {
            return Vec::new();
        }
        let base = layout::size_profile(&self.settings).cell;
        let gap = widget_gap(&self.settings);
        let mut right_cursor = sm.right_anchor_x;
        if self.settings.show_recycle_bin {
            right_cursor -= base + gap;
        }
        if self.settings.show_tray_button {
            right_cursor -= base + gap;
            let pins = self.tray_pin_visuals.len();
            if pins > 0 {
                right_cursor -= tray_pin_cell(base) * pins as f32 + gap;
            }
        }
        let mut rects = vec![(0.0, 0.0, 0.0, 0.0); count];
        for index in (0..count).rev() {
            rects[index] = (
                right_cursor - base,
                pill_top + 6.0,
                right_cursor,
                pill_top + pill_h - 6.0,
            );
            right_cursor -= base + gap;
        }
        rects
    }

    fn hit_stack(&self, mx: f32, my: f32) -> Option<usize> {
        if self.stack_visuals.is_empty() {
            return None;
        }
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        if my < pill_top || my > pill_top + pill_h {
            return None;
        }
        let sm = self.scroll_metrics_for_pill(pill_left, pill_w);
        if mx < sm.vp_left || mx > sm.vp_right {
            return None;
        }
        self.stack_rects_in(&sm, pill_top, pill_h)
            .iter()
            .position(|&(l, _, r, _)| mx >= l && mx <= r)
    }

    /// DIP rect of a shell button, resolved from the *resting* layout so the
    /// Start/Search anchors (and their flyouts) never follow a magnified,
    /// shifted strip (P1-7). `None` means the button is hidden.
    fn shell_button_anchor_dips(&self, kind: ShellButton) -> Option<DipRect> {
        if !is_shell_button_visible(&self.settings, kind) {
            return None;
        }
        let base = layout::size_profile(&self.settings).cell;
        let now = Instant::now();
        let (pill_left, pill_top, pill_w, pill_h) = self.rest_pill_geometry_at(now);
        let rest_sm = self.rest_scroll_metrics();
        let center_x = self
            .shell_center_x_with(kind, &rest_sm, self.rest_apps_width_at(now))
            .clamp(
                pill_left + 1.0,
                (pill_left + pill_w - 1.0).max(pill_left + 1.0),
            );
        Some(centered_shell_rect(
            center_x,
            base,
            pill_top + 4.0,
            pill_top + pill_h - 4.0,
        ))
    }

    /// Publishes the screen rects of the dock's Start, Search and tray/notification
    /// buttons. Start feeds the `on_opened` hook; Search and Control Center are
    /// resolved from the same registry when their flyouts open. All three use the
    /// live shell-button layout, so a flyout lands above the button that was clicked.
    fn refresh_shell_button_anchors(&self) {
        let mut win_rc: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            GetWindowRect(self.hwnd as HWND, &mut win_rc);
        }
        let window = (win_rc.left, win_rc.top, win_rc.right, win_rc.bottom);

        for (shell, kind) in [
            (ShellButton::Start, FlyoutKind::Start),
            (ShellButton::Search, FlyoutKind::Search),
        ] {
            let anchor = self
                .shell_button_anchor_dips(shell)
                .map(|button| anchor_from_window_rect(window, button, self.dpi))
                .unwrap_or((0, 0, 0, 0));
            publish_shell_button_anchor(self.hwnd, kind, anchor);
        }

        // Control Center follows the tray/notification button at the trailing
        // edge of the pill (the same rect used for tray hit testing).
        let (tray, _, _) = self.right_widget_rects_dips();
        let anchor = tray
            .map(|rc| anchor_from_window_rect(window, rc, self.dpi))
            .unwrap_or((0, 0, 0, 0));
        publish_shell_button_anchor(self.hwnd, FlyoutKind::ControlCenter, anchor);
    }

    /// Non-blocking flyout trigger: open paths only post a message, leaving the
    /// anchor lookup and worker spawn to the UI thread's normal message pump.
    fn request_flyout_anchor(&self, kind: FlyoutKind) {
        unsafe {
            let _ = PostMessageW(self.hwnd as HWND, WM_APP_SHELL_FLYOUT, flyout_code(kind), 0);
        }
    }

    /// Visible dock pill in physical screen pixels: excludes the transparent
    /// magnification/tooltip headroom and follows the auto-hide slide, so menus
    /// and flyouts attach to what the user actually sees.
    fn visible_pill_screen_rect(&self) -> Option<windows::Win32::Foundation::RECT> {
        use opentaskbar_core::settings::DockEdge;
        let mut bounds = RECT::default();
        if unsafe { GetWindowRect(self.hwnd as HWND, &mut bounds) } == 0
            || bounds.right <= bounds.left
            || bounds.bottom <= bounds.top
        {
            return None;
        }
        let px = |v: f32| (v * self.dpi).round() as i32;
        let (x, y, w, h) = self.live_pill_geometry();
        let edge = effective_edge(&self.settings);
        let slide = px(self.hide_progress * hide_offset(edge, h + VERTICAL_EDGE_GAP + 10.0));
        let headroom = if magnification_expands_dock(&self.settings) {
            0
        } else {
            px(layout::HEADROOM_DIPS)
        };
        let (left, top, right, bottom) = match edge {
            DockEdge::Left => (
                bounds.left + slide.min(0),
                bounds.top,
                (bounds.right - headroom + slide).max(bounds.left + 1),
                bounds.bottom,
            ),
            DockEdge::Right => (
                (bounds.left + headroom + slide).min(bounds.right - 1),
                bounds.top,
                bounds.right + slide.max(0),
                bounds.bottom,
            ),
            DockEdge::Bottom => (
                bounds.left + px(x),
                (bounds.top + px(y) + slide).min(bounds.bottom - 1),
                bounds.left + px(x + w),
                (bounds.top + px(y + h) + slide).min(bounds.bottom),
            ),
        };
        Some(windows::Win32::Foundation::RECT {
            left,
            top,
            right: right.max(left + 1),
            bottom: bottom.max(top + 1),
        })
    }

    /// Popup-menu anchor for every dock context menu: the visible pill plus the
    /// dock edge, so menus open beside the dock instead of at the raw cursor.
    fn dock_menu_anchor(&self) -> Option<menu::MenuAnchor> {
        use opentaskbar_core::settings::DockEdge;
        let edge = match effective_edge(&self.settings) {
            DockEdge::Bottom => menu::MenuEdge::Bottom,
            DockEdge::Left => menu::MenuEdge::Left,
            DockEdge::Right => menu::MenuEdge::Right,
        };
        Some(menu::MenuAnchor {
            visible_rect: self.visible_pill_screen_rect()?,
            edge: Some(edge),
        })
    }

    /// Which way dock flyouts open (away from the dock's screen edge).
    fn dock_flyout_edge(&self) -> FlyoutEdge {
        use opentaskbar_core::settings::DockEdge;
        match effective_edge(&self.settings) {
            DockEdge::Bottom => FlyoutEdge::Bottom,
            DockEdge::Left => FlyoutEdge::Left,
            DockEdge::Right => FlyoutEdge::Right,
        }
    }

    /// Screen anchor for a flyout opened from the dock item `rect` (DIPs).
    /// Bottom docks anchor above the visible pill, centered on the item; side
    /// docks anchor beside the pill.
    fn flyout_anchor_px(&self, rect: DipRect) -> (i32, i32) {
        let gap = (8.0 * self.dpi).round() as i32;
        let Some(pill) = self.visible_pill_screen_rect() else {
            let (_, pill_top, _, _) = self.live_pill_geometry();
            let sm = self.live_scroll_metrics();
            let (win_x, win_y, _, _) = self.dock_screen_rect();
            let cx_dips = ((rect.0 + rect.2) * 0.5).clamp(sm.vp_left, sm.vp_right);
            return (
                win_x + (cx_dips * self.dpi).round() as i32,
                win_y + ((pill_top - 8.0) * self.dpi).round() as i32,
            );
        };
        match self.dock_flyout_edge() {
            FlyoutEdge::Left => (pill.right + gap, (pill.top + pill.bottom) / 2),
            FlyoutEdge::Right => (pill.left - gap, (pill.top + pill.bottom) / 2),
            FlyoutEdge::Bottom | FlyoutEdge::Top => {
                let sm = self.live_scroll_metrics();
                let (pill_left, _, _, _) = self.live_pill_geometry();
                let center = ((rect.0 + rect.2) * 0.5).clamp(sm.vp_left, sm.vp_right);
                (
                    pill.left + ((center - pill_left) * self.dpi).round() as i32,
                    pill.top - gap,
                )
            }
        }
    }

    /// Flyout anchor at the middle of the visible pill (group flyouts).
    fn flyout_anchor_center_px(&self) -> (i32, i32) {
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        self.flyout_anchor_px((pill_left, pill_top, pill_left + pill_w, pill_top + pill_h))
    }

    fn hit_rect(rc: Option<(f32, f32, f32, f32)>, mx: f32, my: f32) -> bool {
        rc.is_some_and(|(l, t, r, b)| mx >= l && mx <= r && my >= t && my <= b)
    }

    fn hit_rect_in_vp(
        sm: &ScrollMetrics,
        rc: Option<(f32, f32, f32, f32)>,
        mx: f32,
        my: f32,
    ) -> bool {
        mx >= sm.vp_left && mx <= sm.vp_right && Self::hit_rect(rc, mx, my)
    }

    fn hit_tray_pin(&self, tray: Option<DipRect>, mx: f32, my: f32) -> Option<usize> {
        let sm = self.live_scroll_metrics();
        if mx < sm.vp_left || mx > sm.vp_right {
            return None;
        }
        let tray = tray?;
        let cell = tray_pin_cell(layout::size_profile(&self.settings).cell);
        let count = self.tray_pin_visuals.len();
        (0..count).find(|&index| {
            Self::hit_rect(
                Some(tray_pin_rect(
                    tray,
                    cell,
                    widget_gap(&self.settings),
                    count,
                    index,
                )),
                mx,
                my,
            )
        })
    }

    fn hit_test_dips(&self, mx: f32, my: f32) -> bool {
        if self.hide_progress > 0.5 {
            // Keep a 5 DIP edge trigger strip active so touching the screen edge reveals the dock.
            let mut pt = POINT { x: 0, y: 0 };
            return unsafe { GetCursorPos(&mut pt) != 0 }
                && layout::is_at_edge_trigger(
                    effective_edge(&self.settings),
                    self.monitor.clone(),
                    pt.x,
                    pt.y,
                    self.dpi,
                );
        }
        if self.drag.is_some() {
            return true;
        }
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        if mx >= pill_left && mx <= pill_left + pill_w && my >= pill_top && my <= pill_top + pill_h
        {
            return true;
        }
        let sm = self.scroll_metrics_for_pill(pill_left, pill_w);
        if mx < sm.vp_left || mx > sm.vp_right {
            return false;
        }
        // Fitts's law: the strip between the pill and the screen edge (bottom
        // inset) still belongs to the app icons above it, so slamming the
        // pointer to the edge keeps the app under it hoverable and clickable.
        let apps_w = self.live_apps_width_dips();
        if !layout::is_vertical(effective_edge(&self.settings))
            && apps_w > 0.0
            && my > pill_top + pill_h
            && mx >= sm.apps_start_x
            && mx <= sm.apps_start_x + apps_w
        {
            return true;
        }
        let icon_size = layout::size_profile(&self.settings).icon;
        let (_, icon_base_bottom, _) = app_icon_vertical_metrics(pill_top, pill_h, icon_size);
        let centers = self.slot_centers(&sm, false);
        for (idx, slot) in self.slots.iter().enumerate() {
            if slot.is_exiting() {
                continue;
            }
            let dim = icon_size * slot.scale;
            let cx = centers[idx] + slot.slide_x;
            let lift = self.slot_lift(slot);
            let top = icon_base_bottom - lift - dim;
            if mx >= cx - dim * 0.5 && mx <= cx + dim * 0.5 && my >= top && my <= icon_base_bottom {
                return true;
            }
        }
        false
    }

    fn hit_shell_button(&self, mx: f32, my: f32) -> Option<ShellButton> {
        let base = layout::size_profile(&self.settings).cell;
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        let sm = self.scroll_metrics_for_pill(pill_left, pill_w);
        if my < pill_top || my > pill_top + pill_h {
            return None;
        }
        for kind in ShellButton::ALL {
            if !is_shell_button_visible(&self.settings, kind) {
                continue;
            }
            let center = self.shell_center_x_in(kind, &sm);
            let left = center - base * 0.5;
            if mx >= sm.vp_left.max(left) && mx <= sm.vp_right.min(left + base) {
                return Some(kind);
            }
        }
        None
    }

    fn update_hovered_index_from_mouse(&mut self) {
        self.hovered_idx = self.hover_mouse_x.and_then(|mx| self.slot_index_at(mx));
    }

    /// App slot whose *resting* cell contains `mx`. Rest space equals pointer
    /// space under fixed-point anchoring (`f(p) == p`), so this is the icon that
    /// is visually under the cursor in both fisheye modes.
    fn slot_index_at(&self, mx: f32) -> Option<usize> {
        let sm = self.rest_scroll_metrics();
        if mx < sm.vp_left || mx > sm.vp_right {
            return None;
        }
        let base = layout::size_profile(&self.settings).cell;
        let gap = slot_gap(&self.settings);
        let max_dist = (base + gap) * 0.55;
        let centers = self.slot_centers(&sm, true);
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !slot.is_exiting())
            .map(|(i, _)| {
                let cx = centers[i];
                (i, cx, (mx - cx).abs())
            })
            .filter(|(_, cx, _)| *cx >= sm.vp_left - base * 0.4 && *cx <= sm.vp_right + base * 0.4)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .filter(|(_, _, d)| *d <= max_dist)
            .map(|(i, _, _)| i)
    }

    /// Resting (unmagnified, unshifted) center of slot `idx` (P1-4 keyboard
    /// focus, P2 preview anchor).
    fn rest_slot_center_x(&self, idx: usize) -> f32 {
        let sm = self.rest_scroll_metrics();
        self.slot_centers(&sm, true)
            .get(idx)
            .copied()
            .unwrap_or(sm.apps_start_x)
    }

    /// Raised-cosine target for a resting center. `budget` is computed once per
    /// frame (not per slot). Fixed-width docks have no budget, so the peak is the
    /// no-overlap scale (`compute_no_overlap_max_scale`).
    fn target_scale_with(&self, center_x: f32, budget: &layout::ExpansionBudget) -> f32 {
        if self.drag.is_some_and(|d| d.active) {
            return 1.0;
        }
        if !fisheye_enabled(&self.settings) || self.hover_mouse_x.is_none() {
            return 1.0;
        }
        fisheye_target_scale(
            &self.settings,
            self.hover_mouse_x,
            center_x,
            budget.effective_max_scale,
            budget.fraction,
        )
    }

    /// Updates the macOS fixed-point shift so the resting point under the cursor
    /// maps onto itself (`f(p) == p`) and growth splits to both sides.
    fn update_fisheye_shift(&mut self, rest_sm: &ScrollMetrics) {
        let expands = magnification_expands_dock(&self.settings);
        let anchor = self.hover_mouse_x.or(self.fisheye_anchor_x);
        let Some(pointer) = anchor.filter(|_| expands && fisheye_enabled(&self.settings)) else {
            self.fisheye_shift = 0.0;
            return;
        };
        let rest_centers = self.slot_centers(rest_sm, true);
        let mon_w = self.mon_w_dips();
        let (win_w, _) =
            compute_window_dips_with(&self.settings, self.slots.len(), mon_w, &self.effective);
        let (pill_left, _, pill_w, _) = self.live_pill_geometry();
        let live_sm = self.scroll_metrics_for_pill(pill_left, pill_w);
        // The shift actually applied this frame (after the in-window clamp).
        let unshifted_left = (win_w - pill_w) * 0.5;
        let applied = pill_left - unshifted_left;
        let live_centers: Vec<f32> = self
            .slot_centers(&live_sm, false)
            .into_iter()
            .map(|center| center - applied)
            .collect();
        let target = layout::fisheye_fixed_point_shift(&rest_centers, &live_centers, pointer);
        // Keep the shifted pill inside the window (the reserve covers one full arch).
        let min_shift = -unshifted_left;
        let max_shift = (win_w - pill_w - unshifted_left).max(min_shift);
        self.fisheye_shift = target.clamp(min_shift, max_shift);
        if self.fisheye_shift.abs() < 0.05 {
            self.fisheye_shift = 0.0;
        }
    }

    fn step_animations(&mut self, now: Instant) -> bool {
        // With animations disabled there must be no spring/fisheye/bounce math on
        // the VSync path, and no exiting slots waiting for a close animation that
        // will never be ticked. Snap to the final state in one pass.
        if !self.animations_effective() {
            let previous_count = self.slots.len();
            self.slots.retain(|slot| !slot.is_exiting());
            if self.slots.len() != previous_count {
                self.rebuild_visual_tree();
                self.sync_window_bounds();
                self.update_hovered_index_from_mouse();
                self.sync_preview_hover();
                self.request_missing_icons();
            }
            for slot in &mut self.slots {
                slot.close_started = None;
                slot.scale = 1.0;
                slot.scale_velocity = 0.0;
                slot.slide_x = 0.0;
                slot.bounce_started = None;
            }
            self.shell_scales = [1.0; 3];
            self.shell_velocities = [0.0; 3];
            for sv in &mut self.stack_visuals {
                sv.scale = 1.0;
                sv.scale_velocity = 0.0;
            }
            self.fisheye_shift = 0.0;
            self.fisheye_anchor_x = None;
            self.hide_progress = if self.target_hidden { 1.0 } else { 0.0 };
            self.scroll_offset = self
                .scroll_target
                .clamp(0.0, self.live_scroll_metrics().max_scroll);
            self.last_frame = None;
            return false;
        }
        if self.drag.is_none() {
            let previous_count = self.slots.len();
            self.slots
                .retain(|slot| !slot.is_exiting() || close_progress(slot.close_started, now) < 1.0);
            if self.slots.len() != previous_count {
                self.rebuild_visual_tree();
                self.sync_window_bounds();
                self.update_hovered_index_from_mouse();
                self.sync_preview_hover();
                self.request_missing_icons();
            }
        }
        let dt = self
            .last_frame
            .map_or(1.0 / 60.0, |prev| now.duration_since(prev).as_secs_f32())
            .clamp(0.001, 0.05);
        self.last_frame = Some(now);

        let slide_k = 1.0 - (-24.0 * dt).exp();
        let hide_k = 1.0 - (-16.0 * dt).exp();
        // Continuous inertial scrolling matching reference DockScrollViewer (`tau = 0.045s`).
        let scroll_k = 1.0 - (-dt / 0.045).exp();
        let mut moving = self.drag.is_some_and(|d| d.active);

        let scroll_metrics = self.live_scroll_metrics();
        let max_scroll = scroll_metrics.max_scroll;
        self.scroll_target = self.scroll_target.clamp(0.0, max_scroll);
        if self.scroll_pan.is_none() {
            let sdiff = self.scroll_target - self.scroll_offset;
            if sdiff.abs() > 0.3 {
                self.scroll_offset = (self.scroll_offset + sdiff * scroll_k).clamp(0.0, max_scroll);
                moving = true;
            } else if (self.scroll_offset - self.scroll_target).abs() > 0.0 {
                self.scroll_offset = self.scroll_target;
            }
        } else {
            self.scroll_offset = self.scroll_offset.clamp(0.0, max_scroll);
        }

        let hide_target = if self.target_hidden { 1.0 } else { 0.0 };
        let hide_diff = hide_target - self.hide_progress;
        if hide_diff.abs() > 0.004 {
            self.hide_progress += hide_diff * hide_k;
            moving = true;
        } else if (self.hide_progress - hide_target).abs() > 0.0 {
            self.hide_progress = hide_target;
        }

        // Fisheye targets come from the *resting* layout (frozen per hover
        // session) and the budget is computed once per frame: O(N), and the
        // arch cannot feed back into its own targets.
        let rest_sm = self.rest_scroll_metrics();
        let budget = self.fisheye_budget_at(now);
        let resting_centers = self.slot_centers(&rest_sm, true);
        let scale_targets: Vec<f32> = resting_centers
            .iter()
            .map(|&center| self.target_scale_with(center, &budget))
            .collect();
        // Crisp critically damped tracking while hovering, softer decay on exit.
        let (stiffness, damping) = layout::fisheye_spring_params(self.hover_mouse_x.is_some());
        let max_scale = profile_max_scale(&self.settings).min(self.session_max_scale);
        for (idx, slot) in self.slots.iter_mut().enumerate() {
            if slot.close_started.is_some() {
                if slot.is_exiting() || close_progress(slot.close_started, now) < 1.0 {
                    moving = true;
                } else {
                    slot.close_started = None;
                }
            }
            if slot.is_exiting() {
                continue;
            }
            let target = scale_targets[idx];
            let (scale, velocity, settled) = layout::step_spring_with(
                slot.scale,
                target,
                slot.scale_velocity,
                dt,
                max_scale,
                stiffness,
                damping,
            );
            slot.scale = scale;
            slot.scale_velocity = velocity;
            if !settled {
                moving = true;
            }

            let sx = slot.slide_x;
            if sx.abs() > 0.35 {
                slot.slide_x = sx * (1.0 - slide_k);
                moving = true;
            } else if sx != 0.0 {
                slot.slide_x = 0.0;
            }

            if let Some(started) = slot.bounce_started {
                if bounce_offset_dips(Some(started)).is_some() {
                    moving = true;
                } else {
                    slot.bounce_started = None;
                }
            }
        }

        // Shell buttons get the very same raised-cosine target + spring as app
        // slots, evaluated on the resting layout. A Start button outside the lead
        // group (`AfterApps` / `InWidgets`) sits in a non-magnified zone (P1-6).
        let rest_apps_w = self.rest_apps_width_at(now);
        let shell_targets = ShellButton::ALL.map(|kind| {
            if kind == ShellButton::Start
                && layout::start_strip_group(&self.settings) != layout::StartStripGroup::Head
            {
                return 1.0;
            }
            let center = self.shell_center_x_with(kind, &rest_sm, rest_apps_w);
            self.target_scale_with(center, &budget)
        });
        moving |= step_shell_spring(
            &mut self.shell_scales,
            &mut self.shell_velocities,
            shell_targets,
            dt,
            max_scale,
        );

        // Folder stacks magnify in place (fixed cells, no-overlap peak).
        if !self.stack_visuals.is_empty() {
            let profile = layout::size_profile(&self.settings);
            let stack_peak = layout::compute_no_overlap_max_scale(
                profile.cell + widget_gap(&self.settings),
                profile.icon,
                profile_max_scale(&self.settings),
            );
            let centers: Vec<f32> = self
                .stack_rects_dips()
                .iter()
                .map(|&(l, _, r, _)| (l + r) * 0.5)
                .collect();
            let hover_x = if self.drag.is_some_and(|d| d.active) {
                None
            } else {
                self.stack_hover_x
            };
            let (stack_k, stack_c) = layout::fisheye_spring_params(hover_x.is_some());
            for (sv, center) in self.stack_visuals.iter_mut().zip(centers) {
                let target = fisheye_target_scale(&self.settings, hover_x, center, stack_peak, 0.0);
                let (scale, velocity, settled) = layout::step_spring_with(
                    sv.scale,
                    target,
                    sv.scale_velocity,
                    dt,
                    stack_peak.max(1.0),
                    stack_k,
                    stack_c,
                );
                sv.scale = scale;
                sv.scale_velocity = velocity;
                moving |= !settled;
            }
        }

        // macOS fixed-point anchoring for expanding docks: the strip shifts so the
        // hovered point stays under the cursor and growth splits left and right.
        self.update_fisheye_shift(&rest_sm);
        let arch_active = self.slots.iter().any(|s| (s.scale - 1.0).abs() > 0.004);
        if self.hover_mouse_x.is_none() && !arch_active {
            // The arch has fully decayed: end the hover session.
            self.fisheye_anchor_x = None;
            self.fisheye_shift = 0.0;
            self.hover_rest_sm = None;
        }

        if !moving {
            self.last_frame = None;
        }
        moving
    }

    fn update_drag(&mut self, mx: f32) {
        let Some(mut drag) = self.drag else {
            return;
        };
        drag.current_x = mx;
        if !drag.active && (mx - drag.start_x).abs() >= DRAG_THRESHOLD_PX {
            drag.active = true;
            self.hover_mouse_x = None;
            self.hovered_idx = None;
            self.hovered_shell = None;
            // `MainWindow.DragDrop`: only an external file drag hovering a
            // running app for 450 ms brings that app forward.
            if layout::drag_hover_allowed(external_file_drag_active()) {
                self.drag_hover_started = Some(Instant::now());
                unsafe {
                    SetTimer(
                        self.hwnd as HWND,
                        TIMER_DRAG_HOVER,
                        layout::DRAG_HOVER_MS as u32,
                        None,
                    );
                }
            }
        }
        if drag.active && !self.slots.is_empty() {
            let center_x = mx - drag.grab_offset_x;
            let sm = self.live_scroll_metrics();
            let centers = self.slot_centers(&sm, true);
            let cell_w = layout::size_profile(&self.settings).cell;
            let spans: Vec<(f32, f32)> = centers
                .iter()
                .map(|&center| (center - cell_w * 0.5, cell_w))
                .collect();
            // Hysteresis stops the insertion caret flickering between neighbours.
            let target = insertion_index(&spans, center_x, drag.idx as isize, DRAG_THRESHOLD_PX)
                .min(self.slots.len().saturating_sub(1));
            if target != drag.idx && target < self.slots.len() && drag.idx < self.slots.len() {
                let step = layout::size_profile(&self.settings).cell + slot_gap(&self.settings);
                let old_idx = drag.idx;
                self.reorder_group_members(old_idx, target);
                let item = self.slots.remove(old_idx);
                self.slots.insert(target, item);
                if target > old_idx {
                    for slot in &mut self.slots[old_idx..target] {
                        slot.slide_x += step;
                    }
                } else {
                    for slot in &mut self.slots[(target + 1)..=old_idx] {
                        slot.slide_x -= step;
                    }
                }
                drag.idx = target;
                self.pressed_idx = Some(target);
            }
        }
        self.drag = Some(drag);
    }

    /// Keeps a group's persisted member order in sync when a drag reorders two
    /// slots that belong to the same app group (`MainWindow.Groups`).
    fn reorder_group_members(&mut self, from_idx: usize, to_idx: usize) {
        let Some((from_path, to_path)) = self
            .slots
            .get(from_idx)
            .zip(self.slots.get(to_idx))
            .map(|(a, b)| (a.launch_path.clone(), b.launch_path.clone()))
        else {
            return;
        };
        let Some(group_id) = self
            .settings
            .app_groups
            .iter()
            .find(|group| {
                group
                    .executable_paths
                    .iter()
                    .any(|path| path.eq_ignore_ascii_case(&from_path))
            })
            .map(|group| group.id.clone())
        else {
            return;
        };
        let Some(group) = self
            .settings
            .app_groups
            .iter_mut()
            .find(|group| group.id == group_id)
        else {
            return;
        };
        if !group
            .executable_paths
            .iter()
            .any(|path| path.eq_ignore_ascii_case(&to_path))
        {
            return;
        }
        let from = group
            .executable_paths
            .iter()
            .position(|path| path.eq_ignore_ascii_case(&from_path));
        let to = group
            .executable_paths
            .iter()
            .position(|path| path.eq_ignore_ascii_case(&to_path));
        if let (Some(from), Some(to)) = (from, to) {
            group.executable_paths = group_reorder(&group.executable_paths, from, to);
        }
    }

    fn persist_pin_order(&mut self) {
        let order: Vec<String> = self
            .slots
            .iter()
            .filter(|s| s.pinned)
            .map(|s| s.launch_path.clone())
            .collect();
        self.settings.pin_order = Some(order);
        let _ = opentaskbar_core::settings::save_to(
            &opentaskbar_core::settings::settings_path(),
            &self.settings,
        );
    }

    fn finish_drag(&mut self) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        if drag.active {
            self.persist_pin_order();
        }
        self.drag_hover_started = None;
        unsafe {
            KillTimer(self.hwnd as HWND, TIMER_DRAG_HOVER);
        }
        drag.active
    }

    fn fallback_surface_for(&mut self, label: &str) -> Option<IDCompositionSurface> {
        let ch = label
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .to_string();
        if let Some(existing) = self.fallback_surfaces.get(&ch) {
            return Some(existing.clone());
        }
        let created = self
            .dcomp
            .create_fallback_tile_surface(&ch, icon_cache::ICON_PX)
            .ok()?;
        self.fallback_surfaces.insert(ch, created.clone());
        Some(created)
    }

    fn update_tray_pin_visuals(&mut self, tray: DipRect, icon_top: f32) {
        let base = layout::size_profile(&self.settings).cell;
        let cell = tray_pin_cell(base);
        let dpi = self.dpi;
        let count = self.tray_pin_visuals.len();
        for index in 0..count {
            let path = &self.settings.user_pinned_background_apps[index];
            let has_icon = self.icon_surfaces.get(path).is_some_and(Option::is_some);
            if self.tray_pin_visuals[index].source != *path
                || self.tray_pin_visuals[index].has_icon != has_icon
            {
                let path = path.clone();
                let surface = self.icon_surfaces.get(&path).and_then(Clone::clone);
                let label = tray::friendly_name(&path);
                let surface = surface.or_else(|| self.fallback_surface_for(&label));
                let visual = &mut self.tray_pin_visuals[index];
                visual.icon.set_content(surface.as_ref());
                visual.source = path;
                visual.has_icon = has_icon;
                visual.label = label;
            }
            let path = &self.settings.user_pinned_background_apps[index];
            let pressed = self.pressed_tray_pin.as_deref() == Some(path);
            let visual = &self.tray_pin_visuals[index];
            let (left, _, right, _) =
                tray_pin_rect(tray, cell, widget_gap(&self.settings), count, index);
            let center = (left + right) * 0.5;
            if pressed || self.hovered_tray_pin == Some(index) {
                visual.bg.set_content(Some(if pressed {
                    &self.shell_bg_press
                } else {
                    &self.shell_bg_hover
                }));
                visual.bg.set_transform(
                    (cell - 4.0) / base,
                    0.78,
                    (left + 2.0) * dpi,
                    (icon_top + base * 0.11) * dpi,
                );
            } else {
                visual.bg.hide();
            }
            let dim = base * if pressed { 0.56 } else { 0.64 };
            let scale = dim * dpi / icon_cache::ICON_PX as f32;
            visual.icon.set_transform(
                scale,
                scale,
                (center - dim * 0.5) * dpi,
                (icon_top + (base - dim) * 0.5) * dpi,
            );
        }
    }

    /// Draws every folder stack: hover/press backdrop inside the pill and the
    /// native folder icon scaled from the bottom baseline (`ICON_PX` surfaces).
    fn update_stack_visuals(
        &mut self,
        rects: &[DipRect],
        shell_top: f32,
        icon_base_bottom: f32,
        icon_size: f32,
    ) {
        let dpi = self.dpi;
        let icon_px = icon_cache::ICON_PX as f32;
        for index in 0..self.stack_visuals.len().min(rects.len()) {
            let path = self.stack_visuals[index].path.clone();
            let has_icon = self.icon_surfaces.get(&path).is_some_and(Option::is_some);
            if self.stack_visuals[index].bound_source != path
                || self.stack_visuals[index].bound_has_icon != has_icon
            {
                let label = self.stack_visuals[index].label.clone();
                let surface = self
                    .icon_surfaces
                    .get(&path)
                    .and_then(Clone::clone)
                    .or_else(|| self.folder_fallback_surf.clone())
                    .or_else(|| self.fallback_surface_for(&label));
                let visual = &mut self.stack_visuals[index];
                visual.icon.set_content(surface.as_ref());
                visual.bound_source = path;
                visual.bound_has_icon = has_icon;
            }
            let (left, _, right, _) = rects[index];
            let pressed = self.pressed_stack == Some(index);
            let hovered = self.hovered_stack == Some(index);
            let visual = &self.stack_visuals[index];
            let box_x_px = (left * dpi).round();
            let box_y_px = (shell_top * dpi).round();
            if pressed {
                visual.bg.set_content(Some(&self.shell_bg_press));
                visual.bg.set_transform(1.0, 1.0, box_x_px, box_y_px);
            } else if hovered {
                visual.bg.set_content(Some(&self.shell_bg_hover));
                visual.bg.set_transform(1.0, 1.0, box_x_px, box_y_px);
            } else {
                visual.bg.hide();
            }
            let press = if pressed { 0.88 } else { 1.0 };
            let dim = icon_size * visual.scale.max(1.0) * press;
            let center = (left + right) * 0.5;
            visual.icon.set_transform(
                dim * dpi / icon_px,
                dim * dpi / icon_px,
                (center - dim * 0.5) * dpi,
                (icon_base_bottom - dim) * dpi,
            );
        }
        for visual in self.stack_visuals.iter().skip(rects.len()) {
            visual.bg.hide();
            visual.icon.hide();
        }
    }

    /// `"{label} ({count})"` tooltip for a folder stack; the item count is re-read
    /// at most every `STACK_COUNT_TTL` (hidden/system/temp entries excluded).
    fn stack_tooltip_text(&mut self, index: usize) -> Option<String> {
        let now = Instant::now();
        let visual = self.stack_visuals.get(index)?;
        let count = if layout::stack_count_fresh(visual.cached_count, now) {
            visual.cached_count.map(|(_, count)| count)
        } else {
            let fresh = stack_item_count(&visual.path);
            let visual = self.stack_visuals.get_mut(index)?;
            visual.cached_count = fresh.map(|count| (now, count));
            fresh
        };
        let label = self.stack_visuals.get(index)?.label.clone();
        Some(layout::stack_tooltip(&label, count))
    }

    fn update_visuals(&mut self) {
        let now = Instant::now();
        let dpi = self.dpi;
        let profile = layout::size_profile(&self.settings);
        let base = profile.cell;
        let icon_size = profile.icon;
        let gap = slot_gap(&self.settings);
        let (win_w, win_h) = compute_window_dips_with(
            &self.settings,
            self.slots.len(),
            self.mon_w_dips(),
            &self.effective,
        );
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry_at(now);

        // Auto-hide slide translation on root visual (zero child recalculation).
        let edge = effective_edge(&self.settings);
        let vertical = layout::is_vertical(edge);
        let hide_shift_px =
            (self.hide_progress * hide_offset(edge, pill_h + VERTICAL_EDGE_GAP + 10.0) * dpi)
                .round();
        let (hide_x_px, hide_y_px) = if vertical {
            (hide_shift_px, 0.0)
        } else {
            (0.0, hide_shift_px)
        };
        self.dcomp.set_root_offset_x(hide_x_px);
        self.dcomp.set_root_offset_y(hide_y_px);

        // 1. 3-piece pill transform + Clear Crystal Spatial Glass host sync (Glass mode only).
        let cap_w_px = self.pill.cap_w_px;
        let pill_left_px = (pill_left * dpi).round();
        let pill_top_px = (pill_top * dpi).round();
        let pill_w_px = (pill_w * dpi).round().max(cap_w_px * 2.0 + 1.0);
        let pill_h_px = (pill_h * dpi).round().max(1.0);
        let center_w_px = (pill_w_px - cap_w_px * 2.0).max(1.0);
        if let Some(ref mut glass) = self.spatial_glass {
            glass.sync_rect(
                pill_left_px,
                pill_top_px,
                pill_w_px,
                pill_h_px,
                self.settings.corner_radius_dips() * dpi,
                dpi,
                hide_x_px,
                hide_y_px,
            );
        }
        self.pill
            .left
            .set_transform(1.0, 1.0, pill_left_px, pill_top_px);
        self.pill
            .center
            .set_transform(center_w_px, 1.0, pill_left_px + cap_w_px, pill_top_px);
        self.pill
            .right
            .set_transform(1.0, 1.0, pill_left_px + pill_w_px - cap_w_px, pill_top_px);

        let (_, icon_base_bottom, dot_top) = app_icon_vertical_metrics(pill_top, pill_h, icon_size);
        let sm = self.scroll_metrics_for_pill_at(pill_left, pill_w, now);

        // 2. Shell buttons + divider share the horizontally clipped content coordinate space.
        let shell_top = pill_top + (pill_h - base) * 0.5;
        let box_sz_px = (base * dpi).ceil();
        let glyph_dips = (icon_size * 0.95).max(22.0);
        let glyph_surf_px = (glyph_dips * dpi).ceil();
        for kind in ShellButton::ALL {
            let i = kind.index();
            if !is_shell_button_visible(&self.settings, kind) {
                self.shell_bgs[i].hide();
                self.shell_icons[i].hide();
                continue;
            }
            // Single source of truth with hit testing and Start anchoring.
            let x = self.shell_center_x_in(kind, &sm) - base * 0.5;
            let box_x_px = (x * dpi).round();
            let box_y_px = (shell_top * dpi).round();
            let hovered = self.hovered_shell == Some(kind);
            let pressed = self.pressed_shell == Some(kind);
            if pressed {
                self.shell_bgs[i].set_content(Some(&self.shell_bg_press));
                self.shell_bgs[i].set_transform(1.0, 1.0, box_x_px, box_y_px);
            } else if hovered {
                self.shell_bgs[i].set_content(Some(&self.shell_bg_hover));
                self.shell_bgs[i].set_transform(1.0, 1.0, box_x_px, box_y_px);
            } else {
                self.shell_bgs[i].hide();
            }

            let scale = self.shell_scales[i] * if pressed { 0.88 } else { 1.0 };
            let drawn_px = glyph_surf_px * scale;
            // Scale from the bottom baseline like app icons (P1-3). No per-frame
            // `.round()` on the animated offsets: rounding a moving scale made the
            // glyph jitter by a pixel; linear interpolation keeps it smooth.
            let rest_bottom_px = box_y_px + (box_sz_px + glyph_surf_px) * 0.5;
            let gx_px = box_x_px + (box_sz_px - drawn_px) * 0.5;
            let gy_px = rest_bottom_px - drawn_px;
            self.shell_icons[i].set_transform(scale, scale, gx_px, gy_px);
        }

        let div_y = pill_top + (pill_h - 26.0) * 0.5;
        let lead_count = lead_shell_count(&self.settings);
        if lead_count == 0 {
            self.divider.hide();
        } else {
            let div_x =
                sm.shell_start_x + section_width(base, shell_gap(&self.settings), lead_count) + 8.0;
            self.divider
                .set_transform(1.0, 1.0, (div_x * dpi).round(), (div_y * dpi).round());
        }

        // Hardware-clip scrollable content to the dock viewport:
        // - `pill_scroll_clip` uses the exact inner rounded pill capsule so widgets and pill-bound
        //   controls never bleed outside the rounded corners when scrolled.
        // - `scroll_clip` keeps top headroom (`0.0`) so magnified app icons rise freely above the
        //   pill, while still rounding the bottom corners flush with the pill.
        self.scroll_offset = sm.offset;
        self.scroll_target = self.scroll_target.clamp(0.0, sm.max_scroll);
        let clip_inset_px = (1.0 * dpi).max(1.0);
        let pill_h_px = (pill_h * dpi).round();
        let clip_left_px = pill_left_px + clip_inset_px;
        let clip_top_px = pill_top_px + clip_inset_px;
        let clip_right_px = (pill_left_px + pill_w_px - clip_inset_px).max(clip_left_px);
        let clip_bottom_px = (pill_top_px + pill_h_px - clip_inset_px).max(clip_top_px);
        let clip_radius_px = (self.settings.corner_radius_dips() * dpi - clip_inset_px)
            .clamp(0.0, (clip_bottom_px - clip_top_px) * 0.5);
        DcompEngine::update_rounded_clip_rect(
            &self.pill_scroll_clip,
            clip_left_px,
            clip_top_px,
            clip_right_px,
            clip_bottom_px,
            clip_radius_px,
            clip_radius_px,
        );
        DcompEngine::update_rounded_clip_rect(
            &self.scroll_clip,
            clip_left_px,
            0.0,
            clip_right_px,
            clip_bottom_px,
            0.0,
            clip_radius_px,
        );

        // 3. Unified app slots (inside clipped scroll container).
        let active_drag = self.drag.filter(|d| d.active);
        let mut cursor_x = sm.apps_start_x;
        let slot_count = self.slots.len().min(self.slot_visuals.len());
        let slot_widths = self.slot_widths(now);
        for (idx, &slot_w) in slot_widths.iter().enumerate().take(slot_count) {
            let dragging_this = active_drag.is_some_and(|d| d.idx == idx);
            let pressed = self.pressed_idx == Some(idx) && !dragging_this;
            let press_factor = if pressed { 0.88 } else { 1.0 };
            // Icon artwork is smaller than the cell (reference icon 28/34/38 vs cell 38/44/50).
            let dim = icon_size * self.slots[idx].scale * press_factor;
            let bounce = bounce_offset_dips(self.slots[idx].bounce_started).unwrap_or(0.0);
            let lift =
                (self.slot_lift(&self.slots[idx]) - if pressed { 2.0 } else { 0.0 }).max(0.0);
            let slide_x = self.slots[idx].slide_x;
            // `ApplicationIconProvider.UseHoverIcon`: only swap when a hover
            // variant actually exists and the icon is magnified by the pointer.
            let hover_key = icon_cache::hover_variant_key(&self.slots[idx].icon_source, true);
            let has_hover_variant = self
                .icon_surfaces
                .get(&hover_key)
                .is_some_and(|surface| surface.is_some());
            let hover_source = if use_hover_icon(
                self.hovered_idx == Some(idx),
                self.slots[idx].scale > 1.05,
                has_hover_variant,
            ) {
                Some(hover_key)
            } else {
                None
            };
            let source_ref = hover_source
                .as_deref()
                .unwrap_or(self.slots[idx].icon_source.as_str());
            let running = !self.slots[idx].windows.is_empty();
            let active = self.slots[idx].is_foreground;
            let close = close_progress(self.slots[idx].close_started, now);
            let exiting = self.slots[idx].is_exiting();
            if exiting || self.slot_visuals[idx].fading {
                self.slot_visuals[idx].icon.set_opacity(if exiting {
                    1.0 - close.powi(2)
                } else {
                    1.0
                });
                self.slot_visuals[idx].fading = exiting;
            }

            let has_real_icon = self
                .icon_surfaces
                .get(source_ref)
                .and_then(|o| o.as_ref())
                .is_some();
            if self.slot_visuals[idx].bound_source != source_ref
                || self.slot_visuals[idx].bound_has_icon != has_real_icon
            {
                let source = source_ref.to_owned();
                let label = self.slots[idx].label.clone();
                let surf = self
                    .icon_surfaces
                    .get(&source)
                    .and_then(|o| o.clone())
                    .or_else(|| self.fallback_surface_for(&label));
                self.slot_visuals[idx].icon.set_content(surf.as_ref());
                self.slot_visuals[idx].bound_source = source;
                self.slot_visuals[idx].bound_has_icon = has_real_icon;
            }

            if dragging_this {
                self.slot_visuals[idx].icon.hide();
                self.slot_visuals[idx].dot.hide();
            } else {
                let pulse = (close * std::f32::consts::PI).sin().max(0.0);
                let (width, height, sink) = if exiting {
                    // Uniform aspect-ratio shrink with gentle downward ease-out fade.
                    let remaining = (1.0 - close).max(0.0);
                    (dim * remaining, dim * remaining, dim * 0.22 * close)
                } else {
                    (
                        dim * (1.0 - 0.16 * pulse),
                        dim * (1.0 - 0.16 * pulse),
                        dim * 0.08 * pulse,
                    )
                };
                let slot_center = cursor_x + slot_w * self.slots[idx].layout_fraction(now) * 0.5;
                // Side docks bounce away from their screen edge, bottom docks bounce upward.
                let bounce_dx = if vertical {
                    bounce_direction(edge) * bounce
                } else {
                    0.0
                };
                let ix = slot_center + slide_x + bounce_dx - width * 0.5;
                let iy =
                    icon_base_bottom - lift * self.slots[idx].layout_fraction(now) + sink - height;
                let icon_px = icon_cache::ICON_PX as f32;
                self.slot_visuals[idx].icon.set_transform(
                    width * dpi / icon_px,
                    height * dpi / icon_px,
                    ix * dpi,
                    iy * dpi,
                );

                let desired_dot = if !running {
                    0
                } else if active {
                    2
                } else {
                    1
                };
                if self.slot_visuals[idx].bound_dot_state != desired_dot {
                    match desired_dot {
                        2 => self.slot_visuals[idx]
                            .dot
                            .set_content(Some(&self.dot_active)),
                        1 => self.slot_visuals[idx]
                            .dot
                            .set_content(Some(&self.dot_running)),
                        _ => self.slot_visuals[idx].dot.set_content(None),
                    }
                    self.slot_visuals[idx].bound_dot_state = desired_dot;
                }
                if desired_dot == 0 {
                    self.slot_visuals[idx].dot.hide();
                } else {
                    let dot_w = if desired_dot == 2 { 16.0 } else { 6.0 };
                    // Same layout fraction as the icon center, so a closing slot's
                    // dot stays centered under its shrinking icon.
                    let dx = slot_center + slide_x - dot_w * 0.5;
                    self.slot_visuals[idx].dot.set_transform(
                        1.0,
                        1.0,
                        (dx * dpi).round(),
                        (dot_top * dpi).round(),
                    );
                }

                let badge_count = if layout::notification_badges_enabled(&self.settings) {
                    self.slots[idx].badge
                } else {
                    0
                };
                let badge_surface =
                    if badge_count > 0 && self.slot_visuals[idx].badge_count != badge_count {
                        self.dcomp
                            .create_notification_badge_surface(badge_count, self.is_dark, dpi)
                            .ok()
                    } else {
                        None
                    };
                if let Some(badge) = &self.slot_visuals[idx].badge {
                    if badge_count == 0 {
                        badge.hide();
                    } else {
                        if let Some(ref surface) = badge_surface {
                            badge.set_content(Some(surface));
                        }
                        let badge_px = if badge_count > 99 { 22.0 } else { 18.0 };
                        let bx = slot_center + slide_x + width * 0.5 - badge_px * 0.6;
                        let by = icon_base_bottom
                            - lift * self.slots[idx].layout_fraction(now)
                            - height
                            - 4.0;
                        badge.set_transform(1.0, 1.0, (bx * dpi).round(), (by * dpi).round());
                    }
                }
                self.slot_visuals[idx].badge_count = badge_count;
            }

            cursor_x += (slot_w + gap) * self.slots[idx].layout_fraction(now);
        }

        // 4. Widgets inline right after apps (same main strip as `RebuildDockOrder`).
        let widget_h = design::widget_height_dips(pill_h);
        let widget_top = pill_top + (pill_h - widget_h) * 0.5;
        let apps_w = self.live_apps_width_dips_at(now);
        let wg = widget_gap(&self.settings);
        let mut wx = sm.apps_start_x + if apps_w > 0.0 { apps_w + 8.0 } else { 0.0 };
        for wv in &self.widget_visuals {
            // Widget hover/press states are rendered natively on the card surface itself
            // so corner radii never distort when stretched across wide widget cards.
            wv.bg.hide();
            wv.node
                .set_transform(1.0, 1.0, (wx * dpi).round(), (widget_top * dpi).round());
            wx += wv.width_dips + wg;
        }

        // 4a. Trailing shell group pinned to the right edge (`[Divider] [PinnedTray...] [Tray] [Bin]`).
        let trail_w = self.live_trail_section_width();
        if trail_w > 0.0 {
            let mut right_cursor = sm.right_anchor_x;

            if self.settings.show_recycle_bin {
                let bx_box = right_cursor - base;
                let box_x_px = (bx_box * dpi).round();
                let box_y_px = (shell_top * dpi).round();
                if self.pressed_bin {
                    self.bin_bg.set_content(Some(&self.shell_bg_press));
                    self.bin_bg.set_transform(1.0, 1.0, box_x_px, box_y_px);
                } else if self.hovered_bin {
                    self.bin_bg.set_content(Some(&self.shell_bg_hover));
                    self.bin_bg.set_transform(1.0, 1.0, box_x_px, box_y_px);
                } else {
                    self.bin_bg.hide();
                }
                let press_s = if self.pressed_bin { 0.88 } else { 1.0 };
                let target_px = if self.bin_surf_px >= 63.0 {
                    // Native Windows 64x64 Recycle Bin icon: match app icon optical size inside button.
                    (base * 0.86 * dpi * press_s).round()
                } else {
                    glyph_surf_px * press_s
                };
                let scale = target_px / self.bin_surf_px;
                let bx_px = (box_x_px + (box_sz_px - target_px) * 0.5).round();
                let by_px = (box_y_px + (box_sz_px - target_px) * 0.5).round();
                self.bin_node.set_transform(scale, scale, bx_px, by_px);
                right_cursor -= base + wg;
            } else {
                self.bin_bg.hide();
                self.bin_node.hide();
            }

            if self.settings.show_tray_button {
                let tx_box = right_cursor - base;
                let box_x_px = (tx_box * dpi).round();
                let box_y_px = (shell_top * dpi).round();
                if self.pressed_tray {
                    self.tray_bg.set_content(Some(&self.shell_bg_press));
                    self.tray_bg.set_transform(1.0, 1.0, box_x_px, box_y_px);
                } else if self.hovered_tray {
                    self.tray_bg.set_content(Some(&self.shell_bg_hover));
                    self.tray_bg.set_transform(1.0, 1.0, box_x_px, box_y_px);
                } else {
                    self.tray_bg.hide();
                }
                let scale = if self.pressed_tray { 0.88 } else { 1.0 };
                let drawn_px = glyph_surf_px * scale;
                let tx_px = (box_x_px + (box_sz_px - drawn_px) * 0.5).round();
                let ty_px = (box_y_px + (box_sz_px - drawn_px) * 0.5).round();
                self.tray_node.set_transform(scale, scale, tx_px, ty_px);
                let tray_rect = (
                    tx_box,
                    pill_top + 6.0,
                    right_cursor,
                    pill_top + pill_h - 6.0,
                );
                self.update_tray_pin_visuals(tray_rect, shell_top);
                right_cursor -= base + wg;
                if !self.tray_pin_visuals.is_empty() {
                    right_cursor -= tray_pin_cell(base) * self.tray_pin_visuals.len() as f32;
                    if !self.stack_visuals.is_empty() {
                        right_cursor -= wg;
                    }
                }
            } else {
                self.tray_bg.hide();
                self.tray_node.hide();
            }

            // `[Folder1] [Folder2] ...` left of the pinned tray icons, each its own
            // native folder icon, bottom-anchored like app icons.
            let stack_rects = self.stack_rects_in(&sm, pill_top, pill_h);
            if let Some(&(first_left, ..)) = stack_rects.first() {
                right_cursor = first_left - wg;
            }
            self.update_stack_visuals(&stack_rects, shell_top, icon_base_bottom, icon_size);

            let wdiv_x = right_cursor - 5.0;
            self.widget_divider.set_transform(
                1.0,
                1.0,
                (wdiv_x * dpi).round(),
                (div_y * dpi).round(),
            );
        } else {
            self.widget_divider.hide();
            self.tray_bg.hide();
            self.tray_node.hide();
            self.bin_bg.hide();
            self.bin_node.hide();
            for sv in &self.stack_visuals {
                sv.bg.hide();
                sv.icon.hide();
            }
        }

        // 4b. Scroll edge scrim + chevron overlays (only visible when dock content overflows).
        if sm.can_scroll_left {
            self.scroll_prev_node
                .set_content(Some(if self.hovered_scroll == Some(false) {
                    &self.scroll_prev_hover_surf
                } else {
                    &self.scroll_prev_surf
                }));
            self.scroll_prev_node.set_transform(
                1.0,
                1.0,
                (sm.vp_left * dpi).floor(),
                (pill_top * dpi).round(),
            );
        } else {
            self.scroll_prev_node.hide();
        }
        if sm.can_scroll_right {
            self.scroll_next_node
                .set_content(Some(if self.hovered_scroll == Some(true) {
                    &self.scroll_next_hover_surf
                } else {
                    &self.scroll_next_surf
                }));
            self.scroll_next_node.set_transform(
                1.0,
                1.0,
                ((sm.vp_right - SCROLL_EDGE_W_DIPS) * dpi).round(),
                (pill_top * dpi).round(),
            );
        } else {
            self.scroll_next_node.hide();
        }

        // 5. Floating cursor-tracking drag ghost.
        if let Some(drag) = active_drag
            && let Some(slot) = self.slots.get(drag.idx)
        {
            let source = slot.icon_source.clone();
            let label = slot.label.clone();
            if self.drag_ghost_source.as_deref() != Some(source.as_str()) {
                let surf = self
                    .icon_surfaces
                    .get(&source)
                    .and_then(|o| o.clone())
                    .or_else(|| self.fallback_surface_for(&label));
                self.drag_ghost.set_content(surf.as_ref());
                self.drag_ghost_source = Some(source);
            }
            let ghost_dim = base * 1.14;
            let gx = (drag.current_x - drag.grab_offset_x - ghost_dim * 0.5)
                .clamp(4.0, (win_w - ghost_dim - 4.0).max(4.0));
            let gy = win_h - VERTICAL_EDGE_GAP - 20.0 - ghost_dim;
            let s = (ghost_dim * dpi) / icon_cache::ICON_PX as f32;
            self.drag_ghost.set_transform(s, s, gx * dpi, gy * dpi);
        } else {
            self.drag_ghost.hide();
        }

        // 6. Auto-height tooltip rendered last (suppressed when live thumbnail preview is shown).
        // The stack tooltip may refresh its cached item count (`&mut self`), so it
        // is resolved before any borrowed tooltip text below.
        let stack_tip = self
            .hovered_stack
            .and_then(|index| self.stack_tooltip_text(index));
        let preview_showing_for = self
            .preview
            .as_deref()
            .filter(|p| p.is_visible())
            .and_then(|p| p.owner_key());
        // Borrow text from existing state; allocate only when the surface actually changes.
        // Cow keeps borrowed label strings allocation-free; only localized strings whose source
        // is produced by `loc::t` need an owned buffer.
        let tooltip_info: Option<(Cow<'_, str>, f32, f32)> = if active_drag.is_some() {
            None
        } else if let Some(is_next) = self.hovered_scroll {
            let cx = if is_next {
                sm.vp_right - SCROLL_EDGE_W_DIPS * 0.5
            } else {
                sm.vp_left + SCROLL_EDGE_W_DIPS * 0.5
            };
            Some((Cow::Owned(loc::t("ScrollDockHelp")), cx, pill_top - 8.0))
        } else if self.hovered_bin {
            let (_, bin_rc, _) = self.right_widget_rects_dips();
            let cx = bin_rc.map_or(pill_left + pill_w - 60.0, |(l, _, r, _)| (l + r) * 0.5);
            Some((Cow::Owned(loc::t("RecycleBin")), cx, pill_top - 8.0))
        } else if self.hovered_tray {
            let (tray_rc, _, _) = self.right_widget_rects_dips();
            let cx = tray_rc.map_or(pill_left + pill_w - 110.0, |(l, _, r, _)| (l + r) * 0.5);
            Some((Cow::Owned(loc::t("TrayApps")), cx, pill_top - 8.0))
        } else if let Some(index) = self.hovered_tray_pin
            && self
                .settings
                .user_pinned_background_apps
                .get(index)
                .is_some()
            && let (Some(tray), _, _) = self.right_widget_rects_dips()
        {
            let (left, _, right, _) = tray_pin_rect(
                tray,
                tray_pin_cell(base),
                widget_gap(&self.settings),
                self.tray_pin_visuals.len(),
                index,
            );
            let label = self
                .tray_pin_visuals
                .get(index)
                .map_or("", |pin| pin.label.as_str());
            let stack_count = self
                .settings
                .user_pinned_background_apps
                .get(index)
                .and_then(|path| folder_stack_preview_count(path));
            let tooltip = stack_count.map_or_else(
                || Cow::Borrowed(label),
                |count| Cow::Owned(format!("{label} ({count})")),
            );
            Some((tooltip, (left + right) * 0.5, pill_top - 8.0))
        } else if let Some(index) = self.hovered_stack
            && let Some(text) = stack_tip
        {
            let rects = self.stack_rects_in(&sm, pill_top, pill_h);
            let cx = rects
                .get(index)
                .map_or(pill_left + pill_w - 60.0, |(l, _, r, _)| (l + r) * 0.5);
            let lift = self
                .stack_visuals
                .get(index)
                .map_or(0.0, |sv| icon_size * (sv.scale - 1.0).max(0.0));
            Some((Cow::Owned(text), cx, (pill_top - lift).min(pill_top) - 8.0))
        } else if let Some(wid) = self.hovered_widget {
            let (_, _, wrects) = self.right_widget_rects_dips();
            let cx = wrects
                .iter()
                .find(|(id, _)| *id == wid)
                .map_or(pill_left + pill_w - 60.0, |(_, (l, _, r, _))| (l + r) * 0.5);
            let settings = &self.settings;
            let label = self
                .widget_runtime
                .hover_tooltip(wid, self.hovered_widget_sub, settings);
            Some((Cow::Borrowed(label), cx, pill_top - 8.0))
        } else if let Some(shell) = self.hovered_shell {
            Some((
                Cow::Owned(shell.label()),
                self.live_shell_center_x(shell),
                pill_top - 8.0,
            ))
        } else if let Some(idx) = self.hovered_idx
            && let Some(slot) = self.slots.get(idx)
            && preview_showing_for != Some(slot.key.as_str())
        {
            let lift = self.slot_lift(slot);
            let icon_top = icon_base_bottom - lift - icon_size * slot.scale;
            let top_edge = icon_top.min(pill_top);
            let center_x = self.live_slot_center_x(idx);
            Some((Cow::Borrowed(slot.label.as_str()), center_x, top_edge - 8.0))
        } else {
            None
        };

        if let Some((text, center_x, bottom_y)) = tooltip_info {
            let needs_redraw = self
                .tooltip_cache
                .as_ref()
                .is_none_or(|c| c.text != text.as_ref());
            if needs_redraw
                && let Ok((surf, w_dips, h_dips)) = self.dcomp.create_tooltip_surface(
                    text.as_ref(),
                    dpi,
                    self.is_dark,
                    self.settings
                        .menu_border_color
                        .as_deref()
                        .and_then(design::parse_hex_rgb)
                        .unwrap_or(design::palette(self.is_dark).border_rgb),
                    self.settings.menu_border_thickness.clamp(0.0, 8.0) as f32,
                )
            {
                self.tooltip_node.set_content(Some(&surf));
                self.tooltip_cache = Some(TooltipCache {
                    text: text.into_owned(),
                    width_dips: w_dips,
                    height_dips: h_dips,
                });
            }
            if let Some(ref tc) = self.tooltip_cache {
                let tx = (center_x - tc.width_dips * 0.5)
                    .clamp(6.0, (win_w - tc.width_dips - 6.0).max(6.0))
                    .round();
                let ty = (bottom_y - tc.height_dips).max(2.0).round();
                self.tooltip_node
                    .set_transform(1.0, 1.0, (tx * dpi).round(), (ty * dpi).round());
            }
        } else {
            self.tooltip_node.hide();
        }

        self.dcomp.commit();
    }

    fn cancel_preview_timers(&self) {
        unsafe {
            let _ = KillTimer(self.hwnd as HWND, TIMER_PREVIEW_SHOW);
            let _ = KillTimer(self.hwnd as HWND, TIMER_PREVIEW_HIDE);
            let _ = KillTimer(self.hwnd as HWND, TIMER_PREVIEW_WATCH);
        }
    }

    fn is_cursor_over_dock_or_preview(&self) -> bool {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            if GetCursorPos(&mut pt) == 0 {
                return false;
            }
        }
        if let Some(rc) = self.preview.as_deref().and_then(|p| p.window_rect())
            && pt.x >= rc.left
            && pt.x < rc.right
            && pt.y >= rc.top
            && pt.y < rc.bottom
        {
            return true;
        }
        let mut client = pt;
        unsafe {
            ScreenToClient(self.hwnd as HWND, &mut client);
        }
        let mx = client.x as f32 / self.dpi;
        let my = client.y as f32 / self.dpi;
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        // Generous vertical bridge between magnified icons and preview popup so moving upward never flickers.
        mx >= pill_left
            && mx <= pill_left + pill_w
            && my >= pill_top - 36.0
            && my <= pill_top + pill_h
    }

    fn hide_preview_now(&mut self) {
        self.cancel_preview_timers();
        self.preview_pending_idx = None;
        preview::deactivate_live_preview();
        if let Some(ref mut preview) = self.preview {
            preview.hide();
        }
    }

    fn schedule_preview_hide(&mut self) {
        self.preview_pending_idx = None;
        unsafe {
            let _ = KillTimer(self.hwnd as HWND, TIMER_PREVIEW_SHOW);
            SetTimer(
                self.hwnd as HWND,
                TIMER_PREVIEW_HIDE,
                layout::PREVIEW_DELAY_MS as u32,
                None,
            );
        }
    }

    fn sync_preview_hover(&mut self) {
        if self.drag.is_some() || !self.settings.previews_enabled {
            self.hide_preview_now();
            return;
        }
        let running_idx = self
            .hovered_idx
            .filter(|&i| self.slots.get(i).is_some_and(|s| !s.windows.is_empty()));
        if let Some(idx) = running_idx {
            unsafe {
                let _ = KillTimer(self.hwnd as HWND, TIMER_PREVIEW_HIDE);
            }
            let already_showing_this = self.preview.as_deref().is_some_and(|p| {
                p.is_visible() && p.owner_key() == Some(self.slots[idx].key.as_str())
            });
            if already_showing_this {
                return;
            }
            let preview_open = self.preview.as_deref().is_some_and(|p| p.is_visible());
            self.preview_pending_idx = Some(idx);
            if preview_open {
                self.show_preview_for_slot(idx);
            } else {
                unsafe {
                    SetTimer(
                        self.hwnd as HWND,
                        TIMER_PREVIEW_SHOW,
                        layout::PREVIEW_GRACE_MS as u32,
                        None,
                    );
                }
            }
        } else if self.preview.as_deref().is_some_and(|p| p.is_visible()) {
            self.schedule_preview_hide();
        } else {
            self.preview_pending_idx = None;
            unsafe {
                let _ = KillTimer(self.hwnd as HWND, TIMER_PREVIEW_SHOW);
            }
        }
    }

    fn show_preview_for_slot(&mut self, idx: usize) {
        if !self.settings.previews_enabled {
            return;
        }
        let Some(slot) = self.slots.get(idx) else {
            return;
        };
        if slot.windows.is_empty() {
            return;
        }
        let key = slot.key.clone();
        let items = slot.windows.clone();
        let (win_x, win_y, _, _) = self.last_window_rect.unwrap_or_else(|| {
            compute_window_rect_with(
                &self.settings,
                self.slots.len(),
                self.monitor.clone(),
                &self.effective,
            )
        });
        let (_, pill_top, _, _) = self.live_pill_geometry();
        let profile = layout::size_profile(&self.settings);
        let effective_max =
            if fisheye_enabled(&self.settings) && !magnification_expands_dock(&self.settings) {
                profile_max_scale(&self.settings).min(self.session_max_scale)
            } else {
                1.0
            };
        // `ComputePreviewClearance`: keep the thumbnail above the tallest
        // possible magnified icon, including the 1 DIP lift.
        let clearance = compute_preview_clearance(profile.icon, effective_max);
        let top_edge = (pill_top - clearance).min(pill_top);
        // Resting center: with fixed-point anchoring the hovered icon stays at its
        // rest position, so the preview arrow stays centered while the arch decays.
        let anchor_x = win_x + (self.rest_slot_center_x(idx) * self.dpi).round() as i32;
        let anchor_bottom_y = win_y + ((top_edge - 10.0) * self.dpi).round() as i32;

        if let Some(ref mut preview) = self.preview {
            // reference size-profile card dimensions (228x154 / 250x180 DIP).
            preview.set_card_size(profile.preview_card.0, profile.preview_card.1);
            // Clear Crystal optical glass config (strictly active in Glass mode).
            preview.set_glass_config(
                crate::design::spatial_glass::SpatialGlassConfig::from_settings(
                    &self.settings,
                    self.settings.dock_background,
                    self.settings.dock_opacity,
                    self.is_dark,
                    false,
                ),
            );
            preview.show(&key, &items, anchor_x, anchor_bottom_y, self.is_dark);
            // DWM Aero Peek is user-controlled: HKCU\...\DWM\EnableAeroPeek = 0
            // means no live preview even though the thumbnails still render.
            if preview::aero_peek_enabled()
                && let Some(first) = items.first()
            {
                preview::activate_live_preview(first.hwnd);
            }
            unsafe {
                SetTimer(
                    self.hwnd as HWND,
                    TIMER_PREVIEW_WATCH,
                    layout::PREVIEW_FADE_MS as u32,
                    None,
                );
            }
        }
        self.update_visuals();
    }

    // ---------------------------------------------------------------------
    // WidgetHoverCoordinator wiring (WS-D message loop ↔ WS-C hover policy).
    // ---------------------------------------------------------------------

    /// Physical dock rect in pixels, used as the window-picker anchor. Both the
    /// primary dock and a secondary mirror resolve this from their own HWND.
    fn dock_screen_rect(&self) -> (i32, i32, i32, i32) {
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd as HWND, &mut rc) } != 0
            && rc.right > rc.left
            && rc.bottom > rc.top
        {
            (rc.left, rc.top, rc.right, rc.bottom)
        } else {
            self.last_window_rect.unwrap_or((
                self.monitor.left,
                self.monitor.top,
                self.monitor.right,
                self.monitor.bottom,
            ))
        }
    }

    fn picker_monitor_index(&self) -> usize {
        self.all_monitors
            .iter()
            .position(|monitor| monitor.handle == self.monitor.handle)
            .unwrap_or(0)
    }

    /// Promotes preview items to the full `RunningWindow` shape the picker
    /// orders by monitor/AUMID, borrowing the freshest async snapshot we have.
    fn picker_windows(&self, previews: &[PreviewItem], launch_path: &str) -> Vec<RunningWindow> {
        previews
            .iter()
            .map(|item| {
                self.running_windows
                    .iter()
                    .find(|window| window.hwnd == item.hwnd && window.pid == item.pid)
                    .cloned()
                    .unwrap_or_else(|| RunningWindow {
                        hwnd: item.hwnd,
                        pid: item.pid,
                        title: item.title.clone(),
                        exe_path: launch_path.to_owned(),
                        aumid: String::new(),
                        is_foreground: false,
                    })
            })
            .collect()
    }

    /// WS-10: a multi-window application click opens the native window picker
    /// instead of blindly toggling the first window. Reached from the primary
    /// app-click path and, unchanged, from the secondary dock's mirror slot.
    fn show_picker_for_windows(&self, previews: &[PreviewItem], launch_path: &str) -> bool {
        if previews.len() < 2
            || !self.settings.previews_enabled
            || !window_picker::enabled()
            || !extra_flag(&self.settings, "WindowPickerEnabled", true)
        {
            return false;
        }
        let windows = self.picker_windows(previews, launch_path);
        if windows.len() < 2 {
            return false;
        }
        window_picker::show(
            self.picker_monitor_index(),
            windows,
            self.dock_screen_rect(),
        )
    }

    /// Ctrl+right-click on a running app exposes that app's own menu commands
    /// through the WS-B `native_app_menu_entries` / `invoke_native_app_command`
    /// wrappers, remapped so their ids cannot collide with our menu.
    fn show_native_app_menu(&self, target_hwnd: usize) -> bool {
        let entries = menu::native_app_menu_entries(target_hwnd);
        if entries.is_empty() {
            return false;
        }
        let (commands, items) = remap_native_app_menu_entries(entries);
        let chosen = menu::show_popup_menu_with_anchor(
            self.hwnd,
            self.is_dark,
            &items,
            BorderStyle::from_menu_settings(self.is_dark, &self.settings),
            self.dock_menu_anchor(),
        )
        .unwrap_or(0);
        if let Some((_, original)) = commands.into_iter().find(|(id, _)| *id == chosen) {
            menu::invoke_native_app_command(target_hwnd, original);
        }
        true
    }

    /// WS-10: a group flyout row with several live windows opens the picker
    /// keyed by the dock's own monitor; a single window launches directly.
    fn open_group_flyout(&mut self, group_index: usize, slot_path: &str) {
        let Some(group) = self.settings.app_groups.get(group_index).cloned() else {
            return;
        };
        let mut blocks = Vec::new();
        let mut live_total = 0usize;
        for (offset, path) in group.executable_paths.iter().enumerate() {
            let live: Vec<PreviewItem> = self
                .running_windows
                .iter()
                .filter(|window| window.exe_path.eq_ignore_ascii_case(path))
                .map(|window| PreviewItem {
                    hwnd: window.hwnd,
                    pid: window.pid,
                    title: window.title.clone(),
                })
                .collect();
            live_total += live.len();
            blocks.push(FlyoutBlock::ActionRow {
                id: GROUP_FLYOUT_ROW_ID_BASE + offset as u32,
                title: Path::new(path)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or(path)
                    .to_owned(),
                subtitle: Some(format!("{} · {}", live.len(), loc::t("WidgetInstalled"))),
                badge: None,
                icon: None,
                destructive: false,
            });
        }
        blocks.push(FlyoutBlock::Separator);
        blocks.push(FlyoutBlock::ButtonBar(vec![(
            1,
            loc::t("WidgetRemove").to_owned(),
            false,
        )]));
        let title =
            layout::group_flyout_title(&group.name, live_total, group.executable_paths.len());
        let spec = FlyoutSpec {
            title,
            subtitle: None,
            width_dips: 316.0,
            blocks,
        };
        let (anchor_x, anchor_y) = self.flyout_anchor_center_px();
        let action = crate::ui::flyout::show_primary_with_options(
            self.hwnd,
            self.is_dark,
            anchor_x,
            anchor_y,
            spec,
            BorderStyle::from_menu_settings(self.is_dark, &self.settings),
            FlyoutOptions::stack().with_edge(self.dock_flyout_edge()),
        );
        match action {
            Some(1) => {
                if let Some(member) = self.settings.app_groups.get_mut(group_index) {
                    member
                        .executable_paths
                        .retain(|path| !path.eq_ignore_ascii_case(slot_path));
                    if member.executable_paths.is_empty() {
                        self.settings.app_groups.remove(group_index);
                    }
                }
                let _ = settings::save_to(&settings::settings_path(), &self.settings);
                self.reload_settings_live();
            }
            Some(id) if id >= GROUP_FLYOUT_ROW_ID_BASE => {
                let index = (id - GROUP_FLYOUT_ROW_ID_BASE) as usize;
                if let Some(path) = group.executable_paths.get(index).cloned() {
                    let previews: Vec<PreviewItem> = self
                        .running_windows
                        .iter()
                        .filter(|window| window.exe_path.eq_ignore_ascii_case(&path))
                        .map(|window| PreviewItem {
                            hwnd: window.hwnd,
                            pid: window.pid,
                            title: window.title.clone(),
                        })
                        .collect();
                    if previews.len() >= 2
                        && self.settings.previews_enabled
                        && window_picker::enabled()
                        && extra_flag(&self.settings, "WindowPickerEnabled", true)
                    {
                        let windows = self.picker_windows(&previews, &path);
                        window_picker::show(
                            self.picker_monitor_index(),
                            windows,
                            self.dock_screen_rect(),
                        );
                    } else {
                        pins::launch_app(path, None, false);
                    }
                }
            }
            _ => {}
        }
    }

    /// Opens a widget flyout anchored on the dock item `rc`, opening away from
    /// the dock edge. The Keyboard flyout targets the last external foreground
    /// window, never the dock or one of our popups.
    fn open_widget_flyout_at(&mut self, id: &str, rc: DipRect) -> bool {
        self.note_external_foreground();
        if self.keyboard_target_hwnd != 0 {
            self.widget_runtime
                .set_keyboard_target_hwnd(self.keyboard_target_hwnd);
        }
        let (ax, ay) = self.flyout_anchor_px(rc);
        let edge = self.dock_flyout_edge();
        self.hover_flyout_open = true;
        let changed = self.widget_runtime.open_widget_flyout_on_edge(
            id,
            self.hwnd,
            self.is_dark,
            ax,
            ay,
            edge,
            &mut self.settings,
        );
        self.hover_flyout_open = false;
        changed
    }

    /// Remembers the last foreground window that is not OpenTaskbar's own dock,
    /// island, menu or flyout (the Keyboard Layout widget switches *that* app).
    fn note_external_foreground(&mut self) {
        let foreground = unsafe { GetForegroundWindow() } as usize;
        if foreground == 0 || foreground == self.hwnd {
            return;
        }
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(foreground as HWND, &mut pid);
        }
        if pid == 0 || pid == std::process::id() {
            return;
        }
        self.keyboard_target_hwnd = foreground;
        self.widget_runtime.set_keyboard_target_hwnd(foreground);
    }

    /// Left-click on a folder stack: a dedicated flyout listing the folder's
    /// visible subfolders (alphabetical) then recent files, capped at
    /// `STACK_FLYOUT_MAX_ITEMS`, with an "Open in File Explorer" footer.
    fn open_stack_flyout(&mut self, index: usize) {
        let Some(visual) = self.stack_visuals.get(index) else {
            return;
        };
        let path = visual.path.clone();
        let label = visual.label.clone();
        let entries = read_stack_entries(&path).unwrap_or_default();
        if let Some(visual) = self.stack_visuals.get_mut(index) {
            visual.cached_count = Some((Instant::now(), entries.len()));
        }
        let shown = layout::order_stack_entries(entries, layout::STACK_FLYOUT_MAX_ITEMS);
        let row_paths: Vec<String> = shown.iter().map(|entry| entry.path.clone()).collect();
        let icons = if row_paths.is_empty() {
            HashMap::new()
        } else {
            icon_cache::resolve_batch_raw(&row_paths)
        };
        let mut blocks = Vec::with_capacity(shown.len() + 3);
        if shown.is_empty() {
            blocks.push(FlyoutBlock::Note(loc::t("StackEmpty")));
        }
        for (offset, entry) in shown.iter().enumerate() {
            let icon = icons
                .get(&entry.path)
                .and_then(Option::as_ref)
                .map(|raw| FlyoutIcon {
                    width: raw.width,
                    height: raw.height,
                    bgra: raw.pixels.to_vec(),
                });
            blocks.push(FlyoutBlock::ActionRow {
                id: STACK_FLYOUT_ROW_ID_BASE + offset as u32,
                title: entry.name.clone(),
                subtitle: entry.is_dir.then(|| loc::t("FolderLabel")),
                badge: None,
                icon,
                destructive: false,
            });
        }
        blocks.push(FlyoutBlock::Separator);
        blocks.push(FlyoutBlock::ButtonBar(vec![(
            STACK_FLYOUT_OPEN_FOLDER_ID,
            loc::t("OpenFolderInExplorer"),
            false,
        )]));
        let spec = FlyoutSpec {
            title: label,
            subtitle: Some(path.clone()),
            width_dips: 320.0,
            blocks,
        };
        let (anchor_x, anchor_y) = self.stack_rects_dips().get(index).copied().map_or_else(
            || self.flyout_anchor_center_px(),
            |rc| self.flyout_anchor_px(rc),
        );
        self.hover_flyout_open = true;
        let action = crate::ui::flyout::show_primary_with_options(
            self.hwnd,
            self.is_dark,
            anchor_x,
            anchor_y,
            spec,
            BorderStyle::from_menu_settings(self.is_dark, &self.settings),
            FlyoutOptions::stack().with_edge(self.dock_flyout_edge()),
        );
        self.hover_flyout_open = false;
        match action {
            Some(STACK_FLYOUT_OPEN_FOLDER_ID) => open_folder_in_explorer(&path),
            Some(id) if id >= STACK_FLYOUT_ROW_ID_BASE => {
                if let Some(entry) = shown.get((id - STACK_FLYOUT_ROW_ID_BASE) as usize) {
                    pins::launch(entry.path.clone());
                }
            }
            _ => {}
        }
        self.reconcile_pointer_after_interaction();
        self.update_visuals();
    }

    /// Right-click on a folder stack icon.
    fn show_stack_context_menu(&mut self, index: usize) {
        let Some(visual) = self.stack_visuals.get(index) else {
            return;
        };
        let path = visual.path.clone();
        let label = visual.label.clone();
        self.hover_flyout_open = true;
        let action = win32::show_folder_stack_context_menu(
            self.hwnd,
            self.is_dark,
            &label,
            &self.settings,
            self.dock_menu_anchor(),
        );
        self.hover_flyout_open = false;
        match action {
            win32::FolderStackAction::OpenInExplorer => open_folder_in_explorer(&path),
            win32::FolderStackAction::AddFolder => self.add_custom_folder_stack(),
            win32::FolderStackAction::ManageInSettings => settings_ui::spawn_settings_window(),
            win32::FolderStackAction::RemoveFromDock => {
                let before = self.settings.stack_folders.len();
                self.settings
                    .stack_folders
                    .retain(|folder| !folder.trim().eq_ignore_ascii_case(&path));
                if self.settings.stack_folders.len() != before {
                    self.save_and_reload_settings();
                }
            }
            win32::FolderStackAction::None => {}
        }
    }

    /// Native folder picker; appends a unique folder to `stack_folders`.
    fn add_custom_folder_stack(&mut self) {
        let Some(folder) = crate::ui::prompt::open_folder(&loc::t("AddStack")) else {
            return;
        };
        let folder = folder.to_string_lossy().into_owned();
        if layout::add_stack_folder(&mut self.settings.stack_folders, &folder) {
            self.save_and_reload_settings();
        }
    }

    fn save_and_reload_settings(&mut self) {
        if let Err(error) = settings::save_to(&settings::settings_path(), &self.settings) {
            opentaskbar_core::log::error(format!("Could not save dock settings: {error}"));
            return;
        }
        self.reload_settings_live();
        if self.reload_msg != 0 {
            unsafe { PostMessageW(HWND_BROADCAST, self.reload_msg, self.hwnd, 0) };
        }
    }

    fn screen_cursor(&self) -> Option<POINT> {
        let mut pt = POINT { x: 0, y: 0 };
        (unsafe { GetCursorPos(&mut pt) } != 0).then_some(pt)
    }

    fn hover_cursor_point(&self, mx: f32, my: f32) -> POINT {
        self.screen_cursor().unwrap_or_else(|| {
            let (x, y, _, _) = self.last_window_rect.unwrap_or((0, 0, 0, 0));
            POINT {
                x: x + (mx * self.dpi).round() as i32,
                y: y + (my * self.dpi).round() as i32,
            }
        })
    }

    fn widget_phys_rect(&self, id: &str) -> Option<RECT> {
        let (_, _, rects) = self.right_widget_rects_dips();
        let (_, rc) = rects.into_iter().find(|(wid, _)| *wid == id)?;
        let mut win: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            GetWindowRect(self.hwnd as HWND, &mut win);
        }
        let d = self.dpi;
        Some(RECT {
            left: win.left + (rc.0 * d).round() as i32,
            top: win.top + (rc.1 * d).round() as i32,
            right: win.left + (rc.2 * d).round() as i32,
            bottom: win.top + (rc.3 * d).round() as i32,
        })
    }

    fn arm_hover_intent(&mut self, id: &'static str) {
        if self.hover_intent_armed_for == Some(id) {
            return;
        }
        unsafe {
            SetTimer(
                self.hwnd as HWND,
                TIMER_HOVER_INTENT,
                HOVER_INTENT_MS as u32,
                None,
            );
        }
        self.hover_intent_armed_for = Some(id);
    }

    fn disarm_hover_intent(&mut self) {
        if self.hover_intent_armed_for.take().is_some() {
            unsafe {
                KillTimer(self.hwnd as HWND, TIMER_HOVER_INTENT);
            }
        }
    }

    /// `WM_MOUSEMOVE` → `Enter` / `VerifyCursor` / intent arming.
    fn sync_widget_hover(&mut self, mx: f32, my: f32) {
        if self.settings.widget_flyout_trigger
            != opentaskbar_core::settings::WidgetFlyoutTrigger::Hover
        {
            self.disarm_hover_intent();
            self.hover_flyout_pending = None;
            return;
        }
        let now = Instant::now();
        let cursor = self.hover_cursor_point(mx, my);
        self.widget_runtime.hover.set_cursor(cursor.x, cursor.y);
        if self.hover_flyout_open {
            return;
        }
        let Some(id) = self.hovered_widget else {
            self.cancel_widget_hover();
            return;
        };
        if !widget_has_flyout_ci(id) {
            self.cancel_widget_hover();
            return;
        }
        if let Some(rect) = self.widget_phys_rect(id) {
            self.widget_runtime.hover.set_expanded_rect(rect);
        }
        if let Some(previous) = self.widget_runtime.hover.expanded_id().map(str::to_owned)
            && previous != id
        {
            let _ = self.widget_runtime.hover.on_leave(&previous, now);
        }
        self.widget_runtime.hover.on_enter(id, now);
        if self.widget_runtime.hover.expanded_id().is_some()
            && !self.widget_runtime.hover.is_captured()
            && !self.widget_runtime.hover.verify_cursor_stored(cursor)
        {
            self.disarm_hover_intent();
        }
        if self.widget_runtime.hover.phase() == HoverPhase::Pending {
            self.arm_hover_intent(id);
        } else {
            self.disarm_hover_intent();
        }
    }

    /// Cancels pending intent and collapses any expanded inline widget (drag/scroll/leave paths).
    fn cancel_widget_hover(&mut self) {
        let hovered = self.hovered_widget.or(self.hover_intent_armed_for);
        let expanded = self.widget_runtime.hover.expanded_id().map(str::to_owned);
        self.disarm_hover_intent();
        let now = Instant::now();
        let mut changed = false;
        if let Some(id) = hovered {
            changed |= self.widget_runtime.hover.on_leave(id, now) == HoverChange::Collapse;
        }
        if let Some(expanded) = expanded
            && Some(expanded.as_str()) != hovered
        {
            changed |= self.widget_runtime.hover.on_leave(&expanded, now) == HoverChange::Collapse;
        }
        if changed {
            self.refresh_widgets_throttled();
            self.update_visuals();
        }
    }

    /// `WM_TIMER` for the 350 ms hover intent.
    fn tick_widget_hover(&mut self) {
        if self.hover_flyout_open
            || self.settings.widget_flyout_trigger
                != opentaskbar_core::settings::WidgetFlyoutTrigger::Hover
        {
            self.disarm_hover_intent();
            self.hover_flyout_pending = None;
            return;
        }
        let now = Instant::now();
        if self.widget_runtime.hover.tick(now) != HoverChange::Expand {
            if self.widget_runtime.hover.phase() != HoverPhase::Pending {
                self.disarm_hover_intent();
            }
            return;
        }
        self.disarm_hover_intent();
        let Some(id) = self.widget_runtime.hover.expanded_id().map(str::to_owned) else {
            return;
        };
        if !widget_has_flyout_ci(&id) {
            return;
        }
        if let (Some(rect), Some(cursor)) = (self.widget_phys_rect(&id), self.screen_cursor())
            && !self.widget_runtime.hover.verify_cursor(rect, cursor)
        {
            return;
        }
        self.hover_flyout_pending = Some(id);
    }

    /// Opens the rich flyout after hover intent, then suppresses the immediate re-open that would
    /// otherwise be caused by the pointer landing back on the card when the flyout dismisses.
    fn open_pending_hover_flyout(&mut self) {
        if self.hover_flyout_open {
            return;
        }
        let Some(id) = self.hover_flyout_pending.take() else {
            return;
        };
        if self.widget_runtime.hover.is_suppressed(&id, Instant::now()) {
            self.widget_runtime.hover.clear_suppression();
            return;
        }
        let (_, _, rects) = self.right_widget_rects_dips();
        let Some((_, rc)) = rects.into_iter().find(|(wid, _)| wid == &id) else {
            return;
        };
        let anchor_rect = self
            .widget_phys_rect(&id)
            .map(|r| (r.left, r.top, r.right, r.bottom));
        crate::ui::flyout::set_next_hover_anchor(anchor_rect);
        let changed = self.open_widget_flyout_at(&id, rc);
        crate::ui::flyout::set_next_hover_anchor(None);
        if changed {
            // Disk I/O + full DComp rebuild move to the idle
            // `WM_APP_SETTINGS_RELOAD` message; the mouse/timer path only posts.
            self.defer_settings_reload();
        } else {
            self.refresh_widgets_throttled();
        }
        self.finish_widget_hover_flyout(&id);
    }

    fn finish_widget_hover_flyout(&mut self, id: &str) {
        let now = Instant::now();
        self.disarm_hover_intent();
        if self.widget_runtime.hover.expanded_id() == Some(id) {
            let _ = self.widget_runtime.hover.on_leave(id, now);
        }
        self.widget_runtime.hover.suppress(id, now);
        self.reconcile_pointer_after_interaction();
        self.refresh_widgets_throttled();
        self.update_visuals();
    }

    /// Releases the slider-style pointer capture held while a widget is pressed.
    fn finish_widget_pointer_capture(&mut self) {
        let Some(id) = self.hover_capture_id.take() else {
            return;
        };
        let now = Instant::now();
        let ended = self.widget_runtime.hover.end_slider_capture(now);
        let _ = self.widget_runtime.hover.on_leave(&id, now);
        self.widget_runtime.hover.suppress(&id, now);
        self.disarm_hover_intent();
        if ended == HoverChange::Collapse {
            self.refresh_widgets_throttled();
        }
        self.update_visuals();
    }

    fn handle_mouse_move(&mut self, mx: f32, my: f32) {
        if !self.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd as HWND,
                dwHoverTime: 0,
            };
            unsafe {
                TrackMouseEvent(&mut tme);
            }
            self.tracking_mouse = true;
            self.evaluate_auto_hide();
        }

        if let Some((start_mx, start_off, ref mut moved)) = self.scroll_pan {
            let delta = start_mx - mx;
            if delta.abs() > 3.0 {
                *moved = true;
            }
            let max_s = self.live_scroll_metrics().max_scroll;
            self.scroll_offset = (start_off + delta).clamp(0.0, max_s);
            self.scroll_target = self.scroll_offset;
            self.cancel_widget_hover();
            self.update_visuals();
            return;
        }

        if let Some(rdrag) = self.resize_drag {
            self.hide_preview_now();
            let mut pt = POINT { x: 0, y: 0 };
            unsafe {
                GetCursorPos(&mut pt);
            }
            let vertical = layout::is_vertical(effective_edge(&self.settings));
            let delta_px = if vertical {
                pt.y - rdrag.start_screen_y
            } else {
                pt.x - rdrag.start_screen_x
            };
            let delta_dips = delta_px as f32 / self.dpi;
            let signed = match rdrag.edge {
                ResizeEdge::Right | ResizeEdge::Bottom => delta_dips * 2.0,
                ResizeEdge::Left | ResizeEdge::Top => -delta_dips * 2.0,
            };
            let natural_w = if vertical {
                layout::side_dock_height_dips(
                    self.slots.len() + 3,
                    layout::size_profile(&self.settings).cell + slot_gap(&self.settings),
                    22.0,
                    self.settings.show_desktop_button,
                    self.monitor.height() as f32 / self.dpi - 24.0,
                )
            } else {
                content_min_pill_width(&self.settings, self.slots.len(), self.mon_w_dips())
            };
            let min_w = if vertical {
                layout::MIN_CELL_SIZE_DIPS * 2.0
            } else {
                scroll_min_pill_width(&self.settings).min(natural_w)
            };
            let max_w = if vertical {
                self.monitor.height() as f32 / self.dpi - 24.0
            } else {
                (self.mon_w_dips() - 16.0).max(min_w)
            };
            let next_w = (rdrag.start_width_dips + signed)
                .clamp(min_w, max_w)
                .round();
            self.settings.dock_custom_width_dips = Some(next_w);
            self.sync_window_bounds();
            self.cancel_widget_hover();
            self.update_visuals();
            return;
        }

        if self.drag.is_some() {
            self.hide_preview_now();
            self.update_drag(mx);
            self.cancel_widget_hover();
            self.ensure_animation();
            return;
        }

        let resize_edge = self.hit_resize_edge(mx, my);
        if resize_edge != self.hovered_resize {
            self.hovered_resize = resize_edge;
            if resize_edge.is_some() {
                self.hovered_shell = None;
                self.hovered_tray = false;
                self.hovered_tray_pin = None;
                self.hovered_bin = false;
                self.hovered_stack = None;
                self.stack_hover_x = None;
                self.hovered_widget = None;
                self.hover_mouse_x = None;
                self.hovered_idx = None;
                self.sync_preview_hover();
                self.ensure_animation();
                self.update_visuals();
            }
        }
        if resize_edge.is_some() {
            self.cancel_widget_hover();
            return;
        }

        let over_scroll = self.hit_scroll_button(mx, my);
        if over_scroll != self.hovered_scroll {
            let had_widget = self.hovered_widget.is_some();
            self.hovered_scroll = over_scroll;
            if over_scroll.is_some() {
                self.hovered_shell = None;
                self.hovered_tray = false;
                self.hovered_tray_pin = None;
                self.hovered_bin = false;
                self.hovered_stack = None;
                self.stack_hover_x = None;
                self.hovered_widget = None;
                self.hovered_widget_sub = None;
                self.hover_mouse_x = None;
                self.hovered_idx = None;
                self.sync_preview_hover();
                if had_widget {
                    self.refresh_widgets_throttled();
                }
            }
            self.ensure_animation();
            self.update_visuals();
        }
        if over_scroll.is_some() {
            self.cancel_widget_hover();
            return;
        }

        // `End` / `InWidgets` place the Start button inside the trailing group,
        // so shell hit testing must precede the widget/tray boxes to win ties.
        if let Some(shell) = self.hit_shell_button(mx, my) {
            // Only the lead shell group shares the app fisheye. A Start button
            // placed `AfterApps` / `InWidgets` is a non-magnified zone: hovering
            // it must not start (or drag) an app arch (P1-6).
            let magnifies = shell != ShellButton::Start
                || layout::start_strip_group(&self.settings) == layout::StartStripGroup::Head;
            let pointer = magnifies.then_some(mx);
            let changed = self.hovered_shell != Some(shell)
                || match (self.hover_mouse_x, pointer) {
                    (Some(prev), Some(next)) => (prev - next).abs() >= 0.75,
                    (None, None) => false,
                    _ => true,
                };
            self.hovered_shell = Some(shell);
            self.hovered_stack = None;
            self.stack_hover_x = None;
            if magnifies {
                self.begin_hover_session();
                self.fisheye_anchor_x = Some(mx);
            }
            // Shell hover feeds the same fisheye pointer as app icons: the shell
            // glyphs then magnify around the pointer instead of staying at 1.0.
            self.hover_mouse_x = pointer;
            self.hovered_idx = None;
            self.sync_preview_hover();
            if changed {
                self.ensure_animation();
                self.update_visuals();
            }
            return;
        }

        let sm = self.live_scroll_metrics();
        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
        let over_tray = Self::hit_rect_in_vp(&sm, tray_rc, mx, my);
        let over_tray_pin = self.hit_tray_pin(tray_rc, mx, my);
        let over_bin = Self::hit_rect_in_vp(&sm, bin_rc, mx, my);
        let over_stack = self.hit_stack(mx, my);
        if over_stack != self.hovered_stack
            || (over_stack.is_some()
                && self
                    .stack_hover_x
                    .is_none_or(|prev| (prev - mx).abs() >= 0.75))
        {
            let entered = over_stack != self.hovered_stack;
            self.hovered_stack = over_stack;
            self.stack_hover_x = over_stack.map(|_| mx);
            if over_stack.is_some() {
                self.hovered_shell = None;
                self.hover_mouse_x = None;
                self.hovered_idx = None;
                self.sync_preview_hover();
            }
            self.ensure_animation();
            if entered {
                self.update_visuals();
            }
        }
        if over_stack.is_some() {
            self.cancel_widget_hover();
            if self.hovered_tray
                || self.hovered_tray_pin.is_some()
                || self.hovered_bin
                || self.hovered_widget.is_some()
            {
                let had_widget = self.hovered_widget.is_some();
                self.hovered_tray = false;
                self.hovered_tray_pin = None;
                self.hovered_bin = false;
                self.hovered_widget = None;
                self.hovered_widget_sub = None;
                if had_widget {
                    self.refresh_widgets_throttled();
                }
                self.update_visuals();
            }
            return;
        }
        let over_widget_hit = wrects
            .iter()
            .find(|(_, rc)| Self::hit_rect_in_vp(&sm, Some(*rc), mx, my))
            .copied();
        let over_widget = over_widget_hit.map(|(id, _)| id);
        let over_widget_sub = over_widget_hit.and_then(|(id, rc)| {
            let rel_x = mx - rc.0;
            let w = (rc.2 - rc.0).max(1.0);
            match id {
                "media" => {
                    let controls_l = w - 116.0;
                    if rel_x < controls_l {
                        None
                    } else if rel_x < controls_l + 26.0 {
                        Some(0)
                    } else if rel_x < controls_l + 55.0 {
                        Some(1)
                    } else if rel_x < controls_l + 82.0 {
                        Some(2)
                    } else {
                        Some(3)
                    }
                }
                // Three toggles plus the Control Center chevron (`3`).
                "quicksettings" => Some(layout::quick_settings_zone(rel_x, w)),
                "focus" | "stopwatch" | "countdown" => Some(if rel_x >= w - 36.0 { 1 } else { 0 }),
                _ => None,
            }
        });
        if over_tray != self.hovered_tray
            || over_tray_pin != self.hovered_tray_pin
            || over_bin != self.hovered_bin
            || over_widget != self.hovered_widget
            || over_widget_sub != self.hovered_widget_sub
        {
            let widget_changed =
                over_widget != self.hovered_widget || over_widget_sub != self.hovered_widget_sub;
            self.hovered_tray = over_tray;
            self.hovered_tray_pin = over_tray_pin;
            self.hovered_bin = over_bin;
            self.hovered_widget = over_widget;
            self.hovered_widget_sub = over_widget_sub;
            if over_tray || over_tray_pin.is_some() || over_bin || over_widget.is_some() {
                self.hovered_shell = None;
                self.hover_mouse_x = None;
                self.hovered_idx = None;
                self.sync_preview_hover();
            }
            if widget_changed {
                self.refresh_widgets_throttled();
            }
            self.ensure_animation();
            self.update_visuals();
        }
        self.sync_widget_hover(mx, my);
        if over_tray || over_tray_pin.is_some() || over_bin || over_widget.is_some() {
            return;
        }

        let apps_left = sm.vp_left;
        let apps_right = sm
            .vp_right
            .min(sm.apps_start_x + self.live_apps_width_dips() + 8.0);
        if mx >= apps_left && mx <= apps_right && !self.slots.is_empty() {
            if self.hovered_shell.is_some() {
                self.hovered_shell = None;
                self.pressed_shell = None;
            }
            if self
                .hover_mouse_x
                .is_some_and(|prev| (prev - mx).abs() < 0.75)
            {
                return;
            }
            let prev_idx = self.hovered_idx;
            // Freeze the resting layout for this hover session (P0-3/P1-5).
            self.begin_hover_session();
            self.hover_mouse_x = Some(mx);
            self.fisheye_anchor_x = Some(mx);
            self.update_hovered_index_from_mouse();
            if self.hovered_idx != prev_idx {
                self.sync_preview_hover();
            }
            if fisheye_enabled(&self.settings) {
                self.ensure_animation();
            } else if self.hovered_idx != prev_idx {
                self.update_visuals();
            }
        } else if self.hovered_shell.is_some() || self.hover_mouse_x.is_some() {
            self.hovered_shell = None;
            self.hover_mouse_x = None;
            self.hovered_idx = None;
            self.sync_preview_hover();
            self.ensure_animation();
            self.update_visuals();
        }
    }

    fn is_cursor_over_dock_hit_area(&self) -> bool {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            if GetCursorPos(&mut pt) == 0 {
                return false;
            }
            ScreenToClient(self.hwnd as HWND, &mut pt);
        }
        let mx = pt.x as f32 / self.dpi;
        let my = pt.y as f32 / self.dpi;
        self.hit_test_dips(mx, my)
    }

    fn has_active_hover(&self) -> bool {
        self.hover_mouse_x.is_some()
            || self.hovered_idx.is_some()
            || self.hovered_shell.is_some()
            || self.hovered_tray
            || self.hovered_tray_pin.is_some()
            || self.hovered_bin
            || self.hovered_stack.is_some()
            || self.hovered_widget.is_some()
            || self.hovered_scroll.is_some()
            || self.hovered_resize.is_some()
    }

    /// True when another top-level window (a context menu, a flyout, an app
    /// window) is the one under the cursor, even though the cursor is inside
    /// the dock's rect. Our own preview popup does not count as covering.
    fn is_cursor_covered_by_other_window(&self) -> bool {
        let Some(pt) = self.screen_cursor() else {
            return false;
        };
        let top = unsafe { WindowFromPoint(pt) } as usize;
        if top == 0 || top == self.hwnd {
            return false;
        }
        let over_preview = self
            .preview
            .as_deref()
            .and_then(|p| p.window_rect())
            .is_some_and(|rc| {
                pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
            });
        !over_preview
    }

    /// Reconciles pointer hover and animation state after drags, modal flyouts, or context menus,
    /// where `WM_MOUSELEAVE` or `WM_APP_VSYNC` may have been suppressed.
    fn reconcile_pointer_after_interaction(&mut self) {
        // Do NOT clear `tick_pending` here: it belongs to the VSync producer and
        // clearing it on the interaction path floods the queue with new ticks.
        if self.drag.is_some() || self.resize_drag.is_some() || self.scroll_pan.is_some() {
            return;
        }
        // A menu/popup/window covering the dock means the pointer is no longer
        // on the dock even if it is inside its rect: decay the magnification.
        if self.has_active_hover()
            && (self.is_cursor_covered_by_other_window() || !self.is_cursor_over_dock_hit_area())
        {
            self.handle_mouse_leave();
        } else if (self.slots.iter().any(|s| (s.scale - 1.0).abs() > 0.004)
            || self.shell_scales.iter().any(|s| (s - 1.0).abs() > 0.004)
            || self
                .stack_visuals
                .iter()
                .any(|s| (s.scale - 1.0).abs() > 0.004))
            && self.hover_mouse_x.is_none()
        {
            self.ensure_animation();
        }
    }

    fn handle_mouse_leave(&mut self) {
        self.tracking_mouse = false;
        // Re-entry at the same coordinates must run hit testing again.
        self.last_mouse_move = None;
        // A slider capture keeps the panel open; `on_leave` is a no-op while captured.
        self.cancel_widget_hover();
        if self.drag.is_some() || self.resize_drag.is_some() || self.scroll_pan.is_some() {
            return;
        }
        self.hover_mouse_x = None;
        self.hovered_idx = None;
        let had_widget = self.hovered_widget.is_some() || self.pressed_widget.is_some();
        self.hovered_shell = None;
        self.hovered_tray = false;
        self.hovered_tray_pin = None;
        self.hovered_bin = false;
        self.hovered_stack = None;
        self.pressed_stack = None;
        self.stack_hover_x = None;
        // The hover session ends: the next one re-freezes the resting layout.
        self.hover_rest_sm = None;
        self.hovered_widget = None;
        self.hovered_widget_sub = None;
        self.hovered_scroll = None;
        self.hovered_resize = None;
        self.pressed_shell = None;
        self.pressed_tray = false;
        self.pressed_tray_pin = None;
        self.pressed_bin = false;
        self.pressed_widget = None;
        self.pressed_scroll = None;
        self.scroll_pan = None;
        self.pressed_idx = None;
        if had_widget {
            self.refresh_widgets_throttled();
        }
        self.schedule_preview_hide();
        self.evaluate_auto_hide();
        if self.settings.auto_hide != AutoHideMode::Disabled {
            unsafe {
                SetTimer(
                    self.hwnd as HWND,
                    TIMER_HIDE_DELAY,
                    layout::AUTOHIDE_DELAY_MS as u32,
                    None,
                );
            }
        }
        self.ensure_animation();
        self.update_visuals();
    }

    /// Pointer-down snapshot for a shell button. The Start decision also folds
    /// in WS-3's `was_open_at_press(now)`, which attributes a cloak that landed
    /// immediately before the click to the same gesture.
    fn press_shell_button(&mut self, shell: ShellButton) {
        if shell == ShellButton::Start {
            use crate::platform::start_menu_state;
            let now = Instant::now();
            start_menu_state::note_press(now);
            let was_open = start_menu_state::is_open() || start_menu_state::was_open_at_press(now);
            self.pressed_shell_start_was_open = was_open;
            if !was_open {
                let foreground = unsafe { GetForegroundWindow() } as usize;
                let target = restore_target_for_press(
                    foreground,
                    start_menu_state::start_window(),
                    self.hwnd,
                );
                if target != 0 {
                    self.start_restore_hwnd = target;
                }
            }
        }
        self.pressed_shell = Some(shell);
        self.update_visuals();
    }

    fn handle_lbutton_down(&mut self, mx: f32, my: f32) {
        self.hide_preview_now();
        // Remember the real app under the flyout before any modal takes focus.
        let foreground = unsafe { GetForegroundWindow() } as usize;
        self.flyout_focus_return =
            restore_target_for_press(foreground, 0, self.hwnd).max(self.start_restore_hwnd);
        self.note_external_foreground();
        unsafe {
            SetCapture(self.hwnd as HWND);
        }
        if let Some(edge) = self.hit_resize_edge(mx, my) {
            let mut pt = POINT { x: 0, y: 0 };
            unsafe {
                GetCursorPos(&mut pt);
            }
            let (_, _, pill_w, pill_h) = self.live_pill_geometry();
            let vertical = layout::is_vertical(effective_edge(&self.settings));
            self.resize_drag = Some(ResizeDragState {
                edge,
                start_screen_x: pt.x,
                start_screen_y: pt.y,
                start_width_dips: if vertical { pill_h } else { pill_w },
            });
            return;
        }
        if let Some(is_next) = self.hit_scroll_button(mx, my) {
            self.pressed_scroll = Some(is_next);
            self.update_visuals();
            return;
        }
        // Trailing Start (`End` / `InWidgets`) shares a box with widgets/tray.
        if let Some(shell) = self.hit_shell_button(mx, my) {
            self.press_shell_button(shell);
            return;
        }
        let sm = self.live_scroll_metrics();
        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
        if let Some(index) = self.hit_stack(mx, my) {
            self.pressed_stack = Some(index);
            self.update_visuals();
            return;
        }
        if let Some(index) = self.hit_tray_pin(tray_rc, mx, my) {
            self.pressed_tray_pin = self
                .settings
                .user_pinned_background_apps
                .get(index)
                .cloned();
            self.update_visuals();
            return;
        }
        if Self::hit_rect_in_vp(&sm, tray_rc, mx, my) {
            self.pressed_tray = true;
            self.update_visuals();
            return;
        }
        if Self::hit_rect_in_vp(&sm, bin_rc, mx, my) {
            self.pressed_bin = true;
            self.update_visuals();
            return;
        }
        if let Some((wid, _)) = wrects
            .iter()
            .find(|(_, rc)| Self::hit_rect_in_vp(&sm, Some(*rc), mx, my))
        {
            self.pressed_widget = Some(*wid);
            // Explicit click wins over the post-flyout suppression window.
            self.widget_runtime.hover.clear_suppression();
            // Hold the coordinator in `Captured` for the duration of the press so a
            // WM_MOUSELEAVE or gap-crossing cannot collapse the panel mid-interaction.
            let slider_id = u32::from(self.hovered_widget_sub.unwrap_or(0)) + 1;
            self.widget_runtime
                .hover
                .begin_slider_capture(wid, slider_id);
            self.hover_capture_id = Some((*wid).to_owned());
            self.refresh_widgets_throttled();
            self.update_visuals();
            return;
        }

        if let Some(idx) = self.hovered_idx {
            let center = self.live_slot_center_x(idx);
            self.pressed_idx = Some(idx);
            self.drag = Some(DragState {
                idx,
                start_x: mx,
                current_x: mx,
                grab_offset_x: mx - center,
                active: false,
            });
            self.update_visuals();
        }
    }

    /// Start button toggle: a press that found Start open dismisses it and
    /// returns focus to the previously active app; otherwise the Start flyout is
    /// opened through the shell (`SC_TASKLIST`) and anchored by `on_opened`.
    fn toggle_start_menu(&mut self) {
        use crate::platform::start_menu_state;
        if self.settings.start_action == StartButtonAction::Launcher {
            self.request_flyout_anchor(FlyoutKind::Search);
            opentaskbar_shell::taskbar::open_search();
            self.pressed_shell_start_was_open = false;
            return;
        }
        let now = Instant::now();
        if !start_toggle_allowed(self.start_toggle_last, now) {
            self.pressed_shell_start_was_open = false;
            return;
        }
        self.start_toggle_last = Some(now);
        // WS-3's was_open_at_press folds a cloak that landed immediately before
        // the click into the down snapshot; a live re-query here would turn a
        // dismiss into a reopen after the cloak.
        let was_open =
            self.pressed_shell_start_was_open || start_menu_state::was_open_at_press(now);
        if was_open {
            let hwnd = match start_menu_state::start_window() {
                0 => opentaskbar_shell::flyout_reposition::find_start_window().unwrap_or(0),
                hwnd => hwnd,
            };
            if hwnd != 0 {
                let _ = opentaskbar_shell::flyout_reposition::close_start(hwnd);
            }
            // WS-B's reset() also clears start_window; the cloak alone stamps
            // the dismissal grace, not a foreground-only change.
            start_menu_state::reset();
            restore_app_foreground(self.start_restore_hwnd, hwnd);
        } else {
            // `open_start_via_syscommand` already verifies the open and falls
            // back to VK_LWIN internally — a second chord here would toggle it
            // straight back closed.
            let _ = opentaskbar_shell::flyout_reposition::open_start_via_syscommand(self.hwnd);
        }
        self.pressed_shell_start_was_open = false;
    }

    fn handle_lbutton_up(&mut self, mx: f32, my: f32) {
        unsafe {
            let _ = ReleaseCapture();
        }
        if self.resize_drag.take().is_some() {
            let _ = opentaskbar_core::settings::save_to(
                &opentaskbar_core::settings::settings_path(),
                &self.settings,
            );
            return;
        }
        if let Some(is_next) = self.pressed_scroll.take() {
            if self.hit_scroll_button(mx, my) == Some(is_next) {
                self.scroll_by_step(is_next);
            }
            self.update_visuals();
            return;
        }
        // Shell buttons are resolved before widgets/tray so a trailing Start
        // button cannot be swallowed by an overlapping widget chip.
        if let Some(shell) = self.pressed_shell.take() {
            self.update_visuals();
            if self.hit_shell_button(mx, my) == Some(shell) {
                // Re-publish the resting-layout anchors so the Start/Search flyout
                // lands on the button even if the strip was magnified (P1-7).
                self.refresh_shell_button_anchors();
                match shell {
                    ShellButton::Start => self.toggle_start_menu(),
                    ShellButton::Search => {
                        self.request_flyout_anchor(FlyoutKind::Search);
                        opentaskbar_shell::taskbar::open_search();
                    }
                    ShellButton::TaskView => opentaskbar_shell::taskbar::open_task_view(),
                }
            }
            return;
        }
        let sm = self.live_scroll_metrics();
        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
        if let Some(index) = self.pressed_stack.take() {
            self.update_visuals();
            if self.hit_stack(mx, my) == Some(index) {
                self.open_stack_flyout(index);
            }
            return;
        }
        if let Some(path) = self.pressed_tray_pin.take() {
            self.update_visuals();
            if self.hit_tray_pin(tray_rc, mx, my).is_some_and(|index| {
                self.settings.user_pinned_background_apps[index].eq_ignore_ascii_case(&path)
            }) {
                tray::open_pinned_app(self.hwnd, &path);
            }
            return;
        }
        if self.pressed_tray {
            self.pressed_tray = false;
            self.update_visuals();
            if let Some(rc) = tray_rc
                && Self::hit_rect_in_vp(&sm, Some(rc), mx, my)
            {
                let (ax, ay) = self.flyout_anchor_px(rc);
                let edge = self.dock_flyout_edge();
                if let Some((path, pinned)) = tray::open_background_apps_flyout(
                    self.hwnd,
                    self.is_dark,
                    &self.settings,
                    ax,
                    ay,
                    edge,
                    &self.settings.user_pinned_background_apps,
                ) {
                    self.set_background_pin(&path, pinned);
                }
            }
            return;
        }
        if self.pressed_bin {
            self.pressed_bin = false;
            self.update_visuals();
            if Self::hit_rect_in_vp(&sm, bin_rc, mx, my) {
                pins::launch("shell:RecycleBinFolder".to_owned());
            }
            return;
        }
        if let Some(wid) = self.pressed_widget.take() {
            self.update_visuals();
            // The click that merely dismisses a flyout must not re-open it.
            if self.widget_runtime.hover.is_suppressed(wid, Instant::now()) {
                self.widget_runtime.hover.clear_suppression();
                if self.flyout_focus_return != 0 {
                    restore_app_foreground(self.flyout_focus_return, 0);
                    self.flyout_focus_return = 0;
                }
                return;
            }
            if let Some((_, rc)) = wrects
                .iter()
                .find(|(id, rc)| *id == wid && Self::hit_rect_in_vp(&sm, Some(*rc), mx, my))
            {
                let rel_x = mx - rc.0;
                let width = (rc.2 - rc.0).max(1.0);
                let outcome =
                    self.widget_runtime
                        .handle_direct_click(wid, rel_x, width, &mut self.settings);
                match outcome {
                    widgets::DirectClickOutcome::SettingsChanged => {
                        let _ = opentaskbar_core::settings::save_to(
                            &opentaskbar_core::settings::settings_path(),
                            &self.settings,
                        );
                        self.reload_settings_live();
                    }
                    widgets::DirectClickOutcome::Handled => {
                        self.refresh_widgets_throttled();
                        self.update_visuals();
                    }
                    widgets::DirectClickOutcome::OpenFlyout => {
                        crate::ui::flyout::set_next_hover_anchor(None);
                        let changed = self.open_widget_flyout_at(wid, *rc);
                        if changed {
                            let _ = opentaskbar_core::settings::save_to(
                                &opentaskbar_core::settings::settings_path(),
                                &self.settings,
                            );
                            self.reload_settings_live();
                        } else {
                            self.refresh_widgets_throttled();
                            self.update_visuals();
                        }
                    }
                }
            }
            return;
        }
        let was_drag = self.finish_drag();
        let clicked_idx = self.pressed_idx.take();
        if was_drag {
            self.ensure_animation();
            self.update_visuals();
            return;
        }

        if let Some(idx) = clicked_idx
            && let Some(slot) = self.slots.get_mut(idx)
        {
            slot.bounce_started = Some(Instant::now());
            let hwnds = slot.hwnds();
            let slot_windows = slot.windows.clone();
            let launch_path = slot.launch_path.clone();
            let shift = win32::is_shift_down();
            let ctrl = win32::is_ctrl_down();
            let identities: Vec<(usize, u32)> =
                slot.windows.iter().map(|w| (w.hwnd, w.pid)).collect();
            self.ensure_animation();
            self.update_visuals();

            // WS-10: a multi-window click opens the picker on this dock's monitor.
            // A secondary dock instance reaches the exact same path with its own
            // HWND/monitor, so the mirror slot is wired without a special case.
            // Shift+click keeps the classic "launch a new instance" behaviour.
            let picker_opened = !shift && self.show_picker_for_windows(&slot_windows, &launch_path);
            if !picker_opened && (shift || !tracker::activate_or_toggle_verified(&identities)) {
                pins::launch_app(launch_path, hwnds.first().copied(), ctrl && shift);
            }
        } else {
            self.update_visuals();
        }
    }

    fn handle_mbutton_down(&mut self, mx: f32) {
        self.hide_preview_now();
        // Capture on every middle press, including a non-scrollable dock: without
        // it a drag that leaves the window loses the matching WM_MBUTTONUP and
        // leaves the dock (or the shell) in a stuck press state.
        unsafe {
            SetCapture(self.hwnd as HWND);
        }
        if has_scroll_overflow(self.live_scroll_metrics().max_scroll) {
            self.scroll_pan = Some((mx, self.scroll_offset, false));
        }
    }

    fn handle_mbutton_up(&mut self) {
        self.hide_preview_now();
        unsafe {
            let _ = ReleaseCapture();
        }
        if let Some(index) = self.hovered_tray_pin
            && let Some(path) = self
                .settings
                .user_pinned_background_apps
                .get(index)
                .cloned()
        {
            tray::close_pinned_app(self.hwnd, &path);
            return;
        }
        if let Some((_, _, moved)) = self.scroll_pan.take()
            && moved
        {
            return;
        }
        // Widget chips own their middle click (mute, play/pause, lap, refresh, ...).
        if let Some(wid) = self.hovered_widget {
            if self.widget_runtime.handle_middle_click(wid, &self.settings) {
                let _ = self.refresh_widgets();
                self.tooltip_cache = None;
                self.update_visuals();
            }
            return;
        }
        if let Some(idx) = self.hovered_idx
            && let Some(slot) = self.slots.get_mut(idx)
        {
            match self.settings.middle_click_action {
                MiddleClickAction::NewWindow => {
                    slot.bounce_started = Some(Instant::now());
                    let launch_path = slot.launch_path.clone();
                    let window = slot.windows.first().map(|window| window.hwnd);
                    self.ensure_animation();
                    pins::launch_app(launch_path, window, false);
                }
                MiddleClickAction::CloseWindow => {
                    let windows: Vec<(usize, u32)> =
                        slot.windows.iter().map(|w| (w.hwnd, w.pid)).collect();
                    for (hwnd, pid) in windows {
                        tracker::close_window_verified(hwnd, pid);
                    }
                }
                MiddleClickAction::Nothing => {}
            }
        }
    }

    /// Widget chip under `(mx, my)` for wheel routing. Uses the full pill height
    /// (no 6 DIP top/bottom dead band like the hover/click rects).
    fn wheel_widget_at(&self, mx: f32, my: f32) -> Option<&'static str> {
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        if my < pill_top || my > pill_top + pill_h {
            return None;
        }
        let sm = self.scroll_metrics_for_pill(pill_left, pill_w);
        if mx < sm.vp_left || mx > sm.vp_right {
            return None;
        }
        let (_, _, rects) = self.right_widget_rects_dips();
        rects
            .into_iter()
            .find(|(_, (l, _, r, _))| mx >= *l && mx <= *r)
            .map(|(id, _)| id)
    }

    /// Applies one volume wheel step and repaints `%`, speaker glyph and tooltip
    /// on the same frame (unthrottled).
    fn apply_volume_wheel(&mut self, delta: i16) {
        self.widget_runtime.adjust_volume_wheel(i32::from(delta));
        let _ = self.refresh_widgets();
        self.tooltip_cache = None;
        self.update_visuals();
    }

    fn handle_mouse_wheel(&mut self, mx: f32, my: f32, delta: i16, horizontal: bool) {
        if delta == 0 {
            return;
        }
        let hit_widget = self.wheel_widget_at(mx, my);
        if !horizontal {
            match hit_widget.map(registry::canonical_widget_id) {
                // The Audio/Media chips always own the wheel: `wheel_volume_enabled`
                // only governs dock-wide background scrolling.
                Some("audio" | "media") => {
                    self.apply_volume_wheel(delta);
                    return;
                }
                Some("keyboard") => {
                    self.note_external_foreground();
                    if self.keyboard_target_hwnd != 0 {
                        self.widget_runtime
                            .set_keyboard_target_hwnd(self.keyboard_target_hwnd);
                    }
                    self.widget_runtime.cycle_keyboard_layout(delta < 0);
                    let _ = self.refresh_widgets();
                    self.tooltip_cache = None;
                    self.update_visuals();
                    return;
                }
                _ => {}
            }
        }
        let sm = self.live_scroll_metrics();
        if sm.max_scroll > 0.5 && delta != 0 {
            let step = wheel_scroll_step(
                layout::size_profile(&self.settings).cell,
                slot_gap(&self.settings),
            );
            let signed =
                wheel_to_axis_delta(delta, horizontal, effective_edge(&self.settings), step);
            let base = if (self.scroll_offset - self.scroll_target).abs() > 1.0
                && signed.signum() == (self.scroll_target - self.scroll_offset).signum()
            {
                self.scroll_target
            } else {
                self.scroll_offset
            };
            self.scroll_target = (base + signed).clamp(0.0, sm.max_scroll);
            self.ensure_animation();
            self.update_visuals();
            return;
        }
        let over_slot = self.hovered_idx.or_else(|| self.slot_index_at(mx));
        if !horizontal
            && let Some(idx) = over_slot
            && let Some(slot) = self.slots.get(idx)
        {
            if !slot.windows.is_empty() {
                let hwnds = slot.hwnds();
                tracker::cycle_windows(&hwnds, delta < 0);
            }
            return;
        }
        // Dock-wide background wheel (outside widgets and app icons) adjusts the
        // volume only when the user enabled it.
        if !horizontal && hit_widget.is_none() && self.settings.wheel_volume_enabled {
            self.apply_volume_wheel(delta);
        }
    }

    fn toggle_pin_for_slot(&mut self, idx: usize) {
        let Some(slot) = self.slots.get(idx).cloned() else {
            return;
        };
        self.set_dock_pin(&slot.launch_path, !slot.pinned);
    }

    /// Entry point for notification aggregation (`RunningAppProjection.HasNotification`).
    fn set_notification_badge(&mut self, launch_path: &str, count: u32) {
        if let Some(slot) = self
            .slots
            .iter_mut()
            .find(|s| s.launch_path.eq_ignore_ascii_case(launch_path))
        {
            slot.badge = count;
        }
        self.update_visuals();
    }

    /// Drains EVENT_SYSTEM_ALERT posts on the UI thread. Even when badges are
    /// disabled the queue is cleared so it cannot grow across a long session.
    fn drain_notification_badges(&mut self) {
        let alerts: Vec<String> = NOTIFICATION_ALERTS
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default();
        if alerts.is_empty() {
            return;
        }
        if !self.settings.notification_badges_enabled
            || !layout::notification_badges_enabled(&self.settings)
        {
            return;
        }
        let counts = NOTIFICATION_COUNTS.lock();
        let mut seen: HashSet<String> = HashSet::new();
        for path in alerts {
            if !seen.insert(path.clone()) {
                continue;
            }
            let count = counts
                .as_ref()
                .ok()
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|(exe, _)| exe == &path)
                        .map(|(_, count)| *count)
                })
                .unwrap_or(1);
            self.set_notification_badge(&path, count);
        }
    }

    fn set_dock_pin(&mut self, path: &str, pinned: bool) {
        if pinned && pins::pin_from_path(path).is_none() {
            tray::show_action_error(
                self.hwnd,
                "Could not pin the app. Its executable or shortcut is unavailable.",
            );
            return;
        }
        let settings_path = settings::settings_path();
        let mut next = settings::load_from(&settings_path);
        let (add, remove) = if pinned {
            (&mut next.user_pinned_apps, &mut next.user_unpinned_apps)
        } else {
            (&mut next.user_unpinned_apps, &mut next.user_pinned_apps)
        };
        remove.retain(|entry| !entry.eq_ignore_ascii_case(path));
        if !add.iter().any(|entry| entry.eq_ignore_ascii_case(path)) {
            add.push(path.to_owned());
        }
        self.save_pin_change(&next);
    }

    fn set_background_pin(&mut self, path: &str, pinned: bool) {
        if pinned && !Path::new(path).is_file() {
            tray::show_action_error(
                self.hwnd,
                "Could not pin the background app. Its executable is unavailable.",
            );
            return;
        }
        let mut next = settings::load_from(&settings::settings_path());
        update_background_pin(&mut next, path, pinned);
        self.save_pin_change(&next);
    }

    fn save_pin_change(&mut self, next: &DockSettings) {
        if settings::save_to(&settings::settings_path(), next).is_err() {
            tray::show_action_error(
                self.hwnd,
                "Could not save the dock pin. Check that the settings folder is writable and try again.",
            );
            return;
        }

        self.reload_settings_live();
        if self.settings.show_on_all_monitors && self.reload_msg != 0 {
            unsafe { PostMessageW(HWND_BROADCAST, self.reload_msg, self.hwnd, 0) };
        }
    }

    fn handle_drop_files(&mut self, hdrop: usize) {
        let files = win32::extract_dropped_files(hdrop);
        if files.is_empty() {
            return;
        }

        // Dropping files on the Win11 Show Desktop corner moves (or Ctrl+copies) them
        // to the Desktop through a real SHFileOperation with Explorer undo support.
        let mut screen_pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut screen_pt);
        }
        let over_desktop_corner = self.settings.show_desktop_button
            && (self.monitor.right - screen_pt.x).unsigned_abs() <= 24
            && (self.monitor.bottom - screen_pt.y).unsigned_abs() <= 48;
        if over_desktop_corner {
            let copy = win32::is_ctrl_down();
            let paths: Vec<String> = files.to_vec();
            if let Err(error) = shell_files::transfer_to_desktop(&paths, copy) {
                opentaskbar_core::log::warn(format!("Desktop drop failed: {error}"));
            }
            return;
        }

        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
            ScreenToClient(self.hwnd as HWND, &mut pt);
        }
        let mx = pt.x as f32 / self.dpi;
        let my = pt.y as f32 / self.dpi;
        self.hover_mouse_x = Some(mx);
        self.update_hovered_index_from_mouse();

        // Dropping files onto the Recycle Bin deletes them (move to bin, not permanent).
        let sm = self.live_scroll_metrics();
        let (_, bin_rc, _) = self.right_widget_rects_dips();
        if drop_targets_recycle_bin(Self::hit_rect_in_vp(&sm, bin_rc, mx, my), &files)
            && delete_files_to_recycle_bin(self.hwnd, &files)
        {
            self.bin_is_full = widgets::is_recycle_bin_non_empty();
            self.refresh_widgets_throttled();
            self.update_visuals();
            return;
        }

        // Dropping files onto a folder stack moves them into that folder
        // (Ctrl = copy) through a real shell operation with undo support.
        if let Some(index) = self.hit_stack(mx, my)
            && let Some(folder) = self.stack_visuals.get(index).map(|sv| sv.path.clone())
        {
            let sources: Vec<String> = files
                .iter()
                .filter(|file| {
                    !file.eq_ignore_ascii_case(&folder)
                        && Path::new(file.as_str()).parent().is_none_or(|parent| {
                            !parent
                                .to_string_lossy()
                                .trim_end_matches(['\\', '/'])
                                .eq_ignore_ascii_case(folder.trim_end_matches(['\\', '/']))
                        })
                })
                .cloned()
                .collect();
            if !sources.is_empty() {
                let destination = [folder];
                let outcome = if win32::is_ctrl_down() {
                    shell_files::copy_files(
                        &sources,
                        &destination,
                        shell_files::UIOption::AllDialogs,
                    )
                } else {
                    shell_files::move_files(
                        &sources,
                        &destination,
                        shell_files::UIOption::AllDialogs,
                    )
                };
                if let shell_files::FileOperationOutcome::Failed(code) = outcome {
                    opentaskbar_core::log::warn(format!(
                        "Folder stack drop failed (0x{code:08X})."
                    ));
                }
            }
            if let Some(visual) = self.stack_visuals.get_mut(index) {
                visual.cached_count = None;
            }
            self.tooltip_cache = None;
            self.update_visuals();
            return;
        }

        // If dropped directly onto an app icon, open the dropped files with that app.
        if let Some(idx) = self.hovered_idx
            && let Some(slot) = self.slots.get(idx)
        {
            let target = slot.launch_path.clone();
            let args = layout::build_quoted_arguments(&files);
            pins::launch_with_args(target, args);
            return;
        }

        // Otherwise pin dropped executables and shortcuts to the dock, and pin
        // dropped folders as individual folder stacks.
        let mut added = false;
        let mut stacks_added = false;
        for path in files {
            if Path::new(&path).is_dir() {
                stacks_added |= layout::add_stack_folder(&mut self.settings.stack_folders, &path);
                continue;
            }
            let lower = path.to_lowercase();
            if (lower.ends_with(".exe") || lower.ends_with(".lnk") || lower.ends_with(".url"))
                && !self
                    .settings
                    .user_pinned_apps
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(&path))
            {
                self.settings.user_pinned_apps.push(path);
                added = true;
            }
        }
        if stacks_added {
            // New stack icons need a visual-tree rebuild; the full reload also
            // picks up any app pins added in the same drop.
            self.save_and_reload_settings();
        } else if added {
            let _ = opentaskbar_core::settings::save_to(
                &opentaskbar_core::settings::settings_path(),
                &self.settings,
            );
            self.pins = load_effective_pins(&self.settings);
            self.sync_slots(false);
            self.update_visuals();
        }
    }

    fn handle_rbutton_up(&mut self, mx: f32, my: f32) {
        self.hide_preview_now();
        // Cancel and suppress any armed widget hover-intent so right-clicking a widget or
        // keeping its context menu open never pops the widget's flyout behind/after the menu.
        self.disarm_hover_intent();
        self.hover_flyout_pending = None;
        if let Some(wid) = self.hovered_widget {
            self.widget_runtime.hover.suppress(wid, Instant::now());
        }

        let sm = self.live_scroll_metrics();
        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
        if let Some(index) = self.hit_stack(mx, my) {
            self.show_stack_context_menu(index);
            return;
        }
        if let Some(index) = self.hit_tray_pin(tray_rc, mx, my) {
            let path = self.settings.user_pinned_background_apps[index].clone();
            self.hover_flyout_open = true;
            let unpin = tray::show_pinned_app_menu(
                self.hwnd,
                self.is_dark,
                &self.settings,
                &path,
                self.dock_menu_anchor(),
            );
            self.hover_flyout_open = false;
            if unpin {
                self.set_background_pin(&path, false);
            }
            return;
        }
        if Self::hit_rect_in_vp(&sm, bin_rc, mx, my) {
            self.hover_flyout_open = true;
            let action = win32::show_recycle_bin_context_menu(
                self.hwnd,
                self.is_dark,
                &self.settings,
                self.dock_menu_anchor(),
            );
            self.hover_flyout_open = false;
            match action {
                RecycleBinAction::Open => pins::launch("shell:RecycleBinFolder".to_owned()),
                RecycleBinAction::Empty => {
                    widgets::empty_recycle_bin();
                    self.refresh_widgets_throttled();
                    self.update_visuals();
                }
                RecycleBinAction::None => {}
            }
            return;
        }
        if let Some((wid, _)) = wrects
            .iter()
            .find(|(_, rc)| Self::hit_rect_in_vp(&sm, Some(*rc), mx, my))
        {
            self.widget_runtime.hover.suppress(wid, Instant::now());
            let reset_label = loc::t("Reset");
            let sound_label = loc::t_fmt("RadioSettings", &[&loc::t("LauncherSound")]);
            let focus_label = loc::t("FocusMode");
            let secondary_label = match *wid {
                "stopwatch" | "countdown" | "focus" => Some(reset_label.as_str()),
                "audio" => Some(sound_label.as_str()),
                "quicksettings" => Some(focus_label.as_str()),
                _ => None,
            };
            let visible: Vec<&'static str> = self.widget_visuals.iter().map(|wv| wv.id).collect();
            let position = visible.iter().position(|id| id == wid);
            let can_move_earlier = position.is_some_and(|pos| pos > 0);
            let can_move_later = position.is_some_and(|pos| pos + 1 < visible.len());
            self.hover_flyout_open = true;
            let action = win32::show_widget_context_menu(
                self.hwnd,
                self.is_dark,
                wid,
                secondary_label,
                &self.all_monitors,
                false,
                can_move_earlier,
                can_move_later,
                &self.settings,
                self.dock_menu_anchor(),
            );
            self.hover_flyout_open = false;
            self.disarm_hover_intent();
            self.hover_flyout_pending = None;
            self.widget_runtime.hover.suppress(wid, Instant::now());
            match action {
                WidgetContextAction::PlaceAllDisplays => {
                    self.settings.dynamic_island_placements.insert(
                        (*wid).to_owned(),
                        opentaskbar_core::settings::DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned(),
                    );
                    if let Err(error) = opentaskbar_core::settings::save_to(
                        &opentaskbar_core::settings::settings_path(),
                        &self.settings,
                    ) {
                        opentaskbar_core::log::error(format!(
                            "Could not place widget {wid} on Dynamic Island: {error}"
                        ));
                    } else {
                        self.reload_settings_live();
                        if self.reload_msg != 0 {
                            unsafe {
                                windows_sys::Win32::UI::WindowsAndMessaging::SendNotifyMessageW(
                                    HWND_BROADCAST,
                                    self.reload_msg,
                                    0,
                                    0,
                                );
                            }
                        }
                    }
                }
                WidgetContextAction::PlaceOnDisplay(idx) => {
                    if let Some(mon) = self.all_monitors.get(idx) {
                        self.settings
                            .dynamic_island_placements
                            .insert((*wid).to_owned(), mon.device_name.clone());
                        if let Err(error) = opentaskbar_core::settings::save_to(
                            &opentaskbar_core::settings::settings_path(),
                            &self.settings,
                        ) {
                            opentaskbar_core::log::error(format!(
                                "Could not place widget {wid} on display: {error}"
                            ));
                        } else {
                            self.reload_settings_live();
                            if self.reload_msg != 0 {
                                unsafe {
                                    windows_sys::Win32::UI::WindowsAndMessaging::SendNotifyMessageW(
                                        HWND_BROADCAST,
                                        self.reload_msg,
                                        0,
                                        0,
                                    );
                                }
                            }
                        }
                    }
                }
                WidgetContextAction::MoveToTaskbar => {
                    self.settings
                        .dynamic_island_placements
                        .retain(|k, _| !k.eq_ignore_ascii_case(wid));
                    let _ = opentaskbar_core::settings::save_to(
                        &opentaskbar_core::settings::settings_path(),
                        &self.settings,
                    );
                    self.reload_settings_live();
                }
                WidgetContextAction::MoveEarlier | WidgetContextAction::MoveLater => {
                    let offset = if action == WidgetContextAction::MoveEarlier {
                        -1
                    } else {
                        1
                    };
                    if let Some(order) = layout::move_widget_in_order(
                        &visible,
                        &self.settings.widget_order,
                        wid,
                        offset,
                    ) {
                        self.settings.widget_order = order;
                        self.save_and_reload_settings();
                    }
                }
                WidgetContextAction::Remove => {
                    self.settings.set_widget_enabled(wid, false);
                    if let Err(error) = opentaskbar_core::settings::save_to(
                        &opentaskbar_core::settings::settings_path(),
                        &self.settings,
                    ) {
                        opentaskbar_core::log::error(format!(
                            "Could not remove widget {wid}: {error}"
                        ));
                    } else {
                        self.reload_settings_live();
                    }
                }
                WidgetContextAction::Secondary => {
                    if let Some(outcome) = self
                        .widget_runtime
                        .handle_secondary_click(wid, &mut self.settings)
                    {
                        match outcome {
                            widgets::DirectClickOutcome::SettingsChanged => {
                                let _ = opentaskbar_core::settings::save_to(
                                    &opentaskbar_core::settings::settings_path(),
                                    &self.settings,
                                );
                                self.reload_settings_live();
                            }
                            widgets::DirectClickOutcome::Handled
                            | widgets::DirectClickOutcome::OpenFlyout => {
                                self.refresh_widgets_throttled();
                                self.update_visuals();
                            }
                        }
                    }
                }
                WidgetContextAction::Dock(act) => self.execute_context_action(act),
                WidgetContextAction::None => {}
            }
            return;
        }

        if self.hit_shell_button(mx, my) == Some(ShellButton::Start) {
            let act = win32::show_start_context_menu(
                self.hwnd,
                self.is_dark,
                &self.settings,
                self.dock_menu_anchor(),
            );
            self.execute_context_action(act);
            return;
        }

        if self.hit_shell_button(mx, my) == Some(ShellButton::TaskView) {
            match win32::show_taskview_context_menu(
                self.hwnd,
                self.is_dark,
                &self.settings,
                self.dock_menu_anchor(),
            ) {
                TaskViewAction::PreviousDesktop => opentaskbar_shell::taskbar::previous_desktop(),
                TaskViewAction::NextDesktop => opentaskbar_shell::taskbar::next_desktop(),
                TaskViewAction::NewDesktop => opentaskbar_shell::taskbar::new_desktop(),
                TaskViewAction::TaskView => opentaskbar_shell::taskbar::open_task_view(),
                TaskViewAction::None => {}
            }
            return;
        }

        if let Some(idx) = self.hovered_idx
            && let Some(slot) = self.slots.get(idx).cloned()
        {
            let running = !slot.windows.is_empty();
            let window = slot.windows.first().map(|window| window.hwnd);
            // Ctrl+right-click reaches the target application's own menu via the
            // WS-B wrappers instead of our dock menu.
            if win32::is_ctrl_down()
                && let Some(target) = window
                && self.show_native_app_menu(target)
            {
                return;
            }
            match win32::show_app_context_menu(
                self.hwnd,
                self.is_dark,
                &slot.label,
                &slot.launch_path,
                slot.pinned,
                running,
                &self.settings,
                self.dock_menu_anchor(),
            ) {
                AppContextAction::Open => {
                    let identities: Vec<(usize, u32)> =
                        slot.windows.iter().map(|w| (w.hwnd, w.pid)).collect();
                    if !tracker::activate_or_toggle_verified(&identities) {
                        pins::launch_app(slot.launch_path, window, false);
                    }
                }
                AppContextAction::NewWindow => pins::launch_app(slot.launch_path, window, false),
                AppContextAction::RunAsAdmin => pins::launch_app(slot.launch_path, window, true),
                AppContextAction::TogglePin => self.toggle_pin_for_slot(idx),
                AppContextAction::ToggleGroup(gidx) => {
                    // WS-10: clicking a group this app already belongs to opens
                    // its flyout; each row can then show the window picker for
                    // the matching live windows. Clicking a foreign group still
                    // adds this app, and the flyout's Remove button undoes it.
                    let group_has_slot = self.settings.app_groups.get(gidx).is_some_and(|group| {
                        group.executable_paths.len() > 1
                            && group
                                .executable_paths
                                .iter()
                                .any(|path| path.eq_ignore_ascii_case(&slot.launch_path))
                    });
                    if group_has_slot {
                        self.open_group_flyout(gidx, &slot.launch_path);
                        return;
                    }
                    if let Some(group) = self.settings.app_groups.get_mut(gidx) {
                        if let Some(pos) = group
                            .executable_paths
                            .iter()
                            .position(|p| p.eq_ignore_ascii_case(&slot.launch_path))
                        {
                            group.executable_paths.remove(pos);
                            if group.executable_paths.is_empty() {
                                self.settings.app_groups.remove(gidx);
                            }
                        } else {
                            group.executable_paths.push(slot.launch_path.clone());
                        }
                        let _ = settings::save_to(&settings::settings_path(), &self.settings);
                        self.reload_settings_live();
                    }
                }
                AppContextAction::CreateGroup => {
                    let title = loc::t("NewGroup");
                    let label = loc::t("GroupNamePrompt");
                    if let Some(name) =
                        crate::ui::prompt::ask_name(self.hwnd, self.is_dark, &title, &label, "")
                    {
                        if let Some(existing) = self
                            .settings
                            .app_groups
                            .iter_mut()
                            .find(|g| g.name.eq_ignore_ascii_case(&name))
                        {
                            if !existing
                                .executable_paths
                                .iter()
                                .any(|p| p.eq_ignore_ascii_case(&slot.launch_path))
                            {
                                existing.executable_paths.push(slot.launch_path.clone());
                            }
                        } else {
                            let id = name
                                .to_lowercase()
                                .chars()
                                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                                .collect::<String>();
                            self.settings.app_groups.push(
                                opentaskbar_core::settings::DockAppGroup {
                                    id,
                                    name,
                                    executable_paths: vec![slot.launch_path.clone()],
                                },
                            );
                        }
                        let _ = settings::save_to(&settings::settings_path(), &self.settings);
                        self.reload_settings_live();
                    }
                }
                AppContextAction::Close => {
                    for w in &slot.windows {
                        tracker::close_window_verified(w.hwnd, w.pid);
                    }
                }
                AppContextAction::Terminate => {
                    if let Some(first) = slot.windows.first() {
                        tracker::terminate_window_process_verified(
                            first.hwnd,
                            first.pid,
                            &slot.launch_path,
                        );
                    }
                }
                AppContextAction::Dock(act) => self.execute_context_action(act),
                AppContextAction::None => {}
            }
            return;
        }

        let (_, act) = win32::show_dock_context_menu_with_entries(
            self.hwnd,
            self.is_dark,
            &self.settings,
            Vec::new(),
            self.dock_menu_anchor(),
        );
        self.execute_context_action(act);
    }

    fn execute_context_action(&mut self, action: ContextAction) {
        match action {
            ContextAction::Search => {
                self.request_flyout_anchor(FlyoutKind::Search);
                opentaskbar_shell::taskbar::open_search();
            }
            ContextAction::ApplyProfile(idx) => {
                if let Some(prof) = self.settings.profiles.get(idx).cloned() {
                    self.settings.position = prof.position;
                    self.settings.size = prof.size;
                    self.settings.auto_hide = prof.auto_hide;
                    self.settings.enabled_widgets = prof.enabled_widgets.clone();
                    // Canonical, case-insensitive match: profiles written by the
                    // reference app use `Clock` / `SystemStats`, older port builds
                    // `clock` / `stats`.
                    let (show_clock, show_stats) = profile_widget_flags(&prof.enabled_widgets);
                    self.settings.show_clock_widget = show_clock;
                    self.settings.show_system_stats_widget = show_stats;
                    self.settings.active_profile = Some(prof.name);
                    let _ = settings::save_to(&settings::settings_path(), &self.settings);
                    self.reload_settings_live();
                }
            }
            ContextAction::SaveNewProfile => {
                let title = loc::t("Profiles");
                let label = loc::t("ProfileNamePrompt");
                if let Some(name) =
                    crate::ui::prompt::ask_name(self.hwnd, self.is_dark, &title, &label, "")
                {
                    let snap = opentaskbar_core::settings::DockProfile {
                        name: name.clone(),
                        position: self.settings.position,
                        size: self.settings.size,
                        auto_hide: self.settings.auto_hide,
                        enabled_widgets: self.settings.enabled_widgets.clone(),
                        ..Default::default()
                    };
                    self.settings
                        .profiles
                        .retain(|p| !p.name.eq_ignore_ascii_case(&name));
                    self.settings.profiles.push(snap);
                    self.settings.active_profile = Some(name);
                    let _ = settings::save_to(&settings::settings_path(), &self.settings);
                    self.reload_settings_live();
                }
            }
            ContextAction::AddStack(folder) => {
                if let Some(base) = std::env::var_os("USERPROFILE") {
                    let path = std::path::PathBuf::from(base).join(folder);
                    if path.is_dir() {
                        let p = path.to_string_lossy().into_owned();
                        if !self
                            .settings
                            .stack_folders
                            .iter()
                            .any(|x| x.eq_ignore_ascii_case(&p))
                        {
                            self.settings.stack_folders.push(p);
                            let _ = settings::save_to(&settings::settings_path(), &self.settings);
                            self.reload_settings_live();
                        }
                    }
                }
            }
            ContextAction::AddCustomFolderStack => self.add_custom_folder_stack(),
            ContextAction::RemoveStack(idx) => {
                if idx < self.settings.stack_folders.len() {
                    self.settings.stack_folders.remove(idx);
                    let _ = settings::save_to(&settings::settings_path(), &self.settings);
                    self.reload_settings_live();
                }
            }
            ContextAction::ToggleFocusMode => {
                self.settings.focus_mode = !self.settings.focus_mode;
                let _ = opentaskbar_core::settings::save_to(
                    &opentaskbar_core::settings::settings_path(),
                    &self.settings,
                );
                self.reload_settings_live();
            }
            ContextAction::Settings => settings_ui::spawn_settings_window(),
            ContextAction::TaskManager => pins::launch("taskmgr.exe".to_owned()),
            ContextAction::FileExplorer => pins::launch("explorer.exe".to_owned()),
            ContextAction::Terminal => pins::launch("wt.exe".to_owned()),
            ContextAction::TerminalAdmin => pins::launch_elevated("wt.exe".to_owned()),
            ContextAction::RunDialog => opentaskbar_shell::taskbar::open_run_dialog(),
            ContextAction::Notifications => {
                self.request_flyout_anchor(FlyoutKind::ControlCenter);
                opentaskbar_shell::taskbar::open_notifications();
            }
            ContextAction::DeviceManager => pins::launch("devmgmt.msc".to_owned()),
            ContextAction::DiskManagement => pins::launch("diskmgmt.msc".to_owned()),
            ContextAction::ComputerManagement => pins::launch("compmgmt.msc".to_owned()),
            ContextAction::EventViewer => pins::launch("eventvwr.msc".to_owned()),
            ContextAction::NetworkConnections => pins::launch("ncpa.cpl".to_owned()),
            ContextAction::LockWorkstation => {
                pins::launch_with_args(
                    "rundll32.exe".to_owned(),
                    "user32.dll,LockWorkStation".to_owned(),
                );
            }
            ContextAction::SignOut => {
                pins::launch_with_args("shutdown.exe".to_owned(), "/l".to_owned());
            }
            ContextAction::RestartComputer => {
                pins::launch_with_args("shutdown.exe".to_owned(), "/r /t 0".to_owned());
            }
            ContextAction::ShutDownComputer => {
                pins::launch_with_args("shutdown.exe".to_owned(), "/s /t 0".to_owned());
            }
            ContextAction::SleepComputer => {
                // bHibernate=0 (sleep/S3), bForce=1, bWakeupEventsDisabled=0.
                pins::launch_with_args(
                    "rundll32.exe".to_owned(),
                    "powrprof.dll,SetSuspendState 0,1,0".to_owned(),
                );
            }
            ContextAction::ShowDesktop => opentaskbar_shell::taskbar::show_desktop(),
            ContextAction::RestoreTaskbar => opentaskbar_shell::taskbar::restore_from_journal(),
            ContextAction::Exit => {
                opentaskbar_shell::taskbar::end_replacement();
                unsafe { PostQuitMessage(0) };
            }
            ContextAction::None => {}
        }
    }
}

impl DockState {
    /// Opens keyboard navigation from the dock hotkey. The dock itself is
    /// `WS_EX_NOACTIVATE`, so the focus ring is owned by the dock state rather
    /// than by Win32 focus; Enter activates the highlighted slot.
    fn start_keyboard_navigation(&mut self) {
        if self.slots.is_empty() {
            return;
        }
        let idx = self.keyboard_idx.map_or(0, |i| (i + 1) % self.slots.len());
        self.keyboard_idx = Some(idx);
        // Resting center (P1-4): a shifted live center could land between two
        // rest cells and make keyboard focus skip an icon.
        self.begin_hover_session();
        let center = self.rest_slot_center_x(idx);
        self.hover_mouse_x = Some(center);
        self.fisheye_anchor_x = Some(center);
        self.update_hovered_index_from_mouse();
        self.sync_preview_hover();
        self.arm_win_key_reveal();
        self.ensure_animation();
        self.evaluate_auto_hide();
        self.update_visuals();
    }

    fn activate_keyboard_slot(&mut self) {
        let Some(idx) = self.keyboard_idx else {
            return;
        };
        let Some(slot) = self.slots.get_mut(idx) else {
            return;
        };
        slot.bounce_started = Some(Instant::now());
        let slot_windows = slot.windows.clone();
        let identities: Vec<(usize, u32)> = slot.windows.iter().map(|w| (w.hwnd, w.pid)).collect();
        let launch_path = slot.launch_path.clone();
        let hwnds = slot.hwnds();
        self.ensure_animation();
        self.update_visuals();
        // Keyboard activation of a secondary dock slot uses the same picker path.
        let picker_opened = self.show_picker_for_windows(&slot_windows, &launch_path);
        if !picker_opened && !tracker::activate_or_toggle_verified(&identities) {
            pins::launch_app(launch_path, hwnds.first().copied(), false);
        }
    }

    /// Keyboard navigation over dock items: Tab/arrows move focus, Escape closes any
    /// transient island/flyout/stack and returns focus to the originating dock item.
    fn handle_key_down(&mut self, vk: u16) {
        match vk {
            VK_LWIN | VK_RWIN => {
                // `arm_win_key_reveal` applies the same gate as the low-level hook.
                self.arm_win_key_reveal();
                if win_key_reveal_enabled(
                    self.settings.reveal_dock_on_win_key,
                    self.settings.auto_hide,
                ) {
                    self.target_hidden = false;
                    self.ensure_animation();
                }
                self.evaluate_auto_hide();
            }
            VK_ESCAPE => {
                let mut closed_transient =
                    escape_closes_transient(false, false, self.island.expanded);
                if closed_transient {
                    self.island.pre_process_input(IslandInput::Escape);
                } else if self.island.pre_process_input(IslandInput::Escape)
                    == InputDisposition::PassThrough
                {
                    self.hide_preview_now();
                    self.keyboard_idx = None;
                    closed_transient = true;
                }
                if closed_transient && self.flyout_focus_return != 0 {
                    let start_hwnd = crate::platform::start_menu_state::start_window();
                    restore_app_foreground(self.flyout_focus_return, start_hwnd);
                    self.flyout_focus_return = 0;
                }
                self.evaluate_auto_hide();
                self.update_visuals();
            }
            VK_TAB | VK_LEFT | VK_RIGHT => {
                let reverse = vk == VK_LEFT || (vk == VK_TAB && win32::is_shift_down());
                if self.island.expanded
                    && self.island.pre_process_input(if reverse {
                        IslandInput::Left
                    } else {
                        IslandInput::Right
                    }) == InputDisposition::Consumed
                {
                    return;
                }
                let len = self.slots.len();
                let next = keyboard_next_index(self.keyboard_idx, len, reverse);
                self.keyboard_idx = next;
                if let Some(idx) = next {
                    self.begin_hover_session();
                    let mx = self.rest_slot_center_x(idx);
                    self.hover_mouse_x = Some(mx);
                    self.fisheye_anchor_x = Some(mx);
                    self.update_hovered_index_from_mouse();
                    self.sync_preview_hover();
                    self.ensure_animation();
                    self.session_max_scale = layout::HOVER_SCALE;
                }
                self.update_visuals();
            }
            VK_RETURN => self.activate_keyboard_slot(),
            _ => {}
        }
    }

    /// After a secure-desktop session switch the cached foreground/Start state is
    /// stale. Re-sample it on the UI thread so the next Start click restores to
    /// the correct application instead of the lock screen.
    fn resync_foreground_after_session(&mut self) {
        let foreground = unsafe { GetForegroundWindow() } as usize;
        let start_hwnd = crate::platform::start_menu_state::start_window();
        self.flyout_focus_return = restore_target_for_press(foreground, start_hwnd, self.hwnd);
        self.start_restore_hwnd = self.flyout_focus_return;
        self.pressed_shell_start_was_open = crate::platform::start_menu_state::is_open();
        self.keyboard_idx = None;
        // Refresh WS-B's cached Start window without walking windows here:
        // a dock activation note is the same signal the normal click path uses.
        if crate::platform::start_menu_state::is_open() {
            crate::platform::start_menu_state::on_dock_activated();
        }
        self.overlap_dirty = true;
        self.evaluate_auto_hide();
    }

    /// Session switch / lock resets all transient UI (`App.xaml.cs` SessionSwitch).
    fn reset_transient_ui(&mut self) {
        window_picker::request_close_all();
        self.island.reset_transient();
        self.keyboard_idx = None;
        self.drag = None;
        self.resize_drag = None;
        self.scroll_pan = None;
        self.drag_hover_started = None;
        self.session_max_scale = layout::HOVER_SCALE;
        self.hide_preview_now();
        self.hovered_shell = None;
        self.hovered_tray = false;
        self.hovered_tray_pin = None;
        self.hovered_bin = false;
        self.hovered_stack = None;
        self.pressed_stack = None;
        self.stack_hover_x = None;
        self.hover_rest_sm = None;
        self.fisheye_anchor_x = None;
        self.hovered_widget = None;
        self.hovered_widget_sub = None;
        self.disarm_hover_intent();
        self.widget_runtime.hover.reset();
        self.hover_flyout_pending = None;
        self.hover_flyout_open = false;
        self.hover_capture_id = None;
        self.hovered_scroll = None;
        self.hovered_resize = None;
        self.hover_mouse_x = None;
        self.hovered_idx = None;
        self.last_mouse_move = None;
        self.win_key_reveal_until = None;
        // Session switch/lock is a genuine shell change: request a rescan, still
        // subject to the 1 s minimum interval.
        self.overlap_dirty = true;
        self.evaluate_auto_hide();
        self.ensure_animation();
        self.update_visuals();
    }
}

impl Drop for DockState {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let (lock, cvar) = &*self.anim_signal;
        if let Ok(mut guard) = lock.lock() {
            *guard = true;
            cvar.notify_all();
        }
    }
}

thread_local! {
    /// HWND currently owning the two global hotkeys (only the primary dock registers).
    static HOTKEY_OWNER: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Re-registers the launcher/dock hotkeys for the primary dock window. Bindings that
/// fail (another app owns the combination) stay pending and retry on the next reload.
fn register_global_hotkeys(hwnd: HWND, settings: &DockSettings) {
    let previous = HOTKEY_OWNER.replace(hwnd as usize);
    unsafe {
        if previous != 0 && previous != hwnd as usize {
            UnregisterHotKey(previous as HWND, HOTKEY_LAUNCHER_ID);
            UnregisterHotKey(previous as HWND, HOTKEY_DOCK_ID);
        }
        UnregisterHotKey(hwnd, HOTKEY_LAUNCHER_ID);
        UnregisterHotKey(hwnd, HOTKEY_DOCK_ID);

        let launcher = settings.launcher_binding();
        let dock = settings.dock_binding();
        let no_repeat = opentaskbar_core::hotkey::MOD_NOREPEAT;
        if let Some(binding) = &launcher
            && RegisterHotKey(
                hwnd,
                HOTKEY_LAUNCHER_ID,
                binding.modifiers | no_repeat,
                binding.virtual_key,
            ) == 0
        {
            opentaskbar_core::log::warn(format!(
                "RegisterHotKey launcher ({}) failed: {}",
                binding.display,
                windows_sys::Win32::Foundation::GetLastError()
            ));
        }
        if let Some(binding) = &dock
            && launcher.as_ref() != Some(binding)
            && RegisterHotKey(
                hwnd,
                HOTKEY_DOCK_ID,
                binding.modifiers | no_repeat,
                binding.virtual_key,
            ) == 0
        {
            opentaskbar_core::log::warn(format!(
                "RegisterHotKey dock ({}) failed: {}",
                binding.display,
                windows_sys::Win32::Foundation::GetLastError()
            ));
        }
    }
}

#[repr(C)]
struct KbdLlHookStruct {
    vk_code: u32,
    scan_code: u32,
    flags: u32,
    time: u32,
    extra_info: usize,
}

/// Detects a standalone Windows-key tap (Win up with no intervening key) and asks
/// every dock window to reveal itself. Mirrors `WinKeyRevealMonitor` (#164).
unsafe extern "system" fn win_key_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msg = wparam as u32;
    if code >= 0
        && lparam != 0
        && matches!(msg, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP)
    {
        let info = unsafe { &*(lparam as *const KbdLlHookStruct) };
        // Read the previous tap state, then clear it unconditionally BEFORE the
        // injected/modifier/gate checks: a rejected key must never leave a
        // stale "Win alone" flag behind (including while the gate is off).
        let was_alone = WIN_KEY_ALONE.with(std::cell::Cell::get);
        WIN_KEY_ALONE.with(|flag| flag.set(false));
        // SAFETY: GetAsyncKeyState takes a virtual-key code and is safe to call
        // from a low-level hook; the high bit marks a held modifier.
        let ctrl_down = unsafe { GetAsyncKeyState(VK_CONTROL as i32) };
        let shift_down = unsafe { GetAsyncKeyState(VK_SHIFT as i32) };
        let modifier = win_key_modifier_pressed(info.flags, ctrl_down, shift_down);
        let injected = is_injected_key_event(info.flags);
        let gate = WIN_KEY_REVEAL_GATE.load(Ordering::Relaxed);
        if !win_key_reveal_allowed(gate, injected, modifier) {
            return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
        }
        // Alt is covered by LLKHF_ALTDOWN in `info.flags`.
        let is_win = info.vk_code == VK_LWIN as u32 || info.vk_code == VK_RWIN as u32;
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                WIN_KEY_ALONE.with(|flag| flag.set(is_win));
            }
            _ => {
                if is_win && was_alone {
                    DOCK_HWNDS.with(|list| {
                        for hwnd in list.borrow().iter() {
                            unsafe {
                                let _ = PostMessageW(*hwnd as HWND, WM_APP_WINKEY_REVEAL, 0, 0);
                            }
                        }
                    });
                }
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

thread_local! {
    static WIN_KEY_ALONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Prevents re-entrant `&mut DockState` aliasing when modal flyouts/menus or
    /// synchronous Win32 callbacks (`SetWindowPos`) dispatch messages back to the
    /// *same* dock window. HWND-keyed so a second dock on this UI thread is not
    /// blocked by the first one's modal loop.
    static DOCK_WNDPROC_ACTIVE: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// A VSync tick re-entering the guard is re-posted at most once; a second
    /// re-entry must be dropped instead of ping-ponging forever, and recorded here
    /// so the outermost `DockWndProcGuard::drop` delivers one recovery tick and
    /// `tick_pending` can never wedge `true` after a modal menu or flyout.
    static VSYNC_REPOST_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static VSYNC_DROPPED_HWNDS: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Active dock window handles on this UI thread for `WH_MOUSE_LL` wheel routing.
    static DOCK_HWNDS: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Dynamic Island windows that also receive the hook's wheel route. Kept apart
    /// from `DOCK_HWNDS` so islands never get the dock's private `WM_APP` messages.
    static ISLAND_WHEEL_HWNDS: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Registers a Dynamic Island window (created on this dock UI thread) for
/// `WH_MOUSE_LL` wheel routing, so wheel input reaches the `WS_EX_NOACTIVATE`
/// island even when Windows "scroll inactive windows" is off.
pub(super) fn register_island_wheel_hwnd(hwnd: usize) {
    ISLAND_WHEEL_HWNDS.with(|list| {
        let mut list = list.borrow_mut();
        if hwnd != 0 && !list.contains(&hwnd) {
            list.push(hwnd);
        }
    });
}

pub(super) fn unregister_island_wheel_hwnd(hwnd: usize) {
    ISLAND_WHEEL_HWNDS.with(|list| list.borrow_mut().retain(|&h| h != hwnd));
}

const EVENT_SYSTEM_ALERT: u32 = 0x0010;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
const OBJID_WINDOW: i32 = 0;

type WinEventProc = unsafe extern "system" fn(HANDLE, u32, HWND, i32, i32, u32, u32);

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWinEventHook(
        event_min: u32,
        event_max: u32,
        hmod: HMODULE,
        callback: Option<WinEventProc>,
        id_process: u32,
        id_thread: u32,
        flags: u32,
    ) -> HANDLE;
    fn UnhookWinEvent(hook: HANDLE) -> i32;
}

/// Alert queue and per-executable counts shared by the WinEvent callback and
/// the dock UI threads. The callback only records a path and posts a message;
/// no badge surface is created off-thread.
static NOTIFICATION_ALERTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static NOTIFICATION_COUNTS: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

/// Resolves the executable behind an alert window once, on the hook thread.
fn exe_path_from_hwnd(hwnd: HWND) -> String {
    let mut pid = 0u32;
    unsafe {
        if hwnd.is_null() || GetWindowThreadProcessId(hwnd, &mut pid) == 0 || pid == 0 {
            return String::new();
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 || len == 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buf[..len as usize])
        }
    }
}

unsafe extern "system" fn notification_alert_hook(
    _hook: HANDLE,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _time_ms: u32,
) {
    if id_object != OBJID_WINDOW || hwnd.is_null() {
        return;
    }
    let path = exe_path_from_hwnd(hwnd);
    if path.is_empty() {
        return;
    }
    if let Ok(mut counts) = NOTIFICATION_COUNTS.lock() {
        if let Some(entry) = counts.iter_mut().find(|(exe, _)| exe == &path) {
            entry.1 = layout::notification_badge_count(entry.1, 1);
        } else {
            counts.push((path.clone(), 1));
        }
    }
    if let Ok(mut queue) = NOTIFICATION_ALERTS.lock() {
        queue.push(path);
    }
    DOCK_HWNDS.with(|list| {
        for dock in list.borrow().iter() {
            unsafe {
                let _ = PostMessageW(*dock as HWND, WM_APP_NOTIFICATION_BADGE, 0, 0);
            }
        }
    });
}

fn install_notification_alert_hook() -> HANDLE {
    unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_ALERT,
            EVENT_SYSTEM_ALERT,
            std::ptr::null_mut(),
            Some(notification_alert_hook),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    }
}

/// Routes `WM_MOUSEWHEEL` / `WM_MOUSEHWHEEL` to unfocused `WS_EX_NOACTIVATE` dock windows
/// when the pointer is directly over a dock pill, preventing wheel events from leaking to
/// background windows behind the dock.
unsafe extern "system" fn mouse_wheel_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msg = wparam as u32;
    if code >= 0 && (msg == WM_MOUSEWHEEL || msg == WM_MOUSEHWHEEL) && lparam != 0 {
        // Low-level hooks must not call WindowFromPoint (cross-thread / DPI
        // ambiguity). Post the raw coordinates + delta and let the UI thread
        // hit-test its own dock rects in the normal message pump.
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let delta = ((info.mouseData >> 16) & 0xffff) as u16;
        let payload = ((delta as usize) << 1) | usize::from(msg == WM_MOUSEHWHEEL);
        let point = layout::pack_cursor_point(info.pt.x, info.pt.y);
        DOCK_HWNDS.with(|list| {
            for hwnd in list.borrow().iter() {
                unsafe {
                    let _ =
                        PostMessageW(*hwnd as HWND, WM_APP_WHEEL_ROUTE, payload, point as LPARAM);
                }
            }
        });
        ISLAND_WHEEL_HWNDS.with(|list| {
            for hwnd in list.borrow().iter() {
                unsafe {
                    let _ = PostMessageW(
                        *hwnd as HWND,
                        dynamic_island::WM_APP_ISLAND_WHEEL_ROUTE,
                        payload,
                        point as LPARAM,
                    );
                }
            }
        });
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

struct DockWndProcGuard(usize);

impl DockWndProcGuard {
    fn try_enter(hwnd: usize) -> Option<Self> {
        DOCK_WNDPROC_ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if active.contains(&hwnd) {
                None
            } else {
                active.push(hwnd);
                Some(Self(hwnd))
            }
        })
    }
}

fn mark_vsync_dropped(hwnd: usize) {
    VSYNC_DROPPED_HWNDS.with(|list| {
        let mut list = list.borrow_mut();
        if !list.contains(&hwnd) {
            list.push(hwnd);
        }
    });
}

fn take_vsync_dropped(hwnd: usize) -> bool {
    VSYNC_DROPPED_HWNDS.with(|list| {
        let mut list = list.borrow_mut();
        if let Some(pos) = list.iter().position(|&h| h == hwnd) {
            list.swap_remove(pos);
            true
        } else {
            false
        }
    })
}

impl Drop for DockWndProcGuard {
    fn drop(&mut self) {
        let hwnd = self.0;
        DOCK_WNDPROC_ACTIVE.with(|active| {
            active.borrow_mut().retain(|&h| h != hwnd);
        });
        if take_vsync_dropped(hwnd) {
            VSYNC_REPOST_PENDING.with(|pending| pending.set(false));
            unsafe {
                let _ = PostMessageW(hwnd as HWND, WM_APP_VSYNC, 0, 0);
            }
        }
    }
}

const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOGOFF: u32 = 2;
const WTS_SESSION_LOCK: u32 = 7;
const WTS_SESSION_UNLOCK: u32 = 8;

#[link(name = "wtsapi32")]
unsafe extern "system" {
    fn WTSRegisterSessionNotification(hWnd: HWND, dwFlags: u32) -> bool;
    fn WTSUnRegisterSessionNotification(hWnd: HWND) -> bool;
}

/// `CF_HDROP` is placed on the clipboard by Explorer's OLE file drag. Checking it
/// once when a dock drag becomes active keeps the 450 ms bring-to-front gesture
/// scoped to external file drags rather than ordinary icon reordering.
const CF_HDROP: u32 = 15;

#[link(name = "user32")]
unsafe extern "system" {
    fn IsClipboardFormatAvailable(format: u32) -> i32;
}

fn external_file_drag_active() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_HDROP) != 0 }
}

const FO_DELETE: u32 = 3;
const FOF_SILENT: u16 = 0x0004;
const FOF_NOCONFIRMATION: u16 = 0x0010;
const FOF_ALLOWUNDO: u16 = 0x0040;
const FOF_NOERRORUI: u16 = 0x0400;

#[repr(C)]
struct ShFileOpStructW {
    hwnd: HWND,
    w_func: u32,
    p_from: *const u16,
    p_to: *const u16,
    f_flags: u16,
    f_any_operations_aborted: i32,
    h_name_mappings: *mut std::ffi::c_void,
    lpsz_progress_title: *const u16,
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHFileOperationW(op: *mut ShFileOpStructW) -> i32;
}

/// Moves dropped files to the Recycle Bin (`DesktopFileTransfer/Drop.cs`).
fn delete_files_to_recycle_bin(owner_hwnd: usize, paths: &[String]) -> bool {
    let mut from: Vec<u16> = Vec::new();
    for path in paths {
        from.extend(path.encode_utf16());
        from.push(0);
    }
    from.push(0); // SHFileOperationW requires a second NUL terminator.
    let mut op = ShFileOpStructW {
        hwnd: owner_hwnd as HWND,
        w_func: FO_DELETE,
        p_from: from.as_ptr(),
        p_to: std::ptr::null(),
        f_flags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI,
        f_any_operations_aborted: 0,
        h_name_mappings: std::ptr::null_mut(),
        lpsz_progress_title: std::ptr::null(),
    };
    unsafe { SHFileOperationW(&mut op) == 0 && op.f_any_operations_aborted == 0 }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(_guard) = DockWndProcGuard::try_enter(hwnd as usize) else {
        // A reentrant call must not consume the VSync pending flag: it is cleared
        // only in the normal WM_APP_VSYNC handler. Re-post at most once so the
        // VSync producer cannot stall with `tick_pending` stuck true, and a
        // re-post loop can never spin forever.
        if msg == WM_APP_VSYNC {
            let repost = VSYNC_REPOST_PENDING
                .with(|pending| layout::vsync_repost_allowed(pending.replace(true)));
            if repost {
                unsafe {
                    let _ = PostMessageW(hwnd, msg, wparam, lparam);
                }
            } else {
                // Record the dropped VSync tick so exiting the modal guard recovers
                // `tick_pending` with one fresh WM_APP_VSYNC instead of stalling permanently.
                mark_vsync_dropped(hwnd as usize);
            }
        }
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DockState };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };

    if msg != 0 && msg == state.reload_msg {
        if wparam != state.hwnd {
            state.reload_settings_live();
        }
        return 0;
    }

    match msg {
        WM_NCHITTEST => {
            let mut pt = POINT {
                x: loword_i16(lparam) as i32,
                y: hiword_i16(lparam) as i32,
            };
            unsafe {
                ScreenToClient(hwnd, &mut pt);
            }
            let mx = pt.x as f32 / state.dpi;
            let my = pt.y as f32 / state.dpi;
            if state.hit_test_dips(mx, my) {
                HTCLIENT as LRESULT
            } else {
                HTTRANSPARENT as LRESULT
            }
        }
        WM_SETCURSOR => {
            let cursor_id = if state.hovered_resize.is_some() || state.resize_drag.is_some() {
                IDC_SIZEWE
            } else if state.hovered_shell.is_some()
                || state.hovered_idx.is_some()
                || state.hovered_tray
                || state.hovered_tray_pin.is_some()
                || state.hovered_bin
                || state.hovered_stack.is_some()
                || state.hovered_widget.is_some()
                || state.hovered_scroll.is_some()
            {
                IDC_HAND
            } else {
                IDC_ARROW
            };
            unsafe {
                SetCursor(LoadCursorW(std::ptr::null_mut(), cursor_id));
            }
            1
        }
        WM_LBUTTONDBLCLK => {
            let mx = loword_i16(lparam) / state.dpi;
            let my = hiword_i16(lparam) / state.dpi;
            if state.hit_resize_edge(mx, my).is_some() {
                state.settings.dock_custom_width_dips = None;
                let _ = opentaskbar_core::settings::save_to(
                    &opentaskbar_core::settings::settings_path(),
                    &state.settings,
                );
                state.sync_window_bounds();
                state.update_visuals();
            }
            0
        }
        WM_MOUSEMOVE => {
            let mx_px = loword_i16(lparam);
            let my_px = hiword_i16(lparam);
            if let Some(ref mut glass) = state.spatial_glass {
                glass.set_pointer(mx_px, my_px);
            }
            let mx = mx_px / state.dpi;
            let my = my_px / state.dpi;
            // 1000 Hz mice repeat the same point many times; hit testing and
            // animation wake-ups must run only for a genuinely new coordinate.
            if layout::mouse_move_coalesce(&mut state.last_mouse_move, mx, my) {
                // Re-entry cancels the pending 400 ms delayed hide.
                unsafe { KillTimer(hwnd, TIMER_HIDE_DELAY) };
                state.handle_mouse_move(mx, my);
            }
            // Still drain a pending flyout armed by the hover-intent timer.
            state.open_pending_hover_flyout();
            0
        }
        WM_MOUSELEAVE => {
            if let Some(ref mut glass) = state.spatial_glass {
                glass.clear_pointer();
            }
            state.handle_mouse_leave();
            0
        }
        WM_LBUTTONDOWN => {
            let mx = loword_i16(lparam) / state.dpi;
            let my = hiword_i16(lparam) / state.dpi;
            state.handle_lbutton_down(mx, my);
            // The dock can take focus while WS_EX_NOACTIVATE suppresses the
            // foreground hook; tell WS-B so Start's cached state is refreshed
            // (after the down snapshot was taken). Gate on cached state so the
            // live validation inside the API never runs on an ordinary click.
            if crate::platform::start_menu_state::is_open() {
                crate::platform::start_menu_state::on_dock_activated();
            }
            0
        }
        WM_LBUTTONUP => {
            let mx = loword_i16(lparam) / state.dpi;
            let my = hiword_i16(lparam) / state.dpi;
            state.handle_lbutton_up(mx, my);
            state.finish_widget_pointer_capture();
            state.reconcile_pointer_after_interaction();
            0
        }
        WM_MBUTTONDOWN => {
            let mx = loword_i16(lparam) / state.dpi;
            state.handle_mbutton_down(mx);
            0
        }
        WM_MBUTTONUP => {
            state.handle_mbutton_up();
            0
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let horizontal = msg == WM_MOUSEHWHEEL;
            let delta = ((wparam >> 16) & 0xffff) as i16;
            // Wheel messages carry *screen* coordinates in lParam.
            let mut pt = POINT {
                x: loword_i16(lparam) as i32,
                y: hiword_i16(lparam) as i32,
            };
            unsafe {
                ScreenToClient(hwnd, &mut pt);
            }
            let mx = pt.x as f32 / state.dpi;
            let my = pt.y as f32 / state.dpi;
            // The `WH_MOUSE_LL` route usually delivers the same notch first:
            // one physical notch must fire exactly once.
            if state.wheel_dedup.accept(
                layout::WheelSource::Window,
                delta,
                horizontal,
                Instant::now(),
            ) {
                state.handle_mouse_wheel(mx, my, delta, horizontal);
            }
            0
        }
        WM_RBUTTONDOWN => {
            state.disarm_hover_intent();
            state.hover_flyout_pending = None;
            if let Some(wid) = state.hovered_widget {
                state.widget_runtime.hover.suppress(wid, Instant::now());
            }
            0
        }
        WM_RBUTTONUP => {
            let mx = loword_i16(lparam) / state.dpi;
            let my = hiword_i16(lparam) / state.dpi;
            state.handle_rbutton_up(mx, my);
            if crate::platform::start_menu_state::is_open() {
                crate::platform::start_menu_state::on_dock_activated();
            }
            state.reconcile_pointer_after_interaction();
            0
        }
        WM_DROPFILES => {
            state.handle_drop_files(wparam);
            0
        }
        WM_APP_VSYNC => {
            VSYNC_REPOST_PENDING.with(|pending| pending.set(false));
            state.tick_pending.store(false, Ordering::Release);
            let moving = state.step_animations(Instant::now());
            state.update_visuals();
            let effective = effective_animations_enabled(
                state.settings.animations_enabled,
                state.settings.focus_mode,
            );
            if !vsync_tick_allowed(effective, moving) {
                state.set_animating(false);
            }
            0
        }
        WM_APP_ICONS => {
            state.drain_loaded_icons();
            0
        }
        WM_APP_PREVIEW_LEAVE => {
            if state.hovered_idx.is_none() {
                state.schedule_preview_hide();
            }
            state.evaluate_auto_hide();
            0
        }
        WM_APP_WINKEY_REVEAL => {
            if WIN_KEY_REVEAL_GATE.load(Ordering::Relaxed) {
                state.arm_win_key_reveal();
            }
            state.evaluate_auto_hide();
            0
        }
        WM_APP_SETTINGS_RELOAD => {
            // Idle half: save + reload only once per coalesced request.
            state.flush_deferred_settings_reload();
            0
        }
        WM_APP_WHEEL_ROUTE => {
            // Coordinate-only wheel route: hit-test on this UI thread against
            // the dock's own rects (never from the low-level hook).
            let horizontal = (wparam & 1) != 0;
            let delta = (wparam >> 1) as u32 as i16;
            let (x, y) = layout::unpack_cursor_point(lparam);
            let mut pt = POINT { x, y };
            unsafe {
                ScreenToClient(hwnd, &mut pt);
            }
            let mx = pt.x as f32 / state.dpi;
            let my = pt.y as f32 / state.dpi;
            if state.hit_test_dips(mx, my)
                && state.wheel_dedup.accept(
                    layout::WheelSource::Hook,
                    delta,
                    horizontal,
                    Instant::now(),
                )
            {
                state.handle_mouse_wheel(mx, my, delta, horizontal);
            }
            0
        }
        WM_APP_NOTIFICATION_BADGE => {
            state.drain_notification_badges();
            0
        }
        WM_APP_SHELL_FLYOUT => {
            let kind = flyout_kind_from_code(wparam);
            // Unknown flyout HWND: pass 0 and let the reposition worker discover
            // the Search / Control Center window.
            crate::on_shell_flyout_opened(kind, lparam as usize);
            0
        }
        WM_HOTKEY => {
            match wparam as i32 {
                HOTKEY_LAUNCHER_ID => {
                    state.request_flyout_anchor(FlyoutKind::Search);
                    opentaskbar_shell::taskbar::open_search();
                }
                HOTKEY_DOCK_ID => {
                    // The dock hotkey starts keyboard navigation (Tab / arrows move
                    // between slots, Enter launches, Escape returns focus).
                    state.start_keyboard_navigation();
                }
                _ => {}
            }
            0
        }
        WM_KEYDOWN => {
            state.handle_key_down(wparam as u16);
            0
        }
        WM_WTSSESSION_CHANGE => {
            let event = wparam as u32;
            if matches!(event, WTS_SESSION_LOGOFF | WTS_SESSION_LOCK) {
                // A keyup lost across the secure desktop can leave a synthetic
                // modifier held down; release before the session is suspended.
                opentaskbar_shell::taskbar::release_held_keys();
            }
            if matches!(
                event,
                WTS_SESSION_LOGOFF | WTS_SESSION_LOCK | WTS_SESSION_UNLOCK
            ) {
                state.reset_transient_ui();
            }
            if event == WTS_SESSION_UNLOCK {
                // A secure-desktop transition can drop key-ups and unregister
                // hotkeys. Release *physical* left/right modifiers with real
                // Win32 key-up injection, then the shell's generic key cleanup,
                // re-register both hotkeys and resync the cached state.
                crate::platform::shell_shortcuts_ex::release_stuck_modifiers();
                opentaskbar_shell::taskbar::release_held_keys();
                if state.monitor.is_primary {
                    register_global_hotkeys(hwnd, &state.settings);
                }
                state.resync_foreground_after_session();
            }
            0
        }
        WM_TIMER if wparam == TIMER_REVEALED_GUARD => {
            // Never mark overlap dirty here: the 200 ms tick must not turn into a
            // cross-process window scan (nor a Start-process validation).
            state.evaluate_auto_hide_guard();
            0
        }
        WM_TIMER if wparam == TIMER_HOVER_INTENT => {
            state.tick_widget_hover();
            state.open_pending_hover_flyout();
            0
        }
        WM_TIMER if wparam == TIMER_HIDE_DELAY => {
            // Pointer stayed outside for the full `AutoHideDelayMs` window.
            unsafe { KillTimer(hwnd, TIMER_HIDE_DELAY) };
            state.evaluate_auto_hide();
            0
        }
        WM_TIMER if wparam == TIMER_DRAG_HOVER => {
            unsafe {
                KillTimer(hwnd, TIMER_DRAG_HOVER);
            }
            if let Some(drag) = state.drag
                && drag.active
                && let Some(slot) = state.slots.get(drag.idx)
                && let Some(hwnd_target) = slot.windows.first().map(|w| w.hwnd)
            {
                tracker::activate_window(hwnd_target);
            }
            state.drag_hover_started = None;
            0
        }
        WM_TIMER if wparam == TIMER_PREVIEW_SHOW => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_PREVIEW_SHOW);
            }
            if let Some(idx) = state.preview_pending_idx.take()
                && state.hovered_idx == Some(idx)
            {
                state.show_preview_for_slot(idx);
            }
            0
        }
        WM_TIMER if wparam == TIMER_PREVIEW_HIDE => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_PREVIEW_HIDE);
            }
            if !state.is_cursor_over_dock_or_preview() {
                state.hide_preview_now();
                state.hover_mouse_x = None;
                state.hovered_idx = None;
                state.evaluate_auto_hide();
                state.ensure_animation();
                state.update_visuals();
            }
            0
        }
        WM_TIMER if wparam == TIMER_PREVIEW_WATCH => {
            if state.preview.as_deref().is_some_and(|p| p.is_visible()) {
                if !state.is_cursor_over_dock_or_preview() {
                    state.hide_preview_now();
                    state.hover_mouse_x = None;
                    state.hovered_idx = None;
                    state.evaluate_auto_hide();
                    state.ensure_animation();
                    state.update_visuals();
                }
            } else {
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_PREVIEW_WATCH);
                }
            }
            0
        }
        WM_TIMER if wparam == TIMER_WIDGETS => {
            if state.refresh_widgets() {
                state.update_visuals();
            }
            if effective_animations_enabled(
                state.settings.animations_enabled,
                state.settings.focus_mode,
            ) && state.widget_runtime.needs_animation_tick(&state.settings)
            {
                let interval = state.widget_anim_interval_ms();
                unsafe {
                    SetTimer(hwnd, TIMER_WIDGET_ANIM, interval, None);
                }
            } else {
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_WIDGET_ANIM);
                }
            }
            0
        }
        WM_TIMER if wparam == TIMER_WIDGET_ANIM => {
            if effective_animations_enabled(
                state.settings.animations_enabled,
                state.settings.focus_mode,
            ) && state.widget_runtime.needs_animation_tick(&state.settings)
            {
                if state.refresh_widgets() {
                    state.update_visuals();
                }
            } else {
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_WIDGET_ANIM);
                }
            }
            0
        }
        WM_TIMER if wparam == TIMER_POLL => {
            if state.monitor.is_primary {
                let _ = opentaskbar_shell::taskbar::ensure_hidden();
            }
            // Track the user's real target app for the Keyboard Layout widget.
            state.note_external_foreground();
            state.reconcile_pointer_after_interaction();
            let slots_changed = state.sync_slots(true);
            state.evaluate_auto_hide();
            if slots_changed {
                state.update_visuals();
            }
            0
        }
        WM_TIMER if wparam == TIMER_WINDOW_CLOSE => {
            if state.drag.is_none()
                && state
                    .slots
                    .iter()
                    .any(|slot| last_window_closed(slot, tracked_window_is_visible))
                && state.sync_slots(true)
            {
                state.update_visuals();
            }
            0
        }
        WM_CLOSE => {
            // Alt+F4 / the Exit menu must end the message loop and hand Explorer's
            // taskbar back even when a flyout/context menu currently owns the
            // foreground. Never wait on a modal loop here.
            opentaskbar_shell::taskbar::end_replacement();
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        WM_DESTROY => {
            unsafe {
                KillTimer(hwnd, TIMER_POLL);
                KillTimer(hwnd, TIMER_WINDOW_CLOSE);
                KillTimer(hwnd, TIMER_PREVIEW_SHOW);
                KillTimer(hwnd, TIMER_PREVIEW_HIDE);
                KillTimer(hwnd, TIMER_PREVIEW_WATCH);
                KillTimer(hwnd, TIMER_WIDGETS);
                KillTimer(hwnd, TIMER_WIDGET_ANIM);
                KillTimer(hwnd, TIMER_REVEALED_GUARD);
                KillTimer(hwnd, TIMER_DRAG_HOVER);
                KillTimer(hwnd, TIMER_HOVER_INTENT);
                KillTimer(hwnd, TIMER_HIDE_DELAY);
                WTSUnRegisterSessionNotification(hwnd);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub fn run(settings: DockSettings) {
    let startup_t0 = Instant::now();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    window_picker::set_picker_enabled(settings.previews_enabled);

    let auto_hide = settings.auto_hide != AutoHideMode::Disabled;
    let initial_pins = load_effective_pins(&settings);
    let mut initial_running = tracker::enumerate_windows();
    if settings.current_desktop_only {
        initial_running.retain(|window| tracker::is_on_current_desktop(window.hwnd));
    }
    let initial_slots = build_slots_for(&initial_pins, &initial_running, &[], &settings);
    let t_pins_ms = startup_t0.elapsed().as_millis();

    let class_name = wide("OpenTaskBarDock");
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        lpszClassName: class_name.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        RegisterClassExW(&wc);
    }

    let reload_msg = unsafe { RegisterWindowMessageW(wide(settings_ui::RELOAD_MSG_NAME).as_ptr()) };
    RELOAD_MSG.store(reload_msg, Ordering::Relaxed);

    let all_monitors = win32::enumerate_monitors();
    let target_monitors: Vec<MonitorInfo> = if let Some(ref target) = settings.target_display {
        let named: Vec<MonitorInfo> = all_monitors
            .iter()
            .filter(|monitor| monitor.device_name.eq_ignore_ascii_case(target))
            .cloned()
            .collect();
        if named.is_empty() {
            all_monitors.iter().take(1).cloned().collect()
        } else {
            named
        }
    } else if settings.show_on_all_monitors || settings.mirror_main_dock {
        all_monitors.clone()
    } else {
        all_monitors.iter().take(1).cloned().collect()
    };

    let ex_style = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP;
    let mut created: Vec<(HWND, *mut DockState)> = Vec::new();

    for monitor in target_monitors {
        let is_primary = monitor.is_primary;
        let dpi_scale = monitor.dpi_scale;
        let mon_handle = monitor.handle;
        let dock_layout = layout::secondary_dock_layout(&settings, is_primary);
        let light_strip = dock_layout == layout::SecondaryLayout::LightStrip;
        let initial_count = if light_strip { 0 } else { initial_slots.len() };
        let monitor_effective =
            EffectiveMetrics::for_monitor(&settings, monitor.width() as f32 / dpi_scale);
        let rect = compute_window_rect_with(
            &settings,
            initial_count,
            monitor.clone(),
            &monitor_effective,
        );
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                class_name.as_ptr(),
                wide("OpenTaskBar").as_ptr(),
                WS_POPUP | WS_VISIBLE,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            continue;
        }
        let hwnd_val = hwnd as usize;
        win32::configure_native_dock_hwnd(hwnd_val);
        unsafe {
            DragAcceptFiles(hwnd, 1);
            let _ = WTSRegisterSessionNotification(hwnd, 0);
        }

        // Paint and commit the first DirectComposition frame immediately BEFORE synchronous
        // Explorer AppBar RPCs so the dock appears on screen without waiting on shell IPC.
        match DockState::new(
            hwnd_val,
            monitor,
            reload_msg,
            settings.clone(),
            initial_pins.clone(),
            if light_strip {
                Vec::new()
            } else {
                initial_slots.clone()
            },
            if light_strip {
                Vec::new()
            } else {
                initial_running.clone()
            },
            rect,
            !is_primary,
            light_strip,
        ) {
            Ok(mut state) => {
                if light_strip {
                    // Reuse Explorer's existing shelf when it is tall enough;
                    // otherwise register an AppBar for the profile's secondary
                    // shelf (52/60/68 DIP) and let Explorer trim the rect.
                    let shelf_dips = layout::size_profile(&settings).secondary_shelf;
                    if layout::needs_own_shelf(0, dpi_scale, shelf_dips) {
                        let edge = layout::appbar_position_flags(layout::effective_edge(&settings));
                        opentaskbar_shell::appbar::register(hwnd_val, 0, "secondary-light-strip");
                        let missing = layout::missing_shelf_pixels(shelf_dips, 0, dpi_scale);
                        let top = rect.1 - missing.min(rect.3.max(0));
                        let requested = RECT {
                            left: rect.0,
                            top,
                            right: rect.0 + rect.2,
                            bottom: rect.1 + rect.3,
                        };
                        opentaskbar_shell::appbar::set_position(
                            hwnd_val,
                            edge,
                            requested,
                            true,
                            "secondary-light-strip",
                        );
                        state.appbar_registered = true;
                    }
                } else if !is_primary && settings.mirror_main_dock {
                    // #178: a mirror dock is a full-size secondary taskbar, so
                    // Explorer must reserve its edge too. Without this the
                    // mirrored strip overlaps maximised windows on that monitor.
                    let reason = "secondary-mirror-dock";
                    let registered = opentaskbar_shell::appbar::register(hwnd_val, 0, reason);
                    if registered != 0 {
                        let edge = layout::appbar_position_flags(layout::effective_edge(&settings));
                        let requested = RECT {
                            left: rect.0,
                            top: rect.1,
                            right: rect.0 + rect.2,
                            bottom: rect.1 + rect.3,
                        };
                        opentaskbar_shell::appbar::set_position_pinned(
                            hwnd_val, edge, requested, reason,
                        );
                        state.appbar_registered = true;
                        state.appbar_reason = reason;
                    }
                }
                let ptr = Box::into_raw(Box::new(state));
                unsafe {
                    set_window_long_ptr(hwnd, GWLP_USERDATA, ptr as isize);
                    SetTimer(hwnd, TIMER_POLL, 800, None);
                    SetTimer(hwnd, TIMER_WINDOW_CLOSE, 50, None);
                    SetTimer(hwnd, TIMER_WIDGETS, 2000, None);
                }
                created.push((hwnd, ptr));
                if is_primary {
                    let desired_total_dips = pill_height_dips(&settings)
                        + (settings::effective_bottom_gap(&settings).max(0) as f32)
                        + VERTICAL_EDGE_GAP;
                    let desired_total_px = (desired_total_dips * dpi_scale).ceil() as i32;
                    opentaskbar_shell::taskbar::begin_replacement(
                        hwnd_val,
                        auto_hide,
                        desired_total_px,
                    );
                }
            }
            Err(err) => {
                opentaskbar_core::log::error(format!(
                    "Failed to initialize DirectComposition on monitor {mon_handle}: {err}",
                ));
                unsafe {
                    DestroyWindow(hwnd);
                }
            }
        }
    }

    opentaskbar_core::log::info(format!(
        "Dock ready in {} ms (pins+windows: {} ms, monitors: {}).",
        startup_t0.elapsed().as_millis(),
        t_pins_ms,
        created.len()
    ));

    if created.is_empty() {
        opentaskbar_core::log::error("No dock windows could be created.");
        unsafe { CoUninitialize() };
        return;
    }

    win32::trim_working_set();

    DOCK_HWNDS.with(|list| {
        let mut guard = list.borrow_mut();
        guard.clear();
        guard.extend(created.iter().map(|(hwnd, _)| *hwnd as usize));
    });
    if let Some((primary_hwnd, _)) = created.first() {
        register_global_hotkeys(*primary_hwnd as HWND, &settings);
    }
    let wheel_hook: HHOOK =
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_wheel_hook), hinstance, 0) };
    let key_hook: HHOOK =
        unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(win_key_hook), hinstance, 0) };
    let alert_hook = install_notification_alert_hook();

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if ret <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // The Settings UI is a separate `--settings` process; quitting the dock from the Exit
    // menu must close it as reference MainWindow.Window_Closed does with
    // SettingsWindow.CloseForShutdown(), otherwise its window stays on screen.
    settings_ui::close_spawned_settings_windows();

    if !wheel_hook.is_null() {
        unsafe {
            let _ = UnhookWindowsHookEx(wheel_hook);
        }
    }
    if !key_hook.is_null() {
        unsafe {
            let _ = UnhookWindowsHookEx(key_hook);
        }
    }
    if !alert_hook.is_null() {
        unsafe {
            let _ = UnhookWinEvent(alert_hook);
        }
    }
    let owner = HOTKEY_OWNER.take();
    if owner != 0 {
        unsafe {
            UnregisterHotKey(owner as HWND, HOTKEY_LAUNCHER_ID);
            UnregisterHotKey(owner as HWND, HOTKEY_DOCK_ID);
        }
    }
    tray_host::stop_host();
    DOCK_HWNDS.with(|list| list.borrow_mut().clear());

    for (hwnd, ptr) in created {
        unsafe {
            if (*ptr).appbar_registered {
                opentaskbar_shell::appbar::unregister(hwnd as usize, (*ptr).appbar_reason);
            }
            set_window_long_ptr(hwnd, GWLP_USERDATA, 0);
            DestroyWindow(hwnd);
            drop(Box::from_raw(ptr));
        }
    }
    unsafe {
        CoUninitialize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::menu::MenuIcon;

    #[test]
    fn hover_flyout_gate_accepts_legacy_casing_and_custom_ids() {
        assert!(widget_has_flyout_ci("clock"));
        assert!(widget_has_flyout_ci("Clock"));
        assert!(widget_has_flyout_ci("AiUsageClaude"));
        assert!(!widget_has_flyout_ci("no_such_widget"));
    }

    #[test]
    fn multi_monitor_anchor_selects_the_dock_on_the_flyout_monitor() {
        let anchors = [
            (11, FlyoutKind::Start, (100, 900, 160, 940)), // primary dock
            (22, FlyoutKind::Start, (-1860, 900, -1800, 940)), // left secondary dock
            (33, FlyoutKind::Start, (0, 0, 0, 0)),         // hidden Start button
        ];
        let monitors = |hwnd: usize| match hwnd {
            11 => 1,
            22 | 33 => 2,
            900 => 2, // the flyout lives on monitor 2
            _ => 0,
        };
        // A flyout on monitor 2 must use the secondary dock, not the global primary.
        assert_eq!(
            select_anchor_for(&anchors, FlyoutKind::Start, 900, monitors),
            (-1860, 900, -1800, 940)
        );
        assert_eq!(
            select_anchor_for(&anchors, FlyoutKind::Start, 11, monitors),
            (100, 900, 160, 940)
        );
        // Empty anchor is never chosen; unknown monitors fall back to the first
        // valid dock so a missing monitor cannot leave a flyout unanchored.
        assert_eq!(
            select_anchor_for(&anchors, FlyoutKind::Start, 0, monitors),
            (100, 900, 160, 940)
        );
        assert_eq!(
            select_anchor_for(
                &[(33, FlyoutKind::Start, (0, 0, 0, 0))],
                FlyoutKind::Start,
                900,
                monitors
            ),
            (0, 0, 0, 0)
        );
        assert!(!anchor_is_valid((0, 0, 0, 0)));
        assert!(anchor_is_valid((0, 0, 1, 1)));
    }

    #[test]
    fn shell_anchors_are_selected_per_kind_not_across_kinds() {
        // Same dock, three different buttons; each kind must resolve its own rect.
        let anchors = [
            (11, FlyoutKind::Start, (100, 900, 160, 940)),
            (11, FlyoutKind::Search, (200, 900, 260, 940)),
            (11, FlyoutKind::ControlCenter, (300, 900, 360, 940)),
            // A second monitor's buttons must never leak into the first.
            (22, FlyoutKind::Start, (-1860, 900, -1800, 940)),
            (22, FlyoutKind::Search, (-1860, 900, -1800, 940)),
        ];
        let monitors = |hwnd: usize| match hwnd {
            11 => 1,
            22 => 2,
            900 => 2,
            _ => 0,
        };
        for (kind, expected) in [
            (FlyoutKind::Start, (-1860, 900, -1800, 940)),
            (FlyoutKind::Search, (-1860, 900, -1800, 940)),
            // No CC dock on monitor 2: fall back to the first valid *CC* rect,
            // never to another kind's rect on the same monitor.
            (FlyoutKind::ControlCenter, (300, 900, 360, 940)),
        ] {
            assert_eq!(select_anchor_for(&anchors, kind, 900, monitors), expected);
        }
        assert_eq!(
            first_valid_anchor(&anchors, FlyoutKind::ControlCenter),
            (300, 900, 360, 940)
        );
        assert_eq!(
            select_anchor_for(&anchors, FlyoutKind::ControlCenter, 11, monitors),
            (300, 900, 360, 940)
        );
        // An invalid rect for one kind cannot act as a fallback for another.
        let only_invalid = [(11, FlyoutKind::Search, (0, 0, 0, 0))];
        assert_eq!(
            select_anchor_for(&only_invalid, FlyoutKind::ControlCenter, 900, monitors),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn flyout_message_codes_are_distinct_and_round_trip() {
        let mut codes = std::collections::HashSet::new();
        for kind in [
            FlyoutKind::Start,
            FlyoutKind::Search,
            FlyoutKind::ControlCenter,
        ] {
            let code = flyout_code(kind);
            assert!(codes.insert(code), "duplicate flyout code for {kind:?}");
            assert_eq!(flyout_kind_from_code(code), kind);
        }
        // Unknown codes default to Start instead of dispatching a wrong flyout.
        assert_eq!(flyout_kind_from_code(usize::MAX), FlyoutKind::Start);
    }

    #[test]
    fn every_shell_flyout_open_path_posts_the_anchor_request() {
        // Source-level assertion: opening Search/CC must only post the message;
        // the anchor lookup and worker spawn belong to the UI-thread handler.
        let dock_src = include_str!("mod.rs");
        let main_src = include_str!("../main.rs");
        for needle in [
            "self.request_flyout_anchor(FlyoutKind::Search);",
            "state.request_flyout_anchor(FlyoutKind::Search);",
            "self.request_flyout_anchor(FlyoutKind::ControlCenter);",
            "crate::on_shell_flyout_opened(kind, lparam as usize);",
        ] {
            assert!(dock_src.contains(needle), "dock must contain {needle:?}");
        }
        assert!(main_src.contains("dock::shell_button_anchor_for(hwnd, kind)"));
        assert!(main_src.contains("reposition_flyout(kind, anchor, hwnd)"));
    }

    #[test]
    fn start_toggle_debounces_rapid_double_clicks() {
        let now = Instant::now();
        assert!(start_toggle_allowed(None, now));
        assert!(!start_toggle_allowed(
            Some(now - Duration::from_millis(100)),
            now
        ));
        assert!(start_toggle_allowed(
            Some(now - Duration::from_millis(301)),
            now
        ));
        assert!(!start_toggle_allowed(
            Some(now + Duration::from_millis(50)),
            now
        ));
    }

    #[test]
    fn start_foreground_is_not_a_focus_restore_target() {
        // Foreground is the Start flyout or our own dock: keep the prior app.
        assert_eq!(restore_target_for_press(0, 0, 10), 0);
        assert_eq!(restore_target_for_press(99, 99, 10), 0);
        assert_eq!(restore_target_for_press(10, 99, 10), 0);
        // A real app is remembered as the restore target.
        assert_eq!(restore_target_for_press(42, 99, 10), 42);
    }

    #[test]
    fn negative_x_secondary_monitor_positions_correctly() {
        let settings = DockSettings::default();
        let left_monitor = MonitorInfo {
            handle: 2,
            device_name: r"\\.\DISPLAY2".to_owned(),
            left: -2560,
            top: 0,
            right: 0,
            bottom: 1440,
            dpi_scale: 1.0,
            is_primary: false,
        };
        let (x, y, w, h) = compute_window_rect(&settings, 8, left_monitor.clone());
        assert!(
            x < 0,
            "dock on left monitor must keep negative X (Issue #95)"
        );
        assert!(x >= left_monitor.left && x + w <= left_monitor.right);
        assert_eq!(y + h, left_monitor.bottom);
    }

    #[test]
    fn mixed_dpi_monitor_scales_window_bounds() {
        let settings = DockSettings::default();
        let hidpi = MonitorInfo {
            handle: 1,
            device_name: r"\\.\DISPLAY1".to_owned(),
            left: 0,
            top: 0,
            right: 3840,
            bottom: 2160,
            dpi_scale: 1.5,
            is_primary: true,
        };
        let (x, y, w, h) = compute_window_rect(&settings, 6, hidpi);
        assert!(w > 0 && w <= 3840);
        assert!(h > 0 && y + h == 2160);
        assert_eq!(x, (3840 - w) / 2);
    }

    #[test]
    fn build_slots_preserves_unified_user_order() {
        let pins = vec![
            PinnedApp {
                name: "Alpha".into(),
                launch_path: r"C:\Alpha.lnk".into(),
                target_exe: Some(r"C:\Alpha.exe".into()),
                icon_source: r"C:\Alpha.exe".into(),
            },
            PinnedApp {
                name: "Beta".into(),
                launch_path: r"C:\Beta.lnk".into(),
                target_exe: Some(r"C:\Beta.exe".into()),
                icon_source: r"C:\Beta.exe".into(),
            },
        ];
        let running = vec![RunningWindow {
            hwnd: 100,
            pid: 42,
            title: "Gamma Window".into(),
            exe_path: r"C:\Gamma.exe".into(),
            is_foreground: true,
            aumid: String::new(),
        }];
        let initial = build_slots(&pins, &running, &[]);
        assert_eq!(initial.len(), 3);
        // Simulate user dragging unpinned Gamma between Alpha and Beta.
        let reordered = vec![initial[0].clone(), initial[2].clone(), initial[1].clone()];
        let after_poll = build_slots(&pins, &running, &reordered);
        assert_eq!(after_poll[0].label, "Alpha");
        assert_eq!(after_poll[1].label, "Gamma");
        assert_eq!(after_poll[2].label, "Beta");
    }

    #[test]
    fn background_pin_moves_only_the_explicitly_selected_legacy_pin() {
        let path = r"C:\Apps\Steam.exe";
        let other = r"C:\Apps\Normal.exe".to_owned();
        let mut settings = DockSettings {
            user_pinned_apps: vec![path.to_lowercase(), other.clone()],
            pin_order: Some(vec![other.clone(), path.to_lowercase()]),
            ..DockSettings::default()
        };
        update_background_pin(&mut settings, path, true);
        update_background_pin(&mut settings, &path.to_lowercase(), true);
        assert_eq!(settings.user_pinned_background_apps, [path]);
        assert_eq!(settings.user_pinned_apps, std::slice::from_ref(&other));
        assert_eq!(settings.pin_order.as_deref(), Some(&[other][..]));

        update_background_pin(&mut settings, &path.to_lowercase(), false);
        assert!(settings.user_pinned_background_apps.is_empty());
        assert!(settings.user_unpinned_apps.is_empty());
    }

    #[test]
    fn compact_pins_fit_before_the_background_button_and_overflow_on_narrow_monitors() {
        let mut settings = DockSettings {
            user_pinned_background_apps: (0..12).map(|index| format!("app{index}")).collect(),
            ..DockSettings::default()
        };
        let base = layout::size_profile(&settings).cell;
        let cell = tray_pin_cell(base);
        assert!(cell < base);
        let count = visible_tray_pin_count(&settings, 3, 570.0);
        assert!(count > 0 && count < settings.user_pinned_background_apps.len());
        assert_eq!(visible_tray_pin_count(&settings, 3, 1920.0), 12);
        let tray = (500.0, 20.0, 500.0 + base, 70.0);
        let gap = widget_gap(&settings);
        let last = tray_pin_rect(tray, cell, gap, count, count - 1);
        assert_eq!(last.2, tray.0 - gap);
        let first = tray_pin_rect(tray, cell, gap, count, 0);
        assert!((first.0 - (tray.0 - gap - count as f32 * cell)).abs() < 0.01);
        settings.show_tray_button = false;
        assert_eq!(visible_tray_pin_count(&settings, 3, 1920.0), 0);
    }

    #[test]
    fn closing_slot_waits_for_the_last_window_and_cancels_if_the_app_reopens() {
        let mut windows = vec![RunningWindow {
            hwnd: 100,
            pid: 42,
            title: "First".into(),
            exe_path: r"C:\Apps\Running.exe".into(),
            is_foreground: true,
            aumid: String::new(),
        }];
        windows.push(RunningWindow {
            hwnd: 101,
            title: "Second".into(),
            ..windows[0].clone()
        });
        let previous = build_slots(&[], &windows, &[]);
        let now = Instant::now();
        let mut next = build_slots(&[], &windows[1..], &previous);
        keep_closing_slots(&previous, &mut next, now, |hwnd| hwnd == 101);
        assert!(next[0].close_started.is_none());

        let mut closing = build_slots(&[], &[], &next);
        keep_closing_slots(&next, &mut closing, now, |_| false);
        assert!(closing[0].is_exiting());
        assert!(closing[0].windows.is_empty());
        assert!(closing[0].layout_fraction(now + CLOSE_DURATION / 2) < 0.5);
        assert_eq!(closing[0].layout_fraction(now + CLOSE_DURATION), 0.0);

        let mut reopened = build_slots(&[], &windows, &closing);
        keep_closing_slots(&closing, &mut reopened, now, |_| false);
        assert!(!reopened[0].is_exiting());
        let mut expired = Vec::new();
        keep_closing_slots(&closing, &mut expired, now + CLOSE_DURATION, |_| false);
        assert!(expired.is_empty());

        let pin = PinnedApp {
            name: "Running".into(),
            launch_path: windows[0].exe_path.clone(),
            target_exe: Some(windows[0].exe_path.clone()),
            icon_source: windows[0].exe_path.clone(),
        };
        let pins = [pin];
        let previous = build_slots(&pins, &windows, &[]);
        let mut pinned = build_slots(&pins, &[], &previous);
        keep_closing_slots(&previous, &mut pinned, now, |_| false);
        assert_eq!(pinned[0].close_started, Some(now));
        assert!(!pinned[0].is_exiting());
        assert_eq!(pinned[0].layout_fraction(now + CLOSE_DURATION / 2), 1.0);
    }

    #[test]
    fn scroll_metrics_prevent_apps_and_widgets_overlap_and_clamp_edge_indicators() {
        // Natural-width dock (`vp_w = shell_w + apps_w + right_w + 28.0`): never triggers false scrollbars.
        let shell_w = 200.0;
        let natural = compute_scroll_metrics(
            150.0,
            150.0 + shell_w + 260.0 + 320.0 + 28.0,
            260.0,
            320.0,
            shell_w,
            0.0,
            DockContentAlignment::Center,
        );
        assert_eq!(natural.max_scroll, 0.0);
        assert!(!natural.can_scroll_left);
        assert!(!natural.can_scroll_right);
        assert_eq!(natural.shell_start_x, 164.0);
        assert_eq!(natural.apps_start_x, natural.shell_start_x + shell_w);
        assert_eq!(
            natural.right_anchor_x,
            150.0 + shell_w + 260.0 + 320.0 + 28.0 - 14.0
        );

        // Overflowing dock (e.g. many apps + many widgets): zero overlap and accurate chevrons.
        let start = compute_scroll_metrics(
            150.0,
            750.0,
            420.0,
            620.0,
            shell_w,
            0.0,
            DockContentAlignment::Center,
        );
        assert!(start.max_scroll > 600.0);
        assert!(!start.can_scroll_left);
        assert!(start.can_scroll_right);
        let apps_end = start.apps_start_x + 420.0;
        let widgets_left = start.right_anchor_x - 620.0;
        assert!(
            widgets_left >= apps_end - 0.01,
            "widgets ({widgets_left}) must never overlap apps ({apps_end})"
        );

        let mid = compute_scroll_metrics(
            150.0,
            750.0,
            420.0,
            620.0,
            shell_w,
            start.max_scroll * 0.5,
            DockContentAlignment::Center,
        );
        assert!(mid.can_scroll_left);
        assert!(mid.can_scroll_right);
        assert!(mid.shell_start_x < start.shell_start_x);
        assert_eq!(mid.apps_start_x, mid.shell_start_x + shell_w);

        let end = compute_scroll_metrics(
            150.0,
            750.0,
            420.0,
            620.0,
            shell_w,
            start.max_scroll + 200.0,
            DockContentAlignment::Center,
        );
        assert!((end.offset - start.max_scroll).abs() < 0.01);
        assert!(end.shell_start_x < end.vp_left);
        assert!(end.can_scroll_left);
        assert!(!end.can_scroll_right);
        assert!((end.right_anchor_x - 736.0).abs() < 0.01);
    }

    /// Pure geometry probe: an auto-width dock with a fixed number of slots.
    /// Mirrors `live_pill_geometry` + `scroll_metrics_for_pill` without a HWND.
    fn spacing_case(padding: i32) -> (f32, f32, f32, f32, f32) {
        let settings = DockSettings {
            dock_icon_padding: padding,
            ..DockSettings::default()
        };
        let base = layout::size_profile(&settings).cell;
        let shell_w = shell_lead_width(&settings);
        let apps_w = section_width(base, slot_gap(&settings), 4);
        let trail_w = base_trail_width(&settings);
        let pill_w = pill_width_for_content(content_strip_width(shell_w, apps_w, trail_w, 0.0));
        let pill_left = 12.0; // (window width - pill width) / 2 at rest
        let (vp_left, vp_right) = pill_viewport(pill_left, pill_w);
        let sm = compute_scroll_metrics(
            vp_left,
            vp_right,
            apps_w,
            trail_w,
            shell_w,
            0.0,
            settings.content_alignment,
        );
        assert!(
            !sm.can_scroll_right && sm.max_scroll == 0.0,
            "a resting dock must never report overflow"
        );
        let start_center = sm.shell_start_x + base * 0.5;
        let anchor = centered_shell_rect(start_center, base, 960.0, 1004.0);
        (
            sm.shell_start_x,
            sm.apps_start_x + base * 0.5,
            (anchor.0 + anchor.2) * 0.5,
            base + slot_gap(&settings),
            sm.right_anchor_x,
        )
    }

    #[test]
    fn spacing_does_not_shift_start_and_is_counted_once() {
        // Numeric example (Compact, 4 apps): padding 0 -> Start x = 27 DIP,
        // first slot center = 185 DIP; padding 12 -> Start x = 27 DIP,
        // first slot center = 209 DIP (2 shell gaps grew by 12 each).
        let (start0, first0, anchor0, stride0, trail0) = spacing_case(0);
        let (start12, first12, anchor12, stride12, trail12) = spacing_case(12);
        assert_eq!(start0, 27.0);
        assert_eq!(first0, 185.0);
        assert_eq!(start12, start0, "spacing must never drag Start left");
        assert_eq!(
            anchor12, anchor0,
            "Start anchor tracks the fixed shell cell"
        );
        assert_eq!(anchor0, start0 + 19.0);
        assert_eq!(stride12 - stride0, 12.0, "padding counted once, not twice");
        assert_eq!(first12 - first0, 24.0, "two shell gaps, each +12 DIP");
        assert_eq!(trail12 - trail0, 84.0, "apps/shell/trail each grow once");
    }

    #[test]
    fn widgets_sit_inline_after_apps_and_before_trail_group() {
        let mut settings = DockSettings::default();
        settings.set_widget_enabled("clock", true);
        settings.set_widget_enabled("audio", true);
        settings.show_tray_button = true;
        settings.show_recycle_bin = true;

        let base = layout::size_profile(&settings).cell;
        let shell_w = shell_section_width(&settings);
        let apps_w = 3.0 * base + 2.0 * slot_gap(&settings);
        let widgets_w = widgets_strip_width(&settings, true);
        let trail_w = base_trail_width(&settings);

        assert!(widgets_w > 0.0);
        assert!(trail_w > 0.0);

        let sm = compute_scroll_metrics(
            100.0,
            100.0 + shell_w + apps_w + widgets_w + trail_w + 28.0,
            apps_w + widgets_w,
            trail_w,
            shell_w,
            0.0,
            DockContentAlignment::Center,
        );
        let first_widget_x = sm.apps_start_x + apps_w + 8.0;
        let last_widget_right = sm.apps_start_x + apps_w + widgets_w - widget_gap(&settings);
        let trail_left = sm.right_anchor_x - trail_w;
        assert!(first_widget_x > sm.apps_start_x + apps_w);
        assert!(
            last_widget_right < trail_left,
            "widgets end ({last_widget_right}) must stay left of trail group ({trail_left})"
        );
    }

    #[test]
    fn wheel_route_payload_encodes_delta_axis_and_point() {
        // Mirrors what mouse_wheel_hook posts and WM_APP_WHEEL_ROUTE decodes.
        let delta: i16 = 240;
        let horizontal = true;
        let payload = ((delta as u16 as usize) << 1) | usize::from(horizontal);
        assert_eq!((payload >> 1) as u32 as i16, delta);
        assert_eq!((payload & 1) != 0, horizontal);
        let point = layout::pack_cursor_point(120, -40);
        assert_eq!(layout::unpack_cursor_point(point), (120, -40));
    }

    #[test]
    fn wndproc_guard_is_scoped_per_hwnd() {
        let first = DockWndProcGuard::try_enter(11).expect("first dock");
        let second = DockWndProcGuard::try_enter(22).expect("second dock is not blocked");
        assert!(DockWndProcGuard::try_enter(11).is_none());
        drop(first);
        assert!(DockWndProcGuard::try_enter(11).is_some());
        assert!(DockWndProcGuard::try_enter(22).is_none());
        drop(second);
        assert!(DockWndProcGuard::try_enter(22).is_some());
    }

    #[test]
    fn vsync_dropped_during_reentry_is_marked_and_cleared_once() {
        let hwnd = 0xDEAD_BEEFusize;
        assert!(!take_vsync_dropped(hwnd));
        mark_vsync_dropped(hwnd);
        mark_vsync_dropped(hwnd);
        assert!(take_vsync_dropped(hwnd));
        assert!(!take_vsync_dropped(hwnd));
    }

    #[test]
    fn async_snapshot_and_widget_refresh_throttles_come_from_one_policy() {
        // The dock host consumes the layout policy; keep the budget constants
        // below the 800 ms TIMER_POLL so no hot path can starve a snapshot.
        assert!(layout::WINDOW_SNAPSHOT_MIN_INTERVAL <= Duration::from_millis(800));
        assert!(layout::WIDGET_REFRESH_MIN_INTERVAL >= Duration::from_millis(8));
        let now = Instant::now();
        assert!(!layout::window_snapshot_due(Some(now), true, now));
        assert!(layout::widget_refresh_due(None, now));
    }

    #[test]
    fn aumid_grouping_merges_packaged_windows_but_keeps_shared_hosts_separate() {
        let packaged = RunningWindow {
            hwnd: 1,
            pid: 10,
            title: "App".into(),
            exe_path: r"C:\Apps\Contoso.exe".into(),
            is_foreground: false,
            aumid: "Contoso.App_1.0_x64__abc".into(),
        };
        let second = RunningWindow {
            hwnd: 2,
            pid: 11,
            title: "App 2".into(),
            exe_path: r"C:\Apps\Contoso.exe".into(),
            is_foreground: true,
            aumid: "Contoso.App_1.0_x64__abc".into(),
        };
        assert_eq!(
            running_group_key(&packaged, false),
            running_group_key(&second, false)
        );
        assert_ne!(
            running_group_key(&packaged, false),
            running_group_key(&packaged, true)
        );
        assert_eq!(
            running_group_key(&packaged, true),
            format!("exe:{}", packaged.exe_path.to_lowercase())
        );
        let slots = build_slots_for(&[], &[packaged, second], &[], &DockSettings::default());
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].windows.len(), 2);
        assert!(slots[0].is_foreground);
    }

    #[test]
    fn wm_close_path_restores_taskbar_and_quits() {
        // Source-level contract: the dock WM_CLOSE handler must restore Explorer's
        // taskbar and end the message loop, and the Exit menu must do the same.
        let src = include_str!("mod.rs");
        let close = src
            .find("WM_CLOSE => {")
            .expect("dock must handle WM_CLOSE");
        let arm = &src[close..close + 420];
        assert!(arm.contains("taskbar::end_replacement()"));
        assert!(arm.contains("PostQuitMessage(0)"));
        assert!(src.contains("ContextAction::Exit =>"));
    }

    #[test]
    fn dock_hotkey_starts_keyboard_navigation_not_focus_mode_toggle() {
        let src = include_str!("mod.rs");
        let hotkey = src.find("HOTKEY_DOCK_ID =>").expect("dock hotkey arm");
        let arm = &src[hotkey..hotkey + 320];
        assert!(arm.contains("start_keyboard_navigation()"));
        assert!(!arm.contains("focus_mode = !"));
    }

    #[test]
    fn gesture_timers_and_input_guards_are_wired_to_their_policies() {
        let src = include_str!("mod.rs");
        for needle in [
            "SetCapture(self.hwnd as HWND);",
            "layout::drag_hover_allowed(external_file_drag_active())",
            "layout::AUTOHIDE_DELAY_MS as u32",
            "release_held_keys()",
            "register_global_hotkeys(hwnd, &state.settings)",
            "restore_app_foreground(self.flyout_focus_return, start_hwnd)",
        ] {
            assert!(
                src.contains(needle),
                "production code must contain {needle:?}"
            );
        }
    }

    #[test]
    fn secondary_docks_use_profile_shelf_and_fullscreen_guard() {
        let src = include_str!("mod.rs");
        for needle in [
            "layout::size_profile(&settings).secondary_shelf",
            "layout::needs_own_shelf(0, dpi_scale, shelf_dips)",
            "win32::is_any_fullscreen_on_monitor(&self.monitor)",
            "!self.settings.mirror_main_dock",
            "self.fullscreen_on_own_monitor",
        ] {
            assert!(
                src.contains(needle),
                "secondary path must contain {needle:?}"
            );
        }
        // #179: no "any monitor except mine" predicate may remain. Build the
        // needles so this test's own source cannot satisfy its assertion.
        let stale_flag = format!("fullscreen_on_other{}", "_monitor");
        let stale_predicate = format!("monitor.handle != self.monitor.{}", "handle");
        assert!(!src.contains(&stale_flag));
        assert!(!src.contains(&stale_predicate));
    }

    #[test]
    fn mirror_dock_registers_and_releases_its_own_appbar() {
        let src = include_str!("mod.rs");
        for needle in [
            "} else if !is_primary && settings.mirror_main_dock {",
            "opentaskbar_shell::appbar::register(hwnd_val, 0, reason)",
            "opentaskbar_shell::appbar::set_position_pinned(",
            "\"secondary-mirror-dock\"",
            "opentaskbar_shell::appbar::unregister(hwnd as usize, (*ptr).appbar_reason)",
        ] {
            assert!(src.contains(needle), "mirror dock must contain {needle:?}");
        }
    }

    #[test]
    fn window_picker_is_wired_to_app_click_group_row_and_secondary_slot() {
        let src = include_str!("mod.rs");
        assert!(src.contains("window_picker::show("));
        // App click (also the secondary dock mirror) and keyboard slot activation.
        assert!(
            src.contains("!shift && self.show_picker_for_windows(&slot_windows, &launch_path)")
        );
        assert!(src.contains(
            "let picker_opened = self.show_picker_for_windows(&slot_windows, &launch_path);"
        ));
        // Group flyout row: its own picker call keyed by the dock's monitor.
        assert!(src.contains("self.open_group_flyout(gidx, &slot.launch_path)"));
        assert!(src.contains("window_picker::show("));
        assert!(src.contains("self.picker_monitor_index(),"));
        // Group flyouts open away from the dock edge like every other dock flyout.
        assert!(src.contains("FlyoutOptions::stack().with_edge(self.dock_flyout_edge())"));
    }

    #[test]
    fn native_app_menu_wrappers_are_wired_into_the_context_menu() {
        let src = include_str!("mod.rs");
        for needle in [
            "menu::native_app_menu_entries(target_hwnd)",
            "menu::invoke_native_app_command(target_hwnd, original)",
            "self.show_native_app_menu(target)",
            "remap_native_app_menu_entries(",
        ] {
            assert!(src.contains(needle), "native menu must contain {needle:?}");
        }
        let entries = vec![
            MenuEntry::item(1, MenuIcon::Terminal, "Restore".to_owned()),
            MenuEntry::Separator,
        ];
        let (commands, items) = remap_native_app_menu_entries(entries);
        assert_eq!(commands, vec![(NATIVE_APP_MENU_ID_BASE, 1)]);
        assert!(matches!(items[1], MenuEntry::Separator));
        assert!(commands[0].0 > 1000);
    }

    #[test]
    fn picker_is_gated_and_closed_on_session_settings_and_fullscreen() {
        let src = include_str!("mod.rs");
        for needle in [
            "window_picker::set_picker_enabled(settings.previews_enabled)",
            "window_picker::set_picker_enabled(self.settings.previews_enabled)",
            "!self.settings.previews_enabled",
            "window_picker::request_close_all()",
            "window_picker::PICKER_CLOSE_MSG",
            "self.fullscreen_on_own_monitor && !was_hidden",
        ] {
            assert!(
                src.contains(needle),
                "picker wiring must contain {needle:?}"
            );
        }
    }

    #[test]
    fn session_unlock_releases_physical_modifiers() {
        let src = include_str!("mod.rs");
        let unlock = src
            .find("if event == WTS_SESSION_UNLOCK {")
            .expect("unlock arm");
        let arm = &src[unlock..unlock + 900];
        assert!(arm.contains("shell_shortcuts_ex::release_stuck_modifiers()"));
        assert!(arm.contains("register_global_hotkeys(hwnd, &state.settings)"));
        assert!(arm.contains("resync_foreground_after_session()"));
    }

    #[test]
    fn auto_width_mode_does_not_clamp_dock_to_min_surface_width() {
        let settings = DockSettings::default();
        let eff = EffectiveMetrics::for_monitor(&settings, 1920.0);
        assert!(
            eff.width_dips.is_none(),
            "Auto width mode must let the dock size to its icons/widgets"
        );
        assert!(
            eff.height_dips.is_none(),
            "Auto height mode must use the profile shelf height"
        );
        let (win_w, _) = compute_window_dips_with(&settings, 8, 1920.0, &eff);
        let natural = content_min_pill_width(&settings, 8, 1920.0);
        assert!(
            win_w >= natural,
            "window width ({win_w}) must cover natural content width ({natural}) without false scrollbars"
        );
    }

    #[test]
    fn app_icons_are_vertically_centered_and_dots_stay_inside_pill() {
        for (pill_h, icon_size) in [(48.0_f32, 28.0_f32), (58.0, 34.0), (66.0, 38.0)] {
            let pill_top = 100.0;
            let (top, bottom, dot_top) = app_icon_vertical_metrics(pill_top, pill_h, icon_size);
            let top_margin = top - pill_top;
            let bottom_margin = (pill_top + pill_h) - bottom;
            assert!(
                top_margin >= 4.0 && bottom_margin >= 8.0,
                "icon ({icon_size}px in {pill_h}px pill) must sit centered with clean margins: top={top_margin}, bottom={bottom_margin}"
            );
            assert!(
                dot_top >= bottom + 2.0 && dot_top <= pill_top + pill_h - 5.0,
                "indicator dot ({dot_top}) must remain visible inside pill ({pill_top}..{})",
                pill_top + pill_h
            );
        }
    }

    #[test]
    fn fisheye_max_scale_exceeds_one_for_all_intensities() {
        use opentaskbar_core::settings::FisheyeIntensity;
        for (intensity, expected_min) in [
            (FisheyeIntensity::Low, 1.19),
            (FisheyeIntensity::Normal, 1.40),
            (FisheyeIntensity::Strong, 1.49),
        ] {
            let settings = DockSettings {
                fisheye_intensity: intensity,
                ..DockSettings::default()
            };
            let max_scale = profile_max_scale(&settings);
            assert!(
                max_scale >= expected_min && max_scale <= MAX_FISHEYE_SCALE,
                "fisheye {intensity:?} must magnify above 1.0, got {max_scale}"
            );
            let center_target = layout::compute_target_scale(0.0, 120.0, max_scale);
            assert!(
                (center_target - max_scale).abs() < 0.001,
                "center icon target scale must reach {max_scale}, got {center_target}"
            );
        }
    }

    /// P0-4: the resting pill built by the live geometry helpers must hold its
    /// whole content strip, so no bogus 2 DIP overflow / right chevron appears.
    #[test]
    fn resting_pill_never_overflows_its_own_viewport() {
        for (slots, widgets) in [(0usize, false), (1, false), (6, true), (14, true)] {
            let mut settings = DockSettings::default();
            if widgets {
                settings.set_widget_enabled("clock", true);
                settings.set_widget_enabled("audio", true);
            }
            settings.show_tray_button = true;
            settings.show_recycle_bin = true;
            let base = layout::size_profile(&settings).cell;
            let shell_w = shell_lead_width(&settings);
            let apps_w = section_width(base, slot_gap(&settings), slots);
            let widgets_w = widgets_strip_width(&settings, slots > 0);
            let trail_w = base_trail_width(&settings);
            // Same composition as `rest_content_width_at` / `pill_geometry_impl`.
            let pill_w = pill_width_for_content(content_strip_width(
                shell_w,
                apps_w,
                widgets_w + trail_w,
                0.0,
            ));
            let (win_w, _) =
                compute_window_dips_with(&settings, slots, 1920.0, &EffectiveMetrics::default());
            let pill_left = (win_w - pill_w) * 0.5;
            let (vp_left, vp_right) = pill_viewport(pill_left, pill_w);
            // Same split as `scroll_metrics_impl`.
            let sm = compute_scroll_metrics(
                vp_left,
                vp_right,
                apps_w + widgets_w,
                trail_w,
                shell_w,
                0.0,
                settings.content_alignment,
            );
            assert_eq!(sm.max_scroll, 0.0, "{slots} slots: bogus overflow at rest");
            assert!(!sm.can_scroll_right && !sm.can_scroll_left);
            assert!(
                win_w >= pill_w,
                "window ({win_w}) must hold the pill ({pill_w})"
            );
        }
    }

    #[test]
    fn fixed_width_magnification_reserves_no_width_and_expanding_reserves_an_arch() {
        let slots = 8;
        let eff = EffectiveMetrics::default();
        let fixed = DockSettings {
            magnification_enabled: true,
            magnification_expands_dock: false,
            ..DockSettings::default()
        };
        let expanding = DockSettings {
            magnification_expands_dock: true,
            ..fixed.clone()
        };
        let rest = resting_pill_width(&fixed, slots, 1920.0);
        let (fixed_w, _) = compute_window_dips_with(&fixed, slots, 1920.0, &eff);
        assert_eq!(
            fixed_w,
            (rest + 24.0).ceil(),
            "fixed-width: in-place magnification"
        );

        let (expanding_w, _) = compute_window_dips_with(&expanding, slots, 1920.0, &eff);
        let stride = layout::size_profile(&expanding).cell + slot_gap(&expanding);
        let arch = layout::fisheye_expansion_reserve_dips(stride, profile_peak(&expanding));
        assert!(
            expanding_w >= resting_pill_width(&expanding, slots, 1920.0) + 24.0 + arch,
            "expanding: window ({expanding_w}) must reserve a full arch ({arch})"
        );

        // Focus Mode disables the fisheye: no reserve even when expanding.
        let focus = DockSettings {
            focus_mode: true,
            ..expanding.clone()
        };
        let (focus_w, _) = compute_window_dips_with(&focus, slots, 1920.0, &eff);
        assert!(focus_w < expanding_w);

        // Fixed-width growth budget is always zero, so the peak scale is clamped to
        // the no-overlap scale.
        assert_eq!(
            layout::fisheye_growth_budget(false, true, 2000.0, 500.0),
            0.0
        );
        let profile = layout::size_profile(&fixed);
        let budget = capture_expansion_budget(stride, profile.icon, 1.5, 0.0, 0.0, 0.0, 1.5);
        assert!(budget.effective_max_scale <= 1.5);
        assert!(
            (budget.effective_max_scale
                - layout::compute_no_overlap_max_scale(stride, profile.icon, 1.5))
            .abs()
                < 1e-4
        );
    }

    #[test]
    fn live_widget_widths_widen_the_window() {
        let settings = DockSettings::default();
        let mut eff = EffectiveMetrics::default();
        let (static_w, _) = compute_window_dips_with(&settings, 4, 1920.0, &eff);
        eff.live_rest_content_dips = Some(static_w + 100.0);
        let (live_w, _) = compute_window_dips_with(&settings, 4, 1920.0, &eff);
        assert!(live_w >= static_w + 100.0);
        // Never past the monitor.
        eff.live_rest_content_dips = Some(10_000.0);
        let (capped, _) = compute_window_dips_with(&settings, 4, 1920.0, &eff);
        assert!(capped <= 1920.0);
    }

    #[test]
    fn folder_stacks_are_counted_in_the_trailing_group() {
        let mut settings = DockSettings {
            show_tray_button: true,
            show_recycle_bin: true,
            ..DockSettings::default()
        };
        let base = layout::size_profile(&settings).cell;
        let without = base_trail_width(&settings);
        settings.stack_folders = vec![
            r"C:\Users\Public\Downloads".to_owned(),
            "   ".to_owned(),
            r"C:\Projects".to_owned(),
        ];
        assert_eq!(
            stack_folder_count(&settings),
            2,
            "blank entries are skipped"
        );
        let with = base_trail_width(&settings);
        assert!((with - without - 2.0 * (base + widget_gap(&settings))).abs() < 1e-4);
        // Stacks alone still form a trailing group (with its divider room).
        let stacks_only = DockSettings {
            show_tray_button: false,
            show_recycle_bin: false,
            stack_folders: vec![r"C:\Projects".to_owned()],
            ..DockSettings::default()
        };
        assert!(base_trail_width(&stacks_only) > base);
        assert_eq!(base_trail_width_for(&stacks_only, 0), 0.0);
    }

    #[test]
    fn folder_stack_enumeration_hides_temp_and_system_items() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("otb-stack-test-{}-{stamp}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Sub")).expect("create test stack");
        for name in ["a.txt", "desktop.ini", "draft.tmp", "movie.part", ".hidden"] {
            std::fs::write(dir.join(name), b"x").expect("write test file");
        }
        let path = dir.to_string_lossy().into_owned();
        let entries = read_stack_entries(&path).expect("readable stack");
        let ordered = layout::order_stack_entries(entries, layout::STACK_FLYOUT_MAX_ITEMS);
        let names: Vec<&str> = ordered.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Sub", "a.txt"]);
        assert!(ordered[0].is_dir);
        assert_eq!(stack_item_count(&path), Some(2));
        assert_eq!(read_stack_entries(&format!("{path}-missing")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_widget_ids_match_canonically() {
        let (clock, stats) = profile_widget_flags(&["Clock".to_owned(), "stats".to_owned()]);
        assert!(clock && stats);
        let (clock, stats) = profile_widget_flags(&["CLOCK".to_owned()]);
        assert!(clock && !stats);
        let (clock, stats) = profile_widget_flags(&["SystemStats".to_owned()]);
        assert!(!clock && stats);
        assert_eq!(profile_widget_flags(&[]), (false, false));
    }

    #[test]
    fn audio_wheel_is_never_gated_and_menus_use_the_dock_anchor() {
        let src = include_str!("mod.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        let wheel = code.find("fn handle_mouse_wheel(").expect("wheel handler");
        let body = &code[wheel..];
        let audio = body.find("Some(\"audio\" | \"media\")").expect("audio arm");
        let gate = body
            .find("self.settings.wheel_volume_enabled")
            .expect("background gate");
        assert!(
            audio < gate,
            "chip volume must not depend on wheel_volume_enabled"
        );
        assert!(code.contains("adjust_volume_wheel(i32::from(delta))"));
        // Both wheel paths are de-duplicated.
        assert!(code.contains("layout::WheelSource::Window"));
        assert!(code.contains("layout::WheelSource::Hook"));
        // Every dock context menu wrapper receives the visible-pill anchor.
        assert_eq!(
            code.matches("self.dock_menu_anchor(),").count(),
            9,
            "pinned tray, bin, widget, start, taskview, app, dock, native, stack"
        );
        assert!(!code.contains(&["show_dock_context", "_menu(self.hwnd"].concat()));
        assert!(code.contains("open_widget_flyout_on_edge("));
        assert!(code.contains("handle_middle_click(wid, &self.settings)"));
        assert!(!code.contains("settings.show_media"));
    }
}
