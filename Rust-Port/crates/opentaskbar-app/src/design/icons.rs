//! Vector icon catalog shared across Dock, Context Menus, Flyouts, and GPUI Settings.
//!
//! Three complementary sources, one API:
//! * the native `Segoe Fluent Icons` glyph catalog (Windows 11),
//! * a Win10-compatible `Segoe MDL2 Assets` fallback list,
//! * 93 Lucide `StreamGeometry` paths ported 1:1 from C# `Design/Icons/*.xaml`.

#[path = "lucide_paths.rs"]
mod lucide_paths;

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, HDC, SelectObject,
    SetTextColor,
};

/// Unified icon catalog backed by Windows 11's native `Segoe Fluent Icons` font.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppIcon {
    // Dock shell
    Search,
    TaskView,
    TrayChevron,
    RecycleBinEmpty,
    RecycleBinFull,
    // Dock widget chips
    Folder,
    LayoutGrid,
    // Context menus & flyouts
    Settings,
    Activity,
    Terminal,
    Shield,
    Play,
    Monitor,
    RefreshCw,
    Power,
    Plus,
    Pin,
    Close,
    Trash2,
    ChevronLeft,
    ChevronRight,
    Layers,
}

/// Official Windows 11 `Segoe Fluent Icons` Unicode codepoint for each icon.
pub const fn fluent_glyph(icon: AppIcon) -> &'static str {
    match icon {
        AppIcon::Search => "\u{E721}",
        AppIcon::TaskView => "\u{E7C4}",
        AppIcon::TrayChevron => "\u{E70E}",
        // The empty/full bin states must be visually distinct (C# uses two shell icons).
        AppIcon::RecycleBinEmpty => "\u{E74D}",
        AppIcon::RecycleBinFull => "\u{E922}",
        AppIcon::Trash2 => "\u{E74D}",
        AppIcon::Folder => "\u{E8B7}",
        AppIcon::LayoutGrid => "\u{F0E2}",
        AppIcon::Settings => "\u{E713}",
        AppIcon::Activity => "\u{E9D9}",
        AppIcon::Terminal => "\u{E756}",
        AppIcon::Shield => "\u{EA18}",
        AppIcon::Play => "\u{E768}",
        AppIcon::Monitor => "\u{E7F4}",
        AppIcon::RefreshCw => "\u{E72C}",
        AppIcon::Power => "\u{E7E8}",
        AppIcon::Plus => "\u{E710}",
        AppIcon::Pin => "\u{E718}",
        AppIcon::Close => "\u{E711}",
        AppIcon::ChevronLeft => "\u{E76B}",
        AppIcon::ChevronRight => "\u{E76C}",
        AppIcon::Layers => "\u{E81E}",
    }
}

/// Win10-compatible `Segoe MDL2 Assets` fallback set (semantic name → PUA glyph).
///
/// These nine codepoints are the ones the shell guarantees as far back as Windows 10.
/// Anything not covered here keeps its Fluent codepoint (MDL2 shares most of them).
pub const MDL2_FALLBACKS: &[(&str, &str); 9] = &[
    ("ChevronUp", "\u{E70D}"),
    ("ChevronDown", "\u{E70E}"),
    ("Plus", "\u{E710}"),
    ("Close", "\u{E711}"),
    ("Check", "\u{E73E}"),
    ("Save", "\u{E8BB}"),
    ("RecycleBinEmpty", "\u{E921}"),
    ("RecycleBinFull", "\u{E922}"),
    ("Delete", "\u{E923}"),
];

/// Resolves one of the nine guaranteed MDL2 glyphs by its semantic name.
pub fn mdl2_named(name: &str) -> Option<&'static str> {
    MDL2_FALLBACKS
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, glyph)| *glyph)
}

/// MDL2 equivalent for a dock/menu icon. Unmapped icons reuse the shared Fluent codepoint.
pub const fn mdl2_glyph(icon: AppIcon) -> &'static str {
    match icon {
        AppIcon::TrayChevron => "\u{E70E}",
        AppIcon::RecycleBinEmpty => "\u{E921}",
        AppIcon::RecycleBinFull => "\u{E922}",
        AppIcon::Trash2 => "\u{E923}",
        AppIcon::Plus => "\u{E710}",
        AppIcon::Close => "\u{E711}",
        _ => fluent_glyph(icon),
    }
}

/// `true` when Windows 11's `Segoe Fluent Icons` family is installed. On Windows 10
/// the renderer switches to the MDL2 fallback list with the same semantic glyphs.
pub fn fluent_font_installed() -> bool {
    super::fonts::installed_fonts()
        .iter()
        .any(|name| name.eq_ignore_ascii_case("Segoe Fluent Icons"))
}

/// Font family to use for shell glyphs on this machine.
pub fn icon_font_family() -> &'static str {
    if fluent_font_installed() {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    }
}

/// Glyph matching [`icon_font_family`] for `icon`.
pub fn glyph_for_icon(icon: AppIcon) -> &'static str {
    if fluent_font_installed() {
        fluent_glyph(icon)
    } else {
        mdl2_glyph(icon)
    }
}

/// Exact Lucide `StreamGeometry` path data for a C# icon resource key
/// (`IconWifi`, `IconPlayFill`, `IconNavDock`, …). Unknown names return `None`.
pub fn lucide(name: &str) -> Option<&'static str> {
    lucide_paths::LUCIDE_PATHS
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, path)| *path)
}

/// Every ported Lucide key (93 entries, including the two Lucide-style brand marks).
pub fn lucide_names() -> impl Iterator<Item = &'static str> {
    lucide_paths::LUCIDE_PATHS.iter().map(|(key, _)| *key)
}

/// Draws a crisp, ClearType-hinted shell glyph centered at `(cx, cy)` on a Win32 GDI `HDC`.
pub unsafe fn draw_fluent_icon_gdi(
    hdc: HDC,
    icon: AppIcon,
    cx: i32,
    cy: i32,
    size_px: i32,
    fg_colorref: u32,
) {
    let sz = size_px.max(10);
    let font_name: Vec<u16> = format!("{}\0", icon_font_family()).encode_utf16().collect();
    let text = glyph_for_icon(icon);
    if text.is_empty() {
        return;
    }
    let mut glyph: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let half = sz;
    let mut rc = RECT {
        left: cx - half,
        top: cy - half,
        right: cx + half,
        bottom: cy + half,
    };
    unsafe {
        let font = CreateFontW(
            -sz,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            0,
            0,
            0,
            4, // ANTIALIASED_QUALITY (grayscale AA, zero RGB ClearType fringing on dark surfaces)
            0,
            font_name.as_ptr(),
        );
        let old = SelectObject(hdc, font as _);
        SetTextColor(hdc, fg_colorref);
        DrawTextW(
            hdc,
            glyph.as_mut_ptr(),
            (glyph.len() - 1) as i32,
            &mut rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(hdc, old);
        DeleteObject(font as _);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppIcon, MDL2_FALLBACKS, fluent_glyph, glyph_for_icon, lucide, lucide_names, mdl2_glyph,
        mdl2_named,
    };

    #[test]
    fn all_fluent_icons_map_to_private_use_codepoints() {
        let all = [
            AppIcon::Search,
            AppIcon::TaskView,
            AppIcon::TrayChevron,
            AppIcon::RecycleBinEmpty,
            AppIcon::RecycleBinFull,
            AppIcon::Folder,
            AppIcon::LayoutGrid,
            AppIcon::Settings,
            AppIcon::Activity,
            AppIcon::Terminal,
            AppIcon::Shield,
            AppIcon::Play,
            AppIcon::Monitor,
            AppIcon::RefreshCw,
            AppIcon::Power,
            AppIcon::Plus,
            AppIcon::Pin,
            AppIcon::Close,
            AppIcon::Trash2,
            AppIcon::ChevronLeft,
            AppIcon::ChevronRight,
            AppIcon::Layers,
        ];
        for icon in all {
            let g = fluent_glyph(icon);
            assert_eq!(g.chars().count(), 1, "expected single glyph for {icon:?}");
            let cp = g.chars().next().unwrap() as u32;
            assert!(
                (0xE000..=0xF8FF).contains(&cp),
                "expected PUA Fluent glyph for {icon:?}, got U+{cp:04X}"
            );
        }
    }

    #[test]
    fn recycle_bin_states_use_distinct_glyphs() {
        assert_ne!(
            fluent_glyph(AppIcon::RecycleBinEmpty),
            fluent_glyph(AppIcon::RecycleBinFull)
        );
        assert_ne!(
            mdl2_glyph(AppIcon::RecycleBinEmpty),
            mdl2_glyph(AppIcon::RecycleBinFull)
        );
        assert_eq!(mdl2_glyph(AppIcon::RecycleBinFull), "\u{E922}");
        assert_eq!(
            fluent_glyph(AppIcon::Trash2),
            fluent_glyph(AppIcon::RecycleBinEmpty)
        );
    }

    #[test]
    fn mdl2_fallback_list_is_the_nine_win10_codepoints() {
        assert_eq!(MDL2_FALLBACKS.len(), 9);
        let expected = [
            "\u{E70D}", "\u{E70E}", "\u{E710}", "\u{E711}", "\u{E73E}", "\u{E8BB}", "\u{E921}",
            "\u{E922}", "\u{E923}",
        ];
        for glyph in expected {
            assert!(
                MDL2_FALLBACKS.iter().any(|(_, g)| *g == glyph),
                "missing MDL2 fallback U+{:04X}",
                glyph.chars().next().unwrap() as u32
            );
            assert_eq!(glyph.chars().count(), 1);
        }
        assert_eq!(mdl2_named("chevronup"), Some("\u{E70D}"));
        assert_eq!(mdl2_named("Unknown"), None);
        // Every icon still resolves to a single private-use glyph on either font.
        for icon in [
            AppIcon::Search,
            AppIcon::RecycleBinEmpty,
            AppIcon::RecycleBinFull,
            AppIcon::Plus,
            AppIcon::Close,
        ] {
            assert_eq!(mdl2_glyph(icon).chars().count(), 1);
            assert_eq!(glyph_for_icon(icon).chars().count(), 1);
        }
    }

    #[test]
    fn all_ninety_three_lucide_paths_parse_cleanly() {
        let names: Vec<_> = lucide_names().collect();
        assert_eq!(names.len(), 93, "expected 93 ported Lucide paths");
        assert_eq!(
            names.iter().filter(|name| name.starts_with("Icon")).count(),
            91
        );
        for name in names {
            let path = lucide(&name).unwrap_or_else(|| panic!("missing Lucide path {name}"));
            assert!(path.len() > 8, "empty Lucide path {name}");
            let (_, ops, consumed) = crate::design::brand_paths::parse_stream_geometry(path);
            assert_eq!(consumed, path.len(), "parser stopped early for {name}");
            assert!(!ops.is_empty(), "no draw operations for {name}");
        }
        assert!(lucide("IconWifi").unwrap().contains("14.08"));
        assert!(lucide("IconPlayFill").unwrap().starts_with('M'));
        assert!(lucide("IconNavDock").unwrap().contains("M5 3H19"));
        assert_eq!(lucide("NoSuchIcon"), None);
    }
}
