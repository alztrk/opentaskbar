//! reference parity for `Services/App/WidgetPack.cs`: typed design-token overrides from
//! `widgets/pack.json`. `Tokens` apply to both themes; `Dark` / `Light` layer on top.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use opentaskbar_core::settings::AppTheme;
use serde_json::{Map, Value};

use super::tokens::{
    CornerRadius, Gradient, GradientStop, MAX_GRADIENT_STOPS, RadialGradient, Rgba, Thickness,
    TokenKind, TokenMap, TokenValue, editor_token_kind, gradient_endpoints,
};

pub const MANIFEST_FILE_NAME: &str = "pack.json";
const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
const MAX_TOKENS: usize = 256;
pub const MAX_DIMENSION: f64 = 2048.0;
const MAX_FONT_FAMILY: usize = 128;

#[derive(Clone, Debug, Default)]
pub struct WidgetPackManifest {
    pub name: Option<String>,
    pub tokens: BTreeMap<String, Value>,
    pub dark: BTreeMap<String, Value>,
    pub light: BTreeMap<String, Value>,
}

impl PartialEq for WidgetPackManifest {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.tokens == other.tokens
            && self.dark == other.dark
            && self.light == other.light
    }
}

impl WidgetPackManifest {
    /// Manual (de)serialization avoids a direct `serde` dependency in this crate; the
    /// workspace only exposes `serde_json`, which is enough for manifest parsing.
    pub fn from_str(text: &str) -> Result<Self, String> {
        let value = parse_json_lenient(text)?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let Some(object) = value.as_object() else {
            return Err("widget pack manifest must be an object".into());
        };
        let map = |key: &str| -> BTreeMap<String, Value> {
            object
                .get(key)
                .and_then(Value::as_object)
                .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        };
        Ok(Self {
            name: object
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            tokens: map("tokens"),
            dark: map("dark"),
            light: map("light"),
        })
    }

    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        if let Some(name) = &self.name {
            object.insert("name".into(), Value::String(name.clone()));
        }
        for (key, values) in [
            ("tokens", &self.tokens),
            ("dark", &self.dark),
            ("light", &self.light),
        ] {
            object.insert(
                key.to_owned(),
                Value::Object(values.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
            );
        }
        Value::Object(object)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WidgetTokenResolution {
    pub tokens: TokenMap,
    pub problems: Vec<String>,
}

impl WidgetPackManifest {
    /// Loads a widget pack from a directory. Errors mention the offending path.
    pub fn load(directory: &Path) -> Result<Self, String> {
        let path = directory.join(MANIFEST_FILE_NAME);
        let meta = fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "{} exceeds the {MAX_MANIFEST_BYTES}-byte limit.",
                path.display()
            ));
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Merged raw layers for a concrete theme (system resolved by the caller).
    pub fn raw_for(&self, theme: AppTheme) -> BTreeMap<&str, &Value> {
        let mut raw: BTreeMap<&str, &Value> = BTreeMap::new();
        for (key, value) in &self.tokens {
            raw.insert(key, value);
        }
        let layer = if theme == AppTheme::Dark {
            &self.dark
        } else {
            &self.light
        };
        for (key, value) in layer {
            raw.insert(key, value);
        }
        raw
    }
}

/// Parses reference-style JSON: `//` and `/* … */` comments and trailing commas are allowed (matches
/// `JsonSerializerOptions { ReadCommentHandling.Skip, AllowTrailingCommas = true }`).
pub fn parse_json_lenient(text: &str) -> Result<Value, String> {
    // One string/comment scanner; the second pass only handles trailing commas.
    fn strip_comments(text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut cleaned = String::with_capacity(text.len());
        let (mut i, mut in_string, mut escaped) = (0usize, false, false);
        while i < chars.len() {
            let c = chars[i];
            if in_string {
                cleaned.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            if c == '"' {
                in_string = true;
                cleaned.push(c);
                i += 1;
                continue;
            }
            if c == '/' && chars.get(i + 1) == Some(&'/') {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            if c == '/' && chars.get(i + 1) == Some(&'*') {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(chars.len());
                continue;
            }
            cleaned.push(c);
            i += 1;
        }
        cleaned
    }

    let cleaned = strip_comments(text);
    // Drop commas directly followed by a closing brace/bracket.
    let chars: Vec<char> = cleaned.chars().collect();
    let mut result = String::with_capacity(cleaned.len());
    let (mut i, mut in_string, mut escaped) = (0usize, false, false);
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            result.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            result.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            let mut look = i + 1;
            while look < chars.len() && chars[look].is_whitespace() {
                look += 1;
            }
            if matches!(chars.get(look), Some('}') | Some(']')) {
                i += 1;
                continue;
            }
        }
        result.push(c);
        i += 1;
    }
    serde_json::from_str(&result).map_err(|e| e.to_string())
}

/// C5 API: resolves typed tokens for a theme. `current` reports a token's existing type so
/// replacements are interpreted like the reference parser does; unknown tokens and bad values go to `problems`.
pub fn resolve_widget_tokens<F>(
    manifest: &WidgetPackManifest,
    theme: AppTheme,
    current: F,
) -> WidgetTokenResolution
where
    F: Fn(&str) -> Option<TokenKind>,
{
    let mut result = WidgetTokenResolution::default();
    let raw = manifest.raw_for(theme);
    if raw.len() > MAX_TOKENS {
        result
            .problems
            .push("Widget theme contains too many token overrides.".into());
        return result;
    }
    for (key, value) in raw {
        // `WidgetPack.Resolve` rejects keys the current resource tree does not know.
        // Rust's registry replaces `Application.Current.Resources[key]`; a heuristic
        // guess must never turn an unknown key into a live override.
        let Some(kind) = current(key).or_else(|| editor_token_kind(key)) else {
            result.problems.push(format!("Unknown token '{key}'."));
            continue;
        };
        match convert(value, kind) {
            Some(token) => {
                result.tokens.insert(key.to_owned(), token);
            }
            None => result.problems.push(format!(
                "Token '{key}' expects a {} value.",
                kind_name(kind)
            )),
        }
    }
    result
}

/// Alias matching `ThemePacks.ApplyWidgets` naming.
pub fn resolve(
    manifest: &WidgetPackManifest,
    theme: AppTheme,
    current: impl Fn(&str) -> Option<TokenKind>,
) -> WidgetTokenResolution {
    resolve_widget_tokens(manifest, theme, current)
}

pub fn kind_name(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Brush => "Brush",
        TokenKind::Color => "Color",
        TokenKind::CornerRadius => "CornerRadius",
        TokenKind::Thickness => "Thickness",
        TokenKind::Double => "double",
        TokenKind::FontFamily => "FontFamily",
        TokenKind::Mode => "string",
    }
}

/// Converts a raw JSON value into the requested typed token.
pub fn convert(value: &Value, kind: TokenKind) -> Option<TokenValue> {
    match kind {
        TokenKind::Brush => parse_brush(value),
        TokenKind::Color => match value {
            Value::String(text) => Rgba::from_hex(text).map(TokenValue::Color),
            _ => None,
        },
        TokenKind::CornerRadius => parse_numbers(value).map(|n| {
            TokenValue::CornerRadius(CornerRadius {
                top_left: n[0] as i32,
                top_right: n[1] as i32,
                bottom_right: n[2] as i32,
                bottom_left: n[3] as i32,
            })
        }),
        TokenKind::Thickness => parse_numbers(value).map(|n| {
            TokenValue::Thickness(Thickness {
                left: n[0] as i32,
                top: n[1] as i32,
                right: n[2] as i32,
                bottom: n[3] as i32,
            })
        }),
        TokenKind::Double => match value {
            Value::Number(n) => {
                let n = n.as_f64()?;
                (n.is_finite() && (0.0..=MAX_DIMENSION).contains(&n))
                    .then_some(TokenValue::Double(n))
            }
            _ => None,
        },
        TokenKind::FontFamily => match value {
            Value::String(text) if !text.trim().is_empty() && text.len() <= MAX_FONT_FAMILY => {
                Some(TokenValue::FontFamily(text.clone()))
            }
            _ => None,
        },
        TokenKind::Mode => match value {
            Value::String(mode) if matches!(mode.as_str(), "Glass" | "Solid" | "None") => {
                Some(TokenValue::Mode(mode.clone()))
            }
            _ => None,
        },
    }
}

/// Brushes accept a named/`#RGB`… colour, a linear gradient object, or a
/// `{"type":"radial",…}` object (`RadialGradientBrush` such as `IconHoverGlow`).
pub fn parse_brush(value: &Value) -> Option<TokenValue> {
    if let Value::String(text) = value {
        return Rgba::from_hex(text).map(TokenValue::Solid);
    }
    let Value::Object(object) = value else {
        return None;
    };
    let Value::Array(raw_stops) = object.get("stops")? else {
        return None;
    };
    if raw_stops.len() < 2 || raw_stops.len() > MAX_GRADIENT_STOPS {
        return None;
    }
    let mut colors = Vec::with_capacity(raw_stops.len());
    for stop in raw_stops {
        let Value::String(text) = stop else {
            return None;
        };
        colors.push(Rgba::from_hex(text)?);
    }

    let positions = if let Some(Value::Array(positions)) = object.get("positions") {
        if positions.len() != colors.len() {
            return None;
        }
        let mut result = Vec::with_capacity(positions.len());
        for position in positions {
            let number = position.as_f64()?;
            if !number.is_finite() || !(0.0..=100.0).contains(&number) {
                return None;
            }
            result.push(number / 100.0);
        }
        result
    } else {
        (0..colors.len())
            .map(|i| i as f64 / (colors.len() - 1) as f64)
            .collect()
    };
    let stops: Vec<GradientStop> = colors
        .into_iter()
        .zip(positions)
        .map(|(color, position)| GradientStop { color, position })
        .collect();

    let kind = object
        .get("type")
        .or_else(|| object.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("linear")
        .to_ascii_lowercase();
    if kind == "radial" {
        let point = |key: &str, fallback: (f64, f64)| -> Option<(f64, f64)> {
            let Some(Value::Array(values)) = object.get(key) else {
                return Some(fallback);
            };
            let x = values.first()?.as_f64()?;
            let y = values.get(1)?.as_f64()?;
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            Some((x, y))
        };
        let center = point("center", (0.5, 0.5))?;
        let origin = point("origin", center)?;
        let radius = point("radius", (0.5, 0.5))?;
        if [center.0, center.1, origin.0, origin.1, radius.0, radius.1]
            .iter()
            .any(|v| !(0.0..=2.0).contains(v))
        {
            return None;
        }
        return Some(TokenValue::RadialGradient(RadialGradient::new(
            stops, center, origin, radius,
        )));
    }
    if kind != "linear" {
        return None;
    }

    let direction = object
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("vertical")
        .to_ascii_lowercase();
    let (start, end) = if let Some(angle) = object.get("angle").and_then(Value::as_f64) {
        if !angle.is_finite() || !(0.0..=360.0).contains(&angle) {
            return None;
        }
        gradient_endpoints(angle)
    } else {
        match direction.as_str() {
            "horizontal" => ((0.0, 0.5), (1.0, 0.5)),
            "diagonal" => ((0.0, 0.0), (1.0, 1.0)),
            _ => ((0.5, 0.0), (0.5, 1.0)),
        }
    };
    Some(TokenValue::Gradient(Gradient { stops, start, end }))
}

/// One number for all four sides, two (`v,h`), or four (`left,top,right,bottom`).
pub fn parse_numbers(value: &Value) -> Option<[f64; 4]> {
    let parts: Vec<f64> = match value {
        Value::Number(n) => {
            let n = n.as_f64()?;
            vec![n, n, n, n]
        }
        Value::String(text) => text
            .split(',')
            .map(|part| part.trim().parse::<f64>().ok())
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    if !matches!(parts.len(), 1 | 2 | 4)
        || parts
            .iter()
            .any(|n| !n.is_finite() || !(0.0..=MAX_DIMENSION).contains(n))
    {
        return None;
    }
    Some(match parts.as_slice() {
        [a] => [*a, *a, *a, *a],
        [a, b] => [*a, *b, *a, *b],
        [a, b, c, d] => [*a, *b, *c, *d],
        _ => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_layers_override_common_tokens() {
        let manifest = WidgetPackManifest::from_str(
            r##"{
                "tokens": {"WidgetRadius": 12, "WidgetAccent": "#FF112233"},
                "dark": {"WidgetRadius": 14, "TextPrimary": "#FFFFFFFF"},
                "light": {"WidgetRadius": 8}
            }"##,
        )
        .unwrap();
        let dark = resolve_widget_tokens(&manifest, AppTheme::Dark, |_| None);
        assert!(dark.problems.is_empty());
        assert_eq!(
            dark.tokens.get("WidgetRadius"),
            Some(&TokenValue::CornerRadius(CornerRadius::uniform(14)))
        );
        assert_eq!(
            dark.tokens.get("WidgetAccent"),
            Some(&TokenValue::Solid(Rgba::from_rgb(0x11, 0x22, 0x33)))
        );
        let light = resolve_widget_tokens(&manifest, AppTheme::Light, |_| None);
        assert_eq!(
            light.tokens.get("WidgetRadius"),
            Some(&TokenValue::CornerRadius(CornerRadius::uniform(8)))
        );
        assert!(!light.tokens.contains_key("TextPrimary"));
    }

    #[test]
    fn gradient_stops_positions_and_angle() {
        let value: Value = serde_json::from_str(
            r##"{"stops":["#FF000000","#FFFFFFFF","#80808080"],"positions":[0,50,100],"direction":"diagonal"}"##,
        )
        .unwrap();
        let TokenValue::Gradient(gradient) = convert(&value, TokenKind::Brush).unwrap() else {
            panic!("expected gradient")
        };
        assert_eq!(gradient.stops.len(), 3);
        assert_eq!(gradient.stops[1].position, 0.5);
        assert_eq!(gradient.end, (1.0, 1.0));

        let angled: Value =
            serde_json::from_str(r##"{"stops":["#FF000000","#FFFFFFFF"],"angle":90}"##).unwrap();
        let TokenValue::Gradient(gradient) = convert(&angled, TokenKind::Brush).unwrap() else {
            panic!("expected gradient")
        };
        assert!((gradient.start.0 - 0.0).abs() < 1e-9 && (gradient.start.1 - 0.5).abs() < 1e-9);
        assert!((gradient.end.0 - 1.0).abs() < 1e-9 && (gradient.end.1 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn bounds_and_problems_are_reported() {
        let manifest = WidgetPackManifest::from_str(
            r#"{"tokens":{"WidgetRadius":3000,"UnknownToken":5,"DockBackgroundMode":"Glass","InterfaceFont":"Inter"}}"#,
        )
        .unwrap();
        let result = resolve_widget_tokens(&manifest, AppTheme::Dark, |key| {
            if key == "UnknownToken" {
                None
            } else {
                Some(TokenKind::for_key(key))
            }
        });
        assert_eq!(result.problems.len(), 2);
        assert_eq!(
            result.tokens.get("DockBackgroundMode"),
            Some(&TokenValue::Mode("Glass".into()))
        );
        assert_eq!(
            result.tokens.get("InterfaceFont"),
            Some(&TokenValue::FontFamily("Inter".into()))
        );
        assert!(convert(&Value::from(4096.0), TokenKind::Double).is_none());
        assert!(parse_numbers(&Value::from("1,2,3,4,5")).is_none());
    }

    #[test]
    fn radial_gradient_and_short_hex_colors_parse() {
        let value: Value = serde_json::from_str(
            r##"{"type":"radial","stops":["#fff","Transparent"],"center":[0.5,0.75],"origin":[0.5,0.8],"radius":[0.6,0.6]}"##,
        )
        .unwrap();
        let TokenValue::RadialGradient(g) = convert(&value, TokenKind::Brush).unwrap() else {
            panic!("expected radial gradient")
        };
        assert_eq!(g.stops[0].color, Rgba::from_rgb(255, 255, 255));
        assert_eq!(g.stops[1].color, Rgba::new(0, 0, 0, 0));
        assert_eq!(g.center, (0.5, 0.75));
        assert_eq!(g.origin, (0.5, 0.8));
        assert_eq!(g.radius, (0.6, 0.6));
        assert_eq!(
            convert(&Value::from("#ABCDEF"), TokenKind::Brush),
            Some(TokenValue::Solid(Rgba::from_rgb(0xAB, 0xCD, 0xEF)))
        );
    }

    #[test]
    fn block_comments_are_stripped_without_touching_string_literals() {
        let manifest = WidgetPackManifest::from_str(
            r##"{
                /* a block comment with "quotes" and // inside */
                "tokens": { "WidgetAccent": "#12AB34", // line comment
                            "TextPrimary": "#FFF" },
            }"##,
        )
        .unwrap();
        assert_eq!(manifest.tokens.len(), 2);
        let resolved = resolve_widget_tokens(&manifest, AppTheme::Dark, |_| None);
        assert!(resolved.problems.is_empty(), "{:?}", resolved.problems);
        assert_eq!(
            resolved.tokens.get("WidgetAccent"),
            Some(&TokenValue::Solid(Rgba::from_rgb(0x12, 0xAB, 0x34)))
        );
    }

    #[test]
    fn unknown_tokens_are_reported_instead_of_accepted() {
        let manifest = WidgetPackManifest::from_str(
            r##"{"tokens":{"NoSuchToken":5,"WidgetRadius":8,"AlsoUnknown":"#fff"}}"##,
        )
        .unwrap();
        let result = resolve_widget_tokens(&manifest, AppTheme::Dark, |_| None);
        assert_eq!(result.tokens.len(), 1);
        assert_eq!(result.problems.len(), 2);
        assert!(
            result
                .problems
                .iter()
                .any(|p| p.contains("Unknown token 'NoSuchToken'"))
        );
        assert!(
            result
                .problems
                .iter()
                .any(|p| p.contains("Unknown token 'AlsoUnknown'"))
        );
    }

    #[test]
    fn bundled_packs_have_valid_manifests() {
        // Bundled files are shared with the other theme-parity tests; serialize so
        // a concurrent install/import cannot be observed half-written.
        let _guard = crate::design::theme::theme_test_guard();
        let root = Path::new(crate::resource!("themes"));
        for id in crate::design::fonts::BUNDLED_THEME_IDS {
            let pack = WidgetPackManifest::load(&root.join(id).join("widgets"))
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(pack.raw_for(AppTheme::Dark).len() <= MAX_TOKENS);
            assert!(pack.raw_for(AppTheme::Light).len() <= MAX_TOKENS);
            for theme in [AppTheme::Dark, AppTheme::Light] {
                let resolved = resolve_widget_tokens(&pack, theme, |_| None);
                assert!(
                    resolved.problems.is_empty(),
                    "{id} {theme:?}: {:?}",
                    resolved.problems
                );
            }
            for (_key, value) in pack.raw_for(AppTheme::Dark) {
                let kind = TokenKind::for_key(_key);
                assert!(
                    value.is_object() || value.is_string() || value.is_number(),
                    "{_key} ({kind:?}) has an unsupported JSON value"
                );
            }
        }
    }
}
