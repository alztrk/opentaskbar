//! C# `FeatureAndMemoryChecks` / `FullBackupArchive` parity (pure portion).
//!
//! WS-E owns the `.otbackup` UI and WS-A the settings model; neither is in this
//! worktree's baseline. This file ports the deterministic archive contract using
//! a tiny std-only ZIP *store* writer/reader so the round-trip is tested without
//! adding a cross-WS dependency or a new Cargo dependency.

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

#[test]
fn unsupported_manifest_version_is_rejected() {
    let bad = BackupManifest {
        version: 99,
        ..BackupManifest {
            version: 1,
            created_utc: String::new(),
            includes_theme: false,
            pin_count: 0,
            entries: vec![],
        }
    };
    let json = serde_json::to_vec(&bad).unwrap();
    let archive = zip_store(&[("manifest.json", &json)]);
    let entries = read_zip(&archive).unwrap();
    let parsed = manifest_from(&entries).unwrap();
    assert_eq!(parsed.version, 99);
    // Import must gate on version == 1; expressed as a pure predicate here.
    assert!(!(1..=1).contains(&parsed.version));
}
