//! Standalone interactive local test laboratory for OpenTaskbar's zero-side-effect
//! Windows 11 Liquid Glass + 3D Specular Light Reflection engine.
//!
//! Architecture:
//! - Uses `Windows.UI.Composition` (`CreateHostBackdropBrush`) + `IGraphicsEffectD2D1Interop`
//!   (`GaussianBlur` -> `Saturation` -> `ArithmeticComposite` tint) clipped strictly to each
//!   rounded capsule via `CompositionRoundedRectangleGeometry` + `CreateGeometricClip`.
//! - Layers a procedural 3D convex bevel Normal Map + `SceneLightingEffect` illuminated by a
//!   3D `DistantLight`, `AmbientLight`, and a real-time cursor-tracking 3D `PointLight`.
//! - Zero screen capture, zero `WDA_EXCLUDEFROMCAPTURE`, zero yellow capture border, and
//!   zero rectangular DWM gray box bleed.
//!
//! Run locally:
//!   cargo run -p opentaskbar-app --example liquid_glass_lab --release

#![cfg(windows)]

use std::cell::RefCell;
use std::mem::size_of;

use windows::Foundation::{IPropertyValue, PropertyValue, Size};
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource, IGraphicsEffectSource_Impl,
};
use windows::System::DispatcherQueueController;
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::Effects::{SceneLightingEffect, SceneLightingEffectReflectanceModel};
use windows::UI::Composition::{
    AmbientLight, CompositionBackdropBrush, CompositionBrush, CompositionColorBrush,
    CompositionDrawingSurface, CompositionEffectSourceParameter, CompositionGraphicsDevice,
    CompositionStretch, Compositor, ContainerVisual, DistantLight, DropShadow, PointLight,
    SpriteVisual,
};
use windows::Win32::Foundation::{HWND as WinHwnd, POINT as WinPoint};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
    D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1ArithmeticComposite, CLSID_D2D1Contrast, CLSID_D2D1GaussianBlur,
    CLSID_D2D1Saturation, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_PROPERTIES1,
    D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
    D2D1_COLOR_SPACE_SRGB, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_MEDIUM,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWriteCreateFactory, IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop, ICompositorInterop,
};
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT,
    IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl,
};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, Result as WinResult, implement};
use windows_numerics::{Vector2, Vector3};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IDC_ARROW,
    LoadCursorW, MSG, PostQuitMessage, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW,
    SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_RBUTTONUP, WNDCLASSEXW, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOPMOST, WS_POPUP,
};

const WM_MOUSELEAVE: u32 = 0x02A3;
const DWMWCP_DONOTROUND: u32 = 1;
const DWMWA_USE_HOSTBACKDROPBRUSH: u32 = 17;
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

// Window & two floating glass capsule geometries (in DIPs), with outer shadow margin.
const WIN_W_DIPS: f32 = 648.0;
const WIN_H_DIPS: f32 = 344.0;
const CARD_RECT_DIPS: (f32, f32, f32, f32, f32) = (38.0, 26.0, 572.0, 182.0, 22.0);
const DOCK_RECT_DIPS: (f32, f32, f32, f32, f32) = (38.0, 228.0, 572.0, 68.0, 26.0);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn rgba_f(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: a.clamp(0.0, 1.0),
    }
}

fn ui_color(a: u8, r: u8, g: u8, b: u8) -> Color {
    Color {
        A: a,
        R: r,
        G: g,
        B: b,
    }
}

// ============================================================================
// Direct2D -> Windows.UI.Composition effect interop wrappers (Zero Win2D DLL!)
// ============================================================================

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct GaussianBlurEffect {
    name: RefCell<HSTRING>,
    blur_sigma: f32,
    source: IGraphicsEffectSource,
}

impl IGraphicsEffectSource_Impl for GaussianBlurEffect_Impl {}

impl IGraphicsEffect_Impl for GaussianBlurEffect_Impl {
    fn Name(&self) -> WinResult<HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &HSTRING) -> WinResult<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for GaussianBlurEffect_Impl {
    fn GetEffectId(&self) -> WinResult<GUID> {
        Ok(CLSID_D2D1GaussianBlur)
    }
    fn GetNamedPropertyMapping(
        &self,
        name: &PCWSTR,
        index: *mut u32,
        mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> WinResult<()> {
        let s = unsafe { name.to_string().unwrap_or_default() };
        if s.eq_ignore_ascii_case("BlurAmount") || s.eq_ignore_ascii_case("StandardDeviation") {
            unsafe {
                *index = 0;
                *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
            }
            Ok(())
        } else {
            Err(windows::Win32::Foundation::E_INVALIDARG.into())
        }
    }
    fn GetPropertyCount(&self) -> WinResult<u32> {
        Ok(3)
    }
    fn GetProperty(&self, index: u32) -> WinResult<IPropertyValue> {
        match index {
            0 => PropertyValue::CreateSingle(self.blur_sigma)?.cast(),
            1 => PropertyValue::CreateUInt32(1)?.cast(), // Balanced optimization
            2 => PropertyValue::CreateUInt32(1)?.cast(), // Hard border mode (prevents edge fade)
            _ => Err(windows::Win32::Foundation::E_INVALIDARG.into()),
        }
    }
    fn GetSource(&self, index: u32) -> WinResult<IGraphicsEffectSource> {
        if index == 0 {
            Ok(self.source.clone())
        } else {
            Err(windows::Win32::Foundation::E_INVALIDARG.into())
        }
    }
    fn GetSourceCount(&self) -> WinResult<u32> {
        Ok(1)
    }
}

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct SaturationEffect {
    name: RefCell<HSTRING>,
    saturation: f32,
    source: IGraphicsEffectSource,
}

impl IGraphicsEffectSource_Impl for SaturationEffect_Impl {}

impl IGraphicsEffect_Impl for SaturationEffect_Impl {
    fn Name(&self) -> WinResult<HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &HSTRING) -> WinResult<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for SaturationEffect_Impl {
    fn GetEffectId(&self) -> WinResult<GUID> {
        Ok(CLSID_D2D1Saturation)
    }
    fn GetNamedPropertyMapping(
        &self,
        name: &PCWSTR,
        index: *mut u32,
        mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> WinResult<()> {
        let s = unsafe { name.to_string().unwrap_or_default() };
        if s.eq_ignore_ascii_case("Saturation") {
            unsafe {
                *index = 0;
                *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
            }
            Ok(())
        } else {
            Err(windows::Win32::Foundation::E_INVALIDARG.into())
        }
    }
    fn GetPropertyCount(&self) -> WinResult<u32> {
        Ok(1)
    }
    fn GetProperty(&self, index: u32) -> WinResult<IPropertyValue> {
        match index {
            0 => PropertyValue::CreateSingle(self.saturation)?.cast(),
            _ => Err(windows::Win32::Foundation::E_INVALIDARG.into()),
        }
    }
    fn GetSource(&self, index: u32) -> WinResult<IGraphicsEffectSource> {
        if index == 0 {
            Ok(self.source.clone())
        } else {
            Err(windows::Win32::Foundation::E_INVALIDARG.into())
        }
    }
    fn GetSourceCount(&self) -> WinResult<u32> {
        Ok(1)
    }
}

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct ArithmeticCompositeEffect {
    name: RefCell<HSTRING>,
    c1: f32,
    c2: f32,
    c3: f32,
    c4: f32,
    source1: IGraphicsEffectSource,
    source2: IGraphicsEffectSource,
}

impl IGraphicsEffectSource_Impl for ArithmeticCompositeEffect_Impl {}

impl IGraphicsEffect_Impl for ArithmeticCompositeEffect_Impl {
    fn Name(&self) -> WinResult<HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &HSTRING) -> WinResult<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for ArithmeticCompositeEffect_Impl {
    fn GetEffectId(&self) -> WinResult<GUID> {
        Ok(CLSID_D2D1ArithmeticComposite)
    }
    fn GetNamedPropertyMapping(
        &self,
        _name: &PCWSTR,
        _index: *mut u32,
        _mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> WinResult<()> {
        Err(windows::Win32::Foundation::E_INVALIDARG.into())
    }
    fn GetPropertyCount(&self) -> WinResult<u32> {
        Ok(2)
    }
    fn GetProperty(&self, index: u32) -> WinResult<IPropertyValue> {
        match index {
            0 => PropertyValue::CreateSingleArray(&[self.c1, self.c2, self.c3, self.c4])?.cast(),
            1 => PropertyValue::CreateBoolean(true)?.cast(), // Clamp output
            _ => Err(windows::Win32::Foundation::E_INVALIDARG.into()),
        }
    }
    fn GetSource(&self, index: u32) -> WinResult<IGraphicsEffectSource> {
        match index {
            0 => Ok(self.source1.clone()),
            1 => Ok(self.source2.clone()),
            _ => Err(windows::Win32::Foundation::E_INVALIDARG.into()),
        }
    }
    fn GetSourceCount(&self) -> WinResult<u32> {
        Ok(2)
    }
}

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct ContrastEffect {
    name: RefCell<HSTRING>,
    contrast: f32,
    source: IGraphicsEffectSource,
}

impl IGraphicsEffectSource_Impl for ContrastEffect_Impl {}

impl IGraphicsEffect_Impl for ContrastEffect_Impl {
    fn Name(&self) -> WinResult<HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &HSTRING) -> WinResult<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for ContrastEffect_Impl {
    fn GetEffectId(&self) -> WinResult<GUID> {
        Ok(CLSID_D2D1Contrast)
    }
    fn GetNamedPropertyMapping(
        &self,
        _name: &PCWSTR,
        _index: *mut u32,
        _mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> WinResult<()> {
        Err(windows::Win32::Foundation::E_INVALIDARG.into())
    }
    fn GetPropertyCount(&self) -> WinResult<u32> {
        Ok(2)
    }
    fn GetProperty(&self, index: u32) -> WinResult<IPropertyValue> {
        match index {
            0 => PropertyValue::CreateSingle(self.contrast)?.cast(),
            1 => PropertyValue::CreateBoolean(true)?.cast(), // Clamp input
            _ => Err(windows::Win32::Foundation::E_INVALIDARG.into()),
        }
    }
    fn GetSource(&self, index: u32) -> WinResult<IGraphicsEffectSource> {
        if index == 0 {
            Ok(self.source.clone())
        } else {
            Err(windows::Win32::Foundation::E_INVALIDARG.into())
        }
    }
    fn GetSourceCount(&self) -> WinResult<u32> {
        Ok(1)
    }
}

// ============================================================================
// Optical Glass Parameters & Presets
// ============================================================================

#[derive(Clone, Copy)]
struct GlassParams {
    preset_name: &'static str,
    is_dark: bool,
    blur_sigma: f32,
    saturation: f32,
    tint_alpha: f32,
    specular: f32,
    shine: f32,
    meniscus_strength: f32,
    meniscus_enabled: bool,
    pointer_light: bool,
}

impl GlassParams {
    fn liquid_crystal(is_dark: bool) -> Self {
        Self {
            preset_name: "1 · Liquid Crystal",
            is_dark,
            blur_sigma: 6.0,
            saturation: 1.72,
            tint_alpha: if is_dark { 0.14 } else { 0.10 },
            specular: 1.15,
            shine: 112.0,
            meniscus_strength: 0.78,
            meniscus_enabled: true,
            pointer_light: true,
        }
    }

    fn pure_water(is_dark: bool) -> Self {
        Self {
            preset_name: "2 · Pure Water",
            is_dark,
            blur_sigma: 2.0,
            saturation: 1.85,
            tint_alpha: if is_dark { 0.05 } else { 0.04 },
            specular: 1.35,
            shine: 132.0,
            meniscus_strength: 0.92,
            meniscus_enabled: true,
            pointer_light: true,
        }
    }

    fn smoked_obsidian(is_dark: bool) -> Self {
        Self {
            preset_name: "3 · Smoked Obsidian",
            is_dark,
            blur_sigma: 10.0,
            saturation: 1.45,
            tint_alpha: if is_dark { 0.34 } else { 0.28 },
            specular: 0.95,
            shine: 96.0,
            meniscus_strength: 0.65,
            meniscus_enabled: true,
            pointer_light: true,
        }
    }

    fn frosted_silk(is_dark: bool) -> Self {
        Self {
            preset_name: "4 · Frosted Silk",
            is_dark,
            blur_sigma: 22.0,
            saturation: 1.35,
            tint_alpha: if is_dark { 0.26 } else { 0.28 },
            specular: 0.70,
            shine: 64.0,
            meniscus_strength: 0.50,
            meniscus_enabled: true,
            pointer_light: true,
        }
    }
}

// ============================================================================
// Procedural 3D Convex Bevel Normal Map Generator (Signed Distance Field)
// ============================================================================

const BAYER_4X4: [[f32; 4]; 4] = [
    [0.0 / 16.0, 8.0 / 16.0, 2.0 / 16.0, 10.0 / 16.0],
    [12.0 / 16.0, 4.0 / 16.0, 14.0 / 16.0, 6.0 / 16.0],
    [3.0 / 16.0, 11.0 / 16.0, 1.0 / 16.0, 9.0 / 16.0],
    [15.0 / 16.0, 7.0 / 16.0, 13.0 / 16.0, 5.0 / 16.0],
];

/// Generates a premultiplied BGRA8 tangent-space normal map for a thin optical glass bevel.
/// Interior pixels remain strictly flat `(0, 0, 1)` to eliminate 8-bit quantization banding,
/// while the narrow outer `bevel_px` rim curves smoothly with 4x4 ordered dithering.
fn build_bevel_normal_map(width: u32, height: u32, radius_px: f32, bevel_px: f32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut out = vec![0u8; w * h * 4];
    let hw = width as f32 * 0.5;
    let hh = height as f32 * 0.5;
    let r = radius_px.min(hw).min(hh).max(2.0);
    let bx = (hw - r).max(0.0);
    let by = (hh - r).max(0.0);
    let bevel = bevel_px.max(3.0);

    for py in 0..h {
        let y = py as f32 + 0.5 - hh;
        let ay = y.abs() - by;
        for px in 0..w {
            let x = px as f32 + 0.5 - hw;
            let ax = x.abs() - bx;
            let qx = ax.max(0.0);
            let qy = ay.max(0.0);
            let outside = (qx * qx + qy * qy).sqrt();
            let inside = ax.max(ay).min(0.0);
            let sdf = outside + inside - r; // <= 0 inside capsule

            let inward = (-sdf).max(0.0);
            let (nx, ny, nz, dither) = if inward < bevel {
                // Outward gradient of the rounded-box SDF.
                let (gx, gy) = if qx > 0.0 || qy > 0.0 {
                    let len = (qx * qx + qy * qy).sqrt().max(1e-4);
                    ((qx / len) * x.signum(), (qy / len) * y.signum())
                } else if ax > ay {
                    (x.signum(), 0.0)
                } else {
                    (0.0, y.signum())
                };
                // Liquid water-drop meniscus curvature: sharp outer lip melting smoothly inward.
                let u = (inward / bevel).clamp(0.0, 1.0);
                let curve = (1.0 - u).powf(2.6) * 0.90;
                let vx = gx * curve;
                let vy = gy * curve;
                let vz = 1.0_f32;
                let len = (vx * vx + vy * vy + vz * vz).sqrt();
                let d = (BAYER_4X4[py & 3][px & 3] - 0.5) * 0.9;
                (vx / len, vy / len, vz / len, d)
            } else {
                // Strictly flat interior -> exact (128, 128, 255), zero vertical banding.
                (0.0, 0.0, 1.0, 0.0)
            };

            let r_u8 = ((nx * 0.5 + 0.5) * 255.0 + dither)
                .clamp(0.0, 255.0)
                .round() as u8;
            let g_u8 = ((ny * 0.5 + 0.5) * 255.0 + dither)
                .clamp(0.0, 255.0)
                .round() as u8;
            let b_u8 = ((nz * 0.5 + 0.5) * 255.0 + dither)
                .clamp(0.0, 255.0)
                .round() as u8;
            let off = (py * w + px) * 4;
            out[off] = b_u8;
            out[off + 1] = g_u8;
            out[off + 2] = r_u8;
            out[off + 3] = 255;
        }
    }
    out
}

// ============================================================================
// Glass Piece (Backdrop + 3D Normal-Mapped Specular + Fresnel Rim Overlay)
// ============================================================================

struct GlassPiece {
    x_px: f32,
    y_px: f32,
    w_px: f32,
    h_px: f32,
    radius_px: f32,
    _shadow_lift_visual: SpriteVisual,
    drop_shadow_lift: DropShadow,
    _shadow_contact_visual: SpriteVisual,
    drop_shadow_contact: DropShadow,
    core_visual: SpriteVisual,
    rim_visual: SpriteVisual,
    _overlay_visual: SpriteVisual,
    normal_surface: CompositionDrawingSurface,
    meniscus_mask_surface: CompositionDrawingSurface,
    overlay_surface: CompositionDrawingSurface,
    _pill_mask_surface: CompositionDrawingSurface,
}

impl GlassPiece {
    fn create(
        compositor: &Compositor,
        gfx_device: &CompositionGraphicsDevice,
        parent: &ContainerVisual,
        rect_dips: (f32, f32, f32, f32, f32),
        dpi: f32,
    ) -> WinResult<Self> {
        let x_px = (rect_dips.0 * dpi).round();
        let y_px = (rect_dips.1 * dpi).round();
        let w_px = (rect_dips.2 * dpi).round();
        let h_px = (rect_dips.3 * dpi).round();
        let radius_px = (rect_dips.4 * dpi).round();

        let geom = compositor.CreateRoundedRectangleGeometry()?;
        geom.SetSize(Vector2 { X: w_px, Y: h_px })?;
        geom.SetCornerRadius(Vector2 {
            X: radius_px,
            Y: radius_px,
        })?;
        let clip = compositor.CreateGeometricClip()?;
        clip.SetGeometry(&geom)?;

        // Dual-depth floating optical shadow (ethereal wide lift + crisp close contact).
        let pill_mask_surface = gfx_device.CreateDrawingSurface(
            Size {
                Width: w_px,
                Height: h_px,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        upload_pill_mask(&pill_mask_surface, w_px, h_px, radius_px)?;
        let pill_mask_brush = compositor.CreateSurfaceBrushWithSurface(&pill_mask_surface)?;
        pill_mask_brush.SetStretch(CompositionStretch::Fill)?;

        let drop_shadow_lift = compositor.CreateDropShadow()?;
        drop_shadow_lift.SetMask(&pill_mask_brush)?;
        drop_shadow_lift.SetBlurRadius(28.0 * dpi)?;
        drop_shadow_lift.SetOffset(Vector3 {
            X: 0.0,
            Y: 12.0 * dpi,
            Z: 0.0,
        })?;
        drop_shadow_lift.SetColor(ui_color(84, 4, 10, 20))?;

        let shadow_lift_visual = compositor.CreateSpriteVisual()?;
        shadow_lift_visual.SetSize(Vector2 { X: w_px, Y: h_px })?;
        shadow_lift_visual.SetOffset(Vector3 {
            X: x_px,
            Y: y_px,
            Z: 0.0,
        })?;
        shadow_lift_visual.SetShadow(&drop_shadow_lift)?;
        parent.Children()?.InsertAtTop(&shadow_lift_visual)?;

        let drop_shadow_contact = compositor.CreateDropShadow()?;
        drop_shadow_contact.SetMask(&pill_mask_brush)?;
        drop_shadow_contact.SetBlurRadius(7.0 * dpi)?;
        drop_shadow_contact.SetOffset(Vector3 {
            X: 0.0,
            Y: 2.0 * dpi,
            Z: 0.0,
        })?;
        drop_shadow_contact.SetColor(ui_color(56, 2, 8, 16))?;

        let shadow_contact_visual = compositor.CreateSpriteVisual()?;
        shadow_contact_visual.SetSize(Vector2 { X: w_px, Y: h_px })?;
        shadow_contact_visual.SetOffset(Vector3 {
            X: x_px,
            Y: y_px,
            Z: 0.0,
        })?;
        shadow_contact_visual.SetShadow(&drop_shadow_contact)?;
        parent.Children()?.InsertAtTop(&shadow_contact_visual)?;

        // Zone 1: Mirror-polished Crystal Liquid Core visual.
        let core_visual = compositor.CreateSpriteVisual()?;
        core_visual.SetSize(Vector2 { X: w_px, Y: h_px })?;
        core_visual.SetOffset(Vector3 {
            X: x_px,
            Y: y_px,
            Z: 0.0,
        })?;
        core_visual.SetClip(&clip)?;
        parent.Children()?.InsertAtTop(&core_visual)?;

        // Zone 2: Live Optical Meniscus Refraction + Chromatic RGB Dispersion Ring visual.
        let rim_visual = compositor.CreateSpriteVisual()?;
        rim_visual.SetSize(Vector2 { X: w_px, Y: h_px })?;
        rim_visual.SetOffset(Vector3 {
            X: x_px,
            Y: y_px,
            Z: 0.0,
        })?;
        rim_visual.SetClip(&clip)?;
        parent.Children()?.InsertAtTop(&rim_visual)?;

        let normal_surface = gfx_device.CreateDrawingSurface(
            Size {
                Width: w_px,
                Height: h_px,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        upload_normal_map(
            &normal_surface,
            w_px as u32,
            h_px as u32,
            radius_px,
            13.5 * dpi,
        )?;

        let meniscus_mask_surface = gfx_device.CreateDrawingSurface(
            Size {
                Width: w_px,
                Height: h_px,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        upload_meniscus_mask(
            &meniscus_mask_surface,
            w_px as u32,
            h_px as u32,
            radius_px,
            15.0 * dpi,
        )?;

        let overlay_surface = gfx_device.CreateDrawingSurface(
            Size {
                Width: w_px,
                Height: h_px,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        let overlay_brush = compositor.CreateSurfaceBrushWithSurface(&overlay_surface)?;
        overlay_brush.SetStretch(CompositionStretch::Fill)?;

        let overlay_visual = compositor.CreateSpriteVisual()?;
        overlay_visual.SetSize(Vector2 { X: w_px, Y: h_px })?;
        overlay_visual.SetOffset(Vector3 {
            X: x_px,
            Y: y_px,
            Z: 0.0,
        })?;
        overlay_visual.SetClip(&clip)?;
        overlay_visual.SetBrush(&overlay_brush)?;
        parent.Children()?.InsertAtTop(&overlay_visual)?;

        Ok(Self {
            x_px,
            y_px,
            w_px,
            h_px,
            radius_px,
            _shadow_lift_visual: shadow_lift_visual,
            drop_shadow_lift,
            _shadow_contact_visual: shadow_contact_visual,
            drop_shadow_contact,
            core_visual,
            rim_visual,
            _overlay_visual: overlay_visual,
            normal_surface,
            meniscus_mask_surface,
            overlay_surface,
            _pill_mask_surface: pill_mask_surface,
        })
    }

    fn apply_glass_brush(
        &self,
        compositor: &Compositor,
        backdrop: &CompositionBackdropBrush,
        params: GlassParams,
    ) -> WinResult<()> {
        let tint_rgb = if params.is_dark {
            ui_color(255, 10, 14, 22)
        } else {
            ui_color(255, 250, 252, 255)
        };
        let tint_brush: CompositionColorBrush = compositor.CreateColorBrushWithColor(tint_rgb)?;

        let normal_brush = compositor.CreateSurfaceBrushWithSurface(&self.normal_surface)?;
        normal_brush.SetStretch(CompositionStretch::Fill)?;

        // ---- ZONE 1: Mirror-Polished Crystal Liquid Core ----
        let core_backdrop_param: IGraphicsEffectSource =
            CompositionEffectSourceParameter::Create(&HSTRING::from("Backdrop"))?.cast()?;
        let core_tint_param: IGraphicsEffectSource =
            CompositionEffectSourceParameter::Create(&HSTRING::from("Tint"))?.cast()?;
        let core_normal_param: IGraphicsEffectSource =
            CompositionEffectSourceParameter::Create(&HSTRING::from("NormalMap"))?.cast()?;

        let core_blur: IGraphicsEffectSource = IGraphicsEffect::from(GaussianBlurEffect {
            name: RefCell::new(HSTRING::from("CoreBlur")),
            blur_sigma: params.blur_sigma,
            source: core_backdrop_param,
        })
        .cast()?;
        let core_sat: IGraphicsEffectSource = IGraphicsEffect::from(SaturationEffect {
            name: RefCell::new(HSTRING::from("CoreSat")),
            saturation: params.saturation,
            source: core_blur,
        })
        .cast()?;
        let core_contrast: IGraphicsEffectSource = IGraphicsEffect::from(ContrastEffect {
            name: RefCell::new(HSTRING::from("CoreContrast")),
            contrast: if params.is_dark { 0.12 } else { 0.08 },
            source: core_sat,
        })
        .cast()?;

        // Optical Beer-Lambert luminance-preserving glass transmission (never crushes whites into flat gray).
        let a = params.tint_alpha.clamp(0.0, 0.85);
        let core_tinted: IGraphicsEffectSource = if params.is_dark {
            IGraphicsEffect::from(ArithmeticCompositeEffect {
                name: RefCell::new(HSTRING::from("CoreTinted")),
                c1: a * 0.45,
                c2: 1.0 - a * 0.82,
                c3: a * 0.38,
                c4: 0.0,
                source1: core_contrast,
                source2: core_tint_param,
            })
            .cast()?
        } else {
            IGraphicsEffect::from(ArithmeticCompositeEffect {
                name: RefCell::new(HSTRING::from("CoreTinted")),
                c1: 0.0,
                c2: 1.0 - a,
                c3: a,
                c4: 0.0,
                source1: core_contrast,
                source2: core_tint_param,
            })
            .cast()?
        };

        let core_light = SceneLightingEffect::new()?;
        core_light.SetName(&HSTRING::from("CoreLight"))?;
        core_light.SetNormalMapSource(&core_normal_param)?;
        core_light.SetAmbientAmount(0.0)?;
        core_light.SetDiffuseAmount(0.012)?;
        core_light.SetSpecularAmount(params.specular * 0.45)?;
        core_light.SetSpecularShine(params.shine * 0.85)?;
        core_light.SetReflectanceModel(SceneLightingEffectReflectanceModel::BlinnPhong)?;
        let core_light_src: IGraphicsEffectSource = core_light.cast()?;

        let core_final: IGraphicsEffect = IGraphicsEffect::from(ArithmeticCompositeEffect {
            name: RefCell::new(HSTRING::from("CoreFinal")),
            c1: -0.58,
            c2: 1.0,
            c3: 0.65,
            c4: 0.0,
            source1: core_tinted,
            source2: core_light_src,
        });

        let core_factory = compositor.CreateEffectFactory(&core_final)?;
        let core_effect_brush = core_factory.CreateBrush()?;
        core_effect_brush.SetSourceParameter(&HSTRING::from("Backdrop"), backdrop)?;
        core_effect_brush.SetSourceParameter(&HSTRING::from("Tint"), &tint_brush)?;
        core_effect_brush.SetSourceParameter(&HSTRING::from("NormalMap"), &normal_brush)?;
        let core_brush: CompositionBrush = core_effect_brush.cast()?;
        self.core_visual.SetBrush(&core_brush)?;

        // ---- ZONE 2: Live Optical Meniscus Caustic Ring (100% DWM-native, zero capture!) ----
        // Sharpens, concentrates, and boosts optical contrast + vibrancy along the outer 15 DIP
        // curved lens lip, topped with high-gloss dual-color 3D Blinn-Phong specular arcs.
        if params.meniscus_enabled && params.meniscus_strength > 0.05 {
            let rim_backdrop_param: IGraphicsEffectSource =
                CompositionEffectSourceParameter::Create(&HSTRING::from("Backdrop"))?.cast()?;
            let rim_normal_param: IGraphicsEffectSource =
                CompositionEffectSourceParameter::Create(&HSTRING::from("NormalMap"))?.cast()?;

            let rim_blur: IGraphicsEffectSource = IGraphicsEffect::from(GaussianBlurEffect {
                name: RefCell::new(HSTRING::from("RimBlur")),
                blur_sigma: (params.blur_sigma * 0.20).clamp(0.8, 3.0),
                source: rim_backdrop_param,
            })
            .cast()?;
            let rim_sat: IGraphicsEffectSource = IGraphicsEffect::from(SaturationEffect {
                name: RefCell::new(HSTRING::from("RimSat")),
                saturation: (params.saturation + 0.32 * params.meniscus_strength).min(2.5),
                source: rim_blur,
            })
            .cast()?;
            let rim_concentrated: IGraphicsEffectSource = IGraphicsEffect::from(ContrastEffect {
                name: RefCell::new(HSTRING::from("RimContrast")),
                contrast: (0.28 * params.meniscus_strength).clamp(0.0, 0.45),
                source: rim_sat,
            })
            .cast()?;

            let rim_light = SceneLightingEffect::new()?;
            rim_light.SetName(&HSTRING::from("RimLight"))?;
            rim_light.SetNormalMapSource(&rim_normal_param)?;
            rim_light.SetAmbientAmount(0.0)?;
            rim_light.SetDiffuseAmount(0.02)?;
            rim_light.SetSpecularAmount(params.specular * 1.25)?;
            rim_light.SetSpecularShine(params.shine)?;
            rim_light.SetReflectanceModel(SceneLightingEffectReflectanceModel::BlinnPhong)?;
            let rim_light_src: IGraphicsEffectSource = rim_light.cast()?;

            let lift = 0.08 * params.meniscus_strength;
            let rim_final: IGraphicsEffect = IGraphicsEffect::from(ArithmeticCompositeEffect {
                name: RefCell::new(HSTRING::from("RimFinal")),
                c1: -0.54,
                c2: 1.0 + lift,
                c3: 0.94,
                c4: if params.is_dark { 0.012 } else { 0.028 } * params.meniscus_strength,
                source1: rim_concentrated,
                source2: rim_light_src,
            });

            let rim_factory = compositor.CreateEffectFactory(&rim_final)?;
            let rim_effect_brush = rim_factory.CreateBrush()?;
            rim_effect_brush.SetSourceParameter(&HSTRING::from("Backdrop"), backdrop)?;
            rim_effect_brush.SetSourceParameter(&HSTRING::from("NormalMap"), &normal_brush)?;

            let meniscus_mask_brush =
                compositor.CreateSurfaceBrushWithSurface(&self.meniscus_mask_surface)?;
            meniscus_mask_brush.SetStretch(CompositionStretch::Fill)?;

            let masked_rim = compositor.CreateMaskBrush()?;
            masked_rim.SetSource(&rim_effect_brush)?;
            masked_rim.SetMask(&meniscus_mask_brush)?;
            let rim_brush: CompositionBrush = masked_rim.cast()?;
            self.rim_visual.SetBrush(&rim_brush)?;
            self.rim_visual
                .SetOpacity(params.meniscus_strength.clamp(0.15, 1.0))?;
            self.rim_visual.SetIsVisible(true)?;
        } else {
            self.rim_visual.SetIsVisible(false)?;
        }

        self.drop_shadow_lift.SetColor(if params.is_dark {
            ui_color(84, 4, 10, 20)
        } else {
            ui_color(48, 12, 24, 38)
        })?;
        self.drop_shadow_contact.SetColor(if params.is_dark {
            ui_color(56, 2, 8, 16)
        } else {
            ui_color(34, 10, 20, 32)
        })?;
        Ok(())
    }
}

/// Generates a silky smooth radial-SDF alpha mask (`alpha = 1` at outer rim, fading to `0`
/// over `rim_width_px` inward) so the live refracted meniscus melts seamlessly into the core.
fn upload_meniscus_mask(
    surface: &CompositionDrawingSurface,
    width: u32,
    height: u32,
    radius_px: f32,
    rim_width_px: f32,
) -> WinResult<()> {
    let w = width as usize;
    let h = height as usize;
    let mut pixels = vec![0u8; w * h * 4];
    let hw = width as f32 * 0.5;
    let hh = height as f32 * 0.5;
    let r = radius_px.min(hw).min(hh).max(2.0);
    let bx = (hw - r).max(0.0);
    let by = (hh - r).max(0.0);
    let rim_w = rim_width_px.max(4.0);

    for py in 0..h {
        let y = py as f32 + 0.5 - hh;
        let ay = y.abs() - by;
        for px in 0..w {
            let x = px as f32 + 0.5 - hw;
            let ax = x.abs() - bx;
            let qx = ax.max(0.0);
            let qy = ay.max(0.0);
            let sdf = (qx * qx + qy * qy).sqrt() + ax.max(ay).min(0.0) - r;
            let inward = (-sdf).max(0.0);
            let alpha = if sdf > 0.5 {
                0.0
            } else if inward < rim_w {
                let u = (inward / rim_w).clamp(0.0, 1.0);
                // Smooth cubic ease-out so the refracted lens blends invisibly into the core.
                (1.0 - u).powf(1.65)
            } else {
                0.0
            };
            let a_u8 = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
            let off = (py * w + px) * 4;
            pixels[off] = a_u8;
            pixels[off + 1] = a_u8;
            pixels[off + 2] = a_u8;
            pixels[off + 3] = a_u8;
        }
    }

    let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
    let mut offset = WinPoint::default();
    let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
    unsafe {
        dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        dc.Clear(Some(&rgba_f(0, 0, 0, 0.0)));
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        if let Ok(bmp) = dc.CreateBitmap(
            D2D_SIZE_U { width, height },
            Some(pixels.as_ptr().cast()),
            width * 4,
            &props,
        ) {
            let dst = D2D_RECT_F {
                left: offset.x as f32,
                top: offset.y as f32,
                right: offset.x as f32 + width as f32,
                bottom: offset.y as f32 + height as f32,
            };
            dc.DrawBitmap(
                &bmp,
                Some(&dst),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
        }
        interop.EndDraw()?;
    }
    Ok(())
}

fn upload_pill_mask(
    surface: &CompositionDrawingSurface,
    w_px: f32,
    h_px: f32,
    radius_px: f32,
) -> WinResult<()> {
    let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
    let mut offset = WinPoint::default();
    let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
    let ox = offset.x as f32;
    let oy = offset.y as f32;
    unsafe {
        dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        dc.Clear(Some(&rgba_f(0, 0, 0, 0.0)));
        let white = dc.CreateSolidColorBrush(&rgba_f(255, 255, 255, 1.0), None)?;
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: ox,
                top: oy,
                right: ox + w_px,
                bottom: oy + h_px,
            },
            radiusX: radius_px,
            radiusY: radius_px,
        };
        dc.FillRoundedRectangle(&rr, &white);
        interop.EndDraw()?;
    }
    Ok(())
}

fn upload_normal_map(
    surface: &CompositionDrawingSurface,
    width: u32,
    height: u32,
    radius_px: f32,
    bevel_px: f32,
) -> WinResult<()> {
    let pixels = build_bevel_normal_map(width, height, radius_px, bevel_px);
    let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
    let mut offset = WinPoint::default();
    let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
    unsafe {
        dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        if let Ok(bmp) = dc.CreateBitmap(
            D2D_SIZE_U { width, height },
            Some(pixels.as_ptr().cast()),
            width * 4,
            &props,
        ) {
            let dst = D2D_RECT_F {
                left: offset.x as f32,
                top: offset.y as f32,
                right: offset.x as f32 + width as f32,
                bottom: offset.y as f32 + height as f32,
            };
            dc.DrawBitmap(
                &bmp,
                Some(&dst),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
        }
        interop.EndDraw()?;
    }
    Ok(())
}

// ============================================================================
// Main Lab State
// ============================================================================

struct LabState {
    _hwnd: HWND,
    dpi: f32,
    _dispatcher: DispatcherQueueController,
    compositor: Compositor,
    _target: DesktopWindowTarget,
    _d2d_device: ID2D1Device,
    dwrite: IDWriteFactory,
    _root: ContainerVisual,
    backdrop: CompositionBackdropBrush,
    _ambient_light: AmbientLight,
    distant_light: DistantLight,
    bounce_light: DistantLight,
    point_light: PointLight,
    card: GlassPiece,
    dock: GlassPiece,
    params: GlassParams,
    mouse_px: (f32, f32),
    hovered_icon: Option<usize>,
    dragging: Option<(i32, i32, i32, i32)>,
    tracking: bool,
}

impl LabState {
    fn new(hwnd: HWND, dpi: f32) -> WinResult<Self> {
        let options = DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        };
        let dispatcher = unsafe { CreateDispatcherQueueController(options)? };
        let compositor = Compositor::new()?;
        let desktop_interop: ICompositorDesktopInterop = compositor.cast()?;
        let target =
            unsafe { desktop_interop.CreateDesktopWindowTarget(WinHwnd(hwnd as _), true)? };

        let levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
        ];
        let mut d3d_dev = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut d3d_dev),
                None,
                None,
            )?;
        }
        let d3d_dev = d3d_dev.ok_or(windows::Win32::Foundation::E_FAIL)?;
        let dxgi: IDXGIDevice = d3d_dev.cast()?;
        let d2d_factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let d2d_device = unsafe { d2d_factory.CreateDevice(&dxgi)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };

        let comp_interop: ICompositorInterop = compositor.cast()?;
        let gfx_device = unsafe { comp_interop.CreateGraphicsDevice(&d2d_device)? };

        let root = compositor.CreateContainerVisual()?;
        root.SetSize(Vector2 {
            X: (WIN_W_DIPS * dpi).round(),
            Y: (WIN_H_DIPS * dpi).round(),
        })?;
        target.SetRoot(&root)?;

        let backdrop = compositor.CreateHostBackdropBrush()?;
        let card = GlassPiece::create(&compositor, &gfx_device, &root, CARD_RECT_DIPS, dpi)?;
        let dock = GlassPiece::create(&compositor, &gfx_device, &root, DOCK_RECT_DIPS, dpi)?;

        // 3-Light 3D Optical Rig targeting both Crystal Core & Refracted Meniscus Rim.
        let ambient_light = compositor.CreateAmbientLight()?;
        ambient_light.SetColor(ui_color(255, 255, 255, 255))?;
        for v in [
            &card.core_visual,
            &card.rim_visual,
            &dock.core_visual,
            &dock.rim_visual,
        ] {
            ambient_light.Targets()?.Add(v)?;
        }

        // Key light: cool overhead top-left sky reflection.
        let distant_light = compositor.CreateDistantLight()?;
        distant_light.SetCoordinateSpace(&root)?;
        distant_light.SetDirection(Vector3 {
            X: -0.24,
            Y: -0.64,
            Z: -0.73,
        })?;
        distant_light.SetColor(ui_color(255, 236, 246, 255))?;
        for v in [
            &card.core_visual,
            &card.rim_visual,
            &dock.core_visual,
            &dock.rim_visual,
        ] {
            distant_light.Targets()?.Add(v)?;
        }

        // Opposing horizon rebound light: soft bottom-right caustic specular arc.
        let bounce_light = compositor.CreateDistantLight()?;
        bounce_light.SetCoordinateSpace(&root)?;
        bounce_light.SetDirection(Vector3 {
            X: 0.26,
            Y: 0.58,
            Z: -0.77,
        })?;
        bounce_light.SetColor(ui_color(255, 138, 168, 206))?;
        for v in [
            &card.core_visual,
            &card.rim_visual,
            &dock.core_visual,
            &dock.rim_visual,
        ] {
            bounce_light.Targets()?.Add(v)?;
        }

        // Fluid cursor-tracking 3D PointLight -> glides like liquid mercury along the glass rim.
        let point_light = compositor.CreatePointLight()?;
        point_light.SetCoordinateSpace(&root)?;
        point_light.SetColor(ui_color(255, 232, 246, 255))?;
        point_light.SetConstantAttenuation(1.0)?;
        point_light.SetLinearAttenuation(0.0026)?;
        point_light.SetQuadraticAttenuation(0.000_010)?;
        point_light.SetOffset(Vector3 {
            X: WIN_W_DIPS * dpi * 0.35,
            Y: WIN_H_DIPS * dpi * 0.25,
            Z: 32.0 * dpi,
        })?;
        for v in [
            &card.core_visual,
            &card.rim_visual,
            &dock.core_visual,
            &dock.rim_visual,
        ] {
            point_light.Targets()?.Add(v)?;
        }

        let mut state = Self {
            _hwnd: hwnd,
            dpi,
            _dispatcher: dispatcher,
            compositor,
            _target: target,
            _d2d_device: d2d_device,
            dwrite,
            _root: root,
            backdrop,
            _ambient_light: ambient_light,
            distant_light,
            bounce_light,
            point_light,
            card,
            dock,
            params: GlassParams::liquid_crystal(true),
            mouse_px: (WIN_W_DIPS * dpi * 0.5, WIN_H_DIPS * dpi * 0.45),
            hovered_icon: None,
            dragging: None,
            tracking: false,
        };
        state.rebuild_materials()?;
        state.redraw_overlays()?;
        Ok(state)
    }

    fn rebuild_materials(&mut self) -> WinResult<()> {
        let p = self.params;
        self.card
            .apply_glass_brush(&self.compositor, &self.backdrop, p)?;
        self.dock
            .apply_glass_brush(&self.compositor, &self.backdrop, p)?;

        // Opposing chromatic lights -> cool sapphire top-left + subtle prism-violet bottom-right.
        if p.is_dark {
            self.distant_light.SetColor(ui_color(255, 214, 244, 255))?;
            self.bounce_light.SetColor(ui_color(255, 218, 192, 255))?;
            self.point_light.SetColor(if p.pointer_light {
                ui_color(255, 232, 248, 255)
            } else {
                ui_color(0, 0, 0, 0)
            })?;
        } else {
            self.distant_light.SetColor(ui_color(255, 236, 250, 255))?;
            self.bounce_light.SetColor(ui_color(255, 235, 214, 255))?;
            self.point_light.SetColor(if p.pointer_light {
                ui_color(255, 255, 253, 248)
            } else {
                ui_color(0, 0, 0, 0)
            })?;
        }
        Ok(())
    }

    fn update_pointer_light(&mut self, x: f32, y: f32) {
        self.mouse_px = (x, y);
        let _ = self.point_light.SetOffset(Vector3 {
            X: x,
            Y: y,
            Z: 34.0 * self.dpi,
        });
        let next_icon = self.hit_test_dock_icon(x, y);
        if next_icon != self.hovered_icon {
            self.hovered_icon = next_icon;
            let _ = self.redraw_dock_overlay();
        }
    }

    fn hit_test_dock_icon(&self, x: f32, y: f32) -> Option<usize> {
        let rel_x = x - self.dock.x_px;
        let rel_y = y - self.dock.y_px;
        if rel_x < 0.0 || rel_y < 0.0 || rel_x > self.dock.w_px || rel_y > self.dock.h_px {
            return None;
        }
        let icon_sz = 44.0 * self.dpi;
        let gap = 12.0 * self.dpi;
        let count = 8usize;
        let total = count as f32 * icon_sz + (count - 1) as f32 * gap;
        let start_x = (self.dock.w_px - total) * 0.5;
        let idx = ((rel_x - start_x) / (icon_sz + gap)).floor() as isize;
        if (0..count as isize).contains(&idx) {
            let cell_l = start_x + idx as f32 * (icon_sz + gap);
            if rel_x >= cell_l && rel_x <= cell_l + icon_sz {
                return Some(idx as usize);
            }
        }
        None
    }

    fn redraw_overlays(&self) -> WinResult<()> {
        self.redraw_card_overlay()?;
        self.redraw_dock_overlay()?;
        Ok(())
    }
}

impl LabState {
    fn draw_fresnel_rim(
        &self,
        dc: &ID2D1DeviceContext,
        ox: f32,
        oy: f32,
        w: f32,
        h: f32,
        r: f32,
    ) -> WinResult<()> {
        let is_dark = self.params.is_dark;
        let spec_scale = (self.params.specular / 1.15).clamp(0.30, 1.35);

        // 1. Mirror-smooth top optical glass sheen (zero sand grain, zero muddy gray wash).
        let gloss_stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: rgba_f(
                    255,
                    255,
                    255,
                    if is_dark { 0.10 } else { 0.24 } * spec_scale,
                ),
            },
            D2D1_GRADIENT_STOP {
                position: 0.24,
                color: rgba_f(255, 255, 255, 0.0),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: rgba_f(0, 0, 0, 0.0),
            },
        ];
        unsafe {
            let stop_coll = dc.CreateGradientStopCollection(
                &gloss_stops,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                D2D1_EXTEND_MODE_CLAMP,
                D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
            )?;
            let gloss_brush = dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 { X: ox, Y: oy },
                    endPoint: Vector2 { X: ox, Y: oy + h },
                },
                None,
                &stop_coll,
            )?;
            let body_rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: ox,
                    top: oy,
                    right: ox + w,
                    bottom: oy + h,
                },
                radiusX: r,
                radiusY: r,
            };
            dc.FillRoundedRectangle(&body_rr, &gloss_brush);

            // 2. Single razor-thin diagonal specular meniscus lip (brightest on opposing curved
            //    corners, whispering near-zero along flat spans — no double-ring bezel).
            let rim_stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: rgba_f(
                        255,
                        255,
                        255,
                        if is_dark { 0.86 } else { 0.96 } * spec_scale,
                    ),
                },
                D2D1_GRADIENT_STOP {
                    position: 0.22,
                    color: rgba_f(
                        220,
                        240,
                        255,
                        if is_dark { 0.18 } else { 0.34 } * spec_scale,
                    ),
                },
                D2D1_GRADIENT_STOP {
                    position: 0.50,
                    color: rgba_f(
                        255,
                        255,
                        255,
                        if is_dark { 0.05 } else { 0.12 } * spec_scale,
                    ),
                },
                D2D1_GRADIENT_STOP {
                    position: 0.78,
                    color: rgba_f(
                        216,
                        234,
                        255,
                        if is_dark { 0.18 } else { 0.34 } * spec_scale,
                    ),
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: rgba_f(
                        240,
                        248,
                        255,
                        if is_dark { 0.72 } else { 0.90 } * spec_scale,
                    ),
                },
            ];
            let rim_coll = dc.CreateGradientStopCollection(
                &rim_stops,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                D2D1_EXTEND_MODE_CLAMP,
                D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
            )?;
            let rim_brush = dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 { X: ox, Y: oy },
                    endPoint: Vector2 {
                        X: ox + w,
                        Y: oy + h,
                    },
                },
                None,
                &rim_coll,
            )?;
            let stroke_w = (1.05 * self.dpi).max(1.0);
            let inset = stroke_w * 0.5;
            let rim_rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: ox + inset,
                    top: oy + inset,
                    right: ox + w - inset,
                    bottom: oy + h - inset,
                },
                radiusX: (r - inset).max(1.0),
                radiusY: (r - inset).max(1.0),
            };
            dc.DrawRoundedRectangle(&rim_rr, &rim_brush, stroke_w, None);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct TextStyle {
    size_dips: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: u8,
    color: D2D1_COLOR_F,
    fluent: bool,
}

impl LabState {
    fn draw_text(
        &self,
        dc: &ID2D1DeviceContext,
        text: &str,
        rect: D2D_RECT_F,
        style: TextStyle,
    ) -> WinResult<()> {
        let family = if style.fluent {
            "Segoe Fluent Icons"
        } else {
            "Segoe UI Variable Text"
        };
        unsafe {
            let fmt = self.dwrite.CreateTextFormat(
                &HSTRING::from(family),
                None,
                style.weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                style.size_dips * self.dpi,
                &HSTRING::from("tr-TR"),
            )?;
            let _ = fmt.SetTextAlignment(match style.align {
                1 => DWRITE_TEXT_ALIGNMENT_CENTER,
                2 => DWRITE_TEXT_ALIGNMENT_TRAILING,
                _ => DWRITE_TEXT_ALIGNMENT_LEADING,
            });
            let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let wtxt: Vec<u16> = text.encode_utf16().collect();

            // Subtle optical legibility shadow (keeps glass typography crisp on bright skies).
            let shadow_col = if self.params.is_dark {
                rgba_f(0, 4, 12, 0.42)
            } else {
                rgba_f(255, 255, 255, 0.55)
            };
            let shadow_brush = dc.CreateSolidColorBrush(&shadow_col, None)?;
            let sh_off = (0.85 * self.dpi).max(1.0);
            let shadow_rect = D2D_RECT_F {
                left: rect.left,
                top: rect.top + sh_off,
                right: rect.right,
                bottom: rect.bottom + sh_off,
            };
            dc.DrawText(
                &wtxt,
                &fmt,
                &shadow_rect,
                &shadow_brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );

            let brush = dc.CreateSolidColorBrush(&style.color, None)?;
            dc.DrawText(
                &wtxt,
                &fmt,
                &rect,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        Ok(())
    }
}

impl LabState {
    fn redraw_card_overlay(&self) -> WinResult<()> {
        let interop: ICompositionDrawingSurfaceInterop = self.card.overlay_surface.cast()?;
        let mut offset = WinPoint::default();
        let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
        let ox = offset.x as f32;
        let oy = offset.y as f32;
        let w = self.card.w_px;
        let h = self.card.h_px;
        let s = |v: f32| v * self.dpi;
        let p = self.params;
        let fg = if p.is_dark {
            rgba_f(250, 252, 255, 0.98)
        } else {
            rgba_f(14, 20, 30, 0.96)
        };
        let muted = if p.is_dark {
            rgba_f(226, 236, 248, 0.88)
        } else {
            rgba_f(32, 42, 58, 0.86)
        };
        let chip_fill = if p.is_dark {
            rgba_f(10, 14, 24, 0.24)
        } else {
            rgba_f(255, 255, 255, 0.50)
        };
        let chip_stroke = if p.is_dark {
            rgba_f(255, 255, 255, 0.26)
        } else {
            rgba_f(255, 255, 255, 0.82)
        };

        unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            dc.Clear(Some(&rgba_f(0, 0, 0, 0.0)));
        }

        self.draw_fresnel_rim(&dc, ox, oy, w, h, self.card.radius_px)?;

        // Header row with clean inset pill badge for active preset.
        self.draw_text(
            &dc,
            "OpenTaskbar Liquid Glass Lab",
            D2D_RECT_F {
                left: ox + s(20.0),
                top: oy + s(16.0),
                right: ox + w - s(170.0),
                bottom: oy + s(40.0),
            },
            TextStyle {
                size_dips: 16.5,
                weight: DWRITE_FONT_WEIGHT_BOLD,
                align: 0,
                color: fg,
                fluent: false,
            },
        )?;
        let badge_rect = D2D_RECT_F {
            left: ox + w - s(158.0),
            top: oy + s(15.0),
            right: ox + w - s(20.0),
            bottom: oy + s(41.0),
        };
        unsafe {
            let badge_fill = dc.CreateSolidColorBrush(
                &if p.is_dark {
                    rgba_f(14, 116, 198, 0.34)
                } else {
                    rgba_f(14, 116, 214, 0.16)
                },
                None,
            )?;
            let badge_stroke = dc.CreateSolidColorBrush(
                &if p.is_dark {
                    rgba_f(148, 224, 255, 0.52)
                } else {
                    rgba_f(14, 116, 214, 0.38)
                },
                None,
            )?;
            let badge_rr = D2D1_ROUNDED_RECT {
                rect: badge_rect,
                radiusX: s(13.0),
                radiusY: s(13.0),
            };
            dc.FillRoundedRectangle(&badge_rr, &badge_fill);
            dc.DrawRoundedRectangle(&badge_rr, &badge_stroke, 1.0, None);
        }
        self.draw_text(
            &dc,
            p.preset_name,
            badge_rect,
            TextStyle {
                size_dips: 11.5,
                weight: DWRITE_FONT_WEIGHT_SEMI_BOLD,
                align: 1,
                color: if p.is_dark {
                    rgba_f(224, 246, 255, 0.99)
                } else {
                    rgba_f(10, 74, 156, 0.98)
                },
                fluent: false,
            },
        )?;

        // 4 live optical telemetry chips.
        let meniscus_val = if p.meniscus_enabled {
            format!("{:.0}%", p.meniscus_strength * 100.0)
        } else {
            "Kapalı".to_owned()
        };
        let chips = [
            ("Bulanıklık (Q/W)", format!("{:.0} px", p.blur_sigma)),
            ("Kenar Mercek (C/V)", meniscus_val),
            ("Cam Tonu (A/S)", format!("{:.0}%", p.tint_alpha * 100.0)),
            ("Işık Yansıması (Z/X)", format!("{:.2}", p.specular)),
        ];
        let gap = s(8.0);
        let cw = (w - s(40.0) - gap * 3.0) / 4.0;
        let cy = oy + s(52.0);
        let ch = s(52.0);
        unsafe {
            let cb = dc.CreateSolidColorBrush(&chip_fill, None)?;
            let cs = dc.CreateSolidColorBrush(&chip_stroke, None)?;
            for (i, (label, val)) in chips.iter().enumerate() {
                let cl = ox + s(20.0) + i as f32 * (cw + gap);
                let rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: cl,
                        top: cy,
                        right: cl + cw,
                        bottom: cy + ch,
                    },
                    radiusX: s(11.0),
                    radiusY: s(11.0),
                };
                dc.FillRoundedRectangle(&rr, &cb);
                dc.DrawRoundedRectangle(&rr, &cs, 1.0, None);
                self.draw_text(
                    &dc,
                    label,
                    D2D_RECT_F {
                        left: cl + s(9.0),
                        top: cy + s(6.0),
                        right: cl + cw - s(8.0),
                        bottom: cy + s(22.0),
                    },
                    TextStyle {
                        size_dips: 10.0,
                        weight: DWRITE_FONT_WEIGHT_MEDIUM,
                        align: 0,
                        color: muted,
                        fluent: false,
                    },
                )?;
                self.draw_text(
                    &dc,
                    val,
                    D2D_RECT_F {
                        left: cl + s(9.0),
                        top: cy + s(24.0),
                        right: cl + cw - s(8.0),
                        bottom: cy + ch - s(6.0),
                    },
                    TextStyle {
                        size_dips: 14.5,
                        weight: DWRITE_FONT_WEIGHT_BOLD,
                        align: 0,
                        color: fg,
                        fluent: false,
                    },
                )?;
            }
        }

        // Concise live guide lines.
        let light_str = if p.pointer_light { "Açık" } else { "Kapalı" };
        let prism_str = if p.meniscus_enabled {
            "Açık"
        } else {
            "Kapalı"
        };
        let theme_str = if p.is_dark { "Koyu" } else { "Açık" };
        let line1 = format!(
            "Sürükle: Taşı  ·  3D Işık: {light_str} (L)  ·  Kenar Mercek: {prism_str} (P)  ·  Tema: {theme_str} (T)"
        );
        self.draw_text(
            &dc,
            &line1,
            D2D_RECT_F {
                left: ox + s(20.0),
                top: oy + s(116.0),
                right: ox + w - s(20.0),
                bottom: oy + s(138.0),
            },
            TextStyle {
                size_dips: 11.5,
                weight: DWRITE_FONT_WEIGHT_MEDIUM,
                align: 0,
                color: fg,
                fluent: false,
            },
        )?;
        self.draw_text(
            &dc,
            "1: Liquid Crystal  ·  2: Pure Water  ·  3: Obsidian  ·  4: Silk  ·  E/R: Canlılık  ·  Esc: Çıkış",
            D2D_RECT_F {
                left: ox + s(20.0),
                top: oy + s(140.0),
                right: ox + w - s(20.0),
                bottom: oy + s(162.0),
            },
            TextStyle {
                size_dips: 11.0,
                weight: DWRITE_FONT_WEIGHT_NORMAL,
                align: 0,
                color: muted,
                fluent: false,
            },
        )?;

        unsafe { interop.EndDraw()? };
        Ok(())
    }

    fn redraw_dock_overlay(&self) -> WinResult<()> {
        let interop: ICompositionDrawingSurfaceInterop = self.dock.overlay_surface.cast()?;
        let mut offset = WinPoint::default();
        let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
        let ox = offset.x as f32;
        let oy = offset.y as f32;
        let w = self.dock.w_px;
        let h = self.dock.h_px;
        let s = |v: f32| v * self.dpi;
        let is_dark = self.params.is_dark;

        unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            dc.Clear(Some(&rgba_f(0, 0, 0, 0.0)));
        }

        self.draw_fresnel_rim(&dc, ox, oy, w, h, self.dock.radius_px)?;

        // Sample dock icons inside the Liquid Glass capsule to test contrast & reflections.
        let icons: [(&str, (u8, u8, u8)); 8] = [
            ("\u{E721}", (56, 189, 248)),
            ("\u{E7C4}", (129, 140, 248)),
            ("\u{E8B7}", (251, 191, 36)),
            ("\u{E756}", (52, 211, 153)),
            ("\u{E774}", (96, 165, 250)),
            ("\u{E8D6}", (244, 114, 182)),
            ("\u{E706}", (251, 146, 60)),
            ("\u{E713}", (148, 163, 184)),
        ];
        let icon_sz = s(44.0);
        let gap = s(12.0);
        let total = icons.len() as f32 * icon_sz + (icons.len() - 1) as f32 * gap;
        let start_x = ox + (w - total) * 0.5;
        let top_y = oy + (h - icon_sz) * 0.5;

        for (i, (glyph, (r, g, b))) in icons.iter().enumerate() {
            let hovered = self.hovered_icon == Some(i);
            let l = start_x + i as f32 * (icon_sz + gap);
            let lift = if hovered { s(3.0) } else { 0.0 };
            let alpha_top = if hovered {
                if is_dark { 0.64 } else { 0.56 }
            } else if is_dark {
                0.44
            } else {
                0.38
            };
            let alpha_bot = if hovered {
                if is_dark { 0.48 } else { 0.42 }
            } else if is_dark {
                0.30
            } else {
                0.26
            };
            let stroke = rgba_f(
                255,
                255,
                255,
                if hovered {
                    0.78
                } else if is_dark {
                    0.40
                } else {
                    0.72
                },
            );
            unsafe {
                let tile_stops = [
                    D2D1_GRADIENT_STOP {
                        position: 0.0,
                        color: rgba_f(
                            (*r).saturating_add(36),
                            (*g).saturating_add(36),
                            (*b).saturating_add(36),
                            alpha_top,
                        ),
                    },
                    D2D1_GRADIENT_STOP {
                        position: 1.0,
                        color: rgba_f(*r, *g, *b, alpha_bot),
                    },
                ];
                let tile_coll = dc.CreateGradientStopCollection(
                    &tile_stops,
                    D2D1_COLOR_SPACE_SRGB,
                    D2D1_COLOR_SPACE_SRGB,
                    D2D1_BUFFER_PRECISION_8BPC_UNORM,
                    D2D1_EXTEND_MODE_CLAMP,
                    D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
                )?;
                let fb = dc.CreateLinearGradientBrush(
                    &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                        startPoint: Vector2 {
                            X: l,
                            Y: top_y - lift,
                        },
                        endPoint: Vector2 {
                            X: l,
                            Y: top_y - lift + icon_sz,
                        },
                    },
                    None,
                    &tile_coll,
                )?;
                let sb = dc.CreateSolidColorBrush(&stroke, None)?;
                let rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: l,
                        top: top_y - lift,
                        right: l + icon_sz,
                        bottom: top_y - lift + icon_sz,
                    },
                    radiusX: s(12.0),
                    radiusY: s(12.0),
                };
                dc.FillRoundedRectangle(&rr, &fb);
                dc.DrawRoundedRectangle(&rr, &sb, 1.0, None);
            }
            let glyph_col = if is_dark {
                rgba_f(255, 255, 255, 0.98)
            } else {
                rgba_f(14, 20, 32, 0.94)
            };
            self.draw_text(
                &dc,
                glyph,
                D2D_RECT_F {
                    left: l,
                    top: top_y - lift,
                    right: l + icon_sz,
                    bottom: top_y - lift + icon_sz,
                },
                TextStyle {
                    size_dips: 18.0,
                    weight: DWRITE_FONT_WEIGHT_NORMAL,
                    align: 1,
                    color: glyph_col,
                    fluent: true,
                },
            )?;
        }

        unsafe { interop.EndDraw()? };
        Ok(())
    }

    fn handle_key(&mut self, vk: u16) {
        if vk == VK_ESCAPE {
            unsafe { PostQuitMessage(0) };
            return;
        }
        let mut changed = true;
        match vk as u8 {
            b'1' => self.params = GlassParams::liquid_crystal(self.params.is_dark),
            b'2' => self.params = GlassParams::pure_water(self.params.is_dark),
            b'3' => self.params = GlassParams::smoked_obsidian(self.params.is_dark),
            b'4' => self.params = GlassParams::frosted_silk(self.params.is_dark),
            b'T' => {
                self.params.is_dark = !self.params.is_dark;
            }
            b'L' => {
                self.params.pointer_light = !self.params.pointer_light;
            }
            b'P' => {
                self.params.meniscus_enabled = !self.params.meniscus_enabled;
            }
            b'C' => {
                self.params.meniscus_strength = (self.params.meniscus_strength - 0.12).max(0.0);
            }
            b'V' => {
                self.params.meniscus_strength = (self.params.meniscus_strength + 0.12).min(1.0);
            }
            b'Q' => self.params.blur_sigma = (self.params.blur_sigma - 2.0).max(0.0),
            b'W' => self.params.blur_sigma = (self.params.blur_sigma + 2.0).min(48.0),
            b'E' => self.params.saturation = (self.params.saturation - 0.1).max(0.6),
            b'R' => self.params.saturation = (self.params.saturation + 0.1).min(2.4),
            b'A' => self.params.tint_alpha = (self.params.tint_alpha - 0.04).max(0.0),
            b'S' => self.params.tint_alpha = (self.params.tint_alpha + 0.04).min(0.80),
            b'Z' => self.params.specular = (self.params.specular - 0.15).max(0.0),
            b'X' => self.params.specular = (self.params.specular + 0.15).min(2.6),
            _ => changed = false,
        }
        if changed {
            let _ = self.rebuild_materials();
            let _ = self.redraw_overlays();
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut LabState };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };
    match msg {
        WM_ERASEBKGND => 1,
        WM_MOUSEMOVE => {
            if !state.tracking {
                state.tracking = true;
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe { TrackMouseEvent(&mut tme) };
            }
            if let Some((start_cx, start_cy, win_x, win_y)) = state.dragging {
                let mut pt = POINT { x: 0, y: 0 };
                unsafe {
                    GetCursorPos(&mut pt);
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        win_x + (pt.x - start_cx),
                        win_y + (pt.y - start_cy),
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER,
                    );
                }
            }
            let mx = (lparam as u32 & 0xffff) as i16 as f32;
            let my = ((lparam as u32 >> 16) & 0xffff) as i16 as f32;
            state.update_pointer_light(mx, my);
            0
        }
        WM_MOUSELEAVE => {
            state.tracking = false;
            if state.hovered_icon.take().is_some() {
                let _ = state.redraw_dock_overlay();
            }
            0
        }
        WM_LBUTTONDOWN => {
            let mut pt = POINT { x: 0, y: 0 };
            let mut rc = windows_sys::Win32::Foundation::RECT::default();
            unsafe {
                GetCursorPos(&mut pt);
                GetWindowRect(hwnd, &mut rc);
                SetCapture(hwnd);
            }
            state.dragging = Some((pt.x, pt.y, rc.left, rc.top));
            0
        }
        WM_LBUTTONUP => {
            if state.dragging.take().is_some() {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            0
        }
        WM_RBUTTONUP => {
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_KEYDOWN => {
            state.handle_key(wparam as u16);
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let class_name = wide("OpenTaskBarLiquidGlassLab");
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        lpszClassName: class_name.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe { RegisterClassExW(&wc) };

    let scr_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let scr_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
            class_name.as_ptr(),
            wide("OpenTaskbar Liquid Glass Lab").as_ptr(),
            WS_POPUP,
            (scr_w - 640) / 2,
            (scr_h - 340) / 2,
            640,
            340,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        eprintln!("Failed to create test window.");
        unsafe { CoUninitialize() };
        return;
    }

    let dpi = unsafe { (GetDpiForWindow(hwnd).max(96) as f32) / 96.0 };
    let win_w = (WIN_W_DIPS * dpi).round() as i32;
    let win_h = (WIN_H_DIPS * dpi).round() as i32;
    unsafe {
        // Required on Windows 11 for Win32 HWNDs to sample live desktop content via HostBackdropBrush.
        let use_host_backdrop: u32 = 1;
        let hr_hb = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_HOSTBACKDROPBRUSH,
            (&use_host_backdrop as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        if hr_hb < 0 {
            eprintln!(
                "Warning: DWMWA_USE_HOSTBACKDROPBRUSH failed: 0x{:08X}",
                hr_hb as u32
            );
        }
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let border_none = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            (&border_none as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            (scr_w - win_w) / 2,
            (scr_h - win_h) / 2,
            win_w,
            win_h,
            0,
        );
    }

    let mut state = match LabState::new(hwnd, dpi) {
        Ok(s) => Box::new(s),
        Err(err) => {
            eprintln!("Liquid Glass Lab initialization failed: {err}");
            unsafe {
                DestroyWindow(hwnd);
                CoUninitialize();
            }
            return;
        }
    };
    let ptr = (&mut *state) as *mut LabState as isize;
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);
        ShowWindow(hwnd, SW_SHOW);
    }

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

    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        drop(state);
        DestroyWindow(hwnd);
        CoUninitialize();
    }
}
