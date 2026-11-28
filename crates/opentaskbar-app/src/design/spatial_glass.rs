//! Shared production Clear Crystal Spatial Glass engine (`Windows.UI.Composition`).
//!
//! Renders a zero-side-effect, GPU-composited optical glass capsule underneath an existing
//! `IDCompositionDevice` (`topmost = true`) target on the same `WS_EX_NOREDIRECTIONBITMAP`
//! HWND via `CreateDesktopWindowTarget(hwnd, false)`:
//! - Live `HostBackdropBrush` -> `GaussianBlur` -> `Saturation` -> neutral Clear Crystal tint
//! - Unified Screen-blended 1.35 px cursor-reactive `SceneLightingEffect` edge glint
//!   (unlit rim pixels contribute `(0,0,0)` so bright wallpapers never show a dark outline)
//! - Crisp static 1 px top-left architectural glass cut + subtle top sheen
//! - Dual-depth soft GPU `DropShadow` (wide lift + close contact)
//! - Strictly hidden whenever the surface background mode is not `SurfaceBackground::Glass`,
//!   including `SurfaceBackground::TrueBlack` (an opaque black OLED-care fill).

use opentaskbar_core::settings::{DockSettings, SurfaceBackground, clear_crystal_tint_alpha};

/// Immutable optical configuration for one Clear Crystal glass surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpatialGlassConfig {
    pub enabled: bool,
    pub is_dark: bool,
    pub blur_sigma: f32,
    pub saturation: f32,
    pub tint_alpha: f32,
    pub edge_glow: f32,
    pub pointer_light: bool,
    /// When true (Dynamic Island), top corners are square/flush at `y = 0` while bottom
    /// corners are rounded with `radius_px`.
    pub top_flush: bool,
    /// Optional theme-pack glass body tint (`None` keeps Clear Crystal neutral slate).
    pub tint_rgb: Option<crate::design::Rgb>,
    /// Optional theme-pack rim & cursor-light accent (`None` keeps pure specular white).
    pub rim_rgb: Option<crate::design::Rgb>,
}

impl SpatialGlassConfig {
    /// Default Clear Crystal configuration for call sites without `DockSettings`.
    pub fn default_glass(is_dark: bool, surface_opacity: f64) -> Self {
        Self {
            enabled: true,
            is_dark,
            blur_sigma: 9.0,
            saturation: 1.32,
            tint_alpha: clear_crystal_tint_alpha(surface_opacity, is_dark),
            edge_glow: 0.88,
            pointer_light: true,
            top_flush: false,
            tint_rgb: None,
            rim_rgb: None,
        }
    }

    /// Disabled glass configuration for Solid / None / TrueBlack modes.
    pub fn disabled(is_dark: bool) -> Self {
        Self {
            enabled: false,
            is_dark,
            blur_sigma: 9.0,
            saturation: 1.32,
            tint_alpha: 0.0,
            edge_glow: 0.0,
            pointer_light: false,
            top_flush: false,
            tint_rgb: None,
            rim_rgb: None,
        }
    }

    /// Builds a validated Clear Crystal configuration for a specific surface mode and opacity.
    pub fn from_settings(
        settings: &DockSettings,
        background: SurfaceBackground,
        surface_opacity: f64,
        is_dark: bool,
        top_flush: bool,
    ) -> Self {
        let enabled = background == SurfaceBackground::Glass;
        let blur_sigma = if settings.glass_blur_sigma.is_finite() {
            settings.glass_blur_sigma.clamp(0.0, 36.0) as f32
        } else {
            9.0
        };
        let saturation = if settings.glass_saturation.is_finite() {
            settings.glass_saturation.clamp(0.8, 1.8) as f32
        } else {
            1.32
        };
        let mut edge_glow = if settings.glass_edge_glow.is_finite() {
            settings.glass_edge_glow.clamp(0.0, 1.0) as f32
        } else {
            0.88
        };
        let mut pointer_light = settings.glass_pointer_light;
        // OLED glow reduction removes both the 1 px optical rim and the cursor-tracking
        // point light while leaving the rest of the Clear Crystal glass effect intact.
        if settings.oled_reduce_glow_enabled {
            edge_glow = 0.0;
            pointer_light = false;
        }
        Self {
            enabled,
            is_dark,
            blur_sigma,
            saturation,
            tint_alpha: clear_crystal_tint_alpha(surface_opacity, is_dark),
            edge_glow,
            pointer_light,
            top_flush,
            tint_rgb: None,
            rim_rgb: None,
        }
    }

    /// Applies optional theme-pack glass body and rim tints when glass is enabled.
    /// Disabled modes (`Solid`, `None`, `TrueBlack`) ignore any theme tint.
    pub fn with_theme_tint(
        mut self,
        tint_rgb: Option<crate::design::Rgb>,
        rim_rgb: Option<crate::design::Rgb>,
    ) -> Self {
        if self.enabled {
            self.tint_rgb = tint_rgb;
            self.rim_rgb = rim_rgb;
        }
        self
    }

    /// True when the dual GPU drop shadows should be rendered.
    /// Top-flush surfaces (Dynamic Island) disable shadows so no dark halo bleeds into
    /// transparent bottom corner cutouts.
    pub const fn shadows_visible(&self) -> bool {
        self.enabled && !self.top_flush
    }
}

#[cfg(windows)]
mod imp {
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
    use windows::UI::Composition::Effects::{
        SceneLightingEffect, SceneLightingEffectReflectanceModel,
    };
    use windows::UI::Composition::{
        CompositionBackdropBrush, CompositionBrush, CompositionColorBrush,
        CompositionDrawingSurface, CompositionEffectSourceParameter, CompositionGeometricClip,
        CompositionGraphicsDevice, CompositionRoundedRectangleGeometry, CompositionStretch,
        Compositor, ContainerVisual, DropShadow, PointLight, SpriteVisual,
    };
    use windows::Win32::Foundation::{HWND as WinHwnd, POINT as WinPoint};
    use windows::Win32::Graphics::Direct2D::Common::{
        D2D_RECT_F, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
    };
    use windows::Win32::Graphics::Direct2D::{
        CLSID_D2D1ArithmeticComposite, CLSID_D2D1GaussianBlur, CLSID_D2D1Saturation,
        D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BUFFER_PRECISION_8BPC_UNORM,
        D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB, D2D1_EXTEND_MODE_CLAMP,
        D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
        D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
    };
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::System::WinRT::Composition::{
        ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop, ICompositorInterop,
    };
    use windows::Win32::System::WinRT::Graphics::Direct2D::{
        GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT,
        IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl,
    };
    use windows::Win32::System::WinRT::{
        CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
        DispatcherQueueOptions,
    };
    use windows::core::{GUID, HSTRING, Interface, PCWSTR, Result as WinResult, implement};
    use windows_numerics::{Vector2, Vector3};

    use super::SpatialGlassConfig;

    // One DispatcherQueueController per UI thread (Windows rejects creating a second one on
    // the same thread, so multi-monitor docks + Dynamic Islands reuse the thread-local instance).
    thread_local! {
        static THREAD_DISPATCHER: RefCell<Option<DispatcherQueueController>> = const { RefCell::new(None) };
    }

    fn ensure_dispatcher_queue() -> WinResult<()> {
        THREAD_DISPATCHER.with(|slot| {
            let mut guard = slot.borrow_mut();
            if guard.is_some() {
                return Ok(());
            }
            let options = DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            };
            let controller = unsafe { CreateDispatcherQueueController(options)? };
            *guard = Some(controller);
            Ok(())
        })
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

    fn blend_with_white(rgb: crate::design::Rgb, tint_weight: f32) -> (u8, u8, u8) {
        let w = tint_weight.clamp(0.0, 1.0);
        let inv = 1.0 - w;
        let mix =
            |c: u8| -> u8 { (f32::from(c) * w + 255.0 * inv).round().clamp(0.0, 255.0) as u8 };
        (mix(rgb.0), mix(rgb.1), mix(rgb.2))
    }

    fn point_light_color(config: SpatialGlassConfig) -> Color {
        if !config.pointer_light {
            return ui_color(0, 0, 0, 0);
        }
        if let Some(rim) = config.rim_rgb {
            let (r, g, b) = blend_with_white(rim, 0.40);
            ui_color(255, r, g, b)
        } else {
            ui_color(255, 255, 255, 255)
        }
    }

    // ---- Pure-Rust Direct2D -> Windows.UI.Composition Effect Wrappers ----

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
                1 => PropertyValue::CreateUInt32(1)?.cast(),
                2 => PropertyValue::CreateUInt32(1)?.cast(),
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
                0 => {
                    PropertyValue::CreateSingleArray(&[self.c1, self.c2, self.c3, self.c4])?.cast()
                }
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

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct SurfaceKey {
        w_px: u32,
        h_px: u32,
        radius_px: u32,
        dpi_pct: u32,
        top_flush: bool,
    }

    /// Live `Windows.UI.Composition` Clear Crystal glass host bound to one HWND.
    pub struct SpatialGlassHost {
        compositor: Compositor,
        _target: DesktopWindowTarget,
        _d2d_device: ID2D1Device,
        gfx_device: CompositionGraphicsDevice,
        root: ContainerVisual,
        backdrop: CompositionBackdropBrush,
        point_light: PointLight,
        geom: CompositionRoundedRectangleGeometry,
        _clip: CompositionGeometricClip,
        shadow_lift_visual: SpriteVisual,
        drop_shadow_lift: DropShadow,
        shadow_contact_visual: SpriteVisual,
        drop_shadow_contact: DropShadow,
        glass_visual: SpriteVisual,
        rim_overlay_visual: SpriteVisual,
        pill_mask_surface: Option<CompositionDrawingSurface>,
        rim_mask_surface: Option<CompositionDrawingSurface>,
        rim_overlay_surface: Option<CompositionDrawingSurface>,
        surface_key: Option<SurfaceKey>,
        config: SpatialGlassConfig,
        brush_dirty: bool,
        last_dpi: f32,
    }

    impl SpatialGlassHost {
        /// Attaches a Clear Crystal glass visual tree to `hwnd`.
        /// Pass `is_topmost = false` when an existing `IDCompositionDevice` (`topmost = true`)
        /// renders foreground content on the same HWND (Dock, Dynamic Island).
        pub fn attach(hwnd: usize, is_topmost: bool, config: SpatialGlassConfig) -> Option<Self> {
            if hwnd == 0 {
                return None;
            }
            match Self::try_attach(hwnd, is_topmost, config) {
                Ok(host) => Some(host),
                Err(err) => {
                    opentaskbar_core::log::warn(format!(
                        "SpatialGlassHost attach skipped for HWND {hwnd:#x}: {err}"
                    ));
                    None
                }
            }
        }

        fn try_attach(
            hwnd: usize,
            is_topmost: bool,
            config: SpatialGlassConfig,
        ) -> WinResult<Self> {
            ensure_dispatcher_queue()?;
            let compositor = Compositor::new()?;
            let desktop_interop: ICompositorDesktopInterop = compositor.cast()?;
            let target = unsafe {
                desktop_interop.CreateDesktopWindowTarget(WinHwnd(hwnd as _), is_topmost)?
            };

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

            let comp_interop: ICompositorInterop = compositor.cast()?;
            let gfx_device = unsafe { comp_interop.CreateGraphicsDevice(&d2d_device)? };

            let root = compositor.CreateContainerVisual()?;
            root.SetIsVisible(config.enabled)?;
            target.SetRoot(&root)?;

            let backdrop = compositor.CreateHostBackdropBrush()?;
            let geom = compositor.CreateRoundedRectangleGeometry()?;
            let clip = compositor.CreateGeometricClip()?;
            clip.SetGeometry(&geom)?;

            let drop_shadow_lift = compositor.CreateDropShadow()?;
            drop_shadow_lift.SetColor(ui_color(72, 0, 4, 12))?;
            let shadow_lift_visual = compositor.CreateSpriteVisual()?;
            shadow_lift_visual.SetShadow(&drop_shadow_lift)?;
            shadow_lift_visual.SetIsVisible(config.shadows_visible())?;
            root.Children()?.InsertAtTop(&shadow_lift_visual)?;

            let drop_shadow_contact = compositor.CreateDropShadow()?;
            drop_shadow_contact.SetColor(ui_color(42, 0, 2, 8))?;
            let shadow_contact_visual = compositor.CreateSpriteVisual()?;
            shadow_contact_visual.SetShadow(&drop_shadow_contact)?;
            shadow_contact_visual.SetIsVisible(config.shadows_visible())?;
            root.Children()?.InsertAtTop(&shadow_contact_visual)?;

            let glass_visual = compositor.CreateSpriteVisual()?;
            glass_visual.SetClip(&clip)?;
            root.Children()?.InsertAtTop(&glass_visual)?;

            let rim_overlay_visual = compositor.CreateSpriteVisual()?;
            rim_overlay_visual.SetClip(&clip)?;
            root.Children()?.InsertAtTop(&rim_overlay_visual)?;

            let point_light = compositor.CreatePointLight()?;
            point_light.SetCoordinateSpace(&root)?;
            point_light.SetColor(point_light_color(config))?;
            point_light.SetConstantAttenuation(1.0)?;
            point_light.SetLinearAttenuation(0.004)?;
            point_light.SetQuadraticAttenuation(0.000_045)?;
            point_light.SetOffset(Vector3 {
                X: -1000.0,
                Y: -1000.0,
                Z: 58.0,
            })?;
            point_light.Targets()?.Add(&glass_visual)?;

            Ok(Self {
                compositor,
                _target: target,
                _d2d_device: d2d_device,
                gfx_device,
                root,
                backdrop,
                point_light,
                geom,
                _clip: clip,
                shadow_lift_visual,
                drop_shadow_lift,
                shadow_contact_visual,
                drop_shadow_contact,
                glass_visual,
                rim_overlay_visual,
                pill_mask_surface: None,
                rim_mask_surface: None,
                rim_overlay_surface: None,
                surface_key: None,
                config,
                brush_dirty: true,
                last_dpi: 1.0,
            })
        }

        pub fn is_enabled(&self) -> bool {
            self.config.enabled
        }

        fn sync_shadow_visibility(&self) -> WinResult<()> {
            // Top-flush capsules (Dynamic Island) occupy an exact `w_px × h_px` HWND:
            // hiding both drop-shadow visuals eliminates the halo that would otherwise bleed
            // into the transparent bottom corner cutouts and get hard-clipped by DWM.
            let visible = self.config.shadows_visible();
            self.shadow_lift_visual.SetIsVisible(visible)?;
            self.shadow_contact_visual.SetIsVisible(visible)?;
            Ok(())
        }

        /// Updates optical parameters when settings or theme change (diff-checked, no-op if unchanged).
        pub fn apply_config(&mut self, config: SpatialGlassConfig) {
            if self.config == config && !self.brush_dirty {
                return;
            }
            let flush_changed = self.config.top_flush != config.top_flush;
            let overlay_changed = self.config.is_dark != config.is_dark
                || (self.config.edge_glow - config.edge_glow).abs() > 0.001
                || self.config.rim_rgb != config.rim_rgb
                || flush_changed;
            self.config = config;
            let _ = self.root.SetIsVisible(config.enabled);
            let _ = self.sync_shadow_visibility();
            if !config.enabled {
                return;
            }
            if flush_changed {
                self.surface_key = None;
            }
            self.brush_dirty = true;
            if overlay_changed && let Some(key) = self.surface_key {
                let dpi = (key.dpi_pct as f32) / 100.0;
                let _ = self.redraw_static_rim(
                    key.w_px as f32,
                    key.h_px as f32,
                    key.radius_px as f32,
                    dpi,
                );
            }
            if self.rim_mask_surface.is_some() {
                let _ = self.rebuild_glass_brush();
            }
        }

        /// Moves the 3D cursor edge light in window-local pixel coordinates (0 allocations).
        pub fn set_pointer(&mut self, x_px: f32, y_px: f32) {
            if !self.config.enabled || !self.config.pointer_light {
                return;
            }
            let _ = self.point_light.SetOffset(Vector3 {
                X: x_px,
                Y: y_px,
                Z: 58.0 * self.last_dpi.max(0.75),
            });
        }

        /// Parks the cursor light off-capsule when the pointer leaves the window.
        pub fn clear_pointer(&mut self) {
            if !self.config.enabled {
                return;
            }
            let _ = self.point_light.SetOffset(Vector3 {
                X: -2000.0,
                Y: -2000.0,
                Z: 58.0 * self.last_dpi.max(0.75),
            });
        }

        /// Updates only the root slide translation (safe from `&self` hide ticks).
        pub fn set_root_offset(&self, offset_x_px: f32, offset_y_px: f32) {
            let _ = self.root.SetOffset(Vector3 {
                X: offset_x_px,
                Y: offset_y_px,
                Z: 0.0,
            });
        }

        /// Synchronizes glass capsule geometry and root slide offset (`offset_x_px`, `offset_y_px`).
        /// Steady-state frames with unchanged dimensions do zero surface redraws.
        #[allow(clippy::too_many_arguments)]
        pub fn sync_rect(
            &mut self,
            x_px: f32,
            y_px: f32,
            w_px: f32,
            h_px: f32,
            radius_px: f32,
            dpi: f32,
            offset_x_px: f32,
            offset_y_px: f32,
        ) {
            self.last_dpi = dpi.max(0.5);
            let _ = self.root.SetOffset(Vector3 {
                X: offset_x_px,
                Y: offset_y_px,
                Z: 0.0,
            });
            if !self.config.enabled || w_px < 2.0 || h_px < 2.0 {
                return;
            }

            let rw = w_px.round().max(2.0);
            let rh = h_px.round().max(2.0);
            let rr = radius_px.round().min(rw * 0.5).min(rh * 0.5).max(1.0);
            let size = Vector2 { X: rw, Y: rh };
            let pos = Vector3 {
                X: x_px.round(),
                Y: y_px.round(),
                Z: 0.0,
            };

            for v in [
                &self.shadow_lift_visual,
                &self.shadow_contact_visual,
                &self.glass_visual,
                &self.rim_overlay_visual,
            ] {
                let _ = v.SetSize(size);
                let _ = v.SetOffset(pos);
            }

            let key = SurfaceKey {
                w_px: rw as u32,
                h_px: rh as u32,
                radius_px: rr as u32,
                dpi_pct: (dpi * 100.0).round() as u32,
                top_flush: self.config.top_flush,
            };

            if self.surface_key != Some(key) {
                // Only mark the geometry as installed when it actually rebuilt:
                // a transient surface/mask failure must be retried next tick.
                if self.rebuild_geometry_surfaces(rw, rh, rr, dpi).is_ok() {
                    self.surface_key = Some(key);
                } else {
                    self.surface_key = None;
                }
                self.brush_dirty = true;
            }

            if self.brush_dirty {
                let _ = self.rebuild_glass_brush();
            }
        }

        fn rebuild_geometry_surfaces(
            &mut self,
            w_px: f32,
            h_px: f32,
            radius_px: f32,
            dpi: f32,
        ) -> WinResult<()> {
            if self.config.top_flush {
                // Top-flush capsule (Dynamic Island): shift the rounded rect upward by `radius_px`
                // so the top edge is crisp & flat against the monitor bezel while bottom corners curve.
                self.geom.SetOffset(Vector2 {
                    X: 0.0,
                    Y: -radius_px,
                })?;
                self.geom.SetSize(Vector2 {
                    X: w_px,
                    Y: h_px + radius_px,
                })?;
            } else {
                self.geom.SetOffset(Vector2 { X: 0.0, Y: 0.0 })?;
                self.geom.SetSize(Vector2 { X: w_px, Y: h_px })?;
            }
            self.geom.SetCornerRadius(Vector2 {
                X: radius_px,
                Y: radius_px,
            })?;

            let sz = Size {
                Width: w_px,
                Height: h_px,
            };
            let pill_surf = self.gfx_device.CreateDrawingSurface(
                sz,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                DirectXAlphaMode::Premultiplied,
            )?;
            upload_pill_mask(&pill_surf, w_px, h_px, radius_px, self.config.top_flush)?;
            let pill_brush = self.compositor.CreateSurfaceBrushWithSurface(&pill_surf)?;
            pill_brush.SetStretch(CompositionStretch::Fill)?;

            self.drop_shadow_lift.SetMask(&pill_brush)?;
            self.drop_shadow_lift.SetBlurRadius(28.0 * dpi)?;
            self.drop_shadow_lift.SetOffset(Vector3 {
                X: 0.0,
                Y: if self.config.top_flush {
                    8.0 * dpi
                } else {
                    10.0 * dpi
                },
                Z: 0.0,
            })?;

            self.drop_shadow_contact.SetMask(&pill_brush)?;
            self.drop_shadow_contact.SetBlurRadius(6.0 * dpi)?;
            self.drop_shadow_contact.SetOffset(Vector3 {
                X: 0.0,
                Y: 2.0 * dpi,
                Z: 0.0,
            })?;
            self.sync_shadow_visibility()?;
            self.pill_mask_surface = Some(pill_surf);

            let rim_surf = self.gfx_device.CreateDrawingSurface(
                sz,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                DirectXAlphaMode::Premultiplied,
            )?;
            upload_razor_rim_mask(&rim_surf, w_px, h_px, radius_px, dpi, self.config.top_flush)?;
            self.rim_mask_surface = Some(rim_surf);

            let overlay_surf = self.gfx_device.CreateDrawingSurface(
                sz,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                DirectXAlphaMode::Premultiplied,
            )?;
            let overlay_brush = self
                .compositor
                .CreateSurfaceBrushWithSurface(&overlay_surf)?;
            overlay_brush.SetStretch(CompositionStretch::Fill)?;
            self.rim_overlay_visual.SetBrush(&overlay_brush)?;
            self.rim_overlay_surface = Some(overlay_surf);
            self.redraw_static_rim(w_px, h_px, radius_px, dpi)?;
            Ok(())
        }

        fn rebuild_glass_brush(&mut self) -> WinResult<()> {
            let p = self.config;
            let tint_rgb = if let Some((r, g, b)) = p.tint_rgb {
                ui_color(255, r, g, b)
            } else if p.is_dark {
                ui_color(255, 8, 11, 16)
            } else {
                ui_color(255, 246, 248, 252)
            };
            let tint_brush: CompositionColorBrush =
                self.compositor.CreateColorBrushWithColor(tint_rgb)?;

            let backdrop_param: IGraphicsEffectSource =
                CompositionEffectSourceParameter::Create(&HSTRING::from("Backdrop"))?.cast()?;
            let tint_param: IGraphicsEffectSource =
                CompositionEffectSourceParameter::Create(&HSTRING::from("Tint"))?.cast()?;

            let blur: IGraphicsEffectSource = IGraphicsEffect::from(GaussianBlurEffect {
                name: RefCell::new(HSTRING::from("Blur")),
                blur_sigma: p.blur_sigma,
                source: backdrop_param,
            })
            .cast()?;
            let sat: IGraphicsEffectSource = IGraphicsEffect::from(SaturationEffect {
                name: RefCell::new(HSTRING::from("Sat")),
                saturation: p.saturation,
                source: blur,
            })
            .cast()?;

            // Unified polarized Clear Crystal curve shared by Taskbar, Dynamic Island, Menus,
            // Flyouts, and Settings: keeps `c2 + c3 == 1.0` (zero unblurred alpha bleed) while
            // attenuating blindingly bright wallpapers enough for crisp white/dark text contrast
            // without turning any surface into an opaque slab. When a theme pack tint is active,
            // lift the blend curve so signature theme hues read clearly while preserving blur.
            let raw_a = p.tint_alpha.clamp(0.0, 0.85);
            let a = if p.tint_rgb.is_some() {
                if p.is_dark {
                    (0.48 + raw_a * 0.85).clamp(0.42, 0.82)
                } else {
                    (0.52 + raw_a * 0.72).clamp(0.48, 0.84)
                }
            } else if p.is_dark {
                (0.32 + raw_a * 0.90).clamp(0.28, 0.72)
            } else {
                (0.42 + raw_a * 0.75).clamp(0.40, 0.78)
            };
            let tinted_glass: IGraphicsEffect = IGraphicsEffect::from(ArithmeticCompositeEffect {
                name: RefCell::new(HSTRING::from("GlassBody")),
                c1: 0.0,
                c2: 1.0 - a,
                c3: a,
                c4: 0.0,
                source1: sat,
                source2: tint_param,
            });

            let want_rim_light =
                p.pointer_light && p.edge_glow > 0.05 && self.rim_mask_surface.is_some();
            let final_effect = if want_rim_light {
                let rim_mask_param: IGraphicsEffectSource =
                    CompositionEffectSourceParameter::Create(&HSTRING::from("RimMask"))?.cast()?;
                let edge_light = SceneLightingEffect::new()?;
                edge_light.SetName(&HSTRING::from("RazorEdgeLight"))?;
                edge_light.SetAmbientAmount(0.0)?;
                edge_light.SetDiffuseAmount(0.85 * p.edge_glow)?;
                edge_light.SetSpecularAmount(0.80 * p.edge_glow)?;
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

                IGraphicsEffect::from(ArithmeticCompositeEffect {
                    name: RefCell::new(HSTRING::from("SpatialGlassFinal")),
                    c1: -0.60,
                    c2: 1.0,
                    c3: 1.0,
                    c4: 0.0,
                    source1: tinted_glass.cast()?,
                    source2: masked_light,
                })
            } else {
                tinted_glass
            };

            let factory = self.compositor.CreateEffectFactory(&final_effect)?;
            let effect_brush = factory.CreateBrush()?;
            effect_brush.SetSourceParameter(&HSTRING::from("Backdrop"), &self.backdrop)?;
            effect_brush.SetSourceParameter(&HSTRING::from("Tint"), &tint_brush)?;
            if want_rim_light && let Some(ref rim_surf) = self.rim_mask_surface {
                let rim_mask_brush = self.compositor.CreateSurfaceBrushWithSurface(rim_surf)?;
                rim_mask_brush.SetStretch(CompositionStretch::Fill)?;
                effect_brush.SetSourceParameter(&HSTRING::from("RimMask"), &rim_mask_brush)?;
            }
            let brush: CompositionBrush = effect_brush.cast()?;
            self.glass_visual.SetBrush(&brush)?;

            self.point_light.SetColor(point_light_color(p))?;
            self.drop_shadow_lift.SetColor(if p.is_dark {
                ui_color(72, 0, 4, 12)
            } else {
                ui_color(38, 12, 20, 32)
            })?;
            self.drop_shadow_contact.SetColor(if p.is_dark {
                ui_color(42, 0, 2, 8)
            } else {
                ui_color(24, 10, 16, 26)
            })?;
            self.brush_dirty = false;
            Ok(())
        }

        fn redraw_static_rim(&self, w: f32, h: f32, r: f32, dpi: f32) -> WinResult<()> {
            let Some(ref surf) = self.rim_overlay_surface else {
                return Ok(());
            };
            let interop: ICompositionDrawingSurfaceInterop = surf.cast()?;
            let mut offset = WinPoint::default();
            let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
            let ox = offset.x as f32;
            let oy = offset.y as f32;
            let is_dark = self.config.is_dark;
            let edge_scale = (self.config.edge_glow / 0.88).clamp(0.0, 1.35);
            let (rim_r, rim_g, rim_b) = match self.config.rim_rgb {
                Some(rim) => blend_with_white(rim, 0.65),
                None => (255, 255, 255),
            };

            // EndDraw must run even if a gradient/brush creation fails below,
            // otherwise the surface stays locked and every later repaint fails.
            crate::design::d2d::with_surface_draw(
                || unsafe {
                    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    dc.Clear(Some(&rgba_f(0, 0, 0, 0.0)));

                    if edge_scale > 0.01 {
                        let top_shift = if self.config.top_flush { -r } else { 0.0 };
                        // Subtle whisper of top atmospheric glass sheen.
                        let sheen_stops = [
                            D2D1_GRADIENT_STOP {
                                position: 0.0,
                                color: rgba_f(
                                    rim_r,
                                    rim_g,
                                    rim_b,
                                    if is_dark { 0.055 } else { 0.18 } * edge_scale,
                                ),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 0.34,
                                color: rgba_f(rim_r, rim_g, rim_b, 0.0),
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
                                top: oy + top_shift,
                                right: ox + w,
                                bottom: oy + h,
                            },
                            radiusX: r,
                            radiusY: r,
                        };
                        dc.FillRoundedRectangle(&body_rr, &sheen_brush);

                        // Razor-clean 1.0 px architectural glass cut.
                        let rim_stops = [
                            D2D1_GRADIENT_STOP {
                                position: 0.0,
                                color: rgba_f(
                                    rim_r,
                                    rim_g,
                                    rim_b,
                                    if is_dark { 0.54 } else { 0.88 } * edge_scale,
                                ),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 0.30,
                                color: rgba_f(
                                    rim_r,
                                    rim_g,
                                    rim_b,
                                    if is_dark { 0.18 } else { 0.42 } * edge_scale,
                                ),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 0.65,
                                color: rgba_f(
                                    rim_r,
                                    rim_g,
                                    rim_b,
                                    if is_dark { 0.12 } else { 0.28 } * edge_scale,
                                ),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 1.0,
                                color: rgba_f(
                                    rim_r,
                                    rim_g,
                                    rim_b,
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
                        let stroke_w = (1.0 * dpi).max(1.0);
                        let inset = stroke_w * 0.5;
                        let rim_rr = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: ox + inset,
                                top: oy + top_shift + inset,
                                right: ox + w - inset,
                                bottom: oy + h - inset,
                            },
                            radiusX: (r - inset).max(1.0),
                            radiusY: (r - inset).max(1.0),
                        };
                        dc.DrawRoundedRectangle(&rim_rr, &rim_brush, stroke_w, None);
                    }
                    Ok(())
                },
                || unsafe { interop.EndDraw() },
            )
        }
    }

    fn upload_pill_mask(
        surface: &CompositionDrawingSurface,
        w_px: f32,
        h_px: f32,
        radius_px: f32,
        top_flush: bool,
    ) -> WinResult<()> {
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut offset = WinPoint::default();
        let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
        let ox = offset.x as f32;
        let oy = offset.y as f32;
        let top_shift = if top_flush { -radius_px } else { 0.0 };
        crate::design::d2d::with_surface_draw(
            || unsafe {
                dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                dc.Clear(Some(&rgba_f(0, 0, 0, 0.0)));
                let white = dc.CreateSolidColorBrush(&rgba_f(255, 255, 255, 1.0), None)?;
                let rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: ox,
                        top: oy + top_shift,
                        right: ox + w_px,
                        bottom: oy + h_px,
                    },
                    radiusX: radius_px,
                    radiusY: radius_px,
                };
                dc.FillRoundedRectangle(&rr, &white);
                Ok(())
            },
            || unsafe { interop.EndDraw() },
        )
    }

    fn upload_razor_rim_mask(
        surface: &CompositionDrawingSurface,
        w_px: f32,
        h_px: f32,
        radius_px: f32,
        dpi: f32,
        top_flush: bool,
    ) -> WinResult<()> {
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut offset = WinPoint::default();
        let dc: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
        let ox = offset.x as f32;
        let oy = offset.y as f32;
        let stroke_w = (1.35 * dpi).max(1.25);
        let inset = stroke_w * 0.5;
        let top_shift = if top_flush { -radius_px } else { 0.0 };
        crate::design::d2d::with_surface_draw(
            || unsafe {
                dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                dc.Clear(Some(&rgba_f(0, 0, 0, 0.0)));
                let white = dc.CreateSolidColorBrush(&rgba_f(255, 255, 255, 1.0), None)?;
                let rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: ox + inset,
                        top: oy + top_shift + inset,
                        right: ox + w_px - inset,
                        bottom: oy + h_px - inset,
                    },
                    radiusX: (radius_px - inset).max(1.0),
                    radiusY: (radius_px - inset).max(1.0),
                };
                dc.DrawRoundedRectangle(&rr, &white, stroke_w, None);
                Ok(())
            },
            || unsafe { interop.EndDraw() },
        )
    }

    /// Configures a `WS_EX_NOREDIRECTIONBITMAP` HWND so `HostBackdropBrush` samples the live
    /// desktop cleanly without DWM system-backdrop conflicts.
    pub fn configure_host_backdrop_window(hwnd: usize, is_dark: bool, glass_enabled: bool) {
        if hwnd == 0 {
            return;
        }
        use windows_sys::Win32::Foundation::HWND as SysHwnd;
        use windows_sys::Win32::Graphics::Dwm::{
            DWMWA_USE_IMMERSIVE_DARK_MODE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
        };
        use windows_sys::Win32::UI::Controls::MARGINS;
        const DWMWA_USE_HOSTBACKDROPBRUSH: u32 = 17;

        let hwnd = hwnd as SysHwnd;
        unsafe {
            let dark: u32 = u32::from(is_dark);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                (&dark as *const u32).cast(),
                size_of::<u32>() as u32,
            );
            let host_backdrop: u32 = u32::from(glass_enabled);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_HOSTBACKDROPBRUSH,
                (&host_backdrop as *const u32).cast(),
                size_of::<u32>() as u32,
            );
            let margins = if glass_enabled {
                MARGINS {
                    cxLeftWidth: -1,
                    cxRightWidth: -1,
                    cyTopHeight: -1,
                    cyBottomHeight: -1,
                }
            } else {
                MARGINS {
                    cxLeftWidth: 0,
                    cxRightWidth: 0,
                    cyTopHeight: 0,
                    cyBottomHeight: 0,
                }
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        }
    }

    /// Lightweight `IDCompositionDevice` (`topmost = true`) foreground surface paired with
    /// [`SpatialGlassHost`] (`topmost = false`) on popup windows (Menus, Flyouts, Previews).
    ///
    /// Draws premultiplied-alpha Direct2D content directly onto an `IDCompositionSurface` so the
    /// live Clear Crystal glass backdrop underneath shines through untouched.
    pub struct DCompForeground {
        device: windows::Win32::Graphics::DirectComposition::IDCompositionDevice,
        _target: windows::Win32::Graphics::DirectComposition::IDCompositionTarget,
        root: windows::Win32::Graphics::DirectComposition::IDCompositionVisual,
        surface: Option<windows::Win32::Graphics::DirectComposition::IDCompositionSurface>,
        surface_size: (u32, u32),
    }

    impl DCompForeground {
        pub fn new(hwnd: usize) -> Option<Self> {
            if hwnd == 0 {
                return None;
            }
            Self::try_new(hwnd).ok()
        }

        fn try_new(hwnd: usize) -> WinResult<Self> {
            use windows::Win32::Graphics::DirectComposition::{
                DCompositionCreateDevice2, IDCompositionDevice,
            };

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
            let device: IDCompositionDevice = unsafe { DCompositionCreateDevice2(&d2d_device)? };
            let target = unsafe { device.CreateTargetForHwnd(WinHwnd(hwnd as _), true)? };
            let root = unsafe { device.CreateVisual()? };
            unsafe {
                target.SetRoot(&root)?;
                device.Commit()?;
            }
            Ok(Self {
                device,
                _target: target,
                root,
                surface: None,
                surface_size: (0, 0),
            })
        }

        /// Draws one premultiplied-alpha frame using `ID2D1DeviceContext` and commits to DWM.
        /// Keeps the device context DPI at `96.0` (matching `dock/dcomp.rs`) and applies `dpi`
        /// via the 2D transform matrix (`dpi` for DIP coordinates, or `1.0` for pixels) so
        /// `IDCompositionSurface::BeginDraw`'s pixel clip rect and `offset` are never corrupted.
        pub fn draw<F>(&mut self, w_px: u32, h_px: u32, dpi: f32, draw_fn: F) -> WinResult<()>
        where
            F: FnOnce(&ID2D1DeviceContext) -> WinResult<()>,
        {
            use windows::Win32::Graphics::Direct2D::D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE;
            use windows::Win32::Graphics::Dxgi::Common::{
                DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
            };
            use windows_numerics::Matrix3x2;

            let w = w_px.max(1);
            let h = h_px.max(1);
            if self.surface.is_none() || self.surface_size != (w, h) {
                let surface = unsafe {
                    self.device.CreateSurface(
                        w,
                        h,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        DXGI_ALPHA_MODE_PREMULTIPLIED,
                    )?
                };
                unsafe {
                    self.root.SetContent(&surface)?;
                }
                self.surface = Some(surface);
                self.surface_size = (w, h);
            }
            let Some(surface) = self.surface.as_ref() else {
                return Ok(());
            };
            let mut offset = WinPoint { x: 0, y: 0 };
            let dc: ID2D1DeviceContext = unsafe { surface.BeginDraw(None, &mut offset)? };
            unsafe {
                let scale = dpi.max(0.5);
                dc.SetDpi(96.0, 96.0);
                dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                dc.SetTransform(&Matrix3x2 {
                    M11: scale,
                    M12: 0.0,
                    M21: 0.0,
                    M22: scale,
                    M31: offset.x as f32,
                    M32: offset.y as f32,
                });
                dc.Clear(Some(&D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                }));
            }
            let draw_res = draw_fn(&dc);
            unsafe {
                let _ = surface.EndDraw();
                self.device.Commit()?;
            }
            draw_res
        }

        /// Uploads an already-rendered 32-bit premultiplied BGRA pixel buffer onto the DComp
        /// surface and commits to DWM. Handy for existing `ID2D1DCRenderTarget` DIB pipelines
        /// (Flyouts, Previews) so zero block-drawing code has to be duplicated.
        pub fn upload_bgra(&mut self, w_px: u32, h_px: u32, pixels: &[u8]) -> WinResult<()> {
            use windows::Win32::Graphics::Direct2D::Common::{
                D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
            };
            use windows::Win32::Graphics::Direct2D::{
                D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_BITMAP_PROPERTIES,
            };
            use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

            let expected = (w_px as usize)
                .saturating_mul(h_px as usize)
                .saturating_mul(4);
            if pixels.len() < expected || w_px == 0 || h_px == 0 {
                return Ok(());
            }
            self.draw(w_px, h_px, 1.0, |dc| {
                let rt: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget = dc;
                let props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                };
                let size = windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                    width: w_px,
                    height: h_px,
                };
                let bmp = unsafe {
                    rt.CreateBitmap(size, Some(pixels.as_ptr().cast()), w_px * 4, &props)?
                };
                let dst = D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w_px as f32,
                    bottom: h_px as f32,
                };
                unsafe {
                    rt.DrawBitmap(
                        &bmp,
                        Some(&dst),
                        1.0,
                        D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                        None,
                    );
                }
                Ok(())
            })
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::SpatialGlassConfig;

    pub fn configure_host_backdrop_window(_hwnd: usize, _is_dark: bool, _glass_enabled: bool) {}

    pub struct DCompForeground;
    impl DCompForeground {
        pub fn new(_hwnd: usize) -> Option<Self> {
            None
        }
        pub fn upload_bgra(&mut self, _w_px: u32, _h_px: u32, _pixels: &[u8]) -> Result<(), ()> {
            Ok(())
        }
    }

    pub struct SpatialGlassHost {
        config: SpatialGlassConfig,
    }

    impl SpatialGlassHost {
        pub fn attach(_hwnd: usize, _is_topmost: bool, config: SpatialGlassConfig) -> Option<Self> {
            Some(Self { config })
        }
        pub fn is_enabled(&self) -> bool {
            self.config.enabled
        }
        pub fn apply_config(&mut self, config: SpatialGlassConfig) {
            self.config = config;
        }
        pub fn set_pointer(&mut self, _x_px: f32, _y_px: f32) {}
        pub fn clear_pointer(&mut self) {}
        pub fn set_root_offset(&self, _offset_x_px: f32, _offset_y_px: f32) {}
        #[allow(clippy::too_many_arguments)]
        pub fn sync_rect(
            &mut self,
            _x_px: f32,
            _y_px: f32,
            _w_px: f32,
            _h_px: f32,
            _radius_px: f32,
            _dpi: f32,
            _offset_x_px: f32,
            _offset_y_px: f32,
        ) {
        }
    }
}

pub use imp::{DCompForeground, SpatialGlassHost, configure_host_backdrop_window};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spatial_glass_is_enabled_only_in_glass_mode() {
        let s = DockSettings::default();
        assert!(
            SpatialGlassConfig::from_settings(&s, SurfaceBackground::Glass, 0.9, true, false)
                .enabled
        );
        assert!(
            !SpatialGlassConfig::from_settings(&s, SurfaceBackground::Solid, 0.9, true, false)
                .enabled
        );
        assert!(
            !SpatialGlassConfig::from_settings(&s, SurfaceBackground::None, 0.9, true, false)
                .enabled
        );
        assert!(
            !SpatialGlassConfig::from_settings(&s, SurfaceBackground::TrueBlack, 0.9, true, false)
                .enabled
        );
    }

    #[test]
    fn oled_glow_reduction_disables_rim_and_pointer_light() {
        let s = DockSettings {
            oled_reduce_glow_enabled: true,
            ..DockSettings::default()
        };
        let cfg = SpatialGlassConfig::from_settings(&s, SurfaceBackground::Glass, 0.9, true, false);
        assert!(cfg.enabled, "glow reduction must not turn glass off");
        assert_eq!(cfg.edge_glow, 0.0);
        assert!(!cfg.pointer_light);
        assert!(cfg.tint_alpha > 0.0, "the glass body tint stays intact");
    }

    #[test]
    fn clear_crystal_defaults_and_clamping_match_verified_lab_values() {
        let s = DockSettings::default();
        let dark =
            SpatialGlassConfig::from_settings(&s, SurfaceBackground::Glass, 0.9, true, false);
        assert!((dark.blur_sigma - 9.0).abs() < 0.01);
        assert!((dark.saturation - 1.32).abs() < 0.01);
        assert!((dark.edge_glow - 0.88).abs() < 0.01);
        assert!((dark.tint_alpha - 0.1215).abs() < 0.01);
        assert!(dark.pointer_light);

        let light =
            SpatialGlassConfig::from_settings(&s, SurfaceBackground::Glass, 0.9, false, true);
        assert!((light.tint_alpha - 0.18).abs() < 0.01);
        assert!(light.top_flush);

        let extreme = DockSettings {
            glass_blur_sigma: 999.0,
            glass_saturation: -5.0,
            glass_edge_glow: f64::NAN,
            ..DockSettings::default()
        };
        let clamped =
            SpatialGlassConfig::from_settings(&extreme, SurfaceBackground::Glass, 2.0, true, false);
        assert_eq!(clamped.blur_sigma, 36.0);
        assert_eq!(clamped.saturation, 0.8);
        assert_eq!(clamped.edge_glow, 0.88);
    }

    #[test]
    fn with_theme_tint_applies_only_in_glass_mode() {
        let s = DockSettings::default();
        let tint = Some((0x1E, 0x1E, 0x2E));
        let rim = Some((0xCB, 0xA6, 0xF7));

        let glass =
            SpatialGlassConfig::from_settings(&s, SurfaceBackground::Glass, 0.9, true, false)
                .with_theme_tint(tint, rim);
        assert!(glass.enabled);
        assert_eq!(glass.tint_rgb, tint);
        assert_eq!(glass.rim_rgb, rim);

        for mode in [
            SurfaceBackground::TrueBlack,
            SurfaceBackground::Solid,
            SurfaceBackground::None,
        ] {
            let disabled = SpatialGlassConfig::from_settings(&s, mode, 0.9, true, false)
                .with_theme_tint(tint, rim);
            assert!(!disabled.enabled, "{mode:?}");
            assert_eq!(disabled.tint_rgb, None, "{mode:?}");
            assert_eq!(disabled.rim_rgb, None, "{mode:?}");
        }
    }
}
