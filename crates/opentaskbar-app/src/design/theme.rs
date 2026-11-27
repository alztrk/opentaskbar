//! reference parity for `Services/Settings/ThemeManager.cs`.
//!
//! Owns four independently styled surfaces (dock / menu / widget / settings). Override layers
//! are applied in order **base palette < Windows accent < theme pack**, so an explicitly selected
//! pack wins for the keys it defines. Rebuilding a surface emits `ThemeChanged`.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};

use opentaskbar_core::settings::{AppTheme, DockSettings, SurfaceBackground};
use serde_json::Value;

use super::palette;
use super::radius::RadiusManager;
#[allow(unused_imports)]
pub use super::tokens::editor_token_count;
use super::tokens::{
    CornerRadius, Gradient, Rgba, Thickness, TokenKind, TokenMap, TokenValue, editor_token_kind,
};
use super::widget_pack::parse_brush;
use super::windows_palette;

/// The four themed surfaces of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SurfaceKind {
    Dock,
    Menu,
    Widget,
    Settings,
}

impl SurfaceKind {
    pub const ALL: [Self; 4] = [Self::Dock, Self::Menu, Self::Widget, Self::Settings];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dock => "Dock",
            Self::Menu => "Menu",
            Self::Widget => "Widget",
            Self::Settings => "Settings",
        }
    }

    /// Resource key prefix for this surface's generated border thickness/color.
    pub fn stroke_prefix(self) -> &'static str {
        match self {
            Self::Dock => "DockBorder",
            Self::Menu => "MenuStroke",
            Self::Widget => "WidgetStroke",
            Self::Settings => "SettingsStroke",
        }
    }
}

/// Style of one surface: background mode, opacity and optional user border override.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceStyle {
    pub background: SurfaceBackground,
    pub opacity: f64,
    pub border_thickness: f64,
    /// `#RRGGBB` / `#AARRGGBB` user override; `None` keeps the themed stroke.
    pub border_color: Option<Rgba>,
}

impl SurfaceStyle {
    pub const fn new(
        background: SurfaceBackground,
        opacity: f64,
        border_thickness: f64,
        border_color: Option<Rgba>,
    ) -> Self {
        Self {
            background,
            opacity,
            border_thickness,
            border_color,
        }
    }

    /// Contract alias: `SurfaceStyle::set_opacity(kind, value)` updates only that
    /// surface through the shared theme manager.
    pub fn set_opacity(kind: SurfaceKind, opacity: f64) -> Result<(), &'static str> {
        ThemeManager::set_opacity(kind, opacity)
    }

    /// Contract alias: `SurfaceStyle::opacity(kind)` reads only that surface.
    pub fn opacity(kind: SurfaceKind) -> f64 {
        ThemeManager::opacity(kind)
    }
}

impl Default for SurfaceStyle {
    fn default() -> Self {
        Self::new(SurfaceBackground::Glass, 0.9, 1.0, None)
    }
}

/// Materials generated for one surface after all layers are applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceMaterials {
    pub fill: TokenValue,
    pub stroke: TokenValue,
    pub sheen: TokenValue,
    pub border_material: TokenValue,
    pub border_thickness: f64,
    pub shadow_opacity: f64,
    pub sidebar: Option<TokenValue>,
    pub content: Option<TokenValue>,
    pub hover: TokenValue,
    pub pressed: TokenValue,
    /// Snapshot of every registered editor token after all layers are applied.
    pub tokens: TokenMap,
    /// Concentric radii derived from the live dock radius and pack references.
    pub radii: BTreeMap<String, CornerRadius>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeChangedEvent {
    pub theme: AppTheme,
    pub effective: AppTheme,
    /// `None` means a full rebuild (palette / overrides / apply).
    pub surface: Option<SurfaceKind>,
    pub generation: u64,
}

struct ThemeState {
    current: AppTheme,
    effective: AppTheme,
    initialized: bool,
    styles: BTreeMap<SurfaceKind, SurfaceStyle>,
    base: TokenMap,
    windows: TokenMap,
    pack: TokenMap,
    /// Theme-editor tokens saved by the user. Highest layer so the real palette
    /// (and not just a preview swatch) reflects the editor's values.
    overrides: TokenMap,
    generated: BTreeMap<SurfaceKind, SurfaceMaterials>,
    generation: u64,
    legacy_opacity: f64,
    settings_opacity: f64,
    /// Explicit per-surface opacity values. These win over the legacy global
    /// `surface_opacity` fallback on every `apply`, so live previews survive a reload.
    opacity_overrides: BTreeMap<SurfaceKind, f64>,
    /// Radius hierarchy derived from the resolved `DockRadius` token + pack references.
    radii: RadiusManager,
}

impl Default for ThemeState {
    fn default() -> Self {
        let effective = windows_palette::effective_is_dark(AppTheme::System);
        let mut styles = BTreeMap::new();
        for kind in SurfaceKind::ALL {
            styles.insert(kind, SurfaceStyle::default());
        }
        let mut state = Self {
            current: AppTheme::System,
            effective: if effective {
                AppTheme::Dark
            } else {
                AppTheme::Light
            },
            initialized: false,
            styles,
            base: default_palette(effective),
            windows: TokenMap::new(),
            pack: TokenMap::new(),
            overrides: TokenMap::new(),
            generated: BTreeMap::new(),
            generation: 0,
            legacy_opacity: 0.9,
            settings_opacity: 0.95,
            opacity_overrides: BTreeMap::new(),
            radii: RadiusManager::new(),
        };
        state.refresh_radii();
        state.rebuild_all();
        state
    }
}

impl ThemeState {
    fn layers(&self) -> Vec<&TokenMap> {
        vec![&self.base, &self.windows, &self.pack, &self.overrides]
    }

    fn resolve(&self, key: &str) -> Option<&TokenValue> {
        self.layers()
            .into_iter()
            .rev()
            .find_map(|layer| layer.get(key))
    }

    fn brush(&self, key: &str) -> TokenValue {
        self.resolve(key)
            .cloned()
            .unwrap_or(TokenValue::Solid(Rgba::from_rgb(24, 28, 36)))
    }

    fn material_source(&self, key: &str) -> TokenValue {
        // Material keys can be overridden by packs via widgets/pack.json or by the
        // theme editor; the editor is the highest layer and wins for its keys.
        self.overrides
            .get(key)
            .or_else(|| self.pack.get(key))
            .or_else(|| self.windows.get(key))
            .or_else(|| self.base.get(key))
            .cloned()
            .unwrap_or_else(|| self.brush(key))
    }

    /// Every reference editor token, resolved through the real layer stack. This is the
    /// snapshot handed to GPUI/DComp consumers via `ThemeManager::materials`.
    fn editor_tokens(&self) -> TokenMap {
        super::tokens::EDITOR_TOKENS
            .iter()
            .filter_map(|token| {
                self.resolve(token.key)
                    .map(|v| (token.key.to_owned(), v.clone()))
            })
            .collect()
    }

    fn refresh_radii(&mut self) {
        self.radii = RadiusManager::from_tokens(&self.editor_tokens());
    }

    /// Replaces the theme-pack layer (used by `apply_pack_tokens_to_palette`).
    fn set_pack(&mut self, values: TokenMap) {
        self.pack = values;
        if self
            .pack
            .get("DockBackgroundMode")
            .and_then(|v| match v {
                TokenValue::Mode(m) => Some(m == "None"),
                _ => None,
            })
            .unwrap_or(false)
        {
            self.pack.insert("DockBorderMaterial".into(), transparent());
            self.pack.insert("DockHighlight".into(), transparent());
        }
        self.refresh_radii();
        self.generation = self.generation.wrapping_add(1);
        self.rebuild_all();
    }

    fn rebuild_all(&mut self) {
        for kind in SurfaceKind::ALL {
            let materials = self.build(kind);
            self.store(kind, materials);
        }
    }

    fn rebuild(&mut self, kind: SurfaceKind) {
        let materials = self.build(kind);
        self.store(kind, materials);
    }

    fn store(&mut self, kind: SurfaceKind, m: SurfaceMaterials) {
        self.generated.insert(kind, m);
    }

    fn style(&self, kind: SurfaceKind) -> SurfaceStyle {
        *self.styles.get(&kind).unwrap_or(&SurfaceStyle::default())
    }

    fn build(&self, kind: SurfaceKind) -> SurfaceMaterials {
        let style = self.style(kind);
        // Glass/Solid/None is derived from the effective per-surface opacity so the
        // material layer and the backdrop engine always agree on the same choice.
        let background = background_for_opacity(style.background, style.opacity);
        let fill_source = match kind {
            SurfaceKind::Dock => self.material_source("DockMaterial"),
            SurfaceKind::Menu | SurfaceKind::Widget => self.material_source("MenuMaterial"),
            SurfaceKind::Settings => self.brush("SettingsBackground"),
        };
        let stroke_source = match kind {
            SurfaceKind::Dock => self.brush("DockBorderMaterial"),
            _ => self.brush("MenuStroke"),
        };
        let sheen_source = self.brush("DockHighlight");
        let fill = apply_fill(fill_source, style);
        let stroke = apply_stroke(stroke_source, style);
        let sheen = apply_stroke(sheen_source, style);
        let thickness = if style.border_thickness.is_finite() {
            style.border_thickness.clamp(0.0, 12.0)
        } else {
            1.0
        };
        let border_material = if matches!(
            background,
            SurfaceBackground::None | SurfaceBackground::TrueBlack
        ) {
            transparent()
        } else if let Some(color) = style.border_color {
            TokenValue::Solid(color)
        } else {
            stroke.clone()
        };

        let mut sidebar = None;
        let mut content = None;
        if kind == SurfaceKind::Settings {
            let source = self.brush("SettingsSidebarOverlay");
            sidebar = Some(match background {
                SurfaceBackground::None | SurfaceBackground::TrueBlack => transparent(),
                SurfaceBackground::Solid => source.with_alpha(255),
                SurfaceBackground::Glass => source.with_alpha(scaled_alpha(style.opacity)),
            });
            content = Some(match background {
                SurfaceBackground::Solid => self.brush("SettingsBackground").with_alpha(255),
                // TrueBlack must never pick up the themed (accent-tinted) content brush.
                SurfaceBackground::TrueBlack => TokenValue::Solid(Rgba::new(0, 0, 0, 255)),
                // SettingsFrame already paints behind the window: avoid double stacking.
                _ => transparent(),
            });
        }

        let hover_source = self
            .overrides
            .get("WidgetCardHover")
            .or_else(|| self.pack.get("WidgetCardHover"))
            .or_else(|| self.pack.get("SurfaceHover"))
            .or_else(|| self.windows.get("WidgetCardHover"))
            .cloned()
            .unwrap_or_else(|| self.brush("WidgetCardHover"));
        let pressed_source = if let Some(value) = self
            .overrides
            .get("WidgetCardPressed")
            .or_else(|| self.pack.get("WidgetCardPressed"))
            .or_else(|| self.pack.get("SurfacePressed"))
        {
            value.clone()
        } else if let Some(value) = self.pack.get("WidgetCardHover") {
            value.clone().scaled_alpha(1.5)
        } else {
            self.windows
                .get("WidgetCardPressed")
                .cloned()
                .unwrap_or_else(|| self.brush("WidgetCardPressed"))
        };
        let scale = if background == SurfaceBackground::Glass {
            style.opacity
        } else {
            1.0
        };

        SurfaceMaterials {
            fill,
            stroke,
            sheen,
            border_material,
            border_thickness: thickness,
            shadow_opacity: if matches!(
                background,
                SurfaceBackground::None | SurfaceBackground::TrueBlack
            ) {
                0.0
            } else {
                0.45
            },
            sidebar,
            content,
            hover: hover_source.scaled_alpha(scale),
            pressed: pressed_source.scaled_alpha(scale),
            tokens: self.editor_tokens(),
            radii: self.radii.rebuild(),
        }
    }
}

fn scaled_alpha(opacity: f64) -> u8 {
    if !opacity.is_finite() {
        return 255;
    }
    (opacity.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn transparent() -> TokenValue {
    TokenValue::Solid(Rgba::new(0, 0, 0, 0))
}

/// Clamps an opacity into `0.0..=1.0`; non-finite values mean "no surface".
pub fn clamp_opacity(opacity: f64) -> f64 {
    if !opacity.is_finite() {
        return 0.0;
    }
    opacity.clamp(0.0, 1.0)
}

/// Decides Glass / Solid / None / TrueBlack from the effective per-surface opacity.
/// An explicit `Solid`, `TrueBlack` or `None` request is honoured; `Glass` becomes
/// `None` at zero opacity and `Solid` at full opacity.
pub fn background_for_opacity(requested: SurfaceBackground, opacity: f64) -> SurfaceBackground {
    match requested {
        SurfaceBackground::None => SurfaceBackground::None,
        SurfaceBackground::Solid => SurfaceBackground::Solid,
        SurfaceBackground::TrueBlack => SurfaceBackground::TrueBlack,
        SurfaceBackground::Glass => {
            let opacity = clamp_opacity(opacity);
            if opacity <= 0.0 {
                SurfaceBackground::None
            } else if opacity >= 1.0 {
                SurfaceBackground::Solid
            } else {
                SurfaceBackground::Glass
            }
        }
    }
}

fn apply_fill(source: TokenValue, style: SurfaceStyle) -> TokenValue {
    match style.background {
        SurfaceBackground::None => transparent(),
        // Opaque black only: no accent tint, no themed source colour.
        SurfaceBackground::TrueBlack => TokenValue::Solid(Rgba::new(0, 0, 0, 255)),
        SurfaceBackground::Solid => source.with_alpha(255),
        SurfaceBackground::Glass => source.with_alpha(scaled_alpha(style.opacity)),
    }
}

fn apply_stroke(source: TokenValue, style: SurfaceStyle) -> TokenValue {
    if matches!(
        style.background,
        SurfaceBackground::None | SurfaceBackground::TrueBlack
    ) {
        transparent()
    } else {
        source
    }
}

fn default_palette(dark: bool) -> TokenMap {
    let p = palette(dark);
    let mut map = TokenMap::new();
    let put = |map: &mut TokenMap, key: &str, color: Rgba| {
        map.insert(key.to_owned(), TokenValue::Solid(color));
    };
    put(
        &mut map,
        "AccentBrush",
        Rgba::from_rgb(p.accent.0, p.accent.1, p.accent.2),
    );
    put(
        &mut map,
        "AccentText",
        Rgba::from_rgb(p.text_primary.0, p.text_primary.1, p.text_primary.2),
    );
    put(
        &mut map,
        "TextPrimary",
        Rgba::from_rgb(p.text_primary.0, p.text_primary.1, p.text_primary.2),
    );
    put(
        &mut map,
        "TextSecondary",
        Rgba::from_rgb(p.text_secondary.0, p.text_secondary.1, p.text_secondary.2),
    );
    put(
        &mut map,
        "StrokeFocus",
        Rgba::from_rgb(p.accent.0, p.accent.1, p.accent.2),
    );
    put(
        &mut map,
        "MenuStroke",
        Rgba::from_rgb(p.border_rgb.0, p.border_rgb.1, p.border_rgb.2),
    );
    put(
        &mut map,
        "DockBorderMaterial",
        Rgba::from_rgb(p.border_rgb.0, p.border_rgb.1, p.border_rgb.2),
    );
    put(
        &mut map,
        "DockHighlight",
        Rgba::from_rgb(p.specular_top.0, p.specular_top.1, p.specular_top.2),
    );
    put(
        &mut map,
        "SettingsBackground",
        Rgba::from_rgb(p.canvas.0, p.canvas.1, p.canvas.2),
    );
    put(
        &mut map,
        "SettingsSidebarOverlay",
        Rgba::from_rgb(p.sidebar.0, p.sidebar.1, p.sidebar.2),
    );
    put(
        &mut map,
        "WidgetCardHover",
        Rgba::from_rgb(p.hover.0, p.hover.1, p.hover.2).with_alpha(if dark { 0x22 } else { 0x18 }),
    );
    put(
        &mut map,
        "WidgetCardPressed",
        Rgba::from_rgb(p.hover.0, p.hover.1, p.hover.2).with_alpha(if dark { 0x38 } else { 0x2A }),
    );
    put(&mut map, "SurfaceHover", transparent_color());
    let dock = Rgba::from_rgb(p.dock_pill.0, p.dock_pill.1, p.dock_pill.2);
    map.insert(
        "DockMaterial".to_owned(),
        TokenValue::Gradient(Gradient::vertical(
            dock,
            Rgba::from_rgb(p.canvas.0, p.canvas.1, p.canvas.2),
        )),
    );
    map.insert(
        "MenuMaterial".to_owned(),
        TokenValue::Gradient(Gradient::vertical(
            Rgba::from_rgb(p.surface.0, p.surface.1, p.surface.2),
            Rgba::from_rgb(p.elevated.0, p.elevated.1, p.elevated.2),
        )),
    );
    add_reference_editor_defaults(&mut map, dark);
    map
}

/// Seeds every reference editor token from `Design/Palettes/{Dark,Light}.xaml` and
/// `Design/Theme.xaml` so `ThemeManager::resolve()` always has a typed value,
/// even before a pack or the theme editor overrides it.
fn add_reference_editor_defaults(map: &mut TokenMap, dark: bool) {
    let solid = |map: &mut TokenMap, key: &str, hex: &str| {
        if let Some(color) = Rgba::from_hex(hex) {
            map.entry(key.to_owned())
                .or_insert(TokenValue::Solid(color));
        }
    };
    let number = |map: &mut TokenMap, key: &str, value: f64| {
        map.entry(key.to_owned())
            .or_insert(TokenValue::Double(value));
    };
    let radius = |map: &mut TokenMap, key: &str, value: i32| {
        map.entry(key.to_owned())
            .or_insert(TokenValue::CornerRadius(CornerRadius::uniform(value)));
    };
    let color = |d: &'static str, l: &'static str| -> &'static str { if dark { d } else { l } };

    solid(map, "TextPrimary", color("#FFF3F4F6", "#FF1B1B1B"));
    solid(map, "TextSecondary", color("#FFAAB2C0", "#FF61666E"));
    solid(map, "AccentBrush", color("#FF38BDF8", "#FF0369A1"));
    solid(map, "AccentText", color("#FF05070B", "#FFFFFFFF"));
    solid(map, "StrokeSubtle", color("#FF3A4352", "#FFE5E5E5"));
    solid(map, "StrokeFocus", color("#FF38BDF8", "#FF0369A1"));
    solid(map, "SurfaceSelected", color("#14FFFFFF", "#0D000000"));
    solid(map, "SurfaceHover", color("#18FFFFFF", "#12162038"));
    solid(map, "SurfacePressed", color("#22FFFFFF", "#16000000"));
    map.entry("DockBackgroundMode".into())
        .or_insert(TokenValue::Mode("Glass".into()));
    radius(map, "DockRadius", 20);
    radius(map, "DockInnerRadius", 19);
    radius(map, "ControlRadius", 12);
    radius(map, "PopupRadius", 12);
    radius(map, "PopupInnerRadius", 11);
    radius(map, "WidgetRadius", 12);
    radius(map, "DockWidgetRadius", 12);
    radius(map, "DockWidgetActionRadius", 5);
    radius(map, "WidgetFieldRadius", 7);
    radius(map, "WidgetQuickActionRadius", 18);
    map.entry("DockWidgetPadding".into())
        .or_insert_with(|| TokenValue::Thickness(Thickness::parse("9,5").unwrap()));
    map.entry("SettingsPageInset".into())
        .or_insert_with(|| TokenValue::Thickness(Thickness::parse("28,24,28,36").unwrap()));
    number(map, "DockShadowBlur", 28.0);
    number(map, "DockShadowOpacity", 0.45);
    number(map, "SettingsBackgroundOpacity", 0.95);
    number(map, "MenuLabelWidth", 286.0);
    number(map, "TextSizeBody", 13.0);
    number(map, "TextSizeCaption", 12.0);
    number(map, "TextSizeWidgetValue", 18.0);
    solid(map, "DockShadowColor", "#FF000000");

    solid(map, "WidgetCard", "#00000000");
    solid(map, "WidgetCardHover", color("#22FFFFFF", "#18000000"));
    solid(map, "WidgetCardPressed", color("#38FFFFFF", "#2A000000"));
    solid(map, "WidgetCardStroke", "#00000000");
    solid(map, "WidgetTrack", color("#2EFFFFFF", "#1F000000"));
    solid(map, "WidgetAccent", color("#FF2DD4BF", "#FF0D9488"));
    solid(map, "DockWidgetSurface", color("#38252B34", "#AFFFFFFF"));
    solid(map, "MenuDivider", color("#FF323A48", "#FFE8E8E8"));
    solid(map, "SurfacePopup", color("#F7252B37", "#F7FFFFFF"));
    solid(map, "SettingsControl", color("#FF252B37", "#FFFFFFFF"));
    solid(map, "SettingsControlHover", color("#14FFFFFF", "#0D000000"));
    solid(map, "IconForeground", color("#FFF3F4F6", "#FF1B1B1B"));
    solid(map, "StartTileBackground", "#00000000");
    solid(map, "StartTileStroke", "#00000000");
    solid(map, "StartTileHover", color("#2BFFFFFF", "#1E000000"));
    solid(map, "WidgetWarning", color("#FFFBBF24", "#FFD97706"));
    solid(map, "WidgetFocusAccent", color("#FFEF4444", "#FFDC2626"));
    map.entry("InterfaceFont".into())
        .or_insert(TokenValue::FontFamily(
            super::fonts::INTER_FAMILY.to_owned(),
        ));

    let logo = if dark {
        ["#FF38BDF8", "#FF0284C7", "#FF0369A1"]
    } else {
        ["#FF0EA5E9", "#FF0284C7", "#FF0369A1"]
    };
    map.entry("WindowsLogoBrush".into()).or_insert_with(|| {
        TokenValue::Gradient(super::tokens::Gradient {
            stops: vec![
                super::tokens::GradientStop {
                    color: Rgba::from_hex(logo[0]).unwrap(),
                    position: 0.0,
                },
                super::tokens::GradientStop {
                    color: Rgba::from_hex(logo[1]).unwrap(),
                    position: 0.55,
                },
                super::tokens::GradientStop {
                    color: Rgba::from_hex(logo[2]).unwrap(),
                    position: 1.0,
                },
            ],
            start: (0.0, 0.0),
            end: (1.0, 1.0),
        })
    });
    let glow = if dark {
        ["#267DD3FC", "#00000000"]
    } else {
        ["#300284C7", "#00FFFFFF"]
    };
    map.entry("IconHoverGlow".into()).or_insert_with(|| {
        TokenValue::RadialGradient(super::tokens::RadialGradient::new(
            vec![
                super::tokens::GradientStop {
                    color: Rgba::from_hex(glow[0]).unwrap(),
                    position: 0.0,
                },
                super::tokens::GradientStop {
                    color: Rgba::from_hex(glow[1]).unwrap(),
                    position: 1.0,
                },
            ],
            (0.5, 0.75),
            (0.5, 0.8),
            (0.6, 0.6),
        ))
    });
    add_editor_remainder_defaults(map, dark);
}

/// Defaults for the advanced keys exposed in the reference editor's "Advanced" tab.
fn add_editor_remainder_defaults(map: &mut TokenMap, dark: bool) {
    let solid = |map: &mut TokenMap, key: &str, hex: &str| {
        if let Some(color) = Rgba::from_hex(hex) {
            map.entry(key.to_owned())
                .or_insert(TokenValue::Solid(color));
        }
    };
    let number = |map: &mut TokenMap, key: &str, value: f64| {
        map.entry(key.to_owned())
            .or_insert(TokenValue::Double(value));
    };
    let color = |d: &'static str, l: &'static str| -> &'static str { if dark { d } else { l } };

    solid(map, "AccentTrack", color("#FF0369A1", "#FF075985"));
    solid(map, "CaptionCloseForeground", "#FFFFFFFF");
    solid(map, "DangerText", color("#FFFF9DA7", "#FFAF2338"));
    solid(map, "IndicatorBrush", color("#90E2E8F0", "#601E293B"));
    solid(
        map,
        "IndicatorForegroundBrush",
        color("#FFFFFFFF", "#E00F172A"),
    );
    solid(map, "NotificationBrush", color("#FFFF6673", "#FFD22D42"));
    solid(map, "SettingsTrack", color("#FF141922", "#FFE9E9E9"));
    solid(map, "ShellButtonHover", color("#2AFFFFFF", "#1A000000"));
    solid(map, "ShellButtonPressed", color("#47FFFFFF", "#30000000"));
    solid(map, "StartTileBorder", "#00000000");
    solid(map, "StartTilePressed", color("#3EFFFFFF", "#30000000"));
    solid(map, "SurfaceDanger", "#FFC43846");
    solid(map, "SurfaceKeyboardFocus", color("#FF2A303C", "#FFE9E9E9"));
    solid(
        map,
        "SurfaceSelectedStrong",
        color("#22FFFFFF", "#16000000"),
    );
    solid(map, "TextMuted", "#807C8698");
    solid(map, "WidgetAccentText", "#FFFFFFFF");
    solid(map, "WidgetBlue", color("#FFA2C0FF", "#FF1D66D7"));
    solid(
        map,
        "WidgetDangerForeground",
        color("#FFF36C74", "#FFDC2626"),
    );
    solid(map, "WidgetDangerHover", color("#22DF4B53", "#1FDF4B53"));
    solid(map, "WidgetDangerStroke", color("#42DF4B53", "#3ADF4B53"));
    solid(map, "WidgetGlassCard", "#00000000");
    solid(map, "WidgetGlassCardHover", color("#18FFFFFF", "#12000000"));
    solid(map, "WidgetGlassCardStroke", "#00000000");
    solid(map, "WidgetGreen", color("#FF58D8B2", "#FF0D9E78"));
    solid(map, "WidgetIsland", color("#08FFFFFF", "#0A000000"));
    solid(map, "WidgetIslandStroke", color("#10FFFFFF", "#12000000"));
    solid(map, "WidgetOnStroke", color("#FF4F8EF7", "#FF2563EB"));
    solid(map, "WidgetSelectedFill", color("#335B92FF", "#262474E9"));
    solid(
        map,
        "WidgetSelectedForeground",
        color("#FFA2C0FF", "#FF1D66D7"),
    );
    solid(map, "WidgetSelectedStroke", color("#7891B8FF", "#702C74E9"));
    if let Some(color) = Rgba::from_hex(color("#18FFFFFF", "#12162038")) {
        map.entry("SurfaceHoverColor".into())
            .or_insert(TokenValue::Color(color));
    }
    map.entry("DockSheenRadius".into())
        .or_insert(TokenValue::CornerRadius(CornerRadius {
            top_left: 19,
            top_right: 19,
            bottom_right: 0,
            bottom_left: 0,
        }));
    map.entry("MenuRowPadding".into())
        .or_insert_with(|| TokenValue::Thickness(Thickness::parse("10,7").unwrap()));
    map.entry("MenuDividerInset".into())
        .or_insert_with(|| TokenValue::Thickness(Thickness::parse("12,7").unwrap()));
    map.entry("WidgetActionFont".into())
        .or_insert(TokenValue::FontFamily("Segoe MDL2 Assets".to_owned()));
    number(map, "TextSizeWidgetLabel", 11.0);
    number(map, "SurfaceOpacity", 0.9);
    number(map, "DockOpacity", 0.9);
    number(map, "MenuOpacity", 0.9);
    number(map, "WidgetOpacity", 0.9);
    number(map, "SettingsOpacity", 0.95);

    let gradient = |map: &mut TokenMap, key: &str, top: &str, bottom: &str| {
        map.entry(key.to_owned()).or_insert_with(|| {
            TokenValue::Gradient(Gradient::vertical(
                Rgba::from_hex(top).unwrap_or_default(),
                Rgba::from_hex(bottom).unwrap_or_default(),
            ))
        });
    };
    gradient(map, "DockInnerBevelMaterial", "#70FFFFFF", "#18000000");
    gradient(map, "DockScrollScrimStart", "#55000000", "#00000000");
    gradient(map, "DockScrollScrimEnd", "#55000000", "#00000000");
    gradient(map, "DockScrollScrimTop", "#55000000", "#00000000");
    gradient(map, "DockScrollScrimBottom", "#55000000", "#00000000");
    gradient(map, "MenuStroke", "#70FFFFFF", "#4A0B1220");
    gradient(
        map,
        "WidgetAccentFill",
        color("#FF2DD4BF", "#FF14B8A6"),
        color("#FF0EA5E9", "#FF0284C7"),
    );
}

fn transparent_color() -> Rgba {
    Rgba::new(255, 255, 255, 0)
}

fn lock() -> std::sync::MutexGuard<'static, ThemeState> {
    static STATE: std::sync::LazyLock<Mutex<ThemeState>> =
        std::sync::LazyLock::new(|| Mutex::new(ThemeState::default()));
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn subscribers() -> &'static Mutex<Vec<Sender<ThemeChangedEvent>>> {
    static SUBSCRIBERS: Mutex<Vec<Sender<ThemeChangedEvent>>> = Mutex::new(Vec::new());
    &SUBSCRIBERS
}

fn notify(state: &ThemeState, surface: Option<SurfaceKind>) {
    let event = ThemeChangedEvent {
        theme: state.current,
        effective: state.effective,
        surface,
        generation: state.generation,
    };
    let mut subs = subscribers().lock().unwrap_or_else(|p| p.into_inner());
    subs.retain(|tx| tx.send(event.clone()).is_ok());
}

/// Theme manager with four independent surface styles and layered overrides.
pub struct ThemeManager;

impl ThemeManager {
    /// Subscribes to rebuilt-theme notifications; dropped receivers are pruned automatically.
    pub fn subscribe() -> Receiver<ThemeChangedEvent> {
        let (tx, rx) = mpsc::channel();
        subscribers()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(tx);
        rx
    }

    pub fn current() -> AppTheme {
        lock().current
    }

    pub fn effective_theme() -> AppTheme {
        lock().effective
    }

    pub fn effective_is_dark() -> bool {
        Self::effective_theme() == AppTheme::Dark
    }

    pub fn generation() -> u64 {
        lock().generation
    }

    pub fn is_initialized() -> bool {
        lock().initialized
    }

    /// Applies a theme and (optionally) all four surface styles from settings.
    /// `SystemUsesLightTheme` is authoritative when `theme == System`.
    pub fn apply(theme: AppTheme, settings: Option<&DockSettings>) {
        {
            let mut state = lock();
            let dark = windows_palette::effective_is_dark(theme);
            state.current = theme;
            state.effective = if dark {
                AppTheme::Dark
            } else {
                AppTheme::Light
            };
            state.base = default_palette(dark);
            state.styles.clear();
            let overrides = state.opacity_overrides.clone();
            for kind in SurfaceKind::ALL {
                let style = settings
                    .map(|s| style_from_settings(s, kind, &overrides))
                    .unwrap_or_else(|| {
                        let mut style = SurfaceStyle::default();
                        if let Some(opacity) = overrides.get(&kind) {
                            style.opacity = *opacity;
                        }
                        style
                    });
                state.styles.insert(kind, style);
            }
            state.legacy_opacity = state.style(SurfaceKind::Dock).opacity;
            state.settings_opacity = state.style(SurfaceKind::Settings).opacity;
            state.initialized = true;
            state.generation = state.generation.wrapping_add(1);
            state.refresh_radii();
            state.rebuild_all();
        }
        Self::notify_all();
    }

    /// Applies Windows-derived tokens. Theme pack overrides still win.
    pub fn set_windows_palette(palette: &windows_palette::WindowsPalette) {
        {
            let mut state = lock();
            state.windows = palette.tokens().clone();
            state.generation = state.generation.wrapping_add(1);
            state.rebuild_all();
        }
        Self::notify_all();
    }

    /// Replaces the theme pack's override layer; an empty map restores the palette.
    pub fn set_overrides(values: TokenMap) {
        {
            let mut state = lock();
            state.set_pack(values);
        }
        Self::notify_all();
    }

    pub fn get_surface(kind: SurfaceKind) -> SurfaceStyle {
        lock().style(kind)
    }

    /// Effective opacity of a single surface (WS-B cross-WS API).
    pub fn opacity(kind: SurfaceKind) -> f64 {
        lock().style(kind).opacity
    }

    /// Sets the opacity of one surface only. The value is recorded as a per-kind
    /// override so a later `apply(settings)` cannot reset it from the global fallback.
    pub fn set_opacity(kind: SurfaceKind, opacity: f64) -> Result<(), &'static str> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err("opacity out of range");
        }
        {
            let mut state = lock();
            if !state.initialized {
                return Err("theme not initialized");
            }
            state.opacity_overrides.insert(kind, opacity);
            let current = state.style(kind);
            state.styles.insert(
                kind,
                SurfaceStyle::new(
                    current.background,
                    opacity,
                    current.border_thickness,
                    current.border_color,
                ),
            );
            if kind == SurfaceKind::Dock {
                state.legacy_opacity = opacity;
            }
            if kind == SurfaceKind::Settings {
                state.settings_opacity = opacity;
            }
            state.generation = state.generation.wrapping_add(1);
            state.rebuild(kind);
        }
        Self::notify(Some(kind));
        Ok(())
    }

    /// Glass / Solid / None for one surface, derived from its own opacity.
    pub fn effective_background(kind: SurfaceKind) -> SurfaceBackground {
        let state = lock();
        let style = state.style(kind);
        background_for_opacity(style.background, style.opacity)
    }

    /// Drops explicit per-surface overrides; the next `apply(settings)` restores
    /// the settings/global values.
    pub fn clear_opacity_overrides() {
        {
            let mut state = lock();
            state.opacity_overrides.clear();
        }
        Self::notify_all();
    }

    /// Live preview of one surface. Invalid opacity / uninitialized theme is rejected.
    pub fn set_surface(
        kind: SurfaceKind,
        background: SurfaceBackground,
        opacity: f64,
    ) -> Result<(), &'static str> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err("opacity out of range");
        }
        {
            let mut state = lock();
            if !state.initialized {
                return Err("theme not initialized");
            }
            let current = state.style(kind);
            state.styles.insert(
                kind,
                SurfaceStyle::new(
                    background,
                    opacity,
                    current.border_thickness,
                    current.border_color,
                ),
            );
            if kind == SurfaceKind::Dock {
                state.legacy_opacity = opacity;
            }
            state.generation = state.generation.wrapping_add(1);
            state.rebuild(kind);
        }
        Self::notify(Some(kind));
        Ok(())
    }

    /// Live preview of a surface border (thickness 0..12).
    pub fn set_border(
        kind: SurfaceKind,
        thickness: f64,
        color: Option<Rgba>,
    ) -> Result<(), &'static str> {
        if !thickness.is_finite() || !(0.0..=12.0).contains(&thickness) {
            return Err("thickness out of range");
        }
        {
            let mut state = lock();
            if !state.initialized {
                return Err("theme not initialized");
            }
            let current = state.style(kind);
            state.styles.insert(
                kind,
                SurfaceStyle::new(current.background, current.opacity, thickness, color),
            );
            state.generation = state.generation.wrapping_add(1);
            state.rebuild(kind);
        }
        Self::notify(Some(kind));
        Ok(())
    }

    /// Legacy single-opacity API: updates dock and menu together.
    pub fn set_surface_opacity(opacity: f64) -> Result<(), &'static str> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err("opacity out of range");
        }
        {
            let mut state = lock();
            if !state.initialized {
                return Err("theme not initialized");
            }
            state.legacy_opacity = opacity;
            for kind in [SurfaceKind::Dock, SurfaceKind::Menu] {
                let style = state.style(kind);
                state.styles.insert(
                    kind,
                    SurfaceStyle::new(
                        style.background,
                        opacity,
                        style.border_thickness,
                        style.border_color,
                    ),
                );
                state.rebuild(kind);
            }
            state.generation = state.generation.wrapping_add(1);
        }
        Self::notify_all();
        Ok(())
    }

    /// Settings window opacity (clamped to 0.6..1 in `SetSettingsOpacity`).
    pub fn set_settings_opacity(opacity: f64) -> Result<(), &'static str> {
        if !opacity.is_finite() || !(0.6..=1.0).contains(&opacity) {
            return Err("settings opacity out of range");
        }
        {
            let mut state = lock();
            if !state.initialized {
                return Err("theme not initialized");
            }
            state.settings_opacity = opacity;
            let style = state.style(SurfaceKind::Settings);
            state.styles.insert(
                SurfaceKind::Settings,
                SurfaceStyle::new(
                    style.background,
                    opacity,
                    style.border_thickness,
                    style.border_color,
                ),
            );
            state.generation = state.generation.wrapping_add(1);
            state.rebuild(SurfaceKind::Settings);
        }
        Self::notify(Some(SurfaceKind::Settings));
        Ok(())
    }

    pub fn is_applied(settings: &DockSettings) -> bool {
        let state = lock();
        state.initialized
            && state.current == settings.theme
            && SurfaceKind::ALL.iter().all(|kind| {
                state.style(*kind) == style_from_settings(settings, *kind, &state.opacity_overrides)
            })
    }

    /// Layered token lookup (theme pack > Windows > base palette).
    pub fn resolve(key: &str) -> Option<TokenValue> {
        lock().resolve(key).cloned()
    }

    /// Materials produced for a surface by the latest rebuild.
    pub fn materials(kind: SurfaceKind) -> SurfaceMaterials {
        lock().generated.get(&kind).cloned().unwrap_or_default()
    }

    /// Concentric radius value for a reference radius key (`DockRadius`, `PopupRadius`, …).
    pub fn radius(key: &str) -> Option<i32> {
        lock().radii.value(key)
    }

    /// Full radius hierarchy including the top-corner-only `DockSheenRadius`.
    pub fn radii() -> BTreeMap<String, CornerRadius> {
        lock().radii.rebuild()
    }

    fn notify_all() {
        let state = lock();
        if state.initialized {
            drop(state);
            Self::notify(None);
        }
    }

    fn notify(surface: Option<SurfaceKind>) {
        let state = lock();
        notify(&state, surface);
    }
}

/// Module-level alias of [`ThemeManager::set_opacity`] for cross-WS callers.
pub fn set_opacity(kind: SurfaceKind, opacity: f64) -> Result<(), &'static str> {
    ThemeManager::set_opacity(kind, opacity)
}

/// Module-level alias of [`ThemeManager::opacity`] for cross-WS callers.
pub fn opacity(kind: SurfaceKind) -> f64 {
    ThemeManager::opacity(kind)
}

/// Applies the theme editor's saved tokens to the **real** palette.
///
/// Editor tokens live above the theme-pack layer, so every surface rebuilt from
/// `ThemeManager` (dock / menu / widget / settings) observes them. An empty map
/// clears the editor layer and restores the previous palette + pack values.
pub fn apply_token_overrides(values: TokenMap) {
    {
        let mut state = lock();
        state.overrides = values;
        state.refresh_radii();
        state.generation = state.generation.wrapping_add(1);
        state.rebuild_all();
    }
    ThemeManager::notify_all();
}

/// Production bridge for the settings UI / theme packs: pushes resolved reference tokens
/// into the **real** palette (not a preview swatch) and rebuilds every surface.
/// Accepts an owned map or a borrow of one.
pub fn apply_pack_tokens_to_palette(tokens: impl Borrow<TokenMap>) {
    let tokens = tokens.borrow();
    {
        let mut state = lock();
        state.set_pack(tokens.clone());
    }
    ThemeManager::notify_all();
}

/// Module-level alias of [`ThemeManager::resolve`] for cross-WS callers.
pub fn resolve_token(key: &str) -> Option<TokenValue> {
    ThemeManager::resolve(key)
}

/// Startup hook: applies the tokens persisted in `settings.extra["ThemeTokens"]`.
/// Call this right after `ThemeManager::apply` so the real palette is themed
/// before any window opens (the settings window also calls it on load).
pub fn apply_saved_token_overrides(settings: &DockSettings) {
    if let Some(tokens) = settings.extra.get("ThemeTokens").and_then(Value::as_object) {
        apply_token_overrides(token_overrides_from_json(tokens));
    }
}

/// Convenience: parses the JSON object persisted under `extra["ThemeTokens"]`.
///
/// Strings are hex colours (`#RRGGBB` / `#AARRGGBB`), numbers are doubles, and
/// objects of the form `{"stops":["#..",…],"angle":90}` become gradients.
pub fn token_overrides_from_json(map: &serde_json::Map<String, Value>) -> TokenMap {
    let mut out = TokenMap::new();
    for (key, value) in map {
        // Unknown keys are ignored (never smuggled into the live palette with a
        // guessed type), matching `WidgetPack.Resolve`'s "Unknown token" rule.
        let Some(kind) = editor_token_kind(key) else {
            continue;
        };
        let token = match (kind, value) {
            (TokenKind::Double, Value::Number(number)) => number.as_f64().map(TokenValue::Double),
            (TokenKind::Double, Value::String(text)) => {
                text.trim().parse::<f64>().ok().map(TokenValue::Double)
            }
            (TokenKind::FontFamily, Value::String(text)) if !text.trim().is_empty() => {
                Some(TokenValue::FontFamily(text.trim().to_owned()))
            }
            (TokenKind::Mode, Value::String(text))
                if matches!(text.as_str(), "Glass" | "Solid" | "None") =>
            {
                Some(TokenValue::Mode(text.clone()))
            }
            (TokenKind::CornerRadius, Value::String(text)) => {
                super::tokens::CornerRadius::parse(text).map(TokenValue::CornerRadius)
            }
            (TokenKind::CornerRadius, Value::Number(number)) => number
                .as_f64()
                .and_then(|v| (v.is_finite() && (0.0..=2048.0).contains(&v)).then_some(v as i32))
                .map(|v| TokenValue::CornerRadius(CornerRadius::uniform(v))),
            (TokenKind::Thickness, Value::String(text)) => {
                super::tokens::Thickness::parse(text).map(TokenValue::Thickness)
            }
            (TokenKind::Color, Value::String(text)) => Rgba::from_hex(text).map(TokenValue::Color),
            (TokenKind::Brush, Value::Object(_)) => parse_brush(value),
            (TokenKind::Brush, Value::String(text)) => Rgba::from_hex(text)
                .map(TokenValue::Solid)
                .or_else(|| parse_brush(value)),
            _ => None,
        };
        if let Some(token) = token {
            out.insert(key.clone(), token);
        }
    }
    out
}

fn style_from_settings(
    settings: &DockSettings,
    kind: SurfaceKind,
    overrides: &BTreeMap<SurfaceKind, f64>,
) -> SurfaceStyle {
    let background = |name: &str, typed: SurfaceBackground| -> SurfaceBackground {
        match settings.extra.get(name).and_then(|v| v.as_str()) {
            Some("Solid") => SurfaceBackground::Solid,
            Some("None") => SurfaceBackground::None,
            Some("Glass") => SurfaceBackground::Glass,
            Some("TrueBlack") => SurfaceBackground::TrueBlack,
            _ => typed,
        }
    };
    let number = |name: &str, typed: f64, fallback: f64| -> f64 {
        settings
            .extra
            .get(name)
            .and_then(|v| v.as_f64())
            .unwrap_or(if typed.is_finite() { typed } else { fallback })
    };
    // Legacy global opacity stays as the fallback for surfaces without their own
    // value; explicit per-surface overrides (set_opacity) win over everything.
    let global = settings
        .extra
        .get("SurfaceOpacity")
        .and_then(|value| value.as_f64())
        .unwrap_or(settings.surface_opacity);
    let color = |name: &str| -> Option<Rgba> {
        settings
            .extra
            .get(name)
            .and_then(|v| v.as_str())
            .and_then(Rgba::from_hex)
    };
    let mut style = match kind {
        SurfaceKind::Dock => SurfaceStyle::new(
            settings.dock_background,
            settings.dock_opacity,
            settings.dock_border_thickness,
            color("DockBorderColor"),
        ),
        SurfaceKind::Menu => SurfaceStyle::new(
            background("MenuBackground", settings.menu_background),
            number("MenuOpacity", settings.menu_opacity, global),
            number("MenuBorderThickness", settings.menu_border_thickness, 1.0),
            color("MenuBorderColor"),
        ),
        SurfaceKind::Widget => SurfaceStyle::new(
            background("WidgetBackground", settings.widget_background),
            number("WidgetOpacity", settings.widget_opacity, global),
            number(
                "WidgetBorderThickness",
                settings.widget_border_thickness,
                1.0,
            ),
            color("WidgetBorderColor"),
        ),
        SurfaceKind::Settings => SurfaceStyle::new(
            background("SettingsBackground", settings.settings_background),
            number(
                "SettingsOpacity",
                settings.settings_opacity,
                global.clamp(0.6, 1.0),
            ),
            number(
                "SettingsBorderThickness",
                settings.settings_border_thickness,
                1.0,
            ),
            color("SettingsBorderColor"),
        ),
    };
    if let Some(opacity) = overrides.get(&kind) {
        style.opacity = *opacity;
    }
    style
}

/// Helper for consumers that need to convert a token to the reference-style mode name.
pub fn mode_name(value: &TokenValue) -> Option<&str> {
    match value {
        TokenValue::Mode(mode) => Some(mode),
        _ => None,
    }
}

/// Parses a `#RRGGBB` / `#AARRGGBB` user border color with the same rules as reference.
pub fn parse_border_color(text: &str) -> Option<Rgba> {
    Rgba::from_hex(text)
}

/// Keeps the compiler from dropping the `TokenKind` import when downstream code only
/// calls the typed helpers above.
pub fn token_kind(value: &TokenValue) -> TokenKind {
    value.kind()
}

impl ThemeState {
    #[cfg(test)]
    fn build_with_style(&self, kind: SurfaceKind, style: SurfaceStyle) -> SurfaceMaterials {
        let mut clone = ThemeState {
            current: self.current,
            effective: self.effective,
            initialized: self.initialized,
            styles: self.styles.clone(),
            base: self.base.clone(),
            windows: self.windows.clone(),
            pack: self.pack.clone(),
            overrides: self.overrides.clone(),
            generated: self.generated.clone(),
            generation: self.generation,
            legacy_opacity: self.legacy_opacity,
            settings_opacity: self.settings_opacity,
            opacity_overrides: self.opacity_overrides.clone(),
            radii: self.radii.clone(),
        };
        clone.styles.insert(kind, style);
        clone.build(kind)
    }
}

/// One process-wide lock for every test that touches shared theme state: the global
/// `ThemeManager`, bundled theme resources, and the sidecar/registry tests that
/// install or read those resources. Tests must take [`theme_test_guard`] before
/// mutating or observing any of it.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Serializes tests that share the live palette, bundled theme files or sidecar state.
/// Poisoning is recovered from: a failing test must not deadlock every later test.
#[cfg(test)]
pub(crate) fn theme_test_guard() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glass_solid_none_alpha_behaviour() {
        for (mode, expected_a) in [
            (SurfaceBackground::Glass, scaled_alpha(0.5)),
            (SurfaceBackground::Solid, 255),
            (SurfaceBackground::None, 0),
        ] {
            let fill = apply_fill(
                TokenValue::Solid(Rgba::from_rgb(10, 20, 30).with_alpha(0x80)),
                SurfaceStyle::new(mode, 0.5, 1.0, None),
            );
            let TokenValue::Solid(color) = fill else {
                panic!("expected solid fill")
            };
            assert_eq!(color.a, expected_a, "{mode:?}");
            if mode != SurfaceBackground::None && mode != SurfaceBackground::TrueBlack {
                assert_eq!((color.r, color.g, color.b), (10, 20, 30));
            }
        }
    }

    #[test]
    fn true_black_is_opaque_black_without_stroke_sheen_or_shadow() {
        let state = ThemeState::default();
        let style = SurfaceStyle::new(SurfaceBackground::TrueBlack, 0.9, 3.0, None);
        let m = state.build_with_style(SurfaceKind::Dock, style);
        assert_eq!(
            m.fill,
            TokenValue::Solid(Rgba::new(0, 0, 0, 255)),
            "TrueBlack must not inherit the accent-tinted dock material"
        );
        assert_eq!(m.stroke, transparent());
        assert_eq!(m.sheen, transparent());
        assert_eq!(m.border_material, transparent());
        assert_eq!(m.shadow_opacity, 0.0);
        assert_eq!(
            background_for_opacity(SurfaceBackground::TrueBlack, 0.0),
            SurfaceBackground::TrueBlack
        );
    }

    #[test]
    fn user_border_override_wins_over_theme_stroke() {
        let state = ThemeState::default();
        let style = SurfaceStyle::new(
            SurfaceBackground::Glass,
            0.9,
            3.0,
            Some(Rgba::from_rgb(9, 8, 7)),
        );
        let m = state.build_with_style(SurfaceKind::Dock, style);
        assert_eq!(
            m.border_material,
            TokenValue::Solid(Rgba::from_rgb(9, 8, 7))
        );
        assert_eq!(m.border_thickness, 3.0);
    }

    #[test]
    fn editor_token_overrides_reach_the_real_palette_and_clear() {
        let _guard = theme_test_guard();
        ThemeManager::apply(AppTheme::Dark, None);
        apply_token_overrides(TokenMap::new());
        let baseline = ThemeManager::resolve("AccentBrush");
        let mut values = TokenMap::new();
        values.insert(
            "AccentBrush".to_owned(),
            TokenValue::Solid(Rgba::from_rgb(1, 2, 3)),
        );
        apply_token_overrides(values);
        assert_eq!(
            ThemeManager::resolve("AccentBrush"),
            Some(TokenValue::Solid(Rgba::from_rgb(1, 2, 3)))
        );
        let generation = ThemeManager::generation();
        apply_token_overrides(TokenMap::new());
        assert_eq!(ThemeManager::resolve("AccentBrush"), baseline);
        // Other tests share the global generation counter; only monotonicity is stable.
        assert!(ThemeManager::generation() >= generation);
    }

    #[test]
    fn saved_json_tokens_parse_to_solids_doubles_and_gradients() {
        use serde_json::{Map, json};
        let mut map = Map::new();
        map.insert("AccentBrush".to_owned(), json!("#102030"));
        map.insert("MenuOpacity".to_owned(), json!(0.75));
        map.insert(
            "DockMaterial".to_owned(),
            json!({"stops": ["#000000", "#FFFFFF"], "angle": 90}),
        );
        let tokens = token_overrides_from_json(&map);
        assert_eq!(
            tokens.get("AccentBrush"),
            Some(&TokenValue::Solid(Rgba::from_rgb(0x10, 0x20, 0x30)))
        );
        assert_eq!(tokens.get("MenuOpacity"), Some(&TokenValue::Double(0.75)));
        let Some(TokenValue::Gradient(g)) = tokens.get("DockMaterial") else {
            panic!("expected gradient")
        };
        assert_eq!(g.stops.len(), 2);
        assert_eq!(g.stops[0].position, 0.0);
        assert_eq!(g.stops[1].position, 1.0);
    }

    #[test]
    fn live_preview_emits_events_and_settings_opacity_is_clamped() {
        let _guard = theme_test_guard();
        ThemeManager::apply(AppTheme::Dark, None);
        let rx = ThemeManager::subscribe();
        assert!(
            ThemeManager::set_surface(SurfaceKind::Menu, SurfaceBackground::Solid, 0.7).is_ok()
        );
        assert_eq!(
            ThemeManager::get_surface(SurfaceKind::Menu).background,
            SurfaceBackground::Solid
        );
        assert!(rx.try_recv().is_ok());
        assert!(ThemeManager::set_surface_opacity(f64::NAN).is_err());
        assert!(ThemeManager::set_settings_opacity(0.2).is_err());
        assert!(ThemeManager::set_border(SurfaceKind::Widget, 13.0, None).is_err());
    }

    #[test]
    fn background_mode_is_derived_from_opacity() {
        assert_eq!(
            background_for_opacity(SurfaceBackground::Glass, 0.0),
            SurfaceBackground::None
        );
        assert_eq!(
            background_for_opacity(SurfaceBackground::Glass, 0.5),
            SurfaceBackground::Glass
        );
        assert_eq!(
            background_for_opacity(SurfaceBackground::Glass, 1.0),
            SurfaceBackground::Solid
        );
        assert_eq!(
            background_for_opacity(SurfaceBackground::Glass, f64::NAN),
            SurfaceBackground::None
        );
        assert_eq!(
            background_for_opacity(SurfaceBackground::Solid, 0.5),
            SurfaceBackground::Solid
        );
        assert_eq!(
            background_for_opacity(SurfaceBackground::None, 0.5),
            SurfaceBackground::None
        );
        assert_eq!(clamp_opacity(-1.0), 0.0);
        assert_eq!(clamp_opacity(2.0), 1.0);
    }

    #[test]
    fn per_surface_opacity_wins_over_global_fallback_and_survives_reapply() {
        let _guard = theme_test_guard();
        ThemeManager::clear_opacity_overrides();
        ThemeManager::apply(AppTheme::Dark, None);

        // Four independent surfaces; a change to one never touches the others.
        assert!(set_opacity(SurfaceKind::Dock, 0.55).is_ok());
        assert!(set_opacity(SurfaceKind::Menu, 0.70).is_ok());
        assert!(ThemeManager::set_opacity(SurfaceKind::Widget, 0.80).is_ok());
        assert!(set_opacity(SurfaceKind::Settings, 0.65).is_ok());
        assert_eq!(opacity(SurfaceKind::Dock), 0.55);
        assert_eq!(opacity(SurfaceKind::Menu), 0.70);
        assert_eq!(ThemeManager::opacity(SurfaceKind::Widget), 0.80);
        assert_eq!(
            ThemeManager::get_surface(SurfaceKind::Settings).opacity,
            0.65
        );
        assert!(set_opacity(SurfaceKind::Dock, f64::NAN).is_err());
        assert!(set_opacity(SurfaceKind::Menu, 1.2).is_err());

        // Legacy global fallback applies only where no per-kind value is usable.
        ThemeManager::clear_opacity_overrides();
        let settings = DockSettings {
            surface_opacity: 0.75,
            menu_opacity: f64::NAN,
            ..DockSettings::default()
        };
        ThemeManager::apply(AppTheme::Dark, Some(&settings));
        assert_eq!(opacity(SurfaceKind::Menu), 0.75);

        // Explicit per-surface values beat both typed settings and the global key.
        assert!(set_opacity(SurfaceKind::Menu, 0.60).is_ok());
        assert!(set_opacity(SurfaceKind::Settings, 0.85).is_ok());
        ThemeManager::apply(AppTheme::Dark, Some(&settings));
        assert_eq!(opacity(SurfaceKind::Menu), 0.60);
        assert_eq!(opacity(SurfaceKind::Settings), 0.85);

        // The backdrop mode follows the opacity API, not a separate toggle.
        assert_eq!(
            ThemeManager::effective_background(SurfaceKind::Menu),
            SurfaceBackground::Glass
        );
        assert!(set_opacity(SurfaceKind::Menu, 1.0).is_ok());
        assert_eq!(
            ThemeManager::effective_background(SurfaceKind::Menu),
            SurfaceBackground::Solid
        );
        assert!(set_opacity(SurfaceKind::Menu, 0.0).is_ok());
        assert_eq!(
            ThemeManager::effective_background(SurfaceKind::Menu),
            SurfaceBackground::None
        );
        ThemeManager::clear_opacity_overrides();
    }

    #[test]
    fn pack_tokens_reach_the_live_palette_and_materials() {
        let _guard = theme_test_guard();
        ThemeManager::apply(AppTheme::Dark, None);
        let mut pack = TokenMap::new();
        pack.insert(
            "AccentBrush".to_owned(),
            TokenValue::Solid(Rgba::from_rgb(0x12, 0x34, 0x56)),
        );
        pack.insert(
            "WidgetRadius".to_owned(),
            TokenValue::CornerRadius(CornerRadius::uniform(7)),
        );
        apply_pack_tokens_to_palette(&pack);
        assert_eq!(
            ThemeManager::resolve("AccentBrush"),
            Some(TokenValue::Solid(Rgba::from_rgb(0x12, 0x34, 0x56)))
        );
        let materials = ThemeManager::materials(SurfaceKind::Widget);
        assert_eq!(
            materials.tokens.get("AccentBrush"),
            Some(&TokenValue::Solid(Rgba::from_rgb(0x12, 0x34, 0x56)))
        );
        assert_eq!(materials.tokens.len(), editor_token_count());
        assert_eq!(ThemeManager::radius("WidgetRadius"), Some(7));
        assert_eq!(
            materials.radii.get("WidgetRadius"),
            Some(&CornerRadius::uniform(7))
        );
        apply_pack_tokens_to_palette(TokenMap::new());
        apply_token_overrides(TokenMap::new());
    }

    #[test]
    fn editor_tokens_accept_short_hex_named_colors_and_radial_brushes() {
        let _guard = theme_test_guard();
        use serde_json::{Map, json};
        let mut map = Map::new();
        map.insert("AccentBrush".into(), json!("#0af"));
        map.insert("TextPrimary".into(), json!("Transparent"));
        map.insert(
            "IconHoverGlow".into(),
            json!({
                "type": "radial",
                "stops": ["#fff", "#0000"],
                "center": [0.5, 0.75],
                "origin": [0.5, 0.8],
                "radius": [0.6, 0.6],
            }),
        );
        map.insert("UnknownToken".into(), json!("#ffffff"));
        let tokens = token_overrides_from_json(&map);
        assert_eq!(
            tokens.get("AccentBrush"),
            Some(&TokenValue::Solid(Rgba::from_rgb(0, 0xAA, 0xFF)))
        );
        assert_eq!(
            tokens.get("TextPrimary"),
            Some(&TokenValue::Solid(Rgba::new(0, 0, 0, 0)))
        );
        assert!(matches!(
            tokens.get("IconHoverGlow"),
            Some(TokenValue::RadialGradient(_))
        ));
        assert!(!tokens.contains_key("UnknownToken"));
        apply_token_overrides(tokens);
        assert!(matches!(
            ThemeManager::resolve("IconHoverGlow"),
            Some(TokenValue::RadialGradient(_))
        ));
        apply_token_overrides(TokenMap::new());
    }
}
