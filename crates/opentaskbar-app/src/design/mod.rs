//! Single source of truth for OpenTaskBar's Slate Neutral + Aurora design tokens,
//! ported 1-to-1 from `Design/Palettes/Dark.xaml` and `Design/Palettes/Light.xaml`.
//!
//! Shared across DirectComposition (`dock/dcomp.rs`), Win32 flyouts/menus (`ui/flyout.rs`,
//! `ui/menu.rs`), and GPUI (`ui/components.rs`, `ui/settings.rs`) so every surface speaks
//! one cohesive visual language.

pub mod brand_paths;
pub mod brush;
pub mod d2d;
pub mod fonts;
pub mod icons;
pub mod radius;
pub mod spatial_glass;
pub mod theme;
pub mod tokens;
pub mod widget_pack;
pub mod windows_palette;

/// Windows 11 Acrylic transient-window backdrop (`DWMWA_SYSTEMBACKDROP_TYPE`).
pub const DWMSBT_TRANSIENTWINDOW: u32 = 3;

/// RGB color tuple `(r, g, b)`.
pub type Rgb = (u8, u8, u8);

/// Unified semantic color palette for both Dark and Light themes (neutral Zinc/Carbon + Aurora).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub canvas: Rgb,
    pub sidebar: Rgb,
    pub surface: Rgb,
    pub elevated: Rgb,
    pub hover: Rgb,
    pub border_rgb: Rgb,
    pub specular_top: Rgb,
    pub sep_rgb: Rgb,
    pub dock_pill: Rgb,
    pub text_primary: Rgb,
    pub text_secondary: Rgb,
    pub accent: Rgb,
    /// Aurora teal (`WidgetAccent` in reference): `#2DD4BF` dark / `#0D9488` light.
    pub widget_accent: Rgb,
    /// Soft sky blue (`WidgetBlue` in reference): `#A2C0FF` dark / `#1D66D7` light.
    pub widget_blue: Rgb,
    /// Selected pill fill blended on popup surface (`WidgetSelectedFill` in reference).
    pub selected_fill: Rgb,
    /// Selected pill border (`WidgetSelectedStroke` in reference).
    pub selected_stroke: Rgb,
    /// Selected pill text/icon (`WidgetSelectedForeground` in reference).
    pub selected_fg: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
    pub danger_hover_bg: Rgb,
    pub danger_fg: Rgb,
    pub switch_off: Rgb,
}

/// Unified neutral Carbon/Zinc + Aurora dark palette across Dock, Flyouts, Menus, and Settings.
pub const DARK: Palette = Palette {
    canvas: (0x11, 0x11, 0x14),          // Neutral Zinc canvas #111114
    sidebar: (0x0d, 0x0d, 0x10),         // Unified sidebar #0D0D10
    surface: (0x18, 0x18, 0x1c),         // Popup / Card surface #18181C
    elevated: (0x22, 0x22, 0x27),        // Elevated control / tile #222227
    hover: (0x2c, 0x2c, 0x33),           // Hover surface #2C2C33
    border_rgb: (0x2e, 0x2e, 0x36),      // Subtle neutral hairline #2E2E36
    specular_top: (0x46, 0x46, 0x52),    // Top specular edge highlight
    sep_rgb: (0x26, 0x26, 0x2e),         // Hairline divider #26262E
    dock_pill: (0x16, 0x16, 0x1a),       // Dock glass pill base #16161A
    text_primary: (0xf4, 0xf4, 0xf5),    // Zinc-100 #F4F4F5
    text_secondary: (0xa1, 0xa1, 0xaa),  // Zinc-400 #A1A1AA
    accent: (0x38, 0xbd, 0xf8),          // Aurora Sky #38BDF8
    widget_accent: (0x2d, 0xd4, 0xbf),   // Aurora Teal #2DD4BF
    widget_blue: (0xa2, 0xc0, 0xff),     // WidgetBlue #A2C0FF
    selected_fill: (0x23, 0x33, 0x4d),   // Selected pill fill
    selected_stroke: (0x52, 0x6f, 0x9e), // Selected pill border
    selected_fg: (0xa2, 0xc0, 0xff),     // Selected pill foreground
    success: (0x58, 0xd8, 0xb2),         // WidgetGreen #58D8B2
    warning: (0xfb, 0xbf, 0x24),         // WidgetWarning #FBBF24
    danger: (0xef, 0x44, 0x44),          // Danger #EF4444
    danger_hover_bg: (0x3a, 0x1f, 0x24), // Soft translucent red hover
    danger_fg: (0xf8, 0x71, 0x71),       // Danger text #F87171
    switch_off: (0x34, 0x34, 0x3d),
};

/// Unified neutral Zinc + Aurora light palette across Dock, Flyouts, Menus, and Settings.
pub const LIGHT: Palette = Palette {
    canvas: (0xf4, 0xf4, 0xf5),          // Zinc-100 #F4F4F5
    sidebar: (0xec, 0xec, 0xef),         // Neutral sidebar #ECECEF
    surface: (0xff, 0xff, 0xff),         // Pure white surface #FFFFFF
    elevated: (0xf2, 0xf2, 0xf4),        // Subtle elevated control #F2F2F4
    hover: (0xe8, 0xe8, 0xed),           // Neutral hover #E8E8ED
    border_rgb: (0xe2, 0xe2, 0xe7),      // Crisp hairline #E2E2E7
    specular_top: (0xff, 0xff, 0xff),    // Top specular edge highlight
    sep_rgb: (0xe8, 0xe8, 0xec),         // Divider #E8E8EC
    dock_pill: (0xf8, 0xf8, 0xfa),       // Dock glass pill base #F8F8FA
    text_primary: (0x18, 0x18, 0x1b),    // Zinc-900 #18181B
    text_secondary: (0x61, 0x66, 0x6e),  // Neutral secondary #61666E
    accent: (0x03, 0x69, 0xa1),          // Accent #0369A1
    widget_accent: (0x0d, 0x94, 0x88),   // Aurora Teal #0D9488
    widget_blue: (0x1d, 0x66, 0xd7),     // WidgetBlue #1D66D7
    selected_fill: (0xde, 0xea, 0xfc),   // Selected fill
    selected_stroke: (0x92, 0xb7, 0xf4), // Selected stroke
    selected_fg: (0x1d, 0x66, 0xd7),     // Selected fg
    success: (0x0d, 0x9e, 0x78),         // Green #0D9E78
    warning: (0xd9, 0x77, 0x06),         // Warning #D97706
    danger: (0xdc, 0x26, 0x26),          // Danger #DC2626
    danger_hover_bg: (0xfb, 0xe6, 0xe8), // Soft danger hover
    danger_fg: (0xdc, 0x26, 0x26),       // Danger fg #DC2626
    switch_off: (0xd4, 0xd4, 0xd8),
};

pub const fn palette(is_dark: bool) -> Palette {
    if is_dark { DARK } else { LIGHT }
}

/// Parses `#RGB` / `#ARGB` / `#RRGGBB` / `#AARRGGBB` or a named colour into the
/// RGB tuple used by DComp/GDI surfaces. Alpha is intentionally discarded here.
pub fn parse_hex_rgb(value: &str) -> Option<Rgb> {
    let color = tokens::Rgba::from_hex(value)?;
    Some((color.r, color.g, color.b))
}

pub fn palette_with_border(is_dark: bool, border: Option<Rgb>) -> Palette {
    let mut p = palette(is_dark);
    if let Some(rgb) = border {
        p.border_rgb = rgb;
    }
    p
}

fn blend_over(fg: tokens::Rgba, bg: Rgb) -> Rgb {
    if fg.a == 255 {
        return (fg.r, fg.g, fg.b);
    }
    let a = f32::from(fg.a) / 255.0;
    let inv = 1.0 - a;
    let mix = |f: u8, b: u8| -> u8 {
        (f32::from(f) * a + f32::from(b) * inv)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (mix(fg.r, bg.0), mix(fg.g, bg.1), mix(fg.b, bg.2))
}

/// Resolves the live palette for `is_dark`, overlaying the active Windows accent
/// and any loaded theme pack / manual editor token overrides from `ThemeManager`.
pub fn active_palette(is_dark: bool) -> Palette {
    let mut p = palette(is_dark);

    let windows_rgb = |key: &str| -> Option<Rgb> {
        let value = theme::resolve_windows_token(key)?;
        let rgba = theme::token_rgba(&value)?;
        (rgba.a > 0).then_some((rgba.r, rgba.g, rgba.b))
    };
    if let Some(accent) = windows_rgb("AccentBrush") {
        p.accent = accent;
    }
    if let Some(widget_accent) = windows_rgb("WidgetAccent") {
        p.widget_accent = widget_accent;
    }

    if !theme::has_active_theme_customization() {
        return p;
    }

    let custom_rgba = |key: &str| -> Option<tokens::Rgba> {
        let value = theme::resolve_custom_token(key)?;
        let rgba = theme::token_rgba(&value)?;
        (rgba.a > 0).then_some(rgba)
    };
    let custom_rgb = |keys: &[&str]| -> Option<Rgb> {
        keys.iter()
            .copied()
            .find_map(custom_rgba)
            .map(|c| (c.r, c.g, c.b))
    };
    let custom_surface = |keys: &[&str], bg: Rgb| -> Option<Rgb> {
        keys.iter()
            .copied()
            .find_map(custom_rgba)
            .map(|c| blend_over(c, bg))
    };

    if let Some(c) = custom_surface(&["SettingsBackground"], p.canvas) {
        p.canvas = c;
    }
    if let Some(c) = custom_surface(&["SettingsSidebarOverlay"], p.canvas) {
        p.sidebar = c;
    } else if custom_rgba("SettingsBackground").is_some() {
        let darkened = tokens::Rgba::from_rgb(p.canvas.0, p.canvas.1, p.canvas.2).darken(0.14);
        p.sidebar = (darkened.r, darkened.g, darkened.b);
    }
    if let Some(c) = custom_surface(
        &["SurfacePopup", "MenuMaterial", "SettingsControl"],
        p.canvas,
    ) {
        p.surface = c;
    }
    if let Some(c) = custom_surface(&["SettingsControl"], p.surface) {
        p.elevated = c;
    }
    if let Some(c) = custom_surface(&["SettingsControlHover", "SurfaceHover"], p.surface) {
        p.hover = c;
    }
    if let Some(c) = custom_surface(&["DockMaterial"], p.canvas) {
        p.dock_pill = c;
    }
    if let Some(c) = custom_rgb(&["DockBorderMaterial", "StrokeSubtle", "WidgetCardStroke"]) {
        p.border_rgb = c;
    }
    if let Some(c) = custom_rgb(&["DockHighlight"]) {
        p.specular_top = c;
    }
    if let Some(c) = custom_rgb(&["MenuDivider", "StrokeSubtle"]) {
        p.sep_rgb = c;
    }
    if let Some(c) = custom_rgb(&["TextPrimary"]) {
        p.text_primary = c;
    }
    if let Some(c) = custom_rgb(&["TextSecondary"]) {
        p.text_secondary = c;
    }
    if let Some(c) = custom_rgb(&["AccentBrush"]) {
        p.accent = c;
    }
    if let Some(c) = custom_rgb(&["WidgetAccent"]) {
        p.widget_accent = c;
    }
    if let Some(c) = custom_rgb(&["WidgetBlue", "WidgetAccent"]) {
        p.widget_blue = c;
    }
    if let Some(c) = custom_surface(&["WidgetSelectedFill", "SurfaceSelected"], p.surface) {
        p.selected_fill = c;
    }
    if let Some(c) = custom_rgb(&["WidgetSelectedStroke", "StrokeFocus"]) {
        p.selected_stroke = c;
    }
    if let Some(c) = custom_rgb(&["WidgetSelectedForeground", "AccentBrush"]) {
        p.selected_fg = c;
    }
    if let Some(c) = custom_rgb(&["WidgetWarning"]) {
        p.warning = c;
    }
    if let Some(c) = custom_rgb(&["WidgetFocusAccent", "SurfaceDanger"]) {
        p.danger = c;
    }
    p
}

/// Resolves the live palette for `is_dark` and applies an optional explicit border override.
pub fn active_palette_with_border(is_dark: bool, border: Option<Rgb>) -> Palette {
    let mut p = active_palette(is_dark);
    if let Some(rgb) = border {
        p.border_rgb = rgb;
    }
    p
}

/// Converts `(r, g, b)` to Win32 GDI `COLORREF` (`0x00BBGGRR`).
pub const fn gdi_rgb(c: Rgb) -> u32 {
    (c.0 as u32) | ((c.1 as u32) << 8) | ((c.2 as u32) << 16)
}

/// Converts `(r, g, b)` + alpha `a` to GPUI `rgba` (`0xRRGGBBAA`).
pub const fn gpui_rgba(c: Rgb, a: u8) -> u32 {
    ((c.0 as u32) << 24) | ((c.1 as u32) << 16) | ((c.2 as u32) << 8) | (a as u32)
}

/// `SystemStatsWidget.LoadBrushKey` threshold (`<75%` accent, `75-89%` amber, `>=90%` red).
pub fn meter_color(p: &Palette, ratio: f32) -> Rgb {
    if ratio >= 0.90 {
        p.danger
    } else if ratio >= 0.75 {
        p.warning
    } else {
        p.widget_accent
    }
}

/// Semantic threshold color for remaining-level bars like Battery (`<=15%` red, `<=30%` amber, `>30%` green).
pub fn level_color(p: &Palette, ratio: f32) -> Rgb {
    if ratio <= 0.15 {
        p.danger
    } else if ratio <= 0.30 {
        p.warning
    } else {
        p.success
    }
}

/// Computes dock widget chip height automatically from the live dock pill height (`WidgetHeight = 46`).
pub fn widget_height_dips(pill_h_dips: f32) -> f32 {
    (pill_h_dips - 10.0).clamp(32.0, 56.0)
}

#[cfg(test)]
mod tests {
    use super::parse_hex_rgb;

    #[test]
    fn surface_colors_accept_short_hex_and_named_colors() {
        assert_eq!(parse_hex_rgb("#38BDF8"), Some((0x38, 0xBD, 0xF8)));
        assert_eq!(parse_hex_rgb("#0af"), Some((0, 0xAA, 0xFF)));
        assert_eq!(parse_hex_rgb("#80FF0000"), Some((255, 0, 0)));
        assert_eq!(parse_hex_rgb("Transparent"), Some((0, 0, 0)));
        assert_eq!(parse_hex_rgb("not-a-color"), None);
    }
}
