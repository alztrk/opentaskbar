//! C# parity for `Services/Settings/ThemeManager.cs`.
//!
//! Owns four independently styled surfaces (dock / menu / widget / settings). Override layers
//! are applied in order **base palette < Windows accent < theme pack**, so an explicitly selected
//! pack wins for the keys it defines. Rebuilding a surface emits `ThemeChanged`.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};

use opentaskbar_core::settings::{AppTheme, DockSettings, SurfaceBackground};
use serde_json::Value;

use super::brush::GradientSpec;
use super::palette;
use super::tokens::{Gradient, Rgba, TokenKind, TokenMap, TokenValue};
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
        };
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
        let border_material = if style.background == SurfaceBackground::None {
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
            sidebar = Some(match style.background {
                SurfaceBackground::None => transparent(),
                SurfaceBackground::Solid => source.with_alpha(255),
                SurfaceBackground::Glass => source.with_alpha(scaled_alpha(style.opacity)),
            });
            content = Some(match style.background {
                SurfaceBackground::Solid => self.brush("SettingsBackground").with_alpha(255),
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
        let scale = if style.background == SurfaceBackground::Glass {
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
            shadow_opacity: if style.background == SurfaceBackground::None {
                0.0
            } else {
                0.45
            },
            sidebar,
            content,
            hover: hover_source.scaled_alpha(scale),
            pressed: pressed_source.scaled_alpha(scale),
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

fn apply_fill(source: TokenValue, style: SurfaceStyle) -> TokenValue {
    match style.background {
        SurfaceBackground::None => transparent(),
        SurfaceBackground::Solid => source.with_alpha(255),
        SurfaceBackground::Glass => source.with_alpha(scaled_alpha(style.opacity)),
    }
}

fn apply_stroke(source: TokenValue, style: SurfaceStyle) -> TokenValue {
    if style.background == SurfaceBackground::None {
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
    map
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
            for kind in SurfaceKind::ALL {
                let style = settings
                    .map(|s| style_from_settings(s, kind))
                    .unwrap_or_default();
                state.styles.insert(kind, style);
            }
            state.legacy_opacity = state.style(SurfaceKind::Dock).opacity;
            state.settings_opacity = state.style(SurfaceKind::Settings).opacity;
            state.initialized = true;
            state.generation = state.generation.wrapping_add(1);
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
            state.pack = values;
            if state
                .pack
                .get("DockBackgroundMode")
                .and_then(|v| match v {
                    TokenValue::Mode(m) => Some(m == "None"),
                    _ => None,
                })
                .unwrap_or(false)
            {
                state
                    .pack
                    .insert("DockBorderMaterial".into(), transparent());
                state.pack.insert("DockHighlight".into(), transparent());
            }
            state.generation = state.generation.wrapping_add(1);
            state.rebuild_all();
        }
        Self::notify_all();
    }

    pub fn get_surface(kind: SurfaceKind) -> SurfaceStyle {
        lock().style(kind)
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

    /// Settings window opacity (clamped to 0.6..1 in C# `SetSettingsOpacity`).
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
            && SurfaceKind::ALL
                .iter()
                .all(|kind| state.style(*kind) == style_from_settings(settings, *kind))
    }

    /// Layered token lookup (theme pack > Windows > base palette).
    pub fn resolve(key: &str) -> Option<TokenValue> {
        lock().resolve(key).cloned()
    }

    /// Materials produced for a surface by the latest rebuild.
    pub fn materials(kind: SurfaceKind) -> SurfaceMaterials {
        lock().generated.get(&kind).cloned().unwrap_or_default()
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

/// Applies the theme editor's saved tokens to the **real** palette.
///
/// Editor tokens live above the theme-pack layer, so every surface rebuilt from
/// `ThemeManager` (dock / menu / widget / settings) observes them. An empty map
/// clears the editor layer and restores the previous palette + pack values.
pub fn apply_token_overrides(values: TokenMap) {
    {
        let mut state = lock();
        state.overrides = values;
        state.generation = state.generation.wrapping_add(1);
        state.rebuild_all();
    }
    ThemeManager::notify_all();
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
        let token = match value {
            Value::String(text) => {
                let text = text.trim();
                if let Some(color) = Rgba::from_hex(text) {
                    match TokenKind::for_key(key) {
                        TokenKind::Color => Some(TokenValue::Color(color)),
                        TokenKind::Double => Some(TokenValue::Double(color.luminance())),
                        _ => Some(TokenValue::Solid(color)),
                    }
                } else {
                    match TokenKind::for_key(key) {
                        TokenKind::Double => text.parse::<f64>().ok().map(TokenValue::Double),
                        TokenKind::FontFamily => Some(TokenValue::FontFamily(text.to_owned())),
                        _ => Some(TokenValue::Mode(text.to_owned())),
                    }
                }
            }
            Value::Number(number) => number.as_f64().map(TokenValue::Double),
            Value::Object(object) => {
                let stops: Vec<Rgba> = object
                    .get("stops")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().and_then(Rgba::from_hex))
                            .collect()
                    })
                    .unwrap_or_default();
                if stops.is_empty() {
                    None
                } else {
                    let angle = object.get("angle").and_then(Value::as_f64).unwrap_or(90.0);
                    Some(TokenValue::Gradient(
                        GradientSpec::new(stops, angle).to_gradient(),
                    ))
                }
            }
            _ => None,
        };
        if let Some(token) = token {
            out.insert(key.clone(), token);
        }
    }
    out
}

fn style_from_settings(settings: &DockSettings, kind: SurfaceKind) -> SurfaceStyle {
    let background = |name: &str| -> SurfaceBackground {
        match settings.extra.get(name).and_then(|v| v.as_str()) {
            Some("Solid") => SurfaceBackground::Solid,
            Some("None") => SurfaceBackground::None,
            _ => SurfaceBackground::Glass,
        }
    };
    let number = |name: &str, fallback: f64| -> f64 {
        settings
            .extra
            .get(name)
            .and_then(|v| v.as_f64())
            .unwrap_or(fallback)
    };
    let color = |name: &str| -> Option<Rgba> {
        settings
            .extra
            .get(name)
            .and_then(|v| v.as_str())
            .and_then(Rgba::from_hex)
    };
    match kind {
        SurfaceKind::Dock => SurfaceStyle::new(
            settings.dock_background,
            settings.dock_opacity,
            settings.dock_border_thickness,
            color("DockBorderColor"),
        ),
        SurfaceKind::Menu => SurfaceStyle::new(
            background("MenuBackground"),
            number("MenuOpacity", 0.9),
            number("MenuBorderThickness", 1.0),
            color("MenuBorderColor"),
        ),
        SurfaceKind::Widget => SurfaceStyle::new(
            background("WidgetBackground"),
            number("WidgetOpacity", 0.9),
            number("WidgetBorderThickness", 1.0),
            color("WidgetBorderColor"),
        ),
        SurfaceKind::Settings => SurfaceStyle::new(
            background("SettingsBackground"),
            number("SettingsOpacity", 0.95),
            number("SettingsBorderThickness", 1.0),
            color("SettingsBorderColor"),
        ),
    }
}

/// Helper for consumers that need to convert a token to the C#-style mode name.
pub fn mode_name(value: &TokenValue) -> Option<&str> {
    match value {
        TokenValue::Mode(mode) => Some(mode),
        _ => None,
    }
}

/// Parses a `#RRGGBB` / `#AARRGGBB` user border color with the same rules as C#.
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
        };
        clone.styles.insert(kind, style);
        clone.build(kind)
    }
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
            if mode != SurfaceBackground::None {
                assert_eq!((color.r, color.g, color.b), (10, 20, 30));
            }
        }
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
}
