//! Folder-based theme packs: discovery, portable `.ottheme` export/import and deletion.
//!
//! Windows-independent Rust implementation of `ThemePacks`, `ThemePackTransfer` and `WidgetPack`.
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

use gpui::AssetSource;
use image::{Rgba, RgbaImage};

use crate::design::fonts::{BUNDLED_THEME_IDS, EmbeddedAssets};
use crate::design::tokens::{CornerRadius, Gradient, Rgba as TokenRgba, TokenMap};
use crate::design::widget_pack::{WidgetPackManifest, WidgetTokenResolution};

use super::icon_pack::{
    IconPack, IconPackCorner, IconPackFallback, IconPackShape, MANIFEST_FILE_NAME as ICON_MANIFEST,
};
use super::json_ext::{json_get_ci, object_get_ci, relaxed_json, string_ci};
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// File at the root of a theme folder that describes it.
pub const MANIFEST_FILE_NAME: &str = "theme.json";
/// Optional custom icon pack inside a theme.
pub const ICON_FOLDER: &str = "icons";
/// Optional design-token overrides inside a theme.
pub const WIDGET_FOLDER: &str = "widgets";
pub const RENDER_SIZE: u32 = 128;
pub const MAX_AUTHOR_LEN: usize = 200;
pub const MAX_DESCRIPTION_LEN: usize = 1000;

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
///
/// The optional `icons`/`widgets`/`problems` fields carry WS-F's richer reference parity view of a
/// pack; the WIP discovery/export/import APIs keep using `id`, `directory` and `manifest`.
#[derive(Clone, Debug, Default)]
pub struct ThemePack {
    pub id: String,
    pub directory: PathBuf,
    pub manifest: ThemeManifest,
    pub icons: Option<IconPack>,
    pub widgets: Option<WidgetPackManifest>,
    pub problems: Vec<String>,
}

/// Themes bundled with the application; they keep native Windows icons even when no
/// custom icon pack is present.
pub fn built_in_ids() -> &'static [&'static str] {
    &BUNDLED_THEME_IDS
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
/// usable remains. Mirrors `ThemePackTransfer.Sanitize`.
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
    let mut pack = ThemePack::load(directory).ok()?;
    pack.id = id;
    Some(pack)
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
        let raw = entry
            .name()
            .map_err(|e| format!("ErrorThemeInvalid: {e}"))?
            .replace('\\', "/");
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

/// Validates the manifests the same way `ThemePackTransfer.Validate` does.
fn validate_theme(directory: &Path) -> Result<(), String> {
    let manifest = load_manifest(directory).ok_or("ErrorThemeInvalid")?;
    if manifest.author.chars().count() > MAX_AUTHOR_LEN
        || manifest.description.chars().count() > MAX_DESCRIPTION_LEN
    {
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

    #[test]
    fn theme_root_is_the_shared_uppercase_themes_folder() {
        assert_eq!(root(), opentaskbar_core::paths::theme_root());
        assert_eq!(root().file_name().unwrap(), "Themes");
        assert_eq!(
            root().parent(),
            Some(opentaskbar_core::paths::shared_app_data_dir().as_path())
        );
    }

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
            ..ThemePack::default()
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
            .map(|index| {
                zip.by_index(index)
                    .expect("entry")
                    .name()
                    .expect("entry name")
                    .to_string()
            })
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

// ---- reference theme-pack parity (WS-F) ----

/// Converts WS-F's optional-string manifest view into the WIP String-based view.
fn to_theme_manifest(value: &ThemePackManifest) -> ThemeManifest {
    ThemeManifest {
        name: value.name.clone().unwrap_or_default(),
        author: value.author.clone().unwrap_or_default(),
        description: value.description.clone().unwrap_or_default(),
        preserve_original_icons: value.preserve_original_icons,
    }
}

/// WS-F alias for [`is_built_in`].
pub fn is_built_in_theme(id: &str) -> bool {
    is_built_in(id)
}

/// WS-F alias for [`sanitize_id`].
pub fn sanitize(name: &str) -> Option<String> {
    sanitize_id(name)
}

/// WS-F `unique_id` entry point (the WIP helper accepts `Option<&str>`).
pub fn unique_id_named(root: &Path, name: &str) -> String {
    unique_id(root, Some(name))
}

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

impl ThemePack {
    pub fn load(directory: &Path) -> Result<Self, String> {
        let id = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let manifest_path = directory.join(MANIFEST_FILE_NAME);
        let text = fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        // WS-F's strict parser is tried first. WIP manifests allow block comments and
        // case-insensitive keys, so fall back to the lenient reader when needed.
        let manifest = ThemePackManifest::from_str(&text)
            .ok()
            .filter(|parsed| {
                parsed
                    .name
                    .as_ref()
                    .is_some_and(|name| !name.trim().is_empty())
            })
            .or_else(|| {
                load_manifest(directory).map(|legacy| ThemePackManifest {
                    name: Some(legacy.name).filter(|name| !name.is_empty()),
                    author: Some(legacy.author).filter(|author| !author.is_empty()),
                    description: Some(legacy.description).filter(|text| !text.is_empty()),
                    preserve_original_icons: legacy.preserve_original_icons,
                })
            })
            .ok_or_else(|| format!("{}: invalid theme.json", manifest_path.display()))?;
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
            manifest: to_theme_manifest(&manifest),
            icons,
            widgets,
            problems,
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

/// User theme folder (`%LOCALAPPDATA%\OpenTaskbar\Themes`, uppercase T).
pub fn root() -> PathBuf {
    opentaskbar_core::paths::theme_root()
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

fn is_safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && !path.to_string_lossy().contains("..")
}

/// Validates manifest field lengths (`ThemePackTransfer.Validate`).
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
    let id = unique_id(&themes_root, Some(name));
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

/// Applies a selected theme pack's widget tokens as a layered `TokenMap` for `ThemeManager`.
pub fn widgets_override(pack: &ThemePack, theme: opentaskbar_core::settings::AppTheme) -> TokenMap {
    pack.resolve_widgets(theme).tokens
}

/// Five representative swatches extracted from a theme pack for settings cards and pickers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePreviewSwatches {
    pub surface: crate::design::tokens::Rgba,
    pub card: crate::design::tokens::Rgba,
    pub accent: crate::design::tokens::Rgba,
    pub text: crate::design::tokens::Rgba,
    pub border: crate::design::tokens::Rgba,
}

/// Resolves five representative preview swatches from `pack`'s widget tokens for `theme`,
/// falling back cleanly to `design::palette` for any unset token.
pub fn preview_swatches(
    pack: &ThemePack,
    theme: opentaskbar_core::settings::AppTheme,
) -> ThemePreviewSwatches {
    let effective = match theme {
        opentaskbar_core::settings::AppTheme::Dark => opentaskbar_core::settings::AppTheme::Dark,
        opentaskbar_core::settings::AppTheme::Light => opentaskbar_core::settings::AppTheme::Light,
        opentaskbar_core::settings::AppTheme::System => {
            if crate::design::windows_palette::system_is_dark() {
                opentaskbar_core::settings::AppTheme::Dark
            } else {
                opentaskbar_core::settings::AppTheme::Light
            }
        }
    };
    let is_dark = effective == opentaskbar_core::settings::AppTheme::Dark;
    let fallback = crate::design::palette(is_dark);
    let tokens = widgets_override(pack, effective);
    let pick = |keys: &[&str], default_rgb: crate::design::Rgb| -> TokenRgba {
        keys.iter()
            .find_map(|key| {
                tokens
                    .get(*key)
                    .and_then(crate::design::theme::token_rgba)
                    .filter(|color| color.a > 0)
            })
            .unwrap_or_else(|| TokenRgba::from_rgb(default_rgb.0, default_rgb.1, default_rgb.2))
    };
    ThemePreviewSwatches {
        surface: pick(&["DockMaterial"], fallback.dock_pill),
        card: pick(
            &["SettingsControl", "WidgetCard", "SettingsBackground"],
            fallback.elevated,
        ),
        accent: pick(&["AccentBrush", "WidgetAccent"], fallback.accent),
        text: pick(&["TextPrimary"], fallback.text_primary),
        border: pick(
            &["DockBorderMaterial", "StrokeSubtle", "WidgetCardStroke"],
            fallback.border_rgb,
        ),
    }
}

fn write_file_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "ErrorThemeInstall: missing parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
    let temporary = temp_path(parent, "tmp");
    fs::write(&temporary, contents).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
    if path.exists()
        && let Err(error) = fs::remove_file(path)
    {
        let _ = fs::remove_file(&temporary);
        return Err(format!("ErrorThemeInstall: {error}"));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("ErrorThemeInstall: {error}"));
    }
    Ok(())
}

/// Creates or updates a user-authored custom theme pack under `root` with `theme.json`
/// and `widgets/pack.json`. Rejects built-in theme IDs and unsafe folder slugs.
pub fn save_custom_theme(
    root: &Path,
    id: Option<&str>,
    manifest: &ThemePackManifest,
    widgets: &WidgetPackManifest,
) -> Result<ThemePack, String> {
    if let Some(error) = validate_manifest(manifest) {
        return Err(error.to_owned());
    }
    fs::create_dir_all(root).map_err(|e| format!("ErrorThemeInstall: {e}"))?;
    let folder_id = if let Some(explicit) = id {
        let trimmed = explicit.trim();
        if !is_safe_id(trimmed) {
            return Err("ErrorThemeInvalid: invalid theme id".to_owned());
        }
        if is_built_in_theme(trimmed) {
            return Err(format!("Built-in theme '{trimmed}' cannot be overwritten."));
        }
        trimmed.to_owned()
    } else {
        let mut candidate = unique_id(root, manifest.name.as_deref());
        if is_built_in_theme(&candidate) {
            let custom_name = format!("{candidate}-custom");
            candidate = unique_id(root, Some(&custom_name));
        }
        candidate
    };
    let Some(target) = theme_path(root, &folder_id) else {
        return Err("ErrorThemeInvalid".to_owned());
    };
    let manifest_json = manifest.to_string_pretty()?;
    let widgets_json =
        serde_json::to_string_pretty(&widgets.to_value()).map_err(|e| e.to_string())?;
    if widgets_json.len() as u64 > MAX_WIDGET_BYTES {
        return Err("ErrorThemeInvalid: widgets/pack.json too large".to_owned());
    }
    fs::create_dir_all(target.join(WIDGET_FOLDER))
        .map_err(|e| format!("ErrorThemeInstall: {e}"))?;
    write_file_atomic(&target.join(MANIFEST_FILE_NAME), &manifest_json)?;
    write_file_atomic(
        &target
            .join(WIDGET_FOLDER)
            .join(crate::design::widget_pack::MANIFEST_FILE_NAME),
        &widgets_json,
    )?;
    ThemePack::load(&target)
}

/// Shape used when clipping an icon onto its plate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlateShape {
    Circle,
    Rounded(CornerRadius),
}

/// `ThemePacks.CreateShape`: per-corner radii in pixels for a 128px plate.
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

/// `ThemePacks.RenderPlate`: plates an arbitrary icon onto a shaped, shaded tile.
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

/// `ThemePacks.PlateIcon`: default macOS-style squircle plate.
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

#[cfg(test)]
mod parity_tests {
    use super::*;
    use crate::design::tokens::TokenValue;

    #[test]
    fn built_in_ids_and_install_manifest_count() {
        // Shares the bundled theme files with the widget-pack/i18n tests.
        let _guard = crate::design::theme::theme_test_guard();
        assert_eq!(built_in_ids().len(), 6);
        assert!(is_built_in_theme("Tokyo-Night"));
        assert!(!is_built_in_theme("unknown"));
        let assets = EmbeddedAssets;
        assert_eq!(assets.list("themes").unwrap().len(), 14);
        for id in built_in_ids() {
            let manifest = ThemePack::load(&Path::new(crate::resource!("themes")).join(id))
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(manifest.id, *id);
            assert!(manifest.problems.is_empty());
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
        let _guard = crate::design::theme::theme_test_guard();
        let pack = ThemePack::load(Path::new(crate::resource!("themes/nord"))).unwrap();
        let tokens = widgets_override(&pack, opentaskbar_core::settings::AppTheme::Dark);
        assert!(tokens.contains_key("WidgetAccent"));
        assert!(matches!(
            tokens.get("DockMaterial"),
            Some(TokenValue::Gradient(_))
        ));
    }

    #[test]
    fn save_custom_theme_validates_and_round_trips_swatches_export_import() {
        let base = std::env::temp_dir().join(format!("ot-custom-theme-{}", unique_suffix()));
        let root = base.join("themes");
        let import_root = base.join("imported");
        fs::create_dir_all(&root).expect("create root");

        let manifest = ThemePackManifest {
            name: Some("Midnight Berry".to_owned()),
            author: Some("OpenTaskbar Test".to_owned()),
            description: Some("Custom theme roundtrip".to_owned()),
            preserve_original_icons: true,
        };
        let mut widgets = WidgetPackManifest {
            name: Some("Midnight Berry Widgets".to_owned()),
            ..WidgetPackManifest::default()
        };
        widgets
            .dark
            .insert("DockMaterial".to_owned(), Value::from("#FF21162E"));
        widgets
            .dark
            .insert("SettingsControl".to_owned(), Value::from("#FF312242"));
        widgets
            .dark
            .insert("AccentBrush".to_owned(), Value::from("#FFE06C9F"));
        widgets
            .dark
            .insert("TextPrimary".to_owned(), Value::from("#FFF5EDF7"));
        widgets
            .dark
            .insert("DockBorderMaterial".to_owned(), Value::from("#FF9D6BFF"));

        // Built-in IDs (case-insensitive) and unsafe IDs must be rejected.
        assert!(save_custom_theme(&root, Some("nord"), &manifest, &widgets).is_err());
        assert!(save_custom_theme(&root, Some("Catppuccin"), &manifest, &widgets).is_err());
        assert!(save_custom_theme(&root, Some("../escape"), &manifest, &widgets).is_err());

        let saved = save_custom_theme(&root, None, &manifest, &widgets).expect("save custom");
        assert_eq!(saved.id, "Midnight-Berry");
        assert_eq!(saved.manifest.name, "Midnight Berry");
        assert!(saved.manifest.preserve_original_icons);

        let swatches = preview_swatches(&saved, opentaskbar_core::settings::AppTheme::Dark);
        assert_eq!(swatches.surface, TokenRgba::from_rgb(0x21, 0x16, 0x2E));
        assert_eq!(swatches.card, TokenRgba::from_rgb(0x31, 0x22, 0x42));
        assert_eq!(swatches.accent, TokenRgba::from_rgb(0xE0, 0x6C, 0x9F));
        assert_eq!(swatches.text, TokenRgba::from_rgb(0xF5, 0xED, 0xF7));
        assert_eq!(swatches.border, TokenRgba::from_rgb(0x9D, 0x6B, 0xFF));

        let archive = base.join("midnight-berry.ottheme");
        export_theme(&root, &saved.id, &archive).expect("export custom theme");
        let imported_id = import_theme(&import_root, &archive).expect("import custom theme");
        let reloaded = ThemePack::load(&import_root.join(&imported_id)).expect("load imported");
        assert_eq!(
            preview_swatches(&reloaded, opentaskbar_core::settings::AppTheme::Dark),
            swatches
        );

        let _ = fs::remove_dir_all(&base);
    }
}
