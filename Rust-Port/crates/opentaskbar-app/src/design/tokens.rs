//! Typed design token values shared by the Windows accent palette, theme packs and
//! the widget-pack resolver. Mirrors the WPF types (`Brush`, `Color`, `CornerRadius`,
//! `Thickness`, `double`, `FontFamily`) used by C# `WidgetPack.Convert`.

#![allow(dead_code)] // Public C5/C4 API surface; consumed by other workstreams after merge.

use std::collections::BTreeMap;

/// RGBA color; `a` is 255 for opaque colors. Byte order matches WPF `Color.FromArgb`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, 255)
    }

    /// Parses `#RRGGBB` or `#AARRGGBB` (case-insensitive, leading `#` optional).
    pub fn from_hex(text: &str) -> Option<Self> {
        let value = text.trim().trim_start_matches('#');
        let parse = |s: &str| u8::from_str_radix(s, 16).ok();
        match value.len() {
            6 => Some(Self::from_rgb(
                parse(&value[0..2])?,
                parse(&value[2..4])?,
                parse(&value[4..6])?,
            )),
            8 => Some(Self::new(
                parse(&value[2..4])?,
                parse(&value[4..6])?,
                parse(&value[6..8])?,
                parse(&value[0..2])?,
            )),
            _ => None,
        }
    }

    /// Win32 `COLORREF` / WPF `Color` packed as `0xAARRGGBB`.
    pub const fn to_argb(self) -> u32 {
        ((self.a as u32) << 24) | ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    /// GPUI's `0xRRGGBBAA` representation.
    pub const fn to_gpui(self) -> u32 {
        ((self.r as u32) << 24) | ((self.g as u32) << 16) | ((self.b as u32) << 8) | self.a as u32
    }

    pub fn with_alpha(self, alpha: u8) -> Self {
        Self { a: alpha, ..self }
    }

    pub fn scaled_alpha(self, scale: f64) -> Self {
        let a = ((f64::from(self.a) * scale).round() as i64).clamp(0, 255) as u8;
        self.with_alpha(a)
    }

    pub fn hex(self) -> String {
        if self.a == 255 {
            format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            format!("#{:02X}{:02X}{:02X}{:02X}", self.a, self.r, self.g, self.b)
        }
    }

    /// Relative luminance (WCAG 2.0): used for `ContrastForeground`.
    pub fn luminance(self) -> f64 {
        fn channel(value: u8) -> f64 {
            let srgb = f64::from(value) / 255.0;
            if srgb <= 0.04045 {
                srgb / 12.92
            } else {
                ((srgb + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    pub fn contrast_foreground(self) -> Self {
        if self.luminance() < 0.18 {
            Self::from_rgb(255, 255, 255)
        } else {
            Self::from_rgb(5, 7, 11)
        }
    }

    pub fn mix(self, other: Self, amount: f64) -> Self {
        let t = amount.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| {
            ((f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as i64).clamp(0, 255) as u8
        };
        Self::from_rgb(
            mix(self.r, other.r),
            mix(self.g, other.g),
            mix(self.b, other.b),
        )
    }

    pub fn lighten(self, amount: f64) -> Self {
        self.mix(Self::from_rgb(255, 255, 255), amount)
    }

    pub fn darken(self, amount: f64) -> Self {
        self.mix(Self::from_rgb(0, 0, 0), amount)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CornerRadius {
    pub top_left: i32,
    pub top_right: i32,
    pub bottom_right: i32,
    pub bottom_left: i32,
}

impl CornerRadius {
    pub const fn uniform(value: i32) -> Self {
        Self {
            top_left: value,
            top_right: value,
            bottom_right: value,
            bottom_left: value,
        }
    }

    /// WPF-style `left,top,right,bottom` / `left,top` parsing (values are DIPs or px).
    pub fn parse(text: &str) -> Option<Self> {
        let parts: Vec<f64> = text
            .split(',')
            .map(|part| part.trim().parse::<f64>().ok())
            .collect::<Option<Vec<_>>>()?;
        let round = |v: f64| -> Option<i32> {
            (v.is_finite() && (0.0..=2048.0).contains(&v)).then(|| v.round() as i32)
        };
        match parts.as_slice() {
            [a] => Some(Self::uniform(round(*a)?)),
            [a, b] => {
                let (a, b) = (round(*a)?, round(*b)?);
                Some(Self {
                    top_left: a,
                    top_right: b,
                    bottom_right: a,
                    bottom_left: b,
                })
            }
            [a, b, c, d] => Some(Self {
                top_left: round(*a)?,
                top_right: round(*b)?,
                bottom_right: round(*c)?,
                bottom_left: round(*d)?,
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Thickness {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Thickness {
    pub const fn uniform(value: i32) -> Self {
        Self {
            left: value,
            top: value,
            right: value,
            bottom: value,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let radius = CornerRadius::parse(text)?;
        Some(Self {
            left: radius.top_left,
            top: radius.top_right,
            right: radius.bottom_right,
            bottom: radius.bottom_left,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GradientStop {
    pub color: Rgba,
    /// 0.0 ..= 1.0.
    pub position: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gradient {
    pub stops: Vec<GradientStop>,
    pub start: (f64, f64),
    pub end: (f64, f64),
}

impl Gradient {
    /// Two-stop vertical gradient, the C# default for `WidgetAccentFill` / materials.
    pub fn vertical(start: Rgba, end: Rgba) -> Self {
        Self {
            stops: vec![
                GradientStop {
                    color: start,
                    position: 0.0,
                },
                GradientStop {
                    color: end,
                    position: 1.0,
                },
            ],
            start: (0.0, 0.0),
            end: (0.0, 1.0),
        }
    }

    pub fn scaled_alpha(self, scale: f64) -> Self {
        Self {
            stops: self
                .stops
                .into_iter()
                .map(|stop| GradientStop {
                    color: stop.color.scaled_alpha(scale),
                    ..stop
                })
                .collect(),
            ..self
        }
    }
}

/// Value of a resolved theme token.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenValue {
    Solid(Rgba),
    Gradient(Gradient),
    Color(Rgba),
    CornerRadius(CornerRadius),
    Thickness(Thickness),
    Double(f64),
    FontFamily(String),
    Mode(String),
}

impl Default for TokenValue {
    fn default() -> Self {
        Self::Solid(Rgba::new(255, 255, 255, 0))
    }
}

impl TokenValue {
    pub fn kind(&self) -> TokenKind {
        match self {
            Self::Solid(_) | Self::Gradient(_) => TokenKind::Brush,
            Self::Color(_) => TokenKind::Color,
            Self::CornerRadius(_) => TokenKind::CornerRadius,
            Self::Thickness(_) => TokenKind::Thickness,
            Self::Double(_) => TokenKind::Double,
            Self::FontFamily(_) => TokenKind::FontFamily,
            Self::Mode(_) => TokenKind::Mode,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self.kind() {
            TokenKind::Brush => "Brush",
            TokenKind::Color => "Color",
            TokenKind::CornerRadius => "CornerRadius",
            TokenKind::Thickness => "Thickness",
            TokenKind::Double => "Double",
            TokenKind::FontFamily => "FontFamily",
            TokenKind::Mode => "Mode",
        }
    }

    pub fn scaled_alpha(self, scale: f64) -> Self {
        match self {
            Self::Solid(c) => Self::Solid(c.scaled_alpha(scale)),
            Self::Gradient(g) => Self::Gradient(g.scaled_alpha(scale)),
            other => other,
        }
    }

    pub fn with_alpha(self, alpha: u8) -> Self {
        match self {
            Self::Solid(c) => Self::Solid(c.with_alpha(alpha)),
            Self::Gradient(g) => Self::Gradient(Gradient {
                stops: g
                    .stops
                    .into_iter()
                    .map(|s| GradientStop {
                        color: s.color.with_alpha(alpha),
                        ..s
                    })
                    .collect(),
                ..g
            }),
            other => other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Brush,
    Color,
    CornerRadius,
    Thickness,
    Double,
    FontFamily,
    Mode,
}

impl TokenKind {
    /// Best-effort key-based inference used when a theme pack is loaded without a
    /// WPF resource tree (Rust has no `Application.Current.Resources[key]`).
    pub fn for_key(key: &str) -> Self {
        let lower = key.to_ascii_lowercase();
        if lower.contains("font") || lower.contains("family") {
            TokenKind::FontFamily
        } else if lower.contains("mode") || lower == "dockbackgroundmode" {
            TokenKind::Mode
        } else if lower.contains("radius") {
            TokenKind::CornerRadius
        } else if lower.contains("thickness") || lower.contains("padding") {
            TokenKind::Thickness
        } else if lower.contains("opacity")
            || lower.contains("blur")
            || lower.contains("scale")
            || lower.contains("width")
            || lower.contains("height")
            || lower.contains("size")
            || lower.contains("duration")
            || lower.contains("spacing")
        {
            TokenKind::Double
        } else if lower == "textprimary"
            || lower == "textsecondary"
            || lower == "textmuted"
            || lower == "surfacehovercolor"
            || lower.ends_with("color")
        {
            TokenKind::Color
        } else {
            TokenKind::Brush
        }
    }
}

pub type TokenMap = BTreeMap<String, TokenValue>;

/// Resolves a token from a layered map: base palette, then Windows accent, then theme pack.
pub fn resolve_layered<'a>(layers: &[&'a TokenMap], key: &str) -> Option<&'a TokenValue> {
    layers.iter().rev().find_map(|layer| layer.get(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_argb_and_rrggbb_hex() {
        assert_eq!(
            Rgba::from_hex("#FF112233"),
            Some(Rgba::new(0x11, 0x22, 0x33, 0xFF))
        );
        assert_eq!(
            Rgba::from_hex("80AABBCC"),
            Some(Rgba::new(0xAA, 0xBB, 0xCC, 0x80))
        );
        assert_eq!(Rgba::from_hex("#fff"), None);
    }

    #[test]
    fn wcag_luminance_picks_foreground() {
        assert_eq!(
            Rgba::from_rgb(0, 0, 0).contrast_foreground(),
            Rgba::from_rgb(255, 255, 255)
        );
        assert_eq!(
            Rgba::from_rgb(255, 255, 255).contrast_foreground(),
            Rgba::from_rgb(5, 7, 11)
        );
    }

    #[test]
    fn corner_radius_forms() {
        assert_eq!(
            CornerRadius::parse("17,17,0,0"),
            Some(CornerRadius {
                top_left: 17,
                top_right: 17,
                bottom_right: 0,
                bottom_left: 0
            })
        );
        assert_eq!(
            CornerRadius::parse("8,4"),
            Some(CornerRadius {
                top_left: 8,
                top_right: 4,
                bottom_right: 8,
                bottom_left: 4
            })
        );
        assert_eq!(CornerRadius::parse("6"), Some(CornerRadius::uniform(6)));
    }

    #[test]
    fn layered_lookup_orders_packs_above_windows() {
        let mut base = TokenMap::new();
        base.insert(
            "AccentBrush".into(),
            TokenValue::Solid(Rgba::from_rgb(1, 2, 3)),
        );
        let mut pack = TokenMap::new();
        pack.insert(
            "AccentBrush".into(),
            TokenValue::Solid(Rgba::from_rgb(9, 9, 9)),
        );
        let value = resolve_layered(&[&base, &pack], "AccentBrush");
        assert_eq!(value, Some(&TokenValue::Solid(Rgba::from_rgb(9, 9, 9))));
    }
}
