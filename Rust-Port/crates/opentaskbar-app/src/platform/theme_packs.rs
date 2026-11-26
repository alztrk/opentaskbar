//! Folder-based theme packs: discovery, portable `.ottheme` export/import and deletion.
//!
//! Windows-independent port of C# `ThemePacks`, `ThemePackTransfer` and `WidgetPack`.
//! Themes are plain directories under a caller-supplied root; a shared file is a zip
//! whose entries are relative to the theme folder (usually under a folder named after it).
//! All archive and identifier handling is validated before anything is written to disk.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// File at the root of a theme folder that describes it.
pub const MANIFEST_FILE_NAME: &str = "theme.json";
/// Optional custom icon pack inside a theme.
pub const ICON_FOLDER: &str = "icons";
/// Optional design-token overrides inside a theme.
pub const WIDGET_FOLDER: &str = "widgets";

const MAX_FILE_COUNT: usize = 512;
const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_WIDGET_BYTES: u64 = 128 * 1024;
const MAX_ID_LEN: usize = 120;

const ALLOWED_EXTENSIONS: &[&str] = &[
    ".json", ".png", ".ico", ".jpg", ".jpeg", ".bmp", ".md", ".txt",
];

/// Description read from `theme.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeManifest {
    pub name: String,
    pub author: String,
    pub description: String,
    pub preserve_original_icons: bool,
}

/// A discovered theme folder. `id` is the folder name.
#[derive(Clone, Debug)]
pub struct ThemePack {
    pub id: String,
    pub directory: PathBuf,
    pub manifest: ThemeManifest,
}

/// Themes bundled with the application; they keep native Windows icons even when no
/// custom icon pack is present.
pub fn built_in_ids() -> &'static [&'static str] {
    &[
        "catppuccin",
        "gruvbox",
        "nord",
        "prism",
        "solarized",
        "tokyo-night",
    ]
}

/// Whether `id` names a built-in theme (case-insensitive).
pub fn is_built_in(id: &str) -> bool {
    built_in_ids()
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(id))
}

/// Folder names are used as settings values, so only plain alphanumeric names with
/// `-`/`_` separators are accepted. No separators, no `..`, no rooted paths.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        && !id.contains("..")
}

/// Turns a display name into a safe, non-empty folder name, or `None` when nothing
/// usable remains. Mirrors C# `ThemePackTransfer.Sanitize`.
pub fn sanitize_id(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut cleaned: String = trimmed
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.chars().count() > MAX_ID_LEN {
        cleaned = cleaned.chars().take(MAX_ID_LEN).collect();
    }
    let cleaned = cleaned.trim_matches('-').to_string();
    if is_safe_id(&cleaned) {
        Some(cleaned)
    } else {
        None
    }
}

fn unique_id(root: &Path, name: Option<&str>) -> String {
    let base = name
        .and_then(sanitize_id)
        .unwrap_or_else(|| "theme".to_string());
    let mut id = base.clone();
    let mut suffix = 2u32;
    while root.join(&id).exists() {
        let tail = format!("-{suffix}");
        let keep = MAX_ID_LEN.saturating_sub(tail.chars().count());
        let shortened: String = base.chars().take(keep).collect();
        let shortened = shortened.trim_end_matches('-');
        let stem = if shortened.is_empty() {
            "theme"
        } else {
            shortened
        };
        id = format!("{stem}{tail}");
        suffix += 1;
    }
    id
}

/// Resolves an id inside `root`, rejecting unsafe ids and anything that escapes via
/// symlinks.
fn theme_path(root: &Path, id: &str) -> Option<PathBuf> {
    if !is_safe_id(id) {
        return None;
    }
    let base = std::path::absolute(root).ok()?;
    let candidate = std::path::absolute(base.join(id)).ok()?;
    if !candidate.starts_with(&base) || candidate == base {
        return None;
    }
    if let (Ok(resolved_root), Ok(resolved)) =
        (fs::canonicalize(root), fs::canonicalize(&candidate))
        && !resolved.starts_with(&resolved_root)
    {
        return None;
    }
    Some(candidate)
}

fn path_within(root: &Path, child: &Path) -> bool {
    let (Ok(base), Ok(full)) = (std::path::absolute(root), std::path::absolute(child)) else {
        return false;
    };
    full.starts_with(&base) && full != base
}

fn same_path(a: &Path, b: &Path) -> bool {
    if let (Ok(x), Ok(y)) = (fs::canonicalize(a), fs::canonicalize(b)) {
        return x == y;
    }
    match (std::path::absolute(a), std::path::absolute(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        nanos
    )
}

fn temp_path(parent: &Path, tag: &str) -> PathBuf {
    parent.join(format!(".{tag}-{}", unique_suffix()))
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

/// Accepts the two JSON relaxations the C# serializer used: comments and trailing
/// commas. String contents (including URLs such as `https://`) are left untouched.
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

fn read_json_object(path: &Path, max_bytes: u64) -> Option<Value> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let value: Value =
        serde_json::from_str(&relaxed_json(text.trim_start_matches('\u{feff}'))).ok()?;
    value.as_object()?;
    Some(value)
}

fn load_manifest(directory: &Path) -> Option<ThemeManifest> {
    let value = read_json_object(&directory.join(MANIFEST_FILE_NAME), MAX_MANIFEST_BYTES)?;
    Some(ThemeManifest {
        name: string_ci(&value, "name"),
        author: string_ci(&value, "author"),
        description: string_ci(&value, "description"),
        preserve_original_icons: json_get_ci(&value, "preserveOriginalIcons")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn load_pack(directory: &Path, id: String) -> Option<ThemePack> {
    let manifest = load_manifest(directory)?;
    Some(ThemePack {
        id,
        directory: directory.to_path_buf(),
        manifest,
    })
}

/// Lists valid theme folders under `root`, sorted by display name.
pub fn discover(root: &Path) -> Vec<ThemePack> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut packs = Vec::new();
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_safe_id(&name) {
            continue;
        }
        if let Some(pack) = load_pack(&entry.path(), name) {
            packs.push(pack);
        }
    }
    packs.sort_by(|a, b| {
        let a = if a.manifest.name.is_empty() {
            &a.id
        } else {
            &a.manifest.name
        };
        let b = if b.manifest.name.is_empty() {
            &b.id
        } else {
            &b.manifest.name
        };
        a.to_lowercase().cmp(&b.to_lowercase())
    });
    packs
}

/// Contents of `widgets/pack.json` (design-token overrides), or an empty object when
/// the theme has no readable widget pack.
pub fn read_widget_tokens(theme: &ThemePack) -> Value {
    read_json_object(
        &theme.directory.join(WIDGET_FOLDER).join("pack.json"),
        MAX_WIDGET_BYTES,
    )
    .unwrap_or_else(|| Value::Object(Map::new()))
}

fn allowed_extension(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    let extension = format!(".{}", extension.to_ascii_lowercase());
    ALLOWED_EXTENSIONS.contains(&extension.as_str())
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>, total: &mut u64) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("ErrorThemeInstall: {e}"))?;
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_files(&entry.path(), files, total)?;
        } else if kind.is_file() {
            if files.len() >= MAX_FILE_COUNT {
                return Err("ErrorThemeInvalid: too many files".to_string());
            }
            if !allowed_extension(&entry.path()) {
                return Err("ErrorThemeInvalid: file type not allowed".to_string());
            }
            let len = entry
                .metadata()
                .map_err(|e| format!("ErrorThemeInstall: {e}"))?
                .len();
            if len > MAX_FILE_BYTES {
                return Err("ErrorThemeInvalid: file too large".to_string());
            }
            *total = (*total)
                .checked_add(len)
                .ok_or_else(|| "ErrorThemeInvalid: theme too large".to_string())?;
            if *total > MAX_UNCOMPRESSED_BYTES {
                return Err("ErrorThemeInvalid: theme too large".to_string());
            }
            files.push(entry.path());
        }
    }
    Ok(())
}

/// Zip entry name for a file relative to the theme folder (always `/`-separated).
fn zip_relative(relative: &Path) -> String {
    relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().replace('\\', "/")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Writes theme `id` as a zip at `destination` via a temporary file + rename.
pub fn export_theme(root: &Path, id: &str, destination: &Path) -> Result<(), String> {
    let Some(source) = theme_path(root, id) else {
        return Err("ErrorThemeInvalid".to_string());
    };
    if !source.is_dir() {
        return Err("ErrorThemeMissing".to_string());
    }

    let mut files = Vec::new();
    let mut total = 0u64;
    collect_files(&source, &mut files, &mut total)?;
    if files.is_empty() {
        return Err("ErrorThemeInvalid: empty theme".to_string());
    }

    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
    let temporary = temp_path(parent, "ottheme");

    let written = (|| -> Result<(), String> {
        let output = File::create(&temporary).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
        let mut writer = ZipWriter::new(output);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for path in &files {
            let relative = path
                .strip_prefix(&source)
                .map_err(|e| format!("ErrorThemeInstall: {e}"))?;
            let name = format!("{}/{}", id, zip_relative(relative));
            writer
                .start_file(name, options)
                .map_err(|e| format!("ErrorThemeInstall: {e}"))?;
            let mut input = File::open(path).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
            io::copy(&mut input, &mut writer).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
        }
        writer
            .finish()
            .map_err(|e| format!("ErrorThemeInstall: {e}"))?;
        Ok(())
    })();

    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    // std::fs::rename cannot replace an existing file on Windows.
    if destination.exists() {
        fs::remove_file(destination).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
    }
    if let Err(error) = fs::rename(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("ErrorThemeInstall: {error}"));
    }
    Ok(())
}

fn extract_safe(archive: &mut ZipArchive<File>, destination: &Path) -> Result<(), String> {
    let mut count = 0usize;
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| format!("ErrorThemeInvalid: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        count += 1;
        if count > MAX_FILE_COUNT {
            return Err("ErrorThemeInvalid: too many files".to_string());
        }
        if entry.size() > MAX_FILE_BYTES {
            return Err("ErrorThemeInvalid: file too large".to_string());
        }
        let declared = total
            .checked_add(entry.size())
            .ok_or_else(|| "ErrorThemeInvalid: theme too large".to_string())?;
        if declared > MAX_UNCOMPRESSED_BYTES {
            return Err("ErrorThemeInvalid: theme too large".to_string());
        }

        // Backslashes are a path separator on Windows, so normalise before checking.
        let raw = entry.name().replace('\\', "/");
        let parts: Vec<&str> = raw.split('/').collect();
        if raw.is_empty()
            || raw.starts_with('/')
            || parts.iter().any(|part| {
                part.is_empty()
                    || *part == "."
                    || *part == ".."
                    || part.contains(':')
                    || part.contains('\0')
            })
            || entry.enclosed_name().is_none()
        {
            return Err("ErrorThemeInvalid: unsafe path in archive".to_string());
        }

        let full = destination.join(&raw);
        if !path_within(destination, &full) {
            return Err("ErrorThemeInvalid: unsafe path in archive".to_string());
        }
        if !allowed_extension(&full) {
            return Err("ErrorThemeInvalid: file type not allowed".to_string());
        }
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
        }

        let mut output = File::create(&full).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
        let mut written = 0u64;
        let mut buffer = [0u8; 8192];
        let result = (|| -> Result<(), String> {
            loop {
                let read = entry
                    .read(&mut buffer)
                    .map_err(|e| format!("ErrorThemeInvalid: {e}"))?;
                if read == 0 {
                    break;
                }
                written = written
                    .checked_add(read as u64)
                    .ok_or_else(|| "ErrorThemeInvalid: theme too large".to_string())?;
                if written > MAX_FILE_BYTES
                    || total
                        .checked_add(written)
                        .is_none_or(|size| size > MAX_UNCOMPRESSED_BYTES)
                {
                    return Err("ErrorThemeInvalid: theme too large".to_string());
                }
                output
                    .write_all(&buffer[..read])
                    .map_err(|e| format!("ErrorThemeInstall: {e}"))?;
            }
            Ok(())
        })();
        drop(output);
        if result.is_err() {
            let _ = fs::remove_file(&full);
            result?;
        }
        total += written;
    }
    Ok(())
}

fn find_theme_dir(staging: &Path) -> Option<PathBuf> {
    // The archive may hold a single theme folder, or the theme laid out at its root.
    if staging.join(MANIFEST_FILE_NAME).is_file() || staging.join(ICON_FOLDER).is_dir() {
        return Some(staging.to_path_buf());
    }
    for entry in fs::read_dir(staging).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.join(MANIFEST_FILE_NAME).is_file() {
            return Some(path);
        }
    }
    None
}

/// Validates the manifests the same way C# `ThemePackTransfer.Validate` does.
fn validate_theme(directory: &Path) -> Result<(), String> {
    let manifest = load_manifest(directory).ok_or("ErrorThemeInvalid")?;
    if manifest.author.chars().count() > 200 || manifest.description.chars().count() > 1000 {
        return Err("ErrorThemeInvalid: manifest fields too long".to_string());
    }

    let icons = directory.join(ICON_FOLDER).join("pack.json");
    if icons.is_file() {
        let value = read_json_object(&icons, MAX_MANIFEST_BYTES)
            .ok_or_else(|| "ErrorThemeInvalid: icons/pack.json".to_string())?;
        if json_get_ci(&value, "icons")
            .and_then(Value::as_object)
            .is_some_and(|map| map.len() > MAX_FILE_COUNT)
        {
            return Err("ErrorThemeInvalid: too many icon mappings".to_string());
        }
        if let Some(fallback) = json_get_ci(&value, "fallback").and_then(Value::as_object)
            && let Some(scale) = object_get_ci(fallback, "iconScale").and_then(Value::as_f64)
            && !(0.1..=2.0).contains(&scale)
        {
            return Err("ErrorThemeInvalid: icon scale out of range".to_string());
        }
    }

    let widgets = directory.join(WIDGET_FOLDER).join("pack.json");
    if widgets.is_file() {
        let value = read_json_object(&widgets, MAX_WIDGET_BYTES)
            .ok_or_else(|| "ErrorThemeInvalid: widgets/pack.json".to_string())?;
        for key in ["tokens", "dark", "light"] {
            if json_get_ci(&value, key)
                .and_then(Value::as_object)
                .is_some_and(|map| map.len() > MAX_FILE_COUNT)
            {
                return Err("ErrorThemeInvalid: too many widget tokens".to_string());
            }
        }
    }
    Ok(())
}

/// Imports a `.ottheme` file into `root` and returns the new theme id.
pub fn import_theme(root: &Path, archive: &Path) -> Result<String, String> {
    if !archive.is_file() {
        return Err("ErrorThemeMissing".to_string());
    }
    if fs::metadata(archive)
        .map_err(|e| format!("ErrorThemeInstall: {e}"))?
        .len()
        > MAX_UNCOMPRESSED_BYTES
    {
        return Err("ErrorThemeInvalid".to_string());
    }
    fs::create_dir_all(root).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
    let staging = temp_path(root, "import");
    fs::create_dir_all(&staging).map_err(|e| format!("ErrorThemeInstall: {e}"))?;

    let imported = (|| -> Result<String, String> {
        let file = File::open(archive).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
        let mut zip = ZipArchive::new(file).map_err(|e| format!("ErrorThemeInvalid: {e}"))?;
        extract_safe(&mut zip, &staging)?;

        let theme_dir = find_theme_dir(&staging).ok_or("ErrorThemeInvalid")?;
        validate_theme(&theme_dir)?;
        let manifest = load_manifest(&theme_dir).ok_or("ErrorThemeInvalid")?;

        let id = unique_id(root, Some(&manifest.name));
        let target = root.join(&id);
        if same_path(&theme_dir, &target) || target.exists() {
            return Err("ErrorThemeInvalid".to_string());
        }
        fs::rename(&theme_dir, &target).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
        Ok(id)
    })();

    let _ = fs::remove_dir_all(&staging);
    imported
}

/// Deletes a theme folder after an id safety check.
pub fn delete_theme(root: &Path, id: &str) -> Result<(), String> {
    let Some(path) = theme_path(root, id) else {
        return Err("ErrorThemeInvalid".to_string());
    };
    if !path.is_dir() {
        return Err("ErrorThemeMissing".to_string());
    }
    fs::remove_dir_all(&path).map_err(|e| format!("ErrorThemeInstall: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("ot-theme-{tag}-{}", unique_suffix()));
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

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        let mut writer = ZipWriter::new(File::create(path).expect("create zip"));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, bytes) in entries {
            writer.start_file(*name, options).expect("start file");
            writer.write_all(bytes).expect("write entry");
        }
        writer.finish().expect("finish zip");
    }

    #[test]
    fn ids_are_sanitized_and_validated() {
        assert_eq!(sanitize_id("  Cat/PP!  ").as_deref(), Some("Cat-PP"));
        assert_eq!(sanitize_id("!!!"), None);
        assert_eq!(sanitize_id(""), None);
        assert!(is_safe_id("tokyo-night"));
        assert!(is_safe_id("Tema_2"));
        assert!(!is_safe_id("../escape"));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id("a..b"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id(&"a".repeat(121)));
        assert_eq!(built_in_ids().len(), 6);
        assert!(is_built_in("Catppuccin"));
        assert!(!is_built_in("custom"));
    }

    #[test]
    fn relaxed_json_keeps_string_slashes_and_drops_comments() {
        let json = relaxed_json(
            "{\n // note\n \"url\":\"https://x/y\", /* block\n comment */ \"n\": 1, }",
        );
        let value: Value = serde_json::from_str(&json).expect("valid relaxed json");
        assert_eq!(value["url"], "https://x/y");
        assert_eq!(value["n"], 1);
    }

    #[test]
    fn discover_reads_manifest_and_widget_tokens() {
        let temp = TempDir::new("discover");
        let root = temp.path().join("themes");
        let theme = root.join("demo");
        write(
            &theme.join("theme.json"),
            r#"{ /* comment */ "NAME": "Demo Theme", "author": "A", "description": "D",
                 "preserveOriginalIcons": true, }"#,
        );
        write(
            &theme.join("widgets/pack.json"),
            r#"{"tokens":{"WidgetRadius":8}}"#,
        );

        let packs = discover(&root);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].id, "demo");
        assert_eq!(packs[0].manifest.name, "Demo Theme");
        assert_eq!(packs[0].manifest.author, "A");
        assert_eq!(packs[0].manifest.description, "D");
        assert!(packs[0].manifest.preserve_original_icons);
        assert_eq!(read_widget_tokens(&packs[0])["tokens"]["WidgetRadius"], 8);

        let without_widgets = ThemePack {
            id: "none".to_string(),
            directory: root.join("none"),
            manifest: ThemeManifest::default(),
        };
        assert!(
            read_widget_tokens(&without_widgets)
                .as_object()
                .is_some_and(Map::is_empty)
        );
    }

    #[test]
    fn export_delete_import_roundtrip() {
        let temp = TempDir::new("roundtrip");
        let root = temp.path().join("themes");
        let theme = root.join("source");
        write(
            &theme.join("theme.json"),
            r#"{"name":"My Theme","author":"Me"}"#,
        );
        write(
            &theme.join("icons/pack.json"),
            r#"{"icons":{"@start":"icons/start.png"}}"#,
        );
        write(&theme.join("icons/start.png"), "fake png");
        write(
            &theme.join("widgets/pack.json"),
            r#"{"tokens":{"WidgetRadius":12}}"#,
        );

        let archive = temp.path().join("out/theme.ottheme");
        export_theme(&root, "source", &archive).expect("export");

        let mut zip = ZipArchive::new(File::open(&archive).expect("open zip")).expect("read zip");
        let names: Vec<String> = (0..zip.len())
            .map(|index| zip.by_index(index).expect("entry").name().to_string())
            .collect();
        assert!(names.iter().all(|name| name.starts_with("source/")));
        assert!(names.contains(&"source/theme.json".to_string()));
        assert!(names.contains(&"source/icons/start.png".to_string()));
        drop(zip);

        delete_theme(&root, "source").expect("delete");
        assert!(!theme.exists());

        let id = import_theme(&root, &archive).expect("import");
        assert_eq!(id, "My-Theme");
        let imported = root.join(&id);
        assert!(imported.join("theme.json").is_file());
        assert!(imported.join("icons/start.png").is_file());
        let packs = discover(&root);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].manifest.name, "My Theme");
        assert_eq!(read_widget_tokens(&packs[0])["tokens"]["WidgetRadius"], 12);
    }

    #[test]
    fn import_accepts_theme_laid_out_at_archive_root() {
        let temp = TempDir::new("root-layout");
        let root = temp.path().join("themes");
        let archive = temp.path().join("root.ottheme");
        make_zip(
            &archive,
            &[
                ("theme.json", br#"{"name":"Root Layout"}"#),
                ("widgets/pack.json", b"{}"),
            ],
        );
        let id = import_theme(&root, &archive).expect("import root layout");
        assert_eq!(id, "Root-Layout");
        assert!(root.join(id).join("theme.json").is_file());
    }

    #[test]
    fn import_rejects_unsafe_and_invalid_archives() {
        let temp = TempDir::new("reject");
        let root = temp.path().join("themes");
        let traversal = temp.path().join("traversal.ottheme");
        make_zip(
            &traversal,
            &[
                ("theme.json", br#"{"name":"Evil"}"#),
                ("../escape.txt", b"bad"),
            ],
        );
        assert!(import_theme(&root, &traversal).is_err());
        assert!(!temp.path().join("escape.txt").exists());

        let wrong_type = temp.path().join("type.ottheme");
        make_zip(
            &wrong_type,
            &[
                ("theme.json", br#"{"name":"Evil"}"#),
                ("payload.exe", b"bad"),
            ],
        );
        assert!(import_theme(&root, &wrong_type).is_err());

        let no_manifest = temp.path().join("none.ottheme");
        make_zip(&no_manifest, &[("readme.md", b"hello")]);
        assert!(import_theme(&root, &no_manifest).is_err());

        let long_fields = temp.path().join("long.ottheme");
        let author = "a".repeat(201);
        let manifest = format!(r#"{{"name":"Long","author":"{author}"}}"#);
        make_zip(&long_fields, &[("theme.json", manifest.as_bytes())]);
        assert!(import_theme(&root, &long_fields).is_err());

        assert!(delete_theme(&root, "../escape").is_err());
        assert!(delete_theme(&root, "missing").is_err());
        assert!(export_theme(&root, "../escape", &temp.path().join("x.ottheme")).is_err());
        assert!(export_theme(&root, "missing", &temp.path().join("x.ottheme")).is_err());
    }
}
