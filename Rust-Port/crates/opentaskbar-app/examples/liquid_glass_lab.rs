//! Standalone interactive local test laboratory for OpenTaskbar's zero-side-effect
//! Windows 11 Spatial Glass engine (`Windows.UI.Composition` HostBackdropBrush +
//! razor-thin 1.25 px cursor-reactive 3D specular edge lighting).
//!
//! Design goals (Path A - Architectural / visionOS Spatial Glass):
//! - Zero screen capture, zero `WDA_EXCLUDEFROMCAPTURE`, zero yellow border, zero gray box.
//! - Mirror-clean optical glass body: natural filmic saturation, balanced blur, neutral tint,
//!   zero sand grain, zero thick Aero/plastic inner halo.
//! - Razor-thin 1.25 px optical glass edge lit in real time by a GPU `PointLight` +
//!   `SceneLightingEffect` masked strictly to the capsule perimeter (`CompositionMaskBrush`).
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
    CompositionBackdropBrush, CompositionBrush, CompositionColorBrush, CompositionDrawingSurface,
    CompositionEffectSourceParameter, CompositionGraphicsDevice, CompositionStretch, Compositor,
    ContainerVisual, DropShadow, PointLight, SpriteVisual,
};
use windows::Win32::Foundation::{HWND as WinHwnd, POINT as WinPoint};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F, D2D1_GRADIENT_STOP};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1ArithmeticComposite, CLSID_D2D1GaussianBlur, CLSID_D2D1Saturation,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BUFFER_PRECISION_8BPC_UNORM,
    D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
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

// Window & two floating glass capsule geometries (in DIPs), with generous drop-shadow margin.
const WIN_W_DIPS: f32 = 648.0;
const WIN_H_DIPS: f32 = 344.0;
const CARD_RECT_DIPS: (f32, f32, f32, f32, f32) = (40.0, 26.0, 568.0, 178.0, 22.0);
const DOCK_RECT_DIPS: (f32, f32, f32, f32, f32) = (40.0, 224.0, 568.0, 66.0, 24.0);

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
// Pure-Rust Direct2D -> Windows.UI.Composition Effect Wrappers (Zero Win2D DLL)
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
            1 => PropertyValue::CreateUInt32(1)?.cast(), // Balanced
            2 => PropertyValue::CreateUInt32(1)?.cast(), // Hard border mode
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
            1 => PropertyValue::CreateBoolean(true)?.cast(),
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

// ============================================================================
// Architectural / Spatial Glass Presets (Natural Filmic Colors, Zero Halo)
// ============================================================================

#[derive(Clone, Copy)]
struct GlassParams {
    preset_name: &'static str,
    is_dark: bool,
    blur_sigma: f32,
    saturation: f32,
    tint_alpha: f32,
    edge_glow: f32,
    pointer_light: bool,
}

impl GlassParams {
    fn spatial_glass(is_dark: bool) -> Self {
        Self {
            preset_name: "1 · Spatial Glass",
            is_dark,
            blur_sigma: 16.0,
            saturation: 1.28,
            tint_alpha: if is_dark { 0.20 } else { 0.32 },
            edge_glow: 0.78,
            pointer_light: true,
        }
    }

    fn clear_crystal(is_dark: bool) -> Self {
        Self {
            preset_name: "2 · Clear Crystal",
            is_dark,
            blur_sigma: 9.0,
            saturation: 1.32,
            tint_alpha: if is_dark { 0.12 } else { 0.18 },
            edge_glow: 0.88,
            pointer_light: true,
        }
    }

    fn deep_obsidian(is_dark: bool) -> Self {
        Self {
            preset_name: "3 · Deep Obsidian",
            is_dark,
            blur_sigma: 22.0,
            saturation: 1.18,
            tint_alpha: if is_dark { 0.44 } else { 0.46 },
            edge_glow: 0.68,
            pointer_light: true,
        }
    }

    fn soft_frost(is_dark: bool) -> Self {
        Self {
            preset_name: "4 · Soft Frost",
            is_dark,
            blur_sigma: 32.0,
            saturation: 1.16,
            tint_alpha: if is_dark { 0.28 } else { 0.40 },
            edge_glow: 0.58,
            pointer_light: true,
        }
    }
}

// ============================================================================
// Spatial Glass Capsule (Clean Optical Body + 1.25 px Reactive Edge Light)
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
    glass_visual: SpriteVisual,
    _overlay_visual: SpriteVisual,
    rim_mask_surface: CompositionDrawingSurface,
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

        // Whisper-soft dual-depth floating shadow (wide atmospheric lift + subtle close contact).
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
        drop_shadow_lift.SetBlurRadius(32.0 * dpi)?;
        drop_shadow_lift.SetOffset(Vector3 {
            X: 0.0,
            Y: 14.0 * dpi,
            Z: 0.0,
        })?;
        drop_shadow_lift.SetColor(ui_color(74, 0, 4, 12))?;

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
        drop_shadow_contact.SetBlurRadius(6.0 * dpi)?;
        drop_shadow_contact.SetOffset(Vector3 {
            X: 0.0,
            Y: 2.0 * dpi,
            Z: 0.0,
        })?;
        drop_shadow_contact.SetColor(ui_color(44, 0, 2, 8))?;

        let shadow_contact_visual = compositor.CreateSpriteVisual()?;
        shadow_contact_visual.SetSize(Vector2 { X: w_px, Y: h_px })?;
        shadow_contact_visual.SetOffset(Vector3 {
            X: x_px,
            Y: y_px,
            Z: 0.0,
        })?;
        shadow_contact_visual.SetShadow(&drop_shadow_contact)?;
        parent.Children()?.InsertAtTop(&shadow_contact_visual)?;

        // Layer 1: Unified Spatial Glass visual (optical body + additive 1.35 px cursor edge light).
        let glass_visual = compositor.CreateSpriteVisual()?;
        glass_visual.SetSize(Vector2 { X: w_px, Y: h_px })?;
        glass_visual.SetOffset(Vector3 {
            X: x_px,
            Y: y_px,
            Z: 0.0,
        })?;
        glass_visual.SetClip(&clip)?;
        parent.Children()?.InsertAtTop(&glass_visual)?;

        // Perimeter stroke mask multiplied into SceneLightingEffect inside `glass_visual`
        // so unlit rim pixels contribute (0, 0, 0) instead of painting a black outline.
        let rim_mask_surface = gfx_device.CreateDrawingSurface(
            Size {
                Width: w_px,
                Height: h_px,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        upload_razor_rim_mask(&rim_mask_surface, w_px, h_px, radius_px, dpi)?;

        // Layer 2: Crisp static 1 px top-left glass cut + UI content overlay.
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
            glass_visual,
            _overlay_visual: overlay_visual,
            rim_mask_surface,
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
        // Neutral architectural obsidian (dark) or crisp alabaster (light) — zero color cast.
        let tint_rgb = if params.is_dark {
            ui_color(255, 14, 16, 22)
        } else {
            ui_color(255, 246, 248, 252)
        };
        let tint_brush: CompositionColorBrush = compositor.CreateColorBrushWithColor(tint_rgb)?;

        let backdrop_param: IGraphicsEffectSource =
            CompositionEffectSourceParameter::Create(&HSTRING::from("Backdrop"))?.cast()?;
        let tint_param: IGraphicsEffectSource =
            CompositionEffectSourceParameter::Create(&HSTRING::from("Tint"))?.cast()?;

        let blur: IGraphicsEffectSource = IGraphicsEffect::from(GaussianBlurEffect {
            name: RefCell::new(HSTRING::from("Blur")),
            blur_sigma: params.blur_sigma,
            source: backdrop_param,
        })
        .cast()?;

        let sat: IGraphicsEffectSource = IGraphicsEffect::from(SaturationEffect {
            name: RefCell::new(HSTRING::from("Sat")),
            saturation: params.saturation,
            source: blur,
        })
        .cast()?;

        let a = params.tint_alpha.clamp(0.0, 0.85);
        let tinted_glass: IGraphicsEffect = IGraphicsEffect::from(ArithmeticCompositeEffect {
            name: RefCell::new(HSTRING::from("GlassBody")),
            c1: 0.0,
            c2: 1.0 - a,
            c3: a,
            c4: 0.0,
            source1: sat,
            source2: tint_param,
        });

        // Composite cursor-reactive 1.35 px specular edge light directly via Screen blend:
        // unlit pixels add (0,0,0) -> zero black outline on bright wallpapers!
        let (final_effect, use_rim_mask) = if params.pointer_light && params.edge_glow > 0.05 {
            let rim_mask_param: IGraphicsEffectSource =
                CompositionEffectSourceParameter::Create(&HSTRING::from("RimMask"))?.cast()?;
            let edge_light = SceneLightingEffect::new()?;
            edge_light.SetName(&HSTRING::from("RazorEdgeLight"))?;
            edge_light.SetAmbientAmount(0.0)?;
            edge_light.SetDiffuseAmount(0.85 * params.edge_glow)?;
            edge_light.SetSpecularAmount(0.80 * params.edge_glow)?;
            edge_light.SetSpecularShine(28.0)?;
            edge_light.SetReflectanceModel(SceneLightingEffectReflectanceModel::BlinnPhong)?;
            let light_src: IGraphicsEffectSource = edge_light.cast()?;

            let masked_light: IGraphicsEffectSource =
                IGraphicsEffect::from(ArithmeticCompositeEffect {
                    name: RefCell::new(HSTRING::from("MaskedEdgeLight")),
                    c1: 1.0,
                    c2: 0.0,
                    c3: 0.0,
                    c4: 0.0,
                    source1: light_src,
                    source2: rim_mask_param,
                })
                .cast()?;

            let screen_glass: IGraphicsEffect = IGraphicsEffect::from(ArithmeticCompositeEffect {
                name: RefCell::new(HSTRING::from("SpatialGlassFinal")),
                c1: -0.60,
                c2: 1.0,
                c3: 1.0,
                c4: 0.0,
                source1: tinted_glass.cast()?,
                source2: masked_light,
            });
            (screen_glass, true)
        } else {
            (tinted_glass, false)
        };

        let factory = compositor.CreateEffectFactory(&final_effect)?;
        let effect_brush = factory.CreateBrush()?;
        effect_brush.SetSourceParameter(&HSTRING::from("Backdrop"), backdrop)?;
        effect_brush.SetSourceParameter(&HSTRING::from("Tint"), &tint_brush)?;
        if use_rim_mask {
            let rim_mask_brush =
                compositor.CreateSurfaceBrushWithSurface(&self.rim_mask_surface)?;
            rim_mask_brush.SetStretch(CompositionStretch::Fill)?;
            effect_brush.SetSourceParameter(&HSTRING::from("RimMask"), &rim_mask_brush)?;
        }
        let brush: CompositionBrush = effect_brush.cast()?;
        self.glass_visual.SetBrush(&brush)?;

        self.drop_shadow_lift.SetColor(if params.is_dark {
            ui_color(72, 0, 4, 12)
        } else {
            ui_color(38, 12, 20, 32)
        })?;
        self.drop_shadow_contact.SetColor(if params.is_dark {
            ui_color(42, 0, 2, 8)
        } else {
            ui_color(24, 10, 16, 26)
        })?;
        Ok(())
    }
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

/// Renders a crisp 1.35 DIP anti-aliased perimeter stroke mask so cursor light glides
/// exclusively along the thin cut edge of the glass without fogging the interior.
fn upload_razor_rim_mask(
    surface: &CompositionDrawingSurface,
    w_px: f32,
    h_px: f32,
    radius_px: f32,
    dpi: f32,
) -> WinResult<()> {
    let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
    let mut offset = WinPoint::default();
    let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
    let ox = offset.x as f32;
    let oy = offset.y as f32;
    let stroke_w = (1.35 * dpi).max(1.25);
    let inset = stroke_w * 0.5;
    unsafe {
        dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        dc.Clear(Some(&rgba_f(0, 0, 0, 0.0)));
        let white = dc.CreateSolidColorBrush(&rgba_f(255, 255, 255, 1.0), None)?;
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: ox + inset,
                top: oy + inset,
                right: ox + w_px - inset,
                bottom: oy + h_px - inset,
            },
            radiusX: (radius_px - inset).max(1.0),
            radiusY: (radius_px - inset).max(1.0),
        };
        dc.DrawRoundedRectangle(&rr, &white, stroke_w, None);
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
    point_light: PointLight,
    card: GlassPiece,
    dock: GlassPiece,
    params: GlassParams,
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

        // Focused cursor-tracking 3D PointLight illuminating ONLY the 1.25 px glass rim.
        let point_light = compositor.CreatePointLight()?;
        point_light.SetCoordinateSpace(&root)?;
        point_light.SetColor(ui_color(255, 255, 255, 255))?;
        point_light.SetConstantAttenuation(1.0)?;
        point_light.SetLinearAttenuation(0.004)?;
        point_light.SetQuadraticAttenuation(0.000_045)?;
        point_light.SetOffset(Vector3 {
            X: WIN_W_DIPS * dpi * 0.28,
            Y: WIN_H_DIPS * dpi * 0.18,
            Z: 58.0 * dpi,
        })?;
        point_light.Targets()?.Add(&card.glass_visual)?;
        point_light.Targets()?.Add(&dock.glass_visual)?;

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
            point_light,
            card,
            dock,
            params: GlassParams::spatial_glass(true),
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
        self.point_light.SetColor(if p.pointer_light {
            ui_color(255, 255, 255, 255)
        } else {
            ui_color(0, 0, 0, 0)
        })?;
        Ok(())
    }

    fn update_pointer_light(&mut self, x: f32, y: f32) {
        let _ = self.point_light.SetOffset(Vector3 {
            X: x,
            Y: y,
            Z: 58.0 * self.dpi,
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

    /// Draws ONLY a razor-clean 1 px architectural glass edge cut (top-left subtle sky catch,
    /// whisper-thin bottom-right reflection). Zero thick inner glow, zero dark ink outline.
    fn draw_spatial_rim(
        &self,
        dc: &ID2D1DeviceContext,
        ox: f32,
        oy: f32,
        w: f32,
        h: f32,
        r: f32,
    ) -> WinResult<()> {
        let is_dark = self.params.is_dark;
        let edge_scale = (self.params.edge_glow / 0.72).clamp(0.25, 1.35);

        unsafe {
            // Subtle whisper of top atmospheric glass sheen (barely-there optical depth).
            let sheen_stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: rgba_f(
                        255,
                        255,
                        255,
                        if is_dark { 0.055 } else { 0.18 } * edge_scale,
                    ),
                },
                D2D1_GRADIENT_STOP {
                    position: 0.32,
                    color: rgba_f(255, 255, 255, 0.0),
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: rgba_f(0, 0, 0, 0.0),
                },
            ];
            let sheen_coll = dc.CreateGradientStopCollection(
                &sheen_stops,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                D2D1_EXTEND_MODE_CLAMP,
                D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
            )?;
            let sheen_brush = dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 { X: ox, Y: oy },
                    endPoint: Vector2 { X: ox, Y: oy + h },
                },
                None,
                &sheen_coll,
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
            dc.FillRoundedRectangle(&body_rr, &sheen_brush);

            // Razor-clean 1.0 px architectural glass cut: delicate top-left specular catch,
            // ultra-quiet mid-span, soft bottom-right bounce.
            let rim_stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: rgba_f(
                        255,
                        255,
                        255,
                        if is_dark { 0.54 } else { 0.88 } * edge_scale,
                    ),
                },
                D2D1_GRADIENT_STOP {
                    position: 0.30,
                    color: rgba_f(
                        255,
                        255,
                        255,
                        if is_dark { 0.18 } else { 0.42 } * edge_scale,
                    ),
                },
                D2D1_GRADIENT_STOP {
                    position: 0.65,
                    color: rgba_f(
                        255,
                        255,
                        255,
                        if is_dark { 0.12 } else { 0.28 } * edge_scale,
                    ),
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: rgba_f(
                        255,
                        255,
                        255,
                        if is_dark { 0.32 } else { 0.64 } * edge_scale,
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
                        X: ox + w * 0.45,
                        Y: oy + h,
                    },
                },
                None,
                &rim_coll,
            )?;
            let stroke_w = (1.0 * self.dpi).max(1.0);
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

            let shadow_col = if self.params.is_dark {
                rgba_f(0, 0, 0, 0.34)
            } else {
                rgba_f(255, 255, 255, 0.45)
            };
            let shadow_brush = dc.CreateSolidColorBrush(&shadow_col, None)?;
            let sh_off = (0.75 * self.dpi).max(1.0);
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
            rgba_f(248, 250, 252, 0.96)
        } else {
            rgba_f(15, 20, 28, 0.94)
        };
        let muted = if p.is_dark {
            rgba_f(218, 224, 234, 0.80)
        } else {
            rgba_f(44, 52, 66, 0.80)
        };
        let chip_fill = if p.is_dark {
            rgba_f(255, 255, 255, 0.07)
        } else {
            rgba_f(255, 255, 255, 0.42)
        };
        let chip_stroke = if p.is_dark {
            rgba_f(255, 255, 255, 0.14)
        } else {
            rgba_f(255, 255, 255, 0.68)
        };

        unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            dc.Clear(Some(&rgba_f(0, 0, 0, 0.0)));
        }

        self.draw_spatial_rim(&dc, ox, oy, w, h, self.card.radius_px)?;

        self.draw_text(
            &dc,
            "OpenTaskbar Spatial Glass Lab",
            D2D_RECT_F {
                left: ox + s(20.0),
                top: oy + s(16.0),
                right: ox + w - s(170.0),
                bottom: oy + s(38.0),
            },
            TextStyle {
                size_dips: 15.5,
                weight: DWRITE_FONT_WEIGHT_SEMI_BOLD,
                align: 0,
                color: fg,
                fluent: false,
            },
        )?;
        self.draw_text(
            &dc,
            p.preset_name,
            D2D_RECT_F {
                left: ox + w - s(168.0),
                top: oy + s(16.0),
                right: ox + w - s(20.0),
                bottom: oy + s(38.0),
            },
            TextStyle {
                size_dips: 12.0,
                weight: DWRITE_FONT_WEIGHT_MEDIUM,
                align: 2,
                color: muted,
                fluent: false,
            },
        )?;

        let chips = [
            ("Bulanıklık (Q/W)", format!("{:.0} px", p.blur_sigma)),
            ("Doygunluk (E/R)", format!("{:.2}x", p.saturation)),
            ("Cam Tonu (A/S)", format!("{:.0}%", p.tint_alpha * 100.0)),
            ("Kenar Işığı (Z/X)", format!("{:.0}%", p.edge_glow * 100.0)),
        ];
        let gap = s(8.0);
        let cw = (w - s(40.0) - gap * 3.0) / 4.0;
        let cy = oy + s(50.0);
        let ch = s(50.0);
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
                        bottom: cy + s(21.0),
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
                        top: cy + s(23.0),
                        right: cl + cw - s(8.0),
                        bottom: cy + ch - s(6.0),
                    },
                    TextStyle {
                        size_dips: 14.0,
                        weight: DWRITE_FONT_WEIGHT_BOLD,
                        align: 0,
                        color: fg,
                        fluent: false,
                    },
                )?;
            }
        }

        let light_str = if p.pointer_light { "Açık" } else { "Kapalı" };
        let theme_str = if p.is_dark { "Koyu" } else { "Açık" };
        let line1 = format!(
            "Sürükle: Taşı   ·   İmleç Kenar Işığı: {light_str} (L)   ·   Tema: {theme_str} (T)"
        );
        self.draw_text(
            &dc,
            &line1,
            D2D_RECT_F {
                left: ox + s(20.0),
                top: oy + s(114.0),
                right: ox + w - s(20.0),
                bottom: oy + s(136.0),
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
            "1: Spatial Glass  ·  2: Clear Crystal  ·  3: Deep Obsidian  ·  4: Soft Frost  ·  Esc: Çıkış",
            D2D_RECT_F {
                left: ox + s(20.0),
                top: oy + s(138.0),
                right: ox + w - s(20.0),
                bottom: oy + s(158.0),
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

        self.draw_spatial_rim(&dc, ox, oy, w, h, self.dock.radius_px)?;

        // Clean, minimal visionOS-style glass dock glyphs (subtle hover pill instead of noisy pastel boxes).
        let icons = [
            "\u{E721}", "\u{E7C4}", "\u{E8B7}", "\u{E756}", "\u{E774}", "\u{E8D6}", "\u{E706}",
            "\u{E713}",
        ];
        let icon_sz = s(44.0);
        let gap = s(12.0);
        let total = icons.len() as f32 * icon_sz + (icons.len() - 1) as f32 * gap;
        let start_x = ox + (w - total) * 0.5;
        let top_y = oy + (h - icon_sz) * 0.5;

        for (i, glyph) in icons.iter().enumerate() {
            let hovered = self.hovered_icon == Some(i);
            let l = start_x + i as f32 * (icon_sz + gap);
            let lift = if hovered { s(2.0) } else { 0.0 };
            let fill = if hovered {
                if is_dark {
                    rgba_f(255, 255, 255, 0.16)
                } else {
                    rgba_f(255, 255, 255, 0.58)
                }
            } else if is_dark {
                rgba_f(255, 255, 255, 0.06)
            } else {
                rgba_f(255, 255, 255, 0.30)
            };
            let stroke = if hovered {
                if is_dark {
                    rgba_f(255, 255, 255, 0.36)
                } else {
                    rgba_f(255, 255, 255, 0.85)
                }
            } else if is_dark {
                rgba_f(255, 255, 255, 0.11)
            } else {
                rgba_f(255, 255, 255, 0.45)
            };
            unsafe {
                let fb = dc.CreateSolidColorBrush(&fill, None)?;
                let sb = dc.CreateSolidColorBrush(&stroke, None)?;
                let rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: l,
                        top: top_y - lift,
                        right: l + icon_sz,
                        bottom: top_y - lift + icon_sz,
                    },
                    radiusX: s(13.0),
                    radiusY: s(13.0),
                };
                dc.FillRoundedRectangle(&rr, &fb);
                dc.DrawRoundedRectangle(&rr, &sb, 1.0, None);
            }
            let glyph_col = if is_dark {
                rgba_f(248, 250, 255, if hovered { 1.0 } else { 0.90 })
            } else {
                rgba_f(16, 22, 32, if hovered { 1.0 } else { 0.88 })
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
            b'1' => self.params = GlassParams::spatial_glass(self.params.is_dark),
            b'2' => self.params = GlassParams::clear_crystal(self.params.is_dark),
            b'3' => self.params = GlassParams::deep_obsidian(self.params.is_dark),
            b'4' => self.params = GlassParams::soft_frost(self.params.is_dark),
            b'T' => {
                self.params.is_dark = !self.params.is_dark;
            }
            b'L' => {
                self.params.pointer_light = !self.params.pointer_light;
            }
            b'Q' => self.params.blur_sigma = (self.params.blur_sigma - 2.0).max(0.0),
            b'W' => self.params.blur_sigma = (self.params.blur_sigma + 2.0).min(48.0),
            b'E' => self.params.saturation = (self.params.saturation - 0.06).max(0.8),
            b'R' => self.params.saturation = (self.params.saturation + 0.06).min(2.0),
            b'A' => self.params.tint_alpha = (self.params.tint_alpha - 0.04).max(0.0),
            b'S' => self.params.tint_alpha = (self.params.tint_alpha + 0.04).min(0.80),
            b'Z' => self.params.edge_glow = (self.params.edge_glow - 0.10).max(0.0),
            b'X' => self.params.edge_glow = (self.params.edge_glow + 0.10).min(1.0),
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

    let class_name = wide("OpenTaskBarSpatialGlassLab");
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
            wide("OpenTaskbar Spatial Glass Lab").as_ptr(),
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
            eprintln!("Spatial Glass Lab initialization failed: {err}");
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
