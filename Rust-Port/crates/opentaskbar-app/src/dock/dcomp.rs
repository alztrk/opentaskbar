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
    IDCompositionSurface, IDCompositionTarget, IDCompositionVisual, IDCompositionVisual3,
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
    SunCloud,
    Bluetooth,
    Drive,
    Timer,
    Folder,
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetInteraction {
    Idle,
    Hovered(Option<u8>),
    Pressed(Option<u8>),
}

/// Self-contained, directly glanceable & interactive dock widget archetypes.
#[derive(Clone, Debug)]
pub enum WidgetChipSpec {
    DateTime {
        time: String,
        date: String,
    },
    IconTwoLine {
        icon: ChipIcon,
        icon_color: Rgb,
        top: String,
        bottom: String,
        bottom_color: Option<Rgb>,
        active_tint: Option<Rgb>,
        bar: Option<(f32, Rgb)>,
    },
    RingTwoLine {
        ratio: f32,
        ring_color: Rgb,
        top: String,
        bottom: String,
        bottom_color: Option<Rgb>,
        active_tint: Option<Rgb>,
    },
    DualBars {
        top_label: String,
        top_val: String,
        top_ratio: f32,
        top_color: Rgb,
        bot_label: String,
        bot_val: String,
        bot_ratio: f32,
        bot_color: Rgb,
    },
    NetTraffic {
        down: String,
        up: String,
    },
    BatteryGraphic {
        percent: u32,
        has_battery: bool,
        charging: bool,
        top: String,
        bottom: String,
        level_color: Rgb,
    },
    AudioSlider {
        volume: u32,
        muted: bool,
    },
    InlineControls {
        /// `(fluent_glyph, highlighted, highlight_color)`
        buttons: Vec<(String, bool, Rgb)>,
    },
    ProviderBadge {
        name: String,
        monogram: String,
        color: Rgb,
        detected: bool,
    },
    Keycap(String),
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

    /// Renders one of the rich, self-contained dock widget modules at automatic `height_dips`
    /// derived from the live dock pill height, using unified Zinc tokens and Windows 11 Fluent icons.
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

        let fmt_14_c = mk_fmt(14.0, true, 0)?;
        let fmt_12_l = mk_fmt(12.0, true, 1)?;
        let fmt_11_b_c = mk_fmt(11.0, true, 0)?;
        let fmt_11_b_l = mk_fmt(11.0, true, 1)?;
        let fmt_11_b_r = mk_fmt(11.0, true, 2)?;
        let fmt_10_c = mk_fmt(10.5, false, 0)?;
        let fmt_10_l = mk_fmt(10.0, false, 1)?;
        let fmt_95_l = mk_fmt(9.5, true, 1)?;

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

        self.draw_surface(w_px, h_px, |dc, _w, _h| unsafe {
            let p_brush = dc.CreateSolidColorBrush(&fg_p, None)?;
            let s_brush = dc.CreateSolidColorBrush(&fg_s, None)?;
            let tr_brush = dc.CreateSolidColorBrush(&track_c, None)?;
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
            match spec {
                WidgetChipSpec::DateTime { time, date } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    let tw: Vec<u16> = time.encode_utf16().collect();
                    let dw: Vec<u16> = date.encode_utf16().collect();
                    let mid = height_dips * 0.53;
                    dc.DrawText(
                        &tw,
                        &fmt_14_c,
                        &D2D_RECT_F {
                            left: 4.0,
                            top: 2.0,
                            right: width_dips - 4.0,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                    dc.DrawText(
                        &dw,
                        &fmt_10_c,
                        &D2D_RECT_F {
                            left: 4.0,
                            top: mid - 1.0,
                            right: width_dips - 4.0,
                            bottom: height_dips - 2.0,
                        },
                        &s_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::IconTwoLine {
                    icon,
                    icon_color,
                    top,
                    bottom,
                    bottom_color,
                    active_tint,
                    bar,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        *active_tint,
                        interaction,
                    )?;
                    // Subtle rounded icon well on the left
                    let well = 24.0_f32.min(height_dips - 8.0);
                    let wx = 7.0;
                    let wy = (height_dips - well) * 0.5;
                    let well_c = color_rgba(icon_color.0, icon_color.1, icon_color.2, 0.16);
                    let well_b = dc.CreateSolidColorBrush(&well_c, None)?;
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: wx,
                                top: wy,
                                right: wx + well,
                                bottom: wy + well,
                            },
                            radiusX: 7.0,
                            radiusY: 7.0,
                        },
                        &well_b,
                    );
                    draw_chip_icon(
                        &self.dwrite_factory,
                        dc,
                        *icon,
                        wx + well * 0.5,
                        cy,
                        14.0,
                        *icon_color,
                    )?;

                    let tx = wx + well + 7.0;
                    let tr = width_dips - 7.0;
                    let mid = height_dips * 0.52;
                    let tw: Vec<u16> = top.encode_utf16().collect();
                    dc.DrawText(
                        &tw,
                        &fmt_11_b_l,
                        &D2D_RECT_F {
                            left: tx,
                            top: 3.0,
                            right: tr,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );

                    if let Some((ratio, bar_rgb)) = bar {
                        let bar_h = 4.0;
                        let bar_top = mid + (height_dips - mid - bar_h) * 0.42;
                        let bar_r = (tr - tx).max(16.0);
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: tx,
                                    top: bar_top,
                                    right: tx + bar_r,
                                    bottom: bar_top + bar_h,
                                },
                                radiusX: 2.0,
                                radiusY: 2.0,
                            },
                            &tr_brush,
                        );
                        let fw = (bar_r * ratio.clamp(0.04, 1.0)).max(3.0);
                        let fb = dc.CreateSolidColorBrush(
                            &color_rgba(bar_rgb.0, bar_rgb.1, bar_rgb.2, 1.0),
                            None,
                        )?;
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: tx,
                                    top: bar_top,
                                    right: tx + fw,
                                    bottom: bar_top + bar_h,
                                },
                                radiusX: 2.0,
                                radiusY: 2.0,
                            },
                            &fb,
                        );
                    } else {
                        let bw: Vec<u16> = bottom.encode_utf16().collect();
                        let bb = if let Some(c) = bottom_color {
                            dc.CreateSolidColorBrush(&color_rgba(c.0, c.1, c.2, 0.95), None)?
                        } else {
                            s_brush.clone()
                        };
                        dc.DrawText(
                            &bw,
                            &fmt_10_l,
                            &D2D_RECT_F {
                                left: tx,
                                top: mid - 1.0,
                                right: tr,
                                bottom: height_dips - 2.0,
                            },
                            &bb,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                }
                WidgetChipSpec::RingTwoLine {
                    ratio,
                    ring_color,
                    top,
                    bottom,
                    bottom_color,
                    active_tint,
                } => {
                    draw_chip_card(
                        dc,
                        width_dips,
                        height_dips,
                        is_dark,
                        *active_tint,
                        interaction,
                    )?;
                    let rx = 18.0;
                    let rad = 9.0_f32.min(height_dips * 0.30);
                    let rc = color_rgba(ring_color.0, ring_color.1, ring_color.2, 1.0);
                    let r_brush = dc.CreateSolidColorBrush(&rc, None)?;
                    let inner_fill = color_rgba(ring_color.0, ring_color.1, ring_color.2, 0.10);
                    let if_brush = dc.CreateSolidColorBrush(&inner_fill, None)?;
                    let ell = D2D1_ELLIPSE {
                        point: Vector2 { X: rx, Y: cy },
                        radiusX: rad,
                        radiusY: rad,
                    };
                    dc.FillEllipse(&ell, &if_brush);
                    dc.DrawEllipse(&ell, &tr_brush, 2.2, None);
                    let steps = ((ratio.clamp(0.0, 1.0) * 36.0).round() as usize).max(1);
                    if *ratio > 0.01 {
                        let mut prev_pt = Vector2 { X: rx, Y: cy - rad };
                        for i in 1..=steps {
                            let ang = -std::f32::consts::FRAC_PI_2
                                + (i as f32 / 36.0) * std::f32::consts::TAU;
                            let pt = Vector2 {
                                X: rx + ang.cos() * rad,
                                Y: cy + ang.sin() * rad,
                            };
                            dc.DrawLine(prev_pt, pt, &r_brush, 2.2, Some(&stroke));
                            prev_pt = pt;
                        }
                    }

                    let tx = rx + rad + 8.0;
                    let tr = width_dips - 7.0;
                    let mid = height_dips * 0.52;
                    let tw: Vec<u16> = top.encode_utf16().collect();
                    dc.DrawText(
                        &tw,
                        &fmt_12_l,
                        &D2D_RECT_F {
                            left: tx,
                            top: 2.0,
                            right: tr,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                    let bw: Vec<u16> = bottom.encode_utf16().collect();
                    let bb = if let Some(c) = bottom_color {
                        dc.CreateSolidColorBrush(&color_rgba(c.0, c.1, c.2, 0.95), None)?
                    } else {
                        s_brush.clone()
                    };
                    dc.DrawText(
                        &bw,
                        &fmt_10_l,
                        &D2D_RECT_F {
                            left: tx,
                            top: mid - 1.0,
                            right: tr,
                            bottom: height_dips - 2.0,
                        },
                        &bb,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::DualBars {
                    top_label,
                    top_val,
                    top_ratio,
                    top_color,
                    bot_label,
                    bot_val,
                    bot_ratio,
                    bot_color,
                } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    let rows = [
                        (
                            top_label,
                            top_val,
                            *top_ratio,
                            *top_color,
                            3.0,
                            height_dips * 0.5,
                        ),
                        (
                            bot_label,
                            bot_val,
                            *bot_ratio,
                            *bot_color,
                            height_dips * 0.5,
                            height_dips - 3.0,
                        ),
                    ];
                    let lbl_l = 8.0;
                    let bar_l = 35.0;
                    let val_r = width_dips - 8.0;
                    let bar_r = (val_r - 32.0).max(bar_l + 18.0);
                    for (lbl, val, ratio, col, y1, y2) in rows {
                        let lw: Vec<u16> = lbl.encode_utf16().collect();
                        dc.DrawText(
                            &lw,
                            &fmt_95_l,
                            &D2D_RECT_F {
                                left: lbl_l,
                                top: y1,
                                right: bar_l - 2.0,
                                bottom: y2,
                            },
                            &s_brush,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                        );
                        let bh = 4.0;
                        let by = (y1 + y2 - bh) * 0.5;
                        let bw = bar_r - bar_l;
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: bar_l,
                                    top: by,
                                    right: bar_r,
                                    bottom: by + bh,
                                },
                                radiusX: 2.0,
                                radiusY: 2.0,
                            },
                            &tr_brush,
                        );
                        let fw = (bw * ratio.clamp(0.05, 1.0)).max(3.0);
                        let fb =
                            dc.CreateSolidColorBrush(&color_rgba(col.0, col.1, col.2, 1.0), None)?;
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: bar_l,
                                    top: by,
                                    right: bar_l + fw,
                                    bottom: by + bh,
                                },
                                radiusX: 2.0,
                                radiusY: 2.0,
                            },
                            &fb,
                        );
                        let vw: Vec<u16> = val.encode_utf16().collect();
                        dc.DrawText(
                            &vw,
                            &fmt_11_b_r,
                            &D2D_RECT_F {
                                left: bar_r + 2.0,
                                top: y1,
                                right: val_r,
                                bottom: y2,
                            },
                            &p_brush,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                }
                WidgetChipSpec::NetTraffic { down, up } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    let mid = height_dips * 0.5;
                    let down_dot = dc.CreateSolidColorBrush(
                        &color_rgba(pal.success.0, pal.success.1, pal.success.2, 1.0),
                        None,
                    )?;
                    let up_dot = dc.CreateSolidColorBrush(
                        &color_rgba(pal.accent.0, pal.accent.1, pal.accent.2, 1.0),
                        None,
                    )?;
                    let y_top = (3.0 + mid) * 0.5;
                    let y_bot = (mid + height_dips - 3.0) * 0.5;
                    dc.FillEllipse(
                        &D2D1_ELLIPSE {
                            point: Vector2 { X: 11.0, Y: y_top },
                            radiusX: 2.6,
                            radiusY: 2.6,
                        },
                        &down_dot,
                    );
                    dc.FillEllipse(
                        &D2D1_ELLIPSE {
                            point: Vector2 { X: 11.0, Y: y_bot },
                            radiusX: 2.6,
                            radiusY: 2.6,
                        },
                        &up_dot,
                    );
                    let dw: Vec<u16> = format!("↓ {down}").encode_utf16().collect();
                    let uw: Vec<u16> = format!("↑ {up}").encode_utf16().collect();
                    dc.DrawText(
                        &dw,
                        &fmt_11_b_l,
                        &D2D_RECT_F {
                            left: 19.0,
                            top: 3.0,
                            right: width_dips - 6.0,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                    dc.DrawText(
                        &uw,
                        &fmt_10_l,
                        &D2D_RECT_F {
                            left: 19.0,
                            top: mid - 1.0,
                            right: width_dips - 6.0,
                            bottom: height_dips - 3.0,
                        },
                        &s_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::BatteryGraphic {
                    percent,
                    has_battery,
                    charging,
                    top,
                    bottom,
                    level_color,
                } => {
                    let active = charging.then_some(pal.success);
                    draw_chip_card(dc, width_dips, height_dips, is_dark, active, interaction)?;
                    // Crisp horizontal battery glyph (24x13 body + 2.5x5 nub)
                    let bx = 8.0;
                    let bw = 24.0;
                    let bh = 13.0;
                    let by = cy - bh * 0.5;
                    let body_rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: bx,
                            top: by,
                            right: bx + bw,
                            bottom: by + bh,
                        },
                        radiusX: 3.2,
                        radiusY: 3.2,
                    };
                    dc.DrawRoundedRectangle(&body_rr, &s_brush, 1.35, None);
                    // Positive terminal nub
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: bx + bw + 1.2,
                                top: cy - 2.6,
                                right: bx + bw + 3.4,
                                bottom: cy + 2.6,
                            },
                            radiusX: 1.0,
                            radiusY: 1.0,
                        },
                        &s_brush,
                    );
                    // Live interior fill bar
                    let ratio = if *has_battery {
                        (*percent as f32 / 100.0).clamp(0.08, 1.0)
                    } else {
                        1.0
                    };
                    let fill_col = if *has_battery {
                        *level_color
                    } else {
                        pal.accent
                    };
                    let fb = dc.CreateSolidColorBrush(
                        &color_rgba(fill_col.0, fill_col.1, fill_col.2, 0.95),
                        None,
                    )?;
                    let inner_max = bw - 4.4;
                    let inner_w = (inner_max * ratio).max(3.0);
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: bx + 2.2,
                                top: by + 2.2,
                                right: bx + 2.2 + inner_w,
                                bottom: by + bh - 2.2,
                            },
                            radiusX: 1.6,
                            radiusY: 1.6,
                        },
                        &fb,
                    );

                    let tx = bx + bw + 9.0;
                    let tr = width_dips - 6.0;
                    let mid = height_dips * 0.52;
                    let tw: Vec<u16> = top.encode_utf16().collect();
                    let botw: Vec<u16> = bottom.encode_utf16().collect();
                    dc.DrawText(
                        &tw,
                        &fmt_12_l,
                        &D2D_RECT_F {
                            left: tx,
                            top: 2.0,
                            right: tr,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                    dc.DrawText(
                        &botw,
                        &fmt_10_l,
                        &D2D_RECT_F {
                            left: tx,
                            top: mid - 1.0,
                            right: tr,
                            bottom: height_dips - 2.0,
                        },
                        &s_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::AudioSlider { volume, muted } => {
                    let active = muted.then_some(pal.danger);
                    draw_chip_card(dc, width_dips, height_dips, is_dark, active, interaction)?;
                    // Left clickable mute icon well
                    let well = 24.0_f32.min(height_dips - 8.0);
                    let wx = 7.0;
                    let wy = (height_dips - well) * 0.5;
                    let well_rgb = if *muted { pal.danger } else { pal.accent };
                    let well_alpha = if active_sub == Some(0) { 0.30 } else { 0.16 };
                    let well_b = dc.CreateSolidColorBrush(
                        &color_rgba(well_rgb.0, well_rgb.1, well_rgb.2, well_alpha),
                        None,
                    )?;
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: wx,
                                top: wy,
                                right: wx + well,
                                bottom: wy + well,
                            },
                            radiusX: 7.0,
                            radiusY: 7.0,
                        },
                        &well_b,
                    );
                    let glyph = if *muted {
                        "\u{E74F}"
                    } else if *volume == 0 {
                        "\u{E992}"
                    } else if *volume < 34 {
                        "\u{E993}"
                    } else if *volume < 67 {
                        "\u{E994}"
                    } else {
                        "\u{E995}"
                    };
                    let ic_col = if *muted {
                        pal.danger_fg
                    } else {
                        pal.text_primary
                    };
                    draw_fluent_str_d2d(
                        &self.dwrite_factory,
                        dc,
                        glyph,
                        D2D_RECT_F {
                            left: wx,
                            top: wy,
                            right: wx + well,
                            bottom: wy + well,
                        },
                        14.0,
                        ic_col,
                        0.98,
                    )?;

                    // Right side: Volume label + interactive slider track & thumb
                    let sx = wx + well + 8.0;
                    let sr = width_dips - 9.0;
                    let mid = height_dips * 0.50;
                    let lbl = if *muted {
                        "Muted".to_owned()
                    } else {
                        format!("{volume}%")
                    };
                    let lw: Vec<u16> = lbl.encode_utf16().collect();
                    dc.DrawText(
                        &lw,
                        &fmt_11_b_l,
                        &D2D_RECT_F {
                            left: sx,
                            top: 2.0,
                            right: sr,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                    let track_y = mid + (height_dips - mid) * 0.45;
                    let track_h = 4.0;
                    let track_w = (sr - sx).max(20.0);
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: sx,
                                top: track_y - track_h * 0.5,
                                right: sx + track_w,
                                bottom: track_y + track_h * 0.5,
                            },
                            radiusX: 2.0,
                            radiusY: 2.0,
                        },
                        &tr_brush,
                    );
                    let ratio = if *muted {
                        0.0
                    } else {
                        (*volume as f32 / 100.0).clamp(0.0, 1.0)
                    };
                    let fill_w = track_w * ratio;
                    if fill_w > 1.0 {
                        let ac = dc.CreateSolidColorBrush(
                            &color_rgba(pal.accent.0, pal.accent.1, pal.accent.2, 1.0),
                            None,
                        )?;
                        dc.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: sx,
                                    top: track_y - track_h * 0.5,
                                    right: sx + fill_w,
                                    bottom: track_y + track_h * 0.5,
                                },
                                radiusX: 2.0,
                                radiusY: 2.0,
                            },
                            &ac,
                        );
                    }
                    // Crisp circular slider thumb (enlarges smoothly when slider region is hovered)
                    let thumb_x = (sx + fill_w).clamp(sx + 3.5, sr - 3.5);
                    let thumb_r = if active_sub == Some(1) { 4.6 } else { 3.6 };
                    dc.FillEllipse(
                        &D2D1_ELLIPSE {
                            point: Vector2 {
                                X: thumb_x,
                                Y: track_y,
                            },
                            radiusX: thumb_r,
                            radiusY: thumb_r,
                        },
                        &p_brush,
                    );
                }
                WidgetChipSpec::InlineControls { buttons } => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    let count = buttons.len().max(1) as f32;
                    let pad = 5.0;
                    let gap = 4.0;
                    let bw = ((width_dips - pad * 2.0 - gap * (count - 1.0)) / count).max(20.0);
                    let bh = (height_dips - 8.0).clamp(22.0, 32.0);
                    let by = (height_dips - bh) * 0.5;
                    for (i, (glyph, highlighted, hi_rgb)) in buttons.iter().enumerate() {
                        let bx = pad + (i as f32) * (bw + gap);
                        let rect = D2D_RECT_F {
                            left: bx,
                            top: by,
                            right: bx + bw,
                            bottom: by + bh,
                        };
                        let sub_hovered = active_sub == Some(i as u8);
                        if *highlighted || sub_hovered {
                            let bg_col = if *highlighted {
                                color_rgba(
                                    hi_rgb.0,
                                    hi_rgb.1,
                                    hi_rgb.2,
                                    if sub_hovered { 0.34 } else { 0.22 },
                                )
                            } else if is_dark {
                                color_rgba(0xff, 0xff, 0xff, 0.12)
                            } else {
                                color_rgba(0x18, 0x18, 0x1b, 0.09)
                            };
                            let bg = dc.CreateSolidColorBrush(&bg_col, None)?;
                            let rr = D2D1_ROUNDED_RECT {
                                rect,
                                radiusX: 7.0,
                                radiusY: 7.0,
                            };
                            dc.FillRoundedRectangle(&rr, &bg);
                            if *highlighted {
                                let bd = dc.CreateSolidColorBrush(
                                    &color_rgba(hi_rgb.0, hi_rgb.1, hi_rgb.2, 0.50),
                                    None,
                                )?;
                                dc.DrawRoundedRectangle(&rr, &bd, 1.0, None);
                            }
                        }
                        let ic_col = if *highlighted {
                            *hi_rgb
                        } else {
                            pal.text_primary
                        };
                        draw_fluent_str_d2d(
                            &self.dwrite_factory,
                            dc,
                            glyph,
                            rect,
                            13.5,
                            ic_col,
                            0.96,
                        )?;
                    }
                }
                WidgetChipSpec::ProviderBadge {
                    name,
                    monogram,
                    color,
                    detected,
                } => {
                    let active = detected.then_some(*color);
                    draw_chip_card(dc, width_dips, height_dips, is_dark, active, interaction)?;
                    let bsz = 23.0_f32.min(height_dips - 8.0);
                    let bx = 7.0;
                    let by = (height_dips - bsz) * 0.5;
                    let badge_rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: bx,
                            top: by,
                            right: bx + bsz,
                            bottom: by + bsz,
                        },
                        radiusX: 7.0,
                        radiusY: 7.0,
                    };
                    let alpha_bg = if *detected { 0.22 } else { 0.10 };
                    let alpha_bd = if *detected { 0.55 } else { 0.24 };
                    let bg_b = dc.CreateSolidColorBrush(
                        &color_rgba(color.0, color.1, color.2, alpha_bg),
                        None,
                    )?;
                    let bd_b = dc.CreateSolidColorBrush(
                        &color_rgba(color.0, color.1, color.2, alpha_bd),
                        None,
                    )?;
                    let fg_b = dc.CreateSolidColorBrush(
                        &color_rgba(
                            color.0,
                            color.1,
                            color.2,
                            if *detected { 1.0 } else { 0.65 },
                        ),
                        None,
                    )?;
                    dc.FillRoundedRectangle(&badge_rr, &bg_b);
                    dc.DrawRoundedRectangle(&badge_rr, &bd_b, 1.0, None);
                    let mw: Vec<u16> = monogram.encode_utf16().collect();
                    dc.DrawText(
                        &mw,
                        &fmt_11_b_c,
                        &badge_rr.rect,
                        &fg_b,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                    if *detected {
                        let ring_b = dc.CreateSolidColorBrush(
                            &color_rgba(pal.dock_pill.0, pal.dock_pill.1, pal.dock_pill.2, 0.95),
                            None,
                        )?;
                        let ok_b = dc.CreateSolidColorBrush(
                            &color_rgba(pal.success.0, pal.success.1, pal.success.2, 1.0),
                            None,
                        )?;
                        let pt = Vector2 {
                            X: bx + bsz - 1.5,
                            Y: by + bsz - 1.5,
                        };
                        dc.FillEllipse(
                            &D2D1_ELLIPSE {
                                point: pt,
                                radiusX: 4.1,
                                radiusY: 4.1,
                            },
                            &ring_b,
                        );
                        dc.FillEllipse(
                            &D2D1_ELLIPSE {
                                point: pt,
                                radiusX: 2.8,
                                radiusY: 2.8,
                            },
                            &ok_b,
                        );
                    }

                    let tx = bx + bsz + 7.0;
                    let tr = width_dips - 6.0;
                    let mid = height_dips * 0.52;
                    let nw: Vec<u16> = name.encode_utf16().collect();
                    dc.DrawText(
                        &nw,
                        &fmt_11_b_l,
                        &D2D_RECT_F {
                            left: tx,
                            top: 2.0,
                            right: tr,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                    let status_s = if *detected { "Ready" } else { "Sign in" };
                    let sw: Vec<u16> = status_s.encode_utf16().collect();
                    let status_b = if *detected {
                        dc.CreateSolidColorBrush(
                            &color_rgba(pal.success.0, pal.success.1, pal.success.2, 0.95),
                            None,
                        )?
                    } else {
                        s_brush.clone()
                    };
                    dc.DrawText(
                        &sw,
                        &fmt_10_l,
                        &D2D_RECT_F {
                            left: tx,
                            top: mid - 1.0,
                            right: tr,
                            bottom: height_dips - 2.0,
                        },
                        &status_b,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::Keycap(code) => {
                    draw_chip_card(dc, width_dips, height_dips, is_dark, None, interaction)?;
                    let kw = 34.0_f32.min(width_dips - 10.0);
                    let kh = 24.0_f32.min(height_dips - 8.0);
                    let kl = (width_dips - kw) * 0.5;
                    let kt = (height_dips - kh) * 0.5;
                    let rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: kl,
                            top: kt,
                            right: kl + kw,
                            bottom: kt + kh,
                        },
                        radiusX: 6.0,
                        radiusY: 6.0,
                    };
                    let key_bg = if is_dark {
                        color_rgba(0xff, 0xff, 0xff, 0.08)
                    } else {
                        color_rgba(0x18, 0x18, 0x1b, 0.06)
                    };
                    let kb = dc.CreateSolidColorBrush(&key_bg, None)?;
                    dc.FillRoundedRectangle(&rr, &kb);
                    dc.DrawRoundedRectangle(&rr, &tr_brush, 1.0, None);
                    // Subtle bottom keycap bevel line
                    dc.DrawLine(
                        Vector2 {
                            X: kl + 4.0,
                            Y: kt + kh - 1.5,
                        },
                        Vector2 {
                            X: kl + kw - 4.0,
                            Y: kt + kh - 1.5,
                        },
                        &tr_brush,
                        1.5,
                        Some(&stroke),
                    );
                    let wtxt: Vec<u16> = code.encode_utf16().collect();
                    dc.DrawText(
                        &wtxt,
                        &fmt_11_b_c,
                        &D2D_RECT_F {
                            left: kl,
                            top: kt - 1.0,
                            right: kl + kw,
                            bottom: kt + kh - 1.0,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
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

unsafe fn draw_chip_card(
    dc: &ID2D1DeviceContext,
    width_dips: f32,
    height_dips: f32,
    is_dark: bool,
    active_tint: Option<Rgb>,
    interaction: WidgetInteraction,
) -> Result<()> {
    let (lift, bd_boost) = match interaction {
        WidgetInteraction::Idle => (0.0_f32, 0.0_f32),
        WidgetInteraction::Hovered(_) => (0.045, 0.08),
        WidgetInteraction::Pressed(_) => (0.085, 0.14),
    };
    let (fill_c, border_c, spec_c) = if let Some(t) = active_tint {
        (
            color_rgba(
                t.0,
                t.1,
                t.2,
                (if is_dark { 0.14 } else { 0.11 } + lift).min(0.42),
            ),
            color_rgba(
                t.0,
                t.1,
                t.2,
                (if is_dark { 0.36 } else { 0.30 } + bd_boost).min(0.68),
            ),
            color_rgba(0xff, 0xff, 0xff, if is_dark { 0.14 } else { 0.48 }),
        )
    } else if is_dark {
        (
            color_rgba(0xff, 0xff, 0xff, 0.055 + lift),
            color_rgba(0xff, 0xff, 0xff, 0.09 + bd_boost),
            color_rgba(0xff, 0xff, 0xff, 0.09 + lift * 0.6),
        )
    } else {
        (
            color_rgba(0x18, 0x18, 0x1b, 0.042 + lift * 0.7),
            color_rgba(0x18, 0x18, 0x1b, 0.085 + bd_boost * 0.7),
            color_rgba(0xff, 0xff, 0xff, 0.68),
        )
    };
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: 0.5,
            top: 0.5,
            right: width_dips - 0.5,
            bottom: height_dips - 0.5,
        },
        radiusX: 9.0,
        radiusY: 9.0,
    };
    unsafe {
        let fb = dc.CreateSolidColorBrush(&fill_c, None)?;
        let bb = dc.CreateSolidColorBrush(&border_c, None)?;
        let sb = dc.CreateSolidColorBrush(&spec_c, None)?;
        dc.FillRoundedRectangle(&rr, &fb);
        dc.DrawLine(
            Vector2 { X: 9.0, Y: 1.5 },
            Vector2 {
                X: (width_dips - 9.0).max(9.0),
                Y: 1.5,
            },
            &sb,
            1.0,
            None,
        );
        dc.DrawRoundedRectangle(&rr, &bb, 1.0, None);
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
        ChipIcon::SunCloud => AppIcon::SunCloud,
        ChipIcon::Bluetooth => AppIcon::Bluetooth,
        ChipIcon::Drive => AppIcon::HardDrive,
        ChipIcon::Timer => AppIcon::Timer,
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
