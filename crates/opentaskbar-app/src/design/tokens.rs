//! Typed design token values shared by the Windows accent palette, theme packs and
//! the widget-pack resolver. Mirrors the UI types (`Brush`, `Color`, `CornerRadius`,
//! `Thickness`, `double`, `FontFamily`) used by `WidgetPack.Convert`.

use std::collections::BTreeMap;

/// RGBA color; `a` is 255 for opaque colors. Byte order matches `Color.FromArgb`.
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

    /// Parses CSS colour literals: `#RGB`, `#ARGB`, `#RRGGBB`, `#AARRGGBB`
    /// (case-insensitive, leading `#` optional) or a named colour such as `Transparent`.
    pub fn from_hex(text: &str) -> Option<Self> {
        let value = text.trim().trim_start_matches('#');
        // All slicing happens on the byte buffer: a multi-byte character can never
        // be split at a non-char boundary (which would panic with `&value[..]`).
        // Non-ASCII bytes simply fail to parse and yield `None`.
        let bytes = value.as_bytes();
        let pair = |i: usize| -> Option<u8> {
            let digits = std::str::from_utf8(bytes.get(i..i + 2)?).ok()?;
            u8::from_str_radix(digits, 16).ok()
        };
        let nibble = |i: usize| -> Option<u8> {
            let digit = (bytes.get(i).copied()? as char).to_digit(16)? as u8;
            Some(digit * 17)
        };
        match bytes.len() {
            3 => Some(Self::from_rgb(nibble(0)?, nibble(1)?, nibble(2)?)),
            4 => Some(Self::new(nibble(1)?, nibble(2)?, nibble(3)?, nibble(0)?)),
            6 => Some(Self::from_rgb(pair(0)?, pair(2)?, pair(4)?)),
            8 => Some(Self::new(pair(2)?, pair(4)?, pair(6)?, pair(0)?)),
            _ => named_color(text.trim()),
        }
    }

    /// Win32 `COLORREF` / `Color` packed as `0xAARRGGBB`.
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

/// Clamps an opacity into `0.0..=1.0`; non-finite values mean "no surface".
pub fn clamp_opacity(opacity: f64) -> f64 {
    if !opacity.is_finite() {
        return 0.0;
    }
    opacity.clamp(0.0, 1.0)
}

/// Converts an opacity to an 8-bit alpha byte (one rule shared by glass, theme
/// materials and the dock renderer).
pub fn alpha_from_opacity(opacity: f64) -> u8 {
    (clamp_opacity(opacity) * 255.0).round() as u8
}

/// Named colours accepted by `ColorConverter` (the subset used by reference themes).
pub fn named_color(text: &str) -> Option<Rgba> {
    let rgb = |r, g, b| Rgba::from_rgb(r, g, b);
    let value = match text.trim().to_ascii_lowercase().as_str() {
        "transparent" => Rgba::new(0, 0, 0, 0),
        "black" => rgb(0, 0, 0),
        "white" => rgb(255, 255, 255),
        "red" => rgb(255, 0, 0),
        "green" => rgb(0, 128, 0),
        "lime" => rgb(0, 255, 0),
        "blue" => rgb(0, 0, 255),
        "yellow" => rgb(255, 255, 0),
        "cyan" | "aqua" => rgb(0, 255, 255),
        "magenta" | "fuchsia" => rgb(255, 0, 255),
        "gray" | "grey" => rgb(128, 128, 128),
        "darkgray" => rgb(169, 169, 169),
        "lightgray" => rgb(211, 211, 211),
        "slategray" => rgb(112, 128, 144),
        "silver" => rgb(192, 192, 192),
        "orange" => rgb(255, 165, 0),
        "purple" => rgb(128, 0, 128),
        "pink" => rgb(255, 192, 203),
        "brown" => rgb(165, 42, 42),
        "teal" => rgb(0, 128, 128),
        "navy" => rgb(0, 0, 128),
        "maroon" => rgb(128, 0, 0),
        "olive" => rgb(128, 128, 0),
        "indigo" => rgb(75, 0, 130),
        "gold" => rgb(255, 215, 0),
        _ => return None,
    };
    Some(value)
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

    /// UI-style `left,top,right,bottom` / `left,top` parsing (values are DIPs or px).
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

/// Shared cap for parsed/authored gradients (brush specs and theme packs).
pub const MAX_GRADIENT_STOPS: usize = 16;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gradient {
    pub stops: Vec<GradientStop>,
    pub start: (f64, f64),
    pub end: (f64, f64),
}

impl Gradient {
    /// Two-stop vertical gradient, the reference default for `WidgetAccentFill` / materials.
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

    /// Gradient from stops and a reference/`WidgetPack.Convert` theme angle.
    pub fn angled(stops: Vec<GradientStop>, angle_deg: f64) -> Self {
        let (start, end) = gradient_endpoints(angle_deg);
        Self { stops, start, end }
    }
}

/// The one source of truth for gradient angle → endpoints conversion.
///
/// Theme angle follows `WidgetPack.ParseBrush` / `ThemePropertyEditor.BrushToNode`:
/// `0° = bottom→top`, `90° = left→right`, `180° = top→bottom`.
pub fn gradient_endpoints(angle_deg: f64) -> ((f64, f64), (f64, f64)) {
    let radians = angle_deg.rem_euclid(360.0).to_radians();
    let (sin, cos) = radians.sin_cos();
    (
        (0.5 - sin / 2.0, 0.5 + cos / 2.0),
        (0.5 + sin / 2.0, 0.5 - cos / 2.0),
    )
}

/// GPUI brush picker (`0° = left→right`, `90° = top→bottom`) → theme angle.
pub fn picker_to_theme_angle(angle_deg: f64) -> f64 {
    (angle_deg + 90.0).rem_euclid(360.0)
}

/// Theme angle → GPUI brush picker angle.
pub fn theme_to_picker_angle(angle_deg: f64) -> f64 {
    (angle_deg - 90.0).rem_euclid(360.0)
}

/// `RadialGradientBrush` (used by `IconHoverGlow` / `WidgetIsland`) data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RadialGradient {
    pub stops: Vec<GradientStop>,
    /// Center of the outer circle, in relative 0..1 coordinates.
    pub center: (f64, f64),
    /// Focus point of the gradient, in relative 0..1 coordinates.
    pub origin: (f64, f64),
    pub radius: (f64, f64),
}

impl RadialGradient {
    pub fn new(
        stops: Vec<GradientStop>,
        center: (f64, f64),
        origin: (f64, f64),
        radius: (f64, f64),
    ) -> Self {
        Self {
            stops,
            center,
            origin,
            radius,
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

    pub fn with_alpha(self, alpha: u8) -> Self {
        Self {
            stops: self
                .stops
                .into_iter()
                .map(|stop| GradientStop {
                    color: stop.color.with_alpha(alpha),
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
    RadialGradient(RadialGradient),
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
            Self::Solid(_) | Self::Gradient(_) | Self::RadialGradient(_) => TokenKind::Brush,
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
            Self::RadialGradient(g) => Self::RadialGradient(g.scaled_alpha(scale)),
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
            Self::RadialGradient(g) => Self::RadialGradient(g.with_alpha(alpha)),
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
    /// resource tree (Rust has no `Application.Current.Resources[key]`).
    pub fn for_key(key: &str) -> Self {
        if let Some(kind) = editor_token_kind(key) {
            return kind;
        }
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

/// A key the reference theme editor (`Views/ThemePropertyEditor.cs`) or a bundled theme pack
/// can legitimately override. Unknown keys must be rejected, not silently accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorToken {
    pub key: &'static str,
    pub kind: TokenKind,
}

/// Type registry: the 51 editor keys plus the advanced/pack keys reference ships in its
/// palettes and `pack.json` manifests. This is what replaces the reference UI's
/// `Application.Current.Resources[key]` type lookup on the Rust side.
pub static EDITOR_TOKENS: &[EditorToken] = &[
    // ThemePropertyEditor sections (51 keys).
    EditorToken {
        key: "TextPrimary",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "TextSecondary",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "AccentBrush",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "AccentText",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "StrokeSubtle",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "StrokeFocus",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SurfaceSelected",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SurfaceHover",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SurfacePressed",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockBackgroundMode",
        kind: TokenKind::Mode,
    },
    EditorToken {
        key: "DockMaterial",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockBorderMaterial",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockHighlight",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "DockInnerRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "DockWidgetPadding",
        kind: TokenKind::Thickness,
    },
    EditorToken {
        key: "DockShadowBlur",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "DockShadowOpacity",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "DockShadowColor",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetCard",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetCardHover",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetCardPressed",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetCardStroke",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetTrack",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetAccent",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockWidgetSurface",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockWidgetRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "MenuMaterial",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "MenuDivider",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SurfacePopup",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "MenuLabelWidth",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "PopupRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "PopupInnerRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "SettingsBackground",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SettingsBackgroundOpacity",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "SettingsControl",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SettingsControlHover",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SettingsSidebarOverlay",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "IconForeground",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WindowsLogoBrush",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "IconHoverGlow",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "StartTileBackground",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "StartTileStroke",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "StartTileHover",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetWarning",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetFocusAccent",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "InterfaceFont",
        kind: TokenKind::FontFamily,
    },
    EditorToken {
        key: "TextSizeBody",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "TextSizeCaption",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "ControlRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "SettingsPageInset",
        kind: TokenKind::Thickness,
    },
    // Advanced editor tokens and keys shipped by the bundled theme packs.
    EditorToken {
        key: "AccentTrack",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "CaptionCloseForeground",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DangerText",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockInnerBevelMaterial",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockScrollScrimStart",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockScrollScrimEnd",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockScrollScrimTop",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockScrollScrimBottom",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "DockSheenRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "DockWidgetActionRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "IndicatorBrush",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "IndicatorForegroundBrush",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "MenuStroke",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "NotificationBrush",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SettingsTrack",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "ShellButtonHover",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "ShellButtonPressed",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "StartTileBorder",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "StartTilePressed",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SurfaceDanger",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SurfaceKeyboardFocus",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "SurfaceHoverColor",
        kind: TokenKind::Color,
    },
    EditorToken {
        key: "SurfaceSelectedStrong",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "TextMuted",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "TextSizeWidgetLabel",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "TextSizeWidgetValue",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "WidgetAccentFill",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetAccentText",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetActionFont",
        kind: TokenKind::FontFamily,
    },
    EditorToken {
        key: "WidgetBlue",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetDangerForeground",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetDangerHover",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetDangerStroke",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetFieldRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "WidgetGlassCard",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetGlassCardHover",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetGlassCardStroke",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetGreen",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetIsland",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetIslandStroke",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetOnStroke",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetQuickActionRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "WidgetRadius",
        kind: TokenKind::CornerRadius,
    },
    EditorToken {
        key: "WidgetSelectedFill",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetSelectedForeground",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "WidgetSelectedStroke",
        kind: TokenKind::Brush,
    },
    EditorToken {
        key: "MenuRowPadding",
        kind: TokenKind::Thickness,
    },
    EditorToken {
        key: "MenuDividerInset",
        kind: TokenKind::Thickness,
    },
    // Per-surface opacity keys persisted by the settings UI.
    EditorToken {
        key: "SurfaceOpacity",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "DockOpacity",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "MenuOpacity",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "WidgetOpacity",
        kind: TokenKind::Double,
    },
    EditorToken {
        key: "SettingsOpacity",
        kind: TokenKind::Double,
    },
];

/// Exact (case-insensitive) type lookup for an editable theme token.
pub fn editor_token_kind(key: &str) -> Option<TokenKind> {
    EDITOR_TOKENS
        .iter()
        .find(|token| token.key.eq_ignore_ascii_case(key))
        .map(|token| token.kind)
}

/// Number of registered editor tokens (51 reference editor keys + advanced/pack keys).
pub const fn editor_token_count() -> usize {
    EDITOR_TOKENS.len()
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
        assert_eq!(Rgba::from_hex("#fff"), Some(Rgba::from_rgb(255, 255, 255)));
        assert_eq!(Rgba::from_hex("#0f0"), Some(Rgba::from_rgb(0, 255, 0)));
        assert_eq!(Rgba::from_hex("#8f00"), Some(Rgba::new(255, 0, 0, 0x88)));
        assert_eq!(Rgba::from_hex("Transparent"), Some(Rgba::new(0, 0, 0, 0)));
        assert_eq!(
            Rgba::from_hex("  slategray "),
            Some(Rgba::from_rgb(112, 128, 144))
        );
        assert_eq!(Rgba::from_hex("#12"), None);
    }

    #[test]
    fn unicode_and_non_ascii_hex_input_returns_none_without_panicking() {
        // Regression: `from_hex` used byte-length dispatch but sliced `&str` at
        // byte offsets, panicking on multi-byte input inside the UI thread.
        assert_eq!(Rgba::from_hex("aééa"), None); // 6 bytes, split mid-character.
        assert_eq!(Rgba::from_hex("éééé"), None); // 8 bytes, non-ASCII nibbles.
        assert_eq!(Rgba::from_hex("#éé"), None);
        assert_eq!(Rgba::from_hex(" café "), None);
        assert_eq!(Rgba::from_hex("😀😀"), None);
        // Valid input is unaffected.
        assert_eq!(
            Rgba::from_hex("#A1B2C3"),
            Some(Rgba::new(0xA1, 0xB2, 0xC3, 0xFF))
        );
    }

    #[test]
    fn opacity_helpers_share_one_non_finite_rule() {
        assert_eq!(clamp_opacity(f64::NAN), 0.0);
        assert_eq!(clamp_opacity(f64::INFINITY), 0.0); // non-finite = no surface
        assert_eq!(clamp_opacity(-2.0), 0.0);
        assert_eq!(alpha_from_opacity(f64::NAN), 0);
        assert_eq!(alpha_from_opacity(0.5), 128);
        assert_eq!(Rgba::from_rgb(1, 2, 3).scaled_alpha(f64::NAN).a, 0);
    }

    #[test]
    fn editor_token_registry_covers_the_reference_editor_and_pack_keys() {
        assert!(editor_token_count() >= 45);
        assert_eq!(
            editor_token_kind("DockBackgroundMode"),
            Some(TokenKind::Mode)
        );
        assert_eq!(
            editor_token_kind("Dockradius"),
            Some(TokenKind::CornerRadius)
        );
        assert_eq!(
            editor_token_kind("SettingsPageInset"),
            Some(TokenKind::Thickness)
        );
        assert_eq!(editor_token_kind("TextSizeBody"), Some(TokenKind::Double));
        assert_eq!(
            editor_token_kind("InterfaceFont"),
            Some(TokenKind::FontFamily)
        );
        assert_eq!(editor_token_kind("WidgetAccent"), Some(TokenKind::Brush));
        assert_eq!(editor_token_kind("NoSuchToken"), None);
    }

    #[test]
    fn gradient_angle_convention_has_one_source() {
        // reference: 0 = bottom→top, 90 = left→right, 180 = top→bottom.
        let (start, end) = gradient_endpoints(0.0);
        assert!((start.1 - 1.0).abs() < 1e-9 && (end.1 - 0.0).abs() < 1e-9);
        let (start, end) = gradient_endpoints(90.0);
        assert!((start.0 - 0.0).abs() < 1e-9 && (end.0 - 1.0).abs() < 1e-9);
        let (start, end) = gradient_endpoints(180.0);
        assert!((start.1 - 0.0).abs() < 1e-9 && (end.1 - 1.0).abs() < 1e-9);
        assert_eq!(picker_to_theme_angle(90.0), 180.0);
        assert_eq!(theme_to_picker_angle(180.0), 90.0);
        let g = Gradient::angled(
            vec![
                GradientStop {
                    color: Rgba::from_rgb(0, 0, 0),
                    position: 0.0,
                },
                GradientStop {
                    color: Rgba::from_rgb(255, 255, 255),
                    position: 1.0,
                },
            ],
            180.0,
        );
        assert!((g.start.0 - 0.5).abs() < 1e-9 && (g.start.1 - 0.0).abs() < 1e-9);
        assert!((g.end.0 - 0.5).abs() < 1e-9 && (g.end.1 - 1.0).abs() < 1e-9);
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
