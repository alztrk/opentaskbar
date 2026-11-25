//! Single source of truth for OpenTaskBar's Slate Neutral + Aurora design tokens,
//! ported 1-to-1 from C# `Design/Palettes/Dark.xaml` and `Design/Palettes/Light.xaml`.
//!
//! Shared across DirectComposition (`dock/dcomp.rs`), Win32 flyouts/menus (`ui/flyout.rs`,
//! `ui/menu.rs`), and GPUI (`ui/components.rs`, `ui/settings.rs`) so every surface speaks
//! one cohesive visual language.

pub mod brand_paths;
pub mod icons;

/// RGB color tuple `(r, g, b)`.
pub type Rgb = (u8, u8, u8);

/// Unified semantic color palette for both Dark and Light themes (C# parity).
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub canvas: Rgb,
    pub sidebar: Rgb,
    pub surface: Rgb,
    pub elevated: Rgb,
    pub hover: Rgb,
    pub border_rgb: Rgb,
    pub sep_rgb: Rgb,
    pub dock_pill: Rgb,
    pub text_primary: Rgb,
    pub text_secondary: Rgb,
    pub accent: Rgb,
    /// Aurora teal (`WidgetAccent` in C#): `#2DD4BF` dark / `#0D9488` light.
    pub widget_accent: Rgb,
    /// Soft sky blue (`WidgetBlue` in C#): `#A2C0FF` dark / `#1D66D7` light.
    pub widget_blue: Rgb,
    /// Selected pill fill blended on popup surface (`WidgetSelectedFill` in C#).
    pub selected_fill: Rgb,
    /// Selected pill border (`WidgetSelectedStroke` in C#).
    pub selected_stroke: Rgb,
    /// Selected pill text/icon (`WidgetSelectedForeground` in C#).
    pub selected_fg: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
    pub danger_hover_bg: Rgb,
    pub danger_fg: Rgb,
    pub switch_off: Rgb,
}

/// Exact C# `Design/Palettes/Dark.xaml` Slate Neutral + Aurora palette.
pub const DARK: Palette = Palette {
    canvas: (0x1b, 0x20, 0x29),          // SettingsBackground #1B2029
    sidebar: (0x17, 0x1c, 0x26),         // SettingsSidebarOverlay #171C26
    surface: (0x25, 0x2b, 0x37),         // SurfacePopup / MenuMaterial #252B37
    elevated: (0x2e, 0x35, 0x43),        // SurfaceSelected (#14FFFFFF over #252B37)
    hover: (0x38, 0x40, 0x50),           // SurfaceHover (#18FFFFFF over #252B37)
    border_rgb: (0x3a, 0x43, 0x52),      // StrokeSubtle #3A4352
    sep_rgb: (0x32, 0x3a, 0x48),         // MenuDivider #323A48
    dock_pill: (0x22, 0x28, 0x33),       // DockMaterial midpoint (#2B313E -> #1B2029)
    text_primary: (0xf3, 0xf4, 0xf6),    // TextPrimary #F3F4F6
    text_secondary: (0xaa, 0xb2, 0xc0),  // TextSecondary #AAB2C0
    accent: (0x38, 0xbd, 0xf8),          // AccentBrush #38BDF8
    widget_accent: (0x2d, 0xd4, 0xbf),   // WidgetAccent #2DD4BF
    widget_blue: (0xa2, 0xc0, 0xff),     // WidgetBlue #A2C0FF
    selected_fill: (0x2f, 0x40, 0x60),   // WidgetSelectedFill (#335B92FF over #252B37)
    selected_stroke: (0x5d, 0x76, 0xa2), // WidgetSelectedStroke (#7891B8FF over #252B37)
    selected_fg: (0xa2, 0xc0, 0xff),     // WidgetSelectedForeground #A2C0FF
    success: (0x58, 0xd8, 0xb2),         // WidgetGreen #58D8B2
    warning: (0xfb, 0xbf, 0x24),         // WidgetWarning #FBBF24
    danger: (0xef, 0x44, 0x44),          // WidgetFocusAccent #EF4444
    danger_hover_bg: (0x3e, 0x2c, 0x39), // WidgetDangerHover (#22DF4B53 over #252B37)
    danger_fg: (0xf3, 0x6c, 0x74),       // WidgetDangerForeground #F36C74
    switch_off: (0x3a, 0x43, 0x52),
};

/// Exact C# `Design/Palettes/Light.xaml` Slate Neutral + Aurora palette.
pub const LIGHT: Palette = Palette {
    canvas: (0xf3, 0xf3, 0xf3),          // SettingsBackground #F3F3F3
    sidebar: (0xee, 0xee, 0xee),         // SettingsSidebarOverlay #EEEEEE
    surface: (0xff, 0xff, 0xff),         // SurfacePopup #FFFFFF
    elevated: (0xf2, 0xf2, 0xf4),        // SurfaceSelected (#0D000000 over #FFFFFF)
    hover: (0xe8, 0xec, 0xf2),           // SurfaceHover (#12162038 over #FFFFFF)
    border_rgb: (0xe5, 0xe5, 0xe5),      // StrokeSubtle #E5E5E5
    sep_rgb: (0xe8, 0xe8, 0xe8),         // MenuDivider #E8E8E8
    dock_pill: (0xf7, 0xf7, 0xf8),       // DockMaterial midpoint
    text_primary: (0x1b, 0x1b, 0x1b),    // TextPrimary #1B1B1B
    text_secondary: (0x61, 0x66, 0x6e),  // TextSecondary #61666E
    accent: (0x03, 0x69, 0xa1),          // AccentBrush #0369A1
    widget_accent: (0x0d, 0x94, 0x88),   // WidgetAccent #0D9488
    widget_blue: (0x1d, 0x66, 0xd7),     // WidgetBlue #1D66D7
    selected_fill: (0xde, 0xea, 0xfc),   // WidgetSelectedFill (#262474E9 over #FFFFFF)
    selected_stroke: (0x92, 0xb7, 0xf4), // WidgetSelectedStroke
    selected_fg: (0x1d, 0x66, 0xd7),     // WidgetSelectedForeground #1D66D7
    success: (0x0d, 0x9e, 0x78),         // WidgetGreen #0D9E78
    warning: (0xd9, 0x77, 0x06),         // WidgetWarning #D97706
    danger: (0xdc, 0x26, 0x26),          // WidgetFocusAccent #DC2626
    danger_hover_bg: (0xfb, 0xe6, 0xe8), // WidgetDangerHover
    danger_fg: (0xdc, 0x26, 0x26),       // WidgetDangerForeground #DC2626
    switch_off: (0xd1, 0xd1, 0xd6),
};

pub const fn palette(is_dark: bool) -> Palette {
    if is_dark { DARK } else { LIGHT }
}

/// Converts `(r, g, b)` to Win32 GDI `COLORREF` (`0x00BBGGRR`).
pub const fn gdi_rgb(c: Rgb) -> u32 {
    (c.0 as u32) | ((c.1 as u32) << 8) | ((c.2 as u32) << 16)
}

/// Converts `(r, g, b)` + alpha `a` to GPUI `rgba` (`0xRRGGBBAA`).
pub const fn gpui_rgba(c: Rgb, a: u8) -> u32 {
    ((c.0 as u32) << 24) | ((c.1 as u32) << 16) | ((c.2 as u32) << 8) | (a as u32)
}

/// C# `SystemStatsWidget.LoadBrushKey` threshold (`<75%` accent, `75-89%` amber, `>=90%` red).
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

/// Computes dock widget chip height automatically from the live dock pill height (C# `WidgetHeight = 46`).
pub fn widget_height_dips(pill_h_dips: f32) -> f32 {
    (pill_h_dips - 10.0).clamp(32.0, 56.0)
}
