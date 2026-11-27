//! Windows-native UI font resolution and user font selection.
//!
//! By default the app asks Windows which font it uses for menus
//! (`NONCLIENTMETRICSW.lfMenuFont`) and builds a DirectWrite family list with
//! conservative fallbacks. The user can override that from Settings ΓåÆ Themes with
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

/// The app's default UI family. Inter is bundled (four weights) so every machine
/// renders the same typography; this is used by both dock DComp text and GPUI.
pub fn native_ui_face() -> &'static str {
    INTER_FAMILY
}

/// The font Windows itself uses for menus, e.g. `"Segoe UI"` on Windows 10/11.
/// Kept for the OS-font setting/label and as the final GDI fallback.
pub fn system_menu_face() -> &'static str {
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
            families.push(system_menu_face().to_owned());
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
    for fallback in [
        system_menu_face(),
        "Segoe UI",
        "Microsoft Sans Serif",
        "Arial",
    ] {
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
    ensure_inter_fonts();
    set_selected_family(&settings.ui_font_family);
    set_font_size(settings.ui_font_size);
}

/// Makes the bundled Inter faces available to Win32/DirectWrite text (dock DComp,
/// flyouts and menus). GPUI registers the same bytes through `load_bundled_fonts`.
#[cfg(windows)]
pub fn ensure_inter_fonts() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        use windows_sys::Win32::Graphics::Gdi::AddFontResourceW;
        let Ok(directory) = std::env::var("LOCALAPPDATA") else {
            return;
        };
        let directory = std::path::Path::new(&directory)
            .join("Microsoft")
            .join("Windows")
            .join("Fonts");
        if std::fs::create_dir_all(&directory).is_err() {
            return;
        }
        for (file, bytes) in bundled_font_files() {
            let path = directory.join(file);
            if !path.exists() && std::fs::write(&path, bytes).is_err() {
                continue;
            }
            let wide: Vec<u16> = path
                .to_string_lossy()
                .encode_utf16()
                .chain(Some(0))
                .collect();
            unsafe {
                AddFontResourceW(wide.as_ptr());
            }
        }
    });
}

#[cfg(not(windows))]
pub fn ensure_inter_fonts() {}

fn bundled_font_files() -> [(&'static str, &'static [u8]); 4] {
    [
        ("Inter-Regular.ttf", FONT_REGULAR),
        ("Inter-Medium.ttf", FONT_MEDIUM),
        ("Inter-SemiBold.ttf", FONT_SEMIBOLD),
        ("Inter-Bold.ttf", FONT_BOLD),
    ]
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
        assert_eq!(native_ui_face(), INTER_FAMILY);
        assert!(!system_menu_face().is_empty());
        let list = ui_family_list();
        assert!(list.starts_with(INTER_FAMILY), "{list}");
        assert!(list.contains(','), "{list}");
        assert!(list.to_ascii_lowercase().contains("arial"), "{list}");
        assert_eq!(ui_locale().len(), 5);
    }

    #[test]
    fn all_four_inter_weights_are_registered() {
        assert_eq!(INTER_WEIGHTS, [400, 500, 600, 700]);
        for weight in INTER_WEIGHTS {
            assert!(inter_font_file(weight).is_some());
            assert_eq!(inter_family_for_weight(weight), INTER_FAMILY);
        }
        assert_eq!(
            bundled_fonts(),
            bundled_font_files().map(|(_, bytes)| bytes)
        );
        assert!(inter_font_file(800).is_none());
    }

    #[test]
    fn font_size_is_clamped_and_scales() {
        set_font_size(18.0);
        assert!((font_size() - 18.0).abs() < 0.01);
        assert!((scaled_size(13.0) - 18.0).abs() < 0.01);
        set_font_size(13.0);
    }

    #[test]
    fn installed_fonts_contains_system_face() {
        assert!(
            installed_fonts()
                .iter()
                .any(|f| f.eq_ignore_ascii_case(system_menu_face()))
        );
    }
}

// ---- Bundled assets (WS-F) ----
use std::borrow::Cow;

use gpui::{App, AssetSource, SharedString};

/// Inter family name exposed to GPUI styles.
pub const INTER_FAMILY: &str = "Inter";

pub const FONT_REGULAR: &[u8] = include_bytes!("../../../../Fonts/Inter-Regular.ttf");
pub const FONT_MEDIUM: &[u8] = include_bytes!("../../../../Fonts/Inter-Medium.ttf");
pub const FONT_SEMIBOLD: &[u8] = include_bytes!("../../../../Fonts/Inter-SemiBold.ttf");
pub const FONT_BOLD: &[u8] = include_bytes!("../../../../Fonts/Inter-Bold.ttf");
pub const FONT_LICENSE: &[u8] = include_bytes!("../../../../Fonts/LICENSE.txt");

/// All four embedded font weights, in ascending weight order: Regular/Medium/SemiBold/Bold.
pub const INTER_WEIGHTS: [u16; 4] = [400, 500, 600, 700];

/// All four embedded font weights, in ascending weight order.
pub fn bundled_fonts() -> [&'static [u8]; 4] {
    [FONT_REGULAR, FONT_MEDIUM, FONT_SEMIBOLD, FONT_BOLD]
}

/// Asset path for one of the four Inter weights (GPUI picks the nearest face).
pub fn inter_font_file(weight: u16) -> Option<&'static str> {
    match weight {
        400 => Some("fonts/Inter-Regular.ttf"),
        500 => Some("fonts/Inter-Medium.ttf"),
        600 => Some("fonts/Inter-SemiBold.ttf"),
        700 => Some("fonts/Inter-Bold.ttf"),
        _ => None,
    }
}

/// Family name to pass to GPUI `font_family()` for any of the four weights.
pub fn inter_family_for_weight(weight: u16) -> &'static str {
    debug_assert!(INTER_WEIGHTS.contains(&weight), "unsupported Inter weight");
    INTER_FAMILY
}

/// Registers Inter with GPUI. Call once from app startup before creating windows.
pub fn load_bundled_fonts(cx: &mut App) -> gpui::Result<()> {
    let fonts = bundled_fonts().into_iter().map(Cow::Borrowed).collect();
    cx.text_system().add_fonts(fonts)
}

pub const FLAG_LOCALES: [&str; 14] = [
    "de-DE", "en-US", "es-ES", "fr-FR", "it-IT", "ja-JP", "ko-KR", "nl-NL", "pl-PL", "pt-BR",
    "ru-RU", "tr-TR", "uk-UA", "zh-CN",
];

/// Looks up an embedded flag by locale name (e.g. `tr-TR`). Case-insensitive.
pub fn flag_bytes(locale: &str) -> Option<&'static [u8]> {
    match locale.to_ascii_lowercase().as_str() {
        "de-de" => Some(include_bytes!("../../../../Assets/Flags/de-DE.png")),
        "en-us" => Some(include_bytes!("../../../../Assets/Flags/en-US.png")),
        "es-es" => Some(include_bytes!("../../../../Assets/Flags/es-ES.png")),
        "fr-fr" => Some(include_bytes!("../../../../Assets/Flags/fr-FR.png")),
        "it-it" => Some(include_bytes!("../../../../Assets/Flags/it-IT.png")),
        "ja-jp" => Some(include_bytes!("../../../../Assets/Flags/ja-JP.png")),
        "ko-kr" => Some(include_bytes!("../../../../Assets/Flags/ko-KR.png")),
        "nl-nl" => Some(include_bytes!("../../../../Assets/Flags/nl-NL.png")),
        "pl-pl" => Some(include_bytes!("../../../../Assets/Flags/pl-PL.png")),
        "pt-br" => Some(include_bytes!("../../../../Assets/Flags/pt-BR.png")),
        "ru-ru" => Some(include_bytes!("../../../../Assets/Flags/ru-RU.png")),
        "tr-tr" => Some(include_bytes!("../../../../Assets/Flags/tr-TR.png")),
        "uk-ua" => Some(include_bytes!("../../../../Assets/Flags/uk-UA.png")),
        "zh-cn" => Some(include_bytes!("../../../../Assets/Flags/zh-CN.png")),
        _ => None,
    }
}

pub fn flag_path(locale: &str) -> String {
    format!("flags/{locale}.png")
}

pub const BUNDLED_THEME_IDS: [&str; 6] = [
    "catppuccin",
    "gruvbox",
    "nord",
    "prism",
    "solarized",
    "tokyo-night",
];

/// All embedded asset paths (fonts, flags, theme files, licenses).
pub fn all_paths() -> Vec<&'static str> {
    let mut paths = vec![
        "fonts/Inter-Regular.ttf",
        "fonts/Inter-Medium.ttf",
        "fonts/Inter-SemiBold.ttf",
        "fonts/Inter-Bold.ttf",
        "fonts/LICENSE.txt",
    ];
    paths.extend(FLAG_LOCALES.iter().map(|locale| match *locale {
        "de-DE" => "flags/de-DE.png",
        "en-US" => "flags/en-US.png",
        "es-ES" => "flags/es-ES.png",
        "fr-FR" => "flags/fr-FR.png",
        "it-IT" => "flags/it-IT.png",
        "ja-JP" => "flags/ja-JP.png",
        "ko-KR" => "flags/ko-KR.png",
        "nl-NL" => "flags/nl-NL.png",
        "pl-PL" => "flags/pl-PL.png",
        "pt-BR" => "flags/pt-BR.png",
        "ru-RU" => "flags/ru-RU.png",
        "tr-TR" => "flags/tr-TR.png",
        "uk-UA" => "flags/uk-UA.png",
        "zh-CN" => "flags/zh-CN.png",
        _ => unreachable!(),
    }));
    for id in BUNDLED_THEME_IDS {
        paths.push(theme_json_path(id));
        paths.push(widget_pack_path(id));
    }
    paths.push("themes/prism/LICENSE");
    paths.push("themes/prism/README.md");
    paths
}

pub fn theme_json_path(id: &str) -> &'static str {
    match id {
        "catppuccin" => "themes/catppuccin/theme.json",
        "gruvbox" => "themes/gruvbox/theme.json",
        "nord" => "themes/nord/theme.json",
        "prism" => "themes/prism/theme.json",
        "solarized" => "themes/solarized/theme.json",
        "tokyo-night" => "themes/tokyo-night/theme.json",
        _ => "",
    }
}

pub fn widget_pack_path(id: &str) -> &'static str {
    match id {
        "catppuccin" => "themes/catppuccin/widgets/pack.json",
        "gruvbox" => "themes/gruvbox/widgets/pack.json",
        "nord" => "themes/nord/widgets/pack.json",
        "prism" => "themes/prism/widgets/pack.json",
        "solarized" => "themes/solarized/widgets/pack.json",
        "tokyo-night" => "themes/tokyo-night/widgets/pack.json",
        _ => "",
    }
}

/// Embedded asset source. Pass to `gpui::Application::with_assets(EmbeddedAssets)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmbeddedAssets;

impl AssetSource for EmbeddedAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let key = path.replace('\\', "/");
        let bytes: Option<&'static [u8]> = match key.as_str() {
            "fonts/Inter-Regular.ttf" => Some(FONT_REGULAR),
            "fonts/Inter-Medium.ttf" => Some(FONT_MEDIUM),
            "fonts/Inter-SemiBold.ttf" => Some(FONT_SEMIBOLD),
            "fonts/Inter-Bold.ttf" => Some(FONT_BOLD),
            "fonts/LICENSE.txt" => Some(FONT_LICENSE),
            "flags/de-DE.png" => Some(include_bytes!("../../../../Assets/Flags/de-DE.png")),
            "flags/en-US.png" => Some(include_bytes!("../../../../Assets/Flags/en-US.png")),
            "flags/es-ES.png" => Some(include_bytes!("../../../../Assets/Flags/es-ES.png")),
            "flags/fr-FR.png" => Some(include_bytes!("../../../../Assets/Flags/fr-FR.png")),
            "flags/it-IT.png" => Some(include_bytes!("../../../../Assets/Flags/it-IT.png")),
            "flags/ja-JP.png" => Some(include_bytes!("../../../../Assets/Flags/ja-JP.png")),
            "flags/ko-KR.png" => Some(include_bytes!("../../../../Assets/Flags/ko-KR.png")),
            "flags/nl-NL.png" => Some(include_bytes!("../../../../Assets/Flags/nl-NL.png")),
            "flags/pl-PL.png" => Some(include_bytes!("../../../../Assets/Flags/pl-PL.png")),
            "flags/pt-BR.png" => Some(include_bytes!("../../../../Assets/Flags/pt-BR.png")),
            "flags/ru-RU.png" => Some(include_bytes!("../../../../Assets/Flags/ru-RU.png")),
            "flags/tr-TR.png" => Some(include_bytes!("../../../../Assets/Flags/tr-TR.png")),
            "flags/uk-UA.png" => Some(include_bytes!("../../../../Assets/Flags/uk-UA.png")),
            "flags/zh-CN.png" => Some(include_bytes!("../../../../Assets/Flags/zh-CN.png")),
            "themes/catppuccin/theme.json" => {
                Some(include_bytes!("../../../../themes/catppuccin/theme.json"))
            }
            "themes/catppuccin/widgets/pack.json" => Some(include_bytes!(
                "../../../../themes/catppuccin/widgets/pack.json"
            )),
            "themes/gruvbox/theme.json" => {
                Some(include_bytes!("../../../../themes/gruvbox/theme.json"))
            }
            "themes/gruvbox/widgets/pack.json" => Some(include_bytes!(
                "../../../../themes/gruvbox/widgets/pack.json"
            )),
            "themes/nord/theme.json" => Some(include_bytes!("../../../../themes/nord/theme.json")),
            "themes/nord/widgets/pack.json" => {
                Some(include_bytes!("../../../../themes/nord/widgets/pack.json"))
            }
            "themes/prism/theme.json" => {
                Some(include_bytes!("../../../../themes/prism/theme.json"))
            }
            "themes/prism/widgets/pack.json" => {
                Some(include_bytes!("../../../../themes/prism/widgets/pack.json"))
            }
            "themes/prism/LICENSE" => Some(include_bytes!("../../../../themes/prism/LICENSE")),
            "themes/prism/README.md" => Some(include_bytes!("../../../../themes/prism/README.md")),
            "themes/solarized/theme.json" => {
                Some(include_bytes!("../../../../themes/solarized/theme.json"))
            }
            "themes/solarized/widgets/pack.json" => Some(include_bytes!(
                "../../../../themes/solarized/widgets/pack.json"
            )),
            "themes/tokyo-night/theme.json" => {
                Some(include_bytes!("../../../../themes/tokyo-night/theme.json"))
            }
            "themes/tokyo-night/widgets/pack.json" => Some(include_bytes!(
                "../../../../themes/tokyo-night/widgets/pack.json"
            )),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let prefix = path.replace('\\', "/");
        let prefix = prefix.trim_end_matches('/');
        let entries: Vec<SharedString> = all_paths()
            .into_iter()
            .filter(|candidate| {
                prefix.is_empty()
                    || candidate.starts_with(&format!("{prefix}/"))
                    || candidate
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .map(SharedString::from)
            .collect();
        Ok(entries)
    }
}

#[cfg(test)]
mod asset_tests {
    use super::*;

    #[test]
    fn all_assets_embedded() {
        assert_eq!(bundled_fonts().len(), 4);
        assert!(bundled_fonts().iter().all(|bytes| bytes.len() > 100_000));
        assert_eq!(FLAG_LOCALES.len(), 14);
        assert!(
            FLAG_LOCALES
                .iter()
                .all(|locale| flag_bytes(locale).is_some())
        );
        assert_eq!(all_paths().len(), 5 + 14 + 12 + 2);
    }

    #[test]
    fn asset_source_lists_and_loads_every_path() {
        let assets = EmbeddedAssets;
        let paths = all_paths();
        for path in paths {
            assert!(assets.load(path).unwrap().is_some(), "missing {path}");
        }
        assert_eq!(assets.list("themes").unwrap().len(), 14);
        assert_eq!(assets.list("flags").unwrap().len(), 14);
        assert!(assets.load("flags/xx-XX.png").unwrap().is_none());
    }
}
