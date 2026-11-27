//! Custom icon pack manifests and icon lookup.
//!
//! Windows-independent Rust implementation of `IconPack`. A pack is a folder with `pack.json` at
//! its root and optional images; the manifest maps match keys to image paths relative
//! to that folder. Lookup order matches reference exactly:
//! special key → AUMID (and package name) → full exe path → exe file name → stem.
//! No COM/GDI is used; this module only resolves paths.

use std::collections::BTreeMap;
use std::fs;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde_json::{Map, Value};

use super::json_ext::{json_get_ci, object_get_ci, relaxed_json, string_ci};

static ACTIVE_PACK: OnceLock<RwLock<Option<IconPackManifest>>> = OnceLock::new();

/// Loads the icon pack from an active theme directory and makes it process-wide.
/// Passing `None`/empty clears the pack so native icons are used again.
pub fn set_active_pack(theme_directory: Option<&Path>) {
    let manifest = theme_directory.and_then(load);
    let slot = ACTIVE_PACK.get_or_init(|| RwLock::new(None));
    *slot.write().expect("icon pack lock poisoned") = manifest;
}

pub fn active_pack() -> Option<IconPackManifest> {
    ACTIVE_PACK
        .get()
        .and_then(|slot| slot.read().ok())
        .and_then(|guard| guard.clone())
}

/// Resolves a dock icon spec (`exe path`, `shell:AppsFolder\<AUMID>`, `@start`, ...)
/// to an image file inside the active pack, if the pack maps it.
pub fn find_for_spec(spec: &str) -> Option<String> {
    let pack = active_pack()?;
    let trimmed = spec.trim();
    let (exe, app_id, special) = if trimmed.starts_with("@") {
        ("", None, Some(trimmed))
    } else if let Some(aumid) = trimmed
        .strip_prefix("shell:AppsFolder\\")
        .or_else(|| trimmed.strip_prefix("::{"))
    {
        ("", Some(aumid), None)
    } else {
        (trimmed, None, None)
    };
    find_icon(&pack, exe, app_id, special)
}

pub const MANIFEST_FILE_NAME: &str = "pack.json";
pub const START_KEY: &str = "@start";
pub const RECYCLE_BIN_KEY: &str = "@recyclebin";
pub const RECYCLE_BIN_FULL_KEY: &str = "@recyclebin-full";
pub const IMAGE_EXTENSIONS: &[&str] = &[".png", ".ico", ".jpg", ".jpeg", ".bmp"];

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ICON_KEYS: usize = 512;

/// Backwards-compatible name for the unified fallback plate.
pub type IconFallback = IconPackFallback;

/// Contents of `pack.json`. `directory` is kept privately so mapped paths can be
/// resolved and validated by [`find_icon`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IconPackManifest {
    pub name: String,
    pub author: String,
    /// Free-form pack description (WS-F reference parity).
    pub description: String,
    pub icons: BTreeMap<String, String>,
    pub fallback: Option<IconPackFallback>,
    directory: PathBuf,
}

impl IconPackManifest {
    /// Creates an empty manifest for `directory` (useful when composing manifests in
    /// tests or from another source).
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            ..Self::default()
        }
    }

    /// Folder this manifest was loaded from; mapped image paths are relative to it.
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

fn parse_shape(text: &str) -> IconPackShape {
    match text.trim().to_ascii_lowercase().as_str() {
        "leaf" => IconPackShape::Leaf,
        "petal" => IconPackShape::Petal,
        "rounded" => IconPackShape::Rounded,
        "circle" => IconPackShape::Circle,
        _ => IconPackShape::None,
    }
}

fn parse_fallback(object: &Map<String, Value>) -> IconPackFallback {
    let ci = |key: &str| object_get_ci(object, key);
    // reference pack manifests allow either names or the numeric shape values.
    let shape = match ci("shape") {
        Some(Value::String(shape)) => IconPackShape::parse(shape).unwrap_or_default(),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(1) => IconPackShape::Leaf,
            Some(2) => IconPackShape::Petal,
            Some(3) => IconPackShape::Rounded,
            Some(4) => IconPackShape::Circle,
            _ => IconPackShape::None,
        },
        _ => IconPackShape::None,
    };
    let sharp_corner = ci("sharpCorner")
        .or_else(|| ci("sharp_corner"))
        .and_then(Value::as_str)
        .and_then(IconPackCorner::parse)
        .unwrap_or_default();
    let corner_radius = ci("cornerRadius").and_then(Value::as_f64).unwrap_or(0.5);
    // `None`/empty/"auto" mean automatic tinting from the icon.
    let background = ci("background")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty() && !text.eq_ignore_ascii_case("auto"))
        .map(str::to_owned)
        .unwrap_or_else(|| "auto".to_owned());
    // `IconPack.Fallback` clamps the runtime scale to 0.1..=1.0.
    let icon_scale = ci("iconScale")
        .and_then(Value::as_f64)
        .filter(|scale| scale.is_finite())
        .map(|scale| scale.clamp(0.1, 1.0))
        .unwrap_or(0.64);
    IconPackFallback {
        shape,
        sharp_corner,
        corner_radius,
        background,
        icon_scale,
    }
    .clamped()
}

/// Reads `pack.json` from `directory`, returning `None` if it is missing, too large,
/// unreadable or malformed.
pub fn load(directory: &Path) -> Option<IconPackManifest> {
    let path = directory.join(MANIFEST_FILE_NAME);
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    let text = fs::read_to_string(&path).ok()?;
    let value: Value =
        serde_json::from_str(&relaxed_json(text.trim_start_matches('\u{feff}'))).ok()?;
    value.as_object()?;

    let mut manifest = IconPackManifest::new(directory.to_path_buf());
    manifest.name = string_ci(&value, "name");
    manifest.author = string_ci(&value, "author");
    manifest.description = string_ci(&value, "description");
    if let Some(icons) = json_get_ci(&value, "icons").and_then(Value::as_object) {
        for (key, value) in icons {
            if manifest.icons.len() >= MAX_ICON_KEYS {
                break;
            }
            let Some(relative) = value.as_str() else {
                continue;
            };
            let (key, relative) = (key.trim(), relative.trim());
            if !key.is_empty() && !relative.is_empty() {
                manifest.icons.insert(key.to_string(), relative.to_string());
            }
        }
    }
    manifest.fallback = json_get_ci(&value, "fallback")
        .and_then(Value::as_object)
        .map(parse_fallback);
    Some(manifest)
}

/// Handles both Windows and Unix separators so callers can pass either path style.
fn file_name_of(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn stem_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(index) if index > 0 => &name[..index],
        _ => name,
    }
}

/// Candidate keys in the same order as `IconPack.CandidateKeys`.
fn candidate_keys(exe_path: &str, app_id: Option<&str>, special_key: Option<&str>) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(key) = special_key.map(str::trim).filter(|key| !key.is_empty()) {
        keys.push(key.to_string());
        return keys;
    }

    if let Some(id) = app_id.map(str::trim).filter(|id| !id.is_empty()) {
        keys.push(id.to_string());
        let package = id.split('!').next().unwrap_or(id);
        if let Some(index) = package.rfind('_')
            && index > 0
        {
            keys.push(package[..index].to_string());
        }
        keys.push(package.to_string());
    }

    let exe = exe_path.trim();
    if !exe.is_empty() {
        keys.push(exe.to_string());
        let name = file_name_of(exe);
        keys.push(name.to_string());
        keys.push(stem_of(name).to_string());
    }
    keys
}

fn lookup<'a>(icons: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    icons
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, relative)| relative.as_str())
}

/// Returns the absolute image path this pack uses for the app, or `None` when it has
/// no matching image. Mapped keys win; a file named after the executable (in the pack
/// folder or its `icons` subfolder) works without a mapping.
pub fn find_icon(
    manifest: &IconPackManifest,
    exe_path: &str,
    app_id: Option<&str>,
    special_key: Option<&str>,
) -> Option<String> {
    for key in candidate_keys(exe_path, app_id, special_key) {
        if let Some(relative) = lookup(&manifest.icons, &key)
            && let Some(path) =
                resolve_inside_pack(&manifest.directory, Path::new(&relative.replace('\\', "/")))
        {
            return Some(path.to_string_lossy().into_owned());
        }
    }

    let stem = match special_key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => key.trim_start_matches('@').to_string(),
        None => {
            let exe = exe_path.trim();
            if exe.is_empty() {
                String::new()
            } else {
                stem_of(file_name_of(exe)).to_string()
            }
        }
    };
    if stem.is_empty() {
        return None;
    }
    for folder in ["", "icons"] {
        for extension in IMAGE_EXTENSIONS {
            let relative = if folder.is_empty() {
                format!("{stem}{extension}")
            } else {
                format!("{folder}/{stem}{extension}")
            };
            if let Some(path) = resolve_inside_pack(&manifest.directory, Path::new(&relative)) {
                return Some(path.to_string_lossy().into_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "ot-icon-{tag}-{}-{}-{nanos}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    #[test]
    fn load_reads_manifest_and_fallback() {
        let temp = TempDir::new("load");
        let dir = temp.path().join("pack");
        write(
            &dir.join("pack.json"),
            r##"{
              // a comment
              "NAME": "Example", "author": "Someone",
              "icons": { "@start": "icons/start.png", "firefox.exe": "firefox.png" },
              "fallback": { "shape": "circle", "background": "#FF101010", "iconScale": 0.9 },
            }"##,
        );

        let manifest = load(&dir).expect("load manifest");
        assert_eq!(manifest.name, "Example");
        assert_eq!(manifest.author, "Someone");
        assert_eq!(manifest.icons.len(), 2);
        assert_eq!(manifest.directory(), dir.as_path());
        let fallback = manifest.fallback.as_ref().expect("fallback");
        assert_eq!(fallback.shape, IconPackShape::Circle);
        assert_eq!(fallback.background, "#FF101010");
        assert!((fallback.icon_scale - 0.9).abs() < f64::EPSILON);
        assert!(load(&temp.path().join("missing")).is_none());
    }

    #[test]
    fn fallback_defaults_and_auto_background() {
        let temp = TempDir::new("defaults");
        let dir = temp.path().join("pack");
        write(&dir.join("pack.json"), r#"{"fallback":{"iconScale":9}}"#);
        let manifest = load(&dir).expect("load manifest");
        let fallback = manifest.fallback.as_ref().expect("fallback");
        assert_eq!(fallback.shape, IconPackShape::None);
        assert_eq!(fallback.background, "auto");
        assert!((fallback.icon_scale - 1.0).abs() < f64::EPSILON);

        write(
            &dir.join("pack.json"),
            r#"{"fallback":{"shape":"Rounded","background":"auto","iconScale":-3}}"#,
        );
        let manifest = load(&dir).expect("load manifest");
        let fallback = manifest.fallback.as_ref().expect("fallback");
        assert_eq!(fallback.shape, IconPackShape::Rounded);
        assert_eq!(fallback.background, "auto");
        assert!((fallback.icon_scale - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn oversize_manifest_is_rejected() {
        let temp = TempDir::new("size");
        let dir = temp.path().join("pack");
        let json = format!(
            r#"{{"name":"x"}}{}"#,
            " ".repeat(MAX_MANIFEST_BYTES as usize)
        );
        write(&dir.join("pack.json"), &json);
        assert!(load(&dir).is_none());
    }

    #[test]
    fn candidate_keys_follow_reference_order() {
        let keys = candidate_keys(
            r"C:\Apps\Firefox\firefox.exe",
            Some("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"),
            None,
        );
        assert_eq!(
            keys,
            vec![
                "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
                "Microsoft.WindowsCalculator",
                "Microsoft.WindowsCalculator_8wekyb3d8bbwe",
                r"C:\Apps\Firefox\firefox.exe",
                "firefox.exe",
                "firefox",
            ]
        );
        assert_eq!(
            candidate_keys(
                r"C:\Apps\Firefox\firefox.exe",
                Some("App_1!x"),
                Some("@recyclebin")
            ),
            vec!["@recyclebin"]
        );
        assert_eq!(
            candidate_keys(r"C:\Apps\Firefox\firefox.exe", None, None),
            vec![r"C:\Apps\Firefox\firefox.exe", "firefox.exe", "firefox",]
        );
    }

    #[test]
    fn find_icon_precedence_and_fallthrough() {
        let temp = TempDir::new("precedence");
        let dir = temp.path().join("pack");
        write(&dir.join("special.png"), "special");
        write(&dir.join("aumid.png"), "aumid");
        write(&dir.join("full.png"), "full");
        write(&dir.join("name.png"), "name");

        let exe = r"C:\Apps\Firefox\firefox.exe";
        let mut manifest = IconPackManifest::new(dir.clone());
        manifest
            .icons
            .insert("@start".to_string(), "special.png".to_string());
        manifest
            .icons
            .insert("App_1!x".to_string(), "aumid.png".to_string());
        manifest
            .icons
            .insert(exe.to_string(), "full.png".to_string());
        manifest
            .icons
            .insert("firefox.exe".to_string(), "name.png".to_string());

        let found = find_icon(&manifest, exe, Some("App_1!x"), Some("@start")).expect("special");
        assert!(found.ends_with("special.png"));
        let found = find_icon(&manifest, exe, Some("App_1!x"), None).expect("aumid");
        assert!(found.ends_with("aumid.png"));
        let found = find_icon(&manifest, exe, None, None).expect("full path");
        assert!(found.ends_with("full.png"));

        let mut renamed = manifest.clone();
        renamed.icons.remove(exe);
        let found = find_icon(&renamed, exe, None, None).expect("file name");
        assert!(found.ends_with("name.png"));

        // reference CandidateKeys short-circuits on a special key: a mapping to a missing
        // file yields no result instead of falling through to the AUMID candidate.
        let mut missing = manifest.clone();
        missing
            .icons
            .insert("@start".to_string(), "gone.png".to_string());
        assert!(find_icon(&missing, exe, Some("App_1!x"), Some("@start")).is_none());
    }

    #[test]
    fn convention_and_traversal() {
        let temp = TempDir::new("convention");
        let dir = temp.path().join("pack");
        write(&dir.join("icons/firefox.png"), "stem");
        write(&dir.join("icons/recyclebin.png"), "special");
        write(&temp.path().join("outside.png"), "outside");

        let mut manifest = IconPackManifest::new(dir.clone());
        let found = find_icon(&manifest, r"C:\Apps\Firefox\firefox.exe", None, None)
            .expect("conventional image");
        assert!(found.ends_with("firefox.png"));
        let found = find_icon(&manifest, "", None, Some("@recyclebin")).expect("special image");
        assert!(found.ends_with("recyclebin.png"));

        // Remove the conventional image so only the (rejected) mapping remains.
        fs::remove_file(dir.join("icons/firefox.png")).expect("remove conventional image");
        manifest
            .icons
            .insert("firefox".to_string(), "../outside.png".to_string());
        assert_eq!(
            find_icon(&manifest, r"C:\Apps\Firefox\firefox.exe", None, None),
            None
        );
        manifest.icons.clear();
        manifest
            .icons
            .insert("firefox".to_string(), "../../outside.png".to_string());
        assert_eq!(
            find_icon(&manifest, r"C:\Apps\Firefox\firefox.exe", None, None),
            None
        );
        assert!(find_icon(&manifest, "", None, None).is_none());
    }
}

// ---- reference icon-pack parity (WS-F) ----
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
    pub(crate) fn parse(text: &str) -> Option<Self> {
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
    pub(crate) fn parse(text: &str) -> Option<Self> {
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

/// `IconPackFallback` with defaults and clamping applied on read.
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

    /// Load-time normalization: radius 0..1, scale 0.1..1.0 (`IconPack.Fallback`).
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

impl IconPackManifest {
    /// Parses a `pack.json` document (`IconPack` parity).
    pub fn from_str(text: &str) -> Result<Self, String> {
        let value = crate::design::widget_pack::parse_json_lenient(text)?;
        let Some(object) = value.as_object() else {
            return Err("icon pack manifest must be an object".into());
        };
        let string = |key: &str| {
            object
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        let icons = object
            .get("icons")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_owned())))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let fallback = object
            .get("fallback")
            .map(IconPackFallback::from_value)
            .transpose()?
            .map(|fallback| fallback.clamped());
        Ok(Self {
            name: string("name"),
            author: string("author"),
            description: string("description"),
            icons,
            fallback,
            directory: PathBuf::new(),
        })
    }

    /// Serializes the manifest back to a JSON value.
    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("name".into(), Value::String(self.name.clone()));
        object.insert("author".into(), Value::String(self.author.clone()));
        object.insert(
            "description".into(),
            Value::String(self.description.clone()),
        );
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
        let name = self.manifest.name.as_str();
        if name.trim().is_empty() {
            &self.id
        } else {
            name
        }
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
    if let (Ok(resolved_root), Ok(resolved_full)) =
        (fs::canonicalize(root), fs::canonicalize(&full))
        && !resolved_full.starts_with(&resolved_root)
    {
        return None;
    }
    full.is_file().then_some(full)
}

#[cfg(test)]
mod parity_tests {
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
    fn candidate_key_order_matches_reference() {
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
        let root = Path::new(crate::resource!("themes"));
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
