//! Native DirectComposition + Direct2D + DirectWrite visual engine for the floating dock.
//!
//! All icons, pill caps, shell glyphs, indicator dots, and tooltips are drawn ONCE onto static
//! `IDCompositionSurface` textures. During 60-180 FPS fisheye, bounce, and drag animations, only
//! 2D affine transforms (`IDCompositionMatrixTransform`) are updated and committed to DWM
//! (`IDCompositionDevice::Commit`) with zero D3D11 draw calls and ~0-1% GPU usage.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_PROPERTIES1, D2D1_CAP_STYLE_ROUND, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LINE_JOIN_ROUND,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES1, D2D1CreateFactory, ID2D1Device,
    ID2D1DeviceContext, ID2D1Factory1, ID2D1StrokeStyle,
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

use crate::design::{self, Rgb};
use crate::platform::icon_cache::RawIcon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipIcon {
    Sliders,
    Music,
    Speaker,
    SpeakerMuted,
    SunCloud,
    Battery,
    Bluetooth,
    Drive,
    Timer,
    Folder,
    Grid,
}

/// Six unified dock chip archetypes replacing the legacy 2-line plain-text box.
#[derive(Clone, Debug)]
pub enum WidgetChipSpec {
    IconOnly(ChipIcon),
    IconValue {
        icon: ChipIcon,
        value: String,
        active_tint: bool,
    },
    Keycap(String),
    ProgressRing {
        ratio: f32,
        value: String,
        color: Rgb,
        active_tint: bool,
    },
    DualMetric {
        top: String,
        bot: String,
        top_color: Option<Rgb>,
        bot_color: Option<Rgb>,
        bars: Option<(f32, f32)>,
    },
    Provider {
        label: String,
        color: Rgb,
        detected: bool,
    },
    DateTime {
        time: String,
        date: String,
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
        let font_name = HSTRING::from("Segoe UI");
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

    /// Renders one of the 6 unified dock chip archetypes at automatic `height_dips` derived
    /// from the live dock pill height, using unified Zinc tokens and integer DirectWrite sizes.
    pub fn create_widget_chip_surface(
        &self,
        spec: &WidgetChipSpec,
        width_dips: f32,
        height_dips: f32,
        dpi: f32,
        is_dark: bool,
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
            0.92,
        );
        let accent_c = color_rgba(pal.accent.0, pal.accent.1, pal.accent.2, 1.0);
        let track_c = if is_dark {
            color_rgba(0xff, 0xff, 0xff, 0.16)
        } else {
            color_rgba(0x18, 0x18, 0x1b, 0.16)
        };
        let tint_c = color_rgba(pal.accent.0, pal.accent.1, pal.accent.2, 0.18);

        let mk_fmt = |size: f32, bold: bool, align_left: bool| -> Result<IDWriteTextFormat> {
            unsafe {
                let fmt = self.dwrite_factory.CreateTextFormat(
                    &HSTRING::from("Segoe UI"),
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
                fmt.SetTextAlignment(if align_left {
                    windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_LEADING
                } else {
                    DWRITE_TEXT_ALIGNMENT_CENTER
                })?;
                fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                Ok(fmt)
            }
        };

        let fmt_13 = mk_fmt(13.0, true, false)?;
        let fmt_12_l = mk_fmt(12.0, true, true)?;
        let fmt_11_c = mk_fmt(11.0, false, false)?;
        let fmt_11_b = mk_fmt(11.0, true, false)?;
        let fmt_11_l = mk_fmt(11.0, true, true)?;

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
            let a_brush = dc.CreateSolidColorBrush(&accent_c, None)?;
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

            let cy = height_dips * 0.5;
            match spec {
                WidgetChipSpec::IconOnly(icon) => {
                    draw_chip_icon(dc, *icon, width_dips * 0.5, cy, 16.0, &p_brush, &stroke)?;
                }
                WidgetChipSpec::IconValue {
                    icon,
                    value,
                    active_tint,
                } => {
                    if *active_tint {
                        let tb = dc.CreateSolidColorBrush(&tint_c, None)?;
                        let rr = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: 1.0,
                                top: 2.0,
                                right: width_dips - 1.0,
                                bottom: height_dips - 2.0,
                            },
                            radiusX: 8.0,
                            radiusY: 8.0,
                        };
                        dc.FillRoundedRectangle(&rr, &tb);
                    }
                    let icon_brush = if *active_tint { &a_brush } else { &p_brush };
                    draw_chip_icon(dc, *icon, 14.0, cy, 14.0, icon_brush, &stroke)?;
                    let wtxt: Vec<u16> = value.encode_utf16().collect();
                    dc.DrawText(
                        &wtxt,
                        &fmt_12_l,
                        &D2D_RECT_F {
                            left: 25.0,
                            top: 0.0,
                            right: width_dips - 4.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::Keycap(code) => {
                    let kw = 28.0_f32.min(width_dips - 8.0);
                    let kh = 22.0_f32.min(height_dips - 6.0);
                    let kl = (width_dips - kw) * 0.5;
                    let kt = (height_dips - kh) * 0.5;
                    let rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: kl,
                            top: kt,
                            right: kl + kw,
                            bottom: kt + kh,
                        },
                        radiusX: 5.0,
                        radiusY: 5.0,
                    };
                    dc.DrawRoundedRectangle(&rr, &tr_brush, 1.25, None);
                    let wtxt: Vec<u16> = code.encode_utf16().collect();
                    dc.DrawText(
                        &wtxt,
                        &fmt_11_b,
                        &rr.rect,
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::ProgressRing {
                    ratio,
                    value,
                    color,
                    active_tint,
                } => {
                    if *active_tint {
                        let tb = dc.CreateSolidColorBrush(&tint_c, None)?;
                        let rr = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: 1.0,
                                top: 2.0,
                                right: width_dips - 1.0,
                                bottom: height_dips - 2.0,
                            },
                            radiusX: 8.0,
                            radiusY: 8.0,
                        };
                        dc.FillRoundedRectangle(&rr, &tb);
                    }
                    let rc = color_rgba(color.0, color.1, color.2, 1.0);
                    let r_brush = dc.CreateSolidColorBrush(&rc, None)?;
                    let rx = 14.0;
                    let rad = 6.5;
                    let ell = D2D1_ELLIPSE {
                        point: Vector2 { X: rx, Y: cy },
                        radiusX: rad,
                        radiusY: rad,
                    };
                    dc.DrawEllipse(&ell, &tr_brush, 2.0, None);
                    // Draw segmented arc approximation (16 crisp steps) around the ring
                    let steps = ((ratio.clamp(0.0, 1.0) * 20.0).round() as usize).max(1);
                    if *ratio > 0.01 {
                        let mut prev_pt = Vector2 { X: rx, Y: cy - rad };
                        for i in 1..=steps {
                            let ang = -std::f32::consts::FRAC_PI_2
                                + (i as f32 / 20.0) * std::f32::consts::TAU;
                            let pt = Vector2 {
                                X: rx + ang.cos() * rad,
                                Y: cy + ang.sin() * rad,
                            };
                            dc.DrawLine(prev_pt, pt, &r_brush, 2.0, Some(&stroke));
                            prev_pt = pt;
                        }
                    }
                    let wtxt: Vec<u16> = value.encode_utf16().collect();
                    dc.DrawText(
                        &wtxt,
                        &fmt_12_l,
                        &D2D_RECT_F {
                            left: 25.0,
                            top: 0.0,
                            right: width_dips - 4.0,
                            bottom: height_dips,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::DualMetric {
                    top,
                    bot,
                    top_color,
                    bot_color,
                    bars,
                } => {
                    let text_left = if let Some((b1, b2)) = bars {
                        let bh = (height_dips - 12.0).clamp(14.0, 24.0);
                        let bt = (height_dips - bh) * 0.5;
                        for (i, ratio) in [b1, b2].into_iter().enumerate() {
                            let bx = 7.0 + (i as f32) * 5.5;
                            let track = D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: bx,
                                    top: bt,
                                    right: bx + 3.0,
                                    bottom: bt + bh,
                                },
                                radiusX: 1.5,
                                radiusY: 1.5,
                            };
                            dc.FillRoundedRectangle(&track, &tr_brush);
                            let fh = (bh * ratio.clamp(0.05, 1.0)).max(2.0);
                            let mc = design::meter_color(&pal, *ratio);
                            let fb =
                                dc.CreateSolidColorBrush(&color_rgba(mc.0, mc.1, mc.2, 1.0), None)?;
                            let fill = D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: bx,
                                    top: bt + bh - fh,
                                    right: bx + 3.0,
                                    bottom: bt + bh,
                                },
                                radiusX: 1.5,
                                radiusY: 1.5,
                            };
                            dc.FillRoundedRectangle(&fill, &fb);
                        }
                        20.0
                    } else {
                        6.0
                    };
                    let tb = if let Some(c) = top_color {
                        dc.CreateSolidColorBrush(&color_rgba(c.0, c.1, c.2, 1.0), None)?
                    } else {
                        p_brush.clone()
                    };
                    let bb = if let Some(c) = bot_color {
                        dc.CreateSolidColorBrush(&color_rgba(c.0, c.1, c.2, 1.0), None)?
                    } else {
                        s_brush.clone()
                    };
                    let tw: Vec<u16> = top.encode_utf16().collect();
                    let bw: Vec<u16> = bot.encode_utf16().collect();
                    let mid = height_dips * 0.5;
                    dc.DrawText(
                        &tw,
                        &fmt_11_l,
                        &D2D_RECT_F {
                            left: text_left,
                            top: 2.0,
                            right: width_dips - 4.0,
                            bottom: mid + 1.0,
                        },
                        &tb,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                    dc.DrawText(
                        &bw,
                        &fmt_11_l,
                        &D2D_RECT_F {
                            left: text_left,
                            top: mid - 1.0,
                            right: width_dips - 4.0,
                            bottom: height_dips - 2.0,
                        },
                        &bb,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::Provider {
                    label,
                    color,
                    detected,
                } => {
                    let alpha = if *detected { 1.0 } else { 0.35 };
                    let dot_c = color_rgba(color.0, color.1, color.2, alpha);
                    let db = dc.CreateSolidColorBrush(&dot_c, None)?;
                    dc.FillEllipse(
                        &D2D1_ELLIPSE {
                            point: Vector2 { X: 11.0, Y: cy },
                            radiusX: 4.0,
                            radiusY: 4.0,
                        },
                        &db,
                    );
                    let lw: Vec<u16> = label.encode_utf16().collect();
                    dc.DrawText(
                        &lw,
                        &fmt_11_l,
                        &D2D_RECT_F {
                            left: 20.0,
                            top: 0.0,
                            right: width_dips - 4.0,
                            bottom: height_dips,
                        },
                        if *detected { &p_brush } else { &s_brush },
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                WidgetChipSpec::DateTime { time, date } => {
                    let tw: Vec<u16> = time.encode_utf16().collect();
                    let dw: Vec<u16> = date.encode_utf16().collect();
                    let mid = height_dips * 0.54;
                    dc.DrawText(
                        &tw,
                        &fmt_13,
                        &D2D_RECT_F {
                            left: 2.0,
                            top: 1.0,
                            right: width_dips - 2.0,
                            bottom: mid + 1.0,
                        },
                        &p_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                    );
                    dc.DrawText(
                        &dw,
                        &fmt_11_c,
                        &D2D_RECT_F {
                            left: 2.0,
                            top: mid - 1.0,
                            right: width_dips - 2.0,
                            bottom: height_dips - 1.0,
                        },
                        &s_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
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
        let bg_col = color_rgba(bg.0, bg.1, bg.2, bg.3);
        let bd_col = color_rgba(border.0, border.1, border.2, border.3);

        let left_surf = self.draw_surface(cap_w, h, |dc, w, fh| unsafe {
            let bg_brush = dc.CreateSolidColorBrush(&bg_col, None)?;
            let bd_brush = dc.CreateSolidColorBrush(&bd_col, None)?;
            // Draw a wider rounded rect clipped to the left cap so the right seam is square.
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
            if border.3 > 0.0 {
                dc.DrawRoundedRectangle(&rr, &bd_brush, bw, None);
            }
            dc.PopAxisAlignedClip();
            Ok(())
        })?;

        let center_surf = self.draw_surface(1, h, |dc, _w, fh| unsafe {
            let bg_brush = dc.CreateSolidColorBrush(&bg_col, None)?;
            let bd_brush = dc.CreateSolidColorBrush(&bd_col, None)?;
            dc.FillRectangle(
                &D2D_RECT_F {
                    left: 0.0,
                    top: half_bw,
                    right: 1.0,
                    bottom: fh - half_bw,
                },
                &bg_brush,
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
            let bg_brush = dc.CreateSolidColorBrush(&bg_col, None)?;
            let bd_brush = dc.CreateSolidColorBrush(&bd_col, None)?;
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
        let fg = if is_dark {
            color_rgba(0xe2, 0xe8, 0xf0, 1.0)
        } else {
            color_rgba(0x1e, 0x29, 0x3b, 1.0)
        };
        let start_blue = if is_dark {
            color_rgba(0x38, 0xbd, 0xf8, 1.0)
        } else {
            color_rgba(0x02, 0x84, 0xc7, 1.0)
        };

        self.draw_surface(size_px, size_px, |dc, w, _h| unsafe {
            let s = w / 24.0;
            match kind {
                ShellGlyph::Start => {
                    let brush = dc.CreateSolidColorBrush(&start_blue, None)?;
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
                }
                ShellGlyph::Search => {
                    let brush = dc.CreateSolidColorBrush(&fg, None)?;
                    let ellipse = D2D1_ELLIPSE {
                        point: Vector2 {
                            X: 11.0 * s,
                            Y: 11.0 * s,
                        },
                        radiusX: 7.5 * s,
                        radiusY: 7.5 * s,
                    };
                    dc.DrawEllipse(&ellipse, &brush, 2.1 * s, Some(&stroke));
                    dc.DrawLine(
                        Vector2 {
                            X: 16.3 * s,
                            Y: 16.3 * s,
                        },
                        Vector2 {
                            X: 20.5 * s,
                            Y: 20.5 * s,
                        },
                        &brush,
                        2.1 * s,
                        Some(&stroke),
                    );
                }
                ShellGlyph::TaskView => {
                    let brush = dc.CreateSolidColorBrush(&fg, None)?;
                    let front = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: 2.5 * s,
                            top: 4.0 * s,
                            right: 16.0 * s,
                            bottom: 14.5 * s,
                        },
                        radiusX: 2.2 * s,
                        radiusY: 2.2 * s,
                    };
                    dc.DrawRoundedRectangle(&front, &brush, 2.0 * s, Some(&stroke));
                    // Draw the tucked back window's visible bottom + right edges cleanly.
                    dc.DrawLine(
                        Vector2 {
                            X: 8.5 * s,
                            Y: 18.5 * s,
                        },
                        Vector2 {
                            X: 19.3 * s,
                            Y: 18.5 * s,
                        },
                        &brush,
                        2.0 * s,
                        Some(&stroke),
                    );
                    dc.DrawLine(
                        Vector2 {
                            X: 21.5 * s,
                            Y: 8.5 * s,
                        },
                        Vector2 {
                            X: 21.5 * s,
                            Y: 16.3 * s,
                        },
                        &brush,
                        2.0 * s,
                        Some(&stroke),
                    );
                    dc.DrawLine(
                        Vector2 {
                            X: 19.3 * s,
                            Y: 18.5 * s,
                        },
                        Vector2 {
                            X: 21.5 * s,
                            Y: 16.3 * s,
                        },
                        &brush,
                        2.0 * s,
                        Some(&stroke),
                    );
                }
                ShellGlyph::TrayChevron => {
                    let brush = dc.CreateSolidColorBrush(&fg, None)?;
                    dc.DrawLine(
                        Vector2 {
                            X: 6.5 * s,
                            Y: 14.5 * s,
                        },
                        Vector2 {
                            X: 12.0 * s,
                            Y: 9.0 * s,
                        },
                        &brush,
                        2.1 * s,
                        Some(&stroke),
                    );
                    dc.DrawLine(
                        Vector2 {
                            X: 12.0 * s,
                            Y: 9.0 * s,
                        },
                        Vector2 {
                            X: 17.5 * s,
                            Y: 14.5 * s,
                        },
                        &brush,
                        2.1 * s,
                        Some(&stroke),
                    );
                }
                ShellGlyph::RecycleBinEmpty | ShellGlyph::RecycleBinFull => {
                    let full = kind == ShellGlyph::RecycleBinFull;
                    let brush = dc.CreateSolidColorBrush(&fg, None)?;
                    // Lid bar + handle
                    dc.DrawLine(
                        Vector2 {
                            X: 4.5 * s,
                            Y: 6.5 * s,
                        },
                        Vector2 {
                            X: 19.5 * s,
                            Y: 6.5 * s,
                        },
                        &brush,
                        1.9 * s,
                        Some(&stroke),
                    );
                    dc.DrawLine(
                        Vector2 {
                            X: 9.0 * s,
                            Y: 4.0 * s,
                        },
                        Vector2 {
                            X: 15.0 * s,
                            Y: 4.0 * s,
                        },
                        &brush,
                        1.9 * s,
                        Some(&stroke),
                    );
                    // Bin body
                    let body = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: 6.2 * s,
                            top: 6.8 * s,
                            right: 17.8 * s,
                            bottom: 20.5 * s,
                        },
                        radiusX: 2.0 * s,
                        radiusY: 2.0 * s,
                    };
                    if full {
                        let fill_c = if is_dark {
                            color_rgba(0x38, 0xbd, 0xf8, 0.35)
                        } else {
                            color_rgba(0x02, 0x84, 0xc7, 0.25)
                        };
                        let fill_brush = dc.CreateSolidColorBrush(&fill_c, None)?;
                        dc.FillRoundedRectangle(&body, &fill_brush);
                    }
                    dc.DrawRoundedRectangle(&body, &brush, 1.8 * s, Some(&stroke));
                    // Vertical ribs
                    dc.DrawLine(
                        Vector2 {
                            X: 10.2 * s,
                            Y: 10.5 * s,
                        },
                        Vector2 {
                            X: 10.2 * s,
                            Y: 17.2 * s,
                        },
                        &brush,
                        1.6 * s,
                        Some(&stroke),
                    );
                    dc.DrawLine(
                        Vector2 {
                            X: 13.8 * s,
                            Y: 10.5 * s,
                        },
                        Vector2 {
                            X: 13.8 * s,
                            Y: 17.2 * s,
                        },
                        &brush,
                        1.6 * s,
                        Some(&stroke),
                    );
                }
            }
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
                .CreateTextLayout(&wide, &self.tooltip_format, 220.0, 28.0)?
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe {
            layout.GetMetrics(&mut metrics)?;
        }
        let w_dips = (metrics.widthIncludingTrailingWhitespace + 22.0)
            .clamp(48.0, 232.0)
            .ceil();
        let h_dips = 26.0_f32;
        let w_px = (w_dips * dpi).ceil() as u32;
        let h_px = (h_dips * dpi).ceil() as u32;

        let (bg_c, bd_c, fg_c) = if is_dark {
            (
                color_rgba(0x18, 0x1b, 0x24, 0.95),
                color_rgba(0xff, 0xff, 0xff, 0.17),
                color_rgba(0xf8, 0xfa, 0xfc, 1.0),
            )
        } else {
            (
                color_rgba(0xf8, 0xfa, 0xfc, 0.96),
                color_rgba(0x0f, 0x17, 0x2a, 0.14),
                color_rgba(0x0f, 0x17, 0x2a, 1.0),
            )
        };

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

unsafe fn draw_chip_icon(
    dc: &ID2D1DeviceContext,
    icon: ChipIcon,
    cx: f32,
    cy: f32,
    size: f32,
    brush: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    stroke: &ID2D1StrokeStyle,
) -> Result<()> {
    let r = size * 0.5;
    let sw = 1.6_f32;
    let l = |x1: f32, y1: f32, x2: f32, y2: f32| unsafe {
        dc.DrawLine(
            Vector2 {
                X: cx + x1 * r,
                Y: cy + y1 * r,
            },
            Vector2 {
                X: cx + x2 * r,
                Y: cy + y2 * r,
            },
            brush,
            sw,
            Some(stroke),
        );
    };
    unsafe {
        match icon {
            ChipIcon::Sliders => {
                l(-0.8, -0.45, 0.8, -0.45);
                l(-0.8, 0.45, 0.8, 0.45);
                dc.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 {
                            X: cx - 0.25 * r,
                            Y: cy - 0.45 * r,
                        },
                        radiusX: 2.2,
                        radiusY: 2.2,
                    },
                    brush,
                );
                dc.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 {
                            X: cx + 0.3 * r,
                            Y: cy + 0.45 * r,
                        },
                        radiusX: 2.2,
                        radiusY: 2.2,
                    },
                    brush,
                );
            }
            ChipIcon::Music => {
                l(-0.2, 0.5, -0.2, -0.6);
                l(-0.2, -0.6, 0.65, -0.8);
                l(0.65, -0.8, 0.65, 0.35);
                dc.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 {
                            X: cx - 0.45 * r,
                            Y: cy + 0.55 * r,
                        },
                        radiusX: 2.2,
                        radiusY: 1.8,
                    },
                    brush,
                );
                dc.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 {
                            X: cx + 0.4 * r,
                            Y: cy + 0.4 * r,
                        },
                        radiusX: 2.2,
                        radiusY: 1.8,
                    },
                    brush,
                );
            }
            ChipIcon::Speaker | ChipIcon::SpeakerMuted => {
                l(-0.75, -0.25, -0.35, -0.25);
                l(-0.75, 0.25, -0.35, 0.25);
                l(-0.75, -0.25, -0.75, 0.25);
                l(-0.35, -0.25, 0.1, -0.65);
                l(-0.35, 0.25, 0.1, 0.65);
                l(0.1, -0.65, 0.1, 0.65);
                if icon == ChipIcon::Speaker {
                    l(0.45, -0.3, 0.6, 0.0);
                    l(0.6, 0.0, 0.45, 0.3);
                } else {
                    l(0.4, -0.35, 0.85, 0.35);
                    l(0.85, -0.35, 0.4, 0.35);
                }
            }
            ChipIcon::SunCloud => {
                dc.DrawEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 { X: cx, Y: cy },
                        radiusX: 0.42 * r,
                        radiusY: 0.42 * r,
                    },
                    brush,
                    sw,
                    None,
                );
                l(0.0, -0.9, 0.0, -0.72);
                l(0.0, 0.72, 0.0, 0.9);
                l(-0.9, 0.0, -0.72, 0.0);
                l(0.72, 0.0, 0.9, 0.0);
            }
            ChipIcon::Battery => {
                let rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: cx - 0.85 * r,
                        top: cy - 0.48 * r,
                        right: cx + 0.62 * r,
                        bottom: cy + 0.48 * r,
                    },
                    radiusX: 2.0,
                    radiusY: 2.0,
                };
                dc.DrawRoundedRectangle(&rr, brush, sw, None);
                l(0.82, -0.22, 0.82, 0.22);
            }
            ChipIcon::Bluetooth => {
                l(0.0, -0.8, 0.0, 0.8);
                l(0.0, -0.8, 0.55, -0.35);
                l(0.55, -0.35, -0.45, 0.35);
                l(-0.45, -0.35, 0.55, 0.35);
                l(0.55, 0.35, 0.0, 0.8);
            }
            ChipIcon::Drive => {
                let rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: cx - 0.8 * r,
                        top: cy - 0.45 * r,
                        right: cx + 0.8 * r,
                        bottom: cy + 0.45 * r,
                    },
                    radiusX: 2.2,
                    radiusY: 2.2,
                };
                dc.DrawRoundedRectangle(&rr, brush, sw, None);
                l(-0.4, 0.05, 0.05, 0.05);
            }
            ChipIcon::Timer => {
                dc.DrawEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 {
                            X: cx,
                            Y: cy + 0.08 * r,
                        },
                        radiusX: 0.65 * r,
                        radiusY: 0.65 * r,
                    },
                    brush,
                    sw,
                    None,
                );
                l(0.0, 0.08, 0.0, -0.28);
                l(0.0, 0.08, 0.32, 0.08);
                l(-0.25, -0.82, 0.25, -0.82);
            }
            ChipIcon::Folder => {
                let rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: cx - 0.8 * r,
                        top: cy - 0.45 * r,
                        right: cx + 0.8 * r,
                        bottom: cy + 0.6 * r,
                    },
                    radiusX: 2.0,
                    radiusY: 2.0,
                };
                dc.DrawRoundedRectangle(&rr, brush, sw, None);
                l(-0.8, -0.45, -0.2, -0.72);
            }
            ChipIcon::Grid => {
                for (ox, oy) in [(-0.45, -0.45), (0.45, -0.45), (-0.45, 0.45), (0.45, 0.45)] {
                    dc.FillEllipse(
                        &D2D1_ELLIPSE {
                            point: Vector2 {
                                X: cx + ox * r,
                                Y: cy + oy * r,
                            },
                            radiusX: 1.8,
                            radiusY: 1.8,
                        },
                        brush,
                    );
                }
            }
        }
    }
    Ok(())
}
