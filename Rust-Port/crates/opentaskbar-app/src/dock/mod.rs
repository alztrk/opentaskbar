//! Native Win32 + DirectComposition floating dock window (Hybrid Architecture):
//! - Dock strip runs on pure DirectComposition visual transforms (0-1% GPU at 60-180 FPS)
//! - Settings window runs on demand in GPUI (`--settings`), freeing 100% of GPUI RAM on close
//! - Full multi-monitor support (per-monitor DPI + signed coordinates for left-of-primary screens)
//! - Auto-Hide (`Always` & `Smart` overlap detection) with smooth VSync slide-in/out
//! - Native live DWM window thumbnail previews (`DwmRegisterThumbnail`)
//! - Per-app / Task View / Dock context menus, Shift+Click / Ctrl+Shift+Click / Middle-click / Wheel
//! - File drag-and-drop (`WM_DROPFILES`), Dark/Light/System theme, Clock & CPU/RAM widgets, Show Desktop

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use opentaskbar_core::settings::{
    self, AppTheme, AutoHideMode, DockPosition, DockSettings, SurfaceBackground,
};
use opentaskbar_shell::tracker::{self, RunningWindow};
use windows::Win32::Graphics::DirectComposition::IDCompositionSurface;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::Shell::DragAcceptFiles;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect, HTCLIENT, HTTRANSPARENT,
    HWND_BROADCAST, IDC_ARROW, IDC_HAND, IDC_SIZEWE, IsIconic, IsWindowVisible, KillTimer,
    LoadCursorW, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
    SetCursor, SetTimer, SetWindowLongPtrW, TranslateMessage, WM_APP, WM_DESTROY, WM_DROPFILES,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCHITTEST, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

pub mod dcomp;
pub mod preview;
pub mod show_desktop;
pub mod tray;

use self::dcomp::{DcompEngine, PillNodes, ShellGlyph, VisualNode};
use self::preview::{PreviewItem, PreviewPopup};
use self::show_desktop::ShowDesktopCorner;
use crate::design;
use crate::platform::icon_cache::{self, RawIcon};
use crate::platform::pins::{self, PinnedApp};
use crate::platform::win32::{
    self, AppContextAction, ContextAction, MonitorInfo, RecycleBinAction, TaskViewAction,
};
use crate::ui::settings as settings_ui;
use crate::widgets::{self, WidgetRuntime};

const VERTICAL_EDGE_GAP: f32 = 5.0;
const TOOLTIP_HEADROOM: f32 = 36.0;
const MAG_HEADROOM: f32 = 26.0;
const FISHEYE_PEAK: f32 = 0.42;
const FISHEYE_RADIUS_SLOTS: f32 = 2.4;
const BOUNCE_DURATION_SECS: f32 = 0.42;
const CLOSE_DURATION: Duration = Duration::from_millis(240);
const DRAG_THRESHOLD_PX: f32 = 6.0;

const TIMER_POLL: usize = 1;
const TIMER_PREVIEW_SHOW: usize = 2;
const TIMER_PREVIEW_HIDE: usize = 3;
const TIMER_WIDGETS: usize = 4;
const TIMER_PREVIEW_WATCH: usize = 5;
const TIMER_WINDOW_CLOSE: usize = 6;
const RESIZE_EDGE_DIPS: f32 = 8.0;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_APP_VSYNC: u32 = WM_APP + 1;
const WM_APP_ICONS: u32 = WM_APP + 2;
const WM_APP_PREVIEW_LEAVE: u32 = WM_APP + 3;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn loword_i16(v: isize) -> f32 {
    (v as u32 & 0xffff) as i16 as f32
}

fn hiword_i16(v: isize) -> f32 {
    ((v as u32 >> 16) & 0xffff) as i16 as f32
}

fn shell_section_width(base: f32) -> f32 {
    base * 3.0 + 4.0 * 2.0 + 17.0
}

fn base_widgets_width(settings: &DockSettings) -> f32 {
    let mut w = 0.0;
    if settings.show_tray_button {
        w += settings.icon_dips() + 6.0;
    }
    if settings.show_recycle_bin {
        w += settings.icon_dips() + 6.0;
    }
    for wd in widgets::active_widgets(settings) {
        w += wd.width_dips + 6.0;
    }
    if w > 0.0 {
        w + 13.0 // left divider + breathing room
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
    let base = settings.icon_dips();
    let apps = slot_count as f32 * base + slot_count.saturating_sub(1) as f32 * slot_gap(settings);
    let magnification =
        if slot_count > 0 && settings.magnification_enabled && settings.animations_enabled {
            base * 1.4
        } else {
            0.0
        };
    let available = mon_w_dips
        - 16.0
        - shell_section_width(base)
        - base_widgets_width(settings)
        - apps
        - magnification
        - 30.0;
    ((available.max(0.0) / tray_pin_cell(base)).floor() as usize)
        .min(settings.user_pinned_background_apps.len())
}

fn widgets_section_width(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> f32 {
    base_widgets_width(settings)
        + visible_tray_pin_count(settings, slot_count, mon_w_dips) as f32
            * tray_pin_cell(settings.icon_dips())
}

fn tray_pin_rect(tray: DipRect, cell: f32, count: usize, index: usize) -> DipRect {
    let right = tray.0 - 6.0 - (count - 1 - index) as f32 * cell;
    (right - cell, tray.1, right, tray.3)
}

struct WidgetVisual {
    id: &'static str,
    width_dips: f32,
    bg: VisualNode,
    node: VisualNode,
    last_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeEdge {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
struct ResizeDragState {
    edge: ResizeEdge,
    start_screen_x: i32,
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

    fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Search => "Search",
            Self::TaskView => "Task View",
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
    slide_x: f32,
    bounce_started: Option<Instant>,
    close_started: Option<Instant>,
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
    dpi: f32,
    is_dark: bool,
    reload_msg: u32,
    settings: DockSettings,
    pins: Vec<PinnedApp>,
    slots: Vec<DockSlot>,
    dcomp: DcompEngine,
    pill: PillNodes,
    shell_bgs: [VisualNode; 3],
    shell_icons: [VisualNode; 3],
    shell_bg_hover: IDCompositionSurface,
    shell_bg_press: IDCompositionSurface,
    divider: VisualNode,
    widget_divider: VisualNode,
    tray_bg: VisualNode,
    tray_node: VisualNode,
    tray_pin_visuals: Vec<TrayPinVisual>,
    bin_bg: VisualNode,
    bin_node: VisualNode,
    bin_empty_surf: IDCompositionSurface,
    bin_full_surf: IDCompositionSurface,
    bin_is_full: bool,
    widget_visuals: Vec<WidgetVisual>,
    widget_runtime: WidgetRuntime,
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
    hovered_resize: Option<ResizeEdge>,
    pressed_idx: Option<usize>,
    pressed_shell: Option<ShellButton>,
    pressed_tray: bool,
    pressed_tray_pin: Option<String>,
    pressed_bin: bool,
    pressed_widget: Option<&'static str>,
    drag: Option<DragState>,
    resize_drag: Option<ResizeDragState>,
    tracking_mouse: bool,
    /// Auto-hide smooth animation (`0.0` = fully shown, `1.0` = slid off bottom edge).
    hide_progress: f32,
    target_hidden: bool,
    last_frame: Option<Instant>,
    last_window_rect: Option<(i32, i32, i32, i32)>,
    anim_signal: Arc<(Mutex<bool>, Condvar)>,
    tick_pending: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
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

fn build_slots(pins: &[PinnedApp], running: &[RunningWindow], prev: &[DockSlot]) -> Vec<DockSlot> {
    let mut by_exe: HashMap<String, Vec<&RunningWindow>> = HashMap::new();
    for win in running {
        by_exe
            .entry(win.exe_path.to_lowercase())
            .or_default()
            .push(win);
    }

    let mut pin_matches: HashMap<String, (Vec<PreviewItem>, bool)> = HashMap::new();
    let mut claimed_exes: HashMap<String, bool> = HashMap::new();
    for pin in pins {
        let matched_key = pin
            .target_exe
            .as_ref()
            .map(|p| p.to_lowercase())
            .filter(|k| by_exe.contains_key(k))
            .or_else(|| {
                let stem = pin.name.to_lowercase();
                by_exe
                    .keys()
                    .find(|k| {
                        Path::new(k)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|s| s.eq_ignore_ascii_case(&stem))
                    })
                    .cloned()
            });
        let (windows, is_fg) = match matched_key {
            Some(ref key) => {
                claimed_exes.insert(key.clone(), true);
                let list = &by_exe[key];
                (
                    list.iter()
                        .map(|w| PreviewItem {
                            hwnd: w.hwnd,
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
                    slide_x: old.slide_x,
                    bounce_started: old.bounce_started,
                    close_started: None,
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
                        title: w.title.clone(),
                    })
                    .collect(),
                is_foreground: list.iter().any(|w| w.is_foreground),
                scale: old.scale,
                slide_x: old.slide_x,
                bounce_started: old.bounce_started,
                close_started: None,
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
            slide_x: 0.0,
            bounce_started: None,
            close_started: None,
        });
    }

    for win in running {
        let key = win.exe_path.to_lowercase();
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
                    title: w.title.clone(),
                })
                .collect(),
            is_foreground: list.iter().any(|w| w.is_foreground),
            scale: 1.0,
            slide_x: 0.0,
            bounce_started: None,
            close_started: None,
        });
    }

    next
}

fn close_progress(started: Option<Instant>, now: Instant) -> f32 {
    started.map_or(0.0, |started| {
        let time = (now.saturating_duration_since(started).as_secs_f32()
            / CLOSE_DURATION.as_secs_f32())
        .min(1.0);
        1.0 - (1.0 - time).powi(2)
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

fn pill_height_dips(settings: &DockSettings) -> f32 {
    settings.icon_dips() + 22.0
}

fn slot_gap(settings: &DockSettings) -> f32 {
    9.0 + settings.dock_icon_padding.max(0) as f32
}

fn content_min_pill_width(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> f32 {
    let base = settings.icon_dips();
    let gap = slot_gap(settings);
    let icons_w = (slot_count as f32) * base + gap * (slot_count.saturating_sub(1) as f32);
    (shell_section_width(base)
        + icons_w
        + widgets_section_width(settings, slot_count, mon_w_dips)
        + 30.0)
        .max(160.0)
}

fn resting_pill_width(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> f32 {
    let max_w = (mon_w_dips - 16.0).max(200.0);
    if settings.full_width {
        return max_w;
    }
    let min_w = content_min_pill_width(settings, slot_count, mon_w_dips);
    let target = settings
        .dock_custom_width_dips
        .map_or(min_w, |custom| custom.max(min_w));
    target.min(max_w)
}

fn compute_window_dips(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> (f32, f32) {
    let base = settings.icon_dips();
    let resting_w = resting_pill_width(settings, slot_count, mon_w_dips);
    let mag_extra = if settings.magnification_enabled && !settings.full_width {
        base * 1.4
    } else {
        0.0
    };
    let width_dips = (resting_w + mag_extra + 24.0).min(mon_w_dips).ceil();
    let pill_h = pill_height_dips(settings);
    let height_dips = (pill_h + TOOLTIP_HEADROOM + MAG_HEADROOM + VERTICAL_EDGE_GAP).ceil();
    (width_dips, height_dips)
}

/// Computes exact signed physical coordinates for a monitor (supports negative X on left monitors).
fn compute_window_rect(
    settings: &DockSettings,
    slot_count: usize,
    monitor: MonitorInfo,
) -> (i32, i32, i32, i32) {
    let dpi = monitor.dpi_scale;
    let mon_w_dips = monitor.width() as f32 / dpi;
    let (width_dips, height_dips) = compute_window_dips(settings, slot_count, mon_w_dips);
    let w_px = ((width_dips * dpi).ceil() as i32).min(monitor.width());
    let h_px = ((height_dips * dpi).ceil() as i32).min(monitor.height());
    let margin_px = (12.0 * dpi).round() as i32;
    let x_px = if settings.full_width {
        monitor.left + (monitor.width() - w_px) / 2
    } else {
        match settings.position {
            DockPosition::Left => monitor.left + margin_px,
            DockPosition::Center => monitor.left + (monitor.width() - w_px) / 2,
            DockPosition::Right => monitor.right - w_px - margin_px,
        }
    };
    let y_px = monitor.bottom - h_px;
    (x_px, y_px, w_px, h_px)
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

fn spawn_settings_window() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .arg(settings_ui::SETTINGS_ARG)
        .spawn();
}

impl DockState {
    fn new(
        hwnd: usize,
        monitor: MonitorInfo,
        reload_msg: u32,
        settings: DockSettings,
        pins: Vec<PinnedApp>,
        initial_slots: Vec<DockSlot>,
        initial_rect: (i32, i32, i32, i32),
    ) -> windows::core::Result<Self> {
        let dpi = monitor.dpi_scale;
        let is_dark = resolve_is_dark(settings.theme);
        let dcomp = DcompEngine::new(hwnd)?;
        let base = settings.icon_dips();
        let pill_h = pill_height_dips(&settings);
        let radius = settings.corner_radius_dips();

        let pal = design::palette(is_dark);
        let (pill_rgb, bd_rgb, hover_rgb, press_rgb, div_rgb, dot_run_rgb, dot_act_rgb) = if is_dark
        {
            (
                pal.dock_pill,
                (0xff, 0xff, 0xff, 0.11),
                (0xff, 0xff, 0xff, 0.10),
                (0xff, 0xff, 0xff, 0.16),
                (0xff, 0xff, 0xff, 0.14),
                (0xd4, 0xd4, 0xd8, 0.85),
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

        let bg_alpha = match settings.dock_background {
            SurfaceBackground::None => 0.0_f32,
            SurfaceBackground::Solid => 0.98_f32,
            SurfaceBackground::Glass => (settings.dock_opacity.clamp(0.1, 1.0) * 0.88) as f32,
        };
        let bd_alpha = if settings.dock_background == SurfaceBackground::None {
            0.0
        } else {
            bd_rgb.3
        };
        let pill = dcomp.create_pill_nodes(
            radius * dpi,
            pill_h * dpi,
            (pill_rgb.0, pill_rgb.1, pill_rgb.2, bg_alpha),
            (bd_rgb.0, bd_rgb.1, bd_rgb.2, bd_alpha),
            dpi,
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
        let bin_empty_surf =
            dcomp.create_shell_glyph_surface(ShellGlyph::RecycleBinEmpty, glyph_px, is_dark)?;
        let bin_full_surf =
            dcomp.create_shell_glyph_surface(ShellGlyph::RecycleBinFull, glyph_px, is_dark)?;
        let bin_is_full = widgets::is_recycle_bin_non_empty();
        bin_node.set_content(Some(if bin_is_full {
            &bin_full_surf
        } else {
            &bin_empty_surf
        }));

        let corner_h = pill_h + settings.dock_bottom_gap_pixels.max(0) as f32;
        let show_desktop_corner =
            ShowDesktopCorner::new(monitor, corner_h, is_dark, settings.show_desktop_button);

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

        let mut state = Self {
            hwnd,
            monitor,
            dpi,
            is_dark,
            reload_msg,
            settings,
            pins,
            slots: initial_slots,
            dcomp,
            pill,
            shell_bgs,
            shell_icons,
            shell_bg_hover,
            shell_bg_press,
            divider,
            widget_divider,
            tray_bg,
            tray_node,
            tray_pin_visuals: Vec::new(),
            bin_bg,
            bin_node,
            bin_empty_surf,
            bin_full_surf,
            bin_is_full,
            widget_visuals: Vec::new(),
            widget_runtime: WidgetRuntime::default(),
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
            hovered_resize: None,
            pressed_idx: None,
            pressed_shell: None,
            pressed_tray: false,
            pressed_tray_pin: None,
            pressed_bin: false,
            pressed_widget: None,
            drag: None,
            resize_drag: None,
            tracking_mouse: false,
            hide_progress: 0.0,
            target_hidden: false,
            last_frame: None,
            last_window_rect: Some(initial_rect),
            anim_signal,
            tick_pending,
            shutdown,
        };
        state.rebuild_visual_tree();
        state.refresh_widgets();
        state.request_missing_icons();
        state.update_visuals();
        state.spawn_vsync_thread();
        Ok(state)
    }

    fn spawn_vsync_thread(&self) {
        let hwnd_val = self.hwnd;
        let signal = self.anim_signal.clone();
        let tick_pending = self.tick_pending.clone();
        let shutdown = self.shutdown.clone();
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
                    unsafe {
                        let _ = DwmFlush();
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

    fn ensure_animation(&mut self) {
        if self.last_frame.is_none() {
            self.last_frame = Some(Instant::now());
        }
        self.set_animating(true);
    }

    fn rebuild_visual_tree(&mut self) {
        self.dcomp.remove_all_children(&self.dcomp.root);
        self.dcomp.add_child(&self.dcomp.root, &self.pill.left);
        self.dcomp.add_child(&self.dcomp.root, &self.pill.center);
        self.dcomp.add_child(&self.dcomp.root, &self.pill.right);
        for i in 0..3 {
            self.dcomp.add_child(&self.dcomp.root, &self.shell_bgs[i]);
            self.dcomp.add_child(&self.dcomp.root, &self.shell_icons[i]);
        }
        self.dcomp.add_child(&self.dcomp.root, &self.divider);

        while self.slot_visuals.len() < self.slots.len() {
            if let (Ok(icon), Ok(dot)) = (self.dcomp.create_node(), self.dcomp.create_node()) {
                icon.hide();
                dot.hide();
                self.slot_visuals.push(SlotVisuals {
                    icon,
                    dot,
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
            self.dcomp.add_child(&self.dcomp.root, &sv.icon);
            self.dcomp.add_child(&self.dcomp.root, &sv.dot);
        }

        self.dcomp.add_child(&self.dcomp.root, &self.widget_divider);
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
            });
        }
        for pin in &self.tray_pin_visuals {
            self.dcomp.add_child(&self.dcomp.root, &pin.bg);
            self.dcomp.add_child(&self.dcomp.root, &pin.icon);
        }
        self.dcomp.add_child(&self.dcomp.root, &self.tray_bg);
        self.dcomp.add_child(&self.dcomp.root, &self.tray_node);
        self.dcomp.add_child(&self.dcomp.root, &self.bin_bg);
        self.dcomp.add_child(&self.dcomp.root, &self.bin_node);
        for wv in &self.widget_visuals {
            self.dcomp.add_child(&self.dcomp.root, &wv.bg);
            self.dcomp.add_child(&self.dcomp.root, &wv.node);
        }

        self.dcomp.add_child(&self.dcomp.root, &self.drag_ghost);
        self.dcomp.add_child(&self.dcomp.root, &self.tooltip_node);
    }

    fn sync_widget_visuals(&mut self) {
        let active = widgets::active_widgets(&self.settings);
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
                self.widget_visuals.push(WidgetVisual {
                    id: wd.id,
                    width_dips: wd.width_dips,
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
        let mut changed = false;
        let widget_h = design::widget_height_dips(pill_height_dips(&self.settings));
        for wv in &mut self.widget_visuals {
            let spec = self
                .widget_runtime
                .chip_spec(wv.id, &self.settings, self.is_dark);
            let key = format!("{}|{widget_h:.1}|{}", spec.cache_key(), self.is_dark);
            if key != wv.last_key
                && let Ok(surf) = self.dcomp.create_widget_chip_surface(
                    &spec,
                    wv.width_dips,
                    widget_h,
                    self.dpi,
                    self.is_dark,
                )
            {
                wv.node.set_content(Some(&surf));
                wv.last_key = key;
                changed = true;
            }
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

    fn evaluate_auto_hide(&mut self) {
        let pointer_busy = self.tracking_mouse
            || self.drag.is_some()
            || self
                .preview
                .as_deref()
                .is_some_and(|p| p.is_visible() || p.is_pointer_inside());

        let should_hide = match self.settings.auto_hide {
            AutoHideMode::Disabled => false,
            AutoHideMode::Always => !pointer_busy,
            AutoHideMode::Smart => {
                if pointer_busy {
                    false
                } else {
                    self.is_overlapped_by_window()
                }
            }
        };

        if self.target_hidden != should_hide {
            self.target_hidden = should_hide;
            self.ensure_animation();
        }
    }

    /// Checks whether any visible non-minimized window overlaps this monitor's dock shelf
    /// (Issue #85: re-evaluated on poll so moving/restoring windows immediately reveals dock).
    fn is_overlapped_by_window(&self) -> bool {
        let pill_h_px =
            ((pill_height_dips(&self.settings) + VERTICAL_EDGE_GAP) * self.dpi).ceil() as i32;
        let shelf_top = self.monitor.bottom - pill_h_px;
        let running = tracker::enumerate_windows();
        for win in running {
            let hwnd = win.hwnd as HWND;
            unsafe {
                if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                    continue;
                }
                let mut rc: RECT = std::mem::zeroed();
                if GetWindowRect(hwnd, &mut rc) == 0 {
                    continue;
                }
                let horiz_overlap = rc.right > self.monitor.left && rc.left < self.monitor.right;
                let vert_overlap = rc.bottom > shelf_top && rc.top < self.monitor.bottom;
                if horiz_overlap && vert_overlap {
                    return true;
                }
            }
        }
        false
    }

    fn rebuild_theme_surfaces(&mut self) {
        let dpi = self.dpi;
        let is_dark = self.is_dark;
        let base = self.settings.icon_dips();
        let pill_h = pill_height_dips(&self.settings);
        let radius = self.settings.corner_radius_dips();

        let pal = design::palette(is_dark);
        let (pill_rgb, bd_rgb, hover_rgb, press_rgb, div_rgb, dot_run_rgb, dot_act_rgb) = if is_dark
        {
            (
                pal.dock_pill,
                (0xff, 0xff, 0xff, 0.11),
                (0xff, 0xff, 0xff, 0.10),
                (0xff, 0xff, 0xff, 0.16),
                (0xff, 0xff, 0xff, 0.14),
                (0xd4, 0xd4, 0xd8, 0.85),
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

        let bg_alpha = match self.settings.dock_background {
            SurfaceBackground::None => 0.0_f32,
            SurfaceBackground::Solid => 0.98_f32,
            SurfaceBackground::Glass => (self.settings.dock_opacity.clamp(0.1, 1.0) * 0.88) as f32,
        };
        let bd_alpha = if self.settings.dock_background == SurfaceBackground::None {
            0.0
        } else {
            bd_rgb.3
        };
        if let Ok(pill) = self.dcomp.create_pill_nodes(
            radius * dpi,
            pill_h * dpi,
            (pill_rgb.0, pill_rgb.1, pill_rgb.2, bg_alpha),
            (bd_rgb.0, bd_rgb.1, bd_rgb.2, bd_alpha),
            dpi,
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
        if let Ok(s) =
            self.dcomp
                .create_shell_glyph_surface(ShellGlyph::TrayChevron, glyph_px, is_dark)
        {
            self.tray_node.set_content(Some(&s));
        }
        if let Ok(s) =
            self.dcomp
                .create_shell_glyph_surface(ShellGlyph::RecycleBinEmpty, glyph_px, is_dark)
        {
            self.bin_empty_surf = s;
        }
        if let Ok(s) =
            self.dcomp
                .create_shell_glyph_surface(ShellGlyph::RecycleBinFull, glyph_px, is_dark)
        {
            self.bin_full_surf = s;
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
        self.rebuild_visual_tree();
    }

    fn reload_settings_live(&mut self) {
        let next =
            opentaskbar_core::settings::load_from(&opentaskbar_core::settings::settings_path());
        self.settings = next;
        self.hovered_tray_pin = None;
        self.pressed_tray_pin = None;
        self.is_dark = resolve_is_dark(self.settings.theme);
        self.pins = load_effective_pins(&self.settings);
        for wv in &mut self.widget_visuals {
            wv.last_key.clear();
        }
        self.tooltip_cache = None;
        self.rebuild_theme_surfaces();
        if let Some(ref corner) = self.show_desktop_corner {
            let corner_h = pill_height_dips(&self.settings)
                + self.settings.dock_bottom_gap_pixels.max(0) as f32;
            corner.update(
                self.monitor,
                corner_h,
                self.is_dark,
                self.settings.show_desktop_button,
            );
        }
        if self.monitor.is_primary {
            let auto_hide = self.settings.auto_hide != AutoHideMode::Disabled;
            let desired_total_dips = pill_height_dips(&self.settings) + VERTICAL_EDGE_GAP * 2.0;
            let desired_total_px = (desired_total_dips * self.dpi).ceil() as i32;
            opentaskbar_shell::taskbar::update_appbar_height(
                self.hwnd,
                auto_hide,
                desired_total_px,
            );
        }
        self.sync_slots(false);
        self.sync_window_bounds();
        self.refresh_widgets();
        self.evaluate_auto_hide();
        self.update_visuals();
    }

    fn request_missing_icons(&mut self) {
        let mut missing = Vec::new();
        let sources = self.slots.iter().map(|slot| &slot.icon_source).chain(
            self.settings
                .user_pinned_background_apps
                .iter()
                .take(self.tray_pin_visuals.len()),
        );
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
        let running = tracker::enumerate_windows();
        let mut next = build_slots(&self.pins, &running, &self.slots);
        if animate_closes
            && self.settings.animations_enabled
            && win32::client_area_animations_enabled()
        {
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
            self.update_hovered_index_from_mouse();
            self.sync_preview_hover();
        }
        if self.slots.iter().any(|slot| slot.close_started.is_some()) {
            self.ensure_animation();
        }
        self.request_missing_icons();
        changed
    }

    fn sync_window_bounds(&mut self) {
        let rect = compute_window_rect(&self.settings, self.slots.len(), self.monitor);
        if self.last_window_rect != Some(rect) {
            self.last_window_rect = Some(rect);
            win32::reposition_dock_window(self.hwnd, rect.0, rect.1, rect.2, rect.3);
        }
    }

    fn mon_w_dips(&self) -> f32 {
        self.monitor.width() as f32 / self.dpi
    }

    fn live_pill_geometry(&self) -> (f32, f32, f32, f32) {
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let mon_w = self.mon_w_dips();
        let (win_w, win_h) = compute_window_dips(&self.settings, self.slots.len(), mon_w);
        let pill_w = if self.settings.full_width {
            (mon_w - 16.0).max(200.0)
        } else {
            let now = Instant::now();
            let mut icons_w = 0.0;
            let mut largest_fraction = 0.0_f32;
            for slot in &self.slots {
                let fraction = slot.layout_fraction(now);
                icons_w += (base * slot.scale + gap) * fraction;
                largest_fraction = largest_fraction.max(fraction);
            }
            icons_w -= gap * largest_fraction;
            let live_content = (shell_section_width(base)
                + icons_w
                + widgets_section_width(&self.settings, self.slots.len(), mon_w)
                + 30.0)
                .max(160.0);
            let target = self
                .settings
                .dock_custom_width_dips
                .map_or(live_content, |custom| custom.max(live_content));
            target.min(win_w)
        };
        let pill_h = pill_height_dips(&self.settings);
        let pill_left = (win_w - pill_w) * 0.5;
        let pill_top = win_h - VERTICAL_EDGE_GAP - pill_h;
        (pill_left, pill_top, pill_w, pill_h)
    }

    fn hit_resize_edge(&self, mx: f32, my: f32) -> Option<ResizeEdge> {
        if self.settings.full_width {
            return None;
        }
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
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

    fn resting_slot_center_x(&self, idx: usize) -> f32 {
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let mon_w = self.mon_w_dips();
        let (win_w, _) = compute_window_dips(&self.settings, self.slots.len(), mon_w);
        let pill_w = resting_pill_width(&self.settings, self.slots.len(), mon_w);
        let pill_left = (win_w - pill_w) * 0.5;
        let apps_left = pill_left + 15.0 + shell_section_width(base);
        let now = Instant::now();
        let before: f32 = self
            .slots
            .iter()
            .take(idx)
            .map(|slot| (base + gap) * slot.layout_fraction(now))
            .sum();
        apps_left + before + base * 0.5
    }

    fn live_slot_center_x(&self, idx: usize) -> f32 {
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let (pill_left, _, _, _) = self.live_pill_geometry();
        let mut cursor = pill_left + 15.0 + shell_section_width(base);
        let now = Instant::now();
        for i in 0..idx.min(self.slots.len()) {
            let slot = &self.slots[i];
            cursor += (base * slot.scale + gap) * slot.layout_fraction(now);
        }
        let w = self.slots.get(idx).map_or(base, |s| base * s.scale);
        cursor + w * 0.5
    }

    fn live_shell_center_x(&self, kind: ShellButton) -> f32 {
        let base = self.settings.icon_dips();
        let (pill_left, _, _, _) = self.live_pill_geometry();
        pill_left + 15.0 + (kind.index() as f32) * (base + 4.0) + base * 0.5
    }

    fn right_widget_rects_dips(
        &self,
    ) -> (
        Option<DipRect>,
        Option<DipRect>,
        Vec<(&'static str, DipRect)>,
    ) {
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        let mut right_cursor = pill_left + pill_w - 12.0;
        let mut widget_rects = Vec::with_capacity(self.widget_visuals.len());
        for wv in self.widget_visuals.iter().rev() {
            let l = right_cursor - wv.width_dips;
            let r = right_cursor;
            right_cursor -= wv.width_dips + 6.0;
            widget_rects.push((wv.id, (l, pill_top + 6.0, r, pill_top + pill_h - 6.0)));
        }
        let bin = if self.settings.show_recycle_bin {
            let bw = self.settings.icon_dips();
            let l = right_cursor - bw;
            let r = right_cursor;
            right_cursor -= bw + 6.0;
            Some((l, pill_top + 6.0, r, pill_top + pill_h - 6.0))
        } else {
            None
        };
        let tray = if self.settings.show_tray_button {
            let tw = self.settings.icon_dips();
            let l = right_cursor - tw;
            let r = right_cursor;
            Some((l, pill_top + 6.0, r, pill_top + pill_h - 6.0))
        } else {
            None
        };
        (tray, bin, widget_rects)
    }

    fn flyout_anchor_px(&self, rect: DipRect) -> (i32, i32) {
        let (_, pill_top, _, _) = self.live_pill_geometry();
        let mut win_rc: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            GetWindowRect(self.hwnd as HWND, &mut win_rc);
        }
        let cx_dips = (rect.0 + rect.2) * 0.5;
        let anchor_x = win_rc.left + (cx_dips * self.dpi).round() as i32;
        let anchor_y = win_rc.top + ((pill_top - 8.0) * self.dpi).round() as i32;
        (anchor_x, anchor_y)
    }

    fn hit_rect(rc: Option<(f32, f32, f32, f32)>, mx: f32, my: f32) -> bool {
        rc.is_some_and(|(l, t, r, b)| mx >= l && mx <= r && my >= t && my <= b)
    }

    fn hit_tray_pin(&self, tray: Option<DipRect>, mx: f32, my: f32) -> Option<usize> {
        let tray = tray?;
        let cell = tray_pin_cell(self.settings.icon_dips());
        let count = self.tray_pin_visuals.len();
        (0..count)
            .find(|&index| Self::hit_rect(Some(tray_pin_rect(tray, cell, count, index)), mx, my))
    }

    fn hit_test_dips(&self, mx: f32, my: f32) -> bool {
        let (_, win_h) = compute_window_dips(&self.settings, self.slots.len(), self.mon_w_dips());
        if self.hide_progress > 0.5 {
            // Keep a 4px bottom edge trigger strip active so touching the edge reveals the dock.
            return my >= win_h - 4.0;
        }
        if self.drag.is_some() {
            return true;
        }
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        if mx >= pill_left && mx <= pill_left + pill_w && my >= pill_top && my <= pill_top + pill_h
        {
            return true;
        }
        let base = self.settings.icon_dips();
        let icon_base_bottom = pill_top + 7.5 + base;
        for (idx, slot) in self.slots.iter().enumerate() {
            if slot.is_exiting() {
                continue;
            }
            let dim = base * slot.scale;
            let cx = self.live_slot_center_x(idx) + slot.slide_x;
            let bounce = bounce_offset_dips(slot.bounce_started).unwrap_or(0.0);
            let lift = ((slot.scale - 1.0) * 14.0 + bounce).max(0.0);
            let top = icon_base_bottom - lift - dim;
            if mx >= cx - dim * 0.5 && mx <= cx + dim * 0.5 && my >= top && my <= icon_base_bottom {
                return true;
            }
        }
        false
    }

    fn hit_shell_button(&self, mx: f32, my: f32) -> Option<ShellButton> {
        let base = self.settings.icon_dips();
        let (pill_left, pill_top, _, pill_h) = self.live_pill_geometry();
        if my < pill_top || my > pill_top + pill_h {
            return None;
        }
        for kind in ShellButton::ALL {
            let x = pill_left + 15.0 + (kind.index() as f32) * (base + 4.0);
            if mx >= x && mx <= x + base {
                return Some(kind);
            }
        }
        None
    }

    fn update_hovered_index_from_mouse(&mut self) {
        let Some(mx) = self.hover_mouse_x else {
            self.hovered_idx = None;
            return;
        };
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let max_dist = (base + gap) * 0.55;
        self.hovered_idx = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !slot.is_exiting())
            .map(|(i, _)| (i, (mx - self.resting_slot_center_x(i)).abs()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, d)| *d <= max_dist)
            .map(|(i, _)| i);
    }

    fn target_scale_for(&self, idx: usize) -> f32 {
        if self.drag.is_some_and(|d| d.active) {
            return 1.0;
        }
        if !self.settings.magnification_enabled || !self.settings.animations_enabled {
            return 1.0;
        }
        let Some(mx) = self.hover_mouse_x else {
            return 1.0;
        };
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let radius = (base + gap) * FISHEYE_RADIUS_SLOTS;
        let dist = (mx - self.resting_slot_center_x(idx)).abs();
        if dist >= radius {
            return 1.0;
        }
        let c = (dist / radius * std::f32::consts::FRAC_PI_2).cos();
        1.0 + FISHEYE_PEAK * c * c
    }

    fn step_animations(&mut self, now: Instant) -> bool {
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

        let scale_k = 1.0 - (-22.0 * dt).exp();
        let slide_k = 1.0 - (-24.0 * dt).exp();
        let hide_k = 1.0 - (-16.0 * dt).exp();
        let mut moving = self.drag.is_some_and(|d| d.active);

        let hide_target = if self.target_hidden { 1.0 } else { 0.0 };
        let hide_diff = hide_target - self.hide_progress;
        if hide_diff.abs() > 0.004 {
            self.hide_progress += hide_diff * hide_k;
            moving = true;
        } else if (self.hide_progress - hide_target).abs() > 0.0 {
            self.hide_progress = hide_target;
        }

        for idx in 0..self.slots.len() {
            if self.slots[idx].close_started.is_some() {
                if self.slots[idx].is_exiting()
                    || close_progress(self.slots[idx].close_started, now) < 1.0
                {
                    moving = true;
                } else {
                    self.slots[idx].close_started = None;
                }
            }
            if self.slots[idx].is_exiting() {
                continue;
            }
            let target = self.target_scale_for(idx);
            let cur = self.slots[idx].scale;
            let diff = target - cur;
            if diff.abs() > 0.004 {
                self.slots[idx].scale = cur + diff * scale_k;
                moving = true;
            } else if (cur - target).abs() > 0.0 {
                self.slots[idx].scale = target;
            }

            let sx = self.slots[idx].slide_x;
            if sx.abs() > 0.35 {
                self.slots[idx].slide_x = sx * (1.0 - slide_k);
                moving = true;
            } else if sx != 0.0 {
                self.slots[idx].slide_x = 0.0;
            }

            if let Some(started) = self.slots[idx].bounce_started {
                if bounce_offset_dips(Some(started)).is_some() {
                    moving = true;
                } else {
                    self.slots[idx].bounce_started = None;
                }
            }
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
        }
        if drag.active && !self.slots.is_empty() {
            let center_x = mx - drag.grab_offset_x;
            let target = self
                .slots
                .iter()
                .enumerate()
                .map(|(i, _)| (i, (center_x - self.resting_slot_center_x(i)).abs()))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
                .unwrap_or(drag.idx);
            if target != drag.idx && target < self.slots.len() && drag.idx < self.slots.len() {
                let step = self.settings.icon_dips() + slot_gap(&self.settings);
                let old_idx = drag.idx;
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
            return true;
        }
        false
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
        let created = self.dcomp.create_fallback_tile_surface(&ch, 64).ok()?;
        self.fallback_surfaces.insert(ch, created.clone());
        Some(created)
    }

    fn update_tray_pin_visuals(&mut self, tray: DipRect, icon_top: f32) {
        let base = self.settings.icon_dips();
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
                let surface =
                    surface.or_else(|| self.fallback_surface_for(&tray::friendly_name(&path)));
                let visual = &mut self.tray_pin_visuals[index];
                visual.icon.set_content(surface.as_ref());
                visual.source = path;
                visual.has_icon = has_icon;
            }
            let path = &self.settings.user_pinned_background_apps[index];
            let pressed = self.pressed_tray_pin.as_deref() == Some(path);
            let visual = &self.tray_pin_visuals[index];
            let (left, _, right, _) = tray_pin_rect(tray, cell, count, index);
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
            let scale = dim * dpi / 64.0;
            visual.icon.set_transform(
                scale,
                scale,
                (center - dim * 0.5) * dpi,
                (icon_top + (base - dim) * 0.5) * dpi,
            );
        }
    }

    fn update_visuals(&mut self) {
        let dpi = self.dpi;
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let (win_w, win_h) =
            compute_window_dips(&self.settings, self.slots.len(), self.mon_w_dips());
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();

        // Auto-hide slide translation on root visual (zero child recalculation).
        let hide_shift_px =
            (self.hide_progress * (pill_h + VERTICAL_EDGE_GAP + 10.0) * dpi).round();
        self.dcomp.set_root_offset_y(hide_shift_px);

        // 1. 3-piece pill transform.
        let cap_w_px = self.pill.cap_w_px;
        let pill_left_px = (pill_left * dpi).round();
        let pill_top_px = (pill_top * dpi).round();
        let pill_w_px = (pill_w * dpi).round().max(cap_w_px * 2.0 + 1.0);
        let center_w_px = (pill_w_px - cap_w_px * 2.0).max(1.0);
        self.pill
            .left
            .set_transform(1.0, 1.0, pill_left_px, pill_top_px);
        self.pill
            .center
            .set_transform(center_w_px, 1.0, pill_left_px + cap_w_px, pill_top_px);
        self.pill
            .right
            .set_transform(1.0, 1.0, pill_left_px + pill_w_px - cap_w_px, pill_top_px);

        let icon_base_top = pill_top + 7.5;
        let icon_base_bottom = icon_base_top + base;
        let dot_top = icon_base_bottom + 4.5;

        // 2. Shell buttons + left divider.
        let shell_top = icon_base_top;
        let glyph_dips = (base * 0.68).max(22.0);
        for kind in ShellButton::ALL {
            let i = kind.index();
            let x = pill_left + 15.0 + (i as f32) * (base + 4.0);
            let hovered = self.hovered_shell == Some(kind);
            let pressed = self.pressed_shell == Some(kind);
            if pressed {
                self.shell_bgs[i].set_content(Some(&self.shell_bg_press));
                self.shell_bgs[i].set_transform(
                    1.0,
                    1.0,
                    (x * dpi).round(),
                    (shell_top * dpi).round(),
                );
            } else if hovered {
                self.shell_bgs[i].set_content(Some(&self.shell_bg_hover));
                self.shell_bgs[i].set_transform(
                    1.0,
                    1.0,
                    (x * dpi).round(),
                    (shell_top * dpi).round(),
                );
            } else {
                self.shell_bgs[i].hide();
            }

            let scale = if pressed { 0.88 } else { 1.0 };
            let drawn = glyph_dips * scale;
            let gx = x + (base - drawn) * 0.5;
            let gy = shell_top + (base - drawn) * 0.5;
            self.shell_icons[i].set_transform(scale, scale, gx * dpi, gy * dpi);
        }

        let div_x = pill_left + 15.0 + base * 3.0 + 4.0 * 2.0 + 8.0;
        let div_y = pill_top + (pill_h - 26.0) * 0.5;
        self.divider
            .set_transform(1.0, 1.0, (div_x * dpi).round(), (div_y * dpi).round());

        // 3. Unified app slots.
        let active_drag = self.drag.filter(|d| d.active);
        let mut cursor_x = pill_left + 15.0 + shell_section_width(base);
        let slot_count = self.slots.len().min(self.slot_visuals.len());
        let now = Instant::now();
        for idx in 0..slot_count {
            let slot_w = base * self.slots[idx].scale;
            let dragging_this = active_drag.is_some_and(|d| d.idx == idx);
            let pressed = self.pressed_idx == Some(idx) && !dragging_this;
            let press_factor = if pressed { 0.88 } else { 1.0 };
            let dim = slot_w * press_factor;
            let bounce = bounce_offset_dips(self.slots[idx].bounce_started).unwrap_or(0.0);
            let lift = ((self.slots[idx].scale - 1.0) * 14.0 + bounce
                - if pressed { 2.0 } else { 0.0 })
            .max(0.0);
            let slide_x = self.slots[idx].slide_x;
            let source = self.slots[idx].icon_source.clone();
            let label = self.slots[idx].label.clone();
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
                .get(&source)
                .and_then(|o| o.as_ref())
                .is_some();
            if self.slot_visuals[idx].bound_source != source
                || self.slot_visuals[idx].bound_has_icon != has_real_icon
            {
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
                    let remaining = 1.0 - close;
                    (dim * remaining.powi(2), dim * remaining, dim * 0.28 * close)
                } else {
                    (
                        dim * (1.0 - 0.20 * pulse),
                        dim * (1.0 - 0.18 * pulse),
                        dim * 0.10 * pulse,
                    )
                };
                let slot_center = cursor_x + slot_w * self.slots[idx].layout_fraction(now) * 0.5;
                let ix = slot_center + slide_x - width * 0.5;
                let iy =
                    icon_base_bottom - lift * self.slots[idx].layout_fraction(now) + sink - height;
                self.slot_visuals[idx].icon.set_transform(
                    width * dpi / 64.0,
                    height * dpi / 64.0,
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
                    let dx = cursor_x + slide_x + (slot_w - dot_w) * 0.5;
                    self.slot_visuals[idx].dot.set_transform(
                        1.0,
                        1.0,
                        (dx * dpi).round(),
                        (dot_top * dpi).round(),
                    );
                }
            }

            cursor_x += (slot_w + gap) * self.slots[idx].layout_fraction(now);
        }

        // 4. Right-side widget strip (Background Apps, Recycle Bin, user-enabled Widgets).
        let widgets_w = widgets_section_width(&self.settings, self.slots.len(), self.mon_w_dips());
        if widgets_w > 0.0 {
            let mut right_cursor = pill_left + pill_w - 12.0;
            let widget_h = design::widget_height_dips(pill_h);
            let widget_top = pill_top + (pill_h - widget_h) * 0.5;

            for wv in self.widget_visuals.iter().rev() {
                let wx = right_cursor - wv.width_dips;
                let is_pressed = self.pressed_widget == Some(wv.id);
                let is_hovered = self.hovered_widget == Some(wv.id);
                if is_pressed || is_hovered {
                    wv.bg.set_content(Some(if is_pressed {
                        &self.shell_bg_press
                    } else {
                        &self.shell_bg_hover
                    }));
                    wv.bg.set_transform(
                        wv.width_dips / base,
                        widget_h / base,
                        (wx * dpi).round(),
                        (widget_top * dpi).round(),
                    );
                } else {
                    wv.bg.hide();
                }
                wv.node
                    .set_transform(1.0, 1.0, (wx * dpi).round(), (widget_top * dpi).round());
                right_cursor -= wv.width_dips + 6.0;
            }

            if self.settings.show_recycle_bin {
                let bx_box = right_cursor - base;
                if self.pressed_bin {
                    self.bin_bg.set_content(Some(&self.shell_bg_press));
                    self.bin_bg.set_transform(
                        1.0,
                        1.0,
                        (bx_box * dpi).round(),
                        (icon_base_top * dpi).round(),
                    );
                } else if self.hovered_bin {
                    self.bin_bg.set_content(Some(&self.shell_bg_hover));
                    self.bin_bg.set_transform(
                        1.0,
                        1.0,
                        (bx_box * dpi).round(),
                        (icon_base_top * dpi).round(),
                    );
                } else {
                    self.bin_bg.hide();
                }
                let scale = if self.pressed_bin { 0.88 } else { 1.0 };
                let drawn = glyph_dips * scale;
                let bx = bx_box + (base - drawn) * 0.5;
                let by = icon_base_top + (base - drawn) * 0.5;
                self.bin_node
                    .set_transform(scale, scale, bx * dpi, by * dpi);
                right_cursor -= base + 6.0;
            } else {
                self.bin_bg.hide();
                self.bin_node.hide();
            }

            if self.settings.show_tray_button {
                let tx_box = right_cursor - base;
                if self.pressed_tray {
                    self.tray_bg.set_content(Some(&self.shell_bg_press));
                    self.tray_bg.set_transform(
                        1.0,
                        1.0,
                        (tx_box * dpi).round(),
                        (icon_base_top * dpi).round(),
                    );
                } else if self.hovered_tray {
                    self.tray_bg.set_content(Some(&self.shell_bg_hover));
                    self.tray_bg.set_transform(
                        1.0,
                        1.0,
                        (tx_box * dpi).round(),
                        (icon_base_top * dpi).round(),
                    );
                } else {
                    self.tray_bg.hide();
                }
                let scale = if self.pressed_tray { 0.88 } else { 1.0 };
                let drawn = glyph_dips * scale;
                let tx = tx_box + (base - drawn) * 0.5;
                let ty = icon_base_top + (base - drawn) * 0.5;
                self.tray_node
                    .set_transform(scale, scale, tx * dpi, ty * dpi);
                let tray_rect = (
                    tx_box,
                    pill_top + 6.0,
                    right_cursor,
                    pill_top + pill_h - 6.0,
                );
                self.update_tray_pin_visuals(tray_rect, icon_base_top);
                right_cursor -= base + 6.0;
                right_cursor -= tray_pin_cell(base) * self.tray_pin_visuals.len() as f32;
            } else {
                self.tray_bg.hide();
                self.tray_node.hide();
            }

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
            for wv in &self.widget_visuals {
                wv.bg.hide();
                wv.node.hide();
            }
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
            let s = (ghost_dim * dpi) / 64.0;
            self.drag_ghost.set_transform(s, s, gx * dpi, gy * dpi);
        } else {
            self.drag_ghost.hide();
        }

        // 6. Auto-height tooltip rendered last (suppressed when live thumbnail preview is shown).
        let preview_showing_for = self
            .preview
            .as_deref()
            .filter(|p| p.is_visible())
            .and_then(|p| p.owner_key());
        let tooltip_info: Option<(String, f32, f32)> = if active_drag.is_some() {
            None
        } else if self.hovered_bin {
            let (_, bin_rc, _) = self.right_widget_rects_dips();
            let cx = bin_rc.map_or(pill_left + pill_w - 60.0, |(l, _, r, _)| (l + r) * 0.5);
            Some(("Recycle Bin".to_owned(), cx, pill_top - 8.0))
        } else if self.hovered_tray {
            let (tray_rc, _, _) = self.right_widget_rects_dips();
            let cx = tray_rc.map_or(pill_left + pill_w - 110.0, |(l, _, r, _)| (l + r) * 0.5);
            Some(("Background Apps".to_owned(), cx, pill_top - 8.0))
        } else if let Some(index) = self.hovered_tray_pin
            && let Some(path) = self.settings.user_pinned_background_apps.get(index)
            && let (Some(tray), _, _) = self.right_widget_rects_dips()
        {
            let (left, _, right, _) = tray_pin_rect(
                tray,
                tray_pin_cell(base),
                self.tray_pin_visuals.len(),
                index,
            );
            Some((
                tray::friendly_name(path),
                (left + right) * 0.5,
                pill_top - 8.0,
            ))
        } else if let Some(wid) = self.hovered_widget {
            let (_, _, wrects) = self.right_widget_rects_dips();
            let cx = wrects
                .iter()
                .find(|(id, _)| *id == wid)
                .map_or(pill_left + pill_w - 60.0, |(_, (l, _, r, _))| (l + r) * 0.5);
            let label = widgets::widget_descriptor(wid)
                .map_or(wid, |d| d.name)
                .to_owned();
            Some((label, cx, pill_top - 8.0))
        } else if let Some(shell) = self.hovered_shell {
            Some((
                shell.label().to_owned(),
                self.live_shell_center_x(shell),
                pill_top - 8.0,
            ))
        } else if let Some(idx) = self.hovered_idx
            && let Some(slot) = self.slots.get(idx)
            && preview_showing_for != Some(slot.key.as_str())
        {
            let bounce = bounce_offset_dips(slot.bounce_started).unwrap_or(0.0);
            let lift = ((slot.scale - 1.0) * 14.0 + bounce).max(0.0);
            let icon_top = icon_base_bottom - lift - base * slot.scale;
            let top_edge = icon_top.min(pill_top);
            Some((
                slot.label.clone(),
                self.live_slot_center_x(idx),
                top_edge - 8.0,
            ))
        } else {
            None
        };

        if let Some((text, center_x, bottom_y)) = tooltip_info {
            let needs_redraw = self.tooltip_cache.as_ref().is_none_or(|c| c.text != text);
            if needs_redraw
                && let Ok((surf, w_dips, h_dips)) =
                    self.dcomp.create_tooltip_surface(&text, dpi, self.is_dark)
            {
                self.tooltip_node.set_content(Some(&surf));
                self.tooltip_cache = Some(TooltipCache {
                    text,
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
        if let Some(ref mut preview) = self.preview {
            preview.hide();
        }
    }

    fn schedule_preview_hide(&mut self) {
        self.preview_pending_idx = None;
        unsafe {
            let _ = KillTimer(self.hwnd as HWND, TIMER_PREVIEW_SHOW);
            SetTimer(self.hwnd as HWND, TIMER_PREVIEW_HIDE, 220, None);
        }
    }

    fn sync_preview_hover(&mut self) {
        if self.drag.is_some() {
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
                    SetTimer(self.hwnd as HWND, TIMER_PREVIEW_SHOW, 180, None);
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
        let Some(slot) = self.slots.get(idx) else {
            return;
        };
        if slot.windows.is_empty() {
            return;
        }
        let key = slot.key.clone();
        let items = slot.windows.clone();
        let (win_x, win_y, _, _) = self
            .last_window_rect
            .unwrap_or_else(|| compute_window_rect(&self.settings, self.slots.len(), self.monitor));
        let (_, pill_top, _, _) = self.live_pill_geometry();
        let base = self.settings.icon_dips();
        let max_icon_top =
            pill_top + 7.5 + base - (FISHEYE_PEAK * 14.0) - base * (1.0 + FISHEYE_PEAK);
        let top_edge = max_icon_top.min(pill_top);
        let anchor_x = win_x + (self.live_slot_center_x(idx) * self.dpi).round() as i32;
        let anchor_bottom_y = win_y + ((top_edge - 10.0) * self.dpi).round() as i32;

        if let Some(ref mut preview) = self.preview {
            preview.show(&key, &items, anchor_x, anchor_bottom_y);
            unsafe {
                SetTimer(self.hwnd as HWND, TIMER_PREVIEW_WATCH, 140, None);
            }
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

        if let Some(rdrag) = self.resize_drag {
            self.hide_preview_now();
            let mut pt = POINT { x: 0, y: 0 };
            unsafe {
                GetCursorPos(&mut pt);
            }
            let delta_dips = (pt.x - rdrag.start_screen_x) as f32 / self.dpi;
            let signed = match rdrag.edge {
                ResizeEdge::Right => delta_dips * 2.0,
                ResizeEdge::Left => -delta_dips * 2.0,
            };
            let min_w = content_min_pill_width(&self.settings, self.slots.len(), self.mon_w_dips());
            let max_w = (self.mon_w_dips() - 16.0).max(min_w);
            let next_w = (rdrag.start_width_dips + signed)
                .clamp(min_w, max_w)
                .round();
            self.settings.dock_custom_width_dips = Some(next_w);
            self.sync_window_bounds();
            self.update_visuals();
            return;
        }

        if self.drag.is_some() {
            self.hide_preview_now();
            self.update_drag(mx);
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
                self.hovered_widget = None;
                self.hover_mouse_x = None;
                self.hovered_idx = None;
                self.sync_preview_hover();
                self.ensure_animation();
                self.update_visuals();
            }
        }
        if resize_edge.is_some() {
            return;
        }

        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
        let over_tray = Self::hit_rect(tray_rc, mx, my);
        let over_tray_pin = self.hit_tray_pin(tray_rc, mx, my);
        let over_bin = Self::hit_rect(bin_rc, mx, my);
        let over_widget = wrects
            .iter()
            .find(|(_, rc)| Self::hit_rect(Some(*rc), mx, my))
            .map(|(id, _)| *id);
        if over_tray != self.hovered_tray
            || over_tray_pin != self.hovered_tray_pin
            || over_bin != self.hovered_bin
            || over_widget != self.hovered_widget
        {
            self.hovered_tray = over_tray;
            self.hovered_tray_pin = over_tray_pin;
            self.hovered_bin = over_bin;
            self.hovered_widget = over_widget;
            if over_tray || over_tray_pin.is_some() || over_bin || over_widget.is_some() {
                self.hovered_shell = None;
                self.hover_mouse_x = None;
                self.hovered_idx = None;
                self.sync_preview_hover();
            }
            self.ensure_animation();
            self.update_visuals();
        }
        if over_tray || over_tray_pin.is_some() || over_bin || over_widget.is_some() {
            return;
        }

        if let Some(shell) = self.hit_shell_button(mx, my) {
            let changed = self.hovered_shell != Some(shell) || self.hover_mouse_x.is_some();
            self.hovered_shell = Some(shell);
            self.hover_mouse_x = None;
            self.hovered_idx = None;
            self.sync_preview_hover();
            if changed {
                self.ensure_animation();
                self.update_visuals();
            }
            return;
        }

        let (pill_left, _, pill_w, _) = self.live_pill_geometry();
        let apps_left = pill_left + 15.0 + shell_section_width(self.settings.icon_dips()) - 6.0;
        let apps_right = pill_left + pill_w
            - widgets_section_width(&self.settings, self.slots.len(), self.mon_w_dips())
            - 10.0;
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
            self.hover_mouse_x = Some(mx);
            self.update_hovered_index_from_mouse();
            if self.hovered_idx != prev_idx {
                self.sync_preview_hover();
            }
            if self.settings.magnification_enabled && self.settings.animations_enabled {
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

    fn handle_mouse_leave(&mut self) {
        self.tracking_mouse = false;
        if self.drag.is_some() || self.resize_drag.is_some() {
            return;
        }
        self.hover_mouse_x = None;
        self.hovered_idx = None;
        self.hovered_shell = None;
        self.hovered_tray = false;
        self.hovered_tray_pin = None;
        self.hovered_bin = false;
        self.hovered_widget = None;
        self.hovered_resize = None;
        self.pressed_shell = None;
        self.pressed_tray = false;
        self.pressed_tray_pin = None;
        self.pressed_bin = false;
        self.pressed_widget = None;
        self.pressed_idx = None;
        self.schedule_preview_hide();
        self.evaluate_auto_hide();
        self.ensure_animation();
        self.update_visuals();
    }

    fn handle_lbutton_down(&mut self, mx: f32, my: f32) {
        self.hide_preview_now();
        unsafe {
            SetCapture(self.hwnd as HWND);
        }
        if let Some(edge) = self.hit_resize_edge(mx, my) {
            let mut pt = POINT { x: 0, y: 0 };
            unsafe {
                GetCursorPos(&mut pt);
            }
            let (_, _, pill_w, _) = self.live_pill_geometry();
            self.resize_drag = Some(ResizeDragState {
                edge,
                start_screen_x: pt.x,
                start_width_dips: pill_w,
            });
            return;
        }
        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
        if let Some(index) = self.hit_tray_pin(tray_rc, mx, my) {
            self.pressed_tray_pin = self
                .settings
                .user_pinned_background_apps
                .get(index)
                .cloned();
            self.update_visuals();
            return;
        }
        if Self::hit_rect(tray_rc, mx, my) {
            self.pressed_tray = true;
            self.update_visuals();
            return;
        }
        if Self::hit_rect(bin_rc, mx, my) {
            self.pressed_bin = true;
            self.update_visuals();
            return;
        }
        if let Some((wid, _)) = wrects
            .iter()
            .find(|(_, rc)| Self::hit_rect(Some(*rc), mx, my))
        {
            self.pressed_widget = Some(*wid);
            self.update_visuals();
            return;
        }

        if let Some(shell) = self.hit_shell_button(mx, my) {
            self.pressed_shell = Some(shell);
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
        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
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
                && Self::hit_rect(Some(rc), mx, my)
            {
                let (ax, ay) = self.flyout_anchor_px(rc);
                if let Some((path, pinned)) = tray::open_background_apps_flyout(
                    self.hwnd,
                    self.is_dark,
                    ax,
                    ay,
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
            if Self::hit_rect(bin_rc, mx, my) {
                pins::launch("shell:RecycleBinFolder".to_owned());
            }
            return;
        }
        if let Some(wid) = self.pressed_widget.take() {
            self.update_visuals();
            if let Some((_, rc)) = wrects
                .iter()
                .find(|(id, rc)| *id == wid && Self::hit_rect(Some(*rc), mx, my))
            {
                let (ax, ay) = self.flyout_anchor_px(*rc);
                let changed = self.widget_runtime.open_widget_flyout(
                    wid,
                    self.hwnd,
                    self.is_dark,
                    ax,
                    ay,
                    &mut self.settings,
                );
                if changed {
                    let _ = opentaskbar_core::settings::save_to(
                        &opentaskbar_core::settings::settings_path(),
                        &self.settings,
                    );
                    self.reload_settings_live();
                } else {
                    self.refresh_widgets();
                    self.update_visuals();
                }
            }
            return;
        }
        if let Some(shell) = self.pressed_shell.take() {
            self.update_visuals();
            if self.hit_shell_button(mx, my) == Some(shell) {
                match shell {
                    ShellButton::Start => opentaskbar_shell::taskbar::open_start_menu(),
                    ShellButton::Search => opentaskbar_shell::taskbar::open_search(),
                    ShellButton::TaskView => opentaskbar_shell::taskbar::open_task_view(),
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
            let launch_path = slot.launch_path.clone();
            let shift = win32::is_shift_down();
            let ctrl = win32::is_ctrl_down();
            self.ensure_animation();
            self.update_visuals();

            if shift || !tracker::activate_or_toggle(&hwnds) {
                pins::launch_app(launch_path, hwnds.first().copied(), ctrl && shift);
            }
        } else {
            self.update_visuals();
        }
    }

    fn handle_mbutton_up(&mut self) {
        self.hide_preview_now();
        if let Some(idx) = self.hovered_idx
            && let Some(slot) = self.slots.get_mut(idx)
        {
            slot.bounce_started = Some(Instant::now());
            let launch_path = slot.launch_path.clone();
            let window = slot.windows.first().map(|window| window.hwnd);
            self.ensure_animation();
            pins::launch_app(launch_path, window, false);
        }
    }

    fn handle_mouse_wheel(&mut self, delta: i16) {
        if self.hovered_widget == Some("audio") {
            widgets::adjust_audio_volume(if delta > 0 { 4 } else { -4 });
            self.refresh_widgets();
            self.update_visuals();
            return;
        }
        if let Some(idx) = self.hovered_idx
            && let Some(slot) = self.slots.get(idx)
            && !slot.windows.is_empty()
        {
            let hwnds = slot.hwnds();
            tracker::cycle_windows(&hwnds, delta < 0);
        }
    }

    fn toggle_pin_for_slot(&mut self, idx: usize) {
        let Some(slot) = self.slots.get(idx).cloned() else {
            return;
        };
        self.set_dock_pin(&slot.launch_path, !slot.pinned);
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
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
            ScreenToClient(self.hwnd as HWND, &mut pt);
        }
        let mx = pt.x as f32 / self.dpi;
        self.hover_mouse_x = Some(mx);
        self.update_hovered_index_from_mouse();

        // If dropped directly onto an app icon, open the dropped files with that app.
        if let Some(idx) = self.hovered_idx
            && let Some(slot) = self.slots.get(idx)
        {
            let target = slot.launch_path.clone();
            let args = files
                .iter()
                .map(|f| format!("\"{f}\""))
                .collect::<Vec<_>>()
                .join(" ");
            pins::launch_with_args(target, args);
            return;
        }

        // Otherwise pin dropped executables and shortcuts to the dock.
        let mut added = false;
        for path in files {
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
        if added {
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

        let (tray_rc, bin_rc, _) = self.right_widget_rects_dips();
        if let Some(index) = self.hit_tray_pin(tray_rc, mx, my) {
            let path = self.settings.user_pinned_background_apps[index].clone();
            if tray::show_pinned_app_menu(self.hwnd, self.is_dark, &path) {
                self.set_background_pin(&path, false);
            }
            return;
        }
        if Self::hit_rect(bin_rc, mx, my) {
            match win32::show_recycle_bin_context_menu(self.hwnd, self.is_dark) {
                RecycleBinAction::Open => pins::launch("shell:RecycleBinFolder".to_owned()),
                RecycleBinAction::Empty => {
                    widgets::empty_recycle_bin();
                    self.refresh_widgets();
                    self.update_visuals();
                }
                RecycleBinAction::None => {}
            }
            return;
        }

        if self.hit_shell_button(mx, my) == Some(ShellButton::Start) {
            let act = win32::show_start_context_menu(self.hwnd, self.is_dark);
            self.execute_context_action(act);
            return;
        }

        if self.hit_shell_button(mx, my) == Some(ShellButton::TaskView) {
            match win32::show_taskview_context_menu(self.hwnd, self.is_dark) {
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
            match win32::show_app_context_menu(
                self.hwnd,
                self.is_dark,
                &slot.label,
                slot.pinned,
                running,
                self.settings.focus_mode,
            ) {
                AppContextAction::Open => {
                    let hwnds = slot.hwnds();
                    if !tracker::activate_or_toggle(&hwnds) {
                        pins::launch_app(slot.launch_path, window, false);
                    }
                }
                AppContextAction::NewWindow => pins::launch_app(slot.launch_path, window, false),
                AppContextAction::RunAsAdmin => pins::launch_app(slot.launch_path, window, true),
                AppContextAction::TogglePin => self.toggle_pin_for_slot(idx),
                AppContextAction::Close => {
                    for w in &slot.windows {
                        tracker::close_window(w.hwnd);
                    }
                }
                AppContextAction::Terminate => {
                    if let Some(first) = slot.windows.first() {
                        tracker::terminate_window_process(first.hwnd);
                    }
                }
                AppContextAction::Dock(act) => self.execute_context_action(act),
                AppContextAction::None => {}
            }
            return;
        }

        let act = win32::show_dock_context_menu(self.hwnd, self.is_dark, self.settings.focus_mode);
        self.execute_context_action(act);
    }

    fn execute_context_action(&mut self, action: ContextAction) {
        match action {
            ContextAction::Search => opentaskbar_shell::taskbar::open_search(),
            ContextAction::ToggleFocusMode => {
                self.settings.focus_mode = !self.settings.focus_mode;
                let _ = opentaskbar_core::settings::save_to(
                    &opentaskbar_core::settings::settings_path(),
                    &self.settings,
                );
                self.reload_settings_live();
            }
            ContextAction::Settings => spawn_settings_window(),
            ContextAction::TaskManager => pins::launch("taskmgr.exe".to_owned()),
            ContextAction::FileExplorer => pins::launch("explorer.exe".to_owned()),
            ContextAction::Terminal => pins::launch("wt.exe".to_owned()),
            ContextAction::TerminalAdmin => pins::launch_elevated("wt.exe".to_owned()),
            ContextAction::RunDialog => opentaskbar_shell::taskbar::open_run_dialog(),
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
            ContextAction::ShowDesktop => opentaskbar_shell::taskbar::show_desktop(),
            ContextAction::RestoreTaskbar => opentaskbar_shell::taskbar::restore_from_journal(),
            ContextAction::Exit => unsafe {
                PostQuitMessage(0);
            },
            ContextAction::None => {}
        }
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

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
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
                || state.hovered_widget.is_some()
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
            let mx = loword_i16(lparam) / state.dpi;
            let my = hiword_i16(lparam) / state.dpi;
            state.handle_mouse_move(mx, my);
            0
        }
        WM_MOUSELEAVE => {
            state.handle_mouse_leave();
            0
        }
        WM_LBUTTONDOWN => {
            let mx = loword_i16(lparam) / state.dpi;
            let my = hiword_i16(lparam) / state.dpi;
            state.handle_lbutton_down(mx, my);
            0
        }
        WM_LBUTTONUP => {
            let mx = loword_i16(lparam) / state.dpi;
            let my = hiword_i16(lparam) / state.dpi;
            state.handle_lbutton_up(mx, my);
            0
        }
        WM_MBUTTONUP => {
            state.handle_mbutton_up();
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam >> 16) & 0xffff) as i16;
            state.handle_mouse_wheel(delta);
            0
        }
        WM_RBUTTONUP => {
            let mx = loword_i16(lparam) / state.dpi;
            let my = hiword_i16(lparam) / state.dpi;
            state.handle_rbutton_up(mx, my);
            0
        }
        WM_DROPFILES => {
            state.handle_drop_files(wparam);
            0
        }
        WM_APP_VSYNC => {
            state.tick_pending.store(false, Ordering::Release);
            let moving = state.step_animations(Instant::now());
            state.update_visuals();
            if !moving {
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
            0
        }
        WM_TIMER if wparam == TIMER_POLL => {
            if state.monitor.is_primary {
                let _ = opentaskbar_shell::taskbar::ensure_hidden();
            }
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
        WM_DESTROY => {
            unsafe {
                KillTimer(hwnd, TIMER_POLL);
                KillTimer(hwnd, TIMER_WINDOW_CLOSE);
                KillTimer(hwnd, TIMER_PREVIEW_SHOW);
                KillTimer(hwnd, TIMER_PREVIEW_HIDE);
                KillTimer(hwnd, TIMER_PREVIEW_WATCH);
                KillTimer(hwnd, TIMER_WIDGETS);
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

    let auto_hide = settings.auto_hide != AutoHideMode::Disabled;
    let initial_pins = load_effective_pins(&settings);
    let initial_running = tracker::enumerate_windows();
    let initial_slots = build_slots(&initial_pins, &initial_running, &[]);
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

    let all_monitors = win32::enumerate_monitors();
    let target_monitors: Vec<MonitorInfo> = if settings.show_on_all_monitors {
        all_monitors
    } else {
        all_monitors.into_iter().take(1).collect()
    };

    let ex_style = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP;
    let mut created: Vec<(HWND, *mut DockState)> = Vec::new();

    for monitor in target_monitors {
        let rect = compute_window_rect(&settings, initial_slots.len(), monitor);
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
        }

        // Paint and commit the first DirectComposition frame immediately BEFORE synchronous
        // Explorer AppBar RPCs so the dock appears on screen without waiting on shell IPC.
        match DockState::new(
            hwnd_val,
            monitor,
            reload_msg,
            settings.clone(),
            initial_pins.clone(),
            initial_slots.clone(),
            rect,
        ) {
            Ok(state) => {
                let ptr = Box::into_raw(Box::new(state));
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
                    SetTimer(hwnd, TIMER_POLL, 800, None);
                    SetTimer(hwnd, TIMER_WINDOW_CLOSE, 100, None);
                    SetTimer(hwnd, TIMER_WIDGETS, 2000, None);
                }
                created.push((hwnd, ptr));
                if monitor.is_primary {
                    let desired_total_dips = pill_height_dips(&settings) + VERTICAL_EDGE_GAP * 2.0;
                    let desired_total_px = (desired_total_dips * monitor.dpi_scale).ceil() as i32;
                    opentaskbar_shell::taskbar::begin_replacement(
                        hwnd_val,
                        auto_hide,
                        desired_total_px,
                    );
                }
            }
            Err(err) => {
                opentaskbar_core::log::error(format!(
                    "Failed to initialize DirectComposition on monitor {}: {err}",
                    monitor.handle
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

    for (hwnd, ptr) in created {
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
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

    #[test]
    fn negative_x_secondary_monitor_positions_correctly() {
        let settings = DockSettings::default();
        let left_monitor = MonitorInfo {
            handle: 2,
            left: -2560,
            top: 0,
            right: 0,
            bottom: 1440,
            dpi_scale: 1.0,
            is_primary: false,
        };
        let (x, y, w, h) = compute_window_rect(&settings, 8, left_monitor);
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
        let base = settings.icon_dips();
        let cell = tray_pin_cell(base);
        assert!(cell < base);
        let count = visible_tray_pin_count(&settings, 3, 570.0);
        assert!(count > 0 && count < settings.user_pinned_background_apps.len());
        assert_eq!(visible_tray_pin_count(&settings, 3, 1920.0), 12);
        let tray = (500.0, 20.0, 500.0 + base, 70.0);
        let last = tray_pin_rect(tray, cell, count, count - 1);
        assert_eq!(last.2, tray.0 - 6.0);
        let first = tray_pin_rect(tray, cell, count, 0);
        assert!((first.0 - (tray.0 - 6.0 - count as f32 * cell)).abs() < 0.01);
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
}
