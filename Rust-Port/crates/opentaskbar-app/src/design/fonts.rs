//! Windows-native UI font resolution and user font selection.
//!
//! By default the app asks Windows which font it uses for menus
//! (`NONCLIENTMETRICSW.lfMenuFont`) and builds a DirectWrite family list with
//! conservative fallbacks. The user can override that from Settings → Themes with
//! any font installed on the system; the choice is persisted in `settings.json`.

use std::mem::size_of;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU32, Ordering};

use opentaskbar_core::settings::DockSettings;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::core::{BOOL, HSTRING};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SystemParametersInfoW,
};

const DEFAULT_FONT_SIZE: f32 = 13.0;
static NATIVE_FACE: OnceLock<String> = OnceLock::new();
static INSTALLED_FONTS: OnceLock<Vec<String>> = OnceLock::new();
static FONT_SIZE_CENTI: AtomicU32 = AtomicU32::new((DEFAULT_FONT_SIZE * 100.0) as u32);
static SELECTED_FAMILY: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn selected_family() -> Option<String> {
    SELECTED_FAMILY
        .get_or_init(|| RwLock::new(None))
        .read()
        .expect("font lock poisoned")
        .clone()
        .filter(|name| {
            installed_fonts()
                .iter()
                .any(|f| f.eq_ignore_ascii_case(name))
        })
}

pub fn set_selected_family(family: &str) {
    let selected = if family.trim().is_empty() {
        None
    } else {
        Some(family.trim().to_owned())
    };
    *SELECTED_FAMILY
        .get_or_init(|| RwLock::new(None))
        .write()
        .expect("font lock poisoned") = selected;
}

/// The font Windows itself uses for menus, e.g. `"Segoe UI"` on Windows 10/11.
pub fn native_ui_face() -> &'static str {
    NATIVE_FACE.get_or_init(|| {
        let mut metrics: NONCLIENTMETRICSW = unsafe { std::mem::zeroed() };
        metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETNONCLIENTMETRICS,
                metrics.cbSize,
                (&mut metrics as *mut NONCLIENTMETRICSW).cast(),
                0,
            )
        };
        if ok != 0 {
            let face = &metrics.lfMenuFont.lfFaceName;
            let end = face
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(face.len());
            let name = String::from_utf16_lossy(&face[..end]);
            if !name.is_empty() {
                return name;
            }
        }
        "Segoe UI".to_owned()
    })
}

/// Every font family installed on this Windows installation, alphabetically sorted.
/// Read once from the DirectWrite system font collection and cached for the process.
pub fn installed_fonts() -> &'static [String] {
    INSTALLED_FONTS.get_or_init(|| {
        let mut families = Vec::new();
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let initialized = hr.is_ok() || hr.0 == 0x8001_0106u32 as i32; // RPC_E_CHANGED_MODE
            if let Ok(factory) = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                && let Some(collection) = system_collection(&factory)
            {
                for index in 0..collection.GetFontFamilyCount() {
                    if let Some(name) = family_name(&collection, index) {
                        families.push(name);
                    }
                }
            }
            if initialized {
                CoUninitialize();
            }
        }
        families.sort_by_key(|name| name.to_lowercase());
        families.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        if families.is_empty() {
            families.push(native_ui_face().to_owned());
        }
        families
    })
}

unsafe fn system_collection(
    factory: &IDWriteFactory,
) -> Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection> {
    let mut collection = None;
    unsafe {
        factory
            .GetSystemFontCollection(&mut collection, false)
            .ok()?;
    }
    collection
}

unsafe fn family_name(
    collection: &windows::Win32::Graphics::DirectWrite::IDWriteFontCollection,
    index: u32,
) -> Option<String> {
    unsafe {
        let family = collection.GetFontFamily(index).ok()?;
        let names = family.GetFamilyNames().ok()?;
        let locale = HSTRING::from(opentaskbar_core::i18n::language());
        let mut found = 0u32;
        let mut exists = BOOL(0);
        let _ = names.FindLocaleName(
            windows::core::PCWSTR::from_raw(locale.as_ptr()),
            &mut found,
            &mut exists,
        );
        let chosen = if exists.as_bool() { found } else { 0 };
        let length = names.GetStringLength(chosen).ok()? as usize;
        let mut buffer = vec![0u16; length + 1];
        names.GetString(chosen, &mut buffer).ok()?;
        let name = String::from_utf16_lossy(&buffer[..length]);
        (!name.is_empty()).then_some(name)
    }
}

/// Comma-separated family list accepted by `IDWriteFactory::CreateTextFormat`.
pub fn ui_family_list() -> String {
    family_list_with(
        selected_family()
            .as_deref()
            .unwrap_or_else(|| native_ui_face()),
    )
}

fn family_list_with(primary: &str) -> String {
    let mut families = vec![primary.to_owned()];
    for fallback in ["Segoe UI", "Microsoft Sans Serif", "Arial"] {
        if !families
            .iter()
            .any(|family| family.eq_ignore_ascii_case(fallback))
        {
            families.push(fallback.to_owned());
        }
    }
    families.join(", ")
}

/// Pushes persisted font settings into the process-wide render state.
pub fn apply_settings(settings: &DockSettings) {
    set_selected_family(&settings.ui_font_family);
    set_font_size(settings.ui_font_size);
}

/// BCP 47 culture passed to DirectWrite so locale-specific glyph selection works.
pub fn ui_locale() -> String {
    opentaskbar_core::i18n::language().to_owned()
}

pub fn set_font_size(size: f32) {
    FONT_SIZE_CENTI.store((size.clamp(9.0, 24.0) * 100.0) as u32, Ordering::Release);
}

pub fn font_size() -> f32 {
    FONT_SIZE_CENTI.load(Ordering::Acquire) as f32 / 100.0
}

/// Scales a design base size (authored for 13 px) by the user's font size choice.
pub fn scaled_size(base: f32) -> f32 {
    base * font_size() / DEFAULT_FONT_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_face_is_non_empty_and_list_has_fallbacks() {
        assert!(!native_ui_face().is_empty());
        let list = ui_family_list();
        assert!(list.contains(','), "{list}");
        assert!(list.to_ascii_lowercase().contains("arial"), "{list}");
        assert_eq!(ui_locale().len(), 5);
    }

    #[test]
    fn font_size_is_clamped_and_scales() {
        set_font_size(18.0);
        assert!((font_size() - 18.0).abs() < 0.01);
        assert!((scaled_size(13.0) - 18.0).abs() < 0.01);
        set_font_size(13.0);
    }

    #[test]
    fn installed_fonts_contains_native_face() {
        assert!(
            installed_fonts()
                .iter()
                .any(|f| f.eq_ignore_ascii_case(native_ui_face()))
        );
    }
}
