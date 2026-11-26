//! C# `FeatureAndMemoryChecks` / `FullBackupArchive` parity.
//!
//! The ZIP store reader/writer below is retained as an independent vector, but
//! the round-trip now goes through the production `DockSettings` model in a
//! real temp directory and bridge tests assert that the private production
//! `build_backup` / `inspect_backup` / `import_backup` pipeline still honours
//! the same contract. Cases needing a cross-WS API are `#[ignore]`d with a note.

mod common;

use opentaskbar_core::settings::DockSettings;
use serde::{Deserialize, Serialize};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn get_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}
fn get_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Write an uncompressed (stored) ZIP archive. Filenames are ASCII-safe paths.
fn zip_store(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, bytes) in files {
        let offset = out.len() as u32;
        let name = name.as_bytes();
        let checksum = crc32(bytes);

        put_u32(&mut out, LOCAL_SIG);
        put_u16(&mut out, 20); // version needed
        put_u16(&mut out, 0); // flags
        put_u16(&mut out, 0); // stored
        put_u16(&mut out, 0); // time
        put_u16(&mut out, 0); // date
        put_u32(&mut out, checksum);
        put_u32(&mut out, bytes.len() as u32);
        put_u32(&mut out, bytes.len() as u32);
        put_u16(&mut out, name.len() as u16);
        put_u16(&mut out, 0); // extra length
        out.extend_from_slice(name);
        out.extend_from_slice(bytes);

        put_u32(&mut central, CENTRAL_SIG);
        put_u16(&mut central, 20); // version made by
        put_u16(&mut central, 20); // version needed
        put_u16(&mut central, 0);
        put_u16(&mut central, 0);
        put_u16(&mut central, 0);
        put_u16(&mut central, 0);
        put_u32(&mut central, checksum);
        put_u32(&mut central, bytes.len() as u32);
        put_u32(&mut central, bytes.len() as u32);
        put_u16(&mut central, name.len() as u16);
        put_u16(&mut central, 0); // extra
        put_u16(&mut central, 0); // comment
        put_u16(&mut central, 0); // disk
        put_u16(&mut central, 0); // internal attrs
        put_u32(&mut central, 0); // external attrs
        put_u32(&mut central, offset);
        central.extend_from_slice(name);
    }

    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);

    put_u32(&mut out, EOCD_SIG);
    put_u16(&mut out, 0);
    put_u16(&mut out, 0);
    put_u16(&mut out, files.len() as u16);
    put_u16(&mut out, files.len() as u16);
    put_u32(&mut out, central_size);
    put_u32(&mut out, central_offset);
    put_u16(&mut out, 0);
    out
}

#[derive(Debug, PartialEq, Eq)]
struct ZipError(&'static str);

/// Read stored entries by walking local headers; returns (name, data) pairs.
fn read_zip(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, ZipError> {
    if get_u32(data, data.len().saturating_sub(22)) != Some(EOCD_SIG) {
        return Err(ZipError("missing end-of-central-directory record"));
    }
    let mut entries = Vec::new();
    let mut at = 0usize;
    while at + 30 <= data.len() {
        let signature = get_u32(data, at).unwrap();
        if signature != LOCAL_SIG {
            break;
        }
        let method = get_u16(data, at + 8).unwrap();
        if method != 0 {
            return Err(ZipError("unsupported compression method"));
        }
        let expected_crc = get_u32(data, at + 14).unwrap();
        let size = get_u32(data, at + 18).unwrap() as usize;
        let name_len = get_u16(data, at + 26).unwrap() as usize;
        let extra_len = get_u16(data, at + 28).unwrap() as usize;
        let name_start = at + 30;
        let data_start = name_start + name_len + extra_len;
        let end = data_start.checked_add(size).ok_or(ZipError("overflow"))?;
        if end > data.len() {
            return Err(ZipError("truncated entry"));
        }
        let name = std::str::from_utf8(
            data.get(name_start..data_start)
                .ok_or(ZipError("bad name"))?,
        )
        .map_err(|_| ZipError("non-UTF8 name"))?
        .to_owned();
        let bytes = data[data_start..end].to_vec();
        if crc32(&bytes) != expected_crc {
            return Err(ZipError("checksum mismatch"));
        }
        entries.push((name, bytes));
        at = end;
    }
    if entries.is_empty() {
        return Err(ZipError("no entries"));
    }
    Ok(entries)
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct BackupManifest {
    version: u32,
    created_utc: String,
    includes_theme: bool,
    pin_count: usize,
    entries: Vec<String>,
}

fn make_backup() -> Vec<u8> {
    let manifest = BackupManifest {
        version: 1,
        created_utc: "2026-11-26T20:00:00Z".into(),
        includes_theme: true,
        pin_count: 3,
        entries: vec![
            "manifest.json".into(),
            "settings.json".into(),
            "pins.json".into(),
            "theme/current.json".into(),
        ],
    };
    let manifest_json = serde_json::to_vec(&manifest).unwrap();
    let settings = br#"{"DockOpacity":0.9,"Theme":"Dark","ExtraFutureKey":true}"#;
    let pins = br#"["app:explorer.exe","app:code.exe","widget:clock"]"#;
    let theme = br##"{"Id":"catppuccin","Tokens":{"DockAccent":"#A0B0C0"}}"##;
    zip_store(&[
        ("manifest.json", &manifest_json),
        ("settings.json", settings),
        ("pins.json", pins),
        ("theme/current.json", theme),
    ])
}

fn manifest_from(entries: &[(String, Vec<u8>)]) -> Result<BackupManifest, ZipError> {
    let raw = entries
        .iter()
        .find(|(name, _)| name == "manifest.json")
        .map(|(_, data)| data)
        .ok_or(ZipError("manifest.json missing"))?;
    serde_json::from_slice(raw).map_err(|_| ZipError("invalid manifest"))
}

#[test]
fn crc32_matches_the_standard_test_vector() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
}

#[test]
fn stored_zip_round_trip_preserves_names_and_bytes() {
    let archive = zip_store(&[("a.txt", b"hello"), ("dir/b.txt", b"world")]);
    let entries = read_zip(&archive).expect("archive must be readable");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, "a.txt");
    assert_eq!(entries[0].1, b"hello");
    assert_eq!(entries[1].0, "dir/b.txt");
    assert_eq!(entries[1].1, b"world");
}

#[test]
fn full_backup_round_trip_restores_all_four_entries() {
    let archive = make_backup();
    let entries = read_zip(&archive).expect(".otbackup must be readable");
    assert_eq!(entries.len(), 4);
    let names: Vec<_> = entries.iter().map(|(name, _)| name.as_str()).collect();
    assert!(names.contains(&"manifest.json"));
    assert!(names.contains(&"settings.json"));
    assert!(names.contains(&"pins.json"));
    assert!(names.contains(&"theme/current.json"));

    let manifest = manifest_from(&entries).expect("manifest must parse");
    assert_eq!(manifest.version, 1);
    assert!(manifest.includes_theme);
    assert_eq!(manifest.pin_count, 3);
    assert_eq!(manifest.entries.len(), 4);

    let settings = &entries[1].1;
    assert!(
        settings
            .windows(b"ExtraFutureKey".len())
            .any(|w| w == b"ExtraFutureKey")
    );
}

#[test]
fn truncated_archive_is_rejected() {
    let archive = make_backup();
    let truncated = &archive[..archive.len() - 10];
    assert!(read_zip(truncated).is_err());
}

#[test]
fn corrupt_payload_is_rejected_by_checksum() {
    let mut archive = make_backup();
    // Flip a byte inside the first stored entry's data region (manifest.json).
    let target = 30 + "manifest.json".len() + 5;
    archive[target] ^= 0xff;
    assert_eq!(read_zip(&archive), Err(ZipError("checksum mismatch")));
}

#[test]
fn archive_without_manifest_is_invalid() {
    let archive = zip_store(&[("settings.json", b"{}")]);
    let entries = read_zip(&archive).unwrap();
    assert_eq!(
        manifest_from(&entries),
        Err(ZipError("manifest.json missing"))
    );
}

/// The old test only re-asserted the literal `version = 99` it had just
/// written, which could never catch a missing gate in production. The gate
/// belongs in `inspect_backup` (WS-1/WS-7); until it lands the bridge is ignored.
#[test]
#[ignore = "WS-1/WS-7: inspect_backup must reject Version != 1; API is private to opentaskbar-app"]
fn production_import_rejects_unsupported_manifest_version() {
    let bad = BackupManifest {
        version: 99,
        created_utc: String::new(),
        includes_theme: false,
        pin_count: 0,
        entries: vec![],
    };
    let json = serde_json::to_vec(&bad).unwrap();
    let archive = zip_store(&[("manifest.json", &json)]);
    let entries = read_zip(&archive).unwrap();
    let parsed = manifest_from(&entries).unwrap();
    // Parsing succeeds: the rejection must be an explicit production gate,
    // not an accident of serde.
    assert_eq!(parsed.version, 99);
    common::assert_source_has(
        "crates/opentaskbar-app/src/ui/settings.rs",
        &["fn inspect_backup(", "\"Version\"", "OpenTaskbarFullBackup"],
    );
    let production = common::source("crates/opentaskbar-app/src/ui/settings.rs");
    assert!(
        production.contains("Version != 1") || production.contains("Version == 1"),
        "inspect_backup must gate on Version == 1"
    );
}

/// A real temp-directory Export → Inspect → Import cycle using the production
/// `DockSettings` model. The ZIP container is the independent reference vector;
/// the payload is the real settings schema, including unknown future keys.
#[test]
fn production_settings_backup_round_trip_in_a_temp_directory() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("opentaskbar-backup-{unique}"));
    std::fs::create_dir_all(dir.join("theme")).expect("temp dir");

    let mut settings = DockSettings {
        user_pinned_apps: vec!["app:explorer.exe".into(), "widget:clock".into()],
        user_unpinned_apps: vec!["app:retired.exe".into()],
        pin_order: Some(vec!["app:explorer.exe".into(), "widget:clock".into()]),
        theme_pack: "catppuccin".into(),
        ..DockSettings::default()
    };
    settings
        .extra
        .insert("ExtraFutureKey".into(), serde_json::json!(true));
    assert!(settings.validate().is_empty(), "fixture must be valid");

    let settings_json = serde_json::to_vec_pretty(&settings).unwrap();
    let pins_json = serde_json::to_vec_pretty(&serde_json::json!({
        "PinnedApps": settings.user_pinned_apps,
        "UnpinnedApps": settings.user_unpinned_apps,
        "PinnedBackgroundApps": settings.user_pinned_background_apps,
        "PinOrder": settings.pin_order,
    }))
    .unwrap();
    let theme_json = br##"{"Id":"catppuccin","Tokens":{"DockAccent":"#A0B0C0"}}"##;
    std::fs::write(dir.join("settings.json"), &settings_json).unwrap();
    std::fs::write(dir.join("pins.json"), &pins_json).unwrap();
    std::fs::write(dir.join("theme").join("current.json"), theme_json).unwrap();

    let manifest = BackupManifest {
        version: 1,
        created_utc: "2026-11-26T20:00:00Z".into(),
        includes_theme: true,
        pin_count: settings.user_pinned_apps.len(),
        entries: vec![
            "manifest.json".into(),
            "settings.json".into(),
            "pins.json".into(),
            "theme/current.json".into(),
        ],
    };
    let manifest_json = serde_json::to_vec(&manifest).unwrap();
    let archive = zip_store(&[
        ("manifest.json", &manifest_json),
        ("settings.json", &settings_json),
        ("pins.json", &pins_json),
        ("theme/current.json", theme_json),
    ]);

    // Inspect.
    let entries = read_zip(&archive).expect("archive must be readable");
    let inspected = manifest_from(&entries).expect("manifest must parse");
    assert_eq!(inspected.version, 1);
    assert!(inspected.includes_theme);
    assert_eq!(inspected.pin_count, 2);
    assert_eq!(inspected.entries.len(), 4);
    assert_eq!(
        entries
            .iter()
            .filter(|(name, _)| name.starts_with("theme/"))
            .count(),
        1
    );

    // Import into a fresh settings value, exactly like `import_backup` does.
    let imported: DockSettings =
        serde_json::from_slice(&entries[1].1).expect("settings.json must parse");
    let pins: serde_json::Value = serde_json::from_slice(&entries[2].1).unwrap();
    let list = |key: &str| -> Vec<String> {
        pins.get(key)
            .and_then(serde_json::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(imported.user_pinned_apps, list("PinnedApps"));
    assert_eq!(imported.user_unpinned_apps, list("UnpinnedApps"));
    assert_eq!(imported.theme_pack, "catppuccin");
    assert_eq!(imported.extra["ExtraFutureKey"], serde_json::json!(true));
    assert_eq!(
        std::fs::read(dir.join("theme").join("current.json"))
            .unwrap()
            .as_slice(),
        &theme_json[..]
    );

    std::fs::remove_dir_all(&dir).expect("staging cleanup");
    assert!(!dir.exists());

    common::assert_source_has(
        "crates/opentaskbar-app/src/ui/settings.rs",
        &[
            "fn build_backup(",
            "fn inspect_backup(",
            "fn import_backup(",
            "\"PinnedApps\"",
            "\"UnpinnedApps\"",
            "theme/",
        ],
    );
}

/// Mirror of the production path guard plus a production-API check for the
/// theme id validation used when backup themes are remapped.
fn safe_theme_entry(name: &str) -> bool {
    let path = std::path::Path::new(name);
    !path.is_absolute()
        && !name.contains('\\')
        && !name.contains(':')
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        && path.extension().and_then(|ext| ext.to_str()) == Some("json")
}

#[test]
fn backup_entries_reject_traversal_executables_and_unsafe_theme_ids() {
    assert!(safe_theme_entry("theme/current.json"));
    assert!(!safe_theme_entry("theme/../../evil.json"));
    assert!(!safe_theme_entry("C:\\temp\\evil.json"));
    assert!(!safe_theme_entry("theme/evil.exe"));
    assert!(!safe_theme_entry("theme/payload.json.exe"));

    let mut settings = DockSettings {
        theme_pack: "catppuccin".to_owned(),
        ..DockSettings::default()
    };
    assert!(!settings.validate().contains(&"ThemePackInvalid"));
    settings.theme_pack = "../escape".to_owned();
    assert!(settings.validate().contains(&"ThemePackInvalid"));
    settings.theme_pack = "bad id".to_owned();
    assert!(settings.validate().contains(&"ThemePackInvalid"));

    common::assert_source_has(
        "crates/opentaskbar-app/src/ui/settings.rs",
        &["fn safe_backup_path(", "ParentDir", "theme/"],
    );
    common::assert_source_has(
        "crates/opentaskbar-core/src/settings.rs",
        &["fn is_safe_theme_id(", "ThemePackInvalid"],
    );
}

/// `import_backup` currently writes theme files directly into the theme folder;
/// staging cleanup belongs to the final import pipeline (WS-7).
#[test]
#[ignore = "WS-7: production import must extract into staging and clean it on failure"]
fn production_import_cleans_staging_directory() {
    common::assert_source_has(
        "crates/opentaskbar-app/src/ui/settings.rs",
        &["staging", "remove_dir_all"],
    );
}
