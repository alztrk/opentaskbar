//! Real window backdrop engine (C# parity for `SettingsBackdrop.cs` / dock acrylic).
//!
//! `apply_window_backdrop` is the single entry point for GPUI settings, dock, flyouts and
//! menus: on Windows 11 it asks DWM for a real system-backed acrylic backdrop, on
//! Windows 10 it falls back to the undocumented `SetWindowCompositionAttribute`
//! acrylic accent policy, and it always removes the DWM border and extends the frame
//! into the client area. High Contrast and unsupported builds get no backdrop.
//!
//! All Win32 access sits behind [`GlassBackend`] so the decision chain (handle
//! validation, version selection, fallback order) is unit-testable without a window.
//! Helpers at the bottom of the module produce the tint/blur values consumed by the
//! DComp dock and the settings renderer.

use opentaskbar_core::log;

use super::palette;
use super::theme::SurfaceKind;
use super::tokens::Rgba;

/// `DWMWA_SYSTEMBACKDROP_TYPE` (Windows 11).
pub const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
/// `DWMSBT_NONE` — disables system backdrop when the user selects Solid or None.
pub const DWMSBT_NONE: u32 = 1;
/// `DWMSBT_TRANSIENTWINDOW` — the acrylic "flyout" backdrop.
pub const DWMSBT_TRANSIENTWINDOW: u32 = 3;
pub const DWMWA_BORDER_COLOR: u32 = 34;
/// `DWMWA_COLOR_NONE` (note: `DWMWA_COLOR_DEFAULT` is `0xFFFF_FFFF`).
pub const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
pub const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
/// `ACCENT_ENABLE_ACRYLICBLURBEHIND` (Win10 `SetWindowCompositionAttribute`).
pub const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
/// `WCA_ACCENT_POLICY`.
pub const WCA_ACCENT_POLICY: i32 = 19;
/// First Windows 11 build that implements `DWMWA_SYSTEMBACKDROP_TYPE`.
pub const WINDOWS_11_FIRST_BUILD: u32 = 22000;
/// First Windows 10 build (`ACCENT_ENABLE_ACRYLICBLURBEHIND` is available here).
pub const WINDOWS_10_FIRST_BUILD: u32 = 10240;

/// Neutral tint used when the caller does not supply an accent colour (ARGB).
pub const FALLBACK_ACCENT_ARGB: u32 = 0x0018_181C;
/// Alpha byte of the fallback acrylic tint (kept light so wallpaper shows through).
pub const FALLBACK_ACCENT_OPACITY: u8 = 0x66;

/// Blur radius range in DIPs; opaque surfaces blur less than near-transparent ones.
pub const MIN_BLUR_RADIUS: f32 = 8.0;
pub const MAX_BLUR_RADIUS: f32 = 28.0;

/// How a window was classified for backdrop purposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsVersion {
    Unsupported,
    Windows10,
    Windows11OrNewer,
}

/// The backdrop mechanism chosen for a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackdropMode {
    /// Win11: `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW`.
    SystemTransient,
    /// Win10: `ACCENT_ENABLE_ACRYLICBLURBEHIND` via `SetWindowCompositionAttribute`.
    AccentAcrylic,
    /// High Contrast or an unsupported OS: leave the window opaque.
    None,
}

/// Classifies a reported OS build number without touching Win32 (pure).
pub fn classify_windows(build: u32) -> WindowsVersion {
    if build >= WINDOWS_11_FIRST_BUILD {
        WindowsVersion::Windows11OrNewer
    } else if build >= WINDOWS_10_FIRST_BUILD {
        WindowsVersion::Windows10
    } else {
        WindowsVersion::Unsupported
    }
}

/// Pure backdrop decision. High Contrast always wins, then Win11, then Win10.
pub fn select_backdrop(version: WindowsVersion, high_contrast: bool) -> BackdropMode {
    if high_contrast {
        return BackdropMode::None;
    }
    match version {
        WindowsVersion::Windows11OrNewer => BackdropMode::SystemTransient,
        WindowsVersion::Windows10 => BackdropMode::AccentAcrylic,
        WindowsVersion::Unsupported => BackdropMode::None,
    }
}

/// Clamps an opacity into `0.0..=1.0`; non-finite values mean "no glass".
pub fn clamp_opacity(opacity: f64) -> f64 {
    if !opacity.is_finite() {
        return 0.0;
    }
    opacity.clamp(0.0, 1.0)
}

/// Converts an opacity to an 8-bit alpha byte (shared by glass helpers and theme).
pub fn alpha_from_opacity(opacity: f64) -> u8 {
    (clamp_opacity(opacity) * 255.0).round() as u8
}

/// DIP blur radius for a given opacity: more transparent => stronger blur.
pub fn blur_radius_from_opacity(opacity: f64) -> f32 {
    MIN_BLUR_RADIUS + (MAX_BLUR_RADIUS - MIN_BLUR_RADIUS) * (1.0 - clamp_opacity(opacity)) as f32
}

/// Glass fill colour for the dock and settings surfaces (ARGB alpha from `opacity`).
pub fn glass_surface_color(is_dark: bool, opacity: f64) -> Rgba {
    glass_surface_color_for(SurfaceKind::Dock, is_dark, opacity)
}

/// Per-surface variant: dock uses the pill base, menus/widgets the popup surface,
/// settings the canvas, all with the requested glass alpha.
pub fn glass_surface_color_for(kind: SurfaceKind, is_dark: bool, opacity: f64) -> Rgba {
    let p = palette(is_dark);
    let base = match kind {
        SurfaceKind::Dock => p.dock_pill,
        SurfaceKind::Menu | SurfaceKind::Widget => p.surface,
        SurfaceKind::Settings => p.canvas,
    };
    Rgba::new(base.0, base.1, base.2, alpha_from_opacity(opacity))
}

/// Converts an `0xAARRGGBB` tint + alpha byte into the ABGR gradient colour
/// expected by `ACCENT_POLICY` (`SetWindowCompositionAttribute`).
pub fn accent_color_abgr(color_argb: u32, opacity: u8) -> u32 {
    let r = (color_argb >> 16) & 0xFF;
    let g = (color_argb >> 8) & 0xFF;
    let b = color_argb & 0xFF;
    ((u32::from(opacity)) << 24) | (b << 16) | (g << 8) | r
}

/// A handle is usable only when it is non-null (further checks live in the backend).
pub fn valid_handle(hwnd: usize) -> bool {
    hwnd != 0
}

/// Injectable Win32 seam. Production uses [`WindowsBackend`]; tests use a recorder.
pub trait GlassBackend {
    fn build_number(&self) -> u32;
    fn high_contrast(&self) -> bool;
    fn is_valid_window(&self, hwnd: usize) -> bool;
    fn set_window_attribute(&self, hwnd: usize, attribute: u32, value: u32) -> bool;
    fn extend_frame(&self, hwnd: usize) -> bool;
    fn set_accent_policy(&self, hwnd: usize, color_argb: u32, opacity: u8) -> bool;
}

/// Applies the appropriate glass backdrop to an HWND owned by this process.
/// Returns `false` (after logging) when the handle is invalid, glass is disabled,
/// or any of the required DWM/composition calls fail.
pub fn apply_window_backdrop(hwnd: usize, is_dark: bool) -> bool {
    apply_window_backdrop_with(hwnd, is_dark, &WindowsBackend)
}

/// Clears any previously applied DWM system backdrop when a window switches out of Glass mode.
pub fn clear_window_backdrop(hwnd: usize, is_dark: bool) -> bool {
    if !valid_handle(hwnd) || !WindowsBackend.is_valid_window(hwnd) {
        return false;
    }
    let _ = WindowsBackend.set_window_attribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        u32::from(is_dark),
    );
    let version = classify_windows(WindowsBackend.build_number());
    if version == WindowsVersion::Windows11OrNewer {
        WindowsBackend.set_window_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_NONE)
    } else {
        true
    }
}

/// Same as [`apply_window_backdrop`] with an explicit backend (test seam).
pub fn apply_window_backdrop_with(hwnd: usize, is_dark: bool, backend: &dyn GlassBackend) -> bool {
    if !valid_handle(hwnd) {
        log::warn("Glass backdrop rejected: null HWND.");
        return false;
    }
    if !backend.is_valid_window(hwnd) {
        log::warn(format!("Glass backdrop rejected: invalid HWND {hwnd:#x}."));
        return false;
    }

    let version = classify_windows(backend.build_number());
    let mode = select_backdrop(version, backend.high_contrast());
    if mode == BackdropMode::None {
        log::info(format!(
            "Glass backdrop skipped: mode=None, version={version:?}."
        ));
        return false;
    }

    let mut ok = true;
    if !backend.set_window_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, u32::from(is_dark)) {
        log::warn("DWMWA_USE_IMMERSIVE_DARK_MODE failed.");
        ok = false;
    }
    if !backend.set_window_attribute(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE) {
        log::warn("DWMWA_BORDER_COLOR=NONE failed.");
        ok = false;
    }
    if !backend.extend_frame(hwnd) {
        log::warn("DwmExtendFrameIntoClientArea(-1) failed.");
        ok = false;
    }

    let applied = match mode {
        BackdropMode::SystemTransient => {
            if backend.set_window_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW)
            {
                true
            } else {
                // Windows 11 build where the attribute is unavailable: use Win10 acrylic.
                log::warn("DWMWA_SYSTEMBACKDROP_TYPE failed; trying acrylic accent fallback.");
                backend.set_accent_policy(hwnd, FALLBACK_ACCENT_ARGB, FALLBACK_ACCENT_OPACITY)
            }
        }
        BackdropMode::AccentAcrylic => {
            backend.set_accent_policy(hwnd, FALLBACK_ACCENT_ARGB, FALLBACK_ACCENT_OPACITY)
        }
        BackdropMode::None => false,
    };
    if !applied {
        log::error(format!(
            "Glass backdrop could not be applied (mode={mode:?})."
        ));
        return false;
    }
    ok
}

/// Applies an explicit acrylic accent policy (used by callers that own the tint).
pub fn accent_policy(hwnd: usize, color: u32, opacity: u8) -> bool {
    accent_policy_with(hwnd, color, opacity, &WindowsBackend)
}

/// Test seam for [`accent_policy`].
pub fn accent_policy_with(
    hwnd: usize,
    color: u32,
    opacity: u8,
    backend: &dyn GlassBackend,
) -> bool {
    if !valid_handle(hwnd) {
        log::warn("Accent policy rejected: null HWND.");
        return false;
    }
    if !backend.is_valid_window(hwnd) {
        log::warn(format!("Accent policy rejected: invalid HWND {hwnd:#x}."));
        return false;
    }
    if backend.set_accent_policy(hwnd, color, opacity) {
        true
    } else {
        log::error("SetWindowCompositionAttribute(ACCENT_ENABLE_ACRYLICBLURBEHIND) failed.");
        false
    }
}

/// Production backend. On non-Windows hosts every call is a safe no-op.
pub struct WindowsBackend;

impl GlassBackend for WindowsBackend {
    #[cfg(windows)]
    fn build_number(&self) -> u32 {
        win32::os_build_number()
    }

    #[cfg(not(windows))]
    fn build_number(&self) -> u32 {
        0
    }

    #[cfg(windows)]
    fn high_contrast(&self) -> bool {
        win32::high_contrast_enabled()
    }

    #[cfg(not(windows))]
    fn high_contrast(&self) -> bool {
        false
    }

    #[cfg(windows)]
    fn is_valid_window(&self, hwnd: usize) -> bool {
        win32::is_own_window(hwnd)
    }

    #[cfg(not(windows))]
    fn is_valid_window(&self, _hwnd: usize) -> bool {
        false
    }

    #[cfg(windows)]
    fn set_window_attribute(&self, hwnd: usize, attribute: u32, value: u32) -> bool {
        win32::set_window_attribute(hwnd, attribute, value)
    }

    #[cfg(not(windows))]
    fn set_window_attribute(&self, _hwnd: usize, _attribute: u32, _value: u32) -> bool {
        false
    }

    #[cfg(windows)]
    fn extend_frame(&self, hwnd: usize) -> bool {
        win32::extend_frame(hwnd)
    }

    #[cfg(not(windows))]
    fn extend_frame(&self, _hwnd: usize) -> bool {
        false
    }

    #[cfg(windows)]
    fn set_accent_policy(&self, hwnd: usize, color_argb: u32, opacity: u8) -> bool {
        win32::set_accent_policy(hwnd, color_argb, opacity)
    }

    #[cfg(not(windows))]
    fn set_accent_policy(&self, _hwnd: usize, _color_argb: u32, _opacity: u8) -> bool {
        false
    }
}

#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;
    use std::mem::size_of;

    use windows_sys::Win32::Foundation::{FARPROC, HWND};
    use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Controls::MARGINS;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};

    use super::{ACCENT_ENABLE_ACRYLICBLURBEHIND, WCA_ACCENT_POLICY, accent_color_abgr};

    #[repr(C)]
    struct RtlOsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd_version: [u16; 128],
    }

    // `RtlGetVersion` is the only version API unaffected by the executable manifest.
    #[link(name = "ntdll")]
    unsafe extern "system" {
        #[link_name = "RtlGetVersion"]
        fn rtl_get_version(info: *mut RtlOsVersionInfoW) -> i32;
    }

    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }

    #[repr(C)]
    struct WindowCompositionAttributeData {
        attribute: i32,
        data: *mut c_void,
        size: u32,
    }

    type SetWindowCompositionAttribute =
        unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> i32;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// Build number from ntdll; `0` means unknown (no safe version-specific path).
    pub(super) fn os_build_number() -> u32 {
        let mut info = RtlOsVersionInfoW {
            size: size_of::<RtlOsVersionInfoW>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            csd_version: [0; 128],
        };
        // SAFETY: `info` is a correctly initialised, sized structure passed by pointer.
        if unsafe { rtl_get_version(&mut info) } == 0 {
            return info.build;
        }
        0
    }

    /// `HKCU\Control Panel\Accessibility\HighContrast\Flags & 1` (C# parity).
    pub(super) fn high_contrast_enabled() -> bool {
        const KEY: &str = "Control Panel\\Accessibility\\HighContrast";
        let path = wide(KEY);
        let flags_name = wide("Flags");
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: both buffer pointers are null-terminated; `key` is used only on success.
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
                return false;
            }
            let mut flags: u32 = 0;
            let mut len = size_of::<u32>() as u32;
            let status = RegQueryValueExW(
                key,
                flags_name.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                (&mut flags as *mut u32).cast(),
                &mut len,
            );
            RegCloseKey(key);
            status == 0 && flags & 1 != 0
        }
    }

    /// Only accept live windows belonging to the current process.
    pub(super) fn is_own_window(hwnd: usize) -> bool {
        if hwnd == 0 {
            return false;
        }
        let handle = hwnd as HWND;
        // SAFETY: `handle` is an opaque pointer; Win32 validates it and writes only
        // to the local `pid` when the call succeeds.
        unsafe {
            if IsWindow(handle) == 0 {
                return false;
            }
            let mut pid = 0u32;
            if GetWindowThreadProcessId(handle, &mut pid) == 0 || pid != GetCurrentProcessId() {
                return false;
            }
        }
        true
    }

    pub(super) fn set_window_attribute(hwnd: usize, attribute: u32, value: u32) -> bool {
        // SAFETY: the attribute is a plain u32 and the HWND was validated by the caller.
        unsafe {
            DwmSetWindowAttribute(
                hwnd as HWND,
                attribute,
                (&value as *const u32).cast(),
                size_of::<u32>() as u32,
            ) == 0
        }
    }

    /// `DwmExtendFrameIntoClientArea` with `-1` margins (full glass client area).
    pub(super) fn extend_frame(hwnd: usize) -> bool {
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        // SAFETY: `margins` is a valid, stack-owned MARGINS structure.
        unsafe { DwmExtendFrameIntoClientArea(hwnd as HWND, &margins) == 0 }
    }

    /// Undocumented `SetWindowCompositionAttribute` (Win10 acrylic fallback).
    /// Resolved dynamically so platforms without the export fail gracefully.
    pub(super) fn set_accent_policy(hwnd: usize, color_argb: u32, opacity: u8) -> bool {
        let module_name = wide("user32.dll");
        let function_name = c"SetWindowCompositionAttribute";
        // SAFETY: both names are null-terminated; the module handle is never freed.
        let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
        if module.is_null() {
            return false;
        }
        // SAFETY: `GetProcAddress` accepts a null-terminated ASCII symbol name.
        let address: FARPROC = unsafe { GetProcAddress(module, function_name.as_ptr().cast()) };
        let Some(address) = address else {
            return false;
        };
        // SAFETY: both are ABI-compatible function pointers; the signature matches
        // the documented reverse-engineered `SetWindowCompositionAttribute`.
        let function: SetWindowCompositionAttribute = unsafe { std::mem::transmute(address) };

        let mut policy = AccentPolicy {
            state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
            flags: 2,
            gradient_color: accent_color_abgr(color_argb, opacity),
            animation_id: 0,
        };
        let mut data = WindowCompositionAttributeData {
            attribute: WCA_ACCENT_POLICY,
            data: (&mut policy as *mut AccentPolicy).cast(),
            size: size_of::<AccentPolicy>() as u32,
        };
        // SAFETY: `data.pData` points at a live `AccentPolicy` for the call duration
        // and `data.size` describes exactly that allocation.
        unsafe { function(hwnd as HWND, &mut data) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingBackend {
        build: u32,
        high_contrast: bool,
        valid: bool,
        fail_attribute: Option<u32>,
        fail_extend: bool,
        fail_policy: bool,
    }

    impl RecordingBackend {
        fn win11() -> Self {
            Self {
                build: 22_000,
                valid: true,
                ..Self::default()
            }
        }

        fn win10() -> Self {
            Self {
                build: 19_041,
                valid: true,
                ..Self::default()
            }
        }
    }

    impl GlassBackend for RecordingBackend {
        fn build_number(&self) -> u32 {
            self.build
        }
        fn high_contrast(&self) -> bool {
            self.high_contrast
        }
        fn is_valid_window(&self, _hwnd: usize) -> bool {
            self.valid
        }
        fn set_window_attribute(&self, hwnd: usize, attribute: u32, _value: u32) -> bool {
            assert_ne!(hwnd, 0);
            self.fail_attribute != Some(attribute)
        }
        fn extend_frame(&self, _hwnd: usize) -> bool {
            !self.fail_extend
        }
        fn set_accent_policy(&self, hwnd: usize, color: u32, opacity: u8) -> bool {
            assert_ne!(hwnd, 0);
            !self.fail_policy && (color != 0 || opacity != 0)
        }
    }

    // Mutable recorder used where call contents matter (the immutable one above
    // keeps the trait accepting `&self`, matching the production backend).
    struct MutableBackend {
        inner: RecordingBackend,
        calls: std::cell::RefCell<Vec<(u32, u32)>>,
        policy_calls: std::cell::RefCell<Vec<(u32, u8)>>,
        frame_calls: std::cell::Cell<usize>,
    }

    impl MutableBackend {
        fn new(inner: RecordingBackend) -> Self {
            Self {
                inner,
                calls: std::cell::RefCell::new(Vec::new()),
                policy_calls: std::cell::RefCell::new(Vec::new()),
                frame_calls: std::cell::Cell::new(0),
            }
        }
    }

    impl GlassBackend for MutableBackend {
        fn build_number(&self) -> u32 {
            self.inner.build_number()
        }
        fn high_contrast(&self) -> bool {
            self.inner.high_contrast()
        }
        fn is_valid_window(&self, hwnd: usize) -> bool {
            self.inner.is_valid_window(hwnd)
        }
        fn set_window_attribute(&self, hwnd: usize, attribute: u32, value: u32) -> bool {
            let ok = self.inner.set_window_attribute(hwnd, attribute, value);
            self.calls.borrow_mut().push((attribute, value));
            ok
        }
        fn extend_frame(&self, hwnd: usize) -> bool {
            let ok = self.inner.extend_frame(hwnd);
            self.frame_calls.set(self.frame_calls.get() + 1);
            ok
        }
        fn set_accent_policy(&self, hwnd: usize, color: u32, opacity: u8) -> bool {
            let ok = self.inner.set_accent_policy(hwnd, color, opacity);
            self.policy_calls.borrow_mut().push((color, opacity));
            ok
        }
    }

    #[test]
    fn version_and_backdrop_decisions_are_pure() {
        assert_eq!(classify_windows(0), WindowsVersion::Unsupported);
        assert_eq!(classify_windows(19_041), WindowsVersion::Windows10);
        assert_eq!(classify_windows(22_000), WindowsVersion::Windows11OrNewer);
        assert_eq!(classify_windows(26_100), WindowsVersion::Windows11OrNewer);
        assert_eq!(
            select_backdrop(WindowsVersion::Windows11OrNewer, false),
            BackdropMode::SystemTransient
        );
        assert_eq!(
            select_backdrop(WindowsVersion::Windows10, false),
            BackdropMode::AccentAcrylic
        );
        for version in [
            WindowsVersion::Unsupported,
            WindowsVersion::Windows10,
            WindowsVersion::Windows11OrNewer,
        ] {
            assert_eq!(select_backdrop(version, true), BackdropMode::None);
        }
    }

    #[test]
    fn opacity_and_blur_helpers_clamp_and_separate_kinds() {
        assert_eq!(clamp_opacity(f64::NAN), 0.0);
        assert_eq!(clamp_opacity(-2.0), 0.0);
        assert_eq!(clamp_opacity(4.0), 1.0);
        assert!((clamp_opacity(0.4) - 0.4).abs() < f64::EPSILON);
        assert_eq!(alpha_from_opacity(0.5), 128);
        assert_eq!(alpha_from_opacity(f64::INFINITY), 0);
        assert!((blur_radius_from_opacity(1.0) - MIN_BLUR_RADIUS).abs() < 1e-6);
        assert!((blur_radius_from_opacity(0.0) - MAX_BLUR_RADIUS).abs() < 1e-6);
        assert_eq!(
            glass_surface_color_for(SurfaceKind::Settings, true, 1.0).a,
            255
        );
        let dark_dock = glass_surface_color(true, 0.5);
        let light_dock = glass_surface_color(false, 0.5);
        assert_eq!(dark_dock.a, 128);
        assert_ne!(dark_dock.r, light_dock.r);
        assert_eq!(
            glass_surface_color_for(SurfaceKind::Menu, true, 1.0).r,
            palette(true).surface.0
        );
    }

    #[test]
    fn accent_color_is_repacked_as_abgr() {
        // 0xAARRGGBB -> 0xAABBGGRR; the alpha comes from the opacity byte.
        assert_eq!(accent_color_abgr(0x80_10_20_30, 0x90), 0x90_30_20_10);
        assert_eq!(accent_color_abgr(0x00_FF_00_00, 0), 0x00_00_00_FF);
    }

    #[test]
    fn win11_uses_system_backdrop_with_border_none_and_full_frame() {
        let backend = MutableBackend::new(RecordingBackend::win11());
        assert!(apply_window_backdrop_with(0x1234, true, &backend));
        let calls = backend.calls.borrow();
        assert!(calls.contains(&(DWMWA_USE_IMMERSIVE_DARK_MODE, 1)));
        assert!(calls.contains(&(DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE)));
        assert!(calls.contains(&(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW)));
        assert_eq!(backend.frame_calls.get(), 1);
        assert!(backend.policy_calls.borrow().is_empty());
    }

    #[test]
    fn win10_falls_back_to_acrylic_accent_policy() {
        let backend = MutableBackend::new(RecordingBackend::win10());
        assert!(apply_window_backdrop_with(0x1234, false, &backend));
        let calls = backend.calls.borrow();
        assert!(
            !calls
                .iter()
                .any(|(attribute, _)| *attribute == DWMWA_SYSTEMBACKDROP_TYPE)
        );
        assert!(calls.contains(&(DWMWA_USE_IMMERSIVE_DARK_MODE, 0)));
        assert_eq!(
            backend.policy_calls.borrow().as_slice(),
            &[(FALLBACK_ACCENT_ARGB, FALLBACK_ACCENT_OPACITY)]
        );
    }

    #[test]
    fn failed_win11_backdrop_retries_with_accent_policy() {
        let mut inner = RecordingBackend::win11();
        inner.fail_attribute = Some(DWMWA_SYSTEMBACKDROP_TYPE);
        let backend = MutableBackend::new(inner);
        assert!(apply_window_backdrop_with(0x1234, true, &backend));
        assert_eq!(backend.policy_calls.borrow().len(), 1);
    }

    #[test]
    fn failures_and_bad_handles_return_false() {
        assert!(!valid_handle(0));
        let backend = MutableBackend::new(RecordingBackend::win11());
        assert!(!apply_window_backdrop_with(0, true, &backend));
        assert!(backend.calls.borrow().is_empty());

        let mut invalid = RecordingBackend::win11();
        invalid.valid = false;
        let backend = MutableBackend::new(invalid);
        assert!(!apply_window_backdrop_with(0x1234, true, &backend));

        let mut high_contrast = RecordingBackend::win11();
        high_contrast.high_contrast = true;
        let backend = MutableBackend::new(high_contrast);
        assert!(!apply_window_backdrop_with(0x1234, true, &backend));
        assert!(backend.calls.borrow().is_empty());

        let mut broken = RecordingBackend::win10();
        broken.fail_policy = true;
        let backend = MutableBackend::new(broken);
        assert!(!apply_window_backdrop_with(0x1234, true, &backend));

        let mut broken = RecordingBackend::win11();
        broken.fail_extend = true;
        let backend = MutableBackend::new(broken);
        assert!(!apply_window_backdrop_with(0x1234, true, &backend));
    }

    #[test]
    fn explicit_accent_policy_validates_handle_and_forwards_tint() {
        let backend = MutableBackend::new(RecordingBackend::win10());
        assert!(accent_policy_with(0x1234, 0x00_38_BD_F8, 0x80, &backend));
        assert_eq!(
            backend.policy_calls.borrow().as_slice(),
            &[(0x00_38_BD_F8, 0x80)]
        );
        assert!(!accent_policy_with(0, 0, 0, &backend));
    }
}
