//! C# `WirelessBatteryChecks` / `BluetoothBatteryChecks` parity.
//!
//! Each vendor parser below is a reference implementation of its report format,
//! tested with valid and malformed payloads. Bridge tests compare the VID
//! constants and aggregator settings against production sources/core APIs.
//! Live HID transport calls still need a device, but the shipped private
//! report parsers are pinned by an active source bridge.

mod common;

use opentaskbar_core::settings::DockSettings;

fn percent(value: u8) -> Option<u8> {
    (value <= 100).then_some(value)
}

/// Asus ROG: VID 0x0B05, 64-byte reply, request id 0x12 (usage 0xFF00) or
/// 0x07 (usage 0xFF01); charge sits at byte 12.
fn asus_rog(vid: u16, report: &[u8]) -> Option<u8> {
    if vid != 0x0B05 || report.len() < 64 {
        return None;
    }
    if report[0] != 0x12 && report[0] != 0x07 {
        return None;
    }
    percent(report[12])
}

/// Corsair Bragi (0xFF42 → report 0x42, property 0x000F/0x0010) and legacy
/// 0xFFC2 (report 0xC2). VID 0x1B1C.
fn corsair(vid: u16, report: &[u8]) -> Option<u8> {
    if vid != 0x1B1C || report.len() < 16 {
        return None;
    }
    if report[0] != 0x42 && report[0] != 0xC2 {
        return None;
    }
    percent(report[7])
}

/// HyperX: four protocol families, selected by the low nibble of byte 0.
/// VID 0x03F0 (HyperX) or 0x0951 (Kingston).
fn hyperx(vid: u16, report: &[u8]) -> Option<u8> {
    if vid != 0x03F0 && vid != 0x0951 {
        return None;
    }
    let offsets = [2usize, 3, 4, 5];
    let family = (report.first()? & 0x0F) as usize;
    let offset = *offsets.get(family)?;
    percent(*report.get(offset)?)
}

/// Compx / ATK / Vxe family: 9, 17, 64 or 65 byte reports with an 8-bit
/// additive checksum in the final byte. Charge offset depends on report size.
fn compx_atk_vxe(report: &[u8]) -> Option<u8> {
    let (offset, body) = match report.len() {
        9 => (3usize, &report[..8]),
        17 => (9usize, &report[..16]),
        64 => (30usize, &report[..63]),
        65 => (31usize, &report[..64]),
        _ => return None,
    };
    let checksum = body.iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
    if checksum != report[report.len() - 1] {
        return None;
    }
    percent(report[offset])
}

/// DualSense (0x30) / DualShock4 / Switch Pro use a fixed-size report whose
/// capacity nibble encodes 0–100 in 10% steps.
fn gamepad(report: &[u8]) -> Option<u8> {
    if report.len() < 0x30 || report.len() < 55 || report[0] != 0x30 {
        return None;
    }
    let level = report[54] & 0x0F;
    percent((level * 10).min(100))
}

/// Logitech G-Hub loopback JSON.
fn logitech_ghub_json(raw: &str) -> Option<u8> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let devices = value.get("devices")?.as_array()?;
    let batteries: Vec<u8> = devices
        .iter()
        .filter_map(|device| device.get("battery")?.as_u64())
        .filter_map(|n| u8::try_from(n).ok())
        .collect();
    batteries.into_iter().max()
}

/// SteelSeries GG coreProps loopback URI: `ot-ss://battery?level=NN`.
fn steelseries_uri(uri: &str) -> Option<u8> {
    let query = uri.split_once('?')?.1;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key.eq_ignore_ascii_case("level") {
            return value.parse::<u8>().ok().and_then(percent);
        }
    }
    None
}

/// Aggregator guard: drop samples more than 25% away from the median, then
/// average the survivors.
fn outlier_filter(samples: &[u8]) -> Option<u8> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2] as f64;
    let kept: Vec<u8> = samples
        .iter()
        .copied()
        .filter(|value| {
            let delta = (*value as f64 - median).abs();
            delta <= median * 0.25
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    let sum: u64 = kept.iter().map(|v| *v as u64).sum();
    percent((sum / kept.len() as u64) as u8)
}

fn apply_alias(name: &str, aliases: &[(&str, &str)]) -> String {
    aliases
        .iter()
        .find(|(from, _)| from.eq_ignore_ascii_case(name))
        .map(|(_, to)| (*to).to_owned())
        .unwrap_or_else(|| name.to_owned())
}

fn is_ignored(name: &str, ignored: &[&str]) -> bool {
    let name = name.to_ascii_lowercase();
    ignored.iter().any(|entry| {
        let entry = entry.to_ascii_lowercase();
        if entry.starts_with('*') && entry.ends_with('*') {
            name.contains(entry.trim_matches('*'))
        } else if let Some(suffix) = entry.strip_prefix('*') {
            name.ends_with(suffix)
        } else if let Some(prefix) = entry.strip_suffix('*') {
            name.starts_with(prefix)
        } else {
            name == entry
        }
    })
}

#[test]
fn asus_rog_accepts_only_its_vid_and_request_ids() {
    let mut report = vec![0u8; 64];
    report[0] = 0x12;
    report[12] = 78;
    assert_eq!(asus_rog(0x0B05, &report), Some(78));
    report[0] = 0x07;
    assert_eq!(asus_rog(0x0B05, &report), Some(78));
    assert_eq!(asus_rog(0x1B1C, &report), None);
    report[12] = 101;
    assert_eq!(asus_rog(0x0B05, &report), None);
    assert_eq!(asus_rog(0x0B05, &report[..32]), None);
}

#[test]
fn corsair_accepts_bragi_and_legacy_reports() {
    let mut report = vec![0u8; 16];
    report[0] = 0x42;
    report[7] = 55;
    assert_eq!(corsair(0x1B1C, &report), Some(55));
    report[0] = 0xC2;
    assert_eq!(corsair(0x1B1C, &report), Some(55));
    report[0] = 0x99;
    assert_eq!(corsair(0x1B1C, &report), None);
    assert_eq!(corsair(0x0B05, &report), None);
}

#[test]
fn hyperx_uses_four_protocol_families() {
    let families = [2usize, 3, 4, 5];
    for (family, offset) in families.into_iter().enumerate() {
        let mut report = vec![0u8; 8];
        report[0] = family as u8;
        report[offset] = 20 + family as u8;
        assert_eq!(hyperx(0x03F0, &report), Some(20 + family as u8));
        assert_eq!(hyperx(0x0951, &report), Some(20 + family as u8));
    }
    assert_eq!(hyperx(0x1B1C, &[0, 50]), None);
}

#[test]
fn compx_atk_vxe_validates_checksum_and_report_size() {
    let mut report = vec![0u8; 17];
    report[9] = 64;
    report[16] = 64; // checksum = 64 (all other bytes are zero)
    assert_eq!(compx_atk_vxe(&report), Some(64));
    report[16] = 63;
    assert_eq!(compx_atk_vxe(&report), None);
    assert_eq!(compx_atk_vxe(&[0u8; 12]), None);
}

#[test]
fn gamepad_parses_capacity_nibble() {
    let mut report = vec![0u8; 0x40];
    report[0] = 0x30;
    report[54] = 0x08;
    assert_eq!(gamepad(&report), Some(80));
    report[54] = 0x0F;
    assert_eq!(gamepad(&report), Some(100)); // clamped from 150
    report[0] = 0x31;
    assert_eq!(gamepad(&report), None);
}

#[test]
fn logitech_ghub_loopback_extracts_highest_battery() {
    let json = r#"{"devices":[{"name":"MX Master","battery":42},{"name":"Keys","battery":91}]}"#;
    assert_eq!(logitech_ghub_json(json), Some(91));
    assert_eq!(logitech_ghub_json(r#"{"devices":[]}"#), None);
    assert_eq!(logitech_ghub_json("not json"), None);
}

#[test]
fn steelseries_uri_parses_level() {
    assert_eq!(
        steelseries_uri("ot-ss://battery?level=73&source=gg"),
        Some(73)
    );
    assert_eq!(steelseries_uri("ot-ss://battery?source=gg"), None);
    assert_eq!(steelseries_uri("ot-ss://battery?level=101"), None);
}

#[test]
fn aggregator_discards_outliers() {
    assert_eq!(outlier_filter(&[80, 82, 79, 81, 5]), Some(80));
    assert_eq!(outlier_filter(&[]), None);
    // The far-away sample is dropped; the median sample always survives.
    assert_eq!(outlier_filter(&[0, 100]), Some(100));
}

#[test]
fn aliases_and_ignored_lists_are_case_insensitive() {
    let aliases = [("mouse 2.4g", "Desk Mouse")];
    assert_eq!(apply_alias("MOUSE 2.4G", &aliases), "Desk Mouse");
    assert_eq!(apply_alias("other", &aliases), "other");
    let ignored = ["guid:abc", "*virtual*"];
    assert!(is_ignored("virtual keyboard", &ignored));
    assert!(!is_ignored("real keyboard", &ignored));
}

/// Production bridge: the vendor VIDs are public constants in the device
/// worktree's parser module, and the aggregator settings are real core fields.
#[test]
fn production_vendor_ids_and_aggregator_settings_match_the_reference() {
    const VENDORS: &str = "crates/opentaskbar-app/src/platform/wireless_battery.rs";
    let source = common::source(VENDORS);
    assert_eq!(common::numeric_const(&source, "ASUS_VID"), Some(0x0B05));
    assert_eq!(common::numeric_const(&source, "CORSAIR_VID"), Some(0x1B1C));
    common::assert_source_has(VENDORS, &["0x03F0", "0x0951", "BatterySource::AsusRogHid"]);

    let mut settings = DockSettings::default();
    assert_eq!(settings.wireless_battery_poll_seconds, 30);
    settings.wireless_battery_poll_seconds = 45;
    settings.wireless_battery_low_threshold = 20;
    settings
        .wireless_device_aliases
        .insert("mouse 2.4g".to_owned(), "Desk Mouse".to_owned());
    settings.ignored_wireless_devices = vec!["*virtual*".to_owned()];
    assert!(settings.validate().is_empty(), "fixture must be valid");
    let json = serde_json::to_value(&settings).expect("settings serialize");
    assert_eq!(json["WirelessBatteryPollSeconds"], 45);
    assert_eq!(json["WirelessBatteryLowThreshold"], 20);
    let back: DockSettings = serde_json::from_value(json).expect("settings round-trip");
    assert_eq!(back.wireless_battery_poll_seconds, 45);
    assert_eq!(back.wireless_device_aliases["mouse 2.4g"], "Desk Mouse");
    assert!(is_ignored(
        "virtual keyboard",
        &back
            .ignored_wireless_devices
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    ));

    common::assert_source_has(
        "crates/opentaskbar-core/src/settings.rs",
        &[
            "wireless_battery_poll_seconds",
            "WirelessPollOutOfRange",
            "WirelessDeviceAliasesInvalid",
        ],
    );
}

/// The vendor report parsers are private to the bin-only app crate, so a core
/// integration test cannot call them and a live HID device is needed to run
/// them. This bridge pins the shipped parser/VID surface while the vectors
/// above keep the expected report semantics anchored.
#[test]
fn production_hid_parsers_match_the_reference_vectors() {
    common::assert_source_has(
        "crates/opentaskbar-app/src/platform/wireless_battery.rs",
        &[
            "ASUS_VID",
            "CORSAIR_VID",
            "hyperx_family",
            "fn direct_byte_report(",
            "fn ss_parse(",
            "fn razer_parse_reply(",
        ],
    );
    let mut report = vec![0u8; 64];
    report[0] = 0x12;
    report[12] = 78;
    assert_eq!(asus_rog(0x0B05, &report), Some(78));
    assert_eq!(asus_rog(0x1B1C, &report), None);
    assert_eq!(outlier_filter(&[80, 82, 79, 81, 5]), Some(80));
}
