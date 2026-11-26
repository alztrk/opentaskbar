//! C# parity for `Services/App/IconPack.cs`.
//!
//! `pack.json` at the root of an icon pack maps a key to an image path relative to the pack.
//! The fallback plate (C# `IconPackFallback`) now carries `CornerRadius`, `SharpCorner` and a
//! description, and clamps `IconScale` to the C# upper bound of 1.0.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

pub const MANIFEST_FILE_NAME: &str = "pack.json";
pub const START_KEY: &str = "@start";
pub const RECYCLE_BIN_KEY: &str = "@recyclebin";
pub const RECYCLE_BIN_FULL_KEY: &str = "@recyclebin-full";
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub const IMAGE_EXTENSIONS: [&str; 5] = [".png", ".ico", ".jpg", ".jpeg", ".bmp"];

/// The square corner of a `Leaf` plate, or the first square corner of a `Petal` plate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconPackCorner {
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

impl IconPackCorner {
    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "topleft" => Some(Self::TopLeft),
            "topright" => Some(Self::TopRight),
            "bottomleft" => Some(Self::BottomLeft),
            "bottomright" => Some(Self::BottomRight),
            _ => None,
        }
    }
}

/// Plate shape applied to app icons the pack has no image for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconPackShape {
    #[default]
    None,
    Leaf,
    Petal,
    Rounded,
    Circle,
}

impl IconPackShape {
    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "leaf" => Some(Self::Leaf),
            "petal" => Some(Self::Petal),
            "rounded" => Some(Self::Rounded),
            "circle" => Some(Self::Circle),
            _ => None,
        }
    }
}

impl IconPackCorner {
    pub fn opposite(self) -> Self {
        match self {
            Self::TopLeft => Self::BottomRight,
            Self::TopRight => Self::BottomLeft,
            Self::BottomLeft => Self::TopRight,
            Self::BottomRight => Self::TopLeft,
        }
    }
}

/// C# `IconPackFallback` with defaults and clamping applied on read.
#[derive(Clone, Debug, PartialEq)]
pub struct IconPackFallback {
    pub shape: IconPackShape,
    pub sharp_corner: IconPackCorner,
    pub corner_radius: f64,
    /// Hex color or `"auto"` (derive from the icon).
    pub background: String,
    pub icon_scale: f64,
}

impl Default for IconPackFallback {
    fn default() -> Self {
        Self {
            shape: IconPackShape::None,
            sharp_corner: IconPackCorner::TopRight,
            corner_radius: 0.5,
            background: "auto".to_owned(),
            icon_scale: 0.64,
        }
    }
}

impl IconPackFallback {
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let Some(object) = value.as_object() else {
            return Err("Fallback must be an object".into());
        };
        let string = |key: &str| object.get(key).and_then(Value::as_str);
        let number =
            |key: &str, default: f64| object.get(key).and_then(Value::as_f64).unwrap_or(default);
        let shape = string("shape")
            .and_then(IconPackShape::parse)
            .unwrap_or_default();
        let sharp_corner = string("sharpCorner")
            .or_else(|| string("sharp_corner"))
            .and_then(IconPackCorner::parse)
            .unwrap_or_default();
        let fallback = Self::default();
        Ok(Self {
            shape,
            sharp_corner,
            corner_radius: number("cornerRadius", fallback.corner_radius),
            background: string("background").unwrap_or("auto").to_owned(),
            icon_scale: number("iconScale", fallback.icon_scale),
        })
    }

    /// Load-time normalization: radius 0..1, scale 0.1..1.0 (C# `IconPack.Fallback`).
    pub fn clamped(&self) -> Self {
        let finite = |v: f64, fallback: f64| if v.is_finite() { v } else { fallback };
        Self {
            corner_radius: finite(self.corner_radius, 0.5).clamp(0.0, 1.0),
            icon_scale: finite(self.icon_scale, 0.64).clamp(0.1, 1.0),
            ..self.clone()
        }
    }

    /// Corners kept square for the plate geometry.
    pub fn square_corners(&self) -> Vec<IconPackCorner> {
        match self.shape {
            IconPackShape::Leaf | IconPackShape::Petal => vec![self.sharp_corner],
            IconPackShape::None | IconPackShape::Rounded | IconPackShape::Circle => Vec::new(),
        }
        .into_iter()
        .chain((self.shape == IconPackShape::Petal).then_some(self.sharp_corner.opposite()))
        .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IconPackManifest {
    pub name: Option<String>,
    pub author: Option<String>,
    /// Free-form pack description (new in C# parity).
    pub description: Option<String>,
    pub icons: BTreeMap<String, String>,
    pub fallback: Option<IconPackFallback>,
}

impl IconPackManifest {
    pub fn from_str(text: &str) -> Result<Self, String> {
        let value = crate::design::widget_pack::parse_json_lenient(text)?;
        let Some(object) = value.as_object() else {
            return Err("icon pack manifest must be an object".into());
        };
        let string = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
        let icons = object
            .get("icons")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_owned())))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let fallback = match object.get("fallback") {
            Some(value) => Some(IconPackFallback::from_value(value)?),
            None => None,
        };
        Ok(Self {
            name: string("name"),
            author: string("author"),
            description: string("description"),
            icons,
            fallback,
        })
    }

    pub fn to_value(&self) -> Value {
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
            "icons".into(),
            Value::Object(
                self.icons
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect(),
            ),
        );
        Value::Object(object)
    }
}

/// An icon pack loaded from a folder under the themes root.
#[derive(Clone, Debug, PartialEq)]
pub struct IconPack {
    pub id: String,
    pub directory: PathBuf,
    pub manifest: IconPackManifest,
    icons: BTreeMap<String, String>,
}

impl IconPack {
    pub fn load(directory: &Path) -> Result<Self, String> {
        let id = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = directory.join(MANIFEST_FILE_NAME);
        let meta = fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.len() > MAX_MANIFEST_BYTES {
            return Err(format!("{} exceeds size limit.", path.display()));
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let manifest =
            IconPackManifest::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let icons = manifest
            .icons
            .iter()
            .filter(|(key, value)| !key.trim().is_empty() && !value.trim().is_empty())
            .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
            .collect();
        Ok(Self {
            id,
            directory: directory.to_path_buf(),
            manifest,
            icons,
        })
    }

    pub fn display_name(&self) -> &str {
        self.manifest
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&self.id)
    }

    pub fn fallback(&self) -> IconPackFallback {
        self.manifest
            .fallback
            .as_ref()
            .map(IconPackFallback::clamped)
            .unwrap_or_default()
    }

    /// Keys tried in order: special, AUMID/package, full path, file name, stem.
    pub fn candidate_keys<'a>(
        executable_path: Option<&'a str>,
        app_id: Option<&'a str>,
        special_key: Option<&'a str>,
    ) -> Vec<&'a str> {
        if let Some(special) = special_key.filter(|key| !key.trim().is_empty()) {
            return vec![special];
        }
        let mut out = Vec::new();
        if let Some(app) = app_id.filter(|id| !id.trim().is_empty()) {
            out.push(app);
            let package = app.split('!').next().unwrap_or(app);
            if let Some(index) = package.rfind('_')
                && index > 0
            {
                out.push(&package[..index]);
            }
            out.push(package);
        }
        if let Some(exe) = executable_path.filter(|path| !path.trim().is_empty()) {
            out.push(exe);
            if let Some(name) = Path::new(exe).file_name().and_then(|n| n.to_str()) {
                out.push(name);
            }
            if let Some(stem) = Path::new(exe).file_stem().and_then(|n| n.to_str()) {
                out.push(stem);
            }
        }
        out
    }

    /// The image file the pack uses, or `None` when it has none. Mapped paths cannot escape
    /// the pack folder. An image named after the executable in `/` or `icons/` needs no mapping.
    pub fn find_icon_file(
        &self,
        executable_path: Option<&str>,
        app_id: Option<&str>,
        special_key: Option<&str>,
    ) -> Option<PathBuf> {
        for key in Self::candidate_keys(executable_path, app_id, special_key) {
            if let Some(relative) = self.icons.get(key)
                && let Some(found) = resolve_inside_pack(&self.directory, Path::new(relative))
            {
                return Some(found);
            }
        }
        let stem = special_key
            .map(|key| key.trim_start_matches('@'))
            .or_else(|| executable_path.and_then(|path| Path::new(path).file_stem()?.to_str()))
            .filter(|stem| !stem.trim().is_empty())?;
        for folder in ["", "icons"] {
            for extension in IMAGE_EXTENSIONS {
                let relative = Path::new(folder).join(format!("{stem}{extension}"));
                if let Some(found) = resolve_inside_pack(&self.directory, &relative) {
                    return Some(found);
                }
            }
        }
        None
    }
}

/// Resolves a pack-relative path, rejecting absolute paths and `..` traversal.
pub fn resolve_inside_pack(root: &Path, relative: &Path) -> Option<PathBuf> {
    if relative.is_absolute() {
        return None;
    }
    let mut safe = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    let full = root.join(safe);
    let extension = full.extension()?.to_str()?.to_ascii_lowercase();
    if !IMAGE_EXTENSIONS
        .iter()
        .any(|ext| extension == ext.trim_start_matches('.'))
    {
        return None;
    }
    full.is_file().then_some(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_clamps_scale_and_reports_leaf_petal_corners() {
        let fallback = IconPackFallback {
            icon_scale: 4.0,
            corner_radius: -1.0,
            shape: IconPackShape::Petal,
            sharp_corner: IconPackCorner::TopLeft,
            ..IconPackFallback::default()
        }
        .clamped();
        assert_eq!(fallback.icon_scale, 1.0);
        assert_eq!(fallback.corner_radius, 0.0);
        assert_eq!(
            fallback.square_corners(),
            vec![IconPackCorner::TopLeft, IconPackCorner::BottomRight]
        );
        assert_eq!(IconPackFallback::default().clamped().icon_scale, 0.64);
    }

    #[test]
    fn candidate_key_order_matches_csharp() {
        let keys = IconPack::candidate_keys(
            Some(r"C:\Apps\firefox.exe"),
            Some("FirefoxFirefox_1.0!app"),
            None,
        );
        assert_eq!(
            keys,
            vec![
                "FirefoxFirefox_1.0!app",
                "FirefoxFirefox",
                "FirefoxFirefox_1.0",
                r"C:\Apps\firefox.exe",
                "firefox.exe",
                "firefox"
            ]
        );
        assert_eq!(
            IconPack::candidate_keys(None, None, Some("@recyclebin")),
            vec!["@recyclebin"]
        );
    }

    #[test]
    fn rejects_paths_escaping_the_pack() {
        let root = Path::new("C:\\packs\\nord");
        assert!(resolve_inside_pack(root, Path::new("..\\evil.png")).is_none());
        assert!(resolve_inside_pack(root, Path::new("C:\\evil.png")).is_none());
        assert!(resolve_inside_pack(root, Path::new("icons\\x.txt")).is_none());
    }

    #[test]
    fn loads_bundled_icon_manifests_if_any() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes");
        for id in [
            "catppuccin",
            "gruvbox",
            "nord",
            "prism",
            "solarized",
            "tokyo-night",
        ] {
            let dir = root.join(id).join("icons");
            if dir.join(MANIFEST_FILE_NAME).exists() {
                let pack = IconPack::load(&dir).unwrap_or_else(|e| panic!("{id}: {e}"));
                assert!(!pack.id.is_empty());
            }
        }
    }
}
