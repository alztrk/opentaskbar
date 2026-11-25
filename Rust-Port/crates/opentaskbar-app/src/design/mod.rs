//! Single source of truth for OpenTaskBar's unified Neutral Zinc/Carbon design tokens.
//!
//! Shared across DirectComposition (`dock/dcomp.rs`), Win32 GDI surfaces (`ui/flyout.rs`,
//! `ui/menu.rs`), and GPUI (`ui/components.rs`, `ui/settings.rs`) so every surface speaks
//! one cohesive visual language.

pub mod icons;

/// RGB color tuple `(r, g, b)`.
pub type Rgb = (u8, u8, u8);

/// Unified semantic color palette for both Dark and Light themes.
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
    pub success: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
    pub danger_hover_bg: Rgb,
    pub danger_fg: Rgb,
    pub switch_off: Rgb,
}

pub const DARK: Palette = Palette {
    canvas: (0x0c, 0x0c, 0x0e),
    sidebar: (0x14, 0x14, 0x16),
    surface: (0x1a, 0x1a, 0x1e),
    elevated: (0x22, 0x22, 0x27),
    hover: (0x2c, 0x2c, 0x32),
    border_rgb: (0x2a, 0x2a, 0x30),
    sep_rgb: (0x26, 0x26, 0x2c),
    dock_pill: (0x14, 0x14, 0x17),
    text_primary: (0xf4, 0xf4, 0xf5),
    text_secondary: (0xa1, 0xa1, 0xaa),
    accent: (0x0a, 0x84, 0xff),
    success: (0x22, 0xc5, 0x5e),
    warning: (0xf5, 0x9e, 0x0b),
    danger: (0xef, 0x44, 0x44),
    // Subtle 16% red blended onto dark surface #1a1a1e
    danger_hover_bg: (0x3c, 0x21, 0x24),
    danger_fg: (0xfc, 0xa5, 0xa5),
    switch_off: (0x36, 0x36, 0x3c),
};

pub const LIGHT: Palette = Palette {
    canvas: (0xf5, 0xf5, 0xf7),
    sidebar: (0xee, 0xee, 0xf0),
    surface: (0xff, 0xff, 0xff),
    elevated: (0xf2, 0xf2, 0xf5),
    hover: (0xe6, 0xe6, 0xeb),
    border_rgb: (0xd8, 0xd8, 0xdf),
    sep_rgb: (0xe4, 0xe4, 0xea),
    dock_pill: (0xf4, 0xf4, 0xf6),
    text_primary: (0x18, 0x18, 0x1b),
    text_secondary: (0x60, 0x60, 0x69),
    accent: (0x00, 0x6e, 0xdb),
    success: (0x16, 0xa3, 0x4a),
    warning: (0xd9, 0x77, 0x06),
    danger: (0xdc, 0x26, 0x26),
    // Subtle 12% red blended onto white surface
    danger_hover_bg: (0xfb, 0xe5, 0xe5),
    danger_fg: (0xb9, 0x1c, 0x1c),
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

/// Semantic threshold color for utilization bars (`<70%` accent, `70-89%` amber, `>=90%` red).
pub fn meter_color(p: &Palette, ratio: f32) -> Rgb {
    if ratio >= 0.90 {
        p.danger
    } else if ratio >= 0.70 {
        p.warning
    } else {
        p.accent
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

/// Computes dock widget chip height automatically from the live dock pill height.
pub fn widget_height_dips(pill_h_dips: f32) -> f32 {
    (pill_h_dips - 12.0).clamp(30.0, 56.0)
}
