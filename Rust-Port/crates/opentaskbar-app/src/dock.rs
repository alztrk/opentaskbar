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
use std::time::Instant;

use opentaskbar_core::settings::{
    AppTheme, AutoHideMode, DockPosition, DockSettings, SurfaceBackground,
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
    IDC_ARROW, IDC_HAND, IDC_SIZEWE, IsIconic, IsWindowVisible, KillTimer, LoadCursorW, MSG,
    PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetCursor, SetTimer,
    SetWindowLongPtrW, TranslateMessage, WM_APP, WM_DESTROY, WM_DROPFILES, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST,
    WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::dcomp::{DcompEngine, PillNodes, ShellGlyph, VisualNode};
use crate::icon_cache::{self, RawIcon};
use crate::pins::{self, PinnedApp};
use crate::preview::{PreviewItem, PreviewPopup};
use crate::settings_ui;
use crate::tray;
use crate::widgets::{self, WidgetRuntime};
use crate::win32::{
    self, AppContextAction, ContextAction, MonitorInfo, RecycleBinAction, TaskViewAction,
};

const VERTICAL_EDGE_GAP: f32 = 5.0;
const TOOLTIP_HEADROOM: f32 = 36.0;
const MAG_HEADROOM: f32 = 26.0;
const FISHEYE_PEAK: f32 = 0.42;
const FISHEYE_RADIUS_SLOTS: f32 = 2.4;
const BOUNCE_DURATION_SECS: f32 = 0.42;
const DRAG_THRESHOLD_PX: f32 = 6.0;
const SHOW_DESKTOP_W_DIPS: f32 = 8.0;

const TIMER_POLL: usize = 1;
const TIMER_PREVIEW_SHOW: usize = 2;
const TIMER_PREVIEW_HIDE: usize = 3;
const TIMER_WIDGETS: usize = 4;
const TIMER_PREVIEW_WATCH: usize = 5;
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

fn widgets_section_width(settings: &DockSettings) -> f32 {
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
    if settings.show_desktop_button {
        w += SHOW_DESKTOP_W_DIPS + 6.0;
    }
    if w > 0.0 {
        w + 13.0 // left divider + breathing room
    } else {
        0.0
    }
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
}

impl DockSlot {
    fn hwnds(&self) -> Vec<usize> {
        self.windows.iter().map(|w| w.hwnd).collect()
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
    bin_bg: VisualNode,
    bin_node: VisualNode,
    bin_empty_surf: IDCompositionSurface,
    bin_full_surf: IDCompositionSurface,
    bin_is_full: bool,
    widget_visuals: Vec<WidgetVisual>,
    widget_runtime: WidgetRuntime,
    sd_bg: VisualNode,
    show_desktop_node: VisualNode,
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
    hovered_bin: bool,
    hovered_widget: Option<&'static str>,
    hovered_show_desktop: bool,
    hovered_resize: Option<ResizeEdge>,
    pressed_idx: Option<usize>,
    pressed_shell: Option<ShellButton>,
    pressed_tray: bool,
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
        });
    }

    next
}

fn pill_height_dips(settings: &DockSettings) -> f32 {
    settings.icon_dips() + 22.0
}

fn slot_gap(settings: &DockSettings) -> f32 {
    9.0 + settings.dock_icon_padding.max(0) as f32
}

fn content_min_pill_width(settings: &DockSettings, slot_count: usize) -> f32 {
    let base = settings.icon_dips();
    let gap = slot_gap(settings);
    let icons_w = (slot_count as f32) * base + gap * (slot_count.saturating_sub(1) as f32);
    (shell_section_width(base) + icons_w + widgets_section_width(settings) + 30.0).max(160.0)
}

fn resting_pill_width(settings: &DockSettings, slot_count: usize, mon_w_dips: f32) -> f32 {
    let max_w = (mon_w_dips - 16.0).max(200.0);
    if settings.full_width {
        return max_w;
    }
    let min_w = content_min_pill_width(settings, slot_count);
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

        let (pill_rgb, bd_rgb, hover_rgb, press_rgb, div_rgb, dot_run_rgb, dot_act_rgb) = if is_dark
        {
            (
                (0x14, 0x17, 0x1f),
                (0xff, 0xff, 0xff, 0.16),
                (0xff, 0xff, 0xff, 0.11),
                (0xff, 0xff, 0xff, 0.18),
                (0xff, 0xff, 0xff, 0.24),
                (0xe2, 0xe8, 0xf0, 0.90),
                (0xff, 0xff, 0xff, 1.0),
            )
        } else {
            (
                (0xf3, 0xf5, 0xf9),
                (0x0f, 0x17, 0x2a, 0.14),
                (0x0f, 0x17, 0x2a, 0.08),
                (0x0f, 0x17, 0x2a, 0.15),
                (0x0f, 0x17, 0x2a, 0.20),
                (0x47, 0x55, 0x69, 0.85),
                (0x0f, 0x17, 0x2a, 0.95),
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

        let sd_bg = dcomp.create_node()?;
        sd_bg.hide();
        let show_desktop_node = dcomp.create_node()?;
        let sd_surf = dcomp.create_rounded_rect_surface(
            (3.0 * dpi).ceil() as u32,
            (24.0 * dpi).ceil() as u32,
            1.5 * dpi,
            div_rgb,
        )?;
        show_desktop_node.set_content(Some(&sd_surf));

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
            bin_bg,
            bin_node,
            bin_empty_surf,
            bin_full_surf,
            bin_is_full,
            widget_visuals: Vec::new(),
            widget_runtime: WidgetRuntime::default(),
            sd_bg,
            show_desktop_node,
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
            hovered_bin: false,
            hovered_widget: None,
            hovered_show_desktop: false,
            hovered_resize: None,
            pressed_idx: None,
            pressed_shell: None,
            pressed_tray: false,
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
        self.dcomp.add_child(&self.dcomp.root, &self.tray_bg);
        self.dcomp.add_child(&self.dcomp.root, &self.tray_node);
        self.dcomp.add_child(&self.dcomp.root, &self.bin_bg);
        self.dcomp.add_child(&self.dcomp.root, &self.bin_node);
        for wv in &self.widget_visuals {
            self.dcomp.add_child(&self.dcomp.root, &wv.bg);
            self.dcomp.add_child(&self.dcomp.root, &wv.node);
        }
        self.dcomp.add_child(&self.dcomp.root, &self.sd_bg);
        self.dcomp
            .add_child(&self.dcomp.root, &self.show_desktop_node);

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
        let widget_h = 36.0;
        for wv in &mut self.widget_visuals {
            let (top, bottom) = self.widget_runtime.chip_lines(wv.id, &self.settings);
            let key = format!("{top}|{bottom}");
            if key != wv.last_key
                && let Ok(surf) = self.dcomp.create_widget_text_surface(
                    &top,
                    Some(&bottom),
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

        let (pill_rgb, bd_rgb, hover_rgb, press_rgb, div_rgb, dot_run_rgb, dot_act_rgb) = if is_dark
        {
            (
                (0x14, 0x17, 0x1f),
                (0xff, 0xff, 0xff, 0.16),
                (0xff, 0xff, 0xff, 0.11),
                (0xff, 0xff, 0xff, 0.18),
                (0xff, 0xff, 0xff, 0.24),
                (0xe2, 0xe8, 0xf0, 0.90),
                (0xff, 0xff, 0xff, 1.0),
            )
        } else {
            (
                (0xf3, 0xf5, 0xf9),
                (0x0f, 0x17, 0x2a, 0.14),
                (0x0f, 0x17, 0x2a, 0.08),
                (0x0f, 0x17, 0x2a, 0.15),
                (0x0f, 0x17, 0x2a, 0.20),
                (0x47, 0x55, 0x69, 0.85),
                (0x0f, 0x17, 0x2a, 0.95),
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
        self.is_dark = resolve_is_dark(self.settings.theme);
        self.pins = load_effective_pins(&self.settings);
        for wv in &mut self.widget_visuals {
            wv.last_key.clear();
        }
        self.tooltip_cache = None;
        self.rebuild_theme_surfaces();
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
        self.sync_slots();
        self.sync_window_bounds();
        self.refresh_widgets();
        self.evaluate_auto_hide();
        self.update_visuals();
    }

    fn request_missing_icons(&mut self) {
        let mut missing = Vec::new();
        for slot in &self.slots {
            if !self.icon_surfaces.contains_key(&slot.icon_source)
                && self.in_flight_icons.insert(slot.icon_source.clone())
            {
                missing.push(slot.icon_source.clone());
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

    fn sync_slots(&mut self) -> bool {
        let running = tracker::enumerate_windows();
        let next = build_slots(&self.pins, &running, &self.slots);
        let len_changed = next.len() != self.slots.len();
        let changed = len_changed
            || next.iter().zip(self.slots.iter()).any(|(a, b)| {
                a.key != b.key
                    || a.is_foreground != b.is_foreground
                    || a.windows.len() != b.windows.len()
            });
        self.slots = next;
        if len_changed {
            self.rebuild_visual_tree();
            self.sync_window_bounds();
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
            let icons_w: f32 = self.slots.iter().map(|s| base * s.scale).sum::<f32>()
                + gap * (self.slots.len().saturating_sub(1) as f32);
            let live_content = (shell_section_width(base)
                + icons_w
                + widgets_section_width(&self.settings)
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
        apps_left + (idx as f32) * (base + gap) + base * 0.5
    }

    fn live_slot_center_x(&self, idx: usize) -> f32 {
        let base = self.settings.icon_dips();
        let gap = slot_gap(&self.settings);
        let (pill_left, _, _, _) = self.live_pill_geometry();
        let mut cursor = pill_left + 15.0 + shell_section_width(base);
        for i in 0..idx.min(self.slots.len()) {
            cursor += base * self.slots[i].scale + gap;
        }
        let w = self.slots.get(idx).map_or(base, |s| base * s.scale);
        cursor + w * 0.5
    }

    fn live_shell_center_x(&self, kind: ShellButton) -> f32 {
        let base = self.settings.icon_dips();
        let (pill_left, _, _, _) = self.live_pill_geometry();
        pill_left + 15.0 + (kind.index() as f32) * (base + 4.0) + base * 0.5
    }

    fn show_desktop_rect_dips(&self) -> Option<(f32, f32, f32, f32)> {
        if !self.settings.show_desktop_button {
            return None;
        }
        let (pill_left, pill_top, pill_w, pill_h) = self.live_pill_geometry();
        let right = pill_left + pill_w - 8.0;
        let left = right - SHOW_DESKTOP_W_DIPS;
        Some((left, pill_top + 6.0, right, pill_top + pill_h - 6.0))
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
        if self.settings.show_desktop_button {
            right_cursor -= SHOW_DESKTOP_W_DIPS + 6.0;
        }
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

    fn hit_show_desktop(&self, mx: f32, my: f32) -> bool {
        self.show_desktop_rect_dips()
            .is_some_and(|(l, t, r, b)| mx >= l && mx <= r && my >= t && my <= b)
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
                let ix = cursor_x + slide_x + (slot_w - dim) * 0.5;
                let iy = icon_base_bottom - lift - dim;
                let s = (dim * dpi) / 64.0;
                self.slot_visuals[idx]
                    .icon
                    .set_transform(s, s, ix * dpi, iy * dpi);

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

            cursor_x += slot_w + gap;
        }

        // 4. Right-side macOS-style widget strip (System Stats, Clock & Date, Show Desktop).
        let widgets_w = widgets_section_width(&self.settings);
        if widgets_w > 0.0 {
            let mut right_cursor = pill_left + pill_w - 12.0;
            let widget_top = pill_top + (pill_h - 36.0) * 0.5;

            if self.settings.show_desktop_button {
                let sd_x = right_cursor - 3.0;
                let sd_y = pill_top + (pill_h - 24.0) * 0.5;
                if self.hovered_show_desktop {
                    self.sd_bg.set_content(Some(&self.shell_bg_hover));
                    let sx = SHOW_DESKTOP_W_DIPS / base;
                    let sy = 32.0 / base;
                    self.sd_bg.set_transform(
                        sx,
                        sy,
                        ((right_cursor - SHOW_DESKTOP_W_DIPS + 1.0) * dpi).round(),
                        ((pill_top + (pill_h - 32.0) * 0.5) * dpi).round(),
                    );
                } else {
                    self.sd_bg.hide();
                }
                self.show_desktop_node.set_transform(
                    1.0,
                    1.0,
                    (sd_x * dpi).round(),
                    (sd_y * dpi).round(),
                );
                right_cursor -= SHOW_DESKTOP_W_DIPS + 6.0;
            } else {
                self.sd_bg.hide();
                self.show_desktop_node.hide();
            }

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
                        36.0 / base,
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
                right_cursor -= base + 6.0;
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
            self.sd_bg.hide();
            self.show_desktop_node.hide();
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
        } else if self.hovered_show_desktop {
            let cx = self
                .show_desktop_rect_dips()
                .map_or(pill_left + pill_w - 12.0, |(l, _, r, _)| (l + r) * 0.5);
            Some(("Show Desktop".to_owned(), cx, pill_top - 8.0))
        } else if self.hovered_bin {
            let (_, bin_rc, _) = self.right_widget_rects_dips();
            let cx = bin_rc.map_or(pill_left + pill_w - 60.0, |(l, _, r, _)| (l + r) * 0.5);
            Some(("Recycle Bin".to_owned(), cx, pill_top - 8.0))
        } else if self.hovered_tray {
            let (tray_rc, _, _) = self.right_widget_rects_dips();
            let cx = tray_rc.map_or(pill_left + pill_w - 110.0, |(l, _, r, _)| (l + r) * 0.5);
            Some(("Background Apps".to_owned(), cx, pill_top - 8.0))
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
            let min_w = content_min_pill_width(&self.settings, self.slots.len());
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
                self.hovered_bin = false;
                self.hovered_show_desktop = false;
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

        let over_sd = self.hit_show_desktop(mx, my);
        if over_sd != self.hovered_show_desktop {
            self.hovered_show_desktop = over_sd;
            if over_sd {
                self.hovered_shell = None;
                self.hovered_tray = false;
                self.hovered_bin = false;
                self.hover_mouse_x = None;
                self.hovered_idx = None;
                self.sync_preview_hover();
            }
            self.ensure_animation();
            self.update_visuals();
        }
        if over_sd {
            return;
        }

        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
        let over_tray = Self::hit_rect(tray_rc, mx, my);
        let over_bin = Self::hit_rect(bin_rc, mx, my);
        let over_widget = wrects
            .iter()
            .find(|(_, rc)| Self::hit_rect(Some(*rc), mx, my))
            .map(|(id, _)| *id);
        if over_tray != self.hovered_tray
            || over_bin != self.hovered_bin
            || over_widget != self.hovered_widget
        {
            self.hovered_tray = over_tray;
            self.hovered_bin = over_bin;
            self.hovered_widget = over_widget;
            if over_tray || over_bin || over_widget.is_some() {
                self.hovered_shell = None;
                self.hover_mouse_x = None;
                self.hovered_idx = None;
                self.sync_preview_hover();
            }
            self.ensure_animation();
            self.update_visuals();
        }
        if over_tray || over_bin || over_widget.is_some() {
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
        let apps_right = pill_left + pill_w - widgets_section_width(&self.settings) - 10.0;
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
        self.hovered_bin = false;
        self.hovered_widget = None;
        self.hovered_show_desktop = false;
        self.hovered_resize = None;
        self.pressed_shell = None;
        self.pressed_tray = false;
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
        if self.hit_show_desktop(mx, my) {
            return;
        }
        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
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
        if self.hit_show_desktop(mx, my) {
            opentaskbar_shell::taskbar::show_desktop();
            return;
        }
        let (tray_rc, bin_rc, wrects) = self.right_widget_rects_dips();
        if self.pressed_tray {
            self.pressed_tray = false;
            self.update_visuals();
            if let Some(rc) = tray_rc
                && Self::hit_rect(Some(rc), mx, my)
            {
                let (ax, ay) = self.flyout_anchor_px(rc);
                tray::open_background_apps_flyout(self.hwnd, self.is_dark, ax, ay);
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

            if ctrl && shift {
                pins::launch_elevated(launch_path);
            } else if shift {
                pins::launch(launch_path);
            } else if !hwnds.is_empty() {
                if !tracker::activate_or_toggle(&hwnds) {
                    pins::launch(launch_path);
                }
            } else {
                pins::launch(launch_path);
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
            self.ensure_animation();
            pins::launch(launch_path);
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
        if slot.pinned {
            self.settings
                .user_pinned_apps
                .retain(|p| !p.eq_ignore_ascii_case(&slot.launch_path));
            if !self
                .settings
                .user_unpinned_apps
                .iter()
                .any(|p| p.eq_ignore_ascii_case(&slot.launch_path))
            {
                self.settings
                    .user_unpinned_apps
                    .push(slot.launch_path.clone());
            }
        } else {
            self.settings
                .user_unpinned_apps
                .retain(|p| !p.eq_ignore_ascii_case(&slot.launch_path));
            if !self
                .settings
                .user_pinned_apps
                .iter()
                .any(|p| p.eq_ignore_ascii_case(&slot.launch_path))
            {
                self.settings
                    .user_pinned_apps
                    .push(slot.launch_path.clone());
            }
        }
        let _ = opentaskbar_core::settings::save_to(
            &opentaskbar_core::settings::settings_path(),
            &self.settings,
        );
        self.pins = load_effective_pins(&self.settings);
        self.sync_slots();
        self.update_visuals();
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

        // Otherwise pin dropped `.exe` / `.lnk` files to the dock.
        let mut added = false;
        for path in files {
            let lower = path.to_lowercase();
            if (lower.ends_with(".exe") || lower.ends_with(".lnk"))
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
            self.sync_slots();
            self.update_visuals();
        }
    }

    fn handle_rbutton_up(&mut self, mx: f32, my: f32) {
        self.hide_preview_now();

        let (_, bin_rc, _) = self.right_widget_rects_dips();
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
            match win32::show_app_context_menu(
                self.hwnd,
                self.is_dark,
                &slot.label,
                slot.pinned,
                running,
            ) {
                AppContextAction::Open => {
                    let hwnds = slot.hwnds();
                    if !tracker::activate_or_toggle(&hwnds) {
                        pins::launch(slot.launch_path);
                    }
                }
                AppContextAction::NewWindow => pins::launch(slot.launch_path),
                AppContextAction::RunAsAdmin => pins::launch_elevated(slot.launch_path),
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
                AppContextAction::None => {}
            }
            return;
        }

        match win32::show_dock_context_menu(self.hwnd, self.is_dark) {
            ContextAction::Settings => spawn_settings_window(),
            ContextAction::TaskManager => pins::launch("taskmgr.exe".to_owned()),
            ContextAction::FileExplorer => pins::launch("explorer.exe".to_owned()),
            ContextAction::Terminal => pins::launch("wt.exe".to_owned()),
            ContextAction::TerminalAdmin => pins::launch_elevated("wt.exe".to_owned()),
            ContextAction::RunDialog => opentaskbar_shell::taskbar::open_run_dialog(),
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
        state.reload_settings_live();
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
                || state.hovered_bin
                || state.hovered_widget.is_some()
                || state.hovered_show_desktop
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
            let slots_changed = state.sync_slots();
            state.evaluate_auto_hide();
            if slots_changed {
                state.update_visuals();
            }
            0
        }
        WM_DESTROY => {
            unsafe {
                KillTimer(hwnd, TIMER_POLL);
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
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let auto_hide = settings.auto_hide != AutoHideMode::Disabled;
    let initial_pins = load_effective_pins(&settings);
    let initial_running = tracker::enumerate_windows();
    let initial_slots = build_slots(&initial_pins, &initial_running, &[]);

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

        if monitor.is_primary {
            let desired_total_dips = pill_height_dips(&settings) + VERTICAL_EDGE_GAP * 2.0;
            let desired_total_px = (desired_total_dips * monitor.dpi_scale).ceil() as i32;
            opentaskbar_shell::taskbar::begin_replacement(hwnd_val, auto_hide, desired_total_px);
        }

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
                    SetTimer(hwnd, TIMER_WIDGETS, 2000, None);
                }
                created.push((hwnd, ptr));
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
}
