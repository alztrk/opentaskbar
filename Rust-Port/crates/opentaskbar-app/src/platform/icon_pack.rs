//! Custom icon pack manifests and icon lookup.
//!
//! Windows-independent port of C# `IconPack`. A pack is a folder with `pack.json` at
//! its root and optional images; the manifest maps match keys to image paths relative
//! to that folder. Lookup order matches C# exactly:
//! special key → AUMID (and package name) → full exe path → exe file name → stem.
//! No COM/GDI is used; this module only resolves paths.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde_json::{Map, Value};

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

/// The plate drawn behind icons the pack has no image for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconPackShape {
    #[default]
    None,
    Leaf,
    Petal,
    Rounded,
    Circle,
}

/// Plate settings for unthemed apps. `background: None` means automatic tinting.
#[derive(Clone, Debug, PartialEq)]
pub struct IconFallback {
    pub shape: IconPackShape,
    pub background: Option<String>,
    pub icon_scale: f32,
}

impl Default for IconFallback {
    fn default() -> Self {
        Self {
            shape: IconPackShape::None,
            background: None,
            icon_scale: 0.64,
        }
    }
}

/// Contents of `pack.json`. `directory` is kept privately so mapped paths can be
/// resolved and validated by [`find_icon`].
#[derive(Clone, Debug, Default)]
pub struct IconPackManifest {
    pub name: String,
    pub author: String,
    pub icons: HashMap<String, String>,
    pub fallback: Option<IconFallback>,
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

fn object_get_ci<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

fn json_get_ci<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value
        .as_object()
        .and_then(|object| object_get_ci(object, key))
}

fn string_ci(value: &Value, key: &str) -> String {
    json_get_ci(value, key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Accepts comments and trailing commas, as the C# serializer did, without touching
/// string contents.
fn relaxed_json(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
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
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            i += 2;
            while i < chars.len() && chars[i] != '\n' && chars[i] != '\r' {
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
        if c == ','
            && let Some(next) = chars[i + 1..].iter().find(|next| !next.is_whitespace())
            && (*next == '}' || *next == ']')
        {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
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

fn parse_fallback(object: &Map<String, Value>) -> IconFallback {
    let shape = match object_get_ci(object, "shape") {
        Some(Value::String(shape)) => parse_shape(shape),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(1) => IconPackShape::Leaf,
            Some(2) => IconPackShape::Petal,
            Some(3) => IconPackShape::Rounded,
            Some(4) => IconPackShape::Circle,
            _ => IconPackShape::None,
        },
        _ => IconPackShape::None,
    };
    let background = object_get_ci(object, "background")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty() && !text.eq_ignore_ascii_case("auto"))
        .map(str::to_string);
    // C# `IconPack.Fallback` clamps the runtime scale to 0.1..=1.0.
    let icon_scale = object_get_ci(object, "iconScale")
        .and_then(Value::as_f64)
        .filter(|scale| scale.is_finite())
        .map(|scale| scale.clamp(0.1, 1.0) as f32)
        .unwrap_or(0.64);
    IconFallback {
        shape,
        background,
        icon_scale,
    }
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

/// Candidate keys in the same order as C# `IconPack.CandidateKeys`.
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

fn lookup<'a>(icons: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    icons
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, relative)| relative.as_str())
}

/// Resolves a relative image path inside the pack. Paths escaping the pack, or pointing
/// to a non-image or missing file, are ignored.
fn resolve_inside_pack(directory: &Path, relative: &str) -> Option<String> {
    let relative = relative.trim().replace('\\', "/");
    if relative.is_empty() || relative.starts_with('/') {
        return None;
    }
    let parts: Vec<&str> = relative.split('/').collect();
    if parts.iter().any(|part| {
        part.is_empty()
            || *part == "."
            || *part == ".."
            || part.contains(':')
            || part.contains('\0')
    }) {
        return None;
    }

    let mut full = directory.to_path_buf();
    for part in &parts {
        full.push(part);
    }
    let (Ok(base), Ok(absolute)) = (std::path::absolute(directory), std::path::absolute(&full))
    else {
        return None;
    };
    if !absolute.starts_with(&base) || absolute == base {
        return None;
    }
    if let (Ok(resolved_base), Ok(resolved)) =
        (fs::canonicalize(directory), fs::canonicalize(&full))
        && !resolved.starts_with(&resolved_base)
    {
        return None;
    }
    if !absolute.is_file() {
        return None;
    }
    let extension = absolute
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{}", extension.to_ascii_lowercase()))?;
    if !IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }
    Some(absolute.to_string_lossy().into_owned())
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
            && let Some(path) = resolve_inside_pack(&manifest.directory, relative)
        {
            return Some(path);
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
            if let Some(path) = resolve_inside_pack(&manifest.directory, &relative) {
                return Some(path);
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
        assert_eq!(fallback.background.as_deref(), Some("#FF101010"));
        assert!((fallback.icon_scale - 0.9).abs() < f32::EPSILON);
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
        assert_eq!(fallback.background, None);
        assert!((fallback.icon_scale - 1.0).abs() < f32::EPSILON);

        write(
            &dir.join("pack.json"),
            r#"{"fallback":{"shape":"Rounded","background":"auto","iconScale":-3}}"#,
        );
        let manifest = load(&dir).expect("load manifest");
        let fallback = manifest.fallback.as_ref().expect("fallback");
        assert_eq!(fallback.shape, IconPackShape::Rounded);
        assert_eq!(fallback.background, None);
        assert!((fallback.icon_scale - 0.1).abs() < f32::EPSILON);
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
    fn candidate_keys_follow_csharp_order() {
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

        // C# CandidateKeys short-circuits on a special key: a mapping to a missing
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
