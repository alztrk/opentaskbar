//! Native DirectComposition + Direct2D + DirectWrite visual engine for the floating dock.
//!
//! All icons, pill caps, shell glyphs, indicator dots, and tooltips are drawn ONCE onto static
//! `IDCompositionSurface` textures. During 60-180 FPS fisheye, bounce, and drag animations, only
//! 2D affine transforms (`IDCompositionMatrixTransform`) are updated and committed to DWM
//! (`IDCompositionDevice::Commit`) with zero D3D11 draw calls and ~0-1% GPU usage.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_PROPERTIES1, D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_CAP_STYLE_ROUND,
    D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LINE_JOIN_ROUND,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES1,
    D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1, ID2D1StrokeStyle,
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
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, DWriteCreateFactory, IDWriteFactory,
    IDWriteTextFormat,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetInteraction {
    Idle,
    Hovered(Option<u8>),
    Pressed(Option<u8>),
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
    /// C# `WeatherWidget.cs`: `[Icon 19px] [18px Bold Temp] [Place / Condition] | [Today / High/Low]`
    Weather {
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
    },
    /// C# `BluetoothBatteryWidget.cs`: horizontal connected device tiles (`icon + % + bolt`) or `—`
    Bluetooth {
        /// `(percent, low_warning)`
        devices: Vec<(u32, bool)>,
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
    /// C# `AiUsageWidget.cs` dual-bar layout (Claude, Codex, Cursor) with real `Brand.xaml` logo
    AiDual {
        provider_id: &'static str,
        brand_color: Option<Rgb>,
        detected: bool,
    },
    /// C# `AiUsageWidget.cs` multi-column Antigravity layout (`Gemini | Claude`)
    AiAntigravity { detected: bool },
    /// C# `AiUsageWidget.cs` triple-column OpenCode layout (`1h | 7d | 30d`)
    AiOpenCode { detected: bool },
    /// C# `AiUsageWidget.cs` Copilot inline layout (`Logo + Copilot | Status + mini bar`)
    AiCopilot { detected: bool },
    /// C# `AiUsageWidget.cs` DeepSeek / OpenRouter tinted balance card (`29x29 tinted logo well + Name / Status`)
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

impl WidgetChipSpec {
    pub fn cache_key(&self) -> String {
        format!("{self:?}")
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
        let font_name = HSTRING::from("Segoe UI Variable Text");
        let locale = HSTRING::from("en-US");
        let tooltip_format = unsafe {
            let fmt = dwrite_factory.CreateTextFormat(
                &font_name,
                None,
                DWRITE_FONT_WEIGHT_MEDIUM,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.0,
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
                18.0,
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

            let pad_v = (1.5 * dpi).max(1.0);
            let r = radius_px.clamp(2.0, (h - pad_v * 2.0) * 0.5);
            if is_left {
                dc.FillRectangle(
                    &D2D_RECT_F {
                        left: 0.0,
                        top: pad_v,
                        right: w,
                        bottom: h - pad_v,
                    },
                    &grad,
                );
            } else {
                // Clip a wider rounded rect to keep the right outer edge curved with the dock pill.
                dc.PushAxisAlignedClip(
                    &D2D_RECT_F {
                        left: 0.0,
                        top: pad_v,
                        right: w,
                        bottom: h - pad_v,
                    },
                    windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                );
                dc.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: -(r * 2.0),
                            top: pad_v,
                            right: w,
                            bottom: h - pad_v,
                        },
                        radiusX: r,
                        radiusY: r,
                    },
                    &grad,
                );
                dc.PopAxisAlignedClip();
            }

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

    /// Renders one of the 1-to-1 C# dock widget modules at automatic `height_dips`
    /// derived from the live dock pill height, using Slate/Aurora tokens and Windows 11 Fluent icons.
    pub fn create_widget_chip_surface(
        &self,
        spec: &WidgetChipSpec,
        width_dips: f32,
        height_dips: f32,
        dpi: f32,
        is_dark: bool,
        interaction: WidgetInteraction,
    ) -> Result<IDCompositionSurface> {
        let pal = design::palette(is_dark);
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

        // `align`: 0 = center, 1 = leading, 2 = trailing
        let mk_fmt = |size: f32, bold: bool, align: u8| -> Result<IDWriteTextFormat> {
            unsafe {
                let fmt = self.dwrite_factory.CreateTextFormat(
                    &HSTRING::from("Segoe UI Variable Text"),
                    None,
                    if bold {
                        DWRITE_FONT_WEIGHT_SEMI_BOLD
                    } else {
                        DWRITE_FONT_WEIGHT_MEDIUM
                    },
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    &HSTRING::from("en-US"),
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
                Ok(fmt)
            }
        };

        let fmt_18_b_l = mk_fmt(18.0, true, 1)?;
        let fmt_15_b_l = mk_fmt(15.0, true, 1)?;
        let fmt_13_b_c = mk_fmt(13.0, true, 0)?;
        let fmt_13_b_l = mk_fmt(13.0, true, 1)?;
        let fmt_125_b_l = mk_fmt(12.5, true, 1)?;
        let fmt_12_b_l = mk_fmt(12.0, true, 1)?;
        let fmt_115_b_l = mk_fmt(11.5, true, 1)?;
        let fmt_115_l = mk_fmt(11.5, false, 1)?;
        let fmt_11_b_l = mk_fmt(11.0, true, 1)?;
        let fmt_105_l = mk_fmt(10.5, false, 1)?;
        let fmt_10_l = mk_fmt(10.0, false, 1)?;
        let fmt_95_b_l = mk_fmt(9.5, true, 1)?;
        let fmt_9_l = mk_fmt(9.0, false, 1)?;
        let fmt_9_b_l = mk_fmt(9.0, true, 1)?;

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
                    let wtxt: Vec<u16> = text.encode_utf16().collect();
                    dc.DrawText(
                        &wtxt,
                        fmt,
                        &rect,
                        brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                    icon_color,
                    is_rain,
                    temp,
                    place,
                    condition,
                    today_range,
                } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    // WeatherWidget.cs: [Icon 19px] [18px Bold Temp] [Place / Condition] | [Today / range]
                    let glyph = if *is_rain { "\u{E9C4}" } else { "\u{E753}" };
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        glyph,
                        D2D_RECT_F {
                            left: 9.0,
                            top: cy - 10.0,
                            right: 29.0,
                            bottom: cy + 10.0,
                        },
                        18.0,
                        *icon_color,
                        0.98,
                    )?;
                    draw_text(
                        temp,
                        &fmt_18_b_l,
                        D2D_RECT_F {
                            left: 35.0,
                            top: 0.0,
                            right: 78.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                    );
                    let px = 80.0;
                    let pr = if today_range.is_some() {
                        width_dips - 74.0
                    } else {
                        width_dips - 9.0
                    };
                    let mid = height_dips * 0.51;
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
                            bottom: (mid + 13.0).min(height_dips - 2.0),
                        },
                        &s_brush,
                    );
                    if let Some(range) = today_range {
                        let sep_x = width_dips - 68.0;
                        draw_vdiv(sep_x, 20.0);
                        draw_text(
                            "Today",
                            &fmt_9_l,
                            D2D_RECT_F {
                                left: sep_x + 8.0,
                                top: (mid - 13.0).max(2.0),
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
                                top: mid - 1.0,
                                right: width_dips - 8.0,
                                bottom: (mid + 13.5).min(height_dips - 2.0),
                            },
                            &p_brush,
                        );
                    }
                }
                WidgetChipSpec::DayProgress {
                    ratio,
                    percent,
                    unit,
                } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                    if *ratio > 0.01 {
                        let steps = ((ratio.clamp(0.0, 1.0) * 36.0).round() as usize).max(1);
                        let mut prev_pt = Vector2 { X: rx, Y: cy - rad };
                        for i in 1..=steps {
                            let ang = -std::f32::consts::FRAC_PI_2
                                + (i as f32 / 36.0) * std::f32::consts::TAU;
                            let pt = Vector2 {
                                X: rx + ang.cos() * rad,
                                Y: cy + ang.sin() * rad,
                            };
                            dc.DrawLine(prev_pt, pt, &r_brush, 2.8, Some(&stroke));
                            prev_pt = pt;
                        }
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                } => {
                    // Exact C# BatteryWidget.cs horizontal liquid-fill card!
                    if !*present {
                        draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                            radiusX: 12.0,
                            radiusY: 12.0,
                        };
                        let base_track = dc.CreateSolidColorBrush(
                            &if is_dark {
                                color_rgba(0xff, 0xff, 0xff, 0.08)
                            } else {
                                color_rgba(0x00, 0x00, 0x00, 0.06)
                            },
                            None,
                        )?;
                        dc.FillRoundedRectangle(&card_rr, &base_track);

                        // Exact C# BatteryWidget.PaletteForLevel gradient stops
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
                        let edge_x = width_dips * ((*percent).clamp(4, 100) as f32 / 100.0);
                        dc.PushAxisAlignedClip(
                            &D2D_RECT_F {
                                left: 0.0,
                                top: 0.0,
                                right: edge_x,
                                bottom: height_dips,
                            },
                            windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                        );
                        dc.FillRoundedRectangle(&card_rr, &liq_b);
                        dc.PopAxisAlignedClip();

                        // Hover/press overlay above liquid fill (just like C# _hoverOverlay/_pressOverlay)
                        draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;

                        let label = if *charging {
                            format!("⚡ {percent}%")
                        } else {
                            format!("{percent}%")
                        };
                        let sh_b = dc.CreateSolidColorBrush(&color_rgba(0, 0, 0, 0.35), None)?;
                        let wh_b =
                            dc.CreateSolidColorBrush(&color_rgba(0xff, 0xff, 0xff, 0.98), None)?;
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
                WidgetChipSpec::Bluetooth { devices } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    if devices.is_empty() {
                        draw_fluent_str_d2d(
                            &self.dwrite_factory,
                            dc,
                            "\u{E702}",
                            D2D_RECT_F {
                                left: 10.0,
                                top: cy - 8.0,
                                right: 26.0,
                                bottom: cy + 8.0,
                            },
                            14.5,
                            pal.text_secondary,
                            0.90,
                        )?;
                        draw_text(
                            "—",
                            &fmt_12_b_l,
                            D2D_RECT_F {
                                left: 30.0,
                                top: 0.0,
                                right: width_dips - 8.0,
                                bottom: height_dips,
                            },
                            &s_brush,
                        );
                    } else {
                        let count = devices.len().min(2);
                        let tile_w = (width_dips - 12.0) / count as f32;
                        for (i, (pct, low)) in devices.iter().take(2).enumerate() {
                            let tx = 6.0 + i as f32 * tile_w;
                            if i > 0 {
                                draw_vdiv(tx - 2.0, 20.0);
                            }
                            draw_fluent_str_d2d(
                                &self.dwrite_factory,
                                dc,
                                "\u{E702}",
                                D2D_RECT_F {
                                    left: tx + 3.0,
                                    top: cy - 8.0,
                                    right: tx + 19.0,
                                    bottom: cy + 8.0,
                                },
                                14.5,
                                pal.text_secondary,
                                0.92,
                            )?;
                            let val_col = if *low { pal.warning } else { pal.text_primary };
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    // StorageWidget.cs: "C: 213 / 476 GB | D: 1.2 / 1.8 TB"
                    if drives.is_empty() {
                        draw_text(
                            "—",
                            &fmt_115_l,
                            D2D_RECT_F {
                                left: 10.0,
                                top: 0.0,
                                right: width_dips - 10.0,
                                bottom: height_dips,
                            },
                            &s_brush,
                        );
                    } else {
                        let count = drives.len().min(2);
                        let seg_w = (width_dips - 20.0) / count as f32;
                        for (i, (letter, pair)) in drives.iter().take(2).enumerate() {
                            let sx = 10.0 + i as f32 * seg_w;
                            if i > 0 {
                                draw_vdiv(sx - 5.0, 13.0);
                            }
                            draw_text(
                                letter,
                                &fmt_11_b_l,
                                D2D_RECT_F {
                                    left: sx,
                                    top: 0.0,
                                    right: sx + 18.0,
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                    detected,
                } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    // AiUsageWidget.cs DualQuotaBars (Claude, Codex, Cursor):
                    // [Brand Logo 16x16] + stacked rows: "5h" [38x4 track] "—" and "7d" [38x4 track] "—"
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
                        if *detected { 0.98 } else { 0.65 },
                    )?;
                    let rx = 34.0;
                    let mid = height_dips * 0.5;
                    let rows = [("5h", mid - 12.5, mid - 0.5), ("7d", mid + 0.5, mid + 12.5)];
                    let fill_b = dc.CreateSolidColorBrush(
                        &color_rgba(
                            pal.widget_accent.0,
                            pal.widget_accent.1,
                            pal.widget_accent.2,
                            0.95,
                        ),
                        None,
                    )?;
                    for (lbl, y1, y2) in rows {
                        draw_text(
                            lbl,
                            &fmt_95_b_l,
                            D2D_RECT_F {
                                left: rx,
                                top: y1,
                                right: rx + 20.0,
                                bottom: y2,
                            },
                            &s_brush,
                        );
                        let by = (y1 + y2) * 0.5 - 2.0;
                        let bl = rx + 22.0;
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
                            &tr_brush,
                        );
                        if *detected {
                            dc.FillRoundedRectangle(
                                &D2D1_ROUNDED_RECT {
                                    rect: D2D_RECT_F {
                                        left: bl,
                                        top: by,
                                        right: bl + 4.0,
                                        bottom: by + 4.0,
                                    },
                                    radiusX: 2.0,
                                    radiusY: 2.0,
                                },
                                &fill_b,
                            );
                        }
                        draw_text(
                            if *detected { "Ready" } else { "—" },
                            &fmt_9_l,
                            D2D_RECT_F {
                                left: bl + bw + 5.0,
                                top: y1,
                                right: width_dips - 6.0,
                                bottom: y2,
                            },
                            if *detected { &p_brush } else { &s_brush },
                        );
                    }
                }
                WidgetChipSpec::AiAntigravity { detected } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    // AiUsageWidget.cs Antigravity: [Logo 16x16] + Gemini | Claude columns
                    design::brand_paths::draw_brand_logo(
                        &self.d2d_factory,
                        dc,
                        "aiusage_antigravity",
                        D2D_RECT_F {
                            left: 10.0,
                            top: cy - 8.0,
                            right: 26.0,
                            bottom: cy + 8.0,
                        },
                        pal.text_primary,
                        if *detected { 0.98 } else { 0.65 },
                    )?;
                    let cols = [("Gemini", 34.0), ("Claude", 98.0)];
                    let mid = height_dips * 0.5;
                    for (i, (name, cx_l)) in cols.iter().enumerate() {
                        if i > 0 {
                            draw_vdiv(cx_l - 6.0, 22.0);
                        }
                        draw_text(
                            name,
                            &fmt_9_b_l,
                            D2D_RECT_F {
                                left: *cx_l,
                                top: (mid - 13.0).max(2.0),
                                right: cx_l + 52.0,
                                bottom: mid - 1.0,
                            },
                            &p_brush,
                        );
                        let by = mid + 3.5;
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: *cx_l,
                                    top: by,
                                    right: cx_l + 32.0,
                                    bottom: by + 3.5,
                                },
                                radiusX: 1.75,
                                radiusY: 1.75,
                            },
                            &tr_brush,
                        );
                        draw_text(
                            "—",
                            &fmt_9_l,
                            D2D_RECT_F {
                                left: cx_l + 35.0,
                                top: mid - 1.0,
                                right: cx_l + 54.0,
                                bottom: mid + 11.0,
                            },
                            &s_brush,
                        );
                    }
                }
                WidgetChipSpec::AiOpenCode { detected } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    // AiUsageWidget.cs OpenCode: [Logo 16x16] [OpenCode 12px] + [1h / 7d] mini columns
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
                        if *detected { 0.98 } else { 0.65 },
                    )?;
                    draw_text(
                        "OpenCode",
                        &fmt_12_b_l,
                        D2D_RECT_F {
                            left: 32.0,
                            top: 0.0,
                            right: 92.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                    );
                    let cols = [("1h", 96.0), ("7d", 142.0)];
                    let mid = height_dips * 0.5;
                    for (lbl, cx_l) in cols {
                        draw_text(
                            lbl,
                            &fmt_95_b_l,
                            D2D_RECT_F {
                                left: cx_l,
                                top: (mid - 13.0).max(2.0),
                                right: cx_l + 38.0,
                                bottom: mid - 1.0,
                            },
                            &s_brush,
                        );
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: cx_l,
                                    top: mid + 2.0,
                                    right: cx_l + 38.0,
                                    bottom: mid + 5.0,
                                },
                                radiusX: 1.5,
                                radiusY: 1.5,
                            },
                            &tr_brush,
                        );
                    }
                }
                WidgetChipSpec::AiCopilot { detected } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    // AiUsageWidget.cs Copilot: [Logo 16x16] [Copilot 12px] | [Status / Monthly] [31x4 track]
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
                        if *detected { 0.98 } else { 0.65 },
                    )?;
                    draw_text(
                        "Copilot",
                        &fmt_12_b_l,
                        D2D_RECT_F {
                            left: 32.0,
                            top: 0.0,
                            right: 78.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                    );
                    draw_vdiv(81.0, 20.0);
                    let mid = height_dips * 0.51;
                    draw_text(
                        if *detected { "Ready" } else { "—" },
                        &fmt_115_b_l,
                        D2D_RECT_F {
                            left: 88.0,
                            top: (mid - 13.5).max(2.0),
                            right: width_dips - 40.0,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                    );
                    draw_text(
                        if *detected { "Monthly" } else { "Sign in" },
                        &fmt_9_l,
                        D2D_RECT_F {
                            left: 88.0,
                            top: mid - 0.5,
                            right: width_dips - 40.0,
                            bottom: (mid + 12.0).min(height_dips - 2.0),
                        },
                        &s_brush,
                    );
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: width_dips - 37.0,
                                top: cy - 2.0,
                                right: width_dips - 8.0,
                                bottom: cy + 2.0,
                            },
                            radiusX: 2.0,
                            radiusY: 2.0,
                        },
                        &tr_brush,
                    );
                }
                WidgetChipSpec::AiBalance {
                    provider_id,
                    name,
                    brand_color,
                    value_text,
                } => {
                    // Exact C# AiUsageWidget.cs DeepSeek / OpenRouter tinted balance card!
                    let card_rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: 0.0,
                            top: 0.0,
                            right: width_dips,
                            bottom: height_dips,
                        },
                        radiusX: 12.0,
                        radiusY: 12.0,
                    };
                    let tint_b = dc.CreateSolidColorBrush(
                        &color_rgba(brand_color.0, brand_color.1, brand_color.2, 26.0 / 255.0),
                        None,
                    )?;
                    dc.FillRoundedRectangle(&card_rr, &tint_b);
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;

                    let well = 29.0_f32.min(height_dips - 6.0);
                    let wy = cy - well * 0.5;
                    let well_rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: 8.0,
                            top: wy,
                            right: 8.0 + well,
                            bottom: wy + well,
                        },
                        radiusX: well * 0.5,
                        radiusY: well * 0.5,
                    };
                    let well_b = dc.CreateSolidColorBrush(
                        &color_rgba(brand_color.0, brand_color.1, brand_color.2, 52.0 / 255.0),
                        None,
                    )?;
                    dc.FillRoundedRectangle(&well_rr, &well_b);
                    design::brand_paths::draw_brand_logo(
                        &self.d2d_factory,
                        dc,
                        provider_id,
                        D2D_RECT_F {
                            left: 8.0 + (well - 15.0) * 0.5,
                            top: wy + (well - 15.0) * 0.5,
                            right: 8.0 + (well + 15.0) * 0.5,
                            bottom: wy + (well + 15.0) * 0.5,
                        },
                        *brand_color,
                        0.98,
                    )?;

                    let tx = 8.0 + well + 9.0;
                    let mid = height_dips * 0.45;
                    draw_text(
                        name,
                        &fmt_95_b_l,
                        D2D_RECT_F {
                            left: tx,
                            top: (mid - 12.0).max(2.0),
                            right: width_dips - 8.0,
                            bottom: mid,
                        },
                        &s_brush,
                    );
                    draw_text(
                        value_text,
                        &fmt_15_b_l,
                        D2D_RECT_F {
                            left: tx,
                            top: mid - 1.0,
                            right: width_dips - 8.0,
                            bottom: (mid + 17.0).min(height_dips - 2.0),
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
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
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
                &HSTRING::from("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_MEDIUM,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                10.5,
                &HSTRING::from("en-US"),
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
    ) -> Result<PillNodes> {
        let h = pill_h_px.ceil().max(4.0) as u32;
        let r = radius_px.clamp(2.0, pill_h_px * 0.5);
        let cap_w = (r.ceil() as u32).max(2);
        let bw = dpi.max(1.0);
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
            if border.3 > 0.0 {
                dc.DrawRoundedRectangle(&rr, &bd_brush, bw, None);
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
            if border.3 > 0.0 {
                dc.DrawLine(
                    Vector2 { X: 0.0, Y: half_bw },
                    Vector2 { X: 1.0, Y: half_bw },
                    &bd_brush,
                    bw,
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
                    bw,
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
            if border.3 > 0.0 {
                dc.DrawRoundedRectangle(&rr, &bd_brush, bw, None);
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
        let mut premul = raw.pixels.clone();
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

        let pal = design::palette(is_dark);
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
            let bw = dpi.max(1.0);
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

/// Exact C# `WidgetCardBorder` / `WidgetCardButton`: completely transparent at rest (`#00000000`),
/// smooth translucent hover (`#22FFFFFF` dark / `#18000000` light) and press (`#38FFFFFF` / `#2A000000`),
/// with `WidgetRadius = 12.0` and zero borders.
unsafe fn draw_chip_card(
    dc: &ID2D1DeviceContext,
    width_dips: f32,
    height_dips: f32,
    is_dark: bool,
    _active_tint: Option<Rgb>,
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
            &HSTRING::from("en-US"),
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
