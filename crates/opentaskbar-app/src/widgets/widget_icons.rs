//! Widget icon resolution.
//!
//! An icon is data, not code. Resolution order (first hit wins):
//! 1. per-widget override in `settings.json` (`CustomWidgetIcons`),
//! 2. the active theme's `widgets/icons/pack.json`,
//! 3. an `icon.png` bundled in the widget's own data folder,
//! 4. the caller's default glyph.
//!
//! File I/O is cached so the 144 Hz dock render path never touches the disk.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::SystemTime;

use opentaskbar_core::paths;
use opentaskbar_core::settings::DockSettings;
use serde_json::Value;

use crate::design::widget_pack::parse_json_lenient;
use crate::dock::dcomp::WidgetIcon;

use super::catalog_ops::setting_value;

const MAX_ICON_DIMENSION: u32 = 256;
const MAX_DECODE_CACHE: usize = 64;
const MAX_MANIFEST_BYTES: u64 = 128 * 1024;

struct CachedIcon {
    fingerprint: String,
    icon: WidgetIcon,
}

static ICON_CACHE: LazyLock<RwLock<HashMap<String, CachedIcon>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

struct DecodedIcon {
    modified: SystemTime,
    pixels: Arc<(u32, u32, Vec<u8>)>,
}

static DECODE_CACHE: LazyLock<RwLock<HashMap<PathBuf, DecodedIcon>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Rejects ids that could escape their folder when used as a path component.
pub fn safe_id(id: &str) -> Option<&str> {
    let trimmed = id.trim();
    (!trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    .then_some(trimmed)
}

/// Resolves a relative path inside `base`, rejecting absolute paths and `..`.
fn inside_dir(base: &Path, relative: &str) -> Option<PathBuf> {
    let normalized = relative.replace('\\', "/");
    let rel = Path::new(&normalized);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let canonical_base = base.canonicalize().ok()?;
    let canonical = base.join(rel).canonicalize().ok()?;
    canonical.starts_with(&canonical_base).then_some(canonical)
}

/// An override string is either inline `StreamGeometry` path data or a PNG path.
fn icon_from_text(text: &str) -> Option<WidgetIcon> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('M') || trimmed.starts_with('m') {
        return Some(WidgetIcon::PathData(trimmed.to_owned()));
    }
    let path = PathBuf::from(trimmed);
    let is_file = path.is_file();
    is_file.then_some(WidgetIcon::Asset(path))
}

fn lookup_key<'a>(object: &'a Value, key_widget: &str, id: &str) -> Option<&'a str> {
    object
        .as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key_widget) || k.eq_ignore_ascii_case(id))
        .and_then(|(_, v)| v.as_str())
}

/// Reads an icon mapping from a theme's `widgets/icons/pack.json`. `pathData`
/// wins over image files so a monochrome, theme-tinted glyph stays crisp.
fn icon_from_manifest(value: &Value, id: &str, dir: &Path) -> Option<WidgetIcon> {
    let key_widget = format!("widget:{id}");
    if let Some(data) = value
        .get("pathData")
        .and_then(|v| lookup_key(v, &key_widget, id))
    {
        return Some(WidgetIcon::PathData(data.to_owned()));
    }
    if let Some(relative) = value
        .get("icons")
        .and_then(|v| lookup_key(v, &key_widget, id))
    {
        return inside_dir(dir, relative).map(WidgetIcon::Asset);
    }
    None
}

fn user_override(settings: &DockSettings, id: &str) -> Option<String> {
    setting_value(settings, "CustomWidgetIcons")?
        .as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(id))
        .and_then(|(_, v)| v.as_str())
        .map(str::to_owned)
}

fn resolve_uncached(id: &str, settings: &DockSettings) -> Option<WidgetIcon> {
    if let Some(text) = user_override(settings, id)
        && let Some(icon) = icon_from_text(&text)
    {
        return Some(icon);
    }

    let safe = safe_id(id)?;
    let theme = settings.theme_pack.trim();
    if !theme.is_empty() {
        let dir = paths::theme_root()
            .join(theme)
            .join("widgets")
            .join("icons");
        if let Ok(meta) = fs::metadata(dir.join("pack.json"))
            && meta.len() <= MAX_MANIFEST_BYTES
            && let Ok(text) = fs::read_to_string(dir.join("pack.json"))
            && let Ok(value) = parse_json_lenient(&text)
            && let Some(icon) = icon_from_manifest(&value, safe, &dir)
        {
            return Some(icon);
        }
        if let Some(path) = inside_dir(&dir, &format!("{safe}.png")) {
            return Some(WidgetIcon::Asset(path));
        }
    }

    let own = paths::shared_app_data_dir()
        .join("Widgets")
        .join(safe)
        .join("icon.png");
    let is_file = own.is_file();
    is_file.then_some(WidgetIcon::Asset(own))
}

/// Resolves the icon for `id`, falling back to `default`. Cached on the settings
/// fingerprint so repeated `chip_spec()` calls during animation stay allocation-light.
pub fn resolve_widget_icon(id: &str, settings: &DockSettings, default: WidgetIcon) -> WidgetIcon {
    let fingerprint = format!(
        "{}|{}",
        settings.theme_pack,
        user_override(settings, id).unwrap_or_default()
    );
    if let Ok(cache) = ICON_CACHE.read()
        && let Some(cached) = cache.get(id)
        && cached.fingerprint == fingerprint
    {
        return cached.icon.clone();
    }
    let icon = resolve_uncached(id, settings).unwrap_or(default);
    if let Ok(mut cache) = ICON_CACHE.write() {
        cache.insert(
            id.to_owned(),
            CachedIcon {
                fingerprint,
                icon: icon.clone(),
            },
        );
    }
    icon
}

/// Decodes a PNG into premultiplied BGRA32 pixels for Direct2D upload. Results
/// are cached by path + modification time.
pub fn load_bgra(path: &Path) -> Option<Arc<(u32, u32, Vec<u8>)>> {
    let modified = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    if let Ok(cache) = DECODE_CACHE.read()
        && let Some(cached) = cache.get(path)
        && cached.modified == modified
    {
        return Some(cached.pixels.clone());
    }

    let image = image::open(path).ok()?.to_rgba8();
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 || w > MAX_ICON_DIMENSION || h > MAX_ICON_DIMENSION {
        return None;
    }
    let mut bgra = Vec::with_capacity(w as usize * h as usize * 4);
    for pixel in image.pixels() {
        let [r, g, b, a] = pixel.0;
        bgra.extend_from_slice(&[
            (u16::from(b) * u16::from(a) / 255) as u8,
            (u16::from(g) * u16::from(a) / 255) as u8,
            (u16::from(r) * u16::from(a) / 255) as u8,
            a,
        ]);
    }
    let decoded = Arc::new((w, h, bgra));
    if let Ok(mut cache) = DECODE_CACHE.write() {
        if cache.len() >= MAX_DECODE_CACHE {
            cache.clear();
        }
        cache.insert(
            path.to_path_buf(),
            DecodedIcon {
                modified,
                pixels: decoded.clone(),
            },
        );
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_ids_reject_traversal() {
        assert_eq!(safe_id("usd-try"), Some("usd-try"));
        assert_eq!(safe_id(" Clock "), Some("Clock"));
        assert!(safe_id("../evil").is_none());
        assert!(safe_id("a/b").is_none());
        assert!(safe_id("").is_none());
    }

    #[test]
    fn relative_paths_cannot_escape_the_pack() {
        let base = std::env::temp_dir().join("opentaskbar-icon-test");
        fs::create_dir_all(base.join("icons")).unwrap();
        let file = base.join("icon.png");
        let nested = base.join("icons").join("icon.png");
        fs::write(&file, b"not-a-real-png").unwrap();
        fs::write(&nested, b"not-a-real-png").unwrap();
        assert_eq!(
            inside_dir(&base, "icon.png"),
            Some(fs::canonicalize(&file).unwrap())
        );
        assert_eq!(
            inside_dir(&base, "icons\\icon.png"),
            Some(fs::canonicalize(&nested).unwrap())
        );
        assert!(inside_dir(&base, "../secret.png").is_none());
        assert!(inside_dir(&base, "C:\\secret.png").is_none());
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn path_data_wins_over_file_mapping() {
        let dir = std::env::temp_dir().join("opentaskbar-icon-manifest-test");
        fs::create_dir_all(&dir).unwrap();
        let value: Value = serde_json::from_str(
            r#"{"icons":{"widget:clock":"icons/clock.png"},"pathData":{"widget:clock":"M0 0 L10 10 Z"}}"#,
        )
        .unwrap();
        assert_eq!(
            icon_from_manifest(&value, "clock", &dir),
            Some(WidgetIcon::PathData("M0 0 L10 10 Z".to_owned()))
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inline_override_supports_path_data_and_files() {
        assert!(matches!(
            icon_from_text("M0 0 L24 24 Z"),
            Some(WidgetIcon::PathData(_))
        ));
        assert!(icon_from_text("").is_none());
        assert!(icon_from_text("C:\\missing\\icon.png").is_none());
    }
}
