//! Wireless battery providers and aggregator (C# `Services/Hardware/WirelessBattery/*` parity).
//!
//! Ten vendor protocol families are implemented as **pure report parsers** (one or more unit
//! tests each). Win32 HID transport (SetupAPI/HID API) is intentionally not added here: the
//! `windows-sys` feature set owned by the lead has no `Win32_Devices_HumanInterfaceDevice`.
//! Feed the parsers with reports from any transport via `probe_report()`.
//!
//! NOTE for lead/WS-A: `AggregatorSettings` mirrors the WS-A-owned `DockSettings` fields
//! `WirelessBatteryPollSeconds`, `WirelessBatteryLowThreshold`, `IgnoredWirelessDevices`,
//! `WirelessDeviceAliases`, `WirelessBatteryListenOnly`; bridge them when merging.
//!
//! Vendors: ASUS ROG, Corsair Bragi/legacy, HyperX (4 families), Compx/ATK/VXE (13+ VIDs),
//! 2.4 GHz keyboards, DualSense/DualShock4/Switch Pro gamepads, Logitech G-Hub loopback,
//! SteelSeries GG coreProps loopback and BLE GATT BatteryLife.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Provider identity (C# `WirelessBatterySource`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BatterySource {
    Bluetooth,
    XInput,
    SteelSeriesHid,
    SteelSeriesGg,
    LogitechHidPlusPlus,
    LogitechGHub,
    CompxAtkVxe,
    RazerHid,
    BluetoothGatt,
    StandardHid,
    HyperXHid,
    CorsairHid,
    AsusRogHid,
    GamepadHid,
    Keyboard24gHid,
}

impl BatterySource {
    pub fn label(self) -> &'static str {
        match self {
            Self::SteelSeriesHid | Self::SteelSeriesGg => "SteelSeries",
            Self::CompxAtkVxe => "ATK/VXE",
            Self::LogitechHidPlusPlus | Self::LogitechGHub => "HID++",
            Self::RazerHid => "Razer",
            Self::XInput => "XInput",
            Self::BluetoothGatt => "BLE GATT",
            Self::StandardHid => "HID",
            Self::HyperXHid => "HyperX",
            Self::CorsairHid => "Corsair",
            Self::AsusRogHid => "ROG",
            Self::GamepadHid => "Gamepad",
            Self::Keyboard24gHid => "2.4G",
            _ => "Bluetooth",
        }
    }

    /// Lower value wins when two providers report the same device.
    pub fn priority(self) -> u8 {
        match self {
            Self::SteelSeriesHid => 0,
            Self::CompxAtkVxe => 1,
            Self::LogitechHidPlusPlus => 2,
            Self::RazerHid | Self::HyperXHid | Self::CorsairHid | Self::AsusRogHid
            | Self::Keyboard24gHid | Self::GamepadHid => 3,
            Self::StandardHid => 4,
            Self::Bluetooth => 5,
            Self::BluetoothGatt => 6,
            Self::XInput => 7,
            Self::SteelSeriesGg => 8,
            Self::LogitechGHub => 9,
        }
    }

    /// Providers that only observe (never send feature reports) — used by `listen_only`.
    pub fn is_passive(self) -> bool {
        matches!(
            self,
            Self::Bluetooth
                | Self::BluetoothGatt
                | Self::XInput
                | Self::StandardHid
                | Self::GamepadHid
        )
    }
}

#[derive(Clone, Debug)]
pub struct BatteryReading {
    pub id: String,
    pub name: String,
    pub level: i32,
    pub connected: bool,
    pub charging: bool,
    pub stale: bool,
    pub source: BatterySource,
    pub sampled_at: Instant,
    pub voltage_mv: Option<u32>,
    pub secondary_level: Option<i32>,
    pub level_override: Option<String>,
}

impl BatteryReading {
    pub fn new(id: &str, name: &str, level: u32, charging: bool, source: BatterySource) -> Self {
        Self {
            id: id.to_owned(),
            name: name.trim().to_owned(),
            level: level.min(100) as i32,
            connected: true,
            charging,
            stale: false,
            source,
            sampled_at: Instant::now(),
            voltage_mv: None,
            secondary_level: None,
            level_override: None,
        }
    }
}

/// HID interface descriptor (populated by the transport layer).
#[derive(Clone, Debug, Default)]
pub struct HidInterfaceDescriptor {
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_report_length: u16,
    pub output_report_length: u16,
    pub feature_report_length: u16,
    pub product_name: String,
    pub path: String,
}

// ---------------------------------------------------------------------------------------------
// 1. ASUS ROG (VID 0x0B05, usage pages 0xFF00/0xFF01, command 0x12/0x07, 64/65-byte reports)
// ---------------------------------------------------------------------------------------------

pub const ASUS_VID: u16 = 0x0B05;
pub const ASUS_CMD_CATEGORY: u8 = 0x12;
pub const ASUS_CMD_GET_BATTERY: u8 = 0x07;

pub fn build_asus_request(report_len: usize) -> Vec<u8> {
    let len = report_len.max(64);
    let mut packet = vec![0u8; len];
    let offset = usize::from(len == 65);
    packet[offset] = ASUS_CMD_CATEGORY;
    packet[offset + 1] = ASUS_CMD_GET_BATTERY;
    packet
}

fn normalize_framing(reply: &[u8]) -> &[u8] {
    if reply.len() >= 7 && reply[0] == 0 && reply[1] == ASUS_CMD_CATEGORY && reply[2] == ASUS_CMD_GET_BATTERY {
        &reply[1..]
    } else {
        reply
    }
}

/// Parses a 64-byte ASUS ROG battery reply.
pub fn parse_asus_rog(reply: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    let r = normalize_framing(reply);
    if r.len() < 6 || r[0] != ASUS_CMD_CATEGORY || r[1] != ASUS_CMD_GET_BATTERY {
        return None;
    }
    let level = r[4];
    let state = r[5];
    if level > 100 || state > 3 || (level == 0 && state != 1 && state != 2) {
        return None;
    }
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "ASUS ROG Wireless" } else { name },
        level as u32,
        state == 1 || state == 2,
        BatterySource::AsusRogHid,
    ))
}

pub fn is_asus_interface(d: &HidInterfaceDescriptor) -> bool {
    d.vendor_id == ASUS_VID
        && matches!(d.usage_page, 0xFF00 | 0xFF01)
        && matches!(d.input_report_length, 64 | 65)
        && matches!(d.output_report_length, 64 | 65)
}

// ---------------------------------------------------------------------------------------------
// 2. Corsair (VID 0x1B1C, Bragi 0xFF42 property 0x000F/0x0010, legacy 0xFFC2)
// ---------------------------------------------------------------------------------------------

pub const CORSAIR_VID: u16 = 0x1B1C;
pub const BRAGI_USAGE_PAGE: u16 = 0xFF42;
pub const CORSAIR_LEGACY_USAGE_PAGE: u16 = 0xFFC2;
pub const BRAGI_PROPERTY_LEVEL: u16 = 0x000F;
pub const BRAGI_PROPERTY_STATUS: u16 = 0x0010;

pub fn build_bragi_request(endpoint: u8, property: u16, report_len: usize) -> Vec<u8> {
    let len = report_len.max(64);
    let mut packet = vec![0u8; len];
    let offset = usize::from(len == 65);
    packet[offset] = endpoint;
    packet[offset + 1] = 0x02;
    packet[offset + 2] = (property & 0xFF) as u8;
    packet[offset + 3] = (property >> 8) as u8;
    packet
}

fn corsair_framing(reply: &[u8]) -> &[u8] {
    if reply.len() >= 7
        && reply[0] == 0
        && matches!(reply[1], 0x00 | 0x08 | 0x09 | 0xC9)
        && (matches!(reply[1], 0x08 | 0x09 | 0xC9) || reply[2] == 0x02)
    {
        &reply[1..]
    } else {
        reply
    }
}

fn valid_bragi(reply: &[u8]) -> bool {
    let r = corsair_framing(reply);
    r.len() >= 6 && r[1] == 0x02 && r[2] == 0
}

/// Bragi encodes battery in permille (1..1000 -> 1..100%).
pub fn parse_corsair_bragi_level(reply: &[u8]) -> Option<u32> {
    let r = corsair_framing(reply);
    if !valid_bragi(r) {
        return None;
    }
    let raw = u16::from_le_bytes([r[3], r[4]]);
    if raw == 0 || raw > 1000 {
        return None;
    }
    Some(((raw as f64 / 10.0).round() as u32).clamp(1, 100))
}

pub fn parse_corsair_bragi_charging(reply: &[u8]) -> Option<bool> {
    let r = corsair_framing(reply);
    if !valid_bragi(r) || r[3] > 4 {
        return None;
    }
    Some(r[3] == 1 || r[3] == 2)
}

pub fn parse_corsair_legacy(reply: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    let r = corsair_framing(reply);
    if r.len() < 5 || r[0] != 0xC9 || r[1] != 0x64 {
        return None;
    }
    let level = r[4] & 0x7F;
    let charging = (r[4] & 0x80) != 0 || matches!(r[2], 0x02 | 0x04 | 0x05);
    if level == 0 || level > 100 {
        return None;
    }
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "Corsair Wireless Headset" } else { name },
        level as u32,
        charging,
        BatterySource::CorsairHid,
    ))
}

pub fn is_corsair_interface(d: &HidInterfaceDescriptor) -> bool {
    d.vendor_id == CORSAIR_VID
        && matches!(d.usage_page, BRAGI_USAGE_PAGE | CORSAIR_LEGACY_USAGE_PAGE)
        && matches!(d.input_report_length, 64 | 65)
}

// ---------------------------------------------------------------------------------------------
// 3. HyperX (VID 0x03F0 / Kingston 0x0951, four protocol families)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HyperXFamily {
    Ff00,
    Ff13,
    Ff90,
    Ffc0,
}

pub fn hyperx_family(usage_page: u16, vendor_id: u16) -> Option<HyperXFamily> {
    if !matches!(vendor_id, 0x03F0 | 0x0951) {
        return None;
    }
    match usage_page {
        0xFF00 => Some(HyperXFamily::Ff00),
        0xFF13 => Some(HyperXFamily::Ff13),
        0xFF90 => Some(HyperXFamily::Ff90),
        0xFFC0 => Some(HyperXFamily::Ffc0),
        _ => None,
    }
}

pub fn build_hyperx_request(family: HyperXFamily, report_len: usize) -> Vec<u8> {
    let len = report_len.max(64);
    let mut packet = vec![0u8; len];
    let offset = usize::from(len == 65);
    // Each family uses a distinct command prefix; the final byte is the battery query.
    (packet[offset], packet[offset + 1], packet[offset + 2]) = match family {
        HyperXFamily::Ff00 => (0x03, 0x00, 0x08),
        HyperXFamily::Ff13 => (0x05, 0x00, 0x08),
        HyperXFamily::Ff90 => (0x06, 0x00, 0x08),
        HyperXFamily::Ffc0 => (0xC0, 0x00, 0x08),
    };
    packet
}

fn valid_hyperx(family: HyperXFamily, r: &[u8]) -> bool {
    let expected = match family {
        HyperXFamily::Ff00 => 0x03,
        HyperXFamily::Ff13 => 0x05,
        HyperXFamily::Ff90 => 0x06,
        HyperXFamily::Ffc0 => 0xC0,
    };
    let start = usize::from(r.len() >= 65 && r[0] == 0);
    r.len() >= start + 4 && r[start] == expected && r[start + 2] == 0x08
}

/// Parses a HyperX reply for any of the four protocol families.
pub fn parse_hyperx(
    family: HyperXFamily,
    reply: &[u8],
    id: &str,
    name: &str,
) -> Option<BatteryReading> {
    let start = usize::from(reply.len() >= 65 && reply[0] == 0);
    let r = &reply[start..];
    if !valid_hyperx(family, r) {
        return None;
    }
    let level = r[3];
    let charging = r.get(4).is_some_and(|b| b & 0x01 != 0);
    if level == 0 || level > 100 {
        return None;
    }
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "HyperX Wireless" } else { name },
        level as u32,
        charging,
        BatterySource::HyperXHid,
    ))
}

// ---------------------------------------------------------------------------------------------
// 4. Compx / ATK / VXE / Mchose / Lamzu / Keychron ... (13+ VIDs, 17/64/65/9-byte reports)
// ---------------------------------------------------------------------------------------------

pub const COMPX_VENDORS: &[u16] = &[
    0x3554, 0x373B, 0x36A7, 0x36B0, 0x3710, 0x372E, 0x359B, 0x248A, 0x3151, 0x373E, 0x3434,
    0x260D, 0x24AE,
];
pub const COMPX_REPORT_LEN: usize = 17;
pub const COMPX_QUERY_CMD: u8 = 0x04;

/// Compx checksum: `0x4D - sum(bytes[1..16])`.
pub fn compx_checksum(packet: &[u8]) -> u8 {
    let mut sum = 0u32;
    for &b in packet.iter().skip(1).take(COMPX_REPORT_LEN - 2) {
        sum = sum.wrapping_add(b as u32);
    }
    (0x4Du32.wrapping_sub(sum & 0xFF)) as u8
}

pub fn build_compx_request() -> [u8; COMPX_REPORT_LEN] {
    let mut packet = [0u8; COMPX_REPORT_LEN];
    packet[0] = 0x08;
    packet[1] = COMPX_QUERY_CMD;
    packet[16] = 0x49;
    packet
}

fn normalize_nordic(reply: &[u8]) -> &[u8] {
    if reply.len() >= 10 && reply[0] == 0 && matches!(reply[1], 0x08 | 0x04) {
        &reply[1..]
    } else {
        reply
    }
}

/// Standard 17-byte Compx/ATK/VXE report with checksum.
pub fn parse_compx17(reply: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    if reply.len() < 10 || reply[0] != 0x08 || reply[1] != COMPX_QUERY_CMD {
        return None;
    }
    if reply.len() >= COMPX_REPORT_LEN && compx_checksum(reply) != reply[COMPX_REPORT_LEN - 1] {
        return None;
    }
    let level = reply[6];
    let charging = reply[7];
    let voltage = u16::from_be_bytes([reply[8], reply[9]]) as u32;
    if level > 100 || charging > 1 || (level == 0 && charging == 0 && voltage == 0) {
        return None;
    }
    let mut reading = BatteryReading::new(
        id,
        if name.trim().is_empty() { "ATK Wireless Mouse" } else { name },
        level as u32,
        charging == 1,
        BatterySource::CompxAtkVxe,
    );
    if voltage > 0 {
        reading.voltage_mv = Some(voltage);
    }
    Some(reading)
}

/// 64/65-byte Nordic-framed variant.
pub fn parse_compx_nordic(reply: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    let r = normalize_nordic(reply);
    if r.len() >= COMPX_REPORT_LEN
        && r[0] == 0x08
        && r[1] == COMPX_QUERY_CMD
        && compx_checksum(r) == r[COMPX_REPORT_LEN - 1]
    {
        return parse_compx17(r, id, name);
    }
    let base = if r.len() >= 10 && r[0] == 0x08 && r[1] == COMPX_QUERY_CMD {
        6
    } else if r.len() >= 9 && r[0] == COMPX_QUERY_CMD {
        5
    } else {
        return None;
    };
    let level = *r.get(base)?;
    let charging = *r.get(base + 1)?;
    let voltage = u16::from_be_bytes([*r.get(base + 2)?, *r.get(base + 3)?]) as u32;
    if level > 100 || charging > 1 || (level == 0 && charging == 0 && voltage == 0) {
        return None;
    }
    let mut reading = BatteryReading::new(
        id,
        if name.trim().is_empty() { "Wireless Gaming Mouse" } else { name },
        level as u32,
        charging == 1,
        BatterySource::CompxAtkVxe,
    );
    if (2800..=4500).contains(&voltage) {
        reading.voltage_mv = Some(voltage);
    }
    Some(reading)
}

/// 9-byte ATK 1K dongle feature report (`0x82 0x08 0x02 0x00 0xFF 0x03 ... packed`).
pub fn parse_atk1k(reply: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    let r = if reply.len() >= 10 && reply[0] == 0 && reply[1] == 0x82 {
        &reply[1..]
    } else {
        reply
    };
    if r.len() < 9
        || r[..6] != [0x82, 0x08, 0x02, 0x00, 0xFF, 0x03]
    {
        return None;
    }
    let packed = r[8];
    let level = packed & 0x7F;
    let charging = packed & 0x80 != 0;
    if level == 0 && !charging {
        return None;
    }
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "ATK 1K Wireless Mouse" } else { name },
        level as u32,
        charging,
        BatterySource::CompxAtkVxe,
    ))
}

pub fn is_compx_vendor(vendor_id: u16) -> bool {
    COMPX_VENDORS.contains(&vendor_id)
}

// ---------------------------------------------------------------------------------------------
// 5. 2.4 GHz keyboards (Sinowealth/Compx/Akko receivers, 17/64/65-byte reports)
// ---------------------------------------------------------------------------------------------

pub fn parse_keyboard_24g(
    reply: &[u8],
    report_len: usize,
    id: &str,
    name: &str,
) -> Option<BatteryReading> {
    if report_len == 17 {
        return parse_compx17(reply, id, name).map(|mut r| {
            r.source = BatterySource::Keyboard24gHid;
            r
        });
    }
    let r = normalize_nordic(reply);
    let base = if r.len() >= 10 && r[0] == 0x08 && r[1] == COMPX_QUERY_CMD {
        6
    } else if r.len() >= 9 && r[0] == COMPX_QUERY_CMD {
        5
    } else {
        return None;
    };
    let level = *r.get(base)?;
    let charging = *r.get(base + 1)? == 1;
    if level == 0 && !charging {
        return None;
    }
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "2.4G Wireless Keyboard" } else { name },
        level as u32,
        charging,
        BatterySource::Keyboard24gHid,
    ))
}

pub fn is_keyboard_24g(vendor_id: u16, product_id: u16) -> bool {
    matches!(vendor_id, 0x258A)
        || (vendor_id == 0x3554 && product_id == 0xFA09)
        || (vendor_id == 0x3151 && matches!(product_id, 0x4011 | 0x4015))
}

// ---------------------------------------------------------------------------------------------
// 6. Gamepads: DualSense (0x01 USB / 0x31 BT), DualShock4 (0x01 / 0x11), Switch Pro (0x30)
// ---------------------------------------------------------------------------------------------

pub fn parse_dualsense(report: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    let offset = match report.first() {
        Some(0x01) if report.len() >= 54 => 53,
        Some(0x31) if report.len() >= 55 => 54,
        _ => return None,
    };
    let raw = report[offset];
    let step = raw & 0x0F;
    let power = (raw >> 4) & 0x0F;
    if power > 2 || (power != 2 && step > 10) {
        return None;
    }
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "DualSense" } else { name },
        if power == 2 { 100 } else { step as u32 * 10 },
        power == 1,
        BatterySource::GamepadHid,
    ))
}

pub fn parse_dualshock4(report: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    let offset = match report.first() {
        Some(0x01) if report.len() >= 31 => 30,
        Some(0x11) if report.len() >= 33 => 32,
        _ => return None,
    };
    let raw = report[offset];
    let cable = raw & 0x10 != 0;
    let step = raw & 0x0F;
    if step > 11 || (!cable && step > 10) {
        return None;
    }
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "DualShock 4" } else { name },
        step as u32 * 10,
        cable && step < 11,
        BatterySource::GamepadHid,
    ))
}

pub fn parse_switch_pro(report: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    if report.len() < 13 || report[0] != 0x30 || report[2] == 0 {
        return None;
    }
    let raw = report[2];
    let charging = raw & 0x10 != 0;
    let level = match (raw >> 5) & 0x07 {
        4 => 100,
        3 => 75,
        2 => 50,
        1 => 25,
        _ => 10,
    };
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "Nintendo Switch Pro" } else { name },
        level,
        charging,
        BatterySource::GamepadHid,
    ))
}

pub fn parse_gamepad(vendor: u16, product: u16, report: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    match (vendor, product) {
        (0x054C, 0x0CE6 | 0x0DF2) => parse_dualsense(report, id, name),
        (0x054C, 0x05C4 | 0x09CC) => parse_dualshock4(report, id, name),
        (0x057E, 0x2009) => parse_switch_pro(report, id, name),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// 7. Logitech G-Hub loopback JSON + HID++ battery feature parsers
// ---------------------------------------------------------------------------------------------

pub fn is_valid_ghub_uri(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    lower.contains("127.0.0.1")
        || lower.contains("localhost")
        || lower.starts_with("http://[::1]")
}

/// Parses G-Hub loopback JSON into readings (C# `ParseGHubJson` / `ExtractGHubDevices`).
pub fn parse_ghub_json(json: &str) -> Vec<BatteryReading> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    extract_loopback_devices(&root, BatterySource::LogitechGHub, &mut out);
    out
}

fn json_str<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            return Some(s);
        }
    }
    None
}

fn json_num(v: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(n) = v.get(k).and_then(|x| x.as_f64()) {
            return Some(n);
        }
    }
    None
}

fn extract_loopback_devices(node: &serde_json::Value, source: BatterySource, out: &mut Vec<BatteryReading>) {
    match node {
        serde_json::Value::Array(items) => {
            for item in items {
                extract_loopback_devices(item, source, out);
            }
        }
        serde_json::Value::Object(map) => {
            let name = json_str(node, &["name", "deviceName", "displayName"])
                .unwrap_or("Logitech Wireless")
                .to_owned();
            let level = json_num(node, &["batteryLevel", "battery", "level", "percentage"])
                .or_else(|| {
                    json_num(
                        node.get("battery")?,
                        &["level", "percentage", "chargingLevel"],
                    )
                });
            if let Some(level) = level {
                let level = if level <= 1.0 {
                    (level * 100.0).round() as u32
                } else {
                    level.round() as u32
                };
                if level <= 100 {
                    let id = json_str(node, &["id", "deviceId", "serial"])
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("ghub:{name}"));
                    let charging = node
                        .get("charging")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    out.push(BatteryReading::new(&id, &name, level, charging, source));
                    return;
                }
            }
            for value in map.values() {
                extract_loopback_devices(value, source, out);
            }
        }
        _ => {}
    }
}

/// HID++ `BatteryStatus` feature reply (0x1000) -> percent.
pub fn parse_hidpp_battery_status(reply: &[u8]) -> Option<(u32, bool)> {
    if reply.len() < 7 || reply[0] != 0x10 {
        return None;
    }
    let level = reply[2];
    if level > 100 {
        return None;
    }
    Some((level as u32, reply[3] & 0x01 != 0))
}

// ---------------------------------------------------------------------------------------------
// 8. SteelSeries GG coreProps loopback + HID parsers
// ---------------------------------------------------------------------------------------------

/// Finds a loopback endpoint in `coreProps.json` data (`"gcEndpoint": "http://127.0.0.1:..."`) .
pub fn parse_coreprops_endpoint(json: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(json).ok()?;
    fn find(node: &serde_json::Value) -> Option<String> {
        match node {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    let lower = k.to_ascii_lowercase();
                    if lower.contains("endpoint") && lower.contains("gc") {
                        if let Some(url) = v.as_str() {
                            return Some(url.to_owned());
                        }
                    }
                }
                for v in map.values() {
                    if let Some(found) = find(v) {
                        return Some(found);
                    }
                }
                None
            }
            serde_json::Value::Array(items) => items.iter().find_map(find),
            _ => None,
        }
    }
    find(&root).filter(|u| is_valid_ss_endpoint(u))
}

pub fn is_valid_ss_endpoint(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.contains("127.0.0.1") || lower.contains("localhost") || lower.contains("[::1]"))
        && lower.starts_with("http")
}

/// Parses SteelSeries GG battery JSON (loopback `/battery` payload).
pub fn parse_steelseries_gg_json(json: &str) -> Vec<BatteryReading> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    extract_loopback_devices(&root, BatterySource::SteelSeriesGg, &mut out);
    out
}

/// SteelSeries HID status byte map: 0x01 offline, 0x02 charging, 0x08 discharging.
pub fn parse_steelseries_hid(reply: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    if reply.len() < 4 {
        return None;
    }
    let status = reply[1];
    let level = reply[2];
    if !matches!(status, 0x01 | 0x02 | 0x08) || level > 100 {
        return None;
    }
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "SteelSeries Wireless" } else { name },
        level as u32,
        status == 0x02,
        BatterySource::SteelSeriesHid,
    ))
}

// ---------------------------------------------------------------------------------------------
// 9. BLE GATT BatteryLife / DevPkeyIsPresent / AepId filter
// ---------------------------------------------------------------------------------------------

pub const BLE_BATTERY_SERVICE: u16 = 0x180F;
pub const BLE_BATTERY_LEVEL_CHAR: u16 = 0x2A19;

/// True when a PnP association id looks like a BLE AEP id (`..._aep_...` / `BTHENUM` GATT).
pub fn is_ble_aep_id(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    lower.contains("aep")
        || (lower.contains("bthenum") && lower.contains("180f"))
        || lower.contains("blegatt")
}

/// Parses a BLE BatteryLevel characteristic payload (one byte) with optional PnP percentage.
pub fn parse_ble_gatt(payload: &[u8], id: &str, name: &str) -> Option<BatteryReading> {
    let level = *payload.first()?;
    if level > 100 {
        return None;
    }
    Some(BatteryReading::new(
        id,
        if name.trim().is_empty() { "BLE Device" } else { name },
        level as u32,
        false,
        BatterySource::BluetoothGatt,
    ))
}

// ---------------------------------------------------------------------------------------------
// 10. Aggregator: cache, outlier guard, dedup, settings, diagnostics
// ---------------------------------------------------------------------------------------------

pub const OUTLIER_DROP_THRESHOLD: i32 = 25;
pub const OUTLIER_FLOOR: i32 = 25;
pub const MAX_CACHE_BYTES: usize = 65_536;
pub const PROVIDER_TIMEOUT: Duration = Duration::from_millis(1200);
pub const STALE_WINDOW: Duration = Duration::from_secs(30 * 60);
pub const WRAITH_VID: u16 = 0x2E3C;
pub const WRAITH_W75_PID: u16 = 0xC368;

pub fn is_ignored_wired(vendor: u16, product: u16, name: &str) -> bool {
    (vendor == WRAITH_VID && matches!(product, WRAITH_W75_PID | 0))
        || name.to_ascii_lowercase().contains("wraith w75")
}

/// Privacy-safe HID diagnostic summary: only numeric IDs, usages and report lengths.
pub fn privacy_diagnostic_summary(descriptors: &[HidInterfaceDescriptor]) -> String {
    let mut lines: Vec<String> = descriptors
        .iter()
        .filter(|d| !is_ignored_wired(d.vendor_id, d.product_id, ""))
        .map(|d| {
            format!(
                "VID:{:04X} PID:{:04X} UsagePage:0x{:04X} Usage:0x{:04X} In:{} Out:{} Feat:{}",
                d.vendor_id,
                d.product_id,
                d.usage_page,
                d.usage,
                d.input_report_length,
                d.output_report_length,
                d.feature_report_length
            )
        })
        .collect();
    lines.sort();
    lines.dedup();
    if lines.is_empty() {
        "HID Interfaces: 0".to_owned()
    } else {
        lines.join("\n")
    }
}

fn normalize_name(name: &str) -> String {
    name.trim().split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

/// Dedup by name (canonical, shared across providers), falling back to `source:id`.
pub fn deduplicate(readings: Vec<BatteryReading>) -> Vec<BatteryReading> {
    let mut by_key: HashMap<String, BatteryReading> = HashMap::new();
    let mut counts: HashMap<(BatterySource, String), usize> = HashMap::new();
    for r in &readings {
        let n = normalize_name(&r.name);
        if !n.is_empty() {
            *counts.entry((r.source, n)).or_insert(0) += 1;
        }
    }
    for r in readings {
        let n = normalize_name(&r.name);
        let collision = !n.is_empty() && counts.get(&(r.source, n.clone())).copied().unwrap_or(0) > 1;
        let key = if !n.is_empty() && !collision {
            format!("name:{n}")
        } else if !r.id.trim().is_empty() {
            format!("src:{}:id:{}", r.source as u8, r.id.trim().to_ascii_lowercase())
        } else {
            format!("name:{n}")
        };
        match by_key.get_mut(&key) {
            Some(existing) if prefer(&r, existing) => {
                merge_metadata(existing, &r);
            }
            Some(existing) => merge_metadata(existing, &r),
            None => {
                by_key.insert(key, r);
            }
        }
    }
    by_key.into_values().collect()
}

fn prefer(candidate: &BatteryReading, existing: &BatteryReading) -> bool {
    if candidate.connected != existing.connected {
        return candidate.connected;
    }
    let c_valid = candidate.level >= 0 || candidate.level_override.is_some();
    let e_valid = existing.level >= 0 || existing.level_override.is_some();
    if c_valid != e_valid {
        return c_valid;
    }
    if candidate.stale != existing.stale {
        return !candidate.stale;
    }
    candidate.source.priority() < existing.source.priority()
}

fn merge_metadata(primary: &mut BatteryReading, secondary: &BatteryReading) {
    if primary.voltage_mv.is_none() {
        primary.voltage_mv = secondary.voltage_mv;
    }
    if primary.secondary_level.is_none() {
        primary.secondary_level = secondary.secondary_level;
    }
}

#[derive(Default)]
pub struct OutlierGuard {
    pending: HashMap<String, i32>,
}

impl OutlierGuard {
    /// Returns `(accepted_level, was_guarded)`. A single implausible >=25-point drop from a
    /// trusted >25% reading is held for one poll; two consecutive drops are accepted.
    pub fn check(&mut self, key: &str, prev: i32, fresh: i32) -> (i32, bool) {
        if prev > OUTLIER_FLOOR && prev - fresh >= OUTLIER_DROP_THRESHOLD {
            if self
                .pending
                .get(key)
                .is_some_and(|p| prev - *p >= OUTLIER_DROP_THRESHOLD)
            {
                self.pending.remove(key);
                (fresh, false)
            } else {
                self.pending.insert(key.to_owned(), fresh);
                (prev, true)
            }
        } else {
            self.pending.remove(key);
            (fresh, false)
        }
    }
}

/// Settings consumed by the aggregator (the corresponding `DockSettings` fields are WS-A owned).
#[derive(Clone, Debug)]
pub struct AggregatorSettings {
    pub poll_seconds: u32,
    pub low_threshold: u32,
    pub ignored_devices: Vec<String>,
    pub aliases: HashMap<String, String>,
    pub listen_only: bool,
}

impl Default for AggregatorSettings {
    fn default() -> Self {
        Self {
            poll_seconds: 30,
            low_threshold: 20,
            ignored_devices: Vec::new(),
            aliases: HashMap::new(),
            listen_only: false,
        }
    }
}

impl AggregatorSettings {
    /// C# `validate()` range for `WirelessBatteryPollSeconds` is 15..=300.
    pub fn poll_seconds_clamped(&self) -> u32 {
        self.poll_seconds.clamp(15, 300)
    }

    pub fn is_ignored(&self, reading: &BatteryReading) -> bool {
        let name = reading.name.to_ascii_lowercase();
        let id = reading.id.to_ascii_lowercase();
        self.ignored_devices
            .iter()
            .any(|x| !x.is_empty() && (name.contains(&x.to_ascii_lowercase()) || id == x.to_ascii_lowercase()))
    }

    pub fn display_name(&self, reading: &BatteryReading) -> String {
        let key = reading.id.to_ascii_lowercase();
        self.aliases
            .get(&key)
            .cloned()
            .unwrap_or_else(|| reading.name.clone())
    }

    pub fn allows(&self, source: BatterySource) -> bool {
        !self.listen_only || source.is_passive()
    }
}

pub struct WirelessBatteryAggregator {
    pub settings: AggregatorSettings,
    pub cache: HashMap<String, BatteryReading>,
    pub guard: OutlierGuard,
    cache_path: Option<PathBuf>,
    loaded: bool,
}

impl WirelessBatteryAggregator {
    pub fn new(settings: AggregatorSettings) -> Self {
        let cache_path = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("OpenTaskbar").join("wireless-battery-cache.json"));
        Self {
            settings,
            cache: HashMap::new(),
            guard: OutlierGuard::default(),
            cache_path,
            loaded: false,
        }
    }

    pub fn with_cache_path(mut self, path: PathBuf) -> Self {
        self.cache_path = Some(path);
        self
    }

    fn ensure_loaded(&mut self, now: Instant) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let Some(path) = &self.cache_path else {
            return;
        };
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        if meta.len() == 0 || meta.len() > MAX_CACHE_BYTES as u64 {
            return;
        }
        let Ok(json) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(root) = serde_json::from_str::<serde_json::Value>(&json) else {
            return;
        };
        let Some(map) = root.as_object() else {
            return;
        };
        for (key, value) in map {
            if self.cache.len() >= 64 {
                break;
            }
            let level = value.get("level").and_then(|x| x.as_i64()).unwrap_or(-1) as i32;
            if level < -1 || level > 100 {
                continue;
            }
            let source_id = value.get("source").and_then(|x| x.as_u64()).unwrap_or(0) as u8;
            let age_secs = value.get("age_secs").and_then(|x| x.as_u64()).unwrap_or(0);
            if Duration::from_secs(age_secs) > STALE_WINDOW {
                continue;
            }
            let mut reading = BatteryReading::new(
                value.get("id").and_then(|x| x.as_str()).unwrap_or(key),
                value.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                level.max(0) as u32,
                value.get("charging").and_then(|x| x.as_bool()).unwrap_or(false),
                source_from_id(source_id),
            );
            reading.stale = true;
            reading.sampled_at = now - Duration::from_secs(age_secs.min(1800));
            self.cache.insert(key.clone(), reading);
        }
    }

    fn save(&self, now: Instant) {
        let Some(path) = &self.cache_path else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut root = serde_json::Map::new();
        for (key, r) in &self.cache {
            root.insert(
                key.clone(),
                serde_json::json!({
                    "id": r.id,
                    "name": r.name,
                    "level": r.level,
                    "charging": r.charging,
                    "source": r.source as u8,
                    "age_secs": now.saturating_duration_since(r.sampled_at).as_secs(),
                }),
            );
        }
        let _ = std::fs::write(path, serde_json::to_string(&serde_json::Value::Object(root)).unwrap_or_default());
    }

    /// Merges fresh readings with the cache: outlier protection, stale retention (30 min),
    /// alias/ignore settings and disk persistence.
    pub fn merge(&mut self, fresh: Vec<BatteryReading>) -> Vec<BatteryReading> {
        let now = Instant::now();
        self.ensure_loaded(now);
        let fresh: Vec<BatteryReading> = fresh
            .into_iter()
            .filter(|r| !self.settings.is_ignored(r) && self.settings.allows(r.source))
            .collect();
        let fresh = deduplicate(fresh);
        let mut output = Vec::new();
        let mut active = HashSet::new();
        for mut reading in fresh {
            let key = format!("src:{}:id:{}", reading.source as u8, reading.id.to_ascii_lowercase());
            active.insert(key.clone());
            reading.name = self.settings.display_name(&reading);
            if reading.connected && reading.level >= 0 {
                let prev = self.cache.get(&key).map(|r| r.level).unwrap_or(-1);
                let (level, guarded) = self.guard.check(&key, prev, reading.level);
                if guarded && let Some(prev_reading) = self.cache.get(&key) {
                    let mut held = prev_reading.clone();
                    held.charging = reading.charging;
                    held.sampled_at = now;
                    output.push(held.clone());
                    self.cache.insert(key, held);
                    continue;
                }
                reading.level = level;
                reading.stale = false;
                reading.sampled_at = now;
                self.cache.insert(key, reading.clone());
                output.push(reading);
            } else if let Some(cached) = self.cache.get(&key)
                && now.saturating_duration_since(cached.sampled_at) <= STALE_WINDOW
            {
                let mut retained = cached.clone();
                retained.stale = true;
                retained.charging = reading.charging;
                output.push(retained);
            }
        }
        let mut expired = Vec::new();
        for (key, cached) in &self.cache {
            if now.saturating_duration_since(cached.sampled_at) > STALE_WINDOW {
                expired.push(key.clone());
                continue;
            }
            if !active.contains(key) {
                let mut retained = cached.clone();
                retained.stale = true;
                retained.charging = false;
                output.push(retained);
            }
        }
        for key in expired {
            self.cache.remove(&key);
            self.guard.pending.remove(&key);
        }
        output.sort_by_key(|r| (r.stale, r.charging, r.level));
        self.save(now);
        output
    }
}

fn source_from_id(id: u8) -> BatterySource {
    match id {
        0 => BatterySource::Bluetooth,
        1 => BatterySource::XInput,
        2 => BatterySource::SteelSeriesHid,
        3 => BatterySource::SteelSeriesGg,
        4 => BatterySource::LogitechHidPlusPlus,
        5 => BatterySource::LogitechGHub,
        6 => BatterySource::CompxAtkVxe,
        7 => BatterySource::RazerHid,
        8 => BatterySource::BluetoothGatt,
        9 => BatterySource::StandardHid,
        10 => BatterySource::HyperXHid,
        11 => BatterySource::CorsairHid,
        12 => BatterySource::AsusRogHid,
        13 => BatterySource::GamepadHid,
        14 => BatterySource::Keyboard24gHid,
        _ => BatterySource::Bluetooth,
    }
}

/// Routes one raw HID report to the right vendor parser based on its descriptor.
pub fn probe_report(
    descriptor: &HidInterfaceDescriptor,
    report: &[u8],
) -> Option<BatteryReading> {
    let id = format!(
        "hid:{:04X}:{:04X}",
        descriptor.vendor_id, descriptor.product_id
    );
    let name = descriptor.product_name.as_str();
    if is_asus_interface(descriptor) {
        return parse_asus_rog(report, &id, name);
    }
    if is_corsair_interface(descriptor) {
        if descriptor.usage_page == BRAGI_USAGE_PAGE {
            let level = parse_corsair_bragi_level(report)?;
            let charging = parse_corsair_bragi_charging(report).unwrap_or(false);
            let mut r = BatteryReading::new(&id, name, level, charging, BatterySource::CorsairHid);
            r.level = level as i32;
            return Some(r);
        }
        return parse_corsair_legacy(report, &id, name);
    }
    if let Some(family) = hyperx_family(descriptor.usage_page, descriptor.vendor_id) {
        return parse_hyperx(family, report, &id, name);
    }
    if is_compx_vendor(descriptor.vendor_id) {
        return parse_compx_nordic(report, &id, name).or_else(|| parse_compx17(report, &id, name));
    }
    if is_keyboard_24g(descriptor.vendor_id, descriptor.product_id) {
        return parse_keyboard_24g(report, descriptor.input_report_length as usize, &id, name);
    }
    if matches!(descriptor.vendor_id, 0x054C | 0x057E) {
        return parse_gamepad(descriptor.vendor_id, descriptor.product_id, report, &id, name);
    }
    if descriptor.vendor_id == 0x1038 {
        return parse_steelseries_hid(report, &id, name);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(vid: u16, pid: u16, page: u16, len: u16) -> HidInterfaceDescriptor {
        HidInterfaceDescriptor {
            vendor_id: vid,
            product_id: pid,
            usage_page: page,
            usage: 0,
            input_report_length: len,
            output_report_length: len,
            feature_report_length: len,
            product_name: "Test Device".into(),
            path: String::new(),
        }
    }

    #[test]
    fn asus_rog_vendor() {
        let mut report = vec![0u8; 64];
        report[0] = 0x12;
        report[1] = 0x07;
        report[4] = 68;
        report[5] = 1;
        let r = parse_asus_rog(&report, "asus", "ROG Harpe").unwrap();
        assert_eq!(r.level, 68);
        assert!(r.charging);
        assert_eq!(r.source, BatterySource::AsusRogHid);
        assert!(is_asus_interface(&desc(0x0B05, 0x1234, 0xFF00, 64)));
        let req = build_asus_request(65);
        assert_eq!(&req[1..3], &[0x12, 0x07]);
    }

    #[test]
    fn corsair_bragi_and_legacy() {
        let mut level = vec![0u8; 64];
        level[0] = 0x09;
        level[1] = 0x02;
        level[2] = 0;
        level[3] = 0xE8;
        level[4] = 0x03; // 1000 permille = 100%
        assert_eq!(parse_corsair_bragi_level(&level), Some(100));
        let mut status = level.clone();
        status[3] = 2;
        assert_eq!(parse_corsair_bragi_charging(&status), Some(true));
        let mut legacy = vec![0u8; 64];
        legacy[0] = 0xC9;
        legacy[1] = 0x64;
        legacy[4] = 0x55;
        let r = parse_corsair_legacy(&legacy, "corsair", "HS80").unwrap();
        assert_eq!(r.level, 85);
        assert!(is_corsair_interface(&desc(0x1B1C, 1, 0xFF42, 64)));
    }

    #[test]
    fn hyperx_four_families() {
        for (page, prefix) in [(0xFF00, 0x03), (0xFF13, 0x05), (0xFF90, 0x06), (0xFFC0, 0xC0)] {
            let family = hyperx_family(page, 0x03F0).unwrap();
            let mut report = build_hyperx_request(family, 64);
            report[3] = 77;
            let parsed = parse_hyperx(family, &report, "hyperx", "Cloud").unwrap();
            assert_eq!(parsed.level, 77);
            assert!(hyperx_family(page, 0x0951).is_some());
        }
        assert!(hyperx_family(0xFF00, 0x0951).is_some());
        assert!(hyperx_family(0xFF00, 0x1234).is_none());
    }

    #[test]
    fn compx_checksum_and_reports() {
        let request = build_compx_request();
        assert_eq!(compx_checksum(&request), 0x49);
        let mut reply = request.clone();
        reply[6] = 55;
        reply[7] = 1;
        reply[8] = 0x0E;
        reply[9] = 0x74; // 3700 mV
        reply[16] = compx_checksum(&reply);
        assert!(parse_compx17(&reply, "atk", "ATK A9").is_some());
        let mut corrupt = build_compx_request();
        corrupt[6] = 42;
        corrupt[16] = 0; // wrong checksum -> reject (full 17-byte report)
        assert!(parse_compx17(&corrupt, "atk", "mouse").is_none());
        let mut atk9 = [0u8; 9];
        atk9[..6].copy_from_slice(&[0x82, 0x08, 0x02, 0, 0xFF, 0x03]);
        atk9[8] = 0x80 | 42;
        assert_eq!(parse_atk1k(&atk9, "atk1k", "ATK 1K").unwrap().level, 42);
        assert!(is_compx_vendor(0x3434) && is_compx_vendor(0x373B));
    }

    #[test]
    fn keyboard_24g_protocol() {
        assert!(is_keyboard_24g(0x258A, 0x0001));
        assert!(is_keyboard_24g(0x3554, 0xFA09));
        let mut report = build_compx_request();
        report[6] = 60;
        report[16] = compx_checksum(&report);
        let r = parse_keyboard_24g(&report, 17, "kb", "AULA").unwrap();
        assert_eq!(r.source, BatterySource::Keyboard24gHid);
        assert_eq!(r.level, 60);
    }

    #[test]
    fn gamepad_vendor_reports() {
        let mut ds = vec![0u8; 54];
        ds[0] = 0x01;
        ds[53] = 0x05; // step 5 (50%), discharging
        assert_eq!(parse_dualsense(&ds, "ds", "DualSense").unwrap().level, 50);
        ds[53] = 0x1A; // charging (0x1), step 10 -> 100%
        let charged = parse_dualsense(&ds, "ds", "DualSense").unwrap();
        assert_eq!(charged.level, 100);
        assert!(charged.charging);
        let mut ds4 = vec![0u8; 31];
        ds4[0] = 0x01;
        ds4[30] = 0x1A; // cable + step 10
        let r = parse_dualshock4(&ds4, "ds4", "DualShock 4").unwrap();
        assert_eq!(r.level, 100);
        assert!(r.charging);
        let mut sw = vec![0u8; 13];
        sw[0] = 0x30;
        sw[2] = 0x70; // charging (0x10) + level nibble 3 (0x60) = 75%
        let r = parse_switch_pro(&sw, "sw", "Switch Pro").unwrap();
        assert_eq!(r.level, 75);
        assert!(r.charging);
        assert!(parse_gamepad(0x054C, 0x0CE6, &ds, "x", "x").is_some());
    }

    #[test]
    fn logitech_ghub_loopback_json() {
        assert!(is_valid_ghub_uri("http://127.0.0.1:9010"));
        assert!(!is_valid_ghub_uri("https://evil.example.com"));
        let json = r#"{"devices":[{"name":"G Pro X","batteryLevel":73,"charging":false,"id":"1"}]}"#;
        let readings = parse_ghub_json(json);
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].level, 73);
        assert_eq!(readings[0].source, BatterySource::LogitechGHub);
        let mut hidpp = [0u8; 7];
        hidpp[0] = 0x10;
        hidpp[2] = 60;
        hidpp[3] = 1;
        assert_eq!(parse_hidpp_battery_status(&hidpp), Some((60, true)));
    }

    #[test]
    fn steelseries_coreprops_and_gg() {
        let json = r#"{"gcEndpoint":"http://127.0.0.1:8256/","other":1}"#;
        assert_eq!(
            parse_coreprops_endpoint(json).as_deref(),
            Some("http://127.0.0.1:8256/")
        );
        assert!(parse_coreprops_endpoint(r#"{"gcEndpoint":"https://evil.com"}"#).is_none());
        let gg = r#"{"headsets":[{"deviceName":"Arctis 7","battery":88,"charging":true}]}"#;
        let readings = parse_steelseries_gg_json(gg);
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].level, 88);
        let mut hid = [0u8; 4];
        hid[1] = 0x02;
        hid[2] = 40;
        assert_eq!(parse_steelseries_hid(&hid, "ss", "Nova").unwrap().level, 40);
    }

    #[test]
    fn ble_gatt_battery() {
        assert!(is_ble_aep_id("BTHENUM#{180F}_aep_1234"));
        // GATT BatteryLevel is a valid 0..100 byte; 0% is a genuine reading.
        assert_eq!(parse_ble_gatt(&[0], "ble", "Device").unwrap().level, 0);
        assert!(parse_ble_gatt(&[101], "ble", "Device").is_none());
        let r = parse_ble_gatt(&[92], "ble", "Device").unwrap();
        assert_eq!(r.level, 92);
        assert_eq!(r.source, BatterySource::BluetoothGatt);
    }

    #[test]
    fn aggregator_outlier_guard() {
        let mut guard = OutlierGuard::default();
        let (level, guarded) = guard.check("dev", 90, 60);
        assert_eq!(level, 90);
        assert!(guarded);
        let (level, guarded) = guard.check("dev", 90, 55);
        assert_eq!(level, 55);
        assert!(!guarded);
        let (level, guarded) = guard.check("dev", 90, 89);
        assert_eq!(level, 89);
        assert!(!guarded);
    }

    #[test]
    fn aggregator_settings_privacy_and_merge() {
        let mut settings = AggregatorSettings::default();
        settings.poll_seconds = 999;
        settings.aliases.insert("id1".into(), "My Mouse".into());
        settings.ignored_devices.push("hidden".into());
        assert_eq!(settings.poll_seconds_clamped(), 300);
        let mut hidden = BatteryReading::new("hidden", "Hidden", 50, false, BatterySource::Bluetooth);
        let mut good = BatteryReading::new("id1", "Mouse", 80, false, BatterySource::AsusRogHid);
        assert!(settings.is_ignored(&hidden));
        assert_eq!(settings.display_name(&good), "My Mouse");
        hidden.name = "Hidden".into();
        let desc = [desc(0x2E3C, 0xC368, 0xFF00, 64), desc(0x0B05, 1, 0xFF00, 64)];
        let summary = privacy_diagnostic_summary(&desc);
        assert!(!summary.contains("2E3C"));
        assert!(summary.contains("0B05"));
        let cache_path = std::env::temp_dir().join("otb-wireless-battery-test-cache.json");
        let _ = std::fs::remove_file(&cache_path);
        let mut agg = WirelessBatteryAggregator::new(settings).with_cache_path(cache_path.clone());
        good.name = "My Mouse".into();
        let result = agg.merge(vec![hidden, good]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "My Mouse");
        let _ = std::fs::remove_file(cache_path);
    }
}
