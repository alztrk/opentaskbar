//! Native DirectComposition + Direct2D + DirectWrite visual engine for the floating dock.
//!
//! All icons, pill caps, shell glyphs, indicator dots, and tooltips are drawn ONCE onto static
//! `IDCompositionSurface` textures. During 60-180 FPS fisheye, bounce, and drag animations, only
//! 2D affine transforms (`IDCompositionMatrixTransform`) are updated and committed to DWM
//! (`IDCompositionDevice::Commit`) with zero D3D11 draw calls and ~0-1% GPU usage.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Mutex;

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::SurfaceBackground;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_BITMAP_PROPERTIES1,
    D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_CAP_STYLE_ROUND,
    D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LINE_JOIN_ROUND,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES1,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext,
    ID2D1Factory1, ID2D1LinearGradientBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionMatrixTransform,
    IDCompositionRectangleClip, IDCompositionSurface, IDCompositionTarget, IDCompositionVisual,
    IDCompositionVisual3,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::{HSTRING, Interface, Result};
use windows_numerics::{Matrix3x2, Vector2};

use crate::design::{
    self, Rgb,
    icons::{self, AppIcon},
};
use crate::platform::icon_cache::RawIcon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipIcon {
    Folder,
    Grid,
    Sparkle,
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
}

/// Exact C# 1-to-1 dock widget specs ported from `Widgets/*.xaml` and `Widgets/*.cs`.
#[derive(Clone, Debug)]
pub enum WidgetChipSpec {
    /// C# `ClockWidget.xaml`: `[ClockIcon 15px] [15px Bold Time] [11.5px Date / 10.5px Weekday]`
    Clock {
        time: String,
        date: String,
        weekday: String,
    },
    /// C# `WeatherWidget.cs`: `[Icon 20px] [18px Bold Temp] [Place / Condition] | [Today / High/Low]` or unavailable text
    Weather {
        available: bool,
        icon_color: Rgb,
        is_rain: bool,
        temp: String,
        place: String,
        condition: String,
        today_range: Option<String>,
    },
    /// C# `DayProgressWidget.cs`: `[22px RingMeter (WidgetBlue)] [13px SemiBold %] [11.5px "day"]`
    DayProgress {
        ratio: f32,
        percent: String,
        unit: String,
    },
    /// C# `TimedWidgetTile.xaml` & `FocusWidget.xaml`: `[Icon 16px] [13px Tabular Time] [26x26 Play/Pause]`
    Timed {
        is_focus: bool,
        running: bool,
        text: String,
    },
    /// C# `SystemStatsWidget.xaml`: `[CPU / 14%] | [RAM / 48%]` with threshold foreground colors
    SystemStats {
        cpu_text: String,
        cpu_color: Rgb,
        ram_text: String,
        ram_color: Rgb,
    },
    /// C# `NetSpeedWidget.cs`: `[Wifi 15px] [↓ green speed / ↑ blue speed]`
    NetSpeed { down: String, up: String },
    /// C# `BatteryWidget.cs`: full-card horizontal liquid fill gradient + shadowed white `%` + bolt
    BatteryLiquid {
        present: bool,
        percent: u32,
        charging: bool,
        wave_phase: f32,
    },
    /// C# `BluetoothBatteryWidget.cs`: horizontal connected device tiles (`device-kind icon + %`) or centered `—`
    Bluetooth {
        /// `(kind, percent, low_warning, full_charge)`
        devices: Vec<(
            crate::widgets::telemetry::BluetoothDeviceKind,
            u32,
            bool,
            bool,
        )>,
    },
    /// C# `StorageWidget.cs`: `"C: 213 / 476 GB | D: 1.2 / 1.8 TB"` (clean typographic, no bars)
    Storage { drives: Vec<(String, String)> },
    /// C# `MediaWidget.xaml`: `[29x29 tinted Art] [Title / Artist] [Prev | PlayPause | Next] [Volume]`
    Media {
        title: String,
        artist: String,
        muted: bool,
    },
    /// C# `AudioWidget.xaml`: `[Speaker 16px] [12.5px SemiBold 67%]` (scroll adjusts, click opens mixer)
    Audio { volume: u32, muted: bool },
    /// C# `QuickSettingsWidget.xaml`: 3 compact `27x27` toggle pills (`Wi-Fi`, `Bluetooth`, `Night Light/Focus`)
    QuickSettings { pills: [(String, bool); 3] },
    /// C# `KeyboardLayoutWidget.cs`: `[Keyboard 13px] [12px SemiBold TR/EN]`
    Keyboard { code: String },
    /// C# `AiUsageWidget.cs` dual-bar layout (Claude, Codex, single-row Cursor) with real `Brand.xaml` logo
    AiDual {
        provider_id: &'static str,
        brand_color: Option<Rgb>,
        /// `(short_label, used_01, bar_color, reset_text)`
        rows: Vec<AiQuotaRow>,
    },
    /// C# `AiUsageWidget.cs` triple-column OpenCode layout (`1h | 7d | 30d`)
    AiOpenCode {
        /// `(short_label, pct_text, used_01, bar_color, reset_text)`
        columns: Vec<(String, String, f32, Rgb, String)>,
    },
    /// C# `AiUsageWidget.cs` Copilot inline layout (`Logo + Copilot | Value/Meta + 31x4 mini bar`)
    AiCopilot {
        value_text: String,
        meta_text: String,
        used: f32,
        bar_color: Rgb,
    },
    /// C# `AiUsageWidget.cs` DeepSeek / OpenRouter tinted balance card (`29x29 circular tinted well + Name / 16px Balance`)
    AiBalance {
        provider_id: &'static str,
        name: &'static str,
        brand_color: Rgb,
        value_text: String,
    },
    /// Clean borderless icon + two-line helper (Stacks, Groups)
    Simple {
        icon: ChipIcon,
        icon_color: Rgb,
        top: String,
        bottom: String,
    },
}

type AiQuotaRow = (String, f32, Rgb, String);

impl WidgetChipSpec {
    /// Writes the spec into a reusable buffer so per-widget refresh does not allocate a new
    /// String for every Debug-formatted chip spec.
    pub fn write_cache_key(&self, out: &mut String) {
        out.clear();
        let _ = write!(out, "{self:?}");
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

fn color_rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: a.clamp(0.0, 1.0),
    }
}

impl DcompEngine {
    pub fn new(hwnd: usize) -> Result<Self> {
        let mut d3d_device: Option<ID3D11Device> = None;
        let feature_levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
        ];
        // SAFETY: standard D3D11 hardware device creation with BGRA support for Direct2D/DComp.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                None,
            )?;
        }
        let d3d_device = d3d_device.unwrap();
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;

        let d2d_factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };

        // Must create `d2d_device` from `d2d_factory` so stroke styles and device contexts share
        // the exact same factory (otherwise Direct2D fails with D2DERR_WRONG_FACTORY on EndDraw).
        let d2d_device: ID2D1Device = unsafe { d2d_factory.CreateDevice(&dxgi_device)? };

        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let font_name = HSTRING::from(design::fonts::ui_family_list().as_str());
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
        let pal = design::palette(is_dark);
        self.draw_surface(width_px, height_px, |dc, w, h| unsafe {
            let stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: color_rgba(pill_rgb.0, pill_rgb.1, pill_rgb.2, 0.94),
                },
                D2D1_GRADIENT_STOP {
                    position: 0.55,
                    color: color_rgba(pill_rgb.0, pill_rgb.1, pill_rgb.2, 0.72),
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: color_rgba(pill_rgb.0, pill_rgb.1, pill_rgb.2, 0.0),
                },
            ];
            let coll = dc.CreateGradientStopCollection(
                &stops,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                D2D1_EXTEND_MODE_CLAMP,
                D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
            )?;
            let (start_x, end_x) = if is_left { (0.0, w) } else { (w, 0.0) };
            let grad = dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 { X: start_x, Y: 0.0 },
                    endPoint: Vector2 { X: end_x, Y: 0.0 },
                },
                None,
                &coll,
            )?;

            let pad_v = (1.0 * dpi).max(1.0);
            let r = (radius_px - dpi).clamp(2.0, (h - pad_v * 2.0) * 0.5);
            // Clip a wider rounded rect to keep both left (`19,0,0,19`) and right (`0,19,19,0`)
            // scroll overlays flush with the outer rounded corners of the dock pill (exact C# parity).
            dc.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: 0.0,
                    top: pad_v,
                    right: w,
                    bottom: h - pad_v,
                },
                windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            let rr_rect = if is_left {
                D2D_RECT_F {
                    left: 0.0,
                    top: pad_v,
                    right: w + r * 2.0,
                    bottom: h - pad_v,
                }
            } else {
                D2D_RECT_F {
                    left: -(r * 2.0),
                    top: pad_v,
                    right: w,
                    bottom: h - pad_v,
                }
            };
            dc.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: rr_rect,
                    radiusX: r,
                    radiusY: r,
                },
                &grad,
            );
            dc.PopAxisAlignedClip();

            // Crisp hover highlight pill behind the chevron
            let pill_sz = (22.0 * dpi).min(h - 8.0 * dpi).max(14.0);
            let cx = if is_left { w * 0.40 } else { w * 0.60 };
            let cy = h * 0.5;
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
                        rect: D2D_RECT_F {
                            left: cx - pill_sz * 0.5,
                            top: cy - pill_sz * 0.5,
                            right: cx + pill_sz * 0.5,
                            bottom: cy + pill_sz * 0.5,
                        },
                        radiusX: 7.0 * dpi,
                        radiusY: 7.0 * dpi,
                    },
                    &hb,
                );
            }

            let glyph = if is_left { "\u{E76B}" } else { "\u{E76C}" };
            let ink = if hovered {
                pal.accent
            } else {
                pal.text_primary
            };
            draw_fluent_str_d2d(
                &self.dwrite_factory,
                dc,
                glyph,
                D2D_RECT_F {
                    left: cx - pill_sz * 0.5,
                    top: cy - pill_sz * 0.5,
                    right: cx + pill_sz * 0.5,
                    bottom: cy + pill_sz * 0.5,
                },
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
                &HSTRING::from(design::fonts::ui_family_list().as_str()),
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

    /// Renders one of the 1-to-1 C# dock widget modules at automatic `height_dips`
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
        let pal = design::palette_with_border(is_dark, border_rgb);
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
        // Secondary captions sit at 11 DIP (not 10) so the second line of two-line cards stays readable.
        // Formats come from the engine cache; a redraw only AddRefs them.
        let fmt_18_b_l = self.cached_text_format(design::fonts::scaled_size(18.0), true, 1)?;
        let fmt_15_b_l = self.cached_text_format(design::fonts::scaled_size(15.0), true, 1)?;
        let fmt_13_b_c = self.cached_text_format(design::fonts::scaled_size(13.0), true, 0)?;
        let fmt_13_b_l = self.cached_text_format(design::fonts::scaled_size(13.0), true, 1)?;
        let fmt_125_b_l = self.cached_text_format(design::fonts::scaled_size(12.0), true, 1)?;
        let fmt_12_b_l = self.cached_text_format(design::fonts::scaled_size(12.0), true, 1)?;
        let fmt_12_c = self.cached_text_format(design::fonts::scaled_size(12.0), false, 0)?;
        let fmt_115_b_l = self.cached_text_format(design::fonts::scaled_size(11.0), true, 1)?;
        let fmt_115_b_c = self.cached_text_format(design::fonts::scaled_size(11.0), true, 0)?;
        let fmt_115_l = self.cached_text_format(design::fonts::scaled_size(11.0), false, 1)?;
        let fmt_11_b_l = self.cached_text_format(design::fonts::scaled_size(11.0), true, 1)?;
        let fmt_105_b_r = self.cached_text_format(design::fonts::scaled_size(11.0), true, 2)?;
        let fmt_105_l = self.cached_text_format(design::fonts::scaled_size(11.0), false, 1)?;
        let fmt_10_l = self.cached_text_format(design::fonts::scaled_size(11.0), false, 1)?;
        let fmt_95_b_l = self.cached_text_format(design::fonts::scaled_size(10.0), true, 1)?;
        let fmt_95_l = self.cached_text_format(design::fonts::scaled_size(10.0), false, 1)?;
        let fmt_9_l = self.cached_text_format(design::fonts::scaled_size(10.0), false, 1)?;
        let _fmt_9_b_l = self.cached_text_format(design::fonts::scaled_size(9.0), true, 1)?;
        let fmt_85_l = self.cached_text_format(design::fonts::scaled_size(9.0), false, 1)?;
        let _fmt_85_b_l = self.cached_text_format(design::fonts::scaled_size(9.0), true, 1)?;

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

            let active_sub = match interaction {
                WidgetInteraction::Idle => None,
                WidgetInteraction::Hovered(s) | WidgetInteraction::Pressed(s) => s,
            };
            let cy = height_dips * 0.5;
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
                dc.FillRectangle(
                    &D2D_RECT_F {
                        left: x,
                        top,
                        right: x + 1.0,
                        bottom: top + h,
                    },
                    &div_brush,
                );
            };

            match spec {
                WidgetChipSpec::Clock {
                    time,
                    date,
                    weekday,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // ClockWidget.xaml: [IconClock 15px, margin 0,0,8,0] [Time 15px Bold] + 9px + [Date 11.5px / DayOfWeek 10.5px]
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        "\u{E823}",
                        D2D_RECT_F {
                            left: 9.0,
                            top: cy - 8.5,
                            right: 25.0,
                            bottom: cy + 8.5,
                        },
                        15.0,
                        pal.text_secondary,
                        0.92,
                    )?;
                    draw_text(
                        time,
                        &fmt_15_b_l,
                        D2D_RECT_F {
                            left: 32.0,
                            top: 0.0,
                            right: 82.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                    );
                    let dx = 82.0;
                    let mid = height_dips * 0.51;
                    draw_text(
                        date,
                        &fmt_115_b_l,
                        D2D_RECT_F {
                            left: dx,
                            top: (mid - 14.0).max(2.0),
                            right: width_dips - 8.0,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                    );
                    draw_text(
                        weekday,
                        &fmt_105_l,
                        D2D_RECT_F {
                            left: dx,
                            top: mid - 0.5,
                            right: width_dips - 8.0,
                            bottom: (mid + 13.5).min(height_dips - 2.0),
                        },
                        &s_brush,
                    );
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
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    if !*available {
                        draw_text(
                            condition,
                            &fmt_12_c,
                            D2D_RECT_F {
                                left: 10.0,
                                top: 0.0,
                                right: width_dips - 10.0,
                                bottom: height_dips,
                            },
                            &s_brush,
                        );
                    } else {
                        // WeatherWidget.cs: [Icon 20px] [18px Bold Temp] [Place / Condition] | [Today / High/Low]
                        let glyph = if *is_rain { "\u{E9C4}" } else { "\u{E753}" };
                        draw_fluent_str_d2d(
                            &self.dwrite_factory,
                            dc,
                            glyph,
                            D2D_RECT_F {
                                left: 10.0,
                                top: cy - 10.0,
                                right: 30.0,
                                bottom: cy + 10.0,
                            },
                            19.0,
                            *icon_color,
                            0.98,
                        )?;
                        draw_text(
                            temp,
                            &fmt_18_b_l,
                            D2D_RECT_F {
                                left: 37.0,
                                top: 0.0,
                                right: 82.0,
                                bottom: height_dips,
                            },
                            &p_brush,
                        );
                        let px = 83.0;
                        let pr = if today_range.is_some() {
                            width_dips - 82.0
                        } else {
                            width_dips - 10.0
                        };
                        let mid = height_dips * 0.50;
                        draw_text(
                            place,
                            &fmt_11_b_l,
                            D2D_RECT_F {
                                left: px,
                                top: (mid - 14.0).max(2.0),
                                right: pr,
                                bottom: mid + 1.0,
                            },
                            &p_brush,
                        );
                        draw_text(
                            condition,
                            &fmt_10_l,
                            D2D_RECT_F {
                                left: px,
                                top: mid - 0.5,
                                right: pr,
                                bottom: (mid + 13.5).min(height_dips - 2.0),
                            },
                            &s_brush,
                        );
                        if let Some(range) = today_range {
                            let sep_x = width_dips - 76.0;
                            draw_vdiv(sep_x, 20.0);
                            draw_text(
                                &loc::t("Today"),
                                &fmt_9_l,
                                D2D_RECT_F {
                                    left: sep_x + 8.0,
                                    top: (mid - 13.5).max(2.0),
                                    right: width_dips - 8.0,
                                    bottom: mid,
                                },
                                &s_brush,
                            );
                            draw_text(
                                range,
                                &fmt_115_b_l,
                                D2D_RECT_F {
                                    left: sep_x + 8.0,
                                    top: mid - 0.5,
                                    right: width_dips - 8.0,
                                    bottom: (mid + 14.0).min(height_dips - 2.0),
                                },
                                &p_brush,
                            );
                        }
                    }
                }
                WidgetChipSpec::DayProgress {
                    ratio,
                    percent,
                    unit,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // DayProgressWidget.cs: [RingMeter 22x22 Stroke=2.8 WidgetBlue] + 7px + [13px SemiBold %] + 5px + [11.5px "day"]
                    let rx = 21.0;
                    let rad = 9.5_f32.min(height_dips * 0.30);
                    let r_brush = dc.CreateSolidColorBrush(
                        &color_rgba(pal.widget_blue.0, pal.widget_blue.1, pal.widget_blue.2, 1.0),
                        None,
                    )?;
                    let ell = D2D1_ELLIPSE {
                        point: Vector2 { X: rx, Y: cy },
                        radiusX: rad,
                        radiusY: rad,
                    };
                    dc.DrawEllipse(&ell, &tr_brush, 2.8, None);
                    let clamped = ratio.clamp(0.0, 1.0);
                    if clamped >= 0.995 {
                        dc.DrawEllipse(&ell, &r_brush, 2.8, None);
                    } else if clamped > 0.005 {
                        let sweep = clamped * std::f32::consts::TAU;
                        let end_ang = -std::f32::consts::FRAC_PI_2 + sweep;
                        let geom = self.d2d_factory.CreatePathGeometry()?;
                        let sink = geom.Open()?;
                        sink.BeginFigure(Vector2 { X: rx, Y: cy - rad }, D2D1_FIGURE_BEGIN_HOLLOW);
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: Vector2 {
                                X: rx + end_ang.cos() * rad,
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
                        dc.DrawGeometry(&geom, &r_brush, 2.8, Some(&stroke));
                    }
                    draw_text(
                        percent,
                        &fmt_13_b_l,
                        D2D_RECT_F {
                            left: 39.0,
                            top: 0.0,
                            right: 68.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                    );
                    draw_text(
                        unit,
                        &fmt_115_l,
                        D2D_RECT_F {
                            left: 69.0,
                            top: 0.5,
                            right: width_dips - 8.0,
                            bottom: height_dips,
                        },
                        &s_brush,
                    );
                }
                WidgetChipSpec::Timed {
                    is_focus,
                    running,
                    text,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // FocusWidget.xaml / TimedWidgetTile.xaml: [Icon 16px] [13px Tabular Time] [26x26 Play/Pause]
                    let icon_glyph = if *is_focus { "\u{E7C8}" } else { "\u{E916}" };
                    let icon_col = if *running {
                        pal.selected_fg
                    } else {
                        pal.text_secondary
                    };
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        icon_glyph,
                        D2D_RECT_F {
                            left: 9.0,
                            top: cy - 8.5,
                            right: 26.0,
                            bottom: cy + 8.5,
                        },
                        15.0,
                        icon_col,
                        0.95,
                    )?;
                    let btn_sz = 26.0;
                    let btn_l = width_dips - btn_sz - 8.0;
                    let btn_t = cy - btn_sz * 0.5;
                    draw_text(
                        text,
                        &fmt_13_b_l,
                        D2D_RECT_F {
                            left: 32.0,
                            top: 0.0,
                            right: btn_l - 4.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                    );
                    let btn_rect = D2D_RECT_F {
                        left: btn_l,
                        top: btn_t,
                        right: btn_l + btn_sz,
                        bottom: btn_t + btn_sz,
                    };
                    let btn_rr = D2D1_ROUNDED_RECT {
                        rect: btn_rect,
                        radiusX: 7.0,
                        radiusY: 7.0,
                    };
                    let sub_hov = active_sub == Some(1);
                    if *running {
                        dc.FillRoundedRectangle(&btn_rr, &sel_fill_b);
                        dc.DrawRoundedRectangle(&btn_rr, &sel_stroke_b, 1.0, None);
                    } else if sub_hov {
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
                    let play_glyph = if *running { "\u{E769}" } else { "\u{E768}" };
                    let play_col = if *running {
                        pal.selected_fg
                    } else {
                        pal.text_primary
                    };
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        play_glyph,
                        btn_rect,
                        11.5,
                        play_col,
                        0.96,
                    )?;
                }
                WidgetChipSpec::SystemStats {
                    cpu_text,
                    cpu_color,
                    ram_text,
                    ram_color,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // SystemStatsWidget.xaml: [CPU 10px / 14% 12px] | [RAM 10px / 48% 12px]
                    let mid_x = width_dips * 0.5;
                    let mid_y = height_dips * 0.46;
                    let cpu_b = dc.CreateSolidColorBrush(
                        &color_rgba(cpu_color.0, cpu_color.1, cpu_color.2, 0.98),
                        None,
                    )?;
                    let ram_b = dc.CreateSolidColorBrush(
                        &color_rgba(ram_color.0, ram_color.1, ram_color.2, 0.98),
                        None,
                    )?;
                    draw_text(
                        "CPU",
                        &fmt_10_l,
                        D2D_RECT_F {
                            left: 9.0,
                            top: (mid_y - 13.0).max(2.0),
                            right: mid_x - 4.0,
                            bottom: mid_y,
                        },
                        &s_brush,
                    );
                    draw_text(
                        cpu_text,
                        &fmt_12_b_l,
                        D2D_RECT_F {
                            left: 9.0,
                            top: mid_y - 1.0,
                            right: mid_x - 4.0,
                            bottom: (mid_y + 15.0).min(height_dips - 2.0),
                        },
                        &cpu_b,
                    );
                    draw_vdiv(mid_x - 0.5, 20.0);
                    draw_text(
                        "RAM",
                        &fmt_10_l,
                        D2D_RECT_F {
                            left: mid_x + 6.0,
                            top: (mid_y - 13.0).max(2.0),
                            right: width_dips - 6.0,
                            bottom: mid_y,
                        },
                        &s_brush,
                    );
                    draw_text(
                        ram_text,
                        &fmt_12_b_l,
                        D2D_RECT_F {
                            left: mid_x + 6.0,
                            top: mid_y - 1.0,
                            right: width_dips - 6.0,
                            bottom: (mid_y + 15.0).min(height_dips - 2.0),
                        },
                        &ram_b,
                    );
                }
                WidgetChipSpec::NetSpeed { down, up } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // NetSpeedWidget.cs: [Wifi 15px Secondary] + [↓ WidgetGreen + 11.5px] / [↑ WidgetBlue + 11.5px]
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        "\u{E701}",
                        D2D_RECT_F {
                            left: 9.0,
                            top: cy - 8.5,
                            right: 25.0,
                            bottom: cy + 8.5,
                        },
                        14.5,
                        pal.text_secondary,
                        0.90,
                    )?;
                    let mid = height_dips * 0.5;
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        "\u{E74B}",
                        D2D_RECT_F {
                            left: 31.0,
                            top: (mid - 14.0).max(2.0),
                            right: 43.0,
                            bottom: mid,
                        },
                        10.0,
                        pal.success,
                        0.98,
                    )?;
                    draw_text(
                        down,
                        &fmt_115_b_l,
                        D2D_RECT_F {
                            left: 45.0,
                            top: (mid - 15.0).max(2.0),
                            right: width_dips - 6.0,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                    );
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        "\u{E74A}",
                        D2D_RECT_F {
                            left: 31.0,
                            top: mid,
                            right: 43.0,
                            bottom: (mid + 14.0).min(height_dips - 2.0),
                        },
                        10.0,
                        pal.widget_blue,
                        0.98,
                    )?;
                    draw_text(
                        up,
                        &fmt_115_b_l,
                        D2D_RECT_F {
                            left: 45.0,
                            top: mid - 1.0,
                            right: width_dips - 6.0,
                            bottom: (mid + 15.0).min(height_dips - 2.0),
                        },
                        &p_brush,
                    );
                }
                WidgetChipSpec::BatteryLiquid {
                    present,
                    percent,
                    charging,
                    wave_phase,
                } => {
                    // Exact C# BatteryWidget.cs horizontal liquid-fill card with scrolling sinusoidal
                    // meniscus wave + -22deg skewed left-to-right charging sheen!
                    if !*present {
                        draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                        draw_text(
                            "—",
                            &fmt_13_b_c,
                            D2D_RECT_F {
                                left: 4.0,
                                top: 0.0,
                                right: width_dips - 4.0,
                                bottom: height_dips,
                            },
                            &s_brush,
                        );
                    } else {
                        let card_rr = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: 0.0,
                                top: 0.0,
                                right: width_dips,
                                bottom: height_dips,
                            },
                            radiusX: 10.0,
                            radiusY: 10.0,
                        };
                        let base_track = dc.CreateSolidColorBrush(
                            &if is_dark {
                                color_rgba(0xff, 0xff, 0xff, 0.09)
                            } else {
                                color_rgba(0x00, 0x00, 0x00, 0.07)
                            },
                            None,
                        )?;
                        dc.FillRoundedRectangle(&card_rr, &base_track);

                        // Exact C# BatteryWidget.PaletteForLevel vertical gradient stops
                        let (top_c, bot_c) = if *percent <= 20 {
                            ((0xb2, 0x60, 0x67), (0x98, 0x50, 0x5a))
                        } else if *percent <= 50 {
                            ((0xb9, 0x95, 0x63), (0x96, 0x76, 0x48))
                        } else {
                            ((0x58, 0xa8, 0x9c), (0x3e, 0x82, 0x7e))
                        };
                        let stops = [
                            D2D1_GRADIENT_STOP {
                                position: 0.0,
                                color: color_rgba(top_c.0, top_c.1, top_c.2, 0.96),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 1.0,
                                color: color_rgba(bot_c.0, bot_c.1, bot_c.2, 0.96),
                            },
                        ];
                        let stop_col = dc.CreateGradientStopCollection(
                            &stops,
                            D2D1_COLOR_SPACE_SRGB,
                            D2D1_COLOR_SPACE_SRGB,
                            D2D1_BUFFER_PRECISION_8BPC_UNORM,
                            D2D1_EXTEND_MODE_CLAMP,
                            D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
                        )?;
                        let liq_b = dc.CreateLinearGradientBrush(
                            &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                                startPoint: Vector2 { X: 0.0, Y: 0.0 },
                                endPoint: Vector2 {
                                    X: 0.0,
                                    Y: height_dips,
                                },
                            },
                            None,
                            &stop_col,
                        )?;

                        let edge_x = width_dips * ((*percent).clamp(2, 100) as f32 / 100.0);
                        let card_mask: windows::Win32::Graphics::Direct2D::ID2D1Geometry =
                            self.d2d_factory.CreateRoundedRectangleGeometry(&card_rr)?.into();
                        let layer_params = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS1 {
                            contentBounds: card_rr.rect,
                            geometricMask: std::mem::ManuallyDrop::new(Some(card_mask)),
                            maskAntialiasMode:
                                windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                            maskTransform: Matrix3x2::identity(),
                            opacity: 1.0,
                            opacityBrush: std::mem::ManuallyDrop::new(None),
                            layerOptions:
                                windows::Win32::Graphics::Direct2D::D2D1_LAYER_OPTIONS1_NONE,
                        };
                        dc.PushLayer(&layer_params, None);

                        // Build liquid geometry (2-period sinusoidal meniscus when charging & < 100%).
                        let liquid_geom: windows::Win32::Graphics::Direct2D::ID2D1Geometry =
                            if *charging && *percent < 100 {
                                let amp = 3.2_f32.min(edge_x * 0.35);
                                let half = height_dips * 0.5;
                                let y0 = -(wave_phase.clamp(0.0, 1.0)) * height_dips;
                                let path = self.d2d_factory.CreatePathGeometry()?;
                                let sink = path.Open()?;
                                sink.BeginFigure(
                                    Vector2 { X: 0.0, Y: y0 },
                                    windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_FILLED,
                                );
                                sink.AddLine(Vector2 { X: edge_x, Y: y0 });
                                // Period 1: y0 -> y0 + height_dips
                                sink.AddBezier(&windows::Win32::Graphics::Direct2D::Common::D2D1_BEZIER_SEGMENT {
                                    point1: Vector2 {
                                        X: edge_x + amp,
                                        Y: y0 + half * 0.36,
                                    },
                                    point2: Vector2 {
                                        X: edge_x + amp,
                                        Y: y0 + half * 0.64,
                                    },
                                    point3: Vector2 {
                                        X: edge_x,
                                        Y: y0 + half,
                                    },
                                });
                                sink.AddBezier(&windows::Win32::Graphics::Direct2D::Common::D2D1_BEZIER_SEGMENT {
                                    point1: Vector2 {
                                        X: edge_x - amp,
                                        Y: y0 + half + half * 0.36,
                                    },
                                    point2: Vector2 {
                                        X: edge_x - amp,
                                        Y: y0 + half + half * 0.64,
                                    },
                                    point3: Vector2 {
                                        X: edge_x,
                                        Y: y0 + height_dips,
                                    },
                                });
                                // Period 2: y0 + height_dips -> y0 + 2 * height_dips (seamless loop)
                                sink.AddBezier(&windows::Win32::Graphics::Direct2D::Common::D2D1_BEZIER_SEGMENT {
                                    point1: Vector2 {
                                        X: edge_x + amp,
                                        Y: y0 + height_dips + half * 0.36,
                                    },
                                    point2: Vector2 {
                                        X: edge_x + amp,
                                        Y: y0 + height_dips + half * 0.64,
                                    },
                                    point3: Vector2 {
                                        X: edge_x,
                                        Y: y0 + height_dips + half,
                                    },
                                });
                                sink.AddBezier(&windows::Win32::Graphics::Direct2D::Common::D2D1_BEZIER_SEGMENT {
                                    point1: Vector2 {
                                        X: edge_x - amp,
                                        Y: y0 + height_dips + half + half * 0.36,
                                    },
                                    point2: Vector2 {
                                        X: edge_x - amp,
                                        Y: y0 + height_dips + half + half * 0.64,
                                    },
                                    point3: Vector2 {
                                        X: edge_x,
                                        Y: y0 + height_dips * 2.0,
                                    },
                                });
                                sink.AddLine(Vector2 {
                                    X: 0.0,
                                    Y: y0 + height_dips * 2.0,
                                });
                                sink.EndFigure(
                                    windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_CLOSED,
                                );
                                sink.Close()?;
                                path.into()
                            } else {
                                self.d2d_factory
                                    .CreateRectangleGeometry(&D2D_RECT_F {
                                        left: 0.0,
                                        top: 0.0,
                                        right: edge_x,
                                        bottom: height_dips,
                                    })?
                                    .into()
                            };

                        dc.FillGeometry(&liquid_geom, &liq_b, None);

                        // Exact C# -22deg skewed left-to-right charging sheen clipped strictly to liquid fill.
                        if *charging {
                            let sheen_w = 54.0_f32;
                            let t = (wave_phase / 0.875).clamp(0.0, 1.0);
                            let eased = if t < 0.5 {
                                4.0 * t * t * t
                            } else {
                                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                            };
                            let start_x = -sheen_w - 28.0;
                            let end_x = edge_x + 28.0;
                            let sx = start_x + (end_x - start_x) * eased;
                            let sheen_stops = [
                                D2D1_GRADIENT_STOP {
                                    position: 0.0,
                                    color: color_rgba(0xff, 0xff, 0xff, 0.0),
                                },
                                D2D1_GRADIENT_STOP {
                                    position: 0.30,
                                    color: color_rgba(0xff, 0xff, 0xff, 0.09),
                                },
                                D2D1_GRADIENT_STOP {
                                    position: 0.50,
                                    color: color_rgba(0xff, 0xff, 0xff, 0.21),
                                },
                                D2D1_GRADIENT_STOP {
                                    position: 0.70,
                                    color: color_rgba(0xff, 0xff, 0xff, 0.09),
                                },
                                D2D1_GRADIENT_STOP {
                                    position: 1.0,
                                    color: color_rgba(0xff, 0xff, 0xff, 0.0),
                                },
                            ];
                            let sheen_sc = dc.CreateGradientStopCollection(
                                &sheen_stops,
                                D2D1_COLOR_SPACE_SRGB,
                                D2D1_COLOR_SPACE_SRGB,
                                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                                D2D1_EXTEND_MODE_CLAMP,
                                D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
                            )?;
                            let sheen_b = dc.CreateLinearGradientBrush(
                                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                                    startPoint: Vector2 { X: sx, Y: 0.0 },
                                    endPoint: Vector2 {
                                        X: sx + sheen_w,
                                        Y: height_dips * 0.40,
                                    },
                                },
                                None,
                                &sheen_sc,
                            )?;
                            dc.FillGeometry(&liquid_geom, &sheen_b, None);
                        }

                        dc.PopLayer();
                        // Drop ManuallyDrop geometricMask cleanly after PopLayer.
                        let _ = std::mem::ManuallyDrop::into_inner(layer_params.geometricMask);

                        // Hover/press overlay above liquid fill (exact C# _hoverOverlay/_pressOverlay)
                        draw_chip_overlay(dc, width_dips, height_dips, is_dark, interaction)?;

                        let label = format!("{percent}%");
                        let sh_b = dc.CreateSolidColorBrush(&color_rgba(0, 0, 0, 0.35), None)?;
                        let wh_b =
                            dc.CreateSolidColorBrush(&color_rgba(0xff, 0xff, 0xff, 0.98), None)?;
                        if *charging {
                            // Crisp native Fluent bolt (`IconZap` \u{E945}) + drop shadow left of `%` (exact C# `BatteryWidget.cs`)
                            let bolt_w = 13.0;
                            let text_w = (label.len() as f32 * 8.2).max(24.0);
                            let total_w = bolt_w + text_w;
                            let start_x = (width_dips - total_w) * 0.5;
                            draw_fluent_str_d2d(
                                &self.dwrite_factory,
                                dc,
                                "\u{E945}",
                                D2D_RECT_F {
                                    left: start_x,
                                    top: cy - 6.0 + 1.0,
                                    right: start_x + 12.0,
                                    bottom: cy + 6.0 + 1.0,
                                },
                                11.0,
                                (0, 0, 0),
                                0.35,
                            )?;
                            draw_fluent_str_d2d(
                                &self.dwrite_factory,
                                dc,
                                "\u{E945}",
                                D2D_RECT_F {
                                    left: start_x,
                                    top: cy - 6.0,
                                    right: start_x + 12.0,
                                    bottom: cy + 6.0,
                                },
                                11.0,
                                (0xff, 0xff, 0xff),
                                0.98,
                            )?;
                            draw_text(
                                &label,
                                &fmt_13_b_l,
                                D2D_RECT_F {
                                    left: start_x + bolt_w,
                                    top: 1.0,
                                    right: width_dips - 4.0,
                                    bottom: height_dips + 1.0,
                                },
                                &sh_b,
                            );
                            draw_text(
                                &label,
                                &fmt_13_b_l,
                                D2D_RECT_F {
                                    left: start_x + bolt_w,
                                    top: 0.0,
                                    right: width_dips - 4.0,
                                    bottom: height_dips,
                                },
                                &wh_b,
                            );
                        } else {
                            draw_text(
                                &label,
                                &fmt_13_b_c,
                                D2D_RECT_F {
                                    left: 4.0,
                                    top: 1.0,
                                    right: width_dips - 4.0,
                                    bottom: height_dips + 1.0,
                                },
                                &sh_b,
                            );
                            draw_text(
                                &label,
                                &fmt_13_b_c,
                                D2D_RECT_F {
                                    left: 4.0,
                                    top: 0.0,
                                    right: width_dips - 4.0,
                                    bottom: height_dips,
                                },
                                &wh_b,
                            );
                        }
                    }
                }
                WidgetChipSpec::Bluetooth { devices } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    if devices.is_empty() {
                        // Exact C# BluetoothBatteryWidget.cs: centered "—" (`11.5px SemiBold TextSecondary`)
                        draw_text(
                            "—",
                            &fmt_115_b_c,
                            D2D_RECT_F {
                                left: 6.0,
                                top: 0.0,
                                right: width_dips - 6.0,
                                bottom: height_dips,
                            },
                            &s_brush,
                        );
                    } else {
                        let count = devices.len().min(3);
                        let tile_w = (width_dips - 12.0) / count as f32;
                        for (i, (kind, pct, low, full)) in devices.iter().take(3).enumerate() {
                            let tx = 6.0 + i as f32 * tile_w;
                            if i > 0 {
                                draw_vdiv(tx - 2.0, 20.0);
                            }
                            draw_fluent_str_d2d(
                                &self.dwrite_factory,
                                dc,
                                kind.fluent_glyph(),
                                D2D_RECT_F {
                                    left: tx + 3.0,
                                    top: cy - 8.0,
                                    right: tx + 19.0,
                                    bottom: cy + 8.0,
                                },
                                14.5,
                                pal.text_secondary,
                                0.94,
                            )?;
                            let val_col = if *low {
                                pal.warning
                            } else if *full {
                                (0x34, 0xd3, 0x99) // C# FullChargeBrush #34D399
                            } else {
                                pal.text_primary
                            };
                            let vb = dc.CreateSolidColorBrush(
                                &color_rgba(val_col.0, val_col.1, val_col.2, 0.98),
                                None,
                            )?;
                            draw_text(
                                &format!("{pct}%"),
                                &fmt_12_b_l,
                                D2D_RECT_F {
                                    left: tx + 22.0,
                                    top: 0.0,
                                    right: tx + tile_w - 2.0,
                                    bottom: height_dips,
                                },
                                &vb,
                            );
                        }
                    }
                }
                WidgetChipSpec::Storage { drives } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // StorageWidget.cs: "C: 213 / 476 GB | D: 1.2 / 1.8 TB" (all fixed drives up to 3)
                    if drives.is_empty() {
                        draw_text(
                            "—",
                            &fmt_115_b_c,
                            D2D_RECT_F {
                                left: 10.0,
                                top: 0.0,
                                right: width_dips - 10.0,
                                bottom: height_dips,
                            },
                            &s_brush,
                        );
                    } else {
                        let count = drives.len().min(3);
                        let seg_w = (width_dips - 20.0) / count as f32;
                        for (i, (letter, pair)) in drives.iter().take(3).enumerate() {
                            let sx = 10.0 + i as f32 * seg_w;
                            if i > 0 {
                                draw_vdiv(sx - 6.0, 13.0);
                            }
                            draw_text(
                                letter,
                                &fmt_11_b_l,
                                D2D_RECT_F {
                                    left: sx,
                                    top: 0.0,
                                    right: sx + 17.0,
                                    bottom: height_dips,
                                },
                                &s_brush,
                            );
                            draw_text(
                                pair,
                                &fmt_115_b_l,
                                D2D_RECT_F {
                                    left: sx + 18.0,
                                    top: 0.0,
                                    right: sx + seg_w - 4.0,
                                    bottom: height_dips,
                                },
                                &p_brush,
                            );
                        }
                    }
                }
                WidgetChipSpec::Media {
                    title,
                    artist,
                    muted,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // MediaWidget.xaml: [29x29 ArtTint #526D84] [InfoPanel] [Prev 25x27 | Play 27x27 | Next 25x27] [Mute 27x27]
                    let art_sz = 29.0_f32.min(height_dips - 6.0);
                    let art_y = cy - art_sz * 0.5;
                    let art_b =
                        dc.CreateSolidColorBrush(&color_rgba(0x52, 0x6d, 0x84, 0.95), None)?;
                    let art_rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: 8.0,
                            top: art_y,
                            right: 8.0 + art_sz,
                            bottom: art_y + art_sz,
                        },
                        radiusX: 7.0,
                        radiusY: 7.0,
                    };
                    dc.FillRoundedRectangle(&art_rr, &art_b);
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        "\u{E8D6}",
                        art_rr.rect,
                        14.0,
                        (0xef, 0xef, 0xef),
                        0.96,
                    )?;

                    let info_l = 8.0 + art_sz + 8.0;
                    let controls_l = width_dips - 116.0;
                    let mid = height_dips * 0.51;
                    draw_text(
                        title,
                        &fmt_12_b_l,
                        D2D_RECT_F {
                            left: info_l,
                            top: (mid - 14.0).max(2.0),
                            right: controls_l - 4.0,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                    );
                    draw_text(
                        artist,
                        &fmt_105_l,
                        D2D_RECT_F {
                            left: info_l,
                            top: mid - 0.5,
                            right: controls_l - 4.0,
                            bottom: (mid + 13.5).min(height_dips - 2.0),
                        },
                        &s_brush,
                    );

                    // 4 sub-buttons: 0=Prev, 1=Play/Pause, 2=Next, 3=Mute
                    let btns: [(f32, f32, &str, bool); 4] = [
                        (controls_l, 25.0, "\u{E892}", false),
                        (controls_l + 27.0, 27.0, "\u{E768}", true),
                        (controls_l + 56.0, 25.0, "\u{E893}", false),
                        (
                            controls_l + 84.0,
                            27.0,
                            if *muted { "\u{E74F}" } else { "\u{E767}" },
                            true,
                        ),
                    ];
                    let bh = 27.0_f32.min(height_dips - 6.0);
                    let by = cy - bh * 0.5;
                    for (i, (bx, bw, glyph, has_base_bg)) in btns.iter().enumerate() {
                        let rect = D2D_RECT_F {
                            left: *bx,
                            top: by,
                            right: bx + bw,
                            bottom: by + bh,
                        };
                        let rr = D2D1_ROUNDED_RECT {
                            rect,
                            radiusX: 8.0,
                            radiusY: 8.0,
                        };
                        let hov = active_sub == Some(i as u8);
                        if hov {
                            let hb = dc.CreateSolidColorBrush(
                                &if is_dark {
                                    color_rgba(0xff, 0xff, 0xff, 0.16)
                                } else {
                                    color_rgba(0x00, 0x00, 0x00, 0.11)
                                },
                                None,
                            )?;
                            dc.FillRoundedRectangle(&rr, &hb);
                        } else if *has_base_bg {
                            dc.FillRoundedRectangle(&rr, &surf_sel_b);
                        }
                        draw_fluent_str_d2d(
                            &self.dwrite_factory,
                            dc,
                            glyph,
                            rect,
                            if i == 1 || i == 3 { 12.5 } else { 11.0 },
                            pal.text_primary,
                            0.95,
                        )?;
                    }
                }
                WidgetChipSpec::Audio { volume, muted } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // AudioWidget.xaml: [Speaker 16px, margin 0,0,7,0] [12.5px SemiBold "67%" / "0%"]
                    let silent = *muted || *volume == 0;
                    let glyph = if silent {
                        "\u{E74F}"
                    } else if *volume < 50 {
                        "\u{E993}"
                    } else {
                        "\u{E995}"
                    };
                    let col = if silent {
                        pal.text_secondary
                    } else {
                        pal.text_primary
                    };
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        glyph,
                        D2D_RECT_F {
                            left: 10.0,
                            top: cy - 8.5,
                            right: 26.0,
                            bottom: cy + 8.5,
                        },
                        15.5,
                        col,
                        0.96,
                    )?;
                    let txt = if *muted {
                        "0%".to_owned()
                    } else {
                        format!("{volume}%")
                    };
                    let b = if silent { &s_brush } else { &p_brush };
                    draw_text(
                        &txt,
                        &fmt_125_b_l,
                        D2D_RECT_F {
                            left: 33.0,
                            top: 0.0,
                            right: width_dips - 8.0,
                            bottom: height_dips,
                        },
                        b,
                    );
                }
                WidgetChipSpec::QuickSettings { pills } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // QuickSettingsWidget.xaml: 3 x 27x27 rounded pills with 4px gap + 8px padding
                    let sz = 27.0_f32.min(height_dips - 6.0);
                    let gap = 4.0;
                    let start_x = (width_dips - (sz * 3.0 + gap * 2.0)) * 0.5;
                    let py = cy - sz * 0.5;
                    for (i, (glyph, active)) in pills.iter().enumerate() {
                        let px = start_x + i as f32 * (sz + gap);
                        let rect = D2D_RECT_F {
                            left: px,
                            top: py,
                            right: px + sz,
                            bottom: py + sz,
                        };
                        let rr = D2D1_ROUNDED_RECT {
                            rect,
                            radiusX: 7.0,
                            radiusY: 7.0,
                        };
                        let hov = active_sub == Some(i as u8);
                        if *active {
                            dc.FillRoundedRectangle(&rr, &sel_fill_b);
                            dc.DrawRoundedRectangle(&rr, &sel_stroke_b, 1.0, None);
                        } else if hov {
                            let hb = dc.CreateSolidColorBrush(
                                &if is_dark {
                                    color_rgba(0xff, 0xff, 0xff, 0.14)
                                } else {
                                    color_rgba(0x00, 0x00, 0x00, 0.09)
                                },
                                None,
                            )?;
                            dc.FillRoundedRectangle(&rr, &hb);
                        }
                        let col = if *active {
                            pal.selected_fg
                        } else {
                            pal.text_secondary
                        };
                        draw_fluent_str_d2d(
                            &self.dwrite_factory,
                            dc,
                            glyph,
                            rect,
                            14.5,
                            col,
                            0.96,
                        )?;
                    }
                }
                WidgetChipSpec::Keyboard { code } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // KeyboardLayoutWidget.cs: [IconKeyboard 13px Secondary, margin 0,0,5,0] [12px SemiBold Primary]
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        "\u{E765}",
                        D2D_RECT_F {
                            left: 9.0,
                            top: cy - 7.5,
                            right: 23.0,
                            bottom: cy + 7.5,
                        },
                        13.0,
                        pal.text_secondary,
                        0.92,
                    )?;
                    draw_text(
                        code,
                        &fmt_12_b_l,
                        D2D_RECT_F {
                            left: 27.0,
                            top: 0.0,
                            right: width_dips - 7.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                    );
                }
                WidgetChipSpec::AiDual {
                    provider_id,
                    brand_color,
                    rows,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // Exact C# `AiUsageWidget.cs` DualQuotaBars (Claude, Codex, single-row Cursor):
                    // Padding 10, [16x16 Brand Logo] + 8px + stacked rows (1 row for Cursor, 2 rows for Claude/Codex)
                    let logo_col = brand_color.unwrap_or(pal.text_primary);
                    design::brand_paths::draw_brand_logo(
                        &self.d2d_factory,
                        dc,
                        provider_id,
                        D2D_RECT_F {
                            left: 10.0,
                            top: cy - 8.0,
                            right: 26.0,
                            bottom: cy + 8.0,
                        },
                        logo_col,
                        0.98,
                    )?;
                    let rx = 34.0;
                    let muted_tr = dc.CreateSolidColorBrush(
                        &if is_dark {
                            color_rgba(0x7c, 0x86, 0x98, 0.35)
                        } else {
                            color_rgba(0x61, 0x66, 0x6e, 0.28)
                        },
                        None,
                    )?;
                    let n = rows.len().max(1);
                    let row_h = 13.0_f32;
                    let row_gap = 2.0_f32;
                    let total_h = n as f32 * row_h + (n.saturating_sub(1) as f32) * row_gap;
                    let start_y = cy - total_h * 0.5;
                    for (idx, (lbl, used, bar_col, reset_txt)) in rows.iter().enumerate() {
                        let y1 = start_y + idx as f32 * (row_h + row_gap);
                        let y2 = y1 + row_h;
                        draw_text(
                            lbl,
                            &fmt_95_b_l,
                            D2D_RECT_F {
                                left: rx,
                                top: y1,
                                right: rx + 22.0,
                                bottom: y2,
                            },
                            &s_brush,
                        );
                        let by = (y1 + y2) * 0.5 - 2.0;
                        let bl = rx + 25.0;
                        let bw = 38.0;
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: bl,
                                    top: by,
                                    right: bl + bw,
                                    bottom: by + 4.0,
                                },
                                radiusX: 2.0,
                                radiusY: 2.0,
                            },
                            &muted_tr,
                        );
                        let fill_w = (bw * used.clamp(0.0, 1.0)).min(bw);
                        if fill_w >= 1.0 {
                            let fb = dc.CreateSolidColorBrush(
                                &color_rgba(bar_col.0, bar_col.1, bar_col.2, 0.98),
                                None,
                            )?;
                            dc.FillRoundedRectangle(
                                &D2D1_ROUNDED_RECT {
                                    rect: D2D_RECT_F {
                                        left: bl,
                                        top: by,
                                        right: bl + fill_w,
                                        bottom: by + 4.0,
                                    },
                                    radiusX: 2.0,
                                    radiusY: 2.0,
                                },
                                &fb,
                            );
                        }
                        draw_text(
                            reset_txt,
                            &fmt_9_l,
                            D2D_RECT_F {
                                left: bl + bw + 5.0,
                                top: y1,
                                right: width_dips - 8.0,
                                bottom: y2,
                            },
                            &p_brush,
                        );
                    }
                }
                WidgetChipSpec::AiOpenCode { columns } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // Exact C# `AiUsageWidget.cs` OpenCode: [Logo 16x16] [OpenCode 12px SemiBold] + 3 x 54px columns (`1h | 7d | 30d`)
                    design::brand_paths::draw_brand_logo(
                        &self.d2d_factory,
                        dc,
                        "aiusage_opencode",
                        D2D_RECT_F {
                            left: 10.0,
                            top: cy - 8.0,
                            right: 26.0,
                            bottom: cy + 8.0,
                        },
                        pal.text_primary,
                        0.98,
                    )?;
                    draw_text(
                        "OpenCode",
                        &fmt_12_b_l,
                        D2D_RECT_F {
                            left: 32.0,
                            top: 0.0,
                            right: 98.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                    );
                    let start_x = 102.0;
                    let col_w = 54.0;
                    let col_gap = 8.0;
                    let stack_h = 28.0;
                    let top_y = cy - stack_h * 0.5;
                    for (ci, (lbl, pct_txt, used, bar_col, reset_txt)) in
                        columns.iter().take(3).enumerate()
                    {
                        let cx_l = start_x + ci as f32 * (col_w + col_gap);
                        // Head: short label left + percent right
                        draw_text(
                            lbl,
                            &fmt_95_l,
                            D2D_RECT_F {
                                left: cx_l,
                                top: top_y,
                                right: cx_l + 24.0,
                                bottom: top_y + 12.0,
                            },
                            &s_brush,
                        );
                        draw_text(
                            pct_txt,
                            &fmt_105_b_r,
                            D2D_RECT_F {
                                left: cx_l + 20.0,
                                top: top_y,
                                right: cx_l + col_w,
                                bottom: top_y + 12.0,
                            },
                            &p_brush,
                        );
                        // 54x3 track
                        let by = top_y + 13.5;
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: cx_l,
                                    top: by,
                                    right: cx_l + col_w,
                                    bottom: by + 3.0,
                                },
                                radiusX: 1.5,
                                radiusY: 1.5,
                            },
                            &tr_brush,
                        );
                        let fw = (col_w * used.clamp(0.0, 1.0)).min(col_w);
                        if fw >= 1.0 {
                            let fb = dc.CreateSolidColorBrush(
                                &color_rgba(bar_col.0, bar_col.1, bar_col.2, 0.98),
                                None,
                            )?;
                            dc.FillRoundedRectangle(
                                &D2D1_ROUNDED_RECT {
                                    rect: D2D_RECT_F {
                                        left: cx_l,
                                        top: by,
                                        right: cx_l + fw,
                                        bottom: by + 3.0,
                                    },
                                    radiusX: 1.5,
                                    radiusY: 1.5,
                                },
                                &fb,
                            );
                        }
                        // Bottom reset row
                        draw_text(
                            reset_txt,
                            &fmt_85_l,
                            D2D_RECT_F {
                                left: cx_l,
                                top: by + 4.5,
                                right: cx_l + col_w,
                                bottom: by + 15.0,
                            },
                            &s_brush,
                        );
                    }
                }
                WidgetChipSpec::AiCopilot {
                    value_text,
                    meta_text,
                    used,
                    bar_color,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    // Exact C# `AiUsageWidget.cs` Copilot: [Logo 16x16] [Copilot 12px SemiBold] | [12px Val / 9.5px Meta] [31x4 track]
                    design::brand_paths::draw_brand_logo(
                        &self.d2d_factory,
                        dc,
                        "aiusage_copilot",
                        D2D_RECT_F {
                            left: 10.0,
                            top: cy - 8.0,
                            right: 26.0,
                            bottom: cy + 8.0,
                        },
                        pal.text_primary,
                        0.98,
                    )?;
                    draw_text(
                        "Copilot",
                        &fmt_12_b_l,
                        D2D_RECT_F {
                            left: 32.0,
                            top: 0.0,
                            right: 80.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                    );
                    draw_vdiv(83.0, 20.0);
                    let mid = height_dips * 0.50;
                    draw_text(
                        value_text,
                        &fmt_12_b_l,
                        D2D_RECT_F {
                            left: 92.0,
                            top: (mid - 14.0).max(2.0),
                            right: width_dips - 46.0,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                    );
                    draw_text(
                        meta_text,
                        &fmt_95_l,
                        D2D_RECT_F {
                            left: 92.0,
                            top: mid,
                            right: width_dips - 46.0,
                            bottom: (mid + 12.5).min(height_dips - 2.0),
                        },
                        &s_brush,
                    );
                    let bl = width_dips - 41.0;
                    let bw = 31.0;
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: bl,
                                top: cy - 2.0,
                                right: bl + bw,
                                bottom: cy + 2.0,
                            },
                            radiusX: 2.0,
                            radiusY: 2.0,
                        },
                        &tr_brush,
                    );
                    let fw = (bw * used.clamp(0.0, 1.0)).min(bw);
                    if fw >= 1.0 {
                        let fb = dc.CreateSolidColorBrush(
                            &color_rgba(bar_color.0, bar_color.1, bar_color.2, 0.98),
                            None,
                        )?;
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: bl,
                                    top: cy - 2.0,
                                    right: bl + fw,
                                    bottom: cy + 2.0,
                                },
                                radiusX: 2.0,
                                radiusY: 2.0,
                            },
                            &fb,
                        );
                    }
                }
                WidgetChipSpec::AiBalance {
                    provider_id,
                    name,
                    brand_color,
                    value_text,
                } => {
                    // Unified AI card style matching Claude, Codex, Copilot, OpenCode, and Cursor:
                    // neutral card surface, crisp 16x16 brand logo, subtle divider, and 2-line metric stack.
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    design::brand_paths::draw_brand_logo(
                        &self.d2d_factory,
                        dc,
                        provider_id,
                        D2D_RECT_F {
                            left: 10.0,
                            top: cy - 8.0,
                            right: 26.0,
                            bottom: cy + 8.0,
                        },
                        *brand_color,
                        0.98,
                    )?;
                    draw_vdiv(33.0, 20.0);
                    let tx = 41.0;
                    let mid = height_dips * 0.50;
                    draw_text(
                        name,
                        &fmt_95_b_l,
                        D2D_RECT_F {
                            left: tx,
                            top: (mid - 13.5).max(2.0),
                            right: width_dips - 8.0,
                            bottom: mid,
                        },
                        &s_brush,
                    );
                    draw_text(
                        value_text,
                        &fmt_125_b_l,
                        D2D_RECT_F {
                            left: tx,
                            top: mid - 0.5,
                            right: width_dips - 8.0,
                            bottom: (mid + 14.5).min(height_dips - 2.0),
                        },
                        &p_brush,
                    );
                }
                WidgetChipSpec::Simple {
                    icon,
                    icon_color,
                    top,
                    bottom,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        None,
                        interaction,
                        border_rgb,
                        border_width_dips,
                    )?;
                    draw_chip_icon(&self.dwrite_factory, dc, *icon, 18.0, cy, 15.0, *icon_color)?;
                    let mid = height_dips * 0.51;
                    draw_text(
                        top,
                        &fmt_115_b_l,
                        D2D_RECT_F {
                            left: 32.0,
                            top: (mid - 13.5).max(2.0),
                            right: width_dips - 8.0,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                    );
                    draw_text(
                        bottom,
                        &fmt_10_l,
                        D2D_RECT_F {
                            left: 32.0,
                            top: mid - 0.5,
                            right: width_dips - 8.0,
                            bottom: (mid + 13.0).min(height_dips - 2.0),
                        },
                        &s_brush,
                    );
                }
            }
            Ok(())
        })
    }

    #[allow(dead_code)]
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
                &HSTRING::from(design::fonts::ui_family_list().as_str()),
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
    pub fn create_pill_nodes(
        &self,
        radius_px: f32,
        pill_h_px: f32,
        bg: (u8, u8, u8, f32),
        border: (u8, u8, u8, f32),
        dpi: f32,
        border_width_dips: f32,
    ) -> Result<PillNodes> {
        let h = pill_h_px.ceil().max(4.0) as u32;
        let r = radius_px.clamp(2.0, pill_h_px * 0.5);
        let cap_w = (r.ceil() as u32).max(2);
        let bw = dpi.max(1.0);
        let stroke_w = (border_width_dips * dpi).max(0.0);
        let half_bw = bw * 0.5;
        let is_dark = (u16::from(bg.0) + u16::from(bg.1) + u16::from(bg.2)) < 384;
        let top_bg = color_rgba(
            bg.0.saturating_add(if is_dark { 10 } else { 6 }),
            bg.1.saturating_add(if is_dark { 10 } else { 6 }),
            bg.2.saturating_add(if is_dark { 12 } else { 6 }),
            bg.3,
        );
        let bot_bg = color_rgba(bg.0, bg.1, bg.2, bg.3);
        let bd_col = color_rgba(border.0, border.1, border.2, border.3);
        let spec_col = if is_dark {
            color_rgba(0xff, 0xff, 0xff, 0.14)
        } else {
            color_rgba(0xff, 0xff, 0xff, 0.72)
        };

        let make_grad = |dc: &ID2D1DeviceContext, fh: f32| unsafe {
            let stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: top_bg,
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: bot_bg,
                },
            ];
            let coll = dc.CreateGradientStopCollection(
                &stops,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                D2D1_EXTEND_MODE_CLAMP,
                D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
            )?;
            dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 { X: 0.0, Y: 0.0 },
                    endPoint: Vector2 { X: 0.0, Y: fh },
                },
                None,
                &coll,
            )
        };

        let left_surf = self.draw_surface(cap_w, h, |dc, w, fh| unsafe {
            let bg_brush = make_grad(dc, fh)?;
            let bd_brush = dc.CreateSolidColorBrush(&bd_col, None)?;
            let spec_brush = dc.CreateSolidColorBrush(&spec_col, None)?;
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: half_bw,
                    top: half_bw,
                    right: w + r * 2.0,
                    bottom: fh - half_bw,
                },
                radiusX: r,
                radiusY: r,
            };
            let inner_rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: half_bw + bw,
                    top: half_bw + bw,
                    right: w + r * 2.0,
                    bottom: fh - half_bw - bw,
                },
                radiusX: (r - bw).max(1.0),
                radiusY: (r - bw).max(1.0),
            };
            dc.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: fh,
                },
                windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            dc.FillRoundedRectangle(&rr, &bg_brush);
            // Top-half specular inner rim
            dc.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: (r + bw * 2.0).min(fh * 0.5),
                },
                windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            dc.DrawRoundedRectangle(&inner_rr, &spec_brush, bw, None);
            dc.PopAxisAlignedClip();
            if border.3 > 0.0 && stroke_w > 0.0 {
                dc.DrawRoundedRectangle(&rr, &bd_brush, stroke_w, None);
            }
            dc.PopAxisAlignedClip();
            Ok(())
        })?;

        let center_surf = self.draw_surface(1, h, |dc, _w, fh| unsafe {
            let bg_brush = make_grad(dc, fh)?;
            let bd_brush = dc.CreateSolidColorBrush(&bd_col, None)?;
            let spec_brush = dc.CreateSolidColorBrush(&spec_col, None)?;
            dc.FillRectangle(
                &D2D_RECT_F {
                    left: 0.0,
                    top: half_bw,
                    right: 1.0,
                    bottom: fh - half_bw,
                },
                &bg_brush,
            );
            // Subtle top-edge specular inner highlight
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
            if border.3 > 0.0 && stroke_w > 0.0 {
                dc.DrawLine(
                    Vector2 { X: 0.0, Y: half_bw },
                    Vector2 { X: 1.0, Y: half_bw },
                    &bd_brush,
                    stroke_w,
                    None,
                );
                dc.DrawLine(
                    Vector2 {
                        X: 0.0,
                        Y: fh - half_bw,
                    },
                    Vector2 {
                        X: 1.0,
                        Y: fh - half_bw,
                    },
                    &bd_brush,
                    stroke_w,
                    None,
                );
            }
            Ok(())
        })?;

        let right_surf = self.draw_surface(cap_w, h, |dc, w, fh| unsafe {
            let bg_brush = make_grad(dc, fh)?;
            let bd_brush = dc.CreateSolidColorBrush(&bd_col, None)?;
            let spec_brush = dc.CreateSolidColorBrush(&spec_col, None)?;
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: -(r * 2.0),
                    top: half_bw,
                    right: w - half_bw,
                    bottom: fh - half_bw,
                },
                radiusX: r,
                radiusY: r,
            };
            let inner_rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: -(r * 2.0),
                    top: half_bw + bw,
                    right: w - half_bw - bw,
                    bottom: fh - half_bw - bw,
                },
                radiusX: (r - bw).max(1.0),
                radiusY: (r - bw).max(1.0),
            };
            dc.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: fh,
                },
                windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            dc.FillRoundedRectangle(&rr, &bg_brush);
            dc.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: (r + bw * 2.0).min(fh * 0.5),
                },
                windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            dc.DrawRoundedRectangle(&inner_rr, &spec_brush, bw, None);
            dc.PopAxisAlignedClip();
            if border.3 > 0.0 && stroke_w > 0.0 {
                dc.DrawRoundedRectangle(&rr, &bd_brush, stroke_w, None);
            }
            dc.PopAxisAlignedClip();
            Ok(())
        })?;

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
        let pal = design::palette(is_dark);

        self.draw_surface(size_px, size_px, |dc, w, h| unsafe {
            if kind == ShellGlyph::Start {
                // Crisp Windows 11 4-pane logo with diagonal sky-to-azure gradient.
                let s = w / 24.0;
                let stops = if is_dark {
                    [
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
                    [
                        D2D1_GRADIENT_STOP {
                            position: 0.0,
                            color: color_rgba(0x00, 0x95, 0xf6, 1.0),
                        },
                        D2D1_GRADIENT_STOP {
                            position: 1.0,
                            color: color_rgba(0x00, 0x5f, 0xb8, 1.0),
                        },
                    ]
                };
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
        for px in premul.chunks_exact_mut(4) {
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
                .CreateTextLayout(&wide, &self.tooltip_format, 290.0, 28.0)?
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe {
            layout.GetMetrics(&mut metrics)?;
        }
        let w_dips = (metrics.widthIncludingTrailingWhitespace + 22.0)
            .clamp(48.0, 296.0)
            .ceil();
        let h_dips = 26.0_f32;
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
            dc.DrawText(
                &wide,
                &self.tooltip_format,
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w_dips,
                    bottom: h_dips,
                },
                &fg,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
            );
            Ok(())
        })?;

        Ok((surface, w_dips, h_dips))
    }
}

#[allow(dead_code)]
fn _unused_rect_check(_: RECT, _: D2D_SIZE_F) {}

/// Shared widget card material used by every dock widget so all cards read as one family:
/// a frosted-glass surface (vertical fill gradient), a 1px rim that is brightest at the top edge,
/// and a thin specular line under the rim. Hover and press raise fill and rim opacity.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_chip_card(
    dc: &ID2D1DeviceContext,
    width_dips: f32,
    height_dips: f32,
    is_dark: bool,
    _active_tint: Option<Rgb>,
    interaction: WidgetInteraction,
    border_rgb: Option<Rgb>,
    border_width_dips: f32,
) -> Result<()> {
    // (top fill alpha, bottom fill alpha, rim top alpha, rim bottom alpha, specular alpha)
    let (fill_top, fill_bottom, rim_top, rim_bottom, specular) = match (is_dark, interaction) {
        (true, WidgetInteraction::Idle) => (0.105, 0.050, 0.24, 0.06, 0.14),
        (true, WidgetInteraction::Hovered(_)) => (0.170, 0.100, 0.34, 0.12, 0.22),
        (true, WidgetInteraction::Pressed(_)) => (0.230, 0.150, 0.28, 0.14, 0.10),
        (false, WidgetInteraction::Idle) => (0.060, 0.030, 0.12, 0.05, 0.0),
        (false, WidgetInteraction::Hovered(_)) => (0.100, 0.065, 0.18, 0.08, 0.0),
        (false, WidgetInteraction::Pressed(_)) => (0.150, 0.110, 0.20, 0.10, 0.0),
    };
    let base = if is_dark { 0xff } else { 0x00 };
    let rim_base = if is_dark { 0xff } else { 0x00 };
    let gradient = |top: f32, bottom: f32, c: u8| -> Result<ID2D1LinearGradientBrush> {
        unsafe {
            let stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: color_rgba(c, c, c, top),
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: color_rgba(c, c, c, bottom),
                },
            ];
            let collection = dc.CreateGradientStopCollection(
                &stops,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                D2D1_EXTEND_MODE_CLAMP,
                D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
            )?;
            dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 { X: 0.0, Y: 0.0 },
                    endPoint: Vector2 {
                        X: 0.0,
                        Y: height_dips,
                    },
                },
                None,
                &collection,
            )
        }
    };
    unsafe {
        let fill_rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: width_dips,
                bottom: height_dips,
            },
            radiusX: 12.0,
            radiusY: 12.0,
        };
        dc.FillRoundedRectangle(&fill_rr, &gradient(fill_top, fill_bottom, base)?);
        let rim_rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: 0.5,
                top: 0.5,
                right: width_dips - 0.5,
                bottom: height_dips - 0.5,
            },
            radiusX: 11.5,
            radiusY: 11.5,
        };
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

/// Exact C# `WidgetCardBorder` / `WidgetCardButton` hover/press-only overlay (transparent at rest,
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
            &HSTRING::from("Segoe Fluent Icons"),
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

unsafe fn draw_chip_icon(
    dwrite: &IDWriteFactory,
    dc: &ID2D1DeviceContext,
    icon: ChipIcon,
    cx: f32,
    cy: f32,
    size_dips: f32,
    color: Rgb,
) -> Result<()> {
    let app_icon = match icon {
        ChipIcon::Folder => AppIcon::Folder,
        ChipIcon::Grid => AppIcon::LayoutGrid,
        ChipIcon::Sparkle => AppIcon::Layers,
    };
    let half = size_dips * 0.6;
    unsafe {
        draw_fluent_glyph_d2d(
            dwrite,
            dc,
            app_icon,
            D2D_RECT_F {
                left: cx - half,
                top: cy - half,
                right: cx + half,
                bottom: cy + half,
            },
            size_dips,
            color,
            0.96,
        )
    }
}

impl DcompEngine {
    /// Draws the Dynamic Island surface (top-flush rounded bottom pill, border, nav chevrons,
    /// tabular page counter, and expand chevron).
    #[allow(clippy::too_many_arguments)]
    pub fn create_island_chrome_surface(
        &self,
        width_dips: f32,
        height_dips: f32,
        dpi: f32,
        is_dark: bool,
        opacity: f32,
        background: SurfaceBackground,
        use_theme: bool,
        has_prev_next: bool,
        page_count_text: &str,
        has_expand: bool,
        hovered_btn: Option<IslandButton>,
        pressed_btn: Option<IslandButton>,
        border_rgb: Rgb,
        border_width_dips: f32,
    ) -> Result<IDCompositionSurface> {
        let pal = design::palette(is_dark);
        let w_px = (width_dips * dpi).ceil().max(1.0) as u32;
        let h_px = (height_dips * dpi).ceil().max(1.0) as u32;
        let fmt_count = self.cached_text_format(11.0 * dpi, false, 0)?;

        self.draw_surface(w_px, h_px, |dc, w, h| unsafe {
            let r = 20.0 * dpi;
            let stroke = (border_width_dips * dpi).max(0.0);
            // Theme follows the active palette; opting out paints a fixed neutral pill.
            let neutral_pill = if is_dark {
                (0x2b_u8, 0x2b_u8, 0x30_u8)
            } else {
                (0xf7_u8, 0xf7_u8, 0xfa_u8)
            };
            let fill_rgb = if use_theme { pal.dock_pill } else { neutral_pill };
            let fill_alpha = match background {
                SurfaceBackground::None => 0.0,
                SurfaceBackground::Solid => 1.0,
                SurfaceBackground::Glass => opacity.clamp(0.1, 1.0),
            };
            let bd_alpha = if is_dark { 0.14_f32 } else { 0.12_f32 };
            let bd_color = color_rgba(border_rgb.0, border_rgb.1, border_rgb.2, bd_alpha);
            let bd_brush = dc.CreateSolidColorBrush(&bd_color, None)?;

            // Top-flush rounded rectangle (flat top edge, 20 DIP bottom corners).
            let pill_rect = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: stroke * 0.5,
                    top: -r,
                    right: w - stroke * 0.5,
                    bottom: h - stroke * 0.5,
                },
                radiusX: r,
                radiusY: r,
            };
            if fill_alpha > 0.0 {
                let bg_brush = dc.CreateSolidColorBrush(
                    &color_rgba(fill_rgb.0, fill_rgb.1, fill_rgb.2, fill_alpha),
                    None,
                )?;
                dc.FillRoundedRectangle(&pill_rect, &bg_brush);
            }
            if background != SurfaceBackground::None {
                dc.DrawRoundedRectangle(&pill_rect, &bd_brush, stroke, None);
            }

            let draw_btn = |btn: IslandButton, cx_dips: f32, glyph: &str, enabled: bool| -> Result<()> {
                let bx = cx_dips * dpi;
                let by = 9.0 * dpi;
                let bsz = 32.0 * dpi;
                let is_pressed = pressed_btn == Some(btn);
                let is_hovered = hovered_btn == Some(btn);
                if enabled && (is_pressed || is_hovered) {
                    let alpha = if is_pressed {
                        0.16
                    } else if is_dark {
                        0.10
                    } else {
                        0.08
                    };
                    let ink = if is_dark { (0xff, 0xff, 0xff) } else { (0x18, 0x18, 0x1b) };
                    let hb = dc.CreateSolidColorBrush(&color_rgba(ink.0, ink.1, ink.2, alpha), None)?;
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: bx,
                                top: by,
                                right: bx + bsz,
                                bottom: by + bsz,
                            },
                            radiusX: 8.0 * dpi,
                            radiusY: 8.0 * dpi,
                        },
                        &hb,
                    );
                }
                let ink = if enabled { pal.text_primary } else { pal.text_secondary };
                let alpha = if enabled { 0.92 } else { 0.35 };
                draw_fluent_str_d2d(
                    &self.dwrite_factory,
                    dc,
                    glyph,
                    D2D_RECT_F {
                        left: bx,
                        top: by,
                        right: bx + bsz,
                        bottom: by + bsz,
                    },
                    13.0 * dpi,
                    ink,
                    alpha,
                )
            };

            draw_btn(IslandButton::Prev, 3.0, "\u{E76B}", has_prev_next)?;
            let right_total = 38.0 + 56.0 + 38.0;
            let next_x = width_dips - right_total;
            draw_btn(IslandButton::Next, next_x + 3.0, "\u{E76C}", has_prev_next)?;

            if !page_count_text.is_empty() {
                let count_rect = D2D_RECT_F {
                    left: (next_x + 38.0) * dpi,
                    top: 0.0,
                    right: (next_x + 38.0 + 56.0) * dpi,
                    bottom: 50.0 * dpi,
                };
                let sec_brush = dc.CreateSolidColorBrush(
                    &color_rgba(pal.text_secondary.0, pal.text_secondary.1, pal.text_secondary.2, 0.9),
                    None,
                )?;
                let layout = self.dwrite_factory.CreateTextLayout(
                    &HSTRING::from(page_count_text),
                    &fmt_count,
                    56.0 * dpi,
                    50.0 * dpi,
                )?;
                if let Ok(typo) = self.dwrite_factory.CreateTypography() {
                    let _ = typo.AddFontFeature(
                        windows::Win32::Graphics::DirectWrite::DWRITE_FONT_FEATURE {
                            nameTag: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                            parameter: 1,
                        },
                    );
                    let _ = layout.SetTypography(
                        &typo,
                        windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE {
                            startPosition: 0,
                            length: page_count_text.encode_utf16().count() as u32,
                        },
                    );
                }
                dc.DrawTextLayout(
                    Vector2 {
                        X: count_rect.left,
                        Y: count_rect.top,
                    },
                    &layout,
                    &sec_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }

            if has_expand {
                let expand_x = width_dips - 35.0;
                draw_btn(IslandButton::Expand, expand_x, "\u{E70D}", true)?;
            }

            Ok(())
        })
    }
}
