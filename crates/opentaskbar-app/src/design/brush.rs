//! Brush-picker model for the theme editor (`ThemePropertyEditor/ThemeBrushPicker`).
//!
//! Kept free of GPUI/Win32 so the colour math can be unit-tested on any host.
//! The settings UI owns the widgets; this module owns the values they edit:
//! an HSV field + hue/alpha sliders, a 16-stop gradient, an angle and a hex box.

use super::tokens::{
    Gradient, GradientStop, Rgba, TokenValue, gradient_endpoints, picker_to_theme_angle,
    theme_to_picker_angle,
};

/// Maximum number of gradient stops exposed by the picker (`GradientStopCount`).
pub use super::tokens::MAX_GRADIENT_STOPS;

/// Hue / saturation / value, with `h` in degrees and `s`, `v` in `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsv {
    pub h: f64,
    pub s: f64,
    pub v: f64,
}

impl Hsv {
    pub fn new(h: f64, s: f64, v: f64) -> Self {
        Self {
            h: h.rem_euclid(360.0),
            s: s.clamp(0.0, 1.0),
            v: v.clamp(0.0, 1.0),
        }
    }

    /// HSV -> RGB (the standard sector algorithm), preserving `alpha`.
    pub fn to_rgba(self, alpha: u8) -> Rgba {
        let h = self.h.rem_euclid(360.0) / 60.0;
        let sector = h.floor() as i32;
        let f = h - h.floor();
        let p = self.v * (1.0 - self.s);
        let q = self.v * (1.0 - f * self.s);
        let t = self.v * (1.0 - (1.0 - f) * self.s);
        let byte = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        let (r, g, b) = match sector.rem_euclid(6) {
            0 => (self.v, t, p),
            1 => (q, self.v, p),
            2 => (p, self.v, t),
            3 => (p, q, self.v),
            4 => (t, p, self.v),
            _ => (self.v, p, q),
        };
        Rgba::new(byte(r), byte(g), byte(b), alpha)
    }
}

impl From<Rgba> for Hsv {
    fn from(color: Rgba) -> Self {
        let r = f64::from(color.r) / 255.0;
        let g = f64::from(color.g) / 255.0;
        let b = f64::from(color.b) / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let h = if delta == 0.0 {
            0.0
        } else if max == r {
            60.0 * (((g - b) / delta).rem_euclid(6.0))
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        Self {
            h: h.rem_euclid(360.0),
            s: if max == 0.0 { 0.0 } else { delta / max },
            v: max,
        }
    }
}

/// Editable gradient: up to 16 evenly distributed stops plus direction and alpha.
#[derive(Clone, Debug, PartialEq)]
pub struct GradientSpec {
    pub stops: Vec<Rgba>,
    /// Clockwise angle in degrees (0 = left-to-right, 90 = top-to-bottom).
    pub angle_deg: f64,
}

impl GradientSpec {
    pub fn new(stops: Vec<Rgba>, angle_deg: f64) -> Self {
        Self {
            stops: stops.into_iter().take(MAX_GRADIENT_STOPS).collect(),
            angle_deg: angle_deg.rem_euclid(360.0),
        }
    }

    /// Pads a two-stop picker gradient to the full 16-stop resolution so tweak
    /// handles are available for every stop, matching the reference picker.
    pub fn from_endpoints(start: Rgba, end: Rgba, angle_deg: f64) -> Self {
        let stops = (0..MAX_GRADIENT_STOPS)
            .map(|i| {
                let t = i as f64 / (MAX_GRADIENT_STOPS - 1) as f64;
                start.mix(end, t)
            })
            .collect();
        Self::new(stops, angle_deg)
    }

    /// Converts the picker angle into endpoints using the single shared helper.
    pub fn vector(&self) -> ((f64, f64), (f64, f64)) {
        gradient_endpoints(self.to_theme_angle())
    }

    /// Picker angle (`0 = horizontal`, `90 = vertical`) → reference theme angle.
    pub fn to_theme_angle(&self) -> f64 {
        picker_to_theme_angle(self.angle_deg)
    }

    /// reference theme angle → picker angle; use when persisting a picker brush as `pack.json`.
    pub fn from_theme_angle(angle_deg: f64) -> f64 {
        theme_to_picker_angle(angle_deg)
    }

    pub fn to_gradient(&self) -> Gradient {
        let count = self.stops.len().max(1);
        let (start, end) = self.vector();
        Gradient {
            stops: self
                .stops
                .iter()
                .enumerate()
                .map(|(i, color)| GradientStop {
                    color: *color,
                    position: i as f64 / (count - 1).max(1) as f64,
                })
                .collect(),
            start,
            end,
        }
    }
}

/// Value edited by the brush picker: a solid colour or an editable gradient.
#[derive(Clone, Debug, PartialEq)]
pub enum BrushSpec {
    Solid(Rgba),
    Gradient(GradientSpec),
}

impl BrushSpec {
    pub fn to_token_value(&self) -> TokenValue {
        match self {
            Self::Solid(color) => TokenValue::Solid(*color),
            Self::Gradient(spec) => TokenValue::Gradient(spec.to_gradient()),
        }
    }

    /// Applies the alpha slider uniformly across all stops of a gradient.
    pub fn with_alpha(self, alpha: u8) -> Self {
        match self {
            Self::Solid(c) => Self::Solid(c.with_alpha(alpha)),
            Self::Gradient(mut spec) => {
                spec.stops = spec
                    .stops
                    .into_iter()
                    .map(|c| c.with_alpha(alpha))
                    .collect();
                Self::Gradient(spec)
            }
        }
    }
}

/// Parses `#RRGGBB` / `#AARRGGBB` with the same rules as the token parser.
pub fn parse_hex(text: &str) -> Option<Rgba> {
    Rgba::from_hex(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_primary_and_neutral_colors() {
        for color in [
            Rgba::from_rgb(255, 0, 0),
            Rgba::from_rgb(0, 255, 0),
            Rgba::from_rgb(0, 0, 255),
            Rgba::from_rgb(24, 28, 36),
            Rgba::from_rgb(255, 255, 255),
        ] {
            let hsv = Hsv::from(color);
            assert_eq!(hsv.to_rgba(255), color, "hsv={hsv:?}");
        }
    }

    #[test]
    fn hue_wraps_and_alpha_slider_is_preserved() {
        assert!((Hsv::new(-30.0, 1.0, 1.0).h - 330.0).abs() < 1e-9);
        let color = Hsv::new(200.0, 0.5, 0.8).to_rgba(0x80);
        assert_eq!(color.a, 0x80);
        assert_eq!(Hsv::from(color).h.round() as i32, 200);
    }

    #[test]
    fn gradient_picker_builds_sixteen_normalized_stops() {
        let spec = GradientSpec::from_endpoints(
            Rgba::from_rgb(0, 0, 0),
            Rgba::from_rgb(255, 255, 255),
            90.0,
        );
        assert_eq!(spec.stops.len(), MAX_GRADIENT_STOPS);
        let gradient = spec.to_gradient();
        assert_eq!(gradient.stops.len(), MAX_GRADIENT_STOPS);
        assert_eq!(gradient.stops[0].position, 0.0);
        assert_eq!(gradient.stops.last().unwrap().position, 1.0);
        // 8/15 of 255 rounds to 136 with the token palette's integer mixer.
        assert!((120..=140).contains(&gradient.stops[8].color.r));
        // 90 degrees -> vertical gradient (cos ~ 0, sin = 1).
        assert!((gradient.start.0 - gradient.end.0).abs() < 1e-9);
        assert!(gradient.start.1 < gradient.end.1);
    }

    #[test]
    fn brush_spec_converts_to_tokens_and_applies_alpha() {
        let spec = BrushSpec::Gradient(GradientSpec::from_endpoints(
            Rgba::from_rgb(10, 20, 30),
            Rgba::from_rgb(40, 50, 60),
            0.0,
        ));
        let TokenValue::Gradient(g) = spec.with_alpha(0x40).to_token_value() else {
            panic!("expected gradient token")
        };
        assert!(g.stops.iter().all(|s| s.color.a == 0x40));
        assert_eq!(
            BrushSpec::Solid(Rgba::from_rgb(1, 2, 3))
                .to_token_value()
                .kind(),
            super::super::tokens::TokenKind::Brush
        );
        assert_eq!(parse_hex("#38BDF8"), Some(Rgba::from_rgb(0x38, 0xBD, 0xF8)));
    }
}
