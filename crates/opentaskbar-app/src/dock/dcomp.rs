//! Native DirectComposition + Direct2D + DirectWrite visual engine for the floating dock.
//!
//! All icons, pill caps, shell glyphs, indicator dots, and tooltips are drawn ONCE onto static
//! `IDCompositionSurface` textures. During 60-180 FPS fisheye, bounce, and drag animations, only
//! 2D affine transforms (`IDCompositionMatrixTransform`) are updated and committed to DWM
//! (`IDCompositionDevice::Commit`) with zero D3D11 draw calls and ~0-1% GPU usage.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use opentaskbar_core::dynamic_island::geometry as island_geo;
use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::SurfaceBackground;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_BITMAP_PROPERTIES1,
    D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_CAP_STYLE_ROUND,
    D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LINE_JOIN_ROUND, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES1, D2D1_SWEEP_DIRECTION_CLOCKWISE, ID2D1Device,
    ID2D1DeviceContext, ID2D1Factory1, ID2D1LinearGradientBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::DirectComposition::{
    DCOMPOSITION_BITMAP_INTERPOLATION_MODE_LINEAR, DCOMPOSITION_BORDER_MODE_SOFT,
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionMatrixTransform,
    IDCompositionRectangleClip, IDCompositionSurface, IDCompositionTarget, IDCompositionVisual,
    IDCompositionVisual3,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::core::{HSTRING, Interface, Result};
use windows_numerics::{Matrix3x2, Vector2};

use crate::design::{
    self, Rgb,
    icons::{self, AppIcon},
};
use crate::platform::icon_cache::RawIcon;

/// Icon payload for a dock widget chip. Built-ins use a Fluent glyph; user
/// widgets can supply theme-tinted vector path data or a PNG asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WidgetIcon {
    /// Segoe Fluent Icons code point (e.g. `0xE8B7` for Folder).
    Glyph(u16),
    /// Lucide-style `StreamGeometry` path data resolved through Direct2D.
    PathData(String),
    /// PNG file path, decoded and cached by `widgets::widget_icons`.
    Asset(std::path::PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetInteraction {
    Idle,
    Hovered(Option<u8>),
    Pressed(Option<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandButton {
    Prev,
    Next,
    Expand,
    /// Pin toggle mirrored opposite the expand chevron: a pinned island never auto-collapses.
    Pin,
    /// Clickable page indicator dot (absolute page index).
    Dot(u8),
}

/// Hit-tested buttons of the transient island overlay (agent approval / notification pill).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandOverlayButton {
    Allow,
    Deny,
    Always,
    /// Jump to the agent's terminal window.
    Jump,
    /// Notification pill `×`.
    Dismiss,
}

impl IslandOverlayButton {
    /// Localized label (English fallback while a catalog lacks the key).
    pub fn label(self) -> String {
        let (key, fallback) = match self {
            Self::Allow => ("AgentApprove", "Allow"),
            Self::Deny => ("AgentDeny", "Deny"),
            Self::Always => ("AgentAlways", "Always"),
            Self::Jump => ("AgentJumpToTerminal", "Terminal"),
            Self::Dismiss => return String::new(),
        };
        let text = loc::t(key);
        if text == key || text.trim().is_empty() {
            fallback.to_owned()
        } else {
            text
        }
    }
}

/// Widget chip chrome. Dock cards keep the frosted card; the Dynamic Island draws
/// frameless chips that sit directly inside the island capsule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipChrome {
    /// Standard dock widget card (`draw_chip_card`).
    Card,
    /// Frameless Dynamic Island chip: no card at rest, a subtle 12 DIP rounded
    /// hover/press pill only while hovered or pressed.
    Bare,
}

/// Theme-pack widget card overrides resolved once per chip render. `None` fields keep
/// the stock frosted recipe for that layer.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ThemedChipCard {
    fill: Option<design::tokens::Rgba>,
    hover: Option<design::tokens::Rgba>,
    pressed: Option<design::tokens::Rgba>,
    stroke: Option<design::tokens::Rgba>,
    radius: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ChipCardStyle {
    chrome: ChipChrome,
    themed: Option<ThemedChipCard>,
    surface: design::theme::SurfaceStyle,
}

impl ChipCardStyle {
    fn resolve(chrome: ChipChrome) -> Self {
        let themed = if design::theme::has_active_theme_customization() {
            let card = ThemedChipCard {
                fill: custom_token_rgba(&["DockWidgetSurface", "WidgetCard"]),
                hover: custom_token_rgba(&["WidgetCardHover"]),
                pressed: custom_token_rgba(&["WidgetCardPressed"]),
                stroke: custom_token_rgba(&["WidgetCardStroke"]),
                radius: custom_token_radius(&["DockWidgetRadius"]),
            };
            let any = card.fill.is_some()
                || card.hover.is_some()
                || card.pressed.is_some()
                || card.stroke.is_some()
                || card.radius.is_some();
            any.then_some(card)
        } else {
            None
        };
        Self {
            chrome,
            themed,
            surface: design::theme::ThemeManager::get_surface(design::theme::SurfaceKind::Widget),
        }
    }
}

/// First visible colour carried by a theme-pack / editor override token. The stock
/// `base` and `windows` layers are ignored so Windows Default keeps its own recipe.
pub(crate) fn custom_token_rgba(keys: &[&str]) -> Option<design::tokens::Rgba> {
    keys.iter().find_map(|key| {
        design::theme::resolve_custom_token(key)
            .as_ref()
            .and_then(design::theme::token_rgba)
            .filter(|rgba| rgba.a > 0)
    })
}

/// Uniform corner radius (DIP) carried by a theme-pack / editor override token.
pub(crate) fn custom_token_radius(keys: &[&str]) -> Option<f32> {
    keys.iter().find_map(|key| {
        let value = match design::theme::resolve_custom_token(key)? {
            design::tokens::TokenValue::CornerRadius(corner) => corner.top_left as f32,
            design::tokens::TokenValue::Double(value) => value as f32,
            _ => return None,
        };
        (value.is_finite() && value >= 0.0).then_some(value.min(64.0))
    })
}

/// Gradient stops for the Start button Windows logo when a theme pack or editor
/// override defines `WindowsLogoBrush`; `None` keeps the stock sky-to-azure logo.
fn themed_windows_logo_stops() -> Option<Vec<D2D1_GRADIENT_STOP>> {
    if !design::theme::has_active_theme_customization() {
        return None;
    }
    let stop = |color: design::tokens::Rgba, position: f32| D2D1_GRADIENT_STOP {
        position,
        color: color_rgba(color.r, color.g, color.b, f32::from(color.a) / 255.0),
    };
    let stops: Vec<D2D1_GRADIENT_STOP> =
        match design::theme::resolve_custom_token("WindowsLogoBrush")? {
            design::tokens::TokenValue::Solid(color) | design::tokens::TokenValue::Color(color) => {
                vec![stop(color, 0.0), stop(color, 1.0)]
            }
            design::tokens::TokenValue::Gradient(gradient) => gradient
                .stops
                .iter()
                .map(|s| stop(s.color, s.position.clamp(0.0, 1.0) as f32))
                .collect(),
            design::tokens::TokenValue::RadialGradient(gradient) => gradient
                .stops
                .iter()
                .map(|s| stop(s.color, s.position.clamp(0.0, 1.0) as f32))
                .collect(),
            _ => return None,
        };
    (!stops.is_empty() && stops.iter().any(|s| s.color.a > 0.0)).then_some(stops)
}

/// One telemetry column rendered inside `WidgetChipSpec::SystemStats` (CPU, RAM, GPU, GPU °C).
#[derive(Clone, Debug)]
pub struct SystemStatColumn {
    pub label: String,
    pub value_text: String,
    pub ratio: f32,
    pub bar_color: Rgb,
    pub warm_text: bool,
}

/// Exact reference 1-to-1 dock widget specs ported from `Widgets/*.xaml` and `Widgets/*.cs`.
#[derive(Clone, Debug)]
pub enum WidgetChipSpec {
    /// `ClockWidget.xaml`: `[ClockIcon 15px] [15px Bold Time] [11.5px Date / 10.5px Weekday]`
    Clock {
        time: String,
        date: String,
        weekday: String,
    },
    /// `WeatherWidget.cs`: `[Icon 20px] [18px Bold Temp] [Place / Condition] | [Today / High/Low]` or unavailable text
    Weather {
        available: bool,
        icon_color: Rgb,
        is_rain: bool,
        temp: String,
        place: String,
        condition: String,
        today_range: Option<String>,
    },
    /// `DayProgressWidget.cs`: `[22px RingMeter (WidgetBlue)] [13px SemiBold %] [11.5px "day"]`
    DayProgress {
        ratio: f32,
        percent: String,
        unit: String,
    },
    /// `TimedWidgetTile.xaml` & `FocusWidget.xaml`: `[Icon 16px] [13px Tabular Time] [26x26 Play/Pause]`
    Timed {
        is_focus: bool,
        running: bool,
        text: String,
    },
    /// Calm monochrome System Stats columns (`CPU | RAM | GPU | GPU °C`) with 2px bottom hairline bars.
    SystemStats { columns: Vec<SystemStatColumn> },
    /// Frameless live-updating dual-channel sparkline + tabular download/upload speed rows.
    NetSpeed {
        down: String,
        up: String,
        down_history: Vec<f32>,
        up_history: Vec<f32>,
    },
    /// `BatteryWidget.cs`: full-card horizontal liquid fill gradient + shadowed white `%` + bolt
    BatteryLiquid {
        present: bool,
        percent: u32,
        charging: bool,
        wave_phase: f32,
    },
    /// `BluetoothBatteryWidget.cs`: horizontal connected device tiles (`device-kind icon + %`) or centered `—`
    Bluetooth {
        /// `(kind, percent, low_warning, full_charge)`
        devices: Vec<(
            crate::widgets::telemetry::BluetoothDeviceKind,
            u32,
            bool,
            bool,
        )>,
    },
    /// `StorageWidget.cs`: `"C: 213 / 476 GB | D: 1.2 / 1.8 TB"` (clean typographic, no bars)
    Storage { drives: Vec<(String, String)> },
    /// `MediaWidget.xaml`: `[29x29 tinted Art] [Title / Artist] [Prev | PlayPause | Next]`
    Media { title: String, artist: String },
    /// `AudioWidget.xaml`: `[Speaker 16px] [12.5px SemiBold 67%]` (scroll adjusts, click opens mixer)
    Audio { volume: u32, muted: bool },
    /// `QuickSettingsWidget.xaml`: 3 compact `27x27` toggle pills (`Wi-Fi`, `Bluetooth`, `Night Light/Focus`)
    QuickSettings { pills: [(String, bool); 3] },
    /// `KeyboardLayoutWidget.cs`: `[Keyboard 13px] [12px SemiBold TR/EN]`
    Keyboard { code: String },
    /// `AiUsageWidget.cs` dual-bar layout (Claude, Codex, single-row Cursor) with real `Brand.xaml` logo
    AiDual {
        provider_id: &'static str,
        brand_color: Option<Rgb>,
        /// `(short_label, used_01, bar_color, reset_text)`
        rows: Vec<AiQuotaRow>,
    },
    /// `AiUsageWidget.cs` triple-column OpenCode layout (`1h | 7d | 30d`)
    AiOpenCode {
        /// `(short_label, pct_text, used_01, bar_color, reset_text)`
        columns: Vec<(String, String, f32, Rgb, String)>,
    },
    /// `AiUsageWidget.cs` Copilot inline layout (`Logo + Copilot | Value/Meta + 31x4 mini bar`)
    AiCopilot {
        value_text: String,
        meta_text: String,
        used: f32,
        bar_color: Rgb,
    },
    /// `AiUsageWidget.cs` DeepSeek / OpenRouter tinted balance card (`29x29 circular tinted well + Name / 16px Balance`)
    AiBalance {
        provider_id: &'static str,
        name: &'static str,
        brand_color: Rgb,
        value_text: String,
    },
    /// Clean borderless icon + two-line helper (Stacks, Groups)
    Simple {
        icon: WidgetIcon,
        icon_color: Rgb,
        top: String,
        bottom: String,
    },
    /// User-created widget: icon, value, label and an optional progress bar.
    Custom {
        label: String,
        value: String,
        ratio: Option<f32>,
        icon: WidgetIcon,
        color: Rgb,
        /// Detail rows supplied by the widget for the Dynamic Island panel.
        details: Vec<(String, String)>,
    },
}

type AiQuotaRow = (String, f32, Rgb, String);

impl WidgetChipSpec {
    /// Writes the spec and any side-channel chip visual hints into a reusable buffer so per-widget
    /// refresh invalidates cached surfaces without extra allocations.
    pub fn write_cache_key(&self, out: &mut String) {
        out.clear();
        let _ = write!(out, "{self:?}");
        match self {
            Self::Clock { .. } => {
                let _ = write!(out, "|below={}", crate::widgets::chip_clock_below());
            }
            Self::Timed { is_focus, text, .. } => {
                if *is_focus {
                    let (is_break, ratio) = crate::widgets::chip_focus_hint();
                    let _ = write!(out, "|fb={}:r={}", is_break, (ratio * 100.0).round() as u32);
                } else if !text.contains('.') {
                    let r = crate::widgets::chip_countdown_ratio();
                    let _ = write!(out, "|cd={}", (r * 100.0).round() as u32);
                }
            }
            Self::Media { .. } => {
                let hint = crate::widgets::chip_media_hint();
                let _ = write!(
                    out,
                    "|m={}:{}:{}:{}:{}",
                    hint.playing,
                    (hint.progress * 100.0).round() as u32,
                    hint.has_media,
                    hint.cover_path.as_deref().unwrap_or(""),
                    hint.cover_pixels
                        .as_ref()
                        .map_or(0, |pixels| Arc::as_ptr(pixels) as usize)
                );
            }
            Self::Storage { drives } => {
                for (letter, _) in drives {
                    let r = crate::widgets::chip_storage_ratio(letter)
                        .map_or(255u32, |v| (v * 100.0).round() as u32);
                    let _ = write!(out, "|sr={r}");
                }
            }
            Self::AiBalance {
                provider_id: "aiusage_openrouter",
                ..
            } => {
                let r = crate::widgets::chip_openrouter_ratio()
                    .map_or(255u32, |v| (v * 100.0).round() as u32);
                let _ = write!(out, "|or={r}");
            }
            _ => {}
        }
    }

    /// Text payload for the Dynamic Island's 246 DIP expanded details panel
    /// (`GetDetails()` on each `TimedWidgetTile` / widget). Kept next to the
    /// chip renderer so the panel never shows data the chip itself could not have.
    pub fn details_lines(&self) -> Vec<(String, String)> {
        // All user-visible island detail labels/values go through `loc::t`.
        // New keys are listed in the WS-C handoff; WS-F adds them to all 14 catalogs.
        let line = |key: &str, value: String| (loc::t(key), value);
        match self {
            Self::Clock {
                time,
                date,
                weekday,
            } => vec![
                line("WidgetDetailTime", time.clone()),
                line("WidgetDate", date.clone()),
                line("WidgetDetailDay", weekday.clone()),
            ],
            Self::Weather {
                available,
                temp,
                place,
                condition,
                today_range,
                ..
            } => {
                let mut out = vec![
                    line("WidgetDetailTemperature", temp.clone()),
                    line("WidgetDetailPlace", place.clone()),
                    line("WidgetDetailCondition", condition.clone()),
                ];
                if let Some(range) = today_range {
                    out.push(line("WeatherToday", range.clone()));
                }
                if !available {
                    out.push(line("WidgetDetailStatus", loc::t("SystemDataUnavailable")));
                }
                out
            }
            Self::DayProgress { percent, unit, .. } => vec![
                line("WidgetDetailElapsed", percent.clone()),
                line("WidgetDetailUnit", unit.clone()),
            ],
            Self::Timed { text, running, .. } => vec![
                line("WidgetDetailTime", text.clone()),
                line(
                    "WidgetDetailState",
                    if *running {
                        loc::t("WidgetDetailRunning")
                    } else {
                        loc::t("WidgetDetailPaused")
                    },
                ),
            ],
            Self::SystemStats { columns } => columns
                .iter()
                .map(|c| line(&c.label, c.value_text.clone()))
                .collect(),
            Self::NetSpeed { down, up, .. } => vec![
                line("NetworkDownload", down.clone()),
                line("NetworkUpload", up.clone()),
            ],
            Self::BatteryLiquid {
                present,
                percent,
                charging,
                ..
            } => vec![
                line(
                    "WidgetDetailCharge",
                    crate::widgets::format_percent(*percent),
                ),
                line("WidgetDetailState", {
                    if !*present {
                        loc::t("WidgetDetailNoBattery")
                    } else if *charging {
                        loc::t("Charging")
                    } else {
                        loc::t("WidgetDetailOnBattery")
                    }
                }),
            ],
            Self::Bluetooth { devices } => devices
                .iter()
                .map(|(kind, percent, _, _)| {
                    line(
                        "WidgetDetailDevice",
                        format!("{kind:?} {}", crate::widgets::format_percent(*percent)),
                    )
                })
                .collect(),
            Self::Storage { drives } => drives
                .iter()
                .map(|(name, usage)| line(name, usage.clone()))
                .collect(),
            Self::Media { title, artist } => vec![
                line("WidgetDetailTitle", title.clone()),
                line("WidgetDetailArtist", artist.clone()),
            ],
            Self::Audio { volume, muted } => vec![
                line("Volume", crate::widgets::format_percent(*volume)),
                line(
                    "WidgetDetailState",
                    if *muted {
                        loc::t("WidgetDetailMuted")
                    } else {
                        loc::t("WidgetDetailOn")
                    },
                ),
            ],
            Self::QuickSettings { pills } => {
                const KEYS: [&str; 3] = ["WiFi", "Bluetooth", "NightLight"];
                pills
                    .iter()
                    .enumerate()
                    .map(|(i, (_, on))| {
                        line(
                            KEYS.get(i).copied().unwrap_or("WidgetQuickSettings"),
                            if *on {
                                loc::t("WidgetDetailOn")
                            } else {
                                loc::t("WidgetDetailOff")
                            },
                        )
                    })
                    .collect()
            }
            Self::Keyboard { code } => vec![line("Layout", code.clone())],
            Self::AiDual { rows, .. } => rows
                .iter()
                .map(|(name, used, _, reset)| {
                    let pct = crate::widgets::format_percent((used * 100.0).round() as u32);
                    line(name, format!("{pct} · {reset}"))
                })
                .collect(),
            Self::AiOpenCode { columns } => columns
                .iter()
                .map(|(name, pct, _, _, reset)| line(name, format!("{pct} · {reset}")))
                .collect(),
            Self::AiCopilot {
                value_text,
                meta_text,
                ..
            } => vec![
                line("AiUsageBalance", value_text.clone()),
                line("WidgetDetailWindow", meta_text.clone()),
            ],
            Self::AiBalance {
                name, value_text, ..
            } => vec![line(name, value_text.clone())],
            Self::Simple { top, bottom, .. } => vec![
                line("WidgetDetailInfo", top.clone()),
                line("WidgetDetailDetail", bottom.clone()),
            ],
            Self::Custom {
                label,
                value,
                details,
                ..
            } => {
                if details.is_empty() {
                    vec![
                        line("WidgetDetailInfo", value.clone()),
                        line("WidgetDetailDetail", label.clone()),
                    ]
                } else {
                    details.clone()
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellGlyph {
    Start,
    Search,
    TaskView,
    TrayChevron,
    RecycleBinEmpty,
    RecycleBinFull,
}

#[derive(Clone)]
pub struct VisualNode {
    pub visual: IDCompositionVisual,
    pub transform: IDCompositionMatrixTransform,
    opacity: Option<IDCompositionVisual3>,
}

impl VisualNode {
    pub fn set_transform(&self, sx: f32, sy: f32, tx: f32, ty: f32) {
        let mat = Matrix3x2 {
            M11: sx,
            M12: 0.0,
            M21: 0.0,
            M22: sy,
            M31: tx,
            M32: ty,
        };
        // SAFETY: `self.transform` is a live COM object owned by the UI thread.
        unsafe {
            let _ = self.transform.SetMatrix(&mat);
        }
    }

    pub fn hide(&self) {
        self.set_transform(0.0, 0.0, -10000.0, -10000.0);
    }

    pub fn set_content(&self, surface: Option<&IDCompositionSurface>) {
        // SAFETY: `self.visual` is a live COM object owned by the UI thread.
        unsafe {
            let _ = match surface {
                Some(s) => self.visual.SetContent(s),
                None => self.visual.SetContent(None),
            };
        }
    }

    pub fn set_opacity(&self, opacity: f32) {
        if let Some(visual) = &self.opacity {
            // SAFETY: The visual belongs to the UI thread and opacity is normalized by the caller.
            unsafe {
                let _ = visual.SetOpacity2(opacity);
            }
        }
    }

    pub fn set_clip(&self, clip: &IDCompositionRectangleClip) {
        unsafe {
            let _ = self.visual.SetClip(clip);
        }
    }

    pub fn remove_all_children(&self) {
        unsafe {
            let _ = self.visual.RemoveAllVisuals();
        }
    }

    pub fn add_child(&self, child: &VisualNode) {
        unsafe {
            let _ = self.visual.AddVisual(&child.visual, false, None);
        }
    }
}

pub struct PillNodes {
    pub left: VisualNode,
    pub center: VisualNode,
    pub right: VisualNode,
    pub cap_w_px: f32,
}

pub struct DcompEngine {
    _d3d_device: ID3D11Device,
    d2d_factory: ID2D1Factory1,
    _d2d_device: ID2D1Device,
    dwrite_factory: IDWriteFactory,
    tooltip_format: IDWriteTextFormat,
    fallback_format: IDWriteTextFormat,
    /// Chip typography is a fixed set; creating 21 formats on every chip redraw cost ~300+
    /// COM object constructions/sec during the 15 Hz battery animation.
    text_formats: Mutex<HashMap<(u16, bool, u8), IDWriteTextFormat>>,
    dcomp_device: IDCompositionDevice,
    _target: IDCompositionTarget,
    pub root: IDCompositionVisual,
}

use crate::design::d2d::d2d_rect as rect;

fn color_rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    design::d2d::d2d_color_channels(r, g, b, a)
}

#[inline]
fn rrect(l: f32, t: f32, r: f32, b: f32, rad: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: rect(l, t, r, b),
        radiusX: rad,
        radiusY: rad,
    }
}

unsafe fn set_dpi_transform(dc: &ID2D1DeviceContext, dpi: f32) -> Matrix3x2 {
    let mut prev = Matrix3x2::identity();
    unsafe {
        dc.GetTransform(&mut prev);
        dc.SetTransform(&Matrix3x2 {
            M11: dpi,
            M12: 0.0,
            M21: 0.0,
            M22: dpi,
            M31: prev.M31,
            M32: prev.M32,
        });
    }
    prev
}

unsafe fn linear_grad_brush(
    dc: &ID2D1DeviceContext,
    start: (f32, f32),
    end: (f32, f32),
    stops: &[(f32, D2D1_COLOR_F)],
) -> Result<ID2D1LinearGradientBrush> {
    let raw: Vec<D2D1_GRADIENT_STOP> = stops
        .iter()
        .map(|&(position, color)| D2D1_GRADIENT_STOP { position, color })
        .collect();
    unsafe {
        let coll = dc.CreateGradientStopCollection(
            &raw,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_BUFFER_PRECISION_8BPC_UNORM,
            D2D1_EXTEND_MODE_CLAMP,
            D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
        )?;
        dc.CreateLinearGradientBrush(
            &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: Vector2 {
                    X: start.0,
                    Y: start.1,
                },
                endPoint: Vector2 { X: end.0, Y: end.1 },
            },
            None,
            &coll,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_bar(
    dc: &ID2D1DeviceContext,
    l: f32,
    t: f32,
    w: f32,
    h: f32,
    rad: f32,
    ratio: f32,
    min_fill: f32,
    track: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    fill_rgb: Rgb,
    fill_alpha: f32,
) -> Result<()> {
    unsafe {
        dc.FillRoundedRectangle(&rrect(l, t, l + w, t + h, rad), track);
        let fw = (w * ratio.clamp(0.0, 1.0)).min(w);
        if fw >= min_fill {
            let fb = dc.CreateSolidColorBrush(
                &color_rgba(fill_rgb.0, fill_rgb.1, fill_rgb.2, fill_alpha),
                None,
            )?;
            dc.FillRoundedRectangle(&rrect(l, t, l + fw, t + h, rad), &fb);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_arc_ring(
    factory: &ID2D1Factory1,
    dc: &ID2D1DeviceContext,
    cx: f32,
    cy: f32,
    rad: f32,
    ratio: f32,
    thickness: f32,
    track: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    color: Rgb,
    alpha: f32,
    stroke: &ID2D1StrokeStyle,
) -> Result<()> {
    let clamped = ratio.clamp(0.0, 1.0);
    let ell = D2D1_ELLIPSE {
        point: Vector2 { X: cx, Y: cy },
        radiusX: rad,
        radiusY: rad,
    };
    unsafe {
        dc.DrawEllipse(&ell, track, thickness, None);
        let brush =
            dc.CreateSolidColorBrush(&color_rgba(color.0, color.1, color.2, alpha), None)?;
        if clamped >= 0.995 {
            dc.DrawEllipse(&ell, &brush, thickness, None);
        } else if clamped > 0.005 {
            let end_ang = -std::f32::consts::FRAC_PI_2 + clamped * std::f32::consts::TAU;
            let geom = factory.CreatePathGeometry()?;
            let sink = geom.Open()?;
            sink.BeginFigure(Vector2 { X: cx, Y: cy - rad }, D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: Vector2 {
                    X: cx + end_ang.cos() * rad,
                    Y: cy + end_ang.sin() * rad,
                },
                size: D2D_SIZE_F {
                    width: rad,
                    height: rad,
                },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if clamped > 0.5 {
                    D2D1_ARC_SIZE_LARGE
                } else {
                    D2D1_ARC_SIZE_SMALL
                },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            dc.DrawGeometry(&geom, &brush, thickness, Some(stroke));
        }
    }
    Ok(())
}

impl DcompEngine {
    pub fn new(hwnd: usize) -> Result<Self> {
        let d3d_device = design::spatial_glass::shared_d3d11_device()?;
        // Stroke styles and device contexts must use the shared device's factory or EndDraw can
        // fail with D2DERR_WRONG_FACTORY.
        let (d2d_factory, d2d_device) = design::spatial_glass::shared_d2d_resources()?;

        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let font_name = HSTRING::from(design::fonts::dwrite_ui_family().as_str());
        let locale = HSTRING::from(design::fonts::ui_locale().as_str());
        let tooltip_format = unsafe {
            let fmt = dwrite_factory.CreateTextFormat(
                &font_name,
                None,
                DWRITE_FONT_WEIGHT_MEDIUM,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                design::fonts::scaled_size(12.0),
                &locale,
            )?;
            fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let trim = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            if let Ok(ellipsis) = dwrite_factory.CreateEllipsisTrimmingSign(&fmt) {
                let _ = fmt.SetTrimming(&trim, &ellipsis);
            }
            fmt
        };
        let fallback_format = unsafe {
            let fmt = dwrite_factory.CreateTextFormat(
                &font_name,
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                design::fonts::scaled_size(18.0),
                &locale,
            )?;
            fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            fmt
        };

        let dcomp_device: IDCompositionDevice = unsafe { DCompositionCreateDevice2(&d2d_device)? };
        let target = unsafe {
            dcomp_device.CreateTargetForHwnd(HWND(hwnd as *mut core::ffi::c_void), true)?
        };
        let root = unsafe { dcomp_device.CreateVisual()? };
        unsafe {
            root.SetBitmapInterpolationMode(DCOMPOSITION_BITMAP_INTERPOLATION_MODE_LINEAR)?;
            root.SetBorderMode(DCOMPOSITION_BORDER_MODE_SOFT)?;
            target.SetRoot(&root)?;
        }

        Ok(Self {
            _d3d_device: d3d_device,
            d2d_factory,
            _d2d_device: d2d_device,
            dwrite_factory,
            tooltip_format,
            fallback_format,
            text_formats: Mutex::new(HashMap::new()),
            dcomp_device,
            _target: target,
            root,
        })
    }

    pub fn create_node(&self) -> Result<VisualNode> {
        unsafe {
            let visual = self.dcomp_device.CreateVisual()?;
            visual.SetBitmapInterpolationMode(DCOMPOSITION_BITMAP_INTERPOLATION_MODE_LINEAR)?;
            visual.SetBorderMode(DCOMPOSITION_BORDER_MODE_SOFT)?;
            let transform = self.dcomp_device.CreateMatrixTransform()?;
            visual.SetTransform(&transform)?;
            let opacity = visual.cast().ok();
            Ok(VisualNode {
                visual,
                transform,
                opacity,
            })
        }
    }

    pub fn create_rect_clip(&self) -> Result<IDCompositionRectangleClip> {
        unsafe { self.dcomp_device.CreateRectangleClip() }
    }

    pub fn update_clip_rect(
        clip: &IDCompositionRectangleClip,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) {
        unsafe {
            let _ = clip.SetLeft2(left);
            let _ = clip.SetTop2(top);
            let _ = clip.SetRight2(right.max(left));
            let _ = clip.SetBottom2(bottom.max(top));
        }
    }

    /// Updates a DirectComposition clip to a rounded capsule matching the dock pill inner edge
    /// so scrolled widgets and controls never bleed outside the rounded corners.
    #[allow(clippy::too_many_arguments)]
    pub fn update_rounded_clip_rect(
        clip: &IDCompositionRectangleClip,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        top_radius: f32,
        bottom_radius: f32,
    ) {
        let w = (right - left).max(0.0);
        let h = (bottom - top).max(0.0);
        let max_r = (w * 0.5).min(h * 0.5);
        let tr = top_radius.clamp(0.0, max_r);
        let br = bottom_radius.clamp(0.0, max_r);
        unsafe {
            let _ = clip.SetLeft2(left);
            let _ = clip.SetTop2(top);
            let _ = clip.SetRight2(right.max(left));
            let _ = clip.SetBottom2(bottom.max(top));
            let _ = clip.SetTopLeftRadiusX2(tr);
            let _ = clip.SetTopLeftRadiusY2(tr);
            let _ = clip.SetTopRightRadiusX2(tr);
            let _ = clip.SetTopRightRadiusY2(tr);
            let _ = clip.SetBottomLeftRadiusX2(br);
            let _ = clip.SetBottomLeftRadiusY2(br);
            let _ = clip.SetBottomRightRadiusX2(br);
            let _ = clip.SetBottomRightRadiusY2(br);
        }
    }

    /// Creates a directional edge-fade scrim + crisp Fluent chevron button for overflowing dock scroll.
    #[allow(clippy::too_many_arguments)]
    pub fn create_scroll_edge_surface(
        &self,
        is_left: bool,
        width_px: u32,
        height_px: u32,
        radius_px: f32,
        pill_rgb: Rgb,
        is_dark: bool,
        hovered: bool,
        dpi: f32,
    ) -> Result<IDCompositionSurface> {
        let pal = design::active_palette(is_dark);
        self.draw_surface(width_px, height_px, |dc, w, h| unsafe {
            let (start_x, end_x) = if is_left { (0.0, w) } else { (w, 0.0) };
            let grad = linear_grad_brush(
                dc,
                (start_x, 0.0),
                (end_x, 0.0),
                &[
                    (0.0, color_rgba(pill_rgb.0, pill_rgb.1, pill_rgb.2, 0.94)),
                    (0.55, color_rgba(pill_rgb.0, pill_rgb.1, pill_rgb.2, 0.72)),
                    (1.0, color_rgba(pill_rgb.0, pill_rgb.1, pill_rgb.2, 0.0)),
                ],
            )?;

            let pad_v = (1.0 * dpi).max(1.0);
            let r = (radius_px - dpi).clamp(2.0, (h - pad_v * 2.0) * 0.5);
            // Clip a wider rounded rect to keep both left (`19,0,0,19`) and right (`0,19,19,0`)
            // scroll overlays flush with the outer rounded corners of the dock pill (exact reference parity).
            dc.PushAxisAlignedClip(
                &rect(0.0, pad_v, w, h - pad_v),
                windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            let (rl, rr_x) = if is_left {
                (0.0, w + r * 2.0)
            } else {
                (-(r * 2.0), w)
            };
            dc.FillRoundedRectangle(&rrect(rl, pad_v, rr_x, h - pad_v, r), &grad);
            dc.PopAxisAlignedClip();

            // Crisp hover highlight pill behind the chevron
            let pill_sz = (22.0 * dpi).min(h - 8.0 * dpi).max(14.0);
            let cx = if is_left { w * 0.40 } else { w * 0.60 };
            let cy = h * 0.5;
            let pill_rc = rect(
                cx - pill_sz * 0.5,
                cy - pill_sz * 0.5,
                cx + pill_sz * 0.5,
                cy + pill_sz * 0.5,
            );
            if hovered {
                let hb = dc.CreateSolidColorBrush(
                    &if is_dark {
                        color_rgba(0xff, 0xff, 0xff, 0.14)
                    } else {
                        color_rgba(0x18, 0x18, 0x1b, 0.10)
                    },
                    None,
                )?;
                dc.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: pill_rc,
                        radiusX: 7.0 * dpi,
                        radiusY: 7.0 * dpi,
                    },
                    &hb,
                );
            }

            let glyph = icons::glyph_for_icon(if is_left {
                AppIcon::ChevronLeft
            } else {
                AppIcon::ChevronRight
            });
            let ink = if hovered {
                pal.accent
            } else {
                pal.text_primary
            };
            draw_fluent_str_d2d(
                &self.dwrite_factory,
                dc,
                glyph,
                pill_rc,
                13.0 * dpi,
                ink,
                if hovered { 1.0 } else { 0.88 },
            )?;
            Ok(())
        })
    }

    pub fn add_child(&self, parent: &IDCompositionVisual, child: &VisualNode) {
        unsafe {
            let _ = parent.AddVisual(&child.visual, false, None);
        }
    }

    pub fn remove_all_children(&self, parent: &IDCompositionVisual) {
        unsafe {
            let _ = parent.RemoveAllVisuals();
        }
    }

    pub fn commit(&self) {
        unsafe {
            let _ = self.dcomp_device.Commit();
        }
    }

    pub fn set_root_offset_y(&self, offset_y_px: f32) {
        unsafe {
            let _ = self.root.SetOffsetY2(offset_y_px);
        }
    }

    /// Drops every cached chip/island text format so `ui_font_family`, `ui_font_size` and
    /// `InterfaceFont` changes apply on the next redraw instead of after a restart.
    pub fn clear_text_format_cache(&self) {
        self.text_formats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Fades the whole DComp tree to `to`. A DirectComposition animation runs on the
    /// compositor (zero UI-thread frames); falls back to an instant opacity set.
    pub fn animate_root_opacity(&self, from: f32, to: f32, duration_secs: f32) {
        let Ok(visual) = self.root.cast::<IDCompositionVisual3>() else {
            return;
        };
        let from = from.clamp(0.0, 1.0);
        let to = to.clamp(0.0, 1.0);
        // SAFETY: the root visual and the device are live COM objects owned by the UI thread.
        unsafe {
            if duration_secs > 0.001
                && (to - from).abs() > 0.001
                && let Ok(animation) = self.dcomp_device.CreateAnimation()
                && animation
                    .AddCubic(0.0, from, (to - from) / duration_secs, 0.0, 0.0)
                    .is_ok()
                && animation.End(f64::from(duration_secs), to).is_ok()
                && visual.SetOpacity(&animation).is_ok()
            {
                return;
            }
            let _ = visual.SetOpacity2(to);
        }
    }

    /// Returns a cached chip text format; only the first request per (size, weight, alignment)
    /// creates COM objects, later redraws just AddRef the existing format.
    fn cached_text_format(&self, size: f32, bold: bool, align: u8) -> Result<IDWriteTextFormat> {
        let key = ((size * 2.0) as u16, bold, align);
        if let Some(cached) = self
            .text_formats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Ok(cached.clone());
        }
        unsafe {
            let fmt = self.dwrite_factory.CreateTextFormat(
                &HSTRING::from(design::fonts::dwrite_ui_family().as_str()),
                None,
                if bold {
                    DWRITE_FONT_WEIGHT_SEMI_BOLD
                } else {
                    DWRITE_FONT_WEIGHT_MEDIUM
                },
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from(design::fonts::ui_locale().as_str()),
            )?;
            let al = match align {
                1 => windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_LEADING,
                2 => windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_TRAILING,
                _ => DWRITE_TEXT_ALIGNMENT_CENTER,
            };
            fmt.SetTextAlignment(al)?;
            fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            fmt.SetWordWrapping(
                windows::Win32::Graphics::DirectWrite::DWRITE_WORD_WRAPPING_NO_WRAP,
            )?;
            let trim = windows::Win32::Graphics::DirectWrite::DWRITE_TRIMMING {
                granularity:
                    windows::Win32::Graphics::DirectWrite::DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            if let Ok(ellipsis) = self.dwrite_factory.CreateEllipsisTrimmingSign(&fmt) {
                let _ = fmt.SetTrimming(&trim, &ellipsis);
            }
            self.text_formats
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, fmt.clone());
            Ok(fmt)
        }
    }

    /// Horizontal auto-hide slide for left/right docks.
    pub fn set_root_offset_x(&self, offset_x_px: f32) {
        unsafe {
            let _ = self.root.SetOffsetX2(offset_x_px);
        }
    }

    /// Renders one of the 1-to-1 reference dock widget modules at automatic `height_dips`
    /// derived from the live dock pill height, using Slate/Aurora tokens and Windows 11 Fluent icons.
    #[allow(clippy::too_many_arguments)]
    pub fn create_widget_chip_surface(
        &self,
        spec: &WidgetChipSpec,
        width_dips: f32,
        height_dips: f32,
        dpi: f32,
        is_dark: bool,
        interaction: WidgetInteraction,
        border_rgb: Option<Rgb>,
        border_width_dips: f32,
    ) -> Result<IDCompositionSurface> {
        self.create_widget_chip_surface_with_chrome(
            spec,
            width_dips,
            height_dips,
            dpi,
            is_dark,
            interaction,
            border_rgb,
            border_width_dips,
            ChipChrome::Card,
        )
    }

    /// `create_widget_chip_surface` with an explicit chrome (`ChipChrome::Bare` for the
    /// Dynamic Island, where chips sit directly inside the capsule).
    #[allow(clippy::too_many_arguments)]
    pub fn create_widget_chip_surface_with_chrome(
        &self,
        spec: &WidgetChipSpec,
        width_dips: f32,
        height_dips: f32,
        dpi: f32,
        is_dark: bool,
        interaction: WidgetInteraction,
        border_rgb: Option<Rgb>,
        border_width_dips: f32,
        chrome: ChipChrome,
    ) -> Result<IDCompositionSurface> {
        let pal = design::active_palette_with_border(is_dark, border_rgb);
        let card_style = ChipCardStyle::resolve(chrome);
        // A frameless island chip never paints the per-widget card border.
        let border_rgb = if chrome == ChipChrome::Bare {
            None
        } else {
            border_rgb
        };
        let w_px = (width_dips * dpi).ceil().max(1.0) as u32;
        let h_px = (height_dips * dpi).ceil().max(1.0) as u32;
        let fg_p = color_rgba(
            pal.text_primary.0,
            pal.text_primary.1,
            pal.text_primary.2,
            0.98,
        );
        let fg_s = color_rgba(
            pal.text_secondary.0,
            pal.text_secondary.1,
            pal.text_secondary.2,
            0.90,
        );
        let track_c = if is_dark {
            color_rgba(0xff, 0xff, 0xff, 0.14)
        } else {
            color_rgba(0x18, 0x18, 0x1b, 0.14)
        };

        // Integer typography scale (`9, 10, 11, 12, 13, 15, 16, 18` DIP) for sharp pixel hinting.
        // Names are the real scaled size; legacy 12.5/11.5/10.5/9.5 aliases were removed.
        // Formats come from the engine cache; a redraw only AddRefs them.
        let fmt_18_b_l = self.cached_text_format(design::fonts::scaled_size(18.0), true, 1)?;
        let fmt_15_b_l = self.cached_text_format(design::fonts::scaled_size(15.0), true, 1)?;
        let fmt_14_b_c = self.cached_text_format(design::fonts::scaled_size(14.0), true, 0)?;
        let fmt_14_b_l = self.cached_text_format(design::fonts::scaled_size(14.0), true, 1)?;
        let fmt_13_b_c = self.cached_text_format(design::fonts::scaled_size(13.0), true, 0)?;
        let fmt_13_b_l = self.cached_text_format(design::fonts::scaled_size(13.0), true, 1)?;
        let fmt_12_b_l = self.cached_text_format(design::fonts::scaled_size(12.0), true, 1)?;
        let fmt_11_b_l = self.cached_text_format(design::fonts::scaled_size(11.0), true, 1)?;
        let fmt_11_b_c = self.cached_text_format(design::fonts::scaled_size(11.0), true, 0)?;
        let fmt_11_l = self.cached_text_format(design::fonts::scaled_size(11.0), false, 1)?;
        let fmt_11_b_r = self.cached_text_format(design::fonts::scaled_size(11.0), true, 2)?;
        let fmt_10_b_l = self.cached_text_format(design::fonts::scaled_size(10.0), true, 1)?;
        let fmt_10_l = self.cached_text_format(design::fonts::scaled_size(10.0), false, 1)?;
        let fmt_10_c = self.cached_text_format(design::fonts::scaled_size(10.0), false, 0)?;
        let fmt_9_l = self.cached_text_format(design::fonts::scaled_size(9.0), false, 1)?;

        // OpenType `tnum` (tabular lining figures) so clocks, timers, and metrics never jitter horizontally.
        let tnum_typo = unsafe {
            let typo = self.dwrite_factory.CreateTypography()?;
            let _ = typo
                .AddFontFeature(windows::Win32::Graphics::DirectWrite::DWRITE_FONT_FEATURE {
                nameTag:
                    windows::Win32::Graphics::DirectWrite::DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            });
            typo
        };

        let stroke_props = D2D1_STROKE_STYLE_PROPERTIES1 {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            ..Default::default()
        };
        let stroke: ID2D1StrokeStyle = unsafe {
            self.d2d_factory
                .CreateStrokeStyle(&stroke_props, None)?
                .into()
        };

        let div_c = color_rgba(pal.border_rgb.0, pal.border_rgb.1, pal.border_rgb.2, 0.85);
        let sel_fill_c = if is_dark {
            color_rgba(0x5b, 0x92, 0xff, 0.20)
        } else {
            color_rgba(0x24, 0x74, 0xe9, 0.15)
        };
        let sel_stroke_c = if is_dark {
            color_rgba(0x91, 0xb8, 0xff, 0.47)
        } else {
            color_rgba(0x2c, 0x74, 0xe9, 0.44)
        };
        let surf_sel_c = if is_dark {
            color_rgba(0xff, 0xff, 0xff, 0.08)
        } else {
            color_rgba(0x00, 0x00, 0x00, 0.05)
        };

        self.draw_surface(w_px, h_px, |dc, _w, _h| unsafe {
            let p_brush = dc.CreateSolidColorBrush(&fg_p, None)?;
            let s_brush = dc.CreateSolidColorBrush(&fg_s, None)?;
            let tr_brush = dc.CreateSolidColorBrush(&track_c, None)?;
            let div_brush = dc.CreateSolidColorBrush(&div_c, None)?;
            let sel_fill_b = dc.CreateSolidColorBrush(&sel_fill_c, None)?;
            let sel_stroke_b = dc.CreateSolidColorBrush(&sel_stroke_c, None)?;
            let surf_sel_b = dc.CreateSolidColorBrush(&surf_sel_c, None)?;
            let _prev = set_dpi_transform(dc, dpi);

            let active_sub = match interaction {
                WidgetInteraction::Idle => None,
                WidgetInteraction::Hovered(s) | WidgetInteraction::Pressed(s) => s,
            };
            let cy = height_dips * 0.5;
            let draw_card = |border: Option<Rgb>| {
                draw_chip_card(
                    &card_style,
                    dc,
                    width_dips,
                    height_dips,
                    is_dark,
                    None,
                    interaction,
                    border,
                    border_width_dips,
                )
            };
            let draw_glyph = |glyph: &str, r: D2D_RECT_F, sz: f32, col: Rgb, a: f32| {
                draw_fluent_str_d2d(&self.dwrite_factory, dc, glyph, r, sz, col, a)
            };
            let draw_brand = |id: &str, r: D2D_RECT_F, col: Rgb| {
                design::brand_paths::draw_brand_logo(&self.d2d_factory, dc, id, r, col, 0.98)
            };
            let measure_text_w = |text: &str, fmt: &IDWriteTextFormat| -> f32 {
                if text.is_empty() {
                    return 0.0;
                }
                let wtxt: Vec<u16> = text.encode_utf16().collect();
                if let Ok(layout) = self
                    .dwrite_factory
                    .CreateTextLayout(&wtxt, fmt, 400.0, height_dips.max(16.0))
                {
                    let _ = layout.SetTypography(
                        &tnum_typo,
                        windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE {
                            startPosition: 0,
                            length: wtxt.len() as u32,
                        },
                    );
                    let mut m = DWRITE_TEXT_METRICS::default();
                    if layout.GetMetrics(&mut m).is_ok() {
                        return m.widthIncludingTrailingWhitespace.ceil();
                    }
                }
                text.len() as f32 * 7.5
            };
            let draw_text =
                |text: &str,
                 fmt: &IDWriteTextFormat,
                 rect: D2D_RECT_F,
                 brush: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush| {
                    if text.is_empty() {
                        return;
                    }
                    let wtxt: Vec<u16> = text.encode_utf16().collect();
                    let max_w = (rect.right - rect.left).max(1.0);
                    let max_h = (rect.bottom - rect.top).max(1.0);
                    if let Ok(layout) = self
                        .dwrite_factory
                        .CreateTextLayout(&wtxt, fmt, max_w, max_h)
                    {
                        let trimming = DWRITE_TRIMMING {
                            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                            delimiter: 0,
                            delimiterCount: 0,
                        };
                        if let Ok(sign) = self.dwrite_factory.CreateEllipsisTrimmingSign(fmt) {
                            let _ = layout.SetTrimming(&trimming, Some(&sign));
                        }
                        let _ = layout.SetTypography(
                            &tnum_typo,
                            windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE {
                                startPosition: 0,
                                length: wtxt.len() as u32,
                            },
                        );
                        dc.DrawTextLayout(
                            Vector2 {
                                X: rect.left,
                                Y: rect.top,
                            },
                            &layout,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                };
            let draw_vdiv = |x: f32, h: f32| {
                let top = cy - h * 0.5;
                dc.FillRectangle(&rect(x, top, x + 1.0, top + h), &div_brush);
            };
            let draw_chip_ratio = |ratio: f32, color: Rgb| -> Result<()> {
                let w = width_dips - 16.0;
                if w <= 2.0 {
                    return Ok(());
                }
                draw_bar(
                    dc,
                    8.0,
                    height_dips - 5.0,
                    w,
                    2.5,
                    1.25,
                    ratio,
                    1.5,
                    &tr_brush,
                    color,
                    0.94,
                )
            };
            // `Simple` and `Custom` chips share the subtle accent-tinted icon well and 2-line layout.
            struct TwoLine<'a> {
                icon: &'a WidgetIcon,
                color: Rgb,
                primary: &'a str,
                secondary: &'a str,
                ratio: Option<f32>,
                primary_format: &'a IDWriteTextFormat,
                secondary_format: &'a IDWriteTextFormat,
            }
            let draw_two_line_chip = |chip: TwoLine<'_>| -> Result<()> {
                draw_card(border_rgb)?;
                let well_sz = 24.0_f32.min(height_dips - 10.0).max(18.0);
                let well_l = 8.0;
                let well_t = cy - well_sz * 0.5;
                let well_b = dc.CreateSolidColorBrush(
                    &color_rgba(
                        chip.color.0,
                        chip.color.1,
                        chip.color.2,
                        if is_dark { 0.16 } else { 0.12 },
                    ),
                    None,
                )?;
                dc.FillRoundedRectangle(
                    &rrect(well_l, well_t, well_l + well_sz, well_t + well_sz, 7.0),
                    &well_b,
                );
                draw_chip_icon(
                    &self.dwrite_factory,
                    &self.d2d_factory,
                    dc,
                    chip.icon,
                    well_l + well_sz * 0.5,
                    cy,
                    13.5,
                    chip.color,
                )?;
                let tx = well_l + well_sz + 7.0;
                let mid = height_dips * 0.50;
                draw_text(
                    chip.primary,
                    chip.primary_format,
                    rect(tx, (mid - 13.5).max(2.0), width_dips - 8.0, mid + 1.0),
                    &p_brush,
                );
                draw_text(
                    chip.secondary,
                    chip.secondary_format,
                    rect(
                        tx,
                        mid - 0.5,
                        width_dips - 8.0,
                        (mid + 13.0).min(height_dips - 4.0),
                    ),
                    &s_brush,
                );
                if let Some(ratio) = chip.ratio {
                    draw_chip_ratio(ratio, chip.color)?;
                }
                Ok(())
            };

            match spec {
                WidgetChipSpec::Clock {
                    time,
                    date,
                    weekday,
                } => {
                    draw_card(border_rgb)?;
                    let show_wd = !crate::widgets::clock_weekday_is_redundant(date, weekday);
                    let below = crate::widgets::chip_clock_below()
                        || (!date.is_empty() && width_dips < 126.0);
                    let clock_glyph = icons::named_glyph("Clock").unwrap_or("\u{E823}");
                    if date.is_empty() {
                        // Hidden date style: centered clock icon + crisp tabular time.
                        let time_w = measure_text_w(time, &fmt_15_b_l);
                        let start_x = ((width_dips - (18.0 + time_w)) * 0.5).max(8.0);
                        draw_glyph(
                            clock_glyph,
                            rect(start_x, cy - 8.0, start_x + 15.0, cy + 8.0),
                            14.0,
                            pal.text_secondary,
                            0.90,
                        )?;
                        draw_text(
                            time,
                            &fmt_15_b_l,
                            rect(start_x + 18.0, 0.0, width_dips - 6.0, height_dips),
                            &p_brush,
                        );
                    } else if below {
                        // Stacked Below layout (explicit setting or narrow compact chip).
                        let mid = height_dips * 0.54;
                        let sub_line = if show_wd {
                            format!("{date} · {weekday}")
                        } else {
                            date.clone()
                        };
                        let (l, r, t_fmt, s_fmt) = if width_dips >= 106.0 {
                            draw_glyph(
                                clock_glyph,
                                rect(9.0, cy - 8.0, 24.0, cy + 8.0),
                                14.0,
                                pal.text_secondary,
                                0.88,
                            )?;
                            (29.0, width_dips - 8.0, &fmt_14_b_l, &fmt_10_l)
                        } else {
                            (6.0, width_dips - 6.0, &fmt_14_b_c, &fmt_10_c)
                        };
                        draw_text(time, t_fmt, rect(l, (mid - 15.5).max(2.0), r, mid + 1.0), &p_brush);
                        draw_text(
                            &sub_line,
                            s_fmt,
                            rect(l, mid - 0.5, r, (mid + 13.0).min(height_dips - 2.0)),
                            &s_brush,
                        );
                    } else {
                        // Beside layout: dynamically measured time column + 1px divider + date/weekday.
                        draw_glyph(
                            clock_glyph,
                            rect(9.0, cy - 8.0, 24.0, cy + 8.0),
                            14.0,
                            pal.text_secondary,
                            0.90,
                        )?;
                        let time_fmt = if time.len() > 6 && width_dips < 168.0 {
                            &fmt_13_b_l
                        } else {
                            &fmt_15_b_l
                        };
                        let measured = measure_text_w(time, time_fmt)
                            .clamp(40.0, (width_dips - 86.0).max(46.0));
                        let time_r = 28.0 + measured + 2.0;
                        draw_text(time, time_fmt, rect(28.0, 0.0, time_r, height_dips), &p_brush);
                        let div_x = time_r + 5.0;
                        draw_vdiv(div_x, 18.0);
                        let dx = div_x + 7.0;
                        let mid = height_dips * 0.51;
                        if show_wd {
                            draw_text(
                                date,
                                &fmt_11_b_l,
                                rect(dx, (mid - 13.5).max(2.0), width_dips - 8.0, mid + 1.0),
                                &p_brush,
                            );
                            draw_text(
                                weekday,
                                &fmt_10_l,
                                rect(
                                    dx,
                                    mid - 0.5,
                                    width_dips - 8.0,
                                    (mid + 13.0).min(height_dips - 2.0),
                                ),
                                &s_brush,
                            );
                        } else {
                            draw_text(
                                date,
                                &fmt_11_b_l,
                                rect(dx, 0.0, width_dips - 8.0, height_dips),
                                &p_brush,
                            );
                        }
                    }
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
                    draw_card(border_rgb)?;
                    if !*available {
                        let txt_w = measure_text_w(condition, &fmt_11_l);
                        let total = (18.0 + txt_w).min(width_dips - 16.0);
                        let sx = ((width_dips - total) * 0.5).max(8.0);
                        draw_glyph(
                            "\u{E753}",
                            rect(sx, cy - 8.0, sx + 15.0, cy + 8.0),
                            14.0,
                            pal.text_secondary,
                            0.75,
                        )?;
                        draw_text(
                            condition,
                            &fmt_11_l,
                            rect(sx + 19.0, 0.0, width_dips - 8.0, height_dips),
                            &s_brush,
                        );
                    } else {
                        // 5-state semantic tinted icon well + crisp temp + Place/Condition + optional Hi/Lo.
                        let glyph = match crate::widgets::telemetry::weather_kind_from_condition(
                            condition, *is_rain,
                        ) {
                            crate::widgets::WeatherKind::Storm => "\u{E945}",
                            crate::widgets::WeatherKind::Snow => "\u{E9CA}",
                            crate::widgets::WeatherKind::Rain => "\u{EB42}",
                            crate::widgets::WeatherKind::Cloudy => "\u{E753}",
                            crate::widgets::WeatherKind::Clear => "\u{E706}",
                        };
                        let well_sz = 26.0_f32.min(height_dips - 8.0).max(20.0);
                        let well_l = 8.0;
                        let well_t = cy - well_sz * 0.5;
                        let well_rect = rect(well_l, well_t, well_l + well_sz, well_t + well_sz);
                        let well_b = dc.CreateSolidColorBrush(
                            &color_rgba(
                                icon_color.0,
                                icon_color.1,
                                icon_color.2,
                                if is_dark { 0.18 } else { 0.13 },
                            ),
                            None,
                        )?;
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: well_rect,
                                radiusX: 7.0,
                                radiusY: 7.0,
                            },
                            &well_b,
                        );
                        draw_glyph(glyph, well_rect, 15.0, *icon_color, 0.98)?;
                        let temp_fmt = if width_dips >= 184.0 {
                            &fmt_18_b_l
                        } else {
                            &fmt_15_b_l
                        };
                        let temp_l = well_l + well_sz + 6.0;
                        let temp_w = measure_text_w(temp, temp_fmt).clamp(32.0, 50.0);
                        draw_text(
                            temp,
                            temp_fmt,
                            rect(temp_l, 0.0, temp_l + temp_w + 2.0, height_dips),
                            &p_brush,
                        );
                        let px = temp_l + temp_w + 5.0;
                        let show_range = today_range.is_some() && width_dips >= 184.0;
                        let pr = if show_range {
                            width_dips - 58.0
                        } else {
                            width_dips - 8.0
                        };
                        let mid = height_dips * 0.50;
                        if pr > px + 18.0 {
                            draw_text(
                                place,
                                &fmt_11_b_l,
                                rect(px, (mid - 13.5).max(2.0), pr, mid + 1.0),
                                &p_brush,
                            );
                            draw_text(
                                condition,
                                &fmt_10_l,
                                rect(px, mid - 0.5, pr, (mid + 13.0).min(height_dips - 2.0)),
                                &s_brush,
                            );
                        }
                        if show_range && let Some(range) = today_range {
                            let sep_x = width_dips - 54.0;
                            draw_vdiv(sep_x, 18.0);
                            let mut parts = range.split('/');
                            let hi = parts.next().map(str::trim).unwrap_or("");
                            let lo = parts.next().map(str::trim).unwrap_or("");
                            if !hi.is_empty() && !lo.is_empty() {
                                draw_text(
                                    &format!("↑{hi}"),
                                    &fmt_10_b_l,
                                    rect(sep_x + 6.0, (mid - 13.0).max(2.0), width_dips - 6.0, mid + 0.5),
                                    &p_brush,
                                );
                                draw_text(
                                    &format!("↓{lo}"),
                                    &fmt_10_l,
                                    rect(
                                        sep_x + 6.0,
                                        mid - 0.5,
                                        width_dips - 6.0,
                                        (mid + 13.0).min(height_dips - 2.0),
                                    ),
                                    &s_brush,
                                );
                            } else {
                                draw_text(
                                    range,
                                    &fmt_10_b_l,
                                    rect(sep_x + 6.0, 0.0, width_dips - 6.0, height_dips),
                                    &p_brush,
                                );
                            }
                        }
                    }
                }
                WidgetChipSpec::DayProgress {
                    ratio,
                    percent,
                    unit,
                } => {
                    draw_card(border_rgb)?;
                    // Dawn (amber) -> Day (sky blue) -> Dusk (violet) semantic ring + 2-line stack.
                    let clamped = ratio.clamp(0.0, 1.0);
                    let ring_col = if clamped < 0.30 {
                        if is_dark { (0xf5, 0x9e, 0x0b) } else { (0xd9, 0x77, 0x06) }
                    } else if clamped < 0.75 {
                        pal.widget_blue
                    } else if is_dark {
                        (0xa7, 0x8b, 0xfa)
                    } else {
                        (0x7c, 0x3a, 0xed)
                    };
                    draw_arc_ring(
                        &self.d2d_factory,
                        dc,
                        20.0,
                        cy,
                        9.5_f32.min(height_dips * 0.30),
                        clamped,
                        2.8,
                        &tr_brush,
                        ring_col,
                        1.0,
                        &stroke,
                    )?;
                    let mid = height_dips * 0.50;
                    draw_text(
                        percent,
                        &fmt_12_b_l,
                        rect(36.0, (mid - 13.5).max(2.0), width_dips - 8.0, mid + 1.0),
                        &p_brush,
                    );
                    draw_text(
                        unit,
                        &fmt_10_l,
                        rect(36.0, mid - 0.5, width_dips - 8.0, (mid + 12.5).min(height_dips - 2.0)),
                        &s_brush,
                    );
                }
                WidgetChipSpec::Timed {
                    is_focus,
                    running,
                    text,
                } => {
                    draw_card(border_rgb)?;
                    let is_stopwatch = !*is_focus && text.contains('.');
                    let is_countdown = !*is_focus && !is_stopwatch;
                    let btn_sz = 25.0_f32.min(height_dips - 8.0).max(20.0);
                    let btn_l = width_dips - btn_sz - 7.0;
                    let btn_t = cy - btn_sz * 0.5;
                    let (btn_tint, btn_active_fg) = if *is_focus {
                        let (is_break, ratio) = crate::widgets::chip_focus_hint();
                        let ring_col = if is_break {
                            if is_dark { (0x34, 0xd3, 0x99) } else { (0x05, 0x96, 0x69) }
                        } else if is_dark {
                            (0x8b, 0x5c, 0xf6)
                        } else {
                            (0x7c, 0x3a, 0xed)
                        };
                        let glyph = if is_break { "\u{E708}" } else { "\u{E7C8}" };
                        // 20px circular progress ring enclosing Focus/Break glyph.
                        let rx = 19.0;
                        draw_arc_ring(
                            &self.d2d_factory,
                            dc,
                            rx,
                            cy,
                            9.0_f32.min(height_dips * 0.29),
                            ratio,
                            2.2,
                            &tr_brush,
                            ring_col,
                            0.96,
                            &stroke,
                        )?;
                        draw_glyph(
                            glyph,
                            rect(rx - 6.0, cy - 6.0, rx + 6.0, cy + 6.0),
                            10.0,
                            if *running { ring_col } else { pal.text_secondary },
                            0.96,
                        )?;
                        let time_fmt = if text.len() > 5 { &fmt_11_b_l } else { &fmt_13_b_l };
                        draw_text(text, time_fmt, rect(34.0, 0.0, btn_l - 4.0, height_dips), &p_brush);
                        (ring_col, ring_col)
                    } else if is_stopwatch {
                        let em = if is_dark { (0x34, 0xd3, 0x99) } else { (0x05, 0x96, 0x69) };
                        draw_glyph(
                            "\u{E916}",
                            rect(8.0, cy - 8.0, 24.0, cy + 8.0),
                            14.5,
                            if *running { em } else { pal.text_secondary },
                            0.95,
                        )?;
                        if *running {
                            let dot_b =
                                dc.CreateSolidColorBrush(&color_rgba(em.0, em.1, em.2, 0.96), None)?;
                            dc.FillEllipse(
                                &D2D1_ELLIPSE {
                                    point: Vector2 { X: 25.0, Y: cy - 6.0 },
                                    radiusX: 2.3,
                                    radiusY: 2.3,
                                },
                                &dot_b,
                            );
                        }
                        if let Some((mm_ss, cs)) = text.split_once('.') {
                            let main_w = measure_text_w(mm_ss, &fmt_13_b_l);
                            let main_l = 30.0;
                            draw_text(
                                mm_ss,
                                &fmt_13_b_l,
                                rect(main_l, 0.0, (main_l + main_w + 2.0).min(btn_l - 16.0), height_dips),
                                &p_brush,
                            );
                            let cs_l = main_l + main_w;
                            if cs_l + 14.0 <= btn_l - 2.0 {
                                draw_text(
                                    &format!(".{cs}"),
                                    &fmt_10_l,
                                    rect(cs_l, 2.0, btn_l - 2.0, height_dips),
                                    &s_brush,
                                );
                            }
                        } else {
                            draw_text(text, &fmt_13_b_l, rect(30.0, 0.0, btn_l - 4.0, height_dips), &p_brush);
                        }
                        (em, em)
                    } else {
                        // Countdown: distinct timer glyph + dynamic font + 2.5px rounded bottom bar.
                        let cd_ratio = crate::widgets::chip_countdown_ratio();
                        let bar_col = if cd_ratio <= 0.10 {
                            pal.danger
                        } else if cd_ratio <= 0.25 {
                            pal.warning
                        } else {
                            pal.accent
                        };
                        draw_glyph(
                            "\u{EA8F}",
                            rect(9.0, cy - 8.5, 25.0, cy + 7.5),
                            14.5,
                            if *running { bar_col } else { pal.text_secondary },
                            0.95,
                        )?;
                        let time_fmt = if text.len() > 5 { &fmt_11_b_l } else { &fmt_13_b_l };
                        draw_text(text, time_fmt, rect(30.0, 0.0, btn_l - 4.0, height_dips - 2.0), &p_brush);
                        if is_countdown {
                            draw_chip_ratio(cd_ratio, bar_col)?;
                        }
                        (bar_col, bar_col)
                    };

                    let btn_rect = rect(btn_l, btn_t, btn_l + btn_sz, btn_t + btn_sz);
                    let btn_rr = D2D1_ROUNDED_RECT {
                        rect: btn_rect,
                        radiusX: 7.0,
                        radiusY: 7.0,
                    };
                    if *running {
                        let fill_b = dc.CreateSolidColorBrush(
                            &color_rgba(btn_tint.0, btn_tint.1, btn_tint.2, if is_dark { 0.22 } else { 0.16 }),
                            None,
                        )?;
                        let stroke_b = dc.CreateSolidColorBrush(
                            &color_rgba(btn_tint.0, btn_tint.1, btn_tint.2, if is_dark { 0.48 } else { 0.42 }),
                            None,
                        )?;
                        dc.FillRoundedRectangle(&btn_rr, &fill_b);
                        dc.DrawRoundedRectangle(&btn_rr, &stroke_b, 1.0, None);
                    } else if active_sub == Some(1) {
                        let hb = dc.CreateSolidColorBrush(
                            &if is_dark {
                                color_rgba(0xff, 0xff, 0xff, 0.14)
                            } else {
                                color_rgba(0x00, 0x00, 0x00, 0.10)
                            },
                            None,
                        )?;
                        dc.FillRoundedRectangle(&btn_rr, &hb);
                    }
                    draw_glyph(
                        if *running { "\u{E769}" } else { "\u{E768}" },
                        btn_rect,
                        11.0,
                        if *running { btn_active_fg } else { pal.text_primary },
                        0.96,
                    )?;
                }
                WidgetChipSpec::SystemStats { columns } => {
                    draw_card(border_rgb)?;
                    if columns.is_empty() {
                        draw_text("—", &fmt_11_b_c, rect(8.0, 0.0, width_dips - 8.0, height_dips), &s_brush);
                    } else {
                        let count = columns.len().min(4);
                        let (pad_x, gap) = (10.0_f32, 10.0_f32);
                        let col_w = ((width_dips - pad_x * 2.0 - gap * (count.saturating_sub(1) as f32))
                            .max(24.0))
                            / count as f32;
                        let warm_col = if is_dark { pal.warning } else { (0xb4, 0x53, 0x09) };
                        let warm_b = dc.CreateSolidColorBrush(
                            &color_rgba(warm_col.0, warm_col.1, warm_col.2, 0.98),
                            None,
                        )?;
                        let compact = height_dips < 36.0;
                        let bar_h = if compact { 2.0_f32 } else { 2.5_f32 };
                        let bar_y = if compact {
                            (height_dips - bar_h - 2.0).max(height_dips * 0.80)
                        } else {
                            (height_dips - bar_h - 4.5).max(height_dips * 0.76)
                        };
                        let label_top = if compact { 2.5_f32 } else { 3.5_f32 };
                        let label_bottom = (height_dips * 0.40).max(label_top + 9.0);
                        let value_top = (height_dips * 0.38).max(label_bottom - 0.5);
                        let value_bottom = (bar_y - 1.5).max(value_top + 10.5);
                        let label_fmt = if height_dips < 33.0 {
                            self.cached_text_format(design::fonts::scaled_size(8.5), false, 1)?
                        } else if compact {
                            fmt_9_l.clone()
                        } else {
                            fmt_10_l.clone()
                        };
                        let value_fmt = if height_dips < 33.0 {
                            self.cached_text_format(design::fonts::scaled_size(10.5), true, 1)?
                        } else if height_dips < 38.0 {
                            fmt_11_b_l.clone()
                        } else {
                            fmt_12_b_l.clone()
                        };
                        let vdiv_h = (height_dips - 12.0).clamp(14.0, 20.0);
                        for (i, col) in columns.iter().take(4).enumerate() {
                            let cx_l = pad_x + i as f32 * (col_w + gap);
                            if i > 0 {
                                draw_vdiv(cx_l - gap * 0.5, vdiv_h);
                            }
                            draw_text(&col.label, &label_fmt, rect(cx_l, label_top, cx_l + col_w, label_bottom), &s_brush);
                            draw_text(
                                &col.value_text,
                                &value_fmt,
                                rect(cx_l, value_top, cx_l + col_w, value_bottom),
                                if col.warm_text { &warm_b } else { &p_brush },
                            );
                            draw_bar(
                                dc,
                                cx_l,
                                bar_y,
                                col_w,
                                bar_h,
                                bar_h * 0.5,
                                col.ratio,
                                1.5,
                                &tr_brush,
                                col.bar_color,
                                0.96,
                            )?;
                        }
                    }
                }
                WidgetChipSpec::NetSpeed {
                    down,
                    up,
                    down_history,
                    up_history,
                } => {
                    draw_card(border_rgb)?;
                    // Hide the sparkline cleanly on narrow Compact widths so units never clip.
                    let rx = if width_dips >= 135.0 {
                        let (sx, sw, sh) = (10.0_f32, 38.0_f32, 22.0_f32);
                        let sy = cy - sh * 0.5;
                        let build_spark = |samples: &[f32], closed: bool| -> Result<(windows::Win32::Graphics::Direct2D::ID2D1PathGeometry, Vector2)> {
                            let pts: Vec<Vector2> = if samples.len() >= 2 {
                                let n = samples.len();
                                samples
                                    .iter()
                                    .enumerate()
                                    .map(|(i, &v)| Vector2 {
                                        X: sx + 1.0 + (i as f32 / (n - 1) as f32) * (sw - 2.0),
                                        Y: (sy + sh - 2.0) - v.clamp(0.05, 1.0) * (sh - 5.0),
                                    })
                                    .collect()
                            } else {
                                vec![
                                    Vector2 { X: sx + 1.0, Y: sy + sh - 3.0 },
                                    Vector2 { X: sx + sw - 1.0, Y: sy + sh - 3.0 },
                                ]
                            };
                            let path = self.d2d_factory.CreatePathGeometry()?;
                            let sink = path.Open()?;
                            sink.BeginFigure(
                                pts[0],
                                if closed {
                                    windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_FILLED
                                } else {
                                    D2D1_FIGURE_BEGIN_HOLLOW
                                },
                            );
                            for pair in pts.windows(2) {
                                let (p0, p1) = (pair[0], pair[1]);
                                let mx = (p0.X + p1.X) * 0.5;
                                sink.AddBezier(&windows::Win32::Graphics::Direct2D::Common::D2D1_BEZIER_SEGMENT {
                                    point1: Vector2 { X: mx, Y: p0.Y },
                                    point2: Vector2 { X: mx, Y: p1.Y },
                                    point3: p1,
                                });
                            }
                            let tip = *pts.last().unwrap_or(&pts[0]);
                            if closed {
                                sink.AddLine(Vector2 { X: tip.X, Y: sy + sh });
                                sink.AddLine(Vector2 { X: pts[0].X, Y: sy + sh });
                                sink.EndFigure(windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_CLOSED);
                            } else {
                                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                            }
                            sink.Close()?;
                            Ok((path.into(), tip))
                        };

                        let (down_area, tip) = build_spark(down_history, true)?;
                        let area_b = linear_grad_brush(
                            dc,
                            (sx, sy),
                            (sx, sy + sh),
                            &[
                                (0.0, color_rgba(pal.success.0, pal.success.1, pal.success.2, 0.28)),
                                (1.0, color_rgba(pal.success.0, pal.success.1, pal.success.2, 0.0)),
                            ],
                        )?;
                        dc.FillGeometry(&down_area, &area_b, None);

                        let (up_line, _) = build_spark(up_history, false)?;
                        let up_b = dc.CreateSolidColorBrush(
                            &color_rgba(pal.widget_blue.0, pal.widget_blue.1, pal.widget_blue.2, 0.82),
                            None,
                        )?;
                        dc.DrawGeometry(&up_line, &up_b, 1.25, Some(&stroke));

                        let (down_line, _) = build_spark(down_history, false)?;
                        let down_b = dc.CreateSolidColorBrush(
                            &color_rgba(pal.success.0, pal.success.1, pal.success.2, 0.98),
                            None,
                        )?;
                        dc.DrawGeometry(&down_line, &down_b, 1.5, Some(&stroke));
                        dc.FillEllipse(
                            &D2D1_ELLIPSE { point: tip, radiusX: 1.5, radiusY: 1.5 },
                            &down_b,
                        );
                        draw_vdiv(sx + sw + 6.0, 20.0);
                        sx + sw + 13.0
                    } else {
                        10.0
                    };

                    let mid = height_dips * 0.5;
                    let unit_x = (width_dips - 36.0).max(rx + 38.0);
                    for (glyph, col, text, y_top, y_bot, num_top, num_bot) in [
                        ("\u{E74B}", pal.success, down.as_str(), (mid - 14.0).max(2.0), mid, (mid - 15.0).max(2.0), mid + 1.0),
                        ("\u{E74A}", pal.widget_blue, up.as_str(), mid, (mid + 14.0).min(height_dips - 2.0), mid - 1.0, (mid + 15.0).min(height_dips - 2.0)),
                    ] {
                        let (num, unit) = text.rsplit_once(' ').unwrap_or((text, ""));
                        draw_glyph(glyph, rect(rx, y_top, rx + 11.0, y_bot), 9.5, col, 0.98)?;
                        draw_text(num, &fmt_11_b_l, rect(rx + 13.0, num_top, unit_x - 2.0, num_bot), &p_brush);
                        draw_text(unit, &fmt_10_l, rect(unit_x, y_top, width_dips - 6.0, num_bot.min(y_bot + 1.0)), &s_brush);
                    }
                }
                WidgetChipSpec::BatteryLiquid {
                    present,
                    percent,
                    charging,
                    wave_phase,
                } => {
                    // Exact reference BatteryWidget.cs horizontal liquid-fill card with scrolling sinusoidal
                    // meniscus wave + -22deg skewed left-to-right charging sheen!
                    if !*present {
                        draw_card(border_rgb)?;
                        let sx = ((width_dips - 36.0) * 0.5).max(8.0);
                        draw_glyph("\u{E83E}", rect(sx, cy - 8.0, sx + 15.0, cy + 8.0), 14.0, pal.text_secondary, 0.90)?;
                        draw_text("AC", &fmt_12_b_l, rect(sx + 18.0, 0.0, width_dips - 6.0, height_dips), &s_brush);
                    } else {
                        let effective_border = if border_rgb.is_none() && *percent <= 20 {
                            Some(pal.danger)
                        } else {
                            border_rgb
                        };
                        draw_card(effective_border)?;

                        let (bx, by) = (4.0_f32, 4.0_f32);
                        let bw = (width_dips - 12.5).max(12.0);
                        let bh = (height_dips - 8.0).max(12.0);
                        let br = (bh * 0.28).min(8.5);
                        let chamber_rr = rrect(bx, by, bx + bw, by + bh, br);
                        let chamber_bg = dc.CreateSolidColorBrush(
                            &if is_dark {
                                color_rgba(0x00, 0x00, 0x00, 0.28)
                            } else {
                                color_rgba(0x18, 0x18, 0x1b, 0.085)
                            },
                            None,
                        )?;
                        dc.FillRoundedRectangle(&chamber_rr, &chamber_bg);

                        // Aurora-aligned palette: (left_col, right_col, glow_col, back_wave_col)
                        let (left_c, right_c, glow_c, back_c) = if *percent <= 20 {
                            ((0xc2_u8, 0x30_u8, 0x42_u8), (0xec, 0x5e, 0x6c), (0xf8, 0x71, 0x71), (0xf4, 0x80, 0x8a))
                        } else if *percent <= 50 {
                            ((0xc0_u8, 0x7e_u8, 0x22_u8), (0xf2, 0xb8, 0x4e), (0xfb, 0xbf, 0x24), (0xf5, 0xc8, 0x70))
                        } else {
                            ((0x1d_u8, 0x94_u8, 0x86_u8), (0x44, 0xd6, 0xb8), (0x2d, 0xd4, 0xbf), (0x60, 0xe2, 0xc8))
                        };

                        // Subtle right-side micro battery terminal nub.
                        let nub_h = (bh * 0.36).round().max(6.0);
                        let nub_y = by + (bh - nub_h) * 0.5;
                        let nub_col = if *percent >= 98 {
                            color_rgba(right_c.0, right_c.1, right_c.2, 0.95)
                        } else if is_dark {
                            color_rgba(0xff, 0xff, 0xff, 0.24)
                        } else {
                            color_rgba(0x18, 0x18, 0x1b, 0.22)
                        };
                        let nub_b = dc.CreateSolidColorBrush(&nub_col, None)?;
                        dc.FillRoundedRectangle(&rrect(bx + bw + 1.6, nub_y, bx + bw + 4.0, nub_y + nub_h, 1.2), &nub_b);

                        // Clip liquid fill, meniscus glow, bubbles, and sheen strictly to the chamber.
                        let edge_x = bx + bw * ((*percent).clamp(4, 100) as f32 / 100.0);
                        let amp = 2.8_f32.min((edge_x - bx) * 0.30);

                        let chamber_mask: windows::Win32::Graphics::Direct2D::ID2D1Geometry = self
                            .d2d_factory
                            .CreateRoundedRectangleGeometry(&chamber_rr)?
                            .into();
                        let layer_params = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS1 {
                            contentBounds: chamber_rr.rect,
                            geometricMask: std::mem::ManuallyDrop::new(Some(chamber_mask)),
                            maskAntialiasMode: windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                            maskTransform: Matrix3x2::identity(),
                            opacity: 1.0,
                            opacityBrush: std::mem::ManuallyDrop::new(None),
                            layerOptions: windows::Win32::Graphics::Direct2D::D2D1_LAYER_OPTIONS1_NONE,
                        };
                        dc.PushLayer(&layer_params, None);

                        // 1. Soft ambient glow halo along the meniscus into the empty side.
                        if *percent > 4 && *percent < 98 {
                            let glow_b = linear_grad_brush(
                                dc,
                                (edge_x - 10.0, by),
                                (edge_x + 12.0, by),
                                &[
                                    (0.0, color_rgba(glow_c.0, glow_c.1, glow_c.2, 0.0)),
                                    (0.5, color_rgba(glow_c.0, glow_c.1, glow_c.2, 0.44)),
                                    (1.0, color_rgba(glow_c.0, glow_c.1, glow_c.2, 0.0)),
                                ],
                            )?;
                            dc.FillRectangle(&rect(edge_x - 10.0, by, edge_x + 12.0, by + bh), &glow_b);
                        }

                        let build_wave = |ex: f32, wave_amp: f32, phase: f32, filled: bool| -> Result<windows::Win32::Graphics::Direct2D::ID2D1PathGeometry> {
                            let half = bh * 0.5;
                            let y0 = by - phase.rem_euclid(1.0) * bh;
                            let path = self.d2d_factory.CreatePathGeometry()?;
                            let sink = path.Open()?;
                            if filled {
                                sink.BeginFigure(Vector2 { X: bx, Y: y0 }, windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_FILLED);
                                sink.AddLine(Vector2 { X: ex, Y: y0 });
                            } else {
                                sink.BeginFigure(Vector2 { X: ex, Y: y0 }, D2D1_FIGURE_BEGIN_HOLLOW);
                            }
                            for period in 0..2 {
                                let base_y = y0 + period as f32 * bh;
                                for (sign, seg_y) in [(1.0_f32, base_y), (-1.0_f32, base_y + half)] {
                                    sink.AddBezier(&windows::Win32::Graphics::Direct2D::Common::D2D1_BEZIER_SEGMENT {
                                        point1: Vector2 { X: ex + sign * wave_amp, Y: seg_y + half * 0.36 },
                                        point2: Vector2 { X: ex + sign * wave_amp, Y: seg_y + half * 0.64 },
                                        point3: Vector2 { X: ex, Y: seg_y + half },
                                    });
                                }
                            }
                            if filled {
                                sink.AddLine(Vector2 { X: bx, Y: y0 + bh * 2.0 });
                                sink.EndFigure(windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_CLOSED);
                            } else {
                                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                            }
                            sink.Close()?;
                            Ok(path.into())
                        };

                        // 2. Secondary out-of-phase depth wave while charging below 100%.
                        if *charging && *percent < 100 {
                            let back_geom: windows::Win32::Graphics::Direct2D::ID2D1Geometry =
                                build_wave((edge_x + 2.2).min(bx + bw), amp * 0.85, *wave_phase + 0.45, true)?.into();
                            let back_b = dc.CreateSolidColorBrush(&color_rgba(back_c.0, back_c.1, back_c.2, 0.34), None)?;
                            dc.FillGeometry(&back_geom, &back_b, None);
                        }

                        // 3. Primary liquid fill with horizontal brightening toward the active edge.
                        let liq_b = linear_grad_brush(
                            dc,
                            (bx, by),
                            (edge_x.max(bx + 1.0), by + bh * 0.5),
                            &[
                                (0.0, color_rgba(left_c.0, left_c.1, left_c.2, 0.96)),
                                (1.0, color_rgba(right_c.0, right_c.1, right_c.2, 0.98)),
                            ],
                        )?;
                        let liquid_geom: windows::Win32::Graphics::Direct2D::ID2D1Geometry =
                            if *charging && *percent < 100 {
                                build_wave(edge_x, amp, *wave_phase, true)?.into()
                            } else {
                                self.d2d_factory
                                    .CreateRectangleGeometry(&rect(bx, by, edge_x, by + bh))?
                                    .into()
                            };
                        dc.FillGeometry(&liquid_geom, &liq_b, None);

                        // 4. Top glass gloss inside the liquid fill.
                        let gloss_b = linear_grad_brush(
                            dc,
                            (bx, by),
                            (bx, by + bh * 0.46),
                            &[
                                (0.0, color_rgba(0xff, 0xff, 0xff, 0.22)),
                                (1.0, color_rgba(0xff, 0xff, 0xff, 0.0)),
                            ],
                        )?;
                        dc.FillGeometry(&liquid_geom, &gloss_b, None);

                        // 5. Charging micro-particles + -22deg skewed sheen clipped to liquid fill.
                        if *charging {
                            if edge_x - bx > 18.0 {
                                let span = (edge_x - bx - 8.0).max(4.0);
                                for i in 0..3 {
                                    let p = (*wave_phase + i as f32 * 0.33).rem_euclid(1.0);
                                    let px = bx + 4.0 + p * span;
                                    let py = by
                                        + bh * (0.28 + i as f32 * 0.22)
                                        + (p * std::f32::consts::TAU + i as f32).sin() * 2.0;
                                    let alpha = (p * std::f32::consts::PI).sin() * 0.36;
                                    if alpha > 0.02 {
                                        let pb = dc.CreateSolidColorBrush(&color_rgba(0xff, 0xff, 0xff, alpha), None)?;
                                        dc.FillEllipse(
                                            &D2D1_ELLIPSE { point: Vector2 { X: px, Y: py }, radiusX: 1.35, radiusY: 1.35 },
                                            &pb,
                                        );
                                    }
                                }
                            }
                            let sheen_w = 48.0_f32;
                            let t = (wave_phase / 0.875).clamp(0.0, 1.0);
                            let eased = if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 };
                            let start_x = bx - sheen_w - 20.0;
                            let sx = start_x + (edge_x + 20.0 - start_x) * eased;
                            let sheen_b = linear_grad_brush(
                                dc,
                                (sx, by),
                                (sx + sheen_w, by + bh * 0.42),
                                &[
                                    (0.0, color_rgba(0xff, 0xff, 0xff, 0.0)),
                                    (0.35, color_rgba(0xff, 0xff, 0xff, 0.10)),
                                    (0.50, color_rgba(0xff, 0xff, 0xff, 0.24)),
                                    (0.65, color_rgba(0xff, 0xff, 0xff, 0.10)),
                                    (1.0, color_rgba(0xff, 0xff, 0xff, 0.0)),
                                ],
                            )?;
                            dc.FillGeometry(&liquid_geom, &sheen_b, None);
                        }

                        // 6. Crisp meniscus highlight line along the liquid edge.
                        if *percent > 4 && *percent < 99 {
                            let edge_b = dc.CreateSolidColorBrush(&color_rgba(0xff, 0xff, 0xff, 0.44), None)?;
                            if *charging {
                                let edge_geom: windows::Win32::Graphics::Direct2D::ID2D1Geometry =
                                    build_wave(edge_x, amp, *wave_phase, false)?.into();
                                dc.DrawGeometry(&edge_geom, &edge_b, 1.1, Some(&stroke));
                            } else {
                                dc.DrawLine(
                                    Vector2 { X: edge_x - 0.5, Y: by },
                                    Vector2 { X: edge_x - 0.5, Y: by + bh },
                                    &edge_b,
                                    1.1,
                                    None,
                                );
                            }
                        }

                        dc.PopLayer();
                        let _ = std::mem::ManuallyDrop::into_inner(layer_params.geometricMask);

                        // Recessed inner hairline stroke around the capsule chamber.
                        let chamber_rim_b = dc.CreateSolidColorBrush(
                            &if is_dark {
                                color_rgba(0xff, 0xff, 0xff, 0.12)
                            } else {
                                color_rgba(0x18, 0x18, 0x1b, 0.10)
                            },
                            None,
                        )?;
                        dc.DrawRoundedRectangle(
                            &rrect(bx + 0.5, by + 0.5, bx + bw - 0.5, by + bh - 0.5, (br - 0.5).max(1.0)),
                            &chamber_rim_b,
                            0.85,
                            None,
                        );
                        draw_chip_overlay(dc, width_dips, height_dips, is_dark, interaction)?;

                        let mid_light_meniscus = !is_dark && (35..=70).contains(percent);
                        let use_dark_ink = !is_dark && *percent < 35;
                        let fg_rgb = if use_dark_ink { pal.text_primary } else { (0xff, 0xff, 0xff) };
                        let fg_a = if use_dark_ink { 0.94 } else { 0.98 };
                        let fg_b = dc.CreateSolidColorBrush(&color_rgba(fg_rgb.0, fg_rgb.1, fg_rgb.2, fg_a), None)?;
                        let sh_b = dc.CreateSolidColorBrush(&color_rgba(0, 0, 0, 0.32), None)?;

                        let label = crate::widgets::format_percent(*percent);
                        let chamber_cx = bx + bw * 0.5;
                        if mid_light_meniscus {
                            let pill_w = (measure_text_w(&label, &fmt_13_b_l) + if *charging { 20.0 } else { 12.0 })
                                .min(bw - 4.0);
                            let pill_h = 18.0_f32.min(bh - 4.0);
                            let pill_b = dc.CreateSolidColorBrush(&color_rgba(0x0f, 0x17, 0x2a, 0.34), None)?;
                            dc.FillRoundedRectangle(
                                &rrect(chamber_cx - pill_w * 0.5, cy - pill_h * 0.5, chamber_cx + pill_w * 0.5, cy + pill_h * 0.5, 6.0),
                                &pill_b,
                            );
                        }
                        if *charging {
                            let bolt_w = 12.0;
                            let start_x = chamber_cx - (bolt_w + measure_text_w(&label, &fmt_13_b_l).max(22.0)) * 0.5;
                            if !use_dark_ink {
                                draw_glyph("\u{E945}", rect(start_x, cy - 5.0, start_x + 11.5, cy + 7.0), 11.0, (0, 0, 0), 0.32)?;
                                draw_text(&label, &fmt_13_b_l, rect(start_x + bolt_w, 1.0, bx + bw - 2.0, height_dips + 1.0), &sh_b);
                            }
                            draw_glyph("\u{E945}", rect(start_x, cy - 6.0, start_x + 11.5, cy + 6.0), 11.0, fg_rgb, fg_a)?;
                            draw_text(&label, &fmt_13_b_l, rect(start_x + bolt_w, 0.0, bx + bw - 2.0, height_dips), &fg_b);
                        } else {
                            if !use_dark_ink {
                                draw_text(&label, &fmt_13_b_c, rect(bx, 1.0, bx + bw, height_dips + 1.0), &sh_b);
                            }
                            draw_text(&label, &fmt_13_b_c, rect(bx, 0.0, bx + bw, height_dips), &fg_b);
                        }
                    }
                }
                WidgetChipSpec::Bluetooth { devices } => {
                    draw_card(border_rgb)?;
                    if devices.is_empty() {
                        let sx = ((width_dips - 30.0) * 0.5).max(8.0);
                        draw_glyph("\u{E702}", rect(sx, cy - 7.5, sx + 14.0, cy + 7.5), 13.5, pal.text_secondary, 0.72)?;
                        draw_text("—", &fmt_11_b_l, rect(sx + 16.0, 0.0, width_dips - 6.0, height_dips), &s_brush);
                    } else {
                        let tile_w = (width_dips - 12.0) / devices.len().min(3) as f32;
                        let em = if is_dark { (0x34, 0xd3, 0x99) } else { (0x05, 0x96, 0x69) };
                        let bar_y = (height_dips - 6.0).max(height_dips * 0.78);
                        for (i, (kind, pct, low, charging)) in devices.iter().take(3).enumerate() {
                            let tx = 6.0 + i as f32 * tile_w;
                            if i > 0 {
                                draw_vdiv(tx - 1.5, 18.0);
                            }
                            let bar_col = if *low { pal.warning } else if *charging { em } else { pal.accent };
                            draw_glyph(
                                kind.fluent_glyph(),
                                rect(tx + 3.0, cy - 9.0, tx + 18.0, cy + 6.0),
                                13.5,
                                if *low { pal.warning } else { pal.text_secondary },
                                0.94,
                            )?;
                            let val_col = if *low { pal.warning } else if *charging { em } else { pal.text_primary };
                            let vb = dc.CreateSolidColorBrush(&color_rgba(val_col.0, val_col.1, val_col.2, 0.98), None)?;
                            let pct_str = crate::widgets::format_percent(*pct);
                            let pct_r = if *charging && tile_w >= 58.0 { tx + tile_w - 13.0 } else { tx + tile_w - 3.0 };
                            draw_text(&pct_str, &fmt_11_b_l, rect(tx + 20.0, 0.0, pct_r, bar_y), &vb);
                            if *charging && tile_w >= 58.0 {
                                draw_glyph("\u{E945}", rect(tx + tile_w - 13.0, cy - 7.5, tx + tile_w - 2.0, cy + 4.5), 9.5, em, 0.96)?;
                            }
                            draw_bar(
                                dc,
                                tx + 4.0,
                                bar_y,
                                (tile_w - 8.0).max(8.0),
                                2.0,
                                1.0,
                                *pct as f32 / 100.0,
                                1.5,
                                &tr_brush,
                                bar_col,
                                0.95,
                            )?;
                        }
                    }
                }
                WidgetChipSpec::Storage { drives } => {
                    draw_card(border_rgb)?;
                    if drives.is_empty() {
                        draw_text("—", &fmt_11_b_c, rect(10.0, 0.0, width_dips - 10.0, height_dips), &s_brush);
                    } else {
                        let count = drives.len().min(3);
                        let (pad_x, gap) = (8.0_f32, 8.0_f32);
                        let seg_w = ((width_dips - pad_x * 2.0 - gap * (count.saturating_sub(1) as f32)) / count as f32).max(40.0);
                        let bar_y = (height_dips - 6.0).max(height_dips * 0.78);
                        for (i, (letter, pair)) in drives.iter().take(3).enumerate() {
                            let sx = pad_x + i as f32 * (seg_w + gap);
                            if i > 0 {
                                draw_vdiv(sx - gap * 0.5, 18.0);
                            }
                            let used_ratio = crate::widgets::chip_storage_ratio(letter);
                            let bar_col = match used_ratio {
                                Some(r) if r >= 0.90 => pal.danger,
                                Some(r) if r >= 0.75 => pal.warning,
                                _ => pal.accent,
                            };
                            let percent_label = used_ratio.map_or_else(
                                || "—".to_owned(),
                                |r| crate::widgets::format_percent((r * 100.0).round() as u32),
                            );
                            draw_text(letter, &fmt_11_b_l, rect(sx, 3.0, sx + (seg_w * 0.55), 16.0), &p_brush);
                            draw_text(&percent_label, &fmt_11_b_r, rect(sx + (seg_w * 0.45), 3.0, sx + seg_w, 16.0), &s_brush);
                            draw_text(pair, &fmt_10_l, rect(sx, 15.0, sx + seg_w, bar_y - 1.0), &s_brush);
                            if let Some(ratio) = used_ratio {
                                draw_bar(dc, sx, bar_y, seg_w, 2.0, 1.0, ratio, 1.5, &tr_brush, bar_col, 0.95)?;
                            }
                        }
                    }
                }
                WidgetChipSpec::Media {
                    title,
                    artist,
                } => {
                    draw_card(border_rgb)?;
                    let hint = crate::widgets::chip_media_hint();
                    let art_sz = 28.0_f32.min(height_dips - 8.0).max(20.0);
                    let art_y = cy - art_sz * 0.5;
                    let art_rect = rect(7.0, art_y, 7.0 + art_sz, art_y + art_sz);
                    let art_rr = D2D1_ROUNDED_RECT { rect: art_rect, radiusX: 7.0, radiusY: 7.0 };
                    let decoded_cover = hint.cover_pixels.clone().or_else(|| {
                        hint.cover_path.as_ref().and_then(|path| {
                            crate::widgets::widget_icons::load_bgra(std::path::Path::new(path))
                        })
                    });
                    let mut drew_cover = false;
                    if let Some(decoded) = decoded_cover {
                        let (bw, bh, pixels) = &*decoded;
                        let props = D2D1_BITMAP_PROPERTIES1 {
                            pixelFormat: D2D1_PIXEL_FORMAT {
                                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                            },
                            dpiX: 96.0,
                            dpiY: 96.0,
                            ..Default::default()
                        };
                        if let Ok(bitmap) = dc.CreateBitmap(
                            D2D_SIZE_U { width: *bw, height: *bh },
                            Some(pixels.as_ptr().cast()),
                            *bw * 4,
                            &props,
                        ) {
                            let mask: windows::Win32::Graphics::Direct2D::ID2D1Geometry = self
                                .d2d_factory
                                .CreateRoundedRectangleGeometry(&art_rr)?
                                .into();
                            let lp = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS1 {
                                contentBounds: art_rect,
                                geometricMask: std::mem::ManuallyDrop::new(Some(mask)),
                                maskAntialiasMode: windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                                maskTransform: Matrix3x2::identity(),
                                opacity: 1.0,
                                opacityBrush: std::mem::ManuallyDrop::new(None),
                                layerOptions: windows::Win32::Graphics::Direct2D::D2D1_LAYER_OPTIONS1_NONE,
                            };
                            dc.PushLayer(&lp, None);
                            dc.DrawBitmap(&bitmap, Some(&art_rect), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
                            dc.PopLayer();
                            let _ = std::mem::ManuallyDrop::into_inner(lp.geometricMask);
                            drew_cover = true;
                        }
                    }
                    if !drew_cover {
                        let (g0, g1) = if hint.has_media && hint.playing {
                            (
                                color_rgba(pal.accent.0, pal.accent.1, pal.accent.2, 0.85),
                                color_rgba(pal.widget_blue.0, pal.widget_blue.1, pal.widget_blue.2, 0.90),
                            )
                        } else if is_dark {
                            (color_rgba(0x47, 0x55, 0x69, 0.72), color_rgba(0x33, 0x41, 0x55, 0.85))
                        } else {
                            (color_rgba(0x94, 0xa3, 0xb8, 0.55), color_rgba(0x64, 0x74, 0x8b, 0.70))
                        };
                        let grad = linear_grad_brush(
                            dc,
                            (art_rect.left, art_rect.top),
                            (art_rect.right, art_rect.bottom),
                            &[(0.0, g0), (1.0, g1)],
                        )?;
                        dc.FillRoundedRectangle(&art_rr, &grad);
                    }

                    let info_l = 7.0 + art_sz + 8.0;
                    let bh = 26.0_f32.min(height_dips - 8.0).max(20.0);
                    let by = cy - bh * 0.5;
                    if !hint.has_media {
                        let play_w = 26.0_f32;
                        let play_x = (width_dips - play_w - 7.0).max(info_l + 24.0);
                        let avail_w = (play_x - 6.0 - info_l).max(24.0);
                        let idle_fmt = if measure_text_w(title, &fmt_11_l) <= avail_w {
                            &fmt_11_l
                        } else if measure_text_w(title, &fmt_10_l) <= avail_w {
                            &fmt_10_l
                        } else {
                            &fmt_9_l
                        };
                        draw_text(title, idle_fmt, rect(info_l, 0.0, play_x - 6.0, height_dips), &s_brush);
                        let play_rect = rect(play_x, by, play_x + play_w, by + bh);
                        let play_rr = D2D1_ROUNDED_RECT { rect: play_rect, radiusX: 7.0, radiusY: 7.0 };
                        let play_hov = matches!(active_sub, Some(2 | 3));
                        if play_hov {
                            let hb = dc.CreateSolidColorBrush(
                                &if is_dark { color_rgba(0xff, 0xff, 0xff, 0.16) } else { color_rgba(0x00, 0x00, 0x00, 0.11) },
                                None,
                            )?;
                            dc.FillRoundedRectangle(&play_rr, &hb);
                        } else {
                            dc.FillRoundedRectangle(&play_rr, &surf_sel_b);
                        }
                        draw_glyph(
                            "\u{E768}",
                            play_rect,
                            11.5,
                            if play_hov { pal.text_primary } else { pal.text_secondary },
                            0.90,
                        )?;
                    } else {
                        let controls_l = (width_dips - 88.0).max(info_l + 24.0);
                        let mid = height_dips * 0.50;
                        if artist.is_empty() {
                            draw_text(title, &fmt_11_b_l, rect(info_l, 0.0, controls_l - 4.0, height_dips), &p_brush);
                        } else {
                            draw_text(title, &fmt_11_b_l, rect(info_l, (mid - 13.5).max(2.0), controls_l - 4.0, mid + 1.0), &p_brush);
                            draw_text(artist, &fmt_10_l, rect(info_l, mid - 0.5, controls_l - 4.0, (mid + 12.5).min(height_dips - 4.0)), &s_brush);
                        }

                        // 3 sub-buttons: 0=Prev, 1=Play/Pause (dynamic glyph), 2=Next
                        let play_glyph = if hint.playing { "\u{E769}" } else { "\u{E768}" };
                        let btns: [(f32, f32, &str, bool); 3] = [
                            (controls_l, 25.0, "\u{E892}", false),
                            (controls_l + 27.0, 27.0, play_glyph, true),
                            (controls_l + 56.0, 25.0, "\u{E893}", false),
                        ];
                        for (i, (bx, bw, glyph, has_base_bg)) in btns.iter().enumerate() {
                            let brc = rect(*bx, by, bx + bw, by + bh);
                            let rr = D2D1_ROUNDED_RECT { rect: brc, radiusX: 7.0, radiusY: 7.0 };
                            if i == 1 && hint.playing {
                                dc.FillRoundedRectangle(&rr, &sel_fill_b);
                                dc.DrawRoundedRectangle(&rr, &sel_stroke_b, 1.0, None);
                            } else if active_sub == Some(i as u8) {
                                let hb = dc.CreateSolidColorBrush(
                                    &if is_dark { color_rgba(0xff, 0xff, 0xff, 0.16) } else { color_rgba(0x00, 0x00, 0x00, 0.11) },
                                    None,
                                )?;
                                dc.FillRoundedRectangle(&rr, &hb);
                            } else if *has_base_bg {
                                dc.FillRoundedRectangle(&rr, &surf_sel_b);
                            }
                            let glyph_col = if i == 1 && hint.playing {
                                pal.selected_fg
                            } else {
                                pal.text_primary
                            };
                            draw_glyph(glyph, brc, if i == 1 { 12.0 } else { 10.5 }, glyph_col, 0.95)?;
                        }
                        if hint.progress > 0.005 {
                            draw_chip_ratio(hint.progress, pal.accent)?;
                        }
                    }
                }
                WidgetChipSpec::Audio { volume, muted } => {
                    draw_card(border_rgb)?;
                    let silent = *muted || *volume == 0;
                    let glyph = if silent { "\u{E74F}" } else if *volume < 50 { "\u{E993}" } else { "\u{E995}" };
                    let icon_col = if *muted { pal.danger } else if *volume == 0 { pal.text_secondary } else { pal.text_primary };
                    let pad_l = if width_dips < 74.0 { 7.0 } else { 10.0 };
                    draw_glyph(glyph, rect(pad_l, cy - 9.0, pad_l + 16.0, cy + 7.0), 15.0, icon_col, 0.96)?;
                    let txt = crate::widgets::format_percent(*volume);
                    draw_text(
                        &txt,
                        if width_dips < 74.0 { &fmt_11_b_l } else { &fmt_12_b_l },
                        rect(pad_l + 20.0, 0.0, width_dips - 6.0, height_dips - 3.0),
                        if silent { &s_brush } else { &p_brush },
                    );
                    draw_chip_ratio(*volume as f32 / 100.0, if *muted { pal.text_secondary } else { pal.accent })?;
                }
                WidgetChipSpec::QuickSettings { pills } => {
                    draw_card(border_rgb)?;
                    // 3 circular glass toggle pills + 4th compact Control Center chevron pill.
                    let (gap, panel_w) = (4.0_f32, 22.0_f32);
                    let max_sz = ((width_dips - 14.0 - panel_w - gap * 3.0) / 3.0).max(20.0);
                    let sz = 28.0_f32.min(height_dips - 8.0).min(max_sz);
                    let r = sz * 0.5;
                    let start_x = ((width_dips - (sz * 3.0 + gap * 3.0 + panel_w)) * 0.5).max(6.0);
                    for (i, (_glyph, active)) in pills.iter().enumerate() {
                        let cx_btn = start_x + i as f32 * (sz + gap) + r;
                        let ell = D2D1_ELLIPSE { point: Vector2 { X: cx_btn, Y: cy }, radiusX: r, radiusY: r };
                        let hov = active_sub == Some(i as u8);
                        let is_night = i == 2;
                        if *active {
                            let fill = if is_night {
                                color_rgba(pal.warning.0, pal.warning.1, pal.warning.2, if is_dark { 0.18 } else { 0.15 })
                            } else if is_dark {
                                color_rgba(0xff, 0xff, 0xff, if hov { 0.20 } else { 0.14 })
                            } else {
                                color_rgba(0x18, 0x18, 0x1b, if hov { 0.15 } else { 0.10 })
                            };
                            let fb = dc.CreateSolidColorBrush(&fill, None)?;
                            dc.FillEllipse(&ell, &fb);
                        } else if hov {
                            let fill = if is_dark { color_rgba(0xff, 0xff, 0xff, 0.08) } else { color_rgba(0x00, 0x00, 0x00, 0.06) };
                            let hb = dc.CreateSolidColorBrush(&fill, None)?;
                            dc.FillEllipse(&ell, &hb);
                        }

                        let icon_color = if *active {
                            if is_night {
                                if is_dark { color_rgba(0xfc, 0xd3, 0x4d, 0.98) } else { color_rgba(0xb4, 0x53, 0x09, 0.98) }
                            } else if is_dark {
                                color_rgba(0xff, 0xff, 0xff, 0.98)
                            } else {
                                color_rgba(0x18, 0x18, 0x1b, 0.98)
                            }
                        } else if is_dark {
                            color_rgba(pal.text_primary.0, pal.text_primary.1, pal.text_primary.2, if hov { 0.78 } else { 0.36 })
                        } else {
                            color_rgba(0x18, 0x18, 0x1b, if hov { 0.78 } else { 0.36 })
                        };
                        let ib = dc.CreateSolidColorBrush(&icon_color, None)?;

                        match i {
                            0 => {
                                let oy = cy + 4.8;
                                let (a0, a1) = (-std::f32::consts::FRAC_PI_4 * 3.0, -std::f32::consts::FRAC_PI_4);
                                for rad in [3.5_f32, 6.5_f32, 9.5_f32] {
                                    let path = self.d2d_factory.CreatePathGeometry()?;
                                    let sink = path.Open()?;
                                    sink.BeginFigure(Vector2 { X: cx_btn + a0.cos() * rad, Y: oy + a0.sin() * rad }, D2D1_FIGURE_BEGIN_HOLLOW);
                                    sink.AddArc(&D2D1_ARC_SEGMENT {
                                        point: Vector2 { X: cx_btn + a1.cos() * rad, Y: oy + a1.sin() * rad },
                                        size: D2D_SIZE_F { width: rad, height: rad },
                                        rotationAngle: 0.0,
                                        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                                        arcSize: D2D1_ARC_SIZE_SMALL,
                                    });
                                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                                    sink.Close()?;
                                    dc.DrawGeometry(&path, &ib, 1.55, Some(&stroke));
                                }
                                dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx_btn, Y: oy }, radiusX: 1.05, radiusY: 1.05 }, &ib);
                            }
                            1 => {
                                let pt = |x: f32, y: f32| Vector2 { X: cx_btn + (x - 12.0) * 0.58, Y: cy + (y - 12.0) * 0.58 };
                                let path = self.d2d_factory.CreatePathGeometry()?;
                                let sink = path.Open()?;
                                sink.BeginFigure(pt(7.0, 7.0), D2D1_FIGURE_BEGIN_HOLLOW);
                                for (px, py) in [(17.0, 17.0), (12.0, 22.0), (12.0, 2.0), (17.0, 7.0), (7.0, 17.0)] {
                                    sink.AddLine(pt(px, py));
                                }
                                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                                sink.Close()?;
                                dc.DrawGeometry(&path, &ib, 1.55, Some(&stroke));
                            }
                            _ => {
                                let sc = 0.60_f32;
                                let pt = |x: f32, y: f32| Vector2 { X: cx_btn + (x - 12.0) * sc, Y: cy + (y - 12.0) * sc };
                                let rad = 8.5 * sc;
                                let path = self.d2d_factory.CreatePathGeometry()?;
                                let sink = path.Open()?;
                                sink.BeginFigure(
                                    pt(20.2, 14.4),
                                    if *active {
                                        windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_FILLED
                                    } else {
                                        D2D1_FIGURE_BEGIN_HOLLOW
                                    },
                                );
                                sink.AddArc(&D2D1_ARC_SEGMENT {
                                    point: pt(9.6, 3.8),
                                    size: D2D_SIZE_F { width: rad, height: rad },
                                    rotationAngle: 0.0,
                                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                                    arcSize: D2D1_ARC_SIZE_SMALL,
                                });
                                sink.AddArc(&D2D1_ARC_SEGMENT {
                                    point: pt(20.2, 14.4),
                                    size: D2D_SIZE_F { width: rad, height: rad },
                                    rotationAngle: 0.0,
                                    sweepDirection: windows::Win32::Graphics::Direct2D::D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
                                    arcSize: D2D1_ARC_SIZE_LARGE,
                                });
                                sink.EndFigure(windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_CLOSED);
                                sink.Close()?;
                                if *active {
                                    dc.FillGeometry(&path, &ib, None);
                                } else {
                                    dc.DrawGeometry(&path, &ib, 1.55, Some(&stroke));
                                }
                            }
                        }
                    }
                    let panel_l = start_x + 3.0 * (sz + gap);
                    let panel_rect = rect(panel_l, cy - sz * 0.5, (panel_l + panel_w).min(width_dips - 5.0), cy + sz * 0.5);
                    let panel_hov = active_sub == Some(3);
                    let panel_rr = D2D1_ROUNDED_RECT { rect: panel_rect, radiusX: 7.0, radiusY: 7.0 };
                    if panel_hov {
                        let hb = dc.CreateSolidColorBrush(
                            &if is_dark { color_rgba(0xff, 0xff, 0xff, 0.14) } else { color_rgba(0x00, 0x00, 0x00, 0.09) },
                            None,
                        )?;
                        dc.FillRoundedRectangle(&panel_rr, &hb);
                    } else {
                        dc.FillRoundedRectangle(&panel_rr, &surf_sel_b);
                    }
                    draw_glyph(
                        "\u{E70E}",
                        panel_rect,
                        9.5,
                        if panel_hov { pal.text_primary } else { pal.text_secondary },
                        0.92,
                    )?;
                }
                WidgetChipSpec::Keyboard { code } => {
                    draw_card(border_rgb)?;
                    draw_text(code, &fmt_11_b_c, rect(0.0, 0.0, width_dips, height_dips), &p_brush);
                }
                WidgetChipSpec::AiDual {
                    provider_id,
                    brand_color,
                    rows,
                } => {
                    draw_card(border_rgb)?;
                    draw_brand(provider_id, rect(10.0, cy - 8.0, 26.0, cy + 8.0), brand_color.unwrap_or(pal.text_primary))?;
                    draw_vdiv(32.0, 18.0);
                    let rx = 38.0;
                    let avail = (width_dips - rx - 8.0).max(40.0);
                    let lbl_w = 21.0_f32;
                    let reset_w = (avail * 0.40).clamp(28.0, 56.0);
                    let bw = (avail - lbl_w - reset_w - 8.0).clamp(18.0, 44.0);
                    let muted_tr = dc.CreateSolidColorBrush(
                        &if is_dark {
                            color_rgba(0x7c, 0x86, 0x98, 0.35)
                        } else {
                            color_rgba(pal.text_secondary.0, pal.text_secondary.1, pal.text_secondary.2, 0.28)
                        },
                        None,
                    )?;
                    let n = rows.len().max(1);
                    let (row_h, row_gap) = (13.0_f32, 2.0_f32);
                    let start_y = cy - (n as f32 * row_h + (n.saturating_sub(1) as f32) * row_gap) * 0.5;
                    for (idx, (lbl, used, bar_col, reset_txt)) in rows.iter().enumerate() {
                        let y1 = start_y + idx as f32 * (row_h + row_gap);
                        let y2 = y1 + row_h;
                        draw_text(lbl, &fmt_10_b_l, rect(rx, y1, rx + lbl_w, y2), &s_brush);
                        let bl = rx + lbl_w + 3.0;
                        draw_bar(dc, bl, (y1 + y2) * 0.5 - 2.0, bw, 4.0, 2.0, *used, 1.0, &muted_tr, *bar_col, 0.98)?;
                        draw_text(reset_txt, &fmt_10_l, rect(bl + bw + 5.0, y1, width_dips - 7.0, y2), &p_brush);
                    }
                }
                WidgetChipSpec::AiOpenCode { columns } => {
                    draw_card(border_rgb)?;
                    let brand_col = crate::widgets::ai_provider_color_for_theme("aiusage_opencode", is_dark);
                    draw_brand("aiusage_opencode", rect(10.0, cy - 8.0, 26.0, cy + 8.0), brand_col)?;
                    draw_vdiv(32.0, 18.0);
                    let (start_x, col_gap) = (39.0, 7.0);
                    let col_w = (width_dips - start_x - 8.0 - col_gap * 2.0).max(90.0) / 3.0;
                    let top_y = cy - 28.0_f32.min(height_dips - 6.0) * 0.5;
                    for (ci, (lbl, pct_txt, used, bar_col, reset_txt)) in columns.iter().take(3).enumerate() {
                        let cx_l = start_x + ci as f32 * (col_w + col_gap);
                        draw_text(lbl, &fmt_10_l, rect(cx_l, top_y, cx_l + col_w * 0.48, top_y + 12.0), &s_brush);
                        draw_text(pct_txt, &fmt_11_b_r, rect(cx_l + col_w * 0.35, top_y, cx_l + col_w, top_y + 12.0), &p_brush);
                        let by = top_y + 13.5;
                        draw_bar(dc, cx_l, by, col_w, 3.0, 1.5, *used, 1.0, &tr_brush, *bar_col, 0.98)?;
                        draw_text(reset_txt, &fmt_9_l, rect(cx_l, by + 4.0, cx_l + col_w, (by + 15.0).min(height_dips - 2.0)), &s_brush);
                    }
                }
                WidgetChipSpec::AiCopilot {
                    value_text,
                    meta_text,
                    used,
                    bar_color,
                } => {
                    draw_card(border_rgb)?;
                    let brand_col = crate::widgets::ai_provider_color_for_theme("aiusage_copilot", is_dark);
                    draw_brand("aiusage_copilot", rect(10.0, cy - 8.0, 26.0, cy + 8.0), brand_col)?;
                    draw_vdiv(32.0, 18.0);
                    let tx = 39.0;
                    if value_text.is_empty() || value_text == "—" {
                        let status = if meta_text.is_empty() { value_text.as_str() } else { meta_text.as_str() };
                        let avail_w = (width_dips - tx - 8.0).max(24.0);
                        let status_fmt = if measure_text_w(status, &fmt_11_l) <= avail_w {
                            &fmt_11_l
                        } else if measure_text_w(status, &fmt_10_l) <= avail_w {
                            &fmt_10_l
                        } else {
                            &fmt_9_l
                        };
                        draw_text(status, status_fmt, rect(tx, 0.0, width_dips - 8.0, height_dips), &s_brush);
                    } else {
                        let bw = (width_dips * 0.24).clamp(24.0, 36.0);
                        let bl = (width_dips - bw - 8.0).max(tx + 34.0);
                        let mid = height_dips * 0.50;
                        draw_text(value_text, &fmt_12_b_l, rect(tx, (mid - 13.5).max(2.0), bl - 5.0, mid + 1.0), &p_brush);
                        draw_text(meta_text, &fmt_10_l, rect(tx, mid - 0.5, bl - 5.0, (mid + 12.5).min(height_dips - 2.0)), &s_brush);
                        draw_bar(dc, bl, cy - 2.0, bw, 4.0, 2.0, *used, 1.0, &tr_brush, *bar_color, 0.98)?;
                    }
                }
                WidgetChipSpec::AiBalance {
                    provider_id,
                    name,
                    brand_color,
                    value_text,
                } => {
                    draw_card(border_rgb)?;
                    draw_brand(provider_id, rect(10.0, cy - 8.0, 26.0, cy + 8.0), *brand_color)?;
                    draw_vdiv(32.0, 18.0);
                    let tx = 39.0;
                    let mid = height_dips * 0.49;
                    draw_text(name, &fmt_10_b_l, rect(tx, (mid - 13.0).max(2.0), width_dips - 8.0, mid), &s_brush);
                    draw_text(value_text, &fmt_12_b_l, rect(tx, mid - 0.5, width_dips - 8.0, (mid + 14.0).min(height_dips - 4.0)), &p_brush);
                    if *provider_id == "aiusage_openrouter"
                        && let Some(credit_ratio) = crate::widgets::chip_openrouter_ratio()
                    {
                        draw_chip_ratio(credit_ratio, *brand_color)?;
                    }
                }
                WidgetChipSpec::Simple {
                    icon,
                    icon_color,
                    top,
                    bottom,
                } => draw_two_line_chip(TwoLine {
                    icon,
                    color: *icon_color,
                    primary: top.as_str(),
                    secondary: bottom.as_str(),
                    ratio: None,
                    primary_format: &fmt_11_b_l,
                    secondary_format: &fmt_11_l,
                })?,
                WidgetChipSpec::Custom {
                    label,
                    value,
                    ratio,
                    icon,
                    color,
                    ..
                } => draw_two_line_chip(TwoLine {
                    icon,
                    color: *color,
                    primary: value.as_str(),
                    secondary: label.as_str(),
                    ratio: *ratio,
                    primary_format: &fmt_11_b_l,
                    secondary_format: &fmt_11_l,
                })?,
            }
            Ok(())
        })
    }

    pub fn create_widget_text_surface(
        &self,
        primary: &str,
        secondary: Option<&str>,
        width_dips: f32,
        height_dips: f32,
        dpi: f32,
        is_dark: bool,
    ) -> Result<IDCompositionSurface> {
        let w_px = (width_dips * dpi).ceil().max(1.0) as u32;
        let h_px = (height_dips * dpi).ceil().max(1.0) as u32;
        let primary_w: Vec<u16> = primary.encode_utf16().collect();
        let secondary_w: Option<Vec<u16>> = secondary.map(|s| s.encode_utf16().collect());
        let (fg_primary, fg_secondary) = if is_dark {
            (
                color_rgba(0xf1, 0xf5, 0xf9, 0.96),
                color_rgba(0x94, 0xa3, 0xb8, 0.90),
            )
        } else {
            (
                color_rgba(0x0f, 0x17, 0x2a, 0.96),
                color_rgba(0x47, 0x55, 0x69, 0.90),
            )
        };
        let sec_fmt = unsafe {
            let fmt = self.dwrite_factory.CreateTextFormat(
                &HSTRING::from(design::fonts::dwrite_ui_family().as_str()),
                None,
                DWRITE_FONT_WEIGHT_MEDIUM,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                design::fonts::scaled_size(10.5),
                &HSTRING::from(design::fonts::ui_locale().as_str()),
            )?;
            fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            fmt
        };

        self.draw_surface(w_px, h_px, |dc, _w, _h| unsafe {
            let p_brush = dc.CreateSolidColorBrush(&fg_primary, None)?;
            let s_brush = dc.CreateSolidColorBrush(&fg_secondary, None)?;
            let mut prev = Matrix3x2::identity();
            dc.GetTransform(&mut prev);
            dc.SetTransform(&Matrix3x2 {
                M11: dpi,
                M12: 0.0,
                M21: 0.0,
                M22: dpi,
                M31: prev.M31,
                M32: prev.M32,
            });
            if let Some(ref sw) = secondary_w {
                let mid = height_dips * 0.52;
                dc.DrawText(
                    &primary_w,
                    &self.tooltip_format,
                    &D2D_RECT_F {
                        left: 0.0,
                        top: 2.0,
                        right: width_dips,
                        bottom: mid + 2.0,
                    },
                    &p_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                );
                dc.DrawText(
                    sw,
                    &sec_fmt,
                    &D2D_RECT_F {
                        left: 0.0,
                        top: mid - 1.0,
                        right: width_dips,
                        bottom: height_dips - 2.0,
                    },
                    &s_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                );
            } else {
                dc.DrawText(
                    &primary_w,
                    &self.tooltip_format,
                    &D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: width_dips,
                        bottom: height_dips,
                    },
                    &p_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            Ok(())
        })
    }

    /// Notification badge: red circular pill with a centered count (`TrackedWindow.HasNotification`).
    pub fn create_notification_badge_surface(
        &self,
        count: u32,
        is_dark: bool,
        dpi: f32,
    ) -> Result<IDCompositionSurface> {
        let label: std::borrow::Cow<'_, str> = if count > 99 {
            std::borrow::Cow::Borrowed("99+")
        } else {
            std::borrow::Cow::Owned(count.to_string())
        };
        let size_dips = if label.len() > 1 { 22.0 } else { 18.0 };
        let px = (size_dips * dpi).ceil().max(1.0) as u32;
        self.draw_surface(px, px, |dc, w, h| unsafe {
            let bg = dc.CreateSolidColorBrush(&color_rgba(0xe8, 0x11, 0x23, 1.0), None)?;
            dc.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 {
                        X: w * 0.5,
                        Y: h * 0.5,
                    },
                    radiusX: (w * 0.5 - dpi).max(1.0),
                    radiusY: (h * 0.5 - dpi).max(1.0),
                },
                &bg,
            );
            let fmt = self.dwrite_factory.CreateTextFormat(
                &HSTRING::from(design::fonts::dwrite_ui_family()),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                10.0 * dpi,
                &HSTRING::from(design::fonts::ui_locale()),
            )?;
            fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            let fg = dc.CreateSolidColorBrush(&color_rgba(0xff, 0xff, 0xff, 1.0), None)?;
            let text: windows::core::HSTRING = HSTRING::from(label.as_ref());
            dc.DrawText(
                &text,
                &fmt,
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: h,
                },
                &fg,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
            );
            let _ = is_dark;
            Ok(())
        })
    }

    fn draw_surface(
        &self,
        width: u32,
        height: u32,
        draw_fn: impl FnOnce(&ID2D1DeviceContext, f32, f32) -> Result<()>,
    ) -> Result<IDCompositionSurface> {
        let w = width.max(1);
        let h = height.max(1);
        let surface = unsafe {
            self.dcomp_device.CreateSurface(
                w,
                h,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_PREMULTIPLIED,
            )?
        };
        let mut offset = POINT::default();
        let dc: ID2D1DeviceContext = unsafe { surface.BeginDraw(None, &mut offset)? };
        unsafe {
            dc.SetDpi(96.0, 96.0);
            dc.SetTextAntialiasMode(
                windows::Win32::Graphics::Direct2D::D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
            );
            dc.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
            dc.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));
        }
        let draw_res = draw_fn(&dc, w as f32, h as f32);
        let end_res = unsafe { surface.EndDraw() };
        if let Err(ref err) = end_res {
            opentaskbar_core::log::error(format!(
                "DirectComposition EndDraw failed ({w}x{h}): {err}"
            ));
        }
        draw_res?;
        end_res?;
        Ok(surface)
    }

    /// Creates a 3-piece horizontal pill (left cap, 1px stretchable center, right cap) so
    /// widening/narrowing the dock pill during fisheye never redraws any GPU texture.
    /// Includes a subtle vertical depth gradient and a top-edge specular inner highlight.
    /// `flat` paints `SurfaceBackground::TrueBlack`: opaque black with no gradient sheen,
    /// no rim highlight and no border. Existing glass/solid callers pass `false`.
    #[allow(clippy::too_many_arguments)]
    pub fn create_pill_nodes(
        &self,
        radius_px: f32,
        pill_h_px: f32,
        bg: (u8, u8, u8, f32),
        border: (u8, u8, u8, f32),
        dpi: f32,
        border_width_dips: f32,
        flat: bool,
    ) -> Result<PillNodes> {
        let h = pill_h_px.ceil().max(4.0) as u32;
        let r = radius_px.clamp(2.0, pill_h_px * 0.5);
        let cap_w = (r.ceil() as u32).max(2);
        let bw = dpi.max(1.0);
        let stroke_w = (border_width_dips * dpi).max(0.0);
        let half_bw = bw * 0.5;
        let is_dark = (u16::from(bg.0) + u16::from(bg.1) + u16::from(bg.2)) < 384;
        let top_bg = if flat {
            color_rgba(bg.0, bg.1, bg.2, bg.3)
        } else {
            color_rgba(
                bg.0.saturating_add(if is_dark { 10 } else { 6 }),
                bg.1.saturating_add(if is_dark { 10 } else { 6 }),
                bg.2.saturating_add(if is_dark { 12 } else { 6 }),
                bg.3,
            )
        };
        let bot_bg = color_rgba(bg.0, bg.1, bg.2, bg.3);
        let bd_col = color_rgba(border.0, border.1, border.2, border.3);
        let spec_alpha = if flat {
            0.0_f32
        } else if is_dark {
            0.14_f32
        } else {
            0.72_f32
        };
        let spec_col = color_rgba(0xff, 0xff, 0xff, spec_alpha);

        let make_spec_grad = |dc: &ID2D1DeviceContext, fh: f32| unsafe {
            linear_grad_brush(
                dc,
                (0.0, half_bw + bw),
                (0.0, (fh * 0.62).max(half_bw + bw + 1.0)),
                &[
                    (0.0, color_rgba(0xff, 0xff, 0xff, spec_alpha)),
                    (0.55, color_rgba(0xff, 0xff, 0xff, spec_alpha * 0.36)),
                    (1.0, color_rgba(0xff, 0xff, 0xff, 0.0)),
                ],
            )
        };
        let make_grad = |dc: &ID2D1DeviceContext, fh: f32| unsafe {
            linear_grad_brush(dc, (0.0, 0.0), (0.0, fh), &[(0.0, top_bg), (1.0, bot_bg)])
        };
        let draw_cap = |is_left: bool| {
            self.draw_surface(cap_w, h, |dc, w, fh| unsafe {
                let bg_brush = make_grad(dc, fh)?;
                let bd_brush = dc.CreateSolidColorBrush(&bd_col, None)?;
                let spec_brush = make_spec_grad(dc, fh)?;
                let (ol, or, il, ir) = if is_left {
                    (half_bw, w + r * 2.0, half_bw + bw, w + r * 2.0)
                } else {
                    (-(r * 2.0), w - half_bw, -(r * 2.0), w - half_bw - bw)
                };
                let rr = rrect(ol, half_bw, or, fh - half_bw, r);
                let inner_rr = rrect(il, half_bw + bw, ir, fh - half_bw - bw, (r - bw).max(1.0));
                dc.PushAxisAlignedClip(
                    &rect(0.0, 0.0, w, fh),
                    windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                );
                dc.FillRoundedRectangle(&rr, &bg_brush);
                if spec_alpha > 0.0 {
                    dc.DrawRoundedRectangle(&inner_rr, &spec_brush, bw, None);
                }
                if border.3 > 0.0 && stroke_w > 0.0 {
                    dc.DrawRoundedRectangle(&rr, &bd_brush, stroke_w, None);
                }
                dc.PopAxisAlignedClip();
                Ok(())
            })
        };

        let left_surf = draw_cap(true)?;
        let center_surf = self.draw_surface(1, h, |dc, _w, fh| unsafe {
            let bg_brush = make_grad(dc, fh)?;
            let bd_brush = dc.CreateSolidColorBrush(&bd_col, None)?;
            let spec_brush = dc.CreateSolidColorBrush(&spec_col, None)?;
            dc.FillRectangle(&rect(0.0, half_bw, 1.0, fh - half_bw), &bg_brush);
            if spec_alpha > 0.0 {
                dc.DrawLine(
                    Vector2 {
                        X: 0.0,
                        Y: half_bw + bw,
                    },
                    Vector2 {
                        X: 1.0,
                        Y: half_bw + bw,
                    },
                    &spec_brush,
                    bw,
                    None,
                );
            }
            if border.3 > 0.0 && stroke_w > 0.0 {
                for y in [half_bw, fh - half_bw] {
                    dc.DrawLine(
                        Vector2 { X: 0.0, Y: y },
                        Vector2 { X: 1.0, Y: y },
                        &bd_brush,
                        stroke_w,
                        None,
                    );
                }
            }
            Ok(())
        })?;
        let right_surf = draw_cap(false)?;

        let left = self.create_node()?;
        let center = self.create_node()?;
        let right = self.create_node()?;
        left.set_content(Some(&left_surf));
        center.set_content(Some(&center_surf));
        right.set_content(Some(&right_surf));

        Ok(PillNodes {
            left,
            center,
            right,
            cap_w_px: cap_w as f32,
        })
    }

    pub fn create_rounded_rect_surface(
        &self,
        width: u32,
        height: u32,
        radius: f32,
        rgba: (u8, u8, u8, f32),
    ) -> Result<IDCompositionSurface> {
        let col = color_rgba(rgba.0, rgba.1, rgba.2, rgba.3);
        self.draw_surface(width, height, |dc, w, h| unsafe {
            let brush = dc.CreateSolidColorBrush(&col, None)?;
            let rx = radius.min(w * 0.5);
            let ry = radius.min(h * 0.5);
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: h,
                },
                radiusX: rx,
                radiusY: ry,
            };
            dc.FillRoundedRectangle(&rr, &brush);
            Ok(())
        })
    }

    pub fn create_shell_glyph_surface(
        &self,
        kind: ShellGlyph,
        size_px: u32,
        is_dark: bool,
    ) -> Result<IDCompositionSurface> {
        let pal = design::active_palette(is_dark);

        self.draw_surface(size_px, size_px, |dc, w, h| unsafe {
            if kind == ShellGlyph::Start {
                // Crisp Windows 11 4-pane logo with diagonal sky-to-azure gradient.
                let s = w / 24.0;
                // A theme pack / editor `WindowsLogoBrush` repaints the logo in its colours.
                let stops = themed_windows_logo_stops().unwrap_or_else(|| {
                    if is_dark {
                        vec![
                            D2D1_GRADIENT_STOP {
                                position: 0.0,
                                color: color_rgba(0x60, 0xcd, 0xff, 1.0),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 1.0,
                                color: color_rgba(0x00, 0x78, 0xd4, 1.0),
                            },
                        ]
                    } else {
                        vec![
                            D2D1_GRADIENT_STOP {
                                position: 0.0,
                                color: color_rgba(0x00, 0x95, 0xf6, 1.0),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 1.0,
                                color: color_rgba(0x00, 0x5f, 0xb8, 1.0),
                            },
                        ]
                    }
                });
                let coll = dc.CreateGradientStopCollection(
                    &stops,
                    D2D1_COLOR_SPACE_SRGB,
                    D2D1_COLOR_SPACE_SRGB,
                    D2D1_BUFFER_PRECISION_8BPC_UNORM,
                    D2D1_EXTEND_MODE_CLAMP,
                    D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
                )?;
                let brush = dc.CreateLinearGradientBrush(
                    &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                        startPoint: Vector2 {
                            X: 2.5 * s,
                            Y: 2.5 * s,
                        },
                        endPoint: Vector2 {
                            X: 21.5 * s,
                            Y: 21.5 * s,
                        },
                    },
                    None,
                    &coll,
                )?;
                let rects = [(2.5, 2.5), (12.8, 2.5), (2.5, 12.8), (12.8, 12.8)];
                for (x, y) in rects {
                    let rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: x * s,
                            top: y * s,
                            right: (x + 8.7) * s,
                            bottom: (y + 8.7) * s,
                        },
                        radiusX: 1.6 * s,
                        radiusY: 1.6 * s,
                    };
                    dc.FillRoundedRectangle(&rr, &brush);
                }
                return Ok(());
            }

            let app_icon = match kind {
                ShellGlyph::Start => unreachable!(),
                ShellGlyph::Search => AppIcon::Search,
                ShellGlyph::TaskView => AppIcon::TaskView,
                ShellGlyph::TrayChevron => AppIcon::TrayChevron,
                ShellGlyph::RecycleBinEmpty => AppIcon::RecycleBinEmpty,
                ShellGlyph::RecycleBinFull => AppIcon::RecycleBinFull,
            };
            let color = if kind == ShellGlyph::RecycleBinFull {
                pal.accent
            } else {
                pal.text_primary
            };
            let glyph_size = (w * 0.78).max(12.0);
            draw_fluent_glyph_d2d(
                &self.dwrite_factory,
                dc,
                app_icon,
                D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: h,
                },
                glyph_size,
                color,
                0.96,
            )?;
            Ok(())
        })
    }

    pub fn create_app_icon_surface(&self, raw: &RawIcon) -> Result<IDCompositionSurface> {
        // Convert straight BGRA32 to premultiplied BGRA32 once at upload time.
        let mut premul = raw.pixels.to_vec();
        for px in premul.as_chunks_mut::<4>().0 {
            let a = u16::from(px[3]);
            if a == 0 {
                px[0] = 0;
                px[1] = 0;
                px[2] = 0;
            } else if a < 255 {
                px[0] = ((u16::from(px[0]) * a + 127) / 255) as u8;
                px[1] = ((u16::from(px[1]) * a + 127) / 255) as u8;
                px[2] = ((u16::from(px[2]) * a + 127) / 255) as u8;
            }
        }

        let w = raw.width.max(1);
        let h = raw.height.max(1);
        self.draw_surface(w, h, |dc, fw, fh| unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let bitmap = dc.CreateBitmap(
                D2D_SIZE_U {
                    width: w,
                    height: h,
                },
                Some(premul.as_ptr().cast()),
                w * 4,
                &props,
            )?;
            dc.DrawBitmap(
                &bitmap,
                Some(&D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: fw,
                    bottom: fh,
                }),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
            Ok(())
        })
    }

    pub fn create_fallback_tile_surface(
        &self,
        letter: &str,
        size_px: u32,
    ) -> Result<IDCompositionSurface> {
        let wide: Vec<u16> = letter.encode_utf16().collect();
        self.draw_surface(size_px, size_px, |dc, w, h| unsafe {
            let bg = dc.CreateSolidColorBrush(&color_rgba(0x33, 0x41, 0x55, 0.85), None)?;
            let bd = dc.CreateSolidColorBrush(&color_rgba(0xff, 0xff, 0xff, 0.18), None)?;
            let fg = dc.CreateSolidColorBrush(&color_rgba(0xf8, 0xfa, 0xfc, 1.0), None)?;
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: 1.0,
                    top: 1.0,
                    right: w - 1.0,
                    bottom: h - 1.0,
                },
                radiusX: w * 0.22,
                radiusY: h * 0.22,
            };
            dc.FillRoundedRectangle(&rr, &bg);
            dc.DrawRoundedRectangle(&rr, &bd, 1.0, None);
            dc.DrawText(
                &wide,
                &self.fallback_format,
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: h,
                },
                &fg,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
            );
            Ok(())
        })
    }

    /// Creates a crisp DirectWrite tooltip pill surface and returns `(surface, width_dips, height_dips)`.
    pub fn create_tooltip_surface(
        &self,
        text: &str,
        dpi: f32,
        is_dark: bool,
        border_rgb: Rgb,
        border_width_dips: f32,
    ) -> Result<(IDCompositionSurface, f32, f32)> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe {
            self.dwrite_factory
                .CreateTextLayout(&wide, &self.tooltip_format, 418.0, 28.0)?
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe {
            layout.GetMetrics(&mut metrics)?;
        }
        let w_dips = (metrics.widthIncludingTrailingWhitespace + 22.0)
            .clamp(48.0, 440.0)
            .ceil();
        let h_dips = (metrics.height + 10.0).max(26.0).ceil();
        unsafe {
            layout.SetMaxWidth((w_dips - 22.0).max(16.0))?;
            layout.SetMaxHeight(h_dips)?;
        }
        let w_px = (w_dips * dpi).ceil() as u32;
        let h_px = (h_dips * dpi).ceil() as u32;

        let pal = design::palette_with_border(is_dark, Some(border_rgb));
        let (bg_c, bd_c, fg_c) = (
            color_rgba(pal.surface.0, pal.surface.1, pal.surface.2, 0.96),
            color_rgba(pal.border_rgb.0, pal.border_rgb.1, pal.border_rgb.2, 0.90),
            color_rgba(
                pal.text_primary.0,
                pal.text_primary.1,
                pal.text_primary.2,
                1.0,
            ),
        );

        let surface = self.draw_surface(w_px, h_px, |dc, w, h| unsafe {
            let bg = dc.CreateSolidColorBrush(&bg_c, None)?;
            let bd = dc.CreateSolidColorBrush(&bd_c, None)?;
            let fg = dc.CreateSolidColorBrush(&fg_c, None)?;
            let bw = (border_width_dips * dpi).max(0.0);
            let half_bw = bw * 0.5;
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: half_bw,
                    top: half_bw,
                    right: w - half_bw,
                    bottom: h - half_bw,
                },
                radiusX: 8.0 * dpi,
                radiusY: 8.0 * dpi,
            };
            dc.FillRoundedRectangle(&rr, &bg);
            dc.DrawRoundedRectangle(&rr, &bd, bw, None);

            // Scale DirectWrite DIPs into physical pixels while preserving the DComp atlas offset.
            let mut prev = Matrix3x2::identity();
            dc.GetTransform(&mut prev);
            dc.SetTransform(&Matrix3x2 {
                M11: dpi,
                M12: 0.0,
                M21: 0.0,
                M22: dpi,
                M31: prev.M31,
                M32: prev.M32,
            });
            dc.DrawTextLayout(
                Vector2 { X: 11.0, Y: 0.0 },
                &layout,
                &fg,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            Ok(())
        })?;

        Ok((surface, w_dips, h_dips))
    }
}

/// Shared widget card material used by every dock widget so all cards read as one family:
/// a frosted-glass surface (vertical fill gradient), a 1px rim that is brightest at the top edge,
/// and a thin specular line under the rim. Hover and press raise fill and rim opacity.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_chip_card(
    style: &ChipCardStyle,
    dc: &ID2D1DeviceContext,
    width_dips: f32,
    height_dips: f32,
    is_dark: bool,
    _active_tint: Option<Rgb>,
    interaction: WidgetInteraction,
    border_rgb: Option<Rgb>,
    border_width_dips: f32,
) -> Result<()> {
    let ink = if is_dark { 0xff } else { 0x00 };
    let rounded = |radius: f32, inset: f32| D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: inset,
            top: inset,
            right: (width_dips - inset).max(inset),
            bottom: (height_dips - inset).max(inset),
        },
        radiusX: (radius - inset).max(0.0),
        radiusY: (radius - inset).max(0.0),
    };

    // Dynamic Island: frameless chip, only a soft rounded hover/press pill.
    if style.chrome == ChipChrome::Bare {
        let alpha = match interaction {
            WidgetInteraction::Idle => return Ok(()),
            WidgetInteraction::Hovered(_) => 0.08,
            WidgetInteraction::Pressed(_) => 0.13,
        };
        let radius = 12.0_f32.min(height_dips * 0.5);
        unsafe {
            let brush = dc.CreateSolidColorBrush(&color_rgba(ink, ink, ink, alpha), None)?;
            dc.FillRoundedRectangle(&rounded(radius, 0.0), &brush);
        }
        return Ok(());
    }

    if style.surface.background == opentaskbar_core::settings::SurfaceBackground::None
        && matches!(interaction, WidgetInteraction::Idle)
        && border_rgb.is_none()
    {
        return Ok(());
    }

    if matches!(
        style.surface.background,
        opentaskbar_core::settings::SurfaceBackground::Solid
            | opentaskbar_core::settings::SurfaceBackground::TrueBlack
    ) {
        let true_black =
            style.surface.background == opentaskbar_core::settings::SurfaceBackground::TrueBlack;
        let alpha = if true_black {
            1.0
        } else {
            style.surface.opacity as f32
        };
        let (r, g, b) = if true_black {
            (0, 0, 0)
        } else if is_dark {
            (24, 24, 27)
        } else {
            (255, 255, 255)
        };
        unsafe {
            let brush = dc.CreateSolidColorBrush(&color_rgba(r, g, b, alpha), None)?;
            dc.FillRoundedRectangle(&rounded(island_geo::CHIP_RADIUS_DIPS, 0.0), &brush);
            if let Some(rgb) = border_rgb
                && border_width_dips > 0.0
            {
                let border =
                    dc.CreateSolidColorBrush(&color_rgba(rgb.0, rgb.1, rgb.2, 0.85), None)?;
                dc.DrawRoundedRectangle(
                    &rounded(island_geo::CHIP_RADIUS_DIPS, border_width_dips * 0.5),
                    &border,
                    border_width_dips,
                    None,
                );
            }
        }
        return Ok(());
    }

    // Theme pack / editor overrides: tint the card from `ThemeManager` tokens.
    if let Some(theme) = style.themed {
        let radius = theme
            .radius
            .unwrap_or(island_geo::CHIP_RADIUS_DIPS)
            .min(height_dips * 0.5);
        let solid = |color: design::tokens::Rgba, alpha_scale: f32| {
            color_rgba(
                color.r,
                color.g,
                color.b,
                (f32::from(color.a) / 255.0 * alpha_scale).clamp(0.0, 1.0),
            )
        };
        unsafe {
            if let Some(fill) = theme.fill {
                let brush =
                    dc.CreateSolidColorBrush(&solid(fill, style.surface.opacity as f32), None)?;
                dc.FillRoundedRectangle(&rounded(radius, 0.0), &brush);
            } else {
                let base_alpha = if is_dark { 0.08 } else { 0.06 };
                let brush = dc.CreateSolidColorBrush(
                    &color_rgba(ink, ink, ink, base_alpha * style.surface.opacity as f32),
                    None,
                )?;
                dc.FillRoundedRectangle(&rounded(radius, 0.0), &brush);
            }
            let overlay = match interaction {
                WidgetInteraction::Idle => None,
                WidgetInteraction::Hovered(_) => Some(theme.hover.map_or_else(
                    || color_rgba(ink, ink, ink, if is_dark { 0.10 } else { 0.07 }),
                    |c| solid(c, 1.0),
                )),
                WidgetInteraction::Pressed(_) => Some(theme.pressed.or(theme.hover).map_or_else(
                    || color_rgba(ink, ink, ink, if is_dark { 0.16 } else { 0.12 }),
                    |c| solid(c, if theme.pressed.is_some() { 1.0 } else { 1.4 }),
                )),
            };
            if let Some(color) = overlay {
                let brush = dc.CreateSolidColorBrush(&color, None)?;
                dc.FillRoundedRectangle(&rounded(radius, 0.0), &brush);
            }
            let stroke = match (border_rgb, theme.stroke) {
                (Some(rgb), _) if border_width_dips > 0.0 => {
                    Some((color_rgba(rgb.0, rgb.1, rgb.2, 0.85), border_width_dips))
                }
                (_, Some(token)) => Some((solid(token, 1.0), 1.0)),
                _ => None,
            };
            if let Some((color, width)) = stroke {
                let brush = dc.CreateSolidColorBrush(&color, None)?;
                dc.DrawRoundedRectangle(&rounded(radius, width * 0.5), &brush, width, None);
            }
        }
        return Ok(());
    }

    // (top fill alpha, bottom fill alpha, rim top alpha, rim bottom alpha, specular alpha)
    let (fill_top, fill_bottom, rim_top, rim_bottom, specular) = match (is_dark, interaction) {
        (true, WidgetInteraction::Idle) => (0.105, 0.050, 0.24, 0.06, 0.14),
        (true, WidgetInteraction::Hovered(_)) => (0.170, 0.100, 0.34, 0.12, 0.22),
        (true, WidgetInteraction::Pressed(_)) => (0.230, 0.150, 0.28, 0.14, 0.10),
        (false, WidgetInteraction::Idle) => (0.075, 0.045, 0.14, 0.06, 0.55),
        (false, WidgetInteraction::Hovered(_)) => (0.115, 0.075, 0.20, 0.09, 0.70),
        (false, WidgetInteraction::Pressed(_)) => (0.165, 0.120, 0.22, 0.11, 0.35),
    };
    let base = if is_dark { 0xff } else { 0x00 };
    let rim_base = if is_dark { 0xff } else { 0x00 };
    let gradient = |top: f32, bottom: f32, c: u8| -> Result<ID2D1LinearGradientBrush> {
        unsafe {
            linear_grad_brush(
                dc,
                (0.0, 0.0),
                (0.0, height_dips),
                &[
                    (0.0, color_rgba(c, c, c, top * style.surface.opacity as f32)),
                    (
                        1.0,
                        color_rgba(c, c, c, bottom * style.surface.opacity as f32),
                    ),
                ],
            )
        }
    };
    unsafe {
        let fill_rr = rrect(
            0.0,
            0.0,
            width_dips,
            height_dips,
            island_geo::CHIP_RADIUS_DIPS,
        );
        dc.FillRoundedRectangle(&fill_rr, &gradient(fill_top, fill_bottom, base)?);
        let rim_rr = rrect(0.5, 0.5, width_dips - 0.5, height_dips - 0.5, 11.5);
        dc.DrawRoundedRectangle(
            &rim_rr,
            &gradient(rim_top, rim_bottom, rim_base)?,
            1.0,
            None,
        );
        if let Some(rgb) = border_rgb
            && border_width_dips > 0.0
        {
            let brush = dc.CreateSolidColorBrush(&color_rgba(rgb.0, rgb.1, rgb.2, 0.85), None)?;
            dc.DrawRoundedRectangle(&rim_rr, &brush, border_width_dips, None);
        }
        if specular > 0.0 {
            let line = dc.CreateSolidColorBrush(&color_rgba(0xff, 0xff, 0xff, specular), None)?;
            dc.DrawLine(
                Vector2 { X: 12.0, Y: 1.5 },
                Vector2 {
                    X: width_dips - 12.0,
                    Y: 1.5,
                },
                &line,
                1.0,
                None,
            );
        }
    }
    Ok(())
}

/// Exact `WidgetCardBorder` / `WidgetCardButton` hover/press-only overlay (transparent at rest,
/// `#22FFFFFF` dark / `#18000000` light hover, `#38FFFFFF` / `#2A000000` press, `WidgetRadius = 12`).
/// Used where a widget paints its own full-card surface (the liquid Battery card).
unsafe fn draw_chip_overlay(
    dc: &ID2D1DeviceContext,
    width_dips: f32,
    height_dips: f32,
    is_dark: bool,
    interaction: WidgetInteraction,
) -> Result<()> {
    let fill_c = match interaction {
        WidgetInteraction::Idle => return Ok(()),
        WidgetInteraction::Hovered(_) => {
            if is_dark {
                color_rgba(0xff, 0xff, 0xff, 0x22 as f32 / 255.0)
            } else {
                color_rgba(0x00, 0x00, 0x00, 0x18 as f32 / 255.0)
            }
        }
        WidgetInteraction::Pressed(_) => {
            if is_dark {
                color_rgba(0xff, 0xff, 0xff, 0x38 as f32 / 255.0)
            } else {
                color_rgba(0x00, 0x00, 0x00, 0x2a as f32 / 255.0)
            }
        }
    };
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: width_dips,
            bottom: height_dips,
        },
        radiusX: 12.0,
        radiusY: 12.0,
    };
    unsafe {
        let fb = dc.CreateSolidColorBrush(&fill_c, None)?;
        dc.FillRoundedRectangle(&rr, &fb);
    }
    Ok(())
}

unsafe fn draw_fluent_str_d2d(
    dwrite: &IDWriteFactory,
    dc: &ID2D1DeviceContext,
    glyph_str: &str,
    dest: D2D_RECT_F,
    font_size: f32,
    color: Rgb,
    alpha: f32,
) -> Result<()> {
    let glyph: Vec<u16> = glyph_str.encode_utf16().collect();
    let col = color_rgba(color.0, color.1, color.2, alpha);
    unsafe {
        let fmt = dwrite.CreateTextFormat(
            &HSTRING::from(design::icons::icon_font_family()),
            None,
            windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            font_size,
            &HSTRING::from(design::fonts::ui_locale().as_str()),
        )?;
        fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let brush = dc.CreateSolidColorBrush(&col, None)?;
        dc.DrawText(
            &glyph,
            &fmt,
            &dest,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
        );
    }
    Ok(())
}

unsafe fn draw_fluent_glyph_d2d(
    dwrite: &IDWriteFactory,
    dc: &ID2D1DeviceContext,
    icon: AppIcon,
    dest: D2D_RECT_F,
    font_size: f32,
    color: Rgb,
    alpha: f32,
) -> Result<()> {
    unsafe {
        draw_fluent_str_d2d(
            dwrite,
            dc,
            icons::fluent_glyph(icon),
            dest,
            font_size,
            color,
            alpha,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_chip_icon(
    dwrite: &IDWriteFactory,
    factory: &ID2D1Factory1,
    dc: &ID2D1DeviceContext,
    icon: &WidgetIcon,
    cx: f32,
    cy: f32,
    size_dips: f32,
    color: Rgb,
) -> Result<()> {
    let half = size_dips * 0.6;
    let dest = D2D_RECT_F {
        left: cx - half,
        top: cy - half,
        right: cx + half,
        bottom: cy + half,
    };
    unsafe {
        match icon {
            WidgetIcon::Glyph(code) => {
                let glyph = char::from_u32(u32::from(*code)).unwrap_or('?').to_string();
                draw_fluent_str_d2d(dwrite, dc, &glyph, dest, size_dips, color, 0.96)
            }
            WidgetIcon::PathData(data) => {
                design::brand_paths::draw_path_data_logo(factory, dc, data, dest, color, 0.96)
            }
            WidgetIcon::Asset(path) => {
                if let Some(decoded) = crate::widgets::widget_icons::load_bgra(path) {
                    let (w, h, pixels) = &*decoded;
                    let props = D2D1_BITMAP_PROPERTIES1 {
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                        },
                        dpiX: 96.0,
                        dpiY: 96.0,
                        ..Default::default()
                    };
                    let bitmap = dc.CreateBitmap(
                        D2D_SIZE_U {
                            width: *w,
                            height: *h,
                        },
                        Some(pixels.as_ptr().cast()),
                        *w * 4,
                        &props,
                    )?;
                    dc.DrawBitmap(
                        &bitmap,
                        Some(&dest),
                        1.0,
                        D2D1_INTERPOLATION_MODE_LINEAR,
                        None,
                        None,
                    );
                    Ok(())
                } else {
                    // A missing or malformed asset must never blank the chip.
                    draw_fluent_str_d2d(
                        dwrite,
                        dc,
                        icons::glyph_for_icon(AppIcon::Layers),
                        dest,
                        size_dips,
                        color,
                        0.96,
                    )
                }
            }
        }
    }
}

/// Everything `create_island_chrome_surface` needs; the island resolves colours,
/// geometry and the symmetric header layout before asking for a surface.
pub struct IslandChromeRender<'a> {
    pub width_dips: f32,
    pub height_dips: f32,
    pub dpi: f32,
    pub is_dark: bool,
    pub opacity: f32,
    pub background: SurfaceBackground,
    /// Solid / tint fill (custom `#RRGGBB`, active theme surface or neutral pill).
    pub fill_rgb: Rgb,
    /// `Notch`: flat top edge flush with the monitor, rounded bottom corners.
    /// `Floating`: four rounded corners (a detached capsule).
    pub top_flush: bool,
    pub radius_dips: f32,
    pub layout: &'a super::dynamic_island::IslandHeaderLayout,
    pub current_page: usize,
    pub expanded: bool,
    pub hovered_btn: Option<IslandButton>,
    pub pressed_btn: Option<IslandButton>,
    pub border_rgb: Rgb,
    pub border_width_dips: f32,
    pub glass_host_active: bool,
    /// Strip layout hairline divider x positions (island DIPs).
    pub dividers_dips: &'a [f32],
    /// Draw the pin toggle in `layout.pin` (expanded, hovered or pinned island).
    pub show_pin: bool,
    pub pinned: bool,
    /// N9 privacy dots: `bit 0` = microphone (amber), `bit 1` = camera (green).
    pub privacy_dots: u8,
    /// N6 live audio waveform (`0..=100` per bar); all zero = not drawn.
    pub waveform_bars: [u8; 4],
    /// Where the Full-presentation waveform sits (the free pin slot next to the media chip).
    pub waveform_rect: Option<(f32, f32, f32, f32)>,
    /// Slim resting capsule (`DynamicIslandIdleMode::Minimal`): no navigation chrome, only
    /// `lyric_pill` (lyric / clock / track / AI usage line), the waveform and privacy dots.
    pub minimal: bool,
    /// Trimmed one-line text of the minimal capsule.
    pub lyric_pill: Option<&'a str>,
    /// Overlay presentation: capsule only, the overlay node draws the content on top.
    pub bare: bool,
}

/// Content of the single lazily allocated island overlay node.
pub enum IslandOverlayContent<'a> {
    /// N7 agent approval banner.
    AgentApproval {
        agent: &'a str,
        project: &'a str,
        action_title: &'a str,
        detail: &'a str,
        added: usize,
        removed: usize,
    },
    /// N1 transient system HUD pill (progress bar for level HUDs, badge otherwise).
    Hud {
        icon: &'a str,
        title: &'a str,
        subtitle: &'a str,
        progress: Option<f32>,
        accent_rgb: Option<Rgb>,
    },
    /// O5 mirrored notification pill.
    Notification {
        app_name: &'a str,
        title: &'a str,
        body: &'a str,
    },
}

/// Everything `create_island_overlay_surface` needs; geometry comes from
/// `IslandOverlayLayout` so hit testing always matches what is drawn.
pub struct IslandOverlayRender<'a> {
    pub content: IslandOverlayContent<'a>,
    pub layout: &'a super::dynamic_island::IslandOverlayLayout,
    pub dpi: f32,
    pub is_dark: bool,
    pub hovered: Option<IslandOverlayButton>,
    pub pressed: Option<IslandOverlayButton>,
}

/// N6: four rounded live audio bars centered in `rect` (`0..=100` each, 2 DIP minimum).
unsafe fn draw_waveform_bars(
    dc: &ID2D1DeviceContext,
    rect: D2D_RECT_F,
    bars: [u8; 4],
    color: Rgb,
) -> Result<()> {
    let w = (rect.right - rect.left).max(4.0);
    let h = (rect.bottom - rect.top).max(4.0);
    let bar_w = (w / 7.0).clamp(1.5, 3.5);
    let gap = ((w - bar_w * 4.0) / 3.0).max(1.0);
    let total = bar_w * 4.0 + gap * 3.0;
    let mut x = rect.left + ((w - total) * 0.5).max(0.0);
    let cy = rect.top + h * 0.5;
    // SAFETY: `dc` is inside an active `BeginDraw` on the UI thread.
    unsafe {
        let brush = dc.CreateSolidColorBrush(&color_rgba(color.0, color.1, color.2, 0.92), None)?;
        for value in bars {
            let bh = (f32::from(value.min(100)) / 100.0 * h).max(2.0);
            dc.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: x,
                        top: cy - bh * 0.5,
                        right: x + bar_w,
                        bottom: cy + bh * 0.5,
                    },
                    radiusX: bar_w * 0.5,
                    radiusY: bar_w * 0.5,
                },
                &brush,
            );
            x += bar_w + gap;
        }
    }
    Ok(())
}

/// N9: crisp 6 DIP glowing privacy dots (microphone amber, camera green) starting at `cx`.
unsafe fn draw_privacy_dots(dc: &ID2D1DeviceContext, cx: f32, cy: f32, bits: u8) -> Result<()> {
    const MIC_AMBER: Rgb = (255, 159, 10);
    const CAMERA_GREEN: Rgb = (52, 199, 89);
    let mut x = cx;
    for (bit, color) in [(0b01_u8, MIC_AMBER), (0b10_u8, CAMERA_GREEN)] {
        if bits & bit == 0 {
            continue;
        }
        // SAFETY: `dc` is inside an active `BeginDraw` on the UI thread.
        unsafe {
            let glow =
                dc.CreateSolidColorBrush(&color_rgba(color.0, color.1, color.2, 0.28), None)?;
            dc.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: x, Y: cy },
                    radiusX: 5.0,
                    radiusY: 5.0,
                },
                &glow,
            );
            let core =
                dc.CreateSolidColorBrush(&color_rgba(color.0, color.1, color.2, 1.0), None)?;
            dc.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: x, Y: cy },
                    radiusX: 3.0,
                    radiusY: 3.0,
                },
                &core,
            );
        }
        x -= 10.0;
    }
    Ok(())
}

/// Everything `create_island_detail_surface` needs to draw the expanded detail card.
pub struct IslandDetailRender<'a> {
    pub spec: &'a crate::widgets::IslandDetailSpec,
    pub layout: &'a super::dynamic_island::IslandDetailLayout,
    pub dpi: f32,
    pub is_dark: bool,
    pub glass_host_active: bool,
    pub hovered_button: Option<usize>,
    pub pressed_button: Option<usize>,
    /// Localized label of the trailing "open the full flyout" button.
    pub open_full_label: &'a str,
}

impl DcompEngine {
    /// Draws the Dynamic Island capsule (Notch: top-flush with rounded bottom corners,
    /// Floating: fully rounded pill), border, symmetric `‹`/`›` navigation, clickable page
    /// dots, the compact expand chevron and the Strip layout hairline dividers.
    pub fn create_island_chrome_surface(
        &self,
        r: &IslandChromeRender<'_>,
    ) -> Result<IDCompositionSurface> {
        let pal = design::active_palette(r.is_dark);
        let dpi = r.dpi.max(0.5);
        let w_px = (r.width_dips * dpi).ceil().max(1.0) as u32;
        let h_px = (r.height_dips * dpi).ceil().max(1.0) as u32;
        let ink = if r.is_dark {
            (0xff_u8, 0xff_u8, 0xff_u8)
        } else {
            (0x18_u8, 0x18_u8, 0x1b_u8)
        };
        let fmt_line = match r.lyric_pill {
            Some(_) if r.minimal => {
                Some(self.cached_text_format(design::fonts::scaled_size(11.5), true, 0)?)
            }
            _ => None,
        };

        self.draw_surface(w_px, h_px, |dc, _w, _h| unsafe {
            let prev = set_dpi_transform(dc, dpi);
            let w = r.width_dips;
            let h = r.height_dips;
            let stroke = r.border_width_dips.clamp(0.0, 8.0);
            let radius = r.radius_dips.clamp(0.0, (h * 0.5).min(w * 0.5));
            let fill_rgb = if r.background == SurfaceBackground::TrueBlack {
                (0_u8, 0_u8, 0_u8)
            } else {
                r.fill_rgb
            };
            let fill_alpha = match r.background {
                SurfaceBackground::None => 0.0,
                SurfaceBackground::Solid | SurfaceBackground::TrueBlack => 1.0,
                SurfaceBackground::Glass if r.glass_host_active => 0.0,
                SurfaceBackground::Glass => r.opacity.clamp(0.1, 1.0),
            };
            let inset = stroke * 0.5;
            // Notch: extend the rounded rect above the surface so only the bottom
            // corners are rounded and the top edge stays razor-flat at y = 0.
            let capsule = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: inset,
                    top: if r.top_flush { -radius } else { inset },
                    right: w - inset,
                    bottom: h - inset,
                },
                radiusX: radius,
                radiusY: radius,
            };
            if fill_alpha > 0.0 {
                let bg = dc.CreateSolidColorBrush(
                    &color_rgba(fill_rgb.0, fill_rgb.1, fill_rgb.2, fill_alpha),
                    None,
                )?;
                dc.FillRoundedRectangle(&capsule, &bg);
            }
            if stroke > 0.0
                && r.background != SurfaceBackground::None
                && r.background != SurfaceBackground::TrueBlack
                && !(r.glass_host_active && r.background == SurfaceBackground::Glass)
            {
                let bd_alpha = if r.is_dark { 0.14 } else { 0.12 };
                let bd = dc.CreateSolidColorBrush(
                    &color_rgba(r.border_rgb.0, r.border_rgb.1, r.border_rgb.2, bd_alpha),
                    None,
                )?;
                dc.DrawRoundedRectangle(&capsule, &bd, stroke, None);
            }

            let show_nav = !r.minimal && !r.bare;
            // Strip layout: crisp 1 DIP hairlines between packed widgets.
            if show_nav && !r.dividers_dips.is_empty() {
                let header = r.layout.chip_top + r.layout.chip_h;
                let div = dc.CreateSolidColorBrush(
                    &color_rgba(ink.0, ink.1, ink.2, if r.is_dark { 0.16 } else { 0.14 }),
                    None,
                )?;
                for x in r.dividers_dips {
                    let x = x.round() + 0.5 / dpi;
                    dc.FillRectangle(
                        &D2D_RECT_F {
                            left: x - 0.5,
                            top: r.layout.chip_top + 8.0,
                            right: x + 0.5,
                            bottom: (header - 8.0).max(r.layout.chip_top + 9.0),
                        },
                        &div,
                    );
                }
            }

            let draw_btn =
                |btn: IslandButton, rect: (f32, f32, f32, f32), glyph: &str| -> Result<()> {
                    let is_pressed = r.pressed_btn == Some(btn);
                    let is_hovered = r.hovered_btn == Some(btn);
                    let (l, t, rr, b) = rect;
                    if is_pressed || is_hovered {
                        let alpha = if is_pressed {
                            0.16
                        } else if r.is_dark {
                            0.10
                        } else {
                            0.08
                        };
                        let hb = dc
                            .CreateSolidColorBrush(&color_rgba(ink.0, ink.1, ink.2, alpha), None)?;
                        let br = island_geo::BUTTON_RADIUS_DIPS.max(6.0);
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: l,
                                    top: t,
                                    right: rr,
                                    bottom: b,
                                },
                                radiusX: br,
                                radiusY: br,
                            },
                            &hb,
                        );
                    }
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        glyph,
                        D2D_RECT_F {
                            left: l,
                            top: t,
                            right: rr,
                            bottom: b,
                        },
                        12.0,
                        pal.text_primary,
                        if is_hovered || is_pressed { 0.98 } else { 0.78 },
                    )
                };

            if show_nav && let Some(rect) = r.layout.prev {
                draw_btn(
                    IslandButton::Prev,
                    rect,
                    icons::glyph_for_icon(AppIcon::ChevronLeft),
                )?;
            }
            if show_nav && let Some(rect) = r.layout.next {
                draw_btn(
                    IslandButton::Next,
                    rect,
                    icons::glyph_for_icon(AppIcon::ChevronRight),
                )?;
            }
            if show_nav
                && r.show_pin
                && let Some(rect) = r.layout.pin
            {
                // Pin (E718) / pinned fill (E841).
                let glyph = if r.pinned { "\u{E841}" } else { "\u{E718}" };
                draw_btn(IslandButton::Pin, rect, glyph)?;
            }
            if show_nav && let Some(rect) = r.layout.expand {
                let glyph = if r.expanded {
                    icons::named_glyph("ChevronUp").unwrap_or("\u{E70E}")
                } else {
                    icons::named_glyph("ChevronDown").unwrap_or("\u{E70D}")
                };
                draw_btn(IslandButton::Expand, rect, glyph)?;
            }

            // Page indicator: 6 DIP dots, the active page a 14×6 DIP pill.
            let dots = if show_nav {
                r.layout.dots.as_slice()
            } else {
                &[]
            };
            for (page, (l, t, rr, b)) in dots {
                let active = *page == r.current_page;
                let hovered = r.hovered_btn == Some(IslandButton::Dot(*page as u8));
                let (color, alpha) = if active {
                    (pal.text_primary, 0.92)
                } else if hovered {
                    (pal.text_primary, 0.62)
                } else {
                    (pal.text_secondary, 0.38)
                };
                let brush =
                    dc.CreateSolidColorBrush(&color_rgba(color.0, color.1, color.2, alpha), None)?;
                let dot_r = (b - t) * 0.5;
                dc.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: *l,
                            top: *t,
                            right: *rr,
                            bottom: *b,
                        },
                        radiusX: dot_r,
                        radiusY: dot_r,
                    },
                    &brush,
                );
            }

            let wave_on = r.waveform_bars != [0; 4];
            if r.minimal {
                // Slim resting capsule: `[dots] line [waveform]`, symmetric side reserves.
                let pad = (radius * 0.55).clamp(10.0, 16.0);
                let dots_w: f32 = match r.privacy_dots & 0b11 {
                    0 => 0.0,
                    0b11 => 22.0,
                    _ => 12.0,
                };
                let wave_w: f32 = if wave_on { 22.0 } else { 0.0 };
                let side = dots_w.max(wave_w);
                if r.privacy_dots & 0b11 != 0 {
                    let first = pad
                        + if r.privacy_dots & 0b11 == 0b11 {
                            13.0
                        } else {
                            3.0
                        };
                    draw_privacy_dots(dc, first, h * 0.5, r.privacy_dots)?;
                }
                if wave_on {
                    draw_waveform_bars(
                        dc,
                        D2D_RECT_F {
                            left: w - pad - wave_w,
                            top: h * 0.24,
                            right: w - pad,
                            bottom: h * 0.76,
                        },
                        r.waveform_bars,
                        pal.widget_accent,
                    )?;
                }
                if let (Some(text), Some(fmt)) = (r.lyric_pill, fmt_line.as_ref())
                    && !text.is_empty()
                {
                    let left = pad + side + if side > 0.0 { 4.0 } else { 0.0 };
                    let right = (w - left).max(left + 1.0);
                    let wtxt: Vec<u16> = text.encode_utf16().collect();
                    if let Ok(text_layout) =
                        self.dwrite_factory
                            .CreateTextLayout(&wtxt, fmt, right - left, h.max(1.0))
                    {
                        let brush = dc.CreateSolidColorBrush(
                            &color_rgba(
                                pal.text_primary.0,
                                pal.text_primary.1,
                                pal.text_primary.2,
                                0.94,
                            ),
                            None,
                        )?;
                        dc.DrawTextLayout(
                            Vector2 { X: left, Y: 0.0 },
                            &text_layout,
                            &brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
            } else {
                if show_nav
                    && wave_on
                    && let Some((l, t, rr, b)) = r.waveform_rect
                {
                    let inset_y = ((b - t) * 0.22).max(2.0);
                    draw_waveform_bars(
                        dc,
                        D2D_RECT_F {
                            left: l + 3.0,
                            top: t + inset_y,
                            right: rr - 3.0,
                            bottom: b - inset_y,
                        },
                        r.waveform_bars,
                        pal.widget_accent,
                    )?;
                }
                if r.privacy_dots & 0b11 != 0 {
                    // Top-trailing capsule corner, inside the rounded edge.
                    let cx = w - (radius * 0.62).max(14.0);
                    let cy = if r.top_flush {
                        8.0
                    } else {
                        9.0_f32.min(h * 0.5)
                    };
                    draw_privacy_dots(dc, cx, cy, r.privacy_dots)?;
                }
            }
            dc.SetTransform(&prev);
            Ok(())
        })
    }

    /// Transient Dynamic Island overlay drawn into the single lazily allocated overlay node:
    /// N7 agent approval banner (agent, project, action, `+N -M` diff summary and
    /// Allow / Deny / Always / Terminal buttons), N1 system HUD pill (icon, title and a smooth
    /// progress bar or badge) and O5 notification pill (app badge, title, one-line body, `×`).
    /// The capsule background comes from the chrome surface underneath.
    pub fn create_island_overlay_surface(
        &self,
        r: &IslandOverlayRender<'_>,
    ) -> Result<IDCompositionSurface> {
        let pal = design::active_palette(r.is_dark);
        let dpi = r.dpi.max(0.5);
        let layout = r.layout;
        let w_px = (layout.width * dpi).ceil().max(1.0) as u32;
        let h_px = (layout.height * dpi).ceil().max(1.0) as u32;
        let ink = if r.is_dark {
            (0xff_u8, 0xff_u8, 0xff_u8)
        } else {
            (0x00_u8, 0x00_u8, 0x00_u8)
        };
        let fmt_small = self.cached_text_format(design::fonts::scaled_size(10.5), false, 1)?;
        let fmt_title = self.cached_text_format(design::fonts::scaled_size(13.0), true, 1)?;
        let fmt_body = self.cached_text_format(design::fonts::scaled_size(11.0), false, 1)?;
        let fmt_value = self.cached_text_format(design::fonts::scaled_size(11.0), true, 2)?;
        let fmt_button = self.cached_text_format(design::fonts::scaled_size(11.0), true, 0)?;
        let fmt_badge = self.cached_text_format(design::fonts::scaled_size(14.0), true, 0)?;
        let accent = pal.widget_accent;

        self.draw_surface(w_px, h_px, |dc, _w, _h| unsafe {
            let mut prev = Matrix3x2::identity();
            dc.GetTransform(&mut prev);
            dc.SetTransform(&Matrix3x2 {
                M11: dpi,
                M12: 0.0,
                M21: 0.0,
                M22: dpi,
                M31: prev.M31,
                M32: prev.M32,
            });
            let rect = |rc: (f32, f32, f32, f32)| D2D_RECT_F {
                left: rc.0,
                top: rc.1,
                right: rc.2,
                bottom: rc.3,
            };
            let rounded = |rc: D2D_RECT_F, radius: f32| D2D1_ROUNDED_RECT {
                rect: rc,
                radiusX: radius,
                radiusY: radius,
            };
            let p_brush = dc.CreateSolidColorBrush(
                &color_rgba(
                    pal.text_primary.0,
                    pal.text_primary.1,
                    pal.text_primary.2,
                    0.98,
                ),
                None,
            )?;
            let s_brush = dc.CreateSolidColorBrush(
                &color_rgba(
                    pal.text_secondary.0,
                    pal.text_secondary.1,
                    pal.text_secondary.2,
                    0.92,
                ),
                None,
            )?;
            let white = dc.CreateSolidColorBrush(&color_rgba(0xff, 0xff, 0xff, 0.98), None)?;
            let draw_text =
                |text: &str,
                 fmt: &IDWriteTextFormat,
                 rc: D2D_RECT_F,
                 brush: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush| {
                    if text.is_empty() {
                        return;
                    }
                    let wtxt: Vec<u16> = text.encode_utf16().collect();
                    if let Ok(text_layout) = self.dwrite_factory.CreateTextLayout(
                        &wtxt,
                        fmt,
                        (rc.right - rc.left).max(1.0),
                        (rc.bottom - rc.top).max(1.0),
                    ) {
                        dc.DrawTextLayout(
                            Vector2 {
                                X: rc.left,
                                Y: rc.top,
                            },
                            &text_layout,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                };
            // Tinted disc behind the leading icon / badge.
            let icon_disc = |tint: Rgb, alpha: f32| -> Result<()> {
                let (l, t, rr, b) = layout.icon;
                let radius = ((rr - l).min(b - t) * 0.5).max(1.0);
                let brush =
                    dc.CreateSolidColorBrush(&color_rgba(tint.0, tint.1, tint.2, alpha), None)?;
                dc.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 {
                            X: (l + rr) * 0.5,
                            Y: (t + b) * 0.5,
                        },
                        radiusX: radius,
                        radiusY: radius,
                    },
                    &brush,
                );
                Ok(())
            };

            match &r.content {
                IslandOverlayContent::Hud {
                    icon,
                    title,
                    subtitle,
                    progress,
                    accent_rgb,
                } => {
                    let tint = accent_rgb.unwrap_or(accent);
                    icon_disc(tint, if r.is_dark { 0.22 } else { 0.16 })?;
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        icon,
                        rect(layout.icon),
                        15.0,
                        tint,
                        0.98,
                    )?;
                    draw_text(title, &fmt_title, rect(layout.title), &p_brush);
                    match (progress.filter(|p| p.is_finite()), layout.progress) {
                        (Some(level), Some(bar)) => {
                            draw_text(subtitle, &fmt_value, rect(layout.subtitle), &s_brush);
                            let bar = rect(bar);
                            let radius = ((bar.bottom - bar.top) * 0.5).max(1.0);
                            let track = dc.CreateSolidColorBrush(
                                &color_rgba(
                                    ink.0,
                                    ink.1,
                                    ink.2,
                                    if r.is_dark { 0.16 } else { 0.10 },
                                ),
                                None,
                            )?;
                            dc.FillRoundedRectangle(&rounded(bar, radius), &track);
                            let fill_w = (bar.right - bar.left) * level.clamp(0.0, 1.0);
                            if fill_w >= 1.0 {
                                let fill = dc.CreateSolidColorBrush(
                                    &color_rgba(tint.0, tint.1, tint.2, 0.96),
                                    None,
                                )?;
                                dc.FillRoundedRectangle(
                                    &rounded(
                                        D2D_RECT_F {
                                            right: bar.left + fill_w.max(radius * 2.0),
                                            ..bar
                                        },
                                        radius,
                                    ),
                                    &fill,
                                );
                            }
                        }
                        _ if !subtitle.is_empty() => {
                            // Badge pill (Caps Lock, keyboard layout, network, Bluetooth).
                            let mut badge = rect(layout.subtitle);
                            let text_w = subtitle.chars().count() as f32 * 6.4 + 16.0;
                            badge.left = (badge.right - text_w).max(badge.left);
                            let badge_brush = dc.CreateSolidColorBrush(
                                &color_rgba(
                                    tint.0,
                                    tint.1,
                                    tint.2,
                                    if r.is_dark { 0.24 } else { 0.16 },
                                ),
                                None,
                            )?;
                            let radius = ((badge.bottom - badge.top) * 0.5).max(1.0);
                            dc.FillRoundedRectangle(&rounded(badge, radius), &badge_brush);
                            draw_text(subtitle, &fmt_button, badge, &p_brush);
                        }
                        _ => {}
                    }
                }
                IslandOverlayContent::Notification {
                    app_name,
                    title,
                    body,
                } => {
                    icon_disc(accent, 0.92)?;
                    let initial: String = app_name
                        .chars()
                        .find(|c| c.is_alphanumeric())
                        .map(|c| c.to_uppercase().collect())
                        .unwrap_or_else(|| "\u{2022}".to_owned());
                    draw_text(&initial, &fmt_badge, rect(layout.icon), &white);
                    draw_text(app_name, &fmt_small, rect(layout.subtitle), &s_brush);
                    draw_text(title, &fmt_title, rect(layout.title), &p_brush);
                    draw_text(body, &fmt_body, rect(layout.body), &s_brush);
                }
                IslandOverlayContent::AgentApproval {
                    agent,
                    project,
                    action_title,
                    detail,
                    added,
                    removed,
                } => {
                    icon_disc(pal.warning, if r.is_dark { 0.24 } else { 0.18 })?;
                    // Robot glyph (E99A).
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        "\u{E99A}",
                        rect(layout.icon),
                        15.0,
                        pal.warning,
                        0.98,
                    )?;
                    let header = match (agent.is_empty(), project.is_empty()) {
                        (false, false) => format!("{agent} \u{00B7} {project}"),
                        (false, true) => (*agent).to_owned(),
                        (true, false) => (*project).to_owned(),
                        (true, true) => String::new(),
                    };
                    draw_text(&header, &fmt_small, rect(layout.subtitle), &s_brush);
                    draw_text(action_title, &fmt_title, rect(layout.title), &p_brush);
                    draw_text(detail, &fmt_body, rect(layout.body), &s_brush);
                    if let Some(meta) = layout.meta
                        && (*added > 0 || *removed > 0)
                    {
                        let meta = rect(meta);
                        let half = (meta.right - meta.left) * 0.5;
                        let green = dc.CreateSolidColorBrush(
                            &color_rgba(pal.success.0, pal.success.1, pal.success.2, 0.98),
                            None,
                        )?;
                        let red = dc.CreateSolidColorBrush(
                            &color_rgba(pal.danger.0, pal.danger.1, pal.danger.2, 0.98),
                            None,
                        )?;
                        draw_text(
                            &format!("+{added}"),
                            &fmt_value,
                            D2D_RECT_F {
                                right: meta.left + half - 4.0,
                                ..meta
                            },
                            &green,
                        );
                        draw_text(
                            &format!("\u{2212}{removed}"),
                            &fmt_body,
                            D2D_RECT_F {
                                left: meta.left + half + 4.0,
                                ..meta
                            },
                            &red,
                        );
                    }
                }
            }

            for (button, rc) in &layout.buttons {
                let hovered = r.hovered == Some(*button);
                let pressed = r.pressed == Some(*button);
                let rc = rect(*rc);
                if *button == IslandOverlayButton::Dismiss {
                    if hovered || pressed {
                        let alpha = if pressed { 0.18 } else { 0.10 };
                        let hb = dc
                            .CreateSolidColorBrush(&color_rgba(ink.0, ink.1, ink.2, alpha), None)?;
                        let radius = ((rc.right - rc.left).min(rc.bottom - rc.top) * 0.5).max(1.0);
                        dc.FillRoundedRectangle(&rounded(rc, radius), &hb);
                    }
                    // Cancel glyph (E711).
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        "\u{E711}",
                        rc,
                        10.0,
                        pal.text_primary,
                        if hovered || pressed { 0.95 } else { 0.62 },
                    )?;
                    continue;
                }
                let primary = *button == IslandOverlayButton::Allow;
                let fill = if primary {
                    let alpha = if pressed {
                        0.78
                    } else if hovered {
                        1.0
                    } else {
                        0.90
                    };
                    color_rgba(accent.0, accent.1, accent.2, alpha)
                } else {
                    let alpha = match (pressed, hovered, r.is_dark) {
                        (true, _, true) => 0.20,
                        (true, _, false) => 0.15,
                        (false, true, true) => 0.14,
                        (false, true, false) => 0.10,
                        (false, false, true) => 0.08,
                        (false, false, false) => 0.06,
                    };
                    color_rgba(ink.0, ink.1, ink.2, alpha)
                };
                let fill_brush = dc.CreateSolidColorBrush(&fill, None)?;
                dc.FillRoundedRectangle(&rounded(rc, 8.0), &fill_brush);
                let label = button.label();
                let text_brush = if primary { &white } else { &p_brush };
                draw_text(
                    &label,
                    &fmt_button,
                    D2D_RECT_F {
                        left: rc.left + 6.0,
                        right: rc.right - 6.0,
                        ..rc
                    },
                    text_brush,
                );
            }
            dc.SetTransform(&prev);
            Ok(())
        })
    }

    /// Rich expanded Dynamic Island card: localized title, bold headline metric, subtitle,
    /// up to four stat rows with rounded progress bars and up to three quick actions plus
    /// an "open full flyout" button. Geometry comes from `IslandDetailLayout` so the
    /// island's hit testing always matches what is drawn.
    pub fn create_island_detail_surface(
        &self,
        r: &IslandDetailRender<'_>,
    ) -> Result<IDCompositionSurface> {
        let pal = design::active_palette(r.is_dark);
        let dpi = r.dpi.max(0.5);
        let layout = r.layout;
        let (pl, pt, pr, pb) = layout.panel;
        let panel_w = (pr - pl).max(1.0);
        let panel_h = (pb - pt).max(1.0);
        let w_px = (panel_w * dpi).ceil().max(1.0) as u32;
        let h_px = (panel_h * dpi).ceil().max(1.0) as u32;
        let ink = if r.is_dark {
            (0xff_u8, 0xff_u8, 0xff_u8)
        } else {
            (0x00_u8, 0x00_u8, 0x00_u8)
        };
        let fmt_title = self.cached_text_format(design::fonts::scaled_size(11.0), true, 1)?;
        let fmt_headline = self.cached_text_format(design::fonts::scaled_size(20.0), true, 1)?;
        let fmt_sub = self.cached_text_format(design::fonts::scaled_size(11.0), false, 1)?;
        let fmt_label = self.cached_text_format(design::fonts::scaled_size(11.0), false, 1)?;
        let fmt_value = self.cached_text_format(design::fonts::scaled_size(11.0), true, 2)?;
        let fmt_button = self.cached_text_format(design::fonts::scaled_size(11.0), true, 0)?;
        let accent = pal.widget_accent;

        self.draw_surface(w_px, h_px, |dc, _w, _h| unsafe {
            let prev = set_dpi_transform(dc, dpi);
            // Island-local rect -> panel-local rect.
            let local = |rect: (f32, f32, f32, f32)| D2D_RECT_F {
                left: rect.0 - pl,
                top: rect.1 - pt,
                right: rect.2 - pl,
                bottom: rect.3 - pt,
            };
            let rounded = |rect: D2D_RECT_F, radius: f32| D2D1_ROUNDED_RECT {
                rect,
                radiusX: radius,
                radiusY: radius,
            };
            let panel_fill = if r.glass_host_active {
                color_rgba(ink.0, ink.1, ink.2, if r.is_dark { 0.07 } else { 0.05 })
            } else if r.is_dark {
                color_rgba(0x1b, 0x21, 0x2d, 0.97)
            } else {
                color_rgba(0xf2, 0xf4, 0xfa, 0.97)
            };
            let panel_brush = dc.CreateSolidColorBrush(&panel_fill, None)?;
            dc.FillRoundedRectangle(
                &rounded(
                    D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: panel_w,
                        bottom: panel_h,
                    },
                    island_geo::CHIP_RADIUS_DIPS,
                ),
                &panel_brush,
            );

            let p_brush = dc.CreateSolidColorBrush(
                &color_rgba(
                    pal.text_primary.0,
                    pal.text_primary.1,
                    pal.text_primary.2,
                    0.98,
                ),
                None,
            )?;
            let s_brush = dc.CreateSolidColorBrush(
                &color_rgba(
                    pal.text_secondary.0,
                    pal.text_secondary.1,
                    pal.text_secondary.2,
                    0.92,
                ),
                None,
            )?;
            let draw_text =
                |text: &str,
                 fmt: &IDWriteTextFormat,
                 rect: D2D_RECT_F,
                 brush: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush| {
                    if text.is_empty() {
                        return;
                    }
                    let wtxt: Vec<u16> = text.encode_utf16().collect();
                    if let Ok(text_layout) = self.dwrite_factory.CreateTextLayout(
                        &wtxt,
                        fmt,
                        (rect.right - rect.left).max(1.0),
                        (rect.bottom - rect.top).max(1.0),
                    ) {
                        dc.DrawTextLayout(
                            Vector2 {
                                X: rect.left,
                                Y: rect.top,
                            },
                            &text_layout,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                };

            draw_text(&r.spec.title, &fmt_title, local(layout.title), &s_brush);
            draw_text(
                &r.spec.headline,
                &fmt_headline,
                local(layout.headline),
                &p_brush,
            );
            draw_text(&r.spec.subtitle, &fmt_sub, local(layout.subtitle), &s_brush);

            let track = dc.CreateSolidColorBrush(
                &color_rgba(ink.0, ink.1, ink.2, if r.is_dark { 0.14 } else { 0.10 }),
                None,
            )?;
            for (row, rect) in r.spec.rows.iter().zip(layout.rows.iter()) {
                let rc = local(*rect);
                let text_bottom = if row.progress.is_some() {
                    rc.bottom - 7.0
                } else {
                    rc.bottom
                };
                draw_text(
                    &row.label,
                    &fmt_label,
                    D2D_RECT_F {
                        right: rc.left + (rc.right - rc.left) * 0.58,
                        bottom: text_bottom,
                        ..rc
                    },
                    &s_brush,
                );
                draw_text(
                    &row.value,
                    &fmt_value,
                    D2D_RECT_F {
                        left: rc.left + (rc.right - rc.left) * 0.42,
                        bottom: text_bottom,
                        ..rc
                    },
                    &p_brush,
                );
                if let Some(progress) = row.progress.filter(|p| p.is_finite()) {
                    let ratio = progress.clamp(0.0, 1.0);
                    let bar = D2D_RECT_F {
                        left: rc.left,
                        top: rc.bottom - 5.0,
                        right: rc.right,
                        bottom: rc.bottom - 1.0,
                    };
                    dc.FillRoundedRectangle(&rounded(bar, 2.0), &track);
                    let fill_w = (bar.right - bar.left) * ratio;
                    if fill_w >= 1.0 {
                        let color = if ratio >= 0.9 {
                            pal.danger
                        } else if ratio >= 0.75 {
                            pal.warning
                        } else {
                            accent
                        };
                        let fill = dc.CreateSolidColorBrush(
                            &color_rgba(color.0, color.1, color.2, 0.95),
                            None,
                        )?;
                        dc.FillRoundedRectangle(
                            &rounded(
                                D2D_RECT_F {
                                    right: bar.left + fill_w.max(4.0),
                                    ..bar
                                },
                                2.0,
                            ),
                            &fill,
                        );
                    }
                }
            }

            for (idx, rect) in layout.buttons.iter().enumerate() {
                let (label, primary) = match r.spec.actions.get(idx) {
                    Some(action) => (action.label.as_str(), action.primary),
                    None => (r.open_full_label, false),
                };
                let hovered = r.hovered_button == Some(idx);
                let pressed = r.pressed_button == Some(idx);
                let rc = local(*rect);
                let (fill, text_brush) = if primary {
                    let alpha = if pressed {
                        0.78
                    } else if hovered {
                        1.0
                    } else {
                        0.90
                    };
                    let white =
                        dc.CreateSolidColorBrush(&color_rgba(0xff, 0xff, 0xff, 0.98), None)?;
                    (color_rgba(accent.0, accent.1, accent.2, alpha), white)
                } else {
                    let alpha = match (pressed, hovered, r.is_dark) {
                        (true, _, true) => 0.20,
                        (true, _, false) => 0.15,
                        (false, true, true) => 0.14,
                        (false, true, false) => 0.10,
                        (false, false, true) => 0.08,
                        (false, false, false) => 0.06,
                    };
                    (color_rgba(ink.0, ink.1, ink.2, alpha), p_brush.clone())
                };
                let fill_brush = dc.CreateSolidColorBrush(&fill, None)?;
                dc.FillRoundedRectangle(&rounded(rc, 8.0), &fill_brush);
                draw_text(
                    label,
                    &fmt_button,
                    D2D_RECT_F {
                        left: rc.left + 6.0,
                        right: rc.right - 6.0,
                        ..rc
                    },
                    &text_brush,
                );
            }
            dc.SetTransform(&prev);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_lines_scale_ai_dual_percentages_and_localize_quicksettings() {
        let ai = WidgetChipSpec::AiDual {
            provider_id: "aiusage_claude",
            brand_color: None,
            rows: vec![("5h".to_owned(), 0.42, (0, 0, 0), "2h 15m".to_owned())],
        };
        let ai_lines = ai.details_lines();
        assert_eq!(ai_lines.len(), 1);
        assert!(
            ai_lines[0].1.contains("42"),
            "AiDual details_lines should scale 0.42 to 42%, got {:?}",
            ai_lines[0].1
        );

        let qs = WidgetChipSpec::QuickSettings {
            pills: [
                ("\u{E701}".to_owned(), true),
                ("\u{E702}".to_owned(), false),
                ("\u{E708}".to_owned(), true),
            ],
        };
        let qs_lines = qs.details_lines();
        assert_eq!(qs_lines.len(), 3);
        for (label, _) in &qs_lines {
            assert!(
                !label.contains('\u{E701}')
                    && !label.contains('\u{E702}')
                    && !label.contains('\u{E708}'),
                "QuickSettings details_lines must not emit raw PUA glyphs: {label:?}"
            );
        }
    }

    #[test]
    fn write_cache_key_incorporates_visual_side_channel_hints() {
        let media = WidgetChipSpec::Media {
            title: "Track".to_owned(),
            artist: "Artist".to_owned(),
        };
        crate::widgets::set_chip_media_hint(crate::widgets::MediaChipHint {
            playing: false,
            progress: 0.1,
            cover_path: None,
            cover_pixels: None,
            has_media: true,
        });
        let mut k1 = String::new();
        media.write_cache_key(&mut k1);

        crate::widgets::set_chip_media_hint(crate::widgets::MediaChipHint {
            playing: true,
            progress: 0.6,
            cover_path: None,
            cover_pixels: None,
            has_media: true,
        });
        let mut k2 = String::new();
        media.write_cache_key(&mut k2);

        assert_ne!(k1, k2);
    }
}
