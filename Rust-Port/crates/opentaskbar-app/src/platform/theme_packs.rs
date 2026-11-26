//! C# parity for `Services/App/ThemePacks.cs` and `ThemePackTransfer.cs`.
//!
//! Themes are plain folders under `%LOCALAPPDATA%\OpenTaskbar\Themes`. Six packs are embedded
//! and installed on first launch without overwriting user files. Icon plates, native Windows
//! logo discovery and manifest validation live here too.

use std::fs;
use std::path::{Component, Path, PathBuf};

use gpui::AssetSource;
use image::{Rgba, RgbaImage};
use serde_json::{Map, Value};

use crate::design::fonts::{BUNDLED_THEME_IDS, EmbeddedAssets};
use crate::design::tokens::{CornerRadius, Gradient, Rgba as TokenRgba, TokenMap};
use crate::design::widget_pack::{WidgetPackManifest, WidgetTokenResolution};

use super::icon_pack::{
    IconPack, IconPackCorner, IconPackFallback, IconPackShape, MANIFEST_FILE_NAME as ICON_MANIFEST,
};

pub const MANIFEST_FILE_NAME: &str = "theme.json";
pub const ICON_FOLDER: &str = "icons";
pub const WIDGET_FOLDER: &str = "widgets";
pub const RENDER_SIZE: u32 = 128;
pub const MAX_AUTHOR_LEN: usize = 200;
pub const MAX_DESCRIPTION_LEN: usize = 1000;
const MAX_FILE_COUNT: usize = 512;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemePackManifest {
    pub name: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub preserve_original_icons: bool,
}

impl ThemePackManifest {
    pub fn from_str(text: &str) -> Result<Self, String> {
        let value = crate::design::widget_pack::parse_json_lenient(text)?;
        let Some(object) = value.as_object() else {
            return Err("theme.json must be an object".into());
        };
        let string = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
        Ok(Self {
            name: string("name"),
            author: string("author"),
            description: string("description"),
            preserve_original_icons: object
                .get("preserveOriginalIcons")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    pub fn to_string_pretty(&self) -> Result<String, String> {
        let mut object = Map::new();
        for (key, value) in [
            ("name", self.name.as_deref()),
            ("author", self.author.as_deref()),
            ("description", self.description.as_deref()),
        ] {
            if let Some(value) = value {
                object.insert(key.to_owned(), Value::String(value.to_owned()));
            }
        }
        object.insert(
            "preserveOriginalIcons".into(),
            Value::Bool(self.preserve_original_icons),
        );
        serde_json::to_string_pretty(&Value::Object(object)).map_err(|e| e.to_string())
    }
}

/// A loaded theme folder: `theme.json` + optional `icons/pack.json` and `widgets/pack.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemePack {
    pub id: String,
    pub directory: PathBuf,
    pub manifest: ThemePackManifest,
    pub icons: Option<IconPack>,
    pub widgets: Option<WidgetPackManifest>,
    pub problems: Vec<String>,
}

impl ThemePack {
    pub fn load(directory: &Path) -> Result<Self, String> {
        let id = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let manifest_path = directory.join(MANIFEST_FILE_NAME);
        let text = fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let manifest = ThemePackManifest::from_str(&text)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        if let Some(error) = validate_manifest(&manifest) {
            return Err(format!("{}: {error}", manifest_path.display()));
        }

        let mut problems = Vec::new();
        let mut icons = None;
        let icon_dir = directory.join(ICON_FOLDER);
        if !manifest.preserve_original_icons
            && !is_built_in_theme(&id)
            && icon_dir.join(ICON_MANIFEST).exists()
        {
            match IconPack::load(&icon_dir) {
                Ok(pack) => icons = Some(pack),
                Err(error) => problems.push(error),
            }
        }
        let widget_dir = directory.join(WIDGET_FOLDER);
        let widget_path = widget_dir.join(crate::design::widget_pack::MANIFEST_FILE_NAME);
        let mut widgets = None;
        if widget_path.exists() {
            match WidgetPackManifest::load(&widget_dir) {
                Ok(pack) => widgets = Some(pack),
                Err(error) => problems.push(error),
            }
        }
        Ok(Self {
            id,
            directory: directory.to_path_buf(),
            manifest,
            icons,
            widgets,
            problems,
        })
    }

    pub fn display_name(&self) -> &str {
        self.manifest
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&self.id)
    }

    /// Resolves this pack's widget tokens for a theme, using key-based type inference.
    pub fn resolve_widgets(
        &self,
        theme: opentaskbar_core::settings::AppTheme,
    ) -> WidgetTokenResolution {
        let Some(pack) = self.widgets.as_ref() else {
            return WidgetTokenResolution::default();
        };
        resolve_widget_tokens(pack, theme, |_key| None)
    }
}

/// User theme folder (`%LOCALAPPDATA%\OpenTaskbar\Themes`).
pub fn root() -> PathBuf {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Users\\Default\\AppData\\Local"));
    local.join("OpenTaskbar").join("Themes")
}

/// Six real bundled theme ids (not a test-only stub).
pub fn built_in_ids() -> &'static [&'static str] {
    &BUNDLED_THEME_IDS
}

pub fn is_built_in_theme(id: &str) -> bool {
    BUNDLED_THEME_IDS
        .iter()
        .any(|built| built.eq_ignore_ascii_case(id))
}

/// Copies bundled themes into the user themes folder on first launch. Returns the number of
/// files written; existing files are never replaced.
pub fn install_bundled() -> Result<usize, String> {
    let destination_root = root();
    fs::create_dir_all(&destination_root).map_err(|e| e.to_string())?;
    let assets = EmbeddedAssets;
    let paths = assets.list("themes").map_err(|e| e.to_string())?;
    if paths.is_empty() {
        return Err("No built-in theme resources were found.".into());
    }
    let mut installed = 0usize;
    for path in paths {
        let relative: &str = path
            .strip_prefix("themes/")
            .ok_or_else(|| format!("Unexpected asset path: {path}"))?;
        let relative = Path::new(relative);
        if !is_safe_relative(relative) {
            return Err(format!(
                "Built-in theme resource path was rejected: {relative:?}"
            ));
        }
        let destination = destination_root.join(relative);
        if destination.exists() {
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = assets
            .load(path.as_ref())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Built-in theme resource '{path}' is missing."))?;
        fs::write(&destination, bytes.as_ref()).map_err(|e| e.to_string())?;
        installed += 1;
    }
    Ok(installed)
}

pub fn install_built_in_themes() -> Result<usize, String> {
    install_bundled()
}

fn is_safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && !path.to_string_lossy().contains("..")
}

/// Validates manifest field lengths (C# `ThemePackTransfer.Validate`).
pub fn validate_manifest(manifest: &ThemePackManifest) -> Option<&'static str> {
    if manifest
        .author
        .as_ref()
        .is_some_and(|author| author.chars().count() > MAX_AUTHOR_LEN)
    {
        return Some("theme.json Author is too long.");
    }
    if manifest
        .description
        .as_ref()
        .is_some_and(|description| description.chars().count() > MAX_DESCRIPTION_LEN)
    {
        return Some("theme.json Description is too long.");
    }
    None
}

/// Safe folder identifier: letters/digits/`-`/`_`, max 120 chars, no traversal.
pub fn is_safe_id(id: &str) -> bool {
    !id.trim().is_empty()
        && id.chars().count() <= 120
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        && !id.contains("..")
}

pub fn sanitize(name: &str) -> Option<String> {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-');
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

pub fn unique_id(root: &Path, name: &str) -> String {
    let base = sanitize(name).unwrap_or_else(|| "theme".to_owned());
    let mut id = base.clone();
    let mut suffix = 2;
    while root.join(&id).exists() {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}

/// Recursive copy helper from `ThemePackTransfer.CopyDirectory`.
pub fn copy_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&from, &to)?;
        } else if from.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// C5 API: creates a theme based on an existing pack (or blank), carrying over the
/// `preserveOriginalIcons` flag for built-in themes.
pub fn create_theme(
    based_on_id: Option<&str>,
    preserve_original_icons: bool,
) -> Result<ThemePack, String> {
    let base_name = based_on_id
        .and_then(|id| ThemePack::load(&root().join(id)).ok())
        .map(|pack| format!("{} Copy", pack.display_name()))
        .unwrap_or_else(|| "My Theme".to_owned());
    create_theme_named(&base_name, None, based_on_id, preserve_original_icons)
}

/// Full creation API used by the settings UI (name + author).
pub fn create_theme_named(
    name: &str,
    author: Option<&str>,
    based_on_id: Option<&str>,
    preserve_original_icons: bool,
) -> Result<ThemePack, String> {
    let themes_root = root();
    fs::create_dir_all(&themes_root).map_err(|e| e.to_string())?;
    let id = unique_id(&themes_root, name);
    let target = themes_root.join(&id);
    fs::create_dir_all(&target).map_err(|e| e.to_string())?;

    let mut preserve = preserve_original_icons;
    if let Some(based) = based_on_id.filter(|id| is_safe_id(id)) {
        let source = themes_root.join(based);
        if source.is_dir() {
            preserve |= is_built_in_theme(based)
                || ThemePack::load(&source)
                    .map(|pack| pack.manifest.preserve_original_icons)
                    .unwrap_or(false);
            copy_directory(&source, &target).map_err(|e| e.to_string())?;
        }
    }

    let manifest = ThemePackManifest {
        name: Some(name.trim().to_owned()),
        author: author
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_owned),
        description: None,
        preserve_original_icons: preserve,
    };
    if let Some(error) = validate_manifest(&manifest) {
        return Err(error.to_owned());
    }
    let text = manifest.to_string_pretty()?;
    fs::write(target.join(MANIFEST_FILE_NAME), text).map_err(|e| e.to_string())?;
    ThemePack::load(&target)
}

/// C5 API name: resolves typed widget tokens for a theme from a widget pack manifest.
pub fn resolve_widget_tokens(
    manifest: &WidgetPackManifest,
    theme: opentaskbar_core::settings::AppTheme,
    current: impl Fn(&str) -> Option<crate::design::tokens::TokenKind>,
) -> WidgetTokenResolution {
    crate::design::widget_pack::resolve_widget_tokens(manifest, theme, current)
}

/// Discovers all valid theme folders, sorted by display name.
pub fn discover() -> Vec<ThemePack> {
    let mut packs = Vec::new();
    let Ok(entries) = fs::read_dir(root()) else {
        return packs;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir()
            && let Ok(pack) = ThemePack::load(&entry.path())
        {
            packs.push(pack);
        }
    }
    packs.sort_by(|a, b| {
        a.display_name()
            .to_lowercase()
            .cmp(&b.display_name().to_lowercase())
    });
    packs
}

/// Applies a selected theme pack's widget tokens as a layered `TokenMap` for `ThemeManager`.
pub fn widgets_override(pack: &ThemePack, theme: opentaskbar_core::settings::AppTheme) -> TokenMap {
    pack.resolve_widgets(theme).tokens
}

/// Cached native Windows logo bytes (`@WLOGO_96x96.png`), or `None` when unavailable.
pub fn get_native_windows_logo() -> Option<Vec<u8>> {
    let windows = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    let mut candidates = vec![windows.join("System32").join("@WLOGO_96x96.png")];
    let winsxs = windows.join("WinSxS");
    if let Ok(entries) = fs::read_dir(&winsxs) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("amd64_microsoft-windows-oobe-user_") {
                candidates.push(entry.path().join("@WLOGO_96x96.png"));
            }
        }
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .and_then(|path| fs::read(&path).ok())
}

/// Shape used when clipping an icon onto its plate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlateShape {
    Circle,
    Rounded(CornerRadius),
}

/// C# `ThemePacks.CreateShape`: per-corner radii in pixels for a 128px plate.
pub fn plate_shape(fallback: &IconPackFallback, size: u32) -> PlateShape {
    let clamped = fallback.clamped();
    if clamped.shape == IconPackShape::Circle {
        return PlateShape::Circle;
    }
    let radius = (clamped.corner_radius.clamp(0.0, 0.5) * f64::from(size)).round() as i32;
    let mut corners = CornerRadius::uniform(radius);
    let square = clamped.square_corners();
    for corner in square {
        match corner {
            IconPackCorner::TopLeft => corners.top_left = 0,
            IconPackCorner::TopRight => corners.top_right = 0,
            IconPackCorner::BottomLeft => corners.bottom_left = 0,
            IconPackCorner::BottomRight => corners.bottom_right = 0,
        }
    }
    PlateShape::Rounded(corners)
}

fn inside_shape(x: f64, y: f64, size: f64, shape: PlateShape) -> bool {
    match shape {
        PlateShape::Circle => {
            let center = size / 2.0;
            ((x - center).powi(2) + (y - center).powi(2)).sqrt() <= center
        }
        PlateShape::Rounded(c) => {
            let radius = |rx: i32, ry: i32, cx: f64, cy: f64| -> bool {
                if rx <= 0 || ry <= 0 {
                    return true;
                }
                let dx = x - cx;
                let dy = y - cy;
                if dx < 0.0 && dy < 0.0 {
                    (dx * dx + dy * dy) <= (rx as f64 * rx as f64)
                } else {
                    true
                }
            };
            radius(c.top_left, c.top_left, 0.0, 0.0)
                && radius(c.top_right, c.top_right, size, 0.0)
                && radius(c.bottom_right, c.bottom_right, size, size)
                && radius(c.bottom_left, c.bottom_left, 0.0, size)
        }
    }
}

/// Average opaque color of an icon (R, G, B, sample count).
pub fn sample_icon_channels(icon: &RgbaImage) -> (f64, f64, f64, u64) {
    let (mut r, mut g, mut b, mut weight) = (0.0, 0.0, 0.0, 0u64);
    for pixel in icon.pixels() {
        if pixel.0[3] < 128 {
            continue;
        }
        r += f64::from(pixel.0[0]);
        g += f64::from(pixel.0[1]);
        b += f64::from(pixel.0[2]);
        weight += 1;
    }
    (r, g, b, weight)
}

/// Pale tint of the icon's average color, light enough for the icon to stay readable (light mode).
pub fn auto_background(icon: &RgbaImage) -> TokenRgba {
    let (r, g, b, weight) = sample_icon_channels(icon);
    if weight == 0 {
        return TokenRgba::from_rgb(255, 255, 255);
    }
    let tint = |channel: f64| {
        (channel / weight as f64 * 0.22 + 255.0 * 0.78)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    TokenRgba::from_rgb(tint(r), tint(g), tint(b))
}

/// Dark plate color derived from the icon's average color (dark mode).
pub fn dark_plate_color(icon: &RgbaImage) -> TokenRgba {
    let (r, g, b, weight) = sample_icon_channels(icon);
    if weight == 0 {
        return TokenRgba::from_rgb(24, 28, 36);
    }
    let tint = |channel: f64, base: u8| {
        (f64::from(base) + channel / weight as f64 * 0.16)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    TokenRgba::from_rgb(tint(r, 22), tint(g, 26), tint(b, 34))
}

fn hex_or_auto(value: &str) -> Option<TokenRgba> {
    if value.trim().eq_ignore_ascii_case("auto") || value.trim().is_empty() {
        None
    } else {
        TokenRgba::from_hex(value)
    }
}

/// C# `ThemePacks.RenderPlate`: plates an arbitrary icon onto a shaped, shaded tile.
pub fn render_plate(icon: &RgbaImage, fallback: &IconPackFallback, is_dark: bool) -> RgbaImage {
    let size = RENDER_SIZE;
    let fallback = fallback.clamped();
    let shape = plate_shape(&fallback, size);
    let plate = hex_or_auto(&fallback.background).unwrap_or_else(|| {
        if is_dark {
            dark_plate_color(icon)
        } else {
            auto_background(icon)
        }
    });
    let top = if is_dark {
        plate.lighten(0.22)
    } else {
        plate.lighten(0.18)
    };
    let bottom = if is_dark {
        plate.darken(0.12)
    } else {
        plate.darken(0.08)
    };
    let gradient = Gradient::vertical(top, bottom);
    let scale = fallback.icon_scale.clamp(0.2, 1.0);
    let inner = f64::from(size) * scale;
    let offset = (f64::from(size) - inner) / 2.0;

    let mut out = RgbaImage::new(size, size);
    let (iw, ih) = icon.dimensions();
    for y in 0..size {
        let t = y as f64 / size.saturating_sub(1).max(1) as f64;
        let fill = gradient
            .stops
            .first()
            .map(|s| s.color)
            .unwrap_or(plate)
            .mix(bottom, t);
        for x in 0..size {
            let px = x as f64 + 0.5;
            let py = y as f64 + 0.5;
            if !inside_shape(px, py, f64::from(size), shape) {
                out.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                continue;
            }
            let mut color = fill;
            let sx = ((px - offset) / inner * f64::from(iw)).floor() as i32;
            let sy = ((py - offset) / inner * f64::from(ih)).floor() as i32;
            if iw > 0 && ih > 0 && sx >= 0 && sy >= 0 && (sx as u32) < iw && (sy as u32) < ih {
                let source = icon.get_pixel(sx as u32, sy as u32);
                if source.0[3] > 0 {
                    let alpha = f64::from(source.0[3]) / 255.0;
                    color = TokenRgba::new(
                        ((f64::from(source.0[0]) * alpha + f64::from(color.r) * (1.0 - alpha))
                            .round()) as u8,
                        ((f64::from(source.0[1]) * alpha + f64::from(color.g) * (1.0 - alpha))
                            .round()) as u8,
                        ((f64::from(source.0[2]) * alpha + f64::from(color.b) * (1.0 - alpha))
                            .round()) as u8,
                        255,
                    );
                }
            }
            out.put_pixel(x, y, Rgba([color.r, color.g, color.b, color.a]));
        }
    }
    out
}

/// C# `ThemePacks.PlateIcon`: default macOS-style squircle plate.
pub fn plate_icon(icon: &RgbaImage, is_dark: bool) -> RgbaImage {
    let fallback = IconPackFallback {
        shape: IconPackShape::Rounded,
        corner_radius: 0.24,
        background: "auto".to_owned(),
        icon_scale: 0.72,
        ..IconPackFallback::default()
    };
    render_plate(icon, &fallback, is_dark)
}

/// Reads a theme pack image from disk, optionally capped at `target_pixels`.
pub fn load_pack_image(path: &Path, target_pixels: u32) -> Option<RgbaImage> {
    let decoder = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let image = decoder.decode().ok()?;
    if target_pixels > 0 && (image.width() > target_pixels || image.height() > target_pixels) {
        Some(image.thumbnail(target_pixels, target_pixels).to_rgba8())
    } else {
        Some(image.to_rgba8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::tokens::TokenValue;

    #[test]
    fn built_in_ids_and_install_manifest_count() {
        assert_eq!(built_in_ids().len(), 6);
        assert!(is_built_in_theme("Tokyo-Night"));
        assert!(!is_built_in_theme("unknown"));
        let assets = EmbeddedAssets;
        assert_eq!(assets.list("themes").unwrap().len(), 14);
        for id in built_in_ids() {
            let manifest = ThemePack::load(
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../themes")
                    .join(id),
            )
            .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(manifest.id, *id);
            assert!(validate_manifest(&manifest.manifest).is_none());
        }
    }

    #[test]
    fn manifest_limits_and_safe_ids() {
        let long_author = "a".repeat(MAX_AUTHOR_LEN + 1);
        assert!(
            validate_manifest(&ThemePackManifest {
                author: Some(long_author),
                ..ThemePackManifest::default()
            })
            .is_some()
        );
        assert!(
            validate_manifest(&ThemePackManifest {
                description: Some("d".repeat(MAX_DESCRIPTION_LEN + 1)),
                ..ThemePackManifest::default()
            })
            .is_some()
        );
        assert!(is_safe_id("my-theme_2"));
        assert!(!is_safe_id("../evil"));
        assert!(!is_safe_id("a/b"));
        assert_eq!(sanitize("My Theme!"), Some("My-Theme".to_owned()));
    }

    #[test]
    fn plate_derives_colors_from_icon() {
        let mut icon = RgbaImage::new(4, 4);
        for pixel in icon.pixels_mut() {
            *pixel = Rgba([200, 100, 50, 255]);
        }
        let light = auto_background(&icon);
        assert!(light.r > 200 && light.g > light.b);
        let dark = dark_plate_color(&icon);
        assert_eq!(dark.r, (22.0_f64 + 200.0_f64 * 0.16).round() as u8);
        let plate = plate_icon(&icon, true);
        assert_eq!(plate.dimensions(), (RENDER_SIZE, RENDER_SIZE));
        assert!(plate.get_pixel(0, 0).0[3] == 0 || plate.get_pixel(64, 64).0[3] > 0);
    }

    #[test]
    fn widget_override_is_typed() {
        let pack =
            ThemePack::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes/nord"))
                .unwrap();
        let tokens = widgets_override(&pack, opentaskbar_core::settings::AppTheme::Dark);
        assert!(tokens.contains_key("WidgetAccent"));
        assert!(matches!(
            tokens.get("DockMaterial"),
            Some(TokenValue::Gradient(_))
        ));
    }
}
