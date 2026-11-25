//! Native Windows 11 `Segoe Fluent Icons` catalog shared across Dock, Context Menus,
//! Flyouts, and GPUI Settings so every surface uses Windows's own hinted vector icons.

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
        AppIcon::RecycleBinEmpty | AppIcon::RecycleBinFull | AppIcon::Trash2 => "\u{E74D}",
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

/// Draws a crisp, ClearType-hinted `Segoe Fluent Icons` glyph centered at `(cx, cy)` on a Win32 GDI `HDC`.
pub unsafe fn draw_fluent_icon_gdi(
    hdc: HDC,
    icon: AppIcon,
    cx: i32,
    cy: i32,
    size_px: i32,
    fg_colorref: u32,
) {
    let sz = size_px.max(10);
    let font_name: Vec<u16> = "Segoe Fluent Icons\0".encode_utf16().collect();
    let text = fluent_glyph(icon);
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
    use super::{AppIcon, fluent_glyph};

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
}
